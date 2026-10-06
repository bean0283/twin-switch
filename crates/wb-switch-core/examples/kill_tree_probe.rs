//! 诊断示例：真机验证 [`process_list::kill_tree_and_wait`] 的两条关键分支。
//!
//! ## 为什么需要它
//!
//! 2026-10-06 用户报「workbuddy 清理失败：未能在 20 秒内退出」。真机取证发现当时
//! WorkBuddy 正在**自我升级**（`~/.workbuddy/logs/AppStartup.log` 记着
//! `startup_type=upgrade`，5.6.2 → 5.7.6），旧进程被杀掉后**更新器立刻把新版拉了
//! 起来** —— 于是「等进程表变空」永远等不到，而真相并不是「杀不掉」。
//!
//! 这个示例用一个**会自己复活**的诱饵进程（隐藏窗口的 `ping`）把那个场景搬到台面上：
//!
//! 1. 「复活」场景：诱饵被杀掉后由后台线程立刻重新拉起
//!    ⇒ 期望 `ok == false` 且 **`restarted == true`**（而不是含糊的「杀不掉」）；
//! 2. 「正常」场景：没有复活线程
//!    ⇒ 期望 `ok == true`、`attempts == 1`。
//!
//! ## 用法
//!
//! ```text
//! cargo run -p wb-switch-core --example kill_tree_probe
//! ```
//!
//! 只碰自己起的 `ping` 进程，不触碰 WorkBuddy / Trae。全程约 8 秒。

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wb_switch_core::modules::process_list;

/// 诱饵映像名。用 `ping`（不是 WorkBuddy）—— 隐藏窗口、可安全结束。
const DECOY: &str = "ping.exe";

fn spawn_decoy() -> Option<u32> {
    let mut cmd = Command::new("ping");
    // 长时间自旋，保证它不会自己退出。
    cmd.args(["-n", "600", "127.0.0.1"]);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW：不要在屏幕上闪黑框
    }
    cmd.spawn().ok().map(|c| c.id())
}

fn decoy_alive() -> bool {
    !process_list::find_by_names(&[DECOY.to_string()]).is_empty()
}

fn main() {
    // 前置：清掉可能残留的诱饵
    let _ = process_list::kill_now(&[DECOY.to_string()]);
    std::thread::sleep(Duration::from_millis(300));

    println!("=== 场景 1：诱饵被杀掉后由后台线程立刻复活（模拟「更新器把客户端拉起来」）===");
    let Some(first) = spawn_decoy() else {
        eprintln!("起不了诱饵进程，跳过");
        return;
    };
    println!("诱饵 pid = {first}");

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if !decoy_alive() {
                    let _ = spawn_decoy(); // 复活
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }

    let outcome = process_list::kill_tree_and_wait(
        &[DECOY.to_string()],
        Duration::from_secs(4),
        &|m| m.is_empty(),
    );
    stop.store(true, Ordering::Relaxed);

    println!(
        "ok={} attempts={} restarted={} remaining={:?}",
        outcome.ok,
        outcome.attempts,
        outcome.restarted,
        outcome.remaining.iter().map(|(_, p)| *p).collect::<Vec<_>>()
    );
    let hint = outcome.hint("诱饵");
    println!("给用户的一句话：{hint}");
    let pass1 = !outcome.ok && outcome.restarted && outcome.attempts >= 2;
    println!(
        "场景 1 判定：{}",
        if pass1 { "✅ 正确识别为「被重新拉起」" } else { "❌ 判定错误" }
    );

    // 收尾：停掉复活线程之后把诱饵彻底清干净
    std::thread::sleep(Duration::from_millis(400));
    let _ = process_list::kill_now(&[DECOY.to_string()]);
    std::thread::sleep(Duration::from_millis(300));

    println!();
    println!("=== 场景 2：没有复活线程（正常退出路径）===");
    let Some(pid2) = spawn_decoy() else {
        eprintln!("起不了诱饵进程，跳过");
        return;
    };
    println!("诱饵 pid = {pid2}");
    let outcome2 = process_list::kill_tree_and_wait(
        &[DECOY.to_string()],
        Duration::from_secs(5),
        &|m| m.is_empty(),
    );
    println!(
        "ok={} attempts={} restarted={} 诱饵还在跑={}",
        outcome2.ok,
        outcome2.attempts,
        outcome2.restarted,
        decoy_alive()
    );
    let pass2 = outcome2.ok && !outcome2.restarted && outcome2.attempts == 1;
    println!(
        "场景 2 判定：{}",
        if pass2 { "✅ 一轮结束、确认退出" } else { "❌ 判定错误" }
    );

    println!();
    println!("总结：{}", if pass1 && pass2 { "两条分支都符合预期" } else { "有分支不符合预期，见上" });
}
