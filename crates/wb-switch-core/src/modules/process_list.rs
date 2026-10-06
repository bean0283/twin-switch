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
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
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

    let me = std::process::id();
    let mut errors = Vec::new();
    for root in tree_roots(&procs) {
        // 绝不自杀：万一本工具恰好也落在匹配集里（例如以 WorkBuddy.exe 名义跑的
        // 内嵌 CLI），杀了自己就没法把结果回给界面了。
        if root.pid == me {
            continue;
        }
        #[cfg(windows)]
        {
            let args = vec![
                "/PID".to_string(),
                root.pid.to_string(),
                "/T".to_string(),
                "/F".to_string(),
            ];
            match run_cmd("taskkill", &args, std::time::Duration::from_secs(20)) {
                Some(res) if res.ok() => {}
                Some(res) => errors.push(format!(
                    "结束 PID {} 失败（taskkill 返回 {:?}）：{}",
                    root.pid,
                    res.code,
                    res.detail()
                )),
                None => errors.push(format!("结束 PID {} 失败：taskkill 未能启动或超时", root.pid)),
            }
        }
        #[cfg(not(windows))]
        {
            let args = vec!["-9".to_string(), root.pid.to_string()];
            match run_cmd("kill", &args, std::time::Duration::from_secs(20)) {
                Some(res) if res.ok() => {}
                Some(res) => errors.push(format!(
                    "结束 PID {} 失败（kill 返回 {:?}）：{}",
                    root.pid,
                    res.code,
                    res.detail()
                )),
                None => errors.push(format!("结束 PID {} 失败：kill 未能启动或超时", root.pid)),
            }
        }
    }
    (seen, errors)
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
        if !self.errors.is_empty() {
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
            };
        }
        if Instant::now() >= deadline {
            let restarted = !now.is_empty() && !now.iter().any(|(_, pid)| first_pids.contains(pid));
            return KillOutcome {
                ok: false,
                attempts,
                killed,
                remaining: now,
                restarted,
                errors,
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
