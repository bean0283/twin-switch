//! Qoder 浏览器登录（设备授权 + PKCE），照参考实现 `qoder/oauth.rs`。
//!
//! 流程（**与 WorkBuddy 的「扫码添加」同构**，用户点的也是同一个位置）：
//!
//! 1. [`start`] 本地生成 PKCE + nonce + machine_id，返回一个授权页 URL（**不发网络请求**）；
//! 2. 用户在浏览器里登录并选择账号；
//! 3. 前端每 2 秒调一次 [`poll`]，`202/404` = 还没授权，`2xx + token` = 成功；
//! 4. 成功时用 token 拉 `/api/v1/userinfo` 拿 uid / 昵称，写进对应区域的账号库。
//!
//! 三条判据：
//!
//! - **区域绑定**：用 A 区的 loginId 去轮询 B 区直接拒绝，否则会把 A 区登录态写进 B 区库；
//! - **身份不全就不写**：userinfo 没 id 就报错，不编一个 uid；
//! - **网络抖动保持 pending**：一次抖动不该让用户重新点登录（按 TTL 继续轮询）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use serde_json::{json, Value};

use crate::modules::qoder_accounts::{self, QoderAccount};
use crate::modules::qoder_realm::{realm_spec, QoderRegion};

const PATH_DEVICE_TOKEN_POLL: &str = "/api/v1/deviceToken/poll";
const PATH_USERINFO: &str = "/api/v1/userinfo";
/// 登录会话有效期（5 分钟），过点自动失败。
const LOGIN_TTL_MS: i64 = 5 * 60 * 1000;

struct Session {
    region: QoderRegion,
    nonce: String,
    verifier: String,
    expires_at: i64,
    done: bool,
    result: Option<Value>,
    error: Option<String>,
}

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    static S: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- 发起

/// 发起登录：返回 `{loginId, verificationUri, expiresInSecs}`。
pub fn start(region: QoderRegion) -> Result<Value, String> {
    let machine_id = machine_id_for(region)?;
    let (verifier, challenge) = pkce_pair();
    let nonce = nonce_for(region);
    let login_id = uuid::Uuid::new_v4().to_string();
    let url = build_authorize_url(region, &machine_id, &nonce, &challenge);

    let mut map = sessions().lock().map_err(|_| "登录会话锁失效".to_string())?;
    map.insert(
        login_id.clone(),
        Session {
            region,
            nonce,
            verifier,
            expires_at: now_ms() + LOGIN_TTL_MS,
            done: false,
            result: None,
            error: None,
        },
    );

    Ok(json!({
        "loginId": login_id,
        "verificationUri": url,
        "expiresInSecs": LOGIN_TTL_MS / 1000,
        "region": region.as_str(),
    }))
}

/// 轮询一次。`{done:false}` 表示继续等；`{done:true, result}` 表示成功。
pub async fn poll(region: QoderRegion, login_id: &str) -> Value {
    // ① 会话检查（全部在任何 I/O 之前）
    let (nonce, verifier) = {
        let Ok(mut map) = sessions().lock() else {
            return json!({ "done": true, "error": "登录会话锁失效" });
        };
        let Some(s) = map.get_mut(login_id) else {
            return json!({ "done": true, "error": "登录请求不存在（可能已超时，请重新点登录）" });
        };
        if s.region != region {
            return json!({ "done": true, "error": "登录请求与目标区域不匹配" });
        }
        if s.done {
            return json!({ "done": true, "result": s.result, "error": s.error });
        }
        if now_ms() > s.expires_at {
            s.done = true;
            s.error = Some("登录超时".into());
            return json!({ "done": true, "error": "登录超时" });
        }
        (s.nonce.clone(), s.verifier.clone())
    };

    let spec = realm_spec(region);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return json!({ "done": true, "error": format!("构造客户端失败：{e}") }),
    };

    let url = format!(
        "{}{PATH_DEVICE_TOKEN_POLL}?nonce={nonce}&verifier={verifier}&challenge_method=S256",
        spec.openapi_base
    );
    let resp = match client
        .get(&url)
        .header("accept", "application/json, text/plain, */*")
        .header("user-agent", spec.user_agent)
        .send()
        .await
    {
        Ok(r) => r,
        // 网络抖动 ⇒ 保持 pending，让前端按 TTL 继续轮询。
        Err(_) => return json!({ "done": false }),
    };

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if matches!(status.as_u16(), 202 | 404) {
        return json!({ "done": false });
    }
    if !status.is_success() {
        let detail: String = body.chars().take(160).collect();
        let err = format!("轮询失败：HTTP {} {detail}", status.as_u16());
        finish(login_id, None, Some(err.clone()));
        return json!({ "done": true, "error": err });
    }
    let Ok(data) = serde_json::from_str::<Value>(&body) else {
        let err = format!("轮询响应不是 JSON：{}", body.chars().take(160).collect::<String>());
        finish(login_id, None, Some(err.clone()));
        return json!({ "done": true, "error": err });
    };

    let token = field(&data, "token")
        .or_else(|| field(&data, "device_token"))
        .unwrap_or_default();
    if token.is_empty() {
        // 2xx 但还没令牌 ⇒ 与 202 同义。
        return json!({ "done": false });
    }
    let refresh_token = field(&data, "refresh_token").filter(|v| !v.is_empty());

    // ② 拉身份（拿 uid / 昵称）—— 拿不到就不写库。
    let userinfo = match fetch_userinfo(spec.openapi_base, spec.user_agent, &token).await {
        Ok(v) => v,
        Err(e) => {
            finish(login_id, None, Some(e.clone()));
            return json!({ "done": true, "error": e });
        }
    };
    let uid = match userinfo.get("id") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    if uid.is_empty() {
        let err = "userinfo 里没有 id —— 拒绝写入半张账号".to_string();
        finish(login_id, None, Some(err.clone()));
        return json!({ "done": true, "error": err });
    }
    let nickname = userinfo
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut acc = QoderAccount::new(region, &uid, nickname.clone().unwrap_or_default());
    acc.access_token = Some(token);
    acc.refresh_token = refresh_token;
    acc.source = qoder_accounts::SOURCE_OAUTH.to_string();

    // ③ 入库：同区域同 uid 已存在就更新 token，不新增一条。
    let existing = qoder_accounts::list(region)
        .ok()
        .and_then(|l| l.into_iter().find(|x| x.uid == uid));
    let stored = match existing {
        Some(old) => {
            let token = acc.access_token.clone();
            let refresh = acc.refresh_token.clone();
            let name = acc.name.clone();
            let source = acc.source.clone();
            match qoder_accounts::update(region, &old.id, |x| {
                x.access_token = token;
                x.refresh_token = refresh;
                x.name = name;
                x.source = source;
            }) {
                Ok(v) => Ok(v),
                Err(e) => Err(e),
            }
        }
        None => qoder_accounts::add(acc),
    };
    let result = match stored {
        Ok(v) => json!(v),
        Err(e) => {
            finish(login_id, None, Some(e.clone()));
            return json!({ "done": true, "error": e });
        }
    };

    finish(login_id, Some(result.clone()), None);
    json!({ "done": true, "result": result })
}

pub fn cancel(login_id: &str) -> bool {
    sessions()
        .lock()
        .map(|mut m| m.remove(login_id).is_some())
        .unwrap_or(false)
}

fn finish(login_id: &str, result: Option<Value>, error: Option<String>) {
    if let Ok(mut map) = sessions().lock() {
        if let Some(s) = map.get_mut(login_id) {
            s.done = true;
            s.result = result;
            s.error = error;
        }
    }
}

async fn fetch_userinfo(base: &str, ua: &str, token: &str) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("构造客户端失败：{e}"))?;
    let resp = client
        .get(format!("{base}{PATH_USERINFO}"))
        .header("accept", "application/json, text/plain, */*")
        .header("user-agent", ua)
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("拉取账号身份失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "拉取账号身份失败：HTTP {} {}",
            status.as_u16(),
            body.chars().take(160).collect::<String>()
        ));
    }
    serde_json::from_str(&body).map_err(|e| format!("账号身份响应不是 JSON：{e}"))
}

fn field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------- 工具

/// PKCE：verifier 为 64 位 hex；challenge = BASE64URL-NOPAD(SHA256(verifier))。
fn pkce_pair() -> (String, String) {
    use sha2::Digest;
    let mut buf = [0u8; 32];
    // getrandom 已在依赖里（trae_oauth 用），失败就退化成 uuid（不影响 PKCE 正确性）。
    let verifier = match getrandom::getrandom(&mut buf) {
        Ok(()) => buf.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        Err(_) => uuid::Uuid::new_v4().simple().to_string(),
    };
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

fn nonce_for(region: QoderRegion) -> String {
    let u = uuid::Uuid::new_v4();
    if realm_spec(region).nonce_dashed {
        u.to_string()
    } else {
        u.simple().to_string()
    }
}

fn build_authorize_url(region: QoderRegion, machine_id: &str, nonce: &str, challenge: &str) -> String {
    let spec = realm_spec(region);
    let mut q = format!(
        "challenge={challenge}&challenge_method=S256&nonce={nonce}&client_id={}&machine_id={machine_id}",
        spec.client_id
    );
    if spec.send_redirect_uri {
        q.push_str(&format!("&redirect_uri={}", spec.redirect_uri));
    }
    format!("{}/device/selectAccounts?{q}", spec.website)
}

/// 本区域登录用的 `machine_id`。
///
/// 优先级：CLI 的 `.auth/machine_id` → 桌面端 `auth.machine-id` →
/// 本机生成一次并持久化（保证同一区域多次登录复用同一台「虚拟设备」）。
fn machine_id_for(region: QoderRegion) -> Result<String, String> {
    let spec = realm_spec(region);
    let home = dirs::home_dir().ok_or_else(|| "取不到用户主目录".to_string())?;

    let cli = home.join(spec.cli_home_dir).join(".auth").join("machine_id");
    if let Some(v) = read_trimmed(&cli) {
        return Ok(v);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let desktop = std::path::PathBuf::from(appdata)
            .join(spec.desktop_app_dir)
            .join("auth.machine-id");
        if let Some(v) = read_trimmed(&desktop) {
            return Ok(v);
        }
    }
    let store = crate::modules::config::store_dir()
        .join("qoder")
        .join(format!("oauth_machine_id.{}.txt", region.as_str()));
    if let Some(v) = read_trimmed(&store) {
        return Ok(v);
    }
    let generated = uuid::Uuid::new_v4().to_string();
    if let Some(dir) = store.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    std::fs::write(&store, generated.as_bytes()).map_err(|e| format!("保存 machine_id 失败：{e}"))?;
    Ok(generated)
}

fn read_trimmed(path: &std::path::Path) -> Option<String> {
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_carries_pkce_and_client_id() {
        let region = QoderRegion::Cn;
        let url = build_authorize_url(region, "m-1", "n-1", "c-1");
        assert!(url.starts_with("https://qoder.com.cn/device/selectAccounts?"));
        assert!(url.contains("challenge=c-1"));
        assert!(url.contains("challenge_method=S256"));
        assert!(url.contains("nonce=n-1"));
        assert!(url.contains("machine_id=m-1"));
        assert!(url.contains(&format!("client_id={}", realm_spec(region).client_id)));
        // 国内版带 redirect_uri，国际版不带（实测差异）。
        assert!(url.contains("redirect_uri="));
        let global = build_authorize_url(QoderRegion::Global, "m", "n", "c");
        assert!(!global.contains("redirect_uri="));
    }

    #[test]
    fn pkce_challenge_is_sha256_urlsafe_of_verifier() {
        use sha2::Digest;
        let (verifier, challenge) = pkce_pair();
        assert_eq!(verifier.len(), 64);
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expect);
        assert!(!challenge.contains('='));
    }

    #[test]
    fn nonce_form_follows_realm() {
        assert!(nonce_for(QoderRegion::Cn).contains('-'), "国内版带横线");
        assert!(!nonce_for(QoderRegion::Global).contains('-'), "国际版是 32 位 hex");
    }

}
