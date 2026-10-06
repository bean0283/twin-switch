//! WorkBuddy（国内版）账号切换编排：结束客户端 → 备份 → 写登录态 → 校验 → 重启。
//!
//! 与 Trae 侧的 `trae_switch` 同构，但载体不同：
//! - Trae 切换改的是客户端 `storage.json` 等**载体文件**；
//! - WorkBuddy 切换只改**一份官方登录态文件** `workbuddy-desktop.info`
//!   （`workbuddy_auth::auth_file_path`）。
//!
//! 失败回滚靠切换前那一份备份：写登录态失败或写后校验不过，就把备份复制回去。
//!
//! ⚠️ 切换会**结束 WorkBuddy 全部进程**，当前未保存的会话会中断。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::modules::config::{now_ms, store_dir};
use crate::modules::process_list;
use crate::modules::workbuddy_auth;
use crate::modules::workbuddy_vault;

// ---------------------------------------------------------------------------
// 进程
// ---------------------------------------------------------------------------

/// 需要匹配的进程映像名（不含 `.exe`）。
///
/// WorkBuddy 是 Electron 应用，主进程与渲染 / 工具子进程同名，
/// 按映像名一次枚举即可覆盖全部。
fn image_names() -> Vec<String> {
    workbuddy_auth::WINDOWS_IMAGE_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// 枚举 WorkBuddy 进程，返回 `(映像名, pid)`。
///
/// 走 [`process_list::find_by_names`]（一次全量快照后内存过滤），
/// 不再按映像名逐个起 `tasklist`。
pub fn list_processes() -> Vec<(String, u32)> {
    process_list::find_by_names(&image_names())
}

/// 客户端是否在运行。
pub fn is_running() -> bool {
    !list_processes().is_empty()
}

/// 结束全部 WorkBuddy 进程（只对**树根**发一次 `taskkill /T /F`，不等结果）。
///
/// 返回出现过的映像名列表。要「杀到真的没了」用 [`wait_until_stopped`]。
pub fn kill_all() -> Vec<String> {
    process_list::kill_now(&image_names()).0
}

/// 等待进程全部退出；期间**反复重试**杀死新冒出来的进程。
///
/// 单纯「等」是不够的：客户端如果被更新器/守护进程重新拉起，进程表永远非空。
/// 详见 [`process_list::kill_tree_and_wait`]。
pub fn wait_until_stopped(timeout_ms: u64) -> bool {
    quit_client(timeout_ms).ok
}

/// 退出客户端并拿到完整结果（谁被结束、几轮、失败原因、是不是被重启了）。
pub fn quit_client(timeout_ms: u64) -> process_list::KillOutcome {
    process_list::kill_tree_and_wait(
        &image_names(),
        Duration::from_millis(timeout_ms),
        &|m| m.is_empty(),
    )
}

// ---------------------------------------------------------------------------
// 客户端路径与启动
// ---------------------------------------------------------------------------

fn exe_cache_file() -> PathBuf {
    store_dir().join("workbuddy-exe.json")
}

fn load_exe_cache() -> Option<PathBuf> {
    let text = std::fs::read_to_string(exe_cache_file()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let p = PathBuf::from(v.get("path").and_then(|x| x.as_str())?);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

fn save_exe_cache(p: &Path) {
    let _ = std::fs::create_dir_all(store_dir());
    let _ = std::fs::write(
        exe_cache_file(),
        serde_json::to_string(&json!({ "path": p, "savedAt": now_ms() })).unwrap_or_default(),
    );
}

/// 探测 WorkBuddy 可执行文件：缓存 → 常见安装路径。
///
/// 不用注册表（`reg.exe` 在受限环境会被拦截），也不用 `wmic`（新系统已移除）。
pub fn resolve_exe() -> Option<PathBuf> {
    if let Some(cached) = load_exe_cache() {
        return Some(cached);
    }
    let name = workbuddy_auth::WINDOWS_IMAGE_NAMES[0];
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(pf) = std::env::var("PROGRAMFILES") {
        candidates.push(Path::new(&pf).join(name).join(format!("{name}.exe")));
    }
    if let Ok(pf86) = std::env::var("PROGRAMFILES(X86)") {
        candidates.push(Path::new(&pf86).join(name).join(format!("{name}.exe")));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(Path::new(&local).join("Programs").join(name).join(format!("{name}.exe")));
        candidates.push(Path::new(&local).join(name).join(format!("{name}.exe")));
    }
    for c in candidates {
        if c.exists() {
            save_exe_cache(&c);
            return Some(c);
        }
    }
    None
}

/// 启动客户端。
pub fn launch() -> Result<PathBuf, String> {
    let exe = resolve_exe().ok_or_else(|| {
        "未找到 WorkBuddy 安装位置，请手动启动客户端（或把它装到默认目录后重试）".to_string()
    })?;
    Command::new(&exe)
        .spawn()
        .map_err(|e| format!("启动 WorkBuddy 失败（{}）：{e}", exe.display()))?;
    Ok(exe)
}

// ---------------------------------------------------------------------------
// 切换
// ---------------------------------------------------------------------------

fn last_switch_file() -> PathBuf {
    store_dir().join("workbuddy-last-switch.json")
}

/// 上一次切换记录（含备份路径），供回滚用。
pub fn last_switch() -> Option<Value> {
    let text = std::fs::read_to_string(last_switch_file()).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_last_switch(v: &Value) {
    let _ = std::fs::create_dir_all(store_dir());
    let _ = std::fs::write(
        last_switch_file(),
        serde_json::to_string_pretty(v).unwrap_or_default(),
    );
}

/// 切换到指定账号。
///
/// `relaunch = true` 时写成功后重新启动客户端。返回切换报告。
pub fn switch_to(id: &str, relaunch: bool) -> Result<Value, String> {
    let acc = workbuddy_vault::find_account(id)
        .ok_or_else(|| format!("账号不存在：{id}"))?;
    if crate::modules::workbuddy_auth::secret_value(&acc, "access_token").is_none() {
        return Err("该账号没有可用凭据，无法切换（可删除后重新导入本机登录态）".to_string());
    }
    let target_uid = crate::modules::workbuddy_auth::get_str(&acc, "uid").unwrap_or_default();

    // 1) 备份当前登录态（可能没有登录态 → None）
    let backup = workbuddy_auth::backup_auth_file();

    // 2) 结束客户端并等待退出（反复重试；区分「杀不掉」与「被更新器拉起来」）
    let quit = quit_client(20_000);
    if !quit.ok {
        return Err(format!(
            "WorkBuddy 未能退出（重试 {} 轮），切换已中止。{}",
            quit.attempts,
            quit.hint("WorkBuddy")
        ));
    }
    let killed = quit.killed;

    // 3) 写登录态
    if let Err(e) = workbuddy_auth::write_account_to_auth_file(&acc) {
        // 写失败：立刻用备份还原，不让用户停在「登录态被写坏」的状态
        if let Some(b) = &backup {
            let _ = restore_backup(b);
        }
        return Err(format!("写入登录态失败：{e}（已尝试还原备份）"));
    }

    // 4) 写后校验：当前 uid 必须是目标
    let now_uid = workbuddy_auth::current_uid();
    if !target_uid.is_empty() && now_uid.as_deref() != Some(target_uid.as_str()) {
        if let Some(b) = &backup {
            let _ = restore_backup(b);
        }
        return Err(format!(
            "切换后校验失败：登录态里的账号是 {:?}，期望 {target_uid}（已还原备份）",
            now_uid
        ));
    }

    // 5) 记录切换点（供回滚）+ 记录使用
    save_last_switch(&json!({
        "accountId": id,
        "uid": target_uid,
        "backup": backup,
        "at": now_ms(),
    }));
    workbuddy_vault::mark_used(id);

    // 6) 可选重启
    let mut relaunched = false;
    if relaunch {
        relaunched = launch().is_ok();
    }

    Ok(json!({
        "ok": true,
        "accountId": id,
        "uid": target_uid,
        "backup": backup,
        "killed": killed,
        "relaunched": relaunched,
    }))
}

/// 回滚到上一次切换前的登录态。
pub fn rollback() -> Result<Value, String> {
    let last = last_switch().ok_or_else(|| "没有可回滚的切换记录".to_string())?;
    let backup = last
        .get("backup")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "上一次切换没有留下登录态备份，无法回滚".to_string())?;
    let backup_path = PathBuf::from(backup);
    if !backup_path.exists() {
        return Err(format!("备份文件已不存在：{backup}"));
    }
    let killed = quit_client(20_000).killed;
    restore_backup(&backup_path)?;
    let uid = workbuddy_auth::current_uid();
    Ok(json!({ "ok": true, "uid": uid, "killed": killed, "restoredFrom": backup }))
}

fn restore_backup(backup: &Path) -> Result<(), String> {
    let dest = workbuddy_auth::auth_file_path();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(backup, &dest).map_err(|e| e.to_string())?;
    // 备份里没有退出标记语义：还原后清理，避免 WorkBuddy 仍显示未登录
    let mut marker = dest.as_os_str().to_os_string();
    marker.push(".logged-out");
    match std::fs::remove_file(PathBuf::from(marker)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("清理退出标记失败：{e}")),
    }
    Ok(())
}

/// 切换前的门禁检查（只读）：进程状态、登录态可用性、目标账号是否已在登录。
pub fn precheck(id: &str) -> Value {
    let acc = workbuddy_vault::find_account(id);
    let current = workbuddy_auth::current_uid();
    let has_token = acc
        .as_ref()
        .map(|a| crate::modules::workbuddy_auth::secret_value(a, "access_token").is_some())
        .unwrap_or(false);
    let target_uid = acc
        .as_ref()
        .and_then(|a| crate::modules::workbuddy_auth::get_str(a, "uid"))
        .unwrap_or_default();
    json!({
        "running": is_running(),
        "exe": resolve_exe(),
        "loggedIn": workbuddy_auth::is_logged_in(),
        "currentUid": current,
        "hasToken": has_token,
        "alreadyCurrent": !target_uid.is_empty() && current.as_deref() == Some(target_uid.as_str()),
        "authFilePath": workbuddy_auth::auth_file_path(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_names_covers_workbuddy() {
        let names = image_names();
        assert!(names.iter().any(|n| n == "WorkBuddy"), "{names:?}");
    }

    #[test]
    fn switch_to_unknown_account_is_rejected_without_touching_auth_file() {
        // 用一个不可能存在的 id：必须在写登录态之前就失败
        let err = switch_to("__no_such_account__", false).unwrap_err();
        assert!(err.contains("账号不存在"), "错误应说明账号不存在: {err}");
    }

    #[test]
    fn resolve_exe_does_not_panic_when_missing() {
        // 只验证不 panic：本机可能装了也可能没装
        let _ = resolve_exe();
    }

    #[test]
    fn precheck_reports_unknown_account_as_not_current() {
        let v = precheck("__no_such_account__");
        assert_eq!(v["hasToken"], false);
        assert_eq!(v["alreadyCurrent"], false);
        assert!(v["authFilePath"].is_string());
    }

    #[test]
    fn rollback_without_record_is_rejected() {
        // 只在没有切换记录时断言失败分支；有记录时不改动真实登录态
        if last_switch().is_none() {
            let err = rollback().unwrap_err();
            assert!(err.contains("回滚"), "错误应说明无法回滚: {err}");
        }
    }
}
