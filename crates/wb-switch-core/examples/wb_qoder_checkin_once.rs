//! Qoder 签到端到端自检：用**本进程的真实代码路径**查一次、领一次。
//!
//! ```text
//! cargo run --release --example wb_qoder_checkin_once -- global
//! cargo run --release --example wb_qoder_checkin_once -- global --dry   # 只查不领
//! ```
//!
//! 这比手工 curl 有意义的地方：它走的是 `campaign_headers` → `identity_for` →
//! `qoder_umid::probe_cached` 这条**真正会发出去**的链路，能验证
//! 「设备头现取 + cosy-version 取客户端版本」是不是真的生效。
//!
//! 只读账号库，不改任何配置；`--dry` 时连 claim 都不调。

use wb_switch_core::modules::qoder_accounts;
use wb_switch_core::modules::qoder_machine;
use wb_switch_core::modules::qoder_realm::QoderRegion;
use wb_switch_core::modules::qoder_tools;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let region = match args.next().as_deref() {
        Some("cn") => QoderRegion::Cn,
        Some("global") | None => QoderRegion::Global,
        Some(other) => {
            eprintln!("用法：wb_qoder_checkin_once [cn|global] [--dry]（收到 {other}）");
            return;
        }
    };
    let dry = args.any(|a| a == "--dry" || a == "-n");

    println!("=== Qoder 签到自检（{}）===", region.as_str());
    match qoder_machine::identity(region) {
        Some(id) => println!("设备身份: 可用，来源 {}", id.source),
        None => println!("设备身份: 不可用（不会发任何 cosy-machine* 头，大概率拿不到签到活动）"),
    }
    println!("cosy-version: {}", qoder_machine::cosy_version(region));

    let accounts = match qoder_accounts::list(region) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("读账号库失败：{e}");
            return;
        }
    };
    let Some(account) = accounts.iter().find(|a| {
        !a.disabled && a.access_token.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false)
    }) else {
        println!("没有可用账号（未登录或凭据为空）");
        return;
    };
    println!("账号: {}（{}）", account.name, account.uid);

    match qoder_tools::checkin_status(region, account).await {
        Ok(v) => {
            println!(
                "活动: claimable={} 可领 {} 个",
                v["claimable"], v["claimableCount"]
            );
            // 本机缺设备身份时，活动列表本身是**不可信**的（会少掉"每天领积分"那条），
            // 所以要把后端一起返回的告警原样打出来。
            if let Some(w) = v["warning"].as_str() {
                println!("⚠️ 本机设备身份告警：\n{w}");
            }
            for c in v["campaigns"].as_array().cloned().unwrap_or_default() {
                println!(
                    "    {} | {} | {} | {:?} {:?}",
                    c["key"], c["kind"], c["status"], c["amount"], c["endAt"]
                );
            }
        }
        Err(e) => {
            eprintln!("查活动失败：{e}");
            return;
        }
    }

    if dry {
        println!("\n--dry：不调用 claim");
        return;
    }
    match qoder_tools::checkin_claim(region, account).await {
        Ok(v) => println!("\n领奖结果: {}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(e) => eprintln!("\n领奖失败：{e}"),
    }
}
