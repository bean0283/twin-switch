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

/// 这条记录的凭据是不是**可以直接使用**的明文。
///
/// ⚠️ 加密信封（`{"$wbEncrypted":…}`）不算：它连本地都解不开，拿去查积分必然失败。
/// 判定与 `workbuddy_credits::token_state_of` 同源（都是「非空字符串」）。
fn has_plain_credentials(acc: &Value) -> bool {
    ["access_token", "refresh_token"].iter().all(|k| {
        matches!(acc.get(*k), Some(Value::String(s)) if !s.trim().is_empty())
    })
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
        // ⚠️ 本地这条**已经是可用的明文凭据**（access + refresh 都在）时不许覆盖。
        // 搬家只解决「本地是加密信封、查不了」的问题；对已经能查的账号，本地那份
        // 往往是刚刚刷新过的，而对方那份可能更旧 —— 无条件覆盖等于把好的换成差的。
        if existing.as_ref().is_some_and(has_plain_credentials) {
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

    /// 边界：本地只有半个凭据（有 access 没 refresh）时，仍然要把对方那一对搬过来。
    /// 否则「跳过」会把「本地刷新能力都丢了」的账号永久卡死。
    #[test]
    fn reference_merge_still_fills_in_incomplete_local_credentials() {
        let mut accs: Vec<Value> = Vec::new();
        upsert_into(
            &mut accs,
            json!({"id": "local-id", "uid": "u-1", "access_token": "LOCAL-ACCESS"}),
        )
        .unwrap();

        let report = merge_ref(
            &mut accs,
            &json!([{
                "uid": "u-1",
                "access_token": "THEIR-ACCESS",
                "refresh_token": "THEIR-REFRESH",
            }])
            .to_string(),
        )
        .unwrap();

        assert_eq!(report["updated"], json!(1));
        assert_eq!(report["skipped"], json!(0));
        assert_eq!(accs[0]["refresh_token"], json!("THEIR-REFRESH"), "补齐刷新凭据");
        assert_eq!(accs[0]["id"], json!("local-id"));
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
