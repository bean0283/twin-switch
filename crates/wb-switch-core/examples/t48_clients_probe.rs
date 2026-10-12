//! 只读探针：打印 Trae 客户端发现结果（与 `trae_list_clients` 命令同源）。
//!
//! 用于排查「Trae 客户端升级后工具认不到」这类问题：把「数据目录在不在」
//! 「主程序找没找到」「进程名是什么」三件事分开列出来 —— 它们中的任何一个
//! 对不上，都会让上层功能失效，但症状完全不同。
//!
//! ```bash
//! cargo run -p wb-switch-core --example t48_clients_probe
//! ```

use wb_switch_core::modules::{client_usage, trae_discover};

fn main() {
    let mut clients = trae_discover::list_installed_clients();
    println!("候选客户端 {} 个\n", clients.len());
    for c in &clients {
        println!("──────── {} ({})", c.key, c.label);
        println!("  installed      = {}", c.installed);
        println!("  runnable       = {}   ← 数据目录在 **且** 找得到主程序", c.runnable);
        println!("  merged_into    = {:?}", c.merged_into);
        println!("  has_login      = {}", c.has_login);
        println!("  user_data_dir  = {}", c.user_data_dir);
        println!("  exe            = {}", c.exe.as_deref().unwrap_or("(未找到)"));
        if let Some(cl) = trae_discover::get_client(c.key) {
            println!("  process_names  = {:?}", trae_discover::process_names(cl));
            println!("  remote_host    = {}", trae_discover::remote_host(cl));
            println!("  api_host       = {}", cl.api_host);
            let db = trae_discover::database_path(cl);
            let exists = db.is_file();
            let size = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
            println!("  database.db    = {} ({} MB)",
                     if exists { "有" } else { "无" }, size / 1048576);
            let st = trae_discover::storage_json_path(cl);
            println!("  storage.json   = {}", if st.is_file() { "有" } else { "无" });
        }
        println!();
    }

    // ---- 排序：首屏默认打开谁 ----
    client_usage::sort_installed(&mut clients);
    println!("排序后（界面取第一个）——");
    for (i, c) in clients.iter().enumerate() {
        println!(
            "  {}. {}  runnable={} merged_into={:?}",
            i + 1,
            c.key,
            c.runnable,
            c.merged_into
        );
    }
    let first = clients.first().map(|c| c.key).unwrap_or("(无)");
    println!("\n首屏默认 = {first}");
    println!(
        "判定：{}",
        if clients.first().map(|c| c.runnable).unwrap_or(false) {
            "OK —— 默认落在能跑起来的客户端上"
        } else {
            "BAD —— 默认落在一个跑不起来的客户端上（合并后必须避免）"
        }
    );

    println!("\n额外目录（worktrees / mcps）：");
    for p in trae_discover::extra_roots() {
        println!("  {} -> {}", p.display(), if p.exists() { "有" } else { "无" });
    }
}
