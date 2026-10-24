//! 本工具的用户设置（`~/.twin-switch/settings.json`）。
//!
//! 目前只放一项：**整库备份的保留策略**（时间 / 容量 / 份数三重触发）。
//!
//! 为什么单独一个模块：这份策略要同时被**写备份的人**（删会话 / 编辑回写 / 导入 / 导出）
//! 和**清理页**读到 —— 三处必须同源，否则界面上说的策略和实际执行的策略会不一致
//! （那正是本项目最忌讳的「同一屏两个数字不同源」）。
//!
//! ⚠️ 本模块**只管策略判定**，不碰文件；真正删文件的是
//! [`crate::modules::trae_export::prune_with_policy`]。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::modules::config::store_dir;

/// 设置文件：`~/.twin-switch/settings.json`。
pub fn settings_path() -> PathBuf {
    store_dir().join("settings.json")
}

/// 整库备份保留策略。
///
/// **三重触发**：任一条超限就把该批次判为「可清理」。
///
/// ⚠️ 但**每个客户端最新的一批永不清** —— 那是最新一次操作的完整快照，是「出事能回滚」
/// 的唯一依赖。这条写死在 [`excess_batches`] 的最后一道过滤里，任何新规则都不许绕过。
///
/// ⚠️ 只有一个开关（`enabled`），**不设第二个「自动 / 手动」开关** ——
/// 两个开关语义必然重叠，界面上就会出现「关了 A 但 B 还在清」这种说不清的组合。
/// 想手动，就把总开关关掉，然后在清理页逐批删。
///
/// ⚠️ 字段名用 **camelCase**：这份结构既是磁盘上 `settings.json` 的形状，也是 Tauri 命令
/// 的入参形状 —— 两边共用一个命名风格，界面上的 `keepDays` 与文件里的就是同一个键，
/// 不会出现「界面存了 A、后端读的是 B」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRetention {
    /// 总开关。关掉 ⇒ **完全不自动清理**（备份只增不减，可在清理页手动删）。
    #[serde(default = "yes")]
    pub enabled: bool,
    /// **时间**：早于这个天数的批次清掉。`0` = 不按时间判。
    #[serde(default = "days_30")]
    pub keep_days: u32,
    /// **份数**：**每个客户端各自**保留多少批，超出的旧批清掉。`0` = 不按份数判。
    #[serde(default = "count_3")]
    pub keep_count: u32,
    /// **容量**：**每个备份目录各自**最多留多少 MB，从新到旧累加超出后更旧的清掉。
    /// `0` = 不按容量判。
    #[serde(default = "mb_4096")]
    pub max_total_mb: u64,
}

fn yes() -> bool {
    true
}
fn days_30() -> u32 {
    30
}
fn count_3() -> u32 {
    3
}
fn mb_4096() -> u64 {
    4096
}

impl Default for BackupRetention {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_days: 30,
            keep_count: 3,
            max_total_mb: 4096,
        }
    }
}

impl BackupRetention {
    /// 夹到合理区间。
    ///
    /// 界面上是数字输入框，手输可能离谱（比如 999999999 天）—— 那种值不会造成数据风险
    /// （策略只会「少清」或「按最新一批兜底」），但会让界面上的摘要没法看，
    /// 而且 `keep_days * 86_400_000` 会溢出 `i64`。所以落盘前一律夹一次。
    pub fn sanitized(mut self) -> Self {
        self.keep_days = self.keep_days.min(3650);
        self.keep_count = self.keep_count.min(500);
        self.max_total_mb = self.max_total_mb.min(1_048_576);
        self
    }

    /// 给人看的一行摘要（界面上直接显示，**与实际判定同源**）。
    pub fn summary(&self) -> String {
        if !self.enabled {
            return "保留策略已关闭 —— 不会自动清理任何备份（备份只增不减，可到清理页逐批手动删）"
                .to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.keep_days > 0 {
            parts.push(format!("≤{} 天", self.keep_days));
        }
        if self.keep_count > 0 {
            parts.push(format!("每客户端 ≤{} 份", self.keep_count));
        }
        if self.max_total_mb > 0 {
            parts.push(format!("每目录 ≤{}", human_mb(self.max_total_mb)));
        }
        if parts.is_empty() {
            return "保留策略：不限制（三项都是 0）".to_string();
        }
        format!(
            "保留策略：{}（任一超限即清理；每客户端最新一批始终保留）",
            parts.join(" · ")
        )
    }
}

/// MB → 人看的字符串（1024 MB 以上换 GB）。
fn human_mb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

/// 一个备份批次的「策略视角」—— 把 `BackupBatch` 里跟策略有关的字段抽出来，
/// 这样策略判定是**纯函数**，可以脱离文件系统单测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchFacts {
    pub owner: String,
    pub stamp: String,
    /// 批次时间（毫秒）。`0` = 解不出时间戳（此时不按时间判，但仍受份数 / 容量约束）。
    pub at_ms: i64,
    pub bytes: u64,
}

/// 命中的规则 —— **既用来判定，也用来向用户解释**「为什么这一批被标成可清理」。
///
/// ⚠️ 只留一个实现（[`excess_with_reasons`]）：界面上显示的「超出原因」必须与真正删文件的
/// 判据同源，另写一份描述函数迟早会与判定漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcessRule {
    /// 早于 `keep_days` 天。
    Time,
    /// 超出每个客户端保留的份数。
    Count,
    /// 超出每个备份目录的容量上限。
    Size,
}

impl ExcessRule {
    /// 给人看的一句话（带上策略里的具体阈值，用户才知道该改哪个数）。
    pub fn describe(&self, p: &BackupRetention) -> String {
        match self {
            ExcessRule::Time => format!("超过 {} 天", p.keep_days),
            ExcessRule::Count => format!("超过保留份数（每客户端 {} 份）", p.keep_count),
            ExcessRule::Size => format!("超过容量上限（每目录 {}）", human_mb(p.max_total_mb)),
        }
    }
}

/// 选出**超出策略**的批次下标（下标指传入切片里的位置）。
///
/// 三重触发，任一条命中即入选：
/// 1. **时间**：`now - at_ms > keep_days`；
/// 2. **份数**：同一 `owner` 内按时间从新到旧排，序号 ≥ `keep_count` 的；
/// 3. **容量**：把还没被判掉的批次按从新到旧累加，累计超过 `max_total_mb` 的**更旧**批次。
///
/// ⚠️ 最后一道过滤：每个 `owner` 的**最新一批永远不入选**（回滚安全网）。
pub fn excess_batches(batches: &[BatchFacts], p: &BackupRetention, now_ms: i64) -> Vec<usize> {
    excess_with_reasons(batches, p, now_ms)
        .into_iter()
        .map(|(i, _)| i)
        .collect()
}

/// [`excess_batches`] 的实现主体，额外把**命中了哪几条规则**带出来给界面用。
///
/// 返回的下标**升序**且去重（同一批可能同时命中多条规则）。
pub fn excess_with_reasons(
    batches: &[BatchFacts],
    p: &BackupRetention,
    now_ms: i64,
) -> Vec<(usize, Vec<ExcessRule>)> {
    if !p.enabled || batches.is_empty() {
        return Vec::new();
    }
    let n = batches.len();
    let mut rules: Vec<Vec<ExcessRule>> = vec![Vec::new(); n];

    // —— 触发一：时间 ——
    if p.keep_days > 0 {
        let limit = i64::from(p.keep_days) * 86_400_000;
        for (i, b) in batches.iter().enumerate() {
            // ⚠️ `at_ms == 0` = 时间未知 ⇒ **不按时间判**（否则 now - 0 直接判死）
            if b.at_ms > 0 && now_ms.saturating_sub(b.at_ms) > limit {
                rules[i].push(ExcessRule::Time);
            }
        }
    }

    // 每个 owner 的下标，按时间从新到旧
    let mut by_owner: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, b) in batches.iter().enumerate() {
        by_owner.entry(b.owner.as_str()).or_default().push(i);
    }
    for list in by_owner.values_mut() {
        list.sort_by(|&a, &b| {
            batches[b]
                .at_ms
                .cmp(&batches[a].at_ms)
                .then_with(|| batches[b].stamp.cmp(&batches[a].stamp))
        });
    }

    // —— 触发二：份数 ——
    if p.keep_count > 0 {
        let keep = p.keep_count as usize;
        for list in by_owner.values() {
            for &i in list.iter().skip(keep) {
                rules[i].push(ExcessRule::Count);
            }
        }
    }

    // —— 触发三：容量 ——
    if p.max_total_mb > 0 {
        let limit = p.max_total_mb.saturating_mul(1_048_576);
        let mut alive: Vec<usize> = (0..n).filter(|i| rules[*i].is_empty()).collect();
        alive.sort_by(|&a, &b| {
            batches[b]
                .at_ms
                .cmp(&batches[a].at_ms)
                .then_with(|| batches[b].stamp.cmp(&batches[a].stamp))
        });
        let mut acc = 0u64;
        for i in alive {
            acc = acc.saturating_add(batches[i].bytes);
            if acc > limit {
                rules[i].push(ExcessRule::Size);
            }
        }
    }

    // —— 安全网：每个 owner 的最新一批永不清 ——
    for list in by_owner.values() {
        if let Some(&newest) = list.first() {
            rules[newest].clear();
        }
    }

    (0..n)
        .filter(|i| !rules[*i].is_empty())
        .map(|i| (i, rules[i].clone()))
        .collect()
}

/// 完整设置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// **切号时把定时任务同步给新账号**（默认**开**）。
    ///
    /// WorkBuddy 的定时任务按 `owner_user_id` 归属（见
    /// [`crate::modules::workbuddy_automations`]），切到别的账号后原任务会「消失」。
    /// 默认同步 = 切过去还能看到原来的任务；不想要这份「自动复制」的人可以在这里关掉。
    ///
    /// ⚠️ 用 `default = "yes"` 而不是 `#[serde(default)]`：后者的 bool 默认是 `false`，
    /// 老用户的 `settings.json` 里没有这个键，升级后会**悄悄变成关**（行为突变）。
    #[serde(default = "yes")]
    pub sync_workbuddy_automations: bool,
    #[serde(default)]
    pub backup_retention: BackupRetention,
    /// **写库前的整库备份**开关（T66）。**默认关闭**。
    ///
    /// ⚠️ 关掉的只是「留一份整库历史版本」，**不影响数据安全** —— 库位替换走
    /// [`crate::modules::trae_backup::swap_in_place`]（旧库先改名让位、新库再改名就位，
    /// 任何一步失败都自动回滚），所以「写库写一半崩了」不会丢库。备份只负责「能回到从前」。
    ///
    /// ⚠️ 只对**整库级**备份生效（Trae 的删除 / 编辑 / 导入）。WorkBuddy 那边备的是
    /// 0.2 MB 的库和**被改的那一个**会话正文，量级在 KB，不受这个开关影响。
    ///
    /// ⚠️ 默认 `false` ⇒ 老用户的 `settings.json` 里没有这个键，反序列化后就是关，
    /// **与「新装也是关」一致**（不许出现「升级后行为突然变了」这种说不清的事）。
    #[serde(default)]
    pub backup_before_write: bool,
    /// **用户手动指定的客户端主程序路径**（T69）：`clientKey → 绝对路径`。
    ///
    /// ## 为什么需要
    ///
    /// Trae 允许把程序装到任意目录（`D:\Trae CN`、`D:\soft\Trae`…），自动探测只扫
    /// 固定的几处（`%LOCALAPPDATA%\Programs` / `Program Files` / `<盘符>:\Program Files`），
    /// 装在自定义目录的机器上就「未找到主程序，请在设置里手动指定路径」—— 而**设置里
    /// 压根没有那一项**（这就是 2026-10-18 用户报的问题）。现在补上：
    ///
    /// - 这里是**唯一**的用户意图来源，优先级**高于**任何自动探测；
    /// - 路径不存在时**不算数**（见 [`trae_exe_override`]）—— 用户拔了移动硬盘、
    ///   或手动填错，都不许让「切换」拿一个不存在的 exe 去 spawn 然后报一个
    ///   看不出真因的错。此时自动探测照常兜底。
    ///
    /// ## 为什么不复用 `<store>/trae/config.json` 的 `client_exe`
    ///
    /// 那个字段是早期命令行版的遗物，**从来没有界面写过它**（代码里只有读）。
    /// 同一件事留两处配置 = 迟早「界面上改了 A、真正生效的是 B」。
    /// 现在用户可见的只有这一份；`trae_switch` 读路径时也**只**问这里。
    #[serde(default)]
    pub trae_exe: HashMap<String, String>,
    /// **用户手动指定的 WorkBuddy 主程序路径**（`WorkBuddy.exe` 的绝对路径）。
    ///
    /// 与 [`Settings::trae_exe`] 同一个道理：WorkBuddy 也能装到自定义目录，
    /// 自动探测（常见目录 + 运行中的进程）扫不到时报的是
    /// 「未找到 WorkBuddy 主程序（WorkBuddy.exe），请手动启动」——
    /// 那句同样没给出可操作的下一步。这里给它一个能填的地方。
    ///
    /// ⚠️ 空串 = 没指定（走自动探测）；**文件不存在也当没指定**（见
    /// [`workbuddy_exe_override`]），这样拔了移动硬盘不会去 spawn 一个不存在的程序。
    #[serde(default)]
    pub workbuddy_exe: String,
    /// **用户手动指定的 Qoder 主程序路径**（`区域 → 绝对路径`，键是 `cn` / `global`）。
    ///
    /// 前两个客户端的 override 论证同样适用。Qoder 的特殊之处只有一条：
    /// 两个区域的程序名不同（`Qoder CN.exe` / `Qoder.exe`），所以这里按区域分键，
    /// **绝不允许一个区域填另一个区域的路径**（那会让「切换国内版」去启动国际版）。
    #[serde(default)]
    pub qoder_exe: HashMap<String, String>,
    /// **切号时自动把 WorkBuddy 会话同步到所有账号**（默认**关**）。
    ///
    /// 打开后：每次切换账号（客户端已被关掉、还没重新启动的那一刻）都会跑一次
    /// [`crate::modules::workbuddy_sessions::link_all`]——把「还没同步到某个账号」的
    /// 会话补过去，并登记成关联组。用户不用再一个个手动复制。
    ///
    /// ⚠️⚠️ **默认必须是关**，理由与 [`Settings::backup_before_write`] 不同：
    /// 这不是留不留备份，而是**真的往其它账号的会话库里写数据**（会话正文 + 库行），
    /// 一次可能几十上百条。老用户升级后突然发现「每个号里多出一堆别人的会话」
    /// 是不可接受的；必须是本人看过说明、亲手打开的。
    ///
    /// ⚠️ `#[serde(default)]` 让老 `settings.json` 反序列化成 `false`，
    /// 与新装一致 —— 不允许出现「升级后行为突变」。
    #[serde(default)]
    pub auto_link_workbuddy_sessions: bool,
    /// **自动同步的增量起点**（毫秒时间戳；0 = 还没跑过）。
    ///
    /// 切号时的自动同步**只搬这部分之后新建的会话**，绝不搬历史存量：
    ///
    /// - 实测本机 4 个账号、25 个会话时，一次全量要复制 **74 个副本、约 2.98 GB**。
    ///   把这种量级塞进「切号」这种高频动作里，用户会遇到「切个号卡五分钟 + 磁盘少 3 GB」，
    ///   而且他没有任何确认机会。
    /// - 所以历史存量走**界面上的「一键互相关联」**（先给数字、再确认）；
    ///   切号只做「本次之后新产生的会话」这种小额增量。
    ///
    /// 首次开启时（本字段为 0）自动同步只登记「已有副本」（0 字节），并把这里推到当前时刻 ——
    /// 从下一次开始才是真正的增量复制。
    #[serde(default)]
    pub wb_link_since: i64,
}

impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.backup_retention = self.backup_retention.sanitized();
        // 去掉空白项与空值：空串留在磁盘上等于「设了一个空路径」，
        // 会让界面显示「已手动指定」而实际什么都没指定。
        self.trae_exe = self
            .trae_exe
            .into_iter()
            .map(|(k, v)| (k, v.trim().to_string()))
            .filter(|(k, v)| !k.trim().is_empty() && !v.is_empty())
            .collect();
        self.workbuddy_exe = self.workbuddy_exe.trim().to_string();
        self.qoder_exe = self
            .qoder_exe
            .into_iter()
            .map(|(k, v)| (k, v.trim().to_string()))
            .filter(|(k, v)| !k.trim().is_empty() && !v.is_empty())
            .collect();
        self
    }
}

/// 用户手动指定的主程序路径（**唯一出口**）—— 文件不存在就当没指定。
///
/// ⚠️ 这里做 `is_file()` 校验而不是在调用方各写一遍：注册表 / 进程 / 目录扫描
/// 三条自动探测都得先看它，校验散开就会出现「A 处认了 B 处不认」。
pub fn trae_exe_override(client_key: &str) -> Option<PathBuf> {
    let p = load().trae_exe.get(client_key).cloned()?;
    let p = PathBuf::from(p.trim());
    p.is_file().then_some(p)
}

/// 用户手动指定的 Qoder 主程序（**唯一出口**）—— 文件不存在就当没指定。
///
/// ⚠️ 与另两个客户端同一个纪律：`is_file()` 校验放在这里，调用方不再各写一遍。
pub fn qoder_exe_override(region: crate::modules::qoder_realm::QoderRegion) -> Option<PathBuf> {
    let p = load().qoder_exe.get(region.as_str()).cloned()?;
    let p = PathBuf::from(p.trim());
    p.is_file().then_some(p)
}

/// 写库前要不要留整库备份 —— **唯一出口**。
///
/// 各处写库链路一律问它，不许自己 `load()` 再读字段：
/// 读法一分叉就会出现「这里备、那里不备」，而用户看到的是同一个开关。
///
/// ⚠️ 读盘失败（设置文件坏了）时 [`load`] 退回默认 = **不备份**。这是故意的：
/// 备份是「留历史」，不是「保命」（保命靠 `trae_backup::swap_in_place` 的原子替换），
/// 所以坏文件下宁可不动用户的磁盘，也不能凭空写出一堆 609 MB。
/// 切号时是否同步定时任务（默认开）。读不到设置文件也按「开」处理。
pub fn sync_workbuddy_automations() -> bool {
    load().sync_workbuddy_automations
}

pub fn backup_before_write() -> bool {
    load().backup_before_write
}

/// 切号时要不要自动把会话同步给所有账号 —— **唯一出口**。
///
/// 与 [`sync_workbuddy_automations`] 同款：调用方一律问它，不许自己 `load()` 再读字段，
/// 否则「设置里开着、切号时没生效」这种分叉迟早出现。
pub fn auto_link_workbuddy_sessions() -> bool {
    load().auto_link_workbuddy_sessions
}

/// 自动同步的增量起点（见 [`Settings::wb_link_since`]）。
pub fn wb_link_since() -> i64 {
    load().wb_link_since
}

/// 记住「已经同步到这个时刻」。写完必须落盘，否则每次切号都会重复处理同一批。
pub fn set_wb_link_since(at_ms: i64) -> Result<(), String> {
    let mut s = load();
    s.wb_link_since = at_ms;
    save(&s).map(|_| ())
}

/// 读设置：**坏文件不报错、退回默认**。
///
/// 后台链路（每次删会话后自动清理）用它 —— 那份 JSON 坏了不该让删除流程失败。
/// 界面要「如实报错」时用 [`load_strict`]。
pub fn load() -> Settings {
    load_strict().map(|s| s.sanitized()).unwrap_or_default()
}

/// 读设置：文件损坏时**明确报错**（界面用）。
///
/// ⚠️ 「空」和「失败」绝不许长得一样：文件不存在 = 全新安装 = 默认值（`Ok`）；
/// 文件在但内容坏了 = 必须让用户知道，不能静默回默认值假装一切正常。
pub fn load_strict() -> Result<Settings, String> {
    let p = settings_path();
    if !p.exists() {
        return Ok(Settings::default());
    }
    let txt = std::fs::read_to_string(&p).map_err(|e| format!("读设置失败（{}）：{e}", p.display()))?;
    if txt.trim().is_empty() {
        return Ok(Settings::default());
    }
    serde_json::from_str::<Settings>(&txt)
        .map(|s| s.sanitized())
        .map_err(|e| format!("设置文件不是合法 JSON（{}）：{e}", p.display()))
}

/// 写设置（原子替换：先写 `settings.json.tmp` 再 rename）。
pub fn save(s: &Settings) -> Result<PathBuf, String> {
    let p = settings_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建设置目录失败：{e}"))?;
    }
    let txt = serde_json::to_string_pretty(&s.clone().sanitized())
        .map_err(|e| format!("序列化设置失败：{e}"))?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, txt).map_err(|e| format!("写设置失败：{e}"))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("替换设置失败：{e}"))?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    fn b(owner: &str, stamp: &str, age_days: i64, mb: u64) -> BatchFacts {
        BatchFacts {
            owner: owner.to_string(),
            stamp: stamp.to_string(),
            at_ms: 1_800_000_000_000 - age_days * DAY,
            bytes: mb * 1_048_576,
        }
    }

    fn now() -> i64 {
        1_800_000_000_000
    }

    #[test]
    fn disabled_policy_selects_nothing() {
        let p = BackupRetention {
            enabled: false,
            keep_days: 1,
            keep_count: 1,
            max_total_mb: 1,
        };
        let bs = vec![b("trae-cn", "s1", 100, 900), b("trae-cn", "s2", 90, 900)];
        assert!(excess_batches(&bs, &p, now()).is_empty());
    }

    #[test]
    fn keep_count_keeps_newest_per_owner() {
        let p = BackupRetention {
            enabled: true,
            keep_days: 0,
            keep_count: 3,
            max_total_mb: 0,
        };
        // trae-cn：5 批（1 天前最新 → 5 天前最旧）；solo-cn：2 批
        let bs = vec![
            b("trae-cn", "s1", 1, 10),
            b("trae-cn", "s2", 2, 10),
            b("trae-cn", "s3", 3, 10),
            b("trae-cn", "s4", 4, 10),
            b("trae-cn", "s5", 5, 10),
            b("solo-cn", "t1", 1, 10),
            b("solo-cn", "t2", 2, 10),
        ];
        // 下标 3、4 是 trae-cn 的第 4、5 批（s4 / s5）；solo-cn 只有 2 批 ≤ 3 ⇒ 不动
        assert_eq!(excess_batches(&bs, &p, now()), vec![3, 4]);
    }

    /// ★ 安全网 —— 这条是「出事能回滚」的唯一依赖，任何规则都不许绕过它。
    #[test]
    fn newest_batch_per_owner_is_never_selected() {
        let p = BackupRetention {
            enabled: true,
            // 三档全部压到最狠：1 天 / 1 份 / 1 MB
            keep_days: 1,
            keep_count: 1,
            max_total_mb: 1,
        };
        // 三个客户端各只有 1 批，且都很旧、都很大 —— 三条规则都会命中，
        // 但「最新一批」必须活下来。
        let bs = vec![
            b("trae-cn", "s1", 400, 900),
            b("solo-cn", "t1", 400, 900),
            b("trae-intl", "u1", 400, 900),
        ];
        assert!(
            excess_batches(&bs, &p, now()).is_empty(),
            "每个客户端的最新一批是回滚安全网，绝不能被策略选中"
        );
        // 但同一个客户端再来一批（这份就成了「旧批」）⇒ 旧的那批可以被选
        let bs2 = vec![b("trae-cn", "s1", 400, 900), b("trae-cn", "s2", 1, 900)];
        assert_eq!(excess_batches(&bs2, &p, now()), vec![0]);
    }

    #[test]
    fn time_rule_selects_old_batches() {
        let p = BackupRetention {
            enabled: true,
            keep_days: 30,
            keep_count: 0,
            max_total_mb: 0,
        };
        let bs = vec![
            b("trae-cn", "new", 1, 10),
            b("trae-cn", "mid", 29, 10),
            b("trae-cn", "old", 31, 10),
            b("trae-cn", "older", 100, 10),
        ];
        assert_eq!(excess_batches(&bs, &p, now()), vec![2, 3]);
    }

    #[test]
    fn size_rule_selects_from_the_old_end() {
        let p = BackupRetention {
            enabled: true,
            keep_days: 0,
            keep_count: 0,
            // 50 MB 上限；每批 30 MB ⇒ 从新到旧：30、60 ⇒ 第 2 批起超限
            max_total_mb: 50,
        };
        let bs = vec![
            b("trae-cn", "s1", 1, 30),
            b("trae-cn", "s2", 2, 30),
            b("trae-cn", "s3", 3, 30),
        ];
        // 从新到旧：s1 累加 30（≤50 留）；s2 累加 60（>50 ⇒ 清）；s3 更旧 ⇒ 清
        assert_eq!(excess_batches(&bs, &p, now()), vec![1, 2]);
    }

    #[test]
    fn unknown_timestamp_is_not_judged_by_time() {
        let p = BackupRetention {
            enabled: true,
            keep_days: 1,
            keep_count: 0,
            max_total_mb: 0,
        };
        let bs = vec![
            BatchFacts {
                owner: "heal-1791298713".into(),
                stamp: String::new(),
                at_ms: 0,
                bytes: 10,
            },
            b("trae-cn", "s1", 1, 10),
        ];
        // at_ms = 0 视为「时间未知」⇒ 不按时间判（否则会被 now - 0 直接判死）
        assert!(excess_batches(&bs, &p, now()).is_empty());
    }

    #[test]
    fn all_three_rules_fire_together() {
        let p = BackupRetention {
            enabled: true,
            keep_days: 10,
            keep_count: 2,
            max_total_mb: 100,
        };
        let bs = vec![
            b("trae-cn", "s1", 1, 80),   // 最新，安全网保
            b("trae-cn", "s2", 2, 80),   // 份数第 2 ⇒ 留；但容量：80+80=160>100 ⇒ 清
            b("trae-cn", "s3", 40, 80),  // 时间超 10 天 ⇒ 清
            b("trae-cn", "s4", 41, 80),  // 同上 + 份数超 ⇒ 清
        ];
        assert_eq!(excess_batches(&bs, &p, now()), vec![1, 2, 3]);
    }

    #[test]
    fn summary_mentions_every_active_threshold() {
        let s = BackupRetention::default().summary();
        assert!(s.contains("30 天"), "{s}");
        assert!(s.contains("3 份"), "{s}");
        assert!(s.contains("4.0 GB"), "{s}");
        assert!(s.contains("最新一批始终保留"), "{s}");
        let off = BackupRetention {
            enabled: false,
            ..BackupRetention::default()
        }
        .summary();
        assert!(off.contains("已关闭"), "{off}");
    }

    #[test]
    fn sanitize_clamps_absurd_values() {
        let s = BackupRetention {
            enabled: true,
            keep_days: u32::MAX,
            keep_count: u32::MAX,
            max_total_mb: u64::MAX,
        }
        .sanitized();
        assert_eq!(s.keep_days, 3650);
        assert_eq!(s.keep_count, 500);
        assert_eq!(s.max_total_mb, 1_048_576);
        // 夹过之后天数乘法不会溢出
        let _ = i64::from(s.keep_days) * 86_400_000;
    }

    /// ⚠️ 字段名是 **camelCase**（磁盘文件与 Tauri 命令入参共用一套命名）。
    /// 老版本写下的 `settings.json` 只有一个 `enabled` —— 新增字段必须能反序列化成功。
    #[test]
    fn camel_case_keys_parse_and_missing_field_falls_back_to_default() {
        let s: Settings =
            serde_json::from_str(r#"{"backupRetention":{"enabled":false}}"#).unwrap();
        assert!(!s.backup_retention.enabled);
        assert_eq!(s.backup_retention.keep_days, 30);
        assert_eq!(s.backup_retention.keep_count, 3);
        assert_eq!(s.backup_retention.max_total_mb, 4096);

        // 写出去的键也必须是 camelCase（否则界面读 `keepDays` 会拿到 undefined）
        let txt = serde_json::to_string(&Settings::default()).unwrap();
        assert!(txt.contains("\"keepDays\""), "{txt}");
        assert!(txt.contains("\"keepCount\""), "{txt}");
        assert!(txt.contains("\"maxTotalMb\""), "{txt}");
        assert!(txt.contains("\"backupRetention\""), "{txt}");

        // 四个值都能从 camelCase JSON 读进来
        let s2: Settings = serde_json::from_str(
            r#"{"backupRetention":{"enabled":true,"keepDays":7,"keepCount":2,"maxTotalMb":512}}"#,
        )
        .unwrap();
        assert_eq!(s2.backup_retention.keep_days, 7);
        assert_eq!(s2.backup_retention.keep_count, 2);
        assert_eq!(s2.backup_retention.max_total_mb, 512);
    }

    /// 「为什么这条被标成可清理」必须与判定同源 —— 界面上写的原因要能对上真正删掉的东西。
    #[test]
    fn reasons_match_the_rules_that_selected_the_batch() {
        let p = BackupRetention {
            enabled: true,
            keep_days: 10,
            keep_count: 2,
            max_total_mb: 100,
        };
        let bs = vec![
            b("trae-cn", "s1", 1, 80),  // 最新 ⇒ 安全网，无原因
            b("trae-cn", "s2", 2, 80),  // 份数第 2（留）+ 容量超 ⇒ 只有 Size
            b("trae-cn", "s3", 40, 80), // 时间超 + 份数超 ⇒ Time + Count
        ];
        let got = excess_with_reasons(&bs, &p, now());
        // 与 `excess_batches` 必须选出同一批下标
        assert_eq!(
            got.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            excess_batches(&bs, &p, now())
        );
        let m: BTreeMap<usize, Vec<ExcessRule>> = got.into_iter().collect();
        assert!(!m.contains_key(&0), "最新一批不入选、也就不该有原因");
        assert!(m[&1].contains(&ExcessRule::Size), "{:?}", m[&1]);
        assert!(m[&2].contains(&ExcessRule::Time), "{:?}", m[&2]);
        assert!(m[&2].contains(&ExcessRule::Count), "{:?}", m[&2]);
        // 原因文案要带上策略里的具体阈值，用户才知道该改哪个数
        assert!(ExcessRule::Time.describe(&p).contains("10 天"));
        assert!(ExcessRule::Count.describe(&p).contains("2 份"));
        assert!(ExcessRule::Size.describe(&p).contains("100 MB"));
    }
}

/// 用户手动指定的 WorkBuddy 主程序路径 —— **文件不存在就当没指定**。
///
/// 与 [`trae_exe_override`] 同一条规矩：校验只在这里做一次，调用方各写一遍
/// 迟早会出现「A 处认了 B 处不认」。
pub fn workbuddy_exe_override() -> Option<PathBuf> {
    let p = PathBuf::from(load().workbuddy_exe.trim());
    p.is_file().then_some(p)
}
