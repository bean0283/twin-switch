//! Trae → WorkBuddy 导出的离线闭环校验（**不触碰真实的 WorkBuddy 数据**）。
//!
//! 用法：
//! ```text
//! cargo run -p wb-switch-core --example wb_export_probe -- [client_key] [会话id ...]
//! ```
//! 不传会话 id 时，自动挑该客户端里工具步骤最多（信息量最大）的 3 个会话。
//!
//! 校验内容：
//!   1. 生成的事件逐行都是合法 JSON；
//!   2. 用 `workbuddy_source::parse_body`（与 WorkBuddy 读取端**同一套解析**）读回来，
//!      回合数、提问、最终回答、思考条数、工具调用数都能对上；
//!   3. 每个 `function_call` 都有配对的 `function_call_result`；
//!   4. 提问文本是否在往返过程中被改动（被 `<system-reminder>` 规则剥离）。
//!
//! 生成的 JSONL 落在 `%TEMP%/wb_export_probe/`，跑完不删，便于人工翻看。

use serde_json::Value;
use wb_switch_core::modules::{workbuddy_export, workbuddy_source};

fn main() {
    let mut args = std::env::args().skip(1);
    let client_key = args.next().unwrap_or_else(|| "trae-cn".to_string());
    let mut ids: Vec<String> = args.collect();

    if ids.is_empty() {
        println!("未指定会话 id，自动挑选工具步骤最多的 3 个会话（{client_key}）…");
        let listed = match workbuddy_export::list_source(&client_key) {
            Ok(v) => v,
            Err(e) => {
                println!("✗ 列出会话失败：{e}");
                println!("  提示：需要先有该客户端的解密快照（先在界面上执行一次「扫描密钥并解密」）。");
                std::process::exit(2);
            }
        };
        let all: Vec<String> = listed
            .get("sessions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|s| s.get("turns").and_then(Value::as_i64).unwrap_or(0) > 0)
                    .filter_map(|s| s.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        println!("  库内共 {} 个有内容的会话", all.len());
        // 逐个预览以挑出工具步骤最多的（预览是只读的）
        let mut scored: Vec<(usize, String)> = Vec::new();
        for sid in all.iter().take(40) {
            if let Ok(v) = workbuddy_export::preview(&client_key, std::slice::from_ref(sid)) {
                let steps = v
                    .get("preview")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|p| p.get("tool_steps"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                scored.push((steps as usize, sid.clone()));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        ids = scored.into_iter().take(3).map(|(_, s)| s).collect();
    }

    if ids.is_empty() {
        println!("没有可校验的会话。");
        std::process::exit(2);
    }
    println!("校验 {} 个会话：{ids:?}\n", ids.len());

    let conv = match workbuddy_export::convert(&client_key, &ids) {
        Ok(c) => c,
        Err(e) => {
            println!("✗ 转换失败：{e}");
            std::process::exit(1);
        }
    };
    for (sid, why) in &conv.skipped {
        println!("… 跳过 {sid}：{why}");
    }

    let out_dir = std::env::temp_dir().join("wb_export_probe");
    std::fs::create_dir_all(&out_dir).expect("创建输出目录失败");
    println!("输出目录：{}\n", out_dir.display());

    let mut failures = 0usize;
    for c in &conv.items {
        println!("════════ {} ════════", c.title);
        println!(
            "  trae={} → workbuddy={}\n  cwd={}  工作区={}",
            c.trae_id, c.wb_id, c.cwd, c.workspace_key
        );
        println!(
            "  转换结果：{} 回合 / {} 工具步骤 / {} 事件 / {:.1} KB",
            c.turns,
            c.tool_steps,
            c.lines.len(),
            c.bytes() as f64 / 1024.0
        );

        // 1) 逐行合法性
        let mut text = String::new();
        let mut bad_lines = 0usize;
        for l in &c.lines {
            match serde_json::to_string(l) {
                Ok(s) => {
                    text.push_str(&s);
                    text.push('\n');
                }
                Err(_) => bad_lines += 1,
            }
        }
        let path = out_dir.join(format!("{}.jsonl", c.wb_id));
        std::fs::write(&path, &text).expect("写文件失败");
        if bad_lines > 0 {
            println!("  ✗ 有 {bad_lines} 行无法序列化");
            failures += 1;
        }

        // 2) 用 WorkBuddy 的解析器读回来
        let parsed = workbuddy_source::parse_body(&c.wb_id, &text);
        let ok_turns = parsed.turns.len() == c.turns;
        println!(
            "  {} 回合数：解析 {} vs 转换 {}",
            if ok_turns { "✓" } else { "✗" },
            parsed.turns.len(),
            c.turns
        );
        if !ok_turns {
            failures += 1;
        }

        let empty_user = parsed.turns.iter().filter(|t| t.user_text.trim().is_empty()).count();
        let empty_asst = parsed.turns.iter().filter(|t| t.assistant_text.trim().is_empty()).count();
        let expect_empty_asst = c.turns.saturating_sub(c.answered);
        // 空提问一定是转换 bug（Trae 的 user 消息非空才会写）。
        // 空回答则可能是**源会话本身就没有回答**（被中断/重试的回合），因此与
        // 「转换期实际写出回答的回合数」对账，而不是要求必须为 0。
        let empty_ok = empty_user == 0 && empty_asst == expect_empty_asst;
        println!(
            "  {} 空提问 {empty_user} 个 / 空最终回答 {empty_asst} 个（源会话无回答 {expect_empty_asst} 个，写出回答 {} 个）",
            if empty_ok { "✓" } else { "✗" },
            c.answered
        );
        if !empty_ok {
            failures += 1;
        }

        // 3) 事件计数：思考与工具调用
        let reasoning = c.lines.iter().filter(|l| l["type"] == "reasoning").count();
        let calls = c.lines.iter().filter(|l| l["type"] == "function_call").count();
        let results = c.lines.iter().filter(|l| l["type"] == "function_call_result").count();
        let parsed_tools: usize = parsed
            .turns
            .iter()
            .flat_map(|t| t.events.iter())
            .filter(|e| e.kind == "function_call")
            .count();
        let parsed_reasoning: usize = parsed
            .turns
            .iter()
            .flat_map(|t| t.events.iter())
            .filter(|e| e.kind == "reasoning")
            .count();
        println!(
            "  {} 工具调用 {calls}（解析回 {parsed_tools}）/ 结果 {results} / 思考 {reasoning}（解析回 {parsed_reasoning}）",
            if calls == parsed_tools && calls == c.tool_steps && results <= calls {
                "✓"
            } else {
                "✗"
            }
        );
        if calls != c.tool_steps || calls != parsed_tools {
            failures += 1;
        }
        if reasoning != parsed_reasoning {
            failures += 1;
        }

        // 4) 提问文本是否被往返改写（WorkBuddy 的 extract_user_text 会剥离注入块）
        let mut changed = 0usize;
        for (i, t) in parsed.turns.iter().enumerate() {
            let Some(raw) = c
                .lines
                .iter()
                .filter(|l| l["type"] == "message" && l["role"] == "user")
                .nth(i)
                .and_then(|l| l["content"][0]["text"].as_str())
            else {
                continue;
            };
            if raw.trim() != t.user_text.trim() {
                changed += 1;
                if changed == 1 {
                    println!("    · 首例被改写：{:?} → {:?}", &raw[..raw.len().min(80)], &t.user_text[..t.user_text.len().min(80)]);
                }
            }
        }
        println!(
            "  {} 提问文本往返一致性：{changed}/{} 条被改写",
            if changed == 0 { "✓" } else { "!" },
            parsed.turns.len()
        );

        // 样例
        if let Some(t) = parsed.turns.first() {
            println!("  ── 首回合预览 ──");
            println!("     问：{}", t.user_text.chars().take(70).collect::<String>());
            println!("     答：{}", t.assistant_text.chars().take(70).collect::<String>());
            println!("     过程事件 {} 条", t.events.len());
        }
        println!("  文件：{}\n", path.display());
    }

    println!("──────────────────────────────");
    if failures == 0 {
        println!("✅ 全部校验通过");
    } else {
        println!("❌ 有 {failures} 项校验未通过");
        std::process::exit(1);
    }
}
