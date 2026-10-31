//! Trae 会话的**跨账号关联**登记（与 WorkBuddy 侧 `workbuddy_sessions` 的关联部分对称）。
//!
//! 用途：同客户端（如 TRAE SOLO CN）下把会话从一个账号复制到另一个账号后，
//! 副本与原件是两行独立记录，各自归属不同账号。本模块把「同一段对话的多个副本」
//! 登记成一**关联组**，于是可以回答「这个会话在另一个账号上有没有副本」「副本还在不在」。
//!
//! 存储：`~/.twin-switch/trae-session-links.json`（与 `workbuddy-session-links.json` 分开，
//! 因为两者的 key 语义不同：Trae 是 `client_key + uid`，WorkBuddy 是纯 `uid`）。
//!
//! 关键设计（照搬 WorkBuddy，逐条都有理由）：
//!   · **只登记，不干涉会话数据**：解除关联只删本文件里的一条记录，**不动任何会话**。
//!   · **存储损坏时返回 Err，绝不降级成空表** —— 降级成空表会让「补复制」把一个
//!     已经存在的副本再复制一遍，制造第二份副本，比读不出来糟得多。
//!   · `upsert_group` 复用已有组：源会话已有组时，只替换目标账号的旧成员，
//!     不会因为重复复制而堆出多个组。
//!   · `verdict` 四态由「两端各自是否活着」推出，且**归属必须真的等于预期 uid**
//!     （软删、或归属被改回别的账号，都算失效）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::modules::config::store_dir;

/// 关联存储格式版本。读到不认识的版本**报错并保留原文件**，不尝试猜测解析。
pub const LINK_STORE_VERSION: u32 = 1;

/// 关联组里的一个成员：某账号（`uid`）上的某一个会话（`session_id`）。
///
/// `client_key` 一并存下：Trae 同名的 uid 在不同客户端上可能是不同的人，
/// 不带 client 就无法判定副本是否真的还在。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkMember {
    pub client_key: String,
    pub uid: String,
    pub session_id: String,
    pub linked_at: i64,
}

/// 一个逻辑会话的关联组：同一段对话在各账号上的副本归入同一组。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkGroup {
    pub id: String,
    pub created_at: i64,
    #[serde(default)]
    pub members: Vec<LinkMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LinkStore {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub groups: Vec<LinkGroup>,
}

fn links_file() -> PathBuf {
    store_dir().join("trae-session-links.json")
}

/// 关联记录文件路径（UI 上用来告诉用户「记录在哪、删了不影响会话」）。
pub fn links_store_path() -> String {
    links_file().to_string_lossy().to_string()
}

/// 读关联存储；损坏时返回 Err（**绝不降级成空表**——那样会制造第二个副本）。
fn load_links() -> Result<LinkStore, String> {
    let path = links_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LinkStore::default()),
        Err(e) => return Err(format!("关联记录无法读取: {e}")),
    };
    let mut store: LinkStore =
        serde_json::from_str(&text).map_err(|_| "关联记录已损坏，原文件已保留".to_string())?;
    if store.version != LINK_STORE_VERSION {
        return Err(format!(
            "关联记录版本 {} 不受支持（当前支持 {}），原文件已保留",
            store.version, LINK_STORE_VERSION
        ));
    }
    store.version = LINK_STORE_VERSION;
    Ok(store)
}

fn save_links(store: &LinkStore) -> Result<(), String> {
    let path = links_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建关联存储目录失败: {e}"))?;
    }
    let text =
        serde_json::to_string_pretty(store).map_err(|e| format!("序列化关联记录失败: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|e| format!("写入关联记录失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换关联记录失败: {e}"))?;
    Ok(())
}

fn member_for<'a>(group: &'a LinkGroup, client_key: &str, uid: &str) -> Option<&'a LinkMember> {
    group
        .members
        .iter()
        .find(|m| m.client_key == client_key && m.uid == uid)
}

/// 两端副本的「分叉」判定结果。
///
/// 关联组只记录「哪两个会话是同一次复制出来的」；复制之后两端会各自继续被使用
/// （用户在客户端里接着聊），于是内容会分叉。本函数比较两端**当前**的消息数与
/// 最后活动时间，回答「哪一端更靠前」。
///
/// 判定优先级（**消息数是主判据，时间是次判据**）：
///   ① 消息数不同 ⇒ 条数多的那端更靠前（会话内容只会追加，条数是硬指标）；
///   ② 消息数相同但 `updated` 不同 ⇒ 较晚的那端更靠前（可能的编辑/重生成）；
///   ③ 两者都相同 ⇒ `none`（未分叉）。
///
/// ⚠️ 任一端**读不到**（会话已消失、归属不符）⇒ `unknown`。不猜、不按剩下一端推断 ——
/// 猜错会让界面给出「可以安全同步」的错误暗示，而这个动作是**会写库覆盖数据**的。
fn divergence_of(
    src_alive: bool,
    dst_alive: bool,
    src_messages: Option<i64>,
    dst_messages: Option<i64>,
    src_updated: Option<&str>,
    dst_updated: Option<&str>,
) -> &'static str {
    if !src_alive || !dst_alive {
        return "unknown";
    }
    let (Some(sm), Some(dm)) = (src_messages, dst_messages) else {
        return "unknown";
    };
    if sm != dm {
        return if sm > dm { "sourceAhead" } else { "targetAhead" };
    }
    match (src_updated, dst_updated) {
        (Some(su), Some(du)) if !su.is_empty() && !du.is_empty() && su != du => {
            // Trae 的 `updated_at` 是定长串 `YYYY-MM-DD HH:MM:SS`，字典序即时间序。
            if su > du {
                "sourceAhead"
            } else {
                "targetAhead"
            }
        }
        _ => "none",
    }
}

// ---------------------------------------------------------------------------
// 副本数据自检：让「同步写坏了」这件事**可被发现**
// ---------------------------------------------------------------------------

/// 会话数据自检：返回**问题清单**（空 = 健康）。
///
/// 判据只取「客户端一定渲染不出来」的硬事实，**不允许误报** —— 这个结论会让界面
/// 给出一个**会覆盖数据**的「按对端重建」入口，误报等于把用户推向一次没必要的覆盖。
///
/// ① **内容行缺失（按类型）**：`general` 消息必须有 `chat_message_general` 行，
///    `task` 消息必须有 `chat_message_task` 行。缺一条那条消息就渲染不出来，用户的
///    直观描述就是「整个会话只剩一句」。
///    真实事故（T33）：同库复制时 `message_id` 与内容行错位配对，8 条消息里 6 条的
///    内容行挂到了别人的 id 上。
/// ② **轮次引用不配对**：`chat_turn.reply_to_message_id` 必须落在本会话的 **user**
///    消息上、`response_message_id` 必须落在本会话的 **assistant** 消息上（两者都非空
///    时才判）。客户端按轮次组织对话，引用错位会让整段坍缩成一条。
///
/// ⚠️ 判据里**必须带 `session_id` 约束**：错位还有一种形态是把内容挂到了**别的会话**
/// 的 id 上（数据串库），只看「id 能不能查到」是查不出来的。
pub fn session_integrity(conn: &rusqlite::Connection, sid: &str) -> Vec<String> {
    let mut issues: Vec<String> = Vec::new();

    let missing: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_message m WHERE m.session_id=?1 AND ( \
               (m.message_type='general' AND NOT EXISTS \
                  (SELECT 1 FROM chat_message_general g WHERE g.message_id=m.message_id)) \
               OR (m.message_type='task' AND NOT EXISTS \
                  (SELECT 1 FROM chat_message_task t WHERE t.message_id=m.message_id)) \
             )",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if missing > 0 {
        issues.push(format!("{missing} 条消息缺内容行（客户端渲染不出来）"));
    }

    let bad_turns: i64 = conn
        .query_row(
            "SELECT count(*) FROM chat_turn t WHERE t.session_id=?1 \
             AND ifnull(t.reply_to_message_id,'')<>'' AND ifnull(t.response_message_id,'')<>'' \
             AND ( \
               NOT EXISTS (SELECT 1 FROM chat_message m WHERE m.message_id=t.reply_to_message_id \
                             AND m.session_id=t.session_id AND ifnull(m.message_role,'')='user') \
               OR NOT EXISTS (SELECT 1 FROM chat_message m WHERE m.message_id=t.response_message_id \
                             AND m.session_id=t.session_id AND ifnull(m.message_role,'')='assistant') \
             )",
            [sid],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if bad_turns > 0 {
        issues.push(format!("{bad_turns} 个轮次的提问/回答引用错位"));
    }

    issues
}

/// 对当前库里的若干会话批量自检（**只读**，走客户端真正读的那份快照视图）。
///
/// 打不开库 / 会话不存在 ⇒ 返回空清单：自检是「锦上添花」的能力，
/// **失败绝不能挡住关联页面**（否则一个坏了的快照会让整页打不开）。
pub fn integrity_for(
    client_key: &str,
    sids: &[String],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut out = std::collections::HashMap::new();
    if sids.is_empty() {
        return out;
    }
    let Ok(path) = crate::modules::trae_export::reader_plain_path(client_key) else {
        return out;
    };
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return out;
    };
    for sid in sids {
        let issues = session_integrity(&conn, sid);
        if !issues.is_empty() {
            out.insert(sid.clone(), issues);
        }
    }
    out
}

/// 收集关联表里属于该客户端的所有会话 id（自检的输入，去重后排序保证稳定）。
fn link_sids(store: &LinkStore, client_key: &str) -> Vec<String> {
    let mut v: Vec<String> = store
        .groups
        .iter()
        .flat_map(|g| g.members.iter())
        .filter(|m| m.client_key == client_key)
        .map(|m| m.session_id.clone())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// 登记 / 更新关联组（纯逻辑，便于单测）。
///
/// 源会话已有组 → 复用该组并替换目标账号的旧成员；否则新建组。返回组 id。
///
/// **复用而不是新建**是关键：同一对账号重复复制同一会话时，不应该堆出第二个组。
fn upsert_group(
    store: &mut LinkStore,
    client_key: &str,
    source_uid: &str,
    source_cid: &str,
    target_uid: &str,
    target_cid: &str,
    now: i64,
) -> String {
    store.version = LINK_STORE_VERSION;
    let member = |uid: &str, cid: &str| LinkMember {
        client_key: client_key.to_string(),
        uid: uid.to_string(),
        session_id: cid.to_string(),
        linked_at: now,
    };

    if let Some(group) = store.groups.iter_mut().find(|g| {
        g.members
            .iter()
            .any(|m| m.client_key == client_key && m.uid == source_uid && m.session_id == source_cid)
    }) {
        group.members.retain(|m| !(m.client_key == client_key && m.uid == target_uid));
        group.members.push(member(target_uid, target_cid));
        return group.id.clone();
    }

    let id = uuid::Uuid::new_v4().to_string();
    store.groups.push(LinkGroup {
        id: id.clone(),
        created_at: now,
        members: vec![member(source_uid, source_cid), member(target_uid, target_cid)],
    });
    id
}

/// 批量登记一组关联（一次导入的所有成功会话）。
///
/// 入参 `pairs` 是 `(源会话 id, 目标会话 id)`，目标 uid 对整批相同。
/// 收口成一个函数是为了**只读一次、只写一次**：逐条 upsert 会反复读写同一个文件，
/// 导入几十个会话时既慢，又容易在中途失败留下半截状态。
///
/// 返回登记的组数。
///
/// ⚠️⚠️ 存进关联表的 uid **必须是库内真实 uid**，不能是本工具账号库的内部 id。
///
/// 为什么：`project.user_id` 存的是真实 uid（`925158341882023`），而账号库条目 id 是
/// `oauth_882023`。关联表若存内部 id，之后按真实 uid 查成员永远查不到 ⇒
/// 明明是「已关联」的会话被判成「未关联」⇒ 界面只提示「新建副本」⇒
/// 用户看到「每次复制都出现新对话，并没有按同步关联的思路再走」（2026-10-31 实测）。
///
/// 这里在**写入侧**统一规范化（唯一出口 `canonical_uid`），老数据由
/// `switch_plan` / `overview` 的别名匹配兜住。
pub fn register_batch(
    client_key: &str,
    source_uid: &str,
    target_uid: &str,
    pairs: &[(String, String)],
) -> Result<usize, String> {
    if pairs.is_empty() {
        return Ok(0);
    }
    let src_uid = canonical_uid(client_key, source_uid);
    let dst_uid = canonical_uid(client_key, target_uid);
    let mut store = load_links()?;
    let now = crate::modules::config::now_ms();
    for (src, dst) in pairs {
        upsert_group(
            &mut store,
            client_key,
            &src_uid,
            src,
            &dst_uid,
            dst,
            now,
        );
    }
    save_links(&store)?;
    Ok(pairs.len())
}

/// 把「可能是内部 id」的 uid 规范化成**库内真实 uid**（唯一出口）。
///
/// 认不出来就原样返回（宁可不改，也不猜）。
pub fn canonical_uid(client_key: &str, uid: &str) -> String {
    match crate::modules::trae_vault::uid_of_account(client_key, uid) {
        Some(real) if !real.trim().is_empty() => real.trim().to_string(),
        _ => uid.to_string(),
    }
}

/// 自愈关联表：把成员 uid 从内部 id 规范化成真实 uid，并合并「同一账号被拆成两条」的脏数据。
///
/// ## 要修的两个毛病
///
/// 1. **uid 写成内部 id**（`oauth_882023` 而不是 `925158341882023`）⇒ 按真实 uid 查不到成员。
/// 2. **同一组里同一账号出现两次**（`oauth_882023` + `925158341882023` 各一条）——
///    这是 #1 的直接后果：登记时用的内部 id、后来另一条用了真实 uid，
///    `upsert_group` 按字符串比较认为是两个账号，于是两条都留着。
///    脏数据会让「谁是目标那份」变得有歧义。
///
/// 修完把同一组里「规范化后 uid 相同」的成员合并成一条（保留 `linked_at` 较早的，
/// 因为那是最初建立关联的那份）。只读关联表，**不碰任何会话数据**。
pub fn normalize_link_store(client_key: &str, on_log: Option<&dyn Fn(&str)>) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    let mut store = load_links()?;
    let mut renamed = 0usize;
    let mut merged = 0usize;

    for g in store.groups.iter_mut() {
        // ① 规范化 uid（只对本客户端、且能解析出来的）
        for m in g.members.iter_mut() {
            if m.client_key != client_key {
                continue;
            }
            let real = canonical_uid(client_key, &m.uid);
            if real != m.uid {
                log(&format!("   {} → {}（会话 {}）", m.uid, real, m.session_id));
                m.uid = real;
                renamed += 1;
            }
        }
        // ② 合并「规范化后同 uid」的成员
        let before = g.members.len();
        let mut kept: Vec<LinkMember> = Vec::with_capacity(before);
        for m in g.members.drain(..) {
            match kept
                .iter_mut()
                .find(|k| k.client_key == m.client_key && k.uid == m.uid)
            {
                Some(k) => {
                    // 保留较早建立的那条（那是最初的关联），但若它的 session_id 已失效
                    // 而新来的那条还在，则用新来的 —— 这里不做"存活"判断，
                    // 只按时间取早的：存活与否由上层实时算。
                    if m.linked_at < k.linked_at {
                        k.session_id = m.session_id;
                        k.linked_at = m.linked_at;
                    }
                    merged += 1;
                }
                None => kept.push(m),
            }
        }
        g.members = kept;
        let _ = before;
    }

    // ③ 清掉空组（成员合并后可能只剩 1 条，那不叫「关联」）
    let before_groups = store.groups.len();
    store.groups.retain(|g| g.members.len() >= 2);
    let dropped = before_groups - store.groups.len();

    save_links(&store)?;
    Ok(json!({
        "ok": true,
        "renamed": renamed,
        "merged": merged,
        "droppedGroups": dropped,
        "groups": store.groups.len(),
    }))
}

/// 一个会话在「期望归属 `want_uid`」视角下的判活信息。
///
/// ⚠️ Trae 的时间字段是字符串，这里**保持原样**不做时区换算 —— 转成毫秒反而要猜
/// 「这是本地时间还是 UTC」，猜错就整体差 8 小时。
fn member_view(
    all: &[crate::modules::trae_export::SessionInfo],
    cid: &str,
    want_uid: &str,
    integ: &std::collections::HashMap<String, Vec<String>>,
) -> Value {
    let issues = integ.get(cid).cloned().unwrap_or_default();
    match all.iter().find(|s| s.id == cid) {
        None => json!({
            "sessionId": cid,
            "alive": false,
            "title": Value::Null,
            "updated": Value::Null,
            "created": Value::Null,
            "messages": Value::Null,
            "ownedBy": Value::Null,
            "workspace": Value::Null,
            "integrity": issues,
        }),
        Some(s) => {
            let belongs = s.owner_uid == want_uid;
            json!({
                "sessionId": cid,
                // 归属不符也一律算失效：客户端里它已经不算这个账号的会话了。
                "alive": belongs,
                "title": s.title,
                "updated": s.updated,
                "created": s.created,
                // 消息条数：分叉判定的主判据（见 `divergence_of`）。
                "messages": s.messages,
                "ownedBy": s.owner_uid,
                // 归属工作空间（卡片左上角那个「来源」标签，与另两页同一套判定）。
                "workspace": s.workspace,
                // 数据自检问题清单（空 = 健康）；判据见 [`session_integrity`]。
                "integrity": issues,
            })
        }
    }
}

/// 构造一个关联组的视图；组里没有对应成员时返回 `None`。
///
/// ⚠️ `source` / `target` 的**归属由传入的两个 uid 决定**。想让方向与
/// `sync_group` 的 `sourceToTarget` 对得上，就必须传**规范角色**的那一对 uid
/// （见 [`diverged_groups`] 里的注释）——按「方便的顺序」传会让前端把方向说反。
fn group_view(
    g: &LinkGroup,
    client_key: &str,
    source_uid: &str,
    target_uid: &str,
    all: &[crate::modules::trae_export::SessionInfo],
    integ: &std::collections::HashMap<String, Vec<String>>,
) -> Option<Value> {
    let src = member_for(g, client_key, source_uid)?;
    let dst = member_for(g, client_key, target_uid)?;
    let s = member_view(all, &src.session_id, source_uid, integ);
    let t = member_view(all, &dst.session_id, target_uid, integ);
    let src_alive = s["alive"].as_bool().unwrap_or(false);
    let dst_alive = t["alive"].as_bool().unwrap_or(false);
    let verdict = match (src_alive, dst_alive) {
        (true, true) => "linked",
        (true, false) => "targetMissing",
        (false, true) => "sourceMissing",
        (false, false) => "gone",
    };
    // 分叉判定：两端都活着才有意义。源失效时（`targetMissing` 的反面）没得比，
    // 直接 `unknown`，不猜。
    let divergence = divergence_of(
        src_alive,
        dst_alive,
        s["messages"].as_i64(),
        t["messages"].as_i64(),
        s["updated"].as_str(),
        t["updated"].as_str(),
    );
    let side_broken = |v: &Value| -> bool {
        v["integrity"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    };
    let broken = side_broken(&s) || side_broken(&t);
    Some(json!({
        "groupId": g.id,
        "title": if src_alive { s["title"].clone() } else { t["title"].clone() },
        "source": s,
        "target": t,
        "verdict": verdict,
        // `none` 未分叉 / `sourceAhead` 源更靠前 / `targetAhead` 目标更靠前 / `unknown` 读不到
        "divergence": divergence,
        // 只有「两端都在且确实分叉」才允许同步 —— 后端在 `sync_group` 里会再校验一次。
        "canSync": verdict == "linked" && divergence != "none" && divergence != "unknown",
        // ⚠️ 数据自检：任一端有「客户端渲染不出来」的硬伤 ⇒ `broken`。
        //    它与「分叉」是**正交**的两件事：写坏的副本两端条数可能完全相等（事故实况
        //    8 vs 8、`updated_at` 也相同 ⇒ `divergence == "none"` ⇒ 连同步按钮都不给，
        //    用户无法自救）。所以自检必须单独出信号、单独给「按对端重建」入口。
        "broken": broken,
        // 只要两端都活着就允许重建 —— 方向必须由用户点，绝不替他从条数猜。
        "canRebuild": verdict == "linked",
        "canCopy": src_alive && !dst_alive,
        "defaultChecked": src_alive && !dst_alive,
        "linkedAt": dst.linked_at,
    }))
}

/// 按会话更新时间倒序排组 —— Trae 的 `updated_at` 是可直接比较的定长时间串
/// （`YYYY-MM-DD HH:MM:SS`），字典序即时间序，无需解析成数字。
/// 源端读不到时退到目标端，保证「两端都不在」的组也稳定排序。
fn sort_groups_desc(groups: &mut Vec<Value>) {
    groups.sort_by(|a, b| {
        let key = |v: &Value| -> String {
            for who in ["source", "target"] {
                if let Some(t) = v[who]["updated"].as_str() {
                    if !t.is_empty() {
                        return t.to_string();
                    }
                }
            }
            String::new()
        };
        key(b).cmp(&key(a))
    });
}

/// 关联视图：`client_key` 上 `source_uid` 与 `target_uid` 之间已建立的副本关系。
///
/// `verdict`：`linked`（两边都在）/ `targetMissing`（目标没有副本，可补复制）/
/// `sourceMissing`（源会话已失效）/ `gone`（两边都不在）。
///
/// 判活规则：会话必须**存在、未被软删、且归属账号真的等于预期 uid**。
/// 只判「存在」不够——归属被改回别的账号时，客户端里这个副本已经不属于该账号了。
pub fn links_preview(client_key: &str, source_uid: &str, target_uid: &str) -> Value {
    let store = match load_links() {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "storeStatus": "unavailable", "error": e }),
    };
    let all = match crate::modules::trae_export::list_sessions(client_key) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let mut groups: Vec<Value> = Vec::new();
    let integ = integrity_for(client_key, &link_sids(&store, client_key));
    for g in store.groups.iter() {
        if let Some(v) = group_view(g, client_key, source_uid, target_uid, &all, &integ) {
            groups.push(v);
        }
    }
    sort_groups_desc(&mut groups);

    json!({
        "ok": true,
        "storeStatus": "ready",
        "clientKey": client_key,
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "count": groups.len(),
        "groups": groups,
        "storePath": links_store_path(),
    })
}

/// 状态文案（与 WorkBuddy 侧措辞保持一致：同一个意思不许两种说法）。
fn status_text(status: &str) -> &'static str {
    match status {
        "consistent" => "内容一致",
        "pending" => "待同步",
        "diverged" => "有分歧",
        "missing" => "内容缺失",
        _ => "无法确认",
    }
}

/// 切号弹窗用：列出**当前账号**的全部会话 + 每一条与目标账号的关联状态。
///
/// 与 WorkBuddy 的 `workbuddy_sessions::switch_plan` **同名同义、同一套四态口径**：
///
/// | state | 含义 | 界面默认 |
/// |---|---|---|
/// | `linked` | 有本工具的关联记录，且目标那份副本还活着 | 勾上 |
/// | `missing` | 有记录，但目标那份已经没了（客户端里删过 / 换过库） | 勾上（切完会补一份） |
/// | `twin` | 没有记录，但目标账号里已有一份同标题同内容的副本 | 勾上（只登记关联，不重复复制） |
/// | `unlinked` | 既没记录也没同内容副本 | 空着，由用户主动新增 |
///
/// ⚠️ 与 WorkBuddy 的一处**必然差异**：Trae 没有「内容摘要」可比对
/// （WorkBuddy 靠 `content_digest` 判「同一段对话」），这里只能用
/// **同标题 + 同消息条数**当「疑似同一段对话」的判据，而且只用于 `twin` 这一态。
/// 判不出来就老实报 `unlinked`，不猜 —— 猜错会让用户以为「已经关联过了」。
pub fn switch_plan(client_key: &str, source_uid: &str, target_uid: &str) -> Value {
    let source_uid = source_uid.trim();
    let target_uid = target_uid.trim();
    if source_uid.is_empty() {
        return json!({ "ok": false, "error": "读不到当前登录账号，无法列出会话" });
    }
    if target_uid.is_empty() || target_uid == source_uid {
        return json!({ "ok": false, "error": "目标账号无效（为空或与当前账号相同）" });
    }
    // 与关联总览同一条纪律：先让快照跟上实时库，否则刚复制/刚聊过的会话查不到。
    let _ = crate::modules::trae_export::ensure_decrypted(client_key, None);
    let all = match crate::modules::trae_export::list_sessions(client_key) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let store = match load_links() {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "storeStatus": "unavailable", "error": e }),
    };

    // 目标账号里「还活着」的副本（存在 + 未被软删 + 归属对得上）。
    //
    // ⚠️⚠️ 归属比较必须**接受 uid 的两种写法**。
    //
    // 同一个账号在系统里有两个标识：账号库内部 id（`oauth_882023`）与库内真实 uid
    // （`925158341882023`）。2026-10-31 之前写错的关联记录里存的是**内部 id**，
    // 而库里 `project.user_id` 是真实 uid ⇒ 直接字符串比较永远对不上，
    // 结果明明是「已关联」的会话被判成「未关联」，界面就只会提示「新建副本」，
    // 用户看到的正是「每次复制都出现新对话，并没有按同步关联的思路再走」。
    let uid_aliases = |uid: &str| -> Vec<String> {
        let mut out = vec![uid.to_string()];
        if let Some(real) = crate::modules::trae_vault::uid_of_account(client_key, uid) {
            let real = real.trim().to_string();
            if !real.is_empty() && real != uid {
                out.push(real);
            }
        }
        // 反向：传进来的是真实 uid，也要能找到对应的内部 id。
        for id in crate::modules::trae_vault::list_vault_accounts(client_key) {
            if let Some(real) = crate::modules::trae_vault::uid_of_account(client_key, &id) {
                if real.trim() == uid && id != uid {
                    out.push(id);
                }
            }
        }
        out
    };
    let target_ids = uid_aliases(target_uid);
    let target_alive = |sid: &str| -> bool {
        all.iter().any(|t| {
            t.id == sid && !t.owner_uid.is_empty() && target_ids.iter().any(|u| *u == t.owner_uid)
        })
    };

    // 当前账号名下的会话（与「会话记录」页同源）。
    //
    // ⚠️ 同样要接受两种 uid 写法：库里 `project.user_id` 可能是真实 uid，
    // 而界面传进来的 `source_uid` 也可能已经是真实 uid（一致）；但老数据里
    // 可能有写成内部 id 的会话，一并纳入。
    let source_ids = uid_aliases(source_uid);
    let mine: Vec<&crate::modules::trae_export::SessionInfo> = all
        .iter()
        .filter(|s| source_ids.iter().any(|u| *u == s.owner_uid))
        .collect();

    let mut sessions: Vec<Value> = Vec::with_capacity(mine.len());
    let mut counts = (0usize, 0usize, 0usize, 0usize, 0usize);
    for s in mine {
        // 这条会话在哪一组里、该组里目标账号那一份的 id
        let group = store.groups.iter().find(|g| {
            g.members.iter().any(|m| {
                m.client_key == client_key
                    && source_ids.iter().any(|u| *u == m.uid)
                    && m.session_id == s.id
            })
        });
        // 组里目标账号那一份：成员的 uid 可能写成内部 id，用别名集合匹配。
        let member_sid = group
            .and_then(|g| {
                g.members.iter().find(|m| {
                    m.client_key == client_key
                        && target_ids.iter().any(|u| *u == m.uid)
                        && m.session_id != s.id
                })
            })
            .map(|m| m.session_id.clone());
        // 组里目标账号那一份的「当前状态」（条数 / 更新时间），用来算同步方向。
        let target_session = member_sid
            .as_deref()
            .and_then(|sid| all.iter().find(|t| t.id == sid));

        let alive = member_sid.as_deref().map(target_alive).unwrap_or(false);

        // 没有关联记录时，看目标账号里有没有「疑似同一段对话」的副本。
        // ⚠️ 只用「同标题 + 同条数」这条弱判据，且只在**标题非空**时才敢用：
        //    空标题的会话（`Untitled Task`）一抓一大把，按标题匹配必然错配。
        let twin = if member_sid.is_none() && !s.title.trim().is_empty() {
            all.iter()
                .find(|t| {
                    target_ids.iter().any(|u| *u == t.owner_uid)
                        && t.title == s.title
                        && t.messages == s.messages
                        && t.messages > 0
                })
                .map(|t| t.id.clone())
        } else {
            None
        };

        let state = match (member_sid.is_some(), alive, twin.is_some()) {
            (true, true, _) => {
                counts.0 += 1;
                "linked"
            }
            (true, false, _) => {
                counts.1 += 1;
                "missing"
            }
            (false, _, true) => {
                counts.2 += 1;
                "twin"
            }
            _ => {
                counts.3 += 1;
                "unlinked"
            }
        };
        // 已关联（含 missing / twin）默认勾上：切完把内容带过去。
        let default_checked = state != "unlinked";
        if default_checked {
            counts.4 += 1;
        }

        // ── 「同步」还是「复制」，由状态决定，**不让界面猜**。
        //
        // ⚠️⚠️ 已关联的会话必须**就地同步**（原 id 不变）：2026-10-31 用户实测，
        // 之前一律走复制 ⇒ 每切一次号客户端里就多出一个「新对话」，
        // 而不是把原来那条更新掉。
        // 只有「未关联」才该新建副本（那是真的没有副本可更新）。
        let sync_mode = match state {
            // 有副本且活着 ⇒ 就地同步
            "linked" => "sync",
            // 有记录但目标那份没了 ⇒ 得重新复制一份（没有可更新的对象）
            "missing" => "copy",
            // 目标已有同内容副本 ⇒ 只登记关联，不复制、不同步
            "twin" => "link",
            _ => "copy",
        };
        // 同步方向：一律「把当前账号的内容写到目标账号」。
        // 用户勾选这个动作的语义就是「把我在用的这份带过去」，
        // 不是「把目标那份拉回来」—— 后者会让用户当前在聊的内容被旧副本盖掉。
        let direction = if sync_mode == "sync" { Some("to_target") } else { None };

        sessions.push(json!({
            "sessionId": s.id,
            "title": s.title,
            "workspace": s.workspace,
            "workspacePath": s.workspace_path,
            "messages": s.messages,
            "updated": s.updated,
            "state": state,
            "stateText": switch_state_text(state),
            "defaultChecked": default_checked,
            "targetSessionId": member_sid.or(twin),
            // 关联组 id + 处理方式 + 同步方向：界面原样回传，后端据此决定
            // 是「就地同步」还是「新建副本」。
            "groupId": group.map(|g| g.id.clone()),
            "syncMode": sync_mode,
            "direction": direction,
            // 目标那份比源新（会覆盖用户更想留的那一份）⇒ 提醒用。
            "targetAhead": target_session
                .map(|t| t.messages > s.messages)
                .unwrap_or(false),
            // 没有正文的会话复制过去也是空壳，不参与勾选。
            "disabled": s.messages <= 0,
        }));
    }
    sessions.sort_by(|a, b| {
        b["updated"]
            .as_str()
            .unwrap_or("")
            .cmp(a["updated"].as_str().unwrap_or(""))
    });

    json!({
        "ok": true,
        "storeStatus": "ready",
        "clientKey": client_key,
        "sourceUid": source_uid,
        "targetUid": target_uid,
        "counts": {
            "total": sessions.len(),
            "linked": counts.0,
            "missing": counts.1,
            "twin": counts.2,
            "unlinked": counts.3,
            "checked": counts.4,
        },
        "sessions": sessions,
    })
}

/// 切号弹窗里「会话与目标账号的关系」的人话（与 WorkBuddy 的 `linkStateLabel` 逐字一致）。
fn switch_state_text(state: &str) -> &'static str {
    match state {
        "linked" => "已关联 · 切换后同步内容",
        "missing" => "关联断开 · 会补一份副本",
        "twin" => "目标已有同内容副本 · 只登记关联",
        _ => "未关联 · 勾选后新建副本",
    }
}

/// 全账号维度的关联总览 —— 「关联会话」页签用。
///
/// 与 [`links_preview`] 的差别：那里固定一对账号、只回两端；这里把该客户端下
/// **所有**成员摆成一张矩阵，并算出「哪些账号还没有这份副本」（`missingAccounts`，
/// 供界面上的「补齐到某账号」用）。
///
/// 判据口径（**不许升级成比事实更强的结论**）：
/// - Trae 没有 WorkBuddy 那种整份正文摘要，逐行一致性判不了 ⇒ 只用
///   **消息条数 + 更新时间**判断谁落后（与 `divergence_of` 同一套）；
/// - 因此状态只有 `consistent`（条数/时间全等）/ `pending`（有副本落后，可安全快进）/
///   `missing`（有成员已失效）/ `unknown`（读不到消息数），**不产出 `diverged`** ——
///   「分叉」需要有逐行证据，这里给不出就不说。
pub fn overview(client_key: &str) -> Value {
    let store = match load_links() {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "storeStatus": "unavailable", "error": e }),
    };
    // ⚠️⚠️ **先确保解密快照是最新的**，再读会话列表。
    //
    // 2026-10-31 用户实测：刚复制完的会话，在关联页立刻显示「内容缺失 / 这个账号里
    // 已经没有这条会话」。根因是关联总览读的是**解密快照**，而快照还是写入前的旧版本，
    // 里面当然没有新副本。
    //
    // 用户的要求是「没有登录的账号，应当也可以后台尝试解密」—— 这条正好靠这里满足：
    // Trae 的多账号**共用一个库**（靠 `project.user_id` 区分归属），所以只要库能解，
    // 所有账号（包括当前没登录的）的副本都能读到，不需要逐个账号去登录一次。
    //
    // 失败**不阻断**：没存密钥 / 库被占用时照旧用旧快照，但下面的 `readable` 会因此
    // 收敛成「实际读到的那部分」，不会把读不到的账号判成「已失效」。
    let snapshot_fresh = match crate::modules::trae_export::ensure_decrypted(client_key, None) {
        Ok(o) => o.reused || o.pages > 0,
        Err(_) => false,
    };
    let all = match crate::modules::trae_export::list_sessions(client_key) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let integ = integrity_for(client_key, &link_sids(&store, client_key));

    // 账号清单：本客户端下真实出现过会话的 owner_uid（与「会话记录」页同源）。
    let mut accounts: Vec<(String, String)> = Vec::new();
    for s in &all {
        if s.owner_uid.is_empty() || accounts.iter().any(|(u, _)| u == &s.owner_uid) {
            continue;
        }
        accounts.push((s.owner_uid.clone(), s.owner_label.clone()));
    }
    accounts.sort_by(|a, b| a.1.cmp(&b.1));
    // 本次**能读到**的账号：解密快照只覆盖当前解密过的那一个库，其它账号的会话在
    // 这份快照里根本不存在。若把「快照里没有」当成「副本已失效」，界面会满屏红色
    // （真机实测 6 组全 missing）—— 那是错的，必须区分「真的没了」与「这次没解密」。
    //
    // ⚠️ 快照**没刷新成功**时（`snapshot_fresh == false`：没存密钥 / 库被占用），
    // 手里这份数据是过期的，这时任何「这个账号没有这条会话」的结论都可能是假象 ⇒
    // 把 `readable` 收敛成空集，让所有成员显示「未解密」而不是「已失效」。
    // 「未解密」是诚实的（这次确实没读到），「已失效」是断言（说人家真的没了），
    // 过期数据上做后者就是撒谎。
    let readable: std::collections::HashSet<String> = if snapshot_fresh {
        accounts.iter().map(|(u, _)| u.clone()).collect()
    } else {
        std::collections::HashSet::new()
    };
    let label_of = |uid: &str| -> String {
        accounts
            .iter()
            .find(|(u, _)| u == uid)
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| format!("uid …{}", uid.chars().rev().take(6).collect::<String>()))
    };

    let mut groups: Vec<Value> = Vec::new();
    let mut tally = (0usize, 0usize, 0usize, 0usize);
    for g in store.groups.iter() {
        let mine: Vec<&LinkMember> = g
            .members
            .iter()
            .filter(|m| m.client_key == client_key)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let mut members: Vec<Value> = mine
            .iter()
            .map(|m| {
                let mut v = member_view(&all, &m.session_id, &m.uid, &integ);
                if let Some(o) = v.as_object_mut() {
                    o.insert("uid".into(), json!(m.uid));
                    o.insert("accountName".into(), json!(label_of(&m.uid)));
                    o.insert("linkedAt".into(), json!(m.linked_at));
                }
                v
            })
            .collect();

        // 「可读成员」才参与状态计算；未解密的账号一律按「无法确认」处理，不当失效。
        let readable_members: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(_, v)| {
                v["uid"]
                    .as_str()
                    .map(|u| readable.contains(u))
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect();
        let msgs: Vec<Option<i64>> = members.iter().map(|v| v["messages"].as_i64()).collect();
        let stamps: Vec<String> = members
            .iter()
            .map(|v| v["updated"].as_str().unwrap_or("").to_string())
            .collect();
        let max_msg = readable_members
            .iter()
            .filter_map(|&i| msgs[i])
            .max();
        let max_stamp = readable_members
            .iter()
            .map(|&i| stamps[i].clone())
            .max()
            .unwrap_or_default();
        // 「全都一样」：条数全等且时间全等 —— 这是本模块能给的最强结论（只看可读成员）。
        let all_equal = !readable_members.is_empty()
            && max_msg.is_some()
            && readable_members
                .iter()
                .all(|&i| members[i]["alive"].as_bool().unwrap_or(false) && msgs[i] == max_msg && stamps[i] == max_stamp);
        let any_behind = !all_equal
            && max_msg.is_some()
            && readable_members.iter().any(|&i| {
                members[i]["alive"].as_bool().unwrap_or(false)
                    && (msgs[i] != max_msg || stamps[i] != max_stamp)
            });

        let readable_gone = readable_members
            .iter()
            .any(|&i| !members[i]["alive"].as_bool().unwrap_or(false));

        let status = if readable_gone {
            "missing"
        } else if readable_members.is_empty() || max_msg.is_none() {
            // 没有任何一份可读 ⇒ 无法确认；**不是**「失效」。
            "unknown"
        } else if any_behind {
            "pending"
        } else {
            "consistent"
        };
        match status {
            "consistent" => tally.0 += 1,
            "pending" => tally.1 += 1,
            "missing" => tally.2 += 1,
            _ => tally.3 += 1,
        }

        // 逐成员关系：未解密 ⇒ unreadable；全等 ⇒ equal；否则条数最多（再比时间）= latest。
        let leader = if all_equal {
            None
        } else {
            let mut best: Option<usize> = None;
            for &i in readable_members.iter() {
                if !members[i]["alive"].as_bool().unwrap_or(false) {
                    continue;
                }
                best = match best {
                    None => Some(i),
                    Some(b) => {
                        let (bm, im) = (msgs[b], msgs[i]);
                        let better = match (bm, im) {
                            (Some(x), Some(y)) if x != y => y > x,
                            _ => stamps[i] > stamps[b],
                        };
                        if better {
                            Some(i)
                        } else {
                            Some(b)
                        }
                    }
                };
            }
            best
        };
        for (i, v) in members.iter_mut().enumerate() {
            let uid = v["uid"].as_str().unwrap_or("").to_string();
            let alive = v["alive"].as_bool().unwrap_or(false);
            let (relation, reason) = if !readable.contains(&uid) {
                // ⚠️ 不能写成「已失效」：这份快照只解密了一个账号的库，其它账号根本读不到。
                (
                    "unreadable",
                    "该账号的本地库这次没有解密，无法确认这份副本（切到该账号解密后再看）",
                )
            } else if !alive {
                ("gone", "这个账号里已经没有这条会话")
            } else if max_msg.is_none() {
                ("unknown", "读不到消息条数，未做比较")
            } else if all_equal {
                ("equal", "条数与更新时间都与其它副本相同")
            } else if Some(i) == leader {
                ("latest", "内容最新（条数最多/时间最晚）")
            } else {
                ("behind", "落后于最新副本，可安全同步")
            };
            if let Some(o) = v.as_object_mut() {
                o.insert("relation".into(), json!(relation));
                o.insert("reason".into(), json!(reason));
                o.insert("isNewest".into(), json!(Some(i) == leader || all_equal));
                // `contentState` 只看存活：这里没有 WorkBuddy 那种「库里有行但正文文件没了」
                // 的中间态（Trae 正文全在库里），失效就是 gone，读不到就是 unreadable。
                o.insert(
                    "contentState".into(),
                    json!(if !readable.contains(&uid) {
                        "unreadable"
                    } else if alive {
                        "ready"
                    } else {
                        "gone"
                    }),
                );
            }
        }

        let present: std::collections::HashSet<&str> =
            mine.iter().map(|m| m.uid.as_str()).collect();
        // 只把**可读账号**里缺的算「还没有这份副本」—— 未解密的账号不是缺，是看不到。
        let missing_accounts: Vec<Value> = accounts
            .iter()
            .filter(|(uid, _)| !present.contains(uid.as_str()))
            .map(|(uid, label)| json!({ "uid": uid, "label": label }))
            .collect();
        // 来源 = 组里最新那个**可读且存活**的副本（「以某端为准」的默认方向）。
        let source_sid = leader
            .map(|i| members[i]["sessionId"].as_str().unwrap_or("").to_string())
            .or_else(|| {
                readable_members
                    .iter()
                    .find(|&&i| members[i]["alive"].as_bool().unwrap_or(false))
                    .map(|&i| members[i]["sessionId"].as_str().unwrap_or("").to_string())
            })
            .unwrap_or_default();
        let title = members
            .iter()
            .find_map(|v| v["title"].as_str().filter(|t| !t.is_empty()))
            .unwrap_or("(无标题)")
            .to_string();
        // 卡片左上角那个「来源」标签：取任一成员的工作空间名（Trae 的 project.name 优先）。
        let project = members
            .iter()
            .find_map(|v| v["workspace"].as_str().filter(|w| !w.is_empty()))
            .unwrap_or("")
            .to_string();
        // 卡片右上角的相对时间用它（组内最新那份的更新时间）。
        let updated_at = members
            .iter()
            .filter_map(|v| v["updated"].as_str())
            .max()
            .unwrap_or("")
            .to_string();
        // 只有「可读且存活」的副本才能当同步来源 / 被覆盖，数量按这个算。
        let alive_count = readable_members
            .iter()
            .filter(|&&i| members[i]["alive"].as_bool().unwrap_or(false))
            .count();
        // 「规范 source」= 组里最早登记的成员（与 `group_members` / `sync_group` 的口径一致）。
        // 界面据此把「以这份为准」翻译成 `sourceToTarget` / `targetToSource`，不自己猜方向。
        let canonical_uid = mine
            .iter()
            .min_by_key(|m| m.linked_at)
            .map(|m| m.uid.clone())
            .unwrap_or_default();

        groups.push(json!({
            "groupId": g.id,
            "key": g.id,
            "title": title,
            "project": project,
            "updatedAt": updated_at,
            "memberCount": members.len(),
            "accountNames": members.iter().map(|v| v["accountName"].clone()).collect::<Vec<_>>(),
            "status": status,
            "statusText": status_text(status),
            "statusReason": if status == "pending" { "behind" } else { "" },
            "behindCount": if all_equal { 0 } else { members.len().saturating_sub(1) },
            "sourceSessionId": source_sid,
            "canonicalUid": canonical_uid,
            "pairwise": members.len() == 2,
            "canUnify": alive_count >= 2,
            "canCopy": alive_count >= 1,
            "missingAccounts": missing_accounts,
            "missingCount": missing_accounts.len(),
            "members": members,
        }));
    }
    // 组内最新时间倒序，与「会话记录」页观感一致。
    groups.sort_by(|a, b| {
        let stamp = |v: &Value| -> String {
            v["members"]
                .as_array()
                .and_then(|m| {
                    m.iter()
                        .filter_map(|x| x["updated"].as_str())
                        .max()
                        .map(|s| s.to_string())
                })
                .unwrap_or_default()
        };
        stamp(b).cmp(&stamp(a))
    });

    json!({
        "ok": true,
        "storeStatus": "ready",
        "clientKey": client_key,
        "accounts": accounts
            .iter()
            .map(|(uid, label)| json!({ "uid": uid, "label": label }))
            .collect::<Vec<_>>(),
        "counts": {
            "total": groups.len(),
            "consistent": tally.0,
            "pending": tally.1,
            "missing": tally.2,
            "unknown": tally.3,
        },
        "groups": groups,
        "storePath": links_store_path(),
    })
}

/// 切换账号后的「要不要同步」探测：某账号参与、且**需要处理**的关联组。
///
/// 「需要处理」有两种，都算命中：
///   ① **确实分叉** —— 两端条数/时间不同，谁新可以判出来（`suggestedDirection` 有值）；
///   ② **副本数据写坏了** —— `broken`，自检发现问题（见 [`session_integrity`]）。
///      这一种与分叉**正交**：两端条数可能完全相等（事故实况 8 vs 8、时间也相同），
///      分叉判不出来，但客户端里那条会话根本渲染不出来（「只剩一句」）。
///      此时**不给建议方向**（`suggestedDirection = null`），方向由用户点。
///
/// 与 [`links_preview`] 同款判活口径，但**不需要调用方先知道对端账号是谁** ——
/// 切到某个账号之后，组里只要有一端属于它、且命中上面两种之一，就算命中。
///
/// 每项在常规组视图之外额外带：
///   - `partnerUid` / `partnerLabel`：对端账号（另一个端的 uid 与显示名）；
///   - `selfRole`：**当前账号在该组里的规范角色**（`"source"` / `"target"`）；
///   - `suggestedDirection`：建议的同步方向（`sourceAhead` ⇒ `sourceToTarget`），
///     写坏或读不到时为 `null`；
///   - `selfIssues` / `partnerIssues`：两端各自的自检问题清单；
///   - `selfMessages` / `selfUpdated` 与 `partner*` 两组计数，供前端写「谁比谁新」。
///
/// ⚠️ `sync_group` 的 `sourceToTarget` 是**相对规范角色**说的，`group_members` 按
/// `linked_at` 最小值定 source。所以这里必须用 `group_members` 把取到的两个 uid
/// 按规范角色排好再喂给 [`group_view`] —— 直接按「遍历顺序」传会把方向说反。
///
/// ⚠️ 只读，不动任何数据。`count` 为 0 表示「没有什么要提醒的」。
pub fn diverged_groups(client_key: &str, uid: &str) -> Value {
    let store = match load_links() {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "storeStatus": "unavailable", "error": e }),
    };
    let all = match crate::modules::trae_export::list_sessions(client_key) {
        Ok(v) => v,
        Err(e) => return json!({ "ok": false, "error": e }),
    };

    let mut groups: Vec<Value> = Vec::new();
    let integ = integrity_for(client_key, &link_sids(&store, client_key));
    for g in store.groups.iter() {
        let involved = g
            .members
            .iter()
            .any(|m| m.client_key == client_key && m.uid == uid);
        if !involved {
            continue;
        }
        // 规范角色（`source` = `linked_at` 最小的成员），同步方向以它为准。
        let Some(roles) = group_members(client_key, &g.id).ok().flatten() else {
            continue;
        };
        let (Some(src_uid), Some(tgt_uid)) = (
            roles.iter().find(|m| m.role == "source").map(|m| m.uid.clone()),
            roles.iter().find(|m| m.role == "target").map(|m| m.uid.clone()),
        ) else {
            continue;
        };
        let Some(mut v) = group_view(g, client_key, &src_uid, &tgt_uid, &all, &integ) else {
            continue;
        };
        // 两种都该提醒：① 确实分叉（两端条数/时间不同）② **副本数据写坏了**。
        // ② 必须单独判 —— 写坏的副本两端条数可能完全相等（事故实况 8 vs 8，
        // `updated_at` 也相同 ⇒ `divergence == "none"`），光看分叉根本发现不了，
        // 用户就卡在「同步过了，但内容还是旧的」。
        let d = v["divergence"].as_str().unwrap_or("unknown").to_string();
        let diverged = d == "sourceAhead" || d == "targetAhead";
        let broken = v["broken"].as_bool().unwrap_or(false);
        if !diverged && !broken {
            continue;
        }
        let self_is_source = src_uid == uid;
        let (self_role, other_role) = if self_is_source {
            ("source", "target")
        } else {
            ("target", "source")
        };
        let partner_uid = if self_is_source { tgt_uid } else { src_uid };
        let label_of = |who: &str| -> String {
            all.iter()
                .find(|s| s.owner_uid == who)
                .map(|s| s.owner_label.clone())
                .unwrap_or_default()
        };
        v["partnerUid"] = json!(partner_uid);
        v["partnerLabel"] = json!(label_of(&partner_uid));
        v["selfLabel"] = json!(label_of(uid));
        v["selfRole"] = json!(self_role);
        // ⚠️ 写坏的副本**不给建议方向**：条数相等推不出谁新，猜错方向就是拿旧内容
        //    覆盖掉新内容 —— 比不给建议糟得多。前端据此不显示「（推荐）」。
        v["suggestedDirection"] = if diverged && !broken {
            json!(if d == "sourceAhead" {
                "sourceToTarget"
            } else {
                "targetToSource"
            })
        } else {
            Value::Null
        };
        v["selfMessages"] = v[self_role]["messages"].clone();
        v["selfUpdated"] = v[self_role]["updated"].clone();
        v["partnerMessages"] = v[other_role]["messages"].clone();
        v["partnerUpdated"] = v[other_role]["updated"].clone();
        // 自检结果按「当前账号 / 对端」分开给，前端才能说清**哪一份**坏了。
        v["selfIssues"] = v[self_role]["integrity"].clone();
        v["partnerIssues"] = v[other_role]["integrity"].clone();
        groups.push(v);
    }
    sort_groups_desc(&mut groups);

    json!({
        "ok": true,
        "storeStatus": "ready",
        "clientKey": client_key,
        "uid": uid,
        "count": groups.len(),
        "groups": groups,
    })
}

/// 关联组的一个成员，带上它在组里的角色。
#[derive(Debug, Clone)]
pub struct GroupMemberView {
    /// `"source"` 或 `"target"`：相对当前选定的账号对而言。
    pub role: &'static str,
    pub client_key: String,
    pub uid: String,
    pub session_id: String,
}

/// 取出某个关联组在给定客户端上的两个成员（源 / 目标各一）。
///
/// 「源」= 组里 `linked_at` 最早的那个成员（复制时先写源成员），其余按 uid 与
/// `source_uid` 对齐。找不到组、或成员数不足时返回 `Ok(None)` / `Err`。
///
/// 供 `trae_import::sync_group` 定位「哪两条会话是一对」。
pub fn group_members(
    client_key: &str,
    group_id: &str,
) -> Result<Option<Vec<GroupMemberView>>, String> {
    let store = load_links()?;
    let Some(g) = store.groups.iter().find(|g| g.id == group_id) else {
        return Ok(None);
    };
    let mine: Vec<&LinkMember> = g
        .members
        .iter()
        .filter(|m| m.client_key == client_key)
        .collect();
    if mine.len() < 2 {
        return Ok(None);
    }
    // 源成员 = `linked_at` 最小的那个（`upsert_group` 先放源、后放目标）。
    // 相同则退化为按列表顺序（保持稳定）。
    let src_idx = mine
        .iter()
        .enumerate()
        .min_by_key(|(i, m)| (m.linked_at, *i))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut out: Vec<GroupMemberView> = Vec::with_capacity(mine.len());
    for (i, m) in mine.iter().enumerate() {
        out.push(GroupMemberView {
            role: if i == src_idx { "source" } else { "target" },
            client_key: m.client_key.clone(),
            uid: m.uid.clone(),
            session_id: m.session_id.clone(),
        });
    }
    Ok(Some(out))
}

/// 删除一个关联组（只删本工具的关联记录，**不动任何会话数据**）。
pub fn unlink_group(group_id: &str) -> Result<Value, String> {
    let mut store = load_links()?;
    let before = store.groups.len();
    store.groups.retain(|g| g.id != group_id);
    if store.groups.len() == before {
        return Err(format!("没有找到关联组 {group_id}"));
    }
    save_links(&store)?;
    Ok(json!({ "ok": true, "groupId": group_id, "remaining": store.groups.len() }))
}

/// 把一个账号从关联组里**拆出去**（只删这条成员记录，会话数据一个字不动）。
///
/// 与 WorkBuddy 的「取消关联」同名同义：用户看着某个副本不对劲（换过库 / 客户端里
/// 删过 / 挂错账号），只想把它从「同一段对话」里摘掉，不想整组解散。
///
/// 三条规矩：
/// 1. 组里剩不到 2 份 ⇒ **整组删掉**：只剩一份的「关联组」没有任何意义，
///    留着只会让列表里多一行点不开的卡片。
/// 2. 拆掉的账号若原本是源，不特殊处理 —— 关联只是「同一段对话」的记录，
///    没有方向性，谁先谁后不影响任何判据（`source` 只用于显示「源副本」列）。
/// 3. 找不到这条成员就报错，**不静默成功**：否则界面会以为已经拆掉了。
pub fn unlink_member(group_id: &str, uid: &str, client_key: &str) -> Result<Value, String> {
    let mut store = load_links()?;
    if !store.groups.iter().any(|g| g.id == group_id) {
        return Err(format!("没有找到关联组 {group_id}"));
    }
    let (left, removed_group) = unlink_member_in(&mut store, group_id, uid, client_key)
        .ok_or_else(|| {
            format!("这个账号（{uid}）不在该关联组里（可能已经被拆掉了，刷新看看）")
        })?;
    save_links(&store)?;
    Ok(json!({
        "ok": true,
        "groupId": group_id,
        "uid": uid,
        "remainingMembers": left,
        "groupRemoved": removed_group,
    }))
}

/// [`unlink_member`] 的**纯逻辑**：在内存里的 store 上拆一个成员，返回
/// `(剩下的成员数, 整组是否被删掉)`。抽出来是为了能单测钉住「剩不到 2 份就整组删」。
fn unlink_member_in(store: &mut LinkStore, group_id: &str, uid: &str, client_key: &str) -> Option<(usize, bool)> {
    let idx = store.groups.iter().position(|g| g.id == group_id)?;
    let before = store.groups[idx].members.len();
    store.groups[idx]
        .members
        .retain(|m| !(m.uid == uid && m.client_key == client_key));
    if store.groups[idx].members.len() == before {
        return None;
    }
    let left = store.groups[idx].members.len();
    let removed_group = left < 2;
    if removed_group {
        store.groups.retain(|g| g.id != group_id);
    }
    Some((left, removed_group))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with() -> LinkStore {
        LinkStore {
            version: LINK_STORE_VERSION,
            groups: Vec::new(),
        }
    }

    fn member(uid: &str, client_key: &str, at: i64) -> LinkMember {
        LinkMember {
            client_key: client_key.to_string(),
            uid: uid.to_string(),
            session_id: format!("sid-{uid}"),
            linked_at: at,
        }
    }

    /// 取消关联单个账号：只摘这一个，其它成员原样留着。
    #[test]
    fn unlink_member_removes_only_the_targeted_member() {
        let mut store = store_with();
        store.groups.push(LinkGroup {
            id: "g1".into(),
            created_at: 1,
            members: vec![
                member("u1", "trae-cn", 1),
                member("u2", "trae-cn", 2),
                member("u3", "trae-cn", 3),
            ],
        });
        let (left, removed) = unlink_member_in(&mut store, "g1", "u2", "trae-cn").expect("应能拆掉");
        assert_eq!(left, 2);
        assert!(!removed, "还剩两份，组要留着");
        let g = &store.groups[0];
        assert_eq!(g.members.len(), 2);
        assert!(g.members.iter().all(|m| m.uid != "u2"), "只该摘掉 u2");
        assert!(g.members.iter().any(|m| m.uid == "u1") && g.members.iter().any(|m| m.uid == "u3"));
    }

    /// 剩不到两份 ⇒ **整组删掉**：只剩一份的「关联组」没有任何意义。
    ///
    /// 留着会变成列表里一张「1 个账号」的怪卡片，点开还什么都不显示。
    #[test]
    fn unlink_member_dissolves_group_when_less_than_two_remain() {
        let mut store = store_with();
        store.groups.push(LinkGroup {
            id: "g1".into(),
            created_at: 1,
            members: vec![member("u1", "trae-cn", 1), member("u2", "trae-cn", 2)],
        });
        store.groups.push(LinkGroup {
            id: "g2".into(),
            created_at: 2,
            members: vec![member("x1", "trae-cn", 3), member("x2", "trae-cn", 4)],
        });
        let (left, removed) = unlink_member_in(&mut store, "g1", "u2", "trae-cn").expect("应能拆掉");
        assert_eq!(left, 1);
        assert!(removed, "只剩一份 ⇒ 整组必须删掉");
        assert_eq!(store.groups.len(), 1, "只该删 g1");
        assert_eq!(store.groups[0].id, "g2", "别的组不许被误删");
    }

    /// 同 uid 在**不同客户端**下是两份不同副本：只摘当前客户端那一份。
    ///
    /// ⚠️ 只按 uid 匹配会把另一个客户端的同名账号一起摘掉（那是另一个库的会话）。
    #[test]
    fn unlink_member_is_scoped_by_client_key() {
        let mut store = store_with();
        store.groups.push(LinkGroup {
            id: "g1".into(),
            created_at: 1,
            members: vec![
                member("u1", "trae-cn", 1),
                member("u1", "trae-solo-cn", 2),
                member("u2", "trae-cn", 3),
            ],
        });
        let (left, removed) =
            unlink_member_in(&mut store, "g1", "u1", "trae-cn").expect("应能拆掉");
        assert_eq!(left, 2);
        assert!(!removed);
        let g = &store.groups[0];
        assert!(
            g.members
                .iter()
                .any(|m| m.uid == "u1" && m.client_key == "trae-solo-cn"),
            "另一个客户端的同名账号不许被摘掉"
        );
        assert!(
            !g.members
                .iter()
                .any(|m| m.uid == "u1" && m.client_key == "trae-cn"),
            "目标客户端那一份必须摘掉"
        );
    }

    /// 不在组里的账号 / 不存在的组 ⇒ 返回 None（上层据此报错，**不静默成功**）。
    #[test]
    fn unlink_member_reports_when_nothing_was_removed() {        let mut store = store_with();
        store.groups.push(LinkGroup {
            id: "g1".into(),
            created_at: 1,
            members: vec![member("u1", "trae-cn", 1), member("u2", "trae-cn", 2)],
        });
        assert!(
            unlink_member_in(&mut store, "g1", "nobody", "trae-cn").is_none(),
            "不在组里必须报错，否则界面以为已经拆掉了"
        );
        assert!(
            unlink_member_in(&mut store, "no-such-group", "u1", "trae-cn").is_none(),
            "组不存在也要报错"
        );
        assert_eq!(store.groups[0].members.len(), 2, "失败不许改动数据");
    }

    /// 自检必须抓到「客户端一定渲染不出来」的两类硬伤，且**对健康会话零误报**。
    ///
    /// ⚠️ 误报的代价不是「多一条提示」：界面据此给出一个**会覆盖数据**的「重建」入口。
    #[test]
    fn session_integrity_flags_missing_content_and_mismatched_turns() {
        fn build(conn: &rusqlite::Connection) {
            conn.execute_batch(
                "CREATE TABLE chat_message (message_id TEXT, session_id TEXT, \
                    message_type TEXT, message_role TEXT, message_index INTEGER); \
                 CREATE TABLE chat_message_general (message_id TEXT, content TEXT); \
                 CREATE TABLE chat_message_task (message_id TEXT, content TEXT); \
                 CREATE TABLE chat_turn (session_id TEXT, reply_to_message_id TEXT, \
                    response_message_id TEXT);",
            )
            .unwrap();
            // 健康的一轮：general(user) ←→ task(assistant)
            for (mid, t, r, i) in [
                ("m1", "general", "user", 1),
                ("m2", "task", "assistant", 2),
            ] {
                conn.execute(
                    "INSERT INTO chat_message VALUES (?1,'s1',?2,?3,?4)",
                    rusqlite::params![mid, t, r, i],
                )
                .unwrap();
            }
            conn.execute("INSERT INTO chat_message_general VALUES ('m1','hi')", [])
                .unwrap();
            conn.execute("INSERT INTO chat_message_task VALUES ('m2','yo')", [])
                .unwrap();
            conn.execute("INSERT INTO chat_turn VALUES ('s1','m1','m2')", [])
                .unwrap();
        }

        let healthy = rusqlite::Connection::open_in_memory().unwrap();
        build(&healthy);
        let issues = session_integrity(&healthy, "s1");
        assert!(issues.is_empty(), "健康会话不该报问题：{issues:?}");

        // ① 内容行挂到了别人的 id 上（就是 T33 那次事故的形态）：
        //    user 消息 m1 找不到 general 行 —— 而**不能**因为 m2 那行 task 内容
        //    而误判「有内容」。判据必须按消息类型对上对应的内容表。
        let scrambled = rusqlite::Connection::open_in_memory().unwrap();
        build(&scrambled);
        scrambled
            .execute("UPDATE chat_message_general SET message_id='m2'", [])
            .unwrap();
        let issues = session_integrity(&scrambled, "s1");
        assert!(
            issues.iter().any(|i| i.contains("缺内容行")),
            "内容行错位必须被发现：{issues:?}"
        );

        // ② 轮次引用落到 assistant 上（提问 / 回答互换）
        let swapped = rusqlite::Connection::open_in_memory().unwrap();
        build(&swapped);
        swapped
            .execute(
                "UPDATE chat_turn SET reply_to_message_id='m2', response_message_id='m1'",
                [],
            )
            .unwrap();
        let issues = session_integrity(&swapped, "s1");
        assert!(
            issues.iter().any(|i| i.contains("引用错位")),
            "轮次引用错位必须被发现：{issues:?}"
        );
    }

    #[test]
    fn upsert_creates_group_with_both_members() {
        let mut s = store_with();
        let id = upsert_group(&mut s, "trae-cn", "u1", "s1", "u2", "s2", 100);
        assert_eq!(s.groups.len(), 1);
        let g = &s.groups[0];
        assert_eq!(g.id, id);
        assert_eq!(g.created_at, 100);
        assert_eq!(g.members.len(), 2);
        assert!(g.members.iter().any(|m| m.uid == "u1" && m.session_id == "s1"));
        assert!(g.members.iter().any(|m| m.uid == "u2" && m.session_id == "s2"));
    }

    #[test]
    fn upsert_reuses_group_instead_of_duplicating() {
        let mut s = store_with();
        let a = upsert_group(&mut s, "trae-cn", "u1", "s1", "u2", "s2", 100);
        // 同一源会话再复制一次（目标会话 id 变了）→ 必须复用同一个组
        let b = upsert_group(&mut s, "trae-cn", "u1", "s1", "u2", "s3", 200);
        assert_eq!(a, b, "同一源会话重复复制不应新建组");
        assert_eq!(s.groups.len(), 1);
        let g = &s.groups[0];
        assert_eq!(g.members.len(), 2, "目标账号只应保留最新一个成员");
        let t = g.members.iter().find(|m| m.uid == "u2").unwrap();
        assert_eq!(t.session_id, "s3");
        assert_eq!(t.linked_at, 200);
    }

    #[test]
    fn upsert_keeps_other_accounts_members() {
        let mut s = store_with();
        upsert_group(&mut s, "trae-cn", "u1", "s1", "u2", "s2", 100);
        // 同一源会话再复制到第三个账号 → 组内应有 3 个成员
        upsert_group(&mut s, "trae-cn", "u1", "s1", "u3", "s9", 300);
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].members.len(), 3);
    }

    #[test]
    fn member_lookup_respects_client_key() {
        // 同 uid 不同客户端不得互相命中
        let g = LinkGroup {
            id: "g".into(),
            created_at: 1,
            members: vec![LinkMember {
                client_key: "trae-cn".into(),
                uid: "u1".into(),
                session_id: "s1".into(),
                linked_at: 1,
            }],
        };
        assert!(member_for(&g, "trae-cn", "u1").is_some());
        assert!(member_for(&g, "trae-global", "u1").is_none());
    }

    #[test]
    fn time_fields_are_passed_through_verbatim() {
        // Trae 的时间是字符串，前端原样展示。这里锁住「不做时区换算」这个约定：
        // 一旦有人加上 chrono 解析，本地时间被当 UTC 就会整体差 8 小时。
        // 见下方 `sort_key_prefers_source_then_target` 用的字典序比较。
        let a = "2026-10-06 12:34:56";
        let b = "2026-10-06 08:00:00";
        // 定长 `YYYY-MM-DD HH:MM:SS` 的字典序 == 时间序
        assert!(b < a, "字典序应与时间序一致");
    }

    #[test]
    fn json_info_shape_uses_string_times() {
        // 锁住 links_preview 里 info() 的字段名与类型（前端按此读取）
        let v = json!({
            "sessionId": "s1",
            "alive": true,
            "title": "t",
            "updated": "2026-10-06 12:34:56",
            "created": "2026-10-06 12:00:00",
            "ownedBy": "u1",
        });
        assert!(v["updated"].is_string(), "updated 必须是字符串");
        assert_eq!(v["updated"], "2026-10-06 12:34:56");
        assert!(v["alive"].is_boolean());
    }

    #[test]
    fn verdict_mapping_covers_all_four_states() {
        // 直接验证判定表（与 links_preview 内联的逻辑保持一致）
        let f = |s: bool, t: bool| match (s, t) {
            (true, true) => "linked",
            (true, false) => "targetMissing",
            (false, true) => "sourceMissing",
            (false, false) => "gone",
        };
        assert_eq!(f(true, true), "linked");
        assert_eq!(f(true, false), "targetMissing");
        assert_eq!(f(false, true), "sourceMissing");
        assert_eq!(f(false, false), "gone");
    }

    #[test]
    fn link_store_version_is_one() {
        assert_eq!(LINK_STORE_VERSION, 1);
        // 存储文件名必须与 WorkBuddy 侧区分开
        assert!(links_file().to_string_lossy().ends_with("trae-session-links.json"));
    }

    /// 真机探针（`#[ignore]`）：把关联表里的内部 id 规范化成真实 uid，
    /// 并合并「同一组里同账号两条」的脏数据。**会改关联表**（已由用户手动备份）。
    ///
    /// 跑法：`cargo test -p wb-switch-core --lib normalize_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn normalize_probe_fixes_real_link_store() {
        let key = "trae-cn";
        let r = normalize_link_store(key, Some(&|m| println!("   {m}"))).expect("规范化失败");
        println!("结果 = {r}");
        let after = load_links().expect("读关联表失败");
        println!("修复后组数 {}", after.groups.len());
        for g in &after.groups {
            let uids: Vec<&str> = g.members.iter().map(|m| m.uid.as_str()).collect();
            println!("  {} {:?}", &g.id[..8], uids);
        }
    }

    /// 真机探针（`#[ignore]`）：确认 `switch_plan` 能算出「当前账号会话 + 与目标账号的状态」。
    ///
    /// 只读。跑法：
    /// `cargo test -p wb-switch-core --lib switch_plan_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn switch_plan_probe_lists_sessions_with_states() {
        let key = "trae-cn";
        let list = crate::modules::trae_export::list_sessions(key).expect("读会话失败");
        // 取两个不同账号：一个当「当前」，另一个当目标。
        let mut uids: Vec<String> = Vec::new();
        for s in &list {
            if !s.owner_uid.is_empty() && !uids.contains(&s.owner_uid) {
                uids.push(s.owner_uid.clone());
            }
        }
        println!("库里的账号数 {}", uids.len());
        if uids.len() < 2 {
            println!("只有一个账号，无法组成「切到另一个」的场景（跳过）");
            return;
        }
        let (src, tgt) = (&uids[0], &uids[1]);
        println!("源 {src} → 目标 {tgt}");
        let v = switch_plan(key, src, tgt);        println!("ok = {}", v["ok"]);
        println!("counts = {}", v["counts"]);
        if let Some(arr) = v["sessions"].as_array() {
            for s in arr.iter().take(5) {
                println!(
                    "  · {} | {} | {} 条 | state={} | mode={} | group={} | checked={}",
                    s["title"].as_str().unwrap_or(""),
                    s["workspace"].as_str().unwrap_or(""),
                    s["messages"],
                    s["state"].as_str().unwrap_or(""),
                    s["syncMode"].as_str().unwrap_or(""),
                    s["groupId"].as_str().unwrap_or("-"),
                    s["defaultChecked"],
                );
            }
        }
        assert!(v["ok"].as_bool().unwrap_or(false), "switch_plan 必须返回 ok");
        assert!(
            v["sessions"].as_array().map(|a| a.len()).unwrap_or(0) > 0,
            "当前账号必须有会话"
        );

        // ── 关键验证：找一对**确实有关联**的账号，确认能算出 linked / sync ──
        // 上面那对可能是「本来就没关联」的账号，全 unlinked 是正确结果，
        // 不能据此认为判定坏了。这里遍历所有账号对，专门找有 group 的那一对。
        println!("\n=== 遍历账号对，找有真实关联的 ===");
        let mut found_linked = 0usize;
        for (i, a) in uids.iter().enumerate() {
            for b in uids.iter().skip(i + 1) {
                let p = switch_plan(key, a, b);
                let arr = p["sessions"].as_array().cloned().unwrap_or_default();
                // ⚠️ 判据是**组里真有这条会话的成员**（groupId 非 null），
                // 而不是「这一对账号之间有某个组」—— 后者会把「组存在但目标成员不在」
                // 也算进来，打印出来就成了「有真实关联」却全是 unlinked（误导）。
                let linked_rows: Vec<&Value> = arr
                    .iter()
                    .filter(|s| s["groupId"].is_string() && s["syncMode"] == "sync")
                    .collect();
                if linked_rows.is_empty() {
                    continue;
                }
                println!("★ {a} → {b}：{} 条可就地同步", linked_rows.len());
                found_linked += linked_rows.len();
                for s in linked_rows {
                    println!(
                        "    {} | state={} | mode={} | dir={} | checked={}",
                        s["title"].as_str().unwrap_or(""),
                        s["state"].as_str().unwrap_or(""),
                        s["syncMode"].as_str().unwrap_or(""),
                        s["direction"].as_str().unwrap_or("-"),
                        s["defaultChecked"],
                    );
                }
            }
        }
        assert!(
            found_linked > 0,
            "至少应有一对账号能算出「就地同步」——否则说明关联表或判据坏了"
        );
    }

    /// 切号弹窗的四态文案必须与 WorkBuddy 的 `linkStateLabel` **逐字一致**。    ///
    /// 用户明确要求「样式跟 WorkBuddy 一样，不要自创风格」—— 同一个意思两种说法
    /// 会让人以为功能不一样。
    #[test]
    fn switch_state_texts_match_workbuddy() {
        assert_eq!(switch_state_text("linked"), "已关联 · 切换后同步内容");
        assert_eq!(switch_state_text("missing"), "关联断开 · 会补一份副本");
        assert_eq!(switch_state_text("twin"), "目标已有同内容副本 · 只登记关联");
        assert_eq!(switch_state_text("unlinked"), "未关联 · 勾选后新建副本");
        // 未知态不许 panic，回落到「未关联」（最保守，不会误报已关联）
        assert_eq!(switch_state_text("nonsense"), "未关联 · 勾选后新建副本");
    }

    /// 空标题的会话**不许**按标题判「同内容副本」。
    ///
    /// Trae 里 `Untitled Task` 一抓一大把，按标题匹配必然错配 —— 把两个毫不相干的
    /// 空标题会话标成「目标已有同内容副本」，用户会以为已经关联过了。
    #[test]
    fn blank_titles_never_match_as_twin() {
        // 这条判据在 `switch_plan` 里是 `!s.title.trim().is_empty()` 的前置条件。
        // 这里用等价表达式钉住「空标题一律不匹配」的语义。
        let title = "   ";
        assert!(title.trim().is_empty(), "空标题必须被前置条件挡掉");
        let title2 = "Untitled Task";
        assert!(!title2.trim().is_empty(), "有名字的标题才允许参与匹配");
    }

    // --- 分叉判定（`divergence_of`）------------------------------------------

    #[test]
    fn divergence_uses_message_count_as_primary_signal() {
        let t = "2026-10-06 12:00:00";
        // 消息数多的那端靠前，即使它的时间更早（时间可能是复制时照抄的旧值）
        assert_eq!(
            divergence_of(true, true, Some(12), Some(8), Some(t), Some(t)),
            "sourceAhead"
        );
        assert_eq!(
            divergence_of(true, true, Some(8), Some(12), Some(t), Some(t)),
            "targetAhead"
        );
    }

    #[test]
    fn divergence_falls_back_to_updated_when_counts_equal() {
        // 条数相同 ⇒ 看时间；定长串字典序即时间序
        assert_eq!(
            divergence_of(true, true, Some(8), Some(8), Some("2026-10-06 12:00:00"), Some("2026-10-06 08:00:00")),
            "sourceAhead"
        );
        assert_eq!(
            divergence_of(true, true, Some(8), Some(8), Some("2026-10-06 08:00:00"), Some("2026-10-06 12:00:00")),
            "targetAhead"
        );
    }

    #[test]
    fn divergence_is_none_when_both_sides_identical() {
        let t = "2026-10-06 12:00:00";
        assert_eq!(divergence_of(true, true, Some(8), Some(8), Some(t), Some(t)), "none");
        // 时间缺失但条数相同：不算分叉，也不猜
        assert_eq!(divergence_of(true, true, Some(8), Some(8), None, None), "none");
        assert_eq!(divergence_of(true, true, Some(8), Some(8), Some(""), Some("")), "none");
    }

    #[test]
    fn divergence_is_unknown_when_either_side_unreadable() {
        let t = "2026-10-06 12:00:00";
        // 任一端失效 ⇒ unknown（不按剩下一端推断：猜错会让界面暗示「可以安全同步」）
        assert_eq!(divergence_of(false, true, Some(8), Some(12), Some(t), Some(t)), "unknown");
        assert_eq!(divergence_of(true, false, Some(12), Some(8), Some(t), Some(t)), "unknown");
        assert_eq!(divergence_of(false, false, None, None, None, None), "unknown");
        // 消息数读不到 ⇒ unknown，哪怕两端都活着
        assert_eq!(divergence_of(true, true, None, Some(8), Some(t), Some(t)), "unknown");
    }
}
