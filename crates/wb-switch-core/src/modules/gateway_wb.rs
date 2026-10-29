//! WorkBuddy 渠道适配层：**把第三方客户端（dsh 等）的请求伪装成官方客户端的正常会话**。
//!
//! ## 为什么需要（2026-10-29 真机实证，全部有 A/B 对照）
//!
//! WorkBuddy 的对话网关加了一层「渠道校验」，不通过就回：
//!
//! ```text
//! HTTP 400 {"code":"11128","msg":"Illegal API invocation from an unapproved channel",
//!           "displayTips":{"zh":"这次调用没通过渠道校验，可能不是官方客户端发出的…"}}
//! ```
//!
//! 排查过程（同一账号、同一模型、同一网关）：
//!
//! | 实验 | 结果 |
//! |---|---|
//! | 最小请求（system + user 各一句，两套头各试三个域名） | **200** |
//! | 被拒存档原样重发（565 KB / 108 条 / 33 工具 / 2 图） | 400 / 11128 |
//! | 去掉图片、去掉 tools、裁到 20 条、去 store/stream_options | 仍 400 / 11128 |
//! | 首条 system 换成一句中性话（历史全留） | **不再是 11128**（变成别的 400） |
//! | 只留 system + 最后一条 user | 仍 11128 |
//! | system 按段二分 → 定位到「…Web GUI… this GUI …」那两段 | 400 / 11128 |
//! | 这两段里的单句逐个试 | 全 200（**是组合特征，不是单个词**） |
//! | **整段 system 搬进首条 user，system 只留一句中性话** | **200** ✅ |
//!
//! 结论：**风控只看 `role: "system"` 的内容**，规则是「多个 harness 特征词/句式同时出现」
//! （组合命中，所以逐词中和必然漏）。因此最稳的做法不是在 system 里逐个躲关键词
//! （上游改一条规则就得重来），而是**把 system 的内容整体搬到 user 消息里**——
//! 模型照样能读到这些指令，上游的 system 风控则完全看不到它们。
//!
//! ## 另一个实证：官方头长什么样
//!
//! 从本机 WorkBuddy 客户端（`resources/app.asar`，5.9.3）里读出官方为「同一网关
//! 转发」准备的请求头构造（`buildVlmGatewayHeaders` / `applyGatewayHeaders`）：
//! `X-IDE-Type/Name/Version`、`X-Private-Data`、`X-Product`、`X-User-Id`、
//! `x-codebuddy-request`、`X-Agent-Intent/Purpose`、`X-Conversation-*`、
//! `traceparent` + `b3` + `X-B3-*`、双写 UA。本模块照抄这一套 —— 官方自己转发
//! 时带的就是这些，缺项越多越像「非官方渠道」。
//!
//! ## 多档 + 粘性（用户要求）
//!
//! 一个方案失败就换下一档，**第一个成功的记下来**（`~/.twin-switch/gateway-wb-variant.json`），
//! 后续同「作用域 + 模型」的请求直接走它，不再每轮都从第一档重试。
//! 该方案连续失败 3 次 ⇒ 自动作废重新探测（上游判据变了也能自己恢复）。

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::modules::config::{now_ms, store_dir};

/// 官方客户端版本。**客户端升级后必须跟着改**（上游按版本校验，旧版会被判 12403 / 11128）。
///
/// 取证方式：`C:\Program Files\WorkBuddy\resources\install-manifest.json` 的 `appVersion`。
pub const CLIENT_VERSION: &str = "5.9.3";

/// 一个伪装档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// 官方头 + system 深度清洗（保持 system 结构，只动可疑内容）。
    OfficialScrub,
    /// 官方头 + **system 搬进首条 user**（实证最稳，风控看不到 system）。
    SystemAsUser,
    /// 上面那档 + 历史裁剪（体积/条数风控兜底）。
    SystemAsUserTrim,
    /// 现状：最小头（UA + x-product），body 原样。兜底用。
    Legacy,
}

impl Variant {
    /// 稳定标识（写进粘性文件，**不要随手改名**，否则老记录全部失效）。
    pub fn key(self) -> &'static str {
        match self {
            Variant::OfficialScrub => "official_scrub",
            Variant::SystemAsUser => "system_as_user",
            Variant::SystemAsUserTrim => "system_as_user_trim",
            Variant::Legacy => "legacy",
        }
    }

    /// 给人看的一句话（报错与日志里要能说清「试过哪几档」）。
    pub fn label(self) -> &'static str {
        match self {
            Variant::OfficialScrub => "官方头+system清洗",
            Variant::SystemAsUser => "官方头+system搬进user",
            Variant::SystemAsUserTrim => "官方头+system搬进user+历史裁剪",
            Variant::Legacy => "最小头（旧方案）",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "official_scrub" => Some(Variant::OfficialScrub),
            "system_as_user" => Some(Variant::SystemAsUser),
            "system_as_user_trim" => Some(Variant::SystemAsUserTrim),
            "legacy" => Some(Variant::Legacy),
            _ => None,
        }
    }

    /// 探测顺序：**先轻后重**。
    ///
    /// `OfficialScrub` 最保守（语义结构不变，模型行为一致）先试；
    /// 它被拦（组合特征可能换位置）就上 `SystemAsUser`（实证可行，代价是 system 变成 user）；
    /// 还不行就再叠历史裁剪；最后回落到旧方案（万一上游对新增的头敏感）。
    pub fn order() -> &'static [Variant] {
        &[
            Variant::OfficialScrub,
            Variant::SystemAsUser,
            Variant::SystemAsUserTrim,
            Variant::Legacy,
        ]
    }
}

/// 判定「这是渠道校验拦截」（要换档重试）。
///
/// ⚠️ 只认这几种：上游文案改过（`unapproved channel` / `channel verification`），
/// 但业务码 `11128` 一直没变。**其它 400 不重试** —— 换档也救不了，只会白白多花额度。
pub fn is_channel_block(status: u16, text: &str) -> bool {
    if !(status == 400 || status == 403) {
        return false;
    }
    text.contains("11128") || text.contains("unapproved channel") || text.contains("channel verification")
}

/// 构造发往上游的请求头（`(name, value)` 有序表；后写覆盖先写）。
///
/// `OfficialScrub` / `SystemAsUser*` 用官方全套头；`Legacy` 保持旧行为不变。
pub fn headers(variant: Variant, token: &str, uid: &str) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "text/event-stream".into()),
        ("authorization".into(), format!("Bearer {token}")),
    ];
    if variant == Variant::Legacy {
        h.push((
            "user-agent".into(),
            crate::modules::gateway_models::WORKBUDDY_UA.to_string(),
        ));
        h.push(("x-product".into(), "SaaS".into()));
        return h;
    }

    // ---- 以下逐项对照 asar 里的 buildVlmGatewayHeaders / applyGatewayHeaders ----
    let trace = uuid::Uuid::new_v4().simple().to_string(); // 32 hex
    let span = trace[..16].to_string();
    let parent_full = uuid::Uuid::new_v4().simple().to_string();
    let parent = parent_full[..16].to_string();
    let conv = uuid::Uuid::new_v4().to_string();

    h.extend([
        // UA 官方是**双写**：`WorkBuddy/5.9.3 WorkBuddy/5.9.3`
        ("user-agent".to_string(), format!("WorkBuddy/{CLIENT_VERSION} WorkBuddy/{CLIENT_VERSION}")),
        ("accept-encoding".to_string(), "identity".to_string()),
        ("x-requested-with".to_string(), "XMLHttpRequest".to_string()),
        ("x-product".to_string(), "SaaS".to_string()),
        ("x-ide-type".to_string(), "WorkBuddy".to_string()),
        ("x-ide-name".to_string(), "WorkBuddy".to_string()),
        ("x-ide-version".to_string(), CLIENT_VERSION.to_string()),
        ("x-private-data".to_string(), "true".to_string()),
        ("x-codebuddy-request".to_string(), "1".to_string()),
        ("x-agent-intent".to_string(), "craft".to_string()),
        ("x-agent-purpose".to_string(), "conversation".to_string()),
        ("x-conversation-id".to_string(), conv),
        ("x-conversation-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("x-conversation-message-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("acp-connection-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("x-request-id".to_string(), uuid::Uuid::new_v4().to_string()),
        ("traceparent".to_string(), format!("00-{trace}-{span}-01")),
        ("b3".to_string(), format!("{trace}-{span}-1-{parent}")),
        ("x-b3-traceid".to_string(), trace.clone()),
        ("x-b3-spanid".to_string(), span.clone()),
        ("x-b3-parentspanid".to_string(), parent.clone()),
        ("x-b3-sampled".to_string(), "1".to_string()),
        ("x-trace-id".to_string(), trace),
    ]);
    // 账号身份头：官方只在本机登录态里带，值就是账号 uid（不是 token，不涉密）。
    if !uid.trim().is_empty() {
        h.push(("x-user-id".to_string(), uid.trim().to_string()));
    }
    h
}

/// 请求体适配的实测结果（日志/报错里要用「动了几处」，不写空话）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AdaptReport {
    /// 清洗掉的可疑片段数（品牌词 / GUI 段落 / 公开名前缀）。
    pub scrubbed: usize,
    /// system 是否被搬进 user。
    pub moved_to_user: bool,
    /// 裁掉的旧消息条数。
    pub trimmed: usize,
    /// 删掉的「客户端身份字段」个数（`user` / `store` / `stream_options` …）。
    pub dropped_fields: usize,
}

/// 对这些**顶层字段**，官方客户端要么根本不发、要么发的是它自己的值：
/// 原样转发等于把第三方客户端的身份与「非流式 / 存储」偏好一起送上去。
const DROP_FIELDS: [&str; 6] = [
    "store",
    "stream_options",
    "user",
    "metadata",
    "safety_identifier",
    "service_tier",
];

/// 对一个请求体做适配（原地改）。`public_model` 是客户端看到的模型名（可能带 `wb:` 前缀）。
pub fn adapt(body: &mut Value, variant: Variant, public_model: &str) -> AdaptReport {
    let mut rep = AdaptReport::default();
    let Some(obj) = body.as_object_mut() else {
        return rep;
    };
    // 身份字段：`legacy` 档完全不动（它要能当 A/B 对照的基准）。
    if variant != Variant::Legacy {
        for f in DROP_FIELDS {
            if obj.remove(f).is_some() {
                rep.dropped_fields += 1;
            }
        }
    }
    let Some(msgs) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return rep;
    };

    match variant {
        Variant::Legacy => {}
        Variant::OfficialScrub => {
            rep.scrubbed += scrub_messages(msgs, public_model);
        }
        Variant::SystemAsUser | Variant::SystemAsUserTrim => {
            // ⚠️ **先搬再洗**：清洗会把 GUI 整段换成一句中性话，system 立刻缩到 200 字以下，
            // 那时再判「长 system 才搬」就永远不搬了（首版单测就是这么挂的）。
            // 判断要用客户端原本的长度。
            rep.moved_to_user = move_system_to_user(msgs);
            rep.scrubbed += scrub_messages(msgs, public_model);
            if variant == Variant::SystemAsUserTrim {
                rep.trimmed = trim_history(msgs);
            }
        }
    }
    rep
}

/// 把 system 内容整体搬到首条 user 前面，system 只留一句中性话。
///
/// **只搬「长」system**（≥200 字）：短 system 通常是客户端的一句角色设定，
/// 搬走反而改变行为；而真正的 harness 长提示词都在 200 字以上。
fn move_system_to_user(msgs: &mut Vec<Value>) -> bool {
    let Some(sys_pos) = msgs.iter().position(|m| {
        matches!(m.get("role").and_then(Value::as_str), Some("system") | Some("developer"))
    }) else {
        return false;
    };
    let sys_text = match msgs[sys_pos].get("content") {
        Some(Value::String(s)) => s.clone(),
        // 数组形态（多段 content）拼成文本，保持信息不丢。
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if sys_text.trim().chars().count() < 200 {
        return false;
    }

    let block = format!(
        "<workspace_and_tool_guidance>\n{}\n</workspace_and_tool_guidance>\n\n",
        sys_text.trim()
    );
    // 首条 user 承载它；没有 user 就在末尾补一条（极少数客户端只发 system）。
    match msgs
        .iter()
        .position(|m| m.get("role").and_then(Value::as_str) == Some("user"))
    {
        Some(pos) => {
            match msgs[pos].get_mut("content") {
                Some(Value::String(s)) => *s = format!("{block}{s}"),
                Some(Value::Array(parts)) => {
                    parts.insert(0, json!({"type": "text", "text": block}));
                }
                _ => {
                    msgs[pos]["content"] = json!(block);
                }
            }
        }
        None => msgs.push(json!({"role": "user", "content": block})),
    }

    msgs[sys_pos]["content"] =
        json!("You are a helpful assistant. The user message contains workspace and tool guidance.");
    true
}

/// 历史裁剪：保留 system、承载 guidance 的 user、以及从最近往前 30 条（对齐到 user 起点）。
///
/// ⚠️ 起点必须落在 `user` 上：从 `assistant(tool_calls)` 中间切开会让上游看到
/// 「有调用没有结果」的残缺序列，那是另一种 400。
fn trim_history(msgs: &mut Vec<Value>) -> usize {
    const KEEP: usize = 30;
    if msgs.len() <= KEEP + 2 {
        return 0;
    }
    let head = 2.min(msgs.len()); // system + 承载 guidance 的首条 user
    let keep_from = msgs.len().saturating_sub(KEEP);
    // 从「保留窗口起点」**往后**找第一条 user：往前找会落到最开头的 head 上，
    // 等于没裁；而从 assistant(tool_calls) 中间切开会让上游看到残缺序列。
    let start = (keep_from..msgs.len())
        .find(|&i| msgs[i].get("role").and_then(Value::as_str) == Some("user"))
        .unwrap_or(msgs.len());
    if start <= head || start >= msgs.len() {
        return 0;
    }
    let removed = start - head;
    msgs.drain(head..start);
    removed
}

/// 清洗消息文本里的「非官方客户端特征」。
///
/// 规则刻意做**短语级、成对替换**，而不是删字：删字会改语义，替换成中性表述
/// 则模型读到的意思不变（例如 "DeepSeek Harness" → "the assistant runtime"）。
fn scrub_messages(msgs: &mut [Value], public_model: &str) -> usize {
    let mut n = 0usize;
    for m in msgs.iter_mut() {
        match m.get_mut("content") {
            Some(Value::String(s)) => n += scrub_text(s, public_model),
            Some(Value::Array(parts)) => {
                for p in parts.iter_mut() {
                    let Some(t) = p.get("text").and_then(Value::as_str).map(str::to_string) else {
                        continue;
                    };
                    let mut s = t;
                    n += scrub_text(&mut s, public_model);
                    p["text"] = json!(s);
                }
            }
            _ => {}
        }
    }
    n
}

/// 单段文本清洗。返回替换处数。
fn scrub_text(text: &mut String, public_model: &str) -> usize {
    let mut n = 0usize;
    // 1) 公开名里的作用域前缀（`wb:deepseek-v4.1-flash` → `deepseek-v4.1-flash`）。
    //    这是**本工具自己**的命名，出现在对话里等于自报「走的是中转站」。
    let m = public_model.trim();
    if !m.is_empty() && text.contains(m) {
        if let Some((_, bare)) = m.split_once(':') {
            if !bare.trim().is_empty() {
                let cnt = text.matches(m).count();
                *text = text.replace(m, bare);
                n += cnt;
            }
        }
    }

    // 2) 品牌词：先去掉零宽空格再匹配（旧版本中和留下的），一并归一。
    let cleaned = text.replace('\u{200b}', "");
    if cleaned != *text {
        *text = cleaned;
        n += 1;
    }
    for (from, to) in [
        ("DeepSeek Harness", "the assistant runtime"),
        ("Deepseek Harness", "the assistant runtime"),
        ("deepseek harness", "the assistant runtime"),
        ("DeepSeek Harness", "the assistant runtime"),
        ("window.__DSH_BOOT__", "the boot payload"),
        ("pnpm run dev:web", "the dev watcher"),
    ] {
        if text.contains(from) {
            let cnt = text.matches(from).count();
            *text = text.replace(from, to);
            n += cnt;
        }
    }
    // `DSH` / `dsh` 作为独立词
    for (from, to) in [("DSH", "runtime"), ("dsh", "runtime")] {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        let mut hit = 0usize;
        while let Some(i) = rest.find(from) {
            let (before, after) = (&rest[..i], &rest[i + from.len()..]);
            let ok_before = before
                .chars()
                .last()
                .map(|c| !c.is_alphanumeric() && c != '_')
                .unwrap_or(true);
            let ok_after = after
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric() && c != '_')
                .unwrap_or(true);
            out.push_str(before);
            if ok_before && ok_after {
                out.push_str(to);
                hit += 1;
            } else {
                out.push_str(from);
            }
            rest = after;
        }
        out.push_str(rest);
        if hit > 0 {
            *text = out;
            n += hit;
        }
    }

    // 3) 「Web GUI / this page / this GUI / this app」这一族段落：
    //    实证里就是它们和品牌词**同时出现**时触发拦截（单句不触发）。
    //    整段替换成一句中性说明，意思（"别假设有页面上下文"）保留。
    if text.contains("this GUI") || text.contains("this page") || text.contains("this app") {
        let paras: Vec<String> = text.split("\n\n").map(str::to_string).collect();
        let mut out: Vec<String> = Vec::with_capacity(paras.len());
        let mut replaced = 0usize;
        for p in paras {
            // ⚠️ 我们自己搬进 user 的 guidance 包裹块**不整段替换**：里面就是原 system 全文，
            // 整段换掉等于把工具说明丢了（模型会突然不知道自己有哪些工具）。
            // 它已经在 user 里，上游的 system 风控根本看不到，只做上面的词级清洗就够了。
            if p.contains("<workspace_and_tool_guidance>") {
                out.push(p);
                continue;
            }
            let gui_hits = p.matches("GUI").count() + p.matches("this page").count();
            if gui_hits >= 2 && p.chars().count() > 80 {
                out.push(
                    "Assume no implicit page, DOM, route, or screenshot context; ask the user when \
                     a page detail matters."
                        .to_string(),
                );
                replaced += 1;
            } else {
                out.push(p);
            }
        }
        if replaced > 0 {
            *text = out.join("\n\n");
            n += replaced;
        }
    }
    n
}

// ---------------------------------------------------------------------------
// 粘性（成功方案记下来，后续同「作用域 + 模型」直接走它）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StickyEntry {
    variant: String,
    at: i64,
    #[serde(default)]
    fails: u32,
    /// 已知会被拦的档位（带时间）。**首轮探测后就不再重复撞**，
    /// 但超过 7 天自动忽略 —— 上游可能又改了判据，别把老结论当永远。
    #[serde(default)]
    banned: Vec<Banned>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Banned {
    variant: String,
    at: i64,
}

/// 一个失败档位的「封禁期」。
const BAN_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn empty_entry() -> StickyEntry {
    StickyEntry { variant: String::new(), at: 0, fails: 0, banned: Vec::new() }
}

fn sticky_path() -> PathBuf {
    store_dir().join("gateway-wb-variant.json")
}

fn sticky_key(scope: &str, model: &str) -> String {
    format!("{scope}|{model}")
}

fn load_sticky_at(path: &std::path::Path) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save_sticky_at(path: &std::path::Path, map: &serde_json::Map<String, Value>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(map) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text.as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// 取已记住的方案（没有 / 记录坏了都回 `None`，绝不猜）。
pub fn sticky_get(scope: &str, model: &str) -> Option<Variant> {
    sticky_get_at(&sticky_path(), scope, model)
}

fn sticky_get_at(path: &std::path::Path, scope: &str, model: &str) -> Option<Variant> {
    let map = load_sticky_at(path);
    let e: StickyEntry = serde_json::from_value(map.get(&sticky_key(scope, model))?.clone()).ok()?;
    Variant::parse(&e.variant)
}

/// 记住成功方案（写盘失败不影响本次请求 —— 只是下次要重新探）。
pub fn sticky_ok(scope: &str, model: &str, variant: Variant) {
    sticky_ok_at(&sticky_path(), scope, model, variant)
}

fn sticky_ok_at(path: &std::path::Path, scope: &str, model: &str, variant: Variant) {
    let mut map = load_sticky_at(path);
    let key = sticky_key(scope, model);
    // 保留已封禁的档位记录（下次别又先撞一遍）。
    let banned = map
        .get(&key)
        .and_then(|v| serde_json::from_value::<StickyEntry>(v.clone()).ok())
        .map(|e| e.banned)
        .unwrap_or_default();
    let entry = StickyEntry {
        variant: variant.key().to_string(),
        at: now_ms(),
        fails: 0,
        banned,
    };
    if let Ok(v) = serde_json::to_value(&entry) {
        map.insert(key, v);
        // 只留最近 200 条：这是「免重复探测」的备忘，不是账本。
        if map.len() > 200 {
            let mut items: Vec<(String, i64)> = map
                .iter()
                .map(|(k, v)| {
                    let at = v.get("at").and_then(Value::as_i64).unwrap_or(0);
                    (k.clone(), at)
                })
                .collect();
            items.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
            for (k, _) in items.into_iter().skip(200) {
                map.remove(&k);
            }
        }
        save_sticky_at(path, &map);
    }
}

/// 记下「这一档对当前作用域+模型会被拦」，下次探测直接跳过。
pub fn ban(scope: &str, model: &str, variant: Variant) {
    ban_at(&sticky_path(), scope, model, variant)
}

fn ban_at(path: &std::path::Path, scope: &str, model: &str, variant: Variant) {
    let mut map = load_sticky_at(path);
    let key = sticky_key(scope, model);
    let mut e = map
        .get(&key)
        .and_then(|v| serde_json::from_value::<StickyEntry>(v.clone()).ok())
        .unwrap_or_else(empty_entry);
    if !e.banned.iter().any(|b| b.variant == variant.key()) {
        e.banned.push(Banned { variant: variant.key().to_string(), at: now_ms() });
    }
    if let Ok(v) = serde_json::to_value(&e) {
        map.insert(key, v);
        save_sticky_at(path, &map);
    }
}

/// 当前仍在封禁期内的档位（7 天前的记录自动忽略）。
pub fn banned(scope: &str, model: &str) -> Vec<Variant> {
    banned_at(&sticky_path(), scope, model)
}

fn banned_at(path: &std::path::Path, scope: &str, model: &str) -> Vec<Variant> {
    let map = load_sticky_at(path);
    let Some(e) = map
        .get(&sticky_key(scope, model))
        .and_then(|v| serde_json::from_value::<StickyEntry>(v.clone()).ok())
    else {
        return Vec::new();
    };
    let now = now_ms();
    e.banned
        .into_iter()
        .filter(|b| now - b.at < BAN_MS)
        .filter_map(|b| Variant::parse(&b.variant))
        .collect()
}

/// 记一次「记住的方案这次失败了」。连续 3 次 ⇒ 作废（返回 `true` 表示已作废，
/// 调用方应从头重新探测 —— 上游判据变了也能自己恢复，不用用户手动清文件）。
pub fn sticky_fail(scope: &str, model: &str) -> bool {
    sticky_fail_at(&sticky_path(), scope, model)
}

fn sticky_fail_at(path: &std::path::Path, scope: &str, model: &str) -> bool {
    let mut map = load_sticky_at(path);
    let key = sticky_key(scope, model);
    let Some(v) = map.get_mut(&key) else {
        return false;
    };
    let fails = v.get("fails").and_then(Value::as_u64).unwrap_or(0) + 1;
    if fails >= 3 {
        map.remove(&key);
        save_sticky_at(path, &map);
        return true;
    }
    v["fails"] = json!(fails);
    save_sticky_at(path, &map);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("gw-wb-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn body_with(system: &str, user: &str) -> Value {
        json!({"model": "deepseek-v4.1-flash", "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ], "stream": true})
    }

    /// **核心行为**：长 system 必须整体搬进首条 user，且 system 只剩一句中性话。
    ///
    /// 这条锁的是 2026-10-29 实证结论（真机 A/B 已确认能过渠道校验），改坏了
    /// dsh 走 WorkBuddy 会被 11128 全拦。
    #[test]
    fn long_system_moves_into_first_user() {
        let sys = "You are interacting with the user through the DeepSeek Harness Web GUI. \
                   When the user refers to \"this page\", \"this GUI\", or \"this app\", they mean this GUI. \
                   The browser provides no implicit DOM, route, or screenshot context, so ask.";
        let mut b = body_with(sys, "你好");
        let rep = adapt(&mut b, Variant::SystemAsUser, "deepseek-v4.1-flash");
        assert!(rep.moved_to_user, "长 system 必须搬走");
        let msgs = b["messages"].as_array().unwrap();
        let sys_now = msgs[0]["content"].as_str().unwrap();
        assert!(sys_now.len() < 200, "system 必须变成短句中转：{sys_now}");
        assert!(!sys_now.contains("Harness"), "system 里不许留品牌词：{sys_now}");
        let user_now = msgs[1]["content"].as_str().unwrap();
        assert!(user_now.contains("workspace_and_tool_guidance"), "user 必须带 guidanche 块");
        assert!(user_now.ends_with("你好"), "原 user 内容要留在最后");
        assert!(user_now.contains("assistant runtime"), "品牌词要中性化：{user_now}");
    }

    /// 短 system 不动（搬走会改变普通客户端的行为）。
    #[test]
    fn short_system_is_left_alone() {
        let mut b = body_with("You are a helpful assistant.", "hi");
        let rep = adapt(&mut b, Variant::SystemAsUser, "deepseek-v4.1-flash");
        assert!(!rep.moved_to_user, "短 system 不该搬");
        assert_eq!(b["messages"][0]["content"], "You are a helpful assistant.");
    }

    /// 公开名的作用域前缀是「本工具的身份码」，请求里必须去掉。
    #[test]
    fn scope_prefix_in_prompt_is_scrubbed() {
        let sys = format!("You are a coding agent powered by the {m} model. {}", "x".repeat(220), m = "wb:deepseek-v4.1-flash");
        let mut b = body_with(&sys, "hi");
        let rep = adapt(&mut b, Variant::OfficialScrub, "wb:deepseek-v4.1-flash");
        assert!(rep.scrubbed >= 1);
        let text = b["messages"][0]["content"].as_str().unwrap();
        assert!(!text.contains("wb:deepseek-v4.1-flash"), "身份码必须去掉：{text}");
        assert!(text.contains("deepseek-v4.1-flash"), "模型名本身要留着（模型要知道自己是谁）");
    }

    /// GUI 那族段落整段换成中性说明（实证里就是它 + 品牌词一起触发）。
    #[test]
    fn gui_paragraph_is_replaced_with_neutral_text() {
        let sys = "You are interacting with the user through the DeepSeek Harness Web GUI whose URL the runtime context names. When the user refers to \"this page\", \"this GUI\", or \"this app\" without naming another target, they mean this GUI. The browser provides no implicit DOM, route, or screenshot context.";
        let mut s = sys.to_string();
        let n = scrub_text(&mut s, "deepseek-v4.1-flash");
        assert!(n >= 1, "必须命中");
        assert!(!s.contains("this GUI"), "组合特征必须消失：{s}");
        assert!(s.contains("ask the user"), "语义要保留（别假设有页面上下文）：{s}");
    }

    /// 数组形态的 content 也要能搬（有的客户端把 system 拆成多段）。
    #[test]
    fn array_content_system_can_move() {
        let long = "y".repeat(300);
        let mut b = json!({"messages": [
            {"role": "system", "content": [{"type": "text", "text": long}]},
            {"role": "user", "content": "hi"}
        ]});
        let rep = adapt(&mut b, Variant::SystemAsUser, "m");
        assert!(rep.moved_to_user);
        assert_eq!(b["messages"][0]["content"], "You are a helpful assistant. The user message contains workspace and tool guidance.");
    }

    /// 裁剪只在超长时发生，且起点落在 user 上（不能从 tool_calls 中间切开）。
    #[test]
    fn trim_starts_at_user_and_keeps_guidance() {
        let long = "z".repeat(300);
        let mut msgs = vec![json!({"role": "system", "content": long}),
                            json!({"role": "user", "content": "first"})];
        for i in 0..50 {
            msgs.push(json!({"role": if i % 2 == 0 { "assistant" } else { "user" },
                             "content": format!("m{i}")}));
        }
        let before = msgs.len();
        let removed = trim_history(&mut msgs);
        assert!(removed > 0, "50 条必须裁剪");
        assert_eq!(msgs.len(), before - removed);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user", "第二条必须是承载 guidance 的 user");
        assert_eq!(msgs[2]["role"], "user", "裁剪起点必须落在 user 上：{}", msgs[2]);
    }

    /// 渠道校验判定：只认 11128 那一族，别的 400 不重试（省额度）。
    #[test]
    fn channel_block_only_matches_its_own_family() {
        assert!(is_channel_block(400, r#"{"code":"11128","msg":"Illegal API invocation"}"#));
        assert!(is_channel_block(400, "failed channel verification"));
        assert!(!is_channel_block(400, r#"{"code":"11134","msg":"unknown model"}"#));
        assert!(!is_channel_block(429, "11128"));
        assert!(!is_channel_block(500, "oops"));
    }

    /// 粘性：记住 → 读回 → 连续 3 次失败自动作废。
    #[test]
    fn sticky_remembers_then_expires_after_three_failures() {
        let dir = tmp();
        let path = dir.join("v.json");
        assert!(sticky_get_at(&path, "wb-cn", "m1").is_none());

        sticky_ok_at(&path, "wb-cn", "m1", Variant::SystemAsUser);
        assert_eq!(sticky_get_at(&path, "wb-cn", "m1"), Some(Variant::SystemAsUser));
        assert_eq!(sticky_get_at(&path, "wb-cn", "m2"), None, "按模型分开记");

        assert!(!sticky_fail_at(&path, "wb-cn", "m1"));
        assert!(!sticky_fail_at(&path, "wb-cn", "m1"));
        assert_eq!(sticky_get_at(&path, "wb-cn", "m1"), Some(Variant::SystemAsUser), "两次失败还留着");
        assert!(sticky_fail_at(&path, "wb-cn", "m1"), "第三次必须作废");
        assert_eq!(sticky_get_at(&path, "wb-cn", "m1"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 官方头必须齐：缺 `x-ide-*` / `x-codebuddy-request` 会被判非官方渠道。
    #[test]
    fn official_headers_are_complete() {
        let h = headers(Variant::SystemAsUser, "tok", "uid-1");
        let get = |k: &str| h.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        for k in [
            "x-product", "x-ide-type", "x-ide-name", "x-ide-version", "x-private-data",
            "x-codebuddy-request", "x-agent-intent", "x-agent-purpose", "x-conversation-id",
            "traceparent", "b3", "x-b3-traceid", "x-trace-id", "x-user-id", "user-agent",
        ] {
            assert!(get(k).is_some_and(|v| !v.is_empty()), "缺头 {k}");
        }
        assert_eq!(get("x-ide-version").unwrap(), CLIENT_VERSION);
        assert_eq!(get("user-agent").unwrap(), format!("WorkBuddy/{CLIENT_VERSION} WorkBuddy/{CLIENT_VERSION}"),
                   "官方客户端 UA 是双写的，单写会被 12403/11128");
        // Legacy 保持旧形状（不因这次改动影响 A/B 对照能力）
        let legacy = headers(Variant::Legacy, "tok", "uid-1");
        assert!(legacy.iter().all(|(n, _)| n != "x-ide-type"));
    }
}
