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

    // `--sql <文件>`：对实时库执行一批维护 SQL（解密 → 执行 → 增量回写 → 重启）。
    if std::env::args().any(|a| a == "--sql") {
        let path = std::env::args()
            .skip_while(|a| a != "--sql")
            .nth(1)
            .unwrap_or_default();
        let body = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("读取 SQL 文件失败：{e}");
                std::process::exit(1);
            }
        };
        let stmts: Vec<String> = body
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.starts_with("--"))
            .collect();
        let code = match wb_switch_core::modules::trae_import::apply_sql(
            "trae-cn",
            &stmts,
            Some(&|m| println!("{m}")),
        ) {
            Ok(v) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
                );
                0
            }
            Err(e) => {
                eprintln!("失败：{e}");
                1
            }
        };
        std::process::exit(code);
    }

    // `--clone-diag <源会话id> [标题]`：把能打开的会话整份复制成新会话，用于定位加载失败。
    if std::env::args().any(|a| a == "--clone-diag") {
        let mut it = std::env::args().skip_while(|a| a != "--clone-diag");
        it.next();
        let src = it.next().unwrap_or_default();
        let title = it.next().unwrap_or_else(|| "克隆诊断会话".to_string());
        let code = match wb_switch_core::modules::trae_import::clone_session_diag(
            "trae-cn",
            &src,
            &title,
            Some(&|m| println!("{m}")),
        ) {
            Ok(v) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
                );
                0
            }
            Err(e) => {
                eprintln!("失败：{e}");
                1
            }
        };
        std::process::exit(code);
    }

    // `--snapshot [client]`：把实时库解密成只读分析快照（不结束客户端、不写线上库）。
    if std::env::args().any(|a| a == "--snapshot" || a == "-snapshot") {
        let client = std::env::args()
            .skip_while(|a| a != "--snapshot" && a != "-snapshot")
            .nth(1)
            .filter(|a| !a.starts_with('-'))
            .unwrap_or_else(|| "trae-cn".to_string());
        let code = match wb_switch_core::modules::trae_import::write_analysis_snapshot(&client) {
            Ok(v) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
                );
                0
            }
            Err(e) => {
                eprintln!("失败：{e}");
                1
            }
        };
        std::process::exit(code);
    }

    // `--heal-usage [client]`：把导入时写成 0 的 history_v2.token_usage 补成估算值。
    //
    // 为什么要有：这类会话点开只有「会话加载失败」（lite 报 995001），而根因是导入器
    // 把用量列写成了 0。修复要结束客户端、解密、增量回写，走 GUI 反而更麻烦。
    if std::env::args().any(|a| a == "--heal-usage" || a == "-heal-usage") {
        let client = std::env::args()
            .skip_while(|a| a != "--heal-usage" && a != "-heal-usage")
            .nth(1)
            .filter(|a| !a.starts_with('-'))
            .unwrap_or_else(|| "trae-cn".to_string());
        println!("修复客户端 {client} 的零用量历史行…");
        let code = match wb_switch_core::modules::trae_import::heal_zero_token_usage(
            &client,
            Some(&|m| println!("{m}")),
        ) {
            Ok(v) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
                );
                0
            }
            Err(e) => {
                eprintln!("失败：{e}");
                1
            }
        };
        std::process::exit(code);
    }
    wb_switch_rust_lib::run()
}
