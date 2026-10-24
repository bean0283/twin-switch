// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `--self-check`：无头自检，不开窗口、不碰任何写类命令，跑完直接退出。
    //
    // ⚠️ 必须放在 `run()` **之前**：一旦进了 Tauri 的 builder，建窗口 / 起托盘 / 初始化
    //    WebView 都要钱，而自检的意义就是「秒级回答这台机器有没有问题」。
    if std::env::args().any(|a| a == "--self-check" || a == "-self-check") {
        std::process::exit(wb_switch_rust_lib::selfcheck::run());
    }
    wb_switch_rust_lib::run()
}
