//! 开关自检：**起网关 → 挂一个永远发不完的请求 → 停 → 看端口多久释放**。
//!
//! 用户 2026-11-04 反馈「积分共享开关经常无效，得重启软件」。根因是停止没有硬上限：
//! axum 的 graceful shutdown 会一直等所有连接结束，而网关做流式代理，一条对话的 SSE
//! 可以挂几分钟 —— `restart` 等 3 秒就放弃，于是卡在「开关开着、端口没人听」。
//!
//! 这个例子就是那个场景的最小复现：
//!
//! ```text
//! cargo run --release --example wb_gateway_toggle_check          # 默认 57999 端口
//! cargo run --release --example wb_gateway_toggle_check -- 58001
//! ```
//!
//! 判定标准（脚本会把结论打出来）：
//!   * `stop()` 必须在 3 秒预算内返回，并且**端口已经能重新 bind**；
//!   * `restart()` 之后端口能重新连上；
//!   * 全程不碰真实的 57892，不会影响正在用的网关。

use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};
use wb_switch_core::modules::gateway_server;

/// ⚠️ 必须用 async 探测：`serve` 跑在同一个 runtime 里，用阻塞 connect + sleep
/// 会把 current_thread 运行时卡死，被测的 serve 连第一次 poll 都轮不到（第一次跑就踩到了）。
async fn connect_ok(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await,
        Ok(Ok(_))
    )
}

async fn wait_listening(port: u16, budget: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < budget {
        if connect_ok(port).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(57999);

    if !port_free(port) {
        println!("端口 {port} 已被占用，换个端口再跑（例：-- 58001）");
        return;
    }

    println!("① 起网关 :{port}");
    tokio::spawn(async move {
        if let Err(e) = gateway_server::serve(port).await {
            eprintln!("serve 结束：{e}");
        }
    });
    if !wait_listening(port, Duration::from_secs(3)).await {
        println!("   ❌ 3 秒内没监听，测试不成立");
        return;
    }
    println!("   ✅ 已监听");

    // ② 造一个「永远发不完的请求」：声明 Content-Length 但不发完 body，
    //    服务端会一直等 → 正是真实的「对话还在流式输出」那种连接。
    let mut stuck = TcpStream::connect(("127.0.0.1", port)).expect("连不上");
    stuck
        .write_all(b"POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 9999\r\n\r\n{")
        .expect("写失败");
    stuck.flush().ok();
    // 再挂两条空闲 keep-alive
    let keep1 = TcpStream::connect(("127.0.0.1", port)).ok();
    let keep2 = TcpStream::connect(("127.0.0.1", port)).ok();
    println!("② 已挂 1 个未完成请求 + {} 条空闲连接", [&keep1, &keep2].iter().filter(|x| x.is_some()).count());

    println!("③ 停网关（预算 3000 ms）");
    let t = Instant::now();
    let r = gateway_server::stop(3000).await;
    let ms = t.elapsed().as_millis();
    let free = port_free(port);
    println!("   stop() -> {r:?}  耗时 {ms} ms  端口已释放={free}");
    println!(
        "   {}",
        if r.is_ok() && free && ms <= 3500 {
            "✅ 停止有硬上限，端口真的放开了"
        } else {
            "❌ 停止没按预算完成（会表现为「开关点了没反应」）"
        }
    );

    println!("④ 再起来");
    let t = Instant::now();
    let r = gateway_server::restart(port).await;
    let up = wait_listening(port, Duration::from_secs(2)).await;
    println!("   restart() -> {r:?}  耗时 {} ms  端口在听={up}", t.elapsed().as_millis());
    println!("   {}", if r.is_ok() && up { "✅ 能重新监听" } else { "❌ 起不来" });

    println!("⑤ 收尾");
    let _ = gateway_server::stop(3000).await;
    println!(
        "   status.running = {}  端口已释放={}",
        gateway_server::status()["running"],
        port_free(port)
    );
    drop((stuck, keep1, keep2));
}
