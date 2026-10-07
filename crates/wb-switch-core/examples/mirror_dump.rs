//! **只读**探针：把实时视图里「与这两个关联会话相关」的**所有表**列出来，
//! 逐表打印列名 + 行数 + 内容预览，用来定位「同步后客户端仍显示旧内容」时，
//! 旧内容究竟还留在哪张表里。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example mirror_dump -- solo-cn [sidA sidB]

use rusqlite::{Connection, OpenFlags};

fn s(v: &rusqlite::types::Value) -> String {
    match v {
        rusqlite::types::Value::Text(t) => {
            let flat = t.replace(['\n', '\r'], " ");
            let head: String = flat.chars().take(60).collect();
            head
        }
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Null => "NULL".into(),
        rusqlite::types::Value::Real(f) => f.to_string(),
        rusqlite::types::Value::Blob(b) => format!("<blob {}B>", b.len()),
    }
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let sids: Vec<String> = match std::env::args().nth(2) {
        Some(_) => std::env::args().skip(2).collect(),
        None => vec![
            "6ac3a1a461373ef8900b49cd".to_string(),
            "2908be72bbcf78fa054f6577".to_string(),
        ],
    };

    let path = wb_switch_core::modules::trae_export::reader_plain_path(&client).expect("视图");
    println!("== 视图：{} ==", path.display());
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    println!("\n全库表数 = {}", tables.len());
    for t in &tables {
        let cols: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{t}\")"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let has_sid = cols.iter().any(|c| c == "session_id");
        let has_mid = cols.iter().any(|c| c == "message_id");
        let has_cid = cols.iter().any(|c| c == "conversation_id");
        if !has_sid && !has_mid && !has_cid {
            continue;
        }
        let total: i64 = conn
            .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
            .unwrap_or(-1);
        println!("\n===== 表 {t}  总行数={total}");
        println!("     列：{}", cols.join(", "));
        for sid in &sids {
            let mut where_sql = String::new();
            if has_sid {
                where_sql = format!("\"session_id\" = '{}'", sid);
            } else if has_cid {
                where_sql = format!("\"conversation_id\" = '{}'", sid);
            }
            if where_sql.is_empty() {
                if has_mid {
                    let n: i64 = conn
                        .query_row(
                            &format!(
                                "SELECT count(*) FROM \"{t}\" WHERE \"message_id\" IN \
                                 (SELECT message_id FROM chat_message WHERE session_id='{sid}')"
                            ),
                            [],
                            |r| r.get(0),
                        )
                        .unwrap_or(-1);
                    println!("   sid {}… 按 message_id 关联行数 = {n}", &sid[..8]);
                }
                continue;
            }
            let n: i64 = conn
                .query_row(
                    &format!("SELECT count(*) FROM \"{t}\" WHERE {where_sql}"),
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(-1);
            println!("   sid {}… 行数 = {n}", &sid[..8]);
            if n <= 0 || n > 40 {
                continue;
            }
            let sel = if cols.len() > 8 {
                cols[..8].iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ")
            } else {
                "*".to_string()
            };
            let mut st = conn
                .prepare(&format!(
                    "SELECT {sel} FROM \"{t}\" WHERE {where_sql} LIMIT 40"
                ))
                .unwrap();
            let ncol = if cols.len() > 8 { 8 } else { cols.len() };
            let rows = st
                .query_map([], |r| {
                    let mut v = Vec::new();
                    for i in 0..ncol {
                        v.push(r.get::<_, rusqlite::types::Value>(i)?);
                    }
                    Ok(v)
                })
                .unwrap();
            for r in rows.flatten() {
                let parts: Vec<String> = r.iter().enumerate().map(|(i, v)| format!("{}={}", cols[i], s(v))).collect();
                println!("      {}", parts.join(" | "));
            }
        }
    }
}
