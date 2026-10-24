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

use std::path::PathBuf;

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
        // 可用的排前面 → 默认账号 → 越紧迫越靠前。
        b.usable
            .cmp(&a.usable)
            .then_with(|| is_default(b).cmp(&is_default(a)))
            .then_with(|| a.urgency().partial_cmp(&b.urgency()).unwrap_or(std::cmp::Ordering::Equal))
    });
    with_cooldowns(out)
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

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// key = 池条目 key（`qoder:<id>` …），value = (解冻时刻, 冷却类型)。
fn cooldowns() -> &'static Mutex<HashMap<String, (i64, &'static str)>> {
    static T: OnceLock<Mutex<HashMap<String, (i64, &'static str)>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
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
pub fn apply_error(key: &str, http_status: u16, body_hint: &str) -> Option<(i64, &'static str)> {
    let now = now_ms();
    // 排队（10605 / isQueued）通常裹在 401/403 信封里 —— 必须先查它，
    // 否则会把「账号没问题、只是排队」误判成「凭据死」打死。
    let hint = body_hint.to_ascii_lowercase();
    if hint.contains("10605") || hint.contains("isqueued") {
        return None;
    }
    let (kind, until): (&'static str, i64) = match http_status {
        0 => ("transient", now + 60_000),
        401 | 403 => ("auth_dead", now + 30 * 60_000),
        402 => ("balance_exhausted", next_reset_window(now)),
        429 => ("rate_limited", now + 60_000),
        500..=599 => ("transient", now + 60_000),
        _ => return None,
    };
    if let Ok(mut m) = cooldowns().lock() {
        m.insert(key.to_string(), (until, kind));
    }
    Some((until, kind))
}

/// 条目当前是否在冷却中。
pub fn cooling_until(key: &str) -> Option<i64> {
    let now = now_ms();
    cooldowns()
        .lock()
        .ok()?
        .get(key)
        .copied()
        .filter(|(until, _)| *until > now)
        .map(|(until, _)| until)
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
            match kind {
                "auth_dead" => "auth_dead",
                _ => "transient",
            },
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
fn with_cooldowns(mut entries: Vec<PoolEntry>) -> Vec<PoolEntry> {
    for e in entries.iter_mut() {
        e.cooldown_until = cooling_until(&e.key);
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

fn trae_entries() -> Vec<PoolEntry> {
    let vault = store_dir().join("trae").join("vault");
    let Ok(clients) = std::fs::read_dir(&vault) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let now = qoder_accounts::now_ms();

    for client in clients.flatten() {
        let name = client.file_name().to_string_lossy().to_string();
        let scope = match name.as_str() {
            "trae-cn" | "solo-cn" => "trae-cn",
            "trae-intl" | "solo-intl" => "trae-global",
            _ => continue,
        };
        let Ok(dirs) = std::fs::read_dir(client.path()) else {
            continue;
        };
        for dir in dirs.flatten() {
            if !dir.path().is_dir() {
                continue;
            }
            let profile = read_json(&dir.path().join("profile.json"));
            let oauth = read_json(&dir.path().join("oauth.json"));
            let meta = read_json(&dir.path().join("meta.json"));

            // uid 解析顺序（越靠前越可信）：
            //   ① meta.verified_uid  ② profile.user_id
            //   ③ 从 storage.json 解出的 JWT 里取（SOLO 与 Trae 目录常有一边缺 meta，
            //      缺了就会退化成目录名 ⇒ 同一个号被算两次，池子里重复出现）
            //   ④ 目录名兜底
            let uid = meta
                .as_ref()
                .and_then(|v| v.get("verified_uid"))
                .and_then(|v| v.as_i64().map(|n| n.to_string()).or_else(|| v.as_str().map(str::to_string)))
                .or_else(|| {
                    profile
                        .as_ref()
                        .and_then(|v| v.get("user_id"))
                        .and_then(|v| v.as_i64().map(|n| n.to_string()))
                })
                .or_else(|| uid_from_storage(&dir.path()))
                .unwrap_or_else(|| dir.file_name().to_string_lossy().to_string());

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
                        .filter(|p| {
                            p.get("remaining").and_then(Value::as_f64).unwrap_or(0.0) > 0.0
                        })
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

            // Trae 的凭据是设备私钥（oauth.json 里的 privateKeyPEM），
            // 不是 bearer token —— 有文件就当有凭据，真发请求由上游适配器负责。
            let has_cred = oauth
                .as_ref()
                .and_then(|v| v.get("privateKeyPEM"))
                .and_then(Value::as_str)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let expired = expires_at.map(|t| t <= now).unwrap_or(false);

            out.push(PoolEntry {
                key: format!("trae:{uid}"),
                product: "trae".into(),
                scope: scope.into(),
                label,
                remaining,
                expires_at,
                usable: has_cred && !expired,
                cooldown_until: None,
                reason: if !has_cred {
                    Some("缺少设备凭据".into())
                } else if expired {
                    Some("积分已过期".into())
                } else {
                    None
                },
            });
        }
    }
    // SOLO 与 Trae 是同一批号，按 uid 去重（保留剩余更高的一条）。
    out.sort_by(|a, b| b.remaining.partial_cmp(&a.remaining).unwrap_or(std::cmp::Ordering::Equal));
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
    // ① 精确匹配
    let mut scopes: Vec<String> = catalog
        .iter()
        .filter(|(_, models)| models.iter().any(|m| m == model))
        .map(|(k, _)| k.clone())
        .collect();
    let mut canonical = model.to_string();

    // ② 精确匹配不到时，按归一化指纹再找一次（大小写 / 连字符差异是常见输入误差）。
    //    找到就用**清单里的规范名**，避免把 `DeepSeek-V4-Flash` 原样转给只认小写的上游。
    if scopes.is_empty() {
        let want = normalize_model_key(model);
        for (k, models) in &catalog {
            if let Some(hit) = models.iter().find(|m| normalize_model_key(m) == want) {
                if !scopes.contains(k) {
                    scopes.push(k.clone());
                }
                canonical = hit.clone();
            }
        }
    }

    if scopes.is_empty() {
        let known = catalog
            .iter()
            .flat_map(|(k, ms)| ms.iter().map(move |m| format!("{k}:{m}")))
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
fn normalize_model_key(s: &str) -> String {
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

/// 读模型目录 `~/.twin-switch/models.json`（空 = 没登记）。
pub fn catalog() -> Vec<(String, Vec<String>)> {
    let path = store_dir().join("models.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<String, Vec<String>>>(&text)
    else {
        return Vec::new();
    };
    map.into_iter().collect()
}

/// 选号：在候选作用域里挑**最紧迫且可用**的一个。
pub fn preview(model: &str) -> Result<Selection, String> {
    let (bare, scopes) = resolve_scopes(model)?;
    let pool = usable();
    let now = now_ms();
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
            .map(|p| format!("{} 正在冷却（到 {}），下次请求会换下一个账号",
                p.label, p.cooldown_until.unwrap_or(0)))
    };
    Ok(Selection { model: bare, candidates: scopes, picked, reason })
}

/// Qoder 区域 → 作用域字符串（界面展示用）。
pub fn qoder_scope(region: QoderRegion) -> String {
    format!("qoder-{}", region.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn error_feedback_puts_each_kind_in_its_lane() {
        let now = now_ms();
        // 429 → 60 秒档
        if let Some((until, kind)) = apply_error("k429", 429, "") {
            assert_eq!(kind, "rate_limited");
            assert!((until - now - 60_000).abs() < 2_000);
        }
        // 401 → 30 分钟档
        if let Some((until, kind)) = apply_error("k401", 401, "") {
            assert_eq!(kind, "auth_dead");
            assert!((until - now - 30 * 60_000).abs() < 2_000);
        }
        // 排队（10605）→ 不冷却
        assert!(apply_error("kq", 403, "code 10605 isQueued").is_none());
        // 客户端参数错 → 不冷却
        assert!(apply_error("k4xx", 400, "bad request").is_none());
        // 402 → 停到下一个重置窗口（必定 > 现在 + 1 小时）
        if let Some((until, kind)) = apply_error("k402", 402, "") {
            assert_eq!(kind, "balance_exhausted");
            assert!(until > now + 3_600_000, "402 应停到每日重置窗口");
        }
    }

    #[test]
    fn urgency_sorts_missing_expiry_last() {
        let mk = |expires_at: Option<i64>| PoolEntry {
            key: "k".into(), product: "p".into(), scope: "s".into(), label: "l".into(),
            remaining: None, expires_at, usable: true, reason: None, cooldown_until: None,
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
