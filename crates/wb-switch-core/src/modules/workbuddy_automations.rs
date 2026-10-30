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
    let out = match sync_owner_on(&conn, from_uid, to_uid) {
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
    Ok(out)
}

/// 只依赖 `Connection` 的实现体（`sync_owner` 负责开事务/开库），单独抽出来才能用
/// 内存库跑单测。
///
/// ## 为什么要连「子表」一起搬
///
/// `automations` 只是任务**定义**。真正让用户觉得「内容没同步」的是另外两张表：
///
/// - `automation_runs`：任务卡片里那段运行记录（标题、结果、每次 run 的 cwd/成败）。
///   只搬定义 ⇒ 新账号下任务在、点进去运行记录是空的。
/// - `automation_runtime_state`：上次运行时间 / 上次错误 / 正在运行标记。
///   只搬定义 ⇒ 新账号那份没有任何「跑过」的痕迹。
///
/// 还有一条更要紧的：`next_run_at` 是从源账号**原样抄**过来的。用户建完任务过两天才切号，
/// 这个时间点早就过去了 —— 客户端看到「该跑但没跑」就会立刻补跑一次，表现就是
/// 「切到账号 2 任务没按点跑，自己又跑了一遍」。所以过期时间必须按 rrule 重算。
///
/// `automation_delivery_outbox` **不搬**：那是待推送队列，搬过去等于替别的账号
/// 再发一遍微信/企微消息。
fn sync_owner_on(conn: &Connection, from_uid: &str, to_uid: &str) -> Result<Value, String> {
    let mut stmt = conn
        .prepare(
            "select id, name, rrule, prompt, next_run_at
             from automations
             where owner_user_id = ?1 and deleted_at is null",
        )
        .map_err(|e| format!("查询源账号定时任务失败：{e}"))?;
    let src: Vec<(String, String, String, String, Option<i64>)> = stmt
        .query_map(params![from_uid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .map_err(|e| format!("读取源账号定时任务失败：{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("遍历源账号定时任务失败：{e}"))?;
    drop(stmt);

    let now = now_ms();
    let mut copied: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut runs_total = 0usize;
    let mut states_total = 0usize;

    for (src_id, name, rrule, prompt, next_run_at) in src {
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
        // 过期的下次运行时间会让客户端立刻补跑一次 —— 按 rrule 挪到未来。
        let next = match next_run_at {
            Some(ts) if ts > now - 60_000 => Some(ts),
            other => next_after(&rrule, now).or(other),
        };
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
                 select ?1, name, prompt, status, schedule_type, ?5, last_run_at,
                    cwds, rrule, scheduled_at, valid_from, valid_until,
                    model_id, model_is_thinking, skills_json,
                    push_to_wechat, push_to_wecom_bot, wecom_bot_source,
                    ?2, 'confirmed', owner_source,
                    expert_id, expert_marketplace, connector_ids_json,
                    permission_mode, ?3, ?3, null,
                    context_window, reasoning_effort, workspace_scope, wecom_bot_chat_id
                 from automations where id = ?4",
                params![new_id, to_uid, now, src_id, next],
            )
            .map_err(|e| format!("复制定时任务「{name}」失败：{e}"))?;
        if n == 0 {
            continue;
        }
        copied.push(name.clone());
        // ⚠️ 子表复制失败必须**冒泡**，不能 `unwrap_or(0)` 吞掉：
        //    否则「复制失败」在界面上就是 `copiedRuns: 0`，与「源账号本来就没有
        //    运行记录」长得一模一样 —— 这正是这次要修的那个毛病本身。
        //    表不存在时 `columns_of` 返回空表头、`copy_runs` 早早返回 0，不算失败。
        runs_total += copy_runs(conn, &src_id, &new_id)
            .map_err(|e| format!("复制任务「{name}」的运行记录失败：{e}"))?;
        states_total += copy_runtime_state(conn, &src_id, &new_id)
            .map_err(|e| format!("复制任务「{name}」的运行状态失败：{e}"))?;
    }

    Ok(json!({
        "ok": true,
        "from": from_uid,
        "to": to_uid,
        "copied": copied,
        // ⚠️ 用 `skippedNames` 而不是 `skipped`：`switch_to` 里「整体跳过」用的是布尔
        //    `skipped`，同名字段两种类型会让前端判断炸掉（这坑刚踩过）。
        "skippedNames": skipped,
        "copiedRuns": runs_total,
        "copiedRuntimeStates": states_total,
    }))
}

/// 一张表的列名（按声明顺序）。
fn columns_of(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("pragma table_info({table})"))
        .map_err(|e| format!("读取 {table} 结构失败：{e}"))?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("读取 {table} 列失败：{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("遍历 {table} 列失败：{e}"))?;
    Ok(cols)
}

/// 把某个任务的运行记录搬一份给新任务。
///
/// 只搬**最近 50 条**（够撑满「运行记录」页，又不至于把库搬肥）。
/// `thread_id` 是主键，必须重生成；`metadata_json` 里指向源账号会话的
/// `conversationId` / `sessionId` 要摘掉 —— 那两个 id 在新账号下打不开，
/// 留着会让「查看这次运行」点进一个空会话。
fn copy_runs(conn: &Connection, src_id: &str, new_id: &str) -> Result<usize, String> {
    let cols = columns_of(conn, "automation_runs")?;
    if cols.is_empty() {
        return Ok(0);
    }
    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn
        .prepare(&format!(
            "select {col_list} from automation_runs where automation_id = ?1 \
             order by created_at desc limit 50"
        ))
        .map_err(|e| format!("查询运行记录失败：{e}"))?;
    let rows: Vec<Vec<rusqlite::types::Value>> = stmt
        .query_map(params![src_id], |r| {
            let mut v = Vec::with_capacity(cols.len());
            for i in 0..cols.len() {
                v.push(r.get::<_, rusqlite::types::Value>(i)?);
            }
            Ok(v)
        })
        .map_err(|e| format!("读取运行记录失败：{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("遍历运行记录失败：{e}"))?;
    drop(stmt);

    let mut n = 0usize;
    for mut vals in rows {
        for (idx, col) in cols.iter().enumerate() {
            match col.as_str() {
                "automation_id" => {
                    vals[idx] = rusqlite::types::Value::Text(new_id.to_string());
                }
                "thread_id" => {
                    // 原 thread_id 形如 `<automation_id>:<毫秒>`，只换前半段，后半段留作区分
                    let suffix = match &vals[idx] {
                        rusqlite::types::Value::Text(t) => t
                            .split_once(':')
                            .map(|(_, s)| s.to_string())
                            .unwrap_or_else(|| now_ms().to_string()),
                        _ => now_ms().to_string(),
                    };
                    vals[idx] = rusqlite::types::Value::Text(format!("{new_id}:{suffix}"));
                }
                "metadata_json" => {
                    if let rusqlite::types::Value::Text(t) = &vals[idx] {
                        vals[idx] = rusqlite::types::Value::Text(strip_account_refs(t));
                    }
                }
                _ => {}
            }
        }
        let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
        let args: Vec<&dyn rusqlite::types::ToSql> = vals
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        // `insert or ignore` 被忽略时返回 0 —— 计「真写进去的行」，别把忽略也算成复制成功。
        n += conn
            .execute(
                &format!("insert or ignore into automation_runs ({col_list}) values ({placeholders})"),
                rusqlite::params_from_iter(args),
            )
            .map_err(|e| format!("写运行记录失败：{e}"))?;
    }
    Ok(n)
}

/// 摘掉元数据里指向源账号会话的 id（见 [`copy_runs`] 注释）。
fn strip_account_refs(meta: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<Value>(meta) else {
        return meta.to_string();
    };
    if let Some(obj) = v.as_object_mut() {
        for k in ["conversationId", "conversation_id", "sessionId", "session_id"] {
            obj.remove(k);
        }
    }
    v.to_string()
}

/// 搬运行状态：`last_run_at` / `last_error` 留着（这是「内容」的一部分），
/// 但 `running` 必须清零 —— 否则新任务一开就被当成「上次跑到一半崩了」。
fn copy_runtime_state(conn: &Connection, src_id: &str, new_id: &str) -> Result<usize, String> {
    let n = conn
        .execute(
            "insert or replace into automation_runtime_state (
                 automation_id, last_run_at, last_error, running,
                 running_started_at, running_conversation_id, metadata_json
             )
             select ?1, last_run_at, last_error, 0, null, null, metadata_json
             from automation_runtime_state where automation_id = ?2",
            params![new_id, src_id],
        )
        .map_err(|e| format!("复制运行状态失败：{e}"))?;
    Ok(n)
}

/// 按 rrule 算「严格晚于 `after_ms` 的下一次触发时间」（本地时区，毫秒）。
///
/// 只覆盖客户端实际会写出来的那几种：`FREQ=DAILY|WEEKLY|MONTHLY` + `INTERVAL` +
/// `BYHOUR` / `BYMINUTE` / `BYDAY`。解析不出来就返回 `None` —— 调用方据此保留原值，
/// 绝不猜一个时间出来（猜错就是「任务在错误的时间跑」）。
fn next_after(rrule: &str, after_ms: i64) -> Option<i64> {
    use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone, Timelike, Weekday};

    let mut freq = None;
    let mut interval: i64 = 1;
    let mut byhour: Option<u32> = None;
    let mut byminute: Option<u32> = None;
    let mut bydays: Vec<Weekday> = Vec::new();
    let mut bymonthday: Option<u32> = None;

    for part in rrule.split(';') {
        let (k, v) = part.split_once('=')?;
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(v.trim().to_ascii_uppercase()),
            "INTERVAL" => interval = v.trim().parse().ok().filter(|n| *n > 0)?,
            "BYHOUR" => byhour = Some(v.trim().parse().ok()?),
            "BYMINUTE" => byminute = Some(v.trim().parse().ok()?),
            "BYMONTHDAY" => bymonthday = Some(v.trim().parse().ok()?),
            "BYDAY" => {
                for d in v.split(',') {
                    let d = d.trim().to_ascii_uppercase();
                    // 去掉 `2FR` 这种「第几个」前缀，只认星期几
                    let d = d.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
                    bydays.push(match d {
                        "MO" => Weekday::Mon,
                        "TU" => Weekday::Tue,
                        "WE" => Weekday::Wed,
                        "TH" => Weekday::Thu,
                        "FR" => Weekday::Fri,
                        "SA" => Weekday::Sat,
                        "SU" => Weekday::Sun,
                        _ => return None,
                    });
                }
            }
            _ => {}
        }
    }

    let freq = freq?;
    let after = Local.timestamp_millis_opt(after_ms).single()?;
    let hour = byhour.unwrap_or(after.hour());
    // 没写 BYMINUTE 时按 0 分算（RRULE 语义），但只有 BYHOUR 也写了才这么用
    let minute = byminute.unwrap_or(0);
    if hour > 23 || minute > 59 {
        return None;
    }

    // 最多往前找 400 次触发；再找不到就当解析失败
    for step in 0..400i64 {
        let date = match freq.as_str() {
            "DAILY" => after.date_naive() + Duration::days(step * interval),
            "WEEKLY" => {
                let base = after.date_naive();
                let days = if bydays.is_empty() {
                    vec![after.weekday()]
                } else {
                    bydays.clone()
                };
                // 从 after 当天起逐天扫，凑够 step+1 个匹配日
                let mut hit = None;
                let mut seen = 0i64;
                for off in 0..(7 * interval * (step + 1) + 7) {
                    let d = base + Duration::days(off);
                    if days.contains(&d.weekday()) {
                        if seen == step {
                            hit = Some(d);
                            break;
                        }
                        seen += 1;
                    }
                }
                hit?
            }
            "MONTHLY" => {
                let dom = bymonthday.unwrap_or(after.day());
                let mut y = after.year();
                let mut m = after.month() as i64 + step * interval;
                while m > 12 {
                    m -= 12;
                    y += 1;
                }
                NaiveDate::from_ymd_opt(y, m as u32, dom)?
            }
            _ => return None,
        };
        let cand = date.and_hms_opt(hour, minute, 0)?;
        let ts = Local.from_local_datetime(&cand).single()?.timestamp_millis();
        if ts > after_ms {
            return Some(ts);
        }
    }
    None
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

    /// 过期时间要按 rrule 挪到未来，否则客户端会立刻补跑一次。
    #[test]
    fn next_after_moves_daily_into_future() {
        use chrono::{Datelike, Local, TimeZone, Timelike};
        let after = Local
            .with_ymd_and_hms(2026, 10, 31, 10, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let next = next_after("FREQ=DAILY;BYHOUR=9;BYMINUTE=30", after).expect("应当算得出");
        let t = Local.timestamp_millis_opt(next).single().unwrap();
        assert!(next > after, "必须在 after 之后");
        assert_eq!((t.hour(), t.minute()), (9, 30));
        assert_eq!(t.day(), 1, "10-31 10:00 之后的下一次是 11-01 09:30");
    }

    /// 每周：BYDAY=FR 要落到周五，而不是「7 天后」。
    #[test]
    fn next_after_honours_byday() {
        use chrono::{Datelike, Local, TimeZone, Timelike, Weekday};
        // 2026-10-26 是周一
        let after = Local
            .with_ymd_and_hms(2026, 10, 26, 12, 0, 0)
            .single()
            .unwrap()
            .timestamp_millis();
        let next = next_after("FREQ=WEEKLY;BYDAY=FR;BYHOUR=14;BYMINUTE=0", after).expect("应当算得出");
        let t = Local.timestamp_millis_opt(next).single().unwrap();
        assert_eq!(t.weekday(), Weekday::Fri);
        assert_eq!((t.hour(), t.minute()), (14, 0));
        assert_eq!(t.day(), 30);
    }

    /// 解析不出来就返回 None（调用方据此保留原值），不许瞎猜。
    #[test]
    fn next_after_refuses_unknown_rrule() {
        use chrono::{Local, TimeZone};
        let after = Local.with_ymd_and_hms(2026, 10, 31, 10, 0, 0).single().unwrap().timestamp_millis();
        assert!(next_after("FREQ=YEARLY;BYMONTH=1", after).is_none());
        assert!(next_after("", after).is_none());
        assert!(next_after("FREQ=WEEKLY;BYDAY=XX", after).is_none());
    }

    /// 一条任务的定义 + 运行记录 + 运行状态：同步后三样都得在，且 next_run_at 不许过期。
    #[test]
    fn sync_copies_runs_and_state() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            create table automations (
                id text primary key, name text not null, prompt text not null, status text not null,
                schedule_type text not null default 'recurring', next_run_at integer, last_run_at integer,
                cwds text not null default '[]', rrule text not null default '', scheduled_at text,
                valid_from text, valid_until text, model_id text, model_is_thinking integer not null default 0,
                skills_json text not null default '[]', push_to_wechat integer not null default 0,
                push_to_wecom_bot integer not null default 0, wecom_bot_source text, owner_user_id text,
                owner_status text not null default 'legacy_unassigned', owner_source text, expert_id text,
                expert_marketplace text, connector_ids_json text not null default '[]', permission_mode text,
                created_at integer not null, updated_at integer not null, deleted_at integer,
                context_window integer, reasoning_effort text, workspace_scope text, wecom_bot_chat_id text
            );
            create table automation_runs (
                thread_id text primary key, automation_id text not null, status text not null,
                read_at integer, thread_title text, source_cwd text, runs_json text, result_success integer,
                metadata_json text, created_at integer not null, updated_at integer not null,
                failure_code text, reason_code text
            );
            create table automation_runtime_state (
                automation_id text primary key, last_run_at integer, last_error text,
                running integer not null, running_started_at integer, running_conversation_id text,
                metadata_json text
            );
            "#,
        )
        .unwrap();

        let past = now_ms() - 3 * 24 * 3600 * 1000; // 三天前就该跑了
        conn.execute(
            "insert into automations (id,name,prompt,status,next_run_at,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
             values ('a1','历史上的今天','讲个历史上的今天','ACTIVE',?1,'[]','FREQ=DAILY;BYHOUR=9;BYMINUTE=30','u1','confirmed',1,1)",
            params![past],
        ).unwrap();
        conn.execute(
            "insert into automation_runs (thread_id,automation_id,status,thread_title,metadata_json,created_at,updated_at)
             values ('a1:111','a1','ACCEPTED','1938 年的广播剧','{\"runKind\":\"manual_test\",\"conversationId\":\"c1\",\"sessionId\":\"s1\"}',111,111)",
            [],
        ).unwrap();
        conn.execute(
            "insert into automation_runtime_state (automation_id,last_run_at,last_error,running,running_started_at)
             values ('a1',222,null,1,333)",
            [],
        ).unwrap();

        let out = sync_owner_on(&conn, "u1", "u2").expect("同步应当成功");
        assert_eq!(out.get("copiedRuns").and_then(Value::as_u64), Some(1));
        assert_eq!(out.get("copiedRuntimeStates").and_then(Value::as_u64), Some(1));

        let (new_id, next): (String, i64) = conn
            .query_row(
                "select id, next_run_at from automations where owner_user_id='u2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(next > now_ms(), "复制过去的 next_run_at 必须是未来时间，否则会立刻补跑");

        // 运行记录跟着走了，且摘掉了指向源账号会话的 id
        let (title, meta, tid, aid): (String, String, String, String) = conn
            .query_row(
                "select thread_title, metadata_json, thread_id, automation_id from automation_runs where automation_id=?1",
                params![new_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "1938 年的广播剧");
        assert!(!meta.contains("conversationId") && !meta.contains("sessionId"), "应摘掉源账号会话引用：{meta}");
        assert!(tid.starts_with(&format!("{new_id}:")));
        assert_eq!(aid, new_id);

        // 运行状态：last_run_at 留着，running 必须清零
        let (last, running, started): (Option<i64>, i64, Option<i64>) = conn
            .query_row(
                "select last_run_at, running, running_started_at from automation_runtime_state where automation_id=?1",
                params![new_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(last, Some(222));
        assert_eq!(running, 0);
        assert_eq!(started, None);

        // 再同步一次：全部跳过，不产生第二份
        let again = sync_owner_on(&conn, "u1", "u2").unwrap();
        assert_eq!(again.get("copied").and_then(|v| v.as_array()).map(|a| a.len()), Some(0));
        let n: i64 = conn
            .query_row("select count(*) from automations where owner_user_id='u2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
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
