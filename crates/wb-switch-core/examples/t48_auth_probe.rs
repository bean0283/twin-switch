//! 只读探针：把「两套登录态载体」摆在一起看。
//!
//! 新版本 Trae 在 `~/.trae-cn/trae-jwt-token` 下写了一个**裸 JWT**，
//! 而工具一直读的是 `%APPDATA%\<客户端>\User\globalStorage\storage.json` 里
//! KM 加密的 `iCubeAuthInfo://*`。两边是不是同一个账号、哪一边还在被刷新，
//! 决定了「账号切换」要不要改写入点。
//!
//! ```bash
//! cargo run -p wb-switch-core --example t48_auth_probe
//! ```
//! 全程只读；只打印 uid / 过期时间 / host，不打印凭据原文。

use std::path::{Path, PathBuf};

use serde_json::Value;
use wb_switch_core::modules::trae_km::{decrypt_km_json, is_km_value, jwt_exp, jwt_user_id};

fn fmt_ts(ms: i64) -> String {
    if ms <= 0 {
        return "(无)".into();
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn mtime(p: &Path) -> String {
    std::fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let d: chrono::DateTime<chrono::Local> = t.into();
            d.format("%Y-%m-%d %H:%M:%S").to_string()
        })
        .unwrap_or_else(|| "(无)".into())
}

fn main() {
    let home = std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("C:\\Users\\11970"));
    let appdata = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_default();

    // ---- 载体 A：~/.trae-cn/trae-jwt-token（新） ----
    println!("═══ 载体 A：~/.trae-cn/trae-jwt-token（新出现的）");
    let jt = home.join(".trae-cn").join("trae-jwt-token");
    if jt.is_file() {
        let txt = std::fs::read_to_string(&jt).unwrap_or_default();
        let t = txt.trim();
        println!("  mtime      = {}", mtime(&jt));
        println!("  bytes      = {}", t.len());
        println!("  是否 JWT   = {}", t.matches('.').count() == 2);
        println!("  uid        = {:?}", jwt_user_id(t));
        let ms = jwt_exp(t) * 1000; // jwt_exp 返回秒
        println!("  exp        = {} ({})", ms / 1000, fmt_ts(ms));
    } else {
        println!("  (不存在)");
    }

    // ---- 载体 B：storage.json 的 iCubeAuthInfo://* ----
    for client in ["Trae CN", "TRAE SOLO CN"] {
        let st = appdata
            .join(client)
            .join("User")
            .join("globalStorage")
            .join("storage.json");
        println!("\n═══ 载体 B：{client}\\User\\globalStorage\\storage.json");
        if !st.is_file() {
            println!("  (不存在)");
            continue;
        }
        println!("  mtime      = {}", mtime(&st));
        let Ok(text) = std::fs::read_to_string(&st) else {
            println!("  (读不出)");
            continue;
        };
        let Ok(j) = serde_json::from_str::<Value>(&text) else {
            println!("  (不是 JSON)");
            continue;
        };
        for key in [
            "iCubeAuthInfo://icube.cloudide",
            "iCubeAuthInfo://usertag",
        ] {
            let Some(raw) = j.get(key).and_then(|v| v.as_str()) else {
                println!("  {key}: (无)");
                continue;
            };
            print!("  {key}: len={} km={}", raw.len(), is_km_value(raw));
            match decrypt_km_json(raw) {
                Some(dec) => {
                    println!(" 解密=OK");
                    if let Some(o) = dec.as_object() {
                        let mut ks: Vec<&String> = o.keys().collect();
                        ks.sort();
                        println!("      keys = {ks:?}");
                        if let Some(uid) = o.get("userId") {
                            println!("      userId = {uid}");
                        }
                        if let Some(tok) = o.get("token").and_then(|v| v.as_str()) {
                            let ms = jwt_exp(tok) * 1000;
                            println!(
                                "      token: jwt_uid={:?} exp={} ({})",
                                jwt_user_id(tok),
                                ms / 1000,
                                fmt_ts(ms)
                            );
                        }
                        if let Some(h) = o.get("host") {
                            println!("      host = {h}");
                        }
                    }
                }
                None => println!(" 解密=失败（不是本工具的 KM 信封？）"),
            }
        }
        // 设备密钥键名（含 deviceId）
        if let Some(o) = j.as_object() {
            for k in o.keys().filter(|k| k.starts_with("iCubeAuthInfo://icube-dc:")) {
                println!("  {k}: len={}", o[k].as_str().map(|s| s.len()).unwrap_or(0));
            }
        }
        println!(
            "  iCubeLastVersion = {}",
            j.get("iCubeLastVersion").and_then(|v| v.as_str()).unwrap_or("(无)")
        );
    }
}
