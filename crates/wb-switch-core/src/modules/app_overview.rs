//! 首页「本机概览」：把分散在 Trae / WorkBuddy 两侧的状态汇总成一份快照。
//!
//! **只读**：本模块只做统计，不写任何客户端数据、不触发解密、不发起网络请求。
//! 需要联网的东西（积分）走 [`crate::modules::workbuddy_credits::cached`]，只读缓存；
//! 需要遍历磁盘的东西（可回收空间）由前端另行调用两边的 `*_cleanup_scan` 并行拉取。
//!
//! 之所以把这两块拆出去，是因为首页必须**秒开**：
//! - 解密快照可能有 300 MB 量级，`list_sessions` 在 WAL 变动时会复制整份快照；
//! - 清理扫描要递归统计几 GB 目录；
//! - 进程枚举（WMI）本身也要几百毫秒到数秒。
//!
//! 因此这里 Trae 的会话数直接读**快照元信息里 `chat_session` 表的行数**（文件级读取，毫秒级），
//! 并附上 `current` 标记说明这个数字是否仍然等于实时库。
//!
//! ## 磁盘缓存（首屏加速）
//!
//! 即便做了上面的裁剪，全量 [`snapshot`] 仍然要跑进程枚举 + SQLite 统计，冷启动可能几百毫秒到数秒。
//! 所以把它整体落盘到 `~/.twin-switch/cache/overview.json`：
//!
//! - [`cached`]：**只读文件**，毫秒级返回上次的结果 + `ageMs`，前端拿到先渲染，页面立刻可操作；
//! - [`snapshot`]：算完之后**立刻写回缓存**，下次启动就是热的；
//! - [`save_reclaim`]：把前端并行扫出来的可回收空间一起并进缓存（下次连扫描结果也是热的）。
//!
//! 缓存是纯派生数据，随时可以删；删了只是回到「首屏要等一下」。

use serde_json::{json, Value};
use std::path::Path;

use crate::modules::{
    client_usage, config, trae_credits, trae_discover, trae_export, trae_switch, trae_vault,
    workbuddy_accounts, workbuddy_auth, workbuddy_credits, workbuddy_source, workbuddy_switch,
    workbuddy_vault,
};

/// Trae 会话表名（会话列表就是它）。
const TRAE_SESSION_TABLE: &str = "chat_session";

/// 缓存文件名（落在 `config::cache_dir()` 下）。
const CACHE_NAME: &str = "overview.json";

/// 缓存结构版本。字段一改就 +1，旧缓存直接当作不存在，避免前端读到半新半旧的对象。
const CACHE_VERSION: u64 = 2;

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Trae 侧
// ---------------------------------------------------------------------------

fn trae_client_overview(c: &trae_discover::InstalledClient) -> Value {
    let Some(client) = trae_discover::get_client(c.key) else {
        return json!({ "key": c.key, "label": c.label, "installed": false });
    };
    let processes = trae_switch::list_processes(client);
    let accounts = trae_vault::list_vault_accounts(c.key);

    let snap = trae_export::decrypted_db_path(c.key);
    let snap_exists = snap.is_file();
    let meta = trae_export::read_snapshot_meta(c.key);
    let current = match (&meta, snap_exists) {
        (Some(m), true) => trae_export::snapshot_is_current(c.key, &snap, m),
        _ => false,
    };
    // 会话数取快照元信息，避免为了首页去复制整份快照。
    let session_count = meta
        .as_ref()
        .and_then(|m| m.tables.iter().find(|t| t.name == TRAE_SESSION_TABLE))
        .map(|t| t.count);

    json!({
        "key": c.key,
        "label": c.label,
        "installed": c.installed,
        "exe": c.exe,
        "userDataDir": c.user_data_dir,
        "hasLogin": c.has_login,
        // 当前登录账号（真实昵称 + uid 尾号）。未登录 / 读不出登录态 ⇒ null。
        "loginLabel": if c.has_login { live_login_label(c.key, client) } else { None },
        "running": !processes.is_empty(),
        "processCount": processes.len(),
        "accounts": accounts.len(),
        // ⚠️ 每个客户端的会话 / 账号 / 积分都**只从它自己的库与它自己的账号库算** ——
        //    Trae 的 4 个客户端各有独立的 database.db，合并统计会得到一个谁也不对应的数。
        "credits": trae_client_credits(c.key),
        "decrypted": {
            "exists": snap_exists,
            "current": current,
            "path": snap.to_string_lossy(),
            "pages": meta.as_ref().map(|m| m.pages).unwrap_or(0),
            "tableCount": meta.as_ref().map(|m| m.tables.len()).unwrap_or(0),
            "createdMs": meta.as_ref().map(|m| m.created_ms).unwrap_or(0),
            "sessionCount": session_count,
            "dbBytes": file_len(&snap),
        },
    })
}

/// 当前登录账号的显示名（客户端 `storage.json` 里那个 uid → 账号库里的真实昵称）。
///
/// 只读两个几 KB 的 JSON，毫秒级；未登录 / 解不出 Km 一律 `None`（不报错）。
fn live_login_label(client_key: &str, client: &trae_discover::TraeClient) -> Option<String> {
    let text = std::fs::read_to_string(trae_discover::storage_json_path(client)).ok()?;
    let uid = trae_vault::uid_from_storage_text(&text)?;
    Some(trae_vault::account_label(client_key, &uid))
}

/// 单个客户端的积分摘要（**只读账号库里的 `profile.json`，不联网**）。
///
/// 首页必须秒开，所以这里绝不触发接口调用；界面上的「查询积分」按钮才真去拉。
/// 返回的是压缩过的摘要（不含逐包明细），避免把逐账号明细塞进总览缓存文件。
fn trae_client_credits(client_key: &str) -> Value {
    let res = trae_credits::cached(Some(client_key.to_string()));
    let empty = Vec::new();
    let accounts = res["accounts"].as_array().unwrap_or(&empty);
    let queryable = accounts
        .iter()
        .filter(|a| a["queryable"] == json!(true))
        .count();
    // 「有数据」= 曾经成功拉到过积分（profile.json 里 credit_ok = true）。
    let with_data = accounts
        .iter()
        .filter(|a| a["ok"] == json!(true))
        .count();
    let updated = accounts
        .iter()
        .filter_map(|a| a["updatedAt"].as_i64())
        .max()
        .unwrap_or(0);
    json!({
        "accountCount": accounts.len(),
        "queryable": queryable,
        "withData": with_data,
        "totalRemaining": res["summary"]["totalRemaining"].clone(),
        "updatedAt": if updated > 0 { json!(updated) } else { Value::Null },
    })
}

fn trae_overview() -> Value {
    // 与 `trae_list_clients` 同一套排序：首页与账号页看到的客户端顺序必须一致，
    // 否则用户会以为「换个页面顺序就变了」。
    let mut installed = trae_discover::list_installed_clients();
    client_usage::sort_installed(&mut installed);
    let clients: Vec<Value> = installed
        .iter()
        .filter(|c| c.installed)
        .map(trae_client_overview)
        .collect();
    let account_total: u64 = clients
        .iter()
        .filter_map(|c| c["accounts"].as_u64())
        .sum();
    let running = clients.iter().filter(|c| c["running"] == json!(true)).count();
    let session_total: u64 = clients
        .iter()
        .filter_map(|c| c["decrypted"]["sessionCount"].as_u64())
        .sum();
    let any_decrypted = clients
        .iter()
        .any(|c| c["decrypted"]["exists"] == json!(true));
    // 「常用」徽标：只能有一个客户端戴上，且必须与实际排序的第一名一致
    // ⇒ 复用 `client_usage` 的唯一出口，别在首页另算一遍。
    let keys: Vec<&str> = installed
        .iter()
        .filter(|c| c.installed)
        .map(|c| c.key)
        .collect();
    let top_pick = client_usage::snapshot_for(&keys)["topPick"].clone();

    json!({
        "clients": clients,
        "installedClients": clients.len(),
        "runningClients": running,
        "accountTotal": account_total,
        "sessionTotal": session_total,
        "anyDecrypted": any_decrypted,
        "topPick": top_pick,
    })
}

// ---------------------------------------------------------------------------
// WorkBuddy 侧
// ---------------------------------------------------------------------------

fn workbuddy_overview() -> Value {
    let processes = workbuddy_switch::list_processes();
    let exe = workbuddy_switch::resolve_exe().map(|p| p.to_string_lossy().into_owned());
    let auth_path = workbuddy_auth::auth_file_path();
    let logged_in = workbuddy_auth::is_logged_in();
    let current_uid = workbuddy_auth::current_uid().unwrap_or_default();

    let vault = workbuddy_vault::load_accounts();
    let accounts: Vec<Value> = vault
        .iter()
        .map(|a| {
            let uid = workbuddy_auth::get_str(a, "uid").unwrap_or_default();
            let name = workbuddy_vault::display_name(a);
            let token_state = match a.get("access_token") {
                Some(Value::Object(_)) => "envelope",
                Some(Value::String(s)) if !s.trim().is_empty() => "plain",
                _ => "missing",
            };
            json!({
                "id": workbuddy_vault::account_id(a),
                "uid": uid,
                "name": name,
                "tokenState": token_state,
                // 只有明文凭据才查得了积分 / 官方用量
                "queryable": token_state == "plain",
                "isCurrent": !current_uid.is_empty() && uid == current_uid,
                "lastUsedAt": a.get("lastUsedAt"),
            })
        })
        .collect();
    let queryable = accounts
        .iter()
        .filter(|a| a["queryable"] == json!(true))
        .count();

    let db = workbuddy_auth::workbuddy_db_path();
    let (live, deleted_titles) = match workbuddy_source::list_sessions() {
        Ok(list) => {
            let total = list.len();
            let dead = list.iter().filter(|s| s.deleted).count();
            (Some(total - dead), Some(dead))
        }
        Err(_) => (None, None),
    };
    let projects = workbuddy_auth::projects_dir();
    let jsonl_files = count_jsonl(&projects);

    json!({
        "running": !processes.is_empty(),
        "processCount": processes.len(),
        "processes": processes.iter().map(|(n, p)| json!({"name": n, "pid": p})).collect::<Vec<_>>(),
        "exe": exe,
        "loggedIn": logged_in,
        "currentUid": current_uid,
        "currentLabel": if current_uid.is_empty() {
            Value::Null
        } else {
            json!(workbuddy_accounts::label_for(&current_uid))
        },
        "authFilePath": auth_path.to_string_lossy(),
        "authFileExists": auth_path.is_file(),
        "authFileBytes": file_len(&auth_path),
        "accounts": accounts,
        "accountCount": accounts.len(),
        "queryableCount": queryable,
        "sessionCount": live,
        "deletedSessionCount": deleted_titles,
        "bodyFileCount": jsonl_files,
        "dbPath": db.to_string_lossy(),
        "dbBytes": file_len(&db),
        "dbExists": db.is_file(),
        "lastSwitch": workbuddy_switch::last_switch(),
    })
}

/// 统计 `projects/` 下 `.jsonl` 文件数（会话正文）。
fn count_jsonl(root: &Path) -> u64 {
    fn walk(dir: &Path, depth: u32) -> u64 {
        if depth > 3 {
            return 0;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut n = 0;
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += walk(&p, depth + 1);
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                n += 1;
            }
        }
        n
    }
    walk(root, 0)
}

// ---------------------------------------------------------------------------
// 磁盘缓存
// ---------------------------------------------------------------------------

/// 把 `{version, generatedAt, snapshot, reclaim}` 组装成缓存文档。
///
/// 纯函数，便于单测；真正落盘的是 [`snapshot`] / [`save_reclaim`]。
/// `reclaim` 由调用方显式传入（两次调用分别保留旧值 / 写入新值）。
fn build_cache_doc(snapshot: &Value, reclaim: Value) -> Value {
    json!({
        "version": CACHE_VERSION,
        "generatedAt": snapshot.get("generatedAt").cloned().unwrap_or(json!(config::now_ms())),
        "snapshot": snapshot,
        "reclaim": reclaim,
    })
}

/// 读磁盘缓存（**只读文件，不重算**）。
///
/// 返回 `{ok, empty, generatedAt, ageMs, snapshot, reclaim}`。
/// 没有缓存 / 版本不符 / 文件损坏都返回 `empty = true`，调用方据此回落到实时计算。
pub fn cached() -> Value {
    let doc = config::read_cache_json(CACHE_NAME);
    let fresh = doc
        .as_ref()
        .filter(|d| d.get("version").and_then(Value::as_u64) == Some(CACHE_VERSION))
        .filter(|d| d.get("snapshot").map(Value::is_object).unwrap_or(false));

    let Some(doc) = fresh else {
        return json!({
            "ok": true,
            "empty": true,
            "generatedAt": Value::Null,
            "ageMs": Value::Null,
            "snapshot": Value::Null,
            "reclaim": Value::Null,
        });
    };

    let at = doc["generatedAt"].as_i64().unwrap_or(0);
    json!({
        "ok": true,
        "empty": false,
        "generatedAt": at,
        "ageMs": (config::now_ms() - at).max(0),
        "snapshot": doc["snapshot"].clone(),
        "reclaim": doc.get("reclaim").cloned().unwrap_or(Value::Null),
    })
}

/// 从旧缓存里取一个字段（文件缺失 / 版本不符 / 字段不存在都返回 `Null`）。
fn prev_field(key: &str) -> Value {
    config::read_cache_json(CACHE_NAME)
        .filter(|d| d.get("version").and_then(Value::as_u64) == Some(CACHE_VERSION))
        .and_then(|d| d.get(key).cloned())
        .unwrap_or(Value::Null)
}

/// 把前端并行扫出的可回收空间并进缓存（下次首屏连这块也是热的）。
pub fn save_reclaim(trae_bytes: u64, wb_bytes: u64) -> Value {
    let snapshot = prev_field("snapshot");
    let reclaim = json!({
        "traeBytes": trae_bytes,
        "wbBytes": wb_bytes,
        "totalBytes": trae_bytes.saturating_add(wb_bytes),
        "at": config::now_ms(),
    });
    config::write_cache_json(CACHE_NAME, &build_cache_doc(&snapshot, reclaim.clone()));
    reclaim
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 首页「注意事项」里的 WorkBuddy 凭据提示。纯函数，便于单测。
///
/// 判据只有本工具账号库一侧（`queryableCount` 本来也只统计它）—— 2026-10-09 起积分
/// 不许再跨库借读，所以「账号库里 0 个可查」就是**真的查不了**，不再需要合并另算一遍。
/// 提示里给出的两条恢复路径也都是本工具自己的动作，不提任何外部路径。
fn workbuddy_credential_note(account_count: u64, own_queryable: usize) -> Option<String> {
    if account_count == 0 || own_queryable > 0 {
        return None;
    }
    // ⚠️ 文案里给出的每条路都必须是**真的存在**的：手动按钮已删（T43），这里是
    // 「启动时自动导入」+「扫码添加账号」两条，两条都真实可发生。
    Some(format!(
        "账号库里的 {account_count} 个账号存的都是加密信封凭据（「导入本机登录态」就是这么落的，\
         本地解不出明文），不能直接调积分接口；本工具会在每次启动时自动尝试从参考工具账号库\
         搬入同账号的明文凭据，若那边也没有，用「扫码添加账号」重新扫码即可查询"
    ))
}

/// 本机总览快照。
///
/// 全部同步、纯统计；不触发解密、不联网。积分只读缓存。
/// 算完**立刻写回磁盘缓存**（`~/.twin-switch/cache/overview.json`），下次启动直接热启。
pub fn snapshot() -> Value {
    let trae = trae_overview();
    let workbuddy = workbuddy_overview();
    let credits = workbuddy_credits::cached();

    let mut notes: Vec<String> = Vec::new();
    if let Some(note) = workbuddy_credential_note(
        workbuddy["accountCount"].as_u64().unwrap_or(0),
        workbuddy["queryableCount"].as_u64().unwrap_or(0) as usize,
    ) {
        notes.push(note);
    }
    if trae["installedClients"].as_u64().unwrap_or(0) > 0 && trae["anyDecrypted"] != json!(true) {
        notes.push("Trae 还没有解密快照，会话记录需要先到「Trae 会话记录」页执行一次解密".to_string());
    }
    if workbuddy["dbExists"] != json!(true) {
        notes.push("没有找到 WorkBuddy 会话库（~/.workbuddy/workbuddy.db）".to_string());
    }

    let out = json!({
        "ok": true,
        "generatedAt": config::now_ms(),
        "trae": trae,
        "workbuddy": workbuddy,
        "credits": credits,
        "notes": notes,
    });

    // 写回缓存：保留上一份的可回收空间（它是前端另一条腿扫出来的，这次没重算）。
    config::write_cache_json(
        CACHE_NAME,
        &build_cache_doc(&out, prev_field("reclaim")),
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_is_read_only_and_shaped() {
        let v = snapshot();
        assert_eq!(v["ok"], json!(true));
        assert!(v["generatedAt"].as_i64().unwrap() > 0);
        // 三个大块必须在
        for key in ["trae", "workbuddy", "credits"] {
            assert!(v.get(key).is_some(), "缺少 {key}");
        }
        assert!(v["trae"]["clients"].is_array());
        assert!(v["workbuddy"]["accounts"].is_array());
        // 积分只读缓存：首次调用不该有已经查好的账号
        assert!(v["credits"]["accounts"].is_array());
        // notes 永远是数组（前端直接 map）
        assert!(v["notes"].is_array());
    }

    #[test]
    fn workbuddy_block_reports_client_state() {
        let v = workbuddy_overview();
        // 进程 / 登录态都是布尔，不能是 null
        assert!(v["running"].is_boolean());
        assert!(v["loggedIn"].is_boolean());
        assert!(v["dbBytes"].is_number());
        // 账号条目字段齐备
        for a in v["accounts"].as_array().unwrap() {
            for key in ["id", "uid", "name", "tokenState", "queryable"] {
                assert!(a.get(key).is_some(), "账号条目缺少 {key}");
            }
            assert!(a["queryable"].is_boolean());
            // 绝不下发凭据
            assert!(a.get("access_token").is_none());
            assert!(a.get("refresh_token").is_none());
        }
    }

    #[test]
    fn count_jsonl_handles_missing_dir() {
        assert_eq!(count_jsonl(&std::path::PathBuf::from("Z:/definitely/missing")), 0);
    }

    /// 账号库存的凭据都是加密信封（`queryableCount = 0`）⇒ 必须说清「查不了」**和**
    /// 两条恢复路径，且**不许**出现任何外部文件路径（2026-10-09：积分不再跨库借读）。
    ///
    /// ⚠️ 两条路必须都是**真实存在**的动作：手动搬家按钮已删（T43 改为启动自动导入），
    /// 文案若还写着「用『导入参考工具账号』…」就是在指一个用户找不到的按钮。
    #[test]
    fn credential_note_points_at_both_local_recovery_paths() {
        let note = workbuddy_credential_note(3, 0).expect("应当给出一条说明");
        assert!(note.contains("加密信封"), "要点明账号库存的是信封，实际：{note}");
        assert!(note.contains("自动"), "要点明启动时会自动搬明文，实际：{note}");
        assert!(note.contains("扫码添加账号"), "要给扫码入口，实际：{note}");
        assert!(
            !note.contains("导入参考工具账号"),
            "手动入口已删除（T43），提示不许指着它，实际：{note}"
        );
        assert!(
            !note.contains("wb-switch") && !note.contains('/'),
            "提示文案里不许出现外部路径，实际：{note}"
        );
    }

    /// 只要能查到（哪怕只有 1 个），或者干脆没账号，都不该多嘴。
    #[test]
    fn credential_note_is_silent_in_normal_states() {
        assert!(workbuddy_credential_note(0, 0).is_none(), "没有账号就别提示");
        assert!(workbuddy_credential_note(3, 3).is_none(), "账号库自己能查就别提示");
        assert!(workbuddy_credential_note(3, 1).is_none(), "有 1 个可查就算能用");
    }

    /// 首页 Trae 卡片要「按客户端独立」：每个客户端必须自带账号库 / 会话 / 积分三块，
    /// 且数字只能是它**自己**的（不能是全局合计）。
    #[test]
    fn trae_clients_carry_their_own_accounts_sessions_and_credits() {
        let v = trae_overview();
        let clients = v["clients"].as_array().unwrap();
        let total = v["accountTotal"].as_u64().unwrap();
        for c in clients {
            for key in ["accounts", "credits", "decrypted"] {
                assert!(c.get(key).is_some(), "客户端条目缺少 {key}");
            }
            assert!(c["accounts"].is_number());
            // 单个客户端的账号数不可能超过所有客户端的合计
            assert!(c["accounts"].as_u64().unwrap() <= total);
            let cr = &c["credits"];
            for key in ["accountCount", "queryable", "withData", "totalRemaining"] {
                assert!(cr.get(key).is_some(), "积分摘要缺少 {key}");
            }
            // 可查积分的账号数不能超过账号总数
            assert!(cr["queryable"].as_u64().unwrap() <= cr["accountCount"].as_u64().unwrap());
            // 登录名要么是字符串要么是 null，绝不能是 undefined 之外的东西
            assert!(c["loginLabel"].is_string() || c["loginLabel"].is_null());
        }
        // 「常用」徽标只有一个（或没有历史时为 null）
        assert!(v["topPick"].is_string() || v["topPick"].is_null());
    }

    /// 积分摘要必须是**压缩过**的：不含逐账号明细（否则总览缓存会被撑大）。
    #[test]
    fn credit_summary_is_compact_and_never_panics() {
        let c = trae_client_credits("definitely-not-a-client");
        assert_eq!(c["accountCount"], json!(0));
        assert_eq!(c["queryable"], json!(0));
        assert_eq!(c["withData"], json!(0));
        assert!(c["totalRemaining"].is_number());
        assert!(c["updatedAt"].is_null());
        assert!(c.get("accounts").is_none(), "摘要不该带上逐账号明细");
    }

    #[test]
    fn cache_doc_keeps_reclaim_and_carries_version() {
        let snap = json!({ "ok": true, "generatedAt": 1234, "notes": [] });
        let reclaim = json!({ "traeBytes": 10, "wbBytes": 5, "totalBytes": 15, "at": 1 });
        let doc = build_cache_doc(&snap, reclaim.clone());
        assert_eq!(doc["version"], json!(CACHE_VERSION));
        assert_eq!(doc["generatedAt"], json!(1234)); // 用快照自己的时间，不是 now
        assert_eq!(doc["snapshot"], snap);
        assert_eq!(doc["reclaim"], reclaim);
        // 没有快照时不 panic，generatedAt 回落到当前时间
        let doc2 = build_cache_doc(&Value::Null, Value::Null);
        assert!(doc2["generatedAt"].as_i64().unwrap() > 0);
    }

    #[test]
    fn cached_is_always_an_object_and_never_panics() {
        // 本机可能压根没写过缓存 —— 两种情况下形状都必须一致，前端才不会踩空。
        let v = cached();
        assert_eq!(v["ok"], json!(true));
        assert!(v["empty"].is_boolean());
        assert!(v.get("snapshot").is_some());
        assert!(v.get("reclaim").is_some());
        if v["empty"] == json!(false) {
            assert!(v["snapshot"].is_object());
            assert!(v["ageMs"].as_i64().unwrap() >= 0);
        }
    }
}
