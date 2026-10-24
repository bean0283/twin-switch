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
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::workbuddy_accounts;
use crate::modules::workbuddy_auth::{self, get_str, secret_value};

/// 账号库文件。
pub fn accounts_file() -> PathBuf {
    store_dir().join("workbuddy-accounts.json")
}

// ---------------------------------------------------------------------------
// 「用户删掉的账号」名单（自动导入的护栏）
// ---------------------------------------------------------------------------

/// 墓碑文件：记下用户**主动删掉**的身份键，启动时的自动导入必须绕开它们。
///
/// ⚠️ 为什么必须有它（2026-10-09 真事）：T43 把「导入参考工具账号」改成**启动时自动跑**
/// 之后，用户删掉一个账号 → 下次启动又被从参考库搬回来 → **删除等于没删**。
/// 幂等只保证「本地已是明文就不覆盖」，完全管不住「这个身份用户根本不想要」。
///
/// 语义 = **用户表达过「我不要这个身份」**。因此只有用户**主动加回来**
/// （扫码 / 导入本机登录态 / 导入账号包）才撤墓碑 —— 见 [`upsert`] 与 [`import_accounts`]。
/// 键用 [`identity_key`]（`uid:` / `email:` 前缀），与合并逻辑同源。
fn import_blocklist_file() -> PathBuf {
    store_dir().join("workbuddy-import-blocklist.json")
}

/// 解析墓碑名单（纯函数）。坏 JSON 一律当**空名单** —— 护栏自己坏了，
/// 也不能让账号页打不开。
fn parse_blocklist(text: &str) -> BTreeSet<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("blocked").and_then(|x| x.as_array()).cloned())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn load_import_blocklist() -> BTreeSet<String> {
    match std::fs::read_to_string(import_blocklist_file()) {
        Ok(text) => parse_blocklist(&text),
        Err(_) => BTreeSet::new(),
    }
}

/// 落盘墓碑名单。**尽力而为**：写不进去也绝不能让「删除账号」本身失败 ——
/// 账号已经删掉了，护栏丢一次顶多让那个账号被搬回来一次，不该反过来卡住删除。
fn save_import_blocklist_at(path: &Path, set: &BTreeSet<String>) {
    let text = match serde_json::to_string_pretty(&json!({
        "version": 1,
        "blocked": set.iter().collect::<Vec<_>>(),
    })) {
        Ok(t) => t,
        Err(_) => return,
    };
    let _ = atomic_write(path, &text);
}

fn read_blocklist_at(path: &Path) -> BTreeSet<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => parse_blocklist(&t),
        Err(_) => BTreeSet::new(),
    }
}

/// 记一个墓碑（路径可注入 ⇒ 单测用临时文件，**不碰真目录**）。
fn block_identity_at(path: &Path, key: &str) {
    if key.is_empty() {
        return;
    }
    let mut set = read_blocklist_at(path);
    if set.insert(key.to_string()) {
        save_import_blocklist_at(path, &set);
    }
}

/// 撤一个墓碑（用户主动把账号加回来时用）。
fn unblock_identity_at(path: &Path, key: &str) {
    if key.is_empty() {
        return;
    }
    let mut set = read_blocklist_at(path);
    if set.remove(key) {
        save_import_blocklist_at(path, &set);
    }
}

fn block_identity(key: &str) {
    block_identity_at(&import_blocklist_file(), key);
}

fn unblock_identity(key: &str) {
    unblock_identity_at(&import_blocklist_file(), key);
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

/// 账号库落盘（默认路径）—— **本工具账号库的唯一写出口**。
///
/// ⚠️ 落盘之后顺手作废积分缓存：缓存里存的是「当时那批账号的查询结果」，账号库一改就
/// 不再同源，5 分钟 TTL 内旧结果会把「加密信封、查询失败」盖在新账号库上（T41：
/// 同一屏不许两个不同源）。放在这里而不是各调用点，是因为写入口有七处，靠人记必然漏。
/// 单测走 [`save_accounts_at`]（临时路径），不会碰全局缓存。
pub fn save_accounts(accounts: &[Value]) -> std::io::Result<()> {
    let r = save_accounts_at(&accounts_file(), accounts);
    if r.is_ok() {
        crate::modules::workbuddy_credits::clear_cache();
    }
    r
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
    // ⚠️ 用户**主动**把账号加回来（扫码 / 导入本机登录态）⇒ 撤掉墓碑。
    // 只在这里撤：`upsert_into` 被自动导入复用，在那一层撤 = 自动导入自己拆自己的护栏。
    unblock_identity(&identity_key(&collected));
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

/// 去掉用户可能一起粘进来的 `Bearer ` 前缀与首尾空白。
fn strip_bearer(raw: &str) -> String {
    let t = raw.trim();
    let t = t
        .strip_prefix("Bearer ")
        .or_else(|| t.strip_prefix("bearer "))
        .unwrap_or(t);
    t.trim().to_string()
}

/// 毫秒时间戳 → `2026-11-04 12:30`（本地时区，给错误提示用）。
fn fmt_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// 记录 / 清除「服务端已拒绝这枚凭据」的状态（`authError` + `authErrorAt`）。
///
/// ## 为什么不能只看 `exp`
///
/// 401 是服务端**提前**作废凭据（账号在别处重新登录、会话被踢）。此时 token 里的
/// `exp` 仍然在未来 —— 账号池若只比 `exp`，就会把这个号显示成「可用」，
/// 用户切过去必然失败（真机：130****9226 剩余「未知」、到期「21 天后」，状态却是「可用」）。
///
/// ⚠️ 状态没变就不写盘：`save_accounts` 会顺带清积分缓存，频繁写会让界面反复重查。
pub fn mark_auth_error(id: &str, reason: Option<&str>) {
    let mut accounts = load_accounts();
    let Some(pos) = accounts.iter().position(|a| account_id(a) == id) else {
        return;
    };
    let current = accounts[pos]
        .get("authError")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let next = reason.unwrap_or("").trim().to_string();
    if current == next {
        return;
    }
    if next.is_empty() {
        if let Some(obj) = accounts[pos].as_object_mut() {
            obj.remove("authError");
            obj.remove("authErrorAt");
        }
    } else {
        accounts[pos]["authError"] = json!(next);
        accounts[pos]["authErrorAt"] = json!(now_ms());
    }
    let _ = save_accounts(&accounts);
}

/// **手动添加账号**：直接粘 `access_token`（可选 `refresh_token`），不再走一次登录。
///
/// ## 为什么需要这条路径
///
/// 扫码 / 导入本机登录态都会走一次完整授权流程（消耗 re 额度）。有时用户手上
/// 已经有可用 token（从已登录的客户端或浏览器里复制），只想把它登记进来用于切换。
///
/// ## 校验（只做本地能做的，绝不假装成功）
///
/// - 必须是三段式 JWT（复制不完整是最常见的错）；
/// - `sub` 必须存在 —— 没有它无法识别身份，切号也没法对齐；
/// - `exp` 必须**还没过期**（已过期直接拒绝，并把到期时间写进错误里）；
/// - 给了 `refresh_token` 时：同样要三段、没过期，且 `sub` 与 access 一致
///   （两个 token 来自不同账号拼在一起，会得到一个永远刷不新的账号）。
pub fn add_manual_token(
    access: &str,
    refresh: Option<&str>,
    domain: Option<&str>,
    display_name: Option<&str>,
) -> Result<Value, String> {
    let access = strip_bearer(access);
    if access.is_empty() {
        return Err("access_token 不能为空".into());
    }
    if access.split('.').count() != 3 {
        return Err("access_token 不是三段式 JWT（应为 xxx.yyy.zzz）——多半是复制时被截断了".into());
    }
    let ap = crate::modules::jwt::payload(&access)
        .ok_or_else(|| "解不开 access_token 的 payload（可能复制不完整或不是 JWT）".to_string())?;
    let uid = ap
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if uid.is_empty() {
        return Err("access_token 里没有 sub（账号 id），无法识别身份".into());
    }
    let Some(exp) = ap.get("exp").and_then(Value::as_i64) else {
        return Err("access_token 里没有 exp（过期时间），不像是登录 token".into());
    };
    let exp_ms = crate::modules::jwt::normalize_ms(exp);
    let now = now_ms();
    if exp_ms <= now {
        return Err(format!(
            "这枚 access_token 已在 {} 过期，请重新获取一枚",
            fmt_ms(exp_ms)
        ));
    }

    // refresh_token（可选）
    let mut refresh_token = String::new();
    let mut refresh_exp_ms: Option<i64> = None;
    if let Some(raw) = refresh {
        let r = strip_bearer(raw);
        if !r.is_empty() {
            if r.split('.').count() != 3 {
                return Err("refresh_token 不是三段式 JWT —— 要么留空，要么粘贴完整".into());
            }
            let rp = crate::modules::jwt::payload(&r)
                .ok_or_else(|| "解不开 refresh_token 的 payload（可能复制不完整）".to_string())?;
            let sub = rp.get("sub").and_then(Value::as_str).unwrap_or("").trim();
            if !sub.is_empty() && sub != uid {
                return Err(format!(
                    "refresh_token 属于另一个账号（sub={sub}），与 access_token（sub={uid}）不是同一个 —— 请别混用",
                ));
            }
            if let Some(rexp) = rp.get("exp").and_then(Value::as_i64) {
                let ms = crate::modules::jwt::normalize_ms(rexp);
                if ms <= now {
                    return Err(format!(
                        "这枚 refresh_token 已在 {} 过期；可以只填 access_token（但到期后需重新获取）",
                        fmt_ms(ms)
                    ));
                }
                refresh_exp_ms = Some(ms);
            }
            refresh_token = r;
        }
    }

    // ⚠️⚠️ 没填 refresh_token 时，尽量从**本机客户端登录态**里补一枚同账号的。
    //
    // 为什么必须补：只带 access_token 的账号是「一次性」的 —— 只要该账号在别处登录过、
    // 或客户端把自己的 session 刷新了一次，服务端就会把这枚 access 判失效；而本工具
    // 手上没有任何 refresh token，刷新请求无从发起，**只能等用户重新去抓一次**。
    // 本机登录的正好是这个 uid 时能直接补齐，用户不用再翻 DevTools。
    // 补不到也照旧允许保存（用户可能就是想临时加一下），但不会假装它「长期有效」。
    let mut refresh_filled_locally = false;
    if refresh_token.is_empty() {
        if let Some(local) = crate::modules::workbuddy_auth::import_from_auth_file() {
            let luid = crate::modules::workbuddy_auth::get_str(&local, "uid").unwrap_or_default();
            let lrt =
                crate::modules::workbuddy_auth::get_str(&local, "refresh_token").unwrap_or_default();
            if luid == uid && !lrt.is_empty() && lrt.split('.').count() == 3 {
                if let Some(rp) = crate::modules::jwt::payload(&lrt) {
                    let sub = rp.get("sub").and_then(Value::as_str).unwrap_or("").trim();
                    let rexp = rp.get("exp").and_then(Value::as_i64).map(crate::modules::jwt::normalize_ms);
                    // 同账号 + 没过期才敢用；否则宁可不填，也不能塞一枚必然失败的续期令牌。
                    if sub == uid && rexp.map(|ms| ms > now).unwrap_or(true) {
                        refresh_token = lrt;
                        refresh_exp_ms = rexp;
                        refresh_filled_locally = true;
                    }
                }
            }
        }
    }

    let nickname = ap
        .get("nickname")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let email = ap
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    // `preferred_username` 通常是手机号；只在确实是纯数字时才当手机号用。
    let preferred = ap
        .get("preferred_username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let phone = if preferred.len() >= 6 && preferred.chars().all(|c| c.is_ascii_digit()) {
        preferred
    } else {
        String::new()
    };
    // 域名：留空时**按 token 自己的 `iss` 推**，别一律塞 codebuddy.cn。
    // workbuddy.cn 签发的 token 拿去打 codebuddy.cn 虽然眼下也能通，但那是两个域各自的
    // 登录中心（iss 不同），跨域用只是碰巧被接受，出问题时最难查。
    let iss = ap.get("iss").and_then(Value::as_str).unwrap_or("");
    let dom = match domain.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None if iss.contains("workbuddy.cn") => "www.workbuddy.cn".to_string(),
        None => "www.codebuddy.cn".to_string(),
    };

    let mut account = serde_json::json!({
        "uid": uid,
        "nickname": nickname,
        "email": if email.is_empty() { Value::Null } else { Value::String(email) },
        "domain": dom,
        "access_token": access,
        "refresh_token": refresh_token,
        "token_type": "Bearer",
        "expiresAt": exp_ms,
        "source": "manual-token",
        "auth_raw": {
            "accessToken": access,
            "refreshToken": refresh_token,
            "domain": dom,
            "tokenType": "Bearer",
            "expiresIn": (exp_ms - now) / 1000,
            "refreshExpiresIn": refresh_exp_ms.map(|t| (t - now) / 1000),
            "scope": ap.get("scope").cloned().unwrap_or(Value::Null),
            "sessionState": ap.get("session_state").cloned().unwrap_or(Value::Null),
            "tokenSource": if refresh_filled_locally { "manual+local-refresh" } else { "manual" },
        },
        "profile_raw": {
            "uid": uid,
            "nickname": nickname,
            "phoneNumber": phone,
            "type": "",
            "uin": "",
            "lastLogin": true,
            "pluginEnabled": true,
        },
    });
    if let Some(t) = refresh_exp_ms {
        account["refreshExpiresAt"] = serde_json::json!(t);
    }
    if let Some(n) = display_name.map(str::trim).filter(|n| !n.is_empty()) {
        account["display_name"] = serde_json::json!(n);
    }

    upsert(account)
}

/// 删除账号。
///
/// ⚠️ 删完必须**立墓碑**（[`block_identity`]），否则下次启动的自动导入会把它从参考库
/// 原样搬回来 —— 2026-10-09 用户就撞上了「删掉的账号重启后又自己回来了」，删除等于没删。
/// 立碑放在落盘**之后**：账号库都没改成功，就先别急着把人拉黑。
pub fn delete_account(id: &str) -> Result<(), String> {
    let mut accounts = load_accounts();
    let Some(victim) = accounts.iter().find(|a| account_id(a) == id).cloned() else {
        return Err(format!("账号不存在：{id}"));
    };
    accounts.retain(|a| account_id(a) != id);
    save_accounts(&accounts).map_err(|e| e.to_string())?;
    block_identity(&identity_key(&victim));
    Ok(())
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
// 一次性搬家：参考工具账号库里的明文凭据
// ---------------------------------------------------------------------------

/// 参考工具账号库路径。
///
/// ⚠️ **整个仓库里读它只有这一处**，而且只服务于下面那个**用户点一次的搬家动作**——
/// 不是运行期依赖。积分、切换、会话一律只看本工具账号库（T42）。
/// 路径也不进任何用户可见文案（用户明确要求不再出现它）。
fn reference_store_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".wb-switch")
        .join("accounts.json")
}

/// 从参考工具账号库把**明文凭据**搬进本工具账号库。
///
/// 为什么需要它：本工具自己的账号大多来自「导入本机登录态」，凭据是 WorkBuddy 5.6 的
/// 加密信封，**本地解不出明文**（密钥由客户端原生存储模块持有，见 2026-10-09 的记录），
/// 于是这些账号查不了积分。参考工具那边的同一批账号是它自己扫码得到的明文凭据，
/// 搬一次就够，之后两边再无关系。
///
/// ⚠️ **2026-10-09 起由应用启动时自动调用**（`src/main.tsx` 的启动编排），不再有手动按钮。
/// 因此「对方那个文件不存在」是**常态**，必须返回一个可判定的结果而不是 `Err` ——
/// 启动路径上抛错 = 每次开机一条红色提示，而这件事对用户根本不是错误。
///
/// 规则：
/// - **只搬明文**：`access_token` 必须是非空字符串。信封记录搬过来一样查不了，
///   却会把本工具这条记录上的其他字段覆盖掉，所以直接跳过并计入 `skipped`；
/// - 身份按 uid 合并（复用 [`upsert_into`]），命中已有记录时**保留本工具的 `id`、
///   改名与使用时间**，只让凭据与身份字段覆盖过去（见 [`KEEP_LOCAL_FIELDS`]）；
/// - 新 uid 先去掉来源记录的 `id`，免得把别人的 id 占进本工具库。
///
/// 返回 `{ok, available, created, updated, skipped, kept, blocked, imported, total, note}`
/// —— `available=false` 表示对方账号库没读到/读不懂（`note` 里给原因），此时一个字节都没动。
pub fn import_reference_accounts() -> Result<Value, String> {
    let mut accounts = load_accounts();
    let before = accounts.len();
    let blocked = load_import_blocklist();
    let Ok(text) = std::fs::read_to_string(reference_store_path()) else {
        return Ok(unavailable_report(
            before,
            "没有找到参考工具账号库（对方未安装或从未登录过）",
        ));
    };
    let report = match merge_reference_accounts(&mut accounts, &text, &blocked) {
        Ok(r) => r,
        // ⚠️ 对方的文件坏了同样不该在启动时弹错：`accounts` 是本地变量，没落盘就等于没动过。
        Err(e) => return Ok(unavailable_report(before, &format!("参考工具账号库无法使用：{e}"))),
    };
    let moved = report["created"].as_u64().unwrap_or(0) + report["updated"].as_u64().unwrap_or(0);
    if moved > 0 {
        // 积分缓存由 `save_accounts` 顺手作废，这里不用再管。
        save_accounts(&accounts).map_err(|e| e.to_string())?;
    }
    Ok(report)
}

/// 对方账号库不可用时的**正常**返回（不是错误）：一个字节都没动。
fn unavailable_report(total: usize, note: &str) -> Value {
    json!({
        "ok": true,
        "available": false,
        "created": 0,
        "updated": 0,
        "skipped": 0,
        "kept": 0,
        "blocked": 0,
        "imported": 0,
        "total": total,
        "note": note,
    })
}

/// 搬家时**不许被来源覆盖**的本工具字段。
///
/// `id` 是调用方（前端列表、切换、积分缓存）持有的引用；`display_name` 是用户自己改的名；
/// `createdAt` / `lastUsedAt` 是本工具这边的历史，来源那份没有意义。
const KEEP_LOCAL_FIELDS: [&str; 4] = ["id", "display_name", "createdAt", "lastUsedAt"];

/// 从 token 里取 Keycloak 的 `sid`（登录会话 id）；解不出返回空串。
///
/// 用途只有一个：判断一枚 `refresh_token` 能不能配这条 `access_token` 用。
/// 两者必须来自**同一次登录**，否则续期请求会回 `invalid_grant`，然后被上层
/// 误报成「账号在别处重新登录过」——比没有 refresh 更难查。
fn jwt_sid(token: &str) -> String {
    crate::modules::jwt::payload(token)
        .and_then(|p| p.get("sid").and_then(Value::as_str).map(|s| s.trim().to_string()))
        .unwrap_or_default()
}

/// 本地这条记录的凭据**现在还能不能用** —— 启动时自动搬家的「别覆盖」判据。
///
/// ⚠️⚠️ 只看 `access_token` 是否「非空字符串 + 未过期」，**不要求有 refresh_token**。
///
/// 2026-10-24 真 bug：这里原来要求 access + refresh 都在（`has_plain_credentials`），
/// 于是「用户手动粘贴、只填了 access_token」的账号被判成「凭据不完整」，启动时的自动
/// 搬家就拿参考库里那份**更旧的** token 把它整个盖掉 —— 用户每次重启都发现刚加好的
/// 账号又 401，还被告知「refresh token 被作废」（其实它压根没有 refresh token）。
///
/// 加密信封（`access_token` 是对象）与已过期的明文仍然允许被覆盖，那才是搬家本来的目的。
fn local_token_still_good(acc: &Value) -> bool {
    let plain = matches!(acc.get("access_token"), Some(Value::String(s)) if !s.trim().is_empty());
    if !plain {
        return false;
    }
    match acc.get("expiresAt").and_then(Value::as_i64) {
        Some(exp) => exp > now_ms(),
        // 没有到期时间时不要拿「未知」当「失效」——那会把人家刚贴的 token 覆盖掉。
        None => true,
    }
}

/// [`import_reference_accounts`] 的纯函数部分：解析 + 合并，不碰磁盘（单测直接塞 JSON）。
///
/// `blocked` = 用户亲手删过的身份键（[`load_import_blocklist`]）；命中的来源记录**一律跳过**。
fn merge_reference_accounts(
    accounts: &mut Vec<Value>,
    text: &str,
    blocked: &BTreeSet<String>,
) -> Result<Value, String> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|e| format!("参考工具账号库不是合法 JSON：{e}"))?;
    let list = match parsed {
        Value::Array(a) => a,
        Value::Object(o) => o
            .get("accounts")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let (mut created, mut updated, mut skipped, mut kept, mut blocked_hits) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    for record in list {
        let Some(source) = record.as_object() else {
            skipped += 1;
            continue;
        };
        let uid = get_str(&record, "uid").unwrap_or_default();
        let plain_token = matches!(
            record.get("access_token"),
            Some(Value::String(s)) if !s.trim().is_empty()
        );
        if uid.is_empty() || !plain_token {
            skipped += 1;
            continue;
        }

        let key = identity_key(&record);
        // ⚠️ 用户亲手删过这个身份 ⇒ **绝不再自动搬回来**。少了这一条，用户删掉的账号
        // 会在下次启动时原样复活（2026-10-09 真事）——删除等于没删。
        // 只有用户**主动**把它加回来（扫码 / 导入账号包）才撤墓碑，见 `upsert` / `import_accounts`。
        if blocked.contains(&key) {
            blocked_hits += 1;
            continue;
        }
        let existing = accounts.iter().find(|a| identity_key(a) == key).cloned();
        // ⚠️⚠️ 本地这条的 access token **现在还能用**（非空且没过期）时，绝不整条覆盖。
        //
        // 搬家只解决「本地是加密信封 / 没有凭据 / 已过期、查不了」的问题。对已经能查的
        // 账号，本地那份往往是刚刚刷新或**用户刚手填**的，而对方那份可能更旧 ——
        // 无条件覆盖等于把好的换成差的。
        //
        // 判据**不要求有 refresh_token**。2026-10-24 真事：这里原来要求 access + refresh
        // 都在，于是「手动粘贴、只填 access_token」的账号被判成「凭据不完整」，参考库里
        // 那份旧 token 每次启动都把它盖掉，用户看到的就是「重启一次账号就失效」。
        //
        // 但「本地缺 refresh_token」这个洞该补：只把来源的 refresh 塞进去（来源就是同一个
        // uid），access 与其他字段一个都不动。补到 = updated，补不到 = kept。
        if existing.as_ref().is_some_and(local_token_still_good) {
            let missing_refresh = existing
                .as_ref()
                .map(|e| get_str(e, "refresh_token").unwrap_or_default().trim().is_empty())
                .unwrap_or(false);
            let source_refresh = get_str(&record, "refresh_token").unwrap_or_default();
            let local_sid = existing
                .as_ref()
                .map(|e| jwt_sid(&get_str(e, "access_token").unwrap_or_default()))
                .unwrap_or_default();
            let refresh_sid = jwt_sid(source_refresh.trim());
            if missing_refresh
                && !source_refresh.trim().is_empty()
                && !local_sid.is_empty()
                && local_sid == refresh_sid
            {
                if let Some(local) = accounts.iter_mut().find(|a| identity_key(a) == key) {
                    local["refresh_token"] = json!(source_refresh.trim());
                    if let Some(rt) = record.get("refreshExpiresAt") {
                        local["refreshExpiresAt"] = rt.clone();
                    }
                    updated += 1;
                    continue;
                }
            }
            kept += 1;
            continue;
        }
        // 以本工具这条为底，再让来源覆盖凭据与身份字段
        let mut incoming = existing.unwrap_or_else(|| json!({}));
        for (k, v) in source {
            if KEEP_LOCAL_FIELDS.contains(&k.as_str()) {
                continue;
            }
            incoming[k] = v.clone();
        }
        incoming.as_object_mut().unwrap().remove("id");

        let hit = accounts.iter().any(|a| identity_key(a) == key);
        upsert_into(accounts, incoming)?;
        if hit {
            updated += 1;
        } else {
            created += 1;
        }
    }

    Ok(json!({
        "ok": true,
        "available": true,
        "created": created,
        "updated": updated,
        // `skipped` = 来源记录本身不可用（加密信封 / 空 token / 无 uid）；
        // `kept`    = 本地这条已经是可用的明文凭据，**刻意不动**，不是失败；
        // `blocked` = 用户亲手删过的身份，**刻意不搬回来**（这不是失败，是护栏生效）。
        "skipped": skipped,
        "kept": kept,
        "blocked": blocked_hits,
        // 前端只需要这一个数就能决定「要不要提示、要不要刷新」。
        "imported": created + updated,
        "total": accounts.len(),
        "note": Value::Null,
    }))
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
        // ⚠️ 用户主动导入账号包 = 明确要这些账号 ⇒ 撤墓碑，否则自动导入会一直躲着它们
        //（用户会看到「导入成功、但那个账号还是不被搬」）。
        unblock_identity(&key);
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

    /// 测试用的搬家入口：**空墓碑名单**。
    ///
    /// 生产调用点只有一个（`import_reference_accounts`，它自己从磁盘读墓碑）。
    /// 墓碑本身的行为由 `deleted_identity_is_never_imported_again` 单独钉。
    fn merge_ref(accounts: &mut Vec<Value>, text: &str) -> Result<Value, String> {
        merge_reference_accounts(accounts, text, &BTreeSet::new())
    }

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

    // ---- 一次性搬家：参考工具账号库 ----

    #[test]
    fn reference_merge_takes_plain_credentials_and_keeps_local_fields() {
        let mut accs: Vec<Value> = Vec::new();
        upsert_into(
            &mut accs,
            json!({
                "id": "local-id",
                "uid": "u-1",
                "display_name": "我自己改的名",
                "createdAt": 111,
                "lastUsedAt": 222,
                "access_token": {"$wbEncrypted": 1, "envelope": "enc"},
            }),
        )
        .unwrap();

        let report = merge_ref(
            &mut accs,
            &json!([
                {
                    "id": "their-id",
                    "uid": "u-1",
                    "nickname": "来源昵称",
                    "access_token": "PLAIN-ACCESS",
                    "refresh_token": "PLAIN-REFRESH",
                    "domain": "www.codebuddy.cn",
                }
            ])
            .to_string(),
        )
        .unwrap();

        assert_eq!(report["updated"], json!(1));
        assert_eq!(report["created"], json!(0));
        assert_eq!(accs.len(), 1, "同 uid 不许变成两条");
        let a = &accs[0];
        assert_eq!(a["access_token"], json!("PLAIN-ACCESS"), "明文必须顶掉信封");
        assert_eq!(a["refresh_token"], json!("PLAIN-REFRESH"));
        assert_eq!(a["nickname"], json!("来源昵称"), "身份字段应从来源补齐");
        // 本工具这侧的引用与历史不许被来源顶掉
        assert_eq!(a["id"], json!("local-id"), "id 是调用方持有的引用，不能换");
        assert_eq!(a["display_name"], json!("我自己改的名"));
        assert_eq!(a["createdAt"], json!(111));
        assert_eq!(a["lastUsedAt"], json!(222));
    }

    #[test]
    fn reference_merge_skips_envelope_only_and_new_uid_gets_own_id() {
        let mut accs: Vec<Value> = Vec::new();
        let report = merge_ref(
            &mut accs,
            &json!([
                {"uid": "u-envelope", "access_token": {"$wbEncrypted": 1, "envelope": "enc"}},
                {"uid": "u-tokenless", "access_token": "   "},
                {"uid": "", "access_token": "TOKEN"},
                {"uid": "u-new", "id": "their-id", "access_token": "TOKEN-NEW"},
            ])
            .to_string(),
        )
        .unwrap();

        assert_eq!(report["skipped"], json!(3), "信封 / 空 token / 无 uid 都要跳过");
        assert_eq!(report["created"], json!(1));
        assert_eq!(accs.len(), 1);
        assert_eq!(accs[0]["uid"], json!("u-new"));
        assert_eq!(accs[0]["access_token"], json!("TOKEN-NEW"));
        assert_ne!(
            accs[0]["id"], json!("their-id"),
            "新 uid 不能用来源的 id（那是别人库里的号）"
        );
    }

    #[test]
    fn reference_merge_accepts_wrapped_shape_and_reports_created() {
        let mut accs: Vec<Value> = Vec::new();
        let report = merge_ref(
            &mut accs,
            &json!({"accounts": [{"uid": "u-1", "access_token": "T"}]}).to_string(),
        )
        .unwrap();
        assert_eq!(report["created"], json!(1));
        assert_eq!(report["total"], json!(1));
    }

    /// 本地这条**已经是可用的明文凭据**时，来源那份不许顶掉它。
    ///
    /// 2026-10-09 实测（同一个 uid）：本工具库那份 `exp` 到 11-11，参考库那份到 11-06 ——
    /// 无条件覆盖等于把刚刷新出来的 token 换成更旧的。搬家只为解决「本地是信封查不了」。
    #[test]
    fn reference_merge_never_downgrades_a_locally_usable_account() {
        let mut accs: Vec<Value> = Vec::new();
        upsert_into(
            &mut accs,
            json!({
                "id": "local-id",
                "uid": "u-1",
                "access_token": "LOCAL-ACCESS",
                "refresh_token": "LOCAL-REFRESH",
            }),
        )
        .unwrap();

        let report = merge_ref(
            &mut accs,
            &json!([{
                "uid": "u-1",
                "access_token": "THEIR-OLDER-ACCESS",
                "refresh_token": "THEIR-OLDER-REFRESH",
            }])
            .to_string(),
        )
        .unwrap();

        assert_eq!(report["updated"], json!(0), "不该更新");
        assert_eq!(report["kept"], json!(1), "本地已可用 ⇒ 计入保留");
        assert_eq!(report["skipped"], json!(0));
        assert_eq!(accs[0]["access_token"], json!("LOCAL-ACCESS"), "新鲜的不能被换走");
        assert_eq!(accs[0]["refresh_token"], json!("LOCAL-REFRESH"));
    }

    /// 边界：本地只有半个凭据（有 access 没 refresh）时，仍要把**同一个登录会话**的
    /// refresh 补进来 —— 否则「本地刷新能力都丢了」的账号会被永久卡死。
    ///
    /// ⚠️ 必须同 `sid`：随手塞一枚别的 session 的 refresh 只会换来 `invalid_grant`，
    /// 然后被上层误报成「账号在别处重新登录过」。
    #[test]
    fn reference_merge_still_fills_in_incomplete_local_credentials() {
        let access = fake_jwt("u-1", "sid-1");
        let refresh = fake_jwt("u-1", "sid-1");

        let mut accs: Vec<Value> = Vec::new();
        upsert_into(
            &mut accs,
            json!({"id": "local-id", "uid": "u-1", "access_token": access}),
        )
        .unwrap();

        let report = merge_ref(
            &mut accs,
            &json!([{
                "uid": "u-1",
                "access_token": fake_jwt("u-1", "sid-1"),
                "refresh_token": refresh,
            }])
            .to_string(),
        )
        .unwrap();

        assert_eq!(report["updated"], json!(1));
        assert_eq!(report["skipped"], json!(0));
        assert_eq!(accs[0]["refresh_token"], json!(refresh), "补齐刷新凭据");
        assert_eq!(accs[0]["id"], json!("local-id"));
        assert_eq!(
            accs[0]["access_token"], json!(access),
            "只补 refresh，本地这枚 access 不许被换"
        );
    }

    /// 构造一枚只有 payload 有意义的假 JWT（`jwt::payload` 不验签）。
    fn fake_jwt(sub: &str, sid: &str) -> String {
        use base64::Engine;
        let payload = json!({"sub": sub, "sid": sid, "exp": (now_ms() / 1000) + 3600}).to_string();
        let seg = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("eyJhbGciOiJub25lIn0.{seg}.sig")
    }

    #[test]
    fn reference_merge_rejects_broken_json() {
        let mut accs: Vec<Value> = Vec::new();
        assert!(merge_ref(&mut accs, "{ 不是 JSON").is_err());
    }

    /// 自动导入（T43）**幂等**：这个动作现在每次启动都会跑，所以第二趟必须什么都不改。
    ///
    /// 第一趟把本地那条从信封换成明文；第二趟本地已是明文 ⇒ 落进 `kept`，不重写凭据、
    /// 不换 `id`。「启动时自动做」能成立的前提就是它重复跑没有副作用。
    #[test]
    fn reference_merge_is_idempotent_because_it_runs_on_every_startup() {
        let mut accs: Vec<Value> = Vec::new();
        upsert_into(
            &mut accs,
            json!({"id": "local-id", "uid": "u-1", "access_token": {"$wbEncrypted": 1}}),
        )
        .unwrap();
        let src = json!([{
            "uid": "u-1",
            "access_token": "THEIR-ACCESS",
            "refresh_token": "THEIR-REFRESH",
        }])
        .to_string();

        let first = merge_ref(&mut accs, &src).unwrap();
        assert_eq!(first["available"], json!(true), "读到了就要标明可搬");
        assert_eq!(first["imported"], json!(1), "第一趟要把明文搬进来");
        assert_eq!(accs[0]["access_token"], json!("THEIR-ACCESS"));

        let second = merge_ref(&mut accs, &src).unwrap();
        assert_eq!(second["imported"], json!(0), "第二趟不该再有变动");
        assert_eq!(second["kept"], json!(1));
        assert_eq!(accs[0]["access_token"], json!("THEIR-ACCESS"), "凭据原样");
        assert_eq!(accs[0]["id"], json!("local-id"), "id 也不能被换掉");
    }

    /// ⚠️ **用户亲手删掉的账号，不许被启动时的自动导入搬回来**。
    ///
    /// 2026-10-09 真事：用户删了一个账号，重启后它自己从参考库回来了 —— 删除等于没删。
    /// 这是 T43「启动时自动导入」最容易漏的一条：幂等只保证「本地已是明文就不覆盖」，
    /// 完全管不住「这个身份用户根本不想要」。护栏就是墓碑名单（`merge_ref` 之外的第三个参数）。
    #[test]
    fn deleted_identity_is_never_imported_again() {
        let mut accs: Vec<Value> = Vec::new();
        let src = json!([
            {"uid": "u-deleted", "access_token": "THEIR-ACCESS", "refresh_token": "THEIR-REFRESH"},
            {"uid": "u-kept", "access_token": "KEPT-ACCESS", "refresh_token": "KEPT-REFRESH"},
        ])
        .to_string();

        // 用户删过 u-deleted ⇒ 名单里有它的身份键（与 `identity_key` 同格式）
        let mut blocked = BTreeSet::new();
        blocked.insert("uid:u-deleted".to_string());

        let r = merge_reference_accounts(&mut accs, &src, &blocked).unwrap();
        assert_eq!(r["blocked"], json!(1), "被删过的身份要计入 blocked（护栏生效，不是失败）：{r}");
        assert_eq!(r["imported"], json!(1), "没被删过的照常搬");
        assert_eq!(accs.len(), 1);
        assert_eq!(accs[0]["uid"], json!("u-kept"));
        assert!(
            !accs.iter().any(|a| a["uid"] == json!("u-deleted")),
            "删掉的账号不许复活"
        );
    }

    /// ⚠️⚠️ 手动粘贴的账号（只有 `access_token`、没有 `refresh_token`）**不许被启动时的
    /// 自动搬家覆盖**。
    ///
    /// 2026-10-24 真事：用户手填 token 后账号能查积分，重启一次就 401，还被告知
    /// 「refresh token 被作废」—— 它根本没有 refresh token。根因是「别覆盖」的判据
    /// 要求 access + refresh 都在，只有 access 的账号被判成「凭据不完整」，于是参考库
    /// 里那份更旧的 token 被搬进来，把刚贴的这枚盖掉了。
    #[test]
    fn manual_access_only_account_is_not_overwritten_by_reference_store() {
        let mut accs = vec![json!({
            "id": "manual-id",
            "uid": "u-manual",
            "access_token": "FRESH-ACCESS",
            "refresh_token": "",
            "expiresAt": now_ms() + 86_400_000,
            "source": "manual-token",
        })];
        let src = json!([{
            "uid": "u-manual",
            "access_token": "STALE-ACCESS",
            "refresh_token": "STALE-REFRESH",
        }])
        .to_string();

        let r = merge_ref(&mut accs, &src).unwrap();
        assert_eq!(
            accs[0]["access_token"],
            json!("FRESH-ACCESS"),
            "刚贴的 access_token 绝不许被旧 token 盖掉（updated={}, kept={}）",
            r["updated"],
            r["kept"],
        );
        assert_eq!(accs[0]["id"], json!("manual-id"), "id 不能被换掉");
        assert_eq!(accs[0]["uid"], json!("u-manual"));
    }

    /// 本地明文**已过期** ⇒ 允许被来源覆盖（搬家本来的目的仍然是「救回查不了的账号」）。
    #[test]
    fn expired_local_token_may_be_replaced() {
        let mut accs = vec![json!({
            "id": "manual-id",
            "uid": "u-exp",
            "access_token": "EXPIRED",
            "refresh_token": "",
            "expiresAt": 1,
        })];
        let src = json!([{"uid": "u-exp", "access_token": "NEWER", "refresh_token": "R"}]).to_string();
        let r = merge_ref(&mut accs, &src).unwrap();
        assert_eq!(r["updated"], json!(1), "{r}");
        assert_eq!(accs[0]["access_token"], json!("NEWER"));
    }

    /// 墓碑名单的读写往返：写进去的要能读出来，**坏 JSON 当空名单**（护栏坏了不能让账号页打不开）。
    #[test]
    fn blocklist_roundtrips_and_broken_json_is_an_empty_list() {
        let path = std::env::temp_dir().join(format!("wb-blocklist-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        block_identity_at(&path, "uid:u-1");
        block_identity_at(&path, "uid:u-2");
        assert_eq!(read_blocklist_at(&path), {
            let mut s = BTreeSet::new();
            s.insert("uid:u-1".to_string());
            s.insert("uid:u-2".to_string());
            s
        });
        // 重复记同一个身份不产生重复项
        block_identity_at(&path, "uid:u-1");
        assert_eq!(read_blocklist_at(&path).len(), 2);
        // 空键忽略（无身份的记录不该污染名单）
        block_identity_at(&path, "");
        assert_eq!(read_blocklist_at(&path).len(), 2);
        // 用户主动加回来 ⇒ 撤墓碑
        unblock_identity_at(&path, "uid:u-1");
        assert!(!read_blocklist_at(&path).contains("uid:u-1"));

        std::fs::write(&path, "{ 不是 JSON").unwrap();
        assert!(read_blocklist_at(&path).is_empty(), "坏文件当空名单");
        let _ = std::fs::remove_file(&path);
        assert!(read_blocklist_at(&path).is_empty(), "文件不存在同样当空名单");
    }

    /// 「对方账号库不存在」是**常态**（正常用户根本没装过那个工具），必须返回一个可判定的
    /// 结果而不是 `Err` —— 这条路径跑在启动编排上，抛错就变成每次开机一条红色提示。
    #[test]
    fn unavailable_result_is_a_normal_outcome_not_an_error() {
        let r = unavailable_report(2, "没有找到参考工具账号库（对方未安装或从未登录过）");
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["available"], json!(false), "要能区分「没得搬」和「搬了 0 个」");
        assert_eq!(r["imported"], json!(0));
        assert_eq!(r["blocked"], json!(0), "这条路径上不该有护栏命中");
        assert_eq!(r["total"], json!(2), "总数仍是本工具账号库自己的");
        assert!(r["note"].as_str().unwrap().contains("没有找到"));
    }
}

#[cfg(test)]
mod manual_token_tests {
    use super::*;
    use base64::Engine;

    /// 造一个**只解 payload、不验签**的假 JWT（校验逻辑只看 payload）。
    fn fake_jwt(payload: Value) -> String {
        let enc = |v: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
        };
        format!(
            "{}.{}.{}",
            enc(&serde_json::json!({"alg": "none", "typ": "JWT"})),
            enc(&payload),
            "sig"
        )
    }

    fn now_s() -> i64 {
        chrono::Utc::now().timestamp()
    }

    #[test]
    fn rejects_incomplete_and_expired() {
        // 不是三段
        assert!(add_manual_token("abc", None, None, None).is_err());
        // 已过期
        let expired = fake_jwt(serde_json::json!({"sub": "u1", "exp": now_s() - 60}));
        let err = add_manual_token(&expired, None, None, None).unwrap_err();
        assert!(err.contains("过期"), "实际：{err}");
        // 没有 sub
        let no_sub = fake_jwt(serde_json::json!({"exp": now_s() + 3600}));
        assert!(add_manual_token(&no_sub, None, None, None).is_err());
    }

    #[test]
    fn rejects_mismatched_refresh() {
        let a = fake_jwt(serde_json::json!({"sub": "ua", "exp": now_s() + 3600}));
        let r = fake_jwt(serde_json::json!({"sub": "ub", "exp": now_s() + 7200}));
        let err = add_manual_token(&a, Some(&r), None, None).unwrap_err();
        assert!(err.contains("另一个账号"), "实际：{err}");
    }

    /// 真机自检：造一个假账号走完整流程（添加 → 重复添加应 upsert → 删除），
    /// 全程不碰任何真实账号。
    #[test]
    #[ignore]
    fn probe_add_manual_token_roundtrip() {
        let uid = format!("twinswitch-manual-probe-{}", uuid::Uuid::new_v4());
        let access = fake_jwt(serde_json::json!({
            "sub": uid, "exp": now_s() + 3600,
            "nickname": "手动测试", "preferred_username": "13800000000"
        }));
        let refresh = fake_jwt(serde_json::json!({"sub": uid, "exp": now_s() + 86400}));

        let saved = add_manual_token(&access, Some(&refresh), Some("www.codebuddy.cn"), Some("自检"))
            .expect("添加应成功");
        eprintln!(
            "[probe] 已添加 uid={:?} nickname={:?} expiresAt={:?} refreshExpiresAt={:?}",
            saved.get("uid"),
            saved.get("nickname"),
            saved.get("expiresAt"),
            saved.get("refreshExpiresAt")
        );

        let before = load_accounts().len();
        let _ = add_manual_token(&access, Some(&refresh), None, None).expect("重复添加应更新");
        let after = load_accounts().len();
        eprintln!("[probe] 重复添加前后账号数：{before} → {after}（应相等）");

        delete_account(&account_id(&saved)).expect("清理失败");
        eprintln!("[probe] 已清理（账号数 {}）", load_accounts().len());
    }
}
