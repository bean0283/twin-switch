//! 本机排查：Trae 主程序到底被认成了哪个路径（对应「未找到主程序」类问题）。
//!
//! 用法（仓库根 `trae-switch-cn/`）：
//! ```text
//! cargo run --example trae_discover_probe
//! ```
//!
//! 它回答三件事：
//! 1. 四个客户端各自的自动探测结果（数据目录在不在、exe 找到没有、是手动指定还是自动探测）；
//! 2. 注册表卸载项里登记了哪些 Trae 安装路径（`probe_registry` 的原始输出）；
//! 3. 正在运行的客户端进程（`QueryFullProcessImageNameW` 拿到的真实 exe 路径）。
//!
//! ⚠️ 只读：不写设置、不启动进程、不碰账号库。

use wb_switch_core::modules::{settings, trae_discover};

fn main() {
    println!("=== 设置里手动指定的路径（settings.json 的 traeExe） ===");
    let s = settings::load();
    if s.trae_exe.is_empty() {
        println!("（空 —— 全部走自动探测）");
    } else {
        for (k, v) in s.trae_exe.iter() {
            println!("{k:<12} {v}  文件存在={}", std::path::Path::new(v).is_file());
        }
    }

    println!("\n=== 每个客户端的探测结果 ===");
    for c in trae_discover::list_installed_clients() {
        println!(
            "{:<10} 数据目录={:<5} runnable={:<5} 手动={:<5} exe={}",
            c.key,
            c.installed,
            c.runnable,
            c.exe_manual,
            c.exe.clone().unwrap_or_else(|| "未找到".into())
        );
        println!("           数据目录: {}", c.user_data_dir);
    }

    println!("\n=== 注册表卸载项里登记的 Trae 路径 ===");
    let reg = trae_discover::registry_candidates();
    if reg.is_empty() {
        println!("（一条都没有 —— 安装包没登记 DisplayIcon/InstallLocation）");
    } else {
        for p in reg.iter() {
            println!("{}", p.display());
        }
    }

    println!("\n=== 正在运行的客户端（按进程取真实路径） ===");
    for key in ["trae-cn", "solo-cn", "trae-intl", "solo-intl"] {
        match trae_discover::exe_from_running_lenient(key) {
            Some(p) => println!("{key:<10} {}", p.display()),
            None => println!("{key:<10} 没有在运行"),
        }
    }
}
