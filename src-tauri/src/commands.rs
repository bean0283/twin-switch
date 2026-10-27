//! Tauri commands：前端调用的薄包装，对应 trae-switch 原版的账号/记录能力。
//!
//! 覆盖：Trae 客户端发现 / 账号库 / 切换 / 网页登录 / 解密 / 导入 / 删除 / 交接记忆，
//! 外加 relaunch、错误日志等通用命令。

use serde_json::{json, Value};

use std::path::PathBuf;

use tauri::Emitter;
use wb_switch_core::modules::{
    app_overview, checkin, client_usage, config, error_log, settings, trae_backup, trae_cleanup,
    trae_credits, trae_delete,
    trae_discover, trae_export, trae_handoff, trae_import, trae_memory_scan, trae_oauth,
    trae_profile, trae_remote, trae_session_edit, trae_switch, trae_vault, workbuddy_cleanup,
    workbuddy_auth, workbuddy_credits, workbuddy_export, workbuddy_import, workbuddy_oauth,
    workbuddy_session_edit, workbuddy_sessions, workbuddy_switch, workbuddy_vault,
};
use wb_switch_core::modules::{gateway_keys, gateway_models, gateway_pool, gateway_server, qoder_accounts, qoder_api, qoder_auth_dat, qoder_cleanup, qoder_login, qoder_realm, qoder_sessions, qoder_tools};

// ---------------------------------------------------------------------------
// 阻塞型命令的统一出口：**别把重活留在主线程上**
// ---------------------------------------------------------------------------

/// 把阻塞型工作挪到后台线程执行。
///
/// **Tauri 的同步命令跑在主线程**——Windows 上那同时是 WebView2 的消息循环线程。
/// 命令体里只要做「递归扫盘 / 开 SQLite / 枚举进程」，整个窗口就会冻住：
/// 按钮点不动、滚动卡死，**连已经回来的其它 IPC 回包也派发不出去**
/// （回包要走主线程 → WebView，主线程被占着就永远派发不到界面）。
///
/// 这正是「启动后约 10 秒无法操作」的根因，跟磁盘缓存读得快不快**无关**：
/// 首页一挂载就并行触发 `trae_cleanup_scan` + `trae_wb_cleanup_scan`
/// （几 GB 的递归统计），而这两条当时是同步命令，把主线程占满约 10 秒；
/// 排在它们后面的「总览缓存」回包只能干等 —— 缓存明明读到了却迟迟画不出来。
///
/// 用法：`off_main(move || 重活()).await?`。外层 `Result` 只承载线程池本身出错
/// （`JoinError`，实际上不会发生），内层按原命令的返回语义原样透传。
async fn off_main<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("后台任务失败: {e}"))
}

// ---------------------------------------------------------------------------
// 通用命令
// ---------------------------------------------------------------------------

/// 启动当前应用的新进程并退出旧进程，用于更新安装完成后的立即重启。
#[tauri::command]
pub fn relaunch_app(_app: tauri::AppHandle) -> Result<(), String> {
    relaunch_app_inner(&_app)
}

/// 重启实现：保留单实例交棒，避免旧进程未退出时新进程抢锁失败。
pub(crate) fn relaunch_app_inner<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| format!("无法定位应用程序: {e}"))?;
    let args = std::env::args_os().skip(1);
    // 先放弃单例身份（删除 socket，并在 macOS 上释放 flock）再交棒：否则新进程
    // 可能在旧 listener / 锁消失前连上或抢锁失败，出现「旧进程已退、新进程也退出」
    // 而应用彻底消失。
    #[cfg(desktop)]
    tauri_plugin_single_instance::destroy(_app);
    #[cfg(target_os = "macos")]
    crate::instance_lock::release(_app);
    match std::process::Command::new(executable).args(args).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            // 已经放弃单例身份：要么把锁拿回来继续跑，要么退出。
            // 不允许「无锁继续运行」（否则之后再启动就会双开）。
            #[cfg(target_os = "macos")]
            if !crate::instance_lock::reacquire(_app) {
                std::process::exit(0);
            }
            Err(format!("启动应用失败: {e}"))
        }
    }
}

// ---------------------------------------------------------------------------
// 错误日志（前端崩溃 / 未捕获错误落盘）
// ---------------------------------------------------------------------------

/// 记录一条错误日志（`kind` 白名单：frontend_crash / frontend_unhandled / backend）。
///
/// 只落盘、不返回失败：目录只读、磁盘满等写入失败由 core 静默降级（`let _ =`），
/// 绝不让「记日志」反过来打断前端主流程。
#[tauri::command]
pub async fn log_error(kind: String, message: String, detail: Option<String>) {
    error_log::record(&kind, &message, detail.as_deref().unwrap_or_default());
}

/// 错误日志文件路径（设置页展示用）。
#[tauri::command]
pub fn get_error_log_path() -> String {
    error_log::error_log_path().to_string_lossy().to_string()
}

/// 数据目录与搬迁状态（诊断用，纯路径计算 + 读一个进程内静态）。
///
/// `note` 非空说明「数据目录没按预期走」，目前两种：
/// 旧目录改名失败（本次仍用旧目录），或新目录就绪但旧目录仍在（可能有旧实例在跑）。
/// 界面如实展示即可 —— **不要**在这里做任何清理动作。
#[tauri::command]
pub fn store_dir_info() -> Value {
    let (dir, note) = config::ensure_store_dir_ready();
    json!({
        "dir": dir.to_string_lossy(),
        "legacyDirName": config::STORE_DIR_NAME_LEGACY,
        "note": note,
    })
}

/// 在文件管理器中定位错误日志；日志尚未生成时改为定位所在目录。
///
/// 走 tauri-plugin-opener 的 Rust API（不依赖前端 capability）；reveal 内部会
/// canonicalize，路径不存在会直接报错，所以这里按「文件 → 目录」逐级回退。
#[tauri::command]
pub fn reveal_error_log(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let path = error_log::error_log_path();
    // reveal 会 canonicalize，目标不存在就直接失败。日志还没生成时改为定位目录；
    // 目录也不存在（还没写过任何错误）就先建出来，避免按钮第一次点就失败。
    let target = if path.exists() {
        path
    } else {
        match path.parent() {
            Some(dir) => {
                let _ = std::fs::create_dir_all(dir);
                if dir.exists() {
                    dir.to_path_buf()
                } else {
                    path
                }
            }
            None => path,
        }
    };
    app.opener()
        .reveal_item_in_dir(target)
        .map_err(|error| format!("打开日志位置失败: {error}"))
}

/// 在文件管理器中定位任意文件/目录（导出文件、解密库等通用）。目录不存在时逐级回退。
#[tauri::command]
pub fn reveal_path(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let p = std::path::PathBuf::from(&path);
    let target = if p.exists() {
        p
    } else {
        match p.parent() {
            Some(dir) => {
                let _ = std::fs::create_dir_all(dir);
                if dir.exists() {
                    dir.to_path_buf()
                } else {
                    return Err(format!("路径不存在：{path}"));
                }
            }
            None => return Err(format!("路径不存在：{path}")),
        }
    };
    app.opener()
        .reveal_item_in_dir(target)
        .map_err(|error| format!("打开路径失败: {error}"))
}

/// 导出目录绝对路径（前端提示 / 打开用）。
#[tauri::command]
pub fn trae_export_dir() -> String {
    trae_export::export_dir().to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Trae 模块：客户端发现 / 账号库 / 切换 / 解密 / 导出 / 删除 / 交接记忆
// ---------------------------------------------------------------------------

/// 列出已安装的 Trae 客户端（含登录态与安装路径）。
///
/// 要遍历各客户端的安装目录与配置文件，走后台（见 [`off_main`]）。
///
/// **顺序由使用记忆决定**（`client_usage`）：按「切换次数 ×3 + 打开页面次数」降序排，
/// 没有历史时退回内置偏好顺序（`solo-cn` 第一）。`topPick` 是排在最前且**确有使用历史**
/// 的那个 key，界面据此打「常用」徽标。
#[tauri::command]
pub async fn trae_list_clients(refresh: Option<bool>) -> Value {
    let refresh = refresh.unwrap_or(false);
    off_main(move || {
        // 「重新检测」= 丢掉注册表探测缓存再算一次 —— 否则装了新客户端之后
        // 界面会一直返回第一次的结果，与「检测不到」长得一模一样。
        if refresh {
            trae_discover::invalidate_discovery_cache();
        }
        let mut clients = trae_discover::list_installed_clients();
        client_usage::sort_installed(&mut clients);
        let keys: Vec<&str> = clients.iter().map(|c| c.key).collect();
        json!({
            "clients": clients,
            "usage": client_usage::snapshot_for(&keys),
        })
    })
    .await
    .unwrap_or_else(|e| json!({ "clients": [], "error": e }))
}

/// 清空「客户端使用记忆」，排序回到内置偏好（`solo-cn` 第一）。
#[tauri::command]
pub async fn trae_client_usage_reset() -> Value {
    off_main(|| {
        client_usage::reset();
        json!({ "ok": true })
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// Trae 账号总览：当前登录态（describe_account）+ 账号库已建档列表。
/// 资料（昵称 / 积分）只读缓存，不自动访问接口（避免风控）；
/// 登录成功由后端拉取一次，之后仅能通过 trae_refresh_profile 手动刷新。
///
/// 顺带记一次「使用记忆」：打开这个客户端的账号页 = 在用这个客户端。
#[tauri::command]
pub async fn trae_account_overview(client_key: String) -> Result<Value, String> {
    if trae_discover::get_client(&client_key).is_none() {
        return Err(format!("未知客户端：{client_key}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        client_usage::record_use(&client_key);
        let client = trae_discover::get_client(&client_key).ok_or("未知客户端")?;
        let live = trae_switch::describe_account(&client_key);
        // ⚠️ 「当前登录」判据的**唯一来源**：客户端登录态解出来的 uid（未登录 = 空串）。
        // 要在 `live` 被 move 进 json! 之前取出来。
        let current_uid = live
            .get("uid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let vault: Vec<Value> = trae_vault::list_vault_accounts(&client_key)
            .iter()
            .map(|id| {
                let meta = trae_vault::read_meta(&client_key, id);
                let kind = meta
                    .as_ref()
                    .map(|m| m.kind.clone())
                    .unwrap_or_else(|| "carrier".into());
                let oauth = if kind == "oauth" {
                    trae_oauth::read_oauth_account(&client_key, id)
                } else {
                    None
                };
                // ⚠️ 与 WorkBuddy 的 `isCurrent`（`workbuddy_vault::list`）**同源同形**：
                // 上游一次算好、前端只读 —— 界面各处**不许再自己拼判据**。
                //
                // T65 四改前前端拼的是 `meta.verified_uid === live.uid`，而 `meta.verified_uid`
                // 只在**切换成功**时写入、oauth 账号连 `meta` 都没有 ⇒ 三个账号一个徽标都不亮。
                // 现在统一走 `trae_oauth::account_identity`（oauth 读自己的 oauth.json，
                // 载体从库内 storage 解），与「账号还没加入账号库」那个 Alert 用同一份 uid。
                let account_uid = trae_oauth::account_identity(&client_key, id).map(|(u, _)| u);
                let is_current =
                    !current_uid.is_empty() && account_uid.as_deref() == Some(current_uid.as_str());
                json!({
                    "id": id,
                    "meta": meta,
                    "kind": kind,
                    "oauth": oauth,
                    "displayName": trae_vault::display_name(&client_key, id),
                    "profile": trae_vault::read_profile(&client_key, id),
                    // uid 与 `isCurrent` **必须是同一份**：徽标亮着的那张卡，
                    // 卡面上显示的 uid 就得是判「当前登录」时用的那个 uid。
                    // 前端只在它为 null 时才回退到自己解析。
                    "uid": account_uid,
                    "isCurrent": is_current,
                })
            })
            .collect();
        Ok(json!({
            "clientKey": client_key,
            "loggedIn": live.get("loggedIn").cloned().unwrap_or(json!(false)),
            "running": trae_switch::is_running(client),
            "live": live,
            "vault": vault,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 手动刷新单个账号资料（GetUserInfo 真实昵称 + 积分余额），写入 profile.json 缓存。
/// 仅在用户点击「刷新积分」时调用，避免频繁自动访问接口触发风控。
#[tauri::command]
pub async fn trae_refresh_profile(client_key: String, account_id: String) -> Result<Value, String> {
    let oauth = trae_oauth::read_oauth_account(&client_key, &account_id)
        .ok_or("该账号不是网页凭证账号，或凭证已缺失（无 oauth.json）")?;
    let profile = trae_profile::refresh_profile(&client_key, &account_id, &oauth)
        .await
        .ok_or("刷新失败：接口不可用或凭证已失效（可能已退出登录）")?;
    let display_name = profile
        .get("screen_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| trae_vault::display_name(&client_key, &account_id));
    Ok(json!({ "id": account_id, "profile": profile, "displayName": display_name }))
}

/// 账号库里的积分（额度 + 逐个积分包）**离线快照**。
///
/// 只读各账号的 `profile.json`，不联网、毫秒级 —— 首屏先拿它渲染，
/// 随后前端再用 [`trae_credits_query`] 覆盖成实时值。
#[tauri::command]
pub async fn trae_credits_cached(client_key: Option<String>) -> Result<Value, String> {
    off_main(move || trae_credits::cached(client_key)).await
}

/// 查询账号库积分。
///
/// - `force = false`（默认）：5 分钟内的缓存直接返回，不打接口；
/// - `force = true`：逐个有网页凭证的账号真打接口（`user_current_entitlement_list`），
///   结果写入各自的 `profile.json`。
///
/// 单个账号失败（token 过期 / 网络不通）不影响其它账号，会以 `ok = false` + `error`
/// 出现在同一份结果里。接口调用本身是异步的，不占主线程。
#[tauri::command]
pub async fn trae_credits_query(client_key: Option<String>, force: Option<bool>) -> Result<Value, String> {
    Ok(trae_credits::query(client_key, force.unwrap_or(false)).await)
}

/// 只刷新**一个** Trae 账号的积分（账号卡上的小刷新按钮）。
///
/// 与 [`trae_credits_query`] 的区别：那个是整批刷新并写整份磁盘缓存，点一个账号会把
/// 全部账号都打一遍接口；这个只打该账号一次，返回的形状与整批结果里的单条一致。
#[tauri::command]
pub async fn trae_credits_one(client_key: Option<String>, id: String) -> Result<Value, String> {
    Ok(trae_credits::query_one(client_key, &id).await)
}

/// 一键签到（Trae / WorkBuddy 全部账号）。
///
/// - `scope`：`"trae"` / `"workbuddy"` / `"all"`（默认）；
/// - `client_key`：只在 Trae 侧生效，限定本客户端下的账号（账号管理页按当前选中的客户端签）。
///
/// 网络请求本身是异步的，不占主线程；结果里每个账号一条，
/// `status` 六档（`success` / `already` / `skipped` / `unavailable` / `inactive` / `failed`）
/// **绝不允许合并** —— 「跳过」和「失败」长得一样就是骗人。
#[tauri::command]
pub async fn checkin_run(scope: Option<String>, client_key: Option<String>) -> Result<Value, String> {
    Ok(checkin::run(scope.as_deref(), client_key.as_deref()).await)
}

/// 识别当前登录账号（写入账号库前调用，拿 uid 做归属）。
#[tauri::command]
pub async fn trae_identify_live(client_key: String) -> Result<Value, String> {
    let client = trae_discover::get_client(&client_key).ok_or("未知客户端")?;
    let root_dir = trae_discover::user_data_dir(client);
    tauri::async_runtime::spawn_blocking(move || {
        trae_vault::identify(&client_key, &root_dir).ok_or_else(|| {
            "未识别到当前登录账号（storage.json 中无可读账号信息），请先登录该客户端".to_string()
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 把当前登录态备份进账号库（写入前自动识别 uid）。
#[tauri::command]
pub async fn trae_backup_account(client_key: String, account_id: String) -> Result<Value, String> {
    let client = trae_discover::get_client(&client_key).ok_or("未知客户端")?;
    let root_dir = trae_discover::user_data_dir(client);
    tauri::async_runtime::spawn_blocking(move || {
        let entries = trae_switch::effective_entries(&client_key, &root_dir);
        if entries.is_empty() {
            return Err("没有探测到任何登录态载体文件，无法备份。请先在该客户端登录一次。".into());
        }
        let verified = trae_vault::identify(&client_key, &root_dir)
            .and_then(|v| v.get("uid").and_then(|u| u.as_str()).map(String::from));
        match trae_vault::backup(&client_key, &account_id, &root_dir, &entries) {
            Ok(meta) => Ok(json!({ "ok": true, "accountId": account_id, "verifiedUid": verified, "meta": meta })),
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 切换到账号库中的某账号（冷切换：终止进程 → 还原载体 → 重启 → daemon 判定）。
#[tauri::command]
pub async fn trae_switch_to(client_key: String, account_id: String) -> Result<Value, String> {
    if trae_discover::get_client(&client_key).is_none() {
        return Err(format!("未知客户端：{client_key}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let progress: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let r = trae_switch::switch_to(&client_key, &account_id, Some(&|m| {
            progress.lock().unwrap().push(m.to_string());
        }));
        let progress = progress.into_inner().unwrap_or_default();
        match r {
            Ok(sr) => {
                // 切号成功才记账：失败的那次不代表「我在用这个客户端」。
                client_usage::record_switch(&client_key);
                let mut v = serde_json::to_value(&sr).map_err(|e| e.to_string())?;
                v["progress"] = json!(progress);
                Ok(v)
            }
            Err(e) => Err(format!("{e}\n{}", progress.join("\n"))),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 回滚到某账号（本质是切回它，用于切换异常后的恢复）。
#[tauri::command]
pub async fn trae_rollback_to(client_key: String, account_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let progress: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let r = trae_switch::rollback_to(&client_key, &account_id, Some(&|m| {
            progress.lock().unwrap().push(m.to_string());
        }));
        let progress = progress.into_inner().unwrap_or_default();
        match r {
            Ok(sr) => {
                client_usage::record_switch(&client_key);
                let mut v = serde_json::to_value(&sr).map_err(|e| e.to_string())?;
                v["progress"] = json!(progress);
                Ok(v)
            }
            Err(e) => Err(format!("{e}\n{}", progress.join("\n"))),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 从账号库删除某个账号的备份（只删本地档案，不影响客户端登录态）。
#[tauri::command]
pub async fn trae_remove_account(client_key: String, account_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        trae_vault::remove_account(&client_key, &account_id)?;
        Ok(json!({ "ok": true }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 重命名账号库条目（按账号名管理）。
#[tauri::command]
pub async fn trae_rename_account(client_key: String, from_id: String, to_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let new_id = trae_vault::rename_account(&client_key, &from_id, &to_id)?;
        Ok(json!({ "ok": true, "id": new_id }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 导出账号备份为自包含 JSON（文件内容 base64 内联）。
#[tauri::command]
pub async fn trae_export_account(client_key: String, account_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || trae_vault::export_account(&client_key, &account_id))
        .await
        .map_err(|e| e.to_string())?
}

/// 导出**账号包**（写盘版）：把该客户端下所有账号打进一个自包含 JSON 存到磁盘，
/// 返回文件路径 —— 前端拿它去 `reveal_path` 定位，用户不用自己找下载目录。
#[tauri::command]
pub async fn trae_export_pack(client_key: String) -> Result<Value, String> {
    off_main(move || trae_vault::export_pack(&client_key)).await?
}

/// 导入账号备份（自包含 JSON，preferName 可选覆盖账号名）。
#[tauri::command]
pub async fn trae_import_account(client_key: String, payload: Value, prefer_name: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (id, count) = trae_vault::import_account(&client_key, &payload, prefer_name.as_deref())?;
        Ok(json!({ "ok": true, "id": id, "files": count }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 把「已并入 `client_key` 的客户端」账号库里的账号全部并入它。
///
/// 2026-10 官方把 TRAE SOLO 并进 Trae 时（国内版 → `trae-cn`，国际版 → `trae-intl`）
/// 只搬了程序与会话，**本工具自己的账号库**官方不会碰 ⇒ SOLO 下备份过的账号在承载方
/// 下看不见、切不过去。
/// `dry_run = true` 只预演（列出将搬哪些），一个字节都不写。
#[tauri::command]
pub async fn trae_vault_adopt_merged(client_key: String, dry_run: Option<bool>) -> Result<Value, String> {
    // 会读目录树 + 解析账号档案，按主线程硬规则离开主线程。
    off_main(move || trae_vault::adopt_merged_accounts(&client_key, dry_run.unwrap_or(false)))
        .await?
}

/// 账号 token 弹窗的数据：jwt 本体 + 有效期 + 能不能续期。**只读、不联网。**
#[tauri::command]
pub async fn trae_account_token(client_key: String, account_id: String) -> Result<Value, String> {
    // 要读账号目录里的 oauth.json / storage.json（含解密），按主线程硬规则离开主线程。
    off_main(move || trae_remote::account_token_info(&client_key, &account_id)).await?
}

/// 刷新账号 token（界面上的「刷新 token 有效期」）。
///
/// `force = false` 时，access token 剩余 > 7 天**不算失败**：返回
/// `{ ok: true, refreshed: false, reason: "fresh" }`，由界面弹窗让用户决定要不要强制刷新。
/// 真失败（网络 / 服务端拒绝 / 写回自检不过）才返回 `Err`。
#[tauri::command]
pub async fn trae_account_token_refresh(
    client_key: String,
    account_id: String,
    force: Option<bool>,
) -> Result<Value, String> {
    trae_remote::refresh_account_token(&client_key, &account_id, force.unwrap_or(false)).await
}

/// WorkBuddy 账号 token 弹窗的数据：jwt 本体 + 有效期 + 能不能续期。**只读、不联网。**
#[tauri::command]
pub async fn workbuddy_account_token(account_id: String) -> Result<Value, String> {
    // 要读账号库 JSON，按主线程硬规则离开主线程。
    off_main(move || workbuddy_credits::account_token_info(&account_id)).await?
}

/// 刷新 WorkBuddy 账号 token（界面上的「刷新 token 有效期」）。
///
/// 语义与 Trae 侧完全一致：`force = false` 且剩余 > 7 天 ⇒ `{ refreshed: false, reason: "fresh" }`
/// （不是失败），界面据此弹窗让用户决定要不要强制刷新；真失败才 `Err`。
#[tauri::command]
pub async fn workbuddy_account_token_refresh(
    account_id: String,
    force: Option<bool>,
) -> Result<Value, String> {
    workbuddy_credits::refresh_account_token_info(&account_id, force.unwrap_or(false)).await
}

/// 已存盘的 SQLCipher 密钥（供状态展示）。
#[tauri::command]
pub fn trae_saved_key(client_key: String) -> Value {
    json!({ "clientKey": client_key, "key": trae_memory_scan::load_saved_key(&client_key) })
}

/// 一键：扫描进程内存提密钥 → 校验 HMAC → 解密整库到明文 SQLite。
#[tauri::command]
pub async fn trae_scan_and_decrypt(client_key: String) -> Result<Value, String> {
    let client = trae_discover::get_client(&client_key).ok_or("未知客户端")?;
    let db = trae_discover::database_path(client);
    if !db.exists() {
        return Err(format!("未找到数据库：{}", db.display()));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let all = std::time::Instant::now();
        let progress: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let t_scan = std::time::Instant::now();
        let scan = trae_memory_scan::scan_for_key(&client_key, &db, Some(&|m| {
            progress.lock().unwrap().push(m.to_string());
        }))
        .map_err(|e| {
            let p = progress.lock().unwrap().join("\n");
            format!("{e}\n{p}")
        })?;
        if !scan.found {
            return Err(format!("{}\n{}", scan.message, progress.lock().unwrap().join("\n")));
        }
        let key = scan.key.clone().unwrap_or_default();
        trae_memory_scan::save_key(&client_key, &key).map_err(|e| format!("保存密钥失败：{e}"))?;
        let scan_ms = t_scan.elapsed().as_millis() as u64;
        // ⚠️ 这里**不再无条件** `drop_snapshot_meta`。
        //
        // 以前每次点击都强制重解 900MB（快照明明还是最新的）。现在交给签名判断：
        // 签名（salt / 页数 / change counter / schema cookie / mtime）与实时库一致就直接复用；
        // 而「WAL 里更新的那几帧」由读取路径 `reader_plain_path` 在读取时合并，
        // **不会看到旧数据**。密钥换过 / 库被 checkpoint 改写 ⇒ 签名不同 ⇒ 自动整库重解。
        let t_dec = std::time::Instant::now();
        let out = trae_export::ensure_decrypted(&client_key, Some(&|m| {
            progress.lock().unwrap().push(m.to_string());
        }))
        .map_err(|e| {
            let p = progress.lock().unwrap().join("\n");
            format!("{e}\n{p}")
        })?;
        let decrypt_ms = t_dec.elapsed().as_millis() as u64;
        let reused = out.reused;
        let wal = std::path::PathBuf::from(format!("{}-wal", db.display()));
        Ok(json!({
            "scan": scan,
            "reused": reused,
            // 分段耗时：界面上报「总耗时」，不然只报解密一段（体感 20s、提示 4.4s）
            "timings": {
                "scanMs": scan_ms,
                "decryptMs": if reused { 0 } else { decrypt_ms },
                "totalMs": all.elapsed().as_millis() as u64,
            },
            "report": {
                "out_path": out.path,
                "pages": out.pages,
                "elapsed_ms": out.elapsed_ms,
                "hmac_ok": true,
                "tables": out.tables,
            },
            // 实时 WAL 里还有多少「待合并」的已提交帧（读取时会在副本上合并，不动快照）
            "mergedWALFrames": trae_delete::wal_pending_frames(&wal),
            "decryptedDb": trae_export::decrypted_db_path(&client_key).to_string_lossy().into_owned(),
            "progress": progress.into_inner().unwrap_or_default(),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 用已存密钥重新解密一次（跳过内存扫描，密钥过期会报 HMAC 失败）。
#[tauri::command]
pub async fn trae_decrypt_with_saved_key(client_key: String) -> Result<Value, String> {
    if trae_discover::get_client(&client_key).is_none() {
        return Err("未知客户端".into());
    }
    if trae_memory_scan::load_saved_key(&client_key).is_none() {
        return Err("没有已存密钥，请先运行「扫描密钥并解密」".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        // 强制重来：先丢元信息，再走统一的 ensure（含「解密期间实时库被改写就重试」）
        trae_export::drop_snapshot_meta(&client_key);
        let out = trae_export::ensure_decrypted(&client_key, None)?;
        Ok(json!({
            "report": {
                "pages": out.pages,
                "elapsed_ms": out.elapsed_ms,
                "hmac_ok": true,
                "tables": out.tables,
            },
            "reused": out.reused,
            "decryptedDb": out.path,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 解密库状态（是否已生成 + 表行数概览）。
///
/// 行数直接读快照的元信息缓存——早期版本在这里对 ~180 张表逐个 `count(*)`，
/// 每次刷新都要跑一遍，是「记录页加载慢」的两个元凶之一。
#[tauri::command]
pub async fn trae_decrypted_status(client_key: String) -> Value {
    let out = trae_export::decrypted_db_path(&client_key);
    let exists = out.exists();
    let meta = trae_export::read_snapshot_meta(&client_key);
    let current = match (&meta, exists) {
        (Some(m), true) => trae_export::snapshot_is_current(&client_key, &out, m),
        _ => false,
    };
    json!({
        "clientKey": client_key,
        "exists": exists,
        "path": out.to_string_lossy().into_owned(),
        "pages": meta.as_ref().map(|m| m.pages).unwrap_or(0),
        // 快照是否仍然等于「实时库的纯解密结果」：false 表示下次读取需要重新解密
        "current": current,
        "tables": meta.map(|m| m.tables).unwrap_or_default(),
    })
}

/// 确保解密快照可用且与实时库一致：一致则**零解密**直接复用。
#[tauri::command]
pub async fn trae_ensure_decrypted(client_key: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let out = trae_export::ensure_decrypted(&client_key, None)?;
        Ok(json!({
            "reused": out.reused,
            "path": out.path,
            "pages": out.pages,
            "elapsed_ms": out.elapsed_ms,
            "tables": out.tables,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 一键回收工作文件占用：清理三处旧备份（各留最新 1 批）+ 删除可再生的解密快照。
///
/// 备份每批 ≈ 一个整库大小（279 MB 量级），不清理会持续累积；解密快照在下次
/// 「刷新」时会按存盘密钥自动重建，删掉是安全的。
#[tauri::command]
pub async fn trae_cleanup_working_files() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || Ok(trae_export::cleanup_working_files()))
        .await
        .map_err(|e| e.to_string())?
}

/// Trae 会话列表（解密库，按最后活动倒序）。
#[tauri::command]
pub async fn trae_list_sessions(client_key: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // 会话记录页也是「在用这个客户端」，一并记账。
        client_usage::record_use(&client_key);
        Ok(json!({ "sessions": trae_export::list_sessions(&client_key)? }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 单会话详情（标题 / 轮数 / 完整对话，供记录页预览）。
#[tauri::command]
pub async fn trae_session_detail(client_key: String, session_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || trae_export::session_detail(&client_key, &session_id))
        .await
        .map_err(|e| e.to_string())?
}

/// 会话编辑器：读出该会话在全部相关表里的行（结构化 JSON，含列元数据）。**只读。**
#[tauri::command]
pub async fn trae_session_edit_doc(client_key: String, session_id: String) -> Result<Value, String> {
    off_main(move || trae_session_edit::edit_doc(&client_key, &session_id)).await?
}

/// 会话编辑器：可写入的目标账号清单（**只读**）。
///
/// 「跨账号写入」用它在弹窗里列出可选的归属账号；显示名走 `trae_vault::account_label`
/// （与账号页同一出口，别在前端自己截 uid 尾号）。
#[tauri::command]
pub async fn trae_session_edit_targets(client_key: String) -> Result<Value, String> {
    off_main(move || trae_session_edit::list_target_uids(&client_key)).await?
}

/// 会话编辑器：生成一批新的 24 位 hex id（T46「新增回合 / 复制消息」用）。
///
/// 纯计算（12 字节随机数 → hex），不碰库、不碰盘、微秒级 ⇒ 同步命令，
/// 已登记进 `SYNC_ALLOWLIST`。
///
/// ⚠️ 前端**不要**自己造 id：id 的生成规则只能有一处实现
/// （见 `trae_session_edit::new_ids` 的注释 —— 两套生成规则必然在某次调整里跑偏，
/// 而「id 形态不对」的症状是「写完客户端打不开」）。
#[tauri::command]
pub fn trae_session_edit_new_ids(count: usize) -> Result<Value, String> {
    Ok(trae_session_edit::new_ids(count))
}

/// 会话编辑器：分级校验（纯计算，不碰库、不写盘）。
///
/// ⚠️ 规则只有这一份实现 —— 前端只负责显示 `errors` / `warnings`，**绝不在前端重写一遍**
/// （同一套规则被两处独立推导，必然跑偏一次）。
#[tauri::command]
pub async fn trae_session_edit_validate(doc: Value) -> Result<Value, String> {
    off_main(move || trae_session_edit::validate(&doc)).await
}

/// 会话编辑器：写回。
///
/// ⚠️ 写库操作：结束客户端 → 备份 → 解密 → 合并 WAL → 改 → 增量回写 → 校验 → 重启，
/// 全程几十秒，**必须走 `spawn_blocking`**。
///
/// `mode`：`overwrite`（就地覆写，保持 session_id）/ `insert`（同库新增，自动重映射全部 id）。
///
/// `dirtyTables`：前端标出的「真的改过的表」，只重写这些表。省略 = 全部重写。
/// 这只是性能提示（只改标题就不必重写那 74 MB 的 `chat_message_task`），
/// **标错也不会写坏数据** —— 少标只是少写一张表，库里保持原样。
///
/// `targetUid`（S2）：把新增的会话写进**另一个账号**。只允许 `mode = "insert"`；
/// 传了它就必须是该客户端里已有工程的 uid（见 `trae_session_edit_targets`）。
/// 归属改写会同时覆盖 `chat_session.project_id` / `session_project.project_id` /
/// `context.last_real_project_id` 三处（漏一处 = 客户端删不掉 + 重启后复活）。
#[tauri::command]
pub async fn trae_session_edit_save(
    doc: Value,
    mode: String,
    dirty_tables: Option<Vec<String>>,
    target_uid: Option<String>,
) -> Result<Value, String> {
    let progress: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let buf = progress.clone();
    let out = off_main(move || {
        trae_session_edit::edit_save(
            &doc,
            &mode,
            dirty_tables.as_deref(),
            target_uid.as_deref(),
            Some(&|m| {
                buf.lock().unwrap().push(m.to_string());
            }),
        )
    })
    .await?;
    match out {
        Ok(v) => Ok(v),
        Err(e) => Err(format!("{e}\n\n--- 执行日志 ---\n{}", progress.lock().unwrap().join("\n"))),
    }
}

/// 会话编辑器：按需拉取一个**大字段**的完整内容（点「展开」时调）。
///
/// 大字段（单个 > 32 KB，例如 `chat_message_task.content` 单行 9.8 MB）在 `edit_doc` 里
/// 只回了 `$jsonRef` 引用 —— 一条会话实测能到 90 MB，全量塞给前端会把渲染冻死。
#[tauri::command]
pub async fn trae_session_edit_cell(
    client_key: String,
    session_id: String,
    table: String,
    column: String,
    pk: Value,
) -> Result<Value, String> {
    off_main(move || {
        trae_session_edit::edit_cell(&client_key, &session_id, &table, &column, &pk)
    })
    .await?
}

/// 导出单个会话为 MD 文件（导出目录内自动去重命名）。
#[tauri::command]
pub async fn trae_export_session(client_key: String, session_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(json!(trae_export::export_session(&client_key, &session_id)?))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 一键导出全部会话为 zip（可跨数据源）。
#[tauri::command]
pub async fn trae_export_all(sources: Vec<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(json!(trae_export::export_all(&sources)?))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 账号维度导入候选：全部本机账号（vault + 解密库 local + 当前登录态）。
/// 不排除所选会话的归属账号，改用 `is_source` 标记同客户端同归属的候选（前端禁用）。
#[tauri::command]
pub async fn trae_import_candidates(exclude: String, session_id: Option<String>) -> Value {
    // `session_owner_uid` 要开解密库（可能几百 MB），必须离开主线程。
    off_main(move || {
        let owner = session_id
            .as_deref()
            .and_then(|sid| trae_import::session_owner_uid(&exclude, sid));
        json!({
            "candidates": trae_import::list_account_candidates(&exclude, owner.as_deref()),
            "hints": trae_import::client_hints(),
        })
    })
    .await
    .unwrap_or_else(|e| json!({ "candidates": [], "hints": [], "error": e }))
}

/// 目标账号导入就绪状态探测（不写库）。
#[tauri::command]
pub async fn trae_import_inspect(client_key: String, account_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || trae_import::inspect_account(&client_key, &account_id))
        .await
        .map_err(|e| e.to_string())?
}

/// 跨账号导入会话：源账号（已解密）→ 目标账号本地库。
/// `uid` 为目标账号 uid；同客户端跨账号时为同库复制（新 id）。
/// 进度通过 `trae-import-progress` 事件推送。
#[tauri::command]
pub async fn trae_import_run(
    app: tauri::AppHandle,
    src: String,
    dst: String,
    uid: Option<String>,
    sessions: Vec<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let result = trae_import::import_sessions(&src, &dst, uid.as_deref(), &sessions, Some(&|m| {
            let _ = app.emit("trae-import-progress", json!({ "line": m }));
        }))?;
        Ok(json!(result))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 诊断：列出目标库里「`session_project.project_id` 与 `chat_session.project_id` 不一致」
/// 的会话。只读，不改任何数据。
#[tauri::command]
pub async fn trae_find_misaligned_projects(client_key: String) -> Result<Value, String> {
    off_main(move || {
        let rows = trae_import::find_misaligned_session_projects(&client_key)?;
        Ok(json!({
            "count": rows.len(),
            "sessions": rows.into_iter().map(|(sid, sp_pid, sess_pid)| json!({
                "session_id": sid,
                "session_project_id": sp_pid,
                "chat_session_project_id": sess_pid,
            })).collect::<Vec<_>>(),
        }))
    })
    .await?
}

/// 自愈：把历史副本的工程归属对齐到其真实账号的项目。
///
/// 背景：早前版本的同库复制漏改 `session_project.project_id`，导致副本挂在源账号的旧项目下，
/// 用户在 Trae 客户端里**删不掉**、**重启后记录复活**。此命令一次性修干净。
/// 重活（解密整库 + 增量加密回写），必须走 async + 阻塞线程池。
#[tauri::command]
pub async fn trae_heal_session_projects(
    app: tauri::AppHandle,
    client_key: String,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let result = trae_import::heal_session_projects(&client_key, Some(&|m| {
            let _ = app.emit("trae-import-progress", json!({ "line": m }));
        }))?;
        Ok(json!(result))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// WorkBuddy 会话来源列表（供勾选导入）。只读，不触碰 WorkBuddy 任何文件。
#[tauri::command]
pub async fn trae_workbuddy_list() -> Result<Value, String> {
    off_main(workbuddy_import::list_source_sessions).await?
}

/// WorkBuddy → Trae 转换预览：报告每个会话将转换出多少回合与工具步骤（只读，不写库）。
#[tauri::command]
pub async fn trae_workbuddy_preview(sessions: Vec<String>) -> Result<Value, String> {
    off_main(move || workbuddy_import::preview(&sessions)).await?
}

/// 把选中的 WorkBuddy 会话移植进目标 Trae 账号的本地库。
///
/// 会先退出目标客户端，解密目标库 → 合并 WAL 已提交帧 → 写入转换后的行 → 加密回写 →
/// 备份 + 原子替换 → 自检 → 重启客户端。进度通过 `trae-workbuddy-progress` 事件推送。
#[tauri::command]
pub async fn trae_workbuddy_import(
    app: tauri::AppHandle,
    dst: String,
    uid: String,
    sessions: Vec<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        workbuddy_import::import_sessions(&dst, &uid, &sessions, Some(&|m| {
            let _ = app.emit("trae-workbuddy-progress", json!({ "line": m }));
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Trae → WorkBuddy 导出（`workbuddy_import` 的反向）
// ---------------------------------------------------------------------------

/// WorkBuddy 侧现状：数据根、客户端是否在运行、可选目标账号（只读，不写任何文件）。
#[tauri::command]
pub async fn trae_wb_export_target() -> Result<Value, String> {
    off_main(|| Ok(workbuddy_export::target_info())).await?
}

/// 某个 Trae 客户端里可导出的会话列表（带工作目录，用于预览将落到哪个 WorkBuddy 工作区）。
///
/// 读的是解密快照；快照不存在或过期时先由前端调用 `trae_ensure_decrypted` 准备。
#[tauri::command]
pub async fn trae_wb_export_sessions(client_key: String) -> Result<Value, String> {
    off_main(move || workbuddy_export::list_source(&client_key)).await?
}

/// 导出预览：报告每个会话的回合数 / 工具步骤 / 将落到哪个 WorkBuddy 工作区（只读，不写文件）。
#[tauri::command]
pub async fn trae_wb_export_preview(client_key: String, sessions: Vec<String>) -> Result<Value, String> {
    off_main(move || workbuddy_export::preview(&client_key, &sessions)).await?
}

/// 把选中的 Trae 会话导出成 WorkBuddy 的明文会话（JSONL + `sessions` 表行）。
///
/// 会先退出 WorkBuddy 桌面版，写完再自动拉起。进度通过 `trae-wb-export-progress` 推送。
#[tauri::command]
pub async fn trae_wb_export_run(
    app: tauri::AppHandle,
    client_key: String,
    uid: String,
    sessions: Vec<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        workbuddy_export::export_sessions(&client_key, &sessions, &uid, Some(&|m| {
            let _ = app.emit("trae-wb-export-progress", json!({ "line": m }));
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// WorkBuddy 本机垃圾清理（先扫描、后受控清除）
// ---------------------------------------------------------------------------

/// 扫描本机 WorkBuddy 的可清理项（**只读**，不改动任何文件与数据库）。
///
/// - `force = false`（默认）：先读 10 分钟内的磁盘缓存，命中即毫秒级返回；
/// - `force = true`：跳过缓存重扫（前端「重新扫描」按钮）。
///
/// ⚠️ **几 GB 的递归统计，必须走后台线程**（见 [`off_main`]）。
/// 这条以前是同步命令，首页一挂载就触发，把主线程占满约 10 秒 ——
/// 表现就是「启动后整整 10 秒点不动界面」。
#[tauri::command]
pub async fn trae_wb_cleanup_scan(force: Option<bool>) -> Result<Value, String> {
    let force = force.unwrap_or(false);
    off_main(move || workbuddy_cleanup::cached(force)).await?
}

/// 清理选中的项。
///
/// `hard = false`（默认）把文件/目录移入工具回收站，可原样搬回；
/// `hard = true` 直接彻底删除。两种情况都会先退出 WorkBuddy、并整份备份数据库。
/// 进度通过 `trae-wb-cleanup-progress` 推送。
#[tauri::command]
pub async fn trae_wb_cleanup_purge(
    app: tauri::AppHandle,
    ids: Vec<String>,
    hard: bool,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        workbuddy_cleanup::purge(&ids, hard, Some(&|m| {
            let _ = app.emit("trae-wb-cleanup-progress", json!({ "line": m }));
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 清空工具回收站，彻底释放被清理文件占用的空间。
#[tauri::command]
pub async fn trae_wb_cleanup_empty_trash() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(workbuddy_cleanup::empty_trash)
        .await
        .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// 设置（`~/.twin-switch/settings.json`）
// ---------------------------------------------------------------------------

/// 设置的**增量补丁**：只写显式给了的字段，没给的一律保持原样。
///
/// ⚠️ 用 `Option` + `#[serde(default)]`，而不是「整个 `Settings`」：前端可能只想改一项，
/// 传整个结构就会把另一项用「前端自己以为的值」覆盖掉（两个界面同时开着就会互相踩）。
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    pub backup_retention: Option<settings::BackupRetention>,
    /// 写库前的整库备份开关（默认 `false` = 不备份）。
    pub backup_before_write: Option<bool>,
    /// 手动指定的客户端主程序路径（`clientKey → 绝对路径`）。
    ///
    /// ⚠️ **整表覆盖**：前端传的是「改完之后的全量映射」，删掉某个键就等于
    /// 「把这个客户端恢复成自动探测」。这样不会出现「想清空却清不掉」的半个状态。
    pub trae_exe: Option<std::collections::HashMap<String, String>>,
    /// 手动指定的 WorkBuddy 主程序路径（`None` = 不改；空串 = 恢复自动探测）。
    pub workbuddy_exe: Option<String>,
    /// 手动指定的 Qoder 主程序路径（`区域(cn|global) → 绝对路径`，**整表覆盖**）。
    pub qoder_exe: Option<std::collections::HashMap<String, String>>,
    /// 切号时是否把 WorkBuddy 的定时任务同步给新账号（`None` = 不改）。
    pub sync_workbuddy_automations: Option<bool>,
    /// 切号时是否把 WorkBuddy 的会话自动同步给所有账号（`None` = 不改）。
    pub auto_link_workbuddy_sessions: Option<bool>,
    /// **诊断日志**设置（`None` = 不改）。整块覆盖：开关 / 级别 / 保留天数 / 份数 / 单文件上限。
    pub log: Option<wb_switch_core::modules::diag_log::DiagLogConfig>,
}

/// 设置快照的**唯一出口** —— `settings_get` 与 `settings_set` 都返回它的形状。
///
/// ⚠️ 两个命令必须同形（前端拿任一返回值都能直接刷界面）：各拼一份迟早会出现
/// 「保存后界面少了一项」或「同一屏两个说法」。
fn settings_snapshot(s: &settings::Settings, path: &std::path::Path) -> Value {
    json!({
        "ok": true,
        "backupRetention": {
            "enabled": s.backup_retention.enabled,
            "keepDays": s.backup_retention.keep_days,
            "keepCount": s.backup_retention.keep_count,
            "maxTotalMb": s.backup_retention.max_total_mb,
        },
        "summary": s.backup_retention.summary(),
        "backupBeforeWrite": s.backup_before_write,
        // 切号是否同步定时任务（WorkBuddy 的 automations 按 owner_user_id 归属）。
        "syncWorkbuddyAutomations": s.sync_workbuddy_automations,
        // 切号是否把会话自动同步给所有账号（写会话库，默认关）。
        "autoLinkWorkbuddySessions": s.auto_link_workbuddy_sessions,
        "beforeWriteSummary": if s.backup_before_write {
            "开启 —— 每次写库前留一份备份（页级增量，只存变动页，通常几 MB）"
        } else {
            "已关闭（默认）—— 写库不留历史版本；库位替换是原子的，不会因此丢库"
        },
        "path": path.to_string_lossy(),
        // 手动指定的主程序路径（前端拿它渲染「已指定 / 自动探测」两种状态）。
        "traeExe": s.trae_exe,
        "workbuddyExe": s.workbuddy_exe,
        "qoderExe": s.qoder_exe,
        // 诊断日志设置（默认关）。
        "log": {
            "enabled": s.log.enabled,
            "level": s.log.level,
            "maxDays": s.log.max_days,
            "maxFiles": s.log.max_files,
            "maxMb": s.log.max_mb,
        },
        "logDir": wb_switch_core::modules::diag_log::log_root().to_string_lossy(),
        // 自动探测到的 WorkBuddy 主程序（界面在「没手动指定」时显示这一条）。
        // 与真正启动时用的是**同一个出口**，不会出现「显示 A、启动 B」。
        "workbuddyDetected": workbuddy_switch::resolve_exe()
            .map(|p| p.to_string_lossy().into_owned()),
    })
}

/// 读设置（整库备份保留策略 + 写库前是否备份）。
///
/// ⚠️ 走 `load_strict`：**「空」和「失败」绝不许长得一样**。
/// - 文件不存在 = 全新安装 ⇒ 返回默认值（`ok: true`）；
/// - 文件在但内容坏了 ⇒ **如实报错**，不让界面拿着默认值假装一切正常
///   （用户会以为自己的设置「被重置了」，实际是文件坏了）。
///
/// 返回里带 `summary`（给人看的一行摘要）与 `path`（出问题时用户能自己去看），
/// 两者都来自后端同一份结构 ⇒ 界面上的策略原文与真正执行判定的那份**同源**。
#[tauri::command]
pub async fn settings_get() -> Result<Value, String> {
    off_main(|| {
        let s = settings::load_strict()?;
        Ok(settings_snapshot(&s, &settings::settings_path()))
    })
    .await?
}

/// 写设置（原子替换：先写 `.tmp` 再 rename，写到一半断电不会留下半个 JSON）。
///
/// 只写传进来的字段（见 [`SettingsPatch`]）。落在合理区间由后端再夹一次（`sanitized`）——
/// 手输 999999999 天不会造成数据风险，但会让界面摘要没法看、`keep_days * 86_400_000`
/// 还会溢出 `i64`。
///
/// 返回与 [`settings_get`] **同形**，前端可直接用它刷界面（省掉一次读盘 ——
/// 也就不会出现「写成功了但界面显示的是旧值」这种两处不同源的情况）。
#[tauri::command]
pub async fn settings_set(patch: SettingsPatch) -> Result<Value, String> {
    off_main(move || {
        let mut next = settings::load();
        if let Some(r) = patch.backup_retention {
            next.backup_retention = r;
        }
        if let Some(l) = patch.log.clone() {
            next.log = l.sanitized();
        }
        if let Some(b) = patch.backup_before_write {
            next.backup_before_write = b;
        }
        if let Some(v) = patch.sync_workbuddy_automations {
            next.sync_workbuddy_automations = v;
        }
        if let Some(v) = patch.auto_link_workbuddy_sessions {
            next.auto_link_workbuddy_sessions = v;
        }
        if let Some(m) = patch.trae_exe {
            // ⚠️ 存之前先校验：一个不存在的路径存进去，等于把「找不到主程序」
            //    换成「找到但启动失败」，错误更难懂。空串视为「清除这一项」。
            let mut clean: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (k, v) in m {
                let v = v.trim().to_string();
                if v.is_empty() {
                    continue;
                }
                let p = std::path::Path::new(&v);
                if !p.is_file() {
                    return Err(format!("这个路径不是一个文件：{v}"));
                }
                if !v.to_ascii_lowercase().ends_with(".exe") {
                    return Err(format!("请选择客户端的主程序（.exe），当前是：{v}"));
                }
                clean.insert(k, v);
            }
            next.trae_exe = clean;
            // 装完客户端 / 改完路径 ⇒ 让注册表探测重算一次，
            // 否则「重新检测」会返回第一次的结果（等于没检测）。
            trae_discover::invalidate_discovery_cache();
        }
        if let Some(v) = patch.workbuddy_exe {
            let v = v.trim().to_string();
            if v.is_empty() {
                next.workbuddy_exe = String::new();
            } else {
                if !std::path::Path::new(&v).is_file() {
                    return Err(format!("WorkBuddy 的路径不是一个文件：{v}"));
                }
                if !v.to_ascii_lowercase().ends_with(".exe") {
                    return Err(format!("请选择 WorkBuddy 的主程序（.exe），当前是：{v}"));
                }
                next.workbuddy_exe = v;
            }
        }
        if let Some(m) = patch.qoder_exe {
            // ⚠️ 按区域校验程序名：Qoder 两个区域的 exe 名字不同
            //    （国内 `Qoder CN.exe` / 国际 `Qoder.exe`），跨区填进来会让
            //    「切换国内版」去启动国际版 —— 与 MEMORY 里「不许跨区兜底」同一条纪律。
            let mut clean: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (k, v) in m {
                let v = v.trim().to_string();
                if v.is_empty() {
                    continue;
                }
                let region = qoder_realm::QoderRegion::parse(&k)
                    .ok_or_else(|| format!("未知的 Qoder 区域：{k}（只支持 cn / global）"))?;
                let p = std::path::Path::new(&v);
                if !p.is_file() {
                    return Err(format!("这个路径不是一个文件：{v}"));
                }
                if !v.to_ascii_lowercase().ends_with(".exe") {
                    return Err(format!("请选择 Qoder 的主程序（.exe），当前是：{v}"));
                }
                let file = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                let looks_cn = file.contains("cn");
                if region == qoder_realm::QoderRegion::Cn && !looks_cn {
                    return Err(format!(
                        "国内版的主程序叫 `Qoder CN.exe`，选中的是 {file} —— 看起来是另一个版本"
                    ));
                }
                if region == qoder_realm::QoderRegion::Global && looks_cn {
                    return Err(format!(
                        "国际版的主程序叫 `Qoder.exe`，选中的是 {file} —— 看起来是国内版"
                    ));
                }
                clean.insert(region.as_str().to_string(), v);
            }
            next.qoder_exe = clean;
        }
        let path = settings::save(&next)?;
        // 回读一次：把「实际落盘的值」返回给界面，而不是「用户刚提交的值」
        let saved = settings::load_strict()?;
        Ok(settings_snapshot(&saved, &path))
    })
    .await?
}

/// 取「正在运行的客户端」的主程序路径（设置页的「用运行中的客户端」按钮）。
///
/// 为什么需要它：Trae 允许装到任意目录，自动探测再全也可能漏；而**它正在跑**
/// 这件事本身就是最可靠的线索（`QueryFullProcessImageNameW` 直接给绝对路径）。
/// 用户点一下就把路径填对，比让他去「右键快捷方式 → 打开文件所在位置」强。
///
/// ⚠️ 没在跑时**如实报错**，不许回一个猜的路径 —— 填错路径的后果是「切换时启动了
/// 别的东西」，比「没找到」难查得多。
#[tauri::command]
pub async fn trae_exe_from_running(client_key: String) -> Result<Value, String> {
    off_main(move || {
        let client =
            trae_discover::get_client(&client_key).ok_or_else(|| format!("未知客户端：{client_key}"))?;
        match trae_discover::exe_from_running_lenient(client_key.as_str()) {
            Some(p) => Ok(json!({
                "ok": true,
                "clientKey": client_key,
                "label": client.label,
                "path": p.to_string_lossy(),
            })),
            None => Err(format!(
                "{} 现在没有在运行。请先打开它，或直接粘贴它的主程序路径（右键桌面快捷方式 → 打开文件所在位置）。",
                client.label
            )),
        }
    })
    .await?
}

/// 取「正在运行的 WorkBuddy」的主程序路径（设置页的「用运行中的」）。
///
/// 与 [`trae_exe_from_running`] 同一个道理：装在自定义目录时，**它正在跑**
/// 就是最可靠的线索。没在跑就如实报错，不许回一个猜的路径。
#[tauri::command]
pub async fn workbuddy_exe_from_running() -> Result<Value, String> {
    off_main(|| {
        let names: Vec<String> = workbuddy_auth::WINDOWS_IMAGE_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect();
        for p in wb_switch_core::modules::process_list::find_by_names_ex(&names) {
            let Some(exe) = wb_switch_core::modules::process_list::exe_path_of_pid(p.pid) else {
                continue;
            };
            if exe.is_file() {
                return Ok(json!({ "ok": true, "path": exe.to_string_lossy() }));
            }
        }
        Err(
            "WorkBuddy 现在没有在运行。请先打开它，或直接粘贴它的主程序路径（右键桌面快捷方式 → 打开文件所在位置）。"
                .to_string(),
        )
    })
    .await?
}

/// 取「正在运行的 Qoder」的主程序路径（设置页的「用运行中的」）。
///
/// ⚠️ 只认**本区域**的进程名：国内 `Qoder CN.exe`、国际 `Qoder.exe`。
/// 两个版本可以同时开着，拿另一个版本的路径填进来会让切换去启动错的客户端。
#[tauri::command]
pub async fn qoder_exe_from_running(region: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域（只支持 cn / global）")?;
    off_main(move || {
        let want: Vec<String> = match r {
            qoder_realm::QoderRegion::Cn => {
                vec!["Qoder CN.exe".to_string(), "Qoder CN Launcher.exe".to_string()]
            }
            qoder_realm::QoderRegion::Global => {
                vec!["Qoder.exe".to_string(), "Qoder Launcher.exe".to_string()]
            }
        };
        for p in wb_switch_core::modules::process_list::find_by_names_ex(&want) {
            let Some(exe) = wb_switch_core::modules::process_list::exe_path_of_pid(p.pid) else {
                continue;
            };
            if exe.is_file() {
                // 启动器（`*Launcher.exe`）不是主程序，跳过 —— 填它只会多开一个壳。
                let name = exe
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if name.contains("launcher") {
                    continue;
                }
                return Ok(json!({ "ok": true, "region": r.as_str(), "path": exe.to_string_lossy() }));
            }
        }
        Err(format!(
            "{} 现在没有在运行。请先打开它，或直接粘贴主程序路径（默认在 `%LOCALAPPDATA%\\Programs\\{}` 下）。",
            r.display_name(),
            if r == qoder_realm::QoderRegion::Cn { "Qoder CN" } else { "Qoder" }
        ))
    })
    .await?
}

// ---------------------------------------------------------------------------
// Trae 本机清理（会话 / 工具残留 / 客户端残留 / 回收站）
// ---------------------------------------------------------------------------

/// 扫描本机 Trae 相关的可清理项（**只读**，不改动任何文件与数据库）。
///
/// 四类：Trae 库里的会话、本工具目录的残留（备份/快照/中间产物）、Trae 客户端的
/// 可再生缓存、两处回收站。
///
/// - `force = false`（默认）：先读 10 分钟内的磁盘缓存，命中即毫秒级返回；
/// - `force = true`：跳过缓存重扫（前端「重新扫描」按钮）。
///
/// ⚠️ **几 GB 的递归统计，必须走后台线程**（见 [`off_main`]）——这条和
/// [`trae_wb_cleanup_scan`] 是「启动后 10 秒无法操作」的两个直接原因。
#[tauri::command]
pub async fn trae_cleanup_scan(force: Option<bool>) -> Result<Value, String> {
    let force = force.unwrap_or(false);
    off_main(move || trae_cleanup::cached(force)).await?
}

/// 执行清理。
///
/// - `hard = false`（默认）：文件/目录移入工具回收站（可原样搬回），会话仍走整库备份；
/// - `hard = true`：直接彻底删除。
///
/// 选中会话时走**批量**链路（一次解密、一次增量回写、一份整库备份），本地删完再尽力
/// 同步删除云端任务列表记录（云端失败只提示，不影响本地结果）。
/// 进度通过 `trae-cleanup-progress` 推送。
#[tauri::command]
pub async fn trae_cleanup_purge(
    app: tauri::AppHandle,
    ids: Vec<String>,
    hard: bool,
) -> Result<Value, String> {
    let log = move |app: &tauri::AppHandle, m: &str| {
        let _ = app.emit("trae-cleanup-progress", json!({ "line": m }));
    };
    let app_for_task = app.clone();
    let ids_for_task = ids.clone();
    let mut v = tauri::async_runtime::spawn_blocking(move || {
        trae_cleanup::purge(&ids_for_task, hard, Some(&|m| {
            let _ = app_for_task.emit("trae-cleanup-progress", json!({ "line": m }));
        }))
    })
    .await
    .map_err(|e| e.to_string())??;

    // 云端同步（逐条尽力；本地删除已完成，云端失败不影响结果）
    let targets = v
        .get("session_targets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut cloud: Vec<Value> = Vec::new();
    for t in targets {
        let client_key = t.get("client_key").and_then(Value::as_str).unwrap_or("").to_string();
        let session_id = t.get("session_id").and_then(Value::as_str).unwrap_or("").to_string();
        let uid = t.get("owner_uid").and_then(Value::as_str).unwrap_or("").to_string();
        if uid.is_empty() {
            cloud.push(json!({ "session_id": session_id, "attempted": false, "reason": "no_owner" }));
            continue;
        }
        if !trae_remote::has_cloud_credential(&client_key, &uid) {
            cloud.push(json!({ "session_id": session_id, "attempted": false, "reason": "no_credential" }));
            continue;
        }
        log(&app, &format!("同步删除云端记录 {session_id}…"));
        match trae_remote::delete_cloud_session(&client_key, &uid, &session_id).await {
            Ok(r) => cloud.push(json!({
                "session_id": session_id,
                "attempted": true,
                "ok": true,
                "http": r.get("http").cloned().unwrap_or(Value::Null),
            })),
            Err(e) => {
                log(&app, &format!("云端删除失败（已忽略，本地不受影响）：{e}"));
                cloud.push(json!({ "session_id": session_id, "attempted": true, "ok": false, "error": e }));
            }
        }
    }
    if !cloud.is_empty() {
        v["cloud"] = json!(cloud);
    }
    Ok(v)
}

/// 清空本页回收站（`~/.twin-switch/trash`），彻底释放磁盘。不可恢复。
#[tauri::command]
pub async fn trae_cleanup_empty_trash() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(trae_cleanup::empty_trash)
        .await
        .map_err(|e| e.to_string())?
}

/// 备份批次概览（只读）：这一批是增量还是整份、省了多少、能不能还原、不能还原的原因。
///
/// ⚠️ 界面上的「可以还原」必须由它说了算，**不许前端自己看目录里有没有文件** ——
/// 真正的判据在 [`trae_backup::inspect`] 里（比如「增量与整份同时存在」就不敢还原）。
#[tauri::command]
pub async fn trae_backup_inspect(backup_dir: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = PathBuf::from(err_unless_backup_dir(&backup_dir)?);
        Ok(trae_backup::inspect(&dir))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 把这一批备份还原到它所属客户端的实时库。
///
/// ⚠️⚠️ 三条硬约束（都在这里兜住，不指望前端记得）：
///  1. **只认本工具自己的备份根目录** —— 传进来的路径必须落在 `~/.twin-switch` 下的某个
///     备份目录里；否则一个手滑的绝对路径就能把任意文件复制到库位上。
///  2. **还原前先停客户端** —— 不停就等于「还原完立刻被运行中的 Trae 覆盖回去」，
///     而且换库位时它还在读写，会出乱子。停下之后**还原完自动重启**（和删除 / 导入一致）。
///  3. **还原前先把当前库另存** —— 还原错了还能再还原回来（详见 [`trae_backup::restore_with_park`]）。
#[tauri::command]
pub async fn trae_backup_restore(
    client_key: String,
    backup_dir: String,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = PathBuf::from(err_unless_backup_dir(&backup_dir)?);
        let client = trae_discover::get_client(&client_key)
            .ok_or_else(|| format!("未知的客户端：{client_key}"))?;
        let live = trae_discover::database_path(client);

        // 概览先过一遍：不可还原的批次直接如实拒绝（把 `inspect` 给的理由原样带回）
        let info = trae_backup::inspect(&dir);
        if info.get("canRestore").and_then(Value::as_bool) != Some(true) {
            let why = info
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("这份备份不可还原");
            return Err(format!("这份备份不能还原：{why}"));
        }

        // ① 停客户端（⚠️ 走 `stop_for_maintenance`：只碰它**自己的**程序）
        let was_running = matches!(
            trae_switch::stop_for_maintenance(client, 20_000)?,
            trae_switch::MaintenanceStop::Stopped(_)
        );

        // ② 另存当前库 + 还原
        let park_root = trae_export::backup_dir();
        let out = trae_backup::restore_with_park(&dir, &live, &park_root, &client_key);

        // ③ 无论成败都尝试把客户端起回来（失败时也要起 —— 用户还得用它）
        let relaunched = trae_switch::launch(&client_key).is_ok();

        let mut v = out?;
        if let Some(o) = v.as_object_mut() {
            o.insert("clientKey".into(), json!(client_key));
            o.insert("wasRunning".into(), json!(was_running));
            o.insert("relaunched".into(), json!(relaunched));
            o.insert("live".into(), json!(live.to_string_lossy()));
        }
        Ok(v)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 校验「这个路径确实是本工具的备份目录」并返回规范化的绝对路径。
///
/// ⚠️ 两边都过 `lexically_normalized()` 再比 —— `Path::starts_with` 是**逐组件**比较、
/// 不做词法规范化，`.../backup/../../trae/decrypted` 会被它当成「以 backup 开头」（T64 的坑）。
fn err_unless_backup_dir(raw: &str) -> Result<String, String> {
    let p = PathBuf::from(raw.trim());
    if !p.is_absolute() {
        return Err(format!("备份目录必须是绝对路径：{raw}"));
    }
    let p = trae_cleanup::lexically_normalized(&p);
    if !p.is_dir() {
        return Err(format!("备份目录不存在：{}", p.display()));
    }
    let roots = [
        trae_export::backup_dir(),
        trae_export::import_backup_dir(),
        trae_export::wb_import_backup_dir(),
        config::store_dir().join("workbuddy_export_backup"),
    ];
    let inside = roots.iter().any(|r| {
        let r = trae_cleanup::lexically_normalized(r);
        p != r && p.starts_with(&r)
    });
    if !inside {
        return Err(format!(
            "只允许还原本工具自己的备份目录下的条目，拒绝：{}",
            p.display()
        ));
    }
    Ok(p.to_string_lossy().into_owned())
}

/// 删除预览：标题 / 各表行数 / 磁盘文件（只读，不删任何东西）。
#[tauri::command]
pub async fn trae_delete_info(client_key: String, session_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || trae_delete::delete_info(&client_key, &session_id))
        .await
        .map_err(|e| e.to_string())?
}

/// 彻底删除会话：整库备份 → 写实时加密库删行 → 同步删解密库 → 文件移入回收站，
/// 之后再尝试同步删除云端任务列表记录（云端失败仅提示，不影响本地删除结果）。
#[tauri::command]
pub async fn trae_delete_session(client_key: String, session_id: String) -> Result<Value, String> {
    // 归属账号必须先于本地删除解析（本地删除会同步清掉解密库里的会话行）
    let owner_uid = trae_import::session_owner_uid(&client_key, &session_id);
    let progress: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let local = tauri::async_runtime::spawn_blocking({
        let client_key = client_key.clone();
        let session_id = session_id.clone();
        let progress = progress.clone();
        move || {
            trae_delete::delete_session(&client_key, &session_id, Some(&|m| {
                progress.lock().unwrap().push(m.to_string());
            }))
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let mut v = match local {
        Ok(v) => v,
        Err(e) => return Err(format!("{e}\n{}", progress.lock().unwrap().join("\n"))),
    };

    // 云端同步删除（失败仅提示：本地删除已完成，不受影响）
    let cloud = match owner_uid.as_deref() {
        Some(uid) if trae_remote::has_cloud_credential(&client_key, uid) => {
            progress.lock().unwrap().push(format!(
                "同步删除云端任务列表记录（uid …{}）…",
                &uid[uid.len().saturating_sub(6)..]
            ));
            match trae_remote::delete_cloud_session(&client_key, uid, &session_id).await {
                Ok(r) => {
                    progress.lock().unwrap().push("云端记录已删除".into());
                    json!({
                        "attempted": true,
                        "ok": true,
                        "http": r.get("http").cloned().unwrap_or(Value::Null),
                    })
                }
                Err(e) => {
                    progress.lock().unwrap().push(format!(
                        "云端删除失败（已忽略，本地删除不受影响）：{e}"
                    ));
                    json!({ "attempted": true, "ok": false, "error": e })
                }
            }
        }
        Some(uid) => {
            progress.lock().unwrap().push(format!(
                "账号（uid …{}）无云端凭证，仅删除本地记录",
                &uid[uid.len().saturating_sub(6)..]
            ));
            json!({ "attempted": false, "reason": "no_credential" })
        }
        None => {
            progress.lock().unwrap().push("会话归属账号未知，仅删除本地记录".into());
            json!({ "attempted": false, "reason": "no_owner" })
        }
    };
    v["cloud"] = cloud;
    v["progress"] = json!(progress.lock().unwrap().clone());
    Ok(v)
}

/// **批量**彻底删除会话：与 [`trae_delete_session`] 同一条链路，但整批只走一趟 ——
/// 一次解密、一次增量回写、**一份**整库备份。
///
/// 为什么要单独开一条命令而不是前端循环调单条：`delete_session` 每次都做一遍
/// 「结束客户端 → 整库备份 → 解密 → 改 → 加密回写 → 重启」，批量删 20 条就是
/// 20 份整库备份 + 20 次全库加解密，备份目录会按整库大小线性膨胀。
///
/// 云端任务列表的删除仍**逐条尽力尝试**：单条失败只记进结果提示，不影响其余条目，
/// 也不影响已经完成的本地删除。
#[tauri::command]
pub async fn trae_delete_sessions(
    client_key: String,
    session_ids: Vec<String>,
) -> Result<Value, String> {
    // 去重但保持用户勾选顺序（`delete_sessions` 内部也会去重，这里是为了让
    // 下面这轮「查归属」的次数与用户实际勾选条数一致，不做无用功）。
    let mut uniq: Vec<String> = Vec::new();
    for sid in session_ids {
        if !sid.is_empty() && !uniq.contains(&sid) {
            uniq.push(sid);
        }
    }
    if uniq.is_empty() {
        return Err("没有选中任何会话".into());
    }

    // ⚠️ 归属账号必须在本地删除**之前**解析完：本地删除会同步清掉解密库里的会话行，
    //    删完再查就查不到归属，云端任务列表里那些记录会永久残留。
    let owners: Vec<(String, Option<String>)> = uniq
        .iter()
        .map(|sid| (sid.clone(), trae_import::session_owner_uid(&client_key, sid)))
        .collect();

    let progress: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let local = tauri::async_runtime::spawn_blocking({
        let client_key = client_key.clone();
        let ids = uniq.clone();
        let progress = progress.clone();
        move || {
            trae_delete::delete_sessions(&client_key, &ids, Some(&|m| {
                progress.lock().unwrap().push(m.to_string());
            }))
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let mut v = match local {
        Ok(v) => v,
        Err(e) => return Err(format!("{e}\n{}", progress.lock().unwrap().join("\n"))),
    };

    // 云端同步删除：逐条尽力而为。单条失败只记录，不让整批失败 ——
    // 本地删除已经落盘完成，这里失败最多是任务列表多留一条记录。
    let mut cloud_ok = 0usize;
    let mut cloud_failed: Vec<Value> = Vec::new();
    let mut cloud_skipped: Vec<Value> = Vec::new();
    for (sid, owner) in &owners {
        let tail = |uid: &str| uid[uid.len().saturating_sub(6)..].to_string();
        match owner.as_deref() {
            Some(uid) if trae_remote::has_cloud_credential(&client_key, uid) => {
                match trae_remote::delete_cloud_session(&client_key, uid, sid).await {
                    Ok(_) => {
                        cloud_ok += 1;
                        progress
                            .lock()
                            .unwrap()
                            .push(format!("云端记录已删除（uid …{}）", tail(uid)));
                    }
                    Err(e) => {
                        progress
                            .lock()
                            .unwrap()
                            .push(format!("云端删除失败（已忽略）：{e}"));
                        cloud_failed.push(json!({ "sessionId": sid, "uid": uid, "error": e }));
                    }
                }
            }
            Some(uid) => {
                cloud_skipped
                    .push(json!({ "sessionId": sid, "uid": uid, "reason": "no_credential" }));
            }
            None => {
                cloud_skipped.push(json!({ "sessionId": sid, "reason": "no_owner" }));
            }
        }
    }

    v["cloud"] = json!({
        "deleted": cloud_ok,
        "failed": cloud_failed,
        "skipped": cloud_skipped,
    });
    v["progress"] = json!(progress.lock().unwrap().clone());
    Ok(v)
}

/// 交接记忆预览：解密库自动生成条目 → 组装文档，报告落点，不落盘。
#[tauri::command(rename_all = "camelCase")]
pub async fn trae_handoff_preview(
    client_key: String,
    project_path: Option<String>,
    session_ids: Option<Vec<String>>,
    next_steps: Option<Vec<String>>,
    key_files: Option<Vec<String>>,
    note: Option<String>,
) -> Result<Value, String> {
    if trae_discover::get_client(&client_key).is_none() {
        return Err(format!("未知客户端：{client_key}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        trae_handoff::handoff_preview(
            &client_key,
            project_path.as_deref(),
            session_ids,
            next_steps.unwrap_or_default(),
            key_files.unwrap_or_default(),
            note.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 写入交接记忆：项目工作目录 + 项目规则 + Trae 记忆库 topics 追加 + 工具目录归档。
#[tauri::command(rename_all = "camelCase")]
pub async fn trae_handoff_write(
    client_key: String,
    project_path: Option<String>,
    session_ids: Option<Vec<String>>,
    next_steps: Option<Vec<String>>,
    key_files: Option<Vec<String>>,
    note: Option<String>,
) -> Result<Value, String> {
    if trae_discover::get_client(&client_key).is_none() {
        return Err(format!("未知客户端：{client_key}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        trae_handoff::handoff_write(
            &client_key,
            project_path.as_deref(),
            session_ids,
            next_steps.unwrap_or_default(),
            key_files.unwrap_or_default(),
            note.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Trae 网页（OAuth）登录
// ---------------------------------------------------------------------------

/// 起 Trae 网页登录回环监听，返回授权页 URL（不自动打开浏览器，交给调用方决定）。
#[tauri::command]
pub fn trae_oauth_start(client_key: String, name: Option<String>) -> Result<Value, String> {
    trae_oauth::start_loopback(&client_key, name.as_deref())
}

/// Trae 网页登录状态（前端约 1.5s 轮询一次；收到回调时在此驱动完成 token 交换）。
#[tauri::command]
pub async fn trae_oauth_status() -> Value {
    if let Some(url) = trae_oauth::take_callback_url() {
        return trae_oauth::complete_login_from_callback(&url, None, None).await;
    }
    trae_oauth::oauth_status()
}

/// 停止 Trae 网页登录监听（未启动时幂等）。同时清掉落盘的登录会话。
#[tauri::command]
pub fn trae_oauth_stop() -> Value {
    trae_oauth::stop_loopback();
    json!({ "ok": true })
}

/// 查询是否有「已落盘但本进程没在监听」的登录会话（服务重启 / 弹层关掉后）。
#[tauri::command]
pub fn trae_oauth_pending() -> Value {
    json!({ "pending": trae_oauth::pending_login() })
}

/// 手动粘贴授权页回调 URL 完成登录（不依赖回环监听；凭落盘的 verifier 交换）。
#[tauri::command]
pub async fn trae_oauth_manual(client_key: String, name: Option<String>, callback_url: String) -> Value {
    trae_oauth::complete_manual(&client_key, name.as_deref(), &callback_url).await
}

/// 打开 Trae 授权页：默认系统浏览器 / 私密窗口（指定浏览器 key）。
#[tauri::command]
pub fn trae_oauth_open_url(
    url: String,
    private: Option<bool>,
    browser: Option<String>,
) -> Result<Value, String> {
    if private.unwrap_or(false) {
        let info = trae_oauth::open_private(&url, browser.as_deref())?;
        return Ok(json!({ "ok": true, "private": true, "browser": info.label, "browserKey": info.key }));
    }
    trae_oauth::open_in_browser(&url)?;
    Ok(json!({ "ok": true, "private": false }))
}

/// 本机可用的浏览器列表（私密窗口打开用）。要扫注册表与安装目录，走后台。
#[tauri::command]
pub async fn trae_oauth_browsers() -> Value {
    off_main(|| json!({ "browsers": trae_oauth::detect_browsers() }))
        .await
        .unwrap_or_else(|e| json!({ "browsers": [], "error": e }))
}

/// 导入本地登录态：解密指定客户端 storage.json 的授权条目，落库为凭证账号。
#[tauri::command]
pub async fn trae_import_local_login(client_key: String) -> Result<Value, String> {
    off_main(move || trae_oauth::import_local_login(&client_key)).await?
}

/// 一键导入：扫描全部已安装客户端，收集每个客户端的本地登录态。
#[tauri::command]
pub async fn trae_import_all_local_logins() -> Value {
    off_main(trae_oauth::import_all_local_logins)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "imported": 0, "error": e }))
}

// ---------------------------------------------------------------------------
// WorkBuddy 账号管理（国内版）
// ---------------------------------------------------------------------------

/// 账号列表 + 当前登录态。**只读 WorkBuddy 侧数据**，不做任何写操作。
///
/// ⚠️ 这里**必须**把失败原样抛给前端，不能降级成「空账号列表」。
/// 2026-10-08 真事：账号解析里一个 UTF-8 字符边界的 panic 被
/// `unwrap_or_else(|e| json!({ "accounts": [], "error": e }))` 吞掉，
/// 前端又不读 `error` 字段 ⇒ 用户看到的是「账号库还是空的」，以为**账号丢了**。
/// 空的账号列表和「读不出来」是两件完全不同的事，绝不能长得一样。
#[tauri::command]
pub async fn workbuddy_account_list() -> Result<Value, String> {
    off_main(workbuddy_vault::list)
        .await
        .map_err(|e| format!("读取账号库失败：{e}"))
}

/// 切换前的门禁检查（只读）：进程状态、登录态、目标账号凭据。
///
/// 要枚举本机进程（WMI），单次就可能上秒 —— 走后台（见 [`off_main`]）。
#[tauri::command]
pub async fn workbuddy_switch_precheck(account_id: String) -> Value {
    off_main(move || workbuddy_switch::precheck(&account_id))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// 切换到指定账号（会结束 WorkBuddy 进程；失败自动回滚登录态）。
#[tauri::command]
pub async fn workbuddy_switch_to(account_id: String, relaunch: Option<bool>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        workbuddy_switch::switch_to(&account_id, relaunch.unwrap_or(true))
    })
    .await
    .map_err(|e| format!("切换任务失败: {e}"))?
}

/// 回滚到上一次切换前的登录态。
#[tauri::command]
pub async fn workbuddy_rollback() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(workbuddy_switch::rollback)
        .await
        .map_err(|e| format!("回滚任务失败: {e}"))?
}

/// **手动添加账号**：直接粘 `access_token`（可选 `refresh_token`），不再走一次授权。
///
/// 走这条路径**不消耗 re 额度** —— 适合手上已有可用 token、只想登记进来用于切换的场景。
/// 校验（三段式 / sub / 未过期 / 两个 token 同源）全在
/// [`workbuddy_vault::add_manual_token`] 里做，任何一条不符都直接报错，不保存半成品。
#[tauri::command]
pub async fn workbuddy_account_add_token(
    access_token: String,
    refresh_token: Option<String>,
    domain: Option<String>,
    name: Option<String>,
) -> Result<Value, String> {
    off_main(move || {
        let saved = workbuddy_vault::add_manual_token(
            &access_token,
            refresh_token.as_deref(),
            domain.as_deref(),
            name.as_deref(),
        )?;
        // ⚠️ 凭据不回传前端：只给身份与到期时间。
        Ok(json!({
            "ok": true,
            "accountId": saved.get("id").cloned().unwrap_or(Value::Null),
            "uid": saved.get("uid").cloned().unwrap_or(Value::Null),
            "nickname": saved.get("nickname").cloned().unwrap_or(Value::Null),
            "domain": saved.get("domain").cloned().unwrap_or(Value::Null),
            "expiresAt": saved.get("expiresAt").cloned().unwrap_or(Value::Null),
            "refreshExpiresAt": saved.get("refreshExpiresAt").cloned().unwrap_or(Value::Null),
        }))
    })
    .await?
}

/// 导入本机 WorkBuddy 当前登录态到账号库。
#[tauri::command]
pub async fn workbuddy_import_local() -> Result<Value, String> {
    off_main(workbuddy_vault::import_local_login).await?
}

/// 删除账号（只删本工具账号库里的记录，不动 WorkBuddy 登录态）。
#[tauri::command]
pub async fn workbuddy_remove_account(account_id: String) -> Result<Value, String> {
    off_main(move || {
        workbuddy_vault::delete_account(&account_id)?;
        Ok(json!({ "ok": true }))
    })
    .await?
}

/// 账号改名（写 `display_name`，不动账号原始昵称）。
#[tauri::command]
pub async fn workbuddy_rename_account(account_id: String, name: String) -> Result<Value, String> {
    off_main(move || {
        let acc = workbuddy_vault::rename(&account_id, &name)?;
        Ok(json!({ "ok": true, "account": {
            "id": acc.get("id"),
            "name": workbuddy_vault::display_name(&acc),
        }}))
    })
    .await?
}

/// 导出账号包（只含有凭据的账号）。
#[tauri::command]
pub async fn workbuddy_export_accounts() -> Value {
    off_main(workbuddy_vault::export_accounts)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// 导出**账号包**（写盘版）：与 Trae / Qoder 同一个交互 —— 先落到磁盘，
/// 再把路径返回给前端去资源管理器里定位。
///
/// ⚠️ 以前是前端拼 Blob 让浏览器下载：落点由 WebView 决定，用户拿完不知道文件在哪，
/// 也没有路径可以「打开给我看」。
#[tauri::command]
pub async fn workbuddy_export_pack() -> Result<Value, String> {
    off_main(workbuddy_vault::export_pack).await?
}

/// 导入账号包预览（只读，不写库）。
#[tauri::command]
pub async fn workbuddy_preview_import(payload: Value) -> Result<Value, String> {
    off_main(move || workbuddy_vault::preview_import(&payload)).await?
}

/// 导入账号包。`mode`：`merge`（默认，按身份合并）/ `replace`（整库替换）。
#[tauri::command]
pub async fn workbuddy_import_accounts(payload: Value, mode: Option<String>) -> Result<Value, String> {
    off_main(move || workbuddy_vault::import_accounts(&payload, mode.as_deref())).await?
}

/// 一次性搬家：把参考工具账号库里**有明文凭据**的账号并进本工具账号库。
///
/// ⚠️ 这是全仓**唯一**会去读那份外部账号库的动作，用户点一次即可；之后积分、切换、
/// 会话一律只看本工具账号库。返回 `{created, updated, skipped, total}`，
/// 失败（文件不存在 / 不是合法 JSON）**原样上抛**，别降级成「搬了 0 个」——
/// 那会把「读不到」伪装成「没有可搬的」（T40 的教训）。
#[tauri::command]
pub async fn workbuddy_import_reference_accounts() -> Result<Value, String> {
    off_main(workbuddy_vault::import_reference_accounts).await?
}

/// 发起 OAuth 扫码登录，返回授权页 URL 与 loginId。
#[tauri::command]
pub async fn workbuddy_oauth_start() -> Result<Value, String> {
    workbuddy_oauth::oauth_start().await
}

/// 轮询一次登录结果。
#[tauri::command]
pub async fn workbuddy_oauth_poll(login_id: String) -> Value {
    workbuddy_oauth::oauth_poll(&login_id).await
}

/// 取消一次登录请求。
#[tauri::command]
pub fn workbuddy_oauth_stop(login_id: String) {
    workbuddy_oauth::oauth_stop(&login_id)
}

/// 用默认浏览器打开 URL（OAuth 授权页）。
#[tauri::command]
pub fn workbuddy_open_url(url: String) -> Result<Value, String> {
    trae_oauth::open_in_browser(&url)?;
    Ok(json!({ "ok": true }))
}


// ---------------------------------------------------------------------------
// WorkBuddy 会话记录 / 复制 / 关联（国内版）
// ---------------------------------------------------------------------------

/// 本机全部会话，按账号 uid 分组（只读）。
#[tauri::command]
pub async fn workbuddy_session_list_by_account() -> Value {
    off_main(workbuddy_sessions::list_by_account)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e, "accounts": [] }))
}

/// 某账号名下未删除的会话（只读）。
#[tauri::command]
pub async fn workbuddy_session_list(uid: String) -> Value {
    off_main(move || workbuddy_sessions::list_for_account(&uid))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e, "sessions": [] }))
}

/// 会话详情：库行 + 正文回合（只读）。
#[tauri::command]
pub async fn workbuddy_session_detail(uid: String, cid: String) -> Value {
    off_main(move || workbuddy_sessions::detail(&uid, &cid))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// 导出单个会话为 Markdown。
#[tauri::command]
pub async fn workbuddy_session_export_md(uid: String, cid: String) -> Result<Value, String> {
    off_main(move || workbuddy_sessions::export_markdown(&uid, &cid)).await?
}

/// 批量删除会话（软删 + 正文进回收站；会先结束 WorkBuddy 进程，失败可回滚）。
#[tauri::command]
pub async fn workbuddy_session_delete(uid: String, ids: Vec<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || workbuddy_sessions::delete_sessions(&uid, &ids))
        .await
        .map_err(|e| format!("删除任务失败: {e}"))?
}

/// 复制预检（只读）：归属、正文、客户端占用、是否已关联。
#[tauri::command]
pub async fn workbuddy_session_copy_preview(
    source_uid: String,
    target_uid: String,
    ids: Vec<String>,
) -> Value {
    off_main(move || workbuddy_sessions::copy_preview(&source_uid, &target_uid, &ids))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// 把勾选的会话从源账号复制到目标账号（源账号数据一行不改；云端映射库不写）。
#[tauri::command]
pub async fn workbuddy_session_copy(
    source_uid: String,
    target_uid: String,
    ids: Vec<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        workbuddy_sessions::copy_sessions(&source_uid, &target_uid, &ids)
    })
    .await
    .map_err(|e| format!("复制任务失败: {e}"))?
}

/// WorkBuddy：**多目标**复制（一次复制给多个账号）。
///
/// 每个目标独立成败：一个目标失败不中断其余，结果按目标逐条返回
/// （`okCount` 是成功的目标数，`count` 是这些目标里复制成功的会话合计）。
#[tauri::command]
pub async fn workbuddy_session_copy_multi(
    source_uid: String,
    target_uids: Vec<String>,
    ids: Vec<String>,
    close_client: Option<bool>,
) -> Result<Value, String> {
    // `close_client = true`：客户端在跑就由本工具退出它，复制完再开回来。
    // 用户在界面的确认框里点过「确定」才传 true（默认 false = 老行为，拒绝写入）。
    let close = close_client.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        workbuddy_sessions::copy_sessions_multi(&source_uid, &target_uids, &ids, close)
    })
    .await
    .map_err(|e| format!("复制任务失败: {e}"))?
}

/// WorkBuddy：一键把本机所有会话互通到所有账号（补齐缺失副本 + 登记关联组）。
///
/// ⚠️ 这是**重操作**：实测本机一次可能复制几十个副本、几个 GB。界面必须先给预览数字
/// 并让用户确认，再调这里（`link_all_preview` 与它同源）。
#[tauri::command]
pub async fn workbuddy_session_link_all() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        wb_switch_core::modules::workbuddy_sessions::link_all(
            wb_switch_core::modules::workbuddy_sessions::LinkOptions {
                require_closed: true,
                copy_missing: true,
                since: 0,
            },
        )
    })
    .await
    .map_err(|e| format!("一键互通任务失败: {e}"))?
}

/// WorkBuddy：只把「同一段对话已有副本」登记成关联组，**不复制任何文件**（0 字节）。
///
/// 适合「我只要关联关系，不想让磁盘翻倍」的场景；不需要备份、也不碰会话库。
#[tauri::command]
pub async fn workbuddy_session_link_existing() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        wb_switch_core::modules::workbuddy_sessions::link_all(
            wb_switch_core::modules::workbuddy_sessions::LinkOptions {
                require_closed: false,
                copy_missing: false,
                since: 0,
            },
        )
    })
    .await
    .map_err(|e| format!("登记关联失败: {e}"))?
}

/// WorkBuddy：一键互通的只读预览（要复制几个、多少字节、客户端是否在运行）。
///
/// 会扫一遍会话列表并计算内容摘要，属重活 ⇒ `off_main`。
#[tauri::command]
pub async fn workbuddy_session_link_all_preview() -> Value {
    off_main(|| wb_switch_core::modules::workbuddy_sessions::link_all_preview())
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// 源账号与目标账号之间已建立的会话副本关联（只读）。
#[tauri::command]
pub async fn workbuddy_session_links_preview(source_uid: String, target_uid: String) -> Value {
    off_main(move || workbuddy_sessions::links_preview(&source_uid, &target_uid))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// WorkBuddy：关联会话列表（按「同一段对话」聚合各账号副本，只读）。
///
/// 要扫一遍会话库并计算内容摘要，属重活 ⇒ `off_main`（Tauri 同步命令跑主线程，会冻窗口）。
#[tauri::command]
pub async fn workbuddy_session_groups() -> Value {
    off_main(|| {
        wb_switch_core::modules::workbuddy_session_groups::list()
            .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// WorkBuddy：单个关联组的成员明细（与列表同源）。
#[tauri::command]
pub async fn workbuddy_session_group_detail(group_key: String) -> Value {
    off_main(move || {
        wb_switch_core::modules::workbuddy_session_groups::detail(&group_key)
            .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// WorkBuddy：**强制比对**一个关联组（忽略体积/成员数上限，逐行读完再判定）。
///
/// 默认路径（`workbuddy_session_group_detail`）在「体积超限 / 成员过多」时**不做比对**，
/// 只回「无法确认」。用户明确要求「支持手动确定后执行判定分析」——
/// 这个命令就是那条慢路径：会真的读完整正文，因此**要几秒到几十秒**。
#[tauri::command]
pub async fn workbuddy_session_group_detail_forced(group_key: String) -> Value {
    off_main(move || {
        wb_switch_core::modules::workbuddy_session_groups::detail_forced(&group_key)
            .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// WorkBuddy：**取消关联**（把指定账号从「同一段对话」里移除）。
///
/// ⚠️ 只删关联记录，**不动任何会话数据**：不写 sqlite、不碰 jsonl、不备份会话库。
/// 所以这里也不需要关客户端 —— 用户点一下立刻生效，不用等 WorkBuddy 重启。
#[tauri::command]
pub async fn workbuddy_session_unlink_members(
    group_key: String,
    uids: Vec<String>,
) -> Result<Value, String> {
    off_main(move || {
        wb_switch_core::modules::workbuddy_session_groups::unlink_members(&group_key, &uids)
    })
    .await
    .unwrap_or_else(|e| Err(e))
}

/// WorkBuddy：还没纳入关联组的会话（供「新增关联会话」选择，只读）。
#[tauri::command]
pub async fn workbuddy_session_group_candidates() -> Value {
    off_main(|| {
        wb_switch_core::modules::workbuddy_session_groups::candidates()
            .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// WorkBuddy：以某个副本为准，覆盖组内其它账号的副本（写正文 + 改库行时间）。
///
/// 会备份正文与整个会话库；要求客户端已关闭。重活 ⇒ `spawn_blocking`。
#[tauri::command]
pub async fn workbuddy_session_group_unify(
    group_key: String,
    source_session_id: String,
    close_client: Option<bool>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        wb_switch_core::modules::workbuddy_session_groups::unify(
            &group_key,
            &source_session_id,
            close_client.unwrap_or(false),
        )
    })
    .await
    .map_err(|e| format!("统一会话内容失败: {e}"))?
}

/// WorkBuddy：给关联组里缺的账号补一份副本。
#[tauri::command]
pub async fn workbuddy_session_group_link_account(
    group_key: String,
    target_uid: String,
    close_client: Option<bool>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        wb_switch_core::modules::workbuddy_session_groups::link_account(
            &group_key,
            &target_uid,
            close_client.unwrap_or(false),
        )
    })
    .await
    .map_err(|e| format!("关联新账号失败: {e}"))?
}

/// WorkBuddy：把一个未关联的会话复制给目标账号（复制后自动进同一个关联组）。
#[tauri::command]
pub async fn workbuddy_session_group_link_session(
    session_id: String,
    target_uid: String,
    close_client: Option<bool>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        wb_switch_core::modules::workbuddy_session_groups::link_session(
            &session_id,
            &target_uid,
            close_client.unwrap_or(false),
        )
    })
    .await
    .map_err(|e| format!("新增关联会话失败: {e}"))?
}

/// Trae：同客户端下两个账号之间的会话副本关联（只读）。
///
/// 会去读一次已解密库判定副本存活，属重活 ⇒ 必须 `off_main`。
#[tauri::command]
pub async fn trae_session_links_preview(
    client_key: String,
    source_uid: String,
    target_uid: String,
) -> Value {
    off_main(move || {
        wb_switch_core::modules::trae_session_links::links_preview(&client_key, &source_uid, &target_uid)
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// Trae：删除一个关联组（只删本工具的关联记录，不动任何会话数据）。
#[tauri::command]
pub async fn trae_session_unlink(group_id: String) -> Result<Value, String> {
    off_main(move || wb_switch_core::modules::trae_session_links::unlink_group(&group_id)).await?
}

/// Trae：「同步差异」——把关联组里较新一端的内容，就地覆盖到较旧一端
/// （保持会话 id 不变，客户端里的位置与归属不动）。
///
/// 这是**写库**操作：内部会结束客户端 → 整份备份 → 增量回写 → 校验 → 重启，
/// 全程几十秒，必须走 `spawn_blocking`（与 `trae_import_run` 同款处理），
/// 否则会占住 Tauri 主线程把窗口冻住。
#[tauri::command]
pub async fn trae_session_sync_group(
    app: tauri::AppHandle,
    client_key: String,
    group_id: String,
    direction: String,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        wb_switch_core::modules::trae_import::sync_group(
            &client_key,
            &group_id,
            &direction,
            Some(&|m| {
                let _ = app.emit("trae-import-progress", json!({ "line": m }));
            }),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 批量同步的一项（前端传 `[{ groupId, direction }]`）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncGroupRequest {
    pub group_id: String,
    pub direction: String,
}

/// Trae：**批量**「同步差异」——一次写库周期完成多个关联组。
///
/// 为什么要单独有它：每个写库周期都要「退客户端 → 备份 → 回写 → 重启」。
/// 切号后一次性同步 N 组时，若逐组调 `trae_session_sync_group`，客户端要被重启 N 次、
/// 备份 N 份，用户干等几十秒 ×N。批量版只重启一次。
///
/// ⚠️ 同样是写库操作，必须走 `spawn_blocking`。
#[tauri::command]
pub async fn trae_session_sync_groups(
    app: tauri::AppHandle,
    client_key: String,
    items: Vec<SyncGroupRequest>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let reqs: Vec<(String, String)> = items
            .into_iter()
            .map(|i| (i.group_id, i.direction))
            .collect();
        wb_switch_core::modules::trae_import::sync_groups(&client_key, &reqs, Some(&|m| {
            let _ = app.emit("trae-import-progress", json!({ "line": m }));
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Trae：切号后的**分叉探测**（只读）——某账号参与的关联组里，哪些两端已经不一致。
///
/// 返回 `groups[]`，每项带 `partnerUid` / `selfRole` / `suggestedDirection`，
/// 前端据此弹「要不要同步」。`count` 为 0 表示没有需要提醒的内容。
#[tauri::command]
pub async fn trae_session_links_diverged(
    client_key: String,
    uid: String,
) -> Result<Value, String> {
    off_main(move || {
        wb_switch_core::modules::trae_session_links::diverged_groups(&client_key, &uid)
    })
    .await
}

/// 删除一个关联组（只删本工具的关联记录，不动任何会话数据）。
#[tauri::command]
pub async fn workbuddy_session_unlink(group_id: String) -> Result<Value, String> {
    off_main(move || workbuddy_sessions::unlink_group(&group_id)).await?
}

/// 数据根下的云端映射库（只读展示：本工具从不写入）。
#[tauri::command]
pub async fn workbuddy_session_edge_sync_dbs() -> Value {
    off_main(|| {
        json!({
            "items": workbuddy_sessions::edge_sync_databases(),
            "note": "云端映射库仅列出，本工具从不写入；写入会导致 edge-sync 跳过上传、云端缺会话",
        })
    })
    .await
    .unwrap_or_else(|e| json!({ "items": [], "error": e }))
}

// ---------------------------------------------------------------------------
// WorkBuddy 会话编辑器（T46 · S4）
//
// 与 Trae 侧（`trae_session_edit_*`）同构，但数据模型完全不同：WorkBuddy 的正文是
// 「一个 JSONL 文件里的若干行」，所以读分两步（回合索引 / 按区间取完整行），
// 写走「操作数组」（只回传改动过的行）—— 真机单个正文 19 MB，最大的 136 MB，
// 整份 doc 过 IPC 会把前端冻死。
// ---------------------------------------------------------------------------

/// 会话编辑器（WorkBuddy）：读出元数据 + 回合索引（**只读**、轻量）。
#[tauri::command]
pub async fn workbuddy_session_edit_doc(uid: String, cid: String) -> Result<Value, String> {
    off_main(move || workbuddy_session_edit::edit_doc(&uid, &cid)).await?
}

/// 会话编辑器（WorkBuddy）：按行区间取**完整行**（含该行解析后的 JSON）。
///
/// 一次最多 400 行（后端也会再拦一次）—— 前端一次只取当前回合的范围。
#[tauri::command]
pub async fn workbuddy_session_edit_lines(
    uid: String,
    cid: String,
    from: usize,
    to: usize,
) -> Result<Value, String> {
    off_main(move || workbuddy_session_edit::edit_lines(&uid, &cid, from, to)).await?
}

/// 会话编辑器（WorkBuddy）：可写入的目标账号清单（**只读**）。
#[tauri::command]
pub async fn workbuddy_session_edit_targets() -> Value {
    off_main(workbuddy_session_edit::list_target_uids)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e, "accounts": [] }))
}

/// 会话编辑器（WorkBuddy）：分级校验（**不读文件、不开库**，毫秒级）。
///
/// ⚠️ 规则只有这一份实现（`workbuddy_session_edit::validate_submit`）——前端只显示
/// `errors` / `warnings`，**绝不在前端重写一遍**。`summary` 由 `edit_doc` 带回，
/// 有了它才能在不读整份正文的前提下算「改完之后还剩几条用户提问」（空会话硬拦）。
///
/// 这一层是**快速反馈**；写入前 `edit_save` 还会跑一次全量校验，那时的错误会原样回给前端。
#[tauri::command]
pub async fn workbuddy_session_edit_validate(
    ops: Value,
    row_patch: Value,
    columns: Value,
    summary: Value,
    session_id: String,
) -> Result<Value, String> {
    off_main(move || {
        workbuddy_session_edit::validate_submit(&ops, &row_patch, &columns, &summary, &session_id)
    })
    .await
}

/// 会话编辑器（WorkBuddy）：写回。
///
/// ⚠️ 写盘 + 开库 + 可能结束/重启客户端，几十秒 ⇒ 必须 `spawn_blocking`。
///
/// `mode`：`overwrite`（就地改这条会话）/ `insert`（新 UUID 造一条新会话，源会话一行不动）。
/// `target_uid`（S5 跨账号）：只在 `insert` 时可用；只接受本机已有会话的账号 uid。
/// `ops`：只含**改动过的那几行**（replace / delete / insert），未改动的行逐字节原样保留。
///
/// 返回：任何 `error` 都会带 `ok=false` 原样返回（不是 Err），好让前端把「写入前的全量校验」
/// 明细展示出来 —— 见 `workbuddy_session_edit::edit_save` 的注释。
#[tauri::command]
pub async fn workbuddy_session_edit_save(
    uid: String,
    cid: String,
    mode: String,
    ops: Value,
    row_patch: Value,
    target_uid: Option<String>,
) -> Result<Value, String> {
    let progress: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let buf = progress.clone();
    let out = off_main(move || {
        workbuddy_session_edit::edit_save(
            &uid,
            &cid,
            &mode,
            &ops,
            &row_patch,
            target_uid.as_deref(),
            Some(&|m| {
                buf.lock().unwrap().push(m.to_string());
            }),
        )
    })
    .await?;
    match out {
        Ok(v) => Ok(v),
        Err(e) => Err(format!(
            "{e}\n\n--- 执行日志 ---\n{}",
            progress.lock().unwrap().join("\n")
        )),
    }
}

// ---------------------------------------------------------------------------
// 首页总览（只读）
// ---------------------------------------------------------------------------

/// 本机总览：Trae / WorkBuddy 两侧的客户端状态、账号、会话数与缓存积分。
///
/// 纯同步统计——不触发解密、不联网、不写任何客户端数据。
/// 算完会**立刻落盘到 `~/.twin-switch/cache/overview.json`**，下次启动即可热启。
/// 需要遍历磁盘的「可回收空间」由前端另调两侧的 `*_cleanup_scan` 并行拉取。
#[tauri::command]
pub async fn app_overview_snapshot() -> Value {
    tauri::async_runtime::spawn_blocking(app_overview::snapshot)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": format!("总览任务失败: {e}") }))
}

/// 只读上次落盘的总览缓存（**毫秒级，不重算**）；没有缓存时 `empty = true`。
///
/// 启动时前端先调它把界面渲染出来，再后台调 `app_overview_snapshot` 刷新。
#[tauri::command]
pub async fn app_overview_cached() -> Value {
    // 读 40 KB 的 JSON，本身很快；走后台是为了**不排在主线程队列里**
    // —— 万一有别的重活正在主线程上跑，它会连带被卡住（正是之前 10 秒卡顿的次生现象）。
    off_main(app_overview::cached)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "empty": true, "error": e }))
}

/// 把前端并行扫出的可回收空间并进总览缓存，下次首屏连这块也是热的。
#[tauri::command]
pub async fn app_overview_save_reclaim(trae_bytes: Option<u64>, wb_bytes: Option<u64>) -> Value {
    off_main(move || app_overview::save_reclaim(trae_bytes.unwrap_or(0), wb_bytes.unwrap_or(0)))
        .await
        .unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// WorkBuddy 积分 / 积分包（国内版）
// ---------------------------------------------------------------------------

/// 可查询积分的账号清单（合并本工具账号库 + 只读参考工具账号库）。**只读**。
#[tauri::command]
pub async fn workbuddy_credits_accounts() -> Value {
    off_main(workbuddy_credits::accounts)
        .await
        .unwrap_or_else(|e| json!({ "accounts": [], "error": e }))
}

/// 只读缓存（不发请求）；用于首页首屏。
#[tauri::command]
pub async fn workbuddy_credits_cached() -> Value {
    off_main(workbuddy_credits::cached)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "empty": true, "error": e }))
}

/// 查询全部账号的积分。`force = false` 命中 5 分钟缓存直接返回。
///
/// 直接在当前异步运行时上跑：积分链路本身就是 async（reqwest），
/// 唯一的同步部分只是几次小文件读写，不值得再切一次线程池。
#[tauri::command]
pub async fn workbuddy_credits_query(force: Option<bool>) -> Value {
    workbuddy_credits::query_all(force.unwrap_or(false)).await
}

/// 查询单个账号的积分（不走缓存，也不写缓存）。
#[tauri::command]
pub async fn workbuddy_credits_one(account_id: String) -> Value {
    workbuddy_credits::query_one(&account_id).await
}

// ---------------------------------------------------------------------------
// 诊断日志（默认关闭；见 `modules::diag_log`）
// ---------------------------------------------------------------------------

/// 日志现状：开关、级别、目录、占用、每天的文件数与大小（设置页直接展示）。
#[tauri::command]
pub async fn diag_log_status() -> Value {
    // 会扫 logs 目录（几十个文件以内），按项目硬规则离开主线程。
    off_main(wb_switch_core::modules::diag_log::status)
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// 最近的若干行日志（给设置页「看一眼」，不用去开文件夹）。
#[tauri::command]
pub async fn diag_log_tail(limit: Option<usize>) -> Value {
    let n = limit.unwrap_or(200).min(2000);
    off_main(move || {
        json!({ "ok": true, "lines": wb_switch_core::modules::diag_log::tail(n) })
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "lines": [], "error": e }))
}

/// 立即清空全部日志（**直接删，不进回收站** —— 这是本工具自己的诊断文件）。
#[tauri::command]
pub async fn diag_log_clear() -> Value {
    off_main(|| match wb_switch_core::modules::diag_log::clear() {
        Ok(bytes) => json!({ "ok": true, "freedBytes": bytes }),
        Err(e) => json!({ "ok": false, "error": format!("清理失败：{e}") }),
    })
    .await
    .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
}

/// 在文件管理器里打开日志目录。
///
/// ⚠️ 目录不存在时**先建出来**再打开 —— 否则「打开了但什么都没有」会让人以为
/// 「日志功能没生效」，而其实是「还没写过日志」。
#[tauri::command]
pub fn diag_log_open() -> Value {
    let dir = wb_switch_core::modules::diag_log::log_root();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return json!({ "ok": false, "error": format!("创建日志目录失败：{e}") });
    }
    json!({ "ok": true, "dir": dir.to_string_lossy() })
}

// ---------------------------------------------------------------------------
// 自动更新（对接 GitHub Releases）
// ---------------------------------------------------------------------------

/// 当前更新状态快照（同步、只读一个进程内互斥体，微秒级）。
///
/// 前端挂载时先拉一次：订阅事件只能收到**之后**的推送，
/// 「检查在挂载前就已完成」这种时序要靠它兜底。
#[tauri::command]
pub fn update_state() -> crate::update_service::UpdateSnapshot {
    crate::update_service::snapshot()
}

/// 检查更新。`force = true`（默认）绕过 6 小时缓存 —— 界面上手动点「检查更新」用它；
/// 后台周期检查传 `false`，多数轮次读内存就返回，不发网络请求。
#[tauri::command]
pub async fn update_check(app: tauri::AppHandle, force: Option<bool>) -> Value {
    crate::update_service::check(&app, None, force.unwrap_or(true)).await
}

/// 下载更新包：**立即返回**，进度走 `update-state` 事件；重复调用由互斥拒绝。
#[tauri::command]
pub async fn update_download(app: tauri::AppHandle) -> Result<(), String> {
    crate::update_service::start_download(&app)
}

/// 安装已下载的更新包并重启应用（无包时返回可读错误，不 panic）。
#[tauri::command]
pub async fn update_restart(app: tauri::AppHandle) -> Result<(), String> {
    crate::update_service::restart(&app).await
}

// ---------------------------------------------------------------------------
// 回归护栏：同步命令一律不许干重活
// ---------------------------------------------------------------------------

#[cfg(test)]
mod main_thread_guard {
    /// 允许留在**同步**（= 主线程）上的命令白名单。
    ///
    /// 只放「微秒级」的：纯路径计算、开关一个本地监听、拉起一个外部进程。
    /// 判断标准很简单 —— **会不会递归读目录树 / 开 SQLite / 枚举进程**。
    ///
    /// 背景：Tauri 的同步命令跑在主线程上（Windows 上同时是 WebView2 的消息循环），
    /// 主线程被占多久，窗口就冻多久，连已经回来的其它 IPC 回包也派发不出去。
    /// 历史事故：`trae_cleanup_scan` + `trae_wb_cleanup_scan` 两条同步命令被首页
    /// 并行触发，实测合计 **11.1 秒**（5455 + 5679 ms），表现为「启动后约 10 秒
    /// 点不动界面」，且磁盘缓存明明读到了也画不出来（回包排在它们后面）。
    const SYNC_ALLOWLIST: &[&str] = &[
        "relaunch_app",        // 重启进程，本来就在主线程发起
        "get_error_log_path",  // 纯路径计算
        "reveal_error_log",    // 拉起资源管理器后立即返回
        "reveal_path",         // 同上
        "trae_export_dir",     // 纯路径计算
        "trae_session_edit_new_ids", // 纯随机数 → hex，微秒级
        "trae_saved_key",      // 读一个几十字节的保存键
        "update_state",        // 只读一个进程内互斥体，微秒级
        "trae_oauth_start",    // 绑一个回环端口
        "trae_oauth_stop",     // 关监听
        "trae_oauth_pending",  // 读一个落盘的小 JSON
        "trae_oauth_open_url", // 拉起浏览器
        "workbuddy_oauth_stop",
        "workbuddy_open_url", // 拉起浏览器
        // 读 `config::store_dir()` 的**进程内缓存** + 一个静态提示字符串。
        // 真正的路径解析（含一次性的旧目录搬迁）已经在 `lib.rs::run()` 里、
        // 任何命令可能被调用之前跑完了，所以这里不会碰磁盘。
        "store_dir_info",
    ];

    /// 扫自己的源码：任何被 `#[tauri::command]` 标注的**同步** `pub fn`
    /// 必须登记在白名单里。
    ///
    /// 新增同步命令时会直接失败 —— 这是故意的，逼你停下来想一遍
    /// 「它会不会卡住主线程」。
    #[test]
    fn heavy_commands_must_not_be_sync() {
        let src = include_str!("commands.rs");
        let lines: Vec<&str> = src.lines().collect();
        let mut offenders: Vec<String> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            if line.trim() != "#[tauri::command]" {
                continue;
            }
            // 属性后面紧跟函数签名（允许中间有空行）
            let Some(sig) = lines[i + 1..].iter().find(|l| !l.trim().is_empty()) else {
                continue;
            };
            let sig = sig.trim();
            if let Some(rest) = sig.strip_prefix("pub fn ") {
                let name = rest.split(|c| c == '(' || c == '<').next().unwrap_or("").trim();
                if !SYNC_ALLOWLIST.contains(&name) {
                    offenders.push(name.to_string());
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "这些命令是同步的（会跑在主线程上），但没登记在 SYNC_ALLOWLIST 里：{offenders:?}\n\
             只做微秒级的事 → 加进白名单；否则改成 `pub async fn` + `off_main(...)`。"
        );
    }

    /// 白名单别留垃圾：登记了却已经不是同步命令的条目要删掉，
    /// 否则白名单会慢慢变成一张没人敢动的废纸。
    #[test]
    fn allowlist_has_no_stale_entries() {
        let src = include_str!("commands.rs");
        for name in SYNC_ALLOWLIST {
            let pat = format!("pub fn {name}(");
            assert!(
                src.contains(&pat),
                "白名单里的 `{name}` 已经不再是同步命令了，请从 SYNC_ALLOWLIST 移除"
            );
        }
    }
}


// ---------------------------------------------------------------------------
// 积分共享（本地额度网关，127.0.0.1:57892）
// ---------------------------------------------------------------------------

/// 网关运行状态（含账号池规模）。
#[tauri::command]
pub async fn gateway_status() -> Result<Value, String> {
    Ok(gateway_server::status())
}

/// 账号池：三产品摊平，最紧迫的排前面。
#[tauri::command]
pub async fn gateway_pool() -> Result<Value, String> {
    Ok(json!(gateway_pool::snapshot()))
}

/// 选号预览：只算不发，不消耗额度。
#[tauri::command]
pub async fn gateway_preview(model: String) -> Result<Value, String> {
    // 预览要和真正发请求走**同一套名字归一化**：否则界面里填显示名（`DeepSeek-Flash`）
    // 预览报「不认识这个模型」，而实际发出去又能通 —— 两套判据等于骗人。
    let canonical = gateway_models::canonical_model(&model);
    gateway_pool::preview(&canonical).map(|s| json!(s))
}

/// 模型显示名表：`{ 作用域: { 代号: 界面显示名 } }`。
///
/// 界面要用它把 `dfmodel` 显示成 `DeepSeek-Flash`（Qoder 客户端里就是这个名字），
/// 否则用户对着两个列表根本对不上号（2026-10-24 用户反馈）。
#[tauri::command]
pub async fn gateway_model_labels() -> Result<Value, String> {
    Ok(json!(gateway_models::label_map()))
}

#[tauri::command]
pub async fn gateway_keys_list() -> Result<Value, String> {
    gateway_keys::list()
}

/// 新建 Key。**明文只在这里返回一次**。
#[tauri::command]
pub async fn gateway_key_create(name: String, scopes: Vec<String>) -> Result<Value, String> {
    let (plaintext, key) = gateway_keys::create(&name, scopes)?;
    Ok(json!({ "plaintext": plaintext, "key": key }))
}

#[tauri::command]
pub async fn gateway_key_revoke(id: String) -> Result<Value, String> {
    gateway_keys::revoke(&id).map(|k| json!(k))
}

#[tauri::command]
pub async fn gateway_key_restore(id: String) -> Result<Value, String> {
    gateway_keys::restore(&id).map(|k| json!(k))
}

#[tauri::command]
pub async fn gateway_key_delete(id: String) -> Result<(), String> {
    gateway_keys::delete(&id)
}

/// 模型目录（`models.json`）。空对象表示还没登记。
#[tauri::command]
pub async fn gateway_models_get() -> Result<Value, String> {
    read_store_json("models.json")
}

#[tauri::command]
pub async fn gateway_models_set(value: Value) -> Result<(), String> {
    write_store_json("models.json", &value)
}

/// 上游地址表（`upstreams.json`）。没登记的产品会 501，不会假装成功。
#[tauri::command]
pub async fn gateway_upstreams_get() -> Result<Value, String> {
    read_store_json("upstreams.json")
}

#[tauri::command]
pub async fn gateway_upstreams_set(value: Value) -> Result<(), String> {
    write_store_json("upstreams.json", &value)
}

fn read_store_json(name: &str) -> Result<Value, String> {
    let path = config::store_dir().join(name);
    match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{} 格式损坏：{e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("读取 {} 失败：{e}", path.display())),
    }
}

fn write_store_json(name: &str, value: &Value) -> Result<(), String> {
    let path = config::store_dir().join(name);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let body = serde_json::to_vec_pretty(value).map_err(|e| format!("序列化失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落盘失败：{e}"))
}


/// 自动识别：从本机客户端里扫出可用模型（并附带每家的来源说明）。
#[tauri::command]
pub async fn gateway_models_discover() -> Result<Value, String> {
    // 识别完**立即落盘**：新电脑上 models.json 不存在时，纯内存结果会让
    // 选号/预览继续报「模型目录为空」。
    let (models, reports) = gateway_models::discover_and_save().await;
    Ok(json!({ "models": models, "reports": reports }))
}

/// 已勾选的模型（`models.json`）。
#[tauri::command]
pub async fn gateway_models_selected() -> Result<Value, String> {
    Ok(json!(gateway_models::load_selected()))
}

/// 保存勾选结果（`{ 作用域: [模型…] }`）。
#[tauri::command]
pub async fn gateway_models_save(map: Value) -> Result<(), String> {
    let parsed: std::collections::BTreeMap<String, Vec<String>> =
        serde_json::from_value(map).map_err(|e| format!("模型清单格式不对：{e}"))?;
    gateway_models::save_selected(&parsed)
}


// ---------------------------------------------------------------------------
// Qoder 账号（国内 / 国际两套账号库）
// ---------------------------------------------------------------------------

/// 两个区域的静态信息（端口无关，只给界面用）。
#[tauri::command]
pub async fn qoder_regions() -> Result<Value, String> {
    Ok(json!([
        {
            "region": "cn",
            "label": qoder_realm::QoderRegion::Cn.display_name(),
            "domain": qoder_realm::realm_spec(qoder_realm::QoderRegion::Cn).domain,
            "cli_dir": qoder_realm::realm_spec(qoder_realm::QoderRegion::Cn).cli_home_dir,
        },
        {
            "region": "global",
            "label": qoder_realm::QoderRegion::Global.display_name(),
            "domain": qoder_realm::realm_spec(qoder_realm::QoderRegion::Global).domain,
            "cli_dir": qoder_realm::realm_spec(qoder_realm::QoderRegion::Global).cli_home_dir,
        },
    ]))
}

/// 列出账号（默认两个区域都列）。
///
/// 额外补两个**只读**字段（都来自客户端自己写的东西，不解密就不猜）：
/// - `isCurrent`：桌面端登录态里的 `user.id` 与本条 uid 相同（解不开时整列为 false）；
/// - `appStatus`：客户端明文回显（名字 / 头像 / 版本），用来给「账号名」兜底。
#[tauri::command]
pub async fn qoder_accounts_list(region: Option<String>) -> Result<Value, String> {
    let list = match region.as_deref().and_then(qoder_realm::QoderRegion::parse) {
        Some(r) => qoder_accounts::list(r)?,
        None => qoder_accounts::list_all()?,
    };
    let cn_cur = qoder_auth_dat::current_desktop_uid(qoder_realm::QoderRegion::Cn);
    let gl_cur = qoder_auth_dat::current_desktop_uid(qoder_realm::QoderRegion::Global);
    let cn_st = qoder_auth_dat::app_status(qoder_realm::QoderRegion::Cn);
    let gl_st = qoder_auth_dat::app_status(qoder_realm::QoderRegion::Global);

    let mut out: Vec<Value> = Vec::with_capacity(list.len());
    for a in list {
        let (cur, st) = match a.region {
            qoder_realm::QoderRegion::Cn => (&cn_cur, &cn_st),
            qoder_realm::QoderRegion::Global => (&gl_cur, &gl_st),
        };
        let mut v = serde_json::to_value(&a).map_err(|e| e.to_string())?;
        v["isCurrent"] = json!(cur.as_deref() == Some(a.uid.as_str()));
        v["appStatus"] = st.clone().unwrap_or(Value::Null);
        out.push(v);
    }
    Ok(json!(out))
}

/// 新增账号（PAT 登录：uid + token 手填）。
#[tauri::command]
pub async fn qoder_account_add(
    region: String,
    uid: String,
    name: String,
    token: String,
    email: Option<String>,
) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region)
        .ok_or_else(|| format!("未知区域 {region:?}（只支持 cn / global）"))?;
    if uid.trim().is_empty() {
        return Err("uid 不能为空".into());
    }
    let mut acc = qoder_accounts::QoderAccount::new(r, uid.trim(), name.trim());
    acc.email = email.filter(|e| !e.trim().is_empty());
    acc.access_token = Some(token.trim().to_string());
    acc.source = qoder_accounts::SOURCE_PAT.to_string();
    let saved = qoder_accounts::add(acc)?;
    Ok(json!(saved))
}

#[tauri::command]
pub async fn qoder_account_remove(region: String, id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_accounts::remove(r, &id).map(|a| json!(a))
}

#[tauri::command]
pub async fn qoder_account_restore(region: String, id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_accounts::restore(r, &id).map(|a| json!(a))
}

/// 给某个账号换一份 PAT。
#[tauri::command]
pub async fn qoder_account_set_token(region: String, id: String, token: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    if token.trim().is_empty() {
        return Err("token 不能为空".into());
    }
    qoder_accounts::update(r, &id, |a| {
        a.access_token = Some(token.trim().to_string());
        a.source = qoder_accounts::SOURCE_PAT.to_string();
    })
    .map(|a| json!(a))
}

/// 查一次额度与套餐并写回账号记录。
#[tauri::command]
pub async fn qoder_account_refresh(region: String, id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let list = qoder_accounts::list(r)?;
    let acc = list
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("找不到账号 {id}"))?;
    let updated = qoder_api::refresh_account(&acc).await?;
    Ok(json!(updated))
}


/// 批量刷新一个区域的全部账号额度。
///
/// **串行**查（与 0.3.4 的 WorkBuddy 额度查询同口径）：并发会让上游频控，
/// 而且多账号同时刷新 token 容易互相踢。单个失败只记入 `failed`，不影响其它账号。
#[tauri::command]
pub async fn qoder_accounts_refresh_all(region: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let list = qoder_accounts::list(r)?;
    let mut ok = 0usize;
    let mut failed: Vec<Value> = Vec::new();
    for acc in &list {
        if !acc.is_usable() {
            failed.push(json!({ "uid": acc.uid, "error": "没有凭据" }));
            continue;
        }
        match qoder_api::refresh_account(acc).await {
            Ok(_) => ok += 1,
            Err(e) => failed.push(json!({ "uid": acc.uid, "error": e })),
        }
    }
    Ok(json!({ "ok": ok, "failed": failed, "total": list.len() }))
}


/// 近 N 天的 Qoder 积分消耗（**本机快照算出来的真实值**，不是官方用量）。
///
/// Qoder 没有用量 / 历史端点，这里的数据来自每次成功查询后留下的一笔额度快照
/// （`~/.twin-switch/qoder/credit-snapshots.jsonl`，10 分钟节流）。
/// 样本不足时 `enoughData=false` —— 界面据此显示「样本不足」，而不是画一条 0。
#[tauri::command]
pub async fn qoder_credit_history(days: Option<u32>) -> Result<Value, String> {
    Ok(wb_switch_core::modules::qoder_credit_history::summary(
        days.unwrap_or(7),
    ))
}

/// 发起 Qoder 浏览器登录（设备授权 + PKCE）。**不发网络请求**，只返回授权页地址。
#[tauri::command]
pub async fn qoder_login_start(region: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_login::start(r)
}

/// 轮询一次登录状态。`done=false` 就继续等（前端每 2 秒一次）。
#[tauri::command]
pub async fn qoder_login_poll(region: String, login_id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    Ok(qoder_login::poll(r, &login_id).await)
}

#[tauri::command]
pub async fn qoder_login_cancel(login_id: String) -> Result<bool, String> {
    Ok(qoder_login::cancel(&login_id))
}


/// 导出账号包（**含明文凭据**）。返回落盘路径与 JSON 文本。
#[tauri::command]
pub async fn qoder_accounts_export(region: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_tools::export(r)
}

/// 导入预览（脱敏：只回 uid / 昵称 / 有没有凭据）。
#[tauri::command]
pub async fn qoder_import_preview(region: String, json: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_tools::import_preview(r, &json)
}

/// 真正导入。`indexes = null` 表示全部。
#[tauri::command]
pub async fn qoder_import_apply(
    region: String,
    json: String,
    indexes: Option<Vec<usize>>,
) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_tools::import_apply(r, &json, indexes)
}

/// 一键签到：当前区域所有有凭据的账号**串行**签（避免上游频控）。
///
/// 活动制（`/sash/api/v1/me/campaigns`）。结果四档：
/// 领到 / 今日已签 / **有活动但领不了**（前置条件未达成）/ 失败。
/// 「领不了」单独一档 —— 它既不是成功也不是失败，重试也不会成功。
#[tauri::command]
pub async fn qoder_checkin_all(region: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let list = qoder_accounts::list(r)?;
    let mut claimed = 0usize;
    let mut already = 0usize;
    let mut blocked = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut blocked_reason = String::new();
    let mut messages: Vec<String> = Vec::new();

    for acc in list.iter().filter(|a| a.is_usable()) {
        match qoder_tools::checkin_claim(r, acc).await {
            Ok(v) => {
                let is_ok = v.get("ok").and_then(Value::as_bool) == Some(true);
                if is_ok && v.get("already").and_then(Value::as_bool) == Some(true) {
                    already += 1;
                } else if is_ok {
                    claimed += 1;
                } else if v.get("blocked").and_then(Value::as_bool) == Some(true) {
                    blocked += 1;
                    if blocked_reason.is_empty() {
                        blocked_reason = v
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                    }
                } else {
                    failures.push(
                        v.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("签到失败")
                            .to_string(),
                    );
                }
                if let Some(m) = v.get("message").and_then(Value::as_str) {
                    if !m.is_empty() {
                        messages.push(format!("{}：{m}", acc.name));
                    }
                }
            }
            Err(e) => failures.push(format!("{}：{e}", acc.name)),
        }
    }

    Ok(json!({
        "claimed": claimed,
        "already": already,
        "blocked": blocked,
        "blockedReason": blocked_reason,
        "failed": failures.len(),
        "messages": messages,
        "failures": failures,
    }))
}

/// Qoder 账号 token 弹窗的数据：与 Trae / WorkBuddy **同一个形状**（只读、不联网）。
///
/// 三个产品的令牌弹窗是同一个组件，按 `provider` 分流取值 —— 形状一致才能共用。
#[tauri::command]
pub async fn qoder_account_token(region: String, id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let acc = qoder_accounts::list(r)?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("找不到账号 {id}"))?;
    Ok(qoder_accounts::token_info(&acc))
}

/// Qoder 账号 token 续期，返回**与 Trae / WorkBuddy 同一形状**的刷新结果。
///
/// 共用弹窗（`account-token-dialog.tsx`）只认 `{ refreshed, reason?, remainingDays,
/// thresholdDays?, message, tokenExp, tokenExpBefore }`；而 `qoder_token_refresh` 返回的是
/// 整个账号对象。所以这里做一次适配 —— 形状统一放在后端，前端就不用为 Qoder 分叉。
///
/// Qoder 没有「剩余多久才允许续」的阈值语义（有 refresh_token 就能换），
/// 因此 `force` 参数在这里只是**接受但不改变行为**，`thresholdDays` 如实给 0。
#[tauri::command]
pub async fn qoder_account_token_refresh(region: String, id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let acc = qoder_accounts::list(r)?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("找不到账号 {id}"))?;
    let before = acc.expires_at.filter(|v| *v > 0);
    let updated = qoder_api::refresh_token(&acc).await?;
    let after = updated.expires_at.filter(|v| *v > 0);
    let remaining = after.map(qoder_days_left).unwrap_or(-1);
    Ok(json!({
        "refreshed": true,
        "remainingDays": remaining,
        "thresholdDays": 0,
        "message": match (before, after) {
            (Some(b), Some(a2)) => format!(
                "新的到期时间：{}（原来 {}）",
                wb_switch_core::modules::config::fmt_ms_local(a2),
                wb_switch_core::modules::config::fmt_ms_local(b)
            ),
            (_, Some(a2)) => format!("新的到期时间：{}", wb_switch_core::modules::config::fmt_ms_local(a2)),
            _ => "令牌已续期（上游没返回到期时间）".to_string(),
        },
        "tokenExp": after.map(Value::from).unwrap_or(Value::Null),
        "tokenExpBefore": before.map(Value::from).unwrap_or(Value::Null),
    }))
}

/// 毫秒 → 「还剩几天」（与 `config::days_left_ms` 同口径：四舍五入到天）。
fn qoder_days_left(exp_ms: i64) -> i64 {
    wb_switch_core::modules::config::days_left_ms(exp_ms)
}

/// 刷新该账号的访问令牌（用 refresh_token 换新的，并写回账号库）。
#[tauri::command]
pub async fn qoder_token_refresh(region: String, id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let acc = qoder_accounts::list(r)?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("找不到账号 {id}"))?;
    let updated = qoder_api::refresh_token(&acc).await?;
    Ok(json!(updated))
}

/// 改显示名（不动 uid / 凭据）。
#[tauri::command]
pub async fn qoder_account_rename(region: String, id: String, name: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("名字不能为空".into());
    }
    qoder_accounts::update(r, &id, |a| a.name = name.clone()).map(|a| json!(a))
}

/// 「切换」：把该账号的凭据**写回 Qoder 桌面端登录态**（`auth.v1.dat`）。
///
/// 四条纪律见 `qoder_auth_dat` 模块头：原地改写、进程守卫、写后校验、失败回滚。
/// 写完同时把它设为网关的默认账号（这样客户端和网关落在同一个号上）。
///
/// ⚠️ 本机没有桌面端登录态时**不静默降级**，直接报错说明原因。
#[tauri::command]
pub async fn qoder_account_switch(region: String, id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let acc = qoder_accounts::list(r)?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("找不到账号 {id}"))?;
    let access = acc
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "这个账号没有访问令牌，无法切换".to_string())?;
    let refresh = acc.refresh_token.clone().unwrap_or_default();

    let outcome = qoder_auth_dat::switch_to(r, &acc.uid, access, &refresh)?;
    qoder_accounts::set_default(r, &acc.uid)?;

    // 写完把客户端拉起来 —— 用户切号之后本来就要打开它，不该让他再点一次图标。
    let launched = qoder_auth_dat::launch_desktop(r).ok();

    Ok(json!({
        "ok": true,
        "mode": "desktop",
        "path": outcome.path.display().to_string(),
        "backup": outcome.backup.map(|p| p.display().to_string()),
        "launched": launched,
        "note": match &launched {
            Some(p) => format!("{}（已启动 {}）", outcome.note, p),
            None => format!("{}（没能自动启动客户端，请手动打开）", outcome.note),
        },
    }))
}


/// 从本机 Qoder 客户端导入登录态 —— **自动识别两个版本**。
///
/// 本机可能同时装着国内版与国际版（`%APPDATA%\com.qodercn.app.stable` /
/// `%APPDATA%\com.qoder.app.stable`）。这里**各扫各的、各写各的库**：
/// 国际版账号只会进 `accounts.global.json`，不会污染国内版库。
///
/// 导入后立刻逐个校验（能查通或续期成功才算可用），失效的标 `disabled` 并进 `failed`。
#[tauri::command]
pub async fn qoder_import_local() -> Result<Value, String> {
    let mut out = serde_json::Map::new();

    for r in [qoder_realm::QoderRegion::Cn, qoder_realm::QoderRegion::Global] {
        let key = r.as_str().to_string();
        let found = qoder_auth_dat::scan_local(r);
        if found.is_empty() {
            let reason = match qoder_auth_dat::auth_dat_path(r) {
                Some(p) => format!(
                    "找到登录态文件但解不开或没有令牌：{}",
                    p.display()
                ),
                None => "本机没有这个版本的登录态（没装 / 没登录过）".to_string(),
            };
            out.insert(key, json!({ "imported": 0, "updated": 0, "accounts": [], "failed": [], "reason": reason }));
            continue;
        }

        let existing = qoder_accounts::list(r)?;
        let mut imported = 0usize;
        let mut updated = 0usize;
        let mut rows: Vec<Value> = Vec::new();

        for a in found {
            match existing.iter().find(|x| x.uid == a.uid) {
                Some(old) => {
                    let id = old.id.clone();
                    let (token, refresh, name, email, avatar, source, exp) = (
                        a.token.clone(),
                        a.refresh.clone(),
                        a.name.clone(),
                        a.email.clone(),
                        a.avatar.clone(),
                        a.source.clone(),
                        a.expires_at,
                    );
                    qoder_accounts::update(r, &id, |x| {
                        x.access_token = Some(token);
                        if refresh.is_some() {
                            x.refresh_token = refresh;
                        }
                        if !name.is_empty() {
                            x.name = name;
                        }
                        if email.is_some() {
                            x.email = email;
                        }
                        if let Some(v) = avatar {
                            x.meta = serde_json::json!({ "avatar": v });
                        }
                        x.source = source;
                        if exp.is_some() {
                            x.expires_at = exp;
                        }
                        // 重新导入视为「用户想再用它」：先解开禁用，交给下面的校验裁决。
                        x.disabled = false;
                    })?;
                    updated += 1;
                }
                None => {
                    let mut acc = qoder_accounts::QoderAccount::new(r, &a.uid, a.name.clone());
                    acc.access_token = Some(a.token.clone());
                    acc.refresh_token = a.refresh.clone();
                    acc.email = a.email.clone();
                    acc.source = a.source.clone();
                    acc.expires_at = a.expires_at;
                    if let Some(v) = a.avatar.clone() {
                        acc.meta = serde_json::json!({ "avatar": v });
                    }
                    qoder_accounts::add(acc)?;
                    imported += 1;
                }
            }
            rows.push(json!({ "uid": a.uid, "name": a.name, "source": a.source }));
        }

        // 校验：能查通（或续期成功）才算可用。
        let mut failed: Vec<Value> = Vec::new();
        for row in &rows {
            let uid = row.get("uid").and_then(Value::as_str).unwrap_or_default();
            let Some(acc) = qoder_accounts::list(r)?.into_iter().find(|x| x.uid == uid) else {
                continue;
            };
            match qoder_api::refresh_account(&acc).await {
                Ok(_) => {}
                Err(e) => {
                    let _ = qoder_accounts::update(r, &acc.id, |x| x.disabled = true);
                    failed.push(json!({ "uid": uid, "error": e }));
                }
            }
        }

        out.insert(
            key,
            json!({
                "imported": imported,
                "updated": updated,
                "accounts": rows,
                "failed": failed,
            }),
        );
    }

    Ok(Value::Object(out))
}
















// ---------------------------------------------------------------------------
// Qoder 会话记录（只读本地 SQLite）
// ---------------------------------------------------------------------------

/// 会话列表。`include_archived = false` 时过滤归档。
#[tauri::command]
pub async fn qoder_sessions_list(region: String, include_archived: Option<bool>) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_sessions::list(r, include_archived.unwrap_or(false)).map(|v| json!(v))
}

/// 单个会话的消息。正文只取 `parts[type=text]`（`thinking` 单独给）。
#[tauri::command]
pub async fn qoder_session_detail(region: String, session_id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_sessions::detail(r, &session_id)
}

/// 两个版本各有多少会话。
#[tauri::command]
pub async fn qoder_sessions_counts() -> Result<Value, String> {
    Ok(qoder_sessions::counts())
}


/// 改会话标题（会写客户端库：客户端在跑时拒绝，写前自动备份）。
#[tauri::command]
pub async fn qoder_session_rename(region: String, session_id: String, title: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_sessions::rename(r, &session_id, &title)
}

/// 删除会话（软删除 `deleted_at`，消息保留）。
#[tauri::command]
pub async fn qoder_session_delete(region: String, session_id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_sessions::remove(r, &session_id)
}

/// 批量删除会话：一次关客户端、一次备份、一个事务、一次重启。
///
/// 必须走 `spawn_blocking`：关客户端 + 重启要十几秒，占住主线程窗口会假死。
#[tauri::command]
pub async fn qoder_session_delete_many(
    region: String,
    session_ids: Vec<String>,
) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    tauri::async_runtime::spawn_blocking(move || {
        qoder_sessions::remove_many(r, &session_ids)
    })
    .await
    .map_err(|e| format!("批量删除任务失败: {e}"))?
}

/// 复制会话（同库新建一条 + 复制全部消息）。
#[tauri::command]
pub async fn qoder_session_duplicate(region: String, session_id: String) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_sessions::duplicate(r, &session_id)
}


/// 存该账号的 Cosy 设备身份（从抓包里复制 `cosy-machinetoken` / `cosy-machinetype`）。
///
/// 活动下发（`/me/campaigns`）绑定登录时服务端签发的设备令牌；客户端只在内存里持有，
/// 所以要用户粘贴一次。两个都给才算设置了；传空串表示清除、退回派生值。
#[tauri::command]
pub async fn qoder_account_set_cosy(
    region: String,
    id: String,
    machine_token: String,
    machine_type: String,
) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    let token = machine_token.trim().to_string();
    let mtype = machine_type.trim().to_string();
    if token.is_empty() || mtype.is_empty() {
        return Err("machinetoken 与 machinetype 都要填（抓包请求头里各复制一份）".into());
    }
    qoder_accounts::update(r, &id, |a| {
        a.cosy_machine_token = Some(token);
        a.cosy_machine_type = Some(mtype);
    })
    .map(|a| json!(a))
}


/// 积分共享总开关：关掉会**真的停掉网关**（释放端口），不只是隐藏入口。
#[tauri::command]
pub async fn gateway_set_enabled(enabled: bool) -> Result<Value, String> {
    gateway_server::set_enabled(enabled)?;
    if enabled {
        let port = gateway_keys::GATEWAY_PORT;
        tauri::async_runtime::spawn(async move {
            // `restart` 而不是 `serve`：刚关过的话旧实例还在退出，`serve` 会因幂等直接返回，
            // 结果「开关是开的、端口没人听」。
            if let Err(e) = gateway_server::restart(port).await {
                eprintln!("[gateway] {e}");
            }
        });
        gateway_wait_listening(gateway_keys::GATEWAY_PORT, 4000).await;
    } else {
        gateway_server::request_stop();
    }
    Ok(json!({ "enabled": enabled, "status": gateway_server::status() }))
}

/// 局域网开关：开了才绑 `0.0.0.0`（局域网其它设备能连），默认只绑回环。
///
/// ⚠️ 改绑定地址必须**重绑端口**，所以这里会重启网关 —— 正在生成的对话会断。
#[tauri::command]
pub async fn gateway_set_lan(lan: bool) -> Result<Value, String> {
    gateway_server::set_lan(lan)?;
    // 总开关关着的时候不需要重绑（根本没监听），改配置就够了，下次启动生效。
    let enabled = gateway_server::enabled();
    if enabled {
        let port = gateway_keys::GATEWAY_PORT;
        tauri::async_runtime::spawn(async move {
            if let Err(e) = gateway_server::restart(port).await {
                eprintln!("[gateway] {e}");
            }
        });
        gateway_wait_listening(gateway_keys::GATEWAY_PORT, 5000).await;
    }
    Ok(json!({ "lan": lan, "enabled": enabled, "status": gateway_server::status() }))
}

/// 等端口真有应答（`127.0.0.1:<port>` 能连上）。返回是否连上 —— 超时不算成功。
///
/// 为什么不用 `status().running`：`running` 读的是本进程标记，重启窗口里它可能先亮后灭；
/// 「能连上」才是客户端能用的唯一事实。
async fn gateway_wait_listening(port: u16, budget_ms: u64) -> bool {
    let step = 50u64;
    let mut waited = 0u64;
    while waited < budget_ms {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(step)).await;
        waited += step;
    }
    false
}


/// 改 Qoder 会话里**一条消息的正文**（只动 `parts[type=text].text`，保留思维链/工具调用）。
#[tauri::command]
pub async fn qoder_session_update_message(
    region: String,
    session_id: String,
    message_id: String,
    text: String,
) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_sessions::update_message_text(r, &session_id, &message_id, &text)
}

/// 删除 Qoder 会话里的一条消息（物理删，写前已备份）。
#[tauri::command]
pub async fn qoder_session_delete_message(
    region: String,
    session_id: String,
    message_id: String,
) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_sessions::delete_message(r, &session_id, &message_id)
}


/// 首页的 Qoder 概况（账号数 / 积分合计 / 会话数 / 客户端是否运行）。
#[tauri::command]
pub async fn qoder_home_summary() -> Result<Value, String> {
    Ok(qoder_tools::home_summary())
}


/// Qoder 本机数据：扫可清理项（缓存 / 日志 / 我们自己的备份）。
#[tauri::command]
pub async fn qoder_cleanup_scan() -> Result<Value, String> {
    Ok(qoder_cleanup::scan())
}

/// Qoder 本机数据清理（只删勾选项，且必须在扫描清单内）。
#[tauri::command]
pub async fn qoder_cleanup_run(region: String, keys: Vec<String>) -> Result<Value, String> {
    let r = qoder_realm::QoderRegion::parse(&region).ok_or("未知区域")?;
    qoder_cleanup::clean(r, &keys)
}


/// 清空 Qoder 清理页的回收站（硬删，不可恢复）。
#[tauri::command]
pub async fn qoder_cleanup_empty_trash() -> Result<Value, String> {
    qoder_cleanup::empty_trash()
}


/// 「使用说明」页的真机自检：网关在跑 → 能列模型 → 会选哪个账号 →（可选）真发一次对话。
///
/// ⚠️ 这不是示意图：每一步都是**现场跑出来的真实结果**，失败会如实返回原因。
#[tauri::command]
pub async fn gateway_guide_selftest(
    key: Option<String>,
    model: Option<String>,
) -> Result<Value, String> {
    Ok(gateway_server::selftest(key, model).await)
}
