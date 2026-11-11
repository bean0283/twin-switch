//! WorkBuddy **定时任务**（`automations` 表）的读取与跨账号同步。
//!
//! ## 为什么需要这个模块
//!
//! 定时任务存在 `~/.workbuddy/workbuddy.db` 的 `automations` 表里，**按 `owner_user_id`
//! 归属**（客户端只列当前登录账号的任务）。所以切到另一个账号后，原来建的任务会全部
//! 「消失」——不是丢了，是归属变了。用户要求：**切号时默认把它们同步给新账号**，
//! 并能在设置里关掉这个行为。
//!
//! ## 两种「目标账号已有同款」处理方式（2026-11-11 新增，可在设置里切换）
//!
//! - [`SyncMode::Skip`]（**默认**，历史行为）：已有同款就**跳过**，一行都不碰。
//!   适合「各账号任务本就该各管各的」或「只想把新账号补上，不想动它已有的」。
//! - [`SyncMode::Overwrite`]：用源账号的内容**覆盖**目标同款的任务定义 ——
//!   保留目标行的 `id` / `created_at` / `owner_user_id`（这三个是"这条任务在目标账号
//!   里的身份"，换了就等于换了一条任务、运行记录会全部对不上），其余列（`prompt`、
//!   `rrule`、`status`、`model_id`、`skills_json`、推送开关、`permission_mode`…）
//!   全部按源账号刷新，并把 `next_run_at` 按 rrule 重算后写回。
//!
//! **「同一个任务」的判定口径两种模式完全一致**：`name` 相同 + `rrule` **等价**
//! （见 [`normalize_rrule`]：去掉 `RRULE:` 前缀、大小写无关、分号段顺序无关），
//! 且目标行 `deleted_at is null`。
//!
//! ⚠️ **判定键里不能有 `prompt`**（2026-11-11 修）：界面上写的是「同名同周期的算同一个
//! 任务」，而旧实现把 `prompt` 也塞进了比对条件。结果用户在一个账号里把任务正文改了
//! （`test` → `test111`）再切回来，就因为 prompt 不等而被当成两条新任务，界面上并列
//! 出现两个「test / 每月 1 日 15:23」—— 正是这次报的这个 bug。prompt 属于"内容"，
//! 是覆盖模式要刷新的东西，不是身份。
//!
//! 不比 `id`（每个账号各自生成），也不比 `model_id` / `skills_json` / `cwds` —— 同理。
//! 软删除（`deleted_at` 非空）的行**不算**"已有"：用户删掉的，同步时新增一条更安全。
//!
//! 目标账号里已经存在多条同名同周期（历史脏数据）时：
//! - [`SyncMode::Skip`]：只报数（`extraDuplicates`），一条都不动；
//! - [`SyncMode::Overwrite`]：刷新代表行，其余**软删除合并**（`mergedNames`），
//!   界面上回到「一个任务一条」。因为软删除可恢复，不搬走它们的运行记录。
//!
//! ## 安全边界
//!
//! - 只动 `automations` 与它的两张子表（`automation_runs` / `automation_runtime_state`）；
//!   不动会话，也不动 `automation_delivery_outbox`（那是待推送队列，搬过去等于替别的
//!   账号再发一遍微信/企微消息）；
//! - 覆盖模式**不删任何行**：目标账号自己跑出来的运行记录保留，只把源账号缺的那几条
//!   补进去（`thread_id` 按目标 id 重写，`insert or ignore` 天然去重）；
//! - 调用方必须在**客户端已退出**之后调（`switch_to` 里已经 kill 过）；
//! - 一个事务里做完，**任何一条失败整批回滚**（宁可一条都没同步，也不留半份）；
//!   失败**不影响切号本身**：返回报告，界面照实显示，并落一条 error 日志。

use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
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

/// 诊断日志的作用域（切号那次同步也用它，事后 `grep` 一个词就能把前后串起来）。
pub const LOG_SCOPE: &str = "wb.automation-sync";

/// 「目标账号已经有这个任务了」时的处理方式。**用户可在设置里切换**。
///
/// 判定「同一个任务」的口径**两种模式完全一致**（`name` + `rrule` + `prompt` 全等，
/// 且目标行未软删除）—— 否则"跳过"和"覆盖"会各自认一套"相同"，用户永远对不上账。
///
/// 序列化值是 `"skip"` / `"overwrite"`，落在 `settings.json` 的
/// `workbuddyAutomationSyncMode`；缺这个键时按 [`SyncMode::Skip`]（= 老行为，不突变）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// 跳过：目标已有同款就原样不动（默认）。
    Skip,
    /// 覆盖：用源账号的内容刷新目标同款（保留目标行的 id / created_at）。
    Overwrite,
}

impl Default for SyncMode {
    /// **必须与 serde 缺键时的行为一致**（`settings.json` 里没这个键 ⇒ Skip）。
    fn default() -> Self {
        Self::Skip
    }
}

impl SyncMode {
    /// 存进 `settings.json` / 回传给前端的字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Overwrite => "overwrite",
        }
    }

    /// 界面文案（后端给，前端不自己拼 —— 免得"界面说跳过、实际在覆盖"）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Skip => "跳过（目标已有同款就不动它）",
            Self::Overwrite => "覆盖（用当前账号的内容刷新同款任务）",
        }
    }

    /// 宽松解析：接受 `skip` / `overwrite`（大小写、空格都不敏感）。
    /// 认不出来返回 `None`，调用方据此报错，**不许静默退回默认值**。
    pub fn parse_loose(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "skip" | "skipped" | "跳过" => Some(Self::Skip),
            "overwrite" | "overwrite_all" | "覆盖" => Some(Self::Overwrite),
            _ => None,
        }
    }
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

/// 把 `from_uid` 名下的定时任务同步给 `to_uid` —— **「已有同款」的处理方式取自设置**。
///
/// 保留这个签名，是为了让调用方（切号编排、examples、探针测试）不必关心模式；
/// 需要显式指定模式（单测、一次性「强制覆盖」）时用 [`sync_owner_with_mode`]。
///
/// 为什么每次都现读设置：用户改完设置立刻切号就该按新方式走，不该等重启。
pub fn sync_owner(from_uid: &str, to_uid: &str) -> Result<Value, String> {
    let mode = crate::modules::settings::workbuddy_automation_sync_mode();
    sync_owner_with_mode(from_uid, to_uid, mode)
}

/// 同 [`sync_owner`]，但模式由调用方给定（不走设置）。
///
/// 约束（两种模式共用）：
/// - 逐条按 `name` + 等价 `rrule` 判定"同一个任务"（**不看 prompt**，见模块注释）；
/// - 新增行 `id` 重新生成、`owner_status` 置 `confirmed`（否则可能被当成旧数据不显示）；
/// - `next_run_at` 过期就按 rrule 挪到未来（否则客户端立刻补跑一次）；
/// - 一个事务做完，**任何一条失败整批回滚**；失败/成功都落诊断日志。
pub fn sync_owner_with_mode(from_uid: &str, to_uid: &str, mode: SyncMode) -> Result<Value, String> {
    if from_uid.trim().is_empty() || to_uid.trim().is_empty() {
        return Err("同步定时任务需要源账号与目标账号".to_string());
    }
    if from_uid == to_uid {
        return Ok(json!({
            "ok": true,
            "notNeeded": true,
            "mode": mode.as_str(),
            "reason": "同一个账号，无需同步",
        }));
    }
    let path = db_path().ok_or_else(|| "本机没有 WorkBuddy 数据库（没装或没登录过）".to_string())?;
    let conn = open_rw(&path)?;

    conn.execute_batch("begin immediate")
        .map_err(|e| format!("开启事务失败：{e}"))?;
    let out = match sync_owner_on(&conn, from_uid, to_uid, mode) {
        Ok(v) => v,
        Err(e) => {
            // 整批回滚：不允许"一半复制了、一半没复制"这种说不清的状态。
            let _ = conn.execute_batch("rollback");
            // 失败按 ERROR 记（默认级别就落盘）——这是「切了号任务没过去」唯一能事后复现的证据。
            crate::modules::diag_log::error(
                LOG_SCOPE,
                format!(
                    "同步定时任务失败（{}）：{from_uid} → {to_uid}：{e}",
                    mode.as_str()
                ),
            );
            return Err(e);
        }
    };
    conn.execute_batch("commit")
        .map_err(|e| format!("提交事务失败：{e}"))?;
    // 把 WAL 合并回主库，避免客户端重启时从 WAL 读到旧页。
    let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));

    // 成功也留一行 INFO（默认级别不落盘，排查问题时调到 info 才有）。
    let len_of = |k: &str| {
        out.get(k)
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0)
    };
    crate::modules::diag_log::info(
        LOG_SCOPE,
        format!(
            "同步定时任务 OK（{}）：{from_uid} → {to_uid}，新增 {}、覆盖 {}、跳过 {}（源共 {}）",
            mode.as_str(),
            len_of("copied"),
            len_of("overwritten"),
            len_of("skippedNames"),
            out.get("sourceCount").and_then(Value::as_u64).unwrap_or(0),
        ),
    );
    Ok(out)
}

/// 合并某个账号下**同名同周期**的重复任务（不需要切号）。
///
/// 为什么需要单独一个入口：同步只在"切号"时发生，而重复行可能就长在**当前登录
/// 账号**里（例如本机 `test` 的两条：一条是客户端自己建的，一条是上次同步插的）。
/// 这种账号永远不会成为切号的目标，只靠同步永远治不好 —— 用户得有个"就地整理"
/// 的按钮。
///
/// 语义与覆盖模式一致：
/// - 代表行 = 创建时间最早那条（id / created_at 稳定，运行记录都挂在它上面）；
/// - 代表行的**内容**取自同组里 `updated_at` 最新的那条（= 用户最后一次编辑的那份）；
/// - 其余行软删除（`deleted_at`），运行记录不动、不删。
pub fn merge_duplicates(uid: &str) -> Result<Value, String> {
    if uid.trim().is_empty() {
        return Err("需要指定要合并的账号".to_string());
    }
    let path = db_path().ok_or_else(|| "本机没有 WorkBuddy 数据库（没装或没登录过）".to_string())?;
    let conn = open_rw(&path)?;
    conn.execute_batch("begin immediate")
        .map_err(|e| format!("开启事务失败：{e}"))?;
    let out = match merge_duplicates_on(&conn, uid) {
        Ok(v) => v,
        Err(e) => {
            let _ = conn.execute_batch("rollback");
            crate::modules::diag_log::error(
                LOG_SCOPE,
                format!("合并同名同周期任务失败（{uid}）：{e}"),
            );
            return Err(e);
        }
    };
    conn.execute_batch("commit")
        .map_err(|e| format!("提交事务失败：{e}"))?;
    let _ = conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()));
    crate::modules::diag_log::info(
        LOG_SCOPE,
        format!(
            "合并同名同周期任务 OK：{uid}，合并 {} 条（涉及 {} 组）",
            out.get("merged").and_then(Value::as_u64).unwrap_or(0),
            out.get("groups").and_then(Value::as_u64).unwrap_or(0),
        ),
    );
    Ok(out)
}

/// [`merge_duplicates`] 的实现体（可拿内存库跑单测）。
fn merge_duplicates_on(conn: &Connection, uid: &str) -> Result<Value, String> {
    struct Row {
        id: String,
        name: String,
        next_run_at: Option<i64>,
        updated_at: i64,
    }
    let mut stmt = conn
        .prepare(
            "select id, name, rrule, next_run_at, updated_at from automations
             where owner_user_id = ?1 and deleted_at is null
             order by created_at asc, id asc",
        )
        .map_err(|e| format!("查询账号定时任务失败：{e}"))?;
    let rows: Vec<(Row, String)> = stmt
        .query_map(params![uid], |r| {
            Ok((
                Row {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    next_run_at: r.get(3)?,
                    updated_at: r.get(4)?,
                },
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("读取账号定时任务失败：{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("遍历账号定时任务失败：{e}"))?;
    drop(stmt);

    // 分组：同名 + 等价周期。保持插入顺序（SQL 已按 created_at 升序），
    // 组内第一条就是"代表行"。
    let mut groups: Vec<(String, Vec<(Row, String)>)> = Vec::new();
    for (row, rrule) in rows {
        let key = format!("{}{}{}", row.name, '\u{1}', normalize_rrule(&rrule));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push((row, rrule)),
            None => groups.push((key, vec![(row, rrule)])),
        }
    }

    let now = now_ms();
    let mut merged = 0usize;
    let mut touched_groups = 0usize;
    let mut names: Vec<String> = Vec::new();
    for (_, mut g) in groups.into_iter().filter(|(_, v)| v.len() > 1) {
        touched_groups += 1;
        let rep = g.remove(0);
        let name = rep.0.name.clone();
        // 内容来源 = 组内最后被编辑的那条（用户最新的想法）。
        let latest_at = g
            .iter()
            .map(|(r, _)| r.updated_at)
            .chain(std::iter::once(rep.0.updated_at))
            .max()
            .unwrap_or(rep.0.updated_at);
        if latest_at > rep.0.updated_at {
            // 找到那条最新行（同 updated_at 时取数组里靠后的，行为确定）
            let mut best = &rep;
            for item in &g {
                if item.0.updated_at >= best.0.updated_at {
                    best = item;
                }
            }
            // 代表行的 next_run_at 保持不变：合并前后"下次什么时候跑"不该变。
            let n = overwrite_task(conn, &best.0.id, &rep.0.id, rep.0.next_run_at, now)
                .map_err(|e| format!("合并定时任务「{name}」失败：{e}"))?;
            if n == 0 {
                // 代表行在事务内消失：极端情况，跳过这一组，不动其余行。
                continue;
            }
        }
        for (extra, _) in &g {
            // 只合并内容确实"同一个任务"的行；若最新那条就是代表行，其余仍是重复。
            let m = soft_delete_task(conn, &extra.id, now)
                .map_err(|e| format!("合并定时任务「{name}」的重复行失败：{e}"))?;
            if m > 0 {
                merged += 1;
            }
        }
        names.push(name);
    }

    Ok(json!({
        "ok": true,
        "uid": uid,
        // 有几组重复（= 有几个任务名需要合并）
        "groups": touched_groups,
        // 合并掉（软删除）了几行
        "merged": merged,
        "names": names,
    }))
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
fn sync_owner_on(
    conn: &Connection,
    from_uid: &str,
    to_uid: &str,
    mode: SyncMode,
) -> Result<Value, String> {
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
    // 覆盖模式下被刷新的目标行（名字）。skip 模式下恒为空。
    let mut overwritten: Vec<String> = Vec::new();
    // 覆盖模式下被合并掉的重复行（同名同周期，软删除）。skip 模式下恒为空。
    let mut merged: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut overwritten: Vec<String> = Vec::new();
    let mut runs_total = 0usize;
    let mut states_total = 0usize;
    // 目标账号里"同名同周期但多余"的行数。skip 模式只报数、一条都不动。
    let mut extra_duplicates = 0usize;
    // 源账号名下有几条（`copied + overwritten + skipped` 应等于它）。界面靠这个数字区分
    // 「本来就没有任务」和「同步没跑」—— 两者都表现为「新账号里没多东西」。
    let source_count = src.len();
    // 源账号自己就有同名同周期（同一个任务被建了两遍）时，只落一条目标行，
    // 后面的源行把运行记录并进同一条 —— 否则源账号的脏数据会被"忠实"复制成两份。
    let mut done: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for (src_id, name, rrule, prompt, next_run_at) in src {
        // 「同一个任务」= 同名 + 同周期，**不含 prompt**。
        // 2026-11-11 修：旧实现把 prompt 也比了，于是「在一个账号里改了任务正文、
        // 再切回来」会被判成两条新任务 —— 界面上并列两个「test / 每月 1 日 15:23」。
        // prompt 属于"内容"（覆盖模式要刷新的东西），不是任务身份。
        let key = format!("{}{}{}", name, '\u{1}', normalize_rrule(&rrule));
        let _ = &prompt;

        // 源账号自己就有同名同周期：复用刚才那条结果行，只把运行记录并进去。
        if let Some(tid) = done.get(&key).cloned() {
            runs_total += copy_runs(conn, &src_id, &tid)
                .map_err(|e| format!("同步任务「{name}」的运行记录失败：{e}"))?;
            states_total += copy_runtime_state(conn, &src_id, &tid)
                .map_err(|e| format!("同步任务「{name}」的运行状态失败：{e}"))?;
            continue;
        }

        // 判定「同一个任务」两种模式共用这一套键：`name` + 等价 `rrule`。
        // 只认未软删除的行 —— 用户删掉的任务不算"已有"，同步时新增一条更安全。
        // 正常只会命中 1 条；历史脏数据可能多条，这里全取出来。
        let existing = same_key_ids(conn, to_uid, &name, &rrule)?;

        // 过期的下次运行时间会让客户端立刻补跑一次 —— 按 rrule 挪到未来。
        // 覆盖模式同样要算：否则"覆盖完立刻补跑一次"，用户会以为任务被重复触发。
        let next = match next_run_at {
            Some(ts) if ts > now - 60_000 => Some(ts),
            other => next_after(&rrule, now).or(other),
        };

        // 这条任务最终落在哪一行（新增的 id / 被覆盖的目标 id）。子表统一按它搬。
        let mut target_id: Option<String> = None;

        match mode {
            // 方式一：跳过 —— 一行都不碰，只在报告里留个名字。
            SyncMode::Skip if !existing.is_empty() => {
                // 跳过模式一条都不动（历史行为），但把"多出来几条"报上去，
                // 免得用户看着界面上两条同名同周期、却被告知"已存在，跳过"。
                if existing.len() > 1 {
                    extra_duplicates += existing.len() - 1;
                }
                skipped.push(name);
                continue;
            }
            // 方式二：覆盖 —— 用源内容刷新目标同键行（id / created_at / owner 不动）。
            SyncMode::Overwrite if !existing.is_empty() => {
                let dst_id = existing[0].clone();
                let n = overwrite_task(conn, &src_id, &dst_id, next, now)
                    .map_err(|e| format!("覆盖定时任务「{name}」失败：{e}"))?;
                if n > 0 {
                    overwritten.push(name.clone());
                    target_id = Some(dst_id);
                    // 同名同周期 = 同一个任务：多出来的历史重复行在这里合并掉
                    // （软删除，可恢复；运行记录不搬也不删）。不合并的话，界面上
                    // 会一直并列两条「同名同周期」—— 也就是这次报的这个现象。
                    for extra in existing.iter().skip(1) {
                        let m = soft_delete_task(conn, extra, now)
                            .map_err(|e| format!("合并定时任务「{name}」的重复行失败：{e}"))?;
                        if m > 0 {
                            merged.push(name.clone());
                        }
                    }
                } else {
                    // 同键行刚查出来却改不动：极端情况（并发被删 / 行匹配不上）。
                    // 退化成新增 —— 宁可多一条，也不让这个任务凭空消失。
                    crate::modules::diag_log::info(
                        LOG_SCOPE,
                        format!("覆盖「{name}」命中 0 行，退化为新增（目标 {to_uid}）"),
                    );
                }
            }
            // 目标没有同款（两种模式一样都新增）
            _ => {}
        }

        if target_id.is_none() {
            let new_id = uuid::Uuid::new_v4().to_string();
            if insert_task(conn, &src_id, &new_id, to_uid, next, now)
                .map_err(|e| format!("复制定时任务「{name}」失败：{e}"))?
                == 0
            {
                continue;
            }
            copied.push(name.clone());
            target_id = Some(new_id);
        }
        let tid = target_id.expect("上面两条路径都赋过值");
        // 记下"这个名字+周期已经落过一行"，源账号自己的重复不再新增第二行。
        done.insert(key, tid.clone());

        // ⚠️ 子表失败必须**冒泡**，不能 `unwrap_or(0)` 吞掉：
        //    否则「复制失败」在界面上就是 `copiedRuns: 0`，与「源账号本来就没有
        //    运行记录」长得一模一样 —— 这正是上次要修的那个毛病本身。
        //    表不存在时 `columns_of` 返回空表头、`copy_runs` 早早返回 0，不算失败。
        runs_total += copy_runs(conn, &src_id, &tid)
            .map_err(|e| format!("同步任务「{name}」的运行记录失败：{e}"))?;
        states_total += copy_runtime_state(conn, &src_id, &tid)
            .map_err(|e| format!("同步任务「{name}」的运行状态失败：{e}"))?;
    }

    Ok(json!({
        "ok": true,
        // 这次用哪种方式（界面按它决定说"跳过 N 个"还是"覆盖 N 个"）。
        "mode": mode.as_str(),
        "from": from_uid,
        "to": to_uid,
        // 源账号名下的任务总数（新增 + 覆盖 + 跳过）。
        "sourceCount": source_count,
        "copied": copied,
        // 覆盖模式才有值：这次被刷新了内容的目标任务名。
        "overwritten": overwritten,
        // ⚠️ 用 `skippedNames` 而不是 `skipped`：`switch_to` 里「整体跳过」用的是布尔
        //    `skipped`，同名字段两种类型会让前端判断炸掉（这坑刚踩过）。
        "skippedNames": skipped,
        // 目标账号里"同名同周期但多余"的行数（>0 = 历史数据本来就有重复，本次没动它们）。
        "extraDuplicates": extra_duplicates,
        // 覆盖模式才有值：这次被合并（软删除）的重复行所属任务名。
        "mergedNames": merged,
        "copiedRuns": runs_total,
        "copiedRuntimeStates": states_total,
    }))
}

/// 目标账号下与该任务"同一个任务"的行 id —— **同名 + 同周期**、未软删除。
///
/// ## 为什么不再比 `prompt`（2026-11-11 修）
///
/// 界面上写的规则是「同名同周期的算同一个任务」，但旧实现把 `prompt` 也塞进了
/// WHERE 条件。于是用户在账号 A 里把正文从 `test` 改成 `test111`、再切回账号 B 时，
/// 两边 prompt 不等 ⇒ 判定"不是同一个任务" ⇒ 又插了一条。界面上就并列出现
/// 两个「test / 每月 1 日 15:23」，看起来像去重逻辑失效 —— 实际是键比多了。
///
/// ## 为什么要"等价"而不是字符串全等
///
/// `rrule` 是客户端拼出来的字符串，同一套规则可能有 `RRULE:` 前缀、大小写、
/// 分段顺序（`FREQ=MONTHLY;BYHOUR=15;BYMONTHDAY=1` vs 另一种顺序）等差异。
/// 字符串全等会把它们当成两件事 —— 用户看到的"同周期"却匹配不上。
/// 这里按分号段排序后比较，语义上完全等价。
///
/// 正常 1 条；历史脏数据可能多条，按创建时间升序返回：
/// 第一条作为"代表行"，其余在覆盖模式下软删除合并、跳过模式下只报数。
fn same_key_ids(
    conn: &Connection,
    owner_uid: &str,
    name: &str,
    rrule: &str,
) -> Result<Vec<String>, String> {
    let want = normalize_rrule(rrule);
    // name 过滤下推到 SQL（有索引可用、也避免全表扫）；rrule 的"等价"比较
    // 在 SQL 里没有等价函数，只能在取回后于 Rust 侧比对 —— 同一账号下同名任务
    // 数量是个位数，代价可忽略。
    let mut stmt = conn
        .prepare(
            "select id, rrule from automations
             where owner_user_id = ?1 and deleted_at is null and name = ?2
             order by created_at asc, id asc",
        )
        .map_err(|e| format!("检查目标账号是否已有同名任务失败：{e}"))?;
    let rows = stmt
        .query_map(params![owner_uid, name], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| format!("读取同键任务失败：{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("遍历同键任务失败：{e}"))?;
    Ok(rows
        .into_iter()
        .filter(|(_, rr)| normalize_rrule(rr) == want)
        .map(|(id, _)| id)
        .collect())
}

/// 把 `rrule` 归一成"语义等价即可比较"的形式。
///
/// - 去空白、转大写、去掉 `RRULE:` 前缀；
/// - 按 `;` 拆段、每段去掉首尾空白、丢掉空段，**排序后重新拼接** ——
///   RFC 5545 里各段是并列约束，先后顺序不改变语义。
///
/// 解析不了的（空串、乱码）原样比较：宁可"认不出来 ⇒ 不合并"，
/// 也不能把两条真不同的周期错并成一条。
fn normalize_rrule(rrule: &str) -> String {
    let upper = rrule.trim().to_ascii_uppercase();
    let body = upper.strip_prefix("RRULE:").unwrap_or(&upper).to_string();
    let mut parts: Vec<&str> = body
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    parts.sort_unstable();
    parts.join(";")
}

/// 把一条重复任务标记为已删除（软删除，可恢复）。
///
/// 只写 `deleted_at` / `updated_at` 两列：客户端按 `deleted_at is null` 过滤，
/// 所以界面上立刻只剩一条；运行记录仍挂在这个 id 上，用户想恢复也还有据可查。
fn soft_delete_task(conn: &Connection, id: &str, now: i64) -> Result<usize, String> {
    conn.execute(
        "update automations set deleted_at = ?2, updated_at = ?2
         where id = ?1 and deleted_at is null",
        params![id, now],
    )
    .map_err(|e| format!("标记重复任务已删除失败：{e}"))
}

/// 新增一条任务：逐列 `select` 复制。
///
/// **不用手写每个值**：新增列时这里不会漏，也不会把未知字段写成默认值
/// （那等于悄悄改坏用户的任务配置）。返回 0 = 源行不见了，调用方据此跳过。
fn insert_task(
    conn: &Connection,
    src_id: &str,
    new_id: &str,
    to_uid: &str,
    next_run_at: Option<i64>,
    now: i64,
) -> Result<usize, String> {
    conn.execute(
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
        params![new_id, to_uid, now, src_id, next_run_at],
    )
    .map_err(|e| format!("写入任务行失败：{e}"))
}

/// 用源行**覆盖**目标行的任务定义（覆盖模式）。返回受影响行数（0 = 目标行没了）。
///
/// ## 哪些列**不能**被覆盖
///
/// - `id`：这是这条任务在目标账号里的身份。换了它，`automation_runs.automation_id`
///   会全部指向一个不存在的任务（历史运行记录直接"消失"）。
/// - `owner_user_id`：**绝不能**被源账号的 uid 顶掉 —— 那等于把任务搬走，
///   目标账号反而看不见了（这正是"同步完还是没任务"的经典写法错误）。
/// - `created_at`：界面上"创建时间"应保持目标账号那份的首次出现时间。
/// - `next_run_at` / `updated_at` / `owner_status` / `deleted_at`：单独赋值（见下）。
///
/// 其余列一律取源行的值（含 `prompt` / `rrule` / 模型 / skills / 推送开关 / cwds…）。
/// 用动态列名生成 SQL，是为了"以后表里新增列自动跟着覆盖"，不用回来改第二处。
fn overwrite_task(
    conn: &Connection,
    src_id: &str,
    dst_id: &str,
    next_run_at: Option<i64>,
    now: i64,
) -> Result<usize, String> {
    let cols = columns_of(conn, "automations")?;
    if cols.is_empty() {
        return Err("读不到 automations 的表结构".to_string());
    }
    const KEEP: [&str; 7] = [
        "id",
        "owner_user_id",
        "created_at",
        "next_run_at",
        "updated_at",
        "owner_status",
        "deleted_at",
    ];
    let mut sets: Vec<String> = cols
        .iter()
        .filter(|c| !KEEP.iter().any(|k| *k == c.as_str()))
        .map(|c| format!("\"{c}\" = (select \"{c}\" from automations where id = ?1)"))
        .collect();
    sets.push("next_run_at = ?2".to_string());
    sets.push("updated_at = ?3".to_string());
    // 覆盖后必须让客户端"认"这条任务（历史行可能带着 legacy_unassigned）。
    sets.push("owner_status = 'confirmed'".to_string());
    // 源行已确认未删除；目标行若被软删过，这里一并复活。
    sets.push("deleted_at = null".to_string());
    conn.execute(
        &format!("update automations set {} where id = ?4", sets.join(", ")),
        params![src_id, next_run_at, now, dst_id],
    )
    .map_err(|e| format!("写入覆盖结果失败：{e}"))
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
        let conn = mem_db();

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

        let out = sync_owner_on(&conn, "u1", "u2", SyncMode::Skip).expect("同步应当成功");
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
        let again = sync_owner_on(&conn, "u1", "u2", SyncMode::Skip).unwrap();
        assert_eq!(again.get("copied").and_then(|v| v.as_array()).map(|a| a.len()), Some(0));
        let n: i64 = conn
            .query_row("select count(*) from automations where owner_user_id='u2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    /// 覆盖模式：目标账号已有一条「同名同周期」的任务，`overwrite=true` 时
    /// 应当用源账号的定义刷掉它，且**保留目标那份的 id**（运行记录挂在上面）。
    #[test]
    fn sync_overwrites_same_task_and_keeps_target_id() {
        let conn = Connection::open_in_memory().unwrap();
        seed_schema(&conn);
        conn.execute(
            "insert into automations (id,name,prompt,status,schedule_type,rrule,owner_user_id,created_at,updated_at)
             values ('src','每日汇报','源提示词','confirmed','recurring','FREQ=DAILY','u1',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into automations (id,name,prompt,status,schedule_type,rrule,owner_user_id,created_at,updated_at)
             values ('dst','每日汇报','目标自己的提示词','confirmed','recurring','FREQ=DAILY','u2',1,1)",
            [],
        )
        .unwrap();

        // 跳过模式：目标不动
        let a = sync_owner_on(&conn, "u1", "u2", false).unwrap();
        assert_eq!(a.get("overwrittenNames").and_then(|v| v.as_array()).map(|x| x.len()), Some(0));
        let p: String = conn
            .query_row("select prompt from automations where id='dst'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(p, "目标自己的提示词");

        // 覆盖模式：刷成源提示词，id 仍是 dst（运行记录不会变孤儿）
        let b = sync_owner_on(&conn, "u1", "u2", true).unwrap();
        assert_eq!(b.get("overwrittenNames").and_then(|v| v.as_array()).map(|x| x.len()), Some(1));
        let (id, p2): (String, String) = conn
            .query_row(
                "select id, prompt from automations where owner_user_id='u2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(id, "dst", "覆盖必须保留目标行 id");
        assert_eq!(p2, "源提示词");
        let n: i64 = conn
            .query_row("select count(*) from automations where owner_user_id='u2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "覆盖不该多出一条");
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

    /// 建一张与真实库同形的内存表（真实库加列时这里要一起加，否则测试发现不了漏搬）。
    fn mem_db() -> Connection {
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
        conn
    }

    /// **两种模式的语义分界**：同一份数据，跳过模式下目标行一动不动；
    /// 覆盖模式下定义被源刷新，但 **id / created_at / owner_user_id 必须保持目标那份**。
    ///
    /// 这三个字段要是被覆盖，"运行记录"会指向一个不存在的任务、任务会被搬到源账号名下 ——
    /// 都属于"看起来同步成功、实际数据坏了"的事故。
    #[test]
    fn overwrite_refreshes_definition_but_keeps_identity() {
        let conn = mem_db();
        let past = now_ms() - 3 * 24 * 3600 * 1000;
        let rrule = "FREQ=DAILY;BYHOUR=9;BYMINUTE=30";

        // 源账号 u1：模型 m-old、cwds 指向老路径，并有一条运行记录 + 运行状态
        conn.execute(
            "insert into automations (id,name,prompt,status,next_run_at,cwds,rrule,model_id,owner_user_id,owner_status,created_at,updated_at)
             values ('src','日报','每晚汇总','ACTIVE',?1,'[\"C:/old\"]',?2,'m-old','u1','confirmed',1,1)",
            params![past, rrule],
        )
        .unwrap();
        conn.execute(
            "insert into automation_runs (thread_id,automation_id,status,thread_title,created_at,updated_at)
             values ('src:111','src','ACCEPTED','源账号那次',111,111)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into automation_runtime_state (automation_id,last_run_at,running) values ('src',222,0)",
            [],
        )
        .unwrap();

        // 目标账号 u2：**同名同周期同 prompt**（= 判定为"同一个任务"），但模型/状态不同，
        // 且自己有一条运行记录 —— 覆盖模式**不许**把它删掉。
        conn.execute(
            "insert into automations (id,name,prompt,status,next_run_at,cwds,rrule,model_id,owner_user_id,owner_status,created_at,updated_at)
             values ('dst','日报','每晚汇总','PAUSED',?1,'[\"C:/new\"]',?2,'m-new','u2','legacy_unassigned',5,5)",
            params![past, rrule],
        )
        .unwrap();
        conn.execute(
            "insert into automation_runs (thread_id,automation_id,status,thread_title,created_at,updated_at)
             values ('dst:222','dst','DONE','目标账号自己那次',222,222)",
            [],
        )
        .unwrap();

        // ① 跳过模式：目标行一行都不改，只报名字
        let skip = sync_owner_on(&conn, "u1", "u2", SyncMode::Skip).unwrap();
        assert_eq!(skip.get("mode").and_then(Value::as_str), Some("skip"));
        assert_eq!(skip.get("copied").and_then(Value::as_array).map(|a| a.len()), Some(0));
        assert_eq!(skip.get("overwritten").and_then(Value::as_array).map(|a| a.len()), Some(0));
        assert_eq!(skip.get("skippedNames").and_then(Value::as_array).map(|a| a.len()), Some(1));
        let (status, model): (String, Option<String>) = conn
            .query_row(
                "select status, model_id from automations where id='dst'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "PAUSED", "跳过模式不许动目标行");
        assert_eq!(model.as_deref(), Some("m-new"));

        // ② 覆盖模式：定义按源刷新
        let ow = sync_owner_on(&conn, "u1", "u2", SyncMode::Overwrite).unwrap();
        assert_eq!(ow.get("mode").and_then(Value::as_str), Some("overwrite"));
        assert_eq!(ow.get("copied").and_then(Value::as_array).map(|a| a.len()), Some(0));
        assert_eq!(ow.get("overwritten").and_then(Value::as_array).map(|a| a.len()), Some(1));
        assert_eq!(ow.get("skippedNames").and_then(Value::as_array).map(|a| a.len()), Some(0));
        // 子表：源那条补进来（thread_id 按目标 id 重写，所以能算"新的一行"）
        assert_eq!(ow.get("copiedRuns").and_then(Value::as_u64), Some(1));

        #[allow(clippy::type_complexity)]
        let (id, owner, created, status, model, cwds, next, owner_status): (
            String,
            String,
            i64,
            String,
            Option<String>,
            String,
            Option<i64>,
            String,
        ) = conn
            .query_row(
                "select id, owner_user_id, created_at, status, model_id, cwds, next_run_at, owner_status
                   from automations where id='dst'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(id, "dst", "id 是运行记录的外键目标，绝不能换");
        assert_eq!(owner, "u2", "owner 被源 uid 顶掉 = 任务被搬走，目标账号反而看不见");
        assert_eq!(created, 5, "创建时间保持目标账号那份的首次出现时间");
        assert_eq!(status, "ACTIVE", "状态以源为准");
        assert_eq!(model.as_deref(), Some("m-old"), "模型等细项要被刷新");
        assert_eq!(cwds, "[\"C:/old\"]", "cwds 等细项要被刷新");
        assert_eq!(owner_status, "confirmed");
        assert!(
            next.unwrap_or(0) > now_ms(),
            "覆盖后 next_run_at 必须挪到未来，否则客户端立刻补跑一次"
        );

        // 目标账号下仍然只有一条任务（覆盖 ≠ 新增）
        let n: i64 = conn
            .query_row("select count(*) from automations where owner_user_id='u2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        // 运行记录：两条并存 —— 目标自己那条没被删，源那条补进来且 thread_id 已重写
        let (total, from_src, own): (i64, i64, i64) = conn
            .query_row(
                "select count(*),
                        coalesce(sum(case when thread_id='dst:111' then 1 else 0 end), 0),
                        coalesce(sum(case when thread_id='dst:222' then 1 else 0 end), 0)
                   from automation_runs where automation_id='dst'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((total, from_src, own), (2, 1, 1));

        // 再覆盖一次：插入幂等，不许越覆盖越肥
        let again = sync_owner_on(&conn, "u1", "u2", SyncMode::Overwrite).unwrap();
        assert_eq!(
            again.get("copiedRuns").and_then(Value::as_u64),
            Some(0),
            "同一条运行记录不许重复插入（thread_id 已按目标 id 重写，天然去重）"
        );
        let total2: i64 = conn
            .query_row("select count(*) from automation_runs where automation_id='dst'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total2, 2);
    }

    /// 覆盖模式下目标账号**没有**同款：照常新增（不能因为叫"覆盖"就什么都不做）。
    #[test]
    fn overwrite_inserts_when_target_has_none() {
        let conn = mem_db();
        conn.execute(
            "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
             values ('src','周报','写周报','ACTIVE','[]','FREQ=WEEKLY;BYDAY=MO;BYHOUR=10;BYMINUTE=0','u1','confirmed',1,1)",
            [],
        )
        .unwrap();
        let out = sync_owner_on(&conn, "u1", "u2", SyncMode::Overwrite).unwrap();
        assert_eq!(out.get("copied").and_then(Value::as_array).map(|a| a.len()), Some(1));
        assert_eq!(out.get("overwritten").and_then(Value::as_array).map(|a| a.len()), Some(0));
        let n: i64 = conn
            .query_row("select count(*) from automations where owner_user_id='u2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    /// 周期等价判定：只差前缀/大小写/分段顺序的 rrule 必须认成同一个周期。
    #[test]
    fn rrule_normalization_is_semantic() {
        let a = "FREQ=MONTHLY;BYMONTHDAY=1;BYHOUR=15;BYMINUTE=23";
        assert_eq!(normalize_rrule(a), normalize_rrule("RRULE:FREQ=MONTHLY;BYMONTHDAY=1;BYHOUR=15;BYMINUTE=23"));
        assert_eq!(normalize_rrule(a), normalize_rrule("freq=monthly; byminute=23 ;byhour=15;bymonthday=1"));
        // 真不同的周期不能被归一成一样
        assert_ne!(normalize_rrule(a), normalize_rrule("FREQ=MONTHLY;BYMONTHDAY=2;BYHOUR=15;BYMINUTE=23"));
        assert_ne!(normalize_rrule(""), normalize_rrule(a));
    }

    /// **本次 bug 的回归测试**：同名同周期、只有 prompt 不同 —— 必须认成同一个任务。
    ///
    /// 真实数据形态（本机 `test` 那两条）：一条 prompt=`test`，一条 prompt=`test111`，
    /// 名字与 rrule 完全相同，界面上并列显示成两个「test / 每月 1 日 15:23」。
    #[test]
    fn same_name_and_period_is_one_task_even_if_prompt_differs() {
        let conn = mem_db();
        let rr = "FREQ=MONTHLY;BYMONTHDAY=1;BYHOUR=15;BYMINUTE=23";
        // 源账号：prompt = "test"
        conn.execute(
            "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
             values ('src','test','test','ACTIVE','[]',?1,'u1','confirmed',1,1)",
            params![rr],
        )
        .unwrap();
        // 目标账号已有：同名同周期，prompt = "test111"
        conn.execute(
            "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
             values ('dst','test','test111','ACTIVE','[]',?1,'u2','confirmed',2,2)",
            params![rr],
        )
        .unwrap();

        // 跳过模式：认得出是同一个任务 ⇒ 跳过，绝不新增第二条
        let skip = sync_owner_on(&conn, "u1", "u2", SyncMode::Skip).unwrap();
        assert_eq!(skip.get("copied").and_then(Value::as_array).map(|a| a.len()), Some(0));
        assert_eq!(skip.get("skippedNames").and_then(Value::as_array).map(|a| a.len()), Some(1));
        let n: i64 = conn
            .query_row("select count(*) from automations where owner_user_id='u2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "同名同周期不该再插一条");

        // 覆盖模式：刷新的就是那一条，prompt 被改成源的内容
        let ow = sync_owner_on(&conn, "u1", "u2", SyncMode::Overwrite).unwrap();
        assert_eq!(ow.get("overwritten").and_then(Value::as_array).map(|a| a.len()), Some(1));
        assert_eq!(ow.get("copied").and_then(Value::as_array).map(|a| a.len()), Some(0));
        let (id, prompt): (String, String) = conn
            .query_row("select id, prompt from automations where owner_user_id='u2'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(id, "dst", "必须覆盖目标那条，而不是新建");
        assert_eq!(prompt, "test");
    }

    /// 目标账号已有两条同名同周期（历史脏数据）：覆盖模式要**合并成一条**。
    #[test]
    fn overwrite_merges_duplicate_rows() {
        let conn = mem_db();
        let rr = "FREQ=MONTHLY;BYMONTHDAY=1;BYHOUR=15;BYMINUTE=23";
        conn.execute(
            "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
             values ('src','test','new','ACTIVE','[]',?1,'u1','confirmed',1,1)",
            params![rr],
        )
        .unwrap();
        for (id, prompt, at) in [("dup1", "old", 10), ("dup2", "older", 20)] {
            conn.execute(
                "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
                 values (?1,'test',?2,'ACTIVE','[]',?3,'u2','confirmed',?4,?4)",
                params![id, prompt, rr, at],
            )
            .unwrap();
        }
        let out = sync_owner_on(&conn, "u1", "u2", SyncMode::Overwrite).unwrap();
        assert_eq!(out.get("overwritten").and_then(Value::as_array).map(|a| a.len()), Some(1));
        // 多出来的那一行被合并（软删除）
        assert_eq!(out.get("mergedNames").and_then(Value::as_array).map(|a| a.len()), Some(1));
        let alive: i64 = conn
            .query_row(
                "select count(*) from automations where owner_user_id='u2' and deleted_at is null",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alive, 1, "界面上应该只剩一条");
        let total: i64 = conn
            .query_row("select count(*) from automations where owner_user_id='u2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "是软删除，不是物理删除（可恢复）");
        // 留下的是创建最早那条（身份稳定）
        let kept: String = conn
            .query_row(
                "select id from automations where owner_user_id='u2' and deleted_at is null",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, "dup1");
    }

    /// 就地合并（不切号）：保留最早那条的身份，内容取最后编辑的那份，其余软删除。
    #[test]
    fn merge_duplicates_keeps_identity_and_newest_content() {
        let conn = mem_db();
        let rr = "FREQ=MONTHLY;BYMONTHDAY=1;BYHOUR=15;BYMINUTE=23";
        // 最早创建（id=first），但内容最旧；最新编辑的是 id=last
        conn.execute(
            "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,next_run_at,created_at,updated_at)
             values ('first','test','old','ACTIVE','[]',?1,'u9','confirmed',777,10,10)",
            params![rr],
        )
        .unwrap();
        conn.execute(
            "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
             values ('last','test','newest','ACTIVE','[]',?1,'u9','confirmed',20,20)",
            params![rr],
        )
        .unwrap();
        // 不同周期的同名任务：不许被合并
        conn.execute(
            "insert into automations (id,name,prompt,status,cwds,rrule,owner_user_id,owner_status,created_at,updated_at)
             values ('other','test','x','ACTIVE','[]','FREQ=DAILY;BYHOUR=9;BYMINUTE=0','u9','confirmed',30,30)",
            [],
        )
        .unwrap();

        let out = merge_duplicates_on(&conn, "u9").unwrap();
        assert_eq!(out.get("groups").and_then(Value::as_u64), Some(1));
        assert_eq!(out.get("merged").and_then(Value::as_u64), Some(1));

        let alive: Vec<String> = {
            let mut stmt = conn
                .prepare("select id from automations where owner_user_id='u9' and deleted_at is null order by id")
                .unwrap();
            let v = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            v
        };
        assert_eq!(alive, vec!["first".to_string(), "other".to_string()]);
        let (prompt, next): (String, Option<i64>) = conn
            .query_row("select prompt, next_run_at from automations where id='first'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(prompt, "newest", "内容取最后编辑的那份");
        assert_eq!(next, Some(777), "下次运行时间不该被合并改掉");
    }

    /// 模式字符串必须闭环：`settings.json` 里存的就是 `as_str()`，读回来还能认出自己。
    #[test]
    fn sync_mode_roundtrips_through_string() {
        for m in [SyncMode::Skip, SyncMode::Overwrite] {
            assert_eq!(SyncMode::parse_loose(m.as_str()), Some(m));
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{}\"", m.as_str()));
        }
        assert_eq!(SyncMode::default(), SyncMode::Skip, "缺省必须是老行为（跳过）");
        assert_eq!(SyncMode::parse_loose("  BOGUS "), None);
        assert_eq!(SyncMode::parse_loose("Overwrite"), Some(SyncMode::Overwrite));
    }
}
