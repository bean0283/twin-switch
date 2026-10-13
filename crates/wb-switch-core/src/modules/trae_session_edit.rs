//! Trae 会话编辑器：把一条会话的全部相关行读成可编辑的 `EditDoc`，再把编辑结果写回。
//!
//! ## 为什么不是「重建会话」
//!
//! Trae 的会话是**一组按逻辑 id 相互引用的表**（`chat_session` / `chat_message` /
//! `chat_message_general` / `chat_message_task` / `chat_turn` / `agent_run` / `session_project` …），
//! 库还是 SQLCipher 加密的。改坏任意一处引用，客户端表现就是「会话在但打不开 / 重启后消失 / 只剩一句」。
//! 所以本模块**只做两件事**：
//!
//! * **读**：用 `trae_delete::classify_tables()` 按列名枚举会话相关表（**不写死表名**），
//!   把该会话在这些表里的全部行读成 JSON（走 `reader_plain_path()`，**纯只读**）；
//! * **写**：照 `trae_delete::delete_sessions_core()` 的编排逐字复用既有原语 ——
//!   解析密钥 → 结束客户端 → 解密 → **合并 WAL** → 明文副本上改行 → **增量回写** →
//!   整库备份 + 原子替换 → 提升快照 → 重启。
//!
//! **绝不整库重写**（`write_db_incremental` 而非 `encrypt_db_file`），
//! **绝不跳过 WAL 合并**（否则抹掉客户端刚写下、还没 checkpoint 的数据）。
//!
//! ## 值的 JSON 映射（必须可逆）
//!
//! | SQLite | JSON | 回写 |
//! | --- | --- | --- |
//! | NULL | `null` | NULL |
//! | INTEGER | number | INTEGER |
//! | REAL | number（带小数） | REAL |
//! | TEXT | string | TEXT |
//! | BLOB | `{"$blob":"<base64>"}` | 还原为 BLOB |
//!
//! BLOB 用标记对象而不是直接丢掉：**用户改不动它，但必须能原样带回**，否则一存就把二进制字段清空。
//!
//! ## JSON 文本列
//!
//! `content` / `context` / `messages` 这类列在库里是 TEXT，存的却是一段 JSON。
//! 读的时候把它们**解成真正的 JSON 树**（前端才能按结构化表单渲染），
//! 写的时候再序列化回 TEXT。判定方式是「列类型是 TEXT 且抽样 ≥80% 的值能解析成对象/数组」。
//!
//! ## 大字段按引用返回（`$jsonRef`）
//!
//! 真机实测：一条 164 条消息的会话，光是 `chat_message_task.content` 就有 **74 MB**
//! （单行最大 9.8 MB）—— 那是助手每一步工具调用（读文件、diff、命令输出）的完整记录。
//! 整份 doc 若全量解析，实测 **90 MB**，走 IPC 会把前端直接冻死（`JSON.parse` + 渲染都扛不住）。
//!
//! 所以超过 `BIG_CELL_BYTES` 的 JSON 单元格不回传内容，只回一个**引用**：
//!
//! ```json
//! { "$jsonRef": { "pk": 12345, "bytes": 9783679, "preview": "{\"task_id\":\"6a24…" } }
//! ```
//!
//! * `pk` —— 本行主键（SQLite `INTEGER PRIMARY KEY`，即 rowid 别名），回查用，O(1)；
//! * `bytes` —— 原文长度，前端据此提示体积；写回时也用它判断「没动过」；
//! * `preview` —— 开头一小段，让用户看得出这是什么。
//!
//! 三条不变量：
//!
//! 1. **写回时引用必须解成原文**，绝不能把引用对象本身写进库（`ref_to_sql` 负责，`insert` /
//!    `overwrite` 两种方式都走它）。回查发生在**删旧行之前**，否则原行已经没了。
//! 2. **id 重映射必须跳过引用**（源会话的 pk 得留着才能回查），见 `rewrite_value`。
//! 3. **没动过的表整张跳过**：`edit_save` 收 `dirty_tables` 提示，只重写前端标脏的表。
//!    只改个标题就绝不碰那 74 MB —— 这正是「局部修改」要的效果。

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rusqlite::types::ValueRef;
use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Map, Value};

use crate::modules::config::store_dir;
use crate::modules::trae_decrypt::decrypt_database;
use crate::modules::trae_delete::{classify_tables, merge_wal_into_plain, resolve_live_key};
use crate::modules::trae_discover::{database_path, get_client};
use crate::modules::trae_export::{
    backup_dir, is_session_id, label_of, promote_snapshot, prune_with_policy, reader_plain_path,
};
use crate::modules::trae_import::{
    ensure_project_for, native_hex_id, patch_reserved_field, rewrite_last_real_project_id,
    session_owner_uid, write_db_incremental,
};
use crate::modules::trae_switch::{launch, stop_for_maintenance, MaintenanceStop};

/// 抽样上限：判定「这一列是不是 JSON 文本」时最多看多少行。
const JSON_SAMPLE: usize = 50;
/// 大 JSON 单元格阈值（字节）：超过只返回 `$jsonRef` 引用，内容按需拉取。
/// 32 KB 是权衡点 —— `chat_message_task.content` 平均 900 KB（必进引用），
/// 而 `history_v2` 那种 7 KB 的小行仍原样可见可改。
const BIG_CELL_BYTES: usize = 32 * 1024;
/// `$jsonRef.preview` 的字数上限。
const PREVIEW_CHARS: usize = 160;
/// 时间列名（用于格式告警）。
const TIME_COLS: [&str; 4] = ["created_at", "updated_at", "deleted_at", "pinned_at"];
/// 列名命中即认定为 JSON 文本（不再靠抽样）。
const JSON_NAME_HINTS: [&str; 6] = [
    "content",
    "context",
    "messages",
    "extra",
    "user_message_context",
    "skill_list_revisions",
];

// ---------------------------------------------------------------------------
// 元数据 / 单元格
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ColMeta {
    name: String,
    decl: String,
    notnull: bool,
    pk: bool,
    dflt: Option<String>,
}

#[derive(Clone)]
enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

fn cell_of(v: ValueRef<'_>) -> Cell {
    match v {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(i) => Cell::Int(i),
        ValueRef::Real(f) => Cell::Real(f),
        ValueRef::Text(t) => Cell::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Cell::Blob(b.to_vec()),
    }
}

fn table_meta(conn: &Connection, table: &str) -> Result<Vec<ColMeta>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(|e| format!("读取表结构 {table} 失败: {e}"))?;
    let cols = stmt
        .query_map([], |row| {
            Ok(ColMeta {
                name: row.get::<_, String>(1)?,
                decl: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                notnull: row.get::<_, i64>(3)? != 0,
                dflt: row.get::<_, Option<String>>(4)?,
                pk: row.get::<_, i64>(5)? != 0,
            })
        })
        .map_err(|e| format!("读取表结构 {table} 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("解析表结构 {table} 失败: {e}"))?;
    if cols.is_empty() {
        return Err(format!("表 {table} 不存在（客户端版本差异？）"));
    }
    Ok(cols)
}

/// 该列是不是「存着 JSON 的 TEXT 列」。
fn looks_json(s: &str) -> bool {
    let t = s.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return false;
    }
    matches!(serde_json::from_str::<Value>(s), Ok(v) if v.is_object() || v.is_array())
}

/// 该表里「参与 UNIQUE 索引」的列名集合。
///
/// 这张表的身份列就是它们 —— 复制会话时必须换新值，否则撞 `UNIQUE constraint`。
/// ⚠️ 光看主键不够：真机上 `chat_message` 的主键是自增 `id`，
/// 而 `message_id` 只是**带唯一索引的 TEXT 列**，漏掉它就报
/// `UNIQUE constraint failed: chat_message.message_id`（实测踩到过）。
fn unique_cols(conn: &Connection, table: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA index_list(\"{table}\")")) else {
        return out;
    };
    let names: Vec<String> = stmt
        .query_map([], |row| Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))
        .map(|it| {
            it.flatten()
                .filter(|(_, uniq)| *uniq != 0)
                .map(|(n, _)| n)
                .collect()
        })
        .unwrap_or_default();
    for idx in names {
        if let Ok(mut s2) = conn.prepare(&format!("PRAGMA index_info(\"{idx}\")")) {
            if let Ok(rows) = s2.query_map([], |r| r.get::<_, Option<String>>(2)) {
                for c in rows.flatten().flatten() {
                    out.insert(c);
                }
            }
        }
    }
    out
}

fn detect_json_columns(rows: &[Vec<Cell>], cols: &[ColMeta]) -> Vec<bool> {
    cols.iter()
        .enumerate()
        .map(|(i, c)| {
            let upper = c.decl.to_ascii_uppercase();
            let is_text = c.decl.is_empty() || upper.contains("TEXT") || upper.contains("CHAR");
            if !is_text {
                return false;
            }
            if JSON_NAME_HINTS
                .iter()
                .any(|h| c.name.eq_ignore_ascii_case(h))
            {
                return true;
            }
            let mut seen = 0usize;
            let mut ok = 0usize;
            for r in rows {
                if let Cell::Text(s) = &r[i] {
                    if s.trim().is_empty() {
                        continue;
                    }
                    seen += 1;
                    if looks_json(s) {
                        ok += 1;
                    }
                    if seen >= JSON_SAMPLE {
                        break;
                    }
                }
            }
            seen > 0 && ok * 100 / seen >= 80
        })
        .collect()
}

fn cell_to_json(c: &Cell, is_json: bool) -> Value {
    match c {
        Cell::Null => Value::Null,
        Cell::Int(i) => json!(i),
        Cell::Real(f) => json!(f),
        Cell::Text(s) => {
            if is_json {
                if let Ok(v) = serde_json::from_str::<Value>(s) {
                    if v.is_object() || v.is_array() {
                        return v;
                    }
                }
            }
            json!(s)
        }
        Cell::Blob(b) => json!({ "$blob": B64.encode(b) }),
    }
}

// ---------------------------------------------------------------------------
// 大字段引用（$jsonRef）
// ---------------------------------------------------------------------------

/// 是不是一个「大字段引用」。前端会把它渲染成「大字段 · N MB」卡片。
pub fn is_json_ref(v: &Value) -> bool {
    matches!(v.get("$jsonRef"), Some(m) if m.is_object())
}

/// 引用里记下的原行主键。写回时用它回查原文。
fn ref_pk(v: &Value) -> Option<&Value> {
    v.get("$jsonRef").and_then(|m| m.get("pk"))
}

/// 取开头一小段作预览（按**字符**截，别按字节 —— 中文会被切成半个字）。
fn preview_of(s: &str) -> String {
    let mut out: String = s.chars().take(PREVIEW_CHARS).collect();
    if s.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

/// 挑「回查键」列：必须是 **INTEGER 主键**（SQLite rowid 别名），回查才走 O(1) 索引。
///
/// 不能用 `message_id` 之类的 TEXT 主键当回查键：`chat_message_task.id` 是自增 INTEGER，
/// 而 `message_id` 在同表里不保证唯一。`fts_*` 表根本没有主键 ⇒ 不产生引用（它们也都很小）。
fn ref_key_col(cols: &[ColMeta]) -> Option<usize> {
    let is_int_pk = |c: &ColMeta| c.pk && c.decl.to_ascii_uppercase().contains("INT");
    cols.iter()
        .position(is_int_pk)
        .or_else(|| {
            cols.iter()
                .position(|c| c.name == "id" && c.decl.to_ascii_uppercase().contains("INT"))
        })
}

/// 按主键把引用解回**原始 SQL 值**。`table`/`col` 必须来自 `table_meta()`（不做字符串拼接校验）。
fn ref_to_sql(
    conn: &Connection,
    table: &str,
    col: &str,
    pk: &Value,
) -> Result<rusqlite::types::Value, String> {
    use rusqlite::types::Value as SV;
    let pk_col = {
        let cols = table_meta(conn, table)?;
        let ki = ref_key_col(&cols).ok_or_else(|| format!("{table} 没有 INTEGER 主键，无法回查大字段"))?;
        cols[ki].name.clone()
    };
    let param = match pk {
        Value::Number(n) => match n.as_i64() {
            Some(i) => SV::Integer(i),
            None => SV::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => SV::Text(s.clone()),
        other => return Err(format!("{table}.{col} 的大字段引用主键无效：{other}")),
    };
    let sql = format!("SELECT \"{col}\" FROM \"{table}\" WHERE \"{pk_col}\" = ?1");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("回查大字段 {table}.{col} 失败: {e}"))?;
    let mut rows = stmt
        .query(params![param])
        .map_err(|e| format!("回查大字段 {table}.{col} 失败: {e}"))?;
    match rows
        .next()
        .map_err(|e| format!("回查大字段 {table}.{col} 失败: {e}"))?
    {
        Some(r) => {
            let vr = r
                .get_ref(0)
                .map_err(|e| format!("回查大字段 {table}.{col} 失败: {e}"))?;
            Ok(match vr {
                ValueRef::Null => SV::Null,
                ValueRef::Integer(i) => SV::Integer(i),
                ValueRef::Real(f) => SV::Real(f),
                ValueRef::Text(t) => SV::Text(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(b) => SV::Blob(b.to_vec()),
            })
        }
        None => Err(format!(
            "{table}.{col}：大字段引用的原行（主键 {pk}）已不存在，无法回填。请刷新后重试"
        )),
    }
}

// ---------------------------------------------------------------------------
// 读：EditDoc
// ---------------------------------------------------------------------------

fn order_by(cols: &[ColMeta]) -> String {
    if cols.iter().any(|c| c.name == "message_index") {
        return " ORDER BY \"message_index\" ASC".into();
    }
    if let Some(pk) = cols.iter().find(|c| c.pk) {
        return format!(" ORDER BY \"{}\" ASC", pk.name);
    }
    String::new()
}

fn fetch_rows(
    conn: &Connection,
    table: &str,
    cols: &[ColMeta],
    scope: &str,
    sid: &str,
) -> Result<Vec<Vec<Cell>>, String> {
    let col_list = cols
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let from = match scope {
        "message" => format!(
            "\"{table}\" WHERE message_id IN \
             (SELECT message_id FROM chat_message WHERE session_id = ?1)"
        ),
        _ => format!("\"{table}\" WHERE session_id = ?1"),
    };
    let sql = format!("SELECT {col_list} FROM {from}{}", order_by(cols));
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("查询 {table} 失败: {e}"))?;
    let mut rows = stmt
        .query(params![sid])
        .map_err(|e| format!("查询 {table} 失败: {e}"))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| format!("读取 {table} 失败: {e}"))? {
        let mut vals = Vec::with_capacity(cols.len());
        for i in 0..cols.len() {
            vals.push(cell_of(
                r.get_ref(i).map_err(|e| format!("读取 {table} 第 {i} 列失败: {e}"))?,
            ));
        }
        out.push(vals);
    }
    Ok(out)
}

fn build_table(
    conn: &Connection,
    name: &str,
    scope: &str,
    sid: &str,
) -> Result<Value, String> {
    let cols = table_meta(conn, name)?;
    let raw = fetch_rows(conn, name, &cols, scope, sid)?;
    let json_flags = detect_json_columns(&raw, &cols);
    let uniq_cols = unique_cols(conn, name);
    let ref_col = ref_key_col(&cols);

    let mut ref_count = 0usize;
    let mut rows: Vec<Value> = Vec::with_capacity(raw.len());
    for r in &raw {
        let mut m = Map::new();
        // 主键得是能用的标量，否则整表退回「全量返回」（宁可慢，也不能把内容丢掉）。
        let pk_val = ref_col.and_then(|ki| match &r[ki] {
            Cell::Int(i) => Some(json!(i)),
            Cell::Real(f) => Some(json!(f)),
            _ => None,
        });
        for (i, c) in cols.iter().enumerate() {
            if json_flags[i] {
                if let Cell::Text(s) = &r[i] {
                    if s.len() > BIG_CELL_BYTES {
                        if let Some(pk) = pk_val.as_ref() {
                            ref_count += 1;
                            m.insert(
                                c.name.clone(),
                                json!({ "$jsonRef": {
                                    "pk": pk,
                                    "bytes": s.len(),
                                    "preview": preview_of(s),
                                }}),
                            );
                            continue;
                        }
                    }
                }
            }
            m.insert(c.name.clone(), cell_to_json(&r[i], json_flags[i]));
        }
        rows.push(Value::Object(m));
    }

    let cols_json: Vec<Value> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "name": c.name,
                "type": c.decl,
                "notNull": c.notnull,
                "pk": c.pk,
                "dflt": c.dflt,
                "json": json_flags[i],
                // 带 UNIQUE 索引 ⇒ 这张表的身份列。同库新增时这些列必须换新值，
                // 否则撞 `UNIQUE constraint`（`chat_message.message_id` 实测踩到过）。
                "unique": uniq_cols.contains(&c.name),
                // 自增主键交给 SQLite 分配：既不显示为可改，也不参与写回。
                "readOnly": c.pk && c.name == "id",
            })
        })
        .collect();

    Ok(json!({
        "name": name,
        "scope": scope,
        "editable": true,
        "derived": name.starts_with("fts_"),
        // 本表有多少个单元格被折叠成了 `$jsonRef`（前端据此提示「另有 N 个大字段未加载」）。
        "refCount": ref_count,
        "columns": cols_json,
        "rows": rows,
    }))
}

/// 读出该会话在全部相关表里的行。**只读**，不碰任何文件。
pub fn edit_doc(client_key: &str, session_id: &str) -> Result<Value, String> {
    if !is_session_id(session_id) {
        return Err(format!(
            "Session ID 格式不正确（20~24 位十六进制）：{session_id}"
        ));
    }
    let path = reader_plain_path(client_key)?;
    let conn = Connection::open(&path).map_err(|e| format!("打开解密库失败: {e}"))?;
    let (with_sid, mid_only) = classify_tables(&conn)?;
    let has_msg_table = with_sid.iter().any(|t| t == "chat_message");

    let mut warnings: Vec<String> = Vec::new();
    let mut tables: Vec<Value> = Vec::new();
    let mut counts = Map::new();

    for t in &with_sid {
        let v = build_table(&conn, t, "session", session_id)?;
        counts.insert(t.clone(), json!(v["rows"].as_array().map(|a| a.len()).unwrap_or(0)));
        tables.push(v);
    }
    if has_msg_table {
        for t in &mid_only {
            let v = build_table(&conn, t, "message", session_id)?;
            counts.insert(t.clone(), json!(v["rows"].as_array().map(|a| a.len()).unwrap_or(0)));
            tables.push(v);
        }
    } else if !mid_only.is_empty() {
        warnings.push(
            "库里没有 chat_message 表，按 message_id 关联的表已跳过（客户端版本差异？）".into(),
        );
    }

    let title = tables
        .iter()
        .find(|t| t["name"] == "chat_session")
        .and_then(|t| t["rows"].as_array())
        .and_then(|a| a.first())
        .and_then(|r| r["session_title"].as_str())
        .unwrap_or("")
        .to_string();
    if tables
        .iter()
        .find(|t| t["name"] == "chat_session")
        .and_then(|t| t["rows"].as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true)
    {
        return Err(format!(
            "会话 {session_id} 不在当前数据源中（可能已删除，或属于另一个账号）"
        ));
    }

    let ref_total: usize = tables
        .iter()
        .map(|t| t["refCount"].as_u64().unwrap_or(0) as usize)
        .sum();
    if ref_total > 0 {
        warnings.push(format!(
            "有 {ref_total} 个大字段（单个超过 {} KB）未随会话一起加载，显示为「大字段」卡片；\
             不展开就原样保留，展开后才能改",
            BIG_CELL_BYTES / 1024
        ));
    }

    Ok(json!({
        "clientKey": client_key,
        "clientLabel": label_of(client_key),
        "sessionId": session_id,
        "title": title,
        "ownerUid": session_owner_uid(client_key, session_id),
        "tables": tables,
        "counts": counts,
        "refCount": ref_total,
        "warnings": warnings,
    }))
}

/// 按需拉取一个**大字段**的完整内容（前端点「展开」时调）。
///
/// 只允许读 `classify_tables()` 认得出来的表 —— 表名 / 列名一律先跟真实表结构核对，
/// 不做任何字符串拼接式的白名单。
pub fn edit_cell(
    client_key: &str,
    session_id: &str,
    table: &str,
    column: &str,
    pk: &Value,
) -> Result<Value, String> {
    if !is_session_id(session_id) {
        return Err(format!(
            "Session ID 格式不正确（20~24 位十六进制）：{session_id}"
        ));
    }
    let path = reader_plain_path(client_key)?;
    let conn = Connection::open(&path).map_err(|e| format!("打开解密库失败: {e}"))?;

    let (with_sid, mid_only) = classify_tables(&conn)?;
    if !with_sid.iter().any(|t| t == table) && !mid_only.iter().any(|t| t == table) {
        return Err(format!("表 {table} 不属于本会话的数据表"));
    }
    let cols = table_meta(&conn, table)?;
    if !cols.iter().any(|c| c.name == column) {
        return Err(format!("表 {table} 没有列 {column}"));
    }
    let raw = ref_to_sql(&conn, table, column, pk)?;
    match raw {
        rusqlite::types::Value::Text(s) => {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                if v.is_object() || v.is_array() {
                    return Ok(json!({ "ok": true, "bytes": s.len(), "value": v }));
                }
            }
            Ok(json!({ "ok": true, "bytes": s.len(), "value": s }))
        }
        rusqlite::types::Value::Null => Ok(json!({ "ok": true, "bytes": 0, "value": Value::Null })),
        other => Err(format!("{table}.{column} 不是文本字段（{other:?}），无需展开")),
    }
}

// ---------------------------------------------------------------------------
// 校验（纯函数：只吃 doc，不碰库；前后端共用这一份）
// ---------------------------------------------------------------------------

fn tables_of(doc: &Value) -> Vec<&Value> {
    doc["tables"].as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

fn col_rows(t: &Value) -> Vec<&Value> {
    t["rows"].as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

fn is_time_col(name: &str) -> bool {
    TIME_COLS.iter().any(|c| name.eq_ignore_ascii_case(c))
}

fn looks_like_time(s: &str) -> bool {
    let b = s.as_bytes();
    // 宽松：`YYYY-MM-DD` 开头，或纯数字（秒/毫秒时间戳）
    (b.len() >= 10 && b[4] == b'-' && b[7] == b'-') || (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
}

/// 分级校验：`errors` 硬拦（不修好不让写），`warnings` 只提醒。
pub fn validate(doc: &Value) -> Value {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let sid = doc["sessionId"].as_str().unwrap_or("");
    if !is_session_id(sid) {
        errors.push(format!("会话 ID 格式不正确（需 20~24 位十六进制）：{sid}"));
    }

    let tables = tables_of(doc);

    // (1) 表内主键重复 / 非空 / 行内 session_id 一致 / 类型与时间格式
    for t in &tables {
        let name = t["name"].as_str().unwrap_or("?");
        let cols: Vec<(String, String, bool, bool)> = t["columns"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| {
                        (
                            c["name"].as_str().unwrap_or("").to_string(),
                            c["type"].as_str().unwrap_or("").to_uppercase(),
                            c["notNull"].as_bool().unwrap_or(false),
                            c["pk"].as_bool().unwrap_or(false),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let col_names: Vec<String> = cols.iter().map(|c| c.0.clone()).collect();
        let rows = col_rows(t);

        // 主键去重（复合主键时用「拼接值」判）
        let pk_idx: Vec<usize> = cols
            .iter()
            .enumerate()
            .filter(|(_, c)| c.3)
            .map(|(i, _)| i)
            .collect();
        if !pk_idx.is_empty() && !rows.is_empty() {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut dup = 0usize;
            for r in &rows {
                // ⚠️ 自增主键（`id`）由 SQLite 在 INSERT 时分配，**新增的行本来就没有这个键**
                //    （`readOnly` 列不参与写回，前端也不会给它补值）。
                //    若不跳过，两行缺失的主键都会被 `to_string()` 成 `"null"`，
                //    于是「新增 2 行以上」必然误报「主键重复」—— 功能直接被自己的校验拦住。
                //    判据：只要有任何一个主键列没值，这行就不参与去重（真实重复仍会被抓到）。
                if pk_idx.iter().any(|i| r[&col_names[*i]].is_null()) {
                    continue;
                }
                let key: Vec<String> = pk_idx
                    .iter()
                    .map(|i| {
                        r[&col_names[*i]]
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| r[&col_names[*i]].to_string())
                    })
                    .collect();
                if !seen.insert(key.join("\u{1}")) {
                    dup += 1;
                }
            }
            if dup > 0 {
                errors.push(format!("{name}：主键重复 {dup} 处"));
            }
        }

        // 身份列（带 UNIQUE 索引的列）在同一张表里不得重复。
        //
        // 重复一定会在写入时撞 `UNIQUE constraint failed` —— 但那时代码已经解密完整库、
        // 结束了客户端进程，代价很高；提前在这里拦下来，错误信息也直白得多。
        // 这条同时是 T46「新增回合 / 复制消息」两条自动换新 id 路径的护栏：
        // 换 id 的映射漏了某一列，就会在这里被抓住，而不是等到写库那一刻。
        // ⚠️ 跳过 `fts_*` 虚表 —— FTS5 虚表的 `index_list` 语义与普通表不同，不适合套这条规则。
        if !name.starts_with("fts_") {
            let uniq_names: Vec<String> = t["columns"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| {
                            c["unique"].as_bool().unwrap_or(false)
                                && !c["readOnly"].as_bool().unwrap_or(false)
                                && !c["json"].as_bool().unwrap_or(false)
                        })
                        .map(|c| c["name"].as_str().unwrap_or("").to_string())
                        .filter(|n| !n.is_empty() && col_names.iter().any(|x| x == n))
                        .collect()
                })
                .unwrap_or_default();
            for cn in &uniq_names {
                let mut seen: BTreeSet<String> = BTreeSet::new();
                let mut dup = 0usize;
                for r in &rows {
                    // 空值不参与（`NOT NULL` 由上面那条规则管，这里只管「撞唯一索引」）。
                    if let Some(s) = r[cn].as_str() {
                        if !s.is_empty() && !seen.insert(s.to_string()) {
                            dup += 1;
                        }
                    }
                }
                if dup > 0 {
                    errors.push(format!("{name}：{cn} 上有唯一索引，出现 {dup} 处重复值"));
                }
            }
        }

        for (i, c) in cols.iter().enumerate() {
            let (cname, ctype, notnull, is_pk) = (c.0.clone(), c.1.clone(), c.2, c.3);
            let is_json_col = t["columns"][i]["json"].as_bool().unwrap_or(false);
            for (ri, r) in rows.iter().enumerate() {
                let v = &r[&cname];
                if v.is_null() {
                    if notnull && !is_pk {
                        errors.push(format!("{name} 第 {} 行：{cname} 不允许为空", ri + 1));
                    }
                    continue;
                }
                // 类型不符
                match v {
                    Value::Object(_) | Value::Array(_) if !is_json_col => {
                        errors.push(format!(
                            "{name} 第 {} 行：{cname} 是 {ctype} 列，不能放对象/数组",
                            ri + 1
                        ));
                    }
                    Value::String(s) if ctype.contains("INT") || ctype.contains("REAL") => {
                        if !s.trim().is_empty() && s.trim().parse::<f64>().is_err() {
                            errors.push(format!(
                                "{name} 第 {} 行：{cname} 是数值列，当前值是 `{s}`",
                                ri + 1
                            ));
                        }
                    }
                    Value::String(s) if is_time_col(&cname) && !s.trim().is_empty() => {
                        if !looks_like_time(s.trim()) {
                            warnings.push(format!(
                                "{name} 第 {} 行：{cname} 不像时间（`{s}`）",
                                ri + 1
                            ));
                        }
                    }
                    _ => {}
                }
                if cname == "session_id" {
                    if let Value::String(s) = v {
                        if !s.is_empty() && s != sid {
                            errors.push(format!(
                                "{name} 第 {} 行：session_id 是 `{s}`，与本会话 `{sid}` 不一致",
                                ri + 1
                            ));
                        }
                    }
                }
                if cname == "message_id" {
                    if let Value::String(s) = v {
                        if s.trim().is_empty() {
                            errors.push(format!("{name} 第 {} 行：message_id 为空", ri + 1));
                        }
                    }
                }
            }
        }
    }

    // (2) message_id 交叉引用：general / task 的 message_id 必须能在 chat_message 找到
    let known: BTreeSet<String> = tables
        .iter()
        .find(|t| t["name"] == "chat_message")
        .map(|t| {
            col_rows(t)
                .iter()
                .filter_map(|r| r["message_id"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    for t in &tables {
        let name = t["name"].as_str().unwrap_or("");
        if name == "chat_message" || !name.starts_with("chat_message") {
            continue;
        }
        for (ri, r) in col_rows(t).iter().enumerate() {
            if let Some(mid) = r["message_id"].as_str() {
                if !mid.is_empty() && !known.contains(mid) {
                    errors.push(format!(
                        "{name} 第 {} 行：message_id `{mid}` 在 chat_message 里找不到对应行",
                        ri + 1
                    ));
                }
            }
        }
    }

    // (3) 消息序号连续性 / 助手回答是否为空
    if let Some(t) = tables.iter().find(|t| t["name"] == "chat_message") {
        let mut idx: Vec<i64> = col_rows(t)
            .iter()
            .filter_map(|r| r["message_index"].as_i64())
            .collect();
        if idx.len() > 1 {
            idx.sort_unstable();
            if idx.windows(2).any(|w| w[1] - w[0] != 1) {
                warnings.push("消息序号 message_index 不连续（客户端通常仍能打开）".into());
            }
        }
    }
    if let Some(t) = tables.iter().find(|t| t["name"] == "chat_message_task") {
        for (ri, r) in col_rows(t).iter().enumerate() {
            let c = r["content"].as_str().unwrap_or("");
            if c.is_empty() {
                continue;
            }
            if !c.contains("finish") {
                warnings.push(format!(
                    "chat_message_task 第 {} 行：没有 finish 节点，客户端可能显示不出助手回答",
                    ri + 1
                ));
            }
        }
    }

    // (4) context 必须是合法 JSON
    if let Some(t) = tables.iter().find(|t| t["name"] == "chat_session") {
        for (ri, r) in col_rows(t).iter().enumerate() {
            if let Value::String(s) = &r["context"] {
                if !s.trim().is_empty() && serde_json::from_str::<Value>(s).is_err() {
                    errors.push(format!(
                        "chat_session 第 {} 行：context 不是合法 JSON",
                        ri + 1
                    ));
                }
            }
        }
    }

    // (6) 空会话硬拦 —— 这是**客户端侧的真实故障**，不是风格问题（T46 真机取证，2026-10-10）。
    //
    // 把某个会话的 `chat_turn` / `chat_message` 全部删光、只留下 `chat_session` 那一行，
    // 在 Trae SOLO 里就是一个「空壳会话」：打开它时 `chat.getMessages` 返回 0 条，
    // chat-core 记下 `Session messages load failed`，随后沙箱环境**永远**停在
    // `[MessageInput] Please wait for Env Creating` —— 输入框再也发不出任何消息。
    //
    // 对照实测（同一客户端、同一天，`renderer.log` 计数）：
    //   10/7 全部启动 + 10/10 21:15 / 21:23（尚无空会话）→ **0 次**该告警；
    //   10/10 21:24 把「查看之前执行的任务历史」删空 → **5 次**；
    //   10/10 21:30 把「开发文件对比工具」删空   → **2 次**。
    //
    // 客户端没有自愈路径（删除按钮在列表上，而这条会话本身已经打不开），用户只能把整条
    // 会话删掉，所以这里必须**硬拦**。判据同时看两张表：只看 `chat_turn` 会漏掉
    // 「有 chat_message 无 chat_turn」的会话形态；两张表都没进 doc（例如只读了会话元数据）
    // 时不做判断，避免误报。
    if is_session_id(sid) {
        let rows_of = |name: &str| {
            tables
                .iter()
                .find(|t| t["name"].as_str() == Some(name))
                .map(|t| col_rows(t).len())
        };
        let turns = rows_of("chat_turn");
        let msgs = rows_of("chat_message");
        if (turns.is_some() || msgs.is_some()) && turns.unwrap_or(0) == 0 && msgs.unwrap_or(0) == 0 {
            errors.push(
                "会话不能是空的：`chat_turn` 与 `chat_message` 都没有行。\
                 客户端打开这种会话会一直卡在「环境创建中」，**发不出任何消息**；\
                 请至少保留一个回合，或者把整条会话删掉（记录页的「彻底删除会话」）。"
                    .into(),
            );
        }
    }

    // (5) 检索镜像
    let fts_rows: usize = tables
        .iter()
        .filter(|t| t["name"].as_str().unwrap_or("").starts_with("fts_"))
        .map(|t| col_rows(t).len())
        .sum();
    if fts_rows > 0 {
        warnings.push("fts_* 是检索镜像，改动只影响搜索命中，不影响会话内容".into());
    }

    json!({
        "ok": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
    })
}

// ---------------------------------------------------------------------------
// 写入：id 重映射（「新增」必须换 id，否则与同库源会话撞主键）
// ---------------------------------------------------------------------------

fn is_uuid_like(s: &str) -> bool {
    s.len() == 36 && s.as_bytes().iter().filter(|b| **b == b'-').count() == 4
}

fn is_hex_id_like(s: &str) -> bool {
    (20..=24).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// 该列是否「定义一个 id」（供重映射建表用）。
///
/// **定义型**列（主键 / `session_id` / 带 UNIQUE 索引的身份列）才生成新 id；
/// **引用型**列（`reply_to_message_id` / `parent_run_id` / 各类 `*_id`）不单独生成 ——
/// 它们指向的值必然已在某张表的定义型列里出现过，靠后面的**全局替换**一起改写。
///
/// ⚠️ 判据必须包含「带 UNIQUE 索引的 TEXT 列」：`chat_message` 的主键是自增 `id`，
/// 而真正唯一的是 `message_id`（TEXT + UNIQUE）。只看主键就会漏掉它，
/// 新增会话时直接 `UNIQUE constraint failed: chat_message.message_id`。
/// **判据：唯一索引标出来的才是身份列。**
///
/// `project_id` / `user_id` 例外：它们属于**账号**，跨账号才是另一套语义（S2），
/// 同账号复制必须原样保留 —— 它们一变，会话就跑到别的账号下面去了。
fn is_id_defining(cname: &str, decl: &str, pk: bool, unique: bool) -> bool {
    if cname == "project_id" || cname == "user_id" {
        return false;
    }
    if cname == "session_id" {
        return true;
    }
    let is_int = decl.to_ascii_uppercase().contains("INT");
    if pk && !is_int {
        return true;
    }
    if pk && cname != "id" {
        return true;
    }
    // 唯一索引上的非数值列 = 身份列（TEXT/UUID 形态的 id）。
    unique && !is_int
}

/// 规划 id 重映射：`existing` 是本库里已存在的 session_id 集合。
fn plan_id_map(doc: &Value, existing: &BTreeSet<String>) -> (String, HashMap<String, String>) {
    let old_sid = doc["sessionId"].as_str().unwrap_or("").to_string();
    let gen_sid = if existing.contains(&old_sid) || !is_session_id(&old_sid) {
        native_hex_id()
    } else {
        old_sid.clone()
    };

    let mut map: HashMap<String, String> = HashMap::new();
    map.insert(old_sid.clone(), gen_sid.clone());

    for t in tables_of(doc) {
        let cols: Vec<(String, String, bool, bool)> = t["columns"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| {
                        (
                            c["name"].as_str().unwrap_or("").to_string(),
                            c["type"].as_str().unwrap_or("").to_string(),
                            c["pk"].as_bool().unwrap_or(false),
                            c["unique"].as_bool().unwrap_or(false),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        for r in col_rows(t) {
            for (cname, decl, pk, uniq) in &cols {
                if !is_id_defining(cname, decl, *pk, *uniq) {
                    continue;
                }
                if let Some(s) = r[cname].as_str() {
                    if (is_hex_id_like(s) || is_uuid_like(s)) && !map.contains_key(s) {
                        let fresh = if is_uuid_like(s) {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            native_hex_id()
                        };
                        map.insert(s.to_string(), fresh);
                    }
                }
            }
        }
    }
    (gen_sid, map)
}

/// 按映射改写：**任何**字符串值只要整串等于某个旧 id 就替换；JSON 内部递归处理。
///
/// ⚠️ `$jsonRef` 整块跳过 —— 它里面的 `pk` 必须**保持原值**，写回时要靠它回查原文
/// （改了就对不上原行，直接写不回去）。
fn rewrite_value(v: &Value, map: &HashMap<String, String>) -> Value {
    if is_json_ref(v) {
        return v.clone();
    }
    match v {
        Value::String(s) => match map.get(s) {
            Some(new) => json!(new),
            None => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(|x| rewrite_value(x, map)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), rewrite_value(x, map)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

fn apply_id_map(doc: &Value, sid: &str, map: &HashMap<String, String>) -> Value {
    let mut out = doc.clone();
    out["sessionId"] = json!(sid);
    if let Some(tables) = out["tables"].as_array_mut() {
        for t in tables.iter_mut() {
            if let Some(rows) = t["rows"].as_array_mut() {
                for r in rows.iter_mut() {
                    *r = rewrite_value(r, map);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 写入：JSON → SQL
// ---------------------------------------------------------------------------

fn json_to_sql(v: &Value, is_json_col: bool) -> rusqlite::types::Value {
    use rusqlite::types::Value as SV;
    match v {
        Value::Null => SV::Null,
        Value::Bool(b) => SV::Integer(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SV::Integer(i)
            } else {
                SV::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => SV::Text(s.clone()),
        Value::Object(o) => {
            if let Some(b) = o.get("$blob").and_then(|b| b.as_str()) {
                match B64.decode(b) {
                    Ok(bytes) => SV::Blob(bytes),
                    Err(_) => SV::Blob(Vec::new()),
                }
            } else if is_json_col {
                SV::Text(serde_json::to_string(v).unwrap_or_default())
            } else {
                SV::Text(serde_json::to_string(v).unwrap_or_default())
            }
        }
        Value::Array(_) => SV::Text(serde_json::to_string(v).unwrap_or_default()),
    }
}

/// 一张表「已经解析成 SQL 值、随时可落库」的形态。
#[derive(Debug)]
struct ResolvedTable {
    name: String,
    cols: Vec<String>,
    rows: Vec<Vec<rusqlite::types::Value>>,
}

/// 把 doc 里一张表的行解析成 SQL 值。
///
/// ⚠️ **必须在删旧行之前调用** —— `$jsonRef` 要回查数据库里的原行，删掉就再也查不到了。
fn resolve_rows(conn: &Connection, t: &Value) -> Result<ResolvedTable, String> {
    let name = t["name"].as_str().unwrap_or("").to_string();
    let live_cols: BTreeSet<String> = table_meta(conn, &name)?
        .iter()
        .map(|c| c.name.clone())
        .collect();
    // 列名与「是不是 JSON 列」一趟遍历同时取 —— 分成两次 filter 就要靠 debug_assert 保序了。
    let pick: Vec<(String, bool)> = t["columns"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| !c["readOnly"].as_bool().unwrap_or(false))
                .map(|c| {
                    (
                        c["name"].as_str().unwrap_or("").to_string(),
                        c["json"].as_bool().unwrap_or(false),
                    )
                })
                .filter(|(n, _)| !n.is_empty() && live_cols.contains(n))
                .collect()
        })
        .unwrap_or_default();
    let cols: Vec<String> = pick.iter().map(|(n, _)| n.clone()).collect();
    let json_flags: Vec<bool> = pick.iter().map(|(_, j)| *j).collect();
    debug_assert_eq!(cols.len(), json_flags.len());

    let mut rows: Vec<Vec<rusqlite::types::Value>> = Vec::new();
    for r in col_rows(t) {
        let mut vals: Vec<rusqlite::types::Value> = Vec::with_capacity(cols.len());
        for (i, c) in cols.iter().enumerate() {
            let v = &r[c];
            if json_flags[i] && is_json_ref(v) {
                let pk = ref_pk(v).ok_or_else(|| format!("{name}.{c} 的大字段引用缺主键"))?;
                vals.push(ref_to_sql(conn, &name, c, pk)?);
            } else {
                vals.push(json_to_sql(v, json_flags[i]));
            }
        }
        rows.push(vals);
    }
    Ok(ResolvedTable { name, cols, rows })
}

/// 只清掉**一张表**里属于该会话的行。
///
/// 与 `delete_rows_in_conn`（一次清全部表）的区别：这里按表来，好让「没改过的表原封不动」。
fn delete_table_rows(
    conn: &Connection,
    name: &str,
    scope: &str,
    sid: &str,
) -> Result<usize, String> {
    let sql = match scope {
        "message" => format!(
            "DELETE FROM \"{name}\" WHERE message_id IN \
             (SELECT message_id FROM chat_message WHERE session_id = ?1)"
        ),
        _ => format!("DELETE FROM \"{name}\" WHERE session_id = ?1"),
    };
    conn.execute(&sql, [sid])
        .map_err(|e| format!("清除 {name} 失败: {e}"))
}

fn exec_insert(conn: &Connection, t: &ResolvedTable) -> Result<usize, String> {
    if t.cols.is_empty() {
        return Ok(0);
    }
    let col_list = t
        .cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let holders = vec!["?"; t.cols.len()].join(", ");
    let sql = format!("INSERT INTO \"{}\" ({col_list}) VALUES ({holders})", t.name);
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("准备写入 {} 失败: {e}", t.name))?;
    let mut n = 0usize;
    for vals in &t.rows {
        stmt.execute(params_from_iter(vals.iter()))
            .map_err(|e| format!("写入 {} 第 {} 行失败: {e}", t.name, n + 1))?;
        n += 1;
    }
    Ok(n)
}

fn existing_session_ids(conn: &Connection) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Ok(mut stmt) = conn.prepare("SELECT session_id FROM chat_session") {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for r in rows.flatten() {
                out.insert(r);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 写入：编排（照 delete_sessions_core 的顺序）
// ---------------------------------------------------------------------------

/// 把编辑结果写回。
///
/// `mode`：
/// * `overwrite` —— 就地覆写（保持原 session_id，先删该会话旧行再按 doc 重写）
/// * `insert` —— 在同库里新增一条会话（**必须换 session_id 与全部 id**，否则与源会话撞主键）
///
/// `dirty_tables`：前端标出的「用户真的改过的表」。给了就**只重写这些表**，其余原封不动 ——
/// 只改个标题就不必重写那 74 MB 的 `chat_message_task`。给 `None` 表示不信任提示，全部重写
/// （旧行为，最保险）。**这只是一个性能提示，判断错了也不会写坏数据**：漏标 = 少写一张表
/// （库里保持原样，正好是用户想要的），多标 = 多写一张表（慢一点而已）。
pub fn edit_save(
    doc: &Value,
    mode: &str,
    dirty_tables: Option<&[String]>,
    target_uid: Option<&str>,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(cb) = on_progress {
            cb(m);
        }
    };

    if mode != "overwrite" && mode != "insert" {
        return Err(format!("未知写入方式：{mode}（只支持 overwrite / insert）"));
    }
    // S2：跨账号写入。
    //
    // ⚠️ 只允许「新增」时指定目标账号。覆写是「就地改这一条」，它的语义天然绑在
    //    原账号上 —— 允许覆写顺带换账号，等于**悄悄把一条会话从 A 账号搬到 B 账号**，
    //    既覆盖数据又改变了原账号的会话清单，风险远大于收益。要换账号请用「新增」。
    let target_uid = match target_uid {
        Some(u) if u.trim().is_empty() => return Err("目标账号 uid 不能为空".into()),
        Some(_) if mode != "insert" => {
            return Err(
                "「写入到别的账号」只在写入方式 = 新增时可用；覆写只能写回原账号。\
                 若要放进另一个账号，请把写入方式改成「新增」。"
                    .into(),
            )
        }
        other => other.map(str::trim),
    };
    let report = validate(doc);
    if !report["errors"].as_array().map(|a| a.is_empty()).unwrap_or(false) {
        let list: Vec<String> = report["errors"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        return Err(format!("校验未通过，未写入任何数据：\n{}", list.join("\n")));
    }

    let client_key = doc["clientKey"]
        .as_str()
        .ok_or("doc 缺 clientKey")?
        .to_string();
    let src_sid = doc["sessionId"]
        .as_str()
        .ok_or("doc 缺 sessionId")?
        .to_string();

    let client = get_client(&client_key).ok_or("未知客户端")?;
    let live = database_path(client);
    if !live.exists() {
        return Err(format!("实时加密库不存在：{}", live.display()));
    }
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let work = store_dir()
        .join("trae")
        .join("edit_tmp")
        .join(format!("{client_key}-{stamp}"));
    let cleanup = |work: &Path| {
        let _ = std::fs::remove_dir_all(work);
    };
    let err = |e: String| -> Result<Value, String> {
        cleanup(&work);
        Err(e)
    };

    log("校验实时库密钥…");
    let enc_key_hex = match resolve_live_key(&client_key, &live) {
        Ok(k) => k,
        Err(e) => return Err(e),
    };

    // ⚠️⚠️ T61：走 `stop_for_maintenance`，不能直接 `kill_all` —— 对「已并入且本机已无
    //    自己程序」的 SOLO，后者会回退成承载方 `Trae CN` 的映像名，把用户正在用的 IDE 一起杀。
    match stop_for_maintenance(client, 10_000) {
        Ok(MaintenanceStop::NotRunning) => {}
        Ok(MaintenanceStop::NoOwnProgram) => log(&format!(
            "{} 已并入其他客户端、本机没有它自己的程序：不结束任何进程（它的旧库没有被占用）。",
            client.label
        )),
        Ok(MaintenanceStop::Stopped(n)) => {
            log("结束客户端进程…");
            log(&format!("已结束 {n} 个进程，等待退出…"));
        }
        Err(e) => return Err(e),
    }

    log("解密实时库为明文副本（可能数百 MB，请稍候）…");
    let plain = work.join("target-plain.db");
    if let Err(e) = decrypt_database(&live, &enc_key_hex, &plain, Some(&|m| log(&format!("   {m}")))) {
        return err(e);
    }

    let wal = live.with_extension("db-wal");
    let wal_merged = match merge_wal_into_plain(&plain, &wal, &enc_key_hex) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    if wal_merged > 0 {
        log(&format!("已合并 WAL 中 {wal_merged} 个已提交页面帧"));
    }
    if let Err(e) = patch_reserved_field(&plain) {
        return err(e);
    }

    // 明文副本上改行（单个事务）
    let (final_sid, per_table, removed_rows, skipped_tables) = {
        let mut conn = match Connection::open(&plain) {
            Ok(c) => c,
            Err(e) => return err(format!("打开明文副本失败: {e}")),
        };
        let (sid, map) = match mode {
            "insert" => {
                let existing = existing_session_ids(&conn);
                let (new_sid, map) = plan_id_map(doc, &existing);
                log(&format!("新增模式：session_id {src_sid} → {new_sid}，并重映射 {} 个 id", map.len()));
                (new_sid, Some(map))
            }
            _ => (src_sid.clone(), None),
        };
        let work_doc = match &map {
            Some(m) => apply_id_map(doc, &sid, m),
            None => doc.clone(),
        };

        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => return err(format!("开启事务失败: {e}")),
        };

        // 作用域一律按**真实表结构**判（不信 doc 里的 scope 字段）。
        let (sid_set, mid_set) = classify_tables(&tx)?;
        let is_msg = |n: &str| mid_set.iter().any(|x| x == n);
        let known = |n: &str| sid_set.iter().any(|x| x == n) || mid_set.iter().any(|x| x == n);
        let want = |n: &str| match dirty_tables {
            Some(list) => list.iter().any(|x| x == n),
            None => true,
        };

        let mut names: Vec<String> = Vec::new();
        for t in tables_of(&work_doc) {
            let n = t["name"].as_str().unwrap_or("").to_string();
            if n.is_empty() || !known(&n) {
                log(&format!("跳过未知表 {n}（不属于本会话数据表）"));
                continue;
            }
            if mode == "overwrite" && !want(&n) {
                continue;
            }
            names.push(n);
        }

        // 阶段一：解析成 SQL 值。`$jsonRef` 在这里回查原文 —— 必须早于任何删除动作。
        let mut resolved: Vec<ResolvedTable> = Vec::new();
        for t in tables_of(&work_doc) {
            let n = t["name"].as_str().unwrap_or("");
            if !names.iter().any(|x| x == n) {
                continue;
            }
            match resolve_rows(&tx, t) {
                Ok(r) => resolved.push(r),
                Err(e) => return err(e),
            }
        }

        // 阶段二：删旧行（仅 overwrite，且只删要重写的表）。先删按 message_id 关联的表 ——
        // 它们靠 `chat_message.message_id` 定位，必须赶在 chat_message 自己的行被删之前。
        let mut removed = 0usize;
        if mode == "overwrite" {
            for pass_msg in [true, false] {
                for r in &resolved {
                    if is_msg(&r.name) != pass_msg {
                        continue;
                    }
                    let scope = if pass_msg { "message" } else { "session" };
                    match delete_table_rows(&tx, &r.name, scope, &src_sid) {
                        Ok(n) => removed += n,
                        Err(e) => return err(e),
                    }
                }
            }
            log(&format!(
                "已清除该会话旧数据 {removed} 行（仅涉及 {} 张改动过的表）",
                resolved.len()
            ));
        }

        // 阶段三：写新行（保持 doc 顺序：先 session 作用域、后 message 作用域）。
        //
        // ⚠️⚠️ `fts_*` 必须**最后**写，而且写之前先按目标会话清一遍（T46 真机实测到的重复）。
        // 这两张是 FTS5 虚表，**客户端用触发器自动维护**：
        //   `trg_session_title_ai`   AFTER INSERT ON chat_session        → fts_session_title
        //   `trg_message_content_ai` AFTER INSERT ON chat_message_general → fts_message_content
        // 我们插 `chat_session` / `chat_message_general` 时触发器已经补了一份词条，
        // 再照抄源库的 `fts_*` 行 ⇒ **词条翻倍**（实测 `fts_session_title` 1 → 2 行）。
        // 但触发器补的那份**不完整**（只认 `$.type == "text"` 的片段，源库里
        // `[{"text_content":…}]` 这种没有 `type` 的它取不到文本），所以也不能干脆不写。
        // 既有 `copy_session` 的做法就是「先按目标会话 DELETE，再整份写入」，这里与它保持一致。
        // 顺序同样重要：清理必须排在 `chat_message_general` 的插入之后，否则触发器的补词条又回来了。
        let mut counts: Vec<(String, usize)> = Vec::new();
        for pass_fts in [false, true] {
            for r in &resolved {
                if r.name.starts_with("fts_") != pass_fts {
                    continue;
                }
                if pass_fts {
                    // 词条的归属列都是 `session_id`（`fts_message_content` 是复合虚表，
                    // `session_id` 也带）。按新会话 id 清掉触发器刚补的半成品 + 上一轮残留。
                    if let Err(e) = tx.execute(
                        &format!("DELETE FROM \"{}\" WHERE session_id = ?", r.name),
                        [&sid],
                    ) {
                        return err(format!("清理表 {} 的旧检索词条失败: {e}", r.name));
                    }
                }
                match exec_insert(&tx, r) {
                    Ok(n) => counts.push((r.name.clone(), n)),
                    Err(e) => return err(e),
                }
            }
        }

        // 阶段四（S2）：跨账号写入 —— 把新会话的工程归属指到目标账号。
        //
        // 复用 `trae_import::ensure_project_for()`（与既有「同库跨账号复制」同一条出口）：
        // 在目标账号下找**同路径**的工程，找不到就复制源工程行、换新 project_id 新建一个。
        //
        // ⚠️ 归属必须改**两处**，第三处由下面阶段五的对齐兜底 —— 漏任何一处都会
        //    「客户端里删不掉这条会话、重启后记录又复活」（T29 的坑），
        //    因为客户端按 `session_project` 组织工程下的会话、又用 `context` 定位真实工程：
        //      ① `chat_session.project_id`
        //      ② `chat_session.context` 里的 `last_real_project_id`
        //      ③ `session_project.project_id`（跟着 ① 走）
        if let Some(uid) = target_uid {
            let pid = ensure_project_for(&tx, &tx, &src_sid, uid)?;
            log(&format!("跨账号写入：目标账号 uid {uid} 的工程归属 → {pid}"));
            if let Err(e) = tx.execute(
                "UPDATE chat_session SET project_id = ?1 WHERE session_id = ?2",
                params![pid, sid],
            ) {
                return err(format!("改写会话工程归属失败: {e}"));
            }
            let ctx: Option<String> = tx
                .query_row(
                    "SELECT context FROM chat_session WHERE session_id = ?1",
                    params![sid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();
            if let Some(c) = ctx {
                if let Some(fixed) = rewrite_last_real_project_id(&c, &pid) {
                    if let Err(e) = tx.execute(
                        "UPDATE chat_session SET context = ?1 WHERE session_id = ?2",
                        params![fixed, sid],
                    ) {
                        return err(format!("改写 context.last_real_project_id 失败: {e}"));
                    }
                    log("已同步改写 context 里的 last_real_project_id");
                }
            }
        }

        // 阶段五：改了 chat_session.project_id 就必须把 session_project 拉回一致 ——
        // T29 的坑：两者失配时客户端**删不掉这条会话，而且重启后它还会复活**。
        // 只在真的动过 chat_session 时兜底，正常编辑一行都不碰。
        if names.iter().any(|n| n == "chat_session") {
            match tx.execute(
                "UPDATE session_project SET project_id = (\
                     SELECT s.project_id FROM chat_session s \
                     WHERE s.session_id = session_project.session_id\
                 ) \
                 WHERE EXISTS (\
                     SELECT 1 FROM chat_session s \
                     WHERE s.session_id = session_project.session_id \
                       AND ifnull(s.project_id, '') <> ifnull(session_project.project_id, '')\
                 )",
                [],
            ) {
                Ok(0) => {}
                Ok(n) => log(&format!("已对齐 {n} 行工程归属（session_project）")),
                Err(e) => return err(format!("对齐工程归属失败: {e}")),
            }
        }

        if let Err(e) = tx.commit() {
            return err(format!("提交失败: {e}"));
        }
        let total: usize = counts.iter().map(|(_, n)| *n).sum();
        let skipped: Vec<String> = tables_of(&work_doc)
            .into_iter()
            .map(|t| t["name"].as_str().unwrap_or("").to_string())
            .filter(|n| !n.is_empty() && !names.iter().any(|x| x == n))
            .collect();
        if mode == "overwrite" && !skipped.is_empty() {
            log(&format!(
                "另有 {} 张表未改动，已原样保留：{}",
                skipped.len(),
                skipped.join(", ")
            ));
        }
        log(&format!("已写入 {total} 行，分布于 {} 张表", counts.len()));
        (sid, counts, removed, skipped)
    };

    log("增量加密回写（只重写变动页）…");
    let new_db = work.join("target-new.db");
    let stats = match write_db_incremental(
        &enc_key_hex,
        &live,
        &plain,
        &new_db,
        Some(&|m| log(&format!("   {m}"))),
    ) {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    log(&format!(
        "增量回写完成：全库 {} 页，仅重写 {} 页（{:.1} MB），用时 {} ms",
        stats.pages,
        stats.changed_pages,
        stats.changed_bytes as f64 / 1_048_576.0,
        stats.elapsed_ms
    ));

    log("备份原库并原子替换…");
    let bdir = backup_dir();
    if let Err(e) = std::fs::create_dir_all(&bdir) {
        return err(format!("创建备份目录失败: {e}"));
    }
    let mut backup_paths: Vec<String> = Vec::new();
    for (label, suffix) in [("主库", ""), ("WAL", "-wal"), ("SHM", "-shm")] {
        let f = PathBuf::from(format!("{}{}", live.to_string_lossy(), suffix));
        if f.exists() {
            let dst = bdir.join(format!("{client_key}_before_edit_{stamp}{suffix}.db.bak"));
            match std::fs::copy(&f, &dst) {
                Ok(_) => backup_paths.push(dst.to_string_lossy().into_owned()),
                Err(e) => return err(format!("备份{label}失败，已中止写入：{e}")),
            }
        }
    }
    {
        // ⚠️ 走**用户设置里的保留策略**。T62 之前这里写死 `keep=1`，而且
        // `backup_owner_and_stamp` 不认 `_before_edit_` ⇒ 每次编辑留下的备份各自成组、
        // 一份都删不掉（实测 15 份 × 378 MB）。策略自带「每个客户端最新一批永不清」。
        let (n, freed) = prune_with_policy(&bdir);
        if n > 0 {
            log(&format!(
                "已清理 {n} 个旧备份，回收 {:.1} MB（按设置里的整库备份保留策略）",
                freed as f64 / 1_048_576.0
            ));
        }
    }
    let swap = || -> Result<(), String> {
        let shm = live.with_extension("db-shm");
        if wal.exists() {
            std::fs::remove_file(&wal).map_err(|e| format!("清理 WAL 失败: {e}"))?;
        }
        if shm.exists() {
            std::fs::remove_file(&shm).map_err(|e| format!("清理 SHM 失败: {e}"))?;
        }
        if live.exists() {
            std::fs::remove_file(&live).map_err(|e| format!("移除旧库失败: {e}"))?;
        }
        std::fs::rename(&new_db, &live).map_err(|e| format!("替换数据库失败: {e}"))?;
        Ok(())
    };
    if let Err(e) = swap() {
        return err(format!("替换失败（备份已保留，可手动恢复）：{e}"));
    }

    if let Err(e) = promote_snapshot(&client_key, &plain) {
        log(&format!("快照更新失败（不影响写入结果，下次读取会重新解密）：{e}"));
        crate::modules::trae_export::drop_snapshot_meta(&client_key);
    }

    let relaunched = match launch(&client_key, None) {
        Ok(_) => {
            log("写入成功，已自动重启客户端");
            true
        }
        Err(e) => {
            log(&format!("写入成功，但自动重启客户端失败：{e}"));
            false
        }
    };

    cleanup(&work);
    log("写入完成");
    Ok(json!({
        "ok": true,
        "mode": mode,
        "sourceSessionId": src_sid,
        "sessionId": final_sid,
        "idRemapped": mode == "insert",
        "targetUid": target_uid,
        "tables": per_table.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<String, Value>>(),
        "rows": per_table.iter().map(|(_, v)| *v).sum::<usize>(),
        "removedRows": removed_rows,
        "skippedTables": skipped_tables,
        "walMerged": wal_merged,
        "pages": stats.pages,
        "changedPages": stats.changed_pages,
        "backup": backup_paths,
        "relaunched": relaunched,
        "warnings": report["warnings"],
    }))
}

/// 会话级批量删除：直接复用既有实现（一趟完成：一次备份、一次全库加解密）。
pub fn edit_delete_sessions(
    client_key: &str,
    session_ids: &[String],
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    crate::modules::trae_delete::delete_sessions(client_key, session_ids, on_progress)
}

/// 「跨账号写入」的目标账号清单（**只读**，不写库、不杀手）。
///
/// 数据源是解密库里的 `project.user_id` —— 这正是 Trae 判定会话归属的那一列，
/// 与 `trae_import::session_owner_uid()` 走同一条链路，所以「能选的目标」= 库里已有工程的账号。
/// ⚠️ 显示名统一走 `trae_vault::account_label()`（与账号页同一个出口），**别自己截 uid 尾号**。
pub fn list_target_uids(client_key: &str) -> Result<Value, String> {
    if get_client(client_key).is_none() {
        return Err(format!("未知客户端：{client_key}"));
    }
    let path = reader_plain_path(client_key)?;
    let conn = Connection::open(&path).map_err(|e| format!("打开解密库失败: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT p.user_id, \
                    count(DISTINCT p.project_id), \
                    count(DISTINCT s.session_id) \
             FROM project p \
             LEFT JOIN chat_session s ON s.project_id = p.project_id \
             WHERE p.user_id IS NOT NULL AND ifnull(p.deleted_at, 0) = 0 \
             GROUP BY p.user_id \
             ORDER BY 3 DESC, p.user_id",
        )
        .map_err(|e| format!("查询账号清单失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| format!("查询账号清单失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取账号清单失败: {e}"))?;
    let accounts: Vec<Value> = rows
        .into_iter()
        .map(|(uid, projects, sessions)| {
            json!({
                "uid": uid,
                "label": crate::modules::trae_vault::account_label(client_key, &uid),
                "projects": projects,
                "sessions": sessions,
            })
        })
        .collect();
    Ok(json!({ "clientKey": client_key, "accounts": accounts }))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

/// 生成 `n` 个新的 24 位 hex id（T46：前端「新增回合」/「复制消息」时要用）。
///
/// ⚠️ **id 生成只有这一处实现**（`trae_import::native_hex_id`）。
/// 前端自己造一串随机 hex 看着也行，但那样就有了两套生成规则 ——
/// 一旦形态（长度 / 字符集）需要调整，必然只改一处而漏另一处，
/// 而「id 形态不对」是「写完客户端打不开」这类最难查的问题。
///
/// 上限 512：一次「批量复制」最多用到几十个，给个硬顶以防参数被误传成天文数字。
pub fn new_ids(n: usize) -> Value {
    let n = n.min(512);
    json!(
        (0..n)
            .map(|_| native_hex_id())
            .collect::<Vec<String>>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(sid: &str, tables: Value) -> Value {
        json!({ "clientKey": "trae-cn", "sessionId": sid, "tables": tables })
    }

    fn simple_doc() -> Value {
        doc_with(
            "68f0a1b2c3d4e5f6a7b8c9d0",
            json!([
                {
                    "name": "chat_session",
                    "scope": "session",
                    "columns": [
                        { "name": "session_id", "type": "TEXT", "notNull": true, "pk": true, "json": false, "readOnly": false },
                        { "name": "session_title", "type": "TEXT", "notNull": false, "pk": false, "json": false, "readOnly": false },
                        { "name": "context", "type": "TEXT", "notNull": false, "pk": false, "json": true, "readOnly": false }
                    ],
                    "rows": [
                        { "session_id": "68f0a1b2c3d4e5f6a7b8c9d0", "session_title": "标题", "context": { "last_real_project_id": "123" } }
                    ]
                },
                {
                    "name": "chat_message",
                    "scope": "session",
                    "columns": [
                        { "name": "message_id", "type": "TEXT", "notNull": true, "pk": true, "json": false, "readOnly": false },
                        { "name": "session_id", "type": "TEXT", "notNull": true, "pk": false, "json": false, "readOnly": false },
                        { "name": "message_index", "type": "INTEGER", "notNull": false, "pk": false, "json": false, "readOnly": false }
                    ],
                    "rows": [
                        { "message_id": "aaaaaaaaaaaaaaaaaaaaaaaa", "session_id": "68f0a1b2c3d4e5f6a7b8c9d0", "message_index": 0 }
                    ]
                }
            ]),
        )
    }

    #[test]
    fn blob_roundtrips_via_marker() {
        let bytes = vec![0u8, 1u8, 254u8, 255u8];
        let j = cell_to_json(&Cell::Blob(bytes.clone()), false);
        assert!(j["$blob"].is_string());
        let back = json_to_sql(&j, false);
        match back {
            rusqlite::types::Value::Blob(b) => assert_eq!(b, bytes),
            other => panic!("期望 Blob，得到 {other:?}"),
        }
    }

    #[test]
    fn json_columns_are_detected_by_name_hint_and_by_sampling() {
        let cols = vec![
            ColMeta { name: "content".into(), decl: "TEXT".into(), notnull: false, pk: false, dflt: None },
            ColMeta { name: "plain".into(), decl: "TEXT".into(), notnull: false, pk: false, dflt: None },
            ColMeta { name: "kind".into(), decl: "INTEGER".into(), notnull: false, pk: false, dflt: None },
        ];
        let rows = vec![
            vec![Cell::Text("[{\"type\":\"text\"}]".into()), Cell::Text("[]".into()), Cell::Int(1)],
            vec![Cell::Text("{\"a\":1}".into()), Cell::Text("[\"x\"]".into()), Cell::Int(2)],
        ];
        let flags = detect_json_columns(&rows, &cols);
        assert!(flags[0], "content 应按列名命中");
        assert!(flags[1], "plain 应按抽样命中（两行都是 JSON）");
        assert!(!flags[2], "INTEGER 列不该判成 JSON");
    }

    #[test]
    fn json_text_is_decoded_to_a_real_tree_and_back() {
        let c = Cell::Text("[{\"type\":\"text\",\"text_content\":\"问题\"}]".into());
        let j = cell_to_json(&c, true);
        assert!(j.is_array(), "JSON 列应解成数组");
        assert_eq!(j[0]["text_content"], json!("问题"));
        // 写回：数组 → JSON 文本
        match json_to_sql(&j, true) {
            rusqlite::types::Value::Text(s) => {
                let parsed: Value = serde_json::from_str(&s).unwrap();
                assert_eq!(parsed[0]["type"], json!("text"));
            }
            other => panic!("期望 Text，得到 {other:?}"),
        }
    }

    #[test]
    fn validate_passes_on_a_well_formed_doc() {
        let r = validate(&simple_doc());
        assert_eq!(r["ok"], json!(true), "{r}");
        assert!(r["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn validate_hard_blocks_bad_session_id() {
        let d = doc_with("NOT-HEX", json!([]));
        let r = validate(&d);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"][0].as_str().unwrap().contains("格式不正确"));
    }

    /// 真机取证（2026-10-10）：把会话的正文删光、只留 `chat_session` 那一行 ⇒
    /// 客户端打开它时卡在「环境创建中」，**发不出任何消息**。必须硬拦。
    #[test]
    fn validate_hard_blocks_the_empty_session_shell() {
        let mut d = simple_doc();
        // 清空 chat_message（doc 里本来就没有 chat_turn）⇒ 0 回合 / 0 消息
        d["tables"][1]["rows"] = json!([]);
        let r = validate(&d);
        assert_eq!(r["ok"], json!(false), "{r}");
        assert!(
            r["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e.as_str().unwrap().contains("会话不能是空的")),
            "{r}"
        );
    }

    /// 反向：只加载了会话元数据（两张正文表都没进 doc）时**不许误报** ——
    /// 否则「只改个标题」会被自己的校验拦住。
    #[test]
    fn validate_keeps_quiet_when_the_body_tables_are_not_loaded() {
        let mut d = simple_doc();
        d["tables"]
            .as_array_mut()
            .unwrap()
            .retain(|t| t["name"] != "chat_message");
        let r = validate(&d);
        assert_eq!(r["ok"], json!(true), "{r}");
    }

    /// 反向：有一行正文就不算空（哪怕 `chat_turn` 一行没有）。
    #[test]
    fn validate_allows_a_session_that_still_has_messages() {
        let r = validate(&simple_doc());
        assert_eq!(r["ok"], json!(true), "{r}");
    }

    #[test]
    fn validate_hard_blocks_mismatched_session_id_and_duplicate_pk() {
        let mut d = simple_doc();
        d["tables"][1]["rows"][0]["session_id"] = json!("ffffffffffffffffffffffff");
        let r = validate(&d);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("不一致")));

        let mut d2 = simple_doc();
        let row = d2["tables"][1]["rows"][0].clone();
        d2["tables"][1]["rows"].as_array_mut().unwrap().push(row);
        let r2 = validate(&d2);
        assert!(r2["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("主键重复")));
    }

    #[test]
    fn validate_hard_blocks_orphan_message_id() {
        let mut d = simple_doc();
        d["tables"].as_array_mut().unwrap().push(json!({
            "name": "chat_message_general",
            "scope": "message",
            "columns": [
                { "name": "message_id", "type": "TEXT", "notNull": true, "pk": true, "json": false, "readOnly": false },
                { "name": "content", "type": "TEXT", "notNull": false, "pk": false, "json": true, "readOnly": false }
            ],
            "rows": [ { "message_id": "bbbbbbbbbbbbbbbbbbbbbbbb", "content": [{"type":"text"}] } ]
        }));
        let r = validate(&d);
        assert_eq!(r["ok"], json!(false));
        assert!(r["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("找不到对应行")));
    }

    #[test]
    fn validate_warns_but_allows_when_finish_node_is_missing() {
        let mut d = simple_doc();
        d["tables"].as_array_mut().unwrap().push(json!({
            "name": "chat_message_task",
            "scope": "message",
            "columns": [
                { "name": "message_id", "type": "TEXT", "notNull": true, "pk": true, "json": false, "readOnly": false },
                { "name": "content", "type": "TEXT", "notNull": false, "pk": false, "json": false, "readOnly": false }
            ],
            "rows": [ { "message_id": "aaaaaaaaaaaaaaaaaaaaaaaa", "content": "{\"messages\":[]}" } ]
        }));
        let r = validate(&d);
        assert_eq!(r["ok"], json!(true), "只有告警时不该拦：{r}");
        assert!(r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("finish")));
    }

    #[test]
    fn insert_mode_regenerates_session_and_message_ids_and_rewrites_references() {
        let d = simple_doc();
        // 源会话已存在 ⇒ 必须换 id
        let mut existing = BTreeSet::new();
        existing.insert("68f0a1b2c3d4e5f6a7b8c9d0".to_string());
        let (sid, map) = plan_id_map(&d, &existing);
        assert_ne!(sid, "68f0a1b2c3d4e5f6a7b8c9d0", "同库新增必须换 session_id");
        assert_eq!(map.len(), 2, "session + message 两个 id 都要换");
        let out = apply_id_map(&d, &sid, &map);
        assert_eq!(out["sessionId"], json!(sid));
        assert_eq!(out["tables"][1]["rows"][0]["session_id"], json!(sid));
        assert_ne!(out["tables"][1]["rows"][0]["message_id"], json!("aaaaaaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn insert_mode_keeps_ids_when_session_is_brand_new() {
        let d = simple_doc();
        let (sid, _map) = plan_id_map(&d, &BTreeSet::new());
        assert_eq!(sid, "68f0a1b2c3d4e5f6a7b8c9d0", "库里没有这条会话就不必换 id");
    }

    #[test]
    fn reference_ids_are_rewritten_inside_nested_json() {
        let mut map = HashMap::new();
        map.insert("aaaaaaaaaaaaaaaaaaaaaaaa".to_string(), "c".repeat(24));
        let v = json!({ "reply_to_message_id": "aaaaaaaaaaaaaaaaaaaaaaaa",
                        "params": { "refs": ["aaaaaaaaaaaaaaaaaaaaaaaa", "keep-me"] } });
        let out = rewrite_value(&v, &map);
        assert_eq!(out["reply_to_message_id"], json!("c".repeat(24)));
        assert_eq!(out["params"]["refs"][0], json!("c".repeat(24)));
        assert_eq!(out["params"]["refs"][1], json!("keep-me"), "非 id 值不许动");
    }

    #[test]
    fn project_id_is_never_remapped() {
        let d = json!({
            "clientKey": "trae-cn",
            "sessionId": "68f0a1b2c3d4e5f6a7b8c9d0",
            "tables": [{
                "name": "chat_session",
                "columns": [
                    { "name": "session_id", "type": "TEXT", "pk": true, "readOnly": false, "json": false },
                    // project_id 在 session_project 的 UNIQUE(project_id, session_id) 里，
                    // 但它是**账号**维度：唯一的身份列规则也得给它让路。
                    { "name": "project_id", "type": "TEXT", "pk": false, "unique": true, "readOnly": false, "json": false }
                ],
                "rows": [{ "session_id": "68f0a1b2c3d4e5f6a7b8c9d0", "project_id": "1234567890abcdef12345678" }]
            }]
        });
        let mut existing = BTreeSet::new();
        existing.insert("68f0a1b2c3d4e5f6a7b8c9d0".to_string());
        let (sid, map) = plan_id_map(&d, &existing);
        assert!(!map.contains_key("1234567890abcdef12345678"), "project_id 属于账号，不能重映射");
        let out = apply_id_map(&d, &sid, &map);
        assert_eq!(out["tables"][0]["rows"][0]["project_id"], json!("1234567890abcdef12345678"));
    }

    // -----------------------------------------------------------------------
    // 大字段引用（$jsonRef）
    // -----------------------------------------------------------------------

    const BIG_SID: &str = "68f0a1b2c3d4e5f6a7b8c9d0";

    /// 造一个真库：一张会话表 + 一张大 content 的消息表（一行超阈值、一行很小）。
    fn conn_with_big_cell(big: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chat_session (id INTEGER PRIMARY KEY, session_id TEXT, session_title TEXT);
             CREATE TABLE chat_message (id INTEGER PRIMARY KEY, session_id TEXT, message_id TEXT, message_index INTEGER);
             CREATE TABLE chat_message_task (id INTEGER PRIMARY KEY, message_id TEXT, content TEXT);
             CREATE UNIQUE INDEX chat_message_mid ON chat_message(message_id);
             CREATE UNIQUE INDEX chat_message_task_mid ON chat_message_task(message_id);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chat_session (session_id, session_title) VALUES (?1, '题目')",
            [BIG_SID],
        )
        .unwrap();
        for (i, mid) in ["a".repeat(24), "b".repeat(24)].iter().enumerate() {
            conn.execute(
                "INSERT INTO chat_message (session_id, message_id, message_index) VALUES (?1, ?2, ?3)",
                params![BIG_SID, mid, i as i64],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO chat_message_task (message_id, content) VALUES (?1, ?2)",
            params!["a".repeat(24), big],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chat_message_task (message_id, content) VALUES (?1, ?2)",
            params!["b".repeat(24), "[{\"k\":1}]"],
        )
        .unwrap();
        conn
    }

    #[test]
    fn big_json_cells_become_refs_while_small_ones_stay_inline() {
        let big = format!("[{}]", "\"xxxxxxxxxx\",".repeat(4000)); // ≈ 44 KB，超 32 KB 阈值
        assert!(big.len() > BIG_CELL_BYTES);
        let conn = conn_with_big_cell(&big);
        let t = build_table(&conn, "chat_message_task", "message", BIG_SID).unwrap();

        let r0 = &t["rows"][0]["content"];
        assert!(is_json_ref(r0), "超阈值的大字段应折叠成引用，实际：{r0}");
        assert_eq!(r0["$jsonRef"]["pk"], json!(1), "引用必须带上原行主键");
        assert_eq!(r0["$jsonRef"]["bytes"], json!(big.len()));
        assert!(r0["$jsonRef"]["preview"].as_str().unwrap().starts_with("[\"xxx"));
        assert_eq!(t["refCount"], json!(1), "只有一行超阈值");

        let r1 = &t["rows"][1]["content"];
        assert!(r1.is_array(), "小字段要原样解成树，方便直接改：{r1}");
    }

    #[test]
    fn refs_are_resolved_back_to_the_original_text_on_write() {
        let big = format!("[{}]", "\"yyyyyyyyyy\",".repeat(4000));
        let conn = conn_with_big_cell(&big);
        let t = build_table(&conn, "chat_message_task", "message", BIG_SID).unwrap();

        let resolved = resolve_rows(&conn, &t).unwrap();
        assert_eq!(
            resolved.cols,
            vec!["message_id".to_string(), "content".to_string()],
            "自增主键 id 是 readOnly，不该进 INSERT 列"
        );
        assert_eq!(resolved.rows.len(), 2);
        // 第 1 行的 content 必须还原成**原文**，绝不能把引用对象本身写进库
        match &resolved.rows[0][1] {
            rusqlite::types::Value::Text(s) => assert_eq!(s, &big),
            other => panic!("期望还原成原文，得到 {other:?}"),
        }
        // 第 2 行是内联的小 JSON，序列化回去
        match &resolved.rows[1][1] {
            rusqlite::types::Value::Text(s) => assert!(s.contains("\"k\"")),
            other => panic!("期望 Text，得到 {other:?}"),
        }
    }

    #[test]
    fn refs_survive_id_remap_untouched() {
        let mut map = HashMap::new();
        map.insert("aaaaaaaaaaaaaaaaaaaaaaaa".to_string(), "c".repeat(24));
        let v = json!({
            "message_id": "aaaaaaaaaaaaaaaaaaaaaaaa",
            "content": { "$jsonRef": { "pk": "aaaaaaaaaaaaaaaaaaaaaaaa", "bytes": 99 } }
        });
        let out = rewrite_value(&v, &map);
        assert_eq!(out["message_id"], json!("c".repeat(24)), "普通引用型 id 要改");
        assert_eq!(
            out["content"]["$jsonRef"]["pk"],
            json!("aaaaaaaaaaaaaaaaaaaaaaaa"),
            "引用里的 pk 必须保持原值，否则回查不到原行"
        );
    }

    #[test]
    fn resolve_ref_reports_a_clear_error_when_the_row_is_gone() {
        let big = format!("[{}]", "\"z\",".repeat(20000));
        let conn = conn_with_big_cell(&big);
        let t = build_table(&conn, "chat_message_task", "message", BIG_SID).unwrap();
        conn.execute("DELETE FROM chat_message_task WHERE id = 1", []).unwrap();
        let e = resolve_rows(&conn, &t).unwrap_err();
        assert!(e.contains("已不存在"), "错误要能看懂，实际：{e}");
    }

    #[test]
    fn validate_tolerates_ref_cells() {
        let mut d = simple_doc();
        d["tables"].as_array_mut().unwrap().push(json!({
            "name": "chat_message_task",
            "scope": "message",
            "columns": [
                { "name": "message_id", "type": "TEXT", "notNull": true, "pk": true, "json": false, "readOnly": false },
                { "name": "content", "type": "TEXT", "notNull": false, "pk": false, "json": true, "readOnly": false }
            ],
            "rows": [{
                "message_id": "aaaaaaaaaaaaaaaaaaaaaaaa",
                "content": { "$jsonRef": { "pk": 1, "bytes": 9783679, "preview": "[{\"task_id\"" } }
            }]
        }));
        let r = validate(&d);
        assert_eq!(r["ok"], json!(true), "引用单元格不该被当成类型错误：{r}");
        assert!(r["errors"].as_array().unwrap().is_empty());
        // 也不该因为「看不到 finish」就报警 —— 内容根本没加载
        assert!(!r["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("finish")));
    }

    /// 真机上 `chat_message` 的主键是自增 `id`，真正唯一的是带 UNIQUE 索引的 `message_id`。
    /// 只按主键判身份列 ⇒ 新增会话时直接 `UNIQUE constraint failed: chat_message.message_id`
    /// （实测踩到过，写库事务整趟回滚）。
    #[test]
    fn unique_text_columns_count_as_identity_columns() {
        let conn = conn_with_big_cell("[{\"k\":1}]");
        let t = build_table(&conn, "chat_message", "session", BIG_SID).unwrap();
        let col = |n: &str| {
            t["columns"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == json!(n))
                .cloned()
                .unwrap()
        };
        let mid = col("message_id");
        assert_eq!(mid["unique"], json!(true), "message_id 上有唯一索引");
        assert_eq!(mid["pk"], json!(false), "但它不是主键 —— 这正是漏判的原因");
        assert!(is_id_defining("message_id", "TEXT", false, true), "唯一索引上的 TEXT 列是身份列");
        assert!(
            !is_id_defining("message_id", "TEXT", false, false),
            "没有唯一索引就不该换（那只是普通引用列）"
        );
        // INTEGER 唯一列不换：它是自增主键，交给 SQLite 分配
        assert!(!is_id_defining("id", "INTEGER", true, true));
    }

    /// 计划里真的会给 `message_id` 生成新值 —— 不只是分类对。
    #[test]
    fn insert_plan_regenerates_unique_message_ids_not_just_session_id() {
        let conn = conn_with_big_cell("[{\"k\":1}]");
        let mut tables = Vec::new();
        for (name, scope) in [
            ("chat_session", "session"),
            ("chat_message", "session"),
            ("chat_message_task", "message"),
        ] {
            tables.push(build_table(&conn, name, scope, BIG_SID).unwrap());
        }
        let doc = json!({
            "clientKey": "trae-cn",
            "sessionId": BIG_SID,
            "tables": tables,
        });
        let mut existing = BTreeSet::new();
        existing.insert(BIG_SID.to_string());
        let (sid, map) = plan_id_map(&doc, &existing);
        assert_ne!(sid, BIG_SID);
        assert!(map.contains_key(&"a".repeat(24)), "message_id 必须换新值");
        assert!(map.contains_key(&"b".repeat(24)), "每一条 message_id 都要换");
        assert_eq!(map.len(), 3, "session + 2 个 message_id，没有多余的重映射");
    }

    /// S2 门禁：目标账号只在「新增」时可用。
    ///
    /// 这几条都必须**在碰库之前**就被拒 —— 一旦走到开库/解密才发现参数不合法，
    /// 就白解一次 400 MB。所以断言只看错误信息，不依赖任何真实数据。
    #[test]
    fn cross_account_target_is_only_allowed_for_insert() {
        let doc = json!({});

        // 覆写 + 目标账号 ⇒ 拒（覆写是「就地改这一条」，语义绑在原账号上）
        let e = edit_save(&doc, "overwrite", None, Some("842681600717036"), None).unwrap_err();
        assert!(e.contains("新增"), "错误信息要指明改用「新增」：{e}");

        // 空白 uid ⇒ 拒
        let e = edit_save(&doc, "insert", None, Some("   "), None).unwrap_err();
        assert!(e.contains("不能为空"), "{e}");

        // 未知写入方式 ⇒ 拒
        let e = edit_save(&doc, "merge", None, None, None).unwrap_err();
        assert!(e.contains("未知写入方式"), "{e}");
    }

    /// 源码级护栏（T46 真机实测到的「检索词条翻倍」）：
    /// `fts_*` 是客户端用触发器自动维护的 FTS5 虚表 —— 我们插 `chat_session` /
    /// `chat_message_general` 时触发器已经补了一份不完整的词条，再照抄源库的行就会翻倍
    /// （实测 `fts_session_title` 1 行变 2 行）。所以写入阶段必须
    /// **①分两趟、fts_* 排最后；②写之前按目标会话 DELETE 一遍**。
    /// 少任何一半都会让检索结果出现重复或幽灵条目，而会话正文看起来完全正常。
    #[test]
    fn fts_mirror_rows_are_cleared_before_being_rewritten() {
        let src = include_str!("trae_session_edit.rs");
        let pass = src
            .find("for pass_fts in [false, true]")
            .expect("fts_* 必须走「最后写」的两趟");
        let del = src
            .find(r#"DELETE FROM \"{}\" WHERE session_id = ?"#)
            .expect("写入 fts_* 之前必须按目标会话清掉触发器补的半成品");
        let ins = src.find("match exec_insert(&tx, r)").expect("找不到插入调用");
        assert!(pass < del, "清理必须发生在 fts 那一趟里");
        assert!(del < ins, "顺序必须是：清词条 → 插入");
    }

    #[test]
    fn ref_key_column_is_only_an_integer_primary_key() {
        let text_pk = vec![ColMeta {
            name: "message_id".into(),
            decl: "TEXT".into(),
            notnull: true,
            pk: true,
            dflt: None,
        }];
        assert!(ref_key_col(&text_pk).is_none(), "TEXT 主键不做回查键");
        let int_pk = vec![ColMeta {
            name: "id".into(),
            decl: "INTEGER".into(),
            notnull: true,
            pk: true,
            dflt: None,
        }];
        assert_eq!(ref_key_col(&int_pk), Some(0));
        // fts_* 表没有主键 ⇒ 不产生引用
        assert!(ref_key_col(&[]).is_none());
    }

    #[test]
    fn validate_never_panics_on_garbage() {
        for junk in [
            json!(null),
            json!(1),
            json!("x"),
            json!([]),
            json!({}),
            json!({ "tables": [null, 1, {}] }),
            json!({ "sessionId": 3, "tables": [{ "rows": "nope" }] }),
        ] {
            let r = validate(&junk);
            assert!(r["errors"].is_array() && r["warnings"].is_array());
        }
    }

    /// T46：新增的行**没有自增主键**（`id` 由 SQLite 在 INSERT 时分配，`readOnly` 列
    /// 既不显示为可改也不参与写回），校验绝不能把它当成「重复的 null」。
    ///
    /// 不修的话症状很隐蔽：加 1 行没事，加第 2 行起必然报「主键重复」——
    /// 也就是「新增回合 / 新增消息」这个功能会被自己的校验彻底拦住，
    /// 而错误信息（主键重复）完全指不到真正的原因。
    #[test]
    fn rows_without_the_autoincrement_pk_are_not_reported_as_duplicates() {
        let doc = doc_with(
            "68f0a1b2c3d4e5f6a7b8c9d0",
            json!([{
                "name": "chat_turn",
                "scope": "session",
                "columns": [
                    { "name": "id", "type": "INTEGER", "notNull": true, "pk": true, "json": false, "readOnly": true },
                    { "name": "turn_id", "type": "TEXT", "notNull": true, "pk": false, "json": false, "readOnly": false, "unique": true },
                    { "name": "session_id", "type": "TEXT", "notNull": true, "pk": false, "json": false, "readOnly": false }
                ],
                "rows": [
                    { "turn_id": "111111111111111111111111", "session_id": "68f0a1b2c3d4e5f6a7b8c9d0" },
                    { "turn_id": "222222222222222222222222", "session_id": "68f0a1b2c3d4e5f6a7b8c9d0" },
                    { "turn_id": "333333333333333333333333", "session_id": "68f0a1b2c3d4e5f6a7b8c9d0" }
                ]
            }]),
        );
        let r = validate(&doc);
        let errs: Vec<String> = r["errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        assert!(!errs.iter().any(|e| e.contains("主键重复")), "{errs:?}");
        assert_eq!(r["ok"], json!(true), "{r}");
    }

    /// 身份列（UNIQUE 索引列）重复必须在**碰库之前**被拦下。真到写的时候才发现，
    /// 代价是「已解密整库 + 已结束客户端进程」，而且错误是 SQLite 抛的英文约束名。
    /// 这条同时是「新增回合 / 复制消息自动换新 id」的护栏：映射漏了哪一列，这里就会响。
    #[test]
    fn duplicate_identity_column_is_caught_before_writing() {
        let doc = doc_with(
            "68f0a1b2c3d4e5f6a7b8c9d0",
            json!([{
                "name": "chat_message",
                "scope": "session",
                "columns": [
                    { "name": "id", "type": "INTEGER", "notNull": true, "pk": true, "json": false, "readOnly": true },
                    { "name": "message_id", "type": "TEXT", "notNull": true, "pk": false, "json": false, "readOnly": false, "unique": true }
                ],
                "rows": [
                    { "id": 1, "message_id": "aaaaaaaaaaaaaaaaaaaaaaaa" },
                    { "id": 2, "message_id": "aaaaaaaaaaaaaaaaaaaaaaaa" }
                ]
            }]),
        );
        let r = validate(&doc);
        let errs: Vec<String> = r["errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        assert!(
            errs.iter()
                .any(|e| e.contains("message_id") && e.contains("唯一索引")),
            "{errs:?}"
        );
        assert_eq!(r["ok"], json!(false));
        // `id` 是自增主键，两行都给了值且不同 ⇒ 不该报主键重复
        assert!(!errs.iter().any(|e| e.contains("主键重复")), "{errs:?}");
    }

    #[test]
    fn new_ids_are_24_hex_digits_and_unique() {
        let v = new_ids(50);
        let a = v.as_array().expect("返回数组");
        assert_eq!(a.len(), 50);
        let set: BTreeSet<String> = a
            .iter()
            .map(|x| x.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(set.len(), 50, "同一批生成的 id 不应重复");
        for s in &set {
            assert_eq!(s.len(), 24, "id 长度必须是 24：{s}");
            assert!(s.chars().all(|c| c.is_ascii_hexdigit()), "id 必须是 hex：{s}");
        }
        // 上限保护
        assert_eq!(new_ids(9999).as_array().unwrap().len(), 512);
        assert_eq!(new_ids(0).as_array().unwrap().len(), 0);
    }
}
