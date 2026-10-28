//! 积分共享 · 本机网关（默认 `127.0.0.1:57892`，可选局域网 `0.0.0.0:57892`）。
//!
//! 对外只暴露 OpenAI 兼容的三个端点：
//!
//! - `GET  /healthz` —— 网关状态（界面用）
//! - `GET  /v1/models` —— 模型清单（来自 `~/.twin-switch/models.json`）
//! - `POST /v1/chat/completions` —— 选号后透传给对应产品的上游
//!
//! 四条纪律：
//!
//! 1. **默认只绑 127.0.0.1**。网关能代替你花掉账号额度，绑 0.0.0.0 等于借给整个局域网
//!    —— 所以开成 `0.0.0.0` 必须是用户**显式勾选**（`lan` 开关），且非回环来源
//!    一律强制 Key（`/healthz` 除外，它会把账号细节收敛掉）。
//! 2. **没配上游就 501，不假装成功**；鉴权失败单独说，不和网络错误混在一起。
//! 3. **一次失败不许把数字说成成功**：请求数只统计真正打通上游的次数。
//! 4. **「没绑定」与「绑定失败」不许长得一样**：绑定失败的原因要能从 /healthz 与界面读到。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::modules::gateway_keys::{self, GATEWAY_PORT};
use crate::modules::gateway_metrics::Trace;
use crate::modules::gateway_pool;
use crate::modules::gateway_stream::{Delta, Emitter, Kind, StreamReader};

static REQUESTS: AtomicU64 = AtomicU64::new(0);
static REJECTED: AtomicU64 = AtomicU64::new(0);
static STARTED_AT: AtomicI64 = AtomicI64::new(0);
/// **实际生效**的绑定模式（不是配置值）：只有 `serve` 真绑上 `0.0.0.0` 才为 true。
static LAN_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 最近一次绑定失败的原因。空 = 没失败过。
///
/// ⚠️ 存在的理由：`serve` 失败只 `eprintln!` 的话，界面会显示「未启动」，
/// 和用户主动关掉开关长得一模一样 —— 必须能区分「关了」与「起不来」。
fn last_error_slot() -> &'static std::sync::Mutex<Option<String>> {
    static S: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(None))
}

fn set_last_error(msg: Option<String>) {
    if let Ok(mut g) = last_error_slot().lock() {
        *g = msg;
    }
}

pub fn last_error() -> Option<String> {
    last_error_slot().lock().ok().and_then(|g| g.clone())
}

/// 本机对外的局域网 IPv4（没有就 `None`，**不编造**）。
///
/// 用 `UDP connect` 让系统按路由表挑出口网卡 —— UDP 不握手、不发包，纯粹查路由。
/// 多个目标是为了没有默认路由时也能拿到（纯局域网环境）。全部失败就老实返回 `None`。
pub fn lan_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    for target in ["8.8.8.8:80", "223.5.5.5:80", "192.168.1.1:9"] {
        if sock.connect(target).is_err() {
            continue;
        }
        let Ok(addr) = sock.local_addr() else { continue };
        let ip = addr.ip();
        // 回环 / 未指定 都不算「局域网地址」
        if ip.is_loopback() || ip.is_unspecified() {
            continue;
        }
        return Some(ip.to_string());
    }
    None
}

/// 对外地址：`http://<host>:<port>/v1`。
fn base_url(host: &str) -> String {
    format!("http://{host}:{GATEWAY_PORT}/v1")
}

pub fn status() -> Value {
    let started = STARTED_AT.load(Ordering::Relaxed);
    let lan = LAN_ACTIVE.load(Ordering::Relaxed);
    let local = base_url("127.0.0.1");
    // 只有真绑了 0.0.0.0 才给局域网地址；探测不到就 null + 原因，绝不拿 127.0.0.1 冒充。
    let lan_ip = if lan { lan_ip() } else { None };
    let lan_addr = lan_ip.as_deref().map(base_url);
    json!({
        "running": started > 0,
        "port": GATEWAY_PORT,
        "started_at": if started > 0 { Some(started) } else { None },
        "requests": REQUESTS.load(Ordering::Relaxed),
        "rejected": REJECTED.load(Ordering::Relaxed),
        "lan": lan,
        // 配置里的意图值（网关没在跑时 `lan` 恒为 false，但用户的勾选已经落了盘）；
        // 界面靠这两个值区分「没开」与「开了但没起来」。
        "lanPref": lan_enabled(),
        "addr": lan_addr.clone().unwrap_or_else(|| local.clone()),
        "localAddr": local,
        "lanAddr": lan_addr,
        "lanAddrError": if lan && lan_ip.is_none() {
            Some("没有识别到局域网 IPv4 地址（网卡没连上？）".to_string())
        } else {
            None
        },
        "bindError": last_error(),
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
    // 每一条分支都会赋值，所以不需要初始值（`String::new()` 那个初值从来没被读过）。
    let models_detail;
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
    read_flag("enabled").unwrap_or(true) // 老用户默认开（升级前后行为一致）
}

/// 局域网开关（`gateway.json` 的 `lan`）。**默认关**：绑 `0.0.0.0` 等于把额度借给整个局域网，
/// 必须是用户显式勾选，不许因为历史配置缺失就默认放开。
pub fn lan_enabled() -> bool {
    read_flag("lan").unwrap_or(false)
}

fn read_flag(key: &str) -> Option<bool> {
    let path = crate::modules::config::store_dir().join("gateway.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get(key).and_then(Value::as_bool))
}

/// 写一个开关值 —— **合并写**，不是覆盖写。
///
/// ⚠️⚠️ 曾经的写法是 `json!({ "enabled": on })` 整份覆盖：加上 `lan` 之后，
/// 用户点一下总开关就会把 `lan` 抹掉（反之亦然），表现为「设置自己变回去了」。
fn write_flag(key: &str, value: bool) -> Result<(), String> {
    let dir = crate::modules::config::store_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let path = dir.join("gateway.json");
    let body = merge_flag(read_body(&path), key, value);
    std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap_or_default())
        .map_err(|e| format!("写 {} 失败：{e}", path.display()))
}

fn read_body(path: &std::path::Path) -> Value {
    // 读不到 / 不是 JSON 对象时从空对象起步（把坏文件当空处理，不抛）
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// 合并写一个布尔开关：**其余字段原样保留**（这是 `set_enabled` 曾经丢掉 `lan` 的原因）。
fn merge_flag(mut body: Value, key: &str, value: bool) -> Value {
    if let Some(obj) = body.as_object_mut() {
        obj.insert(key.to_string(), json!(value));
    }
    body
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    write_flag("enabled", on)
}

pub fn set_lan(on: bool) -> Result<(), String> {
    write_flag("lan", on)
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

/// 等旧实例真的退出（最多 `timeout_ms`）。返回是否已经停了。
///
/// ⚠️ 不做这个等待就直接 `serve` 会「看起来重启了其实没有」：`serve` 开头
/// `STARTED_AT > 0` 就返回 `Ok`，随后旧实例优雅退出、端口释放 —— 结果没人监听。
async fn wait_stopped(timeout_ms: u64) -> bool {
    let step = 50u64;
    let mut waited = 0u64;
    while STARTED_AT.load(Ordering::Relaxed) > 0 {
        if waited >= timeout_ms {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(step)).await;
        waited += step;
    }
    true
}

/// 换绑定模式 / 改端口后**真的重启**：停旧 → 等端口释放 → 起新。必须在一个 tokio 运行时里调用。
///
/// 这是 `serve` 的推荐入口（`serve` 保留幂等语义，给启动时用）。
pub async fn restart(port: u16) -> Result<(), String> {
    request_stop();
    if !wait_stopped(3000).await {
        let msg = "旧网关实例 3 秒内没退出（可能有正在进行的对话），本次没重新监听；再点一次开关即可";
        set_last_error(Some(msg.to_string()));
        return Err(msg.to_string());
    }
    serve(port).await
}

/// 绑定地址：**局域网开关关着就只绑回环**（默认），开着才绑全网卡。
///
/// 单独抽成函数是为了能被单测钉住 —— 这条判据错了，等于把额度借给整个局域网。
pub fn bind_ip(lan: bool) -> [u8; 4] {
    if lan {
        [0, 0, 0, 0]
    } else {
        [127, 0, 0, 1]
    }
}

pub async fn serve(port: u16) -> Result<(), String> {
    if STARTED_AT.load(Ordering::Relaxed) > 0 {
        return Ok(());
    }
    // 绑定地址由 `lan` 开关决定：默认只听回环。
    let lan = lan_enabled();
    let addr: std::net::SocketAddr = (bind_ip(lan), port).into();
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            let msg = format!(
                "端口 {port} 绑定到 {} 失败：{e}（可能被占用，或被安全软件拦下）",
                addr.ip()
            );
            set_last_error(Some(msg.clone()));
            return Err(msg);
        }
    };
    set_last_error(None);
    LAN_ACTIVE.store(lan, Ordering::Relaxed);
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
    let result = axum::serve(
        listener,
        // 必须带 connect info：`lan_guard` 靠它判断「这个请求是不是从本机来的」。
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        // 关掉开关时收到 `true` → 优雅退出并释放端口。
        let _ = rx.changed().await;
    })
    .await
    .map_err(|e| format!("网关退出：{e}"));
    STARTED_AT.store(0, Ordering::Relaxed);
    LAN_ACTIVE.store(false, Ordering::Relaxed);
    if let Ok(mut guard) = shutdown_tx().lock() {
        *guard = None;
    }
    result
}

/// 请求体上限（字节）。
///
/// ⚠️⚠️ axum 默认 **2 MiB**，超过就直接回 413，且**连 handler 都进不来** —— 客户端里
/// 只看到一行裸文本 `Failed to buffer the request body: length limit exceeded`。
/// 实测踩到：WorkBuddy 让模型分析一份 DXF 转成的 PDF（13551 条路径 / 112 个文本实体），
/// 那一轮的 body 远超 2 MiB，于是「分析这个文档」整条走不通，而网关日志里连请求都没有。
/// 对话类 body 天然会长（历史消息 + 工具结果 + 文件内容），本机只绑 127.0.0.1，
/// 放宽到 64 MiB 即可；真正的瘦身交给 `gateway_stream::prepare()`（发上游前折叠）。
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

fn build_router() -> Router {
    // 两组路径都挂（带 /v1 前缀与不带）：不同客户端对 base_url 的拼接习惯不同
    // —— 有的拼 `{base}/models`，有的拼 `{base}/v1/models`，只挂一组就 404。
    let api = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/models", get(models))
        .route("/chat/completions", post(chat))
        // 余额查询：dsh-pet 这类桌宠把 `{base_url}/user/balance` 当下游地址拼出来请求
        // （实测 `GET /v1/user/balance -> 404`，它会在 2 秒内重试十几次）。
        // 形状对齐 DeepSeek 官方 `/user/balance`，数值按 token 预算估算 —— 见 [`balance`]。
        .route("/v1/user/balance", get(balance))
        .route("/user/balance", get(balance))
        // ⚠️ WorkBuddy 的「接口地址」语义是**完整的对话端点**：用户填什么它就 POST 到什么
        // （抓包日志实测 `POST /v1 -> 404`）。所以把可能被当作端点的路径都兜底挂上，
        // 用户填 `…/v1`、`…/v1/chat/completions`、甚至 `…` 都能通。
        .route("/v1", post(chat))
        .route("/", post(chat));
    api
        // 见 [`MAX_BODY_BYTES`]：默认 2 MiB 会把「带大文件的分析请求」直接 413 掉。
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        // ⚠️ 顺序：后加的 layer 在外层。`cors` 必须最外（预检要被它直接答掉，
        //    否则 `lan_guard` 会先要 Key，浏览器连预检都过不去）。
        .layer(axum::middleware::from_fn(lan_guard))
        .layer(axum::middleware::from_fn(cors))
}

/// 局域网模式的门禁：**非回环来源一律要有效 Key**。
///
/// 绑 `0.0.0.0` 之后，局域网里任何人都能敲到这个端口；网关能代花额度，
/// 所以这里不是「建议带 Key」而是硬拦。`/healthz` 例外（给用户/脚本探活），
/// 但它自己的 handler 会把账号细节收敛掉。
///
/// 回环来源完全不受影响 —— 本机行为与之前一字不差。
async fn lan_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !LAN_ACTIVE.load(Ordering::Relaxed) {
        return next.run(req).await;
    }
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0);
    let from_loopback = peer.map(|a| a.ip().is_loopback()).unwrap_or(false);
    if from_loopback || req.uri().path() == "/healthz" {
        return next.run(req).await;
    }
    let Some(token) = bearer(req.headers()) else {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return api_err(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            format!(
                "局域网访问需要 Key：请求头带 Authorization: Bearer sk-ts-…（来源 {}）",
                peer.map(|a| a.to_string()).unwrap_or_else(|| "未知".into())
            ),
        );
    };
    match gateway_keys::verify(&token) {
        Ok(Some(_)) => next.run(req).await,
        Ok(None) => {
            REJECTED.fetch_add(1, Ordering::Relaxed);
            api_err(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "网关 Key 无效或已吊销",
            )
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", e),
    }
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

async fn healthz(
    // ⚠️ 有它才能区分来源：`serve` 必须配 `into_make_service_with_connect_info`，
    //    否则这个 extractor 直接 500。
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
) -> impl IntoResponse {
    let mut body = status();
    // 局域网来源（非回环）只回「在跑没跑 + 端口 + 计数」：
    // 账号显示名 / 剩余额度是本机信息，不该随 healthz 泄给整个局域网。
    if !peer.ip().is_loopback() {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("remote".into(), json!(peer.ip().to_string()));
            obj.insert(
                "note".into(),
                json!("局域网来源：账号池细节已省略；对话请带 Authorization: Bearer sk-ts-…"),
            );
        }
        return Json(body);
    }
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

/// `GET /v1/user/balance` —— 余额查询（形状对齐 DeepSeek 官方 `/user/balance`）。
///
/// 为什么要有：dsh-pet 这类桌宠把「模型 base_url + `/user/balance`」当作余额地址，
/// 网关没这个路由时它们会一直 404 重试（实测 2 秒内十几次），界面上余额永远空着。
///
/// **口径**
/// - 有额度（`max_total_tokens`）：用 **DeepSeek V4.1-Flash 官方单价**把「预算 token」
///   和「已用 token」都折成钱，返回 `预算金额 − 已花费`。
///   桌宠把币种写死成 `¥`，所以只能给金额；直接给 token 数会显示成「¥94900」。
/// - 无额度：回落账号池真实剩余积分（和「积分共享」页面一致）。
async fn balance(headers: HeaderMap) -> axum::response::Response {
    // Key 可选：带了按这把 Key 的额度算；没带（或已吊销）就只报池子。
    let key = bearer(&headers).and_then(|t| gateway_keys::find_by_token(&t).ok().flatten());

    let pool = gateway_pool::snapshot();
    let pool_remaining: f64 = pool.iter().filter_map(|e| e.remaining).sum();
    let pool_accounts = pool.len();

    // 口径开关：默认按 token 预算折算金额；`TWIN_TS_BALANCE_MODE=pool` 强制报账号池。
    let want_pool = std::env::var("TWIN_TS_BALANCE_MODE")
        .map(|v| v.trim().eq_ignore_ascii_case("pool"))
        .unwrap_or(false);

    let budget = key.as_ref().and_then(|k| k.quota.max_total_tokens);
    let used = key.as_ref().map(|k| k.usage.total_tokens).unwrap_or(0);
    let (used_p, used_c) = key
        .as_ref()
        .map(|k| (k.usage.prompt_tokens, k.usage.completion_tokens))
        .unwrap_or((0, 0));

    // DeepSeek V4.1-Flash 官方单价（元/百万 token），峰谷两档。
    let peak = is_deepseek_peak_now();
    let (price_in, price_out) = deepseek_unit_price(peak);

    // 已花费：按真实的输入/输出拆分算，比套一个混合单价准。
    // 缓存命中价（0.02/0.04）不单独用 —— 网关拿不到上游的命中拆分，按未命中价估是保守的。
    let spent_yuan = used_p as f64 / 1_000_000.0 * price_in
        + used_c as f64 / 1_000_000.0 * price_out;

    // 预算金额：用同一套单价、同一 p/c 比例把额度 token 折成钱；
    // 还没产生用量时按纯输入价估（最保守，不会虚高）。
    let blend_per_token = if used > 0 {
        spent_yuan / used as f64
    } else {
        price_in / 1_000_000.0
    };
    let budget_yuan = budget.map(|b| b as f64 * blend_per_token);

    let (mode, amount, token_remaining, available) = if !want_pool && budget.is_some() {
        let left_tokens = (budget.unwrap_or(0) - used).max(0);
        let left_yuan = (budget_yuan.unwrap_or(0.0) - spent_yuan).max(0.0);
        (
            "token_budget",
            left_yuan,
            Some(left_tokens),
            key.as_ref().map(|k| k.is_active()).unwrap_or(true) && left_tokens > 0,
        )
    } else {
        (
            "pool_remaining",
            pool_remaining,
            budget.map(|b| (b - used).max(0)),
            pool_remaining > 0.0,
        )
    };

    Json(json!({
        "is_available": available,
        "balance_infos": [{
            // 桌宠/客户端都按 ¥ 展示，这里统一 CNY
            "currency": "CNY",
            "total_balance": format!("{amount:.2}"),
            "granted_balance": format!("{amount:.2}"),
            "topped_up_balance": "0.00",
        }],
        // 网关扩展字段（非 DeepSeek 标准，客户端会忽略），用来说明这个数怎么来的
        "twin_switch": {
            "mode": mode,
            "token_remaining": token_remaining,
            "token_used": used,
            "token_budget": budget,
            "spent_yuan": spent_yuan,
            "budget_yuan": budget_yuan,
            "price_input_per_mtok": price_in,
            "price_output_per_mtok": price_out,
            "peak": peak,
            "pool_remaining": pool_remaining,
            "pool_accounts": pool_accounts,
            "key_name": key.as_ref().map(|k| k.name.clone()),
        }
    }))
    .into_response()
}

/// DeepSeek V4.1-Flash 官方单价（元 / 百万 token）：`(输入·缓存未命中, 输出)`。
///
/// 来源：官方 2026-09-10 起执行的峰谷价 —— 输入未命中 1/2 元、输出 4/8 元（空闲/高峰），
/// 缓存命中 0.02/0.04 元。命中价不单独用：网关看不到上游的缓存命中拆分。
fn deepseek_unit_price(peak: bool) -> (f64, f64) {
    if peak {
        (2.0, 8.0)
    } else {
        (1.0, 4.0)
    }
}

/// 当前是否处于 DeepSeek 高峰时段：**工作日 9:00–12:00、14:00–18:00（北京时间）**，其余空闲。
///
/// ⚠️ 法定节假日与调休没有日历，这里只按「周一~周五」近似 —— 会在节假日多算高峰价，
/// 属已知偏差；要精确就得接节假日日历，目前不值当。
fn is_deepseek_peak_now() -> bool {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    if now.weekday().num_days_from_monday() >= 5 {
        return false;
    }
    let h = now.hour();
    (9..12).contains(&h) || (14..18).contains(&h)
}

async fn models() -> impl IntoResponse {
    // ⚠️ 列出去的 **id 只能是公开名**：Qoder 的 `gmodel` 用户认不出，客户端里也搜不到
    // （2026-10-24 用户要求「只显示正常的模型名」）。代号只在网关内部发上游时用。
    // 去重时**按归一化指纹**：`GLM-5.3`（qoder）与 `glm-5.3`（trae）是同一个模型。
    let mut seen: Vec<(String, String, String)> = Vec::new(); // (公开名, 作用域, 代号)
    let norm = gateway_pool::normalize_model_key;
    for (scope, code, public) in crate::modules::gateway_models::catalog_rows(None) {
        if seen.iter().any(|(p, _, _)| norm(p) == norm(&public)) {
            continue;
        }
        seen.push((public, scope, code));
    }
    let data: Vec<Value> = seen
        .iter()
        .map(|(public, scope, code)| {
            // 显示名照旧给客户端里的名字（Trae / WB 的 `DeepSeek-V4-Flash 正式版` 之类）——
            // 有的客户端只读 `display_name`，缺了就会退化成一串代号。
            let display = crate::modules::gateway_models::display_of(scope, code);
            json!({
                "id": public,
                "object": "model",
                "owned_by": "twin-switch",
                "display_name": display,
                "name": display,
            })
        })
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
    let msg = msg.into();
    // **所有对外错误都从这里出去**，所以诊断日志只挂这一处就覆盖了全部
    // 4xx/5xx 返回（用户报的 400 / 401 / 500 都在内）。
    // ⚠️ 只记「状态码 + 类型 + 正文」，不记请求体（正文里可能有对话内容）。
    crate::modules::diag_log::error(
        "gateway",
        format!("HTTP {} {} | {}", code.as_u16(), kind, msg),
    );
    (code, Json(json!({ "error": { "type": kind, "message": msg } }))).into_response()
}

/// 把**最近一次** `/v1/chat/completions` 的请求体落盘（覆盖式，只留一次）。
///
/// 排障用：客户端带附件时到底发的是 `image_url` / `file` / 纯文本，光看「空白」猜不出来
/// （2026-10-24 Qoder 带 xlsx「已完成但空白」）。每个字符串截到 2000 字，避免把
/// 几十 MB 的 base64 写进磁盘；`gateway-last-request.json` 每次覆盖，不累积历史。
fn snapshot_request(req: &Value) {
    fn trim(v: &Value, budget: usize) -> Value {
        match v {
            // 按**字符**截，不能按字节切 —— 切在 UTF-8 中间会 panic。
            Value::String(s) if s.chars().count() > budget => {
                let head: String = s.chars().take(budget).collect();
                json!(format!("{head}…<已截断，原长 {} 字>", s.chars().count()))
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| trim(x, budget)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), trim(x, budget)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    let path = crate::modules::config::store_dir().join("gateway-last-request.json");
    if let Ok(text) = serde_json::to_string_pretty(&trim(req, 2000)) {
        let _ = std::fs::write(path, text);
    }
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
    let key = match gateway_keys::find_by_token(&token) {
        Ok(Some(k)) => k,
        Ok(None) => {
            REJECTED.fetch_add(1, Ordering::Relaxed);
            return api_err(StatusCode::UNAUTHORIZED, "authentication_error", "网关 Key 无效或已吊销");
        }
        Err(e) => {
            return api_err(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", e);
        }
    };

    // ①-a 吊销状态。**分三种说清楚** —— 只回「Key 无效」会让用户以为是复制错了，
    // 跑去重建 Key，而真正的原因是额度用尽（模板里最费时间的一类误解）。
    if !key.is_active() {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        let msg = if key.revoked_by_quota() {
            format!(
                "这把 Key「{}」已因额度用尽被自动吊销（累计 {} token）。\
                 在「积分共享 → Key」里提高额度或点「用量清零」后，再点「恢复」即可继续用。",
                key.name, key.usage.total_tokens
            )
        } else {
            format!("这把 Key「{}」已被吊销。可在「积分共享 → Key」里恢复。", key.name)
        };
        return api_err(StatusCode::UNAUTHORIZED, "authentication_error", msg);
    }

    // ①-b 额度闸门。额度在**请求进来时**判一次，避免超额继续消耗上游积分。
    //
    // 注意这里只拦「额度已到但 Key 还没被吊销」的情况 —— 正常到量时
    // `record_usage` 已经把它自动吊销了，`verify` 就查不到它。
    // 这个分支覆盖的是「已恢复但用量没清零 / 额度被调低」这类手工操作后的状态。
    if let Some(msg) = key.quota_block() {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return api_err(
            StatusCode::TOO_MANY_REQUESTS,
            "quota_exceeded",
            format!(
                "这把 Key [{msg}]。可在「积分共享 → Key」里提高额度，或把用量清零后重试。"
            ),
        );
    }

    let Some(Json(req)) = body else {
        return api_err(StatusCode::BAD_REQUEST, "invalid_request_error", "请求体不是合法 JSON");
    };
    snapshot_request(&req);
    let model = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // 显示名 → 代号：有人会照客户端界面填 `Qwen3.8-Max`，而上游只认 `qmodel_38max`。
    // 归一化放在选号之前，后面 `resolve_scopes` / 上游请求体拿到的都是代号。
    let model = crate::modules::gateway_models::canonical_model(&model);

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

    // ④ 选号：**同一段对话锁定同一账号，新对话才按「积分最早过期」挑**。
    //
    // 为什么不是每次都按紧迫度挑：Trae 的上游会话 ID 由 `session_seed(model, uid, …)`
    // 派生（uid 参与指纹），中途换号 ⇒ 上游当成新会话 ⇒ 前缀缓存全失效、观感上「会话断了」。
    // 所以规则是：**对话第一次请求**按最早过期选（先用快过期的，省钱）；
    // **后续请求**沿用同一个号，只有它不可用/冷却时才重新挑。
    //
    // 会话指纹 = 模型 + 首条 user（不含 uid），见 `gateway_stream::session_seed`。
    // 用原始请求体算指纹（`upstream_req` 是后面才克隆并改写的，那时已经不含原始形态）。
    let conv = crate::modules::gateway_stream::conversation_key(&bare, &req);
    let sticky_key = gateway_pool::sticky_lookup(&conv);

    let sel = match gateway_pool::preview(&model) {
        Ok(s) => s,
        Err(e) => return api_err(StatusCode::BAD_REQUEST, "invalid_request_error", e),
    };
    // 粘住的号仍然可用（在池子里、且没冷却）才继续用它；否则回落到紧迫度最高的那个。
    let sticky_alive = sticky_key.as_ref().and_then(|k| {
        gateway_pool::usable()
            .into_iter()
            .find(|e| &e.key == k && e.cooldown_until.unwrap_or(0) <= crate::modules::config::now_ms())
    });
    let picked = match sticky_alive {
        Some(e) => e,
        None => match sel.picked.clone() {
            Some(p) => p,
            None => {
                return api_err(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no_account",
                    sel.reason.unwrap_or_else(|| "没有可用账号".into()),
                )
            }
        },
    };
    // 记下这段对话用的是哪个号（下次接着用它）。换号时这里会被覆盖。
    gateway_pool::sticky_bind(&conv, &picked.key);
    if !key.allows(&picked.scope) {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return api_err(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("选号落到了 {}，但这把 Key 没有它的授权", picked.scope),
        );
    }

    // ⑤ 上游透传 + **同一个请求内的换号重试**。
    //
    // 客户端要流式时**真的按流式走**（2026-10-24 之前是「向上游要完整结果，再一次性
    // 包成一段 SSE」）。改的理由很直接：整段生成 10 分钟的任务，客户端前 10 分钟
    // 一个字都看不到 ⇒ 它会判定卡住并重试 ⇒ 重试再排一次队，越等越久。
    // 现在三家各有一套解析（Trae 私有事件流 / Qoder 信封 SSE / WB 原生 OpenAI SSE
    // 直接透传），见 `gateway_stream`。
    //
    // ⚠️ 换号只能在**首字之前**做：一旦开始往客户端发字节，这轮就只能在这条流上报错。
    let want_stream = req.get("stream").and_then(Value::as_bool).unwrap_or(false);
    // 降级开关：`TWIN_TS_NO_STREAM=1` 时退回「上游聚合完再一次性包成 SSE」的老路径。
    // 真流式万一在某家上游上踩到边角（半截流比整段失败更难收拾），这是最快的止损手段。
    let stream_ok = want_stream && std::env::var("TWIN_TS_NO_STREAM").as_deref() != Ok("1");
    let mut upstream_req = req.clone();
    let mut trace = Trace::new(&model, want_stream);
    // 这次请求记到哪把 Key 头上（额度在 `Trace::finish` 里统一记账）。
    trace.key_id = Some(key.id.clone());
    // 前缀稳定 + 历史折叠：确定性规则，同样的历史每轮折出同样的字节，
    // 这样上游的前缀缓存才有机会命中（见 `gateway_stream::prepare`）。
    if let Some(obj) = upstream_req.as_object_mut() {
        let prep = crate::modules::gateway_stream::prepare(obj);
        trace.msgs = prep.msgs;
        trace.tools = prep.tools;
        trace.chars = prep.chars;
        trace.tools_chars = prep.tools_chars;
        trace.folded = prep.folded;
    }

    // 候选顺序：pool 的紧迫度顺序（没在冷却的在前），并把 preview 选中的挪到队首，
    // 保证「没有触发换号时挑的还是同一个号」—— 行为对用户可预期。
    let mut cands: Vec<gateway_pool::PoolEntry> = gateway_pool::ordered_candidates(&model)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| key.allows(&c.scope))
        .collect();
    if cands.is_empty() {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return api_err(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_account",
            format!("这把 Key 授权的范围里没有可用账号（模型 {bare}）"),
        );
    }
    if let Some(pos) = cands.iter().position(|c| c.key == picked.key) {
        let head = cands.remove(pos);
        cands.insert(0, head);
    }

    let mut last_err: Option<(StatusCode, String)> = None;
    let mut tried = 0usize;
    for cand in cands.iter().take(MAX_FAILOVER) {
        tried += 1;
        let mut t = trace.clone();
        t.hit(&cand.scope, &cand.key);
        t.failovers = tried - 1;
        match forward(cand, &bare, &upstream_req, stream_ok, t).await {
            Ok(Upstream::Json(resp)) => {
                REQUESTS.fetch_add(1, Ordering::Relaxed);
                gateway_keys::touch(&key.id);
                // 成功了就把这个号之前记的冷却撤掉，别让一次偶发失败按它 60 秒。
                gateway_pool::clear_cooldown(&cand.key, Some(&bare));
                let mut done = trace.clone();
                done.hit(&cand.scope, &cand.key);
                done.failovers = tried - 1;
                if let Some(u) = resp.get("usage") {
                    done.usage(u);
                }
                done.finish(true, 200, "");
                // `stream_ok` 为假但客户端要流式 ⇒ 走老的「聚合后包装」路径。
                return if want_stream {
                    sse_from_completion(&resp)
                } else {
                    Json(resp).into_response()
                };
            }
            Ok(Upstream::Sse(resp)) => {
                REQUESTS.fetch_add(1, Ordering::Relaxed);
                gateway_keys::touch(&key.id);
                gateway_pool::clear_cooldown(&cand.key, Some(&bare));
                // 指标在流结束时由 `gateway_stream::respond` 落盘（那时才知道 ttft 与用量）。
                return resp;
            }
            Err((code, msg)) => {
                // 上游报错 → 反馈给池子（冷却/标记），下一个请求也不会再撞同一个。
                // ⚠️ 按 **(账号, 模型)** 记账：429 只锁这个模型，别的模型还能用这个号。
                if let Some((until, kind)) =
                    gateway_pool::apply_error(&cand.key, code.as_u16(), &msg, Some(&bare))
                {
                    let left = (until - gateway_pool::now_ms_pub()).max(0) / 1000;
                    let _ = gateway_pool::mark_last_error(&cand.key, &msg, kind, left);
                }
                if gateway_pool::is_queued_hint(&msg) {
                    trace.note_queued();
                }
                // ⚠️ 把「最后试过的那个号」记回外层 trace。
                //
                // 原来只在 `t`（每轮 clone）上 hit，失败时落盘的 trace 里 scope/account
                // 是**空的** —— 2026-10-26 排 11128 时翻指标，失败行全是空账号，根本
                // 不知道是哪个号被拒（成功行反而有）。失败比成功更需要这个信息。
                trace.hit(&cand.scope, &cand.key);
                trace.failovers = tried - 1;
                let failover = should_failover(code.as_u16(), &msg);
                log_failover(&model, &cand.key, code.as_u16(), &msg, failover);
                trace.note_failover();
                last_err = Some((code, msg));
                if !failover {
                    break;
                }
            }
        }
    }

    let (code, msg) = last_err.unwrap_or((
        StatusCode::BAD_GATEWAY,
        "上游没有任何响应".to_string(),
    ));
    trace.finish(false, code.as_u16(), &msg);
    api_err(
        code,
        "upstream_error",
        if tried > 1 {
            format!("{msg}（已按积分池顺序换了 {tried} 个账号，全部失败）")
        } else {
            msg
        },
    )
}

/// 一个请求最多试几个账号：换号是为了「让用户这次能成」，不是把 10 个号全撸一遍
/// 把客户端拖到超时。3 个足够覆盖「当天额度用尽的那个 + 一个被限流的」。
const MAX_FAILOVER: usize = 3;

/// 把每一次换号尝试的失败原因落盘（`~/.twin-switch/gateway-errors.jsonl`）。
///
/// ⚠️ 这是「用户说没换号」时唯一的取证入口：以前只记一行 `POST -> 502`，
/// 看不出是哪个号的错、错在哪、有没有往下试 —— 于是只能靠猜。
fn log_failover(model: &str, key: &str, status: u16, msg: &str, failover: bool) {
    let flat: String = msg
        .replace(['\n', '\r', '\t'], " ")
        .chars()
        .take(400)
        .collect();
    let line = format!(
        "{}\tmodel={}\taccount={}\thttp={}\t{}\t{}\n",
        chrono::Local::now().format("%m-%d %H:%M:%S"),
        model,
        key,
        status,
        if failover { "换下一个" } else { "停止（不换）" },
        flat,
    );
    let path = crate::modules::config::store_dir().join("gateway-errors.jsonl");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 超过 512 KB 就只留尾部 —— 这文件是给排查用的，不许无限长大。
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 512 * 1024 {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let mut cut = text.len().saturating_sub(256 * 1024);
                // 不能切在多字节字符中间（中文路径/文案很常见）。
                while cut < text.len() && !text.is_char_boundary(cut) {
                    cut += 1;
                }
                let _ = std::fs::write(&path, &text[cut..]);
            }
        }
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 这次失败该不该**换下一个号**继续（而不是把错误抛回客户端）。
///
/// 只认两类：
/// - **账号自身的问题**：`401/403`（凭据死）、`402/429`（额度 / 频率）、`5xx`、传输层 `-1`；
/// - **上游用业务码说「这个号现在用不了」**：Trae 的 `6804`，以及文案里的
///   「频率限制 / 使用量已超出 / 额度不足 / 余额不足 / quota exceeded / rate limit」。
///
/// 反例（**必须不换**，换了也是白试 + 拖长客户端超时）：`400` 参数错、`404` 模型不存在，
/// 以及**排队**（`10605` / `isQueued` —— 排队是全局的，换号只会再排一次）。
fn should_failover(status: u16, msg: &str) -> bool {
    // 排队是**模型维度**的：换号只会再排一次，白等一轮还拖长客户端超时。
    // 判据与冷却归类同源（`gateway_pool::is_queued_hint`），别再各写一套。
    if crate::modules::gateway_pool::is_queued_hint(msg) {
        return false;
    }
    let m = msg.to_ascii_lowercase();
    if matches!(status, 0 | 401 | 402 | 403 | 429 | 500..=599) {
        return true;
    }
    // WorkBuddy `11128 Illegal API invocation from an unapproved channel`：
    // 上游是按**账号/频道**判的（同一份 body 换其它账号就正常，见 2026-10-26 真机探针），
    // 所以它对「这个号」是死判据 ⇒ 必须换号，而不是直接把 400 丢给用户。
    m.contains("11128")
        || m.contains("unapproved channel")
        || m.contains("6804")
        || m.contains("quota exceeded")
        || m.contains("rate limit")
        || msg.contains("频率限制")
        || msg.contains("使用量已超出")
        || msg.contains("额度不足")
        || msg.contains("余额不足")
}

/// 上游回了「看不懂原因」的错误时，把这次发出去的请求体原样落盘（最多 3 份、≤4 MB）。
///
/// ⚠️ 为什么需要：2026-10-26 的 `11128` 用 15 种合成请求都复现不出来（无 system /
/// 33 工具 / 12 万字符 / 顶层 system 字段 / 图片 / 多轮，全部 200），只有用户真机上
/// 那份**真实内容**会被拒。靠猜形状是死路，只能把出事的那份请求原样留下来比对。
///
/// 隐私：文件里有对话正文，只写在本机 `<store>/diag/`，最多 3 份，路径会显示在错误信息里，
/// 用户可以随时手删；不含任何凭据（Token 在 header 里，不进 body）。
fn dump_rejected_body(body: &Value) -> Option<String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    /// 每个进程最多留几份：够比对一次就行，别把磁盘塞满。
    const KEEP: usize = 3;
    const MAX_BYTES: usize = 4 * 1024 * 1024;
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if COUNT.fetch_add(1, Ordering::Relaxed) >= KEEP {
        return None;
    }
    let dir = crate::modules::config::store_dir().join("diag");
    std::fs::create_dir_all(&dir).ok()?;
    let text = serde_json::to_vec_pretty(body).ok()?;
    if text.len() > MAX_BYTES {
        return None;
    }
    let path = dir.join(format!(
        "rejected-{}-{}.json",
        crate::modules::config::now_ms(),
        COUNT.load(Ordering::Relaxed)
    ));
    std::fs::write(&path, text).ok()?;
    Some(path.to_string_lossy().to_string())
}

/// 把首条消息的 `developer` 角色还原成 `system`（OpenAI 的后继别名还原）。
///
/// 返回「是否改过」。单独抽出来是为了能单测 —— 它决定 11128 的第一道闸门过不过，
/// 而这段逻辑本身不依赖任何 IO。
fn normalize_first_role(body: &mut Value) -> bool {
    let Some(first) = body
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
        .and_then(|a| a.first_mut())
    else {
        return false;
    };
    if first.get("role").and_then(|r| r.as_str()) == Some("developer") {
        first["role"] = json!("system");
        return true;
    }
    false
}

/// 请求体形状摘要：**只说结构，不带任何正文**。
///
/// 用途只有一个：上游报错时让人一眼看出「是不是这个请求太大 / 工具太多 / 工具结果太多」。
/// 2026-10-26 的 `11128` 事故里，网关指标显示失败请求都是 ~98 万字符，而界面只显示了
/// 上游那句 "unapproved channel" —— 两边对不上，排障只能靠翻 jsonl。
fn req_shape(req: &Value) -> String {
    let msgs = req.get("messages").and_then(Value::as_array);
    let count = msgs.map(|a| a.len()).unwrap_or(0);
    let count_role = |role: &str| {
        msgs.map(|a| {
            a.iter()
                .filter(|m| m.get("role").and_then(Value::as_str) == Some(role))
                .count()
        })
        .unwrap_or(0)
    };
    let tools = req
        .get("tools")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let chars = serde_json::to_string(req)
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let images = serde_json::to_string(req)
        .map(|s| s.matches("image_url").count())
        .unwrap_or(0);
    format!(
        "{chars} 字符 / {count} 条消息（工具结果 {} 条、assistant {} 条、system {} 条、developer {} 条）/ {tools} 个工具 / {images} 张图 / model={}",
        count_role("tool"),
        count_role("assistant"),
        count_role("system"),
        count_role("developer"),
        req.get("model").and_then(Value::as_str).unwrap_or("?")
    )
}

/// 把一条完整结果包装成 OpenAI 流式事件。
///
/// 发三个事件：正文 chunk → 结束 chunk → `[DONE]`。
/// 客户端（包括 Trae 的「测试并添加」）看到的是标准 SSE，不会因为
/// `stream:true` 拿不到流而判定失败。
///
/// ⚠️ **工具调用必须在这里发出去**，而且 `finish_reason` 要照实给 `tool_calls`：
/// 以前这里写死 `finish_reason: "stop"`、delta 只发 content，流式客户端永远等不到
/// 工具调用，只会收到一段 `finish_reason=stop` 的空话（2026-10-24 实测）。
///
/// ⚠️ 这不是「逐字流式」：内容一次性到达。真流式要按上游分别解析，后续再做。
fn sse_from_completion(v: &Value) -> axum::response::Response {
    use axum::http::header;

    let body = sse_body(v);
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

/// [`sse_from_completion`] 的正文部分（拆出来才有得测）。
fn sse_body(v: &Value) -> String {
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let calls = v
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop")
        .to_string();
    let model = v.get("model").cloned().unwrap_or_else(|| json!("twin-switch"));
    let id = v
        .get("id")
        .cloned()
        .unwrap_or_else(|| json!("chatcmpl-twinswitch"));
    let created = v
        .get("created")
        .cloned()
        .unwrap_or_else(|| json!(crate::modules::qoder_accounts::now_ms() / 1000));

    let mut delta = json!({ "role": "assistant" });
    if calls.is_empty() {
        delta["content"] = json!(content);
    } else {
        // OpenAI 标准：只调用工具时 content 为 null（给空串会被部分客户端当空回答）。
        delta["content"] = if content.is_empty() {
            Value::Null
        } else {
            json!(content)
        };
        delta["tool_calls"] = json!(calls);
    }

    let first = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": Value::Null,
        }],
    });
    let last = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": finish }],
    });

    format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n")
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

/// 转发结果：要么是完整结果（客户端没要流式），要么是一条已经可以往下发字节的流。
enum Upstream {
    Json(Value),
    /// 已经可以往客户端写的流式响应（`gateway_stream` 里已经组好头和 body）。
    Sse(axum::response::Response),
}

/// 读配置并转发。
///
/// 优先用 `~/.twin-switch/upstreams.json` 里登记的；没登记时回落到 [`builtin_spec`]，
/// 两者都没有才 501（并说明这家为什么还需要适配器）。
async fn forward(
    cand: &gateway_pool::PoolEntry,
    model: &str,
    req: &Value,
    want_stream: bool,
    trace: Trace,
) -> Result<Upstream, (StatusCode, String)> {
    let scope = cand.scope.as_str();
    // ⚠️⚠️ **公开名 → 这一家的代号**，必须在这里做、且每家各算一次。
    //
    // Qoder 对外叫 `GLM-5.3`（客户端里那个名字），上游只认 `gmodel`；而同一行在
    // Trae 那边就是 `glm-5.3`。选号之前不知道会落到哪一家，所以只能在这里换。
    let model = crate::modules::gateway_models::upstream_model(scope, model);
    let model = model.as_str();
    let path = crate::modules::config::store_dir().join("upstreams.json");
    let spec = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get(scope).cloned())
        .and_then(|v| UpstreamSpec::from_json(&v))
        .or_else(|| builtin_spec(scope));

    // Trae 走私有协议（设备 jwt + llm_utils_chat 专用请求体），不进通用透传。
    //
    // ⚠️⚠️ **必须把选中的账号传下去**。原来这里是 `trae_cred(None)` —— 那个 `None`
    // 意思是「随便找一份能解开的登录态」，而它按目录顺序取，实际永远是**当前登录**的号。
    // 于是网关对 Trae **根本不会换号**：池子按积分到期紧迫度选出了 A，请求却用 B 的
    // token 发出去（用户实测：明明 B 的积分快过期了，消耗的还是 A）。
    // 现在按池子条目的 key（`trae:{uid}`）取对应账号的设备 jwt。
    let trae_uid = if scope.starts_with("trae") {
        cand.key.split_once(':').map(|(_, v)| v.trim().to_string())
    } else {
        None
    };
    if scope.starts_with("trae") && spec.is_none() {
        return if want_stream {
            trae_stream(model, req, trace, trae_uid.clone()).await
        } else {
            trae_forward(model, req, trae_uid).await.map(Upstream::Json)
        };
    }

    // Qoder 走 COSY 签名的私有协议（**信封 SSE**）——2026-10-22 真机跑通。
    if scope.starts_with("qoder") && spec.is_none() {
        // ⚠️⚠️ **必须把池子选中的账号传下去**。`qoder_stream` 原来是
        // `accounts.iter().find(|a| a.is_usable())` —— 永远第一个可用号，
        // 于是池子按紧迫度轮换、日志写「已按积分池顺序换了 2 个账号」，
        // 实际每个候选都在用**同一个号**发请求，失败当然一模一样
        // （2026-10-26 用户实测：GLM-5.3-Flash 连续两轮 400）。
        let qoder_id = if scope.starts_with("qoder") {
            cand.key
                .split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .filter(|v| !v.is_empty())
        } else {
            None
        };
        return if want_stream {
            qoder_stream(scope, model, req, trace, qoder_id).await
        } else {
            qoder_forward(scope, model, req, qoder_id)
                .await
                .map(Upstream::Json)
        };
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

    // ⚠️⚠️ 必须按**账号**取凭据：同一个作用域下可能有好几个号（wb-cn 常有 3~4 个），
    // 按作用域取永远是第一个 —— 那样「自动换号」就是一句空话：候选换了，token 没换，
    // 上游照样回同一个 429（2026-10-24 用户实测）。
    let token = token_of_key(&cand.key, scope).ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            format!(
                "{} 找不到可用凭据（账号「{}」可能在导出后被删或没有 token）",
                scope, cand.label
            ),
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

    // ⚠️ 首条 `developer` 角色归一化为 `system`。
    //
    // OpenAI 在 2024-12 把 system 角色更名为 developer（developer 是 system 的后继别名，
    // 语义完全相同），新一些的客户端（如 DeepSeek Harness）就按新规范发 `developer`。
    // 但上游的**渠道校验**只认字面量 `role: "system"`，发 developer 会被判成
    // 「非官方客户端」→ 400 / code 11128 / Illegal API invocation from an unapproved channel。
    //
    // 实测（同一网关、同一账号、同一模型，**仅改角色**，2026-10-27）：
    //   首条 developer → HTTP 400 / 11128
    //   首条 system    → HTTP 200
    //
    // 归一化不改变任何内容，只是把别名还原成上游接受的那个名字。
    // 只处理**首条**：后续消息里的 developer 极少见，且不应影响已生效的首轮系统指令。
    if normalize_first_role(&mut body) {
        // 记一笔：这条请求原本首条是 developer（dsh 这类新客户端），已还原成 system。
        // 出现频率能反映「哪个客户端在用新角色名」，也是上游改判据时的一手线索。
        crate::modules::diag_log::info("gateway", "首条 developer 角色已归一化为 system");
    }

    // ⚠️⚠️ WorkBuddy / CodeBuddy 的对话网关还会对 **system prompt** 做关键词风控：
    // 开头出现 `You are an AI agent powered by DeepSeek Harness` 直接回
    // `400 11128 Illegal API invocation from an unapproved channel`（文案骗人，
    // 跟渠道无关；2026-10-26 真机 A/B 已证实，见 `gateway_prompt` 模块头注释）。
    // DeepSeek Harness 每轮都注入这句 ⇒ 不打这个补丁，走 WB 的每一轮必被拦。
    if cand.product == "workbuddy" || scope.starts_with("wb") {
        let hits = crate::modules::gateway_prompt::neutralize_system(&mut body);
        if hits > 0 {
            crate::modules::diag_log::info(
                "gateway",
                format!("workbuddy：system 命中关键词风控，已中和 {hits} 处（零宽空格，语义不变）"),
            );
        }
    }

    // ⚠️⚠️ 超时口径必须分两档，原来一律 300 秒整段超时是错的：
    //
    // * **非流式**（`want_stream == false`）：整段 300 秒，超了就是真挂了。
    // * **流式**：整段可能很长（WorkBuddy 的 agent 任务实测有 10 分钟以上的），
    //   300 秒会在 5 分钟处**把正在写的流掐断** —— 客户端只看到回答突然没了，
    //   而指标里是一条「成功但很短」的记录，极难查。所以流式改成：
    //   整段给 30 分钟（够任何正常任务），真正的保护交给**每块空闲 120 秒**
    //   （reqwest 的 `read_timeout` 是按单次读计时的）。
    //   `gateway_stream` 里还有一层自己的空闲判据（首字前 90s / 出字后 8s），
    //   两层不冲突：外层是「上游彻底不动了」，内层是「上游还连着但不再吐字」。
    let mut builder = reqwest::Client::builder();
    builder = if want_stream {
        builder
            .timeout(std::time::Duration::from_secs(1800))
            .read_timeout(std::time::Duration::from_secs(120))
    } else {
        builder.timeout(std::time::Duration::from_secs(300))
    };
    let client = builder
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("构造 HTTP 客户端失败：{e}")))?;
    let mut rb = client
        .post(format!("{}{}", spec.base_url, spec.chat_path))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .header("accept", if spec.force_stream { "text/event-stream" } else { "application/json" });

    // ⚠️⚠️ WorkBuddy 的对话端点是**按「频道」校验**的：只带 `Authorization` 会被回
    // `400 {"code":"11128","msg":"Illegal API invocation from an unapproved channel",
    //  "zh":"请求被拦截，请重新发送","displayTips":"This call failed a channel verification…"}`
    // （2026-10-26 用户实测）。
    //
    // 官方客户端在**每个**请求上都带这两个头（本工具取模型清单那条请求一直带着，
    // 所以清单能拉、对话却被拦 —— 差别只在这两个头）。对话这条路径以前漏了。
    if cand.product == "workbuddy" || scope.starts_with("wb") {
        rb = rb
            .header("user-agent", crate::modules::gateway_models::WORKBUDDY_UA)
            .header("x-product", "SaaS");
    }

    let resp = rb
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

    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let hint = match status.as_u16() {
            401 | 403 => "（上游鉴权失败：token 可能已过期，需重新登录该账号）",
            429 => "（上游限流 / 当日额度用完）",
            _ => "",
        };
        // ⚠️ **原样保留上游状态码**。以前一律改成 502，于是 429 被当成 5xx 记「60 秒后再说」，
        // 402 更永远走不到「等到重置窗口」那条分支 —— 冷却判据全被这一个转换废掉了。
        let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        // ⚠️ 带上**请求形状**（不含任何正文）。2026-10-26 排 WorkBuddy `11128` 时，
        // 光看上游那句话（"unapproved channel"）根本判断不了是模型 / 账号 / 体量的问题，
        // 只能靠网关自己的指标去反推「失败请求都是 98 万字符」。既然要报错，就把这个
        // 现成的信息一起报出来 —— 用户复制一句话给我们就够定位。
        // 频道校验类错误（11128 那一族）用合成请求复现不出来，把真实请求留下来。
        let dumped = if text.contains("11128") || text.contains("unapproved channel") {
            dump_rejected_body(&body)
        } else {
            None
        };
        let where_hint = match &dumped {
            Some(p) => {
                format!("\n本次被拒的请求已原样保存：{p}（含对话正文，只在本机，可直接删除）")
            }
            None => String::new(),
        };
        return Err((
            code,
            format!(
                "上游返回 HTTP {}{hint}：{}\n请求形状：{}{where_hint}",
                status.as_u16(),
                explain_upstream_error(&text),
                req_shape(req)
            ),
        ));
    }

    // WorkBuddy 这类 OpenAI 兼容上游：本来就是标准 SSE ⇒ 直接透传字节。
    // 重新解析再拼一遍只会把 `tool_calls` / `finish_reason` 的细节拼丢。
    //
    // ⚠️ 先看 content-type：自建上游有时直接回一整段 JSON（不支持流式），
    // 把 JSON 当 SSE 透出去，客户端会一直等 `data:` 行 —— 那是最难查的一种「卡住」。
    if want_stream {
        let sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("event-stream"))
            .unwrap_or(false);
        if sse {
            return Ok(Upstream::Sse(crate::modules::gateway_stream::passthrough(
                resp, trace,
            )));
        }
        let text = resp.text().await.unwrap_or_default();
        return serde_json::from_str::<Value>(&text)
            .map(Upstream::Json)
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!(
                        "上游既不是 SSE 也不是 JSON（{e}）：{}",
                        text.chars().take(200).collect::<String>()
                    ),
                )
            });
    }

    let text = resp.text().await.unwrap_or_default();
    if spec.force_stream {
        return aggregate_sse(&text, model).map(Upstream::Json);
    }
    serde_json::from_str::<Value>(&text)
        .map(Upstream::Json)
        .map_err(|e| {
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

// ---------------------------------------------------------------------------
// 工具调用（function calling）聚合
//
// 网关对外是**非流式**（上游只吃流式，本地聚合）。但工具调用本来就是「分片到达」的：
// 以前聚合器只拼 `delta.content`，于是带 tools 的请求会出现三种假故障（2026-10-24 实测）：
//   - Trae + DeepSeek：上游只发 tool_calls、没有正文 ⇒ 被当成「空回答」报 502；
//   - glm-5.2：finish_reason 是 tool_calls，但 message 里没有 tool_calls 字段；
//   - 流式客户端：拿到 finish_reason=stop，永远等不到工具调用。
// 所以这里按 OpenAI 语义把分片拼回完整调用，三种上游形态（标准分片 / 完整快照 /
// Trae 的 `function_call`+`partial_arguments`）都要能吃。
// ---------------------------------------------------------------------------

/// 累积中的一次工具调用。
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolCallAcc {
    pub(crate) index: Option<u64>,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// `new` 是不是 `acc` 的**结构化扩展**（同一个 JSON 对象，`acc` 的每个键值都在 `new` 里）。
///
/// ⚠️ 不能用「前缀」判断快照：`{"city":"上海"}` 扩展成 `{"city":"上海","n":1}` 时，
/// 原来结尾的 `}` 变成了 `,`，`new.starts_with(acc)` 是 **false**，拼接后得到两段 JSON。
/// 上游（Trae）恰好是这种「每片给一个更完整的快照」的形态。
fn json_extends(acc: &str, new: &str) -> bool {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) =
        (serde_json::from_str::<Value>(acc), serde_json::from_str::<Value>(new))
    else {
        return false;
    };
    a.iter().all(|(k, v)| b.get(k) == Some(v))
}

/// 合并 `arguments` 分片。三种真实形态都要认：
///
/// - **真分片**：`{"city":"` + `上海"}` ⇒ 必须拼接；
/// - **完整快照**：先给 `{"city":"上海"}`，后面又给一个更完整的完整值 ⇒ 用新的
///   （拼起来会变成两段 JSON，客户端解析必失败）；
/// - **重复旧值**：新值是已累积值的「子集或前缀」 ⇒ 丢掉。
fn merge_arguments(acc: &str, new: &str) -> String {
    if new.is_empty() {
        return acc.to_string();
    }
    if acc.is_empty() {
        return new.to_string();
    }
    if new.starts_with(acc) || json_extends(acc, new) {
        return new.to_string();
    }
    if acc.starts_with(new) || json_extends(new, acc) {
        return acc.to_string();
    }
    let mut out = acc.to_string();
    out.push_str(new);
    out
}

/// `arguments` 取文本：上游可能给字符串，也可能直接给对象（非标准，但要能吃）。
fn arg_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// 把一片工具调用并进累积表。按 `index` 对齐（OpenAI 标准），没有 index 时按 `id`，
/// 都没有就新开一条 —— 宁可多一条，也不要把两次不同的调用合并成一次。
pub(crate) fn absorb_tool_call(accs: &mut Vec<ToolCallAcc>, raw: &Value) {
    let Some(obj) = raw.as_object() else { return };
    let index = obj.get("index").and_then(Value::as_u64);
    let id = obj
        .get("id")
        .or_else(|| obj.get("tool_call_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    // OpenAI 形态是 `function: { name, arguments }`；Trae 私有流用 `function_call`；
    // 还有上游把 name/arguments 平铺在顶层 —— 三种都认。
    let func = obj.get("function").or_else(|| obj.get("function_call"));
    let (name, args, partial) = match func {
        Some(f) => (
            f.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            arg_text(f.get("arguments")),
            arg_text(f.get("partial_arguments")),
        ),
        None => (
            obj.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            arg_text(obj.get("arguments")),
            arg_text(obj.get("partial_arguments")),
        ),
    };
    // `partial_arguments` 是「截至目前完整的参数」，比碎片更可信。
    let next = if partial.trim().len() > args.trim().len() {
        partial
    } else {
        args
    };

    let mut pos = accs.iter().position(|a| match (a.index, index) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    });
    if pos.is_none() && !id.is_empty() {
        pos = accs.iter().position(|a| !a.id.is_empty() && a.id == id);
    }
    let pos = match pos {
        Some(p) => p,
        None => {
            accs.push(ToolCallAcc::default());
            accs.len() - 1
        }
    };

    let slot = &mut accs[pos];
    if slot.index.is_none() {
        slot.index = index;
    }
    if !id.is_empty() {
        slot.id = id;
    }
    if !name.is_empty() {
        slot.name = name;
    }
    slot.arguments = merge_arguments(&slot.arguments, &next);
}

/// 累积表 → OpenAI `message.tool_calls`。
///
/// 没有 `name` 的条目直接丢掉：那是上游的脏数据，编一个名字只会让客户端调用出错的工具。
///
/// ⚠️ 还要保证三件事，否则客户端只能**串行**执行、甚至只执行最后一个（L2-2）：
///
/// 1. `index` 连续且唯一（上游有时候给重复 index，甚至不给）；
/// 2. `id` 唯一（缺 id 或 id 撞了都重编，撞 id 会让客户端把两次调用当成同一次的重试）；
/// 3. 完全相同的重复调用（同名 + 同参 + 同/空 id）只留一条 —— 那是分片重复到达的残留。
pub(crate) fn finalize_tool_calls(accs: &[ToolCallAcc]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for a in accs.iter().filter(|a| !a.name.is_empty()) {
        let args = if a.arguments.trim().is_empty() {
            "{}".to_string()
        } else {
            a.arguments.clone()
        };
        let key = (a.name.clone(), args.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let mut id = a.id.clone();
        if id.is_empty() || ids.iter().any(|x| x == &id) {
            id = format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..24]);
        }
        ids.push(id.clone());
        let index = out.len() as u64;
        out.push(json!({
            "index": index,
            "id": id,
            "type": "function",
            "function": { "name": a.name, "arguments": args }
        }));
    }
    out
}

/// 有工具调用时 `finish_reason` 必须是 `tool_calls` —— 很多客户端只认这个值才去解析
/// 调用（`stop` 会被当成「模型只说了句话」）。上游有时候确实只给 `stop`。
pub(crate) fn finish_with_calls(finish: Option<String>, has_calls: bool) -> String {
    match (finish.as_deref(), has_calls) {
        (Some("tool_calls"), _) => "tool_calls".to_string(),
        (Some("length"), _) => "length".to_string(),
        (Some(other), true) if other != "stop" => other.to_string(),
        (_, true) => "tool_calls".to_string(),
        (Some(other), false) => other.to_string(),
        (None, false) => "stop".to_string(),
    }
}

/// 把 SSE 文本聚合成一条非流式 OpenAI 响应。
///
/// 拼 `delta.content`，同时把 `delta.tool_calls` 的分片拼回完整调用：
/// 网关对外是非流式，客户端拿到的是完整回答（含工具调用）。
fn aggregate_sse(text: &str, model: &str) -> Result<Value, (StatusCode, String)> {
    let mut content = String::new();
    let mut role = String::from("assistant");
    let mut finish: Option<String> = None;
    let mut id = String::from("chatcmpl-twinswitch");
    let mut usage = Value::Null;
    let mut accs: Vec<ToolCallAcc> = Vec::new();

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
            // 工具调用分片（标准 OpenAI 上游就是这么发的）。
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for c in calls {
                    absorb_tool_call(&mut accs, c);
                }
            }
            // 少数上游用单体形态给：`delta.tool_call` / `delta.function_call`。
            for key in ["tool_call", "function_call"] {
                if let Some(c) = delta.get(key) {
                    if c.is_object() {
                        absorb_tool_call(&mut accs, c);
                    }
                }
            }
        }
    }

    let calls = finalize_tool_calls(&accs);
    if content.is_empty() && calls.is_empty() {
        // 上游「成功」但一个字符都没给 —— 不许假装成空回答，要说清楚。
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "上游返回了流但没有任何正文，也没有可用的工具调用（可能是被安全策略拦了）。原始前 300 字：{}",
                text.chars().take(300).collect::<String>()
            ),
        ));
    }

    let finish_reason = finish_with_calls(finish, !calls.is_empty());
    let mut message = json!({ "role": role });
    // 标准里「只有工具调用」时 content 是 null；给空串会让部分客户端当成空回答吞掉。
    if content.is_empty() && !calls.is_empty() {
        message["content"] = Value::Null;
    } else {
        message["content"] = json!(content);
    }
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }

    Ok(json!({
        "id": id,
        "object": "chat.completion",
        "created": crate::modules::qoder_accounts::now_ms() / 1000,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
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
    // ⚠️⚠️ 凭据解析**只有这一个出口**：`gateway_pool::trae_logins()`。
    //
    // 2026-10-26 事故：这里原来自己扫 vault、自己解析 uid，与积分池各有一套 ——
    // 池子用四段回退能解出 842681600717036，这里只认 meta/oauth.id 解不出，
    // 于是「按 uid 找不到账号」，换 3 个号全 502。现在 uid 与 jwt 来自同一个函数，
    // 池子选中的 key 必然能在这里取到。
    //
    // `uid_hint` 为 None 时取第一个可用的（老行为的兜底路径，仍保留）。
    let logins = gateway_pool::trae_logins();
    let hit = match uid_hint {
        Some(hint) => logins.into_iter().find(|l| l.uid == hint),
        None => logins.into_iter().next(),
    }?;
    Some(TraeCred {
        uid: hit.uid,
        jwt: hit.jwt,
        device_id: hit.device_id,
        machine_id: hit.machine_id,
    })
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
/// 账号取自 Qoder 账号库（两个区域各自一份）。⚠️ 这里的 `model` 必须是**上游代号**
/// （`qmodel` / `dmodel` / `gmodel` …）—— 调用方 `forward()` 已经用
/// `gateway_models::upstream_model()` 把对外的 `GLM-5.3` 换成了代号。
/// 真机已验证：正文与 usage 都能取到。
async fn qoder_forward(
    scope: &str,
    model: &str,
    req: &Value,
    acc_id: Option<String>,
) -> Result<Value, (StatusCode, String)> {
    use crate::modules::qoder_realm::QoderRegion;

    let region = if scope.ends_with("global") {
        QoderRegion::Global
    } else {
        QoderRegion::Cn
    };
    let accounts = crate::modules::qoder_accounts::list(region)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("读 Qoder 账号失败：{e}")))?;
    let acc = pick_qoder_account(&accounts, acc_id.as_deref(), region)?;

    let messages = req.get("messages").cloned().unwrap_or_else(|| json!([]));
    // 客户端给的工具定义原样下发（Qoder 模板 `model_config.format = openai`，
    // 吃 OpenAI 形状的 tools）。之前完全没传 ⇒ 模型看不到工具 ⇒ 只会吐 XML 正文，
    // 提升成 tool_calls 后 content 变 null，客户端显示「已完成但空白」。
    let tools = req.get("tools").cloned().unwrap_or(Value::Null);
    let has_tools = tools
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    // `display_name` 只是给上游做界面/日志显示，但填真名比填代号更像官方客户端
    // （`DeepSeek-Flash` 而不是 `dfmodel`）。
    let shown = crate::modules::gateway_models::display_of(scope, model);
    let out = crate::modules::qoder_tools::chat_complete_ex(
        region,
        acc,
        model,
        &shown,
        &messages,
        Some(&tools),
    )
    .await
    .map_err(|e| {
            // 排队不是「网关坏了」，也不是「账号坏了」：换成 429 + 原话，
            // 客户端就会显示「请求过多/稍后重试」，而不是含糊的 502 服务器错误。
            // 状态码必须照实（「空」和「失败」不许长得一样）。
            let status = if e.contains("QUEUED|") {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::BAD_GATEWAY
            };
            let text = e
                .split_once("QUEUED|")
                .and_then(|(_, rest)| rest.split_once('|'))
                .map(|(_, msg)| format!("Qoder 上游排队中：{msg}"))
                .unwrap_or_else(|| format!("Qoder 上游：{e}"));
            (status, text)
        })?;
    // Qoder 的私有协议没有 function calling 通道，模型会把调用**写在正文里**
    // （`<function_calls><invoke …>`）。原样透出等于给客户端一段话术，这里提成标准 tool_calls。
    Ok(lift_qoder_tool_calls(out, has_tools))
}

/// `QUEUED|<秒>|<文案>` → `(秒, 文案)`。标记不一定在开头（外面可能包了 `上游报错：`）。
fn queue_parts(e: &str) -> Option<(u64, String)> {
    let rest = e.split_once("QUEUED|")?.1;
    let mut it = rest.splitn(2, '|');
    let wait = it.next().unwrap_or("30").trim().parse().unwrap_or(30);
    Some((wait, it.next().unwrap_or("上游排队中").to_string()))
}

/// Qoder 真流式：选号 → 打开流 → **首字探测** → 交出响应体。
///
/// 排队（10605）在这里按官方语义处理：`sleep(retryAfterSeconds)` 再来一次，
/// 直到预算用完才回 429 原文。换号是无效的 —— 队列是模型级的。
/// 从本区域账号里挑出**积分池选中的那一个**。
///
/// ⚠️ 为什么不「找不到就退回第一个可用号」：那正是 2026-10-26 的 bug ——
/// 池子轮换到 A、发送端却永远用 B，两轮报错一模一样，日志还写「换了 2 个账号」。
/// 指名的号取不到时必须**说清是哪个号**，否则永远查不出「换号是假的」。
fn pick_qoder_account<'a>(
    accounts: &'a [crate::modules::qoder_accounts::QoderAccount],
    acc_id: Option<&str>,
    region: crate::modules::qoder_realm::QoderRegion,
) -> Result<&'a crate::modules::qoder_accounts::QoderAccount, (StatusCode, String)> {
    if let Some(id) = acc_id.map(str::trim).filter(|s| !s.is_empty()) {
        return accounts.iter().find(|a| a.id == id).ok_or_else(|| {
            (
                StatusCode::BAD_GATEWAY,
                format!(
                    "{} 里找不到账号 {id}（积分池选中的账号可能已被删除，刷新一下账号页）",
                    region.display_name()
                ),
            )
        });
    }
    accounts.iter().find(|a| a.is_usable()).ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{} 没有可用的 Qoder 账号（先在账号页导入/登录）",
                region.display_name()
            ),
        )
    })
}

async fn qoder_stream(
    scope: &str,
    model: &str,
    req: &Value,
    mut trace: Trace,
    acc_id: Option<String>,
) -> Result<Upstream, (StatusCode, String)> {
    use crate::modules::qoder_realm::QoderRegion;

    let region = if scope.ends_with("global") {
        QoderRegion::Global
    } else {
        QoderRegion::Cn
    };
    let accounts = crate::modules::qoder_accounts::list(region)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("读 Qoder 账号失败：{e}")))?;
    let acc = pick_qoder_account(&accounts, acc_id.as_deref(), region)?;

    let messages = req.get("messages").cloned().unwrap_or_else(|| json!([]));
    let tools = req.get("tools").cloned().unwrap_or(Value::Null);
    let shown = crate::modules::gateway_models::display_of(scope, model);

    let budget = qoder_queue_budget();
    let started = std::time::Instant::now();
    loop {
        let resp = crate::modules::qoder_tools::chat_open(
            region,
            acc,
            model,
            &shown,
            &messages,
            Some(&tools),
        )
        .await
        .map_err(|e| {
            if let Some((_, msg)) = queue_parts(&e) {
                trace.note_queued();
                (StatusCode::TOO_MANY_REQUESTS, format!("Qoder 上游排队中：{msg}"))
            } else {
                (StatusCode::BAD_GATEWAY, format!("Qoder 上游：{e}"))
            }
        })?;

        let mut reader = StreamReader::new(resp, Kind::Qoder);
        match reader.next_batch().await {
            Ok(Some(first)) => {
                return Ok(Upstream::Sse(crate::modules::gateway_stream::respond(
                    Emitter::new(model),
                    reader,
                    first,
                    trace,
                )))
            }
            Ok(None) => {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    "Qoder 返回了流，但既没有正文也没有工具调用".to_string(),
                ));
            }
            Err(e) => {
                if let Some((wait, msg)) = queue_parts(&e) {
                    let spent = started.elapsed().as_secs();
                    let nap = wait.clamp(1, 45);
                    if spent + nap <= budget {
                        // 上游说的 retryAfterSeconds 要**照等**：官方客户端就是这么等的。
                        tokio::time::sleep(std::time::Duration::from_secs(nap)).await;
                        continue;
                    }
                    trace.note_queued();
                    return Err((
                        StatusCode::TOO_MANY_REQUESTS,
                        format!(
                            "Qoder 上游排队中：{msg}（已等 {spent} 秒仍没排到 —— 换号没用，\
                             队列是模型级的；可换冷门模型试试）"
                        ),
                    ));
                }
                return Err((StatusCode::BAD_GATEWAY, e));
            }
        }
    }
}

/// 流式路径的排队预算（与 `qoder_tools::queue_budget_secs` 同一口径）。
fn qoder_queue_budget() -> u64 {
    std::env::var("TWIN_TS_QUEUE_BUDGET")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(240)
}

// ---------------------------------------------------------------------------
// Qoder：正文里的 `<function_calls>` → OpenAI `tool_calls`
//
// 实测（2026-10-24）Qoder 的「auto」会把工具调用输出成 Claude 风格的 XML 文本：
//
// ```text
// <function_calls>
//   <invoke name="get_weather">
//     <parameter name="city">上海</parameter>
//   </invoke>
// </function_calls>
// ```
//
// 客户端只认 `message.tool_calls`，看到这段文本只会当成模型在念台词。
// 所以网关在出口处把它提出来，并从正文里删掉。
// ---------------------------------------------------------------------------

/// XML 实体还原（顺序不能换：`&amp;` 必须最后，否则 `&amp;lt;` 会被二次还原）。
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 取标签属性值，允许单双引号。要求 `key` 前面是空白，避免 `pname=` 命中 `name=`。
fn xml_attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=");
    let mut from = 0usize;
    while let Some(rel) = tag[from..].find(&pat) {
        let at = from + rel;
        let prev_ok = tag[..at].chars().next_back().map(|c| c.is_whitespace()).unwrap_or(false);
        let rest = &tag[at + pat.len()..];
        let (quote, rest2) = if let Some(r) = rest.strip_prefix('"') {
            ('"', r)
        } else if let Some(r) = rest.strip_prefix('\'') {
            ('\'', r)
        } else {
            from = at + pat.len();
            continue;
        };
        if !prev_ok {
            from = at + pat.len();
            continue;
        }
        let end = rest2.find(quote)?;
        return Some(rest2[..end].to_string());
    }
    None
}

/// 参数值：能当 JSON 就按 JSON（模型常给数字/布尔/对象），否则按字符串。
fn xml_param_value(raw: &str) -> Value {
    let t = unescape_xml(raw).trim().to_string();
    if t.is_empty() {
        return json!("");
    }
    let looks_json = t.starts_with('{')
        || t.starts_with('[')
        || t.starts_with('"')
        || t == "true"
        || t == "false"
        || t == "null";
    if looks_json {
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            return v;
        }
    }
    // 纯数字按数字给（`"count": "3"` 会让要求 integer 的 schema 校验失败）。
    if t.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c)) && t.parse::<f64>().is_ok() {
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            return v;
        }
    }
    json!(t)
}

/// 解析一段 `<invoke …>…</invoke>`。
fn parse_invoke(seg: &str, index: usize) -> Option<Value> {
    let name = xml_attr(seg, "name")?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let mut args = serde_json::Map::new();
    let mut cur = 0usize;
    while let Some(rel) = seg[cur..].find("<parameter") {
        let start = cur + rel;
        let Some(gt_rel) = seg[start..].find('>') else { break };
        let head_end = start + gt_rel;
        let head = &seg[start..head_end];
        let Some(pname) = xml_attr(head, "name").map(|s| s.trim().to_string()) else {
            cur = head_end + 1;
            continue;
        };
        // 自闭合 `<parameter name="x"/>` = 空值。
        if head.trim_end().ends_with('/') {
            args.insert(pname, json!(""));
            cur = head_end + 1;
            continue;
        }
        let val_start = head_end + 1;
        let Some(close_rel) = seg[val_start..].find("</parameter>") else { break };
        args.insert(pname, xml_param_value(&seg[val_start..val_start + close_rel]));
        cur = val_start + close_rel + "</parameter>".len();
    }
    Some(json!({
        "index": index,
        "id": format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..24]),
        "type": "function",
        "function": {
            "name": name,
            "arguments": serde_json::to_string(&Value::Object(args)).unwrap_or_else(|_| "{}".into()),
        }
    }))
}

/// 把正文里的 `<function_calls>` 块提出来。返回 `(剩下的正文, 工具调用)`。
///
/// 没闭合的块**原样留在正文里**（宁可让用户看到原始文本，也不能吞掉半段内容）。
fn lift_text_tool_calls(text: &str) -> (String, Vec<Value>) {
    const OPEN: &str = "<function_calls";
    const CLOSE: &str = "</function_calls>";
    let mut calls: Vec<Value> = Vec::new();
    let mut rest = String::new();
    let mut cursor = 0usize;

    while let Some(rel) = text[cursor..].find(OPEN) {
        let start = cursor + rel;
        let Some(close_rel) = text[start..].find(CLOSE) else { break };
        let end = start + close_rel + CLOSE.len();
        rest.push_str(&text[cursor..start]);
        let block = &text[start..end];

        let mut inner = 0usize;
        while let Some(inv_rel) = block[inner..].find("<invoke") {
            let inv_start = inner + inv_rel;
            let Some(inv_close) = block[inv_start..].find("</invoke>") else { break };
            let inv_end = inv_start + inv_close + "</invoke>".len();
            if let Some(call) = parse_invoke(&block[inv_start..inv_end], calls.len()) {
                calls.push(call);
            }
            inner = inv_end;
        }
        cursor = end;
    }
    rest.push_str(&text[cursor..]);
    (rest.trim().to_string(), calls)
}

/// 把 Qoder 的完整返回里的正文工具调用提成 `message.tool_calls`。
///
/// ⚠️ `allow_tools` = 本次请求**客户端真的给了工具定义**。没给却提升成 `tool_calls`，
/// 调用无处可去，客户端只会显示「已完成 + 一片空白」（2026-10-24 用户：带文件分析
/// 那两次 7 秒就结束、什么都不出）。那种情况剥掉 XML、写明原因，至少不是空白。
fn lift_qoder_tool_calls(mut v: Value, allow_tools: bool) -> Value {
    let Some(content) = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return v;
    };
    if !content.contains("<function_calls") {
        return v;
    }
    let (rest, calls) = lift_text_tool_calls(&content);
    if calls.is_empty() {
        return v;
    }
    if !allow_tools {
        let names: Vec<&str> = calls
            .iter()
            .filter_map(|c| c.pointer("/function/name").and_then(Value::as_str))
            .collect();
        let hint = if names.is_empty() {
            "（模型想调用工具，但这次请求没有带工具定义，无法执行。）".to_string()
        } else {
            format!(
                "（模型想调用工具 {}，但这次请求没有带工具定义，无法执行。）",
                names.join("、")
            )
        };
        let text = if rest.is_empty() {
            hint
        } else {
            format!("{rest}\n\n{hint}")
        };
        if let Some(msg) = v.pointer_mut("/choices/0/message").and_then(Value::as_object_mut) {
            msg.insert("content".into(), json!(text));
        }
        return v;
    }
    if let Some(msg) = v.pointer_mut("/choices/0/message").and_then(Value::as_object_mut) {
        msg.insert(
            "content".into(),
            if rest.is_empty() { Value::Null } else { json!(rest) },
        );
        msg.insert("tool_calls".into(), json!(calls));
    }
    if let Some(ch) = v.pointer_mut("/choices/0").and_then(Value::as_object_mut) {
        ch.insert("finish_reason".into(), json!("tool_calls"));
    }
    v
}

/// Trae 上游请求体（含 proto 归一化 + 会话 ID）。
///
/// ⚠️ 会话 ID 用**指纹派生**，不再每轮 `Uuid::new_v4()`：每轮换新 ID，上游就没法把
/// 这几次请求认成同一个会话，前缀缓存 100% 失效。多轮 agent 里这是最大的隐性成本
/// （报告 §3.2：45k tokens/轮 × 60 轮 ≈ 2.7M 输入，绝大多数是同一段前缀被反复 prefill）。
/// 指纹只含「对话开始时就不变」的东西，见 [`crate::modules::gateway_stream::session_seed`]。
fn trae_body(model: &str, req: &Value, cred: &TraeCred) -> (Value, String) {
    let (config_name, model_name) = trae_model_config(model);
    let messages = req.get("messages").cloned().unwrap_or_else(|| json!([]));
    let seed = crate::modules::gateway_stream::session_seed(
        &model_name,
        &cred.uid,
        &messages,
        req.get("tools"),
    );
    let mut body = req.clone();
    if let Some(obj) = body.as_object_mut() {
        // 上游要求 `messages[].content` 是数组（字符串形态会回 4001），
        // 且历史里的 `assistant.tool_calls[].function` 必须改成 `function_call`
        // —— 不改第二轮带工具结果就 502（proto `LLMRawMessage read field 5 'ToolCalls'`）。
        normalize_trae_messages(obj);
        // 上游要求 `tools[].function.parameters` 是字符串（OpenAI 标准是对象，
        // WorkBuddy 发对象过来不转就 400：cannot unmarshal object into ... parameters of type string）。
        normalize_trae_tools(obj);
        // `tool_choice` 只认 auto / required / 具体函数名；`none` 要连 tools 一起删。
        normalize_trae_tool_choice(obj);
        obj.insert("config_name".into(), json!(config_name));
        obj.insert("model_name".into(), json!(model_name));
        // 上游只支持流式。
        obj.insert("stream".into(), json!(true));
        obj.insert("function".into(), json!(TRAE_FUNCTION));
        obj.insert("user_id".into(), json!(cred.uid));
        obj.insert("device_id".into(), json!(cred.device_id));
        obj.insert("machine_id".into(), json!(cred.machine_id));
        obj.insert(
            "conversation_id".into(),
            json!(crate::modules::gateway_stream::session_id(&seed, "trae-conv")),
        );
        obj.insert(
            "session_id".into(),
            json!(crate::modules::gateway_stream::session_id(&seed, "trae-sess")),
        );
        obj.insert(
            "project_id".into(),
            json!(crate::modules::gateway_stream::session_id(&seed, "trae-proj")),
        );
        obj.insert("workspace_id".into(), json!("e04cdd"));
        obj.insert("prompt_max_tokens".into(), json!(168_000));
        obj.insert("mode".into(), json!("FunctionCall"));
        obj.insert("ide_version".into(), json!(TRAE_IDE_VERSION));
        obj.insert("ide_version_code".into(), json!(TRAE_IDE_VERSION_CODE));
        obj.insert("app_id".into(), json!(TRAE_APP_ID));
        obj.insert("package_type".into(), json!("stable_cn"));
    }
    (body, format!("{TRAE_AGENT_HOST}{TRAE_LLM_CHAT_PATH}"))
}

/// 发一次 Trae 请求，**不看状态码**（调用方自己判，才说得清错在哪）。
///
/// `timeout_s` 分两档：聚合路径 180 s（整段生成完才算完），流式路径给 900 s
/// —— 流式下首字之外还有整段生成，用 180 s 卡整段会把「模型在慢慢写」误判成
/// 「上游挂了」，然后换号重来，等于把已经花掉的算力全丢掉（报告 §3.3）。
async fn trae_post(
    model: &str,
    req: &Value,
    timeout_s: u64,
    uid: Option<&str>,
) -> Result<reqwest::Response, (StatusCode, String)> {
    let cred = trae_cred(uid).ok_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            match uid {
                // 指名要的账号取不到 ⇒ 必须说清是哪个号，否则用户只会看到「找不到登录态」
                // 却不知道是池子里选中的那个号缺凭据（该号可能没导入到本工具账号库）。
                Some(u) => format!(
                    "找不到账号 {u} 的可用 Trae 登录态（该号可能只有切换载体、没有网页凭证；                     到「账号管理 → Trae 账号」导入一次本机登录态）"
                ),
                None => "找不到可用的 Trae 登录态（storage.json 里没有解不开的授权条目）"
                    .to_string(),
            },
        )
    })?;
    let (body, url) = trae_body(model, req, &cred);
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(timeout_s))
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
    Ok(resp)
}

async fn trae_forward(
    model: &str,
    req: &Value,
    uid: Option<String>,
) -> Result<Value, (StatusCode, String)> {
    let resp = trae_post(model, req, 180, uid.as_deref()).await?;
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

/// Trae 真流式。
///
/// **首字探测**在这里做：先把第一批增量读到手里，再交出响应体。这样「上游 200 但
/// 立刻报错 / 空流」仍然能走换号重试（一旦开始往客户端发字节就没法换了）。
async fn trae_stream(
    model: &str,
    req: &Value,
    trace: Trace,
    uid: Option<String>,
) -> Result<Upstream, (StatusCode, String)> {
    let resp = trae_post(model, req, 900, uid.as_deref()).await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "Trae 上游返回 HTTP {}：{}",
                status.as_u16(),
                text.chars().take(400).collect::<String>()
            ),
        ));
    }
    let mut reader = StreamReader::new(resp, Kind::Trae);
    let first = match reader.next_batch().await {
        Ok(Some(v)) => v,
        Ok(None) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                "Trae 返回了事件流，但没有正文也没有工具调用".to_string(),
            ));
        }
        Err(e) => {
            return Err((StatusCode::BAD_GATEWAY, e));
        }
    };
    // ⚠️ 「只有思考内容」也算有效首包：模型可能先想 30 秒再出正文。
    // 这里判太严会把正常请求当成失败、换号重来（等于白烧掉一次生成）。
    let useful = first
        .iter()
        .any(|d| has_payload(d) || matches!(d, Delta::Reasoning(_)));
    if !useful {
        return Err((
            StatusCode::BAD_GATEWAY,
            "Trae 只回了结束事件，没有正文也没有工具调用".to_string(),
        ));
    }
    Ok(Upstream::Sse(crate::modules::gateway_stream::respond(
        Emitter::new(model),
        reader,
        first,
        trace,
    )))
}

/// 这一包增量里有没有「真内容」（正文或工具调用）。
///
/// 只有思考内容不算 —— 但也不当失败（思考说明上游在干活，继续等正文）。
fn has_payload(d: &Delta) -> bool {
    matches!(d, Delta::Content(_) | Delta::ToolCall(_))
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
    // 没有函数名的 tool 是上游会直接拒的脏数据（`FunctionDefinition.name` 必填），
    // 留着只会让整条请求 400，不如丢掉（照参考实现 `payload.rs::normalize_tools`）。
    tools.retain(|tool| {
        tool.get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .map(|n| !n.trim().is_empty())
            .unwrap_or(false)
    });
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

/// `tool_choice` 归一化：上游只认 `auto` / `required` / **具体函数名** / 缺省。
///
/// OpenAI 的 `{"type":"function","function":{"name":"x"}}` 和 `"none"` 上游都不吃：
/// `none` 要连 `tools` 一起删掉，具名要拍平成字符串。
fn normalize_trae_tool_choice(obj: &mut serde_json::Map<String, Value>) {
    let Some(choice) = obj.remove("tool_choice") else {
        return;
    };
    let suppress = |object: &mut serde_json::Map<String, Value>| {
        object.remove("tools");
        object.remove("functions");
    };
    match choice {
        Value::String(text) => {
            if text.trim().eq_ignore_ascii_case("none") {
                suppress(obj);
            } else {
                obj.insert("tool_choice".into(), Value::String(text));
            }
        }
        Value::Object(map) => {
            let kind = map
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_lowercase)
                .unwrap_or_default();
            match kind.as_str() {
                "none" => suppress(obj),
                "auto" | "required" => {
                    obj.insert("tool_choice".into(), Value::String(kind));
                }
                "function" => {
                    let name = map
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .or_else(|| map.get("name").and_then(Value::as_str))
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .unwrap_or("auto")
                        .to_string();
                    obj.insert("tool_choice".into(), Value::String(name));
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// 消息数组的改写：`content` 字符串 → 数组、`assistant.tool_calls[].function` → `function_call`。
///
/// ⚠️⚠️ **`function_call` 这一步是硬要求，不是审美问题**。Trae 的 proto 里
/// `LLMRawMessage` 的 `ToolCalls`（field 5）元素类型与我们发过去的 OpenAI 形态不兼容，
/// 原样回传会得到（2026-10-24 用户真机报错原文）：
///
/// ```text
/// code 2001: LLMUtilsChatStreamFailed ... rpc error: code = 7
/// desc = 'idecopilot.LLMRawChatV2Request read field 8 'Messages' error:
/// 'idecopilot.LLMRawMessage read field 5 'ToolCalls' error:
/// 'idecopilot.ToolCall read field 4' error ...'
/// ```
///
/// 症状很有欺骗性：**第一轮（模型发起工具调用）正常，第二轮把工具结果带回去就 502**，
/// 所以看起来像「选中 Trae 模型就报 502，WorkBuddy 的模型正常」。
/// 同一件事在参考实现 `buddy-switch-gateway/src/trae/payload.rs` 的模块头部也写着：
/// 「`assistant.tool_calls[].function` 要改成 `function_call`（历史消息回传时）」。
fn normalize_trae_messages(obj: &mut serde_json::Map<String, Value>) {
    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for m in messages.iter_mut() {
        let Some(m) = m.as_object_mut() else { continue };

        if m.get("role").and_then(Value::as_str) == Some("assistant") {
            if let Some(calls) = m.get_mut("tool_calls").and_then(Value::as_array_mut) {
                let mut kept: Vec<Value> = Vec::with_capacity(calls.len());
                for call in calls.iter_mut() {
                    let Some(call) = call.as_object_mut() else { continue };
                    // `function` → `function_call`（不能两者并存，否则上游按未知字段处理）
                    if let Some(function) = call.remove("function") {
                        call.insert("function_call".into(), function);
                    }
                    // 没有函数名的调用上游一定拒，留着会拖垮整条请求。
                    let named = call
                        .get("function_call")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .map(|n| !n.trim().is_empty())
                        .unwrap_or(false);
                    if named {
                        kept.push(Value::Object(call.clone()));
                    }
                }
                if kept.is_empty() {
                    m.remove("tool_calls");
                } else {
                    *calls = kept;
                }
            }
        }

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
    let mut finish: Option<String> = None;
    let mut usage = Value::Null;
    let mut error: Option<String> = None;
    let mut accs: Vec<ToolCallAcc> = Vec::new();

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
                //
                // 工具调用也在这个事件里（`tool_calls`）。**必须收**：模型选了工具时
                // 正文往往是空的，只收正文就会把「调用了工具」当成「上游给了个空回答」
                // 报 502（2026-10-24 实测 DeepSeek-V4-Flash）。
                if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
                    for c in calls {
                        absorb_tool_call(&mut accs, c);
                    }
                }
                for key in ["tool_call", "function_call"] {
                    if let Some(c) = v.get(key) {
                        if c.is_object() {
                            absorb_tool_call(&mut accs, c);
                        }
                    }
                }
            }
            "token_usage" => usage = v.clone(),
            "done" => {
                if let Some(f) = v.get("finish_reason").and_then(Value::as_str) {
                    finish = Some(f.to_string());
                }
            }
            "turn_completion" => {
                if finish.is_none() {
                    finish = Some("stop".into());
                }
            }
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

    let calls = finalize_tool_calls(&accs);
    if answer.trim().is_empty() && calls.is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!(
                "Trae 返回了事件流，但没有正文也没有工具调用（只可能是空回答或被拦）。原始前 500 字：{}",
                text.chars().take(500).collect::<String>()
            ),
        ));
    }

    let finish_reason = finish_with_calls(finish, !calls.is_empty());
    let mut message = json!({ "role": "assistant" });
    if answer.is_empty() && !calls.is_empty() {
        message["content"] = Value::Null;
    } else {
        message["content"] = json!(answer);
    }
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }

    Ok(json!({
        "id": format!("chatcmpl-trae-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion",
        "created": crate::modules::qoder_accounts::now_ms() / 1000,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }],
        "usage": usage,
    }))
}

/// 从 WorkBuddy 账号数组里挑**指定 uid** 的 token（纯函数，单测直接喂数组）。
///
/// ⚠️ 这就是「换号」能不能真生效的分水岭：按 uid 挑，才能把候选账号和实际发出去的
/// token 对上；按作用域挑永远是数组里第一个 —— 多个 WB 账号时换了号还是同一个 token。
fn pick_wb_token(accounts: &[Value], uid: &str) -> Option<String> {
    accounts.iter().find_map(|a| {
        if a.get("uid").and_then(Value::as_str).map(str::trim) != Some(uid) {
            return None;
        }
        a.get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    })
}

/// 按**账号**取 bearer token。
///
/// `key` 是池子条目的全局键：`qoder:{账号id}` / `workbuddy:{uid}` / `trae:{uid}`。
/// 取不到时回落到 [`token_of`]（按作用域取第一个）—— 那条路只保证「这个作用域还有别的号
/// 能用」，不保证是我们挑中的那个；所以调用方的错误信息里会带上账号名。
fn token_of_key(key: &str, scope: &str) -> Option<String> {
    let ident = key
        .split_once(':')
        .map(|(_, v)| v.trim())
        .unwrap_or("")
        .to_string();
    if !ident.is_empty() {
        match scope {
            "qoder-cn" | "qoder-global" => {
                let list = crate::modules::qoder_accounts::list_all().ok()?;
                return list
                    .into_iter()
                    .find(|a| a.id == ident && format!("qoder-{}", a.region.as_str()) == scope)
                    .and_then(|a| a.access_token)
                    .filter(|t| !t.trim().is_empty());
            }
            "wb-cn" | "wb-global" => {
                let text = std::fs::read_to_string(
                    crate::modules::config::store_dir().join("workbuddy-accounts.json"),
                )
                .ok()?;
                let v: Value = serde_json::from_str(&text).ok()?;
                return pick_wb_token(v.get("accounts")?.as_array()?, &ident);
            }
            _ => {}
        }
    }
    token_of(scope)
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

    /// 首条 `developer` 必须还原成 `system`：上游渠道校验只认字面量 system，
    /// 发 developer 就是 400 / 11128（2026-10-27 真机 A/B 证实）。
    #[test]
    fn first_developer_role_is_normalized() {
        let mut body = json!({
            "messages": [
                { "role": "developer", "content": "You are a helpful assistant." },
                { "role": "user", "content": "hi" }
            ]
        });
        assert!(normalize_first_role(&mut body));
        assert_eq!(body["messages"][0]["role"], json!("system"));
        // 后续消息不动（只还原首条）
        assert_eq!(body["messages"][1]["role"], json!("user"));
        // 内容一个字都不许改
        assert_eq!(
            body["messages"][0]["content"],
            json!("You are a helpful assistant.")
        );
    }

    /// 已经是 system 的不能被改坏；developer 出现在非首条也不动。
    #[test]
    fn role_normalization_is_surgical() {
        let mut body = json!({
            "messages": [
                { "role": "system", "content": "a" },
                { "role": "developer", "content": "b" }
            ]
        });
        assert!(!normalize_first_role(&mut body));
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(body["messages"][1]["role"], json!("developer"));

        // 没有 messages 也不能 panic
        let mut empty = json!({});
        assert!(!normalize_first_role(&mut empty));
        let mut not_array = json!({ "messages": "x" });
        assert!(!normalize_first_role(&mut not_array));
    }

    /// 该换号的：限流 / 额度耗尽 / 凭据死 / 5xx / 传输失败。
    #[test]
    fn failover_on_account_level_errors() {
        assert!(should_failover(429, "too many requests"));
        assert!(should_failover(429, r#"{"code":6804,"msg":"您的使用量已超出频率限制"}"#));
        assert!(should_failover(403, "quota exceeded"));
        assert!(should_failover(401, "TOKEN_EXPIRE"));
        assert!(should_failover(402, "balance"));
        assert!(should_failover(503, "gateway"));
        assert!(should_failover(0, "error sending request"));
        // WorkBuddy 频道校验（11128）：真机探针证明同一份 body 换号/换尺寸都正常，
        // 它对「这个号」是死判据 ⇒ 必须换号，不能把 400 原样丢给用户
        // （2026-10-26 用户实测：只回一句 unapproved channel，一次都没换号）。
        assert!(should_failover(
            400,
            r#"{"code":"11128","msg":"Illegal API invocation from an unapproved channel"}"#
        ));
    }

    /// 不该换号的：参数错（换了也一样错）、模型不存在、以及**排队**。
    ///
    /// 排队必须排除：10605 是全局排队，换号只会再排一次，白等一轮。
    #[test]
    fn no_failover_on_request_level_errors() {
        assert!(!should_failover(400, "invalid request: messages[0].content"));
        assert!(!should_failover(404, "model not found"));
        assert!(!should_failover(200, r#"{"code":10605,"message":"CALL_COUNT_LIMIT"}"#));
        assert!(!should_failover(401, r#"{"code":-1,"isQueued":true}"#));
    }

    /// 换号能不能真生效，取决于「挑 token 用的是 uid 还是作用域」。
    #[test]
    fn token_is_picked_by_uid_not_by_position() {
        let accounts = vec![
            json!({"uid": "u1", "access_token": "TOKEN-1"}),
            json!({"uid": "u2", "access_token": "TOKEN-2"}),
            json!({"uid": "u3", "access_token": "  "}),
        ];
        assert_eq!(pick_wb_token(&accounts, "u2").as_deref(), Some("TOKEN-2"));
        assert_eq!(pick_wb_token(&accounts, "u3"), None, "空 token 不算可用");
        assert_eq!(pick_wb_token(&accounts, "u9"), None, "不认识的 uid 不许退化成第一个");
    }

    #[test]
    fn status_reports_not_running_before_start() {
        // 与 `lan_guard_lets_loopback_through` 互斥：那个用例会临时把 LAN_ACTIVE 置 true。
        let _g = lan_test_lock();
        let s = status();
        assert_eq!(s["running"], false);
        assert_eq!(s["port"], GATEWAY_PORT);
        // 没绑 0.0.0.0 时不许冒充「局域网地址」在跑
        assert_eq!(s["lan"], false);
        assert_eq!(s["addr"], base_url("127.0.0.1"));
        assert!(s["lanAddr"].is_null());
    }

    // ---------------------------------------------------------------- 局域网共享

    /// 默认只绑回环：这条判据错了 = 把整个局域网放进来花额度。
    #[test]
    fn default_bind_is_loopback_only() {
        assert_eq!(bind_ip(false), [127, 0, 0, 1]);
        assert_eq!(bind_ip(true), [0, 0, 0, 0]);
        // `lan` 缺省必须是关（`lan_enabled()` 的 `unwrap_or(false)`）——
        // 这条不在这里断真实配置，避免测试依赖本机 `gateway.json`。
    }

    /// 两个开关是**合并写**：写 `lan` 不许把 `enabled` 抹掉，反之亦然。
    ///
    /// 曾经的写法是整份覆盖，加上 `lan` 之后点一次总开关就会把局域网设置清掉。
    #[test]
    fn flag_write_merges_instead_of_overwriting() {
        let body = merge_flag(json!({ "enabled": true }), "lan", true);
        assert_eq!(body, json!({ "enabled": true, "lan": true }));
        // 反向：开总开关不许丢 lan
        let body = merge_flag(body, "enabled", false);
        assert_eq!(body, json!({ "enabled": false, "lan": true }));
        // 坏文件（不是对象）当空处理，不 panic
        let body = merge_flag(json!("oops"), "lan", true);
        assert_eq!(body, json!("oops"), "非对象原样返回，不硬塞字段");
    }

    /// 局域网地址必须是 `http://<ip>:<port>/v1`，端口跟着常量走（别在两处各写一个数）。
    #[test]
    fn lan_url_keeps_single_port_source() {
        let u = base_url("192.168.6.226");
        assert_eq!(u, format!("http://192.168.6.226:{GATEWAY_PORT}/v1"));
        assert!(u.ends_with(&format!(":{GATEWAY_PORT}/v1")));
    }

    /// 探测到的地址绝不可能是回环/未指定（否则「局域网地址」会指回本机，等于骗人）。
    #[test]
    fn lan_probe_never_reports_loopback() {
        if let Some(ip) = lan_ip() {
            let parsed: std::net::IpAddr = ip.parse().expect("探测结果必须是合法 IP");
            assert!(!parsed.is_loopback(), "不许把 127.0.0.1 当局域网地址：{ip}");
            assert!(!parsed.is_unspecified(), "不许把 0.0.0.0 当局域网地址：{ip}");
        }
    }

    /// 会改全局 `LAN_ACTIVE` 的测试必须串行（否则并行跑 `status()` 的用例会看到中间态）。
    fn lan_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// 局域网模式下**本机来源必须放行**：这条一旦坏了，本机所有客户端都会被 401 打死。
    ///
    /// 用真实监听 + 真实请求验证（纯函数测不到 `ConnectInfo` 到底有没有进 extensions ——
    /// 那正是这里最容易踩的坑：忘了 `into_make_service_with_connect_info` 就永远拿不到来源）。
    #[tokio::test]
    async fn lan_guard_lets_loopback_through() {
        let _g = lan_test_lock();
        let app = Router::new()
            .route("/v1/models", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(lan_guard));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("临时端口应能绑定");
        let port = listener.local_addr().expect("取端口").port();
        LAN_ACTIVE.store(true, Ordering::Relaxed);
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });

        // `no_proxy()`：本机环境变量里常挂着沙箱代理，走代理就测不到自己了。
        let c = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("建客户端");
        let r = c
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await
            .expect("本机请求应发出");
        let code = r.status().as_u16();
        LAN_ACTIVE.store(false, Ordering::Relaxed);
        assert_eq!(code, 200, "回环来源被门禁拦了 —— connect info 没进 extensions");
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

#[cfg(test)]
mod tool_call_tests {
    use super::*;

    /// arguments 的三种真实形态：真分片 / 完整快照 / 重复旧值。
    #[test]
    fn merge_arguments_handles_all_three_shapes() {
        // 真分片：`{"city":"` + `上海"}`
        assert_eq!(
            merge_arguments("{\"city\":\"", "上海\"}"),
            "{\"city\":\"上海\"}"
        );
        // 快照扩展：用新的完整值，不能拼接
        assert_eq!(
            merge_arguments(r#"{"city":"上海"}"#, r#"{"city":"上海","n":1}"#),
            r#"{"city":"上海","n":1}"#
        );
        // 重复发旧快照
        assert_eq!(merge_arguments(r#"{"a":1}"#, r#"{"a""#), r#"{"a":1}"#);
        assert_eq!(merge_arguments("", ""), "");
    }

    /// 2026-10-24 用户实测：Trae + DeepSeek 带 tools 时**正文为空、只有 tool_calls**，
    /// 以前被当成「上游给了个空回答」直接 502。
    #[test]
    fn trae_tool_only_reply_is_not_an_empty_answer() {
        let sse = concat!(
            "id:1 event:output\n",
            "id:1 data:{\"response\":\"\"}\n",
            "id:1 data:{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"上海\\\"}\"}}]}\n",
            "id:2 event:done\n",
            "id:2 data:{\"finish_reason\":\"tool_calls\"}\n",
        );
        let v = aggregate_trae_sse(sse, "deepseek-v4-flash").expect("工具调用不能被当成空回答");
        let msg = &v["choices"][0]["message"];
        assert!(msg["content"].is_null(), "只有工具调用时 content 应为 null");
        assert_eq!(
            msg["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            msg["tool_calls"][0]["function"]["arguments"],
            r#"{"city":"上海"}"#
        );
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    }

    /// glm-5.2 的形态：正文有一句「我来帮你查一下」，finish_reason 是 tool_calls，
    /// 工具调用分两片到达（`partial_arguments` 是完整快照）。
    #[test]
    fn trae_partial_arguments_snapshot_is_not_duplicated() {
        let sse = concat!(
            "id:1 event:output\n",
            "id:1 data:{\"response\":\"我来帮你查一下。\"}\n",
            "id:1 data:{\"tool_calls\":[{\"index\":0,\"id\":\"call_b\",\"function_call\":{\"name\":\"get_weather\",\"partial_arguments\":\"{\\\"city\\\":\"}}]}\n",
            "id:1 data:{\"tool_calls\":[{\"index\":0,\"id\":\"call_b\",\"function_call\":{\"name\":\"get_weather\",\"partial_arguments\":\"{\\\"city\\\":\\\"上海\\\"}\"}}]}\n",
            "id:2 event:done\n",
            "id:2 data:{\"finish_reason\":\"tool_calls\"}\n",
        );
        let v = aggregate_trae_sse(sse, "glm-5.2").expect("有正文有工具调用应当成功");
        let msg = &v["choices"][0]["message"];
        assert_eq!(msg["content"], "我来帮你查一下。");
        assert_eq!(
            msg["tool_calls"][0]["function"]["arguments"],
            r#"{"city":"上海"}"#,
            "partial_arguments 是快照，拼起来会变成两段 JSON"
        );
        // `function_call` 必须换成 OpenAI 的 `function`
        assert!(msg["tool_calls"][0].get("function_call").is_none());
    }

    /// 标准 OpenAI 上游：delta.tool_calls 真分片，必须按 index 拼回一次调用。
    #[test]
    fn openai_style_fragments_are_joined_by_index() {
        let sse = concat!(
            "data:{\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n",
            "data:{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n",
            "data:{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\\\"\"}}]}}]}\n",
            "data:{\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"上海\\\"}\"}}]}}]}\n",
            "data:{\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            "data:[DONE]\n",
        );
        let v = aggregate_sse(sse, "gpt-test").expect("分片工具调用应当聚合成功");
        let calls = v["choices"][0]["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1, "同一个 index 不能变成两次调用");
        assert_eq!(calls[0]["function"]["arguments"], r#"{"city":"上海"}"#);
        // 只有工具调用时 content 按标准给 null（空串会被部分客户端当成空回答）
        assert!(v["choices"][0]["message"]["content"].is_null());
    }

    /// 流式客户端要看到 tool_calls 与 `finish_reason: tool_calls`，否则永远等不到工具调用。
    #[test]
    fn streaming_chunk_carries_tool_calls_and_finish_reason() {
        let completion = json!({
            "id": "chatcmpl-1",
            "model": "glm-5.2",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_x",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"上海\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let body = sse_body(&completion);
        assert!(body.contains("\"tool_calls\""), "{body}");
        assert!(body.contains("\"finish_reason\":\"tool_calls\""), "{body}");
        assert!(body.trim_end().ends_with("data: [DONE]"), "{body}");
    }

    /// Qoder 把工具调用写在正文里（Claude 风格 XML）：要提成标准 tool_calls，
    /// 并从正文里删掉，别让客户端把它当台词念出来。
    #[test]
    fn qoder_text_xml_becomes_real_tool_calls() {
        let text = concat!(
            "我先查一下天气。\n",
            "<function_calls>\n",
            "  <invoke name=\"get_weather\">\n",
            "    <parameter name=\"city\">上海</parameter>\n",
            "    <parameter name=\"days\">3</parameter>\n",
            "  </invoke>\n",
            "  <invoke name=\"get_time\">\n",
            "    <parameter name=\"tz\">\"Asia/Shanghai\"</parameter>\n",
            "  </invoke>\n",
            "</function_calls>\n",
        );
        let (rest, calls) = lift_text_tool_calls(text);
        assert_eq!(rest, "我先查一下天气。");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        assert_eq!(
            calls[0]["function"]["arguments"],
            r#"{"city":"上海","days":3}"#
        );
        assert_eq!(
            calls[1]["function"]["arguments"],
            r#"{"tz":"Asia/Shanghai"}"#
        );
        // 每个调用都要有 id（客户端拿它回 tool 结果）
        assert!(calls[0]["id"].as_str().unwrap().starts_with("call_"));
    }

    /// 没闭合的块不能吞掉：宁可原样显示，也不能删用户内容。
    #[test]
    fn qoder_unclosed_block_is_left_alone() {
        let text = "看这个 <function_calls> 后面没了";
        let (rest, calls) = lift_text_tool_calls(text);
        assert!(calls.is_empty());
        assert_eq!(rest, text);
    }

    /// 普通对话里出现 `<function_calls>` 字样但没有 invoke 时，不许改动返回。
    #[test]
    fn qoder_completion_without_invoke_is_untouched() {
        let v = json!({
            "choices": [{ "message": { "role": "assistant", "content": "普通回答" }, "finish_reason": "stop" }]
        });
        let out = lift_qoder_tool_calls(v.clone(), true);
        assert_eq!(out, v);
    }

    /// 有工具调用但上游只给 `stop` 时，finish_reason 必须是 `tool_calls`。
    #[test]
    fn finish_reason_is_promoted_when_calls_exist() {
        assert_eq!(finish_with_calls(Some("stop".into()), true), "tool_calls");
        assert_eq!(finish_with_calls(None, true), "tool_calls");
        assert_eq!(finish_with_calls(Some("stop".into()), false), "stop");
        assert_eq!(finish_with_calls(Some("length".into()), true), "length");
    }
}
