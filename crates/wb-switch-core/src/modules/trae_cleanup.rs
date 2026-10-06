//! Trae 本机清理：会话记录 / 工具残留 / 客户端残留。
//!
//! 与「WorkBuddy 清理」对称：先把这台机器上 **Trae 相关**的可回收物扫出来（**只读**），
//! 再由用户逐项勾选执行。分四类：
//!
//! 1. **会话与记录** —— Trae 库里的会话。走 `trae_delete` 的成熟链路：密钥校验 → 结束
//!    客户端 → 解密 + 合并 WAL → 删行 → 增量加密回写 → 整库备份 + 原子替换 → 会话文件
//!    进回收站。批量删除只做**一次**回写、只留**一份**备份（逐条删会让备份按整库大小
//!    线性膨胀，实测曾把 `trae/backup` 堆到 2.2 GB）。
//! 2. **工具残留** —— 本工具自己的中间产物与备份（`~/.twin-switch`）。整库备份每批
//!    ≈ 300 MB，实测累计到 GB 级；解密快照删掉后下次读取会自动重建。
//! 3. **客户端残留** —— Trae 自身的可再生缓存（Chromium/Electron 缓存、日志、崩溃转储）。
//!    删这些**不动**登录态与偏好设置（`Local Storage` / `Network` / `Preferences` 一律不碰），
//!    但要求客户端已关闭，否则文件被占用会删不掉。
//! 4. **回收站** —— 会话回收站（`deleted_sessions`）与本页的回收站，清空即彻底释放。
//!
//! 默认策略：`hard = false` 时**不真删**，而是移入本工具的回收站
//! （`~/.twin-switch/trash`，同盘 rename，几乎零成本），随时可清空释放空间。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::modules::config::{self, store_dir};
use crate::modules::trae_delete::delete_sessions;
use crate::modules::trae_discover::{get_client, list_installed_clients, user_data_dir};
use crate::modules::trae_export::{
    backup_dir, deleted_sessions_dir, export_dir, import_backup_dir, list_backup_batches,
    wb_import_backup_dir,
};
use crate::modules::trae_switch::is_running;

/// 客户端里**可再生**的缓存/日志（相对 userData 根）。第二个元素是给人看的说明。
///
/// 刻意不含 `Local Storage` / `Session Storage` / `Network` / `User/globalStorage` /
/// `Preferences` —— 那些关系到登录态与偏好，删了要重新登录。
const CLIENT_RESIDUE: [(&str, &str); 17] = [
    ("Cache", "HTTP 磁盘缓存"),
    ("Code Cache", "JS 代码缓存"),
    ("GPUCache", "GPU 缓存"),
    ("DawnGraphiteCache", "Dawn 图形缓存"),
    ("DawnWebGPUCache", "Dawn WebGPU 缓存"),
    ("GrShaderCache", "着色器缓存"),
    ("ShaderCache", "着色器缓存"),
    ("VMCache", "V8 编译缓存"),
    ("CachedData", "Electron 数据缓存"),
    ("CachedConfigurations", "配置缓存"),
    ("CachedExtensionVSIXs", "扩展 VSIX 缓存"),
    ("CachedProfilesData", "配置档案缓存"),
    ("blob_storage", "Blob 临时存储"),
    ("Shared Dictionary", "共享压缩字典"),
    ("DIPS", "隐私沙盒统计"),
    ("VideoDecodeStats", "视频解码统计"),
    ("Crashpad", "崩溃转储"),
];

/// 客户端分区（`Partitions/<name>/`）里同样可再生的子目录。
const PARTITION_RESIDUE: [&str; 7] = [
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "Shared Dictionary",
    "DIPS",
];

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn mb(bytes: u64) -> f64 {
    (bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0
}

/// 递归统计路径字节数（只读）。
fn path_size(p: &Path) -> u64 {
    let Ok(md) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if md.is_file() {
        return md.len();
    }
    if !md.is_dir() {
        return 0;
    }
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten().map(|e| path_size(&e.path())).sum()
}

/// 条目 id：`前缀 + 规范化绝对路径`。仅用于前后端之间对齐选择。
fn id_of(prefix: &str, p: &Path) -> String {
    format!("{prefix}:{}", p.to_string_lossy().replace('\\', "/"))
}

/// 本页回收站根目录。
pub fn trash_root() -> PathBuf {
    store_dir().join("trash")
}

/// 硬删除（文件 / 目录 / 符号链接）。
fn hard_remove(p: &Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if md.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(p).map_err(|e| e.to_string())
    }
}

/// 移入回收站（同盘 `rename`，几乎零成本）；跨盘时退化为「复制 + 删除」。
fn move_to_trash(p: &Path, stamp: &str, seq: usize) -> Result<PathBuf, String> {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_string());
    let dest_dir = trash_root().join(stamp).join(format!("{seq:03}"));
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("创建回收站目录失败: {e}"))?;
    let dest = dest_dir.join(&name);
    if std::fs::rename(p, &dest).is_ok() {
        return Ok(dest);
    }
    // 跨盘：递归复制后再删源
    copy_recursive(p, &dest).map_err(|e| format!("移入回收站失败: {e}"))?;
    hard_remove(p)?;
    Ok(dest)
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    let md = std::fs::symlink_metadata(src)?;
    if md.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// 组装一个「目录/文件」型条目。
fn file_item(
    category: &'static str,
    path: &Path,
    title: String,
    detail: String,
    recommended: bool,
    needs_client_stop: bool,
) -> Value {
    let bytes = path_size(path);
    json!({
        "id": id_of(category, path),
        "kind": if path.is_dir() { "dir" } else { "file" },
        "category": category,
        "title": title,
        "detail": detail,
        "bytes": bytes,
        "bytes_mb": mb(bytes),
        "recommended": recommended,
        "needs_client_stop": needs_client_stop,
        "client_key": "",
        "client_label": "",
        "session_id": "",
        "turns": 0,
        "updated": "",
        "paths": [path.to_string_lossy()],
    })
}

// ---------------------------------------------------------------------------
// 扫描（只读）
// ---------------------------------------------------------------------------

/// 扫描缓存文件名（落在 `config::cache_dir()`）。
const CACHE_NAME: &str = "trae-cleanup.json";
/// 缓存结构版本，字段一改就 +1。
const CACHE_VERSION: u64 = 1;
/// 「这份缓存算旧了」的阈值：10 分钟。
///
/// **注意它不再是「过期就丢」的闸门**（v0.0.23 改）：以前超过 10 分钟就回落到实时扫描，
/// 结果是「每次点进清理页都白等几秒」，与用户「之前扫过的记录可以用」的诉求相反。
/// 现在超时只是给结果打上 `stale = true`，由界面提示、由用户决定要不要重扫。
const CACHE_TTL_MS: i64 = 10 * 60 * 1000;

/// 带缓存的扫描：**有缓存就用缓存**（毫秒级），只有在根本没有缓存时才扫。
///
/// `force = true` 跳过缓存（前端「重新扫描」按钮）。
///
/// 返回值在原结构上多四个字段：
/// - `cached`：本次结果是否来自缓存；
/// - `scannedAt`：这一份数据的生成时间；
/// - `ageMs`：距离生成过了多久（实时扫描为 0）；
/// - `stale`：缓存是否已超过 [`CACHE_TTL_MS`]（界面上提示「建议重新扫描」）。
///
/// 为什么要缓存：`scan()` 要递归统计几 GB 目录（本机实测 5.5 s）。它虽然已经
/// 走后台线程、不再冻界面，但每次进清理页都白等 5 秒没有意义。
///
/// 新鲜数据从哪来：**应用启动时首页会在后台扫一遍**（`HomePage::loadReclaim`），
/// 清理完之后也会立刻重扫一次。所以「进页面」这条路完全可以只读缓存。
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
        obj.insert("scannedAt".into(), json!(config::now_ms()));
        obj.insert("ageMs".into(), json!(0));
    }
    config::write_cache_slot(CACHE_NAME, CACHE_VERSION, &out);
    Ok(out)
}

/// 扫描本机 Trae 相关的全部可清理项；**不改动任何文件与数据库**。
pub fn scan() -> Result<Value, String> {
    let mut cats: Vec<Value> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    let (sessions, mut sess_notes) = scan_sessions();
    notes.append(&mut sess_notes);
    cats.push(sessions);
    cats.push(scan_tool_residue());
    let (client, mut cli_notes) = scan_client_residue();
    notes.append(&mut cli_notes);
    cats.push(client);
    cats.push(scan_recycle());

    let total: u64 = cats.iter().filter_map(|c| c["bytes"].as_u64()).sum();
    Ok(json!({
        "store_root": store_dir().to_string_lossy(),
        "trash_root": trash_root().to_string_lossy(),
        "total_bytes": total,
        "total_mb": mb(total),
        "categories": cats,
        "notes": notes,
    }))
}

/// 第 1 类：Trae 库里的会话。
fn scan_sessions() -> (Value, Vec<String>) {
    let mut items: Vec<Value> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for c in list_installed_clients() {
        if !c.installed {
            continue;
        }
        let Some(client) = get_client(&c.key) else {
            continue;
        };
        if is_running(client) {
            notes.push(format!(
                "{} 正在运行：删除会话时会自动结束进程，并在完成后重新启动。",
                c.label
            ));
        }
        match crate::modules::trae_export::list_sessions(&c.key) {
            Ok(list) => {
                for s in list {
                    // 空会话（一轮都没有）几乎都是误建/中断留下的，默认勾选
                    let empty = s.turns == 0;
                    items.push(json!({
                        "id": format!("session:{}:{}", c.key, s.id),
                        "kind": "session",
                        "category": "sessions",
                        "title": if s.title.trim().is_empty() { "（无标题）".to_string() } else { s.title.clone() },
                        "detail": format!("{} · {} 轮 · 更新 {}", c.label, s.turns, s.updated),
                        "bytes": 0,
                        "bytes_mb": 0.0,
                        "recommended": empty,
                        "needs_client_stop": true,
                        "client_key": c.key,
                        "client_label": c.label,
                        "session_id": s.id,
                        "turns": s.turns,
                        "owner": s.owner_label,
                        "owner_uid": s.owner_uid,
                        "updated": s.updated,
                        "created": s.created,
                        "paths": [],
                    }));
                }
            }
            Err(e) => notes.push(format!("{} 的会话列表暂时读不到：{e}", c.label)),
        }
    }

    let recommended = items.iter().filter(|i| i["recommended"] == json!(true)).count();
    (
        json!({
            "id": "sessions",
            "title": "Trae 会话与记录",
            "desc": "删除 Trae 库里的会话（整库备份 → 加密回写 → 会话文件进回收站）。批量删除只做一次回写、只留一份备份。",
            "bytes": 0,
            "count": items.len(),
            "recommended_count": recommended,
            "items": items,
        }),
        notes,
    )
}

/// 第 2 类：本工具的中间产物与备份。
fn scan_tool_residue() -> Value {
    let mut items: Vec<Value> = Vec::new();

    // 2.1 整库备份批次 —— 每批 ≈ 一个整库（300 MB 量级），按批列出让用户自己挑
    for (dir, label, prefix) in [
        (backup_dir(), "删除会话前的整库备份", "backup"),
        (import_backup_dir(), "分批导入的整库备份", "import_backup"),
        (wb_import_backup_dir(), "WorkBuddy 导入的整库备份", "wb_import_backup"),
        (
            store_dir().join("workbuddy_export_backup"),
            "导出到 WorkBuddy 前的整库备份",
            "wb_export_backup",
        ),
    ] {
        let batches = list_backup_batches(&dir);
        if batches.is_empty() {
            continue;
        }
        // 每个客户端只留最新一批（那就是「出事能回滚」的那一份）
        let mut newest: BTreeMap<&str, &str> = BTreeMap::new();
        for b in &batches {
            newest.insert(b.owner.as_str(), b.stamp.as_str());
        }
        for b in &batches {
            let is_newest = newest.get(b.owner.as_str()) == Some(&b.stamp.as_str());
            // 有些批次名不带客户端前缀（例如 `20261004-220644`），此时解析出的
            // stamp 为空、owner 本身就是时间戳 —— 显示时要把它当时间用。
            let (owner_disp, stamp_disp) = if b.stamp.is_empty() {
                (String::new(), pretty_stamp(&b.owner))
            } else {
                (b.owner.clone(), pretty_stamp(&b.stamp))
            };
            let shown_title = if owner_disp.is_empty() {
                format!("{label} · {stamp_disp}")
            } else {
                format!("{label} · {owner_disp} · {stamp_disp}")
            };
            items.push(json!({
                "id": format!("{prefix}:{}:{}", b.owner, b.stamp),
                "kind": "batch",
                "category": "tool_residue",
                "title": shown_title,
                "detail": format!(
                    "{}（{} 个文件）{}",
                    dir_label(&dir),
                    b.paths.len(),
                    if is_newest { "· 最新一批，保留可回滚" } else { "" }
                ),
                "bytes": b.bytes,
                "bytes_mb": mb(b.bytes),
                // 最新一批是安全网，默认不勾
                "recommended": !is_newest,
                "needs_client_stop": false,
                "client_key": b.owner.clone(),
                "client_label": "",
                "session_id": "",
                "turns": 0,
                "updated": stamp_disp,
                "paths": b.paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
            }));
        }
    }

    // 2.2 中间产物 / 快照 / 日志
    let tool_dirs: [(PathBuf, &str, &str, bool); 7] = [
        (store_dir().join("workbuddy_import"), "WorkBuddy 导入的中间产物", "导入时写库暂存，可随时重建", true),
        (store_dir().join("workbuddy_scan"), "WorkBuddy 库扫描快照", "为避免与客户端争 WAL 锁而复制，可随时重建", true),
        (store_dir().join("trae").join("import_tmp"), "分批导入临时目录", "导入中断留下的中间文件", true),
        (store_dir().join("trae").join("delete_tmp"), "删除临时目录", "删除中断留下的中间文件", true),
        (store_dir().join("trae").join("decrypted"), "解密快照", "读取会话时的解密副本，删后下次读取自动重建", true),
        (store_dir().join("trae").join("logs"), "工具运行日志", "", true),
        (export_dir(), "导出产物（Markdown）", "「导出」功能生成的文件", false),
    ];
    for (p, title, detail, rec) in tool_dirs {
        if p.exists() {
            items.push(file_item(
                "tool_residue",
                &p,
                title.to_string(),
                detail.to_string(),
                rec,
                false,
            ));
        }
    }
    // 工具根目录的错误日志
    for name in ["error.log"] {
        let p = store_dir().join(name);
        if p.is_file() {
            items.push(file_item(
                "tool_residue",
                &p,
                "工具错误日志".to_string(),
                String::new(),
                true,
                false,
            ));
        }
    }

    tool_category("tool_residue", "工具残留（本工具目录）", "整库备份、解密快照、导入中间产物与日志。删除后不影响已写入 Trae 的数据。", items)
}

/// 第 3 类：Trae 客户端的可再生缓存。
fn scan_client_residue() -> (Value, Vec<String>) {
    let mut items: Vec<Value> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for c in list_installed_clients() {
        if !c.installed {
            continue;
        }
        let Some(client) = get_client(&c.key) else {
            continue;
        };
        let root = user_data_dir(client);
        let running = is_running(client);
        if running {
            notes.push(format!(
                "{} 正在运行：客户端缓存被进程占用，请先关闭客户端再清理（其余项不受影响）。",
                c.label
            ));
        }

        for (rel, desc) in CLIENT_RESIDUE {
            let p = root.join(rel);
            if !p.exists() || path_size(&p) == 0 {
                continue;
            }
            items.push(file_item(
                "client_residue",
                &p,
                format!("{} · {}", c.label, rel),
                format!("{desc}（可再生，不影响登录态）"),
                false,
                true,
            ));
        }
        // 客户端日志单独列（在 logs/ 下）
        let logs = root.join("logs");
        if logs.is_dir() && path_size(&logs) > 0 {
            items.push(file_item(
                "client_residue",
                &logs,
                format!("{} · logs", c.label),
                "客户端运行日志（可再生）".to_string(),
                false,
                true,
            ));
        }
        // 编辑器本地历史
        let hist = root.join("User").join("History");
        if hist.is_dir() && path_size(&hist) > 0 {
            items.push(file_item(
                "client_residue",
                &hist,
                format!("{} · 本地文件历史", c.label),
                "编辑器的文件历史与撤销记录，删后无法找回".to_string(),
                false,
                true,
            ));
        }
        // 分区缓存：只碰缓存子目录，绝不动 Local Storage / Network
        let partitions = root.join("Partitions");
        if let Ok(rd) = std::fs::read_dir(&partitions) {
            for ent in rd.flatten() {
                if !ent.path().is_dir() {
                    continue;
                }
                for rel in PARTITION_RESIDUE {
                    let p = ent.path().join(rel);
                    if !p.exists() || path_size(&p) == 0 {
                        continue;
                    }
                    let part = ent.file_name().to_string_lossy().into_owned();
                    items.push(file_item(
                        "client_residue",
                        &p,
                        format!("{} · {part}/{rel}", c.label),
                        "分区缓存（可再生，不影响登录态）".to_string(),
                        false,
                        true,
                    ));
                }
            }
        }
    }

    (
        tool_category(
            "client_residue",
            "客户端残留（Trae 自身缓存）",
            "Chromium/Electron 缓存、日志与崩溃转储。不碰登录态与偏好设置；清理前请关闭客户端。",
            items,
        ),
        notes,
    )
}

/// 第 4 类：回收站。
fn scan_recycle() -> Value {
    let mut items: Vec<Value> = Vec::new();
    let ds = deleted_sessions_dir();
    if ds.is_dir() {
        let n = std::fs::read_dir(&ds).map(|r| r.flatten().count()).unwrap_or(0);
        if n > 0 {
            items.push(file_item(
                "recycle",
                &ds,
                "会话回收站".to_string(),
                format!("{n} 个已删除会话的归档文件，清空后无法恢复"),
                false,
                false,
            ));
        }
    }
    let t = trash_root();
    if t.is_dir() {
        let n = std::fs::read_dir(&t).map(|r| r.flatten().count()).unwrap_or(0);
        if n > 0 {
            items.push(file_item(
                "recycle",
                &t,
                "清理回收站".to_string(),
                format!("{n} 批本页清理时移入的内容，清空后无法恢复"),
                false,
                false,
            ));
        }
    }
    tool_category(
        "recycle",
        "回收站",
        "这两处是安全网：确认不再需要回滚后再清空，才能真释放磁盘。",
        items,
    )
}

/// 统一的分类包装。
fn tool_category(id: &str, title: &str, desc: &str, items: Vec<Value>) -> Value {
    let bytes: u64 = items.iter().filter_map(|i| i["bytes"].as_u64()).sum();
    let recommended = items.iter().filter(|i| i["recommended"] == json!(true)).count();
    json!({
        "id": id,
        "title": title,
        "desc": desc,
        "bytes": bytes,
        "bytes_mb": mb(bytes),
        "count": items.len(),
        "recommended_count": recommended,
        "items": items,
    })
}

fn dir_label(p: &Path) -> String {
    p.strip_prefix(store_dir())
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.to_string_lossy().into_owned())
}

/// `20261005_093156` → `2026-10-05 09:31`
fn pretty_stamp(s: &str) -> String {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.len() >= 12 {
        format!(
            "{}-{}-{} {}:{}",
            &digits[0..4],
            &digits[4..6],
            &digits[6..8],
            &digits[8..10],
            &digits[10..12]
        )
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// 执行
// ---------------------------------------------------------------------------

/// 执行清理。
///
/// `ids` 来自 [`scan`] 的条目 id；`hard = true` 表示直接删除（不进回收站）。
pub fn purge(ids: &[String], hard: bool, on_log: Option<&dyn Fn(&str)>) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    if ids.is_empty() {
        return Err("没有选中任何要清理的项".to_string());
    }

    // 重新扫描拿权威定义，避免前端传来过期的 id / 路径
    let snapshot = scan()?;
    let wanted: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let mut known: BTreeSet<String> = BTreeSet::new();

    let mut session_sel: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // (客户端, 会话 id, 归属账号 uid) —— uid 必须在这里就取出来，
    // 本地删完之后解密库里就没有这一行了，云端同步会无从判断归属。
    let mut session_targets: Vec<(String, String, String)> = Vec::new();
    let mut file_sel: Vec<(String, PathBuf)> = Vec::new();

    for cat in snapshot["categories"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let category = cat["id"].as_str().unwrap_or("").to_string();
        let Some(items) = cat["items"].as_array() else {
            continue;
        };
        for it in items {
            let id = it["id"].as_str().unwrap_or("").to_string();
            known.insert(id.clone());
            if !wanted.contains(id.as_str()) {
                continue;
            }
            if category == "sessions" {
                let ck = it["client_key"].as_str().unwrap_or("").to_string();
                let sid = it["session_id"].as_str().unwrap_or("").to_string();
                if !ck.is_empty() && !sid.is_empty() {
                    session_targets.push((
                        ck.clone(),
                        sid.clone(),
                        it["owner_uid"].as_str().unwrap_or("").to_string(),
                    ));
                    session_sel.entry(ck).or_default().push(sid);
                }
                continue;
            }
            for p in it["paths"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                if let Some(s) = p.as_str() {
                    let pb = PathBuf::from(s);
                    // 兜底：绝不允许把工具根目录 / 用户数据根目录整个删掉
                    if is_protected(&pb) {
                        continue;
                    }
                    file_sel.push((category.clone(), pb));
                }
            }
        }
    }

    let unknown: Vec<String> = wanted
        .iter()
        .filter(|w| !known.contains(**w))
        .map(|w| (*w).to_string())
        .collect();

    let mut errors: Vec<String> = Vec::new();
    let mut details: Vec<Value> = Vec::new();
    let mut removed = 0usize;
    let mut freed = 0u64;

    // 1) 会话：按客户端各走一趟批量删除
    for (ck, sids) in &session_sel {
        log(&format!("{}：批量删除 {} 个会话…", ck, sids.len()));
        match delete_sessions(ck, sids, on_log) {
            Ok(v) => {
                removed += sids.len();
                details.push(json!({
                    "kind": "sessions",
                    "client": ck,
                    "count": sids.len(),
                    "result": v,
                }));
            }
            Err(e) => errors.push(format!("{ck}：删除会话失败 —— {e}")),
        }
    }

    // 2) 文件 / 目录
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let mut seq = 0usize;
    for (category, p) in &file_sel {
        if !p.exists() {
            continue;
        }
        let sz = path_size(p);
        let outcome = if hard {
            hard_remove(p)
        } else {
            move_to_trash(p, &stamp, seq).map(|_| ())
        };
        match outcome {
            Ok(_) => {
                seq += 1;
                removed += 1;
                freed += sz;
                details.push(json!({
                    "kind": "path",
                    "category": category,
                    "path": p.to_string_lossy(),
                    "bytes": sz,
                    "bytes_mb": mb(sz),
                    "hard": hard,
                }));
            }
            Err(e) => errors.push(format!("{}：{e}", p.display())),
        }
    }

    log(&format!(
        "完成：处理 {removed} 项，回收 {:.1} MB{}",
        mb(freed),
        if hard { "" } else { "（未真删，内容已移入回收站）" }
    ));

    // 盘面刚刚变了，扫描缓存立刻作废（下一次进清理页会重扫）。
    config::clear_cache_json(CACHE_NAME);

    Ok(json!({
        "ok": true,
        "hard": hard,
        "removed": removed,
        "freed_bytes": freed,
        "freed_mb": mb(freed),
        "session_count": session_sel.values().map(Vec::len).sum::<usize>(),
        "session_targets": session_targets
            .iter()
            .map(|(c, s, u)| json!({ "client_key": c, "session_id": s, "owner_uid": u }))
            .collect::<Vec<_>>(),
        "details": details,
        "errors": errors,
        "unknown_ids": unknown,
        "trash_root": trash_root().to_string_lossy(),
    }))
}

/// 清空本页回收站（真删，不可恢复）。
pub fn empty_trash() -> Result<Value, String> {
    let root = trash_root();
    let before = path_size(&root);
    let mut removed = 0usize;
    let mut freed = 0u64;
    if root.is_dir() {
        let rd = std::fs::read_dir(&root).map_err(|e| format!("读取回收站失败: {e}"))?;
        for ent in rd.flatten() {
            let p = ent.path();
            let sz = path_size(&p);
            if hard_remove(&p).is_ok() {
                removed += 1;
                freed += sz;
            }
        }
    }
    // 盘面刚变了，扫描缓存立刻作废。
    config::clear_cache_json(CACHE_NAME);
    Ok(json!({
        "ok": true,
        "removed": removed,
        "before_mb": mb(before),
        "freed_bytes": freed,
        "freed_mb": mb(freed),
    }))
}

/// 保护名单：这些路径绝不能被本模块清掉。
///
/// 扫描逻辑本身不会产出它们，这里是**防御性**兜底 —— 万一 id/路径被伪造或过期，
/// 宁可什么都不做，也不能把工具根目录、密钥库、用户数据根整个删掉。
fn is_protected(p: &Path) -> bool {
    let s = p.to_string_lossy().replace('\\', "/");
    let store = store_dir().to_string_lossy().replace('\\', "/");
    if s == store || s == format!("{store}/trae") {
        return true;
    }
    for tail in ["/keys", "/vault", "/handoff"] {
        if s.ends_with(tail) {
            return true;
        }
    }
    // 用户数据根目录本身（`…/Trae CN`）
    list_installed_clients().iter().any(|c| {
        let root = c.user_data_dir.replace('\\', "/");
        s == root
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "trae-cleanup-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn path_size_sums_recursively() {
        let d = tmp("size");
        std::fs::write(d.join("a.txt"), b"12345").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub").join("b.txt"), b"123").unwrap();
        assert_eq!(path_size(&d), 8);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hard_remove_handles_file_and_dir() {
        let d = tmp("hard");
        let f = d.join("x.txt");
        std::fs::write(&f, b"x").unwrap();
        hard_remove(&f).unwrap();
        assert!(!f.exists());
        let sub = d.join("nested");
        std::fs::create_dir_all(sub.join("deep")).unwrap();
        std::fs::write(sub.join("deep").join("y"), b"y").unwrap();
        hard_remove(&sub).unwrap();
        assert!(!sub.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_recursive_reproduces_tree() {
        let d = tmp("copy");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("a")).unwrap();
        std::fs::write(src.join("a").join("1.txt"), b"hello").unwrap();
        let dst = d.join("dst");
        copy_recursive(&src, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("a").join("1.txt")).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pretty_stamp_formats_compact_timestamps() {
        assert_eq!(pretty_stamp("20261005_093156"), "2026-10-05 09:31");
        assert_eq!(pretty_stamp("20261005-093156"), "2026-10-05 09:31");
        assert_eq!(pretty_stamp(""), "");
    }

    #[test]
    fn protected_paths_are_refused() {
        // 工具根目录与 trae 子目录整删会连带毁掉密钥与账号资料
        assert!(is_protected(&store_dir()));
        assert!(is_protected(&store_dir().join("trae")));
        assert!(is_protected(&store_dir().join("trae").join("keys")));
        assert!(is_protected(&store_dir().join("trae").join("vault")));
        // 正常目标不受影响
        assert!(!is_protected(&store_dir().join("trae").join("backup")));
        assert!(!is_protected(&store_dir().join("trae").join("decrypted")));
    }

    #[test]
    fn id_is_stable_and_path_normalized() {
        let p = PathBuf::from("C:\\a\\b\\c");
        assert_eq!(id_of("tool_residue", &p), "tool_residue:C:/a/b/c");
    }
}
