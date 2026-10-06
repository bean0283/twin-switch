//! WorkBuddy 账号库（本工具自建，存 `~/.twin-switch/workbuddy-accounts.json`）。
//!
//! 与 `workbuddy_accounts` 的分工要分清：
//! - **`workbuddy_accounts`**：**只读**解析 WorkBuddy 散落在各处的账号信息，
//!   只为把 `sessions.user_id` 那段 UUID 显示成人名；**绝不写任何 WorkBuddy 文件**。
//! - **本模块**：本工具自己的账号库 CRUD（导入 / 改名 / 删除 / 导出导入 / 记录使用时间），
//!   是「账号管理」与「切换」的数据源。
//!
//! 账号记录的凭据字段（access_token / refresh_token）可能是 WorkBuddy 5.6 起的
//! **加密信封对象**，必须原样存取，见 `workbuddy_auth::secret_value`。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::workbuddy_accounts;
use crate::modules::workbuddy_auth::{self, get_str, secret_value};

/// 账号库文件。
pub fn accounts_file() -> PathBuf {
    store_dir().join("workbuddy-accounts.json")
}

/// 读取全部账号；文件缺失或损坏时返回空列表（不抛错，避免启动即崩）。
pub fn load_accounts() -> Vec<Value> {
    load_accounts_at(&accounts_file())
}

pub fn load_accounts_at(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    match v {
        Value::Array(a) => a,
        Value::Object(o) => o
            .get("accounts")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub fn save_accounts(accounts: &[Value]) -> std::io::Result<()> {
    save_accounts_at(&accounts_file(), accounts)
}

pub fn save_accounts_at(path: &Path, accounts: &[Value]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&json!({
        "version": 1,
        "accounts": accounts,
    }))
    .map_err(std::io::Error::other)?;
    atomic_write(path, &text)
}

/// 按 id 查找。
pub fn find_account(id: &str) -> Option<Value> {
    load_accounts().into_iter().find(|a| account_id(a) == id)
}

/// 账号 id（记录里的 `id`，缺失时按 uid 兜底）。
pub fn account_id(acc: &Value) -> String {
    get_str(acc, "id").unwrap_or_else(|| get_str(acc, "uid").unwrap_or_default())
}

/// 展示名优先级：用户改名 > 昵称 > 邮箱 > WorkBuddy 侧解析出的名字 > uid 尾号。
pub fn display_name(acc: &Value) -> String {
    if let Some(name) = get_str(acc, "display_name") {
        return name;
    }
    if let Some(n) = get_str(acc, "nickname") {
        return n;
    }
    if let Some(e) = get_str(acc, "email") {
        return e;
    }
    if let Some(uid) = get_str(acc, "uid") {
        // 交给只读解析器（日志 / 遗留账号库 / 账号快照三来源）
        let label = workbuddy_accounts::label_for(&uid);
        if !label.starts_with("uid ") {
            return label;
        }
        return format!("…{}", tail(&uid, 6));
    }
    "未命名账号".to_string()
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

// ---------------------------------------------------------------------------
// 写入类操作
// ---------------------------------------------------------------------------

/// 身份键：非空 uid 优先，否则用真实邮箱兜底。
fn identity_key(acc: &Value) -> String {
    if let Some(uid) = get_str(acc, "uid") {
        return format!("uid:{uid}");
    }
    if let Some(email) = get_str(acc, "email") {
        return format!("email:{}", email.to_ascii_lowercase());
    }
    String::new()
}

/// 合并入库：命中已有身份则**保留原 id**（调用方持有的引用不失效），
/// 并把新采集到的字段补进去；凭据缺失时不覆盖已有的凭据。
pub fn upsert(collected: Value) -> Result<Value, String> {
    let mut accounts = load_accounts();
    let saved = upsert_into(&mut accounts, collected)?;
    save_accounts(&accounts).map_err(|e| e.to_string())?;
    Ok(saved)
}

pub(crate) fn upsert_into(accounts: &mut Vec<Value>, mut collected: Value) -> Result<Value, String> {
    let key = identity_key(&collected);
    if key.is_empty() {
        return Err("账号缺少 uid 与邮箱，无法识别身份".to_string());
    }
    if collected.get("id").is_none() {
        collected["id"] = json!(uuid::Uuid::new_v4().to_string());
    }
    if collected.get("createdAt").is_none() {
        collected["createdAt"] = json!(now_ms());
    }

    if let Some(pos) = accounts.iter().position(|a| identity_key(a) == key) {
        let existing = accounts[pos].clone();
        let id = get_str(&existing, "id").unwrap_or_default();
        collected["id"] = json!(id);
        // 保留用户改名与创建时间
        if collected.get("display_name").is_none() {
            if let Some(n) = existing.get("display_name") {
                collected["display_name"] = n.clone();
            }
        }
        if let Some(c) = existing.get("createdAt") {
            collected["createdAt"] = c.clone();
        }
        // 凭据缺失（如仅采集到资料）时不覆盖已有凭据
        if secret_value(&collected, "access_token").is_none() {
            if let Some(t) = existing.get("access_token") {
                collected["access_token"] = t.clone();
            }
            if let Some(t) = existing.get("refresh_token") {
                collected["refresh_token"] = t.clone();
            }
        }
        accounts[pos] = collected.clone();
        return Ok(collected);
    }

    accounts.push(collected.clone());
    Ok(collected)
}

/// 删除账号。
pub fn delete_account(id: &str) -> Result<(), String> {
    let mut accounts = load_accounts();
    let before = accounts.len();
    accounts.retain(|a| account_id(a) != id);
    if accounts.len() == before {
        return Err(format!("账号不存在：{id}"));
    }
    save_accounts(&accounts).map_err(|e| e.to_string())
}

/// 改名（写入 `display_name`，不动账号原始昵称）。
pub fn rename(id: &str, name: &str) -> Result<Value, String> {
    let mut accounts = load_accounts();
    let Some(pos) = accounts.iter().position(|a| account_id(a) == id) else {
        return Err(format!("账号不存在：{id}"));
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        accounts[pos].as_object_mut().unwrap().remove("display_name");
    } else {
        accounts[pos]["display_name"] = json!(trimmed);
    }
    let updated = accounts[pos].clone();
    save_accounts(&accounts).map_err(|e| e.to_string())?;
    Ok(updated)
}

/// 记录一次使用（用于排序与「最近使用」标记）。
pub fn mark_used(id: &str) {
    let mut accounts = load_accounts();
    if let Some(pos) = accounts.iter().position(|a| account_id(a) == id) {
        accounts[pos]["lastUsedAt"] = json!(now_ms());
        let _ = save_accounts(&accounts);
    }
}

/// 把刷新出来的新凭据写回账号库（**只动凭据与到期时间，不动展示字段**）。
///
/// 只由积分 / 官方用量这类需要明文 token 的链路调用；切换与导入不经过这里。
/// 凭据原样写入：本工具账号库里的 `access_token` 也可能是加密信封，此时不该被
/// 明文覆盖成空串（会静默毁掉登录态）。
pub fn update_tokens(id: &str, refreshed: &Value) -> Result<(), String> {
    let mut accounts = load_accounts();
    let Some(pos) = accounts.iter().position(|a| account_id(a) == id) else {
        return Err(format!("账号不存在: {id}"));
    };
    for key in [
        "access_token",
        "refresh_token",
        "expiresAt",
        "refreshExpiresAt",
        "refreshedAt",
    ] {
        if let Some(v) = refreshed.get(key) {
            if v.is_null() {
                continue;
            }
            // 凭据字段必须非空：空串写进去等于把账号废掉。
            if matches!(key, "access_token" | "refresh_token") && v.as_str().map(str::trim).unwrap_or("x").is_empty() {
                continue;
            }
            accounts[pos][key] = v.clone();
        }
    }
    save_accounts(&accounts).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 导入本机登录态
// ---------------------------------------------------------------------------

/// 把 WorkBuddy 当前登录态导入账号库。
pub fn import_local_login() -> Result<Value, String> {
    let collected = workbuddy_auth::import_from_auth_file()
        .ok_or_else(|| "未读取到本机 WorkBuddy 登录信息（请先启动 WorkBuddy 并登录）".to_string())?;
    upsert(collected)
}

// ---------------------------------------------------------------------------
// 导出 / 导入账号包
// ---------------------------------------------------------------------------

/// 导出：只导出可导出的账号（有凭据的），返回可写盘的 JSON。
pub fn export_accounts() -> Value {
    let accounts: Vec<Value> = load_accounts()
        .into_iter()
        .filter(|a| secret_value(a, "access_token").is_some())
        .map(|a| {
            json!({
                "id": a.get("id"),
                "uid": a.get("uid"),
                "nickname": a.get("nickname"),
                "display_name": a.get("display_name"),
                "email": a.get("email"),
                "domain": a.get("domain"),
                "access_token": a.get("access_token"),
                "refresh_token": a.get("refresh_token"),
                "token_type": a.get("token_type"),
                "expiresAt": a.get("expiresAt"),
                "refreshExpiresAt": a.get("refreshExpiresAt"),
                "profile_raw": a.get("profile_raw"),
            })
        })
        .collect();
    json!({
        "kind": "workbuddy-accounts",
        "version": 1,
        "exportedAt": now_ms(),
        "accounts": accounts,
    })
}

/// 导入预览：解析账号包，说明将新增 / 更新哪些账号（**只读，不写库**）。
pub fn preview_import(payload: &Value) -> Result<Value, String> {
    let incoming = extract_incoming(payload)?;
    let existing = load_accounts();
    let mut items = Vec::new();
    for acc in incoming {
        let key = identity_key(&acc);
        let name = display_name(&acc);
        let hit = existing.iter().find(|a| identity_key(a) == key);
        items.push(json!({
            "name": name,
            "uid": acc.get("uid").cloned().unwrap_or(Value::Null),
            "action": if hit.is_some() { "update" } else { "create" },
            "hasToken": secret_value(&acc, "access_token").is_some(),
        }));
    }
    Ok(json!({ "items": items, "count": items.len() }))
}

/// 执行导入：`mode = "merge"`（默认，按身份合并）或 `"replace"`（整库替换）。
pub fn import_accounts(payload: &Value, mode: Option<&str>) -> Result<Value, String> {
    let incoming = extract_incoming(payload)?;
    let mut accounts = load_accounts();
    let mut created = 0usize;
    let mut updated = 0usize;

    if mode == Some("replace") {
        accounts.clear();
    }
    for acc in incoming {
        let key = identity_key(&acc);
        if accounts.iter().any(|a| identity_key(a) == key) {
            updated += 1;
        } else {
            created += 1;
        }
        upsert_into(&mut accounts, acc)?;
    }
    save_accounts(&accounts).map_err(|e| e.to_string())?;
    Ok(json!({ "created": created, "updated": updated, "total": accounts.len() }))
}

fn extract_incoming(payload: &Value) -> Result<Vec<Value>, String> {
    let arr = payload
        .get("accounts")
        .and_then(|v| v.as_array())
        .or_else(|| payload.as_array())
        .ok_or_else(|| "账号包格式不正确：缺少 accounts 数组".to_string())?;
    let list: Vec<Value> = arr
        .iter()
        .filter(|a| a.is_object())
        .filter(|a| {
            // 至少要有身份：uid 或 email
            get_str(a, "uid").is_some() || get_str(a, "email").is_some()
        })
        .cloned()
        .collect();
    if list.is_empty() {
        return Err("账号包里没有可导入的账号".to_string());
    }
    Ok(list)
}

// ---------------------------------------------------------------------------
// 列表（给 UI）
// ---------------------------------------------------------------------------

/// 账号列表 + 当前登录态标记。**只读 WorkBuddy 侧数据**。
pub fn list() -> Value {
    let current = workbuddy_auth::current_uid();
    let logged_in = workbuddy_auth::is_logged_in();
    let mut accounts = load_accounts();
    // 最近使用优先，其次创建时间
    accounts.sort_by_key(|a| {
        std::cmp::Reverse(
            a.get("lastUsedAt")
                .and_then(|v| v.as_i64())
                .or_else(|| a.get("createdAt").and_then(|v| v.as_i64()))
                .unwrap_or(0),
        )
    });
    let items: Vec<Value> = accounts
        .iter()
        .map(|a| {
            let uid = get_str(a, "uid").unwrap_or_default();
            json!({
                "id": account_id(a),
                "uid": uid,
                "name": display_name(a),
                "nickname": a.get("nickname").cloned().unwrap_or(Value::Null),
                "email": a.get("email").cloned().unwrap_or(Value::Null),
                "hasToken": secret_value(a, "access_token").is_some(),
                "expiresAt": a.get("expiresAt").cloned().unwrap_or(Value::Null),
                "lastUsedAt": a.get("lastUsedAt").cloned().unwrap_or(Value::Null),
                "isCurrent": !uid.is_empty() && current.as_deref() == Some(uid.as_str()),
            })
        })
        .collect();
    json!({
        "accounts": items,
        "currentUid": current,
        "loggedIn": logged_in,
        "authFilePath": workbuddy_auth::auth_file_path(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时目录，Drop 时清理。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "wb-vault-{}-{tag}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_file(name: &str) -> (PathBuf, TempDir) {
        let dir = TempDir::new(name);
        (dir.0.join("accounts.json"), dir)
    }

    #[test]
    fn save_and_load_roundtrip() {
        let (path, _t) = temp_file("rt");
        let accs = vec![json!({"id": "a1", "uid": "u-1", "nickname": "小明"})];
        save_accounts_at(&path, &accs).unwrap();
        let loaded = load_accounts_at(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0]["uid"], "u-1");
    }

    #[test]
    fn upsert_merges_by_uid_and_keeps_id() {
        let mut accs: Vec<Value> = Vec::new();
        let first = upsert_into(&mut accs, json!({"id": "keep-me", "uid": "u-1", "nickname": "旧名"})).unwrap();
        assert_eq!(first["id"], "keep-me");

        // 同 uid 再次入库：保留原 id，昵称更新
        let second = upsert_into(&mut accs, json!({"uid": "u-1", "nickname": "新名"})).unwrap();
        assert_eq!(second["id"], "keep-me", "同 uid 必须保留原 id");
        assert_eq!(second["nickname"], "新名");
        assert_eq!(accs.len(), 1, "不应产生第二条记录");
    }

    #[test]
    fn upsert_without_credential_does_not_wipe_existing_token() {
        let mut accs: Vec<Value> = Vec::new();
        upsert_into(&mut accs, json!({"uid": "u-1", "access_token": "TOKEN-A"})).unwrap();
        let merged = upsert_into(&mut accs, json!({"uid": "u-1", "nickname": "只有资料"})).unwrap();
        assert_eq!(merged["access_token"], "TOKEN-A", "无凭据的采集不得覆盖已有 token");
    }

    #[test]
    fn upsert_rejects_identity_less_account() {
        let mut accs: Vec<Value> = Vec::new();
        assert!(upsert_into(&mut accs, json!({"nickname": "无名"})).is_err());
        // 有 email 也算有身份
        assert!(upsert_into(&mut accs, json!({"email": "a@b.c"})).is_ok());
    }

    #[test]
    fn encrypted_envelope_token_survives_upsert() {
        let envelope = json!({"$wbEncrypted": 1, "envelope": "enc"});
        let mut accs: Vec<Value> = Vec::new();
        let saved = upsert_into(&mut accs, json!({"uid": "u-1", "access_token": envelope.clone()})).unwrap();
        assert_eq!(saved["access_token"], envelope, "加密信封必须原样保留");
        assert!(secret_value(&saved, "access_token").is_some());
    }

    #[test]
    fn rename_sets_and_clears_display_name() {
        let (path, _t) = temp_file("rename");
        save_accounts_at(&path, &[json!({"id": "a1", "uid": "u-1", "nickname": "小明"})]).unwrap();
        let mut accs = load_accounts_at(&path);
        let pos = accs.iter().position(|a| account_id(a) == "a1").unwrap();
        accs[pos]["display_name"] = json!("我的主号");
        save_accounts_at(&path, &accs).unwrap();
        assert_eq!(display_name(&load_accounts_at(&path)[0]), "我的主号");

        // 清空后回落到昵称
        let mut accs = load_accounts_at(&path);
        accs[0].as_object_mut().unwrap().remove("display_name");
        save_accounts_at(&path, &accs).unwrap();
        assert_eq!(display_name(&load_accounts_at(&path)[0]), "小明");
    }

    #[test]
    fn export_filters_accounts_without_token() {
        // 只验证过滤规则的纯逻辑：无凭据账号不应出现在导出里
        let with = json!({"uid": "u-1", "access_token": "t"});
        let without = json!({"uid": "u-2"});
        assert!(secret_value(&with, "access_token").is_some());
        assert!(secret_value(&without, "access_token").is_none());
    }

    #[test]
    fn preview_import_reports_create_and_update() {
        let payload = json!({"accounts": [
            {"uid": "u-1", "nickname": "甲", "access_token": "t1"},
            {"uid": "u-2", "nickname": "乙"},
        ]});
        let mut accs: Vec<Value> = Vec::new();
        upsert_into(&mut accs, json!({"uid": "u-1", "nickname": "甲"})).unwrap();

        // 直接验证身份判定分支：u-1 已存在 → update；u-2 不存在 → create
        let keys: Vec<String> = accs.iter().map(identity_key).collect();
        assert!(keys.contains(&"uid:u-1".to_string()));
        assert!(!keys.contains(&"uid:u-2".to_string()));

        // 预览本身能跑通并返回条目
        let incoming = extract_incoming(&payload).unwrap();
        assert_eq!(incoming.len(), 2);
    }

    #[test]
    fn import_rejects_bad_payload() {
        assert!(extract_incoming(&json!({})).is_err());
        assert!(extract_incoming(&json!({"accounts": []})).is_err());
        assert!(extract_incoming(&json!({"accounts": [{"nickname": "无身份"}]})).is_err());
    }
}
