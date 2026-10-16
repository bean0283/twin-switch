//! 账号资料查询（接口契约对齐青龙签到脚本 checkin_ql.js）：
//!   · `GetUserInfo`（x-cloudide-token 头）拉真实昵称 / UserID / 脱敏手机号 / 头像；
//!   · `user_current_entitlement_list`（authorization: Cloud-IDE-JWT）拉积分总额、
//!     已用与**逐个积分包的明细**（包名 / 额度 / 已用 / 到期时间）。
//!
//! 结果缓存到 `<vault>/<client>/<id>/profile.json`，账号库离线也能展示最近一次
//! 拉取到的昵称与积分；网络失败时静默保留旧缓存，不打断账号库主流程。
//!
//! ## 为什么要把包明细也存下来
//!
//! 接口返回的 `user_entitlement_pack_list` 里每个包都有独立额度与到期时间
//! （实测本机账号同时挂着「每月登录赠送」与「每日签到 ×N」）。
//! 只算一个总数会丢掉「哪部分快到期了」这个真正有用的信息，所以这里解析成
//! [`parse_entitlement`] 的统一形状，和 WorkBuddy 侧的账号卡保持同一种读法。
//!
//! ## ⚠️ 这份列表里混着两类东西（T65 五改踩过的坑）
//!
//! 除了真正的积分包，列表**第一项**永远是「免费方案的**功能权益**」：
//! `entitlement_id = free_utc2026xx_<uid>`、`display_desc = "免费"`，
//! 它的 `quota` 里只有 `enable_solo_agent` / `enable_solo_builder` /
//! `no_bonus_quota` / `solo_agent_parallel_limit` 这些**功能开关**，
//! **没有 `credits_limit`** —— 它没有积分，只表示「这个套餐能开哪些功能」。
//!
//! 曾经这里把「没有额度字段」当成「不限量包」（`unlimited: true`），
//! 于是每张账号卡都凭空多出一行「不限量 免费」，而 Trae 根本没有不限量积分。
//! 现在唯一判据是：**带 `credits_limit` 的才是积分包**，其余一律不算。

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::modules::config::http_request;
use crate::modules::trae_vault::account_dir;

const USERINFO_PATH: &str = "/cloudide/api/v3/trae/GetUserInfo";
const ENTITLE_PATH: &str = "/trae/api/v2/pay/user_current_entitlement_list";
/// 实测可用域名：签到脚本走 api.trae.cn；oauth 落库的 host 可能是 api.trae.com.cn，
/// 请求失败时按候选列表逐个回退。
const HOSTS: &[&str] = &["https://api.trae.cn", "https://api.trae.com.cn"];

pub fn profile_path(client_key: &str, id: &str) -> PathBuf {
    account_dir(client_key, id).join("profile.json")
}

fn clean_token(token: &str) -> String {
    token.trim().strip_prefix("Cloud-IDE-JWT ").map(String::from).unwrap_or_else(|| token.trim().to_string())
}

/// 生成形如官方客户端的纯数字设备 ID（oauth.json 无 deviceId 时兜底）。
fn gen_device_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{now}{}", now % 997)
}

/// 候选域名（去重）：oauth 落库 host 优先，接口实测域名兜底。
fn candidate_hosts(oauth: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |h: String| {
        let h = h.trim().trim_end_matches('/').to_string();
        if !h.is_empty() && !out.contains(&h) {
            out.push(h);
        }
    };
    if let Some(h) = oauth.get("host").and_then(|v| v.as_str()) {
        push(h.to_string());
    }
    for h in HOSTS {
        push((*h).to_string());
    }
    out
}

fn value_str(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| {
        x.as_str()
            .map(String::from)
            .or_else(|| x.as_i64().map(|i| i.to_string()))
    })
}

/// 两位小数（接口给的是高精度浮点，界面上只到分）。
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 调 GetUserInfo 拿真实昵称 / UserID / 脱敏手机号 / 头像。失败返回 None。
async fn fetch_user_info(host: &str, token: &str) -> Option<Value> {
    let mut headers = HashMap::new();
    headers.insert("accept".into(), "*/*".into());
    headers.insert("x-cloudide-token".into(), token.to_string());
    let j = http_request(
        &format!("{host}{USERINFO_PATH}"),
        "POST",
        Some(json!({ "ReqSource": "Lite", "IDEVersion": "0.1.63" })),
        Some(&headers),
    )
    .await;
    let r = j.get("Result")?;
    let user_id = value_str(r.get("UserID"))?;
    if user_id.is_empty() {
        return None;
    }
    Some(json!({
        "user_id": user_id,
        "screen_name": r.get("ScreenName").and_then(|v| v.as_str()).map(str::trim).unwrap_or_default(),
        "mobile": r.get("NonPlainTextMobile").and_then(|v| v.as_str()).unwrap_or_default(),
        "avatar": r.get("AvatarUrl").and_then(|v| v.as_str()).unwrap_or_default(),
        "region": r.get("AIRegion").and_then(|v| v.as_str()).unwrap_or_default(),
    }))
}

/// 调 user_current_entitlement_list 拿原始返回。
async fn fetch_entitlement(host: &str, token: &str, device_id: &str) -> Option<Value> {
    let mut headers = HashMap::new();
    headers.insert("authorization".into(), format!("Cloud-IDE-JWT {token}"));
    headers.insert("x-device-id".into(), device_id.to_string());
    headers.insert("x-device-type".into(), "windows".into());
    headers.insert("x-user-region".into(), "CN".into());
    headers.insert("x-market-client-id".into(), "VSCode 1.107.1".into());
    headers.insert("x-app-version".into(), "0.1.63".into());
    headers.insert("app-version".into(), "0.1.63".into());
    headers.insert("package-type".into(), "stable_cn".into());
    headers.insert("accept-language".into(), "zh-CN".into());
    let j = http_request(
        &format!("{host}{ENTITLE_PATH}"),
        "POST",
        Some(json!({ "req_source": 2 })),
        Some(&headers),
    )
    .await;
    j.is_object().then_some(j)
}

/// 从一个积分包条目里取额度（两个位置都见过：直接 quota，或 package_extra.quota）。
fn pack_limit(p: &Value) -> Option<f64> {
    p.pointer("/entitlement_base_info/quota/credits_limit")
        .and_then(Value::as_f64)
        .or_else(|| {
            p.pointer("/entitlement_base_info/product_extra/package_extra/quota/credits_limit")
                .and_then(Value::as_f64)
        })
}

/// 包名优先级：运营给的 `package_name` → `display_desc` → 分组名 → 兜底。
fn pack_name(p: &Value) -> String {
    for path in [
        "/entitlement_base_info/product_extra/package_extra/package_name",
        "/display_desc",
        "/group_name",
    ] {
        if let Some(s) = p.pointer(path).and_then(Value::as_str).map(str::trim) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    "积分包".to_string()
}

/// 把 `user_current_entitlement_list` 的原始返回解析成统一形状（纯函数，便于单测）。
///
/// 输出：
/// ```json
/// {
///   "ok": true, "error": null,
///   "total": 4800.0, "used": 4221.24, "remaining": 578.76,
///   "pack_count": 6, "non_credit_count": 1,
///   "packs": [{ "key","name","group","total","used","remaining",
///               "expire_at","status" }]
/// }
/// ```
///
/// - **积分包判据**：条目带 `credits_limit` 才算积分包。列表里的
///   `free_utc2026xx_<uid>`（`display_desc = "免费"`）是「免费方案能开哪些功能」的
///   权益条目、**没有额度**，会被计入 `non_credit_count` 并从 `packs` 里剔除 ——
///   它不是「不限量包」（Trae 没有不限量积分）。
/// - 汇总优先取 `usage_summary`（官方算好的口径）；它缺失时退化成逐个包累加。
/// - 包按到期时间升序，长期有效的排最后。
pub fn parse_entitlement(j: &Value) -> Value {
    // 认证失败（实测 code=1001，token 过期就是这条）
    if let Some(code) = j.get("code").and_then(Value::as_i64) {
        if code != 0 {
            let msg = j.get("message").and_then(Value::as_str).unwrap_or("");
            let friendly = if code == 1001 || msg.to_lowercase().contains("authenticate") {
                "凭据已失效，请重新登录该账号后再查积分".to_string()
            } else {
                format!("接口返回 code={code}：{}", msg.trim())
            };
            return json!({
                "ok": false,
                "error": friendly,
                "packs": [],
                "pack_count": 0,
                "non_credit_count": 0
            });
        }
    }

    let Some(list) = j.get("user_entitlement_pack_list").and_then(Value::as_array) else {
        return json!({
            "ok": false,
            "error": "接口未返回积分包列表",
            "packs": [],
            "pack_count": 0,
            "non_credit_count": 0
        });
    };

    let mut packs: Vec<Value> = Vec::new();
    let mut sum_limit = 0.0f64;
    let mut sum_used = 0.0f64;
    let mut sum_remaining = 0.0f64;
    // 列表里混进来的「功能权益」条目数（不是积分包，见模块头注释）。
    let mut non_credit = 0usize;

    for p in list {
        // ⚠️⚠️ 唯一的积分包判据：**带 `credits_limit`**。
        //
        // 曾经这里是 `let remaining = limit.map(|l| (l - used).max(0.0));`，
        // 再加上 `"unlimited": limit.is_none()` —— 等于宣布「没有额度字段 = 不限量」。
        // 实测三个账号的列表里各有一条 `free_utc202610_<uid>`（`display_desc = "免费"`），
        // 它的 `quota` 只有 `enable_solo_agent` / `no_bonus_quota` 这些开关、
        // **一个积分都没有** ⇒ 被冤枉成「不限量包」⇒ 每张账号卡多出一行「不限量 免费」。
        // Trae 没有不限量积分：没有 `credits_limit` 就说明它根本不是积分包。
        let Some(limit) = pack_limit(p) else {
            non_credit += 1;
            continue;
        };
        let used = p.pointer("/usage/credits_amount").and_then(Value::as_f64).unwrap_or(0.0);
        let remaining = (limit - used).max(0.0);
        sum_limit += limit;
        sum_used += used;
        sum_remaining += remaining;

        // 到期时间：expire_time（秒）；0 / 缺失视为长期有效。
        let expire_sec = p
            .get("expire_time")
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .or_else(|| {
                p.pointer("/entitlement_base_info/end_time")
                    .and_then(Value::as_i64)
                    .filter(|v| *v > 0)
            });

        packs.push(json!({
            "key": p
                .pointer("/entitlement_base_info/entitlement_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("pack-{}", packs.len())),
            "name": pack_name(p),
            "group": p.get("group_name").and_then(Value::as_str).unwrap_or("").to_string(),
            "total": round2(limit),
            "used": round2(used),
            "remaining": round2(remaining),
            "expire_at": expire_sec.map(|s| s * 1000),
            "status": p.get("status").and_then(Value::as_i64).unwrap_or(0),
        }));
    }

    // 到期升序，长期有效（null）排最后
    packs.sort_by_key(|p| p.get("expire_at").and_then(Value::as_i64).unwrap_or(i64::MAX));

    let us = j.get("usage_summary");
    let total = us
        .and_then(|u| u.get("total_amount"))
        .and_then(Value::as_f64)
        .unwrap_or(sum_limit);

    // 「空」与「失败」绝不许长得一样：列表非空、一个积分包都没认出，**但服务端又报出
    // 有总额度** —— 这不是「这个账号没积分」，是接口结构变了，必须报错而不是画成 0。
    if packs.is_empty() && !list.is_empty() && total > 0.0 {
        return json!({
            "ok": false,
            "error": format!(
                "接口返回 {} 项权益，但没有一项带 credits_limit（总额度却写着 {}）—— 接口结构可能变了",
                list.len(),
                round2(total)
            ),
            "pack_count": 0,
            "non_credit_count": non_credit,
            "packs": [],
        });
    }

    let used = us
        .and_then(|u| u.get("consumed_amount"))
        .and_then(Value::as_f64)
        .unwrap_or(sum_used);
    let remaining = if us.and_then(|u| u.get("total_amount")).and_then(Value::as_f64).is_some() {
        (total - used).max(0.0)
    } else {
        sum_remaining
    };

    json!({
        "ok": true,
        "error": Value::Null,
        "total": round2(total),
        "used": round2(used),
        "remaining": round2(remaining),
        "pack_count": packs.len(),
        "non_credit_count": non_credit,
        "packs": packs,
    })
}

/// 刷新账号资料（昵称 + 积分包明细）并缓存到 profile.json。
/// 网络全部失败时返回 None 且不动旧缓存。oauth 为 `read_oauth_account` 的返回。
pub async fn refresh_profile(client_key: &str, id: &str, oauth: &Value) -> Option<Value> {
    let raw_token = value_str(oauth.get("token"))?;
    let token = clean_token(&raw_token);
    if token.is_empty() {
        return None;
    }
    let device_id = value_str(oauth.get("deviceId")).unwrap_or_else(gen_device_id);
    for host in candidate_hosts(oauth) {
        let info = fetch_user_info(&host, &token).await;
        let ent = fetch_entitlement(&host, &token, &device_id).await;
        if info.is_none() && ent.is_none() {
            continue;
        }
        let credit = ent.as_ref().map(parse_entitlement);
        let profile = json!({
            "screen_name": info.as_ref().and_then(|i| i.get("screen_name")).cloned().unwrap_or(Value::Null),
            "user_id": info.as_ref().and_then(|i| i.get("user_id")).cloned().unwrap_or(Value::Null),
            "mobile": info.as_ref().and_then(|i| i.get("mobile")).cloned().unwrap_or(Value::Null),
            "avatar": info.as_ref().and_then(|i| i.get("avatar")).cloned().unwrap_or(Value::Null),
            "region": info.as_ref().and_then(|i| i.get("region")).cloned().unwrap_or(Value::Null),
            // 兼容旧字段：`credits` 仍是「剩余积分」，只是从整数变成两位小数。
            "credits": credit.as_ref().and_then(|c| c.get("remaining")).cloned().unwrap_or(Value::Null),
            "credits_total": credit.as_ref().and_then(|c| c.get("total")).cloned().unwrap_or(Value::Null),
            "credits_used": credit.as_ref().and_then(|c| c.get("used")).cloned().unwrap_or(Value::Null),
            "credit_packs": credit.as_ref().and_then(|c| c.get("packs")).cloned().unwrap_or(json!([])),
            "credit_ok": credit.as_ref().and_then(|c| c.get("ok")).cloned().unwrap_or(json!(false)),
            "credit_error": credit.as_ref().and_then(|c| c.get("error")).cloned().unwrap_or(Value::Null),
            "host": host,
            "fetched_at": chrono::Local::now().to_rfc3339(),
        });
        let p = profile_path(client_key, id);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&p, serde_json::to_string_pretty(&profile).unwrap_or_default());
        return Some(profile);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(name: &str, limit: Option<f64>, used: f64, expire: i64) -> Value {
        let mut quota = json!({});
        if let Some(l) = limit {
            quota = json!({ "credits_limit": l });
        }
        json!({
            "display_desc": name,
            "group_name": format!("组-{name}"),
            "status": 1,
            "expire_time": expire,
            "usage": if used > 0.0 { json!({ "credits_amount": used }) } else { json!({}) },
            "entitlement_base_info": {
                "entitlement_id": format!("ent-{name}"),
                "quota": quota,
                "product_extra": { "package_extra": { "package_name": name, "quota": quota } },
            },
        })
    }

    #[test]
    fn parses_summary_and_packs() {
        let raw = json!({
            "usage_summary": { "total_amount": 4800.0, "consumed_amount": 4221.24 },
            "user_entitlement_pack_list": [
                pack("每月登录赠送", Some(500.0), 1.5604, 1793462399),
                pack("签到奖励", Some(100.0), 0.0, 1793721119),
                pack("签到奖励", Some(100.0), 0.0, 1793843423),
            ],
        });
        let v = parse_entitlement(&raw);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["pack_count"], json!(3));
        assert_eq!(v["total"], json!(4800.0));
        assert_eq!(v["used"], json!(4221.24));
        assert_eq!(v["remaining"], json!(578.76));
        // 到期升序
        let packs = v["packs"].as_array().unwrap();
        let exps: Vec<i64> = packs.iter().filter_map(|p| p["expire_at"].as_i64()).collect();
        assert!(exps.windows(2).all(|w| w[0] <= w[1]), "包应按到期升序");
        // 单项：额度 / 已用 / 剩余
        assert_eq!(packs[0]["total"], json!(500.0));
        assert_eq!(packs[0]["remaining"], json!(498.44));
        assert_eq!(packs[0]["name"], json!("每月登录赠送"));
        // expire_time 是秒，转成毫秒
        assert_eq!(packs[0]["expire_at"], json!(1793462399000i64));
    }

    #[test]
    fn missing_summary_falls_back_to_pack_sum() {
        let raw = json!({
            "user_entitlement_pack_list": [
                pack("A", Some(100.0), 30.0, 100),
                pack("B", Some(50.0), 0.0, 200),
            ],
        });
        let v = parse_entitlement(&raw);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["total"], json!(150.0));
        assert_eq!(v["used"], json!(30.0));
        assert_eq!(v["remaining"], json!(120.0));
    }

    /// 实测**逐字抄**下来的「免费方案功能权益」条目（`free_utc202610_4052137095223728`）。
    ///
    /// 注意它的 `quota`（以及 `product_extra.subscription_extra.quota`）里
    /// **只有功能开关、没有 `credits_limit`** —— 这就是「不限量 免费」那行假数据的来源。
    fn free_entitlement() -> Value {
        json!({
            "display_desc": "免费",
            "entitlement_base_info": {
                "available_endpoint": 0,
                "charge_amount": 0,
                "currency": 0,
                "end_time": 1793462399,
                "ent_status": 0,
                "entitlement_id": "free_utc202610_4052137095223728",
                "product_extra": {
                    "subscription_extra": {
                        "period_type": 0,
                        "quota": {
                            "enable_solo_agent": true,
                            "enable_solo_builder": true,
                            "enable_solo_coder": true,
                            "enable_solo_lite": true,
                            "enable_solo_web": true,
                            "no_bonus_quota": true,
                            "solo_agent_parallel_limit": 2
                        }
                    }
                },
                "product_id": 0,
                "product_type": 0,
                "quota": {
                    "enable_solo_agent": true,
                    "enable_solo_builder": true,
                    "enable_solo_coder": true,
                    "enable_solo_lite": true,
                    "enable_solo_web": true,
                    "no_bonus_quota": true,
                    "solo_agent_parallel_limit": 2
                },
                "start_time": 1790784000,
                "user_id": "4052137095223728"
            },
            "expire_time": 1793462399,
            "is_hide": false,
            "is_last_period": false,
            "is_oneweek": false,
            "next_billing_time": 0,
            "source_id": "",
            "status": 1,
            "usage": {},
            "yearly_expire_time": 0
        })
    }

    /// 「免费」权益**不是积分包**：没有 `credits_limit` ⇒ 不进 `packs`，
    /// 只记进 `non_credit_count`。用户在账号卡上看不到这一行（T65 五改）。
    #[test]
    fn the_free_plan_entitlement_is_not_a_credit_pack() {
        let v = parse_entitlement(&json!({ "user_entitlement_pack_list": [free_entitlement()] }));
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["pack_count"], json!(0));
        assert_eq!(v["non_credit_count"], json!(1));
        assert!(v["packs"].as_array().unwrap().is_empty(), "免费权益不该出现在积分包里");
        assert_eq!(v["total"], json!(0.0));
        assert_eq!(v["remaining"], json!(0.0));
    }

    /// 混在一起时：真包照常列出，「免费」权益被剔除，且**结果里再没有 `unlimited` 字段**。
    #[test]
    fn free_entitlement_is_skipped_while_real_packs_survive() {
        let raw = json!({
            "usage_summary": { "total_amount": 1700.0, "consumed_amount": 5.74 },
            "user_entitlement_pack_list": [
                free_entitlement(),
                pack("每月登录赠送", Some(500.0), 5.74, 1793462399),
                pack("签到奖励", Some(100.0), 0.0, 1793721119),
            ],
        });
        let v = parse_entitlement(&raw);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["pack_count"], json!(2));
        assert_eq!(v["non_credit_count"], json!(1));
        assert_eq!(v["total"], json!(1700.0));
        assert_eq!(v["remaining"], json!(1694.26));

        let packs = v["packs"].as_array().unwrap();
        assert_eq!(packs[0]["name"], json!("每月登录赠送"));
        assert_eq!(packs[0]["remaining"], json!(494.26));
        // ⚠️ 这条是本事故的钉子：「不限量」说的是**一件不存在的事**，
        // 任何积分包都必须带得出额度。
        assert!(
            packs.iter().all(|p| p.get("unlimited").is_none()),
            "解析结果里不该再出现 unlimited 字段"
        );
        assert!(
            packs.iter().all(|p| !p["total"].is_null() && !p["remaining"].is_null()),
            "真积分包必须带出 total / remaining"
        );
    }

    /// 列表里一个积分包都没认出来、**但服务端说有额度** ⇒ 那是接口结构变了，
    /// 必须报错，不能画成「0 积分」（「空」与「失败」不许长得一样）。
    #[test]
    fn total_amount_without_any_credit_pack_is_a_shape_error_not_a_zero_balance() {
        let v = parse_entitlement(&json!({
            "usage_summary": { "total_amount": 1700.0, "consumed_amount": 5.74 },
            "user_entitlement_pack_list": [free_entitlement()],
        }));
        assert_eq!(v["ok"], json!(false));
        assert!(
            v["error"].as_str().unwrap().contains("credits_limit"),
            "实际：{}",
            v["error"]
        );

        // 边界：服务端也没说有额度（没有 usage_summary）⇒ 就是「这个账号没有积分包」，正常空结果。
        let v2 = parse_entitlement(&json!({ "user_entitlement_pack_list": [free_entitlement()] }));
        assert_eq!(v2["ok"], json!(true));
        assert_eq!(v2["pack_count"], json!(0));
    }

    #[test]
    fn expired_credential_surfaces_friendly_error() {
        let raw = json!({
            "code": 1001,
            "message": "We're sorry, but we are not able to authenticate you.",
            "usage_summary": {},
            "user_entitlement_pack_list": [],
        });
        let v = parse_entitlement(&raw);
        assert_eq!(v["ok"], json!(false));
        assert!(v["error"].as_str().unwrap().contains("凭据已失效"));
        assert_eq!(v["pack_count"], json!(0));
    }

    #[test]
    fn missing_pack_list_is_an_error_not_a_panic() {
        let v = parse_entitlement(&json!({ "usage_summary": {} }));
        assert_eq!(v["ok"], json!(false));
        assert!(v["packs"].as_array().unwrap().is_empty());
    }

    #[test]
    fn pack_name_prefers_product_extra_then_display_desc() {
        // product_extra.package_name 缺失时回落到 display_desc
        let mut p = pack("签到奖励", Some(100.0), 0.0, 10);
        p["entitlement_base_info"]["product_extra"]["package_extra"]
            .as_object_mut()
            .unwrap()
            .remove("package_name");
        let v = parse_entitlement(&json!({ "user_entitlement_pack_list": [p] }));
        assert_eq!(v["packs"][0]["name"], json!("签到奖励"));

        // 两者都没有则回落到 group_name
        let mut p2 = pack("X", Some(1.0), 0.0, 10);
        p2["entitlement_base_info"]["product_extra"]["package_extra"]
            .as_object_mut()
            .unwrap()
            .remove("package_name");
        p2.as_object_mut().unwrap().remove("display_desc");
        let v2 = parse_entitlement(&json!({ "user_entitlement_pack_list": [p2] }));
        assert_eq!(v2["packs"][0]["name"], json!("组-X"));
    }
}

