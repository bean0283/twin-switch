//! 积分共享 · 网关 Key。
//!
//! 客户端拿这把 Key 访问本机网关（`127.0.0.1:57892`）。三条规矩：
//!
//! 1. **明文只在创建时返回一次**：库里只存 `sha256` 与前 4 / 后 4 位。
//! 2. **吊销 ≠ 删除**：吊销立即失效但可恢复；只有已吊销的才允许删除。
//! 3. 一把 Key 可以限定作用域（留空 = 全部），限定值与作用域名逐字比对。
//!
//! 落在 `~/.twin-switch/gateway-keys.json`（与 0.3.x 其它数据同一个根）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::modules::config::store_dir;

/// 统一网关端口（用户定：57892）。
pub const GATEWAY_PORT: u16 = 57892;
/// Key 前缀，便于人眼区分。
pub const KEY_PREFIX: &str = "sk-ts-";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub suffix: String,
    /// `sha256(明文)` 小写 hex。
    pub hash: String,
    /// 允许的作用域；空 = 全部。
    #[serde(default)]
    pub scopes: Vec<String>,
    pub created_at: i64,
    #[serde(default)]
    pub revoked_at: Option<i64>,
    #[serde(default)]
    pub last_used_at: Option<i64>,
}

impl GatewayKey {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    pub fn allows(&self, scope: &str) -> bool {
        self.is_active() && (self.scopes.is_empty() || self.scopes.iter().any(|s| s == scope))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyFile {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    keys: Vec<GatewayKey>,
}

fn one() -> u32 {
    1
}

impl Default for KeyFile {
    fn default() -> Self {
        Self { version: 1, keys: Vec::new() }
    }
}

pub fn keys_file() -> PathBuf {
    store_dir().join("gateway-keys.json")
}

fn read() -> Result<KeyFile, String> {
    let path = keys_file();
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("Key 库格式损坏：{}（{e}）", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KeyFile::default()),
        Err(e) => Err(format!("读取 Key 库失败：{e}")),
    }
}

fn write(file: &KeyFile) -> Result<(), String> {
    let path = keys_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let body = serde_json::to_vec_pretty(file).map_err(|e| format!("序列化失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落盘失败：{e}"))
}

pub fn hash_of(plaintext: &str) -> String {
    let mut h = Sha256::new();
    h.update(plaintext.as_bytes());
    hex::encode(h.finalize())
}

/// 列表（给界面看，绝不含明文）。
pub fn list() -> Result<Value, String> {
    let file = read()?;
    Ok(serde_json::json!(file.keys))
}

/// 新建。返回 `(明文, 记录)` —— 明文只有这一次机会拿到。
pub fn create(name: &str, scopes: Vec<String>) -> Result<(String, GatewayKey), String> {
    let name = name.trim();
    let plaintext = format!("{KEY_PREFIX}{}", uuid::Uuid::new_v4().simple());
    let prefix: String = plaintext.chars().take(6).collect();
    let suffix: String = {
        let rev: String = plaintext.chars().rev().take(4).collect();
        rev.chars().rev().collect()
    };
    let rec = GatewayKey {
        id: uuid::Uuid::new_v4().to_string(),
        name: if name.is_empty() { "默认".into() } else { name.to_string() },
        prefix,
        suffix,
        hash: hash_of(&plaintext),
        scopes,
        created_at: crate::modules::qoder_accounts::now_ms(),
        revoked_at: None,
        last_used_at: None,
    };
    let mut file = read()?;
    file.keys.push(rec.clone());
    write(&file)?;
    Ok((plaintext, rec))
}

/// 校验明文，返回命中的 Key（不改状态）。
pub fn verify(plaintext: &str) -> Result<Option<GatewayKey>, String> {
    let h = hash_of(plaintext.trim());
    let file = read()?;
    Ok(file.keys.into_iter().find(|k| k.is_active() && k.hash == h))
}

fn mutate(id: &str, f: impl FnOnce(&mut GatewayKey)) -> Result<GatewayKey, String> {
    let mut file = read()?;
    let key = file
        .keys
        .iter_mut()
        .find(|k| k.id == id)
        .ok_or_else(|| format!("找不到 Key {id}"))?;
    f(key);
    let out = key.clone();
    write(&file)?;
    Ok(out)
}

pub fn revoke(id: &str) -> Result<GatewayKey, String> {
    let now = crate::modules::qoder_accounts::now_ms();
    mutate(id, |k| {
        if k.revoked_at.is_none() {
            k.revoked_at = Some(now);
        }
    })
}

pub fn restore(id: &str) -> Result<GatewayKey, String> {
    mutate(id, |k| k.revoked_at = None)
}

pub fn delete(id: &str) -> Result<(), String> {
    let mut file = read()?;
    let idx = file
        .keys
        .iter()
        .position(|k| k.id == id)
        .ok_or_else(|| format!("找不到 Key {id}"))?;
    if file.keys[idx].is_active() {
        return Err("这把 Key 还在用，请先吊销再删除".into());
    }
    file.keys.remove(idx);
    write(&file)
}

/// 记一次使用（失败不影响请求）。
pub fn touch(id: &str) {
    let _ = mutate(id, |k| k.last_used_at = Some(crate::modules::qoder_accounts::now_ms()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_distinct() {
        assert_eq!(hash_of("abc"), hash_of("abc"));
        assert_ne!(hash_of("abc"), hash_of("abd"));
        assert_eq!(hash_of("abc").len(), 64);
    }

    #[test]
    fn scope_allow_list_is_exact() {
        let mut k = GatewayKey {
            id: "1".into(), name: "t".into(), prefix: "p".into(), suffix: "s".into(),
            hash: hash_of("x"), scopes: vec!["trae-cn".into()],
            created_at: 0, revoked_at: None, last_used_at: None,
        };
        assert!(k.allows("trae-cn"));
        assert!(!k.allows("trae-cn-x"));
        assert!(!k.allows("wb-cn"));
        k.revoked_at = Some(1);
        assert!(!k.allows("trae-cn"), "吊销后一律拒绝");
    }

    #[test]
    fn empty_scopes_mean_all() {
        let k = GatewayKey {
            id: "1".into(), name: "t".into(), prefix: "p".into(), suffix: "s".into(),
            hash: hash_of("x"), scopes: vec![],
            created_at: 0, revoked_at: None, last_used_at: None,
        };
        assert!(k.allows("qoder-cn"));
    }
}
