//! WorkBuddy 会话记录 / 会话复制 / 跨账号关联（国内版单端）。
//!
//! 只做国内版：数据根固定 `~/.workbuddy`，库固定 `workbuddy.db` 的 `sessions` 表，
//! 正文固定 `~/.workbuddy/projects/<工作区>/<cid>.jsonl`。参考项目里的 CodeBuddy CLI /
//! CodeBuddy IDE / VSCode 插件 / JetBrains 插件 / 国际版分支一律不搬。
//!
//! ## 三条硬规则（照抄参考实现最容易踩的坑）
//!
//! 1. **`edge-sync-mapping*.db` 一个字节都不写。**
//!    它是云端迁移映射（`edge-sync` 用它判断某个会话「是否已迁移到云端」）。
//!    一旦写入，edge-sync 会判定「已迁移」从而**跳过上传**，云端就永久缺这条会话。
//!    本模块所有写路径都过 [`assert_not_edge_sync`] 门禁。
//!
//! 2. **复制 = 新 UUID + INSERT 新行 + 新正文（`cid → new_cid` 文本替换）。**
//!    源账号的数据**一行都不改**：不 UPDATE、不删正文、不改归属。
//!    用 `INSERT` 而非 `INSERT OR REPLACE`——新 UUID 撞库时宁可失败，也不能悄悄覆盖。
//!
//! 3. **删除走软删 + 正文进本工具回收站。**
//!    客户端自己也是 `deleted_at IS NULL` 过滤，所以只置 `deleted_at` 即可让其消失；
//!    正文挪到 `store/trash/wb-sessions/` 而不是直接删，随时可还原。
//!
//! ## 并发门禁
//!
//! 写库只在「目标账号不是当前登录账号」或「客户端已退出」的情况下进行。
//! 目标账号正在被客户端使用时写库，运行中的实例看不到写入，继续对话会让会话状态分叉。

use crate::modules::config::{now_ms, store_dir};
use crate::modules::workspace::workspace_of;
use crate::modules::{
    workbuddy_accounts, workbuddy_auth, workbuddy_source, workbuddy_switch, workbuddy_vault,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 关联存储（本工具自己的）结构版本。
pub const LINK_STORE_VERSION: u32 = 1;

/// 目标账号正在被 WorkBuddy 使用时的统一错误文案。
pub const TARGET_IN_USE: &str =
    "目标账号正在 WorkBuddy 中使用，已阻止修改会话数据；请先退出 WorkBuddy 后重试";

// ---------------------------------------------------------------------------
// 数据库基础
// ---------------------------------------------------------------------------

/// 会话库路径（`~/.workbuddy/workbuddy.db`）。
pub fn db_path() -> PathBuf {
    workbuddy_auth::workbuddy_db_path()
}

fn open_ro(path: &Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let _ = conn.busy_timeout(Duration::from_secs(5));
    Some(conn)
}

pub(crate) fn open_rw(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("打开会话库失败: {e}"))?;
    conn.busy_timeout(Duration::from_secs(10))
        .map_err(|e| format!("设置数据库超时失败: {e}"))?;
    Ok(conn)
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 LIMIT 1")
        .and_then(|mut s| s.exists([name]))
        .unwrap_or(false)
}

/// 硬门禁：任何写操作前调用，确保本次绝不会碰到云端映射库。
///
/// 见模块头「三条硬规则」第 1 条。宁可拒绝也不能写出「云端缺会话」的事故。
pub fn assert_not_edge_sync(path: &Path) -> Result<(), String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with("edge-sync-mapping") {
        return Err(format!(
            "拒绝写入云端映射库 {name}：写入会让 edge-sync 判定「已迁移」并跳过上传，云端将永久缺会话"
        ));
    }
    Ok(())
}

/// 只读列出数据根下的云端映射库（供界面显示「已跳过，未写入」）。
pub fn edge_sync_databases() -> Vec<Value> {
    let root = workbuddy_auth::data_root();
    let mut out: Vec<Value> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&root) {
        for entry in dir.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if name.starts_with("edge-sync-mapping") && name.ends_with(".db") {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                out.push(json!({ "name": name, "path": path.to_string_lossy(), "size": size }));
            }
        }
    }
    out.sort_by_key(|v| v["name"].as_str().unwrap_or("").to_string());
    out
}

/// 会话行的归属 uid（未删除的行才计数）。
pub(crate) fn row_owner(conn: &Connection, cid: &str) -> Option<String> {
    conn.query_row(
        "SELECT user_id FROM sessions WHERE id = ?1 AND deleted_at IS NULL",
        [cid],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

fn row_deleted_at(conn: &Connection, cid: &str) -> Option<i64> {
    conn.query_row("SELECT deleted_at FROM sessions WHERE id = ?1", [cid], |row| {
        row.get::<_, Option<i64>>(0)
    })
    .ok()
    .flatten()
}

/// 读整行（列名 + 值），供复制时按目标表列取交集。
pub(crate) fn read_source_row(
    conn: &Connection,
    cid: &str,
) -> Result<Option<Vec<(String, SqlValue)>>, String> {
    let mut stmt = conn
        .prepare("SELECT * FROM sessions WHERE id = ?1")
        .map_err(|e| format!("读取会话行失败: {e}"))?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([cid]).map_err(|e| format!("查询会话行失败: {e}"))?;
    let Some(row) = rows.next().map_err(|e| format!("遍历会话行失败: {e}"))? else {
        return Ok(None);
    };
    let values = cols
        .into_iter()
        .enumerate()
        .map(|(i, col)| {
            let v = row.get::<_, SqlValue>(i).unwrap_or(SqlValue::Null);
            (col, v)
        })
        .collect();
    Ok(Some(values))
}

pub(crate) fn target_columns(conn: &Connection) -> Result<Vec<String>, String> {
    let stmt = conn
        .prepare("SELECT * FROM sessions LIMIT 0")
        .map_err(|e| format!("读取 sessions 列失败: {e}"))?;
    Ok(stmt.column_names().iter().map(|s| s.to_string()).collect())
}

/// 复制行的列值改写规则。
///
/// `group_id` / `group_title` 置空是**推断**：客户端用这两列做会话分组展示，
/// 副本是独立会话，沿用源值会让客户端把副本与源混进同一组（本机 `sessions` 表实测
/// 确实有这两列）。置空不影响会话本身，只影响分组归属。
fn override_value(
    col: &str,
    value: &SqlValue,
    new_cid: &str,
    target_uid: &str,
    now: i64,
) -> SqlValue {
    match col {
        "id" => SqlValue::Text(new_cid.to_string()),
        "user_id" => SqlValue::Text(target_uid.to_string()),
        "created_at" | "updated_at" | "last_activity_at" => SqlValue::Integer(now),
        "deleted_at" => SqlValue::Null,
        "group_id" | "group_title" => SqlValue::Null,
        _ => value.clone(),
    }
}

/// 把源行按「目标表列 ∩ 源行列」写成新行。
///
/// 目标存在而源没有的列不写，交给库的默认值补齐——写 NULL 会破坏 NOT NULL 约束。
pub(crate) fn insert_copy_row(
    conn: &Connection,
    source: &[(String, SqlValue)],
    new_cid: &str,
    target_uid: &str,
    now: i64,
) -> Result<(), String> {
    let cols = target_columns(conn)?;
    let mut names: Vec<String> = Vec::with_capacity(cols.len());
    let mut vals: Vec<SqlValue> = Vec::with_capacity(cols.len());
    for col in &cols {
        let Some((_, value)) = source.iter().find(|(name, _)| name == col) else {
            continue;
        };
        names.push(col.clone());
        vals.push(override_value(col, value, new_cid, target_uid, now));
    }
    if names.is_empty() {
        return Err("sessions 表没有任何可复制的列".to_string());
    }
    let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("INSERT INTO sessions ({}) VALUES ({})", names.join(", "), placeholders);
    let params: Vec<&SqlValue> = vals.iter().collect();
    conn.execute(&sql, rusqlite::params_from_iter(params))
        .map_err(|e| format!("写入会话副本失败: {e}"))?;
    Ok(())
}

/// 软删一行（只置 `deleted_at`），返回受影响行数。
fn soft_delete_row(
    conn: &Connection,
    cid: &str,
    uid: &str,
    ts: i64,
) -> Result<usize, String> {
    conn.execute(
        "UPDATE sessions SET deleted_at = ?1 WHERE id = ?2 AND user_id = ?3 AND deleted_at IS NULL",
        rusqlite::params![ts, cid, uid],
    )
    .map_err(|e| format!("删除会话记录失败: {e}"))
}

// ---------------------------------------------------------------------------
// 备份 / 回收站
// ---------------------------------------------------------------------------

/// 备份会话库（含 `-wal` / `-shm`），返回备份目录。
///
/// 任何一步失败都返回 Err：备份不可信时后续写入必须先停下来。
pub(crate) fn backup_db(tag: &str) -> Result<PathBuf, String> {
    let db = db_path();
    assert_not_edge_sync(&db)?;
    if !db.is_file() {
        return Err(format!("会话库不存在，未做任何改动：{}", db.display()));
    }
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dir = store_dir()
        .join("backup")
        .join(format!("wb_sessions_{tag}_{stamp}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    for suffix in ["", "-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{}", db.to_string_lossy(), suffix));
        if !src.is_file() {
            continue;
        }
        let dest = dir.join(format!("workbuddy.db{suffix}"));
        std::fs::copy(&src, &dest).map_err(|e| format!("备份 {suffix} 失败: {e}"))?;
        let a = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
        let b = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        if a != b {
            return Err(format!("备份 {suffix} 校验失败：大小不一致，未做任何改动"));
        }
    }
    Ok(dir)
}

fn trash_dir() -> PathBuf {
    store_dir().join("trash").join("wb-sessions")
}

/// 把会话正文挪进本工具回收站（可还原）。源不存在时返回 false。
fn move_body_to_trash(cid: &str) -> Result<bool, String> {
    let Some(src) = workbuddy_source::find_jsonl(cid) else {
        return Ok(false);
    };
    let dir = trash_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建回收站目录失败: {e}"))?;
    let dest = dir.join(format!("{cid}.jsonl"));
    if dest.exists() {
        // 同名残留：加时间戳，绝不覆盖已有回收站内容。
        let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
        let alt = dir.join(format!("{cid}_{stamp}.jsonl"));
        std::fs::rename(&src, &alt).map_err(|e| format!("移动正文到回收站失败: {e}"))?;
        return Ok(true);
    }
    std::fs::rename(&src, &dest).map_err(|e| format!("移动正文到回收站失败: {e}"))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// 一、会话记录：列出 / 详情 / 导出 / 删除
// ---------------------------------------------------------------------------

/// 工作空间判定统一走 [`crate::modules::workspace`]（三产品同一套规则）。

fn session_item(s: &workbuddy_source::WbSession) -> Value {
    let (workspace_key, workspace) = workspace_of(&s.cwd);
    json!({
        "id": s.id,
        "title": s.title,
        "cwd": s.cwd,
        // 归属工作空间（键 + 展示名 + 原样路径）。前端只按 `workspaceKey` 分组，
        // 展示一律用 `workspace`，两者都来自后端 ⇒ 不会出现「同一空间两个名字」。
        "workspace": workspace,
        "workspaceKey": workspace_key,
        "workspacePath": s.cwd.replace('\\', "/"),
        "model": s.model,
        "createdAt": s.created_at,
        "updatedAt": s.updated_at,
        "hasBody": s.has_body,
        "bodyBytes": s.body_bytes,
        "dupGroup": s.dup_group,
        "isNewest": s.is_newest,
        "dupNote": s.dup_note,
    })
}

/// 列出某账号名下未删除的会话（`sessions` 表为准，附正文存在性）。
pub fn list_for_account(uid: &str) -> Value {
    if uid.trim().is_empty() {
        return json!({ "ok": false, "error": "账号缺少 uid", "sessions": [] });
    }
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e, "sessions": [] }),
    };
    let sessions: Vec<Value> = all
        .iter()
        .filter(|s| s.user_id == uid && !s.deleted)
        .map(session_item)
        .collect();
    json!({
        "ok": true,
        "uid": uid,
        "label": workbuddy_accounts::label_for(uid),
        "count": sessions.len(),
        "sessions": sessions,
    })
}

/// 列出本机全部会话按 uid 分组（账号选择器和「未归属」排查用）。
///
/// 每个分组带 `label`：**界面上的账号一律用这个名字，不要再用 uid 前缀**。
/// 名字由 `workbuddy_accounts` 从运行日志 / 账号库 / 账号快照三处合并解析，
/// 解不出来时才退化成 `uid …尾号`。
pub fn list_by_account() -> Value {
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e, "accounts": [] }),
    };
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::BTreeMap<String, Vec<Value>> = std::collections::BTreeMap::new();
    for s in all.iter().filter(|s| !s.deleted) {
        if !map.contains_key(&s.user_id) {
            order.push(s.user_id.clone());
        }
        map.entry(s.user_id.clone())
            .or_default()
            .push(session_item(s));
    }
    let accounts: Vec<Value> = order
        .into_iter()
        .map(|uid| {
            let sessions = map.remove(&uid).unwrap_or_default();
            json!({
                "uid": uid,
                "label": workbuddy_accounts::label_for(&uid),
                "count": sessions.len(),
                "sessions": sessions,
            })
        })
        .collect();
    json!({ "ok": true, "accounts": accounts })
}

/// 会话详情：库里的行信息 + 正文解析出的回合。
pub fn detail(uid: &str, cid: &str) -> Value {
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let Some(row) = all.iter().find(|s| s.id == cid) else {
        return json!({ "ok": false, "error": format!("会话库里找不到会话 {cid}") });
    };
    if row.user_id != uid {
        return json!({
            "ok": false,
            "error": format!("会话 {cid} 属于账号 {}，与指定账号不一致", row.user_id)
        });
    }
    let body = workbuddy_source::load_body(cid);
    let mut out = json!({
        "ok": true,
        "session": session_item(row),
        "bodyPath": workbuddy_source::find_jsonl(cid).map(|p| p.to_string_lossy().to_string()),
    });
    match body {
        Ok(b) => {
            let turns: Vec<Value> = b
                .turns
                .iter()
                .map(|t| {
                    json!({
                        "userText": t.user_text,
                        "assistantText": t.assistant_text,
                        "createdAt": t.created_at,
                        "updatedAt": t.updated_at,
                        "events": t.events.len(),
                        "toolCalls": t.events.iter().filter(|e| e.kind == "function_call").count(),
                    })
                })
                .collect();
            out["aiTitle"] = json!(b.ai_title);
            out["turnCount"] = json!(turns.len());
            out["turns"] = json!(turns);
        }
        Err(e) => {
            out["bodyError"] = json!(e);
            out["turnCount"] = json!(0);
            out["turns"] = json!([]);
        }
    }
    out
}

/// 文件名净化：去掉 Windows 不允许的字符，空标题回落。
pub fn safe_file_name(title: &str) -> String {
    const BAD: [char; 11] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|', '\n', '\r'];
    let s: String = title.chars().filter(|c| !BAD.contains(c)).collect();
    let s = s.trim().to_string();
    if s.is_empty() {
        return "会话".to_string();
    }
    s.chars().take(60).collect()
}

/// 把会话正文渲染成 Markdown（纯函数，便于单测）。
pub fn render_markdown(
    title: &str,
    session_id: &str,
    turns: &[workbuddy_source::WbTurn],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("- 会话 id：`{session_id}`\n"));
    out.push_str(&format!("- 回合数：{}\n\n", turns.len()));
    for (i, t) in turns.iter().enumerate() {
        out.push_str(&format!("## 回合 {}\n\n", i + 1));
        out.push_str("### 提问\n\n");
        let ask = if t.user_text.trim().is_empty() {
            "(无正文)"
        } else {
            t.user_text.as_str()
        };
        out.push_str(ask);
        out.push_str("\n\n### 回答\n\n");
        let answer = if t.assistant_text.trim().is_empty() {
            "(无正文)"
        } else {
            t.assistant_text.as_str()
        };
        out.push_str(answer);
        out.push_str("\n\n");
        let tools: Vec<&workbuddy_source::WbEvent> = t
            .events
            .iter()
            .filter(|e| e.kind == "function_call")
            .collect();
        if !tools.is_empty() {
            out.push_str(&format!(
                "<details><summary>过程工具调用 {} 次</summary>\n\n",
                tools.len()
            ));
            for e in tools {
                if e.name.is_empty() {
                    out.push_str("- (未命名)\n");
                } else {
                    out.push_str(&format!("- `{}`\n", e.name));
                }
            }
            out.push_str("\n</details>\n\n");
        }
    }
    out
}

/// 导出单个会话为 Markdown，落到 `store/exports/workbuddy/`。
pub fn export_markdown(uid: &str, cid: &str) -> Result<Value, String> {
    let all = workbuddy_source::list_sessions()?;
    let row = all
        .iter()
        .find(|s| s.id == cid)
        .ok_or_else(|| format!("会话库里找不到会话 {cid}"))?;
    if row.user_id != uid {
        return Err(format!(
            "会话 {cid} 属于账号 {}，与指定账号不一致",
            row.user_id
        ));
    }
    if !row.has_body {
        return Err(format!("会话 {cid} 没有正文文件，无法导出"));
    }
    let body = workbuddy_source::load_body(cid)?;
    let title = body.ai_title.clone().unwrap_or_else(|| row.title.clone());
    let text = render_markdown(&title, cid, &body.turns);
    let dir = store_dir().join("exports").join("workbuddy");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let short: String = cid.chars().take(8).collect();
    let path = dir.join(format!("{}-{short}.md", safe_file_name(&title)));
    std::fs::write(&path, text.as_bytes()).map_err(|e| format!("写入导出文件失败: {e}"))?;
    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "bytes": text.len(),
        "turns": body.turns.len(),
        "title": title,
    }))
}

/// 批量删除某账号的会话（软删 + 正文进回收站）。
///
/// 安全链：校验归属 → 结束客户端 → 整份备份 → 事务内软删 → 写后校验 → 移动正文 → 重启。
/// 任何一步失败都返回 Err，且已做的数据库改动会在事务内回滚。
pub fn delete_sessions(uid: &str, ids: &[String]) -> Result<Value, String> {
    if uid.trim().is_empty() {
        return Err("账号缺少 uid，无法删除会话".to_string());
    }
    if ids.is_empty() {
        return Err("没有选择任何会话".to_string());
    }
    let db = db_path();
    assert_not_edge_sync(&db)?;

    // 1) 只读校验：全部会话必须属于该账号且未删除，任一不符就整批拒绝。
    let all = workbuddy_source::list_sessions()?;
    let mut targets: Vec<(String, String)> = Vec::new();
    for cid in ids {
        match all.iter().find(|s| s.id == *cid) {
            None => return Err(format!("会话库里找不到会话 {cid}，未做任何改动")),
            Some(s) if s.user_id != uid => {
                return Err(format!("会话 {cid} 不属于该账号，未做任何改动"))
            }
            Some(s) if s.deleted => return Err(format!("会话 {cid} 已删除，未做任何改动")),
            Some(s) => targets.push((s.id.clone(), s.title.clone())),
        }
    }

    // 2) 结束客户端（写库期间不允许客户端同时写入）。
    let was_running = workbuddy_switch::is_running();
    let killed = if was_running {
        let killed = workbuddy_switch::kill_all();
        workbuddy_switch::wait_until_stopped(15_000);
        killed
    } else {
        Vec::new()
    };

    // 3) 整份备份（一次，覆盖整批）。
    let backup = backup_db("delete")?;

    // 4) 事务内软删。
    let ts = now_ms();
    let relaunch = {
        let conn = open_rw(&db)?;
        if !table_exists(&conn, "sessions") {
            return Err("会话库里没有 sessions 表，未做任何改动".to_string());
        }
        let tx = conn.unchecked_transaction().map_err(|e| format!("开启事务失败: {e}"))?;
        for (cid, _) in &targets {
            let n = soft_delete_row(&conn, cid, uid, ts)?;
            if n != 1 {
                return Err(format!("会话 {cid} 删除未生效（影响 {n} 行），已回滚"));
            }
        }
        tx.commit().map_err(|e| format!("提交删除失败: {e}"))?;
        // 5) 写后校验：每行都必须已是 deleted_at 非空。
        for (cid, _) in &targets {
            if row_deleted_at(&conn, cid).is_none() {
                return Err(format!("会话 {cid} 删除后校验失败，请从备份恢复：{}", backup.display()));
            }
        }
        was_running
    };

    // 6) 正文进回收站（可还原），失败只报告不回滚数据库。
    let mut moved: Vec<Value> = Vec::new();
    let mut move_errors: Vec<String> = Vec::new();
    for (cid, title) in &targets {
        match move_body_to_trash(cid) {
            Ok(flag) => moved.push(json!({ "id": cid, "title": title, "bodyMoved": flag })),
            Err(e) => move_errors.push(format!("{cid}: {e}")),
        }
    }

    // 7) 原来在跑就重启。
    let relaunched = if relaunch {
        workbuddy_switch::launch().is_ok()
    } else {
        false
    };

    Ok(json!({
        "ok": true,
        "uid": uid,
        "deleted": moved,
        "count": moved.len(),
        "deletedAt": ts,
        "backup": backup.to_string_lossy(),
        "killed": killed,
        "relaunched": relaunched,
        "trashDir": trash_dir().to_string_lossy(),
        "moveErrors": move_errors,
    }))
}

// ---------------------------------------------------------------------------
// 二、会话复制
// ---------------------------------------------------------------------------

/// 目标正文路径：沿用源正文所在的工作区子目录。
///
/// 客户端按会话行的 `cwd` 推导工作区目录名，而 `cwd` 在复制时原样保留，
/// 所以目录名必须保持一致，否则客户端找不到正文。
pub(crate) fn target_body_path(source_path: &Path, new_cid: &str) -> PathBuf {
    let file_name = format!("{new_cid}.jsonl");
    let projects = workbuddy_auth::projects_dir();
    let relative = source_path
        .parent()
        .and_then(|p| p.strip_prefix(&projects).ok())
        .filter(|r| !r.as_os_str().is_empty());
    match relative {
        Some(r) => projects.join(r).join(file_name),
        None => projects.join(file_name),
    }
}

/// 正文里的会话 id 替换（纯函数）。
///
/// 只替换与源 id 完全相同的字符串。UUID 是 36 位带连字符，正文中出现同形串的概率
/// 为零；但为可测起见单独抽出，单测守住「不替换无关文本」。
pub fn replace_session_id(text: &str, cid: &str, new_cid: &str) -> String {
    text.replace(cid, new_cid)
}

/// 复制预检（只读）：归属、正文、客户端占用、是否已关联。
pub fn copy_preview(source_uid: &str, target_uid: &str, ids: &[String]) -> Value {
    if source_uid.trim().is_empty() || target_uid.trim().is_empty() {
        return json!({ "ok": false, "error": "源账号或目标账号缺少 uid" });
    }
    if source_uid == target_uid {
        return json!({ "ok": false, "error": "源账号与目标账号相同，无需复制" });
    }
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let store = load_links().unwrap_or_default();
    let app_running = workbuddy_switch::is_running();
    let current_uid = workbuddy_auth::current_uid();
    let blocked = app_running && current_uid.as_deref() == Some(target_uid);

    let mut items: Vec<Value> = Vec::new();
    for cid in ids {
        let Some(row) = all.iter().find(|s| s.id == *cid) else {
            items.push(json!({ "id": cid, "available": false, "reason": "会话库里找不到该会话" }));
            continue;
        };
        if row.user_id != source_uid {
            items.push(json!({ "id": cid, "available": false, "reason": "该会话不属于源账号" }));
            continue;
        }
        if row.deleted {
            items.push(json!({ "id": cid, "available": false, "reason": "该会话已删除" }));
            continue;
        }
        if !row.has_body {
            items.push(json!({ "id": cid, "available": false, "reason": "该会话没有正文文件，无法复制" }));
            continue;
        }
        let linked = find_group_for(&store, source_uid, cid)
            .and_then(|g| member_for(g, target_uid))
            .map(|m| m.session_id.clone());
        items.push(json!({
            "id": cid,
            "title": row.title,
            "available": true,
            "hasBody": row.has_body,
            "bodyBytes": row.body_bytes,
            "alreadyLinked": linked.is_some(),
            "targetSessionId": linked,
        }));
    }
    json!({
        "ok": true,
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "appRunning": app_running,
        "targetIsCurrent": current_uid.as_deref() == Some(target_uid),
        "blocked": blocked,
        "reason": if blocked { Some(TARGET_IN_USE) } else { None },
        "items": items,
    })
}

/// 把勾选的会话从源账号复制到目标账号（源账号数据一行不改）。
///
/// 流程：只读校验 → 占用门禁 → 整份备份 → 逐个（新 UUID → 写正文 → INSERT 行 → 校验）
/// → 登记关联 → 失败回滚本次写入。云端映射库始终不写。
/// 复制计划里的一项：源会话 id / 标题 / 磁盘上的正文路径。
pub(crate) struct CopyItem {
    pub cid: String,
    pub title: String,
    pub src_path: PathBuf,
}

/// 把一批「源账号 → 目标账号」的会话副本写进库，并登记关联组。
///
/// ⚠️ **手动复制（[`copy_sessions`]）与一键互通（[`link_all`]）共用这一条路径**：
/// 副本落盘、库行插入、归属校验、关联登记只要分叉，迟早出现「手动复制是对的、
/// 一键互通写出了半截」这种说不清的事故。
///
/// `done_rows` / `done_files` 是**回滚账本**，由调用方持有并跨批次累加 ——
/// 批量（多对账号）中途失败时能一次性撤掉本次全部写入。
fn write_copies(
    conn: &Connection,
    store: &mut LinkStore,
    source_uid: &str,
    target_uid: &str,
    plan: &[CopyItem],
    now: i64,
    alive: &std::collections::HashSet<(String, String)>,
    done_rows: &mut Vec<String>,
    done_files: &mut Vec<PathBuf>,
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut copied: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for item in plan {
        let cid = &item.cid;
        let src_row = read_source_row(conn, cid)?
            .ok_or_else(|| format!("会话 {cid} 在库里没有对应行"))?;
        // 已建立过关联、且那份副本**还在** → 跳过，不制造第二个副本。
        //
        // ⚠️⚠️ 判据是「成员记录 + 会话存活」，不是只看成员记录：
        //    只按记录判的话，副本被客户端清掉之后关联组里永远留着一条死成员，
        //    于是这一组**再也不会被补回来** —— 用户看到的就是「会话关联经常断开」。
        //    反过来，会话行还在、只是被软删（用户自己在客户端删的），仍然算「有成员」，
        //    不复活（否则「删除等于没删」，与自动导入的墓碑是同一个教训）。
        if let Some(existing) = find_group_for(store, source_uid, cid)
            .and_then(|g| member_for(g, target_uid))
            .map(|m| m.session_id.clone())
        {
            if alive.contains(&(target_uid.to_string(), existing.clone())) {
                skipped.push(json!({
                    "id": cid, "title": item.title, "reason": "目标账号已有副本",
                    "targetSessionId": existing,
                }));
                continue;
            }
        }
        let new_cid = uuid::Uuid::new_v4().to_string();
        let dest = target_body_path(&item.src_path, &new_cid);
        if dest.exists() {
            return Err(format!("目标正文已存在同名文件，已停止复制：{}", dest.display()));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("目标工作区目录创建失败: {e}"))?;
        }
        let text =
            std::fs::read_to_string(&item.src_path).map_err(|e| format!("读取源正文失败: {e}"))?;
        let text = replace_session_id(&text, cid, &new_cid);
        std::fs::write(&dest, text.as_bytes())
            .map_err(|e| format!("写入副本正文失败: {e}"))?;
        done_files.push(dest.clone());
        insert_copy_row(conn, &src_row, &new_cid, target_uid, now)?;
        done_rows.push(new_cid.clone());
        // 写后校验：新行必须存在、归属目标账号、未删除。
        match row_owner(conn, &new_cid) {
            Some(owner) if owner == target_uid => {}
            Some(owner) => {
                return Err(format!("副本归属校验失败：期望 {target_uid}，实际 {owner}"))
            }
            None => return Err("副本写入后不可见，未按成功处理".to_string()),
        }
        let group_id = upsert_group(store, source_uid, cid, target_uid, &new_cid, now);
        copied.push(json!({
            "sourceId": cid,
            "targetId": new_cid,
            "title": item.title,
            "bodyPath": dest.to_string_lossy(),
            "groupId": group_id,
        }));
    }
    Ok((copied, skipped))
}

// ---------------------------------------------------------------------------
// 三·三、已关联副本的内容同步（纯追加，零覆盖）
// ---------------------------------------------------------------------------

/// 比对时替代「本副本自己的会话 id」的标记 —— 与内容摘要用的是同一套归一化思路。
const SELF_ID_MARK: &str = "__wb_copy_self_id__";

/// 一份正文相对另一份的关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyRelation {
    /// 逐行相同（归一化后）。
    Identical,
    /// 目标正文是源正文的**有序前缀** —— 纯追加，可以安全覆盖成源那份。
    Prefix,
    /// 有分歧（目标有源没有的内容，或中途不同）—— 一律不动。
    Diverge,
}

/// 逐行比对两份正文（把各自的会话 id 归一化后再比），**流式读，不整份进内存**。
fn body_relation(
    src: &Path,
    src_id: &str,
    tgt: &Path,
    tgt_id: &str,
) -> Result<BodyRelation, String> {
    use std::io::{BufRead, BufReader};
    let sf = std::fs::File::open(src).map_err(|e| format!("读源正文失败：{e}"))?;
    let tf = std::fs::File::open(tgt).map_err(|e| format!("读目标正文失败：{e}"))?;
    let mut s_lines = BufReader::new(sf).lines();
    let mut t_lines = BufReader::new(tf).lines();
    loop {
        let s = s_lines.next();
        let t = t_lines.next();
        match (s, t) {
            // 两边同时到底 → 完全一致。
            (None, None) => return Ok(BodyRelation::Identical),
            // 目标还没到底、源先没了 → 目标更长（有多余内容），不能覆盖。
            (None, Some(_)) => return Ok(BodyRelation::Diverge),
            // 目标到底了、源还有 → 目标是被源包含的前缀。
            (Some(_), None) => return Ok(BodyRelation::Prefix),
            (Some(s), Some(t)) => {
                let s = s.map_err(|e| format!("读源正文失败：{e}"))?;
                let t = t.map_err(|e| format!("读目标正文失败：{e}"))?;
                if s.replace(src_id, SELF_ID_MARK) != t.replace(tgt_id, SELF_ID_MARK) {
                    return Ok(BodyRelation::Diverge);
                }
            }
        }
    }
}

/// 把源正文按「脚本 id 换成目标 id」写到目标位置（先写临时文件再原子替换）。
fn overwrite_body_from(src: &Path, src_id: &str, tgt: &Path, tgt_id: &str) -> Result<u64, String> {
    use std::io::{BufRead, BufReader, BufWriter, Write};
    let tmp = tgt.with_extension("jsonl.wbtmp");
    let mut out =
        BufWriter::new(std::fs::File::create(&tmp).map_err(|e| format!("创建临时正文失败：{e}"))?);
    let mut lines = 0u64;
    for line in BufReader::new(
        std::fs::File::open(src).map_err(|e| format!("读源正文失败：{e}"))?,
    )
    .lines()
    {
        let line = line.map_err(|e| format!("读源正文失败：{e}"))?;
        out.write_all(line.replace(src_id, tgt_id).as_bytes())
            .map_err(|e| format!("写入临时正文失败：{e}"))?;
        out.write_all(b"\n")
            .map_err(|e| format!("写入临时正文失败：{e}"))?;
        lines += 1;
    }
    out.flush().map_err(|e| format!("写入临时正文失败：{e}"))?;
    drop(out);
    std::fs::rename(&tmp, tgt).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换目标正文失败：{e}")
    })?;
    Ok(lines)
}

/// 把源行的可变字段刷到目标副本行上（**不动 id / user_id / deleted_at**）。
fn update_copy_row(
    conn: &Connection,
    source: &[(String, SqlValue)],
    target_cid: &str,
    target_uid: &str,
    now: i64,
) -> Result<(), String> {
    // 只刷「内容相关」的字段：标题跟着内容走，时间是这次写入的时间。
    // ⚠️ 不刷 cwd —— 目标副本的正文就放在它自己的工作区目录里，改 cwd 会让它找不到正文。
    let mut sets: Vec<String> = vec!["updated_at = ?1".to_string()];
    let mut vals: Vec<SqlValue> = vec![SqlValue::Integer(now)];
    for col in ["title", "custom_title", "model"] {
        if let Some((_, v)) = source.iter().find(|(name, _)| name == col) {
            sets.push(format!("{col} = ?{}", vals.len() + 1));
            vals.push(v.clone());
        }
    }
    let id_at = vals.len() + 1;
    let uid_at = vals.len() + 2;
    vals.push(SqlValue::Text(target_cid.to_string()));
    vals.push(SqlValue::Text(target_uid.to_string()));
    let sql = format!(
        "UPDATE sessions SET {} WHERE id = ?{id_at} AND user_id = ?{uid_at}",
        sets.join(", ")
    );
    let params: Vec<&SqlValue> = vals.iter().collect();
    conn.execute(&sql, rusqlite::params_from_iter(params))
        .map_err(|e| format!("更新副本行失败：{e}"))?;
    Ok(())
}

/// 对**已经关联**的副本做内容同步：只在「目标正文是源正文前缀」时整份改写。
///
/// 返回 `(已同步, 跳过)`；跳过项都带 `reason`，界面照实显示「为什么没动」——
/// 分歧、正文缺失、读失败都不许静默咽掉。
#[allow(clippy::type_complexity)]
fn refresh_linked_copies(
    conn: &Connection,
    pairs: &[(String, String, PathBuf, String)],
    target_uid: &str,
    now: i64,
    backup_dir: &Path,
    done: &mut Vec<(PathBuf, PathBuf)>,
) -> (Vec<Value>, Vec<Value>) {
    let mut refreshed: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for (src_cid, src_title, src_path, tgt_cid) in pairs {
        let tgt_path = target_body_path(src_path, tgt_cid);
        if !tgt_path.is_file() {
            skipped.push(json!({
                "id": src_cid, "title": src_title, "targetSessionId": tgt_cid,
                "reason": "目标副本正文不在磁盘上，未做内容同步",
            }));
            continue;
        }
        let relation = match body_relation(src_path, src_cid, &tgt_path, tgt_cid) {
            Ok(r) => r,
            Err(e) => {
                skipped.push(json!({
                    "id": src_cid, "title": src_title, "targetSessionId": tgt_cid, "reason": e,
                }));
                continue;
            }
        };
        match relation {
            BodyRelation::Identical => {
                skipped.push(json!({
                    "id": src_cid, "title": src_title, "targetSessionId": tgt_cid,
                    "reason": "两边内容已经一致",
                }));
            }
            BodyRelation::Diverge => {
                skipped.push(json!({
                    "id": src_cid, "title": src_title, "targetSessionId": tgt_cid,
                    "reason": "内容有分歧（目标账号里也有新内容），未自动覆盖",
                }));
            }
            BodyRelation::Prefix => {
                // 覆盖前先留一份旧正文，回滚时按 (目标, 备份) 还原。
                let backup = backup_dir.join(format!("{tgt_cid}.jsonl"));
                if let Err(e) = std::fs::create_dir_all(backup_dir)
                    .and_then(|_| std::fs::copy(&tgt_path, &backup))
                {
                    skipped.push(json!({
                        "id": src_cid, "title": src_title, "targetSessionId": tgt_cid,
                        "reason": format!("备份目标正文失败，未做同步：{e}"),
                    }));
                    continue;
                }
                match overwrite_body_from(src_path, src_cid, &tgt_path, tgt_cid) {
                    Ok(lines) => {
                        done.push((tgt_path.clone(), backup.clone()));
                        let src_row = read_source_row(conn, src_cid).ok().flatten();
                        if let Some(row) = src_row {
                            if let Err(e) =
                                update_copy_row(conn, &row, tgt_cid, target_uid, now)
                            {
                                skipped.push(json!({
                                    "id": src_cid, "title": src_title, "targetSessionId": tgt_cid,
                                    "reason": e,
                                }));
                                continue;
                            }
                        }
                        refreshed.push(json!({
                            "id": src_cid,
                            "targetSessionId": tgt_cid,
                            "title": src_title,
                            "lines": lines,
                            "backup": backup.to_string_lossy(),
                        }));
                    }
                    Err(e) => skipped.push(json!({
                        "id": src_cid, "title": src_title, "targetSessionId": tgt_cid, "reason": e,
                    })),
                }
            }
        }
    }
    (refreshed, skipped)
}

pub fn copy_sessions(source_uid: &str, target_uid: &str, ids: &[String]) -> Result<Value, String> {
    if source_uid.trim().is_empty() || target_uid.trim().is_empty() {
        return Err("源账号或目标账号缺少 uid，无法复制会话".to_string());
    }
    if source_uid == target_uid {
        return Err("源账号与目标账号相同，无需复制会话".to_string());
    }
    if ids.is_empty() {
        return Err("没有选择任何会话".to_string());
    }
    let db = db_path();
    assert_not_edge_sync(&db)?;

    // 1) 只读校验（用快照，避免与运行中的客户端争 WAL）。
    let all = workbuddy_source::list_sessions()?;
    let snap = workbuddy_source::snapshot_db()?;
    let snap_conn = open_ro(&snap).ok_or_else(|| "会话快照无法打开".to_string())?;
    if !table_exists(&snap_conn, "sessions") {
        return Err("会话库里没有 sessions 表".to_string());
    }
    let mut plan: Vec<CopyItem> = Vec::new();
    for cid in ids {
        let row = all
            .iter()
            .find(|s| s.id == *cid)
            .ok_or_else(|| format!("会话库里找不到会话 {cid}，未做任何改动"))?;
        if row.user_id != source_uid {
            return Err(format!("会话 {cid} 不属于源账号，未做任何改动"));
        }
        if row.deleted {
            return Err(format!("会话 {cid} 已删除，未做任何改动"));
        }
        let path = workbuddy_source::find_jsonl(cid)
            .ok_or_else(|| format!("会话 {cid} 没有正文文件，未做任何改动"))?;
        let src_row = read_source_row(&snap_conn, cid)?
            .ok_or_else(|| format!("会话 {cid} 在库里没有对应行，未做任何改动"))?;
        if src_row
            .iter()
            .find(|(c, _)| c == "user_id")
            .and_then(|(_, v)| match v {
                SqlValue::Text(t) => Some(t.clone()),
                _ => None,
            })
            .as_deref()
            != Some(source_uid)
        {
            return Err(format!("会话 {cid} 的归属与源账号不一致，未做任何改动"));
        }
        plan.push(CopyItem {
            cid: cid.clone(),
            title: row.title.clone(),
            src_path: path,
        });
        let _ = src_row; // 写入时从真实库重读，避免快照与实时库不一致
    }
    drop(snap_conn);

    // 2) 占用门禁：目标账号正被客户端使用时禁止写。
    if workbuddy_switch::is_running() && workbuddy_auth::current_uid().as_deref() == Some(target_uid)
    {
        return Err(TARGET_IN_USE.to_string());
    }

    // 3) 整份备份（一次覆盖整批）。
    let backup = backup_db("copy")?;

    // 4) 逐个复制；任何失败都回滚本次写入。
    let now = now_ms();
    let conn = open_rw(&db)?;
    let mut done_rows: Vec<String> = Vec::new();
    let mut done_files: Vec<PathBuf> = Vec::new();
    // 手动复制沿用老语义：只要目标账号那一行还在（哪怕是软删），就当作「已有副本」跳过。
    // 与 `link_all` 的「死成员允许重建」不同步是故意的 —— 手动点“复制”不该悄悄复活
    // 用户自己删掉的会话。
    let alive_keys: std::collections::HashSet<(String, String)> = all
        .iter()
        .map(|s| (s.user_id.clone(), s.id.clone()))
        .collect();
    let result = (|| -> Result<(Vec<Value>, Vec<Value>), String> {
        let mut store = load_links().unwrap_or_default();
        let out = write_copies(
            &conn,
            &mut store,
            source_uid,
            target_uid,
            &plan,
            now,
            &alive_keys,
            &mut done_rows,
            &mut done_files,
        )?;
        save_links(&store)?;
        Ok(out)
    })();

    let (copied, skipped) = match result {
        Ok(v) => v,
        Err(e) => {
            // 回滚：只撤销本次写入（删新行 + 删新正文），备份保留供人工恢复。
            let mut rolled: Vec<String> = Vec::new();
            for cid in &done_rows {
                let _ = conn.execute("DELETE FROM sessions WHERE id = ?1", [cid]);
                rolled.push(cid.clone());
            }
            for f in &done_files {
                let _ = std::fs::remove_file(f);
            }
            return Err(format!(
                "{e}（已回滚本次复制的 {} 条新记录，备份保留在 {}）",
                rolled.len(),
                backup.display()
            ));
        }
    };

    Ok(json!({
        "ok": true,
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "copied": copied,
        "skipped": skipped,
        "count": copied.len(),
        "backup": backup.to_string_lossy(),
        "edgeSyncSkipped": edge_sync_databases(),
        "note": "云端映射库未做任何写入（写入会导致 edge-sync 跳过上传、云端缺会话）",
    }))
}

/// **多目标**复制：一次把源账号的若干会话复制到多个目标账号。
///
/// 为什么不让前端循环调 `copy_sessions`：
/// * 每个目标各备份一次整库、各开关一次写连接 —— N 个目标就 N 次，明显慢；
/// * 任一个目标失败时，前端拿不到「哪些成了、哪些没成」的统一结论，只能拼字符串。
///
/// 这里逐个目标跑同一套 `copy_sessions`（它内部已有校验 / 备份 / 回滚），
/// 但**每个目标独立成败**：一个目标失败不中断其余，结果按目标逐条汇总。
/// 所以 `ok` 是「有没有目标成功」，不是「全部成功」—— 界面必须分别显示。
pub fn copy_sessions_multi(
    source_uid: &str,
    target_uids: &[String],
    ids: &[String],
    close_client: bool,
) -> Result<Value, String> {
    if source_uid.trim().is_empty() {
        return Err("缺少源账号 uid".to_string());
    }
    if target_uids.is_empty() {
        return Err("没有选择任何目标账号".to_string());
    }
    if ids.is_empty() {
        return Err("没有选择任何会话".to_string());
    }
    // 去重并剔除源账号自己（源=目标复制没有意义，且 copy_sessions 会直接报错）。
    let mut seen: Vec<String> = Vec::new();
    for u in target_uids {
        let u = u.trim();
        if u.is_empty() || u == source_uid || seen.iter().any(|x| x == u) {
            continue;
        }
        seen.push(u.to_string());
    }
    if seen.is_empty() {
        return Err("目标账号里没有可复制的（不能等于源账号）".to_string());
    }

    // ⚠️ 客户端占用门禁：目标账号正被客户端使用时 `copy_sessions` 会直接拒绝
    // （`TARGET_IN_USE`）。用户在确认框里点过「确定」= 同意我们关它，
    // 所以这里先退出客户端、复制完**再由我们自己开回来**（谁关的谁负责开）。
    // 没关过就不启动 —— 不能因为一次复制就凭空多开一个客户端。
    let stopped = crate::modules::workbuddy_session_groups::ensure_closed(close_client)?;

    let outcome = copy_multi_run(source_uid, &seen, ids);

    let (relaunched, restart_err) = crate::modules::workbuddy_session_groups::restart_client(stopped);
    let mut out = outcome?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("clientStopped".into(), json!(stopped));
        obj.insert("clientRelaunched".into(), json!(relaunched));
        if let Some(e) = restart_err {
            obj.insert("restartError".into(), json!(e));
        }
    }
    Ok(out)
}

/// [`copy_sessions_multi`] 的纯复制部分（不含客户端开关）。
///
/// 单独拆出来是为了让「关客户端 → 复制 → 开回来」这段收尾**无论成败都执行**：
/// 早期版本把重启写在成功分支里，中途报错就把客户端关在外面没人管。
fn copy_multi_run(source_uid: &str, seen: &[String], ids: &[String]) -> Result<Value, String> {
    let mut results: Vec<Value> = Vec::new();
    let mut ok_count = 0usize;
    let mut total_copied = 0usize;
    for uid in seen {
        match copy_sessions(source_uid, uid, ids) {
            Ok(v) => {
                ok_count += 1;
                total_copied += v.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
                results.push(json!({
                    "targetUid": uid,
                    "ok": true,
                    "count": v.get("count").cloned().unwrap_or(json!(0)),
                    "copied": v.get("copied").cloned().unwrap_or(json!([])),
                    "skipped": v.get("skipped").cloned().unwrap_or(json!([])),
                    "backup": v.get("backup").cloned().unwrap_or(Value::Null),
                }));
            }
            Err(e) => results.push(json!({
                "targetUid": uid,
                "ok": false,
                "error": e,
            })),
        }
    }
    Ok(json!({
        "ok": ok_count > 0,
        // ⚠️ 三个数字必须同源：okCount 是成功的目标数，count 是成功的那些目标里
        //    复制成功的会话条数合计 —— 界面不能自己再算一遍，否则会出现两个数对不上。
        "okCount": ok_count,
        "targetCount": seen.len(),
        "count": total_copied,
        "sourceUid": source_uid,
        "targets": results,
    }))
}

// ---------------------------------------------------------------------------
// 三、跨账号关联
// ---------------------------------------------------------------------------

/// 关联组里的一个成员：某账号上的某一个会话副本。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkMember {
    pub uid: String,
    pub session_id: String,
    pub linked_at: i64,
}

/// 一个逻辑会话的关联组：同一段对话在各账号上的副本归入同一组。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkGroup {
    pub id: String,
    pub created_at: i64,
    #[serde(default)]
    pub members: Vec<LinkMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LinkStore {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub groups: Vec<LinkGroup>,
}

fn links_file() -> PathBuf {
    store_dir().join("workbuddy-session-links.json")
}

/// 读关联存储；损坏时返回 Err（绝不降级成空表——那样会制造第二个副本）。
pub(crate) fn load_links() -> Result<LinkStore, String> {
    let path = links_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LinkStore::default()),
        Err(e) => return Err(format!("关联记录无法读取: {e}")),
    };
    let mut store: LinkStore =
        serde_json::from_str(&text).map_err(|_| "关联记录已损坏，原文件已保留".to_string())?;
    if store.version != LINK_STORE_VERSION {
        return Err(format!(
            "关联记录版本 {} 不受支持（当前支持 {}），原文件已保留",
            store.version, LINK_STORE_VERSION
        ));
    }
    store.version = LINK_STORE_VERSION;
    Ok(store)
}

pub(crate) fn save_links(store: &LinkStore) -> Result<(), String> {
    let path = links_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建关联存储目录失败: {e}"))?;
    }
    let text = serde_json::to_string_pretty(store).map_err(|e| format!("序列化关联记录失败: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| format!("写入关联记录失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换关联记录失败: {e}"))?;
    Ok(())
}

fn find_group_for<'a>(store: &'a LinkStore, uid: &str, cid: &str) -> Option<&'a LinkGroup> {
    store
        .groups
        .iter()
        .find(|g| g.members.iter().any(|m| m.uid == uid && m.session_id == cid))
}

fn member_for<'a>(group: &'a LinkGroup, uid: &str) -> Option<&'a LinkMember> {
    group.members.iter().find(|m| m.uid == uid)
}

/// 登记 / 更新关联组（纯逻辑，便于单测）。
///
/// 源会话已有组 → 复用组并替换目标账号的旧成员；否则新建组。
/// 返回组 id。
pub(crate) fn upsert_group(
    store: &mut LinkStore,
    source_uid: &str,
    source_cid: &str,
    target_uid: &str,
    target_cid: &str,
    now: i64,
) -> String {
    store.version = LINK_STORE_VERSION;
    if let Some(group) = store.groups.iter_mut().find(|g| {
        g.members
            .iter()
            .any(|m| m.uid == source_uid && m.session_id == source_cid)
    }) {
        group.members.retain(|m| m.uid != target_uid);
        group.members.push(LinkMember {
            uid: target_uid.to_string(),
            session_id: target_cid.to_string(),
            linked_at: now,
        });
        return group.id.clone();
    }
    let id = uuid::Uuid::new_v4().to_string();
    store.groups.push(LinkGroup {
        id: id.clone(),
        created_at: now,
        members: vec![
            LinkMember {
                uid: source_uid.to_string(),
                session_id: source_cid.to_string(),
                linked_at: now,
            },
            LinkMember {
                uid: target_uid.to_string(),
                session_id: target_cid.to_string(),
                linked_at: now,
            },
        ],
    });
    id
}

/// 关联视图：源账号与目标账号之间已建立的会话副本关系。
///
/// `verdict`：`linked`（两边都在）/ `targetMissing`（目标没有副本，可补复制）/
/// `sourceMissing`（源会话已失效）/ `gone`（两边都不在）。
pub fn links_preview(source_uid: &str, target_uid: &str) -> Value {
    let store = match load_links() {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "storeStatus": "unavailable", "error": e }),
    };
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let info = |cid: &str| -> Value {
        match all.iter().find(|s| s.id == cid) {
            None => json!({ "sessionId": cid, "alive": false, "title": Value::Null, "updatedAt": 0, "hasBody": false }),
            Some(s) => json!({
                "sessionId": cid,
                // 已软删或归属不符都算失效：客户端看不到它，副本也就无从谈起。
                "alive": !s.deleted,
                "ownedBy": s.user_id,
                "title": s.title,
                "updatedAt": s.updated_at,
                "hasBody": s.has_body,
            }),
        }
    };
    let mut groups: Vec<Value> = Vec::new();
    for g in store.groups.iter() {
        let (Some(src), Some(dst)) = (member_for(g, source_uid), member_for(g, target_uid)) else {
            continue;
        };
        let s = info(&src.session_id);
        let t = info(&dst.session_id);
        let src_alive = s["alive"].as_bool().unwrap_or(false)
            && s["ownedBy"].as_str().unwrap_or("") == source_uid;
        let dst_alive = t["alive"].as_bool().unwrap_or(false)
            && t["ownedBy"].as_str().unwrap_or("") == target_uid;
        let verdict = match (src_alive, dst_alive) {
            (true, true) => "linked",
            (true, false) => "targetMissing",
            (false, true) => "sourceMissing",
            (false, false) => "gone",
        };
        groups.push(json!({
            "groupId": g.id,
            "title": if src_alive { s["title"].clone() } else { t["title"].clone() },
            "source": s,
            "target": t,
            "verdict": verdict,
            "canCopy": src_alive && !dst_alive,
            "defaultChecked": src_alive && !dst_alive,
            "linkedAt": dst.linked_at,
        }));
    }
    groups.sort_by(|a, b| {
        b["source"]["updatedAt"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["source"]["updatedAt"].as_i64().unwrap_or(0))
    });
    json!({
        "ok": true,
        "storeStatus": "ready",
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "count": groups.len(),
        "groups": groups,
        "storePath": links_file().to_string_lossy(),
    })
}

// ---------------------------------------------------------------------------
// 三·二、切号时的会话勾选（列出当前账号全部会话 + 与目标账号的关联状态）
// ---------------------------------------------------------------------------

/// 一条候选会话在「切到目标账号」时的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchLinkState {
    /// 目标账号有存活副本 → 默认勾选（勾上 = 把内容同步过去）。
    Linked,
    /// 关联组里记着目标成员，但那份副本已经不在了 → 默认勾选（勾上 = 补一份回来）。
    Missing,
    /// 没有关联记录，但目标账号里已有一份同内容副本 → 默认勾选（勾上 = 只登记关联，0 字节）。
    Twin,
    /// 完全没有关联 → 默认不勾（勾上才新建副本 + 关联）。
    Unlinked,
}

impl SwitchLinkState {
    pub fn key(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Missing => "missing",
            Self::Twin => "twin",
            Self::Unlinked => "unlinked",
        }
    }
    /// 「已有关联」的三种都默认勾上；只有完全没关联的留给用户主动新增。
    pub fn default_checked(self) -> bool {
        !matches!(self, Self::Unlinked)
    }
}

/// 默认勾选的**唯一出口**。
///
/// 三条同时成立才默认勾上：① 状态本身默认勾（已关联）；② 有正文（没正文复制不了）；
/// ③ **不是同标题的较旧副本**。
///
/// 第 ③ 条是「切号后会话重复出现多次」的根因修复：WorkBuddy 的定时任务（「每日 AI 推送」）
/// 每次运行都新建一个同名会话，一个账号里能有 5 份。以前这些全被默认勾上，切一次号就把
/// 5 份副本一起搬过去，目标账号里越积越多。现在只勾最新那份，旧的标出来、想同步可手动勾。
fn default_checked(state: SwitchLinkState, has_body: bool, dup_older: bool) -> bool {
    state.default_checked() && has_body && !dup_older
}

/// 每个重复组里**本账号内**最新的一条（组键 → 会话 id），按 `updated_at` 取最大。
///
/// ⚠️ 不能用 [`workbuddy_source::WbSession::is_newest`]：它是**跨账号的全局标记**
///    （同一段对话在多账号各有一份，全局只有一份最新）。真机实测账号 e925618e /
///    fbd4139a 的「每日 AI 新闻推送」组内全局标记全为 false —— 直接拿它判「较旧」，
///    会导致整组都不默认勾选，用户以为关联失效。这里按本账号时间取代表，保证每组留一条。
///
/// 时间相同时取**先出现**的那条（列表本身已按更新时间倒序，语义稳定、不抖动）。
fn newest_by_group(items: &[(&str, &str, i64)]) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, (i64, String)> =
        std::collections::HashMap::new();
    for (group, id, at) in items {
        if group.is_empty() {
            continue;
        }
        match out.get(*group) {
            Some((prev, _)) if *at <= *prev => {}
            _ => {
                out.insert((*group).to_string(), (*at, (*id).to_string()));
            }
        }
    }
    out.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

/// 切号前给界面用的会话清单（只读）。
///
/// 每一行是**当前账号**的一个会话，带上「与目标账号的关联状态」。界面据此决定默认勾选：
/// 已关联的默认勾上（可取消 = 这次不同步），没关联的默认空着（勾上 = 新增关联）。
///
/// ⚠️ 关联状态的判据与 [`plan_links`] 完全一致：**成员记录 + 会话存活**。
/// 两边不一致会出现「界面说已关联、实际却补了一份副本」这种自相矛盾。
pub fn switch_plan(target_uid: &str) -> Value {
    let source_uid = workbuddy_auth::current_uid().unwrap_or_default();
    if source_uid.trim().is_empty() {
        return json!({ "ok": false, "error": "读不到当前登录账号，无法列出会话" });
    }
    if target_uid.trim().is_empty() || target_uid == source_uid {
        return json!({ "ok": false, "error": "目标账号无效（为空或与当前账号相同）" });
    }
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let store = match load_links() {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "storeStatus": "unavailable", "error": e }),
    };
    let is_alive = |uid: &str, cid: &str| -> bool {
        all.iter()
            .any(|t| t.id == cid && !t.deleted && t.user_id == uid)
    };

    // 同标题重复组在**源账号范围内**的份数与组内最新。
    //
    // 为什么需要：WorkBuddy 的定时任务每次运行都新建一个会话、标题相同（「每日 AI 推送」），
    // 于是一个号里会出现好几条同名会话。早前这里把每条都平铺出来并默认勾选，
    // 切一次号就把同一段对话的 N 份副本全搬过去 —— 用户看到的就是「复制后重复出现多次」。
    // 现在把重复组信息一并给界面：默认只勾最新那一份，旧的标出来但不勾（想同步可手动勾）。
    //
    // ⚠️ 「最新」必须按**源账号内**的更新时间判断。`WbSession::is_newest` 是全局标记
    //    （同一段对话在多账号各有一份，全局只有一份最新）；真机实测账号 e925618e /
    //    fbd4139a 的「每日 AI 新闻推送」组内 `is_newest` 全为 false —— 直接用它会导致
    //    **整组都不默认勾选**，用户以为关联失效了。
    let source_rows: Vec<&workbuddy_source::WbSession> = all
        .iter()
        .filter(|s| s.user_id == source_uid && !s.deleted)
        .collect();
    let mut group_size: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for s in &source_rows {
        if !s.dup_group.is_empty() {
            *group_size.entry(s.dup_group.as_str()).or_insert(0) += 1;
        }
    }
    let newest_here = newest_by_group(
        &source_rows
            .iter()
            .map(|s| (s.dup_group.as_str(), s.id.as_str(), s.updated_at))
            .collect::<Vec<_>>(),
    );

    let mut rows: Vec<Value> = Vec::new();
    let mut linked = 0usize;
    let mut missing = 0usize;
    let mut twin_n = 0usize;
    let mut unlinked = 0usize;
    let mut dup_olders = 0usize;
    let mut dup_skipped = 0usize;
    for s in all
        .iter()
        .filter(|s| s.user_id == source_uid && !s.deleted)
    {
        let member_cid = find_group_for(&store, &source_uid, &s.id)
            .and_then(|g| member_for(g, target_uid))
            .map(|m| m.session_id.clone());
        let target_alive = member_cid
            .as_deref()
            .map(|cid| is_alive(target_uid, cid))
            .unwrap_or(false);
        // 没有关联记录时，看目标账号里有没有「同一段对话」的副本（摘要 + 标题都相同）。
        let twin_cid = if member_cid.is_none() && !s.content_digest.is_empty() {
            all.iter()
                .find(|t| {
                    t.user_id == target_uid
                        && !t.deleted
                        && t.title == s.title
                        && t.content_digest == s.content_digest
                })
                .map(|t| t.id.clone())
        } else {
            None
        };
        let state = match (member_cid.is_some(), target_alive, twin_cid.is_some()) {
            (true, true, _) => SwitchLinkState::Linked,
            (true, false, _) => SwitchLinkState::Missing,
            (false, _, true) => SwitchLinkState::Twin,
            _ => SwitchLinkState::Unlinked,
        };
        match state {
            SwitchLinkState::Linked => linked += 1,
            SwitchLinkState::Missing => missing += 1,
            SwitchLinkState::Twin => twin_n += 1,
            SwitchLinkState::Unlinked => unlinked += 1,
        }
        let peers = if s.dup_group.is_empty() {
            1
        } else {
            group_size.get(s.dup_group.as_str()).copied().unwrap_or(1)
        };
        // 「本账号内的组内最新」：只有同组确实有多份时才有意义（唯一标题恒 true）。
        let is_newest_here = if peers <= 1 {
            true
        } else {
            newest_here
                .get(s.dup_group.as_str())
                .map(|id| id == &s.id)
                .unwrap_or(true)
        };
        let dup_older = peers > 1 && !is_newest_here;
        if dup_older {
            dup_olders += 1;
        }
        let default_checked_row = default_checked(state, s.has_body, dup_older);
        // 记下「本来会被默认勾上、因为重复被去掉」的条数：界面要能说清少勾了几条，
        // 不能让用户以为是我们算漏了。
        if dup_older && s.has_body && state.default_checked() {
            dup_skipped += 1;
        }
        rows.push(json!({
            "sessionId": s.id,
            "title": s.title,
            "updatedAt": s.updated_at,
            "hasBody": s.has_body,
            "bodyBytes": s.body_bytes,
            "cwd": s.cwd,
            "state": state.key(),
            "defaultChecked": default_checked_row,
            // 没有正文的会话复制不了（复制只搬正文 + 库行），如实告诉界面，别让它勾了没反应。
            "disabled": !s.has_body,
            "targetSessionId": member_cid.clone().or(twin_cid),
            // 同标题重复组（`dupGroup` 为空 = 标题唯一）。`dupRelation`：
            // `latest` 组内最新；`equal` 与最新内容逐行一致；`diverged` 内容不同。
            "dupGroup": s.dup_group,
            "dupGroupSize": peers,
            // 本账号口径的「组内最新」，界面据此折叠/画徽标（不要用全局 is_newest）。
            "isNewest": is_newest_here,
            "dupRelation": s.dup_relation,
            "dupNote": s.dup_note,
        }));
    }

    // `checked` 必须与逐行的 `defaultChecked` 同源（不是 `linked+missing+twin`）：
    // 排除同标题较旧副本之后，这两个数会不一样，写死了就会出现「说 4 条、实际只勾 3 条」。
    let checked = rows
        .iter()
        .filter(|r| r["defaultChecked"].as_bool().unwrap_or(false))
        .count();
    let dup_groups = group_size.values().filter(|&&n| n > 1).count();

    json!({
        "ok": true,
        "storeStatus": "ready",
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "counts": {
            "total": rows.len(),
            "linked": linked,
            "missing": missing,
            "twin": twin_n,
            "unlinked": unlinked,
            "checked": checked,
            // 同标题重复：组数 / 较旧副本条数 / 其中「本来会勾、因重复被跳过」的条数。
            "dupGroups": dup_groups,
            "dupOlders": dup_olders,
            "dupSkipped": dup_skipped,
        },
        "sessions": rows,
        "storePath": links_file().to_string_lossy(),
    })
}

/// 一键互通的参数。
///
/// ⚠️ 不再 `Copy`：勾选同步要带一串会话 id，`Vec` 不是 `Copy`。
#[derive(Debug, Clone)]
pub struct LinkOptions {
    /// 客户端正在运行就拒绝（界面入口 `true`；切号流程 `false`，那时已确认退出）。
    pub require_closed: bool,
    /// 是否**复制**缺失副本。`false` = 只登记已有副本（0 字节、完全不碰会话库）。
    pub copy_missing: bool,
    /// 只复制这个时刻之后**创建**的会话（`0` = 不限制）。切号自动同步用它做增量。
    pub since: i64,
    /// 只处理这些**源会话 id**（切号时用户勾选的那批）。`None` = 不限制。
    ///
    /// 未列出的源会话完全不参与：不复制、不改写、不登记 —— 这就是「取消勾选 = 不同步」。
    pub only_sessions: Option<Vec<String>>,
    /// 只同步到这个账号。`None` = 所有可用账号（一键互通的语义）。
    pub target_uid: Option<String>,
    /// 对**已经关联**的副本做内容同步（切号勾选时为 `true`）。
    ///
    /// 只做「目标正文是源正文的归一化前缀」这一种安全情形（纯追加，零覆盖）；
    /// 有分歧的一律不动，如实回报「内容有分歧，未自动覆盖」。
    pub refresh_linked: bool,
}

impl LinkOptions {
    /// 只带三件必需参数的老用法（其余取默认）。
    pub fn new(require_closed: bool, copy_missing: bool, since: i64) -> Self {
        Self {
            require_closed,
            copy_missing,
            since,
            only_sessions: None,
            target_uid: None,
            refresh_linked: false,
        }
    }
}

/// 关联组成员登记：目标账号**已有同一段对话的副本**，只需登记，不必复制。
pub(crate) struct LinkAdopt {
    pub src_uid: String,
    pub src_cid: String,
    pub tgt_uid: String,
    pub tgt_cid: String,
}

/// 一键互通的目标账号集合 —— [`link_all`] 与 [`link_all_preview`] **同源**。
///
/// 只认账号库里**未禁用**的账号：用户删掉 / 停用的账号不该再被写入数据。
/// 一个可用的都没有时退回「会话库里出现过的 uid」，至少别把功能说成不可用。
fn link_target_uids(all: &[workbuddy_source::WbSession]) -> Vec<String> {
    let mut uids: Vec<String> = Vec::new();
    for acc in workbuddy_vault::load_accounts() {
        if acc.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        if let Some(uid) = workbuddy_auth::get_str(&acc, "uid") {
            if !uid.trim().is_empty() {
                uids.push(uid);
            }
        }
    }
    if uids.len() < 2 {
        uids = all
            .iter()
            .filter(|s| !s.deleted && !s.user_id.is_empty())
            .map(|s| s.user_id.clone())
            .collect();
    }
    uids.sort();
    uids.dedup();
    uids
}

/// 复制计划：`(源账号, 目标账号) → 要复制过去的会话`。
pub(crate) type LinkPlan = std::collections::BTreeMap<(String, String), Vec<CopyItem>>;

/// 算出「谁复制给谁、谁只需登记」—— **纯逻辑**（正文是否存在由 `body_of` 注入），
/// 单测因此不用碰磁盘。
///
/// `alive` 必须是**按 `updated_at` 倒序**的：同一段对话有多份副本时让最新的先当锚点，
/// 由它去 adopt 旧副本。顺序反了，关联组就会挂到旧副本上。
///
/// `only` 非空时只处理这些源会话（切号勾选）；其余源会话既不复制也不登记。
///
/// ⚠️⚠️ **组员是否「还在」按存活判定，不按记录判定**：`session_links` 里还留着的成员，
/// 如果对应的会话行已经没了（客户端清理 / 用户删库 / 换机），就不算「已有副本」，
/// 否则那一组会**永远**停在「关联断开」——没人会再去补，用户只能手动干预。
/// 反过来，会话行还在、只是被软删（用户在客户端删掉），仍然算「有成员」，不复活。
///
/// 返回 `(复制计划, 只登记项, 无正文数, 已有同内容副本而跳过数)`。
fn plan_links(
    alive: &[&workbuddy_source::WbSession],
    uids: &[String],
    store: &LinkStore,
    since: i64,
    only: Option<&std::collections::HashSet<String>>,
    body_of: impl Fn(&str) -> Option<PathBuf>,
) -> (LinkPlan, Vec<LinkAdopt>, usize, usize) {
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut consumed: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut plan: LinkPlan = std::collections::BTreeMap::new();
    let mut adopts: Vec<LinkAdopt> = Vec::new();
    let mut no_body = 0usize;
    let mut already_there = 0usize;
    // 存活副本的键集合：成员记录 + 这个集合才是「真的还有副本」。
    let alive_keys: std::collections::HashSet<(String, String)> = alive
        .iter()
        .map(|s| (s.user_id.clone(), s.id.clone()))
        .collect();

    for s in alive {
        // 勾选模式下，没勾的源会话一律不碰。
        if let Some(sel) = only {
            if !sel.contains(&s.id) {
                continue;
            }
        }
        // 已经被别的副本 adopt 掉 → 它已经是组员，不再当源（否则会互相复制一轮）。
        if consumed.contains(&(s.user_id.clone(), s.id.clone())) {
            continue;
        }
        let group: Option<(String, std::collections::HashSet<String>)> =
            find_group_for(store, &s.user_id, &s.id).map(|g| {
                (
                    g.id.clone(),
                    g.members
                        .iter()
                        .filter(|m| alive_keys.contains(&(m.uid.clone(), m.session_id.clone())))
                        .map(|m| m.uid.clone())
                        .collect(),
                )
            });
        // 没正文的会话复制不了（复制只搬正文 + 库行）：跳过，但要如实计数。
        let Some(src_path) = body_of(&s.id) else {
            no_body += 1;
            continue;
        };
        // 同一组只处理一次（组里每个成员都会被遍历到）。
        if let Some((gid, _)) = &group {
            if !handled.insert(gid.clone()) {
                continue;
            }
        }
        let existing = group.map(|(_, m)| m).unwrap_or_default();
        // 增量模式（`since > 0`）：更早创建的会话**不复制**，但下面仍会做 0 成本的登记。
        // 用 `created_at` 而不是 `updated_at`：一边聊一边更新的老会话不该每次都被重搬一遍。
        let too_old = since > 0 && s.created_at <= since;
        for u in uids {
            if *u == s.user_id || existing.contains(u) {
                continue;
            }
            // ★ 目标账号里已经有**同一段对话**（同标题 + 同内容摘要）→ 只登记组关系，不复制。
            //
            // 少了这一步，历史上手动复制过的那批会话会被再复制一份：每点一次多一批，
            // 越点越乱。`content_digest` 是项目里既有的「同一段对话」判据
            // （归一化时把副本自己的会话 id 换成固定标记，于是副本之间摘要相同）。
            let twin = alive.iter().find(|o| {
                o.user_id == *u
                    && o.id != s.id
                    && !s.content_digest.is_empty()
                    && o.content_digest == s.content_digest
                    && o.title == s.title
            });
            if let Some(t) = twin {
                // ⚠️ 那个副本若已经属于**别的**关联组，不抢过来（否则同一个会话会挂在两个组里）。
                //    它本来就有这段对话的内容，跳过是安全的。
                if find_group_for(store, &t.user_id, &t.id).is_some() {
                    already_there += 1;
                    continue;
                }
                consumed.insert((t.user_id.clone(), t.id.clone()));
                adopts.push(LinkAdopt {
                    src_uid: s.user_id.clone(),
                    src_cid: s.id.clone(),
                    tgt_uid: u.clone(),
                    tgt_cid: t.id.clone(),
                });
                continue;
            }
            if too_old {
                continue;
            }
            plan.entry((s.user_id.clone(), u.clone()))
                .or_default()
                .push(CopyItem {
                    cid: s.id.clone(),
                    title: s.title.clone(),
                    src_path: src_path.clone(),
                });
        }
    }
    (plan, adopts, no_body, already_there)
}

/// 一键互通：把**每个存活会话补齐到其它所有账号**，并登记成关联组。
///
/// ## 语义（这是「互相关联」的唯一解释）
///
/// 关联组是「同一段对话在各账号上的副本」。所以要把本机所有会话都做成关联，
/// 只能靠**复制**：每个会话在别处不存在副本，就补一份过去。跑完之后任意账号
/// 都能看到本机的全部会话，且每一段对话在各账号上被同一组关系统起来。
///
/// ## 幂等 / 可反复跑
///
/// - 目标账号已是该组员 ⇒ 跳过（`write_copies` 的既有判据），不会造第二个副本。
/// - 副本被用户在目标账号里**删掉**了 ⇒ 仍是组员，**不复活**（否则「删除等于没删」）。
/// - 因此：新会话会被补上、已同步的不会被重复复制，切号后自动跑一次是安全的。
///
/// ## `require_closed`
///
/// 界面入口传 `true`：客户端在跑就**拒绝**写库（正在写的库不能从外面改）。
/// 切号流程传 `false`：调用点已经 `quit_client` 确认进程退出，且马上要重新启动。
///
/// ## 事务边界
///
/// 整批只做**一次**整库备份、一次 `save_links`；任何一步失败按回滚账本
/// （`done_rows` / `done_files`）撤掉本次**全部**写入 —— 不会留下「复制了一半」的
/// 账号组合，也不会留下指向不存在副本的关联成员。
pub fn link_all(opt: LinkOptions) -> Result<Value, String> {
    let db = db_path();
    assert_not_edge_sync(&db)?;
    if opt.require_closed && workbuddy_switch::is_running() {
        return Err(
            "WorkBuddy 正在运行，一键互通要写会话库。请先退出 WorkBuddy 再操作\
             （用本工具切换账号时会自动关它，那一步不需要你手动退出）。"
                .to_string(),
        );
    }

    let all = workbuddy_source::list_sessions()?;

    let mut uids = link_target_uids(&all);
    // 指定了目标账号（切号勾选）就只对它一个同步 —— 别的账号一个字节都不碰。
    if let Some(t) = &opt.target_uid {
        uids.retain(|u| u == t);
        if uids.is_empty() {
            return Ok(json!({
                "ok": true,
                "notNeeded": true,
                "reason": "目标账号不在可用账号列表里（可能已禁用）",
                "accounts": uids,
            }));
        }
    }
    if uids.len() < 2 && opt.target_uid.is_none() {
        return Ok(json!({
            "ok": true,
            "notNeeded": true,
            "reason": "只有一个账号，没有可互通的对方",
            "accounts": uids,
        }));
    }

    // 计划：按「最新更新的会话优先」当源 —— 同一组里哪个副本内容最新，就用它补给别人。
    let mut alive: Vec<_> = all
        .iter()
        .filter(|s| !s.deleted && !s.user_id.is_empty())
        .collect();
    alive.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let alive_keys: std::collections::HashSet<(String, String)> = alive
        .iter()
        .map(|s| (s.user_id.clone(), s.id.clone()))
        .collect();

    let only: Option<std::collections::HashSet<String>> = opt
        .only_sessions
        .as_ref()
        .map(|v| v.iter().cloned().collect());

    let mut store = load_links()?;
    let (mut plan, adopts, no_body, already_there) = plan_links(
        &alive,
        &uids,
        &store,
        opt.since,
        only.as_ref(),
        workbuddy_source::find_jsonl,
    );
    // 「只登记已有副本」模式：计划出来的复制项一律丢掉（0 字节、不碰会话库）。
    if !opt.copy_missing {
        plan.clear();
    }
    let mode = if !opt.copy_missing {
        "adoptOnly"
    } else if opt.since > 0 {
        "incremental"
    } else {
        "copy"
    };

    let planned: usize = plan.values().map(|v| v.len()).sum();

    // 已关联副本的内容同步：**只对勾选的会话**做，且必须在此之前算出配对
    // （`store` 后面要在事务里改，新复制出来的副本不需要「刷新」）。
    //
    // 配对条件：源会话在勾选里 + 关联组里有目标成员 + 那份成员**存活**。
    let refresh_pairs: Vec<(String, String, PathBuf, String)> = if opt.refresh_linked {
        let mut pairs = Vec::new();
        for s in &alive {
            if let Some(sel) = &only {
                if !sel.contains(&s.id) {
                    continue;
                }
            }
            let Some(tgt_cid) = find_group_for(&store, &s.user_id, &s.id)
                .and_then(|g| member_for(g, opt.target_uid.as_deref().unwrap_or("")))
                .map(|m| m.session_id.clone())
            else {
                continue;
            };
            if !alive_keys.contains(&(
                opt.target_uid.as_deref().unwrap_or("").to_string(),
                tgt_cid.clone(),
            )) {
                continue;
            }
            let Some(path) = workbuddy_source::find_jsonl(&s.id) else {
                continue;
            };
            pairs.push((s.id.clone(), s.title.clone(), path, tgt_cid));
        }
        pairs
    } else {
        Vec::new()
    };

    if planned == 0 && refresh_pairs.is_empty() {
        // 只有「登记组关系」，没有复制 → 完全不碰会话库，也不需要备份。
        if !adopts.is_empty() {
            for a in &adopts {
                upsert_group(
                    &mut store,
                    &a.src_uid,
                    &a.src_cid,
                    &a.tgt_uid,
                    &a.tgt_cid,
                    now_ms(),
                );
            }
            save_links(&store)?;
        }
        return Ok(json!({
            "ok": true,
            "mode": mode,
            "notNeeded": adopts.is_empty(),
            "copied": 0,
            "adopted": adopts.len(),
            "planned": 0,
            "accounts": uids,
            "noBody": no_body,
            "alreadyThere": already_there,
            "reason": if adopts.is_empty() {
                if !opt.copy_missing {
                    "没有发现「同一段对话在两处各有一份」的副本，无需登记".to_string()
                } else if opt.since > 0 {
                    "没有需要同步的新会话（自动同步只搬新产生的会话）".to_string()
                } else {
                    "所有会话都已在各账号之间关联，没有需要补的".to_string()
                }
            } else {
                format!("已登记 {} 组关联，未复制新副本", adopts.len())
            },
            "storePath": links_file().to_string_lossy(),
        }));
    }

    let backup = backup_db("link-all")?;
    let conn = open_rw(&db)?;
    let now = now_ms();
    let mut done_rows: Vec<String> = Vec::new();
    let mut done_files: Vec<PathBuf> = Vec::new();
    let mut copied: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    // 被内容同步覆盖过的正文 (目标正文, 覆盖前备份)，失败时逐份还原。
    let mut done_bodies: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut refreshed: Vec<Value> = Vec::new();
    let mut refresh_skipped: Vec<Value> = Vec::new();
    let mut per_target: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let result = (|| -> Result<(), String> {
        // 先落「只登记」的那批：它们本来就有副本，登记完就不用再复制。
        // 这里只改内存里的 store，`save_links` 失败时连同复制一起整体作废。
        for a in &adopts {
            upsert_group(
                &mut store,
                &a.src_uid,
                &a.src_cid,
                &a.tgt_uid,
                &a.tgt_cid,
                now,
            );
        }
        for ((src, dst), items) in &plan {
            let (c, s) = write_copies(
                &conn,
                &mut store,
                src,
                dst,
                items,
                now,
                &alive_keys,
                &mut done_rows,
                &mut done_files,
            )?;
            // 先计数再 move（`extend` 会把 c 吃掉）。
            *per_target.entry(dst.clone()).or_insert(0) += c.len();
            copied.extend(c);
            skipped.extend(s);
        }
        // 已关联副本的内容同步（纯追加）。
        if !refresh_pairs.is_empty() {
            let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let dir = store_dir()
                .join("backup")
                .join(format!("wb_bodies_link_{stamp}"));
            let target_uid = opt.target_uid.clone().unwrap_or_default();
            let (r, rs) = refresh_linked_copies(
                &conn,
                &refresh_pairs,
                &target_uid,
                now,
                &dir,
                &mut done_bodies,
            );
            refreshed = r;
            refresh_skipped = rs;
        }
        save_links(&store)?;
        Ok(())
    })();

    if let Err(e) = result {
        let mut rolled = 0usize;
        for cid in &done_rows {
            let _ = conn.execute("DELETE FROM sessions WHERE id = ?1", [cid]);
            rolled += 1;
        }
        for f in &done_files {
            let _ = std::fs::remove_file(f);
        }
        // 内容同步也要回滚：把覆盖前的正文拷回去，否则「事务回滚了但文件已经变了」。
        let mut restored = 0usize;
        for (target, backup_body) in &done_bodies {
            if std::fs::copy(backup_body, target).is_ok() {
                restored += 1;
            }
        }
        return Err(format!(
            "{e}（已回滚本次互通的 {} 条新记录、{} 份正文，备份保留在 {}）",
            rolled,
            restored,
            backup.display()
        ));
    }

    let by_target: Vec<Value> = per_target
        .iter()
        .map(|(uid, n)| {
            json!({ "uid": uid, "label": workbuddy_accounts::label_for(uid), "count": n })
        })
        .collect();

    Ok(json!({
        "ok": true,
        "mode": mode,
        "copied": copied.len(),
        "adopted": adopts.len(),
        "planned": planned,
        // 内容同步的两个数字：改了几份 / 为什么没改。界面必须分开报，
        // 「0 份已同步」不能同时代表「本来就是最新」和「全都失败了」。
        "refreshed": refreshed.len(),
        "refreshRows": refreshed,
        "refreshSkipped": refresh_skipped,
        "accounts": uids,
        "byTarget": by_target,
        "skipped": skipped.len(),
        "skippedRows": skipped,
        "noBody": no_body,
        "alreadyThere": already_there,
        "backup": backup.to_string_lossy(),
        "edgeSyncSkipped": edge_sync_databases(),
        "storePath": links_file().to_string_lossy(),
        "note": "复制的是会话副本，原会话不受影响；云端映射库未做任何写入",
    }))
}

/// 一键互通的**只读预览**：与 [`link_all`] 共用 [`plan_links`] 与 [`link_target_uids`]，
/// 所以「预览说要复制 N 个」和「真跑复制 N 个」不可能对不上（同屏两个数字必须同源）。
///
/// 只读：不写库、不写关联记录、不备份。
pub fn link_all_preview() -> Value {
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let uids = link_target_uids(&all);
    if uids.len() < 2 {
        return json!({
            "ok": true,
            "accounts": uids,
            "planned": 0,
            "adopted": 0,
            "reason": "只有一个账号，没有可互通的对方",
            "clientRunning": workbuddy_switch::is_running(),
        });
    }
    let mut alive: Vec<_> = all
        .iter()
        .filter(|s| !s.deleted && !s.user_id.is_empty())
        .collect();
    alive.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let store = match load_links() {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let (plan, adopts, no_body, already_there) =
        plan_links(&alive, &uids, &store, 0, None, workbuddy_source::find_jsonl);

    let mut per_target: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut total_bytes: u64 = 0;
    for ((_src, dst), items) in &plan {
        *per_target.entry(dst.clone()).or_insert(0) += items.len();
        for i in items {
            total_bytes += all
                .iter()
                .find(|s| s.id == i.cid)
                .map(|s| s.body_bytes)
                .unwrap_or(0);
        }
    }
    let by_target: Vec<Value> = per_target
        .iter()
        .map(|(uid, n)| json!({ "uid": uid, "label": workbuddy_accounts::label_for(uid), "count": n }))
        .collect();

    json!({
        "ok": true,
        "accounts": uids,
        "planned": plan.values().map(|v| v.len()).sum::<usize>(),
        "adopted": adopts.len(),
        "noBody": no_body,
        "alreadyThere": already_there,
        "totalBytes": total_bytes,
        "byTarget": by_target,
        // 客户端在跑时按钮要提示「先退出」——这是唯一会被后端直接拒掉的情况。
        "clientRunning": workbuddy_switch::is_running(),
    })
}

/// 删除一个关联组（只删本工具的关联记录，不动任何会话数据）。
pub fn unlink_group(group_id: &str) -> Result<Value, String> {
    let mut store = load_links()?;
    let before = store.groups.len();
    store.groups.retain(|g| g.id != group_id);
    if store.groups.len() == before {
        return Err(format!("没有找到关联组 {group_id}"));
    }
    save_links(&store)?;
    Ok(json!({ "ok": true, "groupId": group_id, "remaining": store.groups.len() }))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wb-sessions-{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 建一个带 sessions 表的临时库（列集合与本机实测一致的子集）。
    fn make_db(dir: &Path) -> PathBuf {
        let path = dir.join("workbuddy.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                cwd TEXT,
                user_id TEXT,
                title TEXT,
                custom_title TEXT,
                created_at INTEGER,
                updated_at INTEGER,
                deleted_at INTEGER,
                group_id TEXT,
                group_title TEXT,
                model TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, cwd, user_id, title, custom_title, created_at, updated_at, deleted_at, group_id, group_title, model)
             VALUES ('aaa', 'D:/p', 'u1', '标题A', NULL, 100, 200, NULL, 'g1', '组1', 'm')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, cwd, user_id, title, custom_title, created_at, updated_at, deleted_at, group_id, group_title, model)
             VALUES ('bbb', 'D:/p', 'u1', '已删除', NULL, 100, 300, 999, NULL, NULL, 'm')",
            [],
        )
        .unwrap();
        path
    }

    #[test]
    fn edge_sync_writes_are_refused() {
        assert!(assert_not_edge_sync(Path::new("C:/x/workbuddy.db")).is_ok());
        let err = assert_not_edge_sync(Path::new("C:/x/edge-sync-mapping-v4.db")).unwrap_err();
        assert!(err.contains("edge-sync"), "错误文案应点名云端映射库: {err}");
    }

    #[test]
    fn copy_row_rewrites_identity_and_clears_group() {
        let dir = temp_dir("copyrow");
        let db = make_db(&dir);
        let conn = Connection::open(&db).unwrap();
        let src = read_source_row(&conn, "aaa").unwrap().expect("源行应存在");
        insert_copy_row(&conn, &src, "new-id", "u2", 12345).unwrap();

        let owner = row_owner(&conn, "new-id").expect("副本应存在且未删除");
        assert_eq!(owner, "u2");
        let (title, updated, group): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT title, updated_at, group_id FROM sessions WHERE id='new-id'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "标题A", "正文内容列必须原样保留");
        assert_eq!(updated, 12345, "时间戳必须刷新为写入时刻");
        assert_eq!(group, None, "副本的分组必须置空，避免与源会话混组");

        // 源行必须一字未改。
        let src_owner = row_owner(&conn, "aaa").unwrap();
        assert_eq!(src_owner, "u1");
        assert!(row_deleted_at(&conn, "aaa").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_uuid_insert_fails_instead_of_overwriting() {
        let dir = temp_dir("dupid");
        let db = make_db(&dir);
        let conn = Connection::open(&db).unwrap();
        let src = read_source_row(&conn, "aaa").unwrap().unwrap();
        insert_copy_row(&conn, &src, "new-id", "u2", 1).unwrap();
        let err = insert_copy_row(&conn, &src, "new-id", "u2", 2).unwrap_err();
        assert!(!err.is_empty(), "同 id 二次插入必须失败，不能悄悄覆盖");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions WHERE id='new-id'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn soft_delete_only_touches_owned_live_rows() {
        let dir = temp_dir("softdel");
        let db = make_db(&dir);
        let conn = Connection::open(&db).unwrap();
        assert_eq!(soft_delete_row(&conn, "aaa", "u1", 555).unwrap(), 1);
        assert_eq!(row_deleted_at(&conn, "aaa"), Some(555));
        // 归属不符 → 0 行；已删除 → 0 行。
        assert_eq!(soft_delete_row(&conn, "aaa", "u2", 556).unwrap(), 0);
        assert_eq!(soft_delete_row(&conn, "bbb", "u1", 557).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_session_id_only_touches_the_given_id() {
        let cid = "aaa-bbb-ccc";
        let text = format!("会话 {cid} 开始\n另一段无关文本\n又提到 {cid} 一次");
        let out = replace_session_id(&text, cid, "zzz-yyy-xxx");
        assert!(!out.contains(cid), "源 id 必须全部替换掉");
        assert!(out.contains("另一段无关文本"), "无关文本不能被改动");
        assert_eq!(out.matches("zzz-yyy-xxx").count(), 2);
    }

    #[test]
    fn upsert_group_reuses_existing_group_and_replaces_target_member() {
        let mut store = LinkStore::default();
        let g1 = upsert_group(&mut store, "u1", "s1", "u2", "s2", 10);
        assert_eq!(store.groups.len(), 1);
        // 同组换目标副本：组不变，成员被替换而不是新增。
        let g2 = upsert_group(&mut store, "u1", "s1", "u2", "s3", 20);
        assert_eq!(g1, g2, "同一源会话必须复用同一个组");
        assert_eq!(store.groups[0].members.len(), 2);
        let target = member_for(&store.groups[0], "u2").unwrap();
        assert_eq!(target.session_id, "s3");
        // 不同源会话 → 新组。
        let g3 = upsert_group(&mut store, "u1", "s9", "u2", "s4", 30);
        assert_ne!(g1, g3);
        assert_eq!(store.groups.len(), 2);
        assert_eq!(store.version, LINK_STORE_VERSION);
    }

    #[test]
    fn safe_file_name_strips_windows_illegal_chars() {
        assert_eq!(safe_file_name("a/b\\c:d*e?f\"g<h>i|j"), "abcdefghij");
        assert_eq!(safe_file_name("   "), "会话");
        assert_eq!(safe_file_name(""), "会话");
        assert!(safe_file_name(&"长".repeat(200)).chars().count() <= 60);
    }

    #[test]
    fn render_markdown_includes_turns_and_tool_summary() {
        let turns = vec![workbuddy_source::WbTurn {
            user_text: "提问内容".into(),
            assistant_text: "回答内容".into(),
            events: vec![workbuddy_source::WbEvent {
                kind: "function_call".into(),
                name: "Read".into(),
                call_id: "c1".into(),
                status: String::new(),
                text: String::new(),
            }],
            created_at: 1,
            updated_at: 2,
        }];
        let md = render_markdown("标题", "cid-1", &turns);
        assert!(md.contains("# 标题"));
        assert!(md.contains("提问内容") && md.contains("回答内容"));
        assert!(md.contains("回合 1"));
        assert!(md.contains("`Read`"), "工具调用应出现在折叠块里");
    }

    #[test]
    fn link_store_roundtrip_and_unlink_lookup() {
        let tmp = temp_dir("links");
        let file = tmp.join("workbuddy-session-links.json");
        let mut store = LinkStore::default();
        upsert_group(&mut store, "u1", "s1", "u2", "s2", 1);
        let text = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(&file, text).unwrap();
        let back: LinkStore = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(back.groups.len(), 1);
        assert!(find_group_for(&back, "u1", "s1").is_some());
        assert!(find_group_for(&back, "u1", "nope").is_none());
        assert_eq!(member_for(&back.groups[0], "u2").unwrap().session_id, "s2");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── 一键互通（全账号互相关联）的计划逻辑 ────────────────────────────────

    fn ws(
        id: &str,
        uid: &str,
        title: &str,
        digest: &str,
        updated: i64,
    ) -> workbuddy_source::WbSession {
        workbuddy_source::WbSession {
            id: id.to_string(),
            title: title.to_string(),
            cwd: "D:/p".to_string(),
            user_id: uid.to_string(),
            created_at: 1,
            updated_at: updated,
            model: "m".to_string(),
            deleted: false,
            has_body: true,
            body_bytes: 10,
            body_lines: 1,
            content_digest: digest.to_string(),
            dup_group: String::new(),
            is_newest: true,
            dup_note: String::new(),
            dup_relation: String::new(),
        }
    }

    /// 核心判据：**已有同内容副本的账号只登记、不复制**，且那个副本不再当源。
    ///
    /// 少了这条，历史手动复制过的那批会话会被一键再复制一轮 —— 点几次多几批。
    #[test]
    fn link_plan_adopts_existing_twin_instead_of_copying_again() {
        let uids: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
        // 同一段对话：A 上是最新副本，B 上是旧副本（都没登记过关联）。
        let a1 = ws("s-a1", "A", "方案讨论", "deadbeefdeadbeef", 300);
        let b1 = ws("s-b1", "B", "方案讨论", "deadbeefdeadbeef", 200);
        // 另一段对话，只有 C 有。
        let c1 = ws("s-c1", "C", "别的会话", "cafecafecafecafe", 100);
        let alive = vec![&a1, &b1, &c1]; // 已按 updated_at 倒序

        let store = LinkStore::default();
        let (plan, adopts, no_body, already) =
            plan_links(&alive, &uids, &store, 0, None, |_| Some(PathBuf::from("x.jsonl")));

        assert_eq!(no_body, 0);
        assert_eq!(already, 0);
        assert_eq!(adopts.len(), 1, "B 上的同内容副本应当被登记，而不是复制");
        assert_eq!(adopts[0].tgt_uid, "B");
        assert_eq!(adopts[0].tgt_cid, "s-b1");

        let flat: Vec<String> = plan
            .iter()
            .flat_map(|((s, d), v)| v.iter().map(move |i| format!("{s}->{d}:{}", i.cid)))
            .collect();
        assert!(
            !flat.iter().any(|x| x.starts_with("B->")),
            "被登记掉的副本不该再当源，否则会反向复制一轮：{flat:?}"
        );
        assert!(flat.contains(&"A->C:s-a1".to_string()), "A 的会话要补给 C");
        assert!(flat.contains(&"C->A:s-c1".to_string()), "C 的会话要补给 A");
        assert!(flat.contains(&"C->B:s-c1".to_string()), "C 的会话要补给 B");
        assert_eq!(flat.len(), 3, "只有真正缺副本的三条：{flat:?}");
    }

    /// 已经在同一关联组里的账号不再重复计划（幂等：反复点不会膨胀）。
    #[test]
    fn link_plan_skips_accounts_already_in_the_group() {
        let uids: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let a1 = ws("s-a1", "A", "t", "", 200);
        let b1 = ws("s-b1", "B", "t", "", 100);
        let store = LinkStore {
            version: LINK_STORE_VERSION,
            groups: vec![LinkGroup {
                id: "g1".to_string(),
                created_at: 1,
                members: vec![
                    LinkMember {
                        uid: "A".to_string(),
                        session_id: "s-a1".to_string(),
                        linked_at: 1,
                    },
                    LinkMember {
                        uid: "B".to_string(),
                        session_id: "s-b1".to_string(),
                        linked_at: 1,
                    },
                ],
            }],
        };
        let (plan, adopts, _, _) =
            plan_links(&[&a1, &b1], &uids, &store, 0, None, |_| Some(PathBuf::from("x.jsonl")));
        assert!(plan.is_empty(), "两个账号都已在组里，不该再复制");
        assert!(adopts.is_empty());
    }

    /// 没有正文文件的会话搬不过去：跳过并如实计数（不许假装成功）。
    #[test]
    fn link_plan_counts_sessions_without_body() {
        let uids: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let a1 = ws("s-a1", "A", "t", "", 100);
        let (plan, _, no_body, _) = plan_links(&[&a1], &uids, &LinkStore::default(), 0, None, |_| None);
        assert_eq!(no_body, 1);
        assert!(plan.is_empty());
    }

    // ── 切号勾选的默认值：同标题较旧副本不许默认勾 ──────────────────────────

    /// 「组内最新」必须按**本账号**的更新时间判，不能用全局 `is_newest`。
    ///
    /// 真机场景：账号 e925618e / fbd4139a 的「每日 AI 新闻推送」一组 2～6 条，
    /// 全局最新那份在别的账号里 ⇒ 组内全局标记全 false。若拿全局标记判「较旧」，
    /// 整组都不默认勾选，用户会以为关联同步失效。
    #[test]
    fn newest_by_group_uses_local_time_not_global_flag() {
        let rows = [
            ("每日 ai 新闻推送", "a", 100),
            ("每日 ai 新闻推送", "b", 300),
            ("每日 ai 新闻推送", "c", 200),
            ("", "x", 999), // 标题唯一：不参与分组
        ];
        let m = newest_by_group(&rows);
        assert_eq!(m.get("每日 ai 新闻推送").map(String::as_str), Some("b"));
        assert!(!m.contains_key(""), "空组键不是真实分组");
        // 时间相同取先出现的（不抖动）
        let tie = [("t", "first", 5), ("t", "second", 5)];
        assert_eq!(newest_by_group(&tie).get("t").map(String::as_str), Some("first"));
    }

    /// 定时任务每次运行都建同名会话，一个号里能有 5 份「每日 AI 推送」。
    ///
    /// 判据必须落在 `default_checked()` 这一个出口上，否则「界面默认勾了 5 份」与
    /// 「后端只同步 1 份」会各说一套 —— 用户看到的就是「切完号会话重复出现多次」。
    #[test]
    fn dup_older_copy_is_not_checked_by_default() {
        // 已关联 + 有正文 + 最新 → 勾
        assert!(default_checked(SwitchLinkState::Linked, true, false));
        assert!(default_checked(SwitchLinkState::Missing, true, false));
        assert!(default_checked(SwitchLinkState::Twin, true, false));
        // 同组较旧 → 一律不勾（即使已关联），想同步由用户手动勾
        assert!(!default_checked(SwitchLinkState::Linked, true, true));
        assert!(!default_checked(SwitchLinkState::Twin, true, true));
        // 没关联 / 没正文 → 本来就不勾
        assert!(!default_checked(SwitchLinkState::Unlinked, true, false));
        assert!(!default_checked(SwitchLinkState::Linked, false, false));
    }

    // ── 关联「断开」的两条判据 ───────────────────────────────────────────────

    /// 组员的会话已经不在了（客户端清理 / 删库）⇒ 必须允许**重建**，否则这一组
    /// 会永远卡在「关联断开」，谁也不会再去补。
    #[test]
    fn dead_member_allows_rebuild() {
        let uids: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let a1 = ws("s-a1", "A", "方案讨论", "aaa", 300);
        let alive = vec![&a1];
        // 关联组里记着 A→B，但那份副本 s-gone 已经不在库里了。
        let store = LinkStore {
            version: LINK_STORE_VERSION,
            groups: vec![LinkGroup {
                id: "g1".into(),
                created_at: 1,
                members: vec![
                    LinkMember { uid: "A".into(), session_id: "s-a1".into(), linked_at: 1 },
                    LinkMember { uid: "B".into(), session_id: "s-gone".into(), linked_at: 1 },
                ],
            }],
        };
        let (plan, adopts, _, _) =
            plan_links(&alive, &uids, &store, 0, None, |_| Some(PathBuf::from("x.jsonl")));
        assert!(adopts.is_empty());
        assert_eq!(
            plan.get(&("A".to_string(), "B".to_string())).map(|v| v.len()),
            Some(1),
            "死掉的成员不该挡住重建"
        );
    }

    /// 成员还在（哪怕软删）：不复活，保持老语义「删除等于没删」。
    #[test]
    fn deleted_member_is_not_revived() {
        let uids: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let a1 = ws("s-a1", "A", "方案讨论", "aaa", 300);
        let b1 = ws("s-b1", "B", "方案讨论", "aaa", 200);
        let alive = vec![&a1, &b1];
        let store = LinkStore {
            version: LINK_STORE_VERSION,
            groups: vec![LinkGroup {
                id: "g1".into(),
                created_at: 1,
                members: vec![
                    LinkMember { uid: "A".into(), session_id: "s-a1".into(), linked_at: 1 },
                    LinkMember { uid: "B".into(), session_id: "s-b1".into(), linked_at: 1 },
                ],
            }],
        };
        let (plan, adopts, _, _) =
            plan_links(&alive, &uids, &store, 0, None, |_| Some(PathBuf::from("x.jsonl")));
        assert!(plan.is_empty() && adopts.is_empty(), "已有关联的组不该再动");
    }

    /// 勾选模式：只有被勾中的源会话参与，其余一个字节都不碰。
    #[test]
    fn only_sessions_limits_sources() {
        let uids: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let a1 = ws("s-a1", "A", "勾中的", "aaa", 300);
        let a2 = ws("s-a2", "A", "没勾的", "bbb", 200);
        let alive = vec![&a1, &a2];
        let mut sel = std::collections::HashSet::new();
        sel.insert("s-a1".to_string());
        let (plan, _, _, _) = plan_links(
            &alive,
            &uids,
            &LinkStore::default(),
            0,
            Some(&sel),
            |_| Some(PathBuf::from("x.jsonl")),
        );
        let items: Vec<&CopyItem> = plan.values().flatten().collect();
        assert_eq!(items.len(), 1, "只有勾中的那一条");
        assert_eq!(items[0].cid, "s-a1");
    }

    // ── 内容同步：只在「纯追加」时改写 ──────────────────────────────────────

    fn write_lines(path: &Path, lines: &[&str]) {
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn body_relation_tells_identical_prefix_and_diverge() {
        let dir = temp_dir("rel");
        let src = dir.join("src.jsonl");
        let tgt = dir.join("tgt.jsonl");

        // 两份内容相同，但各自带自己的会话 id → 归一化后应当判 Identical。
        write_lines(&src, &["{\"id\":\"SID-SRC\",\"t\":\"a\"}", "{\"t\":\"b\"}"]);
        write_lines(&tgt, &["{\"id\":\"SID-TGT\",\"t\":\"a\"}", "{\"t\":\"b\"}"]);
        assert_eq!(
            body_relation(&src, "SID-SRC", &tgt, "SID-TGT").unwrap(),
            BodyRelation::Identical
        );

        // 源多一行 → 目标是前缀，可安全追加。
        write_lines(&src, &["l1", "l2", "l3"]);
        write_lines(&tgt, &["l1", "l2"]);
        assert_eq!(
            body_relation(&src, "SID-SRC", &tgt, "SID-TGT").unwrap(),
            BodyRelation::Prefix
        );

        // 目标里也有源没有的内容 → 有分歧，绝不能覆盖。
        write_lines(&tgt, &["l1", "l2", "目标独有的"]);
        assert_eq!(
            body_relation(&src, "SID-SRC", &tgt, "SID-TGT").unwrap(),
            BodyRelation::Diverge
        );

        // 中途不同也算分歧。
        write_lines(&tgt, &["l1", "改了"]);
        assert_eq!(
            body_relation(&src, "SID-SRC", &tgt, "SID-TGT").unwrap(),
            BodyRelation::Diverge
        );
    }

    /// 改写后正文里的会话 id 必须换成目标 id（否则目标账号打开会串到源会话）。
    #[test]
    fn overwrite_body_rewrites_session_id() {
        let dir = temp_dir("ovr");
        let src = dir.join("src.jsonl");
        let tgt = dir.join("tgt.jsonl");
        write_lines(&src, &["{\"id\":\"SRC\"}", "{\"t\":\"next\"}"]);
        write_lines(&tgt, &["{\"id\":\"TGT\"}"]);
        let n = overwrite_body_from(&src, "SRC", &tgt, "TGT").unwrap();
        assert_eq!(n, 2);
        let text = std::fs::read_to_string(&tgt).unwrap();
        assert!(text.contains("TGT") && !text.contains("SRC"), "{text}");
        assert!(text.contains("next"));
    }
}
