//! T68 签到设备 ID 探针 —— **真实跑一次签到**，验证「每个账号一个设备 ID」。
//!
//! 背景：旧实现抄 `oauth.json` 里那个「整台客户端共用」的 `deviceId`
//! （2026-10-15 实测本机 6 份全是 `2081789759006043`）⇒ 服务端对 claim 接口按**设备**
//! 判重 ⇒ 一个客户端里**只有第一个账号签得成**，其余全 `9005 当前设备今日已经签到`。
//!
//! 期望（修复后）：同一客户端下每个账号各自拿到不同的设备 ID ⇒ **每个账号都能签**；
//! 同一账号在 solo-cn / trae-cn 用**同一个**设备 ID ⇒ 第二次显示「今日已签」而不是「失败」。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example t68_checkin_probe            # 全部账号
//!   cargo run -p wb-switch-core --example t68_checkin_probe -- trae    # 只签 Trae
//!   cargo run -p wb-switch-core --example t68_checkin_probe -- trae solo-cn
//!
//! ⚠️ 这会**真的发起签到请求**（幂等：今天签过就回「今日已签」）。

use wb_switch_core::modules::checkin;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let scope = args.next();
    let only = args.next();

    println!(
        "=== T68 签到探针   scope={:?}  onlyClient={:?} ===\n",
        scope, only
    );

    let out = checkin::run(scope.as_deref(), only.as_deref()).await;

    if let Some(items) = out.get("items").and_then(|v| v.as_array()) {
        for it in items {
            let client = it.get("client").and_then(|v| v.as_str()).unwrap_or("?");
            let ck = it.get("clientKey").and_then(|v| v.as_str()).unwrap_or("-");
            let uid = it.get("uid").and_then(|v| v.as_str()).unwrap_or("-");
            let st = it.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let msg = it.get("message").and_then(|v| v.as_str()).unwrap_or("");
            println!("[{st:>11}] {client} / {ck}   uid={uid}");
            println!("              {msg}");
        }
    }

    println!("\n--- summary ---");
    println!(
        "{}",
        serde_json::to_string_pretty(out.get("summary").unwrap_or(&out)).unwrap_or_default()
    );
}
