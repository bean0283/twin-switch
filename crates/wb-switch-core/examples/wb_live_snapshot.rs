//! 实时监控的**载荷自检**：不连网、不连网关，直接喂内存层，把界面真正会拿到的 JSON 打出来。
//!
//! 用途：改字段名 / 改结构时先跑这个，确认前端要的字段都在、单位和口径没变。
//!
//! ```text
//! cargo run --release --example wb_live_snapshot
//! cargo run --release --example wb_live_snapshot -- 300    # 5 分钟窗口
//! ```

use serde_json::json;
use wb_switch_core::modules::gateway_live;

fn main() {
    let win: u64 = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    // ① 两条正在跑的：一条还在等首字，一条已经在输出
    let waiting = gateway_live::start("trae:GLM-5.3-Flash", "trae-cn", Some("demo-key-a"));
    let running = gateway_live::start("trae:DeepSeek-V4.1", "trae-cn", Some("demo-key-b"));
    gateway_live::first_byte(running);
    gateway_live::note_output(running, 48 * 1024);

    // ② 再收一条已经完成的（带真实 usage 形状）
    let done = gateway_live::start("qoder:GLM-5.3-Flash", "qoder-cn", Some("demo-key-a"));
    gateway_live::first_byte(done);
    gateway_live::note_output(done, 12 * 1024);
    gateway_live::finish(done, true, Some(2656), Some(37), Some(2100), false);

    // ③ 一条排队过的（用来验证告警那一档）
    let queued = gateway_live::start("trae:GLM-5.3-Flash", "trae-cn", Some("demo-key-a"));
    gateway_live::finish(queued, true, Some(10), Some(5), None, true);

    let snap = gateway_live::snapshot(win);
    println!(
        "{}",
        serde_json::to_string_pretty(&snap).unwrap_or_else(|_| "{}".into())
    );

    // 校验几个「界面依赖」的字段，缺了就直接喊出来
    let checks = [
        ("kpi.rpm", snap.pointer("/kpi/rpm").is_some()),
        ("kpi.inTokPerSec", snap.pointer("/kpi/inTokPerSec").is_some()),
        ("kpi.outTokPerSec", snap.pointer("/kpi/outTokPerSec").is_some()),
        ("kpi.active", snap.pointer("/kpi/active").is_some()),
        ("series[].inTok", snap.pointer("/series/0/inTok").is_some()),
        ("active[].phase", snap.pointer("/active/0/phase").is_some()),
        ("active[].gotBytes", snap.pointer("/active/0/gotBytes").is_some()),
        ("keys[].promptTokens", snap.pointer("/keys/0/promptTokens").is_some()),
        ("alerts[].kind", snap.pointer("/alerts/0/kind").is_some()),
    ];
    for (name, ok) in checks {
        println!("{} {}", if ok { "OK  " } else { "MISS" }, name);
    }
    println!(
        "（活跃 {} 条，窗口 {} 秒，点数 {} 个）",
        snap.pointer("/active").and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(0),
        win,
        snap.pointer("/series").and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(0)
    );

    // 收尾：把两条假的活跃清掉，避免影响同进程后续读取
    gateway_live::abandon(waiting);
    gateway_live::abandon(running);
    debug_assert_eq!(json!(0), json!(0));
}
