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
//! | `cosy-machineos` | 按架构拼（`x86_64_win32`） | ✅ 自动 |
//! | `cosy-machinecode` | **客户端 umid 组件现算** | ✅ 自动（装客户端即可） |
//! | `cosy-machinetype` | 同上 | ✅ 自动 |
//! | `cosy-machinetoken` | 同上（服务端签发，组件输出里带） | ✅ 自动 |
//!
//! ⚠️ 2026-11-02 更正：这里以前写的是「后三个无法自行计算，只能抓包存配置」——
//! **错了**。它们确实不落盘（扫一万多个文件无明文），但客户端自带的
//! `resources/umid/runtime-info` 组件可以**现场算出来**，客户端主进程自己也是这么调的。
//! 现取通道见 [`crate::modules::qoder_umid`]；本文件的配置只作为**兜底/可迁移**的那一份
//! （抓包手填、或从另一台机器导入）。

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
    /// 这份身份从哪来：`config`（抓包/导入）/ `umid`（本机客户端现取）。
    pub source: &'static str,
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

/// 组装可用的机器身份：**先看配置，再看本机客户端现取**。
///
/// 顺序是有意为之：
///
/// 1. `~/.twin-switch/qoder-machine.json` 三项齐全（抓包粘的 / 从别的机器导入的）→ 用它。
///    这是用户**显式**给的身份，可能就是在模拟另一台机器，不能被本机探测悄悄覆盖。
/// 2. 否则跑本机 Qoder 自带的 `umid/runtime-info` 现取（见 [`crate::modules::qoder_umid`]）。
///    ⇒ **新设备只要装了客户端并登录过，就能直接签到，不需要导包**（2026-11-02 改正：
///    之前认为"三个值不落盘所以只能搬"，漏了客户端能现场生成这一条）。
/// 3. 都没有 → `None`：一个 `cosy-machine*` 头都不发。半套（或假造）比不发更糟。
///
/// ⚠️ `machineId` 必须与 token **同源**：配置来的身份配配置里的 machineId；
/// 现取的身份配**本机** `auth.machine-id`。混着发等于「token 属于 A 机器、id 属于 B 机器」，
/// 服务端一眼看出不是官方客户端。
pub fn identity(region: QoderRegion) -> Option<MachineIdentity> {
    identity_for(region, "")
}

/// 同 [`identity`]，但把账号 uid 传给组件（组件用它判定 `accountOutcome`；
/// 三件套本身是**机器级**、与账号无关 —— 实测同一台机器传不同 uid，token 逐字相同）。
pub fn identity_for(region: QoderRegion, uid: &str) -> Option<MachineIdentity> {
    let cfg = load();
    let get = |k: &str| {
        cfg.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    // ① 配置文件里的整套身份。
    if let (Some(machine_code), Some(machine_token), Some(machine_type)) = (
        get("machineCode"),
        get("machineToken"),
        get("machineType"),
    ) {
        if let Some(machine_id) = get("machineId").or_else(|| machine_id_from_client(region)) {
            return Some(MachineIdentity {
                machine_code,
                machine_hostname: get("machineHostname").unwrap_or_else(hostname),
                machine_id,
                machine_os: get("machineOs").unwrap_or_else(machine_os),
                machine_token,
                machine_type,
                source: "config",
            });
        }
    }

    // ② 本机客户端现取。
    if let Ok(risk) = crate::modules::qoder_umid::probe_cached(region, uid) {
        // machineId 用本机的（与刚取的 token 同源）。读不到就放弃 —— 不拿别的机器 id 凑。
        if let Some(machine_id) = machine_id_from_client(region) {
            return Some(MachineIdentity {
                machine_code: risk.machine_code,
                machine_hostname: hostname(),
                machine_id,
                machine_os: machine_os(),
                machine_token: risk.machine_token,
                machine_type: risk.machine_type,
                source: "umid",
            });
        }
    }

    None
}

/// 当前状态（给设置页显示：缺哪几项、machineid 有没有自动读到、身份从哪来）。
pub fn status() -> Value {
    let cfg = load();
    let has = |k: &str| {
        cfg.get(k)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    };
    // 顺手把两条真实取值跑一遍（内部有 30 分钟缓存），状态页看到的 must 与实际发出去的一致。
    let cn = identity(QoderRegion::Cn);
    let global = identity(QoderRegion::Global);
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
        // 这份身份到底从哪来：config = 抓包/导入，umid = 本机客户端现取，none = 都没有。
        "source": {
            "cn": cn.as_ref().map(|i| i.source),
            "global": global.as_ref().map(|i| i.source),
        },
        // 客户端版本（cosy-version 应该用它，不是本工具版本）。
        "clientVersion": crate::modules::qoder_umid::client_version(QoderRegion::Cn)
            .or_else(|| crate::modules::qoder_umid::client_version(QoderRegion::Global)),
        // 现取通道的诊断：组件在哪、跑没跑通、失败原因。
        "probe": {
            "cn": crate::modules::qoder_umid::diagnose(QoderRegion::Cn),
            "global": crate::modules::qoder_umid::diagnose(QoderRegion::Global),
        },
        "ready": cn.is_some() || global.is_some(),
    })
}

/// 为什么这台机器取不到设备身份（取得到就返回 `None`）。
///
/// 返回的是**给人看的原因**，用于签到失败时明确告知 —— 以前这种情况会被归因成
/// 「官方没有签到活动」，用户按那个提示完全无从下手。
pub fn missing_identity_reason(region: QoderRegion) -> Option<String> {
    if identity(region).is_some() {
        return None;
    }
    // ① 连组件都找不到 = 没装客户端（或装在非常规目录）。
    if crate::modules::qoder_umid::locate(region).is_none() {
        return Some(match region {
            QoderRegion::Global => {
                "本机没有找到 Qoder 客户端（国际版），因此取不到设备身份".to_string()
            }
            QoderRegion::Cn => {
                "本机没有找到 Qoder CN 客户端，因此取不到设备身份".to_string()
            }
        });
    }
    // ② 组件在，但跑不出来 / 输出不全。
    if let Err(e) = crate::modules::qoder_umid::probe_cached(region, "") {
        return Some(format!("客户端组件运行失败：{e}"));
    }
    // ③ 组件跑通了，但读不到客户端设备号（多半是客户端还没登录）。
    if machine_id_from_client(region).is_none() {
        return Some(match region {
            QoderRegion::Global => {
                "读不到客户端的设备号（auth.machine-id）—— 客户端装好了但还没登录过".to_string()
            }
            QoderRegion::Cn => {
                "读不到 Qoder CN 客户端的设备号（auth.machine-id）—— 客户端装好了但还没登录过"
                    .to_string()
            }
        });
    }
    Some("设备身份不完整".to_string())
}

/// 请求头里的 `cosy-version`：**客户端版本**，取不到才退回一个占位值。
///
/// 顺序：客户端 `build-manifest.json` → 配置文件里的 `clientVersion` → 常量。
/// 常量只是让请求头形态完整（官方客户端一定有这个头），不代表真实客户端版本 ——
/// 所以它必须排在最后，能读到真值就绝不用它。
pub fn cosy_version(region: QoderRegion) -> String {
    crate::modules::qoder_umid::client_version(region)
        .or_else(|| {
            load()
                .get("clientVersion")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "0.3.4".to_string())
}

// ---------------------------------------------------------------------------
// 跨机器迁移
//
// ## 为什么还要保留导出/导入
//
// 既然本机能现取（[`crate::modules::qoder_umid`]），为什么还留这套？
//
// **不是所有机器都装得了客户端**：Linux 无头机、容器、青龙面板、公司电脑没权限装。
// 对这种机器，唯一的路是把「已经装过客户端的那台」的身份搬过去。
//
// 另外 `machinecode` / `machinetype` / `machinetoken` 客户端**运行时才生成、不落盘**
// （2026-11-01 实测：把 `%APPDATA%\com.qoder.app.stable` 与 `~/.qoder` 共 1 万多个文件
// 全扫过，三个值一处明文都没有）—— 所以"读文件"永远读不到，只能"让组件算"或"整份搬"。
// ---------------------------------------------------------------------------

/// 导出格式标识（与别的导出物区分，导入时先校验）。
pub const EXPORT_FORMAT: &str = "twinswitch.qoder-machine";
/// 导出格式版本。
pub const EXPORT_VERSION: u32 = 1;

/// 导出机器身份（整份可迁移，含 machineid / hostname）。
///
/// 取值走 [`identity`]，所以**装了客户端就有东西可导**（现取），不必先手工抓包。
/// 导出时把 `source` 一并写上，导入方一眼能看出这份是"本机现取"还是"抓包手填"。
pub fn export_identity() -> Result<Value, String> {
    export_identity_for(QoderRegion::Cn)
        .or_else(|e| export_identity_for(QoderRegion::Global).map_err(|_| e))
}

/// 按区域导出（区域决定去哪个安装目录取）。
pub fn export_identity_for(region: QoderRegion) -> Result<Value, String> {
    let id = identity(region).ok_or_else(|| {
        "本机没有可用的 Qoder 机器身份：没装客户端（或组件跑不起来），也没导入过身份".to_string()
    })?;
    Ok(json!({
        "format": EXPORT_FORMAT,
        "version": EXPORT_VERSION,
        "exportedAt": chrono::Local::now().to_rfc3339(),
        "source": id.source,
        "machineCode": id.machine_code,
        "machineType": id.machine_type,
        "machineToken": id.machine_token,
        "machineId": id.machine_id,
        "machineHostname": id.machine_hostname,
        "machineOs": id.machine_os,
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
    fn identity_is_none_when_no_config_and_no_client() {
        // 只有「配置空 **且** 本机没有可用组件」时才必须返回 None。
        // 装了 Qoder 的机器上 identity() 会走现取通道，这条断言不适用。
        let cfg = load();
        let no_cfg = cfg.get("machineCode").is_none();
        let no_component = crate::modules::qoder_umid::locate(QoderRegion::Cn).is_none()
            && crate::modules::qoder_umid::locate(QoderRegion::Global).is_none();
        if no_cfg && no_component {
            assert!(identity(QoderRegion::Cn).is_none());
            assert!(identity(QoderRegion::Global).is_none());
        }
    }

    /// 导出必须与 `identity` 同源：装了客户端就该能导出（不再要求先抓包）。
    #[test]
    fn export_matches_identity_source() {
        let Ok(id) = std::panic::catch_unwind(|| identity(QoderRegion::Cn)) else {
            return;
        };
        match id {
            Some(id) => {
                let v = export_identity_for(QoderRegion::Cn).expect("有身份就应当导得出来");
                assert_eq!(v.get("source").and_then(Value::as_str), Some(id.source));
                assert_eq!(
                    v.get("machineId").and_then(Value::as_str),
                    Some(id.machine_id.as_str())
                );
            }
            None => assert!(export_identity_for(QoderRegion::Cn).is_err()),
        }
    }
}
