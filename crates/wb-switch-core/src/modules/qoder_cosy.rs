//! Qoder COSY 签名 —— 出站请求的会话密钥包裹、载荷编码与签名头。
//!
//! 移植自参考实现 `buddy-switch-core/src/modules/qoder/cosy.rs`（与官方客户端逐字节对齐）。
//! 三部分：
//!
//! 1. **自定义 base64 变体**：先标准 base64，再按 `a = len/3` 三段轮转
//!    （尾 / 中 / 首），最后逐字符映射到自定义字母表、`'='` 换成 `'$'`。
//! 2. **COSY 会话**：`temp_key`（16 字节）→ RSA PKCS#1 v1.5 包裹成 `cosy_key`；
//!    身份体九个键 → 排序紧凑 JSON → AES-128-CBC（**key == iv == temp_key**）→ `info`。
//! 3. **Bearer**：`payload = base64(排序紧凑 {cosyVersion, ideVersion, info, requestId, version})`；
//!    `sig = hex(md5(payload + "\n" + cosyKey + "\n" + date + "\n" + body + "\n" + path))`；
//!    `authorization = "Bearer COSY." + payload + "." + sig`。
//!
//! ## ⚠️ 不许「加固」的两处（参考实现明确警告）
//!
//! - `temp_key` 是 **16 个十六进制字符**（UUIDv4 hex 前 16 位）直接当字节用，
//!   名义 AES-128、实际熵约 64 bit。服务端用同一串字节解 `info`，**换成真随机 16 字节必然全盘失败**。
//! - 服务端 RSA 公钥是官方客户端硬编码的 **1024 位**，必须与官方私钥配对，换不了。

use base64::Engine;
use md5::{Digest as _, Md5};
use serde_json::{Map, Value};
use sha2::Sha512;

use crate::modules::qoder_accounts::QoderAccount;
use crate::modules::qoder_realm::QoderRegion;

const CUSTOM_ALPHABET: &str = "_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STD_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD_CHAR: u8 = b'$';

/// 官方客户端内置的服务端公钥（1024 位，RSA PKCS#1 v1.5）。
pub const SERVER_PUB_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc\n\
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l\n\
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17\n\
XcW+ML9FoCI6AOvOzwIDAQAB\n\
-----END PUBLIC KEY-----";

pub const COSY_VERSION: &str = "1.1.64";
pub const DEFAULT_USER_TYPE: &str = "personal_professional_trial";
const COSY_CLIENT_IP: &str = "169.254.198.161";

// ---------------------------------------------------------------- 编码

/// 出站请求体的自定义 base64 变体。
pub fn qoder_encode(plain: &[u8]) -> String {
    let std = base64::engine::general_purpose::STANDARD.encode(plain);
    let bytes = std.as_bytes();
    let n = bytes.len();
    let a = n / 3;
    let rearranged = [&bytes[n - a..], &bytes[a..n - a], &bytes[..a]].concat();
    let custom = CUSTOM_ALPHABET.as_bytes();
    rearranged
        .iter()
        .map(|&b| {
            if b == b'=' {
                PAD_CHAR as char
            } else {
                match STD_ALPHABET.as_bytes().iter().position(|&x| x == b) {
                    Some(i) => custom[i] as char,
                    None => b as char,
                }
            }
        })
        .collect()
}

/// [`qoder_encode`] 的逆运算（测试用）。
pub fn qoder_decode(encoded: &str) -> Result<Vec<u8>, String> {
    let custom = CUSTOM_ALPHABET.as_bytes();
    let std: String = encoded
        .bytes()
        .map(|b| {
            if b == PAD_CHAR {
                '='
            } else {
                match custom.iter().position(|&x| x == b) {
                    Some(i) => STD_ALPHABET.as_bytes()[i] as char,
                    None => b as char,
                }
            }
        })
        .collect();
    let bytes = std.as_bytes();
    let n = bytes.len();
    let a = n / 3;
    // 这个三段轮转**是自逆的**：对结果再转一次就还原
    // （R = 尾A + 中B + 首C；对 R 再转得 C + B + A = 原串）。
    let restored = [&bytes[n - a..], &bytes[a..n - a], &bytes[..a]].concat();
    base64::engine::general_purpose::STANDARD
        .decode(restored)
        .map_err(|e| format!("解码失败：{e}"))
}

/// 键排序后的紧凑 JSON（签名与 AES 输入都要求字节稳定）。
pub fn json_sorted_compact(values: &Map<String, Value>) -> Vec<u8> {
    let mut keys: Vec<&String> = values.keys().collect();
    keys.sort();
    let mut out = String::from("{");
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(k).unwrap_or_default());
        out.push(':');
        out.push_str(&serde_json::to_string(&values[*k]).unwrap_or_default());
    }
    out.push('}');
    out.into_bytes()
}

/// AES-128-CBC + PKCS#7。
pub fn aes_cbc_encrypt(plain: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Enc = cbc::Encryptor<aes::Aes128>;
    // 用 `encrypt_padded_mut`（不需要 cbc 的 alloc feature），自己准备缓冲区。
    let mut buf = vec![0u8; plain.len() + 16];
    buf[..plain.len()].copy_from_slice(plain);
    let n = Enc::new(key.into(), iv.into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
        .expect("缓冲区按明文长度 + 一个块分配，不会越界")
        .len();
    buf.truncate(n);
    buf
}

// ---------------------------------------------------------------- RSA

fn server_public_key() -> Result<rsa::RsaPublicKey, String> {
    use rsa::pkcs8::DecodePublicKey;
    rsa::RsaPublicKey::from_public_key_pem(SERVER_PUB_PEM)
        .map_err(|e| format!("内置公钥解析失败：{e}"))
}

fn rsa_encrypt(plain: &[u8]) -> Result<Vec<u8>, String> {
    use rsa::rand_core::OsRng;
    use rsa::Pkcs1v15Encrypt;
    server_public_key()?
        .encrypt(&mut OsRng, Pkcs1v15Encrypt, plain)
        .map_err(|e| format!("RSA 包裹失败：{e}"))
}

/// 签名串：`hex(md5(payload \n cosyKey \n date \n body \n path))`。
pub fn signature(payload_b64: &str, cosy_key: &str, date: &str, body: &str, path: &str) -> String {
    let mut h = Md5::new();
    h.update(payload_b64.as_bytes());
    h.update(b"\n");
    h.update(cosy_key.as_bytes());
    h.update(b"\n");
    h.update(date.as_bytes());
    h.update(b"\n");
    h.update(body.as_bytes());
    h.update(b"\n");
    h.update(path.as_bytes());
    format!("{:x}", h.finalize())
}

fn now_secs() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into())
}

// ---------------------------------------------------------------- 身份

/// 一个账号的全部派生身份（四个值用不同 salt，避免被上游关联）。
pub struct MachineIdentity {
    pub machine_id: String,
    pub machine_token: String,
    pub machine_type: String,
    pub session_id: String,
}

fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

pub fn machine_identity(uid: &str) -> MachineIdentity {
    let mut h = Sha512::new();
    h.update(format!("machinetoken:{uid}").as_bytes());
    let token: String = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(h.finalize())
        .chars()
        .take(43)
        .collect();

    MachineIdentity {
        machine_id: md5_hex(&format!("machine:{uid}")),
        machine_token: token,
        machine_type: md5_hex(&format!("machinetype:{uid}")).chars().take(18).collect(),
        session_id: md5_hex(&format!("session:{uid}")),
    }
}

// ---------------------------------------------------------------- 会话

/// 一次调用所需的 COSY 会话（`cosy_key` + `info`）。
pub struct CosySession {
    pub uid: String,
    pub machine_id: String,
    pub machine_token: String,
    pub machine_type: String,
    pub cosy_key: String,
    pub info: String,
}

impl CosySession {
    /// 为一个账号建立会话（会做一次 RSA + 一次 AES）。
    pub fn new(account: &QoderAccount) -> Result<Self, String> {
        let token = account
            .access_token
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| "这个账号还没有凭据".to_string())?;

        // temp_key = UUIDv4 hex 前 16 位（协议对齐，别换随机字节）。
        let hex = uuid::Uuid::new_v4().simple().to_string();
        let mut temp_key = [0u8; 16];
        temp_key.copy_from_slice(&hex.as_bytes()[..16]);

        let cosy_key = base64::engine::general_purpose::STANDARD.encode(rsa_encrypt(&temp_key)?);

        let identity = machine_identity(&account.uid);
        let mut map = Map::new();
        map.insert("name".into(), Value::String(account.name.clone()));
        map.insert("aid".into(), Value::String(account.uid.clone()));
        map.insert("uid".into(), Value::String(account.uid.clone()));
        map.insert("yx_uid".into(), Value::String(String::new()));
        map.insert("organization_id".into(), Value::String(String::new()));
        map.insert("organization_name".into(), Value::String(String::new()));
        map.insert(
            "user_type".into(),
            Value::String(DEFAULT_USER_TYPE.to_string()),
        );
        map.insert(
            "security_oauth_token".into(),
            Value::String(token.to_string()),
        );
        map.insert(
            "refresh_token".into(),
            Value::String(account.refresh_token.clone().unwrap_or_default()),
        );

        let plain = json_sorted_compact(&map);
        let info = base64::engine::general_purpose::STANDARD
            .encode(aes_cbc_encrypt(&plain, &temp_key, &temp_key));

        Ok(Self {
            uid: account.uid.clone(),
            machine_id: identity.machine_id,
            machine_token: identity.machine_token,
            machine_type: identity.machine_type,
            cosy_key,
            info,
        })
    }

    fn bearer(&self, body: &str, raw_path: &str) -> String {
        let mut payload = Map::new();
        payload.insert("cosyVersion".into(), Value::String(COSY_VERSION.into()));
        payload.insert("ideVersion".into(), Value::String(String::new()));
        payload.insert("info".into(), Value::String(self.info.clone()));
        payload.insert(
            "requestId".into(),
            Value::String(uuid::Uuid::new_v4().to_string()),
        );
        payload.insert("version".into(), Value::String("v1".into()));

        let payload_b64 =
            base64::engine::general_purpose::STANDARD.encode(json_sorted_compact(&payload));
        let date = now_secs();
        let path = strip_algo_prefix(raw_path);
        let sig = signature(&payload_b64, &self.cosy_key, &date, body, path);
        format!("Bearer COSY.{payload_b64}.{sig}")
    }

    /// 完整头集。`sse=false` 用于模型清单这类一次性 JSON 响应。
    pub fn headers(
        &self,
        body: &str,
        raw_path: &str,
        model_key: Option<&str>,
        sse: bool,
    ) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = vec![
            ("cosy-data-policy".into(), "AGREE".into()),
            ("content-type".into(), "application/json".into()),
            ("cosy-machinetype".into(), self.machine_type.clone()),
            ("cosy-clienttype".into(), "5".into()),
            ("cosy-date".into(), now_secs()),
            ("cosy-user".into(), self.uid.clone()),
            ("cosy-key".into(), self.cosy_key.clone()),
            (
                "accept".into(),
                if sse { "text/event-stream" } else { "application/json" }.into(),
            ),
            ("cosy-clientip".into(), COSY_CLIENT_IP.into()),
            ("authorization".into(), self.bearer(body, raw_path)),
            ("accept-encoding".into(), "identity".into()),
            ("cosy-version".into(), COSY_VERSION.into()),
            ("cosy-machineid".into(), self.machine_id.clone()),
            ("cosy-machinetoken".into(), self.machine_token.clone()),
            ("login-version".into(), "v2".into()),
            ("user-agent".into(), "Go-http-client/2.0".into()),
        ];
        if sse {
            headers.push(("cache-control".into(), "no-cache".into()));
        }
        if let Some(k) = model_key {
            headers.push(("x-model-key".into(), k.to_string()));
            headers.push(("x-model-source".into(), "system".into()));
        }
        headers
    }
}

/// 签名覆盖 path 时**要剥掉 `/algo` 前缀**（与官方客户端一致）。
pub fn strip_algo_prefix(path: &str) -> &str {
    path.strip_prefix("/algo").unwrap_or(path)
}

/// 给上层用的一句话入口：把账号 + 请求体变成可直接发的头。
pub fn headers_for(
    account: &QoderAccount,
    body: &str,
    raw_path: &str,
    model_key: Option<&str>,
    sse: bool,
) -> Result<Vec<(String, String)>, String> {
    let session = CosySession::new(account)?;
    Ok(session.headers(body, raw_path, model_key, sse))
}

/// 区域 → 推理网关基址（对话要走 gateway，不是 openapi）。
pub fn gateway_base(region: QoderRegion) -> &'static str {
    crate::modules::qoder_realm::realm_spec(region).gateway_base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip_for_various_lengths() {
        for len in [0usize, 1, 2, 3, 4, 5, 7, 16, 100, 1000] {
            let plain: Vec<u8> = (0..len).map(|i| (i * 37 % 251) as u8).collect();
            let enc = qoder_encode(&plain);
            let back = qoder_decode(&enc).expect("应能解回");
            assert_eq!(back, plain, "长度 {len} 往返不一致");
        }
    }

    #[test]
    fn encode_uses_custom_alphabet_not_standard() {
        let enc = qoder_encode(b"hello world");
        assert!(!enc.contains('='), "填充符必须换成 $");
        assert!(
            enc.chars().all(|c| CUSTOM_ALPHABET.contains(c) || c == '$'),
            "只能出现自定义字母表里的字符：{enc}"
        );
    }

    #[test]
    fn sorted_json_is_byte_stable() {
        let mut a = Map::new();
        a.insert("b".into(), Value::String("2".into()));
        a.insert("a".into(), Value::String("1".into()));
        let mut b = Map::new();
        b.insert("a".into(), Value::String("1".into()));
        b.insert("b".into(), Value::String("2".into()));
        assert_eq!(json_sorted_compact(&a), json_sorted_compact(&b));
        assert_eq!(
            String::from_utf8(json_sorted_compact(&a)).unwrap(),
            r#"{"a":"1","b":"2"}"#
        );
    }

    #[test]
    fn signature_is_deterministic_and_covers_every_part() {
        let base = signature("p", "k", "d", "b", "/x");
        assert_eq!(base, signature("p", "k", "d", "b", "/x"));
        assert_ne!(base, signature("p2", "k", "d", "b", "/x"));
        assert_ne!(base, signature("p", "k2", "d", "b", "/x"));
        assert_ne!(base, signature("p", "k", "d2", "b", "/x"));
        assert_ne!(base, signature("p", "k", "d", "b2", "/x"));
        assert_ne!(base, signature("p", "k", "d", "b", "/y"));
        assert_eq!(base.len(), 32);
    }

    #[test]
    fn machine_identity_uses_distinct_salts() {
        let i = machine_identity("u-1");
        assert_eq!(i.machine_id.len(), 32);
        assert_eq!(i.machine_token.len(), 43);
        assert_eq!(i.machine_type.len(), 18);
        assert_eq!(i.session_id.len(), 32);
        assert_ne!(i.machine_id, i.session_id);
        assert_ne!(i.machine_id, machine_identity("u-2").machine_id);
    }

    #[test]
    fn algo_prefix_is_stripped_only_when_present() {
        assert_eq!(strip_algo_prefix("/algo/api/v2/x"), "/api/v2/x");
        assert_eq!(strip_algo_prefix("/api/v2/x"), "/api/v2/x");
    }

    #[test]
    fn session_headers_carry_every_required_field() {
        let mut acc = QoderAccount::new(QoderRegion::Cn, "u-1", "测试");
        acc.access_token = Some("tok-abc".into());
        let s = CosySession::new(&acc).unwrap();
        let h = s.headers("{\"a\":1}", "/algo/api/v2/service/pro/sse/agent_chat_generation", None, true);
        let get = |name: &str| {
            h.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        for k in [
            "cosy-data-policy",
            "cosy-machinetype",
            "cosy-date",
            "cosy-user",
            "cosy-key",
            "cosy-clientip",
            "authorization",
            "cosy-version",
            "cosy-machineid",
            "cosy-machinetoken",
            "login-version",
        ] {
            assert!(!get(k).is_empty(), "缺少头 {k}");
        }
        assert!(get("authorization").starts_with("Bearer COSY."));
        assert_eq!(get("cosy-user"), "u-1");
        assert_eq!(get("cosy-version"), COSY_VERSION);
        assert_eq!(get("accept"), "text/event-stream");
    }

    #[test]
    fn account_without_token_is_refused() {
        let acc = QoderAccount::new(QoderRegion::Cn, "u-2", "无凭据");
        assert!(CosySession::new(&acc).is_err());
    }
}
