//! 真机只读核查：账号显示名 / 同标题副本新旧判定 / 可清理垃圾扫描。
//!
//! **全程只读**——只调用 `scan()`、`list_sessions()`、`resolve()`，不写任何文件。
//!
//! ```bash
//! cargo run -p wb-switch-core --example wb_audit
//! ```

use wb_switch_core::modules::{workbuddy_accounts, workbuddy_cleanup, workbuddy_source};

fn human(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1048576.0)
    } else {
        format!("{:.2} GB", bytes as f64 / 1073741824.0)
    }
}

fn fmt(ms: i64) -> String {
    if ms <= 0 {
        return "—".to_string();
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

fn main() {
    println!("数据根：{}", workbuddy_source::data_root().display());
    println!("可用：{}", workbuddy_source::is_available());

    // ---------------------------------------------------------------
    println!("\n══════ 1. 账号显示名 ══════");
    let accounts = workbuddy_accounts::resolve();
    println!("解析出 {} 个账号", accounts.len());
    for a in &accounts {
        let meta = a.meta();
        println!(
            "  {}  | 次要：{:10} | 来源：{}{}",
            a.label(),
            if meta.is_empty() { "—".to_string() } else { meta },
            a.source,
            if a.is_primary { " | 当前登录" } else { "" }
        );
    }
    println!("  尾号对照：");
    for uid in workbuddy_accounts::known_uids() {
        println!("    …{}  ←→  {}", &uid[uid.len() - 6..], workbuddy_accounts::label_for(&uid));
    }

    // ---------------------------------------------------------------
    println!("\n══════ 2. 会话列表 + 同标题副本判定 ══════");
    match workbuddy_source::list_sessions() {
        Err(e) => println!("读取失败：{e}"),
        Ok(list) => {
            println!("共 {} 条（含已删除）\n", list.len());
            for s in &list {
                let tag = if s.dup_group.is_empty() {
                    "唯一".to_string()
                } else if s.is_newest {
                    "★最新".to_string()
                } else {
                    "较旧".to_string()
                };
                println!(
                    "  [{tag}] {:<34} {} · {} · {} 行 · {}",
                    s.title.chars().take(32).collect::<String>(),
                    fmt(s.updated_at),
                    workbuddy_accounts::label_for(&s.user_id),
                    s.body_lines,
                    if s.has_body { human(s.body_bytes) } else { "无正文".to_string() }
                );
                if !s.dup_note.is_empty() {
                    println!("        └─ {}", s.dup_note);
                }
                if !s.content_digest.is_empty() {
                    println!("        └─ 内容摘要 {}", s.content_digest);
                }
            }
            let dups = list.iter().filter(|s| !s.dup_group.is_empty()).count();
            println!("\n  同标题重复副本：{dups} 条；最新 {} 条", list.iter().filter(|s| s.is_newest && !s.dup_group.is_empty()).count());
        }
    }

    // ---------------------------------------------------------------
    println!("\n══════ 3. 可清理项扫描（只读）══════");
    match workbuddy_cleanup::scan() {
        Err(e) => println!("扫描失败：{e}"),
        Ok(v) => {
            let total_count = v["totals"]["count"].as_u64().unwrap_or(0);
            let total_bytes = v["totals"]["bytes"].as_u64().unwrap_or(0);
            println!(
                "会话表 {} 条；可清理 {} 项，合计 {}（其中推荐清理 {} 项 / {}，保留窗口 {} 天）",
                v["totals"]["sessions"].as_u64().unwrap_or(0),
                total_count,
                human(total_bytes),
                v["totals"]["recommended_count"].as_u64().unwrap_or(0),
                human(v["totals"]["recommended_bytes"].as_u64().unwrap_or(0)),
                v["log_keep_days"].as_i64().unwrap_or(0),
            );
            println!("\n  ── 占用大头（只读说明）──");
            for h in v["large_holdings"].as_array().cloned().unwrap_or_default() {
                println!(
                    "    {:>10}  {:<10} {}{}",
                    human(h["bytes"].as_u64().unwrap_or(0)),
                    h["name"].as_str().unwrap_or(""),
                    h["note"].as_str().unwrap_or(""),
                    if h["cleanable"].as_bool().unwrap_or(false) { "  ← 可清" } else { "" }
                );
            }
            for cat in v["categories"].as_array().cloned().unwrap_or_default() {
                let c = cat["count"].as_u64().unwrap_or(0);
                let b = cat["bytes"].as_u64().unwrap_or(0);
                println!(
                    "\n  ▸ {}（{}）— {} 项 / {}",
                    cat["title"].as_str().unwrap_or(""),
                    cat["key"].as_str().unwrap_or(""),
                    c,
                    human(b)
                );
                println!("    {}", cat["desc"].as_str().unwrap_or(""));
                for it in cat["items"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "      · {:<34} {:>10}  {}  {}",
                        it["title"].as_str().unwrap_or("").chars().take(32).collect::<String>(),
                        human(it["bytes"].as_u64().unwrap_or(0)),
                        it["owner"].as_str().unwrap_or("-"),
                        it["detail"].as_str().unwrap_or("")
                    );
                    for p in it["paths"].as_array().cloned().unwrap_or_default() {
                        println!("          → {}", p.as_str().unwrap_or(""));
                    }
                }
            }
            println!("\n  回收站：{}", v["trash"]["dir"].as_str().unwrap_or(""));
        }
    }
}
