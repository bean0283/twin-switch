//! 缓存命中（上下文 / 前缀缓存）统计：网关账本 + WorkBuddy 客户端。
//!
//! ## 为什么要单独一个模块
//!
//! 「用量」和「命中」是两个问题：用量是加法（input + output），命中是**一个除法**，
//! 而除法最容易错的地方全在分母与「有没有观测到」上。把它们塞进用量统计里，
//! 迟早会出现「页面恒显示 50%」这种静默错（参考项目 issue #154 就是这么来的）。
//! 所以这里只做一件事：**把三处口径固定下来，并且只输出原始数**，命中率由前端按
//! 同一公式算（`cached / input`，分母 0 或无观测 ⇒ `null`）。
//!
//! ## 三端数据可得性（2026-11-13 本机实测，不是猜的）
//!
//! | 端 | 数据源 | 能不能算命中率 |
//! |---|---|---|
//! | 网关 | `~/.twin-switch/gateway-metrics.jsonl` 的 `cachedTokens` | 能（上游给了才算） |
//! | WorkBuddy | `~/.workbuddy/projects/**/*.jsonl` 的 `message.usage` | 能（实测 `cache_read_input_tokens` 有真值） |
//! | Trae | 客户端会话库 | **不能**：只有 input/output，没有缓存字段 |
//! | Qoder | `~/.qoder*` 客户端日志 | **不能**：`input_tokens/output_tokens/cache_*` 全是 0，只有 `credits` 有值 |
//!
//! 后两端**必须如实返回「不可用 + 原因」**，绝不能返回 0 —— 「算不出」与「命中 0%」
//! 是同一条硬规则里的两个不同事实（见项目根记忆 §0）。
//!
//! ## 口径（三条，改任何一条都会让数字变形）
//!
//! 1. **`cached` 是 `input` 的子集，绝不并入合计**：`input` 保持上游 `prompt_tokens`
//!    原值。命中率 = `cached / input`。把它加进 total 会让命中率翻倍。
//! 2. **分母只取输入侧**：拿 input+output 当分母，输出越长命中率越低，与缓存无关。
//! 3. **没观测 ≠ 命中 0**：`cachedTokens` 缺键 / `null` ⇒ 这次请求不进 `observed`。
//!    `observed == 0` 时前端显示「—」而不是 `0.0%`。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::TimeZone;
use serde_json::{json, Value};

/// 一条用量读数（三处来源归一后的最小集）。
#[derive(Default, Clone, Copy)]
struct Reading {
    input: u64,
    output: u64,
    /// `None` = 上游没给这项统计。
    cached: Option<u64>,
}

/// 分桶汇总。
#[derive(Default)]
struct Bucket {
    input: u64,
    output: u64,
    cached: u64,
    requests: u64,
    observed: u64,
}

impl Bucket {
    fn add(&mut self, r: Reading) {
        self.requests += 1;
        self.input = self.input.saturating_add(r.input);
        self.output = self.output.saturating_add(r.output);
        if let Some(c) = r.cached {
            self.cached = self.cached.saturating_add(c);
            self.observed += 1;
        }
    }

    fn value(&self) -> Value {
        json!({
            "input": self.input,
            "output": self.output,
            "cached": self.cached,
            "requests": self.requests,
            // 上报过缓存字段的请求数：前端据此区分「没观测」与「命中 0」
            "observed": self.observed,
        })
    }
}

/// 一天的汇总（前端画趋势用）。
fn day_list(map: &BTreeMap<String, Bucket>) -> Value {
    Value::Array(
        map.iter()
            .map(|(date, b)| {
                let mut v = b.value();
                v["date"] = json!(date);
                v
            })
            .collect(),
    )
}

fn top_models(map: &HashMap<String, Bucket>, limit: usize) -> Value {
    let mut rows: Vec<(&String, &Bucket)> = map.iter().collect();
    // 按输入量降序：命中率低但量大的模型才是优化重点。
    rows.sort_by(|a, b| b.1.input.cmp(&a.1.input));
    Value::Array(
        rows.into_iter()
            .take(limit)
            .map(|(model, b)| {
                let mut v = b.value();
                v["model"] = json!(model);
                v
            })
            .collect(),
    )
}

// ---------------------------------------------------------------- 网关账本

/// 网关账本：读 `gateway-metrics.jsonl`（环形日志，超过 1 MB 只留尾部）。
///
/// 只统计**拿到过用量**的行：失败请求的 `prompt_tokens` 是 `null`，把它们算进
/// 分母会把命中率稀释（它们既没有 input，也没有命中）。
pub fn gateway(days: i64) -> Value {
    let path = crate::modules::config::store_dir().join("gateway-metrics.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({
            "available": false,
            "reason": format!("还没有网关账本（{} 不存在）：网关还没转发过请求，或刚清过数据。", path.display()),
            "requests": 0, "observed": 0, "input": 0, "output": 0, "cached": 0,
            "byDay": [], "byModel": [],
        });
    };
    let cutoff = cutoff_date(days);
    let mut total = Bucket::default();
    let mut by_day: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut by_model: HashMap<String, Bucket> = HashMap::new();
    let mut skipped_failed = 0u64;

    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // `ts` 形如 `2026-11-13 21:03:44`（本地时间），前 10 位就是日期。
        let Some(ts) = v.get("ts").and_then(Value::as_str) else {
            continue;
        };
        let Some(date) = ts.get(0..10) else { continue };
        if date < cutoff.as_str() {
            continue;
        }
        let Some(input) = v.get("prompt_tokens").and_then(Value::as_i64) else {
            skipped_failed += 1;
            continue;
        };
        if input < 0 {
            continue;
        }
        let output = v
            .get("completion_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        // `cachedTokens` 缺键或 `null` ⇒ 没观测；**不许**当成 0。
        let cached = v
            .get("cachedTokens")
            .and_then(Value::as_i64)
            .filter(|c| *c >= 0)
            .map(|c| c as u64);
        let r = Reading {
            input: input as u64,
            output: output as u64,
            cached,
        };
        total.add(r);
        by_day.entry(date.to_string()).or_default().add(r);
        let model = v
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("(未知)")
            .to_string();
        by_model.entry(model).or_default().add(r);
    }

    let mut out = total.value();
    out["available"] = json!(true);
    out["byDay"] = day_list(&by_day);
    out["byModel"] = top_models(&by_model, 8);
    // 「有行但都没用量」与「账本不存在」必须分开说：前者是网关在跑但上游没回用量。
    out["note"] = json!(format!(
        "来自网关账本（{} 条无用量记录已跳过）",
        skipped_failed
    ));
    out
}

// ------------------------------------------------------------ WorkBuddy 客户端

/// WorkBuddy 客户端：扫 `~/.workbuddy/projects/**/*.jsonl` 里的 `usage`。
///
/// ⚠️ 实测两件事（2026-11-13，改之前先复测）：
/// 1. 带 `usage` 的**绝大多数是 `function_call` 记录**（一个 138 轮的会话里 137 条是它，
///    `message` 只有 1 条）—— 只认 `type == "message"` 会漏掉 99% 的用量；
/// 2. 每条记录的 `id` / `parentId` 都唯一，所以「每轮各算一次」是对的，
///    不能按 `(input, output, cached)` 去重（真实存在两轮读数完全相同的情况）。
///
/// 结果按 10 分钟 + 文件签名缓存到 `cache/`：1.2 GB 扫描约 3 秒，不能每次开页都重跑。
pub fn workbuddy(days: i64, refresh: bool) -> Value {
    let root = home_dir().join(".workbuddy").join("projects");
    if !root.is_dir() {
        return json!({
            "available": false,
            "reason": "本机没有 WorkBuddy 会话目录（`~/.workbuddy/projects`），可能还没装 / 没用过客户端。",
            "requests": 0, "observed": 0, "input": 0, "output": 0, "cached": 0, "byDay": [],
        });
    }
    let files = collect_jsonl(&root);
    let signature = signature_of(&files);
    let cache_path = crate::modules::config::store_dir()
        .join("cache")
        .join("cache-stats-wb.json");

    if !refresh {
        if let Some(cached) = read_cache(&cache_path, days, &signature) {
            return cached;
        }
    }

    let started = std::time::Instant::now();
    let cutoff_ms = cutoff_ms(days);
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();

    // 并行扫：与 `workbuddy_sessions::search_messages` 同一策略（按文件切块）。
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let chunk = files.len().div_ceil(threads).max(1);
    let mut partial: Vec<(Bucket, BTreeMap<String, Bucket>)> = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for part in files.chunks(chunk) {
            handles.push(scope.spawn(move || scan_files(part, cutoff_ms)));
        }
        for h in handles {
            if let Ok(v) = h.join() {
                partial.push(v);
            }
        }
    });

    let mut total = Bucket::default();
    let mut by_day: BTreeMap<String, Bucket> = BTreeMap::new();
    for (b, days_map) in partial {
        total.input = total.input.saturating_add(b.input);
        total.output = total.output.saturating_add(b.output);
        total.cached = total.cached.saturating_add(b.cached);
        total.requests += b.requests;
        total.observed += b.observed;
        for (date, db) in days_map {
            let slot = by_day.entry(date).or_default();
            slot.input = slot.input.saturating_add(db.input);
            slot.output = slot.output.saturating_add(db.output);
            slot.cached = slot.cached.saturating_add(db.cached);
            slot.requests += db.requests;
            slot.observed += db.observed;
        }
    }

    let mut out = total.value();
    out["available"] = json!(true);
    out["byDay"] = day_list(&by_day);
    out["files"] = json!(files.len());
    out["bytes"] = json!(total_bytes);
    out["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
    out["scannedAt"] = json!(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
    out["note"] = json!("来自 WorkBuddy 会话日志（每轮一条 usage，按记录 id 唯一）");
    write_cache(&cache_path, days, &signature, &out);
    out
}

struct FileEntry {
    path: PathBuf,
    /// 文件修改时间（毫秒）——只用来判断「这个文件里不可能有窗口内的记录」，不参与统计。
    mtime_ms: i64,
    size: u64,
}

fn collect_jsonl(root: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in dirs.flatten() {
        let p = entry.path();
        if p.is_file() {
            push_if_jsonl(&mut out, &p);
            continue;
        }
        let Ok(files) = std::fs::read_dir(&p) else {
            continue;
        };
        for f in files.flatten() {
            let fp = f.path();
            if fp.is_file() {
                push_if_jsonl(&mut out, &fp);
            }
        }
    }
    out
}

fn push_if_jsonl(out: &mut Vec<FileEntry>, p: &Path) {
    if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return;
    }
    let Ok(meta) = std::fs::metadata(p) else {
        return;
    };
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    out.push(FileEntry {
        path: p.to_path_buf(),
        mtime_ms,
        size: meta.len(),
    });
}

/// 文件签名（数量 + 总字节 + 最新 mtime）：任一变化都说明有新的会话内容。
fn signature_of(files: &[FileEntry]) -> String {
    let newest = files.iter().map(|f| f.mtime_ms).max().unwrap_or(0);
    let bytes: u64 = files.iter().map(|f| f.size).sum();
    format!("{}:{}:{}", files.len(), bytes, newest)
}

fn scan_files(
    files: &[FileEntry],
    cutoff_ms: i64,
) -> (Bucket, BTreeMap<String, Bucket>) {
    let mut total = Bucket::default();
    let mut by_day: BTreeMap<String, Bucket> = BTreeMap::new();
    // 记录 id 去重：同一条记录被写两遍时不该算两次用量。
    let mut seen: HashSet<String> = HashSet::new();

    for f in files {
        // 整个文件最后写入时间早于窗口起点 ⇒ 里面不可能有窗口内的记录（省掉 90% 的读取）。
        if f.mtime_ms > 0 && f.mtime_ms < cutoff_ms {
            continue;
        }
        let Ok(bytes) = std::fs::read(&f.path) else {
            continue;
        };
        for line_bytes in bytes.split(|b| *b == b'\n') {
            // 快速预筛：绝大多数行没有 `"usage"`，不必解码、更不必解析 JSON。
            // ⚠️ 必须直接把切片传进去：早先写的是 `&line_bytes.to_vec()`，等于把
            // 1.4 GB 逐行复制一遍，实测从 3 秒劣化到 23 秒。
            if !crate::modules::search_text::contains_ci_bytes(line_bytes, "\"usage\"") {
                continue;
            }
            let line = String::from_utf8_lossy(line_bytes);
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let Some(reading) = reading_of(&v) else {
                continue;
            };
            let Some(ts) = v.get("timestamp").and_then(Value::as_i64) else {
                continue;
            };
            if ts < cutoff_ms {
                continue;
            }
            if let Some(id) = v.get("id").and_then(Value::as_str) {
                if !seen.insert(id.to_string()) {
                    continue;
                }
            }
            total.add(reading);
            let date = date_of(ts);
            by_day.entry(date).or_default().add(reading);
        }
    }
    (total, by_day)
}

/// 从一条记录里取用量：`message.usage` → `providerData.usage` → 顶层 `usage`。
fn reading_of(v: &Value) -> Option<Reading> {
    let usage = v
        .get("message")
        .and_then(|m| m.get("usage"))
        .or_else(|| v.get("providerData").and_then(|p| p.get("usage")))
        .or_else(|| v.get("usage"))?;
    let obj = usage.as_object()?;
    let input = field(obj, &["input_tokens", "inputTokens", "prompt_tokens"])?;
    let output = field(obj, &["output_tokens", "outputTokens", "completion_tokens"]).unwrap_or(0);
    Some(Reading {
        input,
        output,
        cached: cached_field(obj),
    })
}

/// 缓存命中数的别名族（OpenAI / DeepSeek / Anthropic / WB 实测）。
///
/// ⚠️ 用 `positive_field` 而不是 `field`：某些上游会留一个陈旧的 `cache_read: 0`，
/// 用「第一个存在的键」会被它挡住，真正的 `prompt_cache_hit_tokens` 就永远读不到。
fn cached_field(obj: &serde_json::Map<String, Value>) -> Option<u64> {
    let flat = positive_field(
        obj,
        &[
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "prompt_cache_hit_tokens",
            "cached_tokens",
        ],
    );
    flat.or_else(|| {
        obj.get("prompt_tokens_details")
            .and_then(Value::as_object)
            .and_then(|d| positive_field(d, &["cached_tokens"]))
    })
    .or_else(|| {
        obj.get("inputTokensDetails")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find_map(|it| {
                    it.as_object()
                        .and_then(|d| positive_field(d, &["cached_tokens"]))
                })
            })
    })
}

fn number(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|n| n as u64)
    })
}

fn field(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| obj.get(*k).and_then(number))
}

fn positive_field(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(number).filter(|n| *n > 0))
}

fn date_of(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn cutoff_ms(days: i64) -> i64 {
    let days = days.clamp(1, 365);
    let now = chrono::Local::now();
    (now - chrono::Duration::days(days))
        .timestamp_millis()
}

fn cutoff_date(days: i64) -> String {
    let days = days.clamp(1, 365);
    (chrono::Local::now() - chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 磁盘缓存

fn read_cache(path: &Path, days: i64, signature: &str) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let same_days = v.get("days").and_then(Value::as_i64) == Some(days);
    let same_sig = v.get("signature").and_then(Value::as_str) == Some(signature);
    let at = v.get("cachedAtMs").and_then(Value::as_i64).unwrap_or(0);
    let fresh = chrono::Local::now().timestamp_millis() - at < 10 * 60 * 1000;
    if same_days && same_sig && fresh {
        let mut out = v.get("result")?.clone();
        // 缓存命中必须能看出来，否则用户会以为刚算过。
        out["fromCache"] = json!(true);
        return Some(out);
    }
    None
}

fn write_cache(path: &Path, days: i64, signature: &str, result: &Value) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = json!({
        "days": days,
        "signature": signature,
        "cachedAtMs": chrono::Local::now().timestamp_millis(),
        "result": result,
    });
    if let Ok(text) = serde_json::to_string(&body) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

// ---------------------------------------------------------------- 汇总出口

/// 一次返回三端 + 两处「不可用及原因」。
///
/// ⚠️ Trae / Qoder 的 `available=false` 是**结论**，不是失败：它们的客户端日志里
/// 根本没有缓存字段（见模块注释的实测表）。前端必须显示原因，不能显示 0%。
pub fn report(days: i64, refresh: bool) -> Value {
    let days = days.clamp(1, 365);
    json!({
        "days": days,
        "gateway": gateway(days),
        "workbuddy": workbuddy(days, refresh),
        "trae": {
            "available": false,
            "reason": "Trae 客户端会话库里只有输入 / 输出 token，没有缓存命中字段；网关侧从 Trae 上游拿到的命中数恒为 0，据此算等于伪造。",
        },
        "qoder": {
            "available": false,
            "reason": "Qoder 客户端日志里的 input / output / cache 字段全是 0（只有 credits 有真值），拿不到可计算的分母。",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_alias_family_prefers_positive_value() {
        let v: Value = serde_json::from_str(
            r#"{"cache_read_input_tokens":0,"prompt_cache_hit_tokens":512,"input_tokens":1000}"#,
        )
        .unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(cached_field(obj), Some(512), "陈旧的 0 不许挡住真值");
    }

    #[test]
    fn cached_nested_and_absent_are_distinct() {
        let nested: Value = serde_json::from_str(
            r#"{"prompt_tokens_details":{"cached_tokens":64},"prompt_tokens":128}"#,
        )
        .unwrap();
        assert_eq!(cached_field(nested.as_object().unwrap()), Some(64));

        let absent: Value =
            serde_json::from_str(r#"{"input_tokens":100,"output_tokens":5}"#).unwrap();
        assert_eq!(cached_field(absent.as_object().unwrap()), None, "没上报必须是 None");
    }

    #[test]
    fn bucket_counts_only_observed_requests() {
        let mut b = Bucket::default();
        b.add(Reading { input: 100, output: 10, cached: Some(60) });
        b.add(Reading { input: 100, output: 10, cached: None });
        assert_eq!(b.requests, 2);
        assert_eq!(b.observed, 1, "没上报的那条不进观测计数");
        assert_eq!(b.cached, 60, "cached 不因另一条缺失而变");
    }

    /// 真机探针（`#[ignore]`）：把三端现状打出来，只读。
    ///
    /// 跑法：`cargo test -p wb-switch-core --lib cache_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn cache_probe_reads_real_data() {
        let v = report(7, true);
        for key in ["gateway", "workbuddy"] {
            let s = &v[key];
            println!(
                "{key}: available={} requests={} observed={} input={} cached={} 用时={}ms",
                s["available"], s["requests"], s["observed"], s["input"], s["cached"],
                s.get("elapsedMs").cloned().unwrap_or(serde_json::json!("-"))
            );
            if s["requests"].as_u64().unwrap_or(0) > 0 {
                let input = s["input"].as_u64().unwrap_or(0);
                let cached = s["cached"].as_u64().unwrap_or(0);
                println!(
                    "   命中率(观测到的那部分) = {:.2}%",
                    if input > 0 { cached as f64 * 100.0 / input as f64 } else { 0.0 }
                );
            }
            if let Some(days) = s["byDay"].as_array() {
                for d in days.iter().rev().take(3) {
                    println!("   {} input={} cached={}", d["date"], d["input"], d["cached"]);
                }
            }
        }
        println!("trae: {}", v["trae"]["reason"]);
        println!("qoder: {}", v["qoder"]["reason"]);
    }

    #[test]
    fn reading_of_prefers_message_usage_then_provider() {
        let v: Value = serde_json::from_str(
            r#"{"message":{"usage":{"input_tokens":200,"output_tokens":3,"cache_read_input_tokens":150}}}"#,
        )
        .unwrap();
        let r = reading_of(&v).unwrap();
        assert_eq!((r.input, r.output, r.cached), (200, 3, Some(150)));

        let p: Value = serde_json::from_str(
            r#"{"providerData":{"usage":{"prompt_tokens":50,"completion_tokens":1}}}"#,
        )
        .unwrap();
        let r = reading_of(&p).unwrap();
        assert_eq!((r.input, r.cached), (50, None));
    }
}
