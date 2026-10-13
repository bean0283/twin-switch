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

use std::collections::BTreeMap;
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
    #[serde(default)]
    pub backup_retention: BackupRetention,
}

impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.backup_retention = self.backup_retention.sanitized();
        self
    }
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
