//! **只读**取证探针：把某个会话在全库的「原始行」逐列 dump 出来，用于定位
//! 「复制/同步之后客户端不能继续对话」这类**状态型**故障。
//!
//! 与 `consistency_probe` 的区别：那个按「消息/轮次」的业务口径体检；这个**不做任何口径假设**，
//! 直接把行摊开（含 `chat_session` 全列、`chat_turn`、`agent_run`、`chat_message` 等），
//! 并列出**全库所有带 session_id / message_id 的表**在两会话上的行数，便于一眼看出「哪张表多了/少了」。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example session_forensics -- solo-cn --title 百度文库
//!   cargo run -p wb-switch-core --example session_forensics -- solo-cn --sid 6ac3a1a4...
//!   cargo run -p wb-switch-core --example session_forensics -- solo-cn --title 百度 --tables

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

fn fmt_val(v: &Value, maxlen: usize) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Blob(b) => format!("<blob {}B>", b.len()),
        Value::Text(t) => {
            let flat = t.replace(['\n', '\r'], "⏎");
            if flat.chars().count() > maxlen {
                let head: String = flat.chars().take(maxlen).collect();
                format!("{head}…(共 {} 字)", t.chars().count())
            } else {
                flat
            }
        }
    }
}

/// 把 `SELECT * FROM {sql}` 的每一行按 `列=值` 摊开打印。
fn dump(conn: &Connection, sql: &str, params: &[&dyn rusqlite::types::ToSql], maxlen: usize) {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            println!("   ⚠️ 查询失败: {e}\n      {sql}");
            return;
        }
    };
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = match stmt.query_map(params, |r| {
        let mut vals = Vec::new();
        for i in 0..names.len() {
            vals.push(r.get::<_, Value>(i)?);
        }
        Ok(vals)
    }) {
        Ok(r) => r,
        Err(e) => {
            println!("   ⚠️ 查询失败: {e}");
            return;
        }
    };
    let mut n = 0usize;
    for r in rows.flatten() {
        n += 1;
        let parts: Vec<String> = names
            .iter()
            .zip(r.iter())
            .map(|(k, v)| format!("{k}={}", fmt_val(v, maxlen)))
            .collect();
        println!("   [{n}] {}", parts.join(" | "));
    }
    if n == 0 {
        println!("   (无行)");
    }
}

fn table_columns(conn: &Connection, t: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA table_info(\"{t}\")"))
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let client = args.next().unwrap_or_else(|| "solo-cn".into());
    let mut title: Option<String> = None;
    let mut sid: Option<String> = None;
    let mut show_tables = false;
    let mut rest: Vec<String> = args.collect();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--title" => {
                title = rest.get(i + 1).cloned();
                i += 2;
            }
            "--sid" => {
                sid = rest.get(i + 1).cloned();
                i += 2;
            }
            "--tables" => {
                show_tables = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    rest.clear();

    // ⚠️ 先强制刷新快照：`reader_plain_path()` 只会在**实时库主文件**的签名变化时更新快照，
    // 而 Trae 的写入大多只在 WAL 里 ⇒ 「陈旧快照 + 新 WAL」合并出来的是坏库（实测 quick_check 报
    // `btreeInitPage() returns error code 11`）。取证前必须刷新，否则看到的是假象。
    if std::env::var("FORENSICS_NO_REFRESH").is_err() {
        match wb_switch_core::modules::trae_export::ensure_decrypted(&client, None) {
            Ok(o) => println!("已刷新解密快照：reused={} pages={}", o.reused, o.pages),
            Err(e) => println!("⚠️ 刷新快照失败（继续用现有副本）: {e}"),
        }
    }
    let path = wb_switch_core::modules::trae_export::reader_plain_path(&client).expect("视图");
    println!("== 读取视图：{} ==", path.display());
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    match conn.query_row("PRAGMA quick_check(3)", [], |r| r.get::<_, String>(0)) {
        Ok(v) => println!("== quick_check: {v} =="),
        Err(e) => println!("== quick_check 出错: {e} =="),
    }

    // —— 目标会话 ——
    let targets: Vec<(String, String)> = match (&sid, &title) {
        (Some(s), _) => {
            let t: String = conn
                .query_row(
                    "SELECT ifnull(session_title,'') FROM chat_session WHERE session_id=?",
                    [s],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| "(不存在)".into());
            vec![(s.clone(), t)]
        }
        (None, Some(kw)) => conn
            .prepare("SELECT session_id, ifnull(session_title,'') FROM chat_session WHERE session_title LIKE ? ORDER BY ifnull(updated_at,created_at) DESC")
            .unwrap()
            .query_map([format!("%{kw}%")], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap()
            .flatten()
            .collect(),
        (None, None) => {
            println!("用法：--title <关键字> 或 --sid <会话id>");
            return;
        }
    };
    if targets.is_empty() {
        println!("没找到匹配的会话");
        return;
    }

    // —— 全库表分类 ——
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .flatten()
        .collect();
    let mut with_sid: Vec<String> = Vec::new();
    let mut mid_only: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for t in &names {
        let cols = table_columns(&conn, t);
        if cols.iter().any(|c| c == "session_id") {
            with_sid.push(t.clone());
        } else if cols.iter().any(|c| c == "message_id") {
            mid_only.push(t.clone());
        } else {
            other.push(t.clone());
        }
    }
    if show_tables {
        println!("\n== 带 session_id 的表 ==");
        for t in &with_sid {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(-1);
            println!("   {t}  全库 {n} 行  (列: {})", table_columns(&conn, t).join(","));
        }
        println!("\n== 只带 message_id 的表 ==");
        for t in &mid_only {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(-1);
            println!("   {t}  全库 {n} 行  (列: {})", table_columns(&conn, t).join(","));
        }
        println!("\n== 其余表 ==");
        for t in &other {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(-1);
            println!("   {t}  全库 {n} 行");
        }
    }

    // —— 全库会话总览（8 个，便于找「源/副本」对应关系）——
    println!("\n-- 全库会话总览 --");
    dump(
        &conn,
        "SELECT s.id, s.session_id, s.session_type, s.project_id, \
                (SELECT ifnull(user_id,'') FROM project p WHERE p.project_id=s.project_id) AS owner_uid, \
                s.session_title, s.created_at, s.updated_at, s.deleted_at, s.hidden_status, \
                (SELECT count(*) FROM chat_message m WHERE m.session_id=s.session_id) AS msgs, \
                (SELECT count(*) FROM chat_turn t WHERE t.session_id=s.session_id) AS turns \
         FROM chat_session s ORDER BY s.id",
        &[],
        60,
    );

    // —— 工程与工作区注册表：排查「同一路径出现多个 project 行」这类副本副作用 ——
    println!("\n-- 全部 project 行 --");
    dump(
        &conn,
        "SELECT * FROM project ORDER BY user_id, absolute_path, project_id",
        &[],
        80,
    );
    println!("\n-- multi_root_path（工作区根注册表）--");
    dump(&conn, "SELECT * FROM multi_root_path", &[], 140);
    println!("\n-- user_configuration --");
    dump(&conn, "SELECT * FROM user_configuration", &[], 140);

    for (tsid, ttitle) in &targets {
        println!("\n╔════════════════ 会话 {tsid}");
        println!("║ 标题：{ttitle}");
        println!("╚════════════════ chat_session 全列");
        dump(
            &conn,
            "SELECT * FROM chat_session WHERE session_id=?",
            &[tsid],
            200,
        );

        // 把 `context` 原文落盘，便于两端 diff（JSON 键序保留自源行，diff 有意义）。
        if let Ok(ctx) = conn.query_row(
            "SELECT ifnull(context,'') FROM chat_session WHERE session_id=?",
            [tsid],
            |r| r.get::<_, String>(0),
        ) {
            let out = std::env::temp_dir().join(format!("ctx-{tsid}.json"));
            if std::fs::write(&out, &ctx).is_ok() {
                println!(
                    "\n-- context 原文已落盘：{} （{} 字节）--",
                    out.display(),
                    ctx.len()
                );
            }
        }

        println!("\n-- 各表行数 --");
        for t in &with_sid {
            let n: i64 = conn
                .query_row(
                    &format!("SELECT count(*) FROM \"{t}\" WHERE session_id=?"),
                    [tsid],
                    |r| r.get(0),
                )
                .unwrap_or(-1);
            let total: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(-1);
            if n != 0 {
                println!("   {t} = {n}  (全库 {total})");
            }
        }
        for t in &mid_only {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM \"{t}\" WHERE message_id IN \
                         (SELECT message_id FROM chat_message WHERE session_id=?)"
                    ),
                    [tsid],
                    |r| r.get(0),
                )
                .unwrap_or(-1);
            let total: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(-1);
            if n != 0 {
                println!("   {t} = {n}  (全库 {total})");
            }
        }

        println!("\n-- chat_message（按 message_index 排）--");
        dump(
            &conn,
            "SELECT * FROM chat_message WHERE session_id=? ORDER BY ifnull(message_index,0), ifnull(created_at,'')",
            &[tsid],
            90,
        );

        println!("\n-- message_index / 唯一性检查 --");
        let dup: i64 = conn
            .query_row(
                "SELECT count(*) FROM (SELECT message_index FROM chat_message \
                 WHERE session_id=? AND message_index IS NOT NULL \
                 GROUP BY message_index HAVING count(*)>1)",
                [tsid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        println!("   重复 message_index 组数 = {dup}");
        let noidx: i64 = conn
            .query_row(
                "SELECT count(*) FROM chat_message WHERE session_id=? AND message_index IS NULL",
                [tsid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        println!("   message_index 为 NULL 的行数 = {noidx}");
        let del: i64 = conn
            .query_row(
                "SELECT count(*) FROM chat_message WHERE session_id=? AND ifnull(deleted_at,0)<>0",
                [tsid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        println!("   已软删消息数 = {del}");

        println!("\n-- chat_turn 全列 --");
        dump(
            &conn,
            "SELECT * FROM chat_turn WHERE session_id=? ORDER BY ifnull(created_at,0), id",
            &[tsid],
            200,
        );

        println!("\n-- agent_run 全列 --");
        dump(
            &conn,
            "SELECT * FROM agent_run WHERE session_id=? ORDER BY ifnull(created_at,0), id",
            &[tsid],
            90,
        );

        println!("\n-- task / todo_list / history_todo_list --");
        dump(
            &conn,
            "SELECT * FROM task WHERE session_id=? ORDER BY ifnull(created_at,0)",
            &[tsid],
            200,
        );
        dump(
            &conn,
            "SELECT * FROM history_todo_list WHERE session_id=? ORDER BY ifnull(created_at,0) LIMIT 10",
            &[tsid],
            120,
        );
        dump(
            &conn,
            "SELECT * FROM todo_list WHERE id IN (SELECT todo_list_id FROM history_todo_list WHERE session_id=?) LIMIT 10",
            &[tsid],
            120,
        );

        println!("\n-- history_v2（客户端构建上下文用的镜像行，只看头尾）--");
        dump(
            &conn,
            "SELECT * FROM history_v2 WHERE session_id=? ORDER BY ifnull(created_at,0) LIMIT 6",
            &[tsid],
            160,
        );
        println!("   ...");
        dump(
            &conn,
            "SELECT * FROM history_v2 WHERE session_id=? ORDER BY ifnull(created_at,0) DESC LIMIT 4",
            &[tsid],
            160,
        );

        println!("\n-- server_history_info --");
        match conn
            .prepare("SELECT count(*) FROM server_history_info WHERE session_id=?")
            .and_then(|mut s| s.query_row([tsid], |r| r.get::<_, i64>(0)))
        {
            Ok(n) => {
                println!("   session_id 命中 {n} 行");
                if n > 0 {
                    dump(
                        &conn,
                        "SELECT * FROM server_history_info WHERE session_id=? ORDER BY ifnull(updated_at,0) LIMIT 6",
                        &[tsid],
                        160,
                    );
                }
            }
            Err(e) => println!("   ⚠️ 查询失败: {e}"),
        }

        println!("\n-- rules_attachment / local_artifact --");
        dump(
            &conn,
            "SELECT * FROM rules_attachment WHERE chat_session_id=?",
            &[tsid],
            120,
        );

        println!("\n-- session_project --");
        dump(
            &conn,
            "SELECT * FROM session_project WHERE session_id=?",
            &[tsid],
            90,
        );

        println!("\n-- project（本会话 project_id 指向的那条）--");
        dump(
            &conn,
            "SELECT * FROM project WHERE project_id = (SELECT project_id FROM chat_session WHERE session_id=?)",
            &[tsid],
            120,
        );
    }
}
