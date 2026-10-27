//! 关联会话：按「同一段对话」聚合各账号副本，并支持统一内容 / 补齐账号。
//!
//! ## 为什么是「按会话」而不是「按账号」
//!
//! 客户端把会话存在 `sessions` 表里，一行只属于一个 `user_id`；正文是
//! `projects/<工作区>/<会话id>.jsonl`，文件里的 `sessionId` 也必须是自己的 id。
//! 所以**同一段对话在每个账号下必然是独立的一份**，客户端没有任何跨账号共享机制。
//! 这个模块的作用就是把这些「其实是同一段对话」的副本认出来并摆在一起：
//!
//!   - 已有 [`workbuddy_sessions::LinkStore`] 关联记录的 → 以组为准（权威关系）
//!   - 没有关联记录、但标题相同的副本    → 按 `dup_group` 聚类（启发式关系）
//!
//! ## 状态判据
//!
//! | 状态 | 含义 | 判据 |
//! |---|---|---|
//! | `missing` | 内容缺失 | 组内有成员库行还在、正文文件没了 |
//! | `diverged` | 有分歧 | 存在 `diverged` / `ahead` 关系（各写各的，必须人选来源） |
//! | `pending` | 待同步 | 有 `behind` 副本（纯落后，可安全快进），或还有账号没这份会话 |
//! | `consistent` | 内容一致 | 全部同摘要且账号齐全 |
//! | `unknown` | 无法确认 | 标题被改过等原因没做逐行比对，**不能当成一致** |
//!
//! `unknown` 绝不并入 `consistent`：本工具宁可说「不知道」，也不能把两份可能不同的
//! 内容说成一样。

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::path::Path;

use crate::modules::config::store_dir;
use crate::modules::workbuddy_source::WbSession;
use crate::modules::{
    workbuddy_accounts, workbuddy_auth, workbuddy_sessions, workbuddy_source, workbuddy_switch,
    workbuddy_vault,
};

// ---------------------------------------------------------------------------
// 数据装配
// ---------------------------------------------------------------------------

/// 本机可用账号（未禁用），`(uid, 显示名)`。顺序稳定，便于界面一致。
fn usable_accounts() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for acc in workbuddy_vault::load_accounts() {
        if acc
            .get("disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let Some(uid) = workbuddy_auth::get_str(&acc, "uid") else {
            continue;
        };
        if uid.trim().is_empty() {
            continue;
        }
        if !out.iter().any(|(u, _)| *u == uid) {
            out.push((uid.clone(), workbuddy_accounts::label_for(&uid)));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

struct Member {
    uid: String,
    label: String,
    /// `None` = 组里登记了这个成员，但会话库里已经没有它（被删 / 换号后清理）。
    session: Option<WbSession>,
}

impl Member {
    fn cid(&self) -> &str {
        self.session.as_ref().map(|s| s.id.as_str()).unwrap_or("")
    }
    fn has_body(&self) -> bool {
        self.session.as_ref().map(|s| s.has_body).unwrap_or(false)
    }
    fn digest(&self) -> &str {
        self.session
            .as_ref()
            .map(|s| s.content_digest.as_str())
            .unwrap_or("")
    }
    fn relation(&self) -> &str {
        self.session
            .as_ref()
            .map(|s| s.dup_relation.as_str())
            .unwrap_or("")
    }
    fn lines(&self) -> Option<usize> {
        let s = self.session.as_ref()?;
        // `body_lines` 只在「同标题 ≥2」时才算过；0 表示没算，不是「空正文」。
        if s.has_body && s.body_lines > 0 {
            Some(s.body_lines)
        } else {
            None
        }
    }
    /// 来源模块给出的备注（如「比最新副本少 N 条」「共 N 份重复标题副本」）。
    fn notes(&self) -> String {
        self.session
            .as_ref()
            .map(|s| s.dup_note.clone())
            .unwrap_or_default()
    }
    fn updated_at(&self) -> i64 {
        self.session.as_ref().map(|s| s.updated_at).unwrap_or(0)
    }
    fn title(&self) -> String {
        self.session
            .as_ref()
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "(会话已不存在)".to_string())
    }
    fn project(&self) -> String {
        let cwd = self
            .session
            .as_ref()
            .map(|s| s.cwd.clone())
            .unwrap_or_default();
        project_label(&cwd)
    }
}

struct Group {
    key: String,
    /// 有正式关联记录时是组 id；来自同标题聚类时为 `None`。
    group_id: Option<String>,
    /// 备用 key：组变成正式关联组之后，原来按标题聚出来的 `dup:<标题>` 仍能打开它。
    ///
    /// ⚠️ 为什么需要：`unify` 会把同标题簇升级成正式关联组，key 从 `dup:标题` 变成
    /// `link:<id>`。界面手里那个旧 key 立刻失效，用户看到的就是「点开就报不存在」
    /// （2026-10-24 实测反馈）。有了 alias，旧 key 照样指向同一个组。
    alias: Option<String>,
    members: Vec<Member>,
}

impl Group {
    fn title(&self) -> String {
        // 取最近更新的那个副本的标题：同一组里标题可能被人改过。
        self.members
            .iter()
            .max_by_key(|m| m.updated_at())
            .map(|m| m.title())
            .unwrap_or_else(|| "(无标题)".to_string())
    }
    fn project(&self) -> String {
        self.members
            .iter()
            .max_by_key(|m| m.updated_at())
            .map(|m| m.project())
            .unwrap_or_default()
    }
    fn updated_at(&self) -> i64 {
        self.members.iter().map(|m| m.updated_at()).max().unwrap_or(0)
    }
    /// 推荐来源：内容最新且有正文的那一份。
    fn source(&self) -> Option<&Member> {
        self.members
            .iter()
            .filter(|m| m.has_body())
            .max_by(|a, b| {
                let rank = |m: &Member| match m.relation() {
                    "latest" => 2,
                    "ahead" => 1,
                    "" | "equal" | "unknown" => 1,
                    _ => 0,
                };
                (rank(a), a.updated_at()).cmp(&(rank(b), b.updated_at()))
            })
    }
}

/// 组内成员的标题簇 id（`dup_group`）完全一致时，把它当成备用 key。
///
/// 只有**全部成员都算出了同一个 `dup_group`** 才给别名：标题被改过、成员缺失等
/// 情况下不猜，宁可让旧 key 失效（界面会提示刷新），也不把两个组混成一个。
fn title_alias(members: &[Member]) -> Option<String> {
    let mut seen: Option<&str> = None;
    for m in members {
        let group = m.session.as_ref().map(|s| s.dup_group.as_str()).unwrap_or("");
        if group.is_empty() {
            return None;
        }
        match seen {
            None => seen = Some(group),
            Some(v) if v == group => {}
            Some(_) => return None,
        }
    }
    seen.map(|v| format!("dup:{v}"))
}

/// 按 key（或备用 key）找组。
///
/// 备用 key 命中多个组时**不猜**：给「有多个同名关联组，请从列表重新打开」，
/// 因为随手挑一个会把用户带到另一段对话上。
fn find_group<'a>(groups: &'a [Group], key: &str) -> Result<&'a Group, String> {
    if let Some(g) = groups.iter().find(|g| g.key == key) {
        return Ok(g);
    }
    let hits: Vec<&Group> = groups
        .iter()
        .filter(|g| g.alias.as_deref() == Some(key))
        .collect();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err("这个关联组已不存在（可能副本被删了），请刷新".to_string()),
        _ => Err("有多个同名关联组，请刷新列表后从卡片重新打开".to_string()),
    }
}

fn project_label(cwd: &str) -> String {
    if cwd.trim().is_empty() {
        return "—".to_string();
    }
    let tail = Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(cwd);
    tail.to_string()
}

/// 装配全部会话组。**列表与详情共用**，避免两处算出不同的成员集合。
fn collect() -> Result<(Vec<Group>, Vec<(String, String)>), String> {
    collect_opt(false)
}

/// [`collect`] 的带开关版本。
///
/// `force = true` ⇒ 会话列表走 [`workbuddy_source::list_sessions_forced`]，
/// **忽略体积/成员数上限真的逐行比对**（用户手动点了「强制比对」才走这条）。
fn collect_opt(force: bool) -> Result<(Vec<Group>, Vec<(String, String)>), String> {
    let all = if force {
        workbuddy_source::list_sessions_forced()?
    } else {
        workbuddy_source::list_sessions()?
    };
    let accounts = usable_accounts();
    let store = workbuddy_sessions::load_links()?;

    // (uid, cid) → 会话
    let mut index: HashMap<(String, String), WbSession> = HashMap::new();
    for s in all.iter() {
        if s.deleted || s.user_id.is_empty() {
            continue;
        }
        index.insert((s.user_id.clone(), s.id.clone()), s.clone());
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut used: HashSet<(String, String)> = HashSet::new();

    // 1) 正式关联组：这是权威关系，哪怕成员标题被改过也照认。
    for g in store.groups.iter() {
        if g.members.is_empty() {
            continue;
        }
        let members: Vec<Member> = g
            .members
            .iter()
            .map(|m| Member {
                uid: m.uid.clone(),
                label: workbuddy_accounts::label_for(&m.uid),
                session: index.get(&(m.uid.clone(), m.session_id.clone())).cloned(),
            })
            .collect();
        for m in g.members.iter() {
            used.insert((m.uid.clone(), m.session_id.clone()));
        }
        let alias = title_alias(&members);
        groups.push(Group {
            key: format!("link:{}", g.id),
            group_id: Some(g.id.clone()),
            alias,
            members,
        });
    }

    // 2) 没有关联记录、但标题相同的副本：按 `dup_group` 聚类。
    let mut by_dup: BTreeMap<String, Vec<WbSession>> = BTreeMap::new();
    for s in all.iter() {
        if s.deleted || s.user_id.is_empty() || s.dup_group.is_empty() {
            continue;
        }
        if used.contains(&(s.user_id.clone(), s.id.clone())) {
            continue;
        }
        by_dup
            .entry(s.dup_group.clone())
            .or_default()
            .push(s.clone());
    }
    for (dup, list) in by_dup {
        // 一个 uid 在同一簇里出现两次（同标题的两个不同会话）时只认最新的那个，
        // 否则界面会把「一个账号两份副本」画成两个成员。
        let mut per_uid: HashMap<String, WbSession> = HashMap::new();
        for s in list {
            let better = per_uid
                .get(&s.user_id)
                .map(|old| s.updated_at > old.updated_at)
                .unwrap_or(true);
            if better {
                per_uid.insert(s.user_id.clone(), s);
            }
        }
        if per_uid.len() < 2 {
            // 只有一份的「同标题簇」没有关联价值，跳过（避免列表被单副本淹没）。
            continue;
        }
        let mut members: Vec<Member> = per_uid
            .into_values()
            .map(|s| Member {
                uid: s.user_id.clone(),
                label: workbuddy_accounts::label_for(&s.user_id),
                session: Some(s),
            })
            .collect();
        members.sort_by(|a, b| b.updated_at().cmp(&a.updated_at()));
        groups.push(Group {
            key: format!("dup:{dup}"),
            group_id: None,
            alias: None,
            members,
        });
    }

    // 最近更新的排前面，与客户端列表观感一致。
    groups.sort_by(|a, b| b.updated_at().cmp(&a.updated_at()));
    Ok((groups, accounts))
}

/// 组状态。判据见模块头部表格。
///
/// ⚠️ 2026-10-24 用户明确要求：**「缺账号」不再单独提示**，内容一样就显示「相同」。
/// 缺的账号由「关联 / 关联新账号」去补，不该占用状态位 —— 否则列表里一半卡片都是
/// 黄字，真正需要处理的「有分歧」反而被淹掉。
fn status_of(g: &Group, _account_total: usize) -> &'static str {
    if g.members.iter().any(|m| !m.has_body()) {
        return "missing";
    }
    if g
        .members
        .iter()
        .any(|m| matches!(m.relation(), "diverged" | "ahead"))
    {
        return "diverged";
    }
    // 追溯不到摘要 ⇒ 没做过逐行比对（标题被改 / 体积超限），只能说「不知道」。
    let comparable = !g.members.is_empty() && g.members.iter().all(|m| !m.digest().is_empty());
    if !comparable {
        return "unknown";
    }
    if g.members.iter().any(|m| m.relation() == "behind") {
        return "pending";
    }
    "consistent"
}

fn status_text(status: &str) -> &'static str {
    match status {
        "consistent" => "内容一致",
        "pending" => "待同步",
        "diverged" => "有分歧",
        "missing" => "内容缺失",
        _ => "无法确认",
    }
}

fn member_payload(m: &Member) -> Value {
    json!({
        "uid": m.uid,
        "accountName": m.label,
        "sessionId": m.cid(),
        "title": m.title(),
        "project": m.project(),
        "updatedAt": m.updated_at(),
        "recordCount": m.lines(),
        "contentState": if m.cid().is_empty() { "gone" } else if m.has_body() { "ready" } else { "missing" },
        "relation": m.relation(),
        "isNewest": m.relation() == "latest",
        "digest": m.digest(),
        "reason": if m.relation() == "unknown" && !m.notes().is_empty() {
            // 「为什么没比对」由来源模块写清楚（体积超限 / 成员过多 / 缺正文）。
            m.notes()
        } else {
            match m.relation() {
                "latest" => "内容最新".to_string(),
                "equal" => "与最新副本完全相同".to_string(),
                "behind" => "落后于最新副本，可安全同步".to_string(),
                "ahead" => "比其它副本多出内容（未包含关系）".to_string(),
                "diverged" => "内容分叉，需要人工选择来源".to_string(),
                "no-body" => "正文文件不存在".to_string(),
                "unknown" => "未做逐行比对".to_string(),
                _ => String::new(),
            }
        },
    })
}

fn group_payload(g: &Group, accounts: &[(String, String)]) -> Value {
    let status = status_of(g, accounts.len());
    let present: HashSet<&str> = g.members.iter().map(|m| m.uid.as_str()).collect();
    let missing_accounts: Vec<Value> = accounts
        .iter()
        .filter(|(uid, _)| !present.contains(uid.as_str()))
        .map(|(uid, label)| json!({ "uid": uid, "label": label }))
        .collect();
    // 「待同步」只留一种原因：`behind`（有副本真的落后了、可以安全快进）。
    // 2026-10-24 用户实测反馈：两边都显示 1745 条、关系「最新/相同」，界面却说
    // 「有副本落后了」；随后又要求「缺账号不用提示，显示相同就行」——所以缺账号
    // 不再进状态位，只作为 `missingAccounts` / `missingCount` 供「关联新账号」用。
    let missing_count = missing_accounts.len();
    let behind = g.members.iter().filter(|m| m.relation() == "behind").count();
    let status_reason = if status == "pending" && behind > 0 {
        "behind"
    } else {
        ""
    };
    let src = g.source().map(|m| m.cid().to_string()).unwrap_or_default();
    json!({
        "key": g.key,
        "groupId": g.group_id,
        "title": g.title(),
        "project": g.project(),
        "updatedAt": g.updated_at(),
        "memberCount": g.members.len(),
        "accountNames": g.members.iter().map(|m| m.label.clone()).collect::<Vec<_>>(),
        "status": status,
        "statusText": status_text(status),
        "statusReason": status_reason,
        "behindCount": behind,
        "sourceSessionId": src,
        "canUnify": g.members.len() >= 2 && g.members.iter().filter(|m| m.has_body()).count() >= 1,
        "missingAccounts": missing_accounts,
        "missingCount": missing_count,
        "members": g.members.iter().map(member_payload).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// 只读接口
// ---------------------------------------------------------------------------

/// 关联会话列表。`clientRunning` 一并下发：界面据此决定「同步」按钮能不能点，
/// 而不是让人点完再看到失败。
pub fn list() -> Result<Value, String> {
    let (groups, accounts) = collect()?;
    let items: Vec<Value> = groups.iter().map(|g| group_payload(g, &accounts)).collect();
    let count = |s: &str| {
        items
            .iter()
            .filter(|v| v.get("status").and_then(|x| x.as_str()) == Some(s))
            .count()
    };
    Ok(json!({
        "ok": true,
        "accounts": accounts.iter().map(|(uid, label)| json!({ "uid": uid, "label": label })).collect::<Vec<_>>(),
        "accountTotal": accounts.len(),
        "clientRunning": workbuddy_switch::is_running(),
        "counts": {
            "all": items.len(),
            "pending": count("pending"),
            "diverged": count("diverged") + count("unknown"),
            "consistent": count("consistent"),
            "missing": count("missing"),
            "unknown": count("unknown"),
        },
        "items": items,
        "note": "只读聚合：以关联记录为准，没有关联记录的按同标题副本聚类",
    }))
}

/// 单个关联组的成员明细（与 [`list`] 同源，保证数字一致）。
pub fn detail(group_key: &str) -> Result<Value, String> {
    detail_opt(group_key, false)
}

/// [`detail`] 的**强制比对**版本：忽略体积上限，逐行读完再判「一致 / 不一致」。
///
/// ⚠️ 慢是预期：一份 227 MB 的正文要读几秒，几个副本就是十几秒。
/// 所以只在用户**手动点「强制比对」**时调用（默认路径绝不走这里）。
pub fn detail_forced(group_key: &str) -> Result<Value, String> {
    detail_opt(group_key, true)
}

fn detail_opt(group_key: &str, force: bool) -> Result<Value, String> {
    let (groups, accounts) = collect_opt(force)?;
    let g = find_group(&groups, group_key)?;
    let mut payload = group_payload(g, &accounts);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("ok".to_string(), json!(true));
        obj.insert("accountTotal".to_string(), json!(accounts.len()));
        obj.insert(
            "clientRunning".to_string(),
            json!(workbuddy_switch::is_running()),
        );
    }
    Ok(payload)
}

/// 还没纳入任何关联组的会话（供「新增关联会话」选择）。
///
/// 只列有正文、未删除的；上限 300 条，避免把整个仓库塞给下拉框。
pub fn candidates() -> Result<Value, String> {
    let (groups, accounts) = collect()?;
    let mut linked: HashSet<String> = HashSet::new();
    for g in groups.iter() {
        for m in g.members.iter() {
            if !m.cid().is_empty() {
                linked.insert(m.cid().to_string());
            }
        }
    }
    let all = workbuddy_source::list_sessions()?;
    let items: Vec<Value> = all
        .iter()
        .filter(|s| !s.deleted && !s.user_id.is_empty() && s.has_body)
        .filter(|s| !linked.contains(&s.id))
        .take(300)
        .map(|s| {
            json!({
                "sessionId": s.id,
                "uid": s.user_id,
                "accountName": workbuddy_accounts::label_for(&s.user_id),
                "title": s.title,
                "project": project_label(&s.cwd),
                "updatedAt": s.updated_at,
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "items": items,
        "accounts": accounts.iter().map(|(uid, label)| json!({ "uid": uid, "label": label })).collect::<Vec<_>>(),
        "clientRunning": workbuddy_switch::is_running(),
    }))
}

// ---------------------------------------------------------------------------
// 写入接口
// ---------------------------------------------------------------------------

fn require_closed() -> Result<(), String> {
    if workbuddy_switch::is_running() {
        return Err(
            "WorkBuddy 正在运行。请先退出客户端再同步——它开着的时候正文会被它重新写回，\
             这本工具写进去的内容会被冲掉"
                .to_string(),
        );
    }
    Ok(())
}

/// 写入前置：保证客户端不在跑。返回「是不是本工具替你关掉的」。
///
/// ⚠️ 以前只有 [`require_closed`]：客户端开着就把「请先退出」丢回给用户，界面上
/// 按钮只能跟着变灰 —— 用户看到的就是「以此为准不可用」（2026-10-24 实测反馈）。
/// 但「客户端开着不能写」这个判断本身是对的（它会把正文写回，覆盖我们写的内容），
/// 所以不能去掉；正确做法是**用户在确认框里同意后，由本工具主动退出它再写**。
///
/// `close_client = false` 时行为不变（拒绝 + 说明原因），供没有界面的调用方使用。
pub(crate) fn ensure_closed(close_client: bool) -> Result<bool, String> {
    if !workbuddy_switch::is_running() {
        return Ok(false);
    }
    if !close_client {
        return require_closed().map(|_| false);
    }
    let out = workbuddy_switch::quit_client(20_000);
    if !out.ok {
        return Err(format!(
            "已尝试退出 WorkBuddy 但没成功：{}",
            out.hint("WorkBuddy")
        ));
    }
    // 退出后再确认一次：`quit_client` 说 ok 是「等到进程没了」才成立，这里只是
    // 防一手「刚退出又被更新器拉起」的极端情况，避免写进一个正在被写的库。
    if workbuddy_switch::is_running() {
        return Err(
            "WorkBuddy 在退出后又被重新拉起来了（多半在自动更新），请等它稳定后再试".to_string(),
        );
    }
    Ok(true)
}

/// 两个文件长度是否一致（回滚时用：Windows 的 `copy` 有时「内容已回写、改属性
/// 失败」返回 Err，只靠返回值会把成功的回滚报成失败）。
fn same_len(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.len() == y.len(),
        _ => false,
    }
}

/// 一次正文写入的实测量（据实回报，不写「大概」）。
#[derive(Debug)]
struct BodyStat {
    lines: usize,
    replaced: usize,
    bytes: u64,
}

/// 按字节做全量替换，顺手数出替换了几处（纯函数，便于单测）。
fn replace_bytes(hay: &[u8], from: &[u8], to: &[u8], hits: &mut usize) -> Vec<u8> {
    if from.is_empty() {
        return hay.to_vec();
    }
    let mut out = Vec::with_capacity(hay.len() + 64);
    let mut i = 0usize;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
            *hits += 1;
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

/// FNV-1a 64：只用来核对「刚写进去的字节」和「磁盘上的字节」是否同一份。
fn fnv1a(bytes: &[u8], h: &mut u64) {
    for b in bytes {
        *h ^= *b as u64;
        *h = h.wrapping_mul(0x0100_0000_01b3);
    }
}

/// 逐行把来源正文写成目标副本：把 `from` 那个会话 id 换成 `to`，边写边算指纹，
/// 写完再回读比对一次。
///
/// ⚠️⚠️ 为什么不用「换成目标 id 再换回来，看是否逐字节等于原文」这种校验：
/// 正文里**本来就可能出现别的会话 id 字面量**（比如对话里引用过它，本机实测源
/// 正文里有 41 处 `f92b521c…`）。`A→B` 再 `B→A` 不是恒等变换 —— 换回来时那 41 处
/// 也被改成了 A，于是必然报「内容与来源不一致」、整批回滚。用户看到的就是
/// 「这个会话报错，别的会话又能正常备份」（2026-10-24 实测）。
///
/// 正确判据：**磁盘上的字节 = 来源逐行替换后的字节**。所以这里边写边算指纹，
/// 写完回读比对指纹与字节数；替换 0 处也算失败（id 对不上，不能当成功）。
fn write_body(src: &Path, dest: &Path, from: &str, to: &str) -> Result<BodyStat, String> {
    let (from_b, to_b) = (from.as_bytes(), to.as_bytes());
    let input = std::fs::File::open(src).map_err(|e| format!("打开来源正文失败: {e}"))?;
    let output = std::fs::File::create(dest).map_err(|e| format!("创建目标正文失败: {e}"))?;
    let mut reader = std::io::BufReader::with_capacity(1 << 20, input);
    let mut writer = std::io::BufWriter::with_capacity(1 << 20, output);
    let mut stat = BodyStat {
        lines: 0,
        replaced: 0,
        bytes: 0,
    };
    // 一边写一边算，避免把 200 MB 正文在内存里抄两份。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("读取来源正文失败: {e}"))?;
        if n == 0 {
            break;
        }
        // 会话 id 是 ASCII，逐行按字节替换不会切断 UTF-8 字符（id 不会跨行）。
        let out = replace_bytes(&buf, from_b, to_b, &mut stat.replaced);
        writer
            .write_all(&out)
            .map_err(|e| format!("写入目标正文失败: {e}"))?;
        fnv1a(&out, &mut hash);
        stat.bytes += out.len() as u64;
        stat.lines += 1;
    }
    writer
        .flush()
        .map_err(|e| format!("写入目标正文失败: {e}"))?;
    drop(writer);
    if stat.replaced == 0 {
        return Err("来源正文里找不到这个会话 id，未按成功处理".to_string());
    }

    let mut f = std::fs::File::open(dest).map_err(|e| format!("回读目标正文失败: {e}"))?;
    let mut back_hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut back_bytes: u64 = 0;
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut chunk)
            .map_err(|e| format!("回读目标正文失败: {e}"))?;
        if n == 0 {
            break;
        }
        fnv1a(&chunk[..n], &mut back_hash);
        back_bytes += n as u64;
    }
    if back_hash != hash || back_bytes != stat.bytes {
        return Err(format!(
            "回读校验失败：磁盘上是 {back_bytes} 字节，应为 {} 字节",
            stat.bytes
        ));
    }
    Ok(stat)
}

/// 「以此为准」：把 `source_session_id` 的正文统一到组内其它副本。
///
/// 语义是**覆盖已有副本**，不是再复制一份 —— 否则组里会越同步越多。
/// 覆盖前把每个目标正文各备一份，任何一步失败都按备份写回。
pub fn unify(
    group_key: &str,
    source_session_id: &str,
    close_client: bool,
) -> Result<Value, String> {
    let client_stopped = ensure_closed(close_client)?;
    // 外层统一收尾：无论从哪个出口返回，客户端都要被拉回来。
    with_client_restart(client_stopped, || unify_locked(group_key, source_session_id))
}

/// [`unify`] 的本体：**假定客户端已经关掉**（不再自己关，避免嵌套关两次）。
fn unify_locked(group_key: &str, source_session_id: &str) -> Result<Value, String> {
    if source_session_id.trim().is_empty() {
        return Err("没有指定要作为标准的副本".to_string());
    }
    let (groups, _accounts) = collect()?;
    let g = find_group(&groups, group_key)?;
    let src = g
        .members
        .iter()
        .find(|m| m.cid() == source_session_id)
        .ok_or_else(|| "选中的副本不属于这个关联组".to_string())?;
    if !src.has_body() {
        return Err("选中的副本没有正文文件，不能作为标准".to_string());
    }
    let src_path = workbuddy_source::find_jsonl(source_session_id)
        .ok_or_else(|| "找不到来源副本的正文文件".to_string())?;

    let targets: Vec<(&str, &str)> = g
        .members
        .iter()
        .filter(|m| m.cid() != source_session_id && !m.cid().is_empty())
        .map(|m| (m.uid.as_str(), m.cid()))
        .collect();
    if targets.is_empty() {
        return Err("这个组里没有其它副本可同步".to_string());
    }

    // 备份目录：正文与库都要能回滚。
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let backup_dir = store_dir()
        .join("workbuddy")
        .join("session-backups")
        .join(format!("unify-{stamp}"));
    std::fs::create_dir_all(&backup_dir)
        .map_err(|e| format!("创建正文备份目录失败: {e}"))?;
    let db_backup = workbuddy_sessions::backup_db("unify")?;

    let mut written: Vec<(String, std::path::PathBuf, Option<std::path::PathBuf>)> = Vec::new();
    let mut details: Vec<Value> = Vec::new();
    let result = (|| -> Result<(), String> {
        for (uid, cid) in &targets {
            let dest = workbuddy_source::find_jsonl(cid)
                .unwrap_or_else(|| workbuddy_sessions::target_body_path(&src_path, cid));
            // 备份已有正文（不存在就是新建，回滚时删掉）。
            let saved = if dest.is_file() {
                let slot = backup_dir.join(format!("{cid}.jsonl"));
                std::fs::copy(&dest, &slot)
                    .map_err(|e| format!("备份目标正文失败（{cid}）: {e}"))?;
                Some(slot)
            } else {
                None
            };
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建目标目录失败: {e}"))?;
            }
            // ⚠️ 先登记再写：写到一半失败时这个文件同样要按备份回滚。
            written.push((cid.to_string(), dest.clone(), saved));
            let stat = write_body(&src_path, &dest, source_session_id, cid)
                .map_err(|e| format!("写入副本 {cid} 失败（已回滚）：{e}"))?;
            details.push(json!({
                "uid": uid,
                "sessionId": cid,
                "lines": stat.lines,
                "replaced": stat.replaced,
                "bytes": stat.bytes,
            }));
        }
        Ok(())
    })();

    if let Err(e) = result {
        let mut restored = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for (cid, dest, saved) in written.iter().rev() {
            let ok = match saved {
                Some(slot) => {
                    // Windows 上 `copy` 可能「内容已经写回、改属性失败」而返回 Err，
                    // 所以再核一次长度，免得把回滚成功报成失败（反之亦然）。
                    std::fs::copy(slot, dest).is_ok() || same_len(slot, dest)
                }
                None => std::fs::remove_file(dest).is_ok() || !dest.exists(),
            };
            if ok {
                restored += 1;
            } else {
                failed.push(cid.clone());
            }
        }
        if failed.is_empty() {
            return Err(format!(
                "{e}（已按备份回写 {restored} 个副本，库备份在 {}）",
                db_backup.display()
            ));
        }
        return Err(format!(
            "{e}（{restored} 个副本已回写，{} 个回写失败：{}；正文备份在 {}，库备份在 {}）",
            failed.len(),
            failed.join("、"),
            backup_dir.display(),
            db_backup.display()
        ));
    }

    // ---- 回读校验：把「写进去了」变成「写进去的确实一致」 ---------------------
    //
    // 用户反馈原话：「强制对比，然后以此为准同步后，还是出现这个问题」——
    // 同步明明成功了，界面却还是「无法确认这些副本是否一致」。根因是同步完界面走的
    // 是**非强制**路径，体积超限就只会说「不知道」，于是看起来像没修好。
    //
    // 这里真把每一份读回来算摘要（不是拿「我刚写过」当结论），一致就把结论登记下来：
    // 之后的刷新、甚至重启工具，都不再退回「未比对」。
    let group_ids: Vec<String> = g
        .members
        .iter()
        .map(|m| m.cid().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    let mut verify_paths: Vec<std::path::PathBuf> = vec![src_path.clone()];
    let mut verify_ids: Vec<String> = vec![source_session_id.to_string()];
    let mut pairs: Vec<(String, std::path::PathBuf)> =
        vec![(source_session_id.to_string(), src_path.clone())];
    for (cid, dest, _saved) in &written {
        verify_paths.push(dest.clone());
        verify_ids.push(cid.clone());
        pairs.push((cid.clone(), dest.clone()));
    }
    let report = workbuddy_source::verify_same_content(&verify_paths, &group_ids);
    if report.all_same {
        // 校验不过就**不登记**：界面上能看到的永远只能是「真的核对过」的结论。
        let _ = workbuddy_source::mark_verified(&pairs, report.lines, &report.digest);
    }
    let verify_mismatch: Vec<String> = report
        .mismatched
        .iter()
        .map(|&i| verify_ids.get(i).cloned().unwrap_or_default())
        .collect();

    // 库行时间跟着走，否则客户端列表里的「更新时间」还停在旧值。
    let now = chrono::Local::now().timestamp_millis();
    let conn = rusqlite::Connection::open(workbuddy_sessions::db_path())
        .map_err(|e| format!("打开会话库失败: {e}"))?;
    for (uid, cid) in &targets {
        conn.execute(
            "UPDATE sessions SET updated_at = ?1, last_activity_at = ?1 \
             WHERE id = ?2 AND user_id = ?3",
            rusqlite::params![now, cid, uid],
        )
        .map_err(|e| format!("更新副本时间失败（{cid}）: {e}"))?;
    }
    drop(conn);

    // 把这一组登记成正式关联组：下次列表就按组显示，不再靠同标题猜。
    //
    // ⚠️ 登记后组 key 会从 `dup:<标题>` 变成 `link:<组id>`。旧 key 之所以还能用，
    // 靠的是 [`title_alias`]；界面那边则由返回的 `groupKey` 直接换到新 key。
    let mut store = workbuddy_sessions::load_links()?;
    let src_uid = src.uid.clone();
    let mut gid = String::new();
    for (uid, cid) in &targets {
        gid = workbuddy_sessions::upsert_group(&mut store, &src_uid, source_session_id, uid, cid, now);
    }
    // 组里原本就有的成员也补登记，避免「部分成员在组外」。
    for m in g.members.iter() {
        if !m.cid().is_empty() && m.uid != src_uid {
            gid = workbuddy_sessions::upsert_group(&mut store, &src_uid, source_session_id, &m.uid, m.cid(), now);
        }
    }
    workbuddy_sessions::save_links(&store)?;
    let new_key = if gid.is_empty() {
        group_key.to_string()
    } else {
        format!("link:{gid}")
    };

    Ok(json!({
        "ok": true,
        "groupKey": new_key,
        "source": { "uid": src_uid, "sessionId": source_session_id },
        "synced": details,
        "syncedCount": details.len(),
        "backupDir": backup_dir.to_string_lossy(),
        "dbBackup": db_backup.to_string_lossy(),
        // 回读校验结论：`verified = false` 时正文已写入但内容对不上，界面必须照实说。
        "verified": report.all_same,
        "verifiedLines": report.lines,
        "verifyMs": report.elapsed_ms,
        "verifyMismatch": verify_mismatch,
        "note": if report.all_same {
            "已用选中副本覆盖其它账号副本，并回读校验一致；原内容已备份，可人工恢复"
        } else {
            "已写入，但回读校验发现有不一致的副本，请人工核对；原内容已备份，可人工恢复"
        },
    }))
}

/// 给组里缺的账号补一份副本（复用「复制会话」同一条写入路径）。
pub fn link_account(
    group_key: &str,
    target_uid: &str,
    close_client: bool,
) -> Result<Value, String> {
    let client_stopped = ensure_closed(close_client)?;
    with_client_restart(client_stopped, || {
        let (groups, _accounts) = collect()?;
        let g = find_group(&groups, group_key)?;
        let src = g
            .source()
            .ok_or_else(|| "这个组里没有可用的来源副本（正文都不存在）".to_string())?;
        let src_uid = src.uid.clone();
        let src_cid = src.cid().to_string();
        let mut out = workbuddy_sessions::copy_sessions(&src_uid, target_uid, &[src_cid])?;
        with_group_key(&mut out, group_key);
        Ok(out)
    })
}

/// 把一个还没关联的会话复制给目标账号（复制后自动进同一个关联组）。
pub fn link_session(
    session_id: &str,
    target_uid: &str,
    close_client: bool,
) -> Result<Value, String> {
    if session_id.trim().is_empty() {
        return Err("没有指定要关联的会话".to_string());
    }
    if target_uid.trim().is_empty() {
        return Err("没有指定目标账号".to_string());
    }
    let client_stopped = ensure_closed(close_client)?;
    with_client_restart(client_stopped, || {
        let all = workbuddy_source::list_sessions()?;
        let src = all
            .iter()
            .find(|s| s.id == session_id && !s.deleted && !s.user_id.is_empty())
            .ok_or_else(|| "这个会话已不存在（可能被删除），请刷新".to_string())?;
        if src.user_id == target_uid {
            return Err("来源账号与目标账号相同，无需关联".to_string());
        }
        let mut out =
            workbuddy_sessions::copy_sessions(&src.user_id, target_uid, &[session_id.to_string()])?;
        with_group_key(&mut out, "");
        Ok(out)
    })
}

/// [`unlink_members`] 的存储层：就地把要移除的成员从关联记录里拿掉。
///
/// 返回「组是否被整体删除」。**单独拆出来是为了能单测** —— 真正的入口要先扫会话库
/// （本机 DB + 账号库），单测里造不出那些条件；而这段「按 uid 删成员 / 剩几个就解散 /
/// 聚类组要补记录」的判据恰恰是最容易写错的部分。
fn apply_unlink_store(
    store: &mut workbuddy_sessions::LinkStore,
    group_id: Option<&str>,
    removed: &[String],
    keep: &[(String, String)],
    now: i64,
) -> Result<bool, String> {
    let mut group_deleted = false;
    match group_id {
        // ① 正式关联组：直接删成员。
        Some(id) => {
            let Some(idx) = store.groups.iter().position(|x| x.id == id) else {
                return Err("关联记录刚被改动过，请刷新后重试".to_string());
            };
            store.groups[idx]
                .members
                .retain(|m| !removed.iter().any(|u| *u == m.uid));
            // 只剩 0/1 个成员时「组」不成立：留着会让那份副本永远被算作「已关联」，
            // 以后再也无法和别的副本聚到一起。
            if store.groups[idx].members.len() <= 1 {
                store.groups.remove(idx);
                group_deleted = true;
            }
        }
        // ② 同标题聚类组（本来没有记录）：给留下的人补一条，才能表达「已经拆开」。
        None => {
            if keep.len() >= 2 {
                store.version = workbuddy_sessions::LINK_STORE_VERSION;
                store.groups.push(workbuddy_sessions::LinkGroup {
                    id: uuid::Uuid::new_v4().to_string(),
                    created_at: now,
                    members: keep
                        .iter()
                        .map(|(uid, sid)| workbuddy_sessions::LinkMember {
                            uid: uid.clone(),
                            session_id: sid.clone(),
                            linked_at: now,
                        })
                        .collect(),
                });
            }
            // keep < 2：没有可登记的组合，什么都不写（单份副本本来就不成组）。
        }
    }
    Ok(group_deleted)
}

/// 取消关联：把指定账号从这个「同一段对话」的关联里移除。
///
/// ## 只删记录，不动会话
///
/// 这里**不写会话库、不碰正文文件**，只改 `workbuddy-session-links.json`。
/// 所以也**不需要关客户端**（其它写入接口要先退出，是因为它们要改 sqlite / jsonl）。
/// 用户点了「取消关联」之后，那个账号的会话仍然在它自己名下，只是不再被当成同一段对话。
///
/// ## 两种组的处理不一样
///
/// * **正式关联组**（`link:<id>`）：直接从记录里删成员；删到只剩 0/1 个时把整组删掉
///   —— 只留一个成员的话「组」不成立，留着会让那份副本永远被当成「已关联」，
///   以后再也无法和别的副本聚到一起。
/// * **同标题聚类组**（`dup:<标题>`）：本来就没有记录，要表达「把他们拆开」只能
///   **给留下的人补一条正式记录**。否则下次列表又按标题把它们聚回去，用户会看到
///   「点了取消关联，它还在」。
pub fn unlink_members(group_key: &str, uids: &[String]) -> Result<Value, String> {
    // 定位组（key 可能是 link:<id> / dup:<标题> / 备用别名 —— 都交给 find_group）。
    let (groups, _accounts) = collect()?;
    let g = find_group(&groups, group_key)?;
    let group_id = g.group_id.clone();
    let title = g.title();
    let members: Vec<(String, String)> = g
        .members
        .iter()
        .map(|m| (m.uid.clone(), m.cid().to_string()))
        .collect();

    // 归一化要移除的账号：去重、忽略空串。**一个都没给 = 取消全部**
    // （界面「取消全部关联」会把成员 uid 全传过来，走的是同一条路，不搞两个分支）。
    let mut want: Vec<String> = Vec::new();
    for u in uids {
        let u = u.trim();
        if !u.is_empty() && !want.iter().any(|x| x == u) {
            want.push(u.to_string());
        }
    }
    let remove_all = want.is_empty();

    let mut removed: Vec<String> = Vec::new();
    let mut keep: Vec<(String, String)> = Vec::new();
    for (uid, sid) in members.iter() {
        if remove_all || want.iter().any(|w| w == uid) {
            removed.push(uid.clone());
        } else {
            keep.push((uid.clone(), sid.clone()));
        }
    }
    if removed.is_empty() {
        return Err("这些账号本来就不在这个关联组里（请刷新后重试）".to_string());
    }

    let mut store = workbuddy_sessions::load_links()?;
    let group_deleted = apply_unlink_store(
        &mut store,
        group_id.as_deref(),
        &removed,
        &keep,
        crate::modules::config::now_ms(),
    )?;
    workbuddy_sessions::save_links(&store)?;

    Ok(json!({
        "ok": true,
        "title": title,
        "removed": removed,
        "removedCount": removed.len(),
        "remaining": keep.len(),
        "groupDeleted": group_deleted,
        "note": "只删了关联记录，会话数据一个字节都没动",
    }))
}

/// 把客户端重新拉起来。
///
/// ⚠️ **我们关的就得我们开回来**：2026-10-24 用户反馈「同步会话后 WorkBuddy 不会自动
/// 重启，要我自己再打开」。启动是异步的（Electron 起窗口要几秒），这里只确认进程被
/// 拉起，不等窗口就绪 —— 界面不该为此卡住。
///
/// 返回 `(是否已启动, 失败原因)`。没关过就不启动（不能因为一次同步就凭空多开一个客户端）。
pub(crate) fn restart_client(stopped: bool) -> (bool, Option<String>) {
    if !stopped {
        return (false, None);
    }
    match workbuddy_switch::launch() {
        Ok(_) => (true, None),
        Err(e) => (false, Some(e)),
    }
}

/// 统一收尾：跑完 `body` 之后，只要客户端是我们关的，就一定把它开回来。
///
/// ⚠️ 早期 `?` 直接返回的错误也走这里 —— 以前只有「写入成功」那条路会带
/// `clientStopped`，中途报错（组已不存在、来源找不到…）就直接返回，客户端被关在
/// 外面没人管（用户眼里就是「同步失败之后 WorkBuddy 自己没了」）。
fn with_client_restart<F>(stopped: bool, body: F) -> Result<Value, String>
where
    F: FnOnce() -> Result<Value, String>,
{
    match body() {
        Ok(mut out) => {
            finish_client(&mut out, stopped);
            Ok(out)
        }
        Err(e) => {
            let tail = client_tail_of(stopped);
            Err(format!("{e}{tail}"))
        }
    }
}

/// 失败路径专用：回滚之后把客户端拉回来，并给出可拼进报错文案的尾巴。
///
/// 空串表示「本来就没关过」—— 文案里出现「已重启」而其实没关过，和「失败」长得一样糟。
fn client_tail_of(stopped: bool) -> String {
    if !stopped {
        return String::new();
    }
    match restart_client(true) {
        (true, _) => "；WorkBuddy 已自动重新启动".to_string(),
        (false, Some(e)) => format!("；WorkBuddy 没能自动启动：{e}"),
        (false, None) => "；WorkBuddy 没有自动启动".to_string(),
    }
}

/// 把「客户端被本工具关过 / 有没有重新启动」如实带回去，并真的把它拉起来。
///
/// 界面必须拿到这两个字段：否则用户会以为客户端自己崩了，或者以为还要手动去开。
fn finish_client(out: &mut Value, stopped: bool) {
    if !stopped {
        return;
    }
    let (restarted, err) = restart_client(stopped);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("clientStopped".to_string(), json!(true));
        obj.insert("clientRestarted".to_string(), json!(restarted));
        if let Some(e) = err {
            obj.insert("restartError".to_string(), json!(e));
        }
    }
}

/// 把复制后的组 key 带回去，界面据此换掉手上那个旧 key。
///
/// `copied[].groupId` 是复制时登记的组 id；一个都没复制（目标已有副本被跳过）时
/// 退回 `fallback` —— 组没变，原 key 仍然有效。
fn with_group_key(out: &mut Value, fallback: &str) {
    let from_copy = out
        .get("copied")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find_map(|v| v.get("groupId").and_then(|g| g.as_str()))
        })
        .map(|id| format!("link:{id}"));
    let key = from_copy.unwrap_or_else(|| fallback.to_string());
    if key.is_empty() {
        return;
    }
    if let Some(obj) = out.as_object_mut() {
        obj.insert("groupKey".to_string(), json!(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(uid: &str, digest: &str, relation: &str, has_body: bool) -> Member {
        let s = WbSession {
            id: format!("s-{uid}"),
            title: "同一段对话".to_string(),
            cwd: "C:\\proj\\demo".to_string(),
            user_id: uid.to_string(),
            created_at: 1,
            updated_at: 10,
            model: String::new(),
            deleted: false,
            has_body,
            body_bytes: 100,
            body_lines: 5,
            content_digest: digest.to_string(),
            dup_group: "同一段对话".to_string(),
            is_newest: relation == "latest",
            dup_note: String::new(),
            dup_relation: relation.to_string(),
        };
        Member {
            uid: uid.to_string(),
            label: uid.to_string(),
            session: Some(s),
        }
    }

    fn group(members: Vec<Member>) -> Group {
        Group {
            key: "dup:x".to_string(),
            group_id: None,
            alias: None,
            members,
        }
    }

    #[test]
    fn id_swap_is_not_reversible_when_the_target_id_already_appeared_in_the_body() {
        // 2026-10-24 实测：源正文里有 41 处别的会话 id（对话里引用过），
        // 于是「换成目标 id 再换回来」必然不等于原文 —— 那不是内容不一致。
        let src = b"{\"sessionId\":\"aaa-id\"} mentions bbb-id here\n";
        let mut hits = 0;
        let out = replace_bytes(src, b"aaa-id", b"bbb-id", &mut hits);
        assert_eq!(hits, 1);
        assert_eq!(
            out.windows(6).filter(|w| *w == b"bbb-id").count(),
            2,
            "写出去的副本里出现两次 bbb-id：一处是 sessionId，一处是原文引用"
        );

        let mut back_hits = 0;
        let back = replace_bytes(&out, b"bbb-id", b"aaa-id", &mut back_hits);
        assert_eq!(back_hits, 2);
        assert_ne!(back, src, "换回来会把原文引用也改掉 ⇒ 旧校验必然假失败");
        // 现在的判据是「写进去的字节 = 逐行替换后的字节」，与可逆性无关。
        let mut again = 0;
        assert_eq!(replace_bytes(src, b"aaa-id", b"bbb-id", &mut again), out);
    }

    #[test]
    fn identical_copies_with_missing_accounts_say_accounts_not_behind() {
        let accounts = vec![
            ("u1".to_string(), "甲".to_string()),
            ("u2".to_string(), "乙".to_string()),
            ("u3".to_string(), "丙".to_string()),
        ];
        // 两份内容完全一样、丙没有这份会话 ⇒ 2026-10-24 用户要求：**就当「内容一致」**，
        // 不许冒黄字（缺账号不再进状态位，只留在 missingAccounts 里供「关联新账号」用）。
        let g = group(vec![
            member("u1", "aaaa", "latest", true),
            member("u2", "aaaa", "equal", true),
        ]);
        let p = group_payload(&g, &accounts);
        assert_eq!(p["status"], "consistent");
        assert_eq!(p["statusReason"], "");
        assert_eq!(p["behindCount"], 0);
        assert_eq!(p["missingAccounts"].as_array().unwrap().len(), 1);
        assert_eq!(p["missingCount"], 1);

        // 真有落后副本时才是 `behind`（这一条不能因为上面放宽而丢掉）。
        let g2 = group(vec![
            member("u1", "aaaa", "latest", true),
            member("u2", "bbbb", "behind", true),
        ]);
        let p2 = group_payload(&g2, &accounts[..2].to_vec());
        assert_eq!(p2["status"], "pending");
        assert_eq!(p2["statusReason"], "behind");
        assert_eq!(p2["behindCount"], 1);
    }

    #[test]
    fn write_body_swaps_ids_and_verifies_the_written_bytes() {
        let dir = std::env::temp_dir().join(format!("wb-unify-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.jsonl");
        let dst = dir.join("dst.jsonl");
        // 第一行里 id 出现两次（sessionId + 正文引用），第二行一次。
        let body = "{\"sessionId\":\"src-1\"} text src-1 and other-9\n{\"sessionId\":\"src-1\"}\n";
        std::fs::write(&src, body).unwrap();

        let st = write_body(&src, &dst, "src-1", "dst-2").unwrap();
        assert_eq!(st.lines, 2);
        assert_eq!(st.replaced, 3);
        assert_eq!(st.bytes, std::fs::metadata(&dst).unwrap().len());
        assert_eq!(
            std::fs::read_to_string(&dst).unwrap(),
            "{\"sessionId\":\"dst-2\"} text dst-2 and other-9\n{\"sessionId\":\"dst-2\"}\n"
        );

        // 来源里没有这个 id ⇒ 必须报错，不能「照原样写一份还说成功」。
        let e = write_body(&src, &dst, "nope-id", "dst-2").unwrap_err();
        assert!(e.contains("找不到"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_dup_key_still_opens_the_group_after_it_becomes_a_link_group() {
        // 这一条是 2026-10-24 用户实测报的 bug：点「以此为准」后组被登记成正式
        // 关联组，key 从 `dup:标题` 变成 `link:<id>`，界面手上的旧 key 立刻打不开。
        let members = vec![
            member("u1", "aaaa", "latest", true),
            member("u2", "aaaa", "equal", true),
        ];
        let alias = title_alias(&members);
        assert_eq!(alias.as_deref(), Some("dup:同一段对话"));
        let g = Group {
            key: "link:abc".to_string(),
            group_id: Some("abc".to_string()),
            alias,
            members,
        };
        assert_eq!(find_group(&[g], "dup:同一段对话").unwrap().key, "link:abc");
    }

    #[test]
    fn alias_is_dropped_when_titles_differ_and_never_guesses() {
        // 标题被改过（两个成员算不出同一个簇）时不给别名：旧 key 宁可失效也不能
        // 指到一段不确定的对话上。
        let mut a = member("u1", "aaaa", "latest", true);
        let mut b = member("u2", "aaaa", "equal", true);
        a.session.as_mut().unwrap().dup_group = "标题甲".to_string();
        b.session.as_mut().unwrap().dup_group = "标题乙".to_string();
        assert!(title_alias(&[a, b]).is_none());

        // 两个同名组抢同一个别名时同样不猜。
        let m = |u: &str| member(u, "aaaa", "equal", true);
        let g1 = Group {
            key: "link:1".to_string(),
            group_id: None,
            alias: Some("dup:同名".to_string()),
            members: vec![m("u1"), m("u2")],
        };
        let g2 = Group {
            key: "link:2".to_string(),
            group_id: None,
            alias: Some("dup:同名".to_string()),
            members: vec![m("u3"), m("u4")],
        };
        let err = match find_group(&[g1, g2], "dup:同名") {
            Err(e) => e,
            Ok(_) => panic!("同名别名不能猜一个组出来"),
        };
        assert!(err.contains("多个同名关联组"), "{err}");
    }

    #[test]
    fn identical_copies_with_all_accounts_are_consistent() {
        let g = group(vec![
            member("u1", "aaaa", "latest", true),
            member("u2", "aaaa", "equal", true),
        ]);
        assert_eq!(status_of(&g, 2), "consistent");
    }

    #[test]
    fn a_missing_account_does_not_take_over_the_status() {
        // 2026-10-24 用户要求：内容一致就是「一致」，缺的账号不占用状态位
        // （否则列表一半是黄字，真正要处理的「有分歧」被淹掉）。
        let g = group(vec![
            member("u1", "aaaa", "latest", true),
            member("u2", "aaaa", "equal", true),
        ]);
        assert_eq!(status_of(&g, 3), "consistent");
    }

    #[test]
    fn behind_is_pending_but_diverged_needs_a_human() {
        let behind = group(vec![
            member("u1", "aaaa", "latest", true),
            member("u2", "bbbb", "behind", true),
        ]);
        assert_eq!(status_of(&behind, 2), "pending");
        let diverged = group(vec![
            member("u1", "aaaa", "latest", true),
            member("u2", "bbbb", "diverged", true),
        ]);
        assert_eq!(status_of(&diverged, 2), "diverged");
    }

    #[test]
    fn missing_body_outranks_every_other_state() {
        let g = group(vec![
            member("u1", "", "no-body", false),
            member("u2", "aaaa", "latest", true),
        ]);
        assert_eq!(status_of(&g, 2), "missing");
    }

    #[test]
    fn unknown_is_never_reported_as_consistent() {
        // 没做过逐行比对（标题被改 / 体积超限）时摘要为空：只能说「不知道」。
        let g = group(vec![
            member("u1", "", "", true),
            member("u2", "", "", true),
        ]);
        assert_eq!(status_of(&g, 2), "unknown");
        assert_eq!(status_text("unknown"), "无法确认");
    }
}
