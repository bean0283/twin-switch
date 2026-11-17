//! Qoder 积分快照与日消耗（本机实测值，**不编数**）。
//!
//! ## 为什么要有它
//!
//! Qoder / Trae / WorkBuddy 都没有官方的用量 / 历史端点，能拿到的只有**此刻的剩余额度**。
//! 想知道「今天用了多少」，唯一诚实的办法是自己隔一段时间记一笔，再用相邻两笔的差算。
//!
//! - 本模块：Qoder 老流水（`qoder/credit-snapshots.jsonl`，存量数据不动）；
//! - [`crate::modules::credit_snapshots`]：Trae / WorkBuddy 共享流水（`credit-snapshots.jsonl`）；
//! - [`summary`] 把两边**一起读一起算**，按 `platform` 可筛全部 / 单平台。
//!
//! ## 三条纪律
//!
//! 1. **只记真实查到的数**：查失败不写、额度未知（`None`）不写 —— 绝不写 0 充数；
//! 2. **10 分钟节流**：界面上「刷新积分」按一下就连发好几次，不节流会把文件写成流水账；
//! 3. **增加记为收入**：额度变多（充值 / 签到奖励 / 周期重置）不算消耗，按日记为
//!    「收入」——这样每日收支（收入 / 支出 / 净变化）三条线都来自真实快照差值，
//!    而不是把增加丢成一个 0。
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

/// 一个快照点。`platform` 为 `qoder`（老文件）/ `trae` / `workbuddy`（共享文件）。
#[derive(Debug, Clone)]
struct Point {
    at: i64,
    region: String,
    platform: String,
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
        platform: "qoder".to_string(),
        uid,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        credits,
    })
}

/// 全量快照点：Qoder 老文件（存量数据不动）+ Trae / WorkBuddy 共享文件。
fn read_all() -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(history_file()) {
        out.extend(text.lines().filter_map(parse_line));
    }
    for p in crate::modules::credit_snapshots::read_shared() {
        if p.platform.is_empty() {
            continue;
        }
        out.push(Point {
            at: p.at,
            region: String::new(),
            platform: p.platform,
            uid: p.uid,
            name: p.name,
            credits: p.credits,
        });
    }
    out
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

/// 本地完整日期（`YYYY-MM-DD`）：排序 / 自定义日期范围都靠它，
/// 跨年时 `MM-DD` 字符串序会把 1 月排在 12 月前面，完整日期不会。
fn date_key(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// 当地时区某天的 0 点毫秒值。
fn day_start_ms(d: chrono::NaiveDate) -> i64 {
    use chrono::TimeZone;
    let Some(naive) = d.and_hms_opt(0, 0, 0) else {
        return 0;
    };
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.timestamp_millis())
        .unwrap_or_else(|| naive.and_utc().timestamp_millis())
}

/// 当天最后一毫秒（含端点）。
fn day_end_ms(d: chrono::NaiveDate) -> i64 {
    day_start_ms(d) + 86_400_000 - 1
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 按日收支汇总（**三个平台一起算**：Qoder / Trae / WorkBuddy）。
///
/// 算法：同一账号按时间排序，**相邻两点**做差 ——
/// 额度减少记为该时刻所在日的「支出」，额度增加记为「收入」（充值 / 签到 / 重置）。
///
/// 窗口两种给法：
/// - `days`：近 N 天（1..=90）；
/// - `from`/`to`（`YYYY-MM-DD`，含端点）：自定义日期范围，跨度上限 366 天，
///   两个都合法才生效，否则退回 `days` 模式。
///
/// `platform`：`None` / `all` = 全部平台；`trae` / `workbuddy` / `qoder` = 只算该平台。
///
/// 返回结构与前端一一对应，缺数据时给空数组而不是 0 行占位。
pub fn summary(days: u32, from: Option<&str>, to: Option<&str>, platform: Option<&str>) -> Value {
    let now = crate::modules::qoder_accounts::now_ms();

    // 平台过滤：all / 空 = 不筛；其余按小写精确匹配（认不出来的当 all，别静默返回空）。
    let want = platform
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all"))
        .map(str::to_ascii_lowercase);

    let all: Vec<Point> = read_all()
        .into_iter()
        .filter(|p| {
            want.as_deref()
                .map(|w| p.platform.eq_ignore_ascii_case(w))
                .unwrap_or(true)
        })
        .collect();

    // 窗口解析：自定义范围优先，无效则退回近 N 天。
    let (since, until, window_days, range) =
        match (from.and_then(parse_date), to.and_then(parse_date)) {
            (Some(f), Some(t)) if f <= t && (t - f).num_days() < 366 => (
                day_start_ms(f),
                day_end_ms(t),
                (t - f).num_days() as u32 + 1,
                Some((f.to_string(), t.to_string())),
            ),
            _ => {
                let days = days.clamp(1, 90);
                (now - (days as i64) * 86_400_000, i64::MAX, days, None)
            }
        };

    // (platform, uid) → 按时间排序的点。uid 只在平台内唯一，跨平台可能撞。
    let mut by_account: std::collections::HashMap<(String, String), Vec<&Point>> =
        std::collections::HashMap::new();
    for p in &all {
        by_account
            .entry((p.platform.clone(), p.uid.clone()))
            .or_default()
            .push(p);
    }

    // date → (income, expense, 平台拆分)
    type PlatformAgg = std::collections::BTreeMap<String, (f64, f64)>;
    let mut per_day: std::collections::BTreeMap<String, (f64, f64, PlatformAgg)> =
        std::collections::BTreeMap::new();
    let mut per_account: std::collections::HashMap<(String, String), (String, f64, String)> =
        std::collections::HashMap::new();
    let mut platform_totals: PlatformAgg = std::collections::BTreeMap::new();
    let mut resets = 0usize;
    let mut used_total = 0.0f64;
    let mut income_total = 0.0f64;
    let mut points_in_window = 0usize;

    for ((platform, uid), mut pts) in by_account {
        pts.sort_by_key(|p| p.at);
        for w in pts.windows(2) {
            let (prev, cur) = (w[0], w[1]);
            if cur.at < since || cur.at > until {
                continue;
            }
            points_in_window += 1;
            let delta = prev.credits - cur.credits;
            let date = date_key(cur.at);
            let (add_income, add_expense) = if delta < 0.0 {
                // 额度增加：记收入（充值 / 签到奖励 / 周期重置）。
                resets += 1;
                (-delta, 0.0)
            } else if delta > 0.0 {
                // 额度减少：记支出。
                (0.0, delta)
            } else {
                continue;
            };
            let day = per_day.entry(date).or_default();
            day.0 += add_income;
            day.1 += add_expense;
            let part = day.2.entry(platform.clone()).or_default();
            part.0 += add_income;
            part.1 += add_expense;
            let pt = platform_totals.entry(platform.clone()).or_default();
            pt.0 += add_income;
            pt.1 += add_expense;
            if add_expense > 0.0 {
                let entry = per_account
                    .entry((platform.clone(), uid.clone()))
                    .or_insert_with(|| (cur.name.clone(), 0.0, cur.region.clone()));
                entry.1 += add_expense;
                if entry.0.is_empty() && !cur.name.is_empty() {
                    entry.0 = cur.name.clone();
                }
            }
            if add_income > 0.0 {
                income_total += add_income;
            }
            if add_expense > 0.0 {
                used_total += add_expense;
            }
        }
    }

    let mut accounts: Vec<Value> = per_account
        .into_iter()
        .map(|((platform, uid), (name, used, region))| {
            json!({
                "platform": platform,
                "uid": uid,
                "name": if name.is_empty() { format!("uid …{}", tail(&uid, 6)) } else { name },
                "region": region,
                "used": round2(used),
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
        .map(|(date, (income, expense, parts))| {
            let parts_json: Value = parts
                .into_iter()
                .map(|(platform, (pin, pout))| {
                    (
                        platform,
                        json!({ "income": round2(pin), "expense": round2(pout) }),
                    )
                })
                .collect::<serde_json::Map<String, Value>>()
                .into();
            json!({
                "date": date,
                "day": date.get(5..).unwrap_or_default().to_string(),
                "income": round2(income),
                "expense": round2(expense),
                "net": round2(income - expense),
                "parts": parts_json,
            })
        })
        .collect();

    let platform_totals_json: Value = platform_totals
        .into_iter()
        .map(|(platform, (pin, pout))| {
            (
                platform,
                json!({ "income": round2(pin), "expense": round2(pout) }),
            )
        })
        .collect::<serde_json::Map<String, Value>>()
        .into();

    let mut out = json!({
        "ok": true,
        "windowDays": window_days,
        "from": range.as_ref().map(|r| r.0.clone()),
        "to": range.as_ref().map(|r| r.1.clone()),
        "platform": want,
        "days": days_arr,
        "accounts": accounts,
        "platformTotals": platform_totals_json,
        "totalUsed": round2(used_total),
        "totalIncome": round2(income_total),
        "totalNet": round2(income_total - used_total),
        "resetEvents": resets,
        "points": points_in_window,
        "sampledAt": now,
        // 数据不足时把话说清楚：只有 1 个点算不出差，界面要显示「样本不足」而不是 0。
        "enoughData": points_in_window > 0,
        "file": history_file().display().to_string(),
    });
    if let Some((f, t)) = range {
        out["rangeMode"] = json!("custom");
        out["rangeFrom"] = json!(f);
        out["rangeTo"] = json!(t);
    } else {
        out["rangeMode"] = json!("days");
    }
    out
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
            platform: "qoder".into(),
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

    /// 日期键是完整 YYYY-MM-DD：跨年时字符串序与时间序一致（MM-DD 会把 1 月排在 12 月前）。
    #[test]
    fn date_key_sorts_across_years() {
        let jan = date_key(day_start_ms(parse_date("2027-01-05").unwrap()));
        let dec = date_key(day_start_ms(parse_date("2026-12-30").unwrap()));
        assert_eq!(jan, "2027-01-05");
        assert_eq!(dec, "2026-12-30");
        assert!(dec < jan, "12 月要排在次年 1 月前面");
    }

    /// 当天窗口：start 是 0 点、end 是当天最后一毫秒，含端点。
    #[test]
    fn day_window_covers_whole_day() {
        let d = parse_date("2026-11-16").unwrap();
        let s = day_start_ms(d);
        let e = day_end_ms(d);
        assert_eq!(date_key(s), "2026-11-16");
        assert_eq!(date_key(e), "2026-11-16");
        assert!(e - s < 86_400_000 && e > s);
    }
}
