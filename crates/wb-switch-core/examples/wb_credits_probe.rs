//! 只读探针：打印本机各账号的积分与积分包。
//!
//! ```text
//! cargo run -p wb-switch-core --example wb_credits_probe            # 用缓存（5 分钟）
//! cargo run -p wb-switch-core --example wb_credits_probe -- force   # 强制重查
//! ```
//!
//! 唯一的写副作用：属于**本工具账号库**的账号在 token 刷新成功后会把新 token 落盘
//! （这是必需的，否则每次查询都要重新刷新）。参考工具账号库一个字节都不写。

use wb_switch_core::modules::workbuddy_credits;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let force = std::env::args().any(|a| a == "force");

    let list = workbuddy_credits::accounts();
    println!("=== 账号清单（合并本工具账号库 + 只读参考工具账号库）===");
    println!("路径 : {}", list["referenceStore"].as_str().unwrap_or(""));
    println!("总数 : {} 个，其中可查询 {}", list["count"], list["queryable"]);
    for a in list["accounts"].as_array().unwrap() {
        println!(
            "  - id={} uid={} name={} origin={} token={} queryable={}",
            a["id"].as_str().unwrap_or(""),
            a["uid"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or(""),
            a["origin"].as_str().unwrap_or(""),
            a["tokenState"].as_str().unwrap_or(""),
            a["queryable"],
        );
        if let Some(r) = a["blockedReason"].as_str() {
            println!("      不可查询：{r}");
        }
    }

    println!("\n=== 查询积分（force={force}）===");
    let out = workbuddy_credits::query_all(force).await;
    let s = &out["summary"];
    println!(
        "查询 {} / 成功 {} / 失败 {}  合计剩余 {}  合计额度 {}  缓存={}  更新于 {}",
        s["queried"],
        s["succeeded"],
        s["failed"],
        s["totalRemaining"],
        s["totalCapacity"],
        out["cached"],
        out["updatedAt"],
    );

    for a in out["accounts"].as_array().unwrap() {
        let acc = &a["account"];
        println!(
            "\n● {}（{}）  剩余 {} / 额度 {}  有效 {}/{} 个积分包  来源={}  已刷新={}",
            acc["name"].as_str().unwrap_or(""),
            acc["uid"].as_str().unwrap_or(""),
            a["totalRemaining"],
            a["totalCapacity"],
            a["activePackageCount"],
            a["packageCount"],
            a["source"].as_str().unwrap_or(""),
            a["refreshed"],
        );
        let soonest = a["soonestExpireAt"].as_i64();
        if let Some(ts) = soonest {
            println!(
                "  最近到期: {}",
                chrono::DateTime::from_timestamp_millis(ts)
                    .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default()
            );
        }
        for r in a["resources"].as_array().unwrap() {
            let exp = r["expireAt"]
                .as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "长期有效".to_string());
            println!(
                "    - {:<28} 剩余 {:<10} 额度 {:<10} 到期 {}{}{}",
                r["packageName"].as_str().unwrap_or(&r["packageCode"].to_string()),
                format!("{}", r["remaining"]),
                format!("{}", r["total"]),
                exp,
                if r["expiringSoon"] == serde_json::json!(true) { "  [近期到期]" } else { "" },
                if r["expired"] == serde_json::json!(true) { "  [已过期]" } else { "" },
            );
            // 商品码：前端要按它映射成官方中文名（PackageName 是运营原文，不能直接展示）
            if let Some(code) = r["packageCode"].as_str() {
                println!("      code = {code}");
            }
        }
    }

    for e in out["errors"].as_array().unwrap() {
        println!(
            "\n× {}（{}）: {}",
            e["name"].as_str().unwrap_or(""),
            e["uid"].as_str().unwrap_or(""),
            e["error"].as_str().unwrap_or("")
        );
    }
}
