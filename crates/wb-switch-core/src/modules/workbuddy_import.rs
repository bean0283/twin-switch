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
//! ## 第三批硬约束（3.4.9 新版客户端：导入的会话「打不开」）
//!
//! 现象：会话在列表里正常显示，点进去却只有一句
//! 「**由于网络原因，会话加载失败。请刷新页面。**」，刷新无用；
//! 客户端 renderer 日志里是 `chat.getMessages → bizCode=995001 / failed to load messages`。
//!
//! 10. **`chat_turn.context` 里两个字段必须是「JSON 字符串」，不能是数组/对象**：
//!     `persist_user_message_context.query`（值形如 `"[{\"type\":\"text\",\"data\":{\"content\":…}}]"`）
//!     与 `context_usage`（值形如 `"{\"contexts\":[]}"`）。旧实现写成真数组/真对象，
//!     新版客户端按 `String` 反序列化 → 整个 `get_messages` 失败 → 会话打不开。
//!     **这是「库是对的、只有类型错」的故障，从数据上看不出问题**，只有逐字段比类型才查得到。
//! 11. **`plan_item` 别多写当前版本没有的键**：`confirm_info` / `parent_agent_run_ids` /
//!     `result.error_variant` / `result.is_async` 在当前真实行里已不存在（只有旧版才写 null）。
//!     与真实行同形，才不会被严格的反序列化拒掉。
//! 12. **`project` 行必须是 domain v2**：`domain_version = 2` +
//!     `single_root_workspace_folder`/`workspace_folders` 都填好。新版客户端多处按
//!     `domain_version = 2` 过滤项目，停在 v1 的行在它眼里等于「项目不存在」。
//! 13. **不要凭空写 `chat_session.context.vm_mode`**：旧代码在模板缺该键时补一个 `aha_vm`，
//!     等于把本地会话标成 TraeWork 云工作区会话。
//! 14. **`history_v2.messages` 必须带 `mode`，`raw_messages[]` 六个键要齐**（2026-10-28 第三处根因）：
//!     `harness.dll` 里 `HistoryV2Messages` 的字段序列是
//!     `raw_messages · important_user_message_prompt_section · bottom_line_tool_call_summary ·
//!      mode · summary · system_reminder_session_initialized · …`，`mode` 是**非 Option 字段**
//!     （原生行 10/10 写 `"FunctionCall"`）。此前从未写过它，整行解码即失败 ——
//!     症状与第 10 条一模一样（`995001` + 界面「由于网络原因」），极难分辨。
//!     `HistoryRawChatMessage` 的 `content`/`extra_info`/`name`/`role`/`tool_call_id`/`tool_calls`
//!     原生 18/18 全有（可为 null），少键同样失败。
//! 15. **`server_history_info` 一个回合只许有「一份」历史**（2026-10-28 第五处根因）：
//!     镜像行按 `task_id` 被认成回合（原生：每回合恰好 1 个 `task_id`，`session_id` 必是该回合
//!     用户消息 id，`client_history_id` 100% 命中 `history_v2`）。清理旧镜像**必须按
//!     `conversation_id` 整条会话删**：重复导入会重建 `history_v2` 主键，只按
//!     `client_history_id` 删会漏掉上一代，一个回合就有两份历史（行数、时间窗完全相同）。
//!     「缺镜像」和「多一份镜像」是**同一个 995001**，方向相反。
//!
//! 旧数据由 `heal_legacy_rows()` 就地修（随每次导入自动跑一遍，幂等）。
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
use crate::modules::trae_switch::{is_running, launch};
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
        upgrade_project_row(conn, &pid, &path)?;
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
          created_at, updated_at, deleted_at, workspace_status, last_active_at, work_mode, \
          workspace_type, single_root_workspace_folder, workspace_folders, project_kind, domain_version) \
         VALUES (?1, 'native-ide', ?2, ?3, NULL, ?4, ?5, ?6, ?6, NULL, ?7, ?6, NULL, \
                 'single_root', ?4, ?8, 'workspace', 2)",
        params![pid, uid, name, path, biz, ts, workspace_status_of(&path), workspace_folders_of(&path)],
    )
    .map_err(|e| format!("写入 project 失败: {e}"))?;
    Ok(pid)
}

/// 目录存在 → `single_root`；不存在 → `virtual`（真实库两种取值都有，语义是「能不能挂载」）。
fn workspace_status_of(path: &str) -> &'static str {
    if Path::new(path).is_dir() {
        "single_root"
    } else {
        "virtual"
    }
}

/// `workspace_folders` 是**数组序列化后的字符串**（真实行形如 `["d:\\htw\\签到"]`）。
fn workspace_folders_of(path: &str) -> String {
    json!([path]).to_string()
}

/// 把旧版本写下的 project 行升级到当前的「project domain v2」形态。
///
/// 新版客户端的项目查询普遍带 `domain_version = 2` 过滤，`single_root_workspace_folder`
/// 也为空的话这个项目在它眼里就是「不存在」，会话虽然还列得出来，打开却会失败。
/// 只对**单根工作区**做升级（多根项目本来就没有这个字段），已经是 v2 且填好路径的行不动。
fn upgrade_project_row(conn: &Connection, pid: &str, path: &str) -> Result<(), String> {
    conn.execute(
        // ⚠️⚠️ `work_mode` 不许乱填 'work'（第九处根因）。
        // work 模式的项目在库里必须是「Trae 托管目录」形态：`absolute_path` 指向
        // `...\ModularData\ai-agent\work-mode-projects\<id>`，且磁盘上有对应空目录。
        // 把普通用户目录标成 work 模式 → 状态自相矛盾 → 客户端准备不了 workspace，
        // 会话初始化直接失败（界面照旧只显示「由于网络原因，会话加载失败」）。
        // 能正常打开的原生会话旁边的 project 行一律 `work_mode=NULL`。
        // 判据就是路径：只有真·work 模式项目才保留原值。
        "UPDATE project SET domain_version = 2, workspace_type = 'single_root', \
         single_root_workspace_folder = ?2, workspace_folders = ?3, workspace_status = ?4, \
         project_kind = 'workspace', \
         work_mode = CASE WHEN ifnull(absolute_path,'') LIKE '%work-mode-projects%' \
                          THEN work_mode ELSE NULL END \
         WHERE project_id = ?1 \
           AND ifnull(workspace_type, 'single_root') = 'single_root' \
           AND (ifnull(domain_version, 0) < 2 \
                OR single_root_workspace_folder IS NULL \
                OR TRIM(single_root_workspace_folder) = '')",
        params![pid, path, workspace_folders_of(path), workspace_status_of(path)],
    )
    .map_err(|e| format!("升级 project 行失败: {e}"))?;
    Ok(())
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

/// 取用户消息 context 形状的 SQL（全库最新一条真实用户消息）。
const USER_CTX_TPL_SQL: &str = "SELECT user_message_context FROM chat_message \
     WHERE message_role='user' AND ifnull(user_message_context,'')<>'' \
     ORDER BY created_at DESC LIMIT 1";

/// 取 `chat_turn.context` 形状的 SQL（全库最新一条真实回合）。
const TURN_CTX_TPL_SQL: &str = "SELECT context FROM chat_turn \
     WHERE ifnull(context,'')<>'' AND context<>'{}' \
     ORDER BY updated_at DESC LIMIT 1";

/// 会话级模板缓存：两个 context 形状各查一次，供该会话的**全部回合**复用。
///
/// ⚠️ 以前是每回合各查一次这两个 `SELECT … ORDER BY … LIMIT 1`，而循环正在往
/// `chat_message` / `chat_turn` 里插行 —— 表越写越大，后面的回合扫描 + 排序的
/// 数据量也跟着变大，整体是 **O(回合²)**。实测 324 回合 / 17263 工具步骤的会话
/// （正文 243.8 MB）会慢到数十分钟，且越导越慢。模板在整轮导入期间是稳定的
/// （取的是库里**已有**的真实行，不是我们刚插的），缓存一次即可。
struct ShapeTemplates {
    user_message: Value,
    turn: Value,
}

impl ShapeTemplates {
    fn load(conn: &Connection) -> Self {
        Self {
            user_message: shape_template(conn, USER_CTX_TPL_SQL),
            turn: shape_template(conn, TURN_CTX_TPL_SQL),
        }
    }
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
    // ⚠️ 不写 `vm_mode`：它是「TraeWork 云工作区（VM）会话」的标记。本工具的会话是
    // 本地文件会话，旧代码在模板缺该键时**凭空补一个 `aha_vm`**，等于告诉客户端
    // 「这个会话跑在 VM 里」，客户端就会去拉 VM 实例（失败时报「正在启动 TraeWork 工作区」）。
    // 新版真实行的 `chat_session.context` 里根本没有这个键。
    out.remove("vm_mode");
    Value::Object(out).to_string()
}

/// 用户提问在 Trae 里的**双重编码**形态：`[{"type":"text","data":{"content":…}}]`
/// 再整体序列化成一个字符串。
///
/// `chat_message.user_message_context.query` 与
/// `chat_turn.context.persist_user_message_context.query` **都必须是这种字符串**。
/// 写成真正的数组（本模块 0.4.0～0.4.4 的写法）会让新版客户端按 `String` 反序列化失败，
/// 整条会话打不开，界面只显示「由于网络原因，会话加载失败。请刷新页面。」——
/// 而库里数据是好的，只有这一处类型不对，所以从表面完全看不出问题。
fn query_blob(user_text: &str) -> String {
    json!([{ "type": "text", "data": { "content": user_text } }]).to_string()
}

/// 真实行 `history_v2.accumulated_summary` 的默认取值。
///
/// ⚠️ 这一列（连同 `summary` / `compress_token_usage` / `micro_compacted` /
/// `truncated_above` / `micro_compact_strategy`）在真实行里**永远不是 NULL**。
/// 旧实现没写这几列（即 NULL），新版客户端按非 Option 解码 → 整行读取失败 →
/// `get_messages` 报 `failed to load messages`，会话打不开。默认值逐字抄自真实行。
const EMPTY_ACCUMULATED_SUMMARY: &str = r#"{"summary":"","accumulated_summary_used":false}"#;

/// 空上下文用量。真实行同样是**字符串**：`"{\"contexts\":[]}"`，不是对象。
const EMPTY_CONTEXT_USAGE: &str = r#"{"contexts":[]}"#;

/// 取目标库中最近一条用户消息的 `user_message_context` 作为模板，替换提问内容。
fn template_user_context(tpl: &Value, user_text: &str) -> String {
    let mut ctx = if tpl.is_object() && !tpl.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        tpl.clone()
    } else {
        json!({})
    };
    let query_json = query_blob(user_text);
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
    tpl: &Value,
    workspace: Option<&str>,
    user_text: &str,
    t_start: i64,
    t_end: i64,
) -> String {
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

    // ⚠️⚠️ `turn_record` 必须**整键不写**（第六处根因，2026-10-28 夜）。
    //
    // 它是客户端「回合持久化元数据」的权威来源。全库 421 个回合的实测分布：
    //   - 419 个：**完全没有这个键**（老会话）→ 走兼容路径，能打开；
    //   - 1 个：原生新会话，7 个字段全填满（`task_id`=`task_…`、`agent_run_id`、
    //     `trace_id`(32 hex)、`turn_type`="User"、`root_turn_id`、`message_index`=1、
    //     `chat_start_time`=毫秒）→ 能打开；
    //   - 1 个：**我们这个导入器造的，存在但全空**（`task_id`/`agent_run_id`/`trace_id`/
    //     `turn_type`/`root_turn_id` 都是 `""`，`message_index`=0，`chat_start_time`=0）。
    //
    // 「键在、值空」是库里**只有导入器能造出来**的状态。上面那段「保留形状、清空取值」
    // 会把模板里的 `turn_record` 一并拷进来再清空，正好落进这个坑：
    // restorer 判定 `restored body has incomplete durable Turn metadata` /
    // `invalid turn metadata for history entry` → 整条会话 995001
    // （界面：「由于网络原因，会话加载失败」）。
    //
    // 「缺键」是安全的（419/421 原生就是缺键）；「写成空壳」是致命的。
    // 宁可缺，不可空。
    out.remove("turn_record");

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
            // ⚠️ 必须是**字符串**（内容本身是一段 JSON，属于「双重编码」）：
            // 真实行取值形如 `"[{\"type\":\"text\",\"data\":{\"content\":\"…\"}}]"`。
            // 写成一真数组会让新版客户端按 String 反序列化失败 → 整条会话打不开
            // （界面只显示「由于网络原因，会话加载失败」）。
            "query": query_blob(user_text),
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
    // 同理是**字符串**（真实行 `"{\"contexts\":[]}"`），不是对象。
    out.insert("context_usage".into(), json!(EMPTY_CONTEXT_USAGE));
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
        //     （原生取值 false / true）；缺失时工具卡片不渲染。
        //   · `params` 必须是对象（`parse_params`），写成 JSON 字符串会让卡片取不到参数。
        //   · `confirm_info` / `parent_agent_run_ids` / `result.error_variant` / `result.is_async`
        //     在**当前版本**的真实行里根本不存在（只有旧版才写 null），这里也不再写：
        //     与真实行同形，才不会被严格的反序列化拒掉。
        let tool_call_info = json!({
            "id": hex_id_at(item_ts, 24),
            "name": s.tool,
            "params": params_val,
            // ⚠️ 字段集合要和**当前版本**真实行逐一对齐：新版真实行的 `result` 里
            // 只有 status / error_message / data / render / is_truncated / interrupt / images，
            // 没有 `error_variant` / `is_async`（旧版有）。多写只会增加反序列化风险。
            "result": {
                "status": status,
                "error_message": "",
                "data": data_val,
                "render": Value::Null,
                "is_truncated": Value::Null,
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
                    // ⚠️⚠️ `products` 必须是**对象**，`artifact_paths` 必须是**数组**。
                    // 旧写法 `"products": null` 是第七处根因（2026-10-28 深夜三次）：
                    // 客户端按 `Map<String, Value>` 读它，`null` 直接解码失败 →
                    // `get_messages` 整条会话报 995001「由于网络原因，会话加载失败」。
                    // 真实库：finish 项的 data 恒为
                    // `{"products": {…}, "artifact_paths": [...]}`（`products` 取空对象
                    // `{}` 的真实样本也存在，所以空对象是安全值）；非 finish 的工具项则
                    // **完全没有** `products` 键 —— 空与缺不是一回事，别互相顶替。
                    "data": { "products": {}, "artifact_paths": [] },
                    "render": Value::Null,
                    "is_truncated": Value::Null,
                    "interrupt": Value::Null,
                    "images": Value::Null
                },
                "meta": Value::Null,
                "already_emitted_generating_event": false,
                "already_emitted_run_event": true
            },
            "hide": Value::Null
        }
    }));

    json!({ "task_id": task_id, "messages": items }).to_string()
}

/// `history_v2.messages.mode` 的固定值。
///
/// ⚠️⚠️ **这是 3.4.9 之后导入会话「打不开」的第三处根因（2026-10-28 查实）**：
/// `harness.dll` 里 `HistoryV2Messages` 的字段序列是
/// `raw_messages · important_user_message_prompt_section · bottom_line_tool_call_summary ·
///  mode · summary · system_reminder_session_initialized · summarized_token_usage · …`，
/// `mode` 是**非 Option 字段**；原生行 10/10 都写 `"mode":"FunctionCall"`，
/// 我们此前从未写过它 ⇒ 整行反序列化失败 ⇒ `lite.get_messages` 回 `995001`
/// ⇒ 前端把它渲染成「由于网络原因，会话加载失败」。
const HISTORY_MODE: &str = "FunctionCall";

/// 构造 `history_v2.messages` 的 raw_messages 包装。
///
/// ⚠️ 两条硬约束（都由原生行反推，18/18 个 raw message 全部具备）：
/// 1. 顶层必须有 `mode`（见 [`HISTORY_MODE`]）；
/// 2. `raw_messages[]` 的六个键 `content` / `extra_info` / `name` / `role` /
///    `tool_call_id` / `tool_calls` 必须齐，缺的写 `null` —— 原生行就是这么写的，
///    少键会让 `HistoryRawChatMessage` 解码失败。
fn history_messages(role: &str, text: &str) -> String {
    let mut v = json!({
        "mode": HISTORY_MODE,
        "raw_messages": [{
            "role": role,
            // ⚠️ content 元素必须是五个键（type/text/image_url/video_url/cache_control）。
            // 原生行 100% 都带后三个（值为 null）；少键会让 `LiteMessagePart` 解码失败，
            // 整条会话就渲染成「由于网络原因，会话加载失败」。
            "content": [{
                "type": "text",
                "text": text,
                "image_url": Value::Null,
                "video_url": Value::Null,
                "cache_control": Value::Null
            }],
            "name": Value::Null,
            "tool_call_id": Value::Null,
            "tool_calls": Value::Null,
            "extra_info": Value::Null
        }]
    });
    // ⚠️ 用户行还必须带 `important_user_message_prompt_section`。
    //
    // 这是「导入的会话点开就报『会话加载失败』」的**最后一处根因**（2026-10-29 查实）：
    // `harness.dll` 里 `HistoryV2Messages` 的字段序列是
    //   raw_messages · important_user_message_prompt_section · bottom_line_tool_call_summary ·
    //   mode · …
    // 它与 `mode` 紧邻、同样**不是 Option**。实测本机 136 个会话：
    //   - 该段「0 条命中」的会话：3 个 —— 正是打不开的那 3 个（零误报、零漏报）
    //   - 只要有一条 user_input 行带它，会话就能打开（133 个里有 119 个是部分覆盖）
    // 原生行的该段形状固定为 `{original_user_input, must_keep_rules, response_rules,
    // parent_agent_instruction}`；抽样 1635 行，`original_user_input` 基本都是空串。
    if role == "user" {
        v["important_user_message_prompt_section"] = prompt_section();
    }
    v.to_string()
}

/// `important_user_message_prompt_section` 的固定形状（从原生行反推）。
fn prompt_section() -> Value {
    json!({
        "original_user_input": "",
        "must_keep_rules": Value::Null,
        "response_rules": Value::Null,
        "parent_agent_instruction": Value::Null
    })
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
// 旧数据修复（新版客户端把 context 里的两个字段按字符串解析）
// ---------------------------------------------------------------------------

/// 修复旧版本写下的、**类型不对**的 context 行。
///
/// 0.4.0～0.4.4 写进 `chat_turn.context` 的是「一真数组 / 一真对象」：
///
/// ```text
/// persist_user_message_context.query  = [{"type":"text","data":{"content":…}}]   ← 该是字符串
/// context_usage                       = {"contexts":[]}                          ← 该是字符串
/// ```
///
/// 新版客户端（3.4.9 起）按 `String` 反序列化这两个字段，类型不符时整条会话加载失败，
/// 界面只显示「由于网络原因，会话加载失败。请刷新页面。」——**库是对的，只有类型错**，
/// 所以既不会丢数据也看不出原因。这里就地改成字符串形态（内容不变）。
///
/// 同时补上 `history_v2` 里旧实现留成 NULL 的 6 个非空列（真实行永远不是 NULL，
/// 新版客户端按非 Option 解码，NULL 会让整行读不出来）。
///
/// 幂等：已经是字符串 / 已有值的行不动。
/// `turn_record` 是不是「空壳」。
///
/// 原生只有两种形态：**没有这个键**（实测 419/421）或 **7 个字段全填满**（1/421）。
/// 只要不是全填满，就说明它是由导入器「保留形状、清空取值」清出来的壳子。
/// 壳子留着 = restorer 判定 `restored body has incomplete durable Turn metadata`
/// → 整条会话 995001。所以判据取「非全填满」，宁可删。
fn turn_record_incomplete(v: &Value) -> bool {
    let Some(o) = v.as_object() else {
        return true;
    };
    let filled = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    };
    let positive = |k: &str| o.get(k).and_then(Value::as_i64).map(|n| n > 0).unwrap_or(false);
    !(filled("task_id")
        && filled("agent_run_id")
        && filled("trace_id")
        && filled("turn_type")
        && filled("root_turn_id")
        && positive("message_index")
        && positive("chat_start_time"))
}

/// 列出表的列名（表不存在时返回空）。
fn cols_of(conn: &Connection, table: &str) -> Vec<String> {
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info(\"{table}\")")) else {
        return Vec::new();
    };
    let Ok(it) = st.query_map([], |r| r.get::<_, String>(1)) else {
        return Vec::new();
    };
    it.filter_map(Result::ok).collect()
}

pub fn heal_legacy_rows(conn: &Connection, on_log: &dyn Fn(&str)) -> Result<Value, String> {
    let mut turns_fixed = 0usize;
    let mut projects_fixed = 0usize;

    // 1) chat_turn.context
    let turns: Vec<(i64, String)> = {
        let mut st = conn
            .prepare("SELECT id, context FROM chat_turn WHERE ifnull(context,'') NOT IN ('', '{}')")
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    };
    for (id, raw) in turns {
        let Ok(mut v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if !v.is_object() {
            continue;
        }
        let mut changed = false;
        if let Some(pu) = v.get_mut("persist_user_message_context").and_then(Value::as_object_mut) {
            if let Some(q) = pu.get("query") {
                if !q.is_string() && !q.is_null() {
                    pu.insert("query".into(), json!(q.to_string()));
                    changed = true;
                }
            }
        }
        if let Some(u) = v.get("context_usage") {
            if !u.is_string() && !u.is_null() {
                v["context_usage"] = json!(u.to_string());
                changed = true;
            }
        }
        // 空壳 `turn_record`：只有老版本的导入器写得出来（原生要么没有这个键，
        // 要么 7 个字段全填满）。留着它 = 「回合元数据不完整」→ 995001，
        // 所以这里**整键删掉**，退回原生老会话那条能打开的路径。
        if v.get("turn_record").map(turn_record_incomplete).unwrap_or(false) {
            v.as_object_mut().map(|m| m.remove("turn_record"));
            changed = true;
        }
        if changed {
            conn.execute(
                "UPDATE chat_turn SET context = ?2 WHERE id = ?1",
                params![id, v.to_string()],
            )
            .map_err(|e| format!("修复 chat_turn.context 失败: {e}"))?;
            turns_fixed += 1;
        }
    }

    // 2) chat_session.context 里旧代码凭空补的 `vm_mode` 不在这里删：
    //    它对加载无影响（客户端从会话列表接口读 vm_mode，不读这份 context），
    //    而真·VM 会话可能确实带这个键，批量删会误伤。新导入不再写它即可。

    // 2b) history_v2 里旧实现留成 NULL 的列。真实行这几列**永远不是 NULL**，
    //     新版客户端按非 Option 解码，一 NULL 整行读不出来 → 会话打不开。
    let history_fixed = conn
        .execute(
            "UPDATE history_v2 SET \
                summary = ifnull(summary, ''), \
                accumulated_summary = ifnull(accumulated_summary, ?1), \
                compress_token_usage = ifnull(compress_token_usage, 0), \
                micro_compacted = ifnull(micro_compacted, 0), \
                truncated_above = ifnull(truncated_above, 0), \
                micro_compact_strategy = ifnull(micro_compact_strategy, '') \
             WHERE summary IS NULL OR accumulated_summary IS NULL \
                OR compress_token_usage IS NULL OR micro_compacted IS NULL \
                OR truncated_above IS NULL OR micro_compact_strategy IS NULL",
            params![EMPTY_ACCUMULATED_SUMMARY],
        )
        .map_err(|e| format!("修复 history_v2 的 NULL 列失败: {e}"))?;

    // 2c) history_v2.messages 的 JSON 形态 —— 「打不开」的第三处根因。
    //     3.4.9 起 `HistoryV2Messages` 的 `mode` 是非 Option 字段，
    //     `HistoryRawChatMessage` 的 content/extra_info/name/role/tool_call_id/tool_calls
    //     六个键也必须齐（缺的写 null）。旧实现两者都没写 ⇒ 整行解码失败。
    //     先做廉价的字符串预筛，避免把几十万行大 JSON 全解析一遍。
    //
    //     ⚠️ 老库/测试库的 history_v2 可能没有 messages 列，缺列时整段跳过（不能当成失败）。
    let mut history_json_fixed = 0usize;
    let has_messages = conn
        .prepare("PRAGMA table_info(history_v2)")
        .and_then(|mut st| {
            let it = st.query_map([], |r| r.get::<_, String>(1))?;
            Ok(it.filter_map(Result::ok).any(|c| c == "messages"))
        })
        .unwrap_or(false);
    let rows: Vec<(i64, String)> = if has_messages {
        // ⚠️ 必须有 `messages NOT LIKE '%"mode"%'`：全库 history 上万行，
        // 幂等重跑时让 SQLite 直接把这些行筛掉，比拿回 Rust 再判快一个数量级。
        let mut st = conn
            .prepare(
                "SELECT id, messages FROM history_v2 \
                 WHERE ifnull(messages,'') LIKE '%raw_messages%' AND messages NOT LIKE '%\"mode\"%'",
            )
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    } else {
        Vec::new()
    };
    // 上万行逐条 UPDATE 各自成事务会很慢；SAVEPOINT 可嵌套，外层已有事务也安全。
    let _ = conn.execute_batch("SAVEPOINT heal_history_messages");
    for (id, raw) in rows {
        let Ok(mut v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(obj) = v.as_object_mut() else {
            continue;
        };
        let mut changed = false;
        if !obj.contains_key("mode") {
            obj.insert("mode".into(), json!(HISTORY_MODE));
            changed = true;
        }
        if let Some(msgs) = obj.get_mut("raw_messages").and_then(Value::as_array_mut) {
            for m in msgs.iter_mut() {
                let Some(mo) = m.as_object_mut() else {
                    continue;
                };
                for k in ["name", "tool_call_id", "tool_calls", "extra_info"] {
                    if !mo.contains_key(k) {
                        mo.insert(k.into(), Value::Null);
                        changed = true;
                    }
                }
                // `content[].*` 与 `tool_calls[].*` 的**可选键**同样要补齐：
                // 实测全库「有 mode 的行」100% 都带 image_url/video_url/cache_control
                // 与 index/type —— 旧版导入写出来的行全都缺，光补 mode 不够。
                if let Some(parts) = mo.get_mut("content").and_then(Value::as_array_mut) {
                    for c in parts.iter_mut() {
                        let Some(co) = c.as_object_mut() else { continue };
                        for k in ["image_url", "video_url", "cache_control"] {
                            if !co.contains_key(k) {
                                co.insert(k.into(), Value::Null);
                                changed = true;
                            }
                        }
                    }
                }
                if let Some(calls) = mo.get_mut("tool_calls").and_then(Value::as_array_mut) {
                    for (i, c) in calls.iter_mut().enumerate() {
                        let Some(co) = c.as_object_mut() else { continue };
                        if !co.contains_key("index") {
                            co.insert("index".into(), json!(i as i64));
                            changed = true;
                        }
                        if !co.contains_key("type") {
                            co.insert("type".into(), json!("function"));
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            conn.execute(
                "UPDATE history_v2 SET messages = ?2 WHERE id = ?1",
                params![id, v.to_string()],
            )
            .map_err(|e| format!("修复 history_v2.messages 失败: {e}"))?;
            history_json_fixed += 1;
        }
    }
    let _ = conn.execute_batch("RELEASE heal_history_messages");

    // 3) project 行升级到 domain v2
    let projects: Vec<(String, String)> = {
        let mut st = conn
            .prepare(
                "SELECT project_id, absolute_path FROM project \
                 WHERE ifnull(deleted_at,0)=0 AND ifnull(workspace_type,'single_root')='single_root' \
                   AND (ifnull(domain_version,0) < 2 \
                        OR single_root_workspace_folder IS NULL \
                        OR TRIM(single_root_workspace_folder) = '')",
            )
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    };
    for (pid, path) in &projects {
        if path.trim().is_empty() {
            continue;
        }
        upgrade_project_row(conn, pid, &normalize_abs_path(path))?;
        projects_fixed += 1;
    }

    if turns_fixed + history_fixed + history_json_fixed + projects_fixed > 0 {
        on_log(&format!(
            "   已修复旧数据：{turns_fixed} 个回合上下文改成字符串形态 / {history_fixed} 行历史补上非空列 / \
             {history_json_fixed} 行历史补上 mode 与 raw_messages 缺键 / {projects_fixed} 个项目行升级到 domain v2"
        ));
    }

    // 4) 补 / 重建 `server_history_info` 消息镜像。命中任一判据就整条会话重建：
    //    ① 整条会话一行镜像都没有（第四处根因，旧版导入）；
    //    ② 会话里有**悬空镜像行**（`client_history_id` 已不在 `history_v2`）——第五处根因：
    //       重复导入会把 history_v2 主键全部换新，上一代镜像就再也删不掉，一个回合于是有了
    //       两份历史（实测 17907 行，行数与时间窗和新版逐回合完全一致，只有 task_id 不同）。
    //    两条都不会误伤原生会话：原生镜像的 `client_history_id` 全部命中 `history_v2`（18/18）。
    let has_mirror_table = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='server_history_info'")
        .and_then(|mut st| st.exists([]))
        .unwrap_or(false);
    let naked: Vec<String> = if has_messages && has_mirror_table {
        let mut st = conn
            .prepare(
                "SELECT DISTINCT sid FROM ( \
                     SELECT h.session_id AS sid FROM history_v2 h \
                      WHERE ifnull(h.deleted_at,0)=0 \
                        AND NOT EXISTS (SELECT 1 FROM server_history_info s \
                                        WHERE s.client_history_id = h.history_v2_id) \
                     UNION \
                     SELECT s.conversation_id AS sid FROM server_history_info s \
                      WHERE s.client_history_id NOT IN (SELECT history_v2_id FROM history_v2) \
                 ) \
                 WHERE sid IN (SELECT session_id FROM history_v2 WHERE ifnull(deleted_at,0)=0) \
                 ORDER BY sid",
            )
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    } else {
        Vec::new()
    };
    let mut mirrored_rows = 0usize;
    let mut mirrored_sessions = 0usize;
    for sid in &naked {
        match mirror_server_history(conn, sid) {
            Ok(n) if n > 0 => {
                mirrored_rows += n;
                mirrored_sessions += 1;
            }
            Ok(_) => {}
            Err(e) => on_log(&format!("   补消息镜像失败（{sid}）：{e}")),
        }
    }
    if mirrored_sessions > 0 {
        on_log(&format!(
            "   已补/重建消息镜像：{mirrored_sessions} 条会话 / {mirrored_rows} 行（缺它或多一份都打不开）"
        ));
    }

    // 5) 镜像里的 `task_id` 归一成 UUID。
    //    老版本写的是 `task:<turn_id>`，那是自造命名空间；真实值一律是 UUID
    //    （全库 779 组全是 UUID）。按「旧值 → 新值」整组替换，保持「一回合一个 id」。
    let mut task_ids_fixed = 0usize;
    if has_messages && has_mirror_table {
        let olds: Vec<String> = {
            let mut st = conn
                .prepare("SELECT DISTINCT task_id FROM server_history_info WHERE task_id LIKE 'task:%'")
                .map_err(|e| e.to_string())?;
            let it = st
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            it.filter_map(Result::ok).collect()
        };
        for old in olds {
            conn.execute(
                "UPDATE server_history_info SET task_id=?2 WHERE task_id=?1",
                params![old, uuid::Uuid::new_v4().to_string()],
            )
            .map_err(|e| format!("修复镜像 task_id 失败: {e}"))?;
            task_ids_fixed += 1;
        }
        if task_ids_fixed > 0 {
            on_log(&format!(
                "   已归一镜像 task_id：{task_ids_fixed} 个回合（`task:` 前缀不是客户端的命名空间）"
            ));
        }
    }

    // 6) `chat_message_task.content` 里 finish 项的 `result.data` 形状。
    //    旧写法 `"products": null`，而客户端按对象读（真实库 finish 恒为对象，
    //    空对象 `{}` 也有真实样本）；`artifact_paths` 则整键缺失（真实库恒为数组）。
    //    这一个字段就让**每条助手消息**解码失败 ⇒ 整条会话 995001。
    let mut task_data_fixed = 0usize;
    let has_task_table = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='chat_message_task'")
        .and_then(|mut st| st.exists([]))
        .unwrap_or(false);
    let rows: Vec<(i64, String)> = if has_task_table {
        let mut st = conn
            .prepare("SELECT id, content FROM chat_message_task WHERE ifnull(content,'') <> ''")
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    } else {
        Vec::new()
    };
    for (id, raw) in rows {
        let Ok(mut v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(items) = v.get_mut("messages").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut touched = false;
        for it in items.iter_mut() {
            let Some(tc) = it
                .get_mut("plan_item")
                .and_then(|p| p.get_mut("tool_call_info"))
            else {
                continue;
            };
            let is_finish = tc.get("name").and_then(Value::as_str) == Some("finish");
            let Some(res) = tc.get_mut("result") else {
                continue;
            };
            let Some(data) = res.get_mut("data") else {
                continue;
            };
            let Some(obj) = data.as_object_mut() else {
                continue;
            };
            let prod_bad = !matches!(obj.get("products"), Some(Value::Object(_)));
            if is_finish {
                if prod_bad {
                    obj.insert("products".into(), json!({}));
                    touched = true;
                }
                let ap_bad = !matches!(obj.get("artifact_paths"), Some(Value::Array(_)));
                if ap_bad {
                    obj.insert("artifact_paths".into(), json!([]));
                    touched = true;
                }
            } else if prod_bad && obj.contains_key("products") {
                // 非 finish 项在真实库里**没有** `products` 键；`null` 撑不起对象语义，
                // 直接去掉而不是替换成 `{}`（空与缺不是一回事）。
                obj.remove("products");
                touched = true;
            }
        }
        if touched {
            conn.execute(
                "UPDATE chat_message_task SET content=?2 WHERE id=?1",
                params![id, v.to_string()],
            )
            .map_err(|e| format!("修复 chat_message_task.content 失败: {e}"))?;
            task_data_fixed += 1;
        }
    }
    if task_data_fixed > 0 {
        on_log(&format!(
            "   已修复步骤数据形状：{task_data_fixed} 条助手消息（`products` 必须是对象、`artifact_paths` 必须是数组）"
        ));
    }

    // 7) 会话级的 `work_mode` / `session_icon` 归一。
    //    旧版写死 `work_mode='work'`（会把请求切到 `trae-work` 频道）和
    //    `session_icon='personal'`（原生没有这个值）。两者一起改，判据用
    //    「personal + work」这个组合——原生会话不可能同时命中。
    let mut sessions_fixed = 0usize;
    let sess_cols: Vec<String> = {
        let mut st = conn
            .prepare("PRAGMA table_info(chat_session)")
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    };
    if sess_cols.iter().any(|c| c == "work_mode") && sess_cols.iter().any(|c| c == "session_icon") {
        sessions_fixed += conn
            .execute(
                "UPDATE chat_session SET work_mode=NULL, session_icon='chat' \
                 WHERE session_icon='personal' AND work_mode='work'",
                [],
            )
            .map_err(|e| format!("修复会话 work_mode/icon 失败: {e}"))?;
    }
    if sessions_fixed > 0 {
        on_log(&format!(
            "   已归一 {sessions_fixed} 条会话的 work_mode/icon（`work` 会把请求切到 trae-work 频道）"
        ));
    }

    // 8) 项目级 `work_mode` **不在这里批量清**：
    //    实测原生库里有 22 个 `work_mode='work'` + 普通目录的项目，说明这个组合是合法的
    //    （work 模式 ≠ 必须托管目录）。批量清会误伤用户正在用的 work 模式项目，
    //    所以只在新导入/升级时不再凭空写 'work'（见 `create_project` / `upgrade_project_row`）。

    Ok(json!({
        "turns": turns_fixed,
        "history": history_fixed,
        "history_json": history_json_fixed,
        "projects": projects_fixed,
        "mirror_sessions": mirrored_sessions,
        "mirror_rows": mirrored_rows,
        "mirror_task_ids": task_ids_fixed,
        "task_data": task_data_fixed,
        "sessions": sessions_fixed,
    }))
}

// ---------------------------------------------------------------------------
// 单个会话写入
// ---------------------------------------------------------------------------

/// 把 `history_v2` 镜像进 `server_history_info`。
///
/// ⚠️⚠️ 2026-10-28 定位到的**第四处**不兼容：3.4.9 的 `lite.get_messages` 读的是
/// `server_history_info`（消息持久化镜像），不是 `history_v2`。
///
/// 实测判据（同一个库里对照，容不得巧合）：
/// - 所有**能打开**的会话，每条 `history_v2` 都有 1~2 行镜像
///   （`server_history_info.client_history_id = history_v2.history_v2_id`），覆盖率 100%；
/// - 所有旧版导入的会话**一行镜像都没有**（17907 / 748 / 1121 行全是 0%）。
///
/// 缺镜像就走到 harness 的 `message persistence unavailable`，前端统一渲染成
/// 「由于网络原因，会话加载失败」（995001）。
///
/// ⚠️⚠️ 2026-10-28 定位到的**第五处**不兼容：镜像**多一份同样打不开**。
/// 重复导入会重建 `history_v2`（主键换新），上一代镜像行不再命中 DELETE，于是同一个
/// 回合在库里有两份历史（行数、时间窗逐回合完全一致，只有 `task_id` 不同）。
/// harness 的 `history-archive` restorer 按 `task_id` 认出两个「被接受的历史」，
/// 身份对不上 ⇒ 仍是同一个 995001。判据：`client_history_id` 必须 100% 命中
/// `history_v2`（原生样本 18/18；出问题的库里是 18231/36138 = 50.5%）。
///
/// 拆分规则照抄原生样本：
/// - `history_v2.messages.raw_messages` 的**每条消息拆成一行镜像**；
/// - 首条 `source` = `user_input`（用户提问）或 `llm_default`（模型输出），
///   后续条 `source` = `history_v2.content_source`（工具名）；
/// - 首条的 `history_id` = `history_v2_id`，后续条另生成 24 位 hex；
/// - ⚠️ `session_id` 列存的是**该回合用户消息的 id**，`conversation_id` 才是会话 id
///   （按 `session_id` 查会一行都查不到，这是最容易踩的坑）。
///
/// 幂等：先清掉本会话已有的镜像行再重建，重复调用结果一致。
fn mirror_server_history(conn: &Connection, session_id: &str) -> Result<usize, String> {
    use std::collections::HashMap;

    // ⚠️⚠️ 必须按 `conversation_id` 整条会话删，不能只删「client_history_id 命中当前
    // history_v2」的行 —— 重复导入会重建 history_v2（`history_v2_id` 全换新），
    // 上一代的镜像行就再也命中不了 DELETE，留在库里变成**同一回合的第二份历史**
    // （实测 17907 行：与新版逐回合行数、时间窗完全相同，只是 task_id 不同）。
    // harness 的 restorer 按 `task_id` 认回合，一个回合出现两份 ⇒ 995001。
    conn.execute(
        "DELETE FROM server_history_info WHERE conversation_id=?1 \
           OR client_history_id IN (SELECT history_v2_id FROM history_v2 WHERE session_id=?1)",
        params![session_id],
    )
    .map_err(|e| format!("清理旧的消息镜像失败: {e}"))?;

    // 回合索引：message_id -> (该回合的用户消息 id, 服务器任务 id)
    let mut owner: HashMap<String, (String, String)> = HashMap::new();
    {
        let mut st = conn
            .prepare(
                "SELECT turn_id, reply_to_message_id, response_message_id \
                 FROM chat_turn WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map(params![session_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for (_turn_id, user_msg, resp_msg) in it.flatten() {
            // 一个回合一个「服务器任务 id」，同回合的多条历史共用（原生即如此）。
            // ⚠️ 真实取值是 **UUID**（`8d0196d8-796f-4200-96e0-01a7095ac2c9` 这种，
            // 全库 779 组全是 UUID），不是 `task:<turn_id>`。后者是我们自造的第 4 套 ID
            // 命名空间，在任务表/回合表里都对不上任何一行。
            let task_id = uuid::Uuid::new_v4().to_string();
            owner.insert(user_msg.clone(), (user_msg.clone(), task_id.clone()));
            owner.insert(resp_msg, (user_msg, task_id));
        }
    }
    // 兜底：没有 turn 行（例如只有消息没有回合）时，至少别把行写成空 session_id。
    let fallback_user = conn
        .query_row(
            "SELECT message_id FROM chat_message WHERE session_id=?1 AND message_role='user' \
             ORDER BY message_index LIMIT 1",
            params![session_id],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default();

    let (ws_path, ws_id) = conn
        .query_row(
            "SELECT ifnull(p.single_root_workspace_folder,''), ifnull(s.project_id,'') \
             FROM chat_session s LEFT JOIN project p ON p.project_id = s.project_id \
             WHERE s.session_id=?1",
            params![session_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .unwrap_or((String::new(), String::new()));
    let extra_info = json!({
        "workspace_folder": ws_path,
        "workspace_id": ws_id,
        "workspace_path": ws_path,
    })
    .to_string();

    let rows: Vec<(String, String, String, i64, i64, String, String, String)> = {
        let mut st = conn
            .prepare(
                "SELECT history_v2_id, message_id, ifnull(messages,''), ifnull(token_usage,0), \
                        created_at, ifnull(content_source,''), ifnull(agent_type,''), \
                        ifnull(agent_run_id,'') \
                 FROM history_v2 WHERE session_id=?1 ORDER BY created_at, id",
            )
            .map_err(|e| e.to_string())?;
        let it = st
            .query_map(params![session_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        it.flatten().collect()
    };

    let mut written = 0usize;
    for (hv_id, msg_id, raw, token_usage, created_at, source, agent_type, run_id) in rows {
        let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let mode = parsed
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or(HISTORY_MODE)
            .to_string();
        let Some(list) = parsed.get("raw_messages").and_then(|v| v.as_array()) else {
            continue;
        };
        let (user_msg, task_ref) = owner
            .get(&msg_id)
            .cloned()
            .unwrap_or_else(|| (fallback_user.clone(), format!("task:{session_id}")));
        let n = list.len().max(1);
        let per_item = token_usage / n as i64;

        for (i, m) in list.iter().enumerate() {
            let one = mirror_raw_message(m.clone());
            let src = if i == 0 {
                if source == "user_input" {
                    "user_input"
                } else {
                    "llm_default"
                }
            } else {
                source.as_str()
            };
            // 同一行拆出来的多条要保证顺序稳定：`created_at` 递增一秒，
            // 避免同秒时客户端按 history_id 排序把我们拆散。
            let ts = created_at + i as i64;
            let history_id = if i == 0 {
                hv_id.clone()
            } else {
                hex_id_at(ts, 24)
            };
            let payload = json!({ "mode": mode, "raw_messages": [one] }).to_string();

            conn.execute(
                "INSERT INTO server_history_info \
                 (history_id, client_history_id, conversation_id, session_id, task_id, agent_run_id, \
                  is_deleted, messages, token_usage, summary, created_at, updated_at, source, user_id, \
                  agent_type, item_summary, item_token_usage, expired_at, history_source, \
                  summarized_above, micro_compacted, extra_info, micro_compact_strategy) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, 0, '', ?8, ?8, ?9, NULL, \
                         ?10, '', ?11, NULL, 'server', 0, 0, ?12, '')",
                params![
                    history_id,
                    hv_id,
                    session_id,
                    user_msg,
                    task_ref,
                    run_id,
                    payload,
                    ts,
                    src,
                    agent_type,
                    per_item,
                    extra_info
                ],
            )
            .map_err(|e| format!("写入消息镜像失败: {e}"))?;
            written += 1;
        }
    }
    Ok(written)
}

/// 镜像侧的 raw message 比 `history_v2` 多几个键（`content.tool_name`、
/// `function_call.partial_arguments` / `namespace`）。缺了它们，非 Option 解码同样会失败。
fn mirror_raw_message(mut m: Value) -> Value {
    if let Some(blocks) = m.get_mut("content").and_then(|c| c.as_array_mut()) {
        for b in blocks.iter_mut() {
            if let Some(o) = b.as_object_mut() {
                o.entry("image_url").or_insert(Value::Null);
                o.entry("video_url").or_insert(Value::Null);
                o.entry("cache_control").or_insert(Value::Null);
                o.entry("tool_name").or_insert(Value::Null);
            }
        }
    }
    if let Some(calls) = m.get_mut("tool_calls").and_then(|c| c.as_array_mut()) {
        for c in calls.iter_mut() {
            if let Some(f) = c.get_mut("function_call").and_then(|f| f.as_object_mut()) {
                f.entry("partial_arguments").or_insert(Value::Null);
                f.entry("namespace").or_insert(Value::Null);
            }
        }
    }
    if let Some(o) = m.as_object_mut() {
        o.entry("name").or_insert(Value::Null);
        o.entry("tool_call_id").or_insert(Value::Null);
        o.entry("tool_calls").or_insert(Value::Null);
        o.entry("extra_info").or_insert(Value::Null);
    }
    m
}

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
    // 模板在**清完旧行之后**取一次：此时库里剩的都是真实行，不会把上一轮导入
    // 自己写的 context 当模板，也不会每回合重扫一遍越来越大的表。
    let tpl = ShapeTemplates::load(conn);

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
    // ⚠️⚠️ `work_mode` 必须是 **NULL**、`session_icon` 用 `'chat'`（第八处根因）。
    // 旧写法硬编码 `work_mode='work'`、`session_icon='personal'`：
    //   - `work_mode` 不是装饰字段。客户端的 `_getContributionChannel` 见到 work_mode
    //     就会把请求切到 **`trae-work` 频道**（`icube-ai` → `trae-work`），而那条频道
    //     面向「Work 模式项目」，导入的会话在那里没有对应状态；
    //   - 实测能正常打开的原生会话 `work_mode` 一律是 NULL（`chat_session` 全表如此），
    //     只有我们导入的是 `'work'`；
    //   - `session_icon` 原生取值是 `'chat'` / `'data'`，`'personal'` 是我们编的。
    conn.execute(
        "INSERT INTO chat_session \
         (session_id, project_id, created_at, updated_at, deleted_at, session_type, \
          agent_process_support, session_title, work_mode, context, session_icon, \
          is_pinned, pinned_at, extra, is_unread, last_unread_turn_id) \
         VALUES (?1, ?2, ?3, ?4, 0, 'side_chat', 'v3', ?5, NULL, ?6, 'chat', \
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
        let u_ctx = template_user_context(&tpl.user_message, &turn.user_text);
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
              agent_run_id, accumulated_summary, compress_token_usage, micro_compacted, \
              truncated_above, micro_compact_strategy) \
             VALUES (?1, ?2, ?3, ?4, 0, '', 0, ?5, ?5, 0, ?6, 'user_input', ?7, ?8, 0, 0, 0, '')",
            params![
                hex_id_at(t_start, 24),
                sid,
                u_msg,
                history_messages("user", &turn.user_text),
                t_start,
                agent.0,
                run_id,
                EMPTY_ACCUMULATED_SUMMARY
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
                "mode": HISTORY_MODE,
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
                    }],
                    "extra_info": Value::Null
                }]
            })
            .to_string();
            conn.execute(
                "INSERT INTO history_v2 \
                 (history_v2_id, session_id, message_id, messages, token_usage, summary, \
                  summarized_above, created_at, updated_at, deleted_at, agent_type, content_source, \
                  agent_run_id, accumulated_summary, compress_token_usage, micro_compacted, \
                  truncated_above, micro_compact_strategy) \
                 VALUES (?1, ?2, ?3, ?4, 0, '', 0, ?5, ?5, 0, ?6, ?7, ?8, ?9, 0, 0, 0, '')",
                params![
                    hex_id_at(t_end, 24),
                    sid,
                    a_msg,
                    payload,
                    t_end,
                    agent.0,
                    s.tool,
                    run_id,
                    EMPTY_ACCUMULATED_SUMMARY
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
                  agent_run_id, accumulated_summary, compress_token_usage, micro_compacted, \
                  truncated_above, micro_compact_strategy) \
                 VALUES (?1, ?2, ?3, ?4, 0, '', 0, ?5, ?5, 0, ?6, 'llm_default', ?7, ?8, 0, 0, 0, '')",
                params![
                    hex_id_at(t_end, 24),
                    sid,
                    a_msg,
                    history_messages("assistant", &final_text),
                    t_end,
                    agent.0,
                    run_id,
                    EMPTY_ACCUMULATED_SUMMARY
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
                build_turn_context(&tpl.turn, workspace.as_deref(), &turn.user_text, t_start, t_end),
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

    // 7) 消息持久化镜像（`lite.get_messages` 真正读的就是它，缺了整条会话打不开）
    let mirrored = mirror_server_history(conn, &sid)?;

    on_log(&format!(
        "  会话「{}」→ {} 个回合 / {} 个工具步骤 / {} 条历史行 / {} 条消息镜像",
        title, stats.turns, stats.steps, stats.history_rows, mirrored
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

/// 按**用户设置里的保留策略**清理 WorkBuddy 导入备份，回收磁盘
/// （每份 ≈ 目标库大小，279 MB 量级）。
///
/// 实际清理逻辑走共享的 [`crate::modules::trae_export::prune_with_policy`]（按客户端分组、
/// 时间 / 份数 / 容量三重触发，每个客户端最新一份永不清），这里只负责补日志。
fn prune_backups(client_key: &str, on_log: &dyn Fn(&str)) -> usize {
    let root = crate::modules::trae_export::wb_import_backup_dir();
    let (removed, freed) = crate::modules::trae_export::prune_with_policy(&root);
    if removed > 0 {
        on_log(&format!(
            "已清理 {removed} 份 {client_key} 旧备份，回收 {:.1} MB（按设置里的整库备份保留策略）",
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

/// 导入前结束目标客户端，**失败时把真实原因带出来**。
///
/// ⚠️ 2026-10-28 的教训：这里原来只有 `if !wait_until_stopped(..10s) { return Err("未能退出") }`，
/// 把 [`KillOutcome`] 里的残留 PID 与 `taskkill` 的原文全丢了 —— 界面上只剩一句
/// 「目标客户端未能退出」，既分不清「杀不掉」还是「被杀后又自己起来了」，也没法判断
/// 是不是提权问题。失败原因不许塌缩成 `bool`（同 MEMORY 里的硬规则）。
///
/// 现在的口径：
/// 1. 结束**前**把现场进程表写进 `switch.log`，事后仅凭日志就能复盘；
/// 2. 每轮 15 秒、最多 3 轮 —— 被更新器/崩溃恢复拉起来时重新等一遍，而不是直接失败；
/// 3. 最终失败返回 `hint()`（残留 PID + `taskkill` 原文），并单独认出「拒绝访问」。
fn stop_for_import(client: &crate::modules::trae_discover::TraeClient, log: &dyn Fn(&str)) -> Result<(), String> {
    use crate::modules::trae_switch;

    let before = trae_switch::list_processes(client);
    log(&format!(
        "{} 正在运行（{} 个进程），先将其退出…",
        client.label,
        before.len()
    ));
    // 现场留档：进程名 + PID。事后能判断出「只匹配到一个」还是「有进程没被匹配到」。
    trae_switch::switch_log(&format!(
        "[import] 退出目标客户端 {}：{}",
        client.label,
        before
            .iter()
            .map(|(n, p)| format!("{n}#{p}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));

    let mut tries = 0u32;
    loop {
        tries += 1;
        let out = trae_switch::quit_client(client, 15_000);
        if out.ok {
            log(&format!("目标客户端已退出（第 {tries} 轮确认，进程表已清空）"));
            return Ok(());
        }
        for e in &out.errors {
            trae_switch::switch_log(&format!("[import] 结束进程失败：{e}"));
        }
        // 残留下来的 PID 与首轮完全不重叠 ⇒ 它是被重新拉起来的（自动更新 / 崩溃恢复），
        // 隔一会儿再等一轮，而不是把一次「正在更新」判成「杀不掉」。
        if out.restarted && tries < 3 {
            log(&format!(
                "{} 被重新拉起来了（进程号全换了），稍后再试一轮…",
                client.label
            ));
            std::thread::sleep(std::time::Duration::from_millis(1500));
            continue;
        }
        let mut msg = out.hint(client.label);
        let denied = out.errors.iter().any(|e| {
            e.contains("拒绝访问") || e.to_lowercase().contains("access is denied")
        });
        if denied {
            msg.push_str("；结束进程被系统拒绝，请以管理员身份运行 TwinSwitch 后重试");
        }
        trae_switch::switch_log(&format!("[import] 放弃退出目标客户端：{msg}"));
        return Err(format!("{msg}。因此无法写入它本地的会话库"));
    }
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
        stop_for_import(dst_client, &log)?;
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
    // 4c) 再把旧版本写下的「类型不对」的 context 修好（新版客户端按字符串解析这两处）。
    //     这样即便这次不重导同一条会话，之前导入过的会话也能重新打开。
    heal_legacy_rows(&conn, &log)?;

    // 4d) 整批写入放进**一个事务**。
    //
    //     SQLite 默认「每条语句 = 一个事务」，每次提交都要一次 fsync。324 回合 /
    //     17263 个工具步骤会产生近 2 万条 INSERT，等于近 2 万次刷盘 —— 这是「导入慢」
    //     的另一半原因（另一半是每回合重扫模板表，已由 `ShapeTemplates` 修掉）。
    //     一个事务只需一次提交。
    //
    //     这里的库是**临时工作副本**：失败就整体丢弃、绝不替换线上库，所以顺带关掉
    //     `synchronous` 也没有任何风险；进程真在这里崩掉，丢的只是这一轮导入。
    conn.execute_batch("PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY;")
        .map_err(|e| format!("设置写入期 PRAGMA 失败: {e}"))?;
    // 64 MB 页缓存：这台机器上目标库 639 MB / 156007 页，默认 2 MB 缓存会让索引进出
    // 反复读同一批页。
    let _ = conn.execute_batch("PRAGMA cache_size=-65536");
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| format!("开启写入事务失败: {e}"))?;

    let mut total_turns = 0usize;
    let mut total_steps = 0usize;
    let mut written_sessions = 0usize;
    let mut write_err: Option<String> = None;
    for (meta, body) in &sources {
        log(&format!("转换会话「{}」…", meta.title));
        // 整批写在**明文副本**上，任一条失败就整体放弃（副本会被丢掉），不会留下半截数据；
        // 报错带上会话名，便于一眼看出是哪条出的问题。
        match write_session(&conn, dst_client_key, dst_uid, meta, body, &log) {
            Ok(st) => {
                total_turns += st.turns;
                total_steps += st.steps;
                written_sessions += 1;
            }
            Err(e) => {
                write_err = Some(format!("会话「{}」写入失败：{e}", meta.title));
                break;
            }
        }
    }
    if let Some(e) = write_err {
        // 事务没提交，副本本来就会被丢弃；显式回滚是为了让 `close()` 干净收尾。
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e);
    }
    conn.execute_batch("COMMIT")
        .map_err(|e| format!("提交写入事务失败: {e}"))?;
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
    // 6) 备份原库（统一出口，可在设置里关掉）→ 原子替换
    //
    // ⚠️ 这里的目标库是 **Trae 的库**（609 MB 量级），所以也走整库备份开关：
    //    关了就不留历史版本，「不丢库」由 `swap_in_place` 的原子替换保证。
    let bdir = crate::modules::trae_export::wb_import_backup_dir();
    let backup_dir = bdir.join(crate::modules::trae_backup::entry_name(
        dst_client_key,
        "wbimport",
        &crate::modules::trae_backup::new_stamp(),
    ));
    let backup_mode = {
        let out = match crate::modules::trae_backup::snapshot_before_write(
            &dst_db,
            &new_db,
            &backup_dir,
            dst_client_key,
            "wbimport",
            "导入 WorkBuddy 会话前的整库备份",
        ) {
            Ok(out) => out,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                return Err(format!("备份原库失败，已中止导入：{e}"));
            }
        };
        log(&format!("   {}", crate::modules::trae_backup::describe(&out)));
        out.get("mode").and_then(Value::as_str).unwrap_or("?").to_string()
    };
    // ⚠️ `-wal` / `-shm` 由 `swap_in_place` 连同旧库一起移走清掉，这里不手动删。
    if let Err(e) = crate::modules::trae_backup::swap_in_place(&new_db, &dst_db) {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("替换失败（原库未被改动）：{e}"));
    }

    // 7) 把工作明文提升为新快照：它就是新库的**完整明文**（增量回写保证逐页一致），
    //    提升后下一次读取 / 导入无需再解密。
    log("更新解密快照…");
    if let Err(e) = crate::modules::trae_export::promote_snapshot(dst_client_key, &plain) {
        log(&format!("快照更新失败（不影响导入结果，下次读取会重新解密）：{e}"));
        crate::modules::trae_export::drop_snapshot_meta(dst_client_key);
    }
    let _ = std::fs::remove_dir_all(&work);

    // 7b) 按**用户设置里的保留策略**清理旧备份：每份 ≈ 目标库大小（279 MB 量级），
    //     不清理会越攒越多。策略自带「每个客户端最新一份永不清」的安全网。
    let pruned = prune_backups(dst_client_key, &log);

    // 8) 重启客户端，让导入的会话立即可用
    let relaunched = match launch(dst_client_key) {
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
        "backup_dir": if backup_mode == "skipped" { Value::Null } else { json!(backup_dir.to_string_lossy()) },
        "backup_mode": backup_mode,
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
        // 新版真实行的 result 里没有 error_variant / is_async，plan_item 里没有
        // confirm_info / parent_agent_run_ids —— 多写反而可能被严格反序列化拒掉。
        for k in ["error_variant", "is_async"] {
            assert!(
                tci.pointer(&format!("/result/{k}")).is_none(),
                "result 不该再写 {k}（当前版本真实行没有这个键）"
            );
        }
        for k in ["confirm_info", "parent_agent_run_ids"] {
            assert!(
                pi.get(k).is_none(),
                "plan_item 不该再写 {k}（当前版本真实行没有这个键；需要确认的卡片才带 confirm_info）"
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
        // ⚠️⚠️ finish 项的 `result.data` 形状（第七处根因）：
        // `products` 必须是**对象**（`null` 会让客户端按 Map 解码失败 → 995001），
        // `artifact_paths` 必须是**数组**（真实库恒存在）。
        let data = msgs[0]
            .pointer("/plan_item/tool_call_info/result/data")
            .expect("finish 必须有 result.data");
        assert!(
            data.get("products").map(Value::is_object).unwrap_or(false),
            "products 必须是对象，实际 {}",
            data.get("products").map(|x| x.to_string()).unwrap_or_default()
        );
        assert!(
            data.get("artifact_paths").map(Value::is_array).unwrap_or(false),
            "artifact_paths 必须是数组，实际 {}",
            data.get("artifact_paths").map(|x| x.to_string()).unwrap_or_default()
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
                        assert!(
                            tci["result"].get(k).is_none(),
                            "{name}: result 不该再写 {k}（当前版本真实行没有）"
                        );
                    }
                    for k in ["confirm_info", "parent_agent_run_ids"] {
                        assert!(
                            pi.get(k).is_none(),
                            "{name}: plan_item 不该再写 {k}（当前版本真实行没有）"
                        );
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
            // 原生新会话的形态：turn_record 七项全填满。导入器**不许**把它抄进来
            // （抄进来会被 neutralize 清空，变成「存在但全空」→ 995001）。
            "turn_record": {
                "task_id": "task_6ae1eddeab76cf15092c5a28",
                "agent_run_id": "8e314e86-763f-5127-87c3-aa169fb0acb0",
                "message_index": 1,
                "chat_start_time": 1793191390346_i64,
                "trace_id": "37cc5f6d21284b82ae43dbcd9e3f658a",
                "turn_type": "User",
                "root_turn_id": "6ae1eddeab76cf15092c5a27",
                "user_message_context": { "query": "别人的" }
            },
            "future_unknown_field": { "v": "别人的值" }
        })
        .to_string();
        c.execute("INSERT INTO chat_turn VALUES(1, ?1)", params![tpl])
            .unwrap();

        let tpl = ShapeTemplates::load(&c);
        let out = build_turn_context(&tpl.turn, Some("d:\\htw\\test"), "nihao", 1791114394, 1791114410);

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
        // 2b) 这两个字段必须是**字符串**（内容本身是 JSON）。写成数组/对象会让新版客户端
        //     按 String 反序列化失败，整条会话报「由于网络原因，会话加载失败」。
        let q = v
            .pointer("/persist_user_message_context/query")
            .expect("query 必须存在");
        assert!(q.is_string(), "query 必须是字符串（双重编码），实际是 {q}");
        assert!(q.as_str().unwrap().contains("nihao"), "字符串里要带本次提问");
        let usage = v.get("context_usage").expect("context_usage 必须存在");
        assert!(
            usage.is_string(),
            "context_usage 必须是字符串（真实行是 \"{{\\\"contexts\\\":[]}}\"），实际是 {usage}"
        );
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
        let out2 = build_turn_context(&tpl.turn, None, "x", 1, 2);
        let v2: Value = serde_json::from_str(&out2).unwrap();
        assert_eq!(v2.pointer("/workspace_folders").and_then(Value::as_array).map(Vec::len), Some(0));
        // 6) ⚠️⚠️ `turn_record` 必须**整键消失**。
        //    它是客户端「回合持久化元数据」的权威来源：键在、值空 ⇒ restorer 判定
        //    `restored body has incomplete durable Turn metadata`，整条会话 995001 打不开
        //    （界面只显示「由于网络原因，会话加载失败」）。
        //    实测全库 421 个回合：419 个没有这个键、1 个原生新会话七项全填满、
        //    1 个就是我们导入的空壳 —— 空壳是库里只有导入器造得出来的畸形态。
        assert!(
            v.get("turn_record").is_none(),
            "turn_record 必须不写：抄模板再清空会得到「键在、值空」的致命形态，实际 {}",
            v.get("turn_record").map(|x| x.to_string()).unwrap_or_default()
        );
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
        assert!(
            v.get("vm_mode").is_none(),
            "不许凭空写 vm_mode：它会让客户端把本地会话当成 TraeWork VM 会话去拉实例"
        );
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
    fn heal_rewrites_legacy_context_types_and_project_domain() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chat_turn(id INTEGER PRIMARY KEY, context TEXT);\
             CREATE TABLE chat_session(session_id TEXT PRIMARY KEY, context TEXT);\
             CREATE TABLE history_v2(id INTEGER PRIMARY KEY, summary TEXT, accumulated_summary TEXT,\
                compress_token_usage INTEGER, micro_compacted INTEGER, truncated_above INTEGER,\
                micro_compact_strategy TEXT);\
             CREATE TABLE project(project_id TEXT PRIMARY KEY, absolute_path TEXT, deleted_at INTEGER,\
                workspace_type TEXT, workspace_status TEXT, single_root_workspace_folder TEXT,\
                workspace_folders TEXT, project_kind TEXT, work_mode TEXT, domain_version INTEGER);",
        )
        .unwrap();
        // 旧版本写下的形态：query 是数组、context_usage 是对象、会话带 vm_mode、项目停在 v1
        let legacy_turn = json!({
            "persist_user_message_context": {
                "query": [{ "type": "text", "data": { "content": "旧提问" } }],
                "parsed_query": ["旧提问"]
            },
            "context_usage": { "contexts": [] }
        })
        .to_string();
        c.execute("INSERT INTO chat_turn VALUES(1, ?1)", params![legacy_turn])
            .unwrap();
        c.execute(
            "INSERT INTO chat_session VALUES('s1', ?1)",
            params![json!({ "vm_mode": "aha_vm", "x": 1 }).to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO project VALUES('p1','d:\\htw\\签到',NULL,'single_root','virtual',NULL,'[]','workspace','work',1)",
            [],
        )
        .unwrap();
        // 旧实现把 history_v2 的 6 个非空列留成 NULL
        c.execute("INSERT INTO history_v2(id) VALUES(1)", [])
            .unwrap();

        let n = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert_eq!(n["turns"], 1);
        assert_eq!(n["history"], 1);
        assert_eq!(n["projects"], 1);

        let ctx: String = c
            .query_row("SELECT context FROM chat_turn WHERE id=1", [], |r| r.get(0))
            .unwrap();
        let v: Value = serde_json::from_str(&ctx).unwrap();
        assert!(
            v.pointer("/persist_user_message_context/query")
                .map(Value::is_string)
                .unwrap_or(false),
            "query 必须被改成字符串，否则新版客户端整条会话打不开"
        );
        assert!(v.get("context_usage").map(Value::is_string).unwrap_or(false));
        assert_eq!(
            v.pointer("/persist_user_message_context/parsed_query/0")
                .and_then(Value::as_str),
            Some("旧提问"),
            "修复只改类型，不许改内容"
        );

        let sctx: String = c
            .query_row("SELECT context FROM chat_session WHERE session_id='s1'", [], |r| r.get(0))
            .unwrap();
        assert!(
            sctx.contains("vm_mode"),
            "会话 context 不在这里改：真·VM 会话可能确实带这个键，批量删会误伤"
        );

        let (dv, folder, folders): (i64, String, String) = c
            .query_row(
                "SELECT domain_version, single_root_workspace_folder, workspace_folders \
                 FROM project WHERE project_id='p1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(dv, 2, "项目行必须升级到 domain v2");
        assert_eq!(folder, "d:\\htw\\签到");
        assert_eq!(folders, json!(["d:\\htw\\签到"]).to_string());

        // history_v2 的 NULL 列必须补成真实行的默认值（NULL 会让整行解码失败）
        let (sum, acc, ctu, mc, ta, mcs): (String, String, i64, i64, i64, String) = c
            .query_row(
                "SELECT summary, accumulated_summary, compress_token_usage, micro_compacted, \
                 truncated_above, micro_compact_strategy FROM history_v2 WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(sum, "");
        assert_eq!(acc, EMPTY_ACCUMULATED_SUMMARY);
        assert_eq!((ctu, mc, ta), (0, 0, 0));
        assert_eq!(mcs, "");

        // 幂等：再跑一次什么都不该改
        let again = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert_eq!(again["turns"], 0);
        assert_eq!(again["history"], 0);
        assert_eq!(again["projects"], 0);
    }

    #[test]
    fn heal_rebuilds_stale_mirror_generation() {
        // 第五处根因：重复导入重建 history_v2 后，上一代 server_history_info 删不掉，
        // 同一个回合在库里有两份历史（行数、时间窗一致，只有 task_id 不同）⇒ 995001。
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chat_session(session_id TEXT PRIMARY KEY, project_id TEXT, context TEXT);\
             CREATE TABLE chat_turn(id INTEGER PRIMARY KEY, session_id TEXT, turn_id TEXT,\
                reply_to_message_id TEXT, response_message_id TEXT, context TEXT);\
             CREATE TABLE chat_message(id INTEGER PRIMARY KEY, session_id TEXT, message_id TEXT,\
                message_role TEXT, message_index INTEGER);\
             CREATE TABLE history_v2(id INTEGER PRIMARY KEY, history_v2_id TEXT, session_id TEXT,\
                message_id TEXT, messages TEXT, token_usage INTEGER, summary TEXT,\
                accumulated_summary TEXT, compress_token_usage INTEGER, micro_compacted INTEGER,\
                truncated_above INTEGER, micro_compact_strategy TEXT, created_at INTEGER,\
                content_source TEXT, agent_type TEXT, agent_run_id TEXT, deleted_at INTEGER);\
             CREATE TABLE server_history_info(history_id TEXT, client_history_id TEXT,\
                conversation_id TEXT, session_id TEXT, task_id TEXT, agent_run_id TEXT,\
                is_deleted INTEGER, messages TEXT, token_usage INTEGER, summary TEXT,\
                created_at INTEGER, updated_at INTEGER, source TEXT, user_id TEXT,\
                agent_type TEXT, item_summary TEXT, item_token_usage INTEGER, expired_at INTEGER,\
                history_source TEXT, summarized_above INTEGER, micro_compacted INTEGER,\
                extra_info TEXT, micro_compact_strategy TEXT);\
             CREATE TABLE project(project_id TEXT PRIMARY KEY, absolute_path TEXT, deleted_at INTEGER,\
                workspace_type TEXT, workspace_status TEXT, single_root_workspace_folder TEXT,\
                workspace_folders TEXT, project_kind TEXT, work_mode TEXT, domain_version INTEGER);",
        )
        .unwrap();

        let raw_msg = json!({
            "mode": HISTORY_MODE,
            "raw_messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "答复" }] }]
        })
        .to_string();
        c.execute("INSERT INTO project VALUES('p1','d:\\htw\\签到',NULL,'single_root','virtual',\
                   'd:\\htw\\签到','[]','workspace','work',2)", [])
            .unwrap();
        c.execute(
            "INSERT INTO chat_session VALUES('s1','p1','{}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chat_turn VALUES(1,'s1','T1','A1','B1','{}')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO chat_message VALUES(1,'s1','A1','user',1),(2,'s1','B1','task',2)", [])
            .unwrap();
        c.execute(
            "INSERT INTO history_v2 VALUES(1,'hv1','s1','B1',?1,10,'','{}',0,0,0,'',100,'llm_default',\
             'solo_agent','run-1',0)",
            params![raw_msg],
        )
        .unwrap();
        // 上一代残留镜像：client_history_id 已不在 history_v2，task_id 也不是任何 turn
        c.execute(
            "INSERT INTO server_history_info VALUES('old1','gone','s1','A_old','task:OLD','run-0',\
             NULL,?1,0,'',100,100,'llm_default',NULL,'solo_agent','',0,NULL,'server',0,0,'','')",
            params![raw_msg],
        )
        .unwrap();

        // 前置：残留行让「每回合一个 task_id」不成立
        let stale: i64 = c
            .query_row(
                "SELECT count(*) FROM server_history_info WHERE client_history_id NOT IN \
                 (SELECT history_v2_id FROM history_v2)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 1);

        let n = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert_eq!(n["mirror_sessions"], 1, "有悬空镜像的会话必须被重建");
        assert_eq!(n["mirror_rows"], 1);

        let left: i64 = c
            .query_row("SELECT count(*) FROM server_history_info WHERE conversation_id='s1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(left, 1, "整条会话只许留当前这一代镜像");
        let (tid, sid): (String, String) = c
            .query_row("SELECT task_id, session_id FROM server_history_info WHERE conversation_id='s1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(
            tid.len() == 36 && tid.matches('-').count() == 4,
            "镜像 task_id 必须是 UUID（原生全是 UUID），实际 {tid}"
        );
        assert_eq!(sid, "A1", "session_id 必须是该回合的用户消息 id");

        // 幂等
        let again = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert_eq!(again["mirror_sessions"], 0);
    }

    #[test]
    fn heal_drops_empty_turn_record_and_normalizes_mirror_task_id() {
        // 第六处根因：`chat_turn.context.turn_record` 被写成「存在但全空」，
        // restorer 判定 `restored body has incomplete durable Turn metadata` ⇒ 995001。
        // 存量会话的镜像本身是好的（client_history_id 命中 history_v2），
        // 所以第 4 步不会重建它 —— 必须靠这两步单独修。
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE chat_session(session_id TEXT PRIMARY KEY, project_id TEXT, context TEXT);\
             CREATE TABLE chat_turn(id INTEGER PRIMARY KEY, session_id TEXT, turn_id TEXT,\
                reply_to_message_id TEXT, response_message_id TEXT, context TEXT);\
             CREATE TABLE chat_message(id INTEGER PRIMARY KEY, session_id TEXT, message_id TEXT,\
                message_role TEXT, message_index INTEGER);\
             CREATE TABLE history_v2(id INTEGER PRIMARY KEY, history_v2_id TEXT, session_id TEXT,\
                message_id TEXT, messages TEXT, token_usage INTEGER, summary TEXT,\
                accumulated_summary TEXT, compress_token_usage INTEGER, micro_compacted INTEGER,\
                truncated_above INTEGER, micro_compact_strategy TEXT, created_at INTEGER,\
                content_source TEXT, agent_type TEXT, agent_run_id TEXT, deleted_at INTEGER);\
             CREATE TABLE server_history_info(history_id TEXT, client_history_id TEXT,\
                conversation_id TEXT, session_id TEXT, task_id TEXT, agent_run_id TEXT,\
                is_deleted INTEGER, messages TEXT, token_usage INTEGER, summary TEXT,\
                created_at INTEGER, updated_at INTEGER, source TEXT, user_id TEXT,\
                agent_type TEXT, item_summary TEXT, item_token_usage INTEGER, expired_at INTEGER,\
                history_source TEXT, summarized_above INTEGER, micro_compacted INTEGER,\
                extra_info TEXT, micro_compact_strategy TEXT);\
             CREATE TABLE project(project_id TEXT PRIMARY KEY, absolute_path TEXT, deleted_at INTEGER,\
                workspace_type TEXT, workspace_status TEXT, single_root_workspace_folder TEXT,\
                workspace_folders TEXT, project_kind TEXT, work_mode TEXT, domain_version INTEGER);\
             CREATE TABLE chat_message_task(id INTEGER PRIMARY KEY, message_id TEXT, task_id TEXT,\
                content TEXT, summary TEXT);",
        )
        .unwrap();

        let raw_msg = json!({
            "mode": HISTORY_MODE,
            "raw_messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "答复" }] }]
        })
        .to_string();
        // 旧版写的 finish 项：`products` 是 null、`artifact_paths` 整键缺失
        let bad_task = json!({
            "task_id": "t1",
            "messages": [{
                "id": "pi1",
                "type": "plan_item",
                "plan_item": {
                    "id": "pi1",
                    "tool_call_info": {
                        "name": "finish",
                        "params": { "summary": "答复" },
                        "result": { "status": "success", "error_message": "",
                                    "data": { "summary": "", "products": Value::Null } }
                    }
                }
            }]
        })
        .to_string();
        // 空壳 turn_record：五个字符串全 ""、两个数字为 0（导入器的产物）
        let turn_ctx = json!({
            "chat_start_time": 1791114394000_i64,
            "chat_end_time": 1791114410000_i64,
            "turn_record": {
                "task_id": "",
                "agent_run_id": "",
                "message_index": 0,
                "chat_start_time": 0,
                "trace_id": "",
                "turn_type": "",
                "root_turn_id": "",
                "user_message_context": {}
            }
        })
        .to_string();

        c.execute("INSERT INTO project VALUES('p1','d:\\htw\\签到',NULL,'single_root','virtual',\
                   'd:\\htw\\签到','[]','workspace','work',2)", [])
            .unwrap();
        c.execute("INSERT INTO chat_session VALUES('s1','p1','{}')", [])
            .unwrap();
        c.execute("INSERT INTO chat_turn VALUES(1,'s1','T1','A1','B1',?1)", params![turn_ctx])
            .unwrap();
        c.execute("INSERT INTO chat_message VALUES(1,'s1','A1','user',1),(2,'s1','B1','task',2)", [])
            .unwrap();
        c.execute(
            "INSERT INTO history_v2 VALUES(1,'hv1','s1','B1',?1,10,'','{}',0,0,0,'',100,'llm_default',\
             'solo_agent','run-1',0)",
            params![raw_msg],
        )
        .unwrap();
        // 镜像完好（命中 history_v2），只有 task_id 是自造的 `task:` 前缀
        c.execute(
            "INSERT INTO server_history_info VALUES('hv1','hv1','s1','A1','task:T1','run-1',\
             NULL,?1,0,'',100,100,'llm_default',NULL,'solo_agent','',0,NULL,'server',0,0,'','')",
            params![raw_msg],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chat_message_task VALUES(1,'B1','t1',?1,'')",
            params![bad_task],
        )
        .unwrap();

        let n = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert_eq!(n["turns"], 1, "空壳 turn_record 必须被清掉");
        assert_eq!(n["mirror_sessions"], 0, "镜像本身是好的，不该整条重建");
        assert_eq!(n["mirror_task_ids"], 1, "`task:` 前缀要归一成 UUID");
        assert_eq!(n["task_data"], 1, "finish 项的 data 形状必须被修");

        // finish 项的 products 必须是对象、artifact_paths 必须是数组
        let fixed: String = c
            .query_row("SELECT content FROM chat_message_task WHERE id=1", [], |r| r.get(0))
            .unwrap();
        let fv: Value = serde_json::from_str(&fixed).unwrap();
        let data = fv
            .pointer("/messages/0/plan_item/tool_call_info/result/data")
            .expect("data");
        assert!(data.get("products").map(Value::is_object).unwrap_or(false), "实际 {fixed}");
        assert!(data.get("artifact_paths").map(Value::is_array).unwrap_or(false), "实际 {fixed}");

        let ctx: String = c
            .query_row("SELECT context FROM chat_turn WHERE id=1", [], |r| r.get(0))
            .unwrap();
        let v: Value = serde_json::from_str(&ctx).unwrap();
        assert!(
            v.get("turn_record").is_none(),
            "空壳 turn_record 必须删掉（缺键安全，空壳致命），实际 {ctx}"
        );

        let tid: String = c
            .query_row("SELECT task_id FROM server_history_info WHERE conversation_id='s1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            tid.len() == 36 && tid.matches('-').count() == 4,
            "task_id 必须是 UUID，实际 {tid}"
        );

        // 幂等
        let again = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert_eq!(again["turns"], 0);
        assert_eq!(again["mirror_task_ids"], 0);
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
    /// 复现用户实测故障：导入会话点开报
    /// 「由于网络原因，会话加载失败」/ `chat.getMessages bizCode=995001`。
    ///
    /// 真实数据里故障会话的 history_v2 行长这样（30/30 行）：
    /// `{"raw_messages":[{"content":[{"text":"…","type":"text"}],"role":"user"}]}`
    /// —— 缺 `mode`、缺 `content[].image_url/video_url/cache_control`、
    /// 缺 `tool_calls[].index/type`。而全库「能打开的」行 100% 都带这些键。
    #[test]
    fn heal_fills_mode_and_optional_keys_on_old_import_rows() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"CREATE TABLE history_v2 ( id integer NOT NULL PRIMARY KEY AUTOINCREMENT, history_v2_id text NOT NULL,
                 session_id text NOT NULL, message_id text NOT NULL, messages text NOT NULL,
                 token_usage integer NOT NULL, summary text, summarized_above boolean NOT NULL DEFAULT FALSE,
                 created_at bigint NOT NULL DEFAULT 0, updated_at bigint NOT NULL DEFAULT 0,
                 deleted_at bigint NOT NULL DEFAULT 0, agent_type text NOT NULL DEFAULT '',
                 content_source text NOT NULL DEFAULT '', accumulated_summary text,
                 agent_run_id text NOT NULL DEFAULT '', compress_token_usage integer,
                 micro_compacted boolean NULL, micro_compact_strategy text NULL, truncated_above boolean NULL);
               CREATE TABLE server_history_info ( history_id text NOT NULL PRIMARY KEY, client_history_id text NOT NULL,
                 conversation_id text NULL, session_id text NULL, task_id text NOT NULL, agent_run_id text NULL,
                 is_deleted boolean NULL DEFAULT FALSE, messages text NOT NULL, token_usage bigint NULL DEFAULT 0,
                 summary text NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, source text NOT NULL,
                 user_id text NULL, agent_type text NOT NULL, item_summary text NULL, item_token_usage bigint NULL,
                 expired_at bigint NULL, history_source text NOT NULL, summarized_above boolean NOT NULL DEFAULT FALSE,
                 micro_compacted boolean NULL, extra_info text NULL, micro_compact_strategy text NULL);
               CREATE TABLE chat_turn ( id integer NOT NULL PRIMARY KEY AUTOINCREMENT, session_id text NOT NULL,
                 turn_id text NOT NULL, reply_to_message_id text NOT NULL, response_message_id text NOT NULL,
                 requirement_id text NOT NULL, rewritten_user_message text NOT NULL, turn_status text NOT NULL,
                 error_message text NOT NULL, context text NOT NULL, created_at bigint NOT NULL DEFAULT 0,
                 updated_at bigint NOT NULL DEFAULT 0, deleted_at bigint NOT NULL DEFAULT 0,
                 agent_type text NOT NULL DEFAULT 'builder', agent_id text NOT NULL DEFAULT '',
                 agent_name text NOT NULL DEFAULT '', additional_context text NOT NULL DEFAULT '{}',
                 agent_avatar_id text, is_worktree boolean NOT NULL DEFAULT FALSE, products text, merged_commit text NULL);
               CREATE TABLE chat_session ( id integer NOT NULL PRIMARY KEY AUTOINCREMENT, session_id text NOT NULL,
                 project_id text NOT NULL, created_at bigint NOT NULL DEFAULT 0, updated_at bigint NOT NULL DEFAULT 0,
                 deleted_at bigint NOT NULL DEFAULT 0, session_type text NOT NULL DEFAULT 'side_chat',
                 agent_process_support text NOT NULL DEFAULT 'v2', session_title text NOT NULL DEFAULT '',
                 worktree_id text NOT NULL DEFAULT '', context text NOT NULL DEFAULT '{}', work_mode text NULL,
                 session_icon text NOT NULL DEFAULT '', hidden_status text NULL, is_pinned boolean NOT NULL DEFAULT FALSE,
                 pinned_at bigint NOT NULL DEFAULT 0, extra text NOT NULL DEFAULT '{}', is_unread boolean NOT NULL DEFAULT FALSE,
                 last_unread_turn_id text NOT NULL DEFAULT '');
               CREATE TABLE project ( id integer NOT NULL PRIMARY KEY AUTOINCREMENT, project_id text NOT NULL,
                 source text NOT NULL DEFAULT 'native-ide', user_id text NOT NULL, name text NULL, description text NULL,
                 absolute_path text NULL, biz_project_id text NOT NULL, created_at bigint NULL, updated_at bigint NULL,
                 deleted_at bigint NULL, workspace_status text NOT NULL DEFAULT 'single_root',
                 transient_fallback_project_id text NULL, remote_project_id text NULL, last_active_at bigint NULL,
                 work_mode text NULL, workspace_type text NOT NULL DEFAULT 'single_root',
                 single_root_workspace_folder text NULL, multi_root_workspace_id text NULL,
                 multi_root_workspace_config_path text NULL, workspace_folders text NOT NULL DEFAULT '[]',
                 project_kind text NOT NULL DEFAULT 'workspace', domain_version integer NOT NULL DEFAULT 1);"#,
        )
        .unwrap();

        // 与故障会话同形：user 那条只有 content/role
        let user = json!({
            "raw_messages": [{
                "content": [{ "text": "双组分聚氨酯碎石透水混合料的BPN最大可以做到多少", "type": "text" }],
                "role": "user"
            }]
        })
        .to_string();
        // 带工具调用的那条：缺 index/type，content 缺 image_url 等键
        let asst = json!({
            "raw_messages": [{
                "content": [{ "text": "", "type": "text" }],
                "name": null,
                "role": "assistant",
                "tool_call_id": null,
                "tool_calls": [{
                    "id": "call_1",
                    "function_call": { "name": "WebSearch", "arguments": "{}" }
                }]
            }]
        })
        .to_string();
        c.execute(
            "INSERT INTO history_v2(id, history_v2_id, session_id, message_id, messages, token_usage) \
             VALUES(1,'h1','s1','m1',?1,0)",
            params![user],
        )
        .unwrap();
        c.execute(
            "INSERT INTO history_v2(id, history_v2_id, session_id, message_id, messages, token_usage) \
             VALUES(2,'h2','s1','m2',?1,0)",
            params![asst],
        )
        .unwrap();
        // 会话与回合也建上，heal 的镜像重建那一步会用到
        c.execute(
            "INSERT INTO chat_session(session_id, project_id, session_title) VALUES('s1','p1','测试')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO project(project_id, user_id, biz_project_id, absolute_path, \
                single_root_workspace_folder, workspace_folders, domain_version) \
             VALUES('p1','u1','b1','d:/t','d:/t','[]',2)",
            [],
        )
        .unwrap();

        let n = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert!(
            n["history_json"].as_u64().unwrap_or(0) >= 2,
            "两行都该被修，heal 返回 {n}"
        );

        for id in [1, 2] {
            let raw: String = c
                .query_row("SELECT messages FROM history_v2 WHERE id=?1", params![id], |r| r.get(0))
                .unwrap();
            let v: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(
                v.get("mode").and_then(Value::as_str),
                Some("FunctionCall"),
                "缺 mode 会让新版客户端整条会话加载失败（995001）"
            );
            for m in v["raw_messages"].as_array().unwrap() {
                for part in m["content"].as_array().unwrap() {
                    for k in ["image_url", "video_url", "cache_control"] {
                        assert!(part.get(k).is_some(), "content 缺 {k}");
                    }
                }
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        assert!(call.get("index").is_some(), "tool_calls 缺 index");
                        assert_eq!(call.get("type").and_then(Value::as_str), Some("function"));
                    }
                }
            }
        }

        // 幂等：再跑一遍不应该再改任何行
        let again = heal_legacy_rows(&c, &|_| {}).unwrap();
        assert_eq!(again["history_json"].as_u64().unwrap_or(0), 0);
    }

}
