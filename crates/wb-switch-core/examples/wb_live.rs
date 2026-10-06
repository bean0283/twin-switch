//! 真机只读探针：dump Trae **实时库（解密快照 + 未 checkpoint 的 WAL）** 里某个会话的
//! 当前真实状态。
//!
//! 为什么要它：Trae 的库跑在 WAL 模式，**客户端运行时的写入全在 WAL 里**，主库文件的
//! mtime 不变。所以只看 `decrypted/<key>.db`（纯解密快照）会读到「上次 checkpoint 时」
//! 的旧状态，必须走 `reader_plain_path()`（快照副本 + 合并 WAL）才等于客户端真正读到的数据。
//!
//! **全程只读**：只打开 `reader_plain_path()` 返回的文件，不触碰实时库。
//!
//! ```bash
//! cargo run -p wb-switch-core --example wb_live -- solo-cn 6ac22cadf92b521cbac24f51
//! ```

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::BTreeSet;

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let sid = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "6ac22cadf92b521cbac24f51".into());

    let path = match wb_switch_core::modules::trae_export::reader_plain_path(&client) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("读取视图失败：{e}");
            return;
        }
    };
    println!("实时视图：{}", path.display());
    let c = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    // 会话概要
    let row: Option<(String, i64, i64)> = c
        .query_row(
            "SELECT session_title, created_at, updated_at FROM chat_session WHERE session_id=?1",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match row {
        Some((t, cr, up)) => println!("会话：{sid}\n  标题={t}\n  created={cr} updated={up}"),
        None => {
            println!("会话 {sid} 不存在");
            return;
        }
    }

    // 消息清单
    let mut st = c
        .prepare(
            "SELECT message_id, message_role, message_index, message_type, created_at
             FROM chat_message WHERE session_id=?1 ORDER BY message_index",
        )
        .unwrap();
    let rows: Vec<(String, String, i64, String, i64)> = st
        .query_map([&sid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    println!("\n消息总数：{}", rows.len());
    let (mut nu, mut na) = (0, 0);
    for (_, role, _, _, _) in &rows {
        if role == "user" {
            nu += 1
        } else {
            na += 1
        }
    }
    println!("  user={nu} assistant={na}");

    // 逐条 assistant：plan_item 字段形态
    println!("\n--- assistant 回合明细（尾部 12 条）---");
    let mut keys_union: BTreeSet<String> = BTreeSet::new();
    let mut types_seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut tail = Vec::new();
    for (mid, role, idx, _mt, _ca) in &rows {
        if role != "assistant" {
            continue;
        }
        let content: String = match c.query_row(
            "SELECT content FROM chat_message_task WHERE message_id=?1",
            [mid],
            |r| r.get(0),
        ) {
            Ok(v) => v,
            Err(_) => {
                tail.push(format!("  idx={idx} {mid} <无 chat_message_task 行>"));
                continue;
            }
        };
        let o: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                tail.push(format!("  idx={idx} {mid} <content 非 JSON：{e}>"));
                continue;
            }
        };
        let ms = o.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut n_tool = 0usize;
        let mut bad = Vec::new();
        for m in &ms {
            let pi = m.get("plan_item").unwrap_or(&Value::Null);
            if pi.is_null() {
                continue;
            }
            walk(pi, "", &mut keys_union, &mut types_seen);
            let tci = pi.get("tool_call_info").unwrap_or(&Value::Null);
            if tci.is_null() {
                continue;
            }
            n_tool += 1;
            if !tci.get("params").map(|p| p.is_object()).unwrap_or(false) {
                bad.push("params 非对象".to_string());
            }
            for k in ["already_emitted_generating_event", "already_emitted_run_event"] {
                if tci.get(k).is_none() {
                    bad.push(format!("缺 {k}"));
                }
            }
            let res = tci.get("result").unwrap_or(&Value::Null);
            for k in ["status", "error_message", "data", "render", "is_truncated", "interrupt", "images"] {
                if res.get(k).is_none() {
                    bad.push(format!("result 缺 {k}"));
                }
            }
        }
        bad.sort();
        bad.dedup();
        let note = if bad.is_empty() { "OK".to_string() } else { bad.join(" / ") };
        tail.push(format!(
            "  idx={idx} items={} tools={} -> {}",
            ms.len(),
            n_tool,
            note
        ));
    }
    for line in tail.iter().rev().take(12).rev() {
        println!("{line}");
    }

    println!("\n--- plan_item 全路径集合（判定 Trae 能否解析）---");
    for (p, t) in types_seen.iter() {
        println!("  {p} :: {t}");
    }

    // 其他关联表
    println!("\n--- 关联表行数 ---");
    for t in [
        "chat_turn",
        "task",
        "history_v2",
        "agent_run",
        "fts_message_content",
        "chat_message_general",
        "chat_message_task",
    ] {
        let n: i64 = if t.starts_with("chat_message_") {
            c.query_row(
                &format!(
                    "SELECT count(*) FROM {t} WHERE message_id IN (SELECT message_id FROM chat_message WHERE session_id=?1)"
                ),
                [&sid],
                |r| r.get(0),
            )
            .unwrap_or(-1)
        } else {
            c.query_row(
                &format!("SELECT count(*) FROM {t} WHERE session_id=?1"),
                [&sid],
                |r| r.get(0),
            )
            .unwrap_or(-1)
        };
        println!("  {t}: {n}");
    }
}

/// 收集某节点的全部 (路径, 类型) 组合。
fn walk(v: &Value, pre: &str, keys: &mut BTreeSet<String>, types: &mut BTreeSet<(String, String)>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                let p = if pre.is_empty() {
                    k.clone()
                } else {
                    format!("{pre}.{k}")
                };
                keys.insert(p.clone());
                types.insert((p.clone(), kind(val).to_string()));
                walk(val, &p, keys, types);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                walk(item, &format!("{pre}[]"), keys, types);
            }
        }
        _ => {}
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
