//! Trae → WorkBuddy 会话移植（`workbuddy_import` 的反向）。
//!
//! 把 Trae SQLCipher 关系库里的会话**转换**成 WorkBuddy 的明文会话记录，写入本机
//! WorkBuddy 数据根（`~/.workbuddy`）。方向与 `workbuddy_import` 相反，两侧格式互为逆映射。
//!
//! ## WorkBuddy 里「一个会话」= 两样东西（缺一不可）
//!
//! | 组成 | 位置 |
//! | --- | --- |
//! | 正文 | `~/.workbuddy/projects/{工作区key}/{会话id}.jsonl`（一行一个事件） |
//! | 元数据 | `~/.workbuddy/workbuddy.db` → `sessions` 表一行 |
//!
//! **工作区 key** = cwd 小写后删掉盘符冒号，再把 `\` `/` 换成 `-`
//! （`D:\htw\test` → `d-htw-test`，`D:/htw/签到/trae-switch-cn` → `d-htw-签到-trae-switch-cn`）。
//! 工作区子目录只是分类、不参与识别，所以 key 算错也能被读到，但会话会被归到陌生的分组下。
//!
//! ## 正文事件骨架（取自真实原生会话，按原样复刻）
//!
//! ```text
//! session-meta ×2         （进程/宿主元信息，成对出现）
//! message(user)           content=[{type:"input_text", text}]
//! file-history-snapshot   （每条用户消息后跟一条，trackedFileBackups 可为空对象）
//! reasoning               rawContent=[{type:"reasoning_text", text}]
//! message(assistant)      content=[{type:"output_text", text, providerData:{annotations:[]}}]
//! function_call           arguments 是 JSON **字符串**，callId 用于与结果配对
//! function_call_result    output={type:"text", text}
//! ...
//! ai-title                aiTitle，放在文件末尾
//! ```
//!
//! ## Trae 侧的字段来源（与 `workbuddy_import` 的写入位置一一对应）
//!
//! | 内容 | Trae 位置 |
//! | --- | --- |
//! | 用户提问 | `chat_message_general.content` |
//! | 助手过程叙述 | `plan_item.thought` |
//! | 思考 | `plan_item.reasoning_content` |
//! | 工具调用 | `plan_item.tool_call_info.params` + `meta.llm_toolcall_id` |
//! | 工具结果 | `plan_item.tool_call_info.result` |
//! | 回合最终回答 | 末项 `plan_item`（`tool_call_info.name == "finish"`）的 `params.summary` |
//!
//! ## 三条设计约束
//!
//! 1. **会话 id 必须确定性派生**。`uuid5(trae-wb:{client_key}:{session_id})` 再把版本位
//!    改写为 4（外形与原生随机 UUID 一致），于是「同一 Trae 会话重复导出」命中同一条
//!    WorkBuddy 会话，走 `INSERT OR REPLACE` 覆盖而不是堆出重复项——这与反向导入的
//!    `det_session_id` 是同一个思路。
//! 2. **只借模板的静态字段，绝不照抄上下文类字段**。以 `sessions` 表最近一条真实行为模板，
//!    只取 `status/model/mode/permission_mode/...` 这类与「本会话内容」无关的静态列；
//!    `addon_selection` / `buddy_binding_json` / `group_*` / `expert_*` / `session_settings`
//!    这类含工作区、场景、绑定关系的列一律留空——照抄会让导入的会话冒充别的会话上下文
//!    （反向导入曾经就栽在这里，见 `workbuddy_import` 第 5 条硬约束）。
//! 3. **写盘顺序：先正文、后数据库**。万一插 `sessions` 行失败，多出来的 JSONL 在客户端
//!    是不可见的（列表读的是库表），属于无害残骸；反过来则会留下一条打不开的幽灵会话。
//!    数据库写失败时回滚到备份，并清掉本轮新建的 JSONL。
//!
//! ## 为什么要退出 WorkBuddy
//!
//! `workbuddy.db` 跑在 WAL 模式，且客户端会把会话列表缓存在内存里。导出前先把
//! WorkBuddy 进程结束（写完再拉起），既避免客户端退出时用旧快照覆盖新行，也保证
//! 重新打开后列表立刻就带上导入的会话——与「导入 Trae 前先退出 Trae 客户端」同构。

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::modules::config::store_dir;
use crate::modules::process_list;
use crate::modules::trae_export;
use crate::modules::workbuddy_accounts;
use crate::modules::workbuddy_source;

/// WorkBuddy 主程序镜像名（Electron，多个进程共用同一镜像名）。
const WB_IMAGE: &str = "WorkBuddy.exe";

/// 一次导出任务的互斥标记（与反向导入同理：并发导出会互相覆盖数据库）。
static EXPORTING: AtomicBool = AtomicBool::new(false);

struct ExportGuard;

impl Drop for ExportGuard {
    fn drop(&mut self) {
        EXPORTING.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// 基础工具
// ---------------------------------------------------------------------------

/// 确定性会话 id：把 Trae 会话稳定映射成一个「看起来像 v4」的 UUID。
///
/// 版本位改写为 4 只是为了让外形与 WorkBuddy 原生 id 一致（原生 id 是随机 v4），
/// 取值本身仍完全由 (client_key, session_id) 决定，重复导出因此天然幂等。
fn det_session_uuid(client_key: &str, session_id: &str) -> String {
    let name = format!("trae-wb:{client_key}:{session_id}");
    let mut b = *uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).as_bytes();
    b[6] = (b[6] & 0x0F) | 0x40; // version 4
    b[8] = (b[8] & 0x3F) | 0x80; // RFC 4122 variant
    uuid::Uuid::from_bytes(b).to_string()
}

/// 事件 id：UUID v7（48 位毫秒时间戳 + 随机），与真实会话里的 `01a1067e-…` 同构。
fn event_id(ms: i64) -> String {
    let mut b = [0u8; 16];
    if getrandom::getrandom(&mut b).is_err() {
        b = *uuid::Uuid::new_v4().as_bytes();
    }
    let ts = (ms.max(0) as u64) & 0x0000_FFFF_FFFF_FFFF;
    b[0] = (ts >> 40) as u8;
    b[1] = (ts >> 32) as u8;
    b[2] = (ts >> 24) as u8;
    b[3] = (ts >> 16) as u8;
    b[4] = (ts >> 8) as u8;
    b[5] = ts as u8;
    b[6] = (b[6] & 0x0F) | 0x70; // version 7
    b[8] = (b[8] & 0x3F) | 0x80; // variant
    uuid::Uuid::from_bytes(b).to_string()
}

/// 工作区目录 key：cwd 小写后**去掉盘符冒号**，再把路径分隔符换成 `-`。
///
/// 注意不能「把 `:` 也换成 `-`」：`D:\htw\test` 会变成 `d--htw-test`（两个短横线），
/// 与实测目录名 `d-htw-test` 不符。正确规则是 `d:` + `\` 合并成一个 `-`，
/// 等价于「删掉 `:`，再把 `\` `/` 换成 `-`」。首版为此写错，被单测挡下。
fn workspace_key(cwd: &str) -> String {
    cwd.trim()
        .to_ascii_lowercase()
        .replace(':', "")
        .replace(|c| c == '\\' || c == '/', "-")
}

/// 事件里 `cwd` 的写法：盘符小写 + 反斜杠（实测原生会话用 `d:\htw\签到`，
/// 而 `sessions.cwd` 存的是用户原样选择的 `D:/htw/签到`）。
fn norm_cwd(p: &str) -> String {
    let s = p.trim().replace('/', "\\");
    let mut cs: Vec<char> = s.chars().collect();
    if cs.len() >= 2 && cs[1] == ':' {
        cs[0] = cs[0].to_ascii_lowercase();
    }
    cs.into_iter().collect()
}

/// SQLite 时间列 → 毫秒（Trae 用秒，个别版本用毫秒）。
fn ts_to_ms(v: &SqlValue) -> i64 {
    match v {
        SqlValue::Integer(i) => {
            if *i > 1_000_000_000_000 {
                *i
            } else {
                *i * 1000
            }
        }
        SqlValue::Real(f) => {
            let i = *f as i64;
            if i > 1_000_000_000_000 {
                i
            } else {
                i * 1000
            }
        }
        _ => 0,
    }
}

fn truncate_utf8(s: &str, max: usize) -> String {
    trae_export::truncate_utf8(s, max)
}

// ---------------------------------------------------------------------------
// 进程控制（WorkBuddy.exe：tasklist / taskkill / 直接拉起）
// ---------------------------------------------------------------------------

fn run_cmd(program: &str, args: &[String], timeout: Duration) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => return None,
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 正在运行的 WorkBuddy 进程 [(名称, pid)]。
///
/// 走 [`process_list::find_by_names`]：一次全量快照后内存过滤，
/// 不再为此起一次 `tasklist` 子进程。
pub fn wb_processes() -> Vec<(String, u32)> {
    process_list::find_by_names(&[WB_IMAGE.to_string()])
}

pub fn wb_is_running() -> bool {
    !wb_processes().is_empty()
}

/// 结束全部 WorkBuddy 进程（Electron 是多进程，只对**树根**发一次 `taskkill /T /F`）。
///
/// 只杀一轮、不等；要「杀到真的没了」用 [`wb_quit`]。
/// 返回出现过的映像名列表（供调用方记日志）。
pub(crate) fn wb_kill_all() -> Vec<String> {
    process_list::kill_now(&[WB_IMAGE.to_string()]).0
}

/// 结束 WorkBuddy 并**确认真的退出**：反复重试到匹配集为空或超时。
///
/// 这是「退出客户端」的唯一正确入口 —— 详情见
/// [`process_list::kill_tree_and_wait`]（为什么要重试、如何区分「杀不掉」与
/// 「被杀掉后又被更新器拉起来」）。
pub(crate) fn wb_quit(timeout: Duration) -> process_list::KillOutcome {
    process_list::kill_tree_and_wait(&[WB_IMAGE.to_string()], timeout, &|m| m.is_empty())
}

pub(crate) fn wb_wait_stopped(timeout_ms: u64) -> bool {
    wb_quit(Duration::from_millis(timeout_ms)).ok
}

/// 定位 WorkBuddy 主程序：优先常见安装位置，再退回询问一次正在运行的进程。
pub fn resolve_wb_exe() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(pf) = std::env::var("ProgramFiles") {
        candidates.push(PathBuf::from(pf).join("WorkBuddy").join(WB_IMAGE));
    }
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(la).join("Programs").join("WorkBuddy").join(WB_IMAGE));
    }
    for c in &candidates {
        if c.is_file() {
            return Some(c.clone());
        }
    }
    // 兜底：问一次正在运行的进程（Electron 多进程，取第一个能读到路径的）
    if let Some(out) = run_cmd(
        "powershell",
        &[
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!("(Get-Process -Name '{WB_IMAGE}' -ErrorAction SilentlyContinue | Where-Object {{ $_.Path }} | Select-Object -First 1).Path"),
        ],
        Duration::from_secs(20),
    ) {
        let p = out.trim().trim_matches('"').to_string();
        if !p.is_empty() && Path::new(&p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

pub(crate) fn wb_launch() -> Result<PathBuf, String> {
    let exe = resolve_wb_exe().ok_or("未找到 WorkBuddy 主程序（WorkBuddy.exe），请手动启动")?;
    use std::process::Command;
    let mut cmd = Command::new(&exe);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd.spawn().map_err(|e| format!("启动 {} 失败: {e}", exe.display()))?;
    Ok(exe)
}

// ---------------------------------------------------------------------------
// WorkBuddy 侧信息：数据根 / 目标账号
// ---------------------------------------------------------------------------

fn data_root() -> PathBuf {
    workbuddy_source::data_root()
}

fn projects_dir() -> PathBuf {
    data_root().join("projects")
}

fn db_path() -> PathBuf {
    data_root().join("workbuddy.db")
}

/// 只读打开 workbuddy.db（WAL 允许并发读，不会与运行中的客户端争抢写锁）。
pub(crate) fn open_wb_db_ro() -> Result<Connection, String> {
    let p = db_path();
    if !p.is_file() {
        return Err(format!("未找到 WorkBuddy 会话库：{}", p.display()));
    }
    Connection::open_with_flags(
        &p,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("打开 WorkBuddy 会话库失败: {e}"))
}

/// 本机登录过的账号 uid（委托给 `workbuddy_accounts`，避免两处各写一份规则）。
fn known_uids() -> Vec<String> {
    workbuddy_accounts::known_uids()
}

/// WorkBuddy 侧现状：是否可用、数据根、客户端是否在运行、可选目标账号。
pub fn target_info() -> Value {
    let available = workbuddy_source::is_available();
    let mut accounts: Vec<Value> = Vec::new();
    let mut default_uid = String::new();

    if available {
        // 会话数按账号统计，用于给账号排序（读不到就退化为 0，不影响导出）
        let mut counts: Vec<(String, i64, i64)> = Vec::new(); // (uid, sessions, last_ms)
        if let Ok(conn) = open_wb_db_ro() {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT COALESCE(user_id,''), count(*), COALESCE(max(updated_at),0) \
                 FROM sessions GROUP BY COALESCE(user_id,'')",
            ) {
                if let Ok(rows) = stmt.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
                }) {
                    counts = rows.flatten().collect();
                }
            }
            // 最近活动账号 = 最新一条会话的归属，作为默认值
            if let Ok(uid) = conn.query_row(
                "SELECT COALESCE(user_id,'') FROM sessions \
                 WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            ) {
                default_uid = uid;
            }
        }

        let mut uids = known_uids();
        for (uid, _, _) in &counts {
            if !uid.trim().is_empty() && !uids.contains(uid) {
                uids.push(uid.clone());
            }
        }
        // 账号显示名：多来源合并（账号快照 / 切换工具账号库 / 运行日志），绝不编造。
        let names = workbuddy_accounts::resolve();
        let find_name = |uid: &str| names.iter().find(|a| a.uid == uid).cloned();
        for uid in uids {
            let hit = counts.iter().find(|(u, _, _)| u == &uid);
            let info = find_name(&uid);
            accounts.push(json!({
                "uid": uid,
                // 主显示名：`13780001455（uid …97eac1）`
                "label": info.as_ref().map(|a| a.label())
                    .unwrap_or_else(|| format!("uid …{}", uid_suffix(&uid))),
                // 纯名字（不含 uid），供前端做标题
                "name": info.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                // 次要信息：手机号 · 类型 · 版本
                "meta": info.as_ref().map(|a| a.meta()).unwrap_or_default(),
                "kind": info.as_ref().map(|a| a.kind.clone()).unwrap_or_default(),
                "edition": info.as_ref().map(|a| a.edition.clone()).unwrap_or_default(),
                "name_source": info.as_ref().map(|a| a.source.clone()).unwrap_or_default(),
                "is_current": uid == default_uid,
                "sessions": hit.map(|(_, c, _)| *c).unwrap_or(0),
                "last_active_ms": hit.map(|(_, _, m)| *m).unwrap_or(0),
            }));
        }
        // 默认账号排最前
        accounts.sort_by(|a, b| {
            let ac = a.get("is_current").and_then(Value::as_bool).unwrap_or(false);
            let bc = b.get("is_current").and_then(Value::as_bool).unwrap_or(false);
            bc.cmp(&ac).then_with(|| {
                b.get("last_active_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .cmp(&a.get("last_active_ms").and_then(Value::as_i64).unwrap_or(0))
            })
        });
        if default_uid.is_empty() {
            default_uid = accounts
                .first()
                .and_then(|a| a.get("uid"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
    }

    json!({
        "available": available,
        "data_root": data_root().to_string_lossy(),
        "db_path": db_path().to_string_lossy(),
        "projects_dir": projects_dir().to_string_lossy(),
        "running": wb_is_running(),
        "exe": resolve_wb_exe().map(|p| p.to_string_lossy().into_owned()),
        "accounts": accounts,
        "default_uid": default_uid,
    })
}

fn uid_suffix(uid: &str) -> String {
    uid.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect()
}

// ---------------------------------------------------------------------------
// 源：Trae 会话列表
// ---------------------------------------------------------------------------

/// 列出某个 Trae 客户端里可导出的会话（带工作目录，供前端勾选与预览归属工作区）。
///
/// 只做**廉价**查询：轮数用 SQL 直接数用户消息，不解析正文——整库逐条解析 `task` 内容
/// 会让列表变得很慢（该项目此前正是栽在「列表页做重活」上）。
pub fn list_source(client_key: &str) -> Result<Value, String> {
    let label = trae_export::label_of(client_key);
    let conn = trae_export::open_decrypted(client_key)?;
    let mut stmt = conn
        .prepare(
            "SELECT s.session_id, COALESCE(s.session_title,''), s.created_at, s.updated_at, \
                    COALESCE(p.absolute_path,''), COALESCE(p.user_id,''), \
                    (SELECT count(*) FROM chat_message m \
                      WHERE m.session_id = s.session_id AND m.message_role='user' \
                        AND ifnull(m.deleted_at,0)=0) \
             FROM chat_session s \
             LEFT JOIN project p ON s.project_id = p.project_id \
             ORDER BY ifnull(s.updated_at, s.created_at) DESC",
        )
        .map_err(|e| format!("会话列表查询失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, SqlValue>(2)?,
                r.get::<_, SqlValue>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(|e| format!("会话列表查询失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("会话列表读取失败: {e}"))?;

    let sessions: Vec<Value> = rows
        .into_iter()
        .map(|(id, title, created, updated, cwd, owner_uid, turns)| {
            json!({
                "id": id,
                "title": if title.trim().is_empty() { "（无标题）".to_string() } else { title.trim().to_string() },
                "cwd": cwd,
                "workspace_key": if cwd.trim().is_empty() { String::new() } else { workspace_key(&cwd) },
                "created_ms": ts_to_ms(&created),
                "updated_ms": ts_to_ms(&updated),
                "turns": turns,
                "owner_uid": owner_uid,
                "owner_label": if owner_uid.is_empty() {
                    "（无归属）".to_string()
                } else {
                    crate::modules::trae_vault::account_label(client_key, &owner_uid)
                },
            })
        })
        .collect();

    Ok(json!({
        "client_key": client_key,
        "client_label": label,
        "data_root": projects_dir().to_string_lossy(),
        "count": sessions.len(),
        "sessions": sessions,
    }))
}

// ---------------------------------------------------------------------------
// Trae 回合 → WorkBuddy 事件
// ---------------------------------------------------------------------------

/// `runtime/` 之外的过程规划项 → 事件序列（按 plan_item 原始顺序）。
struct TurnBuilder {
    session_id: String,
    cwd: String,
    events: Vec<Value>,
    /// 用户提问数（= 回合数）
    turns: usize,
    tool_steps: usize,
}

impl TurnBuilder {
    fn new(session_id: &str, cwd: &str, start_ms: i64) -> Self {
        let mut events = Vec::new();
        // 真实会话开头成对出现两条 session-meta（宿主/进程元信息），照原样复刻。
        for _ in 0..2 {
            events.push(json!({
                "type": "session-meta",
                "id": event_id(start_ms),
                "sessionId": session_id,
                "timestamp": start_ms,
                "meta": { "codebuddy.ai/hostKind": "unopted" },
            }));
        }
        Self {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            events,
            turns: 0,
            tool_steps: 0,
        }
    }

    fn provider_data(&self, ms: i64) -> Value {
        json!({
            "conversationRequestId": event_id(ms).replace('-', ""),
            "messageId": event_id(ms).replace('-', ""),
            "model": "trae-import",
            "agent": "cli",
        })
    }

    fn push_user(&mut self, text: &str, ms: i64) {
        let id = event_id(ms);
        self.events.push(json!({
            "id": id,
            "timestamp": ms,
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }],
            "providerData": self.provider_data(ms),
            "__codebuddyLocal": { "sensitiveUserInputReviewed": true },
            "sessionId": self.session_id,
            "cwd": self.cwd,
        }));
        // 每条用户消息后跟一条文件历史快照（原生会话如此；内容可为空）
        self.events.push(json!({
            "timestamp": ms.saturating_add(1),
            "type": "file-history-snapshot",
            "messageId": id,
            "isSnapshotUpdate": false,
            "snapshot": { "messageId": id, "trackedFileBackups": {} },
            "cwd": self.cwd,
        }));
    }

    fn push_reasoning(&mut self, text: &str, ms: i64) {
        self.events.push(json!({
            "id": event_id(ms),
            "timestamp": ms,
            "type": "reasoning",
            "providerData": { "agent": "cli" },
            "content": [],
            "rawContent": [{ "type": "reasoning_text", "text": text }],
            "sessionId": self.session_id,
            "cwd": self.cwd,
        }));
    }

    fn push_assistant(&mut self, text: &str, ms: i64) {
        self.events.push(json!({
            "id": event_id(ms),
            "timestamp": ms,
            "type": "message",
            "role": "assistant",
            "providerData": self.provider_data(ms),
            "status": "completed",
            "content": [{
                "providerData": { "annotations": [] },
                "type": "output_text",
                "text": text,
            }],
            "sessionId": self.session_id,
            "cwd": self.cwd,
        }));
    }

    fn push_tool(&mut self, name: &str, call_id: &str, arguments: &str, ms: i64) {
        let id = event_id(ms);
        self.events.push(json!({
            "id": id,
            "timestamp": ms,
            "type": "function_call",
            "providerData": { "agent": "cli" },
            "callId": call_id,
            "name": name,
            "arguments": arguments,
            "sessionId": self.session_id,
            "cwd": self.cwd,
        }));
        self.tool_steps += 1;
    }

    fn push_tool_result(&mut self, name: &str, call_id: &str, text: &str, status: &str, ms: i64) {
        self.events.push(json!({
            "id": event_id(ms),
            "timestamp": ms,
            "type": "function_call_result",
            "name": name,
            "callId": call_id,
            "status": status,
            "output": { "type": "text", "text": text },
            "providerData": { "agent": "cli" },
            "sessionId": self.session_id,
            "cwd": self.cwd,
        }));
    }
}

/// 按 WorkBuddy 的解析语义（见 `workbuddy_source::parse_body`）统计「有最终回答的回合数」。
///
/// 不能简单数 `finish` 项：一个 Trae 回合里可能有**多条** assistant 消息、各自带一个
/// `finish`（工具打断后续写时就是这样），而 WorkBuddy 只把「下一条用户消息之前的最后一条
/// assistant 消息」当成该回合的最终回答。这里按同一语义重算，保证计数口径与读取口径一致。
fn count_answered(events: &[Value]) -> usize {
    let mut answered = 0usize;
    let mut in_turn = false;
    let mut has_answer = false;
    for e in events {
        if e.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        match e.get("role").and_then(Value::as_str).unwrap_or("") {
            "user" => {
                if in_turn && has_answer {
                    answered += 1;
                }
                in_turn = true;
                has_answer = false;
            }
            "assistant" => has_answer = true,
            _ => {}
        }
    }
    if in_turn && has_answer {
        answered += 1;
    }
    answered
}

/// 从 `tool_call_info.result` 里抽一段可读文本。
///
/// Trae 的结果是结构化 `data`（例如 `{todo_list:{...}}`），没有统一的 `text` 字段；
/// 这里按常见键优先取字符串，取不到就把 `data` 紧凑序列化（超长截断）。
fn tool_result_text(result: &Value) -> String {
    if let Some(err) = result.get("error_message").and_then(Value::as_str) {
        if !err.trim().is_empty() {
            return err.trim().to_string();
        }
    }
    let data = result.get("data").unwrap_or(&Value::Null);
    for key in ["text", "output", "content", "result", "summary", "stdout", "message"] {
        if let Some(s) = data.get(key).and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return truncate_utf8(s, 200_000);
            }
        }
    }
    if let Some(s) = result.get("render").and_then(Value::as_str) {
        if !s.trim().is_empty() {
            return truncate_utf8(s, 200_000);
        }
    }
    if data.is_null() {
        return String::new();
    }
    let s = serde_json::to_string(data).unwrap_or_default();
    truncate_utf8(&s, 200_000)
}

/// `finish.summary` 为空时的兜底：从 `history_v2` 取该助手消息的可见文本。
///
/// 绝大多数真实会话的最终回答就在 `finish.params.summary` 里；少数会话（例如被中断后重试、
/// 或早期版本写入的记录）只在 `history_v2` 里留了正文。取**最后一段非空文本**而不是整段
/// 拼接——拼接会把中间叙述一起带进来，与前面已经写出的 `thought` 重复。
fn history_fallback(conn: &Connection, mid: &str) -> String {
    let Ok(mut stmt) = conn.prepare(
        "SELECT messages FROM history_v2 WHERE message_id=? AND ifnull(deleted_at,0)=0 ORDER BY id",
    ) else {
        return String::new();
    };
    let Ok(rows) = stmt.query_map([mid], |r| r.get::<_, String>(0)) else {
        return String::new();
    };
    let mut last = String::new();
    for raw in rows.flatten() {
        let t = trae_export::assistant_text(&raw);
        if !t.trim().is_empty() {
            last = t;
        }
    }
    last
}

/// 把一条 assistant 的 `chat_message_task.content` 还原成 WorkBuddy 事件。
fn push_assistant_task(
    b: &mut TurnBuilder,
    conn: &Connection,
    mid: &str,
    raw: &str,
    fallback_ms: i64,
) {
    let Ok(data) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let Some(items) = data.get("messages").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(pi) = item.get("plan_item") else {
            continue;
        };
        let ti = pi.get("tool_call_info").unwrap_or(&Value::Null);
        let name = ti.get("name").and_then(Value::as_str).unwrap_or("");
        let ms = pi
            .get("timing")
            .and_then(|t| t.get("generated_at_ms"))
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .map(|v| if v > 1_000_000_000_000 { v } else { v * 1000 })
            .unwrap_or(fallback_ms);

        let reasoning = pi.get("reasoning_content").and_then(Value::as_str).unwrap_or("").trim();
        if !reasoning.is_empty() {
            b.push_reasoning(reasoning, ms);
        }

        if name == "finish" {
            // 末项的 finish 承载**该回合的最终回答**（见 workbuddy_import 第 6 条硬约束）
            let mut summary = ti
                .get("params")
                .and_then(|p| p.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if summary.is_empty() {
                summary = history_fallback(conn, mid);
            }
            if !summary.trim().is_empty() {
                b.push_assistant(&summary, ms);
            }
            continue;
        }

        // 过程叙述：中间那条 assistant message（WorkBuddy 里就是一条 assistant message）
        let thought = pi.get("thought").and_then(Value::as_str).unwrap_or("").trim();
        if !thought.is_empty() {
            b.push_assistant(thought, ms);
        }

        if name.is_empty() {
            continue;
        }
        let params = ti.get("params").cloned().unwrap_or_else(|| json!({}));
        let arguments = match &params {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let call_id = ti
            .get("meta")
            .and_then(|m| m.get("llm_toolcall_id"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("call_{}", event_id(ms).replace('-', "")));
        b.push_tool(name, &call_id, &arguments, ms);

        let result = ti.get("result").unwrap_or(&Value::Null);
        if !result.is_null() {
            let status_raw = result.get("status").and_then(Value::as_str).unwrap_or("");
            let status = match status_raw {
                "failed" | "error" => "failed",
                _ => "completed",
            };
            b.push_tool_result(name, &call_id, &tool_result_text(result), status, ms.saturating_add(1));
        }
    }
}

/// 把 Trae 的 `chat_message_general.content` 还原成用户提问原文。
fn user_text(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Array(arr)) => {
            let mut parts: Vec<String> = Vec::new();
            for p in arr {
                if let Value::Object(m) = p {
                    let t = m
                        .get("text_content")
                        .and_then(Value::as_str)
                        .or_else(|| m.get("text").and_then(Value::as_str));
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

/// 一个已转换好、等待落盘的 WorkBuddy 会话。
#[derive(Debug, Clone)]
pub struct Converted {
    /// 源 Trae 会话 id。
    pub trae_id: String,
    /// 目标 WorkBuddy 会话 id（确定性 UUID）。
    pub wb_id: String,
    pub title: String,
    /// WorkBuddy `sessions.cwd`（沿用 Trae 会话的原工作目录）。
    pub cwd: String,
    /// 正文会落到的 `projects/{key}/` 目录名。
    pub workspace_key: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub turns: usize,
    /// 写出了最终回答的回合数（`turns - answered` = 源会话本身就没回答的回合）。
    pub answered: usize,
    pub tool_steps: usize,
    /// 逐行 JSONL 事件。
    pub lines: Vec<Value>,
}

impl Converted {
    /// 正文序列化后的字节数（预览用）。
    pub fn bytes(&self) -> usize {
        self.lines
            .iter()
            .map(|l| serde_json::to_string(l).map(|s| s.len() + 1).unwrap_or(0))
            .sum()
    }
}

/// 转换结果：成功项 + 被跳过的会话及原因。
pub struct Conversion {
    pub items: Vec<Converted>,
    /// `(会话 id, 跳过原因)`
    pub skipped: Vec<(String, String)>,
}

/// 把选中的 Trae 会话转换成 WorkBuddy 事件序列（**只读**，不写任何文件）。
///
/// 导出、预览、离线校验三条路径都走它，保证「预览看到的」就是「实际写出去的」。
///
/// 单条会话失败或没有用户提问只记进 `skipped`，不打断整批——列表可能已过期，
/// 也可能有会话本身就没有正文。
pub fn convert(client_key: &str, session_ids: &[String]) -> Result<Conversion, String> {
    let conn = trae_export::open_decrypted(client_key)?;
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    for sid in session_ids {
        match prepare_session(&conn, client_key, sid) {
            Ok(p) if p.turns == 0 => skipped.push((sid.clone(), "没有用户提问".to_string())),
            Ok(p) => items.push(p),
            Err(e) => skipped.push((sid.clone(), e)),
        }
    }
    Ok(Conversion { items, skipped })
}

/// 只读预览：报告每个会话会导出多少回合 / 工具步骤 / 落到哪个工作区，不写任何文件。
pub fn preview(client_key: &str, session_ids: &[String]) -> Result<Value, String> {
    let conv = convert(client_key, session_ids)?;
    Ok(json!({
        "preview": conv.items.iter().map(|c| json!({
            "trae_id": c.trae_id,
            "workbuddy_id": c.wb_id,
            "title": c.title,
            "cwd": c.cwd,
            "workspace_key": c.workspace_key,
            "turns": c.turns,
            "answered": c.answered,
            "tool_steps": c.tool_steps,
            "events": c.lines.len(),
            "bytes": c.bytes(),
        })).collect::<Vec<_>>(),
        "skipped": conv.skipped.iter().map(|(s, w)| json!({ "session_id": s, "reason": w })).collect::<Vec<_>>(),
    }))
}

/// 读一个 Trae 会话并转换成一个待写入的 WorkBuddy 会话。
fn prepare_session(conn: &Connection, client_key: &str, sid: &str) -> Result<Converted, String> {
    let row = conn
        .query_row(
            "SELECT COALESCE(s.session_title,''), s.created_at, s.updated_at, \
                    COALESCE(p.absolute_path,'') \
             FROM chat_session s LEFT JOIN project p ON s.project_id = p.project_id \
             WHERE s.session_id = ?",
            [sid],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, SqlValue>(1)?,
                    r.get::<_, SqlValue>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("读取会话失败: {e}"))?
        .ok_or_else(|| format!("会话 {sid} 不在当前数据源中"))?;

    let (title, created, updated, cwd) = row;
    let wb_id = det_session_uuid(client_key, sid);
    let key = if cwd.trim().is_empty() {
        String::new()
    } else {
        workspace_key(&cwd)
    };
    let evt_cwd = if cwd.trim().is_empty() { String::new() } else { norm_cwd(&cwd) };

    let mut msgs: Vec<(String, String, SqlValue)> = conn
        .prepare(
            "SELECT message_id, message_role, created_at FROM chat_message \
             WHERE session_id=? AND ifnull(deleted_at,0)=0 ORDER BY message_index",
        )
        .map_err(|e| format!("会话消息查询失败: {e}"))?
        .query_map([sid], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, SqlValue>(2)?))
        })
        .map_err(|e| format!("会话消息查询失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("会话消息读取失败: {e}"))?;

    let mut b = TurnBuilder::new(&wb_id, &evt_cwd, ts_to_ms(&created));
    for (mid, role, created_at) in msgs.drain(..) {
        let ms = {
            let v = ts_to_ms(&created_at);
            if v > 0 {
                v
            } else {
                ts_to_ms(&created)
            }
        };
        if role == "user" {
            let raw = conn
                .query_row(
                    "SELECT content FROM chat_message_general WHERE message_id=?",
                    [&mid],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default();
            let text = user_text(&raw);
            if text.is_empty() {
                continue;
            }
            b.push_user(&text, ms);
            b.turns += 1;
        } else {
            let raw = conn
                .query_row(
                    "SELECT content FROM chat_message_task WHERE message_id=?",
                    [&mid],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default();
            push_assistant_task(&mut b, conn, &mid, &raw, ms);
        }
    }

    let title = if title.trim().is_empty() {
        format!("Trae 会话 {}", &sid[..sid.len().min(8)])
    } else {
        title.trim().to_string()
    };

    // 末尾补 ai-title（原生会话把它放在文件最后）
    let end_ms = if b.turns == 0 { ts_to_ms(&created) } else { ts_to_ms(&updated).max(ts_to_ms(&created)) };
    b.events.push(json!({
        "id": event_id(end_ms),
        "timestamp": end_ms,
        "type": "ai-title",
        "aiTitle": title,
        "sessionId": wb_id,
        "cwd": evt_cwd,
    }));

    Ok(Converted {
        trae_id: sid.to_string(),
        wb_id,
        title,
        cwd,
        workspace_key: key,
        created_ms: ts_to_ms(&created),
        updated_ms: ts_to_ms(&updated).max(ts_to_ms(&created)),
        turns: b.turns,
        answered: count_answered(&b.events),
        tool_steps: b.tool_steps,
        lines: b.events,
    })
}

// ---------------------------------------------------------------------------
// 写入 workbuddy.db
// ---------------------------------------------------------------------------

/// 模板列：只借「与本会话内容无关」的静态字段。
///
/// 刻意**不**包含 `addon_selection` / `buddy_binding_json` / `group_id` / `group_title` /
/// `expert_*` / `session_settings` / `project_id` / `buddy_snapshot_id` —— 它们描述的是
/// 模板那一条会话的上下文（场景、工作区、专家、分组），照抄会让导入的会话冒充别人的上下文。
const TEMPLATE_COLS: [&str; 8] = [
    "model",
    "mode",
    "permission_mode",
    "source_mode",
    "use_sandbox_cli",
    "context_window",
    "thought_level",
    "transport",
];

/// 读一条真实行作为静态字段模板（没有历史会话时用硬编码默认值）。
fn session_template(conn: &Connection) -> Vec<(String, SqlValue)> {
    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT {} FROM sessions WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT 1",
        TEMPLATE_COLS.join(", ")
    )) {
        if let Ok(mut rows) = stmt.query([]) {
            if let Ok(Some(r)) = rows.next() {
                return TEMPLATE_COLS
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.to_string(), r.get::<_, SqlValue>(i).unwrap_or(SqlValue::Null)))
                    .collect();
            }
        }
    }
    Vec::new()
}

fn insert_sessions(conn: &mut Connection, items: &[Converted], uid: &str) -> Result<(), String> {
    let template = session_template(conn);
    let tx = conn.transaction().map_err(|e| format!("开启事务失败: {e}"))?;

    for p in items {
        let mut cols: Vec<String> = vec![
            "id".into(),
            "cwd".into(),
            "user_id".into(),
            "title".into(),
            "status".into(),
            "created_at".into(),
            "updated_at".into(),
            "last_activity_at".into(),
            "is_playground".into(),
            "unread".into(),
        ];
        let mut vals: Vec<SqlValue> = vec![
            SqlValue::Text(p.wb_id.clone()),
            SqlValue::Text(p.cwd.clone()),
            SqlValue::Text(uid.to_string()),
            SqlValue::Text(p.title.clone()),
            SqlValue::Text("completed".into()),
            SqlValue::Integer(p.created_ms),
            SqlValue::Integer(p.updated_ms),
            SqlValue::Integer(p.updated_ms),
            SqlValue::Integer(0),
            SqlValue::Integer(0),
        ];
        for (name, v) in &template {
            // status/transport 已显式给值，模板里若含同名列则跳过
            if cols.iter().any(|c| c == name) {
                continue;
            }
            if matches!(v, SqlValue::Null) {
                continue; // 空值不写，交给列默认值
            }
            cols.push(name.clone());
            vals.push(v.clone());
        }
        let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "INSERT OR REPLACE INTO sessions ({}) VALUES ({})",
            cols.join(", "),
            placeholders.join(", ")
        );
        tx.execute(&sql, rusqlite::params_from_iter(vals.iter()))
            .map_err(|e| format!("写入会话「{}」失败: {e}", p.title))?;

        // 工作区补登记（已存在则原样不动，避免打乱用户「最近打开」的顺序）
        if !p.cwd.trim().is_empty() {
            let _ = tx.execute(
                "INSERT OR IGNORE INTO workspaces (path, last_opened_at) VALUES (?1, ?2)",
                params![p.cwd, p.updated_ms],
            );
        }
    }

    tx.commit().map_err(|e| format!("提交事务失败: {e}"))
}

/// 计数（用于自检）。
fn count_sessions(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 编排：Trae → WorkBuddy
// ---------------------------------------------------------------------------

/// 执行 Trae → WorkBuddy 导出。
///
/// 顺序：**读源（Trae）→ 退出 WorkBuddy → 备份 → 写 JSONL → 写数据库 → 重新拉起**。
/// 任何一步失败都会把数据库还原到备份、并清掉本轮新建的 JSONL，不留半截状态。
pub fn export_sessions(
    client_key: &str,
    session_ids: &[String],
    target_uid: &str,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(cb) = on_log {
            cb(m);
        }
    };
    if EXPORTING.swap(true, Ordering::SeqCst) {
        return Err("已有导出任务正在进行中，请等它结束后再试（并发导出会互相覆盖数据库）".into());
    }
    let _guard = ExportGuard;

    if session_ids.is_empty() {
        return Err("没有选择要导出的 Trae 会话".into());
    }
    if !workbuddy_source::is_available() {
        return Err(format!(
            "本机未检测到 WorkBuddy 数据（缺 projects/ 或 workbuddy.db）：{}",
            data_root().display()
        ));
    }
    if target_uid.trim().is_empty() {
        return Err("未指定目标账号，请先在页面上选择".into());
    }

    // 1) 读源：先把 Trae 侧全部内容解析好，避免中途出问题却已经关掉了 WorkBuddy
    log("读取 Trae 会话…");
    let conv = convert(client_key, session_ids)?;
    for (sid, why) in &conv.skipped {
        log(&format!("会话 {sid} 跳过：{why}"));
    }
    let items = conv.items;
    for p in &items {
        log(&format!(
            "  「{}」→ {} 回合 / {} 个工具步骤 / 工作区 {}",
            p.title, p.turns, p.tool_steps, p.workspace_key
        ));
    }
    if items.is_empty() {
        return Err("所选会话都没有可导出的对话内容".into());
    }
    let total_turns: usize = items.iter().map(|i| i.turns).sum();
    let total_steps: usize = items.iter().map(|i| i.tool_steps).sum();
    log(&format!("已解析 {} 个会话 / {} 回合 / {} 个工具步骤", items.len(), total_turns, total_steps));

    // 2) 退出 WorkBuddy
    let was_running = wb_is_running();
    if was_running {
        log("WorkBuddy 正在运行，先将其退出…");
        let killed = wb_kill_all();
        log(&format!("已结束 {} 个进程，等待退出…", killed.len()));
        if !wb_wait_stopped(10_000) {
            return Err("WorkBuddy 未能退出，无法写入会话库".into());
        }
    }

    // 3) 备份数据库三件套
    let backup_dir = store_dir()
        .join("workbuddy_export_backup")
        .join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let db = db_path();
    let wal = db.with_extension("db-wal");
    let shm = db.with_extension("db-shm");
    for (label, p) in [("主库", &db), ("WAL", &wal), ("SHM", &shm)] {
        if p.exists() {
            let dst = backup_dir.join(p.file_name().unwrap_or_default());
            std::fs::copy(p, &dst).map_err(|e| format!("备份{label}失败: {e}"))?;
        }
    }
    log(&format!("已备份 WorkBuddy 会话库到 {}", backup_dir.display()));

    /// 出错时把数据库还原到备份（尽力而为，失败仅记录）。
    fn restore(backup_dir: &Path, db: &Path, log: &dyn Fn(&str)) {
        for name in ["workbuddy.db", "workbuddy.db-wal", "workbuddy.db-shm"] {
            let src = backup_dir.join(name);
            let dst = db.with_file_name(name);
            if src.exists() {
                if let Err(e) = std::fs::copy(&src, &dst) {
                    log(&format!("  还原 {name} 失败：{e}"));
                }
            } else if dst.exists() && name != "workbuddy.db" {
                // 备份里没有（备份时不存在），把新产生的残留清掉
                let _ = std::fs::remove_file(&dst);
            }
        }
    }

    // 4) 写正文（先正文后数据库：多余的 JSONL 在客户端不可见，属无害残骸）
    let mut created: Vec<PathBuf> = Vec::new();
    let mut write_body = || -> Result<(), String> {
        for p in &items {
            let key = if p.workspace_key.is_empty() {
                "unknown-workspace".to_string()
            } else {
                p.workspace_key.clone()
            };
            let dir = projects_dir().join(&key);
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
            let path = dir.join(format!("{}.jsonl", p.wb_id));
            let mut body = String::new();
            for ev in &p.lines {
                body.push_str(&serde_json::to_string(ev).map_err(|e| format!("序列化事件失败: {e}"))?);
                body.push('\n');
            }
            let mut f = std::fs::File::create(&path).map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
            f.write_all(body.as_bytes()).map_err(|e| format!("写 {} 失败: {e}", path.display()))?;
            created.push(path);
        }
        Ok(())
    };
    if let Err(e) = write_body() {
        for p in &created {
            let _ = std::fs::remove_file(p);
        }
        return Err(e);
    }
    log(&format!("已写入 {} 个正文文件", created.len()));

    // 5) 写数据库
    let mut conn = match Connection::open(&db) {
        Ok(c) => c,
        Err(e) => {
            for p in &created {
                let _ = std::fs::remove_file(p);
            }
            return Err(format!("打开 WorkBuddy 会话库失败: {e}"));
        }
    };
    if let Err(e) = insert_sessions(&mut conn, &items, target_uid) {
        drop(conn);
        restore(&backup_dir, &db, &log);
        for p in &created {
            let _ = std::fs::remove_file(p);
        }
        return Err(e);
    }
    // 把 WAL 折叠进主库，退出时留下干净状态（此时客户端已关闭，可以安全 checkpoint）
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    let verified = count_sessions(&conn);
    drop(conn);
    log(&format!("已写入 {} 条会话记录（库内现有 {} 条）", items.len(), verified));

    // 6) 只保留最新 2 份备份
    let (pruned, freed) = trae_export::prune_backups(
        &store_dir().join("workbuddy_export_backup"),
        2,
    );
    if pruned > 0 {
        log(&format!("已清理 {pruned} 份旧备份，回收 {:.1} MB", freed as f64 / 1_048_576.0));
    }

    // 7) 重新拉起 WorkBuddy，让导入的会话立即可见
    let relaunched = if was_running {
        match wb_launch() {
            Ok(exe) => {
                log(&format!("已重新启动 {}", exe.display()));
                true
            }
            Err(e) => {
                log(&format!("导出成功，但自动启动 WorkBuddy 失败：{e}"));
                false
            }
        }
    } else {
        log("WorkBuddy 原本未运行，未自动启动（打开后即可看到导入的会话）");
        false
    };

    // 涉及的 WorkBuddy 工作区（去重后回到前端展示）
    let mut workspace_keys: Vec<String> =
        items.iter().map(|i| i.workspace_key.clone()).collect();
    workspace_keys.sort();
    workspace_keys.dedup();

    Ok(json!({
        "written_sessions": items.len(),
        "turns": total_turns,
        "tool_steps": total_steps,
        "target_uid": target_uid,
        "target_label": workbuddy_accounts::label_for(target_uid),
        "workspaces": workspace_keys,
        "sessions": items.iter().map(|i| json!({
            "trae_id": i.trae_id,
            "workbuddy_id": i.wb_id,
            "title": i.title,
            "cwd": i.cwd,
            "workspace_key": i.workspace_key,
            "turns": i.turns,
            "tool_steps": i.tool_steps,
        })).collect::<Vec<_>>(),
        "verified_sessions": verified,
        "was_running": was_running,
        "relaunched": relaunched,
        "backup_dir": backup_dir.to_string_lossy(),
        "pruned_backups": pruned,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_key_matches_real_dirs() {
        // 与 `~/.workbuddy/projects/` 下实测目录名逐一对照
        assert_eq!(workspace_key("D:\\htw\\test"), "d-htw-test");
        assert_eq!(workspace_key("D:/htw/签到"), "d-htw-签到");
        assert_eq!(workspace_key("D:/htw/签到/trae-switch-cn"), "d-htw-签到-trae-switch-cn");
    }

    #[test]
    fn norm_cwd_lowercases_drive_and_uses_backslash() {
        assert_eq!(norm_cwd("D:/htw/签到"), "d:\\htw\\签到");
        assert_eq!(norm_cwd("d:\\htw\\test"), "d:\\htw\\test");
    }

    #[test]
    fn det_session_uuid_is_stable_and_v4_shaped() {
        let a = det_session_uuid("trae-cn", "6a733b6a087dc2ffec694450");
        let b = det_session_uuid("trae-cn", "6a733b6a087dc2ffec694450");
        assert_eq!(a, b);
        assert_ne!(a, det_session_uuid("solo-cn", "6a733b6a087dc2ffec694450"));
        assert_eq!(a.as_bytes()[14] as char, '4'); // 版本位
        assert_eq!(uuid::Uuid::parse_str(&a).map(|u| u.get_version_num()), Ok(4));
    }

    #[test]
    fn event_id_is_v7_shaped() {
        let id = event_id(1_791_110_329_949);
        assert_eq!(uuid::Uuid::parse_str(&id).map(|u| u.get_version_num()), Ok(7));
        assert!(id.starts_with("01a1067e"), "48 位毫秒前缀应保留：{id}");
    }

    #[test]
    fn ts_to_ms_normalizes_seconds_and_millis() {
        assert_eq!(ts_to_ms(&SqlValue::Integer(1_785_936_746)), 1_785_936_746_000);
        assert_eq!(ts_to_ms(&SqlValue::Integer(1_791_110_329_949)), 1_791_110_329_949);
        assert_eq!(ts_to_ms(&SqlValue::Text("x".into())), 0);
    }

    /// 构造的事件必须能被 WorkBuddy 自己的解析器原样读回来——这是整个反向导出
    /// 最核心的契约（harness 不依赖任何真实库，可随时回归）。
    #[test]
    fn turn_builder_round_trips_through_workbuddy_parser() {
        const SID: &str = "11111111-1111-4111-8111-111111111111";
        let mut b = TurnBuilder::new(SID, "d:\\htw\\test", 1_791_110_000_000);
        b.push_user("帮我看看这个文件", 1_791_110_000_100);
        b.push_reasoning("先读文件", 1_791_110_000_200);
        b.push_assistant("我先读一下。", 1_791_110_000_300);
        b.push_tool("Read", "call_1", "{\"file_path\":\"a.md\"}", 1_791_110_000_400);
        b.push_tool_result("Read", "call_1", "文件内容", "completed", 1_791_110_000_401);
        b.push_assistant("这是最终回答。", 1_791_110_000_500);
        b.turns = 1;

        let text: String = b
            .events
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
            .collect();
        let parsed = workbuddy_source::parse_body(SID, &text);

        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].user_text, "帮我看看这个文件");
        // WorkBuddy 只认「下一条用户消息之前最后一条 assistant」为最终回答，
        // 所以中间那句叙述不能覆盖最终回答。
        assert_eq!(parsed.turns[0].assistant_text, "这是最终回答。");
        let kinds: Vec<&str> = parsed.turns[0].events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["reasoning", "assistant_text", "function_call", "function_call_result", "assistant_text"]
        );
        assert_eq!(count_answered(&b.events), 1);
    }

    #[test]
    fn count_answered_ignores_turns_without_assistant_output() {
        let mut b = TurnBuilder::new("s", "c", 0);
        b.push_user("第一问", 1);
        b.push_assistant("第一答", 2);
        b.push_user("第二问（源会话里被中断，没有回答）", 3);
        assert_eq!(count_answered(&b.events), 1);
    }

    /// `sessions` / `workspaces` 的真实 DDL（从本机 `workbuddy.db` 导出，逐字复制）。
    /// 用真表结构而不是简化表，才能真正挡住「列名写错 / 违反 NOT NULL」这类错误。
    const REAL_DDL: &str = "
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY, cwd TEXT NOT NULL, user_id TEXT NOT NULL,
            title TEXT, custom_title TEXT, status TEXT NOT NULL DEFAULT 'Pending',
            created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            last_activity_at INTEGER, deleted_at INTEGER,
            is_playground INTEGER NOT NULL DEFAULT 0, source_mode TEXT,
            is_background_automation INTEGER, mode TEXT, model TEXT,
            expert_id TEXT, expert_locale TEXT, expert_runtime_identity TEXT,
            expert_marketplace TEXT, permission_mode TEXT, use_sandbox_cli INTEGER,
            project_id TEXT, plugin_context_json TEXT, addon_selection TEXT,
            session_settings TEXT, last_user_prompt_expert_selection TEXT,
            context_window INTEGER, buddy_snapshot_id TEXT, buddy_binding_json TEXT,
            thought_level TEXT, transport TEXT NOT NULL DEFAULT 'local',
            conversation_origin TEXT, visibility TEXT, group_id TEXT, group_title TEXT,
            agent_dirty INTEGER, agent_dirty_at INTEGER, agent_last_synced INTEGER,
            verified_at INTEGER, unread INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE workspaces (path TEXT PRIMARY KEY, last_opened_at INTEGER NOT NULL);
    ";

    fn sample() -> Converted {
        Converted {
            trae_id: "6a733b6a087dc2ffec694450".into(),
            wb_id: "329e85c0-50d4-4ab9-8ec1-c9b03b5153e5".into(),
            title: "交叉口流量分析".into(),
            cwd: "C:/Users/11970/Desktop/111".into(),
            workspace_key: "c-users-11970-desktop-111".into(),
            created_ms: 1_785_936_746_000,
            updated_ms: 1_785_937_160_000,
            turns: 10,
            answered: 10,
            tool_steps: 3,
            lines: Vec::new(),
        }
    }

    #[test]
    fn insert_sessions_writes_against_real_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(REAL_DDL).unwrap();
        // 一条真实历史行作为模板（含上下文类字段，用来验证它们**不会**被照抄）
        conn.execute(
            "INSERT INTO sessions (id, cwd, user_id, title, status, created_at, updated_at, \
             model, mode, permission_mode, use_sandbox_cli, context_window, thought_level, transport, \
             addon_selection, group_id) \
             VALUES ('tpl','d:/x','u1','模板会话','working',1,2,'hy4-preview','craft','fullAccess',1,300000,'high','local', \
             '{\"microSceneIds\":[\"file:///别人的工作区\"]}','grp-1')",
            [],
        )
        .unwrap();

        let items = vec![sample()];
        insert_sessions(&mut conn, &items, "63e05cca-cf7d-4dfa-af52-65168597eac1").unwrap();

        let row = conn
            .query_row(
                "SELECT cwd, user_id, title, status, created_at, updated_at, model, mode, \
                 transport, addon_selection, group_id, project_id, is_playground, unread \
                 FROM sessions WHERE id='329e85c0-50d4-4ab9-8ec1-c9b03b5153e5'",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<String>>(11)?,
                        r.get::<_, i64>(12)?,
                        r.get::<_, i64>(13)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, "C:/Users/11970/Desktop/111");
        assert_eq!(row.1, "63e05cca-cf7d-4dfa-af52-65168597eac1");
        assert_eq!(row.2, "交叉口流量分析");
        // status 显式写 completed，不被模板的 working 覆盖
        assert_eq!(row.3, "completed");
        assert_eq!((row.4, row.5), (1_785_936_746_000, 1_785_937_160_000));
        // 静态字段借自模板
        assert_eq!(row.6.as_deref(), Some("hy4-preview"));
        assert_eq!(row.7.as_deref(), Some("craft"));
        assert_eq!(row.8, "local");
        // 上下文类字段绝不照抄：模板里明明有值，导入的会话必须为空
        assert_eq!(row.9, None, "addon_selection 不能照抄模板（会冒充别人的场景）");
        assert_eq!(row.10, None, "group_id 不能照抄模板");
        assert_eq!(row.11, None, "project_id 不能照抄模板");
        assert_eq!((row.12, row.13), (0, 0));

        // 工作区补登记
        let ws: i64 = conn
            .query_row(
                "SELECT count(*) FROM workspaces WHERE path='C:/Users/11970/Desktop/111'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ws, 1);

        // 重复导出必须幂等：同一 id 再写一次仍然只有 1 行
        insert_sessions(&mut conn, &items, "63e05cca-cf7d-4dfa-af52-65168597eac1").unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sessions WHERE id='329e85c0-50d4-4ab9-8ec1-c9b03b5153e5'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn tool_result_text_prefers_known_keys() {
        assert_eq!(tool_result_text(&json!({"error_message": "boom", "data": {}})), "boom");
        assert_eq!(tool_result_text(&json!({"data": {"text": "hello"}})), "hello");
        assert_eq!(tool_result_text(&json!({"data": {"a": 1}})), "{\"a\":1}");
        assert_eq!(tool_result_text(&json!({"data": null})), "");
    }
}
