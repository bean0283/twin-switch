//! 积分共享 · 模型自动识别。
//!
//! 用户不要再手写 `models.json`：改成**从本机各客户端里把模型清单读出来**，
//! 勾选哪个就登记哪个。
//!
//! 三家的现实（都实测过，不猜）：
//!
//! | 产品 | 本机来源 | 结果 |
//! | --- | --- | --- |
//! | Trae | `%APPDATA%\Trae [CN]\User\globalStorage\state.vscdb` 里
//!   `<uid>_AI.agent.model.model_list_map` | ✅ 能读（本机实测 41 个模型） |
//! | Qoder | `~/.qoder*/.models/<uid>/catalog-v6` | ❌ 内容是密文，解不开；要拿清单得走线上接口（COSY 签名，未接入） |
//! | WorkBuddy | `~/.workbuddy/models.json` | ❌ 本机是空数组；没有可用清单 |
//!
//! 所以本模块的返回值里**每家都要带一句来源说明**：识别到就报数量，
//! 没识别到就报原因。绝不把「没找到」说成「没有模型」。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::modules::config::store_dir;

/// 一个可勾选的模型。
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredModel {
    /// 作用域（`trae-cn` / `wb-cn` / `qoder-cn` …）
    pub scope: String,
    pub product: String,
    /// 真正要填进客户端的模型名。
    pub model: String,
    /// 客户端里的显示名（可能为空）。
    pub display: String,
    /// 这条是怎么来的：`state.vscdb` / `本地文件` / `手填`
    pub source: String,
}

/// 一个来源的侦察结果（界面要原样显示，别把「没找到」藏起来）。
#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub product: String,
    pub scope: String,
    pub found: usize,
    /// 一句话说明：读到了什么 / 为什么没读到。
    pub note: String,
}

/// WorkBuddy 的客户端 UA。
///
/// ⚠️ 服务端会校验 UA（不带会回 `12403 check ua, get coding copilot version error`）。
/// 这个值取自 WorkBuddy 自己的日志 `~/.workbuddy/logs/*.log`。
/// 客户端升级后若这里失效，表现就是 12403 —— 去日志里抄一条新的即可。
pub const WORKBUDDY_UA: &str = "WorkBuddy/5.8.3 WorkBuddy/5.8.3 CLI/2.161.0";

/// 扫描本机与在线清单，返回 (识别到的模型, 每个来源的报告)。
pub async fn discover() -> (Vec<DiscoveredModel>, Vec<SourceReport>) {
    let mut models = Vec::new();
    let mut reports = Vec::new();

    let (trae, trae_report) = discover_trae();
    models.extend(trae);
    reports.push(trae_report);

    // Qoder：COSY 签名走推理网关的 `/algo/api/v2/model/list`（本机 catalog-v6 是密文，读不出）。
    let (qoder, qoder_reports) = discover_qoder().await;
    models.extend(qoder);
    reports.extend(qoder_reports);
    let (wb, wb_report) = discover_workbuddy().await;
    models.extend(wb);
    reports.push(wb_report);

    (models, reports)
}

// ---------------------------------------------------------------- Qoder

/// 从 Qoder 的推理网关拉模型清单（COSY 签名）。两个区域各自一个来源：
/// 国内版 `qoder-cn`、国际版 `qoder-global`。
///
/// 本机 `catalog-v6` 是密文读不出，但**线上接口能给**——`chat[].key` 就是可用的模型名
/// （`qmodel` / `dmodel` / `gmodel` 这类代号，`display_name` 才是可读名）。
async fn discover_qoder() -> (Vec<DiscoveredModel>, Vec<SourceReport>) {
    use crate::modules::qoder_realm::QoderRegion;

    let mut out = Vec::new();
    let mut reports = Vec::new();
    for region in [QoderRegion::Cn, QoderRegion::Global] {
        let scope = format!("qoder-{}", region.as_str());
        let accounts = crate::modules::qoder_accounts::list(region).unwrap_or_default();
        let Some(acc) = accounts.into_iter().find(|a| a.is_usable()) else {
            reports.push(SourceReport {
                product: "Qoder".into(),
                scope,
                found: 0,
                note: format!("{} 没有可用账号（先把账号加进来）", region.display_name()),
            });
            continue;
        };
        match crate::modules::qoder_tools::fetch_models(region, &acc).await {
            Ok(list) => {
                let found = list.len();
                for (key, display) in list {
                    out.push(DiscoveredModel {
                        scope: format!("qoder-{}", region.as_str()),
                        product: "Qoder".into(),
                        model: key,
                        display,
                        source: "线上模型清单（COSY /algo/api/v2/model/list）".into(),
                    });
                }
                reports.push(SourceReport {
                    product: "Qoder".into(),
                    scope: format!("qoder-{}", region.as_str()),
                    found,
                    note: format!("{}：线上模型清单（COSY）", region.display_name()),
                });
            }
            Err(e) => reports.push(SourceReport {
                product: "Qoder".into(),
                scope: format!("qoder-{}", region.as_str()),
                found: 0,
                note: format!("拉取失败：{e}"),
            }),
        }
    }
    (out, reports)
}

// ---------------------------------------------------------------- Trae

fn discover_trae() -> (Vec<DiscoveredModel>, SourceReport) {
    let mut out: Vec<DiscoveredModel> = Vec::new();
    let mut last_err = String::new();
    let mut found_db = false;

    for (dir_name, scope) in [("Trae CN", "trae-cn"), ("Trae", "trae-global")] {
        let Some(db) = trae_db_path(dir_name) else {
            continue;
        };
        if !db.is_file() {
            continue;
        }
        found_db = true;
        match read_trae_models(&db) {
            Ok(names) => {
                for (model, display) in names {
                    out.push(DiscoveredModel {
                        scope: scope.into(),
                        product: "Trae".into(),
                        model,
                        display,
                        source: "state.vscdb".into(),
                    });
                }
            }
            Err(e) => last_err = e,
        }
    }

    // 去重（同一个模型会出现在多个 uid / 多个场景里）
    out.sort_by(|a, b| a.scope.cmp(&b.scope).then_with(|| a.model.cmp(&b.model)));
    out.dedup_by(|a, b| a.scope == b.scope && a.model == b.model);

    let found = out.len();
    let note = if !found_db {
        "没找到 Trae 的状态库（`%APPDATA%\\Trae CN\\User\\globalStorage\\state.vscdb`），可能没装或还没登录过。".into()
    } else if out.is_empty() {
        if last_err.is_empty() {
            "状态库里没有模型清单（可能还没打开过对话）。".into()
        } else {
            format!("读取状态库失败：{last_err}")
        }
    } else {
        format!("从状态库读到 {} 个模型。", out.len())
    };

    (
        out,
        SourceReport {
            product: "Trae".into(),
            scope: "trae-cn".into(),
            found,
            note,
        },
    )
}

fn trae_db_path(dir_name: &str) -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join(dir_name)
            .join("User")
            .join("globalStorage")
            .join("state.vscdb"),
    )
}

/// 从 Trae 状态库读模型清单。
///
/// 结构：`{ 场景: [ { name, display_name, … } ] }`，key 形如
/// `<uid>_AI.agent.model.model_list_map`。只读打开，不碰写锁。
fn read_trae_models(db: &std::path::Path) -> Result<Vec<(String, String)>, String> {
    use rusqlite::{Connection, OpenFlags};

    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("打开状态库失败：{e}"))?;

    let mut stmt = conn
        .prepare("select value from ItemTable where key like '%model_list_map%'")
        .map_err(|e| format!("查询失败：{e}"))?;

    let rows = stmt
        .query_map([], |r| r.get::<_, rusqlite::types::Value>(0))
        .map_err(|e| format!("读取失败：{e}"))?;

    let mut out = Vec::new();
    for row in rows.flatten() {
        // 有的版本存 TEXT，有的存 BLOB 的 UTF-8 —— 两种都认。
        let text = match row {
            rusqlite::types::Value::Text(s) => s,
            rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).to_string(),
            _ => continue,
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(scenes) = parsed.as_object() else {
            continue;
        };
        for arr in scenes.values() {
            let Some(list) = arr.as_array() else { continue };
            for m in list {
                let Some(name) = m.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if name.trim().is_empty() {
                    continue;
                }
                let display = m
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                out.push((name.to_string(), display));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- WorkBuddy（在线清单）

/// 从 WorkBuddy 的 `/v3/config` 拉模型清单。
///
/// 本机 `~/.workbuddy/models.json` 是空的，真正的清单在服务端下发：
/// `data.models[]`，每条有 `id`（真正要填进客户端的名字）与 `name`（显示名）。
async fn discover_workbuddy() -> (Vec<DiscoveredModel>, SourceReport) {
    let scope = "wb-cn";
    let Some(token) = workbuddy_token() else {
        return (
            Vec::new(),
            SourceReport {
                product: "WorkBuddy".into(),
                scope: scope.into(),
                found: 0,
                note: "没有可用的 WorkBuddy 登录态（先登录一次）。".into(),
            },
        );
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                Vec::new(),
                SourceReport {
                    product: "WorkBuddy".into(),
                    scope: scope.into(),
                    found: 0,
                    note: format!("构造 HTTP 客户端失败：{e}"),
                },
            )
        }
    };

    let resp = client
        .get("https://www.workbuddy.cn/v3/config")
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", WORKBUDDY_UA)
        .header("x-product", "SaaS")
        .header("accept", "application/json")
        .send()
        .await;

    let (found, note) = match resp {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(v) => {
                let mut out = Vec::new();
                if let Some(list) = v.pointer("/data/models").and_then(Value::as_array) {
                    for m in list {
                        let Some(id) = m.get("id").and_then(Value::as_str) else {
                            continue;
                        };
                        if id.trim().is_empty() {
                            continue;
                        }
                        let display = m
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        out.push(DiscoveredModel {
                            scope: scope.into(),
                            product: "WorkBuddy".into(),
                            model: id.to_string(),
                            display,
                            source: "/v3/config".into(),
                        });
                    }
                }
                out.sort_by(|a, b| a.model.cmp(&b.model));
                out.dedup_by(|a, b| a.model == b.model);
                let n = out.len();
                let note = if n == 0 {
                    "配置里 `data/models` 为空。".to_string()
                } else {
                    format!("从服务端配置拉到 {n} 个模型（这是唯一权威清单，注意大小写）。")
                };
                (out, note)
            }
            Err(e) => (Vec::new(), format!("配置响应不是 JSON：{e}")),
        },
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.text().await.unwrap_or_default();
            let body: String = body.chars().take(200).collect();
            (
                Vec::new(),
                if body.contains("12403") {
                    format!("服务端校验 UA 失败（12403）。客户端升级后需要更新 WORKBUDDY_UA。原始：{body}")
                } else {
                    format!("配置接口返回 HTTP {status}：{body}")
                },
            )
        }
        Err(e) => (Vec::new(), format!("请求配置失败：{e}")),
    };

    let n = found.len();
    (
        found,
        SourceReport { product: "WorkBuddy".into(), scope: scope.into(), found: n, note },
    )
}

/// 取一个 WorkBuddy 的 access_token（国内版账号）。
fn workbuddy_token() -> Option<String> {
    let text = std::fs::read_to_string(store_dir().join("workbuddy-accounts.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("accounts")?.as_array()?.iter().find_map(|a| {
        let domain = a.get("domain").and_then(Value::as_str).unwrap_or_default();
        if !domain.contains("cn") {
            return None;
        }
        a.get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())
            .map(str::to_string)
    })
}

// ---------------------------------------------------------------- models.json

pub fn models_file() -> PathBuf {
    store_dir().join("models.json")
}

/// 已登记（勾选过）的模型：`{ 作用域: [模型…] }`。
/// 识别 + **落盘**（合并写入 `models.json`）。
///
/// ## 为什么要落盘
///
/// `discover()` 只把结果放在内存里返回 ⇒ 新电脑上第一次打开页面时文件根本不存在，
/// 选号/预览会直接报「模型目录为空」。所以：
/// - **每个识别成功的作用域覆盖写入**；
/// - 识别失败的作用域（没账号 / 网络不通）**保留旧值**，绝不因为一次失败就清空；
/// - 目录不存在时自动建（`store_dir()` 下）。
pub async fn discover_and_save() -> (Vec<DiscoveredModel>, Vec<SourceReport>) {
    let (models, reports) = discover().await;
    if models.is_empty() {
        return (models, reports);
    }
    let path = models_file();
    let mut map: BTreeMap<String, Vec<String>> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();

    let mut by_scope: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in &models {
        by_scope.entry(m.scope.clone()).or_default().push(m.model.clone());
    }
    for (scope, list) in by_scope {
        if !list.is_empty() {
            map.insert(scope, list);
        }
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(&path, text);
    }
    (models, reports)
}

pub fn load_selected() -> BTreeMap<String, Vec<String>> {
    let path = models_file();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 保存勾选结果。空值会被清掉，避免留下空数组干扰判断。
pub fn save_selected(map: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let cleaned: BTreeMap<String, Vec<String>> = map
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| {
            let mut v = v.clone();
            v.sort();
            v.dedup();
            (k.clone(), v)
        })
        .collect();
    let path = models_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let body = serde_json::to_vec_pretty(&cleaned).map_err(|e| format!("序列化失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("写入失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("落盘失败：{e}"))
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    /// 把**三个产品**的模型清单一次性同步进 `~/.twin-switch/models.json`。
    ///
    /// 这是「让某个产品的积分可以被别的程序用」的关键一步：`models.json` 决定了
    /// 网关 `/v1/models` 能列什么、以及裸模型名路由到哪个作用域。
    #[tokio::test]
    #[ignore]
    async fn sync_models_json() {
        let path = crate::modules::config::store_dir().join("models.json");
        let (models, reports) = discover().await;
        let mut map: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for m in &models {
            map.entry(m.scope.clone()).or_default().push(m.model.clone());
        }
        for (scope, list) in &map {
            eprintln!("[{scope}] {} 个：{:?}", list.len(), &list[..list.len().min(6)]);
        }
        for r in &reports {
            eprintln!("  {}", r.note.chars().take(90).collect::<String>());
        }
        if let Ok(text) = serde_json::to_string_pretty(&map) {
            let _ = std::fs::write(&path, text);
            eprintln!("已写 {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trae_db_path_uses_appdata() {
        std::env::set_var("APPDATA", r"C:\fake\Roaming");
        let p = trae_db_path("Trae CN").unwrap();
        assert!(p.ends_with("state.vscdb"));
        assert!(p.to_string_lossy().contains("Trae CN"));
    }

    #[test]
    fn save_cleans_empty_and_sorts() {
        let mut map = BTreeMap::new();
        map.insert("trae-cn".to_string(), vec!["b".into(), "a".into(), "a".into()]);
        map.insert("wb-cn".to_string(), vec![]);
        let cleaned: BTreeMap<String, Vec<String>> = map
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| {
                let mut v = v.clone();
                v.sort();
                v.dedup();
                (k.clone(), v)
            })
            .collect();
        assert_eq!(cleaned.get("trae-cn").unwrap(), &vec!["a".to_string(), "b".to_string()]);
        assert!(cleaned.get("wb-cn").is_none());
    }
}
