//! WorkBuddy **定时任务**（`automations` 表）的读取与跨账号同步。
//!
//! ## 为什么需要这个模块
//!
//! 定时任务存在 `~/.workbuddy/workbuddy.db` 的 `automations` 表里，**按 `owner_user_id`
//! 归属**（客户端只列当前登录账号的任务）。所以切到另一个账号后，原来建的任务会全部
//! 「消失」——不是丢了，是归属变了。用户要求：**切号时默认把它们同步给新账号**，
//! 并能在设置里关掉这个行为。
//!
//! ## 安全边界
//!
//! - 只动 `automations` **一张表**，且**只插入**（绝不改/删原有行）；
//! - 调用方必须在**客户端已退出**之后调（`switch_to` 里已经 kill 过）；
//! - 去重键 = `name + rrule + prompt`：新账号下已有同款就跳过，避免每切一次多一份；
//! - 失败**不影响切号本身**：返回报告，界面照实显示。

use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::PathBuf;

/// `~/.workbuddy/workbuddy.db`（WorkBuddy 的主库：会话、定时任务、工作区都在这）。
pub fn db_path() -> Option<PathBuf> {
    let root = crate::modules::workbuddy_auth::data_root();
    let p = root.join("workbuddy.db");
    p.is_file().then_some(p)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn open_rw(path: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("打开 workbuddy.db（可写）失败：{e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| format!("设置 busy_timeout 失败：{e}"))?;
    Ok(conn)
}

/// 某个账号名下的定时任务（未删除）。只读，不发网络请求。
pub fn list_for(owner_uid: &str) -> Result<Vec<Value>, String> {
    let path = db_path().ok_or_else(|| "本机没有 WorkBuddy 数据库（没装或没登录过）".to_string())?;
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("打开 workbuddy.db 失败：{e}"))?;
    let mut stmt = conn
        .prepare(
            "select id, name, status, schedule_type, rrule, next_run_at, last_run_at
             from automations
             where owner_user_id = ?1 and deleted_at is null
             order by created_at asc",
        )
        .map_err(|e| format!("查询定时任务失败：{e}"))?;
    let rows = stmt
        .query_map(params![owner_uid], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "scheduleType": r.get::<_, String>(3)?,
                "rrule": r.get::<_, String>(4)?,
                "nextRunAt": r.get::<_, Option<i64>>(5)?,
                "lastRunAt": r.get::<_, Option<i64>>(6)?,
            }))
        })
        .map_err(|e| format!("读取定时任务失败：{e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("读取定时任务行失败：{e}"))?);
    }
    Ok(out)
}

/// 把 `from_uid` 名下的定时任务**复制**给 `to_uid`。
///
/// - 逐条按 `name + rrule + prompt` 去重（新账号已有同款就跳过）；
/// - 新行 `id` 重新生成、`owner_status` 置 `confirmed`（否则可能被当成旧数据不显示）；
/// - `next_run_at` 原样带过去，客户端自己会重排；
/// - 一个事务里做完，中途失败回滚。
pub fn sync_owner(from_uid: &str, to_uid: &str) -> Result<Value, String> {
    if from_uid.trim().is_empty() || to_uid.trim().is_empty() {
        return Err("同步定时任务需要源账号与目标账号".to_string());
    }
    if from_uid == to_uid {
        return Ok(json!({ "ok": true, "notNeeded": true, "reason": "同一个账号，无需同步" }));
    }
    let path = db_path().ok_or_else(|| "本机没有 WorkBuddy 数据库（没装或没登录过）".to_string())?;
    let conn = open_rw(&path)?;

    conn.execute_batch("begin immediate")
        .map_err(|e| format!("开启事务失败：{e}"))?;

    let result: Result<(Vec<String>, Vec<String>), String> = (|| {
        let mut stmt = conn
            .prepare(
                "select id, name, rrule, prompt
                 from automations
                 where owner_user_id = ?1 and deleted_at is null",
            )
            .map_err(|e| format!("查询源账号定时任务失败：{e}"))?;
        let src: Vec<(String, String, String, String)> = stmt
            .query_map(params![from_uid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("读取源账号定时任务失败：{e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("遍历源账号定时任务失败：{e}"))?;
        drop(stmt);

        let mut copied: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();

        for (src_id, name, rrule, prompt) in src {
            let exists: i64 = conn
                .query_row(
                    "select count(*) from automations
                     where owner_user_id = ?1 and deleted_at is null
                       and name = ?2 and rrule = ?3 and prompt = ?4",
                    params![to_uid, name, rrule, prompt],
                    |r| r.get(0),
                )
                .map_err(|e| format!("检查目标账号是否已有同名任务失败：{e}"))?;
            if exists > 0 {
                skipped.push(name);
                continue;
            }
            let new_id = uuid::Uuid::new_v4().to_string();
            let now = now_ms();
            // 逐列 SELECT 复制：**不用手写每个值**，新增列时这里不会漏，也不会把
            // 未知字段写成默认值（那等于悄悄改坏用户的任务配置）。
            let n = conn
                .execute(
                    "insert into automations (
                        id, name, prompt, status, schedule_type, next_run_at, last_run_at,
                        cwds, rrule, scheduled_at, valid_from, valid_until,
                        model_id, model_is_thinking, skills_json,
                        push_to_wechat, push_to_wecom_bot, wecom_bot_source,
                        owner_user_id, owner_status, owner_source,
                        expert_id, expert_marketplace, connector_ids_json,
                        permission_mode, created_at, updated_at, deleted_at,
                        context_window, reasoning_effort, workspace_scope, wecom_bot_chat_id
                     )
                     select ?1, name, prompt, status, schedule_type, next_run_at, last_run_at,
                        cwds, rrule, scheduled_at, valid_from, valid_until,
                        model_id, model_is_thinking, skills_json,
                        push_to_wechat, push_to_wecom_bot, wecom_bot_source,
                        ?2, 'confirmed', owner_source,
                        expert_id, expert_marketplace, connector_ids_json,
                        permission_mode, ?3, ?3, null,
                        context_window, reasoning_effort, workspace_scope, wecom_bot_chat_id
                     from automations where id = ?4",
                    params![new_id, to_uid, now, src_id],
                )
                .map_err(|e| format!("复制定时任务「{name}」失败：{e}"))?;
            if n > 0 {
                copied.push(name);
            }
        }
        Ok((copied, skipped))
    })();

    let (copied, skipped) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            return Err(e);
        }
    };

    conn.execute_batch("commit")
        .map_err(|e| format!("提交事务失败：{e}"))?;
    // 把 WAL 合并回主库，避免客户端重启时从 WAL 读到旧页。
    let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));

    Ok(json!({
        "ok": true,
        "from": from_uid,
        "to": to_uid,
        "copied": copied,
        // ⚠️ 用 `skippedNames` 而不是 `skipped`：`switch_to` 里「整体跳过」用的是布尔
        //    `skipped`，同名字段两种类型会让前端判断炸掉（这坑刚踩过）。
        "skippedNames": skipped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机自检：把当前账号的任务同步给一个**假账号**，验证复制与去重，最后清理。
    /// 只新增再删除 `owner_user_id = 假账号` 的行，不碰真实任务。
    #[test]
    #[ignore]
    fn probe_sync_roundtrip() {
        let Some(from) = crate::modules::workbuddy_auth::current_uid() else {
            eprintln!("[probe] 没有当前登录账号，跳过");
            return;
        };
        let to = format!("twinswitch-sync-probe-{}", uuid::Uuid::new_v4());
        match sync_owner(&from, &to) {
            Ok(v) => eprintln!(
                "[probe] 第一次同步 copied={:?} skipped={:?}",
                v.get("copied"), v.get("skippedNames")
            ),
            Err(e) => {
                eprintln!("[probe] 同步失败：{e}");
                return;
            }
        }
        match sync_owner(&from, &to) {
            Ok(v) => eprintln!(
                "[probe] 第二次同步（应全部 skipped）copied={:?} skipped={:?}",
                v.get("copied"), v.get("skippedNames")
            ),
            Err(e) => eprintln!("[probe] 第二次同步失败：{e}"),
        }
        eprintln!("[probe] 目标账号下任务数 = {:?}", list_for(&to).map(|v| v.len()));
        // 清理假账号的行
        if let Some(path) = db_path() {
            if let Ok(conn) = open_rw(&path) {
                let n = conn
                    .execute("delete from automations where owner_user_id = ?1", params![to])
                    .unwrap_or(0);
                eprintln!("[probe] 已清理 {n} 行");
            }
        }
    }

    /// 只读自检：列出当前登录账号名下的定时任务（不写任何东西）。
    #[test]
    #[ignore]
    fn probe_list_current_owner() {
        let Some(uid) = crate::modules::workbuddy_auth::current_uid() else {
            eprintln!("[probe] 没有当前登录账号");
            return;
        };
        eprintln!("[probe] 当前 uid = {uid}");
        match list_for(&uid) {
            Ok(list) => {
                eprintln!("[probe] 定时任务 {} 条：", list.len());
                for t in list {
                    eprintln!(
                        "  - {} | {} | {}",
                        t.get("name").and_then(Value::as_str).unwrap_or(""),
                        t.get("status").and_then(Value::as_str).unwrap_or(""),
                        t.get("rrule").and_then(Value::as_str).unwrap_or("")
                    );
                }
            }
            Err(e) => eprintln!("[probe] 失败：{e}"),
        }
    }
}
