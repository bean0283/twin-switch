//! T59 探针：诊断 / 验证「WorkBuddy 刷新 token 失败」。
//!
//! ```bash
//! # 只看（默认，不联网、不消耗 refreshToken）
//! cargo run -p wb-switch-core --example t59_wb_refresh_probe
//! # 真去刷新（只刷 T59_ONLY 指定的那个账号；T59_FORCE=1 无视 7 天门槛）
//! T59_GO=1 T59_ONLY=3b0e0821 T59_FORCE=1 \
//!   cargo run -p wb-switch-core --example t59_wb_refresh_probe
//! ```
//!
//! ⚠️ 刷新会**消耗一代 refreshToken**（服务端只保留最近两代轮换链），所以默认是干的：
//!    必须显式给 `T59_GO=1`，且建议用 `T59_ONLY=<id 前缀>` 一次只试一个账号。

use serde_json::Value;
use wb_switch_core::modules::{config, jwt, workbuddy_credits, workbuddy_vault};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let only = std::env::var("T59_ONLY").unwrap_or_default();
    let go = std::env::var("T59_GO").is_ok();
    let force = std::env::var("T59_FORCE").is_ok();

    let accounts = workbuddy_vault::load_accounts();
    println!("账号库：{} 个账号", accounts.len());
    if !go {
        println!("（干跑：不联网。要真刷新请加 T59_GO=1）");
    }
    println!();

    for acc in &accounts {
        let id = workbuddy_vault::account_id(acc);
        let short = &id[..id.len().min(8)];
        if !only.is_empty() && !id.starts_with(&only) {
            continue;
        }
        let at = acc.get("access_token").and_then(Value::as_str).unwrap_or("");
        let rt = acc.get("refresh_token").and_then(Value::as_str).unwrap_or("");
        let exp = acc.get("expiresAt").and_then(|v| v.as_i64()).unwrap_or(0);
        let exp = if exp > 0 { jwt::normalize_ms(exp) } else { jwt::exp_ms(at) };
        println!(
            "{short}  uid={}  domain={}  剩余 {} 天  at={}B rt={}B",
            acc.get("uid").and_then(Value::as_str).unwrap_or("-"),
            acc.get("domain").and_then(Value::as_str).unwrap_or("-"),
            config::days_left_ms(exp),
            at.len(),
            rt.len(),
        );

        // 只读那一步永远跑：确认「能不能刷」的判定与界面一致。
        match workbuddy_credits::account_token_info(&id) {
            Ok(info) => println!(
                "   只读：source={} canRefresh={} tokenExp={} refreshExpiredAt={}",
                info.get("source").and_then(Value::as_str).unwrap_or("-"),
                info.get("canRefresh").and_then(Value::as_bool).unwrap_or(false),
                info.get("tokenExp").map(|v| v.to_string()).unwrap_or("null".into()),
                info.get("refreshExpiredAt").map(|v| v.to_string()).unwrap_or("null".into()),
            ),
            Err(e) => println!("   只读：❌ {e}"),
        }

        if !go {
            println!();
            continue;
        }

        match workbuddy_credits::refresh_account_token_info(&id, force).await {
            Ok(v) => {
                let refreshed = v.get("refreshed").and_then(Value::as_bool).unwrap_or(false);
                if refreshed {
                    println!(
                        "   刷新：✅ 已刷新  新剩余 {} 天  写回 {}",
                        v.get("remainingDays").map(|x| x.to_string()).unwrap_or("-".into()),
                        v.get("updatedFiles").map(|x| x.to_string()).unwrap_or("-".into()),
                    );
                } else {
                    println!(
                        "   刷新：⏭  跳过（{}）",
                        v.get("message").and_then(Value::as_str).unwrap_or("-")
                    );
                }
            }
            // ⚠️ 「跳过」和「失败」必须长得不一样：失败原文照打（现在含 URL + code + message）。
            Err(e) => println!("   刷新：❌ {e}"),
        }
        println!();
    }
}
