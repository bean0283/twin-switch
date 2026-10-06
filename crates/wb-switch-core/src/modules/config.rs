//! 常量、路径与通用工具函数

use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 官网套餐页桌面 Chrome UA（plans-usage 捕获）。
pub const DEFAULT_HTTP_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// 覆盖家目录的环境变量名（**新名**，v0.0.24 起）。
///
/// 设置后 `home_dir()` 返回它的值，于是 `~/.twin-switch`、`~/.codebuddy`、
/// `~/.codebuddy-rotate` 全部落在指定目录下。用途：
/// - 集成测试把家目录沙箱化，避免读写真实账号与 CLI 配置；
/// - 自定义部署位置。
///
/// 未设置时行为与之前完全一致（`dirs::home_dir()`）。
pub const HOME_ENV_VAR: &str = "TWIN_SWITCH_HOME";

/// 改名前的环境变量名，**仍然接受**（老脚本 / 老文档里写的是它）。
///
/// 只在 [`HOME_ENV_VAR`] 未设置时才看它 —— 新名优先，同时不打断已有用法。
pub const HOME_ENV_VAR_LEGACY: &str = "TRAE_SWITCH_HOME";

/// 本工具的数据目录名（用户资产：账号库、回收站、备份、解密库快照）。
pub const STORE_DIR_NAME: &str = ".twin-switch";

/// 改名前的数据目录名（v0.0.2x 之前叫 `trae-switch-cn`）。
///
/// 老用户机器上只会有这一个目录，里面压着几个 GB 的真数据（解密库 626 MB、
/// 回收站 940 MB、导入备份 318 MB…）。所以它只用于**一次性搬迁**，任何写路径都不许再落到它上面。
pub const STORE_DIR_NAME_LEGACY: &str = ".trae-switch-cn";

/// 一次搬迁尝试失败后的重试间隔（毫秒）。Windows 上杀软 / 索引器会短暂占住句柄，
/// 等一两百毫秒重试通常就过了。
const RENAME_RETRY_DELAYS_MS: [u64; 4] = [120, 240, 480, 960];

/// 从两个环境变量的值里挑出家目录覆盖（**纯函数**，便于测试）。
///
/// 新名优先；旧名兜底；空串一律视为「未设置」。返回 `None` = 没被覆盖，
/// 调用方应回落 `dirs::home_dir()`。
fn home_override(primary: Option<OsString>, legacy: Option<OsString>) -> Option<PathBuf> {
    for candidate in [primary, legacy] {
        if let Some(v) = candidate {
            if !v.is_empty() {
                return Some(PathBuf::from(v));
            }
        }
    }
    None
}

pub fn home_dir() -> PathBuf {
    // 新名优先，旧名兜底（改名兼容，见 HOME_ENV_VAR_LEGACY）。
    // 用 `var_os` 而不是 `var`：家目录可能是非 UTF-8 路径，`var` 会直接报错丢掉它。
    home_override(
        std::env::var_os(HOME_ENV_VAR),
        std::env::var_os(HOME_ENV_VAR_LEGACY),
    )
    .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
}

/// 数据目录解析结果（进程内缓存）。缓存 `home` 一起存，这样 [`HOME_ENV_VAR`]
/// 被改掉时（集成测试会这么做）能自动重新解析，而不是返回上一次的结果。
static STORE_DIR_CACHE: std::sync::Mutex<Option<(PathBuf, PathBuf)>> = std::sync::Mutex::new(None);

/// 数据目录搬迁过程中留下的提示（`None` = 一切正常）。
///
/// 目前只有一种情况会写它：**旧目录改名失败**，于是本次运行退回用旧目录。
/// 界面/诊断可以用它如实告诉用户「数据这次没搬过去、原因是什么」，而不是静默降级。
static MIGRATION_NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 本工具的数据目录（`~/.twin-switch`）。
///
/// **首次调用会顺带把旧的 `~/.trae-switch-cn` 搬过来**（同盘 `rename`，元数据操作，
/// 即使目录有 2 GB 也是瞬时的）。所以所有路径都必须走这里，不要在别处拼目录名。
pub fn store_dir() -> PathBuf {
    let home = home_dir();
    if let Ok(mut guard) = STORE_DIR_CACHE.lock() {
        if let Some((cached_home, dir)) = guard.as_ref() {
            if *cached_home == home {
                return dir.clone();
            }
        }
        let (dir, note) = resolve_store_dir(&home);
        if let Some(n) = note {
            set_migration_note(n);
        }
        *guard = Some((home, dir.clone()));
        return dir;
    }
    let (dir, note) = resolve_store_dir(&home);
    if let Some(n) = note {
        set_migration_note(n);
    }
    dir
}

/// 提前触发数据目录解析与搬迁，并返回 `(目录, 搬迁提示)`。
///
/// Tauri 启动时在**任何数据库被打开之前**显式调一次：一是让「搬迁」这件事发生在
/// 确定的时间点上（便于日志与排障），二是把提示拿给界面用。
pub fn ensure_store_dir_ready() -> (PathBuf, Option<String>) {
    let dir = store_dir();
    let note = MIGRATION_NOTE.lock().ok().and_then(|g| g.clone());
    (dir, note)
}

/// 解析数据目录，必要时把旧目录搬过来。
///
/// 返回 `(最终使用的目录, 需要告诉用户的提示)`。提示目前只会在两种「没按预期走」的情况下出现：
/// 旧目录改名失败（退回旧目录），或新目录已就绪但旧目录仍在（可能还有旧实例在跑）。
///
/// 三条分支：
/// 1. 新目录已存在 ⇒ 直接用（已搬过 / 全新安装跑过一次），**顺便看一下旧目录还在不在**；
/// 2. 新旧都不存在 ⇒ 全新安装，建新目录；
/// 3. 只有旧目录 ⇒ 尝试 `rename`；失败则**本会话继续用旧目录**。
fn resolve_store_dir(home: &Path) -> (PathBuf, Option<String>) {
    let new_dir = home.join(STORE_DIR_NAME);
    let old_dir = home.join(STORE_DIR_NAME_LEGACY);

    if new_dir.exists() {
        // 旧目录还在，通常是「旧版本实例没关就启动了新版本」——旧实例会继续往旧目录写。
        // 不自动删（那是用户数据），但把话说清楚。
        if old_dir.exists() {
            return (
                new_dir.clone(),
                Some(format!(
                    "检测到旧数据目录仍然存在：{}\n\
                     新版本已经在用 {}，旧目录里的内容是**迁移之前**的副本，不再被读写。\n\
                     确认没有旧版本实例在运行、且账号与回收站数据都在新目录里之后，\
                     可以手动删除旧目录以释放空间。",
                    old_dir.display(),
                    new_dir.display()
                )),
            );
        }
        return (new_dir, None);
    }

    if !old_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&new_dir) {
            eprintln!("[store] mkdir {:?} FAILED: {e}", new_dir);
        }
        return (new_dir, None);
    }

    // 关键一步：同盘目录改名。**不复制、不全量读取** —— 目录里可能压着几个 GB。
    match rename_with_retry(&old_dir, &new_dir) {
        Ok(()) => {
            eprintln!(
                "[store] 数据目录已迁移：{} → {}",
                old_dir.display(),
                new_dir.display()
            );
            (new_dir, None)
        }
        Err(e) => {
            // 绝不静默失败、绝不复制、绝不删除：退回旧目录继续用，下次启动自然重试。
            // （升级时 `trae/decrypted/*.db`、`workbuddy_scan/*.db` 可能被别的东西占着。）
            let note = format!(
                "数据目录迁移失败，本次仍在使用旧目录 {}。\n原因：{e}\n\
                 这通常是杀毒软件 / 索引器 / 另一个还在运行的实例临时占用了里面的文件。\n\
                 关掉这些之后重启本程序会自动重试；期间数据一切照旧，不会丢也不会被复制。",
                old_dir.display()
            );
            eprintln!("[store] {note}");
            (old_dir, Some(note))
        }
    }
}

/// 带退避重试的目录改名。返回最后一次的失败原因。
fn rename_with_retry(from: &Path, to: &Path) -> Result<(), String> {
    let mut last = String::new();
    for (i, delay) in std::iter::once(0u64)
        .chain(RENAME_RETRY_DELAYS_MS)
        .enumerate()
    {
        if delay > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay));
        }
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                eprintln!("[store] rename 第 {} 次失败: {e}", i + 1);
            }
        }
    }
    Err(last)
}

fn set_migration_note(note: String) {
    if let Ok(mut guard) = MIGRATION_NOTE.lock() {
        // 只留第一条 —— 后面的都只是重复报同一个问题。
        if guard.is_none() {
            *guard = Some(note);
        }
    }
}

/// 本工具的缓存目录（首屏加速用，随时可以整目录删掉，删了只是变慢不会出错）。
///
/// 与 [`store_dir`] 的关系：`store_dir` 存的是**用户资产**（账号库、回收站、解密快照元信息），
/// 这里存的只是**可再生的派生数据**（总览快照、积分结果）。分开是为了让「清缓存」能一键完成。
pub fn cache_dir() -> PathBuf {
    store_dir().join("cache")
}

/// 写一份 JSON 缓存（自动建目录；失败只打印，不影响主流程）。
pub fn write_cache_json(name: &str, value: &Value) -> Option<PathBuf> {
    let dir = cache_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[cache] mkdir {:?} FAILED: {e}", dir);
        return None;
    }
    let path = dir.join(name);
    let text = serde_json::to_string(value).ok()?;
    if let Err(e) = atomic_write(&path, &text) {
        eprintln!("[cache] write {:?} FAILED: {e}", path);
        return None;
    }
    Some(path)
}

/// 读一份 JSON 缓存；不存在或解析失败都返回 `None`（调用方回落到实时计算）。
pub fn read_cache_json(name: &str) -> Option<Value> {
    let path = cache_dir().join(name);
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<Value>(&text).ok()
}

/// 删掉一份缓存（清理动作做完后调用，避免界面继续显示过期结果）。
pub fn clear_cache_json(name: &str) {
    let path = cache_dir().join(name);
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// 带版本与 TTL 的缓存槽
// ---------------------------------------------------------------------------

/// 磁盘缓存文档的形状：`{ version, scannedAt, payload }`。
///
/// `version` 一改就视同没有缓存 —— 结构变了以后前端绝不会读到半新半旧的对象。
/// 之所以把两个字段放进文档而不是依赖文件 mtime：文件复制 / 云同步都会改 mtime。
pub fn write_cache_slot(name: &str, version: u64, payload: &Value) -> Option<PathBuf> {
    let doc = json!({
        "version": version,
        "scannedAt": now_ms(),
        "payload": payload,
    });
    write_cache_json(name, &doc)
}

/// 读缓存槽。以下任一情况返回 `None`（调用方回落到实时计算）：
/// 文件不存在 / 版本不符 / 超时 [`ttl_ms`] / 结构损坏 / `payload` 不是对象。
///
/// 命中时返回 `(payload, ageMs)`，`ageMs` 供前端显示「缓存 · N 分钟前」。
pub fn read_cache_slot(name: &str, version: u64, ttl_ms: i64) -> Option<(Value, i64)> {
    let doc = read_cache_json(name)?;
    if doc.get("version").and_then(Value::as_u64) != Some(version) {
        return None;
    }
    let at = doc.get("scannedAt").and_then(Value::as_i64)?;
    let age = (now_ms() - at).max(0);
    if age > ttl_ms {
        return None;
    }
    let payload = doc.get("payload")?.clone();
    payload.is_object().then_some((payload, age))
}

/// 读缓存槽，但**不因超时而丢弃**：只要版本对、结构没坏就把 `payload` 交出来，
/// 同时用 `stale` 告诉调用方「这份数据已经比 `ttl_ms` 旧了」。
///
/// 为什么不合并进 [`read_cache_slot`]：那个是「超时就当没有，回落到实时计算」的语义，
/// 总览 / 积分接口依赖它保证数字新鲜；而**清理页**要的恰好相反 ——
/// 宁可显示一份昨天的扫描结果（并标注「N 分钟前」），也不要用户每点一次页面就等一次
/// 几 GB 的递归遍历。两种诉求没法塞进一个签名，所以并列两个函数。
///
/// 返回值：`(payload, ageMs, stale)`。
pub fn read_cache_slot_stale_ok(name: &str, version: u64, ttl_ms: i64) -> Option<(Value, i64, bool)> {
    let doc = read_cache_json(name)?;
    if doc.get("version").and_then(Value::as_u64) != Some(version) {
        return None;
    }
    let at = doc.get("scannedAt").and_then(Value::as_i64)?;
    let age = (now_ms() - at).max(0);
    let payload = doc.get("payload")?.clone();
    payload
        .is_object()
        .then_some((payload, age, age > ttl_ms))
}

// ---------------------------------------------------------------------------
// 时间
// ---------------------------------------------------------------------------

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 文件
// ---------------------------------------------------------------------------

/// 原子写文件（临时文件 + rename）。
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{file_name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    if let Err(e) = std::fs::write(&tmp, content) {
        eprintln!("[atomic] write tmp FAILED: {e}");
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        eprintln!("[atomic] rename FAILED: {e}");
        // rename 失败时清理临时文件，避免在目标目录残留 `<name>.tmp-*`。
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP 客户端
// ---------------------------------------------------------------------------

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(DEFAULT_HTTP_USER_AGENT)
}

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        http_client_builder()
            .build()
            .expect("failed to build reqwest client")
    })
}

/// 通用 HTTP 请求，返回解析后的 JSON。
///
/// 行为对齐 Python 版：
/// - 2xx：解析 body 为 JSON；
/// - HTTP 错误：body 可解析则返回其 JSON，否则 `{"code": <status>, "message": <body 前 500 字符>}`；
/// - 网络错误：`{"code": -1, "message": <原因>}`。
pub async fn http_request(
    url: &str,
    method: &str,
    body: Option<Value>,
    headers: Option<&HashMap<String, String>>,
) -> Value {
    http_request_with_proxy(url, method, body, headers, None).await
}

/// 通用 HTTP 请求，可为单次请求显式指定 HTTP/HTTPS 代理。
pub async fn http_request_with_proxy(
    url: &str,
    method: &str,
    body: Option<Value>,
    headers: Option<&HashMap<String, String>>,
    proxy: Option<&str>,
) -> Value {
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = match proxy.map(str::trim).filter(|value| !value.is_empty()) {
        Some(proxy) => match http_client_builder()
            .proxy(match reqwest::Proxy::all(proxy) {
                Ok(proxy) => proxy,
                Err(e) => return json!({"code": -1, "message": format!("代理地址无效: {e}")}),
            })
            .build()
        {
            Ok(client) => client,
            Err(e) => return json!({"code": -1, "message": format!("代理客户端创建失败: {e}")}),
        },
        None => http_client().clone(),
    };
    let mut req = client.request(method, url);
    req = req.header("Content-Type", "application/json");
    if let Some(h) = headers {
        for (k, v) in h {
            req = req.header(k, v);
        }
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                serde_json::from_str(&text).unwrap_or(Value::Null)
            } else {
                serde_json::from_str(&text).unwrap_or_else(|_| {
                    json!({
                        "code": status.as_u16(),
                        "message": normalize_error_body(&text),
                    })
                })
            }
        }
        Err(e) => json!({"code": -1, "message": e.to_string()}),
    }
}

/// 非 JSON 错误响应体归一化：网关（openresty / APISIX 等）的 401/5xx 常返回
/// 整页 HTML，原样截断会把 `<html>…` 整段塞进通知与界面卡片（issue #94）。
/// HTML 提取 `<title>` 作为可读信息；其余保持原有的 500 字符截断。
fn normalize_error_body(text: &str) -> String {
    if text.trim_start().starts_with('<') {
        let title = text
            .split_once("<title>")
            .and_then(|(_, rest)| rest.split_once("</title>"))
            .map(|(title, _)| title.trim())
            .unwrap_or_default();
        return if title.is_empty() {
            "服务端返回 HTML 错误页（无标题）".to_string()
        } else {
            format!("服务端返回 HTML 错误页：{title}")
        };
    }
    text.chars().take(500).collect::<String>()
}

/// 通用 HTTP 请求，返回原始响应（状态码 + 响应头 + 响应体），可选是否跟随重定向。
///
/// 供需要读取响应头（如 302 的 `Location`）或自行处理非 JSON 响应的场景使用；
/// 其余场景优先用 [`http_request_with_proxy`]。失败（网络错误 / 代理配置错误）
/// 返回 `(0, HashMap::new(), 错误信息)`，由调用方根据 status 判断。
pub async fn http_request_raw(
    url: &str,
    method: &str,
    body: Option<Value>,
    headers: Option<&HashMap<String, String>>,
    proxy: Option<&str>,
    follow_redirects: bool,
) -> (u16, HashMap<String, String>, String) {
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = match proxy.map(str::trim).filter(|value| !value.is_empty()) {
        Some(proxy) => {
            let mut builder = http_client_builder().proxy(match reqwest::Proxy::all(proxy) {
                Ok(proxy) => proxy,
                Err(e) => return (0, HashMap::new(), format!("代理地址无效: {e}")),
            });
            if !follow_redirects {
                builder = builder.redirect(reqwest::redirect::Policy::none());
            }
            match builder.build() {
                Ok(client) => client,
                Err(e) => return (0, HashMap::new(), format!("代理客户端创建失败: {e}")),
            }
        }
        None => {
            if follow_redirects {
                http_client().clone()
            } else {
                match http_client_builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                {
                    Ok(client) => client,
                    Err(e) => return (0, HashMap::new(), format!("客户端创建失败: {e}")),
                }
            }
        }
    };
    let mut req = client.request(method, url);
    req = req.header("Content-Type", "application/json");
    if let Some(h) = headers {
        for (k, v) in h {
            req = req.header(k, v);
        }
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let mut resp_headers = HashMap::new();
            for (k, v) in resp.headers() {
                if let Ok(vs) = v.to_str() {
                    resp_headers.insert(k.as_str().to_string(), vs.to_string());
                }
            }
            let text = resp.text().await.unwrap_or_default();
            (status, resp_headers, text)
        }
        Err(e) => (0, HashMap::new(), e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个一次性的假家目录（不碰真实 `~`）。
    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "twin-switch-cfg-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("建临时家目录");
        dir
    }

    /// 改名兼容：`TWIN_SWITCH_HOME` 优先，老的 `TRAE_SWITCH_HOME` 仍然认。
    ///
    /// 断言纯函数而不去动进程环境变量 —— `set_var` 是进程全局的，测试并行跑会互相
    /// 踩（本模块其它用例也依赖家目录），这是踩过的坑。
    #[test]
    fn home_override_prefers_new_env_var_and_accepts_legacy() {
        let os = |s: &str| Some(OsString::from(s));

        assert_eq!(
            home_override(os("/new"), os("/old")),
            Some(PathBuf::from("/new")),
            "新名设置时必须优先"
        );
        assert_eq!(
            home_override(None, os("/old")),
            Some(PathBuf::from("/old")),
            "只设旧名时仍要认（老脚本 / 老文档）"
        );
        assert_eq!(
            home_override(os(""), os("/old")),
            Some(PathBuf::from("/old")),
            "空串视为未设置，应继续往下看旧名"
        );
        assert_eq!(home_override(os(""), os("")), None, "都是空串 = 未覆盖");
        assert_eq!(home_override(None, None), None, "都没设 = 走真实家目录");
    }

    /// 全新安装：新旧目录都不存在 ⇒ 用新目录，并把它建出来。
    #[test]
    fn fresh_install_uses_and_creates_new_store_dir() {
        let home = temp_home("fresh");
        let (dir, note) = resolve_store_dir(&home);

        assert_eq!(dir, home.join(STORE_DIR_NAME));
        assert!(dir.is_dir(), "新目录应被创建");
        assert!(!home.join(STORE_DIR_NAME_LEGACY).exists(), "不该顺手建旧目录");
        assert!(note.is_none(), "顺利路径不该产生提示");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// 老用户升级：只有旧目录 ⇒ 同盘改名搬过去，内容原样保留、旧目录消失。
    #[test]
    fn legacy_dir_is_renamed_with_contents_intact() {
        let home = temp_home("legacy");
        let old = home.join(STORE_DIR_NAME_LEGACY);
        std::fs::create_dir_all(old.join("trae/decrypted")).expect("造旧目录结构");
        std::fs::write(old.join("workbuddy-accounts.json"), "{\"a\":1}").expect("写账号库");
        std::fs::write(old.join("trae/decrypted/solo-cn.db"), b"DUMMY").expect("写解密库");

        let (dir, note) = resolve_store_dir(&home);

        assert_eq!(dir, home.join(STORE_DIR_NAME));
        assert!(note.is_none(), "改名成功不该产生提示");
        assert!(!old.exists(), "旧目录必须已不存在（是改名，不是复制）");
        assert_eq!(
            std::fs::read_to_string(dir.join("workbuddy-accounts.json")).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(std::fs::read(dir.join("trae/decrypted/solo-cn.db")).unwrap(), b"DUMMY");

        // 幂等：再解析一次同样是新目录，且不再有提示。
        let (again, note2) = resolve_store_dir(&home);
        assert_eq!(again, home.join(STORE_DIR_NAME));
        assert!(note2.is_none());

        let _ = std::fs::remove_dir_all(&home);
    }

    /// 新目录已就绪、旧目录仍在（典型的「旧实例没关就启动了新版本」）：
    /// 必须用新目录，并把「旧目录还在」如实报出来，**绝不能自动删**。
    #[test]
    fn leftover_legacy_dir_is_reported_not_deleted() {
        let home = temp_home("leftover");
        let old = home.join(STORE_DIR_NAME_LEGACY);
        let new = home.join(STORE_DIR_NAME);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("keep.json"), "old").unwrap();
        std::fs::write(new.join("keep.json"), "new").unwrap();

        let (dir, note) = resolve_store_dir(&home);

        assert_eq!(dir, new, "新目录已存在时它优先");
        assert!(note.is_some(), "必须提示旧目录还在");
        assert!(old.exists(), "旧目录是用户数据，绝不能自动删");
        assert_eq!(std::fs::read_to_string(old.join("keep.json")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(new.join("keep.json")).unwrap(), "new");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// 回归：清理页要的语义是「缓存多旧都先拿来用」，不是「过期就重扫」。
    ///
    /// 两条读法必须**并存且互不影响**：`read_cache_slot` 超时返回 `None`（总览/积分
    /// 依赖它保证数字新鲜），`read_cache_slot_stale_ok` 超时照样把 payload 交出来、
    /// 只把 `stale` 置真（清理页依赖它保证进页面毫秒级）。
    #[test]
    fn stale_ok_read_keeps_expired_payload() {
        let name = "__test_stale_ok.json";
        // 手工写一份「1 小时前」的缓存槽，绕过 write_cache_slot 的 now_ms。
        let doc = json!({
            "version": 7u64,
            "scannedAt": now_ms() - 3_600_000,
            "payload": { "hello": "world" },
        });
        write_cache_json(name, &doc);

        // TTL 10 分钟 ⇒ 这份数据已经过期 50 分钟。
        assert!(
            read_cache_slot(name, 7, 600_000).is_none(),
            "带 TTL 的读法必须把过期缓存判为「没有」"
        );

        let (payload, age, stale) =
            read_cache_slot_stale_ok(name, 7, 600_000).expect("过期也要能读到");
        assert_eq!(payload.get("hello").and_then(Value::as_str), Some("world"));
        assert!(stale, "超过 TTL 必须标 stale");
        assert!(age >= 3_600_000, "age 应约为构造时的 1 小时，实际 {age}");

        // 版本不符仍然一律作废 —— 「有缓存就用」不能变成「读到什么用什么」。
        assert!(read_cache_slot_stale_ok(name, 8, 600_000).is_none());

        clear_cache_json(name);
    }

    /// 回归 issue #94：网关 401 返回的整页 HTML 要归一化为可读信息，
    /// 不能把 `<html>…` 原样塞进通知与界面卡片。
    #[test]
    fn normalize_error_body_extracts_html_title() {
        let html = "<html>\n<head><title>401 Authorization Required</title></head>\n\
                    <body>\n<center><h1>401 Authorization Required</h1></center>\n\
                    <hr><center>openresty</center>\n</body>\n</html>\n";
        assert_eq!(
            normalize_error_body(html),
            "服务端返回 HTML 错误页：401 Authorization Required"
        );

        assert_eq!(
            normalize_error_body("<!DOCTYPE html><html><body>boom</body></html>"),
            "服务端返回 HTML 错误页（无标题）"
        );

        // 非 HTML 错误体保持原有截断行为。
        let plain = "plain gateway error";
        assert_eq!(normalize_error_body(plain), plain);
        let long = "x".repeat(600);
        assert_eq!(normalize_error_body(&long).chars().count(), 500);
    }

    #[test]
    fn default_http_user_agent_matches_official_chrome_desktop() {
        assert_eq!(
            DEFAULT_HTTP_USER_AGENT,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36"
        );
        let _ = http_client_builder();
    }
}
