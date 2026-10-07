//! **只读**探针：用真实数据验证「跨账号关联同步」的归属校验是否已经修好。
//!
//! 背景：`sync_group` 曾经 100% 报「被覆盖的会话已不在预期账号下」。根因是
//! 归属校验与写库各写了一套 uid —— 校验按 role 写死取 `src_uid`，而跨账号时
//! 两端 uid 必然不同，于是每次都判失败。
//!
//! 判定依据：`diverged_groups` 里每一端的 `alive` 就是「该会话存在且
//! `owner_uid == 期望 uid`」（见 `member_view`），与 `sync_group` 的校验等价。
//! 所以只要这里能捞出 `verdict == "linked"` 的分叉组，就说明
//! 同步的归属校验**现在会通过**。
//!
//! ⚠️ 全程只读（解密到临时目录），不写会话库、不动关联表。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example sync_divergence_probe -- trae-cn

use std::collections::HashSet;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let client_keys: Vec<String> = if args.is_empty() {
        wb_switch_core::modules::trae_discover::list_installed_clients()
            .iter()
            .map(|c| c.key.to_string())
            .collect()
    } else {
        args
    };
    if client_keys.is_empty() {
        println!("没有发现 Trae 客户端");
        return;
    }

    for ck in client_keys {
        println!("\n=== 客户端 {ck} ===");
        let sessions = match wb_switch_core::modules::trae_export::list_sessions(&ck) {
            Ok(v) => v,
            Err(e) => {
                println!("  读会话失败（需要密钥 / 客户端在跑）：{e}");
                continue;
            }
        };
        // 只挑有归属的账号（关联按 uid 记录，空 uid 不算）。
        let mut uids: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for s in &sessions {
            let uid = s.owner_uid.trim().to_string();
            if uid.is_empty() || !seen.insert(uid.clone()) {
                continue;
            }
            uids.push(uid);
        }
        println!("  会话 {} 个，有归属账号 {} 个", sessions.len(), uids.len());
        if uids.is_empty() {
            continue;
        }

        let mut total = 0usize;
        let mut reviewed: HashSet<String> = HashSet::new();
        for uid in &uids {
            let v = wb_switch_core::modules::trae_session_links::diverged_groups(&ck, uid);
            if v["ok"] != serde_json::Value::Bool(true) {
                println!(
                    "  [{}] 查询失败：{}",
                    &uid[..uid.len().min(8)],
                    v["error"].as_str().unwrap_or("未知错误")
                );
                continue;
            }
            let groups = v["groups"].as_array().cloned().unwrap_or_default();
            for g in groups {
                // 同一个组会被两端各命中一次，按 groupId 去重只审一遍。
                if !reviewed.insert(g["groupId"].as_str().unwrap_or("").to_string()) {
                    continue;
                }
                total += 1;
                let s_alive = g["source"]["alive"].as_bool().unwrap_or(false);
                let t_alive = g["target"]["alive"].as_bool().unwrap_or(false);
                let title = g["title"].as_str().unwrap_or("(无标题)");
                let partner = g["partnerLabel"].as_str().unwrap_or("");
                let verdict = g["verdict"].as_str().unwrap_or("?");
                let divergence = g["divergence"].as_str().unwrap_or("?");
                let broken = g["broken"].as_bool().unwrap_or(false);
                let suggested = g["suggestedDirection"].as_str().unwrap_or("<null:不给建议>");
                println!(
                    "  · {} | {} 条 vs {} 条 | divergence={} | broken={} | suggested={} | 对端: {} | src_alive={} tgt_alive={}",
                    title,
                    g["source"]["messages"].as_i64().unwrap_or(-1),
                    g["target"]["messages"].as_i64().unwrap_or(-1),
                    divergence,
                    broken,
                    suggested,
                    partner,
                    s_alive,
                    t_alive
                );
                for (who, key) in [("源", "source"), ("目标", "target")] {
                    let issues = g[key]["integrity"].as_array().cloned().unwrap_or_default();
                    for i in issues {
                        println!("      ⚠️ {who}端自检：{}", i.as_str().unwrap_or("?"));
                    }
                }
                if verdict != "linked" || !s_alive || !t_alive {
                    println!("      ⚠️ 该组两端未同时存活，同步仍会被拒（预期行为）");
                }
            }
        }
        println!(
            "  → 去重后 {} 个需要处理的关联组（分叉 或 副本数据错位）",
            total
        );
    }
}
