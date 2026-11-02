//! 抓一段 Qoder 上游 SSE 原文（诊断用）：看它到底有没有下发 `usage`。
//!
//! ```text
//! cargo run --release --example wb_qoder_frames -- cn
//! ```
//!
//! 背景：网关指标里 Qoder 请求的 `prompt_tokens/completion_tokens` 一直是 0，
//! 而"0"和"上游没给"在账上必须能分开。这个工具把原始帧打出来，一眼看清是哪种。

use futures_util::StreamExt;
use serde_json::json;
use wb_switch_core::modules::qoder_accounts;
use wb_switch_core::modules::qoder_realm::QoderRegion;
use wb_switch_core::modules::qoder_tools;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let region = match std::env::args().nth(1).as_deref() {
        Some("cn") => QoderRegion::Cn,
        _ => QoderRegion::Global,
    };
    let accounts = qoder_accounts::list(region).unwrap_or_default();
    let Some(acc) = accounts.into_iter().find(|a| a.is_usable()) else {
        println!("没有可用账号");
        return;
    };
    // `--pad <KB>` 把对话填充到指定大小，`--tools <N>` 造 N 个工具 —— 用来验证
    // 「上游是不是按请求体积/工具数拒绝」（2026-11-02：dsh 的 1MB 请求稳定 provider_error）。
    let pad_kb: usize = std::env::args()
        .position(|a| a == "--pad")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let tool_n: usize = std::env::args()
        .position(|a| a == "--tools")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    // `--history <N>`：造 N 轮「assistant tool_calls + tool 结果」，模拟真实 agent 历史。
    let hist: usize = std::env::args()
        .position(|a| a == "--history")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let pad = "内容".repeat(pad_kb * 1024 / 6);
    if hist > 0 {
        let mut msgs: Vec<serde_json::Value> = vec![json!({ "role": "system", "content": "你是助手。" })];
        for i in 0..hist {
            msgs.push(json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": format!("call_{i}"),
                    "type": "function",
                    "function": { "name": "tool_0", "arguments": "{\"path\":\"a.txt\"}" }
                }]
            }));
            msgs.push(json!({
                "role": "tool",
                "tool_call_id": format!("call_{i}"),
                "content": "结果".repeat(20)
            }));
        }
        msgs.push(json!({ "role": "user", "content": "只回复两个字：收到" }));
        println!("历史 {} 轮（{} 条消息）", hist, msgs.len());
        // 直接用这份消息跑完整体验
        let tools = json!((0..36).map(|i| json!({
            "type": "function",
            "function": {
                "name": format!("tool_{i}"),
                "description": "做点事情".repeat(6),
                "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }
            }
        })).collect::<Vec<_>>());
        match qoder_tools::chat_complete_ex(region, &acc, "gmodel", "GLM-5.3-Flash", &json!(msgs), Some(&tools)).await {
            Ok(v) => println!("完成：usage={}", v.get("usage").map(|u| u.to_string()).unwrap_or_default()),
            Err(e) => println!("失败：{}", e.chars().take(300).collect::<String>()),
        }
        return;
    }
    let messages = if pad_kb == 0 {
        json!([{ "role": "user", "content": "只回复两个字：收到" }])
    } else {
        json!([
            { "role": "system", "content": "你是助手。" },
            { "role": "user", "content": pad },
            { "role": "user", "content": "只回复两个字：收到" }
        ])
    };
    let tools = if tool_n == 0 {
        json!([])
    } else {
        json!((0..tool_n)
            .map(|i| json!({
                "type": "function",
                "function": {
                    "name": format!("tool_{i}"),
                    "description": "做点事情".repeat(6),
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }
                }
            }))
            .collect::<Vec<_>>())
    };
    println!(
        "请求体约 {} KB，工具 {} 个",
        (serde_json::to_string(&messages).map(|s| s.len()).unwrap_or(0)
            + serde_json::to_string(&tools).map(|s| s.len()).unwrap_or(0))
            / 1024,
        tool_n
    );

    // `--complete`：走网关同一条聚合路径（chat_complete_ex），直接看它拿到的 usage。
    if std::env::args().any(|a| a == "--complete") {
        match qoder_tools::chat_complete_ex(
            region,
            &acc,
            "gmodel",
            "GLM-5.3-Flash",
            &messages,
            None,
        )
        .await
        {
            Ok(v) => println!(
                "完成：content={:?}\nusage={}",
                v.get("content").and_then(|c| c.as_str()).unwrap_or_default(),
                serde_json::to_string_pretty(v.get("usage").unwrap_or(&json!(null))).unwrap_or_default()
            ),
            Err(e) => println!("失败：{e}"),
        }
        return;
    }

    // 排队（10605）时按上游给的 retryAfterSeconds 等一等再重来 —— 真实网关也是这么做的，
    // 这里照做才能抓到**成功的那条流**（否则永远只能看到排队帧）。
    for round in 1..=3 {
        let resp = match qoder_tools::chat_open(
            region,
            &acc,
            "gmodel",
            "GLM-5.3-Flash",
            &messages,
            tools.as_object().map(|_| &tools),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("打开失败：{e}");
                return;
            }
        };
        println!("第 {round} 轮：HTTP {}", resp.status().as_u16());

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut frames = 0usize;
        let mut queued_for: Option<u64> = None;
        let mut done = false;
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                frames += 1;
                if line.contains("isQueued") || line.contains("10605") {
                    let secs = line
                        .split("retryAfterSeconds")
                        .nth(1)
                        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).nth(1))
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(30);
                    queued_for = Some(secs);
                    println!("  ⏳ 排队帧：retryAfterSeconds={secs}");
                }
                if frames <= 2 || line.contains("usage") || line.contains("finish_reason") || line.contains("DONE") || (frames >= 15 && frames <= 22) {
                    println!("  [{}] {}", frames, line.chars().take(400).collect::<String>());
                }
                if line.contains("[DONE]") {
                    done = true;
                }
            }
            if done {
                break;
            }
        }
        if queued_for.is_none() {
            println!("--- 正常流结束，共 {frames} 行 ---");
            return;
        }
        if round < 3 {
            println!("  等 {} 秒后重试……", queued_for.unwrap_or(30));
            tokio::time::sleep(std::time::Duration::from_secs(queued_for.unwrap_or(30))).await;
        }
    }
}
