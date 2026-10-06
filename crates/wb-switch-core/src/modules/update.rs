//! 自动更新：检查公开 GitHub Releases 上的新版本。
//!
//! 版本检查**刻意不走 GitHub API**（`api.github.com` 对未认证请求限 60 次/小时/IP，
//! 一次误循环就会把配额打光），改用两条都不吃配额的路子：
//!
//! 1. **主端点**：release 资产里的 updater manifest（`latest.json`）。它挂在
//!    `releases/latest/download/` 下，就是普通的资产下载，不计 API 配额；
//! 2. **兜底端点**：请求 `/releases/latest` 但**不跟随重定向**，从 302 的
//!    `Location` 头（形如 `.../releases/tag/v0.2.0`）里解析 tag。
//!
//! 成功结果进程级缓存 6 小时（失败**不**缓存，否则一次网络抖动会冻住半小时）；
//! 用户手动点「检查更新」传 `force=true` 绕过缓存。
//!
//! 下载与安装**不在这里**：走 `tauri-plugin-updater` 的 Rust API（签名校验在
//! `Update::download` 内部完成），见 `src-tauri/src/update_service.rs`。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::modules::config::{
    atomic_write, http_request_raw, http_request_with_proxy, now_ms, store_dir,
};

/// 应用当前版本（来自 `Cargo.toml` 的 `package.version`）。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GITHUB_OWNER: &str = "bean0283";
pub const GITHUB_REPO: &str = "twin-switch";

/// 成功结果缓存有效期（6 小时）。后台周期检查（30 分钟一轮）多数轮次命中缓存、
/// 不发网络请求；设置页 / 界面上手动点「检查更新」传 `force=true` 强制刷新。
const CACHE_TTL_SECS: i64 = 6 * 60 * 60;

/// 进程级内存缓存。**只缓存 `ok=true` 的结果**，失败不写缓存。
struct CachedCheck {
    checked_at: i64,
    value: Value,
}

static CACHE: Mutex<Option<CachedCheck>> = Mutex::new(None);

pub fn now_secs() -> i64 {
    now_ms() / 1000
}

/// 更新相关配置的落盘位置（`~/.twin-switch/update.json`）。
///
/// 目前只认一个字段 `proxy`：GitHub 在部分网络环境下直连不通时，
/// 手写 `{"proxy": "http://127.0.0.1:7890"}` 即可让检查与下载都走代理。
/// 文件不存在 = 直连，属正常情况。
pub fn update_config_file() -> PathBuf {
    store_dir().join("update.json")
}

/// 读取更新配置。**永不返回 token**：这是公开仓库，不需要也不需要凭据。
pub fn load_update_config() -> Value {
    let mut proxy = String::new();
    let f = update_config_file();
    if f.exists() {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(value) = v.get("proxy").and_then(Value::as_str) {
                    proxy = value.trim().to_string();
                }
            }
        }
    }
    json!({ "owner": GITHUB_OWNER, "repo": GITHUB_REPO, "proxy": proxy })
}

/// 保存更新配置（界面暂未暴露入口，留给「网络设置」用）。
pub fn save_update_config(cfg: &Value) -> std::io::Result<()> {
    let proxy = cfg
        .get("proxy")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let clean = json!({ "owner": GITHUB_OWNER, "repo": GITHUB_REPO, "proxy": proxy });
    std::fs::create_dir_all(store_dir())?;
    atomic_write(
        &update_config_file(),
        &serde_json::to_string_pretty(&clean).unwrap_or_default(),
    )
}

fn version_tuple(v: &str) -> Vec<i64> {
    // 先截掉预发布 / 构建后缀：`1.2.3-beta.1` → `1.2.3`。
    //
    // 不截会出事：按 `.` 切之后 `-beta` 段虽然被丢弃，但它后面的 `.1` 会**补位成第三段**，
    // 于是 `0.2.0-beta.1` 被算成 `[0,2,1]` —— 反而比正式的 `0.2.0` 还大。
    // 本项目只发正式版，后缀一律不参与比较。
    let core = v.trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    core.split('.').filter_map(|x| x.parse::<i64>().ok()).collect()
}

/// 版本比较：`a > b` 返回 1，`a < b` 返回 -1，相等返回 0。
///
/// 按点分段转整数比较，段数不齐时缺的段按 0 补齐（`0.2` == `0.2.0`）；
/// 预发布 / 构建后缀整个截掉（`0.2.0-beta.1` == `0.2.0`）。
pub fn compare_versions(a: &str, b: &str) -> i64 {
    let ta = version_tuple(a);
    let tb = version_tuple(b);
    for i in 0..ta.len().max(tb.len()) {
        let x = ta.get(i).copied().unwrap_or(0);
        let y = tb.get(i).copied().unwrap_or(0);
        if x != y {
            return if x > y { 1 } else { -1 };
        }
    }
    0
}

/// updater manifest 的候选 URL（按优先级）。
///
/// 本项目的 manifest 只有合并版 `latest.json` 一份（Windows 单平台分发），
/// 但保留平台专名候选：将来若要分平台发，不必改客户端代码。
pub fn updater_manifest_urls(owner: &str, repo: &str, os: &str, arch: &str) -> Vec<String> {
    let os_slug = match os {
        "macos" | "darwin" => "macos",
        other => other,
    };
    let mut urls = vec![format!(
        "https://github.com/{owner}/{repo}/releases/latest/download/latest.json"
    )];
    urls.push(format!(
        "https://github.com/{owner}/{repo}/releases/latest/download/latest-{os_slug}-{arch}.json"
    ));
    urls.dedup();
    urls
}

/// 主端点：拉取 updater manifest。成功返回解析后的 JSON（含 `version` / `pub_date`）。
async fn fetch_manifest_version(
    owner: &str,
    repo: &str,
    proxy: Option<&str>,
) -> Result<Value, String> {
    let mut headers = HashMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("User-Agent".to_string(), "TwinSwitch".to_string());
    let mut last_err = "更新清单解析失败".to_string();
    for url in updater_manifest_urls(owner, repo, std::env::consts::OS, std::env::consts::ARCH) {
        let resp = http_request_with_proxy(&url, "GET", None, Some(&headers), proxy).await;
        let version = resp.get("version").and_then(|v| v.as_str()).unwrap_or("");
        if !version.trim().is_empty() {
            return Ok(resp);
        }
        last_err = resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("更新清单解析失败")
            .to_string();
    }
    Err(last_err)
}

/// 兜底端点：请求 `/releases/latest`，读 302 的 `Location` 头解析 tag。
///
/// **必须禁止跟随重定向**：跟过去拿到的是发布页 HTML（几十 KB），既浪费流量又解析不出东西。
/// 成功返回 tag（如 `v0.2.0`），失败返回可读错误 + code（状态码或 -1）。
async fn fetch_latest_tag(
    owner: &str,
    repo: &str,
    proxy: Option<&str>,
) -> Result<String, (String, i64)> {
    let url = format!("https://github.com/{owner}/{repo}/releases/latest");
    let mut headers = HashMap::new();
    headers.insert("Accept".to_string(), "text/html".to_string());
    headers.insert("User-Agent".to_string(), "TwinSwitch".to_string());
    let (status, resp_headers, body) =
        http_request_raw(&url, "GET", None, Some(&headers), proxy, false).await;

    if status == 0 {
        let msg = if body.trim().is_empty() {
            "网络请求失败".to_string()
        } else {
            body
        };
        return Err((msg, -1));
    }
    if status == 404 {
        // 没有正式 release，或仓库不存在。
        return Err(("未找到可用的发布版本".to_string(), 404));
    }
    let location = resp_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.clone());
    let location = match location {
        Some(location) => location,
        None => {
            return Err((
                format!("无法获取发布页跳转地址（HTTP {status}）"),
                status as i64,
            ))
        }
    };
    let tag = location.rsplit('/').next().unwrap_or("").trim().to_string();
    if tag.is_empty() || !location.contains("/releases/tag/") {
        return Err(("无法解析发布版本标签".to_string(), -1));
    }
    Ok(tag)
}

/// 查询最新 Release 并与本地版本对比。
///
/// `force=true` 绕过缓存强制刷新（界面手动检查）；否则 6 小时内的成功结果直接返回，
/// 一个字节的网络请求都不发。主端点失败时自动走 302 兜底；两个都失败才报错。
pub async fn update_check(proxy: Option<&str>, force: bool) -> Value {
    if !force {
        if let Some(cached) = CACHE.lock().unwrap().as_ref() {
            if now_secs() - cached.checked_at < CACHE_TTL_SECS {
                return cached.value.clone();
            }
        }
    }

    let cfg = load_update_config();
    let owner = cfg
        .get("owner")
        .and_then(|v| v.as_str())
        .unwrap_or(GITHUB_OWNER)
        .to_string();
    let repo = cfg
        .get("repo")
        .and_then(|v| v.as_str())
        .unwrap_or(GITHUB_REPO)
        .to_string();
    let configured_proxy = cfg
        .get("proxy")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let proxy = proxy.or(configured_proxy);
    let release_url = format!("https://github.com/{owner}/{repo}/releases/latest");
    let current = APP_VERSION.to_string();

    // 主端点：updater manifest（release 资产下载，不计 GitHub API 配额）。
    if let Ok(manifest) = fetch_manifest_version(&owner, &repo, proxy).await {
        let version = manifest
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let latest = version.strip_prefix('v').unwrap_or(version).to_string();
        let tag = format!("v{latest}");
        let release_name = manifest
            .get("notes")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(&tag)
            .to_string();
        let published_at = manifest
            .get("pub_date")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let value = json!({
            "ok": true,
            "current": current,
            "latest": latest,
            "latestTag": tag,
            "hasUpdate": compare_versions(&latest, &current) > 0,
            "releaseName": release_name,
            "releaseUrl": release_url,
            "publishedAt": published_at,
            "checkedAt": now_secs(),
        });
        *CACHE.lock().unwrap() = Some(CachedCheck {
            checked_at: now_secs(),
            value: value.clone(),
        });
        return value;
    }

    // 兜底端点：`/releases/latest` 的 302 Location 头（仅主端点失败时走）。
    match fetch_latest_tag(&owner, &repo, proxy).await {
        Ok(tag) => {
            let latest = tag.strip_prefix('v').unwrap_or(&tag).to_string();
            let value = json!({
                "ok": true,
                "current": current,
                "latest": latest,
                "latestTag": tag,
                "hasUpdate": compare_versions(&latest, &current) > 0,
                "releaseName": tag.clone(),
                "releaseUrl": release_url,
                "checkedAt": now_secs(),
            });
            *CACHE.lock().unwrap() = Some(CachedCheck {
                checked_at: now_secs(),
                value: value.clone(),
            });
            value
        }
        Err((msg, code)) => json!({
            "ok": false,
            "error": msg,
            "message": format!("{msg}（code={code}）"),
            "releaseUrl": release_url,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_versions_orders_semver() {
        assert_eq!(compare_versions("0.2.0", "0.2.0"), 0);
        assert_eq!(compare_versions("v0.2.1", "0.1.0"), 1);
        assert_eq!(compare_versions("0.1.0", "0.2.0"), -1);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), 1, "跨大版本按数值比，不按字符串");
        assert_eq!(compare_versions("0.10.0", "0.9.0"), 1, "两位数是 10 不是 1");
        assert_eq!(compare_versions("0.2", "0.2.0"), 0, "段数不齐按 0 补齐");
        assert_eq!(
            compare_versions("0.2.0-beta.1", "0.2.0"),
            0,
            "预发布后缀整个截掉，不能把 `.1` 补位成第三段"
        );
        assert_eq!(compare_versions("0.10.0-beta.2", "0.9.9"), 1);
    }

    #[test]
    fn updater_manifest_urls_prefers_merged_then_platform() {
        let urls = updater_manifest_urls("bean0283", "twin-switch", "windows", "x86_64");
        assert_eq!(
            urls,
            vec![
                "https://github.com/bean0283/twin-switch/releases/latest/download/latest.json",
                "https://github.com/bean0283/twin-switch/releases/latest/download/latest-windows-x86_64.json",
            ]
        );
    }

    #[test]
    fn updater_manifest_urls_normalizes_darwin_to_macos() {
        let urls = updater_manifest_urls("bean0283", "twin-switch", "darwin", "aarch64");
        assert!(urls[1].ends_with("latest-macos-aarch64.json"), "{urls:?}");
    }

    #[test]
    fn version_constants_point_at_the_real_repo() {
        assert_eq!(GITHUB_OWNER, "bean0283");
        assert_eq!(GITHUB_REPO, "twin-switch");
        assert!(!APP_VERSION.is_empty());
    }

    /// 配置读取永不返回 token，且缺文件时给出默认 owner/repo。
    #[test]
    fn load_update_config_defaults_without_file() {
        let cfg = load_update_config();
        assert_eq!(cfg.get("owner").and_then(Value::as_str), Some(GITHUB_OWNER));
        assert_eq!(cfg.get("repo").and_then(Value::as_str), Some(GITHUB_REPO));
        assert!(cfg.get("token").is_none());
    }
}
