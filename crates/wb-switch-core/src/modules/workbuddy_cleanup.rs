//! WorkBuddy 本地会话垃圾清理（**先扫描、后受控清除**）。
//!
//! 用户看到的现象：导入列表里有一批「无正文」的会话根本选不中，另有一批「已删除」
//! 的会话藏在开关后面；它们都是 `sessions` 表里的**残留行**，占着位置、混在列表里，
//! 也占着磁盘。本模块把这些残留分类列出，并在用户逐项确认后清掉。
//!
//! ## 五类垃圾
//!
//! | 分类 | 判定 | 处置 |
//! | --- | --- | --- |
//! | `orphan_meta` | `sessions` 有行、磁盘**没有**正文 JSONL（且未标记删除） | 删行（含 `session_usage` / 云端映射） |
//! | `deleted` | `deleted_at` 非空（WorkBuddy 里已删除，本地仍留行与文件） | 删行 + 正文与附属文件移入回收站 |
//! | `orphan_body` | 磁盘有正文 JSONL、`sessions` 里**没有**对应行 | 文件移入回收站 |
//! | `stale_artifacts` | `changes-detail/<id>`、`file-history/<id>`、`file-tree-manifests/<id>.json` 指向已不存在的会话 | 目录/文件移入回收站 |
//! | `stale_workspace` | `workspace/sessions/<id>` 指向已不存在的会话 | 目录移入回收站 |
//! | `logs_old` | `logs/<日期>/`、`logs/sandbox/<日期>/` 等诊断日志 | 目录移入回收站（客户端会重建） |
//! | `traces_old` | `traces/<id>/` 性能追踪缓存 | 目录移入回收站 |
//!
//! 「过期」的判定：目录 mtime 早于 [`LOG_KEEP_DAYS`] 天。早于该天数的项标记
//! `recommended: true`（前端默认勾选），更新的项也列出来但默认不勾——删除日志本身
//! 不影响任何会话数据，只是默认保守。
//!
//! ## 安全约束（逐条对应实现）
//!
//! 1. **扫描是只读的**：`scan()` 只用只读连接 + 目录遍历，绝不写任何东西。
//! 2. **清除前先退出 WorkBuddy**：`workbuddy.db` 与 `edge-sync-mapping-v4.db` 都在运行中
//!    被客户端持有，不清空进程就改库会被客户端用内存快照覆盖（与导入/导出同一条理由）。
//!    退出走 [`wb_quit`]：**反复重试**到进程真的消失，而不是「杀一次 + 死等 20 秒」——
//!    后者在客户端自我升级（更新器把新版拉起来）时会误报失败，且分不清
//!    「杀不掉」和「被杀掉后又被重启」（见 `process_list::kill_tree_and_wait`）。
//! 3. **改库前整份备份**：`workbuddy.db` + WAL/SHM、`edge-sync-*.db` + WAL/SHM 全部复制到
//!    `<工具目录>/workbuddy_cleanup_backup/<时间戳>/`。
//! 4. **文件先移入回收站、不直接删**：默认把文件/目录搬到
//!    `<工具目录>/workbuddy_cleanup_trash/<时间戳>/<原相对路径>`，可原样搬回；
//!    只有显式勾选「彻底删除」才真正 unlink。
//! 5. **同步映射一起清**：`edge_sync_mapping` 是按 `session_id` 建的行，只删 `sessions`
//!    会留下孤儿映射，WorkBuddy 下次同步可能把会话又拉回来——所以一并删掉。

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::modules::config::{self, now_ms, store_dir};
use crate::modules::workbuddy_accounts;
use crate::modules::workbuddy_export::{
    open_wb_db_ro, resolve_wb_exe, wb_is_running, wb_launch, wb_quit,
};
use crate::modules::workbuddy_source;

/// WorkBuddy 数据根。
fn data_root() -> PathBuf {
    workbuddy_source::data_root()
}

fn db_path() -> PathBuf {
    data_root().join("workbuddy.db")
}

/// 回收站根目录。
fn trash_root() -> PathBuf {
    store_dir().join("workbuddy_cleanup_trash")
}

/// 备份根目录。
fn backup_root() -> PathBuf {
    store_dir().join("workbuddy_cleanup_backup")
}

/// 形如 `20261004-221500` 的时间戳目录名。
fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// 日志 / 追踪缓存的保留天数：早于此天数的项标记为「推荐清理」，前端默认勾选。
const LOG_KEEP_DAYS: i64 = 3;

/// 目录（或文件）的 mtime（毫秒）。
fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 是否早于保留窗口。
fn is_stale(mtime: i64) -> bool {
    mtime > 0 && now_ms() - mtime > LOG_KEEP_DAYS * 86_400_000
}

fn dir_size(path: &Path) -> u64 {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if meta.is_file() {
        return meta.len();
    }
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            total += dir_size(&e.path());
        }
    }
    total
}

fn is_uuid_like(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

/// `projects/` 下的全部正文：`会话 id → 路径`。
fn index_bodies() -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    let projects = data_root().join("projects");
    let mut stack = vec![projects];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.insert(stem.to_string(), p);
                }
            }
        }
    }
    out
}

/// `~/.workbuddy` 下的相对路径（用于回收站内保持原目录结构）。
fn rel_to_root(p: &Path) -> String {
    p.strip_prefix(data_root())
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

/// 一条待清理项。
struct Item {
    id: String,
    kind: &'static str,
    category: &'static str,
    title: String,
    uid: String,
    updated_at: i64,
    bytes: u64,
    detail: String,
    /// 将被删除的库行所属的会话 id（`kind == "session"` 时有值）。
    session_id: Option<String>,
    /// 将被移入回收站的文件 / 目录。
    paths: Vec<PathBuf>,
    /// 是否建议清理（前端默认勾选）。会话类残留恒为 true；日志类按保留窗口判定。
    recommended: bool,
}

impl Item {
    fn to_json(&self) -> Value {
        let owner = if self.uid.is_empty() {
            String::new()
        } else {
            workbuddy_accounts::label_for(&self.uid)
        };
        json!({
            "id": self.id,
            "kind": self.kind,
            "category": self.category,
            "title": self.title,
            "uid": self.uid,
            "owner": owner,
            "updated_at": self.updated_at,
            "bytes": self.bytes,
            "detail": self.detail,
            "recommended": self.recommended,
            "paths": self.paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
        })
    }
}

/// 会话在磁盘上的全部附属物（除正文外）。
fn artifacts_of(sid: &str) -> Vec<PathBuf> {
    let root = data_root();
    let mut out = Vec::new();
    for dir in ["changes-detail", "file-history"] {
        let p = root.join(dir).join(sid);
        if p.exists() {
            out.push(p);
        }
    }
    let m = root.join("file-tree-manifests").join(format!("{sid}.json"));
    if m.is_file() {
        out.push(m);
    }
    out
}

/// 会话的工作区快照目录。
fn workspace_dir_of(sid: &str) -> Option<PathBuf> {
    let p = data_root().join("workspace").join("sessions").join(sid);
    p.is_dir().then_some(p)
}

// ---------------------------------------------------------------------------
// 扫描（只读）
// ---------------------------------------------------------------------------

/// 扫描缓存文件名（落在 `config::cache_dir()`）。
const CACHE_NAME: &str = "wb-cleanup.json";
/// 缓存结构版本，字段一改就 +1。
const CACHE_VERSION: u64 = 1;
/// 「这份缓存算旧了」的阈值：10 分钟（理由与 Trae 侧一致）。
///
/// **它不再是「过期就丢」的闸门**（v0.0.23 改）：超时只打 `stale = true` 标，
/// 不触发自动重扫 —— 否则每次点进清理页都要重走一遍 5 秒多的递归统计。
const CACHE_TTL_MS: i64 = 10 * 60 * 1000;

/// 带缓存的扫描：**有缓存就用缓存**（毫秒级），只有根本没有缓存时才扫。
///
/// `force = true` 跳过缓存（前端「重新扫描」按钮）。
/// 返回值在原结构上多四个字段：`cached` / `scannedAt` / `ageMs` / `stale`。
pub fn cached(force: bool) -> Result<Value, String> {
    if !force {
        if let Some((mut payload, age, stale)) =
            config::read_cache_slot_stale_ok(CACHE_NAME, CACHE_VERSION, CACHE_TTL_MS)
        {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("cached".into(), json!(true));
                obj.insert("ageMs".into(), json!(age));
                obj.insert("stale".into(), json!(stale));
            }
            return Ok(payload);
        }
    }
    let mut out = scan()?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("cached".into(), json!(false));
        obj.insert("stale".into(), json!(false));
        obj.insert("scannedAt".into(), json!(now_ms()));
        obj.insert("ageMs".into(), json!(0));
    }
    config::write_cache_slot(CACHE_NAME, CACHE_VERSION, &out);
    Ok(out)
}

/// 扫描本机 WorkBuddy 的全部可清理项；**不改动任何文件与数据库**。
pub fn scan() -> Result<Value, String> {
    let root = data_root();
    if !workbuddy_source::is_available() {
        return Ok(json!({
            "available": false,
            "data_root": root.to_string_lossy(),
            "categories": [],
            "totals": { "count": 0, "bytes": 0 },
            "trash": trash_info_json(),
        }));
    }

    // 会话表（只读）
    let mut rows: Vec<(String, String, String, i64, Option<i64>, String)> = Vec::new(); // id, title, uid, updated, deleted, cwd
    if let Ok(conn) = open_wb_db_ro() {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, COALESCE(NULLIF(custom_title,''), NULLIF(title,''), '(无标题)'), \
             COALESCE(user_id,''), COALESCE(updated_at,0), deleted_at, COALESCE(cwd,'') FROM sessions",
        ) {
            if let Ok(it) = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, String>(5)?,
                ))
            }) {
                rows = it.flatten().collect();
            }
        }
    } else {
        return Err(format!(
            "未找到 WorkBuddy 会话库：{}",
            db_path().display()
        ));
    }

    let bodies = index_bodies();
    let known: BTreeSet<String> = rows.iter().map(|r| r.0.clone()).collect();

    let mut items: Vec<Item> = Vec::new();

    // B) 已删除的会话
    for (id, title, uid, updated, deleted, _cwd) in &rows {
        if deleted.is_none() {
            continue;
        }
        let body = bodies.get(id).cloned();
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Some(b) = &body {
            paths.push(b.clone());
        }
        paths.extend(artifacts_of(id));
        if let Some(w) = workspace_dir_of(id) {
            paths.push(w);
        }
        let bytes = paths.iter().map(|p| dir_size(p)).sum();
        items.push(Item {
            id: format!("session:{id}"),
            kind: "session",
            category: "deleted",
            title: title.clone(),
            uid: uid.clone(),
            updated_at: *updated,
            bytes,
            detail: if body.is_some() {
                format!(
                    "已在 WorkBuddy 删除（{}），本地仍保留记录行与正文",
                    fmt_time(*deleted)
                )
            } else {
                format!(
                    "已在 WorkBuddy 删除（{}），只剩记录行",
                    fmt_time(*deleted)
                )
            },
            recommended: true,
            session_id: Some(id.clone()),
            paths,
        });
    }

    // A) 孤儿元数据：有行、无正文、且未标记删除
    for (id, title, uid, updated, deleted, _cwd) in &rows {
        if deleted.is_some() || bodies.contains_key(id) {
            continue;
        }
        let bytes = artifacts_of(id).iter().map(|p| dir_size(p)).sum();
        items.push(Item {
            id: format!("session:{id}"),
            kind: "session",
            category: "orphan_meta",
            title: title.clone(),
            uid: uid.clone(),
            updated_at: *updated,
            bytes,
            detail: "只有记录行、磁盘上没有正文（在 WorkBuddy 里点开也打不开），导入时无法选中".to_string(),
            recommended: true,
            session_id: Some(id.clone()),
            paths: artifacts_of(id),
        });
    }

    // C) 孤儿正文：磁盘有 JSONL、表里没有行
    for (sid, path) in &bodies {
        if known.contains(sid) {
            continue;
        }
        items.push(Item {
            id: format!("file:{}", rel_to_root(path)),
            kind: "file",
            category: "orphan_body",
            title: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            uid: String::new(),
            updated_at: 0,
            bytes: dir_size(path),
            detail: "有正文文件、会话表里没有对应记录（列表里看不到，纯占空间）".to_string(),
            recommended: true,
            session_id: None,
            paths: vec![path.clone()],
        });
    }

    // D) 陈旧附属目录（指向已不存在的会话）
    for dir in ["changes-detail", "file-history"] {
        let base = root.join(dir);
        let Ok(rd) = std::fs::read_dir(&base) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // 目录名不是会话 id 的一律不碰（可能有别的用途）
            if !is_uuid_like(name) || known.contains(name) {
                continue;
            }
            items.push(Item {
                id: format!("dir:{}", rel_to_root(&p)),
                kind: "dir",
                category: "stale_artifacts",
                title: name.to_string(),
                uid: String::new(),
                updated_at: 0,
                bytes: dir_size(&p),
                detail: format!("{dir}/ 下属于已不存在会话的残留目录"),
                recommended: true,
                session_id: None,
                paths: vec![p],
            });
        }
    }
    {
        let base = root.join("file-tree-manifests");
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !is_uuid_like(stem) || known.contains(stem) {
                    continue;
                }
                items.push(Item {
                    id: format!("file:{}", rel_to_root(&p)),
                    kind: "file",
                    category: "stale_artifacts",
                    title: stem.to_string(),
                    uid: String::new(),
                    updated_at: 0,
                    bytes: dir_size(&p),
                    detail: "file-tree-manifests/ 下属于已不存在会话的残留清单".to_string(),
                    recommended: true,
                    session_id: None,
                    paths: vec![p],
                });
            }
        }
    }

    // E) 陈旧会话工作区快照
    {
        let base = root.join("workspace").join("sessions");
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                if !p.is_dir() {
                    continue;
                }
                let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                // editor-sdk-* 与当前会话在用的工作区不动
                if !is_uuid_like(name) || known.contains(name) {
                    continue;
                }
                items.push(Item {
                    id: format!("dir:{}", rel_to_root(&p)),
                    kind: "dir",
                    category: "stale_workspace",
                    title: name.to_string(),
                    uid: String::new(),
                    updated_at: 0,
                    bytes: dir_size(&p),
                    detail: "workspace/sessions/ 下属于已不存在会话的工作区快照".to_string(),
                    recommended: true,
                    session_id: None,
                    paths: vec![p],
                });
            }
        }
    }

    // F) 诊断日志与性能追踪缓存（客户端会自行重建，删除不影响会话数据）
    let mut push_cache_item = |dir: &'static str, category: &'static str, detail: &'static str| {
        let base = root.join(dir);
        let Ok(rd) = std::fs::read_dir(&base) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // 只认「按日期/编号命名」的子目录，避免误伤未知用途的目录
            let dated = name.len() == 10
                && name.as_bytes()[4] == b'-'
                && name[..4].chars().all(|c| c.is_ascii_digit());
            let stamped = name.len() == 8 && name.chars().all(|c| c.is_ascii_digit());
            let numbered = name.chars().all(|c| c.is_ascii_digit());
            if !(dated || stamped || numbered) {
                continue;
            }
            let bytes = dir_size(&p);
            if bytes == 0 {
                continue;
            }
            let stale = is_stale(mtime_ms(&p));
            items.push(Item {
                id: format!("dir:{}", rel_to_root(&p)),
                kind: "dir",
                category,
                title: name.to_string(),
                uid: String::new(),
                updated_at: mtime_ms(&p),
                bytes,
                detail: detail.to_string(),
                session_id: None,
                paths: vec![p],
                recommended: stale,
            });
        }
    };
    push_cache_item(
        "traces",
        "traces_old",
        "性能追踪缓存目录（诊断用，客户端会重建）",
    );
    push_cache_item(
        "logs",
        "logs_old",
        "按日期滚动的运行日志（客户端会重建；不影响会话数据）",
    );
    // 沙箱日志与崩溃报告：整体作为一项，避免刷屏
    for (path, label, detail) in [
        (
            root.join("logs").join("sandbox"),
            "logs/sandbox",
            "沙箱运行日志（客户端会重建）",
        ),
        (
            root.join("logs").join("Crash-Log"),
            "logs/Crash-Log",
            "崩溃报告（仅在排查闪退时需要）",
        ),
    ] {
        if !path.is_dir() {
            continue;
        }
        let bytes = dir_size(&path);
        if bytes == 0 {
            continue;
        }
        items.push(Item {
            id: format!("dir:{}", rel_to_root(&path)),
            kind: "dir",
            category: "logs_old",
            title: label.to_string(),
            uid: String::new(),
            updated_at: mtime_ms(&path),
            bytes,
            detail: detail.to_string(),
            session_id: None,
            paths: vec![path],
            recommended: false,
        });
    }

    items.sort_by(|a, b| {
        a.category
            .cmp(b.category)
            .then_with(|| b.bytes.cmp(&a.bytes))
    });

    let categories: Vec<Value> = [
        ("deleted", "已删除的会话", "在 WorkBuddy 里删过、本地仍留着记录行与文件；清理后不可在 WorkBuddy 里恢复（但回收站与备份都在）"),
        ("orphan_meta", "孤儿记录（无正文）", "只有记录行、没有正文，导入列表里显示「无正文」选不中；删掉不影响任何可读会话"),
        ("orphan_body", "孤儿正文", "有正文文件、会话表里没有记录，列表里根本看不到"),
        ("stale_artifacts", "陈旧附属文件", "改动详情 / 文件历史 / 文件树清单里属于已不存在会话的残留"),
        ("stale_workspace", "陈旧工作区快照", "会话代理的工作区快照目录，对应会话已不存在"),
        ("logs_old", "诊断日志", "按日期滚动的运行日志与崩溃报告；删掉客户端会重建，不影响任何会话数据"),
        ("traces_old", "性能追踪缓存", "排障用的性能追踪目录；删掉客户端会重建"),
    ]
    .iter()
    .map(|(key, title, desc)| {
        let group: Vec<&Item> = items.iter().filter(|i| i.category == *key).collect();
        json!({
            "key": key,
            "title": title,
            "desc": desc,
            "count": group.len(),
            "bytes": group.iter().map(|i| i.bytes).sum::<u64>(),
            "items": group.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
        })
    })
    .collect();

    Ok(json!({
        "available": true,
        "data_root": root.to_string_lossy(),
        "db_path": db_path().to_string_lossy(),
        "running": wb_is_running(),
        "exe": resolve_wb_exe().map(|p| p.to_string_lossy().into_owned()),
        "generated_at": now_ms(),
        "log_keep_days": LOG_KEEP_DAYS,
        "categories": categories,
        "totals": {
            "count": items.len(),
            "bytes": items.iter().map(|i| i.bytes).sum::<u64>(),
            "recommended_count": items.iter().filter(|i| i.recommended).count(),
            "recommended_bytes": items.iter().filter(|i| i.recommended).map(|i| i.bytes).sum::<u64>(),
            "sessions": rows.len(),
        },
        // 只读的「占用大头」清单：解释为什么 `~/.workbuddy` 这么大，并标明哪些**不建议**清
        "large_holdings": large_holdings(),
        "trash": trash_info_json(),
    }))
}

/// `~/.workbuddy` 下几个大头的占用（只读；用于说明「为什么这么占地方」）。
fn large_holdings() -> Vec<Value> {
    let root = data_root();
    let mut out: Vec<Value> = Vec::new();
    for (rel, cleanable, note) in [
        ("workspace", false, "会话代理的工作区快照（含当前会话的改动备份，勿清）"),
        ("binaries", false, "内置运行时（便携版 git / python / node）"),
        ("plugins", false, "插件与市场缓存"),
        ("logs", true, "运行日志，可在「诊断日志」分组里清理"),
        ("traces", true, "性能追踪，可在「性能追踪缓存」分组里清理"),
        ("projects", false, "会话正文（就是会话本身）"),
    ] {
        let p = root.join(rel);
        if !p.exists() {
            continue;
        }
        out.push(json!({
            "name": rel,
            "path": p.to_string_lossy(),
            "bytes": dir_size(&p),
            "cleanable": cleanable,
            "note": note,
        }));
    }
    out.sort_by(|a, b| {
        b.get("bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .cmp(&a.get("bytes").and_then(Value::as_u64).unwrap_or(0))
    });
    out
}

fn fmt_time(ms: Option<i64>) -> String {
    match ms {
        Some(ms) if ms > 0 => chrono::DateTime::from_timestamp_millis(ms)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "—".to_string()),
        _ => "—".to_string(),
    }
}

/// 回收站现状（只读）。
fn trash_info_json() -> Value {
    let root = trash_root();
    let mut count = 0usize;
    let mut bytes = 0u64;
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            count += 1;
            bytes += dir_size(&e.path());
        }
    }
    json!({ "dir": root.to_string_lossy(), "count": count, "bytes": bytes })
}

// ---------------------------------------------------------------------------
// 清除（受控写入）
// ---------------------------------------------------------------------------

/// 把一批待清理项搬进回收站：保持 `~/.workbuddy` 下的相对路径结构。
fn move_to_trash(paths: &[PathBuf], stamp: &str, failed: &mut Vec<String>) {
    for p in paths {
        if !p.exists() {
            continue;
        }
        let rel = rel_to_root(p);
        let dst = trash_root().join(stamp).join(&rel);
        if let Some(parent) = dst.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                failed.push(format!("{}（无法创建回收站目录）", p.display()));
                continue;
            }
        }
        if std::fs::rename(p, &dst).is_ok() {
            continue;
        }
        // 跨盘 / 占用：退化成复制 + 删除
        let copied = if p.is_dir() {
            copy_dir(p, &dst).is_ok()
        } else {
            std::fs::copy(p, &dst).is_ok()
        };
        if !copied {
            failed.push(format!("{}（移动失败）", p.display()));
            continue;
        }
        let removed = if p.is_dir() {
            std::fs::remove_dir_all(p).is_ok()
        } else {
            std::fs::remove_file(p).is_ok()
        };
        if !removed {
            // 复制成功但原文件删不掉：回收站里已有副本，原文件保留（更安全）
            failed.push(format!("{}（已复制到回收站，但原文件删除失败）", p.display()));
        }
    }
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let from = e.path();
        let to = dst.join(e.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 备份数据库（含 WAL/SHM；存在才复制）。
fn backup_databases(into: &Path) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(into).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let root = data_root();
    let mut names: Vec<String> = vec!["workbuddy.db".into(), "workbuddy.db-wal".into(), "workbuddy.db-shm".into()];
    // 云端同步映射库带版本号后缀，按前缀扫出来
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("edge-sync-mapping-v") && n.ends_with(".db") {
                for suffix in ["", "-wal", "-shm"] {
                    names.push(format!("{n}{suffix}"));
                }
            }
        }
    }
    let mut done = Vec::new();
    for n in names {
        let src = root.join(&n);
        if src.is_file() {
            std::fs::copy(&src, into.join(&n)).map_err(|e| format!("备份 {n} 失败: {e}"))?;
            done.push(n);
        }
    }
    Ok(done)
}

/// 打开一个可写库并清掉这些会话 id 的行；返回删除的行数。
fn purge_db_rows(path: &Path, sids: &BTreeSet<String>, table: &str, column: &str) -> Result<usize, String> {
    if sids.is_empty() || !path.is_file() {
        return Ok(0);
    }
    let conn = Connection::open(path).map_err(|e| format!("打开 {} 失败: {e}", path.display()))?;
    // 有些表在老版本里不存在 —— 不是错误，跳过即可
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(0);
    }
    let mut removed = 0usize;
    {
        let sql = format!("DELETE FROM {table} WHERE {column} = ?1");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("准备删除 {table} 失败: {e}"))?;
        for sid in sids {
            removed += stmt.execute([sid]).unwrap_or(0);
        }
    }
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    drop(conn);
    Ok(removed)
}

/// 执行清理。
///
/// `ids` 来自 `scan()` 的条目 id；`hard = true` 表示彻底删除（不留回收站副本）。
pub fn purge(
    ids: &[String],
    hard: bool,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };

    if ids.is_empty() {
        return Err("没有选中任何要清理的项".to_string());
    }

    // 重新扫一遍拿权威的条目定义（避免前端传来过期 id）
    let snapshot = scan()?;
    let wanted: BTreeSet<&String> = ids.iter().collect();
    let mut picked: Vec<Item> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut known_ids: BTreeSet<String> = BTreeSet::new();
    if let Some(cats) = snapshot.get("categories").and_then(Value::as_array) {
        for cat in cats {
            let Some(raw_items) = cat.get("items").and_then(Value::as_array) else {
                continue;
            };
            for raw in raw_items {
                let id = raw
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                known_ids.insert(id.clone());
                if !wanted.contains(&id) {
                    continue;
                }
                let category = raw
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let paths: Vec<PathBuf> = raw
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
                    .unwrap_or_default();
                let session_id = id.strip_prefix("session:").map(str::to_string);
                let kind = if session_id.is_some() { "session" } else { "file" };
                picked.push(Item {
                    id,
                    kind,
                    category: match category {
                        "deleted" => "deleted",
                        "orphan_body" => "orphan_body",
                        "stale_artifacts" => "stale_artifacts",
                        "stale_workspace" => "stale_workspace",
                        _ => "orphan_meta",
                    },
                    title: raw
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    uid: raw
                        .get("uid")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    updated_at: raw.get("updated_at").and_then(Value::as_i64).unwrap_or(0),
                    bytes: raw.get("bytes").and_then(Value::as_u64).unwrap_or(0),
                    detail: String::new(),
                    recommended: true,
                    session_id,
                    paths,
                });
            }
        }
    }
    for id in ids {
        if !known_ids.contains(id) {
            skipped.push(json!({ "id": id, "reason": "本轮扫描里已不存在（可能刚被清过）" }));
        }
    }
    if picked.is_empty() {
        return Err("选中的项在当前扫描结果里都找不到了，请重新扫描".to_string());
    }

    let session_items: Vec<&Item> = picked.iter().filter(|i| i.session_id.is_some()).collect();
    let sids: BTreeSet<String> = session_items
        .iter()
        .filter_map(|i| i.session_id.clone())
        .collect();
    let all_paths: Vec<PathBuf> = picked.iter().flat_map(|i| i.paths.clone()).collect();
    let planned_bytes: u64 = picked.iter().map(|i| i.bytes).sum();

    log(&format!(
        "准备清理 {} 项（其中会话 {} 个、磁盘 {}）",
        picked.len(),
        sids.len(),
        human(planned_bytes)
    ));

    // 1) 退出 WorkBuddy（库被它held住时改库会被覆盖）
    let was_running = wb_is_running();
    if was_running {
        log("WorkBuddy 正在运行，先将其退出…");
        let quit = wb_quit(Duration::from_secs(20));
        for e in &quit.errors {
            log(&format!("  · {e}"));
        }
        if !quit.ok {
            return Err(format!(
                "WorkBuddy 未能退出（重试 {} 轮）：{}。已中止，未改动任何数据。",
                quit.attempts,
                quit.hint("WorkBuddy")
            ));
        }
        log(&format!(
            "WorkBuddy 已退出（{} 轮，涉及进程 {} 种）",
            quit.attempts,
            quit.killed.len()
        ));
    }

    // 2) 备份数据库
    let stamp = stamp();
    let backup_dir = backup_root().join(&stamp);
    let backed_up = backup_databases(&backup_dir)?;
    log(&format!("已备份数据库（{} 个文件）到 {}", backed_up.len(), backup_dir.display()));

    let mut rows_deleted = 0usize;
    let mut files_moved = 0usize;
    let mut failed: Vec<String> = Vec::new();

    // 3) 删库行（workbuddy.db 的 sessions + session_usage；同步映射库一起清）
    if !sids.is_empty() {
        rows_deleted += purge_db_rows(&db_path(), &sids, "sessions", "id")?;
        rows_deleted += purge_db_rows(&db_path(), &sids, "session_usage", "session_id")?;
        let root = data_root();
        if let Ok(rd) = std::fs::read_dir(&root) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("edge-sync-mapping-v") && n.ends_with(".db") {
                    let p = e.path();
                    rows_deleted += purge_db_rows(&p, &sids, "edge_sync_mapping", "session_id")?;
                    rows_deleted += purge_db_rows(&p, &sids, "edge_sync_image_mapping", "session_id")?;
                }
            }
        }
        log(&format!("已删除 {rows_deleted} 条数据库记录（含云端同步映射）"));
    }

    // 4) 文件：移入回收站 或 彻底删除
    if hard {
        for p in &all_paths {
            if !p.exists() {
                continue;
            }
            let ok = if p.is_dir() {
                std::fs::remove_dir_all(p).is_ok()
            } else {
                std::fs::remove_file(p).is_ok()
            };
            if ok {
                files_moved += 1;
            } else {
                failed.push(format!("{}（删除失败，可能被占用）", p.display()));
            }
        }
        log(&format!("已彻底删除 {files_moved} 个文件/目录"));
    } else {
        move_to_trash(&all_paths, &stamp, &mut failed);
        files_moved = all_paths.len().saturating_sub(failed.len());
        log(&format!(
            "已把 {files_moved} 个文件/目录移入回收站 {}",
            trash_root().join(&stamp).display()
        ));
    }

    // 5) 校验：清完还剩多少会话
    let verified_sessions = {
        let mut n = 0usize;
        if let Ok(conn) = open_wb_db_ro() {
            n = conn
                .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as usize;
        }
        n
    };
    log(&format!("清理后 WorkBuddy 会话表剩余 {verified_sessions} 条"));

    // 6) 重启客户端
    let mut relaunched = false;
    if was_running {
        if wb_launch().is_ok() {
            relaunched = true;
            log("已重新启动 WorkBuddy");
        } else {
            log("未能自动启动 WorkBuddy，请手动打开");
        }
    }

    // 盘面刚刚变了，扫描缓存立刻作废（下一次进清理页会重扫）。
    config::clear_cache_json(CACHE_NAME);

    Ok(json!({
        "requested": ids.len(),
        "purged": picked.len(),
        "sessions": sids.len(),
        "rows_deleted": rows_deleted,
        "files_removed": files_moved,
        "planned_bytes": planned_bytes,
        "hard": hard,
        "reclaimed_bytes": if hard { planned_bytes } else { 0 },
        "trash_dir": if hard { String::new() } else { trash_root().join(&stamp).to_string_lossy().into_owned() },
        "backup_dir": backup_dir.to_string_lossy(),
        "verified_sessions": verified_sessions,
        "was_running": was_running,
        "relaunched": relaunched,
        "failed": failed,
        "skipped": skipped,
    }))
}

/// 清空回收站（彻底释放空间）。
pub fn empty_trash() -> Result<Value, String> {
    let root = trash_root();
    if !root.exists() {
        return Ok(json!({ "removed": 0, "bytes": 0, "dir": root.to_string_lossy() }));
    }
    let bytes = dir_size(&root);
    let before = std::fs::read_dir(&root).map(|rd| rd.count()).unwrap_or(0);
    std::fs::remove_dir_all(&root).map_err(|e| format!("清空回收站失败: {e}"))?;
    config::clear_cache_json(CACHE_NAME);
    Ok(json!({
        "removed": before,
        "bytes": bytes,
        "dir": root.to_string_lossy(),
    }))
}

fn human(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1048576.0)
    } else {
        format!("{:.2} GB", bytes as f64 / 1073741824.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_like_guard() {
        assert!(is_uuid_like("28562deb-41c1-4113-bea2-97dc93e8490b"));
        assert!(!is_uuid_like("editor-sdk-1844-057f8623"));
        assert!(!is_uuid_like("tmbs"));
    }

    #[test]
    fn human_formats_units() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2 KB");
        assert_eq!(human(3 * 1048576), "3.0 MB");
    }

    #[test]
    fn copy_dir_copies_recursively() {
        let base = std::env::temp_dir().join(format!("wbcl-{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), "a").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), "b").unwrap();
        copy_dir(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "a");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
            "b"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn purge_db_rows_skips_missing_table() {
        let conn = Connection::open_in_memory().unwrap();
        drop(conn);
        let dir = std::env::temp_dir().join(format!("wbcl-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY); INSERT INTO sessions VALUES('a');")
                .unwrap();
        }
        let mut sids = BTreeSet::new();
        sids.insert("a".to_string());
        assert_eq!(purge_db_rows(&p, &sids, "sessions", "id").unwrap(), 1);
        // 不存在的表要静默跳过，而不是报错
        assert_eq!(purge_db_rows(&p, &sids, "no_such_table", "id").unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
