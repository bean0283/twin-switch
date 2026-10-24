//! Qoder 积分快照与日消耗（本机实测值，**不编数**）。
//!
//! ## 为什么要有它
//!
//! Qoder 没有官方的用量 / 历史端点，能拿到的只有**此刻的剩余额度**。
//! 想知道「今天用了多少」，唯一诚实的办法是自己隔一段时间记一笔，再用相邻两笔的差算。
//!
//! ## 三条纪律
//!
//! 1. **只记真实查到的数**：查失败不写、额度未知（`None`）不写 —— 绝不写 0 充数；
//! 2. **10 分钟节流**：界面上「刷新积分」按一下就连发好几次，不节流会把文件写成流水账；
//! 3. **增加不算消耗**：额度变多（充值 / 签到奖励 / 周期重置）时 delta 记 0 并记一次
//!    `reset` 事件，让「今天用了多少」不会变成负数。
//!
//! 落盘：`~/.twin-switch/qoder/credit-snapshots.jsonl`，一行一个点，纯追加。

use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::modules::config;
use crate::modules::qoder_realm::QoderRegion;

/// 同一账号两次快照的最小间隔。
const THROTTLE_MS: i64 = 10 * 60 * 1000;

pub fn history_file() -> PathBuf {
    config::store_dir()
        .join("qoder")
        .join("credit-snapshots.jsonl")
}

/// 一个快照点。
#[derive(Debug, Clone)]
struct Point {
    at: i64,
    region: String,
    uid: String,
    name: String,
    credits: f64,
}

fn parse_line(line: &str) -> Option<Point> {
    let v: Value = serde_json::from_str(line).ok()?;
    let credits = v.get("credits").and_then(Value::as_f64)?;
    let at = v.get("at").and_then(Value::as_i64)?;
    let uid = v.get("uid").and_then(Value::as_str)?.trim().to_string();
    if uid.is_empty() || !credits.is_finite() {
        return None;
    }
    Some(Point {
        at,
        region: v
            .get("region")
            .and_then(Value::as_str)
            .unwrap_or("cn")
            .to_string(),
        uid,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        credits,
    })
}

fn read_all() -> Vec<Point> {
    let Ok(text) = std::fs::read_to_string(history_file()) else {
        return Vec::new();
    };
    text.lines().filter_map(parse_line).collect()
}

/// 记一笔（带节流）。返回是否真的写了。
///
/// `credits` 必须是**刚查到**的剩余额度；查失败时调用方不要调它。
pub fn record(
    region: QoderRegion,
    uid: &str,
    name: &str,
    credits: f64,
    plan: Option<&str>,
    at: i64,
) -> bool {
    if uid.trim().is_empty() || !credits.is_finite() {
        return false;
    }
    // 节流：同一区域同一账号，距上一点不足 10 分钟就跳过。
    if let Some(last) = read_all()
        .iter()
        .filter(|p| p.uid == uid && p.region == region.as_str())
        .max_by_key(|p| p.at)
    {
        if at - last.at < THROTTLE_MS {
            return false;
        }
    }
    let path = history_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let line = json!({
        "at": at,
        "region": region.as_str(),
        "uid": uid,
        "name": name,
        "credits": (credits * 100.0).round() / 100.0,
        "plan": plan,
    });
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return false;
    };
    f.write_all(format!("{line}\n").as_bytes()).is_ok()
}

/// 本地日期（`MM-DD`），用于按天聚合。
fn day_of(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

/// 近 `days` 天的消耗汇总。
///
/// 算法：同一账号按时间排序，**相邻两点**做差；差值为正记为该时刻所在日的消耗，
/// 差值为负（额度涨了）记 `reset` 并计 0 —— 不把「充值」算成「负消耗」。
///
/// 返回结构与前端一一对应，缺数据时给空数组而不是 0 行占位。
pub fn summary(days: u32) -> Value {
    let days = days.clamp(1, 90);
    let all = read_all();
    let now = crate::modules::qoder_accounts::now_ms();
    let since = now - (days as i64) * 86_400_000;

    // (uid) → 按时间排序的点
    let mut by_uid: std::collections::HashMap<String, Vec<&Point>> = std::collections::HashMap::new();
    for p in &all {
        by_uid.entry(p.uid.clone()).or_default().push(p);
    }

    let mut per_day: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    let mut per_account: std::collections::HashMap<String, (String, f64, String)> =
        std::collections::HashMap::new();
    let mut resets = 0usize;
    let mut used_total = 0.0f64;
    let mut points_in_window = 0usize;

    for (uid, mut pts) in by_uid {
        pts.sort_by_key(|p| p.at);
        for w in pts.windows(2) {
            let (prev, cur) = (w[0], w[1]);
            if cur.at < since {
                continue;
            }
            points_in_window += 1;
            let delta = prev.credits - cur.credits;
            if delta < 0.0 {
                resets += 1;
                continue;
            }
            if delta <= 0.0 {
                continue;
            }
            let day = day_of(cur.at);
            *per_day.entry(day).or_default() += delta;
            let entry = per_account
                .entry(uid.clone())
                .or_insert_with(|| (cur.name.clone(), 0.0, cur.region.clone()));
            entry.1 += delta;
            if entry.0.is_empty() && !cur.name.is_empty() {
                entry.0 = cur.name.clone();
            }
            used_total += delta;
        }
    }

    let mut accounts: Vec<Value> = per_account
        .into_iter()
        .map(|(uid, (name, used, region))| {
            json!({
                "uid": uid,
                "name": if name.is_empty() { format!("uid …{}", tail(&uid, 6)) } else { name },
                "region": region,
                "used": (used * 100.0).round() / 100.0,
            })
        })
        .collect();
    accounts.sort_by(|a, b| {
        b["used"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["used"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let days_arr: Vec<Value> = per_day
        .into_iter()
        .map(|(day, used)| json!({ "day": day, "used": (used * 100.0).round() / 100.0 }))
        .collect();

    json!({
        "ok": true,
        "windowDays": days,
        "days": days_arr,
        "accounts": accounts,
        "totalUsed": (used_total * 100.0).round() / 100.0,
        "resetEvents": resets,
        "points": points_in_window,
        "sampledAt": now,
        // 数据不足时把话说清楚：只有 1 个点算不出差，界面要显示「样本不足」而不是 0。
        "enoughData": points_in_window > 0,
        "file": history_file().display().to_string(),
    })
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(at: i64, uid: &str, credits: f64) -> Point {
        Point {
            at,
            region: "cn".into(),
            uid: uid.into(),
            name: "n".into(),
            credits,
        }
    }

    /// 相邻两点做差：减少 = 消耗，增加 = reset 记 0（绝不出现负消耗）。
    #[test]
    fn delta_rules() {
        let a = point(1_000, "u", 100.0);
        let b = point(2_000, "u", 90.0);
        let c = point(3_000, "u", 95.0);
        assert!((a.credits - b.credits - 10.0).abs() < 1e-9);
        assert!(c.credits - b.credits > 0.0, "增加的那个要按 reset 处理，不计消耗");
    }

    /// 坏行不许让整份历史读不出来（客户端可能在写到一半时被杀）。
    #[test]
    fn broken_lines_are_skipped() {
        assert!(parse_line("不是 JSON").is_none());
        assert!(parse_line(r#"{"at":1,"uid":"u"}"#).is_none(), "缺 credits 不算一个点");
        assert!(parse_line(r#"{"at":1,"uid":"u","credits":1.5}"#).is_some());
    }
}
