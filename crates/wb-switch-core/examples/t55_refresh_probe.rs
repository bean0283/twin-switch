//! T55 探针：诊断「刷新 token 失败（401/20403 Token device not match）」。
//!
//! ```bash
//! cargo run -p wb-switch-core --example t55_refresh_probe
//! ```
//!
//! 三段，全程只读（后两段会真发请求，但只用 `force = false`：剩余 > 7 天的账号在本地就
//! 返回「fresh」，**不联网、不消耗 refreshToken 轮换代次**）：
//!   A. 设备密钥对照：本机各客户端的 `icube-dc` 与账号档里存的，是不是同一对密钥；
//!   B. 凭据对照：**客户端实时登录态**的 RT 与账号档里的 RT 是不是同一把（参考实现第一轮
//!      用的是实时登录态，第二轮才用备份档 —— 只试后者会漏掉一整类情况）；
//!   C. 逐个账号走真实续期链路，看服务端对每套 (设备密钥 × ClientID) 的答复。

use serde_json::Value;
use sha2::{Digest, Sha256};
use wb_switch_core::modules::trae_km::{decrypt_km_json, is_km_value, jwt_exp, jwt_user_id};
use wb_switch_core::modules::{trae_discover, trae_remote, trae_vault};

/// 只打印公钥指纹，绝不打印密钥本体。
fn fingerprint(pem: &str) -> String {
    let d = Sha256::digest(pem.trim().as_bytes());
    format!("{:x}", d)[..12].to_string()
}

/// 凭据指纹（RT / token 都不许原文外泄）：sha256 前 12 位，用来判断「是不是同一把」。
fn secret_id(s: &str) -> String {
    if s.is_empty() {
        return "(空)".into();
    }
    let d = Sha256::digest(s.as_bytes());
    format!("{}…{}", s.len(), &format!("{d:x}")[..12])
}

/// 从一份 storage.json 里解出登录态（token / refreshToken / 其所属 uid）。
fn live_login(path: &std::path::Path) -> Option<(String, String, Option<String>)> {
    let obj: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let raw = obj.get("iCubeAuthInfo://icube.cloudide")?.as_str()?;
    if !is_km_value(raw) {
        return None;
    }
    let auth = decrypt_km_json(raw)?;
    let token = auth.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let rt = auth
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let uid = auth
        .get("uid")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| jwt_user_id(&token));
    Some((token, rt, uid))
}

/// 从一份 storage.json 里取 `iCubeAuthInfo://icube-dc:*` 的设备材料。
fn live_device(path: &std::path::Path) -> Option<(String, String, String)> {
    let obj: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let key = obj
        .as_object()?
        .keys()
        .find(|k| k.starts_with("iCubeAuthInfo://icube-dc:"))?
        .clone();
    let raw = obj.get(&key)?.as_str()?;
    if !is_km_value(raw) {
        return None;
    }
    let dc = decrypt_km_json(raw)?;
    let pub_pem = dc.get("publicKeyPEM")?.as_str()?.to_string();
    Some((
        key.trim_start_matches("iCubeAuthInfo://icube-dc:").to_string(),
        pub_pem,
        obj.get("telemetry.machineId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    ))
}

/// 打印「还剩几天」（毫秒时间戳）。
fn left_days(exp_ms: i64) -> String {
    if exp_ms <= 0 {
        return "无 exp".into();
    }
    let d = (exp_ms - chrono::Utc::now().timestamp_millis()) / 86_400_000;
    format!("{d} 天")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let home = std::env::var("TWIN_SWITCH_HOME").unwrap_or_else(|_| {
        let up = std::env::var("USERPROFILE").unwrap_or_default();
        format!("{up}/.twin-switch")
    });
    let vault = format!("{home}/trae/vault");

    println!("═══ A. 设备密钥对照（只比公钥指纹）═══");
    for key in ["trae-cn", "solo-cn", "trae-intl", "solo-intl"] {
        let Some(client) = trae_discover::get_client(key) else {
            continue;
        };
        let sp = trae_discover::storage_json_path(client);
        match live_device(&sp) {
            Some((dev, pem, mid)) => println!(
                "  本机 {key:<11} deviceId={dev:<20} 公钥指纹={}  machineId={}  appVersion={}",
                fingerprint(&pem),
                if mid.is_empty() { "(空)".to_string() } else { format!("{}…", &mid[..mid.len().min(12)]) },
                trae_discover::app_version(client).unwrap_or_else(|| "(读不到)".into()),
            ),
            None => println!("  本机 {key:<11} （没读到 icube-dc）"),
        }
    }
    for key in ["trae-cn", "solo-cn"] {
        let dir = format!("{vault}/{key}");
        if !std::path::Path::new(&dir).is_dir() {
            continue;
        }
        for id in trae_vault::list_vault_accounts(key) {
            let oauth: Value = std::fs::read_to_string(format!("{dir}/{id}/oauth.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let pem = oauth.get("publicKeyPEM").and_then(|v| v.as_str()).unwrap_or("");
            let mid = oauth.get("machineId").and_then(|v| v.as_str()).unwrap_or("");
            println!(
                "  账号档 {key:<11} /{id:<14} deviceId={:<20} 公钥指纹={}  machineId={}  登录客户端={}",
                oauth.get("deviceId").and_then(|v| v.as_str()).unwrap_or("-"),
                if pem.is_empty() { "(无)".to_string() } else { fingerprint(pem) },
                if mid.is_empty() { "(空)".to_string() } else { format!("{}…", &mid[..mid.len().min(12)]) },
                oauth.get("client").and_then(|v| v.as_str()).unwrap_or("-"),
            );
        }
    }

    println!("\n═══ B. 凭据对照：客户端实时登录态 vs 账号档（参考实现第一轮用的是前者）═══");
    for key in ["trae-cn", "solo-cn"] {
        let Some(client) = trae_discover::get_client(key) else {
            continue;
        };
        match live_login(&trae_discover::storage_json_path(client)) {
            Some((token, rt, uid)) => println!(
                "  实时 {key:<11} uid={:<18} token=({}) 剩 {:<8} refreshToken={}",
                uid.as_deref().unwrap_or("-"),
                secret_id(&token),
                left_days(jwt_exp(&token)),
                secret_id(&rt),
            ),
            None => println!("  实时 {key:<11} （没读到登录态）"),
        }
    }
    for key in ["trae-cn", "solo-cn"] {
        let dir = format!("{vault}/{key}");
        if !std::path::Path::new(&dir).is_dir() {
            continue;
        }
        for id in trae_vault::list_vault_accounts(key) {
            let oauth: Value = std::fs::read_to_string(format!("{dir}/{id}/oauth.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let token = oauth.get("token").and_then(|v| v.as_str()).unwrap_or("");
            let rt = oauth.get("refreshToken").and_then(|v| v.as_str()).unwrap_or("");
            println!(
                "  档   {key:<11} /{id:<14} uid={:<18} token=({}) 剩 {:<8} refreshToken={}",
                oauth
                    .get("uid")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| jwt_user_id(token))
                    .as_deref()
                    .unwrap_or("-"),
                secret_id(token),
                left_days(jwt_exp(token)),
                secret_id(rt),
            );
        }
    }

    println!("\n═══ C. 真实续期链路（force=false：> 7 天不联网）═══");
    // 验证用开关（默认全关，保证反复跑探针不会白白消耗 refreshToken 轮换代次）：
    //   T55_ONLY=<账号目录 id 或 uid 片段>  只跑这一个账号
    //   T55_FORCE=1                        忽略 7 天门槛，强制刷新（RT 幂等，重复兑换返回同一份）
    let only = std::env::var("T55_ONLY").unwrap_or_default();
    let force = std::env::var("T55_FORCE").map(|v| v == "1").unwrap_or(false);
    if force {
        println!("⚠️ T55_FORCE=1：忽略门槛，强制刷新");
    }
    for key in ["trae-cn", "solo-cn"] {
        let dir = format!("{vault}/{key}");
        if !std::path::Path::new(&dir).is_dir() {
            continue;
        }
        let ids: Vec<String> = trae_vault::list_vault_accounts(key)
            .into_iter()
            .filter(|id| only.is_empty() || id.contains(&only))
            .collect();
        if ids.is_empty() {
            continue;
        }
        println!("== {key} ==");
        for id in ids {
            let oauth: Value = std::fs::read_to_string(format!("{dir}/{id}/oauth.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            print!(
                "  {id:<16} 登录客户端={:<9} 上次成功={:<9} ",
                oauth.get("client").and_then(|v| v.as_str()).unwrap_or("-"),
                oauth.get("profileClient").and_then(|v| v.as_str()).unwrap_or("-"),
            );
            match trae_remote::refresh_account_token(key, &id, force).await {
                Ok(v) => {
                    let refreshed = v.get("refreshed").and_then(|x| x.as_bool()).unwrap_or(false);
                    let left = v.get("remainingDays").and_then(|x| x.as_i64()).unwrap_or(-999);
                    if refreshed {
                        println!(
                            "→ ✅ 已刷新  参数={}/{}  新的剩余={left} 天  写回={:?}",
                            v.get("profile").and_then(|x| x.as_str()).unwrap_or("-"),
                            v.get("profileClientId").and_then(|x| x.as_str()).unwrap_or("-"),
                            v.get("updatedFiles").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>()).unwrap_or_default(),
                        );
                    } else {
                        println!("→ ⏭  本次跳过（剩余 {left} 天，> 门槛 7 天，不联网）");
                    }
                }
                // 「跳过」和「失败」必须长得不一样：失败原文照打。
                Err(e) => println!("→ ❌ {e}"),
            }
        }
        println!();
    }
}
