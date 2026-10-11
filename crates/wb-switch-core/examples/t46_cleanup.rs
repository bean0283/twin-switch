//! T46 清理工具：把探针（或任何异常中断）留在库里的**多余会话**删掉。
//!
//! 用法：
//! ```text
//! cargo run -p wb-switch-core --example t46_cleanup -- <clientKey> <sessionId> [<sessionId>…] [--yes]
//! ```
//! 不带 `--yes` 只做只读预检（确认这些会话确实存在、标题是什么），不做任何写入。
//!
//! 走的是产品同一条删除链路 [`trae_delete::delete_sessions`]：解密 → 合并 WAL →
//! 删行 → 增量回写 → 备份 → 原子替换 → 重启客户端。

use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let confirm = args.iter().any(|a| a == "--yes");
    let rest: Vec<String> = args.into_iter().filter(|a| a != "--yes").collect();
    if rest.len() < 2 {
        eprintln!("用法: t46_cleanup <clientKey> <sessionId> [<sessionId>…] [--yes]");
        std::process::exit(2);
    }
    let client_key = rest[0].clone();
    let ids: Vec<String> = rest[1..].to_vec();

    println!("[cleanup] 客户端 {client_key}，目标会话 {} 条", ids.len());
    for id in &ids {
        println!("  - {id}");
    }

    if !confirm {
        println!("[cleanup] 预检模式（未写库）。加 --yes 才真正删除。");
    }

    let log = |m: &str| println!("   {m}");
    let out: Result<Value, String> = trae_session_edit_cleanup(&client_key, &ids, confirm, &log);
    match out {
        Ok(v) => {
            println!("[cleanup] 结果：{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }
        Err(e) => {
            eprintln!("[cleanup] 失败：{e}");
            std::process::exit(1);
        }
    }
}

/// 薄封装：非确认模式只读（调 `edit_doc` 确认会话存在），确认模式才调删除。
fn trae_session_edit_cleanup(
    client_key: &str,
    ids: &[String],
    confirm: bool,
    log: &dyn Fn(&str),
) -> Result<Value, String> {
    use wb_switch_core::modules::trae_session_edit;
    let mut seen = Vec::new();
    for id in ids {
        match trae_session_edit::edit_doc(client_key, id) {
            Ok(doc) => {
                log(&format!(
                    "存在：{id} 《{}》",
                    doc["title"].as_str().unwrap_or("(无标题)")
                ));
                seen.push(id.clone());
            }
            Err(e) => log(&format!("不存在 / 读取失败：{id} —— {e}")),
        }
    }
    if seen.is_empty() {
        return Err("没有任何会话可删（都读不到）".into());
    }
    if !confirm {
        return Ok(serde_json::json!({
            "dryRun": true,
            "wouldDelete": seen,
        }));
    }
    trae_session_edit::edit_delete_sessions(client_key, &seen, Some(log))
}
