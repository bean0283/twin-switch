//! 只读比对探针：把 **WorkBuddy 源会话** 与 **Trae 已导入结果** 逐条对齐，
//! 找出「Trae 里看到的」与「WorkBuddy 里原本的」不一致的地方。
//!
//! 为什么需要它：v0.0.12 修掉了「卡片卡在进行中 / 详细过程为空」，
//! 但用户反馈「之前还是有部分记录显示与 WorkBuddy 不一致」——
//! 这类问题必须**两边都打印出来对着看**，不能靠猜。
//!
//! **全程只读**：Trae 走 `reader_plain_path()`（快照 + WAL 合并），WorkBuddy 走
//! `load_body()`（纯读 jsonl）。两边都不写。
//!
//! ```bash
//! cargo run -p wb-switch-core --example wb_diff -- \
//!     solo-cn 6ac22cadf92b521cbac24f51 f92b521c-bac2-4f51-9ba6-880e08384eb7
//! ```

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use wb_switch_core::modules::workbuddy_source;

/// 去掉多余空白并截断，便于并排打印。
fn cut(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let t: String = flat.chars().take(n).collect();
        format!("{t}…")
    }
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let sid = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "6ac22cadf92b521cbac24f51".into());
    let wb_id = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "f92b521c-bac2-4f51-9ba6-880e08384eb7".into());

    let path = match wb_switch_core::modules::trae_export::reader_plain_path(&client) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("读取视图失败：{e}");
            return;
        }
    };
    let c = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    // ---------------------------------------------------------------- 源侧
    let body = match workbuddy_source::load_body(&wb_id) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("读源会话失败：{e}");
            return;
        }
    };
    println!("════════ 源侧 WorkBuddy ════════");
    println!("会话 {wb_id}");
    println!("  ai 标题 = {:?}", body.ai_title);
    println!("  回合数   = {}", body.turns.len());
    let src_turns = &body.turns;

    // 源侧：每个回合的构成
    for (i, t) in src_turns.iter().enumerate() {
        let calls: Vec<&str> = t
            .events
            .iter()
            .filter(|e| e.kind == "function_call")
            .map(|e| e.name.as_str())
            .collect();
        let results = t
            .events
            .iter()
            .filter(|e| e.kind == "function_call_result")
            .count();
        let narrations = t
            .events
            .iter()
            .filter(|e| e.kind == "message")
            .count();
        let thinks = t.events.iter().filter(|e| e.kind == "reasoning").count();
        println!(
            "\n  [源 {i:>3}] 用户: {}\n          助手: {}\n          事件: msg={narrations} 思考={thinks} 调用={} 结果={results}",
            cut(&t.user_text, 46),
            cut(&t.assistant_text, 46),
            calls.len()
        );
        if !calls.is_empty() {
            println!("          工具: {}", calls.join(" → "));
        }
    }

    // ---------------------------------------------------------------- Trae 侧
    println!("\n════════ 目标侧 Trae ════════");
    let row: Option<(String, i64, i64)> = c
        .query_row(
            "SELECT session_title, created_at, updated_at FROM chat_session WHERE session_id=?1",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match &row {
        Some((t, cr, up)) => println!("会话 {sid}\n  标题={t}\n  created={cr} updated={up}"),
        None => {
            println!("会话 {sid} 不存在");
            return;
        }
    }

    let mut st = c
        .prepare(
            "SELECT message_id, message_role, message_index, message_type, created_at, ifnull(deleted_at,0)
             FROM chat_message WHERE session_id=?1 ORDER BY message_index",
        )
        .unwrap();
    let rows: Vec<(String, String, i64, String, i64, i64)> = st
        .query_map([&sid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    println!("\n  消息总数 = {}（含 deleted）", rows.len());

    let mut n_ur = 0usize;
    let mut n_as = 0usize;
    let mut n_del = 0usize;
    for (_, role, _, _, _, del) in &rows {
        if *del != 0 {
            n_del += 1;
        }
        if role == "user" {
            n_ur += 1
        } else {
            n_as += 1
        }
    }
    println!("  user={n_ur} assistant={n_as} deleted={n_del}");

    // 逐条明细
    let mut asst_idx = 0usize;
    for (mid, role, idx, mtype, ca, del) in &rows {
        let flag = if *del != 0 { " [已删]" } else { "" };
        if role == "user" {
            let raw: String = c
                .query_row(
                    "SELECT content FROM chat_message_general WHERE message_id=?1",
                    [mid],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            println!(
                "\n  [目标 {idx:>3}] user{flag} type={mtype} ts={ca}\n          文本: {}",
                cut(&general_text(&raw), 90)
            );
        } else {
            let raw: String = c
                .query_row(
                    "SELECT content FROM chat_message_task WHERE message_id=?1",
                    [mid],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            let (items, tools, finish) = task_digest(&raw);
            let hist = history_text(&c, mid);
            println!(
                "\n  [目标 {idx:>3}] assistant#{asst_idx}{flag} type={mtype} ts={ca}\n          过程项={items} 工具={tools}\n          history_v2: {}\n          finish: {}",
                cut(&hist, 70),
                cut(&finish, 70)
            );
            asst_idx += 1;
        }
    }

    // ---------------------------------------------------------------- 差异
    println!("\n════════ 差异汇总 ════════");
    println!("  源回合数        = {}", src_turns.len());
    println!("  目标 user 条数  = {n_ur}");
    println!("  目标 asst 条数  = {n_as}");
    if src_turns.len() != n_ur {
        println!("  ⚠️ user 消息数不一致：源 {} vs 目标 {n_ur}", src_turns.len());
    }
    if src_turns.len() != n_as {
        println!("  ⚠️ 助手回合数不一致：源 {} vs 目标 {n_as}", src_turns.len());
    }

    // 逐回合文本比对
    let mut si = 0usize;
    for (i, t) in src_turns.iter().enumerate() {
        if i >= n_ur || i >= n_as {
            break;
        }
        let _ = t;
        si += 1;
    }
    let _ = si;
    println!("  （逐条文本形状见上方明细）");
}

/// 取 Trae `chat_message_general.content` 里的用户文本。
fn general_text(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    for key in ["content", "text", "message", "display_text"] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    raw.to_string()
}

/// 取助手 `history_v2.messages` 里的文本。
fn history_text(c: &Connection, mid: &str) -> String {
    let rows: Vec<String> = c
        .prepare(
            "SELECT messages FROM history_v2 WHERE message_id=?1 AND ifnull(deleted_at,0)=0 ORDER BY id",
        )
        .unwrap()
        .query_map([mid], |r| r.get(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    let mut out = Vec::new();
    for r in rows {
        let Ok(v) = serde_json::from_str::<Value>(&r) else {
            continue;
        };
        if let Some(s) = v.as_str() {
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
        if let Some(arr) = v.as_array() {
            for it in arr {
                if let Some(s) = it.get("text").and_then(Value::as_str) {
                    if !s.is_empty() {
                        out.push(s.to_string());
                    }
                }
            }
        }
        if let Some(s) = v.get("content").and_then(Value::as_str) {
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
    }
    out.join(" | ")
}

/// 统计助手任务内容：过程项数、工具名序列、finish 摘要。
fn task_digest(raw: &str) -> (usize, String, String) {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return (0, "<非 JSON>".into(), String::new());
    };
    let ms = v
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut tools = Vec::new();
    let mut finish = String::new();
    for m in &ms {
        let pi = m.get("plan_item").unwrap_or(&Value::Null);
        let tci = pi.get("tool_call_info").unwrap_or(&Value::Null);
        if tci.is_null() {
            continue;
        }
        let name = tci.get("name").and_then(Value::as_str).unwrap_or("");
        if name == "finish" {
            finish = tci
                .get("params")
                .and_then(|p| p.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            continue;
        }
        if !name.is_empty() {
            tools.push(name.to_string());
        }
    }
    (ms.len(), tools.join(" → "), finish)
}
