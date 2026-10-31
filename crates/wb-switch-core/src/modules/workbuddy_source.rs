//! WorkBuddy 会话来源：发现并解析本机 WorkBuddy 桌面版的会话记录。
//!
//! WorkBuddy 的会话由「数据三件套」定义（缺一不可）：
//!   1) 正文：`~/.workbuddy/projects/{工作区目录}/{会话id}.jsonl`（JSONL，一行一个事件）
//!   2) 元数据：`~/.workbuddy/workbuddy.db` → `sessions` 表（标题 / 工作区 / 归属账号 / 时间）
//!   3) 云端映射：`~/.workbuddy/edge-sync-mapping-v{N}.db`（本工具只读，不参与移植）
//!
//! **识别依据**：会话 id 就是正文 JSONL 的文件名词干（带连字符的 UUID），与 `sessions.id`
//! 完全一致。工作区子目录名只是分类，不参与识别——定位时遍历 `projects/` 下所有子目录。
//!
//! 正文事件类型：`session-meta` / `message`（user、assistant）/ `reasoning` /
//! `function_call` / `function_call_result` / `file-history-snapshot` / `ai-title`。
//! 一个「回合」= `message(user)` → 若干过程事件 → `message(assistant)`。
//!
//! 本模块只读：绝不修改 WorkBuddy 的任何文件。读取 `workbuddy.db` 时先复制
//! db + wal + shm 到工具目录再打开，避免与运行中的 WorkBuddy 争抢 WAL 锁。

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::modules::config::store_dir;

/// WorkBuddy 数据根目录名（位于用户主目录下）。
const WB_DIR_NAME: &str = ".workbuddy";

/// WorkBuddy 数据根：`~/.workbuddy`。
pub fn data_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(WB_DIR_NAME)
}

/// 能力探测：数据根同时具备 `projects/` 目录与 `workbuddy.db` 才算可用。
pub fn is_available() -> bool {
    let root = data_root();
    root.join("projects").is_dir() && root.join("workbuddy.db").is_file()
}

/// 会话列表项（元数据来自 `sessions` 表，正文有无来自磁盘探测）。
#[derive(Serialize, Clone, Debug)]
pub struct WbSession {
    /// 会话 id（UUID 带连字符）。
    pub id: String,
    /// 标题：优先 `custom_title`，回退 `title`。
    pub title: String,
    /// 工作目录（原样，未做归一化）。
    pub cwd: String,
    /// 归属账号 uid。
    pub user_id: String,
    /// 创建时间（毫秒）。
    pub created_at: i64,
    /// 最后更新时间（毫秒）。
    pub updated_at: i64,
    /// 模型名（可能为空）。
    pub model: String,
    /// 是否已删除（`deleted_at` 非空）。
    pub deleted: bool,
    /// 磁盘上是否存在正文 JSONL。
    pub has_body: bool,
    /// 正文文件字节数（无正文为 0）。
    pub body_bytes: u64,
    /// 正文行数（事件条数；无正文为 0）。
    pub body_lines: usize,
    /// 正文内容摘要（逐行摘要的汇总，前 16 位十六进制；无正文为空）。
    ///
    /// 归一化时会把「本副本自己的会话 id」替换成固定标记，于是同一段对话在两个
    /// 账号里的两份副本可以拿到**同一个摘要**——这正是「哪些是同一份、谁更新」的判据。
    pub content_digest: String,
    /// 同标题分组键（归一化标题；唯一标题时为空串）。
    pub dup_group: String,
    /// 同组内是否为最新副本（无重复时为 true）。
    pub is_newest: bool,
    /// 相对最新副本的差异说明（最新副本为「共 N 份重复」，无重复为空串）。
    pub dup_note: String,
    /// 与同标题最新副本的关系：`latest` / `equal` / `behind` / `ahead` / `diverged`
    /// / `no-body` / `unknown`（体积或成员数超限没做比对）/ 空串（无同标题副本）。
    ///
    /// 关联会话页靠它判断「这份副本是否只是落后」——`behind` 才能安全快进，
    /// `diverged` 必须让人选来源。
    pub dup_relation: String,
}

/// 同标题分组的判定上限：超过这个成员数就不做逐行摘要比对，
/// 只按 `updated_at` + 行数给结论（避免在大仓库上读上百个 10 MB 级正文）。
const DUP_MAX_MEMBERS: usize = 8;
/// 单个正文参与逐行摘要比对的体积上限。
///
/// 曾经从 64 MB 放宽到 256 MB（为了让最大的那份也参与比对），但实测代价不可接受：
/// 全账号互通之后同一段对话有 2–4 份副本，本机 20 个待比对文件合计 **1.06 GB**，
/// 其中 4 份各 **227 MB** 是同一段对话 —— 一次列表要读 1 GB、耗时 35 秒。
///
/// 现在收紧到 32 MB：超过即「不逐行比对」，界面如实显示
/// 「有副本体积超过 32 MB，未做逐行比对」（这本来就是一个明确状态，不是猜）。
/// 超过 32 MB 的会话，副本新旧只按 `updated_at` 判断 —— 对这么大的对话，
/// 逐行比对能给出的价值远小于它带来的等待。
const DUP_MAX_BYTES: u64 = 32 << 20;

/// 正文内容身份：行数 + 逐行摘要 + 总摘要。
///
/// 逐行摘要对「长度编码 + 归一化行字节」取 SHA-256，记录分隔无歧义；行顺序、重复次数
/// 都参与摘要，因此重排、去重、重写都会得到不同的内容身份。
#[derive(Debug, Clone)]
struct ContentId {
    lines: usize,
    line_digests: Vec<String>,
    total: String,
}

/// 两份正文的关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// 逐行完全相同
    Equal,
    /// 前者是后者的严格有序前缀（后者更新）
    Prefix,
    /// 后者是前者的严格有序前缀（前者更新）
    Extension,
    /// 内容分叉（同源但各写各的）
    Diverged,
}

/// 行摘要：长度编码 + 归一化后的行字节。
fn line_digest(line: &str) -> String {
    use sha2::{Digest, Sha256};
    let bytes = line.as_bytes();
    let mut h = Sha256::new();
    h.update((bytes.len() as u64).to_be_bytes());
    h.update(bytes);
    to_hex(&h.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 由有序行摘要推导总摘要（与 `~/.wb-switch` 的 session_link 同构，
/// 便于用同一套口径解释「内容是否一致」）。
fn total_digest(line_digests: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"wb-switch-lines-v1\0");
    h.update((line_digests.len() as u64).to_be_bytes());
    for d in line_digests {
        h.update(d.as_bytes());
        h.update([0u8]);
    }
    to_hex(&h.finalize())
}

/// 归一化会话 id 时用的固定标记。
const SESSION_MARKER: &str = "__wb_session_id__";

/// 把**整组**副本的会话 id 都换成同一个标记。
///
/// ⚠️ 只换「自己那个」不够 —— 正文里可能引用**别的副本的 id 文本**。本机实测：
/// 一段对话里打印过同组另外三个 id（各 30~40 处），于是 A 的副本里留着 B 的原样 id，
/// 而 B 的副本里那处也被换成了自己的 id ⇒ 两边归一化结果不同 ⇒ **刚同步完就报「有分歧」**。
/// 所以这里按整组 id 集合归一化：任何一方的 id 都变成同一个标记，谁引用谁都不影响比对。
fn normalize_ids(line: &str, ids: &[String]) -> String {
    let mut out = std::borrow::Cow::Borrowed(line);
    for id in ids {
        if id.is_empty() || !out.contains(id.as_str()) {
            continue;
        }
        out = std::borrow::Cow::Owned(out.replace(id.as_str(), SESSION_MARKER));
    }
    out.into_owned()
}

/// 正文摘要缓存（进程内）。
///
/// ## 为什么必须有
///
/// [`annotate_duplicates`] 对每一组「同标题副本」都要**逐行读正文**算摘要，而
/// `DUP_MAX_BYTES` 是 256 MB（基本等于不限制）。全账号互通之后本机同一段对话常有
/// 2–4 份副本 —— 实测 40 条会话一次列表要 **24 秒**，界面表现就是「每次点进会话页
/// 都在重载」，而且一次进入页面会跑两三遍（列表 + 一键互通预览 + 关联预检）。
///
/// ## 为什么这样缓存是安全的
///
/// 摘要只取决于「文件内容」和「参与归一化的 id 集」，所以键就是
/// `路径 + 长度 + 修改时间 + id 集`：文件被编辑/替换/重写、或组成员变了 ⇒ 键变 ⇒
/// 自动重算。**不设过期时间**——过期时间只会让「刚改完的会话」读到旧摘要，
/// 反而制造出「显示与内容不一致」这种最难查的问题。
/// 摘要缓存的容量上限（估算值；超了就整体清空重来）。
const MAX_CACHED_BYTES: usize = 256 << 20;

struct DigestCache {
    map: std::collections::HashMap<String, ContentId>,
    bytes: usize,
}

fn digest_cache() -> &'static std::sync::Mutex<DigestCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<DigestCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        std::sync::Mutex::new(DigestCache {
            map: std::collections::HashMap::new(),
            bytes: 0,
        })
    })
}

/// 缓存键：`路径|长度|mtime|id集哈希`。
///
/// 任一项变了就 miss —— 文件被改写/替换、或组成员变了，都必须重算。
/// id 集用「排序后哈希」入键：组内顺序不影响归一化结果，避免仅仅换个顺序就 miss。
fn cache_key(path: &std::path::Path, ids: &[String]) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut ids_sorted: Vec<&str> = ids.iter().map(String::as_str).collect();
    ids_sorted.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ids_sorted.hash(&mut h);
    Some(format!(
        "{}|{}|{}|{:x}",
        path.to_string_lossy(),
        meta.len(),
        mtime,
        h.finish()
    ))
}

/// **只查不算**：缓存里有就返回，没有就 `None`（绝不读盘）。
///
/// 用来回答「这一组能不能直接下结论」：摘要都在、且文件键没变 ⇒ 逐行比对的结论
/// 是现成的，没有理由再读一遍，更没有理由把它退回「未比对」。
fn peek_content_id(path: &std::path::Path, ids: &[String]) -> Option<ContentId> {
    let key = cache_key(path, ids)?;
    digest_cache().lock().ok()?.map.get(&key).cloned()
}

fn content_id_cached(path: &std::path::Path, ids: &[String]) -> Option<ContentId> {

    let key = cache_key(path, ids)?;
    if let Some(hit) = digest_cache().lock().ok().and_then(|g| g.map.get(&key).cloned()) {
        return Some(hit);
    }
    let id = content_id(path, ids)?;
    // 估算：每条行摘要约 48 字节 + 结构体开销
    let cost = id.line_digests.len() * 48 + id.total.len() + 64;
    if let Ok(mut guard) = digest_cache().lock() {
        if guard.bytes + cost > MAX_CACHED_BYTES {
            guard.map.clear();
            guard.bytes = 0;
        }
        guard.bytes += cost;
        guard.map.insert(key, id.clone());
    }
    Some(id)
}

fn content_id(path: &std::path::Path, ids: &[String]) -> Option<ContentId> {
    use std::io::BufRead;
    // ⚠️ 必须**流式**逐行读：会话正文单个可达 200 MB 以上（实测本机有一份 216 MB），
    // 一次 `read_to_string` 会把整份文本 + 行摘要同时压在内存里。逐行读只保留摘要，
    // 内存与文件大小无关。
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::with_capacity(1 << 20, file);
    let mut line_digests = Vec::new();
    for raw in reader.split(b'\n') {
        let bytes = raw.ok()?;
        let line = String::from_utf8(bytes).ok()?;
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.trim().is_empty() {
            continue;
        }
        // 非 JSON 行也算一条记录（保守：宁可判定为「内容不同」也不视为一致）
        line_digests.push(line_digest(&normalize_ids(&line, ids)));
    }
    if line_digests.is_empty() {
        return None;
    }
    let total = total_digest(&line_digests);
    Some(ContentId {
        lines: line_digests.len(),
        line_digests,
        total,
    })
}

fn is_ordered_prefix(prefix: &[String], full: &[String]) -> bool {
    prefix.len() <= full.len() && full[..prefix.len()] == *prefix
}

fn relate(a: &ContentId, b: &ContentId) -> Relation {
    if a.lines == b.lines && a.total == b.total {
        return Relation::Equal;
    }
    if is_ordered_prefix(&a.line_digests, &b.line_digests) {
        return Relation::Prefix;
    }
    if is_ordered_prefix(&b.line_digests, &a.line_digests) {
        return Relation::Extension;
    }
    Relation::Diverged
}

/// 标题归一化：小写、压缩空白、去掉 WorkBuddy 列表里的截断省略号。
///
/// 只用于**分组**（同标题才需要比对），不参与任何写库。
fn norm_title(title: &str) -> String {
    let mut s = title.trim().to_lowercase();
    for suffix in ['…', '⋯'] {
        while s.ends_with(suffix) {
            s.pop();
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 纯决策：同标题一组里哪一份是最新，其余各差多少。
///
/// 判定顺序（与 `~/.wb-switch` 的 session_link 同一思路）：
/// 1. **内容包含关系优先于时间**：某一份的逐行摘要是其它所有成员的有序前缀
///    ⇒ 那一份最新（它把别人的历史全包含了）。即使它的 `updated_at` 更旧，
///    也以内容为准——时钟在多账号/多设备下并不可靠。
/// 2. 没有这样的成员（内容分叉）⇒ 按 `updated_at`，再按行数。
/// 3. 逐行摘要完全相同 ⇒ 走第 2 条，并在说明里点明「内容完全相同」。
///
/// 返回 `(最新成员下标, 每个成员相对最新的说明)`。
fn decide_group(ids: &[Option<ContentId>], updated_at: &[i64]) -> (usize, Vec<String>) {
    // 「覆盖其余所有成员」的候选。逐行完全相同的副本会互相覆盖 ⇒ 候选不止一个，
    // 这时不能靠位置先后定输赢，必须回落到时间戳（否则最新的是列表里靠前的那份，
    // 而它可能恰恰是旧的）。
    let covering: Vec<usize> = (0..ids.len())
        .filter(|&pos| {
            let Some(cid) = ids[pos].as_ref() else {
                return false;
            };
            ids.iter().enumerate().all(|(other, oid)| {
                if other == pos {
                    return true;
                }
                match oid {
                    Some(o) => {
                        o.lines <= cid.lines && is_ordered_prefix(&o.line_digests, &cid.line_digests)
                    }
                    // 没有正文的成员不算「被包含」，否则会把空壳选成最新
                    None => true,
                }
            })
        })
        .collect();

    let pick_by_clock = |pool: &[usize]| -> usize {
        let mut best = pool[0];
        for &pos in &pool[1..] {
            let better = (updated_at[pos], ids[pos].as_ref().map(|c| c.lines).unwrap_or(0))
                > (updated_at[best], ids[best].as_ref().map(|c| c.lines).unwrap_or(0));
            if better {
                best = pos;
            }
        }
        best
    };

    let newest_pos = if covering.len() == 1 {
        covering[0]
    } else if !covering.is_empty() {
        pick_by_clock(&covering)
    } else {
        let all: Vec<usize> = (0..ids.len()).collect();
        pick_by_clock(&all)
    };

    let newest_id = ids[newest_pos].clone();
    let notes = ids
        .iter()
        .enumerate()
        .map(|(pos, id)| {
            if pos == newest_pos {
                return String::new(); // 由调用方填「共 N 份」
            }
            match (id, &newest_id) {
                (Some(me), Some(top)) => match relate(me, top) {
                    Relation::Equal => "与最新副本内容完全相同".to_string(),
                    Relation::Prefix => format!("比最新副本少 {} 条记录", top.lines - me.lines),
                    Relation::Extension => {
                        format!("比最新副本多 {} 条记录", me.lines - top.lines)
                    }
                    Relation::Diverged => {
                        format!("内容分叉（本副本 {} 条 / 最新 {} 条）", me.lines, top.lines)
                    }
                },
                (None, _) => "无正文，不参与比对".to_string(),
                (Some(me), None) => format!("{} 条记录（最新副本无正文）", me.lines),
            }
        })
        .collect();
    (newest_pos, notes)
}

// ---------------------------------------------------------------------------
// 同步后的回读校验 / 「已校验一致」记录
// ---------------------------------------------------------------------------

/// 一份文件的内容摘要（`digest` 为 16 位十六进制，与 `WbSession::content_digest` 同口径）。
pub struct VerifyReport {
    /// 全部一致（且至少有一份）。
    pub all_same: bool,
    /// 行数（以第一份为准）。
    pub lines: usize,
    /// 摘要前 16 位。
    pub digest: String,
    /// 与第一份不一致的下标（空 = 全部一致）。
    pub mismatched: Vec<usize>,
    pub elapsed_ms: u64,
}

/// 逐个**读回**算出摘要，判断这些文件内容是否一致。
///
/// ⚠️ 这一步是真读盘，不是拿「我刚写过」当结论。用户反复觉得「同步没生效」，
/// 正是因为界面只会说「无法确认」——读一遍换一个确定的答案，值得。
/// 读完摘要会留在缓存里，紧接着刷新界面时不必再读一遍。
pub fn verify_same_content(paths: &[PathBuf], ids: &[String]) -> VerifyReport {
    let t0 = std::time::Instant::now();
    let mut base: Option<(usize, String)> = None;
    let mut mismatched: Vec<usize> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        match content_id_cached(p, ids) {
            Some(cid) => match &base {
                None => base = Some((cid.lines, cid.total.clone())),
                Some((lines, total)) => {
                    if *lines != cid.lines || *total != cid.total {
                        mismatched.push(i);
                    }
                }
            },
            // 读不出来也算不一致：绝不能把「读失败」当成「一样」。
            None => mismatched.push(i),
        }
    }
    let (lines, digest) = base.unwrap_or((0, String::new()));
    VerifyReport {
        all_same: mismatched.is_empty() && !paths.is_empty() && !digest.is_empty(),
        lines,
        digest: digest.chars().take(16).collect(),
        mismatched,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    }
}

/// 一条记录：某个会话正文最后一次**被回读校验**时的文件指纹与内容摘要。
///
/// 为什么需要它：超过 [`DUP_MAX_BYTES`] 的组默认不比对，界面上只能写「无法确认」。
/// 但用户刚点过同步、我们也真的读回来校验过 —— 这个结论不该因为「刷新了一下列表」
/// 或者「重启了工具」就丢掉，否则用户只会看到同一句话反复出现，以为同步没生效。
///
/// 判据严格：`长度 + mtime` 必须与记录逐项相同。文件被改过（哪怕只追加一条）
/// mtime 就变 ⇒ 记录失效 ⇒ 老老实实退回「未比对」。**绝不拿旧结论冒充当前状态。**
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VerifiedEntry {
    len: u64,
    mtime: u128,
    lines: usize,
    digest: String,
    at: i64,
}

fn verified_store_path() -> PathBuf {
    store_dir().join("workbuddy").join("verified-bodies.json")
}

fn file_stamp(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

fn load_verified_at(path: &Path) -> std::collections::HashMap<String, VerifiedEntry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_verified_at(path: &Path, map: &std::collections::HashMap<String, VerifiedEntry>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 只留最近 200 条：这是「免重复读」的备忘，不是账本。
    let mut items: Vec<(&String, &VerifiedEntry)> = map.iter().collect();
    if items.len() > 200 {
        items.sort_by_key(|(_, e)| std::cmp::Reverse(e.at));
        items.truncate(200);
    }
    let trimmed: std::collections::HashMap<&String, &VerifiedEntry> = items.into_iter().collect();
    if let Ok(text) = serde_json::to_string_pretty(&trimmed) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// 同步后回读校验通过 ⇒ 把「会话 id → 文件指纹 + 摘要」登记下来。
pub fn mark_verified(pairs: &[(String, PathBuf)], lines: usize, digest: &str) -> Result<(), String> {
    mark_verified_at(&verified_store_path(), pairs, lines, digest)
}

fn mark_verified_at(
    path: &Path,
    pairs: &[(String, PathBuf)],
    lines: usize,
    digest: &str,
) -> Result<(), String> {
    if digest.is_empty() {
        return Err("摘要为空，不能登记「已校验」".to_string());
    }
    let mut map = load_verified_at(path);
    let at = chrono::Local::now().timestamp_millis();
    for (id, p) in pairs {
        let Some((len, mtime)) = file_stamp(p) else {
            continue;
        };
        map.insert(
            id.clone(),
            VerifiedEntry {
                len,
                mtime,
                lines,
                digest: digest.to_string(),
                at,
            },
        );
    }
    save_verified_at(path, &map);
    Ok(())
}

/// 这些副本是不是「上次同步时回读校验过、之后谁都没动过」的同一份内容？
///
/// 任一份没有记录、或文件指纹变了，就返回 `None`（= 老实说不知道）。
pub fn verified_after_sync(pairs: &[(String, PathBuf)]) -> Option<(usize, String)> {
    verified_after_sync_at(&verified_store_path(), pairs)
}

fn verified_after_sync_at(path: &Path, pairs: &[(String, PathBuf)]) -> Option<(usize, String)> {
    if pairs.len() < 2 {
        return None;
    }
    let map = load_verified_at(path);
    let mut lines = 0usize;
    let mut digest = String::new();
    for (id, p) in pairs {
        let e = map.get(id)?;
        let (len, mtime) = file_stamp(p)?;
        if e.len != len || e.mtime != mtime {
            return None;
        }
        if digest.is_empty() {
            lines = e.lines;
            digest = e.digest.clone();
        } else if e.digest != digest {
            return None;
        }
    }
    if digest.is_empty() {
        None
    } else {
        Some((lines, digest))
    }
}

/// 强制比对完成后把**每个成员自己的**结论落盘（不要求全组一致）。
///
/// 与 [`mark_verified`] 的区别：那里只在「同步后回读校验一致」时登记，
/// 每条存的是**整组共同**的摘要；这里存的是各文件自己的摘要 ——
/// 全组一致时四条摘要自然相同（[`verified_after_sync`] 照样能命中），
/// 不一致时也能把「内容不同」这个结论保下来，重启工具后不再退回「未比对」。
///
/// 输入是「会话 id、正文路径、行数、摘要前 16 位」四元组（**不重新读盘**，
/// 摘要由调用方从刚算完的列表里带来）；摘要为空的成员跳过（没有可信结论可存）。
/// 返回实际登记的条数。
pub fn mark_compared(values: &[(String, PathBuf, usize, String)]) -> usize {
    mark_compared_at(&verified_store_path(), values)
}

fn mark_compared_at(
    path: &Path,
    values: &[(String, PathBuf, usize, String)],
) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut map = load_verified_at(path);
    let at = chrono::Local::now().timestamp_millis();
    let mut n = 0;
    for (id, p, lines, digest) in values {
        if digest.is_empty() {
            continue;
        }
        let Some((len, mtime)) = file_stamp(p) else {
            continue;
        };
        map.insert(
            id.clone(),
            VerifiedEntry {
                len,
                mtime,
                lines: *lines,
                digest: digest.clone(),
                at,
            },
        );
        n += 1;
    }
    save_verified_at(path, &map);
    n
}

/// 这些副本是否都留有「上次比对」的落盘结论，且**每份文件的指纹都没变**？
///
/// 全部命中才返回 `Some`（按传入顺序给出各成员的 `行数 + 摘要`）；
/// 任一份没有记录或被改过就返回 `None` —— 老实说不知道，绝不拿旧结论冒充。
///
/// 与 [`verified_after_sync`] 的区别：那条只在**全部摘要相同**时命中，
/// 这条把「摘要不同」也带回来，由调用方决定怎么呈现。
/// 从落盘结论里取每个成员自己的摘要 —— **允许部分命中**。
///
/// ⚠️ 这里原来要求「全组每份文件的 len/mtime 都与记录一模一样」，任一成员变一下
/// （典型：某个账号刚发了一轮对话，jsonl 被追加）就整组返回 `None`，于是
/// 4 个账号一起从「内容一致」退回「未比对」，又得重新强制比对 —— 用户 2026-10-30 报的就是这个。
///
/// 现在的口径：
/// - 返回长度恒等于 `pairs.len()`，`Some((行数, 摘要))` = 这份没变、结论仍然有效；
/// - `None` = 这份在上次比对后被改过，**它自己**的结论作废，但不连累其它成员；
/// - 命中 0 份时返回 `None`（一份可信结论都没有，不能下任何判断）。
///   ⚠️ 早期版本要求「≥2 份命中」，于是 2 成员的组里只要有一份变了，另一份的
///   结论也被一起丢掉（整组退回「无法确认」）—— 同样是 2026-10-30 那个反馈。
pub fn compared_from_disk(
    pairs: &[(String, PathBuf)],
) -> Option<Vec<Option<(usize, String)>>> {
    compared_from_disk_at(&verified_store_path(), pairs)
}

fn compared_from_disk_at(
    path: &Path,
    pairs: &[(String, PathBuf)],
) -> Option<Vec<Option<(usize, String)>>> {
    if pairs.len() < 2 {
        return None;
    }
    let map = load_verified_at(path);
    let mut hit = 0usize;
    let mut out: Vec<Option<(usize, String)>> = Vec::with_capacity(pairs.len());
    for (id, p) in pairs {
        let ok = (|| -> Option<(usize, String)> {
            let e = map.get(id)?;
            let (len, mtime) = file_stamp(p)?;
            if e.len != len || e.mtime != mtime {
                return None; // 这份文件变了 ⇒ 只有它的结论作废
            }
            if e.digest.is_empty() {
                return None;
            }
            Some((e.lines, e.digest.clone()))
        })();
        if ok.is_some() {
            hit += 1;
        }
        out.push(ok);
    }
    if hit == 0 {
        return None;
    }
    Some(out)
}

/// 内容既然已经一致，「谁最新」就只能按更新时间定，其余一律 `equal`。
///
/// 抽成纯函数是为了能单测：这条判据一旦歪掉，界面就会重新长出
/// 「明明一致却说有副本落后」那种自相矛盾的结论。
fn synced_relations(updated_at: &[i64]) -> (usize, Vec<&'static str>) {
    let mut newest = 0usize;
    for pos in 1..updated_at.len() {
        if updated_at[pos] > updated_at[newest] {
            newest = pos;
        }
    }
    let rels = (0..updated_at.len())
        .map(|pos| if pos == newest { "latest" } else { "equal" })
        .collect();
    (newest, rels)
}

/// 一组的客观事实（判据的输入；全是「已算好的布尔值」，不含 IO）。
#[derive(Debug, Clone, Copy)]
struct GroupFacts {
    members: usize,
    all_body: bool,
    all_fit: bool,
    all_cached: bool,
    recorded: bool,
}

/// 这一组该怎么下结论。
#[derive(Debug, PartialEq, Eq)]
enum Decide {
    /// 用户点了「强制比对」：不管上限，真读。
    Forced,
    /// 体积/成员数都在上限内：逐行比对（常规路径）。
    Compare,
    /// 体积超限，但摘要已在缓存（强制比对过 / 同步回读校验算过）⇒ 直接用。
    ReuseCache,
    /// 有落盘的「同步后回读校验一致」记录，且文件指纹没变 ⇒ 直接用。
    ReuseRecord,
    /// 只能说不知道（成员太多 / 缺正文 / 体积超限且什么都没算过）。
    Unknown,
}

/// 纯判据：**顺序即优先级**，别随意调换。
///
/// - `Forced` 永远最优先（用户明确要求真读，缓存再新也要重算）；
/// - 成员数超上限时直接放弃（读 9 份 200 MB 不是「耐心」问题）；
/// - 「体积合规」「摘要已缓存」「有校验记录」三者任一成立就能给结论。
///
/// 抽出来是因为这里一旦写歪，界面就会重新出现「明明一致却写未比对」
/// 或更糟的「没比对却说一致」——后者是绝对不能有的。
fn decide_mode(force: bool, f: &GroupFacts) -> Decide {
    if force {
        return Decide::Forced;
    }
    if f.members > DUP_MAX_MEMBERS || !f.all_body {
        return Decide::Unknown;
    }
    if f.all_fit {
        return Decide::Compare;
    }
    if f.all_cached {
        return Decide::ReuseCache;
    }
    if f.recorded {
        return Decide::ReuseRecord;
    }
    Decide::Unknown
}

/// 用「已校验一致」的落盘结论直接给整组下结论（不读一个字节）。
///
/// `note` 说明结论来历（「同步后回读校验」还是「上次强制比对」），界面原样展示 ——
/// 用户看到「一致」时必须能知道这是谁说的。
fn apply_synced_group(
    list: &mut [WbSession],
    idx: &[usize],
    key: &str,
    lines: usize,
    digest: &str,
    note: &str,
) {
    let stamps: Vec<i64> = idx.iter().map(|&i| list[i].updated_at).collect();
    let (newest, rels) = synced_relations(&stamps);
    let label: String = key.chars().take(16).collect();
    for (pos, &i) in idx.iter().enumerate() {
        list[i].dup_group = label.clone();
        list[i].body_lines = lines;
        list[i].content_digest = digest.to_string();
        list[i].is_newest = pos == newest;
        list[i].dup_relation = rels[pos].to_string();
        list[i].dup_note = if pos == newest {
            format!("共 {} 份重复标题副本", idx.len())
        } else {
            note.to_string()
        };
    }
}

/// 用「上次比对（不要求一致）」的落盘结论下结论：全同 → 与同步回读同形状；
/// 不同 → 只能如实说「内容不同」——落盘的只有整份摘要，没有行级摘要，
/// 判不出包含关系（谁落后于谁），绝不冒充更细的结论。
///
/// ⚠️ `per` 允许部分为 `None`（那份文件在上次比对后被改过）。此时**只作废它自己**的结论，
/// 其余成员照常复用；被作废的那份标 `stale`，组状态记为 `pending`。
/// 早期版本要求全组指纹完全一致，于是「某个账号刚说了一句话」就会让 4 份副本一起退回
/// 「未比对」—— 这才是用户反复重新比对的真正原因。
fn apply_compared_from_disk(
    list: &mut [WbSession],
    idx: &[usize],
    key: &str,
    per: &[Option<(usize, String)>],
) {
    // 基准 = 「最新的那一份**有结论**的成员」的摘要。
    let mut newest = 0usize;
    let mut newest_at = i64::MIN;
    let mut any = false;
    for (pos, &i) in idx.iter().enumerate() {
        if per[pos].is_some() && list[i].updated_at > newest_at {
            newest_at = list[i].updated_at;
            newest = pos;
            any = true;
        }
    }
    if !any {
        return;
    }
    let base = per[newest].as_ref().map(|(_, d)| d.clone()).unwrap_or_default();
    // 全部命中且摘要全同 ⇒ 与「同步后回读校验一致」同形状（文案也保持一致）。
    let hit = per.iter().flatten().count();
    if hit == per.len() && per.iter().flatten().all(|(_, d)| *d == base) {
        let lines = per[newest].as_ref().map(|(l, _)| *l).unwrap_or(0);
        apply_synced_group(
            list,
            idx,
            key,
            lines,
            &base,
            "上次强制比对逐行一致（文件未再改动）",
        );
        return;
    }
    let label: String = key.chars().take(16).collect();
    for (pos, &i) in idx.iter().enumerate() {
        list[i].dup_group = label.clone();
        match &per[pos] {
            Some((lines, digest)) => {
                list[i].body_lines = *lines;
                list[i].content_digest = digest.chars().take(16).collect();
                list[i].is_newest = pos == newest;
                list[i].dup_relation = if pos == newest {
                    "latest"
                } else if *digest == base {
                    "equal"
                } else {
                    "diverged"
                }
                .to_string();
                list[i].dup_note = if pos == newest {
                    format!("共 {} 份重复标题副本（上次比对的结论）", idx.len())
                } else if *digest == base {
                    "与最新副本内容相同（上次强制比对的结论）".to_string()
                } else {
                    "上次强制比对：内容不同。要判定谁包含谁，请再次强制比对".to_string()
                };
            }
            None => {
                // 这份在上次比对之后有新内容 ⇒ 只有它的结论作废。
                list[i].is_newest = false;
                list[i].content_digest.clear();
                list[i].dup_relation = "stale".to_string();
                list[i].dup_note =
                    "上次比对之后这份有新内容，未重新比对（可点「强制比对」）".to_string();
            }
        }
    }
}

/// 给列表补上「同标题副本」信息：哪一份最新、其余各差多少。
///
/// 读盘（逐行摘要）只对**同标题分组**内的成员做，且限制成员数与单文件体积，
/// 避免在大仓库上一次性读上百个 10 MB 级正文。
fn annotate_duplicates(list: &mut [WbSession]) {
    annotate_duplicates_opt(list, false)
}

/// [`annotate_duplicates`] 的带开关版本。
///
/// `force = true` 时**忽略体积上限与成员数上限**，真的逐行读完整正文做比对。
///
/// 为什么要这个开关：体积超限时默认不比对（一份 227 MB 的正文读一次要好几秒，
/// 20 个副本一次列表就 1 GB / 35 秒 —— 不能让「打开列表」被它拖死）。
/// 但用户明确要求「支持手动确定后执行判定分析」：他自己决定等，就真的要比。
/// 所以这里是**用户显式触发**才走的慢路径，默认路径的性能不受影响。
fn annotate_duplicates_opt(list: &mut [WbSession], force: bool) {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, s) in list.iter().enumerate() {
        let key = norm_title(&s.title);
        if key.is_empty() {
            continue;
        }
        groups.entry(key).or_default().push(i);
    }

    for (key, idx) in groups {
        if idx.len() < 2 {
            continue;
        }
        // 逐行摘要只对「有正文且在体积上限内」的成员计算
        let mut ids: Vec<Option<ContentId>> = Vec::with_capacity(idx.len());
        // ⚠️ 归一化要用**整组**的 id：正文可能引用组内其它副本的 id（实测有过），
        // 只换自己那个会让「刚同步完的副本」互相比对不上，误报「有分歧」。
        let group_ids: Vec<String> = idx.iter().map(|&i| list[i].id.clone()).collect();
        // ---- 这一组到底能不能下结论 -------------------------------------------
        // 事实先算出来（贵的两项只在「体积超限」时才去碰，常见路径零额外 IO）。
        let all_body = idx.iter().all(|&i| list[i].has_body);
        let all_fit = all_body && idx.iter().all(|&i| list[i].body_bytes <= DUP_MAX_BYTES);
        let mut all_cached = false;
        let mut recorded: Option<(usize, String)> = None;
        let mut from_disk: Option<Vec<Option<(usize, String)>>> = None;
        if !force && all_body && !all_fit && idx.len() <= DUP_MAX_MEMBERS {
            // ① 摘要是否都已在缓存里（强制比对 / 同步回读校验留下的）——只查不读盘。
            all_cached = idx.iter().all(|&i| {
                find_jsonl(&list[i].id)
                    .map(|p| peek_content_id(&p, &group_ids).is_some())
                    .unwrap_or(false)
            });
            // ② 有没有「同步后回读校验一致」的记录，且每份文件的长度/mtime 与记录逐项相同。
            if !all_cached {
                let pairs: Vec<(String, PathBuf)> = idx
                    .iter()
                    .filter_map(|&i| {
                        find_jsonl(&list[i].id).map(|p| (list[i].id.clone(), p))
                    })
                    .collect();
                if pairs.len() == idx.len() {
                    recorded = verified_after_sync(&pairs);
                    // ③ 落盘的「上次比对」结论（强制比对留下的，不要求一致）：
                    //    指纹没变就直接复用，重启工具也不退回「未比对」。
                    if recorded.is_none() {
                        from_disk = compared_from_disk(&pairs);
                    }
                }
            }
        }
        let mode = decide_mode(
            force,
            &GroupFacts {
                members: idx.len(),
                all_body,
                all_fit,
                all_cached,
                recorded: recorded.is_some(),
            },
        );
        if mode == Decide::ReuseRecord {
            if let Some((lines, digest)) = recorded {
                apply_synced_group(
                    list,
                    &idx,
                    &key,
                    lines,
                    &digest,
                    "已与来源同步，回读校验一致（文件未再改动）",
                );
                continue;
            }
        }
        // 内存缓存没命中、但落盘结论还在（指纹没变）⇒ 不读一个字节直接复用。
        // 只在非强制路径生效：强制比对永远重算，绝不吃旧结论。
        if !force {
            if let Some(per) = from_disk {
                apply_compared_from_disk(list, &idx, &key, &per);
                continue;
            }
        }
        let detailed = matches!(mode, Decide::Forced | Decide::Compare | Decide::ReuseCache);
        for &i in &idx {
            if !list[i].has_body {
                ids.push(None);
                continue;
            }
            if detailed {
                let id = find_jsonl(&list[i].id)
                    .and_then(|p| content_id_cached(&p, &group_ids));
                ids.push(id);
            } else {
                ids.push(None);
            }
            if let (Some(cid), j) = (ids.last().and_then(|v| v.as_ref()), i) {
                list[j].body_lines = cid.lines;
                list[j].content_digest = cid.total.chars().take(16).collect();
            }
        }

        let updated: Vec<i64> = idx.iter().map(|&i| list[i].updated_at).collect();
        let (newest_pos, mut notes) = decide_group(&ids, &updated);
        notes[newest_pos] = format!("共 {} 份重复标题副本", idx.len());
        if !detailed {
            // 没做逐行比对时，把**为什么没做**写进备注：界面显示「无法确认」时必须
            // 能回答「为什么」，否则用户只会觉得工具坏了。
            let why = if idx.iter().any(|&i| !list[i].has_body) {
                "有副本没有正文文件，未做逐行比对".to_string()
            } else if idx.len() > DUP_MAX_MEMBERS {
                format!(
                    "同标题副本有 {} 份（比对上限 {} 份），未做逐行比对",
                    idx.len(),
                    DUP_MAX_MEMBERS
                )
            } else {
                format!(
                    "有副本体积超过 {} MB，未做逐行比对（可点「强制比对」逐行分析）",
                    DUP_MAX_BYTES >> 20
                )
            };
            for n in notes.iter_mut() {
                *n = why.clone();
            }
        }

        // 每个成员相对最新副本的关系。`behind` = 有序前缀（可安全快进），
        // `diverged` = 各写各的（必须人工选来源）。没做逐行比对时一律 `unknown`，
        // 绝不能因为「没算」就说成「无正文」。
        let rels: Vec<&'static str> = if !detailed {
            vec!["unknown"; ids.len()]
        } else {
            let top = ids.get(newest_pos).and_then(|v| v.as_ref());
            ids.iter()
                .enumerate()
                .map(|(pos, id)| match (id, top) {
                    (None, _) => "no-body",
                    (Some(_), None) => "sole-body",
                    (Some(_), Some(_)) if pos == newest_pos => "latest",
                    (Some(me), Some(top)) => match relate(me, top) {
                        Relation::Equal => "equal",
                        Relation::Prefix => "behind",
                        Relation::Extension => "ahead",
                        Relation::Diverged => "diverged",
                    },
                })
                .collect()
        };

        let group_label: String = key.chars().take(16).collect();
        for (pos, &i) in idx.iter().enumerate() {
            list[i].dup_group = group_label.clone();
            list[i].is_newest = pos == newest_pos;
            list[i].dup_note = std::mem::take(&mut notes[pos]);
            list[i].dup_relation = rels[pos].to_string();
        }
    }

    // 非重复项：没有分组，视为「唯一副本」
    for s in list.iter_mut() {
        if s.dup_group.is_empty() {
            s.is_newest = true;
            s.dup_note = String::new();
        }
    }
}

/// 一条过程事件（思考 / 工具调用 / 工具结果），按原始顺序保留。
#[derive(Serialize, Clone, Debug)]
pub struct WbEvent {
    /// `reasoning` / `function_call` / `function_call_result`。
    pub kind: String,
    /// 工具名（思考事件为空）。
    pub name: String,
    /// 工具调用 id（用于把 call 与 result 配对）。
    pub call_id: String,
    /// 结果状态（`function_call_result` 才有）。
    pub status: String,
    /// 文本内容：思考正文 / 调用参数 / 结果文本。
    pub text: String,
}

/// 一个回合：一条用户提问 + 对应的助手最终回答 + 中间过程事件。
///
/// WorkBuddy 的一个回合里，助手会**多次**发 `message`（过程中间叙述），最后一条才是
/// 最终回答。`assistant_text` 保存最后那条（= 最终回答），全部中间叙述连同思考与工具
/// 调用按原始顺序进入 `events`，以保证完整还原过程。
#[derive(Serialize, Clone, Debug, Default)]
pub struct WbTurn {
    pub user_text: String,
    pub assistant_text: String,
    pub events: Vec<WbEvent>,
    /// 回合起始时间（毫秒），取用户消息的时间戳。
    pub created_at: i64,
    /// 回合结束时间（毫秒），取该回合最后一条事件的时间戳。
    pub updated_at: i64,
}

/// 一个会话的完整正文。
#[derive(Serialize, Clone, Debug, Default)]
pub struct WbBody {
    pub session_id: String,
    /// `ai-title` 事件里的自动标题（比 sessions.title 更贴近正文）。
    pub ai_title: Option<String>,
    pub turns: Vec<WbTurn>,
}

// ---------------------------------------------------------------------------
// 会话列表
// ---------------------------------------------------------------------------

/// 把文件复制过去，**用手工读写而不是 `std::fs::copy`**。
///
/// ⚠️ Windows 上 `fs::copy` 走 `CopyFileExW`，它只申请 `FILE_SHARE_READ`；
/// 而 **WorkBuddy 正开着 `workbuddy.db-shm` 的内存映射**，于是复制直接失败：
/// `请求的操作无法在使用用户映射区域打开的文件上执行。(os error 1224)`
/// —— 也就是「WorkBuddy 在跑的时候，会话记录页整个打不开」。
///
/// `File::open` 申请的是 `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`，
/// 与已存在的映射兼容，能正常把字节读出来。
fn copy_shared(src: &Path, dst: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut f = std::fs::File::open(src)
        .map_err(|e| format!("打开 {} 失败: {e}", src.display()))?;
    let mut out = std::fs::File::create(dst)
        .map_err(|e| format!("创建 {} 失败: {e}", dst.display()))?;
    std::io::copy(&mut f, &mut out).map_err(|e| format!("复制 {} 失败: {e}", src.display()))?;
    // 显式读一下，确认不是「打开成功但读不出来」。
    let mut probe = [0u8; 1];
    let _ = f.read(&mut probe);
    Ok(())
}

/// 把 `workbuddy.db`（含 WAL）复制到工具目录后打开，避免与运行中的客户端
/// 争抢 WAL 锁。复制品可正常回放 WAL，读到最新数据。
///
/// ## 三个必须守住的细节
///
/// 1. **`-shm` 故意不复制**。它是 WAL 索引，被客户端内存映射着；复制它既会失败
///    （见 [`copy_shared`]），又是多余的 —— 复制完主库 + WAL 之后用 SQLite 打开一次，
///    SQLite 自己会把索引从 WAL 重建出来（下面第 3 步）。
/// 2. **`-wal` 必须一起复制**。客户端的写入全在 WAL 里，漏了它就会读到**上一次**的
///    事务帧，那比「缺文件」严重得多（用户会看到已经删掉的会话又冒出来）。
/// 3. ⚠️⚠️ **每次调用用独立的文件名**，不再复用固定的 `workbuddy.db`。
///    同一个页面会并发发起多个扫描（会话列表 + 一键互通预览 + 详情），旧实现先
///    `remove_file` 再复制固定文件名 —— 两个调用会**互删对方正在读的快照**，结果
///    是「打不开快照」或者更糟：读到别人刚写了一半的库。用唯一名 + 过期清理，
///    并发天然安全，也不需要一把全局锁（那把锁还会因为重入而卡死）。
pub fn snapshot_db() -> Result<PathBuf, String> {
    let root = data_root();
    let dir = store_dir().join("workbuddy_scan");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 WorkBuddy 快照目录失败: {e}"))?;
    prune_old_snapshots(&dir);
    let main = root.join("workbuddy.db");
    if !main.is_file() {
        return Err(format!("未找到 WorkBuddy 会话库：{}", main.display()));
    }
    // 唯一名：不同调用之间不共享任何文件。
    let stem = format!("workbuddy-{}", uuid::Uuid::new_v4().simple());
    let dst = dir.join(format!("{stem}.db"));
    copy_shared(&main, &dst)?;
    let wal = root.join("workbuddy.db-wal");
    if wal.is_file() {
        copy_shared(&wal, &dir.join(format!("{stem}.db-wal")))?;
    }
    // 3) 打开一次让 SQLite 依据 WAL 重建 `-shm`。此后 `open_ro()` 才能读这个快照
    //    （只读打开 WAL 库需要一份可用的 WAL 索引）。
    {
        let conn = rusqlite::Connection::open(&dst)
            .map_err(|e| format!("打开 WorkBuddy 快照失败: {e}"))?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(|e| format!("校验 WorkBuddy 快照失败: {e}"))?;
    }
    Ok(dst)
}

/// 清掉一小时前的旧快照（读失败就跳过 —— 清理失败不该让扫描失败）。
///
/// 1 小时远大于任何一次扫描的耗时，所以并发中的快照不会被误删。
fn prune_old_snapshots(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|d| d.as_secs() > 3600)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// 列出本机全部 WorkBuddy 会话（含已删除标记，由前端决定是否展示）。
pub fn list_sessions() -> Result<Vec<WbSession>, String> {
    if !is_available() {
        return Err(format!(
            "本机未检测到 WorkBuddy 数据（缺 projects/ 或 workbuddy.db）：{}",
            data_root().display()
        ));
    }
    let db = snapshot_db()?;
    let conn = rusqlite::Connection::open(&db).map_err(|e| format!("打开 WorkBuddy 快照失败: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, cwd, user_id, title, custom_title, created_at, updated_at, \
             deleted_at, model FROM sessions",
        )
        .map_err(|e| format!("查询 sessions 失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            let id: String = r.get(0)?;
            let cwd: Option<String> = r.get(1).ok();
            let user_id: Option<String> = r.get(2).ok();
            let title: Option<String> = r.get(3).ok();
            let custom: Option<String> = r.get(4).ok();
            let created: i64 = r.get(5).unwrap_or(0);
            let updated: i64 = r.get(6).unwrap_or(0);
            let deleted: Option<i64> = r.get(7).ok();
            let model: Option<String> = r.get(8).ok();
            Ok((id, cwd, user_id, title, custom, created, updated, deleted, model))
        })
        .map_err(|e| format!("读取 sessions 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("解析 sessions 行失败: {e}"))?;
    drop(stmt);
    drop(conn);

    let mut out: Vec<WbSession> = Vec::with_capacity(rows.len());
    for (id, cwd, user_id, title, custom, created, updated, deleted, model) in rows {
        let body = find_jsonl(&id);
        let body_bytes = body
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        out.push(WbSession {
            title: custom
                .or(title)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "(无标题)".to_string()),
            id,
            cwd: cwd.unwrap_or_default(),
            user_id: user_id.unwrap_or_default(),
            created_at: created,
            updated_at: updated,
            model: model.unwrap_or_default(),
            deleted: deleted.map(|d| d != 0).unwrap_or(false),
            has_body: body.is_some(),
            body_bytes,
            body_lines: 0,
            content_digest: String::new(),
            dup_group: String::new(),
            is_newest: true,
            dup_note: String::new(),
            dup_relation: String::new(),
        });
    }
    // 按最后更新时间倒序，与客户端列表观感一致
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    // 同标题副本的新旧判定（多账号时同一段对话会有多份，必须能分清谁是最新）。
    // 不重排列表：重复副本的时间本来就接近，天然相邻，全局「最近优先」的顺序更顺手。
    annotate_duplicates(&mut out);
    Ok(out)
}

/// [`list_sessions`] 的**强制比对**版本：忽略体积/成员数上限，逐行读完整正文。
///
/// 只给「用户手动点了强制比对」用（见 `annotate_duplicates_opt` 的注释）。
/// 慢是预期行为：一份 227 MB 的正文要读几秒，界面必须自己显示「正在分析」。
pub fn list_sessions_forced() -> Result<Vec<WbSession>, String> {
    let mut out = list_sessions()?;
    annotate_duplicates_opt(&mut out, true);
    Ok(out)
}

/// 在 `projects/` 下任意子目录中定位 `{会话id}.jsonl`。
pub fn find_jsonl(session_id: &str) -> Option<PathBuf> {
    let projects = data_root().join("projects");
    if !projects.is_dir() {
        return None;
    }
    let file_name = format!("{session_id}.jsonl");
    // 直接位于 projects/ 根下的情况
    let direct = projects.join(&file_name);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(&projects).ok()?.flatten() {
        let candidate = entry.path().join(&file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 正文解析
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 用户消息的「真实 / 注入」判别
// ---------------------------------------------------------------------------

/// WorkBuddy 会把**客户端自己生成**的内容也写成 `role=user`。
///
/// 它们不是用户敲的字，客户端界面也不会把它们当提问展示。若原样透传，Trae 里就会
/// 凭空多出用户从没见过的「用户消息」，以及为它们补出的**空白助手气泡** ——
/// 这正是「Trae 显示与 WorkBuddy 不一致」的主因（v0.0.13 复盘）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMsgKind {
    /// 真实提问（正文包在 `<user_query>…</user_query>` 里）。
    Real,
    /// 纯内部上下文摘要：`<conversation_history_summary>` / `<cb_summary>`。
    ///
    /// 实测**没有任何助手回答**跟它配对（回答属于紧随其后的 `Please continue`），
    /// 且长度可达 1.5×10⁵ 字符。整条丢弃。
    SilentSummary,
    /// 自动续写指令 / 后台任务通知：`Please continue with…`、`<task-notification>`。
    ///
    /// 同样不是用户输入，但**有**助手回答 —— 丢掉这层壳，让回答并入当前回合。
    AutoTrigger,
}

/// 判定注入类型时只看**行首**，避免在摘要正文里误伤
/// （`<cb_summary>` 正文里会成段引用 `<system-reminder>` 与 `<user_query>`）。
const SUMMARY_PREFIXES: [&str; 2] = ["<conversation_history_summary>", "<cb_summary>"];
const AUTO_PREFIXES: [&str; 4] = [
    "<task-notification",
    "Please continue with the conversation based on the summarized context",
    // `<task-notification>` 块**之后**还挂着两段指令尾巴，它们不在标签内
    // （只剥标签会把这些文字留下来当提问 —— 实测 6 条用户消息就是这样来的）。
    "Use the TaskOutput tool with task_id=",
    "IMPORTANT: Before responding, scroll back",
];

/// 判断一条 `role=user` 消息到底是不是用户在提问。
pub fn classify_user_message(raw: &str) -> UserMsgKind {
    let t = raw.trim_start();
    if SUMMARY_PREFIXES.iter().any(|p| t.starts_with(p)) {
        return UserMsgKind::SilentSummary;
    }
    if AUTO_PREFIXES.iter().any(|p| t.starts_with(p)) {
        return UserMsgKind::AutoTrigger;
    }
    UserMsgKind::Real
}

/// 从**行首**逐个剥掉完整的注入块；碰到不是注入块的内容立刻停手。
///
/// 只剥行首（旧实现是全文搜索）很关键：`<cb_summary>` 正文里引用了同名标签，
/// 全文搜索会把摘要**拦腰削断**（实测 80947 字符被削成 8614 字符的乱码）。
fn strip_leading_blocks(text: &str) -> &str {
    const BLOCKS: [&str; 2] = ["<system-reminder", "<task-notification"];
    let mut rest = text;
    loop {
        let t = rest.trim_start();
        let Some(tag) = BLOCKS.iter().find(|tag| t.starts_with(**tag)) else {
            return t;
        };
        let close = format!("</{}>", tag.trim_start_matches('<'));
        match t.find(&close) {
            Some(i) => rest = &t[i + close.len()..],
            // 只有开标签没有闭合：整段视为注入
            None => return "",
        }
    }
}

/// 从用户消息文本中取出**真实提问**。
///
/// 结构恒定：`[system-reminder 块…][<user_query>…</user_query>][附加数据块…]`。
/// 必须先剥**行首**的注入块、再认 `<user_query>`，不能全文 `find` ——
/// 摘要正文里最多出现 7 个 `<user_query>`，全文搜索会取出一段毫不相干的文字
/// （实测取到的是源码片段，甚至上一轮的续写指令）。
pub(crate) fn extract_user_text(text: &str) -> String {
    let raw = text.trim();
    if classify_user_message(raw) != UserMsgKind::Real {
        return String::new();
    }
    let stripped = strip_leading_blocks(raw);
    if let Some(rest) = stripped.strip_prefix("<user_query>") {
        let end = rest.find("</user_query>").unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    stripped.trim().to_string()
}

/// 拼接 `message` 事件 `content[]` 里所有文本块（assistant 用 `output_text`）。
pub(crate) fn join_content_blocks(content: &Value) -> String {
    let Some(blocks) = content.as_array() else {
        return content.as_str().unwrap_or_default().trim().to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        if let Some(t) = block.get("text").and_then(Value::as_str) {
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }
    }
    parts.join("\n").trim().to_string()
}

fn ts_of(v: &Value) -> i64 {
    v.get("timestamp").and_then(Value::as_i64).unwrap_or(0)
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// 解析单个会话的正文 JSONL 为归一化回合序列。
pub fn load_body(session_id: &str) -> Result<WbBody, String> {
    let path = find_jsonl(session_id).ok_or_else(|| {
        format!(
            "会话 {session_id} 的正文文件不存在（projects/ 下未找到 {session_id}.jsonl）"
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    Ok(parse_body(session_id, &text))
}

/// 从 JSONL 文本解析为归一化回合序列。
///
/// 与 [`load_body`] 走的是**同一套规则**：导出方向（`workbuddy_export`）生成的正文
/// 可以直接喂进来校验，离线测试因此能覆盖「写出去 → 再读回来」的完整闭环，
/// 而不是各写一份可能同时出错的解析。
pub fn parse_body(session_id: &str, text: &str) -> WbBody {
    let mut body = WbBody {
        session_id: session_id.to_string(),
        ..Default::default()
    };
    let mut current: Option<WbTurn> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            // 单行损坏不阻断整个会话（WorkBuddy 偶有截断行）
            continue;
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "ai-title" => {
                let t = str_of(&v, "aiTitle");
                if !t.is_empty() {
                    body.ai_title = Some(t);
                }
            }
            "message" => {
                let role = str_of(&v, "role");
                let content = v.get("content").cloned().unwrap_or(Value::Null);
                match role.as_str() {
                    "user" => {
                        let raw = join_content_blocks(&content);
                        match classify_user_message(&raw) {
                            // 纯内部摘要：界面上看不到，也没有助手回答配对 —— 整条不要。
                            // （旧实现在这里建回合，于是 Trae 里多出 1.2 万～1.5 万字符的
                            //   「用户消息」，还配了一个空白助手气泡。）
                            UserMsgKind::SilentSummary => {}
                            // 自动续写 / 后台任务通知：不是用户输入，但**有**助手回答。
                            // 不闭合当前回合，让随后的 assistant 事件落进上一个真实回合，
                            // 既不凭空多出用户消息，也不丢掉助手的输出。
                            UserMsgKind::AutoTrigger => {
                                if current.is_none() {
                                    // 极端情况（会话以注入开头）：仍开一个回合接住回答，
                                    // 否则这段助手输出会被静默丢弃。
                                    current = Some(WbTurn::default());
                                }
                            }
                            // 真实提问：闭合上一回合，开新回合。
                            UserMsgKind::Real => {
                                // 上一条回合没有等到新的提问即已结束（正常情况：下一轮提问）
                                if let Some(prev) = current.take() {
                                    body.turns.push(prev);
                                }
                                let ts = ts_of(&v);
                                current = Some(WbTurn {
                                    user_text: extract_user_text(&raw),
                                    created_at: ts,
                                    updated_at: ts,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    "assistant" => {
                        // 助手在一个回合里会多次发 message：中间叙述全部保留为过程事件，
                        // 最后一条同时作为该回合的「最终回答」。
                        let text = join_content_blocks(&content);
                        if text.is_empty() {
                            continue;
                        }
                        let ts = ts_of(&v);
                        let turn = current.get_or_insert_with(WbTurn::default);
                        turn.events.push(WbEvent {
                            kind: "assistant_text".to_string(),
                            name: String::new(),
                            call_id: String::new(),
                            status: String::new(),
                            text: text.clone(),
                        });
                        turn.assistant_text = text;
                        turn.updated_at = ts;
                    }
                    _ => {}
                }
            }
            "reasoning" | "function_call" | "function_call_result" => {
                let Some(turn) = current.as_mut() else {
                    continue;
                };
                let event = match kind {
                    "reasoning" => {
                        let raw = v.get("rawContent").and_then(Value::as_array);
                        let text = raw
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        if text.trim().is_empty() {
                            continue;
                        }
                        WbEvent {
                            kind: kind.to_string(),
                            name: String::new(),
                            call_id: String::new(),
                            status: String::new(),
                            text: text.trim().to_string(),
                        }
                    }
                    "function_call" => WbEvent {
                        kind: kind.to_string(),
                        name: str_of(&v, "name"),
                        call_id: str_of(&v, "callId"),
                        status: String::new(),
                        // 真实数据里 `arguments` 是**字符串**（内容形如 `{"file_path":"…"}`）。
                        // 原来直接 `a.to_string()` 会把它再序列化一遍，得到
                        // `"{\"file_path\":\"…\"}"`（外层多一对引号 = 双重编码）；
                        // 下游 `parse_params` 于是解析成 Value::String 而非对象，
                        // 落到 Trae 的 tool_call_info.params 就成了字符串 —— 工具卡片
                        // 取不到参数，表现为「WorkBuddy 的过程在 Trae 显示奇怪」。
                        // 所以字符串优先取内容，只有 arguments 本身是对象/数组时才序列化。
                        text: v
                            .get("arguments")
                            .map(|a| {
                                a.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| a.to_string())
                            })
                            .unwrap_or_default(),
                    },
                    _ => {
                        // 工具结果文本的抽取顺序很关键，抽错会让 Trae 卡片显示成
                        // 一整坨带引号/带转义的原始 JSON：
                        //   ① output 本身是字符串 → 直接用（否则 to_string 会再包一层引号）
                        //   ② output 是对象且有 text → 取 text（常态：{type,text}）
                        //   ③ output 是数组 → 拼各段 text（如 present_files 的 input_text）
                        //   ④ 其余 → 序列化兜底
                        let text = match v.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(o) => {
                                if let Some(t) = o.get("text").and_then(Value::as_str) {
                                    t.to_string()
                                } else if let Some(arr) = o.as_array() {
                                    let parts: Vec<&str> = arr
                                        .iter()
                                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                                        .collect();
                                    if parts.is_empty() {
                                        o.to_string()
                                    } else {
                                        parts.join("\n")
                                    }
                                } else {
                                    o.to_string()
                                }
                            }
                            None => String::new(),
                        };
                        WbEvent {
                            kind: kind.to_string(),
                            name: str_of(&v, "name"),
                            call_id: str_of(&v, "callId"),
                            status: str_of(&v, "status"),
                            text,
                        }
                    }
                };
                turn.events.push(event);
                turn.updated_at = ts_of(&v).max(turn.updated_at);
            }
            _ => {}
        }
    }
    if let Some(rest) = current {
        body.turns.push(rest);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_text_prefers_user_query_block() {
        let raw = "<system-reminder data-role=\"user-context\">\n一堆上下文\n</system-reminder>\n<user_query>真实提问</user_query>";
        assert_eq!(extract_user_text(raw), "真实提问");
    }

    #[test]
    fn user_text_strips_leading_reminder_only() {
        // 只剥行首注入块；正文里 `提到的` 标签不再被误伤
        let raw = "<system-reminder>上下文</system-reminder>\n<user_query>真实提问</user_query>";
        assert_eq!(extract_user_text(raw), "真实提问");

        // 没有 user_query 时，行首的注入块照样剥掉
        let raw = "<system-reminder>上下文</system-reminder>\n剩下的话";
        assert_eq!(extract_user_text(raw), "剩下的话");
    }

    #[test]
    fn user_text_does_not_scan_body_for_tags() {
        // 旧实现在**全文**搜 `<system-reminder`／`<user_query>`，会把引用了这些标签的
        // 正文拦腰削断（实测把 80947 字符的摘要削成 8614 字符的乱码）。
        // 现在只剥行首 → 中间的标签原样保留。这是一条**回归护栏**。
        let raw = "开头\n<system-reminder>上下文</system-reminder>\n结尾";
        assert_eq!(
            extract_user_text(raw),
            "开头\n<system-reminder>上下文</system-reminder>\n结尾"
        );
    }

    #[test]
    fn classify_separates_injections_from_real_questions() {
        use UserMsgKind::*;
        // 纯内部摘要
        assert_eq!(classify_user_message("<conversation_history_summary>\nSummary:…"), SilentSummary);
        assert_eq!(classify_user_message("<cb_summary>\nSummary of the conversation so far:"), SilentSummary);
        // 自动续写 / 后台任务通知
        assert_eq!(
            classify_user_message(
                "Please continue with the conversation based on the summarized context above."
            ),
            AutoTrigger
        );
        assert_eq!(classify_user_message("<task-notification>\n<task-id>x</task-id>\n</task-notification>"), AutoTrigger);
        assert_eq!(
            classify_user_message("Use the TaskOutput tool with task_id=\"x\" to retrieve the full output"),
            AutoTrigger
        );
        // 真实提问
        assert_eq!(classify_user_message("<system-reminder>x</system-reminder>\n<user_query>提问</user_query>"), Real);
        assert_eq!(classify_user_message("我如何测试？怎么操作"), Real);
    }

    #[test]
    fn extract_user_text_ignores_injections_entirely() {
        // 注入条目本身不产出提问文本（即使正文里嵌着 <user_query>）
        let cb = "<cb_summary>\n…<user_query>\nPlease continue with the conversation based on the summarized context above.\n</user_query>\n</cb_summary>";
        assert_eq!(extract_user_text(cb), "");
        assert_eq!(extract_user_text("<conversation_history_summary>Summary:…"), "");
    }

    #[test]
    fn parse_body_drops_injections_and_keeps_replies() {
        // 一段最小会话：真实提问 → 回答；摘要（无回答）；续写（有回答）；真实提问 → 回答
        let ev = |role: &str, text: &str| {
            serde_json::json!({
                "type": "message",
                "role": role,
                "content": [{ "type": "input_text", "text": text }],
                "timestamp": 1_700_000_000_000i64,
            })
        };
        let mut lines: Vec<String> = Vec::new();
        let mut push = |v: Value| lines.push(v.to_string());
        push(ev("user", "<system-reminder>x</system-reminder>\n<user_query>第一个问题</user_query>"));
        push(ev("assistant", "第一个回答"));
        push(ev("user", "<conversation_history_summary>\nSummary: 一大段"));
        push(ev("user", "Please continue with the conversation based on the summarized context above."));
        push(ev("assistant", "续写的回答"));
        push(ev("user", "<system-reminder>y</system-reminder>\n<user_query>第二个问题</user_query>"));
        push(ev("assistant", "第二个回答"));
        drop(push);
        let text = lines.join("\n");

        let body = parse_body("s", &text);
        assert_eq!(body.turns.len(), 2, "注入条目不得产生回合");
        assert_eq!(body.turns[0].user_text, "第一个问题");
        // 摘要被丢弃、续写不新开回合 → 续写回答并入第一回合
        assert_eq!(body.turns[0].assistant_text, "续写的回答");
        assert_eq!(body.turns[1].user_text, "第二个问题");
        assert_eq!(body.turns[1].assistant_text, "第二个回答");
    }

    #[test]
    fn joins_text_blocks() {
        let v: Value = serde_json::json!([
            {"type": "output_text", "text": "第一段"},
            {"type": "output_text", "text": "第二段"}
        ]);
        assert_eq!(join_content_blocks(&v), "第一段\n第二段");
    }

    // -----------------------------------------------------------------
    // 同标题副本的新旧判定
    // -----------------------------------------------------------------

    fn cid(lines: &[&str]) -> ContentId {
        let d: Vec<String> = lines.iter().map(|l| line_digest(l)).collect();
        let t = total_digest(&d);
        ContentId {
            lines: d.len(),
            line_digests: d,
            total: t,
        }
    }

    #[test]
    fn norm_title_strips_truncation_ellipsis_and_case() {
        assert_eq!(norm_title("这份是之前执行的记录，你按之前记录继续完…"), "这份是之前执行的记录，你按之前记录继续完");
        assert_eq!(norm_title("Hello   World"), "hello world");
        assert_eq!(norm_title("  "), "");
    }

    #[test]
    fn relate_detects_prefix_extension_and_divergence() {
        let short = cid(&["a", "b"]);
        let long = cid(&["a", "b", "c"]);
        assert_eq!(relate(&short, &long), Relation::Prefix);
        assert_eq!(relate(&long, &short), Relation::Extension);
        assert_eq!(relate(&short, &short.clone()), Relation::Equal);
        assert_eq!(relate(&cid(&["a", "x"]), &long), Relation::Diverged);
    }

    fn mk(id: &str, title: &str, updated: i64, has_body: bool) -> WbSession {
        WbSession {
            id: id.to_string(),
            title: title.to_string(),
            cwd: String::new(),
            user_id: String::new(),
            created_at: 0,
            updated_at: updated,
            model: String::new(),
            deleted: false,
            has_body,
            body_bytes: if has_body { 10 } else { 0 },
            body_lines: 0,
            content_digest: String::new(),
            dup_group: String::new(),
            is_newest: true,
            dup_note: String::new(),
            dup_relation: String::new(),
        }
    }

    #[test]
    fn duplicates_are_grouped_and_newest_marked() {
        // 两份同标题、都无正文：退化为按 updated_at 选最新
        let mut list = vec![
            mk("a", "查看 cargo 测试结果", 200, false),
            mk("b", "查看 cargo 测试结果", 100, false),
        ];
        annotate_duplicates(&mut list);
        assert!(!list[0].dup_group.is_empty());
        assert_eq!(list[0].dup_group, list[1].dup_group);
        assert!(list[0].is_newest, "updated_at 大的应判为最新");
        assert!(!list[1].is_newest);

        // 唯一标题：不分组、视为最新
        let mut solo = vec![mk("c", "独一份", 1, false)];
        annotate_duplicates(&mut solo);
        assert!(solo[0].dup_group.is_empty());
        assert!(solo[0].is_newest);
        assert!(solo[0].dup_note.is_empty());
    }

    #[test]
    fn content_containment_beats_timestamp() {
        // 0 号：时间更新，但只有 2 条；1 号：时间更旧，却包含 0 号的全部 2 条 + 1 条。
        // 结论必须是以内容为准 —— 1 号才是最新。
        let ids = vec![Some(cid(&["a", "b"])), Some(cid(&["a", "b", "c"]))];
        let (newest, notes) = decide_group(&ids, &[900, 100]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "比最新副本少 1 条记录");
        assert!(notes[1].is_empty(), "最新那条的说明由调用方填，此处应为空");
    }

    #[test]
    fn identical_content_falls_back_to_timestamp() {
        let ids = vec![Some(cid(&["a", "b"])), Some(cid(&["a", "b"]))];
        let (newest, notes) = decide_group(&ids, &[100, 900]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "与最新副本内容完全相同");
    }

    #[test]
    fn diverged_content_falls_back_to_timestamp_and_says_so() {
        let ids = vec![Some(cid(&["a", "x"])), Some(cid(&["a", "b", "c"]))];
        let (newest, notes) = decide_group(&ids, &[100, 900]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "内容分叉（本副本 2 条 / 最新 3 条）");
    }

    #[test]
    fn body_less_member_never_becomes_newest() {
        // 只有一个成员有正文时，最新必须有正文的那份（空壳没有可比内容）
        let ids = vec![None, Some(cid(&["a"]))];
        let (newest, notes) = decide_group(&ids, &[900, 100]);
        assert_eq!(newest, 1);
        assert_eq!(notes[0], "无正文，不参与比对");
    }

    #[test]
    fn content_id_cache_hits_and_never_serves_stale() {
        // 缓存必须做到两件事：① 同一份文件重复取摘要不重读；② 文件变了立刻失效。
        let dir = std::env::temp_dir().join(format!("wbsrc-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.jsonl");
        let ids = vec!["aaaa".to_string()];
        std::fs::write(&f, "{\"sessionId\":\"aaaa\",\"t\":1}\n").unwrap();

        let first = content_id_cached(&f, &ids).unwrap();
        let again = content_id_cached(&f, &ids).unwrap();
        assert_eq!(first.total, again.total);
        assert_eq!(first.lines, 1);

        // 改写文件（长度也变）⇒ 键变 ⇒ 必须重算，绝不能返回旧摘要
        std::fs::write(&f, "{\"sessionId\":\"aaaa\",\"t\":1}\n{\"t\":2}\n").unwrap();
        let after = content_id_cached(&f, &ids).unwrap();
        assert_eq!(after.lines, 2);
        assert_ne!(after.total, first.total);

        // id 集不同 ⇒ 归一化结果不同 ⇒ 摘要不同（键必须包含 id 集）
        let other = content_id_cached(&f, &["zzzz".to_string()]).unwrap();
        assert_ne!(other.total, after.total);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_id_normalizes_own_session_id() {
        // 同一段对话存在两个账号里：除会话 id 外逐字相同 ⇒ 必须拿到同一个摘要
        let dir = std::env::temp_dir().join(format!("wbsrc-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        let ids = vec!["aaaa".to_string(), "bbbb".to_string()];
        std::fs::write(&a, "{\"sessionId\":\"aaaa\",\"text\":\"hi\"}\n{\"n\":2}\n").unwrap();
        std::fs::write(&b, "{\"sessionId\":\"bbbb\",\"text\":\"hi\"}\n{\"n\":2}\n").unwrap();
        let ca = content_id(&a, &ids).unwrap();
        let cb = content_id(&b, &ids).unwrap();
        assert_eq!(ca.total, cb.total);
        assert_eq!(ca.lines, 2);
        // 行序不同则摘要不同（重排必须被识别为差异）
        std::fs::write(&b, "{\"n\":2}\n{\"sessionId\":\"bbbb\",\"text\":\"hi\"}\n").unwrap();
        assert_ne!(content_id(&b, &ids).unwrap().total, ca.total);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cross_referenced_ids_do_not_create_fake_divergence() {
        // 正文里引用同组另一副本的 id（本机真实发生过：对话里打印过那三个 id）：
        // 只归一化「自己那个」时，A 保留字面 bbbb、B 却全变成标记 ⇒ 假分歧。
        // 按整组归一化后，两份必须完全一致。
        let dir = std::env::temp_dir().join(format!("wbsrc-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        let ids = vec!["aaaa".to_string(), "bbbb".to_string()];
        std::fs::write(&a, "{\"sessionId\":\"aaaa\",\"text\":\"另见 bbbb\"}\n").unwrap();
        // B 是 A 的副本：自己的 id 是 bbbb，引用处也成了 bbbb
        std::fs::write(&b, "{\"sessionId\":\"bbbb\",\"text\":\"另见 bbbb\"}\n").unwrap();
        let ca = content_id(&a, &ids).unwrap();
        let cb = content_id(&b, &ids).unwrap();
        assert_eq!(ca.total, cb.total, "引用了组内别的 id 也不能算分歧");
        // 对照组：只传自己的 id（旧行为）时两份确实不同 —— 锁住根因，别改回去
        let ca_old = content_id(&a, &["aaaa".to_string()]).unwrap();
        let cb_old = content_id(&b, &["bbbb".to_string()]).unwrap();
        assert_ne!(ca_old.total, cb_old.total);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回读校验：一样就必须说一样，差一行就必须说不一样（读不出来也算不一样）。
    #[test]
    fn verify_same_content_detects_real_difference() {
        let dir = std::env::temp_dir().join(format!("wbsrc-v-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        let ids = vec!["aaaa".to_string(), "bbbb".to_string()];
        std::fs::write(&a, "{\"n\":1}\n{\"n\":2}\n").unwrap();
        std::fs::write(&b, "{\"n\":1}\n{\"n\":2}\n").unwrap();
        let r = verify_same_content(&[a.clone(), b.clone()], &ids);
        assert!(r.all_same, "内容相同必须判一致");
        assert_eq!(r.lines, 2, "行数要如实报，界面要用它显示");
        assert!(r.mismatched.is_empty());
        assert_eq!(r.digest.len(), 16, "摘要取前 16 位，与 content_digest 同口径");

        // 多一行 ⇒ 必须判不一致，且下标指向第二份
        std::fs::write(&b, "{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n").unwrap();
        let r = verify_same_content(&[a, b.clone()], &ids);
        assert!(!r.all_same);
        assert_eq!(r.mismatched, vec![1usize]);

        // 文件不存在也算不一致 —— 绝不能把「读失败」当成「一样」
        let missing = dir.join("nope.jsonl");
        let r = verify_same_content(&[b, missing], &ids);
        assert!(!r.all_same);
        assert_eq!(r.mismatched, vec![1usize]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 「已同步并校验一致」的记录：刷新能复用，文件一动必须失效。
    #[test]
    fn verified_record_survives_refresh_but_dies_on_change() {
        let dir = std::env::temp_dir().join(format!("wbsrc-r-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        let store = dir.join("verified.json");
        std::fs::write(&a, "{\"n\":1}\n").unwrap();
        std::fs::write(&b, "{\"n\":1}\n").unwrap();
        let pairs = vec![
            ("aaaa".to_string(), a.clone()),
            ("bbbb".to_string(), b.clone()),
        ];

        assert!(
            verified_after_sync_at(&store, &pairs).is_none(),
            "没登记过就只能是「不知道」，不许猜"
        );

        mark_verified_at(&store, &pairs, 1, "0123456789abcdef").unwrap();
        let hit = verified_after_sync_at(&store, &pairs).expect("登记后应能复用结论");
        assert_eq!(hit, (1usize, "0123456789abcdef".to_string()));
        assert_eq!(
            load_verified_at(&store).len(),
            2,
            "必须落盘；只放内存的话重启就退回「未比对」"
        );

        // 文件被改（长度也变）⇒ 记录失效
        std::fs::write(&b, "{\"n\":1}\n{\"n\":2}\n").unwrap();
        assert!(
            verified_after_sync_at(&store, &pairs).is_none(),
            "文件变了就不能再拿旧结论冒充当前状态"
        );

        // 只登记了一份 ⇒ 不够下结论
        let store2 = dir.join("verified2.json");
        mark_verified_at(&store2, &pairs[..1], 1, "0123456789abcdef").unwrap();
        assert!(verified_after_sync_at(&store2, &pairs).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 已校验一致时的关系：最新那份 `latest`，其余 `equal`；不能冒出 `behind`。
    #[test]
    fn synced_group_never_reports_behind() {
        let (newest, rels) = synced_relations(&[100, 300, 200]);
        assert_eq!(newest, 1);
        assert_eq!(rels, vec!["equal", "latest", "equal"]);
        assert!(!rels.contains(&"behind"), "内容一致就不许说「有副本落后」");

        // 并列时取靠前那个，避免每次刷新「最新」跳来跳去
        let (newest, rels) = synced_relations(&[500, 500]);
        assert_eq!(newest, 0);
        assert_eq!(rels, vec!["latest", "equal"]);
    }

    /// 强制比对结论的落盘与复用：摘要各存各的，指纹没变就能整组取回；
    /// 摘要不同时 `verified_after_sync` 不命中（它只认全同），`compared_from_disk` 命中。
    #[test]
    fn compared_record_roundtrips_and_dies_on_change() {
        let dir = std::env::temp_dir().join(format!("wbsrc-c-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        let store = dir.join("verified.json");
        std::fs::write(&a, "{\"n\":1}\n").unwrap();
        std::fs::write(&b, "{\"n\":1}\n{\"n\":2}\n").unwrap();
        let pairs = vec![
            ("aaaa".to_string(), a.clone()),
            ("bbbb".to_string(), b.clone()),
        ];
        let values = vec![
            ("aaaa".to_string(), a.clone(), 1usize, "aaaaaaaaaaaaaaaa".to_string()),
            ("bbbb".to_string(), b.clone(), 2usize, "bbbbbbbbbbbbbbbb".to_string()),
        ];

        assert!(compared_from_disk_at(&store, &pairs).is_none(), "没登记过 = 不知道");
        assert_eq!(mark_compared_at(&store, &values), 2, "两条都该登记上");

        // 摘要不同：verified_after_sync（只认全同）不命中；compared_from_disk 带回各自结论
        assert!(verified_after_sync_at(&store, &pairs).is_none());
        let hit = compared_from_disk_at(&store, &pairs).expect("指纹没变就该复用");
        assert_eq!(
            hit,
            vec![
                Some((1usize, "aaaaaaaaaaaaaaaa".to_string())),
                Some((2usize, "bbbbbbbbbbbbbbbb".to_string())),
            ]
        );

        // 摘要全相同时两条出口都要命中（同步回读与强制比对的结论互通）
        let same = vec![
            ("aaaa".to_string(), a.clone(), 1usize, "dddddddddddddddd".to_string()),
            ("bbbb".to_string(), b.clone(), 1usize, "dddddddddddddddd".to_string()),
        ];
        // b 的长度变了，先重登记（mark_compared 会取当前指纹）
        mark_compared_at(&store, &same);
        assert_eq!(
            verified_after_sync_at(&store, &pairs),
            Some((1usize, "dddddddddddddddd".to_string())),
            "全同结论走旧出口也要能命中"
        );

        // 文件被改 ⇒ **它自己**的结论失效，没动的那份照常留着（只作废一个，不连累全组）
        std::fs::write(&b, "{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n").unwrap();
        assert_eq!(
            compared_from_disk_at(&store, &pairs),
            Some(vec![Some((1usize, "dddddddddddddddd".to_string())), None]),
            "动过的那份不许拿旧结论冒充，没动过的必须复用"
        );
        assert!(verified_after_sync_at(&store, &pairs).is_none(), "全组一致不再成立");

        // 只有 一份成员 ⇒ 不够下结论
        assert!(compared_from_disk_at(&store, &pairs[..1]).is_none());

        // ⚠️ 两份里只改了一份：另一份的结论**必须留着**（2 成员组也要能部分复用）。
        // 早期版本要求「≥2 份命中」，2 成员的组只要有一个账号说了句话就整组退回
        // 「无法确认」，用户只能再等一次十几秒的强制比对。
        mark_compared_at(&store, &same);
        assert_eq!(
            verified_after_sync_at(&store, &pairs),
            Some((1usize, "dddddddddddddddd".to_string())),
            "重新登记后两份都该是有效结论"
        );
        std::fs::write(&b, "{\"n\":1}\n{\"n\":2}\n").unwrap();
        let part = compared_from_disk_at(&store, &pairs).expect("还剩一份有效就该给出来");
        assert_eq!(
            part,
            vec![Some((1usize, "dddddddddddddddd".to_string())), None],
            "改过的那份作废，没改的照常复用"
        );

        // 空摘要跳过不登记
        let empty = vec![("cccc".to_string(), a.clone(), 1usize, String::new())];
        assert_eq!(mark_compared_at(&store, &empty), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 落盘结论全同 ⇒ 与「同步回读一致」同形状（latest + equal）。
    #[test]
    fn apply_compared_from_disk_equal_uses_synced_shape() {
        let mut list = vec![mk("a", "T", 100, true), mk("b", "T", 200, true)];
        let idx = vec![0usize, 1usize];
        apply_compared_from_disk(
            &mut list,
            &idx,
            "T",
            &[
                Some((7usize, "0123456789abcdef".to_string())),
                Some((7, "0123456789abcdef".to_string())),
            ],
        );
        assert!(list[1].is_newest, "updated_at 大的判最新");
        assert_eq!(list[0].dup_relation, "equal");
        assert_eq!(list[1].dup_relation, "latest");
        assert_eq!(list[0].body_lines, 7);
        assert!(list[0].dup_note.contains("一致"), "来历要写清楚：{}", list[0].dup_note);
        assert!(!list[0].dup_relation.is_empty());
    }

    /// 落盘结论不同 ⇒ 只能如实报「内容不同」；没有行级摘要，绝不冒充
    /// `behind` / `ahead`（那需要逐行包含关系，落盘的只有整份摘要）。
    #[test]
    fn apply_compared_from_disk_diff_reports_diverged_honestly() {
        let mut list = vec![mk("a", "T", 100, true), mk("b", "T", 200, true)];
        let idx = vec![0usize, 1usize];
        apply_compared_from_disk(
            &mut list,
            &idx,
            "T",
            &[
                Some((3usize, "aaaaaaaaaaaaaaaa".to_string())),
                Some((5, "bbbbbbbbbbbbbbbb".to_string())),
            ],
        );
        assert_eq!(list[1].dup_relation, "latest");
        assert_eq!(list[0].dup_relation, "diverged", "摘要不同且无行级信息 ⇒ 只能说 diverged");
        assert_eq!(list[0].body_lines, 3);
        assert_eq!(list[0].content_digest, "aaaaaaaaaaaaaaaa");
        assert!(list[0].dup_note.contains("内容不同"), "{}", list[0].dup_note);
    }

    /// 某个账号发了新对话（只有它那份文件变了）⇒ **只作废它自己**的结论，
    /// 其余成员照常复用，组状态记「待同步」。
    ///
    /// 这条锁的是 2026-10-30 的反馈：4 个账号比对过，其中一个说了句话，
    /// 整组立刻退回「未比对」，用户还得再等十几秒强制比对一次。
    #[test]
    fn apply_compared_from_disk_keeps_unaffected_members() {
        let mut list = vec![
            mk("a", "T", 100, true),
            mk("b", "T", 200, true),
            mk("c", "T", 300, true),
        ];
        let idx = vec![0usize, 1usize, 2usize];
        apply_compared_from_disk(
            &mut list,
            &idx,
            "T",
            &[
                Some((3usize, "aaaaaaaaaaaaaaaa".to_string())),
                Some((3, "aaaaaaaaaaaaaaaa".to_string())),
                None, // c 在上次比对后被改过
            ],
        );
        assert_eq!(list[0].dup_relation, "equal", "没变的成员结论要留着");
        assert_eq!(list[1].dup_relation, "latest", "有结论里最新的那份当基准");
        assert_eq!(list[1].is_newest, true);
        assert_eq!(list[2].dup_relation, "stale", "变了的只作废自己");
        assert!(list[2].dup_note.contains("有新内容"), "{}", list[2].dup_note);
        assert!(list[2].content_digest.is_empty(), "作废的摘要不能留着冒充结论");
    }

    /// 判据优先级：强制 > 成员超限 > 体积合规 > 摘要缓存 > 校验记录 > 不知道。
    ///
    /// 这条顺序一旦写反，要么「明明一致却写未比对」（用户看到的那个 bug），
    /// 要么「没比对却说一致」（更糟）。两头都必须锁住。
    #[test]
    fn decide_mode_priority_is_stable() {
        let facts = |members: usize, all_body: bool, all_fit: bool, all_cached: bool, recorded: bool| {
            GroupFacts {
                members,
                all_body,
                all_fit,
                all_cached,
                recorded,
            }
        };
        // 强制比对永远有效：成员超限、什么都没算过也照样真读
        assert_eq!(
            decide_mode(true, &facts(20, true, false, false, false)),
            Decide::Forced
        );
        // 成员超上限 ⇒ 不知道（缓存/记录都不该越过这条线）
        assert_eq!(
            decide_mode(false, &facts(9, true, false, true, true)),
            Decide::Unknown
        );
        // 缺正文 ⇒ 不知道
        assert_eq!(
            decide_mode(false, &facts(3, false, false, true, true)),
            Decide::Unknown
        );
        // 体积合规 ⇒ 常规逐行比对（优先于复用）
        assert_eq!(
            decide_mode(false, &facts(3, true, true, true, true)),
            Decide::Compare
        );
        // 超限但摘要已在缓存 ⇒ 直接复用（「强制比对后刷新又变未比对」的修复点）
        assert_eq!(
            decide_mode(false, &facts(4, true, false, true, false)),
            Decide::ReuseCache
        );
        // 超限、无缓存、但有同步回读校验记录 ⇒ 复用记录
        assert_eq!(
            decide_mode(false, &facts(4, true, false, false, true)),
            Decide::ReuseRecord
        );
        // 什么都没有 ⇒ 老实说不知道，绝不猜「一致」
        assert_eq!(
            decide_mode(false, &facts(4, true, false, false, false)),
            Decide::Unknown
        );
    }
}
