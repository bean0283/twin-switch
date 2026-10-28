//! 积分共享 · 网关 Key。
//!
//! 客户端拿这把 Key 访问本机网关（`127.0.0.1:57892`）。三条规矩：
//!
//! 1. **明文只在创建时返回一次**：库里只存 `sha256` 与前 4 / 后 4 位。
//! 2. **吊销 ≠ 删除**：吊销立即失效但可恢复；只有已吊销的才允许删除。
//! 3. 一把 Key 可以限定作用域（留空 = 全部），限定值与作用域名逐字比对。
//!
//! 落在 `~/.twin-switch/gateway-keys.json`（与 0.3.x 其它数据同一个根）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::modules::config::store_dir;

/// 统一网关端口（用户定：57892）。
pub const GATEWAY_PORT: u16 = 57892;
/// Key 前缀，便于人眼区分。
pub const KEY_PREFIX: &str = "sk-ts-";

/// 额度：两个上限互相独立，**任一触发即算超限**；`None` = 该项不限。
///
/// 计量单位固定为 token（`prompt_tokens + completion_tokens`）——
/// 这是上游唯一按次回传的准确数字（见 `gateway_metrics::Trace`）。
/// 积分没有按次回传，只能估算，所以不做进额度判据。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyQuota {
    /// 累计 token 上限（这把 Key 一生总计）。
    #[serde(default)]
    pub max_total_tokens: Option<i64>,
    /// 每日 token 上限（本地时区自然日，跨日自动归零）。
    #[serde(default)]
    pub max_daily_tokens: Option<i64>,
}

/// 累计用量。跨日时 `day_tokens` 在**下一次记账**时归零（不需要定时任务）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyUsage {
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    /// 真正产生了用量的请求数（拿不到用量的失败请求不计）。
    #[serde(default)]
    pub requests: i64,
    /// `day_tokens` 所属日期（`YYYY-MM-DD` 本地时区）。
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub day_tokens: i64,
    #[serde(default)]
    pub last_at: Option<i64>,
}

fn today_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl KeyUsage {
    /// 当日用量（日期不是今天就是 0 —— 不靠定时任务滚动）。
    pub fn today_tokens(&self) -> i64 {
        if self.day == today_str() {
            self.day_tokens
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub suffix: String,
    /// `sha256(明文)` 小写 hex。
    pub hash: String,
    /// 允许的作用域；空 = 全部。
    #[serde(default)]
    pub scopes: Vec<String>,
    pub created_at: i64,
    #[serde(default)]
    pub revoked_at: Option<i64>,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    /// 额度（两个上限，可任一为空 = 不限）。
    #[serde(default)]
    pub quota: KeyQuota,
    /// 累计用量。
    #[serde(default)]
    pub usage: KeyUsage,
    /// 吊销原因：`quota` = 额度用尽自动吊销；`None` = 手工吊销。
    /// 恢复时要一并清空，否则界面上会一直挂着「因额度吊销」。
    #[serde(default)]
    pub revoked_reason: Option<String>,
}

impl GatewayKey {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    pub fn allows(&self, scope: &str) -> bool {
        self.is_active() && (self.scopes.is_empty() || self.scopes.iter().any(|s| s == scope))
    }

    /// 额度是否已用尽。返回 `Some(说明)` 表示该拒绝请求。
    ///
    /// 两个上限任一达到即算（`>=`：「到一定值就停」是用户原话，不是「超过才停」）。
    pub fn quota_block(&self) -> Option<String> {
        if let Some(t) = self.quota.max_total_tokens {
            if self.usage.total_tokens >= t {
                return Some(format!(
                    "累计 token 已到上限（{} / {}）",
                    self.usage.total_tokens, t
                ));
            }
        }
        if let Some(d) = self.quota.max_daily_tokens {
            let used = self.usage.today_tokens();
            if used >= d {
                return Some(format!("今日 token 已到上限（{used} / {d}），明日 0 点自动恢复"));
            }
        }
        None
    }

    /// 自检用：是否因为额度被自动吊销。
    pub fn revoked_by_quota(&self) -> bool {
        self.revoked_reason.as_deref() == Some("quota")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyFile {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    keys: Vec<GatewayKey>,
}

fn one() -> u32 {
    1
}

impl Default for KeyFile {
    fn default() -> Self {
        Self { version: 1, keys: Vec::new() }
    }
}

pub fn keys_file() -> PathBuf {
    store_dir().join("gateway-keys.json")
}

fn read() -> Result<KeyFile, String> {
    let path = keys_file();
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("Key 库格式损坏：{}（{e}）", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KeyFile::default()),
        Err(e) => Err(format!("读取 Key 库失败：{e}")),
    }
}

fn write(file: &KeyFile) -> Result<(), String> {
    let path = keys_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let body = serde_json::to_vec_pretty(file).map_err(|e| format!("序列化失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落盘失败：{e}"))
}

pub fn hash_of(plaintext: &str) -> String {
    let mut h = Sha256::new();
    h.update(plaintext.as_bytes());
    hex::encode(h.finalize())
}

/// 列表（给界面看，绝不含明文）。
pub fn list() -> Result<Value, String> {
    let file = read()?;
    Ok(serde_json::json!(file.keys))
}

/// 新建。返回 `(明文, 记录)` —— 明文只有这一次机会拿到。
pub fn create(name: &str, scopes: Vec<String>) -> Result<(String, GatewayKey), String> {
    let name = name.trim();
    let plaintext = format!("{KEY_PREFIX}{}", uuid::Uuid::new_v4().simple());
    let prefix: String = plaintext.chars().take(6).collect();
    let suffix: String = {
        let rev: String = plaintext.chars().rev().take(4).collect();
        rev.chars().rev().collect()
    };
    let rec = GatewayKey {
        id: uuid::Uuid::new_v4().to_string(),
        name: if name.is_empty() { "默认".into() } else { name.to_string() },
        prefix,
        suffix,
        hash: hash_of(&plaintext),
        scopes,
        created_at: crate::modules::qoder_accounts::now_ms(),
        revoked_at: None,
        last_used_at: None,
        quota: KeyQuota::default(),
        usage: KeyUsage::default(),
        revoked_reason: None,
    };
    let _g = lock();
    let mut file = read()?;
    file.keys.push(rec.clone());
    write(&file)?;
    Ok((plaintext, rec))
}

/// 校验明文，返回命中的 Key（不改状态）。
pub fn verify(plaintext: &str) -> Result<Option<GatewayKey>, String> {
    let h = hash_of(plaintext.trim());
    let file = read()?;
    Ok(file.keys.into_iter().find(|k| k.is_active() && k.hash == h))
}

/// 按明文找 Key，**不过滤吊销状态**。
///
/// 网关用它区分三种情况，好给出不同的报错：
/// 不存在（401 无效）／被额度自动吊销（401 + 说明怎么恢复）／手工吊销（401 已吊销）。
/// 只用 `verify` 的话，这三种在客户端看来都是同一句「Key 无效」。
pub fn find_by_token(plaintext: &str) -> Result<Option<GatewayKey>, String> {
    let h = hash_of(plaintext.trim());
    Ok(read()?.keys.into_iter().find(|k| k.hash == h))
}

/// 进程内串行化「读-改-写」。
///
/// ⚠️ 没有这把锁，并发请求（agent 并行调用很常见）会各自读到同一份旧文件、
/// 各自累加后写回，**后写的把先写的覆盖掉** —— 用量统计会静默偏小，
/// 额度就永远触发不了。网关只有一个进程，进程内锁足够；
/// 跨进程并发不在支持范围内（也不该同时跑两个网关）。
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

fn mutate(id: &str, f: impl FnOnce(&mut GatewayKey)) -> Result<GatewayKey, String> {
    let _g = lock();
    let mut file = read()?;
    let key = file
        .keys
        .iter_mut()
        .find(|k| k.id == id)
        .ok_or_else(|| format!("找不到 Key {id}"))?;
    f(key);
    let out = key.clone();
    write(&file)?;
    Ok(out)
}

pub fn revoke(id: &str) -> Result<GatewayKey, String> {
    let now = crate::modules::qoder_accounts::now_ms();
    mutate(id, |k| {
        if k.revoked_at.is_none() {
            k.revoked_at = Some(now);
            k.revoked_reason = None; // 手工吊销
        }
    })
}

pub fn restore(id: &str) -> Result<GatewayKey, String> {
    mutate(id, |k| {
        k.revoked_at = None;
        k.revoked_reason = None;
    })
}

pub fn delete(id: &str) -> Result<(), String> {
    let _g = lock();
    let mut file = read()?;
    let idx = file
        .keys
        .iter()
        .position(|k| k.id == id)
        .ok_or_else(|| format!("找不到 Key {id}"))?;
    if file.keys[idx].is_active() {
        return Err("这把 Key 还在用，请先吊销再删除".into());
    }
    file.keys.remove(idx);
    write(&file)
}

/// 记一次使用（失败不影响请求）。
pub fn touch(id: &str) {
    let _ = mutate(id, |k| k.last_used_at = Some(crate::modules::qoder_accounts::now_ms()));
}

// ---------------------------------------------------------------------------
// 额度与用量
// ---------------------------------------------------------------------------

/// 设置额度。传 `None` 或 `<= 0` 表示该项**不限**。
pub fn set_quota(
    id: &str,
    max_total_tokens: Option<i64>,
    max_daily_tokens: Option<i64>,
) -> Result<GatewayKey, String> {
    mutate(id, |k| {
        k.quota.max_total_tokens = max_total_tokens.filter(|v| *v > 0);
        k.quota.max_daily_tokens = max_daily_tokens.filter(|v| *v > 0);
    })
}

/// 清零累计与当日用量（不动额度、不动吊销状态）。
pub fn reset_usage(id: &str) -> Result<GatewayKey, String> {
    mutate(id, |k| {
        k.usage = KeyUsage::default();
        // 因额度被自动吊销的，清零后自动放行 —— 否则「重置了还是不能用」很费解。
        if k.revoked_by_quota() {
            k.revoked_at = None;
            k.revoked_reason = None;
        }
    })
}

/// 记一次真实用量，并在**跨过上限时自动吊销**。
///
/// - 拿不到任何 token 值（失败 / 上游没回 usage）时**不记账**：不让失败请求
///   去消耗额度，也不把 `requests` 灌水。
/// - 吊销是软吊销（只置 `revoked_at` + `revoked_reason="quota"`），
///   提高额度或清零用量后可以在界面上一键恢复。
pub fn record_usage(id: &str, prompt: Option<i64>, completion: Option<i64>) {
    if prompt.is_none() && completion.is_none() {
        return;
    }
    let p = prompt.unwrap_or(0).max(0);
    let c = completion.unwrap_or(0).max(0);
    let now = crate::modules::qoder_accounts::now_ms();
    let today = today_str();
    let _ = mutate(id, |k| {
        // 跨日先滚动
        if k.usage.day != today {
            k.usage.day = today.clone();
            k.usage.day_tokens = 0;
        }
        k.usage.prompt_tokens += p;
        k.usage.completion_tokens += c;
        k.usage.total_tokens += p + c;
        k.usage.day_tokens += p + c;
        k.usage.requests += 1;
        k.usage.last_at = Some(now);

        // 达到任一上限 ⇒ 自动吊销（幂等）
        if k.revoked_at.is_none() {
            let hit = k.quota.max_total_tokens.map(|t| k.usage.total_tokens >= t).unwrap_or(false)
                || k
                    .quota
                    .max_daily_tokens
                    .map(|d| k.usage.day_tokens >= d)
                    .unwrap_or(false);
            if hit {
                k.revoked_at = Some(now);
                k.revoked_reason = Some("quota".into());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_distinct() {
        assert_eq!(hash_of("abc"), hash_of("abc"));
        assert_ne!(hash_of("abc"), hash_of("abd"));
        assert_eq!(hash_of("abc").len(), 64);
    }

    #[test]
    fn scope_allow_list_is_exact() {
        let mut k = GatewayKey {
            id: "1".into(), name: "t".into(), prefix: "p".into(), suffix: "s".into(),
            hash: hash_of("x"), scopes: vec!["trae-cn".into()],
            created_at: 0, revoked_at: None, last_used_at: None,
            quota: KeyQuota::default(), usage: KeyUsage::default(), revoked_reason: None,
        };
        assert!(k.allows("trae-cn"));
        assert!(!k.allows("trae-cn-x"));
        assert!(!k.allows("wb-cn"));
        k.revoked_at = Some(1);
        assert!(!k.allows("trae-cn"), "吊销后一律拒绝");
    }

    #[test]
    fn empty_scopes_mean_all() {
        let k = GatewayKey {
            id: "1".into(), name: "t".into(), prefix: "p".into(), suffix: "s".into(),
            hash: hash_of("x"), scopes: vec![],
            created_at: 0, revoked_at: None, last_used_at: None,
            quota: KeyQuota::default(), usage: KeyUsage::default(), revoked_reason: None,
        };
        assert!(k.allows("qoder-cn"));
    }

    fn mkkey() -> GatewayKey {
        GatewayKey {
            id: "1".into(), name: "t".into(), prefix: "p".into(), suffix: "s".into(),
            hash: hash_of("x"), scopes: vec![],
            created_at: 0, revoked_at: None, last_used_at: None,
            quota: KeyQuota::default(), usage: KeyUsage::default(), revoked_reason: None,
        }
    }

    #[test]
    fn 未设额度一律放行() {
        let mut k = mkkey();
        k.usage.total_tokens = 10_000_000;
        assert!(k.quota_block().is_none(), "两个上限都为空 = 不限");
    }

    #[test]
    fn 累计上限达到即拦() {
        let mut k = mkkey();
        k.quota.max_total_tokens = Some(1000);
        k.usage.total_tokens = 999;
        assert!(k.quota_block().is_none(), "差 1 个 token 还没到");
        k.usage.total_tokens = 1000;
        assert!(k.quota_block().is_some(), "到上限即拦，不是超过才拦");
    }

    #[test]
    fn 每日上限只看当天且跨日自动归零() {
        let mut k = mkkey();
        k.quota.max_daily_tokens = Some(500);
        k.usage.day = chrono::Local::now().format("%Y-%m-%d").to_string();
        k.usage.day_tokens = 500;
        k.usage.total_tokens = 500;
        assert!(k.quota_block().is_some(), "今天到量要拦");

        // 日期换成昨天 ⇒ 当日用量视为 0，不该拦（不需要定时任务滚动）
        k.usage.day = "2000-01-01".to_string();
        assert!(k.quota_block().is_none(), "跨日后自动归零");
        assert_eq!(k.usage.today_tokens(), 0);
    }

    #[test]
    fn 两个上限任一触发就拦() {
        let mut k = mkkey();
        k.quota.max_total_tokens = Some(10_000);
        k.quota.max_daily_tokens = Some(500);
        k.usage.day = chrono::Local::now().format("%Y-%m-%d").to_string();
        k.usage.total_tokens = 400;
        k.usage.day_tokens = 500;
        let msg = k.quota_block().expect("日上限应先触发");
        assert!(msg.contains("今日"), "应命中每日口径：{msg}");
    }

    #[test]
    fn 因额度吊销可被识别() {
        let mut k = mkkey();
        k.revoked_at = Some(1);
        k.revoked_reason = Some("quota".into());
        assert!(k.revoked_by_quota());
        k.revoked_reason = None;
        assert!(!k.revoked_by_quota(), "手工吊销不算额度吊销");
    }
}
