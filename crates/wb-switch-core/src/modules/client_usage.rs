//! Trae 客户端的「使用记忆」：按历史使用情况决定界面排序（T34）。
//!
//! 落盘 `~/.twin-switch/trae-client-usage.json`。放在 [`store_dir`]（用户资产）而**不是**
//! `cache/`（可再生的派生数据）——「我平时用哪个客户端」是用户的习惯，清缓存不该顺手清掉它。
//!
//! ## 打分
//!
//! ```text
//! score = switches * SWITCH_WEIGHT + uses
//! ```
//!
//! - `switches`：每次**成功切换 / 回滚到**该客户端的某个账号 +1。主动切号是最强的使用信号，
//!   所以乘 [`SWITCH_WEIGHT`]。
//! - `uses`：每次**打开该客户端的功能页去读它的数据**（账号管理页 / 会话记录页）+1。
//!
//! ## 排序（全序，结果可复现）
//!
//! ① `score` 降序 → ② `lastUsedAt` 降序 → ③ 内置偏好顺序 → ④ `key` 字典序。
//!
//! 没有任何历史时只剩 ③ —— 于是首屏就是内置偏好，**`solo-cn` 排第一**（用户最常用的那个）。
//! 换句话说：默认值本身就是「用户要的那个顺序」，记忆只是在此之上做微调。
//!
//! ## 边界（两条硬约束）
//!
//! - ⚠️ 本模块**只决定展示顺序**，绝不参与任何写库 / 遍历逻辑的顺序语义。
//!   内部循环（导入、清理、账号发现）仍然用 [`crate::modules::trae_discover::CLIENTS`] 的固定顺序。
//! - ⚠️ 未知 `key` 一律忽略，不写进文件 —— 否则版本演进留下的垃圾 key 会把文件越撑越大。
//! - ⚠️ 文件损坏 / 读不出来时**降级成「没有历史」**（退回内置偏好顺序），不报错。
//!   这里丢的只是一点排序偏好，不值得把一次切换流程打断。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::modules::config::{atomic_write, now_ms, store_dir};
use crate::modules::trae_discover::{get_client, InstalledClient};

/// 落盘文件名（在 [`store_dir`] 下）。
pub const USAGE_FILE: &str = "trae-client-usage.json";

/// 存储结构版本。读到不认识的版本就当没有历史（**不尝试猜测解析**）。
pub const USAGE_VERSION: u32 = 1;

/// 一次「切换账号」折算成多少次「打开页面」。
///
/// 3 是拍出来的：切号是用户明确表达「我要用这个客户端」的动作，理应压过零星几次页面刷新；
/// 但也不该大到一次切号就永久锁死顺序（三次页面访问就能追平）。
pub const SWITCH_WEIGHT: u64 = 3;

/// 内置偏好顺序：**没有任何历史记录时**的默认排列。
///
/// `solo-cn`（TRAE SOLO CN）排第一 —— 这是实际最常用的客户端，新装用户一进来就该看到它。
/// 不在表里的 key 排在所有在表 key 之后。
pub const DEFAULT_PREFERENCE: &[&str] = &["solo-cn", "trae-cn", "solo-intl", "trae-intl"];

/// 单个客户端的使用记录。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientUsage {
    /// 打开功能页读取该客户端数据的次数。
    #[serde(default)]
    pub uses: u64,
    /// 成功切换 / 回滚到该客户端账号的次数。
    #[serde(default)]
    pub switches: u64,
    /// 最后一次使用时间（毫秒时间戳）。
    #[serde(default)]
    pub last_used_at: i64,
}

impl ClientUsage {
    /// 常用度分数（越大越常用）。
    pub fn score(&self) -> u64 {
        self.switches
            .saturating_mul(SWITCH_WEIGHT)
            .saturating_add(self.uses)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    clients: BTreeMap<String, ClientUsage>,
}

impl Default for UsageStore {
    fn default() -> Self {
        Self {
            version: USAGE_VERSION,
            clients: BTreeMap::new(),
        }
    }
}

/// 读改写的互斥体。多个页面可能同时上报使用，不加锁会丢更新。
static USAGE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn usage_path() -> PathBuf {
    store_dir().join(USAGE_FILE)
}

/// 全部使用记录。读不出来 / 版本不对 / 结构坏了 ⇒ 空表（退回内置偏好顺序）。
pub fn usage_map() -> BTreeMap<String, ClientUsage> {
    let Ok(text) = std::fs::read_to_string(usage_path()) else {
        return BTreeMap::new();
    };
    let Ok(store) = serde_json::from_str::<UsageStore>(&text) else {
        eprintln!("[client-usage] 解析失败，本次按「没有历史」处理：{:?}", usage_path());
        return BTreeMap::new();
    };
    if store.version != USAGE_VERSION {
        return BTreeMap::new();
    }
    store
        .clients
        .into_iter()
        .filter(|(k, _)| get_client(k).is_some())
        .collect()
}

fn save(store: &UsageStore) {
    let path = usage_path();
    let Ok(text) = serde_json::to_string_pretty(store) else {
        return;
    };
    if let Err(e) = atomic_write(&path, &text) {
        // 排序偏好写不进去不值得报错给用户，但必须留痕，否则「记忆功能没生效」会变成
        // 一个查不出来的谜。
        eprintln!("[client-usage] 写入失败 {:?}: {e}", path);
    }
}

/// 读改写一次（加锁 + 落盘）。`f` 返回 `false` 表示「本次不值得写盘」。
fn update<F: FnOnce(&mut BTreeMap<String, ClientUsage>) -> bool>(f: F) {
    let Ok(_guard) = USAGE_LOCK.lock() else {
        return;
    };
    let mut store = UsageStore {
        version: USAGE_VERSION,
        clients: usage_map(),
    };
    if !f(&mut store.clients) {
        return;
    }
    save(&store);
}

/// 记录一次「该客户端的功能页被打开」（账号管理页 / 会话记录页）。
pub fn record_use(key: &str) {
    if get_client(key).is_none() {
        return;
    }
    let now = now_ms();
    update(|clients| {
        let e = clients.entry(key.to_string()).or_default();
        e.uses = e.uses.saturating_add(1);
        e.last_used_at = now;
        true
    });
}

/// 记录一次「成功切换 / 回滚到该客户端下的某个账号」。
pub fn record_switch(key: &str) {
    if get_client(key).is_none() {
        return;
    }
    let now = now_ms();
    update(|clients| {
        let e = clients.entry(key.to_string()).or_default();
        e.switches = e.switches.saturating_add(1);
        e.last_used_at = now;
        true
    });
}

/// 内置偏好里的名次；不在表里 ⇒ `usize::MAX`（排在所有在表 key 之后）。
fn preference_rank(key: &str) -> usize {
    DEFAULT_PREFERENCE
        .iter()
        .position(|k| *k == key)
        .unwrap_or(usize::MAX)
}

/// 纯排序：给定候选 `keys` 与使用记录，返回排好的顺序。
///
/// 抽成纯函数是为了**能测**：直接测文件读写就得往真实 `~/.twin-switch` 里写，
/// 而 `set_var` 是进程全局的、并行测试会互相踩（`config` 模块里已有同款踩坑记录）。
pub fn order_keys(keys: &[&str], usage: &BTreeMap<String, ClientUsage>) -> Vec<String> {
    let mut out: Vec<&str> = keys.to_vec();
    out.sort_by(|a, b| {
        let ua = usage.get(*a);
        let ub = usage.get(*b);
        let sa = ua.map(ClientUsage::score).unwrap_or(0);
        let sb = ub.map(ClientUsage::score).unwrap_or(0);
        let la = ua.map(|u| u.last_used_at).unwrap_or(0);
        let lb = ub.map(|u| u.last_used_at).unwrap_or(0);
        // ① 分数高者在前 ② 最近用过者在前 ③ 内置偏好 ④ 字典序兜底（保证全序、结果稳定）
        sb.cmp(&sa)
            .then_with(|| lb.cmp(&la))
            .then_with(|| preference_rank(a).cmp(&preference_rank(b)))
            .then_with(|| a.cmp(b))
    });
    out.into_iter().map(|s| s.to_string()).collect()
}

/// 按使用记忆就地重排客户端列表（只改顺序，不增删）。
pub fn sort_installed(clients: &mut [InstalledClient]) {
    let usage = usage_map();
    let keys: Vec<&str> = clients.iter().map(|c| c.key).collect();
    let order = order_keys(&keys, &usage);
    let rank: BTreeMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    // `sort_by_key` 是稳定排序；rank 已保证全序，稳定性在这里只是额外保险。
    clients.sort_by_key(|c| rank.get(c.key).copied().unwrap_or(usize::MAX));
}

/// 给界面看的诊断快照：谁被排到最前、各项计数是多少。
///
/// `topPick` 只在**确有历史**时才是 `Some` —— 全是 0 分时不该给「常用」徽标，
/// 那只是内置默认值，不是用户习惯。
pub fn snapshot_for(keys: &[&str]) -> Value {
    let usage = usage_map();
    let score_of = |k: &str| usage.get(k).map(ClientUsage::score).unwrap_or(0);
    // ⚠️ 徽标必须与**实际排序的第一名**一致：所以复用 `order_keys` 取队首，
    //    而不是另写一套「最大值」逻辑 —— 同一件事被推导两次，早晚会不一致。
    let top = order_keys(keys, &usage)
        .into_iter()
        .find(|k| score_of(k) > 0);
    let clients: Vec<Value> = keys
        .iter()
        .map(|k| {
            let u = usage.get(*k).cloned().unwrap_or_default();
            json!({
                "key": k,
                "uses": u.uses,
                "switches": u.switches,
                "lastUsedAt": u.last_used_at,
                "score": u.score(),
            })
        })
        .collect();
    json!({
        "topPick": top,
        "switchWeight": SWITCH_WEIGHT,
        "preference": DEFAULT_PREFERENCE,
        "clients": clients,
    })
}

/// 清空使用记忆（界面重新回到内置偏好顺序）。
pub fn reset() {
    let Ok(_guard) = USAGE_LOCK.lock() else {
        return;
    };
    let path = usage_path();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            eprintln!("[client-usage] 删除失败 {:?}: {e}", path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(pairs: &[(&str, u64, u64, i64)]) -> BTreeMap<String, ClientUsage> {
        pairs
            .iter()
            .map(|(k, uses, switches, last)| {
                (
                    k.to_string(),
                    ClientUsage {
                        uses: *uses,
                        switches: *switches,
                        last_used_at: *last,
                    },
                )
            })
            .collect()
    }

    /// 默认值就是用户要的那个顺序：**没有任何历史时 solo-cn 排第一**。
    #[test]
    fn empty_history_falls_back_to_preference_with_solo_cn_first() {
        let keys = ["trae-cn", "solo-cn", "trae-intl", "solo-intl"];
        let got = order_keys(&keys, &BTreeMap::new());
        assert_eq!(
            got,
            vec!["solo-cn", "trae-cn", "solo-intl", "trae-intl"],
            "没有历史时必须按内置偏好排，solo-cn 第一"
        );
    }

    /// 偏好表里没有的 key 排在所有在表 key 之后，且彼此按字典序（全序、可复现）。
    #[test]
    fn unknown_keys_sort_after_known_ones_and_stay_stable() {
        let keys = ["zzz", "trae-cn", "aaa"];
        let got = order_keys(&keys, &BTreeMap::new());
        assert_eq!(got, vec!["trae-cn", "aaa", "zzz"]);
    }

    /// 分数高的在前 —— 用得多就往前排，这正是「记忆功能」的核心。
    #[test]
    fn higher_score_wins() {
        let u = usage(&[
            ("solo-cn", 1, 0, 100),  // 1
            ("trae-cn", 0, 3, 100),  // 9
        ]);
        let got = order_keys(&["solo-cn", "trae-cn"], &u);
        assert_eq!(got, vec!["trae-cn", "solo-cn"]);
    }

    /// 切号权重生效：两次切号（6）压过五次打开页面（5）。
    #[test]
    fn switch_weight_beats_repeated_page_views() {
        let u = usage(&[
            ("solo-cn", 0, 2, 0), // 6
            ("trae-cn", 5, 0, 0), // 5
        ]);
        let got = order_keys(&["solo-cn", "trae-cn"], &u);
        assert_eq!(got, vec!["solo-cn", "trae-cn"]);
    }

    /// 同分看「最近用过」，再看内置偏好 —— 顺序必须是全序，不能随机。
    #[test]
    fn ties_break_by_last_used_then_preference() {
        let u = usage(&[
            ("trae-cn", 2, 0, 500),
            ("solo-cn", 2, 0, 900),
            ("solo-intl", 2, 0, 500),
        ]);
        let got = order_keys(&["trae-cn", "solo-cn", "solo-intl"], &u);
        assert_eq!(
            got,
            vec!["solo-cn", "trae-cn", "solo-intl"],
            "先按 lastUsedAt，再按内置偏好"
        );
    }

    /// 内置偏好顺序本身不能忘掉 solo-cn 第一（改这张表就等于改了默认体验）。
    #[test]
    fn default_preference_starts_with_solo_cn() {
        assert_eq!(DEFAULT_PREFERENCE.first().copied(), Some("solo-cn"));
    }
}
