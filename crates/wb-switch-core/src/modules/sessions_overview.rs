//! 会话概要：Trae / WorkBuddy / Qoder 本地会话的数量、占用与活跃度。
//!
//! ## 为什么单独算，而不是前端把三个列表拉下来自己数
//!
//! 三个会话页各拉一次全量列表是**三份重活**：Trae 要解密 SQLCipher、Qoder 要开
//! `main.sqlite`、WorkBuddy 要读索引。首页概要想显示的只是几个数字，不该为此把
//! 三个库都翻一遍再丢掉。所以这里在一个命令里各取一次，只留下计数。
//!
//! ## 三条口径（写在代码里，避免以后各算各的）
//!
//! 1. **占用只累加「会话正文」字节**：WorkBuddy 用 `body_bytes`、Qoder 用
//!    `payload_bytes`，两者都是上游自己记的正文体积，可以相加。
//! 2. **Trae 的占用是整库大小**，含索引与非会话表，**不并入总量**，单独标注
//!    （Trae 的正文存在 SQLite 里，没有「单会话字节数」这个事实）。
//! 3. **活跃度用会话自身的更新时间**，不用文件 mtime：文件可能被备份工具碰到。
//!    `active7d` = 7 天内更新过；`stale30d` = 30 天没动过（清理页的候选口径）。

use serde_json::{json, Value};

use crate::modules::config::now_ms;
use crate::modules::qoder_realm::QoderRegion;

const DAY_MS: i64 = 86_400_000;

/// 一端的汇总。
#[derive(Default)]
struct Acc {
    count: u64,
    /// 会话正文字节（仅 WB / Qoder 有意义）。
    bytes: u64,
    /// **本工具的解密副本**字节（仅 Trae 有意义）。
    ///
    /// ⚠️ 不是客户端原库、也不是会话正文：Trae 的正文在 SQLCipher 里，
    /// 我们能加的是 `~/.twin-switch/trae/decrypted/*.db` 这几个副本的大小。
    /// 口径不同，所以**不并入** `contentBytes`，界面单独标注为「解密副本」。
    db_bytes: u64,
    active7d: u64,
    stale30d: u64,
    newest: i64,
    /// 读不到时的原因（有值就说明这一端的数字不完整）。
    note: Option<String>,
}

impl Acc {
    fn add(&mut self, updated_ms: i64, bytes: Option<u64>) {
        self.count += 1;
        if let Some(b) = bytes {
            self.bytes = self.bytes.saturating_add(b);
        }
        if updated_ms > 0 {
            self.newest = self.newest.max(updated_ms);
            let age = now_ms() - updated_ms;
            if age <= 7 * DAY_MS {
                self.active7d += 1;
            }
            if age > 30 * DAY_MS {
                self.stale30d += 1;
            }
        }
    }

    fn value(&self, platform: &str) -> Value {
        json!({
            "platform": platform,
            "count": self.count,
            // `bytes`：会话正文总量（WB / Qoder）
            "bytes": self.bytes,
            // `decryptedBytes`：本工具的解密副本大小（Trae）；0 = 这端没有这个概念
            "decryptedBytes": self.db_bytes,
            "active7d": self.active7d,
            "stale30d": self.stale30d,
            "newestAt": if self.newest > 0 { json!(self.newest) } else { Value::Null },
            "note": self.note.clone().map(Value::String).unwrap_or(Value::Null),
        })
    }
}

/// 三端会话概要。
pub fn overview() -> Value {
    let wb = workbuddy_overview();
    let qoder = qoder_overview();
    let trae = trae_overview();

    // 总量只加 WB + Qoder 的正文字节：Trae 是整库口径，混进来就成了「苹果加橘子」。
    let content_bytes = wb.bytes.saturating_add(qoder.bytes);
    let count = wb.count + qoder.count + trae.count;

    json!({
        "ok": true,
        "generatedAt": now_ms(),
        "total": {
            "count": count,
            "contentBytes": content_bytes,
            "active7d": wb.active7d + qoder.active7d + trae.active7d,
            "stale30d": wb.stale30d + qoder.stale30d + trae.stale30d,
        },
        "platforms": [
            wb.value("workbuddy"),
            trae.value("trae"),
            qoder.value("qoder"),
        ],
    })
}

fn workbuddy_overview() -> Acc {
    let mut acc = Acc::default();
    match crate::modules::workbuddy_source::list_sessions() {
        Ok(rows) => {
            for s in rows.iter().filter(|s| !s.deleted) {
                acc.add(s.updated_at, Some(s.body_bytes));
            }
        }
        Err(e) => acc.note = Some(format!("读取 WorkBuddy 会话失败：{e}")),
    }
    acc
}

fn qoder_overview() -> Acc {
    let mut acc = Acc::default();
    for region in [QoderRegion::Cn, QoderRegion::Global] {
        match crate::modules::qoder_sessions::list(region, false) {
            Ok(rows) => {
                for r in rows {
                    let updated = r.get("updatedAt").and_then(Value::as_i64).unwrap_or(0);
                    let bytes = r.get("bytes").and_then(Value::as_u64);
                    acc.add(updated, bytes);
                }
            }
            Err(e) => {
                // 两个区域各自可能有 / 没有客户端数据，缺一个不算整端失败。
                let msg = format!("{}：{e}", region_label(region));
                acc.note = Some(match acc.note.take() {
                    Some(prev) => format!("{prev}；{msg}"),
                    None => msg,
                });
            }
        }
    }
    acc
}

fn region_label(region: QoderRegion) -> &'static str {
    match region {
        QoderRegion::Cn => "国内版",
        QoderRegion::Global => "国际版",
    }
}

fn trae_overview() -> Acc {
    let mut acc = Acc::default();
    let clients = crate::modules::trae_discover::list_installed_clients();
    if clients.is_empty() {
        acc.note = Some("本机没有检测到 Trae 客户端".to_string());
        return acc;
    }
    let mut unreadable = 0usize;
    for c in clients {
        // 整库大小：即使没解密也能给（文件本身就在那里）。
        let db = crate::modules::trae_export::decrypted_db_path(&c.key);
        if let Ok(meta) = std::fs::metadata(&db) {
            acc.db_bytes = acc.db_bytes.saturating_add(meta.len());
        }
        match crate::modules::trae_export::list_sessions(&c.key) {
            Ok(rows) => {
                for s in rows {
                    let updated = parse_trae_time(&s.updated).or_else(|| parse_trae_time(&s.created));
                    // Trae 正文在 SQLite 里，没有单会话字节数 —— 传 None，别拿 0 充数。
                    acc.add(updated.unwrap_or(0), None);
                }
            }
            Err(_) => unreadable += 1,
        }
    }
    if unreadable > 0 {
        acc.note = Some(format!(
            "{unreadable} 个客户端的会话库还没解密，会话数只是已解密的那部分（解密副本大小照算）"
        ));
    }
    acc
}

/// Trae 的 `updated_at / created_at` 是 TEXT，实测有两种形态：
/// 毫秒时间戳字符串、`YYYY-MM-DD HH:MM:SS`。两种都认，认不出返回 `None`
/// （**别猜一个时间**：猜错会让「30 天没动」这类判据直接失真）。
fn parse_trae_time(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(n) = t.parse::<i64>() {
        // 10 位当秒、13 位当毫秒。
        return Some(if n > 10_000_000_000 { n } else { n * 1000 });
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S%.3f"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, fmt) {
            return Some(dt.and_local_timezone(chrono::Local).single()?.timestamp_millis());
        }
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Some(d.and_hms_opt(0, 0, 0)?.and_local_timezone(chrono::Local).single()?.timestamp_millis());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trae_time_accepts_seconds_millis_and_text() {
        assert_eq!(parse_trae_time("1794308612"), Some(1_794_308_612_000));
        assert_eq!(parse_trae_time("1794308612345"), Some(1_794_308_612_345));
        let text = parse_trae_time("2026-11-17 20:00:00").expect("文本时间要能解析");
        assert!(text > 1_700_000_000_000, "必须是毫秒量级：{text}");
        assert_eq!(parse_trae_time(""), None);
        assert_eq!(parse_trae_time("不是时间"), None, "认不出就是 None，不许猜");
    }

    /// 真机探针（`#[ignore]`）：把三端真实数字打出来，只读。
    #[test]
    #[ignore]
    fn sessions_probe_real() {
        let v = overview();
        println!("总量: {}", v["total"]);
        for p in v["platforms"].as_array().unwrap() {
            println!(
                "{:10} 会话 {:>4} | 正文 {:>10} | 整库 {:>10} | 7天 {:>3} | 30天未动 {:>3} | {}",
                p["platform"].as_str().unwrap(),
                p["count"],
                p["bytes"],
                p["decryptedBytes"],
                p["active7d"],
                p["stale30d"],
                p["note"].as_str().unwrap_or("")
            );
        }
    }

    #[test]
    fn acc_buckets_by_age() {
        let now = now_ms();
        let mut a = Acc::default();
        a.add(now - 2 * DAY_MS, Some(100)); // 7 天内
        a.add(now - 10 * DAY_MS, Some(200)); // 7~30 天
        a.add(now - 40 * DAY_MS, Some(300)); // 30 天以上
        assert_eq!(a.count, 3);
        assert_eq!(a.bytes, 600);
        assert_eq!(a.active7d, 1);
        assert_eq!(a.stale30d, 1);
    }

    #[test]
    fn zero_updated_at_does_not_count_as_stale() {
        // 时间未知（0）不能算进「30 天没动」——那会把「不知道」说成「很久没用」。
        let mut a = Acc::default();
        a.add(0, Some(10));
        assert_eq!(a.count, 1);
        assert_eq!(a.stale30d, 0);
        assert_eq!(a.active7d, 0);
    }
}
