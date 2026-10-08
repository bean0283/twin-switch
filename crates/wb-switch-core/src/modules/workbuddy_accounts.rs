//! WorkBuddy 账号显示名解析（**只读**）。
//!
//! 问题：WorkBuddy 的会话只记录 `sessions.user_id`（一段 UUID），界面上一律显示成
//! `…97eac1` 这样的尾号，多账号时根本分不清谁是谁。本模块把散落在本机的账号信息
//! 汇总成「人看得懂的名字」。
//!
//! **只读、只读、只读**——绝不写 WorkBuddy 的任何文件；也绝不读取 token 字段
//! （只看 uid / 昵称 / 手机号 / 邮箱 / 账号类型 / 版本这些展示字段）。
//!
//! 三个来源按**可信度从低到高**依次合并（后者覆盖前者，缺项才补）：
//!
//! | 优先级 | 来源 | 说明 |
//! | --- | --- | --- |
//! | 1 | `~/.workbuddy/logs/*.log` | 遥测行 `"userId":"…","username":"…","userNickname":"…"`，覆盖最全但会被日志轮转清掉 |
//! | 2 | `~/.wb-switch/accounts.json` | WorkBuddy 账号切换工具（有人装过）留下的账号库，含手机号 |
//! | 3 | `~/.twin-switch/workbuddy-accounts.json` | **本工具自己的**账号库（扫码登录采集的昵称） |
//! | 4 | `~/.workbuddy/storage/skeleton/account-snapshot.json` | WorkBuddy 自己写的账号快照，**只覆盖当前登录账号**，最权威 |
//!
//! 解析不出来的 uid 一律回退成 `uid …xxxxxx`，绝不编造。

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::modules::workbuddy_source;

/// 单个日志文件的读取上限：超出部分只读**末尾**（登录遥测在尾部更常见）。
const LOG_READ_CAP: u64 = 4 << 20;
/// 主日志（体积小、命中率最高）整份读。
const LOG_FULL_CAP: u64 = 8 << 20;
/// 在 `userId` 之后多远范围内找 `username`。
const PAIR_WINDOW: usize = 400;

/// 一个 WorkBuddy 账号的可展示信息（不含任何凭据）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct WbAccount {
    pub uid: String,
    /// 最佳显示名：昵称 → 手机号 → 邮箱 → 空。
    pub name: String,
    pub nickname: String,
    pub phone: String,
    pub email: String,
    /// 账号类型：`personal` / `enterprise`（空为未知）。
    pub kind: String,
    /// 版本：`free` / `pro`（空为未知）。
    pub edition: String,
    /// 是否本机当前登录账号（账号快照里的 primary）。
    pub is_primary: bool,
    /// 名字来源（中文，便于判断可信度）。
    pub source: String,
}

impl WbAccount {
    /// 主显示名；没有名字时回退成 uid（调用方可用 `label()`）。
    fn best_name(&self) -> String {
        for c in [&self.nickname, &self.phone, &self.email] {
            let t = c.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        String::new()
    }

    /// `昵称（uid …97eac1）`；没有昵称时退化成 `uid …97eac1`。
    pub fn label(&self) -> String {
        let n = self.best_name();
        if n.is_empty() {
            format!("uid …{}", uid_suffix(&self.uid))
        } else {
            format!("{n}（uid …{}）", uid_suffix(&self.uid))
        }
    }

    /// 次要信息行：手机号 / 类型 / 版本，去掉与显示名重复的部分。
    pub fn meta(&self) -> String {
        let name = self.best_name();
        let mut parts: Vec<String> = Vec::new();
        for (v, prefix) in [
            (&self.phone, ""),
            (&self.email, ""),
            (&self.kind, ""),
            (&self.edition, ""),
        ] {
            let t = v.trim();
            if t.is_empty() || t == name || parts.iter().any(|p| p == t) {
                continue;
            }
            if t == "personal" {
                parts.push("个人版".to_string());
            } else if t == "enterprise" {
                parts.push("企业版".to_string());
            } else if t == "free" {
                parts.push("免费版".to_string());
            } else if t == "pro" {
                parts.push("Pro".to_string());
            } else {
                parts.push(format!("{prefix}{t}"));
            }
        }
        parts.join(" · ")
    }
}

fn uid_suffix(uid: &str) -> String {
    uid.chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn data_root() -> PathBuf {
    workbuddy_source::data_root()
}

/// uid 形如 36 字符、4 个连字符的 UUID —— 用来把「解析到的东西」和「误命中的文本」区分开。
fn is_uuid_like(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

// ---------------------------------------------------------------------------
// 来源 1：运行日志遥测
// ---------------------------------------------------------------------------

/// 读文件；超过 `cap` 时只读末尾 `cap` 字节（可能从多字节字符中间截断，用宽容解码）。
fn read_capped(path: &Path, cap: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let bytes = if meta.len() <= cap {
        std::fs::read(path).ok()?
    } else {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path).ok()?;
        f.seek(SeekFrom::End(-(cap as i64))).ok()?;
        let mut buf = Vec::with_capacity(cap as usize);
        f.read_to_end(&mut buf).ok()?;
        buf
    };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// 把字节下标收拢到 UTF-8 字符边界上，并夹在 `text` 长度内。
///
/// **⚠️ 本模块所有切片都必须先过这一层。** 日志里混着中文（昵称、工具名、标题），
/// 而这里的下标全是「字节偏移量 + 一个常量跨度」算出来的（见 [`PAIR_WINDOW`]）——
/// 只要跨度落在一个多字节字符中间，`&text[a..b]` 就会 panic：
/// `byte index N is not a char boundary; it is inside '工'`。
///
/// 这个 panic 的后果特别恶劣：它发生在「只是为了把 uid 显示成人名」的只读解析里，
/// 却会把**整条调用链**一起带走（`workbuddy_vault::list` 直接 panic ⇒ 命令层拿到
/// JoinError ⇒ 界面显示「账号库还是空的」）。所以这里只往前退、绝不往外抛。
fn clamp_boundary(text: &str, idx: usize) -> usize {
    let mut i = idx.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 解析 `key` 后面的字符串值，容忍 JSON 在被嵌进日志时产生的 `\"` / `\\"` 转义。
///
/// 返回值与「值结束的位置」；找不到返回 `None`。
fn take_value(text: &str, from: usize, key: &str) -> Option<(String, usize)> {
    let from = clamp_boundary(text, from);
    if from >= text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let idx = from + text[from..].find(key)?;
    let mut i = idx + key.len();
    // 跳过值与键之间的引号 / 冒号 / 反斜杠 / 空白
    while i < bytes.len() && matches!(bytes[i], b'\\' | b'"' | b':' | b' ' | b'\t') {
        i += 1;
    }
    let start = clamp_boundary(text, i);
    while i < bytes.len() && !matches!(bytes[i], b'\\' | b'"') {
        i += 1;
    }
    let end = clamp_boundary(text, i);
    if end <= start {
        return None;
    }
    Some((text[start..end].to_string(), end))
}

/// 从一段日志文本中抽取 `userId → username` 配对。
fn scan_uid_names(text: &str, out: &mut BTreeMap<String, String>) {
    let mut cursor = 0usize;
    while let Some((uid, end)) = take_value(text, cursor, "userId") {
        cursor = end;
        if !is_uuid_like(&uid) {
            continue;
        }
        // 姓名紧跟其后；优先 userNickname（WorkBuddy 5.6 起 username 可能是邮箱或空）
        //
        // ⚠️ `end + PAIR_WINDOW` 是**字节**偏移，可能正好落在中文字符中间 ——
        // 必须收拢到字符边界再切片，否则这里会 panic（2026-10-08 线上真事：
        // `daemon.log` 尾部中文工具名踩中，账号库整页变成「空」）。
        let window_end = clamp_boundary(text, end + PAIR_WINDOW);
        let window = &text[clamp_boundary(text, end)..window_end];
        let name = take_value(window, 0, "userNickname")
            .or_else(|| take_value(window, 0, "username"))
            .map(|(v, _)| v)
            .unwrap_or_default();
        let name = name.trim().to_string();
        if name.is_empty() || name == "unknown" {
            continue;
        }
        out.entry(uid).or_insert(name);
    }
}

/// 扫描 WorkBuddy 运行日志，返回 `uid → 显示名`。
fn names_from_logs() -> BTreeMap<String, String> {
    let logs = data_root().join("logs");
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let candidates = [
        (logs.join("main.log"), LOG_FULL_CAP),
        (logs.join("AppStartup.log"), LOG_FULL_CAP),
        (logs.join("daemon.log"), LOG_READ_CAP),
    ];
    for (path, cap) in candidates {
        if let Some(text) = read_capped(&path, cap) {
            scan_uid_names(&text, &mut out);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 来源 2：切换工具的账号库（可选，装了才有）
// ---------------------------------------------------------------------------

/// 取字符串字段；WorkBuddy 5.6 起昵称可能是 `{$wbEncrypted: …}` 加密信封对象，
/// 这种形态一律视为不可用（不能把对象当字符串透传给前端，会白屏）。
fn str_at(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// 账号信息条目（`~/.wb-switch/accounts.json` 只取展示字段）。
struct Entry {
    uid: String,
    nickname: String,
    phone: String,
    email: String,
    kind: String,
    edition: String,
}

fn accounts_json_entries() -> Vec<Entry> {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".wb-switch")
        .join("accounts.json");
    let Some(text) = std::fs::read_to_string(&path).ok() else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for it in items {
        let profile = it.get("profile_raw").filter(|v| v.is_object());
        // uid 优先取 profile_raw.uid（登录返回的原样数据），退到顶层 uid / id
        let mut uid = profile.map(|p| str_at(p, "uid")).unwrap_or_default();
        if uid.is_empty() {
            uid = str_at(&it, "uid");
        }
        if uid.is_empty() {
            uid = str_at(&it, "id");
        }
        if !is_uuid_like(&uid) {
            continue;
        }
        let nickname = profile
            .map(|p| str_at(p, "nickname"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| str_at(&it, "nickname"));
        let phone = profile
            .map(|p| str_at(p, "phoneNumber"))
            .unwrap_or_default();
        let email = str_at(&it, "email");
        let kind = profile.map(|p| str_at(p, "type")).unwrap_or_default();
        out.push(Entry {
            uid,
            nickname,
            phone,
            email,
            kind,
            edition: String::new(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// 来源 3：本工具自己的账号库（扫码登录采集的昵称 / 手机号）
// ---------------------------------------------------------------------------

/// 读 `~/.twin-switch/workbuddy-accounts.json`（本工具账号库）。
///
/// 不直接调用 `workbuddy_vault`：那个模块反向依赖本模块，会形成循环。
fn own_vault_entries() -> Vec<Entry> {
    let path = crate::modules::config::store_dir().join("workbuddy-accounts.json");
    let Some(text) = std::fs::read_to_string(&path).ok() else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let items = match v {
        Value::Array(a) => a,
        Value::Object(o) => o
            .get("accounts")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for it in items {
        let uid = str_at(&it, "uid");
        if !is_uuid_like(&uid) {
            continue;
        }
        out.push(Entry {
            uid,
            nickname: str_at(&it, "nickname"),
            phone: str_at(&it, "phoneNumber"),
            email: str_at(&it, "email"),
            kind: str_at(&it, "type"),
            edition: str_at(&it, "editionType"),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// 来源 4：WorkBuddy 自己的账号快照（只覆盖当前登录账号）
// ---------------------------------------------------------------------------

fn account_snapshot_entries() -> Vec<Entry> {
    let path = data_root()
        .join("storage")
        .join("skeleton")
        .join("account-snapshot.json");
    let Some(text) = std::fs::read_to_string(&path).ok() else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["primary", "current", "account"] {
        if let Some(node) = v.get(key).filter(|n| n.is_object()) {
            out.push(snapshot_entry(node));
        }
    }
    if let Some(Value::Array(items)) = v.get("accounts") {
        for node in items {
            out.push(snapshot_entry(node));
        }
    }
    out.retain(|e| is_uuid_like(&e.uid));
    out
}

fn snapshot_entry(node: &Value) -> Entry {
    let edition = match str_at(node, "editionType") {
        s if !s.is_empty() => s,
        _ => {
            if node.get("isPro").and_then(Value::as_bool).unwrap_or(false) {
                "pro".to_string()
            } else {
                String::new()
            }
        }
    };
    Entry {
        uid: str_at(node, "uid"),
        nickname: str_at(node, "nickname"),
        phone: str_at(node, "phoneNumber"),
        email: str_at(node, "email"),
        kind: str_at(node, "type"),
        edition,
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 本机登录过的账号 uid：`storage/user-{uid}-personal` 目录名是最可靠的线索
/// （WorkBuddy 每个登录过的账号都会留下这样一个目录）。
pub fn known_uids() -> Vec<String> {
    let mut out = Vec::new();
    let storage = data_root().join("storage");
    if let Ok(rd) = std::fs::read_dir(&storage) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(rest) = name.strip_prefix("user-") {
                if let Some(uid) = rest.strip_suffix("-personal") {
                    if is_uuid_like(uid) {
                        out.push(uid.to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 把一条账号信息合并进表：非空字段覆盖旧值（来源按可信度从低到高依次调用）。
fn merge_account(
    map: &mut BTreeMap<String, WbAccount>,
    sources: &mut BTreeMap<String, Vec<&'static str>>,
    uid: &str,
    nickname: &str,
    phone: &str,
    email: &str,
    kind: &str,
    edition: &str,
    from: &'static str,
    primary: bool,
) {
    let a = map.entry(uid.to_string()).or_insert_with(|| WbAccount {
        uid: uid.to_string(),
        ..Default::default()
    });
    if !nickname.trim().is_empty() {
        a.nickname = nickname.trim().to_string();
    }
    if !phone.trim().is_empty() {
        a.phone = phone.trim().to_string();
    }
    if !email.trim().is_empty() {
        a.email = email.trim().to_string();
    }
    if !kind.trim().is_empty() {
        a.kind = kind.trim().to_string();
    }
    if !edition.trim().is_empty() {
        a.edition = edition.trim().to_string();
    }
    if primary {
        a.is_primary = true;
    }
    let list = sources.entry(uid.to_string()).or_default();
    if !list.contains(&from) {
        list.push(from);
    }
}

/// 合并全部来源，返回本机已知的 WorkBuddy 账号（按 uid 升序）。
pub fn resolve() -> Vec<WbAccount> {
    let mut map: BTreeMap<String, WbAccount> = BTreeMap::new();
    let mut sources: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();

    // 全部已知 uid 先落座，保证「有账号但没名字」也能被列出
    for uid in known_uids() {
        map.entry(uid.clone()).or_insert_with(|| WbAccount {
            uid,
            ..Default::default()
        });
    }
    for (uid, name) in names_from_logs() {
        merge_account(&mut map, &mut sources, &uid, &name, "", "", "", "", "运行日志", false);
    }
    for e in accounts_json_entries() {
        merge_account(
            &mut map, &mut sources, &e.uid, &e.nickname, &e.phone, &e.email, &e.kind, &e.edition,
            "账号库", false,
        );
    }
    for e in own_vault_entries() {
        merge_account(
            &mut map, &mut sources, &e.uid, &e.nickname, &e.phone, &e.email, &e.kind, &e.edition,
            "本工具账号库", false,
        );
    }
    for e in account_snapshot_entries() {
        merge_account(
            &mut map, &mut sources, &e.uid, &e.nickname, &e.phone, &e.email, &e.kind, &e.edition,
            "账号快照", true,
        );
    }

    let mut out: Vec<WbAccount> = map.into_values().collect();
    for a in &mut out {
        a.name = a.best_name();
        a.source = sources
            .get(&a.uid)
            .map(|v| v.join("+"))
            .unwrap_or_else(|| "uid".to_string());
    }
    out
}

/// 单个 uid 的账号信息；查不到返回 `None`。
pub fn lookup(uid: &str) -> Option<WbAccount> {
    if uid.trim().is_empty() {
        return None;
    }
    resolve().into_iter().find(|a| a.uid == uid)
}

/// 一句可展示的标签：`13780001455（uid …97eac1）`。
///
/// 名字解析不出来时退化成 `uid …97eac1`，绝不返回空串。
pub fn label_for(uid: &str) -> String {
    match lookup(uid) {
        Some(a) => a.label(),
        None => {
            if uid.trim().is_empty() {
                "未知账号".to_string()
            } else {
                format!("uid …{}", uid_suffix(uid))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_like_guard() {
        assert!(is_uuid_like("63e05cca-cf7d-4dfa-af52-65168597eac1"));
        assert!(!is_uuid_like("changed: <init> -> <empty>"));
        assert!(!is_uuid_like(""));
        assert!(!is_uuid_like("63e05cca-cf7d-4dfa-af52-65168597eac"));
    }

    #[test]
    fn take_value_handles_log_escaping() {
        // 紧邻形态（正常 JSON）
        let t = "\"userId\":\"63e05cca-cf7d-4dfa-af52-65168597eac1\",\"username\":\"13780001455\"";
        let (v, end) = take_value(t, 0, "userId").unwrap();
        assert_eq!(v, "63e05cca-cf7d-4dfa-af52-65168597eac1");
        let (n, _) = take_value(t, end, "username").unwrap();
        assert_eq!(n, "13780001455");
    }

    #[test]
    fn take_value_handles_double_escaped_quotes() {
        // 日志里 JSON 被嵌进字符串后引号会变成 \" 甚至 \\"
        let t = r#":\"info\",\\"userId\\":\\"6aa2c43b-05f5-489a-840a-298f6cbe81ba\\",\\"username\\":\\"弦思\\""#;
        let (v, end) = take_value(t, 0, "userId").unwrap();
        assert_eq!(v, "6aa2c43b-05f5-489a-840a-298f6cbe81ba");
        let (n, _) = take_value(t, end, "username").unwrap();
        assert_eq!(n, "弦思");
    }

    #[test]
    fn scan_skips_plain_text_userid_line() {
        // `[AuthenticationManager] userId changed: <init> -> <empty>` 不是账号数据
        let mut out = BTreeMap::new();
        scan_uid_names(
            r#"[AuthenticationManager] userId changed: <init> -> <empty>"#,
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn scan_pairs_uid_with_nickname() {
        let text = r#"{\"qimei36\":\"abc\",\"userId\":\"fbd4139a-676d-4271-bcbe-8360aa5d5e70\",\"username\":\"x\",\"userNickname\":\"19550125362\"}"#;
        let mut out = BTreeMap::new();
        scan_uid_names(text, &mut out);
        assert_eq!(
            out.get("fbd4139a-676d-4271-bcbe-8360aa5d5e70").map(String::as_str),
            Some("19550125362")
        );
    }

    #[test]
    fn clamp_boundary_never_returns_a_mid_char_index() {
        let t = "abc工作工具";
        assert_eq!(clamp_boundary(t, 0), 0);
        assert_eq!(clamp_boundary(t, 3), 3); // '工' 起点，本身就是边界
        assert_eq!(clamp_boundary(t, 4), 3); // '工' 中间 → 退回起点
        assert_eq!(clamp_boundary(t, 5), 3);
        assert_eq!(clamp_boundary(t, 6), 6); // '作' 起点
        assert_eq!(clamp_boundary(t, 999), t.len()); // 越界夹到长度
        // 中文尾部：'具' 占 12..15，落在其中的下标都要退回 12
        assert_eq!(clamp_boundary(t, 13), 12);
        assert_eq!(clamp_boundary(t, 14), 12);
    }

    /// 回归（2026-10-08 线上真事）：`end + PAIR_WINDOW` 是**字节**偏移，
    /// 落在中文工具名 / 昵称中间时，`&text[end..window_end]` 会 panic ——
    /// 而那个 panic 会把 `workbuddy_vault::list()` 一起带走，界面变成「账号库还是空的」。
    ///
    /// 触发文件是 `~/.workbuddy/logs/daemon.log`（>4 MB，只读尾部 4 MB 那条路），
    /// 断点前正好是一串中文。
    #[test]
    fn scan_survives_window_ending_inside_a_multibyte_char() {
        // ⚠️ 前缀必须换**不同字节长度**：全是 3 字节中文时，`end + 400` 与字符边界
        // 的模 3 关系不变，永远撞不上（第一版夹具就是这么写错的，恒为 0 次命中）。
        let mut reproduced = 0;
        for lead in ["", "x", "xx", "xy", "😀", "abcde"] {
            let text = format!(
                r#"{{"qimei36":"abc","userId":"63e05cca-cf7d-4dfa-af52-65168597eac1","userNickname":"张萍","note":"{}{}"}}"#,
                lead,
                // 断点后是一长串 3 字节中文 —— 与 daemon.log 里那句中文工具名同形
                "工作工具".repeat(120),
            );
            let (_, end) = take_value(&text, 0, "userId").unwrap();
            let window_end = end + PAIR_WINDOW;
            // 只关心真的会踩到字符中间的那些形状
            if window_end >= text.len() || text.is_char_boundary(window_end) {
                continue;
            }
            reproduced += 1;
            // 修好之前这一行会 panic
            let mut out = BTreeMap::new();
            scan_uid_names(&text, &mut out);
            assert_eq!(
                out.get("63e05cca-cf7d-4dfa-af52-65168597eac1")
                    .map(String::as_str),
                Some("张萍"),
                "窗口被截断到字符边界后，昵称仍应解析出来"
            );
        }
        assert!(
            reproduced > 0,
            "夹具没能构造出「PAIR_WINDOW 落在多字节字符中间」的形状"
        );
    }

    #[test]
    fn label_falls_back_to_uid_suffix() {
        let a = WbAccount {
            uid: "63e05cca-cf7d-4dfa-af52-65168597eac1".to_string(),
            ..Default::default()
        };
        assert_eq!(a.label(), "uid …97eac1");
        assert_eq!(label_for(""), "未知账号");
    }

    #[test]
    fn label_prefers_nickname_and_meta_hides_duplicates() {
        let a = WbAccount {
            uid: "6aa2c43b-05f5-489a-840a-298f6cbe81ba".to_string(),
            nickname: "弦思".to_string(),
            phone: "18858464309".to_string(),
            kind: "personal".to_string(),
            edition: "free".to_string(),
            ..Default::default()
        };
        assert_eq!(a.label(), "弦思（uid …be81ba）");
        assert_eq!(a.meta(), "18858464309 · 个人版 · 免费版");

        // 昵称就是手机号时不能再重复显示一次
        let b = WbAccount {
            nickname: "13780001455".to_string(),
            phone: "13780001455".to_string(),
            ..Default::default()
        };
        assert_eq!(b.meta(), "");
    }

    #[test]
    fn encrypted_envelope_is_ignored() {
        // WorkBuddy 5.6 起昵称字段可能是加密信封对象，不能当字符串用
        let v: Value = serde_json::from_str(r#"{"nickname":{"$wbEncrypted":"zzz"}}"#).unwrap();
        assert_eq!(str_at(&v, "nickname"), "");
    }
}
