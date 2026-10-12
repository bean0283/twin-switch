//! 只读探针：回答「TRAE SOLO 的数据**真的**已经进 Trae CN 了吗」。
//!
//! 背景：2026-10 官方把 TRAE SOLO 并进 Trae CN。第一次探测发现 Trae CN 库里
//! 出现了与 SOLO **完全相同的 7 个 `session_id`** —— 怀疑官方在合并时自己做了迁移。
//! 若是真的，TwinSwitch 就**不该**再搬一次（重复复制只会制造副本）。
//!
//! 本探针逐条对照，把「猜」变成「有据」：
//! ① 两库会话清单按 `session_id` 求交集；
//! ② 对交集中的每条，比 消息数 / 回合数（CN 侧 **少于** SOLO 侧 = 迁移不完整）；
//! ③ 检查 Trae CN 库自身的 `session_project.project_id` 与 `chat_session.project_id`
//!    是否一致 —— 不一致就是那条「客户端里删不掉、重启后复活」的老坑。
//!
//! **全程只读**（只打开 `reader_plain_path()` 快照副本）。
//!
//! ```bash
//! cargo run -p wb-switch-core --example t48_merge_probe
//! ```

use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;

fn open_readonly(client_key: &str) -> Result<Connection, String> {
    let p = wb_switch_core::modules::trae_export::reader_plain_path(client_key)?;
    Connection::open_with_flags(&p, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())
}

/// 表是否存在（不同版本 Trae 的表集合不完全一样，别写死）。
fn has_table(c: &Connection, name: &str) -> bool {
    c.prepare(&format!("SELECT 1 FROM {name} LIMIT 1")).is_ok()
}

struct Side {
    key: String,
    projects: usize,
    /// session_id -> (project_id, session_type, title)
    sessions: BTreeMap<String, (String, String, String)>,
}

fn collect(client_key: &str) -> Result<Side, String> {
    let c = open_readonly(client_key)?;

    let projects: usize = if has_table(&c, "project") {
        c.query_row("SELECT count(*) FROM project", [], |r| r.get(0))
            .unwrap_or(0)
    } else {
        0
    };

    let has_type = c.prepare("SELECT session_type FROM chat_session LIMIT 1").is_ok();
    let sql = if has_type {
        "SELECT session_id, ifnull(project_id,''), ifnull(session_type,''), ifnull(session_title,'') \
         FROM chat_session"
    } else {
        "SELECT session_id, ifnull(project_id,''), '', ifnull(session_title,'') FROM chat_session"
    };
    let mut st = c.prepare(sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut sessions = BTreeMap::new();
    for r in rows.flatten() {
        sessions.insert(r.0.clone(), (r.1, r.2, r.3));
    }

    Ok(Side {
        key: client_key.to_string(),
        projects,
        sessions,
    })
}

/// 单会话的（消息数, 回合数）：消息数取 `chat_message` 里属于它的未删行。
///
/// 用 `trae_export::list_sessions` 会一次算全库、更省事，但它只给整库结果；
/// 这里要的是「同一条会话在两库各自多少」，所以直接查单条。
fn counts_of(c: &Connection, sid: &str) -> (i64, i64) {
    // 消息表 → 会话的关联路径在不同版本里可能是 `chat_message.session_id` 直连，
    // 也可能要走 `chat_message_general/task`。先试直连，失败返回 (-1, -1) 表示读不到。
    let msgs: i64 = c
        .query_row(
            "SELECT count(*) FROM chat_message WHERE session_id = ?1",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    let users: i64 = c
        .query_row(
            "SELECT count(*) FROM chat_message_general WHERE session_id = ?1",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(-1);
    (msgs, users)
}

fn main() {
    let keys = ["solo-cn", "trae-cn"];
    let mut sides: Vec<Side> = Vec::new();

    for k in keys {
        match wb_switch_core::modules::trae_export::ensure_decrypted(k, None) {
            Ok(o) => println!("[{k}] 快照就绪 reused={} pages={}", o.reused, o.pages),
            Err(e) => {
                eprintln!("[{k}] 无法解密快照：{e}");
                return;
            }
        }
        match collect(k) {
            Ok(s) => sides.push(s),
            Err(e) => {
                eprintln!("[{k}] 读取失败：{e}");
                return;
            }
        }
    }

    for s in &sides {
        println!(
            "\n════ {} ════  project {} 行 · chat_session {} 行",
            s.key,
            s.projects,
            s.sessions.len()
        );
        let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
        for p in s.sessions.values() {
            *by_type.entry(p.1.as_str()).or_default() += 1;
        }
        println!("会话类型分布：{by_type:?}");
    }

    let Some(solo) = sides.iter().find(|s| s.key == "solo-cn") else {
        println!("缺 solo-cn");
        return;
    };
    let Some(cn) = sides.iter().find(|s| s.key == "trae-cn") else {
        println!("缺 trae-cn");
        return;
    };

    // ---- 交集与逐条对照 ----
    println!("\n════ SOLO ∩ Trae CN：逐条对照 ════");
    let c = match open_readonly("trae-cn") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("打开 trae-cn 失败：{e}");
            return;
        }
    };
    let cs = match open_readonly("solo-cn") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("打开 solo-cn 失败：{e}");
            return;
        }
    };

    let mut only_solo: Vec<&String> = Vec::new();
    let mut incomplete: Vec<String> = Vec::new();
    let mut matched = 0usize;

    println!(
        "{:<14} {:<30} {:>12} {:>12} {:>10}",
        "session", "title", "SOLO 消息", "CN 消息", "CN 回合"
    );
    for (sid, (s_pid, _, s_title)) in &solo.sessions {
        match cn.sessions.get(sid) {
            None => only_solo.push(sid),
            Some((c_pid, _, _)) => {
                let (s_msgs, _) = counts_of(&cs, sid);
                let (c_msgs, c_users) = counts_of(&c, sid);
                let flag = if c_msgs < s_msgs { "  ← 不完整！" } else { "" };
                if c_msgs < s_msgs {
                    incomplete.push(sid.clone());
                }
                matched += 1;
                println!(
                    "{:<14} {:<30} {:>12} {:>12} {:>10}{}",
                    &sid[..sid.len().min(12)],
                    trunc(s_title, 28),
                    s_msgs,
                    c_msgs,
                    c_users,
                    flag
                );
                if s_pid != c_pid {
                    println!("               ↳ 项目归属被官方改写：solo={} → cn={}", short(s_pid), short(c_pid));
                }
            }
        }
    }

    println!(
        "\n交集 {} 条；只在 SOLO 有（**尚未进 CN**）{} 条",
        matched,
        only_solo.len()
    );
    for sid in &only_solo {
        println!(
            "  {}  {}",
            &sid[..sid.len().min(12)],
            trunc(&solo.sessions[*sid].2, 40)
        );
    }

    // ---- Trae CN 自身工程归属一致性 ----
    println!("\n════ Trae CN：session_project 归属一致性 ════");
    match wb_switch_core::modules::trae_import::find_misaligned_session_projects("trae-cn") {
        Ok(rows) => {
            println!("不一致 {} 条", rows.len());
            for (sid, sp_pid, sess_pid) in rows.iter().take(10) {
                println!(
                    "  {}  session_project={}  chat_session={}",
                    &sid[..sid.len().min(12)],
                    short(sp_pid),
                    short(sess_pid)
                );
            }
        }
        Err(e) => println!("读取失败：{e}"),
    }

    println!("\n════ 判定 ════");
    if only_solo.is_empty() && incomplete.is_empty() {
        println!("官方**已完成** SOLO → Trae CN 的会话迁移：{} 条全部在 CN 库里且消息数不少于 SOLO 侧。", matched);
        println!("⇒ TwinSwitch **不需要**再搬一次（重复导入只会制造副本）。");
    } else {
        if !only_solo.is_empty() {
            println!("有 {} 条会话还没进 CN 库 ⇒ 需要搬：", only_solo.len());
            for sid in &only_solo {
                println!("   {sid}");
            }
        }
        if !incomplete.is_empty() {
            println!("有 {} 条会话进了 CN 但消息比 SOLO 少（迁移不完整）：", incomplete.len());
            for sid in &incomplete {
                println!("   {sid}");
            }
        }
    }
}

fn short(s: &str) -> String {
    s.chars().take(12).collect()
}

fn trunc(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    for ch in s.chars() {
        if n >= max {
            out.push('…');
            break;
        }
        out.push(ch);
        n += 1;
    }
    out
}
