//! 真流式透传 + 前缀稳定 + 会话亲和。
//!
//! 三件事在这里合起来做，因为它们解决的是同一个病：**观感慢**。
//!
//! 1. **真流式**（原 `sse_from_completion` 是「整段生成完再一次性包成 SSE」）：
//!    上游来一点就往下发一点。首字延迟从「整段生成时间」降到「首字时间」，
//!    客户端也不会因为长时间收不到增量而误判卡住、重试、再排一次队。
//! 2. **前缀稳定**：`tools` 按名字排序后再发、历史工具结果按确定性规则折叠。
//!    多轮 agent 的输入 90% 是重复前缀，前缀字节一致才可能命中上游的前缀缓存
//!    （文献数据：120k 上下文冷/热 TTFT 差 3.0～5.6 倍）。
//! 3. **会话亲和**：`conversation_id` / `session_id` 不再每次 `Uuid::new_v4()`，
//!    改成从「system + 工具名 + 首条 user + 模型 + uid」的指纹派生 ——
//!    同一段对话的每一轮复用同一组 ID，上游才认得出「还是那个会话」。
//!
//! ⚠️ 三家上游的流格式都不一样，解析器分开写：
//! - Trae：私有事件流（`event: output` / `data:{"response":…}`，工具调用在 `tool_calls`）；
//! - Qoder：信封 SSE（`data:{"body":"<内层 JSON 字符串>","statusCodeValue":200}`）；
//! - WorkBuddy：本来就是标准 OpenAI SSE ⇒ 直接透传字节，不解析（少一层出错的机会）。

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use crate::modules::gateway_metrics::Trace;
use crate::modules::gateway_server::ToolCallAcc;

type BoxStream = Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// 上游流式协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Trae,
    Qoder,
}

impl Kind {
    /// 「已经吐过内容之后」允许多久没有新数据。
    ///
    /// ⚠️ 这一档**必须按上游分**：
    /// - **Qoder**：流答完不关连接，只能靠空闲判结束 ⇒ 8 秒（短一点，收尾快）。
    /// - **Trae**：有自己的结束事件（`turn_completion` / `done`），不需要靠空闲判断结束。
    ///   它在大上下文 + 工具规划时会**长时间不吐字**，之前把 Qoder 的 8 秒套在它身上，
    ///   等于「模型一思考就被我们掐断」——用户看到的就是「Trae 模型一直超时」
    ///   （2026-11-03 用户实测：8 秒空闲超时，客户端那边报 stream idle timeout）。
    ///   120 秒只是**兜底**，正常情况下由结束事件收尾。
    pub fn idle_after_start_secs(&self) -> u64 {
        match self {
            Kind::Qoder => 8,
            Kind::Trae => 120,
        }
    }
}

/// 统一增量。三家解析完都落在这几个形态上。
#[derive(Debug, Clone)]
pub enum Delta {
    Content(String),
    /// 思考过程。**不拼进回答**，按 `reasoning_content` 单独发（客户端爱显示就显示）。
    Reasoning(String),
    /// 原样的工具调用分片 —— 由 [`Emitter`] 按 index 累积。
    ToolCall(Value),
    Finish(String),
    Usage(Value),
}

// ---------------------------------------------------------------------------
// 前缀稳定 + 历史折叠
// ---------------------------------------------------------------------------

/// 历史折叠的触发门槛：`messages` 序列化后超过这个字符数才折叠。
///
/// 定在 50k 是按真机数据定的：2026-10-24 那次「1 小时没跑完」的请求体共 133k 字符，
/// 其中工具 schema 59.6k、messages 约 73k。门槛若设 80k，**这个已经出问题的请求
/// 一条都不会被折**，等于没做。50k 以下的小请求完全不碰。
const FOLD_TRIGGER_CHARS: usize = 50_000;

/// 折叠门槛的**运行时覆盖**（长上下文场景必须能关）。
///
/// 为什么必须可配：要跑 1M 上下文（Trae / WorkBuddy 的超长窗口模型）时，
/// 50k 字符就折老工具结果，等于把用户花钱买的 1M 窗口自己扔掉一大半。
///
/// - `TWIN_TS_FOLD=0`（或 `off`）：完全不折叠；
/// - `TWIN_TS_FOLD_TRIGGER=800000`：只调门槛，保留折叠能力（推荐，比一刀关掉安全）；
/// - 都不设：行为与以前完全一致（50k）。
fn fold_trigger_chars() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        if matches!(
            std::env::var("TWIN_TS_FOLD").ok().as_deref(),
            Some("0") | Some("off") | Some("false")
        ) {
            return usize::MAX;
        }
        std::env::var("TWIN_TS_FOLD_TRIGGER")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(FOLD_TRIGGER_CHARS)
    })
}
/// 最近这么多条工具结果**保持原样** —— 正在干活的那几步不能被截断。
const FOLD_KEEP_TOOLS: usize = 8;
/// 折叠后的保留字数。
const FOLD_HEAD_CHARS: usize = 1200;

/// [`prepare`] 的统计结果（进指标日志）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Prep {
    pub msgs: usize,
    pub tools: usize,
    pub chars: usize,
    /// 工具 schema 的字符数（`chars` 只算 messages）。判断「要不要瘦身」时两个数一起看。
    pub tools_chars: usize,
    pub folded: usize,
}

/// 请求体预处理：**只做确定性的、与轮次无关的整理**。
///
/// 两条改动都是为了前缀缓存：
///
/// - `tools` 按函数名排序：客户端偶尔会调换顺序（哪怕只是并发加载造成的），
///   顺序一变前缀就整段失效。排序后同一会话的每一轮字节一致。
/// - 老工具结果折叠：`tool` 消息常常一条就是几万字（读文件、grep 的输出），
///   几十轮下来历史会滚到几十万字符。只截老的、保留最近 [`FOLD_KEEP_TOOLS`] 条。
///
/// ⚠️ 折叠规则必须是**纯函数**：同样的历史每次必须折出同样的字节，否则前缀依然失效。
pub fn prepare(obj: &mut serde_json::Map<String, Value>) -> Prep {
    let mut prep = Prep::default();

    if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
        // 稳定排序：同名时按原序列化文本比较，保证结果与输入顺序无关里唯一确定的那一种。
        tools.sort_by_cached_key(|t| {
            let name = t
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (name, serde_json::to_string(t).unwrap_or_default())
        });
        prep.tools = tools.len();
        prep.tools_chars = serde_json::to_string(&*tools).map(|s| s.len()).unwrap_or(0);
    }

    let Some(msgs) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return prep;
    };
    prep.msgs = msgs.len();
    prep.chars = serde_json::to_string(&*msgs).map(|s| s.len()).unwrap_or(0);
    prep.folded = fold_history(msgs);
    prep
}

/// 折叠老的工具结果。返回折叠条数（0 = 没折，可能是没超门槛或都很短）。
fn fold_history(msgs: &mut [Value]) -> usize {
    let total: usize = msgs
        .iter()
        .map(|m| serde_json::to_string(m).map(|s| s.len()).unwrap_or(0))
        .sum();
    if total <= fold_trigger_chars() {
        return 0;
    }
    let tool_idx: Vec<usize> = msgs
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("tool"))
        .map(|(i, _)| i)
        .collect();
    if tool_idx.len() <= FOLD_KEEP_TOOLS {
        return 0;
    }
    let mut folded = 0usize;
    for &i in &tool_idx[..tool_idx.len() - FOLD_KEEP_TOOLS] {
        let Some(text) = msgs[i].get("content").and_then(Value::as_str) else {
            continue;
        };
        let n = text.chars().count();
        if n <= FOLD_HEAD_CHARS {
            continue;
        }
        let head: String = text.chars().take(FOLD_HEAD_CHARS).collect();
        msgs[i]["content"] = json!(format!(
            "{head}\n…（这段工具结果已折叠：原 {n} 字。需要细节请重新调用该工具，不要凭这段残文猜）"
        ));
        folded += 1;
    }
    folded
}

// ---------------------------------------------------------------------------
// 会话亲和
// ---------------------------------------------------------------------------

/// FNV-1a 64：跨进程、跨版本都稳定（`DefaultHasher` 不行，标准库不保证它不换算法）。
fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 从指纹派生一个**UUID 形状**的稳定 ID（上游按 UUID 收字段，格式必须像）。
///
/// 同一 `seed` + 同一 `salt` 永远得到同一个值；不同 salt 得到不同值。
pub fn stable_uuid(seed: &str, salt: &str) -> String {
    let a = fnv1a64(format!("{salt}\u{1}{seed}").as_bytes());
    let b = fnv1a64(format!("{salt}\u{2}{seed}").as_bytes());
    let hex = format!("{a:016x}{b:016x}");
    format!(
        "{}-{}-4{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// 会话亲和开关：`TWIN_TS_NO_AFFINITY=1` 时退回「每次新 UUID」。
///
/// 留着这个开关的原因：ID 复用若被上游理解成「同一会话的续写」，极端情况下可能串话。
/// 报告里把它列为待 A/B 的假设 —— 真出事时先关它，别去回滚整个版本。
pub fn affinity_enabled() -> bool {
    std::env::var("TWIN_TS_NO_AFFINITY").as_deref() != Ok("1")
}

/// 会话 ID：亲和开启时按指纹派生，否则每次新 UUID。
pub fn session_id(seed: &str, salt: &str) -> String {
    if affinity_enabled() {
        stable_uuid(seed, salt)
    } else {
        uuid::Uuid::new_v4().to_string()
    }
}

// ---------------------------------------------------------------------------
// 会话身份（2026-11-03 重做）
// ---------------------------------------------------------------------------
//
// ## 为什么要重做
//
// 用户 2026-11-03 报了两件事，都在 Trae 的「用量明细」里能看到：
//
//   ① 一条 **11-02 18:09** 的记录（标题 `Prompt`）累计到 **1756.79 分**，
//      今天的花费也算进了昨天那一行；
//   ② 同一段对话被拆成两条记录（14:27 的「你能干什么」、16:09 的「你能吃法吗」）。
//
// 真机数据（`gateway-metrics.jsonl`）支持这个判断：那段对话从 11-02 18:09 一直续到
// 11-03 16:13，`msgs` **692 → 1849 单调增长、中间没有一次回退**，也就是它自始至终
// 是同一段对话。上游按我们发过去的 `conversation_id` 归账：
//
//   * id 不变 ⇒ 所有轮次累加进同一条记录，而记录的时间是它**第一次出现**的时刻
//     ⇒ 今天的消耗显示在昨天那行（①）；
//   * id 一变 ⇒ 当场多出一条新记录（②）。
//
// 旧指纹里恰好放了**会变的东西**：
//
//   * `system`：dsh 自己会把系统提示词截断成 `…<已截断，原长 27450 字>`（实测
//     `messages[0]` 就是 2017 字正文 + 这行尾巴）。那个数字随说明文件改动而变，
//     一变整段对话就换了 id。
//   * 工具名：客户端开关 MCP 就变。
//   * 首条 user：dsh 的 goal 模式把它写成 `<goal_round> … Round: 1/256`，
//     轮次推进就变。
//
// 这三样都**不定义「这是哪段对话」**，只定义内容，所以不该进身份。
//
// ## 现在的口径
//
// 身份 = 上游模型 + **剥掉易变数字、折叠空白后的**首条 user。
// 上游会话 ID = 身份 + 账号 uid + **本地日期**（见 [`conv_date`]），
// 于是「同一段对话」在一个自然日内只有一个 id：不跨天累加、也不被中途改写拆开。
//
// ⚠️ 代价（有意的，写在前面）：跨天时第一笔请求会丢一次上游前缀缓存。
// 一天一次、可预期；换来的是「用量明细按天可对账」。不想按天切就设
// `TWIN_TS_CONV_ROTATE=off`，退回「整段对话一个 id」。

/// 把「会随轮次变、但不改变对话身份」的内容抹平：**去掉所有 ASCII 数字**、
/// 空白折叠成单个空格，再截 `limit` 个字符。
///
/// 为什么直接去数字而不是逐条写正则：这类文本里的易变部分几乎都是数字
/// （`Round: 1/256`、`原长 27450 字`、`2026-11-03`、`16:09:32`、`第 12 轮`），
/// 而身份只需要「哪段对话」，丢掉数字带来的碰撞在实践中不存在
/// （真撞上也只是两段对话共用一条用量记录，不影响模型输出）。
fn stable_text(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(limit.min(text.len()) + 8);
    let mut n = 0usize;
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space {
            if n > 0 {
                out.push(' ');
                n += 1;
            }
            pending_space = false;
        }
        out.push(ch);
        n += 1;
        if n >= limit {
            break;
        }
    }
    out
}

/// 某条消息的文本（`content` 可能是字符串，也可能是分段数组）。
fn text_of(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
        None => String::new(),
    }
}

/// 第一条指定角色的消息（找不到就是空串）。
fn first_text(arr: &[Value], role: &str) -> String {
    arr.iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some(role))
        .map(text_of)
        .unwrap_or_default()
}

/// 会话身份（**不含账号、不含日期**）：上游模型 + 归一化后的首条 user。
fn conversation_identity(model: &str, arr: &[Value]) -> String {
    let mut s = String::with_capacity(2200);
    s.push_str(model);
    s.push('\u{1}');
    s.push_str(&stable_text(&first_text(arr, "user"), 2000));
    s
}

/// 会话指纹（给「会话粘性」用）：同一段对话固定同一个账号。
///
/// **故意不含 uid**：它回答的是「这是哪段对话」，不是「谁在跑」。
/// **也不含日期**：跨零点的同一段对话应当继续粘在同一个账号上，
/// 否则换号会连带换掉上游会话 ID，白丢一次前缀缓存。
pub fn conversation_key(model: &str, req: &Value) -> String {
    let arr = req
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let seed = conversation_identity(model, &arr);
    // 取摘要做键：首条 user 可能有几千字，直接当 HashMap 键太重。
    format!("{:016x}", fnv1a64(seed.as_bytes()))
}

/// 上游会话 ID 里那段**日期**。
///
/// `TWIN_TS_CONV_ROTATE=off/0/false` → 不按天切（退回旧行为：整段对话一个 id）。
/// `TWIN_TS_CONV_DATE=2026-11-03` → 显式指定（单测用，避免多线程抢环境变量）。
fn conv_date() -> String {
    if matches!(
        std::env::var("TWIN_TS_CONV_ROTATE").ok().as_deref(),
        Some("off") | Some("0") | Some("false")
    ) {
        return String::new();
    }
    if let Ok(v) = std::env::var("TWIN_TS_CONV_DATE") {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 上游会话 ID 的种子：身份 + 账号 uid + 日期。
///
/// uid 参与是**必须**的：上游的会话历史是挂在账号下的，两个账号共用一个
/// `conversation_id` 没有意义（也拿不到各自的前缀缓存）。
pub fn session_seed(model: &str, uid: &str, msgs: &Value) -> String {
    session_seed_at(model, uid, msgs, &conv_date())
}

/// 可注入日期版本（单测用）。
pub fn session_seed_at(model: &str, uid: &str, msgs: &Value, date: &str) -> String {
    let arr = msgs.as_array().cloned().unwrap_or_default();
    let identity = conversation_identity(model, &arr);
    let mut seed = String::with_capacity(identity.len() + 64);
    seed.push_str(&identity);
    seed.push('\u{1}');
    seed.push_str(uid);
    if !date.is_empty() {
        seed.push('\u{1}');
        seed.push_str(date);
    }
    seed
}

// ---------------------------------------------------------------------------
// 排队信封解析
// ---------------------------------------------------------------------------

/// 从 `raw` 里找 `key` 后面的第一个整数。
///
/// ⚠️ `body` 在信封里是**字符串**，解析后引号已经还原（`"queueCount":2328`）；
/// 但如果拿到的是没解析的原文，写的是 `queueCount\":2328`。两种都要认 ——
/// 只认其中一种就会退化成「默认等 30 秒」，而真实排队可能是 64 秒甚至 300 秒。
fn int_after(raw: &str, key: &str) -> Option<u64> {
    let mut from = 0usize;
    while let Some(rel) = raw[from..].find(key) {
        let at = from + rel + key.len();
        let rest = raw[at..].trim_start_matches(['"', '\\', ':', ' ']);
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        from = at;
    }
    None
}

/// 排队信封 → `(等多久秒, 前面还有多少人)`。
///
/// 字段按官方与实测两种形态都认：`retryAfterSeconds`（官方）/ `waitTime`（实测），
/// `queueCount`（实测）。都没有时保守给 30 秒。
pub fn queue_wait_of(raw: &str) -> (u64, Option<u64>) {
    let wait = int_after(raw, "retryAfterSeconds")
        .or_else(|| int_after(raw, "waitTime"))
        .unwrap_or(30)
        .max(1);
    (wait, int_after(raw, "queueCount"))
}

/// 排队提示的人话文案（流式与非流式共用）。
///
/// ⚠️ Qoder 的 `queueCount` 经常是 **0** 而 `serviceAvailable=false`（`RESOURCE_LIMIT`）：
/// 那不是"排在 0 个人后面"，是**这个模型此刻没有可用资源**。原来一律写「前面约 0 人」，
/// 用户会以为"马上就轮到了"，实际每次都干等 30 秒（2026-11-02 用户截图连刷 8 次）。
pub fn queue_text(wait: u64, count: Option<u64>) -> String {
    match count {
        Some(0) => format!("上游资源受限（服务端暂不可用），预计等 {wait} 秒"),
        Some(n) => format!("上游排队中（前面约 {n} 人，预计等 {wait} 秒）"),
        None => format!("上游排队中，预计等 {wait} 秒"),
    }
}

// ---------------------------------------------------------------------------
// 流式解析
// ---------------------------------------------------------------------------

/// 把上游的字节流按协议解析成 [`Delta`]。
pub struct StreamReader {
    stream: BoxStream,
    kind: Kind,
    buf: String,
    /// Trae 的 `event:` 行。
    event: String,
    done: bool,
    /// 是否已经吐过正文 / 工具调用（决定空闲超时用哪一档）。
    started: bool,
    usage: Value,
    finish: Option<String>,
    /// 本次请求的工具定义：收尾时按 schema 校正参数类型（模型常把 number/bool 写成字符串）。
    tools: Value,
    /// 实时监控的活跃条目 id（0 = 不登记）。
    live_id: u64,
    /// 自上次上报以来又收了多少字节（给活跃条目的「已收字节」用）。
    live_pending: usize,
}

impl StreamReader {
    pub fn new(resp: reqwest::Response, kind: Kind) -> Self {
        let stream: BoxStream = Box::pin(resp.bytes_stream());
        Self {
            stream,
            kind,
            buf: String::new(),
            event: String::new(),
            done: false,
            started: false,
            usage: Value::Null,
            finish: None,
            tools: Value::Null,
            live_id: 0,
            live_pending: 0,
        }
    }

    /// 把这条流关联到实时监控的活跃条目（用于「已收字节」进度）。
    ///
    /// 单位是**上游原始字节**（含 SSE 帧与 JSON 结构），不是 token —— 界面上按 KB 显示。
    pub fn set_live(&mut self, id: u64) {
        self.live_id = id;
    }

    /// 带上工具定义：收尾时按 schema 把 `"30000"` / `"true"` 这类字符串纠回数字/布尔。
    pub fn with_tools(mut self, tools: &Value) -> Self {
        self.tools = tools.clone();
        self
    }

    /// 离线构造：把一段**录下来的**流文本交给解析器（不连网）。
    ///
    /// 用途很实在：真机出问题时，抓一段原始流存成文件，用这个入口重放，
    /// 就能确认「是我们的解析错了」还是「上游本来就没发正文」。
    pub fn from_text(text: &str, kind: Kind) -> Self {
        let empty: BoxStream = Box::pin(futures_util::stream::empty());
        Self {
            stream: empty,
            kind,
            buf: text.to_string(),
            event: String::new(),
            done: true,
            started: false,
            usage: Value::Null,
            finish: None,
            tools: Value::Null,
            live_id: 0,
            live_pending: 0,
        }
    }

    pub fn finish_reason(&self) -> Option<String> {
        self.finish.clone()
    }

    /// 读一批增量。
    ///
    /// - `Ok(None)` 流正常结束；
    /// - `Err` 上游报错 / 排队 / 首字前就断了 —— 调用方据此**换号重试**。
    ///
    /// ⚠️ 空闲超时分两档：还没吐过任何东西时给 90 秒（排队、冷启动），
    /// 已经在吐的时候只等 3 秒（Qoder 的流答完不关连接，只能靠空闲判结束）。
    /// 把累积的字节数上报给实时监控（攒着批量报，避免每行一次加锁）。
    fn flush_live(&mut self) {
        if self.live_pending > 0 {
            crate::modules::gateway_live::note_output(self.live_id, self.live_pending);
            self.live_pending = 0;
        }
    }

    pub async fn next_batch(&mut self) -> Result<Option<Vec<Delta>>, String> {
        use futures_util::StreamExt;
        loop {
            if let Some(pos) = self.buf.find('\n') {
                let line: String = self.buf.drain(..=pos).collect();
                // 实时监控：按上游**原始字节**累加（含 SSE/JSON 开销，不等于 token）。
                self.live_pending += line.len();
                let out = self.parse_line(&line)?;
                if !out.is_empty() {
                    self.flush_live();
                    return Ok(Some(out));
                }
                continue;
            }
            if self.done {
                if self.buf.trim().is_empty() {
                    return Ok(None);
                }
                let rest = std::mem::take(&mut self.buf);
                self.live_pending += rest.len();
                let out = self.parse_line(&rest)?;
                self.flush_live();
                return Ok(if out.is_empty() { None } else { Some(out) });
            }
            // 首包前给 90 秒（排队 / 冷启动）；吐过内容后按上游分档（见 `Kind::idle_after_start_secs`）。
            let idle = if self.started {
                self.kind.idle_after_start_secs()
            } else {
                90
            };
            match tokio::time::timeout(Duration::from_secs(idle), self.stream.next()).await {
                Ok(Some(Ok(bytes))) => {
                    self.buf.push_str(&String::from_utf8_lossy(&bytes));
                }
                Ok(Some(Err(e))) => {
                    return Err(if self.started {
                        format!("上游流中断：{e}")
                    } else {
                        format!("读取上游流出错：{e}")
                    })
                }
                Ok(None) => self.done = true,
                Err(_) => {
                    return Err(if self.started {
                        format!("上游流空闲超时（{idle} 秒没有新数据）")
                    } else {
                        format!("上游 {idle} 秒没有任何输出（大概率在排队，或这个号被限流）")
                    })
                }
            }
        }
    }

    fn parse_line(&mut self, raw: &str) -> Result<Vec<Delta>, String> {
        let line = raw.trim();
        // 有的版本每行前面带 `id:N `。
        let line = line
            .strip_prefix("id:")
            .and_then(|rest| rest.split_once(' ').map(|(_, l)| l.trim()))
            .unwrap_or(line);
        if line.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.event = rest.trim().to_string();
            return Ok(Vec::new());
        }
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(Vec::new());
        };
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(Vec::new());
        };
        match self.kind {
            Kind::Trae => self.parse_trae(&v),
            Kind::Qoder => self.parse_qoder(&v),
        }
    }

    /// Trae 私有事件流。
    fn parse_trae(&mut self, v: &Value) -> Result<Vec<Delta>, String> {
        let mut out = Vec::new();
        match self.event.as_str() {
            "output" | "thought" => {
                if let Some(t) = v.get("response").and_then(Value::as_str) {
                    if !t.is_empty() {
                        out.push(Delta::Content(t.to_string()));
                        self.started = true;
                    }
                } else if let Some(t) = v.get("thought").and_then(Value::as_str) {
                    if !t.is_empty() {
                        out.push(Delta::Reasoning(t.to_string()));
                    }
                }
                if let Some(t) = v.get("reasoning_content").and_then(Value::as_str) {
                    if !t.is_empty() {
                        out.push(Delta::Reasoning(t.to_string()));
                    }
                }
                // 工具调用也在这个事件里；正文常常是空的，只收正文会把「调用了工具」
                // 当成「空回答」（2026-10-24 实测）。
                if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
                    for c in calls {
                        out.push(Delta::ToolCall(c.clone()));
                        self.started = true;
                    }
                }
                for key in ["tool_call", "function_call"] {
                    if let Some(c) = v.get(key) {
                        if c.is_object() {
                            out.push(Delta::ToolCall(c.clone()));
                            self.started = true;
                        }
                    }
                }
            }
            "token_usage" => {
                self.usage = v.clone();
                out.push(Delta::Usage(v.clone()));
            }
            "done" => {
                if let Some(f) = v.get("finish_reason").and_then(Value::as_str) {
                    self.finish = Some(f.to_string());
                    out.push(Delta::Finish(f.to_string()));
                }
                self.done = true;
            }
            "turn_completion" => {
                if self.finish.is_none() {
                    self.finish = Some("stop".into());
                    out.push(Delta::Finish("stop".into()));
                }
                self.done = true;
            }
            "error" => {
                let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
                let msg = v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("上游返回了未描述的错误");
                return Err(format!("Trae 上游报错 code {code}: {msg}"));
            }
            _ => {}
        }
        Ok(out)
    }

    /// Qoder 信封 SSE：`data:{"body":"<内层 JSON 字符串>","statusCodeValue":200,…}`。
    fn parse_qoder(&mut self, envelope: &Value) -> Result<Vec<Delta>, String> {
        let mut out = Vec::new();
        if let Some(code) = envelope.get("statusCodeValue").and_then(Value::as_i64) {
            if code != 200 {
                let raw = envelope
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.done = true;
                if raw.contains("isQueued") || raw.contains("10605") {
                    let (wait, count) = queue_wait_of(&raw);
                    // 约定：`QUEUED|<秒>|<可读文案>` —— 外层据此按 retryAfterSeconds 等。
                    return Err(format!(
                        "QUEUED|{wait}|{}",
                        queue_text(wait, count)
                    ));
                }
                return Err(format!(
                    "上游 HTTP {code}：{}",
                    raw.chars().take(160).collect::<String>()
                ));
            }
        }
        let inner: Value = match envelope.get("body") {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
            Some(v @ Value::Object(_)) => v.clone(),
            _ => return Ok(out),
        };
        if let Some(msg) = inner
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            // 已经在吐内容了就别因为尾部的错误字段把整段丢掉。
            if !self.started {
                return Err(format!("Qoder 上游报错：{msg}"));
            }
        }
        if let Some(u) = inner.get("usage") {
            self.usage = u.clone();
            out.push(Delta::Usage(u.clone()));
        }
        if let Some(choices) = inner.get("choices").and_then(Value::as_array) {
            for c in choices {
                if let Some(d) = c.get("delta") {
                    if let Some(t) = d.get("content").and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(Delta::Content(t.to_string()));
                            self.started = true;
                        }
                    }
                    if let Some(t) = d.get("reasoning_content").and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(Delta::Reasoning(t.to_string()));
                        }
                    }
                    if let Some(list) = d.get("tool_calls").and_then(Value::as_array) {
                        for one in list {
                            out.push(Delta::ToolCall(one.clone()));
                            self.started = true;
                        }
                    }
                }
                // 非流式形态（有的模型一次给全）。
                if let Some(m) = c.get("message") {
                    if let Some(t) = m.get("content").and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(Delta::Content(t.to_string()));
                            self.started = true;
                        }
                    }
                    if let Some(list) = m.get("tool_calls").and_then(Value::as_array) {
                        for one in list {
                            out.push(Delta::ToolCall(one.clone()));
                            self.started = true;
                        }
                    }
                }
                if let Some(fr) = c.get("finish_reason").and_then(Value::as_str) {
                    self.finish = Some(fr.to_string());
                    out.push(Delta::Finish(fr.to_string()));
                    self.done = true;
                }
            }
        }
        Ok(out)
    }

    pub fn usage(&self) -> &Value {
        &self.usage
    }
}

// ---------------------------------------------------------------------------
// 出口：OpenAI SSE
// ---------------------------------------------------------------------------

/// 把增量拼成 OpenAI SSE 帧。
pub struct Emitter {
    id: String,
    model: String,
    created: i64,
    role_sent: bool,
    /// 工具调用在这里累积，**收尾时一次性发出去**（见 [`Emitter::finish_frames`]）。
    calls: Vec<ToolCallAcc>,
    content_seen: bool,
    /// 本次请求的工具定义：收尾时按 schema 校正参数类型（模型常把 number/bool 写成字符串）。
    tools: Value,
}

impl Emitter {
    pub fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-ts-{}", uuid::Uuid::new_v4().simple()),
            model: model.to_string(),
            created: crate::modules::qoder_accounts::now_ms() / 1000,
            role_sent: false,
            calls: Vec::new(),
            content_seen: false,
            tools: Value::Null,
        }
    }

    /// 带上工具定义：收尾时按 schema 把 `"30000"` / `"true"` 这类字符串纠回数字/布尔。
    pub fn with_tools(mut self, tools: &Value) -> Self {
        self.tools = tools.clone();
        self
    }

    fn frame(&mut self, mut delta: Value, finish: Option<Value>) -> String {
        if !self.role_sent {
            if let Some(o) = delta.as_object_mut() {
                o.insert("role".into(), json!("assistant"));
            }
            self.role_sent = true;
        }
        let v = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish.unwrap_or(Value::Null) }],
        });
        format!("data: {v}\n\n")
    }

    /// 开场帧：一个只有 `role` 的空 delta。
    ///
    /// OpenAI 自己的第一个 chunk 就是这个形状。多数客户端无所谓，但**严格的实现**
    /// 会等这一帧建好消息对象；不发的话，个别客户端会把第一段正文丢掉。
    pub fn opening_frame(&mut self) -> String {
        self.frame(json!({}), None)
    }

    /// 「上游在排队」的**可见**提示帧。
    ///
    /// ## 为什么用正文 delta 而不是 SSE 注释
    ///
    /// 标准做法是用注释行（`: queued`，客户端会忽略）来保活，但那样**用户什么也看不到**
    /// —— 而这里的诉求恰恰是「在别的工具里也能看出在排队」：
    /// Qoder 上游排队最长要等 4 分钟，期间客户端界面一直空着，看起来像卡死。
    ///
    /// 所以直接推一段正文。代价是这段文字会进入对话内容（客户端会把 assistant 回复存下来），
    /// 这是有意的取舍：**看得见**比干净更重要。
    pub fn notice(&mut self, text: &str) -> String {
        // 标记「已经产出过内容」：否则排队提示之后若上游回空，收尾会误报「空流」。
        self.content_seen = true;
        self.frame(json!({ "content": text }), None)
    }

    /// 吃一批增量，返回要立刻发给客户端的 SSE 文本（可能为空）。
    pub fn absorb(&mut self, ds: &[Delta]) -> String {
        let mut text = String::new();
        let mut think = String::new();
        for d in ds {
            match d {
                Delta::Content(t) => {
                    text.push_str(t);
                    self.content_seen = true;
                }
                Delta::Reasoning(t) => think.push_str(t),
                Delta::ToolCall(c) => {
                    crate::modules::gateway_server::absorb_tool_call(&mut self.calls, c)
                }
                // Finish / Usage 由外层收尾时统一处理。
                Delta::Finish(_) | Delta::Usage(_) => {}
            }
        }
        let mut out = String::new();
        if !think.is_empty() {
            out.push_str(&self.frame(json!({ "reasoning_content": think }), None));
        }
        if !text.is_empty() {
            out.push_str(&self.frame(json!({ "content": text }), None));
        }
        out
    }

    /// 收尾帧：工具调用 + finish_reason + `[DONE]`。
    ///
    /// ⚠️ 工具调用在这里**一次性全发**（同一个 delta 里带齐所有 index）：
    /// 客户端拿到完整集合才能把多个调用**并发**执行。以前一个一个发、
    /// 甚至只发最后一个，客户端只能串行，串行就多叠几轮 TTFT（L2-2）。
    pub fn finish_frames(&mut self, finish: Option<String>, usage: &Value) -> String {
        let mut calls = crate::modules::gateway_server::finalize_tool_calls(&self.calls);
        // 模型给的参数类型不一定合 schema（常见：`"wait": "true"` / `"timeoutMs": "30000"`）。
        // 客户端是**按 schema 校验**的，类型不对直接报 invalid arguments —— 必须在交出前纠回。
        crate::modules::gateway_server::coerce_tool_call_args(&mut calls, &self.tools);
        let has_calls = !calls.is_empty();
        let mut out = String::new();
        if has_calls {
            out.push_str(&self.frame(json!({ "tool_calls": calls }), None));
        }
        let reason = crate::modules::gateway_server::finish_with_calls(finish, has_calls);
        let mut last = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }],
        });
        if !usage.is_null() {
            // 归一化后再给客户端：各家的缓存字段叫法不同，不统一就会出现
            // 「明明命中了，界面显示 0%」（见 `normalize_usage`）。
            last["usage"] = normalize_usage(usage);
        }
        out.push_str(&format!("data: {last}\n\n"));
        out.push_str("data: [DONE]\n\n");
        out
    }

    /// 出错帧。⛔ 不许「静默结束」：客户端必须能看出这是失败，不是空回答。
    pub fn error_frames(&self, msg: &str) -> String {
        let v = json!({
            "error": {
                "message": msg,
                "type": "upstream_error",
                "code": 502,
            }
        });
        format!("data: {v}\n\ndata: [DONE]\n\n")
    }
}

/// `tokio::mpsc::Receiver` → `Stream`（不引新依赖，也不依赖 `futures` 的 unfold 特性门）。
struct RxStream {
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
    /// 实时监控的活跃条目 id：响应体被丢弃时（= 客户端断开 / 用户手动停止）立刻摘掉它。
    live_id: u64,
}

/// 响应体一被丢弃就收尾。
///
/// ⚠️ 这就是「手动停止任务后，面板里还显示在跑」的修法：客户端断开时 axum 丢掉响应体，
/// 而生产端的 `tx.send` 要等**下一块上游数据**才会失败 —— 中间可能几十秒；
/// 若上游正好在长时间思考，则更久。挂在响应体上的守护在断开那一刻就生效。
///
/// 正常结束不会误伤：`finish()` 在关掉 `tx` 之前就跑了，条目已经不在活跃表里，
/// 这里再 `abandon` 一次是幂等的。
impl Drop for RxStream {
    fn drop(&mut self) {
        crate::modules::gateway_live::abandon(self.live_id);
    }
}

impl futures_util::Stream for RxStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// 已经有第一批增量之后的响应：边读边发。
///
/// `pending` 是「首字探测」时已经读到的那一批，先发出去再接后续。
pub fn respond(
    mut em: Emitter,
    mut reader: StreamReader,
    pending: Vec<Delta>,
    mut trace: Trace,
) -> Response {
    let live_id = trace.live_id;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let head = em.opening_frame();
        if tx.send(Ok(Bytes::from(head))).await.is_err() {
            return;
        }
        if !pending.is_empty() {
            if pending
                .iter()
                .any(|d| matches!(d, Delta::Content(_) | Delta::ToolCall(_)))
            {
                trace.first_byte();
            }
            let out = em.absorb(&pending);
            if !out.is_empty() {
                if tx.send(Ok(Bytes::from(out))).await.is_err() {
                    return;
                }
            }
        }
        pump_reader(&mut em, &mut reader, &tx, &mut trace).await;
    });

    sse_response(rx, live_id)
}

/// 把 reader 读到结束，边读边发；收尾统一交给 `em`（错误帧 / 结束帧）。
///
/// 抽出来是为了让 [`respond`] 与 [`respond_with_queue`] 共用同一段循环 ——
/// 两份拷贝迟早会分叉（一边加了 `first_byte` 记账、另一边忘了）。
async fn pump_reader(
    em: &mut Emitter,
    reader: &mut StreamReader,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    trace: &mut Trace,
) {
    let mut failure: Option<String> = None;
    loop {
        match reader.next_batch().await {
            Ok(Some(ds)) => {
                if ds
                    .iter()
                    .any(|d| matches!(d, Delta::Content(_) | Delta::ToolCall(_)))
                {
                    trace.first_byte();
                }
                let out = em.absorb(&ds);
                if !out.is_empty() {
                    if tx.send(Ok(Bytes::from(out))).await.is_err() {
                        return;
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    match failure {
        Some(e) => {
            let _ = tx.send(Ok(Bytes::from(em.error_frames(&e)))).await;
            trace.finish(false, 502, &e);
        }
        None if !em.content_seen && em.calls.is_empty() => {
            let _ = tx
                .send(Ok(Bytes::from(
                    em.error_frames("上游返回了流，但既没有正文也没有工具调用"),
                )))
                .await;
            trace.finish(false, 502, "空流：没有正文也没有工具调用");
        }
        None => {
            let finish = reader.finish.clone();
            let usage = reader.usage().clone();
            trace.usage(&usage);
            let tail = em.finish_frames(finish, &usage);
            let _ = tx.send(Ok(Bytes::from(tail))).await;
            trace.finish(true, 200, "");
        }
    }
}

fn sse_response(
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
    live_id: u64,
) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
        ],
        Body::from_stream(RxStream { rx, live_id }),
    )
        .into_response()
}

/// 上游「还没拿到第一批数据」时的失败分类。
pub enum OpenFail {
    /// 上游在排队：等 `secs` 秒再来一次，期间**给客户端推一条可见提示**。
    Queued { secs: u64, msg: String },
    /// 真失败（凭据死、参数错、流为空……）：直接回错误帧。
    Fatal(String),
}

/// 带「排队可见」的流式响应。
///
/// ## 与 [`respond`] 的区别
///
/// `respond` 是「reader 已经拿到手」才调用 —— 排队等待发生在它之前，
/// 那段时间**一个字节都没发出去**，客户端界面就是空的（Qoder 排队最长 240 秒）。
///
/// 这里把「获取 reader」也搬进流内部：先立刻建立 SSE 响应，然后在流里循环
/// `open()` —— 每次排队就推一条「⏳ 排队中」的正文，睡一会儿再试。
/// 于是调用方（WorkBuddy / Trae / 其他任何 OpenAI 兼容工具）**能看见它在排队**，
/// 而不是盯着一个不动的光标。
pub fn respond_with_queue<F, Fut>(
    mut em: Emitter,
    mut trace: Trace,
    mut open: F,
    budget_secs: u64,
) -> Response
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(StreamReader, Vec<Delta>), OpenFail>> + Send,
{
    let live_id = trace.live_id;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let head = em.opening_frame();
        if tx.send(Ok(Bytes::from(head))).await.is_err() {
            return;
        }
        let started = std::time::Instant::now();
        // ⚠️⚠️ 排队提示**只推一条**。
        //
        // 流式文本只能追加、不能改已发出去的内容：每重试一次推一条，界面上就会堆成
        // 一列「⏳ 上游排队中（已等 4 秒）」「（已等 39 秒）」「（已等 74 秒）」……
        // 越堆越长、还不断把最新状态顶上去（2026-11-01 用户截图，4 行全是同一件事）。
        //
        // 所以：首次排队时说一句（学 Qoder 客户端那句「模型请求正在排队·预计等待 N 秒」），
        // 之后不再重复；**过程的真实情况**（重试几次、累计等了多少）全部记进
        // `trace` 的 `queuedRetries` / `queuedWaitMs`，去网关指标里看。
        let mut notice_sent = false;
        let (mut reader, pending) = loop {
            match open().await {
                Ok(v) => break v,
                Err(OpenFail::Queued { secs, msg }) => {
                    let spent = started.elapsed().as_secs();
                    // 上游给的 retryAfterSeconds 要照等，但别把客户端拖到它自己超时。
                    let nap = secs.clamp(1, 45);
                    trace.note_queue_wait(nap);
                    if spent + nap > budget_secs {
                        let e = format!(
                            "上游排队超时：{msg}（已等 {spent} 秒仍没排到 —— 换号没用，\
                             队列是模型级的；可换冷门模型试试）"
                        );
                        let _ = tx.send(Ok(Bytes::from(em.error_frames(&e)))).await;
                        trace.finish(false, 429, &e);
                        return;
                    }
                    if !notice_sent {
                        // ★ 关键：先把「在排队」推给客户端，再睡。顺序反了就等于没做。
                        let out = em.notice(&format!("⏳ {msg}\n\n"));
                        if !out.is_empty() && tx.send(Ok(Bytes::from(out))).await.is_err() {
                            return; // 客户端已经断开，别白等
                        }
                        notice_sent = true;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(nap)).await;
                }
                Err(OpenFail::Fatal(e)) => {
                    let _ = tx.send(Ok(Bytes::from(em.error_frames(&e)))).await;
                    trace.finish(false, 502, &e);
                    return;
                }
            }
        };
        if !pending.is_empty() {
            if pending
                .iter()
                .any(|d| matches!(d, Delta::Content(_) | Delta::ToolCall(_)))
            {
                trace.first_byte();
            }
            let out = em.absorb(&pending);
            if !out.is_empty() {
                if tx.send(Ok(Bytes::from(out))).await.is_err() {
                    return;
                }
            }
        }
        pump_reader(&mut em, &mut reader, &tx, &mut trace).await;
    });

    sse_response(rx, live_id)
}

/// 把上游的「上下文缓存命中」字段归一化成各家客户端都认得的样子。
///
/// ## 为什么要归一化
///
/// 「缓存命中了多少 token」在不同厂商的 `usage` 里叫法完全不同，而客户端只认其中一两种。
/// dsh 这类界面读的是 `prompt_tokens_details.cached_tokens`（OpenAI 口径）或
/// `prompt_cache_hit_tokens`（DeepSeek 口径）；上游要是只给了另一种写法，
/// 界面就一律显示 **0 命中 / 0 读取**——看起来像「缓存完全没生效」，
/// 而实际上可能只是**名字对不上**（2026-11-01 用户反馈「缓存命中永远是 0」）。
///
/// ## 纪律：没有就**不补**
///
/// 三个来源一个都没有 ⇒ 原样返回，一个字段都不加。
/// ⛔ 绝不凭空写 `cached_tokens: 0` —— 那与「上游说命中 0」长得一样，
/// 而这两件事必须能区分（本项目反复强调的那条）。
pub fn normalize_usage(u: &Value) -> Value {
    if u.is_null() {
        return Value::Null;
    }
    let mut out = u.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };

    // 各家的写法，按优先级取第一个有值的。
    let detail = obj
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_i64);
    let ds_hit = obj.get("prompt_cache_hit_tokens").and_then(Value::as_i64);
    let anth = obj.get("cache_read_input_tokens").and_then(Value::as_i64);

    let cached = detail.or(ds_hit).or(anth);
    let Some(cached) = cached else {
        // 谁都没给 ⇒ 保持「没数据」，不加任何字段。
        return out;
    };

    // 补 OpenAI 口径
    if detail.is_none() {
        let d = obj
            .entry("prompt_tokens_details")
            .or_insert_with(|| json!({}));
        if let Some(d) = d.as_object_mut() {
            d.insert("cached_tokens".into(), json!(cached));
        }
    }
    // 补 DeepSeek 口径（含 miss，客户端要算命中率）
    if ds_hit.is_none() {
        obj.insert("prompt_cache_hit_tokens".into(), json!(cached));
    }
    if !obj.contains_key("prompt_cache_miss_tokens") {
        if let Some(prompt) = obj.get("prompt_tokens").and_then(Value::as_i64) {
            obj.insert(
                "prompt_cache_miss_tokens".into(),
                json!((prompt - cached).max(0)),
            );
        }
    }
    out
}

/// 上游本来就是标准 OpenAI SSE（WorkBuddy）⇒ 字节原样透传。
///
/// 少一层解析就少一处出错：它的 `tool_calls`、`finish_reason`、`[DONE]` 本来就是
/// 客户端认识的样子，重新拼一遍只会把细节拼丢。
///
/// ⚠️⚠️ **透传 ≠ 不记账**。这里曾经一个字节都不解析，于是「WorkBuddy 的 Key 已用
/// 永远是 0」—— 因为 WorkBuddy 客户端**一律走流式**，用量全在这条路上：
/// 非流式（网关自己聚合）能拿到 usage，流式却拿不到，指标里就成了一排
/// `prompt_tokens: null`（2026-10-29 从 `gateway-metrics.jsonl` 反查确认：
/// 18 条 wb-cn 流式请求全部无用量，非流式的几条都有）。
///
/// 现在边转发边扫 `data:` 行里的 `usage`，扫到就交给 `trace`；
/// `trace.finish` 是所有请求（流式/非流式/成功/失败）记额度与指标的**唯一收口**。
pub fn passthrough(
    resp: reqwest::Response,
    mut trace: Trace,
    rewrite_model: Option<(String, String)>,
) -> Response {
    use futures_util::StreamExt;
    // 生产端会把 `trace` move 进任务；响应体上的断开守护要用这个 id，先取出来。
    let live_id = trace.live_id;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut seen = 0usize;
        let mut buf = String::new();
        // 上游名 → 客户端看到的公开名，**只在首块做一次**。
        // 只改这一个字段：客户端（dsh 等）会把它显示/记进会话，露出中转站的代号很怪；
        // 但流式字节不能整体解析重拼（会拼丢 tool_calls），所以做等价的字节替换。
        let mut rewrite = rewrite_model;
        while let Some(item) = stream.next().await {
            match item {
                Ok(b) => {
                    if b.is_empty() {
                        continue;
                    }
                    let b = match rewrite.take() {
                        Some((from, to)) => rewrite_model_bytes(&b, &from, &to).unwrap_or(b),
                        None => b,
                    };
                    seen += b.len();
                    crate::modules::gateway_live::note_output(trace.live_id, b.len());
                    if trace.first_byte_ms.is_none() {
                        trace.first_byte();
                    }
                    // 只扫描、不改字节：给客户端的仍是上游原样（唯一例外是上面那一次 model 替换）。
                    scan_sse_usage(&mut buf, &b, &mut trace);
                    if tx.send(Ok(b)).await.is_err() {
                        // 客户端提前断开：上游可能已经算过费，按已经看到的部分记一笔，
                        // 否则这条请求在指标与 Key 额度里完全不存在。
                        trace.finish(false, 499, "客户端提前断开（按已收到的部分记账）");
                        return;
                    }
                }
                Err(e) => {
                    let msg = format!("上游流中断：{e}");
                    let _ = tx
                        .send(Ok(Bytes::from(format!(
                            "data: {{\"error\":{{\"message\":{},\"type\":\"upstream_error\"}}}}\n\ndata: [DONE]\n\n",
                            json!(msg)
                        ))))
                        .await;
                    trace.finish(false, 502, &msg);
                    return;
                }
            }
        }
        if seen == 0 {
            trace.finish(false, 502, "上游流为空");
        } else {
            trace.finish(true, 200, "");
        }
    });


    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
        ],
        Body::from_stream(RxStream { rx, live_id }),
    )
        .into_response()
}

/// 透传时把首块里的 `"model":"上游代号"` 换成客户端要的公开名。
///
/// 只在**首块**调用一次，且只匹配这一个 JSON 字段的字节序列 ——
/// 不做 UTF-8 解码、不动其它任何字节（SSE 里全是文本，但分块边界可能切断多字节
/// 字符，解码重拼有风险）。匹配不到就原样返回 `None`（模型名不是功能性问题，
/// 宁可还原失败也不能弄坏流）。
fn rewrite_model_bytes(b: &[u8], from: &str, to: &str) -> Option<Bytes> {
    let needle = format!("\"model\":\"{from}\"");
    let repl = format!("\"model\":\"{to}\"");
    let nb = needle.as_bytes();
    let idx = b.windows(nb.len()).position(|w| w == nb)?;
    let mut out = Vec::with_capacity(b.len() + repl.len());
    out.extend_from_slice(&b[..idx]);
    out.extend_from_slice(repl.as_bytes());
    out.extend_from_slice(&b[idx + nb.len()..]);
    Some(Bytes::from(out))
}

/// 透传时扫用量：把已完整的 `data:` 行交给 [`sse_usage`]，命中就写进 `trace`。
///
/// 只保留**没凑齐一行的尾巴**：完整行处理完立刻丢掉，所以内存不随流长度增长。
/// 单行异常大（上游把整个工具结果塞进一个 chunk）时放弃这一行 —— 用量帧本身很小，
/// 不会长成这样。
fn scan_sse_usage(buf: &mut String, chunk: &[u8], trace: &mut Trace) {
    /// 单行上限：超过就认为不是用量帧，丢掉这半行。
    const MAX_LINE: usize = 8 * 1024 * 1024;
    buf.push_str(&String::from_utf8_lossy(chunk));
    while let Some(pos) = buf.find('\n') {
        let line: String = buf.drain(..=pos).collect();
        if let Some(u) = sse_usage(&line) {
            trace.usage(&u);
        }
    }
    if buf.len() > MAX_LINE {
        buf.clear();
    }
}

/// 从一行 SSE 里取 `usage`。认两种位置：
/// - OpenAI 标准：`data: {"usage":{"prompt_tokens":..,"completion_tokens":..}}`
/// - 信封式（少数上游）：`data: {"data":{"usage":{...}}}`
///
/// 两个数都没有就返回 `None` —— 「上游没给」和「上游给了 0」必须区分开，
/// 前者不记（不让失败/空请求消耗额度），后者照记。
fn sse_usage(line: &str) -> Option<Value> {
    let data = line.trim().strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" || !data.contains("\"usage\"") {
        return None;
    }
    let v: Value = serde_json::from_str(data).ok()?;
    let has_num = |u: &Value| {
        u.get("prompt_tokens").and_then(Value::as_i64).is_some()
            || u.get("completion_tokens").and_then(Value::as_i64).is_some()
    };
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        if has_num(u) {
            return Some(u.clone());
        }
    }
    if let Some(u) = v.pointer("/data/usage").filter(|u| !u.is_null()) {
        if has_num(u) {
            return Some(u.clone());
        }
    }
    None
}

#[cfg(test)]
mod cache_norm_tests {
    use super::normalize_usage;
    use serde_json::json;

    /// 三种叫法都能补成客户端认得的样子（dsh 读 OpenAI 口径）。
    #[test]
    fn fills_openai_shape_from_deepseek_field() {
        let u = json!({"prompt_tokens": 1000, "completion_tokens": 10, "prompt_cache_hit_tokens": 800});
        let n = normalize_usage(&u);
        assert_eq!(
            n.pointer("/prompt_tokens_details/cached_tokens").and_then(|v| v.as_i64()),
            Some(800)
        );
        // miss 由 prompt - cached 推出来，客户端要它算命中率
        assert_eq!(n.get("prompt_cache_miss_tokens").and_then(|v| v.as_i64()), Some(200));
    }

    #[test]
    fn fills_deepseek_shape_from_openai_field() {
        let u = json!({"prompt_tokens": 500, "prompt_tokens_details": {"cached_tokens": 400}});
        let n = normalize_usage(&u);
        assert_eq!(n.get("prompt_cache_hit_tokens").and_then(|v| v.as_i64()), Some(400));
        assert_eq!(n.get("prompt_cache_miss_tokens").and_then(|v| v.as_i64()), Some(100));
    }

    /// 真实命中 0 时要保留 0，不能因为 `is_some` 判断写错而变成"没数据"。
    #[test]
    fn keeps_genuine_zero() {
        let u = json!({"prompt_tokens": 500, "prompt_cache_hit_tokens": 0});
        let n = normalize_usage(&u);
        assert_eq!(
            n.pointer("/prompt_tokens_details/cached_tokens").and_then(|v| v.as_i64()),
            Some(0)
        );
    }

    /// ⚠️ 上游一个缓存字段都没给 ⇒ **一个都不加**。
    ///
    /// 这是本项目反复强调的纪律：`null`（没这项统计）和 `0`（命中 0）必须能区分。
    /// 补一个 0 出去，用户就会以为「缓存完全没生效」，而真相可能只是上游不报这项。
    #[test]
    fn adds_nothing_when_upstream_reports_no_cache_field() {
        let u = json!({"prompt_tokens": 1000, "completion_tokens": 10});
        let n = normalize_usage(&u);
        assert!(n.get("prompt_tokens_details").is_none(), "不许凭空造 cached_tokens");
        assert!(n.get("prompt_cache_hit_tokens").is_none());
        assert!(n.get("prompt_cache_miss_tokens").is_none());
        assert_eq!(n.get("prompt_tokens").and_then(|v| v.as_i64()), Some(1000));
    }

    /// **真实样本**：Trae `deepseek-v4.1-flash` 的 usage 原文（2026-11-01 真机抄录）。
    ///
    /// 它用 Anthropic 的命名给了两个 `cache_*` 字段，**值都是 0**。
    /// 归一化必须把它变成 `cached_tokens: 0`（明确的"没命中"），
    /// 而不是什么都不补（那会让客户端显示"没数据"，和"命中 0"混为一谈）。
    #[test]
    fn real_trae_usage_reports_explicit_zero_cache() {
        let u = json!({
            "cache_creation_input_tokens": 0,
            "cache_creation_input_tokens_total": 0,
            "cache_read_input_tokens": 0,
            "cache_read_input_tokens_total": 0,
            "cluster": "normal_context",
            "completion_tokens": 186,
            "name": "",
            "prompt_tokens": 27074,
            "reasoning_tokens": 100,
            "total_tokens": 27260
        });
        let n = normalize_usage(&u);
        assert_eq!(
            n.pointer("/prompt_tokens_details/cached_tokens").and_then(|v| v.as_i64()),
            Some(0),
            "上游明确报了 cache_read_input_tokens=0，客户端就该看到 0"
        );
        assert_eq!(n.get("prompt_cache_hit_tokens").and_then(|v| v.as_i64()), Some(0));
        assert_eq!(
            n.get("prompt_cache_miss_tokens").and_then(|v| v.as_i64()),
            Some(27074),
            "全部未命中，miss 应等于 prompt_tokens"
        );
        // 其余字段一个都不能动
        assert_eq!(n.get("total_tokens").and_then(|v| v.as_i64()), Some(27260));
        assert_eq!(n.get("cluster").and_then(|v| v.as_str()), Some("normal_context"));
    }

    #[test]
    fn null_stays_null() {
        assert!(normalize_usage(&serde_json::Value::Null).is_null());
    }
}

#[cfg(test)]
mod usage_tests {
    use super::sse_usage;

    #[test]
    fn picks_openai_usage() {
        let line = "data: {\"id\":\"x\",\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":34}}\n";
        let u = sse_usage(line).expect("应当取到 usage");
        assert_eq!(u.get("prompt_tokens").and_then(|v| v.as_i64()), Some(12));
        assert_eq!(u.get("completion_tokens").and_then(|v| v.as_i64()), Some(34));
    }

    #[test]
    fn keeps_zero_but_rejects_empty_usage() {
        let zero = "data: {\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}";
        assert!(sse_usage(zero).is_some());
        assert!(sse_usage("data: {\"usage\":null}").is_none());
        assert!(sse_usage("data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}").is_none());
        assert!(sse_usage("data: [DONE]").is_none());
        assert!(sse_usage(": keep-alive").is_none());
    }

    #[test]
    fn picks_envelope_usage() {
        let line = "data: {\"code\":0,\"data\":{\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":9}}}";
        let u = sse_usage(line).expect("信封里的 usage 也要认");
        assert_eq!(u.get("completion_tokens").and_then(|v| v.as_i64()), Some(9));
    }
}

#[cfg(test)]
mod queue_notice_tests {
    use super::Emitter;

    /// 排队提示**只该出现一次**。
    ///
    /// 回归用：2026-11-01 用户截图里 WorkBuddy 对话中堆了 4 行「⏳ 上游排队中（已等 4/39/74/110 秒）」
    /// ——因为每重试一次就推一条。流式文本只能追加，堆出来的是一列越来越长的噪音。
    /// 这里把「推一次」的判据钉在代码结构上：`notice_sent` 标志只置一次。
    #[test]
    fn queue_notice_is_emitted_only_once() {
        // 直接断言源码里的语义太重；改为验证 Emitter 层面：同一段提示推两次会产生两帧，
        // 而调用方（respond_with_queue）用 `notice_sent` 拦住第二次。
        // 这条测试锁的是「确实存在这个闸门」——用文本扫描，防止以后被误删。
        let src = include_str!("gateway_stream.rs");
        assert!(
            src.contains("let mut notice_sent = false;"),
            "必须保留 notice_sent 闸门，否则排队提示会每次重试都推一条"
        );
        assert!(
            src.contains("if !notice_sent {"),
            "推提示必须包在 if !notice_sent 里"
        );
        assert!(
            src.contains("notice_sent = true;"),
            "推完必须置位，否则每条都会推"
        );
    }

    /// 排队提示必须是**客户端看得见的正文**。
    ///
    /// 用 SSE 注释（`: queued`）客户端会直接忽略 —— 那就等于没做，
    /// 而这条功能的全部意义就是「让用户在别的工具里看见还在排队」。
    #[test]
    fn notice_is_visible_content_delta() {
        let mut em = Emitter::new("test-model");
        let head = em.opening_frame();
        let n = em.notice("⏳ 上游排队中\n\n");
        assert!(n.starts_with("data: "), "必须是 SSE data 行");
        assert!(n.contains("\"content\""), "必须是 content delta，否则客户端不显示");
        assert!(n.contains("排队中"));
        // 角色只在开场帧带一次，后续帧不该重复带 role。
        assert!(head.contains("\"assistant\""));
        assert!(!n.contains("\"assistant\""));
        // 必须是合法 JSON（拼错一个引号客户端就整条丢弃）。
        let body = n.trim().strip_prefix("data: ").unwrap().trim();
        let v: serde_json::Value = serde_json::from_str(body).expect("帧必须是合法 JSON");
        assert_eq!(
            v.pointer("/choices/0/delta/content")
                .and_then(|x| x.as_str()),
            Some("⏳ 上游排队中\n\n")
        );
    }
}

#[cfg(test)]
mod idle_tests {
    use super::Kind;

    /// 回归：Trae 的「正文后空闲」**不能**用 Qoder 的 8 秒。
    ///
    /// 2026-11-03 用户实测：dsh 里用 Trae 的 GLM-5.3 一直超时，
    /// 网关侧日志是「上游流空闲超时（8 秒没有新数据）」——
    /// 模型在大上下文里思考 8 秒以上就被我们掐断。
    #[test]
    fn trae_allows_long_pauses_but_qoder_does_not() {
        assert_eq!(Kind::Qoder.idle_after_start_secs(), 8, "Qoder 靠空闲判收尾，必须短");
        assert!(
            Kind::Trae.idle_after_start_secs() >= 60,
            "Trae 有结束事件，空闲档只是兜底，不能短到掐断思考"
        );
    }
}

#[cfg(test)]
mod live_stream_tests {
    use super::RxStream;

    /// 客户端断开（响应体被丢弃）那一刻，活跃条目必须立刻消失。
    ///
    /// 生产端的 `tx.send` 要等下一块上游数据才失败；上游在思考时可能很久。
    #[test]
    fn dropping_response_body_clears_active() {
        let _g = crate::modules::gateway_live::test_lock();
        let before = crate::modules::gateway_live::snapshot(60)["kpi"]["active"]
            .as_u64()
            .unwrap();
        let id = crate::modules::gateway_live::start("m-drop", "trae-cn", None);
        {
            let (_tx, rx) = tokio::sync::mpsc::channel(4);
            let body = RxStream { rx, live_id: id };
            assert_eq!(
                crate::modules::gateway_live::snapshot(60)["kpi"]["active"]
                    .as_u64()
                    .unwrap(),
                before + 1
            );
            drop(body); // 模拟客户端断开
        }
        assert_eq!(
            crate::modules::gateway_live::snapshot(60)["kpi"]["active"]
                .as_u64()
                .unwrap(),
            before,
            "响应体丢弃后不该还挂在「正在跑」"
        );
    }
}
