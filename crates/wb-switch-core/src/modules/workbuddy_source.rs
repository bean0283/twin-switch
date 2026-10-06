//! WorkBuddy 会话来源：发现并解析本机 WorkBuddy 桌面版的会话记录。
//!
//! WorkBuddy 的会话由「数据三件套」定义（缺一不可）：
//!   1) 正文：`~/.workbuddy/projects/{工作区目录}/{会话id}.jsonl`（JSONL，一行一个事件）
//!   2) 元数据：`~/.workbuddy/workbuddy.db` → `sessions` 表（标题 / 工作区 / 归属账号 / 时间）
//!   3) 云端映射：`~/.workbuddy/edge-sync-mapping-v{N}.db`（本工具只读，不参与移植）
//!
//! **识别依据**：会话 id 就是正文 JSONL 的文件名词干（带连字符的 UUID），与 `sessions.id`
//! 完全一致。工作区子目录名只是分类，不参与识别——定位时遍历 `projects/` 下所有子目录。
//!
//! 正文事件类型：`session-meta` / `message`（user、assistant）/ `reasoning` /
//! `function_call` / `function_call_result` / `file-history-snapshot` / `ai-title`。
//! 一个「回合」= `message(user)` → 若干过程事件 → `message(assistant)`。
//!
//! 本模块只读：绝不修改 WorkBuddy 的任何文件。读取 `workbuddy.db` 时先复制
//! db + wal + shm 到工具目录再打开，避免与运行中的 WorkBuddy 争抢 WAL 锁。

use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

use crate::modules::config::store_dir;

/// WorkBuddy 数据根目录名（位于用户主目录下）。
const WB_DIR_NAME: &str = ".workbuddy";

/// WorkBuddy 数据根：`~/.workbuddy`。
pub fn data_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(WB_DIR_NAME)
}

/// 能力探测：数据根同时具备 `projects/` 目录与 `workbuddy.db` 才算可用。
pub fn is_available() -> bool {
    let root = data_root();
    root.join("projects").is_dir() && root.join("workbuddy.db").is_file()
}

/// 会话列表项（元数据来自 `sessions` 表，正文有无来自磁盘探测）。
#[derive(Serialize, Clone, Debug)]
pub struct WbSession {
    /// 会话 id（UUID 带连字符）。
    pub id: String,
    /// 标题：优先 `custom_title`，回退 `title`。
    pub title: String,
    /// 工作目录（原样，未做归一化）。
    pub cwd: String,
    /// 归属账号 uid。
    pub user_id: String,
    /// 创建时间（毫秒）。
    pub created_at: i64,
    /// 最后更新时间（毫秒）。
    pub updated_at: i64,
    /// 模型名（可能为空）。
    pub model: String,
    /// 是否已删除（`deleted_at` 非空）。
    pub deleted: bool,
    /// 磁盘上是否存在正文 JSONL。
    pub has_body: bool,
    /// 正文文件字节数（无正文为 0）。
    pub body_bytes: u64,
    /// 正文行数（事件条数；无正文为 0）。
    pub body_lines: usize,
    /// 正文内容摘要（逐行摘要的汇总，前 16 位十六进制；无正文为空）。
    ///
    /// 归一化时会把「本副本自己的会话 id」替换成固定标记，于是同一段对话在两个
    /// 账号里的两份副本可以拿到**同一个摘要**——这正是「哪些是同一份、谁更新」的判据。
    pub content_digest: String,
    /// 同标题分组键（归一化标题；唯一标题时为空串）。
    pub dup_group: String,
    /// 同组内是否为最新副本（无重复时为 true）。
    pub is_newest: bool,
    /// 相对最新副本的差异说明（最新副本为「共 N 份重复」，无重复为空串）。
    pub dup_note: String,
}

/// 同标题分组的判定上限：超过这个成员数就不做逐行摘要比对，
/// 只按 `updated_at` + 行数给结论（避免在大仓库上读上百个 10 MB 级正文）。
const DUP_MAX_MEMBERS: usize = 8;
/// 单个正文参与逐行摘要比对的体积上限。
const DUP_MAX_BYTES: u64 = 64 << 20;

/// 正文内容身份：行数 + 逐行摘要 + 总摘要。
///
/// 逐行摘要对「长度编码 + 归一化行字节」取 SHA-256，记录分隔无歧义；行顺序、重复次数
/// 都参与摘要，因此重排、去重、重写都会得到不同的内容身份。
#[derive(Debug, Clone)]
struct ContentId {
    lines: usize,
    line_digests: Vec<String>,
    total: String,
}

/// 两份正文的关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// 逐行完全相同
    Equal,
    /// 前者是后者的严格有序前缀（后者更新）
    Prefix,
    /// 后者是前者的严格有序前缀（前者更新）
    Extension,
    /// 内容分叉（同源但各写各的）
    Diverged,
}

/// 行摘要：长度编码 + 归一化后的行字节。
fn line_digest(line: &str) -> String {
    use sha2::{Digest, Sha256};
    let bytes = line.as_bytes();
    let mut h = Sha256::new();
    h.update((bytes.len() as u64).to_be_bytes());
    h.update(bytes);
    to_hex(&h.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 由有序行摘要推导总摘要（与 `~/.wb-switch` 的 session_link 同构，
/// 便于用同一套口径解释「内容是否一致」）。
fn total_digest(line_digests: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"wb-switch-lines-v1\0");
    h.update((line_digests.len() as u64).to_be_bytes());
    for d in line_digests {
        h.update(d.as_bytes());
        h.update([0u8]);
    }
    to_hex(&h.finalize())
}

/// 读一份正文 JSONL 的内容身份。
///
/// `own_session_id` 会被替换成固定标记再逐行摘要：同一段对话在两个账号里的副本
/// 会话 id 不同，但其余内容一致，于是能得到相同的摘要。
fn content_id(path: &std::path::Path, own_session_id: &str) -> Option<ContentId> {
    const MARKER: &str = "__wb_session_id__";
    let text = std::fs::read_to_string(path).ok()?;
    let mut line_digests = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.trim().is_empty() {
            continue;
        }
        // 非 JSON 行也算一条记录（保守：宁可判定为「内容不同」也不视为一致）
        line_digests.push(line_digest(&line.replace(own_session_id, MARKER)));
    }
    if line_digests.is_empty() {
        return None;
    }
    let total = total_digest(&line_digests);
    Some(ContentId {
        lines: line_digests.len(),
        line_digests,
        total,
    })
}

fn is_ordered_prefix(prefix: &[String], full: &[String]) -> bool {
    prefix.len() <= full.len() && full[..prefix.len()] == *prefix
}

fn relate(a: &ContentId, b: &ContentId) -> Relation {
    if a.lines == b.lines && a.total == b.total {
        return Relation::Equal;
    }
    if is_ordered_prefix(&a.line_digests, &b.line_digests) {
        return Relation::Prefix;
    }
    if is_ordered_prefix(&b.line_digests, &a.line_digests) {
        return Relation::Extension;
    }
    Relation::Diverged
}

/// 标题归一化：小写、压缩空白、去掉 WorkBuddy 列表里的截断省略号。
///
/// 只用于**分组**（同标题才需要比对），不参与任何写库。
fn norm_title(title: &str) -> String {
    let mut s = title.trim().to_lowercase();
    for suffix in ['…', '⋯'] {
        while s.ends_with(suffix) {
            s.pop();
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 纯决策：同标题一组里哪一份是最新，其余各差多少。
///
/// 判定顺序（与 `~/.wb-switch` 的 session_link 同一思路）：
/// 1. **内容包含关系优先于时间**：某一份的逐行摘要是其它所有成员的有序前缀
///    ⇒ 那一份最新（它把别人的历史全包含了）。即使它的 `updated_at` 更旧，
///    也以内容为准——时钟在多账号/多设备下并不可靠。
/// 2. 没有这样的成员（内容分叉）⇒ 按 `updated_at`，再按行数。
/// 3. 逐行摘要完全相同 ⇒ 走第 2 条，并在说明里点明「内容完全相同」。
///
/// 返回 `(最新成员下标, 每个成员相对最新的说明)`。
fn decide_group(ids: &[Option<ContentId>], updated_at: &[i64]) -> (usize, Vec<String>) {
    // 「覆盖其余所有成员」的候选。逐行完全相同的副本会互相覆盖 ⇒ 候选不止一个，
    // 这时不能靠位置先后定输赢，必须回落到时间戳（否则最新的是列表里靠前的那份，
    // 而它可能恰恰是旧的）。
    let covering: Vec<usize> = (0..ids.len())
        .filter(|&pos| {
            let Some(cid) = ids[pos].as_ref() else {
                return false;
            };
            ids.iter().enumerate().all(|(other, oid)| {
                if other == pos {
                    return true;
                }
                match oid {
                    Some(o) => {
                        o.lines <= cid.lines && is_ordered_prefix(&o.line_digests, &cid.line_digests)
                    }
                    // 没有正文的成员不算「被包含」，否则会把空壳选成最新
                    None => true,
                }
            })
        })
        .collect();

    let pick_by_clock = |pool: &[usize]| -> usize {
        let mut best = pool[0];
        for &pos in &pool[1..] {
            let better = (updated_at[pos], ids[pos].as_ref().map(|c| c.lines).unwrap_or(0))
                > (updated_at[best], ids[best].as_ref().map(|c| c.lines).unwrap_or(0));
            if better {
                best = pos;
            }
        }
        best
    };

    let newest_pos = if covering.len() == 1 {
        covering[0]
    } else if !covering.is_empty() {
        pick_by_clock(&covering)
    } else {
        let all: Vec<usize> = (0..ids.len()).collect();
        pick_by_clock(&all)
    };

    let newest_id = ids[newest_pos].clone();
    let notes = ids
        .iter()
        .enumerate()
        .map(|(pos, id)| {
            if pos == newest_pos {
                return String::new(); // 由调用方填「共 N 份」
            }
            match (id, &newest_id) {
                (Some(me), Some(top)) => match relate(me, top) {
                    Relation::Equal => "与最新副本内容完全相同".to_string(),
                    Relation::Prefix => format!("比最新副本少 {} 条记录", top.lines - me.lines),
                    Relation::Extension => {
                        format!("比最新副本多 {} 条记录", me.lines - top.lines)
                    }
                    Relation::Diverged => {
                        format!("内容分叉（本副本 {} 条 / 最新 {} 条）", me.lines, top.lines)
                    }
                },
                (None, _) => "无正文，不参与比对".to_string(),
                (Some(me), None) => format!("{} 条记录（最新副本无正文）", me.lines),
            }
        })
        .collect();
    (newest_pos, notes)
}

/// 给列表补上「同标题副本」信息：哪一份最新、其余各差多少。
///
/// 读盘（逐行摘要）只对**同标题分组**内的成员做，且限制成员数与单文件体积，
/// 避免在大仓库上一次性读上百个 10 MB 级正文。
fn annotate_duplicates(list: &mut [WbSession]) {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, s) in list.iter().enumerate() {
        let key = norm_title(&s.title);
        if key.is_empty() {
            continue;
        }
        groups.entry(key).or_default().push(i);
    }

    for (key, idx) in groups {
        if idx.len() < 2 {
            continue;
        }
        // 逐行摘要只对「有正文且在体积上限内」的成员计算
        let mut ids: Vec<Option<ContentId>> = Vec::with_capacity(idx.len());
        let detailed = idx.len() <= DUP_MAX_MEMBERS
            && idx
                .iter()
                .all(|&i| list[i].has_body && list[i].body_bytes <= DUP_MAX_BYTES);
        for &i in &idx {
            if !list[i].has_body {
                ids.push(None);
                continue;
            }
            if detailed {
                let id = find_jsonl(&list[i].id)
                    .and_then(|p| content_id(&p, &list[i].id));
                ids.push(id);
            } else {
                ids.push(None);
            }
            if let (Some(cid), j) = (ids.last().and_then(|v| v.as_ref()), i) {
                list[j].body_lines = cid.lines;
                list[j].content_digest = cid.total.chars().take(16).collect();
            }
        }

        let updated: Vec<i64> = idx.iter().map(|&i| list[i].updated_at).collect();
        let (newest_pos, mut notes) = decide_group(&ids, &updated);
        notes[newest_pos] = format!("共 {} 份重复标题副本", idx.len());

        let group_label: String = key.chars().take(16).collect();
        for (pos, &i) in idx.iter().enumerate() {
            list[i].dup_group = group_label.clone();
            list[i].is_newest = pos == newest_pos;
            list[i].dup_note = std::mem::take(&mut notes[pos]);
        }
    }

    // 非重复项：没有分组，视为「唯一副本」
    for s in list.iter_mut() {
        if s.dup_group.is_empty() {
            s.is_newest = true;
            s.dup_note = String::new();
        }
    }
}

/// 一条过程事件（思考 / 工具调用 / 工具结果），按原始顺序保留。
#[derive(Serialize, Clone, Debug)]
pub struct WbEvent {
    /// `reasoning` / `function_call` / `function_call_result`。
    pub kind: String,
    /// 工具名（思考事件为空）。
    pub name: String,
    /// 工具调用 id（用于把 call 与 result 配对）。
    pub call_id: String,
    /// 结果状态（`function_call_result` 才有）。
    pub status: String,
    /// 文本内容：思考正文 / 调用参数 / 结果文本。
    pub text: String,
}

/// 一个回合：一条用户提问 + 对应的助手最终回答 + 中间过程事件。
///
/// WorkBuddy 的一个回合里，助手会**多次**发 `message`（过程中间叙述），最后一条才是
/// 最终回答。`assistant_text` 保存最后那条（= 最终回答），全部中间叙述连同思考与工具
/// 调用按原始顺序进入 `events`，以保证完整还原过程。
#[derive(Serialize, Clone, Debug, Default)]
pub struct WbTurn {
    pub user_text: String,
    pub assistant_text: String,
    pub events: Vec<WbEvent>,
    /// 回合起始时间（毫秒），取用户消息的时间戳。
    pub created_at: i64,
    /// 回合结束时间（毫秒），取该回合最后一条事件的时间戳。
    pub updated_at: i64,
}

/// 一个会话的完整正文。
#[derive(Serialize, Clone, Debug, Default)]
pub struct WbBody {
    pub session_id: String,
    /// `ai-title` 事件里的自动标题（比 sessions.title 更贴近正文）。
    pub ai_title: Option<String>,
    pub turns: Vec<WbTurn>,
}

// ---------------------------------------------------------------------------
// 会话列表
// ---------------------------------------------------------------------------

/// 把 `workbuddy.db`（含 WAL/SHM）复制到工具目录后打开，避免与运行中的客户端
/// 争抢 WAL 锁。复制品可正常回放 WAL，读到最新数据。
pub fn snapshot_db() -> Result<PathBuf, String> {
    let root = data_root();
    let dir = store_dir().join("workbuddy_scan");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 WorkBuddy 快照目录失败: {e}"))?;
    let names = ["workbuddy.db", "workbuddy.db-wal", "workbuddy.db-shm"];
    for name in names {
        let src = root.join(name);
        if src.is_file() {
            std::fs::copy(&src, dir.join(name))
                .map_err(|e| format!("复制 {name} 失败: {e}"))?;
        }
    }
    let dst = dir.join("workbuddy.db");
    if !dst.is_file() {
        return Err(format!(
            "未找到 WorkBuddy 会话库：{}",
            root.join("workbuddy.db").display()
        ));
    }
    Ok(dst)
}

/// 列出本机全部 WorkBuddy 会话（含已删除标记，由前端决定是否展示）。
pub fn list_sessions() -> Result<Vec<WbSession>, String> {
    if !is_available() {
        return Err(format!(
            "本机未检测到 WorkBuddy 数据（缺 projects/ 或 workbuddy.db）：{}",
            data_root().display()
        ));
    }
    let db = snapshot_db()?;
    let conn = rusqlite::Connection::open(&db).map_err(|e| format!("打开 WorkBuddy 快照失败: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, cwd, user_id, title, custom_title, created_at, updated_at, \
             deleted_at, model FROM sessions",
        )
        .map_err(|e| format!("查询 sessions 失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            let id: String = r.get(0)?;
            let cwd: Option<String> = r.get(1).ok();
            let user_id: Option<String> = r.get(2).ok();
            let title: Option<String> = r.get(3).ok();
            let custom: Option<String> = r.get(4).ok();
            let created: i64 = r.get(5).unwrap_or(0);
            let updated: i64 = r.get(6).unwrap_or(0);
            let deleted: Option<i64> = r.get(7).ok();
            let model: Option<String> = r.get(8).ok();
            Ok((id, cwd, user_id, title, custom, created, updated, deleted, model))
        })
        .map_err(|e| format!("读取 sessions 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("解析 sessions 行失败: {e}"))?;
    drop(stmt);
    drop(conn);

    let mut out: Vec<WbSession> = Vec::with_capacity(rows.len());
    for (id, cwd, user_id, title, custom, created, updated, deleted, model) in rows {
        let body = find_jsonl(&id);
        let body_bytes = body
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        out.push(WbSession {
            title: custom
                .or(title)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "(无标题)".to_string()),
            id,
            cwd: cwd.unwrap_or_default(),
            user_id: user_id.unwrap_or_default(),
            created_at: created,
            updated_at: updated,
            model: model.unwrap_or_default(),
            deleted: deleted.map(|d| d != 0).unwrap_or(false),
            has_body: body.is_some(),
            body_bytes,
            body_lines: 0,
            content_digest: String::new(),
            dup_group: String::new(),
            is_newest: true,
            dup_note: String::new(),
        });
    }
    // 按最后更新时间倒序，与客户端列表观感一致
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    // 同标题副本的新旧判定（多账号时同一段对话会有多份，必须能分清谁是最新）。
    // 不重排列表：重复副本的时间本来就接近，天然相邻，全局「最近优先」的顺序更顺手。
    annotate_duplicates(&mut out);
    Ok(out)
}

/// 在 `projects/` 下任意子目录中定位 `{会话id}.jsonl`。
pub fn find_jsonl(session_id: &str) -> Option<PathBuf> {
    let projects = data_root().join("projects");
    if !projects.is_dir() {
        return None;
    }
    let file_name = format!("{session_id}.jsonl");
    // 直接位于 projects/ 根下的情况
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

// ---------------------------------------------------------------------------
// 正文解析
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 用户消息的「真实 / 注入」判别
// ---------------------------------------------------------------------------

/// WorkBuddy 会把**客户端自己生成**的内容也写成 `role=user`。
///
/// 它们不是用户敲的字，客户端界面也不会把它们当提问展示。若原样透传，Trae 里就会
/// 凭空多出用户从没见过的「用户消息」，以及为它们补出的**空白助手气泡** ——
/// 这正是「Trae 显示与 WorkBuddy 不一致」的主因（v0.0.13 复盘）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMsgKind {
    /// 真实提问（正文包在 `<user_query>…</user_query>` 里）。
    Real,
    /// 纯内部上下文摘要：`<conversation_history_summary>` / `<cb_summary>`。
    ///
    /// 实测**没有任何助手回答**跟它配对（回答属于紧随其后的 `Please continue`），
    /// 且长度可达 1.5×10⁵ 字符。整条丢弃。
    SilentSummary,
    /// 自动续写指令 / 后台任务通知：`Please continue with…`、`<task-notification>`。
    ///
    /// 同样不是用户输入，但**有**助手回答 —— 丢掉这层壳，让回答并入当前回合。
    AutoTrigger,
}

/// 判定注入类型时只看**行首**，避免在摘要正文里误伤
/// （`<cb_summary>` 正文里会成段引用 `<system-reminder>` 与 `<user_query>`）。
const SUMMARY_PREFIXES: [&str; 2] = ["<conversation_history_summary>", "<cb_summary>"];
const AUTO_PREFIXES: [&str; 4] = [
    "<task-notification",
    "Please continue with the conversation based on the summarized context",
    // `<task-notification>` 块**之后**还挂着两段指令尾巴，它们不在标签内
    // （只剥标签会把这些文字留下来当提问 —— 实测 6 条用户消息就是这样来的）。
    "Use the TaskOutput tool with task_id=",
    "IMPORTANT: Before responding, scroll back",
];

/// 判断一条 `role=user` 消息到底是不是用户在提问。
pub fn classify_user_message(raw: &str) -> UserMsgKind {
    let t = raw.trim_start();
    if SUMMARY_PREFIXES.iter().any(|p| t.starts_with(p)) {
        return UserMsgKind::SilentSummary;
    }
    if AUTO_PREFIXES.iter().any(|p| t.starts_with(p)) {
        return UserMsgKind::AutoTrigger;
    }
    UserMsgKind::Real
}

/// 从**行首**逐个剥掉完整的注入块；碰到不是注入块的内容立刻停手。
///
/// 只剥行首（旧实现是全文搜索）很关键：`<cb_summary>` 正文里引用了同名标签，
/// 全文搜索会把摘要**拦腰削断**（实测 80947 字符被削成 8614 字符的乱码）。
fn strip_leading_blocks(text: &str) -> &str {
    const BLOCKS: [&str; 2] = ["<system-reminder", "<task-notification"];
    let mut rest = text;
    loop {
        let t = rest.trim_start();
        let Some(tag) = BLOCKS.iter().find(|tag| t.starts_with(**tag)) else {
            return t;
        };
        let close = format!("</{}>", tag.trim_start_matches('<'));
        match t.find(&close) {
            Some(i) => rest = &t[i + close.len()..],
            // 只有开标签没有闭合：整段视为注入
            None => return "",
        }
    }
}

/// 从用户消息文本中取出**真实提问**。
///
/// 结构恒定：`[system-reminder 块…][<user_query>…</user_query>][附加数据块…]`。
/// 必须先剥**行首**的注入块、再认 `<user_query>`，不能全文 `find` ——
/// 摘要正文里最多出现 7 个 `<user_query>`，全文搜索会取出一段毫不相干的文字
/// （实测取到的是源码片段，甚至上一轮的续写指令）。
fn extract_user_text(text: &str) -> String {
    let raw = text.trim();
    if classify_user_message(raw) != UserMsgKind::Real {
        return String::new();
    }
    let stripped = strip_leading_blocks(raw);
    if let Some(rest) = stripped.strip_prefix("<user_query>") {
        let end = rest.find("</user_query>").unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    stripped.trim().to_string()
}

/// 拼接 `message` 事件 `content[]` 里所有文本块（assistant 用 `output_text`）。
fn join_content_blocks(content: &Value) -> String {
    let Some(blocks) = content.as_array() else {
        return content.as_str().unwrap_or_default().trim().to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        if let Some(t) = block.get("text").and_then(Value::as_str) {
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }
    }
    parts.join("\n").trim().to_string()
}

fn ts_of(v: &Value) -> i64 {
    v.get("timestamp").and_then(Value::as_i64).unwrap_or(0)
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// 解析单个会话的正文 JSONL 为归一化回合序列。
pub fn load_body(session_id: &str) -> Result<WbBody, String> {
    let path = find_jsonl(session_id).ok_or_else(|| {
        format!(
            "会话 {session_id} 的正文文件不存在（projects/ 下未找到 {session_id}.jsonl）"
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    Ok(parse_body(session_id, &text))
}

/// 从 JSONL 文本解析为归一化回合序列。
///
/// 与 [`load_body`] 走的是**同一套规则**：导出方向（`workbuddy_export`）生成的正文
/// 可以直接喂进来校验，离线测试因此能覆盖「写出去 → 再读回来」的完整闭环，
/// 而不是各写一份可能同时出错的解析。
pub fn parse_body(session_id: &str, text: &str) -> WbBody {
    let mut body = WbBody {
        session_id: session_id.to_string(),
        ..Default::default()
    };
    let mut current: Option<WbTurn> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            // 单行损坏不阻断整个会话（WorkBuddy 偶有截断行）
            continue;
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "ai-title" => {
                let t = str_of(&v, "aiTitle");
                if !t.is_empty() {
                    body.ai_title = Some(t);
                }
            }
            "message" => {
                let role = str_of(&v, "role");
                let content = v.get("content").cloned().unwrap_or(Value::Null);
                match role.as_str() {
                    "user" => {
                        let raw = join_content_blocks(&content);
                        match classify_user_message(&raw) {
                            // 纯内部摘要：界面上看不到，也没有助手回答配对 —— 整条不要。
                            // （旧实现在这里建回合，于是 Trae 里多出 1.2 万～1.5 万字符的
                            //   「用户消息」，还配了一个空白助手气泡。）
                            UserMsgKind::SilentSummary => {}
                            // 自动续写 / 后台任务通知：不是用户输入，但**有**助手回答。
                            // 不闭合当前回合，让随后的 assistant 事件落进上一个真实回合，
                            // 既不凭空多出用户消息，也不丢掉助手的输出。
                            UserMsgKind::AutoTrigger => {
                                if current.is_none() {
                                    // 极端情况（会话以注入开头）：仍开一个回合接住回答，
                                    // 否则这段助手输出会被静默丢弃。
                                    current = Some(WbTurn::default());
                                }
                            }
                            // 真实提问：闭合上一回合，开新回合。
                            UserMsgKind::Real => {
                                // 上一条回合没有等到新的提问即已结束（正常情况：下一轮提问）
                                if let Some(prev) = current.take() {
                                    body.turns.push(prev);
                                }
                                let ts = ts_of(&v);
                                current = Some(WbTurn {
                                    user_text: extract_user_text(&raw),
                                    created_at: ts,
                                    updated_at: ts,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    "assistant" => {
                        // 助手在一个回合里会多次发 message：中间叙述全部保留为过程事件，
                        // 最后一条同时作为该回合的「最终回答」。
                        let text = join_content_blocks(&content);
                        if text.is_empty() {
                            continue;
                        }
                        let ts = ts_of(&v);
                        let turn = current.get_or_insert_with(WbTurn::default);
                        turn.events.push(WbEvent {
                            kind: "assistant_text".to_string(),
                            name: String::new(),
                            call_id: String::new(),
                            status: String::new(),
                            text: text.clone(),
                        });
                        turn.assistant_text = text;
                        turn.updated_at = ts;
                    }
                    _ => {}
                }
            }
            "reasoning" | "function_call" | "function_call_result" => {
                let Some(turn) = current.as_mut() else {
                    continue;
                };
                let event = match kind {
                    "reasoning" => {
                        let raw = v.get("rawContent").and_then(Value::as_array);
                        let text = raw
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        if text.trim().is_empty() {
                            continue;
                        }
                        WbEvent {
                            kind: kind.to_string(),
                            name: String::new(),
                            call_id: String::new(),
                            status: String::new(),
                            text: text.trim().to_string(),
                        }
                    }
                    "function_call" => WbEvent {
                        kind: kind.to_string(),
                        name: str_of(&v, "name"),
                        call_id: str_of(&v, "callId"),
                        status: String::new(),
                        // 真实数据里 `arguments` 是**字符串**（内容形如 `{"file_path":"…"}`）。
                        // 原来直接 `a.to_string()` 会把它再序列化一遍，得到
                        // `"{\"file_path\":\"…\"}"`（外层多一对引号 = 双重编码）；
                        // 下游 `parse_params` 于是解析成 Value::String 而非对象，
                        // 落到 Trae 的 tool_call_info.params 就成了字符串 —— 工具卡片
                        // 取不到参数，表现为「WorkBuddy 的过程在 Trae 显示奇怪」。
                        // 所以字符串优先取内容，只有 arguments 本身是对象/数组时才序列化。
                        text: v
                            .get("arguments")
                            .map(|a| {
                                a.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| a.to_string())
                            })
                            .unwrap_or_default(),
                    },
                    _ => {
                        // 工具结果文本的抽取顺序很关键，抽错会让 Trae 卡片显示成
                        // 一整坨带引号/带转义的原始 JSON：
                        //   ① output 本身是字符串 → 直接用（否则 to_string 会再包一层引号）
                        //   ② output 是对象且有 text → 取 text（常态：{type,text}）
                        //   ③ output 是数组 → 拼各段 text（如 present_files 的 input_text）
                        //   ④ 其余 → 序列化兜底
                        let text = match v.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(o) => {
                                if let Some(t) = o.get("text").and_then(Value::as_str) {
                                    t.to_string()
                                } else if let Some(arr) = o.as_array() {
                                    let parts: Vec<&str> = arr
                                        .iter()
                                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                                        .collect();
                                    if parts.is_empty() {
                                        o.to_string()
                                    } else {
                                        parts.join("\n")
                                    }
                                } else {
                                    o.to_string()
                                }
                            }
                            None => String::new(),
                        };
                        WbEvent {
                            kind: kind.to_string(),
                            name: str_of(&v, "name"),
                            call_id: str_of(&v, "callId"),
                            status: str_of(&v, "status"),
                            text,
                        }
                    }
                };
                turn.events.push(event);
                turn.updated_at = ts_of(&v).max(turn.updated_at);
            }
            _ => {}
        }
    }
    if let Some(rest) = current {
        body.turns.push(rest);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_text_prefers_user_query_block() {
        let raw = "<system-reminder data-role=\"user-context\">\n一堆上下文\n</system-reminder>\n<user_query>真实提问</user_query>";
        assert_eq!(extract_user_text(raw), "真实提问");
    }

    #[test]
    fn user_text_strips_leading_reminder_only() {
        // 只剥行首注入块；正文里 `提到的` 标签不再被误伤
        let raw = "<system-reminder>上下文</system-reminder>\n<user_query>真实提问</user_query>";
        assert_eq!(extract_user_text(raw), "真实提问");

        // 没有 user_query 时，行首的注入块照样剥掉
        let raw = "<system-reminder>上下文</system-reminder>\n剩下的话";
        assert_eq!(extract_user_text(raw), "剩下的话");
    }

    #[test]
    fn user_text_does_not_scan_body_for_tags() {
        // 旧实现在**全文**搜 `<system-reminder`／`<user_query>`，会把引用了这些标签的
        // 正文拦腰削断（实测把 80947 字符的摘要削成 8614 字符的乱码）。
        // 现在只剥行首 → 中间的标签原样保留。这是一条**回归护栏**。
        let raw = "开头\n<system-reminder>上下文</system-reminder>\n结尾";
        assert_eq!(
            extract_user_text(raw),
            "开头\n<system-reminder>上下文</system-reminder>\n结尾"
        );
    }

    #[test]
    fn classify_separates_injections_from_real_questions() {
        use UserMsgKind::*;
        // 纯内部摘要
        assert_eq!(classify_user_message("<conversation_history_summary>\nSummary:…"), SilentSummary);
        assert_eq!(classify_user_message("<cb_summary>\nSummary of the conversation so far:"), SilentSummary);
        // 自动续写 / 后台任务通知
        assert_eq!(
            classify_user_message(
                "Please continue with the conversation based on the summarized context above."
            ),
            AutoTrigger
        );
        assert_eq!(classify_user_message("<task-notification>\n<task-id>x</task-id>\n</task-notification>"), AutoTrigger);
        assert_eq!(
            classify_user_message("Use the TaskOutput tool with task_id=\"x\" to retrieve the full output"),
            AutoTrigger
        );
        // 真实提问
        assert_eq!(classify_user_message("<system-reminder>x</system-reminder>\n<user_query>提问</user_query>"), Real);
        assert_eq!(classify_user_message("我如何测试？怎么操作"), Real);
    }

    #[test]
    fn extract_user_text_ignores_injections_entirely() {
        // 注入条目本身不产出提问文本（即使正文里嵌着 <user_query>）
        let cb = "<cb_summary>\n…<user_query>\nPlease continue with the conversation based on the summarized context above.\n</user_query>\n</cb_summary>";
        assert_eq!(extract_user_text(cb), "");
        assert_eq!(extract_user_text("<conversation_history_summary>Summary:…"), "");
    }

    #[test]
    fn parse_body_drops_injections_and_keeps_replies() {
        // 一段最小会话：真实提问 → 回答；摘要（无回答）；续写（有回答）；真实提问 → 回答
        let ev = |role: &str, text: &str| {
            serde_json::json!({
                "type": "message",
                "role": role,
                "content": [{ "type": "input_text", "text": text }],
                "timestamp": 1_700_000_000_000i64,
            })
        };
        let mut lines: Vec<String> = Vec::new();
        let mut push = |v: Value| lines.push(v.to_string());
        push(ev("user", "<system-reminder>x</system-reminder>\n<user_query>第一个问题</user_query>"));
        push(ev("assistant", "第一个回答"));
        push(ev("user", "<conversation_history_summary>\nSummary: 一大段"));
        push(ev("user", "Please continue with the conversation based on the summarized context above."));
        push(ev("assistant", "续写的回答"));
        push(ev("user", "<system-reminder>y</system-reminder>\n<user_query>第二个问题</user_query>"));
        push(ev("assistant", "第二个回答"));
        drop(push);
        let text = lines.join("\n");

        let body = parse_body("s", &text);
        assert_eq!(body.turns.len(), 2, "注入条目不得产生回合");
        assert_eq!(body.turns[0].user_text, "第一个问题");
        // 摘要被丢弃、续写不新开回合 → 续写回答并入第一回合
        assert_eq!(body.turns[0].assistant_text, "续写的回答");
        assert_eq!(body.turns[1].user_text, "第二个问题");
        assert_eq!(body.turns[1].assistant_text, "第二个回答");
    }

    #[test]
    fn joins_text_blocks() {
        let v: Value = serde_json::json!([
            {"type": "output_text", "text": "第一段"},
            {"type": "output_text", "text": "第二段"}
        ]);
        assert_eq!(join_content_blocks(&v), "第一段\n第二段");
    }

    // -----------------------------------------------------------------
    // 同标题副本的新旧判定
    // -----------------------------------------------------------------

    fn cid(lines: &[&str]) -> ContentId {
        let d: Vec<String> = lines.iter().map(|l| line_digest(l)).collect();
        let t = total_digest(&d);
        ContentId {
            lines: d.len(),
            line_digests: d,
            total: t,
        }
    }

    #[test]
    fn norm_title_strips_truncation_ellipsis_and_case() {
        assert_eq!(norm_title("这份是之前执行的记录，你按之前记录继续完…"), "这份是之前执行的记录，你按之前记录继续完");
        assert_eq!(norm_title("Hello   World"), "hello world");
        assert_eq!(norm_title("  "), "");
    }

    #[test]
    fn relate_detects_prefix_extension_and_divergence() {
        let short = cid(&["a", "b"]);
        let long = cid(&["a", "b", "c"]);
        assert_eq!(relate(&short, &long), Relation::Prefix);
        assert_eq!(relate(&long, &short), Relation::Extension);
        assert_eq!(relate(&short, &short.clone()), Relation::Equal);
        assert_eq!(relate(&cid(&["a", "x"]), &long), Relation::Diverged);
    }

    fn mk(id: &str, title: &str, updated: i64, has_body: bool) -> WbSession {
        WbSession {
            id: id.to_string(),
            title: title.to_string(),
            cwd: String::new(),
            user_id: String::new(),
            created_at: 0,
            updated_at: updated,
            model: String::new(),
            deleted: false,
            has_body,
            body_bytes: if has_body { 10 } else { 0 },
            body_lines: 0,
            content_digest: String::new(),
            dup_group: String::new(),
            is_newest: true,
            dup_note: String::new(),
        }
    }

    #[test]
    fn duplicates_are_grouped_and_newest_marked() {
        // 两份同标题、都无正文：退化为按 updated_at 选最新
        let mut list = vec![
            mk("a", "查看 cargo 测试结果", 200, false),
            mk("b", "查看 cargo 测试结果", 100, false),
        ];
        annotate_duplicates(&mut list);
        assert!(!list[0].dup_group.is_empty());
        assert_eq!(list[0].dup_group, list[1].dup_group);
        assert!(list[0].is_newest, "updated_at 大的应判为最新");
        assert!(!list[1].is_newest);

        // 唯一标题：不分组、视为最新
        let mut solo = vec![mk("c", "独一份", 1, false)];
        annotate_duplicates(&mut solo);
        assert!(solo[0].dup_group.is_empty());
        assert!(solo[0].is_newest);
        assert!(solo[0].dup_note.is_empty());
    }

    #[test]
    fn content_containment_beats_timestamp() {
        // 0 号：时间更新，但只有 2 条；1 号：时间更旧，却包含 0 号的全部 2 条 + 1 条。
        // 结论必须是以内容为准 —— 1 号才是最新。
        let ids = vec![Some(cid(&["a", "b"])), Some(cid(&["a", "b", "c"]))];
        let (newest, notes) = decide_group(&ids, &[900, 100]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "比最新副本少 1 条记录");
        assert!(notes[1].is_empty(), "最新那条的说明由调用方填，此处应为空");
    }

    #[test]
    fn identical_content_falls_back_to_timestamp() {
        let ids = vec![Some(cid(&["a", "b"])), Some(cid(&["a", "b"]))];
        let (newest, notes) = decide_group(&ids, &[100, 900]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "与最新副本内容完全相同");
    }

    #[test]
    fn diverged_content_falls_back_to_timestamp_and_says_so() {
        let ids = vec![Some(cid(&["a", "x"])), Some(cid(&["a", "b", "c"]))];
        let (newest, notes) = decide_group(&ids, &[100, 900]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "内容分叉（本副本 2 条 / 最新 3 条）");
    }

    #[test]
    fn body_less_member_never_becomes_newest() {
        // 只有一个成员有正文时，最新必须有正文的那份（空壳没有可比内容）
        let ids = vec![None, Some(cid(&["a"]))];
        let (newest, notes) = decide_group(&ids, &[900, 100]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "无正文，不参与比对");
    }

    #[test]
    fn content_id_normalizes_own_session_id() {
        // 同一段对话存在两个账号里：除会话 id 外逐字相同 ⇒ 必须拿到同一个摘要
        let dir = std::env::temp_dir().join(format!("wbsrc-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        std::fs::write(&a, "{\"sessionId\":\"aaaa\",\"text\":\"hi\"}\n{\"n\":2}\n").unwrap();
        std::fs::write(&b, "{\"sessionId\":\"bbbb\",\"text\":\"hi\"}\n{\"n\":2}\n").unwrap();
        let ca = content_id(&a, "aaaa").unwrap();
        let cb = content_id(&b, "bbbb").unwrap();
        assert_eq!(ca.total, cb.total);
        assert_eq!(ca.lines, 2);
        // 行序不同则摘要不同（重排必须被识别为差异）
        std::fs::write(&b, "{\"n\":2}\n{\"sessionId\":\"bbbb\",\"text\":\"hi\"}\n").unwrap();
        assert_ne!(content_id(&b, "bbbb").unwrap().total, ca.total);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
