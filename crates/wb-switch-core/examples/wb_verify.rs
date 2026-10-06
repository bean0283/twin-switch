//! 结构校验：把 WorkBuddy 会话转换成 Trae 行写入**解密库的临时副本**，
//! 然后对比「合成会话」与「真实会话」的结构不变式，确认转换结果与真库同构。
//!
//! 只操作副本，绝不触碰真实加密库或解密缓存。
//! 用法：cargo run -p wb-switch-core --example wb_verify

use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;
use wb_switch_core::modules::{config, workbuddy_import, workbuddy_source};

fn decrypted_db() -> PathBuf {
    // 走 config::store_dir()：数据目录名改过一次（`~/.twin-switch` → `~/.twin-switch`），
    // 手动拼路径的示例会在改名后直接失效。
    config::store_dir().join("trae/decrypted/solo-cn.db")
}

/// 统计一个会话在各关联表里的行数 + 结构不变式。
fn profile(conn: &Connection, sid: &str) -> Value {
    let n = |sql: &str| -> i64 {
        conn.query_row(sql, rusqlite::params![sid], |r| r.get(0))
            .unwrap_or(-1)
    };
    let sessions = n("SELECT count(*) FROM chat_session WHERE session_id=?1");
    let messages = n("SELECT count(*) FROM chat_message WHERE session_id=?1 AND ifnull(deleted_at,0)=0");
    let user_msgs = n("SELECT count(*) FROM chat_message WHERE session_id=?1 AND message_role='user'");
    let asst_msgs = n("SELECT count(*) FROM chat_message WHERE session_id=?1 AND message_role='assistant'");
    let turns = n("SELECT count(*) FROM chat_turn WHERE session_id=?1");
    let hv2 = n("SELECT count(*) FROM history_v2 WHERE session_id=?1");
    let sprj = n("SELECT count(*) FROM session_project WHERE session_id=?1");
    let runs = n("SELECT count(*) FROM agent_run WHERE session_id=?1");
    // 引用完整性：每个 chat_message 都应在 general 或 task 里有对应内容行
    let orphan = n(
        "SELECT count(*) FROM chat_message m WHERE m.session_id=?1 AND m.deleted_at=0 \
         AND NOT EXISTS (SELECT 1 FROM chat_message_general g WHERE g.message_id=m.message_id) \
         AND NOT EXISTS (SELECT 1 FROM chat_message_task t WHERE t.message_id=m.message_id)",
    );
    // chat_turn 引用必须指向本会话真实消息
    let bad_turn = n(
        "SELECT count(*) FROM chat_turn ct WHERE ct.session_id=?1 AND ( \
           NOT EXISTS (SELECT 1 FROM chat_message m WHERE m.message_id=ct.response_message_id) \
        OR NOT EXISTS (SELECT 1 FROM chat_message m2 WHERE m2.message_id=ct.reply_to_message_id))",
    );
    // 每个 task 消息必须有 history_v2 支撑（渲染回答）
    let asst_without_history = n(
        "SELECT count(*) FROM chat_message m WHERE m.session_id=?1 AND m.message_role='assistant' \
         AND NOT EXISTS (SELECT 1 FROM history_v2 h WHERE h.message_id=m.message_id)",
    );
    // JSON 列必须可解析
    let mut bad_json = 0i64;
    let mut stmt = conn
        .prepare("SELECT content FROM chat_message_task t JOIN chat_message m ON m.message_id=t.message_id WHERE m.session_id=?1")
        .unwrap();
    let rows: Vec<String> = stmt
        .query_map(rusqlite::params![sid], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    for c in &rows {
        if serde_json::from_str::<Value>(c).is_err() {
            bad_json += 1;
        }
    }
    let mut stmt2 = conn
        .prepare("SELECT messages FROM history_v2 WHERE session_id=?1")
        .unwrap();
    let rows2: Vec<String> = stmt2
        .query_map(rusqlite::params![sid], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    for c in &rows2 {
        if serde_json::from_str::<Value>(c).is_err() {
            bad_json += 1;
        }
    }
    // plan_item 结构检查
    let mut plan_items = 0i64;
    let mut plan_items_with_tool = 0i64;
    for c in &rows {
        if let Ok(v) = serde_json::from_str::<Value>(c) {
            if let Some(msgs) = v.get("messages").and_then(Value::as_array) {
                for m in msgs {
                    if m.get("type").and_then(Value::as_str) == Some("plan_item") {
                        plan_items += 1;
                        if m.pointer("/plan_item/tool_call_info/name")
                            .and_then(Value::as_str)
                            .map(|s| !s.is_empty())
                            .unwrap_or(false)
                        {
                            plan_items_with_tool += 1;
                        }
                    }
                }
            }
        }
    }
    serde_json::json!({
        "session_rows": sessions,
        "messages": messages,
        "user_msgs": user_msgs,
        "asst_msgs": asst_msgs,
        "turns": turns,
        "history_rows": hv2,
        "session_project": sprj,
        "agent_run": runs,
        "orphan_messages": orphan,
        "broken_turn_refs": bad_turn,
        "asst_without_history": asst_without_history,
        "bad_json_columns": bad_json,
        "plan_items": plan_items,
        "plan_items_with_tool": plan_items_with_tool,
    })
}

fn main() {
    let src_db = decrypted_db();
    if !src_db.is_file() {
        println!("找不到解密库：{}", src_db.display());
        return;
    }
    // 复制到临时副本
    let tmp = std::env::temp_dir().join("wb_verify_plain.db");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(&src_db, &tmp).expect("复制解密库失败");
    let conn = Connection::open(&tmp).expect("打开副本失败");

    // 目标账号 uid：取库内已有的一个
    let uid: String = conn
        .query_row(
            "SELECT user_id FROM project WHERE ifnull(user_id,'')<>'' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "925158341882023".to_string());
    println!("目标 uid: {uid}");

    // 真实会话基线
    let real_sid: String = conn
        .query_row(
            "SELECT session_id FROM chat_message GROUP BY session_id ORDER BY count(*) DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("\n=== 真实会话基线 {real_sid} ===");
    let real_profile = profile(&conn, &real_sid);
    println!("{}", serde_json::to_string_pretty(&real_profile).unwrap());

    // 挑出所有可用会话做批量转换（覆盖多回合场景）
    let listed = workbuddy_source::list_sessions().expect("列出 WorkBuddy 会话失败");
    let usable: Vec<_> = listed
        .iter()
        .filter(|s| s.has_body && !s.deleted && s.body_bytes > 200)
        .collect();
    println!("\n=== 批量转换 {} 个 WorkBuddy 会话 ===", usable.len());

    let log = |m: &str| println!("{m}");
    let mut made: Vec<(String, String)> = Vec::new(); // (new_sid, 标题)
    for target in &usable {
        let body = match workbuddy_source::load_body(&target.id) {
            Ok(b) => b,
            Err(e) => {
                println!("解析失败 {}: {e}", target.id);
                continue;
            }
        };
        if body.turns.is_empty() {
            continue;
        }
        let title = body
            .ai_title
            .clone()
            .unwrap_or_else(|| target.title.clone());
        match workbuddy_import::write_session(&conn, "solo-cn", &uid, target, &body, &log) {
            Ok(st) => {
                let new_sid: String = conn
                    .query_row(
                        "SELECT session_id FROM chat_session WHERE session_title=?1 ORDER BY id DESC LIMIT 1",
                        rusqlite::params![title],
                        |r| r.get(0),
                    )
                    .unwrap_or_default();
                println!(
                    "  ✓ 「{}」{} 回合 / {} 步骤 → {}",
                    title, st.turns, st.steps, new_sid
                );
                made.push((new_sid, title));
            }
            Err(e) => println!("  ✗ 「{title}」转换失败: {e}"),
        }
    }

    // 汇总不变式：全部合成会话都必须干净
    println!("\n=== 全部合成会话的不变式汇总 ===");
    let mut worst: Vec<String> = Vec::new();
    let mut tot_turns = 0i64;
    let mut tot_msgs = 0i64;
    for (sid, title) in &made {
        let p = profile(&conn, sid);
        tot_turns += p.get("turns").and_then(Value::as_i64).unwrap_or(0);
        tot_msgs += p.get("messages").and_then(Value::as_i64).unwrap_or(0);
        for key in [
            "orphan_messages",
            "broken_turn_refs",
            "asst_without_history",
            "bad_json_columns",
        ] {
            let v = p.get(key).and_then(Value::as_i64).unwrap_or(-1);
            if v != 0 {
                worst.push(format!("{title}: {key}={v}"));
            }
        }
        // session_project 必须恰好 1 行（缺行会导致无法续聊）
        let sp = p.get("session_project").and_then(Value::as_i64).unwrap_or(-1);
        if sp != 1 {
            worst.push(format!("{title}: session_project={sp}（应为 1）"));
        }
        let ar = p.get("agent_run").and_then(Value::as_i64).unwrap_or(-1);
        let tn = p.get("turns").and_then(Value::as_i64).unwrap_or(-1);
        if ar != tn {
            worst.push(format!("{title}: agent_run={ar} 与 turns={tn} 不符"));
        }
    }
    println!("合成会话数: {}", made.len());
    println!("合计回合: {tot_turns} / 消息: {tot_msgs}");
    if worst.is_empty() {
        println!("全部不变式合格 ✅");
    } else {
        println!("发现问题 {} 项：", worst.len());
        for w in worst {
            println!("  - {w}");
        }
    }

    // 展示一个多回合合成会话
    if let Some((sid, title)) = made.iter().find(|(_, t)| t.contains("trae_refresh")).or(made.first()) {
        println!("\n=== 合成会话可读性抽样「{title}」 ===");
        let mut stmt = conn
            .prepare(
                "SELECT message_id, message_role, message_index FROM chat_message \
                 WHERE session_id=?1 AND ifnull(deleted_at,0)=0 ORDER BY message_index LIMIT 8",
            )
            .unwrap();
        let msgs: Vec<(String, String, i64)> = stmt
            .query_map(rusqlite::params![sid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for (mid, role, idx) in msgs {
            if role == "user" {
                let c: String = conn
                    .query_row(
                        "SELECT content FROM chat_message_general WHERE message_id=?1",
                        rusqlite::params![mid],
                        |r| r.get(0),
                    )
                    .unwrap_or_default();
                let shown = serde_json::from_str::<Value>(&c)
                    .ok()
                    .and_then(|v| {
                        v.get(0)
                            .and_then(|b| b.get("text_content"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or(c);
                println!("  [{idx}] user  : {}", shown.chars().take(100).collect::<String>());
            } else {
                let n: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM history_v2 WHERE message_id=?1",
                        rusqlite::params![mid],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                let final_text: Option<String> = conn
                    .query_row(
                        "SELECT messages FROM history_v2 WHERE message_id=?1 AND content_source='llm_default' LIMIT 1",
                        rusqlite::params![mid],
                        |r| r.get(0),
                    )
                    .ok();
                let shown = final_text
                    .and_then(|s| {
                        serde_json::from_str::<Value>(&s).ok().and_then(|v| {
                            v.pointer("/raw_messages/0/content/0/text")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                    })
                    .unwrap_or_default();
                println!(
                    "  [{idx}] assist: ({n} 条历史) {}",
                    shown.chars().take(100).collect::<String>()
                );
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
}
