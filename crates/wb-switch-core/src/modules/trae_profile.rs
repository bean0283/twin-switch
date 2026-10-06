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
//! （实测本机账号同时挂着「免费」「每月登录赠送」「每日签到 ×N」）。
//! 只算一个总数会丢掉「哪部分快到期了」这个真正有用的信息，所以这里解析成
//! [`parse_entitlement`] 的统一形状，和 WorkBuddy 侧的账号卡保持同一种读法。

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
///   "pack_count": 6,
///   "packs": [{ "key","name","group","total","used","remaining",
///               "expire_at","status","unlimited" }]
/// }
/// ```
///
/// - 汇总优先取 `usage_summary`（官方算好的口径）；它缺失时退化成逐个包累加。
/// - `total`/`used` 为 `null` 表示「这一项接口没给」（例如不限量的“免费”包没有额度）。
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
            return json!({ "ok": false, "error": friendly, "packs": [], "pack_count": 0 });
        }
    }

    let Some(list) = j.get("user_entitlement_pack_list").and_then(Value::as_array) else {
        return json!({ "ok": false, "error": "接口未返回积分包列表", "packs": [], "pack_count": 0 });
    };

    let mut packs: Vec<Value> = Vec::new();
    let mut sum_limit = 0.0f64;
    let mut sum_used = 0.0f64;
    let mut sum_remaining = 0.0f64;

    for p in list {
        let limit = pack_limit(p);
        let used = p.pointer("/usage/credits_amount").and_then(Value::as_f64).unwrap_or(0.0);
        let remaining = limit.map(|l| (l - used).max(0.0));
        if let Some(l) = limit {
            sum_limit += l;
        }
        sum_used += used;
        if let Some(r) = remaining {
            sum_remaining += r;
        }

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
            "total": limit.map(round2),
            "used": round2(used),
            "remaining": remaining.map(round2),
            // 额度缺失 = 这个包不限量（接口确实这么给，不是解析失败）
            "unlimited": limit.is_none(),
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

    #[test]
    fn pack_without_limit_is_unlimited() {
        let raw = json!({ "user_entitlement_pack_list": [pack("免费", None, 0.0, 0)] });
        let v = parse_entitlement(&raw);
        let p = &v["packs"][0];
        assert_eq!(p["unlimited"], json!(true));
        assert_eq!(p["total"], Value::Null);
        assert_eq!(p["remaining"], Value::Null);
        // expire_time=0 → 长期有效
        assert_eq!(p["expire_at"], Value::Null);
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
