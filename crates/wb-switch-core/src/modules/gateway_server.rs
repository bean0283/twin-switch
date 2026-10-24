//! 积分共享 · 本机网关（`127.0.0.1:57892`）。
//!
//! 对外只暴露 OpenAI 兼容的三个端点：
//!
//! - `GET  /healthz` —— 网关状态（界面用）
//! - `GET  /v1/models` —— 模型清单（来自 `~/.twin-switch/models.json`）
//! - `POST /v1/chat/completions` —— 选号后透传给对应产品的上游
//!
//! 三条纪律：
//!
//! 1. **只绑 127.0.0.1**。网关能代替你花掉账号额度，绑 0.0.0.0 等于借给整个局域网。
//! 2. **没配上游就 501，不假装成功**；鉴权失败单独说，不和网络错误混在一起。
//! 3. **一次失败不许把数字说成成功**：请求数只统计真正打通上游的次数。

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::modules::gateway_keys::{self, GATEWAY_PORT};
use crate::modules::gateway_pool;

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static REJECTED: AtomicU64 = AtomicU64::new(0);
static STARTED_AT: AtomicI64 = AtomicI64::new(0);

pub fn status() -> Value {
    let started = STARTED_AT.load(Ordering::Relaxed);
    json!({
        "running": started > 0,
        "port": GATEWAY_PORT,
        "started_at": if started > 0 { Some(started) } else { None },
        "requests": REQUESTS.load(Ordering::Relaxed),
        "rejected": REJECTED.load(Ordering::Relaxed),
        "addr": format!("http://127.0.0.1:{GATEWAY_PORT}/v1"),
    })
}

/// 使用说明页的**真机自检**：把「网关在跑 → 能列出模型 → 会选到哪个账号 → 真发一次对话」
/// 四步都跑一遍，每步都带**真实结果**（不是示意图）。
///
/// - 全程只读，除第 4 步（需要用户提供一枚 Key）外不发任何消耗额度的请求；
/// - 任何一步失败都如实返回 `ok:false` + 原因，绝不把「没跑」写成「通过」。
pub async fn selftest(key: Option<String>, model: Option<String>) -> Value {
    let st = status();
    let running = st.get("running").and_then(Value::as_bool).unwrap_or(false);
    let base = format!("http://127.0.0.1:{GATEWAY_PORT}");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("建 HTTP 客户端失败：{e}") }),
    };

    // ⚠️ `status()` 读的是**本进程**的启动标记：同进程里准确，但换进程（例如自检工具、
    //    或者网关其实是另一个实例起的）就会误报「没在跑」。所以再**探一次活**
    //    （`GET /healthz`）——「端口上真的有人应答」才是事实。
    let alive = client
        .get(format!("{base}/healthz"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let running = running || alive;

    let mut steps: Vec<Value> = Vec::new();
    steps.push(json!({
        "id": "gateway",
        "label": "网关在监听 127.0.0.1:57892",
        "ok": running,
        "detail": if running {
            if alive { "端口有应答（/healthz 200）".to_string() }
            else { "本进程已启动监听".to_string() }
        } else {
            "没有在监听：到「积分共享」页把开关打开".to_string()
        },
    }));

    // ② 真实模型清单（`/v1/models` 不需要 Key）
    let mut models: Vec<String> = Vec::new();
    let mut models_detail = String::new();
    let mut models_ok = false;
    if running {
        match client.get(format!("{base}/v1/models")).send().await {
            Ok(r) => {
                let code = r.status();
                let text = r.text().await.unwrap_or_default();
                if code.is_success() {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if let Some(arr) = v.get("data").and_then(Value::as_array) {
                            models = arr
                                .iter()
                                .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                                .collect();
                            models_ok = !models.is_empty();
                            models_detail = format!("{} 个模型", models.len());
                        } else {
                            models_detail = "响应里没有 data 数组".into();
                        }
                    } else {
                        models_detail = "响应不是 JSON".into();
                    }
                } else {
                    models_detail = format!("HTTP {}", code.as_u16());
                }
            }
            Err(e) => models_detail = format!("连不上：{e}"),
        }
    } else {
        models_detail = "网关没在跑，跳过".into();
    }
    steps.push(json!({
        "id": "models",
        "label": "能列出可用模型",
        "ok": models_ok,
        "detail": models_detail,
        "sample": models.iter().take(6).cloned().collect::<Vec<_>>(),
    }));

    // ③ 会路由到哪个账号（本地判定，不发请求）
    let target = model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| {
            models
                .iter()
                .find(|m| !m.eq_ignore_ascii_case("auto"))
                .cloned()
        })
        .unwrap_or_default();
    let (route_ok, route_detail) = if target.is_empty() {
        (false, "没有可用来演示的模型".to_string())
    } else {
        match gateway_pool::preview(&target) {
            Ok(sel) => match sel.picked {
                Some(p) => (
                    true,
                    format!(
                        "{} → 「{}」（{}，剩余 {}）",
                        target,
                        p.label,
                        p.scope,
                        p.remaining
                            .map(|n| format!("{n:.0}"))
                            .unwrap_or_else(|| "未知".into())
                    ),
                ),
                None => (
                    false,
                    format!(
                        "{}：{}",
                        target,
                        sel.reason.unwrap_or_else(|| "没有可用账号".into())
                    ),
                ),
            },
            Err(e) => (false, format!("{}：{e}", target)),
        }
    };
    steps.push(json!({
        "id": "route",
        "label": "模型能选到账号",
        "ok": route_ok,
        "detail": route_detail,
        "model": target,
    }));

    // ④ 真发一次对话（需要 Key）
    let mut chat = Value::Null;
    if let Some(k) = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        if running && !target.is_empty() {
            let t0 = std::time::Instant::now();
            let body = json!({
                "model": target,
                "messages": [{ "role": "user", "content": "只回复两个字母：ok" }],
                "stream": false,
            });
            let resp = client
                .post(format!("{base}/v1/chat/completions"))
                .header("authorization", format!("Bearer {k}"))
                .json(&body)
                .send()
                .await;
            let ms = t0.elapsed().as_millis() as u64;
            chat = match resp {
                Ok(r) => {
                    let code = r.status();
                    let text = r.text().await.unwrap_or_default();
                    if code.is_success() {
                        let content = serde_json::from_str::<Value>(&text)
                            .ok()
                            .and_then(|v| {
                                v.pointer("/choices/0/message/content")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            })
                            .unwrap_or_default();
                        json!({
                            "ok": true, "ms": ms, "content": content.chars().take(120).collect::<String>(),
                            "model": target,
                        })
                    } else {
                        json!({
                            "ok": false, "ms": ms, "status": code.as_u16(),
                            "error": text.chars().take(300).collect::<String>(),
                        })
                    }
                }
                Err(e) => json!({ "ok": false, "ms": ms, "error": format!("请求失败：{e}") }),
            };
            steps.push(json!({
                "id": "chat",
                "label": "真发一次对话",
                "ok": chat.get("ok").and_then(Value::as_bool).unwrap_or(false),
                "detail": match chat.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                    true => format!("{} ms 收到回复", ms),
                    false => "失败（见下方原文）".to_string(),
                },
            }));
        } else {
            steps.push(json!({
                "id": "chat", "label": "真发一次对话", "ok": false,
                "detail": "网关没在跑或没有模型，跳过",
            }));
        }
    }

    json!({
        "ok": steps.iter().all(|s| s.get("ok").and_then(Value::as_bool).unwrap_or(false)),
        "steps": steps,
        "models": { "count": models.len(), "sample": models.iter().take(12).cloned().collect::<Vec<_>>() },
        "chat": chat,
        "status": st,
        "checkedAt": chrono::Utc::now().timestamp_millis(),
    })
}

/// 起服务（幂等）。必须在一个 tokio 运行时里调用。
// ---------------------------------------------------------------- 开关

/// 积分共享总开关（持久化在 `~/.twin-switch/gateway.json` 的 `enabled`）。
///
/// 关掉之后：网关不再监听端口、控制台也不提供对话能力 —— 用户明确要求
/// 「不启动的状态下网关等都可以关闭」，所以这是**真的停服务**，不是只藏入口。
pub fn enabled() -> bool {
    let path = crate::modules::config::store_dir().join("gateway.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("enabled").and_then(Value::as_bool))
        .unwrap_or(true) // 老用户默认开（升级前后行为一致）
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    let dir = crate::modules::config::store_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let path = dir.join("gateway.json");
    let body = serde_json::json!({ "enabled": on });
    std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap_or_default())
        .map_err(|e| format!("写 {} 失败：{e}", path.display()))
}

/// 停止信号（`serve` 收到后优雅退出）。
fn shutdown_tx() -> &'static std::sync::Mutex<Option<tokio::sync::watch::Sender<bool>>> {
    static T: std::sync::OnceLock<std::sync::Mutex<Option<tokio::sync::watch::Sender<bool>>>> =
        std::sync::OnceLock::new();
    T.get_or_init(|| std::sync::Mutex::new(None))
}

/// 请求网关退出（同步返回，实际退出是异步的；端口释放后再 `serve` 可重启）。
pub fn request_stop() {
    if let Ok(guard) = shutdown_tx().lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(true);
        }
    }
}

pub async fn serve(port: u16) -> Result<(), String> {
    if STARTED_AT.load(Ordering::Relaxed) > 0 {
        return Ok(());
    }
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("端口 {port} 绑定失败：{e}（可能被占用）"))?;

    STARTED_AT.store(crate::modules::qoder_accounts::now_ms(), Ordering::Relaxed);

    // 后台刷一次额度：池子按「谁先过期用谁」排序，靠的就是这份数据。
    // 失败不抛（可能没网 / 没登录态），池子里该账号就继续显示「未知」。
    tokio::spawn(async move {
        let wb = crate::modules::workbuddy_credits::query_all(false).await;
        // core 没有 tracing 依赖，用最朴素的输出（只在启动时打一行）。
        if let Some(summary) = wb.get("summary") {
            eprintln!("[gateway] 额度刷新（WorkBuddy）：{summary}");
        }
        let _ = crate::modules::trae_credits::query(None, false).await;
    });
    let app = build_router();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    if let Ok(mut guard) = shutdown_tx().lock() {
        *guard = Some(tx);
    }
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // 关掉开关时收到 `true` → 优雅退出并释放端口。
            let _ = rx.changed().await;
        })
        .await
        .map_err(|e| format!("网关退出：{e}"));
    STARTED_AT.store(0, Ordering::Relaxed);
    if let Ok(mut guard) = shutdown_tx().lock() {
        *guard = None;
    }
    result
}

fn build_router() -> Router {
    // 两组路径都挂（带 /v1 前缀与不带）：不同客户端对 base_url 的拼接习惯不同
    // —— 有的拼 `{base}/models`，有的拼 `{base}/v1/models`，只挂一组就 404。
    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/models", get(models))
        .route("/chat/completions", post(chat))
        // ⚠️ WorkBuddy 的「接口地址」语义是**完整的对话端点**：用户填什么它就 POST 到什么
        // （抓包日志实测 `POST /v1 -> 404`）。所以把可能被当作端点的路径都兜底挂上，
        // 用户填 `…/v1`、`…/v1/chat/completions`、甚至 `…` 都能通。
        .route("/v1", post(chat))
        .route("/", post(chat));
    api.layer(axum::middleware::from_fn(cors))
}

/// CORS：WorkBuddy / Trae 客户端的「测试连接」是**渲染进程（Chromium）发的 fetch**，
/// 跨源请求没有 `Access-Control-Allow-*` 会被浏览器直接拦掉 —— 界面表现为「连接失败」，
/// 但网关日志里连请求都没有。本机网关只绑 127.0.0.1，放行所有来源没有风险。
/// 另外预检（OPTIONS）必须自己答 200，否则 POST 之前就死。
async fn cors(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let wants_preflight = req.method() == axum::http::Method::OPTIONS;
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let mut res = if wants_preflight {
        axum::http::Response::builder()
            .status(axum::http::StatusCode::OK)
            .body(axum::body::Body::empty())
            .expect("静态预检响应不会失败")
    } else {
        next.run(req).await
    };
    // 请求日志：排查「测试连接」这类客户端问题全靠它 —— 它发了什么、我们答了什么状态。
    {
        let status = res.status().as_u16();
        let line = format!(
            "{} [gateway] {} {} -> {}\n",
            chrono::Local::now().format("%m-%d %H:%M:%S%.3f"),
            method,
            path,
            status
        );
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let mut p = std::path::PathBuf::from(home);
            p.push(".twin-switch");
            let _ = std::fs::create_dir_all(&p);
            p.push("gateway-http.log");
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&p)
                .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        }
    }
    let h = res.headers_mut();
    h.insert("Access-Control-Allow-Origin", "*".parse().expect("静态头"));
    h.insert("Access-Control-Allow-Headers", "*".parse().expect("静态头"));
    h.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().expect("静态头"),
    );
    res
}

async fn healthz() -> impl IntoResponse {
    let mut body = status();
    let pool = gateway_pool::snapshot();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("pool_total".into(), json!(pool.len()));
        obj.insert(
            "pool_usable".into(),
            json!(pool.iter().filter(|e| e.usable).count()),
        );
        // 前三条（已按「最紧迫」排序）——用于自查「池子到底读到额度没有」。
        let head: Vec<Value> = pool
            .iter()
            .take(3)
            .map(|e| {
                json!({
                    "scope": e.scope,
                    "label": e.label,
                    "remaining": e.remaining,
                    "expires_at": e.expires_at,
                    "usable": e.usable,
                })
            })
            .collect();
        obj.insert("pool_head".into(), json!(head));
    }
    Json(body)
}

async fn models() -> impl IntoResponse {
    let catalog = gateway_pool::catalog();
    let mut seen: Vec<String> = Vec::new();
    for (_, list) in &catalog {
        for m in list {
            if !seen.contains(m) {
                seen.push(m.clone());
            }
        }
    }
    let data: Vec<Value> = seen
        .iter()
        .map(|m| json!({ "id": m, "object": "model", "owned_by": "twin-switch" }))
        .collect();
    (
        StatusCode::OK,
        Json(json!({ "object": "list", "data": data })),
    )
}

/// 取请求里的 Bearer。
fn bearer(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("authorization")?.to_str().ok()?.trim();
    // 认证方案名按 RFC 7235 是**大小写不敏感**的（`Bearer` / `bearer` / `BEARER` 都合法）。
    let token = match raw.split_once(' ') {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => rest.trim(),
        Some(_) => return None,
        None => return None,
    };
    (!token.is_empty()).then(|| token.to_string())
}

fn api_err(code: StatusCode, kind: &str, msg: impl Into<String>) -> axum::response::Response {
    (code, Json(json!({ "error": { "type": kind, "message": msg.into() } }))).into_response()
}

async fn chat(
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> axum::response::Response {
    // ① Key
    let Some(token) = bearer(&headers) else {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return api_err(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            format!("缺少 Authorization 头。客户端里填本机网关 Key（在「积分共享」页新建）"),
        );
    };
    let key = match gateway_keys::verify(&token) {
        Ok(Some(k)) => k,
        Ok(None) => {
            REJECTED.fetch_add(1, Ordering::Relaxed);
            return api_err(StatusCode::UNAUTHORIZED, "authentication_error", "网关 Key 无效或已吊销");
        }
        Err(e) => {
            return api_err(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", e);
        }
    };

    let Some(Json(req)) = body else {
        return api_err(StatusCode::BAD_REQUEST, "invalid_request_error", "请求体不是合法 JSON");
    };
    let model = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // ② 模型名 → 候选作用域
    let (bare, scopes) = match gateway_pool::resolve_scopes(&model) {
        Ok(v) => v,
        Err(e) => {
            REJECTED.fetch_add(1, Ordering::Relaxed);
            return api_err(StatusCode::BAD_REQUEST, "invalid_request_error", e);
        }
    };

    // ③ Key 的授权范围
    if !scopes.iter().any(|s| key.allows(s)) {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return api_err(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("这把 Key 没有 {} 的授权", scopes.join(" / ")),
        );
    }

    // ④ 选号（最紧迫优先）
    let sel = match gateway_pool::preview(&model) {
        Ok(s) => s,
        Err(e) => return api_err(StatusCode::BAD_REQUEST, "invalid_request_error", e),
    };
    let Some(picked) = sel.picked.clone() else {
        return api_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_account",
            sel.reason.unwrap_or_else(|| "没有可用账号".into()),
        );
    };
    if !key.allows(&picked.scope) {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return api_err(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("选号落到了 {}，但这把 Key 没有它的授权", picked.scope),
        );
    }

    // ⑤ 上游透传。
    //
    // 客户端要流式时，我们**仍然向上游要完整结果**，再把结果包装成一段 SSE 发回去。
    // 理由：两家上游的流格式各不相同（Trae 是私有事件流、WorkBuddy 是 OpenAI SSE），
    // 逐字转发需要两套解析；先把「能不能用」保证住，逐字流式留作后续优化。
    let want_stream = req.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let mut upstream_req = req.clone();
    if want_stream {
        if let Some(obj) = upstream_req.as_object_mut() {
            obj.insert("stream".into(), json!(false));
        }
    }

    match forward(&picked.scope, &bare, &upstream_req).await {
        Ok(resp) => {
            REQUESTS.fetch_add(1, Ordering::Relaxed);
            gateway_keys::touch(&key.id);
            if want_stream {
                sse_from_completion(&resp)
            } else {
                Json(resp).into_response()
            }
        }
        Err((code, msg)) => {
            // 上游报错 → 反馈给池子（冷却/标记），下一个请求自动换号 —— 不然还会撞同一个。
            let status = code.as_u16();
            if let Some((until, kind)) = gateway_pool::apply_error(&picked.key, status, &msg) {
                let left = (until - gateway_pool::now_ms_pub()).max(0) / 1000;
                let _ = gateway_pool::mark_last_error(&picked.key, &msg, kind, left);
            }
            api_err(code, "upstream_error", msg)
        }
    }
}

/// 把一条完整结果包装成 OpenAI 流式事件。
///
/// 发三个事件：正文 chunk → 结束 chunk → `[DONE]`。
/// 客户端（包括 Trae 的「测试并添加」）看到的是标准 SSE，不会因为
/// `stream:true` 拿不到流而判定失败。
///
/// ⚠️ 这不是「逐字流式」：内容一次性到达。真流式要按上游分别解析，后续再做。
fn sse_from_completion(v: &Value) -> axum::response::Response {
    use axum::http::header;

    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model = v.get("model").cloned().unwrap_or_else(|| json!("twin-switch"));
    let id = v
        .get("id")
        .cloned()
        .unwrap_or_else(|| json!("chatcmpl-twinswitch"));
    let created = v
        .get("created")
        .cloned()
        .unwrap_or_else(|| json!(crate::modules::qoder_accounts::now_ms() / 1000));

    let first = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": content },
            "finish_reason": Value::Null,
        }],
    });
    let last = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
    });

    let body = format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n");
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// 上游配置。
#[derive(Debug, Clone)]
struct UpstreamSpec {
    base_url: String,
    chat_path: String,
    /// 上游只接受流式（WorkBuddy 的 `/v2/chat/completions` 就是这样：
    /// 非流式会被回 `11101 Non-stream chat request is currently not supported`）。
    force_stream: bool,
    /// 把模型名改写成上游认识的名字（可选）。
    model_override: Option<String>,
}

impl UpstreamSpec {
    fn from_json(v: &Value) -> Option<Self> {
        let base = v.get("base_url").and_then(Value::as_str)?.trim_end_matches('/').to_string();
        if base.is_empty() {
            return None;
        }
        Some(Self {
            base_url: base,
            chat_path: v
                .get("chat_path")
                .and_then(Value::as_str)
                .unwrap_or("/v1/chat/completions")
                .to_string(),
            force_stream: v.get("force_stream").and_then(Value::as_bool).unwrap_or(false),
            model_override: v.get("model").and_then(Value::as_str).map(str::to_string),
        })
    }
}

/// 内置默认上游 —— **只写实测过的**，没测过的一律要求用户自己登记。
///
/// WorkBuddy 的对话端点是 `https://www.codebuddy.cn/v2/chat/completions`
/// （实测：`/v1/...` 一律 404；非流式回 11101；bearer 直接用账号 access_token）。
/// 所以 `wb-cn` 不需要用户配任何东西就能用。
fn builtin_spec(scope: &str) -> Option<UpstreamSpec> {
    match scope {
        "wb-cn" => Some(UpstreamSpec {
            base_url: "https://www.codebuddy.cn".into(),
            chat_path: "/v2/chat/completions".into(),
            force_stream: true,
            model_override: None,
        }),
        _ => None,
    }
}

/// 读配置并转发。
///
/// 优先用 `~/.twin-switch/upstreams.json` 里登记的；没登记时回落到 [`builtin_spec`]，
/// 两者都没有才 501（并说明这家为什么还需要适配器）。
async fn forward(
    scope: &str,
    model: &str,
    req: &Value,
) -> Result<Value, (StatusCode, String)> {
    let path = crate::modules::config::store_dir().join("upstreams.json");
    let spec = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get(scope).cloned())
        .and_then(|v| UpstreamSpec::from_json(&v))
        .or_else(|| builtin_spec(scope));

    // Trae 走私有协议（设备 jwt + llm_utils_chat 专用请求体），不进通用透传。
    if scope.starts_with("trae") && spec.is_none() {
        return trae_forward(model, req).await;
    }

    // Qoder 走 COSY 签名的私有协议（**信封 SSE**）——2026-10-22 真机跑通。
    if scope.starts_with("qoder") && spec.is_none() {
        return qoder_forward(scope, model, req).await;
    }

    let Some(spec) = spec else {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            format!(
                "{scope} 还没有可用的对话上游。可以：\n\
                 ① 在 {file} 里登记一个 OpenAI 兼容上游（会真的走它，不消耗本机账号额度）；\n\
                 ② Trae / Qoder 走的是各家私有协议（Trae: trae-api-cn.mchost.guru/api/agent/v3/llm_utils_chat，\
                 需设备私钥签 JWT；Qoder 需 COSY 签名），适配器还没接完。\n\
                 WorkBuddy 是 OpenAI 兼容的，用 wb-cn 作用域可以直接跑。",
                file = path.display()
            ),
        ));
    };

    let token = token_of(scope).ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            format!("{scope} 找不到可用凭据（先在账号页登录或填 PAT）"),
        )
    })?;

    let upstream_model = spec.model_override.clone().unwrap_or_else(|| model.to_string());
    let mut body = req.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".into(), json!(upstream_model));
        if spec.force_stream {
            // 上游只吃流式；这里强制打开，收完再聚合成非流式回给客户端。
            obj.insert("stream".into(), json!(true));
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("构造 HTTP 客户端失败：{e}")))?;
    let resp = client
        .post(format!("{}{}", spec.base_url, spec.chat_path))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .header("accept", if spec.force_stream { "text/event-stream" } else { "application/json" })
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("传输失败（可能是网络抖动，可重试）：{e}"),
            )
        })?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => "（上游鉴权失败：token 可能已过期，需重新登录该账号）",
            _ => "",
        };
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "上游返回 HTTP {}{hint}：{}",
                status.as_u16(),
                explain_upstream_error(&text)
            ),
        ));
    }

    if spec.force_stream {
        return aggregate_sse(&text, model);
    }
    serde_json::from_str::<Value>(&text).map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("上游返回的不是 JSON（{e}）：{}", text.chars().take(200).collect::<String>()),
        )
    })
}

/// 把上游的业务错误码翻成人能懂的句子（原文一并带上，便于排查）。
fn explain_upstream_error(text: &str) -> String {
    let snippet: String = text.chars().take(320).collect();
    if text.contains("11101") {
        return format!("上游只接受流式请求（11101），本次未开启聚合。原始返回：{snippet}");
    }
    if text.contains("11134") || text.contains("20095") {
        return format!(
            "上游不认识这个模型名，或该模型当前不可用（11134）。请在模型页把模型勾到对应作用域，             或换一个该产品确实提供的模型。原始返回：{snippet}"
        );
    }
    if text.trim_start().starts_with('{') {
        return snippet;
    }
    snippet
}

/// 把 SSE 文本聚合成一条非流式 OpenAI 响应。
///
/// 只做「拼 delta.content」这一件事：网关对外是非流式，客户端拿到的是完整回答。
fn aggregate_sse(text: &str, model: &str) -> Result<Value, (StatusCode, String)> {
    let mut content = String::new();
    let mut role = String::from("assistant");
    let mut finish: Option<String> = None;
    let mut id = String::from("chatcmpl-twinswitch");
    let mut usage = Value::Null;

    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() || rest == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(rest) else {
            continue;
        };
        if let Some(i) = v.get("id").and_then(Value::as_str) {
            id = i.to_string();
        }
        if let Some(u) = v.get("usage") {
            if !u.is_null() {
                usage = u.clone();
            }
        }
        let Some(ch) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            continue;
        };
        if let Some(f) = ch.get("finish_reason").and_then(Value::as_str) {
            finish = Some(f.to_string());
        }
        if let Some(delta) = ch.get("delta") {
            if let Some(r) = delta.get("role").and_then(Value::as_str) {
                role = r.to_string();
            }
            if let Some(c) = delta.get("content").and_then(Value::as_str) {
                content.push_str(c);
            }
        }
    }

    if content.is_empty() {
        // 上游「成功」但一个字符都没给 —— 不许假装成空回答，要说清楚。
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "上游返回了流但没有任何文本内容（可能是被安全策略拦了）。原始前 300 字：{}",
                text.chars().take(300).collect::<String>()
            ),
        ));
    }

    Ok(json!({
        "id": id,
        "object": "chat.completion",
        "created": crate::modules::qoder_accounts::now_ms() / 1000,
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": role, "content": content },
            "finish_reason": finish.unwrap_or_else(|| "stop".into()),
        }],
        "usage": usage,
    }))
}

// ---------------------------------------------------------------------------
// Trae 适配器（照参考实现 `buddy-switch-gateway/src/trae`）
// ---------------------------------------------------------------------------

/// Trae 对话上游（消耗 IDE 积分）。
const TRAE_AGENT_HOST: &str = "https://trae-api-cn.mchost.guru";
const TRAE_LLM_CHAT_PATH: &str = "/api/agent/v3/llm_utils_chat";
const TRAE_APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
const TRAE_IDE_VERSION: &str = "0.1.50";
const TRAE_IDE_VERSION_CODE: &str = "20260811";
const TRAE_USER_AGENT: &str = "TraeClient/TTNet";
/// TraeWork 的 chat 分组（上游按 `function` 做白名单）。
const TRAE_FUNCTION: &str = "solo_work_lite";

/// 一个 Trae 账号的对话凭据。
struct TraeCred {
    uid: String,
    jwt: String,
    device_id: String,
    machine_id: String,
}

/// 从 vault 里找一个可用账号（`uid_hint` 为 None 时取第一个能解开的）。
///
/// token 在 `User/globalStorage/storage.json` 的 `iCubeAuthInfo://icube.cloudide`，
/// 是 Km 加密串 —— 用 0.3.4 现成的 [`trae_km::decrypt_km_json`] 解开取 `token`。
fn trae_cred(uid_hint: Option<&str>) -> Option<TraeCred> {
    let vault = crate::modules::config::store_dir().join("trae").join("vault");
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for client in ["trae-cn", "solo-cn"] {
        let Ok(rd) = std::fs::read_dir(vault.join(client)) else {
            continue;
        };
        for e in rd.flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }

    for dir in dirs {
        let meta = read_json_at(&dir.join("meta.json"));
        let oauth = read_json_at(&dir.join("oauth.json"));
        let uid = meta
            .as_ref()
            .and_then(|v| v.get("verified_uid"))
            .and_then(|v| v.as_i64().map(|n| n.to_string()))
            .or_else(|| {
                oauth
                    .as_ref()
                    .and_then(|v| v.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if let Some(hint) = uid_hint {
            if uid != hint {
                continue;
            }
        }

        let Some(storage) = read_json_at(&dir.join("User").join("globalStorage").join("storage.json"))
        else {
            continue;
        };
        let Some(enc) = storage
            .get("iCubeAuthInfo://icube.cloudide")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(auth) = crate::modules::trae_km::decrypt_km_json(enc) else {
            continue;
        };
        let Some(token) = auth.get("token").and_then(Value::as_str) else {
            continue;
        };
        let jwt = crate::modules::trae_km::strip_cloud_prefix(token).to_string();
        if jwt.is_empty() {
            continue;
        }

        let device_id = oauth
            .as_ref()
            .and_then(|v| v.get("deviceId"))
            .and_then(|v| v.as_i64().map(|n| n.to_string()))
            .unwrap_or_default();
        let machine_id = oauth
            .as_ref()
            .and_then(|v| v.get("machineId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        return Some(TraeCred { uid, jwt, device_id, machine_id });
    }
    None
}

fn read_json_at(path: &std::path::Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 模型名 → (config_name, model_name)。与参考实现的 [`model_config`] 同形。
fn trae_model_config(model: &str) -> (String, String) {
    let name = model.trim();
    let name = if name.is_empty() { "deepseek-v4-flash" } else { name };
    let known = match name.to_lowercase().as_str() {
        "deepseek-v4-flash" => Some(("DeepSeek-V4-Flash", "deepseek_v4_flash__dev")),
        "deepseek-v4-flash-official" => {
            Some(("DeepSeek-V4-Flash-Official", "DeepSeek-V4-Flash-Official__dev"))
        }
        "deepseek-v4-pro" => Some(("DeepSeek-V4-Pro", "deepseek_v4_pro__dev")),
        "glm-5.2" => Some(("glm-5.2", "glm-5.2__dev")),
        "glm-5.3" => Some(("glm-5.3", "glm-5.3__dev")),
        "doubao-seed-2.1-pro" | "seed-code-pro-0430" => {
            Some(("Doubao-Seed-2.1-Pro", "Doubao-Seed-2.1-Pro__dev"))
        }
        "doubao-seed-2.1-turbo" => Some(("Doubao-Seed-2.1-Turbo", "Doubao-Seed-2.1-Turbo__dev")),
        "kimi-k2.7-code" => Some(("kimi-k2.7-code", "kimi-k2.7-code__dev")),
        "minimax-m3" => Some(("minimax-m3", "minimax-m3__dev")),
        _ => None,
    };
    match known {
        Some((c, m)) => (c.to_string(), m.to_string()),
        None => (name.to_string(), format!("{name}__dev")),
    }
}

/// Trae 对话：专用请求体 + 私有 SSE。
/// Qoder 转发：COSY 签名 + 信封 SSE 聚合。
///
/// 账号取自 Qoder 账号库（两个区域各自一份），模型名就是 `/v1/models` 里那套代号
/// （`qmodel` / `dmodel` / `gmodel` …）。真机已验证：正文与 usage 都能取到。
async fn qoder_forward(
    scope: &str,
    model: &str,
    req: &Value,
) -> Result<Value, (StatusCode, String)> {
    use crate::modules::qoder_realm::QoderRegion;

    let region = if scope.ends_with("global") {
        QoderRegion::Global
    } else {
        QoderRegion::Cn
    };
    let accounts = crate::modules::qoder_accounts::list(region)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("读 Qoder 账号失败：{e}")))?;
    let acc = accounts
        .iter()
        .find(|a| a.is_usable())
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("{} 没有可用的 Qoder 账号（先在账号页导入/登录）", region.display_name()),
            )
        })?;

    let messages = req.get("messages").cloned().unwrap_or_else(|| json!([]));
    // display_name 直接用模型 key：可读性一般，但上游只认 key。
    crate::modules::qoder_tools::chat_complete(region, acc, model, model, &messages)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Qoder 上游：{e}")))
}

async fn trae_forward(model: &str, req: &Value) -> Result<Value, (StatusCode, String)> {
    let cred = trae_cred(None).ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            "找不到可用的 Trae 登录态（storage.json 里没有解不开的授权条目）".to_string(),
        )
    })?;

    let (config_name, model_name) = trae_model_config(model);
    let mut body = req.clone();
    if let Some(obj) = body.as_object_mut() {
        // 上游要求 `messages[].content` 是数组（字符串形态会回 4001）。
        normalize_trae_messages(obj);
        // 上游要求 `tools[].function.parameters` 是字符串（OpenAI 标准是对象，
        // WorkBuddy 发对象过来不转就 400：cannot unmarshal object into ... parameters of type string）。
        normalize_trae_tools(obj);
        obj.insert("config_name".into(), json!(config_name));
        obj.insert("model_name".into(), json!(model_name));
        // 上游只支持流式；本地聚合。
        obj.insert("stream".into(), json!(true));
        obj.insert("function".into(), json!(TRAE_FUNCTION));
        obj.insert("user_id".into(), json!(cred.uid));
        obj.insert("device_id".into(), json!(cred.device_id));
        obj.insert("machine_id".into(), json!(cred.machine_id));
        obj.insert("conversation_id".into(), json!(uuid::Uuid::new_v4().to_string()));
        obj.insert("session_id".into(), json!(uuid::Uuid::new_v4().to_string()));
        obj.insert("project_id".into(), json!(uuid::Uuid::new_v4().to_string()));
        obj.insert("workspace_id".into(), json!("e04cdd"));
        obj.insert("prompt_max_tokens".into(), json!(168_000));
        obj.insert("mode".into(), json!("FunctionCall"));
        obj.insert("ide_version".into(), json!(TRAE_IDE_VERSION));
        obj.insert("ide_version_code".into(), json!(TRAE_IDE_VERSION_CODE));
        obj.insert("app_id".into(), json!(TRAE_APP_ID));
        obj.insert("package_type".into(), json!("stable_cn"));
    }

    let url = format!("{TRAE_AGENT_HOST}{TRAE_LLM_CHAT_PATH}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("构造客户端失败：{e}")))?;
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "*/*")
        .header("accept-encoding", "identity")
        .header("user-agent", TRAE_USER_AGENT)
        .header("referer", &url)
        .header("x-ide-token", &cred.jwt)
        .header("x-app-id", TRAE_APP_ID)
        .header("x-app-version", "default")
        .header("x-app-version-code", TRAE_IDE_VERSION_CODE)
        .header("x-ide-version", TRAE_IDE_VERSION)
        .header("x-ide-version-code", TRAE_IDE_VERSION_CODE)
        .header("x-ide-version-type", "stable")
        .header("x-device-type", "windows")
        .header("x-device-brand", "CREFG-XX")
        .header("x-device-cpu", "Intel")
        .header("x-device-id", &cred.device_id)
        .header("x-machine-id", &cred.machine_id)
        .header("x-os-version", "Windows 11 Home China")
        .header("request-traffic-type", "prod")
        .header("package-type", "stable_cn")
        .header("x-lgw-req-sdk-type", "3")
        .header("x-lscbd-aid", "787976")
        .json(&body)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("请求 Trae 失败：{e}")))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "Trae 上游返回 HTTP {}：{}",
                status.as_u16(),
                text.chars().take(400).collect::<String>()
            ),
        ));
    }

    aggregate_trae_sse(&text, model)
}

/// `messages[].content` 字符串 → `[{ "type": "text", "text": … }]`。
///
/// 上游的 Go 结构体是 `[]LLMRawMessageContent`，传字符串直接 4001
/// （实测错误：`cannot unmarshal string into … content of type []*idecopilot.LLMRawMessageContent`）。
/// Trae 的 Go 结构体要求 `tools[].function.parameters` 是**字符串**（序列化后的 JSON）。
/// OpenAI 标准里它是对象；WorkBuddy 按标准发对象过来，不转就 400
/// （实测：`cannot unmarshal object into Go struct field FunctionDefinition.tools.function.parameters of type string`）。
fn normalize_trae_tools(obj: &mut serde_json::Map<String, Value>) {
    let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools.iter_mut() {
        let Some(func) = tool.get_mut("function") else { continue };
        let Some(params) = func.get("parameters") else { continue };
        if params.is_string() {
            continue;
        }
        let serialized = serde_json::to_string(params).unwrap_or_default();
        if let Some(f) = func.as_object_mut() {
            f.insert("parameters".into(), Value::String(serialized));
        }
    }
}

fn normalize_trae_messages(obj: &mut serde_json::Map<String, Value>) {
    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for m in messages.iter_mut() {
        let Some(m) = m.as_object_mut() else { continue };
        if let Some(text) = m.get("content").and_then(Value::as_str) {
            let text = text.to_string();
            m.insert("content".into(), json!([{ "type": "text", "text": text }]));
        }
    }
}

/// 拼 Trae 私有事件流里的文本。
///
/// 事件格式（照参考实现 `trae/sse.rs` 实测口径，每行带 `id:N` 前缀）：
///
/// | 事件 | 字段 | 含义 |
/// | --- | --- | --- |
/// | `output` / `thought` | `response` | **正文增量** |
/// | 同上 | `reasoning_content` | 思维链（不是正文，不能拼进回答） |
/// | `token_usage` | — | 用量 |
/// | `done` | `finish_reason` | 结束原因 |
/// | `error` | `code` / `message` | 上游报错 |
///
/// ⚠️ 正文是 `response`，**不是** `content` —— 用 `content` 会把
/// `{"reasoning_content":"…"}` 整段当成回答吐给用户（真实踩过）。
fn aggregate_trae_sse(text: &str, model: &str) -> Result<Value, (StatusCode, String)> {
    let mut answer = String::new();
    let mut event = String::new();
    let mut finish = String::from("stop");
    let mut usage = Value::Null;
    let mut error: Option<String> = None;

    for raw in text.lines() {
        let line = raw.trim();
        // 有的版本每行前面带 `id:N `。
        let line = line
            .strip_prefix("id:")
            .and_then(|rest| rest.split_once(' ').map(|(_, l)| l.trim()))
            .unwrap_or(line);

        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = rest.trim().to_string();
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };

        match event.as_str() {
            "output" | "thought" => {
                if let Some(t) = v.get("response").and_then(Value::as_str) {
                    answer.push_str(t);
                } else if let Some(t) = v.get("thought").and_then(Value::as_str) {
                    answer.push_str(t);
                }
                // `reasoning_content` 单独存在，**不拼进回答**。
            }
            "token_usage" => usage = v.clone(),
            "done" => {
                if let Some(f) = v.get("finish_reason").and_then(Value::as_str) {
                    finish = f.to_string();
                }
            }
            "turn_completion" => finish = "stop".into(),
            "error" => {
                let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
                let msg = v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("上游返回了未描述的错误");
                error = Some(format!("code {code}: {msg}"));
            }
            _ => {}
        }
    }

    if let Some(err) = error {
        return Err((StatusCode::BAD_GATEWAY, format!("Trae 上游报错：{err}")));
    }
    if answer.trim().is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "Trae 返回了事件流但没有正文（只可能是空回答或被拦）。原始前 500 字：{}",
                text.chars().take(500).collect::<String>()
            ),
        ));
    }

    Ok(json!({
        "id": format!("chatcmpl-trae-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion",
        "created": crate::modules::qoder_accounts::now_ms() / 1000,
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": answer },
            "finish_reason": finish
        }],
        "usage": usage,
    }))
}

/// 按作用域取一个可用的 bearer token。
///
/// ⚠️ Trae 用设备私钥签 JWT，不是 bearer token —— 这里如实返回「暂不支持」，
/// 而不是伪造一个空 token 去撞上游。
fn token_of(scope: &str) -> Option<String> {
    match scope {
        "qoder-cn" | "qoder-global" => {
            let list = crate::modules::qoder_accounts::list_all().ok()?;
            list.into_iter()
                .filter(|a| format!("qoder-{}", a.region.as_str()) == scope && a.is_usable())
                .find_map(|a| a.access_token)
        }
        "wb-cn" | "wb-global" => {
            let text = std::fs::read_to_string(
                crate::modules::config::store_dir().join("workbuddy-accounts.json"),
            )
            .ok()?;
            let v: Value = serde_json::from_str(&text).ok()?;
            let want_cn = scope == "wb-cn";
            v.get("accounts")?.as_array()?.iter().find_map(|a| {
                let domain = a.get("domain").and_then(Value::as_str).unwrap_or_default();
                if domain.contains("cn") != want_cn {
                    return None;
                }
                a.get("access_token")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_string)
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reports_not_running_before_start() {
        let s = status();
        assert_eq!(s["running"], false);
        assert_eq!(s["port"], GATEWAY_PORT);
    }

    #[test]
    fn bearer_parsing_trims_and_rejects_empty() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert("authorization", "Bearer sk-ts-abc".parse().unwrap());
        assert_eq!(bearer(&h).as_deref(), Some("sk-ts-abc"));
        h.insert("authorization", "bearer   ".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn trae_has_no_bearer_token() {
        assert_eq!(token_of("trae-cn"), None, "Trae 需要设备私钥签名，不应伪造 token");
    }
}

#[cfg(test)]
mod selftest_probe {
    use super::*;

    /// 真机自检：跑一次使用说明页的检测（不发对话，不消耗额度）。
    #[tokio::test]
    #[ignore]
    async fn probe_guide_selftest() {
        let v = selftest(None, None).await;
        eprintln!("ok={}", v.get("ok").and_then(Value::as_bool).unwrap_or(false));
        if let Some(steps) = v.get("steps").and_then(Value::as_array) {
            for s in steps {
                eprintln!(
                    "  [{}] {} — {}",
                    if s.get("ok").and_then(Value::as_bool).unwrap_or(false) { "✓" } else { "✗" },
                    s.get("label").and_then(Value::as_str).unwrap_or(""),
                    s.get("detail").and_then(Value::as_str).unwrap_or("")
                );
            }
        }
        eprintln!(
            "模型数={:?}",
            v.pointer("/models/count").and_then(Value::as_u64)
        );
    }
}
