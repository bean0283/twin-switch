//! 在**真实目标库的副本**上离线跑一遍 `write_session`，复现／回归「导入失败」类事故。
//!
//! 用法：
//!   `cargo run --example wb_write_probe -- <明文库> <workbuddy 会话 id> [uid]`
//!
//! - `<明文库>` 通常是用 `wb_probe` 解出来的实时库副本；本工具会**再复制一份**再写，
//!   原文件不会被改动，**不碰线上库**。
//! - 重点验证：库里若残留同 `agent_run_id` 的孤儿行（客户端删除会话时只清
//!   `chat_session`），导入仍应成功。
use std::path::PathBuf;

use rusqlite::{Connection, params};
use wb_switch_core::modules::workbuddy_import::write_session;
use wb_switch_core::modules::workbuddy_source::{list_sessions, load_body};

/// 与 `workbuddy_import::det_session_id` 同一公式（那边是私有的，这里复制一份用于诊断）：
/// `hex(created_at_secs, 8) + wb_id 的前 16 个 hex 字符`。
fn det_session_id(wb_id: &str, start: i64) -> String {
    let hex: String = wb_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    format!("{:08x}{}", start.max(0) as u32, &hex[..16])
}

fn uuid5(name: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let plain = PathBuf::from(
        args.get(1)
            .cloned()
            .unwrap_or_else(|| "C:/Users/11970/AppData/Local/Temp/wb_probe/live2.db".into()),
    );
    let wb_id = args
        .get(2)
        .cloned()
        .expect("用法: wb_write_probe <明文库> <workbuddy 会话 id> [uid]");

    if !plain.is_file() {
        eprintln!("找不到明文库: {}", plain.display());
        std::process::exit(1);
    }

    let work_dir = std::env::temp_dir().join("wb_write_probe");
    let _ = std::fs::create_dir_all(&work_dir);
    let sim = work_dir.join("sim.db");
    std::fs::copy(&plain, &sim).expect("复制副本失败");
    println!("明文副本: {}", sim.display());

    let conn = Connection::open(&sim).expect("打开副本失败");
    let uid: String = args.get(3).cloned().unwrap_or_else(|| {
        conn.query_row(
            "SELECT user_id FROM project WHERE ifnull(deleted_at,0)=0 LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default()
    });
    println!("uid     : {uid}");

    let listed = list_sessions().expect("读取 WorkBuddy 会话列表失败");
    let meta = listed
        .iter()
        .find(|s| s.id == wb_id)
        .cloned()
        .expect("该 WorkBuddy 会话不在本机列表中");
    let body = load_body(&wb_id).expect("读取 WorkBuddy 会话正文失败");
    println!(
        "源会话  : 「{}」{} 个回合 / cwd={}",
        meta.title,
        body.turns.len(),
        meta.cwd
    );

    let start = meta.created_at / 1000;
    let sid = det_session_id(&wb_id, start);
    let run_id = uuid5(&format!("{sid}:{start}"));
    println!("预期 sid: {sid}");
    println!("预期 run: {run_id}");

    let before_run: i64 = conn
        .query_row(
            "SELECT count(*) FROM agent_run WHERE agent_run_id=?1",
            params![run_id],
            |r| r.get(0),
        )
        .unwrap();
    let before_sess: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_session WHERE session_id=?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    println!("写入前：同 id 的 agent_run {before_run} 行 / chat_session {before_sess} 行");
    if before_run > 0 && before_sess == 0 {
        println!("        ↑ 正是线上事故的形态：孤儿 run 行（客户端删会话留下的）");
    }

    let r = write_session(&conn, "solo-cn", &uid, &meta, &body, &|m| println!("   {m}"));
    match &r {
        Ok(s) => println!(
            "✅ 写入成功：{} 回合 / {} 工具步骤 / {} 消息 / {} history / 清理旧行 {}",
            s.turns, s.steps, s.messages, s.history_rows, s.skipped_duplicates
        ),
        Err(e) => println!("❌ 写入失败：{e}"),
    }

    let after_run: i64 = conn
        .query_row(
            "SELECT count(*) FROM agent_run WHERE agent_run_id=?1",
            params![run_id],
            |r| r.get(0),
        )
        .unwrap();
    let after_sess: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_session WHERE session_id=?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    let turns: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_turn WHERE session_id=?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    let tasks: i64 = conn
        .query_row(
            "SELECT count(*) FROM task WHERE session_id=?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    println!("写入后：agent_run {after_run} 行（必须=1）/ chat_session {after_sess} 行 / chat_turn {turns} / task {tasks}");
    println!("        task 行数必须等于 chat_turn 行数（任务列表 UI 依赖该不变量）");

    if r.is_ok() && after_run == 1 && after_sess == 1 && turns == tasks {
        println!("==> 通过");
    } else {
        println!("==> 未通过");
        std::process::exit(2);
    }
}
