//! 端到端往返自检：在**解密库的临时副本**上完成
//! 「写入 WorkBuddy 会话 → 补 reserved → 加密回写 → 重新解密 → 校验」全链路。
//!
//! 这是导入功能最核心、也最危险的一段（加密回写）的离线验证，
//! 全程只操作临时文件，**不接触任何真实库**。
//!
//! 用法：cargo run -p wb-switch-core --example wb_roundtrip

use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;
use wb_switch_core::modules::{
    config, trae_decrypt, trae_import, trae_memory_scan, workbuddy_import, workbuddy_source,
};

fn main() {
    // 走 config::store_dir()：数据目录名改过一次，手拼路径的示例会在改名后失效。
    let plain_src = config::store_dir().join("trae/decrypted/solo-cn.db");
    if !plain_src.is_file() {
        println!("找不到解密库：{}", plain_src.display());
        return;
    }
    let Some(key) = trae_memory_scan::load_saved_key("solo-cn") else {
        println!("没有 solo-cn 的存盘密钥，请先在应用里执行一次「扫描密钥并解密」");
        return;
    };
    println!("已取得存盘密钥（{} 位）", key.len());

    let work: PathBuf = std::env::temp_dir().join("wb_roundtrip");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();

    // 1) 明文副本（解密的库本身就是标准 SQLite）
    let plain = work.join("plain.db");
    std::fs::copy(&plain_src, &plain).unwrap();
    // 解密库的页头 reserved 可能已是 80；重设一次保证加密前提成立
    trae_import::patch_reserved_field(&plain).expect("补 reserved 字段失败");

    // 2) 写入 WorkBuddy 会话
    let listed = workbuddy_source::list_sessions().expect("列出 WorkBuddy 会话失败");
    let usable: Vec<_> = listed
        .iter()
        .filter(|s| s.has_body && !s.deleted && s.body_bytes > 200)
        .collect();
    let conn = Connection::open(&plain).unwrap();
    let uid: String = conn
        .query_row(
            "SELECT user_id FROM project WHERE ifnull(user_id,'')<>'' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "925158341882023".to_string());
    let log = |m: &str| println!("{m}");
    let mut made: Vec<String> = Vec::new();
    for target in &usable {
        let Ok(body) = workbuddy_source::load_body(&target.id) else {
            continue;
        };
        if body.turns.is_empty() {
            continue;
        }
        let title = body
            .ai_title
            .clone()
            .unwrap_or_else(|| target.title.clone());
        workbuddy_import::write_session(&conn, "solo-cn", &uid, target, &body, &log)
            .expect("写入会话失败");
        let sid: String = conn
            .query_row(
                "SELECT session_id FROM chat_session WHERE session_title=?1 ORDER BY id DESC LIMIT 1",
                rusqlite::params![title],
                |r| r.get(0),
            )
            .unwrap();
        made.push(sid);
    }
    conn.close().unwrap();
    println!("已在明文副本写入 {} 个会话", made.len());

    // 3) 加密回写为 SQLCipher 库
    let sealed = work.join("sealed.db");
    let pages = trae_import::encrypt_db_file(&key, &plain, &sealed, Some(&|m| println!("   {m}")))
        .expect("加密回写失败");
    println!("加密回写 {pages} 页 → {}", sealed.display());

    // 4) 用同一密钥重新解密
    let back = work.join("back.db");
    let rep = trae_decrypt::decrypt_database(&sealed, &key, &back, None).expect("重新解密失败");
    println!("重新解密成功：{} 页 / {} 表", rep.pages, rep.tables.len());

    // 5) 校验：会话与内容都在，且结构不变式成立
    let c = Connection::open(&back).unwrap();
    let mut ok = true;
    for sid in &made {
        let turns: i64 = c
            .query_row(
                "SELECT count(*) FROM chat_turn WHERE session_id=?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        let hv2: i64 = c
            .query_row(
                "SELECT count(*) FROM history_v2 WHERE session_id=?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        let sp: i64 = c
            .query_row(
                "SELECT count(*) FROM session_project WHERE session_id=?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        let ar: i64 = c
            .query_row(
                "SELECT count(*) FROM agent_run WHERE session_id=?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        let orphan: i64 = c
            .query_row(
                "SELECT count(*) FROM chat_message m WHERE m.session_id=?1 \
                 AND NOT EXISTS (SELECT 1 FROM chat_message_general g WHERE g.message_id=m.message_id) \
                 AND NOT EXISTS (SELECT 1 FROM chat_message_task t WHERE t.message_id=m.message_id)",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        let bad_json: i64 = {
            let mut n = 0;
            let mut st = c
                .prepare("SELECT messages FROM history_v2 WHERE session_id=?1")
                .unwrap();
            for row in st
                .query_map(rusqlite::params![sid], |r| r.get::<_, String>(0))
                .unwrap()
                .flatten()
            {
                if serde_json::from_str::<Value>(&row).is_err() {
                    n += 1;
                }
            }
            n
        };
        let pass = turns > 0 && hv2 > 0 && sp == 1 && ar == turns && orphan == 0 && bad_json == 0;
        if !pass {
            ok = false;
        }
        println!(
            "  {} {} → turns={turns} history={hv2} session_project={sp} agent_run={ar} orphan={orphan} bad_json={bad_json}",
            if pass { "OK  " } else { "FAIL" },
            sid
        );
    }

    // 6) 用导出模块的读法确认「可读」
    if let Some(sid) = made.first() {
        let title: String = c
            .query_row(
                "SELECT session_title FROM chat_session WHERE session_id=?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap_or_default();
        println!("\n抽样会话「{title}」的内容行：");
        let mut st = c
            .prepare(
                "SELECT message_role, message_index, message_id FROM chat_message \
                 WHERE session_id=?1 ORDER BY message_index LIMIT 6",
            )
            .unwrap();
        let rows: Vec<(String, i64, String)> = st
            .query_map(rusqlite::params![sid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
            .flatten()
            .collect();
        for (role, idx, mid) in rows {
            if role == "user" {
                let content: String = c
                    .query_row(
                        "SELECT content FROM chat_message_general WHERE message_id=?1",
                        rusqlite::params![mid],
                        |r| r.get(0),
                    )
                    .unwrap_or_default();
                println!("  [{idx}] user  : {}", content.chars().take(90).collect::<String>());
            } else {
                let text: String = c
                    .query_row(
                        "SELECT messages FROM history_v2 WHERE message_id=?1 AND content_source='llm_default' LIMIT 1",
                        rusqlite::params![mid],
                        |r| r.get(0),
                    )
                    .unwrap_or_default();
                let shown = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/raw_messages/0/content/0/text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                println!("  [{idx}] assist: {}", shown.chars().take(90).collect::<String>());
            }
        }
    }

    println!(
        "\n=== 往返自检 {} ===",
        if ok { "全部通过 ✅" } else { "存在失败项 ❌" }
    );
    let _ = std::fs::remove_dir_all(&work);
}
