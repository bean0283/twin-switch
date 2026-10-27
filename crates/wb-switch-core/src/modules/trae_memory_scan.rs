//! 进程内存扫描：从运行中的 Trae 客户端进程里提取 SQLCipher 加密密钥
//! （移植自 trae-session-export decrypt_tool/scan_memory.py，思路源自 wechat-decrypt）。
//!
//! 原理：
//!   1. 用 tasklist 找含 `ai_agent` 模块的客户端主进程（按内存占用取最大者）
//!   2. VirtualQueryEx 枚举全部 committed 可读内存区
//!   3. ReadProcessMemory 读回，在字节流里找 64 位十六进制密钥候选
//!   4. 用数据库第 1 页的盐 + HMAC-SHA512 逐候选验证（SQLCipher 4 的页面校验）
//!
//! 只支持 Windows（Trae 客户端密钥就藏在主进程内存里；macOS 方案不在本模块范围）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha512;

const PAGE_SZ: usize = 4096;
const KEY_SZ: usize = 32;
const SALT_SZ: usize = 16;
const RESERVE_SZ: usize = 80;
const MEM_COMMIT: u32 = 0x1000;

/// 数据库首页信息（验证密钥用）。
#[derive(Debug, Clone)]
pub struct DbInfo {
    pub path: PathBuf,
    pub page1: Vec<u8>,
    pub salt_hex: String,
}

pub fn load_database_info(path: &Path) -> Option<DbInfo> {
    let mut f = std::fs::File::open(path).ok()?;
    use std::io::Read;
    let mut page1 = vec![0u8; PAGE_SZ];
    let n = f.read(&mut page1).ok()?;
    if n < PAGE_SZ {
        return None;
    }
    let salt_hex = hex_encode(&page1[..SALT_SZ]);
    Some(DbInfo {
        path: path.to_path_buf(),
        page1,
        salt_hex,
    })
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn hex_char(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b)
}

/// 用 HMAC-SHA512 校验密钥候选（SQLCipher 4 页面 1 的校验结构）。
fn verify_enc_key(enc_key: &[u8], page1: &[u8]) -> bool {
    let salt = &page1[..SALT_SZ];
    let mac_salt: Vec<u8> = salt.iter().map(|b| b ^ 0x3a).collect();
    let mut mac_key = [0u8; KEY_SZ];
    pbkdf2::pbkdf2_hmac::<Sha512>(enc_key, &mac_salt, 2, &mut mac_key);
    let hmac_data = &page1[SALT_SZ..PAGE_SZ - RESERVE_SZ + 16];
    let stored = &page1[PAGE_SZ - 64..];
    let mut mac = match Hmac::<Sha512>::new_from_slice(&mac_key) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(hmac_data);
    mac.update(&1u32.to_le_bytes());
    let digest = mac.finalize().into_bytes();
    digest.as_slice() == stored
}

// ---------------------------------------------------------------------------
// 进程定位
// ---------------------------------------------------------------------------

fn run_tasklist(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("tasklist");
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.stdin(std::process::Stdio::null());
    let out = cmd.output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 该客户端的进程镜像名（必须与所选客户端一一对应，避免拿别的客户端的
/// 内存去验证本客户端的库盐——那是「0 候选」的典型原因）。
pub fn process_names_for_client(client_key: &str) -> &'static [&'static str] {
    match client_key {
        "trae-cn" => &["Trae CN.exe"],
        "solo-cn" => &["TRAE SOLO CN.exe"],
        "trae-intl" => &["Trae.exe"],
        "solo-intl" => &["TRAE SOLO.exe"],
        _ => &["Trae CN.exe", "TRAE SOLO CN.exe", "Trae.exe", "TRAE SOLO.exe"],
    }
}

/// 解析**不带** `/M` 的 5 列 CSV 行：`"镜像","PID","会话名","会话#","内存"`。
///
/// 内存列形如 `"541,234 K"`，永远是**最后一列** —— 早先按固定下标取第 3 列，
/// 拿到的其实是「会话名」，所有进程内存都解析成 0，排序直接失效。
fn parse_tasklist_mem_line(line: &str) -> Option<(u32, u64)> {
    let s = line.trim().trim_matches('\0');
    if s.is_empty() || s.starts_with("INFO:") || !s.starts_with('"') {
        return None;
    }
    let parts: Vec<&str> = s.trim_matches('"').split("\",\"").collect();
    if parts.len() < 2 {
        return None;
    }
    let pid: u32 = parts[1].trim().parse().ok()?;
    let mem: u64 = parts
        .last()
        .filter(|x| x.contains('K'))
        .map(|x| x.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    Some((pid, mem))
}

/// 解析 `tasklist /M /FO CSV /NH` 的一行：`"镜像","PID","模块,列表"`。
///
/// 模块列本身含逗号但被引号整体包住，所以按 `","` 切是安全的。
fn parse_tasklist_line(line: &str) -> Option<(String, u32, String)> {
    let s = line.trim().trim_matches('\0');
    if !s.starts_with('"') {
        return None;
    }
    let s = s.strip_prefix('"').unwrap_or(s);
    let s = s.strip_suffix('"').unwrap_or(s);
    let parts: Vec<&str> = s.split("\",\"").collect();
    if parts.len() < 2 {
        return None;
    }
    let image = parts[0].to_string();
    let pid: u32 = parts[1].trim().parse().ok()?;
    let modules = parts.get(2).copied().unwrap_or("").to_string();
    Some((image, pid, modules))
}

/// 该客户端全部进程：`(pid, 内存KB, 是否加载 ai_agent 模块)`。
///
/// ⚠️ 必须查两次 tasklist，两次的列数不一样：
///   · 不带 `/M`：5 列（镜像, PID, 会话名, 会话#, 内存），**内存是最后一列**；
///   · 带 `/M`  ：3 列（镜像, PID, 模块），**没有内存列**。
/// 早先按固定下标取第 3 列当内存，结果所有进程内存都解析成 0，排序直接失效
/// （报告里踩过的坑，这里逐字保留）。
fn collect_processes(client_key: &str) -> Vec<(u32, u64, bool)> {
    let names = process_names_for_client(client_key);
    let mut collected: Vec<(u32, u64, bool)> = Vec::new();
    for name in names {
        // tasklist 的 /FI 值含空格时不能加引号（实测加引号报 Invalid argument），
        // 不加引号反而能正确匹配 TRAE SOLO CN.exe 这类镜像名。
        let filter = format!("IMAGENAME eq {name}");

        let mut mem_of: HashMap<u32, u64> = HashMap::new();
        if let Some(out) = run_tasklist(&["/FI", &filter, "/FO", "CSV", "/NH"]) {
            for line in out.lines() {
                let s = line.trim().trim_matches('\0');
                if s.is_empty() || s.starts_with("INFO:") {
                    continue;
                }
                if let Some((pid, mem)) = parse_tasklist_mem_line(line) {
                    mem_of.insert(pid, mem);
                }
            }
        }

        let Some(out) = run_tasklist(&["/FI", &filter, "/M", "/FO", "CSV", "/NH"]) else {
            // 拿不到模块列表也不能丢掉进程：标记为「没有 ai_agent」，走多进程兜底。
            for (pid, mem) in mem_of {
                collected.push((pid, mem, false));
            }
            continue;
        };
        for line in out.lines() {
            let Some((_image, pid, modules)) = parse_tasklist_line(line) else {
                continue;
            };
            if pid == 0 {
                continue;
            }
            let mem = mem_of.get(&pid).copied().unwrap_or(0);
            let has_agent = modules.to_lowercase().contains("ai_agent");
            collected.push((pid, mem, has_agent));
        }
    }
    collected
}

/// 密钥在进程里的存放布局 —— 直接决定「要扫几个进程」，也就决定耗时。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyLayout {
    /// 旧版：密钥只在加载了 `ai_agent` 模块的进程里 ⇒ 只扫这几个，命中即停。
    Legacy,
    /// 新版（Electron 多进程）：所有进程都不再加载 `ai_agent`，
    /// 密钥由 `--type=utility` 子进程持有，只能按内存降序逐个扫。
    MultiProcess,
}

impl KeyLayout {
    fn as_str(self) -> &'static str {
        match self {
            KeyLayout::Legacy => "legacy",
            KeyLayout::MultiProcess => "multi",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "legacy" => Some(KeyLayout::Legacy),
            "multi" => Some(KeyLayout::MultiProcess),
            _ => None,
        }
    }
}

/// 从这个版本起改用多进程布局（实测 3.4.9：14 个进程里已无任何 ai_agent 模块）。
///
/// ⚠️ 阈值只决定「先试哪条路」，**不决定正确性** —— 没命中会自动改走另一条，
/// 所以更早的版本不必逐一验证（最多第一次多花一趟）。
const MULTI_PROCESS_SINCE: (u32, u32, u32) = (3, 4, 9);

/// 解析 `3.4.9` / `3.4.9-beta.1` / `4.0` 这类版本号。
fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().split('.');
    let a: u32 = it.next()?.trim().parse().ok()?;
    let b: u32 = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let c: u32 = it
        .next()
        .unwrap_or("0")
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    Some((a, b, c))
}

fn layout_for_version(v: &str) -> KeyLayout {
    match parse_version(v) {
        Some(t) if t >= MULTI_PROCESS_SINCE => KeyLayout::MultiProcess,
        _ => KeyLayout::Legacy,
    }
}

/// 布局结论的缓存文件（`<store>/trae/scan-layout.json`）。
///
/// 为什么缓存：版本号只代表「厂商改结构的那一刻」，而真正命中的是哪条路才是事实。
/// 下次同版本直接走已验证的那条，不再多花一趟。
fn layout_cache_path() -> PathBuf {
    crate::modules::config::store_dir()
        .join("trae")
        .join("scan-layout.json")
}

fn read_cached_layout(client_key: &str) -> Option<(String, KeyLayout)> {
    let text = std::fs::read_to_string(layout_cache_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let item = v.get(client_key)?;
    let version = item.get("version").and_then(|x| x.as_str())?.to_string();
    let layout = KeyLayout::parse(item.get("layout").and_then(|x| x.as_str())?)?;
    Some((version, layout))
}

fn write_cached_layout(client_key: &str, version: &str, layout: KeyLayout) {
    let path = layout_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    v[client_key] = serde_json::json!({ "version": version, "layout": layout.as_str() });
    if let Ok(text) = serde_json::to_string_pretty(&v) {
        let _ = std::fs::write(path, text);
    }
}

/// 判定本次该走哪条路。返回 `(客户端版本, 先试哪条布局)`。
///
/// 优先级：同版本已验证的结论 → 版本阈值 → 读不到版本先走旧版
/// （旧版在「没有 ai_agent 进程」时**一次内存都不读**，立刻回落，成本几乎为零）。
pub fn resolve_layout(client_key: &str) -> (Option<String>, KeyLayout) {
    let version = crate::modules::trae_discover::get_client(client_key)
        .and_then(crate::modules::trae_discover::app_version);
    if let Some(v) = &version {
        if let Some((cached_version, layout)) = read_cached_layout(client_key) {
            if &cached_version == v {
                return (version.clone(), layout);
            }
        }
        return (version.clone(), layout_for_version(v));
    }
    (version, KeyLayout::Legacy)
}

/// 旧版路径：只取加载了 ai_agent 的进程，组内按内存降序。
///
/// ⚠️ 必须与旧实现选中的进程**完全一致**：旧实现是「按内存降序取第一个带 ai_agent 的」，
/// 也就是这里排序后的第 0 个。
fn legacy_candidates(procs: &[(u32, u64, bool)]) -> Vec<(u32, u64)> {
    let mut v: Vec<(u32, u64)> = procs
        .iter()
        .filter(|(_, _, has_agent)| *has_agent)
        .map(|(pid, mem, _)| (*pid, *mem))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// 新版路径：全部进程按内存降序。
fn multi_candidates(procs: &[(u32, u64, bool)]) -> Vec<(u32, u64)> {
    let mut v: Vec<(u32, u64)> = procs.iter().map(|(pid, mem, _)| (*pid, *mem)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

// ---------------------------------------------------------------------------
// Windows 内存读取
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_ACCESS_RIGHTS};

    const PROCESS_QUERY_INFORMATION: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0400);
    const PROCESS_VM_READ: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0010);

    pub fn open(pid: u32) -> Option<HANDLE> {
        unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok() }
    }

    pub fn close(h: HANDLE) {
        unsafe {
            let _ = CloseHandle(h);
        }
    }

    /// 枚举 committed + 可读的内存区（单区 ≤500MB，与参考实现一致）。
    ///
    /// 第三个值 = **是否私有区**（`MEM_PRIVATE`）。密钥只在私有堆里，
    /// 而 IMAGE 段（DLL/EXE 映射）占进程 60%~80% —— 先扫私有区能少读一大半。
    pub fn enum_regions(h: HANDLE) -> Vec<(u64, usize, bool)> {
        let mut regions = Vec::new();
        let mut addr: u64 = 0;
        let max_addr: u64 = 0x7FFF_FFFF_FFFF;
        loop {
            if addr >= max_addr {
                break;
            }
            let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            let ret = unsafe {
                VirtualQueryEx(
                    h,
                    Some(addr as *const std::ffi::c_void),
                    &mut mbi as *mut MEMORY_BASIC_INFORMATION,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if ret == 0 {
                break;
            }
            if mbi.State.0 == MEM_COMMIT {
                let protect = mbi.Protect.0;
                let readable = matches!(protect, 0x02 | 0x04 | 0x08 | 0x10 | 0x20 | 0x40 | 0x80);
                if readable && mbi.RegionSize > 0 && mbi.RegionSize < 500 * 1024 * 1024 {
                    // 0x20000 = MEM_PRIVATE；IMAGE / MAPPED 一律归「其余区域」。
                    let is_private = mbi.Type.0 == 0x20000;
                    regions.push((mbi.BaseAddress as u64, mbi.RegionSize, is_private));
                }
            }
            let next = (mbi.BaseAddress as u64).wrapping_add(mbi.RegionSize as u64);
            if next <= addr {
                break;
            }
            addr = next;
        }
        regions
    }

    pub fn read_mem(h: HANDLE, addr: u64, size: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; size];
        let mut read: usize = 0;
        let ok = unsafe {
            ReadProcessMemory(
                h,
                addr as *const std::ffi::c_void,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                size,
                Some(&mut read),
            )
        };
        if ok.is_ok() {
            buf.truncate(read);
            Some(buf)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// 扫描
// ---------------------------------------------------------------------------

/// 在内存块里找 SQLCipher 密钥候选并验证。
///
/// 真实形态是 `x'<64位hex密钥><32位hex库盐>'` —— 密钥后面**紧跟库盐**。
/// 于是先用 memcmp 预筛「窗口后 32 字符是否等于库盐」，只对命中窗口做昂贵的
/// PBKDF2-HMAC-SHA512。实测 14.5 万个候选里真正需要算的只有个位数。
///
/// `brute = true` 时退化为「每个 64 位 hex 窗口都校验」的全量兜底 ——
/// 只留给历史上不带盐的形态。**兼容性不降，日常不慢。**
fn scan_region(
    data: &[u8],
    base: u64,
    db: &DbInfo,
    candidates: &mut usize,
    brute: bool,
) -> Option<(String, u64)> {
    let salt_hex = db.salt_hex.as_bytes();
    let n = data.len();
    if n < 64 {
        return None;
    }
    let mut i = 0usize;
    while i + 64 <= n {
        if !data[i..i + 64].iter().all(|b| hex_char(*b)) {
            i += 1;
            continue;
        }
        // 预筛：64 位密钥之后是否紧跟库盐
        let salt_end = i + 64 + salt_hex.len();
        let salt_adjacent = salt_end <= n && data[i + 64..salt_end].eq_ignore_ascii_case(salt_hex);

        if salt_adjacent || brute {
            *candidates += 1;
            let win = &data[i..i + 64];
            if verify_enc_key(&hex_to_bytes(win), &db.page1) {
                let key_hex = std::str::from_utf8(win)
                    .map(str::to_string)
                    .unwrap_or_else(|_| hex_encode(win));
                return Some((key_hex, base + i as u64));
            }
        }
        i += 1; // 全滑动：密钥嵌在更长 hex 段中也能命中
    }
    None
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// 从十六进制字符序列还原密钥字节（容忍奇数长度，尾部截断）。
fn hex_to_bytes(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0usize;
    while i + 1 < b.len() {
        out.push((hex_val(b[i]) << 4) | hex_val(b[i + 1]));
        i += 2;
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub found: bool,
    pub key: Option<String>,
    pub address: Option<String>,
    pub candidates: usize,
    pub scanned_mb: u64,
    pub elapsed_ms: u64,
    pub pid: Option<u32>,
    /// 客户端版本（读不到就是 `None`，界面上显示「未知」）。
    pub client_version: Option<String>,
    /// 这次实际生效的布局（`legacy` / `multi`）。
    pub layout: KeyLayout,
    pub message: String,
}

/// 扫描时选取的内存区类型。分两遍扫：先私有堆（快、命中率高），再其余区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionFilter {
    Private,
    Rest,
}

impl RegionFilter {
    fn keep(self, is_private: bool) -> bool {
        match self {
            RegionFilter::Private => is_private,
            RegionFilter::Rest => !is_private,
        }
    }
    fn label(self) -> &'static str {
        match self {
            RegionFilter::Private => "私有堆",
            RegionFilter::Rest => "其余区域",
        }
    }
}

/// 一次扫描的累计状态（跨多个计划步骤复用，命中即停）。
#[derive(Default)]
struct Tally {
    candidates: usize,
    scanned: u64,
    read_fail: usize,
    hit: Option<(String, u64)>,
    pid: Option<u32>,
}

/// 按给定的进程列表扫一遍：逐个开句柄 → 枚举内存区 → 按 `filter` 过滤 → 逐区读。
///
/// 命中立刻返回（`tally.hit` 有值），后面的进程一个都不碰。
#[cfg(windows)]
fn scan_pids(
    pids: &[(u32, u64)],
    db: &DbInfo,
    filter: RegionFilter,
    brute: bool,
    tally: &mut Tally,
) {
    for (pid, _mem) in pids {
        if tally.hit.is_some() {
            return;
        }
        let Some(h) = win::open(*pid) else {
            continue;
        };
        tally.pid = Some(*pid);
        for (base, size, is_private) in win::enum_regions(h) {
            if !filter.keep(is_private) {
                continue;
            }
            match win::read_mem(h, base, size) {
                Some(data) => {
                    tally.scanned += data.len() as u64;
                    if let Some(hit) = scan_region(&data, base, db, &mut tally.candidates, brute) {
                        tally.hit = Some(hit);
                        break;
                    }
                }
                None => tally.read_fail += 1,
            }
        }
        win::close(h);
    }
}

/// 扫描客户端进程内存，返回验证过的密钥（hex 字符串）。
/// `client_key` 决定扫描哪个客户端的进程（避免拿错进程的内存验证本库的盐）。
pub fn scan_for_key(
    client_key: &str,
    db_path: &Path,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<ScanResult, String> {
    let db = load_database_info(db_path)
        .ok_or_else(|| format!("无法读取数据库首页：{}", db_path.display()))?;

    #[cfg(not(windows))]
    {
        let _ = (db, on_progress, client_key);
        return Err("进程内存扫描仅支持 Windows".into());
    }

    #[cfg(windows)]
    {
        let t0 = Instant::now();
        let (version, planned) = resolve_layout(client_key);
        let procs = collect_processes(client_key);
        if procs.is_empty() {
            return Err(format!(
                "未检测到运行中的客户端进程（{}）。请先启动并登录后再扫描。",
                client_key
            ));
        }
        let alt = match planned {
            KeyLayout::Legacy => KeyLayout::MultiProcess,
            KeyLayout::MultiProcess => KeyLayout::Legacy,
        };
        let planned_pids = match planned {
            KeyLayout::Legacy => legacy_candidates(&procs),
            KeyLayout::MultiProcess => multi_candidates(&procs),
        };
        let alt_pids = match alt {
            KeyLayout::Legacy => legacy_candidates(&procs),
            KeyLayout::MultiProcess => multi_candidates(&procs),
        };
        if let Some(cb) = on_progress {
            cb(&format!(
                "客户端版本 {} ⇒ 先按{}布局扫描（候选 {}/{} 个进程，共 {} 个进程）",
                version.as_deref().unwrap_or("未知"),
                if planned == KeyLayout::Legacy {
                    "旧版（ai_agent）"
                } else {
                    "多进程"
                },
                planned_pids.len(),
                alt_pids.len(),
                procs.len()
            ));
        }

        // 扫描计划：按「越省越先」排序，命中即停。
        //   ① 私有堆 + 库盐预筛   —— 真实形态，通常几百 MB 内结束
        //   ② 其余区域 + 库盐预筛 —— 私有堆没命中时的补扫
        //   ③ 全量校验（每个 hex 窗口都试）—— 只留给非 x'…' 形态的历史版本
        // 每条计划都先试版本指定的布局，再试另一条，保证阈值猜偏也能解出密钥。
        let mut tally = Tally::default();
        let plan: [(&[(u32, u64)], KeyLayout, RegionFilter, bool); 8] = [
            (&planned_pids, planned, RegionFilter::Private, false),
            (&alt_pids, alt, RegionFilter::Private, false),
            (&planned_pids, planned, RegionFilter::Rest, false),
            (&alt_pids, alt, RegionFilter::Rest, false),
            (&planned_pids, planned, RegionFilter::Private, true),
            (&alt_pids, alt, RegionFilter::Private, true),
            (&planned_pids, planned, RegionFilter::Rest, true),
            (&alt_pids, alt, RegionFilter::Rest, true),
        ];
        let mut used_layout = planned;
        for (pids, layout, filter, brute) in plan {
            if tally.hit.is_some() {
                break;
            }
            if pids.is_empty() {
                continue;
            }
            if let Some(cb) = on_progress {
                cb(&format!(
                    "扫描{} 布局 · {}{}（{} 个进程）…",
                    if layout == KeyLayout::Legacy { "旧版" } else { "多进程" },
                    filter.label(),
                    if brute { "全量校验" } else { "盐预筛" },
                    pids.len()
                ));
            }
            scan_pids(pids, &db, filter, brute, &mut tally);
            if tally.hit.is_some() {
                used_layout = layout;
            }
        }

        // 命中且与「版本推断」不一致 ⇒ 把真实结论缓存下来，下次直接走对的那条。
        if let (Some(v), true) = (&version, tally.hit.is_some()) {
            if used_layout != planned {
                write_cached_layout(client_key, v, used_layout);
            }
        }

        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let candidates = tally.candidates;
        let scanned_mb = tally.scanned / 1024 / 1024;
        match tally.hit {
            Some((key, addr)) => Ok(ScanResult {
                found: true,
                key: Some(key),
                address: Some(format!("0x{addr:016X}")),
                candidates,
                scanned_mb,
                elapsed_ms,
                pid: tally.pid,
                client_version: version,
                layout: used_layout,
                message: "已在进程内存中找到并验证密钥".to_string(),
            }),
            None => Ok(ScanResult {
                found: false,
                key: None,
                address: None,
                candidates,
                scanned_mb,
                elapsed_ms,
                pid: tally.pid,
                client_version: version,
                layout: used_layout,
                message: format!(
                    "未在进程内存中找到有效密钥（已扫 {} 个进程 / 读取失败 {} 区，                     客户端可能未运行或结构已变化）",
                    procs.len(),
                    tally.read_fail
                ),
            }),
        }
    }
}

/// 便捷：把验证过的密钥按客户端存盘，供解密/导出直接使用。
pub fn save_key(client_key: &str, key_hex: &str) -> Result<(), String> {
    let dir = crate::modules::config::store_dir().join("trae").join("keys");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{client_key}.json"));
    let payload = serde_json::json!({
        "client": client_key,
        "enc_key": key_hex,
        "saved_at": chrono::Local::now().to_rfc3339(),
    });
    std::fs::write(path, serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写密钥文件失败: {e}"))?;
    Ok(())
}

/// 读取已存盘的密钥（hex 字符串）。
pub fn load_saved_key(client_key: &str) -> Option<String> {
    let path = crate::modules::config::store_dir()
        .join("trae")
        .join("keys")
        .join(format!("{client_key}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("enc_key").and_then(|k| k.as_str()).map(String::from)
}

/// 临时诊断：读指定地址所在区域，搜索已知密钥字节并输出上下文。
#[cfg(windows)]
pub fn diag_read_at(client_key: &str, addr: u64, key_hex: &str) -> Vec<String> {
    let mut out = Vec::new();
    // 诊断用：按当前布局挑一个候选进程（旧版没命中就回落到多进程的第一个）。
    let (_version, planned) = resolve_layout(client_key);
    let procs = collect_processes(client_key);
    let mut pids = match planned {
        KeyLayout::Legacy => legacy_candidates(&procs),
        KeyLayout::MultiProcess => multi_candidates(&procs),
    };
    if pids.is_empty() {
        pids = multi_candidates(&procs);
    }
    let Some((pid, _)) = pids.first().copied() else {
        out.push("未找到客户端进程".into());
        return out;
    };
    let h = match win::open(pid) {
        Some(h) => h,
        None => {
            out.push("打开进程失败".into());
            return out;
        }
    };
    let regions = win::enum_regions(h);
    let mut hit_region = None;
    for (base, size, _is_private) in &regions {
        if *base <= addr && addr < *base + *size as u64 {
            hit_region = Some((*base, *size));
            break;
        }
    }
    let Some((base, size)) = hit_region else {
        out.push(format!("地址 0x{addr:X} 不在任何区域"));
        win::close(h);
        return out;
    };
    out.push(format!("区域 base=0x{base:X} size={size}"));
    match win::read_mem(h, base, size) {
        Some(data) => {
            let needle = key_hex.as_bytes();
            let off = (addr - base) as usize;
            let mut hit = None;
            if data.len() >= needle.len() {
                let mut i = 0usize;
                while i + needle.len() <= data.len() {
                    if &data[i..i + needle.len()] == needle {
                        hit = Some(i);
                        break;
                    }
                    i += 1;
                }
            }
            out.push(format!(
                "读取 OK {} 字节，目标偏移 {} 命中={:?}",
                data.len(),
                off,
                hit
            ));
            let ctx_s = off.saturating_sub(64);
            let ctx_e = (off + 96).min(data.len());
            out.push(format!(
                "上下文: {:?}",
                String::from_utf8_lossy(&data[ctx_s..ctx_e])
            ));
            // 顺便用滑动窗口验证：若命中，verify 结果？
            if let Some(hit_at) = hit {
                if let Some(info) = load_database_info(
                    std::path::Path::new(
                        r"C:\Users\11970\AppData\Roaming\TRAE SOLO CN\ModularData\ai-agent\database.db",
                    ),
                ) {
                    let ok = verify_enc_key(
                        &hex_to_bytes(&data[hit_at..hit_at + 64]),
                        &info.page1,
                    );
                    out.push(format!("verify(命中窗口) = {ok}"));
                }
            }
        }
        None => out.push("读取失败！".into()),
    }
    win::close(h);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧版路径必须与**旧实现选中的进程完全一致**：只取加载 ai_agent 的，组内按内存降序。
    #[test]
    fn legacy_only_scans_ai_agent_processes_sorted_by_memory() {
        let procs = vec![
            (100u32, 900_000u64, false), // 内存最大，但没加载 ai_agent —— 旧实现不会选它
            (200, 500_000, true),        // 旧版主进程
            (300, 700_000, true),
            (400, 100_000, false),
        ];
        let picked = legacy_candidates(&procs);
        assert_eq!(picked, vec![(300, 700_000), (200, 500_000)]);
        assert_eq!(picked[0].0, 300, "必须是旧实现会选中的那个进程");
    }

    /// 新版路径覆盖全部进程，同样按内存降序。
    #[test]
    fn multi_process_covers_every_process() {
        let procs = vec![
            (100u32, 900_000u64, false),
            (200, 500_000, true),
            (300, 700_000, false),
        ];
        let picked = multi_candidates(&procs);
        assert_eq!(picked, vec![(100, 900_000), (300, 700_000), (200, 500_000)]);
    }

    /// 版本号决定先走哪条路（只决定顺序，不决定正确性）。
    #[test]
    fn version_decides_layout() {
        assert_eq!(layout_for_version("3.4.9"), KeyLayout::MultiProcess);
        assert_eq!(layout_for_version("4.0.0"), KeyLayout::MultiProcess);
        assert_eq!(layout_for_version("3.4.8"), KeyLayout::Legacy);
        assert_eq!(layout_for_version("3.3.0"), KeyLayout::Legacy);
        assert_eq!(layout_for_version("garbage"), KeyLayout::Legacy);
    }

    #[test]
    fn version_parsing_is_tolerant() {
        assert_eq!(parse_version("3.4.9"), Some((3, 4, 9)));
        assert_eq!(parse_version("3.4.9-beta.1"), Some((3, 4, 9)));
        assert_eq!(parse_version("4.0"), Some((4, 0, 0)));
        assert_eq!(parse_version("  3.10.2  "), Some((3, 10, 2)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("x.y.z"), None);
    }

    /// 带 `/M` 的 CSV：3 列，模块列本身含逗号，必须整体当一格。
    #[test]
    fn parses_tasklist_module_rows() {
        let line = r#""Trae CN.exe","41972","ai_agent.dll, KERNEL32.DLL, USER32.dll""#;
        let (image, pid, modules) = parse_tasklist_line(line).expect("应能解析");
        assert_eq!(image, "Trae CN.exe");
        assert_eq!(pid, 41972);
        assert!(modules.to_lowercase().contains("ai_agent"));
        // 模块列被当成一格了，不能把 KERNEL32.DLL 当成第三列
        assert!(modules.contains("KERNEL32.DLL"));

        // 没有模块时只是两列，也不能崩
        assert!(parse_tasklist_line(r#""Trae CN.exe","123""#).is_some());
        assert!(parse_tasklist_line("INFO: No tasks are running").is_none());
    }

    /// 不带 `/M` 的 CSV：5 列，内存在**最后一列**（形如 "541,234 K"）。
    #[test]
    fn parses_tasklist_memory_rows() {
        let line = r#""Trae CN.exe","6816","Console","1","541,234 K""#;
        let (pid, mem) = parse_tasklist_mem_line(line).expect("应能解析");
        assert_eq!(pid, 6816);
        assert_eq!(mem, 541_234, "内存必须取最后一列并去掉千分位");

        // 会话名里带逗号也不能把内存列挤掉
        let odd = r#""Trae CN.exe","42","Console, x","1","1,024 K""#;
        let (pid, mem) = parse_tasklist_mem_line(odd).expect("应能解析");
        assert_eq!(pid, 42);
        assert_eq!(mem, 1_024);
    }

    /// 布局字符串要能往返（缓存文件里存的就是这两个字面量）。
    #[test]
    fn layout_string_roundtrip() {
        for l in [KeyLayout::Legacy, KeyLayout::MultiProcess] {
            assert_eq!(KeyLayout::parse(l.as_str()), Some(l));
        }
        assert_eq!(KeyLayout::parse("whatever"), None);
    }

    /// 分区过滤：私有堆与其余区域必须互补且不重叠。
    #[test]
    fn region_filter_is_exclusive() {
        for is_private in [true, false] {
            assert_ne!(
                RegionFilter::Private.keep(is_private),
                RegionFilter::Rest.keep(is_private)
            );
        }
    }
}
