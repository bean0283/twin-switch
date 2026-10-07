// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
use tauri::{Manager, WindowEvent};
mod commands;
mod update_service;
#[cfg(target_os = "macos")]
mod instance_lock;

/// 右下角托盘：关闭窗口时隐藏到托盘（不退出），托盘菜单可恢复 / 真正退出。
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().expect("缺省窗口图标").clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
            "quit" => app.exit(0),
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
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
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
            commands::trae_identify_live,
            commands::trae_backup_account,
            commands::trae_switch_to,
            commands::trae_rollback_to,
            commands::trae_remove_account,
            commands::trae_rename_account,
            commands::trae_export_account,
            commands::trae_import_account,
            commands::trae_saved_key,
            commands::trae_scan_and_decrypt,
            commands::trae_decrypt_with_saved_key,
            commands::trae_decrypted_status,
            commands::trae_ensure_decrypted,
            commands::trae_cleanup_working_files,
            commands::trae_list_sessions,
            commands::trae_session_detail,
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
            commands::trae_cleanup_scan,
            commands::trae_cleanup_purge,
            commands::trae_cleanup_empty_trash,
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
            commands::workbuddy_switch_precheck,
            commands::workbuddy_switch_to,
            commands::workbuddy_rollback,
            commands::workbuddy_import_local,
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
            commands::workbuddy_session_links_preview,
            commands::workbuddy_session_unlink,
            commands::workbuddy_session_edge_sync_dbs,
            // 首页总览 + WorkBuddy 积分
            commands::app_overview_snapshot,
            commands::app_overview_cached,
            commands::app_overview_save_reclaim,
            commands::workbuddy_credits_accounts,
            commands::workbuddy_credits_cached,
            commands::workbuddy_credits_query,
            commands::workbuddy_credits_one,
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
