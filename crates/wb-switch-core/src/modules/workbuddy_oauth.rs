//! WorkBuddy（国内版）OAuth 扫码登录采集。
//!
//! 流程（复刻官方客户端的登录链路，三步）：
//! 1. `POST {api}/v2/plugin/auth/state?platform=workbuddy` → 拿到 `state` 与授权页 `authUrl`；
//! 2. 用户在浏览器打开授权页完成登录；
//! 3. 轮询 `GET {api}/v2/plugin/auth/token?state=…` 拿 token，
//!    再 `GET {api}/v2/plugin/login/account?state=…` 拉账号资料，入库。
//!
//! 只做国内版：domain 必须是 `*.codebuddy.cn` 系，其它域名一律拒绝入库
//! （否则切换后客户端会一直处于登录失效状态）。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::modules::config::{http_request, now_ms};
use crate::modules::workbuddy_auth::API_ENDPOINT;
use crate::modules::workbuddy_vault;

const API_PREFIX: &str = "/v2/plugin";
const OAUTH_PLATFORM: &str = "workbuddy";
/// 登录请求有效期（秒）。
const OAUTH_TIMEOUT_SECONDS: i64 = 600;

struct OAuthInfo {
    state: String,
    expires_at: i64,
    done: bool,
    result: Option<Value>,
    error: Option<String>,
}

static OAUTH_STATES: OnceLock<Mutex<HashMap<String, OAuthInfo>>> = OnceLock::new();

fn oauth_states() -> &'static Mutex<HashMap<String, OAuthInfo>> {
    OAUTH_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_secs() -> i64 {
    now_ms() / 1000
}

fn state_url() -> String {
    format!("{API_ENDPOINT}{API_PREFIX}/auth/state?platform={OAUTH_PLATFORM}")
}

fn token_url(state: &str) -> String {
    format!("{API_ENDPOINT}{API_PREFIX}/auth/token?state={state}")
}

fn account_url(state: &str) -> String {
    format!("{API_ENDPOINT}{API_PREFIX}/login/account?state={state}")
}

/// 国内版只接受这些域；其它域（含国际版）拒绝入库。
fn domain_mismatch_error(domain: &str) -> Option<String> {
    let d = domain.trim().to_ascii_lowercase();
    if d.is_empty() || d.ends_with("codebuddy.cn") || d.ends_with("workbuddy.cn") {
        return None;
    }
    Some(format!(
        "登录响应的 domain（{domain}）与国内版不符，已拒绝入库"
    ))
}

/// 发起登录：返回 `loginId` / `verificationUri`（授权页） / `expiresIn`。
pub async fn oauth_start() -> Result<Value, String> {
    let login_id = format!("wb_{}", uuid::Uuid::new_v4().simple());
    let resp = http_request(&state_url(), "POST", Some(json!({})), None).await;
    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    let state = data
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if state.is_empty() {
        let snippet = serde_json::to_string(&resp)
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect::<String>();
        return Err(format!("auth/state 响应缺少 state: {snippet}"));
    }
    let auth_url = data
        .get("authUrl")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("auth_url").and_then(|v| v.as_str()))
        .or_else(|| data.get("url").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{API_ENDPOINT}/login?state={state}"));

    let mut map = oauth_states().lock().unwrap();
    map.insert(
        login_id.clone(),
        OAuthInfo {
            state,
            expires_at: now_secs() + OAUTH_TIMEOUT_SECONDS,
            done: false,
            result: None,
            error: None,
        },
    );
    drop(map);

    Ok(json!({
        "loginId": login_id,
        "verificationUri": auth_url,
        "expiresIn": OAUTH_TIMEOUT_SECONDS,
    }))
}

fn fail_oauth(login_id: &str, error: String) -> Value {
    let mut map = oauth_states().lock().unwrap();
    if let Some(info) = map.get_mut(login_id) {
        info.done = true;
        info.error = Some(error.clone());
    }
    json!({"done": true, "error": error})
}

/// 轮询一次；已完成、超时或成功时 `done = true`。
///
/// 返回 `{done, result?|error?}`。`result` 是入库后的账号（脱敏后的展示字段）。
pub async fn oauth_poll(login_id: &str) -> Value {
    let state = {
        let mut map = oauth_states().lock().unwrap();
        let Some(info) = map.get_mut(login_id) else {
            return json!({"done": true, "error": "登录请求不存在"});
        };
        if info.done {
            return json!({"done": true, "result": info.result.clone(), "error": info.error.clone()});
        }
        if now_secs() > info.expires_at {
            info.done = true;
            info.error = Some("登录超时".to_string());
            return json!({"done": true, "error": "登录超时"});
        }
        info.state.clone()
    };

    let resp = http_request(&token_url(&state), "GET", None, None).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 && code != 200 {
        return json!({"done": false});
    }
    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    let access_token = data
        .get("accessToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("access_token").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    if access_token.is_empty() {
        return json!({"done": false});
    }

    let domain = data.get("domain").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(error) = domain_mismatch_error(domain) {
        return fail_oauth(login_id, error);
    }

    // 拉账号资料
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {access_token}"));
    if !domain.is_empty() {
        headers.insert("X-Domain".to_string(), domain.to_string());
    }
    let acc_resp = http_request(&account_url(&state), "GET", None, Some(&headers)).await;
    let acc_data = acc_resp.get("data").cloned().unwrap_or_else(|| json!({}));
    if let Some(error) = domain_mismatch_error(acc_data.get("domain").and_then(|v| v.as_str()).unwrap_or("")) {
        return fail_oauth(login_id, error);
    }

    let expires_at = norm_ts(data.get("expiresAt").or_else(|| data.get("expires_at")))
        .or_else(|| {
            data.get("expiresIn")
                .and_then(|v| v.as_i64())
                .map(|e| now_ms() + e * 1000)
        });
    let refresh_expires_at =
        norm_ts(data.get("refreshExpiresAt").or_else(|| data.get("refresh_expires_at"))).or_else(|| {
            data.get("refreshExpiresIn")
                .and_then(|v| v.as_i64())
                .map(|e| now_ms() + e * 1000)
        });

    let account = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "uid": acc_data.get("uid").and_then(|v| v.as_str()),
        "nickname": acc_data.get("nickname").and_then(|v| v.as_str()),
        "email": acc_data.get("email").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()),
        "access_token": access_token,
        "refresh_token": data.get("refreshToken").and_then(|v| v.as_str())
            .or_else(|| data.get("refresh_token").and_then(|v| v.as_str())),
        "token_type": data.get("tokenType").and_then(|v| v.as_str())
            .or_else(|| data.get("token_type").and_then(|v| v.as_str()))
            .unwrap_or("Bearer"),
        "domain": domain,
        "expiresAt": expires_at,
        "refreshExpiresAt": refresh_expires_at,
        "auth_raw": data,
        "profile_raw": acc_data,
        "createdAt": now_ms(),
    });

    let saved = match workbuddy_vault::upsert(account) {
        Ok(v) => v,
        Err(e) => return fail_oauth(login_id, format!("保存账号失败: {e}")),
    };

    let result = json!({
        "id": saved.get("id"),
        "uid": saved.get("uid"),
        "name": workbuddy_vault::display_name(&saved),
    });
    let mut map = oauth_states().lock().unwrap();
    if let Some(info) = map.get_mut(login_id) {
        info.done = true;
        info.result = Some(result.clone());
    }
    drop(map);
    json!({"done": true, "result": result})
}

/// 取消 / 丢弃一次登录请求。
pub fn oauth_stop(login_id: &str) {
    let mut map = oauth_states().lock().unwrap();
    map.remove(login_id);
}

fn norm_ts(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_built_for_cn_endpoint() {
        assert_eq!(
            state_url(),
            "https://www.codebuddy.cn/v2/plugin/auth/state?platform=workbuddy"
        );
        assert!(token_url("s1").ends_with("/auth/token?state=s1"));
        assert!(account_url("s1").ends_with("/login/account?state=s1"));
    }

    #[test]
    fn only_cn_domains_are_accepted() {
        for ok in ["", "www.codebuddy.cn", "WWW.CODEBUDDY.CN", "www.workbuddy.cn"] {
            assert!(domain_mismatch_error(ok).is_none(), "{ok} 应被接受");
        }
        for bad in ["www.workbuddy.ai", "example.com", "codebuddy.cn.evil.com"] {
            assert!(domain_mismatch_error(bad).is_some(), "{bad} 应被拒绝");
        }
    }

    #[test]
    fn polling_unknown_login_id_is_rejected() {
        // 同步断言：不存在的 loginId 应立刻返回 done
        let id = "__nope__";
        let mut map = oauth_states().lock().unwrap();
        assert!(map.get(id).is_none());
        map.insert(
            id.to_string(),
            OAuthInfo { state: "s".into(), expires_at: 0, done: false, result: None, error: None },
        );
        drop(map);
        oauth_stop(id);
        let map = oauth_states().lock().unwrap();
        assert!(map.get(id).is_none(), "stop 后应移除登录请求");
    }

    #[test]
    fn norm_ts_handles_number_and_string() {
        assert_eq!(norm_ts(Some(&json!(1791912333558i64))), Some(1791912333558));
        assert_eq!(norm_ts(Some(&json!("1791912333558"))), Some(1791912333558));
        assert_eq!(norm_ts(Some(&json!("abc"))), None);
        assert_eq!(norm_ts(None), None);
    }
}
