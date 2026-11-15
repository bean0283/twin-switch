//! 账号优先级：用户自定义的派单顺序（拖拽排序的结果）。
//!
//! ## 为什么单独存一份，而不是塞进各产品的账号库
//!
//! 需求是两条同时成立：
//! 1. **通用模型**（裸模型名，可能被任一作用域提供）→ 所有账号**全局**按优先级；
//! 2. **特定模型**（`trae:glm-5.3`）→ 只在该产品的账号内部按优先级。
//!
//! 如果每个产品各存一份顺序，第 1 条就没法实现（跨产品之间谁先谁后没有答案）。
//! 所以这里只有**一条全局列表**；第 2 条靠「在这条列表里筛出该产品的账号」自动得到 ——
//! 也就是产品内的相对顺序天然与全局一致，不需要两套数据互相同步。
//!
//! ## 落盘
//!
//! `~/.twin-switch/gateway-priority.json`：`{ "order": ["trae:uid…", "wb:uid…"] }`。
//!
//! ⚠️ 列表里**只存 key，不存余额/作用域**：那些每分钟都在变，存进来就会过期；
//! 读的时候拿 key 与当前池子对照，池子里没有的 key 直接忽略（账号被删了也不报错）。
//! 没进列表的账号排在**所有已列账号之后**，同组内仍按「最早过期」——升级后不突变行为。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    order: Vec<String>,
}

fn path() -> PathBuf {
    crate::modules::config::store_dir().join("gateway-priority.json")
}

/// mtime 缓存：网关热路径每轮都要问顺序，不能每次都读盘。
static CACHE: OnceLock<Mutex<Option<(SystemTime, u64, Vec<String>)>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<(SystemTime, u64, Vec<String>)>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 当前优先级顺序（`产品:账号` 列表，靠前的优先）。
pub fn order() -> Vec<String> {
    let p = path();
    let Ok(meta) = std::fs::metadata(&p) else {
        return Vec::new();
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let size = meta.len();
    if let Ok(g) = cache().lock() {
        if let Some((m, s, v)) = g.as_ref() {
            if *m == mtime && *s == size {
                return v.clone();
            }
        }
    }
    let parsed = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str::<Stored>(&t).ok())
        .map(|s| s.order)
        .unwrap_or_default();
    if let Ok(mut g) = cache().lock() {
        *g = Some((mtime, size, parsed.clone()));
    }
    parsed
}

/// 去空白、去重、保持用户给的次序（纯函数；`save` 与单测共用）。
fn clean(order: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    order
        .into_iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .filter(|k| seen.insert(k.clone()))
        .collect()
}

/// 把「已存的顺序 + 当前池子的 key」合并成一条新顺序（纯函数）。
///
/// 规则：已存且仍在池子里的**保持原相对次序**，池子里新增的接在末尾。
fn merge_existing(existing: &[String], pool_keys: &[String]) -> Vec<String> {
    let mut out: Vec<String> = existing
        .iter()
        .filter(|k| pool_keys.contains(k))
        .cloned()
        .collect();
    for k in pool_keys {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// 保存顺序：去空白、去重、保持用户给的次序。
///
/// 空列表是**合法**的：等于「没有自定义优先级」，全部回落到按最早过期。
pub fn save(order: Vec<String>) -> Result<Vec<String>, String> {
    let cleaned = clean(order);
    let body = serde_json::to_string_pretty(&Stored { order: cleaned.clone() })
        .map_err(|e| format!("序列化失败：{e}"))?;
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("落盘失败：{e}"))?;
    // mtime 精度可能不足一秒，只靠它会让「刚拖完又拖回去」读到旧值。
    if let Ok(mut g) = cache().lock() {
        *g = None;
    }
    Ok(cleaned)
}

/// 某个账号在优先级里的名次（0 = 最优先）。
///
/// `None` = 没被用户排过 ⇒ 调用方必须把它排在**所有有名次的账号之后**
/// （用 `usize::MAX`），否则「新加进来的账号」会莫名其妙插到队首。
pub fn rank(key: &str) -> Option<usize> {
    order().iter().position(|k| k == key)
}

/// 给一组 key 排出「用户看到的顺序」：有名次的按名次，其余按 `fallback` 的相对次序。
///
/// 前端拖拽后回传的整份列表用它做一次规范化，避免把池子里已经不存在的 key 一起存进去。
pub fn merge_with_pool(pool_keys: &[String]) -> Vec<String> {
    merge_existing(&order(), pool_keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ⚠️ 这里**不碰磁盘**：`save()` / `order()` 写的是用户目录，沙箱下会「拒绝访问」，
    // 并行跑还会互相覆盖。所以单测只钉纯函数，落盘由界面上的拖拽自证。
    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clean_dedupes_and_trims() {
        assert_eq!(
            clean(v(&[" trae:a ", "wb:b", "trae:a", "  "])),
            v(&["trae:a", "wb:b"])
        );
    }

    #[test]
    fn merge_keeps_saved_order_and_appends_new() {
        // 已存的顺序不能因为池子顺序变化而被打乱；新账号接在最后。
        let merged = merge_existing(
            &v(&["wb:b", "trae:a"]),
            &v(&["trae:a", "wb:b", "qoder:new"]),
        );
        assert_eq!(merged, v(&["wb:b", "trae:a", "qoder:new"]));
    }

    #[test]
    fn merge_drops_keys_that_left_the_pool() {
        let merged = merge_existing(&v(&["gone:x", "trae:a"]), &v(&["trae:a", "wb:new"]));
        assert_eq!(merged, v(&["trae:a", "wb:new"]));
        assert!(!merged.contains(&"gone:x".to_string()), "账号被删了不该一直占着名次");
    }
}
