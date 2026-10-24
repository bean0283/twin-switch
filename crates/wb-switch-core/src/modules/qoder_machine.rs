//! Qoder 活动平台的**机器身份**（`cosy-machine*` 头）。
//!
//! ## 为什么必须有这组头（真机对照，2026-10-24）
//!
//! 同一个 token（`dt-kGB…`）、同一台机器、同一个接口，只改这组头：
//!
//! | 请求头 | `claimable` | 活动数 | 是否含「每天领 100 Credits」 |
//! | --- | --- | --- | --- |
//! | 只带 `cosy-clienttype` / `cosy-version` / UA | **false** | **2** | ❌ 只有 VIEW_DETAILS |
//! | 带全套 6 个 `cosy-machine*` | **true** | **3** | ✅ `CLAIM_BENEFIT` / `CLAIMABLE` / 100 |
//!
//! ⇒ 服务端按「是不是那台登了客户端的机器」决定下发不下发签到活动。
//!   之前那次「带不带一样」的结论是在**活动已被领走**的状态下测的（两次都是 CLAIMED），
//!   看不出差别 —— 这个坑记在这里，别再重复。
//!
//! ## 六个头的来源
//!
//! | 头 | 来源 | 能否自动 |
//! | --- | --- | --- |
//! | `cosy-machineid` | `%APPDATA%\com.qoder{,.cn}.app.stable/auth.machine-id` | ✅ 自动读 |
//! | `cosy-machinehostname` | 本机 hostname | ✅ 自动 |
//! | `cosy-machineos` | 固定 `x86_64_win32` | ✅ 自动 |
//! | `cosy-machinecode` | 客户端内部派生（16 hex） | ❌ 需抓包 |
//! | `cosy-machinetype` | 客户端内部派生（20 hex） | ❌ 需抓包 |
//! | `cosy-machinetoken` | **服务端签发**（客户端登录后拿到） | ❌ 需抓包 |
//!
//! 后三个无法自行计算，所以存成配置：`~/.twin-switch/qoder-machine.json`。
//! 用户从抓包里粘一次即可；`machineid` / `hostname` / `os` 会自动补齐并写回。

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::modules::qoder_realm::QoderRegion;

/// 配置文件名（落在 `~/.twin-switch/` 下，所有账号共用 —— 这是**机器**属性）。
const FILE_NAME: &str = "qoder-machine.json";

fn config_path() -> PathBuf {
    crate::modules::config::store_dir().join(FILE_NAME)
}

/// 组好的机器身份（六个头一次给齐）。
#[derive(Debug, Clone)]
pub struct MachineIdentity {
    pub machine_code: String,
    pub machine_hostname: String,
    pub machine_id: String,
    pub machine_os: String,
    pub machine_token: String,
    pub machine_type: String,
}

impl MachineIdentity {
    /// 转成请求头（顺序与官方客户端一致）。
    pub fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("cosy-machinecode".into(), self.machine_code.clone()),
            (
                "cosy-machinehostname".into(),
                self.machine_hostname.clone(),
            ),
            ("cosy-machineid".into(), self.machine_id.clone()),
            ("cosy-machineos".into(), self.machine_os.clone()),
            ("cosy-machinetoken".into(), self.machine_token.clone()),
            ("cosy-machinetype".into(), self.machine_type.clone()),
        ]
    }
}

/// 读客户端自己写的 `auth.machine-id`（两个版本各一份，按区域取且**不跨区兜底**）。
fn machine_id_from_client(region: QoderRegion) -> Option<String> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = match region {
        QoderRegion::Cn => "com.qodercn.app.stable",
        QoderRegion::Global => "com.qoder.app.stable",
    };
    let path = PathBuf::from(appdata).join(dir).join("auth.machine-id");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn machine_os() -> String {
    if cfg!(target_arch = "x86_64") {
        "x86_64_win32".to_string()
    } else if cfg!(target_arch = "aarch64") {
        "aarch64_win32".to_string()
    } else {
        "x86_64_win32".to_string()
    }
}

/// 读配置（原样返回 JSON）。
pub fn load() -> Value {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({}))
}

/// 写配置（合并，不覆盖已有字段）。
pub fn save(patch: &Value) -> Result<Value, String> {
    let path = config_path();
    let mut cur = load();
    if let (Some(dst), Some(src)) = (cur.as_object_mut(), patch.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    } else if patch.is_object() {
        cur = patch.clone();
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&cur).unwrap_or_default(),
    )
    .map_err(|e| format!("写 {} 失败：{e}", path.display()))?;
    Ok(cur)
}

/// 组装可用的机器身份：抓包三项（配置里）+ 自动补齐的三项。
///
/// 缺一不可 —— 缺 `machinecode/token/type` 里的任何一个都返回 `None`，
/// 因为**发一半比不发更糟**（服务端会当成畸形客户端）。
pub fn identity(region: QoderRegion) -> Option<MachineIdentity> {
    let cfg = load();
    let get = |k: &str| {
        cfg.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let machine_code = get("machineCode")?;
    let machine_token = get("machineToken")?;
    let machine_type = get("machineType")?;

    // machineid 优先用配置里手填的，其次读客户端自己的文件（**按区域**）。
    let machine_id = get("machineId")
        .or_else(|| machine_id_from_client(region))
        .or_else(|| cfg.get("machineId").and_then(Value::as_str).map(str::to_string))?;

    Some(MachineIdentity {
        machine_code,
        machine_hostname: get("machineHostname").unwrap_or_else(hostname),
        machine_id,
        machine_os: get("machineOs").unwrap_or_else(machine_os),
        machine_token,
        machine_type,
    })
}

/// 当前状态（给设置页显示：缺哪几项、machineid 有没有自动读到）。
pub fn status() -> Value {
    let cfg = load();
    let has = |k: &str| {
        cfg.get(k)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    };
    json!({
        "path": config_path().display().to_string(),
        "machineCode": has("machineCode"),
        "machineToken": has("machineToken"),
        "machineType": has("machineType"),
        "machineId": has("machineId") || machine_id_from_client(QoderRegion::Cn).is_some()
            || machine_id_from_client(QoderRegion::Global).is_some(),
        "autoMachineId": {
            "cn": machine_id_from_client(QoderRegion::Cn),
            "global": machine_id_from_client(QoderRegion::Global),
        },
        "hostname": hostname(),
        "machineOs": machine_os(),
        "ready": identity(QoderRegion::Cn).is_some() || identity(QoderRegion::Global).is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_requires_all_three_captured_fields() {
        // 配置为空时（测试环境通常没有这个文件）必须返回 None，不能拼半个头出去。
        let cfg = load();
        if cfg.get("machineCode").is_none() {
            assert!(identity(QoderRegion::Cn).is_none());
        }
    }
}
