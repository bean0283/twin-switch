//! 用真实被拒请求验证「system 提示词中和」这一步：读入被拒的请求体，跑一遍
//! `gateway_prompt::neutralize_system`，把改过的请求体打到 stdout。
//!
//! 用法：`cargo run --release --example wb_prompt_check -- <rejected-*.json> [out.json]`
//!
//! 有了它就能对着真上游做 A/B：原样发 → 400/11128，中和后发 → 200。
//! （`gateway_prompt` 的单测只覆盖文本规则，这一步覆盖「真的能过上游」。）

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("用法：wb_prompt_check <被拒请求体.json> [输出.json]");
        std::process::exit(2);
    };
    let raw = std::fs::read_to_string(path).expect("读不到输入文件");
    let mut body: serde_json::Value = serde_json::from_str(&raw).expect("不是合法 JSON");

    let before = body
        .pointer("/messages/0/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .len();
    let hits = wb_switch_core::modules::gateway_prompt::neutralize_system(&mut body);
    let after = body
        .pointer("/messages/0/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .len();
    eprintln!("命中 {hits} 处 system 消息；首条长度 {before} → {after}");

    let out = serde_json::to_string(&body).expect("序列化失败");
    match args.get(1) {
        Some(p) => {
            std::fs::write(p, &out).expect("写不了输出文件");
            eprintln!("已写出 {p}");
        }
        None => println!("{out}"),
    }
}
