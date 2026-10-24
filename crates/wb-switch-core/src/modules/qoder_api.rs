//! Qoder 开放接口（额度 / 套餐）。**接口与解析规则照抄参考实现。**
//!
//! 只实现查询，不改任何远端状态。
//!
//! 两条从参考实现学来的关键点：
//!
//! 1. **请求头不是只有 Authorization**：还要 `X-Request-ID` / `X-Machine-ID` /
//!    `X-Session-ID`，三者都由 `md5("{salt}:{uid}")` 稳定派生（见
//!    [`qoder_realm::derive_id`]）。少一个就 401/403。
//! 2. **额度字段是浮点**：实测上游给的是 `99.0 / 100.0`。用 `as_i64` 解析会把
//!    「有余额」读成 `0`，界面就变成「已用完」—— 这是**谎报**，必须按 f64 读。

use serde_json::{Map, Value};

use crate::modules::qoder_accounts::QoderAccount;
use crate::modules::qoder_realm::{
    derive_id, realm_spec, QoderRegion, PATH_QUOTA_USAGE, PATH_USER_PLAN,
};

/// 额度视图。
#[derive(Debug, Clone, serde::Serialize)]
pub struct QuotaView {
    pub remaining: f64,
    pub used: f64,
    pub total: f64,
    pub is_exceeded: bool,
    /// 原样保留上游响应，界面可展开看。
    pub raw: Value,
}

impl QuotaView {
    /// 剩余比例（0–1）；total 为 0 时返回 None（**不是 0**）。
    pub fn ratio(&self) -> Option<f64> {
        if self.total > 0.0 {
            Some((self.remaining / self.total).clamp(0.0, 1.0))
        } else {
            None
        }
    }
}

/// 按区域拼请求头。
pub fn openapi_headers(region: QoderRegion, uid: &str, access_token: &str) -> Vec<(String, String)> {
    let spec = realm_spec(region);
    let request_id = format!(
        "{}-{:06}",
        derive_id(uid, "req"),
        now_ms() % 1_000_000
    );
    vec![
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/json, text/plain, */*".into()),
        ("User-Agent".into(), spec.user_agent.to_string()),
        ("Authorization".into(), format!("Bearer {access_token}")),
        ("X-Request-ID".into(), request_id),
        ("X-Machine-ID".into(), derive_id(uid, "machine")),
        ("X-Session-ID".into(), derive_id(uid, "session")),
        ("Origin".into(), spec.website.to_string()),
        ("Referer".into(), format!("{}/", spec.website)),
    ]
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败：{e}"))
}

/// 查额度。
pub async fn fetch_quota(account: &QoderAccount) -> Result<QuotaView, String> {
    let token = account
        .access_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "这个账号还没有凭据（先在账号页填一次 PAT 或登录）".to_string())?;
    let spec = realm_spec(account.region);
    let mut req = client()?
        .get(format!("{}{}", spec.openapi_base, PATH_QUOTA_USAGE))
        .timeout(std::time::Duration::from_secs(30));
    for (k, v) in openapi_headers(account.region, &account.uid, token) {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| format!("额度查询失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "额度查询失败：HTTP {} {}",
            status.as_u16(),
            body.chars().take(160).collect::<String>()
        ));
    }
    let data: Value =
        serde_json::from_str(&body).map_err(|e| format!("额度响应不是 JSON：{e}"))?;
    parse_quota(&data)
}

/// 解析额度响应（与参考实现同一套判据）。
pub fn parse_quota(payload: &Value) -> Result<QuotaView, String> {
    let user = payload.get("userQuota").and_then(Value::as_object);
    let add_on = payload.get("addOnQuota").and_then(Value::as_object);
    if user.is_none() && add_on.is_none() {
        return Err("额度响应缺少 userQuota / addOnQuota（解析不了，拒绝写 0）".into());
    }
    let num = |src: Option<&Map<String, Value>>, key: &str| -> f64 {
        src.and_then(|m| m.get(key)).and_then(Value::as_f64).unwrap_or(0.0)
    };
    let remaining = num(user, "remaining") + num(add_on, "remaining");
    let used = num(user, "used") + num(add_on, "used");
    let total = num(user, "total") + num(add_on, "total");
    let is_exceeded = payload
        .get("isQuotaExceeded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_exceeded && remaining == 0.0 && total == 0.0 {
        return Err("额度响应为退化形态（exceeded 且全 0）—— 拒绝写入".into());
    }
    Ok(QuotaView { remaining, used, total, is_exceeded, raw: payload.clone() })
}

/// 查套餐（Pro Trial 之类），原样返回。
pub async fn fetch_plan(account: &QoderAccount) -> Result<Value, String> {
    let token = account
        .access_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "这个账号还没有凭据".to_string())?;
    let spec = realm_spec(account.region);
    let mut req = client()?
        .get(format!("{}{}", spec.openapi_base, PATH_USER_PLAN))
        .timeout(std::time::Duration::from_secs(15));
    for (k, v) in openapi_headers(account.region, &account.uid, token) {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| format!("套餐查询失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "套餐查询失败：HTTP {} {}",
            status.as_u16(),
            body.chars().take(160).collect::<String>()
        ));
    }
    serde_json::from_str(&body).map_err(|e| format!("套餐响应不是 JSON：{e}"))
}

/// 刷新令牌：用 `refresh_token` 换一份新的访问令牌，并写回账号。
///
/// 端点按 refresh token 的前缀固化（参考实现 `token.rs` 的穷举表）：
/// - `drt-` → `/api/v1/deviceToken/refresh`
/// - `jrt-` → `/api/v1/jobToken/refresh`
///
/// ⚠️ 认不出前缀（含空白字符、PAT、访问令牌）**直接报错**，不猜端点 ——
/// 打错端点只会拿到一个与根因毫不相干的 401。
pub async fn refresh_token(account: &QoderAccount) -> Result<QoderAccount, String> {
    let rt = account
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "这个账号没有 refresh_token，无法刷新（需要重新登录一次）".to_string())?;
    if rt.chars().any(char::is_whitespace) {
        return Err("refresh_token 里含空白字符（多半是复制时夹带了换行/引号）".into());
    }
    let path = if rt.starts_with("drt-") {
        "/api/v1/deviceToken/refresh"
    } else if rt.starts_with("jrt-") {
        "/api/v1/jobToken/refresh"
    } else {
        return Err(format!(
            "认不出 refresh_token 的形态（前缀 {:?}）；只支持 drt- / jrt-",
            rt.chars().take(4).collect::<String>()
        ));
    };

    let spec = realm_spec(account.region);
    let client = client()?;
    let resp = client
        .post(format!("{}{path}", spec.openapi_base))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("user-agent", spec.user_agent)
        // 刷新端点**不带 Authorization**（凭据就在 body 里）。
        .json(&serde_json::json!({ "refresh_token": rt }))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("刷新令牌请求失败：{e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "刷新令牌失败：HTTP {} {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ));
    }
    let data: Value = serde_json::from_str(&body)
        .map_err(|e| format!("刷新响应不是 JSON（{e}）：{}", body.chars().take(200).collect::<String>()))?;

    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| {
            data.get(*k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
    };
    let new_access = pick(&["token", "access_token", "device_token", "accessToken"])
        .ok_or_else(|| {
            format!(
                "刷新响应里没有访问令牌：{}",
                body.chars().take(200).collect::<String>()
            )
        })?;
    let new_refresh = pick(&["refresh_token", "refreshToken"]);

    // 过期时间三种形态都认：
    //   `expires_in`（秒，相对）→ `expires_at` 数字（秒/毫秒）→ `expires_at` **RFC3339 字符串**
    // 实测这个端点返回的是字符串（`"2026-11-19T14:03:14Z"`），只认数字会把它默默丢掉。
    let expires_at = data
        .get("expires_in")
        .and_then(Value::as_i64)
        .map(|sec| now_ms() + sec * 1000)
        .or_else(|| {
            data.get("expires_at").and_then(|v| {
                v.as_i64()
                    .map(|n| if n < 1_000_000_000_000 { n * 1000 } else { n })
                    .or_else(|| {
                        v.as_str().and_then(|s| {
                            chrono::DateTime::parse_from_rfc3339(s)
                                .ok()
                                .map(|t| t.timestamp_millis())
                        })
                    })
            })
        });

    let now = now_ms();
    crate::modules::qoder_accounts::update(account.region, &account.id, |a| {
        a.access_token = Some(new_access.clone());
        if let Some(r) = new_refresh.clone() {
            a.refresh_token = Some(r);
        }
        if let Some(exp) = expires_at {
            a.expires_at = Some(exp);
        }
        a.credits_updated_at = a.credits_updated_at.or(Some(now));
    })
}

/// 上游说「令牌过期」了没有。
///
/// 实测形态：`HTTP 401 {"code":"TOKEN_EXPIRE","message":"token is not active"}`。
fn is_token_expired(msg: &str) -> bool {
    msg.contains("TOKEN_EXPIRE")
        || msg.contains("token is not active")
        || msg.contains("TOKEN_INVALID")
        || msg.contains("invalid_token")
}

/// 查一次并写进账号记录（额度 / 套餐 / 时间戳）。返回更新后的账号。
pub async fn refresh_account(account: &QoderAccount) -> Result<QoderAccount, String> {
    // 导入的登录态里 token 往往已经过期（桌面端自己会续期，我们直读就撞上）。
    // 所以第一次 401 + TOKEN_EXPIRE 时，用 refresh_token 续一次再重试 —— 只重试一次。
    let quota = match fetch_quota(account).await {
        Ok(q) => q,
        Err(e) if is_token_expired(&e) && account.refresh_token.is_some() => {
            let renewed = refresh_token(account).await.map_err(|re| {
                format!("令牌已过期（{e}），续期也失败：{re}。请用「浏览器登录」重新登录这个账号")
            })?;
            fetch_quota(&renewed).await?
        }
        Err(e) => return Err(e),
    };
    let plan = fetch_plan(account).await.ok();
    let plan_name = plan
        .as_ref()
        .and_then(|v| v.get("planName").or_else(|| v.get("plan")).or_else(|| v.get("name")))
        .and_then(Value::as_str)
        .map(str::to_string);

    let now = crate::modules::qoder_accounts::now_ms();
    let updated = crate::modules::qoder_accounts::update(account.region, &account.id, |a| {
        a.credits = Some(quota.remaining);
        // 上游没给过期时间就不写 —— 不能编一个出来。
        a.plan = plan_name.clone().or_else(|| a.plan.clone());
        a.credits_updated_at = Some(now);
        a.last_used_at = Some(now);
    })?;

    // 顺手记一笔快照（10 分钟节流）。Qoder 没有官方用量端点，
    // 「今天用了多少」只能靠相邻两笔真实额度做差 —— 所以每次成功查询都要留点。
    crate::modules::qoder_credit_history::record(
        account.region,
        &account.uid,
        &updated.label(),
        quota.remaining,
        plan_name.as_deref(),
        now,
    );
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn headers_carry_the_three_derived_ids() {
        let h = openapi_headers(QoderRegion::Cn, "u-1", "tok");
        let get = |name: &str| {
            h.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("Authorization"), "Bearer tok");
        assert_eq!(get("X-Machine-ID").len(), 32);
        assert_eq!(get("X-Session-ID").len(), 32);
        assert_ne!(get("X-Machine-ID"), get("X-Session-ID"), "机器码与会话码必须不同");
        assert!(get("X-Request-ID").contains('-'));
        assert_eq!(get("Origin"), "https://qoder.com.cn");
    }

    #[test]
    fn quota_sums_user_and_addon() {
        let v = json!({
            "userQuota": {"remaining": 99.0, "used": 1.0, "total": 100.0},
            "addOnQuota": {"remaining": 5.0, "used": 0.0, "total": 5.0}
        });
        let q = parse_quota(&v).unwrap();
        assert_eq!(q.remaining, 104.0);
        assert_eq!(q.total, 105.0);
        assert!(!q.is_exceeded);
        assert!(q.ratio().unwrap() > 0.9);
    }

    #[test]
    fn float_quota_is_not_read_as_zero() {
        // 这正是「有余额却显示 0」的那个坑：as_i64 会读成 0，as_f64 才对。
        let v = json!({"userQuota": {"remaining": 99.0, "used": 1.0, "total": 100.0}});
        let q = parse_quota(&v).unwrap();
        assert_eq!(q.remaining, 99.0);
    }

    #[test]
    fn missing_quota_objects_is_an_error_not_zero() {
        let v = json!({"foo": 1});
        assert!(parse_quota(&v).is_err());
    }

    #[test]
    fn degenerate_response_is_refused() {
        let v = json!({"isQuotaExceeded": true, "userQuota": {"remaining": 0.0, "total": 0.0}});
        assert!(parse_quota(&v).is_err());
    }

    #[test]
    fn ratio_is_none_when_total_is_zero() {
        let v = json!({"userQuota": {"remaining": 0.0, "used": 0.0, "total": 0.0}});
        let q = parse_quota(&v).unwrap();
        assert_eq!(q.ratio(), None);
    }
}
