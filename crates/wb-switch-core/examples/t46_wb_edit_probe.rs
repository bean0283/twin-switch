//! T46 · S4：WorkBuddy 会话编辑器真机探针（**只读**，绝不写库、不写正文）。
//!
//! 五件事：
//! 1. 列出本机 WorkBuddy 会话，挑一条（默认字节数最小的一条，便于快速跑完）；
//! 2. `edit_doc` 读元数据 + 回合索引，打印轮数 / 行数 / 各类型计数；
//! 3. `edit_lines` 取第 1 个回合的完整行，打印每段的可读正文前 60 字；
//! 4. `validate_submit` 跑三组校验：无改动 / 删光用户提问 / 改一个非法字段；
//! 5. 把 doc 落到 `--out` 指定的文件（默认 `t46-wb-doc.json`），供渲染自查当真实数据。
//!
//! 用法：
//! `cargo run -p wb-switch-core --example t46_wb_edit_probe -- [uid] [cid] [out.json]`

use serde_json::{json, Value};
use wb_switch_core::modules::{workbuddy_session_edit as edit, workbuddy_sessions, workbuddy_source};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "t46-wb-doc.json".to_string());

    let list = match workbuddy_sessions::list_by_account() {
        Value::Object(o) => o,
        _ => {
            println!("[probe] list_by_account 返回了非对象");
            std::process::exit(2);
        }
    };
    let accounts = list.get("accounts").and_then(Value::as_array).cloned().unwrap_or_default();
    if accounts.is_empty() {
        println!("[probe] 本机没有 WorkBuddy 会话：{:?}", list.get("error"));
        std::process::exit(2);
    }

    // 选会话：显式传 uid/cid 就用传的；否则挑「有正文且字节数最小」的一条
    let (uid, cid) = if args.len() >= 3 && !args[1].is_empty() && !args[2].is_empty() {
        (args[1].clone(), args[2].clone())
    } else {
        let mut best: Option<(String, String, u64)> = None;
        for acc in &accounts {
            let u = acc.get("uid").and_then(Value::as_str).unwrap_or("").to_string();
            for s in acc.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default() {
                let id = s.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let bytes = s.get("bodyBytes").and_then(Value::as_u64).unwrap_or(0);
                let has = s.get("hasBody").and_then(Value::as_bool).unwrap_or(false);
                if !has || bytes == 0 {
                    continue;
                }
                if best.as_ref().map(|b| bytes < b.2).unwrap_or(true) {
                    best = Some((u.clone(), id, bytes));
                }
            }
        }
        match best {
            Some((u, id, _)) => (u, id),
            None => {
                println!("[probe] 所有会话都没有正文文件");
                std::process::exit(2);
            }
        }
    };
    println!("[probe] 目标会话：uid={uid}\ncid={cid}");

    // --- 1) edit_doc ---
    let doc = match edit::edit_doc(&uid, &cid) {
        Ok(v) => v,
        Err(e) => {
            println!("[probe] edit_doc 失败：{e}");
            std::process::exit(3);
        }
    };
    println!(
        "[probe] 标题 = {}",
        doc.get("title").and_then(Value::as_str).unwrap_or("")
    );
    println!("[probe] 正文 = {:?}", doc.get("body"));
    println!("[probe] counts = {:?}", doc.get("counts"));
    let turns = doc.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
    let head = doc.get("head").and_then(Value::as_array).cloned().unwrap_or_default();
    println!("[probe] 回合数 = {}，头部行 = {:?}", turns.len(), head);
    for (i, t) in turns.iter().take(3).enumerate() {
        println!(
            "  #{i} 行[{}, {}) 用户「{}」答复「{}」工具{:?} 状态={}",
            t["start"], t["end"], t["userText"], t["replyText"], t["tools"], t["status"]
        );
    }
    for w in doc.get("warnings").and_then(Value::as_array).cloned().unwrap_or_default() {
        println!("[probe] warning: {w}");
    }

    // --- 2) edit_lines（第 1 个回合） ---
    if let Some(t0) = turns.first() {
        let from = t0["start"].as_u64().unwrap_or(0) as usize;
        let full_to = t0["end"].as_u64().unwrap_or(0) as usize;
        // 渲染自查用：把第 1 个回合的**完整行**单独落一个文件，
        // 前端渲染时由宿主桩把 `workbuddy_session_edit_lines` 指向它。
        if let Ok(full) = edit::edit_lines(&uid, &cid, from, full_to.min(from + 400)) {
            let path = format!("{out}.lines.json");
            if std::fs::write(&path, serde_json::to_string(&full).unwrap_or_default()).is_ok() {
                println!("[probe] 第 1 个回合完整行已写出：{path}");
            }
        }
        let to = full_to.min(from + 40);
        match edit::edit_lines(&uid, &cid, from, to) {
            Ok(v) => {
                for l in v["lines"].as_array().cloned().unwrap_or_default() {
                    let text = l["text"].as_str().unwrap_or("");
                    let short: String = text.chars().take(60).collect();
                    println!(
                        "  [{}] {:<12} {:<8} path={} text={}",
                        l["index"],
                        l["kind"].as_str().unwrap_or(""),
                        l["name"].as_str().unwrap_or(""),
                        l["textPath"],
                        short.replace('\n', " ")
                    );
                }
            }
            Err(e) => println!("[probe] edit_lines 失败：{e}"),
        }
    }

    // --- 3) validate_submit ---
    let summary = json!({
        "lineCount": doc["body"]["lineCount"],
        "counts": doc["counts"],
        "lineKinds": [],
    });
    let cols = doc["columns"].clone();
    let cases: Vec<(&str, Value, Value)> = vec![
        ("无改动", json!([]), json!({})),
        (
            "改主键 id（应拦）",
            json!([]),
            json!({ "id": "00000000-0000-4000-8000-000000000000" }),
        ),
        (
            "塞不存在的列（应拦）",
            json!([]),
            json!({ "not_a_column": 1 }),
        ),
        (
            "新增一行的 sessionId 写错（应拦）",
            json!([{ "op": "append", "json": { "type": "message", "role": "assistant", "sessionId": "别处的会话", "content": [{"type":"output_text","text":"x"}] } }]),
            json!({}),
        ),
    ];
    for (name, ops, patch) in cases {
        let r = edit::validate_submit(&ops, &patch, &cols, &summary, &cid);
        println!(
            "[probe] 校验「{name}」→ errors={} warnings={} 首条={}",
            r["errorCount"],
            r["warnCount"],
            r["errors"].as_array().and_then(|a| a.first()).and_then(Value::as_str).unwrap_or("—")
        );
    }

    // --- 4) 落盘 doc（渲染自查用） ---
    match std::fs::write(&out, serde_json::to_string_pretty(&doc).unwrap_or_default()) {
        Ok(_) => println!("[probe] doc 已写出：{out}"),
        Err(e) => println!("[probe] 写 doc 失败：{e}"),
    }

    // --- 5) 顺带看一眼正文文件本体路径（只读） ---
    if let Some(p) = workbuddy_source::find_jsonl(&cid) {
        println!("[probe] 正文文件：{}", p.display());
    }
}
