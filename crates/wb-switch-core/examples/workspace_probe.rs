//! 只读探针：打印 Trae 各客户端会话的「工作空间」字段，用于核对
//! `trae_export::list_sessions()` 新增的 `workspace / workspace_key / workspace_path`
//! 在真机上是否取到了有意义的值（`project.name` 优先，其次 `absolute_path` 的目录名）。
//!
//! 用法：`cargo run -p wb-switch-core --example workspace_probe`

fn main() {
    let clients = ["trae-cn", "trae-global", "solo-cn"];
    for ck in clients {
        match wb_switch_core::modules::trae_export::list_sessions(ck) {
            Ok(list) => {
                println!("== {ck}: {} 个会话", list.len());
                for s in list.iter().take(10) {
                    println!(
                        "   [{:>2}字] {:<28} <- {}",
                        s.workspace.chars().count(),
                        s.workspace,
                        if s.workspace_path.is_empty() {
                            "（无路径）"
                        } else {
                            &s.workspace_path
                        }
                    );
                }
                let empty = list.iter().filter(|s| s.workspace_key.is_empty()).count();
                if empty > 0 {
                    println!("   ⚠️ 有 {empty} 条没有空间键（project 缺失或路径为空）");
                }
            }
            Err(e) => println!("== {ck}: 读不到 —— {e}"),
        }
    }
}
