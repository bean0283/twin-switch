//! 探针：验证 Qoder 私有协议**接受 OpenAI 形状的 `tools`**。
//!
//! 背景（2026-10-24）：网关以前完全不把 `tools` 发给 Qoder，带工具/带文件的对话
//! 里模型看不到工具，只会把调用写成 `<function_calls>` 正文 —— 提升成 `tool_calls`
//! 后 `content` 为 null，客户端就是「已完成 + 一片空白」。
//!
//! 跑法（会消耗极少量额度）：
//! ```text
//! cargo run -p wb-switch-core --example qoder_tools_probe
//! ```
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let region = wb_switch_core::modules::qoder_realm::QoderRegion::Cn;
    let list = wb_switch_core::modules::qoder_accounts::list(region).unwrap_or_default();
    let Some(acc) = list.iter().find(|a| a.is_usable()) else {
        eprintln!("没有可用的 Qoder 国内账号");
        return;
    };
    eprintln!("账号: {} ({})", acc.label(), acc.uid);

    let messages = json!([{ "role": "user", "content": "北京今天天气怎么样？用工具查。" }]);
    let tools = json!([{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "查询某个城市的实时天气",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string", "description": "城市名" } },
                "required": ["city"]
            }
        }
    }]);

    for (key, shown) in [("dmodel", "DeepSeek-V4-Pro"), ("gmodel", "GLM-5.3")] {
        eprintln!("\n=== {key} / {shown} 带 tools ===");
        match wb_switch_core::modules::qoder_tools::chat_complete_ex(
            region,
            acc,
            key,
            shown,
            &messages,
            Some(&tools),
        )
        .await
        {
            Ok(v) => {
                let calls = v
                    .pointer("/choices/0/message/tool_calls")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                eprintln!(
                    "正文={:?} | finish={} | tool_calls={}",
                    v.pointer("/choices/0/message/content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                    v.pointer("/choices/0/finish_reason")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?"),
                    serde_json::to_string(&calls).unwrap_or_default()
                );

                // ② 第二轮：把工具结果按 OpenAI 形状回传，看 Qoder 认不认 `role:"tool"`。
                //    带文件的对话必然是多轮（读文件 → 再总结），这一轮不通等于还是不能用。
                if let Some(first) = calls.first() {
                    let id = first
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("call_1");
                    let name = first
                        .pointer("/function/name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("get_weather");
                    let round2 = json!([
                        { "role": "user", "content": "北京今天天气怎么样？用工具查。" },
                        {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": id,
                                "type": "function",
                                "function": { "name": name, "arguments": "{\"city\":\"北京\"}" }
                            }]
                        },
                        { "role": "tool", "tool_call_id": id, "content": "晴，22℃，西北风 2 级" }
                    ]);
                    eprintln!("--- 第二轮（回传 tool 结果）---");
                    match wb_switch_core::modules::qoder_tools::chat_complete_ex(
                        region, acc, key, shown, &round2, Some(&tools),
                    )
                    .await
                    {
                        Ok(v) => eprintln!(
                            "正文={:?}",
                            v.pointer("/choices/0/message/content")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("")
                        ),
                        Err(e) => eprintln!("第二轮失败：{e}"),
                    }
                }
            }
            Err(e) => eprintln!("失败：{e}"),
        }
    }
}
