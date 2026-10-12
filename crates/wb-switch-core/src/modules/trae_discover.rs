//! Trae 客户端发现：定位各版本的用户数据目录、可执行文件与进程名（移植自 trae-switch src/discover.js）。
//!
//! 同一台机器可能同时装多个 Trae 客户端（国内版 / 国际版，以及合并前遗留的 SOLO），
//! 它们的账号库相互独立；登录态只认 `storage.json` 的 `iCubeAuthInfo://*`。
//!
//! 2026-10 官方把 TRAE SOLO 并进 Trae（国内版 `solo-cn → trae-cn`、国际版 `solo-intl → trae-intl`）
//! ⇒ 这两条 SOLO 记录仍留在这个表里（旧数据要能读），但**不再是可独立使用的客户端**，
//! 判据见 [`has_own_exe`]。

use std::path::{Path, PathBuf};

use serde::Serialize;

/// 客户端定义。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TraeClient {
    pub key: &'static str,
    pub label: &'static str,
    pub user_data: &'static str,
    pub api_host: &'static str,
    /// 会话/任务类接口的 remote 网关（安装目录可读时以 product.json 为准）。
    pub remote_host: &'static str,
    /// **已并入哪个客户端**（`None` = 仍是独立客户端）。
    ///
    /// 2026-10 官方把 TRAE SOLO 并进了 Trae：国内版 `solo-cn → trae-cn`、
    /// 国际版 `solo-intl → trae-intl`（两条线同一次动作）。装一个客户端，双击先开 SOLO
    /// 界面、再双击开 IDE，SOLO 自己的主程序被卸载（只剩日志）。
    /// 所以 SOLO 的**程序与进程都归承载方**，只有数据目录留在原处（成为历史数据）。
    ///
    /// ⚠️ 这个字段只影响「去哪找程序 / 杀哪个进程 / 界面上还算不算一个可选项」，
    /// **不动 `user_data`** —— 老数据仍要能读，否则用户原来的会话就彻底看不见了。
    ///
    /// ⚠️ 判据本身**不带「本机」信息**：合并是产品事实，与这台机器装没装无关。
    /// 「这台机器上还该不该出现」由 `runnable`（有没有它自己的主程序）决定，
    /// 见 `InstalledClient::runnable` 与前端 `@/lib/trae-client-list` 的两段判据。
    pub merged_into: Option<&'static str>,
}

pub const CLIENTS: [TraeClient; 4] = [
    TraeClient {
        key: "trae-cn",
        label: "Trae CN（IDE 国内版）",
        user_data: "Trae CN",
        api_host: "https://api.trae.cn",
        remote_host: "https://trae-api-cn.mchost.guru",
        merged_into: None,
    },
    TraeClient {
        key: "solo-cn",
        label: "TRAE SOLO CN",
        user_data: "TRAE SOLO CN",
        api_host: "https://api.trae.cn",
        remote_host: "https://trae-api-cn.mchost.guru",
        merged_into: Some("trae-cn"),
    },
    TraeClient {
        key: "trae-intl",
        label: "Trae（国际版）",
        user_data: "Trae",
        api_host: "https://api.trae.ai",
        remote_host: "https://api.trae.ai",
        merged_into: None,
    },
    TraeClient {
        key: "solo-intl",
        label: "TRAE SOLO（国际版）",
        user_data: "TRAE SOLO",
        api_host: "https://api.trae.ai",
        remote_host: "https://api.trae.ai",
        merged_into: Some("trae-intl"),
    },
];

pub fn get_client(key: &str) -> Option<&'static TraeClient> {
    CLIENTS.iter().find(|c| c.key == key)
}

pub fn appdata_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join("AppData").join("Roaming"))
}

pub fn localappdata_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join("AppData").join("Local"))
}

pub fn user_data_dir(client: &TraeClient) -> PathBuf {
    appdata_dir().join(client.user_data)
}

pub fn storage_json_path(client: &TraeClient) -> PathBuf {
    user_data_dir(client).join("User").join("globalStorage").join("storage.json")
}

/// 探测客户端主程序路径；未装返回 None。
///
/// 已合并的客户端（见 [`TraeClient::merged_into`]）在自己找不到程序时**回退到承载方** ——
/// SOLO 并入 Trae 之后，SOLO 界面就是承载方那个 `.exe` 开出来的，回退是对「它现在住在哪」的
/// 如实描述。老机器上 SOLO 还独立装着，`detect_exe_own` 会先命中，回退不会生效。
pub fn detect_exe(client: &TraeClient) -> Option<PathBuf> {
    if let Some(p) = detect_exe_own(client) {
        return Some(p);
    }
    if let Some(host) = client.merged_into.and_then(get_client) {
        return detect_exe_own(host);
    }
    None
}

/// 本机还找得到**它自己的**主程序（不含合并回退）—— 即「它仍是一个能独立使用的客户端」。
///
/// 这是「这台机器上它还该不该被当成一个可选项」的唯一判据，**别拿 `detect_exe()` 代替**：
/// 后者对合并后的 SOLO 会回退到承载方的 exe（读网关、杀进程都需要），
/// 于是「已并入」的 SOLO 会被算成「可运行」，首屏又会默认落到它身上。
///
/// 它与合并事实是两件事：合并是产品事实（`merged_into`），能不能跑是本机事实（这里）。
/// 老机器上 SOLO 还独立装着 ⇒ `true` ⇒ 界面照常显示、切号照常放行（别砍功能）。
pub fn has_own_exe(client: &TraeClient) -> bool {
    detect_exe_own(client).is_some()
}

/// 只按**本客户端自己的名字**找主程序（不含合并回退）。
fn detect_exe_own(client: &TraeClient) -> Option<PathBuf> {
    let exe_names: [&str; 2] = match client.key {
        "trae-cn" => ["Trae CN.exe", "Trae.exe"],
        "solo-cn" => ["TRAE SOLO CN.exe", "Trae Solo.exe"],
        "trae-intl" => ["Trae.exe", "Trae CN.exe"],
        _ => ["TRAE SOLO.exe", "Trae Solo.exe"],
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.push(user_data_dir(client));
    dirs.push(localappdata_dir().join("Programs").join(client.user_data));
    dirs.push(localappdata_dir().join(client.user_data));
    for base in [
        std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into()),
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into()),
    ] {
        dirs.push(PathBuf::from(base).join(client.user_data));
    }
    for ch in "CDEFGH".chars() {
        for sub in ["Program Files", "Program Files (x86)"] {
            dirs.push(PathBuf::from(format!("{ch}:\\")).join(sub).join(client.user_data));
        }
    }
    for dir in dirs {
        for name in exe_names {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        // 目录里名字含 Trae 的 exe 兜底
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut hit: Option<PathBuf> = None;
            let mut count = 0usize;
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if !name.ends_with(".exe") {
                    continue;
                }
                count += 1;
                if name.contains("trae") {
                    hit = Some(e.path());
                }
            }
            if hit.is_some() {
                return hit;
            }
            if count == 1 {
                let _ = count;
                for e in std::fs::read_dir(&dir).ok()?.flatten() {
                    if e.file_name().to_string_lossy().to_lowercase().ends_with(".exe") {
                        return Some(e.path());
                    }
                }
            }
        }
    }
    None
}

/// 本客户端**自己的**进程名（不含合并回退）。
fn own_process_names(client: &TraeClient) -> &'static [&'static str] {
    match client.key {
        "trae-cn" => &["Trae CN", "Trae"],
        "solo-cn" => &["TRAE SOLO CN"],
        "trae-intl" => &["Trae"],
        _ => &["TRAE SOLO"],
    }
}

/// 进程名（tasklist 过滤用）。
///
/// ⚠️ 合并回退要**先确认本机真的已合并**（自己的 exe 找不到）才加，不能无条件加：
/// 在「SOLO 与 Trae CN 都还独立装着」的机器上，无条件带上 `Trae CN` 会让
/// 「退出 SOLO 客户端」把 IDE 一起杀掉。
pub fn process_names(client: &TraeClient) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = own_process_names(client).to_vec();
    if client.merged_into.is_some() && detect_exe_own(client).is_none() {
        if let Some(host) = client.merged_into.and_then(get_client) {
            for n in own_process_names(host) {
                if !out.contains(n) {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// 读取安装目录 product.json 的 remote 网关；失败回落默认值。
pub fn remote_host(client: &TraeClient) -> String {
    if let Some(exe) = detect_exe(client) {
        if let Some(dir) = exe.parent() {
            let pj = dir.join("resources").join("app").join("product.json");
            if let Ok(text) = std::fs::read_to_string(&pj) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(h) = v
                        .get("bootConfig")
                        .and_then(|b| b.get("remote"))
                        .and_then(|r| r.get("trae"))
                        .and_then(|t| t.get("normal"))
                        .and_then(|n| n.as_str())
                    {
                        if h.starts_with("https://") || h.starts_with("http://") {
                            return h.trim_end_matches('/').to_string();
                        }
                    }
                }
            }
        }
    }
    client.remote_host.to_string()
}

/// 该客户端当前是否确实处于登录态（两把凭据键都在且非空）。
pub fn has_login_state(client: &TraeClient) -> bool {
    let Ok(text) = std::fs::read_to_string(storage_json_path(client)) else {
        return false;
    };
    let Ok(j) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let auth = j.get("iCubeAuthInfo://icube.cloudide").and_then(|v| v.as_str());
    let tag = j.get("iCubeAuthInfo://usertag").and_then(|v| v.as_str());
    auth.map(|a| !a.trim().is_empty()).unwrap_or(false)
        && tag.map(|t| !t.trim().is_empty()).unwrap_or(false)
}

/// 已安装（有数据目录）的客户端列表。
#[derive(Serialize)]
pub struct InstalledClient {
    pub key: &'static str,
    pub label: &'static str,
    pub user_data_dir: String,
    pub installed: bool,
    pub exe: Option<String>,
    pub has_login: bool,
    /// 已并入哪个客户端（`None` = 独立客户端）。
    pub merged_into: Option<&'static str>,
    /// **本机能不能「独立」把它跑起来**：数据目录在 **且** **它自己的**主程序在。
    ///
    /// ⚠️ 判据必须是「自己的主程序」，**不能算合并回退**：SOLO 并入 Trae 之后，
    /// `detect_exe(solo-cn)` 仍能回退到承载方的 exe（那是对的 —— 退出进程、读网关都要用它），
    /// 但 SOLO 已经**不是一个能独立使用的客户端**了（切号被拦、也没有自己的界面）。
    /// 若这里跟着回退走，`solo-cn` 的使用分最高，首屏又会默认落到它身上 —— 白修。
    ///
    /// 它和 `installed` **不是一回事**，别互相替代：界面拿 `runnable` 决定
    /// 「默认打开谁 / 能不能切号」，拿 `installed` 决定「要不要把这一项显示出来」。
    ///
    /// 判据与 [`has_own_exe`] 是同一条（数据目录在 **且** 自己的主程序在），
    /// 切号护栏 `trae_switch::ensure_switchable` 也走它 —— 三处必须同源。
    pub runnable: bool,
}

pub fn list_installed_clients() -> Vec<InstalledClient> {
    CLIENTS
        .iter()
        .map(|c| {
            let dir = user_data_dir(c);
            let installed = dir.exists();
            // ⚠️ `exe` 也报**它自己的**程序（不回退）：合并后如实显示「未找到」，
            //    比显示一个其实启动了另一个客户端的路径要诚实。
            let own_exe = detect_exe_own(c);
            InstalledClient {
                key: c.key,
                label: c.label,
                user_data_dir: dir.to_string_lossy().into_owned(),
                installed,
                runnable: installed && own_exe.is_some(),
                exe: own_exe.map(|p| p.to_string_lossy().into_owned()),
                has_login: installed && has_login_state(c),
                merged_into: c.merged_into,
            }
        })
        .collect()
}

/// 数据库路径：`%APPDATA%\<userData>\ModularData\ai-agent\database.db`。
pub fn database_path(client: &TraeClient) -> PathBuf {
    user_data_dir(client)
        .join("ModularData")
        .join("ai-agent")
        .join("database.db")
}

/// snapshot 会话目录根。
pub fn snapshot_root(client: &TraeClient) -> PathBuf {
    user_data_dir(client).join("ModularData").join("ai-agent").join("snapshot")
}

/// 全局附加目录（worktrees / mcps 按会话 ID 命名）。
pub fn extra_roots() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default().join(".trae-cn");
    vec![home.join("worktrees"), home.join("mcps")]
}

/// 判断路径是否是某客户端的数据目录（供校验）。
pub fn is_client_root(client: &TraeClient, dir: &Path) -> bool {
    dir == user_data_dir(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 承载方必须真实存在，而且不能是自己（否则 `detect_exe` / `process_names` 的回退会绕死）。
    #[test]
    fn merged_into_points_at_a_real_client() {
        for c in CLIENTS.iter() {
            let Some(host) = c.merged_into else { continue };
            assert!(get_client(host).is_some(), "{} 的承载方「{host}」不存在", c.key);
            assert_ne!(host, c.key, "{} 不能并入自己", c.key);
        }
    }

    /// ⚠️ 合并是**两条线同时**发生的，这条把「改了一半」钉死。
    ///
    /// 反例（真机踩过）：只写了 `solo-cn → trae-cn`，于是国内版 SOLO 从标签条上消失了，
    /// 国际版 SOLO 还挂在上面 —— 用户看到的就是「国际版的那个怎么还在」。
    /// 这类「同一件事只做一半」的漏改在界面上很难看出是漏改（另一条线本机可能压根没装）。
    #[test]
    fn both_solo_lines_are_merged_into_their_own_ide() {
        assert_eq!(get_client("solo-cn").unwrap().merged_into, Some("trae-cn"));
        assert_eq!(get_client("solo-intl").unwrap().merged_into, Some("trae-intl"));
        // 承载方本身必须仍是独立客户端 —— 否则回退链没有终点。
        assert!(get_client("trae-cn").unwrap().merged_into.is_none());
        assert!(get_client("trae-intl").unwrap().merged_into.is_none());
    }
}
