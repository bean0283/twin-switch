//! T46 会话编辑器：真机只读探针。
//!
//! 做三件事（**全程只读，绝不写库**）：
//! 1. 列出真实会话，挑一条（默认取消息最多的一条，便于看全字段）；
//! 2. 调 `edit_doc` 把它的全部相关表读出来，打印每张表的行数与列数；
//! 3. 调 `validate` 跑一遍分级校验，打印 errors / warnings；
//!    并把 doc 落到文件，供渲染自查当真实数据用。
//!
//! 用法：`cargo run -p wb-switch-core --example t46_edit_probe -- [clientKey] [sessionId] [out.json]`

use wb_switch_core::modules::{trae_export, trae_session_edit};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let client = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "trae-cn".to_string());
    let out = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "t46-doc.json".to_string());

    let sessions = match trae_export::list_sessions(&client) {
        Ok(v) => v,
        Err(e) => {
            println!("[probe] list_sessions 失败：{e}");
            std::process::exit(2);
        }
    };
    println!("[probe] 客户端 {client}，会话数 {}", sessions.len());
    let mut sorted: Vec<_> = sessions.iter().collect();
    sorted.sort_by_key(|s| -s.messages);
    for s in sorted.iter().take(8) {
        println!(
            "   {}  消息 {:>5}  轮 {:>4}  {}",
            s.id,
            s.messages,
            s.turns,
            if s.title.is_empty() { "（无标题）" } else { &s.title }
        );
    }

    let sid = match args.get(2).cloned() {
        Some(x) if !x.is_empty() => x,
        _ => match sorted.first() {
            Some(s) => s.id.clone(),
            None => {
                println!("[probe] 没有会话可读");
                std::process::exit(3);
            }
        },
    };
    println!("\n[probe] 读取会话 {sid}");

    let t0 = std::time::Instant::now();
    let doc = match trae_session_edit::edit_doc(&client, &sid) {
        Ok(d) => d,
        Err(e) => {
            println!("[probe] edit_doc 失败：{e}");
            std::process::exit(4);
        }
    };
    println!("[probe] edit_doc 用时 {} ms", t0.elapsed().as_millis());
    println!(
        "[probe] 标题={:?} 归属={:?}",
        doc["title"].as_str().unwrap_or(""),
        doc["ownerUid"].as_str().unwrap_or("(none)")
    );

    let tables = doc["tables"].as_array().cloned().unwrap_or_default();
    let mut total_rows = 0usize;
    println!("[probe] 表 {} 张：", tables.len());
    for t in &tables {
        let name = t["name"].as_str().unwrap_or("?");
        let scope = t["scope"].as_str().unwrap_or("?");
        let rows = t["rows"].as_array().map(|a| a.len()).unwrap_or(0);
        let cols = t["columns"].as_array().map(|a| a.len()).unwrap_or(0);
        let json_cols: Vec<&str> = t["columns"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|c| c["json"].as_bool().unwrap_or(false))
                    .filter_map(|c| c["name"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        let refs = t["refCount"].as_u64().unwrap_or(0);
        total_rows += rows;
        if rows > 0 || !json_cols.is_empty() {
            println!(
                "   {name:<26} [{scope:<7}] {rows:>4} 行 / {cols:>3} 列{}{}",
                if refs > 0 {
                    format!("  [{refs} 个大字段→引用]")
                } else {
                    String::new()
                },
                if json_cols.is_empty() {
                    String::new()
                } else {
                    format!("  JSON 列: {}", json_cols.join(","))
                }
            );
        }
    }
    println!("[probe] 合计 {total_rows} 行，大字段引用 {}", doc["refCount"]);

    // 抽一行看 JSON 列是否真的解成了树（而不是一段字符串）
    if let Some(t) = tables.iter().find(|t| t["name"] == "chat_message_general") {
        if let Some(r) = t["rows"].as_array().and_then(|a| a.first()) {
            let c = &r["content"];
            println!(
                "[probe] chat_message_general.content 类型 = {}",
                if c.is_array() {
                    "数组（已解成树 ✓）"
                } else if c.is_string() {
                    "字符串（未解析 ✗）"
                } else {
                    "其它"
                }
            );
        }
    }

    let rep = trae_session_edit::validate(&doc);
    let errs = rep["errors"].as_array().cloned().unwrap_or_default();
    let warns = rep["warnings"].as_array().cloned().unwrap_or_default();
    println!(
        "\n[probe] 校验 ok={} errors={} warnings={}",
        rep["ok"],
        errs.len(),
        warns.len()
    );
    for e in errs.iter().take(12) {
        println!("   E {}", e.as_str().unwrap_or(""));
    }
    for w in warns.iter().take(12) {
        println!("   W {}", w.as_str().unwrap_or(""));
    }

    // 再跑一次：edit_doc 必须是幂等的（第二次应命中快照，明显更快）
    let t1 = std::time::Instant::now();
    match trae_session_edit::edit_doc(&client, &sid) {
        Ok(d2) => {
            let same = d2 == doc;
            println!(
                "[probe] 二次读取用时 {} ms，内容一致={}",
                t1.elapsed().as_millis(),
                same
            );
        }
        Err(e) => println!("[probe] 二次读取失败：{e}"),
    }

    match std::fs::write(&out, serde_json::to_string(&doc).unwrap()) {
        Ok(_) => println!("[probe] doc 已写出：{out}"),
        Err(e) => println!("[probe] 写文件失败：{e}"),
    }
}
