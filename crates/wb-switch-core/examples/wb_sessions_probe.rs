//! 只读探针：WorkBuddy 会话记录 / 复制预检 / 关联视图。
//!
//! 全程不写任何 WorkBuddy 数据，只读 `workbuddy.db` 快照与关联记录。
//! 用法：`cargo run -p wb-switch-core --example wb_sessions_probe`

use wb_switch_core::modules::{workbuddy_auth, workbuddy_sessions};

fn main() {
    println!("=== 数据根 ===");
    println!("data_root   : {}", workbuddy_auth::data_root().display());
    println!("db          : {}", workbuddy_sessions::db_path().display());
    println!(
        "当前登录 uid: {:?}",
        workbuddy_auth::current_uid().unwrap_or_else(|| "(未登录)".to_string())
    );

    println!("\n=== 云端映射库（只读列出，本工具从不写入） ===");
    for db in workbuddy_sessions::edge_sync_databases() {
        println!(
            "  {}  {} bytes",
            db["name"].as_str().unwrap_or("?"),
            db["size"].as_i64().unwrap_or(0)
        );
    }

    println!("\n=== 按账号分组的会话 ===");
    let by_account = workbuddy_sessions::list_by_account();
    if !by_account["ok"].as_bool().unwrap_or(false) {
        println!("读取失败: {}", by_account["error"]);
        return;
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let accounts = by_account["accounts"].as_array().unwrap_or(&empty);
    let mut uids: Vec<String> = Vec::new();
    for acc in accounts {
        let uid = acc["uid"].as_str().unwrap_or("").to_string();
        let count = acc["count"].as_i64().unwrap_or(0);
        uids.push(uid.clone());
        println!("\n  账号 {uid}  会话 {count}");
        let sessions = acc["sessions"].as_array().unwrap_or(&empty);
        for s in sessions.iter().take(5) {
            println!(
                "    - {} | {} | body={} {}B",
                s["id"].as_str().unwrap_or("?"),
                s["title"].as_str().unwrap_or("?"),
                s["hasBody"].as_bool().unwrap_or(false),
                s["bodyBytes"].as_i64().unwrap_or(0)
            );
        }
        if sessions.len() > 5 {
            println!("    … 其余 {} 条", sessions.len() - 5);
        }
    }

    if uids.len() < 2 {
        println!("\n（本机只有一个账号的会话，跳过复制预检演示）");
        return;
    }

    let (src, dst) = (uids[0].clone(), uids[1].clone());
    let ids: Vec<String> = accounts[0]["sessions"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|s| s["hasBody"].as_bool().unwrap_or(false))
        .take(2)
        .map(|s| s["id"].as_str().unwrap_or("").to_string())
        .collect();

    println!("\n=== 复制预检（只读）: {} → {} ===", src, dst);
    let preview = workbuddy_sessions::copy_preview(&src, &dst, &ids);
    println!("{}", serde_json::to_string_pretty(&preview).unwrap_or_default());

    println!("\n=== 关联视图（只读）: {} ↔ {} ===", src, dst);
    let links = workbuddy_sessions::links_preview(&src, &dst);
    println!("{}", serde_json::to_string_pretty(&links).unwrap_or_default());

    if !ids.is_empty() {
        println!("\n=== 详情（只读）: {} ===", ids[0]);
        let detail = workbuddy_sessions::detail(&src, &ids[0]);
        let turns = detail["turnCount"].as_i64().unwrap_or(0);
        println!("  回合数 {turns}  正文路径 {:?}", detail["bodyPath"]);
        let empty2: Vec<serde_json::Value> = Vec::new();
        for t in detail["turns"].as_array().unwrap_or(&empty2).iter().take(3) {
            let ask: String = t["userText"].as_str().unwrap_or("").chars().take(40).collect();
            let ans: String = t["assistantText"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect();
            println!("    Q: {ask}");
            println!("    A: {ans}");
        }
    }
}
