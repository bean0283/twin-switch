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
        label: "Trae CN（国内版）",
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
///
/// 四级，**顺序即优先级**（2026-10-18 加前三级，起因：用户把 Trae 装在自定义目录，
/// 固定目录扫不到就报「未找到主程序，请在设置里手动指定路径」，而当时设置里
/// 根本没有那一项）：
///
/// 1. **用户在设置里手动指定**的路径（[`crate::modules::settings::trae_exe_override`]）——
///    文件不存在就当没指定，自动探测继续兜底；
/// 2. **它自己正在跑的进程**（`QueryFullProcessImageNameW`）—— 客户端开着时最准；
/// 3. **注册表卸载项**里的 `DisplayIcon` / `InstallLocation` —— 装过就有，与是否在跑无关；
/// 4. 固定的几个常见目录（原有逻辑）。
///
/// ⚠️ 2/3 都**必须过** [`client_of_exe`] 这一关：`Trae.exe` 同时出现在国内版与国际版的
/// 候选名里，只看文件名会把国际版的路径当成国内版的 —— 于是「切换国内版」实际启动了
/// 国际版，而界面上一切正常。判定靠安装目录名 + `product.json` 的网关地址。
fn detect_exe_own(client: &TraeClient) -> Option<PathBuf> {
    if let Some(p) = crate::modules::settings::trae_exe_override(client.key) {
        return Some(p);
    }
    if let Some(p) = from_running_process(client) {
        return Some(p);
    }
    for exe in registry_exes() {
        if exe.is_file() && client_of_exe(&exe) == Some(client.key) {
            return Some(exe);
        }
    }
    scan_known_dirs(client)
}

/// 本客户端认的主程序文件名（顺序即优先）。
fn exe_names_of(client: &TraeClient) -> [&'static str; 2] {
    match client.key {
        "trae-cn" => ["Trae CN.exe", "Trae.exe"],
        "solo-cn" => ["TRAE SOLO CN.exe", "Trae Solo.exe"],
        "trae-intl" => ["Trae.exe", "Trae CN.exe"],
        _ => ["TRAE SOLO.exe", "Trae Solo.exe"],
    }
}

/// 这个 exe 属于哪个客户端 —— `None` = 认不出（宁可不认，也不猜）。
///
/// 判据一：**安装目录名**等于该客户端的用户数据目录名（`Trae CN` / `Trae` / …）。
/// 判据二：读它自己的 `resources/app/product.json`，看 remote 网关与谁一致 ——
/// 自定义目录（`D:\soft\Trae`）靠这条才认得出。
fn client_of_exe(exe: &Path) -> Option<&'static str> {
    let dir_name = exe.parent()?.file_name()?.to_string_lossy().to_string();
    for c in CLIENTS.iter() {
        if dir_name.eq_ignore_ascii_case(c.user_data) {
            return Some(c.key);
        }
    }
    let remote = product_remote_host(exe)?;
    CLIENTS
        .iter()
        .find(|c| remote.eq_ignore_ascii_case(c.remote_host))
        .map(|c| c.key)
}

/// 读 `<安装目录>/resources/app/product.json` 的 `bootConfig.remote.trae.normal`。
fn product_remote_host(exe: &Path) -> Option<String> {
    let pj = exe.parent()?.join("resources").join("app").join("product.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(pj).ok()?).ok()?;
    let h = v
        .get("bootConfig")?
        .get("remote")?
        .get("trae")?
        .get("normal")?
        .as_str()?
        .trim_end_matches('/')
        .to_string();
    (!h.is_empty()).then_some(h)
}

/// 从**正在运行的它自己的进程**反查 exe 路径。
fn from_running_process(client: &TraeClient) -> Option<PathBuf> {
    let names: Vec<String> = own_process_names(client).iter().map(|s| s.to_string()).collect();
    for p in crate::modules::process_list::find_by_names_ex(&names) {
        // 只认主进程：Electron 的 GPU / renderer 子进程用同一个映像名，但
        // 「谁的路径」都一样，取到就是安装目录；这里不额外挑，先取第一个能认出来的。
        let Some(exe) = crate::modules::process_list::exe_path_of_pid(p.pid) else { continue };
        if !exe.is_file() {
            continue;
        }
        let base = exe
            .file_name()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !exe_names_of(client)
            .iter()
            .any(|n| n.to_ascii_lowercase() == base)
        {
            continue;
        }
        if client_of_exe(&exe) == Some(client.key) {
            return Some(exe);
        }
    }
    None
}

/// 从**正在运行的进程**取某客户端的主程序路径 —— 设置页「用运行中的客户端」用。
///
/// 与 [`from_running_process`] 的区别：**不校验安装目录**。用户点这个按钮就是
/// 「它明明开着，把它的路径给我」，此时目录名/网关都可能不标准（自定义安装）。
/// 仍要求文件名里带 `trae`，免得把同映像名的其它程序（含本工具自己）当成它。
pub fn exe_from_running_lenient(client_key: &str) -> Option<PathBuf> {
    let client = get_client(client_key)?;
    let mut names: Vec<String> = own_process_names(client).iter().map(|s| s.to_string()).collect();
    // 已合并的 SOLO：它的界面就是承载方开出来的，进程名归承载方。
    if let Some(h) = client.merged_into.and_then(get_client) {
        for n in own_process_names(h) {
            if !names.iter().any(|x| x == n) {
                names.push((*n).to_string());
            }
        }
    }
    for p in crate::modules::process_list::find_by_names_ex(&names) {
        let Some(exe) = crate::modules::process_list::exe_path_of_pid(p.pid) else {
            continue;
        };
        let base = exe
            .file_name()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if exe.is_file() && base.contains("trae") {
            return Some(exe);
        }
    }
    None
}

/// 注册表里登记的 Trae 主程序路径（**本进程内缓存一次**）。
///
/// 走 `reg query` 而不是再引一个注册表 crate：这是**兜底路径**，只在固定目录扫不到的
/// 时候跑一次，输出解析足够简单（`REG_SZ` 行 + 值里带 `.exe`）。
/// 缓存是必须的 —— `list_installed_clients()` 每次刷新要对 4 个客户端各问一遍。
fn registry_exes() -> Vec<PathBuf> {
    let Ok(g) = registry_cell().lock() else {
        return probe_registry();
    };
    if let Some(v) = g.as_ref() {
        return v.clone();
    }
    drop(g);
    let found = probe_registry();
    if let Ok(mut w) = registry_cell().lock() {
        *w = Some(found.clone());
    }
    found
}

/// 注册表探测结果的进程内缓存（只算一次；`None` = 还没算过）。
static REGISTRY_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<Vec<PathBuf>>>> =
    std::sync::OnceLock::new();

fn registry_cell() -> &'static std::sync::Mutex<Option<Vec<PathBuf>>> {
    REGISTRY_CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// 注册表里登记的主程序路径（**只读诊断用**：`examples/trae_discover_probe` 会打印它）。
///
/// 「装在自定义目录却找不到」时，先看这里有没有它 —— 有 = 我们的判定（`client_of_exe`）
/// 没认出来；没有 = 安装包压根没登记 `DisplayIcon`/`InstallLocation`，只能手动指定。
pub fn registry_candidates() -> Vec<PathBuf> {
    registry_exes()
}

/// 丢掉注册表探测缓存 —— 用户装完客户端 / 改完设置点「重新检测」时调，
/// 否则「重新检测」会永远返回第一次的结果（**「没重新检测」和「检测不到」长得一样**）。
pub fn invalidate_discovery_cache() {
    if let Ok(mut g) = registry_cell().lock() {
        *g = None;
    }
}

#[cfg(windows)]
fn probe_registry() -> Vec<PathBuf> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY,
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
    };
    use windows::core::{PCWSTR, PWSTR};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 读一个 REG_SZ 值（读不到 / 不是字符串都返回 `None`）。
    fn query_sz(key: HKEY, name: &str) -> Option<String> {
        let name_w = wide(name);
        let mut ty = Default::default();
        let mut size: u32 = 0;
        unsafe {
            if RegQueryValueExW(
                key,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                None,
                Some(&mut size),
            ) != ERROR_SUCCESS
                || size == 0
            {
                return None;
            }
        }
        let mut buf = vec![0u8; size as usize + 2];
        unsafe {
            if RegQueryValueExW(
                key,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut size),
            ) != ERROR_SUCCESS
            {
                return None;
            }
        }
        let units: Vec<u16> = buf[..size as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|c| *c != 0)
            .collect();
        String::from_utf16(&units).ok().map(|s| s.trim().to_string())
    }

    /// 把一条候选值收进结果：`DisplayIcon` 形如 `"C://X//Trae CN.exe",0`。
    fn push_candidate(value: &str, out: &mut Vec<PathBuf>) {
        let raw = value.split(',').next().unwrap_or(value).trim().trim_matches('"');
        let lower = raw.to_ascii_lowercase();
        if !lower.contains("trae") {
            return;
        }
        let p = PathBuf::from(raw);
        if p.is_file() && !out.contains(&p) {
            out.push(p);
        }
    }

    /// `InstallLocation` 只有一个目录，得自己拼 exe 名。
    fn push_location(value: &str, out: &mut Vec<PathBuf>) {
        let dir = PathBuf::from(value.trim().trim_matches('"'));
        if !dir.is_dir() {
            return;
        }
        for name in ["Trae CN.exe", "Trae.exe", "TRAE SOLO CN.exe", "TRAE SOLO.exe"] {
            let p = dir.join(name);
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        }
    }

    let mut out: Vec<PathBuf> = Vec::new();
    for (root, path) in [
        (HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKEY_LOCAL_MACHINE, r"Software\Microsoft\Windows\CurrentVersion\Uninstall"),
        (HKEY_LOCAL_MACHINE, r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
    ] {
        let root_w = wide(path);
        let mut hkey = HKEY::default();
        unsafe {
            if RegOpenKeyExW(root, PCWSTR(root_w.as_ptr()), 0, KEY_READ, &mut hkey) != ERROR_SUCCESS
            {
                continue;
            }
            let mut index = 0u32;
            loop {
                let mut name = [0u16; 512];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(
                    hkey,
                    index,
                    PWSTR(name.as_mut_ptr()),
                    &mut len,
                    None,
                    PWSTR::null(),
                    None,
                    None,
                ) != ERROR_SUCCESS
                {
                    break;
                }
                index += 1;
                let sub_name = String::from_utf16_lossy(&name[..len as usize]);
                let sub_w = wide(&sub_name);
                let mut sub = HKEY::default();
                if RegOpenKeyExW(hkey, PCWSTR(sub_w.as_ptr()), 0, KEY_READ, &mut sub)
                    != ERROR_SUCCESS
                {
                    continue;
                }
                if let Some(v) = query_sz(sub, "DisplayIcon") {
                    push_candidate(&v, &mut out);
                }
                if let Some(v) = query_sz(sub, "InstallLocation") {
                    push_location(&v, &mut out);
                }
                let _ = RegCloseKey(sub);
            }
            let _ = RegCloseKey(hkey);
        }
    }
    out
}

#[cfg(not(windows))]
fn probe_registry() -> Vec<PathBuf> {
    Vec::new()
}

/// 原有的固定目录扫描（常见安装位置）。
fn scan_known_dirs(client: &TraeClient) -> Option<PathBuf> {
    let exe_names = exe_names_of(client);
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
        // 盘符根 + 常见的一级子目录：`D:\Trae CN`、`D:\Program Files\Trae CN`。
        // （用户自定义安装最常落在盘符根或一个自建目录里，深度再大就靠注册表/手动指定。）
        dirs.push(PathBuf::from(format!("{ch}:\\")).join(client.user_data));
        for sub in ["Program Files", "Program Files (x86)", "Apps", "Software"] {
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
///
/// ⚠️ 与 [`process_names`] 的分工见那里的注释：**「它自己在不在跑」只能看这里**。
pub fn own_process_names(client: &TraeClient) -> &'static [&'static str] {
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

/// **它自己的**程序是否在跑 —— ⚠️ 只用 [`own_process_names`]，**不带合并回退**。
///
/// 这条判据回答的是「这台机器上，**它自己**的进程在不在」，与「它现在住在哪」（[`process_names`]）
/// 是两件事，**不要互相替代**：
/// - 已合并、本机已无自己程序的 SOLO ⇒ 它的映像名不可能出现在进程表里 ⇒ 恒 `false`（正确）。
///   走 [`crate::modules::trae_switch::is_running`] 会回退成承载方 `Trae CN` 的映像名 ⇒
///   **只要用户开着 IDE 就谎报「SOLO 正在运行」**，而且「退出 SOLO」会连正在用的 IDE 一起杀。
/// - 老机器上 SOLO 仍独立装着且在跑 ⇒ 命中自己的映像名 ⇒ `true`（正确，别砍功能）。
///
/// 「要不要为写它的库而结束进程」的判据见
/// [`crate::modules::trae_switch::stop_for_maintenance`]。
pub fn own_running(client: &TraeClient) -> bool {
    let names: Vec<String> = own_process_names(client).iter().map(|s| s.to_string()).collect();
    crate::modules::process_list::find_by_names(&names)
        .iter()
        .any(|(n, _)| !n.to_lowercase().contains("ai-agent"))
}

/// 是不是「已并入、但本机已没有它自己的程序」——**只剩历史数据**的那种。
///
/// ⚠️ 这是**界面摘不摘**的同一条判据（前端 `@/lib/trae-client-list` 的
/// `!(merged_into && runnable === false)`）：两侧必须同源。
/// 注意它与「已并入」不是一回事 —— 老机器上 SOLO 还独立装着（`runnable = true`）时这里是 `false`。
pub fn is_merged_shadow(c: &InstalledClient) -> bool {
    c.merged_into.is_some() && !c.runnable
}

/// 承载方（合并进的那个客户端）的显示名 —— 拼提示文案用。
pub fn merged_host_label(c: &InstalledClient) -> Option<&'static str> {
    c.merged_into.and_then(get_client).map(|h| h.label)
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

/// 读取安装目录 product.json 的 `appVersion` —— 就是客户端发给服务端的
/// `ClientVersion` / `IDEVersion`（本机 Trae CN = `3.4.2`）。
///
/// ⚠️ **绝不能拿 `storage.json` 的 `iCubeLastVersion` 顶替**：那是 tronBuildVersion
///    （本机 `2.3.91xxx`），与服务端认的 `appVersion` 不是一回事 —— 参考实现
///    `trae_refresh.js` 是踩过这个坑才把这条写进注释的。
///
/// 返回 `None` = 客户端没装 / 读不到，调用方自行回落。
pub fn app_version(client: &TraeClient) -> Option<String> {
    let exe = detect_exe(client)?;
    let dir = exe.parent()?;
    let pj = dir.join("resources").join("app").join("product.json");
    let text = std::fs::read_to_string(&pj).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("appVersion")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
    /// 这个 exe 是**用户手动指定**的（设置里填的），还是自动探测出来的。
    ///
    /// 界面要能分开显示：自动探测的结果随时可能变（换了安装位置），手动指定的是
    /// 用户明确意图 —— 混成一句「路径：X」会让用户以为「我明明没设，哪来的路径」。
    pub exe_manual: bool,
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
                exe_manual: crate::modules::settings::trae_exe_override(c.key).is_some(),
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

    fn fake(key: &'static str, installed: bool, runnable: bool) -> InstalledClient {
        let c = get_client(key).unwrap();
        InstalledClient {
            key: c.key,
            label: c.label,
            user_data_dir: format!("C:/fake/{}", c.user_data),
            installed,
            exe: if runnable { Some("C:/fake/x.exe".into()) } else { None },
            has_login: false,
            merged_into: c.merged_into,
            runnable,
            exe_manual: false,
        }
    }

    /// T61 的判据本体：`is_merged_shadow` 必须只在「已并入 **且** 本机已无自己程序」时为真。
    #[test]
    fn merged_shadow_requires_no_own_program_on_this_machine() {
        // 合并事实成立 + 本机没程序 ⇒ 影子（只剩历史数据）
        assert!(is_merged_shadow(&fake("solo-cn", true, false)));
        assert!(is_merged_shadow(&fake("solo-intl", true, false)));
        // ⚠️ 反证一：老机器上 SOLO 还独立装着 ⇒ **不是**影子，别把它当影子处理（= 砍功能）
        assert!(!is_merged_shadow(&fake("solo-cn", true, true)));
        // ⚠️ 反证二：承载方自己永远不是影子（它没有 merged_into）
        assert!(!is_merged_shadow(&fake("trae-cn", true, true)));
        assert!(!is_merged_shadow(&fake("trae-cn", true, false)));
        // 数据目录都没了就无从谈起（installed=false 由调用方先滤掉）
        assert!(is_merged_shadow(&fake("solo-cn", false, false)));
    }

    /// 「它自己在不在跑」只能看自己的映像名 —— 这条把「回退承载方」那个坑钉死。
    #[test]
    fn own_process_names_never_include_the_merged_host() {
        let solo = get_client("solo-cn").unwrap();
        let host = get_client("trae-cn").unwrap();
        assert_eq!(own_process_names(solo).to_vec(), vec!["TRAE SOLO CN"]);
        // 回退版**会**带上承载方（`Trae CN`/`Trae`）—— 这是给「启动 / 读网关」用的，
        // 也正是「判定在不在跑」绝不能用的那一版。
        let with_fallback = process_names(solo);
        for n in own_process_names(host) {
            assert!(
                !own_process_names(solo).contains(n),
                "own_process_names(solo-cn) 不该含承载方映像名 {n}"
            );
            let _ = with_fallback.contains(n); // 回退版含不含取决于本机装没装，不作断言
        }
    }

    /// 承载方显示名 —— 提示文案「已并入 Trae CN（国内版）」靠它。
    #[test]
    fn merged_host_label_names_the_host() {
        assert_eq!(merged_host_label(&fake("solo-cn", true, false)), Some("Trae CN（国内版）"));
        assert_eq!(merged_host_label(&fake("trae-cn", true, true)), None);
    }
}
