//! 本机排查：Trae 两个接口域名到底能不能连（打印 reqwest 的**完整错误链**）。
//!
//! 用法：`cargo run --example trae_host_probe`
//!
//! 存在的理由：`http_request` 只把 `e.to_string()` 抛上来，而 reqwest 的顶层文案
//! 永远是那句 `error sending request for url (…)` —— 真正的原因（证书链不完整 /
//! 域名不匹配 / DNS / TCP）在 `source()` 里。排查「有的网络下必失败」时先看这个。
//!
//! ⚠️ 不带 token、不碰账号库，只对根路径发 HEAD/GET，纯连通性探测。

use std::error::Error;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let path = "/trae/api/v2/pay/user_current_entitlement_list";
    for host in ["https://api.trae.cn", "https://api.trae.com.cn", "https://api.trae.ai"] {
        println!("===== {host}");
        println!("  --- GET /");
        probe_get(host).await;
        println!("  --- POST {path}（不带 token，预期 401/业务错码而不是连接失败）");
        probe_post(&format!("{host}{path}")).await;
    }
}

async fn probe_get(host: &str) {
    {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("client");
        match client.get(host).send().await {
            Ok(r) => println!("  HTTP {}", r.status()),
            Err(e) => print_chain(&e),
        }
    }
}

async fn probe_post(url: &str) {
    {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("client");
        match client
            .post(url)
            .header("Content-Type", "application/json")
            .header("authorization", "Cloud-IDE-JWT invalid-probe")
            .header("accept-language", "zh-CN")
            .json(&serde_json::json!({ "req_source": 2 }))
            .send()
            .await
        {
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                let head: String = body.chars().take(160).collect();
                println!("  HTTP {status}  body={head}");
            }
            Err(e) => print_chain(&e),
        }
    }
}

fn print_chain(e: &reqwest::Error) {
    println!("  顶层：{e}");
    let mut src = e.source();
    let mut depth = 0;
    while let Some(s) = src {
        depth += 1;
        println!("  source[{depth}]：{s}");
        src = s.source();
    }
}
