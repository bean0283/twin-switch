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

// ---------------------------------------------------------------- 明文回显 / 当前登录

/// 客户端自己写下的**明文**回显：`~/.qoder{,-cn}/.qoder-app-status.json`。
///
/// 客户端的 `writer: "main"` 每次切换/登录都会重写这份小文件，内容是
/// `{logged_in, name, avatar_url, version, snapshot_at}` —— **不加密**。
/// 用途有两个，都不能靠猜：
///
/// 1. 账号名兜底：`auth.v1.dat` 解不开（换机器、DPAPI 变了）时至少还能显示名字；
/// 2. 「当前登录」旁证：与 [`current_desktop_uid`] 的结论应当一致，不一致说明
///    客户端刚被切换、回显还没刷新 —— 界面别把它当成权威。
pub fn app_status(region: QoderRegion) -> Option<Value> {
    let home = dirs::home_dir()?;
    let path = home
        .join(realm_spec(region).cli_home_dir)
        .join(".qoder-app-status.json");
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    // 统一成 camelCase 给前端，省得前端去记客户端那套 snake_case。
    Some(serde_json::json!({
        "loggedIn": v.get("logged_in").and_then(Value::as_bool).unwrap_or(false),
        "name": v.get("name").cloned().unwrap_or(Value::Null),
        "avatarUrl": v.get("avatar_url").cloned().unwrap_or(Value::Null),
        "version": v.get("version").cloned().unwrap_or(Value::Null),
        "snapshotAt": v.get("snapshot_at").cloned().unwrap_or(Value::Null),
    }))
}

/// 桌面端**当前登录**的 uid：解 `auth.v1.dat` 明文里的 `user.id`。
///
/// ⚠️ 这是「当前登录」的唯一权威判据（照 Trae / WorkBuddy 的规矩）。解不开就返回
/// `None` —— 界面据此**不亮任何徽标**，而不是拿回显里的名字去猜（同名不同号会骗人）。
pub fn current_desktop_uid(region: QoderRegion) -> Option<String> {
    let path = auth_dat_path(region)?;
    let plain = decrypt_auth_dat(&path).ok()?;
    plain
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- 托管判定

/// 本进程相对 Qoder 客户端的位置 —— **杀进程前必须先问的问题**。
///
/// 为什么非要有这道门：TwinSwitch 可能是从 **Qoder 的内嵌终端**、或 Qoder 拉起的脚本里
/// 启动的。那种情况下「结束 Qoder 进程树」会把本工具自己也杀掉，用户看到的是
/// **程序闪退 + 客户端没了**，而且没有任何错误信息（进程都没了，谁来报错）。
///
/// 判定不出来时一律按危险处理（fail-closed）：「没看到目标」不等于「没被托管」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// 不在目标客户端的进程树里，可以安全终止。
    Outside,
    /// 就在目标客户端的进程树里（或带着它的环境标记）。
    Hosted { evidence: String },
    /// 判不出来：父链断在某处 / 进程快照读不到。按「不许」处理。
    Unknown { reason: String },
}

/// 本区域目标客户端的映像名（比较时**不区分大小写**）。
fn target_image_names(region: QoderRegion) -> Vec<&'static str> {
    match region {
        QoderRegion::Cn => vec!["Qoder CN.exe", "Qoder CN Launcher.exe"],
        QoderRegion::Global => vec!["Qoder.exe", "Qoder Launcher.exe"],
    }
}

/// 读「客户端注入的环境标记」。抽成纯函数只为一件事：可单测（真去读进程环境会污染并行测试）。
///
/// 这些键来自对 Qoder 客户端注入子进程的实测：带着它们基本就说明本进程是客户端拉起来的。
fn env_markers_from(get: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out = Vec::new();
    for key in [
        "QODER_PRODUCT_ID",
        "QODERCN_CLI",
        "QODER_SESSION_TYPE",
        "QODERCN_SESSION_TYPE",
    ] {
        if let Some(v) = get(key) {
            let v = v.trim();
            if !v.is_empty() {
                out.push(format!("{key}={v}"));
            }
        }
    }
    out
}

/// `me` 是不是 `root` 的子孙（沿 parent→child 关系往下走，宽度优先）。
///
/// ⚠️ 为什么用**反向**遍历而不是从自己往上爬：Windows 上父进程经常先退出
/// （`explorer.exe` 的父是早已结束的 `userinit.exe`），往上爬一两层就断链，
/// 而断链在「从桌面图标正常启动」时是**常态** —— 拿它当「判不出来」会把所有正常用户拦住。
/// 往下走只依赖**当前还活着的** Qoder 进程，托管场景（内嵌终端 / 脚本）里中间那层 shell
/// 通常也还活着，所以这条路既准又不会误伤。
pub(crate) fn is_descendant(
    children: &std::collections::HashMap<u32, Vec<u32>>,
    root: u32,
    me: u32,
) -> bool {
    let mut queue = std::collections::VecDeque::from([root]);
    let mut seen = std::collections::HashSet::from([root]);
    while let Some(pid) = queue.pop_front() {
        if pid == me {
            return true;
        }
        if let Some(kids) = children.get(&pid) {
            for &k in kids {
                if seen.insert(k) {
                    queue.push_back(k);
                }
            }
        }
    }
    false
}

/// 现场判定：环境标记 → **反向子孙遍历** → 正向链兜底。
///
/// ⚠️ 全程只用 `process_list::all_processes_ex()` 的**一次** Toolhelp 快照（带 ppid），
/// 不起 `PowerShell` / `wmic` 子进程 —— 那两条路都有坑：多行脚本经 `-Command` 传进去时
/// 内嵌引号会被 CreateProcess 的参数拼接破坏、**静默返回空值**，安全门就形同不存在。
pub fn placement(region: QoderRegion) -> Placement {
    let env_markers = env_markers_from(&|k| std::env::var(k).ok());
    if !env_markers.is_empty() {
        return Placement::Hosted {
            evidence: format!("环境变量 {}", env_markers.join(" / ")),
        };
    }

    let procs = crate::modules::process_list::all_processes_ex();
    if procs.is_empty() {
        return Placement::Unknown {
            reason: "读不到本机进程表".to_string(),
        };
    }
    let targets = target_image_names(region);
    let me = std::process::id();

    // 反向：从每个目标进程出发，看子树里有没有自己。
    let mut children: std::collections::HashMap<u32, Vec<u32>> =
        std::collections::HashMap::new();
    for p in &procs {
        children.entry(p.parent).or_default().push(p.pid);
    }
    for p in &procs {
        if !targets.iter().any(|t| p.name.eq_ignore_ascii_case(t)) {
            continue;
        }
        if is_descendant(&children, p.pid, me) {
            return Placement::Hosted {
                evidence: format!("本进程是 {} 的子进程（pid {}）", p.name, p.pid),
            };
        }
    }

    // 正向：从自己向上爬，能命中目标也算（覆盖「反向被已退出的中间进程挡住」的极端情况）。
    let by_pid: std::collections::HashMap<u32, (&str, u32)> =
        procs.iter().map(|p| (p.pid, (p.name.as_str(), p.parent))).collect();
    let mut cur = me;
    for _ in 0..32 {
        let Some((_, parent)) = by_pid.get(&cur) else { break };
        let parent = *parent;
        if parent == 0 || parent == cur {
            break;
        }
        let Some((pname, _)) = by_pid.get(&parent) else { break };
        if targets.iter().any(|t| pname.eq_ignore_ascii_case(t)) {
            return Placement::Hosted {
                evidence: format!("父进程链上有 {pname}"),
            };
        }
        cur = parent;
    }

    // 两条路都没命中 ⇒ 不在客户端树里。**不把「正向链断了」当 Unknown**：
    // 那是 Windows 上的常态，拿它拦住用户等于功能不可用。
    Placement::Outside
}

/// 把判定结果翻成用户能照做的错误；[`Placement::Outside`] 返回 `None`（放行）。
fn placement_blocker(region: QoderRegion, p: &Placement) -> Option<String> {
    match p {
        Placement::Outside => None,
        Placement::Hosted { evidence } => Some(format!(
            "已拒绝：本工具正运行在 {} 的进程树里（{evidence}）—— 现在结束客户端会连本工具一起死。\n\
             ⇒ ① 先手动退出 {} 再点一次（客户端没在跑时不会触发本拦截）；\
             ② 或从开始菜单 / 桌面图标单独启动 TwinSwitch 后再操作。",
            region.display_name(),
            region.display_name()
        )),
        Placement::Unknown { reason } => Some(format!(
            "已拒绝：读不全本进程的父链（{reason}），无法确认结束 {} 会不会连本工具一起结束。\n\
             ⇒ 请从开始菜单 / 桌面图标单独启动 TwinSwitch 再操作（在终端或脚本里启动时最容易出现这种情况）。",
            region.display_name()
        )),
    }
}

// ---------------------------------------------------------------- 进程守卫

/// Qoder 客户端是否在跑 —— **两个版本的进程名都要查**。
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

    // ★ fail-closed 安全门：先证明「杀掉它不会连本工具一起死」。
    //   放在「确认有进程要杀」之后 —— 客户端没在跑时这一步根本不触发，
    //   日常用法（从桌面图标启动、客户端已退出）不会被拦。
    if let Some(msg) = placement_blocker(region, &placement(region)) {
        return Err(msg);
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
    // ① **用户手动指定的主程序优先**（设置页「Qoder 客户端主程序路径」）。
    //    为什么会需要：Qoder 允许装到任意目录，`install.ini` 也可能被卸载残留删掉；
    //    这时自动探测报「没找到启动程序」，用户没有任何可操作的地方。
    if let Some(exe) = crate::modules::settings::qoder_exe_override(region) {
        std::process::Command::new(&exe)
            .spawn()
            .map_err(|e| format!("启动失败（{}）：{e}", exe.display()))?;
        return Ok(exe.display().to_string());
    }

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

    fn kids(pairs: &[(u32, u32)]) -> std::collections::HashMap<u32, Vec<u32>> {
        let mut m: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
        for &(parent, child) in pairs {
            m.entry(parent).or_default().push(child);
        }
        m
    }

    /// 父链干净、没有目标进程 ⇒ 放行。
    #[test]
    fn placement_outside_is_allowed() {
        // Qoder(1000) 的子树里没有我(2000)
        let tree = kids(&[(1000, 1100), (1100, 1101)]);
        assert!(!is_descendant(&tree, 1000, 2000));
        let p = Placement::Outside;
        assert!(placement_blocker(QoderRegion::Cn, &p).is_none());
    }

    /// 自己是 Qoder 的孙进程（内嵌终端 → shell → 本工具）⇒ 必须判 Hosted，并给出出路。
    #[test]
    fn placement_hosted_by_descendant_walk() {
        let tree = kids(&[(1000, 1100), (1100, 1200)]);
        assert!(is_descendant(&tree, 1000, 1200), "隔一层也要认出来");
        let p = Placement::Hosted {
            evidence: "本进程是 Qoder CN.exe 的子进程（pid 1000）".to_string(),
        };
        let msg = placement_blocker(QoderRegion::Cn, &p).expect("必须拦住");
        assert!(msg.contains("已拒绝"), "{msg}");
        assert!(msg.contains("开始菜单"), "必须给出独立启动这条出路：{msg}");
    }

    /// 环境标记优先于树遍历：客户端注入的标记本身就是硬证据。
    #[test]
    fn env_marker_is_detected() {
        let marks = env_markers_from(&|k| {
            if k == "QODERCN_SESSION_TYPE" {
                Some("app".to_string())
            } else {
                None
            }
        });
        assert_eq!(marks, vec!["QODERCN_SESSION_TYPE=app".to_string()]);
        // 空值不算标记（客户端可能把它设成空串）
        let none = env_markers_from(&|_| Some("   ".to_string()));
        assert!(none.is_empty());
    }

    /// 兄弟进程 / 自身不在 Qoder 子树里时不许误判（防止把正常用户拦住）。
    #[test]
    fn unrelated_process_is_not_descendant() {
        let tree = kids(&[(1000, 1100), (2000, 2100)]);
        assert!(is_descendant(&tree, 1000, 1100));
        assert!(!is_descendant(&tree, 1000, 2100), "另一个 shell 的子树不算");
        assert!(!is_descendant(&tree, 9999, 2100), "根本不存在的根不算");
    }

    /// ⚠️ 真判不出来（进程表都读不到）⇒ **必须**拦住（fail-closed）。
    #[test]
    fn placement_unknown_blocks_by_default() {
        let p = Placement::Unknown {
            reason: "读不到本机进程表".to_string(),
        };
        let msg = placement_blocker(QoderRegion::Cn, &p).expect("Unknown 必须拦住");
        assert!(msg.contains("已拒绝"), "{msg}");
    }

    /// 现场探针：打印本进程的父链与本区域的判定结果（只打进程名，不碰任何凭据）。
    ///
    /// ```text
    /// cargo test -p wb-switch-core --lib probe_placement -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn probe_placement() {
        for region in [QoderRegion::Cn, QoderRegion::Global] {
            let p = placement(region);
            eprintln!("[placement] {} => {p:?}", region.as_str());
            match &p {
                Placement::Outside => eprintln!("   → 放行（可以安全杀进程）"),
                Placement::Hosted { evidence } => eprintln!("   → 拦（{evidence}）"),
                Placement::Unknown { reason } => eprintln!("   → 拦（{reason}）"),
            }
        }
    }

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
