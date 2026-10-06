//! 只读探针：核对 WorkBuddy 账号链路在本机的真实落点。
//!
//! 用法：`cargo run -p wb-switch-core --example wb_accounts_probe`
//!
//! 只读：不写任何文件，不碰账号库，只打印本机现状。

use wb_switch_core::modules::{workbuddy_auth, workbuddy_switch, workbuddy_vault};

fn main() {
    println!("=== 登录态 ===");
    let path = workbuddy_auth::auth_file_path();
    println!("路径 : {}", path.display());
    println!("存在 : {}", path.exists());
    println!("已登录: {}", workbuddy_auth::is_logged_in());
    match workbuddy_auth::read_auth_file() {
        Some(root) => {
            println!("当前 uid: {:?}", workbuddy_auth::current_uid_of(&root));
            let keys: Vec<String> = root
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            println!("顶层字段: {keys:?}");
            if let Some(all) = root.get("allAccounts").and_then(|v| v.as_array()) {
                println!("allAccounts 条数: {}", all.len());
                for (i, a) in all.iter().take(10).enumerate() {
                    let uid = a.get("uid").and_then(|v| v.as_str()).unwrap_or("-");
                    let name = a
                        .get("nickname")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("(无昵称)");
                    println!("  [{i}] uid={uid} nickname={name}");
                }
            }
        }
        None => println!("当前 uid: 无（未读到登录态）"),
    }

    println!();
    println!("=== 账号库（本工具自建） ===");
    println!("路径 : {}", workbuddy_vault::accounts_file().display());
    let list = workbuddy_vault::list();
    let accounts = list.get("accounts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    println!("账号数: {}", accounts.len());
    for a in &accounts {
        println!(
            "  - {} | uid={} | 当前={} | 有凭据={}",
            a.get("name").and_then(|v| v.as_str()).unwrap_or("-"),
            a.get("uid").and_then(|v| v.as_str()).unwrap_or("-"),
            a.get("isCurrent").and_then(|v| v.as_bool()).unwrap_or(false),
            a.get("hasToken").and_then(|v| v.as_bool()).unwrap_or(false),
        );
    }

    println!();
    println!("=== 客户端进程 ===");
    println!("运行中: {}", workbuddy_switch::is_running());
    println!("进程数: {}", workbuddy_switch::list_processes().len());
    println!("exe   : {:?}", workbuddy_switch::resolve_exe());

    println!();
    println!("=== 会话数据（为会话记录页预留） ===");
    let db = workbuddy_auth::workbuddy_db_path();
    println!("workbuddy.db: {}（存在={}）", db.display(), db.exists());
    println!("projects 目录: {}（存在={}）", workbuddy_auth::projects_dir().display(), workbuddy_auth::projects_dir().exists());
}
