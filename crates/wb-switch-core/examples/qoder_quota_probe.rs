//! 只读探针：打印 Qoder 各账号的**积分包分块**（`userQuota` / `addOnQuota`），
//! 用来核对 `qoder_api::parse_quota` 的 parts 与界面上「积分包」口径是否一致。
//!
//! ⚠️ 会真实请求 Qoder 额度接口（只读，不写库、不消耗额度），每个账号最多一次。
//!
//! 用法：`cargo run -p wb-switch-core --example qoder_quota_probe`

use wb_switch_core::modules::{qoder_accounts, qoder_api, qoder_realm::QoderRegion};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    for region in [QoderRegion::Cn, QoderRegion::Global] {
        let list = match qoder_accounts::list(region) {
            Ok(v) => v,
            Err(e) => {
                println!("== {} 读不到账号：{e}", region.display_name());
                continue;
            }
        };
        println!("== {}（{} 个账号）", region.display_name(), list.len());
        for a in list.iter().filter(|a| a.is_usable()) {
            // REFRESH=1：走真实刷新链路（会写账号库：credits / credit_parts / credits_expire_at），
            // 等价于界面上点一次「刷新积分」。不带这个变量就只读。
            if std::env::var("REFRESH").is_ok() {
                match qoder_api::refresh_account(a).await {
                    Ok(up) => {
                        println!(
                            "  {} 已写入：credits={:?} expire_at={:?} 包数={}",
                            up.label(),
                            up.credits,
                            up.credits_expire_at,
                            up.credit_parts.len()
                        );
                        for p in &up.credit_parts {
                            println!(
                                "      - {:<8} 总量 {:>8.2}  已用 {:>8.2}  剩余 {:>8.2}",
                                p.name, p.total, p.used, p.remaining
                            );
                        }
                    }
                    Err(e) => println!("  {} 刷新失败：{e}", a.label()),
                }
                continue;
            }
            match qoder_api::fetch_quota(a).await {
                Ok(q) => {
                    // 先看上游到底给了哪几块（顶层键 + 每块的键），再谈分块口径。
                    if std::env::var("RAW").is_ok() {
                        println!("  raw = {}", serde_json::to_string_pretty(&q.raw).unwrap_or_default());
                    }
                    println!(
                        "  {} 合计剩余 {:.2} / 总量 {:.2}（{} 个包）",
                        a.label(),
                        q.remaining,
                        q.total,
                        q.parts.len()
                    );
                    for p in &q.parts {
                        println!(
                            "      - {:<8} 总量 {:>8.2}  已用 {:>8.2}  剩余 {:>8.2}",
                            p.name, p.total, p.used, p.remaining
                        );
                    }
                }
                Err(e) => println!("  {} 查询失败：{e}", a.label()),
            }
        }
    }
}
