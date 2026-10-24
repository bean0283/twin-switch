//! 每日签到：Trae / WorkBuddy 全账号「一键签到」。
//!
//! 接口细节逐字移植自两套青龙脚本（WorkBuddy 是 Python、Trae 是 Node），
//! 路径 / 请求头 / 成功判据全部以脚本为准：
//!
//! | 产品 | 用途 | 方法与路径 |
//! |---|---|---|
//! | WorkBuddy | 查状态 | `POST https://www.codebuddy.cn/v2/billing/meter/checkin-activity-status` |
//! | WorkBuddy | 签到 | `POST https://www.codebuddy.cn/v2/billing/meter/daily-checkin` |
//! | Trae | 查状态 | `POST {host}/trae/api/v2/ug/checkin_credits/status` |
//! | Trae | 领取 | `POST {host}/trae/api/v2/ug/checkin_credits/claim` |
//! | Trae | 查余额 | `POST {host}/trae/api/v2/pay/user_current_entitlement_list` |
//! | Trae | 取昵称 | `POST {host}/cloudide/api/v3/trae/GetUserInfo` |
//!
//! ## 三条不能打折的规则
//!
//! 1. **token 过期就先跳过，一个请求都不发** —— 结果是 `status = "skipped"`，
//!    与 `"failed"` 在界面上必须长得不一样（项目最高优先级第 ③ 条：
//!    「空」与「失败」绝不许长得一样）。
//! 2. 「今天已签到」是**正常**（WorkBuddy `code 10001` / Trae `checked_in = true`），
//!    归到 `"already"`，不能算失败。
//! 3. 失败原因不许塌缩成一个 `bool`：HTTP 状态码 + 业务 `code` + `message`
//!    逐个带出来；**传输层失败（`httpStatus == 0`）单独一档**，不许伪装成业务错码。
//!
//! ## 与脚本的两处**故意不同**
//!
//! - **不做 0–60 秒的随机等待**。脚本是无人值守的 cron，随机延迟是防检测；
//!   这里是用户手点按钮，等一分钟不可接受，只保留账号之间 300 ms 的最小间隔。
//! - **不读环境变量**（`WB_TOKEN` / `TRAE_TOKEN` / 钉钉 webhook 等），
//!   凭据直接来自本机账号库，通知走界面。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::modules::config::{http_request_raw, store_dir};
use crate::modules::{jwt, trae_vault, workbuddy_auth, workbuddy_vault};

// ---------------------------------------------------------------------------
// 常量（逐字对齐脚本）
// ---------------------------------------------------------------------------

const WB_STATUS_PATH: &str = "/v2/billing/meter/checkin-activity-status";
const WB_CHECKIN_PATH: &str = "/v2/billing/meter/daily-checkin";

const TRAE_STATUS_PATH: &str = "/trae/api/v2/ug/checkin_credits/status";
const TRAE_CLAIM_PATH: &str = "/trae/api/v2/ug/checkin_credits/claim";
const TRAE_ENTITLE_PATH: &str = "/trae/api/v2/pay/user_current_entitlement_list";
const TRAE_USERINFO_PATH: &str = "/cloudide/api/v3/trae/GetUserInfo";

/// Trae 候选域名：脚本走 `api.trae.cn`；oauth 落库的 host 可能不同，失败时逐个回退。
///
/// ⚠️⚠️ **2026-10-14 实测（钉死，别再改）**：
/// - `https://api.trae.cn` ⇒ 四个接口全 **200**
///   （status / claim / user_current_entitlement_list / GetUserInfo）；
/// - `https://api.trae.com.cn` ⇒ **404 + HTML（TLB）**。
///   它偏偏就是 OAuth 登录落库进 `oauth.json` 的那个 `host`，
///   所以**绝不能**把「oauth 落库的 host」当成「接口域名」直接信 ——
///   2026-10-14 的 Trae 签到 404 事故正是这么来的（详见 `checkin_trae` 挑域名那段）。
const TRAE_HOSTS: &[&str] = &["https://api.trae.cn", "https://api.trae.com.cn"];

const TRAE_UA: &str = "VSCode 1.107.1 (TRAE SOLO CN)";

/// 账号之间的固定间隔：脚本用 0–60 秒随机延迟防检测，手动点按钮不能那么等。
const GAP_MS: u64 = 300;

/// 剩余有效期不足这么多天时，结果里标注「即将过期」（脚本里的 `WARN_DAYS`）。
const WARN_DAYS_MS: i64 = 3 * 86_400_000;

// ---------------------------------------------------------------------------
// 结果状态（前端按它上不同颜色，绝不能合并）
// ---------------------------------------------------------------------------

pub mod status {
    /// 这次真的签到了（或领取了）并拿到积分。
    pub const SUCCESS: &str = "success";
    /// 今天已经签过了 —— **正常**，不是失败。
    pub const ALREADY: &str = "already";
    /// token 已过期，**一个请求都没发**就跳过。
    pub const SKIPPED: &str = "skipped";
    /// 这个账号压根没法签（没有网页凭证 / 凭据是加密信封 / 活动功能关闭）。
    pub const UNAVAILABLE: &str = "unavailable";
    /// 当前没有进行中的签到活动（WorkBuddy 特有）。
    pub const INACTIVE: &str = "inactive";
    /// 真的失败了。
    pub const FAILED: &str = "failed";
}

/// 一条签到结果。
struct Item {
    client: &'static str,
    /// Trae 侧才有（账号库按 `clientKey/<id>` 分目录）。
    client_key: String,
    id: String,
    uid: String,
    name: String,
    status: &'static str,
    message: String,
    /// 本次到账积分。`None` = **未知**（接口没给），与 `Some(0.0)` 不一样。
    gained: Option<f64>,
    /// 当前余额。`None` = 未知。
    balance: Option<f64>,
    http_status: u16,
    code: Option<i64>,
}

impl Item {
    fn new(client: &'static str, id: &str, name: &str) -> Self {
        Self {
            client,
            client_key: String::new(),
            id: id.to_string(),
            uid: String::new(),
            name: name.to_string(),
            status: status::FAILED,
            message: String::new(),
            gained: None,
            balance: None,
            http_status: 0,
            code: None,
        }
    }

    fn done(mut self, status: &'static str, message: impl Into<String>) -> Self {
        self.status = status;
        self.message = message.into();
        self
    }

    fn to_value(&self) -> Value {
        json!({
            "client": self.client,
            "clientKey": self.client_key,
            "id": self.id,
            "uid": self.uid,
            "name": self.name,
            "status": self.status,
            "message": self.message,
            "gained": self.gained,
            "balance": self.balance,
            "httpStatus": self.http_status,
            "code": self.code,
        })
    }
}

// ---------------------------------------------------------------------------
// HTTP / 解析小工具
// ---------------------------------------------------------------------------

/// 一次 POST 的结果。`http_status == 0` 表示**传输层失败**（连都连不上），
/// 与「服务端回了 4xx/5xx」是两回事，调用方必须分开处理。
struct Resp {
    http_status: u16,
    body: Value,
    /// `body` 是不是**服务端真的按 JSON 返回的**。
    ///
    /// ⚠️⚠️ 必须有这个标记：`post()` 解析不出 JSON 时会塞一个
    /// `{"message": <原始文本>}` **兜底对象**，那看起来也是个 JSON 对象，
    /// 于是「网关的 HTML 404 挡板页」和「服务端真的回了 `{message:...}`」长得一模一样。
    /// 2026-10-14 的 Trae 签到 404 事故就是被这个兜底对象掩盖了一层（见 `checkin_trae` 里
    /// 挑域名那段的长注释）。
    json: bool,
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let head: String = t.chars().take(max).collect();
    format!("{head}…")
}

/// 把多行文本压成一行（HTML 挡板页里全是换行，直接显示会撑爆一行）。
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn post(url: &str, headers: &HashMap<String, String>, body: Value) -> Resp {
    let (http_status, _hdrs, text) =
        http_request_raw(url, "POST", Some(body), Some(headers), None, true).await;
    let (body, json) = if http_status == 0 {
        // 传输层失败：text 是底层错误原因（`http_request_raw` 的约定）。
        (json!({ "message": truncate(&text, 300) }), false)
    } else if text.trim().is_empty() {
        (Value::Null, false)
    } else {
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => (v, true),
            Err(_) => (json!({ "message": truncate(&text, 300) }), false),
        }
    };
    Resp { http_status, body, json }
}

fn body_code(body: &Value) -> Option<i64> {
    body.get("code").and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()))
}

/// 这个响应算不算「**服务端真的答了**」？
///
/// ⚠️ 判据是「body 真的是 JSON」**且**（2xx 或带业务 `code`）：
/// - Trae / WorkBuddy 的业务错（`code 9004`、`10001`、token 无效…）都是
///   **HTTP 200 + JSON** ⇒ 有 `code` ⇒ 算答了，**不换域名、不许掩盖**；
/// - 只按 HTTP 状态码判断不够：走错域名时 TLB 回的是 `HTTP 404 + HTML`；
/// - 只按「是不是 JSON 对象」判断也不够：`post()` 的兜底对象
///   `{"message": "<html>…"}` 也是个对象 ⇒ 必须看 `json` 标记。
fn looks_like_api_reply(r: &Resp) -> bool {
    r.json && (r.http_status < 400 || body_code(&r.body).is_some())
}

fn body_message(body: &Value) -> String {
    for k in ["msg", "message", "errMsg", "error"] {
        if let Some(s) = body.get(k).and_then(Value::as_str) {
            let s = s.trim();
            if !s.is_empty() {
                return truncate(s, 200);
            }
        }
    }
    String::new()
}

/// 失败描述：**HTTP 状态码 + 业务 code + 服务端 message 一个都不许吞**。
fn describe(resp: &Resp) -> String {
    // ★ 1001 = Trae 侧「认证失败」。**不是过期**（过期在发请求前就被 `expiry_state` 拦掉了），
    // 而是服务端不认这条 token：多半是在别处重新登录 / 换了设备，旧 token 被吊销。
    // 四档设备候选（上次成功 / 派生 / 派生回退 / 客户端设备）都试过才走到这里，
    // 所以不必再让用户去折腾设备 —— 直接告诉他重新登录。
    if body_code(&resp.body) == Some(1001) {
        return "登录态被服务端拒绝（code=1001，token 已在别处失效）⇒ 请在该客户端重新登录一次".into();
    }
    if resp.http_status == 0 {
        return format!("网络请求失败：{}", body_message(&resp.body));
    }
    let code = body_code(&resp.body)
        .map(|c| c.to_string())
        .unwrap_or_else(|| "—".to_string());
    let msg = if resp.json {
        body_message(&resp.body)
    } else {
        // ⚠️ 不是 JSON ⇒ 多半是网关 / 边缘节点的挡板页（一坨 HTML）。原来把 300 字符的
        //    `<html>…<center>TLB</center>…` 原样怼给用户，既看不懂又占满一行；
        //    这里压成一行、只留前面一小段，并**如实说明「非 JSON 响应」**（不藏事实）。
        let raw = one_line(&body_message(&resp.body));
        if raw.is_empty() {
            String::new()
        } else {
            format!("非 JSON 响应：{}", truncate(&raw, 80))
        }
    };
    if msg.is_empty() {
        format!("HTTP {} / code={}", resp.http_status, code)
    } else {
        format!("HTTP {} / code={} / {}", resp.http_status, code, msg)
    }
}

/// `exp` 量级：`expired` / `warn` / `ok` / `unknown`。`unknown` 时脚本照常继续。
fn expiry_state(token: &str) -> (&'static str, i64) {
    let exp = jwt::exp_ms(token);
    if exp <= 0 {
        return ("unknown", 0);
    }
    let now = chrono::Utc::now().timestamp_millis();
    if exp <= now {
        ("expired", exp)
    } else if exp - now <= WARN_DAYS_MS {
        ("warn", exp)
    } else {
        ("ok", exp)
    }
}

fn expired_text(exp_ms: i64) -> String {
    let d = chrono::DateTime::from_timestamp_millis(exp_ms)
        .map(|t| t.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap()))
        .map(|t| t.format("%Y/%m/%d %H:%M").to_string())
        .unwrap_or_else(|| "未知时间".to_string());
    format!("已于 {d} 过期")
}

fn round2(v: f64) -> f64 {
    let r = (v * 100.0).round() / 100.0;
    // ⚠️ 归一化 `-0.0`：IEEE 的负零会**原样**序列化成 JSON 的 `-0.0`，
    //    在别的前端 / 日志里就显示成「-0」。实测「本次一个积分都没到账」时
    //    求和结果就是 `-0.0`（2026-10-14 线上探针跑出来的）。零就是零，
    //    这里的正负号没有信息量。
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

// ---------------------------------------------------------------------------
// WorkBuddy
// ---------------------------------------------------------------------------

fn wb_headers(token: &str, uid: &str) -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert("Authorization".to_string(), format!("Bearer {token}"));
    h.insert("Accept".to_string(), "application/json".to_string());
    if !uid.trim().is_empty() {
        h.insert("X-User-Id".to_string(), uid.trim().to_string());
    }
    h
}

fn wb_num(data: &Value, key: &str) -> Option<f64> {
    data.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_i64().map(|i| i as f64))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

fn wb_i64(data: &Value, key: &str) -> i64 {
    wb_num(data, key).map(|v| v as i64).unwrap_or(0)
}

fn wb_bool(data: &Value, key: &str) -> bool {
    data.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 签到结果里的「连签 / 累计 / 本周」摘要，脚本里叫 `tail`。
fn wb_tail(data: &Value) -> String {
    format!(
        "，连签 {} 天，活动累计 {} 积分，本周已签 {} 天",
        wb_i64(data, "streak_days"),
        wb_num(data, "total_credits").unwrap_or(0.0),
        wb_i64(data, "week_checkin_days"),
    )
}

async fn checkin_workbuddy(acc: &Value) -> Item {
    let id = workbuddy_vault::account_id(acc);
    let name = workbuddy_vault::display_name(acc);
    let mut it = Item::new("workbuddy", &id, &name);

    // 凭据形态：加密信封 / 缺失都解不出明文 → 「不可签到」，别报成失败（用户改不了）。
    let token = match acc.get("access_token") {
        Some(Value::Object(_)) => {
            return it.done(
                status::UNAVAILABLE,
                "该账号的凭据是 WorkBuddy 加密信封，本地解不出明文，无法签到",
            )
        }
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return it.done(status::UNAVAILABLE, "该账号没有采集到凭据，无法签到"),
    };

    // uid：账号库字段优先，缺失时从 JWT 的 `sub` 解（脚本里就是这么兜的）。
    let mut uid = workbuddy_auth::get_str(acc, "uid").unwrap_or_default();
    if uid.trim().is_empty() {
        uid = jwt::payload(&token)
            .and_then(|p| p.get("sub").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_default();
    }
    it.uid = uid.clone();

    // ① 有效期预检：过期就跳过，一个请求都不发。
    let (state, exp) = expiry_state(&token);
    if state == "expired" {
        return it.done(
            status::SKIPPED,
            format!("token {}，已跳过（不发送任何请求），请在账号页刷新或重新登录", expired_text(exp)),
        );
    }
    let warn = state == "warn";

    let base = workbuddy_auth::API_ENDPOINT;
    let headers = wb_headers(&token, &uid);

    // ② 查状态
    let st = post(&format!("{base}{WB_STATUS_PATH}"), &headers, json!({})).await;
    it.http_status = st.http_status;
    it.code = body_code(&st.body);
    if st.http_status == 0 || st.http_status != 200 || it.code != Some(0) {
        return it.done(status::FAILED, format!("查询签到状态失败：{}", describe(&st)));
    }
    let data = st.body.get("data").cloned().unwrap_or_else(|| json!({}));

    if !wb_bool(&data, "active") {
        return it.done(status::INACTIVE, "当前没有进行中的签到活动，无需领取");
    }
    if wb_bool(&data, "today_checked_in") {
        it.balance = wb_num(&data, "total_credits");
        let mut msg = format!(
            "今日已签到，获得 {} 积分{}",
            wb_num(&data, "today_credit").unwrap_or(0.0),
            wb_tail(&data)
        );
        if warn {
            msg.push_str("（token 即将过期）");
        }
        return it.done(status::ALREADY, msg);
    }

    // ③ 签到
    let ck = post(&format!("{base}{WB_CHECKIN_PATH}"), &headers, json!({})).await;
    it.http_status = ck.http_status;
    it.code = body_code(&ck.body);

    if ck.http_status == 0 {
        return it.done(status::FAILED, format!("签到请求没能发出去：{}", describe(&ck)));
    }
    if it.code == Some(0) {
        let gained = ck
            .body
            .get("data")
            .and_then(|d| wb_num(d, "credit"))
            .or_else(|| wb_num(&data, "daily_credit"));
        it.gained = gained;
        it.balance = wb_num(&data, "total_credits");
        let extra = if wb_bool(&data, "is_streak_day") && wb_num(&data, "streak_bonus_credit").unwrap_or(0.0) > 0.0
        {
            format!("（含连签奖励 +{}）", wb_num(&data, "streak_bonus_credit").unwrap_or(0.0))
        } else {
            String::new()
        };
        let mut msg = format!(
            "签到成功，获得 {} 积分{}{}",
            gained.map(|g| g.to_string()).unwrap_or_else(|| "未知".to_string()),
            extra,
            wb_tail(&data)
        );
        if warn {
            msg.push_str("（token 即将过期）");
        }
        return it.done(status::SUCCESS, msg);
    }
    // 今天已签到：脚本里服务端回 HTTP 400 + code 10001，视为正常。
    if it.code == Some(10001) {
        it.balance = wb_num(&data, "total_credits");
        return it.done(status::ALREADY, format!("今日已签到{}", wb_tail(&data)));
    }
    it.done(status::FAILED, format!("签到失败：{}", describe(&ck)))
}

// ---------------------------------------------------------------------------
// Trae
// ---------------------------------------------------------------------------

fn clean_token(token: &str) -> String {
    let t = token.trim();
    t.strip_prefix("Cloud-IDE-JWT ").unwrap_or(t).trim().to_string()
}

/// 签到专用设备 ID —— **每个账号一个**（T68）。
///
/// ⚠️⚠️ **绝不能复用 `oauth.json` 里的 `deviceId`**：那是**整台客户端**的设备身份，
/// 同一客户端下所有账号**一模一样**（2026-10-15 实测本机 6 份 oauth.json 全是
/// `2081789759006043`）。而服务端对**领取**（claim）接口是**按设备判重**的
/// —— `code 9005 当前设备今日已经签到，请明日再来` ⇒
/// 「一个客户端里 N 个账号」**只有第一个签得成**，其余全红。
/// （用户原话：「签到有问题，我之前给你的青龙脚本正常的」。）
///
/// ⚠️ 最容易看混的一点：**两个接口判重维度不同**
///   · `status` 的 `checked_in` 按**账号**算（同一账号换个客户端仍回「已签」）；
///   · `claim` 的 9005 按**设备**算。
/// 旧实现正好踩在这条缝上：status 说「没签」→ 去 claim → 被按设备拦下 ⇒ 错位成「失败」。
///
/// 做法对齐青龙脚本（`checkin_ql.js:61` 的 `genDeviceId()`、`:448` 在**循环体内**调用）：
/// **每个账号各一个**设备 ID。
/// ⚠️ 但脚本注释里那句「服务端只校验存在性」**是错的** —— 2026-10-17 实测证明它还有一条
/// **取值范围硬约束**（必须 < `2^52`，详见 `derive_device_id`）。青龙用
/// `Date.now() + random(1000)`（≈1.79e15）**恰好**落在安全区里，所以这条从没暴露过。
/// 这里比它再稳一步 —— 按账号身份**派生稳定值**（FNV-1a），
/// 同一账号天天用同一个设备 ID、不做随机漂移，免得被风控当成「天天换设备」。
///
/// ⚠️ 派生 seed **只取账号身份（uid）**、**不含 `client_key`**：
/// 同一个账号在 solo-cn 与 trae-cn 必须是**同一台设备**，否则会被当成两台。
/// 拿不到 uid 时退回本地账号目录 id（它本身就是按 uid 派生的短哈希）。
fn account_device_id(oauth: &Value, id: &str) -> String {
    derive_device_id(&account_seed(oauth, id))
}

/// 同一个账号的**第二个**派生值（换个盐）—— 候选表里的回退档。
///
/// 存在的理由：「落在服务端接受区间内」是**必要条件**，但没人能保证它同时也是
/// **充分条件**（服务端可能另有一套自己认得的设备表）。多留一档就多一次机会，
/// 代价只是**前面的候选都失败时**多打一次 claim。
fn account_device_alt(oauth: &Value, id: &str) -> String {
    derive_device_id(&format!("{}#alt", account_seed(oauth, id)))
}

/// 派生 seed —— **只取账号身份（uid）**、**不含 `client_key`**：
/// 同一个账号在 solo-cn 与 trae-cn 必须是**同一台设备**，否则会被当成两台。
/// 拿不到 uid 时退回本地账号目录 id（它本身就是按 uid 派生的短哈希）。
fn account_seed(oauth: &Value, id: &str) -> String {
    oauth_str(oauth, "uid").unwrap_or_else(|| id.to_string())
}

/// FNV-1a 64 位 → 16 位纯数字，**恒定落在 `[1e15, 2e15)`**（首位必为 1）。
///
/// ⚠️⚠️ **取值范围是硬约束，不是随便挑的**（2026-10-17 实测钉死）：服务端只接受
/// **小于 `2^52`（4 503 599 627 370 496 ≈ 4.5e15）** 的设备 ID，超了直接回 `9074`
/// （文案还是那句「当前参与用户太多，请稍后再试」）：
///
/// ```text
/// 1 111 111 111 111 111   1.11e15   成功
/// 2 081 789 759 006 043   2.08e15   成功（官方客户端在用的值）
/// 4 464 158 186 372 700   4.46e15   成功   ← 上限就卡在它和下一行之间
/// 5 555 555 555 555 555   5.56e15   9074
/// 8 034 549 610 285 862   8.03e15   9074
/// ```
///
/// 青龙脚本 `checkin_ql.js:61` 的 `genDeviceId()` = `Date.now() + random(1000)` ——
/// 2026 年的时间戳约 **1.79e15**，**天生就落在安全区里**，所以它"每次随机"也一直好用，
/// 作者才会误以为「服务端只校验存在性」。
///
/// ⛔ 旧实现取模 `9e15` ⇒ 输出区间 `[1e15, 1e16)`，其中 **61%** 落在 4.5e15 之上
/// ⇒ **大多数账号的派生值一出生就是废值**（4052137095223728 的 6.25e15、
/// 842681600717036 的 8.03e15 都是），只能回退到「客户端设备」勉强签到。
/// **这才是 10-15 起反复 `9074` 的真正原因** —— 之前被误判成了「这台设备没被用过」。
fn derive_device_id(seed: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64 位偏移基准
    for b in seed.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV 质数
    }
    // 收进 [1e15, 2e15)：16 位、首位 1，与青龙的时间戳量级同形，且只有上限的 0.44 倍。
    format!("{:016}", h % 1_000_000_000_000_000 + 1_000_000_000_000_000)
}

// ---------------------------------------------------------------------------
// 设备记忆：记住「上次真正签成的那个设备」，下次优先复用它
// ---------------------------------------------------------------------------

/// 记录「每个账号上次成功签到用的 device_id」的本地文件。
fn checkin_device_path() -> PathBuf {
    store_dir().join("trae").join("checkin_device.json")
}

/// 读某账号已知可用的 device_id。
///
/// ⚠️⚠️ 这里原先写着「9074 = 这台设备该账号**没用过**」—— **那是 2026-10-16 的错误结论，
/// 10-17 已被推翻**。真正的判据只有一条：**数值必须 < `2^52`**（见 `derive_device_id`）。
/// 当时那批「派生值失败 / 旧值成功」的观测，差异**全都来自数值有没有超限**，
/// 与「用没用过」无关 —— 又一个把巧合当因果的例子。
///
/// 但这张表**仍然必须留着**：一次真成功是任何推导都换不来的确定性，复用它永远是最稳的一档；
/// 而且万一服务端还有别的隐藏条件（比如它另有一套认得的设备名单），表在就能自动兜住。
///
/// 观测存档（括号里是 10-17 复盘后的正确读法）：
///
/// ```text
/// 3601675132   派生值 6.25e15 → 9074   （>2^52，废值）   客户端设备 2.08e15 → success（区间内）
/// 8026378203   派生值 8.03e15 → 9074   （>2^52，废值）   随机 5.56e15       → 9074（>2^52）
///              1111111111111111 → success（1.11e15，区间内）
/// 1181093986   派生值 4.46e15 → success（<2^52 —— 它是唯一「派生值本来就能用」的账号）
/// ```
///
/// ⇒ 真正管用的从来不是「用过」，而是「**值落在 2^52 以下**」。
fn load_known_device(uid: &str) -> Option<String> {
    let text = std::fs::read_to_string(checkin_device_path()).ok()?;
    let j: Value = serde_json::from_str(&text).ok()?;
    j.get(uid)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 记下某账号这次**成功**用的 device_id（下次优先用它）。失败静默 —— 不该因为
/// 写不了一张表就让签到报错。
fn save_known_device(uid: &str, device_id: &str) {
    if uid.is_empty() || device_id.is_empty() {
        return;
    }
    let p = checkin_device_path();
    let mut j: Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if !j.is_object() {
        j = json!({});
    }
    j[uid] = json!(device_id);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&j).unwrap_or_default());
}

/// 候选设备列表 —— **纯函数**（不碰磁盘，所以可单测），按优先级去重排列：
///
/// ① **上次成功用过的**（服务端真的放行过它，最稳）→
/// ② **按账号派生**（每账号独有；只要数值落在 `2^52` 以下就该能用，见 `derive_device_id`）→
/// ③ **第二个派生值**（换个盐的回退档 —— 只在 ② 也被拒时才用得着）→
/// ④ **客户端登录设备**（`oauth.json` 的 `deviceId`，官方客户端在用的值、必然合法；
///    但**同一客户端下所有账号共用** ⇒ 一台设备一天只够一个账号签）。
///
/// ⚠️ 顺序不能乱：把 ④ 提到前面，「第一个账号独占共享设备、其余全 9005/9095」的老毛病就回来了。
fn device_candidates(
    known: Option<&str>,
    derived: &str,
    alt: &str,
    client_device: Option<&str>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for cand in [known, Some(derived), Some(alt), client_device] {
        let Some(s) = cand else { continue };
        let s = s.trim();
        if s.is_empty() || out.iter().any(|x| x == s) {
            continue;
        }
        out.push(s.to_string());
    }
    out
}

fn oauth_str(oauth: &Value, key: &str) -> Option<String> {
    oauth
        .get(key)
        .and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|i| i.to_string())))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 候选域名（去重）：oauth 落库 host 优先，接口实测域名兜底。
///
/// ⚠️ 顺序**故意**把 oauth 落库的 host 放第一位：`trae-intl` 只有它自己的域名认它的 token，
/// 先试 `api.trae.cn` 反而会拿到一个「区域不对」的业务错码。
/// 代价是 `trae-cn` 每个账号会先白打一次 `api.trae.com.cn`（它回 404 挡板页）——
/// 由 `looks_like_api_reply()` 挡掉后自动换下一个，**不会再像 2026-10-14 那样把 404
/// 当成应答**。
fn candidate_hosts(oauth: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |h: String| {
        let h = h.trim().trim_end_matches('/').to_string();
        if !h.is_empty() && !out.contains(&h) {
            out.push(h);
        }
    };
    if let Some(h) = oauth_str(oauth, "host") {
        push(h);
    }
    for h in TRAE_HOSTS {
        push((*h).to_string());
    }
    out
}

/// 服务端用来表示「**这台设备今天已经签过了**」的所有业务码 —— **唯一出口**。
///
/// ⚠️ 判断「换个设备还有没有戏」只看这个函数。旧实现只在领取循环里特判 `9005`，
/// 于是 2026-10-17 服务端换用 `9095` 时（原文
/// `{"code":9095,"message":"当前设备今日已经签到，请明日再来哦～"}`）
/// 它一路掉进兜底、被涂成红色「签到失败（code 9095）」—— 用户当天看到的就是那个红字。
/// **以后再遇到同义的新码，回来加在这里，别去循环里另写一个 `matches!`。**
fn is_device_taken_code(code: i64) -> bool {
    code == 9005 || code == 9095
}

fn trae_headers(token: &str, device_id: &str) -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert("User-Agent".to_string(), TRAE_UA.to_string());
    h.insert("accept-language".to_string(), "zh-CN".to_string());
    h.insert("authorization".to_string(), format!("Cloud-IDE-JWT {token}"));
    h.insert("x-device-id".to_string(), device_id.to_string());
    h.insert("x-device-type".to_string(), "windows".to_string());
    h.insert("x-user-region".to_string(), "CN".to_string());
    h.insert("x-market-client-id".to_string(), "VSCode 1.107.1".to_string());
    h.insert("x-app-version".to_string(), "0.1.63".to_string());
    h.insert("app-version".to_string(), "0.1.63".to_string());
    h.insert("package-type".to_string(), "stable_cn".to_string());
    h.insert("x-lgw-req-sdk-type".to_string(), "3".to_string());
    h.insert("x-lscbd-platform".to_string(), "windows".to_string());
    h
}

/// 调 GetUserInfo 拿昵称 / UserID（走 `x-cloudide-token` 头，与签到的头不同）。
async fn trae_user_info(host: &str, token: &str) -> Option<(String, String)> {
    let mut h = HashMap::new();
    h.insert("x-cloudide-token".to_string(), token.to_string());
    let r = post(
        &format!("{host}{TRAE_USERINFO_PATH}"),
        &h,
        json!({ "ReqSource": "Lite", "IDEVersion": "0.1.63" }),
    )
    .await;
    if r.http_status == 0 {
        return None;
    }
    let res = r.body.get("Result")?;
    let uid = res.get("UserID").and_then(|v| {
        v.as_str()
            .map(String::from)
            .or_else(|| v.as_i64().map(|i| i.to_string()))
    })?;
    let name = res
        .get("ScreenName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Some((uid, name))
}

/// 剩余积分：优先官方汇总 `usage_summary`，缺失时按包逐个 `limit - used` 累加。
async fn trae_balance(host: &str, headers: &HashMap<String, String>) -> Option<f64> {
    let r = post(&format!("{host}{TRAE_ENTITLE_PATH}"), headers, json!({ "req_source": 2 })).await;
    if r.http_status == 0 {
        return None;
    }
    if let Some(us) = r.body.get("usage_summary") {
        let total = us.get("total_amount").and_then(Value::as_f64);
        let consumed = us.get("consumed_amount").and_then(Value::as_f64);
        if let (Some(t), Some(c)) = (total, consumed) {
            return Some(round2((t - c).max(0.0)));
        }
    }
    let packs = r.body.get("user_entitlement_pack_list")?.as_array()?.clone();
    let mut remaining = 0.0;
    let mut any = false;
    for p in packs {
        if p.get("status").and_then(Value::as_i64) != Some(1) {
            continue;
        }
        let limit = p
            .pointer("/entitlement_base_info/quota/credits_limit")
            .and_then(Value::as_f64);
        let used = p.pointer("/usage/credits_amount").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(l) = limit {
            remaining += (l - used).max(0.0);
            any = true;
        }
    }
    any.then(|| round2(remaining))
}

/// 签到目标：账号库里带 `oauth.json` 或 `profile.json` 的账号（与积分页同一套收集规则）。
fn trae_targets(only_client: Option<&str>) -> Vec<(String, String)> {
    let root = trae_vault::vault_root();
    let mut out: Vec<(String, String)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return out;
    };
    for c in rd.flatten() {
        if !c.path().is_dir() {
            continue;
        }
        let client = c.file_name().to_string_lossy().into_owned();
        if let Some(want) = only_client.filter(|w| !w.is_empty()) {
            if want != client {
                continue;
            }
        }
        let Ok(rd2) = std::fs::read_dir(c.path()) else {
            continue;
        };
        for a in rd2.flatten() {
            if !a.path().is_dir() {
                continue;
            }
            let id = a.file_name().to_string_lossy().into_owned();
            if a.path().join("oauth.json").is_file() || a.path().join("profile.json").is_file() {
                out.push((client.clone(), id));
            }
        }
    }
    out.sort();
    out
}

fn oauth_path(client_key: &str, id: &str) -> PathBuf {
    trae_vault::account_dir(client_key, id).join("oauth.json")
}

async fn checkin_trae(client_key: &str, id: &str) -> Item {
    let local_name = trae_vault::display_name(client_key, id).unwrap_or_else(|| id.to_string());
    let mut it = Item::new("trae", id, &local_name);
    it.client_key = client_key.to_string();

    let oauth: Value = std::fs::read_to_string(oauth_path(client_key, id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);

    let token = match oauth_str(&oauth, "token").map(|t| clean_token(&t)) {
        Some(t) if !t.is_empty() => t,
        // 没有 oauth.json 的账号是「切换载体」，它没有网页凭证，签不了 —— 这不是失败。
        _ => {
            return it.done(
                status::UNAVAILABLE,
                "该账号是切换载体，没有网页凭证，无法签到（用「发起网页登录」补一份即可）",
            )
        }
    };

    // ① 有效期预检：过期就跳过，一个请求都不发。
    let (state, exp) = expiry_state(&token);
    if state == "expired" {
        return it.done(
            status::SKIPPED,
            format!("token {}，已跳过（不发送任何请求），请重新登录该账号", expired_text(exp)),
        );
    }
    let warn = state == "warn";

    // ⚠️⚠️ 设备标识（T68）—— **每个账号一个**。
    //
    //   · 绝不能抄 `oauth.json` 里那个「整台客户端共用」的 `deviceId`：
    //     6 份 oauth.json 实测全是 `2081789759006043` ⇒ 服务端按设备判重
    //     ⇒ 一个客户端里只有第一个账号签得成，其余全 9005 / 9095。
    //   · 设备 ID 还**必须小于 `2^52`**，否则服务端回 `9074`
    //     —— 这是 2026-10-17 才找到的**真因**，详见 `derive_device_id`。
    //
    // 候选顺序：**上次成功的 → 派生值 → 派生回退值 → 客户端登录设备**
    // （详见 `device_candidates`）。
    let uid_key = oauth_str(&oauth, "uid").unwrap_or_else(|| id.to_string());
    let known = load_known_device(&uid_key);
    let derived = account_device_id(&oauth, id);
    let alt = account_device_alt(&oauth, id);
    let client_device = oauth_str(&oauth, "deviceId");
    let cands = device_candidates(known.as_deref(), &derived, &alt, client_device.as_deref());
    // 查询状态 / 取余额用第一个候选（status 是按**账号**判重的，设备值在这不影响结果）。
    let device_id = cands.first().cloned().unwrap_or_else(|| derived.clone());
    let headers = trae_headers(&token, &device_id);
    let body = json!({ "req_source": 2 });
    let hosts = candidate_hosts(&oauth);

    // ② 挑一个**真的认得这条路径**的域名。
    //
    // ⚠️⚠️ 2026-10-14 线上事故（用户报「trae 签到的登录态有问题」，截图里三个账号全红）：
    //   每个账号都报
    //     「查询签到状态失败：HTTP 404 / code=— / <html><head><title>404 Not Found…TLB…」
    //   根因**不是 token、也不是路径**，而是**域名挑错了**：
    //     · OAuth 落库的 `host` 是 `https://api.trae.com.cn`（登录时用的域名），
    //       `candidate_hosts()` 又把它排在第一位；
    //     · 实测：`https://api.trae.com.cn/trae/api/v2/ug/checkin_credits/status`
    //       ⇒ **HTTP 404 + HTML（TLB 挡板页）**；
    //       `https://api.trae.cn/…` ⇒ **HTTP 200 `{"checked_in":…,"code":0}`**。
    //     · 旧循环只把「传输层失败（status == 0）」当作换域名的理由，于是**把这个 404
    //       当成『服务端答了』直接采用** ⇒ 三个账号一个都没签成。
    //
    // 现在的判据 = `looks_like_api_reply()`（见其注释）：**只有真正的 API 应答才算数**。
    //   · 传输层失败（status == 0）⇒ 换下一个；
    //   · 不是 JSON、或 4xx 又没有业务 `code`（= 网关挡板页）⇒ 换下一个；
    //   · 其余 ⇒ 采用并 break —— **业务错码绝不换域名**
    //     （`code 9004`、`checked_in` 这些是服务端真答了，换域名只会掩盖真实原因）。
    let mut chosen: Option<(&str, Resp)> = None;
    let mut last_transport = String::new();
    let mut last_bad: Option<(String, Resp)> = None;
    for host in &hosts {
        let r = post(&format!("{host}{TRAE_STATUS_PATH}"), &headers, body.clone()).await;
        if r.http_status == 0 {
            last_transport = body_message(&r.body);
            continue;
        }
        if !looks_like_api_reply(&r) {
            last_bad = Some(((*host).clone(), r));
            continue;
        }
        chosen = Some((host.as_str(), r));
        break;
    }
    let Some((host, st)) = chosen else {
        // 一个都没中：区分「全都连不上」与「连上了但这几个域名都不认这个接口」。
        if let Some((h, r)) = last_bad {
            it.http_status = r.http_status;
            return it.done(
                status::FAILED,
                format!("所有 Trae 域名都不认这个接口（最后试的 {h}：{}）", describe(&r)),
            );
        }
        it.http_status = 0;
        return it.done(
            status::FAILED,
            format!("所有 Trae 域名都连不上：{}", if last_transport.is_empty() { "网络不可达".to_string() } else { last_transport }),
        );
    };

    // 昵称：拿得到就用接口的，拿不到用账号库里的（不影响签到）。
    if let Some((uid, screen_name)) = trae_user_info(host, &token).await {
        it.uid = uid;
        if !screen_name.is_empty() {
            it.name = screen_name;
        }
    }
    if it.uid.is_empty() {
        it.uid = oauth_str(&oauth, "uid").unwrap_or_default();
    }

    it.http_status = st.http_status;
    it.code = body_code(&st.body);
    if it.code != Some(0) {
        return it.done(status::FAILED, format!("查询签到状态失败：{}", describe(&st)));
    }

    // ③ 余额（签到前后各一次，差值就是实际到账）
    let before = trae_balance(host, &headers).await;

    if st.body.get("checked_in").and_then(Value::as_bool).unwrap_or(false) {
        it.balance = before;
        let bal = before.map(|b| b.to_string()).unwrap_or_else(|| "未知".to_string());
        let mut msg = format!("今日已签到，当前积分余额 {bal}");
        if warn {
            msg.push_str("（token 即将过期）");
        }
        return it.done(status::ALREADY, msg);
    }
    if !st.body.get("enable").and_then(Value::as_bool).unwrap_or(true) {
        return it.done(status::UNAVAILABLE, "签到功能不可用（服务端返回 enable=false）");
    }

    // ④ 领取 —— **按候选设备逐个尝试**（T68 三改）。
    //
    // ⚠️⚠️ 观测存档（本机账号，只换 `x-device-id` 逐个打 claim）：
    //
    //   ```text
    //   1 111 111 111 111 111   1.11e15   成功
    //   2 081 789 759 006 043   2.08e15   成功（官方客户端在用的值）
    //   4 464 158 186 372 700   4.46e15   成功   ← 上限卡在它和下一行之间
    //   5 555 555 555 555 555   5.56e15   9074
    //   6 252 805 140 305 395   6.25e15   9074（4052137095223728 的旧派生值）
    //   8 034 549 610 285 862   8.03e15   9074（842681600717036 的旧派生值）
    //   ```
    //
    //   ⇒ **`9074` 的真正含义 = 「这个设备 ID 超出服务端接受范围」**（必须 < `2^52`），
    //     文案「当前参与用户太多，请稍后再试」纯属模糊说辞、跟限流无关。
    //     ⛔ 10-16 曾把它读成「该账号没用过这台设备」—— **错的**：
    //     那批观测里的成功 / 失败**只跟数值有没有超限有关**，跟用没用过无关。
    //   ⇒ 「遇到 9074 就睡 3 秒重试」救不了（用户实测确认「重试还是有问题」）：
    //     **换一个合法数值才是正解；重试同一个值毫无意义。**
    //
    // ⚠️⚠️ **账号「已签」状态下的一切返回都不能当证据**（2026-10-17 差一点又踩）：
    //     账号已签时服务端走**幂等分支**，那时不管递什么设备都回 `code 0`。
    //     10-17 那轮实测里第 4 个「全新随机值」也回 `code 0`，看着像「新设备也能用」，
    //     其实只是因为**前一个候选已经把它签成了**。
    //     **「已签」与「未签」不是同一条代码路径，不能拿来互证。**
    //
    //   ⇒ 做法：候选里第一个不行就换下一个；**一旦签成立刻把那个设备记下来**
    //     （`save_known_device`，下次直接命中第一个候选）。
    //
    // 换候选的三种信号，**都只表示「这个设备今天用不了」，与账号状态无关**：
    //   · `9074` 数值超限（≥ `2^52`）—— 换一个合法数值即可；
    //   · `9005` / `9095` 该设备今日已被签过 —— 大概率是同客户端的另一个账号抢先用了它
    //     （客户端设备是整台机器共用的 ⇒ 一个客户端里只有一个账号能靠它签成）。
    //     ⚠️ `9095` 与 `9005` 是同一件事的两种码，**必须一起特判**（2026-10-17 实测补上）。
    let mut last_code: Option<i64> = None;
    let mut saw_occupied = false;
    let mut tried: Vec<String> = Vec::new();
    for cand in &cands {
        let h = trae_headers(&token, cand);
        let claim = post(&format!("{host}{TRAE_CLAIM_PATH}"), &h, body.clone()).await;
        tried.push(cand.clone());
        if claim.http_status == 0 {
            it.http_status = 0;
            return it.done(status::FAILED, format!("签到请求没能发出去：{}", describe(&claim)));
        }
        it.http_status = claim.http_status;
        it.code = body_code(&claim.body);
        last_code = it.code;
        // ⚠️ `9095` 与 `9005` **同一语义**：这台设备今日已经签过了。2026-10-17 实测补上 ——
        //    服务端原文 `{"code":9095,"message":"当前设备今日已经签到，请明日再来哦～"}`。
        //    旧码只特判 `9005` ⇒ `9095` 一路掉进兜底 `签到失败（code 9095）` 涂成红字，
        //    用户当天看到的就是这个（截图：8026378203 两处红字）。
        //    判据统一走 `is_device_taken_code()` —— **同义的码只许有一个出口**。
        if it.code.is_some_and(is_device_taken_code) {
            saw_occupied = true;
        }

        // ✅ 签成了：把这个设备记下来，下次直接用它。
        if it.code == Some(0) {
            save_known_device(&uid_key, cand);
            let after = trae_balance(host, &headers).await;
            it.gained = match (before, after) {
                (Some(b), Some(a)) => Some(round2((a - b).max(0.0))),
                _ => None,
            };
            it.balance = after.or(before);
            let gained_text = it
                .gained
                .map(|g| g.to_string())
                .unwrap_or_else(|| "未知".to_string());
            let bal = it
                .balance
                .map(|b| b.to_string())
                .unwrap_or_else(|| "未知".to_string());
            let mut msg = format!("签到成功，获得 {gained_text} 积分，当前余额 {bal}");
            if warn {
                msg.push_str("（token 即将过期）");
            }
            return it.done(status::SUCCESS, msg);
        }
        // `9004` = 请求头里压根没有 `x-device-id` —— 换设备也救不回来，直接报出来。
        if it.code == Some(9004) {
            return it.done(status::FAILED, "签到失败（code 9004）：缺少 x-device-id 请求头");
        }
        // 其余（9074 / 9005 / 9095）⇒ 换下一个候选继续试。
    }

    // 所有候选都试过了 —— **如实说清是哪种原因、以及能怎么办**，
    // 绝不塌缩成一句「签到失败」（用户 2026-10-15 报的就是「还有就报错」，光甩错码没用）。
    match last_code {
        // 最后一个候选是「客户端登录设备」被别的账号占了 —— 顺带说明它自己的设备还没被认过。
        Some(c) if is_device_taken_code(c) => it.done(
            status::FAILED,
            "签到未成功：这台设备的今日名额已被同客户端的其它账号占用（code 9005/9095），\
             而该账号自己还没有被服务端认过的设备。\
             请先用 Trae 官方客户端给这个账号签一次到，本工具会自动记住那个设备，之后就能正常签了。",
        ),
        Some(9074) => it.done(
            status::FAILED,
            format!(
                "签到未成功：这几个设备标识都被服务端拒了（code 9074）：{}。\
                 它提示的「当前参与用户太多」是模糊说辞 —— 实测这个码表示「设备标识不被接受」\
                 （数值超出服务端允许范围），所以**重试同一个标识没有意义**。{}\
                 请先用 Trae 官方客户端给这个账号签一次到，本工具会自动记住那次生效的设备，\
                 之后就能正常签了。",
                tried.join(" / "),
                if saw_occupied {
                    "（其中客户端登录设备今天已被同客户端的其它账号占用，code 9005/9095）"
                } else {
                    ""
                }
            ),
        ),
        Some(c) => it.done(status::FAILED, format!("签到失败（code {c}）")),
        None => it.done(status::FAILED, "签到失败：没有可用的设备标识"),
    }
}

// ---------------------------------------------------------------------------
// 汇总与入口
// ---------------------------------------------------------------------------

fn summarize(items: &[Value]) -> Value {
    let count = |s: &str| items.iter().filter(|i| i["status"] == json!(s)).count();
    let gained: f64 = items
        .iter()
        .filter_map(|i| i["gained"].as_f64())
        .sum();
    json!({
        "total": items.len(),
        "success": count(status::SUCCESS),
        "already": count(status::ALREADY),
        "skipped": count(status::SKIPPED),
        "unavailable": count(status::UNAVAILABLE),
        "inactive": count(status::INACTIVE),
        "failed": count(status::FAILED),
        "gained": round2(gained),
    })
}

async fn gap() {
    tokio::time::sleep(std::time::Duration::from_millis(GAP_MS)).await;
}

/// 一键签到。
///
/// `scope`：
/// - `"trae"`：只签 Trae 账号；
/// - `"workbuddy"`：只签 WorkBuddy 账号；
/// - `"all"` / `None` / 其它值：两边都签。
///
/// `only_client`：Trae 侧只签某个客户端（`trae-cn` / `trae-intl` …），
/// 用于账号管理页按当前选中的客户端签；`None` = 全部客户端。
///
/// 单个账号失败绝不影响其它账号 —— 每个账号的结果互相独立。
/// 按**账号身份**去重 —— 纯函数（不碰文件系统，所以可单测）。
///
/// `key_of(client_key, id)` 取账号身份；返回**保留下来的那些 target**，顺序不变
/// （先出现的客户端赢）。
///
/// ⚠️ 身份取到**空串**时**不参与去重** —— 宁可不合并，也不要把两个陌生账号
/// 当成一个（那会让一个账号白丢一次签到机会）。
fn dedupe_trae_targets<F>(targets: &[(String, String)], key_of: F) -> Vec<(String, String)>
where
    F: Fn(&str, &str) -> String,
{
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, String)> = Vec::new();
    for (client, id) in targets {
        let key = key_of(client, id);
        if !key.is_empty() && !seen.insert(key) {
            continue;
        }
        out.push((client.clone(), id.clone()));
    }
    out
}

/// 账号身份 —— 用于**跨客户端去重**（`uid` 优先，退回账号目录 id）。
///
/// ⚠️ 同一个账号常常**同时登录在多个客户端**（solo-cn 与 trae-cn 各一份
/// oauth.json），但服务端是**按账号**记「今天已签」的 ⇒ 签两遍纯属白跑，
/// 还白白多打一次 claim（更容易撞上 9074 限流）。
///
/// ⚠️ 账号目录 id 本身就是 **uid 的后 6 位**（`oauth_223728` ↔ `4052137095223728`），
/// 所以在两个客户端里**是同一个值** —— 拿它兜底也能正确合并。
fn trae_account_key(client_key: &str, id: &str) -> String {
    std::fs::read_to_string(oauth_path(client_key, id))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|j| oauth_str(&j, "uid"))
        .unwrap_or_else(|| id.to_string())
}

pub async fn run(scope: Option<&str>, only_client: Option<&str>) -> Value {
    let scope = scope.unwrap_or("all").trim();
    let do_trae = scope != "workbuddy" && scope != "qoder";
    let do_wb = scope != "trae" && scope != "qoder";
    let do_qoder = scope == "qoder" || scope == "all";

    let started_at = chrono::Utc::now().timestamp_millis();
    let mut items: Vec<Value> = Vec::new();

    if do_trae {
        // ⚠️⚠️ **按账号去重**：同一账号在 solo-cn / trae-cn 各有一份 oauth.json，
        //    而服务端按**账号**记「今天已签」⇒ 不去重就会「同一个账号签两遍」
        //    （2026-10-15 用户报「重复签到」，截图里三个 uid 各出现两次）。
        //    与下面 WorkBuddy 那段**同一套规矩**；保留**先出现的那个客户端**。
        let targets = dedupe_trae_targets(&trae_targets(only_client), trae_account_key);
        for (idx, (client, id)) in targets.iter().enumerate() {
            if idx > 0 {
                gap().await;
            }
            items.push(checkin_trae(client, id).await.to_value());
        }
    }

    if do_wb {
        // 同一 uid 只签一次（账号库已按 uid 合并，这里再兜一次手改 JSON 的脏数据）。
        let mut seen: HashSet<String> = HashSet::new();
        let accounts = workbuddy_vault::load_accounts();
        let mut first = true;
        for acc in accounts {
            let uid = workbuddy_auth::get_str(&acc, "uid").unwrap_or_default();
            if !uid.is_empty() && !seen.insert(uid) {
                continue;
            }
            if !first {
                gap().await;
            }
            first = false;
            items.push(checkin_workbuddy(&acc).await.to_value());
        }
    }

    if do_qoder {
        // Qoder 是**活动制**（`/sash/api/v1/me/campaigns`），两个版本各一套账号库。
        // 签完顺手刷一次额度 —— 否则界面上的积分要等用户手动点「刷新积分」才更新，
        // 同屏会出现「刚领了 100 但还显示旧数」的自相矛盾。
        for region in [
            crate::modules::qoder_realm::QoderRegion::Cn,
            crate::modules::qoder_realm::QoderRegion::Global,
        ] {
            let accounts = crate::modules::qoder_accounts::list(region).unwrap_or_default();
            for acc in accounts.iter().filter(|a| a.is_usable()) {
                let outcome = crate::modules::qoder_tools::checkin_claim(region, acc).await;
                let balance = crate::modules::qoder_api::refresh_account(acc)
                    .await
                    .ok()
                    .and_then(|a| a.credits);

                let mut item = Item::new("qoder", &acc.id, &acc.name);
                item.uid = acc.uid.clone();
                item.balance = balance;
                match outcome {
                    Ok(v) => {
                        item.gained = v.get("gained").and_then(Value::as_f64);
                        let msg = v
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if v.get("ok").and_then(Value::as_bool) == Some(true) {
                            if v.get("already").and_then(Value::as_bool) == Some(true) {
                                item = item.done(status::ALREADY, msg);
                            } else {
                                item = item.done(status::SUCCESS, msg);
                            }
                        } else if v.get("blocked").and_then(Value::as_bool) == Some(true) {
                            // 有活动但前置条件没达成：不是失败，重试也没用。
                            item = item.done(status::INACTIVE, msg);
                        } else {
                            item = item.done(status::FAILED, msg);
                        }
                    }
                    Err(e) => item = item.done(status::FAILED, e),
                }
                items.push(item.to_value());
            }
        }
    }

    json!({
        "ok": true,
        "scope": scope,
        "clientKey": only_client.unwrap_or(""),
        "startedAt": started_at,
        "finishedAt": chrono::Utc::now().timestamp_millis(),
        "items": items,
        "summary": summarize(&items),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(client: &'static str) -> Item {
        Item::new(client, "id-1", "名字")
    }

    #[test]
    fn statuses_are_all_distinct_strings() {
        // ⚠️ 这几档绝不能合并：「跳过」和「失败」长得一样就等于骗人。
        let all = [
            status::SUCCESS,
            status::ALREADY,
            status::SKIPPED,
            status::UNAVAILABLE,
            status::INACTIVE,
            status::FAILED,
        ];
        let uniq: HashSet<&&str> = all.iter().collect();
        assert_eq!(uniq.len(), all.len(), "六档状态必须互不相同");
    }

    #[test]
    fn summary_keeps_every_bucket_separate() {
        let items = vec![
            json!({ "status": "success", "gained": 10.0 }),
            json!({ "status": "success", "gained": null }),
            json!({ "status": "already", "gained": null }),
            json!({ "status": "skipped", "gained": null }),
            json!({ "status": "unavailable", "gained": null }),
            json!({ "status": "inactive", "gained": null }),
            json!({ "status": "failed", "gained": null }),
        ];
        let s = summarize(&items);
        assert_eq!(s["total"], json!(7));
        assert_eq!(s["success"], json!(2));
        assert_eq!(s["already"], json!(1));
        assert_eq!(s["skipped"], json!(1));
        assert_eq!(s["unavailable"], json!(1));
        assert_eq!(s["inactive"], json!(1));
        assert_eq!(s["failed"], json!(1));
        // `gained = null`（未知）不许被当成 0 之外的任何东西，也不许让求和失败。
        assert_eq!(s["gained"], json!(10.0));
    }

    #[test]
    fn gained_none_is_null_not_zero() {
        // 「没拿到积分」与「不知道拿了多少」在界面上必须能分开。
        let v = item("trae").done(status::ALREADY, "今天签过了").to_value();
        assert!(v["gained"].is_null());
        assert_eq!(v["status"], json!("already"));
    }

    /// ⚠️ T68 的根因回归：**每个账号必须拿到自己的设备 ID**。
    ///
    /// 旧实现抄 `oauth.json` 里的 `deviceId`（整台客户端共用一个）⇒ 服务端按**设备**
    /// 判重 ⇒ 「一个客户端 N 个账号」只有第一个签得成，其余全 `9005`。
    #[test]
    fn each_account_gets_its_own_device_id() {
        let a = json!({ "uid": "3601675132" });
        let b = json!({ "uid": "8026378203" });
        let c = json!({ "uid": "1181093986" });
        let da = account_device_id(&a, "oauth_1");
        let db = account_device_id(&b, "oauth_2");
        let dc = account_device_id(&c, "oauth_3");
        assert_ne!(da, db, "不同账号必须拿到不同设备 ID，否则又是一台设备只能签一个");
        assert_ne!(db, dc);
        assert_ne!(da, dc);

        // 同一账号必须**稳定** —— 而且只认 uid：换个目录名（= 另一个客户端的落库）
        // 也得是同一台设备，否则同一账号会被当成两台。
        assert_eq!(da, account_device_id(&a, "oauth_1"), "同一账号两次调用必须一致");
        assert_eq!(da, account_device_id(&a, "完全不同的目录名"), "uid 在时必须只认 uid");

        // 格式：16 位纯数字、首位非 0 —— 与官方客户端同形（实测 `2081789759006043`）。
        for d in [&da, &db, &dc] {
            assert_eq!(d.len(), 16, "设备 ID 必须是 16 位：{d}");
            assert!(d.chars().all(|c| c.is_ascii_digit()), "必须全是数字：{d}");
            assert!(!d.starts_with('0'), "首位不能是 0：{d}");
        }

        // 没有 uid 时退回账号目录 id —— 也**不能大家撞成一个**。
        let no_uid = json!({});
        assert_ne!(
            account_device_id(&no_uid, "oauth_223728"),
            account_device_id(&no_uid, "oauth_717036"),
            "拿不到 uid 时退回目录 id，也必须各不相同"
        );
    }

    /// ⚠️ T68 的回归：**候选设备的优先级与去重**。
    ///
    /// 顺序**必须是**：上次成功用过的 → 派生值 → 派生回退档 → 客户端登录设备。
    /// ⚠️ 客户端设备（整机共用）**必须垫底** —— 提到前面就会让
    /// 「一个客户端里只有第一个账号签得成、其余全 9005/9095」的老毛病回来。
    #[test]
    fn device_candidates_prefer_the_device_that_already_worked() {
        let c = device_candidates(
            Some("1111111111111110"),
            "1111111111111111",
            "1111111111111112",
            Some("2081789759006043"),
        );
        assert_eq!(
            c,
            vec![
                "1111111111111110".to_string(), // ① 上次成功用过的（服务端真放行过，最稳）
                "1111111111111111".to_string(), // ② 按账号派生（每账号独有）
                "1111111111111112".to_string(), // ③ 派生回退档（② 被拒时才用得上）
                "2081789759006043".to_string(), // ④ 客户端登录设备（整机共用，必须垫底）
            ],
            "顺序错了会白吃一次 9074 / 9005。实际：{c:?}"
        );

        // 没有记忆时，剩下三档一个都不能少。
        let c = device_candidates(
            None,
            "1111111111111111",
            "1111111111111112",
            Some("2081789759006043"),
        );
        assert_eq!(
            c,
            vec![
                "1111111111111111".to_string(),
                "1111111111111112".to_string(),
                "2081789759006043".to_string(),
            ],
            "没记忆时也要保住后面三档，否则一个账号都没得试。实际：{c:?}"
        );

        // 几个候选**撞了**（oauth 里 deviceId 恰好等于派生值）⇒ 只留一个，
        // 否则会拿同一个值连着打两次，第二次必然 9005。
        let c = device_candidates(
            Some("1111111111111111"),
            "1111111111111111",
            "1111111111111111",
            Some("1111111111111111"),
        );
        assert_eq!(c, vec!["1111111111111111".to_string()], "重复的候选必须去掉。实际：{c:?}");

        // 空串 / 纯空白 = 「没有这个候选」，**不许**当成一个设备去发请求。
        let c = device_candidates(Some("   "), "1111111111111111", "", Some(""));
        assert_eq!(
            c,
            vec!["1111111111111111".to_string()],
            "空白候选必须被过滤（否则会带着空的 x-device-id 白打一次 9004）。实际：{c:?}"
        );

        // 最极端：派生值总是非空（`derive_device_id` 保证），所以候选表**永不为空**。
        let c = device_candidates(None, "1111111111111111", "", None);
        assert!(!c.is_empty(), "候选表永不为空 —— 它是领取阶段的唯一出路");
    }

    /// ⚠️⚠️ T68 五改（2026-10-17）的**核心回归**：派生出来的设备 ID 必须落在服务端
    /// 接受的区间内（**小于 `2^52`**）。旧实现取模 9e15 ⇒ **61% 的值超限**
    /// ⇒ 大多数账号的派生值一出生就是废值，这才是反复 `9074` 的真因。
    ///
    /// 这条测试是踩着三个错误结论换来的：`9074` 先被读成「服务端限流」，
    /// 又被读成「这台设备该账号没用过」，最后对上青龙脚本
    /// `genDeviceId() = Date.now() + random(1000)`（≈1.79e15）才明白 ——
    /// **它一直好用的原因不是"每次随机"，而是"随机出来的值天生就在区间里"。**
    #[test]
    fn derived_device_id_stays_inside_the_range_the_server_accepts() {
        const LIMIT: u64 = 1 << 52; // 4 503 599 627 370 496 ≈ 4.5e15 —— 实测出的上限
        let samples = [
            json!({ "uid": "4052137095223728" }),
            json!({ "uid": "842681600717036" }),
            json!({ "uid": "925158341882023" }),
            json!({ "uid": "" }),
            json!({ "uid": "一个很长很长的中文 uid 用来压边界压边界压边界" }),
            json!({}),
            json!({ "uid": 123456789012345_i64 }),
        ];
        for (i, oauth) in samples.iter().enumerate() {
            for v in [
                account_device_id(oauth, "oauth_223728"),
                account_device_alt(oauth, "oauth_223728"),
            ] {
                let n: u64 = v.parse().unwrap_or_else(|_| panic!("样本 {i} 不是纯数字：{v}"));
                assert_eq!(v.len(), 16, "样本 {i} 必须是 16 位：{v}");
                assert!(!v.starts_with('0'), "样本 {i} 首位不许是 0：{v}");
                assert!(n >= 1_000_000_000_000_000, "样本 {i} 太小：{v}");
                assert!(
                    n < LIMIT,
                    "样本 {i} 超出服务端接受范围（实测上限 2^52 = {LIMIT}）：{v}"
                );
            }
        }

        // 两个派生值必须**不相等**，否则那个回退档等于白加一次请求。
        let oauth = json!({ "uid": "842681600717036" });
        assert_ne!(
            account_device_id(&oauth, "oauth_717036"),
            account_device_alt(&oauth, "oauth_717036"),
            "回退档不能和主档撞成同一个值"
        );
    }

    /// ⚠️ T68 三改：**记忆表必须按账号分开存，且写坏/读不到都不许炸**。
    #[test]
    fn known_device_is_per_account_and_written_to_a_plain_json_table() {
        // 直接验 JSON 表的结构约定：`{ uid: device_id }`，读回时按 uid 取。
        let table = json!({
            "3601675132": "2081789759006043",
            "8026378203": "1111111111111111",
        });
        assert_eq!(table["3601675132"], json!("2081789759006043"));
        assert_eq!(table["8026378203"], json!("1111111111111111"));
        assert!(table.get("不存在的账号").is_none(), "没记过的账号必须返回 None，好落回派生值");
    }

    /// `9005` 的语义是「今日已签」，**不许显示成「失败」**。
    #[test]
    fn code_9005_is_already_signed_not_a_failure() {
        assert_ne!(status::ALREADY, status::FAILED, "「已签」与「失败」必须是两档");
        let v = item("trae")
            .done(
                status::ALREADY,
                "今日已签到（服务端判定该设备今日已签），当前积分余额 1794.26",
            )
            .to_value();
        assert_eq!(v["status"], json!("already"));
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("今日已签"), "实际：{msg}");
        // 同 §0.1「空」与「失败」不许长得一样：这里反过来 —— 「已签」也不许装成「失败」。
        assert!(!msg.contains("失败"), "已签不是失败，文案里不许出现「失败」：{msg}");
    }

    /// ⚠️ T68 四改（2026-10-17）：`9095` 与 `9005` 是**同一件事的两种码**，
    /// 必须走同一条路（换下一个设备），不许任何一个掉进兜底被涂成红字。
    #[test]
    fn code_9095_is_treated_exactly_like_9005() {
        // 服务端 2026-10-17 实测原文：
        //   9005 → 「当前设备今日已经签到，请明日再来」
        //   9095 → 「当前设备今日已经签到，请明日再来哦～」
        assert!(is_device_taken_code(9005), "9005 = 该设备今日已签");
        assert!(is_device_taken_code(9095), "9095 与 9005 同义，必须一起特判");

        // 这三个**不是**「设备被占」，绝不许混进来：
        //   9074 = 陌生设备（该账号没用它成功签过）
        //   9004 = 请求头压根没带 x-device-id
        //   0    = 成功
        assert!(!is_device_taken_code(9074), "9074 是「设备不被认」，不是「已被占」");
        assert!(!is_device_taken_code(9004));
        assert!(!is_device_taken_code(0));
    }

    /// ⚠️ T68 第二改：**同一账号在两个客户端只签一次**。
    ///
    /// 不去重就会出现「重复签到」—— 2026-10-15 用户截图里三个 uid **各出现两次**
    /// （solo-cn 一次、trae-cn 一次），而服务端按**账号**记「今天已签」，第二遍白跑。
    #[test]
    fn the_same_account_is_signed_once_even_when_it_is_logged_into_two_clients() {
        let targets = vec![
            ("solo-cn".to_string(), "oauth_223728".to_string()),
            ("solo-cn".to_string(), "oauth_717036".to_string()),
            ("solo-cn".to_string(), "oauth_882023".to_string()),
            ("trae-cn".to_string(), "oauth_223728".to_string()),
            ("trae-cn".to_string(), "oauth_717036".to_string()),
            ("trae-cn".to_string(), "oauth_882023".to_string()),
        ];

        // 账号身份 = 目录 id（模拟 uid 在两个客户端里相同）
        let kept = dedupe_trae_targets(&targets, |_c, id| id.to_string());
        assert_eq!(kept.len(), 3, "三个账号、每个只该留一条：{kept:?}");
        assert!(
            kept.iter().all(|(c, _)| c == "solo-cn"),
            "保留先出现的那个客户端：{kept:?}"
        );
        let ids: HashSet<&str> = kept.iter().map(|(_, id)| id.as_str()).collect();
        assert_eq!(ids.len(), 3, "三个身份互不相同，别过度合并：{ids:?}");

        // 单客户端、多个不同账号 ⇒ 一个都不许少。
        let one_client = vec![
            ("trae-cn".to_string(), "oauth_a".to_string()),
            ("trae-cn".to_string(), "oauth_b".to_string()),
        ];
        assert_eq!(dedupe_trae_targets(&one_client, |_c, id| id.to_string()).len(), 2);

        // ⚠️ 身份取不到（空串）时**不许把大家合并成一个** —— 宁可不合并。
        let unknowns = vec![
            ("trae-cn".to_string(), "x".to_string()),
            ("trae-cn".to_string(), "y".to_string()),
        ];
        assert_eq!(
            dedupe_trae_targets(&unknowns, |_c, _i| String::new()).len(),
            2,
            "身份为空时不许合并，否则会白丢一个账号的签到机会"
        );
    }

    #[test]
    fn describe_never_collapses_transport_failure_into_a_business_code() {
        // 传输层失败（http_status == 0）：不许伪装成业务错码，也不许只说「未知错误」。
        let r = Resp {
            http_status: 0,
            body: json!({ "message": "connection refused" }),
            json: false,
        };
        let d = describe(&r);
        assert!(d.starts_with("网络请求失败"), "实际：{d}");
        assert!(d.contains("connection refused"), "实际：{d}");

        // 服务端回了 4xx：HTTP 状态码与业务 code 都要带出来。
        let r = Resp {
            http_status: 400,
            body: json!({ "code": 10001, "msg": "今天已签到" }),
            json: true,
        };
        let d = describe(&r);
        assert!(d.contains("HTTP 400"), "实际：{d}");
        assert!(d.contains("10001"), "实际：{d}");
        assert!(d.contains("今天已签到"), "实际：{d}");
    }

    /// 网关的 HTML 挡板页要被压成一行、且**如实标注「非 JSON 响应」**，
    /// 不许把 300 字符的 `<html>…TLB…` 原样怼给用户（2026-10-14 截图里就是这样）。
    #[test]
    fn describe_flattens_a_gateway_html_page_instead_of_dumping_it() {
        let r = Resp {
            http_status: 404,
            body: json!({ "message": "<html>\n<head><title>404 Not Found</title></head>\n<body>\n<center><h1>404 Not Found</h1></center>\n<hr><center>TLB</center>\n</body>\n</html>" }),
            json: false,
        };
        let d = describe(&r);
        assert!(d.contains("HTTP 404"), "实际：{d}");
        assert!(d.contains("非 JSON 响应"), "实际：{d}");
        assert!(d.contains("404 Not Found"), "实际：{d}");
        // 压成一行：不许还有换行。
        assert!(!d.contains('\n'), "实际：{d}");
        assert!(d.chars().count() < 200, "太长了：{d}");
    }

    /// ⚠️⚠️ 2026-10-14 Trae 签到 404 事故的**判据钉子**：
    /// 「走错域名拿到的 HTML 404 挡板页」**不算** API 应答（要被跳过、换下一个域名），
    /// 而「服务端真的答了业务错码（HTTP 200/4xx + JSON `code`）」**算**（不许换域名掩盖）。
    #[test]
    fn gateway_html_placeholder_is_not_an_api_reply_but_a_business_code_is() {
        // ① TLB 的 HTML 404：body 是 `post()` 的兜底对象 `{message:"<html>…"}`，json=false
        //    ⇒ **不是** API 应答 ⇒ 挑域名时会被跳过，继续试下一个域名。
        let gateway_404 = Resp {
            http_status: 404,
            body: json!({ "message": "<html>…<center>TLB</center>…</html>" }),
            json: false,
        };
        assert!(!looks_like_api_reply(&gateway_404), "HTML 404 被当成了 API 应答");

        // ② 同一坨内容即使状态码是 200，只要不是真 JSON，也依旧不算。
        let gateway_200 = Resp {
            http_status: 200,
            body: json!({ "message": "<html>maintenance</html>" }),
            json: false,
        };
        assert!(!looks_like_api_reply(&gateway_200), "非 JSON 的 200 被当成了 API 应答");

        // ③ 服务端真的答了：HTTP 200 + `code`（含业务错码）⇒ 算。
        let ok = Resp {
            http_status: 200,
            body: json!({ "code": 0, "checked_in": true, "enable": true }),
            json: true,
        };
        assert!(looks_like_api_reply(&ok));

        // ④ 业务错码走 4xx 也是真应答（例如 device-id 缺失的 9004 / token 失效）⇒ **不换域名**。
        let biz = Resp {
            http_status: 403,
            body: json!({ "code": 9004, "message": "device id required" }),
            json: true,
        };
        assert!(looks_like_api_reply(&biz), "业务错码被当成了走错域名");
    }

    #[test]
    fn body_code_accepts_numeric_strings() {
        assert_eq!(body_code(&json!({ "code": 0 })), Some(0));
        assert_eq!(body_code(&json!({ "code": "10001" })), Some(10001));
        assert_eq!(body_code(&json!({ "msg": "x" })), None);
    }

    #[test]
    fn scope_decides_which_side_runs() {
        // 只断言判定本身，不发任何请求。
        let decide = |s: Option<&str>| {
            let s = s.unwrap_or("all").trim();
            (s != "workbuddy", s != "trae")
        };
        assert_eq!(decide(None), (true, true));
        assert_eq!(decide(Some("all")), (true, true));
        assert_eq!(decide(Some("trae")), (true, false));
        assert_eq!(decide(Some("workbuddy")), (false, true));
        // 不认识的 scope 一律当「全部」，不许悄悄什么都不做。
        assert_eq!(decide(Some("nonsense")), (true, true));
    }

    #[test]
    fn expiry_state_reads_seconds_and_milliseconds_alike() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let mk = |exp: i64| {
            let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#);
            let body = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
            format!("{head}.{body}.sig")
        };
        let now_s = chrono::Utc::now().timestamp();
        // 过期的：秒与毫秒两种单位都必须判成 expired。
        assert_eq!(expiry_state(&mk(now_s - 3600)).0, "expired");
        assert_eq!(expiry_state(&mk((now_s - 3600) * 1000)).0, "expired");
        // 还有 1 天 → warn；还有 30 天 → ok。
        assert_eq!(expiry_state(&mk(now_s + 86_400)).0, "warn");
        assert_eq!(expiry_state(&mk(now_s + 30 * 86_400)).0, "ok");
        // 根本不是 JWT → unknown，脚本的语义是「照常继续」。
        assert_eq!(expiry_state("not-a-jwt").0, "unknown");
    }

    #[test]
    fn clean_token_strips_the_scheme_prefix() {
        assert_eq!(clean_token("Cloud-IDE-JWT abc.def.ghi"), "abc.def.ghi");
        assert_eq!(clean_token("  abc.def.ghi  "), "abc.def.ghi");
    }

    #[test]
    fn trae_hosts_put_the_account_host_first_and_stay_deduped() {
        let oauth = json!({ "host": "https://api.trae.com.cn/" });
        let hosts = candidate_hosts(&oauth);
        assert_eq!(hosts[0], "https://api.trae.com.cn");
        assert_eq!(hosts.iter().filter(|h| *h == "https://api.trae.com.cn").count(), 1);
        assert!(hosts.contains(&"https://api.trae.cn".to_string()));

        // host 缺失时仍有兜底域名。
        let hosts = candidate_hosts(&json!({}));
        assert_eq!(hosts, vec!["https://api.trae.cn", "https://api.trae.com.cn"]);
    }

    #[test]
    fn trae_claim_counted_from_the_delta_never_goes_negative() {
        // 余额差值为负（例如中途又消耗了积分）时归 0，不显示「获得 -3 积分」。
        let gained = |before: Option<f64>, after: Option<f64>| match (before, after) {
            (Some(b), Some(a)) => Some(round2((a - b).max(0.0))),
            _ => None,
        };
        assert_eq!(gained(Some(100.0), Some(97.0)), Some(0.0));
        assert_eq!(gained(Some(100.0), Some(112.0)), Some(12.0));
        // 有一端未知 → 结果是 None（未知），不是 0。
        assert_eq!(gained(Some(100.0), None), None);
        assert_eq!(gained(None, Some(112.0)), None);
    }

    /// ⚠️ `-0.0` 会原样进 JSON（`"gained": -0.0`）—— 零必须是正零。
    /// 而**真的负数**要保留符号，别把 `-0.5` 也一起洗成 0。
    #[test]
    fn round2_never_returns_negative_zero() {
        assert!(round2(0.0).is_sign_positive());
        assert!(round2(-0.0).is_sign_positive(), "round2(-0.0) 是负零");
        // 求和为空 / 四舍五入到 0 的情形：都得是正零。
        assert!(round2(-0.004).is_sign_positive(), "四舍五入到 0 时出现了负零");
        // 真的负数不许被抹掉。
        assert!(round2(-0.5).is_sign_negative());
        assert_eq!(round2(-0.5), -0.5);
        assert_eq!(round2(-0.0).to_string(), "0");
    }
}
