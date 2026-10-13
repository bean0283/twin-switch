//! T61 真机取证：**「已并入、本机已无自己程序」的客户端不许再被当成活客户端**。
//!
//! 只读，不结束任何进程、不写任何文件（`scan()` 只写自己的缓存槽）。
//!
//! 修前症状（本机实测）：
//!   · `trae_cleanup` 的「正在运行」提示里出现 `TRAE SOLO CN（国内版）正在运行` —— 因为
//!     `trae_switch::is_running` 走 `process_names()`，对已合并的 SOLO 会**回退成承载方
//!     `Trae CN` 的映像名**，于是「开着 IDE」= 「SOLO 正在运行」；
//!   · 删 SOLO 的旧会话时，`kill_all(solo)` 会**把正在用的 `Trae CN` 一起杀掉**。
//!
//! 判据（本探针逐条打印）：
//!   1. `is_running` 与 `own_running` 在影子上必须**不一致**（不一致本身不是 bug，是根因）；
//!   2. `stop_for_maintenance` 对影子必须返回 `NoOwnProgram`（**一个进程都不碰**）；
//!   3. `trae_cleanup::scan()` 的 notes 里，影子**不许**出现「正在运行」，且要出现
//!      「已并入 … 只剩历史数据」；
//!   4. 真在用的客户端（`Trae CN`）**必须**仍按原样判运行（不许一刀切关掉提醒）。
//!
//! 跑：`cargo run -p wb-switch-core --example t61_shadow_probe`

use wb_switch_core::modules::{trae_cleanup, trae_discover, trae_switch};

fn main() {
    println!("=== T61 影子客户端取证（只读） ===\n");

    let clients = trae_discover::list_installed_clients();
    println!("{:<12} {:<22} {:>9} {:>8} {:>12} {:>10} {:>10}", "key", "label", "installed", "runnable", "merged_into", "is_running", "own_running");
    for c in &clients {
        let Some(client) = trae_discover::get_client(c.key) else { continue };
        let shadow = trae_discover::is_merged_shadow(c);
        println!(
            "{:<12} {:<22} {:>9} {:>8} {:>12} {:>10} {:>10}{}",
            c.key,
            c.label,
            c.installed,
            c.runnable,
            c.merged_into.unwrap_or("-"),
            trae_switch::is_running(client),
            trae_switch::is_running_own(client),
            if shadow { "   <= 影子（只剩历史数据）" } else { "" }
        );
    }

    println!("\n--- 1) 影子客户端上「为写库而结束进程」的判定 ---");
    for c in &clients {
        if !trae_discover::is_merged_shadow(c) {
            continue;
        }
        let Some(client) = trae_discover::get_client(c.key) else { continue };
        match trae_switch::stop_for_maintenance(client, 1) {
            Ok(trae_switch::MaintenanceStop::NoOwnProgram) => println!(
                "  ✅ {} ⇒ NoOwnProgram（不枚举也不结束任何进程，承载方 {} 安全）",
                c.label,
                trae_discover::merged_host_label(c).unwrap_or("?")
            ),
            other => println!("  ❌ {} ⇒ {other:?}（应恒为 NoOwnProgram）", c.label),
        }
    }
    println!("--- 2) 非影子的客户端必须保持原行为 ---");
    for c in &clients {
        if !c.installed || trae_discover::is_merged_shadow(c) {
            continue;
        }
        let Some(client) = trae_discover::get_client(c.key) else { continue };
        println!(
            "  · {} ⇒ is_running={} / own_running={}（两者应一致）",
            c.label,
            trae_switch::is_running(client),
            trae_switch::is_running_own(client)
        );
    }

    match trae_cleanup::scan() {
        Ok(v) => {
            println!("\n--- 3) 清理页 notes（影子不许出现「正在运行」）---");
            let notes = v["notes"].as_array().cloned().unwrap_or_default();
            if notes.is_empty() {
                println!("  （无 note）");
            }
            for n in &notes {
                println!("  · {}", n.as_str().unwrap_or(""));
            }
            let joined = notes
                .iter()
                .filter_map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let shadows: Vec<_> = clients
                .iter()
                .filter(|c| trae_discover::is_merged_shadow(c))
                .collect();
            for s in &shadows {
                // 只有**有数据目录**的影子才该出现在 notes 里（没装过的不必解释）。
                if !s.installed {
                    println!("  · {}（本机无数据目录，不参与）", s.label);
                    continue;
                }
                let lie = joined.contains(&format!("{} 正在运行", s.label));
                let honest = joined.contains(&format!("{} 已并入", s.label));
                println!(
                    "  {} {} ⇒ 谎报「正在运行」={}｜如实说明「已并入」={}",
                    if !lie && honest { "✅" } else { "❌" },
                    s.label,
                    lie,
                    honest
                );
            }
            if shadows.iter().all(|s| !s.installed) {
                println!("  （本机没有装过的影子客户端 —— 本轮的判据靠单测覆盖）");
            }

            println!("\n--- 4) 各类目条目数 ---");
            for cat in v["categories"].as_array().cloned().unwrap_or_default() {
                println!(
                    "  · {} :: {} 项 / {} MB",
                    cat["title"].as_str().unwrap_or("?"),
                    cat["count"].as_u64().unwrap_or(0),
                    cat["bytes_mb"].as_f64().unwrap_or(0.0)
                );
            }

            // T62 的落地点：整库备份按 (owner, stamp) 分批，清理顺序 = 从最旧的清。
            println!("\n--- 5) 工具残留逐条（T62：超限先清最旧的，每个 owner 最新一批永远保留）---");
            for cat in v["categories"].as_array().cloned().unwrap_or_default() {
                if cat["id"].as_str() != Some("tool_residue") {
                    continue;
                }
                if let Some(p) = cat.get("backup_policy") {
                    println!(
                        "  策略：{}｜启用={} 天数={} 份数={} 容量={}MB",
                        p["summary"].as_str().unwrap_or("-"),
                        p["enabled"], p["keepDays"], p["keepCount"], p["maxTotalMb"]
                    );
                }
                for it in cat["items"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "  · [{}] {:<58} {:>9.1} MB  超出策略={} 最旧={} 推荐={} {}",
                        it["kind"].as_str().unwrap_or("?"),
                        it["title"].as_str().unwrap_or("?"),
                        it["bytes_mb"].as_f64().unwrap_or(0.0),
                        it["over_policy"].as_bool().unwrap_or(false),
                        it["oldest"].as_bool().unwrap_or(false),
                        it["recommended"].as_bool().unwrap_or(false),
                        it["policy_reason"].as_str().unwrap_or("")
                    );
                }
            }

            // 客户端残留：按「标签 ·」前缀归堆，看看影子客户端占了多少（用户报的「solo 的影子」）。
            println!("\n--- 6) 客户端残留按客户端归堆 ---");
            let mut per: std::collections::BTreeMap<String, (usize, f64)> = Default::default();
            for cat in v["categories"].as_array().cloned().unwrap_or_default() {
                if cat["id"].as_str() != Some("client_residue") {
                    continue;
                }
                for it in cat["items"].as_array().cloned().unwrap_or_default() {
                    let t = it["title"].as_str().unwrap_or("").to_string();
                    let who = t.split(" · ").next().unwrap_or(&t).to_string();
                    let e = per.entry(who).or_insert((0, 0.0));
                    e.0 += 1;
                    e.1 += it["bytes_mb"].as_f64().unwrap_or(0.0);
                }
            }
            for (who, (n, mb)) in per {
                let shadow = clients
                    .iter()
                    .find(|c| c.label == who)
                    .map(trae_discover::is_merged_shadow)
                    .unwrap_or(false);
                println!("  · {:<24} {:>3} 项 / {:>8.1} MB{}", who, n, mb, if shadow { "   <= 影子（只剩历史数据）" } else { "" });
            }
        }
        Err(e) => println!("\n清理扫描失败：{e}"),
    }
}
