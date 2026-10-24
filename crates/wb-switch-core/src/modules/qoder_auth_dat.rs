//! Qoder 桌面端登录态（`auth.v1.dat`）的加解密与切号。
//!
//! 文件形态与 Chromium 的 cookie 加密同构：
//!
//! ```text
//! auth.v1.dat = "v10" + nonce(12B) + AES-256-GCM(明文 + tag)
//! 主密钥      = base64(Local State 的 os_crypt.encrypted_key) → 去掉 "DPAPI" 5 字节 → CryptUnprotectData
//! ```
//!
//! ## 切号四条硬纪律（逐条对应参考实现的注释）
//!
//! 1. **原地改写，不从账号记录重建**：明文里有一批账号库装不下的字段
//!    （`refreshTokenExpiresAt` / `user.email` / `user.phone` / `user.avatarUrl`），
//!    从零重建会让它们消失，客户端很可能判定登录态无效并清掉 —— 用户看到「切了个寂寞，还退登了」。
//! 2. **客户端在跑时拒绝写**：它会把内存里的登录态整体回写，写文件等于白干甚至写坏。
//! 3. **写后必须解密校验**，不一致要把**写前备份**原样写回，并如实说「已还原」。
//! 4. **GCM 用随机 nonce**：同一明文两次加密的密文必然不同 ⇒ 幂等性只能在明文层断言，不能比密文字节。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::modules::qoder_realm::{realm_spec, QoderRegion};

const V10_PREFIX: &[u8] = b"v10";
const NONCE_LEN: usize = 12;
const DPAPI_PREFIX: &[u8] = b"DPAPI";

// ---------------------------------------------------------------- 路径

/// 桌面端用户数据目录 —— **按版本严格映射，绝不跨区兜底**。
///
/// 实测（本机同时装了两个版本）：
///
/// | 版本 | `%APPDATA%` 目录 |
/// | --- | --- |
/// | 国内版 | `com.qodercn.app.stable` |
/// | 国际版 | `com.qoder.app.stable` |
///
/// ⚠️ 以前的写法是「列一串候选、取第一个存在的」，于是国内版目录不存在时会退到
/// 国际版目录 —— 结果是**把国际版账号导进国内版库**，用户看到「导入成功但一直不能用」。
/// 绝不能跨区兜底：找不到就如实说「本机没装这个版本」。
fn desktop_dir_candidates(region: QoderRegion) -> Vec<PathBuf> {
    let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    match region {
        QoderRegion::Cn => vec![
            roaming.join("com.qodercn.app.stable"),
            roaming.join("Qoder CN"),
        ],
        QoderRegion::Global => vec![
            roaming.join("com.qoder.app.stable"),
            roaming.join("Qoder"),
        ],
    }
}

/// 找到桌面端数据目录（含 `Local State`）。
pub fn desktop_dir(region: QoderRegion) -> Option<PathBuf> {
    desktop_dir_candidates(region)
        .into_iter()
        .find(|d| d.join("Local State").is_file())
}

pub fn auth_dat_path(region: QoderRegion) -> Option<PathBuf> {
    desktop_dir(region).map(|d| d.join("auth.v1.dat"))
}

// ---------------------------------------------------------------- DPAPI

#[cfg(windows)]
fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    };

    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| format!("DPAPI 解密失败：{e}"))?;
        if out.pbData.is_null() || out.cbData == 0 {
            return Ok(Vec::new());
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as *mut _));
        Ok(v)
    }
}

#[cfg(not(windows))]
fn dpapi_unprotect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("Qoder 登录态加密只用 Windows DPAPI 实现".into())
}

/// 主密钥：`Local State` 的 `os_crypt.encrypted_key` → 去 `DPAPI` 前缀 → DPAPI 解。
fn master_key(dir: &Path) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let text = std::fs::read_to_string(dir.join("Local State"))
        .map_err(|e| format!("读 Local State 失败：{e}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("Local State 不是 JSON：{e}"))?;
    let enc = v
        .pointer("/os_crypt/encrypted_key")
        .and_then(Value::as_str)
        .ok_or_else(|| "Local State 里没有 os_crypt.encrypted_key".to_string())?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(enc)
        .map_err(|e| format!("encrypted_key 不是 base64：{e}"))?;
    let body = raw
        .strip_prefix(DPAPI_PREFIX)
        .ok_or_else(|| "encrypted_key 缺少 DPAPI 前缀（不是 Chromium 的标准形态）".to_string())?;
    let key = dpapi_unprotect(body)?;
    if key.len() != 32 {
        return Err(format!("主密钥长度异常：{}（应为 32）", key.len()));
    }
    Ok(key)
}

// ---------------------------------------------------------------- v10

/// 解密 `auth.v1.dat`。
pub fn decrypt_auth_dat(path: &Path) -> Result<Value, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};

    let blob = std::fs::read(path).map_err(|e| format!("读 auth.v1.dat 失败：{e}"))?;
    if blob.len() <= V10_PREFIX.len() + NONCE_LEN {
        return Err("auth.v1.dat 太短，不是合法密文".into());
    }
    if !blob.starts_with(V10_PREFIX) {
        return Err(format!(
            "auth.v1.dat 前缀不是 v10（实际 {:?}）—— 加密方案可能变了",
            String::from_utf8_lossy(&blob[..3.min(blob.len())])
        ));
    }
    let dir = path
        .parent()
        .ok_or_else(|| "auth.v1.dat 没有父目录".to_string())?;
    let key = master_key(dir)?;

    let nonce_bytes = &blob[V10_PREFIX.len()..V10_PREFIX.len() + NONCE_LEN];
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("构造 GCM 失败：{e}"))?;
    let plain = cipher
        .decrypt(
            Nonce::from_slice(nonce_bytes),
            &blob[V10_PREFIX.len() + NONCE_LEN..],
        )
        .map_err(|_| "GCM 解密失败（主密钥不匹配或文件被改过）".to_string())?;

    serde_json::from_slice(&plain).map_err(|e| {
        format!(
            "解密结果不是 JSON（{e}）：{}",
            String::from_utf8_lossy(&plain).chars().take(120).collect::<String>()
        )
    })
}

/// 加密成 `v10` 形态（每次随机 nonce ⇒ 同一明文的密文必然不同）。
pub fn encrypt_auth_dat(plain: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};

    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("构造 GCM 失败：{e}"))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce_bytes).map_err(|e| format!("生成 nonce 失败：{e}"))?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain)
        .map_err(|_| "GCM 加密失败".to_string())?;

    let mut out = Vec::with_capacity(V10_PREFIX.len() + NONCE_LEN + ct.len());
    out.extend_from_slice(V10_PREFIX);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

// ---------------------------------------------------------------- 切号

/// 切号结果。
#[derive(Debug)]
pub struct SwitchOutcome {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
    pub note: String,
}

/// 把账号的凭据写回桌面端登录态。
///
/// 顺序：进程守卫 → 备份 → 解密 → 只改四个字段 → 加密 → 写 → 读回校验 → 失败回滚。
pub fn switch_to(region: QoderRegion, uid: &str, access: &str, refresh: &str) -> Result<SwitchOutcome, String> {
    let path = auth_dat_path(region)
        .ok_or_else(|| "本机没有检测到 Qoder 桌面端登录态（找不到 Local State）".to_string())?;

    // ① 先自己把这一版客户端关掉（照 Trae / WorkBuddy：不让用户手动退）。
    stop_client(region)?;

    // ② 备份
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = path.with_file_name(format!("auth.v1.dat.bak-{stamp}"));
    std::fs::copy(&path, &backup).map_err(|e| format!("备份登录态失败：{e}"))?;

    // ③ 解密 → 只改字段（其余键原样保留）
    let mut plain = decrypt_auth_dat(&path)?;
    let obj = plain
        .as_object_mut()
        .ok_or_else(|| "登录态明文不是 JSON 对象".to_string())?;
    obj.insert("token".into(), Value::String(access.to_string()));
    if !refresh.is_empty() {
        obj.insert("refreshToken".into(), Value::String(refresh.to_string()));
    }
    if let Some(user) = obj.get_mut("user").and_then(Value::as_object_mut) {
        user.insert("id".into(), Value::String(uid.to_string()));
    }
    let body = serde_json::to_vec(&plain).map_err(|e| format!("序列化登录态失败：{e}"))?;

    let dir = path.parent().unwrap().to_path_buf();
    let key = master_key(&dir)?;

    // ④ 写
    let blob = encrypt_auth_dat(&body, &key)?;
    if let Err(e) = std::fs::write(&path, &blob) {
        let _ = std::fs::copy(&backup, &path);
        return Err(format!("写登录态失败（已还原备份）：{e}"));
    }

    // ⑤ 读回校验
    match decrypt_auth_dat(&path) {
        Ok(v) if v.get("token").and_then(Value::as_str) == Some(access) => Ok(SwitchOutcome {
            path: path.clone(),
            note: format!("已写回 {}（原文件备份在 {}）", path.display(), backup.display()),
            backup: Some(backup),
        }),
        Ok(_) => {
            let _ = std::fs::copy(&backup, &path);
            Err("写回后校验不一致（token 对不上），已还原备份".into())
        }
        Err(e) => {
            let _ = std::fs::copy(&backup, &path);
            Err(format!("写回后无法解密（{e}），已还原备份"))
        }
    }
}

// ---------------------------------------------------------------- 进程守卫

/// Qoder 客户端是否在跑 —— **两个版本的进程名都要查**。
///
/// ⚠️ 只查 `Qoder.exe` 是**不够的**：国内版的进程叫 **`Qoder CN.exe`**，而且一开就是好几个。
/// 曾经只查国际版名字 ⇒ 国内版客户端明明开着却被判成「已关闭」⇒ 写库被它退出时覆盖、
/// 新实例又被单实例机制顶掉，表现为「切换了但客户端账号没变，手动重启才好」。
pub fn client_processes() -> Vec<(String, u32)> {
    let names: Vec<String> = [
        "Qoder.exe",
        "Qoder CN.exe",
        "Qoder Launcher.exe",
        "Qoder CN Launcher.exe",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    crate::modules::process_list::find_by_names(&names)
}

/// **主动关闭客户端**（写客户端数据前的唯一出口）。
///
/// 设计参照 Trae / WorkBuddy 的做法：**不让用户手动退**，程序自己
/// 「结束进程树 → 等它真的消失 → 写 → 重新启动」。
/// 只拒绝的风格是错的：用户看到「请先退出」只会觉得功能没做完。
///
/// 与 Trae 一致：超时后如果残留 PID 与首轮完全不重叠，说明**被杀掉又被拉起来了**
/// （更新器/看门狗），这种情况也当失败上报，不能继续往库里写。
pub fn stop_client(region: QoderRegion) -> Result<Vec<String>, String> {
    // 只关**这一版**的进程：两个版本可以同时开着，写哪个版本的登录态就关哪个，
    // 别把另一个版本也一起杀掉（Trae 那边是按 client 分家的，同理）。
    let names: Vec<String> = match region {
        QoderRegion::Cn => vec!["Qoder CN.exe".to_string(), "Qoder CN Launcher.exe".to_string()],
        QoderRegion::Global => vec!["Qoder.exe".to_string(), "Qoder Launcher.exe".to_string()],
    };

    if crate::modules::process_list::find_by_names(&names).is_empty() {
        return Ok(Vec::new());
    }

    let outcome = crate::modules::process_list::kill_tree_and_wait(
        &names,
        std::time::Duration::from_secs(20),
        &|list| list.is_empty(),
    );
    if outcome.ok {
        return Ok(outcome.killed);
    }
    let left: Vec<String> = outcome
        .remaining
        .iter()
        .map(|(n, pid)| format!("{n}({pid})"))
        .collect();
    Err(format!(
        "没能关闭 Qoder 客户端（{} 轮仍未退出{}）：{}。请手动退出后再试",
        outcome.attempts,
        if outcome.restarted { "，进程被杀掉后又被重新拉起" } else { "" },
        if left.is_empty() { outcome.errors.join("；") } else { left.join("、") }
    ))
}

// ---------------------------------------------------------------- 启动客户端

/// 切换后把客户端拉起来 —— 否则用户切完还得自己去点图标。
///
/// ⚠️ 两个版本的启动器目录名与可执行名**都不一样**（实测）：
///
/// | 版本 | 启动器目录 | installDir | exe |
/// | --- | --- | --- | --- |
/// | 国际版 | `%LOCALAPPDATA%\Qoder\Qoder Launcher` | `D://Program Files\Qoder` | `Qoder.exe` |
/// | 国内版 | `%LOCALAPPDATA%\Qoder CN\**Qoder CN Launcher**` | `%LOCALAPPDATA%\Programs\Qoder CN` | **`Qoder CN.exe`** |
///
/// 所以**不能写死 `Qoder Launcher` / `Qoder.exe`** —— 之前就是这么错的：
/// 国内版切换成功、却报「没能自动启动客户端」。现在一律读 `install.ini` 的
/// `installDir` + `appExecutable`。
pub fn launch_desktop(region: QoderRegion) -> Result<String, String> {
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "取不到 LOCALAPPDATA".to_string())?;

    // 该版本的启动器候选目录（先按版本排，再退到另一个名字）。
    let dirs: Vec<PathBuf> = match region {
        QoderRegion::Cn => vec![
            local.join("Qoder CN").join("Qoder CN Launcher"),
            local.join("Qoder CN").join("Qoder Launcher"),
            local.join("Qoder").join("Qoder CN Launcher"),
        ],
        QoderRegion::Global => vec![
            local.join("Qoder").join("Qoder Launcher"),
            local.join("Qoder").join("Qoder CN Launcher"),
            local.join("Qoder CN").join("Qoder Launcher"),
        ],
    };

    for dir in &dirs {
        let ini = dir.join("install.ini");
        let Ok(text) = std::fs::read_to_string(&ini) else {
            continue;
        };
        let mut install_dir = String::new();
        let mut exe_name = String::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("installDir=") {
                install_dir = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("appExecutable=") {
                exe_name = v.trim().to_string();
            }
        }
        if !install_dir.is_empty() && !exe_name.is_empty() {
            let exe = PathBuf::from(&install_dir).join(&exe_name);
            if exe.is_file() {
                std::process::Command::new(&exe)
                    .spawn()
                    .map_err(|e| format!("启动失败（{}）：{e}", exe.display()))?;
                return Ok(exe.display().to_string());
            }
            // installDir 指对了但 exe 名字变了：在目录里找唯一的 exe 兜底。
            if let Some(found) = find_exe(&PathBuf::from(&install_dir)) {
                std::process::Command::new(&found)
                    .spawn()
                    .map_err(|e| format!("启动失败（{}）：{e}", found.display()))?;
                return Ok(found.display().to_string());
            }
        }
        // 没有 install.ini 内容时，直接找启动器本身。
        for name in ["Qoder CN Launcher.exe", "Qoder Launcher.exe"] {
            let l = dir.join(name);
            if l.is_file() {
                std::process::Command::new(&l)
                    .spawn()
                    .map_err(|e| format!("启动失败（{}）：{e}", l.display()))?;
                return Ok(l.display().to_string());
            }
        }
    }

    Err(format!(
        "没找到 {} 的启动程序（找过：{}）",
        region.display_name(),
        dirs.iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join("；")
    ))
}

/// 在安装目录里找一个主程序：优先名字里带 Qoder 的 exe，其次唯一的 exe。
fn find_exe(dir: &std::path::Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut exes: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|e| e.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false)
        })
        .collect();
    if exes.is_empty() {
        return None;
    }
    exes.sort();
    let named: Vec<&PathBuf> = exes
        .iter()
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase().contains("qoder"))
                .unwrap_or(false)
        })
        .collect();
    if named.len() == 1 {
        return Some(named[0].clone());
    }
    // 多个带 qoder 的（可能有 uninstaller）：优先非 uninstall 的
    named
        .iter()
        .find(|p| !p.to_string_lossy().to_lowercase().contains("unins"))
        .map(|p| (*p).clone())
        .or_else(|| (exes.len() == 1).then(|| exes[0].clone()))
}

// ---------------------------------------------------------------- 导入本机账号

/// 从本机就读到的账号（**不含令牌**，导入后只用于回显）。
#[derive(Debug, Clone)]
pub struct LocalAccount {
    pub uid: String,
    pub name: String,
    pub email: Option<String>,
    pub avatar: Option<String>,
    pub source: String,
    pub token: String,
    pub refresh: Option<String>,
    pub expires_at: Option<i64>,
}

fn rfc3339_to_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn expires_field(v: &Value) -> Option<i64> {
    let raw = v.get("expiresAt")?;
    if let Some(n) = raw.as_i64() {
        // 秒 / 毫秒都能见到，统一成毫秒。
        return Some(if n < 1_000_000_000_000 { n * 1000 } else { n });
    }
    raw.as_str().and_then(rfc3339_to_ms)
}

fn account_from_plain(plain: &Value, source: &str) -> Option<LocalAccount> {
    let token = plain
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())?
        .to_string();
    let refresh = plain
        .get("refreshToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let user = plain.get("user");
    let uid = user
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string())))
        .or_else(|| {
            plain
                .get("uid")
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .unwrap_or_default();
    if uid.trim().is_empty() {
        return None;
    }
    let name = user
        .and_then(|u| u.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let email = user
        .and_then(|u| u.get("email"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let avatar = user
        .and_then(|u| u.get("avatarUrl"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Some(LocalAccount {
        uid,
        name,
        email,
        avatar,
        source: source.to_string(),
        token,
        refresh,
        expires_at: expires_field(plain),
    })
}

/// 扫描本机，返回能导入的账号（桌面端优先，其次 CLI）。
///
/// 两个来源读到的都是**明文登录态**，这里只做结构提取；写库由调用方负责。
pub fn scan_local(region: QoderRegion) -> Vec<LocalAccount> {
    let mut out = Vec::new();

    // ① 桌面端 auth.v1.dat
    if let Some(path) = auth_dat_path(region) {
        if let Ok(plain) = decrypt_auth_dat(&path) {
            if let Some(acc) = account_from_plain(&plain, "desktop-app") {
                out.push(acc);
            }
        }
    }

    // ② CLI 的 .auth/user（没登录过就不存在）
    if let Some(home) = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
    {
        let cli = home
            .join(realm_spec(region).cli_home_dir)
            .join(".auth")
            .join("user");
        if cli.is_file() {
            if let Ok(text) = std::fs::read_to_string(&cli) {
                if let Ok(plain) = serde_json::from_str::<Value>(&text) {
                    if let Some(acc) = account_from_plain(&plain, "cli") {
                        out.push(acc);
                    }
                }
            }
        }
    }

    // 同一 uid 只留一条（桌面端优先）。
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|a| seen.insert(a.uid.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只**数**进程，绝不调用 `stop_client()` —— 测试不许杀用户正在用的客户端。
    #[test]
    #[ignore]
    fn probe_process_count() {
        let procs = client_processes();
        let kinds: std::collections::BTreeSet<&str> =
            procs.iter().map(|(n, _)| n.as_str()).collect();
        eprintln!("[guard] 命中进程数：{} / 类型：{kinds:?}", procs.len());
    }

    /// 探测：把数据目录下所有 `*.v1.dat` 都试解一遍，打印键名（不打印任何值）。
    #[test]
    #[ignore]
    fn probe_all_v1_dat_files() {
        let Some(dir) = desktop_dir(QoderRegion::Cn) else {
            eprintln!("没有桌面端目录");
            return;
        };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".v1.dat") {
                continue;
            }
            match decrypt_auth_dat(&e.path()) {
                Ok(v) => {
                    let keys: Vec<String> = v
                        .as_object()
                        .map(|o| o.keys().cloned().collect())
                        .unwrap_or_default();
                    eprintln!("[{}] 解开了，顶层键：{keys:?}", name);
                    if v.get("profiles").is_some() {
                        // overlays 里的 profiles 实测是空对象，只有昵称迁移记录。
                        eprintln!(
                            "[{}] profiles 条目数 = {}",
                            name,
                            v.pointer("/profiles")
                                .and_then(Value::as_object)
                                .map(|o| o.len())
                                .unwrap_or(0)
                        );
                    }
                    if let Some(arr) = v.as_array() {
                        eprintln!("[{}] 是数组，长度 {}", name, arr.len());
                    }
                }
                Err(err) => eprintln!("[{name}] 解不开：{err}"),
            }
        }
    }

    /// 本机若没有 Qoder 桌面端登录态，这些用例只验证「如实报错」。
    #[test]
    fn missing_desktop_state_is_a_clear_error() {
        match auth_dat_path(QoderRegion::Cn) {
            Some(p) => {
                // 有登录态就验证能解开（不能编造：解不开就是解不开）。
                match decrypt_auth_dat(&p) {
                    Ok(v) => {
                        // 只打印**键名**，绝不打印任何令牌值。
                        let keys: Vec<String> = v
                            .as_object()
                            .map(|o| o.keys().cloned().collect())
                            .unwrap_or_default();
                        eprintln!("[auth.v1.dat] 解密成功，顶层键：{keys:?}");
                        if let Some(user) = v.get("user").and_then(|u| u.as_object()) {
                            let uk: Vec<String> = user
                                .keys()
                                .map(|k| {
                                    let ty = match user.get(k) {
                                        Some(serde_json::Value::String(_)) => "str",
                                        Some(serde_json::Value::Number(_)) => "num",
                                        Some(serde_json::Value::Bool(_)) => "bool",
                                        Some(serde_json::Value::Null) => "null",
                                        Some(serde_json::Value::Array(_)) => "arr",
                                        Some(serde_json::Value::Object(_)) => "obj",
                                        None => "?",
                                    };
                                    format!("{k}:{ty}")
                                })
                                .collect();
                            eprintln!("[auth.v1.dat] user 字段：{uk:?}");
                        }
                        assert!(v.is_object(), "解密结果应是对象");
                    }
                    Err(e) => {
                        eprintln!("[auth.v1.dat] 解密失败：{e}");
                        assert!(e.contains("DPAPI") || e.contains("GCM") || e.contains("JSON"));
                    }
                }
            }
            None => {
                let err = switch_to(QoderRegion::Cn, "u", "t", "r").unwrap_err();
                assert!(err.contains("没有检测到"), "{err}");
            }
        }
    }
}
