//! WorkBuddy 积分 / 积分包查询（**国内版**）。
//!
//! 从参考实现 `credits.rs` 只提取国内版链路：
//!
//! - 主链路：`summary` / `paid-packages` / `free-packages` 三路并行查（国内版只有一个
//!   路径候选，无需 404 回落）；
//! - 回退链路：任一路 404 时退到旧的 `POST /v2/billing/meter/get-user-resource`；
//! - 鉴权：先按「惰性刷新」保证 token 新鲜，遇到 401/403 再刷新一次并重试**该路**，
//!   一次查询最多刷新一次（三路同时刷新会用旧 refresh token 互相覆盖）；
//! - 归一化：把五花八门的容量字段收敛成 `total` / `remaining` / `used` / `expireAt`，
//!   并算出「总额度、总剩余、近期到期、已过期」。
//!
//! ## 账号来源
//!
//! 本工具账号库（`~/.twin-switch/workbuddy-accounts.json`）为主，另**只读**借用参考
//! 工具留下的 `~/.wb-switch/accounts.json`。
//!
//! **从参考工具读到的东西一个字节都不写**：刷新出来的新 token 只留在内存里供本次查询
//! 使用，绝不回写别人的文件。只有来自本工具账号库的账号，刷新成功后才会落盘。
//!
//! ## 加密信封
//!
//! WorkBuddy 5.6 起，「导入本机登录态」拿到的凭据可能是 `{"$wbEncrypted":…}` 信封对象，
//! 解不出明文 → **任何接口都只会换回 401**。这类账号在入口直接短路成可读提示，
//! 不发空 `Bearer` 出去（参考实现 issue #94 的教训）。

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::modules::config::{self, http_request, now_ms};
use crate::modules::workbuddy_auth::{self, API_ENDPOINT};
use crate::modules::workbuddy_vault;

/// 报表接口前缀（国内版与官方客户端一致）。
const CHECKIN_API_PREFIX: &str = "/v2/billing/meter";
/// 旧资源接口后缀。
const USER_RESOURCE_SUFFIX: &str = "/get-user-resource";
const RESOURCE_SUMMARY_PATH: &str = "/billing/meter/get-user-resource-summary";
const RESOURCE_PAID_PACKAGES_PATH: &str = "/billing/meter/get-user-resource-paid-packages";
const RESOURCE_FREE_PACKAGES_PATH: &str = "/billing/meter/get-user-resource-free-packages";
const WORKBUDDY_WEB_ENDPOINT: &str = "https://www.workbuddy.cn";
const PRODUCT_CODE: &str = "p_tcaca";

/// 到期在 7 天内算「近期到期」。
const EXPIRING_SOON_DAYS: i64 = 7;
/// `DeductionEndTime` 比 `CycleEndTime` 晚超过该天数时，视前者为长期占位。
const EXPIRY_CYCLE_OVERRIDE_DAYS: i64 = 365;
/// 距 now 超过该天数的到期时间视为长期有效（置 `null`）。
const FAR_FUTURE_EXPIRY_DAYS: i64 = 730;

/// 查询结果缓存有效期。
const CACHE_TTL_MS: i64 = 5 * 60 * 1000;
/// 惰性刷新窗口：剩余不足该小时数就提前刷新。
const LAZY_REFRESH_HOURS: i64 = 24;

/// 付费 / 免费包查询码表（国内版公开套餐配置）。
///
/// 多带不存在的码对请求无副作用，解析器也不依赖这份清单（summary 仍可带回未列出的包）。
const PAID_PACKAGE_CODES: &[&str] = &[
    "TCACA_code_002_AkiJS3ZHF5",
    "TCACA_code_023_4xbGhMrE6q",
    "TCACA_code_026_BaESVICNoi",
    "TCACA_code_027_0FCGVA6vSa",
    "TCACA_code_009_0XmEQc2xOf",
    "TCACA_code_038_OhvqZtiPKr",
    "TCACA_code_003_FAnt7lcmRT",
    "TCACA_code_036_lupO5WgNdG",
];
const FREE_PACKAGE_CODES: &[&str] = &[
    "TCACA_code_008_cfWoLwvjU4",
    "TCACA_code_007_nzdH5h4Nl0",
    "TCACA_code_028_NtpWi0jzXs",
    "TCACA_code_029_6wCGEWquYy",
    "TCACA_code_030_BjSt89qTvr",
    "TCACA_code_001_PqouKr6QWV",
    "TCACA_code_006_DbXS0lrypC",
    "TCACA_code_035_ArVxJcGDsm",
    "TCACA_code_037_WxOD3MpI2o",
    "TCACA_code_039_KRcQj7wUat",
    "TCACA_code_040_mi9rCYg46x",
];

// ---------------------------------------------------------------------------
// 账号来源合并
// ---------------------------------------------------------------------------

/// 凭据形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenState {
    /// 明文 token，可直接调接口。
    Plain,
    /// WorkBuddy 加密信封，解不出来，任何接口都会 401。
    Envelope,
    /// 没有凭据（只导入过登录态但没采到 token）。
    Missing,
}

impl TokenState {
    fn as_str(self) -> &'static str {
        match self {
            TokenState::Plain => "plain",
            TokenState::Envelope => "envelope",
            TokenState::Missing => "missing",
        }
    }
}

/// 账号从哪来（决定刷新出来的 token 能不能落盘）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// 本工具账号库 —— 可写。
    Own,
    /// 参考工具的账号库 —— **只读**，刷新结果只留在内存。
    Ref,
}

/// 一个可查询积分的账号（含凭据，只在内存中流转，不进前端）。
#[derive(Debug, Clone)]
pub struct CreditAccount {
    /// 稳定键：本工具账号库用自己的 `id`，参考工具库用 `ref:<uid>`。
    pub id: String,
    pub uid: String,
    /// 展示名（解析不出来时是 `uid …尾号`）。
    pub name: String,
    pub origin: Origin,
    pub domain: String,
    pub token_state: TokenState,
    /// 待发请求的账号对象（含 `access_token` / `refresh_token` / `uid` / `domain`）。
    pub raw: Value,
}

impl CreditAccount {
    /// 能不能发请求拿积分。
    pub fn queryable(&self) -> bool {
        self.token_state == TokenState::Plain
    }

    /// 不可查询时的可读原因。
    pub fn blocked_reason(&self) -> Option<String> {
        match self.token_state {
            TokenState::Plain => None,
            TokenState::Envelope => Some(
                "该账号凭据是 WorkBuddy 加密信封，无法直接调用积分接口；\
                 请用「发起网页登录」重新扫码添加以获得明文凭据"
                    .to_string(),
            ),
            TokenState::Missing => Some("该账号没有采集到凭据，无法查询积分".to_string()),
        }
    }

    /// 脱敏后的展示信息（不含任何凭据字段）。
    fn public_info(&self) -> Value {
        json!({
            "id": self.id,
            "uid": self.uid,
            "name": self.name,
            "origin": if self.origin == Origin::Own { "own" } else { "ref" },
            "tokenState": self.token_state.as_str(),
            "queryable": self.queryable(),
            "blockedReason": self.blocked_reason(),
        })
    }
}

/// 参考工具账号库路径（`~/.wb-switch/accounts.json`）。**只读**。
fn reference_accounts_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".wb-switch")
        .join("accounts.json")
}

fn str_at(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// 字段是否是加密信封对象。*不*复制 `workbuddy_auth` 的实现，避免两处判定漂移——
/// 这里只判断「不是字符串」即可（信封一定是对象）。
fn is_envelope(v: &Value, key: &str) -> bool {
    matches!(v.get(key), Some(Value::Object(_)))
}

fn token_state_of(acc: &Value) -> TokenState {
    if is_envelope(acc, "access_token") {
        return TokenState::Envelope;
    }
    if str_at(acc, "access_token").is_empty() {
        TokenState::Missing
    } else {
        TokenState::Plain
    }
}

/// 合并账号来源，按 uid 去重。
///
/// 去重规则（顺序即优先级）：
/// 1. **明文凭据优先**——同一个 uid 在参考工具库里是明文、在本工具库里是信封时，
///    取明文的那个（信封根本查不了积分）；
/// 2. 都是明文时**本工具账号库优先**（能落盘刷新结果）。
fn collect_accounts() -> Vec<CreditAccount> {
    let mut by_uid: HashMap<String, CreditAccount> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    let mut consider = |acc: CreditAccount| {
        match by_uid.get(&acc.uid) {
            None => {
                order.push(acc.uid.clone());
                by_uid.insert(acc.uid.clone(), acc);
            }
            Some(old) => {
                let better = match (old.queryable(), acc.queryable()) {
                    (false, true) => true,
                    (true, false) => false,
                    // 同档位：本工具账号库优先
                    _ => old.origin == Origin::Ref && acc.origin == Origin::Own,
                };
                if better {
                    by_uid.insert(acc.uid.clone(), acc);
                }
            }
        }
    };

    // 先放本工具账号库（后放的参考工具库在「凭据更优」时会替换掉它）
    for a in workbuddy_vault::load_accounts() {
        let uid = str_at(&a, "uid");
        if uid.is_empty() {
            continue;
        }
        consider(CreditAccount {
            id: str_at(&a, "id"),
            name: workbuddy_vault::display_name(&a),
            uid,
            origin: Origin::Own,
            domain: str_at(&a, "domain"),
            token_state: token_state_of(&a),
            raw: a,
        });
    }

    for a in reference_accounts() {
        let uid = str_at(&a, "uid");
        if uid.is_empty() {
            continue;
        }
        let name = {
            let n = str_at(&a, "nickname");
            if n.is_empty() {
                crate::modules::workbuddy_accounts::label_for(&uid)
            } else {
                n
            }
        };
        consider(CreditAccount {
            id: format!("ref:{uid}"),
            name,
            uid,
            origin: Origin::Ref,
            domain: str_at(&a, "domain"),
            token_state: token_state_of(&a),
            raw: a,
        });
    }

    order
        .into_iter()
        .filter_map(|uid| by_uid.remove(&uid))
        .collect()
}

/// 读参考工具账号库（只读；缺失或损坏返回空）。
fn reference_accounts() -> Vec<Value> {
    let path = reference_accounts_path();
    let Some(text) = std::fs::read_to_string(&path).ok() else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(a)) => a,
        Ok(Value::Object(o)) => o
            .get("accounts")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 纯解析工具（与参考实现逐条对齐）
// ---------------------------------------------------------------------------

fn first_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value.get(*key))
}

fn parse_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn first_number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| parse_number(value.get(*key)))
}

/// 时间戳：秒 / 毫秒 / RFC3339 / `%Y-%m-%d %H:%M:%S` / `%Y-%m-%d`（当天 23:59:59）。
fn parse_timestamp_ms(value: Option<&Value>) -> Option<i64> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let value = value?;
    if let Some(number) = parse_number(Some(value)) {
        let millis = if number.abs() < 10_000_000_000.0 {
            number * 1000.0
        } else {
            number
        };
        return Some(millis.round() as i64);
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(parsed.timestamp_millis());
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(text, fmt) {
            if let Some(v) = Local.from_local_datetime(&parsed).single() {
                return Some(v.timestamp_millis());
            }
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(23, 59, 59))
        .and_then(|date| Local.from_local_datetime(&date).single())
        .map(|date| date.timestamp_millis())
}

fn value_at_path<'a>(mut current: &'a Value, path: &[&str]) -> Option<&'a Value> {
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

const ACCOUNT_PATHS: &[&[&str]] = &[
    &["data", "Accounts"],
    &["data", "data", "Accounts"],
    &["data", "Response", "Data", "Accounts"],
    &["data", "data", "Response", "Data", "Accounts"],
    &["data", "accounts"],
    &["data", "data", "accounts"],
];

const PACKAGE_PATHS: &[&[&str]] = &[
    &["data", "Packages"],
    &["data", "data", "Packages"],
    &["data", "Response", "Data", "Packages"],
    &["data", "data", "Response", "Data", "Packages"],
    &["data", "packages"],
    &["data", "data", "packages"],
];

fn list_at_paths<'a>(response: &'a Value, paths: &[&[&str]]) -> Option<Vec<&'a Value>> {
    paths
        .iter()
        .find_map(|path| value_at_path(response, path).and_then(Value::as_array))
        .map(|items| items.iter().collect())
}

fn resource_accounts(response: &Value) -> Vec<&Value> {
    list_at_paths(response, ACCOUNT_PATHS).unwrap_or_default()
}

fn resource_packages(response: &Value) -> Vec<&Value> {
    list_at_paths(response, PACKAGE_PATHS).unwrap_or_default()
}

fn has_resource_accounts(response: &Value) -> bool {
    list_at_paths(response, ACCOUNT_PATHS).is_some()
}

fn has_resource_packages(response: &Value) -> bool {
    list_at_paths(response, PACKAGE_PATHS).is_some()
}

/// 到期时间：优先 `DeductionEndTime`；仅当 `CycleEndTime` 比它早超过
/// `EXPIRY_CYCLE_OVERRIDE_DAYS` 时改用周期结束时间（视前者为长期占位）；
/// 最终值距 now 超过 `FAR_FUTURE_EXPIRY_DAYS` 视为长期有效（返回 `None`）。
fn resolve_expire_at(raw: &Value, now: i64) -> Option<i64> {
    let deduction_end = parse_timestamp_ms(first_value(
        raw,
        &[
            "DeductionEndTime",
            "deductionEndTime",
            "ExpiredTime",
            "expiredTime",
        ],
    ));
    let cycle_end = parse_timestamp_ms(first_value(raw, &["CycleEndTime", "cycleEndTime"]));
    let override_ms = EXPIRY_CYCLE_OVERRIDE_DAYS * 24 * 3600 * 1000;
    let expire_at = match (deduction_end, cycle_end) {
        (Some(deduction), Some(cycle)) if deduction.saturating_sub(cycle) > override_ms => {
            Some(cycle)
        }
        (Some(deduction), _) => Some(deduction),
        (None, cycle) => cycle,
    };
    let far_future_ms = FAR_FUTURE_EXPIRY_DAYS * 24 * 3600 * 1000;
    expire_at.filter(|value| value.saturating_sub(now) <= far_future_ms)
}

const TOTAL_KEYS: [&str; 7] = [
    "CycleCapacitySizePrecise",
    "CycleCapacitySize",
    "CycleTotalCapacity",
    "CapacitySizePrecise",
    "CapacitySize",
    "SlicePeriodCapacitySizePrecise",
    "SlicePeriodCapacitySize",
];
const REMAIN_KEYS: [&str; 7] = [
    "CycleCapacityRemainPrecise",
    "CycleCapacityRemain",
    "CycleRemainCapacity",
    "CapacityRemainPrecise",
    "CapacityRemain",
    "SlicePeriodCapacityRemainPrecise",
    "SlicePeriodCapacityRemain",
];
const USED_KEYS: [&str; 7] = [
    "CycleCapacityUsedPrecise",
    "CycleCapacityUsed",
    "CycleUsedCapacity",
    "CapacityUsedPrecise",
    "CapacityUsed",
    "SlicePeriodCapacityUsedPrecise",
    "SlicePeriodCapacityUsed",
];

/// 把一条积分包记录归一化成统一字段。
fn resource_summary(raw: &Value, now: i64) -> Value {
    let slice = first_value(raw, &["SlicePeriodUsageDetails", "slicePeriodUsageDetails"])
        .and_then(Value::as_array)
        .and_then(|items| items.first());
    let raw_total = first_number(raw, &TOTAL_KEYS)
        .or_else(|| slice.and_then(|value| first_number(value, &TOTAL_KEYS)));
    let raw_remaining = first_number(raw, &REMAIN_KEYS)
        .or_else(|| slice.and_then(|value| first_number(value, &REMAIN_KEYS)));
    let raw_used = first_number(raw, &USED_KEYS)
        .or_else(|| slice.and_then(|value| first_number(value, &USED_KEYS)));
    let total = raw_total
        .or_else(|| {
            raw_remaining
                .zip(raw_used)
                .map(|(remaining, used)| remaining + used)
        })
        .or(raw_remaining)
        .or(raw_used)
        .unwrap_or(0.0)
        .max(0.0);
    let remaining = raw_remaining
        .unwrap_or_else(|| (total - raw_used.unwrap_or(0.0)).max(0.0))
        .max(0.0);
    let used = raw_used
        .unwrap_or_else(|| (total - remaining).max(0.0))
        .max(0.0);
    let expire_at = resolve_expire_at(raw, now);
    let expired = expire_at.map(|value| value <= now).unwrap_or(false);
    let expiring_soon = expire_at
        .map(|value| value > now && value - now <= EXPIRING_SOON_DAYS * 24 * 3600 * 1000)
        .unwrap_or(false);
    let status = first_value(raw, &["Status", "status"])
        .and_then(|value| parse_number(Some(value)))
        .map(|value| value as i64);

    json!({
        "packageCode": first_value(raw, &["PackageCode", "packageCode"]),
        "packageName": first_value(raw, &["PackageName", "packageName"]),
        "total": total,
        "remaining": remaining,
        "used": used,
        "status": status,
        "expireAt": expire_at,
        "expired": expired,
        "expiringSoon": expiring_soon,
    })
}

fn response_code(response: &Value) -> Option<i64> {
    fn parse_code(value: &Value) -> Option<i64> {
        value.as_i64().or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<i64>().ok())
        })
    }
    response
        .get("code")
        .and_then(parse_code)
        .or_else(|| response.get("data")?.get("code").and_then(parse_code))
}

fn response_error(response: &Value) -> String {
    let nested = response.get("data").filter(|value| value.is_object());
    let code = response_code(response).unwrap_or(-1);
    response
        .get("message")
        .or_else(|| response.get("msg"))
        .or_else(|| nested.and_then(|value| value.get("message")))
        .or_else(|| nested.and_then(|value| value.get("msg")))
        .and_then(|value| value.as_str())
        .filter(|message| !message.trim().is_empty())
        .map(|message| message.chars().take(160).collect::<String>())
        .unwrap_or_else(|| format!("积分查询失败（code={code}）"))
}

fn is_success(response: &Value) -> bool {
    if !response.is_object() {
        return false;
    }
    match response_code(response) {
        Some(0) | Some(200) => true,
        Some(_) => false,
        None => {
            response.get("data").is_some()
                && response.get("ok").and_then(Value::as_bool) != Some(false)
                && response.get("success").and_then(Value::as_bool) != Some(false)
        }
    }
}

fn is_unauthorized(response: &Value) -> bool {
    let code = response_code(response).unwrap_or(-1);
    // 网关 WAF 10085 是客户端指纹拦截，不是 token 过期；刷新无效。
    if code == 10085 {
        return false;
    }
    if code == 401 || code == 403 {
        return true;
    }
    let message = response
        .get("message")
        .or_else(|| response.get("msg"))
        .or_else(|| response.get("data").and_then(|value| value.get("message")))
        .or_else(|| response.get("data").and_then(|value| value.get("msg")))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_lowercase();
    ["unauthorized", "401", "登录", "失效", "过期", "token"]
        .iter()
        .any(|keyword| message.contains(keyword))
}

fn is_route_missing(response: &Value) -> bool {
    response_code(response) == Some(404)
}

fn is_transport_error(response: &Value) -> bool {
    response_code(response) == Some(-1)
        && response
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.trim().is_empty())
}

// ---------------------------------------------------------------------------
// 鉴权 / 刷新
// ---------------------------------------------------------------------------

/// 国内版只认这两个官方 origin；账号数据不能拼出任意主机。
fn api_base_for(account: &Value) -> &'static str {
    match account
        .get("domain")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("workbuddy.cn") | Some("www.workbuddy.cn") => WORKBUDDY_WEB_ENDPOINT,
        _ => API_ENDPOINT,
    }
}

fn build_auth_headers(account: &Value) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", str_at(account, "access_token")),
    );
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if let Some(uid) = workbuddy_auth::get_str(account, "uid") {
        headers.insert("X-User-Id".to_string(), uid);
    }
    if let Some(domain) = workbuddy_auth::get_str(account, "domain") {
        headers.insert("X-Domain".to_string(), domain);
    }
    // 用户中心的 axios 拦截器始终携带该头，桌面端复用同一组 billing 接口时保持一致。
    headers.insert("X-Client-Platform".to_string(), "web".to_string());
    headers.insert(
        "Accept".to_string(),
        "application/json, text/plain, */*".to_string(),
    );
    headers
}

/// 资源接口请求头：在鉴权头基础上补 `Origin` / `Referer`（跟随本次请求 host）。
fn resource_auth_headers(account: &Value, base: &str) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("Origin".to_string(), base.to_string());
    headers.insert("Referer".to_string(), format!("{base}/profile/plans-usage"));
    headers
}

/// 刷新接口 URL（国内版）。
fn refresh_url(account: &Value) -> String {
    format!("{}{CHECKIN_API_PREFIX}/auth/token/refresh", api_base_for(account))
}

/// 刷新请求头。
///
/// `X-Auth-Refresh-Source: plugin` 必须保留：官方客户端刷新时发送该头，
/// 缺失时网关会把这次刷新判定成另一个 client 来源并返回 invalid_grant。
fn refresh_headers(account: &Value, refresh_token: &str) -> HashMap<String, String> {
    let mut headers = build_auth_headers(account);
    headers.insert("X-Refresh-Token".to_string(), refresh_token.to_string());
    headers.insert("X-Auth-Refresh-Source".to_string(), "plugin".to_string());
    headers
}

/// 刷新一次 token。
///
/// 返回 `(刷新后的账号, 是否刷新成功)`。失败时保留原账号，由上层决定怎么报错。
async fn refresh_account_token(mut account: Value) -> (Value, bool) {
    let rt = str_at(&account, "refresh_token");
    if rt.is_empty() {
        return (account, false);
    }
    let url = refresh_url(&account);
    let resp = http_request(&url, "POST", Some(json!({})), Some(&refresh_headers(&account, &rt))).await;
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 && code != 200 {
        return (account, false);
    }
    let data = resp.get("data").cloned().unwrap_or_else(|| json!({}));
    let Some(new_at) = data
        .get("accessToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("access_token").and_then(|v| v.as_str()))
        .map(str::to_string)
    else {
        return (account, false);
    };
    account["access_token"] = json!(new_at);
    if let Some(new_rt) = data
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("refresh_token").and_then(|v| v.as_str()))
    {
        account["refresh_token"] = json!(new_rt);
    }
    // 官方只返回相对 expiresIn（秒），换算成绝对时间戳。
    let new_exp = workbuddy_auth::norm_ts(data.get("expiresAt").or_else(|| data.get("expires_at")))
        .or_else(|| {
            data.get("expiresIn")
                .and_then(|v| v.as_i64())
                .map(|e| now_ms() + e * 1000)
        });
    if let Some(v) = new_exp {
        account["expiresAt"] = json!(v);
    }
    let mut new_rt_exp =
        workbuddy_auth::norm_ts(data.get("refreshExpiresAt").or_else(|| data.get("refresh_expires_at")));
    if new_rt_exp.is_none() {
        new_rt_exp = workbuddy_auth::norm_ts(
            account
                .get("auth_raw")
                .and_then(|a| a.get("refreshExpiresAt")),
        );
    }
    if let Some(v) = new_rt_exp {
        account["refreshExpiresAt"] = json!(v);
    }
    account["refreshedAt"] = json!(now_ms());
    (account, true)
}

/// 惰性刷新：`expiresAt` 缺失或剩余 < `LAZY_REFRESH_HOURS` 就刷新。
async fn ensure_fresh_token(account: Value) -> Value {
    let exp = account.get("expiresAt").and_then(|v| v.as_i64());
    let stale = match exp {
        Some(e) => now_ms() >= e || e - now_ms() < LAZY_REFRESH_HOURS * 3600 * 1000,
        None => true,
    };
    if !stale || str_at(&account, "refresh_token").is_empty() {
        return account;
    }
    refresh_account_token(account).await.0
}

/// 刷新结果落盘——**只对来自本工具账号库的账号做**。
///
/// 参考工具账号库是别人的文件，刷新出来的新 token 只留在内存里供本次查询使用。
fn persist_if_owned(account: &CreditAccount, refreshed: &Value) {
    if account.origin != Origin::Own {
        return;
    }
    let _ = workbuddy_vault::update_tokens(&account.id, refreshed);
}

// ---------------------------------------------------------------------------
// 请求编排
// ---------------------------------------------------------------------------

async fn post_with_account(account: &Value, url: &str, body: Value) -> Value {
    let base = api_base_for(account);
    let headers = resource_auth_headers(account, base);
    let mut response = http_request(url, "POST", Some(body.clone()), Some(&headers)).await;
    if std::env::var("WB_CREDITS_DEBUG").is_ok() {
        eprintln!(
            "[credits] POST {url} -> {}",
            &serde_json::to_string(&response)
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>()
        );
    }
    // 网关在短时间内收到多个请求时会偶发直接断开连接（实测「error sending request」，
    // 隔一会儿重发就正常）。退避重试两次，别让一次抖动变成界面上的「查询失败」。
    for attempt in 1..=2 {
        if !is_transport_error(&response) {
            return response;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400 * attempt)).await;
        response = http_request(url, "POST", Some(body.clone()), Some(&headers)).await;
    }
    response
}

fn paid_packages_body() -> Value {
    json!({
        "PageNumber": 1,
        "PageSize": 200,
        "Status": [0, 3],
        "PackageCodes": PAID_PACKAGE_CODES,
        "NeedRenewInfo": true,
    })
}

fn free_packages_body() -> Value {
    use chrono::Local;
    let now = Local::now();
    let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap_or(now.naive_local());
    let end = now
        .date_naive()
        .and_hms_opt(23, 59, 59)
        .unwrap_or(now.naive_local());
    json!({
        "PageNumber": 1,
        "PageSize": 200,
        "Status": [0, 3],
        "SlicePeriodStartTime": start.format("%Y-%m-%d %H:%M:%S").to_string(),
        "SlicePeriodEndTime": end.format("%Y-%m-%d %H:%M:%S").to_string(),
        "PackageCodes": FREE_PACKAGE_CODES,
    })
}

async fn fetch_summary(account: &Value) -> Value {
    let url = format!("{}{RESOURCE_SUMMARY_PATH}", api_base_for(account));
    post_with_account(account, &url, json!({})).await
}

async fn fetch_paid(account: &Value) -> Value {
    let url = format!("{}{RESOURCE_PAID_PACKAGES_PATH}", api_base_for(account));
    post_with_account(account, &url, paid_packages_body()).await
}

async fn fetch_free(account: &Value) -> Value {
    let url = format!("{}{RESOURCE_FREE_PACKAGES_PATH}", api_base_for(account));
    post_with_account(account, &url, free_packages_body()).await
}

/// 旧接口回退（`/v2/billing/meter/get-user-resource`）。
async fn fetch_legacy(account: &Value) -> Value {
    use chrono::Local;
    let now = Local::now();
    let body = json!({
        "PageNumber": 1,
        "PageSize": 100,
        "ProductCode": PRODUCT_CODE,
        "Status": [0, 3],
        "PackageEndTimeRangeBegin": now.format("%Y-%m-%d %H:%M:%S").to_string(),
        "PackageEndTimeRangeEnd": (now + chrono::Duration::days(365 * 101))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    });
    let url = format!("{}{CHECKIN_API_PREFIX}{USER_RESOURCE_SUFFIX}", api_base_for(account));
    post_with_account(account, &url, body).await
}

/// 三路响应 → 归一化资源列表；全不可用返回 `None`。
fn normalized_new_resources(
    summary: &Value,
    paid: &Value,
    free: &Value,
    now: i64,
) -> Option<Vec<Value>> {
    let summary_ok = is_success(summary) && has_resource_packages(summary);
    let paid_ok = is_success(paid) && has_resource_accounts(paid);
    let free_ok = is_success(free) && has_resource_accounts(free);
    if !(summary_ok || paid_ok || free_ok) {
        return None;
    }
    let summary_resources: Vec<Value> = if summary_ok {
        resource_packages(summary)
            .into_iter()
            .map(|r| resource_summary(r, now))
            .collect()
    } else {
        Vec::new()
    };
    let mut detail_resources = Vec::new();
    if paid_ok {
        detail_resources.extend(
            resource_accounts(paid)
                .into_iter()
                .map(|r| resource_summary(r, now)),
        );
    }
    if free_ok {
        detail_resources.extend(
            resource_accounts(free)
                .into_iter()
                .map(|r| resource_summary(r, now)),
        );
    }
    Some(merge_resources(summary_resources, detail_resources))
}

/// 明细优先：`summary` 里已出现在明细中的包不再重复计入。
fn merge_resources(summary_resources: Vec<Value>, detail_resources: Vec<Value>) -> Vec<Value> {
    let detail_codes: HashSet<String> = detail_resources
        .iter()
        .filter_map(|resource| {
            first_value(resource, &["packageCode", "PackageCode"])
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut resources = detail_resources;
    resources.extend(summary_resources.into_iter().filter(|resource| {
        first_value(resource, &["packageCode", "PackageCode"])
            .and_then(Value::as_str)
            .map(|code| !detail_codes.contains(code))
            .unwrap_or(true)
    }));
    resources
}

/// 资源列表 → 最终结果（含各账号汇总统计）。
fn credit_result(resources: Vec<Value>, now: i64) -> Value {
    let total_remaining: f64 = resources
        .iter()
        .filter_map(|r| r.get("remaining").and_then(Value::as_f64))
        .sum();
    let total_capacity: f64 = resources
        .iter()
        .filter_map(|r| r.get("total").and_then(Value::as_f64))
        .sum();
    let soonest_expire_at = resources
        .iter()
        .filter(|r| r.get("remaining").and_then(Value::as_f64).unwrap_or(0.0) > 0.0)
        .filter_map(|r| r.get("expireAt").and_then(Value::as_i64))
        .min();
    let expiring_soon: Vec<Value> = resources
        .iter()
        .filter(|r| {
            r.get("expiringSoon").and_then(Value::as_bool) == Some(true)
                && r.get("remaining").and_then(Value::as_f64).unwrap_or(0.0) > 0.0
        })
        .cloned()
        .collect();
    let expiring_soon_remaining: f64 = expiring_soon
        .iter()
        .filter_map(|r| r.get("remaining").and_then(Value::as_f64))
        .sum();
    let expired_remaining: f64 = resources
        .iter()
        .filter(|r| r.get("expired").and_then(Value::as_bool) == Some(true))
        .filter_map(|r| r.get("remaining").and_then(Value::as_f64))
        .sum();
    let expired = resources
        .iter()
        .any(|r| r.get("expired").and_then(Value::as_bool) == Some(true));

    // 有效积分包：还有剩余、按到期时间升序（长期有效排最后）。
    // 参考实现的卡片统计用的就是这份列表 —— 已用完的包不该计入「N 个积分包」。
    let mut active: Vec<(usize, Value)> = resources
        .iter()
        .enumerate()
        .filter(|(_, r)| r.get("remaining").and_then(Value::as_f64).unwrap_or(0.0) > 0.0)
        .map(|(i, r)| (i, r.clone()))
        .collect();
    active.sort_by(|(li, l), (ri, r)| {
        let le = l.get("expireAt").and_then(Value::as_i64).unwrap_or(i64::MAX);
        let re = r.get("expireAt").and_then(Value::as_i64).unwrap_or(i64::MAX);
        le.cmp(&re).then(li.cmp(ri))
    });
    let active_resources: Vec<Value> = active.into_iter().map(|(_, r)| r).collect();

    json!({
        "totalCapacity": round2(total_capacity),
        "totalRemaining": round2(total_remaining),
        "expiringSoonRemaining": round2(expiring_soon_remaining),
        "expiredRemaining": round2(expired_remaining),
        "soonestExpireAt": soonest_expire_at,
        "expiringSoon": expiring_soon,
        "expired": expired,
        "packageCount": resources.len(),
        "activePackageCount": active_resources.len(),
        "resources": resources,
        "activeResources": active_resources,
        "updatedAt": now,
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 查询单个账号的积分。
async fn query_account(account: &CreditAccount) -> Value {
    let now = now_ms();
    if let Some(reason) = account.blocked_reason() {
        return json!({
            "ok": false,
            "account": account.public_info(),
            "error": reason,
        });
    }

    // 惰性刷新（返回的账号可能换了新 token）
    let mut working = ensure_fresh_token(account.raw.clone()).await;

    let (summary, paid, free) = tokio::join!(
        fetch_summary(&working),
        fetch_paid(&working),
        fetch_free(&working),
    );

    let mut refresh_attempted = false;
    let mut summary = summary;
    let mut paid = paid;
    let mut free = free;

    if is_unauthorized(&summary) || is_unauthorized(&paid) || is_unauthorized(&free) {
        if !str_at(&working, "refresh_token").is_empty() {
            let (refreshed, ok) = refresh_account_token(working.clone()).await;
            if ok {
                working = refreshed;
                refresh_attempted = true;
                // 只重试失败的那几路，避免三路各自刷新互相覆盖 token。
                if is_unauthorized(&summary) {
                    summary = fetch_summary(&working).await;
                }
                if is_unauthorized(&paid) {
                    paid = fetch_paid(&working).await;
                }
                if is_unauthorized(&free) {
                    free = fetch_free(&working).await;
                }
            }
        }
    }

    if refresh_attempted {
        persist_if_owned(account, &working);
    }

    if let Some(resources) = normalized_new_resources(&summary, &paid, &free, now) {
        let mut out = credit_result(resources, now);
        out["ok"] = json!(true);
        out["account"] = account.public_info();
        out["source"] = json!("summary");
        out["refreshed"] = json!(refresh_attempted);
        return out;
    }

    // 三路全废 → 旧的单接口回退。必须复用已经刷新过的账号，
    // 否则会用旧 refresh token 再刷一次并把刚落盘的新 token 覆盖成失效状态。
    let legacy = fetch_legacy(&working).await;
    if is_success(&legacy) && has_resource_accounts(&legacy) {
        let resources: Vec<Value> = resource_accounts(&legacy)
            .into_iter()
            .map(|r| resource_summary(r, now))
            .collect();
        let mut out = credit_result(resources, now);
        out["ok"] = json!(true);
        out["account"] = account.public_info();
        out["source"] = json!("legacy");
        out["refreshed"] = json!(refresh_attempted);
        return out;
    }

    // 404 说明两代接口都不认识这个账号档位；其余情况报原始错误更利于排查。
    let error = if is_route_missing(&legacy) || is_route_missing(&summary) {
        "积分接口不可用（404）：该域名下没有当前的计费接口，可能账号档位不符".to_string()
    } else {
        response_error(&legacy)
    };
    json!({
        "ok": false,
        "account": account.public_info(),
        "error": error,
    })
}

// ---------------------------------------------------------------------------
// 缓存与对外入口
// ---------------------------------------------------------------------------

/// 内存缓存 TTL：5 分钟内不重复联网（常量在文件头部 `CACHE_TTL_MS`）。
///
/// 磁盘缓存文件名（落在 `config::cache_dir()`）。
///
/// 意义：**跨进程重启**。上一次运行查到的积分随进程退出就没了，
/// 首屏只能等联网；落盘之后下次启动可以先渲染上次的数字，再后台刷新。
const CACHE_NAME: &str = "credits.json";
const CACHE_VERSION: u64 = 1;

#[derive(Clone)]
struct Cache {
    at: i64,
    items: Vec<Value>,
    errors: Vec<Value>,
}

static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<Cache>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 读磁盘缓存（只读文件）。
fn disk_cache() -> Option<Cache> {
    let doc = config::read_cache_json(CACHE_NAME)?;
    if doc.get("version").and_then(Value::as_u64) != Some(CACHE_VERSION) {
        return None;
    }
    let at = doc.get("at").and_then(Value::as_i64)?;
    Some(Cache {
        at,
        items: doc.get("items").and_then(Value::as_array).cloned().unwrap_or_default(),
        errors: doc.get("errors").and_then(Value::as_array).cloned().unwrap_or_default(),
    })
}

/// 写磁盘缓存（失败只打印，绝不影响查询结果返回）。
fn persist_disk(c: &Cache) {
    config::write_cache_json(
        CACHE_NAME,
        &json!({ "version": CACHE_VERSION, "at": c.at, "items": c.items, "errors": c.errors }),
    );
}

/// 取当前缓存：内存优先，内存空则回落到磁盘并顺手灌进内存。
fn current_cache() -> Option<Cache> {
    {
        let guard = cache().lock().unwrap();
        if let Some(c) = guard.as_ref() {
            return Some(c.clone());
        }
    }
    let disk = disk_cache()?;
    let mut guard = cache().lock().unwrap();
    if guard.is_none() {
        *guard = Some(disk.clone());
    }
    Some(disk)
}

/// 汇总结果：`{ok, accounts, updatedAt, cached, summary}`。
fn pack(items: Vec<Value>, errors: Vec<Value>, at: i64, cached: bool) -> Value {
    let ok_items: Vec<&Value> = items.iter().filter(|i| i["ok"] == json!(true)).collect();
    let total_remaining: f64 = ok_items
        .iter()
        .filter_map(|i| i.get("totalRemaining").and_then(Value::as_f64))
        .sum();
    let total_capacity: f64 = ok_items
        .iter()
        .filter_map(|i| i.get("totalCapacity").and_then(Value::as_f64))
        .sum();
    json!({
        "ok": true,
        "accounts": items,
        "errors": errors,
        "updatedAt": at,
        "cached": cached,
        "summary": {
            "queried": items.len() + errors.len(),
            "succeeded": ok_items.len(),
            "failed": errors.len(),
            "totalCapacity": round2(total_capacity),
            "totalRemaining": round2(total_remaining),
        },
    })
}

/// 账号清单（只读，不发请求）。
pub fn accounts() -> Value {
    let list: Vec<Value> = collect_accounts().iter().map(|a| a.public_info()).collect();
    json!({
        "ok": true,
        "count": list.len(),
        "queryable": list.iter().filter(|a| a["queryable"] == json!(true)).count(),
        "accounts": list,
        "referenceStore": reference_accounts_path().to_string_lossy(),
    })
}

/// 只读缓存（**不发请求**）：内存 → 磁盘 → 空。
///
/// 有数据时附带 `stale`（超过 5 分钟）与 `fromDisk`（来自上次运行落盘的结果），
/// 前端据此决定要不要显示「数据可能已过期」。
pub fn cached() -> Value {
    match current_cache() {
        Some(c) => {
            let mut out = pack(c.items.clone(), c.errors.clone(), c.at, true);
            out["stale"] = json!(now_ms() - c.at > CACHE_TTL_MS);
            out
        }
        None => json!({
            "ok": true,
            "accounts": [],
            "errors": [],
            "updatedAt": null,
            "cached": true,
            "empty": true,
            "summary": { "queried": 0, "succeeded": 0, "failed": 0 },
        }),
    }
}

/// 查询全部账号的积分。
///
/// `force = false` 时命中 5 分钟内缓存（含上次运行留下的磁盘缓存）直接返回；
/// `force = true` 强制重新查询。
pub async fn query_all(force: bool) -> Value {
    if !force {
        if let Some(c) = current_cache() {
            if now_ms() - c.at < CACHE_TTL_MS {
                return pack(c.items, c.errors, c.at, true);
            }
        }
    }

    let list = collect_accounts();
    let mut items = Vec::new();
    let mut errors = Vec::new();
    // 串行查询：并发会让多个账号同时刷新 token，且上游对同 IP 有频控。
    for account in &list {
        let result = query_account(account).await;
        if result["ok"] == json!(true) {
            items.push(result);
        } else {
            errors.push(json!({
                "id": account.id,
                "uid": account.uid,
                "name": account.name,
                "error": result.get("error").cloned().unwrap_or(json!("查询失败")),
            }));
        }
    }

    let at = now_ms();
    let fresh = Cache {
        at,
        items: items.clone(),
        errors: errors.clone(),
    };
    {
        let mut guard = cache().lock().unwrap();
        *guard = Some(fresh.clone());
    }
    // 立刻落盘：下次启动首屏可以直接用这份数字，不必等联网。
    persist_disk(&fresh);
    pack(items, errors, at, false)
}

/// 查询单个账号（不走缓存，也不写缓存）。
pub async fn query_one(id: &str) -> Value {
    let list = collect_accounts();
    let Some(account) = list.iter().find(|a| a.id == id) else {
        return json!({ "ok": false, "error": format!("账号不存在: {id}") });
    };
    query_account(account).await
}

/// 清空缓存（切换账号 / 重新登录后调用）。
pub fn invalidate() {
    let mut guard = cache().lock().unwrap();
    *guard = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(total: f64, remain: f64, ded_end: &str, cycle_end: &str) -> Value {
        json!({
            "PackageCode": "P1",
            "PackageName": "平台奖励积分",
            "CycleCapacitySizePrecise": total,
            "CycleCapacityRemainPrecise": remain,
            "DeductionEndTime": ded_end,
            "CycleEndTime": cycle_end,
        })
    }

    #[test]
    fn resource_summary_derives_used_when_missing() {
        let now = 1_791_000_000_000i64;
        let r = resource_summary(&sum(100.0, 30.0, "2026-10-18 18:28:00", "2026-10-10 00:00:00"), now);
        assert_eq!(r["total"], json!(100.0));
        assert_eq!(r["remaining"], json!(30.0));
        assert_eq!(r["used"], json!(70.0));
        assert_eq!(r["expiringSoon"], json!(false)); // 10 天后到期，不算近期
    }

    #[test]
    fn far_future_deduction_uses_cycle_end() {
        let now = 1_791_000_000_000i64;
        // DeductionEndTime 是 2049 占位值，比 CycleEndTime 晚 >365 天 → 改用周期结束
        let r = resource_summary(&sum(100.0, 100.0, "2049-12-31 23:59:59", "2026-10-20 00:00:00"), now);
        let exp = r["expireAt"].as_i64().expect("应解析出到期时间");
        // 2026-10-20 00:00:00 本地时间
        assert!(exp < now + 40 * 24 * 3600 * 1000, "应使用周期结束时间而非 2049");
    }

    #[test]
    fn far_future_expiry_becomes_null() {
        let now = 1_791_000_000_000i64;
        // 两个时间都在遥遥无期 → 视为长期有效
        let r = resource_summary(&sum(100.0, 100.0, "2049-12-31 23:59:59", "2049-01-01 00:00:00"), now);
        assert_eq!(r["expireAt"], Value::Null);
        assert_eq!(r["expiringSoon"], json!(false));
    }

    #[test]
    fn expiring_soon_is_flagged_within_seven_days() {
        // now 取 2026-10-10 本地午夜附近
        let now = parse_timestamp_ms(Some(&json!("2026-10-10 00:00:00"))).unwrap();
        let r = resource_summary(&sum(100.0, 50.0, "2026-10-14 09:24:00", ""), now);
        assert_eq!(r["expiringSoon"], json!(true));
        let r2 = resource_summary(&sum(100.0, 50.0, "2026-11-14 09:24:00", ""), now);
        assert_eq!(r2["expiringSoon"], json!(false));
    }

    #[test]
    fn expired_package_is_marked() {
        let now = parse_timestamp_ms(Some(&json!("2026-10-10 00:00:00"))).unwrap();
        let r = resource_summary(&sum(100.0, 50.0, "2026-10-01 00:00:00", ""), now);
        assert_eq!(r["expired"], json!(true));
    }

    #[test]
    fn merge_resources_prefers_detail_and_drops_duplicate_summary() {
        let detail = vec![json!({"packageCode": "A", "remaining": 10.0})];
        let summary = vec![
            json!({"packageCode": "A", "remaining": 99.0}),
            json!({"packageCode": "B", "remaining": 5.0}),
        ];
        let merged = merge_resources(summary, detail);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["remaining"], json!(10.0)); // 明细优先
        assert_eq!(merged[1]["packageCode"], json!("B"));
    }

    #[test]
    fn credit_result_sums_and_dedupes_expiring() {
        let now = 1_791_000_000_000i64;
        let resources = vec![
            json!({"remaining": 100.0, "total": 200.0, "expireAt": now + 1000, "expiringSoon": true, "expired": false}),
            json!({"remaining": 1.15, "total": 50.0, "expireAt": now - 1000, "expiringSoon": false, "expired": true}),
            json!({"remaining": 0.0, "total": 10.0, "expireAt": Value::Null, "expiringSoon": false, "expired": false}),
        ];
        let r = credit_result(resources, now);
        assert_eq!(r["totalRemaining"], json!(101.15));
        assert_eq!(r["totalCapacity"], json!(260.0));
        assert_eq!(r["expiringSoonRemaining"], json!(100.0));
        assert_eq!(r["expiredRemaining"], json!(1.15));
        assert_eq!(r["packageCount"], json!(3));
        assert_eq!(r["soonestExpireAt"], json!(now - 1000)); // 只看还有剩余的包
        // 有效积分包：已用完的（remaining = 0）排除，按到期时间升序（长期有效排最后）
        assert_eq!(r["activePackageCount"], json!(2));
        let active = r["activeResources"].as_array().unwrap();
        // 已过期但仍有剩余的那份排在最前（到期时间更早）
        assert_eq!(active[0]["expireAt"], json!(now - 1000));
        assert_eq!(active[1]["expireAt"], json!(now + 1000));
    }

    #[test]
    fn response_classification_matches_gateway_rules() {
        assert!(is_success(&json!({"code": 0, "data": {}})));
        assert!(is_success(&json!({"code": 200, "data": {}})));
        assert!(!is_success(&json!({"code": 401})));
        assert!(is_unauthorized(&json!({"code": 401})));
        assert!(is_unauthorized(&json!({"code": 403})));
        // WAF 10085 是客户端指纹拦截，不是 token 过期
        assert!(!is_unauthorized(&json!({"code": 10085})));
        assert!(is_route_missing(&json!({"code": 404})));
        assert!(is_route_missing(&json!({"data": {"code": "404"}})));
        // 只凭文案也要认出未授权
        assert!(is_unauthorized(&json!({"message": "登录状态已失效"})));
    }

    #[test]
    fn account_paths_cover_wrapped_responses() {
        let flat = json!({"data": {"Accounts": [{"PackageCode": "A"}]}});
        assert_eq!(resource_accounts(&flat).len(), 1);
        let wrapped = json!({"data": {"data": {"Response": {"Data": {"Accounts": [{"x": 1}, {"x": 2}]}}}}});
        assert_eq!(resource_accounts(&wrapped).len(), 2);
        assert!(has_resource_accounts(&wrapped));

        let pkgs = json!({"data": {"data": {"Packages": [{"PackageCode": "P"}]}}});
        assert_eq!(resource_packages(&pkgs).len(), 1);
        assert!(has_resource_packages(&pkgs));
        assert!(!has_resource_accounts(&pkgs));
    }

    #[test]
    fn envelope_and_plain_tokens_are_distinguished() {
        let envelope = json!({"access_token": {"$wbEncrypted": 1, "envelope": "zzz"}});
        assert_eq!(token_state_of(&envelope), TokenState::Envelope);
        let plain = json!({"access_token": "eyJhbGciOi"});
        assert_eq!(token_state_of(&plain), TokenState::Plain);
        let none = json!({});
        assert_eq!(token_state_of(&none), TokenState::Missing);

        let acc = CreditAccount {
            id: "x".into(),
            uid: "u".into(),
            name: "n".into(),
            origin: Origin::Own,
            domain: String::new(),
            token_state: TokenState::Envelope,
            raw: envelope,
        };
        assert!(!acc.queryable());
        assert!(acc.blocked_reason().is_some());
        // 阻塞原因不能泄露凭据内容
        assert!(!acc.blocked_reason().unwrap().contains("zzz"));
    }

    #[test]
    fn plaintext_credentials_win_over_envelope_regardless_of_origin() {
        // 本工具库是信封、参考工具库是明文 → 取明文
        let mut by_uid: HashMap<String, CreditAccount> = HashMap::new();
        let own = CreditAccount {
            id: "own".into(),
            uid: "u".into(),
            name: "本工具".into(),
            origin: Origin::Own,
            domain: String::new(),
            token_state: TokenState::Envelope,
            raw: json!({}),
        };
        by_uid.insert("u".into(), own);
        let reference = CreditAccount {
            id: "ref:u".into(),
            uid: "u".into(),
            name: "参考".into(),
            origin: Origin::Ref,
            domain: String::new(),
            token_state: TokenState::Plain,
            raw: json!({}),
        };
        let old = &by_uid["u"];
        let better = match (old.queryable(), reference.queryable()) {
            (false, true) => true,
            (true, false) => false,
            _ => old.origin == Origin::Ref && reference.origin == Origin::Own,
        };
        assert!(better, "明文凭据应胜出");
    }

    #[test]
    fn api_base_only_switches_between_official_origins() {
        assert_eq!(api_base_for(&json!({"domain": "www.codebuddy.cn"})), API_ENDPOINT);
        assert_eq!(api_base_for(&json!({"domain": "www.workbuddy.cn"})), WORKBUDDY_WEB_ENDPOINT);
        // 任意主机不能被拼进请求
        assert_eq!(api_base_for(&json!({"domain": "evil.example.com"})), API_ENDPOINT);
        assert_eq!(api_base_for(&json!({})), API_ENDPOINT);
    }

    #[test]
    fn refresh_headers_keep_the_plugin_source_header() {
        let h = refresh_headers(&json!({"access_token": "t", "uid": "u"}), "rt");
        assert_eq!(h.get("X-Auth-Refresh-Source").map(String::as_str), Some("plugin"));
        assert_eq!(h.get("X-Refresh-Token").map(String::as_str), Some("rt"));
        assert_eq!(h.get("Authorization").map(String::as_str), Some("Bearer t"));
    }

    #[test]
    fn paid_and_free_bodies_carry_package_codes() {
        let paid = paid_packages_body();
        assert!(paid["PackageCodes"].as_array().unwrap().len() >= 8);
        assert_eq!(paid["Status"], json!([0, 3]));
        let free = free_packages_body();
        assert!(free["PackageCodes"].as_array().unwrap().len() >= 10);
        assert!(free["SlicePeriodStartTime"].as_str().unwrap().ends_with("00:00:00"));
        assert!(free["SlicePeriodEndTime"].as_str().unwrap().ends_with("23:59:59"));
    }
}
