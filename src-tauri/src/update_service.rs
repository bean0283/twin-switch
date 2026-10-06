//! 统一更新服务：检查 / 下载 / 安装重启的**单一状态机**。
//!
//! 前端只订阅 `update-state` 事件与查询 `update_state` 命令，不自己调 JS 版 updater ——
//! 状态只有一个真相源，界面不会出现「弹窗说 30%、版本行说已最新」这种自相矛盾。
//!
//! - **检查**复用 `wb_switch_core::modules::update::update_check`（release 资产 manifest
//!   主端点 + 302 兜底，不占 GitHub API 配额）；
//! - **下载**走 `tauri-plugin-updater` 的 Rust API，**签名校验在 `Update::download` 内部完成**，
//!   也就是「装了不是我们签的包」这件事在下载阶段就会被拒；
//! - 代理取自 `update::load_update_config()` 的 `proxy`（`~/.twin-switch/update.json`）。
//!
//! **安装时机**：下载完成的包**暂存在内存**，等用户点「重启并安装」才落盘安装。
//! 原因是 Windows 的 NSIS 安装器要求应用先退出才能替换文件 —— 下载完就自动装等于
//! 强制重启，用户正在做的事会被打断。把它放在用户主动动作上，语义才自然。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_updater::UpdaterExt;

use wb_switch_core::modules::update;

/// 后台首次检查延迟：启动 15 秒后。
///
/// 不让它和首屏抢资源 —— 启动瞬间首页已经在跑「读缓存 + 后台重算总览」，
/// 再叠一个 GitHub 请求只会让首屏更慢。
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(15);
/// 后台检查间隔（30 分钟）。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

const BUSY_MESSAGE: &str = "更新任务正在进行中，请稍候";
const NO_PACKAGE_MESSAGE: &str = "没有可安装的更新包，请先下载更新包";

/// 更新阶段。前端与托盘只读这一份状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    /// 尚未检查 / 未开始。
    Idle,
    /// 正在检查更新源。
    Checking,
    /// 已是最新版本。
    UpToDate,
    /// 有新版且可下载。
    Available,
    /// 正在后台下载更新包。
    Downloading,
    /// 包已下载完成，等待用户重启（安装发生在重启时）。
    ReadyToRestart,
    /// 检查 / 下载 / 安装失败，可按文案重试。
    Error,
}

/// 更新状态快照：命令返回值、`update-state` 事件负载的唯一来源。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSnapshot {
    pub phase: UpdatePhase,
    /// 目标版本号（无 `v` 前缀）；检查失败或已是最新时为 None。
    pub latest: Option<String>,
    /// 下载进度（0-100）；总量未知（无 `Content-Length`）时为 None。
    pub percent: Option<u8>,
    /// 错误 / 提示文案。
    pub message: Option<String>,
    /// 最近一次检查完成时刻（秒）。
    pub checked_at: Option<i64>,
    /// 当前运行版本，供界面显示「v0.1.0 → v0.2.0」。
    pub current: String,
}

impl UpdateSnapshot {
    fn idle() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            latest: None,
            percent: None,
            message: None,
            checked_at: None,
            current: update::APP_VERSION.to_string(),
        }
    }
}

/// 已下载的更新包：`bytes` 交给 `Update::install`，`update` 提供安装上下文
/// （安装目标路径、安装器参数等，**无法在外部重建**，所以必须一起留着）。
struct DownloadedPackage {
    version: String,
    bytes: Vec<u8>,
    update: tauri_plugin_updater::Update,
}

static SNAPSHOT: OnceLock<Mutex<UpdateSnapshot>> = OnceLock::new();
static PACKAGE: OnceLock<Mutex<Option<DownloadedPackage>>> = OnceLock::new();
/// 「检查 / 下载 / 安装」共用互斥：任一任务在跑时重复点击直接拒绝。
static BUSY: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// 互斥
// ---------------------------------------------------------------------------

/// 更新任务互斥 guard：覆盖「检查 / 下载 / 安装」全过程，drop 即释放。
struct BusyGuard;

impl BusyGuard {
    /// 抢占更新任务；已有任务在跑时返回 None（调用方按各自契约拒绝）。
    fn acquire() -> Option<Self> {
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// 快照读写（无宿主依赖：单测直接驱动这一层）
// ---------------------------------------------------------------------------

fn snapshot_cell() -> &'static Mutex<UpdateSnapshot> {
    SNAPSHOT.get_or_init(|| Mutex::new(UpdateSnapshot::idle()))
}

fn package_cell() -> &'static Mutex<Option<DownloadedPackage>> {
    PACKAGE.get_or_init(|| Mutex::new(None))
}

fn with_snapshot<T>(edit: impl FnOnce(&mut UpdateSnapshot) -> T) -> T {
    let mut guard = snapshot_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    edit(&mut guard)
}

/// 读当前快照（`update_state` 命令用）。
pub fn snapshot() -> UpdateSnapshot {
    with_snapshot(|snapshot| snapshot.clone())
}

/// 进入检查阶段。
///
/// 下载中 / 已就绪**不被后台检查改写**：否则界面上「正在下载 40% / 重启并安装」
/// 会闪回「正在检查…」，用户正准备点的入口会消失。
fn begin_check() -> UpdateSnapshot {
    with_snapshot(|snapshot| {
        if matches!(
            snapshot.phase,
            UpdatePhase::Downloading | UpdatePhase::ReadyToRestart
        ) {
            return snapshot.clone();
        }
        snapshot.phase = UpdatePhase::Checking;
        snapshot.percent = None;
        snapshot.message = None;
        snapshot.clone()
    })
}

/// 写入检查结果（`ok=false` 或 `hasUpdate=false` 分别落到 Error / UpToDate）。
fn apply_check_result(result: &Value) -> UpdateSnapshot {
    with_snapshot(|snapshot| {
        if matches!(
            snapshot.phase,
            UpdatePhase::Downloading | UpdatePhase::ReadyToRestart
        ) {
            return snapshot.clone();
        }
        snapshot.percent = None;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            // 已有明确目标版本时（下载失败后再检查、或 Available 被后台复查带进 Checking），
            // 失败**不得清掉升级入口**：否则 30 分钟后台检查一旦网络抖动，
            // 界面就从「升级到 vX」变成「检查更新失败」。
            if snapshot.latest.is_some()
                && matches!(
                    snapshot.phase,
                    UpdatePhase::Checking | UpdatePhase::Available
                )
            {
                snapshot.phase = UpdatePhase::Available;
                snapshot.message = None;
                return snapshot.clone();
            }
            snapshot.phase = UpdatePhase::Error;
            snapshot.latest = None;
            snapshot.message = result
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| result.get("error").and_then(Value::as_str))
                .map(str::to_string);
            return snapshot.clone();
        }
        let latest = result
            .get("latest")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let has_update =
            result.get("hasUpdate").and_then(Value::as_bool) == Some(true) && !latest.is_empty();
        snapshot.phase = if has_update {
            UpdatePhase::Available
        } else {
            UpdatePhase::UpToDate
        };
        snapshot.latest = has_update.then_some(latest);
        snapshot.message = None;
        snapshot.checked_at = result
            .get("checkedAt")
            .and_then(Value::as_i64)
            .or_else(|| Some(update::now_secs()));
        snapshot.clone()
    })
}

/// 进入下载阶段（进度百分比由首个 chunk 回调填入，总量未知时保持 None）。
fn begin_download(latest: &str) -> UpdateSnapshot {
    with_snapshot(|snapshot| {
        snapshot.phase = UpdatePhase::Downloading;
        snapshot.latest = Some(latest.to_string());
        snapshot.percent = None;
        snapshot.message = None;
        snapshot.clone()
    })
}

/// 下载完成：包已存入 `PACKAGE`，界面转为「重启并安装」。
fn mark_ready_to_restart(latest: &str) -> UpdateSnapshot {
    with_snapshot(|snapshot| {
        snapshot.phase = UpdatePhase::ReadyToRestart;
        snapshot.latest = Some(latest.to_string());
        snapshot.percent = Some(100);
        snapshot.message = None;
        snapshot.clone()
    })
}

/// 失败：置 Error（**保留 `latest`**，界面据此区分「检查失败」与「下载失败」两种重试文案）。
fn fail(message: String) -> UpdateSnapshot {
    with_snapshot(|snapshot| {
        snapshot.phase = UpdatePhase::Error;
        snapshot.percent = None;
        snapshot.message = Some(message);
        snapshot.clone()
    })
}

/// 下载进度换算：总量未知或为 0 → None；超出总量按 100 截断。
fn percent_of(downloaded: u64, total: Option<u64>) -> Option<u8> {
    let total = total.filter(|total| *total > 0)?;
    let percent = downloaded.saturating_mul(100) / total;
    Some(percent.min(100) as u8)
}

/// 记录一次进度回调：**整数百分比变化才写快照并返回新值**（未变化返回 None）。
///
/// 下载回调是按 TCP 分片触发的，一次下载能来上千次；每次写快照 + emit 事件
/// 会把 IPC 打满，所以按整数百分比去重。
fn advance_progress(downloaded: u64, total: Option<u64>) -> Option<u8> {
    let percent = percent_of(downloaded, total)?;
    with_snapshot(|snapshot| {
        if snapshot.percent == Some(percent) {
            return None;
        }
        snapshot.percent = Some(percent);
        Some(percent)
    })
}

// ---------------------------------------------------------------------------
// 对外能力
// ---------------------------------------------------------------------------

/// 检查更新：走状态机（Checking → Available / UpToDate / Error），返回与旧
/// `check_update` 完全同形的结果。已有更新任务在跑时直接返回忙碌错误，不并发触发第二次。
pub async fn check<R: Runtime>(app: &AppHandle<R>, proxy: Option<&str>, force: bool) -> Value {
    let Some(_busy) = BusyGuard::acquire() else {
        eprintln!("[更新] 已有任务在跑，跳过本次检查");
        let mut value = json!({
            "ok": false,
            "error": BUSY_MESSAGE,
            "message": BUSY_MESSAGE,
        });
        value["releaseUrl"] = json!(release_url());
        return value;
    };
    eprintln!("[更新] 检查更新（force={force}）");
    set_phase(app, begin_check());
    let result = update::update_check(proxy, force).await;
    let snapshot = set_phase(app, apply_check_result(&result));
    eprintln!(
        "[更新] 检查结果：{:?} latest={:?}",
        snapshot.phase, snapshot.latest
    );
    result
}

/// 启动下载：**立即返回**，进度走 `update-state` 事件。
///
/// 下载在后台任务里进行（含重新 `check()` 一次以拿到 `Update` 句柄），因此界面不用等网络；
/// 重复调用由 `BUSY` 拒绝。
pub fn start_download<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let Some(busy) = BusyGuard::acquire() else {
        eprintln!("[更新] 已有任务在跑，跳过本次下载");
        return Err(BUSY_MESSAGE.to_string());
    };
    eprintln!("[更新] 启动下载任务");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _busy = busy;
        match download_package(&app).await {
            // 下载完成：包先暂存，等用户点「重启并安装」时才真正安装。
            Ok(Some(package)) => {
                let version = package.version.clone();
                eprintln!("[更新] 下载完成 v{version}（{} 字节）", package.bytes.len());
                store_package(package);
                set_phase(&app, mark_ready_to_restart(&version));
            }
            // 更新源已无可用包（版本已追平）。
            Ok(None) => {
                eprintln!("[更新] 更新源无可用更新包，转为已是最新");
                set_phase(
                    &app,
                    apply_check_result(&json!({"ok": true, "hasUpdate": false})),
                );
            }
            Err(message) => {
                eprintln!("[更新] 下载失败：{message}");
                set_phase(&app, fail(message));
            }
        }
    });
    Ok(())
}

/// 安装已下载的更新包并重启应用（用户点「重启并安装」时调用）。
///
/// 无包时返回**可读错误**（不 panic）；安装失败置 Error 并释放已暂存的包。
pub async fn restart<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let package = take_package_for_install()?;
    let Some(_busy) = BusyGuard::acquire() else {
        // 拿不到互斥就把包放回去：用户还能再点一次，不必重下。
        store_package(package);
        return Err(BUSY_MESSAGE.to_string());
    };

    let DownloadedPackage {
        version,
        bytes,
        update,
    } = package;
    eprintln!("[更新] 开始安装 v{version}");
    // 安装要解压整包并替换应用文件，放 blocking 线程，别占住 async worker。
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await;
    match installed {
        Ok(Ok(())) => match crate::commands::relaunch_app_inner(app) {
            // 非 Windows：安装已完成，由本进程拉起新版本。Windows 侧 NSIS 安装器
            // 会先让本进程退出（`install` 内部 `std::process::exit`），到不了这里。
            Ok(()) => Ok(()),
            Err(error) => {
                // 包已装好，重启失败只剩「手动重启」一条路：明确告知，避免用户以为要重下。
                let message = format!("v{version} 已安装，请手动重启应用完成升级：{error}");
                eprintln!("[更新] 重启失败：{message}");
                set_phase(app, fail(message.clone()));
                Err(message)
            }
        },
        Ok(Err(error)) => {
            let message = format!("安装 v{version} 失败：{error}");
            eprintln!("[更新] {message}");
            set_phase(app, fail(message.clone()));
            Err(message)
        }
        Err(error) => {
            let message = format!("安装 v{version} 失败：{error}");
            eprintln!("[更新] {message}");
            set_phase(app, fail(message.clone()));
            Err(message)
        }
    }
}

/// 后台定时检查：首次延迟 [`FIRST_CHECK_DELAY`]，之后每 [`CHECK_INTERVAL`] 一次。
///
/// 常驻循环随进程存活；未带 `force` 时走 core 的 6 小时缓存，
/// 因此多数轮次**一个网络请求都不发**（读内存就返回了）。
pub fn spawn_periodic_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let _ = check(&app, None, false).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 下载 / 安装内部实现
// ---------------------------------------------------------------------------

/// 下载更新包：`Ok(None)` 表示更新源当前没有可用包（版本已追平）。
async fn download_package<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Option<DownloadedPackage>, String> {
    let builder = app.updater_builder();
    let builder = match configured_proxy() {
        Some(proxy) => builder.proxy(
            tauri::Url::parse(&proxy).map_err(|error| format!("更新代理地址无效：{error}"))?,
        ),
        None => builder,
    };
    let updater = builder
        .build()
        .map_err(|error| format!("初始化更新器失败：{error}"))?;
    let update = updater
        .check()
        .await
        .map_err(|error| format!("检查更新包失败：{error}"))?;
    let Some(update) = update else {
        return Ok(None);
    };

    let version = update.version.clone();
    set_phase(app, begin_download(&version));
    let progress_app = app.clone();
    let mut downloaded: u64 = 0;
    let bytes = update
        .download(
            move |chunk_length, total| {
                downloaded = downloaded.saturating_add(chunk_length as u64);
                on_download_chunk(&progress_app, downloaded, total);
            },
            || {},
        )
        .await
        .map_err(|error| format!("下载更新包失败：{error}"))?;
    Ok(Some(DownloadedPackage {
        version,
        bytes,
        update,
    }))
}

/// 单个下载分包的回调：整数百分比变化才广播事件。
fn on_download_chunk<R: Runtime>(app: &AppHandle<R>, downloaded: u64, total: Option<u64>) {
    if advance_progress(downloaded, total).is_none() {
        return;
    }
    let _ = app.emit("update-state", snapshot());
}

/// 更新代理：`update::load_update_config()` 的 `proxy` 注入 updater builder；留空表示不设代理。
fn configured_proxy() -> Option<String> {
    update::load_update_config()
        .get("proxy")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|proxy| !proxy.is_empty())
        .map(str::to_string)
}

fn store_package(package: DownloadedPackage) {
    *package_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(package);
}

/// 取出待安装的包；无包时返回可读错误（不 panic）。
fn take_package_for_install() -> Result<DownloadedPackage, String> {
    package_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .ok_or_else(|| NO_PACKAGE_MESSAGE.to_string())
}

/// 公开发布页地址（与 core 的 `update_check` 同源）。
fn release_url() -> String {
    format!(
        "https://github.com/{}/{}/releases/latest",
        update::GITHUB_OWNER,
        update::GITHUB_REPO
    )
}

// ---------------------------------------------------------------------------
// 宿主出口
// ---------------------------------------------------------------------------

/// 统一状态出口：写好的快照经 emit 对外可见。
fn set_phase<R: Runtime>(app: &AppHandle<R>, snapshot: UpdateSnapshot) -> UpdateSnapshot {
    let _ = app.emit("update-state", &snapshot);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// 快照 / BUSY / PACKAGE 都是进程级单例，用例之间必须串行。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn reset() {
        *snapshot_cell()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = UpdateSnapshot::idle();
        BUSY.store(false, Ordering::Release);
    }

    #[test]
    fn phases_flow_idle_available_downloading_ready_to_restart() {
        let _guard = lock();
        reset();
        assert_eq!(snapshot().phase, UpdatePhase::Idle);

        assert_eq!(begin_check().phase, UpdatePhase::Checking);

        let available = apply_check_result(&json!({
            "ok": true,
            "latest": "9.9.9",
            "hasUpdate": true,
            "checkedAt": 1_700_000_000,
        }));
        assert_eq!(available.phase, UpdatePhase::Available);
        assert_eq!(available.latest.as_deref(), Some("9.9.9"));
        assert_eq!(available.checked_at, Some(1_700_000_000));

        let downloading = begin_download("9.9.9");
        assert_eq!(downloading.phase, UpdatePhase::Downloading);
        assert_eq!(downloading.percent, None);

        assert_eq!(advance_progress(30, Some(100)), Some(30));
        assert_eq!(snapshot().percent, Some(30));

        let ready = mark_ready_to_restart("9.9.9");
        assert_eq!(ready.phase, UpdatePhase::ReadyToRestart);
        assert_eq!(ready.percent, Some(100));
    }

    #[test]
    fn check_without_update_lands_on_up_to_date() {
        let _guard = lock();
        reset();
        let snapshot =
            apply_check_result(&json!({"ok": true, "latest": "0.1.0", "hasUpdate": false}));
        assert_eq!(snapshot.phase, UpdatePhase::UpToDate);
        assert_eq!(snapshot.latest, None);
        assert!(snapshot.checked_at.is_some());
    }

    #[test]
    fn failed_check_lands_on_error_with_message() {
        let _guard = lock();
        reset();
        let snapshot = apply_check_result(&json!({
            "ok": false,
            "error": "网络请求失败",
            "message": "网络请求失败（code=-1）",
        }));
        assert_eq!(snapshot.phase, UpdatePhase::Error);
        assert_eq!(snapshot.message.as_deref(), Some("网络请求失败（code=-1）"));
        assert_eq!(snapshot.latest, None);
    }

    /// 已有目标版本时，复查失败必须回到 Available，不能清掉「升级到 vX」入口。
    #[test]
    fn failed_recheck_keeps_available_target() {
        let _guard = lock();
        reset();
        apply_check_result(&json!({"ok": true, "latest": "9.9.9", "hasUpdate": true}));
        assert_eq!(begin_check().phase, UpdatePhase::Checking);
        let snapshot = apply_check_result(&json!({
            "ok": false,
            "error": "网络请求失败",
            "message": "网络请求失败（code=-1）",
        }));
        assert_eq!(snapshot.phase, UpdatePhase::Available);
        assert_eq!(snapshot.latest.as_deref(), Some("9.9.9"));
        assert_eq!(snapshot.message, None);
    }

    #[test]
    fn failed_check_from_idle_still_errors() {
        let _guard = lock();
        reset();
        begin_check();
        let snapshot = apply_check_result(&json!({"ok": false, "error": "网络请求失败"}));
        assert_eq!(snapshot.phase, UpdatePhase::Error);
        assert_eq!(snapshot.latest, None);
    }

    /// 下载中 / 待重启不被后台检查改写：界面上的升级入口不能闪掉。
    #[test]
    fn check_does_not_downgrade_downloading_or_ready_to_restart() {
        let _guard = lock();
        reset();
        begin_download("9.9.9");
        assert_eq!(begin_check().phase, UpdatePhase::Downloading);
        assert_eq!(
            apply_check_result(&json!({"ok": true, "latest": "9.9.10", "hasUpdate": true})).phase,
            UpdatePhase::Downloading
        );

        mark_ready_to_restart("9.9.9");
        assert_eq!(begin_check().phase, UpdatePhase::ReadyToRestart);
        assert_eq!(snapshot().phase, UpdatePhase::ReadyToRestart);
    }

    #[test]
    fn failed_download_keeps_target_version_for_retry() {
        let _guard = lock();
        reset();
        apply_check_result(&json!({"ok": true, "latest": "9.9.9", "hasUpdate": true}));
        begin_download("9.9.9");
        let snapshot = fail("下载更新包失败：连接超时".to_string());
        assert_eq!(snapshot.phase, UpdatePhase::Error);
        assert_eq!(snapshot.latest.as_deref(), Some("9.9.9"));
        assert_eq!(snapshot.percent, None);
    }

    #[test]
    fn busy_guard_rejects_second_entry_until_released() {
        let _guard = lock();
        reset();
        let first = BusyGuard::acquire();
        assert!(first.is_some(), "首次进入应拿到互斥");
        assert!(BusyGuard::acquire().is_none(), "并发进入必须被拒");
        drop(first);
        assert!(BusyGuard::acquire().is_some(), "释放后应重新可进入");
    }

    #[test]
    fn restart_without_package_returns_error_instead_of_panicking() {
        let _guard = lock();
        *package_cell()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let error = match take_package_for_install() {
            Ok(_) => panic!("无包时不应返回可安装的更新包"),
            Err(error) => error,
        };
        assert!(error.contains("没有可安装的更新包"), "{error}");
    }

    #[test]
    fn percent_conversion_clamps_and_handles_unknown_total() {
        assert_eq!(percent_of(0, Some(100)), Some(0));
        assert_eq!(percent_of(1, Some(3)), Some(33));
        assert_eq!(percent_of(100, Some(100)), Some(100));
        assert_eq!(percent_of(150, Some(100)), Some(100), "超出总量按 100 截断");
        assert_eq!(percent_of(10, None), None, "无 Content-Length 时不报百分比");
        assert_eq!(percent_of(10, Some(0)), None, "总量为 0 不除零");
    }

    #[test]
    fn progress_snapshot_written_only_on_integer_change() {
        let _guard = lock();
        reset();
        begin_download("9.9.9");
        assert_eq!(advance_progress(1, Some(1000)), Some(0));
        assert_eq!(
            advance_progress(2, Some(1000)),
            None,
            "同一整数百分比不重复写"
        );
        assert_eq!(advance_progress(10, Some(1000)), Some(1));
        assert_eq!(advance_progress(10, None), None, "总量未知不写百分比");
    }

    /// 事件 / 命令负载的字段契约（前端 `UpdateSnapshot` 按此解析）。
    #[test]
    fn snapshot_serializes_camel_case_contract() {
        let value = serde_json::to_value(UpdateSnapshot::idle()).unwrap();
        assert_eq!(value["phase"], json!("idle"));
        assert_eq!(value["latest"], json!(null));
        assert_eq!(value["percent"], json!(null));
        assert_eq!(value["message"], json!(null));
        assert_eq!(value["checkedAt"], json!(null));
        assert_eq!(value["current"], json!(update::APP_VERSION));

        let value = serde_json::to_value(mark_ready_to_restart("1.2.3")).unwrap();
        assert_eq!(value["phase"], json!("readyToRestart"));
        assert_eq!(value["latest"], json!("1.2.3"));
        assert_eq!(value["percent"], json!(100));
    }

    #[test]
    fn release_url_points_at_the_public_repo() {
        assert_eq!(
            release_url(),
            "https://github.com/bean0283/twin-switch/releases/latest"
        );
    }
}
