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
    /// 到期时间（毫秒，上游顶层 `expiresAt`）。
    ///
    /// ⚠️ 它**在顶层，不在每个块里** —— 2026-10-25 之前只解析了两块，于是界面一直显示
    /// 「到期时间未提供」，其实是漏读了这个字段（真机实测：`1793713084271` = 2026-11-03 21:38）。
    /// 语义是**当前套餐周期结束时间**，两块额度共用同一个到期。
    pub expire_at: Option<i64>,
    /// **按块**的明细：Qoder 上游把额度分成 `userQuota` 与 `addOnQuota` 两块，
    /// 界面要像 Trae / WorkBuddy 那样逐包显示，所以这里就分好再往下传。
    /// 哪块上游没给就少一项（不补 0 —— 0 和「没有这块」是两件事）。
    pub parts: Vec<crate::modules::qoder_accounts::CreditPart>,
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
    // 只用来判「退化形态」（exceeded 且全 0）；展示用的合计在下面按块重算。
    let remaining = num(user, "remaining") + num(add_on, "remaining");
    let total = num(user, "total") + num(add_on, "total");

    // 分块明细：上游给哪块就出哪块。`remaining` 缺失时用 total-used 兜底
    // （只在 total>0 时兜底，否则会把「不知道」写成 0）。
    let expire_at = payload
        .get("expiresAt")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0);

    // 第三块：组织资源包（企业/团队账号才有，个人账号没有这个键）。
    // 官方 SDK 文档写的是「用 cap 表示总量，另有 used / remaining / available」，
    // 我们按 total←cap 归一，缺了就用 total。
    let org = payload.get("orgResourcePackage").and_then(Value::as_object);
    let mut parts: Vec<crate::modules::qoder_accounts::CreditPart> = Vec::new();
    let org_part = org.map(|src| {
        let t = src
            .get("cap")
            .and_then(Value::as_f64)
            .or_else(|| src.get("total").and_then(Value::as_f64))
            .unwrap_or(0.0);
        let u = src.get("used").and_then(Value::as_f64).unwrap_or(0.0);
        let r = src
            .get("remaining")
            .and_then(Value::as_f64)
            .or_else(|| src.get("available").and_then(Value::as_f64))
            .unwrap_or_else(|| (t - u).max(0.0));
        crate::modules::qoder_accounts::CreditPart {
            name: "组织资源包".to_string(),
            total: t,
            used: u,
            remaining: r,
        }
    });

    for (name, src) in [("用户额度", user), ("加购额度", add_on)] {
        let Some(src) = src else { continue };
        let t = num(Some(src), "total");
        let u = num(Some(src), "used");
        let r = if src.get("remaining").and_then(Value::as_f64).is_some() {
            num(Some(src), "remaining")
        } else if t > 0.0 {
            (t - u).max(0.0)
        } else {
            u
        };
        parts.push(crate::modules::qoder_accounts::CreditPart {
            name: name.to_string(),
            total: t,
            used: u,
            remaining: r,
        });
    }
    let is_exceeded = payload
        .get("isQuotaExceeded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_exceeded && remaining == 0.0 && total == 0.0 {
        return Err("额度响应为退化形态（exceeded 且全 0）—— 拒绝写入".into());
    }
    if let Some(p) = org_part {
        parts.push(p);
    }
    // 合计**按块求和**（而不是再单独加一遍 user/addon）：这样组织资源包也算进来，
    // 企业账号的「剩余」才不会少算一块。退化判据在上面已用原始两块判过，这里只覆盖展示值。
    let remaining: f64 = parts.iter().map(|p| p.remaining).sum();
    let used: f64 = parts.iter().map(|p| p.used).sum();
    let total: f64 = parts.iter().map(|p| p.total).sum();
    Ok(QuotaView { remaining, used, total, is_exceeded, expire_at, parts, raw: payload.clone() })
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
        Err(e) => {
            // 诊断日志：Qoder 的「刷新额度失败 / 401」终结在这里。
            crate::modules::diag_log::error(
                "qoder_quota",
                format!(
                    "刷新额度失败 region={} id={} uid={} | {e}",
                    account.region.as_str(),
                    account.id,
                    account.uid
                ),
            );
            return Err(e);
        }
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
        a.credit_parts = quota.parts.clone();
        // 上游这次没给就保留上次的值（别把已知的到期时间擦成空）。
        a.credits_expire_at = quota.expire_at.or(a.credits_expire_at);
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

    /// 分块：上游两块都要成「积分包」，缺一块就只出一块（不补 0）。
    #[test]
    fn quota_parts_split_user_and_addon() {
        let v = json!({
            "userQuota": {"remaining": 99.0, "used": 1.0, "total": 100.0},
            "addOnQuota": {"remaining": 5.0, "used": 0.0, "total": 5.0}
        });
        let q = parse_quota(&v).unwrap();
        assert_eq!(q.parts.len(), 2);
        assert_eq!(q.parts[0].name, "用户额度");
        assert_eq!(q.parts[0].remaining, 99.0);
        assert_eq!(q.parts[1].name, "加购额度");
        assert_eq!(q.parts[1].total, 5.0);

        // 只有一块 ⇒ 只出一块（「没有这块」不等于「这块是 0」）
        let only = json!({"userQuota": {"remaining": 99.0, "used": 1.0, "total": 100.0}});
        let q2 = parse_quota(&only).unwrap();
        assert_eq!(q2.parts.len(), 1);
        assert_eq!(q2.parts[0].name, "用户额度");
    }

    /// `remaining` 缺失时按 total-used 兜底；total 也是 0 时不许把「不知道」写成 0 混淆。
    #[test]
    fn quota_part_remaining_falls_back() {
        let v = json!({"userQuota": {"used": 30.0, "total": 100.0}});
        let q = parse_quota(&v).unwrap();
        assert_eq!(q.parts[0].remaining, 70.0);
    }

    /// 到期时间在**顶层** `expiresAt`（不在块里）—— 漏读它会一直显示「到期时间未提供」。
    #[test]
    fn quota_reads_top_level_expires_at() {
        let v = json!({
            "userQuota": {"remaining": 104.0, "used": 196.0, "total": 300.0},
            "addOnQuota": {"remaining": 600.0, "used": 0.0, "total": 600.0},
            "expiresAt": 1793713084271i64
        });
        let q = parse_quota(&v).unwrap();
        assert_eq!(q.expire_at, Some(1793713084271));
        assert_eq!(q.parts.len(), 2);

        // 没有该字段就是 None（不编一个到期时间）
        let q2 = parse_quota(&json!({"userQuota": {"remaining": 1.0, "total": 1.0}})).unwrap();
        assert_eq!(q2.expire_at, None);
    }

    /// 企业账号第三块 `orgResourcePackage`（用 cap 表示总量）也要算进明细与合计。
    #[test]
    fn quota_reads_org_resource_package() {
        let v = json!({
            "userQuota": {"remaining": 100.0, "used": 0.0, "total": 100.0},
            "orgResourcePackage": {"cap": 1000.0, "used": 250.0, "available": 750.0}
        });
        let q = parse_quota(&v).unwrap();
        assert_eq!(q.parts.len(), 2);
        let org = &q.parts[1];
        assert_eq!(org.name, "组织资源包");
        assert_eq!(org.total, 1000.0);
        assert_eq!(org.remaining, 750.0, "优先用 available");
        assert_eq!(q.remaining, 850.0, "合计要包含组织资源包");
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
