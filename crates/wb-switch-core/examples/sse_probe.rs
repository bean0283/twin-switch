//! 离线重放：把一段录下来的上游流喂给 `gateway_stream` 的解析器 + 出口，
//! 打印客户端最终会收到的 SSE。**不连网、不花额度**。
//!
//! 用法：`cargo run -p wb-switch-core --example sse_probe`

use wb_switch_core::modules::gateway_stream::{Emitter, Kind, StreamReader};

const TRAE: &str = r#"event: output
data: {"response":"北京"}

event: output
data: {"response":"今天晴，22℃"}

event: output
data: {"tool_calls":[{"index":0,"id":"c1","function":{"name":"get_weather","arguments":"{\"city\":\"北京\"}"}}]}

event: token_usage
data: {"prompt_tokens":120,"completion_tokens":30}

event: done
data: {"finish_reason":"tool_calls"}

"#;

const QODER: &str = r#"data:{"body":"{\"choices\":[{\"delta\":{\"content\":\"我来\"}}]}","statusCodeValue":200}
data:{"body":"{\"choices\":[{\"delta\":{\"content\":\"查一下\"}}]}","statusCodeValue":200}
data:{"body":"{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"上海\\\"}\"}}]}}]}","statusCodeValue":200}
data:{"body":"{\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":88,\"completion_tokens\":12}}","statusCodeValue":200}
data:[DONE]

"#;

async fn replay(name: &str, text: &str, kind: Kind) {
    println!("=========== {name} ===========");
    let mut reader = StreamReader::from_text(text, kind);
    let mut em = Emitter::new("glm-5.3");
    loop {
        match reader.next_batch().await {
            Ok(Some(ds)) => print!("{}", em.absorb(&ds)),
            Ok(None) => break,
            Err(e) => {
                println!("[解析失败] {e}");
                return;
            }
        }
    }
    let usage = reader.usage().clone();
    print!("{}", em.finish_frames(reader.finish_reason(), &usage));
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    replay("Trae（私有事件流 + 工具调用 + 用量）", TRAE, Kind::Trae).await;
    replay("Qoder（信封 SSE + 工具调用 + 用量）", QODER, Kind::Qoder).await;
    extra_checks().await;
    prepare_checks();
}

// ---------------------------------------------------------------------------
// 追加检查：排队 / 上游报错 / 前缀稳定 / 历史折叠
// ---------------------------------------------------------------------------

const QODER_QUEUED: &str = r#"data:{"body":"{\"code\":10605,\"message\":\"isQueued\",\"queueCount\":2328,\"waitTime\":64}","statusCodeValue":403}
"#;

const TRAE_ERROR: &str = r#"event: error
data: {"code":2001,"message":"LLMUtilsChat ChatStream failed: failed to receive event"}

"#;

/// 这些用例期望**失败**，且失败原因要能分辨（排队 ≠ 上游报错）。
async fn expect_err(name: &str, text: &str, kind: Kind) {
    let mut reader = StreamReader::from_text(text, kind);
    let err = loop {
        match reader.next_batch().await {
            Ok(Some(_)) => continue,
            Ok(None) => {
                println!("[{name}] 意外正常结束（应当失败）");
                return;
            }
            Err(e) => break e,
        }
    };
    println!("[{name}] 失败原因：{err}");
}

#[allow(dead_code)]
async fn extra_checks() {
    expect_err("Qoder 排队", QODER_QUEUED, Kind::Qoder).await;
    expect_err("Trae 断流", TRAE_ERROR, Kind::Trae).await;
}

/// 前缀稳定 + 历史折叠的检查：排序确定、折叠只会打老的结果、重复调用字节一致。
fn prepare_checks() {
    use serde_json::json;
    use wb_switch_core::modules::gateway_stream::prepare;

    let mut tools: Vec<serde_json::Value> = Vec::new();
    for name in ["zeta", "alpha", "mike", "bravo"] {
        tools.push(json!({ "type": "function", "function": { "name": name, "parameters": { "type": "object" } } }));
    }
    let mut msgs: Vec<serde_json::Value> = vec![json!({ "role": "system", "content": "你是助手" })];
    msgs.push(json!({ "role": "user", "content": "开始干活" }));
    // 12 条工具结果，每条 5000 字 ⇒ 总量超过折叠门槛。
    for i in 0..12 {
        msgs.push(json!({ "role": "assistant", "content": "", "tool_calls": [] }));
        msgs.push(json!({ "role": "tool", "tool_call_id": format!("c{i}"), "content": "x".repeat(5000) }));
    }
    msgs.push(json!({ "role": "user", "content": "继续" }));
    let mut obj = json!({ "model": "glm-5.3", "tools": tools, "messages": msgs })
        .as_object()
        .cloned()
        .unwrap();

    let prep = prepare(&mut obj);
    let names: Vec<String> = obj["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    println!(
        "prepare：工具顺序={names:?} 折叠={} 条 消息={} 条 字符={}",
        prep.folded, prep.msgs, prep.chars
    );
    let kept = obj["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter(|m| !m["content"].as_str().unwrap().contains("已折叠"))
        .count();
    println!("        保持原样的工具结果 = {kept} 条（应等于 8）");

    // 幂等性：同样输入再跑一次，字节必须完全一致（否则前缀缓存永远命不中）。
    let mut again = json!({ "model": "glm-5.3", "tools": tools, "messages": msgs })
        .as_object()
        .cloned()
        .unwrap();
    prepare(&mut again);
    println!(
        "        二次整理后字节一致 = {}",
        serde_json::to_string(&obj).unwrap() == serde_json::to_string(&again).unwrap()
    );
}
