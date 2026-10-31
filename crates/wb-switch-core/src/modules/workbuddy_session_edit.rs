//! T46 · S4 —— WorkBuddy 会话编辑器（后端）。
//!
//! 与 Trae 侧（`trae_session_edit`）是**两套完全不同的数据模型**，所以这是两个模块，
//! 只在「分级校验 + 四种写入方式」的语义上保持同构：
//!
//! | | Trae | WorkBuddy |
//! | --- | --- | --- |
//! | 元数据 | SQLCipher 库里 20+ 张互相引用的表 | `~/.workbuddy/workbuddy.db` 的 `sessions` 一张表 |
//! | 正文 | `chat_message_*.content` 列里的 JSON 载荷 | `projects/<工作区>/<cid>.jsonl`，**一行一个事件** |
//!
//! ## 编辑模型
//!
//! - **正文按「行」编辑**：一行 = 一段（用户提问 / 助手回答 / 思考 / 工具调用 / 工具结果 / 元信息）。
//!   写入 = 把改动后的行重新拼成整份 JSONL，**未改动的行逐字节原样保留**（见 [`plan_lines`]）。
//! - **元数据按列更新**：`sessions` 表全字段走结构化表单。
//!
//! ## 为什么写入提交「操作数组」而不是「整份 doc」
//!
//! 真机实测单个会话正文 **19 MB / 3184 行**，最大的一个 **136 MB**。整份 doc 过 IPC 会把
//! 前端冻死（Trae 侧踩过同一个坑，那边靠 `$jsonRef` 才压到 8.8 MB）。WorkBuddy 的正文是
//! 「一个文件里的若干行」，天然可以**只回传改动过的行**，所以这里更进一步：
//!
//! - 读：`edit_doc` 只回**回合索引**（轻量），`edit_lines` 按回合区间取**完整行**。
//! - 写：`edit_save` 收 `ops`（replace / delete / insert），后端在**内存里对原文应用**
//!   再整份落盘，全程不把整份正文交给前端。
//!
//! ## 三条硬规则（沿用 `workbuddy_sessions` 模块头，一条都不许破）
//!
//! 1. **`edge-sync-mapping*.db` 一个字节都不写** —— 所有写路径过 [`assert_not_edge_sync`]。
//! 2. **新增 = 新 UUID + `INSERT`（不是 `INSERT OR REPLACE`）**，源会话一行不改。
//! 3. **删除走软删**（`UPDATE sessions SET deleted_at`）—— 本模块删的是**行**不是会话，
//!    删空整条会话仍交给 `workbuddy_sessions::delete_sessions`。
//!
//! ## 校验分两层（都要，缺一不可）
//!
//! - **前端每次改动**调 [`edit_validate`]：**不读文件、不开库**，只校验「提交上来的那几行 +
//!   行级不变量 + 剩余用户提问数」，毫秒级。
//! - **写入前**在 [`edit_save`] 里跑一遍**全量校验**（解析整份结果、查 callId 配对 / parentId
//!   断链 / sessionId 一致性），任何 `error` 直接拒绝写入。
//!
//! 之所以分两层：真机 136 MB 正文每次按键都整份解析是不可能的。分层的代价是
//! 「前端看到 0 错、写入却被拒」可能出现 —— 所以**写入前那次全量错误必须原样回给前端**，
//! 不许吞掉。

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::modules::config::{now_ms, store_dir};
use crate::modules::{
    workbuddy_accounts, workbuddy_auth, workbuddy_sessions as wbs, workbuddy_source,
    workbuddy_switch,
};

/// 轻量描述里正文的截断长度（字符）。完整正文走 [`edit_lines`]。
const PREVIEW_CHARS: usize = 160;
/// 单次 `edit_lines` 允许取回的行数上限（防止前端一次要整份正文）。
const MAX_LINES_PER_FETCH: usize = 400;
/// 单次提交的 `ops` 条数上限。
const MAX_OPS: usize = 4096;

/// 已知的行类型。**未知类型不是错误**（WorkBuddy 加新事件类型时不该直接拒写），
/// 只给一条 warning，原样保留即可。
const KNOWN_TYPES: [&str; 8] = [
    "message",
    "reasoning",
    "function_call",
    "function_call_result",
    "ai-title",
    "session-meta",
    "file-history-snapshot",
    "summary",
];

// ---------------------------------------------------------------------------
// 值映射（可逆）
// ---------------------------------------------------------------------------

/// SQLite 值 → JSON。BLOB 用 `{"$blob": base64}` 标记，**必须可逆**：
/// 用户改不动它，但回写时要能原样带回，否则一存就把二进制字段清空。
pub fn sql_to_json(v: &SqlValue) -> Value {
    match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(f) => json!(f),
        SqlValue::Text(t) => Value::String(t.clone()),
        SqlValue::Blob(b) => {
            use base64::Engine;
            json!({ "$blob": base64::engine::general_purpose::STANDARD.encode(b) })
        }
    }
}

/// JSON → SQLite 值。`{"$blob": …}` 还原为 BLOB；其余按 JSON 类型映射。
pub fn json_to_sql(v: &Value) -> Result<SqlValue, String> {
    Ok(match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqlValue::Real(f)
            } else {
                return Err(format!("无法映射的数字: {n}"));
            }
        }
        Value::String(s) => SqlValue::Text(s.clone()),
        Value::Object(o) => {
            if let Some(b) = o.get("$blob").and_then(Value::as_str) {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b)
                    .map_err(|e| format!("BLOB base64 解码失败: {e}"))?;
                SqlValue::Blob(bytes)
            } else {
                // 对象/数组列在 sessions 表里不存在，但真要出现就存 JSON 文本，
                // 比直接报错更不容易把用户卡死。
                SqlValue::Text(serde_json::to_string(v).map_err(|e| e.to_string())?)
            }
        }
        Value::Array(_) => {
            SqlValue::Text(serde_json::to_string(v).map_err(|e| e.to_string())?)
        }
    })
}

/// 是否像 WorkBuddy / 客户端认的 UUID（8-4-4-4-12 hex）。
pub fn is_uuid_like(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let want = [8usize, 4, 4, 4, 12];
    if parts.len() != 5 {
        return false;
    }
    parts
        .iter()
        .zip(want.iter())
        .all(|(p, n)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------
// 行分类
// ---------------------------------------------------------------------------

/// 一行的可读语义。`kind` 决定前端画成什么卡片、`text_path` 决定改哪一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineInfo {
    /// 原始 `type`。
    pub ty: String,
    /// 派生种类：user / auto / summary / assistant / reasoning / tool_call / tool_result / meta / unknown
    pub kind: String,
    /// 中文标签（前端直接显示，避免前端再维护一份映射）。
    pub label: String,
    /// 工具名（工具调用 / 工具结果 / 工具才有）。
    pub name: String,
    /// 结果状态。
    pub status: String,
    /// 时间戳（毫秒，可能为 0）。
    pub timestamp: i64,
    /// 可读正文（可能很大；轻量描述里会被截断）。
    pub text: String,
    /// `text` 在**该行 JSON 里**的路径；`null` = 不能就地编辑（只能改原始 JSON）。
    pub text_path: Value,
    /// 用户提问是否包在 `<user_query>…</user_query>` 里（改的时候只换内层，护住 system-reminder）。
    pub wrapped: bool,
}

fn empty_info(ty: &str) -> LineInfo {
    LineInfo {
        ty: ty.to_string(),
        kind: "unknown".to_string(),
        label: "未知事件".to_string(),
        name: String::new(),
        status: String::new(),
        timestamp: 0,
        text: String::new(),
        text_path: Value::Null,
        wrapped: false,
    }
}

/// 取 `content[]` 里第一个含 `text` 的块的路径（返回 `(下标, 文本)`）。
fn first_text_block(content: &Value) -> Option<(usize, String)> {
    content.as_array()?.iter().enumerate().find_map(|(i, b)| {
        let t = b.get("text").and_then(Value::as_str)?;
        if t.trim().is_empty() {
            None
        } else {
            Some((i, t.to_string()))
        }
    })
}

/// 从用户消息的原始块文本里取出真实提问，并报告是否被 `<user_query>` 包住。
///
/// 复用 `workbuddy_source::extract_user_text`（**同一套规则**，不另写一份），
/// 这里额外回传「有没有壳」——前端改完要用同一个壳把它包回去。
fn user_question(raw_block: &str) -> (String, bool) {
    let text = workbuddy_source::extract_user_text(raw_block);
    let wrapped = raw_block.contains("<user_query>") && raw_block.contains("</user_query>");
    (text, wrapped)
}

/// 把一行原始 JSONL 文本分类成 [`LineInfo`]。`Err` = 该行不是合法 JSON。
pub fn classify_line(raw: &str) -> Result<LineInfo, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| format!("第 ? 行不是合法 JSON：{e}"))?;
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    let mut info = empty_info(&ty);
    info.timestamp = v.get("timestamp").and_then(Value::as_i64).unwrap_or(0);

    match ty.as_str() {
        "message" => {
            let role = v.get("role").and_then(Value::as_str).unwrap_or("");
            let content = v.get("content").cloned().unwrap_or(Value::Null);
            match role {
                "user" => {
                    // 按 `parse_body` 的同一口径取正文（多块拼接），别只看 `content[0]`。
                    let joined = workbuddy_source::join_content_blocks(&content);
                    let raw_block = if joined.trim().is_empty() {
                        first_text_block(&content).map(|(_, t)| t).unwrap_or_default()
                    } else {
                        joined
                    };
                    let (q, wrapped) = user_question(&raw_block);
                    info.name = String::new();
                    info.text = q;
                    info.wrapped = wrapped;
                    // 正文可改 = `content[0]` 上真的挂着 `text`（写回时也只改这一格）。
                    // ⚠️ 没有 `<user_query>` 壳的行**同样可改**：它是导入出来的老格式，
                    //    不给编辑路径就只能去改原始 JSON 了。
                    let editable = content
                        .as_array()
                        .and_then(|a| a.first())
                        .map(|b| b.get("text").is_some())
                        .unwrap_or(false);
                    info.text_path = if editable {
                        json!(["content", 0, "text"])
                    } else {
                        Value::Null
                    };
                    // ⚠️⚠️ 真伪判定**只走** `workbuddy_source::classify_user_message`（与 `parse_body`
                    //    一个出口）。早先这里按「有没有 `<user_query>` 壳」二分：有壳算提问、没壳一律
                    //    算自动指令 —— 原生会话恰好成立（实测 4 条会话的没壳行 100% 落在 auto/summary），
                    //    但 **Trae → WorkBuddy 导出写出的正文没有壳**，于是这类会话被算成 0 个回合、
                    //    编辑器里一个回合都列不出来（用户报的「trae 导入的会话回合无法识别」）。
                    match workbuddy_source::classify_user_message(&raw_block) {
                        workbuddy_source::UserMsgKind::SilentSummary => {
                            info.kind = "summary".into();
                            info.label = "内部摘要".into();
                        }
                        workbuddy_source::UserMsgKind::AutoTrigger => {
                            info.kind = "auto".into();
                            info.label = "自动指令".into();
                        }
                        workbuddy_source::UserMsgKind::Real => {
                            info.kind = "user".into();
                            info.label = "用户提问".into();
                        }
                    }
                }
                "assistant" => {
                    info.kind = "assistant".into();
                    info.label = "助手回答".into();
                    info.status = v
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if let Some((i, t)) = first_text_block(&content) {
                        info.text = t;
                        info.text_path = json!(["content", i, "text"]);
                    } else {
                        info.text = workbuddy_source::join_content_blocks(&content);
                    }
                }
                _ => {
                    info.kind = "message".into();
                    info.label = format!("消息（{role}）");
                    info.text = workbuddy_source::join_content_blocks(&content);
                }
            }
        }
        "reasoning" => {
            info.kind = "reasoning".into();
            info.label = "思考".into();
            let raw_content = v.get("rawContent").cloned().unwrap_or(Value::Null);
            if let Some((i, t)) = first_text_block(&raw_content) {
                info.text = t;
                info.text_path = json!(["rawContent", i, "text"]);
            }
        }
        "function_call" => {
            info.kind = "tool_call".into();
            info.label = "工具调用".into();
            info.name = v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            info.text = v
                .get("arguments")
                .map(|a| {
                    a.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| a.to_string())
                })
                .unwrap_or_default();
            if v.get("arguments").map(|a| a.is_string()).unwrap_or(false) {
                info.text_path = json!(["arguments"]);
            }
        }
        "function_call_result" => {
            info.kind = "tool_result".into();
            info.label = "工具结果".into();
            info.name = v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            info.status = v
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            match v.get("output") {
                Some(Value::String(s)) => info.text = s.clone(),
                Some(o) => {
                    if let Some(t) = o.get("text").and_then(Value::as_str) {
                        info.text = t.to_string();
                        info.text_path = json!(["output", "text"]);
                    } else {
                        info.text = o.to_string();
                    }
                }
                None => {}
            }
        }
        "ai-title" => {
            info.kind = "meta".into();
            info.label = "自动标题".into();
            info.text = v
                .get("aiTitle")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            info.text_path = json!(["aiTitle"]);
        }
        "session-meta" | "file-history-snapshot" => {
            info.kind = "meta".into();
            info.label = if ty == "session-meta" {
                "会话元信息".into()
            } else {
                "文件快照".into()
            };
        }
        _ => {
            info.kind = "unknown".into();
            info.label = if ty.is_empty() {
                "无 type 事件".into()
            } else {
                format!("{ty}")
            };
        }
    }
    Ok(info)
}

/// 按字符（不是字节）安全截断。
fn truncate_chars(s: &str, n: usize) -> (String, bool) {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i >= n {
            return (out, true);
        }
        out.push(c);
    }
    (out, false)
}

/// 轻量行描述（`edit_doc` / 回合索引里用）。
fn describe_line(index: usize, raw: &str) -> Value {
    let bytes = raw.as_bytes().len();
    match classify_line(raw) {
        Ok(info) => {
            let (preview, cut) = truncate_chars(&info.text, PREVIEW_CHARS);
            json!({
                "index": index,
                "type": info.ty,
                "kind": info.kind,
                "label": info.label,
                "name": info.name,
                "status": info.status,
                "timestamp": info.timestamp,
                "bytes": bytes,
                "textBytes": info.text.as_bytes().len(),
                "text": preview,
                "textTruncated": cut,
                "textPath": info.text_path,
                "wrapped": info.wrapped,
                "parseError": Value::Null,
            })
        }
        Err(e) => json!({
            "index": index,
            "type": "",
            "kind": "bad",
            "label": "无法解析的行",
            "name": "",
            "status": "",
            "timestamp": 0,
            "bytes": bytes,
            "textBytes": 0,
            "text": truncate_chars(raw, PREVIEW_CHARS).0,
            "textTruncated": raw.chars().count() > PREVIEW_CHARS,
            "textPath": Value::Null,
            "wrapped": false,
            "parseError": e,
        }),
    }
}

/// 完整行描述（`edit_lines` 用）：带 `json`，未改动的行回原始文本 `raw`。
fn detail_line(index: usize, raw: &str) -> Value {
    let mut d = describe_line(index, raw);
    d["raw"] = json!(raw);
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => {
            d["json"] = v;
            let info = classify_line(raw).unwrap_or_else(|_| empty_info(""));
            d["text"] = json!(info.text);
            d["textTruncated"] = json!(false);
        }
        Err(_) => {
            d["json"] = Value::Null;
        }
    }
    d
}

// ---------------------------------------------------------------------------
// 读取
// ---------------------------------------------------------------------------

fn open_ro(path: &Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    Some(conn)
}

/// `sessions` 表的列元数据（`PRAGMA table_info`）。
fn columns_meta(conn: &Connection) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(sessions)")
        .map_err(|e| format!("读取 sessions 列定义失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?, // notnull
                r.get::<_, i64>(5)?, // pk
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| format!("读取 sessions 列定义失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("解析 sessions 列定义失败: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(name, ty, notnull, pk, dflt)| {
            json!({
                "name": name,
                "type": ty,
                "notNull": notnull != 0,
                // ⚠️ `id` 是主键：覆写不许改它（改 id 等于换一条会话，要走「新增」）。
                "pk": pk != 0,
                "dflt": dflt,
                // sessions 表没有「TEXT 里存 JSON」的列（正文在文件里，不在库里）。
                "json": false,
                "readOnly": pk != 0,
            })
        })
        .collect())
}

/// 读正文文件为「逐行原文」。**保留每一行的原始字节**（不 trim、不重排），
/// 这样未改动的行回写时逐字节一致。
fn read_body_lines(cid: &str) -> Result<(PathBuf, Vec<String>), String> {
    let path = workbuddy_source::find_jsonl(cid)
        .ok_or_else(|| format!("会话 {cid} 的正文文件不存在（projects/ 下未找到 {cid}.jsonl）"))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取正文 {} 失败: {e}", path.display()))?;
    Ok((path, split_lines(&text)))
}

/// 按 `\n` 切行，保留行内内容；丢掉**末尾**的空行（文件结尾的换行不该算作一行）。
pub fn split_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    while out.last().map(|s| s.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// 把行数组拼回文本（每行一个 `\n`，末尾也带一个 —— 与客户端写出的形态一致）。
pub fn join_lines(lines: &[String]) -> String {
    let mut s = String::new();
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// 会话正文 → 回合索引（**轻量**，每行只带截断预览）。
///
/// 回合的切分与 `workbuddy_source::parse_body` 同一口径：**真实用户提问**开新回合；
/// `auto`（自动续写 / 后台通知）与 `summary`（内部摘要）都不开新回合。
/// 第一条用户提问之前的行进 `head`（`session-meta` / `ai-title` 常落在这里）。
pub fn build_turn_index(descs: &[Value]) -> (Vec<Value>, Vec<usize>) {
    let mut head: Vec<usize> = Vec::new();
    let mut turns: Vec<Value> = Vec::new();
    let mut cur: Option<(usize, usize, String, Vec<String>)> = None; // (start, end, userText, tools)

    for (i, d) in descs.iter().enumerate() {
        let kind = d["kind"].as_str().unwrap_or("");
        if kind == "user" {
            if let Some((s, e, t, tools)) = cur.take() {
                turns.push(turn_json(turns.len(), s, e, &t, &tools, descs));
            }
            let text = d["text"].as_str().unwrap_or("").to_string();
            cur = Some((i, i + 1, text, Vec::new()));
            continue;
        }
        match cur.as_mut() {
            None => head.push(i),
            Some((_, end, _, tools)) => {
                *end = i + 1;
                if kind == "tool_call" {
                    let n = d["name"].as_str().unwrap_or("");
                    if !n.is_empty() {
                        tools.push(n.to_string());
                    }
                }
            }
        }
    }
    if let Some((s, e, t, tools)) = cur.take() {
        turns.push(turn_json(turns.len(), s, e, &t, &tools, descs));
    }
    (turns, head)
}

fn turn_json(
    index: usize,
    start: usize,
    end: usize,
    user_text: &str,
    tools: &[String],
    descs: &[Value],
) -> Value {
    let (ut, _) = truncate_chars(user_text, PREVIEW_CHARS);
    // 「最终回答」= 该回合里**最后一条** assistant（与 parse_body 一致）
    let reply = (start..end)
        .rev()
        .filter(|i| descs[*i]["kind"].as_str() == Some("assistant"))
        .next()
        .map(|i| descs[i]["text"].as_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let (rt, _) = truncate_chars(&reply, PREVIEW_CHARS);
    let time = (start..end)
        .map(|i| descs[i]["timestamp"].as_i64().unwrap_or(0))
        .max()
        .unwrap_or(0);
    let status = (start..end)
        .map(|i| descs[i]["status"].as_str().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
        .next_back()
        .unwrap_or_default();
    json!({
        "index": index,
        "start": start,
        "end": end,
        "lines": end - start,
        "userText": ut,
        "replyText": rt,
        "tools": tools,
        "time": time,
        "status": status,
    })
}

/// 读出一份 EditDoc（**只读**）。
///
/// 体积控制：正文只回**回合索引 + 行预览**，完整行按回合走 [`edit_lines`]。
pub fn edit_doc(uid: &str, cid: &str) -> Result<Value, String> {
    if uid.trim().is_empty() {
        return Err("账号缺少 uid".to_string());
    }
    let snap = workbuddy_source::snapshot_db()?;
    let conn = open_ro(&snap).ok_or_else(|| "会话快照无法打开".to_string())?;
    let columns = columns_meta(&conn)?;
    let src = wbs::read_source_row(&conn, cid)?
        .ok_or_else(|| format!("会话库里找不到会话 {cid}"))?;
    let owner = src
        .iter()
        .find(|(c, _)| c == "user_id")
        .and_then(|(_, v)| match v {
            SqlValue::Text(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();
    if owner != uid {
        return Err(format!(
            "会话 {cid} 的归属是 {}，与请求的账号不一致",
            workbuddy_accounts::label_for(&owner)
        ));
    }
    let deleted = src
        .iter()
        .find(|(c, _)| c == "deleted_at")
        .map(|(_, v)| !matches!(v, SqlValue::Null))
        .unwrap_or(false);
    let mut warnings: Vec<String> = Vec::new();
    if deleted {
        warnings.push("这条会话已被软删（deleted_at 非空），客户端列表里看不到它".to_string());
    }
    let row: serde_json::Map<String, Value> = src
        .iter()
        .map(|(c, v)| (c.clone(), sql_to_json(v)))
        .collect();
    let title = row
        .get("custom_title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| row.get("title").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    let (path, lines) = read_body_lines(cid)?;
    let descs: Vec<Value> = lines
        .iter()
        .enumerate()
        .map(|(i, raw)| describe_line(i, raw))
        .collect();
    let (turns, head) = build_turn_index(&descs);
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &descs {
        *counts.entry(kind_key(d)).or_insert(0) += 1;
    }
    if counts.get("user").copied().unwrap_or(0) == 0 {
        warnings.push(
            "这条会话没有一条用户提问（真用户提问只会是 user 角色且非自动指令）—— 客户端打开可能显示空白，请谨慎覆写"
                .to_string(),
        );
    } else {
        // ⚠️ 「有提问」不等于「格式原生」：Trae → WorkBuddy 导出写出的用户行没有
        //    `<user_query>` 壳（v0.2.21 及更早）。本工具现在照样认得出它是提问，
        //    但 WorkBuddy 客户端不一定照原生那样渲染 —— 值得提示一句，并给出自愈办法。
        let unwrapped = descs
            .iter()
            .filter(|d| d["kind"].as_str() == Some("user") && d["wrapped"].as_bool() != Some(true))
            .count();
        if unwrapped > 0 {
            warnings.push(format!(
                "有 {unwrapped} 条用户提问没有 <user_query> 壳（多是早期从 Trae 导入写出的格式）—— 编辑器能正常识别与改写，但客户端可能不照原生样式渲染；重新导入一次即可修好"
            ));
        }
    }

    let workspace = path
        .parent()
        .and_then(|p| p.strip_prefix(workbuddy_auth::projects_dir()).ok())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let body_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    Ok(json!({
        "clientKey": "workbuddy",
        "sessionId": cid,
        "ownerUid": owner,
        "ownerLabel": workbuddy_accounts::label_for(&owner),
        "title": title,
        "row": row,
        "columns": columns,
        "body": {
            "path": path.to_string_lossy(),
            "workspace": workspace,
            "exists": true,
            "bytes": body_bytes,
            "lineCount": lines.len(),
        },
        "turns": turns,
        "head": head,
        "counts": counts,
        // 行号 → 种类。前端把它回传给 `validate_submit`，后端才能在不读文件的前提下
        // 算出「删掉的行里有几条用户提问」（空会话硬拦）。每项只有十几字节。
        "lineKinds": descs
            .iter()
            .map(|d| d["kind"].as_str().unwrap_or("unknown"))
            .collect::<Vec<_>>(),
        "warnings": warnings,
    }))
}

fn kind_key(d: &Value) -> &str {
    // counts 的键用静态字符串，避免把整份 value 借出去。
    match d["kind"].as_str().unwrap_or("unknown") {
        "user" => "user",
        "auto" => "auto",
        "summary" => "summary",
        "assistant" => "assistant",
        "reasoning" => "reasoning",
        "tool_call" => "toolCall",
        "tool_result" => "toolResult",
        "meta" => "meta",
        "bad" => "bad",
        "message" => "message",
        _ => "unknown",
    }
}

/// 按**行区间**取完整行（`[from, to)`）。一次最多 [`MAX_LINES_PER_FETCH`] 行。
pub fn edit_lines(uid: &str, cid: &str, from: usize, to: usize) -> Result<Value, String> {
    if to <= from {
        return Err("行区间为空".to_string());
    }
    if to - from > MAX_LINES_PER_FETCH {
        return Err(format!(
            "一次最多取 {MAX_LINES_PER_FETCH} 行，本次要了 {} 行",
            to - from
        ));
    }
    // 归属校验（只读一行，不解析正文）
    let snap = workbuddy_source::snapshot_db()?;
    let conn = open_ro(&snap).ok_or_else(|| "会话快照无法打开".to_string())?;
    let owner = conn
        .query_row(
            "SELECT user_id FROM sessions WHERE id = ?1",
            [cid],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .unwrap_or_default();
    if owner != uid {
        return Err("该会话不属于这个账号".to_string());
    }
    drop(conn);

    let (_, lines) = read_body_lines(cid)?;
    if to > lines.len() {
        return Err(format!("区间越界：共 {} 行，请求 [{} , {})", lines.len(), from, to));
    }
    let items: Vec<Value> = (from..to)
        .map(|i| detail_line(i, &lines[i]))
        .collect();
    Ok(json!({ "ok": true, "sessionId": cid, "from": from, "to": to, "lines": items }))
}

// ---------------------------------------------------------------------------
// 操作数组 → 最终行列表（纯函数，被 validate 与 save 共用）
// ---------------------------------------------------------------------------

/// 应用 `ops` 后的一行。
#[derive(Debug, Clone)]
pub enum OutLine {
    /// 原文照抄（逐字节不变）。
    Orig(usize),
    /// 改动过 / 新增的行（已是序列化后的文本）。
    New(String),
}

/// 把前端提交的 `ops` 应用到原始行序上。
///
/// `ops` 形态：
/// ```jsonc
/// [
///   { "op": "replace", "index": 5, "json": { … } },   // 整行替换（改动过的那几行）
///   { "op": "delete",  "indexes": [7, 8] },           // 删行（删一段 = 删它所在的行）
///   { "op": "insert",  "at": 9, "json": { … } },      // 在第 9 行**之前**插入（at == 行数 = 追加）
///   { "op": "append",  "json": { … } }
/// ]
/// ```
///
/// ⚠️ **未改动的行必须是 `Orig`**：只有这样才能保证「没碰的内容逐字节不变」，
/// 也才不会让整份文件被重新格式化一遍。
pub fn apply_ops(lines: &[String], ops: &Value) -> Result<Vec<OutLine>, String> {
    let Some(arr) = ops.as_array() else {
        return Err("ops 必须是数组".to_string());
    };
    if arr.len() > MAX_OPS {
        return Err(format!("改动项过多（{} > {MAX_OPS}）", arr.len()));
    }
    let n = lines.len();
    let mut replaced: BTreeMap<usize, String> = BTreeMap::new();
    let mut deleted: BTreeSet<usize> = BTreeSet::new();
    let mut inserts: BTreeMap<usize, Vec<String>> = BTreeMap::new();

    for op in arr {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        match kind {
            "replace" => {
                let idx = op
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "replace 缺少 index".to_string())? as usize;
                if idx >= n {
                    return Err(format!("replace 的行号 {idx} 越界（共 {n} 行）"));
                }
                if deleted.contains(&idx) || replaced.contains_key(&idx) {
                    return Err(format!("第 {idx} 行被重复修改，拒绝写入"));
                }
                let json = op
                    .get("json")
                    .ok_or_else(|| format!("replace 第 {idx} 行缺少 json"))?;
                if !json.is_object() {
                    return Err(format!("第 {idx} 行必须是一个 JSON 对象"));
                }
                replaced.insert(idx, serde_json::to_string(json).map_err(|e| e.to_string())?);
            }
            "delete" => {
                let list = op
                    .get("indexes")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "delete 缺少 indexes".to_string())?;
                for v in list {
                    let idx = v
                        .as_u64()
                        .ok_or_else(|| "delete 的 indexes 必须是数字".to_string())?
                        as usize;
                    if idx >= n {
                        return Err(format!("delete 的行号 {idx} 越界（共 {n} 行）"));
                    }
                    if replaced.contains_key(&idx) {
                        return Err(format!("第 {idx} 行既被改又被删，拒绝写入"));
                    }
                    deleted.insert(idx);
                }
            }
            "insert" | "append" => {
                let at = if kind == "append" {
                    n
                } else {
                    op.get("at")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| "insert 缺少 at".to_string())? as usize
                };
                if at > n {
                    return Err(format!("insert 的位置 {at} 越界（共 {n} 行）"));
                }
                let json = op
                    .get("json")
                    .ok_or_else(|| "insert 缺少 json".to_string())?;
                if !json.is_object() {
                    return Err("新增的行必须是一个 JSON 对象".to_string());
                }
                inserts
                    .entry(at)
                    .or_default()
                    .push(serde_json::to_string(json).map_err(|e| e.to_string())?);
            }
            other => return Err(format!("未知的操作 {other}")),
        }
    }

    let mut out: Vec<OutLine> = Vec::with_capacity(n);
    for i in 0..=n {
        if let Some(batch) = inserts.get(&i) {
            for s in batch {
                out.push(OutLine::New(s.clone()));
            }
        }
        if i == n {
            break;
        }
        if deleted.contains(&i) {
            continue;
        }
        match replaced.get(&i) {
            Some(s) => out.push(OutLine::New(s.clone())),
            None => out.push(OutLine::Orig(i)),
        }
    }
    Ok(out)
}

/// 把 `OutLine` 落到最终文本行。
pub fn render_lines(lines: &[String], plan: &[OutLine]) -> Vec<String> {
    plan.iter()
        .map(|o| match o {
            OutLine::Orig(i) => lines[*i].clone(),
            OutLine::New(s) => s.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 校验
// ---------------------------------------------------------------------------

/// 校验结果容器。
#[derive(Debug, Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Report {
    fn err(&mut self, m: impl Into<String>) {
        self.errors.push(m.into());
    }
    fn warn(&mut self, m: impl Into<String>) {
        self.warnings.push(m.into());
    }
    fn into_json(self, stats: Value) -> Value {
        json!({
            "errors": self.errors,
            "warnings": self.warnings,
            "errorCount": self.errors.len(),
            "warnCount": self.warnings.len(),
            "stats": stats,
        })
    }
}

/// 单行校验的完整结论（除错误/提醒外，还带着「两类历史形态」的标记）。
///
/// 为什么要这两个标记：全量校验里 90+ 行都缺 `sessionId`，逐条写 warning 会把界面刷成
/// 一片红字；真正有用的是「共 N 行」这一句汇总。所以由调用方决定怎么聚合。
#[derive(Debug, Default)]
struct LineCheck {
    errors: Vec<String>,
    warnings: Vec<String>,
    /// 该行**整行没有** `sessionId`（不是不匹配）。
    missing_sid: bool,
    /// 该行 `id` 不是 UUID 形态（如 `cmp_…`）。
    odd_id: bool,
}

/// 单行内容校验（**严格版**：用户新增/改动的行必须完全合规）。
///
/// `expect_session_id`：该行 `sessionId` 必须等于它（客户端按这个字段过滤事件）。
pub fn check_line(line: &Value, expect_session_id: &str) -> (Vec<String>, Vec<String>) {
    let c = check_line_detail(line, expect_session_id, true);
    (c.errors, c.warnings)
}

/// 单行校验的实现，`strict` 决定两处「真实数据里本来就存在」的形态算不算错。
///
/// ⚠️ 为什么要分严格/宽松（2026-10-30 实测）：
/// 某条真实会话的正文里有 90+ 行**本来就缺 `sessionId`**，还有 `id: "cmp_793f78ae-…"`
/// 这种不是 UUID 的 id（WorkBuddy 自己的压缩/检查点事件）。早前写入前的全量校验拿严格
/// 判据逐行卡，于是这条会话**永远存不了盘** —— 用户只是想改个标题，也被挡在
/// 「必须修好才能写入（97）」上，等于编辑器对这批会话完全不可用。
///
/// 两类错误的性质完全不同：
/// - `sessionId` **存在但不等于本会话** ⇒ 真危险（会把别的会话事件混进来）⇒ 永远 error；
/// - `sessionId` **整行缺失** / `id` 非 UUID ⇒ 既有事实，客户端本来就这么存 ⇒ 宽松时只提醒。
///
/// 严格版仍用于「用户新增的行」与「前端改动校验」：新增行必须带齐字段，
/// 否则就是我们把不合规的数据写进库。
///
/// ⚠️ 上面那几行**不要缩进超过 3 个空格**：Rust 会把 doc 注释里缩进 4+ 的行当成
/// **Rust 代码块**去做 doctest 编译，中文自然编译不过（2026-10-31 `cargo test --workspace`
/// 的 doc-test 就是这样失败的）。要展示缩进文本请用 ```text 包裹。
fn check_line_with(
    line: &Value,
    expect_session_id: &str,
    strict: bool,
) -> (Vec<String>, Vec<String>) {
    let c = check_line_detail(line, expect_session_id, strict);
    (c.errors, c.warnings)
}

/// 校验实现本体；`strict` 只影响两类「历史形态」的定级（见上）。
fn check_line_detail(line: &Value, expect_session_id: &str, strict: bool) -> LineCheck {
    let mut r = Report::default();
    let mut out = LineCheck::default();
    let Some(o) = line.as_object() else {
        out.errors.push("该行必须是一个 JSON 对象".to_string());
        return out;
    };
    let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
    if ty.is_empty() {
        r.err("该行缺少 type");
    } else if !KNOWN_TYPES.contains(&ty) {
        r.warn(format!("未知的事件类型 {ty}（会原样保留，客户端可能不认识）"));
    }
    if let Some(id) = o.get("id").and_then(Value::as_str) {
        if !is_uuid_like(id) {
            out.odd_id = true;
            let m = format!("id 不是 UUID 形态：{id}");
            if strict {
                r.err(m);
            } else {
                // 客户端自己能读（`cmp_` 开头的压缩事件就是这样），原样保留即可。
                r.warn(format!("{m}（WorkBuddy 自己的事件 id，原样保留）"));
            }
        }
    }
    match ty {
        "message" => {
            let role = o.get("role").and_then(Value::as_str).unwrap_or("");
            if role != "user" && role != "assistant" {
                r.err(format!("message 的 role 必须是 user / assistant，实际是 {role:?}"));
            }
            if o.get("content").and_then(Value::as_array).is_none() {
                r.err("message 的 content 必须是数组");
            }
        }
        "function_call" => {
            if o.get("name").and_then(Value::as_str).unwrap_or("").is_empty() {
                r.err("function_call 缺少 name");
            }
            if o.get("callId").and_then(Value::as_str).unwrap_or("").is_empty() {
                r.err("function_call 缺少 callId（工具结果靠它配对）");
            }
        }
        "function_call_result" => {
            if o.get("callId").and_then(Value::as_str).unwrap_or("").is_empty() {
                r.err("function_call_result 缺少 callId");
            }
        }
        "reasoning" => {
            if !o.get("rawContent").map(|v| v.is_array()).unwrap_or(false)
                && !o.get("content").map(|v| v.is_array()).unwrap_or(false)
            {
                r.warn("reasoning 的 rawContent / content 都不是数组");
            }
        }
        _ => {}
    }
    match o.get("sessionId").and_then(Value::as_str) {
        None => {
            out.missing_sid = true;
            let m = "该行缺少 sessionId（客户端靠它筛选本会话的事件）";
            if strict {
                r.err(m);
            } else {
                r.warn(format!("{m}（WorkBuddy 原有事件本来就没带，原样保留）"));
            }
        }
        // ⚠️ 这一条永远是 error：带着**别的**会话 id 的行混进来，客户端会把事件挂错会话。
        Some(s) if s != expect_session_id => r.err(format!(
            "该行的 sessionId 是 {s}，必须等于本会话 {expect_session_id}"
        )),
        _ => {}
    }
    out.errors = r.errors;
    out.warnings = r.warnings;
    out
}

/// 前端每次改动调用的分级校验（毫秒级）。
///
/// `summary` 由前端从 `edit_doc` 带回：`{ "lineCount": n, "counts": { user: n, … } }`。
/// 有了它就能在**不读文件**的前提下算「改完之后还剩几条用户提问」——
/// 这正是 WorkBuddy 版的「空会话硬拦」。
pub fn validate_submit(
    ops: &Value,
    row_patch: &Value,
    columns: &Value,
    summary: &Value,
    session_id: &str,
) -> Value {
    let mut r = Report::default();
    let total = summary
        .get("lineCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    // --- 行改动 ---
    let mut del_user = 0usize;
    let mut new_user = 0usize;
    let mut new_lines = 0usize;
    let mut seen: HashSet<usize> = HashSet::new();

    match ops.as_array() {
        None => r.err("ops 必须是数组"),
        Some(arr) => {
            for op in arr {
                let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "replace" | "insert" | "append" => {
                        let json = match op.get("json") {
                            Some(v) => v,
                            None => {
                                r.err(format!("{kind} 缺少 json"));
                                continue;
                            }
                        };
                        // `replace` 改的是**既有行**（字段来自原行，可能本来就缺 sessionId），
                        // 用宽松判据；`insert` / `append` 是我们凭空加的新行，必须严格合规。
                        let strict = kind != "replace";
                        let (e, w) = check_line_with(json, session_id, strict);
                        for m in e {
                            r.err(m);
                        }
                        for m in w {
                            r.warn(m);
                        }
                        if classify_json_kind(json) == "user" {
                            new_user += 1;
                        }
                        new_lines += 1;
                        if kind == "replace" {
                            if let Some(i) = op.get("index").and_then(Value::as_u64) {
                                if !seen.insert(i as usize) {
                                    r.err(format!("第 {i} 行被重复修改"));
                                }
                                if let Some(orig) = summary
                                    .get("lineKinds")
                                    .and_then(|v| v.as_array())
                                    .and_then(|a| a.get(i as usize))
                                    .and_then(Value::as_str)
                                {
                                    if orig == "user" {
                                        del_user += 1;
                                    }
                                }
                            } else {
                                r.err("replace 缺少 index");
                            }
                        }
                    }
                    "delete" => {
                        match op.get("indexes").and_then(Value::as_array) {
                            None => r.err("delete 缺少 indexes"),
                            Some(list) => {
                                for v in list {
                                    match v.as_u64() {
                                        None => r.err("delete 的 indexes 必须是数字"),
                                        Some(i) => {
                                            let i = i as usize;
                                            if i >= total {
                                                r.err(format!("delete 的行号 {i} 越界（共 {total} 行）"));
                                            }
                                            if !seen.insert(i) {
                                                r.err(format!("第 {i} 行被重复修改"));
                                            }
                                            if let Some(k) = summary
                                                .get("lineKinds")
                                                .and_then(|v| v.as_array())
                                                .and_then(|a| a.get(i))
                                                .and_then(Value::as_str)
                                            {
                                                if k == "user" {
                                                    del_user += 1;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    other => r.err(format!("未知的操作 {other}")),
                }
            }
        }
    }

    // --- 空会话硬拦（WorkBuddy 版）---
    if total > 0 {
        let before = summary
            .get("counts")
            .and_then(|c| c.get("user"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let after = before.saturating_sub(del_user) + new_user;
        if after == 0 {
            r.err(
                "改完之后一条用户提问都不剩 —— 客户端打开这种会话会显示空白且无法继续对话。请至少保留或新增一条用户提问",
            );
        }
    }

    // --- 元数据字段 ---
    if let Some(patch) = row_patch.as_object() {
        let cols: Vec<Value> = columns.as_array().cloned().unwrap_or_default();
        for (name, value) in patch {
            let Some(col) = cols
                .iter()
                .find(|c| c.get("name").and_then(Value::as_str) == Some(name.as_str()))
            else {
                r.err(format!("sessions 表没有列 {name}"));
                continue;
            };
            if col.get("readOnly").and_then(Value::as_bool).unwrap_or(false) {
                r.err(format!("列 {name} 是主键，不允许改动（要换 id 请用「新增会话」）"));
                continue;
            }
            if value.is_null() && col.get("notNull").and_then(Value::as_bool).unwrap_or(false) {
                r.err(format!("列 {name} 是 NOT NULL，不能为空"));
            }
            let ty = col.get("type").and_then(Value::as_str).unwrap_or("");
            if ty.contains("INT") && !value.is_null() && !value.is_i64() && !value.is_u64() {
                r.err(format!("列 {name} 是整数列，当前值不是整数"));
            }
            if (ty.contains("TEXT") || ty.contains("CHAR")) && value.is_object() {
                r.err(format!("列 {name} 是文本列，不能塞对象"));
            }
        }
    } else if !row_patch.is_null() {
        r.err("rowPatch 必须是对象");
    }

    if del_user + new_user > 0 {
        r.warn("改动涉及用户提问的增减，请确认客户端能正常继续对话");
    }

    let stats = json!({
        "lineCount": total,
        "newLines": new_lines,
        "deleted": seen.len(),
        "userBefore": summary.get("counts").and_then(|c| c.get("user")).cloned().unwrap_or(json!(0)),
    });
    r.into_json(stats)
}

/// 只按 JSON 判种类（供 `validate_submit` 用，不依赖原文）。
///
/// ⚠️ 必须与 [`classify_line`] **同口径**（都走 `classify_user_message`）：这条数字直接进
/// 「改完之后还剩几条用户提问」的算式（`before - del_user + new_user`）。若这里只认
/// `<user_query>` 壳，那么**编辑一条没壳的旧提问**会被算成「删掉 1 条、新增 0 条」——
/// 单回合的导入会话（正文只有 1 条没壳提问）一改就被误判成空会话而拒写。
fn classify_json_kind(v: &Value) -> &'static str {
    if v.get("type").and_then(Value::as_str) != Some("message") {
        return "other";
    }
    if v.get("role").and_then(Value::as_str) != Some("user") {
        return "other";
    }
    let raw = workbuddy_source::join_content_blocks(v.get("content").unwrap_or(&Value::Null));
    match workbuddy_source::classify_user_message(&raw) {
        workbuddy_source::UserMsgKind::Real => "user",
        _ => "other",
    }
}

/// **全量校验**：对「应用 ops 之后」的整份正文跑一遍不变量检查。
///
/// 只在 [`edit_save`] 里调（要读整份文件），所以可以慢。
fn validate_full(lines: &[String], expect_session_id: &str) -> Report {
    let mut r = Report::default();
    if lines.is_empty() {
        r.err("改完之后正文一行都不剩 —— 客户端读不到任何事件");
        return r;
    }
    let mut ids: HashSet<String> = HashSet::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    let mut calls: HashSet<String> = HashSet::new();
    let mut results: HashSet<String> = HashSet::new();
    let mut users = 0usize;
    let mut bad = 0usize;
    // 两类「WorkBuddy 自己就这么存」的形态，只计数、最后汇总提醒（逐条刷屏没人看）。
    let mut no_sid = 0usize;
    let mut odd_id = 0usize;

    for (i, raw) in lines.iter().enumerate() {
        if raw.trim().is_empty() {
            r.err(format!("第 {i} 行是空行（空行会被客户端当成损坏事件）"));
            bad += 1;
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            r.err(format!("第 {i} 行不是合法 JSON"));
            bad += 1;
            continue;
        };
        // ⚠️ 这里**必须用宽松判据**：既有行原样带回时不能因为「WorkBuddy 自己就没写
        //    sessionId / id 不是 UUID」而拒绝整份写入 —— 那等于这类会话永远存不了盘。
        //    用户新增行的合规性由 `validate_submit`（严格版）在改动时就卡住。
        let c = check_line_detail(&v, expect_session_id, false);
        for m in c.errors {
            r.err(format!("第 {i} 行：{m}"));
        }
        if c.missing_sid {
            no_sid += 1;
        }
        if c.odd_id {
            odd_id += 1;
        }
        for m in c.warnings {
            // 这两类已在上面计数汇总，不再逐行重复。
            if !m.contains("缺少 sessionId") && !m.contains("不是 UUID 形态") {
                r.warn(format!("第 {i} 行：{m}"));
            }
        }
        let kind = classify_line(raw).map(|c| c.kind).unwrap_or_default();
        if kind == "user" {
            users += 1;
        }
        if kind == "tool_call" {
            if let Some(c) = v.get("callId").and_then(Value::as_str) {
                calls.insert(c.to_string());
            }
        }
        if kind == "tool_result" {
            if let Some(c) = v.get("callId").and_then(Value::as_str) {
                results.insert(c.to_string());
            }
        }
        if let Some(id) = v.get("id").and_then(Value::as_str) {
            if !ids.insert(id.to_string()) {
                r.err(format!("第 {i} 行的 id 与前面某行重复：{id}"));
            }
        }
        if let Some(p) = v.get("parentId").and_then(Value::as_str) {
            parents.push((i, p.to_string()));
        }
    }

    if users == 0 {
        r.err("整份正文里没有任何用户提问，客户端会显示成空白会话");
    }
    // 两类历史形态汇总成两句 —— 让用户知道「这 97 条不是我们的错，也不影响保存」。
    if no_sid > 0 {
        r.warn(format!(
            "有 {no_sid} 行本来就没有 sessionId（WorkBuddy 自己写的；客户端按会话过滤时会跳过它们，原样保留即可）"
        ));
    }
    if odd_id > 0 {
        r.warn(format!(
            "有 {odd_id} 行的事件 id 不是 UUID 形态（如 cmp_… 的压缩/检查点事件，客户端认，原样保留即可）"
        ));
    }
    for (i, p) in &parents {
        if !ids.contains(p) {
            r.warn(format!("第 {i} 行的 parentId 指向一个不存在的 id（断链，客户端会跳过这一支）"));
        }
    }
    let orphan_call: Vec<&String> = calls.difference(&results).collect();
    let orphan_result: Vec<&String> = results.difference(&calls).collect();
    if !orphan_call.is_empty() {
        r.warn(format!(
            "有 {} 个工具调用没有对应的结果行（客户端会显示成「执行中」）",
            orphan_call.len()
        ));
    }
    if !orphan_result.is_empty() {
        r.warn(format!(
            "有 {} 个工具结果没有对应的调用行",
            orphan_result.len()
        ));
    }
    r.warn(format!(
        "共 {} 行（其中 {} 行解析失败、{} 个用户提问）",
        lines.len(),
        bad,
        users
    ));
    r
}

// ---------------------------------------------------------------------------
// 写入
// ---------------------------------------------------------------------------

/// 目标账号清单（**只读**）。来源与「WorkBuddy 会话记录」页一致：`sessions.user_id` 去重。
pub fn list_target_uids() -> Value {
    let all = match workbuddy_source::list_sessions() {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e, "accounts": [] }),
    };
    let mut order: Vec<String> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for s in all.iter().filter(|s| !s.deleted) {
        if !counts.contains_key(&s.user_id) {
            order.push(s.user_id.clone());
        }
        *counts.entry(s.user_id.clone()).or_insert(0) += 1;
    }
    let accounts: Vec<Value> = order
        .into_iter()
        .map(|uid| {
            json!({
                "uid": uid,
                "label": workbuddy_accounts::label_for(&uid),
                "sessions": counts.get(&uid).copied().unwrap_or(0),
            })
        })
        .collect();
    json!({ "ok": true, "accounts": accounts })
}

/// 备份正文文件（可还原）。返回备份文件路径。
fn backup_body(src: &Path, cid: &str, tag: &str) -> Result<PathBuf, String> {
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dir = store_dir().join("backup").join(format!("wb_body_{tag}_{stamp}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建正文备份目录失败: {e}"))?;
    let dest = dir.join(format!("{cid}.jsonl"));
    std::fs::copy(src, &dest).map_err(|e| format!("备份正文失败: {e}"))?;
    let a = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let b = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    if a != b {
        return Err("正文备份大小校验失败，未做任何改动".to_string());
    }
    Ok(dest)
}

/// 原子替换正文文件：先写同目录 `.tmp`，再 `rename` 覆盖。
///
/// ⚠️ 不用「先删后写」：中途失败会留下一个不存在的正文，客户端直接读不到会话。
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("jsonl.twb-tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| format!("写入临时正文失败: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换正文文件失败: {e}")
    })
}

/// 覆写时更新 `sessions` 行的若干列。
fn update_row(
    conn: &Connection,
    cid: &str,
    uid: &str,
    patch: &serde_json::Map<String, Value>,
    cols: &[String],
) -> Result<usize, String> {
    let mut names: Vec<String> = Vec::new();
    let mut vals: Vec<SqlValue> = Vec::new();
    for (k, v) in patch {
        if !cols.iter().any(|c| c == k) {
            return Err(format!("sessions 表没有列 {k}"));
        }
        if k == "id" {
            return Err("不允许改 id（要换会话 id 请用「新增会话」）".to_string());
        }
        if k == "user_id" {
            let t = v.as_str().unwrap_or("");
            if t != uid {
                return Err(
                    "覆写不允许改归属账号（那等于把会话悄悄搬到另一个账号）；要换账号请用「新增会话」"
                        .to_string(),
                );
            }
        }
        names.push(k.clone());
        vals.push(json_to_sql(v)?);
    }
    if names.is_empty() {
        return Ok(0);
    }
    let sets = names
        .iter()
        .map(|c| format!("{c} = ?"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("UPDATE sessions SET {sets} WHERE id = ? AND user_id = ?");
    let mut params: Vec<SqlValue> = vals;
    params.push(SqlValue::Text(cid.to_string()));
    params.push(SqlValue::Text(uid.to_string()));
    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
        .map_err(|e| format!("更新会话行失败: {e}"))
}

/// 写入（覆写 / 新增）。
///
/// 参数：
/// - `mode`：`overwrite`（就地改这条会话）/ `insert`（新 UUID 造一条新会话，源会话一行不动）
/// - `ops`：行改动（见 [`apply_ops`]）
/// - `row_patch`：`sessions` 列改动
/// - `target_uid`：只在 `insert` 时可用；写入另一个账号
/// 写入目标账号（`insert` 可指定别的账号；覆写恒等于源账号）。
fn dest_uid_of(uid: &str, mode: &str, target_uid: Option<&str>) -> String {
    match (mode, target_uid) {
        ("insert", Some(t)) if !t.is_empty() => t.to_string(),
        _ => uid.to_string(),
    }
}

/// 写回会话（**外层**：占用门禁 + 自动退出客户端 + 写完自动拉回来）。
///
/// `close_client` 来自界面确认框里用户点的那一下：
/// - `false` 且客户端正占着这个号 ⇒ 返回 `{ok:false, code:"clientRunning"}`（**正常返回**，
///   让界面弹「要不要我来退出它再写」，而不是让用户自己去关）；
/// - `true` ⇒ 由本工具退出 WorkBuddy → 写入 → **无论成败都重新打开**（谁关的谁负责开）。
///
/// ⚠️ 关客户端属于破坏性动作，只走用户点过确认的这条路；`copy_sessions` 也是同样的口径。
pub fn edit_save(
    uid: &str,
    cid: &str,
    mode: &str,
    ops: &Value,
    row_patch: &Value,
    target_uid: Option<&str>,
    close_client: bool,
    progress: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let say = |m: &str| {
        if let Some(f) = progress {
            f(m);
        }
    };
    let dest_uid = dest_uid_of(uid, mode, target_uid);
    // 客户端在跑、且它登录的正是要写的这个号 ⇒ 它内存里有一份会话，写盘会被它覆盖回去。
    let occupied = workbuddy_switch::is_running()
        && workbuddy_auth::current_uid().as_deref() == Some(dest_uid.as_str());
    if occupied && !close_client {
        return Ok(json!({
            "ok": false,
            "code": "clientRunning",
            "stage": wbs::TARGET_IN_USE,
        }));
    }
    let stopped = if occupied {
        say("WorkBuddy 正在使用这个账号，先退出它再写入（写完自动重新打开）…");
        crate::modules::workbuddy_session_groups::ensure_closed(true)?
    } else {
        false
    };

    let outcome = edit_save_inner(uid, cid, mode, ops, row_patch, target_uid, progress);

    // ⚠️ 重启必须放在 `?` **之前**：中途任何一处失败都不能把客户端关在外面没人管。
    let (relaunched, restart_err) =
        crate::modules::workbuddy_session_groups::restart_client(stopped);
    let mut out = outcome?;
    if let Some(o) = out.as_object_mut() {
        o.insert("clientStopped".into(), json!(stopped));
        o.insert("clientRelaunched".into(), json!(relaunched));
        if let Some(e) = restart_err {
            o.insert("restartError".into(), json!(e));
        }
    }
    Ok(out)
}

/// 真正的写入流程（不碰客户端进程）。
fn edit_save_inner(
    uid: &str,
    cid: &str,
    mode: &str,
    ops: &Value,
    row_patch: &Value,
    target_uid: Option<&str>,
    progress: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let say = |m: &str| {
        if let Some(f) = progress {
            f(m);
        }
    };
    if uid.trim().is_empty() {
        return Err("账号缺少 uid".to_string());
    }
    if mode != "overwrite" && mode != "insert" {
        return Err(format!("未知的写入方式 {mode}"));
    }
    if mode == "overwrite" && target_uid.filter(|t| !t.is_empty()).is_some() {
        return Err(
            "覆写只能写回原账号 —— 允许覆写顺带换账号等于把一条会话悄悄搬到别处。要换账号请用「新增会话」"
                .to_string(),
        );
    }
    let dest_uid = dest_uid_of(uid, mode, target_uid);

    // 1) 只读校验归属
    let all = workbuddy_source::list_sessions()?;
    let row = all
        .iter()
        .find(|s| s.id == cid)
        .ok_or_else(|| format!("会话库里找不到会话 {cid}，未做任何改动"))?;
    if row.user_id != uid {
        return Err(format!("会话 {cid} 不属于该账号，未做任何改动"));
    }
    if row.deleted {
        return Err(format!("会话 {cid} 已删除，未做任何改动"));
    }
    if mode == "insert" {
        let known = list_target_uids();
        let exists = known["accounts"]
            .as_array()
            .map(|a| {
                a.iter()
                    .any(|v| v["uid"].as_str() == Some(dest_uid.as_str()))
            })
            .unwrap_or(false);
        if !exists {
            return Err(format!(
                "目标账号 {dest_uid} 在本机会话库里没有任何会话，不是可写入的账号"
            ));
        }
    }

    // 2) 占用门禁在外层 [`edit_save`] 处理（那里才能安全地「关客户端 → 写完再开回来」），
    //    内层假设调用方已经把客户端退出。

    // 3) 读原文 + 应用 ops
    let (src_path, lines) = read_body_lines(cid)?;
    say(&format!("读取正文：{} 行 / {} 字节", lines.len(), src_path.display()));
    let plan = apply_ops(&lines, ops)?;
    let mut next = render_lines(&lines, &plan);
    let new_cid = if mode == "insert" {
        uuid::Uuid::new_v4().to_string()
    } else {
        cid.to_string()
    };
    if mode == "insert" {
        // 正文里的会话 id 必须跟着换，否则客户端按 sessionId 过滤时一条都不认。
        for l in next.iter_mut() {
            if l.contains(cid) {
                *l = wbs::replace_session_id(l, cid, &new_cid);
            }
        }
    }

    // 4) 全量校验（任何 error 直接拒绝，**不写任何东西**）
    let report = validate_full(&next, &new_cid);
    if !report.errors.is_empty() {
        let mut out = report.into_json(json!({ "lineCount": next.len() }));
        out["ok"] = json!(false);
        out["stage"] = json!("全量校验未通过，未做任何改动");
        return Ok(out);
    }

    // 5) 备份（库 + 正文）
    say("备份会话库与正文…");
    let backup_db = wbs::backup_db(if mode == "insert" { "edit-insert" } else { "edit" })?;
    let backup_file = backup_body(&src_path, cid, if mode == "insert" { "insert" } else { "edit" })?;

    // 6) 写正文
    let dest_path = if mode == "insert" {
        wbs::target_body_path(&src_path, &new_cid)
    } else {
        src_path.clone()
    };
    if mode == "insert" && dest_path.exists() {
        return Err(format!(
            "目标正文已存在同名文件，已停止写入：{}",
            dest_path.display()
        ));
    }
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("目标工作区目录创建失败: {e}"))?;
    }
    say(&format!("写入正文：{}", dest_path.display()));
    atomic_write(&dest_path, &join_lines(&next))?;

    // 7) 写会话行
    let db = wbs::db_path();
    wbs::assert_not_edge_sync(&db)?;
    let patch = row_patch.as_object().cloned().unwrap_or_default();
    let written_cid = {
        let conn = wbs::open_rw(&db)?;
        let cols = wbs::target_columns(&conn)?;
        if mode == "overwrite" {
            let n = update_row(&conn, cid, uid, &patch, &cols)?;
            if n == 0 && !patch.is_empty() {
                let _ = std::fs::remove_file(&dest_path);
                return Err("会话行没有更新（影响 0 行），已停止写入".to_string());
            }
            cid.to_string()
        } else {
            // 新增：以源行为模板 + 列改动
            let snap = workbuddy_source::snapshot_db()?;
            let snap_conn = open_ro(&snap).ok_or_else(|| "会话快照无法打开".to_string())?;
            let mut src_row = wbs::read_source_row(&snap_conn, cid)?
                .ok_or_else(|| "源会话行读取失败".to_string())?;
            drop(snap_conn);
            // 把列改动合并进源行
            for (k, v) in &patch {
                if !cols.iter().any(|c| c == k) {
                    let _ = std::fs::remove_file(&dest_path);
                    return Err(format!("sessions 表没有列 {k}"));
                }
                if k == "id" || k == "user_id" {
                    continue; // 这两列由复制规则接管
                }
                let sv = json_to_sql(v)?;
                match src_row.iter_mut().find(|(c, _)| c == k) {
                    Some(slot) => slot.1 = sv,
                    None => src_row.push((k.clone(), sv)),
                }
            }
            let now = now_ms();
            wbs::insert_copy_row(&conn, &src_row, &new_cid, &dest_uid, now)?;
            new_cid.clone()
        }
    };

    // 8) 写后校验
    say("写后校验…");
    let check = (|| -> Result<(), String> {
        let conn = open_ro(&db).ok_or_else(|| "会话库打开失败".to_string())?;
        match wbs::row_owner(&conn, &written_cid) {
            Some(o) if o == dest_uid => {}
            Some(o) => return Err(format!("归属校验失败：期望 {dest_uid}，实际 {o}")),
            None => return Err("会话行写入后不可见".to_string()),
        }
        let p = workbuddy_source::find_jsonl(&written_cid)
            .ok_or_else(|| "正文文件写入后找不到".to_string())?;
        let written = std::fs::read_to_string(&p)
            .map_err(|e| format!("回读正文失败: {e}"))?;
        let got = split_lines(&written);
        if got.len() != next.len() {
            return Err(format!(
                "正文行数校验失败：期望 {} 行，实际 {} 行",
                next.len(),
                got.len()
            ));
        }
        Ok(())
    })();
    if let Err(e) = check {
        return Err(format!(
            "{e}\n\n正文备份：{}\n库备份：{}\n（可据此手工恢复）",
            backup_file.display(),
            backup_db.display()
        ));
    }

    say("完成");
    Ok(json!({
        "ok": true,
        "mode": mode,
        "sourceId": cid,
        "sessionId": written_cid,
        "ownerUid": dest_uid,
        "ownerLabel": workbuddy_accounts::label_for(&dest_uid),
        "lines": next.len(),
        "bodyPath": dest_path.to_string_lossy(),
        "bodyBackup": backup_file.to_string_lossy(),
        "dbBackup": backup_db.to_string_lossy(),
        "warnings": report.warnings,
        "edgeSyncSkipped": wbs::edge_sync_databases(),
    }))
}

// ---------------------------------------------------------------------------
// 单测
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<String> {
        vec![
            r#"{"type":"a"}"#.to_string(),
            r#"{"type":"b","n":1}"#.to_string(),
            r#"{"type":"c"}"#.to_string(),
        ]
    }

    /// 既有行原样保留时不能被「缺 sessionId / id 非 UUID」卡住（否则整条会话永远存不了盘）。
    ///
    /// 用户 2026-10-30 的实际场景：某会话里有 90+ 行 WorkBuddy 自己就没写 `sessionId`，
    /// 还有 `id: cmp_793f78ae-…` 这种压缩事件 id，于是「必须修好才能写入（97）」，
    /// 连只改标题都保存不了。
    #[test]
    fn legacy_session_lines_pass_lenient_but_not_strict() {
        let legacy = serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "text", "text": "hi"}],
            "id": "cmp_793f78ae-0b94-4545-9991-963fc2b8abbf",
        });
        // 宽松（写入前全量兜底 / replace 既有行）：放行，但两条都记成提醒
        let (e, w) = check_line_with(&legacy, "sid-1", false);
        assert!(e.is_empty(), "既有行不该拦：{e:?}");
        assert!(w.iter().any(|m| m.contains("缺少 sessionId")), "要提醒：{w:?}");
        assert!(w.iter().any(|m| m.contains("不是 UUID")), "要提醒：{w:?}");
        // 严格（用户新增/追加的行）：必须挡住
        let (e2, _) = check_line(&legacy, "sid-1");
        assert_eq!(e2.len(), 2, "新增行缺字段必须报错：{e2:?}");
        // 带着**别的**会话 id：宽松也一律 error（会把事件挂错会话）
        let other = serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [],
            "sessionId": "sid-other",
        });
        let (e3, _) = check_line_with(&other, "sid-1", false);
        assert_eq!(e3.len(), 1);
        assert!(e3[0].contains("必须等于本会话"), "{e3:?}");
    }

    #[test]
    fn blob_roundtrip_is_lossless() {
        let v = SqlValue::Blob(vec![0, 1, 2, 250]);
        let j = sql_to_json(&v);
        assert!(j.get("$blob").is_some());
        match json_to_sql(&j).unwrap() {
            SqlValue::Blob(b) => assert_eq!(b, vec![0, 1, 2, 250]),
            other => panic!("还原失败: {other:?}"),
        }
    }

    #[test]
    fn untouched_lines_stay_byte_identical() {
        // 只有第 1 行被改：第 0 / 2 行必须是 Orig（原样照抄）。
        let ops = json!([{ "op": "replace", "index": 1, "json": {"type":"b","n":2} }]);
        let plan = apply_ops(&lines(), &ops).unwrap();
        assert!(matches!(plan[0], OutLine::Orig(0)));
        assert!(matches!(plan[2], OutLine::Orig(2)));
        let out = render_lines(&lines(), &plan);
        assert_eq!(out[0], r#"{"type":"a"}"#);
        assert_eq!(out[2], r#"{"type":"c"}"#);
        assert!(out[1].contains("\"n\":2"));
    }

    #[test]
    fn delete_and_insert_land_in_the_right_place() {
        let ops = json!([
            { "op": "delete", "indexes": [1] },
            { "op": "insert", "at": 2, "json": {"type":"x"} },
            { "op": "append", "json": {"type":"z"} }
        ]);
        let plan = apply_ops(&lines(), &ops).unwrap();
        let out = render_lines(&lines(), &plan);
        assert_eq!(
            out,
            vec![
                r#"{"type":"a"}"#.to_string(),
                r#"{"type":"x"}"#.to_string(),
                r#"{"type":"c"}"#.to_string(),
                r#"{"type":"z"}"#.to_string(),
            ]
        );
    }

    #[test]
    fn same_line_cannot_be_both_replaced_and_deleted() {
        let ops = json!([
            { "op": "replace", "index": 1, "json": {"type":"b"} },
            { "op": "delete", "indexes": [1] }
        ]);
        assert!(apply_ops(&lines(), &ops).is_err());
    }

    #[test]
    fn out_of_range_index_is_rejected() {
        let ops = json!([{ "op": "delete", "indexes": [99] }]);
        assert!(apply_ops(&lines(), &ops).is_err());
        let ops = json!([{ "op": "insert", "at": 99, "json": {"type":"x"} }]);
        assert!(apply_ops(&lines(), &ops).is_err());
    }

    #[test]
    fn non_object_payload_is_rejected() {
        let ops = json!([{ "op": "replace", "index": 1, "json": [1, 2] }]);
        assert!(apply_ops(&lines(), &ops).is_err());
    }

    #[test]
    fn insert_at_end_appends() {
        let ops = json!([{ "op": "insert", "at": 3, "json": {"type":"tail"} }]);
        let plan = apply_ops(&lines(), &ops).unwrap();
        let out = render_lines(&lines(), &plan);
        assert_eq!(out.len(), 4);
        assert_eq!(out[3], r#"{"type":"tail"}"#);
    }

    #[test]
    fn split_join_lines_roundtrip() {
        let text = "a\nb\nc\n";
        let l = split_lines(text);
        assert_eq!(l, vec!["a", "b", "c"]);
        assert_eq!(join_lines(&l), text);
        // 没有结尾换行也要能还原
        let l2 = split_lines("a\nb");
        assert_eq!(l2, vec!["a", "b"]);
        assert_eq!(join_lines(&l2), "a\nb\n");
    }

    #[test]
    fn uuid_shape_check() {
        assert!(is_uuid_like("abdbb02b-bd38-415d-a738-f50776a5dce6"));
        assert!(!is_uuid_like("abdbb02bbd3841 5da738f50776a5dce6"));
        assert!(!is_uuid_like("01a120f0-65cd-725c-82d5-af851869eb2"));
        assert!(!is_uuid_like(""));
    }

    #[test]
    fn classifies_real_user_question_but_not_summary() {
        let real = r#"{"type":"message","role":"user","sessionId":"x","content":[{"type":"input_text","text":"<system-reminder>…</system-reminder>\n<user_query>帮我改一下这个表</user_query>"}]}"#;
        let info = classify_line(real).unwrap();
        assert_eq!(info.kind, "user");
        assert_eq!(info.text, "帮我改一下这个表");
        assert!(info.wrapped);
        assert_eq!(info.text_path, json!(["content", 0, "text"]));

        let summary = r#"{"type":"message","role":"user","sessionId":"x","content":[{"type":"input_text","text":"<conversation_history_summary>…</conversation_history_summary>"}]}"#;
        assert_eq!(classify_line(summary).unwrap().kind, "summary");

        let auto = r#"{"type":"message","role":"user","sessionId":"x","content":[{"type":"input_text","text":"Please continue with the conversation based on the summarized context"}]}"#;
        assert_eq!(classify_line(auto).unwrap().kind, "auto");
    }

    /// ⚠️ 回归：**没有 `<user_query>` 壳的真实提问也要算提问**。
    ///
    /// Trae → WorkBuddy 导出（v0.2.21 及更早）写出的用户行只有裸文本，没有壳。
    /// 早先这里按「有没有壳」二分 ⇒ 这类会话在编辑器里显示成 **0 个回合**
    /// （用户报的「从 trae 导入到 workbuddy 的会话，会话回合无法正常识别」）。
    #[test]
    fn unwrapped_real_user_question_still_opens_a_turn() {
        let bare = r#"{"type":"message","role":"user","sessionId":"x","content":[{"type":"input_text","text":"这个是百度文库的文件，给我下载原版文件"}]}"#;
        let info = classify_line(bare).unwrap();
        assert_eq!(info.kind, "user");
        assert_eq!(info.text, "这个是百度文库的文件，给我下载原版文件");
        // 没有壳 ⇒ 不能按「壳内层替换」写回，但仍要可编辑（否则只能去改原始 JSON）。
        assert!(!info.wrapped);
        assert_eq!(info.text_path, json!(["content", 0, "text"]));

        // 与 `parse_body` 同一出口：导入的会话能切出回合，且每一回合都挂在裸提问上。
        let reply = r#"{"type":"message","role":"assistant","status":"completed","sessionId":"x","content":[{"type":"output_text","text":"好的，我去本机跑脚本"}]}"#;
        let descs: Vec<Value> = [bare, reply]
            .iter()
            .enumerate()
            .map(|(i, r)| describe_line(i, r))
            .collect();
        let (turns, head) = build_turn_index(&descs);
        assert!(head.is_empty());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["start"], 0);
        assert_eq!(turns[0]["end"], 2);

        // 空会话硬拦的算式也要认它：编辑这条裸提问 ⇒ 新增 1 条、删掉 1 条，不该被判空。
        let op = json!({
            "op": "replace",
            "index": 0,
            "json": json!({
                "type": "message", "role": "user", "sessionId": "x",
                "content": [{ "type": "input_text", "text": "改后的提问" }]
            }),
        });
        let summary = json!({
            "lineCount": 2,
            "lineKinds": ["user", "assistant"],
            "counts": { "user": 1 },
        });
        let rep = validate_submit(&json!([op]), &json!(null), &json!([]), &summary, "x");
        assert_eq!(rep["errorCount"], 0, "误报空会话：{rep}");
    }

    #[test]
    fn classifies_assistant_reasoning_and_tools() {
        let a = r#"{"type":"message","role":"assistant","status":"completed","sessionId":"x","content":[{"type":"output_text","text":"已经跑完了"}]}"#;
        let i = classify_line(a).unwrap();
        assert_eq!(i.kind, "assistant");
        assert_eq!(i.text, "已经跑完了");
        assert_eq!(i.status, "completed");

        let r = r#"{"type":"reasoning","sessionId":"x","content":[],"rawContent":[{"type":"reasoning_text","text":"先看一下"}]}"#;
        let i = classify_line(r).unwrap();
        assert_eq!(i.kind, "reasoning");
        assert_eq!(i.text_path, json!(["rawContent", 0, "text"]));

        let c = r#"{"type":"function_call","name":"Bash","callId":"call_1","sessionId":"x","arguments":"{\"command\":\"ls\"}"}"#;
        let i = classify_line(c).unwrap();
        assert_eq!(i.kind, "tool_call");
        assert_eq!(i.text_path, json!(["arguments"]));

        let res = r#"{"type":"function_call_result","name":"Bash","callId":"call_1","status":"completed","sessionId":"x","output":{"type":"text","text":"total 0"}}"#;
        let i = classify_line(res).unwrap();
        assert_eq!(i.kind, "tool_result");
        assert_eq!(i.text, "total 0");
        assert_eq!(i.text_path, json!(["output", "text"]));
    }

    #[test]
    fn bad_json_line_is_reported_not_panicking() {
        assert!(classify_line("{not json").is_err());
        let d = describe_line(3, "{not json");
        assert_eq!(d["kind"], "bad");
        assert!(d["parseError"].is_string());
    }

    #[test]
    fn turn_index_groups_by_real_user_question() {
        let raws = vec![
            r#"{"type":"session-meta","sessionId":"x"}"#.to_string(),
            r#"{"type":"message","role":"user","sessionId":"x","content":[{"type":"input_text","text":"<user_query>第一个问题</user_query>"}]}"#.to_string(),
            r#"{"type":"reasoning","sessionId":"x","rawContent":[{"type":"reasoning_text","text":"想一下"}]}"#.to_string(),
            r#"{"type":"function_call","name":"Read","callId":"c1","sessionId":"x","arguments":"{}"}"#.to_string(),
            r#"{"type":"message","role":"assistant","sessionId":"x","content":[{"type":"output_text","text":"第一个回答"}]}"#.to_string(),
            r#"{"type":"message","role":"user","sessionId":"x","content":[{"type":"input_text","text":"<user_query>第二个问题</user_query>"}]}"#.to_string(),
            r#"{"type":"message","role":"assistant","sessionId":"x","content":[{"type":"output_text","text":"第二个回答"}]}"#.to_string(),
        ];
        let descs: Vec<Value> = raws
            .iter()
            .enumerate()
            .map(|(i, r)| describe_line(i, r))
            .collect();
        let (turns, head) = build_turn_index(&descs);
        assert_eq!(head, vec![0]);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["start"], 1);
        assert_eq!(turns[0]["end"], 5);
        assert_eq!(turns[0]["replyText"], "第一个回答");
        assert_eq!(turns[0]["tools"], json!(["Read"]));
        assert_eq!(turns[1]["start"], 5);
        assert_eq!(turns[1]["end"], 7);
    }

    #[test]
    fn validate_submit_blocks_emptying_all_user_questions() {
        let summary = json!({
            "lineCount": 3,
            "counts": { "user": 1 },
            "lineKinds": ["meta", "user", "assistant"]
        });
        let cols = json!([{ "name": "title", "type": "TEXT", "notNull": false, "readOnly": false }]);
        let ops = json!([{ "op": "delete", "indexes": [1] }]);
        let r = validate_submit(&ops, &json!({}), &cols, &summary, "sid");
        assert_eq!(r["errorCount"], 1);
        assert!(r["errors"][0].as_str().unwrap().contains("用户提问"));

        // 留下用户提问就不报这个错
        let ops = json!([{ "op": "delete", "indexes": [2] }]);
        let r = validate_submit(&ops, &json!({}), &cols, &summary, "sid");
        assert_eq!(r["errorCount"], 0);
    }

    #[test]
    fn validate_submit_checks_metadata_columns() {
        let summary = json!({ "lineCount": 3, "counts": { "user": 1 }, "lineKinds": [] });
        let cols = json!([
            { "name": "id", "type": "TEXT", "notNull": true, "readOnly": true },
            { "name": "custom_title", "type": "TEXT", "notNull": false, "readOnly": false },
            { "name": "updated_at", "type": "INTEGER", "notNull": false, "readOnly": false }
        ]);
        let r = validate_submit(
            &json!([]),
            &json!({ "id": "other-id" }),
            &cols,
            &summary,
            "sid",
        );
        assert!(r["errors"][0].as_str().unwrap().contains("主键"));

        let r = validate_submit(
            &json!([]),
            &json!({ "updated_at": "不是数字" }),
            &cols,
            &summary,
            "sid",
        );
        assert!(r["errors"][0].as_str().unwrap().contains("整数列"));

        let r = validate_submit(
            &json!([]),
            &json!({ "nope": 1 }),
            &cols,
            &summary,
            "sid",
        );
        assert!(r["errors"][0].as_str().unwrap().contains("没有列"));
    }

    #[test]
    fn validate_submit_rejects_wrong_session_id_in_new_line() {
        let summary = json!({ "lineCount": 2, "counts": { "user": 1 }, "lineKinds": [] });
        let cols = json!([]);
        let ops = json!([{
            "op": "append",
            "json": { "type": "message", "role": "assistant", "sessionId": "别的会话", "content": [{"type":"output_text","text":"x"}] }
        }]);
        let r = validate_submit(&ops, &json!({}), &cols, &summary, "本会话");
        assert!(r["errorCount"].as_u64().unwrap() >= 1);
        assert!(r["errors"][0].as_str().unwrap().contains("sessionId"));
    }

    #[test]
    fn full_validation_catches_duplicate_ids_and_orphan_calls() {
        let ls = vec![
            r#"{"type":"message","role":"user","id":"a","sessionId":"s","content":[{"type":"input_text","text":"<user_query>x</user_query>"}]}"#.to_string(),
            r#"{"type":"function_call","name":"Read","id":"b","callId":"c1","sessionId":"s","arguments":"{}"}"#.to_string(),
            r#"{"type":"message","role":"assistant","id":"a","sessionId":"s","content":[{"type":"output_text","text":"ok"}]}"#.to_string(),
        ];
        let r = validate_full(&ls, "s");
        assert!(r.errors.iter().any(|e| e.contains("重复")));
        assert!(r.warnings.iter().any(|w| w.contains("工具调用")));
    }

    #[test]
    fn full_validation_blocks_empty_body() {
        let r = validate_full(&[], "s");
        assert!(!r.errors.is_empty());
    }

    /// 缺 `sessionId` 的既有行：**只提醒、不拦写入**。
    ///
    /// 这条断言在 2026-10-30 反转过：原来是「必须报错」。实测 WorkBuddy 自己的正文里就有
    /// 大量不带 `sessionId` 的事件行，一旦计入 error，这类会话连「只改标题」都存不了盘
    /// （界面：「必须修好才能写入（97）」）。所以全量校验降级为 warning，
    /// 严格拦截只留给「用户新增的行」与「带着别的会话 id 的行」。
    #[test]
    fn full_validation_reports_missing_session_id() {
        let ls = vec![
            r#"{"type":"message","role":"user","sessionId":"s","content":[{"type":"input_text","text":"<user_query>x</user_query>"}]}"#.to_string(),
            r#"{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}"#.to_string(),
        ];
        let r = validate_full(&ls, "s");
        assert!(
            r.errors.is_empty(),
            "既有行缺 sessionId 不该阻断保存：{:?}",
            r.errors
        );
        assert!(r.warnings.iter().any(|w| w.contains("sessionId")), "{:?}", r.warnings);
    }

    #[test]
    fn truncate_is_char_safe() {
        let s = "中文中文中文";
        let (t, cut) = truncate_chars(s, 3);
        assert_eq!(t, "中文中");
        assert!(cut);
        let (t, cut) = truncate_chars(s, 99);
        assert_eq!(t, s);
        assert!(!cut);
    }
}
