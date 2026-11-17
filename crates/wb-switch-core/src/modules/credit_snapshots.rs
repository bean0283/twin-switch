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
//! 2. **只做 5 秒防抖**（同一秒内重复写没意义），**不做长节流**：
//!    这是**事实流水**，不是缓存 —— 用户主动查了一次就该留一个点。
//!    早先这里是 10 分钟节流，结果「消耗积分后手动刷新，趋势纹丝不动」，
//!    用户以为功能坏了（2026-11-17 反馈）。

use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::modules::config;

/// 同一账号两次快照的最小间隔（只防抖，不是节流）。
pub const THROTTLE_MS: i64 = 5 * 1000;

/// 保留天数：超过就裁掉，避免流水无限增长。
pub const KEEP_DAYS: i64 = 90;

/// 行数超过这个值才触发裁剪（避免每次写都读全文件）。
const PRUNE_AT_LINES: usize = 5000;

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
    let ok = f.write_all(format!("{line}\n").as_bytes()).is_ok();
    drop(f);
    if ok {
        prune_if_needed();
    }
    ok
}

/// 超过 [`PRUNE_AT_LINES`] 行时，把 [`KEEP_DAYS`] 之前的点裁掉。
///
/// 为什么按行数触发而不是每次写都裁：裁剪要读全文件 + 重写，
/// 只有在文件真的大了才值得做；趋势图最多看 90 天，更老的点没人用。
fn prune_if_needed() {
    let path = history_file();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < PRUNE_AT_LINES {
        return;
    }
    let cutoff = chrono::Local::now().timestamp_millis() - KEEP_DAYS * 86_400_000;
    let kept: Vec<&str> = lines
        .into_iter()
        .filter(|l| {
            // 解析不了的行（写一半 / 手工编辑）一律保留：删掉等于丢证据。
            serde_json::from_str::<Value>(l)
                .ok()
                .and_then(|v| v.get("at").and_then(Value::as_i64))
                .map(|at| at >= cutoff)
                .unwrap_or(true)
        })
        .collect();
    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
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
