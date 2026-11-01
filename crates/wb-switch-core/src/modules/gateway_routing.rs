//! 积分共享的**选号策略**（用户可切换的三档）。
//!
//! ## 为什么需要这个
//!
//! 界面上「账号池」按**积分最早过期**排序，用户看到的是「谁快过期就先花谁」。
//! 但实际选号还有一层**会话粘性**：同一段对话固定用同一个账号，只有它不可用才换
//! （原因见 [`crate::modules::gateway_server`] 选号段：上游会话 ID 的指纹含 uid，
//! 中途换号等于换了一个上游会话，前缀缓存全失效、观感上「对话断了」）。
//!
//! 两件事都合理，但**摆在一起就会让人误解**：看起来该按过期挑，实际一直用同一个号。
//! 所以把「到底怎么挑」做成显式选项，让用户自己选，而不是替他藏着。
//!
//! ## 三档
//!
//! | 策略 | 行为 | 代价 |
//! | --- | --- | --- |
//! | [`CreditPolicy::Strict`] | 每一轮都按「最早过期」重挑 | 中途可能换号 ⇒ 上游会话断开、前缀缓存失效 |
//! | [`CreditPolicy::Sticky`] | 粘住当前账号，不可用才按最早过期重挑（**默认**，即原行为） | 账号池排序只对「新对话」生效 |
//! | [`CreditPolicy::Pinned`] | 只用指定账号；它不可用 → 退化成 `Sticky` → 再不行退化成 `Strict` | 指定账号额度耗尽时会先用别的号 |
//!
//! ## 存哪
//!
//! `~/.twin-switch/gateway-routing.json`。**故意不放 `settings.json`**：
//! 那边是「本工具怎么干活」的全局设置，这个是网关热路径上的选号规则，
//! 混在一起会让「网关为什么不按设置走」变得更难查。
//!
//! 读取带 mtime 缓存：网关每个请求都要问一次策略，但文件几乎不变，
//! 所以只在 mtime/size 变了才重新解析。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// 选号策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreditPolicy {
    /// 严格按「积分最早过期」选，**每一轮都重挑**。
    #[serde(rename = "strict")]
    Strict,
    /// 同一段对话粘住同一个账号，它不可用才重挑。
    #[serde(rename = "sticky")]
    Sticky,
    /// 只用指定账号；不可用时依次退化为 `Sticky` → `Strict`。
    #[serde(rename = "pinned")]
    Pinned,
}

impl Default for CreditPolicy {
    fn default() -> Self {
        // 默认 = 原行为，升级后不许突变。
        CreditPolicy::Sticky
    }
}

impl CreditPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            CreditPolicy::Strict => "strict",
            CreditPolicy::Sticky => "sticky",
            CreditPolicy::Pinned => "pinned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(CreditPolicy::Strict),
            "sticky" => Some(CreditPolicy::Sticky),
            "pinned" => Some(CreditPolicy::Pinned),
            _ => None,
        }
    }
}

/// 落盘结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    #[serde(default)]
    pub policy: CreditPolicy,
    /// 指定账号的池子 key（`产品:账号标识`）。仅 `pinned` 用得上。
    ///
    /// 空串 = 没指定 ⇒ `pinned` 会直接退化成 `sticky`，不会「挑不到号就报错」。
    #[serde(default)]
    pub pinned: String,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            policy: CreditPolicy::default(),
            pinned: String::new(),
        }
    }
}

fn config_path() -> PathBuf {
    crate::modules::config::store_dir().join("gateway-routing.json")
}

/// mtime 缓存：`(mtime, size, 解析结果)`。
static CACHE: OnceLock<Mutex<Option<(SystemTime, u64, RoutingPolicy)>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<(SystemTime, u64, RoutingPolicy)>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 当前策略（网关热路径调用，带 mtime 缓存）。
pub fn current() -> RoutingPolicy {
    let path = config_path();
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        // 文件不存在 = 全新安装 / 从没设置过 ⇒ 默认策略。
        Err(_) => return RoutingPolicy::default(),
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let size = meta.len();

    if let Ok(g) = cache().lock() {
        if let Some((m, s, p)) = g.as_ref() {
            if *m == mtime && *s == size {
                return p.clone();
            }
        }
    }

    let parsed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<RoutingPolicy>(&t).ok())
        .unwrap_or_default();
    if let Ok(mut g) = cache().lock() {
        *g = Some((mtime, size, parsed.clone()));
    }
    parsed
}

/// 原样读盘（给界面看；解析失败时返回默认值并带 `parseError`，不静默）。
pub fn load_report() -> serde_json::Value {
    let path = config_path();
    let raw = std::fs::read_to_string(&path).ok();
    let mut parse_error: Option<String> = None;
    let policy = match raw.as_deref() {
        Some(t) => match serde_json::from_str::<RoutingPolicy>(t) {
            Ok(p) => p,
            Err(e) => {
                parse_error = Some(e.to_string());
                RoutingPolicy::default()
            }
        },
        None => RoutingPolicy::default(),
    };
    serde_json::json!({
        "path": path.display().to_string(),
        "exists": raw.is_some(),
        "policy": policy.policy.as_str(),
        "pinned": policy.pinned,
        "parseError": parse_error,
    })
}

/// 写入（合并式：只改传进来的字段）。
pub fn save(policy: Option<CreditPolicy>, pinned: Option<String>) -> Result<RoutingPolicy, String> {
    let mut cur = current();
    if let Some(p) = policy {
        cur.policy = p;
    }
    if let Some(k) = pinned {
        cur.pinned = k.trim().to_string();
    }
    // `pinned` 选了策略但没给账号 = 等于没指定，明确落成空，别留一半状态。
    if cur.policy != CreditPolicy::Pinned {
        // 不清空：用户切回 pinned 时还能用上次的号。只是当前不生效。
    }
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let text = serde_json::to_string_pretty(&cur).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("写 {} 失败：{e}", path.display()))?;
    // 立刻让缓存失效（mtime 精度可能不足一秒，不能只靠它）。
    if let Ok(mut g) = cache().lock() {
        *g = None;
    }
    Ok(cur)
}

/// 按策略决定这次用哪个号。
///
/// 三个入参都是**已经筛过可用性**的池子 key（None = 该来源没有候选）：
/// - `pinned`：指定账号，且它当前可用
/// - `sticky`：这段对话上次用的号，且它当前可用
/// - `urgency`：按「积分最早过期」排出来的那个
///
/// 返回值 `None` 表示三个来源都空 ⇒ 调用方按「没有可用账号」处理。
///
/// ⚠️ 这个函数是**纯函数**，选号优先级只在这里定义一次。
/// 别在调用处再写一遍 `if`，否则「界面说一套、实际走另一套」的老问题会回来。
pub fn pick_key(
    policy: CreditPolicy,
    pinned: Option<String>,
    sticky: Option<String>,
    urgency: Option<String>,
) -> Option<String> {
    match policy {
        CreditPolicy::Pinned => pinned.or(sticky).or(urgency),
        CreditPolicy::Sticky => sticky.or(urgency),
        CreditPolicy::Strict => urgency,
    }
}

/// 这个策略要不要记会话粘性。
///
/// `Strict` 不记：它每轮都重挑，绑了也没人用，反而会让用户切回 `sticky` 时
/// 莫名其妙粘到一个「严格模式下碰巧最后用过」的号。
pub fn should_bind(policy: CreditPolicy) -> bool {
    policy != CreditPolicy::Strict
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn strict_always_takes_urgency() {
        // 指定了号也不用、粘住的也不用 —— 严格模式只认「最早过期」。
        assert_eq!(
            pick_key(CreditPolicy::Strict, s("pin"), s("stick"), s("urg")),
            s("urg")
        );
    }

    #[test]
    fn sticky_prefers_sticky_then_falls_back() {
        assert_eq!(
            pick_key(CreditPolicy::Sticky, s("pin"), s("stick"), s("urg")),
            s("stick")
        );
        // 粘住的号不可用时（调用方传 None）落到紧迫度
        assert_eq!(
            pick_key(CreditPolicy::Sticky, s("pin"), None, s("urg")),
            s("urg")
        );
    }

    #[test]
    fn pinned_degrades_through_sticky_to_urgency() {
        // 完整三级：指定 → 粘住 → 紧迫度
        assert_eq!(
            pick_key(CreditPolicy::Pinned, s("pin"), s("stick"), s("urg")),
            s("pin")
        );
        assert_eq!(
            pick_key(CreditPolicy::Pinned, None, s("stick"), s("urg")),
            s("stick")
        );
        assert_eq!(pick_key(CreditPolicy::Pinned, None, None, s("urg")), s("urg"));
    }

    #[test]
    fn returns_none_when_everything_missing() {
        for p in [CreditPolicy::Strict, CreditPolicy::Sticky, CreditPolicy::Pinned] {
            assert_eq!(pick_key(p, None, None, None), None);
        }
    }

    #[test]
    fn default_is_sticky_so_upgrade_does_not_change_behavior() {
        assert_eq!(CreditPolicy::default(), CreditPolicy::Sticky);
        assert_eq!(RoutingPolicy::default().policy, CreditPolicy::Sticky);
    }

    #[test]
    fn policy_strings_roundtrip() {
        for p in [CreditPolicy::Strict, CreditPolicy::Sticky, CreditPolicy::Pinned] {
            assert_eq!(CreditPolicy::parse(p.as_str()), Some(p));
        }
        assert_eq!(CreditPolicy::parse("nonsense"), None);
    }

    /// 指定账号**必须受作用域约束**。
    ///
    /// 这条不在 `pick_key` 里（那是个纯函数，拿不到池子），而是钉在调用方的
    /// `find_usable` 上：它要同时校验「在池子里 + 作用域匹配 + 没冷却」。
    /// 回归用：2026-11-01 第一版只查了「池子里 + 没冷却」，指定一个 Qoder 号、
    /// 请求 Trae 模型时会把 Qoder 的号硬塞进去 —— 越权且必然失败。
    #[test]
    fn pinned_selection_must_respect_scope() {
        let src = include_str!("gateway_server.rs");
        assert!(
            src.contains("sel.candidates.iter().any(|s| s == &e.scope)"),
            "find_usable 必须校验作用域，否则指定账号会跨产品越权"
        );
    }

    #[test]
    fn strict_does_not_bind_sticky() {
        assert!(!should_bind(CreditPolicy::Strict));
        assert!(should_bind(CreditPolicy::Sticky));
        assert!(should_bind(CreditPolicy::Pinned));
    }

    #[test]
    fn serde_uses_explicit_lowercase_names() {
        // 落盘值必须显式（历史坑：靠 rename_all 推会把 OAuth 写成 o_auth）。
        assert_eq!(serde_json::to_string(&CreditPolicy::Pinned).unwrap(), "\"pinned\"");
        let p: RoutingPolicy = serde_json::from_str(r#"{"policy":"strict","pinned":"x"}"#).unwrap();
        assert_eq!(p.policy, CreditPolicy::Strict);
        assert_eq!(p.pinned, "x");
        // 老文件没有 policy 键 ⇒ 默认 sticky（不许行为突变）
        let p2: RoutingPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(p2.policy, CreditPolicy::Sticky);
    }
}
