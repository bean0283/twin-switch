//! WorkBuddy → Trae 会话移植。
//!
//! 把 WorkBuddy 的明文会话记录（`workbuddy_source` 解析出的回合序列）**转换**为 Trae
//! 关系库所需的 10 张表的行，再复用现有「页面级解密 → 改数据 → 逐页加密回写 → 备份 +
//! 原子替换」链路写回目标账号的加密库。
//!
//! 为什么必须转换：两者格式完全不同。WorkBuddy 是「一个会话一个 JSONL 文本文件」的扁平
//! 事件流；Trae 是 SQLCipher 加密库里的强关系结构，用户文本走 `chat_message_general`、
//! 助手过程走 `chat_message_task`、流式文本走 `history_v2`，会话可续聊还依赖
//! `session_project` / `chat_turn` / `agent_run` 三张关联表（缺行会导致打开后无法继续对话）。
//!
//! 字段映射（实测对齐真实库取值）：
//!
//! | WorkBuddy | Trae |
//! | --- | --- |
//! | `sessions.id` / 标题 / cwd | 重新生成 `chat_session.session_id` / `session_title` / `project` |
//! | `message(user)` 文本 | `chat_message_general.content` + `history_v2`(user_input) |
//! | `assistant_text` 中间叙述 | `chat_message_task` 的 `plan_item.thought` |
//! | `reasoning` | `chat_message_task` 的 `plan_item.reasoning_content` |
//! | `function_call` + `_result` | `chat_message_task` 的 `plan_item.tool_call_info` |
//! | 最后一条 assistant 文本 | `history_v2`(llm_default)，即该回合的最终回答 |
//! | 一个回合 | `chat_turn`（completed）+ `task` + `agent_run` |
//!
//! **模板取自目标库自身**：`chat_session.context` / `chat_message.user_message_context` /
//! `chat_turn.context` 的形状随客户端版本演进，因此不硬编码，而是读目标库里最近一条真实行
//! 做模板、只替换其中与本次会话相关的字段（project_id / 提问文本），以保证与已安装版本
//! 字段级兼容。
//!
//! ## 四条实测硬约束（首版导入失效后的修复，踩坑记录）
//!
//! 首版导入「写进去了但客户端不显示、且客户端加载变慢」，排查后确认四条硬约束：
//!
//! 1. **`task` 表必须写，且行数 ≡ `chat_turn` 行数**。客户端「任务列表」按 `task` 渲染；
//!    漏写时会话在库里存在、列表里却看不见。实测 6/6 真实会话满足 `task 数 == 回合数`。
//! 2. **id 前 8 位必须是 unix 秒的 hex**，后 16 位才随机（24 位）。实测 6/6 真实会话
//!    `id[:8]` 换算后精确等于其 `created_at`；纯随机 id 会被排到错误的时间区间。
//! 3. **`history_v2_id` 是 24 位**（不是 32 位）；`agent_run_id` 是 UUID **v5**（不是 v4）。
//! 4. **`project` 行必须合法**：`absolute_path` 用「小写盘符 + 反斜杠」（`d:\htw\签到`），
//!    `biz_project_id` = `project_id` 数值减一的 24 位 hex。否则匹配不到已有项目、重复建库。
//!
//! 另外同一源会话重复导入必须幂等：`session_id` 由源 id 确定性派生，导入前先删旧行。
//!
//! ## 第二批实测硬约束（v0.0.5：导入成功但「内容不对 + 客户端变慢」的根因）
//!
//! 首版修完上面四条后，会话已能出现在列表里，但出现两个新症状，逐列比真实行后确认：
//!
//! 5. **`chat_turn.context` 绝不能整段照抄真实行**。它内部嵌着
//!    `persist_user_message_context.query`（**该会话的提问原文**）、`workspace_folders`、
//!    `context_usage`（文件索引结果）、`trace_id`、`token_usage`、`chat_start_time/end_time`。
//!    照抄的后果是导入会话在客户端里**显示别的会话的提问**（实测：导入「向助手打招呼」
//!    却显示「提交更新到github，包括Releases，并…」），并让本会话宣称一个不属于它的工作区
//!    ——客户端据此去预热/索引别的工程，这就是「会话加载异常慢」。做法：只借字段形状 +
//!    `model_info`（唯一必须照用者），其余全部重建，未知字段按类型中性化（见 `build_turn_context`）。
//! 6. **助手的可见回答必须写进 `chat_message_task` 末尾的 `finish` 项**：
//!    真实行里最后一个 `plan_item` 的 `tool_call_info.name == "finish"`，
//!    回答正文在 `params.summary`。只写 `{"messages":[]}` 会让 Trae 里助手气泡**空白**。
//! 7. **`chat_session.context.skill_list_revisions` 的键名里含 session_id**，
//!    照抄会夹带别的会话 id（见 `build_session_context`）。
//! 8. `chat_message.revertible` 恒为 1；`agent_run` 是**会话级**（73 回合的会话只有 3 条），
//!    不是每回合一条。
//! 9. **工作目录不存在时不要声明工作区**（`workspace_folders` 留空），否则客户端反复挂载失败。
//!
//! ## 空间占用与「不该做的工作量」（v0.0.6）
//!
//! SQLCipher 是**逐页独立加密**（每页自带随机 IV + HMAC、页号参与 HMAC、页长恒 4096），
//! 所以「只改了几页」根本不需要重写整库。导入链路因此改成：
//!
//! 1. `trae_export::ensure_decrypted` 取一份**与实时库一致的明文快照**——
//!    签名（源库大小 / mtime / 首页 salt / 首页字段）一致就零解密直接复用；
//! 2. 把快照**字节复制**为工作明文（零加解密），合并 WAL、写入转换结果；
//! 3. `trae_import::write_db_incremental` 把原库密文整份复制成新库，**只对变动的页**重新加密
//!    回写（salt 沿用原库首页，否则未变动页的 HMAC 全部失效），每写一页立刻回读解密比对；
//! 4. 备份 + 原子替换，然后把工作明文提升为新快照。
//!
//! 实测（真实 279 MB / 71477 页库）：整库只有 5 页变动时，重写量 0.02 MB、用时约 0.5 s；
//! 而旧实现是「解密 71477 页 + 加密 71477 页 + 再解密 71477 页自检」。
//! 备份只保留最新 2 份（每份 ≈ 库大小）。
//!
//! ## 第三批实测硬约束（v0.0.7：导入直接失败）
//!
//! 10. **`agent_run_id` 是唯一由会话 id 确定性推导的 id（`uuid5(sid:start)`），
//!     写之前必须无条件清掉同 id 的残留行。** 客户端删除会话时只清 `chat_session`，
//!     会留下 `agent_run` 孤儿行（实测活库 25 行里 14 行是孤儿），此时
//!     「chat_session 查不到 ⇒ 不用清」的旧逻辑会让 INSERT 撞
//!     `UNIQUE constraint failed: agent_run.agent_run_id`，把整次导入打断。
//!     见 `purge_session_rows`。其余表（message/turn/task/history）的 id 后 16 位是
//!     UUID v4 随机数，天然不会撞，所以只有 `agent_run` 需要这层兜底。

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::modules::config::store_dir;
use crate::modules::trae_decrypt::{hex_to_bytes, verify_page1_hmac};
use crate::modules::trae_delete::merge_wal_into_plain;
use crate::modules::trae_discover::{database_path, get_client};
use crate::modules::trae_import::{patch_reserved_field, write_db_incremental};
use crate::modules::trae_memory_scan::{load_saved_key, save_key, scan_for_key};
use crate::modules::trae_switch::{is_running, kill_all, launch, wait_until_stopped};
use crate::modules::workbuddy_accounts;
use crate::modules::workbuddy_source::{self, WbBody, WbSession, WbTurn};

const PAGE_SZ: usize = 4096;

// ---------------------------------------------------------------------------
// 基础工具
// ---------------------------------------------------------------------------

/// Trae 的 id 约定：**前 8 位 = unix 秒的十六进制，后 16 位随机**，共 24 位。
///
/// 实测目标库 6/6 的真实会话完全吻合——例如 `6abf7aed853237fb78521db8` 的前缀
/// `0x6abf7aed` = 1790933741，正是该会话的 `created_at`。id 在客户端里同时承担排序键
/// 与时间范围查询的职责，纯随机 id 会把记录落到错误的时间区间（我们首版生成的
/// `13a30c1b…` 前缀换算出来是 1980 年），因此必须按同一规则构造。
fn hex_id_at(secs: i64, n: usize) -> String {
    let mut out = format!("{:08x}", secs.max(0) as u32);
    while out.len() < n {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(n);
    out
}

/// `biz_project_id`：实测等于 `project_id`(96 位整数) 减一，同为 24 位 hex
/// （`…6258d706956d3479` → `…6258d706956d3478`）。
fn biz_of(project_id: &str) -> String {
    let v = u128::from_str_radix(project_id, 16).unwrap_or(0);
    format!("{:024x}", v.saturating_sub(1))
}

/// agent_run_id 实测是 UUID v5（第三段首字符为 `5`，如
/// `5ee812da-b97a-54d5-a2d8-5ab83d60b6d7`），并非 v4。
fn uuid5(name: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

/// Trae 的 `absolute_path` 约定：**盘符小写 + 反斜杠**（`d:\htw\签到`）。
///
/// 不归一化就匹配不到已有项目行，同一目录会一次次堆出新的 project（我们首版把
/// `D:/htw/test` 原样写入，于是已有的 `d:\htw\test` 项目没被复用，白建了一份）。
fn normalize_abs_path(p: &str) -> String {
    let s = p.trim().replace('/', "\\");
    let mut cs: Vec<char> = s.chars().collect();
    if cs.len() >= 2 && cs[1] == ':' {
        cs[0] = cs[0].to_ascii_lowercase();
    }
    cs.into_iter().collect()
}

/// 由 WorkBuddy 会话 id 派生**确定性** session_id。
///
/// 时间戳前缀取源会话起点（保留原始时间语义），后 16 位取源 id 的 hex 前 16 位。
/// 于是「同一个源会话重复导入」会命中同一个 session_id —— 我们据此覆盖而不是
/// 堆出重复会话（用户第一次测试时因为 id 随机，导两次就真的出现两个会话）。
fn det_session_id(wb_id: &str, start: i64) -> String {
    let hex: String = wb_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if hex.len() >= 16 {
        format!("{:08x}{}", start.max(0) as u32, &hex[..16])
    } else {
        hex_id_at(start, 24)
    }
}

/// 毫秒时间戳 → 秒（Trae 用秒；WorkBuddy 用毫秒）。已是秒的原样返回。
fn secs(ms: i64) -> i64 {
    if ms > 1_000_000_000_000 {
        ms / 1000
    } else {
        ms
    }
}

/// 目标库的 agent 身份三元组（agent_type / agent_id / agent_name）。
///
/// 优先取目标库 `chat_turn` 里出现最多的取值——这样与目标客户端当前版本天然一致；
/// 库里没有历史行时按客户端回退到实测值（solo-cn 为 `solo_work_lite` / SOLO MTC，
/// trae-cn 为 `solo_agent` / SOLO Agent）。
fn detect_agent(conn: &Connection, client_key: &str) -> (String, String, String) {
    let found = conn
        .query_row(
            "SELECT agent_type, agent_id, agent_name FROM chat_turn \
             WHERE ifnull(agent_type,'')<>'' \
             GROUP BY agent_type, agent_id, agent_name ORDER BY count(*) DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .ok();
    found.unwrap_or_else(|| default_agent(client_key))
}

fn default_agent(client_key: &str) -> (String, String, String) {
    match client_key {
        "trae-cn" => (
            "solo_agent".to_string(),
            "solo_agent".to_string(),
            "SOLO Agent".to_string(),
        ),
        _ => (
            "solo_work_lite".to_string(),
            "solo_work_lite".to_string(),
            "SOLO MTC".to_string(),
        ),
    }
}

/// agent 的展示名（plan_item.agent_display_name，与 agent_name 略有差异）。
fn agent_display_name(agent_id: &str) -> String {
    match agent_id {
        "solo_agent" => "SOLO Agent".to_string(),
        "solo_coder" => "SOLO Coder".to_string(),
        "solo_work_lite" => "SOLO Work Lite".to_string(),
        other => other.to_string(),
    }
}

fn now_secs() -> i64 {
    chrono::Local::now().timestamp()
}

// ---------------------------------------------------------------------------
// 目标库：project 与模板
// ---------------------------------------------------------------------------

/// 为目标账号 + 工作目录找到（或新建）project 行，返回 project_id。
///
/// Trae 按 `user_id` + `absolute_path` 组织工作区；路径先按客户端约定归一化再比对，
/// 已存在则复用，避免同一目录堆出多份。`start` 用作新建行的 `created_at`
/// （真实库里 project 总是先于它名下的会话存在）。
fn ensure_project(conn: &Connection, uid: &str, cwd: &str, start: i64) -> Result<String, String> {
    let path = normalize_abs_path(cwd);
    let existing: Option<String> = conn
        .query_row(
            "SELECT project_id FROM project \
             WHERE user_id=?1 AND absolute_path=?2 AND ifnull(deleted_at,0)=0 LIMIT 1",
            params![uid, path],
            |r| r.get(0),
        )
        .ok();
    if let Some(pid) = existing {
        return Ok(pid);
    }
    let ts = if start > 0 { start } else { now_secs() };
    let pid = hex_id_at(ts, 24);
    let biz = biz_of(&pid);
    let name = Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty());
    conn.execute(
        "INSERT INTO project \
         (project_id, source, user_id, name, description, absolute_path, biz_project_id, \
          created_at, updated_at, deleted_at, workspace_status, last_active_at, work_mode) \
         VALUES (?1, 'native-ide', ?2, ?3, NULL, ?4, ?5, ?6, ?6, NULL, 'virtual', ?6, 'work')",
        params![pid, uid, name, path, biz, ts],
    )
    .map_err(|e| format!("写入 project 失败: {e}"))?;
    Ok(pid)
}

/// 递归把「可能来自别的会话」的取值清空：数组→`[]`，对象→保持形状递归清空，
/// 字符串→`""`，数字→`0`，布尔→`false`，null 保持。
///
/// 用途：借真实行的**字段形状**（抗客户端版本漂移），但不借它的**取值**。
fn neutralize(v: &Value) -> Value {
    match v {
        Value::Array(_) => json!([]),
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                out.insert(k.clone(), neutralize(val));
            }
            Value::Object(out)
        }
        Value::Null => Value::Null,
        Value::String(_) => json!(""),
        Value::Bool(_) => json!(false),
        Value::Number(_) => json!(0),
    }
}

/// 与 `neutralize` 相同，但**标量一律清成 `null`**。
///
/// 专用于 `render_context`：真实行的取值只有 `null` 与空数组两种，没有空串/0 这种形态，
/// 清成 `null` 才与真实行同形（`current_file` 真实就是 `null`，写空串会让形状对不上）。
fn nullify(v: &Value) -> Value {
    match v {
        Value::Array(_) => json!([]),
        Value::Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                out.insert(k.clone(), nullify(val));
            }
            Value::Object(out)
        }
        _ => Value::Null,
    }
}

/// 读目标库最近一条真实 context JSON 作为「字段形状」模板。
fn shape_template(conn: &Connection, sql: &str) -> Value {
    conn.query_row(sql, [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v.is_object() && !v.as_object().map(|m| m.is_empty()).unwrap_or(true))
        .unwrap_or_else(|| json!({}))
}

/// 构造 `chat_session.context`。
///
/// **模板只借字段形状，取值逐项重建**。首版把真实行整段抄下来，于是
/// `skill_list_revisions` 的**键名**里夹带了别的会话 id
/// （`conversation_skill_list_revision_<别的 session_id>_…`），客户端会把它当作本会话
/// 的技能修订去解析。
fn build_session_context(conn: &Connection, pid: &str) -> String {
    let tpl = shape_template(
        conn,
        "SELECT context FROM chat_session WHERE ifnull(context,'')<>'' \
         ORDER BY updated_at DESC LIMIT 1",
    );
    let mut out = tpl
        .as_object()
        .cloned()
        .unwrap_or_default();
    let vm_mode = tpl.get("vm_mode").cloned().unwrap_or_else(|| json!("aha_vm"));
    let limit = tpl
        .get("server_history_cache_limit")
        .cloned()
        .unwrap_or_else(|| json!(1000));

    out.insert("activated_feature_flags".into(), json!([]));
    out.insert("file_read_state_cache".into(), Value::Null);
    out.insert("cc_file_read_state_cache".into(), Value::Null);
    out.insert("has_remote_counterpart".into(), json!(false));
    out.insert("is_worktree".into(), json!(false));
    out.insert("last_real_project_id".into(), json!(pid));
    out.insert("server_history_cache_limit".into(), limit);
    // 本会话尚无任何技能修订缓存
    out.insert("skill_list_revisions".into(), json!({}));
    out.insert("vm_mode".into(), vm_mode);
    Value::Object(out).to_string()
}

/// 取目标库中最近一条用户消息的 `user_message_context` 作为模板，替换提问内容。
fn template_user_context(conn: &Connection, user_text: &str) -> String {
    let mut ctx = conn
        .query_row(
            "SELECT user_message_context FROM chat_message \
             WHERE message_role='user' AND ifnull(user_message_context,'')<>'' \
             ORDER BY created_at DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}));
    if !ctx.is_object() {
        ctx = json!({});
    }
    let query_json = json!([{ "type": "text", "data": { "content": user_text } }]).to_string();
    let obj = ctx.as_object_mut().expect("已保证是对象");
    obj.insert("query".into(), json!(query_json));
    obj.insert("parsed_query".into(), json!([user_text]));
    obj.insert("turn_type".into(), json!("default"));
    obj.insert("hide_user_query".into(), json!(false));
    obj.insert("is_goal_loop".into(), json!(false));
    obj.insert("is_background_wakeup".into(), json!(false));
    ctx.to_string()
}

/// 构造 `chat_turn.context`（**首版最严重的坑**）。
///
/// 首版直接取目标库最近一条真实 `chat_turn.context` 整段照抄，只改了 outer 结构，于是
/// 我们导入的会话里带着**上一个真实会话**的：
///
/// - `persist_user_message_context.query` / `parsed_query` —— 它的**提问原文**。客户端按
///   这份 context 渲染用户气泡，所以导入的会话在 Trae 里显示的是**别人会话的提问**
///   （用户实测：导入「向助手打招呼」却看到「提交更新到github，包括Releases，并…」）。
/// - `workspace_folders` —— 它的工作目录（`d:\htw\签到`），本会话并不属于那里。
/// - `context_usage` —— 它的文件索引结果（另一项目的一堆文件），客户端会据此去解析/预热。
/// - `trace_id` / `token_usage` / `fee_usage` / `chat_start_time` / `chat_end_time`。
///
/// 正确做法：**只借字段形状 + `model_info`（唯一随版本漂移且必须照用的部分），其余全部重建**。
/// 未知的新字段按类型中性化，保持形状不与新版错位。
fn build_turn_context(
    conn: &Connection,
    workspace: Option<&str>,
    user_text: &str,
    t_start: i64,
    t_end: i64,
) -> String {
    let tpl = shape_template(
        conn,
        "SELECT context FROM chat_turn WHERE ifnull(context,'')<>'' AND context<>'{}' \
         ORDER BY updated_at DESC LIMIT 1",
    );
    let model_info = tpl
        .pointer("/persist_user_message_context/model_info")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let version_code = tpl.get("version_code").cloned().unwrap_or(Value::Null);
    let locale = tpl.get("locale").cloned().unwrap_or_else(|| json!("zh-cn"));
    let render_context = tpl.get("render_context").cloned().unwrap_or_else(|| json!({}));
    let token_usage = tpl.get("token_usage").cloned().unwrap_or_else(|| json!({}));
    let render_variables = tpl.get("render_variables").cloned().unwrap_or_else(|| json!({}));

    // 未知新字段：保留形状、清空取值
    let mut out = serde_json::Map::new();
    if let Some(m) = tpl.as_object() {
        for (k, v) in m {
            out.insert(k.clone(), neutralize(v));
        }
    }

    out.insert("references".into(), json!([]));
    out.insert("render_context".into(), nullify(&render_context));
    out.insert("render_variables".into(), neutralize(&render_variables));
    out.insert("metadata".into(), Value::Null);
    out.insert("locale".into(), locale);
    out.insert("rewritten_query".into(), Value::Null);
    out.insert(
        "persist_user_message_context".into(),
        json!({
            "ppe_env_name": "",
            "model_info": model_info,
            "parsed_query": [user_text],
            "asr_times": Value::Null,
            "is_in_plan_mode": Value::Null,
            "is_in_spec_mode": Value::Null,
            "is_in_code_mode": Value::Null,
            "command_type": Value::Null,
            "is_ralph_loop": false,
            "turn_type": "default",
            "query": [{ "type": "text", "data": { "content": user_text } }],
            "is_goal_loop": false,
            "hide_user_query": false,
            "is_background_wakeup": false
        }),
    );
    out.insert("trace_id".into(), json!(hex_id_at(now_secs(), 32)));
    out.insert("fee_usage".into(), Value::Null);
    out.insert("max_fee_usage".into(), Value::Null);
    out.insert("token_usage".into(), neutralize(&token_usage));
    out.insert("notifications".into(), Value::Null);
    out.insert("search_reference_data".into(), Value::Null);
    out.insert("document_contexts".into(), json!([]));
    // 工作区只认本会话真正所属的目录；目录不存在时留空，避免客户端去预热一个不存在的工程
    out.insert(
        "workspace_folders".into(),
        match workspace {
            Some(w) => json!([w]),
            None => json!([]),
        },
    );
    out.insert("context_usage".into(), json!({ "contexts": [] }));
    out.insert("is_user_canceled".into(), Value::Null);
    out.insert("chat_start_time".into(), json!(t_start.saturating_mul(1000)));
    out.insert("chat_end_time".into(), json!(t_end.saturating_mul(1000)));
    out.insert("version_code".into(), version_code);
    Value::Object(out).to_string()
}

// ---------------------------------------------------------------------------
// 过程事件 → plan_item 步骤
// ---------------------------------------------------------------------------

/// 一个 plan_item 步骤：叙述/思考 + 一次工具调用及其结果。
#[derive(Default, Clone)]
struct Step {
    thought: String,
    reasoning: String,
    call_id: String,
    tool: String,
    params: String,
    result: Option<String>,
    status: String,
    ts_ms: i64,
}

/// 把回合内按序排列的事件归并成步骤序列。
///
/// WorkBuddy 的节奏是「叙述/思考 → 工具调用 → 工具结果」循环，末尾可能剩一段只有叙述、
/// 没有工具调用的内容——那正是该回合的最终回答，不生成 plan_item。
fn build_steps(turn: &WbTurn) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut pending: Option<Step> = None;
    for e in &turn.events {
        match e.kind.as_str() {
            "assistant_text" => {
                let s = pending.get_or_insert_with(Step::default);
                if !s.thought.is_empty() {
                    s.thought.push('\n');
                }
                s.thought.push_str(&e.text);
            }
            "reasoning" => {
                let s = pending.get_or_insert_with(Step::default);
                if !s.reasoning.is_empty() {
                    s.reasoning.push('\n');
                }
                s.reasoning.push_str(&e.text);
            }
            "function_call" => {
                let mut s = pending.take().unwrap_or_default();
                s.call_id = e.call_id.clone();
                s.tool = e.name.clone();
                s.params = e.text.clone();
                s.ts_ms = turn.updated_at;
                steps.push(s);
            }
            "function_call_result" => {
                // 优先按 call_id 配对，缺失时退化为「同名且尚无结果的最后一步」
                let mut idx = None;
                for (i, s) in steps.iter().enumerate().rev() {
                    if s.result.is_some() {
                        continue;
                    }
                    if (!e.call_id.is_empty() && s.call_id == e.call_id)
                        || (e.call_id.is_empty() && s.tool == e.name)
                    {
                        idx = Some(i);
                        break;
                    }
                }
                if let Some(i) = idx {
                    steps[i].result = Some(e.text.clone());
                    steps[i].status = e.status.clone();
                }
            }
            _ => {}
        }
    }
    if let Some(rest) = pending {
        steps.push(rest);
    }
    steps
}

/// 工具参数：尽量还原成 JSON 对象，失败则退回字符串包装。
///
/// 额外拆一层：若结果解析出来是**字符串**且内容又是 JSON 对象/数组，说明上游把参数
/// 序列化了两次（双重编码）。这会让 Trae 的 `tool_call_info.params` 变成字符串，
/// 卡片取不到参数 —— 所以这里兜底再解一次。
fn parse_params(raw: &str) -> Value {
    let t = raw.trim();
    if t.is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(t) {
        Ok(Value::String(inner)) => {
            let it = inner.trim();
            if it.starts_with('{') || it.starts_with('[') {
                if let Ok(v) = serde_json::from_str::<Value>(it) {
                    return v;
                }
            }
            json!({ "raw": it })
        }
        Ok(v) => v,
        Err(_) => json!({ "raw": t }),
    }
}

/// 把 WorkBuddy 的工具状态映射成 **Trae 认识的那几种**。
///
/// 根因（v0.0.11 复盘）：WorkBuddy 侧 `function_call_result.status` **恒为 `completed`**，
/// 而 Trae 前端的 `transformToolCallStatus` 只认
/// `success` / `failed` / `skipped` / `canceled` / `running` / `pending`，
/// 其余一律落到 `default` → **被当成 `running`（进行中）**。
/// 于是导入的每个工具卡片都停在「运行中」，结果区不渲染 ——
/// 表现就是「WorkBuddy 里有详细过程，Trae 里显示得奇奇怪怪」。
fn map_tool_status(raw: &str, ok: bool) -> &'static str {
    match raw {
        "error" | "failed" => "failed",
        "canceled" | "cancelled" => "canceled",
        "skipped" => "skipped",
        "running" | "pending" => "running",
        // `completed` 是 WorkBuddy 的措辞，必须翻成 Trae 的 `success`
        "completed" | "success" | "partial_success" | "no_need_execute" | "" => {
            if ok {
                "success"
            } else {
                "failed"
            }
        }
        _ => {
            if ok {
                "success"
            } else {
                "failed"
            }
        }
    }
}

/// 构造工具卡片的结果数据（`tool_call_info.result.data`）。
///
/// Trae 按工具类型从 `data` 取内容渲染。实测前端代码
/// （`@byted-icube/ai-modules-chat/dist/index.mjs`）里命令行卡片读的是
/// `data.stderr || data.stdout` 与 `data.display_command || data.command`；
/// 原生各工具的形态又各不相同（Shell → stdout/exit_code；Edit/Write → changes；
/// MCP/Skill 类 → `content: [{type,text}]`）。
/// 之前这里恒写成 `{}`，等于**把工具的输出全丢了** —— 卡片只剩个空壳。
fn tool_result_data(tool: &str, params: &Value, output: &str) -> Value {
    if output.trim().is_empty() {
        return json!({});
    }
    let s_of = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| params.get(*k).and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    };
    match tool {
        // 命令行：对齐 harness.dll 的 `RunCommandOutput`
        "Bash" | "Shell" | "Exec" | "PowerShell" | "RunCommand" => {
            let cmd = s_of(&["command", "cmd"]);
            json!({
                "status": "completed",
                "stdout": output,
                "stderr": "",
                "exit_code": 0,
                "command": cmd,
                "display_command": cmd,
                "cwd": s_of(&["cwd", "working_directory"]),
                "stdout_complete": true
            })
        }
        // 文件写改：对齐原生 Edit/Write 的 changes 形态
        "Write" | "Edit" | "MultiEdit" | "DeleteFile" => json!({
            "can_show_diff": true,
            "change_time": 0,
            "file_path": s_of(&["file_path", "path"]),
            "changes": [],
            "content": output
        }),
        // 其余一律用通用文本块（原生 MCP 类工具就是 `content: [{type,text}]`）
        _ => json!({ "content": [{ "type": "text", "text": output }] }),
    }
}

/// 构造 `chat_message_task.content`（助手过程流）。
///
/// `task_id` 必须与 `task` 表那一行的 `task_id` 一致——客户端按它把过程流关联回任务。
///
/// **末尾必须补一条 `finish` 项**：实测真实库里助手这条可见回答并不在 `history_v2`，
/// 而是落在最后一个 `plan_item` 的 `tool_call_info.name == "finish"` 的
/// `params.summary` 里（并在 `result.data.summary` 留空）。首版只写了 `{"messages":[]}`，
/// 于是 Trae 里助手气泡是**空白的**——这就是「WorkBuddy 有回答、Trae 看不到」的原因。
fn build_task_content(
    steps: &[Step],
    agent: &(String, String, String),
    run_id: &str,
    task_id: &str,
    final_text: &str,
    end_ms: i64,
) -> String {
    let (agent_id, _, _) = agent;
    let display = agent_display_name(agent_id);
    let mut items: Vec<Value> = Vec::new();
    for s in steps {
        // 末段只有叙述、没有工具调用的内容属最终回答，不进过程流
        if s.tool.is_empty() {
            continue;
        }
        let ok = s.result.is_some();
        // `completed` → `success`：Trae 前端只认 success/failed/skipped/canceled/running，
        // 透传 `completed` 会被当成「进行中」，卡片不渲染结果（v0.0.11 根因）。
        let status = map_tool_status(&s.status, ok);
        let params_val = parse_params(&s.params);
        let data_val = tool_result_data(&s.tool, &params_val, s.result.as_deref().unwrap_or(""));
        let item_ts = secs(s.ts_ms);
        // 字段必须与 Trae 原生 plan_item 逐一对齐（实测原生库 + 客户端模块
        // `modules/ai-agent/{ai_agent,harness}.dll` 的字段表）：
        //   · tool_call_info 必带 `already_emitted_generating_event`/`already_emitted_run_event`
        //     （原生取值 false / true）；**缺失时客户端反序列化整条 plan_item 失败，
        //     于是工具卡片不渲染 —— 表现为「WorkBuddy 里的详细过程在 Trae 看不到」**。
        //   · result 必带 `error_variant`/`is_async`（原生绝大多数为 null）。
        //   · plan_item 必带 `confirm_info`/`parent_agent_run_ids`（原生绝大多数为 null）。
        //   · `params` 必须是对象（`parse_params`），写成 JSON 字符串会让卡片取不到参数。
        let tool_call_info = json!({
            "id": hex_id_at(item_ts, 24),
            "name": s.tool,
            "params": params_val,
            "result": {
                "status": status,
                "error_message": "",
                "error_variant": Value::Null,
                "data": data_val,
                "render": Value::Null,
                "is_truncated": Value::Null,
                "is_async": Value::Null,
                "interrupt": Value::Null,
                "images": Value::Null
            },
            "meta": { "llm_toolcall_id": s.call_id },
            "already_emitted_generating_event": false,
            "already_emitted_run_event": true
        });
        items.push(json!({
            "id": hex_id_at(item_ts, 24),
            "type": "plan_item",
            "plan_item": {
                "id": hex_id_at(item_ts, 24),
                "agent_id": agent_id,
                "agent_display_name": display,
                "agent_run_id": run_id,
                "sub_agent_call_description": Value::Null,
                "render_mode": Value::Null,
                "agent_status": { "status": if ok { "completed" } else { "failed" }, "run_mode": "foreground" },
                "thought": s.thought,
                "plan_type": Value::Null,
                "reasoning_content": s.reasoning,
                "timing": {
                    "generated_at_ms": s.ts_ms,
                    "tool_call_started_at_ms": s.ts_ms,
                    "tool_call_finished_at_ms": s.ts_ms
                },
                "tool_call_info": tool_call_info,
                "confirm_info": Value::Null,
                "parent_agent_run_ids": Value::Null,
                "hide": Value::Null
            }
        }));
    }

    // 末尾的 finish 项：客户端据此渲染助手的最终回答。
    // 真实库 129/129 条助手消息都至少有一个 plan_item 且末项是 finish，所以即使这一回合
    // 没拿到正文也照写（`summary` 留空），保持结构一致，避免出现空 messages 的畸形行。
    let end_s = secs(end_ms);
    let reasoning = steps
        .iter()
        .rev()
        .map(|s| s.reasoning.as_str())
        .find(|r| !r.trim().is_empty())
        .unwrap_or("")
        .to_string();
    items.push(json!({
        "id": hex_id_at(end_s, 24),
        "type": "plan_item",
        "plan_item": {
            "id": hex_id_at(end_s, 24),
            "agent_id": agent_id,
            "agent_display_name": display,
            "agent_run_id": run_id,
            "sub_agent_call_description": Value::Null,
            "render_mode": Value::Null,
            "agent_status": { "status": "completed", "run_mode": "foreground" },
            "thought": "",
            "plan_type": Value::Null,
            "reasoning_content": reasoning,
            "timing": {
                "generated_at_ms": end_ms,
                "tool_call_started_at_ms": end_ms,
                "tool_call_finished_at_ms": end_ms
            },
            "tool_call_info": {
                "id": hex_id_at(end_s, 24),
                "name": "finish",
                "params": { "summary": final_text },
                "result": {
                    "status": "success",
                    "error_message": "",
                    "error_variant": Value::Null,
                    "data": { "summary": "", "products": Value::Null },
                    "render": Value::Null,
                    "is_truncated": Value::Null,
                    "is_async": Value::Null,
                    "interrupt": Value::Null,
                    "images": Value::Null
                },
                "meta": Value::Null,
                "already_emitted_generating_event": false,
                "already_emitted_run_event": true
            },
            "confirm_info": Value::Null,
            "parent_agent_run_ids": Value::Null,
            "hide": Value::Null
        }
    }));

    json!({ "task_id": task_id, "messages": items }).to_string()
}

/// 构造 `history_v2.messages` 的 raw_messages 包装。
fn history_messages(role: &str, text: &str) -> String {
    json!({
        "raw_messages": [{
            "role": role,
            "content": [{ "type": "text", "text": text }]
        }]
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// 清理：重复导入覆盖 & 历史残骸
// ---------------------------------------------------------------------------

/// 执行只吃一个文本参数的 DELETE；目标表在当前/未来客户端版本里可能不存在，此时静默跳过。
fn exec_ignore_missing(conn: &Connection, sql: &str, val: &str) -> Result<(), String> {
    exec_count_ignore_missing(conn, sql, val).map(|_| ())
}

/// 同 [`exec_ignore_missing`]，但返回受影响行数（用于统计清理掉多少残骸）。
fn exec_count_ignore_missing(conn: &Connection, sql: &str, val: &str) -> Result<usize, String> {
    match conn.execute(sql, params![val]) {
        Ok(n) => Ok(n),
        Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("no such table") => Ok(0),
        Err(e) => Err(format!("{sql} 失败: {e}")),
    }
}

/// 删除某个会话在全部相关表中的行，返回删除总行数。
///
/// 两类关联键：正文表按 `message_id`，其余（含两张 FTS5 虚表）按 `session_id`。
/// 用于①同一源会话重复导入时先清旧行再写；②清理历史错误导入的残骸。
fn delete_session_rows(conn: &Connection, sid: &str) -> Result<usize, String> {
    let mids: Vec<String> = {
        let mut st = conn
            .prepare("SELECT message_id FROM chat_message WHERE session_id=?1")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![sid], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(Result::ok).collect()
    };
    let mut total = 0usize;
    for m in &mids {
        for t in [
            "chat_message_general",
            "chat_message_task",
            "chat_message_chat",
        ] {
            total += exec_count_ignore_missing(conn, &format!("DELETE FROM {t} WHERE message_id=?1"), m)?;
        }
    }
    for t in [
        "agent_run",
        "chat_message",
        "chat_session",
        "chat_session_goal",
        "chat_turn",
        "fts_message_content",
        "fts_session_title",
        "history_todo_list",
        "history_v2",
        "im_session_route",
        "plan",
        "proposal",
        "server_history_info",
        "session_project",
        "task",
        "worktree",
    ] {
        total += exec_count_ignore_missing(conn, &format!("DELETE FROM {t} WHERE session_id=?1"), sid)?;
    }
    Ok(total)
}

/// 清掉「同一源会话 / 同一个确定性 run」可能残留在库里的行，返回删除总行数。
///
/// **关键：不能只在 `chat_session` 查得到时才清。** 客户端（或用户在 Trae 里手动删除）
/// 删掉一个导入过的会话时，只清 `chat_session`，会留下 `agent_run` 等孤儿行——实测本机
/// 库里就有 **14 条 `agent_run` 孤儿**（其中两条正是我们上一轮导入的会话
/// `6ac23c9a…`）。此时 `chat_session` 查不到，但 `agent_run_id` 仍由同一个 `sid` 确定性
/// 算出（`uuid5(sid:start)`），于是 INSERT 直接撞
/// `UNIQUE constraint failed: agent_run.agent_run_id`，把整次导入打断——**这正是
/// v0.0.6「新版本直接导入失败」的根因**。
///
/// 所以我们**无条件**先清一遍：按 `session_id` 清全部关联表，再按 id 兜一遍
/// `agent_run`（孤儿行的 `session_id` 可能为空或被改写）。清理是幂等的，
/// 清不到东西就是 0 行、无副作用。
fn purge_session_rows(conn: &Connection, sid: &str, run_id: &str) -> Result<usize, String> {
    let mut n = delete_session_rows(conn, sid)?;
    match conn.execute(
        "DELETE FROM agent_run WHERE agent_run_id=?1",
        params![run_id],
    ) {
        Ok(c) => n += c,
        Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("no such table") => {}
        Err(e) => return Err(format!("清理残留 agent_run 失败: {e}")),
    }
    Ok(n)
}

/// 清理「有回合却没有 task 行」的会话。
///
/// 真实 Trae 会话的 `task` 行数与 `chat_turn` 行数**恒等**（实测 6/6：2/2、9/9、73/73…）。
/// 违反此不变式只可能来自漏写 `task` 表的早期导入版本——它们在客户端里表现为「列表
/// 里看不见 / 会话加载卡顿」的幽灵记录。删除前逐条打日志，且此时目标库尚未替换、
/// 备份也尚未生成失效（备份在替换前一刻做）。
///
/// 公开供离线干跑 / 修复工具复用（`examples/wb_dryrun.rs`）。
pub fn purge_broken_sessions(conn: &Connection, on_log: &dyn Fn(&str)) -> Result<usize, String> {
    let broken: Vec<(String, String, i64)> = {
        let mut st = conn
            .prepare(
                "SELECT s.session_id, ifnull(s.session_title,''), \
                        (SELECT count(*) FROM chat_turn ct WHERE ct.session_id=s.session_id) \
                 FROM chat_session s \
                 WHERE (SELECT count(*) FROM chat_turn ct WHERE ct.session_id=s.session_id) > 0 \
                   AND NOT EXISTS (SELECT 1 FROM task t WHERE t.session_id=s.session_id)",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        rows.filter_map(Result::ok).collect()
    };
    for (sid, title, turns) in &broken {
        on_log(&format!(
            "   清理异常会话 {sid}（{turns} 个回合 / 0 条 task）「{title}」"
        ));
        delete_session_rows(conn, sid)?;
    }
    Ok(broken.len())
}

/// 清理旧版遗留的**畸形 project 行**。
///
/// 首版把 `biz_project_id` 写成了 6 位（真实是 24 位 hex），这类行只可能来自那个版本。
/// 仅仅清掉「已无任何会话引用」的那些（孤儿），避免误伤正在用的项目。
pub fn purge_malformed_projects(conn: &Connection, on_log: &dyn Fn(&str)) -> Result<usize, String> {
    let bad: Vec<(String, String, String)> = {
        let mut st = conn
            .prepare(
                "SELECT p.project_id, ifnull(p.name,''), ifnull(p.absolute_path,'') FROM project p \
                 WHERE length(ifnull(p.biz_project_id,'')) <> 24 \
                   AND NOT EXISTS (SELECT 1 FROM session_project sp WHERE sp.project_id=p.project_id) \
                   AND NOT EXISTS (SELECT 1 FROM chat_session s WHERE s.project_id=p.project_id)",
            )
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    };
    for (pid, name, path) in &bad {
        on_log(&format!(
            "   清理畸形项目 {pid}（biz_project_id 非 24 位，已无会话引用）「{name}」{path}"
        ));
        exec_ignore_missing(conn, "DELETE FROM project WHERE project_id=?1", pid)?;
    }
    Ok(bad.len())
}

// ---------------------------------------------------------------------------
// 单个会话写入
// ---------------------------------------------------------------------------

/// 单项写入统计。
#[derive(Default, Debug)]
pub struct WriteStats {
    pub turns: usize,
    pub steps: usize,
    pub messages: usize,
    pub history_rows: usize,
    pub skipped_duplicates: usize,
}

/// 把一个 WorkBuddy 会话写入目标明文库（调用方负责解密 / 加密 / 替换）。
///
/// 返回写入统计。若目标库已存在同标题同起点的会话会重复写入——去重由调用方按
/// 「来源标记」处理，本函数只保证单次写入的完整性。
pub fn write_session(
    conn: &Connection,
    client_key: &str,
    dst_uid: &str,
    src: &WbSession,
    body: &WbBody,
    on_log: &dyn Fn(&str),
) -> Result<WriteStats, String> {
    let mut stats = WriteStats::default();
    if body.turns.is_empty() {
        return Err(format!("会话 {} 没有可移植的对话内容", src.id));
    }
    let agent = detect_agent(conn, client_key);
    let cwd = if src.cwd.trim().is_empty() {
        "~".to_string()
    } else {
        src.cwd.clone()
    };
    let title = body
        .ai_title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| src.title.clone());

    let start = secs(if src.created_at > 0 {
        src.created_at
    } else {
        body.turns.first().map(|t| t.created_at).unwrap_or(0)
    });
    let end = secs(if src.updated_at > 0 {
        src.updated_at
    } else {
        body.turns.last().map(|t| t.updated_at).unwrap_or(start * 1000)
    });
    let start = if start > 0 { start } else { now_secs() };
    let end = if end > 0 { end } else { start };

    // 确定性 id：同一源会话重复导入必须命中同一组 id（幂等）。
    // `agent_run_id` 是**唯一**由会话 id 确定性推导出来的 id（`uuid5(sid:start)`），
    // 一旦库里存在同 id 的孤儿行就会撞 UNIQUE 约束，所以必须**无条件**先清一遍，
    // 不能只在 `chat_session` 查得到时才清（详见 `purge_session_rows` 的注释）。
    let sid = det_session_id(&src.id, start);
    let run_id = uuid5(&format!("{sid}:{start}"));
    let wiped = purge_session_rows(conn, &sid, &run_id)?;
    if wiped > 0 {
        on_log(&format!(
            "   清理同源会话的旧记录 {wiped} 行（含客户端删除后残留的孤儿行）"
        ));
        stats.skipped_duplicates += 1;
    }
    let pid = ensure_project(conn, dst_uid, &cwd, start)?;

    // 工作区声明：只在本会话真正所属的目录确实存在时才写进 context。
    // 声明一个不存在的目录会让客户端反复尝试挂载 / 预热该工程，是「会话加载异常慢」的来源之一。
    let abs = normalize_abs_path(&cwd);
    let workspace = if Path::new(&abs).is_dir() {
        Some(abs.clone())
    } else {
        on_log(&format!(
            "   提示：源工作目录 {abs} 在本机不存在，本会话不声明工作区"
        ));
        None
    };

    // 1) chat_session
    conn.execute(
        "INSERT INTO chat_session \
         (session_id, project_id, created_at, updated_at, deleted_at, session_type, \
          agent_process_support, session_title, work_mode, context, session_icon, \
          is_pinned, pinned_at, extra, is_unread, last_unread_turn_id) \
         VALUES (?1, ?2, ?3, ?4, 0, 'side_chat', 'v3', ?5, 'work', ?6, 'personal', \
                 0, 0, '{}', 0, '')",
        params![sid, pid, start, end, title, build_session_context(conn, &pid)],
    )
    .map_err(|e| format!("写入 chat_session 失败: {e}"))?;

    // agent_run 是**会话级**的：实测 73 回合的大会话只有 3 条 run、多数会话恒为 1 条，
    // 并非每回合一条。所有回合的 history / plan_item 都指向这一条 run。
    // （`run_id` 在进函数时就已算好，并在 `purge_session_rows` 里清过同 id 的残留行。）
    conn.execute(
        "INSERT INTO agent_run \
         (agent_run_id, parent_run_id, agent_id, created_at, updated_at, deleted_at, session_id, agent_call_id) \
         VALUES (?1, NULL, ?2, ?3, ?3, 0, ?4, NULL)",
        params![run_id, agent.1, start + 1, sid],
    )
    .map_err(|e| format!("写入 agent_run 失败: {e}"))?;

    let ts_cursor = start;

    let mut last_turn_id = String::new();
    let mut msg_index: i64 = 0;
    let count = body.turns.len();
    for (i, turn) in body.turns.iter().enumerate() {
        let mut t_start = secs(turn.created_at);
        if t_start <= 0 {
            t_start = ts_cursor + i as i64;
        }
        let mut t_end = secs(turn.updated_at);
        if t_end < t_start {
            t_end = t_start;
        }

        let steps = build_steps(turn);
        let real_steps: Vec<&Step> = steps.iter().filter(|s| !s.tool.is_empty()).collect();
        // 该回合的最终回答：优先 WorkBuddy 的 assistant_text；为空时退回「最后一条工具结果」，
        // 再退回「最后一段叙述」——真实库里助手消息从不空着，兜底能少一个空白气泡。
        let final_text = {
            let direct = turn.assistant_text.trim();
            if !direct.is_empty() {
                direct.to_string()
            } else {
                real_steps
                    .iter()
                    .rev()
                    .find_map(|s| {
                        s.result
                            .as_deref()
                            .map(str::trim)
                            .filter(|r| !r.is_empty())
                            .map(str::to_string)
                    })
                    .or_else(|| {
                        steps
                            .iter()
                            .rev()
                            .map(|s| s.thought.trim())
                            .find(|t| !t.is_empty())
                            .map(str::to_string)
                    })
                    .unwrap_or_default()
            }
        };

        // 2) 用户消息（context 形状取自目标库真实行，只替换提问文本）
        let u_msg = hex_id_at(t_start, 24);
        msg_index += 1;
        let u_ctx = template_user_context(conn, &turn.user_text);
        conn.execute(
            "INSERT INTO chat_message \
             (session_id, message_id, message_type, message_role, message_index, is_archived, \
              reply_to_message_id, user_message_context, created_at, updated_at, deleted_at, revertible) \
             VALUES (?1, ?2, 'general', 'user', ?3, 0, '', ?4, ?5, ?5, 0, 1)",
            params![sid, u_msg, msg_index, u_ctx, t_start],
        )
        .map_err(|e| format!("写入用户 chat_message 失败: {e}"))?;
        conn.execute(
            "INSERT INTO chat_message_general (message_id, content, created_at, updated_at, deleted_at) \
             VALUES (?1, ?2, ?3, ?3, 0)",
            params![
                u_msg,
                json!([{ "type": "text", "text_content": turn.user_text }]).to_string(),
                t_start
            ],
        )
        .map_err(|e| format!("写入 chat_message_general 失败: {e}"))?;
        // 用户侧历史行
        conn.execute(
            "INSERT INTO history_v2 \
             (history_v2_id, session_id, message_id, messages, token_usage, summary, \
              summarized_above, created_at, updated_at, deleted_at, agent_type, content_source, \
              agent_run_id) \
             VALUES (?1, ?2, ?3, ?4, 0, NULL, 0, ?5, ?5, 0, ?6, 'user_input', ?7)",
            params![
                hex_id_at(t_start, 24),
                sid,
                u_msg,
                history_messages("user", &turn.user_text),
                t_start,
                agent.0,
                run_id
            ],
        )
        .map_err(|e| format!("写入用户 history_v2 失败: {e}"))?;
        stats.history_rows += 1;

        // 3) 助手消息（过程 + 最终回答）
        let a_msg = hex_id_at(t_end, 24);
        msg_index += 1;
        let task_id = hex_id_at(t_start, 24);
        let task_content =
            build_task_content(&steps, &agent, &run_id, &task_id, &final_text, t_end * 1000);
        conn.execute(
            "INSERT INTO chat_message \
             (session_id, message_id, message_type, message_role, message_index, is_archived, \
              reply_to_message_id, user_message_context, created_at, updated_at, deleted_at, revertible) \
             VALUES (?1, ?2, 'task', 'assistant', ?3, 0, ?4, '', ?5, ?5, 0, 1)",
            params![sid, a_msg, msg_index, u_msg, t_end],
        )
        .map_err(|e| format!("写入助手 chat_message 失败: {e}"))?;
        conn.execute(
            "INSERT INTO chat_message_task \
             (message_id, task_id, content, summary, created_at, updated_at, deleted_at) \
             VALUES (?1, ?2, ?3, '', ?4, ?4, 0)",
            params![a_msg, task_id, task_content, t_end],
        )
        .map_err(|e| format!("写入 chat_message_task 失败: {e}"))?;

        // 3b) task 行：客户端的「任务列表」按这张表渲染，且实测 task 行数 ≡ 回合数。
        //     漏写它会造成「会话写进去了但列表里看不见 / 加载卡顿」——首版正是栽在这里。
        conn.execute(
            "INSERT INTO task \
             (task_id, session_id, message_id, status, summary, created_at, updated_at, \
              deleted, messages, server_task_id) \
             VALUES (?1, ?2, ?3, 'completed', '', ?4, ?5, 0, '[]', NULL)",
            params![task_id, sid, a_msg, t_start, t_end],
        )
        .map_err(|e| format!("写入 task 失败: {e}"))?;

        // 每个工具步骤一条 history_v2（content_source = 工具名），还原过程流
        for s in &real_steps {
            let text = if !s.thought.is_empty() {
                s.thought.clone()
            } else {
                s.reasoning.clone()
            };
            let payload = json!({
                "raw_messages": [{
                    "role": "assistant",
                    "content": [{ "type": "text", "text": text, "image_url": Value::Null,
                                  "video_url": Value::Null, "cache_control": Value::Null }],
                    "name": Value::Null,
                    "tool_call_id": Value::Null,
                    "tool_calls": [{
                        "index": 0,
                        "id": s.call_id,
                        "type": "function",
                        "function_call": { "name": s.tool, "arguments": s.params }
                    }]
                }]
            })
            .to_string();
            conn.execute(
                "INSERT INTO history_v2 \
                 (history_v2_id, session_id, message_id, messages, token_usage, summary, \
                  summarized_above, created_at, updated_at, deleted_at, agent_type, content_source, \
                  agent_run_id) \
                 VALUES (?1, ?2, ?3, ?4, 0, NULL, 0, ?5, ?5, 0, ?6, ?7, ?8)",
                params![
                    hex_id_at(t_end, 24),
                    sid,
                    a_msg,
                    payload,
                    t_end,
                    agent.0,
                    s.tool,
                    run_id
                ],
            )
            .map_err(|e| format!("写入过程 history_v2 失败: {e}"))?;
            stats.history_rows += 1;
            stats.steps += 1;
        }

        // 最终回答（同时镜像进 chat_message_task 的 finish 项，客户端渲染后者）
        if !final_text.trim().is_empty() {
            conn.execute(
                "INSERT INTO history_v2 \
                 (history_v2_id, session_id, message_id, messages, token_usage, summary, \
                  summarized_above, created_at, updated_at, deleted_at, agent_type, content_source, \
                  agent_run_id) \
                 VALUES (?1, ?2, ?3, ?4, 0, NULL, 0, ?5, ?5, 0, ?6, 'llm_default', ?7)",
                params![
                    hex_id_at(t_end, 24),
                    sid,
                    a_msg,
                    history_messages("assistant", &final_text),
                    t_end,
                    agent.0,
                    run_id
                ],
            )
            .map_err(|e| format!("写入回答 history_v2 失败: {e}"))?;
            stats.history_rows += 1;
        }

        // 4) chat_turn（续聊的语义结构）
        //    context **只借形状**：整段照抄会把别的会话的提问原文 / 工作区 / token 用量
        //    一起带进来（首版实测事故），见 `build_turn_context`。
        let turn_id = hex_id_at(t_start, 24);
        conn.execute(
            "INSERT INTO chat_turn \
             (session_id, turn_id, reply_to_message_id, response_message_id, requirement_id, \
              rewritten_user_message, turn_status, error_message, context, created_at, updated_at, \
              deleted_at, agent_type, agent_id, agent_name, additional_context, is_worktree, products) \
             VALUES (?1, ?2, ?3, ?4, '', '', 'completed', '', ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, 0, '{}')",
            params![
                sid,
                turn_id,
                u_msg,
                a_msg,
                build_turn_context(conn, workspace.as_deref(), &turn.user_text, t_start, t_end),
                t_start,
                t_end,
                agent.0,
                agent.1,
                agent.2,
                json!({
                    "related_to_workspace": false,
                    "smt": "disabled_by_remote",
                    "cmt": "disabled_by_remote",
                    "refresh_project_memento": false
                })
                .to_string()
            ],
        )
        .map_err(|e| format!("写入 chat_turn 失败: {e}"))?;
        last_turn_id = turn_id;

        stats.turns += 1;
        stats.messages += 2;
        if count > 8 && i % 8 == 0 {
            on_log(&format!("   已转换 {}/{} 个回合", i + 1, count));
        }
    }

    // 回写最后回合 id（未读定位用）
    conn.execute(
        "UPDATE chat_session SET last_unread_turn_id=?1 WHERE session_id=?2",
        params![last_turn_id, sid],
    )
    .map_err(|e| format!("回写 last_unread_turn_id 失败: {e}"))?;

    // 6) session_project（缺行会导致打开会话后无法继续对话）
    conn.execute(
        "INSERT INTO session_project (project_id, session_id, created_at) VALUES (?1, ?2, ?3)",
        params![pid, sid, start],
    )
    .map_err(|e| format!("写入 session_project 失败: {e}"))?;

    on_log(&format!(
        "  会话「{}」→ {} 个回合 / {} 个工具步骤 / {} 条历史行",
        title, stats.turns, stats.steps, stats.history_rows
    ));
    Ok(stats)
}

// ---------------------------------------------------------------------------
// 编排：WorkBuddy → 目标 Trae 账号
// ---------------------------------------------------------------------------

/// 导入互斥标记。
///
/// 整条链路要「关目标客户端 → 解密 → 改 → 加密 → 替换 → 重启」，耗时数分钟。若前端重复
/// 点击或并发触发，两轮导入会同时改同一个库并各自替换文件（实测日志里出现过两条
/// 「已解析 1 个 WorkBuddy 会话」交错），既翻倍占用又可能互相覆盖。这里直接拒绝并发。
static IMPORTING: AtomicBool = AtomicBool::new(false);

/// 保证函数退出（含提前 return / panic）时释放互斥标记。
struct ImportGuard;

impl Drop for ImportGuard {
    fn drop(&mut self) {
        IMPORTING.store(false, Ordering::SeqCst);
    }
}

/// 保留最新 `keep` 份备份，删除更旧的，回收磁盘（每份 ≈ 目标库大小，279 MB 量级）。
///
/// 实际清理逻辑走共享的 [`crate::modules::trae_export::prune_backups`]（按客户端分组，
/// 字典序=时间序），这里只负责补日志。
fn prune_backups(client_key: &str, keep: usize, on_log: &dyn Fn(&str)) -> usize {
    let root = crate::modules::trae_export::wb_import_backup_dir();
    let (removed, freed) = crate::modules::trae_export::prune_backups(&root, keep);
    if removed > 0 {
        on_log(&format!(
            "已清理 {removed} 份 {client_key} 旧备份，回收 {:.1} MB（保留最新 {keep} 份）",
            freed as f64 / 1_048_576.0
        ));
    }
    removed
}

/// 列出本机 WorkBuddy 会话（供前端勾选）。
pub fn list_source_sessions() -> Result<Value, String> {
    let available = workbuddy_source::is_available();
    let sessions = if available {
        workbuddy_source::list_sessions()?
    } else {
        Vec::new()
    };
    // 账号显示名：`uid …97eac1` 这种尾部 6 位在多账号时根本分不清谁是谁，
    // 这里把解析出的人名/手机号一并给出，前端按 user_id 查表显示。
    let accounts: Vec<Value> = workbuddy_accounts::resolve()
        .into_iter()
        .map(|a| {
            json!({
                "uid": a.uid,
                "label": a.label(),
                "name": a.name,
                "meta": a.meta(),
                "kind": a.kind,
                "edition": a.edition,
                "is_primary": a.is_primary,
                "name_source": a.source,
            })
        })
        .collect();
    Ok(json!({
        "available": available,
        "data_root": workbuddy_source::data_root().to_string_lossy(),
        "accounts": accounts,
        "sessions": sessions,
    }))
}

/// 只读预览：把 WorkBuddy 会话转换成 Trae 行的统计（不写任何库）。
pub fn preview(session_ids: &[String]) -> Result<Value, String> {
    let mut out = Vec::new();
    for sid in session_ids {
        let body = workbuddy_source::load_body(sid)?;
        let turns = body.turns.len();
        let mut steps = 0usize;
        for t in &body.turns {
            steps += build_steps(t).iter().filter(|s| !s.tool.is_empty()).count();
        }
        out.push(json!({
            "session_id": sid,
            "ai_title": body.ai_title,
            "turns": turns,
            "tool_steps": steps,
        }));
    }
    Ok(json!({ "preview": out }))
}

/// 执行 WorkBuddy → Trae 导入。
///
/// 流程与 Trae↔Trae 导入一致（复用同一套页面级加解密链路）：
/// 关闭目标客户端 → 取密钥 → 解密目标库 → **合并 WAL 已提交帧** → 写入转换后的行 →
/// 逐页加密回写 → 备份 + 原子替换 → 自检 → 重启客户端。
pub fn import_sessions(
    dst_client_key: &str,
    dst_uid: &str,
    session_ids: &[String],
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(cb) = on_log {
            cb(m);
        }
    };
    if IMPORTING.swap(true, Ordering::SeqCst) {
        return Err("已有导入任务正在进行中，请等它结束后再试（重复点击会让两轮导入互相覆盖）".into());
    }
    let _guard = ImportGuard;
    let Some(dst_client) = get_client(dst_client_key) else {
        return Err(format!("未知目标客户端：{dst_client_key}"));
    };
    if session_ids.is_empty() {
        return Err("没有选择要导入的 WorkBuddy 会话".into());
    }
    if !workbuddy_source::is_available() {
        return Err(format!(
            "本机未检测到 WorkBuddy 数据：{}",
            workbuddy_source::data_root().display()
        ));
    }

    // 0) 源：解析全部选中会话（先读，避免中途关客户端失败后白跑）
    let mut sources: Vec<(WbSession, WbBody)> = Vec::new();
    let listed = workbuddy_source::list_sessions()?;
    for sid in session_ids {
        // 单条会话读不出来**只跳过**，不打断整批：多选时不该因为其中一条
        // （没正文 / 列表已过期 / 文件被清掉）就让其它会话也白跑一遍。
        let Some(meta) = listed.iter().find(|s| &s.id == sid).cloned() else {
            log(&format!("会话 {sid} 已不在本机列表中，跳过"));
            continue;
        };
        let body = match workbuddy_source::load_body(sid) {
            Ok(b) => b,
            Err(e) => {
                log(&format!("会话「{}」读取失败，跳过：{e}", meta.title));
                continue;
            }
        };
        if body.turns.is_empty() {
            log(&format!("会话「{}」没有对话内容，跳过", meta.title));
            continue;
        }
        sources.push((meta, body));
    }
    if sources.is_empty() {
        return Err("所选会话都没有可移植的对话内容".into());
    }
    log(&format!("已解析 {} 个 WorkBuddy 会话", sources.len()));

    // 1) 目标库与密钥
    //    顺序很重要：**先取密钥、再退客户端**。没有存盘密钥时只能从进程内存里扫，
    //    而扫描要求客户端还活着——旧代码先杀进程再扫描，等于把这条路堵死。
    let dst_db = database_path(dst_client);
    if !dst_db.is_file() {
        return Err(format!(
            "目标账号 {} 在本机没有会话库，请先用它登录一次 Trae",
            dst_client.label
        ));
    }
    let enc_key_hex = match load_saved_key(dst_client_key) {
        Some(k) => k,
        None => {
            log("目标账号没有存盘密钥，需要从进程内存扫描…");
            let scan = scan_for_key(dst_client_key, &dst_db, None)
                .map_err(|e| format!("扫描目标密钥失败：{e}"))?;
            let k = scan.key.ok_or("未在目标客户端进程中找到有效密钥")?;
            save_key(dst_client_key, &k)?;
            k
        }
    };
    if is_running(dst_client) {
        log(&format!("目标客户端 {} 正在运行，先将其退出…", dst_client.label));
        let killed = kill_all(dst_client);
        log(&format!("已结束 {} 个进程，等待退出…", killed.len()));
        if !wait_until_stopped(dst_client, 10_000) {
            return Err("目标客户端未能退出，无法写入数据库".into());
        }
    }
    {
        use std::io::Read;
        let mut page1 = [0u8; PAGE_SZ];
        std::fs::File::open(&dst_db)
            .map_err(|e| format!("打开目标库失败: {e}"))?
            .read_exact(&mut page1)
            .map_err(|e| format!("读目标库首页失败: {e}"))?;
        let key = hex_to_bytes(&enc_key_hex);
        if !verify_page1_hmac(&key, &page1) {
            return Err("目标库密钥不匹配（密钥过期或账号已重新登录），请重新扫描密钥".into());
        }
    }
    // 目标账号必须真的是该 uid（防止写错账号）
    if let Some(live_uid) = live_uid_of(dst_client_key) {
        if live_uid != dst_uid {
            log(&format!(
                "提示：当前登录 uid 与目标账号不一致（目标 …{}），导入会写入目标账号的本地库",
                uid_suffix(dst_uid)
            ));
        }
    }

    // 2) 取得「与实时库一致」的明文快照：一致时**零解密**直接复用。
    //    Trae 的库跑在 WAL 模式，未 checkpoint 前实时库文件的字节不会变，所以
    //    「大小 + mtime + 首页 salt + 首页关键字段」一致即可判定快照仍是
    //    「实时库的纯解密结果」——不必为了导入一个小会话把整库重新解密一遍。
    log("准备目标库明文（与实时库比对，一致则直接复用快照）…");
    let ensure = crate::modules::trae_export::ensure_decrypted(
        dst_client_key,
        Some(&|m| log(&format!("   {m}"))),
    )?;
    if ensure.reused {
        log(&format!(
            "明文快照与实时库一致，直接复用（本轮未做整库解密）· {} 页",
            ensure.pages
        ));
    } else {
        log(&format!("已解密实时库 {} 页", ensure.pages));
    }
    let snapshot = std::path::PathBuf::from(&ensure.path);

    let work = store_dir()
        .join("workbuddy_import")
        .join(format!("{dst_client_key}-{}", chrono::Local::now().timestamp()));
    std::fs::create_dir_all(&work).map_err(|e| format!("创建工作目录失败: {e}"))?;
    let plain = work.join("target-plain.db");

    // 3) 复制一份工作明文（纯字节复制，零加解密），再合并 WAL 已提交帧——
    //    否则其他会话写在 WAL 里的最新数据会随替换一起丢失。
    std::fs::copy(&snapshot, &plain).map_err(|e| format!("复制明文快照失败: {e}"))?;
    let wal = dst_db.with_extension("db-wal");
    match merge_wal_into_plain(&plain, &wal, &enc_key_hex) {
        Ok(n) if n > 0 => log(&format!("已合并 WAL 中 {n} 个已提交页面帧")),
        Ok(_) => {}
        Err(e) => log(&format!("WAL 合并跳过：{e}")),
    }
    patch_reserved_field(&plain)?;

    // 4) 转换 + 写入明文副本
    let conn = Connection::open(&plain).map_err(|e| format!("打开目标明文副本失败: {e}"))?;

    // 4a) 先清理历史错误导入留下的幽灵会话（有回合却无 task 行），否则它们会一直
    //     拖累客户端加载且在列表里表现为打不开的条目。
    let purged = purge_broken_sessions(&conn, &log)?;
    if purged > 0 {
        log(&format!("已清理 {purged} 个历史异常会话"));
    }
    // 4b) 再清掉旧版遗留、已无人引用的畸形 project 行。
    let bad_projects = purge_malformed_projects(&conn, &log)?;
    if bad_projects > 0 {
        log(&format!("已清理 {bad_projects} 个畸形项目行"));
    }

    let mut total_turns = 0usize;
    let mut total_steps = 0usize;
    let mut written_sessions = 0usize;
    for (meta, body) in &sources {
        log(&format!("转换会话「{}」…", meta.title));
        // 整批写在**明文副本**上，任一条失败就整体放弃（副本会被丢掉），不会留下半截数据；
        // 报错带上会话名，便于一眼看出是哪条出的问题。
        let st = write_session(&conn, dst_client_key, dst_uid, meta, body, &log)
            .map_err(|e| format!("会话「{}」写入失败：{e}", meta.title))?;
        total_turns += st.turns;
        total_steps += st.steps;
        written_sessions += 1;
    }
    conn.close().map_err(|(_, e)| format!("关闭明文副本失败: {e}"))?;
    log(&format!(
        "转换完成：{written_sessions} 个会话 / {total_turns} 个回合 / {total_steps} 个工具步骤"
    ));

    // 5) 增量加密回写：**只重写真正变动的页**。
    //    SQLCipher 逐页独立加密（每页自带随机 IV + HMAC、页号参与 HMAC、页长恒 4096），
    //    所以未变动的页可以直接沿用原密文，只把变动的页重新加密写回；首页 salt 沿用原值
    //    （换 salt 会让所有未变动页的 HMAC 失效）。每写一页立刻回读解密比对。
    log("增量加密回写（只重写变动页，其余沿用原密文）…");
    let new_db = work.join("target-new.db");
    let stats = write_db_incremental(
        &enc_key_hex,
        &dst_db,
        &plain,
        &new_db,
        Some(&|m| log(&format!("   {m}"))),
    )?;
    log(&format!(
        "增量回写完成：全库 {} 页，仅重写 {} 页（{:.1} MB，其中新增 {} 页），其余 {} 页直接复用原密文；用时 {} ms",
        stats.pages,
        stats.changed_pages,
        stats.changed_bytes as f64 / 1_048_576.0,
        stats.appended_pages,
        stats.pages.saturating_sub(stats.changed_pages),
        stats.elapsed_ms,
    ));
    log("进行原子替换…");

    // 6) 自检（新库首页 HMAC 与逐页回读已在回写中校验；这里只确认会话数）+ 备份 + 原子替换
    let verified_sessions: i64 = Connection::open(&plain)
        .ok()
        .and_then(|c| {
            c.query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0))
                .ok()
        })
        .unwrap_or(0);
    let backup_dir = store_dir().join("workbuddy_import_backup").join(format!(
        "{dst_client_key}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let shm = dst_db.with_extension("db-shm");
    for (label, p) in [("主库", &dst_db), ("WAL", &wal), ("SHM", &shm)] {
        if p.exists() {
            let dst = backup_dir.join(p.file_name().unwrap_or_default());
            std::fs::copy(p, &dst).map_err(|e| format!("备份{label}失败: {e}"))?;
        }
    }
    let swap = || -> Result<(), String> {
        if wal.exists() {
            std::fs::remove_file(&wal).map_err(|e| format!("清理 WAL 失败: {e}"))?;
        }
        if shm.exists() {
            std::fs::remove_file(&shm).map_err(|e| format!("清理 SHM 失败: {e}"))?;
        }
        if dst_db.exists() {
            std::fs::remove_file(&dst_db).map_err(|e| format!("移除旧库失败: {e}"))?;
        }
        std::fs::rename(&new_db, &dst_db).map_err(|e| format!("替换数据库失败: {e}"))?;
        Ok(())
    };
    if let Err(e) = swap() {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("替换失败（原库已备份到 {}）：{e}", backup_dir.display()));
    }

    // 7) 把工作明文提升为新快照：它就是新库的**完整明文**（增量回写保证逐页一致），
    //    提升后下一次读取 / 导入无需再解密。
    log("更新解密快照…");
    if let Err(e) = crate::modules::trae_export::promote_snapshot(dst_client_key, &plain) {
        log(&format!("快照更新失败（不影响导入结果，下次读取会重新解密）：{e}"));
        crate::modules::trae_export::drop_snapshot_meta(dst_client_key);
    }
    let _ = std::fs::remove_dir_all(&work);

    // 7b) 只保留最新 2 份备份：每份 ≈ 目标库大小（279 MB 量级），不清理会越攒越多。
    let pruned = prune_backups(dst_client_key, 2, &log);

    // 8) 重启客户端，让导入的会话立即可用
    let relaunched = match launch(dst_client_key, None) {
        Ok(_) => {
            log("导入成功，已自动重启目标客户端");
            true
        }
        Err(e) => {
            log(&format!("导入成功，但自动重启目标客户端失败：{e}"));
            false
        }
    };

    Ok(json!({
        "written_sessions": written_sessions,
        "turns": total_turns,
        "tool_steps": total_steps,
        "pages": stats.pages,
        // 增量回写明细：整库页数 vs 真正重写的页数（让「为什么这么大」一目了然）
        "changed_pages": stats.changed_pages,
        "appended_pages": stats.appended_pages,
        "changed_mb": (stats.changed_bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0,
        "write_ms": stats.elapsed_ms,
        "verified_sessions": verified_sessions,
        "target_client": dst_client_key,
        "target_label": dst_client.label,
        "target_uid": dst_uid,
        "relaunched": relaunched,
        "backup_dir": backup_dir.to_string_lossy(),
        "pruned_backups": pruned,
    }))
}

fn uid_suffix(uid: &str) -> String {
    uid.chars().rev().take(6).collect::<String>().chars().rev().collect()
}

/// 当前登录账号的 uid（用于提示，不阻断导入）。
fn live_uid_of(client_key: &str) -> Option<String> {
    let client = get_client(client_key)?;
    let text = std::fs::read_to_string(crate::modules::trae_discover::storage_json_path(client)).ok()?;
    crate::modules::trae_vault::uid_of_live_storage(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::workbuddy_source::WbEvent;

    fn ev(kind: &str, name: &str, call: &str, text: &str) -> WbEvent {
        WbEvent {
            kind: kind.to_string(),
            name: name.to_string(),
            call_id: call.to_string(),
            status: String::new(),
            text: text.to_string(),
        }
    }

    #[test]
    fn generated_ids_are_lowercase_hex_of_requested_length() {
        for n in [24usize] {
            let id = hex_id_at(1790933741, n);
            assert_eq!(id.len(), n);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }
    }

    #[test]
    fn secs_converts_milliseconds_only() {
        assert_eq!(secs(1790853131000), 1790853131);
        assert_eq!(secs(1790853131), 1790853131);
        assert_eq!(secs(0), 0);
    }

    #[test]
    fn steps_group_narration_reasoning_call_and_result() {
        // 一个回合：叙述 + 思考 → 工具调用 → 结果（末段叙述是最终回答，不生成 plan_item）
        let turn = WbTurn {
            user_text: "问题".into(),
            assistant_text: "最终回答".into(),
            events: vec![
                ev("assistant_text", "", "", "我先看看"),
                ev("reasoning", "", "", "需要读文件"),
                ev("function_call", "Read", "call_1", "{\"path\":\"a\"}"),
                ev("function_call_result", "Read", "call_1", "文件内容"),
                ev("assistant_text", "", "", "最终回答"),
            ],
            created_at: 1790853131000,
            updated_at: 1790853135000,
        };
        let steps = build_steps(&turn);
        assert_eq!(steps.len(), 2, "应产生「工具步骤 + 末段叙述」两步");
        // 第一步：叙述/思考 + 工具 + 结果
        assert_eq!(steps[0].tool, "Read");
        assert_eq!(steps[0].thought, "我先看看");
        assert_eq!(steps[0].reasoning, "需要读文件");
        assert_eq!(steps[0].call_id, "call_1");
        assert_eq!(steps[0].result.as_deref(), Some("文件内容"));
        // 第二步：只有叙述（= 最终回答），没有工具
        assert!(steps[1].tool.is_empty());
        assert_eq!(steps[1].thought, "最终回答");
    }

    #[test]
    fn task_content_only_includes_real_tool_steps() {
        let steps = vec![
            Step {
                thought: "看看".into(),
                reasoning: "想".into(),
                call_id: "c1".into(),
                tool: "Read".into(),
                params: "{\"path\":\"a\"}".into(),
                result: Some("ok".into()),
                status: "completed".into(),
                ts_ms: 1790853135000,
            },
            Step {
                thought: "最终回答".into(),
                ..Default::default()
            },
        ];
        let agent = (
            "solo_work_lite".to_string(),
            "solo_work_lite".to_string(),
            "SOLO MTC".to_string(),
        );
        let raw = build_task_content(
            &steps,
            &agent,
            "run-1",
            "task-abc",
            "这是最终回答",
            1790853174000,
        );
        let v: Value = serde_json::from_str(&raw).expect("必须是合法 JSON");
        assert_eq!(
            v.get("task_id").and_then(Value::as_str),
            Some("task-abc"),
            "过程流里的 task_id 必须等于 task 表那一行"
        );
        let msgs = v.get("messages").and_then(Value::as_array).expect("messages");
        assert_eq!(msgs.len(), 2, "1 个工具步骤 + 1 个 finish 项");
        // 工具步骤
        assert_eq!(msgs[0].get("type").and_then(Value::as_str), Some("plan_item"));
        let pi = msgs[0].get("plan_item").expect("plan_item");
        assert_eq!(pi.get("agent_id").and_then(Value::as_str), Some("solo_work_lite"));
        assert_eq!(pi.get("agent_display_name").and_then(Value::as_str), Some("SOLO Work Lite"));
        assert_eq!(pi.get("agent_run_id").and_then(Value::as_str), Some("run-1"));
        assert_eq!(pi.get("reasoning_content").and_then(Value::as_str), Some("想"));
        assert_eq!(
            pi.pointer("/tool_call_info/name").and_then(Value::as_str),
            Some("Read")
        );
        assert_eq!(
            pi.pointer("/tool_call_info/params/path").and_then(Value::as_str),
            Some("a")
        );
        // 与 Trae 原生 plan_item 逐字段对齐（缺失会让客户端整条 plan_item 反序列化失败，
        // 工具卡片就不渲染 —— 即用户看到的「详细过程在 Trae 里看不到」）。
        let tci = pi.get("tool_call_info").expect("tool_call_info");
        assert!(
            tci.get("params").map(Value::is_object).unwrap_or(false),
            "params 必须是对象而不是 JSON 字符串，否则卡片取不到参数"
        );
        for k in ["already_emitted_generating_event", "already_emitted_run_event"] {
            assert!(
                tci.get(k).map(Value::is_boolean).unwrap_or(false),
                "tool_call_info 缺布尔字段 {k}（原生每个工具项都带）"
            );
        }
        assert!(
            tci.pointer("/result/error_variant").is_some(),
            "result 缺 error_variant（原生即使成功也写 null）"
        );
        assert!(
            tci.pointer("/result/is_async").is_some(),
            "result 缺 is_async（原生即使同步也写 null）"
        );
        for k in ["confirm_info", "parent_agent_run_ids", "hide"] {
            assert!(
                pi.get(k).is_some(),
                "plan_item 缺字段 {k}（原生每个 plan_item 都带，多数为 null）"
            );
        }
        assert!(pi.get("hide") == Some(&Value::Null), "hide 应与原生一致为 null");
        // 末尾 finish 项：客户端从这里取助手的可见回答
        let last = msgs.last().expect("末条");
        assert_eq!(
            last.pointer("/plan_item/tool_call_info/name").and_then(Value::as_str),
            Some("finish")
        );
        assert_eq!(
            last.pointer("/plan_item/tool_call_info/params/summary")
                .and_then(Value::as_str),
            Some("这是最终回答"),
            "助手最终回答必须落在 finish 项的 params.summary"
        );
        assert_eq!(
            last.pointer("/plan_item/tool_call_info/result/status")
                .and_then(Value::as_str),
            Some("success")
        );
    }

    #[test]
    fn task_content_always_ends_with_finish_item() {
        // 真实库 129/129 条助手消息都有至少一个 plan_item 且末项是 finish；没正文时也照写
        let agent = (
            "solo_work_lite".to_string(),
            "solo_work_lite".to_string(),
            "SOLO MTC".to_string(),
        );
        let raw = build_task_content(&[], &agent, "run-1", "t", "   ", 1);
        let v: Value = serde_json::from_str(&raw).unwrap();
        let msgs = v.get("messages").and_then(Value::as_array).expect("messages");
        assert_eq!(msgs.len(), 1, "即使没有正文也要有 finish 项，不能是空数组");
        assert_eq!(
            msgs[0].pointer("/plan_item/tool_call_info/name").and_then(Value::as_str),
            Some("finish")
        );
    }

    #[test]
    fn params_accept_json_object_and_fall_back_to_raw() {
        assert_eq!(parse_params("{\"a\":1}")["a"], 1);
        assert_eq!(parse_params("").as_object().map(|o| o.len()), Some(0));
        assert_eq!(parse_params("not json")["raw"], "not json");
    }

    /// 状态必须落在 Trae 前端 `transformToolCallStatus` 认识的集合里。
    ///
    /// WorkBuddy 恒发 `completed`，一旦透传，前端就落到 `default` → 卡片永远停在
    /// 「进行中」，结果区不渲染。这是「详细过程在 Trae 显示异常」的根因，锁死它。
    #[test]
    fn tool_status_speaks_trae_vocabulary() {
        assert_eq!(map_tool_status("completed", true), "success");
        assert_eq!(map_tool_status("", true), "success");
        assert_eq!(map_tool_status("", false), "failed");
        assert_eq!(map_tool_status("error", true), "failed");
        assert_eq!(map_tool_status("cancelled", false), "canceled");
        assert_eq!(map_tool_status("skipped", true), "skipped");
        let known = ["success", "failed", "skipped", "canceled", "running"];
        for raw in [
            "completed",
            "success",
            "partial_success",
            "no_need_execute",
            "failed",
            "error",
            "canceled",
            "cancelled",
            "skipped",
            "running",
            "pending",
            "",
            "什么鬼",
        ] {
            for ok in [true, false] {
                let got = map_tool_status(raw, ok);
                assert!(
                    known.contains(&got),
                    "状态 {raw:?}(ok={ok}) 映射成 {got:?}，Trae 不认识 → 卡片会卡在「进行中」"
                );
            }
        }
    }

    /// 工具输出必须进 `result.data`，否则卡片只有空壳、没有详细过程。
    #[test]
    fn tool_result_data_keeps_output() {
        let p = json!({ "command": "cargo test", "file_path": "a\\b.txt" });
        let shell = tool_result_data("Bash", &p, "all good");
        assert_eq!(shell["stdout"], "all good");
        assert_eq!(shell["display_command"], "cargo test");
        assert_eq!(shell["exit_code"], 0);

        let read = tool_result_data("Read", &p, "file body");
        assert_eq!(read["content"][0]["text"], "file body");

        let wr = tool_result_data("Write", &p, "ok");
        assert_eq!(wr["file_path"], "a\\b.txt");

        // 空输出不硬塞空壳
        assert_eq!(
            tool_result_data("Bash", &p, "   ").as_object().map(|o| o.len()),
            Some(0)
        );
    }

    /// 真机自检：把本机真实 WorkBuddy 会话过一遍转换，断言每个 plan_item 都带齐
    /// Trae 原生字段（缺字段会让客户端反序列化整条丢弃 → 工具卡片不显示）。
    /// 需要本机有 `~/.workbuddy` 数据，默认跳过，手动跑：
    ///   cargo test -p wb-switch-core -- --ignored real_plan_item_shape --nocapture
    #[test]
    #[ignore = "需要本机 WorkBuddy 数据"]
    fn real_plan_item_shape_matches_native() {
        let sessions = match workbuddy_source::list_sessions() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("本机读不到 WorkBuddy 数据，跳过：{e}");
                return;
            }
        };
        let agent = (
            "solo_work_lite".to_string(),
            "solo_work_lite".to_string(),
            "SOLO MTC".to_string(),
        );
        let (mut turns, mut tools) = (0usize, 0usize);
        for s in sessions.iter().filter(|s| s.has_body).take(5) {
            let Ok(body) = workbuddy_source::load_body(&s.id) else {
                continue;
            };
            for t in &body.turns {
                // 回归护栏（v0.0.13 用户反馈）：解析出的提问里绝不能残留客户端的
                // 内部注入 —— 一旦残留，Trae 里就会多出用户从没见过的「用户消息」
                // （`<conversation_history_summary>` / `Please continue…` /
                //  `Use the TaskOutput tool with task_id=…`），并连带走空白助手气泡。
                let head: String = t.user_text.chars().take(80).collect();
                assert_eq!(
                    workbuddy_source::classify_user_message(&t.user_text),
                    workbuddy_source::UserMsgKind::Real,
                    "源会话 {} 解析出了注入型提问: {head:?}",
                    s.id
                );
                assert!(
                    !t.user_text.trim().is_empty(),
                    "源会话 {} 解析出了空的用户提问",
                    s.id
                );
                let steps = build_steps(t);
                if steps.iter().all(|x| x.tool.is_empty()) {
                    continue;
                }
                turns += 1;
                let raw = build_task_content(
                    &steps,
                    &agent,
                    "run-probe",
                    "task-probe",
                    "答案",
                    t.updated_at,
                );
                let v: Value = serde_json::from_str(&raw).expect("生成物必须是合法 JSON");
                // plan_item 与「非空工具」的 step 一一对应（顺序相同，末尾多一个 finish）
                let expect: Vec<(String, bool)> = steps
                    .iter()
                    .filter(|s| !s.tool.is_empty())
                    .map(|s| {
                        (
                            s.tool.clone(),
                            s.result
                                .as_deref()
                                .map(|r| !r.trim().is_empty())
                                .unwrap_or(false),
                        )
                    })
                    .collect();
                for (i, m) in v["messages"].as_array().expect("messages").iter().enumerate() {
                    let pi = &m["plan_item"];
                    let tci = &pi["tool_call_info"];
                    let name = tci["name"].as_str().unwrap_or("");
                    assert!(tci["params"].is_object(), "{name}: params 必须是对象");
                    // 真机里 Read/Bash 的参数一定是带字段的对象；若只剩一个 raw，
                    // 说明双重编码没拆干净（卡片会显示成一坨转义 JSON）。
                    if matches!(name, "Read" | "Bash" | "Edit" | "Write") {
                        let o = tci["params"].as_object().expect("object");
                        assert!(
                            !(o.len() == 1 && o.contains_key("raw")),
                            "{name}: params 仍是未解析的原始串"
                        );
                    }
                    for k in ["already_emitted_generating_event", "already_emitted_run_event"] {
                        assert!(tci.get(k).is_some(), "{name}: tool_call_info 缺 {k}");
                    }
                    for k in ["error_variant", "is_async"] {
                        assert!(tci["result"].get(k).is_some(), "{name}: result 缺 {k}");
                    }
                    for k in ["confirm_info", "parent_agent_run_ids", "hide"] {
                        assert!(pi.get(k).is_some(), "{name}: plan_item 缺 {k}");
                    }
                    // 状态必须是 Trae 认识的那几种，否则卡片卡在「进行中」不渲染结果
                    let st = tci["result"]["status"].as_str().unwrap_or("");
                    assert!(
                        ["success", "failed", "skipped", "canceled", "running"].contains(&st),
                        "{name}: result.status = {st:?}，Trae 不认识（会当成 running）"
                    );
                    // 有输出的工具必须把内容带进 data，别让卡片只剩空壳
                    if !name.is_empty() && name != "finish" {
                        tools += 1;
                        if let Some((_, true)) = expect.get(i) {
                            assert!(
                                tci["result"]["data"]
                                    .as_object()
                                    .map(|o| !o.is_empty())
                                    .unwrap_or(false),
                                "{name}: 有输出但 result.data 为空，卡片会没有详细过程"
                            );
                        }
                    }
                }
            }
        }
        eprintln!("真机自检：{turns} 个回合 / {tools} 个工具项，字段全部齐备");
    }

    #[test]
    fn default_agent_differs_per_client() {
        assert_eq!(default_agent("trae-cn").0, "solo_agent");
        assert_eq!(default_agent("solo-cn").0, "solo_work_lite");
    }

    #[test]
    fn turn_context_rebuilds_user_text_and_drops_foreign_state() {
        // 模板来自「别的会话」，里面夹带它的提问原文、工作区、token 用量
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE chat_turn(updated_at INTEGER, context TEXT);")
            .unwrap();
        let tpl = json!({
            "references": [{"path": "别人的引用.md"}],
            "render_context": { "current_file": "d:\\other\\a.py", "hash_files": [{"p": 1}],
                                "hash_folder_paths": null },
            "render_variables": {},
            "metadata": null,
            "locale": "zh-cn",
            "rewritten_query": "重写过的别人的提问",
            "persist_user_message_context": {
                "ppe_env_name": "",
                "model_info": { "config_name": "deepseek-v4.1-flash", "prompt_max_tokens": 168000 },
                "parsed_query": ["别人的提问原文"],
                "turn_type": "default",
                "query": [{ "type": "text", "data": { "content": "别人的提问原文" } }],
                "is_goal_loop": false,
                "hide_user_query": false,
                "is_background_wakeup": false
            },
            "trace_id": "别人的trace",
            "token_usage": { "name": "", "prompt_tokens": 127929 },
            "notifications": [{"x": 1}],
            "document_contexts": [{"d": 1}],
            "workspace_folders": ["d:\\htw\\签到"],
            "context_usage": { "contexts": [{ "name": "别人的文件" }] },
            "chat_start_time": 1791043687680_i64,
            "chat_end_time": 1791043907022_i64,
            "version_code": 20260917,
            "future_unknown_field": { "v": "别人的值" }
        })
        .to_string();
        c.execute("INSERT INTO chat_turn VALUES(1, ?1)", params![tpl])
            .unwrap();

        let out = build_turn_context(&c, Some("d:\\htw\\test"), "nihao", 1791114394, 1791114410);

        // 1) 提问文本必须换成本次的
        assert!(out.contains("nihao"));
        assert!(
            !out.contains("别人的提问原文"),
            "绝不能把别的会话的提问原文带进来（客户端会照它渲染用户气泡）"
        );
        // 2) 工作区 / token / 时间 / trace 都必须重建
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.pointer("/workspace_folders/0").and_then(Value::as_str), Some("d:\\htw\\test"));
        assert_eq!(v.pointer("/token_usage/prompt_tokens").and_then(Value::as_i64), Some(0));
        assert_eq!(v.pointer("/chat_start_time").and_then(Value::as_i64), Some(1791114394000));
        assert_eq!(v.pointer("/chat_end_time").and_then(Value::as_i64), Some(1791114410000));
        assert_eq!(v.pointer("/persist_user_message_context/parsed_query/0").and_then(Value::as_str), Some("nihao"));
        assert_ne!(v.get("trace_id").and_then(Value::as_str), Some("别人的trace"));
        assert_eq!(v.pointer("/context_usage/contexts").and_then(Value::as_array).map(Vec::len), Some(0));
        assert_eq!(v.pointer("/references").and_then(Value::as_array).map(Vec::len), Some(0));
        assert_eq!(
            v.pointer("/render_context/current_file"),
            Some(&Value::Null),
            "render_context 的取值要中性化"
        );
        // 3) 版本相关字段保留
        assert_eq!(v.get("version_code").and_then(Value::as_u64), Some(20260917));
        assert_eq!(
            v.pointer("/persist_user_message_context/model_info/config_name")
                .and_then(Value::as_str),
            Some("deepseek-v4.1-flash"),
            "model_info 是唯一必须照用的部分"
        );
        // 4) 未知新字段保留形状但清空取值
        assert_eq!(
            v.pointer("/future_unknown_field/v").and_then(Value::as_str),
            Some("")
        );
        // 5) 不声明不存在的工作区
        let out2 = build_turn_context(&c, None, "x", 1, 2);
        let v2: Value = serde_json::from_str(&out2).unwrap();
        assert_eq!(v2.pointer("/workspace_folders").and_then(Value::as_array).map(Vec::len), Some(0));
    }

    #[test]
    fn session_context_drops_foreign_skill_revisions() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE chat_session(updated_at INTEGER, context TEXT);")
            .unwrap();
        let tpl = json!({
            "activated_feature_flags": ["a"],
            "file_read_state_cache": {"x": 1},
            "cc_file_read_state_cache": {"y": 1},
            "has_remote_counterpart": true,
            "is_worktree": true,
            "vm_mode": "aha_vm",
            "last_real_project_id": "别人的项目",
            "server_history_cache_limit": 1000,
            "skill_list_revisions": {
                "conversation_skill_list_revision_6abf7aed853237fb78521db8_solo_work_lite_x": "hash"
            }
        })
        .to_string();
        c.execute("INSERT INTO chat_session VALUES(1, ?1)", params![tpl])
            .unwrap();

        let out = build_session_context(&c, "6ac0eff953164cb16eb88438");
        assert!(
            !out.contains("6abf7aed853237fb78521db8"),
            "skill_list_revisions 的键名不能夹带别的会话 id"
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("vm_mode").and_then(Value::as_str), Some("aha_vm"));
        assert_eq!(
            v.get("last_real_project_id").and_then(Value::as_str),
            Some("6ac0eff953164cb16eb88438")
        );
        assert_eq!(v.get("has_remote_counterpart").and_then(Value::as_bool), Some(false));
        assert_eq!(v.get("is_worktree").and_then(Value::as_bool), Some(false));
        assert_eq!(v.pointer("/skill_list_revisions").and_then(Value::as_object).map(serde_json::Map::len), Some(0));
        assert_eq!(v.get("file_read_state_cache"), Some(&Value::Null));
    }

    #[test]
    fn history_messages_wraps_role_and_text() {
        let raw = history_messages("assistant", "你好");
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v.pointer("/raw_messages/0/role").and_then(Value::as_str),
            Some("assistant")
        );
        assert_eq!(
            v.pointer("/raw_messages/0/content/0/text").and_then(Value::as_str),
            Some("你好")
        );
    }

    #[test]
    fn time_based_ids_match_trae_convention() {
        // 真实库实测：id[:8] 换算 == created_at（0x6abf7aed == 1790933741）
        let id = hex_id_at(1790933741, 24);
        assert_eq!(id.len(), 24);
        assert_eq!(&id[..8], "6abf7aed");
        assert_eq!(u32::from_str_radix(&id[..8], 16).unwrap(), 1790933741);
    }

    #[test]
    fn biz_project_id_is_decremented_hex() {
        assert_eq!(biz_of("6abf106a6258d706956d3479"), "6abf106a6258d706956d3478");
        assert_eq!(biz_of("6abf7a9a853237fb78521db7"), "6abf7a9a853237fb78521db6");
        // 已到 0 时不能回绕
        assert_eq!(biz_of("000000000000000000000000"), "000000000000000000000000");
    }

    #[test]
    fn abs_path_normalised_to_trae_style() {
        assert_eq!(normalize_abs_path("D:/htw/test"), "d:\\htw\\test");
        assert_eq!(normalize_abs_path("d:\\htw\\签到"), "d:\\htw\\签到");
        assert_eq!(normalize_abs_path("C:\\Users\\x"), "c:\\Users\\x");
    }

    #[test]
    fn session_id_is_deterministic_and_time_prefixed() {
        let wb = "f92b521c-bac2-4f51-9ba6-880e08384eb7";
        let a = det_session_id(wb, 1791114394);
        let b = det_session_id(wb, 1791114394);
        assert_eq!(a, b, "同一源会话必须派生同一 id，重复导入才幂等");
        assert_eq!(a.len(), 24);
        assert_eq!(u32::from_str_radix(&a[..8], 16).unwrap(), 1791114394);
        assert_eq!(&a[8..], "f92b521cbac24f51");
    }

    #[test]
    fn agent_run_id_is_uuid_v5_and_deterministic() {
        let id = uuid5("hello");
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14] as char, '5', "第三段首字符应为 5（v5）");
        assert_eq!(id, uuid5("hello"), "v5 必须确定性");
    }

    #[test]
    fn purge_removes_sessions_with_turns_but_no_task() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chat_session(session_id TEXT, session_title TEXT);
             CREATE TABLE chat_turn(session_id TEXT);
             CREATE TABLE task(session_id TEXT);
             CREATE TABLE chat_message(session_id TEXT, message_id TEXT);
             CREATE TABLE chat_message_general(message_id TEXT);
             CREATE TABLE session_project(session_id TEXT);",
        )
        .unwrap();
        // 健康会话：2 回合 / 2 task
        c.execute("INSERT INTO chat_session VALUES('healthy','ok')", []).unwrap();
        c.execute("INSERT INTO chat_turn VALUES('healthy')", []).unwrap();
        c.execute("INSERT INTO chat_turn VALUES('healthy')", []).unwrap();
        c.execute("INSERT INTO task VALUES('healthy')", []).unwrap();
        c.execute("INSERT INTO task VALUES('healthy')", []).unwrap();
        // 幽灵会话：1 回合 / 0 task，并带正文行
        c.execute("INSERT INTO chat_session VALUES('ghost','bad')", []).unwrap();
        c.execute("INSERT INTO chat_turn VALUES('ghost')", []).unwrap();
        c.execute("INSERT INTO chat_message VALUES('ghost','m1')", []).unwrap();
        c.execute("INSERT INTO chat_message_general VALUES('m1')", []).unwrap();

        let n = purge_broken_sessions(&c, &|_| {}).unwrap();
        assert_eq!(n, 1, "只应识别出幽灵会话");
        assert_eq!(
            c.query_row("SELECT count(*) FROM chat_session", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1,
            "健康会话必须保留"
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM chat_message_general", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0,
            "幽灵会话的正文行也必须一并删除"
        );
    }

    #[test]
    fn purge_removes_orphan_agent_run_left_by_client_side_delete() {
        // 复刻线上事故：Trae（或用户手动）删掉了此前导入的会话，只清了 chat_session，
        // 留下 agent_run 孤儿行。此时 chat_session 查不到，但 agent_run_id 仍是
        // uuid5(sid:start) 那一个 → 重新导入会撞 UNIQUE(agent_run_id) 直接失败。
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chat_session(session_id TEXT UNIQUE, session_title TEXT);\
             CREATE TABLE agent_run(agent_run_id TEXT NOT NULL UNIQUE, session_id TEXT);\
             CREATE TABLE chat_message(session_id TEXT, message_id TEXT UNIQUE);\
             CREATE TABLE chat_turn(session_id TEXT, turn_id TEXT UNIQUE);\
             CREATE TABLE task(session_id TEXT, task_id TEXT UNIQUE);\
             CREATE TABLE history_v2(session_id TEXT, history_v2_id TEXT);",
        )
        .unwrap();

        let sid = det_session_id("a68be9bf-8fdd-44c3-a829-afda1a036897", 1790475240);
        let run_id = uuid5(&format!("{sid}:1790475240"));
        // 会话行已被客户端删除，只剩孤儿 run + 孤儿正文行
        c.execute(
            "INSERT INTO agent_run VALUES(?1, ?2)",
            params![run_id, sid],
        )
        .unwrap();
        c.execute("INSERT INTO chat_message VALUES(?1,'m1')", params![sid])
            .unwrap();

        // 清理前：直接插同一 run_id 必须失败（这就是用户看到的那条报错）
        let before = c.execute(
            "INSERT INTO agent_run VALUES(?1, ?2)",
            params![run_id, sid],
        );
        assert!(
            before.is_err(),
            "前置条件不成立：应当能复现 UNIQUE 冲突"
        );

        let n = purge_session_rows(&c, &sid, &run_id).unwrap();
        assert_eq!(n, 2, "孤儿 run 行 + 孤儿正文行都必须清掉");

        // 清理后：同样的 INSERT 必须成功（导入不再被历史残骸打断）
        c.execute(
            "INSERT INTO agent_run VALUES(?1, ?2)",
            params![run_id, sid],
        )
        .expect("清掉孤儿行后，同 id 的 run 必须能重新写入");
    }

    #[test]
    fn purge_is_noop_when_nothing_to_clean() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chat_session(session_id TEXT UNIQUE);\
             CREATE TABLE chat_message(session_id TEXT, message_id TEXT);\
             CREATE TABLE agent_run(agent_run_id TEXT NOT NULL UNIQUE, session_id TEXT);",
        )
        .unwrap();
        let sid = det_session_id("a68be9bf-8fdd-44c3-a829-afda1a036897", 1790475240);
        let run_id = uuid5(&format!("{sid}:1790475240"));
        assert_eq!(purge_session_rows(&c, &sid, &run_id).unwrap(), 0);
    }
}
