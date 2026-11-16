//! 「关联会话」加载性能基准：对比新旧 find_jsonl 与整页 e2e 耗时。
//!
//! 只读。用法：`cargo run -p wb-switch-core --example wb_groups_bench`
//!
//! - `old_find`：逐目录 walk 的旧实现（每次 read_dir + 逐目录 stat），复刻修复前代码
//! - `new_find`：带索引的 find_jsonl（首次调用会建索引，之后 O(1) 查表）
//! - `list_sessions`：会话列表 e2e（含同标题注记比对）
//! - `groups::list`：关联会话页后端全链路（列表 + 组装配 + payload）

use std::time::Instant;
use wb_switch_core::modules::{workbuddy_session_groups as groups, workbuddy_source};

fn main() {
    let t0 = Instant::now();
    let list = workbuddy_source::list_sessions().expect("list_sessions 失败");
    let t_list_cold = t0.elapsed();
    let ids: Vec<String> = list.iter().map(|s| s.id.clone()).collect();
    println!(
        "[1] list_sessions()            冷启动（含索引构建）: {:>10.1?}  ({} 个会话)",
        t_list_cold,
        ids.len()
    );

    // 旧实现复刻：对每个 id 都走一遍目录（修复前的真实成本）
    let t0 = Instant::now();
    let mut hit_old = 0usize;
    for id in &ids {
        if old_find_jsonl(id).is_some() {
            hit_old += 1;
        }
    }
    let t_old = t0.elapsed();
    println!(
        "[2] find_jsonl × {}（旧·逐目录 walk）: {:>10.1?}  (命中 {})",
        ids.len(),
        t_old,
        hit_old
    );

    // 新实现：第一次调用建索引，之后查表（含命中 is_file 校验）
    let t0 = Instant::now();
    let mut hit_new = 0usize;
    for id in &ids {
        if workbuddy_source::find_jsonl(id).is_some() {
            hit_new += 1;
        }
    }
    let t_new = t0.elapsed();
    println!(
        "[3] find_jsonl × {}（新·索引查表）  : {:>10.1?}  (命中 {})  —— 提升约 {:.0}×",
        ids.len(),
        t_new,
        hit_new,
        t_old.as_secs_f64() / t_new.as_secs_f64().max(1e-9)
    );

    // 热索引下再来一次 e2e（模拟「刷新」）
    let t0 = Instant::now();
    let _ = workbuddy_source::list_sessions().expect("list_sessions 二次失败");
    println!(
        "[4] list_sessions()            热索引   : {:>10.1?}",
        t0.elapsed()
    );

    // 关联会话页后端全链路（前端打开该 tab 调的就是它）
    let t0 = Instant::now();
    match groups::list() {
        Ok(v) => {
            let n = v.get("items").and_then(|g| g.as_array()).map(|a| a.len());
            println!(
                "[5] groups::list()             页面全链路: {:>10.1?}  ({} 组)",
                t0.elapsed(),
                n.unwrap_or(0)
            );
        }
        Err(e) => println!("[5] groups::list() 失败: {e}"),
    }

    // 分解 [5] 里 list_sessions 之外的独立环节
    let t0 = Instant::now();
    let _ = workbuddy_source::snapshot_db();
    println!(
        "[6] snapshot_db()              库快照   : {:>10.1?}",
        t0.elapsed()
    );
    let t0 = Instant::now();
    let running = wb_switch_core::modules::workbuddy_switch::is_running();
    println!(
        "[7] is_running()               进程探测 : {:>10.1?}  (running={})",
        t0.elapsed(),
        running
    );
}

/// 修复前的 find_jsonl：每次调用都完整走一遍 projects/。
fn old_find_jsonl(session_id: &str) -> Option<std::path::PathBuf> {
    let projects = dirs_root().join("projects");
    if !projects.is_dir() {
        return None;
    }
    let file_name = format!("{session_id}.jsonl");
    let direct = projects.join(&file_name);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(&projects).ok()?.flatten() {
        let candidate = entry.path().join(&file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 与 core 的 `data_root()` 同口径（home/.workbuddy）。
fn dirs_root() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("找不到用户目录");
    std::path::PathBuf::from(home).join(".workbuddy")
}
