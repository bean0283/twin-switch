//! Qoder 会话记录（只读本地 SQLite）。
//!
//! 数据在客户端的 `main.sqlite`：
//!
//! | 表 | 用途 |
//! | --- | --- |
//! | `chat_sessions` | 会话元信息（`session_id` / `title` / `cwd` / `model` / 时间戳 / `archived`） |
//! | `chat_session_messages` | 每条消息一行，正文在 `payload_json.parts[]` |
//!
//! `parts[]` 里 `type=text` 才是正文；`type=thinking` 是**思维链**（不能拼进回答），
//! `tool` / `hook` 之类直接跳过 —— 与 Trae / WorkBuddy 两页的口径一致。
//!
//! 三条纪律：
//!
//! 1. **只读**：`mode=ro` 打开，绝不写客户端库（那边退出时会整体回写）。
//! 2. **两个版本各读各的库**：国内版 `com.qodercn.app.stable`、国际版 `com.qoder.app.stable`。
//! 3. 读不到就如实报错（客户端在跑时 WAL 可能还没落盘），**不编空列表充数**。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::modules::qoder_realm::QoderRegion;

/// 会话库路径（找不到返回 None）。
pub fn db_path(region: QoderRegion) -> Option<PathBuf> {
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = match region {
        QoderRegion::Cn => "com.qodercn.app.stable",
        QoderRegion::Global => "com.qoder.app.stable",
    };
    let p = roaming.join(dir).join("main.sqlite");
    p.is_file().then_some(p)
}

fn open(region: QoderRegion) -> Result<Connection, String> {
    let path = db_path(region).ok_or_else(|| {
        format!(
            "本机没有 {} 的会话库（{} 未登录过或没装）",
            region.display_name(),
            region.as_str()
        )
    })?;
    Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("打开会话库失败（{}）：{e}", path.display()))
}

/// 会话列表（按最近更新倒序）。
///
/// `include_archived = false` 时过滤归档会话 —— 与另两页的默认口径一致。
pub fn list(region: QoderRegion, include_archived: bool) -> Result<Vec<Value>, String> {
    let conn = open(region)?;
    // 正文体积取 `payload_bytes` 求和（上游自己记的，比 length(json) 准）；
    // 来源取 `extra_json` 里的 `source` / `importedFrom`（会话**没有账号归属字段**，
    // `registration_json` 实测为空，所以界面上不提供「按账号筛选」）。
    let sql = format!(
        "select s.session_id, s.title, s.cwd, s.model, s.created_at, s.updated_at, s.archived,
                (select count(*) from chat_session_messages m where m.session_id = s.session_id),
                (select coalesce(sum(m.payload_bytes), 0) from chat_session_messages m where m.session_id = s.session_id),
                s.extra_json
         from chat_sessions s
         where s.deleted_at is null {}
         order by s.updated_at desc
         limit 500",
        if include_archived { "" } else { "and s.archived = 0" }
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("查询会话失败：{e}"))?;
    let rows = stmt
        .query_map([], |r| {
            let archived: i64 = r.get(6).unwrap_or(0);
            let extra: Option<String> = r.get(9).ok();
            let (source, imported_from) = parse_extra(extra.as_deref());
            Ok(serde_json::json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                "cwd": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                "model": r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                "createdAt": r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                "updatedAt": r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                "archived": archived != 0,
                "messageCount": r.get::<_, i64>(7).unwrap_or(0),
                "bytes": r.get::<_, i64>(8).unwrap_or(0),
                "source": source,
                "importedFrom": imported_from,
            }))
        })
        .map_err(|e| format!("读取会话失败：{e}"))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("读取会话行失败：{e}"))?);
    }
    Ok(out)
}

/// 从 `extra_json` 取来源标签：`source`（workspace…）与 `importedFrom`（如 workbuddy）。
fn parse_extra(raw: Option<&str>) -> (String, Option<String>) {
    let Some(raw) = raw else {
        return (String::new(), None);
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return (String::new(), None);
    };
    let source = v
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let imported = v
        .get("importedFrom")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (source, imported)
}

/// 会话详情：逐条消息（正文只取 `parts[type=text]`）。
pub fn detail(region: QoderRegion, session_id: &str) -> Result<Value, String> {
    let conn = open(region)?;

    let meta = conn
        .query_row(
            "select title, cwd, model, created_at, updated_at from chat_sessions where session_id = ?1",
            [session_id],
            |r| {
                Ok(serde_json::json!({
                    "title": r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    "cwd": r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    "model": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    "createdAt": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "updatedAt": r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                }))
            },
        )
        .optional_map()?;

    let mut stmt = conn
        .prepare(
            "select role, payload_json, created_at, status, message_id
             from chat_session_messages
             where session_id = ?1
             order by sequence",
        )
        .map_err(|e| format!("查询消息失败：{e}"))?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })
        .map_err(|e| format!("读取消息失败：{e}"))?;

    let mut messages = Vec::new();
    for row in rows {
        let (role, payload, created_at, status, message_id) =
            row.map_err(|e| format!("读取消息行失败：{e}"))?;
        let parsed: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let parts = parsed
            .get("parts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut text = String::new();
        let mut thinking = String::new();
        let mut tools = 0usize;
        for part in &parts {
            match part.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" => {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
                // 思维链单独放，不拼进正文（拼了用户会看到一坨 "The user asks…"）。
                "thinking" => {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        thinking.push_str(t);
                    }
                }
                "tool" | "tool_use" | "tool_result" => tools += 1,
                _ => {}
            }
        }

        // 没有正文、也没有思维链和工具调用的空消息直接跳过（客户端会写占位行）。
        if text.trim().is_empty() && thinking.trim().is_empty() && tools == 0 {
            continue;
        }
        messages.push(serde_json::json!({
            "messageId": message_id,
            "role": role,
            "text": text,
            "thinking": if thinking.is_empty() { Value::Null } else { Value::String(thinking) },
            "toolCount": tools,
            "createdAt": created_at,
            "status": status,
        }));
    }

    Ok(serde_json::json!({
        "id": session_id,
        "region": region.as_str(),
        "meta": meta.unwrap_or(Value::Null),
        "messages": messages,
    }))
}

/// `query_row` 的「没有这行就返回 None」扩展。
trait OptionalRow<T> {
    fn optional_map(self) -> Result<Option<T>, String>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional_map(self) -> Result<Option<T>, String> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("读取会话元信息失败：{e}")),
        }
    }
}

// ---------------------------------------------------------------- 写操作
//
// ⚠️ 这是本模块**唯一**会写客户端数据的地方，四条纪律：
//
// 1. **客户端在跑就拒绝**：Qoder 会把内存里的会话状态整体回写，此时写库等于白干甚至写坏。
// 2. **写前备份**：`main.sqlite` 连同 `-wal` / `-shm` 一起复制一份带时间戳的副本。
// 3. **删除是软删除**（`deleted_at`），不是 `DELETE` —— 客户端自己也按这个字段过滤，
//    真要恢复还能改回来。
// 4. 每个写操作都先确认目标存在，不存在就报错（不静默成功）。

fn ensure_client_closed(region: QoderRegion) -> Result<(), String> {
    // 写客户端库前先把它关掉（与切号同一套：主动关，不是拒绝）。
    crate::modules::qoder_auth_dat::stop_client(region).map(|_| ())
}

/// 会话写操作的**取证日志**（`~/.twin-switch/qoder-sessions.log`）。
///
/// 用户反复反馈「点了删除没反应 / 重启客户端记录还在」，而界面看不到任何错误。
/// 把每一步（关客户端耗时、备份、影响行数、复查结果）落盘，出问题能直接定位，
/// 不再靠猜。
fn log_line(msg: &str) {
    let dir = crate::modules::config::store_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("qoder-sessions.log");
    let line = format!("{} {msg}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 该区域的客户端此刻是否在跑（按可执行文件名，不跨区）。
fn client_running(region: QoderRegion) -> bool {
    let names: &[&str] = match region {
        QoderRegion::Cn => &["Qoder CN.exe", "Qoder CN Launcher.exe"],
        QoderRegion::Global => &["Qoder.exe", "Qoder Launcher.exe"],
    };
    crate::modules::qoder_auth_dat::client_processes()
        .iter()
        .any(|(name, _)| names.iter().any(|n| name.eq_ignore_ascii_case(n)))
}

/// **写库后自动把客户端拉回来**（用户要求：删/改会话后不能让 Qoder 停着）。
///
/// 用 `Drop` 而不是在每个 `return` 前手动调 —— 中途任何一个 `?` 提前返回
/// 也能保证「关掉的客户端会被重新拉起」，否则用户会遇到「操作失败还把客户端关了」。
struct ResumeClientOnDrop {
    region: QoderRegion,
    was_running: bool,
    /// 是否已经处理过重启（显式 `resume_now` 或确认无需启动）—— 避免 Drop 再启动一次。
    done: bool,
}

impl ResumeClientOnDrop {
    /// **显式重启并把结果带出去**。
    ///
    /// 之前只在 `Drop` 里 `let _ = launch_desktop(...)` —— 启动失败被静默吞掉，
    /// 用户看到的就是「删是删了，但客户端没回来，也没有任何提示」。
    /// 这里把结果返回给调用方（进而返回前端），失败必须能看见。
    ///
    /// ⚠️ `spawn` 成功 ≠ 客户端起来了：Qoder 可能因为残留单实例锁立刻退出，
    /// 所以要**轮询等进程真的出现**（最多 10 秒）。
    fn resume_now(&mut self) -> Result<Value, String> {
        if self.done {
            return Ok(serde_json::json!({ "ok": true, "action": "skipped" }));
        }
        self.done = true;

        // 关完之后如果它又在了（守护/看门狗拉起），不要重复启动，免得双开。
        if client_running(self.region) {
            log_line("[resume] 客户端已在运行，跳过启动");
            return Ok(serde_json::json!({ "ok": true, "action": "already-running" }));
        }

        // ★ 用户要求：**删完/改完一定让客户端处于运行状态**。
        //   之前的逻辑是「原本没开就不启动」，结果用户看到「删了但客户端没回来」，
        //   体感就是「没重启」。既然是我们关的（或本来就没开），就把它拉起来。
        let exe = crate::modules::qoder_auth_dat::launch_desktop(self.region)?;
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(10) {
            if client_running(self.region) {
                log_line(&format!(
                    "[resume] 已启动 {}（{} ms，原状态={}）",
                    exe,
                    t0.elapsed().as_millis(),
                    if self.was_running { "运行中" } else { "未运行" }
                ));
                return Ok(serde_json::json!({
                    "ok": true,
                    "action": "launched",
                    "exe": exe,
                    "wasRunning": self.was_running,
                }));
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let msg = format!("已执行启动命令，但 10 秒内没看到进程出现（{exe}）");
        log_line(&format!("[resume] {msg}"));
        Err(msg)
    }
}

impl Drop for ResumeClientOnDrop {
    fn drop(&mut self) {
        // 兜底：中途 `?` 提前返回时也要把客户端拉回来（结果无法上报，只记日志）。
        if self.was_running && !self.done {
            match crate::modules::qoder_auth_dat::launch_desktop(self.region) {
                Ok(exe) => log_line(&format!("[resume] 兜底重启已发出：{exe}")),
                Err(e) => log_line(&format!("[resume] 兜底重启失败：{e}")),
            }
        }
    }
}

/// 关客户端并返回「原本是否在跑」的恢复守卫。用法：
/// `let _resume = close_client_for_write(region)?;`
fn close_client_for_write(region: QoderRegion) -> Result<ResumeClientOnDrop, String> {
    let was_running = client_running(region);
    // ⚠️ 这里必须调 `ensure_client_closed`，**不能**调 `close_client_for_write` ——
    //    之前批量替换脚本把手写的这一行也换成了自己，造成**无限递归 → 栈溢出**，
    //    后端进程在动手前就崩了，表现就是「点删除没反应、没报错、记录还在」。
    ensure_client_closed(region)?;
    Ok(ResumeClientOnDrop {
        region,
        was_running,
        done: false,
    })
}

/// 写前备份（`main.sqlite` + 同目录的 `-wal` / `-shm`）。返回备份路径。
fn backup_db(region: QoderRegion) -> Result<PathBuf, String> {
    let src = db_path(region).ok_or_else(|| "找不到会话库".to_string())?;

    // ⚠️ 备份写到**工具目录**（`~/.twin-switch/qoder/backups/<区域>/`），
    //    不再写在客户端数据目录里 —— 早期版本写在那里，每删一次就留一份整库
    //    （19MB × N），把 `%APPDATA%\com.qoder.app.stable` 撑到 200MB+，
    //    而清理页只扫工具目录 ⇒ 这堆东西谁都看不见。
    let dir = crate::modules::qoder_cleanup::backup_root().join(region.as_str());
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败：{e}"))?;

    // 毫秒级时间戳：同一秒内连续两次写库不会互相覆盖。
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let dst = dir.join(format!("main.sqlite.bak-{stamp}"));
    std::fs::copy(&src, &dst).map_err(|e| format!("备份会话库失败：{e}"))?;
    for ext in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{ext}", src.display()));
        if from.is_file() {
            let to = PathBuf::from(format!("{}{ext}", dst.display()));
            let _ = std::fs::copy(&from, &to);
        }
    }

    prune_backups(&dir, 5);
    Ok(dst)
}

/// 每个区域只保留最近 `keep` 份备份。
///
/// 每份就是整个库（国际版 19MB），不设上限会无限膨胀 —— Trae 那边踩过
/// 「备份堆到 2.2GB」的坑。文件名里的时间戳是定宽的，字典序即时间序。
fn prune_backups(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut list: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with("main.sqlite.bak-")
                        && !n.ends_with("-wal")
                        && !n.ends_with("-shm")
                })
                .unwrap_or(false)
        })
        .collect();
    list.sort();
    for old in list.into_iter().rev().skip(keep) {
        let _ = std::fs::remove_file(&old);
        for ext in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(PathBuf::from(format!("{}{ext}", old.display())));
        }
    }
}

fn open_rw(region: QoderRegion) -> Result<Connection, String> {
    let path = db_path(region).ok_or_else(|| {
        format!("本机没有 {} 的会话库", region.display_name())
    })?;
    Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("打开会话库（可写）失败：{e}"))
}

fn session_exists(conn: &Connection, session_id: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "select count(*) from chat_sessions where session_id = ?1 and deleted_at is null",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("查询会话失败：{e}"))?;
    Ok(n > 0)
}

/// 改标题。
pub fn rename(region: QoderRegion, session_id: &str, title: &str) -> Result<Value, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("标题不能为空".into());
    }
    let _resume = close_client_for_write(region)?;
    let backup = backup_db(region)?;
    let conn = open_rw(region)?;
    if !session_exists(&conn, session_id)? {
        return Err(format!("找不到会话 {session_id}"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "update chat_sessions set title = ?1, updated_at = ?2 where session_id = ?3",
        rusqlite::params![title, now, session_id],
    )
    .map_err(|e| format!("改标题失败：{e}"))?;
    Ok(serde_json::json!({
        "ok": true,
        "backup": backup.display().to_string(),
        "title": title,
    }))
}

/// 改**一条消息的正文**（只改 `payload_json.parts[]` 里 `type == "text"` 的 `text`）。
///
/// 其它 part（thinking / tool 调用 / 图片）原样保留 —— 直接整段替换 payload 会把
/// 工具调用链和思维链一起抹掉，客户端打开就崩。
pub fn update_message_text(
    region: QoderRegion,
    session_id: &str,
    message_id: &str,
    text: &str,
) -> Result<Value, String> {
    let _resume = close_client_for_write(region)?;
    let backup = backup_db(region)?;
    let conn = open_rw(region)?;

    let payload: Option<String> = conn
        .query_row(
            "select payload_json from chat_session_messages where session_id = ?1 and message_id = ?2",
            rusqlite::params![session_id, message_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional_map()?
        .flatten();
    let Some(payload) = payload else {
        return Err(format!("找不到消息 {message_id}"));
    };
    let mut value: Value = serde_json::from_str(&payload)
        .map_err(|e| format!("这条消息的 payload 不是 JSON：{e}"))?;
    let Some(parts) = value.get_mut("parts").and_then(Value::as_array_mut) else {
        return Err("这条消息没有 parts 数组，无法编辑".into());
    };
    let mut replaced = false;
    for part in parts.iter_mut() {
        if part.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(obj) = part.as_object_mut() {
                obj.insert("text".into(), Value::String(text.to_string()));
                replaced = true;
            }
        }
    }
    if !replaced {
        return Err("这条消息里没有正文段（可能只有思维链/工具调用），不支持编辑".into());
    }
    let serialized = value.to_string();
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "update chat_session_messages set payload_json = ?1, payload_bytes = ?2, updated_at = ?3
         where session_id = ?4 and message_id = ?5",
        rusqlite::params![
            serialized,
            serialized.len() as i64,
            now,
            session_id,
            message_id
        ],
    )
    .map_err(|e| format!("写回消息失败：{e}"))?;
    // 会话本身的 updated_at 也动一下，列表排序才反映「刚编辑过」。
    let _ = conn.execute(
        "update chat_sessions set updated_at = ?1 where session_id = ?2",
        rusqlite::params![now, session_id],
    );
    Ok(serde_json::json!({
        "ok": true,
        "backup": backup.display().to_string(),
        "messageId": message_id,
    }))
}

/// 删除**一条消息**（物理删；这张表没有软删列）。
///
/// ⚠️ 删掉用户提问会让后续助手回复失去上下文，客户端里表现为「答非所问」；
/// 这里不阻拦，但界面上必须提示清楚。
pub fn delete_message(
    region: QoderRegion,
    session_id: &str,
    message_id: &str,
) -> Result<Value, String> {
    let _resume = close_client_for_write(region)?;
    let backup = backup_db(region)?;
    let conn = open_rw(region)?;
    let affected = conn
        .execute(
            "delete from chat_session_messages where session_id = ?1 and message_id = ?2",
            rusqlite::params![session_id, message_id],
        )
        .map_err(|e| format!("删除消息失败：{e}"))?;
    if affected == 0 {
        return Err(format!("找不到消息 {message_id}"));
    }
    Ok(serde_json::json!({
        "ok": true,
        "backup": backup.display().to_string(),
        "removed": affected,
    }))
}

/// **真删**：把会话行与消息行都从客户端库里删掉（写前已备份）。
///
/// ## 为什么不再用软删（`deleted_at`）
///
/// 用户明确要求「删除要和客户端里真的删掉一致」，而软删只是让列表不显示，
/// 消息仍占着库、在客户端的统计/导出里还在。所以这里改成物理删除：
/// `chat_session_messages` 先删（按 session_id），再删 `chat_sessions` 行。
///
/// ⚠️ 仍保留两道保护：**客户端在跑就拒绝**（`ensure_client_closed` 会先主动关掉对应版本），
/// 写前 `backup_db` 生成一致快照 —— 删错了可以从备份整库恢复。
pub fn remove(region: QoderRegion, session_id: &str) -> Result<Value, String> {
    let started = std::time::Instant::now();
    log_line(&format!(
        "[remove] 开始 region={} session={session_id}",
        region.as_str()
    ));

    let mut resume = match close_client_for_write(region) {
        Ok(g) => g,
        Err(e) => {
            log_line(&format!("[remove] 关闭客户端失败，已中止：{e}"));
            return Err(e);
        }
    };
    log_line(&format!(
        "[remove] 客户端已关闭（{} ms）",
        started.elapsed().as_millis()
    ));

    let backup = backup_db(region)?;
    let conn = open_rw(region)?;

    // 一个事务里删两张表，任何一步失败都回滚，不留半条。
    conn.execute_batch("begin immediate")
        .map_err(|e| format!("开启事务失败：{e}"))?;

    let result: Result<(usize, usize), String> = (|| {
        if !session_exists(&conn, session_id)? {
            return Err(format!("找不到会话 {session_id}"));
        }
        let messages = conn
            .execute(
                "delete from chat_session_messages where session_id = ?1",
                rusqlite::params![session_id],
            )
            .map_err(|e| format!("删除消息失败：{e}"))?;
        let sessions = conn
            .execute(
                "delete from chat_sessions where session_id = ?1",
                rusqlite::params![session_id],
            )
            .map_err(|e| format!("删除会话失败：{e}"))?;
        Ok((messages, sessions))
    })();

    let (messages, sessions) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            log_line(&format!("[remove] 失败并回滚：{e}"));
            return Err(e);
        }
    };

    // ★ 删后**复查**：还在就是没删掉，报错而不是假装成功。
    let left_session: i64 = conn
        .query_row(
            "select count(*) from chat_sessions where session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    let left_msgs: i64 = conn
        .query_row(
            "select count(*) from chat_session_messages where session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    if left_session != 0 || left_msgs != 0 {
        let _ = conn.execute_batch("rollback");
        let msg = format!(
            "删除未生效（会话剩 {left_session} 条、消息剩 {left_msgs} 条）\
             ⇒ 客户端可能仍在运行并回写，请退出该版本 Qoder 后重试"
        );
        log_line(&format!("[remove] 复查失败：{msg}"));
        return Err(msg);
    }

    conn.execute_batch("commit")
        .map_err(|e| format!("提交事务失败：{e}"))?;

    // ★ 把 WAL 合并回主库：客户端重启时若从 WAL 读到旧页，删掉的会话会「复活」。
    let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));

    // 显式重启并把结果带出去（失败必须让用户看见，不能静默）。
    let relaunch = match resume.resume_now() {
        Ok(v) => v,
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };

    log_line(&format!(
        "[remove] 成功 会话{sessions} 消息{messages} 用时{}ms 重启={relaunch} 备份={}",
        started.elapsed().as_millis(),
        backup.display()
    ));

    Ok(serde_json::json!({
        "ok": true,
        "backup": backup.display().to_string(),
        "deletedMessages": messages,
        "deletedSessions": sessions,
        "hard": true,
        "verified": true,
        "clientRelaunch": relaunch,
    }))
}

/// 批量删除：一次关客户端、一次备份、一个事务、一次重启。
///
/// ⚠️ 绝不循环调用 [`remove`]：那样每条都要「关客户端 → 备份 → 重启客户端」，
/// 删 10 条就开关 10 次（每次十几秒，还有 10 份全库备份）。这里全程只做一遍。
///
/// ⚠️ 选中的会话在库里不存在时**不算失败**（列表可能是几分钟前读的，期间被客户端删了），
/// 但要如实放进 `missing` 带回去 —— 不能把「跳过了 3 个」说成「删了 10 个」。
pub fn remove_many(region: QoderRegion, session_ids: &[String]) -> Result<Value, String> {
    let ids: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for id in session_ids {
            let t = id.trim();
            if !t.is_empty() && !v.iter().any(|x| x == t) {
                v.push(t.to_string());
            }
        }
        v
    };
    if ids.is_empty() {
        return Err("没有选中任何会话".to_string());
    }
    let started = std::time::Instant::now();
    log_line(&format!(
        "[remove_many] 开始 region={} 数量={}",
        region.as_str(),
        ids.len()
    ));

    let mut resume = match close_client_for_write(region) {
        Ok(g) => g,
        Err(e) => {
            log_line(&format!("[remove_many] 关闭客户端失败，已中止：{e}"));
            return Err(e);
        }
    };
    let backup = backup_db(region)?;
    let conn = open_rw(region)?;

    conn.execute_batch("begin immediate")
        .map_err(|e| format!("开启事务失败：{e}"))?;

    let mut deleted = 0usize;
    let mut deleted_msgs = 0usize;
    let mut missing: Vec<String> = Vec::new();
    let result: Result<(), String> = (|| {
        for id in &ids {
            if !session_exists(&conn, id)? {
                missing.push(id.clone());
                continue;
            }
            deleted_msgs += conn
                .execute(
                    "delete from chat_session_messages where session_id = ?1",
                    rusqlite::params![id],
                )
                .map_err(|e| format!("删除消息失败（{id}）：{e}"))?;
            deleted += conn
                .execute(
                    "delete from chat_sessions where session_id = ?1",
                    rusqlite::params![id],
                )
                .map_err(|e| format!("删除会话失败（{id}）：{e}"))?;
        }
        Ok(())
    })();

    if let Err(e) = result {
        let _ = conn.execute_batch("rollback");
        log_line(&format!("[remove_many] 失败并回滚：{e}"));
        return Err(e);
    }

    // ★ 逐条复查：还在就是没删掉 ⇒ 回滚整批，报错而不是假装成功。
    let mut still: Vec<String> = Vec::new();
    for id in &ids {
        if missing.iter().any(|m| m == id) {
            continue;
        }
        let left: i64 = conn
            .query_row(
                "select count(*) from chat_sessions where session_id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        if left != 0 {
            still.push(format!("{id}（剩 {left} 条）"));
        }
    }
    if !still.is_empty() {
        let _ = conn.execute_batch("rollback");
        let msg = format!(
            "删除未生效，已整批回滚：{} ⇒ 客户端可能仍在运行并回写，请退出该版本 Qoder 后重试",
            still.join("、")
        );
        log_line(&format!("[remove_many] 复查失败：{msg}"));
        return Err(msg);
    }

    conn.execute_batch("commit")
        .map_err(|e| format!("提交事务失败：{e}"))?;
    let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));

    let relaunch = match resume.resume_now() {
        Ok(v) => v,
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };

    log_line(&format!(
        "[remove_many] 成功 会话{} 消息{} 跳过{} 用时{}ms 重启={} 备份={}",
        deleted,
        deleted_msgs,
        missing.len(),
        started.elapsed().as_millis(),
        relaunch,
        backup.display()
    ));

    Ok(serde_json::json!({
        "ok": true,
        "backup": backup.display().to_string(),
        "deletedSessions": deleted,
        "deletedMessages": deleted_msgs,
        "missing": missing,
        "requested": ids.len(),
        "hard": true,
        "verified": true,
        "clientRelaunch": relaunch,
    }))
}

/// 复制会话（同库内新建一条 + 复制全部消息）。标题加「（副本）」。
pub fn duplicate(region: QoderRegion, session_id: &str) -> Result<Value, String> {
    ensure_closed_and_backup(region)?;
    let conn = open_rw(region)?;
    if !session_exists(&conn, session_id)? {
        return Err(format!("找不到会话 {session_id}"));
    }

    let new_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    // 复制会话行：把 session_id 换掉、标题加后缀、时间刷新。
    let cols: Vec<String> = conn
        .prepare("pragma table_info(chat_sessions)")
        .and_then(|mut st| {
            let rows = st.query_map([], |r| r.get::<_, String>(1))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .map_err(|e| format!("读取会话表结构失败：{e}"))?;
    let select_cols: Vec<String> = cols
        .iter()
        .map(|c| {
            if c == "session_id" {
                format!("'{new_id}'")
            } else if c == "title" {
                format!("coalesce(title, '') || '（副本）'")
            } else if c == "created_at" || c == "updated_at" {
                now.to_string()
            } else {
                format!("\"{c}\"")
            }
        })
        .collect();
    let sql = format!(
        "insert into chat_sessions ({}) select {} from chat_sessions where session_id = ?1",
        cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(","),
        select_cols.join(",")
    );
    conn.execute(&sql, [session_id])
        .map_err(|e| format!("复制会话失败：{e}"))?;

    // 复制消息：同样换 session_id 与 message_id。
    let msg_cols: Vec<String> = conn
        .prepare("pragma table_info(chat_session_messages)")
        .and_then(|mut st| {
            let rows = st.query_map([], |r| r.get::<_, String>(1))?;
            Ok(rows.filter_map(Result::ok).collect())
        })
        .map_err(|e| format!("读取消息表结构失败：{e}"))?;
    let msg_select: Vec<String> = msg_cols
        .iter()
        .map(|c| {
            if c == "session_id" {
                format!("'{new_id}'")
            } else if c == "message_id" {
                "lower(hex(randomblob(16)))".to_string()
            } else {
                format!("\"{c}\"")
            }
        })
        .collect();
    let msg_sql = format!(
        "insert into chat_session_messages ({}) select {} from chat_session_messages where session_id = ?1",
        msg_cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(","),
        msg_select.join(",")
    );
    conn.execute(&msg_sql, [session_id])
        .map_err(|e| format!("复制消息失败：{e}"))?;

    Ok(serde_json::json!({ "ok": true, "id": new_id, "title": "（副本）" }))
}

fn ensure_closed_and_backup(region: QoderRegion) -> Result<PathBuf, String> {
    let _resume = close_client_for_write(region)?;
    backup_db(region)
}

/// 两个版本各有多少会话（界面上的计数用）。
pub fn counts() -> Value {
    let mut out = serde_json::Map::new();
    for r in [QoderRegion::Cn, QoderRegion::Global] {
        let n = list(r, true).map(|v| v.len()).unwrap_or(0);
        out.insert(r.as_str().to_string(), serde_json::json!(n));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_db_is_reported_not_empty() {
        // 正常情况下本机有库；这里只验证「路径找不到时」的分支不 panic。
        for r in [QoderRegion::Cn, QoderRegion::Global] {
            if db_path(r).is_none() {
                let err = list(r, false).unwrap_err();
                assert!(err.contains("没有"), "{err}");
            }
        }
    }

    #[test]
    fn paths_point_at_each_versions_own_db() {
        let cn = db_path(QoderRegion::Cn);
        let gl = db_path(QoderRegion::Global);
        if let (Some(a), Some(b)) = (cn, gl) {
            assert_ne!(a, b, "两个版本必须各读各的库");
            assert!(a.to_string_lossy().contains("com.qodercn"));
            assert!(b.to_string_lossy().contains("com.qoder.app"));
        }
    }
}

#[cfg(test)]
mod delete_probe {
    use super::*;

    /// 真机自检：**造一条假会话**（不动用户数据）→ 调 `remove` → 直接查库复查。
    ///
    /// 这是「删除到底生不生效」的唯一判据。会关掉该版本客户端（DROP 守卫会重启）。
    #[test]
    #[ignore]
    fn probe_delete_really_removes() {
        let region = QoderRegion::Global;
        let id = format!("twinswitch-delete-probe-{}", uuid::Uuid::new_v4());
        {
            let conn = open_rw(region).expect("打开可写库");
            let now = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "insert into chat_sessions (session_id, title, cwd, created_at, updated_at, archived)
                 values (?1, ?2, ?3, ?4, ?4, 0)",
                rusqlite::params![id, "TwinSwitch 删除自检（可忽略）", "D:/tmp", now],
            )
            .expect("插入假会话");
            eprintln!("[probe] 已插入假会话 {id}");
        }

        match super::remove(region, &id) {
            Ok(v) => eprintln!("[probe] remove 返回：{}", serde_json::to_string(&v).unwrap()),
            Err(e) => eprintln!("[probe] remove 失败：{e}"),
        }

        // 用**只读连接**复查（不复用写入连接，避免缓存误导）。
        let conn = super::open(region).expect("只读打开");
        let left: i64 = conn
            .query_row(
                "select count(*) from chat_sessions where session_id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        eprintln!("[probe] 复查：库里剩 {left} 条（0 = 真删掉）");

        // 清理残留（如果 remove 失败，别把假数据留给用户）。
        if left > 0 {
            if let Ok(c) = super::open_rw(region) {
                let _ = c.execute(
                    "delete from chat_sessions where session_id = ?1",
                    rusqlite::params![id],
                );
                eprintln!("[probe] 已清理残留假会话");
            }
        }
    }
}
