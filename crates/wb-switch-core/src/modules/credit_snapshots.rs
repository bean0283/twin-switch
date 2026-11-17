//! 跨平台积分快照流水（Trae / WorkBuddy）。
//!
//! ## 为什么要有它
//!
//! Trae / WorkBuddy 都没有官方的用量 / 历史端点，能拿到的只有**此刻的剩余积分**。
//! 想知道「今天收了多少 / 用了多少」，唯一诚实的办法是自己隔一段时间记一笔，
//! 再用相邻两笔的差算 —— 与 [`crate::modules::qoder_credit_history`]（Qoder）同口径。
//!
//! ## 落盘
//!
//! `~/.twin-switch/credit-snapshots.jsonl`，一行一个点，纯追加，带 `platform` 字段。
//! Qoder 的历史数据在它自己的 `qoder/credit-snapshots.jsonl`（老文件，不动），汇总时两边一起读。
//!
//! ## 纪律（与 Qoder 一致）
//!
//! 1. **只记真实查到的数**：查失败 / 额度未知不写 —— 绝不写 0 充数；
//! 2. **10 分钟节流**：同平台同账号距上一点不足 10 分钟就跳过。

use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::modules::config;

/// 同一账号两次快照的最小间隔。
pub const THROTTLE_MS: i64 = 10 * 60 * 1000;

pub fn history_file() -> PathBuf {
    config::store_dir().join("credit-snapshots.jsonl")
}

/// 记一笔（带节流）。返回是否真的写了。
///
/// `credits` 必须是**刚查到**的剩余积分；查失败 / 未知时调用方不要调它。
pub fn record(platform: &str, uid: &str, name: &str, credits: f64, at: i64) -> bool {
    if platform.trim().is_empty() || uid.trim().is_empty() || !credits.is_finite() {
        return false;
    }
    // 节流：同一平台同一账号，距上一点不足 10 分钟就跳过。
    if let Some(last) = read_shared()
        .iter()
        .filter(|p| p.platform == platform && p.uid == uid)
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
        "platform": platform,
        "at": at,
        "uid": uid,
        "name": name,
        "credits": (credits * 100.0).round() / 100.0,
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

/// 共享流水里的一个点（供汇总模块读取）。
#[derive(Debug, Clone)]
pub struct SharedPoint {
    pub at: i64,
    pub platform: String,
    pub uid: String,
    pub name: String,
    pub credits: f64,
}

/// 解析共享流水的一行。坏行返回 `None`（客户端写到一半被杀不能毁掉整份历史）。
pub fn parse_shared_line(line: &str) -> Option<SharedPoint> {
    let v: Value = serde_json::from_str(line).ok()?;
    let credits = v.get("credits").and_then(Value::as_f64)?;
    let at = v.get("at").and_then(Value::as_i64)?;
    let uid = v.get("uid").and_then(Value::as_str)?.trim().to_string();
    let platform = v
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if uid.is_empty() || platform.is_empty() || !credits.is_finite() {
        return None;
    }
    Some(SharedPoint {
        at,
        platform,
        uid,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        credits,
    })
}

pub fn read_shared() -> Vec<SharedPoint> {
    let Ok(text) = std::fs::read_to_string(history_file()) else {
        return Vec::new();
    };
    text.lines().filter_map(parse_shared_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_shared_lines_are_skipped() {
        assert!(parse_shared_line("不是 JSON").is_none());
        assert!(parse_shared_line(r#"{"at":1,"uid":"u","credits":1.5}"#).is_none(), "缺 platform 不算一个点");
        assert!(parse_shared_line(r#"{"platform":"trae","at":1,"uid":"u","credits":1.5}"#).is_some());
    }

    #[test]
    fn record_rejects_garbage() {
        let now = 1_799_999_000_000i64;
        assert!(!record("", "u", "n", 1.0, now), "空 platform 不许写");
        assert!(!record("trae", " ", "n", 1.0, now), "空 uid 不许写");
        assert!(!record("trae", "u", "n", f64::NAN, now), "非有限值不许写");
    }
}
