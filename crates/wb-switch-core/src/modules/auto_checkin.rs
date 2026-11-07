//! **自动签到**：每天（按平台分开算）第一次时跑一次，并把当日结果记下来。
//!
//! ## 为什么要有「记录」而不是每次直接签
//!
//! 签到接口本身是幂等的（服务端会回「今日已签」），但每次启动都把所有平台扫一遍
//! 会白白发几十个请求、还要占着客户端进程的注意力。所以规则是：
//!
//! > **今天这个平台已经跑过 ⇒ 不再触发。**
//!
//! 「跑过」按 `scope`（trae / workbuddy / qoder）分别记：早上手动点了 Trae 的签到，
//! 下午自动签到就不该再把 Trae 重跑一遍，但 WorkBuddy 还没签、照样要签。
//!
//! ## 存哪
//!
//! `~/.twin-switch/auto-checkin.json`：
//!
//! ```json
//! {
//!   "enabled": true,
//!   "days": {
//!     "2026-11-02": {
//!       "trae": { "at": 1793..., "trigger": "auto", "summary": {...}, "items": [...] }
//!     }
//!   }
//! }
//! ```
//!
//! 只留最近 7 天（`days` 是日期键的 map，清理时按日期字符串排序丢老的）。
//!
//! ## 纪律
//!
//! - **默认关**。没开就不发任何请求 —— 「自动」不能变成「背着你联网」。
//! - 并发只跑一份：启动时的后台任务和首页挂载时都会问一次，
//!   用 `RUNNING` 闸门保证同一时刻只有一份在跑。
//! - 失败照样记录（`failed` 计数在 summary 里），否则「今天跑过但全失败」
//!   会被下次启动当成「还没跑」再撞一遍。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 参与自动签到的平台（与 `checkin::run` 的 `scope` 取值一致）。
pub const SCOPES: [&str; 3] = ["trae", "workbuddy", "qoder"];

/// 保留多少天的记录。
const KEEP_DAYS: usize = 7;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Store {
    /// 自动签到总开关。默认 `false`（见模块文档）。
    #[serde(default)]
    pub enabled: bool,
    /// 日期 → 平台 → 当日结果。
    #[serde(default)]
    pub days: BTreeMap<String, BTreeMap<String, Value>>,
}

fn path() -> PathBuf {
    crate::modules::config::store_dir().join("auto-checkin.json")
}

pub fn load() -> Store {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str::<Store>(&t).ok())
        .unwrap_or_default()
}

fn save(s: &Store) -> Result<(), String> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&p, text).map_err(|e| format!("写 {} 失败：{e}", p.display()))
}

/// 本地日期 `YYYY-MM-DD`（用本地时区：用户理解的「今天」是自己的今天）。
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 剪掉超过 `KEEP_DAYS` 的旧记录（按日期字符串排序，`YYYY-MM-DD` 天然可比）。
fn prune(s: &mut Store) {
    if s.days.len() <= KEEP_DAYS {
        return;
    }
    let mut keys: Vec<String> = s.days.keys().cloned().collect();
    keys.sort();
    let drop_n = keys.len() - KEEP_DAYS;
    for k in keys.into_iter().take(drop_n) {
        s.days.remove(&k);
    }
}

/// 今天各平台跑过没有。
pub fn done_today(s: &Store, scope: &str) -> bool {
    s.days
        .get(&today())
        .map(|m| m.contains_key(scope))
        .unwrap_or(false)
}

/// 今天**还需要跑**的平台（纯函数，便于单测）。
///
/// 这是「已签到则不触发」的**唯一判据**：`maybe_run` 只认它，
/// 别在调用处再写一遍 `if` —— 两处判据迟早会分叉。
pub fn scopes_due(s: &Store) -> Vec<&'static str> {
    SCOPES.iter().copied().filter(|sc| !done_today(s, sc)).collect()
}

/// 给界面看的完整状态。
pub fn state() -> Value {
    let s = load();
    let today = today();
    let today_map = s.days.get(&today).cloned().unwrap_or_default();

    let scopes: Vec<Value> = SCOPES
        .iter()
        .map(|sc| {
            let rec = today_map.get(*sc);
            json!({
                "scope": sc,
                "done": rec.is_some(),
                // 没记录 ⇒ 明确是 `null`，别让界面把「今天还没跑」画成「跑了但啥也没签」
                "record": rec,
            })
        })
        .collect();

    json!({
        "enabled": s.enabled,
        "today": today,
        "scopes": scopes,
        // 最近几天（倒序），给界面画个小历史
        "history": s.days.iter().rev().take(KEEP_DAYS).map(|(d, m)| json!({
            "date": d,
            "scopes": m.keys().cloned().collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// 开/关自动签到。
pub fn set_enabled(on: bool) -> Result<Value, String> {
    let mut s = load();
    s.enabled = on;
    save(&s)?;
    Ok(state())
}

/// 并发闸门：启动任务与首页挂载会同时问，保证同一秒只有一份在跑。
static RUNNING: OnceLock<Mutex<bool>> = OnceLock::new();

/// 今天该跑的跑一遍；已经跑过的平台跳过。
///
/// 返回 `{ran: [...], skipped: [...], state: {...}}`：
/// - `ran`：这次真的执行了的平台
/// - `skipped`：今天已经跑过、这次没碰的
///
/// 未开启时两者都为空、不发任何请求。
pub async fn maybe_run(trigger: &str) -> Value {
    let s = load();
    if !s.enabled {
        return json!({ "ran": [], "skipped": SCOPES, "reason": "disabled", "state": state() });
    }

    // 先占闸门再干活；拿不到说明另一份正在跑，直接返回。
    let guard = RUNNING.get_or_init(|| Mutex::new(false));
    {
        let mut g = match guard.lock() {
            Ok(g) => g,
            Err(_) => return json!({ "ran": [], "skipped": [], "reason": "busy", "state": state() }),
        };
        if *g {
            return json!({ "ran": [], "skipped": [], "reason": "busy", "state": state() });
        }
        *g = true;
    }
    // 无论如何都要放闸（下面可能提前 return）。
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(g) = RUNNING.get() {
                if let Ok(mut g) = g.lock() {
                    *g = false;
                }
            }
        }
    }
    let _release = Release;

    let mut ran: Vec<&str> = Vec::new();
    let mut skipped: Vec<&str> = Vec::new();

    // 一开始就把「今天还差哪些」算出来，跑的过程中只做记录、不再重新判断：
    // 判据只有 `scopes_due` 一份，避免「循环里又 reload 一次」这种两套逻辑。
    let due = scopes_due(&s);
    for scope in SCOPES {
        if !due.contains(&scope) {
            skipped.push(scope);
            continue;
        }
        let mut cur = load();
        let res = crate::modules::checkin::run(Some(scope), None).await;
        let summary = res.get("summary").cloned().unwrap_or(Value::Null);
        let items = res.get("items").cloned().unwrap_or_else(|| json!([]));

        let day = today();
        cur.days
            .entry(day)
            .or_default()
            .insert(scope.to_string(), json!({
                "at": crate::modules::qoder_accounts::now_ms(),
                "trigger": trigger,
                "summary": summary,
                "items": items,
            }));
        prune(&mut cur);
        // 写失败不该让整轮白跑：记录只是「别重复签」，签已经签过了。
        let _ = save(&cur);
        ran.push(scope);
    }

    json!({ "ran": ran, "skipped": skipped, "reason": "", "state": state() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_by_default() {
        // 默认必须是关的：开启自动签到 = 允许它在后台联网，不能替用户决定。
        assert!(!Store::default().enabled);
    }

    #[test]
    fn done_today_is_per_scope_not_per_day() {
        let mut s = Store::default();
        let d = today();
        s.days.entry(d).or_default().insert("trae".into(), json!({"at": 1}));

        assert!(done_today(&s, "trae"), "跑过 trae");
        // 关键：WorkBuddy 没跑过 —— 早上手动签了 Trae，不该连累 WorkBuddy 也不签。
        assert!(!done_today(&s, "workbuddy"));
        assert!(!done_today(&s, "qoder"));
    }

    /// 核心要求：今天跑过的平台不再进「待跑」清单，没跑过的照常进。
    #[test]
    fn scopes_due_excludes_finished_platforms() {
        let mut s = Store::default();
        assert_eq!(scopes_due(&s), vec!["trae", "workbuddy", "qoder"], "全新状态三个都要跑");

        s.days.entry(today()).or_default().insert("trae".into(), json!({"at": 1}));
        assert_eq!(scopes_due(&s), vec!["workbuddy", "qoder"], "签过 Trae 就不再触发它");

        for sc in ["workbuddy", "qoder"] {
            s.days.entry(today()).or_default().insert(sc.into(), json!({"at": 1}));
        }
        assert!(scopes_due(&s).is_empty(), "全签过 ⇒ 一个都不跑");
    }

    #[test]
    fn yesterday_does_not_count_as_today() {
        let mut s = Store::default();
        s.days
            .entry("2000-01-01".into())
            .or_default()
            .insert("trae".into(), json!({"at": 1}));
        assert!(!done_today(&s, "trae"), "昨天的记录不能当今天已签");
    }

    #[test]
    fn prune_keeps_newest_days() {
        let mut s = Store::default();
        for d in 1..=12 {
            s.days
                .entry(format!("2026-01-{d:02}"))
                .or_default()
                .insert("trae".into(), json!({"at": d}));
        }
        prune(&mut s);
        assert_eq!(s.days.len(), KEEP_DAYS);
        assert!(s.days.contains_key("2026-01-12"), "最新的必须在");
        assert!(!s.days.contains_key("2026-01-01"), "最老的必须被剪掉");
    }

    #[test]
    fn serde_roundtrip_keeps_nested_records() {
        let mut s = Store::default();
        s.enabled = true;
        s.days
            .entry("2026-11-02".into())
            .or_default()
            .insert("qoder".into(), json!({"at": 1793000000000i64, "trigger": "auto"}));
        let t = serde_json::to_string(&s).unwrap();
        let back: Store = serde_json::from_str(&t).unwrap();
        assert!(back.enabled);
        assert_eq!(back.days["2026-11-02"]["qoder"]["trigger"], "auto");
    }

    /// 老文件没有 `days` / `enabled` 键时不能炸 —— 也不能默认成「开」。
    #[test]
    fn missing_keys_fall_back_safely() {
        let s: Store = serde_json::from_str("{}").unwrap();
        assert!(!s.enabled);
        assert!(s.days.is_empty());
    }
}
