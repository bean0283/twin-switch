//! 验证清理扫描的缓存行为（只读，不改任何数据）。
//!
//! 用户诉求：「每次点清理页都要重扫，反应慢；之前扫过的记录可以用。」
//! 所以这里要证明三件事：
//!
//! 1. `cached(false)` 在**缓存很旧**（远超 10 分钟阈值）时也**直接返回**，不再重扫；
//! 2. 返回体带 `cached=true` / `ageMs` / `stale=true`，界面能如实告诉用户这份数据多旧；
//! 3. `cached(true)`（「重新扫描」按钮）仍然真扫并刷新缓存。
//!
//! 跑法：`cargo run -p wb-switch-core --example cleanup_cache_probe`

use std::time::Instant;

use wb_switch_core::modules::{trae_cleanup, workbuddy_cleanup};

fn probe(label: &str, f: impl Fn(bool) -> Result<serde_json::Value, String>) {
    let t0 = Instant::now();
    let first = f(false);
    let ms = t0.elapsed().as_millis();
    match first {
        Ok(v) => {
            println!(
                "[{label}] cached(false) 用时 {ms} ms · cached={} stale={} ageMs={}",
                v.get("cached").and_then(|x| x.as_bool()).unwrap_or(false),
                v.get("stale").and_then(|x| x.as_bool()).unwrap_or(false),
                v.get("ageMs").and_then(|x| x.as_i64()).unwrap_or(-1),
            );
        }
        Err(e) => println!("[{label}] cached(false) 出错：{e}"),
    }

    // 再点一次（模拟「来回切页面」），应当同样快。
    let t1 = Instant::now();
    let second = f(false);
    let ms2 = t1.elapsed().as_millis();
    match second {
        Ok(v) => {
            let bytes = v
                .get("total_bytes")
                .or_else(|| v.get("totals"))
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".into());
            println!("[{label}] cached(false) 第二次用时 {ms2} ms · 合计字段 {bytes}");
        }
        Err(e) => println!("[{label}] cached(false) 第二次出错：{e}"),
    }

    // force=true 必须真的重扫（用来证明「重新扫描」按钮仍然有效）。
    let t2 = Instant::now();
    match f(true) {
        Ok(v) => println!(
            "[{label}] cached(true)  用时 {} ms · cached={}（false = 真扫过）",
            t2.elapsed().as_millis(),
            v.get("cached").and_then(|x| x.as_bool()).unwrap_or(false),
        ),
        Err(e) => println!("[{label}] cached(true) 出错：{e}"),
    }
}

fn main() {
    println!("== Trae 清理扫描缓存探针 ==");
    probe("trae", trae_cleanup::cached);
    println!();
    println!("== WorkBuddy 清理扫描缓存探针 ==");
    probe("wb  ", workbuddy_cleanup::cached);
}
