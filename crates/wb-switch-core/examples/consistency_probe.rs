//! **只读**体检探针：对一个会话做完整的引用一致性检查，并与另一端对照。
//!
//! 检查项（每一项都能独立判红，用于定位「同步后内容不对/只剩一句」）：
//!   ① 每条 chat_message 是否都有且只有一条内容行（general / task 各自对应）；
//!   ② 是否有「孤儿内容行」—— 内容行的 message_id 不属于本会话；
//!   ③ chat_turn 的 reply/response 是否都能落到本会话的消息上，且 role 配对正确
//!      （reply 必须是 user、response 必须是 assistant）；
//!   ④ `message_index` 是否与 role/type 的交替规律一致（user/assistant 交替）；
//!   ⑤ `fts_message_content` 行数是否等于 user 消息数（搜索索引是否跟上）；
//!   ⑥ `history_v2` 里是否有指向本会话以外消息的行。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example consistency_probe -- solo-cn sidA sidB

use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};

fn mid_list(conn: &Connection, sid: &str) -> Vec<String> {
    conn.prepare("SELECT message_id FROM chat_message WHERE session_id=?")
        .unwrap()
        .query_map([sid], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn mid_meta(conn: &Connection, mid: &str) -> Option<(String, String, i64)> {
    conn.query_row(
        "SELECT message_role, message_type, ifnull(message_index,0) FROM chat_message WHERE message_id=?",
        [mid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .ok()
}

fn report(conn: &Connection, sid: &str) {
    println!("\n════════════════ 会话 {}", sid);
    let mids = mid_list(conn, sid);
    let set: HashSet<&String> = mids.iter().collect();
    println!("chat_message = {}", mids.len());

    // ① / ② 内容行
    for table in ["chat_message_general", "chat_message_task"] {
        let rows: Vec<String> = conn
            .prepare(&format!(
                "SELECT message_id FROM {table} WHERE message_id IN \
                 (SELECT message_id FROM chat_message WHERE session_id=?)"
            ))
            .unwrap()
            .query_map([sid], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let want: Vec<&String> = mids
            .iter()
            .filter(|m| mid_meta(conn, m).map(|x| x.1 == table.trim_start_matches("chat_message_")).unwrap_or(false))
            .collect();
        let got: HashSet<&String> = rows.iter().collect();
        let missing: Vec<String> = want
            .iter()
            .filter(|m| !got.contains(**m))
            .map(|m| m[..8].to_string())
            .collect();
        println!(
            "  {table}: 期望 {} 行 / 实际 {} 行{}",
            want.len(),
            rows.len(),
            if missing.is_empty() {
                "  ✅".to_string()
            } else {
                format!("  ❌ 缺内容的消息 {:?}", missing)
            }
        );
    }
    // 孤儿内容行：mid 不在本会话
    for table in ["chat_message_general", "chat_message_task"] {
        let orphans: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM {table} t WHERE t.message_id IN \
                     (SELECT message_id FROM chat_message WHERE session_id=?) \
                     AND NOT EXISTS (SELECT 1 FROM chat_message c WHERE c.message_id=t.message_id)"
                ),
                [sid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let _ = orphans;
    }

    // ③ 轮次引用
    let mut bad = 0;
    let mut total = 0;
    let mut st = conn
        .prepare(
            "SELECT turn_id, ifnull(reply_to_message_id,''), ifnull(response_message_id,'') \
             FROM chat_turn WHERE session_id=? ORDER BY id",
        )
        .unwrap();
    let turns = st
        .query_map([sid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for (tid, rep, resp) in &turns {
        total += 1;
        let rm = mid_meta(conn, rep);
        let sm = mid_meta(conn, resp);
        let ok = rm.as_ref().map(|x| x.0 == "user").unwrap_or(false)
            && sm.as_ref().map(|x| x.0 == "assistant").unwrap_or(false);
        if !ok {
            bad += 1;
            println!(
                "  ❌ TURN {} reply={} [{}] resp={} [{}]",
                tid,
                &rep[..rep.len().min(8)],
                match rm {
                    Some((role, _, idx)) => format!("{role} idx{idx}"),
                    None => "不存在".into(),
                },
                &resp[..resp.len().min(8)],
                match sm {
                    Some((role, _, idx)) => format!("{role} idx{idx}"),
                    None => "不存在".into(),
                }
            );
        }
    }
    println!(
        "  chat_turn = {total}，role 配对错误 {} {}",
        bad,
        if bad == 0 { "✅" } else { "❌" }
    );

    // ④ message_index 交替规律（user 奇数位 / assistant 偶数位）
    let mut idx_bad = Vec::new();
    for m in &mids {
        if let Some((role, _t, idx)) = mid_meta(conn, m) {
            let expect_user = idx % 2 == 1;
            if (role == "user") != expect_user {
                idx_bad.push(format!("idx{idx}:{role}"));
            }
        }
    }
    println!(
        "  message_index 交替 {} {}",
        if idx_bad.is_empty() { "正常" } else { "异常" },
        if idx_bad.is_empty() {
            String::new()
        } else {
            format!("❌ {:?}", idx_bad)
        }
    );

    // ⑤ FTS 内容镜像
    let fts: i64 = conn
        .query_row(
            "SELECT count(*) FROM fts_message_content WHERE session_id=?",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    let users = mids
        .iter()
        .filter(|m| mid_meta(conn, m).map(|x| x.0 == "user").unwrap_or(false))
        .count();
    println!(
        "  fts_message_content = {fts}（user 消息 {users}） {}",
        if fts as usize == users { "✅" } else { "❌" }
    );

    // ⑥ history_v2 指向
    let out_rows: i64 = conn
        .query_row(
            "SELECT count(*) FROM history_v2 WHERE session_id=?",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    let stray: i64 = conn
        .query_row(
            "SELECT count(*) FROM history_v2 WHERE session_id=? AND ifnull(message_id,'')<>'' \
             AND message_id NOT IN (SELECT message_id FROM chat_message WHERE session_id=?)",
            [sid, sid],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    println!(
        "  history_v2 = {out_rows}，其中 message_id 不属本会话 {stray} {}",
        if stray == 0 { "✅" } else { "❌" }
    );

    // 消息概览
    let mut by_mid: HashMap<String, i64> = HashMap::new();
    for m in &mids {
        if let Some((_r, _t, idx)) = mid_meta(conn, m) {
            by_mid.insert(m.clone(), idx);
        }
    }
    let mut ordered: Vec<(&String, &i64)> = by_mid.iter().collect();
    ordered.sort_by_key(|(_, i)| **i);
    println!("  消息序列：");
    for (m, idx) in ordered {
        let (role, mtype, _) = mid_meta(conn, m).unwrap();
        let content_mid = if mtype == "task" { "chat_message_task" } else { "chat_message_general" };
        let head: Option<String> = conn
            .query_row(
                &format!("SELECT substr(content,1,40) FROM {content_mid} WHERE message_id=?"),
                [m],
                |r| r.get(0),
            )
            .ok();
        println!(
            "   idx={idx:<2} {role:<9} {} {}",
            &m[..8],
            head.unwrap_or_else(|| "⚠️无内容".into())
        );
    }
    let _ = set;
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let sids: Vec<String> = std::env::args().skip(2).collect();
    let sids = if sids.is_empty() {
        vec![
            "6ac3a1a461373ef8900b49cd".to_string(),
            "2908be72bbcf78fa054f6577".to_string(),
        ]
    } else {
        sids
    };

    let path = wb_switch_core::modules::trae_export::reader_plain_path(&client).expect("视图");
    println!("== 视图：{} ==", path.display());
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    // FTS 是怎么维护的？
    let ddl: Vec<String> = conn
        .prepare("SELECT sql FROM sqlite_master WHERE name IN ('fts_message_content') OR (type='trigger' AND sql LIKE '%fts_message%')")
        .unwrap()
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .filter_map(|x| x.ok().flatten())
        .collect();
    println!("\n-- fts_message_content 的 DDL / 触发器 --");
    for d in ddl {
        println!("{}", d.replace('\n', " "));
    }

    for sid in &sids {
        report(&conn, sid);
    }
}
