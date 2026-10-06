use serde_json::json;
use std::collections::HashMap;
use wb_switch_core::modules::config::http_request;
use wb_switch_core::modules::workbuddy_vault;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    for a in workbuddy_vault::load_accounts() {
        let at = a.get("access_token").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let uid = a.get("uid").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if at.is_empty() { continue; }
        println!("账号 uid={uid} token 长度={} 含换行={} 前缀={}", at.len(), at.contains('\n'), &at[..12.min(at.len())]);

        let mut h = HashMap::new();
        h.insert("Authorization".to_string(), format!("Bearer {at}"));
        h.insert("Content-Type".to_string(), "application/json".to_string());
        h.insert("X-User-Id".to_string(), uid.clone());
        h.insert("X-Domain".to_string(), "www.codebuddy.cn".to_string());
        h.insert("X-Client-Platform".to_string(), "web".to_string());
        h.insert("Accept".to_string(), "application/json, text/plain, */*".to_string());
        h.insert("Origin".to_string(), "https://www.codebuddy.cn".to_string());
        h.insert("Referer".to_string(), "https://www.codebuddy.cn/profile/plans-usage".to_string());

        for url in [
            "https://www.codebuddy.cn/billing/meter/get-user-resource-summary",
            "https://www.codebuddy.cn/v2/billing/meter/get-user-resource",
        ] {
            let r = http_request(url, "POST", Some(json!({})), Some(&h)).await;
            let s = serde_json::to_string(&r).unwrap_or_default();
            println!("   POST {url}\n      -> {}", s.chars().take(260).collect::<String>());
        }
    }
}
