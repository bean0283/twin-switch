//! 诊断日志（**默认关闭**）。
//!
//! 存在理由：2026-10-26 起有用户反馈「账号刷新失败 / 401」，而开发侧完全正常、复现不了。
//! 这种问题只能靠**出问题那台机器自己留下证据** —— 所以加一个用户可开、默认关的诊断日志。
//!
//! ## 落地形状
//!
//! ```text
//! ~/.twin-switch/v4/logs/
//!   2026-10-26/
//!     app.log            ← 正在写
//!     app-221503.log     ← 超过单文件上限后轮转出来的
//!   2026-10-25/
//!     app.log
//! ```
//!
//! - **按天分文件夹**：出问题时用户只要说日期，就能定位到那一份。
//! - **单文件限额轮转**：一个文件写满 [`DiagLogConfig::max_mb`] 就改名让位，
//!   新开一个 `app.log`；避免「一份日志几百 MB，记事本打不开」。
//! - **保留策略**：超过 `max_days` 的日期文件夹、以及每个日期里超过 `max_files` 份的旧文件，
//!   一律**直接删除（不进回收站）**。理由：这是本工具自己产生的临时诊断文件，
//!   进回收站只会把用户的磁盘占满 —— 而且清日志本来就是「我要腾地方」的动作。
//!
//! ## 级别
//!
//! 默认 `error`：只记**失败**（请求错误、400 / 401 / 403 / 429 / 5xx、刷新积分失败…）。
//! 切到 `info` 才会连正常流程一起记 —— 那个量级大得多，只建议短期开着排查。
//!
//! ## 纪律
//!
//! - **绝不记录凭据**：写盘前统一过 [`scrub`]，把 jwt / token / authorization 这类长串打码。
//! - **绝不影响主流程**：任何 IO 失败都静默降级（日志本身不该让功能变不好用）。
//! - **不阻塞主线程**：写入是几十字节的追加；只有轮转 / 清理会做目录操作，
//!   且清理有节流（[`CLEANUP_INTERVAL_MS`]），不是每次写都扫全目录。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use crate::modules::config::{now_ms, store_dir};

/// 单文件默认上限（MB）。8 MB 大约是「几万行错误」的量级，记事本仍能打开。
const DEFAULT_MAX_MB: u64 = 8;
/// 每个日期文件夹默认最多留几份（含正在写的 `app.log`）。
const DEFAULT_MAX_FILES: u32 = 5;
/// 默认保留天数。
const DEFAULT_MAX_DAYS: u32 = 7;
/// 单行上限：再长就截断。上游错误体偶尔会把整个 HTML 打回来，不截断会撑爆日志。
const LINE_MAX_BYTES: usize = 4000;
/// 清理节流：两次清理之间至少隔这么久（避免高频写日志时反复扫目录）。
const CLEANUP_INTERVAL_MS: i64 = 60 * 60 * 1000;

/// 写入串行化：多条线程同时追加会互相覆盖（尤其是「快要轮转」的那一刻）。
static WRITE_LOCK: Mutex<()> = Mutex::new(());
/// 上次清理的时间戳（毫秒）；0 = 还没清理过。
static LAST_CLEANUP_MS: AtomicI64 = AtomicI64::new(0);

/// 诊断日志设置。字段是 **camelCase** —— 与 `settings.json` 和界面入参共用一套命名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagLogConfig {
    /// 总开关。**默认关**：日志涉及把运行细节写进磁盘，必须由用户亲手打开。
    #[serde(default)]
    pub enabled: bool,
    /// 级别：`error`（默认，只记失败）/ `info`（连正常流程一起记）。
    #[serde(default = "level_error")]
    pub level: String,
    /// 保留天数（早于今天-N 天的文件夹整个删掉）。`0` = 不按天删。
    #[serde(default = "days_7")]
    pub max_days: u32,
    /// 每个日期文件夹最多留几份文件（超出删最旧的）。`0` = 不按份数删。
    #[serde(default = "files_5")]
    pub max_files: u32,
    /// 单个文件上限（MB）。`0` = 不轮转（不建议）。
    #[serde(default = "mb_8")]
    pub max_mb: u64,
}

fn level_error() -> String {
    "error".to_string()
}
fn days_7() -> u32 {
    DEFAULT_MAX_DAYS
}
fn files_5() -> u32 {
    DEFAULT_MAX_FILES
}
fn mb_8() -> u64 {
    DEFAULT_MAX_MB
}

impl Default for DiagLogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            level: level_error(),
            max_days: DEFAULT_MAX_DAYS,
            max_files: DEFAULT_MAX_FILES,
            max_mb: DEFAULT_MAX_MB,
        }
    }
}

impl DiagLogConfig {
    /// 夹到合理区间。界面上是数字输入框，手输可能离谱（99 万天 / 0 MB）。
    pub fn sanitized(mut self) -> Self {
        self.level = match self.level.trim().to_ascii_lowercase().as_str() {
            "info" => "info".to_string(),
            _ => "error".to_string(),
        };
        self.max_days = self.max_days.min(365);
        self.max_files = self.max_files.min(200);
        self.max_mb = self.max_mb.min(512);
        self
    }

    /// 该级别要不要记：`info` 记全部，`error` 只记 error。
    fn allows(&self, level: &str) -> bool {
        match level {
            "error" => true,
            _ => self.level == "info",
        }
    }
}

/// 日志根目录（`<store>/logs`）。
pub fn log_root() -> PathBuf {
    store_dir().join("logs")
}

/// 当前设置（读不到就回默认：**关**）。
///
/// ⚠️ 这里不能反过来依赖 `settings::load()` 的缓存 —— 用户刚拨开关就该立刻生效，
/// 而 `settings::load()` 每次都是现读文件，正好满足。
fn config_now() -> DiagLogConfig {
    crate::modules::settings::load().log.sanitized()
}

// ---------------------------------------------------------------------------
// 对外写入口
// ---------------------------------------------------------------------------

/// 记一条 **错误**（默认就在记这一级）。
pub fn error(scope: &str, message: impl AsRef<str>) {
    write("ERROR", scope, message.as_ref());
}

/// 记一条 **警告**（只有 `info` 级别才落盘）。
pub fn warn(scope: &str, message: impl AsRef<str>) {
    write("WARN", scope, message.as_ref());
}

/// 记一条 **信息**（只有 `info` 级别才落盘）。
pub fn info(scope: &str, message: impl AsRef<str>) {
    write("INFO", scope, message.as_ref());
}

/// 记一条 **系统备注**：只看总开关，**不受级别门槛限制**。
///
/// 为什么需要：默认级别是 `error`，用户刚把开关拨开时如果没有失败发生，
/// 日志就永远是 0 个文件 —— 用户没法确认「功能真的在跑」，只会觉得它坏了
/// （实测反馈：「日志功能不可用，没有写入」）。保存设置的那一刻写一条自证行，
/// 「打开文件夹」立刻能看到东西。
pub fn system_note(scope: &str, message: impl AsRef<str>) {
    let cfg = config_now();
    if !cfg.enabled {
        return;
    }
    let _ = append_at(&log_root(), &cfg, "NOTE", scope, message.as_ref(), now_ms());
}

/// 统一写入口：开关关着就**立刻返回**（一个字符串都不拼，避免给热路径加成本）。
fn write(level: &str, scope: &str, message: &str) {
    let cfg = config_now();
    if !cfg.enabled || !cfg.allows(level) {
        return;
    }
    let _ = append_at(&log_root(), &cfg, level, scope, message, now_ms());
    // 清理按小时节流；失败无所谓（下次还会试）。
    let last = LAST_CLEANUP_MS.load(Ordering::Relaxed);
    let now = now_ms();
    if now - last > CLEANUP_INTERVAL_MS {
        LAST_CLEANUP_MS.store(now, Ordering::Relaxed);
        let _ = cleanup_at(&log_root(), &cfg, now);
    }
}

// ---------------------------------------------------------------------------
// 核心实现（全部接收显式路径 —— 单测用临时目录，绝不碰用户的真实 store）
// ---------------------------------------------------------------------------

/// 毫秒时间戳 → 本地日期目录名 `YYYY-MM-DD`。
fn day_dir_name(ms: i64) -> String {
    crate::modules::config::fmt_day_local(ms)
}

/// 毫秒时间戳 → 一行日志的时间前缀 `YYYY-MM-DD HH:MM:SS.mmm`。
fn stamp(ms: i64) -> String {
    crate::modules::config::fmt_ms_with_millis(ms)
}

/// 把一条日志追加进当天的 `app.log`，必要时先轮转。
fn append_at(
    root: &Path,
    cfg: &DiagLogConfig,
    level: &str,
    scope: &str,
    message: &str,
    at_ms: i64,
) -> std::io::Result<()> {
    let day = root.join(day_dir_name(at_ms));
    std::fs::create_dir_all(&day)?;
    let active = day.join("app.log");

    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // 轮转：先看现有大小（含本条会更新的量级），超了就改名让位。
    if cfg.max_mb > 0 {
        let limit = cfg.max_mb.saturating_mul(1024 * 1024);
        if let Ok(meta) = std::fs::metadata(&active) {
            if meta.len() >= limit {
                let rotated = day.join(format!("app-{}.log", short_time(at_ms)));
                // 同名（同一秒内连续轮转）时加序号，绝不覆盖已有文件。
                let mut target = rotated;
                let mut n = 1;
                while target.exists() {
                    target = day.join(format!("app-{}-{n}.log", short_time(at_ms)));
                    n += 1;
                }
                let _ = std::fs::rename(&active, &target);
            }
        }
    }

    let line = format!(
        "{} {} [{}] {}\n",
        stamp(at_ms),
        level,
        scope,
        truncate(&scrub(message))
    );
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&active)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

fn short_time(ms: i64) -> String {
    // `HHMMSS`：同一天内唯一到秒即可，比完整时间戳短。
    let full = stamp(ms);
    full.chars()
        .skip(11)
        .take(8)
        .filter(|c| *c != ':')
        .collect::<String>()
}

/// 截断到 [`LINE_MAX_BYTES`]，且不切断多字节字符。
fn truncate(s: &str) -> String {
    if s.len() <= LINE_MAX_BYTES {
        return s.to_string();
    }
    let mut end = LINE_MAX_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…（已截断，共 {} 字节）", &s[..end], s.len())
}

/// **凭据打码**：日志里绝不允许出现能直接拿去用的 token。
///
/// 规则简单粗暴但覆盖得住实际情况：
/// - `Bearer xxx` / `authorization: xxx` / `"token":"xxx"` 这类**键值后面**的长串打码；
/// - 任何连续 ≥ 40 的字母数字串（jwt 就是这种形态）打码成 `abcd…efgh（共 N 字符）`。
///
/// ⚠️ 宁可多打码也不能漏：日志是用户会直接发到群里的东西。
pub fn scrub(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let mut prev_sep = true;

    let flush = |run: &mut String, out: &mut String| {
        if run.is_empty() {
            return;
        }
        let is_secretish = run.len() >= 40
            && run.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        if is_secretish {
            let head: String = run.chars().take(6).collect();
            let tail: String = run.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            out.push_str(&format!("{head}…{tail}（共 {} 字符，已打码）", run.chars().count()));
        } else {
            out.push_str(run);
        }
        run.clear();
    };

    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            run.push(ch);
            prev_sep = false;
        } else {
            flush(&mut run, &mut out);
            out.push(ch);
            prev_sep = !ch.is_whitespace() || prev_sep;
        }
    }
    flush(&mut run, &mut out);
    let _ = prev_sep;
    out
}

/// 清理结果（给界面「清掉了什么」用）。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub removed_days: usize,
    pub removed_files: usize,
    pub freed_bytes: u64,
}

/// 按保留策略清理：**直接删除，不进回收站**。
///
/// 顺序：先按天数删整个日期文件夹，再按份数删每日多余的旧文件。
/// `now_ms` 显式传入，便于单测。
pub fn cleanup_at(root: &Path, cfg: &DiagLogConfig, at_ms: i64) -> CleanupReport {
    let mut report = CleanupReport::default();
    let Ok(rd) = std::fs::read_dir(root) else {
        return report;
    };

    // 今天（含）以内要保留的日期集合：用「日期字符串倒序比较」判断太脆，
    // 直接算允许的最早日期字符串（`YYYY-MM-DD` 字典序 = 时间序，可以直接比）。
    let cutoff = if cfg.max_days > 0 {
        Some(day_dir_name(at_ms - (cfg.max_days as i64) * 86_400_000))
    } else {
        None
    };

    let mut days: Vec<(String, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !e.path().is_dir() {
            continue;
        }
        // 只认 `YYYY-MM-DD` 形状的目录 —— 别人往这里放的东西不归我们删。
        if name.len() != 10 || name.chars().filter(|c| *c == '-').count() != 2 {
            continue;
        }
        days.push((name, e.path()));
    }
    days.sort();

    for (name, path) in &days {
        let too_old = cutoff.as_ref().map(|c| name.as_str() < c.as_str()).unwrap_or(false);
        if !too_old {
            continue;
        }
        report.freed_bytes += dir_bytes(path);
        match std::fs::remove_dir_all(path) {
            Ok(_) => report.removed_days += 1,
            Err(_) => {}
        }
    }

    if cfg.max_files > 0 {
        for (name, path) in &days {
            if let Some(c) = cutoff.as_ref() {
                if name.as_str() < c.as_str() {
                    continue; // 整目录已经删了
                }
            }
            let removed = trim_day_files(path, cfg.max_files, &mut report.freed_bytes);
            report.removed_files += removed;
        }
    }
    report
}

/// 每个日期目录内只留最新的 `keep` 份文件。
fn trim_day_files(day: &Path, keep: u32, freed: &mut u64) -> usize {
    let Ok(rd) = std::fs::read_dir(day) else {
        return 0;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        files.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), p, meta.len()));
    }
    if files.len() <= keep as usize {
        return 0;
    }
    // 最新的 keep 份留下，其余删掉。
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let mut removed = 0;
    for (_, p, len) in files.into_iter().skip(keep as usize) {
        if std::fs::remove_file(&p).is_ok() {
            removed += 1;
            *freed += len;
        }
    }
    removed
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| if m.is_file() { m.len() } else { 0 })
        .sum()
}

// ---------------------------------------------------------------------------
// 给界面用的只读快照
// ---------------------------------------------------------------------------

/// 日志现状：开关、目录、当前占用、文件数、最近几行（供设置页直接看）。
pub fn status() -> Value {
    let cfg = config_now();
    let root = log_root();
    let mut days: Vec<Value> = Vec::new();
    let mut total_bytes = 0u64;
    let mut total_files = 0usize;
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let bytes = dir_bytes(&e.path());
            let files = std::fs::read_dir(e.path())
                .map(|r| r.flatten().filter(|f| f.path().is_file()).count())
                .unwrap_or(0);
            total_bytes += bytes;
            total_files += files;
            days.push(json!({ "day": name, "bytes": bytes, "files": files }));
        }
    }
    days.sort_by(|a, b| b["day"].as_str().cmp(&a["day"].as_str()));
    json!({
        "ok": true,
        "enabled": cfg.enabled,
        "level": cfg.level,
        "maxDays": cfg.max_days,
        "maxFiles": cfg.max_files,
        "maxMb": cfg.max_mb,
        "dir": root.to_string_lossy(),
        "totalBytes": total_bytes,
        "totalFiles": total_files,
        "days": days,
    })
}

/// 最近 `limit` 行（从最新的日期目录往回取），供界面「看一眼」。
pub fn tail(limit: usize) -> Vec<String> {
    let root = log_root();
    let mut out: Vec<String> = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return out;
    };
    let mut days: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    days.sort();
    for day in days.into_iter().rev() {
        let Ok(files) = std::fs::read_dir(&day) else {
            continue;
        };
        let mut list: Vec<(std::time::SystemTime, PathBuf)> = files
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| {
                let t = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, e.path())
            })
            .collect();
        list.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, p) in list {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
            if lines.len() >= limit - out.len() {
                let take = limit - out.len();
                let start = lines.len() - take;
                out.extend(lines.drain(start..));
                return out;
            }
            out.extend(lines);
        }
        if out.len() >= limit {
            return out;
        }
    }
    out
}

/// 清空全部日志（用户点「立即清理」）。**直接删目录，不进回收站。**
pub fn clear() -> std::io::Result<u64> {
    let root = log_root();
    let bytes = dir_bytes(&root);
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("diag-log-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg_on(max_mb: u64, max_days: u32, max_files: u32) -> DiagLogConfig {
        DiagLogConfig {
            enabled: true,
            level: "error".into(),
            max_days,
            max_files,
            max_mb,
        }
    }

    /// 基本写入：一行、含级别与 scope、落在当日目录的 app.log。
    #[test]
    fn writes_one_line_into_today_folder() {
        let root = tmp();
        let now = now_ms();
        append_at(&root, &cfg_on(8, 7, 5), "ERROR", "trae_credits", "401 unauthorized", now).unwrap();
        let active = root.join(day_dir_name(now)).join("app.log");
        let text = std::fs::read_to_string(&active).unwrap();
        assert!(text.contains(" ERROR ["), "缺级别：{text}");
        assert!(text.contains("trae_credits"), "缺 scope：{text}");
        assert!(text.contains("401 unauthorized"), "缺正文：{text}");
        assert_eq!(text.lines().count(), 1, "一次写入只该一行");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 轮转：写满上限后新开一个 app.log，旧的改名保留。
    #[test]
    fn rotates_when_file_reaches_limit() {
        let root = tmp();
        let now = now_ms();
        let day = root.join(day_dir_name(now));
        std::fs::create_dir_all(&day).unwrap();
        // max_mb = 0 会关闭轮转，所以这里用 1 MB，先塞一个刚好超限的 app.log
        std::fs::write(day.join("app.log"), vec![b'x'; 1024 * 1024 + 10]).unwrap();

        append_at(&root, &cfg_on(1, 7, 5), "ERROR", "gateway", "boom", now).unwrap();

        let files: Vec<String> = std::fs::read_dir(&day)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(files.contains(&"app.log".to_string()), "轮转后要新开 app.log：{files:?}");
        assert!(
            files.iter().any(|f| f.starts_with("app-") && f != "app.log"),
            "旧文件要改名保留：{files:?}"
        );
        let fresh = std::fs::read_to_string(day.join("app.log")).unwrap();
        assert!(fresh.contains("boom"), "新文件里要有这次的内容");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 保留天数：早于 maxDays 的日期目录整个删除，且在保留期内的不动。
    #[test]
    fn cleanup_drops_folders_older_than_max_days() {
        let root = tmp();
        let now = now_ms();
        let day = |offset: i64| root.join(day_dir_name(now - offset * 86_400_000));
        std::fs::create_dir_all(day(1)).unwrap();
        std::fs::write(day(1).join("app.log"), b"keep me").unwrap();
        std::fs::create_dir_all(day(10)).unwrap();
        std::fs::write(day(10).join("app.log"), b"delete me").unwrap();

        let rep = cleanup_at(&root, &cfg_on(8, 7, 5), now);
        assert_eq!(rep.removed_days, 1, "只该删掉 10 天前那个");
        assert!(day(1).exists(), "1 天前的必须留着");
        assert!(!day(10).exists(), "10 天前的必须删掉");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 保留份数：同一日期里超过 maxFiles 的旧文件删掉，新的留着。
    #[test]
    fn cleanup_keeps_only_newest_files_per_day() {
        let root = tmp();
        let now = now_ms();
        let day = root.join(day_dir_name(now));
        std::fs::create_dir_all(&day).unwrap();
        for i in 0..6 {
            std::fs::write(day.join(format!("app-{i}.log")), format!("line {i}")).unwrap();
            // 让 mtime 明确区分新旧（Windows 时间精度足够 10ms 级）
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
        let rep = cleanup_at(&root, &cfg_on(8, 7, 3), now);
        assert_eq!(rep.removed_files, 3, "6 份只留 3 份 ⇒ 删 3 份");
        let left = std::fs::read_dir(&day).unwrap().flatten().count();
        assert_eq!(left, 3);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 不认识的目录名不许删（别人往 logs 里放的东西不归我们清理）。
    #[test]
    fn cleanup_ignores_foreign_directories() {
        let root = tmp();
        let now = now_ms();
        let foreign = root.join("my-own-stuff");
        std::fs::create_dir_all(&foreign).unwrap();
        cleanup_at(&root, &cfg_on(8, 7, 1), now);
        assert!(foreign.exists(), "非日期目录必须原样保留");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 打码：jwt 这类长串必须被抹掉，短词不动。
    #[test]
    fn scrub_masks_jwt_like_runs_only() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let s = format!("authorization: {jwt} status 401");
        let masked = scrub(&s);
        assert!(!masked.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"), "jwt 必须打码：{masked}");
        assert!(masked.contains("已打码"), "要说明打码了：{masked}");
        assert!(masked.contains("status 401"), "普通短词不该被动：{masked}");

        // 短串不受影响
        assert_eq!(scrub("code 1001 token expired"), "code 1001 token expired");
    }

    /// 关闭时不许落盘（哪怕调了写入口）。
    #[test]
    fn disabled_config_allows_nothing() {
        let off = DiagLogConfig::default();
        assert!(!off.enabled);
        assert!(off.allows("error"), "error 级别本身永远允许，由 enabled 兜底");
        assert!(!off.allows("info"), "默认 error 级别不该记 info");
        let info = DiagLogConfig {
            enabled: true,
            level: "info".into(),
            ..DiagLogConfig::default()
        };
        assert!(info.allows("info"));
    }

    /// 单行截断：超长错误体不许把日志撑爆。
    #[test]
    fn long_lines_are_truncated() {
        let long = "x".repeat(LINE_MAX_BYTES * 3);
        let t = truncate(&long);
        assert!(t.len() < LINE_MAX_BYTES + 100, "必须截断到上限附近");
        assert!(t.contains("已截断"), "要标记已截断");
    }
}
