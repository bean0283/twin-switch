//! 离线干跑：在**已解密快照的副本**上执行一次「清理 + 转换写入」，产出一个明文库供
//! sqlite 检查结构是否与真实 Trae 库一致。
//!
//! 刻意不接触任何加密库、不改动线上数据——只验证 `write_session` / `purge_broken_sessions`
//! 产生的行是否符合实测硬约束（task 行数 ≡ 回合数、id 时间戳前缀、project 行合法性）。
//!
//! ```text
//! cargo run --example wb_dryrun                       # 自动挑标题含 nihao 的会话
//! cargo run --example wb_dryrun -- <wb-session-id>    # 指定源会话
//! ```

use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;

use wb_switch_core::modules::{workbuddy_import, workbuddy_source};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut want = None;
    let mut db_override = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--db" if i + 1 < args.len() => {
                db_override = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            other => {
                want = Some(other.to_string());
                i += 1;
            }
        }
    }

    // 输入：优先用已解密快照（快），没有则报错提示先生成。
    // 路径走 config::store_dir()，别手拼目录名（改名后手拼的会失效）。
    let snapshot = db_override.unwrap_or_else(|| {
        wb_switch_core::modules::config::store_dir()
            .join("trae")
            .join("decrypted")
            .join("solo-cn.db")
    });
    if !snapshot.is_file() {
        eprintln!("找不到解密快照：{}", snapshot.display());
        eprintln!("请先在 App 的「会话记录」页刷新一次（生成快照）后重试。");
        std::process::exit(1);
    }

    // 1) 复制快照 → 干跑工作副本（绝不改动源快照）
    let dir = std::env::temp_dir().join("wb_dryrun");
    std::fs::create_dir_all(&dir).expect("创建干跑目录");
    let work = dir.join("dry-run.db");
    std::fs::copy(&snapshot, &work).expect("复制快照");
    println!("干跑副本：{}", work.display());

    // 2) 选源会话
    let sessions = workbuddy_source::list_sessions().expect("读取 WorkBuddy 会话");
    let pick = match &want {
        Some(id) => sessions
            .iter()
            .find(|s| &s.id == id)
            .cloned()
            .unwrap_or_else(|| {
                eprintln!("源会话 {id} 不存在");
                std::process::exit(1);
            }),
        None => sessions
            .iter()
            .find(|s| s.has_body && s.title.to_lowercase().contains("nihao"))
            .or_else(|| sessions.iter().find(|s| s.has_body))
            .cloned()
            .unwrap_or_else(|| {
                eprintln!("没有可导入的源会话");
                std::process::exit(1);
            }),
    };
    println!("源会话：{} [{}] cwd={}", pick.title, pick.id, pick.cwd);

    let body = workbuddy_source::load_body(&pick.id).expect("解析源会话");
    println!("回合数：{}", body.turns.len());

    // 3) 干跑：清理幽灵 + 写入转换结果
    let conn = Connection::open(&work).expect("打开干跑副本");
    let before: i64 = conn
        .query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0))
        .unwrap();
    println!("\n--- 清理前 chat_session = {before} ---");
    let purged = workbuddy_import::purge_broken_sessions(&conn, &|m| println!("{m}")).expect("清理");
    println!("清理掉 {purged} 个幽灵会话");
    let badp =
        workbuddy_import::purge_malformed_projects(&conn, &|m| println!("{m}")).expect("清理畸形项目");
    println!("清理掉 {badp} 个畸形项目行");

    let log = |m: &str| println!("{m}");
    let st = workbuddy_import::write_session(&conn, "solo-cn", "925158341882023", &pick, &body, &log)
        .expect("写入干跑副本");
    println!("写入：{} 回合 / {} 工具步骤 / {} 历史行", st.turns, st.steps, st.history_rows);

    // 4) 自检：把实测硬约束逐条验一遍
    println!("\n=== 干跑结果自检 ===");
    let rows: Vec<(String, String, i64, i64, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT s.session_id, ifnull(s.session_title,''), s.created_at,
                        (SELECT count(*) FROM chat_turn t WHERE t.session_id=s.session_id),
                        (SELECT count(*) FROM task k WHERE k.session_id=s.session_id)
                 FROM chat_session s ORDER BY s.session_id",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap();
        it.filter_map(Result::ok).collect()
    };
    let mut bad = 0;
    for (sid, title, created, turns, tasks) in &rows {
        let pref = u32::from_str_radix(&sid[..8], 16).unwrap_or(0);
        let id_ok = pref as i64 == *created;
        let task_ok = turns == tasks;
        if !id_ok || !task_ok {
            bad += 1;
        }
        println!(
            "  {} turns={turns:<3} task={tasks:<3} id前缀{} {}  {}  [{}]",
            sid,
            if id_ok { "✅" } else { "❌" },
            if task_ok { "✅" } else { "❌" },
            if *created == 0 { "-" } else { "" },
            title.chars().take(24).collect::<String>()
        );
    }
    println!(
        "\n会话总数 {}，违反硬约束的 {} 个 {}",
        rows.len(),
        bad,
        if bad == 0 { "✅ 全部符合" } else { "❌" }
    );

    // 4b) v0.0.5 不变量：context 不得夹带别的会话、finish 项承载最终回答、revertible=1
    println!("\n=== v0.0.5 不变量自检 ===");
    let ours: String = conn
        .query_row(
            "SELECT session_id FROM chat_session ORDER BY updated_at DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let uid: String = "925158341882023".to_string();
    let _ = uid;

    // 本会话全部用户提问
    let mut mine: Vec<String> = Vec::new();
    {
        let mut st = conn
            .prepare(
                "SELECT g.content FROM chat_message_general g \
                 JOIN chat_message m ON m.message_id=g.message_id \
                 WHERE m.session_id=?1 AND m.message_role='user'",
            )
            .unwrap();
        for r in st.query_map([&ours], |r| r.get::<_, String>(0)).unwrap() {
            let raw = r.unwrap();
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(t) = v.get(0).and_then(|x| x.get("text_content")).and_then(Value::as_str)
                {
                    mine.push(t.to_string());
                }
            }
        }
    }
    // 其他会话的全部用户提问（仅用于统计，判定用下面的精确比对——
    // 同一句话在多个会话里都出现过属常态，子串匹配会误报）
    let mut foreign: Vec<String> = Vec::new();
    {
        let mut st = conn
            .prepare(
                "SELECT g.content FROM chat_message_general g \
                 JOIN chat_message m ON m.message_id=g.message_id \
                 WHERE m.session_id<>?1 AND m.message_role='user'",
            )
            .unwrap();
        for r in st.query_map([&ours], |r| r.get::<_, String>(0)).unwrap() {
            let raw = r.unwrap();
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(t) = v.get(0).and_then(|x| x.get("text_content")).and_then(Value::as_str)
                {
                    if t.chars().count() >= 4 {
                        foreign.push(t.to_string());
                    }
                }
            }
        }
    }
    println!(
        "  本会话提问 {} 条；其他会话提问 {} 条（本会话 context 只允许出现自己的提问）",
        mine.len(),
        foreign.len()
    );

    // 精确判定：context 里声明的每一段提问文本都必须来自本会话
    let mut mismatch = 0;
    let mut leaked: Vec<String> = Vec::new();
    {
        let mut st = conn
            .prepare("SELECT turn_id, context FROM chat_turn WHERE session_id=?1")
            .unwrap();
        for r in st
            .query_map([&ours], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
        {
            let (tid, ctx) = r.unwrap();
            let Ok(v) = serde_json::from_str::<Value>(&ctx) else {
                continue;
            };
            let mut declared: Vec<String> = Vec::new();
            if let Some(arr) = v
                .pointer("/persist_user_message_context/query")
                .and_then(Value::as_array)
            {
                for it in arr {
                    if let Some(c) = it.pointer("/data/content").and_then(Value::as_str) {
                        declared.push(c.to_string());
                    }
                }
            }
            if let Some(arr) = v
                .pointer("/persist_user_message_context/parsed_query")
                .and_then(Value::as_array)
            {
                for it in arr {
                    if let Some(c) = it.as_str() {
                        declared.push(c.to_string());
                    }
                }
            }
            for d in &declared {
                if d.trim().is_empty() {
                    continue;
                }
                if !mine.iter().any(|m| m == d) {
                    mismatch += 1;
                    if leaked.len() < 3 {
                        leaked.push(format!("{tid} → {}", d.chars().take(40).collect::<String>()));
                    }
                }
            }
        }
    }
    for l in &leaked {
        println!("  ❌ context 里出现了不属于本会话的提问：{l}");
    }
    println!(
        "  context 提问文本不匹配 {} 处 {}",
        mismatch,
        if mismatch == 0 { "✅" } else { "❌" }
    );

    // 助手可见回答必须落在末条 finish 项的 params.summary
    let mut missing_finish = 0;
    let mut empty_summary = 0;
    let total_msgs = {
        let mut st = conn
            .prepare(
                "SELECT t.message_id, t.content FROM chat_message_task t \
                 JOIN chat_message m ON m.message_id=t.message_id WHERE m.session_id=?1",
            )
            .unwrap();
        let mut total = 0;
        for r in st.query_map([&ours], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).unwrap()
        {
            let (mid, content) = r.unwrap();
            total += 1;
            let Ok(v) = serde_json::from_str::<Value>(&content) else {
                continue;
            };
            let msgs = v.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
            let last = msgs.last();
            let name = last
                .and_then(|m| m.pointer("/plan_item/tool_call_info/name"))
                .and_then(Value::as_str)
                .unwrap_or("<无>");
            let summary = last
                .and_then(|m| m.pointer("/plan_item/tool_call_info/params/summary"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if name != "finish" {
                missing_finish += 1;
                println!("  ❌ {mid} 末条项不是 finish（name={name}，messages={}）", msgs.len());
            } else if summary.trim().is_empty() {
                empty_summary += 1;
            }
        }
        total
    };
    println!(
        "  助手消息 {total_msgs} 条：末项不是 finish 的 {missing_finish} 条 {}；正文为空的 {empty_summary} 条",
        if missing_finish == 0 { "✅" } else { "❌" }
    );

    let rev: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_message WHERE session_id=?1 AND ifnull(revertible,0)<>1",
            [&ours],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    let runs: i64 = conn
        .query_row("SELECT count(*) FROM agent_run WHERE session_id=?1", [&ours], |r| r.get(0))
        .unwrap_or(-1);
    let wsf: String = conn
        .query_row(
            "SELECT ifnull(context,'') FROM chat_turn WHERE session_id=?1 LIMIT 1",
            [&ours],
            |r| r.get(0),
        )
        .unwrap_or_default();
    println!(
        "  revertible≠1 的消息 {} 条 {}；agent_run {} 条（真实为会话级，通常 1 条）",
        rev,
        if rev == 0 { "✅" } else { "❌" },
        runs
    );
    println!(
        "  首回合 workspace_folders = {}",
        serde_json::from_str::<Value>(&wsf)
            .ok()
            .and_then(|v| v.get("workspace_folders").cloned())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<解析失败>".into())
    );

    // 5) 打印新写入会话的关键字段，便于人工比对
    println!("\n=== project 行 ===");
    let mut s = conn
        .prepare("SELECT project_id, user_id, name, absolute_path, biz_project_id, workspace_status FROM project ORDER BY id DESC LIMIT 3")
        .unwrap();
    for r in s
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .unwrap()
        .filter_map(Result::ok)
    {
        println!("  pid={} uid={} name={:?} path={:?} biz={:?} ws={:?}", r.0, r.1, r.2, r.3, r.4, r.5);
    }

    println!("\n干跑明文库已就绪，可用 sqlite 进一步检查：{}", work.display());
}
