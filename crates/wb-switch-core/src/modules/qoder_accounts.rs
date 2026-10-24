//! Qoder 账号库（国内 / 国际分家），照抄参考实现的数据形态。
//!
//! 文件布局（与参考实现一致）：
//!
//! ```text
//! ~/.twin-switch/qoder/
//!   accounts.cn.json          国内版账号库
//!   accounts.global.json      国际版账号库
//!   accounts.trash.cn.json    回收站（软删除）
//!   accounts.trash.global.json
//! ```
//!
//! 三条纪律：
//!
//! 1. **两个区域各存一份**：同一个 uid 在国内 / 国际是两套账号体系，不许混。
//! 2. **删除是软的**：先进回收站，随时可恢复；这也和 0.3.x 里 Trae / WorkBuddy 的做法一致。
//! 3. **写盘必须原子**：先写同目录 `.tmp` 再 `rename`，中途崩不会留半截 JSON。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::modules::config::store_dir;
use crate::modules::qoder_realm::QoderRegion;

/// 账号来源。与参考实现的 `SOURCE_*` 常量对齐。
pub const SOURCE_OAUTH: &str = "oauth";
pub const SOURCE_PAT: &str = "pat";
pub const SOURCE_DESKTOP_APP: &str = "desktop-app";
pub const SOURCE_CLI: &str = "cli";

/// 一个 Qoder 账号。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QoderAccount {
    /// 本机内部 id（uuid）。
    pub id: String,
    /// 产品侧用户 id。
    pub uid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    pub region: QoderRegion,
    /// `oauth` / `pat` / `desktop-app` / `cli`
    #[serde(default = "default_source")]
    pub source: String,
    /// 访问令牌（PAT 登录时就是用户填的那个）。
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// 令牌过期时间（毫秒时间戳）。
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// 套餐名（`free` / `pro` …），来自 user/plan。
    #[serde(default)]
    pub plan: Option<String>,
    /// 剩余额度（最近一次查询结果；未知就留空，**不许填 0**）。
    #[serde(default)]
    pub credits: Option<f64>,
    /// 额度到期时间（毫秒）。
    #[serde(default)]
    pub credits_expire_at: Option<i64>,
    /// 上次成功查到额度的时刻（毫秒）—— 卡片上的「x 分钟前更新」就是它。
    #[serde(default)]
    pub credits_updated_at: Option<i64>,
    /// **服务端登录时签发的设备令牌**（抓包里 `cosy-machinetoken`，86 字符）。
    ///
    /// ⚠️ 活动下发（`/me/campaigns`）绑定这个身份：用派生值查，部分账号（新号等）
    /// 返回空列表。客户端只在内存里持有、不落盘 —— 所以要从抓包粘贴一次存进来。
    #[serde(default)]
    pub cosy_machine_token: Option<String>,
    /// 配套的 `cosy-machinetype`（18 hex），与上面成对。
    #[serde(default)]
    pub cosy_machine_type: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    #[serde(default)]
    pub disabled: bool,
    /// 原样保留的产品侧字段，不做二次解释。
    #[serde(default)]
    pub meta: Value,
}

fn default_source() -> String {
    SOURCE_PAT.to_string()
}

impl QoderAccount {
    pub fn new(region: QoderRegion, uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            uid: uid.into(),
            name: name.into(),
            email: None,
            region,
            source: SOURCE_PAT.to_string(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            plan: None,
            credits: None,
            credits_expire_at: None,
            credits_updated_at: None,
            cosy_machine_token: None,
            cosy_machine_type: None,
            created_at: now_ms(),
            last_used_at: None,
            disabled: false,
            meta: Value::Null,
        }
    }

    /// 能不能用来查额度 / 切号。
    pub fn is_usable(&self) -> bool {
        !self.disabled && self.access_token.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false)
    }

    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        if let Some(e) = self.email.as_deref().filter(|e| !e.trim().is_empty()) {
            return e.to_string();
        }
        let tail: String = self.uid.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
        format!("uid …{tail}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountFile {
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    accounts: Vec<QoderAccount>,
}

fn one() -> u32 {
    1
}

impl Default for AccountFile {
    fn default() -> Self {
        Self { version: 1, accounts: Vec::new() }
    }
}

// ---------------------------------------------------------------- 路径

pub fn qoder_dir() -> PathBuf {
    store_dir().join("qoder")
}

pub fn accounts_file(region: QoderRegion) -> PathBuf {
    qoder_dir().join(format!("accounts.{}.json", region.as_str()))
}

pub fn trash_file(region: QoderRegion) -> PathBuf {
    qoder_dir().join(format!("accounts.trash.{}.json", region.as_str()))
}

// ---------------------------------------------------------------- 读写

fn read_file(path: &PathBuf) -> Result<AccountFile, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("账号库格式损坏：{}（{e}）", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AccountFile::default()),
        Err(e) => Err(format!("读取账号库失败：{}（{e}）", path.display())),
    }
}

fn write_file(path: &PathBuf, file: &AccountFile) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{}（{e}）", dir.display()))?;
    }
    let body = serde_json::to_vec_pretty(file).map_err(|e| format!("序列化失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("写入失败：{}（{e}）", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("落盘失败：{}（{e}）", path.display())
    })
}

// ---------------------------------------------------------------- CRUD

/// 列出某个区域的账号。
pub fn list(region: QoderRegion) -> Result<Vec<QoderAccount>, String> {
    let file = read_file(&accounts_file(region))?;
    Ok(file.accounts)
}

/// 列出两个区域的全部账号（界面一次显示完）。
pub fn list_all() -> Result<Vec<QoderAccount>, String> {
    let mut out = list(QoderRegion::Cn)?;
    out.extend(list(QoderRegion::Global)?);
    Ok(out)
}

/// 新增账号；同区域 uid 重复直接报错（不静默覆盖）。
pub fn add(account: QoderAccount) -> Result<QoderAccount, String> {
    if account.uid.trim().is_empty() {
        return Err("uid 不能为空".into());
    }
    let region = account.region;
    let path = accounts_file(region);
    let mut file = read_file(&path)?;
    if file.accounts.iter().any(|a| a.uid == account.uid) {
        return Err(format!("{} 下已经有一个 uid={} 的账号", region.as_str(), account.uid));
    }
    file.accounts.push(account.clone());
    write_file(&path, &file)?;
    Ok(account)
}

/// 按 id 改账号。
pub fn update<F>(region: QoderRegion, id: &str, f: F) -> Result<QoderAccount, String>
where
    F: FnOnce(&mut QoderAccount),
{
    let path = accounts_file(region);
    let mut file = read_file(&path)?;
    let acc = file
        .accounts
        .iter_mut()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("找不到账号 {id}"))?;
    f(acc);
    let out = acc.clone();
    write_file(&path, &file)?;
    Ok(out)
}

/// 软删除：挪进回收站。
pub fn remove(region: QoderRegion, id: &str) -> Result<QoderAccount, String> {
    let path = accounts_file(region);
    let mut file = read_file(&path)?;
    let idx = file
        .accounts
        .iter()
        .position(|a| a.id == id)
        .ok_or_else(|| format!("找不到账号 {id}"))?;
    let acc = file.accounts.remove(idx);
    write_file(&path, &file)?;

    let trash = trash_file(region);
    let mut tr = read_file(&trash)?;
    tr.accounts.push(acc.clone());
    write_file(&trash, &tr)?;
    Ok(acc)
}

/// 回收站列表。
pub fn list_trash(region: QoderRegion) -> Result<Vec<QoderAccount>, String> {
    Ok(read_file(&trash_file(region))?.accounts)
}

/// 从回收站恢复。
pub fn restore(region: QoderRegion, id: &str) -> Result<QoderAccount, String> {
    let trash = trash_file(region);
    let mut tr = read_file(&trash)?;
    let idx = tr
        .accounts
        .iter()
        .position(|a| a.id == id)
        .ok_or_else(|| format!("回收站里没有 {id}"))?;
    let acc = tr.accounts.remove(idx);
    write_file(&trash, &tr)?;

    let path = accounts_file(region);
    let mut file = read_file(&path)?;
    if file.accounts.iter().any(|a| a.uid == acc.uid) {
        return Err(format!("{} 下已存在 uid={}，无法恢复", region.as_str(), acc.uid));
    }
    file.accounts.push(acc.clone());
    write_file(&path, &file)?;
    Ok(acc)
}

/// 记一笔「刚用过」。
pub fn touch(region: QoderRegion, id: &str) {
    let _ = update(region, id, |a| a.last_used_at = Some(now_ms()));
}

/// Qoder 的本地设置（目前只存「默认使用哪个账号」）。
pub fn settings_file() -> PathBuf {
    qoder_dir().join("settings.json")
}

/// 读默认账号 uid（每个区域一个）。
pub fn default_uid(region: QoderRegion) -> Option<String> {
    let text = std::fs::read_to_string(settings_file()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("defaultUid")?
        .get(region.as_str())?
        .as_str()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// 把某个账号设为该区域的默认使用账号。
///
/// ⚠️ **这不是「切号」**：Qoder 的客户端登录态（桌面端 `auth.v1.dat` 是加密的、
/// CLI 的 `.auth/user`）本机可能根本不存在。这里只影响**网关选号优先级**，
/// 界面必须如实说明，不能让用户以为客户端也跟着换了。
pub fn set_default(region: QoderRegion, uid: &str) -> Result<(), String> {
    let path = settings_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let mut v: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    let obj = v.as_object_mut().unwrap();
    let entry = obj
        .entry("defaultUid")
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    entry
        .as_object_mut()
        .unwrap()
        .insert(region.as_str().to_string(), Value::String(uid.to_string()));
    let body = serde_json::to_vec_pretty(&v).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("写入失败：{e}"))
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_falls_back_to_uid_tail_not_empty() {
        let mut a = QoderAccount::new(QoderRegion::Cn, "uid-abcdef123456", "");
        assert_eq!(a.label(), "uid …123456");
        a.email = Some("me@example.com".into());
        assert_eq!(a.label(), "me@example.com");
        a.name = "小明".into();
        assert_eq!(a.label(), "小明");
    }

    #[test]
    fn account_without_token_is_not_usable() {
        let mut a = QoderAccount::new(QoderRegion::Cn, "u", "n");
        assert!(!a.is_usable());
        a.access_token = Some("pat-xxx".into());
        assert!(a.is_usable());
        a.disabled = true;
        assert!(!a.is_usable());
    }

    #[test]
    fn files_are_split_by_region() {
        assert_ne!(
            accounts_file(QoderRegion::Cn),
            accounts_file(QoderRegion::Global)
        );
        assert!(accounts_file(QoderRegion::Cn).ends_with("accounts.cn.json"));
        assert!(accounts_file(QoderRegion::Global).ends_with("accounts.global.json"));
    }
}
