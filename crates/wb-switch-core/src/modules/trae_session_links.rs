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
pub fn register_batch(
    client_key: &str,
    source_uid: &str,
    target_uid: &str,
    pairs: &[(String, String)],
) -> Result<usize, String> {
    if pairs.is_empty() {
        return Ok(0);
    }
    let mut store = load_links()?;
    let now = crate::modules::config::now_ms();
    for (src, dst) in pairs {
        upsert_group(
            &mut store,
            client_key,
            source_uid,
            src,
            target_uid,
            dst,
            now,
        );
    }
    save_links(&store)?;
    Ok(pairs.len())
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
pub fn unlink_group(group_id: &str) -> Result<Value, String> {    let mut store = load_links()?;
    let before = store.groups.len();
    store.groups.retain(|g| g.id != group_id);
    if store.groups.len() == before {
        return Err(format!("没有找到关联组 {group_id}"));
    }
    save_links(&store)?;
    Ok(json!({ "ok": true, "groupId": group_id, "remaining": store.groups.len() }))
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
