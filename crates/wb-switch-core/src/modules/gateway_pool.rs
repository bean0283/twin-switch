//! 积分共享 · 账号池与选号。
//!
//! 把三个产品的账号摊平成一个池子，按**「谁先过期就用谁」**排序
//! —— 这一条是用户要的核心：Trae / WorkBuddy / Qoder 谁的积分先到期，
//! 就让网关先花它的，避免积分过期作废。
//!
//! 数据来源（都是 0.3.x 已有的本机文件，不新增采集）：
//!
//! | 产品 | 账号 | 额度 | 到期 |
//! | --- | --- | --- | --- |
//! | Qoder | `qoder/accounts.{cn,global}.json` | `credits` | `credits_expire_at` |
//! | WorkBuddy | `workbuddy-accounts.json` | 未知（`None`，不编） | `expiresAt`（令牌到期） |
//! | Trae | `trae/vault/<client>/oauth_*/profile.json` | `credits` | `credit_packs[].expire_at` 最早值 |
//!
//! ⚠️ 两条判据：
//! 1. **额度未知 ≠ 为 0**：查不到就留 `None`，不许填 0（填 0 等于把一个能用的号永久排除）。
//! 2. **没有凭据的账号不进池**：否则选号会选到一个发不出请求的空壳。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

use crate::modules::config::store_dir;
use crate::modules::qoder_accounts::{self, QoderAccount};
use crate::modules::qoder_realm::QoderRegion;

/// 池子里的一个账号（界面与选号共用）。
#[derive(Debug, Clone, Serialize)]
pub struct PoolEntry {
    /// 全局唯一：产品 + 账号标识。
    pub key: String,
    /// `trae` / `workbuddy` / `qoder`
    pub product: String,
    /// `trae-cn` / `wb-cn` / `qoder-cn` …
    pub scope: String,
    pub label: String,
    /// 剩余额度；`None` = 未知。
    pub remaining: Option<f64>,
    /// 到期时间（毫秒）。`None` = 没有明确到期。
    pub expires_at: Option<i64>,
    /// 能不能用；不能时 `reason` 说明原因。
    pub usable: bool,
    pub reason: Option<String>,
    /// 冷却截止（毫秒）—— 上游报错后由 [`apply_error`] 写入，`None` = 没在冷却。
    pub cooldown_until: Option<i64>,
    /// **按模型**记的限额（照 changexbc/workbuddy-switch 的 `limits.rs` 台账口径）。
    ///
    /// 上游 429 说的通常是「这个号的这个模型今天用完了」，不是「这个号废了」——
    /// 只按号冷却会导致「一个模型用完就把整个号打进冷宫」，别的模型明明还能用。
    /// 这里逐条列出受限模型，选号时按当前请求的模型精确跳过。
    pub limited_models: Vec<ModelLimit>,
}

/// 一个账号在某个模型上的限额（`resetAt` 由上游文案给出）。
#[derive(Debug, Clone, Serialize)]
pub struct ModelLimit {
    pub model: String,
    /// 官方给的重置时刻（毫秒）。
    pub reset_at: i64,
    /// 冷却类型：`balance_exhausted` / `rate_limited` …
    pub kind: String,
}

impl PoolEntry {
    /// 紧迫度：越小越该先用。没有到期时间的排最后。
    fn urgency(&self) -> f64 {
        match self.expires_at {
            Some(ms) => ms as f64,
            None => f64::MAX,
        }
    }
}

// ---------------------------------------------------------------------------
// 会话粘性：同一段对话固定用同一个账号
// ---------------------------------------------------------------------------

/// 会话 → 账号 key 的绑定表（进程内）。
///
/// **为什么必须有这张表**：Trae 的上游会话 ID 由 `session_seed(model, uid, …)` 派生，
/// **uid 参与指纹**。所以同一段对话中途换号 ⇒ 上游认为「这是另一个会话」⇒ 前缀缓存全失效，
/// 而且用户会看到「会话像断了一样」（上下文还在，但上游那边的连续性是断的）。
///
/// 用户的原始要求就是这个：「如果是担心同一个会话，切换模型会出现会话中断，
/// 应当在新会话的时候就要切换」。于是规则定为：
/// * **一段对话第一次请求** → 按积分最早过期选号（省钱、先用快过期的）；
/// * **同一段对话后续请求** → 锁定第一次选中的那个账号，不换（除非它不可用/冷却）。
///
/// 会话指纹用 [`crate::modules::gateway_stream::session_seed`] 的**前两段**（模型 + 首条 user），
/// 不含 uid —— 正好就是「这段对话是谁」。
fn sticky_path() -> std::path::PathBuf {
    crate::modules::config::store_dir().join("gateway-sticky.json")
}

/// 进程内绑定表（读盘一次、写时落盘，重启后仍能接上同一段对话）。
fn sticky_map() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(|| {
        let loaded = std::fs::read_to_string(sticky_path())
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, String>>(&t).ok())
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

/// 绑定表上限：只留最近这么多段对话，避免无限增长（LRU 用「最近写入」近似）。
const STICKY_MAX: usize = 512;

/// 查这段对话绑定的账号 key。
pub fn sticky_lookup(conv: &str) -> Option<String> {
    sticky_map().lock().ok()?.get(conv).cloned()
}

/// 记下这段对话用的账号（覆盖写：换号后以最新的为准）。
pub fn sticky_bind(conv: &str, key: &str) {
    if conv.is_empty() || key.is_empty() {
        return;
    }
    let Ok(mut m) = sticky_map().lock() else { return };
    // 近似 LRU：超限时按插入顺序丢最早的一批（HashMap 无顺序，这里直接截断到上限的 3/4）。
    if m.len() >= STICKY_MAX && !m.contains_key(conv) {
        let keep: Vec<(String, String)> = m.iter().skip(m.len() / 4).map(|(k, v)| (k.clone(), v.clone())).collect();
        m.clear();
        m.extend(keep);
    }
    m.insert(conv.to_string(), key.to_string());
    if let Ok(text) = serde_json::to_string(&*m) {
        let _ = std::fs::write(sticky_path(), text);
    }
}

/// 忘掉这段对话的绑定（那个账号不可用时调用，下次重新按紧迫度选）。
pub fn sticky_forget(conv: &str) {
    let Ok(mut m) = sticky_map().lock() else { return };
    if m.remove(conv).is_some() {
        if let Ok(text) = serde_json::to_string(&*m) {
            let _ = std::fs::write(sticky_path(), text);
        }
    }
}

/// 全部账号（已排序：最紧迫在前）。
pub fn snapshot() -> Vec<PoolEntry> {
    let mut out = Vec::new();
    out.extend(qoder_entries());
    out.extend(workbuddy_entries());
    out.extend(trae_entries());
    // 「设为默认」的账号优先（用户显式指定过，就该先用它），其余按最紧迫。
    let defaults = [
        crate::modules::qoder_accounts::default_uid(
            crate::modules::qoder_realm::QoderRegion::Cn,
        ),
        crate::modules::qoder_accounts::default_uid(
            crate::modules::qoder_realm::QoderRegion::Global,
        ),
    ];
    let is_default = |e: &PoolEntry| {
        defaults
            .iter()
            .flatten()
            .any(|d| e.key.ends_with(&format!(":{d}")))
    };

    out.sort_by(|a, b| {
        // ⚠️⚠️ **紧迫度优先于「默认账号」**（2026-10-26 用户实测纠正）。
        //
        // 原来的顺序是「可用的 → 默认账号 → 紧迫度」，于是「设为默认」的号永远排在
        // 所有快过期的号前面 —— 用户看到的现象正是「积分没有按最早过期的消耗，
        // 反而一直消耗当前登录的那个号」（Qoder 切号时会 `set_default`）。
        // 积分的意义就是**先用会过期的**，让默认账号压过它等于把这条规则废掉。
        // 现在：可用的在前 → 越紧迫越靠前 → 只有到期时间完全相同时才让默认账号占先。
        b.usable
            .cmp(&a.usable)
            .then_with(|| {
                a.urgency()
                    .partial_cmp(&b.urgency())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| is_default(b).cmp(&is_default(a)))
    });
    with_cooldowns(out, None)
}

/// 只有可用的。
pub fn usable() -> Vec<PoolEntry> {
    snapshot().into_iter().filter(|e| e.usable).collect()
}

// ---------------------------------------------------------------- 错误反馈
//
// 照参考实现（buddy-switch-gateway/src/qoder/pool.rs）的四类处理：
// 上游报错后把**冷却**写回内存表，下次选号跳过 —— 否则同一个请求失败后，
// 下一个请求还会选同一个账号再撞一次。


/// 冷却表落盘位置。
fn cooldowns_path() -> std::path::PathBuf {
    crate::modules::config::store_dir().join("gateway-cooldowns.json")
}

/// JSON 里存的是字符串，读回来要还原成静态串（kind 只用于展示）。
fn kind_static(kind: &str) -> &'static str {
    match kind {
        "auth_dead" => "auth_dead",
        "balance_exhausted" => "balance_exhausted",
        "rate_limited" => "rate_limited",
        _ => "transient",
    }
}

/// key = 池条目 key（`qoder:<id>` …），value = (解冻时刻, 冷却类型)。
///
/// ⚠️⚠️ **必须跨进程重启活下来**：以前这张表只在内存里，重开程序就清空 ——
/// 刚被上游限流的号立刻又变成「最紧迫、最该先用」，预览照样选中它，用户看到的
/// 就是「说好的换号呢，预览还是那个被限制的账号」（2026-10-24 用户实测两次）。
fn cooldowns() -> &'static Mutex<HashMap<String, (i64, &'static str)>> {
    static T: OnceLock<Mutex<HashMap<String, (i64, &'static str)>>> = OnceLock::new();
    T.get_or_init(|| {
        let mut map: HashMap<String, (i64, &'static str)> = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(cooldowns_path()) {
            if let Ok(serde_json::Value::Object(obj)) =
                serde_json::from_str::<serde_json::Value>(&text)
            {
                let now = now_ms();
                for (k, item) in obj {
                    let until = item.get("until").and_then(|x| x.as_i64()).unwrap_or(0);
                    // 过期的不留：盘上越攒越多没有意义。
                    if until <= now {
                        continue;
                    }
                    let kind = item
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("transient");
                    map.insert(k, (until, kind_static(kind)));
                }
            }
        }
        Mutex::new(map)
    })
}

/// 把当前冷却表写回磁盘（只留未过期的）。
fn persist_cooldowns(map: &HashMap<String, (i64, &'static str)>) {
    use serde_json::json;
    let now = now_ms();
    let mut obj = serde_json::Map::new();
    for (k, (until, kind)) in map {
        // `{key}#err` 是「最后一次错误」的展示用记录（见 [`mark_last_error`]），
        // 不是选号依据 —— 写进盘只会让冷却表越来越脏。
        if *until > now && !k.contains("#err") {
            obj.insert(k.clone(), json!({ "until": until, "kind": kind }));
        }
    }
    let path = cooldowns_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&obj) {
        let _ = std::fs::write(path, text);
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 下一个每日重置窗口（**10:00 UTC+8** = 02:00 UTC，Qoder 的额度每天这时重置）。
fn next_reset_window(now: i64) -> i64 {
    const DAY: i64 = 86_400_000;
    const RESET_AT_UTC: i64 = 2 * 3_600_000;
    let today_utc0 = now.div_euclid(DAY) * DAY;
    let today_reset = today_utc0 + RESET_AT_UTC;
    if now < today_reset { today_reset } else { today_reset + DAY }
}

/// 上游报错后的反馈。返回 `Some((解冻时刻, 类型))` 表示已置冷却；
/// `None` 表示这**不该冷却**（客户端参数错 / 上游排队），调用方只需如实回错。
///
/// 四类（与参考实现同口径）：
/// - `401/403`：凭据死 → 冷却 30 分钟；
/// - `402`：余额耗尽 → **停到下一个每日重置窗口**（短冷却毫无意义，几分钟后又撞）；
/// - `429`：限流 → 冷却 60 秒；
/// - body 含 `10605` / `isQueued`：上游**排队** → 不冷却（账号没问题）；
/// - `5xx` / 传输层（status 0）→ 短冷却 60 秒；其余 4xx → 不冷却。
/// 从上游文案里抠出「何时重置」（`将在 2026-10-24 22:04:42 UTC+8 重置`）。
///
/// 抠不到返回 `None`（调用方按 1 小时兜底）。留 1 分钟余量：正好卡在重置那一秒
/// 发请求，服务端往往还没翻转。
fn reset_at_from_hint(text: &str) -> Option<i64> {
    // ⚠️ 不再靠「将在」定位：真实文案有三种写法都出现过 ——
    //   ① `…将在 2026-10-25 11:12:02 UTC+8 重置，您也可以切换其他模型继续使用。`
    //   ② `…将于 2026-10-25 11:12:02 重置`
    //   ③ JSON 里直接给 `"resetTime":"2026-10-25T11:12:02"`
    // 只要整段里出现一个时间戳，就拿它当重置时刻 —— 引导词变了也不会瞎。
    let flat: String = text
        .chars()
        .map(|c| match c {
            '/' => '-',   // 2026/10/25 → 2026-10-25
            'T' => ' ',   // ISO 的 2026-10-25T11:12:02
            _ => c,
        })
        .collect();
    let chars: Vec<char> = flat.chars().collect();
    for start in 0..chars.len() {
        if !chars[start].is_ascii_digit() {
            continue;
        }
        // 左边界不能还是数字：避免从 `01a16cf7…` 这种长串中间切出假年份。
        if start > 0 && chars[start - 1].is_ascii_digit() {
            continue;
        }
        for len in [19usize, 18, 17, 16] {
            if start + len > chars.len() {
                continue;
            }
            let cand: String = chars[start..start + len].iter().collect();
            let Some(naive) = parse_stamp(&cand) else {
                continue;
            };
            // ⚠️ **优先用文案里的 `UTC+8`**，而不是本机时区。
            // 上游是固定 UTC+8 给的时间，以前按 `chrono::Local` 解释 —— 只有在
            // 恰好 UTC+8 的机器上才对，换台机器就把「11:12 恢复」算成别的时刻。
            let ms = match parse_utc_offset(&flat) {
                Some(offset_ms) => naive.and_utc().timestamp_millis() - offset_ms,
                None => match naive.and_local_timezone(chrono::Local).single() {
                    Some(local) => local.timestamp_millis(),
                    None => continue,
                },
            };
            // 留 1 分钟余量：正好卡在重置那一秒发请求，服务端往往还没翻转。
            return Some(ms + 60_000);
        }
    }
    None
}

/// 文案里的 `UTC±H[:MM]` 偏移（毫秒）。
///
/// 上游两种写法都出现过：`UTC+8`、`UTC+08:00`。抠不到就返回 `None`，
/// 由调用方回落到本机时区（**不是**默认 +8 —— 宁可承认不知道）。
fn parse_utc_offset(text: &str) -> Option<i64> {
    let index = text.find("UTC")?;
    let rest = &text[index + 3..];
    let sign = match rest.chars().next()? {
        '+' => 1i64,
        '-' => -1i64,
        _ => return None,
    };
    let digits: String = rest[1..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':')
        .collect();
    let (hours, minutes) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None => match digits.len() {
            1 | 2 => (digits.parse::<i64>().ok()?, 0),
            4 => (
                digits[..2].parse::<i64>().ok()?,
                digits[2..].parse::<i64>().ok()?,
            ),
            _ => return None,
        },
    };
    Some(sign * (hours * 3600 + minutes * 60) * 1000)
}

fn parse_stamp(s: &str) -> Option<chrono::NaiveDateTime> {
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
    }
    None
}

/// 记录一次上游失败并置冷却。
///
/// **记账粒度按失败性质分叉**（这是「换号换得对不对」的分水岭）：
/// - `balance_exhausted` / `rate_limited` → 记到 **(账号, 模型)** 上。
///   上游原话就是「…重置，**您也可以切换其他模型继续使用**」—— 限的是模型不是账号，
///   按整号停会把一个还能用别的模型的号白白关掉。
/// - `auth_dead` / `transient` → 记到**整号**上。凭据死了、网络炸了跟模型无关。
pub fn apply_error(
    key: &str,
    http_status: u16,
    body_hint: &str,
    model: Option<&str>,
) -> Option<(i64, &'static str)> {
    let (until, kind) = classify(http_status, body_hint, now_ms())?;
    let scoped_model = cooldown_scope(kind, model);
    if let Ok(mut m) = cooldowns().lock() {
        m.insert(cooldown_key(key, scoped_model), (until, kind));
        persist_cooldowns(&m);
    }
    Some((until, kind))
}

/// 这次失败该记在模型上还是整个号上（`None` = 整号）。
fn cooldown_scope<'a>(kind: &str, model: Option<&'a str>) -> Option<&'a str> {
    match kind {
        // 上游明说「您也可以切换其他模型继续使用」⇒ 只是这个模型的额度问题。
        "balance_exhausted" | "rate_limited" => model,
        // 凭据死 / 传输炸跟模型无关：锁整个号，别让别的模型去白撞一次 401。
        _ => None,
    }
}

/// 「上游在排队」的唯一判据。
///
/// ⚠️ 三处必须同源：冷却归类（[`classify`]）、是否换号（`gateway_server::should_failover`）、
/// 以及文案展示。以前只认 `10605` / `isQueued`，而 Qoder 私有协议回的是
/// `QUEUED|30|上游排队中…`（我们已经把信封里的 `isQueued` 转成了这句中文标记），
/// 于是排队被当成 429 限流：既冷却了账号，又换了一轮号（2026-10-24 实测）。
pub fn is_queued_hint(text: &str) -> bool {
    let m = text.to_ascii_lowercase();
    m.contains("10605") || m.contains("isqueued") || m.contains("queued") || text.contains("排队")
}

/// 纯判定：这次失败该冷却多久、算什么类型（不碰任何全局状态，便于单测）。
fn classify(http_status: u16, body_hint: &str, now: i64) -> Option<(i64, &'static str)> {
    // 排队（10605 / isQueued）通常裹在 401/403 信封里 —— 必须先查它，
    // 否则会把「账号没问题、只是排队」误判成「凭据死」打死。
    let hint = body_hint.to_ascii_lowercase();
    if is_queued_hint(body_hint) {
        return None;
    }

    // ★★ 额度 / 频率耗尽：**按上游给的重置时间**冷却，不能一律 60 秒。
    //
    // 实测两种文案：
    //   Trae `6804`：「您的使用量已超出频率限制，将在 2026-10-24 22:04:42 UTC+8 重置」
    //   WorkBuddy 429：「您的使用量已超出频率限制，将在 2026-10-25 11:12:02 UTC+8 重置，
    //                    您也可以切换其他模型继续使用。 (01a16cf7…/71e874b5-…)」
    // 都是**当日额度用完**，60 秒后再试必然还是 429。以前按 429 记 60 秒，
    // 于是「自动换号」在第一轮之后又回到这个号，用户看到的就是「说好的换号呢，
    // 预览还选它」（2026-10-24 用户实测两次）。
    //
    // 判据放宽成「能解析出重置时刻就当额度耗尽」：不再挑文案里的关键词，
    // 上游换个说法（`将于…` / JSON `resetTime`）也不会退化成 60 秒。
    let quota_words = hint.contains("6804")
        || hint.contains("频率限制")
        || body_hint.contains("使用量已超出")
        || body_hint.contains("额度不足")
        || body_hint.contains("额度已用完")
        || body_hint.contains("余额不足")
        || hint.contains("quota exceeded");
    if let Some(until) = reset_at_from_hint(body_hint) {
        if quota_words || matches!(http_status, 402 | 429) {
            return Some((until, "balance_exhausted"));
        }
    }
    if quota_words {
        // 说了额度但没说什么时候恢复：按 1 小时兜底（绝不编一个精确时间出来）。
        return Some((now + 60 * 60_000, "balance_exhausted"));
    }

    // ★ WorkBuddy `11128 Illegal API invocation from an unapproved channel`：
    // 上游按**频道/账号**校验，换一份同样的请求体、换一个号就正常（2026-10-26 真机探针
    // 7 种请求形状全部 200，只有这条错误凭空出现）。所以它是**账号级**判据：
    // 记 10 分钟整号冷却（`auth_dead` 会锁整个号），期间直接跳过它、自动换号。
    //
    // 为什么不记更长（比如 30 分钟，与 401 一致）：万一是「请求形状」引起的，
    // 一次长锁会把本来好好的号封半小时；10 分钟足够躲过一轮，也不会长期误伤。
    if hint.contains("11128") || hint.contains("unapproved channel") {
        return Some((now + 10 * 60_000, "auth_dead"));
    }

    match http_status {
        0 => Some((now + 60_000, "transient")),
        401 | 403 => Some((now + 30 * 60_000, "auth_dead")),
        402 => Some((next_reset_window(now), "balance_exhausted")),
        429 => Some((now + 60_000, "rate_limited")),
        500..=599 => Some((now + 60_000, "transient")),
        _ => None,
    }
}

/// 冷却键：`{账号key}` = 整号级（凭据死 / 传输炸）；`{账号key}@{模型}` = 只锁这个模型。
///
/// 抄自 changexbc/workbuddy-switch 的台账口径：它的键是 `(账号, 模型)`。
/// 我们额外保留整号级 —— 401 / 网络错跟模型无关，锁整个号才对。
fn cooldown_key(key: &str, model: Option<&str>) -> String {
    match model {
        Some(m) if !m.trim().is_empty() => format!("{key}@{}", m.trim()),
        _ => key.to_string(),
    }
}

/// 条目当前是否在冷却中。
///
/// `model` 为 `Some` 时同时认「该模型的冷却」和「整号冷却」，取较晚的那个 ——
/// 两个都指向同一个事实：这个号现在接不了这个请求。
pub fn cooling_until(key: &str, model: Option<&str>) -> Option<i64> {
    let now = now_ms();
    let Ok(m) = cooldowns().lock() else {
        return None;
    };
    let get = |k: String| -> Option<i64> {
        m.get(&k)
            .copied()
            .filter(|(until, _)| *until > now)
            .map(|(until, _)| until)
    };
    let whole = get(key.to_string());
    match model {
        Some(name) => match (whole, get(cooldown_key(key, Some(name)))) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        },
        None => whole,
    }
}

/// 某账号所有**模型级**限额（界面展示「哪些模型要等到几点」）。
pub fn model_limits(key: &str) -> Vec<ModelLimit> {
    let now = now_ms();
    let prefix = format!("{key}@");
    let Ok(m) = cooldowns().lock() else {
        return Vec::new();
    };
    let mut out: Vec<ModelLimit> = m
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .filter(|(_, (until, _))| *until > now)
        .map(|(k, (until, kind))| ModelLimit {
            model: k[prefix.len()..].to_string(),
            reset_at: *until,
            kind: (*kind).to_string(),
        })
        .collect();
    // 最早解锁的排前面（最有行动价值）—— 与参考实现的排序一致。
    out.sort_by_key(|l| l.reset_at);
    out
}

pub fn now_ms_pub() -> i64 {
    now_ms()
}

/// 记录最后一次上游错误（给「积分共享」页展示：谁、何时、为什么被冷却）。
pub fn mark_last_error(key: &str, message: &str, kind: &str, seconds_left: i64) {
    let Ok(mut m) = cooldowns().lock() else { return };
    m.insert(
        format!("{key}#err"),
        (
            now_ms() + seconds_left * 1000,
            // 以前这里只认 auth_dead、其余一律 transient —— 于是界面上
            // 「额度用完」和「网络抖一下」长得一模一样，用户没法判断该不该换号。
            kind_static(kind),
        ),
    );
    if let Ok(mut g) = LAST_ERROR.get_or_init(|| Mutex::new(String::new())).lock() {
        *g = format!("{key}｜{kind}｜{seconds_left}s｜{message}");
    }
}

static LAST_ERROR: OnceLock<Mutex<String>> = OnceLock::new();

/// 最后一次上游错误（`None` = 还没发生过）。
pub fn last_error() -> Option<String> {
    let g = LAST_ERROR.get_or_init(|| Mutex::new(String::new())).lock().ok()?;
    (!g.is_empty()).then(|| g.clone())
}

/// 冷却中的条目排到最后；`usable` 不动（冷却不等于账号坏了，界面要能看到）。
///
/// `model` = 当前要用的模型：给了就按「整号冷却 或 该模型冷却」算有效冷却，
/// 这样「只在这个模型上被限」的号会被排到后面，而它在别的模型上仍然能用。
fn with_cooldowns(mut entries: Vec<PoolEntry>, model: Option<&str>) -> Vec<PoolEntry> {
    for e in entries.iter_mut() {
        e.cooldown_until = cooling_until(&e.key, model);
        e.limited_models = model_limits(&e.key);
    }
    entries.sort_by(|a, b| {
        a.cooldown_until
            .unwrap_or(0)
            .cmp(&b.cooldown_until.unwrap_or(0))
            .then_with(|| b.usable.cmp(&a.usable))
    });
    entries
}

// ---------------------------------------------------------------- Qoder

fn qoder_entries() -> Vec<PoolEntry> {
    let Ok(list) = qoder_accounts::list_all() else {
        return Vec::new();
    };
    list.into_iter()
        .map(|a: QoderAccount| {
            let usable = a.is_usable();
            PoolEntry {
                key: format!("qoder:{}", a.id),
                product: "qoder".into(),
                scope: format!("qoder-{}", a.region.as_str()),
                label: a.label(),
                remaining: a.credits,
                expires_at: a.credits_expire_at,
                usable,
                cooldown_until: None,
                limited_models: Vec::new(),
                reason: if usable {
                    None
                } else if a.disabled {
                    Some("已停用".into())
                } else {
                    Some("还没有凭据".into())
                },
            }
        })
        .collect()
}

// ---------------------------------------------------------------- WorkBuddy

/// 从 0.3.4 已有的额度缓存里取 WorkBuddy 的「剩余 / 积分到期」。
///
/// 缓存由账号页的「查额度」写（`workbuddy_credits`），这里**只读**。
/// 取不到就返回空表 —— 池子里仍然显示「未知」，绝不填 0。
fn workbuddy_credit_map() -> std::collections::BTreeMap<String, (Option<f64>, Option<i64>)> {
    let mut out = std::collections::BTreeMap::new();
    let v = crate::modules::workbuddy_credits::cached();
    let Some(list) = v.get("accounts").and_then(Value::as_array) else {
        return out;
    };
    for item in list {
        if item.get("ok").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let remaining = item.get("totalRemaining").and_then(Value::as_f64);
        // `soonestExpireAt` 是上游算好的「最早作废时间」，比我们自己扫 activeResources 可靠。
        let expires = item.get("soonestExpireAt").and_then(Value::as_i64);
        // ⚠️ 账号信息嵌在 `account` 里（不是条目顶层）——踩过一次：顶层取 uid 恒为 None，
        //    结果池子里永远匹配不上、全显示「未知」。
        let Some(acc) = item.get("account") else {
            continue;
        };
        for key in ["uid", "id"] {
            if let Some(k) = acc.get(key).and_then(Value::as_str) {
                if !k.is_empty() {
                    out.insert(k.to_string(), (remaining, expires));
                }
            }
        }
    }
    out
}

fn workbuddy_entries() -> Vec<PoolEntry> {
    let path: PathBuf = store_dir().join("workbuddy-accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(list) = v.get("accounts").and_then(Value::as_array) else {
        return Vec::new();
    };

    let credits = workbuddy_credit_map();

    list.iter()
        .filter_map(|item| {
            let uid = item.get("uid").and_then(Value::as_str)?.to_string();
            let acc_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let domain = item.get("domain").and_then(Value::as_str).unwrap_or_default();
            let scope = if domain.contains("cn") { "wb-cn" } else { "wb-global" };
            let label = item
                .get("nickname")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&uid)
                .to_string();
            let token = item
                .get("access_token")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // 额度到期优先于「登录态到期」——「谁先过期用谁」比的是积分。
            let token_expires = item.get("expiresAt").and_then(Value::as_i64);
            let credit = credits.get(&uid).or_else(|| credits.get(acc_id));
            let remaining = credit.and_then(|c| c.0);
            let expires_at = credit.and_then(|c| c.1).or(token_expires);
            let now = qoder_accounts::now_ms();
            let expired = expires_at.map(|t| t <= now).unwrap_or(false);
            // ★ 服务端已作废的凭据：`exp` 还在未来（被踢下线时不会改 exp），
            //   但这种号切过去必然 401 ⇒ 不能算「可用」（真机：130****9226 到期 21 天后却已失效）。
            //   结论保留 7 天；过期后允许重新验证一次（用户可能已经重新登录过）。
            let auth_error = item
                .get("authError")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let auth_error_at = item.get("authErrorAt").and_then(Value::as_i64).unwrap_or(0);
            let revoked = auth_error.is_some() && now - auth_error_at < 7 * 86_400_000;
            let usable = !token.trim().is_empty() && !expired && !revoked;
            Some(PoolEntry {
                key: format!("workbuddy:{uid}"),
                product: "workbuddy".into(),
                scope: scope.into(),
                label,
                remaining,
                expires_at,
                usable,
                cooldown_until: None,
                limited_models: Vec::new(),
                reason: if usable {
                    None
                } else if revoked {
                    Some(format!(
                        "{}，需重新登录",
                        auth_error.unwrap_or("登录态已被服务端作废")
                    ))
                } else if expired {
                    Some("登录态已过期，需重新登录".into())
                } else {
                    Some("还没有凭据".into())
                },
            })
        })
        .collect()
}

// ---------------------------------------------------------------- Trae

/// 一个 Trae 账号**真正能被网关使用**的凭据（唯一出口）。
///
/// ⚠️⚠️ 这张结构存在的理由，是 2026-10-26 的一次真实事故：
/// 积分池判断「这个号能不能用」看的是 `oauth.json` 里有没有 `privateKeyPEM`，
/// 而真正发请求要的是 `User/globalStorage/storage.json` 里能解开的 `iCubeAuthInfo`
/// —— **两个判据不同源**。于是池子把「有私钥但没有登录态」的号当成可用、按紧迫度选中，
/// 发出去却 502「找不到账号 xxx 的可用登录态」，连换 3 个号全失败。
///
/// 更隐蔽的一层：**uid 解析也有两套**。池子用「meta → profile → 解 JWT → 目录名」四段回退，
/// 而旧的 `trae_cred` 只认 meta/oauth.id；`solo-cn/oauth_717036` 的 meta 里没有
/// `verified_uid`，池子能解出 842681600717036，发送端却解不出 ⇒ 按 uid 找不到。
///
/// 所以现在只留**一个**函数：uid + jwt + 设备标识一起给，池子和发送端都只认它。
pub struct TraeLogin {
    pub uid: String,
    pub jwt: String,
    pub device_id: String,
    pub machine_id: String,
    /// 账号目录（取 profile.json 算额度/到期用）。
    pub dir: std::path::PathBuf,
}

/// 扫 vault 下所有 Trae 账号目录（`trae-cn` / `solo-cn`）。
fn trae_vault_dirs() -> Vec<std::path::PathBuf> {
    let vault = store_dir().join("trae").join("vault");
    let mut dirs = Vec::new();
    // ⚠️ 2026-10-26 用户决定：**弃用 solo-cn**，网关只认 trae-cn。
    // 老机器上可能还留着 solo-cn 目录，但它的账号与 trae-cn 是同一批（uid 相同），
    // 两边都扫只会让同一个号出现两次、并让「按 uid 取凭据」取到缺 meta 的那一份。
    for client in ["trae-cn"] {
        let Ok(rd) = std::fs::read_dir(vault.join(client)) else {
            continue;
        };
        for e in rd.flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }
    dirs
}

/// 解析某个账号目录的 uid（四段回退，顺序不能乱）。
///
/// ⚠️ 这个顺序就是池子原来那套：`meta.verified_uid` → `profile.user_id` →
/// 从 `storage.json` 的 JWT 里取 → 目录名兜底。**发送端必须用同一个结果**，
/// 否则「池子按 uid 选中的号」在发送端找不到（2026-10-26 的事故）。
fn trae_dir_uid(dir: &std::path::Path) -> String {
    let meta = read_json(&dir.join("meta.json"));
    let profile = read_json(&dir.join("profile.json"));
    let oauth = read_json(&dir.join("oauth.json"));
    meta.as_ref()
        .and_then(|v| v.get("verified_uid"))
        .and_then(|v| {
            v.as_i64()
                .map(|n| n.to_string())
                .or_else(|| v.as_str().map(str::to_string))
        })
        .or_else(|| {
            // ⚠️ `user_id` 在真实 profile.json 里有**两种类型**：数字（`925158341882023`）
            // 和字符串（同一份文件被不同版本写成 `"925158341882023"`）。
            // 原来只认 `as_i64`，字符串就会被跳过、一路掉到目录名兜底
            // （实测 `oauth_882023` 因此变成 `trae:oauth_882023`，与账号页的 uid 对不上）。
            profile
                .as_ref()
                .and_then(|v| v.get("user_id"))
                .and_then(|v| {
                    v.as_i64()
                        .map(|n| n.to_string())
                        .or_else(|| v.as_str().map(str::to_string))
                })
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            oauth
                .as_ref()
                .and_then(|v| v.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| uid_from_storage(dir))
        .unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
}

/// 一个账号目录能否产出可用凭据 ⇒ `Some(TraeLogin)`。
///
/// `None` 的三种原因（都算「不可用」，但界面要把原因说清）：
/// 没有 `storage.json` / 里面没有 `iCubeAuthInfo://icube.cloudide` / 解得开但 token 空。
pub fn trae_vault_login(dir: &std::path::Path) -> Option<TraeLogin> {
    let oauth = read_json(&dir.join("oauth.json"));
    let storage = read_json(&dir.join("User").join("globalStorage").join("storage.json"))?;
    let enc = storage
        .get("iCubeAuthInfo://icube.cloudide")
        .and_then(Value::as_str)?;
    let auth = crate::modules::trae_km::decrypt_km_json(enc)?;
    let token = auth.get("token").and_then(Value::as_str)?;
    let jwt = crate::modules::trae_km::strip_cloud_prefix(token).to_string();
    if jwt.is_empty() {
        return None;
    }
    Some(TraeLogin {
        uid: trae_dir_uid(dir),
        jwt,
        // ⚠️ 真实 `oauth.json` 里 `deviceId` 是**字符串**（`"2081789759006043"`）。
        // 原来只认 `as_i64` ⇒ 一直取到空串 ⇒ `x-device-id` 头是空的。
        // 上游对这个头有要求（缺了会回 9004 一类），空着发出去等于白撞。
        device_id: oauth
            .as_ref()
            .and_then(|v| v.get("deviceId"))
            .and_then(|v| {
                v.as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(str::to_string))
            })
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        machine_id: oauth
            .as_ref()
            .and_then(|v| v.get("machineId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        dir: dir.to_path_buf(),
    })
}

/// 全部**可用**的 Trae 登录态（按 uid 去重，先出现的优先）。
pub fn trae_logins() -> Vec<TraeLogin> {
    let mut out: Vec<TraeLogin> = Vec::new();
    for dir in trae_vault_dirs() {
        let Some(login) = trae_vault_login(&dir) else {
            continue;
        };
        if out.iter().any(|x| x.uid == login.uid) {
            continue;
        }
        out.push(login);
    }
    out
}

fn trae_entries() -> Vec<PoolEntry> {
    let now = qoder_accounts::now_ms();
    let mut out: Vec<PoolEntry> = Vec::new();

    // ⚠️ 目录枚举与凭据判定都走 `trae_vault_dirs` / `trae_vault_login`
    // （solo-cn 已弃用，见 `trae_vault_dirs` 的注释）。
    //
    // 2026-10-26 事故复盘：这里原来自己再枚举一遍 vault、自己 match 客户端名、
    // 自己判断「有没有凭据」，与发送端各有一套 ⇒ 池子选中了发不出去的号，
    // 网关连换 3 个号全 502。**枚举、uid、凭据三者现在都只有一个出口。**
    for dir in trae_vault_dirs() {
        let profile = read_json(&dir.join("profile.json"));
        let oauth = read_json(&dir.join("oauth.json"));

        let uid = trae_dir_uid(&dir);

        let label = profile
            .as_ref()
            .and_then(|v| v.get("screen_name"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Trae {uid}"));

        let remaining = profile
            .as_ref()
            .and_then(|v| v.get("credits"))
            .and_then(Value::as_f64)
            .map(|v| (v * 100.0).round() / 100.0);

        // ⚠️ 只算**有余额**的包：`credit_packs` 里混着大量已经用完的空包（remaining=0），
        // 它们的 `expire_at` 常常就在几小时后 —— 取最小值会把「已用完的包明天过期」
        // 显示成「这个账号的积分 10 小时后到期」（用户实测对不上）。
        let expires_at = profile
            .as_ref()
            .and_then(|v| v.get("credit_packs"))
            .and_then(Value::as_array)
            .and_then(|packs| {
                packs
                    .iter()
                    .filter(|p| p.get("remaining").and_then(Value::as_f64).unwrap_or(0.0) > 0.0)
                    .filter_map(|p| p.get("expire_at").and_then(Value::as_i64))
                    .min()
            })
            .or_else(|| {
                oauth
                    .as_ref()
                    .and_then(|v| v.get("expiredAt"))
                    .and_then(Value::as_str)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.timestamp_millis())
            });

        // ⚠️⚠️ 「可用」必须等于「**真能发出去**」。
        // 旧判据是「oauth.json 里有 privateKeyPEM」—— 那只是**签名私钥**，
        // 真正发请求要的是 storage.json 里能解开的 jwt。实测 `oauth_717036`
        // 有私钥却解不出登录态 ⇒ 池子把它当可用、按紧迫度选中、发出去 502。
        let has_cred = trae_vault_login(&dir).is_some();
        let expired = expires_at.map(|t| t <= now).unwrap_or(false);

        out.push(PoolEntry {
            key: format!("trae:{uid}"),
            product: "trae".into(),
            scope: "trae-cn".into(),
            label,
            remaining,
            expires_at,
            usable: has_cred && !expired,
            reason: if !has_cred {
                Some("缺少可用登录态（storage.json 里没有能解开的授权）".into())
            } else if expired {
                Some("积分已过期".into())
            } else {
                None
            },
            cooldown_until: None,
            limited_models: Vec::new(),
        });
    }
    // 同一个 uid 可能出现在多个目录（历史遗留的 solo-cn 副本）：按 uid 去重，
    // 保留剩余更高的一条。
    out.sort_by(|a, b| {
        b.remaining
            .partial_cmp(&a.remaining)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|e| seen.insert(e.key.clone()));
    out
}


/// 从 `storage.json` 的登录态里解出 JWT，再取 uid。
///
/// 只在 meta / profile 都拿不到 uid 时用；解不开就返回 None（调用方回退目录名）。
fn uid_from_storage(dir: &std::path::Path) -> Option<String> {
    let storage = read_json(&dir.join("User").join("globalStorage").join("storage.json"))?;
    let enc = storage
        .get("iCubeAuthInfo://icube.cloudide")
        .and_then(Value::as_str)?;
    let auth = crate::modules::trae_km::decrypt_km_json(enc)?;
    let token = auth.get("token").and_then(Value::as_str)?;
    let trimmed = crate::modules::trae_km::strip_cloud_prefix(token);
    crate::modules::trae_km::jwt_user_id(trimmed)
}

fn read_json(path: &std::path::Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

// ---------------------------------------------------------------- 选号

/// 一次选号的结果（给界面预览，也供网关内部使用）。
#[derive(Debug, Clone, Serialize)]
pub struct Selection {
    pub model: String,
    /// 这次允许的作用域（带前缀时只有一个）。
    pub candidates: Vec<String>,
    pub picked: Option<PoolEntry>,
    /// 为什么没选出来。
    pub reason: Option<String>,
}

/// 模型名 → 候选作用域。
///
/// - 带 `作用域:` 前缀：**只认那一个**（锁死，不会被紧迫度带走）
/// - 裸模型名：查 `models.json`（`{"trae-cn":["glm-5"]}`），返回所有提供它的作用域
/// 作用域**短别名**（用户手打友好）：
/// `trae` → `trae-cn`、`wb` → `wb-cn`、`qoder` → `qoder-cn`；
/// 国际版用 `-g` 后缀：`trae-g` / `wb-g` / `qoder-g`。
///
/// 全称永远有效；别名只是省打字，且**必须在解析前缀时归一化**，
/// 否则 `wb:glm-5.3` 会被当成未知作用域。
pub fn scope_alias(scope: &str) -> &str {
    match scope {
        "trae" => "trae-cn",
        "trae-g" => "trae-global",
        "wb" => "wb-cn",
        "wb-g" => "wb-global",
        "qoder" => "qoder-cn",
        "qoder-g" => "qoder-global",
        other => other,
    }
}

pub fn resolve_scopes(model: &str) -> Result<(String, Vec<String>), String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("模型名不能为空".into());
    }
    if let Some((ns, rest)) = model.split_once(':') {
        let ns = scope_alias(ns.trim());
        if rest.trim().is_empty() {
            return Err(format!("{ns} 后面没有模型名"));
        }
        if !is_known_scope(ns) {
            return Err(format!(
                "未知作用域 {ns}（可用：trae-cn / trae-global / wb-cn / wb-global / qoder-cn / qoder-global，\
                 或短别名 trae / trae-g / wb / wb-g / qoder / qoder-g）"
            ));
        }
        return Ok((rest.trim().to_string(), vec![ns.to_string()]));
    }

    let catalog = catalog();
    if catalog.is_empty() {
        return Err(
            "模型目录为空：请在 ~/.twin-switch/models.json 里登记（例如 {\"trae-cn\":[\"glm-5\"]}），\
             或写成 作用域:模型"
                .into(),
        );
    }
    // 清单三元组（作用域 / 代号 / 公开名）—— 与界面分组、`/v1/models` 同一个出口。
    // Qoder 的公开名是客户端里显示的名字（`GLM-5.3`），代号 `gmodel` 不外露。
    let rows = crate::modules::gateway_models::catalog_rows(None);

    // ① 代号精确匹配
    let mut scopes: Vec<String> = rows
        .iter()
        .filter(|(_, code, _)| code == model)
        .map(|(scope, _, _)| scope.clone())
        .collect();
    let mut canonical = rows
        .iter()
        .find(|(_, code, _)| code == model)
        .map(|(_, _, public)| public.clone())
        .unwrap_or_else(|| model.to_string());

    // ② 公开名精确匹配（Qoder 客户端里那个名字）
    if scopes.is_empty() {
        scopes = rows
            .iter()
            .filter(|(_, _, public)| public == model)
            .map(|(scope, _, _)| scope.clone())
            .collect();
        if !scopes.is_empty() {
            canonical = model.to_string();
        }
    }

    // ③ 精确匹配不到时，按归一化指纹再找一次（大小写 / 连字符差异是常见输入误差）。
    //    找到就用**清单里的规范名**，避免把 `DeepSeek-V4-Flash` 原样转给只认小写的上游。
    if scopes.is_empty() {
        let want = normalize_model_key(model);
        for (scope, code, public) in &rows {
            if normalize_model_key(code) == want || normalize_model_key(public) == want {
                if !scopes.contains(scope) {
                    scopes.push(scope.clone());
                }
                if canonical == model {
                    canonical = public.clone();
                }
            }
        }
    }

    if scopes.is_empty() {
        let known = rows
            .iter()
            .map(|(scope, _, public)| format!("{scope}:{public}"))
            .collect::<Vec<_>>()
            .join("、");
        return Err(format!("模型 {model:?} 不在模型目录里。已知：{known}"));
    }
    scopes.sort();
    scopes.dedup();
    Ok((canonical, scopes))
}

/// 模型名的归一化指纹：只留字母数字并小写。
///
/// 用途：用户从客户端抄来的名字和上游清单常差在大小写或连字符上 ——
/// `DeepSeek-V4-Flash` / `deepseek_v4_flash` / `deepseek-v4-flash` 应该被认成同一个。
///
/// ⚠️ 界面（`GatewayPage.tsx` 的分组）抄的是同一条规则 —— 改这里必须同步改前端，
/// 否则「通用模型」里会出现两个只差大小写的同名条目。
pub fn normalize_model_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_known_scope(s: &str) -> bool {
    matches!(
        s,
        "wb-cn" | "wb-global" | "trae-cn" | "trae-global" | "qoder-cn" | "qoder-global"
    )
}

/// 读模型目录（空 = 没登记）。
///
/// ⚠️ 唯一出口是 `gateway_models::load_effective()`：`models.json` 已升级成
/// `defaults` + `user` 两段（GitHub 同步只覆盖前者、用户手动添加的永不覆盖），
/// 这里不能再自己解析那份文件 —— 否则用户加的模型会被静默忽略。
pub fn catalog() -> Vec<(String, Vec<String>)> {
    crate::modules::gateway_models::load_effective()
        .into_iter()
        .collect()
}

/// 选号：在候选作用域里挑**最紧迫且可用**的一个。
pub fn preview(model: &str) -> Result<Selection, String> {
    let (bare, scopes) = resolve_scopes(model)?;
    let mut pool = usable();
    let now = now_ms();
    // 同 `ordered_candidates`：预览必须按**这个模型**的有效冷却挑，否则会给用户
    // 展示一个「其实用不了」的号（2026-10-24 用户实测：预览还显示被限制的账号）。
    for e in pool.iter_mut() {
        e.cooldown_until = cooling_until(&e.key, Some(&bare));
        e.limited_models = model_limits(&e.key);
    }
    // 冷却中的放最后（不是排除：全部在冷却时宁可等最早解冻的，也别误报「没有账号」）。
    let mut candidates: Vec<&PoolEntry> = pool.iter().filter(|e| scopes.contains(&e.scope)).collect();
    candidates.sort_by(|a, b| {
        a.cooldown_until
            .unwrap_or(0)
            .cmp(&b.cooldown_until.unwrap_or(0))
    });
    let picked = candidates
        .iter()
        .find(|e| e.cooldown_until.unwrap_or(0) <= now)
        .or_else(|| candidates.first())
        .cloned()
        .cloned();
    let reason = if picked.is_none() {
        Some(format!("候选作用域 {} 里没有可用账号", scopes.join(" / ")))
    } else {
        picked
            .as_ref()
            .filter(|p| p.cooldown_until.unwrap_or(0) > now)
            .map(|p| {
                let until = p.cooldown_until.unwrap_or(0);
                format!(
                    "{} 正在冷却（{} 秒后解冻），这次只能先用它；换号会优先挑别的账号",
                    p.label,
                    ((until - now) / 1000).max(0)
                )
            })
    };
    Ok(Selection { model: bare, candidates: scopes, picked, reason })
}

/// **一个请求内的换号顺序**：同 [`preview`] 的排序，但返回**全部**候选账号。
///
/// 为什么需要它：`preview` 只给「这一次挑谁」。而实际上游会临时拒绝某个账号
/// （最典型的就是 Trae 的 `HTTP 429 / code 6804「使用量已超出频率限制」`，
/// 以及某个号的额度当天耗尽）—— 这时**同一个请求**就该换下一个号继续，
/// 而不是把 429 原样抛给用户让他手动再点一次。
///
/// 排序：**没在冷却的在前**（同组内保持 `snapshot()` 的紧迫度顺序），
/// 冷却中的排后面兜底 —— 全在冷却时仍然会试（宁可用最早解冻的，也别直接报「没有账号」）。
pub fn ordered_candidates(model: &str) -> Result<Vec<PoolEntry>, String> {
    let (bare, scopes) = resolve_scopes(model)?;
    let now = now_ms();
    let mut out: Vec<PoolEntry> = usable()
        .into_iter()
        .filter(|e| scopes.contains(&e.scope))
        .collect();
    // ⚠️ 必须用**这次请求的模型**重算有效冷却：`usable()` 里算的是整号级，
    // 而「只在这个模型上被限」的号在那里看起来毫无问题，会被排到队首再撞一次。
    for e in out.iter_mut() {
        e.cooldown_until = cooling_until(&e.key, Some(&bare));
        e.limited_models = model_limits(&e.key);
    }
    // stable sort：只按「是否冷却」分组，组内顺序（紧迫度）不变。
    out.sort_by_key(|e| e.cooldown_until.unwrap_or(0) > now);
    Ok(out)
}

/// 某个账号成功过一次 ⇒ 撤掉它的冷却，别让一次偶发失败把它按 60 秒。
///
/// 整号级与该模型的冷却一起撤：既然这一次通了，说明两级的判断都过期了。
pub fn clear_cooldown(key: &str, model: Option<&str>) {
    if let Ok(mut m) = cooldowns().lock() {
        let mut changed = m.remove(key).is_some();
        if let Some(name) = model {
            changed = m.remove(&cooldown_key(key, Some(name))).is_some() || changed;
        }
        if changed {
            persist_cooldowns(&m);
        }
    }
}

/// Qoder 区域 → 作用域字符串（界面展示用）。
pub fn qoder_scope(region: QoderRegion) -> String {
    format!("qoder-{}", region.as_str())
}

#[cfg(test)]
mod tests {

    /// 回归（2026-10-26 事故）：**有私钥但解不出登录态**的号，积分池必须判为不可用。
    ///
    /// 事故现象：池子按紧迫度选中了 `oauth_717036`（它有 `privateKeyPEM`），
    /// 真正发请求时需要 `storage.json` 里的 jwt，那份解不出来 ⇒ 502
    /// 「找不到账号 842681600717036 的可用 Trae 登录态」，连换 3 个号全失败。
    #[test]
    fn trae_login_requires_decryptable_storage_auth() {
        let dir = std::env::temp_dir().join(format!("trae-login-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("User").join("globalStorage")).expect("建目录");
        // 只有签名私钥，没有可解开的授权条目 —— 正是出事的那种号
        std::fs::write(
            dir.join("oauth.json"),
            r#"{"privateKeyPEM":"-----BEGIN PRIVATE KEY-----","deviceId":6200000000000001}"#,
        )
        .expect("写 oauth");
        std::fs::write(
            dir.join("User").join("globalStorage").join("storage.json"),
            r#"{"other":"x"}"#,
        )
        .expect("写 storage");
        assert!(
            trae_vault_login(&dir).is_none(),
            "解不出 jwt 就不许判为可用，否则又会选中一个发不出去的号"
        );

        // 补上能解开的授权条目才可用 —— 但本测试不构造真实密文，
        // 因此只断言「没有条目 ⇒ None」这条硬判据（剩下的由真机验证覆盖）。
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回归：uid 解析必须与池子/发送端**同一套回退顺序**（meta → profile → oauth.id → 目录名）。
    #[test]
    fn trae_dir_uid_falls_back_in_one_order() {
        let dir = std::env::temp_dir().join(format!("trae-uid-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let name = dir.file_name().unwrap().to_string_lossy().to_string();

        // 什么都没有 ⇒ 目录名兜底
        assert_eq!(trae_dir_uid(&dir), name);

        // 只有 oauth.id ⇒ 用 oauth.id
        std::fs::write(dir.join("oauth.json"), r#"{"id":"888800001111"}"#).unwrap();
        assert_eq!(trae_dir_uid(&dir), "888800001111");

        // profile.user_id 优先于 oauth.id
        std::fs::write(dir.join("profile.json"), r#"{"user_id":222233334444}"#).unwrap();
        assert_eq!(trae_dir_uid(&dir), "222233334444", "数字 uid 会被转成字符串");

        // meta.verified_uid 最优先（这才是权威值）
        std::fs::write(dir.join("meta.json"), r#"{"verified_uid":842681600717036}"#).unwrap();
        assert_eq!(trae_dir_uid(&dir), "842681600717036");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 排序：**最早过期的必须排最前**，不能被「默认账号」压过去。
    ///
    /// 这正是 2026-10-26 用户实测的问题：明明有个号积分 11-01 就过期，
    /// 消耗却一直落在当前登录（默认）的那个号上。
    #[test]
    fn soonest_expiry_beats_default_account() {
        let mk = |key: &str, exp: Option<i64>| PoolEntry {
            key: key.to_string(),
            product: "trae".to_string(),
            scope: "trae-cn".to_string(),
            label: key.to_string(),
            remaining: Some(100.0),
            expires_at: exp,
            usable: true,
            reason: None,
            cooldown_until: None,
            limited_models: Vec::new(),
        };
        let mut list = vec![
            // 默认账号：到期最晚
            mk("trae:default", Some(1_800_000_000_000)),
            mk("trae:soon", Some(1_790_000_000_000)),
            mk("trae:none", None),
        ];
        // 与 `snapshot()` 里的比较器同序（这里不依赖磁盘，只验排序规则本身）
        let is_default = |k: &str| k == "trae:default";
        list.sort_by(|a, b| {
            b.usable
                .cmp(&a.usable)
                .then_with(|| {
                    a.urgency()
                        .partial_cmp(&b.urgency())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| is_default(&b.key).cmp(&is_default(&a.key)))
        });
        assert_eq!(list[0].key, "trae:soon", "最早过期的必须排第一");
        assert_eq!(list[1].key, "trae:default", "默认账号只能排在它后面");
        assert_eq!(list[2].key, "trae:none", "没有到期时间的排最后");
    }

    /// 会话粘性：同一段对话查得到、换号后能覆盖；清掉之后查不到。
    #[test]
    fn sticky_binds_and_forgets() {
        let conv = "test-conv-sticky-001";
        sticky_forget(conv);
        assert_eq!(sticky_lookup(conv), None);
        sticky_bind(conv, "trae:aaa");
        assert_eq!(sticky_lookup(conv), Some("trae:aaa".to_string()));
        // 换号 ⇒ 覆盖（以最新为准，不是"保留第一个"）
        sticky_bind(conv, "trae:bbb");
        assert_eq!(sticky_lookup(conv), Some("trae:bbb".to_string()));
        sticky_forget(conv);
        assert_eq!(sticky_lookup(conv), None);
    }

    /// 空键不许写进表（否则会把无关对话粘到同一个号上）。
    #[test]
    fn sticky_ignores_empty() {
        sticky_bind("", "trae:aaa");
        assert_eq!(sticky_lookup(""), None);
        sticky_bind("x", "");
        assert_eq!(sticky_lookup("x"), None);
    }
    use super::*;

    /// 上游文案里的「将在 2026-10-24 22:04:42 UTC+8 重置」要能抠出来 ——
    /// 抠得到才能把「当日额度用完」钉到重置时刻，而不是 60 秒后又去撞它。
    #[test]
    fn reset_time_is_parsed_from_upstream_hint() {
        let hint = r#"{"code":6804,"msg":"您的使用量已超出频率限制，将在 2026-10-24 22:04:42 UTC+8 重置"}"#;
        let at = reset_at_from_hint(hint).expect("应当解析出重置时间");
        let expect = chrono::NaiveDateTime::parse_from_str("2026-10-24 22:04:42", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_local_timezone(chrono::Local)
            .single()
            .unwrap()
            .timestamp_millis()
            + 60_000;
        assert_eq!(at, expect, "留 1 分钟余量");
        // 抠不到就交回 None，由调用方按 1 小时兜底（绝不编一个时间出来）
        assert!(reset_at_from_hint("没有时间的一段话").is_none());
    }

    #[test]
    fn unknown_scope_is_rejected_not_guessed() {
        assert!(resolve_scopes("nope:glm-5").is_err());
        assert!(resolve_scopes("trae-cn:").is_err());
        assert!(resolve_scopes("   ").is_err());
    }

    #[test]
    fn short_aliases_map_to_real_scopes() {
        assert_eq!(resolve_scopes("wb:glm-5.3").unwrap().1, vec!["wb-cn".to_string()]);
        assert_eq!(resolve_scopes("trae:glm-5.3").unwrap().1, vec!["trae-cn".to_string()]);
        assert_eq!(
            resolve_scopes("qoder-g:qmodel").unwrap().1,
            vec!["qoder-global".to_string()]
        );
        assert_eq!(resolve_scopes("wb-g:x").unwrap().1, vec!["wb-global".to_string()]);
    }

    #[test]
    fn explicit_prefix_locks_to_one_scope() {
        let (model, scopes) = resolve_scopes("trae-cn:glm-5").unwrap();
        assert_eq!(model, "glm-5");
        assert_eq!(scopes, vec!["trae-cn".to_string()]);
    }

    #[test]
    fn known_scopes_cover_all_six() {
        for s in ["wb-cn", "wb-global", "trae-cn", "trae-global", "qoder-cn", "qoder-global"] {
            assert!(is_known_scope(s), "{s}");
        }
        assert!(!is_known_scope("wb-cn-x"));
    }

    /// 2026-10-24 用户贴回来的**上游原文**（WorkBuddy 网关实拍）——
    /// 逐字抄进来当判据，别再凭记忆写一个理想化的版本。
    const REAL_WB_429: &str = "上游返回 HTTP 429（上游限流 / 当日额度用完）：\
        您的使用量已超出频率限制，将在 2026-10-25 11:12:02 UTC+8 重置，\
        您也可以切换其他模型继续使用。 (01a16cf737ce75ba.../71e874b5-2357-41f9-bfa5-825ffbf2bdfe)";

    #[test]
    fn real_upstream_429_parks_the_account_until_its_reset_time() {
        // ⚠️ `now` 必须**固定在文案里的重置时刻之前**（这里取 2026-10-24 12:00 UTC+8）。
        //
        // 原来用的是 `Local::now()` —— 那是个时间炸弹：断言「冷却必须超过 60 秒」
        // 在 2026-10-25 11:12 之后必然失败（重置时刻已经过去），而那时它跟真的
        // 回归长得一模一样。2026-10-26 起这条一直在报错、掩盖真失败，就是它。
        // 判据本身（钉在上游给的重置时刻）与被测代码无关，所以固定 now 完全够用。
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 24)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            - 8 * 3_600_000;
        let (until, kind) = classify(429, REAL_WB_429, now).expect("额度耗尽必定要冷却");
        assert_eq!(kind, "balance_exhausted", "不该退化成 60 秒的 rate_limited");
        // ⚠️ 期望值按文案里的 **UTC+8** 算，不看本机时区 —— 换台机器（UTC+0 / UTC-5）
        // 这个断言也必须成立，否则「11:12 恢复」会被人家的本地时区解释成别的时刻。
        let expect = chrono::NaiveDate::from_ymd_opt(2026, 10, 25)
            .unwrap()
            .and_hms_opt(11, 12, 2)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            - 8 * 3_600_000
            + 60_000;
        assert_eq!(until, expect, "应当钉在上游给的重置时刻（按 UTC+8，留 1 分钟余量）");
        // 关键：不能只停 60 秒 —— 那 60 秒后又会挑回这个号。
        assert!(until - now > 60_000, "必须超过 60 秒，否则换号等于没换");
    }

    #[test]
    fn utc_offset_is_read_from_the_text_not_from_the_machine_timezone() {
        assert_eq!(parse_utc_offset("2026-10-25 11:12:02 UTC+8 重置"), Some(8 * 3_600_000));
        assert_eq!(parse_utc_offset("will reset at 2026-10-25 11:12:02 UTC+08:00"), Some(8 * 3_600_000));
        assert_eq!(parse_utc_offset("UTC-5"), Some(-5 * 3_600_000));
        assert_eq!(parse_utc_offset("UTC+05:30"), Some(5 * 3_600_000 + 30 * 60_000));
        // 没有时区就承认不知道，不默认 +8。
        assert_eq!(parse_utc_offset("2026-10-25 11:12:02"), None);
    }

    /// 冷却键必须能区分「整号」和「某个模型」：前者锁整个号，后者只锁一个模型。
    /// 这是「一个模型用完不该把整个号打进冷宫」的唯一判据。
    /// 记账粒度：额度/限流只锁模型，凭据死锁整个号。
    ///
    /// 反例就在用户贴的上游原文里：「…重置，**您也可以切换其他模型继续使用**」——
    /// 若按整号停，一个还能用别的模型的号会被白白关到明天。
    #[test]
    fn quota_errors_lock_the_model_but_auth_errors_lock_the_account() {
        let m = Some("deepseek-v4.1-flash");
        assert_eq!(cooldown_scope("balance_exhausted", m), m);
        assert_eq!(cooldown_scope("rate_limited", m), m);
        assert_eq!(cooldown_scope("auth_dead", m), None);
        assert_eq!(cooldown_scope("transient", m), None);
        // 没有模型名时退化成整号级（不能记到 `@` 这种空模型上）。
        assert_eq!(cooldown_scope("balance_exhausted", None), None);
    }

    #[test]
    fn cooldown_key_separates_account_wide_from_per_model() {
        assert_eq!(cooldown_key("workbuddy:abc", None), "workbuddy:abc");
        assert_eq!(
            cooldown_key("workbuddy:abc", Some("deepseek-v4.1-flash")),
            "workbuddy:abc@deepseek-v4.1-flash"
        );
        // 空白模型名当成整号级（不能生成 `workbuddy:abc@` 这种半截键）。
        assert_eq!(cooldown_key("workbuddy:abc", Some("   ")), "workbuddy:abc");
        // 两个模型的键互不相同 —— 只锁一个不会误伤另一个。
        assert_ne!(
            cooldown_key("k", Some("a")),
            cooldown_key("k", Some("b"))
        );
    }

    #[test]
    fn reset_stamp_is_found_without_the_will_reset_wording() {
        // JSON resetTime（ISO 风格 T 分隔）
        assert!(reset_at_from_hint(r#"{"resetTime":"2026-10-25T11:12:02"}"#).is_some());
        // 「将于」
        assert!(reset_at_from_hint("您的额度已用完，将于 2026-10-25 11:12:02 重置").is_some());
        // 斜杠 + 单数字月日
        assert!(reset_at_from_hint("quota exceeded, resets at 2026/10/5 9:03:01").is_some());
        // 长十六进制串里的假年份不能当成时间
        assert!(reset_at_from_hint("trace 01a16cf737ce75ba71e874b5").is_none());
        // 完全没时间 → None（由调用方按 1 小时兜底，绝不编）
        assert!(reset_at_from_hint("service unavailable").is_none());
    }

    #[test]
    fn error_feedback_puts_each_kind_in_its_lane() {
        let now = now_ms();
        // 429 且文案没说额度 → 60 秒档
        let (until, kind) = classify(429, "too many requests", now).unwrap();
        assert_eq!(kind, "rate_limited");
        assert!((until - now - 60_000).abs() < 2_000);
        // 401 → 30 分钟档
        let (until, kind) = classify(401, "", now).unwrap();
        assert_eq!(kind, "auth_dead");
        assert!((until - now - 30 * 60_000).abs() < 2_000);
        // 排队（10605）→ 不冷却
        assert!(classify(403, "code 10605 isQueued", now).is_none());
        // 客户端参数错 → 不冷却
        assert!(classify(400, "bad request", now).is_none());
        // WorkBuddy 频道校验（11128）→ 账号级 10 分钟（不是 400 那种「不冷却」）
        let (until, kind) = classify(
            400,
            r#"{"code":"11128","msg":"Illegal API invocation from an unapproved channel"}"#,
            now,
        )
        .expect("11128 必须冷却这个号，否则下一个请求还会撞它");
        assert_eq!(kind, "auth_dead");
        assert!((until - now - 10 * 60_000).abs() < 2_000);
        // 402 → 停到下一个重置窗口（必定 > 现在 + 1 小时）
        let (until, kind) = classify(402, "", now).unwrap();
        assert_eq!(kind, "balance_exhausted");
        assert!(until > now + 3_600_000, "402 应停到每日重置窗口");
        // 额度不足但上游没说时间 → 1 小时兜底，不编精确时刻
        let (until, kind) = classify(403, "额度不足", now).unwrap();
        assert_eq!(kind, "balance_exhausted");
        assert!((until - now - 3_600_000).abs() < 2_000);
    }

    #[test]
    fn urgency_sorts_missing_expiry_last() {
        let mk = |expires_at: Option<i64>| PoolEntry {
            key: "k".into(), product: "p".into(), scope: "s".into(), label: "l".into(),
            remaining: None, expires_at, usable: true, reason: None, cooldown_until: None,
            limited_models: Vec::new(),
        };
        assert!(mk(Some(100)).urgency() < mk(Some(200)).urgency());
        assert!(mk(Some(200)).urgency() < mk(None).urgency());
    }
}

#[cfg(test)]
mod lock_scope_probe {
    use super::*;

    /// 真机自检：通用模型「裸名」会跨产品选号；带前缀「锁死」只留那一家。
    #[test]
    #[ignore]
    fn probe_lock_scope() {
        for m in [
            "deepseek-v4.1-flash",
            "wb:deepseek-v4.1-flash",
            "trae:deepseek-v4.1-flash",
        ] {
            match preview(m) {
                Ok(sel) => eprintln!(
                    "[probe] {m} → 候选={:?} 选中={:?}",
                    sel.candidates,
                    sel.picked.as_ref().map(|p| format!("{} / {}", p.label, p.scope))
                ),
                Err(e) => eprintln!("[probe] {m} → 失败：{e}"),
            }
        }
    }
}
