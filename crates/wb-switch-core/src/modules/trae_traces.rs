//! 「登录痕迹」的按账号清点与清理（issue #19）。
//!
//! ## 痕迹到底存在哪
//!
//! Trae 的账号痕迹有两处，性质完全不同：
//!
//! | 位置 | 能否按账号区分 |
//! | --- | --- |
//! | `ModularData/ai-agent/database.db`（加密库）里的 `user_configuration` / `model_config_cache` / `project` / `chat_session` | **能** —— 都带 `user_id` |
//! | `ModularData/ai-agent/command-execution/<hash>/execution.json.ownerHash` | **不能** —— 是盐化哈希，实测反推不出 uid |
//!
//! 所以本模块做两件事：
//!   - [`scan`]：把库里有痕迹的账号全列出来（uid、会话数、配置行数、是否当前登录）；
//!   - [`purge`]：按勾选的 uid 删行（可选连会话一起删），命令执行记录只能整体清。
//!
//! ## 写库纪律（与 `trae_delete` 同一套）
//!
//! 结束客户端 → 取密钥 → 解密实时库 → 合并 WAL → **单事务删行** → 增量加密回写
//! → 页级备份 → 原子替换 → 提升明文快照 → 重启客户端。
//!
//! ⚠️ 绝不直接改实时库：它是 SQLCipher 逐页加密的，乱写一页就整库打不开。

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use crate::modules::config::store_dir;
use crate::modules::trae_delete::{merge_wal_into_plain, resolve_live_key};
use crate::modules::trae_discover::{database_path, get_client};
use crate::modules::trae_export::{decrypted_db_path, ensure_decrypted, promote_snapshot};
use crate::modules::trae_switch::{stop_for_maintenance, MaintenanceStop};

/// 带 `user_id` 的“痕迹表”：删掉这些行，客户端就不再认为这台机器用过该账号。
///
/// 刻意**不含** `server_history_info`（它 5 万多行且 `user_id` 全为 NULL，
/// 是会话镜像的另一套存储，删它等于删会话正文）。
const TRACE_TABLES: &[&str] = &["user_configuration", "model_config_cache", "local_artifact"];

/// 单个账号在库里的痕迹统计。
#[derive(Default)]
struct Row {
    uid: String,
    config: i64,
    model_cache: i64,
    artifacts: i64,
    projects: i64,
    sessions: i64,
}

fn bump(map: &mut BTreeMap<String, Row>, uid: &str, f: impl FnOnce(&mut Row)) {
    let e = map.entry(uid.to_string()).or_insert_with(|| Row {
        uid: uid.to_string(),
        ..Default::default()
    });
    f(e);
}

/// 列出本机有痕迹的账号（只读，走解密快照）。
pub fn scan(client_key: &str) -> Result<Value, String> {
    let client = get_client(client_key).ok_or("未知客户端")?;
    if !database_path(client).exists() {
        return Err(format!("{} 还没有会话库（先登录一次）", client.label));
    }
    // 快照要与实时库对齐，否则刚登录的号看不到。
    ensure_decrypted(client_key, None)?;
    let snap = decrypted_db_path(client_key);
    let conn = Connection::open_with_flags(&snap, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开解密快照失败：{e}"))?;

    let mut map: BTreeMap<String, Row> = BTreeMap::new();

    for (table, field) in [
        ("user_configuration", 0usize),
        ("model_config_cache", 1),
        ("local_artifact", 2),
    ] {
        let sql = format!("select user_id, count(*) from \"{table}\" group by user_id");
        let Ok(mut stmt) = conn.prepare(&sql) else { continue };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, i64>(1)?))
        }) else {
            continue;
        };
        for row in rows.flatten() {
            let (uid, n) = row;
            if uid.trim().is_empty() {
                continue;
            }
            bump(&mut map, &uid, |e| match field {
                0 => e.config = n,
                1 => e.model_cache = n,
                _ => e.artifacts = n,
            });
        }
    }

    // 项目数 + 会话数（会话挂在 project 上，按 project.user_id 归属）
    if let Ok(mut stmt) = conn.prepare("select user_id, count(*) from project group by user_id") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, i64>(1)?))
        }) {
            for (uid, n) in rows.flatten() {
                if uid.trim().is_empty() {
                    continue;
                }
                bump(&mut map, &uid, |e| e.projects = n);
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare(
        "select p.user_id, count(s.session_id) from project p \
         join chat_session s on s.project_id = p.project_id group by p.user_id",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, i64>(1)?))
        }) {
            for (uid, n) in rows.flatten() {
                if uid.trim().is_empty() {
                    continue;
                }
                bump(&mut map, &uid, |e| e.sessions = n);
            }
        }
    }

    let live_uid = crate::modules::trae_switch::describe_account(client_key)
        .get("uid")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut accounts: Vec<Value> = map
        .values()
        .map(|r| {
            json!({
                "uid": r.uid,
                "label": crate::modules::trae_vault::account_label(client_key, &r.uid),
                "suffix": r.uid.chars().rev().take(6).collect::<String>().chars().rev().collect::<String>(),
                "config": r.config,
                "modelCache": r.model_cache,
                "artifacts": r.artifacts,
                "projects": r.projects,
                "sessions": r.sessions,
                "isCurrent": !live_uid.is_empty() && r.uid == live_uid,
            })
        })
        .collect();
    // 当前登录的排最前，其余按痕迹多少倒序 —— 用户最先要看的是「这个还在用」。
    accounts.sort_by(|a, b| {
        let cur = |v: &Value| v["isCurrent"].as_bool().unwrap_or(false);
        cur(b)
            .cmp(&cur(a))
            .then_with(|| {
                let n = |v: &Value| v["sessions"].as_i64().unwrap_or(0) + v["config"].as_i64().unwrap_or(0);
                n(b).cmp(&n(a))
            })
    });

    let (cmd_dirs, cmd_files, cmd_bytes) = crate::modules::trae_reset::scan_command_traces(client);
    Ok(json!({
        "ok": true,
        "clientKey": client.key,
        "clientLabel": client.label,
        "accounts": accounts,
        "total": accounts.len(),
        "liveUid": if live_uid.is_empty() { Value::Null } else { json!(live_uid) },
        // 命令执行记录没有账号维度，只能整体清（ownerHash 反推不出 uid）
        "commandTraces": { "dirs": cmd_dirs, "files": cmd_files, "bytes": cmd_bytes },
        "note": "列表来自本地会话库的 user_id；命令执行记录（ownerHash）无法按账号归因，只能整体清",
    }))
}

/// 按 uid 清痕迹。`with_sessions = true` 时连该账号的会话、项目一起删。
pub fn purge(
    client_key: &str,
    uids: &[String],
    with_sessions: bool,
    with_command_log: bool,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    let client = get_client(client_key).ok_or("未知客户端")?;
    let want: Vec<String> = uids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .collect();
    if want.is_empty() {
        return Err("没有选择任何账号".into());
    }

    let live = database_path(client);
    if !live.exists() {
        return Err(format!("实时加密库不存在：{}", live.display()));
    }
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let work = store_dir()
        .join("trae")
        .join("trace_tmp")
        .join(format!("{client_key}-{stamp}"));
    let cleanup = |w: &Path| {
        let _ = std::fs::remove_dir_all(w);
    };

    // 0. 密钥
    log("校验实时库密钥…");
    let enc_key_hex = match resolve_live_key(client_key, &live) {
        Ok(k) => k,
        Err(e) => {
            cleanup(&work);
            return Err(e);
        }
    };

    // 1. 停客户端
    match stop_for_maintenance(client, 10_000) {
        Ok(MaintenanceStop::NotRunning) => {}
        Ok(MaintenanceStop::NoOwnProgram) => log(&format!(
            "{} 已并入其他客户端、本机没有它自己的程序：不结束任何进程",
            client.label
        )),
        Ok(MaintenanceStop::Stopped(n)) => log(&format!("已结束 {n} 个进程…")),
        Err(e) => {
            cleanup(&work);
            return Err(e);
        }
    }

    // 2. 解密 + 合并 WAL
    if let Err(e) = std::fs::create_dir_all(&work) {
        return Err(format!("创建临时目录失败：{e}"));
    }
    let plain = work.join("target-plain.db");
    log("解密实时库（可能数百 MB，请稍候）…");
    if let Err(e) = crate::modules::trae_decrypt::decrypt_database(
        &live,
        &enc_key_hex,
        &plain,
        Some(&|m| log(&format!("   {m}"))),
    ) {
        cleanup(&work);
        return Err(e);
    }
    if let Err(e) = merge_wal_into_plain(&plain, &live.with_extension("db-wal"), &enc_key_hex) {
        cleanup(&work);
        return Err(e);
    }
    if let Err(e) = crate::modules::trae_import::patch_reserved_field(&plain) {
        cleanup(&work);
        return Err(e);
    }

    // 3. 删行
    let stats = {
        let mut conn = match Connection::open(&plain) {
            Ok(c) => c,
            Err(e) => {
                cleanup(&work);
                return Err(format!("打开明文副本失败：{e}"));
            }
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => {
                cleanup(&work);
                return Err(format!("开启事务失败：{e}"));
            }
        };
        let mut out = serde_json::Map::new();
        let mut total = 0i64;

        // 3a) 会话（可选）：逐会话走 `trae_delete` 的表分类，避免漏表
        if with_sessions {
            let mut sessions: Vec<String> = Vec::new();
            if let Ok(mut stmt) = tx.prepare(
                "select s.session_id from chat_session s \
                 join project p on s.project_id = p.project_id where p.user_id = ?1",
            ) {
                if let Ok(rows) = stmt.query_map([&want[0]], |r| r.get::<_, String>(0)) {
                    sessions.extend(rows.flatten());
                }
            }
            // 多账号时逐个查（IN 列表拼字符串不安全）
            for uid in want.iter().skip(1) {
                if let Ok(mut stmt) = tx.prepare(
                    "select s.session_id from chat_session s \
                     join project p on s.project_id = p.project_id where p.user_id = ?1",
                ) {
                    if let Ok(rows) = stmt.query_map([uid], |r| r.get::<_, String>(0)) {
                        sessions.extend(rows.flatten());
                    }
                }
            }
            let mut n = 0usize;
            for sid in &sessions {
                if crate::modules::trae_delete::delete_rows_in_conn(&tx, sid).is_ok() {
                    n += 1;
                }
            }
            out.insert("sessions".into(), json!(n));
            total += n as i64;
            log(&format!("已删除 {n} 个会话"));
        }

        // 3b) 项目 + 配置 / 缓存 / 附件
        for uid in &want {
            if with_sessions {
                if let Ok(k) = tx.execute("delete from project where user_id = ?1", [uid]) {
                    total += k as i64;
                }
            }
            for t in TRACE_TABLES {
                match tx.execute(&format!("delete from \"{t}\" where user_id = ?1"), [uid]) {
                    Ok(k) => {
                        total += k as i64;
                        out.insert((*t).to_string(), json!(k));
                    }
                    // 表可能不存在（客户端版本差异）：跳过，不把整次清理判失败
                    Err(_) => {}
                }
            }
        }

        if let Err(e) = tx.commit() {
            cleanup(&work);
            return Err(format!("提交失败：{e}"));
        }
        out.insert("rows".into(), json!(total));
        Value::Object(out)
    };

    // 4~7. 增量回写 / 备份 / 替换 / 提升快照（与 trae_delete 同一套）
    let new_db = work.join("target-new.db");
    log("增量加密回写…");
    if let Err(e) = crate::modules::trae_import::write_db_incremental(
        &enc_key_hex,
        &live,
        &plain,
        &new_db,
        Some(&|m| log(&format!("   {m}"))),
    ) {
        cleanup(&work);
        return Err(e);
    }
    let bdir = crate::modules::trae_export::backup_dir();
    let entry = bdir.join(crate::modules::trae_backup::entry_name(client_key, "trace", &stamp));
    match crate::modules::trae_backup::snapshot_before_write(
        &live,
        &new_db,
        &entry,
        client_key,
        "trace",
        "清理登录痕迹前的整库备份",
    ) {
        Ok(out) => log(&format!("   {}", crate::modules::trae_backup::describe(&out))),
        Err(e) => {
            cleanup(&work);
            return Err(format!("备份原库失败，已中止：{e}"));
        }
    }
    if let Err(e) = crate::modules::trae_backup::swap_in_place(&new_db, &live) {
        cleanup(&work);
        return Err(format!("替换数据库失败（原库未被改动）：{e}"));
    }
    if let Err(e) = promote_snapshot(client_key, &plain) {
        log(&format!("快照更新失败（不影响结果）：{e}"));
        crate::modules::trae_export::drop_snapshot_meta(client_key);
    }

    // 8. 命令执行记录（只能整体清）
    let (cmd_removed, cmd_bytes) = if with_command_log {
        match crate::modules::trae_reset::clean_command_traces(client) {
            Ok((n, b)) => {
                log(&format!("已清理 {n} 条 AI 命令执行记录"));
                (n, b)
            }
            Err(e) => {
                log(&format!("清理命令执行记录失败：{e}"));
                (0, 0)
            }
        }
    } else {
        (0, 0)
    };

    let _ = std::fs::remove_dir_all(&work);
    // 9. 客户端会自己重启（quit 时用的是 stop_for_maintenance，不负责拉起）
    if let Err(e) = crate::modules::trae_switch::launch(client_key) {
        log(&format!("客户端未能自动启动：{e}"));
    }

    Ok(json!({
        "ok": true,
        "uids": want,
        "withSessions": with_sessions,
        "stats": stats,
        "commandTracesRemoved": cmd_removed,
        "commandTracesBytes": cmd_bytes,
        "backup": entry.to_string_lossy(),
        "note": "只动本地库；账号档案（本工具账号库）不受影响，切回去照样能用",
    }))
}
