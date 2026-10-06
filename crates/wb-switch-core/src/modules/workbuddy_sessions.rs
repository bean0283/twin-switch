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
use crate::modules::{workbuddy_accounts, workbuddy_auth, workbuddy_source, workbuddy_switch};
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

fn open_rw(path: &Path) -> Result<Connection, String> {
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
fn row_owner(conn: &Connection, cid: &str) -> Option<String> {
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
fn read_source_row(
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

fn target_columns(conn: &Connection) -> Result<Vec<String>, String> {
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
fn insert_copy_row(
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
fn backup_db(tag: &str) -> Result<PathBuf, String> {
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

fn session_item(s: &workbuddy_source::WbSession) -> Value {
    json!({
        "id": s.id,
        "title": s.title,
        "cwd": s.cwd,
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
fn target_body_path(source_path: &Path, new_cid: &str) -> PathBuf {
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
    let mut plan: Vec<(String, String, PathBuf)> = Vec::new();
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
        plan.push((cid.clone(), row.title.clone(), path));
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
    let mut copied: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let result = (|| -> Result<(), String> {
        let mut store = load_links().unwrap_or_default();
        for (cid, title, src_path) in &plan {
            let src_row = read_source_row(&conn, cid)?
                .ok_or_else(|| format!("会话 {cid} 在库里没有对应行"))?;
            // 已建立过关联（目标账号已有副本）→ 跳过，不制造第二个副本。
            if let Some(existing) = find_group_for(&store, source_uid, cid)
                .and_then(|g| member_for(g, target_uid))
                .map(|m| m.session_id.clone())
            {
                skipped.push(json!({
                    "id": cid, "title": title, "reason": "目标账号已有副本",
                    "targetSessionId": existing,
                }));
                continue;
            }
            let new_cid = uuid::Uuid::new_v4().to_string();
            let dest = target_body_path(src_path, &new_cid);
            if dest.exists() {
                return Err(format!("目标正文已存在同名文件，已停止复制：{}", dest.display()));
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("目标工作区目录创建失败: {e}"))?;
            }
            let text = std::fs::read_to_string(src_path)
                .map_err(|e| format!("读取源正文失败: {e}"))?;
            let text = replace_session_id(&text, cid, &new_cid);
            std::fs::write(&dest, text.as_bytes())
                .map_err(|e| format!("写入副本正文失败: {e}"))?;
            done_files.push(dest.clone());
            insert_copy_row(&conn, &src_row, &new_cid, target_uid, now)?;
            done_rows.push(new_cid.clone());
            // 写后校验：新行必须存在、归属目标账号、未删除。
            match row_owner(&conn, &new_cid) {
                Some(owner) if owner == target_uid => {}
                Some(owner) => {
                    return Err(format!("副本归属校验失败：期望 {target_uid}，实际 {owner}"))
                }
                None => return Err("副本写入后不可见，未按成功处理".to_string()),
            }
            let group_id = upsert_group(&mut store, source_uid, cid, target_uid, &new_cid, now);
            copied.push(json!({
                "sourceId": cid,
                "targetId": new_cid,
                "title": title,
                "bodyPath": dest.to_string_lossy(),
                "groupId": group_id,
            }));
        }
        save_links(&store)?;
        Ok(())
    })();

    if let Err(e) = result {
        // 回滚：只撤销本次写入（删新行 + 删新正文），备份保留供人工恢复。
        let mut rolled: Vec<String> = Vec::new();
        for cid in &done_rows {
            let _ = conn.execute("DELETE FROM sessions WHERE id = ?1", [cid]);
            rolled.push(cid.clone());
        }
        for f in &done_files {
            let _ = std::fs::remove_file(f);
        }
        return Err(format!("{e}（已回滚本次复制的 {} 条新记录，备份保留在 {}）", rolled.len(), backup.display()));
    }

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
fn load_links() -> Result<LinkStore, String> {
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

fn save_links(store: &LinkStore) -> Result<(), String> {
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
fn upsert_group(
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
}
