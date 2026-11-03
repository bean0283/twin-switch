//! 对账工具：看某个请求会被派生成什么**上游会话 ID**。
//!
//! 用户反馈「同一段对话被拆成两条用量记录」或「今天的消耗算进昨天那行」时，先跑这个：
//!
//! ```text
//! cargo run --release --example wb_conv_check -- ~/.twin-switch/gateway-last-request.json
//! ```
//!
//! 输出里 `conversation_id` 就是发给上游的 `conversation_id`；
//! 同一段对话在**同一个自然日内**必须始终是同一个值。
//! 指标里的 `conv` 列是「会话身份键」的前 8 位（不含 uid / 日期），它一变上面这个必变。
//!
//! 想验证「易变内容不影响身份」，可以自己改文件里 `Round: 1/256` 或
//! `…<已截断，原长 N 字>` 再跑一次，输出应当一模一样。

use serde_json::Value;
use wb_switch_core::modules::gateway_stream::{conversation_key, session_seed_at, session_id};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| panic!("用法：wb_conv_check <request.json> [uid]"));
    let uid = std::env::args().nth(2).unwrap_or_else(|| "u-preview".to_string());
    let text = std::fs::read_to_string(&path).expect("读不到请求文件");
    let req: Value = serde_json::from_str(&text).expect("不是合法 JSON");

    let model = req.get("model").and_then(Value::as_str).unwrap_or("(未指定)");
    let msgs = req.get("messages").cloned().unwrap_or(Value::Null);
    let arr = msgs.as_array().cloned().unwrap_or_default();

    let first_user = arr
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("(没有 user 消息)");
    let head: String = first_user.chars().take(90).collect();

    println!("请求文件     {path}");
    println!("模型         {model}");
    println!("消息数       {}", arr.len());
    println!("首条 user    {head}");
    println!();
    println!("会话身份键   {}（挑账号用，不含 uid / 日期）", conversation_key(model, &req));

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let seed = session_seed_at(model, &uid, &msgs, &today);
    println!();
    println!("今天 {today} 的会话 ID：");
    println!("  conversation_id  {}", session_id(&seed, "trae-conv"));
    println!("  session_id       {}", session_id(&seed, "trae-sess"));
    println!("  （指标 metrics 里的 `conv` 列 = 会话身份键的前 8 位；它一变这里必变）");
    println!();
    println!("换一天（证明按天切，不会跨天累加）：");
    for d in ["2026-11-02", "2026-11-03"] {
        let s = session_seed_at(model, &uid, &msgs, d);
        println!("  {d}  {}", session_id(&s, "trae-conv"));
    }
    println!();
    println!("不按天切（TWIN_TS_CONV_ROTATE=off 的效果）：");
    let off = session_seed_at(model, &uid, &msgs, "");
    println!("  {}", session_id(&off, "trae-conv"));
}
