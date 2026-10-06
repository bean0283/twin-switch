//! Trae 账号积分（额度 + 逐个积分包）：账号库全量查询与离线读取。
//!
//! 与 [`crate::modules::workbuddy_credits`] 对称：WorkBuddy 那边是「查接口 → 落盘 →
//! 界面按账号卡展示」，这里把 Trae 的 `user_current_entitlement_list` 走同一条路。
//!
//! ## 数据来源
//!
//! - **联网**：每个账号自己的 `oauth.json` 里的 token → [`trae_profile::refresh_profile`]，
//!   结果写进该账号的 `profile.json`（含 `credit_packs` 明细）；
//! - **离线**：直接读各账号的 `profile.json`（[`cached`]），毫秒级、不联网。
//!
//! ## 风控
//!
//! Trae 侧的既有约定是「登录成功自动拉一次，之后手动刷新」（见 `trae_oauth` 的注释）。
//! 所以 [`query`] 默认吃 5 分钟内的缓存，只有 `force = true`（用户点「刷新积分」）
//! 才真的逐个账号打接口 —— 账号数不多，但没必要每次进页面都打一遍。

use serde_json::{json, Value};

use crate::modules::{config, trae_profile, trae_vault};

/// 磁盘缓存文件名（落在 `config::cache_dir()`）。
const CACHE_NAME: &str = "trae-credits.json";
/// 缓存结构版本，字段一改就 +1。
const CACHE_VERSION: u64 = 1;
/// 联网结果的保鲜期：5 分钟。窗口内重复进页面不再打接口。
const CACHE_TTL_MS: i64 = 5 * 60 * 1000;

/// 一个待处理的账号：`(clientKey, 账号 id, 是否有网页凭证)`。
///
/// 没有 `oauth.json` 的账号（切换载体来的）拿不到 token，只能读上次的 `profile.json`，
/// 界面要能明确区分这两种状态。
fn collect_targets(client_key: Option<&str>) -> Vec<(String, String, bool)> {
    let root = trae_vault::vault_root();
    let mut out: Vec<(String, String, bool)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return out;
    };
    for c in rd.flatten() {
        if !c.path().is_dir() {
            continue;
        }
        let client = c.file_name().to_string_lossy().into_owned();
        if let Some(want) = client_key.filter(|w| !w.is_empty()) {
            if want != client {
                continue;
            }
        }
        let Ok(rd2) = std::fs::read_dir(c.path()) else {
            continue;
        };
        for a in rd2.flatten() {
            if !a.path().is_dir() {
                continue;
            }
            let id = a.file_name().to_string_lossy().into_owned();
            let has_oauth = a.path().join("oauth.json").is_file();
            let has_profile = a.path().join("profile.json").is_file();
            // 什么都没有的空目录不算账号
            if has_oauth || has_profile {
                out.push((client.clone(), id, has_oauth));
            }
        }
    }
    out.sort();
    out
}

/// RFC3339 → 毫秒时间戳（解析不了返回 `None`）。
fn rfc3339_ms(s: Option<&str>) -> Option<i64> {
    let s = s?;
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

/// 把「账号库条目 + 它的 profile.json」拼成前端要的一条。
fn entry(client: &str, id: &str, has_oauth: bool, profile: Option<&Value>) -> Value {
    let oauth: Option<Value> = std::fs::read_to_string(trae_vault::account_dir(client, id).join("oauth.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());

    let oauth_ref = oauth.as_ref();
    let get_s = |k: &str| -> Option<String> {
        profile
            .and_then(|p| p.get(k))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };

    let uid = get_s("user_id")
        .or_else(|| oauth_ref.and_then(|o| o.get("uid")).and_then(Value::as_str).map(String::from))
        .unwrap_or_default();
    let name = get_s("screen_name")
        .or_else(|| {
            oauth_ref
                .and_then(|o| o.get("displayName"))
                .and_then(Value::as_str)
                .map(String::from)
        })
        .unwrap_or_else(|| id.to_string());
    let avatar = get_s("avatar").or_else(|| {
        oauth_ref
            .and_then(|o| o.get("avatar"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    });

    let packs = profile
        .and_then(|p| p.get("credit_packs"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let pack_count = packs.as_array().map(Vec::len).unwrap_or(0);
    let ok = profile
        .and_then(|p| p.get("credit_ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let error = profile
        .and_then(|p| p.get("credit_error"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);

    json!({
        "id": id,
        "clientKey": client,
        "uid": uid,
        "name": name,
        "avatar": avatar,
        "mobile": get_s("mobile"),
        "host": get_s("host"),
        // 有网页凭证才谈得上「联网查积分」
        "queryable": has_oauth,
        "ok": ok,
        "error": if has_oauth {
            error
        } else {
            Some("该账号是切换载体，没有网页凭证，无法查询积分".to_string())
        },
        "total": profile.and_then(|p| p.get("credits_total")).cloned().unwrap_or(Value::Null),
        "used": profile.and_then(|p| p.get("credits_used")).cloned().unwrap_or(Value::Null),
        "remaining": profile.and_then(|p| p.get("credits")).cloned().unwrap_or(Value::Null),
        "packCount": pack_count,
        "packs": packs,
        "updatedAt": rfc3339_ms(profile.and_then(|p| p.get("fetched_at")).and_then(Value::as_str)),
    })
}

/// 由当前账号库（各账号的 `profile.json`）现拼一份结果。**不联网、毫秒级**。
fn from_profiles(client_key: Option<&str>) -> Value {
    let accounts: Vec<Value> = collect_targets(client_key)
        .into_iter()
        .map(|(c, id, has_oauth)| {
            let profile = trae_vault::read_profile(&c, &id);
            entry(&c, &id, has_oauth, profile.as_ref())
        })
        .collect();
    let summary = summarize(&accounts);
    json!({
        "ok": true,
        "clientKey": client_key.unwrap_or(""),
        "accounts": accounts,
        "summary": summary,
        "updatedAt": Value::Null,
        "cached": true,
    })
}

/// 汇总：查了几个、成了几个、合计剩余多少。
fn summarize(accounts: &[Value]) -> Value {
    let queried = accounts.len();
    let succeeded = accounts.iter().filter(|a| a["ok"] == json!(true)).count();
    let total_remaining: f64 = accounts
        .iter()
        .filter(|a| a["ok"] == json!(true))
        .filter_map(|a| a["remaining"].as_f64())
        .sum();
    json!({
        "queried": queried,
        "succeeded": succeeded,
        "failed": queried - succeeded,
        "totalRemaining": (total_remaining * 100.0).round() / 100.0,
    })
}

/// 只读离线结果：把账号库里已有的 `profile.json` 拼出来，**不联网**。
///
/// 用于首屏 —— 界面一开始就有数字，随后 [`query`] 再覆盖。
pub fn cached(client_key: Option<String>) -> Value {
    from_profiles(client_key.as_deref())
}

/// 查询积分。
///
/// - `force = false`：5 分钟内的磁盘缓存直接返回（`cached: true`）；
/// - `force = true`：逐个账号真打接口，结果写进各自的 `profile.json` 并落缓存。
///
/// 单个账号失败不影响其它账号（`ok = false` + `error` 原因）。
pub async fn query(client_key: Option<String>, force: bool) -> Value {
    if !force {
        if let Some((mut payload, _age)) = config::read_cache_slot(CACHE_NAME, CACHE_VERSION, CACHE_TTL_MS)
        {
            let same_key =
                payload.get("clientKey").and_then(Value::as_str).unwrap_or("")
                    == client_key.as_deref().unwrap_or("");
            if same_key {
                if let Some(o) = payload.as_object_mut() {
                    o.insert("cached".into(), json!(true));
                }
                return payload;
            }
        }
    }

    for (client, id, has_oauth) in collect_targets(client_key.as_deref()) {
        // 没有网页凭证的账号（切换载体来的）压根没有 token，跳过即可 ——
        // 它会带着「无法查询积分」的说明出现在结果里。
        if !has_oauth {
            continue;
        }
        let path = trae_vault::account_dir(&client, &id).join("oauth.json");
        let Some(oauth) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        // 单个账号失败不影响其它账号：refresh_profile 失败时保留旧 profile.json。
        let _ = trae_profile::refresh_profile(&client, &id, &oauth).await;
    }

    // 汇总与条目一律从落盘后的 profile.json 现拼 —— 以文件为准，
    // 避免「刷新结果」与「界面看到的」出现两套口径。
    let mut out = from_profiles(client_key.as_deref());
    let accounts = out.get("accounts").cloned().unwrap_or_else(|| json!([]));
    let summary = summarize(accounts.as_array().map(Vec::as_slice).unwrap_or_default());
    let updated_at = config::now_ms();
    if let Some(o) = out.as_object_mut() {
        o.insert("cached".into(), json!(false));
        o.insert("updatedAt".into(), json!(updated_at));
        o.insert("summary".into(), summary);
    }
    config::write_cache_slot(CACHE_NAME, CACHE_VERSION, &out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_only_successful_accounts() {
        let accounts = vec![
            json!({ "ok": true, "remaining": 100.5 }),
            json!({ "ok": true, "remaining": 0.25 }),
            json!({ "ok": false, "remaining": 999.0 }),
        ];
        let s = summarize(&accounts);
        assert_eq!(s["queried"], json!(3));
        assert_eq!(s["succeeded"], json!(2));
        assert_eq!(s["failed"], json!(1));
        // 失败账号的 remaining 不该被算进去
        assert_eq!(s["totalRemaining"], json!(100.75));
    }

    #[test]
    fn cached_is_always_shaped() {
        // 本机可能压根没有账号库 —— 形状必须一致，前端才不会踩空。
        let v = cached(None);
        assert_eq!(v["ok"], json!(true));
        assert!(v["accounts"].is_array());
        assert_eq!(v["cached"], json!(true));
        assert!(v["summary"]["queried"].is_number());
    }

    #[test]
    fn rfc3339_parsing_is_tolerant() {
        assert!(rfc3339_ms(Some("2026-10-05T09:33:11.964261+08:00")).unwrap() > 1_700_000_000_000);
        assert_eq!(rfc3339_ms(Some("not-a-date")), None);
        assert_eq!(rfc3339_ms(None), None);
    }

    #[test]
    fn entry_without_oauth_is_marked_unqueryable() {
        let e = entry("solo-cn", "carrier_1", false, None);
        assert_eq!(e["queryable"], json!(false));
        assert_eq!(e["ok"], json!(false));
        assert!(e["error"].as_str().unwrap().contains("没有网页凭证"));
        assert_eq!(e["name"], json!("carrier_1")); // 没资料时回落到 id
        assert_eq!(e["packs"], json!([]));
    }
}
