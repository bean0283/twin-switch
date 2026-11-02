//! Qoder 官方客户端的 **UMID 组件**（`resources/umid/runtime-info`）—— 设备风险身份的现取入口。
//!
//! ## 为什么需要这个模块
//!
//! 「每天领 100 Credits」只下发给**带了全套 `cosy-machine*` 头**的请求。这套身份里
//! `machineToken` / `machineCode` / `machineType` 由客户端运行时生成，**不落盘**
//! （2026-10-31 把 `%APPDATA%\com.qoder.app.*` 与 `~/.qoder` 一万多个文件扫遍，无明文）。
//!
//! 之前的结论是「只能抓包，然后手工搬机器」。**那是错的** —— 值确实不落盘，但客户端自带的
//! `umid` 组件可以现场算出来：
//!
//! ```text
//! <安装目录>\resources\umid\runtime-info.exe --account-stdin
//! stdin: {"account":"<uid>"}\n   （可为空）
//! stdout: {"machineToken":"…","machineCode":"…","machineType":"…", …}
//! ```
//!
//! 客户端主进程自己也是这么调的（同类实现 magpie / qoder-checkin / Qoder-Daily 一致）。
//! 所以「换台电脑就签不了到」只是**我们没去调它**，不是服务端限制。
//!
//! ## 区域绑定
//!
//! 设备身份与区域绑定：国内版用 `com.qodercn` / `Qoder CN`，国际版用 `com.qoder` / `Qoder`。
//! 参考实现给组件传的**环境号**分别是 `0`（国内）与 `3`（国际），本模块照抄。
//! 两侧都装了时各自优先自己那个 —— 用错一侧会被风控拒绝。
//!
//! ## 失败就如实失败
//!
//! 组件缺失 / 跑不出来时返回具体原因，**绝不编造**。半套身份发出去比不发更糟
//! （服务端会当成畸形客户端，连社区活动都不下发）。

use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::modules::qoder_realm::QoderRegion;

/// 成功结果缓存时长。参考实现用 30~50 分钟（组件本身不便宜，且值短期内稳定）。
const TTL_OK: Duration = Duration::from_secs(30 * 60);
/// 失败结果缓存时长：短一点，用户刚装完客户端重试就能成功。
const TTL_FAIL: Duration = Duration::from_secs(60);
/// 组件执行超时。实测毫秒级，给足余量但不无限等。
const TIMEOUT: Duration = Duration::from_secs(25);
/// 单个字段上限（组件输出异常时别把整坨东西塞进请求头）。
const MAX_FIELD: usize = 4096;

/// 从组件取到的设备风险身份（三件套）。
#[derive(Debug, Clone)]
pub struct RiskIdentity {
    pub machine_token: String,
    pub machine_code: String,
    pub machine_type: String,
    /// 来源描述（`app` / `env:…` / `cli` / `data`），写进状态里便于排查。
    pub source: String,
}

struct Entry {
    at: Instant,
    result: Result<RiskIdentity, String>,
}

fn cache() -> &'static Mutex<HashMap<&'static str, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key_of(region: QoderRegion) -> &'static str {
    match region {
        QoderRegion::Cn => "cn",
        QoderRegion::Global => "global",
    }
}

/// 组件的区域环境号：国内 `0`、国际 `3`（与 Qoder-Daily 一致）。
fn region_env(region: QoderRegion) -> &'static str {
    match region {
        QoderRegion::Cn => "0",
        QoderRegion::Global => "3",
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "runtime-info.exe"
    } else {
        "runtime-info"
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .filter(|p| p.is_file())
}

/// 显式覆盖：先通用 `QODER_UMID_PATH`，再区域专属变量。
fn override_exe(region: QoderRegion) -> Option<(PathBuf, String)> {
    let region_var = match region {
        QoderRegion::Cn => "QODERCN_RUNTIME_INFO",
        QoderRegion::Global => "QODER_RUNTIME_INFO",
    };
    if let Some(p) = env_path(region_var) {
        return Some((p, format!("env:{region_var}")));
    }
    env_path("QODER_UMID_PATH").map(|p| (p, "env:QODER_UMID_PATH".into()))
}

/// 候选安装目录（**本区域优先**，再退到另一区域）。
fn install_roots(region: QoderRegion) -> Vec<(PathBuf, String)> {
    /// 收一个候选目录（去重；同路径只留第一次）。
    fn add(out: &mut Vec<(PathBuf, String)>, p: PathBuf) {
        if !out.iter().any(|(q, _)| q == &p) {
            out.push((p, "app".into()));
        }
    }
    let mut out: Vec<(PathBuf, String)> = Vec::new();

    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let pf = std::env::var_os("ProgramFiles").map(PathBuf::from);
        let pf86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
        // 本区域优先：国内版 `Qoder CN` / `QoderCN`，国际版 `Qoder`。
        let names: &[&str] = match region {
            QoderRegion::Cn => &["Qoder CN", "QoderCN", "Qoder"],
            QoderRegion::Global => &["Qoder", "Qoder CN"],
        };
        for name in names {
            if let Some(base) = &local {
                add(&mut out, base.join("Programs").join(name));
            }
            if let Some(base) = &pf {
                add(&mut out, base.join(name));
            }
            if let Some(base) = &pf86 {
                add(&mut out, base.join(name));
            }
        }
        // 兜底：`%LOCALAPPDATA%\Programs` 下任何名字带 qoder 的目录（安装名不规范时也能找到）。
        if let Some(base) = &local {
            if let Ok(rd) = std::fs::read_dir(base.join("Programs")) {
                for e in rd.flatten() {
                    let p = e.path();
                    let n = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                    if n.contains("qoder") {
                        add(&mut out, p);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let names: &[&str] = match region {
            QoderRegion::Cn => &["Qoder CN", "Qoder"],
            QoderRegion::Global => &["Qoder", "Qoder CN"],
        };
        for name in names {
            add(&mut out, PathBuf::from("/Applications").join(format!("{name}.app/Contents")));
        }
    }

    // CLI 缓存里也可能有一份（`~/.qoder/.bin/umid-*/runtime-info`）。
    let cli_home = match region {
        QoderRegion::Cn => ".qoder-cn",
        QoderRegion::Global => ".qoder",
    };
    if let Some(home) = dirs::home_dir() {
        if let Ok(rd) = std::fs::read_dir(home.join(cli_home).join(".bin")) {
            for e in rd.flatten() {
                let p = e.path().join(exe_name());
                if p.is_file() {
                    out.push((p, "cli".into()));
                }
            }
        }
    }

    // 注册表里的安装位置 —— **装在别的盘也能找到**（本机实测：`D:\Program Files\Qoder`，
    // 固定目录列表全落空）。顺序按区域挑一遍，同区优先。
    let mut reg = registry::install_dirs();
    let want_cn = matches!(region, QoderRegion::Cn);
    reg.sort_by_key(|(_, name)| {
        let n = name.to_lowercase();
        let is_cn = n.contains("qoder cn") || n.contains("qodercn");
        if is_cn == want_cn {
            0
        } else {
            1
        }
    });
    for (dir, _) in reg {
        add(&mut out, dir);
    }
    out
}

/// Windows 注册表里的安装目录（`Uninstall` 项的 `InstallLocation` / `DisplayIcon`）。
///
/// 为什么非要有这一步：Qoder 允许装到任意盘，本机就是 `D:\Program Files\Qoder`；
/// 只扫 `%LOCALAPPDATA%\Programs` 与 `C:\Program Files` 会全部落空，
/// 表现成「明明装了客户端却说没找到组件」。
#[cfg(windows)]
mod registry {
    use std::path::PathBuf;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ,
    };

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
            let mut buf = vec![0u8; size as usize + 2];
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
            let units: Vec<u16> = buf[..size as usize]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|c| *c != 0)
                .collect();
            String::from_utf16(&units).ok().map(|s| s.trim().to_string())
        }
    }

    /// 枚举卸载项，收集「名字里带 qoder」的安装目录。
    pub fn install_dirs() -> Vec<(PathBuf, String)> {
        let mut out: Vec<(PathBuf, String)> = Vec::new();
        for (root, path) in [
            (
                HKEY_CURRENT_USER,
                r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
            (
                HKEY_LOCAL_MACHINE,
                r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            ),
        ] {
            let root_w = wide(path);
            let mut hkey = HKEY::default();
            unsafe {
                if RegOpenKeyExW(root, PCWSTR(root_w.as_ptr()), 0, KEY_READ, &mut hkey)
                    != ERROR_SUCCESS
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
                    let display = query_sz(sub, "DisplayName").unwrap_or_default();
                    if display.to_lowercase().contains("qoder") {
                        let mut dir: Option<PathBuf> = None;
                        if let Some(v) = query_sz(sub, "InstallLocation") {
                            let d = PathBuf::from(v.trim().trim_matches('"'));
                            if d.is_dir() {
                                dir = Some(d);
                            }
                        }
                        // `DisplayIcon` 形如 `"D:\X\Qoder.exe",0`：取 exe 的父目录当安装目录。
                        if dir.is_none() {
                            if let Some(v) = query_sz(sub, "DisplayIcon") {
                                let raw = v
                                    .split(',')
                                    .next()
                                    .unwrap_or(&v)
                                    .trim()
                                    .trim_matches('"');
                                let p = PathBuf::from(raw);
                                if let Some(parent) = p.parent() {
                                    if parent.is_dir() {
                                        dir = Some(parent.to_path_buf());
                                    }
                                }
                            }
                        }
                        if let Some(d) = dir {
                            if !out.iter().any(|(q, _)| q == &d) {
                                out.push((d, display));
                            }
                        }
                    }
                    let _ = RegCloseKey(sub);
                }
                let _ = RegCloseKey(hkey);
            }
        }
        out
    }
}

#[cfg(not(windows))]
mod registry {
    use std::path::PathBuf;

    pub fn install_dirs() -> Vec<(PathBuf, String)> {
        Vec::new()
    }
}

/// 定位本区域的组件可执行文件。
pub fn locate(region: QoderRegion) -> Option<(PathBuf, String)> {
    if let Some(hit) = override_exe(region) {
        return Some(hit);
    }
    for (root, source) in install_roots(region) {
        // root 是安装目录（mac 下是 Contents），组件在 `<root>/resources/umid/<exe>`；
        // CLI 缓存那条给的是 exe 本身，直接判存在。
        let direct = root.join(exe_name());
        if direct.is_file() {
            return Some((direct, source));
        }
        let p = root.join("resources").join("umid").join(exe_name());
        if p.is_file() {
            return Some((p, source));
        }
    }
    None
}

/// 解析组件输出：取**最后一个**非空行（组件可能先打日志再打 JSON）。
fn parse_output(raw: &str) -> Result<RiskIdentity, String> {
    let line = raw
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or("组件没有输出")?;
    let v: Value = serde_json::from_str(line).map_err(|e| format!("组件输出不是 JSON：{e}"))?;
    let pick = |k: &str| -> Option<String> {
        let hit = v.get(k).or_else(|| v.pointer(&format!("/data/{k}")));
        hit.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= MAX_FIELD)
            .map(str::to_string)
    };
    let machine_token = pick("machineToken").ok_or("组件输出缺 machineToken")?;
    let machine_code = pick("machineCode").ok_or("组件输出缺 machineCode")?;
    let machine_type = pick("machineType").ok_or("组件输出缺 machineType")?;
    Ok(RiskIdentity {
        machine_token,
        machine_code,
        machine_type,
        source: String::new(),
    })
}

/// 跑一次组件（不缓存）。
///
/// 参数形态按参考实现试两遍：先带**区域号**（`runtime-info 3 --account-stdin`），
/// 不行再退回只带 `--account-stdin`。不同版本的组件对未知位置参数容忍度不一样，
/// 多试一次的成本只有几十毫秒，比"某台机器上就是不行"好。
fn run(exe: &PathBuf, source: &str, uid: &str, region: QoderRegion) -> Result<RiskIdentity, String> {
    let env = region_env(region);
    let mut id = match exec(exe, &[env, "--account-stdin"], uid) {
        Ok(v) => v,
        Err(first) => exec(exe, &["--account-stdin"], uid)
            .map_err(|second| format!("{first}；退回不带区域号再试也失败：{second}"))?,
    };
    id.source = source.to_string();
    Ok(id)
}

fn exec(exe: &PathBuf, args: &[&str], uid: &str) -> Result<RiskIdentity, String> {
    let payload = format!("{}\n", serde_json::json!({ "account": uid }));
    let mut child = Command::new(exe)
        .args(args)
        .current_dir(exe.parent().unwrap_or(exe))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动组件失败（{}）：{e}", exe.display()))?;

    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
        // 主动关闭 stdin：组件在等 EOF
    }

    // 另起一线程读 stdout，避免输出塞满管道导致互相等待。
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stdout.take() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

    // 没有 wait_timeout：自己轮询，超时就杀，别把签到线程挂死。
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if started.elapsed() > TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(format!("等待组件退出失败：{e}")),
        }
    };
    let bytes = reader.join().unwrap_or_default();
    if status.is_none() {
        return Err(format!("组件执行超时（{} 秒）", TIMEOUT.as_secs()));
    }
    let raw = String::from_utf8_lossy(&bytes);
    parse_output(&raw)
}

/// 取设备风险身份（带缓存）。失败也缓存 60 秒，避免每次签到都去 fork 一次。
pub fn risk_identity(region: QoderRegion) -> Option<RiskIdentity> {
    match probe_cached(region, "") {
        Ok(id) => Some(id),
        Err(_) => None,
    }
}

/// 同上，但把失败原因也返回给调用方（状态页/错误文案要用）。
pub fn probe_cached(region: QoderRegion, uid: &str) -> Result<RiskIdentity, String> {
    let key = key_of(region);
    {
        let guard = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = guard.get(key) {
            let ttl = if e.result.is_ok() { TTL_OK } else { TTL_FAIL };
            if e.at.elapsed() < ttl {
                return e.result.clone();
            }
        }
    }

    let result = (|| -> Result<RiskIdentity, String> {
        let (exe, source) = locate(region).ok_or_else(|| {
            "本机没找到 Qoder 客户端的 umid 组件（未安装客户端 / 安装目录不在常见位置）".to_string()
        })?;
        run(&exe, &source, uid, region)
    })();

    let mut guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    guard.insert(
        key,
        Entry {
            at: Instant::now(),
            result: result.clone(),
        },
    );
    result
}

/// 组件的诊断信息（状态页展示 / 自检用）。
pub fn diagnose(region: QoderRegion) -> Value {
    let located = locate(region);
    let cached = {
        let guard = cache().lock().unwrap_or_else(|e| e.into_inner());
        guard.get(key_of(region)).map(|e| {
            serde_json::json!({
                "ageSeconds": e.at.elapsed().as_secs(),
                "ok": e.result.is_ok(),
                "error": e.result.as_ref().err().cloned(),
            })
        })
    };
    serde_json::json!({
        "component": located.as_ref().map(|(p, _)| p.display().to_string()),
        "source": located.as_ref().map(|(_, s)| s.clone()),
        "runtimeEnv": region_env(region),
        "clientVersion": client_version(region),
        "cached": cached,
    })
}

/// 客户端版本号（`resources/build-manifest.json` 的 `productVersion`）。
///
/// 请求头 `cosy-version` **应该**是客户端版本，不是本工具版本 —— 之前写死成
/// 我们自己的 `0.4.8`，等于告诉服务端"我是 0.4.8 版客户端"，纯属自曝。
pub fn client_version(region: QoderRegion) -> Option<String> {
    let (exe, _) = locate(region)?;
    let resources = exe.parent()?.parent()?; // .../resources/umid/runtime-info.exe → .../resources
    let text = std::fs::read_to_string(resources.join("build-manifest.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("productVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 清缓存（导入新身份、切区域后强制重取）。
pub fn invalidate() {
    cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_output() {
        let raw = "{\"machineToken\":\"tok\",\"machineCode\":\"code\",\"machineType\":\"type\"}\n";
        let id = parse_output(raw).expect("应当解析成功");
        assert_eq!(id.machine_token, "tok");
        assert_eq!(id.machine_code, "code");
        assert_eq!(id.machine_type, "type");
    }

    #[test]
    fn takes_last_json_line_and_ignores_noise() {
        // 组件偶尔先打一行日志再打 JSON：必须取最后一行。
        let raw = "[info] warming up\n{\"machineToken\":\"t\",\"machineCode\":\"c\",\"machineType\":\"y\"}\n\n";
        let id = parse_output(raw).expect("噪声不该影响解析");
        assert_eq!(id.machine_token, "t");
    }

    #[test]
    fn accepts_enveloped_output() {
        let raw = "{\"code\":0,\"data\":{\"machineToken\":\"t\",\"machineCode\":\"c\",\"machineType\":\"y\"}}";
        let id = parse_output(raw).expect("信封里也要认");
        assert_eq!(id.machine_type, "y");
    }

    #[test]
    fn rejects_incomplete_output() {
        // 缺任何一个都必须报错 —— 半套身份比不发更糟。
        let raw = "{\"machineToken\":\"t\",\"machineCode\":\"c\"}";
        let err = parse_output(raw).unwrap_err();
        assert!(err.contains("machineType"), "{err}");
        assert!(parse_output("").is_err());
        assert!(parse_output("not json").is_err());
    }

    #[test]
    fn region_env_matches_reference() {
        assert_eq!(region_env(QoderRegion::Cn), "0");
        assert_eq!(region_env(QoderRegion::Global), "3");
    }
}
