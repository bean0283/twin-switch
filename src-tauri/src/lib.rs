// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use tauri::{Manager, WindowEvent};
mod commands;
pub mod selfcheck;
mod update_service;
#[cfg(target_os = "macos")]
mod instance_lock;

/// 托盘图标 hover 时的提示。
///
/// ⚠️ 只在这里出现一次 —— 想改文案改这一处，别去别处再抄一遍。
const TRAY_TOOLTIP: &str = "TwinSwitch —— 左键单击打开，右键菜单";

/// 把主窗口显示出来并聚焦 —— **托盘所有入口的唯一出口**。
///
/// ⚠️⚠️ 三个入口（右键菜单「显示主窗口」/ 左键单击 / 左键双击）**都**必须调它：
///   ① 只 `show()` 不够 —— 窗口如果先前是最小化的，`show()` 之后仍是最小化状态，
///      用户看着「打开了但没反应」，所以要 `unminimize`；
///   ② `set_focus` 保证它真的跑到前台，而不是在别的窗口后面闪一下。
fn focus_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 右下角托盘：关闭窗口时隐藏到托盘（不退出），托盘菜单可恢复 / 真正退出。
///
/// ⚠️⚠️ **「打开程序」在托盘上有三个入口，必须全部接上，行为完全一致**：
///   ① 右键菜单「显示主窗口」；② 左键单击；③ 左键双击。
///
/// **T67**：原先只接了 ①，左键那两个入口**压根没人处理** ⇒ 用户「双击打不开，
/// 只能右键」。注意 `show_menu_on_left_click(false)` 的语义**只是「左键不弹菜单」**，
/// 它**不会**替我们把窗口显示出来 —— 关掉菜单等于把左键空出来，还得自己填行为。
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().expect("缺省窗口图标").clone())
        // 用户「不知道图标怎么打开」时，hover 提示就是最省事的引导
        .tooltip(TRAY_TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => focus_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        // ⚠️ 左键必须接 —— 不接的话单击/双击都是「点了没反应」（T67 的成因）
        .on_tray_icon_event(|tray, event| match event {
            // Windows 上双击的消息序列是 `Click` → `DoubleClick` → `Click`
            // （tray-icon 把 `WM_LBUTTONUP` 映射成 Click、`WM_LBUTTONDBLCLK` 映射成
            // DoubleClick）⇒ 其实只接 Click 就已经覆盖双击了。
            // 这里仍**显式**接住 DoubleClick，是为了让意图留在代码里：
            // 将来谁要改成「单击切前台、双击干别的」，一眼能看见双击早被管着。
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => focus_main_window(tray.app_handle()),
            _ => {}
        })
        .build(app)?;
    // 保持托盘存活：TrayIcon 被 drop 会从系统托盘消失
    app.manage(tray);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ⚠️ 数据目录搬迁必须在**任何东西打开数据库之前**、也在单实例插件之前完成。
    //
    // 改名前的目录是 `~/.trae-switch-cn`（老用户机器上压着几个 GB：解密库、回收站、
    // 导入备份）。这里做的是**同盘 `rename`**，元数据操作，2 GB 也是瞬时的。
    // 失败则本次运行退回旧目录（不复制、不删除），下次启动自动重试。
    //
    // 放在这里的额外理由：`Builder::build()` 里单实例插件命中已有实例会直接
    // `std::process::exit(0)`，那种情况下什么活都不该干 —— 更别说搬目录。
    let (store_dir, migration_note) = wb_switch_core::modules::config::ensure_store_dir_ready();
    eprintln!("[startup] 数据目录: {}", store_dir.display());
    if let Some(note) = migration_note {
        eprintln!("[startup] 数据目录提示:\n{note}");
    }

    let mut builder = tauri::Builder::default();

    // 单实例互斥必须最先注册：`Builder::build()` 按注册顺序 initialize_plugins，
    // 插件 setup 命中已有实例会直接 `std::process::exit(0)`，因此第二个进程在
    // 建主窗口之前就已退出；这里把既有窗口弹出到前台。
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // ⚠️ 与托盘三入口走**同一个出口**：原先这里只 `show + set_focus`、
            // 漏了 `unminimize` ⇒ 「应用已最小化时再次双击 exe」同样是「没反应」。
            focus_main_window(app);
        }));
    }

    builder = builder.plugin(tauri_plugin_opener::init());

    // 自动更新：检查 / 下载 / 安装都走 `update_service`，签名校验由插件内部完成。
    // 只注册不自动装 —— 下载完的包暂存在内存里，等用户点「重启并安装」。
    builder = builder.plugin(tauri_plugin_updater::Builder::new().build());

    let app = builder
        .setup(|app| {
            #[cfg(target_os = "macos")]
            instance_lock::acquire_or_exit(app.handle());
            build_tray(app.handle())?;
            // 后台周期检查：启动 15 秒后首检，此后每 30 分钟一次（多数轮次命中
            // core 的 6 小时缓存，不发网络请求）。
            update_service::spawn_periodic_check(app.handle().clone());
            // 模型目录：**后台空闲时自动刷新并落盘**（启动 20 秒后首跑，此后每 6 小时）。
            // 用户不点「重新识别」也能保持 models.json 是最新的；失败静默（下次再试）。
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                let _ = wb_switch_core::modules::gateway_models::discover_and_save().await;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
                    let _ = wb_switch_core::modules::gateway_models::discover_and_save().await;
                }
            });

            // 积分共享网关：**受开关控制**（`~/.twin-switch/gateway.json` 的 enabled）。
            // 关掉时不监听任何端口；绑定失败只记日志，不影响主界面。
            if !wb_switch_core::modules::gateway_server::enabled() {
                eprintln!("[gateway] 积分共享已关闭（设置里可开启），本次不启动网关");
            } else {
            tauri::async_runtime::spawn(async move {
                if let Err(e) = wb_switch_core::modules::gateway_server::serve(
                    wb_switch_core::modules::gateway_keys::GATEWAY_PORT,
                )
                .await
                {
                    eprintln!("[gateway] {e}");
                }
            });
            }
            Ok(())
        })
        // 点窗口关闭按钮：隐藏到托盘，不退出程序
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::qoder_regions,
            commands::qoder_token_refresh,
            commands::qoder_account_rename,
            commands::qoder_account_switch,
            commands::qoder_accounts_export,
            commands::qoder_import_preview,
            commands::qoder_import_apply,
            commands::qoder_checkin_all,
            commands::qoder_sessions_list,
            commands::qoder_session_detail,
            commands::qoder_sessions_counts,
            commands::qoder_cleanup_scan,
            commands::qoder_cleanup_empty_trash,
            commands::qoder_cleanup_run,
            commands::qoder_home_summary,
            commands::qoder_account_set_cosy,
            commands::qoder_session_update_message,
            commands::qoder_session_delete_message,
            commands::qoder_session_rename,
            commands::qoder_session_delete,
            commands::qoder_session_delete_many,
            commands::qoder_session_duplicate,
            commands::qoder_import_local,
            commands::qoder_login_start,
            commands::qoder_login_poll,
            commands::qoder_login_cancel,
            commands::qoder_accounts_list,
            commands::qoder_account_add,
            commands::qoder_account_remove,
            commands::qoder_account_restore,
            commands::qoder_account_set_token,
            commands::qoder_account_refresh,
            commands::qoder_accounts_refresh_all,
            commands::qoder_credit_history,
            commands::qoder_exe_from_running,
            commands::trae_export_pack,
            commands::workbuddy_export_pack,
            commands::gateway_status,
            commands::gateway_guide_selftest,
            commands::gateway_set_enabled,
            commands::gateway_pool,
            commands::gateway_preview,
            commands::gateway_keys_list,
            commands::gateway_key_create,
            commands::gateway_key_revoke,
            commands::gateway_key_restore,
            commands::gateway_key_delete,
            commands::gateway_models_discover,
            commands::gateway_models_selected,
            commands::gateway_models_save,
            commands::gateway_models_get,
            commands::gateway_models_set,
            commands::gateway_upstreams_get,
            commands::gateway_upstreams_set,
            commands::relaunch_app,
            commands::log_error,
            commands::get_error_log_path,
            commands::store_dir_info,
            commands::reveal_error_log,
            commands::reveal_path,
            commands::trae_export_dir,
            commands::trae_list_clients,
            commands::trae_client_usage_reset,
            commands::trae_account_overview,
            commands::trae_refresh_profile,
            commands::trae_credits_cached,
            commands::trae_credits_query,
            commands::checkin_run,
            commands::trae_identify_live,
            commands::trae_backup_account,
            commands::trae_switch_to,
            commands::trae_rollback_to,
            commands::trae_remove_account,
            commands::trae_rename_account,
            commands::trae_export_account,
            commands::trae_import_account,
            commands::trae_vault_adopt_merged,
            commands::trae_account_token,
            commands::trae_account_token_refresh,
            commands::trae_saved_key,
            commands::trae_scan_and_decrypt,
            commands::trae_decrypt_with_saved_key,
            commands::trae_decrypted_status,
            commands::trae_ensure_decrypted,
            commands::trae_cleanup_working_files,
            commands::trae_list_sessions,
            commands::trae_session_detail,
            commands::trae_session_edit_doc,
            commands::trae_session_edit_targets,
            commands::trae_session_edit_new_ids,
            commands::trae_session_edit_validate,
            commands::trae_session_edit_save,
            commands::trae_session_edit_cell,
            commands::trae_export_session,
            commands::trae_export_all,
            commands::trae_import_candidates,
            commands::trae_import_inspect,
            commands::trae_import_run,
            commands::trae_find_misaligned_projects,
            commands::trae_heal_session_projects,
            commands::trae_session_links_preview,
            commands::trae_session_unlink,
            commands::trae_session_sync_group,
            commands::trae_session_sync_groups,
            commands::trae_session_links_diverged,
            commands::trae_workbuddy_list,
            commands::trae_workbuddy_preview,
            commands::trae_workbuddy_import,
            commands::trae_wb_export_target,
            commands::trae_wb_export_sessions,
            commands::trae_wb_export_preview,
            commands::trae_wb_export_run,
            commands::trae_wb_cleanup_scan,
            commands::trae_wb_cleanup_purge,
            commands::trae_wb_cleanup_empty_trash,
            commands::settings_get,
            commands::settings_set,
            commands::trae_exe_from_running,
            commands::workbuddy_exe_from_running,
            commands::trae_cleanup_scan,
            commands::trae_cleanup_purge,
            commands::trae_cleanup_empty_trash,
            commands::trae_backup_inspect,
            commands::trae_backup_restore,
            commands::trae_delete_info,
            commands::trae_delete_session,
            commands::trae_delete_sessions,
            commands::trae_handoff_preview,
            commands::trae_handoff_write,
            commands::trae_oauth_start,
            commands::trae_oauth_status,
            commands::trae_oauth_stop,
            commands::trae_oauth_pending,
            commands::trae_oauth_manual,
            commands::trae_oauth_open_url,
            commands::trae_oauth_browsers,
            commands::trae_import_local_login,
            commands::trae_import_all_local_logins,
            // WorkBuddy 账号管理（国内版）
            commands::workbuddy_account_list,
            commands::workbuddy_account_add_token,
            commands::workbuddy_switch_precheck,
            commands::workbuddy_switch_to,
            commands::workbuddy_rollback,
            commands::workbuddy_import_local,
            commands::workbuddy_import_reference_accounts,
            commands::workbuddy_remove_account,
            commands::workbuddy_rename_account,
            commands::workbuddy_export_accounts,
            commands::workbuddy_preview_import,
            commands::workbuddy_import_accounts,
            commands::workbuddy_oauth_start,
            commands::workbuddy_oauth_poll,
            commands::workbuddy_oauth_stop,
            commands::workbuddy_open_url,
            commands::workbuddy_session_list_by_account,
            commands::workbuddy_session_list,
            commands::workbuddy_session_detail,
            commands::workbuddy_session_export_md,
            commands::workbuddy_session_delete,
            commands::workbuddy_session_copy_preview,
            commands::workbuddy_session_copy,
            commands::workbuddy_session_link_all,
            commands::workbuddy_session_link_existing,
            commands::workbuddy_session_link_all_preview,
            commands::workbuddy_session_links_preview,
            commands::workbuddy_session_groups,
            commands::workbuddy_session_group_detail,
            commands::workbuddy_session_group_candidates,
            commands::workbuddy_session_group_unify,
            commands::workbuddy_session_group_link_account,
            commands::workbuddy_session_group_link_session,
            commands::workbuddy_session_unlink,
            commands::workbuddy_session_edge_sync_dbs,
            // T46 · S4：WorkBuddy 会话编辑器
            commands::workbuddy_session_edit_doc,
            commands::workbuddy_session_edit_lines,
            commands::workbuddy_session_edit_targets,
            commands::workbuddy_session_edit_validate,
            commands::workbuddy_session_edit_save,
            // 首页总览 + WorkBuddy 积分
            commands::app_overview_snapshot,
            commands::app_overview_cached,
            commands::app_overview_save_reclaim,
            commands::workbuddy_credits_accounts,
            commands::workbuddy_credits_cached,
            commands::workbuddy_credits_query,
            commands::workbuddy_credits_one,
            commands::workbuddy_account_token,
            commands::workbuddy_account_token_refresh,
            // 自动更新
            commands::update_state,
            commands::update_check,
            commands::update_download,
            commands::update_restart,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app_handle, _event| {});
}
