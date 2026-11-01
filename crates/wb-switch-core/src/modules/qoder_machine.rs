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

// ---------------------------------------------------------------------------
// 跨机器迁移
//
// ## 为什么需要导出/导入
//
// 这六个头是「这台机器装了官方客户端」的证明。服务端按它决定下发不下发
// 「每天领 100 Credits」：**只带 clienttype/version/UA 时只有 VIEW_DETAILS**，
// 带全套才有 `CLAIM_BENEFIT`（真机对照见模块头）。
//
// 其中 `machinecode` / `machinetype` / `machinetoken` 客户端**运行时才生成、不落盘**
// （2026-11-01 实测：把 `%APPDATA%\com.qoder.app.stable` 与 `~/.qoder` 共 1 万多个文件
// 全扫过，三个值一处明文都没有），所以**没法在新电脑上自动读出来**。
//
// 结论：换机器时只能把这份身份**带过去**。本模块提供导出/导入，
// 就是让"能签到的那台"把身份交给"签不了的那台"。
// ---------------------------------------------------------------------------

/// 导出格式标识（与别的导出物区分，导入时先校验）。
pub const EXPORT_FORMAT: &str = "twinswitch.qoder-machine";
/// 导出格式版本。
pub const EXPORT_VERSION: u32 = 1;

/// 导出机器身份（整份可迁移，含 machineid / hostname）。
///
/// ⚠️ 为什么要连 `machineId` / `machineHostname` 一起导：服务端看到的是一整套身份，
/// 只搬 token 而让新机器用自己的 machineid，等于"token 属于 A 机器、请求来自 B 机器"，
/// 比不发更容易被判成异常客户端。整份照搬才是自洽的。
pub fn export_identity() -> Result<Value, String> {
    let cfg = load();
    let get = |k: &str| {
        cfg.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let code = get("machineCode")
        .ok_or("本机还没有机器身份（缺 machineCode）——请先在有官方客户端的机器上抓包配置")?;
    let token = get("machineToken").ok_or("本机还没有机器身份（缺 machineToken）")?;
    let mtype = get("machineType").ok_or("本机还没有机器身份（缺 machineType）")?;
    // machineid 允许来自客户端文件（自动读），导出时固化下来，免得新机器读到不同的值。
    let mid = get("machineId")
        .or_else(|| machine_id_from_client(QoderRegion::Global))
        .or_else(|| machine_id_from_client(QoderRegion::Cn))
        .ok_or("读不到 machineId（客户端没安装或还没登录过）")?;

    Ok(json!({
        "format": EXPORT_FORMAT,
        "version": EXPORT_VERSION,
        "exportedAt": chrono::Local::now().to_rfc3339(),
        "machineCode": code,
        "machineType": mtype,
        "machineToken": token,
        "machineId": mid,
        "machineHostname": get("machineHostname").unwrap_or_else(hostname),
        "machineOs": get("machineOs").unwrap_or_else(machine_os),
    }))
}

/// 导入机器身份（覆盖写入，字段逐个校验）。
///
/// 只接受 [`EXPORT_FORMAT`] 的导出物；三项关键字段缺一不可 ——
/// 半套身份发出去会被服务端当成畸形客户端（比不发更糟），所以宁可直接拒绝。
pub fn import_identity(raw: &Value) -> Result<Value, String> {
    let obj = raw.as_object().ok_or("内容不是 JSON 对象")?;
    if let Some(f) = obj.get("format").and_then(Value::as_str) {
        if f != EXPORT_FORMAT {
            return Err(format!("这不是 Qoder 机器身份文件（format = {f}）"));
        }
    }
    let pick = |k: &str| {
        obj.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let code = pick("machineCode").ok_or("缺少 machineCode")?;
    let token = pick("machineToken").ok_or("缺少 machineToken")?;
    let mtype = pick("machineType").ok_or("缺少 machineType")?;
    let mid = pick("machineId").ok_or("缺少 machineId（只搬 token 不搬 machineId 会被判成异常设备）")?;

    let patch = json!({
        "machineCode": code,
        "machineToken": token,
        "machineType": mtype,
        "machineId": mid,
        "machineHostname": pick("machineHostname").unwrap_or_else(hostname),
        "machineOs": pick("machineOs").unwrap_or_else(machine_os),
        "source": format!("从另一台机器导入（{}）", chrono::Local::now().format("%Y-%m-%d %H:%M")),
    });
    save(&patch)?;
    Ok(json!({
        "ok": true,
        "path": config_path().display().to_string(),
        "status": status(),
    }))
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
