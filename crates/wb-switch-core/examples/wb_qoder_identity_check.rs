//! Qoder 设备身份自检：本机到底能不能取到 `cosy-machine*` 三件套。
//!
//! 用途：用户在"某台机器签不了到"时先跑这个，一眼看出卡在哪一步
//! （没装客户端 / 组件跑不起来 / 少字段 / 正常）。
//!
//! ```text
//! cargo run --release --example wb_qoder_identity_check
//! ```
//!
//! 不联网、不签到、不写任何文件；只读客户端目录并跑一次组件。

use wb_switch_core::modules::qoder_machine;
use wb_switch_core::modules::qoder_realm::QoderRegion;
use wb_switch_core::modules::qoder_umid;

fn mask(s: &str) -> String {
    if s.len() <= 8 {
        return format!("{s}（{} 字符）", s.len());
    }
    format!("{}…（{} 字符）", &s[..6], s.len())
}

fn main() {
    println!("=== Qoder 设备身份自检 ===");
    for region in [QoderRegion::Cn, QoderRegion::Global] {
        let name = region.as_str();
        println!("\n--- {name} ---");
        match qoder_umid::locate(region) {
            Some((exe, source)) => {
                println!("组件: {}（来源 {source}）", exe.display());
            }
            None => {
                println!("组件: 未找到");
                println!("       （装了客户端但装在非默认目录？设 QODER_UMID_PATH 指向 runtime-info.exe）");
            }
        }
        println!("客户端版本: {:?}", qoder_umid::client_version(region));
        println!(
            "现取结果: {}",
            match qoder_umid::probe_cached(region, "") {
                Ok(id) => format!(
                    "OK token={} code={} type={}",
                    mask(&id.machine_token),
                    mask(&id.machine_code),
                    mask(&id.machine_type)
                ),
                Err(e) => format!("失败 —— {e}"),
            }
        );
        println!("诊断: {}", qoder_umid::diagnose(region));
        match qoder_machine::identity(region) {
            Some(id) => {
                println!(
                    "最终身份: 可用（source={}）\n  将要发送的头：\n{}",
                    id.source,
                    id.headers()
                        .iter()
                        .map(|(k, v)| format!("    {k}: {}", mask(v)))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
            None => println!("最终身份: 不可用 —— 不会发送任何 cosy-machine* 头"),
        }
    }

    println!("\n=== 汇总 ===");
    let st = qoder_machine::status();
    println!("ready = {}", st["ready"]);
    println!("source = {}", st["source"]);
    println!("cosy-version 将发送 = {}", qoder_machine::cosy_version(QoderRegion::Global));
}
