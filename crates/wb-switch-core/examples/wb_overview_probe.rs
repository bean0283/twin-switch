//! 只读探针：打印首页「本机概览」的原始 JSON，并顺带验证磁盘缓存链路。
//!
//! ```text
//! cargo run -p wb-switch-core --example wb_overview_probe          # 实时算一遍并落盘
//! cargo run -p wb-switch-core --example wb_overview_probe -- cache # 只读缓存（毫秒级）
//! cargo run -p wb-switch-core --example wb_overview_probe -- scans # 另加：给两侧清理扫描计时
//! ```
//!
//! 用于核对首页四个大数字与两张状态卡的取值来源；不联网、不写客户端数据。
//! 唯一的写副作用是本工具自己的缓存文件 `~/.twin-switch/cache/overview.json`。
//!
//! `scans` 会用 [`std::time::Instant`] 量出三条重活各自耗时，并**对比第一次真扫
//! 与第二次读缓存**的差距。这组数字的意义：
//!
//! - 它们**曾经被放在 Tauri 主线程上跑** —— 主线程被占多久，窗口就冻多久。
//!   现在都改走 `spawn_blocking`（见 `src-tauri/src/commands.rs` 的 `off_main`），
//!   界面全程可操作；
//! - 清理扫描本身在 v0.0.19 加了 **10 分钟磁盘缓存**（`*_cleanup::cached`），
//!   所以「进清理页」的正常路径是**读缓存那条**，不是真扫那条。

use std::time::Instant;

use wb_switch_core::modules::{app_overview, config, trae_cleanup, workbuddy_cleanup};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let only_cache = args.iter().any(|a| a == "cache");
    let with_scans = args.iter().any(|a| a == "scans");

    println!("缓存路径 : {}", config::cache_dir().join("overview.json").to_string_lossy());

    if only_cache {
        let t = Instant::now();
        let c = app_overview::cached();
        println!(
            "只读缓存 : empty={} generatedAt={} ageMs={}  (读盘耗时 {} ms)",
            c["empty"],
            c["generatedAt"],
            c["ageMs"],
            t.elapsed().as_millis()
        );
        if c["empty"] == serde_json::json!(false) {
            println!("reclaim  : {}", c["reclaim"]);
            println!("{}", serde_json::to_string_pretty(&c["snapshot"]).unwrap_or_default());
        }
        return;
    }

    let t = Instant::now();
    let v = app_overview::snapshot();
    println!("总览重算耗时 {} ms\n", t.elapsed().as_millis());
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());

    if with_scans {
        // 真扫：跳过缓存（等价于前端点「重新扫描」）
        let t = Instant::now();
        let a = trae_cleanup::cached(true);
        let trae_ms = t.elapsed().as_millis();
        let t = Instant::now();
        let b = workbuddy_cleanup::cached(true);
        let wb_ms = t.elapsed().as_millis();

        // 读缓存：进清理页的正常路径
        let t = Instant::now();
        let a2 = trae_cleanup::cached(false);
        let trae_cached_ms = t.elapsed().as_millis();
        let t = Instant::now();
        let b2 = workbuddy_cleanup::cached(false);
        let wb_cached_ms = t.elapsed().as_millis();
        let from_cache = a2.as_ref().map(|v| v["cached"] == serde_json::json!(true)).unwrap_or(false)
            && b2.as_ref().map(|v| v["cached"] == serde_json::json!(true)).unwrap_or(false);

        println!("\n=== 重活耗时（这些曾经全部压在主线程上）===");
        println!("Trae 清理扫描 · 真扫       {} ms  ok={}", trae_ms, a.is_ok());
        println!("WorkBuddy 清理扫描 · 真扫  {} ms  ok={}", wb_ms, b.is_ok());
        println!("合计真扫                   {} ms  ← 约等于旧版「启动后点不动的时长」", trae_ms + wb_ms);
        println!("Trae 清理扫描 · 读缓存     {} ms", trae_cached_ms);
        println!("WorkBuddy 清理扫描 · 读缓存 {} ms", wb_cached_ms);
        println!(
            "读缓存合计                 {} ms  （命中={}）",
            trae_cached_ms + wb_cached_ms,
            from_cache
        );
    }

    // 立刻回读一次，确认「算完即落盘」真的生效
    let c = app_overview::cached();
    println!(
        "\n=== 回读缓存 ===\nempty={} ageMs={}  (0 表示刚才这次写入)",
        c["empty"], c["ageMs"]
    );
}
