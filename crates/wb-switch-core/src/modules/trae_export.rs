//! Trae 会话记录导出（移植自 trae-session-export/trae_web.py）。
//!
//! 输入是解密库（`trae_decrypt` 产出的明文 SQLite），输出对话 Markdown 与批量 zip。
//! 覆盖：会话列表、会话信息、对话解析（user / assistant 交替）、单会话导出、批量打包。

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

use crate::modules::config::store_dir;
use crate::modules::trae_discover::get_client;

/// 解密库路径：`<工具目录>/trae/decrypted/<clientKey>.db`。
pub fn decrypted_db_path(client_key: &str) -> PathBuf {
    store_dir().join("trae").join("decrypted").join(format!("{client_key}.db"))
}

/// 解密快照的元信息文件。
pub fn snapshot_meta_path(client_key: &str) -> PathBuf {
    store_dir()
        .join("trae")
        .join("decrypted")
        .join(format!("{client_key}.db.meta.json"))
}

/// 「读取视图」明文路径：实时库 WAL 有待合并帧时，用快照的**副本**合并 WAL，
/// 保证快照本体始终是「实时库的纯解密结果」（导入流程把它当作差异比对的基准）。
pub fn decrypted_view_path(client_key: &str) -> PathBuf {
    store_dir()
        .join("trae")
        .join("decrypted")
        .join(format!("{client_key}.view.db"))
}

/// 实时库当前状态的签名。Trae 的库跑在 WAL 模式，**未 checkpoint 前实时库文件字节不变**，
/// 所以「大小 + mtime + 首页 salt + 首页关键字段」一致 ⇒ 解密快照依然等于 `decrypt(实时库)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrcSignature {
    pub size: u64,
    pub mtime_ns: u64,
    pub salt_hex: String,
    pub change_counter: u32,
    pub page_count: u32,
    pub schema_cookie: u32,
}

/// 解密快照的元信息（持久化在 `decrypted/<key>.db.meta.json`）。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SnapshotMeta {
    pub client_key: String,
    pub src_size: u64,
    pub src_mtime_ns: u64,
    pub salt_hex: String,
    pub hdr_change_counter: u32,
    pub hdr_page_count: u32,
    pub hdr_schema_cookie: u32,
    pub pages: u64,
    pub created_ms: i64,
    pub tables: Vec<crate::modules::trae_decrypt::TableStat>,
}

impl SnapshotMeta {
    fn to_signature(&self) -> SrcSignature {
        SrcSignature {
            size: self.src_size,
            mtime_ns: self.src_mtime_ns,
            salt_hex: self.salt_hex.clone(),
            change_counter: self.hdr_change_counter,
            page_count: self.hdr_page_count,
            schema_cookie: self.hdr_schema_cookie,
        }
    }
}

fn mtime_ns(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn hex16(b: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(32);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

/// 读取实时库的签名（首页 HMAC 校验通过才算有效）。
pub fn src_signature(client_key: &str) -> Option<SrcSignature> {
    let client = get_client(client_key)?;
    let db = crate::modules::trae_discover::database_path(client);
    let md = std::fs::metadata(&db).ok()?;
    let key_hex = crate::modules::trae_memory_scan::load_saved_key(client_key)?;
    let key = crate::modules::trae_decrypt::hex_to_bytes(&key_hex);
    let (salt, plain1, size) = crate::modules::trae_decrypt::read_plain_page1(&db, &key)?;
    let be32 = |o: usize| u32::from_be_bytes([plain1[o], plain1[o + 1], plain1[o + 2], plain1[o + 3]]);
    Some(SrcSignature {
        size,
        mtime_ns: mtime_ns(&md),
        salt_hex: hex16(&salt),
        change_counter: be32(24),
        page_count: {
            let pc = be32(28);
            // 头里的页数为 0 时（旧式写法）以文件大小为准
            if pc == 0 { (size / 4096) as u32 } else { pc }
        },
        schema_cookie: be32(40),
    })
}

pub fn read_snapshot_meta(client_key: &str) -> Option<SnapshotMeta> {
    let text = std::fs::read_to_string(snapshot_meta_path(client_key)).ok()?;
    serde_json::from_str::<SnapshotMeta>(&text).ok()
}

pub fn write_snapshot_meta(client_key: &str, meta: &SnapshotMeta) {
    let p = snapshot_meta_path(client_key);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(meta) {
        let _ = std::fs::write(p, text);
    }
}

/// 丢弃元信息：快照内容不再可信时调用（下次使用会重新解密）。
pub fn drop_snapshot_meta(client_key: &str) {
    let _ = std::fs::remove_file(snapshot_meta_path(client_key));
}

/// 快照是否仍是「实时库的纯解密结果」。
pub fn snapshot_is_current(client_key: &str, snap: &Path, meta: &SnapshotMeta) -> bool {
    if meta.client_key != client_key {
        return false;
    }
    let Some(sig) = src_signature(client_key) else {
        return false;
    };
    if sig != meta.to_signature() {
        return false;
    }
    match std::fs::metadata(snap) {
        Ok(md) => md.len() == meta.pages * 4096,
        Err(_) => false,
    }
}

/// 确保解密快照可用且与实时库一致。
///
/// - 一致（签名相同）→ **零解密**直接复用，返回 `reused = true`；
/// - 不一致 → 重新解密并写元信息。
///
/// 为避免「解密期间实时库被客户端改写」导致快照与签名不符，最多重试 3 次，
/// 每次都在解密前后各取一次签名，只有前后一致才落盘元信息。
pub fn ensure_decrypted(
    client_key: &str,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<EnsureOutcome, String> {
    let t0 = std::time::Instant::now();
    let client = get_client(client_key).ok_or("未知客户端")?;
    let db = crate::modules::trae_discover::database_path(client);
    if !db.exists() {
        return Err(format!("未找到实时库：{}", db.display()));
    }
    let out = decrypted_db_path(client_key);

    if out.exists() {
        if let Some(meta) = read_snapshot_meta(client_key) {
            if snapshot_is_current(client_key, &out, &meta) {
                return Ok(EnsureOutcome {
                    reused: true,
                    path: out.to_string_lossy().into_owned(),
                    pages: meta.pages,
                    tables: meta.tables,
                    elapsed_ms: t0.elapsed().as_millis() as u64,
                });
            }
        }
    }

    let key = crate::modules::trae_memory_scan::load_saved_key(client_key)
        .ok_or("没有已存密钥，请先运行「扫描密钥并解密」")?;
    let mut last_err: Option<String> = None;
    for attempt in 1..=3 {
        let before = src_signature(client_key);
        let rep = match crate::modules::trae_decrypt::decrypt_database(&db, &key, &out, on_progress) {
            Ok(r) => r,
            Err(e) => return Err(e),
        };
        let after = src_signature(client_key);
        match (before, after) {
            (Some(b), Some(a)) if b == a => {
                let tables = rep.tables.clone();
                write_snapshot_meta(
                    client_key,
                    &SnapshotMeta {
                        client_key: client_key.to_string(),
                        src_size: a.size,
                        src_mtime_ns: a.mtime_ns,
                        salt_hex: a.salt_hex.clone(),
                        hdr_change_counter: a.change_counter,
                        hdr_page_count: a.page_count,
                        hdr_schema_cookie: a.schema_cookie,
                        pages: rep.pages,
                        created_ms: crate::modules::config::now_ms(),
                        tables: rep.tables,
                    },
                );
                return Ok(EnsureOutcome {
                    reused: false,
                    path: out.to_string_lossy().into_owned(),
                    pages: rep.pages,
                    tables,
                    elapsed_ms: t0.elapsed().as_millis() as u64,
                });
            }
            _ => {
                last_err = Some("实时库在解密期间被客户端改写".into());
                if let Some(cb) = on_progress {
                    cb(&format!("实时库在解密期间发生变化，重试第 {attempt} 次…"));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "解密失败".into()))
}

#[derive(Debug, Clone, Serialize)]
pub struct EnsureOutcome {
    /// 是否直接复用了已有快照（true = 本次没有做整库解密）。
    pub reused: bool,
    pub path: String,
    pub pages: u64,
    pub tables: Vec<crate::modules::trae_decrypt::TableStat>,
    pub elapsed_ms: u64,
}

/// 把一份「刚刚写进实时库的明文」提升为新快照（导入 / 删除成功后调用）。
///
/// 调用前提：实时库已经替换完毕、客户端已退出，且 `plain` 就是新库的完整明文
/// （`write_db_incremental` 保证逐页一致）。提升后元信息立即与新库对齐，
/// 后续读取无需再解密。
pub fn promote_snapshot(client_key: &str, plain: &Path) -> Result<(), String> {
    let snap = decrypted_db_path(client_key);
    if let Some(dir) = snap.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建快照目录失败: {e}"))?;
    }
    if snap.exists() {
        let _ = std::fs::remove_file(&snap);
    }
    std::fs::rename(plain, &snap).map_err(|e| format!("提升快照失败: {e}"))?;
    // 与实时库签名对齐；对不上就丢元信息，下次重新解密（宁可慢，不可错）
    match src_signature(client_key) {
        Some(sig) => {
            let pages = std::fs::metadata(&snap).map(|m| m.len() / 4096).unwrap_or(0);
            let tables = crate::modules::trae_decrypt::list_tables(&snap);
            let meta = SnapshotMeta {
                client_key: client_key.to_string(),
                src_size: sig.size,
                src_mtime_ns: sig.mtime_ns,
                salt_hex: sig.salt_hex,
                hdr_change_counter: sig.change_counter,
                hdr_page_count: sig.page_count,
                hdr_schema_cookie: sig.schema_cookie,
                pages,
                created_ms: crate::modules::config::now_ms(),
                tables,
            };
            write_snapshot_meta(client_key, &meta);
            if !snapshot_is_current(client_key, &snap, &meta) {
                drop_snapshot_meta(client_key);
            }
        }
        None => drop_snapshot_meta(client_key),
    }
    Ok(())
}

/// 导出目录：`<工具目录>/trae/export/`。
pub fn export_dir() -> PathBuf {
    store_dir().join("trae").join("export")
}

/// 整库备份目录（删除会话前）。
pub fn backup_dir() -> PathBuf {
    store_dir().join("trae").join("backup")
}

/// 会话回收站目录（删除会话后文件移入，可恢复）。
pub fn deleted_sessions_dir() -> PathBuf {
    store_dir().join("trae").join("deleted_sessions")
}

/// 分批导入（`trae_import`）的整库备份目录。
pub fn import_backup_dir() -> PathBuf {
    store_dir().join("trae").join("import_backup")
}

/// WorkBuddy → Trae 导入的整库备份目录。
pub fn wb_import_backup_dir() -> PathBuf {
    store_dir().join("workbuddy_import_backup")
}

/// 递归统计一个路径的字节数（文件即自身大小，目录累加）。
fn path_size(p: &Path) -> u64 {
    let Ok(md) = std::fs::metadata(p) else {
        return 0;
    };
    if md.is_file() {
        return md.len();
    }
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten().map(|e| path_size(&e.path())).sum()
}

/// 从备份条目名解析出 `(归属客户端, 时间戳)` 分组键。
///
/// 三种实测命名都能覆盖：
/// - `solo-cn_before_delete_20261004_183502.db.bak`（`trae/backup` 的整库备份，含 `-wal`/`-shm` 兄弟文件）
/// - `solo-cn-20261004-194801`（`workbuddy_import_backup` 的子目录）
/// - `solo-cn-20261003-200952`（`trae/import_backup` 的子目录）
fn backup_owner_and_stamp(entry_name: &str) -> (String, String) {
    if let Some(pos) = entry_name.find("_before_delete_") {
        let owner = entry_name[..pos].to_string();
        let rest = &entry_name[pos + "_before_delete_".len()..];
        // `YYYYMMDD_HHMMSS` 共 15 个字符
        let stamp: String = rest.chars().take(15).collect();
        return (owner, stamp);
    }
    let parts: Vec<&str> = entry_name.split('-').collect();
    if parts.len() >= 3 {
        let n = parts.len();
        let stamp = format!("{}-{}", parts[n - 2], parts[n - 1]);
        let owner = parts[..n - 2].join("-");
        return (owner, stamp);
    }
    (entry_name.to_string(), String::new())
}

/// 备份目录里的一「批」：同一客户端、同一时间戳落下的若干文件/目录。
///
/// 一次整库备份会产生 3 个兄弟文件（`.db.bak` / `-wal.db.bak` / `-shm.db.bak`），
/// 它们必须**同生同死**——只删其中一个会留下无法使用的残片。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupBatch {
    /// 归属客户端 key（`solo-cn` / `trae-cn`）。
    pub owner: String,
    /// `YYYYMMDD_HHMMSS` / `YYYYMMDD-HHMMSS`。
    pub stamp: String,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
}

/// 扫描备份目录并按「客户端 + 时间戳」分批（**只读**，不删任何东西）。
///
/// 顺序即时间序（`owner` 升序、`stamp` 字典序），调用方可据此判定「最新 N 批」。
pub fn list_backup_batches(dir: &Path) -> Vec<BackupBatch> {
    if !dir.is_dir() {
        return Vec::new();
    }
    // 键是 (owner, stamp) 的 BTreeMap → 天然按 owner 分组、按 stamp 升序
    let mut groups: BTreeMap<(String, String), Vec<PathBuf>> = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let (owner, stamp) = backup_owner_and_stamp(&name);
        groups.entry((owner, stamp)).or_default().push(ent.path());
    }
    groups
        .into_iter()
        .map(|((owner, stamp), mut paths)| {
            paths.sort();
            let bytes = paths.iter().map(|p| path_size(p)).sum();
            BackupBatch { owner, stamp, paths, bytes }
        })
        .collect()
}

/// 清理备份目录：**按客户端分组**，每组只保留最新 `keep` 批，回收磁盘。
///
/// 每批 ≈ 一个整库大小（实测 279 MB 量级）。目录里可能是子目录（导入备份），也可能是
/// 文件组（删除前的整库备份 + `-wal`/`-shm` 兄弟文件），本函数两种形态都支持：分组键
/// 取「客户端 + 时间戳」，时间戳为 `YYYYMMDD_HHMMSS` / `YYYYMMDD-HHMMSS`，字典序即时间序。
///
/// 返回 `(删除的条目数, 回收的字节数)`。任何单条删除失败都跳过、不中断。
pub fn prune_backups(dir: &Path, keep: usize) -> (usize, u64) {
    if keep == 0 || !dir.is_dir() {
        return (0, 0);
    }
    let batches = list_backup_batches(dir);
    let mut by_owner: BTreeMap<&str, Vec<&BackupBatch>> = BTreeMap::new();
    for b in &batches {
        by_owner.entry(b.owner.as_str()).or_default().push(b);
    }

    let mut removed = 0usize;
    let mut freed = 0u64;
    for list in by_owner.values() {
        if list.len() <= keep {
            continue;
        }
        // BTreeMap 已按 stamp 升序 → 前 `len - keep` 批是最旧的
        for b in &list[..list.len() - keep] {
            for p in &b.paths {
                let sz = path_size(p);
                let ok = if p.is_dir() {
                    std::fs::remove_dir_all(p).is_ok()
                } else {
                    std::fs::remove_file(p).is_ok()
                };
                if ok {
                    removed += 1;
                    freed += sz;
                }
            }
        }
    }
    (removed, freed)
}

/// 一键回收工作文件占用：
/// 1. 三处备份目录各保留最新 1 批（`trae/backup`、`trae/import_backup`、`workbuddy_import_backup`）；
/// 2. 删除可再生的解密快照（`trae/decrypted/*.db*`，刷新记录页会自动重新解密）。
///
/// 返回 `{ freed_bytes, freed_mb, pruned_entries, details }`。
pub fn cleanup_working_files() -> Value {
    let mut freed = 0u64;
    let mut entries = 0usize;
    let mut details: Vec<Value> = Vec::new();

    let dirs = [
        ("trae/backup（删除会话前的整库备份）", backup_dir()),
        ("trae/import_backup（分批导入备份）", import_backup_dir()),
        ("workbuddy_import_backup（WorkBuddy 导入备份）", wb_import_backup_dir()),
    ];
    for (label, dir) in dirs {
        if !dir.is_dir() {
            continue;
        }
        // 先统计清理前占用，便于把「本来就有多少」如实告诉用户
        let before = path_size(&dir);
        let (n, f) = prune_backups(&dir, 1);
        if n > 0 || before > 0 {
            details.push(json!({
                "name": label,
                "removed": n,
                "beforeMb": (before as f64 / 1_048_576.0 * 10.0).round() / 10.0,
                "freedMb": (f as f64 / 1_048_576.0 * 10.0).round() / 10.0,
            }));
        }
        freed += f;
        entries += n;
    }

    // 解密快照：直接删，下次刷新会按存盘密钥重新生成
    let snap_dir = store_dir().join("trae").join("decrypted");
    let mut snap_freed = 0u64;
    let mut snap_n = 0usize;
    if let Ok(rd) = std::fs::read_dir(&snap_dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            let name = ent.file_name().to_string_lossy().into_owned();
            // 只删 db / -wal / -shm / 元信息，别误删目录里其它东西
            if !(name.ends_with(".db")
                || name.ends_with(".db-wal")
                || name.ends_with(".db-shm")
                || name.ends_with(".meta.json"))
            {
                continue;
            }
            let sz = path_size(&p);
            if std::fs::remove_file(&p).is_ok() {
                snap_freed += sz;
                snap_n += 1;
            }
        }
    }
    if snap_n > 0 {
        details.push(json!({
            "name": "trae/decrypted（解密快照，可自动重建）",
            "removed": snap_n,
            "freedMb": (snap_freed as f64 / 1_048_576.0 * 10.0).round() / 10.0,
        }));
    }
    freed += snap_freed;
    entries += snap_n;

    json!({
        "freed_bytes": freed,
        "freed_mb": (freed as f64 / 1_048_576.0 * 10.0).round() / 10.0,
        "removed_entries": entries,
        "details": details,
    })
}

/// 会话 ID 合法性：20~24 位十六进制（不含 `sess_` 前缀）。
pub fn is_session_id(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    (20..=24).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit())
}

fn prefix_of(client_key: &str) -> &'static str {
    match client_key {
        "trae-cn" => "traecn_session",
        _ => "trae_session",
    }
}

pub fn label_of(client_key: &str) -> String {
    get_client(client_key).map(|c| c.label).unwrap_or(client_key).to_string()
}

fn format_ts(ts: &rusqlite::types::Value) -> String {
    match ts {
        rusqlite::types::Value::Integer(i) => {
            let secs = if *i > 1_000_000_000_000 { *i / 1000 } else { *i };
            chrono::DateTime::from_timestamp(secs, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| i.to_string())
        }
        rusqlite::types::Value::Text(s) => s.clone(),
        _ => String::new(),
    }
}

/// 文件名安全化（非法字符 → `_`）。
pub fn safe_filename(name: &str) -> String {
    let t: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|\r\n\t".contains(c) { '_' } else { c })
        .collect();
    let t = t.trim().to_string();
    if t.is_empty() {
        "session".into()
    } else {
        t
    }
}

/// 按字节上限安全截断：不切断多字节 UTF-8 字符，避免 `&s[..n]` 的 char 边界 panic。
pub fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// 打开解密库（明文 SQLite）。不存在返回错误。
pub(crate) fn open_decrypted(client_key: &str) -> Result<Connection, String> {
    let path = reader_plain_path(client_key)?;
    Connection::open(&path).map_err(|e| format!("打开解密库失败: {e}"))
}

/// 「读取视图」的来源标记：快照与 WAL 都没变时，视图可以直接复用，
/// 不必每次刷新都复制一份 279 MB 的快照。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct ViewProvenance {
    snap_size: u64,
    snap_mtime_ns: u64,
    wal_size: u64,
    wal_mtime_ns: u64,
    frames: usize,
}

fn view_meta_path(client_key: &str) -> PathBuf {
    store_dir()
        .join("trae")
        .join("decrypted")
        .join(format!("{client_key}.view.json"))
}

fn current_view_provenance(snap: &Path, wal: &Path, frames: usize) -> Option<ViewProvenance> {
    let sm = std::fs::metadata(snap).ok()?;
    let wm = std::fs::metadata(wal).ok()?;
    Some(ViewProvenance {
        snap_size: sm.len(),
        snap_mtime_ns: mtime_ns(&sm),
        wal_size: wm.len(),
        wal_mtime_ns: mtime_ns(&wm),
        frames,
    })
}

/// 供**读取**使用的明文路径。
///
/// 快照本体始终是「实时库的纯解密结果」，绝不就地合并 WAL（导入流程把它当差异比对基准）。
/// 若实时库的 WAL 里还有已提交帧（客户端在运行），就复制一份快照、在**副本**上合并 WAL，
/// 读到的数据仍然是实时的；快照与 WAL 都没变时直接复用上一次的副本。
pub fn reader_plain_path(client_key: &str) -> Result<PathBuf, String> {
    let snap = decrypted_db_path(client_key);
    if !snap.exists() {
        return Err(format!(
            "未找到解密数据库：{}\n请先在「Trae 记录」页运行「扫描密钥并解密」。",
            snap.display()
        ));
    }
    let Some(client) = get_client(client_key) else {
        return Ok(snap);
    };
    let db = crate::modules::trae_discover::database_path(client);
    let wal = PathBuf::from(format!("{}-wal", db.to_string_lossy()));
    let frames = crate::modules::trae_delete::wal_pending_frames(&wal);
    let view = decrypted_view_path(client_key);
    let meta_path = view_meta_path(client_key);
    if frames == 0 {
        // 没有待合并内容：直接用快照，顺手清掉旧副本
        let _ = std::fs::remove_file(&view);
        let _ = std::fs::remove_file(&meta_path);
        return Ok(snap);
    }
    let Some(key) = crate::modules::trae_memory_scan::load_saved_key(client_key) else {
        return Ok(snap);
    };
    let prov = current_view_provenance(&snap, &wal, frames);
    if view.exists() {
        let cached = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|t| serde_json::from_str::<ViewProvenance>(&t).ok());
        if cached.is_some() && cached == prov {
            return Ok(view);
        }
    }
    if std::fs::copy(&snap, &view).is_err() {
        return Ok(snap);
    }
    if crate::modules::trae_delete::merge_wal_into_plain(&view, &wal, &key).is_err() {
        // 合并失败就退回快照（略旧但可用），别让整个列表打不开
        let _ = std::fs::remove_file(&view);
        return Ok(snap);
    }
    if let Some(p) = prov {
        if let Ok(text) = serde_json::to_string(&p) {
            let _ = std::fs::write(&meta_path, text);
        }
    }
    Ok(view)
}

// ---------------------------------------------------------------------------
// 会话列表
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub created: String,
    pub updated: String,
    pub turns: i64,
    /// 消息总条数（chat_message 全部未删行）。列表「正文」列用它当体量指标 ——
    /// Trae 的正文存在 SQLite 里没有独立文件，WorkBuddy 那边的 MB 在这里没有对应物。
    pub messages: i64,
    /// 归属账号 uid（project.user_id；无归属为空串）。
    pub owner_uid: String,
    /// 归属账号显示名（昵称或 uid 尾号；无归属为「（无归属）」）。
    pub owner_label: String,
}

/// 列出全部会话（按最后活动时间倒序），带归属账号信息。
pub fn list_sessions(client_key: &str) -> Result<Vec<SessionInfo>, String> {
    let conn = open_decrypted(client_key)?;
    let mut stmt = conn
        .prepare(
            "SELECT s.session_id, s.session_title, s.created_at, s.updated_at, \
                    COALESCE(p.user_id, '') AS owner_uid \
             FROM chat_session s \
             LEFT JOIN project p ON s.project_id = p.project_id \
             ORDER BY ifnull(s.updated_at, s.created_at) DESC",
        )
        .map_err(|e| format!("会话列表查询失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, rusqlite::types::Value>(2)?,
                row.get::<_, rusqlite::types::Value>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| format!("会话列表查询失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("会话列表读取失败: {e}"))?;

    let turns_map: HashMap<String, i64> = conn
        .prepare(
            "SELECT session_id, count(*) FROM chat_message \
             WHERE message_role='user' AND ifnull(deleted_at,0)=0 GROUP BY session_id",
        )
        .map_err(|e| format!("轮数统计失败: {e}"))?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|e| format!("轮数统计失败: {e}"))?
        .collect::<Result<HashMap<_, _>, _>>()
        .map_err(|e| format!("轮数统计失败: {e}"))?;

    // 消息总条数：与轮数同一张表，只是不过滤 role，顺手一起聚合。
    let messages_map: HashMap<String, i64> = conn
        .prepare(
            "SELECT session_id, count(*) FROM chat_message \
             WHERE ifnull(deleted_at,0)=0 GROUP BY session_id",
        )
        .map_err(|e| format!("消息数统计失败: {e}"))?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|e| format!("消息数统计失败: {e}"))?
        .collect::<Result<HashMap<_, _>, _>>()
        .map_err(|e| format!("消息数统计失败: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|(id, title, created, updated, owner_uid)| {
            let owner_label = if owner_uid.is_empty() {
                "（无归属）".into()
            } else {
                crate::modules::trae_vault::account_label(client_key, &owner_uid)
            };
            SessionInfo {
                id: id.clone(),
                title: title.trim().to_string(),
                created: format_ts(&created),
                updated: format_ts(&updated),
                turns: turns_map.get(&id).copied().unwrap_or(0),
                messages: messages_map.get(&id).copied().unwrap_or(0),
                owner_uid,
                owner_label,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// 对话解析（字段语义与 trae_web.py 一致）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub role: String,
    pub text: String,
    /// 该轮出现过的工具名（去重，供记忆交接统计）。
    pub tools: Vec<String>,
}

/// `chat_message_general.content` → 用户输入文本（干净原文）。
fn general_text(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Array(arr)) => {
            let mut parts = Vec::new();
            for p in arr {
                if let Value::Object(m) = p {
                    let t = m
                        .get("text_content")
                        .and_then(|v| v.as_str())
                        .or_else(|| m.get("text").and_then(|v| v.as_str()));
                    if let Some(t) = t {
                        let t = t.trim();
                        if !t.is_empty() {
                            parts.push(t.to_string());
                        }
                    }
                }
            }
            parts.join("\n").trim().to_string()
        }
        _ => raw.trim().to_string(),
    }
}

/// `history_v2.messages` → assistant 过程文本（多段 text 按顺序拼接）。
pub(crate) fn assistant_text(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let Ok(data) = serde_json::from_str::<Value>(raw) else {
        return String::new();
    };
    let Some(raw_msgs) = data.get("raw_messages").and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for m in raw_msgs {
        if m.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        match m.get("content") {
            Some(Value::Array(arr)) => {
                for p in arr {
                    if p.get("type").and_then(|v| v.as_str()) == Some("text") {
                        if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                            let t = t.trim();
                            if !t.is_empty() {
                                parts.push(t.to_string());
                            }
                        }
                    }
                }
            }
            Some(Value::String(s)) => {
                let s = s.trim();
                if !s.is_empty() {
                    parts.push(s.to_string());
                }
            }
            _ => {}
        }
    }
    parts.join("\n\n").trim().to_string()
}

/// `chat_message_task.content` → plan_item.tool_call_info.params.summary（该轮最终完整回答）。
fn task_summary(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let Ok(data) = serde_json::from_str::<Value>(raw) else {
        return String::new();
    };
    let Some(msgs) = data.get("messages").and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for m in msgs {
        let s = m
            .get("plan_item")
            .and_then(|p| p.get("tool_call_info"))
            .and_then(|t| t.get("params"))
            .and_then(|p| p.get("summary"))
            .and_then(|v| v.as_str());
        if let Some(s) = s {
            let s = s.trim();
            if !s.is_empty() {
                parts.push(s.to_string());
            }
        }
    }
    parts.join("\n\n").trim().to_string()
}

/// 一次工具调用 → markdown 渲染块（input 全保，防超大）。
fn render_trae_tool(name: &str, params: &Value) -> String {
    let params = if params.is_null() { &json!({}) } else { params };

    let code_block = |s: &str, lang: &str| format!("```{lang}\n{s}\n```");

    let mut lines: Vec<String> = vec![format!("🔧 **[{name}]**")];
    let mut body: Vec<String> = Vec::new();
    let get = |k: &str| params.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "Write" => {
            body.push(format!("创建文件：`{}`", get("file_path")));
            body.push(code_block(get("content"), ""));
        }
        "Edit" => {
            body.push(format!("编辑文件：`{}`", get("file_path")));
            let old = get("old_string");
            let new = get("new_string");
            let content = get("content");
            if !old.is_empty() {
                body.push(format!("旧内容：\n{}", code_block(old, "")));
            }
            if !new.is_empty() {
                body.push(format!("新内容：\n{}", code_block(new, "")));
            }
            if !content.is_empty() {
                body.push(code_block(content, ""));
            }
        }
        "RunCommand" | "CheckCommandStatus" | "StopCommand" => {
            let cmd = get("command");
            if !cmd.is_empty() {
                body.push(code_block(cmd, "bash"));
            }
        }
        "Read" => body.push(format!("读取文件：`{}`", get("file_path"))),
        "Grep" => body.push(format!(
            "搜索 `{}`：{}",
            get("path"),
            get("pattern")
        )),
        "LS" => body.push(format!("列目录：`{}`", get("path"))),
        "Glob" => body.push(format!("匹配 `{}`：{}", get("path"), get("pattern"))),
        "DeleteFile" => body.push(format!("删除文件：{}", get("file_paths"))),
        "TodoWrite" => {
            if let Some(todos) = params.get("todos").and_then(|v| v.as_array()) {
                for t in todos {
                    if let Value::Object(m) = t {
                        body.push(format!(
                            "- [{}] {}",
                            m.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                            m.get("content").and_then(|v| v.as_str()).unwrap_or("")
                        ));
                    } else {
                        body.push(format!("- {t}"));
                    }
                }
            }
        }
        "WebSearch" => body.push(format!("搜索：{}", get("query"))),
        "AskUserQuestion" => {
            if let Some(qs) = params.get("questions").and_then(|v| v.as_array()) {
                for q in qs {
                    if let Value::Object(m) = q {
                        body.push(format!(
                            "提问：{}",
                            m.get("question").and_then(|v| v.as_str()).unwrap_or("")
                        ));
                    } else {
                        body.push(format!("提问：{q}"));
                    }
                }
            }
        }
        "finish" | "CompactFake" => return String::new(),
        _ => {
            let s = serde_json::to_string(params).unwrap_or_else(|_| format!("{params}"));
            let s = if s.len() > 1500 {
                format!("{} ...(截断)", truncate_utf8(&s, 1500))
            } else {
                s
            };
            body.push(code_block(&s, "json"));
        }
    }
    lines.extend(body);
    lines.join("\n").trim().to_string()
}

/// `chat_message_task.content` → (渲染块列表, 工具名列表)。
fn task_tool_blocks(raw: &str) -> (Vec<String>, Vec<String>) {
    let mut blocks = Vec::new();
    let mut names = Vec::new();
    let Ok(data) = serde_json::from_str::<Value>(raw) else {
        return (blocks, names);
    };
    let Some(msgs) = data.get("messages").and_then(|v| v.as_array()) else {
        return (blocks, names);
    };
    for m in msgs {
        let Some(pi) = m.get("plan_item") else {
            continue;
        };
        let Some(ti) = pi.get("tool_call_info") else {
            continue;
        };
        let Some(name) = ti.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name == "finish" || name == "CompactFake" {
            continue;
        }
        let params = ti.get("params").cloned().unwrap_or_else(|| json!({}));
        if name == "Edit"
            && params.get("old_string").and_then(|v| v.as_str()).unwrap_or("").is_empty()
            && params.get("new_string").and_then(|v| v.as_str()).unwrap_or("").is_empty()
            && params.get("content").and_then(|v| v.as_str()).unwrap_or("").is_empty()
        {
            continue;
        }
        let block = render_trae_tool(name, &params);
        if !block.is_empty() {
            blocks.push(block);
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    (blocks, names)
}

/// `server_history_info.messages` 增量流重组：以 user 为界分组 assistant 文本。
/// 返回 (分组文本, 每组工具名) 或 None。
fn server_stream_groups(conn: &Connection, sid: &str) -> Option<(Vec<Vec<String>>, Vec<Vec<String>>)> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut names: Vec<Vec<String>> = Vec::new();
    let mut stmt = conn
        .prepare(
            "SELECT messages FROM server_history_info WHERE conversation_id=? \
             ORDER BY created_at, rowid",
        )
        .ok()?;
    let rows: Vec<String> = stmt
        .query_map([sid], |row| row.get::<_, String>(0))
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if rows.is_empty() {
        return None;
    }
    for raw in rows {
        let Ok(data) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(raw_msgs) = data.get("raw_messages").and_then(|v| v.as_array()) else {
            continue;
        };
        for m in raw_msgs {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role == "user" {
                groups.push(Vec::new());
                names.push(Vec::new());
                continue;
            }
            if role != "assistant" {
                continue;
            }
            if groups.is_empty() {
                groups.push(Vec::new());
                names.push(Vec::new());
            }
            let mut texts: Vec<String> = Vec::new();
            match m.get("content") {
                Some(Value::Array(arr)) => {
                    for p in arr {
                        if p.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                                let t = t.trim();
                                if !t.is_empty() {
                                    texts.push(t.to_string());
                                }
                            }
                        }
                    }
                }
                Some(Value::String(s)) => {
                    let s = s.trim();
                    if !s.is_empty() {
                        texts.push(s.to_string());
                    }
                }
                _ => {}
            }
            if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let fc = tc.get("function_call").cloned().unwrap_or(Value::Null);
                    let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                    let args = fc.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                    let params: Value = serde_json::from_str(args)
                        .unwrap_or_else(|_| json!({ "raw": args.chars().take(500).collect::<String>() }));
                    let block = render_trae_tool(&name, &params);
                    if !block.is_empty() {
                        texts.push(block);
                        let g = names.last_mut().unwrap();
                        if !g.iter().any(|n| n == &name) {
                            g.push(name);
                        }
                    }
                }
            }
            let g = groups.last_mut().unwrap();
            for t in texts {
                if g.is_empty() || g.last().unwrap() != &t {
                    g.push(t);
                }
            }
        }
    }
    if groups.is_empty() {
        None
    } else {
        Some((groups, names))
    }
}

/// 取一个会话的完整对话（user / assistant 交替，含每轮工具名）。
pub fn fetch_conversation(conn: &Connection, sid: &str) -> Result<Vec<Turn>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT message_id, message_role FROM chat_message \
             WHERE session_id=? AND ifnull(deleted_at,0)=0 ORDER BY message_index",
        )
        .map_err(|e| format!("会话消息查询失败: {e}"))?;
    let rows: Vec<(String, String)> = stmt
        .query_map([sid], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("会话消息查询失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("会话消息读取失败: {e}"))?;

    let asst_mids: Vec<String> = rows
        .iter()
        .filter(|(_, role)| role == "assistant")
        .map(|(mid, _)| mid.clone())
        .collect();

    let mut summaries: Vec<String> = Vec::with_capacity(asst_mids.len());
    let mut blocks_list: Vec<Vec<String>> = Vec::with_capacity(asst_mids.len());
    let mut task_names: Vec<Vec<String>> = Vec::with_capacity(asst_mids.len());
    for mid in &asst_mids {
        let raw = conn
            .query_row(
                "SELECT content FROM chat_message_task WHERE message_id=?",
                [mid],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default();
        summaries.push(task_summary(&raw));
        let (blocks, names) = task_tool_blocks(&raw);
        blocks_list.push(blocks);
        task_names.push(names);
    }

    let groups = server_stream_groups(conn, sid);
    let asst_texts: Option<Vec<String>> = if let Some((g, _)) = &groups {
        if g.len() == asst_mids.len() {
            Some(g.iter().map(|x| x.join("\n\n")).collect())
        } else {
            None
        }
    } else {
        None
    };
    let group_names: Option<Vec<Vec<String>>> = groups.map(|(_, n)| n);

    let mut turns: Vec<Turn> = Vec::with_capacity(rows.len());
    let mut asst_i = 0usize;
    for (mid, role) in rows {
        if role == "user" {
            let raw = conn
                .query_row(
                    "SELECT content FROM chat_message_general WHERE message_id=?",
                    [&mid],
                    |row| row.get::<_, String>(0),
                )
                .unwrap_or_default();
            turns.push(Turn { role, text: general_text(&raw), tools: Vec::new() });
        } else {
            let i = asst_i;
            asst_i += 1;
            let text: String;
            let mut tools: Vec<String> = group_names
                .as_ref()
                .and_then(|n| n.get(i))
                .cloned()
                .unwrap_or_default();
            if let Some(texts) = &asst_texts {
                let mut pieces: Vec<String> = vec![texts[i].clone()];
                pieces.extend(blocks_list[i].iter().cloned().filter(|x| !x.is_empty()));
                let s = &summaries[i];
                if !s.is_empty() && !texts[i].contains(s.as_str()) {
                    pieces.push(s.clone());
                }
                text = pieces.join("\n\n");
            } else {
                let h_rows: Vec<String> = conn
                    .prepare(
                        "SELECT messages FROM history_v2 \
                         WHERE message_id=? AND ifnull(deleted_at,0)=0 ORDER BY id",
                    )
                    .map_err(|e| format!("history_v2 查询失败: {e}"))?
                    .query_map([&mid], |row| row.get::<_, String>(0))
                    .map_err(|e| format!("history_v2 查询失败: {e}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("history_v2 读取失败: {e}"))?;
                let joined: Vec<String> = h_rows
                    .iter()
                    .map(|r| assistant_text(r))
                    .filter(|x| !x.is_empty())
                    .collect();
                let mut pieces: Vec<String> = vec![joined.join("\n\n")];
                pieces.extend(blocks_list[i].iter().cloned().filter(|x| !x.is_empty()));
                let s = &summaries[i];
                if !s.is_empty() && !pieces.iter().any(|p| p == s) {
                    pieces.push(s.clone());
                }
                text = pieces.join("\n\n");
            }
            for n in &task_names[i] {
                if !tools.iter().any(|t| t == n) {
                    tools.push(n.clone());
                }
            }
            turns.push(Turn {
                role,
                text: text.trim().to_string(),
                tools,
            });
        }
    }
    Ok(turns)
}

// ---------------------------------------------------------------------------
// MD 生成与导出
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ExportMeta {
    pub title: String,
    pub turns: usize,
    pub messages: usize,
    pub empty_user: usize,
    pub empty_assistant: usize,
    pub chars: usize,
}

/// 生成对话记录 MD，返回 (md 文本, 统计)。
pub fn build_chat_md(client_key: &str, session_id: &str) -> Result<(String, ExportMeta), String> {
    if !is_session_id(session_id) {
        return Err("会话 ID 格式不正确，应为 20~24 位十六进制".into());
    }
    let conn = open_decrypted(client_key)?;
    let row = conn
        .query_row(
            "SELECT session_title, created_at, updated_at FROM chat_session WHERE session_id=?",
            [session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, rusqlite::types::Value>(1)?,
                    row.get::<_, rusqlite::types::Value>(2)?,
                ))
            },
        )
        .map_err(|_| format!("会话 {session_id} 不在当前数据源中"))?;
    let (title, created, updated) = row;
    let turns = fetch_conversation(&conn, session_id)?;

    let label = label_of(client_key);
    let display = if title.trim().is_empty() {
        session_id.to_string()
    } else {
        title.trim().to_string()
    };
    let user_turns = turns.iter().filter(|t| t.role == "user").count();

    let mut l: Vec<String> = Vec::new();
    l.push(format!("# {display}"));
    l.push(String::new());
    l.push(format!("> 来源: {label}  |  Session: `{session_id}`"));
    l.push(format!(
        "> 创建: {}  |  更新: {}  |  轮数: {user_turns}",
        format_ts(&created),
        format_ts(&updated)
    ));
    l.push(String::new());
    l.push("---".into());
    l.push(String::new());

    let mut empty_user = 0usize;
    let mut empty_asst = 0usize;
    for t in &turns {
        let role_name = if t.role == "user" { "user" } else { "assistant" };
        if t.role == "user" && t.text.is_empty() {
            empty_user += 1;
        }
        if t.role == "assistant" && t.text.is_empty() {
            empty_asst += 1;
        }
        l.push(format!("## {role_name}"));
        l.push(String::new());
        l.push(if t.text.is_empty() { "（无记录）".into() } else { t.text.clone() });
        l.push(String::new());
        l.push("---".into());
        l.push(String::new());
    }

    let meta = ExportMeta {
        title: display,
        turns: user_turns,
        messages: turns.len(),
        empty_user,
        empty_assistant: empty_asst,
        chars: turns.iter().map(|t| t.text.len()).sum(),
    };
    Ok((l.join("\n"), meta))
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportedFile {
    pub session_id: String,
    pub filename: String,
    pub path: String,
    pub size_kb: f64,
    pub stats: ExportMeta,
}

/// 导出单个会话为 MD 文件（导出目录内自动去重命名）。
pub fn export_session(client_key: &str, session_id: &str) -> Result<ExportedFile, String> {
    let (md, meta) = build_chat_md(client_key, session_id)?;
    let dir = export_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let base = format!(
        "{}_{}_{}.md",
        prefix_of(client_key),
        safe_filename(&meta.title),
        &session_id[..session_id.len().min(8)]
    );
    let mut path = dir.join(&base);
    let mut n = 1usize;
    while path.exists() {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        path = dir.join(format!("{stem}_{n}.md"));
        n += 1;
    }
    std::fs::write(&path, md).map_err(|e| format!("写导出文件失败: {e}"))?;
    let size_kb = std::fs::metadata(&path)
        .map(|m| m.len() as f64 / 1024.0)
        .unwrap_or(0.0);
    Ok(ExportedFile {
        session_id: session_id.to_string(),
        filename: path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        path: path.to_string_lossy().into_owned(),
        size_kb: (size_kb * 10.0).round() / 10.0,
        stats: meta,
    })
}

/// 会话信息（详情弹窗用）。
///
/// 除计数外还返回**逐回合正文**（`turns[]`），形态对齐 WorkBuddy 的 `WbSessionDetail`，
/// 让两端详情弹窗能共用同一套渲染。Trae 库里的消息是**扁平的 user/assistant 交错序列**，
/// 这里按「一个 user 起头、其后连续 assistant 归并」配对成回合；配不上的 assistant
/// 归到前一个回合（或单独成回合），与 MD 导出的并列顺序保持一致。
pub fn session_detail(client_key: &str, session_id: &str) -> Result<Value, String> {
    if !is_session_id(session_id) {
        return Err("会话 ID 格式不正确，应为 20~24 位十六进制".into());
    }
    let conn = open_decrypted(client_key)?;
    let row = conn
        .query_row(
            "SELECT s.session_title, s.created_at, s.updated_at, COALESCE(p.user_id, '') \
             FROM chat_session s LEFT JOIN project p ON s.project_id = p.project_id \
             WHERE s.session_id=?",
            [session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, rusqlite::types::Value>(1)?,
                    row.get::<_, rusqlite::types::Value>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|_| format!("会话 {session_id} 不在当前数据源中"))?;
    let (title, created, updated, owner_uid) = row;
    let turns = fetch_conversation(&conn, session_id)?;
    let title = if title.trim().is_empty() {
        session_id.to_string()
    } else {
        title.trim().to_string()
    };
    let owner_label = if owner_uid.is_empty() {
        "（无归属）".to_string()
    } else {
        crate::modules::trae_vault::account_label(client_key, &owner_uid)
    };
    let created_s = format_ts(&created);
    let updated_s = format_ts(&updated);

    let rounds = pair_rounds(&turns);
    Ok(json!({
        "session_id": session_id,
        "title": title,
        "source": label_of(client_key),
        "turns": turns.iter().filter(|t| t.role == "user").count(),
        "messages": turns.len(),
        "created": created_s,
        "updated": updated_s,
        // 归属账号（原列表列，本次挪进详情）
        "owner_uid": owner_uid,
        "owner_label": owner_label,
        // 逐回合正文（对齐 WorkBuddy 的 turns[] 形态）
        "rounds": rounds,
    }))
}

/// 把扁平的 user/assistant 消息序列配对成「提问 + 回答」回合。
///
/// 规则（与 MD 导出的并列顺序一致，不丢消息）：
/// - `user` 起一个新回合；
/// - `assistant` 追加到当前回合的回答里，多条用空行连接；
/// - 开头就是 `assistant`（没有前导 user）时单开一个只有回答的回合，不丢弃。
fn pair_rounds(turns: &[Turn]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut cur_user: Option<String> = None;
    let mut cur_asst: Vec<String> = Vec::new();
    let mut cur_tools: usize = 0;

    for t in turns {
        if t.role == "user" {
            // 遇到新的 user：先把上一回合收口
            flush_round(&mut out, &mut cur_user, &mut cur_asst, &mut cur_tools);
            cur_user = Some(t.text.clone());
        } else {
            if !t.text.is_empty() {
                cur_asst.push(t.text.clone());
            }
            cur_tools += t.tools.len();
        }
    }
    flush_round(&mut out, &mut cur_user, &mut cur_asst, &mut cur_tools);
    out
}

/// 把当前累积的回合推进 `out`；尚未开始任何内容时是空操作。
fn flush_round(
    out: &mut Vec<Value>,
    user: &mut Option<String>,
    asst: &mut Vec<String>,
    tools: &mut usize,
) {
    if user.is_none() && asst.is_empty() {
        return;
    }
    out.push(json!({
        "userText": user.take().unwrap_or_default(),
        "assistantText": asst.join("\n\n"),
        "toolCalls": *tools,
    }));
    asst.clear();
    *tools = 0;
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportAllReport {
    pub path: String,
    pub filename: String,
    pub ok: usize,
    pub failed: Vec<String>,
    pub total: usize,
}

/// 一键导出全部会话为 zip（可跨数据源）。
pub fn export_all(sources: &[String]) -> Result<ExportAllReport, String> {
    if sources.is_empty() {
        return Err("未指定数据源".into());
    }
    let dir = export_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let tag = sources
        .iter()
        .map(|s| if s == "trae-cn" { "traecn" } else { "solo" })
        .collect::<Vec<_>>()
        .join("_");
    let fname = format!("trae_chats_{tag}_{stamp}.zip");
    let path = dir.join(&fname);

    let file = std::fs::File::create(&path).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zw = zip::ZipWriter::new(file);
    let opts = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut ok = 0usize;
    let mut total = 0usize;
    let mut failed: Vec<String> = Vec::new();
    let mut any_found = false;

    for src in sources {
        let sessions = list_sessions(src);
        match sessions {
            Err(e) => failed.push(format!("{}: {e}", label_of(src))),
            Ok(list) => {
                if !list.is_empty() {
                    any_found = true;
                }
                total += list.len();
                for s in &list {
                    match build_chat_md(src, &s.id) {
                        Ok((md, meta)) => {
                            let name = format!(
                                "{}_{}_{}.md",
                                prefix_of(src),
                                safe_filename(&meta.title),
                                &s.id[..s.id.len().min(8)]
                            );
                            let _ = zw.start_file(name, opts);
                            let _ = zw.write_all(md.as_bytes());
                            ok += 1;
                        }
                        Err(e) => failed.push(format!("{}: {e}", s.id)),
                    }
                }
            }
        }
    }

    if !any_found {
        let _ = zw.finish();
        let _ = std::fs::remove_file(&path);
        let msg = if failed.is_empty() {
            "未找到任何会话".to_string()
        } else {
            format!("未找到任何会话；{}", failed.join("；"))
        };
        return Err(msg);
    }

    if !failed.is_empty() {
        let note = format!(
            "以下会话导出失败（数据异常，不影响其余文件）：\n\n{}",
            failed.join("\n")
        );
        let _ = zw.start_file("_导出失败清单.txt", opts);
        let _ = zw.write_all(note.as_bytes());
    }

    zw.finish().map_err(|e| format!("完成 zip 失败: {e}"))?;
    Ok(ExportAllReport {
        path: path.to_string_lossy().into_owned(),
        filename: fname,
        ok,
        failed,
        total,
    })
}

/// 便捷：删除文件（供前端清理过期 zip 等）。
pub fn remove_file_quiet(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(role: &str, text: &str, tools: &[&str]) -> Turn {
        Turn {
            role: role.to_string(),
            text: text.to_string(),
            tools: tools.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// 正常的 user/assistant 交错：逐条配对成回合。
    #[test]
    fn pair_rounds_matches_user_with_following_assistant() {
        let turns = vec![
            turn("user", "问题一", &[]),
            turn("assistant", "回答一", &["Read"]),
            turn("user", "问题二", &[]),
            turn("assistant", "回答二", &[]),
        ];
        let r = pair_rounds(&turns);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["userText"], "问题一");
        assert_eq!(r[0]["assistantText"], "回答一");
        assert_eq!(r[0]["toolCalls"], 1);
        assert_eq!(r[1]["userText"], "问题二");
        assert_eq!(r[1]["assistantText"], "回答二");
    }

    /// 同一个 user 后跟多条 assistant（工具调用拆成多段）时，回答要合并而不是丢掉。
    #[test]
    fn pair_rounds_merges_multiple_assistant_messages_into_one_round() {
        let turns = vec![
            turn("user", "问", &[]),
            turn("assistant", "第一段", &["Bash"]),
            turn("assistant", "第二段", &["Read", "Write"]),
        ];
        let r = pair_rounds(&turns);
        assert_eq!(r.len(), 1, "多条 assistant 应归并进同一回合");
        assert_eq!(r[0]["assistantText"], "第一段\n\n第二段");
        assert_eq!(r[0]["toolCalls"], 3, "工具调用数应累加");
    }

    /// 开头就是 assistant（没有前导 user）不能丢，要单开一个只有回答的回合。
    #[test]
    fn pair_rounds_keeps_leading_assistant_without_user() {
        let turns = vec![turn("assistant", "开场白", &[]), turn("user", "问", &[])];
        let r = pair_rounds(&turns);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["userText"], "");
        assert_eq!(r[0]["assistantText"], "开场白");
        assert_eq!(r[1]["userText"], "问");
        assert_eq!(r[1]["assistantText"], "", "该回合没有回答，答案为空串而不是被省略");
    }

    /// 结尾悬空的 user（有问无答）也要保留，否则列表里会少一轮。
    #[test]
    fn pair_rounds_keeps_trailing_user_without_assistant() {
        let turns = vec![turn("user", "有问无答", &[])];
        let r = pair_rounds(&turns);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["userText"], "有问无答");
        assert_eq!(r[0]["assistantText"], "");
    }

    /// 空序列不产生任何回合（也不能因为 flush 判空逻辑漏判而多出一个空回合）。
    #[test]
    fn pair_rounds_of_empty_input_is_empty() {
        assert!(pair_rounds(&[]).is_empty());
    }

    /// 角色大小写/其余取值一律当 assistant 处理（库里的 role 不只是 user/assistant）。
    #[test]
    fn pair_rounds_treats_non_user_roles_as_assistant() {
        let turns = vec![turn("user", "问", &[]), turn("system", "系统提示", &[])];
        let r = pair_rounds(&turns);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["assistantText"], "系统提示");
    }

    #[test]
    fn backup_group_key_parses_all_three_naming_schemes() {
        // trae/backup 的整库备份（含 -wal / -shm 兄弟文件）
        assert_eq!(
            backup_owner_and_stamp("solo-cn_before_delete_20261004_183502.db.bak"),
            ("solo-cn".to_string(), "20261004_183502".to_string())
        );
        assert_eq!(
            backup_owner_and_stamp("solo-cn_before_delete_20261004_183502-wal.db.bak"),
            ("solo-cn".to_string(), "20261004_183502".to_string())
        );
        // trae/import_backup 与 workbuddy_import_backup 的子目录
        assert_eq!(
            backup_owner_and_stamp("solo-cn-20261003-200952"),
            ("solo-cn".to_string(), "20261003-200952".to_string())
        );
        assert_eq!(
            backup_owner_and_stamp("trae-cn-20261004-194801"),
            ("trae-cn".to_string(), "20261004-194801".to_string())
        );
    }

    /// 关键行为：**每个客户端各自保留最新 `keep` 批**，且同批的 `-wal`/`-shm`
    /// 兄弟文件必须整组删除（只删主库会留下无主的 wal）。
    #[test]
    fn prune_keeps_newest_per_client_and_removes_sibling_files() {
        let root = std::env::temp_dir().join(format!("wb_prune_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // solo-cn 三批（每批 3 个文件），trae-cn 两批
        let mk = |name: &str, bytes: usize| {
            let p = root.join(name);
            std::fs::write(&p, vec![0u8; bytes]).unwrap();
        };
        for stamp in ["20261001_100000", "20261002_100000", "20261003_100000"] {
            mk(&format!("solo-cn_before_delete_{stamp}.db.bak"), 100);
            mk(&format!("solo-cn_before_delete_{stamp}-wal.db.bak"), 10);
            mk(&format!("solo-cn_before_delete_{stamp}-shm.db.bak"), 5);
        }
        for stamp in ["20261001_090000", "20261002_090000"] {
            mk(&format!("trae-cn_before_delete_{stamp}.db.bak"), 50);
        }

        let (removed, freed) = prune_backups(&root, 2);
        // solo-cn: 删最旧 1 批 = 3 个文件；trae-cn: 2 批 ≤ keep，不动
        assert_eq!(removed, 3);
        assert_eq!(freed, 115);

        let left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !left.iter().any(|n| n.contains("20261001_100000")),
            "最旧的 solo-cn 批次（含 wal/shm 兄弟文件）应被整组删除，实际残留：{left:?}"
        );
        assert_eq!(left.len(), 8, "应剩 solo-cn 2 批 × 3 文件 + trae-cn 2 批 × 1 文件");

        // keep 大于批次数时不动任何东西
        let (removed2, freed2) = prune_backups(&root, 99);
        assert_eq!((removed2, freed2), (0, 0));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_recurses_into_backup_subdirs_and_counts_bytes() {
        let root = std::env::temp_dir().join(format!("wb_prune_dir_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        for (name, n) in [("solo-cn-20261001-100000", 3), ("solo-cn-20261002-100000", 3), ("solo-cn-20261003-100000", 3)] {
            let d = root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("database.db"), vec![0u8; n]).unwrap();
        }

        let (removed, freed) = prune_backups(&root, 2);
        assert_eq!(removed, 1, "应删掉最旧的 1 个子目录");
        assert_eq!(freed, 3, "回收字节数应包含子目录内的文件");
        assert!(!root.join("solo-cn-20261001-100000").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_on_missing_dir_is_noop() {
        let root = std::env::temp_dir().join("wb_prune_missing_dir");
        assert_eq!(prune_backups(&root, 1), (0, 0));
    }
}
