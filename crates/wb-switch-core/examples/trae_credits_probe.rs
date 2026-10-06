//! 只读探针：打印 Trae 账号「额度 / 积分包」接口的**原始返回**。
//!
//! ```text
//! cargo run -p wb-switch-core --example trae_credits_probe           # 打原始接口
//! cargo run -p wb-switch-core --example trae_credits_probe -- parsed # 走完整链路
//! ```
//!
//! 用途：给「Trae 账号卡显示积分包明细」定字段。`parsed` 模式直接跑
//! [`wb_switch_core::modules::trae_credits::query`]（真联网 + 写各账号的
//! `profile.json`），打印前端的最终形状并计时；默认模式只打印接口原样返回，
//! 用于核对字段。
//!
//! ⚠️ 默认模式只读 `oauth.json` 并打印；`parsed` 模式会**写账号库里的
//! `profile.json`**（这是产品本身的缓存文件，非用户资产）。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Instant;

use wb_switch_core::modules::{config, trae_credits, trae_vault};

fn clean(token: &str) -> String {
    let t = token.trim();
    t.strip_prefix("Cloud-IDE-JWT ").unwrap_or(t).to_string()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::args().any(|a| a == "parsed") {
        let t = Instant::now();
        let v = trae_credits::query(None, true).await;
        println!("trae_credits::query(force=true) 耗时 {} ms\n", t.elapsed().as_millis());
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        let t = Instant::now();
        let c = trae_credits::query(None, false).await;
        println!(
            "\n回读（force=false）耗时 {} ms  cached={}",
            t.elapsed().as_millis(),
            c["cached"]
        );
        return;
    }

    let root = trae_vault::vault_root();
    println!("账号库根目录：{}", root.to_string_lossy());

    let mut targets: Vec<(String, String, std::path::PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for c in rd.flatten() {
            if !c.path().is_dir() {
                continue;
            }
            let client = c.file_name().to_string_lossy().into_owned();
            if let Ok(rd2) = std::fs::read_dir(c.path()) {
                for a in rd2.flatten() {
                    if !a.path().is_dir() {
                        continue;
                    }
                    let id = a.file_name().to_string_lossy().into_owned();
                    let p = a.path().join("oauth.json");
                    if p.is_file() {
                        targets.push((client.clone(), id, p));
                    }
                }
            }
        }
    }
    println!("找到 {} 个 oauth 账号\n", targets.len());

    for (client, id, path) in targets {
        let oauth: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let token = clean(oauth.get("token").and_then(Value::as_str).unwrap_or(""));
        let host = oauth
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("https://api.trae.cn")
            .trim_end_matches('/')
            .to_string();
        let device_id = oauth.get("deviceId").and_then(Value::as_str).unwrap_or("0").to_string();
        let uid = oauth.get("uid").and_then(Value::as_str).unwrap_or("").to_string();

        println!("===================== {client}/{id}  uid={uid}  host={host}");
        if token.is_empty() {
            println!("  ✗ 没有 token，跳过");
            continue;
        }

        let mut h = HashMap::new();
        h.insert("authorization".to_string(), format!("Cloud-IDE-JWT {token}"));
        h.insert("x-device-id".to_string(), device_id);
        h.insert("x-device-type".to_string(), "windows".to_string());
        h.insert("x-user-region".to_string(), "CN".to_string());
        h.insert("x-market-client-id".to_string(), "VSCode 1.107.1".to_string());
        h.insert("x-app-version".to_string(), "0.1.63".to_string());
        h.insert("app-version".to_string(), "0.1.63".to_string());
        h.insert("package-type".to_string(), "stable_cn".to_string());
        h.insert("accept-language".to_string(), "zh-CN".to_string());

        let r = config::http_request(
            &format!("{host}/trae/api/v2/pay/user_current_entitlement_list"),
            "POST",
            Some(json!({ "req_source": 2 })),
            Some(&h),
        )
        .await;
        println!("----- entitlement -----");
        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());

        let mut h2 = HashMap::new();
        h2.insert("accept".to_string(), "*/*".to_string());
        h2.insert("x-cloudide-token".to_string(), token.clone());
        let r2 = config::http_request(
            &format!("{host}/cloudide/api/v3/trae/GetUserInfo"),
            "POST",
            Some(json!({ "ReqSource": "Lite", "IDEVersion": "0.1.63" })),
            Some(&h2),
        )
        .await;
        println!("----- GetUserInfo -----");
        println!("{}", serde_json::to_string_pretty(&r2).unwrap_or_default());
        println!();
    }
}
