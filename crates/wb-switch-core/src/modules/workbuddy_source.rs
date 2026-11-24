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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::modules::config::store_dir;
use crate::modules::settings;

/// WorkBuddy 数据根目录名（位于用户主目录下）。
const WB_DIR_NAME: &str = ".workbuddy";

/// WorkBuddy 数据根：`~/.workbuddy`。
pub fn data_root() -> PathBuf {
    // 用户把 `~/.workbuddy` 挪到别的盘 / 做成 junction 时，以手动指定为准（issue #26）。
    // ⚠️ 只在**配置了**的时候覆盖：没配置就走默认，行为与历史版本一致。
    if let Some(p) = crate::modules::config::workbuddy_data_root_override() {
        return p;
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(WB_DIR_NAME)
}

/// 能力探测：数据根同时具备 `projects/` 目录与 `workbuddy.db` 才算可用。
pub fn is_available() -> bool {
    let root = data_root();
    root.join("projects").is_dir() && root.join("workbuddy.db").is_file()
}

/// WorkBuddy 会话比对方式（设置项，前端可选）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareMode {
    /// 逐行全量：对所有行做摘要比对（历史行为，权威结论；默认）。
    Full,
    /// 快速：只比「用户提问条数 + 提问内容摘要」（大库可用，权威性低于逐行）。
    Fast,
}

impl Default for CompareMode {
    /// **必须与 serde 缺键时的行为一致**（`settings.json` 里没这个键 ⇒ Full，
    /// 老用户升级后行为不突变）。
    fn default() -> Self {
        Self::Full
    }
}

impl CompareMode {
    /// 存进 `settings.json` / 回传给前端的字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Fast => "fast",
        }
    }

    /// 界面文案（后端给，前端不自己拼）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "逐行全量（权威结论，大库可能超限退化为未比对）",
            Self::Fast => "快速（只比提问条数+提问内容，大库也能自动比对）",
        }
    }

    /// 宽松解析：认不出来返回 `None`，调用方据此报错，**不许静默退回默认值**。
    pub fn parse_loose(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" | "逐行" | "全量" => Some(Self::Full),
            "fast" | "快速" => Some(Self::Fast),
            _ => None,
        }
    }
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
    /// 快速比对口径：真实提问条数（0 = 未算 / 不适用）。
    pub question_count: usize,
    /// 本次结论的比对口径：`full`（逐行）/ `questions`（提问）/ 空串（无结论）。
    ///
    /// 快速口径的「一致」权威性**低于**逐行口径 —— 界面必须能区分，
    /// 否则「提问一致但完整记录不同」会被误读成「完全一样」。
    pub compare_basis: String,
    /// 是否被 WorkBuddy 置顶。
    ///
    /// ⚠️ 这个状态**不在** `workbuddy.db` 里（`sessions` 表没有置顶列，只有
    /// `spaces.pinned` 那是「空间置顶」）。客户端把它存在自己的侧栏快照：
    /// `~/.workbuddy/<uid>/sidebar-list-snapshot.json` → `items[].isPinned`。
    /// 快照是缓存，可能比客户端慢一拍（客户端退出/切账号时才落盘），但这是本机唯一来源。
    pub pinned: bool,
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
    /// 原始 `space_id`（空串 = NULL / 库里没有该列）。
    pub space_id: String,
    /// `space_id` 悬空：非空但不属于本账号的 `spaces` 表。
    ///
    /// 这是跨账号复制会话时的历史污染（2026-11-15 修复前 `override_value` 会把源
    /// 账号的 space_id 原样抄进副本）：客户端空间视图按「本账号 spaces 表」过滤，
    /// 悬空的副本**在界面上不可见**，但比对页按内容比对又是一片绿灯——两边自相矛盾。
    /// 所以必须在比对环节单独点名，不能只报「一致」。
    pub space_dangling: bool,
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

/// **快速比对口径**的单文件体积上限（3 GB）。
///
/// 快速比对只对 `role == "user"` 的行做流式提取与摘要，每字节代价远低于逐行
/// 全量（不用解析整行 JSON 为对象、不做全行哈希），所以上限从 32 MB 放开到
/// 3 GB —— 这正是快速比对的核心价值：288 MB 级的大库自动比对不再退化成
/// 「未比对」。超过它仍然如实说「未比对」，绝不冒充。
const DUP_MAX_BYTES_FAST: u64 = 3 << 30;

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

/// 缓存条目：行摘要（full 口径）或「行 + 提问」双产出（fast 口径）。
///
/// 两种口径**并存**而不是互斥：fast 模式算完的行摘要同时登记到 full 口径的键下，
/// 用户在设置里来回切换比对方式时不必重读盘。键上用 `q|` 前缀区分（见
/// `combined_stamp_cached`），绝不互相覆盖。
enum CachedDigest {
    Lines(ContentId),
    Questions(CombinedStamp),
}

impl Clone for CachedDigest {
    fn clone(&self) -> Self {
        match self {
            CachedDigest::Lines(c) => CachedDigest::Lines(c.clone()),
            CachedDigest::Questions(s) => CachedDigest::Questions(s.clone()),
        }
    }
}

struct DigestCache {
    map: std::collections::HashMap<String, CachedDigest>,
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
    match digest_cache().lock().ok()?.map.get(&key).cloned()? {
        CachedDigest::Lines(cid) => Some(cid),
        CachedDigest::Questions(_) => None,
    }
}

fn content_id_cached(path: &std::path::Path, ids: &[String]) -> Option<ContentId> {

    let key = cache_key(path, ids)?;
    if let Some(CachedDigest::Lines(hit)) =
        digest_cache().lock().ok().and_then(|g| g.map.get(&key).cloned())
    {
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
        guard.map.insert(key, CachedDigest::Lines(id.clone()));
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

// ---------------------------------------------------------------------------
// 快速比对（提问口径）：真实提问条数 + 提问内容摘要
// ---------------------------------------------------------------------------

/// 快速比对里单条提问文本的截断上限（按 char 计，非字节）。
///
/// 覆盖绝大多数提问全文。已知误判面：两份副本在同一条提问的**前 4096 个字符
/// 完全相同、尾部不同**时，快速比对判「相同」—— 只见于「同一会话两份独立手写
/// 且都粘贴了超长内容」；同源同步的副本逐字节相同，不受影响。截断后仍走
/// [`line_digest`] 的长度前缀编码，第 4095 个字符处的差异照样判异。
/// 权威结论仍由行摘要兜底（见 `CombinedStamp` 的双产出设计）。
const QUESTION_TEXT_MAX_CHARS: usize = 4096;

/// 空提取文本（纯图片提问等没有 text 块）的固定占位符。
///
/// **占一条**，不静默丢弃：丢弃会让「有图」与「无图」两份副本因提问条数不同而
/// 误报分叉，或让「都丢」的实现在条数口径上含糊。两边用同一规则占位，
/// 有图 vs 无图照样能判异。
const EMPTY_QUESTION_PLACEHOLDER: &str = "<wb-empty-question>";

/// 提问口径的内容身份：真实提问条数 + 每条提问摘要序列 + 总摘要。
///
/// 与 [`ContentId`] 同构：条目顺序、重复次数都参与摘要（同句问两遍 ≠ 问一遍）。
/// 判据只取「真实用户提问」（[`UserMsgKind::Real`]）—— 内部摘要与自动触发指令
/// 客户端界面上根本不展示为提问，计入只会让条数虚高、两份口径不一致的副本误判。
#[derive(Debug, Clone)]
struct QuestionId {
    questions: usize,
    question_digests: Vec<String>,
    total: String,
}

/// 一次流式扫描的双产出：行摘要（full 口径）+ 提问摘要（fast 口径）。
///
/// fast 模式两者都留：提问判「同」而行摘要不同时，只能展示「提问口径一致」，
/// 绝不冒充逐行权威结论；行摘要也相同则直接升级为 full 权威结论。
#[derive(Debug, Clone)]
struct CombinedStamp {
    lines: ContentId,
    questions: QuestionId,
}

/// 提问文本摘要：id 归一化 → 压空白 + trim → 截断 → 行摘要。
///
/// 压空白是刻意的：两份副本同一条提问只因换行/缩进排版不同就判「分叉」，
/// 对「快速判断两个会话是否相同」这个目标弊大于利。markdown 结构原样保留。
/// 归一化在前、截断在后：占位符替换不改变字符数级联关系。
fn question_text_digest(raw: &str, ids: &[String]) -> String {
    let normalized = normalize_ids(raw, ids);
    let flat: String = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = flat.trim().chars().take(QUESTION_TEXT_MAX_CHARS).collect();
    line_digest(&truncated)
}

/// 提问口径的总摘要：与 [`total_digest`] 同构，版本串区分口径（绝不能与行口径撞值）。
fn question_total_digest(digests: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"wb-switch-questions-v1\0");
    h.update((digests.len() as u64).to_be_bytes());
    for d in digests {
        h.update(d.as_bytes());
        h.update([0u8]);
    }
    to_hex(&h.finalize())
}

/// 双产出核心（纯函数，便于单测）：逐行扫，每行非空就进行摘要；
/// `type=="message" && role=="user"` 且 [`classify_user_message`] 判为 Real 的行，
/// 取真实提问文本入提问序列。
///
/// - 单行损坏只进行摘要、不计提问（与 `parse_body` 同口径：WorkBuddy 偶有截断行，
///   不能让它打断整份文件的比对）；
/// - 空提取文本用 [`EMPTY_QUESTION_PLACEHOLDER`] 占一条；
/// - 全部行都空 ⇒ `None`（与 `content_id` 一致：没有可比内容就老实说没有）。
///
/// ⚠️ 必须复用 [`extract_user_text`]（行首剥注入块 + 认 `<user_query>`），
/// 不能全文搜标签 —— 摘要正文里最多出现 7 个同名标签，全文搜会取出毫不相干的文字。
fn scan_lines<I>(lines: I, ids: &[String]) -> Option<CombinedStamp>
where
    I: Iterator<Item = String>,
{
    let mut line_digests: Vec<String> = Vec::new();
    let mut question_digests: Vec<String> = Vec::new();
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.trim().is_empty() {
            continue;
        }
        line_digests.push(line_digest(&normalize_ids(line, ids)));
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if str_of(&v, "type") != "message" || str_of(&v, "role") != "user" {
            continue;
        }
        let content = v.get("content").cloned().unwrap_or(Value::Null);
        let raw = join_content_blocks(&content);
        if classify_user_message(&raw) != UserMsgKind::Real {
            continue;
        }
        let text = extract_user_text(&raw);
        let digest = if text.is_empty() {
            question_text_digest(EMPTY_QUESTION_PLACEHOLDER, ids)
        } else {
            question_text_digest(&text, ids)
        };
        question_digests.push(digest);
    }
    if line_digests.is_empty() {
        return None;
    }
    let lines_total = total_digest(&line_digests);
    let questions_total = question_total_digest(&question_digests);
    Some(CombinedStamp {
        lines: ContentId {
            lines: line_digests.len(),
            line_digests,
            total: lines_total,
        },
        questions: QuestionId {
            questions: question_digests.len(),
            question_digests,
            total: questions_total,
        },
    })
}

/// 流式读盘版：与 [`content_id`] 一样 1MB BufRead 逐行读，内存与文件大小无关
/// （单份正文实测可达 200 MB 以上）。
fn combined_stamp(path: &std::path::Path, ids: &[String]) -> Option<CombinedStamp> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::with_capacity(1 << 20, file);
    scan_lines(
        reader
            .split(b'\n')
            .filter_map(|r| r.ok().and_then(|b| String::from_utf8(b).ok())),
        ids,
    )
}

/// [`combined_stamp`] 的缓存版。算完同时登记两个键：
/// `q|` 前缀下存双产出（fast 口径），原键下存行摘要（full 口径互切不重读盘）。
fn combined_stamp_cached(path: &std::path::Path, ids: &[String]) -> Option<CombinedStamp> {
    let key = cache_key(path, ids)?;
    let qkey = format!("q|{key}");
    if let Some(CachedDigest::Questions(hit)) = digest_cache()
        .lock()
        .ok()
        .and_then(|g| g.map.get(&qkey).cloned())
    {
        return Some(hit);
    }
    let stamp = combined_stamp(path, ids)?;
    // 估算：行摘要 + 提问摘要各约 48 字节/条 + 两个总摘要 + 结构体开销
    let cost = stamp.lines.line_digests.len() * 48
        + stamp.questions.question_digests.len() * 48
        + stamp.lines.total.len()
        + stamp.questions.total.len()
        + 128;
    if let Ok(mut guard) = digest_cache().lock() {
        if guard.bytes + cost > MAX_CACHED_BYTES {
            guard.map.clear();
            guard.bytes = 0;
        }
        guard.bytes += cost;
        guard.map.insert(qkey, CachedDigest::Questions(stamp.clone()));
        guard.map.insert(key, CachedDigest::Lines(stamp.lines.clone()));
    }
    Some(stamp)
}

/// 快速比对对外出口：`(真实提问条数, 提问总摘要前 16 位)`。
///
/// 与 [`content_stamp`] 同形状，调用方可以按同一套口径记录与展示。
pub fn question_stamp(path: &Path, ids: &[String]) -> Option<(usize, String)> {
    let stamp = combined_stamp_cached(path, ids)?;
    Some((
        stamp.questions.questions,
        stamp.questions.total.chars().take(16).collect(),
    ))
}

/// 双口径对外出口：`(行数, 行摘要, 提问条数, 提问摘要)`，一次读盘算全两种口径。
///
/// 供快速比对（`force_compare` 快速分支）使用：落盘时两种口径都存，之后
/// 无论设置选哪种模式，[`compared_from_disk`] 复用路径都能命中。
pub fn stamp_both(path: &Path, ids: &[String]) -> Option<(usize, String, usize, String)> {
    let stamp = combined_stamp_cached(path, ids)?;
    Some((
        stamp.lines.lines,
        stamp.lines.total.chars().take(16).collect(),
        stamp.questions.questions,
        stamp.questions.total.chars().take(16).collect(),
    ))
}

/// 提问序列关系判定（与 [`relate`] 同构，判据换成提问摘要序列）。
fn relate_questions(a: &QuestionId, b: &QuestionId) -> Relation {
    if a.questions == b.questions && a.total == b.total {
        return Relation::Equal;
    }
    if is_ordered_prefix(&a.question_digests, &b.question_digests) {
        return Relation::Prefix;
    }
    if is_ordered_prefix(&b.question_digests, &a.question_digests) {
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

/// [`decide_group`] 的**快速口径**版：判据换成「提问条数 + 提问摘要序列」，
/// 文案点明口径（提问一致 ≠ 完整记录一致，界面必须能看出来）。
///
/// 判定顺序与 [`decide_group`] 一致：提问序列包含关系优先于时间，分叉按
/// `updated_at`，提问完全相同回落时间并点明口径。
fn decide_group_questions(
    qs: &[Option<QuestionId>],
    updated_at: &[i64],
) -> (usize, Vec<String>) {
    let covering: Vec<usize> = (0..qs.len())
        .filter(|&pos| {
            let Some(q) = qs[pos].as_ref() else {
                return false;
            };
            qs.iter().enumerate().all(|(other, oq)| {
                if other == pos {
                    return true;
                }
                match oq {
                    Some(o) => {
                        o.questions <= q.questions
                            && is_ordered_prefix(&o.question_digests, &q.question_digests)
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
            let better = (updated_at[pos], qs[pos].as_ref().map(|q| q.questions).unwrap_or(0))
                > (updated_at[best], qs[best].as_ref().map(|q| q.questions).unwrap_or(0));
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
        let all: Vec<usize> = (0..qs.len()).collect();
        pick_by_clock(&all)
    };

    let newest_q = qs[newest_pos].clone();
    let notes = qs
        .iter()
        .enumerate()
        .map(|(pos, q)| {
            if pos == newest_pos {
                return String::new(); // 由调用方填「共 N 份」
            }
            match (q, &newest_q) {
                (Some(me), Some(top)) => match relate_questions(me, top) {
                    Relation::Equal => "与最新副本提问内容一致（快速比对口径）".to_string(),
                    Relation::Prefix => {
                        format!("比最新副本少 {} 条提问", top.questions - me.questions)
                    }
                    Relation::Extension => {
                        format!("比最新副本多 {} 条提问", me.questions - top.questions)
                    }
                    Relation::Diverged => format!(
                        "提问内容分叉（本副本 {} 条 / 最新 {} 条）",
                        me.questions, top.questions
                    ),
                },
                (None, _) => "无正文，不参与比对".to_string(),
                (Some(me), None) => format!("{} 条提问（最新副本无正文）", me.questions),
            }
        })
        .collect();
    (newest_pos, notes)
}

// ---------------------------------------------------------------------------
// 同步后的回读校验 / 「已校验一致」记录
// ---------------------------------------------------------------------------

/// 读回**一份**正文的 `(行数, 摘要)`（16 位十六进制，与 `WbSession::content_digest` 同口径）。
///
/// 用途：切号前要判定「这一组里谁最新」时，必须拿到**每份**的摘要 ——
/// `verify_same_content` 只回答「是否都跟第一份一样」，判不出谁更靠前。
/// 读完会进缓存，紧接着的 `collect()` 不必再读一遍。
pub fn content_stamp(path: &Path, ids: &[String]) -> Option<(usize, String)> {
    let cid = content_id_cached(path, ids)?;
    Some((cid.lines, cid.total.chars().take(16).collect()))
}

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
    /// 快速比对口径：真实提问条数（`None` = 只有逐行结论的旧记录 / 本次只算了行口径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    q_lines: Option<usize>,
    /// 快速比对口径总摘要（16 位十六进制）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    q_digest: Option<String>,
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

/// 落盘结论的封顶条数。
///
/// ⚠️ 为什么不是 200：快速比对模式会给**每次重算的每个成员**落盘结论，一轮
/// 列表就是几百条写入。封顶 200 时同一轮里后写的组会把先写的挤掉 —— 下一轮
/// 冷启动 [1] 阶段重算标题簇、写入时又把关联组的结论挤掉…… 备忘录永远追着
/// 自己的尾巴，**所有组的落盘复用全部失效**，冷启动永远重算（2026-11-16 实测：
/// 760 会话 / 508 组，store 里只剩最近 ~40 秒的写入，冷启动 142s 且每轮如此）。
/// 放宽到能容纳全部会话的量级（760 会话 × 双口径约 120 字节/条 ≈ 1 MB JSON），
/// 驱逐只在超出一整轮写入量时才发生。
const VERIFIED_MAX_ENTRIES: usize = 4000;

fn save_verified_at(path: &Path, map: &std::collections::HashMap<String, VerifiedEntry>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 只留最近的：这是「免重复读」的备忘，不是账本。
    let mut items: Vec<(&String, &VerifiedEntry)> = map.iter().collect();
    if items.len() > VERIFIED_MAX_ENTRIES {
        items.sort_by_key(|(_, e)| std::cmp::Reverse(e.at));
        items.truncate(VERIFIED_MAX_ENTRIES);
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
                // 同步回读校验是 full 口径的权威结论，不写提问口径字段
                q_lines: None,
                q_digest: None,
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
/// 输入是「会话 id、正文路径、两种口径的摘要」（**不重新读盘**，摘要由调用方
/// 从刚算完的列表里带来）；行摘要为空的成员跳过（没有可信结论可存）。
/// 提问口径（`q_*`）可空：只算过行口径时写出的记录不带 q 字段，向后兼容。
/// 返回实际登记的条数。
pub fn mark_compared(values: &[StampRecord]) -> usize {
    mark_compared_at(&verified_store_path(), values)
}

/// 落盘比对结论的单条输入（调用方从刚算完的列表带来，不重新读盘）。
#[derive(Debug, Clone)]
pub struct StampRecord {
    pub id: String,
    pub path: PathBuf,
    /// 行口径：行数 + 总摘要（16 位）。
    pub lines: usize,
    pub digest: String,
    /// 提问口径：真实提问条数 + 总摘要（16 位）。`None` = 本次没算提问口径。
    pub q_lines: Option<usize>,
    pub q_digest: Option<String>,
}

/// 从磁盘取回的**单个成员**的落盘比对结论（指纹已核对一致）。
///
/// `q_*` 为 `None` 表示该记录只有行口径（旧记录 / 只做过 full 比对）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparedStamp {
    pub lines: usize,
    pub digest: String,
    pub q_lines: Option<usize>,
    pub q_digest: Option<String>,
}

fn mark_compared_at(path: &Path, values: &[StampRecord]) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut map = load_verified_at(path);
    let at = chrono::Local::now().timestamp_millis();
    let mut n = 0;
    for v in values {
        if v.digest.is_empty() {
            continue;
        }
        let Some((len, mtime)) = file_stamp(&v.path) else {
            continue;
        };
        map.insert(
            v.id.clone(),
            VerifiedEntry {
                len,
                mtime,
                lines: v.lines,
                digest: v.digest.clone(),
                q_lines: v.q_lines,
                q_digest: v.q_digest.clone(),
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
) -> Option<Vec<Option<ComparedStamp>>> {
    compared_from_disk_at(&verified_store_path(), pairs)
}

fn compared_from_disk_at(
    path: &Path,
    pairs: &[(String, PathBuf)],
) -> Option<Vec<Option<ComparedStamp>>> {
    if pairs.len() < 2 {
        return None;
    }
    let map = load_verified_at(path);
    let mut hit = 0usize;
    let mut out: Vec<Option<ComparedStamp>> = Vec::with_capacity(pairs.len());
    for (id, p) in pairs {
        let ok = (|| -> Option<ComparedStamp> {
            let e = map.get(id)?;
            let (len, mtime) = file_stamp(p)?;
            if e.len != len || e.mtime != mtime {
                return None; // 这份文件变了 ⇒ 只有它的结论作废
            }
            if e.digest.is_empty() {
                return None;
            }
            Some(ComparedStamp {
                lines: e.lines,
                digest: e.digest.clone(),
                q_lines: e.q_lines,
                q_digest: e.q_digest.clone(),
            })
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
    /// 快速口径的体积合规（≤ [`DUP_MAX_BYTES_FAST`]）。
    fast_fit: bool,
    all_cached: bool,
    recorded: bool,
}

/// 这一组该怎么下结论。
#[derive(Debug, PartialEq, Eq)]
enum Decide {
    /// 用户点了「强制比对」：不管上限，真读（逐行还是快速跟随设置）。
    Forced,
    /// 体积/成员数都在上限内：逐行比对（常规路径）。
    Compare,
    /// 快速口径：体积在 3 GB 内且设置选了快速 ⇒ 只比提问条数+提问内容。
    CompareFast,
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
/// - 快速模式：**免费的结论优先**（回读校验记录 / 内存缓存的逐行摘要 ——
///   都不读盘且更权威），然后才是 `CompareFast`（3 GB 内快速重算）；
/// - 逐行模式：「体积合规」就重算（≤32 MB 便宜且权威），复用只救超限的组。
///
/// 抽出来是因为这里一旦写歪，界面就会重新出现「明明一致却写未比对」
/// 或更糟的「没比对却说一致」——后者是绝对不能有的。
fn decide_mode(force: bool, fast: bool, f: &GroupFacts) -> Decide {
    if force {
        return Decide::Forced;
    }
    if f.members > DUP_MAX_MEMBERS || !f.all_body {
        return Decide::Unknown;
    }
    if fast {
        // 快速模式下「重算」并不便宜（一组可能要读上 GB），所以免费的权威结论
        // 优先：回读校验记录 / 内存缓存里的逐行摘要，都命中就不读一个字节。
        if f.recorded {
            return Decide::ReuseRecord;
        }
        if f.all_cached {
            return Decide::ReuseCache;
        }
        if f.fast_fit {
            return Decide::CompareFast;
        }
        // 超 3GB 且没有任何复用 ⇒ 落盘的「上次比对」结论（外面接住）或不知道。
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
        list[i].question_count = 0;
        list[i].compare_basis = "full".to_string();
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
    per: &[Option<ComparedStamp>],
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
    let base = per[newest].as_ref().map(|s| s.digest.clone()).unwrap_or_default();
    // 全部命中且行摘要全同 ⇒ 与「同步后回读校验一致」同形状（文案也保持一致）。
    let hit = per.iter().flatten().count();
    if hit == per.len() && per.iter().flatten().all(|s| s.digest == base) {
        let lines = per[newest].as_ref().map(|s| s.lines).unwrap_or(0);
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
    // 行摘要不全同，但**快速口径**全命中且全同 ⇒ 只能给「提问口径一致」。
    // 行级摘要不同说明完整记录（助手回答 / 工具调用 / 中间叙述等）有差异，
    // 绝不冒充逐行权威结论 —— 权威性差异必须让界面看得出来。
    let all_q = hit == per.len()
        && per.iter().flatten().all(|s| s.q_digest.as_deref().map(|d| !d.is_empty()).unwrap_or(false));
    if all_q {
        let base_q = per[newest]
            .as_ref()
            .and_then(|s| s.q_digest.clone())
            .unwrap_or_default();
        if per
            .iter()
            .flatten()
            .all(|s| s.q_digest.as_deref() == Some(base_q.as_str()))
        {
            let label: String = key.chars().take(16).collect();
            for (pos, &i) in idx.iter().enumerate() {
                let stamp = per[pos].as_ref().expect("all_q 已保证全命中");
                list[i].dup_group = label.clone();
                list[i].body_lines = stamp.lines;
                list[i].content_digest = stamp.digest.chars().take(16).collect();
                list[i].question_count = stamp.q_lines.unwrap_or(0);
                list[i].compare_basis = "questions".to_string();
                list[i].is_newest = pos == newest;
                list[i].dup_relation = if pos == newest {
                    "latest"
                } else {
                    "equal"
                }
                .to_string();
                list[i].dup_note = if pos == newest {
                    format!("共 {} 份重复标题副本（上次快速比对的结论）", idx.len())
                } else {
                    "与最新副本提问内容一致（快速比对口径；完整记录有差异，需要时再逐行比对）"
                        .to_string()
                };
            }
            return;
        }
    }
    let label: String = key.chars().take(16).collect();
    for (pos, &i) in idx.iter().enumerate() {
        list[i].dup_group = label.clone();
        match &per[pos] {
            Some(stamp) => {
                list[i].body_lines = stamp.lines;
                list[i].content_digest = stamp.digest.chars().take(16).collect();
                list[i].question_count = stamp.q_lines.unwrap_or(0);
                list[i].compare_basis = "full".to_string();
                list[i].is_newest = pos == newest;
                list[i].dup_relation = if pos == newest {
                    "latest"
                } else if stamp.digest == base {
                    "equal"
                } else {
                    "diverged"
                }
                .to_string();
                list[i].dup_note = if pos == newest {
                    format!("共 {} 份重复标题副本（上次比对的结论）", idx.len())
                } else if stamp.digest == base {
                    "与最新副本内容相同（上次强制比对的结论）".to_string()
                } else {
                    "上次强制比对：内容不同。要判定谁包含谁，请再次强制比对".to_string()
                };
            }
            None => {
                // 这份在上次比对之后有新内容 ⇒ 只有它的结论作废。
                list[i].is_newest = false;
                list[i].content_digest.clear();
                list[i].question_count = 0;
                list[i].compare_basis.clear();
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
/// 比对方式是否为快速口径 —— 给其它模块用的薄封装（唯一出口仍是 settings）。
pub fn settings_compare_mode_is_fast() -> bool {
    settings::workbuddy_session_compare_mode() == CompareMode::Fast
}

fn annotate_duplicates(list: &mut [WbSession]) {
    annotate_duplicates_opt(list, false, settings_compare_mode_is_fast())
}

/// [`annotate_duplicates`] 的带开关版本。
///
/// `force = true` 时**忽略体积上限与成员数上限**，真的逐行读完整正文做比对。
///
/// 为什么要这个开关：体积超限时默认不比对（一份 227 MB 的正文读一次要好几秒，
/// 20 个副本一次列表就 1 GB / 35 秒 —— 不能让「打开列表」被它拖死）。
/// 但用户明确要求「支持手动确定后执行判定分析」：他自己决定等，就真的要比。
/// 所以这里是**用户显式触发**才走的慢路径，默认路径的性能不受影响。
///
/// `fast = true`（设置选了「快速比对」）时：比对判据换成「提问条数 + 提问内容」，
/// 体积上限放宽到 3 GB；`force` 也跟随这个口径（用户确认过的口径）。
/// 快速口径的权威性**低于**逐行 —— 结论一律带 `compare_basis = "questions"`，
/// 界面据此区分。
fn annotate_duplicates_opt(list: &mut [WbSession], force: bool, fast: bool) {
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
        annotate_cluster(list, &idx, force, fast, &key);
    }

    // 非重复项：没有分组，视为「唯一副本」
    for s in list.iter_mut() {
        if s.dup_group.is_empty() {
            s.is_newest = true;
            s.dup_note = String::new();
        }
    }
}

/// 对「同一簇」的成员（`idx` 是 `list` 的下标）做一次比对，就地填写 dup_* 结论字段。
///
/// 从 [`annotate_duplicates_opt`] 抽出来的单簇逻辑：同标题聚类与正式关联组
/// （[`annotate_sessions_group`]）共用同一套判据 / 复用 / 落盘路径，避免两处
/// 各写一份然后悄悄走样。
///
/// * `key` 只用作 `dup_group` 标签（截 16 位）与 trace 输出；
/// * `force` / `fast` 语义与 [`annotate_duplicates_opt`] 相同；
/// * 结论已可直接复用（同步回读 / 落盘记录）时提前返回，不做多余计算。
fn annotate_cluster(list: &mut [WbSession], idx: &[usize], force: bool, fast: bool, key: &str) {
        // 逐行摘要只对「有正文且在体积上限内」的成员计算
        let mut ids: Vec<Option<ContentId>> = Vec::with_capacity(idx.len());
        // 快速口径的提问身份（fast 模式才填充；与 ids 一一对应）
        let mut qs: Vec<Option<QuestionId>> = Vec::with_capacity(idx.len());
        // ⚠️ 归一化要用**整组**的 id：正文可能引用组内其它副本的 id（实测有过），
        // 只换自己那个会让「刚同步完的副本」互相比对不上，误报「有分歧」。
        let group_ids: Vec<String> = idx.iter().map(|&i| list[i].id.clone()).collect();
        // ---- 这一组到底能不能下结论 -------------------------------------------
        // 事实先算出来（贵的两项只在「体积超限」时才去碰，常见路径零额外 IO）。
        let all_body = idx.iter().all(|&i| list[i].has_body);
        let bytes_fit = |limit: u64| all_body && idx.iter().all(|&i| list[i].body_bytes <= limit);
        let all_fit = bytes_fit(DUP_MAX_BYTES);
        let fast_fit = bytes_fit(DUP_MAX_BYTES_FAST);
        let mut all_cached = false;
        let mut recorded: Option<(usize, String)> = None;
        let mut from_disk: Option<Vec<Option<ComparedStamp>>> = None;
        let mut pairs_n = 0usize;
        if !force && all_body && (fast || !all_fit) && idx.len() <= DUP_MAX_MEMBERS {
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
                    pairs_n = pairs.len();
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
            fast,
            &GroupFacts {
                members: idx.len(),
                all_body,
                all_fit,
                fast_fit,
                all_cached,
                recorded: recorded.is_some(),
            },
        );
        // 阶段计时（WB_GROUPS_TRACE=1 时输出；排查「关联会话」页加载慢用）。
        let trace = std::env::var("WB_GROUPS_TRACE").as_deref() == Ok("1");
        let t_stage = std::time::Instant::now();
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
                return;
            }
        }
        // 内存缓存没命中、但落盘结论还在（指纹没变）⇒ 不读一个字节直接复用。
        // 只在非强制路径生效：强制比对永远重算，绝不吃旧结论。
        if !force {
            if let Some(per) = from_disk {
                apply_compared_from_disk(list, &idx, &key, &per);
                return;
            }
        }
        let detailed = matches!(
            mode,
            Decide::Forced | Decide::Compare | Decide::CompareFast | Decide::ReuseCache
        );
        // 本组是否走快速口径（设置选了 fast，且本次确实要算 —— Forced/CompareFast）。
        let fast_compare = fast
            && matches!(mode, Decide::Forced | Decide::CompareFast);
        for &i in idx {
            if !list[i].has_body {
                ids.push(None);
                qs.push(None);
                continue;
            }
            if detailed {
                let path = find_jsonl(&list[i].id);
                if fast_compare {
                    // 快速口径：一次读盘同时算全行+提问两种口径（落盘后两条
                    // 复用路径都能命中）。行口径照样填，字段不浪费。
                    let stamp = path.and_then(|p| combined_stamp_cached(&p, &group_ids));
                    qs.push(stamp.as_ref().map(|s| s.questions.clone()));
                    ids.push(stamp.map(|s| s.lines));
                } else {
                    let id = path.and_then(|p| content_id_cached(&p, &group_ids));
                    qs.push(None);
                    ids.push(id);
                }
            } else {
                ids.push(None);
                qs.push(None);
            }
            if let (Some(cid), j) = (ids.last().and_then(|v| v.as_ref()), i) {
                list[j].body_lines = cid.lines;
                list[j].content_digest = cid.total.chars().take(16).collect();
            }
        }

        let updated: Vec<i64> = idx.iter().map(|&i| list[i].updated_at).collect();
        let (newest_pos, mut notes) = if fast_compare {
            decide_group_questions(&qs, &updated)
        } else {
            decide_group(&ids, &updated)
        };
        notes[newest_pos] = if fast_compare {
            format!("共 {} 份重复标题副本（快速比对口径）", idx.len())
        } else {
            format!("共 {} 份重复标题副本", idx.len())
        };
        if !detailed {
            // 没做比对时，把**为什么没做**写进备注：界面显示「无法确认」时必须
            // 能回答「为什么」，否则用户只会觉得工具坏了。
            let why = if idx.iter().any(|&i| !list[i].has_body) {
                "有副本没有正文文件，未做比对".to_string()
            } else if idx.len() > DUP_MAX_MEMBERS {
                format!(
                    "同标题副本有 {} 份（比对上限 {} 份），未做比对",
                    idx.len(),
                    DUP_MAX_MEMBERS
                )
            } else if fast {
                format!(
                    "有副本体积超过 {} MB，快速比对也未做（可点「强制比对」重试）",
                    DUP_MAX_BYTES_FAST >> 20
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
        // `diverged` = 各写各的（必须人工选来源）。没做比对时一律 `unknown`，
        // 绝不能因为「没算」就说成「无正文」。快速口径判据换成提问序列，
        // 权威性低一档，界面按 `compare_basis` 区分。
        let rels: Vec<&'static str> = if !detailed {
            vec!["unknown"; ids.len()]
        } else if fast_compare {
            let top = qs.get(newest_pos).and_then(|v| v.as_ref());
            qs.iter()
                .enumerate()
                .map(|(pos, q)| match (q, top) {
                    (None, _) => "no-body",
                    (Some(_), None) => "sole-body",
                    (Some(_), Some(_)) if pos == newest_pos => "latest",
                    (Some(me), Some(top)) => match relate_questions(me, top) {
                        Relation::Equal => "equal",
                        Relation::Prefix => "behind",
                        Relation::Extension => "ahead",
                        Relation::Diverged => "diverged",
                    },
                })
                .collect()
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
            if fast_compare {
                list[i].compare_basis = "questions".to_string();
                list[i].question_count = qs[pos].as_ref().map(|q| q.questions).unwrap_or(0);
            } else if detailed {
                list[i].compare_basis = "full".to_string();
                list[i].question_count = 0;
            } else {
                list[i].compare_basis.clear();
                list[i].question_count = 0;
            }
        }

        // 快速口径的结论**自动落盘**：行 + 提问两种口径都存 —— 大库自动比对
        // 每次重算也要读 3 GB 级文件，落盘后重启/刷新直接复用，不再白算。
        // （逐行全量路径维持既有行为：落盘只发生在强制比对 / 同步回读校验。）
        if fast_compare {
            let values: Vec<StampRecord> = idx
                .iter()
                .enumerate()
                .filter_map(|(pos, &i)| {
                    let cid = ids[pos].as_ref()?;
                    let q = qs[pos].as_ref()?;
                    let path = find_jsonl(&list[i].id)?;
                    Some(StampRecord {
                        id: list[i].id.clone(),
                        path,
                        lines: cid.lines,
                        digest: cid.total.chars().take(16).collect(),
                        q_lines: Some(q.questions),
                        q_digest: Some(q.total.chars().take(16).collect()),
                    })
                })
                .collect();
            if !values.is_empty() {
                mark_compared(&values);
            }
        }
        if trace {
            let members: Vec<String> = idx
                .iter()
                .map(|&i| format!("{}({}MB)", &list[i].id[..8.min(list[i].id.len())], list[i].body_bytes >> 20))
                .collect();
            eprintln!(
                "[wb-groups-trace] 组「{}」 mode={:?} 耗时 {:.2?} 复用(缓存={} 落盘校验={} 落盘记录={:?} pairs={}/{}) 成员 {:?}",
                key,
                mode,
                t_stage.elapsed(),
                all_cached,
                from_disk.is_some(),
                recorded.as_ref().map(|(_, d)| d.to_string()),
                pairs_n,
                idx.len(),
                members
            );
        }
}

/// 关联组专用比对：把「正式关联组」的成员当成一个簇直接比对，**不依赖标题聚类**。
///
/// 为什么需要它：正式关联组的成员关系是用户权威指定的（标题改过也照认），但
/// 比对历史上只发生在「同标题聚类」里 —— 任一成员的标题被改过，它就不在任何
/// 同标题簇里、从来没被比对过，整组在界面上只能退化成「无法确认」（2026-11-16
/// 用户截图：4 份副本 3 份相同、1 份「条数未知」，体积远没超限，跟比对上限无关）。
///
/// 判据 / 复用 / 落盘与同标题簇完全一致（见 [`annotate_cluster`]）；比对范围只
/// 用关联组自己的成员 id 做归一化，不受组外同标题副本影响。
pub fn annotate_sessions_group(sessions: &mut [WbSession], force: bool, fast: bool) {
    if sessions.len() < 2 {
        return;
    }
    let idx: Vec<usize> = (0..sessions.len()).collect();
    annotate_cluster(sessions, &idx, force, fast, "关联组");
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
             deleted_at, model, space_id FROM sessions",
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
            let space_id: Option<String> = r.get(9).ok();
            Ok((id, cwd, user_id, title, custom, created, updated, deleted, model, space_id))
        })
        .map_err(|e| format!("读取 sessions 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("解析 sessions 行失败: {e}"))?;
    drop(stmt);
    // 各账号自己的空间 id 集合（旧库可能没有 spaces 表 → 视为「无从判断」，不算悬空）。
    let space_owner: std::collections::HashMap<String, std::collections::HashSet<String>> =
        load_space_owners(&conn);
    drop(conn);

    let mut out: Vec<WbSession> = Vec::with_capacity(rows.len());
    // 每个账号的置顶快照只读一次（快照按账号分目录）。
    let mut pinned_cache: std::collections::HashMap<String, std::collections::HashMap<String, bool>> =
        std::collections::HashMap::new();
    for (id, cwd, user_id, title, custom, created, updated, deleted, model, space_id) in rows {
        let body = find_jsonl(&id);
        let body_bytes = body
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        // 置顶取自该账号的侧栏快照（按 uid 分目录，一个账号只读一次）。
        let sid = user_id.clone().unwrap_or_default();
        // 置顶取自该账号的侧栏快照（按 uid 分目录，一个账号只读一次）。
        let pinned = pinned_cache
            .entry(sid.clone())
            .or_insert_with(|| pinned_snapshot(&sid))
            .get(&id)
            .copied()
            .unwrap_or(false);
        let space_raw = space_id.unwrap_or_default();
        let space_dangling = !space_raw.trim().is_empty()
            && space_owner
                .get(&sid)
                .map(|own| !own.contains(&space_raw))
                .unwrap_or(false);
        out.push(WbSession {
            title: custom
                .or(title)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "(无标题)".to_string()),
            id,
            cwd: cwd.unwrap_or_default(),
            user_id: sid.clone(),
            created_at: created,
            updated_at: updated,
            model: model.unwrap_or_default(),
            deleted: deleted.map(|d| d != 0).unwrap_or(false),
            has_body: body.is_some(),
            pinned,
            body_bytes,
            body_lines: 0,
            content_digest: String::new(),
            question_count: 0,
            compare_basis: String::new(),
            dup_group: String::new(),
            is_newest: true,
            dup_note: String::new(),
            dup_relation: String::new(),
            space_id: space_raw,
            space_dangling,
        });
    }
    // 置顶优先，其次按最后更新时间倒序（与客户端列表观感一致）
    out.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| b.updated_at.cmp(&a.updated_at)));
    // 同标题副本的新旧判定（多账号时同一段对话会有多份，必须能分清谁是最新）。
    // 不重排列表：重复副本的时间本来就接近，天然相邻，全局「最近优先」的顺序更顺手。
    annotate_duplicates(&mut out);
    annotate_space_dangling(&mut out);
    Ok(out)
}

/// [`list_sessions`] 的**强制比对**版本：忽略体积/成员数上限，读完整正文。
///
/// 只给「用户手动点了强制比对」用（见 `annotate_duplicates_opt` 的注释）。
/// 慢是预期行为：一份 227 MB 的正文要读几秒，界面必须自己显示「正在分析」。
///
/// 比对口径跟随设置（[`settings::workbuddy_session_compare_mode`]）：
/// 快速模式下强制比对也只比「提问条数 + 提问内容」—— 用户已确认这个口径。
pub fn list_sessions_forced() -> Result<Vec<WbSession>, String> {
    let fast = settings_compare_mode_is_fast();
    let mut out = list_sessions()?;
    annotate_duplicates_opt(&mut out, true, fast);
    annotate_space_dangling(&mut out);
    Ok(out)
}

/// 读各账号自己的空间 id 集合：`spaces` 表按 `user_id` 归属。
///
/// 旧版库里可能没有 `spaces` 表（客户端还没在任何一个账号下 seed 过）：
/// 返回空表，调用方把「无从判断」当「不悬空」处理，绝不能让检查报错。
fn load_space_owners(
    conn: &rusqlite::Connection,
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let mut out: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT id, user_id FROM spaces") else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        let (id, uid) = row;
        if let Some(uid) = uid {
            if !uid.is_empty() {
                out.entry(uid).or_default().insert(id);
            }
        }
    }
    out
}

/// 给 `space_dangling` 的成员追加备注（**追加**，不覆盖重复比对已写好的说明）。
///
/// 在 `annotate_duplicates` / `annotate_duplicates_opt` 之后调用——两条会话列表
/// 路径都必须走到，否则「强制比对」会把悬空提示冲掉，界面又只剩绿灯。
fn annotate_space_dangling(list: &mut [WbSession]) {
    const NOTE: &str = "空间归属异常：挂的是别的账号的空间，客户端空间视图里看不到";
    for s in list.iter_mut() {
        if s.space_dangling {
            if s.dup_note.is_empty() {
                s.dup_note = NOTE.to_string();
            } else {
                s.dup_note.push_str("；");
                s.dup_note.push_str(NOTE);
            }
        }
    }
}

/// 读某账号的「会话置顶快照」：`~/.workbuddy/<uid>/sidebar-list-snapshot.json`。
///
/// ⚠️ 这是客户端自己落的**缓存**，不是权威表：`workbuddy.db` 的 `sessions` 表里
/// 根本没有置顶列（`spaces.pinned` 是「空间置顶」，不是会话）。所以：
/// - 文件不存在 / 解析失败 ⇒ 一律返回空表（当没置顶），绝不让它阻塞会话列表；
/// - 想让客户端把最新置顶写下来，需要在客户端里退出一次（它退出时才落盘）。
/// 宁可显示「没置顶」，也不能因为读不到快照就让整个列表报错。
pub fn pinned_snapshot(uid: &str) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    if uid.trim().is_empty() {
        return out;
    }
    let path = data_root().join(uid).join("sidebar-list-snapshot.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return out;
    };
    let Some(items) = v.get("items").and_then(Value::as_array) else {
        return out;
    };
    for it in items {
        let (Some(id), Some(pinned)) = (
            it.get("id").and_then(Value::as_str),
            it.get("isPinned").and_then(Value::as_bool),
        ) else {
            continue;
        };
        if pinned {
            out.insert(id.to_string(), true);
        }
    }
    out
}

/// 在 `projects/` 下任意子目录中定位 `{会话id}.jsonl`。
pub fn find_jsonl(session_id: &str) -> Option<PathBuf> {
    // 快路径：查索引。命中还要 `is_file()` 校验一次 —— 文件可能在 TTL 窗口内
    // 被删掉，拿一个不存在的路径去读会让「有正文」误判。一次 stat 换正确性，值。
    if let Some(map) = jsonl_index() {
        if let Some(p) = map.get(session_id) {
            if p.is_file() {
                return Some(p.clone());
            }
        }
    }
    // 慢路径：兜底走一遍目录（新会话正文刚写出来、索引还没刷新时走这里），
    // 找到就回填进索引自愈，之后同 id 不再走目录。
    let found = walk_jsonl(session_id);
    if found.is_some() {
        heal_jsonl_index(session_id, found.as_ref());
    }
    found
}

/// `projects/` 下全部 `.jsonl` 的索引缓存（stem → 路径）。
///
/// 为什么需要：`list_sessions` 要对**每个会话**各找一次正文，而旧实现每次都
/// `read_dir(projects)` + 逐目录 stat —— 本机 758 个会话 × 22 个子目录 ≈ 3.4 万次
/// 文件系统调用，仅这一步就耗掉数秒（「关联会话」页打开慢的主因）。
/// 索引把成本摊成一次全树遍历（约 800 次），之后 O(1) 查表。
///
/// TTL 只有 2 秒：宁可偶尔重建，也不冒「正文刚写出来却查不到」的险。
/// 索引命中的 `is_file()` 校验 + miss 后兜底自愈保证正确性与旧实现一致。
static JSONL_INDEX: std::sync::Mutex<Option<(std::time::Instant, std::collections::HashMap<String, PathBuf>)>> =
    std::sync::Mutex::new(None);
const JSONL_INDEX_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// 取当前有效的索引；过期就重建。构建失败（无 projects/）返回 `None`。
fn jsonl_index() -> Option<std::collections::HashMap<String, PathBuf>> {
    let mut guard = JSONL_INDEX.lock().ok()?;
    let fresh = match guard.as_ref() {
        Some((at, map)) if at.elapsed() < JSONL_INDEX_TTL => Some(map.clone()),
        _ => None,
    };
    if let Some(map) = fresh {
        return Some(map);
    }
    let map = build_jsonl_index()?;
    *guard = Some((std::time::Instant::now(), map.clone()));
    Some(map)
}

/// 一次性遍历 `projects/` 建「会话 id → 正文路径」索引。
///
/// 规则与 [`walk_jsonl`] 一致：根下的 `.jsonl` 优先，其次一级子目录（先到先得）。
fn build_jsonl_index() -> Option<std::collections::HashMap<String, PathBuf>> {
    let projects = data_root().join("projects");
    if !projects.is_dir() {
        return None;
    }
    let mut map = std::collections::HashMap::new();
    for entry in std::fs::read_dir(&projects).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            // 读不了的子目录跳过（不因一个目录失败就放弃整棵树）。
            if let Ok(rd) = std::fs::read_dir(&p) {
                for sub in rd.flatten() {
                    let sp = sub.path();
                    if let Some(stem) = jsonl_stem(&sp) {
                        map.entry(stem).or_insert(sp);
                    }
                }
            }
        } else if let Some(stem) = jsonl_stem(&p) {
            map.insert(stem, p);
        }
    }
    Some(map)
}

/// `.jsonl` 文件的 stem（即会话 id）；非 `.jsonl` 文件返回 `None`。
fn jsonl_stem(p: &Path) -> Option<String> {
    let name = p.file_name()?.to_str()?;
    let stem = name.strip_suffix(".jsonl")?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_string())
}

/// 把刚找到的正文路径回填进还活着的索引（miss 自愈；索引过期了就算了，
/// 下次重建自然会带上）。
fn heal_jsonl_index(session_id: &str, path: Option<&PathBuf>) {
    let Ok(mut guard) = JSONL_INDEX.lock() else {
        return;
    };
    let Some((at, map)) = guard.as_mut() else {
        return;
    };
    if at.elapsed() >= JSONL_INDEX_TTL {
        return;
    }
    match path {
        Some(p) => {
            map.insert(session_id.to_string(), p.clone());
        }
        None => {
            map.remove(session_id);
        }
    }
}

/// 旧实现：逐目录找 `{session_id}.jsonl`。只做兜底与测试基准。
fn walk_jsonl(session_id: &str) -> Option<PathBuf> {
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

    /// 行口径落盘结论（无提问口径）。
    fn cs(lines: usize, digest: &str) -> Option<ComparedStamp> {
        Some(ComparedStamp {
            lines,
            digest: digest.to_string(),
            q_lines: None,
            q_digest: None,
        })
    }

    /// 双口径落盘结论（行 + 提问）。
    fn csq(lines: usize, digest: &str, q_lines: usize, q_digest: &str) -> Option<ComparedStamp> {
        Some(ComparedStamp {
            lines,
            digest: digest.to_string(),
            q_lines: Some(q_lines),
            q_digest: Some(q_digest.to_string()),
        })
    }

    /// 落盘登记的单条输入。
    fn rec(id: &str, path: &Path, lines: usize, digest: &str) -> StampRecord {
        StampRecord {
            id: id.to_string(),
            path: path.to_path_buf(),
            lines,
            digest: digest.to_string(),
            q_lines: None,
            q_digest: None,
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
            pinned: false,
            body_bytes: if has_body { 10 } else { 0 },
            body_lines: 0,
            content_digest: String::new(),
            question_count: 0,
            compare_basis: String::new(),
            dup_group: String::new(),
            is_newest: true,
            dup_note: String::new(),
            dup_relation: String::new(),
            space_id: String::new(),
            space_dangling: false,
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
            rec("aaaa", &a, 1, "aaaaaaaaaaaaaaaa"),
            rec("bbbb", &b, 2, "bbbbbbbbbbbbbbbb"),
        ];

        assert!(compared_from_disk_at(&store, &pairs).is_none(), "没登记过 = 不知道");
        assert_eq!(mark_compared_at(&store, &values), 2, "两条都该登记上");

        // 摘要不同：verified_after_sync（只认全同）不命中；compared_from_disk 带回各自结论
        assert!(verified_after_sync_at(&store, &pairs).is_none());
        let hit = compared_from_disk_at(&store, &pairs).expect("指纹没变就该复用");
        assert_eq!(
            hit,
            vec![cs(1, "aaaaaaaaaaaaaaaa"), cs(2, "bbbbbbbbbbbbbbbb")],
        );

        // 摘要全相同时两条出口都要命中（同步回读与强制比对的结论互通）
        let same = vec![
            rec("aaaa", &a, 1, "dddddddddddddddd"),
            rec("bbbb", &b, 1, "dddddddddddddddd"),
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
            Some(vec![cs(1, "dddddddddddddddd"), None]),
            "动过的那份不许拿旧结论冒充，没动过的必须复用"
        );
        assert!(verified_after_sync_at(&store, &pairs).is_none(), "全组一致不再成立");

        // 只有一份成员 ⇒ 不够下结论
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
            vec![cs(1, "dddddddddddddddd"), None],
            "改过的那份作废，没改的照常复用"
        );

        // 空摘要跳过不登记
        let empty = vec![rec("cccc", &a, 1, "")];
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
            &[cs(7, "0123456789abcdef"), cs(7, "0123456789abcdef")],
        );
        assert!(list[1].is_newest, "updated_at 大的判最新");
        assert_eq!(list[0].dup_relation, "equal");
        assert_eq!(list[1].dup_relation, "latest");
        assert_eq!(list[0].body_lines, 7);
        assert_eq!(list[0].compare_basis, "full", "行级结论必须标 full 口径");
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
            &[cs(3, "aaaaaaaaaaaaaaaa"), cs(5, "bbbbbbbbbbbbbbbb")],
        );
        assert_eq!(list[1].dup_relation, "latest");
        assert_eq!(list[0].dup_relation, "diverged", "摘要不同且无行级信息 ⇒ 只能说 diverged");
        assert_eq!(list[0].body_lines, 3);
        assert_eq!(list[0].content_digest, "aaaaaaaaaaaaaaaa");
        assert!(list[0].dup_note.contains("内容不同"), "{}", list[0].dup_note);
    }

    /// 快速口径全同、行级不全同 ⇒ 只能给「提问口径一致」，绝不冒充逐行权威；
    /// `compare_basis` 必须标 `questions`，条数与备注都要带口径字样。
    #[test]
    fn apply_compared_from_disk_questions_equal_reports_fast_basis() {
        let mut list = vec![mk("a", "T", 100, true), mk("b", "T", 200, true)];
        let idx = vec![0usize, 1usize];
        apply_compared_from_disk(
            &mut list,
            &idx,
            "T",
            &[csq(3, "aaaaaaaaaaaaaaaa", 5, "qqqqqqqqqqqqqqqq"), csq(5, "bbbbbbbbbbbbbbbb", 5, "qqqqqqqqqqqqqqqq")],
        );
        assert_eq!(list[1].dup_relation, "latest");
        assert_eq!(list[0].dup_relation, "equal", "提问口径全同 ⇒ equal（但要能看出口径）");
        assert_eq!(list[0].compare_basis, "questions");
        assert_eq!(list[0].question_count, 5);
        assert!(list[0].dup_note.contains("快速比对口径"), "{}", list[0].dup_note);
        assert!(list[0].dup_note.contains("有差异"), "行级不全同时必须点明：{}", list[0].dup_note);
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
            &[cs(3, "aaaaaaaaaaaaaaaa"), cs(3, "aaaaaaaaaaaaaaaa"), None],
        );
        assert_eq!(list[0].dup_relation, "equal", "没变的成员结论要留着");
        assert_eq!(list[1].dup_relation, "latest", "有结论里最新的那份当基准");
        assert_eq!(list[1].is_newest, true);
        assert_eq!(list[2].dup_relation, "stale", "变了的只作废自己");
        assert!(list[2].dup_note.contains("有新内容"), "{}", list[2].dup_note);
        assert!(list[2].content_digest.is_empty(), "作废的摘要不能留着冒充结论");
        assert!(list[2].compare_basis.is_empty(), "作废的口径也不能留着");
    }

    /// 关联组按组比对：标题不同的成员也能成组拿到结论（此前它们不在任何
    /// 同标题簇里，dup 字段全空，整组只能「无法确认」）。缺正文时结论是
    /// 老实的 `unknown` + 原因，绝不冒充一致。
    #[test]
    fn annotate_sessions_group_covers_renamed_members_without_body() {
        let mut list = vec![
            mk("a", "标题A", 100, false),
            mk("b", "标题B（改过）", 200, false),
        ];
        annotate_sessions_group(&mut list, false, true);
        for s in &list {
            assert_eq!(s.dup_relation, "unknown");
            assert_eq!(s.dup_group, "关联组");
            assert!(s.dup_note.contains("没有正文"), "{}", s.dup_note);
            assert!(s.content_digest.is_empty(), "没算过的摘要不能留");
            assert!(s.compare_basis.is_empty(), "未比对不报口径");
        }
    }

    /// 单成员不成组：什么都不填（避免把唯一副本误标成簇成员）。
    #[test]
    fn annotate_sessions_group_skips_single_member() {
        let mut list = vec![mk("a", "只有一份", 100, false)];
        annotate_sessions_group(&mut list, false, true);
        assert!(list[0].dup_relation.is_empty());
        assert!(list[0].dup_group.is_empty());
    }

    /// 旧版落盘记录（没有 q 字段）必须能读回来：q_* 为 None，不报错。
    #[test]
    fn verified_entry_backward_compatible() {
        let old = r#"{"len":1,"mtime":2,"lines":3,"digest":"aaaaaaaaaaaaaaaa","at":4}"#;
        let e: VerifiedEntry = serde_json::from_str(old).expect("旧记录必须能读");
        assert_eq!(e.q_lines, None);
        assert_eq!(e.q_digest, None);

        // 新记录带 q 字段：round-trip 保留；序列化键名沿用蛇形（与旧字段一致）
        let e2 = VerifiedEntry {
            len: 1,
            mtime: 2,
            lines: 3,
            digest: "aaaaaaaaaaaaaaaa".to_string(),
            q_lines: Some(5),
            q_digest: Some("bbbbbbbbbbbbbbbb".to_string()),
            at: 4,
        };
        let text = serde_json::to_string(&e2).unwrap();
        let back: VerifiedEntry = serde_json::from_str(&text).unwrap();
        assert_eq!(back.q_lines, Some(5));
        assert_eq!(back.q_digest, Some("bbbbbbbbbbbbbbbb".to_string()));
    }

    /// 快速比对结论落盘：指纹没变能带回 q 口径；文件一动只作废它自己。
    #[test]
    fn mark_compared_fast_roundtrip_and_fingerprint_invalidation() {
        let dir = std::env::temp_dir().join(format!("wbsrc-qf-{}", uuid::Uuid::new_v4().simple()));
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
            StampRecord {
                id: "aaaa".to_string(),
                path: a.clone(),
                lines: 1,
                digest: "aaaaaaaaaaaaaaaa".to_string(),
                q_lines: Some(1),
                q_digest: Some("qqqqqqqqqqqqqqqq".to_string()),
            },
            StampRecord {
                id: "bbbb".to_string(),
                path: b.clone(),
                lines: 2,
                digest: "bbbbbbbbbbbbbbbb".to_string(),
                q_lines: Some(1),
                q_digest: Some("qqqqqqqqqqqqqqqq".to_string()),
            },
        ];
        assert_eq!(mark_compared_at(&store, &values), 2);
        let hit = compared_from_disk_at(&store, &pairs).expect("指纹没变就该复用");
        assert_eq!(hit[0].as_ref().unwrap().q_digest, Some("qqqqqqqqqqqqqqqq".to_string()));
        assert_eq!(hit[1].as_ref().unwrap().q_lines, Some(1));

        // b 被改 ⇒ 只有 b 的结论作废，a 的（含 q 口径）照常复用
        std::fs::write(&b, "{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n").unwrap();
        let hit = compared_from_disk_at(&store, &pairs).expect("a 还有效就该给出来");
        assert_eq!(hit[0].as_ref().unwrap().q_digest, Some("qqqqqqqqqqqqqqqq".to_string()));
        assert!(hit[1].is_none(), "动过的那份不许拿旧结论冒充");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 判据优先级：强制 > 成员超限 > 快速体积合规 > 逐行体积合规 > 摘要缓存 > 校验记录 > 不知道。
    ///
    /// 这条顺序一旦写反，要么「明明一致却写未比对」（用户看到的那个 bug），
    /// 要么「没比对却说一致」（更糟）。两头都必须锁住。
    #[test]
    fn decide_mode_priority_is_stable() {
        let facts = |members: usize,
                     all_body: bool,
                     all_fit: bool,
                     fast_fit: bool,
                     all_cached: bool,
                     recorded: bool| {
            GroupFacts {
                members,
                all_body,
                all_fit,
                fast_fit,
                all_cached,
                recorded,
            }
        };
        // 强制比对永远有效：成员超限、什么都没算过也照样真读
        assert_eq!(
            decide_mode(true, false, &facts(20, true, false, false, false, false)),
            Decide::Forced
        );
        // 成员超上限 ⇒ 不知道（缓存/记录都不该越过这条线）
        assert_eq!(
            decide_mode(false, false, &facts(9, true, false, false, true, true)),
            Decide::Unknown
        );
        // 缺正文 ⇒ 不知道
        assert_eq!(
            decide_mode(false, false, &facts(3, false, false, false, true, true)),
            Decide::Unknown
        );
        // 体积合规 ⇒ 常规逐行比对（优先于复用）
        assert_eq!(
            decide_mode(false, false, &facts(3, true, true, false, true, true)),
            Decide::Compare
        );
        // 快速模式 + 体积在 3 GB 内 ⇒ 快速比对（即使逐行口径也合规，快速更便宜）
        assert_eq!(
            decide_mode(false, true, &facts(3, true, true, true, false, false)),
            Decide::CompareFast
        );
        // 快速模式、文件在 32MB–3GB 之间 ⇒ 快速比对（这是本功能的核心价值）
        assert_eq!(
            decide_mode(false, true, &facts(3, true, false, true, false, false)),
            Decide::CompareFast
        );
        // 快速模式但有回读校验记录 ⇒ 免费的权威结论优先（不读盘）
        assert_eq!(
            decide_mode(false, true, &facts(3, true, false, true, false, true)),
            Decide::ReuseRecord
        );
        // 快速模式但逐行摘要已在内存缓存 ⇒ 免费的权威结论优先（不读盘）
        assert_eq!(
            decide_mode(false, true, &facts(3, true, false, true, true, false)),
            Decide::ReuseCache
        );
        // 非快速模式、文件在 32MB–3GB 之间 ⇒ 不会走快速
        assert_eq!(
            decide_mode(false, false, &facts(3, true, false, true, false, false)),
            Decide::Unknown
        );
        // 快速模式但文件超 3 GB ⇒ 不走快速，回落到复用/未知
        assert_eq!(
            decide_mode(false, true, &facts(3, true, false, false, true, false)),
            Decide::ReuseCache
        );
        // 超限但摘要已在缓存 ⇒ 直接复用（「强制比对后刷新又变未比对」的修复点）
        assert_eq!(
            decide_mode(false, false, &facts(4, true, false, false, true, false)),
            Decide::ReuseCache
        );
        // 超限、无缓存、但有同步回读校验记录 ⇒ 复用记录
        assert_eq!(
            decide_mode(false, false, &facts(4, true, false, false, false, true)),
            Decide::ReuseRecord
        );
        // 什么都没有 ⇒ 老实说不知道，绝不猜「一致」
        assert_eq!(
            decide_mode(false, false, &facts(4, true, false, false, false, false)),
            Decide::Unknown
        );
    }

    // -----------------------------------------------------------------
    // 快速比对（提问口径）
    // -----------------------------------------------------------------

    /// 构造一条 message 事件行（与 parse_body 测试同一形状）。
    fn msg_line(role: &str, text: &str) -> String {
        serde_json::json!({
            "type": "message",
            "role": role,
            "content": [{ "type": "input_text", "text": text }],
            "timestamp": 1_700_000_000_000i64,
        })
        .to_string()
    }

    /// 构造一条带 content 数组的用户消息行（图片块没有 text 字段）。
    fn user_msg_line(blocks: Value) -> String {
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": blocks,
            "timestamp": 1_700_000_000_000i64,
        })
        .to_string()
    }

    fn plain_line(n: u32) -> String {
        serde_json::json!({ "n": n }).to_string()
    }

    /// ① 只计「真实提问」：内部摘要 / 自动触发 / 助手消息 / 损坏行一律不计，
    /// 但损坏行**参与行摘要**（与 content_id 同口径，不能让它改变行数）。
    #[test]
    fn scan_lines_counts_only_real_user_questions() {
        let ids = vec!["aaaa".to_string()];
        let lines = vec![
            plain_line(1),
            msg_line("user", "<system-reminder>x</system-reminder>\n<user_query>第一个问题</user_query>"),
            msg_line("assistant", "回答"),
            msg_line("user", "<conversation_history_summary>\nSummary: 一大段"),
            msg_line("user", "Please continue with the conversation based on the summarized context above."),
            msg_line("user", "<system-reminder>y</system-reminder>\n<user_query>第二个问题</user_query>"),
            "{bad json".to_string(),
        ];
        let stamp = scan_lines(lines.into_iter(), &ids).expect("有正文就该有产出");
        assert_eq!(stamp.questions.questions, 2, "两条真实提问，注入/助手/损坏行都不计");
        assert_eq!(stamp.lines.lines, 7, "损坏行也算一条记录（行口径不受影响）");
        assert_eq!(stamp.questions.question_digests.len(), 2);
    }

    /// ② 跨副本 id 归一化：提问正文里引用了组内另一副本的 id 也不能算分叉。
    #[test]
    fn question_stamp_normalizes_ids_across_copies() {
        let dir = std::env::temp_dir().join(format!("wbsrc-q-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.jsonl");
        let b = dir.join("b.jsonl");
        let ids = vec!["aaaa".to_string(), "bbbb".to_string()];
        std::fs::write(&a, msg_line("user", "<user_query>另见 bbbb 的记录</user_query>")).unwrap();
        std::fs::write(&b, msg_line("user", "<user_query>另见 bbbb 的记录</user_query>")).unwrap();
        let qa = question_stamp(&a, &ids).unwrap();
        let qb = question_stamp(&b, &ids).unwrap();
        assert_eq!(qa, qb, "提问里引用组内别的 id 不能算分叉");
        // 对照组：只归一化自己那个 id 时确实不同 —— 锁住根因
        let qa_old = question_stamp(&a, &["aaaa".to_string()]).unwrap();
        let qb_old = question_stamp(&b, &["bbbb".to_string()]).unwrap();
        assert_ne!(qa_old, qb_old);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ③ 空白归一化：同一条提问只因换行/缩进不同必须判同（markdown 结构原样保留）。
    #[test]
    fn question_stamp_normalizes_whitespace() {
        let ids = vec!["aaaa".to_string()];
        let tight = scan_lines(
            vec![msg_line("user", "<user_query>第一步  配置环境\n第二步 运行测试</user_query>")].into_iter(),
            &ids,
        )
        .unwrap();
        let loose = scan_lines(
            vec![msg_line("user", "<user_query>第一步 配置环境\n   第二步   运行测试</user_query>")].into_iter(),
            &ids,
        )
        .unwrap();
        assert_eq!(tight.questions.total, loose.questions.total, "多空格/缩进差异不产生分叉");
        // markdown 结构（非空白字符）原样参与摘要
        let md = scan_lines(
            vec![msg_line("user", "<user_query>第一步 配置环境\n-  第二步 运行测试</user_query>")].into_iter(),
            &ids,
        )
        .unwrap();
        assert_ne!(tight.questions.total, md.questions.total, "内容本身的差异必须判异");
    }

    /// ④ 空消息（纯图片提问）：占一条占位符，不静默丢弃；
    /// 「有图」与「无图」照样判异。
    #[test]
    fn empty_question_uses_placeholder_and_still_counts() {
        let ids = vec!["aaaa".to_string()];
        // 纯图片：content 里只有 image 块，没有 text 字段
        let image_only = user_msg_line(serde_json::json!([
            { "type": "input_image", "image_url": "data:image/png;base64,xxx" }
        ]));
        let with_image = scan_lines(vec![image_only.clone()].into_iter(), &ids).unwrap();
        assert_eq!(with_image.questions.questions, 1, "纯图片提问占一条，不许丢");
        assert!(!with_image.questions.total.is_empty());

        // 有图 vs 无图（同位置一条纯文本提问）→ 判异
        let text_only = scan_lines(
            vec![msg_line("user", "<user_query>看看这张图</user_query>")].into_iter(),
            &ids,
        )
        .unwrap();
        assert_eq!(
            relate_questions(&with_image.questions, &text_only.questions),
            Relation::Diverged,
            "占位符 ≠ 真实提问文本"
        );

        // 两份都是纯图片（占位符一致）→ 判同
        let with_image2 = scan_lines(vec![image_only].into_iter(), &ids).unwrap();
        assert_eq!(
            relate_questions(&with_image.questions, &with_image2.questions),
            Relation::Equal,
            "同一规则占位的两份必须判同"
        );
    }

    /// ⑤ 截断边界：前 4096 字符相同、尾部不同 ⇒ 快速口径判同（已知误判面）；
    /// 第 4095 个字符处不同 ⇒ 必须判异（长度前缀编码兜底）。
    #[test]
    fn question_truncation_boundary() {
        let ids = vec!["aaaa".to_string()];
        let long = "甲".repeat(5000);
        let tail_diff = format!("{long}尾部不同");
        let s1 = scan_lines(vec![msg_line("user", &format!("<user_query>{long}</user_query>"))].into_iter(), &ids).unwrap();
        let s2 = scan_lines(
            vec![msg_line("user", &format!("<user_query>{tail_diff}</user_query>"))].into_iter(),
            &ids,
        )
        .unwrap();
        assert_eq!(
            relate_questions(&s1.questions, &s2.questions),
            Relation::Equal,
            "差异落在 4096 截断点之后 ⇒ 快速口径判同（行口径仍会判异兜底）"
        );

        // 差异落在截断窗口内（第 4095 个字符）⇒ 必须判异
        let within = "乙".repeat(4095);
        let t1 = format!("{within}A{}", "丙".repeat(900));
        let t2 = format!("{within}B{}", "丙".repeat(900));
        let d1 = scan_lines(vec![msg_line("user", &format!("<user_query>{t1}</user_query>"))].into_iter(), &ids).unwrap();
        let d2 = scan_lines(vec![msg_line("user", &format!("<user_query>{t2}</user_query>"))].into_iter(), &ids).unwrap();
        assert_eq!(
            relate_questions(&d1.questions, &d2.questions),
            Relation::Diverged,
            "截断窗口内的差异绝不能漏"
        );
    }

    /// ⑥ 重复提问的次数参与摘要：同句问两遍 ≠ 问一遍。
    #[test]
    fn question_repeat_count_matters() {
        let ids = vec!["aaaa".to_string()];
        let once = scan_lines(vec![msg_line("user", "<user_query>怎么测试</user_query>")].into_iter(), &ids).unwrap();
        let twice = scan_lines(
            vec![
                msg_line("user", "<user_query>怎么测试</user_query>"),
                msg_line("assistant", "这样这样"),
                msg_line("user", "<user_query>怎么测试</user_query>"),
            ]
            .into_iter(),
            &ids,
        )
        .unwrap();
        assert_eq!(once.questions.questions, 1);
        assert_eq!(twice.questions.questions, 2);
        assert_ne!(once.questions.total, twice.questions.total);
        assert_eq!(
            relate_questions(&once.questions, &twice.questions),
            Relation::Prefix,
            "少的那份是多的的有序前缀 ⇒ behind（可安全快进）"
        );
    }

    /// ⑦ 提问顺序参与摘要：同样两条提问、顺序不同 ⇒ 分叉。
    #[test]
    fn question_order_matters() {
        let ids = vec!["aaaa".to_string()];
        let ab = scan_lines(
            vec![
                msg_line("user", "<user_query>问题一</user_query>"),
                msg_line("user", "<user_query>问题二</user_query>"),
            ]
            .into_iter(),
            &ids,
        )
        .unwrap();
        let ba = scan_lines(
            vec![
                msg_line("user", "<user_query>问题二</user_query>"),
                msg_line("user", "<user_query>问题一</user_query>"),
            ]
            .into_iter(),
            &ids,
        )
        .unwrap();
        assert_eq!(ab.questions.questions, ba.questions.questions);
        assert_ne!(ab.questions.total, ba.questions.total);
        assert_eq!(
            relate_questions(&ab.questions, &ba.questions),
            Relation::Diverged,
            "顺序不同就是各写各的，不冒充可快进"
        );
    }

    /// ⑧ relate_questions 的前缀 / 扩展 / 分叉 / 相等四态。
    #[test]
    fn relate_questions_prefix_extension_and_divergence() {
        let q = |texts: &[&str]| -> QuestionId {
            let d: Vec<String> = texts.iter().map(|t| question_text_digest(t, &[])).collect();
            QuestionId {
                questions: d.len(),
                question_digests: d.clone(),
                total: question_total_digest(&d),
            }
        };
        let short = q(&["a", "b"]);
        let long = q(&["a", "b", "c"]);
        assert_eq!(relate_questions(&short, &long), Relation::Prefix);
        assert_eq!(relate_questions(&long, &short), Relation::Extension);
        assert_eq!(relate_questions(&short, &short.clone()), Relation::Equal);
        assert_eq!(relate_questions(&q(&["a", "x"]), &long), Relation::Diverged);
        // 空序列（全注入的会话）：条数 0 也是合法产出
        let none = scan_lines(vec![msg_line("assistant", "只有回答")].into_iter(), &[]).unwrap();
        assert_eq!(none.questions.questions, 0);
        assert_eq!(
            relate_questions(&none.questions, &none.questions),
            Relation::Equal
        );
    }

    /// ⑨ 两种口径的缓存互不覆盖：full / fast 先后取摘要各自命中，
    /// 且 fast 算完的行摘要同时登记到 full 键下（互切不重读盘）。
    #[test]
    fn stamp_caches_do_not_collide() {
        let dir = std::env::temp_dir().join(format!("wbsrc-qc-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.jsonl");
        let ids = vec!["aaaa".to_string()];
        std::fs::write(&f, msg_line("user", "<user_query>问题</user_query>")).unwrap();

        // 先 full 后 fast：两种摘要都必须算出来，且口径不同（版本串区分）
        let full = content_stamp(&f, &ids).unwrap();
        let fast = question_stamp(&f, &ids).unwrap();
        assert_eq!(fast.0, 1, "一条真实提问");
        assert_ne!(full.1, fast.1, "行摘要与提问摘要必须不同（wb-switch-lines-v1 vs questions-v1）");

        // fast 的行摘要副产品：full 口径第二次取直接命中，不必重读
        let full_again = content_stamp(&f, &ids).unwrap();
        assert_eq!(full_again, full);
        // fast 第二次也命中
        assert_eq!(question_stamp(&f, &ids).unwrap(), fast);

        // 反向：先 fast 后 full，同样互不覆盖
        let dir2 = std::env::temp_dir().join(format!("wbsrc-qc2-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir2).unwrap();
        let f2 = dir2.join("c.jsonl");
        std::fs::write(&f2, msg_line("user", "<user_query>问题</user_query>")).unwrap();
        let fast2 = question_stamp(&f2, &ids).unwrap();
        let full2 = content_stamp(&f2, &ids).unwrap();
        assert_eq!(full2.0, 1);
        assert_ne!(full2.1, fast2.1, "先 fast 再 full 也必须拿到行口径摘要");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
