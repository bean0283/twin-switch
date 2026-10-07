//! **只读**探针：dump 关联组两端会话在**客户端实际读到的视图**
//! （`reader_plain_path()` = 快照副本 + 合并 WAL）里的逐条内容，
//! 用于判定「同步后 A 显示的到底是谁的内容」。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example linked_dump -- solo-cn [sid1 sid2 ...]
//! 不传 sid 时自动读 `trae-session-links.json` 里所有关联成员。

use rusqlite::{Connection, OpenFlags};
use std::path::Path;

fn s(v: &rusqlite::types::Value) -> String {
    match v {
        rusqlite::types::Value::Text(t) => t.clone(),
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Null => String::new(),
        other => format!("{other:?}"),
    }
}

fn preview(conn: &Connection, table: &str, mid: &str) -> String {
    let r: Result<String, _> = conn.query_row(
        &format!("SELECT content FROM {table} WHERE message_id=?"),
        [mid],
        |r| r.get(0),
    );
    match r {
        Ok(c) => {
            let flat = c.replace(['\n', '\r'], " ");
            let chars: Vec<char> = flat.chars().collect();
            let head: String = chars.iter().take(70).collect();
            format!(
                "{} [{} B]",
                head,
                c.as_bytes().len()
            )
        }
        Err(_) => "<无内容行>".into(),
    }
}

fn dump(conn: &Connection, sid: &str) {
    println!("\n================ sid {}", sid);
    let row: Result<(String, String, String, String), _> = conn.query_row(
        "SELECT s.session_title, ifnull(s.project_id,''), ifnull(p.user_id,''), \
         ifnull(p.project_name,'') \
         FROM chat_session s LEFT JOIN project p ON s.project_id = p.project_id \
         WHERE s.session_id = ?",
        [sid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    );
    match row {
        Ok((t, pid, uid, pname)) => println!(
            "标题={t:?}  owner_uid={uid}  project_id={pid}  project={pname:?}"
        ),
        Err(e) => println!("会话行读取失败：{e}"),
    }

    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_message WHERE session_id=?",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    let nt: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_turn WHERE session_id=?",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    println!("chat_message={n}  chat_turn={nt}");

    println!("  -- 消息（按 message_index） --");
    let mut st = conn
        .prepare(
            "SELECT message_id, message_type, message_role, message_index, \
             ifnull(reply_to_message_id,''), created_at \
             FROM chat_message WHERE session_id=? ORDER BY message_index, message_id",
        )
        .unwrap();
    let rows = st
        .query_map([sid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, rusqlite::types::Value>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, rusqlite::types::Value>(5)?,
            ))
        })
        .unwrap();
    for r in rows.flatten() {
        let tbl = if r.1.contains("task") {
            "chat_message_task"
        } else {
            "chat_message_general"
        };
        println!(
            "   idx={:<3} {:<10} {:<9} mid={} reply={} created={}",
            s(&r.3),
            r.1,
            r.2,
            r.0,
            r.4,
            s(&r.5)
        );
        println!("        ↳ {}", preview(conn, tbl, &r.0));
    }

    println!("  -- 轮次（chat_turn） --");
    let mut st2 = conn
        .prepare(
            "SELECT turn_id, ifnull(reply_to_message_id,''), ifnull(response_message_id,'') \
             FROM chat_turn WHERE session_id=? ORDER BY id",
        )
        .unwrap();
    let rows2 = st2
        .query_map([sid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap();
    for r in rows2.flatten() {
        // 判定引用是否自洽：引用的 mid 是否真属于本会话
        let ok = |mid: &str| -> bool {
            if mid.is_empty() {
                return true;
            }
            conn.query_row(
                "SELECT count(*) FROM chat_message WHERE message_id=? AND session_id=?",
                rusqlite::params![mid, sid],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
                > 0
        };
        println!(
            "   TURN {} reply={} [{}] resp={} [{}]",
            r.0,
            r.1,
            if ok(&r.1) { "本会话" } else { "⚠️不属于本会话" },
            r.2,
            if ok(&r.2) { "本会话" } else { "⚠️不属于本会话" }
        );
    }
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let mut sids: Vec<String> = std::env::args().skip(2).collect();
    if sids.is_empty() {
        if let Ok(Some(g)) = wb_switch_core::modules::trae_session_links::group_members(
            &client,
            "0a935c7f-81c5-41f2-8278-f31a91908095",
        ) {
            for m in g {
                println!("关联成员 role={} uid={} sid={}", m.role, m.uid, m.session_id);
                sids.push(m.session_id);
            }
        }
    }

    let path = match wb_switch_core::modules::trae_export::reader_plain_path(&client) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("读取视图失败：{e}");
            return;
        }
    };
    println!("== 视图：{} ==", path.display());
    let conn = match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("打开失败：{e}");
            return;
        }
    };
    let total: i64 = conn
        .query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0))
        .unwrap_or(-1);
    println!("全库 chat_session = {total}");

    for sid in &sids {
        dump(&conn, sid);
    }

    let _ = Path::new(".");
}
