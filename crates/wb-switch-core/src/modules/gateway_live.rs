//! 积分共享页的**实时监控**：内存里的秒级计数器。
//!
//! ## 为什么不直接读 `gateway-metrics.jsonl`
//!
//! 那份是**事后账本**（只写不读、按请求追加、超 1 MB 才滚动）。要 1 秒刷一次就得
//! 反复读几 MB 文件，成本高且数据天生滞后。所以这里另起一层**只活在内存里**的状态：
//!
//! ```text
//! 请求进 handle()   → start()        活跃 +1，登记条目（模型 / Key / 开始时刻）
//! 上游首字          → first_byte()   记「首字延迟」，活跃条目进入「输出中」
//! 流式每批次        → note_output()  累加已收字节（给活跃条目做进度）
//! Trace::finish()   → finish()       活跃 -1，把这条计入秒桶 / 分钟桶
//! 前端每秒          → snapshot(60)   读桶 → 聚合 → 渲染
//! ```
//!
//! ## 三个必须守住的点
//!
//! 1. **活跃计数不能泄漏**：任何提前 return 都得走 [`ActiveGuard`]（Drop 时减一），
//!    否则面板永远显示「正在跑」。`finish` 与 `abandon` 都是幂等的（按 id 找，找不到就忽略）。
//! 2. **刻度分两种**：秒桶留 300 个（5 分钟）、分钟桶留 60 个（1 小时）。60 秒/5 分钟窗口
//!    读秒桶，1 小时窗口读分钟桶 —— 只用一套秒桶要 3600 个槽，没必要。
//! 3. **只报真数**：token 来自上游 usage（拿不到就是 0，不估）；已收字节是上游原始流长度，
//!    不等于 token，界面上必须分开写。**不做**积分/金额折算（用户 2026-11-04 明确不要）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

/// 秒桶个数（5 分钟）。
const SEC_BUCKETS: usize = 300;
/// 分钟桶个数（1 小时）。
const MIN_BUCKETS: usize = 60;
/// 活跃列表上限：极端并发下只留最近这么多条，避免快照无限大。
const MAX_ACTIVE: usize = 32;
/// 活跃条目最多活这么久（20 分钟）：兜底清扫，见 `snapshot`。
const STALE_ACTIVE_MS: i64 = 20 * 60 * 1000;

/// 一个时间桶的累计值。
#[derive(Default, Clone)]
struct Slice {
    requests: u32,
    ok: u32,
    failed: u32,
    /// 被中断的请求（客户端点了停止 / 连接断了）：**不计入 requests、不计入失败率**。
    ///
    /// 它存在的唯一理由：用户点了停止之后，窗口里如果一片 0，看上去像统计坏了
    /// （2026-11-04 用户实测）。这一格让面板能说清「我看见了你的请求，是它被停了」。
    aborted: u32,
    queued: u32,
    prompt: i64,
    completion: i64,
    ttft_sum: i64,
    ttft_n: u32,
    /// 按 Key 的切片（只有产生过用量的 Key 才有条目）。
    keys: Option<HashMap<String, Slice>>,
}

/// 时间桶：`slot` 是它对应的秒（秒桶）或分钟（分钟桶）起点。
struct Bucket {
    slot: i64,
    data: Slice,
}

impl Bucket {
    fn new() -> Self {
        Self {
            slot: i64::MIN,
            data: Slice::default(),
        }
    }
    /// 归位到 `slot`：换桶就清空（环形复用）。
    fn at(&mut self, slot: i64) -> &mut Slice {
        if self.slot != slot {
            self.slot = slot;
            self.data = Slice::default();
        }
        &mut self.data
    }
}

/// 一条正在跑的请求。
struct Active {
    id: u64,
    model: String,
    scope: String,
    key_id: Option<String>,
    started_ms: i64,
    first_byte_ms: Option<i64>,
    /// 已收上游原始字节（含 SSE / JSON 协议开销，**不是 token**）。
    got_bytes: u64,
}

struct State {
    sec: Vec<Bucket>,
    min: Vec<Bucket>,
    active: Vec<Active>,
    /// Key 名字缓存：`key_id → (name, 刷新时刻 ms)`。避免每秒读一次 keys 文件。
    names: HashMap<String, (String, i64)>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(State {
            sec: (0..SEC_BUCKETS).map(|_| Bucket::new()).collect(),
            min: (0..MIN_BUCKETS).map(|_| Bucket::new()).collect(),
            active: Vec::new(),
            names: HashMap::new(),
        })
    })
}

/// 下一个活跃条目 id（0 表示「没登记」）。
fn next_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed) + 1
}

fn now_ms() -> i64 {
    crate::modules::qoder_accounts::now_ms()
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    // 监控数据**不值得让业务为它失败**：锁中毒时直接拿回内部值。
    state().lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// 写入侧
// ---------------------------------------------------------------------------

/// 登记一条请求，返回 id（`Trace::live_id`）。
pub fn start(model: &str, scope: &str, key_id: Option<&str>) -> u64 {
    let id = next_id();
    let mut st = lock();
    if st.active.len() >= MAX_ACTIVE {
        // 只留最近的：老条目丢掉（它的 finish 会找不到 → 幂等忽略）
        st.active.remove(0);
    }
    st.active.push(Active {
        id,
        model: model.to_string(),
        scope: scope.to_string(),
        key_id: key_id.map(str::to_string),
        started_ms: now_ms(),
        first_byte_ms: None,
        got_bytes: 0,
    });
    id
}

/// 补上作用域（`Trace::hit` 里才知道是 trae-cn / qoder / wb）。
///
/// 登记时还不知道，先给空串；界面上不能显示空作用域，所以命中后立刻补。
pub fn set_scope(id: u64, scope: &str) {
    if id == 0 || scope.is_empty() {
        return;
    }
    let mut st = lock();
    if let Some(a) = st.active.iter_mut().find(|a| a.id == id) {
        a.scope = scope.to_string();
    }
}

/// 补上 Key（`Trace::hit` 里才知道最终用了哪把 Key）。
pub fn set_key(id: u64, key_id: Option<&str>) {
    if id == 0 {
        return;
    }
    let Some(k) = key_id else { return };
    let mut st = lock();
    if let Some(a) = st.active.iter_mut().find(|a| a.id == id) {
        a.key_id = Some(k.to_string());
    }
}

/// 记首字时刻（只记第一次）。
pub fn first_byte(id: u64) {
    if id == 0 {
        return;
    }
    let now = now_ms();
    let mut st = lock();
    if let Some(a) = st.active.iter_mut().find(|a| a.id == id) {
        if a.first_byte_ms.is_none() {
            a.first_byte_ms = Some(now);
        }
    }
}

/// 累加「已收上游字节」（流式每批一次）。
///
/// ⚠️ 这是**原始流长度**（含 SSE 帧与 JSON 结构），不等于 token；界面上按 KB 显示。
pub fn note_output(id: u64, bytes: usize) {
    if id == 0 || bytes == 0 {
        return;
    }
    let mut st = lock();
    if let Some(a) = st.active.iter_mut().find(|a| a.id == id) {
        a.got_bytes = a.got_bytes.saturating_add(bytes as u64);
    }
}

/// 收尾：从活跃里摘掉，并计入秒桶 / 分钟桶。**幂等**。
#[allow(clippy::too_many_arguments)]
pub fn finish(
    id: u64,
    ok: bool,
    prompt: Option<i64>,
    completion: Option<i64>,
    ttft_ms: Option<i64>,
    queued: bool,
) {
    if id == 0 {
        return;
    }
    let now = now_ms();
    let sec = now / 1000;
    let min = sec / 60;
    let prompt = prompt.unwrap_or(0).max(0);
    let completion = completion.unwrap_or(0).max(0);

    let mut st = lock();
    let Some(pos) = st.active.iter().position(|a| a.id == id) else {
        // 已经结束（或被 `abandon` 清掉）：不重复记数。
        return;
    };
    let a = st.active.remove(pos);
    let key_id = a.key_id.clone();

    let add = |b: &mut Bucket, slot: i64| {
        let s = b.at(slot);
        s.requests += 1;
        if ok {
            s.ok += 1;
        } else {
            s.failed += 1;
        }
        if queued {
            s.queued += 1;
        }
        s.prompt += prompt;
        s.completion += completion;
        if let Some(t) = ttft_ms {
            s.ttft_sum += t;
            s.ttft_n += 1;
        }
        if let Some(k) = &key_id {
            let m = s.keys.get_or_insert_with(HashMap::new);
            let ks = m.entry(k.clone()).or_default();
            ks.requests += 1;
            ks.prompt += prompt;
            ks.completion += completion;
            if !ok {
                ks.failed += 1;
            }
        }
    };
    let slot_s = bslot(sec, SEC_BUCKETS);
    add(&mut st.sec[slot_s], sec);
    let slot_m = bslot(min, MIN_BUCKETS);
    add(&mut st.min[slot_m], min);
}

/// 没走 `finish` 就丢掉的请求（提前 return / 客户端停止 / panic）：从活跃里摘掉，
/// **不计入请求数与失败率**，但会在秒桶 / 分钟桶里记一次「中断」。
///
/// 为什么不计入统计：这条请求没有结果也没有用量，计进去会把「失败率」和「tok/s」都污染。
/// 为什么还要记 `aborted`：用户点了「停止」之后如果面板一片 0，看起来就像统计坏了
/// （2026-11-04 用户实测）。这一格让面板能说清「请求被中断过，所以没有 token」。
pub fn abandon(id: u64) {
    if id == 0 {
        return;
    }
    let now = now_ms();
    let sec = now / 1000;
    let min = sec / 60;
    let mut st = lock();
    let Some(pos) = st.active.iter().position(|a| a.id == id) else {
        // 已经 `finish` 过（或被清扫掉）：不重复记。
        return;
    };
    st.active.remove(pos);
    let slot_s = bslot(sec, SEC_BUCKETS);
    st.sec[slot_s].at(sec).aborted += 1;
    let slot_m = bslot(min, MIN_BUCKETS);
    st.min[slot_m].at(min).aborted += 1;
}

/// 活跃条目的生命周期守卫：`Drop` 时兜底摘除。
pub struct ActiveGuard(pub u64);

impl ActiveGuard {
    /// 把寿命**移交出去**（之后本守卫销毁时不再摘除）。
    ///
    /// 只有一个场景需要：请求已经把响应体交给 hyper 继续输出（流式）。这时函数返回
    /// 不代表请求结束，摘掉就等于面板提前说「跑完了」。移交后由响应体上的
    /// `RxStream`（Drop）与 `Trace::finish` 各自收口。
    pub fn disarm(&mut self) {
        self.0 = 0;
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        abandon(self.0);
    }
}

fn bslot(slot: i64, n: usize) -> usize {
    slot.rem_euclid(n as i64) as usize
}

// ---------------------------------------------------------------------------
// 读取侧
// ---------------------------------------------------------------------------

/// 给界面用的快照。`window_secs` 支持 60 / 300 / 3600（其它值就近取）。
pub fn snapshot(window_secs: u64) -> Value {
    let now = now_ms();
    let now_sec = now / 1000;
    let win = window_secs.clamp(10, 3600);

    let mut st = lock();
    // ⓪ 清扫卡死的条目：万一某条请求既没 `finish` 也没 `abandon`（提前 return、panic），
    //    它会永远挂在「正在跑」里。20 分钟远超任何正常流（Trae 的读超时上限是 15 分钟），
    //    过期的直接摘掉 —— 监控数据宁可不完整，也不能显示假的「正在跑」。
    st.active
        .retain(|a| now - a.started_ms < STALE_ACTIVE_MS);

    // ① 聚合窗口
    let (tot, series, per_key) = if win <= SEC_BUCKETS as u64 {
        let n = win as usize;
        let mut tot = Slice::default();
        let mut per_key: HashMap<String, (i64, i64, u32)> = HashMap::new();
        let mut series = Vec::with_capacity(n);
        for back in (0..n).rev() {
            let slot = now_sec - back as i64;
            let b = &mut st.sec[bslot(slot, SEC_BUCKETS)];
            let s = if b.slot == slot { b.data.clone() } else { Slice::default() };
            merge(&mut tot, &s);
            merge_keys(&mut per_key, &s);
            series.push(Point::of(slot, &s));
        }
        (tot, series, per_key)
    } else {
        let mins = (win / 60).max(1) as usize;
        let now_min = now_sec / 60;
        let mut tot = Slice::default();
        let mut per_key: HashMap<String, (i64, i64, u32)> = HashMap::new();
        let mut series = Vec::with_capacity(mins);
        for back in (0..mins).rev() {
            let slot = now_min - back as i64;
            let b = &mut st.min[bslot(slot, MIN_BUCKETS)];
            let s = if b.slot == slot { b.data.clone() } else { Slice::default() };
            merge(&mut tot, &s);
            merge_keys(&mut per_key, &s);
            series.push(Point::of(slot * 60, &s));
        }
        (tot, series, per_key)
    };

    // ② 先把所有要显示的 Key 名字解析出来（`key_name` 需要可变借，不能夹在借用 active 的时候调）
    let mut names: HashMap<String, String> = HashMap::new();
    let mut need: Vec<String> = per_key.keys().cloned().collect();
    need.extend(st.active.iter().filter_map(|a| a.key_id.clone()));
    need.sort();
    need.dedup();
    for k in need {
        let n = key_name(&mut st, &k);
        names.insert(k, n);
    }
    let active_count_of = |k: &str| st.active.iter().filter(|a| a.key_id.as_deref() == Some(k)).count();

    // ③ 活跃列表
    let active: Vec<Value> = st
        .active
        .iter()
        .map(|a| {
            let elapsed = (now - a.started_ms).max(0);
            let ttft = a.first_byte_ms.map(|t| (t - a.started_ms).max(0));
            json!({
                "model": a.model,
                "scope": a.scope,
                "key": a.key_id.as_deref().and_then(|k| names.get(k).cloned()).unwrap_or_default(),
                "elapsedMs": elapsed,
                "ttftMs": ttft,
                // 已经吐过首字 = 进入输出阶段
                "phase": if a.first_byte_ms.is_some() { "streaming" } else { "waiting" },
                "gotBytes": a.got_bytes,
            })
        })
        .collect();

    // ④ 按 Key（窗口内的增量）
    let mut keys: Vec<Value> = per_key
        .iter()
        .map(|(k, (p, c, req))| {
            json!({
                "key": names.get(k).cloned().unwrap_or_else(|| k.clone()),
                "keyId": k,
                "promptTokens": p,
                "completionTokens": c,
                "requests": req,
                "active": active_count_of(k),
            })
        })
        .collect();
    keys.sort_by_key(|v| -(v["promptTokens"].as_i64().unwrap_or(0) + v["completionTokens"].as_i64().unwrap_or(0)));

    // ④ KPI
    let secs = win as f64;
    let kpi = json!({
        "windowSecs": win,
        "requests": tot.requests,
        "rpm": if secs > 0.0 { (tot.requests as f64) * 60.0 / secs } else { 0.0 },
        "inTokPerSec": if secs > 0.0 { tot.prompt as f64 / secs } else { 0.0 },
        "outTokPerSec": if secs > 0.0 { tot.completion as f64 / secs } else { 0.0 },
        "failed": tot.failed,
        "aborted": tot.aborted,
        "queued": tot.queued,
        "active": st.active.len(),
        // 首字中位数：这里给均值（桶里只存了和与个数，中位数要另存数组）
        "ttftAvgMs": if tot.ttft_n > 0 { tot.ttft_sum / tot.ttft_n as i64 } else { 0 },
        "ttftSamples": tot.ttft_n,
    });

    json!({
        "kpi": kpi,
        "series": series.iter().map(|p| json!({
            "t": p.t,
            "inTok": p.prompt,
            "outTok": p.completion,
            "requests": p.requests,
            "failed": p.failed,
        })).collect::<Vec<_>>(),
        "active": active,
        "keys": keys,
        "alerts": alerts(&kpi, &keys, &tot, win),
        "sampledAt": now,
    })
}

/// 一个折线点。
struct Point {
    t: i64,
    prompt: i64,
    completion: i64,
    requests: u32,
    failed: u32,
}

impl Point {
    fn of(t: i64, s: &Slice) -> Self {
        Self {
            t,
            prompt: s.prompt,
            completion: s.completion,
            requests: s.requests,
            failed: s.failed,
        }
    }
}

fn merge(into: &mut Slice, from: &Slice) {
    into.requests += from.requests;
    into.ok += from.ok;
    into.failed += from.failed;
    into.aborted += from.aborted;
    into.queued += from.queued;
    into.prompt += from.prompt;
    into.completion += from.completion;
    into.ttft_sum += from.ttft_sum;
    into.ttft_n += from.ttft_n;
}

fn merge_keys(into: &mut HashMap<String, (i64, i64, u32)>, from: &Slice) {
    if let Some(m) = &from.keys {
        for (k, s) in m {
            let e = into.entry(k.clone()).or_insert((0, 0, 0));
            e.0 += s.prompt;
            e.1 += s.completion;
            e.2 += s.requests;
        }
    }
}

/// Key 名字：先查缓存（5 秒），过期再读一次 keys 台账。
fn key_name(st: &mut State, key_id: &str) -> String {
    let now = now_ms();
    if let Some((name, at)) = st.names.get(key_id) {
        if now - at < 5_000 {
            return name.clone();
        }
    }
    let resolved = crate::modules::gateway_keys::list()
        .ok()
        .and_then(|v| {
            v.as_array()
                .and_then(|arr| {
                    arr.iter()
                        .find(|k| k.get("id").and_then(Value::as_str) == Some(key_id))
                        .and_then(|k| k.get("name").and_then(Value::as_str))
                        .map(str::to_string)
                })
                // `list()` 可能返回 `{ keys: [...] }`
                .or_else(|| {
                    v.get("keys")
                        .and_then(Value::as_array)
                        .and_then(|arr| {
                            arr.iter()
                                .find(|k| k.get("id").and_then(Value::as_str) == Some(key_id))
                                .and_then(|k| k.get("name").and_then(Value::as_str))
                                .map(str::to_string)
                        })
                })
        })
        .unwrap_or_else(|| {
            // 名字查不到就显示 id 前 8 位，至少还能对上号
            key_id.chars().take(8).collect()
        });
    st.names.insert(key_id.to_string(), (resolved.clone(), now));
    resolved
}

// ---------------------------------------------------------------------------
// P2：阈值告警
// ---------------------------------------------------------------------------

/// 单 Key 在窗口内的 token 速率超过这个值就提示（tok/min）。
const KEY_FAST_TOK_PER_MIN: f64 = 300_000.0;
/// 窗口内请求数达到这个量级才判断失败率（样本太少不下结论）。
const FAIL_MIN_REQUESTS: u32 = 5;
/// 失败率告警阈值。
const FAIL_RATE: f64 = 0.2;
/// 并发活跃超过这个数提醒（一般意味着客户端在并发跑多个会话）。
const ACTIVE_HIGH: usize = 8;

fn alerts(kpi: &Value, keys: &[Value], tot: &Slice, win: u64) -> Vec<Value> {
    let mut out = Vec::new();
    let requests = tot.requests;

    if requests >= FAIL_MIN_REQUESTS {
        let rate = tot.failed as f64 / requests as f64;
        if rate >= FAIL_RATE {
            out.push(json!({
                "level": "error",
                "kind": "fail_rate",
                "text": format!(
                    "失败率 {:.0}%（{} / {} 次请求）",
                    rate * 100.0,
                    tot.failed,
                    requests
                ),
                "hint": "看下方「最近错误」或在网关日志里查具体错误码",
            }));
        }
    }

    if tot.queued > 0 {
        out.push(json!({
            "level": "warn",
            "kind": "queued",
            "text": format!("窗口内有 {} 次撞上上游排队", tot.queued),
            "hint": "上游资源受限，不是本机问题；排队不计费",
        }));
    }

    let secs = win as f64;
    if secs > 0.0 {
        for k in keys {
            let tok = k["promptTokens"].as_i64().unwrap_or(0) + k["completionTokens"].as_i64().unwrap_or(0);
            let per_min = tok as f64 * 60.0 / secs;
            if per_min >= KEY_FAST_TOK_PER_MIN {
                out.push(json!({
                    "level": "warn",
                    "kind": "key_fast",
                    "text": format!(
                        "Key「{}」约 {:.0}k tok/min",
                        k["key"].as_str().unwrap_or("?"),
                        per_min / 1000.0
                    ),
                    "hint": "确认是不是某个客户端在刷；长上下文每轮都会重发全量历史",
                }));
            }
        }
    }

    let active = kpi["active"].as_u64().unwrap_or(0) as usize;
    if active >= ACTIVE_HIGH {
        out.push(json!({
            "level": "info",
            "kind": "active_high",
            "text": format!("当前 {active} 条请求在并发"),
            "hint": "并发高时上游更容易排队，首字延迟会变长",
        }));
    }

    out
}

/// 单测用的**全局串行锁**：实时监控是进程级全局状态，多个测试模块都会碰它，
/// 不串行就会互相把对方的计数搅进来（`before/after` 增量断言直接失真）。
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 单测用：把某条活跃条目的开始时刻往前挪（验证过期清扫）。
#[cfg(test)]
pub fn debug_age(id: u64, ms: i64) {
    let mut st = lock();
    if let Some(a) = st.active.iter_mut().find(|a| a.id == id) {
        a.started_ms -= ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这些用例共享进程级全局状态，必须串行跑（见 `super::test_lock`）。
    fn tlock() -> std::sync::MutexGuard<'static, ()> {
        super::test_lock()
    }

    fn sum(field: &str) -> i64 {
        snapshot(60)["series"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p[field].as_i64().unwrap_or(0))
            .sum()
    }

    fn alert_kinds() -> Vec<String> {
        snapshot(60)["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["kind"].as_str().unwrap_or("").to_string())
            .collect()
    }

    /// 一条完整流程：登记 → 首字 → 输出 → 收尾，快照必须能读到（用增量断言，避免被别的用例污染）。
    #[test]
    fn finish_lands_in_window() {
        let _g = tlock();
        let b0 = snapshot(60);
        let before_req = b0["kpi"]["requests"].as_u64().unwrap();
        let before_failed = b0["kpi"]["failed"].as_u64().unwrap();
        let before_active = b0["kpi"]["active"].as_u64().unwrap();
        let before_in = sum("inTok");

        let id = start("m1", "trae-cn", Some("k1"));
        first_byte(id);
        note_output(id, 1024);
        finish(id, true, Some(100), Some(10), Some(300), false);

        let s = snapshot(60);
        assert_eq!(s["kpi"]["requests"].as_u64().unwrap() - before_req, 1);
        assert_eq!(
            s["kpi"]["failed"].as_u64().unwrap(),
            before_failed,
            "这条是成功的，不该增加失败数"
        );
        assert_eq!(
            s["kpi"]["active"].as_u64().unwrap(),
            before_active,
            "收尾后活跃应当回到原值"
        );
        assert_eq!(s["series"].as_array().unwrap().len(), 60);
        assert_eq!(sum("inTok") - before_in, 100);
    }

    /// 幂等：同一条 `finish` 两次只算一次（换号重试 / 守卫兜底后重复调用）。
    #[test]
    fn finish_is_idempotent() {
        let _g = tlock();
        let before = snapshot(60)["kpi"]["requests"].as_u64().unwrap();
        let id = start("m2", "wb", None);
        finish(id, true, Some(1), Some(1), None, false);
        finish(id, true, Some(1), Some(1), None, false);
        assert_eq!(
            snapshot(60)["kpi"]["requests"].as_u64().unwrap() - before,
            1,
            "第二次 finish 不该再计一次"
        );
    }

    /// 守卫兜底：没走 finish 的请求必须从活跃里消失（否则「正在跑」只增不减）。
    #[test]
    fn guard_prevents_active_leak() {
        let _g = tlock();
        let before = snapshot(60)["kpi"]["active"].as_u64().unwrap();
        {
            let id = start("m3", "qoder", None);
            let _g2 = ActiveGuard(id);
            assert!(snapshot(60)["kpi"]["active"].as_u64().unwrap() >= before + 1);
        }
        assert_eq!(snapshot(60)["kpi"]["active"].as_u64().unwrap(), before);
    }

    /// 移交后的守卫**不许**再摘条目：流式请求把 body 交给 hyper 后，函数返回不代表跑完
    /// （2026-11-04：这条不成立时，流式一开始就回「0 条正在跑」）。
    #[test]
    fn disarmed_guard_hands_over_lifetime() {
        let _g = tlock();
        let before = snapshot(60)["kpi"]["active"].as_u64().unwrap();
        let id = start("m9", "trae-cn", None);
        {
            let mut g = ActiveGuard(id);
            g.disarm();
        }
        assert_eq!(
            snapshot(60)["kpi"]["active"].as_u64().unwrap(),
            before + 1,
            "移交后条目应当还在跑"
        );
        abandon(id);
        assert_eq!(snapshot(60)["kpi"]["active"].as_u64().unwrap(), before);
    }

    /// 活跃条目里能看到「等首字 / 输出中」两档，以及已收字节。
    #[test]
    fn active_entry_reports_phase_and_bytes() {
        let _g = tlock();
        let id = start("m4", "trae-cn", None);
        let find = || {
            snapshot(60)["active"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["model"] == json!("m4"))
                .cloned()
        };
        let row = find().expect("应当出现在活跃列表里");
        assert_eq!(row["phase"], json!("waiting"));
        assert_eq!(row["gotBytes"], json!(0));

        first_byte(id);
        note_output(id, 2048);
        let row = find().unwrap();
        assert_eq!(row["phase"], json!("streaming"));
        assert_eq!(row["gotBytes"], json!(2048));
        abandon(id);
    }

    /// 失败率要高到一定程度才报（一条失败不刷红）。
    #[test]
    fn fail_rate_alert_needs_a_sample() {
        let _g = tlock();
        let before = snapshot(60)["kpi"]["requests"].as_u64().unwrap();
        let id = start("m5", "trae-cn", None);
        finish(id, false, None, None, None, false);
        // 窗口内样本不足（< 5 条）时不该有失败率告警
        if snapshot(60)["kpi"]["requests"].as_u64().unwrap() - before < 5 {
            assert!(!alert_kinds().contains(&"fail_rate".to_string()));
        }

        for _ in 0..5 {
            let id = start("m5", "trae-cn", None);
            finish(id, false, None, None, None, false);
        }
        assert!(
            alert_kinds().contains(&"fail_rate".to_string()),
            "{:?}",
            alert_kinds()
        );
    }

    /// 排队单独一档：它不是失败，别混进失败率。
    #[test]
    fn queued_is_its_own_alert() {
        let _g = tlock();
        let id = start("m6", "trae-cn", None);
        finish(id, true, Some(1), Some(1), None, true);
        let kinds = alert_kinds();
        assert!(kinds.contains(&"queued".to_string()), "{kinds:?}");
    }

    /// 中断单独一格：**不算一次请求**，否则窗口里的「失败率」会被用户点停止的动作污染。
    #[test]
    fn abandon_counts_as_aborted_not_request() {
        let _g = tlock();
        let b = snapshot(60);
        let before_abort = b["kpi"]["aborted"].as_u64().unwrap();
        let before_req = b["kpi"]["requests"].as_u64().unwrap();
        let id = start("m10", "trae-cn", None);
        abandon(id);
        let s = snapshot(60);
        assert_eq!(s["kpi"]["aborted"].as_u64().unwrap(), before_abort + 1);
        assert_eq!(
            s["kpi"]["requests"].as_u64().unwrap(),
            before_req,
            "中断不该被算成一次完成的请求"
        );
    }

    /// `finish` 之后再 `abandon`（守卫兜底必调）不能重复记中断。
    #[test]
    fn abandon_after_finish_is_not_double_counted() {
        let _g = tlock();
        let before = snapshot(60)["kpi"]["aborted"].as_u64().unwrap();
        let id = start("m11", "trae-cn", None);
        finish(id, true, Some(1), Some(1), None, false);
        abandon(id);
        assert_eq!(snapshot(60)["kpi"]["aborted"].as_u64().unwrap(), before);
    }

    /// 卡死的条目（没走 finish/abandon）必须被 20 分钟清扫掉，不能永远显示「正在跑」。
    #[test]
    fn stale_active_is_swept() {
        let _g = tlock();
        let before = snapshot(60)["kpi"]["active"].as_u64().unwrap();
        let id = start("m8", "trae-cn", None);
        assert_eq!(snapshot(60)["kpi"]["active"].as_u64().unwrap(), before + 1);
        // 挪到 21 分钟前
        debug_age(id, 21 * 60 * 1000);
        assert_eq!(
            snapshot(60)["kpi"]["active"].as_u64().unwrap(),
            before,
            "过期条目应被清扫"
        );
    }

    /// 1 小时窗口走分钟桶，点数应当是 60。
    #[test]
    fn hour_window_uses_minute_buckets() {
        let _g = tlock();
        let s = snapshot(3600);
        assert_eq!(s["kpi"]["windowSecs"], json!(3600));
        assert_eq!(s["series"].as_array().unwrap().len(), 60);
    }

    /// 窗口换算：60 秒窗口里 1 条请求 = 1 req/min，速率按窗口秒数折算。
    #[test]
    fn rates_are_per_window() {
        let _g = tlock();
        let before = snapshot(60)["kpi"]["requests"].as_u64().unwrap() as f64;
        let id = start("m7", "trae-cn", None);
        finish(id, true, Some(600), Some(60), None, false);
        let s = snapshot(60);
        let req = s["kpi"]["requests"].as_u64().unwrap() as f64;
        assert!((req - before) >= 1.0);
        // rpm = requests * 60 / 60 = requests
        assert!((s["kpi"]["rpm"].as_f64().unwrap() - req).abs() < 0.001);
    }
}
