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
use crate::modules::{process_list, settings};
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

/// 探测 WorkBuddy 可执行文件 —— **唯一出口**，顺序即优先级：
///
/// 1. **用户在设置里手动指定**的路径（`settings.json` 的 `workbuddyExe`；
///    文件不存在就当没指定，见 [`settings::workbuddy_exe_override`]）；
/// 2. 上次探测成功记下的缓存；
/// 3. 常见安装路径；
/// 4. **它正在跑的进程**（`QueryFullProcessImageNameW`）—— 装到自定义目录时靠这条。
///
/// 不用注册表（`reg.exe` 在受限环境会被拦截），也不用 `wmic`（新系统已移除）。
pub fn resolve_exe() -> Option<PathBuf> {
    if let Some(p) = settings::workbuddy_exe_override() {
        return Some(p);
    }
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
    // 兜底：问一次**正在跑的进程**（Electron 多进程，取第一个读得到路径的那条）。
    let names: Vec<String> = workbuddy_auth::WINDOWS_IMAGE_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    for p in crate::modules::process_list::find_by_names_ex(&names) {
        let Some(exe) = crate::modules::process_list::exe_path_of_pid(p.pid) else {
            continue;
        };
        if exe.is_file() {
            save_exe_cache(&exe);
            return Some(exe);
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

/// 切号时用户勾选的会话同步范围（界面传过来，`None` = 老行为：只看看设置）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSelection {
    /// `selected` = 只同步 `session_ids`；`none` = 这次不同步；其余按设置。
    pub mode: String,
    #[serde(default)]
    pub session_ids: Vec<String>,
}

/// 切换到指定账号。
///
/// `relaunch = true` 时写成功后重新启动客户端。返回切换报告。
///
/// `sessions` 是界面上那张会话勾选表的结果（`None` = 用户没走勾选流程，按设置来）。
pub fn switch_to(
    id: &str,
    relaunch: bool,
    sessions: Option<SessionSelection>,
) -> Result<Value, String> {
    let acc = workbuddy_vault::find_account(id)
        .ok_or_else(|| format!("账号不存在：{id}"))?;
    if crate::modules::workbuddy_auth::secret_value(&acc, "access_token").is_none() {
        return Err("该账号没有可用凭据，无法切换（可删除后重新导入本机登录态）".to_string());
    }
    let target_uid = crate::modules::workbuddy_auth::get_str(&acc, "uid").unwrap_or_default();

    // 0) 记下切换前的账号：定时任务是按 `owner_user_id` 归属的，同步时要用它当来源。
    //    ⚠️ 必须在写登录态**之前**取，写完读到的就是目标账号了。
    let previous_uid = workbuddy_auth::current_uid();

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

    // 4.5) 把定时任务同步给新账号（可在设置里关闭）。
    //
    // ⚠️ 失败**不阻断**切号：定时任务是附属功能，切号本身已经成功了，
    //    把报告带回界面照实显示即可（不许静默，也不许因此回滚登录态）。
    let automation_sync = if settings::sync_workbuddy_automations() {
        match previous_uid.as_deref().filter(|u| !u.is_empty()) {
            Some(from) if from != target_uid => {
                match crate::modules::workbuddy_automations::sync_owner(from, &target_uid) {
                    Ok(v) => v,
                    Err(e) => json!({ "ok": false, "error": e }),
                }
            }
            _ => json!({ "ok": true, "notNeeded": true, "reason": "没有切换前的账号或同一个账号" }),
        }
    } else {
        json!({ "ok": true, "notNeeded": true, "reason": "设置里已关闭定时任务同步" })
    };

    // 4.6) 会话同步（设置里打开才做）—— **只做增量，绝不搬历史存量**。
    //
    // 位置**不能挪**：必须在 `quit_client` 之后（会话库没有客户端在写）、
    // `launch` 之前（新账号起来后库又被占用）。放到「切号成功之后」是错的：
    // 那时目标账号正是当前登录账号，会被 `TARGET_IN_USE` 门禁直接拒掉。
    //
    // ⚠️⚠️ 为什么切号只做增量：实测本机 4 账号 / 25 会话时，一次全量要复制
    //    **74 个副本、约 2.98 GB**。把它塞进「切号」这种高频动作，用户会遇到
    //    「切个号卡几分钟 + 磁盘少 3 GB」，而且没有任何确认机会。
    //    历史存量走会话页的「一键互相关联」（先给数字、再确认）；
    //    切号只搬 `wb_link_since` 之后新建的会话，首次（起点为 0）连复制都不做、
    //    只登记已有副本（0 字节）。
    // ⚠️ 失败**不阻断**切号：切号本身已经成功，会话同步是附属动作，照实带回去。
    // 勾选模式优先于设置：用户在切号弹窗里明确点过，就照他的选择来。
    let selected: Option<Vec<String>> = sessions.as_ref().and_then(|s| {
        if s.mode == "selected" {
            Some(s.session_ids.clone())
        } else {
            None
        }
    });
    let skip_all = sessions.as_ref().map(|s| s.mode == "none").unwrap_or(false);

    let session_link = if let Some(ids) = selected {
        // ⚠️ 勾了但一条都没勾上 = 用户就是不想同步，不是「同步失败」。
        if ids.is_empty() {
            json!({ "ok": true, "notNeeded": true, "reason": "本次切换没有勾选任何会话" })
        } else {
            match crate::modules::workbuddy_sessions::link_all(
                crate::modules::workbuddy_sessions::LinkOptions {
                    require_closed: false,
                    copy_missing: true,
                    since: 0,
                    only_sessions: Some(ids.clone()),
                    target_uid: Some(target_uid.clone()),
                    // 勾选的「已关联」会话要把内容同步过去 —— 这正是「切号自动同步」。
                    refresh_linked: true,
                },
            ) {
                Ok(v) => v,
                Err(e) => json!({ "ok": false, "error": e, "selectedCount": ids.len() }),
            }
        }
    } else if skip_all {
        json!({ "ok": true, "notNeeded": true, "reason": "本次切换未勾选会话同步" })
    } else if settings::auto_link_workbuddy_sessions() {
        let since = settings::wb_link_since();
        let first = since <= 0;
        match crate::modules::workbuddy_sessions::link_all(
            crate::modules::workbuddy_sessions::LinkOptions::new(false, !first, since),
        ) {
            Ok(mut v) => {
                // 成功才推进起点（失败时下次还会重试这批，不会漏）。
                // 这里写不进去只影响「下次重复登记一次」（幂等、无害），不阻断切号。
                if let Err(e) = settings::set_wb_link_since(now_ms()) {
                    if let Some(o) = v.as_object_mut() {
                        o.insert(
                            "progressWarning".to_string(),
                            json!(format!("没记住同步进度（下次会重复登记一次，无害）：{e}")),
                        );
                    }
                }
                if first {
                    if let Some(o) = v.as_object_mut() {
                        o.insert("firstRun".to_string(), json!(true));
                    }
                }
                v
            }
            Err(e) => json!({ "ok": false, "error": e }),
        }
    } else {
        json!({ "ok": true, "notNeeded": true, "reason": "设置里未开启「切号时自动同步会话」" })
    };

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
        "automationSync": automation_sync,
        "sessionLink": session_link,
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
        let err = switch_to("__no_such_account__", false, None).unwrap_err();
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
