//! 进程枚举与结束：**一次系统调用**拿全量进程表，交给调用方按映像名过滤。
//!
//! ## 模块里有什么
//!
//! | 用途 | 入口 |
//! | --- | --- |
//! | 只想要 `(映像名, pid)` | [`all_processes`] / [`find_by_names`] |
//! | 还要父子关系（挑 Electron 主进程） | [`all_processes_ex`] / [`find_by_names_ex`] / [`tree_roots`] |
//! | 跑一条外部命令（**带 stderr**） | [`run_cmd`] |
//! | 结束客户端并确认真的退了 | [`kill_tree_and_wait`] |
//!
//! ## 为什么要有这个模块
//!
//! 之前 Trae / WorkBuddy 两侧都靠 `tasklist /FI "IMAGENAME eq X.exe"` **逐个映像名**
//! 起一次子进程：一次 `tasklist` 冷启动约 300–500 ms，Trae 两个客户端 × 各自若干
//! 映像名，加上 WorkBuddy 的一串名字，光枚举进程就要 3 秒以上。
//! 这就是首页「本机概览」重算 3.5 s 的主要构成（见 `app_overview::snapshot`）。
//!
//! 这里换成「一个窗口看全表」：
//!
//! - Windows：`CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` 一次拿全部
//!   `(映像名, pid, 父 pid)`，本机实测 < 20 ms，比逐名 `tasklist` 快两个数量级；
//! - 其他平台：一次 `ps -A -o pid=,ppid=,comm=`。
//!
//! ## 语义与原来保持一致
//!
//! 返回的仍是 `(映像名, pid)`（Windows 上带 `.exe`），**过滤逻辑留在调用方**——
//! 这样 `trae_switch` 能继续把 `ai-agent` 单列、`workbuddy_switch` 能继续按
//! `WINDOWS_IMAGE_NAMES` 判定，行为与改前逐字节一致。
//!
//! 不做缓存：枚举本身只有毫秒级，而切换账号时「杀进程 → 立刻复查是否还在跑」
//! 依赖结果**必须实时**，加 TTL 反而会误判。

/// 一个进程条目的最小信息。
///
/// 比 `(String, u32)` 多带一个**父进程 PID**：WorkBuddy / Trae 都是 Electron
/// 多进程应用，同一映像名会同时出现主进程、GPU、utility、renderer、crashpad 等，
/// 靠父子关系才认得出谁是主进程（见 `workbuddy_export::pick_main_process`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    /// 映像名（Windows 上带 `.exe`）。
    pub name: String,
    pub pid: u32,
    /// 父进程 PID。父进程可能早已退出，这个值仍然保留（Windows 的行为）。
    pub parent: u32,
}

/// 枚举本机全部进程（含父子关系）。
///
/// 失败返回空表（调用方按「没有相关进程在跑」处理，与旧实现一致）。
#[cfg(windows)]
pub fn all_processes_ex() -> Vec<ProcInfo> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out: Vec<ProcInfo> = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                // szExeFile 是定长宽字符数组，遇到 NUL 截断。
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if !name.is_empty() && entry.th32ProcessID > 0 {
                    out.push(ProcInfo {
                        name,
                        pid: entry.th32ProcessID,
                        parent: entry.th32ParentProcessID,
                    });
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// 枚举本机全部进程（非 Windows：一次 `ps`，不逐名 pgrep）。
#[cfg(not(windows))]
pub fn all_processes_ex() -> Vec<ProcInfo> {
    use std::process::{Command, Stdio};

    let Ok(out) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let mut it = line.split_whitespace();
            let pid: u32 = it.next()?.parse().ok()?;
            let parent: u32 = it.next()?.parse().unwrap_or(0);
            let name = it.next()?;
            // ps 的 comm 可能是全路径，只保留末段，与 Windows 的映像名语义对齐。
            let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
            (!base.is_empty()).then(|| ProcInfo {
                name: base.to_string(),
                pid,
                parent,
            })
        })
        .collect()
}

/// 枚举本机全部进程，返回 `(映像名, pid)`（不需要父子关系时用这个）。
pub fn all_processes() -> Vec<(String, u32)> {
    all_processes_ex()
        .into_iter()
        .map(|p| (p.name, p.pid))
        .collect()
}

/// 取某个进程**主程序的绝对路径**（`QueryFullProcessImageNameW`）。
///
/// 用途：Trae 允许把程序装到任意目录，自动探测扫不到时，「它现在正在跑」
/// 就是最可靠的路径来源 —— 用户开着客户端点一下就能把路径填对。
///
/// 失败返回 `None`（权限不足 / 进程已退出 / 非 Windows 平台）。**不报错**：
/// 对调用方来说「拿不到」和「没有这个进程」是同一件事，不必区分。
#[cfg(windows)]
pub fn exe_path_of_pid(pid: u32) -> Option<std::path::PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    unsafe {
        // PROCESS_QUERY_LIMITED_INFORMATION：权限要求最低的一档，普通用户可开。
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        if r.is_err() || len == 0 {
            return None;
        }
        Some(std::path::PathBuf::from(String::from_utf16_lossy(
            &buf[..len as usize],
        )))
    }
}

/// 非 Windows：拿不到（本工具只在 Windows 上跑 Trae，这里仅为编译通过）。
#[cfg(not(windows))]
pub fn exe_path_of_pid(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

/// 归一化映像名：去空白 / 去引号 / 去 `.exe` 后缀 / 转小写。
fn norm(name: &str) -> String {
    let s = name.trim().trim_matches('"').to_ascii_lowercase();
    s.strip_suffix(".exe").map(str::to_string).unwrap_or(s)
}

/// 按映像名过滤全量进程表（不区分大小写，`.exe` 可省）。
///
/// 与旧实现 `tasklist /FI "IMAGENAME eq X.exe"` 的匹配口径一致：**精确**匹配整名，
/// 不是前缀匹配。
pub fn find_by_names(names: &[String]) -> Vec<(String, u32)> {
    all_processes()
        .into_iter()
        .filter(|(n, _)| names.iter().map(|k| norm(k)).any(|w| w == norm(n)))
        .collect()
}

/// 同 [`find_by_names`]，但保留**父进程 PID**（Electron 应用挑主进程要用）。
pub fn find_by_names_ex(names: &[String]) -> Vec<ProcInfo> {
    all_processes_ex()
        .into_iter()
        .filter(|p| names.iter().map(|k| norm(k)).any(|w| w == norm(&p.name)))
        .collect()
}

/// 从匹配集合里挑出「树根」：父进程**不在**同一集合里的那些。
///
/// WorkBuddy / Trae 都是 Electron：主进程 + GPU/utility/renderer/crashpad 同映像名。
/// 只对树根发一次 `taskkill /T`（连带整棵子树）就够，不必逐个 PID 打一遍 ——
/// 后者既慢（N 次 `taskkill` 冷启动）又会在杀到一半时打断子进程的父子链。
pub fn tree_roots(procs: &[ProcInfo]) -> Vec<ProcInfo> {
    let pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
    procs
        .iter()
        .filter(|p| !pids.contains(&p.parent))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// 起子进程 / 结束进程
// ---------------------------------------------------------------------------

/// 一次外部命令的结果。
///
/// **stderr 一定要收**：`taskkill` 失败原因（拒绝访问 / 找不到进程 / 已被其它
/// 进程占用）全部写在 stderr 上，旧实现只收 stdout、把 stderr 丢进 `null`，
/// 于是「杀不掉」在界面上只剩一句没有信息量的「未能在 N 秒内退出」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CmdResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdResult {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// 给日志用的一行摘要：stderr 优先（错误都写在那里），压成单行并截断。
    pub fn detail(&self) -> String {
        let raw = if !self.stderr.trim().is_empty() {
            self.stderr.as_str()
        } else {
            self.stdout.as_str()
        };
        let flat: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() > 200 {
            let mut s: String = flat.chars().take(200).collect();
            s.push('…');
            s
        } else {
            flat
        }
    }
}

/// 解码一条**控制台命令**的输出。
///
/// ⚠️ 中文 Windows 上 `taskkill` / `kill` 的输出是 **GBK（OEM 代码页）**，不是 UTF-8。
/// 以前一律 `from_utf8_lossy`：中文全变成 `\u{FFFD}`，用户看到的就是
/// 「…（一串乱码）… PID 33104（乱码）…」（2026-11-02 用户截图）。
///
/// 而且更糟：代码里还用 `detail().contains("找不到")` 判「进程已经没了」——
/// 乱码之后这个判断永远为假，于是「已经退出」被当成「拒绝访问」报给用户。
///
/// 这里不引编码库：**能按 UTF-8 解就解，解不了只保留 ASCII**（PID/英文关键字还在），
/// 中文一律丢弃并标注。反正最终给用户的结论由 [`terminate_direct`] 的错误码给出，
/// 不再依赖这些本地化文案。
fn decode_console(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let ascii: String = bytes
                .iter()
                .map(|b| if b.is_ascii_graphic() || *b == b' ' || *b == b'\n' { *b as char } else { ' ' })
                .collect();
            let flat = ascii.split_whitespace().collect::<Vec<_>>().join(" ");
            // 全中文输出（ASCII 部分为空）也要留下这句说明 ——
            // 返回空串会让用户以为「命令什么都没说」，实际是说了但我们解不了。
            if flat.trim().is_empty() {
                "（该命令输出非 UTF-8，中文已省略）".to_string()
            } else {
                format!("{flat}（该命令输出非 UTF-8，中文已省略）")
            }
        }
    }
}

/// 直接 `TerminateProcess` 一个 PID，返回**结构化的失败原因**（不依赖本地化文案）。
///
/// 为什么需要它：`taskkill` 失败只说一句本地化的话，还分不清到底是
/// 「进程已经没了」「拒绝访问（要管理员）」还是「受保护」。这些正是用户要采取
/// 不同动作的地方，所以用错误码自己判。
#[cfg(windows)]
pub fn terminate_direct(pid: u32) -> Result<(), String> {
    use windows::Win32::Foundation::{CloseHandle, GetLastError};
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    if pid == 0 {
        return Err("PID 0 不是可结束的进程".to_string());
    }
    unsafe {
        let h = match OpenProcess(PROCESS_TERMINATE, false, pid) {
            Ok(h) => h,
            Err(_) => {
                // 5 = ERROR_ACCESS_DENIED；87 = 参数错（进程已不在）；其余按码报。
                return Err(match GetLastError().0 {
                    5 => "拒绝访问（该进程权限更高，需要以管理员身份运行本工具）".to_string(),
                    87 => "进程已退出".to_string(),
                    c => format!("打开进程失败（Win32 错误码 {c}）"),
                });
            }
        };
        let r = TerminateProcess(h, 1);
        let _ = CloseHandle(h);
        if r.is_err() {
            return Err(format!("终止进程失败（Win32 错误码 {}）", GetLastError().0));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn terminate_direct(pid: u32) -> Result<(), String> {
    let args = vec!["-9".to_string(), pid.to_string()];
    match run_cmd("kill", &args, std::time::Duration::from_secs(10)) {
        Some(r) if r.ok() => Ok(()),
        Some(r) => Err(format!("kill 返回 {:?}", r.code)),
        None => Err("kill 未能启动或超时".to_string()),
    }
}

/// 跑一条外部命令并带超时；起不来或超时返回 `None`（超时会顺手 `kill` 掉子进程）。
pub fn run_cmd(program: &str, args: &[String], timeout: std::time::Duration) -> Option<CmdResult> {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW：别在用户屏幕上闪黑框
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => return None,
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = child.wait_with_output().ok()?;
    Some(CmdResult {
        code: out.status.code(),
        stdout: decode_console(&out.stdout),
        stderr: decode_console(&out.stderr),
    })
}

/// 结束一轮匹配到的进程（每个树根一次 `taskkill /T /F`）。
///
/// 返回 `(出现过的映像名去重列表, 失败原因列表)`。**只杀一轮、不等**，
/// 要「杀到真的没了」用 [`kill_tree_and_wait`]。
pub fn kill_now(names: &[String]) -> (Vec<String>, Vec<String>) {
    let procs = find_by_names_ex(names);
    let mut seen: Vec<String> = procs.iter().map(|p| p.name.clone()).collect();
    seen.sort();
    seen.dedup();

    // 绝不自杀：万一本工具恰好也落在匹配集里（例如以 WorkBuddy.exe 名义跑的
    // 内嵌 CLI），杀了自己就没法把结果回给界面了。
    let me = std::process::id();
    let roots: Vec<ProcInfo> = tree_roots(&procs)
        .into_iter()
        .filter(|r| r.pid != me)
        .collect();
    if roots.is_empty() {
        return (seen, Vec::new());
    }

    #[cfg(windows)]
    {
        // ⚠️⚠️ **一次 taskkill 打完所有树根**，不要逐 PID 各起一次。
        // `/F /T` 对命令行上每个 `/PID` 都生效，语义与逐个调用等价；
        // 但 `taskkill` 每次冷启动要 150–300 ms —— Trae 3.4.9 有二十来个同映像名进程，
        // 逐个调用会把「等它退出」的整个预算吃光（2026-10-28 实测：一轮还没跑完就到点，
        // 界面只报「未能退出」，像极了「杀不掉」，实际是「还没杀完」）。
        let mut args: Vec<String> = vec!["/F".to_string(), "/T".to_string()];
        for r in &roots {
            args.push("/PID".into());
            args.push(r.pid.to_string());
        }
        match run_cmd("taskkill", &args, std::time::Duration::from_secs(20)) {
            Some(res) if res.ok() => (seen, Vec::new()),
            Some(res) => {
                // 合并调用失败时**退回逐个**：只有逐个跑才说得清「哪个 PID、为什么」，
                // 而失败原因正是排查所需要的（拒绝访问 / 找不到进程 / 无法终止）。
                let mut errors: Vec<String> = Vec::new();
                // 「找不到进程」= 合并那一轮已经把它杀掉了，不算失败 ——
                // 否则复核时满屏「找不到」，把真正的原因（拒绝访问）淹掉。
                let gone = |s: &str| s.contains("找不到") || s.to_lowercase().contains("not found");
                for r in &roots {
                    let a = vec![
                        "/PID".to_string(),
                        r.pid.to_string(),
                        "/T".to_string(),
                        "/F".to_string(),
                    ];
                    match run_cmd("taskkill", &a, std::time::Duration::from_secs(20)) {
                        Some(x) if x.ok() => {}
                        Some(x) if gone(&x.detail()) => {}
                        Some(x) => errors.push(format!(
                            "结束 PID {} 失败（taskkill 返回 {:?}）：{}",
                            r.pid,
                            x.code,
                            x.detail()
                        )),
                        None => errors.push(format!(
                            "结束 PID {} 失败：taskkill 未能启动或超时",
                            r.pid
                        )),
                    }
                }
                if errors.is_empty() {
                    // 合并调用返回非 0、但逐个复查都成功（或本来就已经没了）⇒
                    // 只是命令行上某个 PID 在那一瞬间退出过，不算失败。
                    (seen, Vec::new())
                } else {
                    errors.insert(
                        0,
                        format!(
                            "合并结束 {} 个进程失败（taskkill 返回 {:?}）：{}",
                            roots.len(),
                            res.code,
                            res.detail()
                        ),
                    );
                    (seen, errors)
                }
            }
            None => (
                seen,
                vec![format!(
                    "结束 {} 个进程失败：taskkill 未能启动或超时",
                    roots.len()
                )],
            ),
        }
    }
    #[cfg(not(windows))]
    {
        let mut errors = Vec::new();
        for r in &roots {
            let args = vec!["-9".to_string(), r.pid.to_string()];
            match run_cmd("kill", &args, std::time::Duration::from_secs(20)) {
                Some(res) if res.ok() => {}
                Some(res) => errors.push(format!(
                    "结束 PID {} 失败（kill 返回 {:?}）：{}",
                    r.pid,
                    res.code,
                    res.detail()
                )),
                None => errors.push(format!("结束 PID {} 失败：kill 未能启动或超时", r.pid)),
            }
        }
        (seen, errors)
    }
}

/// 「结束客户端并确认真的没了」的结果。
#[derive(Debug, Clone, Default)]
pub struct KillOutcome {
    pub ok: bool,
    /// 实际发了多少轮结束命令（每轮都重新枚举 + 复查）。
    pub attempts: u32,
    /// 过程中出现过的映像名（去重）。
    pub killed: Vec<String>,
    /// 超时那一刻仍在跑的进程。
    pub remaining: Vec<(String, u32)>,
    /// 残留进程号与**首轮**完全不重叠 ⇒ 它是被重新拉起来的，不是杀不掉。
    pub restarted: bool,
    /// 每轮失败原因（taskkill 的 stderr 摘要等）。
    pub errors: Vec<String>,
    /// 结束失败时**逐条原因**：`(映像名, PID, 原因)`。
    ///
    /// 与 `errors` 的区别：这里的原因是我们自己判的（`TerminateProcess` 错误码），
    /// 不是 `taskkill` 的本地化文案 —— 后者在中文系统上既读不懂又容易被编码搞乱。
    pub blockers: Vec<(String, u32, String)>,
}

impl KillOutcome {
    /// 给用户看的一句话解释：区分「杀不掉」与「被重新拉起」。
    pub fn hint(&self, label: &str) -> String {
        if self.restarted {
            return format!(
                "{} 在被结束后又重新出现了（进程号全换了），多半是它正在自动更新、或被守护进程/更新器拉了起来。请等它稳定不再自动重启后再试。",
                label
            );
        }
        let pids: Vec<String> = self.remaining.iter().map(|(_, p)| p.to_string()).collect();
        let mut s = format!(
            "{} 仍有 {} 个进程没退出（PID {}）",
            label,
            self.remaining.len(),
            pids.join(", ")
        );
        if !self.blockers.is_empty() {
            // 按原因归类：用户要看到的是「该怎么办」，不是一串 PID。
            let mut by_reason: Vec<(String, Vec<u32>)> = Vec::new();
            for (_, pid, why) in &self.blockers {
                match by_reason.iter_mut().find(|(r, _)| r == why) {
                    Some((_, v)) => v.push(*pid),
                    None => by_reason.push((why.clone(), vec![*pid])),
                }
            }
            s.push_str("。原因：");
            s.push_str(
                &by_reason
                    .iter()
                    .map(|(why, ps)| {
                        format!(
                            "{} 个{}",
                            ps.len(),
                            why
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("；"),
            );
            s.push_str("。可以：① 手动退出该客户端后重试；② 以管理员身份运行本工具再切；③ 若它刚自动更新过，等更新结束再试");
        } else if !self.errors.is_empty() {
            s.push_str("；结束命令的返回：");
            s.push_str(&self.errors.join("；"));
        }
        s
    }
}

/// 结束匹配到的进程（含子树）并等待它们真的消失，期间**反复重试**。
///
/// ## 为什么不只是「杀一次 + 等 20 秒」
///
/// 旧实现是 `kill_all()` 后死等 20 秒，超时直接报「未能在 20 秒内退出」。这条
/// 策略在真机上会翻车（2026-10-06 实测）：WorkBuddy 当时正在**自我升级**
/// （`~/.workbuddy/logs/AppStartup.log` 记着 `startup_type=upgrade`，5.6.2 → 5.7.6），
/// 旧进程被杀掉后**更新器立刻把新版拉了起来**，于是「进程表永远非空」→ 20 秒后
/// 报失败，而真相是「杀掉了，但被重启了」。此时若继续往数据库里写，就会和正在
/// 启动的客户端抢库。
///
/// 所以这里的口径是：
/// 1. **每轮都重新枚举**，把新冒出来的进程也一并结束（最多等到 `timeout`）；
/// 2. 超时后区分两种结局 —— 残留的 PID 与首轮**完全不重叠** = 被重新拉起
///    （[`KillOutcome::restarted`]），否则才是真的杀不掉；
/// 3. 把 `taskkill` 的 stderr 原样带出来，不再只剩一句没有信息量的超时。
///
/// `is_clear` 决定「算不算都退干净了」：WorkBuddy 要求匹配集为空；Trae 不把
/// `ai-agent` 算作客户端进程，于是传自己的判据。
pub fn kill_tree_and_wait(
    names: &[String],
    timeout: std::time::Duration,
    is_clear: &dyn Fn(&[(String, u32)]) -> bool,
) -> KillOutcome {
    use std::time::Instant;

    let snapshot = || -> Vec<(String, u32)> {
        find_by_names_ex(names)
            .into_iter()
            .map(|p| (p.name, p.pid))
            .collect()
    };

    // 首轮只枚举一次：既当「already clear」的判据，也留下首轮 PID 供后面区分
    // 「被杀掉后又被拉起来」与「真的杀不掉」。
    let first = snapshot();
    if is_clear(&first) {
        return KillOutcome {
            ok: true,
            ..KillOutcome::default()
        };
    }
    let first_pids: Vec<u32> = first.iter().map(|(_, pid)| *pid).collect();

    let deadline = Instant::now() + timeout;
    let mut attempts = 0u32;
    // 连续「全部都是拒绝访问」的轮数（确定性失败，早退用）。
    let mut denied_rounds = 0u32;
    let mut killed: Vec<String> = Vec::new();
    // 每轮重新取；只保留最后一轮的错误（20 轮全留会把清单撑爆，且失败原因通常稳定）。
    let mut errors: Vec<String>;

    loop {
        attempts += 1;
        let (round_names, round_errors) = kill_now(names);
        for n in round_names {
            if !killed.contains(&n) {
                killed.push(n);
            }
        }
        errors = round_errors;

        // taskkill 返回成功 ≠ 进程已经消失，给系统一点时间再复查。
        std::thread::sleep(std::time::Duration::from_millis(400));
        let now = snapshot();
        if is_clear(&now) {
            return KillOutcome {
                ok: true,
                attempts,
                killed,
                remaining: Vec::new(),
                restarted: false,
                errors,
                blockers: Vec::new(),
            };
        }

        // 第二轮起换**直接终止**：`taskkill` 在中文系统上失败时只吐一句本地化的话，
        // 既说不清原因（拒绝访问 / 已退出 / 受保护），也容易被编码搞成乱码。
        // `TerminateProcess` 能拿到错误码，结论是我们自己判的（2026-11-02 用户反馈）。
        if attempts >= 2 {
            let mut ok_any = false;
            let mut denied = 0usize;
            for (_, pid) in &now {
                match terminate_direct(*pid) {
                    Ok(()) => ok_any = true,
                    Err(e) if e.contains("拒绝访问") => denied += 1,
                    Err(_) => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            let after = snapshot();
            if is_clear(&after) {
                return KillOutcome {
                    ok: true,
                    attempts,
                    killed,
                    remaining: Vec::new(),
                    restarted: false,
                    errors,
                    blockers: Vec::new(),
                };
            }
            // 权限被拒是**确定性失败**：再来 14 轮也是同一个结果。
            // 连拒 3 轮就直接收工，把「要管理员」这个结论早点给用户
            // （不然他要盯着转 20 秒，还不知道卡在哪）。
            if !ok_any && denied >= after.len() && denied > 0 {
                denied_rounds += 1;
                if denied_rounds >= 3 {
                    let mut detailed = errors;
                    let mut blockers: Vec<(String, u32, String)> = Vec::new();
                    for (name, pid) in &after {
                        let why = terminate_direct(*pid)
                            .err()
                            .unwrap_or_else(|| "仍然无法结束".to_string());
                        if why.contains("已退出") {
                            continue;
                        }
                        detailed.push(format!("{name}（PID {pid}）：{why}"));
                        blockers.push((name.clone(), *pid, why));
                    }
                    return KillOutcome {
                        ok: false,
                        attempts,
                        killed,
                        remaining: after,
                        restarted: false,
                        errors: detailed,
                        blockers,
                    };
                }
            } else {
                denied_rounds = 0;
            }
        }

        if Instant::now() >= deadline {
            let restarted = !now.is_empty() && !now.iter().any(|(_, pid)| first_pids.contains(pid));
            // 最后一轮：把「还剩哪些、为什么」逐条问出来（不看 taskkill 的文案）。
            let mut detailed = errors;
            let mut blockers: Vec<(String, u32, String)> = Vec::new();
            for (name, pid) in &now {
                match terminate_direct(*pid) {
                    Ok(()) => {}
                    // 「进程已退出」说明这条只是快照过时，不算失败。
                    Err(why) if why.contains("已退出") => {}
                    Err(why) => {
                        detailed.push(format!("{name}（PID {pid}）：{why}"));
                        blockers.push((name.clone(), *pid, why));
                    }
                }
            }
            if detailed.is_empty() {
                detailed.push("已发出结束命令，但系统还没回收这些进程".to_string());
            }
            return KillOutcome {
                ok: false,
                attempts,
                killed,
                remaining: now,
                restarted,
                errors: detailed,
                blockers,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_strips_exe_quotes_and_case() {
        assert_eq!(norm("Trae CN.exe"), "trae cn");
        assert_eq!(norm("\"WorkBuddy.EXE\""), "workbuddy");
        assert_eq!(norm("  Trae  "), "trae");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn all_processes_contains_ourselves() {
        // 至少能枚举到当前进程本身 —— 证明快照链路真的通。
        let me = std::process::id();
        let list = all_processes();
        assert!(!list.is_empty(), "全量进程表不该为空");
        assert!(list.iter().any(|(_, pid)| *pid == me), "全量进程表里应当有本进程 {me}");
    }

    #[test]
    fn find_by_names_is_exact_not_prefix() {
        let me = std::process::id();
        let all = all_processes();
        let mine = all.iter().find(|(_, p)| *p == me).map(|(n, _)| n.clone());
        let Some(mine) = mine else {
            return; // 拿不到自己的名字就跳过（非 Windows 环境极少见）
        };
        // 全名能命中自己
        assert!(find_by_names(&[mine.clone()]).iter().any(|(_, p)| *p == me));
        // 名字去掉 .exe 也能命中
        let no_ext = mine.strip_suffix(".exe").unwrap_or(&mine).to_string();
        assert!(find_by_names(&[no_ext]).iter().any(|(_, p)| *p == me));
        // 前缀（截断一个字符）不该命中：口径是精确匹配
        if mine.chars().count() > 3 {
            let cut: String = mine.chars().take(mine.chars().count() - 1).collect();
            assert!(!find_by_names(&[cut]).iter().any(|(_, p)| *p == me));
        }
    }

    #[test]
    fn find_by_names_handles_empty_and_unknown() {
        assert!(find_by_names(&[]).is_empty());
        assert!(find_by_names(&["definitely-not-a-real-process-xyz.exe".to_string()]).is_empty());
    }

    #[test]
    fn tree_roots_keeps_only_top_of_each_tree() {
        let procs = vec![
            ProcInfo { name: "App.exe".into(), pid: 10, parent: 4 },
            ProcInfo { name: "App.exe".into(), pid: 11, parent: 10 },
            ProcInfo { name: "App.exe".into(), pid: 12, parent: 11 },
            // 父进程不在匹配集里（比如由系统拉起的另一个实例）→ 自己就是树根
            ProcInfo { name: "App.exe".into(), pid: 20, parent: 2 },
        ];
        let roots: Vec<u32> = tree_roots(&procs).iter().map(|p| p.pid).collect();
        assert_eq!(roots, vec![10, 20]);
    }

    #[test]
    fn tree_roots_of_empty_is_empty() {
        assert!(tree_roots(&[]).is_empty());
    }

    #[test]
    fn cmd_result_detail_prefers_stderr_and_flattens() {
        let r = CmdResult {
            code: Some(128),
            stdout: "SUCCESS".into(),
            stderr: "错误: 没有找到进程 \"1234\".\r\n".into(),
        };
        assert!(!r.ok());
        assert!(r.detail().contains("没有找到进程"));
        assert!(!r.detail().contains('\n'), "摘要必须是单行：{}", r.detail());

        let ok = CmdResult { code: Some(0), stdout: "成功".into(), stderr: String::new() };
        assert!(ok.ok());
        assert_eq!(ok.detail(), "成功");
    }

    /// 控制台输出解码：GBK 的中文**不许**变成一串 `\u{FFFD}`（用户截图里的乱码）。
    #[test]
    fn console_output_never_emits_mojibake() {
        // UTF-8 正常放行
        assert_eq!(decode_console("ERROR: The process 123 could not be terminated.".as_bytes()),
                   "ERROR: The process 123 could not be terminated.");
        assert_eq!(decode_console("中文原样".as_bytes()), "中文原样");

        // GBK 的「拒绝访问。」= BE DC BE F8 B7 C3 CE CA A1 A3
        let gbk = [0xBE, 0xDC, 0xBE, 0xF8, 0xB7, 0xC3, 0xCE, 0xCA, 0xA1, 0xA3];
        let out = decode_console(&gbk);
        assert!(!out.contains('\u{FFFD}'), "不许出现替换字符（那就是乱码）");
        assert!(out.contains("非 UTF-8"), "要明确说中文被省略了: {out}");

        // 纯 ASCII 混在 GBK 里时，ASCII 部分（PID / 关键字）要留住
        let mixed = b"PID 33104 \xBE\xDC\xBE\xF8\xB7\xC3";
        let out2 = decode_console(mixed);
        assert!(out2.contains("33104"), "ASCII 信息必须保留: {out2}");
        assert!(!out2.contains('\u{FFFD}'));
    }

    /// 失败提示必须说「怎么办」，而不是把一串 PID 堆给用户。
    #[test]
    fn hint_offers_actions_when_processes_refuse_to_die() {
        let out = KillOutcome {
            ok: false,
            attempts: 16,
            killed: vec!["WorkBuddy.exe".into()],
            remaining: vec![("WorkBuddy.exe".into(), 33104), ("WorkBuddy.exe".into(), 33328)],
            restarted: false,
            errors: vec![],
            blockers: vec![
                ("WorkBuddy.exe".into(), 33104, "拒绝访问（该进程权限更高，需要以管理员身份运行本工具）".into()),
                ("WorkBuddy.exe".into(), 33328, "拒绝访问（该进程权限更高，需要以管理员身份运行本工具）".into()),
            ],
        };
        let h = out.hint("WorkBuddy");
        assert!(h.contains("2 个"), "同类原因要合并计数: {h}");
        assert!(h.contains("管理员"), "要给出可执行建议: {h}");
        assert!(!h.contains('\u{FFFD}'));
    }

    #[test]
    fn kill_tree_and_wait_is_noop_when_nothing_matches() {
        // 用一个绝不可能存在的映像名：应当立刻返回成功、且一次命令都不发。
        let out = kill_tree_and_wait(
            &["definitely-not-a-real-process-xyz.exe".to_string()],
            std::time::Duration::from_millis(500),
            &|m| m.is_empty(),
        );
        assert!(out.ok);
        assert_eq!(out.attempts, 0);
        assert!(out.killed.is_empty());
        assert!(out.errors.is_empty());
    }

    #[test]
    fn kill_outcome_hint_distinguishes_restart_from_stubborn() {
        let restarted = KillOutcome {
            ok: false,
            remaining: vec![("App.exe".into(), 999)],
            restarted: true,
            ..KillOutcome::default()
        };
        assert!(restarted.hint("App").contains("重新出现"));

        let stubborn = KillOutcome {
            ok: false,
            remaining: vec![("App.exe".into(), 999)],
            errors: vec!["结束 PID 999 失败：拒绝访问".into()],
            ..KillOutcome::default()
        };
        let h = stubborn.hint("App");
        assert!(h.contains("999"));
        assert!(h.contains("拒绝访问"));
    }
}
