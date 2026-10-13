//! 账号库（vault）：按账号把「载体」原样字节备份 / 还原，并做哈希校验、归属识别、导入导出
//! （移植自 trae-switch src/vault.js）。
//!
//! 只做字节拷贝，不解密、不改动内容；导入/导出走自包含 JSON（文件内容 base64 内联）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::modules::config::store_dir;
use crate::modules::trae_carriers::{
    expand_entries, hash_file, overwrite_entry, safe_rel, sanitize_entries, CarrierFile,
};
use crate::modules::trae_discover::TraeClient;
use crate::modules::trae_km::{decrypt_km_json, is_km_value};

pub const KEY_AUTH: &str = "iCubeAuthInfo://icube.cloudide";
pub const REL_STORAGE: &str = "User/globalStorage/storage.json";

/// 把名字清洗成安全目录名。
pub fn sanitize_name(name: &str) -> String {
    let t: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|\r\n\t".contains(c) { '_' } else { c })
        .collect();
    let t = t.trim().to_string();
    if t.is_empty() {
        "account".into()
    } else {
        t
    }
}

/// vault 根目录：`<工具目录>/trae/vault/<clientKey>/<accountId>/`。
pub fn vault_root() -> PathBuf {
    store_dir().join("trae").join("vault")
}

pub fn account_dir(client_key: &str, id: &str) -> PathBuf {
    vault_root().join(client_key).join(sanitize_name(id))
}

pub fn meta_path(client_key: &str, id: &str) -> PathBuf {
    account_dir(client_key, id).join("meta.json")
}

/// 从 storage.json 文本里解出账号 uid（未登录 / 非 Km 条目 → None）。
pub fn uid_from_storage_text(text: &str) -> Option<String> {
    let root = serde_json::from_str::<Value>(text).ok()?;
    let raw = root.get(KEY_AUTH)?;
    let raw_str = raw.as_str()?;
    if !is_km_value(raw_str) {
        return None;
    }
    let auth = decrypt_km_json(raw_str)?;
    match auth.get("userId") {
        Some(v) => v.as_i64().map(|i| i.to_string()).or_else(|| v.as_str().map(String::from)),
        None => None,
    }
}

fn uid_from_storage_file(p: &Path) -> Option<String> {
    uid_from_storage_text(&std::fs::read_to_string(p).ok()?)
}

/// 档案里该账号的 uid：优先 storage.json（客户端真正认的），其次 oauth.json。
pub fn uid_of_account(client_key: &str, id: &str) -> Option<String> {
    let dir = account_dir(client_key, id);
    if let Some(uid) = uid_from_storage_file(&dir.join(REL_STORAGE)) {
        return Some(uid);
    }
    let oauth = dir.join("oauth.json");
    if let Ok(text) = std::fs::read_to_string(&oauth) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(uid) = v.get("uid") {
                if let Some(s) = uid.as_str() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// 从载体 storage.json 解密登录态取 Trae 客户端显示的真实用户名（如「用户1181093986」）。
fn username_from_storage(dir: &Path) -> Option<String> {
    let storage_text = std::fs::read_to_string(dir.join(REL_STORAGE)).ok()?;
    let root: Value = serde_json::from_str(&storage_text).ok()?;
    let raw = root.get(KEY_AUTH)?.as_str()?;
    if !is_km_value(raw) {
        return None;
    }
    let auth = decrypt_km_json(raw)?;
    auth.get("account")?
        .get("username")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 读取缓存的账号资料（GetUserInfo 昵称 + 积分余额，由 trae_profile::refresh_profile 写入）。
pub fn read_profile(client_key: &str, id: &str) -> Option<Value> {
    let text = std::fs::read_to_string(account_dir(client_key, id).join("profile.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 账号真实显示名：优先接口拉取并缓存的真实昵称（GetUserInfo ScreenName，与 Trae
/// 界面一致），其次 Trae 客户端登录态里的真实用户名（storage.json 的 account.username），
/// 再其次 oauth.json 的 userName / displayName。取不到返回 None。
pub fn display_name(client_key: &str, id: &str) -> Option<String> {
    if let Some(p) = read_profile(client_key, id) {
        if let Some(s) = p
            .get("screen_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
    }
    let dir = account_dir(client_key, id);
    if let Some(u) = username_from_storage(&dir) {
        return Some(u);
    }
    let oauth = dir.join("oauth.json");
    if let Ok(text) = std::fs::read_to_string(&oauth) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            for k in ["userName", "displayName"] {
                if let Some(s) = v
                    .get(k)
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// 列出某客户端下已有的账号目录。
pub fn list_vault_accounts(client_key: &str) -> Vec<String> {
    let dir = vault_root().join(client_key);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.metadata().map(|m| m.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

fn uid_suffix(uid: &str) -> String {
    uid[uid.len().saturating_sub(6)..].to_string()
}

/// 账号显示名：优先真实用户名（storage.json / oauth.json，与 display_name 同优先级），
/// 其次 vault 账号目录名，兜底仅 uid 尾号。找不到档案时同样兜底 uid 尾号。
pub fn account_label(client_key: &str, uid: &str) -> String {
    for id in list_vault_accounts(client_key) {
        if uid_of_account(client_key, &id).as_deref() == Some(uid) {
            let label = display_name(client_key, &id).unwrap_or_else(|| id.clone());
            return format!("{label}（uid …{}）", uid_suffix(uid));
        }
    }
    format!("uid …{}", uid_suffix(uid))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultMeta {
    pub id: String,
    pub client: String,
    /// 账号类型：`carrier`（客户端登录态载体，可切换）/ `oauth`（网页登录凭证，仅可查询）。
    /// 历史账号无此字段，反序列化按 carrier 处理。
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub root_dir: String,
    #[serde(default)]
    pub entries: Vec<String>,
    #[serde(default)]
    pub files: Vec<CarrierFile>,
    #[serde(default)]
    pub file_count: usize,
    #[serde(default)]
    pub total_bytes: u64,
    pub created_at: String,
    /// 最近一次切换成功时间（RFC3339）。
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// 最近一次切换成功时客户端确认的账号 uid。
    #[serde(default)]
    pub verified_uid: Option<String>,
}

/// 切换成功后更新账号元数据（last_used_at / verified_uid）。
pub fn mark_used(client_key: &str, id: &str, verified_uid: Option<String>) {
    let Some(mut meta) = read_meta(client_key, id) else {
        return;
    };
    meta.last_used_at = Some(chrono::Local::now().to_rfc3339());
    meta.verified_uid = verified_uid;
    let path = meta_path(client_key, id);
    let _ = std::fs::write(path, serde_json::to_string_pretty(&meta).unwrap_or_default());
}

pub fn read_meta(client_key: &str, id: &str) -> Option<VaultMeta> {
    let text = std::fs::read_to_string(meta_path(client_key, id)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 建档：把 rootDir 下 entries 指向的载体复制进 vault。
pub fn backup(client_key: &str, id: &str, root_dir: &Path, entries: &[String]) -> Result<VaultMeta, String> {
    let dst_root = account_dir(client_key, id);
    std::fs::create_dir_all(&dst_root).map_err(|e| format!("创建账号目录失败: {e}"))?;
    let list = sanitize_entries(entries);
    for rel in &list {
        overwrite_entry(root_dir, &dst_root, rel);
    }
    let mut files = Vec::new();
    for rel in expand_entries(root_dir, &list) {
        let p = dst_root.join(rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str()));
        if let Ok(meta) = std::fs::metadata(&p) {
            files.push(CarrierFile {
                rel: rel.clone(),
                len: meta.len() as i64,
                sha256: hash_file(&p).unwrap_or_default(),
            });
        }
    }
    let total: u64 = files.iter().map(|f| f.len.max(0) as u64).sum();
    let meta = VaultMeta {
        id: id.to_string(),
        client: client_key.to_string(),
        kind: "carrier".into(),
        root_dir: root_dir.to_string_lossy().into_owned(),
        entries: list,
        file_count: files.len(),
        files,
        total_bytes: total,
        created_at: chrono::Local::now().to_rfc3339(),
        last_used_at: None,
        verified_uid: None,
    };
    let path = meta_path(client_key, id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写 meta 失败: {e}"))?;
    Ok(meta)
}

/// 还原：把 vault 里的载体写回 rootDir。
pub fn restore(client_key: &str, id: &str, root_dir: &Path, entries: &[String]) -> Result<usize, String> {
    let src_root = account_dir(client_key, id);
    if !src_root.exists() {
        return Err(format!("账号 {id} 未建档"));
    }
    let meta_entries = read_meta(client_key, id).map(|m| m.entries).unwrap_or_default();
    let list = sanitize_entries(if !entries.is_empty() { entries } else { &meta_entries });
    for rel in &list {
        overwrite_entry(&src_root, root_dir, rel);
    }
    Ok(list.len())
}

/// 校验：vault 与 live 逐文件哈希一致。
pub fn verify(client_key: &str, id: &str, root_dir: &Path) -> Result<Value, String> {
    let Some(meta) = read_meta(client_key, id) else {
        return Err("未建档".into());
    };
    let mut checked = 0usize;
    let mut mismatched = Vec::new();
    for f in &meta.files {
        let v = account_dir(client_key, id).join(f.rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str()));
        let l = root_dir.join(f.rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str()));
        if !v.is_file() || !l.is_file() {
            continue;
        }
        checked += 1;
        if hash_file(&v) != hash_file(&l) {
            mismatched.push(f.rel.clone());
        }
    }
    Ok(json!({
        "ok": mismatched.is_empty(),
        "checked": checked,
        "mismatched": mismatched.iter().take(20).collect::<Vec<_>>(),
    }))
}

/// 归属识别：当前 live 更像哪个账号。首选 uid 比对（解密 storage.json），
/// uid 解不出时退回哈希相似度。
pub fn identify(client_key: &str, root_dir: &Path) -> Option<Value> {
    let ids = list_vault_accounts(client_key);
    if ids.is_empty() {
        return None;
    }
    let live_uid = uid_from_storage_file(&root_dir.join(REL_STORAGE));
    if let Some(live_uid) = live_uid {
        let mut rows: Vec<Value> = ids
            .iter()
            .filter_map(|id| {
                let uid = uid_of_account(client_key, id)?;
                let hit = uid == live_uid;
                Some(json!({ "id": id, "score": if hit { 1 } else { 0 }, "hit": hit as i32, "total": 1, "uid": uid }))
            })
            .collect();
        rows.sort_by(|a, b| b["score"].as_i64().unwrap_or(0).cmp(&a["score"].as_i64().unwrap_or(0)));
        if !rows.is_empty() {
            return Some(json!({ "best": rows[0], "all": rows, "by": "uid", "live_uid": live_uid }));
        }
    }
    // 兜底：哈希相似度（简化为按判别文件命中率）
    let usable: Vec<VaultMeta> = ids.iter().filter_map(|id| read_meta(client_key, id)).collect();
    if usable.is_empty() {
        return None;
    }
    let discriminant = build_discriminant(&usable);
    let mut scored: Vec<(String, f64, usize, usize)> = Vec::new();
    for m in &usable {
        let mut total = 0usize;
        let mut hit = 0usize;
        for f in &m.files {
            let rel = f.rel.clone();
            if is_runtime_state_file(&rel) || (discriminant.is_some() && !discriminant.as_ref().unwrap().contains(&rel)) {
                continue;
            }
            total += 1;
            let live = root_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str()));
            if hash_file(&live).as_deref() == Some(f.sha256.as_str()) {
                hit += 1;
            }
        }
        if total > 0 {
            scored.push((m.id.clone(), hit as f64 / total as f64, hit, total));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if scored.is_empty() {
        return None;
    }
    let (id, score, hit, total) = &scored[0];
    Some(json!({
        "best": { "id": id, "score": score, "hit": hit, "total": total },
        "all": scored.iter().map(|(id, score, hit, total)| json!({ "id": id, "score": score, "hit": hit, "total": total })).collect::<Vec<_>>(),
        "by": "hash",
    }))
}

fn is_runtime_state_file(rel: &str) -> bool {
    let t = rel.to_lowercase();
    matches!(t.as_str(), "local storage/config.db" | "network/network persistent state" | "user/globalstorage/storage.json")
}

/// 判别文件集合：跨账号哈希互不相同，或仅部分账号拥有。
fn build_discriminant(metas: &[VaultMeta]) -> Option<HashSetString> {
    let mut by_rel: HashMap<String, (HashSet<String>, usize)> = HashMap::new();
    let n = metas.len();
    for m in metas {
        let mut seen = HashSet::new();
        for f in &m.files {
            let rel = f.rel.clone();
            if is_runtime_state_file(&rel) || seen.contains(&rel) {
                continue;
            }
            seen.insert(rel.clone());
            let e = by_rel.entry(rel).or_insert_with(|| (HashSet::new(), 0));
            e.0.insert(f.sha256.clone());
        }
    }
    for m in metas {
        let mut seen = HashSet::new();
        for f in &m.files {
            let rel = f.rel.clone();
            if is_runtime_state_file(&rel) || seen.contains(&rel) {
                continue;
            }
            seen.insert(rel.clone());
            if let Some(e) = by_rel.get_mut(&rel) {
                e.1 += 1;
            }
        }
    }
    let mut set = HashSet::new();
    for (rel, (shas, owners)) in by_rel {
        if shas.len() > 1 || owners < n {
            set.insert(rel);
        }
    }
    if set.is_empty() {
        None
    } else {
        Some(HashSetString(set))
    }
}

/// 判别文件集合的包装（内部 HashSet<String>）。
pub struct HashSetString(pub HashSet<String>);

impl HashSetString {
    pub fn contains(&self, s: &str) -> bool {
        self.0.contains(s)
    }
}

/// 从存储文本里取 uid 的便捷入口（供外部调用）。
pub fn uid_of_live_storage(text: &str) -> Option<String> {
    uid_from_storage_text(text)
}

pub fn remove_account(client_key: &str, id: &str) -> Result<(), String> {
    let dir = account_dir(client_key, id);
    if !dir.exists() {
        return Err("账号不存在".into());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("删除账号失败: {e}"))
}

pub fn rename_account(client_key: &str, from_id: &str, to_id: &str) -> Result<String, String> {
    let from = account_dir(client_key, from_id);
    let to = account_dir(client_key, to_id);
    if !from.exists() {
        return Err("账号不存在".into());
    }
    if to.exists() {
        return Err(format!("目标名 {to_id} 已存在"));
    }
    std::fs::rename(&from, &to).map_err(|e| format!("重命名失败: {e}"))?;
    if let Some(mut meta) = read_meta(client_key, to_id) {
        meta.id = to_id.to_string();
        let path = meta_path(client_key, to_id);
        std::fs::write(path, serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?)
            .map_err(|e| format!("更新 meta 失败: {e}"))?;
    }
    Ok(to_id.to_string())
}

const EXPORT_FORMAT: &str = "trae-switch-vault";
/// 1 = 只带载体文件（`meta.files`，即 `entries` 指到的那些）；2 = 带账号目录里的**全部**文件 + 原始 meta。
const EXPORT_VERSION: i64 = 2;

/// 账号目录里**除 `meta.json` 之外**的全部文件（递归），相对路径统一用 `/`。
///
/// ⚠️ 为什么不能只搬 `meta.files`：那只列了「载体」文件（`entries` 指到的那些）。
/// 账号目录里还有两类**侧车**文件，丢掉它们账号就残废：
///   - `oauth.json` —— 网页凭证（查积分、续期 token 都要它）；
///   - `profile.json` —— 上一次拉到的资料 / 积分缓存。
/// 只搬载体时的表现（真机踩过）：并入后的账号名变成原始目录名、身份行显示 `oauth_717036`、
/// 大小 `0 B`、且永远查不了积分 —— 功能其实还能用（能切号），但看上去像坏了。
fn collect_account_files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, cur: &Path, depth: usize, out: &mut Vec<String>) {
        if depth > 8 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(cur) else {
            return;
        };
        let mut items: Vec<_> = rd.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                walk(base, &p, depth + 1, out);
            } else if md.is_file() {
                if let Ok(rel) = p.strip_prefix(base) {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    // meta.json 由 `meta` 字段单独承载（导入时要改 id / client），不重复进文件清单
                    if rel != "meta.json" {
                        if let Some(r) = safe_rel(&rel) {
                            out.push(r);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, 0, &mut out);
    out
}

/// 读取 meta.json 的**原始** JSON（`VaultMeta` 结构体存不下的字段也要带走：
/// `display_name` / `carrier` / `note` …）。读不出来返回 `Null`。
fn read_raw_meta(client_key: &str, id: &str) -> Value {
    std::fs::read_to_string(meta_path(client_key, id))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or(Value::Null)
}

/// 导出为单个 JSON 文本（文件内容 base64 内联）。
pub fn export_account(client_key: &str, id: &str) -> Result<Value, String> {
    let dir = account_dir(client_key, id);
    if !dir.is_dir() {
        return Err("账号不存在，无法导出".into());
    }
    let meta = read_meta(client_key, id).ok_or("账号未建档，无法导出")?;
    let mut files = Vec::new();
    for rel in collect_account_files(&dir) {
        let p = dir.join(rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str()));
        let Ok(data) = std::fs::read(&p) else {
            continue;
        };
        files.push(json!({
            "rel": rel,
            "len": data.len(),
            "sha256": hash_file(&p).unwrap_or_default(),
            "b64": STANDARD.encode(&data),
        }));
    }
    Ok(json!({
        "format": EXPORT_FORMAT,
        "version": EXPORT_VERSION,
        "exported_at": chrono::Local::now().to_rfc3339(),
        "client": client_key,
        "id": id,
        "kind": meta.kind,
        "entries": sanitize_entries(&meta.entries),
        // 原始 meta：导入时原样恢复（只改 id / client、清空 last_used_at / verified_uid）
        "meta": read_raw_meta(client_key, id),
        "files": files,
    }))
}

/// 从导出 JSON 恢复到 vault；冲突时自动加后缀。返回新账号 id。
pub fn import_account(client_key: &str, payload: &Value, prefer_name: Option<&str>) -> Result<(String, usize), String> {
    if payload.get("format").and_then(|v| v.as_str()) != Some(EXPORT_FORMAT) {
        return Err("不是 trae-switch 导出的账号文件".into());
    }
    let base = sanitize_name(prefer_name.unwrap_or(payload.get("id").and_then(|v| v.as_str()).unwrap_or("imported")));
    let mut id = base.clone();
    let mut n = 1usize;
    while account_dir(client_key, &id).exists() {
        id = format!("{base}_{}", n + 1);
        n += 1;
    }
    let dir = account_dir(client_key, &id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建账号目录失败: {e}"))?;

    // 1) 原样写回文件（v2 含 oauth.json / profile.json 等侧车；v1 只有载体文件，仍可导入）
    let mut all_files: Vec<(String, i64, String)> = Vec::new();
    for f in payload.get("files").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let Some(rel) = f.get("rel").and_then(|v| v.as_str()).and_then(safe_rel) else {
            continue;
        };
        let Some(b64) = f.get("b64").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(data) = STANDARD.decode(b64) else {
            continue;
        };
        let p = dir.join(rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str()));
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let len = data.len() as i64;
        if std::fs::write(&p, data).is_err() {
            continue;
        }
        all_files.push((rel, len, hash_file(&p).unwrap_or_default()));
    }

    let entries = sanitize_entries(
        &payload
            .get("entries")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(String::from).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    // 2) 「载体文件」的账只算 `entries` 里那些（侧车不是载体：restore() 不会把它们写进客户端）
    let files: Vec<CarrierFile> = all_files
        .iter()
        .filter(|(rel, _, _)| entries.iter().any(|e| e == rel))
        .map(|(rel, len, sha)| CarrierFile { rel: rel.clone(), len: *len, sha256: sha.clone() })
        .collect();
    let total: u64 = files.iter().map(|f| f.len.max(0) as u64).sum();
    let file_count = files.len();

    // 3) meta：优先**原样**恢复导出时的 meta（保住 kind / display_name / carrier / created_at…）；
    //    v1 老备份没有 meta ⇒ 退回按字段拼一份（行为与修复前一致）。
    let mut meta_value = payload
        .get("meta")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({ "kind": payload.get("kind").and_then(|v| v.as_str()).unwrap_or("carrier") }));
    if let Some(obj) = meta_value.as_object_mut() {
        obj.insert("id".into(), json!(id));
        obj.insert("client".into(), json!(client_key));
        // 刚进本库的账号**还没在本客户端切过** ⇒ 这两项是「本机事实」，必须清空。
        // 留着源客户端的值会让界面把它算成「当前登录」（`isLive` 就是比 `verified_uid`）。
        obj.insert("last_used_at".into(), Value::Null);
        obj.insert("verified_uid".into(), Value::Null);
        obj.entry("kind").or_insert_with(|| json!("carrier"));
        obj.insert("entries".into(), json!(entries));
        obj.insert("files".into(), json!(files));
        obj.insert("file_count".into(), json!(file_count));
        obj.insert("total_bytes".into(), json!(total));
        obj.entry("created_at").or_insert_with(|| json!(chrono::Local::now().to_rfc3339()));
    }
    let path = meta_path(client_key, &id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, serde_json::to_string_pretty(&meta_value).map_err(|e| e.to_string())?)
        .map_err(|e| format!("写 meta 失败: {e}"))?;
    Ok((id, file_count))
}

/// 一条「待搬入」计划。
#[derive(Debug, Clone, PartialEq)]
pub struct AdoptionPlan {
    pub from: String,
    pub source_id: String,
    pub uid: String,
}

/// 一条「跳过」记录（带原因，供界面如实说明为什么没搬）。
#[derive(Debug, Clone, PartialEq)]
pub struct AdoptionSkip {
    pub from: String,
    pub source_id: String,
    pub uid: Option<String>,
    pub reason: &'static str,
}

/// 「哪些账号该搬过去」的**纯判定** —— 去重与跳过规则只此一处实现。
///
/// `have` = 目标账号库现有的 uid 集合。**会被就地补进**：这样同一个 uid 即使在
/// 多个源客户端下各有一份（SOLO 与 IDE 都备份过同一账号），也只会出现在计划里一次。
/// `sources` = 按顺序的 `(源客户端 key, [(账号 id, 归属 uid)])`，`uid = None` 表示档案不可解析。
pub fn plan_adoptions(
    have: &mut HashSet<String>,
    sources: &[(String, Vec<(String, Option<String>)>)],
) -> (Vec<AdoptionPlan>, Vec<AdoptionSkip>) {
    let mut plan = Vec::new();
    let mut skip = Vec::new();
    for (from, accounts) in sources {
        for (id, uid) in accounts {
            let Some(uid) = uid.clone() else {
                skip.push(AdoptionSkip {
                    from: from.clone(),
                    source_id: id.clone(),
                    uid: None,
                    reason: "读不出归属 uid（账号档案不可解析）",
                });
                continue;
            };
            if have.contains(&uid) {
                skip.push(AdoptionSkip {
                    from: from.clone(),
                    source_id: id.clone(),
                    uid: Some(uid),
                    reason: "目标账号库里已有这个账号（同一 uid）",
                });
                continue;
            }
            have.insert(uid.clone());
            plan.push(AdoptionPlan {
                from: from.clone(),
                source_id: id.clone(),
                uid,
            });
        }
    }
    (plan, skip)
}

/// 残档补全：目标库里同一 uid 的账号若**缺**侧车文件（`oauth.json` / `profile.json`）而源有，就补上。
///
/// 为什么会有这种残档：v0.2.26 之前这里走的是「只搬载体文件」的老导出格式
/// （见 [`collect_account_files`] 的说明），于是并入过来的账号丢了网页凭证 ——
/// 功能上还能切号（载体在），但界面上显示成 `oauth_717036` / `0 B` / 查不了积分，看着像坏了。
///
/// ⚠️ 只补**目标没有的**文件，绝不覆盖已有内容；`storage.json`（真正的凭据）一个字都不碰。
/// 补完把 meta 里的账（`kind` / `files` / `file_count` / `total_bytes`）按磁盘实际情况重算 ——
/// 原来那份写着 `total_bytes: 0`，本身就是错的。
fn repair_missing_sidecars(target_key: &str, target_id: &str, src_key: &str, src_id: &str) -> Vec<String> {
    let src_dir = account_dir(src_key, src_id);
    let dst_dir = account_dir(target_key, target_id);
    let mut copied = Vec::new();
    for name in ["oauth.json", "profile.json"] {
        let from = src_dir.join(name);
        let to = dst_dir.join(name);
        if to.is_file() || !from.is_file() {
            continue;
        }
        if std::fs::copy(&from, &to).is_ok() {
            copied.push(name.to_string());
        }
    }
    if !copied.is_empty() {
        refresh_meta_bookkeeping(target_key, target_id);
    }
    copied
}

/// 按磁盘上的实际情况重算 meta 里的载体账（导入 / 补全之后调用）。
fn refresh_meta_bookkeeping(client_key: &str, id: &str) {
    let dir = account_dir(client_key, id);
    let mut meta_value = match read_raw_meta(client_key, id) {
        Value::Object(_) => read_raw_meta(client_key, id),
        _ => json!({}),
    };
    let entries = sanitize_entries(
        &meta_value
            .get("entries")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(String::from).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    let files: Vec<CarrierFile> = expand_entries(&dir, &entries)
        .into_iter()
        .filter_map(|rel| {
            let p = dir.join(rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str()));
            let md = std::fs::metadata(&p).ok()?;
            Some(CarrierFile { rel, len: md.len() as i64, sha256: hash_file(&p).unwrap_or_default() })
        })
        .collect();
    let total: u64 = files.iter().map(|f| f.len.max(0) as u64).sum();
    if let Some(obj) = meta_value.as_object_mut() {
        if dir.join("oauth.json").is_file() {
            obj.insert("kind".into(), json!("oauth"));
        }
        obj.insert("files".into(), json!(files));
        obj.insert("file_count".into(), json!(files.len()));
        obj.insert("total_bytes".into(), json!(total));
    }
    let path = meta_path(client_key, id);
    let _ = std::fs::write(path, serde_json::to_string_pretty(&meta_value).unwrap_or_default());
}

/// 把「已并入 `target_key` 的客户端」账号库里的账号，全部并入 `target_key` 自己的账号库。
///
/// 为什么需要它：2026-10 官方把 TRAE SOLO 并进 Trae 时（国内版 → `trae-cn`，
/// 国际版 → `trae-intl`）**只搬了程序与会话**
/// （实测：SOLO 的 7 条会话已原样出现在 Trae CN 库里，消息数逐条相等）——
/// 本工具自己的账号库（`<工具目录>/trae/vault/<clientKey>/`）官方不会碰。于是 SOLO 下
/// 备份过的账号在承载方下**看不见、也切不过去**：用户在 SOLO 里存了 3 个账号，
/// 合并后 Trae CN 这边只剩 1 个，另外两个像是凭空消失了。
///
/// ⚠️ 源是**按 `merged_into == 目标` 反查**出来的（见 `adopt_merged_accounts`）：
/// 所以两条 SOLO 线的并入关系一旦漏写一条，那一条的账号就永远没有入口搬过来。
///
/// 三条安全约定：
/// - **按 uid 去重、绝不覆盖**：目标已有同一 uid 的账号就跳过。判据是 **uid 而非账号 id** ——
///   同一账号在两边可能落在不同 id 下，只看 id 会重复搬第二份（规则见 [`plan_adoptions`]）；
/// - **不动源**：只是一份复制，SOLO 的账号库原样保留（老数据仍要能看、能导）；
/// - 两端 `api_host` 相同（`https://api.trae.cn` / `trae-api-cn.mchost.guru`），
///   载体可直接使用，无需重新登录。
///
/// 顺带**补全残档**：老版本「一并带入」漏搬的侧车文件（`oauth.json` / `profile.json`）
/// 在这里补回去（只补缺失的，见 [`repair_missing_sidecars`]）—— 用户不用手工删号重搬。
///
/// `dry_run = true` 时只列出「会搬哪些 / 会修哪些」，**一个字节都不写**。
pub fn adopt_merged_accounts(target_key: &str, dry_run: bool) -> Result<Value, String> {
    let target = crate::modules::trae_discover::get_client(target_key)
        .ok_or_else(|| format!("未知客户端: {target_key}"))?;

    // 目标已有的 uid（去重判据）
    let mut have: HashSet<String> = list_vault_accounts(target_key)
        .iter()
        .filter_map(|id| uid_of_account(target_key, id))
        .collect();
    // uid → 目标库里的账号 id（补残档时要按 uid 找到那份残档）
    let target_index = crate::modules::trae_oauth::vault_uid_index(target_key);

    // 已并入 `target_key` 的源客户端 → (key, [(id, uid)])
    let sources: Vec<(String, Vec<(String, Option<String>)>)> =
        crate::modules::trae_discover::CLIENTS
            .iter()
            .filter(|c| c.merged_into == Some(target_key))
            .map(|c| {
                let src = c.key;
                (
                    src.to_string(),
                    list_vault_accounts(src)
                        .into_iter()
                        .map(|id| {
                            let uid = uid_of_account(src, &id);
                            (id, uid)
                        })
                        .collect(),
                )
            })
            .collect();

    let (plan, skips) = plan_adoptions(&mut have, &sources);

    // 返回 `String` 而不是 `&str`：`TraeClient::label` 是 `&'static str`，而 `k` 是调用方的
    // 短生命周期引用，混在同一个返回位置会让编译器要求 `'1: '2`。
    let label_of = |k: &str| -> String {
        crate::modules::trae_discover::get_client(k)
            .map(|c| c.label.to_string())
            .unwrap_or_else(|| k.to_string())
    };

    let mut adopted: Vec<Value> = Vec::new();
    for p in &plan {
        if dry_run {
            adopted.push(json!({
                "from": p.from,
                "from_label": label_of(&p.from),
                "source_id": p.source_id,
                "id": p.source_id,
                "uid": p.uid,
            }));
            continue;
        }
        let payload = export_account(&p.from, &p.source_id)?;
        let (new_id, files) = import_account(target_key, &payload, Some(p.source_id.as_str()))?;
        adopted.push(json!({
            "from": p.from,
            "from_label": label_of(&p.from),
            "source_id": p.source_id,
            "id": new_id,
            "uid": p.uid,
            "files": files,
        }));
    }
    let skipped: Vec<Value> = skips
        .iter()
        .map(|s| {
            json!({
                "from": s.from,
                "from_label": label_of(&s.from),
                "source_id": s.source_id,
                "uid": s.uid,
                "reason": s.reason,
            })
        })
        .collect();

    // 残档补全（只补目标缺的侧车文件，绝不覆盖）：老版本「一并带入」漏搬 oauth.json /
    // profile.json，用户看到的账号就成了「oauth_xxxxxx / 0 B / 查不了积分」。
    // 这里不依赖 `plan_adoptions` 的跳过理由（那是会变的文案），直接拿目标 uid 索引对一遍。
    let mut repaired: Vec<Value> = Vec::new();
    for (from, accounts) in &sources {
        for (sid, uid) in accounts {
            let Some(uid) = uid.as_deref() else { continue };
            let Some((tid, _)) = target_index.get(uid) else { continue };
            if sid == tid && from == target_key {
                continue;
            }
            let src_dir = account_dir(from, sid);
            let dst_dir = account_dir(target_key, tid);
            let need: Vec<&str> = ["oauth.json", "profile.json"]
                .into_iter()
                .filter(|n| src_dir.join(n).is_file() && !dst_dir.join(n).is_file())
                .collect();
            if need.is_empty() {
                continue;
            }
            if dry_run {
                repaired.push(json!({
                    "from": from, "from_label": label_of(from),
                    "source_id": sid, "id": tid, "uid": uid, "files": need,
                }));
                continue;
            }
            let copied = repair_missing_sidecars(target_key, tid, from, sid);
            if !copied.is_empty() {
                repaired.push(json!({
                    "from": from, "from_label": label_of(from),
                    "source_id": sid, "id": tid, "uid": uid, "files": copied,
                }));
            }
        }
    }

    Ok(json!({
        "target": target_key,
        "target_label": target.label,
        "dry_run": dry_run,
        "adopted": adopted,
        "skipped": skipped,
        "repaired": repaired,
    }))
}

/// 便捷：uid 字段取值。
pub fn value_uid(v: &Value) -> Option<String> {
    v.as_i64().map(|i| i.to_string()).or_else(|| v.as_str().map(String::from))
}

#[allow(unused)]
fn unused_trae_client_guard(_c: &TraeClient) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(key: &str, accounts: &[(&str, Option<&str>)]) -> (String, Vec<(String, Option<String>)>) {
        (
            key.to_string(),
            accounts
                .iter()
                .map(|(i, u)| (i.to_string(), u.map(String::from)))
                .collect(),
        )
    }

    /// 目标已有的 uid 一律跳过 —— 这是「绝不覆盖」的唯一实现处。
    #[test]
    fn plan_skips_uids_target_already_has() {
        let mut have: HashSet<String> = ["u1".to_string()].into();
        let sources = vec![src(
            "solo-cn",
            &[("oauth_a", Some("u1")), ("oauth_b", Some("u2"))],
        )];
        let (plan, skip) = plan_adoptions(&mut have, &sources);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].source_id, "oauth_b");
        assert_eq!(plan[0].uid, "u2");
        assert_eq!(skip.len(), 1);
        assert_eq!(skip[0].source_id, "oauth_a");
        assert_eq!(skip[0].uid.as_deref(), Some("u1"));
    }

    /// 同一 uid 在多个源客户端下各有一份（SOLO 与 IDE 都备份过同一账号）⇒ 只搬第一份。
    #[test]
    fn plan_dedupes_same_uid_across_sources() {
        let mut have = HashSet::new();
        let sources = vec![
            src("solo-cn", &[("oauth_x", Some("u9"))]),
            src("solo-intl", &[("oauth_y", Some("u9"))]),
        ];
        let (plan, skip) = plan_adoptions(&mut have, &sources);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].from, "solo-cn");
        assert_eq!(skip.len(), 1);
        assert_eq!(skip[0].from, "solo-intl");
        assert_eq!(skip[0].uid.as_deref(), Some("u9"));
    }

    /// 档案不可解析（uid 读不出）⇒ 跳过并给出原因，**不能猜一个 uid 搬过去**。
    #[test]
    fn plan_skips_unreadable_profiles() {
        let mut have = HashSet::new();
        let sources = vec![src("solo-cn", &[("oauth_bad", None)])];
        let (plan, skip) = plan_adoptions(&mut have, &sources);
        assert!(plan.is_empty());
        assert_eq!(skip.len(), 1);
        assert!(skip[0].uid.is_none());
    }

    /// 判定会就地补 `have`：同一批连跑两次，第二次一条都不搬（幂等）。
    ///
    /// 这条保护的是「搬完不刷新就再点一次」——重复搬运会在账号库里堆出同名副本。
    #[test]
    fn plan_is_idempotent_when_reused() {
        let mut have = HashSet::new();
        let sources = vec![src(
            "solo-cn",
            &[("oauth_a", Some("u1")), ("oauth_b", Some("u2"))],
        )];
        let (first, _) = plan_adoptions(&mut have, &sources);
        assert_eq!(first.len(), 2);
        let (second, skip) = plan_adoptions(&mut have, &sources);
        assert!(second.is_empty(), "第二次不该再搬同一批");
        assert_eq!(skip.len(), 2);
    }
}

/// 落盘用例：搬账号要**字节级保真**（老版本只搬载体文件，账号就残了）。
///
/// `account_dir` 走 `config::store_dir()`，而 `TWIN_SWITCH_HOME` 是**进程级**环境变量
/// ⇒ 这些用例必须串行（共用一把锁），并在结束时恢复原值 + 删掉临时家目录。
#[cfg(test)]
mod adopt_fs_tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    struct Sandbox {
        prev: Option<std::ffi::OsString>,
        dir: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let prev = std::env::var_os(crate::modules::config::HOME_ENV_VAR);
            let dir = std::env::temp_dir().join(format!(
                "twin-switch-vault-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("建临时家目录");
            std::env::set_var(crate::modules::config::HOME_ENV_VAR, &dir);
            Self { prev, dir }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(crate::modules::config::HOME_ENV_VAR, v),
                None => std::env::remove_var(crate::modules::config::HOME_ENV_VAR),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 一份**真能被解开**的载体（uid 就藏在里面）—— `have` / `target_index` 都靠它认账号。
    fn km_storage(uid: &str) -> String {
        let blob = crate::modules::trae_km::encrypt_km_json(&json!({
            "userId": uid, "token": "tok", "refreshToken": "rt",
        }))
        .expect("加密登录态");
        format!("{{\"{KEY_AUTH}\":{}}}", serde_json::to_string(&blob).unwrap())
    }

    /// 造一份「SOLO 里的网页登录账号」：`oauth.json`（含 uid）+ `profile.json` + 载体。
    fn seed_solo_account(id: &str, uid: &str) -> PathBuf {
        let dir = account_dir("solo-cn", id);
        std::fs::create_dir_all(dir.join("User").join("globalStorage")).unwrap();
        let storage = km_storage(uid);
        std::fs::write(dir.join(REL_STORAGE), &storage).unwrap();
        std::fs::write(
            dir.join("oauth.json"),
            serde_json::to_string_pretty(&json!({
                "kind": "oauth", "id": id, "client": "solo-cn",
                "displayName": "账号 000000", "uid": uid, "token": "t", "refreshToken": "r",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("profile.json"), "{\"screen_name\":\"老王\"}").unwrap();
        std::fs::write(
            meta_path("solo-cn", id),
            serde_json::to_string_pretty(&json!({
                "id": id, "client": "solo-cn", "kind": "oauth",
                "display_name": "账号 000000", "root_dir": "C:/x/TRAE SOLO CN",
                "entries": [REL_STORAGE],
                "files": [{ "rel": REL_STORAGE, "len": storage.len(), "sha256": "x" }],
                "file_count": 1, "total_bytes": storage.len(),
                "created_at": "2026-10-01T00:00:00+08:00",
            }))
            .unwrap(),
        )
        .unwrap();
        dir
    }

    /// 造一份「老版本一并带入留下的残档」：同一 uid 的载体在，但**没有 `oauth.json`**，
    /// meta 里写着一份明显不对的账（`kind: carrier` / `total_bytes: 0`）。
    fn seed_legacy_copy(id: &str, uid: &str) -> PathBuf {
        let dst = account_dir("trae-cn", id);
        std::fs::create_dir_all(dst.join("User").join("globalStorage")).unwrap();
        std::fs::write(dst.join(REL_STORAGE), km_storage(uid)).unwrap();
        std::fs::write(
            meta_path("trae-cn", id),
            serde_json::to_string_pretty(&json!({
                "id": id, "client": "trae-cn", "kind": "carrier",
                "entries": [REL_STORAGE], "files": [], "file_count": 1, "total_bytes": 0,
                "created_at": "2026-10-12T00:00:00+08:00",
            }))
            .unwrap(),
        )
        .unwrap();
        dst
    }

    /// 导出必须带上账号目录里的**全部**文件（侧车也要），meta 原样带走，但 `meta.json` 不进文件清单。
    #[test]
    fn export_carries_sidecars_and_raw_meta() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sb = Sandbox::new("export");
        seed_solo_account("oauth_a", "u1");

        let payload = export_account("solo-cn", "oauth_a").unwrap();
        let rels: Vec<&str> = payload["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["rel"].as_str())
            .collect();
        assert!(rels.contains(&"oauth.json"), "侧车 oauth.json 必须带上：{rels:?}");
        assert!(rels.contains(&"profile.json"), "侧车 profile.json 必须带上：{rels:?}");
        assert!(rels.contains(&REL_STORAGE), "载体文件必须在：{rels:?}");
        assert!(!rels.contains(&"meta.json"), "meta.json 由 meta 字段承载，不进文件清单");
        assert_eq!(payload["kind"], json!("oauth"));
        assert_eq!(payload["meta"]["display_name"], json!("账号 000000"));
        assert_eq!(payload["version"], json!(EXPORT_VERSION));
    }

    /// 老格式（v1，没有 `meta` / 侧车）仍要能导入，且**不能**把它当成 oauth（它就是载体账号）。
    #[test]
    fn import_still_accepts_legacy_v1_payload() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sb = Sandbox::new("legacy");
        let payload = json!({
            "format": EXPORT_FORMAT, "version": 1, "client": "solo-cn", "id": "old",
            "entries": [REL_STORAGE],
            "files": [{ "rel": REL_STORAGE, "len": 5, "sha256": "", "b64": STANDARD.encode(b"hello") }],
        });
        let (id, count) = import_account("trae-cn", &payload, None).unwrap();
        assert_eq!(id, "old");
        assert_eq!(count, 1);
        assert_eq!(std::fs::read_to_string(account_dir("trae-cn", "old").join(REL_STORAGE)).unwrap(), "hello");
        // v1 没有 kind ⇒ 按载体处理，且 last_used_at / verified_uid 必须是空的
        let raw = read_raw_meta("trae-cn", "old");
        assert_eq!(raw["kind"], json!("carrier"));
        assert!(raw["last_used_at"].is_null());
        assert!(raw["verified_uid"].is_null());
    }

    /// 导入落盘后，`verified_uid` / `last_used_at` 必须是空的 ——
    /// 否则界面会把这个「刚进库、还没在本客户端切过」的账号算成「当前登录」。
    #[test]
    fn import_clears_machine_facts() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sb = Sandbox::new("clear");
        seed_solo_account("oauth_a", "u1");
        // 源档案里带着「切过」的痕迹（源客户端的本机事实）
        let mut meta = read_raw_meta("solo-cn", "oauth_a");
        meta["verified_uid"] = json!("u1");
        meta["last_used_at"] = json!("2026-10-11T00:00:00+08:00");
        std::fs::write(meta_path("solo-cn", "oauth_a"), meta.to_string()).unwrap();

        let payload = export_account("solo-cn", "oauth_a").unwrap();
        let (id, _) = import_account("trae-cn", &payload, Some("oauth_a")).unwrap();
        let raw = read_raw_meta("trae-cn", &id);
        assert!(raw["verified_uid"].is_null(), "verified_uid 是本客户端的本机事实，必须清空");
        assert!(raw["last_used_at"].is_null());
        // 但账号本身的信息要留住
        assert_eq!(raw["kind"], json!("oauth"));
        assert_eq!(raw["display_name"], json!("账号 000000"));
    }

    /// 一并带入：侧车、kind、display_name、total_bytes 全都要跟着过来。
    #[test]
    fn adopt_keeps_sidecars_kind_and_bookkeeping() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sb = Sandbox::new("adopt");
        seed_solo_account("oauth_a", "u1");

        let res = adopt_merged_accounts("trae-cn", false).unwrap();
        assert_eq!(res["adopted"].as_array().unwrap().len(), 1);
        assert!(res["repaired"].as_array().unwrap().is_empty());

        let dir = account_dir("trae-cn", "oauth_a");
        assert!(dir.join("oauth.json").is_file(), "并入后必须带网页凭证");
        assert!(dir.join("profile.json").is_file(), "并入后必须带资料缓存");
        let raw = read_raw_meta("trae-cn", "oauth_a");
        assert_eq!(raw["kind"], json!("oauth"), "账号类型不能被写成载体");
        assert_eq!(raw["display_name"], json!("账号 000000"));
        let len = std::fs::metadata(dir.join(REL_STORAGE)).unwrap().len() as u64;
        assert_eq!(raw["total_bytes"], json!(len), "total_bytes 不能再是 0");
        assert_eq!(raw["client"], json!("trae-cn"));
        // 源**一个字节都不动**
        assert!(account_dir("solo-cn", "oauth_a").join("oauth.json").is_file());
    }

    /// 老版本留下的残档（同一 uid 已在目标库里、但缺 oauth.json/profile.json）
    /// 要被**补全**，而且**绝不覆盖**已有的载体文件与已有账号。
    #[test]
    fn adopt_repairs_legacy_copy_without_overwriting() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sb = Sandbox::new("repair");
        seed_solo_account("oauth_a", "u1");
        let dst = seed_legacy_copy("oauth_a", "u1");
        let carrier_before = std::fs::read(dst.join(REL_STORAGE)).unwrap();

        let res = adopt_merged_accounts("trae-cn", false).unwrap();
        // 同一 uid ⇒ 不重复搬
        assert!(res["adopted"].as_array().unwrap().is_empty());
        let repaired = res["repaired"].as_array().unwrap();
        assert_eq!(repaired.len(), 1, "残档必须被补全：{res}");
        assert_eq!(repaired[0]["id"], json!("oauth_a"));

        assert!(dst.join("oauth.json").is_file());
        assert!(dst.join("profile.json").is_file());
        // 载体**原封不动**（绝不覆盖）
        assert_eq!(std::fs::read(dst.join(REL_STORAGE)).unwrap(), carrier_before);
        let raw = read_raw_meta("trae-cn", "oauth_a");
        assert_eq!(raw["kind"], json!("oauth"), "补齐网页凭证后类型要跟上来");
        assert!(raw["total_bytes"].as_u64().unwrap() > 0, "账要按磁盘重算，不能留 0");
        // 幂等：再跑一次没有可修的了
        let again = adopt_merged_accounts("trae-cn", true).unwrap();
        assert!(again["repaired"].as_array().unwrap().is_empty());
    }

    /// 预演（`dry_run`）**一个字节都不写**：残档还是要原样留着。
    #[test]
    fn adopt_dry_run_writes_nothing() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sb = Sandbox::new("dryrun");
        seed_solo_account("oauth_a", "u1");
        let dst = seed_legacy_copy("oauth_a", "u1");

        let res = adopt_merged_accounts("trae-cn", true).unwrap();
        assert_eq!(res["dry_run"], json!(true));
        assert_eq!(res["repaired"].as_array().unwrap().len(), 1, "预演要如实报告会修什么");
        assert!(!dst.join("oauth.json").exists(), "预演不该落盘");
    }

    /// 导入的备份是**外部输入**：`rel` 带 `..` / 盘符 / 绝对路径的一律丢掉，
    /// 不许写到账号目录之外。
    #[test]
    fn import_rejects_path_traversal() {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _sb = Sandbox::new("traversal");
        let payload = json!({
            "format": EXPORT_FORMAT, "version": EXPORT_VERSION, "client": "trae-cn", "id": "bad",
            "entries": [REL_STORAGE],
            "files": [
                { "rel": "../evil.json", "b64": STANDARD.encode(b"pwn") },
                { "rel": "C:/Windows/Temp/evil.json", "b64": STANDARD.encode(b"pwn") },
                { "rel": "/etc/passwd", "b64": STANDARD.encode(b"pwn") },
                { "rel": "ok/../../evil.json", "b64": STANDARD.encode(b"pwn") },
                { "rel": REL_STORAGE, "b64": STANDARD.encode(b"good") },
            ],
        });
        let (id, count) = import_account("trae-cn", &payload, None).unwrap();
        assert_eq!(count, 1, "只有合法的那一条算进载体文件");
        // 越界目标：账号目录的上一级（`..` 会落到这里）与临时家目录根
        let account = account_dir("trae-cn", &id);
        assert!(!account.parent().unwrap().join("evil.json").exists(), "越界文件一个都不许落盘");
        assert!(!vault_root().join("evil.json").exists());
        assert_eq!(std::fs::read_to_string(account.join(REL_STORAGE)).unwrap(), "good");
    }
}
