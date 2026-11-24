//! 导入「凭据账号」：只带 token / refreshToken 的那份 JSON（issue #25）。
//!
//! ## 输入格式（第三方账号管理工具导出的那种）
//!
//! ```json
//! { "accounts": [ { "name": "xxx", "token": "<JWT>", "refreshToken": "…",
//!   "enabled": true, "userId": "xxx", "phone": "xxx",
//!   "tokenExpiresAt": "2026-12-07T02:09:58.000Z",
//!   "refreshExpiresAt": "2027-05-22T02:09:58.000Z" } ] }
//! ```
//!
//! 也接受「单个对象」和「裸数组」两种形状 —— 用户手上有什么就贴什么，别让人先改格式。
//!
//! ## 这类账号能做什么、不能做什么（必须写在界面上）
//!
//! 落库只写出 `oauth.json`（+ 一份 `meta.json`），**没有客户端载体文件**
//! （`User/globalStorage/storage.json` 那套）。所以：
//!
//! - ✅ 查积分 / 签到 / 看账号信息（只需要 token）；
//! - ✅ **可以切换**：`meta.files` 为空时，切换流程会**以当前登录态为骨架合成载体**
//!   （与 `kind=oauth` 的既有账号同一条路），不需要我们手里有载体文件；
//! - ⚠️ token 过期后大概率续不上：Trae 的续期要用**登录时注册的私钥**签名，
//!   而这份 JSON 里没有私钥（我们写 `privateKeyPEM: null`，不编假值）。
//!   所以请在过期前用官方客户端重新登录一次。
//!
//! 这些限制不写成「已知问题」而写进 `note` 与界面，是因为「导入成功」四个字
//! 会让人以为它能像本地账号一样切来切去。
//!
//! ## uid 从哪来
//!
//! `token` 是 JWT，载荷 `data.id` 就是 uid（与本地库 `storage.json` 里的一致）；
//! 取不到才退回 JSON 里的 `userId`。**两者都没有就跳过这一条**，不猜。

use std::path::PathBuf;

use base64::Engine;
use chrono::TimeZone;
use serde_json::{json, Value};

use crate::modules::trae_vault;

/// 需要签名的客户端对应的默认 host（与官方客户端 oauth.json 里的值一致）。
fn default_host(client_key: &str) -> &'static str {
    if client_key.starts_with("trae-cn") || client_key == "trae" {
        "https://api.trae.cn"
    } else {
        "https://api.trae.ai"
    }
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

/// 从 JWT 载荷里取 uid（`data.id`）。
fn uid_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    // JWT 段是 base64url，且可能省略 padding。
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("data")
        .and_then(|d| d.get("id"))
        .and_then(|id| {
            id.as_str()
                .map(str::to_string)
                .or_else(|| id.as_i64().map(|n| n.to_string()))
        })
        .filter(|s| !s.trim().is_empty())
}

/// `2026-12-07T02:09:58.000Z` → RFC3339（原样即可，Trae 的过期时间就是 ISO8601）。
fn norm_time(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

/// 秒级时间戳（JWT 的 exp 用它）。
fn epoch_of(iso: Option<&Value>) -> Option<i64> {
    let s = iso?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp())
}

fn account_id_for(uid: &str) -> String {
    // 与本地库的命名一致：`oauth_<uid 后 6 位>`；太短就整段用上。
    let tail: String = uid.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
    format!("oauth_{tail}")
}

/// 把一条凭证写进账号库（oauth.json + meta.json），返回账号 id。
///
/// 导入 JSON 与「手动填 token」都走这里 —— 两条入口的落盘形状必须一模一样，
/// 否则同一个账号会因为入口不同而表现不同（这是最难查的一类 bug）。
#[allow(clippy::too_many_arguments)]
fn write_account(
    client_key: &str,
    uid: &str,
    name: &str,
    token: &str,
    refresh: &str,
    expired_at: Option<String>,
    refresh_expired_at: Option<String>,
    token_exp: i64,
) -> Result<(String, bool), String> {
    let id = account_id_for(uid);
    let dir: PathBuf = trae_vault::account_dir(client_key, &id);
    let existed = dir.exists();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建账号目录失败：{e}"))?;

    let oauth = json!({
        "appVersion": "",
        "avatar": "",
        "client": client_key,
        "createdAt": now_rfc3339(),
        "deviceId": "",
        "deviceSource": "凭据导入（无客户端设备信息）",
        "displayName": if name.trim().is_empty() { uid } else { name },
        "expiredAt": expired_at,
        "host": default_host(client_key),
        "id": id,
        "kind": "oauth",
        "loginSource": "credential-import",
        // 没有私钥：续期会失败。用 null 明确表示「不知道」，不编一个假值。
        "privateKeyPEM": Value::Null,
        "publicKeyPEM": Value::Null,
        "refreshExpiredAt": refresh_expired_at,
        "refreshToken": refresh,
        "token": token,
        "tokenExp": token_exp,
    });
    std::fs::write(
        dir.join("oauth.json"),
        serde_json::to_string_pretty(&oauth).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写 oauth.json 失败：{e}"))?;

    // meta：`entries` 为空是**故意的** —— 表示「没有载体文件」，
    // 切换时会以当前登录态为骨架合成载体（前端 `hasCarrier=false` 那条路）。
    let meta = json!({
        "id": id,
        "client": client_key,
        "kind": "oauth",
        "root_dir": "",
        "entries": [],
        "files": [],
        "file_count": 0,
        "total_bytes": 0,
        "created_at": now_rfc3339(),
        "last_used_at": Value::Null,
        "verified_uid": uid,
        "credential_only": true,
        "note": "凭据导入：可切换（首次会以当前登录态为骨架合成载体）；无私钥，token 过期后需用官方客户端重新登录",
    });
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写 meta.json 失败：{e}"))?;
    Ok((id, existed))
}

/// **手动填 token**（与 WorkBuddy 的「手动添加账号（Token）」同一套交互）。
///
/// 只做本地校验（三段式 / 有 uid / 没过期），**不发任何网络请求** ——
/// 用户填的是从客户端或浏览器里复制出来的现成 token，本地就能判真伪。
pub fn add_token(
    client_key: &str,
    access: &str,
    refresh: Option<&str>,
    display_name: Option<&str>,
) -> Result<Value, String> {
    let token = strip_bearer(access);
    if token.is_empty() {
        return Err("access_token 不能为空".into());
    }
    if token.split('.').count() != 3 {
        return Err(
            "access_token 不是三段式 JWT（应为 xxx.yyy.zzz）——多半是复制时被截断了".into(),
        );
    }
    let uid = uid_from_jwt(&token)
        .ok_or_else(|| "解不开 token 里的 uid（payload.data.id）——可能不是 Trae 的登录 token".to_string())?;
    let payload = jwt_payload(&token).unwrap_or(Value::Null);
    let exp = payload.get("exp").and_then(Value::as_i64).unwrap_or(0);
    let exp_ms = if exp > 10_000_000_000 { exp } else { exp * 1000 };
    let now = chrono::Local::now().timestamp_millis();
    if exp_ms <= now {
        return Err(format!(
            "这枚 token 已在 {} 过期，请重新获取",
            chrono::Local
                .timestamp_millis_opt(exp_ms)
                .single()
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| exp_ms.to_string())
        ));
    }
    let expired_at = chrono::Local
        .timestamp_millis_opt(exp_ms)
        .single()
        .map(|t| t.to_rfc3339());

    // Trae 的 refreshToken 不是 JWT（`xxx=.hex` 形状），不做三段式校验。
    let refresh_token = refresh.map(strip_bearer).unwrap_or_default();
    let (id, existed) = write_account(
        client_key,
        &uid,
        display_name.unwrap_or(""),
        &token,
        &refresh_token,
        expired_at.clone(),
        None,
        exp_ms / 1000,
    )?;
    Ok(json!({
        "ok": true,
        "accountId": id,
        "uid": uid,
        "name": display_name.unwrap_or("").to_string(),
        "expiresAt": expired_at,
        "updated": existed,
        "note": if refresh_token.is_empty() {
            "没填 refreshToken：token 过期后需要重新填；不会自动续期"
        } else {
            "已保存；但这份凭据没有私钥，续期仍可能失败，过期后请用官方客户端重新登录"
        },
    }))
}

fn strip_bearer(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_prefix("Bearer ").or_else(|| t.strip_prefix("bearer ")).unwrap_or(t);
    t.trim().to_string()
}

fn jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 导入一份凭据 JSON。返回 `{imported, updated, skipped: [{name, reason}]}`。
pub fn import_credentials(client_key: &str, payload: &Value) -> Result<Value, String> {
    // 三种形状归一成数组。
    let list: Vec<Value> = if let Some(arr) = payload.get("accounts").and_then(Value::as_array) {
        arr.clone()
    } else if let Some(arr) = payload.as_array() {
        arr.clone()
    } else if payload.get("token").is_some() {
        vec![payload.clone()]
    } else {
        return Err("没找到 accounts 数组，也没找到带 token 的单个账号".into());
    };
    if list.is_empty() {
        return Err("accounts 是空的".into());
    }

    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut ids: Vec<String> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    for (idx, acc) in list.iter().enumerate() {
        let name = acc
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let token = acc.get("token").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let refresh = acc
            .get("refreshToken")
            .or_else(|| acc.get("refresh_token"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if token.is_empty() {
            skipped.push(json!({ "index": idx, "name": name, "reason": "没有 token" }));
            continue;
        }
        let uid = match uid_from_jwt(&token).or_else(|| {
            acc.get("userId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
        }) {
            Some(u) => u,
            None => {
                skipped.push(json!({ "index": idx, "name": name, "reason": "token 里没有 uid（data.id）" }));
                continue;
            }
        };

        let expired_at = norm_time(acc.get("tokenExpiresAt").or_else(|| acc.get("expiresAt")));
        let refresh_expired_at = norm_time(acc.get("refreshExpiresAt"));
        let token_exp =
            epoch_of(acc.get("tokenExpiresAt").or_else(|| acc.get("expiresAt"))).unwrap_or(0);
        let (id, existed) = write_account(
            client_key,
            &uid,
            &name,
            &token,
            &refresh,
            expired_at,
            refresh_expired_at,
            token_exp,
        )?;
        ids.push(id);
        if existed {
            updated += 1;
        } else {
            imported += 1;
        }
    }

    Ok(json!({
        "imported": imported,
        "updated": updated,
        "ids": ids,
        "skipped": skipped,
        "note": "已导入凭据账号：可查积分/签到、也能切换（首次切换会借用当前登录态合成载体）；但无私钥，token 过期后需用官方客户端重新登录",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_comes_from_jwt_payload() {
        // 用 issue #25 里那份真实结构（去掉签名部分）验证解析。
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            br#"{"data":{"id":"2609619705025184","type":"user"},"exp":1796609398,"iat":1795399798}"#,
        );
        let token = format!("h.{payload}.s");
        assert_eq!(uid_from_jwt(&token).as_deref(), Some("2609619705025184"));
    }

    #[test]
    fn uid_falls_back_to_none_for_garbage() {
        assert_eq!(uid_from_jwt("not-a-jwt"), None);
        assert_eq!(uid_from_jwt("a.b.c"), None);
    }

    #[test]
    fn add_token_rejects_obvious_garbage() {
        // 空串
        assert!(add_token("trae-cn", "  ", None, None).is_err());
        // 不是三段式（复制被截断最常见）
        assert!(add_token("trae-cn", "abc.def", None, None).is_err());
        // 三段但 payload 不是 JSON
        assert!(add_token("trae-cn", "a.b.c", None, None).is_err());
    }

    #[test]
    fn add_token_rejects_expired() {
        // exp 在 2020 年：必须当场拒，别等用户切过去才发现全 401。
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"data":{"id":"1234567890123456"},"exp":1577836800}"#);
        let token = format!("h.{payload}.s");
        let err = add_token("trae-cn", &token, None, None).unwrap_err();
        assert!(err.contains("过期"), "错误信息要点明过期：{err}");
    }

    #[test]
    fn bearer_prefix_is_tolerated() {
        // 用一个未来时间的 token 验证「带 Bearer 也认」；校验失败原因不能是「格式」。
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"data":{"id":"1234567890123456"},"exp":4102444800}"#);
        let token = format!("h.{payload}.s");
        let r = add_token("__probe_client__", &format!("Bearer {token}"), None, Some("测试"));
        match r {
            Ok(v) => {
                assert_eq!(v["uid"], "1234567890123456");
                // 清理探针目录，别在用户库里留垃圾
                let _ = std::fs::remove_dir_all(trae_vault::account_dir("__probe_client__", "oauth_123456"));
                // 父目录也要删：只删账号子目录会留下一个空的 `__probe_client__/`（踩过）。
                let _ = std::fs::remove_dir_all(trae_vault::vault_root().join("__probe_client__"));
            }
            Err(e) => assert!(!e.contains("三段"), "带 Bearer 不该被判格式错误：{e}"),
        }
    }

    #[test]
    fn account_id_matches_local_convention() {
        assert_eq!(account_id_for("2609619705025184"), "oauth_025184");
        assert_eq!(account_id_for("123"), "oauth_123");
    }
}
