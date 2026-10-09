//! Tauri commands：前端调用的薄包装，对应 trae-switch 原版的账号/记录能力。
//!
//! 覆盖：Trae 客户端发现 / 账号库 / 切换 / 网页登录 / 解密 / 导入 / 删除 / 交接记忆，
//! 外加 relaunch、错误日志等通用命令。

use serde_json::{json, Value};

use tauri::Emitter;
use wb_switch_core::modules::{
    app_overview, client_usage, config, error_log, trae_cleanup, trae_credits, trae_delete,
    trae_discover, trae_export, trae_handoff, trae_import, trae_memory_scan, trae_oauth,
    trae_profile, trae_remote, trae_switch, trae_vault, workbuddy_cleanup, workbuddy_credits,
    workbuddy_export, workbuddy_import, workbuddy_oauth, workbuddy_sessions, workbuddy_switch,
    workbuddy_vault,
};

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
pub async fn trae_list_clients() -> Value {
    off_main(|| {
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
                json!({
                    "id": id,
                    "meta": meta,
                    "kind": kind,
                    "oauth": oauth,
                    "displayName": trae_vault::display_name(&client_key, id),
                    "profile": trae_vault::read_profile(&client_key, id),
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
        let progress: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
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
        // 丢弃元信息 → 强制重新解密一次，拿到与实时库一致的最新快照
        trae_export::drop_snapshot_meta(&client_key);
        let out = trae_export::ensure_decrypted(&client_key, Some(&|m| {
            progress.lock().unwrap().push(m.to_string());
        }))
        .map_err(|e| {
            let p = progress.lock().unwrap().join("\n");
            format!("{e}\n{p}")
        })?;
        let wal = std::path::PathBuf::from(format!("{}-wal", db.display()));
        Ok(json!({
            "scan": scan,
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

/// 源账号与目标账号之间已建立的会话副本关联（只读）。
#[tauri::command]
pub async fn workbuddy_session_links_preview(source_uid: String, target_uid: String) -> Value {
    off_main(move || workbuddy_sessions::links_preview(&source_uid, &target_uid))
        .await
        .unwrap_or_else(|e| json!({ "ok": false, "error": e }))
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
