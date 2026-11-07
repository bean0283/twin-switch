//! 青龙面板对接：把账号 token / 通用环境变量一键提交到青龙。
//!
//! 数据目录 `~/.twin-switch/qinglong/`（目前只有「上次可用基址」缓存一个文件）。
//!
//! 判据与流程**整体移植**自 `D:\htw\签到2\token_tool.ps1`（用户已在用的工具），
//! 不重新发明。三条硬规矩也从那边带过来了：
//!
//! 1. **基址跟随 302**：穿透地址会漂移、入口会临时挂掉 —— 入口跳转解出的真实基址
//!    缓存到本地，下次连接先试缓存再试入口（ps1 的 `ql_base_cache.txt` 机制）。
//! 2. **token 识别**（[`identify`]）：WorkBuddy 是 Keycloak 风格 JWT（`iss` 含
//!    `copilot` / `app_type=codebuddy` / `azp=console`+`aud` 含 `account`），
//!    Trae 的 payload 里有 `data.id / data.user_id / data.tenant_id`。
//!    两类都解不出 = `Unknown`（如实返回，不瞎猜）。
//! 3. **同账号匹配**（[`find_matches`]）：Trae 按 `data.id`（userId）、
//!    WorkBuddy 按 `sub` 回退 `preferred_username`；命中 = 覆盖（PUT），未命中 =
//!    新增（POST，备注 = 账号显示名）。Qoder 的 access_token **不是 JWT**（解不出
//!    账号标识，用户已确认），按「备注 = 账号显示名」匹配覆盖。
//!
//! 网络一律走 [`crate::modules::config::http_request_raw`] / [`http_request`]，
//! 失败照实返回（`Err`），绝不把失败伪装成成功。

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::modules::config::{http_request, http_request_raw, store_dir};
use crate::modules::jwt;
use crate::modules::settings;

// ---------------------------------------------------------------------------
// 目录
// ---------------------------------------------------------------------------

/// `~/.twin-switch/qinglong/`。
fn qinglong_dir() -> PathBuf {
    store_dir().join("qinglong")
}

/// 「上次可用基址」缓存（穿透漂移兜底，ps1 的 `ql_base_cache.txt` 同机制）。
fn base_cache_path() -> PathBuf {
    qinglong_dir().join("base.txt")
}

fn read_base_cache() -> Option<String> {
    let text = std::fs::read_to_string(base_cache_path()).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn write_base_cache(base: &str) {
    let dir = qinglong_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(base_cache_path(), base);
}

// ---------------------------------------------------------------------------
// 配置读取
// ---------------------------------------------------------------------------

/// 面板连接参数（从 [`settings::Settings::qinglong`] 读）。
#[derive(Debug, Clone)]
pub struct QlConn {
    pub url: String,
    pub client_id: String,
    pub client_secret: String,
}

impl QlConn {
    /// 配置不完整 = 没法连（如实说明缺哪项，不猜）。
    pub fn validate(&self) -> Result<(), String> {
        if self.url.trim().is_empty() {
            return Err("面板地址未填写（设置 → 青龙面板）".into());
        }
        if self.client_id.trim().is_empty() || self.client_secret.trim().is_empty() {
            return Err("Client ID / Client Secret 未填写（设置 → 青龙面板）".into());
        }
        Ok(())
    }
}

/// 当前设置里的青龙配置。
pub fn conn_from_settings() -> QlConn {
    let s = settings::load();
    QlConn {
        url: s.qinglong.url.trim().trim_end_matches('/').to_string(),
        client_id: s.qinglong.client_id.trim().to_string(),
        client_secret: s.qinglong.client_secret.trim().to_string(),
    }
}

/// 某产品的环境变量名（按 provider 取；未配置 = 空串，调用方负责报错）。
pub fn env_name_for(provider: &str) -> String {
    let s = settings::load();
    match provider {
        "trae" => s.qinglong.env_trae.trim().to_string(),
        "workbuddy" => s.qinglong.env_wb.trim().to_string(),
        "qoder" => s.qinglong.env_qoder.trim().to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// 基址解析（302 跟随）与登录
// ---------------------------------------------------------------------------

/// 跟随最多 4 跳 302，解出真实基址。地址完全连不上返回 `None`。
///
/// ⚠️ 必须用 `follow_redirects = false` 的 raw 请求读 `Location` 头 —— 自动跟随
/// 会直接落到最终页（HTML），拿不到中间跳转。
async fn resolve_base(entry: &str) -> Option<String> {
    let mut cur = entry.trim_end_matches('/').to_string();
    if cur.is_empty() {
        return None;
    }
    for _ in 0..4 {
        let (status, headers, _body) =
            http_request_raw(&cur, "GET", None, None, None, false).await;
        // 连不上（status == 0）= 这个候选不可用。
        if status == 0 {
            return None;
        }
        let location = headers.get("location").cloned().unwrap_or_default();
        if location.trim().is_empty() {
            return Some(cur);
        }
        let next = join_url(&cur, location.trim());
        if next == cur {
            return Some(cur);
        }
        cur = next;
    }
    Some(cur)
}

/// 相对 `Location` 头拼接（`/open/...` 这类相对跳转）。
fn join_url(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        location.trim_end_matches('/').to_string()
    } else if let Some(rest) = location.strip_prefix('/') {
        format!("{base}/{rest}")
    } else {
        format!("{base}/{location}")
    }
    .trim_end_matches('/')
    .to_string()
}

/// 登录拿 Bearer（青龙 openapi：`GET /open/auth/token`）。
async fn login(base: &str, conn: &QlConn) -> Result<String, String> {
    let url = format!(
        "{base}/open/auth/token?client_id={}&client_secret={}",
        urlencode(&conn.client_id),
        urlencode(&conn.client_secret),
    );
    let resp = http_request(&url, "GET", None, None).await;
    if resp.get("code").and_then(Value::as_i64) == Some(-1) {
        return Err(format!(
            "面板登录请求失败：{}",
            resp.get("message").and_then(Value::as_str).unwrap_or("网络错误")
        ));
    }
    let token = resp
        .get("data")
        .and_then(|d| d.get("token"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if token.is_empty() {
        return Err(format!(
            "登录响应里没有 token（面板返回：{}）",
            serde_json::to_string(&resp).unwrap_or_default().chars().take(200).collect::<String>()
        ));
    }
    Ok(token.to_string())
}

/// 连接青龙：按「缓存基址 → 入口」顺序试，登录成功即采用并写缓存。
/// 返回 `(基址, Bearer)`。
async fn connect(conn: &QlConn) -> Result<(String, String), String> {
    conn.validate()?;
    let entry = conn.url.trim_end_matches('/').to_string();
    let mut candidates: Vec<String> = Vec::new();
    if let Some(cached) = read_base_cache() {
        if cached != entry {
            candidates.push(cached);
        }
    }
    candidates.push(entry.clone());

    let mut errors: Vec<String> = Vec::new();
    for cand in candidates {
        let Some(base) = resolve_base(&cand).await else {
            errors.push(format!("{cand} → 地址不可达"));
            continue;
        };
        match login(&base, conn).await {
            Ok(token) => {
                // 与 ps1 一致：成功即缓存（下次穿透漂移时最快）。
                write_base_cache(&base);
                return Ok((base, token));
            }
            Err(e) => errors.push(format!("{base} → {e}")),
        }
    }
    Err(format!("无法连接青龙面板：\n  {}", errors.join("\n  ")))
}

// ---------------------------------------------------------------------------
// 环境变量 CRUD
// ---------------------------------------------------------------------------

async fn list_envs(base: &str, bearer: &str) -> Result<Vec<Value>, String> {
    let resp = ql_api(base, bearer, "GET", "/open/envs", None).await?;
    let data = resp.get("data").cloned().unwrap_or(Value::Null);
    Ok(match data {
        Value::Array(items) => items,
        Value::Object(_) => vec![data],
        _ => Vec::new(),
    })
}

/// 单条 PUT（覆盖）。
async fn update_env(
    base: &str,
    bearer: &str,
    env: &Value,
    value: &str,
) -> Result<Value, String> {
    let body = json!({
        "id": env.get("id").cloned().unwrap_or(Value::Null),
        "name": env.get("name").cloned().unwrap_or(Value::Null),
        "value": value,
        "remarks": env.get("remarks").cloned().unwrap_or(json!("")),
    });
    let resp = ql_api(base, bearer, "PUT", "/open/envs", Some(body)).await?;
    ensure_ql_ok(&resp, "更新")
}

/// 新增（青龙 openapi 的 POST body 是**数组**）。
async fn create_env(
    base: &str,
    bearer: &str,
    name: &str,
    value: &str,
    remarks: &str,
) -> Result<Value, String> {
    let body = json!([{ "name": name, "value": value, "remarks": remarks }]);
    let resp = ql_api(base, bearer, "POST", "/open/envs", Some(body)).await?;
    ensure_ql_ok(&resp, "新增")
}

async fn ql_api(
    base: &str,
    bearer: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let url = format!("{base}{path}");
    let mut headers = std::collections::HashMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {bearer}"));
    let resp = http_request(&url, method, body, Some(&headers)).await;
    if resp.get("code").and_then(Value::as_i64) == Some(-1) {
        return Err(format!(
            "面板请求失败（{method} {path}）：{}",
            resp.get("message").and_then(Value::as_str).unwrap_or("网络错误")
        ));
    }
    Ok(resp)
}

/// 青龙业务应答的 `code == 200` 才算成功（传输层通了 ≠ 业务成了）。
fn ensure_ql_ok(resp: &Value, what: &str) -> Result<Value, String> {
    if resp.get("code").and_then(Value::as_i64) == Some(200) {
        Ok(resp.clone())
    } else {
        Err(format!(
            "{what}失败：{}",
            serde_json::to_string(resp).unwrap_or_default().chars().take(300).collect::<String>()
        ))
    }
}

// ---------------------------------------------------------------------------
// token 识别与匹配（ps1 判据原样移植）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QlTokenKind {
    Trae,
    WorkBuddy,
    Unknown,
}

/// 识别结果：类型 + 账号标识（Trae = userId；WB = sub，次标识 = 手机号）。
#[derive(Debug, Clone)]
pub struct Ident {
    pub kind: QlTokenKind,
    pub key: Option<String>,
    pub key2: Option<String>,
}

/// 从 JWT payload 识别 token 类型与账号标识（**无验签**，与 ps1 同判据）。
pub fn identify(token: &str) -> Ident {
    let Some(p) = jwt::payload(token) else {
        return Ident { kind: QlTokenKind::Unknown, key: None, key2: None };
    };
    let iss = str_of(&p, "iss");
    let app_type = str_of(&p, "app_type");
    let azp = str_of(&p, "azp");
    let aud = aud_str(&p);

    // ---- 先判 WorkBuddy（Keycloak 风格）----
    let is_wb = iss.contains("copilot")
        || app_type == "codebuddy"
        || (azp == "console" && aud.contains("account") && !str_of(&p, "preferred_username").is_empty());
    if is_wb {
        return Ident {
            kind: QlTokenKind::WorkBuddy,
            key: non_empty(str_of(&p, "sub")),
            key2: non_empty(str_of(&p, "preferred_username")),
        };
    }

    // ---- 再判 Trae ----
    if let Some(data) = p.get("data").filter(|d| d.is_object()) {
        let id = non_empty(str_of(data, "id")).or_else(|| non_empty(str_of(data, "user_id")));
        let tenant = non_empty(str_of(data, "tenant_id"));
        if id.is_some() || tenant.is_some() {
            return Ident { kind: QlTokenKind::Trae, key: id, key2: None };
        }
    }

    Ident { kind: QlTokenKind::Unknown, key: None, key2: None }
}

/// 清洗粘贴输入（ps1 的 `Get-CleanToken`）：支持裸 token、`NAME=xxx` 整行、带引号。
pub fn clean_token(raw: &str) -> String {
    for line in raw.lines() {
        let mut s = line.trim();
        if s.is_empty() {
            continue;
        }
        s = strip_assignment(s).trim();
        s = s.trim_matches('"').trim_matches('\'').trim();
        // ps1 同判据：三段 base64url，头两段非空（签名段允许空）。
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() == 3
            && !parts[0].is_empty()
            && !parts[1].is_empty()
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return s.to_string();
        }
    }
    strip_assignment(raw.trim()).trim().trim_matches('"').trim_matches('\'').trim().to_string()
}

fn strip_assignment(s: &str) -> &str {
    // `TRAE_TOKEN = xxx` / `WB_TOKEN="xxx"`：剥 `名字 =` 前缀。
    if let Some(eq) = s.find('=') {
        let name = s[..eq].trim();
        if !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return s[eq + 1..].trim();
        }
    }
    s
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn aud_str(p: &Value) -> String {
    match p.get("aud") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// 在环境变量列表里找「同账号」的旧变量（始终返回数组，可能为空）。
///
/// - Trae / WorkBuddy：变量值也要能解出同类 token，再按账号标识比对；
/// - Qoder（kind=Unknown，用户已确认按备注匹配）：`remarks` 与传入备注一致即命中。
pub fn find_matches(envs: &[Value], env_name: &str, ident: &Ident, remark: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for e in envs {
        if str_of(e, "name") != env_name {
            continue;
        }
        match ident.kind {
            QlTokenKind::Trae | QlTokenKind::WorkBuddy => {
                let Some(value) = e.get("value").and_then(Value::as_str) else { continue };
                let other = identify(value);
                if other.kind != ident.kind {
                    continue;
                }
                let key_hit = match (&ident.key, &other.key) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                let key2_hit = match (&ident.key2, &other.key2) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                // WB：sub 相同即命中；sub 缺失时回退手机号（ps1 同判据）。
                if key_hit || (ident.kind == QlTokenKind::WorkBuddy && key2_hit) {
                    out.push(e.clone());
                }
            }
            QlTokenKind::Unknown => {
                if !remark.trim().is_empty()
                    && str_of(e, "remarks").trim() == remark.trim()
                {
                    out.push(e.clone());
                }
            }
        }
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 对外入口（status / 提交）
// ---------------------------------------------------------------------------

/// 探测当前连接状态（弹窗按钮的显示依据）。**开关没开直接短返回**，不打网络。
pub async fn status() -> Value {
    let s = settings::load();
    let cfg = &s.qinglong;
    if !cfg.enabled {
        return json!({ "enabled": false });
    }
    let conn = QlConn {
        url: cfg.url.trim().trim_end_matches('/').to_string(),
        client_id: cfg.client_id.trim().to_string(),
        client_secret: cfg.client_secret.trim().to_string(),
    };
    if let Err(e) = conn.validate() {
        return json!({ "enabled": true, "connected": false, "error": e });
    }
    // 探测兜底 8 秒：面板挂了不能让弹窗干等。
    match tokio::time::timeout(std::time::Duration::from_secs(8), connect(&conn)).await {
        Ok(Ok((base, bearer))) => {
            let envs = list_envs(&base, &bearer).await.unwrap_or_default();
            json!({
                "enabled": true,
                "connected": true,
                "base": base,
                "envs": envs.len(),
                "envTrae": cfg.env_trae,
                "envWb": cfg.env_wb,
                "envQoder": cfg.env_qoder,
            })
        }
        Ok(Err(e)) => json!({ "enabled": true, "connected": false, "error": e }),
        Err(_) => json!({ "enabled": true, "connected": false, "error": "连接超时（8 秒）" }),
    }
}

/// 提交一个账号 token（弹窗的「⬆ 提交到青龙」）。
///
/// `provider` = `trae` | `workbuddy` | `qoder`；`remark` = 账号显示名
/// （新增时的备注；Qoder 匹配也按它）。返回落盘结果（更新了哪些 id / 新增了哪条）。
pub async fn submit_token(provider: &str, token: &str, remark: &str) -> Result<Value, String> {
    let conn = conn_from_settings();
    if !settings::load().qinglong.enabled {
        return Err("青龙提交未启用（设置 → 青龙面板）".into());
    }
    let env_name = env_name_for(provider);
    if env_name.is_empty() {
        return Err(format!("未配置 {provider} 的环境变量名（设置 → 青龙面板）"));
    }
    let cleaned = clean_token(token);
    if cleaned.is_empty() {
        return Err("token 为空，没有可提交的内容".into());
    }
    let ident = identify(&cleaned);
    // 类型与 provider 对不上 = 提交的是别的产品的 token（照实拦，不硬塞）。
    let kind_ok = match provider {
        "trae" => ident.kind == QlTokenKind::Trae,
        "workbuddy" => ident.kind == QlTokenKind::WorkBuddy,
        // Qoder 不是 JWT，本来就识别不出 —— 不拦。
        _ => true,
    };
    if !kind_ok {
        return Err(format!(
            "这个 token 不是 {provider} 的（识别结果：{}），没有提交",
            match ident.kind {
                QlTokenKind::Trae => "Trae",
                QlTokenKind::WorkBuddy => "WorkBuddy",
                QlTokenKind::Unknown => "无法识别",
            }
        ));
    }

    let (base, bearer) = connect(&conn).await?;
    let envs = list_envs(&base, &bearer).await?;
    let matches = find_matches(&envs, &env_name, &ident, remark);

    if !matches.is_empty() {
        let mut ids: Vec<Value> = Vec::new();
        for m in &matches {
            update_env(&base, &bearer, m, &cleaned).await?;
            ids.push(m.get("id").cloned().unwrap_or(Value::Null));
        }
        return Ok(json!({
            "action": "updated",
            "envName": env_name,
            "ids": ids,
            "matched": matches.len(),
        }));
    }

    create_env(&base, &bearer, &env_name, &cleaned, remark.trim()).await?;
    Ok(json!({
        "action": "created",
        "envName": env_name,
        "remark": remark.trim(),
    }))
}

/// 提交一条通用环境变量（设置页的「直接提交」）。
///
/// `mode`：`None` = 自动（无同名 → 新增；有同名 → 返回 `needChoice` 让用户选）；
/// `Some("update")` 覆盖 `env_id` 那条；`Some("append")` 强制追加一条。
pub async fn submit_env(
    name: &str,
    value: &str,
    remarks: &str,
    mode: Option<&str>,
    env_id: Option<i64>,
) -> Result<Value, String> {
    let conn = conn_from_settings();
    if !settings::load().qinglong.enabled {
        return Err("青龙提交未启用（设置 → 青龙面板）".into());
    }
    let name = name.trim();
    let value = value;
    if name.is_empty() {
        return Err("变量名不能为空".into());
    }
    if value.trim().is_empty() {
        return Err("变量值不能为空（要清空请去面板操作）".into());
    }
    let (base, bearer) = connect(&conn).await?;
    let envs = list_envs(&base, &bearer).await?;
    let same_name: Vec<&Value> =
        envs.iter().filter(|e| str_of(e, "name") == name).collect();

    match mode {
        Some("update") => {
            let id = env_id.ok_or("覆盖模式必须指定要覆盖的变量 id")?;
            let env = same_name
                .iter()
                .find(|e| e.get("id").and_then(Value::as_i64) == Some(id))
                .ok_or(format!("找不到 id={id} 的同名变量，请刷新后重试"))?;
            update_env(&base, &bearer, env, value).await?;
            Ok(json!({ "action": "updated", "envName": name, "id": id }))
        }
        Some("append") => {
            create_env(&base, &bearer, name, value, remarks.trim()).await?;
            Ok(json!({ "action": "created", "envName": name }))
        }
        _ => {
            if same_name.is_empty() {
                create_env(&base, &bearer, name, value, remarks.trim()).await?;
                Ok(json!({ "action": "created", "envName": name }))
            } else {
                // 不替用户做决定：列出同名变量让他选「覆盖哪条 / 追加」。
                Ok(json!({
                    "needChoice": true,
                    "envName": name,
                    "existing": same_name.iter().map(|e| json!({
                        "id": e.get("id").cloned().unwrap_or(Value::Null),
                        "remarks": str_of(e, "remarks"),
                        "valuePreview": {
                            "len": e.get("value").and_then(Value::as_str).map(str::len).unwrap_or(0),
                        },
                    })).collect::<Vec<_>>(),
                }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工拼一个假 JWT（只解 payload，不验签，所以随便签）。
    fn fake_jwt(payload: &Value) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let b64 = |v: &Value| {
            use base64::Engine;
            engine.encode(serde_json::to_vec(v).unwrap())
        };
        format!(
            "{}.{}.sig",
            b64(&json!({ "alg": "RS256", "typ": "JWT" })),
            b64(payload)
        )
    }

    fn ident_of(kind: &QlTokenKind, key: &str, key2: &str) -> Ident {
        Ident {
            kind: *kind,
            key: non_empty(key.to_string()),
            key2: non_empty(key2.to_string()),
        }
    }

    // ---- 识别 ----

    #[test]
    fn trae_token_is_identified_by_data_id() {
        let t = fake_jwt(&json!({ "data": { "id": "717036", "tenant_id": "t1" }, "exp": 1798675200 }));
        let id = identify(&t);
        assert_eq!(id.kind, QlTokenKind::Trae);
        assert_eq!(id.key.as_deref(), Some("717036"));
    }

    #[test]
    fn trae_token_falls_back_to_user_id() {
        let t = fake_jwt(&json!({ "data": { "user_id": "u9" } }));
        assert_eq!(identify(&t).key.as_deref(), Some("u9"));
    }

    #[test]
    fn workbuddy_token_identified_by_iss_copilot() {
        let t = fake_jwt(&json!({
            "iss": "https://auth.codewb.cn/realms/copilot",
            "sub": "abcd1234-5678",
            "preferred_username": "13800001111",
        }));
        let id = identify(&t);
        assert_eq!(id.kind, QlTokenKind::WorkBuddy);
        assert_eq!(id.key.as_deref(), Some("abcd1234-5678"));
        assert_eq!(id.key2.as_deref(), Some("13800001111"));
    }

    #[test]
    fn workbuddy_token_identified_by_app_type() {
        let t = fake_jwt(&json!({ "app_type": "codebuddy", "sub": "s1" }));
        assert_eq!(identify(&t).kind, QlTokenKind::WorkBuddy);
    }

    #[test]
    fn workbuddy_token_identified_by_azp_console() {
        let t = fake_jwt(&json!({
            "azp": "console",
            "aud": ["account"],
            "preferred_username": "13900002222",
        }));
        assert_eq!(identify(&t).kind, QlTokenKind::WorkBuddy);
    }

    #[test]
    fn azp_console_without_username_is_unknown() {
        // azp=console + aud=account 但没有 preferred_username：不满足 WB 判据。
        let t = fake_jwt(&json!({ "azp": "console", "aud": ["account"] }));
        assert_eq!(identify(&t).kind, QlTokenKind::Unknown);
    }

    #[test]
    fn qoder_opaque_token_is_unknown() {
        assert_eq!(identify("dt-abc123notajwt").kind, QlTokenKind::Unknown);
    }

    #[test]
    fn cloud_ide_jwt_prefix_is_stripped() {
        let t = fake_jwt(&json!({ "data": { "id": "1" } }));
        let id = identify(&format!("Cloud-IDE-JWT {t}"));
        assert_eq!(id.kind, QlTokenKind::Trae);
    }

    // ---- 输入清洗 ----

    #[test]
    fn clean_token_handles_assignment_and_quotes() {
        let t = fake_jwt(&json!({ "data": { "id": "1" } }));
        assert_eq!(clean_token(&format!("TRAE_TOKEN={t}")), t);
        assert_eq!(clean_token(&format!("WB_TOKEN = \"{t}\"")), t);
        assert_eq!(clean_token(&format!("  {t}\n  垃圾行  ")), t);
        assert_eq!(clean_token("   "), "");
    }

    // ---- 匹配 ----

    fn env_of(id: i64, name: &str, value: &str, remarks: &str) -> Value {
        json!({ "id": id, "name": name, "value": value, "remarks": remarks, "status": 0 })
    }

    #[test]
    fn trae_matches_by_user_id_only() {
        let envs = [
            env_of(1, "TRAE_TOKEN", &fake_jwt(&json!({ "data": { "id": "717036" } })), "韩腾飞"),
            env_of(2, "TRAE_TOKEN", &fake_jwt(&json!({ "data": { "id": "999" } })), "别人"),
            env_of(3, "WB_TOKEN", &fake_jwt(&json!({ "iss": "x/copilot", "sub": "717036" })), "同名不同类"),
        ];
        let ident = ident_of(&QlTokenKind::Trae, "717036", "");
        let hits = find_matches(&envs, "TRAE_TOKEN", &ident, "韩腾飞");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("id").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn workbuddy_matches_by_sub_then_phone() {
        let envs = [
            env_of(1, "WB_TOKEN", &fake_jwt(&json!({ "iss": "x/copilot", "sub": "sub-a", "preferred_username": "138" })), "甲"),
            env_of(2, "WB_TOKEN", &fake_jwt(&json!({ "iss": "x/copilot", "sub": "sub-b", "preferred_username": "13800001111" })), "乙"),
        ];
        // sub 与手机号都各自只命中一条：判据是「任一命中即算」，所以各匹配各的。
        let ident = ident_of(&QlTokenKind::WorkBuddy, "sub-a", "13899999999");
        let hits = find_matches(&envs, "WB_TOKEN", &ident, "甲");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("id").and_then(Value::as_i64), Some(1));

        // sub 不在面板里时回退手机号。
        let ident2 = ident_of(&QlTokenKind::WorkBuddy, "sub-none", "13800001111");
        let hits2 = find_matches(&envs, "WB_TOKEN", &ident2, "");
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].get("id").and_then(Value::as_i64), Some(2));
    }

    #[test]
    fn qoder_matches_by_remark() {
        let envs = [
            env_of(1, "QODER_TOKEN", "dt-opaque-1", "弦思十安"),
            env_of(2, "QODER_TOKEN", "dt-opaque-2", "别人"),
            env_of(3, "QODER_TOKEN", "dt-opaque-3", ""),
        ];
        let ident = ident_of(&QlTokenKind::Unknown, "", "");
        let hits = find_matches(&envs, "QODER_TOKEN", &ident, "弦思十安");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("id").and_then(Value::as_i64), Some(1));
        // 备注为空 = 没有匹配依据 → 不命中（避免误覆盖）。
        assert!(find_matches(&envs, "QODER_TOKEN", &ident, "").is_empty());
    }

    #[test]
    fn match_requires_same_env_name() {
        let envs = [env_of(1, "TRAE_TOKEN_OLD", &fake_jwt(&json!({ "data": { "id": "1" } })), "")];
        let ident = ident_of(&QlTokenKind::Trae, "1", "");
        assert!(find_matches(&envs, "TRAE_TOKEN", &ident, "").is_empty());
    }

    // ---- 杂项 ----

    #[test]
    fn join_url_handles_relative_and_absolute() {
        assert_eq!(join_url("https://a.b", "/open/x"), "https://a.b/open/x");
        assert_eq!(join_url("https://a.b", "https://c.d/"), "https://c.d");
    }

    #[test]
    fn urlencode_escapes_specials() {
        assert_eq!(urlencode("aB1-_.~"), "aB1-_.~");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
    }
}
