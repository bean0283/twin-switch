//! 只读探针：打印「Trae 清理」页扫描到的全部可清理项与体积。
//!
//! 用于核对扫描结果与真机是否一致（**绝不删除任何东西**）。
//!
//! ```bash
//! cargo run -p wb-switch-core --example clean_scan
//! ```

use serde_json::Value;

fn main() {
    let v = match wb_switch_core::modules::trae_cleanup::scan() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("扫描失败：{e}");
            return;
        }
    };
    println!("工具目录：{}", v["store_root"].as_str().unwrap_or(""));
    println!("回收站  ：{}", v["trash_root"].as_str().unwrap_or(""));
    println!(
        "可回收合计：{:.1} MB",
        v["total_mb"].as_f64().unwrap_or(0.0)
    );

    for note in v["notes"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        println!("  注意：{}", note.as_str().unwrap_or(""));
    }

    for cat in v["categories"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        println!(
            "\n══ {} ══  {} 项 / {:.1} MB（推荐 {} 项）",
            cat["title"].as_str().unwrap_or(""),
            cat["count"].as_u64().unwrap_or(0),
            cat["bytes_mb"].as_f64().unwrap_or(0.0),
            cat["recommended_count"].as_u64().unwrap_or(0),
        );
        println!("   {}", cat["desc"].as_str().unwrap_or(""));
        let items = cat["items"].as_array().cloned().unwrap_or_default();
        // 按体积降序，只打前 40 条（会话可能上百条）
        let mut list: Vec<&Value> = items.iter().collect();
        list.sort_by(|a, b| {
            b["bytes"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["bytes"].as_u64().unwrap_or(0))
        });
        for it in list.iter().take(40) {
            println!(
                "   [{}] {:<52} {:>10.1} MB  {}",
                if it["recommended"] == Value::Bool(true) { "荐" } else { "  " },
                trunc(&it["title"].as_str().unwrap_or(""), 52),
                it["bytes_mb"].as_f64().unwrap_or(0.0),
                if it["needs_client_stop"] == Value::Bool(true) { "需关客户端" } else { "" },
            );
        }
        if list.len() > 40 {
            println!("   … 另有 {} 项未显示", list.len() - 40);
        }
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}
