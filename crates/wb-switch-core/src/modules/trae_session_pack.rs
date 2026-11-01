//! 会话包：把会话导出成**可回灌的数据包**，用于跨机器迁移。
//!
//! ## 和「导出 md」的区别
//!
//! `trae_export::export_all` 生成的是给人读的 `.md`，**没法导回去**。
//! 本模块生成的是 SQLite 裁剪库 + manifest，导入侧能把内容原样写回客户端库。
//!
//! ## 包结构
//!
//! ```text
//! twinswitch-sessions-traecn_20261101_091800.zip
//! ├── pack.db          # 裁剪后的明文 SQLite（只含选中会话的行）
//! └── manifest.json    # 版本与清单，导入前校验
//! ```
//!
//! ## 为什么用 SQLite 而不是 JSON
//!
//! 1. **能直接复用 `trae_import::copy_session`** —— 它本来就是「两个 `Connection`
//!    之间搬一个会话」的通用实现，只要包里是同样 schema 的库就直接能跑，
//!    不用为几十个字段手写序列化（漏一个字段就是「会话打不开」）。
//! 2. **保真**：`chat_message_task` 里是大段 JSON（工具输出、文件内容），
//!    走 JSON 往返容易在类型/转义上失真。
//! 3. **体积可控**：只 `INSERT ... SELECT` 选中会话的行，不会把整库（含其他账号
//!    数据）带出去。
//!
//! ## 导出的表
//!
//! `chat_session` / `chat_message` / `chat_message_general` / `chat_message_task` /
//! `chat_turn` / `session_project` / `project`。
//!
//! `fts_*` 是派生的全文索引虚表，不导出 —— 客户端与本工具都会重建。

use crate::modules::trae_discover::get_client;
use crate::modules::trae_export;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::session_pack::{self, Manifest, ManifestSession, PACK_FORMAT, PACK_VERSION};

/// 需要导出的表（顺序无关，但保持稳定便于测试）。
///
/// ⚠️ `agent_run` / `history_v2` **必须带**：它们都挂 `session_id`，
/// 是会话的一部分（agent 执行记录、历史明细）。漏了不会让导入报错，
/// 但客户端里那几条会话的历史会残缺 —— 这种「静默缺失」最难被发现。
///
/// `server_history_info` 不带：实测与存活会话相关的行数是 0（它存的是
/// 服务端同步镜像，导入侧也不需要），带它只会让包无谓变大。
pub const PACK_TABLES: &[&str] = &[
    "project",
    "chat_session",
    "chat_message",
    "chat_message_general",
    "chat_message_task",
    "chat_turn",
    "session_project",
    "agent_run",
    "history_v2",
];

/// 导出会话包。
///
/// `sources`：客户端 key（`trae-cn` 等）。`session_ids` 为 `None` 时导出该客户端全部会话。
pub fn export_pack(
    sources: &[String],
    session_ids: Option<&[String]>,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    if sources.is_empty() {
        return Err("未指定数据源".into());
    }
    let dir = trae_export::export_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();

    // ⚠️ 临时目录名必须带 pid + 纳秒：只靠秒级时间戳时，并发导出（或同一秒连点两次）
    // 会共用同一个目录，第二个直接撞主键 / 读到半成品。
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = dir.join(format!(".pack-build-{stamp}-{}-{uniq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("创建临时目录失败: {e}"))?;

    let pack_db = work.join("pack.db");
    let mut manifest = Manifest {
        format: PACK_FORMAT.to_string(),
        format_version: PACK_VERSION,
        exported_at: chrono::Local::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        source_client: sources.join(","),
        source_label: sources
            .iter()
            .map(|s| get_client(s).map(|c| c.label.to_string()).unwrap_or_else(|| s.clone()))
            .collect::<Vec<_>>()
            .join("、"),
        source_uid: None,
        sessions: Vec::new(),
        workspaces: Vec::new(),
        tables: PACK_TABLES.iter().map(|s| s.to_string()).collect(),
        kind: "trae".to_string(),
    };

    let mut src_uids: Vec<String> = Vec::new();
    let mut total_rows = 0usize;

    {
        let pack = Connection::open(&pack_db).map_err(|e| format!("创建包数据库失败: {e}"))?;
        // ⚠️ 用 WAL 之外的默认 journal：包是静态文件，不需要 WAL 的并发特性，
        // 而且导入侧会用只读方式打开，留一份 `-wal` 在旁边反而容易漏带。
        let _ = pack.pragma_update(None, "journal_mode", "DELETE");

        for src in sources {
            let Some(client) = get_client(src) else {
                log(&format!("跳过未知客户端：{src}"));
                continue;
            };
            // 让解密快照跟上实时库（否则刚聊过的会话查不到）。
            if let Err(e) = trae_export::ensure_decrypted(src, None) {
                log(&format!("{} 解密失败，跳过：{e}", client.label));
                continue;
            }
            let snap = trae_export::decrypted_db_path(src);
            if !snap.is_file() {
                log(&format!("{} 没有解密库，跳过", client.label));
                continue;
            }

            let list = match trae_export::list_sessions(src) {
                Ok(v) => v,
                Err(e) => {
                    log(&format!("{} 读取会话列表失败：{e}", client.label));
                    continue;
                }
            };
            let picked: Vec<_> = match session_ids {
                Some(ids) => list.iter().filter(|s| ids.iter().any(|i| i == &s.id)).collect(),
                None => list.iter().collect(),
            };
            if picked.is_empty() {
                log(&format!("{} 没有可导出的会话", client.label));
                continue;
            }

            let start_rows = total_rows;
            match copy_into_pack(&pack, &snap, &picked.iter().map(|s| s.id.clone()).collect::<Vec<_>>()) {
                Ok(n) => {
                    total_rows += n;
                    log(&format!("{}：已写入 {n} 行", client.label, ));
                }
                Err(e) => {
                    log(&format!("{} 写入包失败：{e}", client.label));
                    continue;
                }
            }
            if total_rows == start_rows {
                continue;
            }

            for s in &picked {
                if !s.owner_uid.is_empty() && !src_uids.contains(&s.owner_uid) {
                    src_uids.push(s.owner_uid.clone());
                }
                if !s.workspace_path.is_empty() && !manifest.workspaces.contains(&s.workspace_path) {
                    manifest.workspaces.push(s.workspace_path.clone());
                }
                manifest.sessions.push(ManifestSession {
                    session_id: s.id.clone(),
                    title: s.title.clone(),
                    messages: s.messages,
                    workspace: s.workspace.clone(),
                    workspace_path: s.workspace_path.clone(),
                    updated: s.updated.clone(),
                    owner_uid: s.owner_uid.clone(),
                    has_body: s.messages > 0,
                });
            }
        }
        let _ = pack.execute_batch("PRAGMA optimize;");
    }

    if manifest.sessions.is_empty() {
        let _ = std::fs::remove_dir_all(&work);
        return Err("没有导出任何会话（可能是所选客户端尚未解密，或会话已被删除）".into());
    }
    manifest.source_uid = src_uids.first().cloned();

    // manifest 落盘
    let mpath = work.join("manifest.json");
    std::fs::write(
        &mpath,
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("生成 manifest 失败: {e}"))?,
    )
    .map_err(|e| format!("写入 manifest 失败: {e}"))?;

    // 打包
    let tag = sources
        .iter()
        .map(|s| s.replace("-cn", "cn").replace('-', ""))
        .collect::<Vec<_>>()
        .join("_");
    let fname = format!("twinswitch-sessions-{tag}_{stamp}.zip");
    let path = dir.join(&fname);
    let file = std::fs::File::create(&path).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zw = zip::ZipWriter::new(file);
    let opts = zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for name in ["manifest.json", "pack.db"] {
        let fp = work.join(name);
        if !fp.is_file() {
            continue;
        }
        let bytes = std::fs::read(&fp).map_err(|e| format!("读取 {name} 失败: {e}"))?;
        zw.start_file(name, opts).map_err(|e| format!("写入 zip 失败: {e}"))?;
        zw.write_all(&bytes).map_err(|e| format!("写入 zip 失败: {e}"))?;
    }
    zw.finish().map_err(|e| format!("完成 zip 失败: {e}"))?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&work);

    log(&format!(
        "已导出 {} 个会话（{:.1} MB）",
        manifest.sessions.len(),
        size as f64 / 1_048_576.0
    ));
    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "filename": fname,
        "sessions": manifest.sessions.len(),
        "rows": total_rows,
        "bytes": size,
        "workspaces": manifest.workspaces,
        "manifest": manifest,
    }))
}

/// 把选中会话的行从解密快照抄进包库（建表 + 按会话过滤的 INSERT ... SELECT）。
fn copy_into_pack(pack: &Connection, snap: &Path, ids: &[String]) -> Result<usize, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    pack.execute("ATTACH DATABASE ?1 AS src", [snap.to_string_lossy().as_ref()])
        .map_err(|e| format!("挂载源库失败: {e}"))?;

    let in_list = ids
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");

    // 每张表的行过滤条件。用子查询把「会话 → 消息 → 内容」的链带出来。
    let filters: Vec<(&str, String)> = vec![
        (
            "project",
            format!(
                "SELECT DISTINCT s.project_id FROM src.chat_session s \
                 WHERE s.session_id IN ({in_list}) AND ifnull(s.project_id,'') <> ''"
            ),
        ),
        ("chat_session", format!("SELECT session_id FROM src.chat_session WHERE session_id IN ({in_list})")),
        ("chat_message", format!("SELECT session_id FROM src.chat_message WHERE session_id IN ({in_list})")),
        (
            "chat_message_general",
            format!(
                "SELECT session_id FROM src.chat_message WHERE session_id IN ({in_list}) AND \
                 message_id IN (SELECT message_id FROM src.chat_message_general)"
            ),
        ),
        (
            "chat_message_task",
            format!(
                "SELECT session_id FROM src.chat_message WHERE session_id IN ({in_list}) AND \
                 message_id IN (SELECT message_id FROM src.chat_message_task)"
            ),
        ),
        ("chat_turn", format!("SELECT session_id FROM src.chat_turn WHERE session_id IN ({in_list})")),
        (
            "session_project",
            format!("SELECT session_id FROM src.session_project WHERE session_id IN ({in_list})"),
        ),
        // 挂 session_id 的辅助表：agent 执行记录、历史明细。
        ("agent_run", format!("SELECT session_id FROM src.agent_run WHERE session_id IN ({in_list})")),
        ("history_v2", format!("SELECT session_id FROM src.history_v2 WHERE session_id IN ({in_list})")),
    ];

    let mut rows = 0usize;
    for (table, _) in &filters {
        let ddl: Option<String> = pack
            .query_row(
                "SELECT sql FROM src.sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .ok();
        let Some(ddl) = ddl else {
            // 源库没有这张表（客户端版本差异）——跳过，别让整包失败。
            continue;
        };
        // 幂等：重复导出同一个客户端时别撞「表已存在」。
        let stmt = ddl.replacen("CREATE TABLE", "CREATE TABLE IF NOT EXISTS", 1);
        pack.execute_batch(&stmt)
            .map_err(|e| format!("建表 {table} 失败: {e}"))?;

        // ⚠️ 行过滤按表分别写：内容表要通过 message_id 关联，
        // 不能统一用 session_id（`chat_message_general/task` 没有这一列）。
        let where_sql = match *table {
            "project" => format!(
                "project_id IN (SELECT DISTINCT project_id FROM src.chat_session \
                 WHERE session_id IN ({in_list}) AND ifnull(project_id,'') <> '')"
            ),
            "chat_message_general" | "chat_message_task" => format!(
                "message_id IN (SELECT message_id FROM src.chat_message WHERE session_id IN ({in_list}))"
            ),
            _ => format!("session_id IN ({in_list})"),
        };
        let n = pack
            .execute(
                &format!("INSERT INTO main.\"{table}\" SELECT * FROM src.\"{table}\" WHERE {where_sql}"),
                [],
            )
            .map_err(|e| format!("复制表 {table} 失败: {e}"))?;
        rows += n;
    }
    let _ = pack.execute_batch("DETACH DATABASE src");
    Ok(rows)
}

/// 预检会话包（只读）：不写任何东西，只回答「这个包能不能导、导进去会发生什么」。
pub fn preview_pack(zip_path: &str) -> Result<Value, String> {
    let dir = session_pack::unpack_to_temp(zip_path, "trae-preview")?;
    let result = preview_unpacked(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn preview_unpacked(dir: &Path) -> Result<Value, String> {
    let manifest = session_pack::read_manifest(dir, PACK_FORMAT, PACK_VERSION)?;
    let pack_db = dir.join("pack.db");
    let conn = Connection::open(&pack_db).map_err(|e| format!("打开包数据库失败: {e}"))?;

    // 包里实际有多少条会话（以库为准，manifest 只作参考 —— 它可能被人改过）。
    let in_pack: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT session_id FROM chat_session")
            .map_err(|e| format!("读取包内会话失败: {e}"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("读取包内会话失败: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取包内会话失败: {e}"))?;
        rows
    };
    let _ = conn.close();

    Ok(json!({
        "ok": true,
        "manifest": manifest,
        "sessionCount": in_pack.len(),
        "sessionIds": in_pack,
        // 工作空间只做提示：路径在目标机不一定存在，但仍按原路径写入。
        "workspacesMissing": manifest
            .workspaces
            .iter()
            .filter(|w| !Path::new(w).exists())
            .cloned()
            .collect::<Vec<_>>(),
    }))
}

/// 导入会话包到指定账号。
///
/// `dst_uid`：目标账号的**库内真实 uid**（不是账号库内部 id，
/// 两者不同的坑见 `trae_vault::uid_of_account`）。
pub fn import_pack(
    zip_path: &str,
    dst_client_key: &str,
    dst_uid: &str,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    if get_client(dst_client_key).is_none() {
        return Err(format!("未知客户端：{dst_client_key}"));
    }
    if dst_uid.trim().is_empty() {
        return Err("缺少目标账号 uid".into());
    }

    let dir = session_pack::unpack_to_temp(zip_path, "trae-import")?;
    let outcome = (|| -> Result<Value, String> {
        let manifest = session_pack::read_manifest(&dir, PACK_FORMAT, PACK_VERSION)?;
        let pack_db = dir.join("pack.db");
        if !pack_db.is_file() {
            return Err("包里缺少 pack.db（不是有效的会话包）".into());
        }
        log(&format!(
            "包内含 {} 个会话（导出于 {}）",
            manifest.sessions.len(),
            manifest.exported_at
        ));

        // 以包里库为准列出会话 id：manifest 只作参考，可能被改过。
        let ids: Vec<String> = {
            let conn = Connection::open(&pack_db).map_err(|e| format!("打开包数据库失败: {e}"))?;
            let mut stmt = conn
                .prepare("SELECT session_id FROM chat_session")
                .map_err(|e| format!("读取包内会话失败: {e}"))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| format!("读取包内会话失败: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取包内会话失败: {e}"))?;
            drop(stmt);
            let _ = conn.close();
            rows
        };
        if ids.is_empty() {
            return Err("包里没有任何会话".into());
        }
        log(&format!("准备导入 {} 个会话…", ids.len()));

        // 复用本机导入的整套写库流程（结束客户端 → 备份 → 解密 → 合并 WAL →
        // 复制 → 自检 → 增量回写 → 替换 → 刷新快照 → 重启），只把「源库」换成包里的库。
        let src = super::trae_import::PackSrc { db: &pack_db };
        let res = super::trae_import::import_pack_source(
            dst_client_key,
            dst_uid,
            &ids,
            Some(&log),
            Some(&src),
        )?;
        Ok(res)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

/// 供 UI 展示的导出目录（与 md 导出同一个目录，用户不用记两个地方）。
pub fn pack_dir() -> PathBuf {
    trae_export::export_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机探针（`#[ignore]`）：把一个真实客户端的会话导出成包，再把包读回来核对行数。
    ///
    /// 只读源库、只写导出目录，不动任何客户端数据。
    /// 跑法：`cargo test -p wb-switch-core --lib pack_export_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn pack_export_probe_writes_real_pack() {
        let r = export_pack(&["trae-cn".to_string()], None, Some(&|m| println!("   {m}")))
            .expect("导出失败");
        println!("包路径：{}", r["path"]);
        println!("会话数：{}  行数：{}", r["sessions"], r["rows"]);
        println!("工作空间：{}", r["workspaces"]);
        assert!(r["sessions"].as_u64().unwrap_or(0) > 0, "至少要导出 1 个会话");

        // 读回来核对：包内会话数应与 manifest 一致，且 pack.db 有真实行
        let p = r["path"].as_str().unwrap();
        let prev = preview_pack(p).expect("预检失败");
        println!("预检：{}", prev);
        assert_eq!(
            prev["sessionCount"].as_u64().unwrap_or(0),
            r["sessions"].as_u64().unwrap_or(0),
            "包内会话数应与导出一致"
        );
    }

    /// 真机探针（`#[ignore]`）：核对包里的表与行确实覆盖了选中会话的全部内容。
    ///
    /// 判据：每个会话在 `chat_message` 里都有行，且 `chat_message_task` 的 message_id
    /// 都能在包内 `chat_message` 里找到（否则导进去就是断链的）。
    #[test]
    #[ignore]
    fn pack_probe_has_complete_message_chain() {
        let r = export_pack(&["trae-cn".to_string()], None, None).expect("导出失败");
        let dir = super::session_pack::unpack_to_temp(r["path"].as_str().unwrap(), "probe")
            .expect("解包失败");
        let conn = Connection::open(dir.join("pack.db")).expect("打开 pack.db 失败");
        let mut stmt = conn
            .prepare(
                "SELECT count(*) FROM chat_message_task t \
                 WHERE t.message_id NOT IN (SELECT message_id FROM chat_message)",
            )
            .unwrap();
        let orphan: i64 = stmt.query_row([], |r| r.get(0)).unwrap();
        let total_task: i64 = conn
            .query_row("SELECT count(*) FROM chat_message_task", [], |r| r.get(0))
            .unwrap();
        println!("chat_message_task {total_task} 行，其中孤立引用 {orphan} 行");
        assert_eq!(orphan, 0, "task 正文必须都能在包内找到对应消息，否则导入后是断链");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
