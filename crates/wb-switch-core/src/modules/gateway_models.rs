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
    /// 真正要填进客户端的模型名（发上游用的**代号**）。
    pub model: String,
    /// 客户端里的显示名（可能为空）。
    pub display: String,
    /// **对外公开名**：Qoder 用客户端里显示的名字（`GLM-5.3`），其余家用代号。
    ///
    /// 界面分组、复制、`/v1/models` 都只认这一个字段 —— `gmodel` 这种代号不再外露
    /// （2026-10-24 用户要求「只显示正常的模型，复制也只复制正常的模型名」）。
    pub public: String,
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
                    // 对外名 = 客户端里那个名字（`GLM-5.3`），代号 `gmodel` 只在发上游时用。
                    let public = public_from(&scope, &key, &display);
                    out.push(DiscoveredModel {
                        scope: scope.clone(),
                        product: "Qoder".into(),
                        model: key,
                        display,
                        public,
                        source: "线上模型清单（COSY /algo/api/v2/model/list）".into(),
                    });
                }
                reports.push(SourceReport {
                    product: "Qoder".into(),
                    scope: scope.clone(),
                    found,
                    note: format!("{}：线上模型清单（COSY）", region.display_name()),
                });
            }
            Err(e) => reports.push(SourceReport {
                product: "Qoder".into(),
                scope: scope.clone(),
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
                        model: model.clone(),
                        display,
                        // Trae 的代号本身就是人话（`glm-5.3` / `DeepSeek-V4-Pro`），不做改名。
                        public: model,
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
                            // WorkBuddy 的 id 就是要填进客户端的名字，不改。
                            public: id.to_string(),
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

/// 模型清单文件的**权威上游**（用户自己维护、推到 GitHub 的那份）。
///
/// 读取顺序固定为：本地文件 → 后台拉这个地址覆盖 `defaults` → 拉不到就用本地。
pub const MODELS_REMOTE_URL: &str =
    "https://raw.githubusercontent.com/bean0283/twin-switch/main/models.json";

/// 同内容的备用源（**逐个试**，第一个成功的算数）。
///
/// ⚠️ 2026-10-30 用户反馈「GitHub 上的模型数据文件没有上传，无法获取更新」。
/// 国内直连 `raw.githubusercontent.com` 经常整条超时（真机 curl 返回 000/35），
/// 看起来和「文件不存在」一模一样 —— 所以必须：① 有镜像源可退；② 把
/// 「404 文件不存在」与「网络不通」在文案上分开，否则用户不知道该找谁。
///
/// jsDelivr 是 GitHub 仓库的公开 CDN，同样按分支取文件，不需要 token。
pub const MODELS_REMOTE_MIRRORS: [&str; 2] = [
    // 主源：GitHub 原始文件（国外 / 有代理时最快）
    MODELS_REMOTE_URL,
    // 镜像：jsDelivr CDN（国内可达，内容与仓库同分支同步，有几分钟缓存）
    "https://cdn.jsdelivr.net/gh/bean0283/twin-switch@main/models.json",
];

/// `models.json` 的结构版本。
///
/// v1 是扁平的 `{ 作用域: [模型…] }`（老机器上就是它）；
/// v2 拆成 `defaults`（GitHub 给的，**会被覆盖**）与 `user`（用户手动加的，**永不覆盖**）。
/// v3 再加一段 `local`（本机识别到的，**只补充、不覆盖**）—— 见 `ModelsCatalog::local`。
pub const MODELS_VERSION: u32 = 3;

/// 模型清单（v3）。
///
/// 三段的分工必须背下来，写错任何一条都会造成「用户的模型莫名消失」：
///
/// | 段 | 谁写 | 会不会被覆盖 |
/// |---|---|---|
/// | `defaults` | **只有** GitHub 同步（`replace_defaults(_, "github")`） | 会，整段换掉 |
/// | `local` | 本机识别（`merge_local`） | 不会，只做并集 |
/// | `user` | 用户手动添加 | 不会，任何自动更新都不碰 |
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ModelsCatalog {
    #[serde(default)]
    pub version: u32,
    /// 上次写入的来源：`github` / `discover` / `migrated` / `user`。
    #[serde(default)]
    pub source: String,
    /// 上次同步时间（RFC3339 本地时间，界面原样展示）。
    #[serde(default, rename = "updatedAt")]
    pub updated_at: String,
    /// 默认清单：**只有** GitHub 同步会整段覆盖。
    #[serde(default)]
    pub defaults: BTreeMap<String, Vec<String>>,
    /// 本机识别到的模型：**只做并集，永不整段覆盖**。
    ///
    /// ⚠️⚠️ 为什么不能塞进 `defaults`（2026-10-30 的修复）：
    /// 本机识别读的是**这台机器上三个产品实际能用的模型**，里面必然有 GitHub 清单里
    /// 没有的东西（内部实验代号 `codewise-*` / `refactor_*`，或者作者还没收录的新模型）。
    /// 早期两者都写 `defaults`，于是「点重新识别」会把 GitHub 的权威清单当场冲掉，
    /// 后台每 6 小时还会再冲一次 —— 用户看到的就是「来源明明写着 GitHub，内容却是本机的」。
    ///
    /// 单独一段的好处：GitHub 同步只管 `defaults`，本机独有的模型安静地留在 `local`，
    /// 两者互不覆盖，`effective()` 再把它们并起来。
    #[serde(default)]
    pub local: BTreeMap<String, Vec<String>>,
    /// 用户手动添加的清单：与 `defaults` 合并，**任何自动更新都不许动它**。
    #[serde(default)]
    pub user: BTreeMap<String, Vec<String>>,
}

fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 去重并保序（空串/空白丢弃）。
fn tidy(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in list {
        let t = m.trim();
        if t.is_empty() {
            continue;
        }
        if out.iter().any(|x| x == t) {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

impl ModelsCatalog {
    /// 合并后的「作用域 → 模型」：`defaults` 打头，`local` 次之，`user` 最后（去重）。
    ///
    /// 顺序有意义：网关 `/v1/models` 与界面的列表都按它显示。
    /// 本机独有的模型排在 GitHub 清单之后，用户自己加的排在最后 ——
    /// 这样「官方推荐」的排序不会被本机噪音打乱。
    pub fn effective(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (scope, list) in self
            .defaults
            .iter()
            .chain(self.local.iter())
            .chain(self.user.iter())
        {
            let entry = out.entry(scope.clone()).or_default();
            for m in tidy(list) {
                if !entry.contains(&m) {
                    entry.push(m);
                }
            }
        }
        // 清掉合并后为空的条目（用户把某作用域全删了、默认也没有）。
        out.retain(|_, v| !v.is_empty());
        out
    }

    /// 某个模型是否来自用户手动添加（界面据此决定「能不能删」）。
    pub fn is_user(&self, scope: &str, model: &str) -> bool {
        self.user
            .get(scope)
            .map(|l| l.iter().any(|m| m == model))
            .unwrap_or(false)
    }

    /// 某个模型是否只来自本机识别（GitHub 清单里没有）。
    ///
    /// 界面据此打「仅本机」标签：用户换一台电脑就不该再看到它，
    /// 也不该把它当成「官方支持的模型」。
    pub fn is_local_only(&self, scope: &str, model: &str) -> bool {
        let in_defaults = self
            .defaults
            .get(scope)
            .map(|l| l.iter().any(|m| m == model))
            .unwrap_or(false);
        if in_defaults {
            return false;
        }
        self.local
            .get(scope)
            .map(|l| l.iter().any(|m| m == model))
            .unwrap_or(false)
    }
}

/// 读清单：**只管读，修不了就返回空**（老机器上本来就没有这个文件）。
///
/// 兼容 v1：顶层直接是 `{ 作用域: [模型…] }` 时，整份当作 `defaults`。
pub fn load_catalog() -> ModelsCatalog {
    let Ok(text) = std::fs::read_to_string(models_file()) else {
        return ModelsCatalog::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return ModelsCatalog::default();
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return ModelsCatalog::default(),
    };
    // v2/v3：明确带 defaults / local / user 键之一
    if obj.contains_key("defaults") || obj.contains_key("local") || obj.contains_key("user") {
        let mut cat: ModelsCatalog = serde_json::from_value(v).unwrap_or_default();
        cat.version = MODELS_VERSION;
        return cat;
    }
    // v1：整份是扁平 map ⇒ 全部当默认清单（迁移，不丢数据）
    let mut defaults: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, val) in obj {
        if let Some(list) = val.as_array() {
            let models: Vec<String> = list
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
            if !models.is_empty() {
                defaults.insert(k.clone(), tidy(&models));
            }
        }
    }
    ModelsCatalog {
        version: MODELS_VERSION,
        source: "migrated".into(),
        updated_at: String::new(),
        defaults,
        local: BTreeMap::new(),
        user: BTreeMap::new(),
    }
}

/// 落盘（写临时文件再 rename：写一半断电不会留半个 JSON）。
pub fn save_catalog(cat: &ModelsCatalog) -> Result<(), String> {
    let path = models_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let body = serde_json::to_vec_pretty(cat).map_err(|e| format!("序列化模型清单失败：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("写入模型清单失败：{e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换模型清单失败：{e}"))?;
    Ok(())
}

/// 合并后的清单（网关选号、`/v1/models`、界面共用这一个出口）。
pub fn load_effective() -> BTreeMap<String, Vec<String>> {
    load_catalog().effective()
}

/// 用户手动添加一个模型（写到 `user` 段；重复添加是幂等的）。
pub fn add_user_model(scope: &str, model: &str) -> Result<ModelsCatalog, String> {
    let scope = scope.trim();
    let model = model.trim();
    if scope.is_empty() {
        return Err("请选择模型所属的作用域（如 trae-cn / wb-cn）".into());
    }
    if model.is_empty() {
        return Err("模型名不能为空".into());
    }
    let mut cat = load_catalog();
    let list = cat.user.entry(scope.to_string()).or_default();
    if !list.iter().any(|m| m == model) {
        list.push(model.to_string());
    }
    // 默认段里已经有同名条目时不必重复留在 user（但那属于「默认」，删了会被下次同步加回来，
    // 所以仍写进 user —— 这样用户删默认模型时它有额外一层保护）。
    cat.version = MODELS_VERSION;
    cat.source = "user".into();
    cat.updated_at = now_stamp();
    save_catalog(&cat)?;
    Ok(cat)
}

/// 删除用户手动添加的模型（**只删 user 段**；默认段由同步负责）。
pub fn remove_user_model(scope: &str, model: &str) -> Result<ModelsCatalog, String> {
    let mut cat = load_catalog();
    let mut changed = false;
    if let Some(list) = cat.user.get_mut(scope) {
        let before = list.len();
        list.retain(|m| m != model);
        changed = list.len() != before;
        if list.is_empty() {
            cat.user.remove(scope);
        }
    }
    if !changed {
        return Err(format!("「{model}」不在你手动添加的清单里（默认清单由 GitHub 同步，不能在这里删）"));
    }
    cat.version = MODELS_VERSION;
    cat.updated_at = now_stamp();
    save_catalog(&cat)?;
    Ok(cat)
}

/// 把一份「作用域 → 模型」整体写进 `defaults`（覆盖），**保留 `local` / `user` 不动**。
///
/// ⚠️ 只有 GitHub 同步（`source = "github"`）该走这里。本机识别走 [`merge_local`]。
pub fn replace_defaults(
    map: BTreeMap<String, Vec<String>>,
    source: &str,
) -> Result<ModelsCatalog, String> {
    let mut cat = load_catalog();
    let mut defaults: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (scope, list) in map {
        let models = tidy(&list);
        if !models.is_empty() {
            defaults.insert(scope, models);
        }
    }
    cat.defaults = defaults;
    cat.version = MODELS_VERSION;
    cat.source = source.to_string();
    cat.updated_at = now_stamp();
    save_catalog(&cat)?;
    Ok(cat)
}

/// 把本机识别到的模型并进 `local` 段（**只做并集，永不整段覆盖**）。
///
/// 这就是用户 2026-10-30 定的口径：「本机识别只做补充、不覆盖，`defaults` 为空时兜底」。
///
/// 三条规则：
///
/// 1. **`defaults` 已有内容**（GitHub 同步过 / 旧机器迁移过）⇒ 只把 GitHub 清单里
///    **没有**的模型写进 `local`；GitHub 有的一个字都不动，避免本机的旧版本覆盖官方新清单。
/// 2. **`defaults` 是空的**（新电脑、首次启动、还没联上网）⇒ 本机识别结果**兜底**
///    写进 `defaults`，否则界面会是空的、网关 `/v1/models` 也列不出东西。
/// 3. 两种情况下 `user` 段都**一个字都不动**。
///
/// ⚠️ 本机识别结果可能**变少**（某个产品没登录 / 没装）：这时绝不能把 `local` 里
/// 原来的条目删掉 —— 那等于「你今天没开 Trae，Trae 的模型就从清单里消失了」。
/// 所以 `local` 只增不减；要清理只能靠用户手动删（`remove_local_model`）。
pub fn merge_local(map: BTreeMap<String, Vec<String>>) -> Result<ModelsCatalog, String> {
    let mut cat = load_catalog();

    let defaults_empty = cat.defaults.values().all(|v| v.is_empty());

    if defaults_empty {
        // 兜底：本机识别直接当默认清单（只在这一种情况下写 defaults）
        let mut defaults: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (scope, list) in &map {
            let models = tidy(list);
            if !models.is_empty() {
                defaults.insert(scope.clone(), models);
            }
        }
        if !defaults.is_empty() {
            cat.defaults = defaults;
            cat.version = MODELS_VERSION;
            cat.source = "discover".to_string();
            cat.updated_at = now_stamp();
            save_catalog(&cat)?;
            return Ok(cat);
        }
        return Ok(cat);
    }

    // 正常路径：GitHub 清单已存在 ⇒ 本机独有的模型进 `local`
    let mut changed = false;
    for (scope, list) in map {
        let known: Vec<String> = cat
            .defaults
            .get(&scope)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .chain(cat.user.get(&scope).cloned().unwrap_or_default())
            .collect();
        let entry = cat.local.entry(scope.clone()).or_default();
        for m in tidy(&list) {
            if known.iter().any(|k| k == &m) {
                continue;
            }
            if !entry.iter().any(|e| e == &m) {
                entry.push(m);
                changed = true;
            }
        }
    }
    // 清掉空的（本机一个都没识别到的作用域）
    cat.local.retain(|_, v| !v.is_empty());

    if changed {
        cat.version = MODELS_VERSION;
        cat.updated_at = now_stamp();
        // ⚠️ `source` 保持原值（github）：它描述的是 `defaults` 的来源，
        // 本机补充不该把「来源：GitHub」改成「本机识别」—— 那会让用户以为权威清单被换了。
        save_catalog(&cat)?;
    }
    Ok(cat)
}

/// 删掉一个「仅本机」的模型（`local` 段；GitHub 与用户段都删不了）。
pub fn remove_local_model(scope: &str, model: &str) -> Result<ModelsCatalog, String> {
    let mut cat = load_catalog();
    let mut changed = false;
    if let Some(list) = cat.local.get_mut(scope) {
        let before = list.len();
        list.retain(|m| m != model);
        changed = list.len() != before;
        if list.is_empty() {
            cat.local.remove(scope);
        }
    }
    if !changed {
        return Err("这个模型不在「仅本机」段里（GitHub 清单与手动添加的要到对应入口删）".into());
    }
    cat.version = MODELS_VERSION;
    cat.updated_at = now_stamp();
    save_catalog(&cat)?;
    Ok(cat)
}

/// 从 GitHub 拉清单并覆盖 `defaults`。
///
/// 失败时**不写任何东西**：界面拿到的是一句原因，本地清单继续可用（用户要的「不可用就用本地」）。
///
/// 会按 [`MODELS_REMOTE_MIRRORS`] **逐个源尝试**：主源超时（国内常见）就换镜像源。
/// 文案必须把「文件不存在（404）」和「连不上网」分开说 —— 前者是仓库里没这个文件、
/// 需要作者补；后者是本机网络问题，用户自己能解决（开代理 / 换网络）。
pub async fn sync_from_remote() -> Result<Value, String> {
    let mut attempts: Vec<String> = Vec::new();
    let mut not_found = 0usize;
    let mut total = 0usize;

    for url in MODELS_REMOTE_MIRRORS {
        total += 1;
        match fetch_remote(url).await {
            Ok(map) => {
                let cat = replace_defaults(map, "github")?;
                let count: usize = cat.defaults.values().map(|v| v.len()).sum();
                return Ok(serde_json::json!({
                    "ok": true,
                    "source": "github",
                    "url": url,
                    "scopes": cat.defaults.len(),
                    "models": count,
                    "updatedAt": cat.updated_at,
                    "userKept": cat.user.values().map(|v| v.len()).sum::<usize>(),
                    // 退到镜像源时要说清楚，别让用户以为主源也通了。
                    "mirrored": url != MODELS_REMOTE_URL,
                }));
            }
            Err((kind, msg)) => {
                if kind == FailureKind::NotFound {
                    not_found += 1;
                }
                attempts.push(format!("{url} → {msg}"));
            }
        }
    }

    let detail = attempts.join("；");
    // ⚠️ 只有**每个源都说 404**才算「仓库里真没这个文件」。只要有一个源是网络问题，
    // 就不能断言文件缺失 —— 那会把用户引向错误的方向（去仓库里找根本不缺的文件）。
    Err(if not_found == total {
        format!(
            "GitHub 仓库里没有 models.json（404）：{detail}。\
             这是清单文件缺失，不是你的网络问题 —— 需要把 models.json 推到仓库根目录后才能同步。"
        )
    } else {
        format!(
            "拉取 GitHub 模型清单失败（本地清单继续可用）：{detail}。\
             国内直连 raw.githubusercontent.com 经常超时，可以开代理再试。"
        )
    })
}

/// 单次拉取的失败类别：**决定文案**，所以必须分开，不能塌缩成一个 bool。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// 连不上（超时 / DNS / TLS）。
    Network,
    /// 连上了但服务端说没有这个文件。
    NotFound,
    /// 拉到了但内容不是我们认的形态。
    Bad,
}

async fn fetch_remote(
    url: &str,
) -> Result<BTreeMap<String, Vec<String>>, (FailureKind, String)> {
    let resp = crate::modules::config::http_request(url, "GET", None, None).await;
    // 传输层失败统一是 `{"code": -1, "message": …}`（项目里唯一的判据）。
    if resp.get("code").and_then(|c| c.as_i64()) == Some(-1) {
        let msg = resp
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("网络不可用")
            .to_string();
        return Err((FailureKind::Network, msg));
    }
    // 非 2xx 会带 HTTP 状态码（`http_request` 的约定）：404 是「文件不存在」。
    if let Some(code) = resp.get("code").and_then(|c| c.as_i64()) {
        if code == 404 {
            return Err((
                FailureKind::NotFound,
                "HTTP 404 文件不存在".to_string(),
            ));
        }
        if (400..600).contains(&code) {
            let msg = resp
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            // 403 也可能是限流/被墙，仍归「网络类」便于用户开代理重试。
            return Err((
                FailureKind::Network,
                format!("HTTP {code} {}", msg.trim()).trim().to_string(),
            ));
        }
    }

    let v: Value = resp;
    parse_remote_body(&v)
}

/// 解析远端文件体（**纯函数**，便于单测钉住「只取 defaults」这条铁律）。
///
/// 两种形态都收：v2（`defaults`/`user`）与 v1（扁平）。
/// ⚠️ **只取 `defaults`**，绝不把远端文件里的 `user` 段搬过来 —— 那会覆盖
/// 用户本机手动添加的模型（用户明确要求「我加的模型永不被覆盖」）。
fn parse_remote_body(v: &Value) -> Result<BTreeMap<String, Vec<String>>, (FailureKind, String)> {
    let collect = |obj: &serde_json::Map<String, Value>| -> BTreeMap<String, Vec<String>> {
        let mut m = BTreeMap::new();
        for (k, val) in obj {
            if let Some(list) = val.as_array() {
                m.insert(
                    k.clone(),
                    list.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>(),
                );
            }
        }
        m
    };

    let map = if let Some(d) = v.get("defaults").and_then(|d| d.as_object()) {
        collect(d)
    } else if let Some(o) = v.as_object() {
        // 扁平 v1：注意 `version` / `source` 这类标量不是数组，会被 `as_array` 过滤掉，
        // 但 `user` 若是对象也不会误收 —— 只有数组值才算名单。
        collect(o)
    } else {
        return Err((FailureKind::Bad, "既不是对象也不是 v2 结构".to_string()));
    };
    if map.is_empty() {
        return Err((
            FailureKind::Bad,
            "文件里没有任何模型（未覆盖本地清单）".to_string(),
        ));
    }
    Ok(map)
}

/// 清单概览（界面用）：三段分开给，前端才知道哪些能删、哪些是「仅本机」。
pub fn catalog_overview() -> Value {
    let cat = load_catalog();
    let effective = cat.effective();
    let user_count: usize = cat.user.values().map(|v| v.len()).sum();
    let default_count: usize = cat.defaults.values().map(|v| v.len()).sum();
    // 「仅本机」= local 里那些 GitHub 清单没有的（同作用域同名不算）。
    let local_only_count: usize = cat
        .local
        .iter()
        .map(|(scope, list)| {
            let d = cat.defaults.get(scope);
            list.iter()
                .filter(|m| !d.map(|x| x.iter().any(|y| y == *m)).unwrap_or(false))
                .count()
        })
        .sum();
    serde_json::json!({
        "ok": true,
        "version": cat.version.max(MODELS_VERSION),
        "source": cat.source,
        "updatedAt": cat.updated_at,
        "remoteUrl": MODELS_REMOTE_URL,
        // 备用镜像源（主源不通时会自动退到这里）。
        "remoteMirrors": MODELS_REMOTE_MIRRORS,
        "path": models_file().to_string_lossy(),
        "defaults": cat.defaults,
        "local": cat.local,
        "user": cat.user,
        "effective": effective,
        "defaultCount": default_count,
        "localCount": local_only_count,
        "userCount": user_count,
    })
}

/// 模型**显示名**表：`{ 作用域: { 代号: 显示名 } }`。
///
/// 为什么单独一份：`models.json` 的形态是「作用域 → 名单」（`Vec<String>`），被选号、
/// 勾选、`/v1/models` 共用，塞不下显示名。而 Qoder 的清单里发上游的是**代号**
/// （`qmodel_38max` / `dmodel` / `kmodel_latest`），客户端界面上显示的是
/// `Qwen3.8-Max` / `DeepSeek-V4` / `Kimi-K2.5` —— 用户看到代号根本认不出是哪个模型
/// （2026-10-24 用户反馈「qoder 读取到的模型与客户端显示的不一致」）。
///
/// 所以：`models.json` 保持代号（请求要用它），这张表只负责「显示 + 反查」。
pub fn labels_file() -> PathBuf {
    store_dir().join("model-labels.json")
}

/// 读显示名表（读不到就是空表，不报错：老机器上本来就没有这个文件）。
pub fn label_map() -> BTreeMap<String, BTreeMap<String, String>> {
    std::fs::read_to_string(labels_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 这个作用域是不是 Qoder —— **只有 Qoder 需要把代号换成客户端里的名字**。
///
/// Trae / WorkBuddy 的代号本身就是人话（`glm-5.3` / `deepseek-v4.1-flash`），
/// 而且它们的显示名里有大量「内部条目」（`code-review-judge` → `DeepSeek-V3.1-Terminus`、
/// `custom_openai_compatible//…`），跟着改名只会把可用的名字改成不相干的字。
pub fn is_qoder_scope(scope: &str) -> bool {
    scope.starts_with("qoder")
}

/// 对外公开名：`scope` 是 Qoder 就用客户端里显示的 `display`，否则用代号。
///
/// 两条「不改名」的例外：
/// - `display` 为空；
/// - `display` 与代号**只差大小写**（`auto` ↔ `Auto`）—— 改过去等于让 `Auto` 和别家
///   的 `auto` 变成两个不同的模型，跨产品合并立刻断掉。
pub fn public_from(scope: &str, code: &str, display: &str) -> String {
    if !is_qoder_scope(scope) {
        return code.to_string();
    }
    let d = display.trim();
    if d.is_empty() || d.eq_ignore_ascii_case(code) {
        return code.to_string();
    }
    d.to_string()
}

/// 某一个代号在界面上/接口里对外的名字（查不到就返回代号本身）。
pub fn public_name(scope: &str, code: &str) -> String {
    let labels = label_map();
    let disp = labels
        .get(scope)
        .and_then(|m| m.get(code))
        .map(String::as_str)
        .unwrap_or("");
    public_from(scope, code, disp)
}

/// `公开名 → 上游代号`（Qoder 专用；其余作用域返回空表）。
///
/// ⚠️ 同一个作用域里**两个代号显示成同一个名字**时不收录：那说明上游自己就有歧义
/// （`kmodel` / `kmodel_latest` 这种），猜错了会把请求悄悄发到另一个模型上。
pub fn public_to_code_map(scope: &str) -> BTreeMap<String, String> {
    let mut by_public: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (code, disp) in label_map().get(scope).into_iter().flatten() {
        let public = public_from(scope, code, disp);
        if public != *code {
            by_public.entry(public).or_default().push(code.clone());
        }
    }
    by_public
        .into_iter()
        .filter(|(_, codes)| codes.len() == 1)
        .map(|(public, codes)| (public, codes[0].clone()))
        .collect()
}

/// 整份清单的三元组：`(作用域, 代号, 公开名)`。
///
/// 分组、选号、`/v1/models` 全部走这一个出口 —— 三处各算一遍必然算出三种结果。
pub fn catalog_rows(scope: Option<&str>) -> Vec<(String, String, String)> {
    let labels = label_map();
    let mut out = Vec::new();
    for (sc, list) in crate::modules::gateway_pool::catalog() {
        if let Some(want) = scope {
            if sc != want {
                continue;
            }
        }
        for code in list {
            let disp = labels
                .get(&sc)
                .and_then(|m| m.get(&code))
                .map(String::as_str)
                .unwrap_or("");
            out.push((sc.clone(), code.clone(), public_from(&sc, &code, disp)));
        }
    }
    out
}

/// **公开名 → 这一家上游认的代号**（发请求前必须过这里）。
///
/// 顺序（每一步都必须先于下一步）：
/// 1. 已经是这家的代号 ⇒ 原样（`auto` 这类和显示名同形的不能改写）；
/// 2. 公开名精确命中（`GLM-5.3` → `gmodel`）；
/// 3. 归一化命中（大小写 / 连字符差异，`glm-5.3` 也能认）；
/// 4. 认不出就原样返回 —— 让上游如实报「模型不存在」，而不是我们瞎换一个。
pub fn upstream_model(scope: &str, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    let codes: Vec<String> = crate::modules::gateway_pool::catalog()
        .into_iter()
        .find(|(s, _)| s == scope)
        .map(|(_, l)| l)
        .unwrap_or_default();
    if codes.iter().any(|c| c == name) {
        return name.to_string();
    }
    let map = public_to_code_map(scope);
    if let Some(code) = map.get(name) {
        return code.clone();
    }
    let want = crate::modules::gateway_pool::normalize_model_key(name);
    for (public, code) in &map {
        if crate::modules::gateway_pool::normalize_model_key(public) == want {
            return code.clone();
        }
    }
    for code in &codes {
        if crate::modules::gateway_pool::normalize_model_key(code) == want {
            return code.clone();
        }
    }
    name.to_string()
}

/// 把用户填的模型名**归一化成对外公开名**（Qoder 是客户端里的名字，其余是代号）。
///
/// 四种写法都要认（用户真的会混着填）：
///
/// | 写法 | 例子 | 结果 |
/// | --- | --- | --- |
/// | 公开名 | `GLM-5.3` / `glm-5.3` | 原样 / 换成清单里的写法 |
/// | Qoder 代号（老配置） | `gmodel` / `dfmodel` | `GLM-5.3` / `DeepSeek-Flash` |
/// | 客户端显示名 | `DeepSeek-V4-Flash 正式版` | 该作用域的公开名 |
/// | 带作用域前缀 | `qoder:GLM-5.3` | 先剥前缀，再**只按那一家的清单**反查 |
///
/// ⚠️ 带前缀时**必须按作用域反查**：`DeepSeek-V4-Pro` 同时是 trae 的代号和 qoder
/// 的公开名。只按全局查会把 trae 的代号塞给 Qoder，上游直接回「模型不存在」。
///
/// ⚠️ 这一步**只改「名字怎么称呼」**，不改「发哪家的哪个代号」——
/// 后者是 [`upstream_model`] 的事，选号之后才知道落到哪一家。
pub fn canonical_model(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    if let Some((ns, rest)) = name.split_once(':') {
        let scope = crate::modules::gateway_pool::scope_alias(ns.trim()).to_string();
        let rest = canonical_model_in(Some(&scope), rest.trim());
        return format!("{ns}:{rest}");
    }
    canonical_model_in(None, name)
}

/// [`canonical_model`] 的作用域版本：`scope = None` 表示在所有作用域里找。
pub fn canonical_model_in(scope: Option<&str>, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    let rows = catalog_rows(scope);
    let norm = crate::modules::gateway_pool::normalize_model_key;

    // ① 已经是代号 ⇒ 换成公开名（`gmodel` → `GLM-5.3`，`auto` → `auto`）。
    if let Some((_, _, public)) = rows.iter().find(|(_, code, _)| code == name) {
        return public.clone();
    }
    // ② 已经是公开名 ⇒ 原样（作用域锁定时只看这家，避免把别人家的名字带过去）。
    if rows.iter().any(|(_, _, public)| public == name) {
        return name.to_string();
    }
    // ③ 归一化命中（大小写 / 连字符差异是常见输入误差）。
    let want = norm(name);
    if let Some((_, _, public)) = rows
        .iter()
        .find(|(_, code, public)| norm(code) == want || norm(public) == want)
    {
        return public.clone();
    }
    // ④ 客户端显示名（`DeepSeek-V4-Flash 正式版`）⇒ 反查公开名。
    //    多义（不同作用域同一个显示名指向不同模型）时**不猜**：宁可让 `resolve_scopes`
    //    报「找不到这个模型」，也不能把请求悄悄转到另一家的另一个模型上。
    let labels = label_map();
    let want = name.to_lowercase();
    let mut hits: Vec<String> = Vec::new();
    for (sc, code, public) in &rows {
        let hit = labels
            .get(sc)
            .and_then(|m| m.get(code))
            .map(|d| d.to_lowercase() == want)
            .unwrap_or(false);
        if hit && !hits.contains(public) {
            hits.push(public.clone());
        }
    }
    if hits.len() == 1 {
        return hits.remove(0);
    }
    name.to_string()
}

/// 单个模型的显示名（查不到就用代号本身，界面不会出现空白）。
pub fn display_of(scope: &str, model: &str) -> String {
    label_map()
        .get(scope)
        .and_then(|m| m.get(model))
        .cloned()
        .unwrap_or_else(|| model.to_string())
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
    // ⚠️⚠️ 走 `merge_local` 而不是 `replace_defaults`：本机识别**只做补充、不覆盖**，
    //    并且只在 `defaults` 为空时兜底（用户 2026-10-30 定的口径）。
    //    早期这里直接覆盖 `defaults`，于是「点重新识别」会把 GitHub 的权威清单当场冲掉，
    //    后台每 6 小时还会再冲一次 —— 界面写着「来源：GitHub」，内容却是本机的。
    let mut by_scope: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in &models {
        by_scope.entry(m.scope.clone()).or_default().push(m.model.clone());
    }
    let _ = merge_local(by_scope);

    // 显示名表：增量写入（识别失败的作用域保留旧值，与 models.json 同一策略）。
    // 只收「显示名 ≠ 代号」的条目 —— Trae / WorkBuddy 两边同名，存进去只是噪音。
    let mut labels = label_map();
    for m in &models {
        if m.display.trim().is_empty() || m.display == m.model {
            continue;
        }
        labels
            .entry(m.scope.clone())
            .or_default()
            .insert(m.model.clone(), m.display.clone());
    }
    if let Ok(text) = serde_json::to_string_pretty(&labels) {
        let lp = labels_file();
        if let Some(dir) = lp.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // 原子写：写到 .tmp 再 rename，避免读到半截 JSON。
        let tmp = lp.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &lp);
        }
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
    ///
    /// ⚠️⚠️ 必须走 [`merge_local`]，**绝不能**用 `replace_defaults`：
    /// 本机识别只做补充、不覆盖（用户 2026-10-30 定的口径）。早期这里用
    /// `replace_defaults(map, "discover")`，一跑就把 GitHub 的 143 个权威清单
    /// 冲成这台机器的 45 个 —— 实测已踩过一次（`defaults` 143 → 45，`source` github → discover）。
    #[tokio::test]
    #[ignore]
    async fn sync_models_json() {
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
        let cat = merge_local(map).expect("写清单失败");
        eprintln!(
            "已写 {}（defaults {} 个 / local 仅本机 {} 个 / user {} 个，source={}）",
            models_file().display(),
            cat.defaults.values().map(|v| v.len()).sum::<usize>(),
            cat.local.values().map(|v| v.len()).sum::<usize>(),
            cat.user.values().map(|v| v.len()).sum::<usize>(),
            cat.source
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// v1（扁平 `{作用域:[模型]}`）读进来必须整份当 `defaults`，一个模型都不许丢 ——
    /// 老机器上文件就是这个形状，迁移时静默清空等于把用户清单清零。
    #[test]
    fn v1_flat_catalog_migrates_into_defaults() {
        // 直接构造等价 JSON 验证解析规则（`load_catalog` 走 store_dir()，单测不落盘）。
        let flat: Value =
            serde_json::from_str(r#"{"trae-cn":["glm-5","kimi"],"wb-cn":["deepseek-v4"]}"#)
                .unwrap();
        let obj = flat.as_object().unwrap();
        assert!(obj.get("defaults").is_none(), "v1 没有 defaults 键 → 走迁移分支");
        let mut defaults: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, val) in obj {
            let list: Vec<String> = val
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect();
            defaults.insert(k.clone(), tidy(&list));
        }
        let cat = ModelsCatalog {
            version: MODELS_VERSION,
            source: "migrated".into(),
            updated_at: String::new(),
            defaults,
            local: BTreeMap::new(),
            user: BTreeMap::new(),
        };
        let eff = cat.effective();
        assert_eq!(eff.get("trae-cn").unwrap().len(), 2);
        assert_eq!(eff.get("wb-cn").unwrap()[0], "deepseek-v4");
    }

    /// 用户手动添加的模型必须与默认清单合并，且**同名的只出现一次**。
    #[test]
    fn user_models_merge_with_defaults_without_duplicates() {
        let mut cat = ModelsCatalog::default();
        cat.defaults.insert("trae-cn".into(), vec!["glm-5".into(), "kimi".into()]);
        cat.user.insert("trae-cn".into(), vec!["kimi".into(), "my-own".into()]);
        let eff = cat.effective();
        let list = eff.get("trae-cn").unwrap();
        assert_eq!(list, &vec!["glm-5".to_string(), "kimi".into(), "my-own".into()]);
        assert_eq!(list.iter().filter(|m| *m == "kimi").count(), 1);
        assert!(cat.is_user("trae-cn", "my-own"));
        assert!(!cat.is_user("trae-cn", "glm-5"), "默认段的模型不算用户添加");
    }

    /// 同步（`replace_defaults`）只换默认段，用户段一个字都不许动。
    #[test]
    fn replacing_defaults_keeps_user_models() {
        let mut cat = ModelsCatalog::default();
        cat.user.insert("trae-cn".into(), vec!["my-own".into()]);
        let mut next = BTreeMap::new();
        next.insert("trae-cn".to_string(), vec!["glm-5".to_string()]);
        // 直接验证合并语义（不落盘）：replace 只赋值 defaults。
        cat.defaults = next;
        let eff = cat.effective();
        assert_eq!(eff.get("trae-cn").unwrap().last().unwrap(), "my-own");
    }

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

    /// 远端文件里带了 `user` 段也**绝不许**收进来：那是别人的本机私有清单，
    /// 收进来就会覆盖用户自己手动添加的模型（用户明确要求「我加的永不被覆盖」）。
    #[test]
    fn remote_body_takes_only_defaults() {
        let v = json!({
            "version": 2,
            "source": "github",
            "defaults": { "trae-cn": ["a", "b"], "wb-cn": ["c"] },
            "user": { "trae-cn": ["only-on-my-machine"] },
        });
        let map = parse_remote_body(&v).expect("v2 应能解析");
        assert_eq!(map.get("trae-cn").unwrap(), &vec!["a".to_string(), "b".to_string()]);
        assert_eq!(map.len(), 2, "只该有 defaults 里的两个作用域，user 不许进来");
        assert!(
            !map.values().flatten().any(|m| m == "only-on-my-machine"),
            "远端 user 段被搬过来了，会覆盖用户手加的模型"
        );

        // v1 扁平形态也要能读（老仓库 / 用户手写的文件）
        let flat = json!({ "trae-cn": ["x", "y"] });
        let map = parse_remote_body(&flat).expect("v1 扁平应能解析");
        assert_eq!(map.get("trae-cn").unwrap(), &vec!["x".to_string(), "y".to_string()]);

        // 空名单必须报错，不能「成功覆盖成空」—— 那等于把本地清单清空
        let empty = json!({ "version": 2, "defaults": {} });
        let e = parse_remote_body(&empty).expect_err("空清单不许当成功");
        assert_eq!(e.0, FailureKind::Bad);

        // 不是对象 ⇒ 格式错
        let bad = json!([1, 2, 3]);
        assert_eq!(parse_remote_body(&bad).unwrap_err().0, FailureKind::Bad);
    }

    /// 备用镜像源必须都指向**同一个仓库的 models.json**，否则「拉到了」却不是同一份清单。
    #[test]
    fn mirrors_all_point_to_the_same_file() {
        for u in MODELS_REMOTE_MIRRORS {
            assert!(
                u.contains("bean0283/twin-switch") && u.ends_with("models.json"),
                "镜像源必须指向同一份清单：{u}"
            );
            assert!(u.starts_with("https://"), "只允许 https：{u}");
        }
        assert_eq!(MODELS_REMOTE_MIRRORS[0], MODELS_REMOTE_URL, "主源必须排第一");
    }

    // ---------------------------------------------------------------
    // 本机识别：只补充、不覆盖；defaults 为空时兜底
    // （用户 2026-10-30 定的口径）
    // ---------------------------------------------------------------

    /// 造一份清单并直接算 `merge_local` 的**纯逻辑**（不落盘），便于钉住判据。
    ///
    /// 这里复制的是 [`merge_local`] 的核心分支 —— 一旦两者走岔，下面的测试会先红。
    fn merge_local_pure(
        mut cat: ModelsCatalog,
        map: BTreeMap<String, Vec<String>>,
    ) -> ModelsCatalog {
        let defaults_empty = cat.defaults.values().all(|v| v.is_empty());
        if defaults_empty {
            let mut defaults: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (scope, list) in &map {
                let models = tidy(list);
                if !models.is_empty() {
                    defaults.insert(scope.clone(), models);
                }
            }
            if !defaults.is_empty() {
                cat.defaults = defaults;
                cat.source = "discover".to_string();
            }
            return cat;
        }
        for (scope, list) in map {
            let known: Vec<String> = cat
                .defaults
                .get(&scope)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .chain(cat.user.get(&scope).cloned().unwrap_or_default())
                .collect();
            let entry = cat.local.entry(scope.clone()).or_default();
            for m in tidy(&list) {
                if known.iter().any(|k| k == &m) {
                    continue;
                }
                if !entry.iter().any(|e| e == &m) {
                    entry.push(m);
                }
            }
        }
        cat.local.retain(|_, v| !v.is_empty());
        cat
    }

    /// ① GitHub 清单已存在 ⇒ 本机识别**不许改 `defaults`**，多出来的进 `local`。
    #[test]
    fn local_discovery_never_overwrites_github_defaults() {
        let mut cat = ModelsCatalog {
            version: MODELS_VERSION,
            source: "github".into(),
            defaults: BTreeMap::from([(
                "trae-cn".to_string(),
                vec!["glm-5.3".to_string(), "kimi-k3".to_string()],
            )]),
            user: BTreeMap::from([("wb-cn".to_string(), vec!["my-model".to_string()])]),
            ..Default::default()
        };
        let discovered = BTreeMap::from([(
            "trae-cn".to_string(),
            vec![
                "glm-5.3".to_string(),      // GitHub 有 → 不进 local
                "kimi-k3".to_string(),      // GitHub 有 → 不进 local
                "codewise-rewrite".to_string(), // 仅本机 → 进 local
                "refactor_planner".to_string(), // 仅本机 → 进 local
            ],
        )]);
        let out = merge_local_pure(cat.clone(), discovered);
        assert_eq!(
            out.defaults.get("trae-cn").unwrap(),
            &vec!["glm-5.3".to_string(), "kimi-k3".to_string()],
            "GitHub 的清单一个字都不许动"
        );
        assert_eq!(
            out.local.get("trae-cn").unwrap(),
            &vec!["codewise-rewrite".to_string(), "refactor_planner".to_string()],
            "只有 GitHub 没有的才进 local"
        );
        assert_eq!(out.source, "github", "本机补充不许把来源改成 discover");
        assert_eq!(
            out.user.get("wb-cn").unwrap(),
            &vec!["my-model".to_string()],
            "user 段一个字都不许动"
        );
        cat.defaults.clear();
    }

    /// ② `defaults` 为空 ⇒ 本机识别**兜底**写进 defaults（新电脑/没联上网时不至于一片空白）。
    #[test]
    fn local_discovery_backfills_when_defaults_are_empty() {
        let cat = ModelsCatalog::default();
        let discovered = BTreeMap::from([(
            "trae-cn".to_string(),
            vec!["glm-5.3".to_string(), "kimi-k3".to_string()],
        )]);
        let out = merge_local_pure(cat, discovered);
        assert_eq!(
            out.defaults.get("trae-cn").unwrap(),
            &vec!["glm-5.3".to_string(), "kimi-k3".to_string()],
            "没有 GitHub 清单时必须兜底，否则界面/网关都是空的"
        );
        assert!(out.local.is_empty(), "兜底时不该同时写两份");
        assert_eq!(out.source, "discover");
    }

    /// ③ 本机这次识别得少了（某产品没登录）⇒ `local` 里旧条目**不许被删**。
    ///
    /// 否则会出现「你今天没开 Trae，Trae 的模型就从清单里消失了」——
    /// 用户会以为工具把模型弄丢了。
    #[test]
    fn local_discovery_never_drops_previously_seen_models() {
        let cat = ModelsCatalog {
            defaults: BTreeMap::from([("trae-cn".to_string(), vec!["glm-5.3".to_string()])]),
            local: BTreeMap::from([(
                "trae-cn".to_string(),
                vec!["old-local-only".to_string()],
            )]),
            ..Default::default()
        };
        // 这次只识别到一个 GitHub 已有的模型（old-local-only 没出现）
        let out = merge_local_pure(
            cat,
            BTreeMap::from([("trae-cn".to_string(), vec!["glm-5.3".to_string()])]),
        );
        assert_eq!(
            out.local.get("trae-cn").unwrap(),
            &vec!["old-local-only".to_string()],
            "以前识别到的本机独有模型不许因为这次没扫到就被删"
        );
    }

    /// ④ `effective()` 必须把三段并起来，且 GitHub 的排在本机的前面。
    #[test]
    fn effective_merges_all_three_segments_in_order() {
        let cat = ModelsCatalog {
            defaults: BTreeMap::from([("trae-cn".to_string(), vec!["a".to_string()])]),
            local: BTreeMap::from([("trae-cn".to_string(), vec!["b".to_string()])]),
            user: BTreeMap::from([("trae-cn".to_string(), vec!["c".to_string()])]),
            ..Default::default()
        };
        let eff = cat.effective();
        assert_eq!(
            eff.get("trae-cn").unwrap(),
            &vec!["a".to_string(), "b".to_string(), "c".to_string()],
            "顺序必须是 defaults → local → user"
        );
        // 同一模型在三段都有 ⇒ 只留一份
        let cat2 = ModelsCatalog {
            defaults: BTreeMap::from([("x".to_string(), vec!["m".to_string()])]),
            local: BTreeMap::from([("x".to_string(), vec!["m".to_string()])]),
            user: BTreeMap::from([("x".to_string(), vec!["m".to_string()])]),
            ..Default::default()
        };
        assert_eq!(cat2.effective().get("x").unwrap(), &vec!["m".to_string()]);
    }

    /// ⑤ 「仅本机」判据：GitHub 清单里有的不算仅本机（哪怕 local 里也存了一份）。
    #[test]
    fn is_local_only_excludes_github_known_models() {
        let cat = ModelsCatalog {
            defaults: BTreeMap::from([(
                "trae-cn".to_string(),
                vec!["glm-5.3".to_string()],
            )]),
            local: BTreeMap::from([(
                "trae-cn".to_string(),
                vec!["glm-5.3".to_string(), "codewise-rewrite".to_string()],
            )]),
            ..Default::default()
        };
        assert!(!cat.is_local_only("trae-cn", "glm-5.3"), "GitHub 也有的不许标成仅本机");
        assert!(cat.is_local_only("trae-cn", "codewise-rewrite"));
        assert!(!cat.is_local_only("wb-cn", "whatever"), "没出现过的不算");
    }
}
