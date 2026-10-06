//! WorkBuddy（**国内版**）官方登录态文件 `workbuddy-desktop.info` 的路径与读写。
//!
//! 只做国内版单档位：不引入 Cn / Ai 变体枚举，档位相关字面量集中在本文开头。
//! 参考实现（workbuddy-switch）里同一份逻辑按 `WbVariant` 分叉，这里取 `Cn` 一支。
//!
//! 登录态文件是**四段 JSON**：
//!
//! ```text
//! {
//!   "account":     { uid, nickname, type, accountType, … },   // 当前账号资料
//!   "auth":        { accessToken, refreshToken, tokenType, domain, … },
//!   "accounts":    [ … ],                                     // 历史账号（并入 allAccounts）
//!   "allAccounts": [ … ]
//! }
//! ```
//!
//! 两条必须遵守的事实（踩过的坑，详见各函数注释）：
//! 1. **token 可能是 WorkBuddy 5.6 起的加密信封** `{"$wbEncrypted":…,"envelope":…}`，
//!    必须**原样**读写；降级成空串会静默毁掉登录态。
//! 2. **退出标记** `workbuddy-desktop.info.logged-out` 存在时，即使凭据齐全 WorkBuddy
//!    也视为未登录。写入成功后必须清理它。

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::modules::config::{atomic_write, now_ms, store_dir};

// ---------------------------------------------------------------------------
// 国内版常量（单一事实来源）
// ---------------------------------------------------------------------------

/// 官方登录态文件名（国内版）。
pub const AUTH_FILE_NAME: &str = "workbuddy-desktop.info";

/// 登录态文件所在目录（相对家目录，按平台）。
const AUTH_DIR_REL: &str = if cfg!(target_os = "windows") {
    "AppData/Local/CodeBuddyExtension/Data/Public/auth"
} else if cfg!(target_os = "macos") {
    "Library/Application Support/CodeBuddyExtension/Data/Public/auth"
} else {
    ".local/share/CodeBuddyExtension/Data/Public/auth"
};

/// 客户端数据根（会话正文 `projects/`、元数据 `workbuddy.db` 都在这下面）。
pub const DATA_ROOT_NAME: &str = ".workbuddy";

/// Windows 进程映像名（精确匹配，忽略 `.exe` 与大小写）。
pub const WINDOWS_IMAGE_NAMES: [&str; 1] = ["WorkBuddy"];

/// 国内版 API 基址（OAuth 与刷新用）。
pub const API_ENDPOINT: &str = "https://www.codebuddy.cn";

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// 官方登录态文件路径。
pub fn auth_file_path() -> PathBuf {
    crate::modules::config::home_dir()
        .join(AUTH_DIR_REL)
        .join(AUTH_FILE_NAME)
}

/// 客户端数据根 `~/.workbuddy`。
pub fn data_root() -> PathBuf {
    crate::modules::config::home_dir().join(DATA_ROOT_NAME)
}

/// 会话正文目录 `~/.workbuddy/projects`。
pub fn projects_dir() -> PathBuf {
    data_root().join("projects")
}

/// 会话元数据库 `~/.workbuddy/workbuddy.db`。
pub fn workbuddy_db_path() -> PathBuf {
    data_root().join("workbuddy.db")
}

/// 登录态备份目录（本工具自己的存储根下）。
fn backup_dir() -> PathBuf {
    store_dir().join("backups")
}

/// 官方退出标记：在完整认证文件名后**追加**后缀（不是替换 `.info`）。
fn logout_marker_path(auth_path: &Path) -> PathBuf {
    let mut marker = auth_path.as_os_str().to_os_string();
    marker.push(".logged-out");
    PathBuf::from(marker)
}

// ---------------------------------------------------------------------------
// 读
// ---------------------------------------------------------------------------

/// 读取本机登录态；不存在或解析失败返回 `None`。
pub fn read_auth_file() -> Option<Value> {
    read_auth_file_at(&auth_file_path())
}

/// 读取指定路径的登录态（单测注入临时文件用）。
pub fn read_auth_file_at(path: &Path) -> Option<Value> {
    if !path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 当前登录账号 uid（登录态 `account.uid`）。
pub fn current_uid() -> Option<String> {
    current_uid_of(&read_auth_file()?)
}

/// 从登录态 JSON 取 uid（`account.uid` → 根 `uid` → `account.id`）。
pub fn current_uid_of(root: &Value) -> Option<String> {
    let account = root.get("account").filter(|v| v.is_object());
    get_str(root, "uid")
        .or_else(|| account.and_then(|a| get_str(a, "uid")))
        .or_else(|| account.and_then(|a| get_str(a, "id")))
}

/// 是否已登录：有登录态、有 uid、且**没有**退出标记。
pub fn is_logged_in() -> bool {
    let path = auth_file_path();
    if logout_marker_path(&path).exists() {
        return false;
    }
    read_auth_file_at(&path)
        .as_ref()
        .and_then(current_uid_of)
        .is_some()
}

// ---------------------------------------------------------------------------
// 备份与写
// ---------------------------------------------------------------------------

/// 切换前备份当前登录态，返回备份路径；无登录态返回 `None`。
pub fn backup_auth_file() -> Option<PathBuf> {
    let path = auth_file_path();
    if !path.exists() {
        return None;
    }
    let dir = backup_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let stem = AUTH_FILE_NAME.trim_end_matches(".info");
    let dest = dir.join(format!("{stem}.{ts}.info"));
    std::fs::copy(&path, &dest).ok()?;
    Some(dest)
}

/// 从账号库记录构造官方 `account` 字段。
pub fn build_account_obj(acc: &Value) -> Value {
    let mut obj: Map<String, Value> = match acc.get("profile_raw") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    obj.insert(
        "uid".to_string(),
        acc.get("uid").cloned().unwrap_or_else(|| json!("")),
    );
    obj.insert(
        "nickname".to_string(),
        acc.get("nickname").cloned().unwrap_or_else(|| json!("")),
    );
    setdefault(&mut obj, "type", json!("personal"));
    setdefault(&mut obj, "accountType", json!(""));
    setdefault(&mut obj, "idp", json!(""));
    setdefault(&mut obj, "oneidAccountId", json!(""));
    setdefault(&mut obj, "areaInfoComplete", json!(false));
    setdefault(&mut obj, "isCurrentOneIdEnterprise", json!(false));
    setdefault(&mut obj, "isCurrentOneIdPersonal", json!(false));
    setdefault(&mut obj, "isFirstLogin", json!(false));
    setdefault(&mut obj, "isCreator", json!(false));
    setdefault(&mut obj, "isAdmin", json!(false));
    setdefault(&mut obj, "uin", json!(""));
    setdefault(&mut obj, "phoneNumber", json!(""));
    setdefault(&mut obj, "lastLogin", json!(true));
    setdefault(&mut obj, "pluginEnabled", json!(true));
    setdefault(
        &mut obj,
        "deployStatus",
        json!({"statusCode": 0, "statusMsg": "", "detailMsg": ""}),
    );
    setdefault(
        &mut obj,
        "sso",
        json!({"domain": "", "domainModifiedTimes": 0}),
    );
    Value::Object(obj)
}

/// 从账号库记录构造官方 `auth` 字段。
///
/// token 用 `secret_value` 读取：明文字符串与 5.6 加密信封都原样写回，
/// 由 WorkBuddy 自行解密（同一 keyblob）。
pub fn build_auth_obj(acc: &Value) -> Value {
    let mut obj: Map<String, Value> = Map::new();
    let raw = acc.get("auth_raw");
    if let Some(Value::Object(m)) = raw {
        let inner = match m.get("auth") {
            Some(Value::Object(im)) => im.clone(),
            _ => m.clone(),
        };
        obj.extend(inner);
    }
    let token_type = acc
        .get("token_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Bearer")
        .to_string();
    let expires_at = acc.get("expiresAt").and_then(|v| v.as_i64());
    let now = now_ms();

    obj.insert(
        "accessToken".to_string(),
        secret_value(acc, "access_token").unwrap_or_else(|| json!("")),
    );
    obj.insert(
        "refreshToken".to_string(),
        secret_value(acc, "refresh_token").unwrap_or_else(|| json!("")),
    );
    obj.insert("tokenType".to_string(), token_type.into());
    obj.insert(
        "domain".to_string(),
        get_str(acc, "domain").unwrap_or_default().into(),
    );
    obj.insert("lastRefreshTime".to_string(), json!(now));
    setdefault(
        &mut obj,
        "scope",
        json!("openid profile offline_access email"),
    );

    if let Some(expires_at) = expires_at {
        obj.insert("expiresAt".to_string(), json!(expires_at));
        obj.insert(
            "expiresIn".to_string(),
            json!(((expires_at - now) / 1000).max(0)),
        );
        let refresh_exp = raw
            .and_then(|r| r.get("refreshExpiresAt"))
            .and_then(|v| v.as_i64())
            .unwrap_or(expires_at);
        if !obj.contains_key("refreshExpiresAt") {
            obj.insert("refreshExpiresAt".to_string(), json!(refresh_exp));
        }
        obj.insert(
            "refreshExpiresIn".to_string(),
            json!(((refresh_exp - now) / 1000).max(0)),
        );
    } else {
        setdefault(&mut obj, "expiresIn", json!(0));
        setdefault(&mut obj, "refreshExpiresIn", json!(0));
    }
    setdefault(&mut obj, "notBeforePolicy", json!(0));
    setdefault(&mut obj, "sessionState", json!(""));
    Value::Object(obj)
}

/// 把账号写入本机登录态文件（原子写 + 写后校验 + 清理退出标记）。
pub fn write_account_to_auth_file(acc: &Value) -> Result<(), String> {
    write_account_to_auth_file_at(acc, &auth_file_path())
}

/// 写入指定路径（单测注入临时文件用）。
pub fn write_account_to_auth_file_at(acc: &Value, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let existing = read_auth_file_at(path).unwrap_or_else(|| json!({}));
    let all_accounts = existing
        .get("allAccounts")
        .cloned()
        .or_else(|| existing.get("accounts").cloned())
        .filter(|v| v.is_array())
        .unwrap_or_else(|| json!([]));
    let account_obj = build_account_obj(acc);
    let auth_obj = build_auth_obj(acc);

    // 目标账号并入 allAccounts（按 uid / id 去重）
    let target_uid = get_str(acc, "uid").unwrap_or_default();
    let mut all: Vec<Value> = all_accounts.as_array().cloned().unwrap_or_default();
    all.retain(|a| {
        let primary = a
            .get("uid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                a.get("id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("");
        primary != target_uid
    });
    all.push(account_obj.clone());

    let session = json!({
        "account": &account_obj,
        "auth": &auth_obj,
        "accounts": &all,
        "allAccounts": &all,
    });
    let content = serde_json::to_string_pretty(&session).map_err(|e| e.to_string())?;
    atomic_write(path, &content).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "无权限写入 WorkBuddy 登录态文件：请先完全退出 WorkBuddy 后重试".to_string()
        } else {
            e.to_string()
        }
    })?;

    // 写后校验：按值比较（token 可能是明文，也可能是加密信封对象）
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let written_token = written
        .get("auth")
        .and_then(|a| a.get("accessToken"))
        .cloned()
        .unwrap_or(Value::Null);
    let expect_token = auth_obj.get("accessToken").cloned().unwrap_or(Value::Null);
    if written_token != expect_token {
        return Err("登录态文件写后校验失败，未写入目标账号".to_string());
    }

    // 退出标记优先于凭据：写入成功后必须清理，否则 WorkBuddy 仍显示未登录。
    let marker = logout_marker_path(path);
    match std::fs::remove_file(&marker) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "清理 WorkBuddy 退出标记失败（{}）：{e}",
                marker.display()
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 导入本机登录态
// ---------------------------------------------------------------------------

/// 从本机 WorkBuddy 当前登录态导入一条账号；无有效 token 返回 `None`。
pub fn import_from_auth_file() -> Option<Value> {
    imported_account_from_root(read_auth_file()?)
}

/// 从登录态 JSON 构造账号库记录（单测可注入）。
pub fn imported_account_from_root(root: Value) -> Option<Value> {
    let account_obj = root
        .get("account")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let auth_obj = root
        .get("auth")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));

    let uid = current_uid_of(&root);
    let nickname = secret_value(&root, "nickname")
        .or_else(|| secret_value(&root, "name"))
        .or_else(|| secret_value(&account_obj, "nickname"))
        .or_else(|| secret_value(&account_obj, "label"));
    let email = get_str(&root, "email")
        .or_else(|| get_str(&account_obj, "email"))
        .or_else(|| get_str(&auth_obj, "email"));
    let access_token = secret_value(&auth_obj, "accessToken")
        .or_else(|| secret_value(&auth_obj, "access_token"))
        .or_else(|| secret_value(&root, "accessToken"))
        .or_else(|| secret_value(&root, "access_token"));
    let refresh_token = secret_value(&auth_obj, "refreshToken")
        .or_else(|| secret_value(&auth_obj, "refresh_token"))
        .or_else(|| secret_value(&root, "refreshToken"))
        .or_else(|| secret_value(&root, "refresh_token"));
    let token_type = get_str(&auth_obj, "tokenType")
        .or_else(|| get_str(&auth_obj, "token_type"))
        .unwrap_or_else(|| "Bearer".to_string());
    let domain = get_str(&root, "domain").or_else(|| get_str(&auth_obj, "domain"));
    let expires_at = parse_ts(root.get("expiresAt").or_else(|| auth_obj.get("expiresAt")));
    let refresh_expires_at = parse_ts(
        root.get("refreshExpiresAt")
            .or_else(|| auth_obj.get("refreshExpiresAt")),
    );

    // 没有 token 就没有账号（空串 / 纯空白同样算没有）
    access_token.as_ref()?;

    Some(json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "uid": uid,
        "nickname": nickname,
        "email": email,
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": token_type,
        "domain": domain,
        "expiresAt": expires_at,
        "refreshExpiresAt": refresh_expires_at,
        "auth_raw": root,
        "profile_raw": account_obj,
        "createdAt": now_ms(),
    }))
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// 取非空字符串字段；空 / 缺失返回 `None`。
pub fn get_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 凭据字段读取：明文字符串或 WorkBuddy 5.6 加密信封对象，其它类型返回 `None`。
///
/// 空白字符串按「没有值」处理——否则 `"accessToken": ""` 会被判为已登录，
/// 导入一条空凭据账号。
pub fn secret_value(v: &Value, key: &str) -> Option<Value> {
    match v.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(Value::String(s.clone())),
        Some(o @ Value::Object(map)) if map.contains_key("$wbEncrypted") => Some(o.clone()),
        _ => None,
    }
}

/// 字符串 / 数字时间戳转 i64，**秒自动升为毫秒**（官方部分接口返回相对
/// `expiresIn` 的绝对秒，混用时差 1000 倍）。
pub fn norm_ts(v: Option<&Value>) -> Option<i64> {
    let mut ts: i64 = match v {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok()? as i64,
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?,
        _ => return None,
    };
    if ts < 10_000_000_000 {
        ts *= 1000;
    }
    Some(ts)
}

/// 字符串 / 数字时间戳转 i64（数字原样，不做秒↔毫秒换算）。
fn parse_ts(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

fn setdefault(map: &mut Map<String, Value>, key: &str, value: Value) {
    if !map.contains_key(key) {
        map.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempAuth(PathBuf);
    impl TempAuth {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "wb-auth-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> PathBuf {
            self.0.join(AUTH_FILE_NAME)
        }
        fn marker(&self) -> PathBuf {
            self.0.join(format!("{AUTH_FILE_NAME}.logged-out"))
        }
    }
    impl Drop for TempAuth {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn auth_file_path_points_at_codebuddy_extension_dir() {
        let p = auth_file_path();
        let s = p.to_string_lossy();
        assert!(s.contains("CodeBuddyExtension"), "路径应含 CodeBuddyExtension: {s}");
        assert!(s.ends_with(AUTH_FILE_NAME), "文件名应为 {AUTH_FILE_NAME}: {s}");
    }

    #[test]
    fn write_clears_logout_marker_and_verifies_token() {
        // 明文与 5.6 加密信封两种 token 形态都要能原样写回
        for token in [
            json!("test-access-token"),
            json!({"$wbEncrypted": 1, "envelope": {"v": 1, "wrapped": "test"}}),
        ] {
            let dir = TempAuth::new();
            let path = dir.path();
            let marker = dir.marker();
            std::fs::write(&marker, "logged-out").unwrap();
            let acc = json!({"uid": "u-target", "access_token": token});

            write_account_to_auth_file_at(&acc, &path).unwrap();

            assert!(!marker.exists(), "切换成功必须清理退出标记");
            let written = read_auth_file_at(&path).unwrap();
            assert_eq!(written["auth"]["accessToken"], token, "token 必须原样写回");
            assert_eq!(written["account"]["uid"], "u-target");
            assert_eq!(written["allAccounts"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn write_merges_all_accounts_without_duplicate_uid() {
        let dir = TempAuth::new();
        let path = dir.path();
        write_account_to_auth_file_at(&json!({"uid": "u-1", "access_token": "t1"}), &path).unwrap();
        write_account_to_auth_file_at(&json!({"uid": "u-2", "access_token": "t2"}), &path).unwrap();
        // 再写 u-1：应替换而不是新增
        write_account_to_auth_file_at(&json!({"uid": "u-1", "access_token": "t1b"}), &path).unwrap();

        let all = read_auth_file_at(&path).unwrap()["allAccounts"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(all.len(), 2, "同 uid 不应重复入列: {all:?}");
        let uids: Vec<&str> = all.iter().filter_map(|a| a["uid"].as_str()).collect();
        assert!(uids.contains(&"u-1") && uids.contains(&"u-2"));
    }

    #[test]
    fn blank_access_token_is_not_imported() {
        for blank in ["", "   ", "\t\n"] {
            let imported = imported_account_from_root(json!({
                "account": {"uid": "u-1", "nickname": "小明"},
                "auth": {"accessToken": blank, "refreshToken": "RT-1"}
            }));
            assert!(imported.is_none(), "空 accessToken（{blank:?}）不得导入");
        }
        // 加密信封不受影响
        let envelope = json!({"$wbEncrypted": 1, "envelope": "enc"});
        let imported = imported_account_from_root(json!({
            "account": {"uid": "u-1"},
            "auth": {"accessToken": envelope.clone()}
        }))
        .expect("加密信封必须可导入");
        assert_eq!(imported["access_token"], envelope);
    }

    #[test]
    fn import_extracts_uid_nickname_and_expiry() {
        let acc = imported_account_from_root(json!({
            "account": {"uid": "u-1", "nickname": "小明", "email": "a@b.c"},
            "auth": {
                "accessToken": "AT-1", "refreshToken": "RT-1",
                "tokenType": "Bearer", "domain": "www.codebuddy.cn",
                "expiresAt": "1791912333558"
            }
        }))
        .expect("应导入");
        assert_eq!(acc["uid"], "u-1");
        assert_eq!(acc["nickname"], "小明");
        assert_eq!(acc["email"], "a@b.c");
        assert_eq!(acc["expiresAt"], 1791912333558i64);
        assert_eq!(acc["token_type"], "Bearer");
        assert!(acc["id"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn failed_marker_cleanup_is_not_reported_as_success() {
        let dir = TempAuth::new();
        let path = dir.path();
        let marker = dir.marker();
        // 同名非空目录 → remove_file 失败，且不依赖平台权限行为
        std::fs::create_dir(&marker).unwrap();
        std::fs::write(marker.join("keep"), "x").unwrap();

        let err = write_account_to_auth_file_at(&json!({"uid": "u", "access_token": "t"}), &path)
            .unwrap_err();
        assert!(err.contains("退出标记"), "错误应说明未清理退出标记: {err}");
        assert!(marker.exists());
    }

    #[test]
    fn is_logged_in_respects_marker_and_uid() {
        // 只测纯函数分支，不碰真实登录态
        assert_eq!(current_uid_of(&json!({"account": {"uid": "u-1"}})), Some("u-1".to_string()));
        assert_eq!(current_uid_of(&json!({"uid": "u-root"})), Some("u-root".to_string()));
        assert_eq!(current_uid_of(&json!({})), None);
    }
}
