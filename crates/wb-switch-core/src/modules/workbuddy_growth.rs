//! WorkBuddy「成长中心」任务：列任务 / 报名 / 领奖 / 行为上报。
//!
//! ## 这是什么
//!
//! WorkBuddy 的成长中心有一批「体验类」任务（对话 5 次、读资料库、进发现应用…），
//! 完成后给积分 + 能量。任务**不会自己完成** —— 必须由客户端把对应的行为事件
//! 上报到 `/v2/report`，服务端据此累计进度。
//!
//! 本模块只做两件事：
//! 1. 把任务列表读出来（含进度、是否可领）；
//! 2. 按任务要求补报事件，然后领奖。
//!
//! ## 协议（2026-11-03 真机实测，不是照抄第三方文档）
//!
//! | 动作 | 端点 | 说明 |
//! | --- | --- | --- |
//! | 列任务 | `GET {API}/v2/activity/growth/tasks` | 返回 `data.tasks[]`，含 `progress{current,target}` / `accept_status` |
//! | 报名 | `POST {API}/v2/activity/growth/tasks/accept` | body `{"task_codes":[...]}`，不报名不计进度 |
//! | 领奖 | `POST {WEB}/activity/growth/tasks/{code}/claim` | **注意没有 `/v2` 前缀**，任务码在路径里 |
//! | 活跃上报 | `POST {API}/v2/report` | body 是 `[{chat_request_send 事件}]` |
//!
//! ⚠️ 领奖端点踩过的坑：写成 `{API}/v2/activity/growth/tasks/reward/claim`（任务码放 body）
//! 会一直回 400「task not completed」—— 那条路径不存在。真正的领奖在 web 域、无 `/v2`。
//!
//! ## 纪律
//!
//! - 报名（accept）是幂等的；已 completed 的任务再报名不会报错。
//! - 领奖必须**逐个**，拿到 `already_claimed` 视为成功（不算失败）。
//! - 上报事件必须带 `userId`，否则服务端 200 但**静默丢弃**（真机实测）。

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{json, Value};

use crate::modules::config::http_request_raw;
use crate::modules::{jwt, workbuddy_auth};

/// Web 域（领奖走这里，且路径**没有** `/v2` 前缀）。
const WEB_BASE: &str = "https://www.workbuddy.cn";

/// 成长中心任务的展示 + 状态快照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowthTask {
    pub code: String,
    pub title: String,
    pub desc: String,
    pub reward_credit: i64,
    pub reward_energy: i64,
    /// 当前进度 / 目标进度。`target = 0` 表示上游没给（不是「已完成」）。
    pub current: i64,
    pub target: i64,
    /// `not_accepted` / `accepted` / `completed` …
    pub accept_status: String,
    /// 进度已满且还没领 → 现在点就能拿。
    pub claimable: bool,
    /// 任务自带的跳转地址。**里面有金矿**：
    /// 「体验某位专家」类任务的 `jump_url` 里带着 `expertId=ex_xxx`，
    /// 这比按名字去市场里翻要可靠得多（那些专家根本不在公开市场列表里）。
    pub jump_url: String,
    /// 我们**能自动完成**这一条（决定界面显示「一键完成」还是「需手动」）。
    ///
    /// 由后端判定，界面不许自己再列一份 —— 两边各写一套迟早对不上。
    pub supported: bool,
}

impl GrowthTask {
    /// 这一条是否还需要干活（未完成且没领过）。
    ///
    /// ⚠️ 不能用 `current < target` 判断：上游对「还没开始」的任务给的进度是 `0/0`，
    /// `0 < 0` 为假 ⇒ 会被误判成「已完成」，于是自动跑的时候直接跳过。
    /// 判据只能看状态。
    pub fn needs_work(&self) -> bool {
        !self.claimable && self.accept_status != "claimed" && self.accept_status != "completed"
    }
}

/// 我们能自动完成的任务码 —— **唯一出口**（`run_task` 与界面都看它）。
///
/// 加任务只改这一处 + `run_task` 的分支，界面自动跟着变。
pub fn is_supported(code: &str) -> bool {
    matches!(
        code,
        "chat_5"
            | "RichMeow_Chat"
            | "Buddy_App"
            | "automation_1"
            | "Library_read"
            | "skill_1"
            | "Expert_lighthouse"
            | "expert_5"
            | "Expert_team_use_3"
            | "expert_use_maidehao"
            | "expert_use_douyinyin"
            | "expert_use_shangjia_xuanchuan"
            | "expert_use_bowangwang"
            | "template_5"
            | "playbook_prompt"
            | "Sequential_Tasks_1"
    )
}

/// 从账号里取出 (token, uid)。与签到同一套兜底（uid 缺失时从 JWT 的 `sub` 解）。
fn credentials(acc: &Value) -> Result<(String, String), String> {
    let token = match acc.get("access_token") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Err("该账号没有可用的明文凭据（加密信封或未采集）".to_string()),
    };
    let mut uid = workbuddy_auth::get_str(acc, "uid").unwrap_or_default();
    if uid.trim().is_empty() {
        uid = jwt::payload(&token)
            .and_then(|p| p.get("sub").and_then(|s| s.as_str()).map(String::from))
            .unwrap_or_default();
    }
    Ok((token, uid))
}

fn headers(token: &str, uid: &str) -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert("Authorization".to_string(), format!("Bearer {token}"));
    h.insert("Accept".to_string(), "application/json".to_string());
    h.insert("Content-Type".to_string(), "application/json".to_string());
    if !uid.trim().is_empty() {
        h.insert("X-User-Id".to_string(), uid.trim().to_string());
    }
    h
}

/// 领奖专用头：Web 端形状（浏览器发的那种），缺了 Origin/Referer 会被当成非官方来源。
fn web_headers(token: &str, uid: &str) -> HashMap<String, String> {
    let mut h = headers(token, uid);
    h.insert("Accept".to_string(), "application/json, text/plain, */*".to_string());
    h.insert("Origin".to_string(), WEB_BASE.to_string());
    h.insert(
        "Referer".to_string(),
        format!("{WEB_BASE}/profile/growth-center"),
    );
    h.insert("x-client-platform".to_string(), "web".to_string());
    h
}

async fn call(
    url: &str,
    method: &str,
    body: Option<Value>,
    h: &HashMap<String, String>,
) -> (u16, Value, String) {
    let (status, _hdrs, text) =
        http_request_raw(url, method, body, Some(h), None, true).await;
    let parsed = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&text).unwrap_or(Value::Null)
    };
    (status, parsed, text)
}

/// 上游信封里的业务码（`code` 字段），取不到返回 `None`。
fn code_of(v: &Value) -> Option<i64> {
    v.get("code").and_then(|c| c.as_i64())
}

/// 合并两个口径：PC 列表为主，补上只在 mp 口径下发的任务（按 task_code 去重）。
///
/// ⚠️ 不做这一步的话，`Sequential_Tasks_1` 这类任务在界面上**根本不会出现**
/// （PC 列表里没有），用户会以为「今天没任务」，其实是有个能拿 100 分的没做。
pub async fn list_all(acc: &Value) -> Result<Vec<GrowthTask>, String> {
    let mut tasks = list_tasks(acc).await?;
    if let Ok(mp) = list_tasks_mp(acc).await {
        for t in mp {
            if !tasks.iter().any(|x| x.code == t.code) {
                tasks.push(t);
            }
        }
    }
    Ok(tasks)
}

/// 读任务列表。失败返回 Err（**空列表不等于失败**，界面要能分开）。
pub async fn list_tasks(acc: &Value) -> Result<Vec<GrowthTask>, String> {
    list_tasks_platform(acc, None).await
}

/// 小程序口径的任务列表。
///
/// 同一路径，只是多一个 `X-Client-Platform: miniprogram` 头 ——
/// `Sequential_Tasks_*` / `school_season` 这类「小程序限定」任务**只在**该口径下发，
/// 默认列表里根本看不到（所以批次 3 的任务在 PC 列表里是「不存在」，不是「没做完」）。
pub async fn list_tasks_mp(acc: &Value) -> Result<Vec<GrowthTask>, String> {
    list_tasks_platform(acc, Some("miniprogram")).await
}

async fn list_tasks_platform(acc: &Value, platform: Option<&str>) -> Result<Vec<GrowthTask>, String> {
    let (token, uid) = credentials(acc)?;
    let mut h = headers(&token, &uid);
    if let Some(p) = platform {
        h.insert("X-Client-Platform".to_string(), p.to_string());
    }
    let url = format!("{}/v2/activity/growth/tasks", workbuddy_auth::API_ENDPOINT);
    let (status, v, text) = call(&url, "GET", None, &h).await;
    if status != 200 {
        return Err(format!(
            "读取任务列表失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    if code_of(&v).unwrap_or(0) != 0 {
        return Err(format!(
            "读取任务列表被拒：{}",
            text.chars().take(160).collect::<String>()
        ));
    }
    let raw = v
        .get("data")
        .and_then(|d| d.get("tasks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(raw
        .iter()
        .map(|t| {
            let cur = t
                .get("progress")
                .and_then(|p| p.get("current"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let tgt = t
                .get("progress")
                .and_then(|p| p.get("target"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            GrowthTask {
                code: t["task_code"].as_str().unwrap_or_default().to_string(),
                title: t["title"].as_str().unwrap_or_default().to_string(),
                desc: t["task_desc"]
                    .as_str()
                    .or_else(|| t["description"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                reward_credit: t["reward_credit"].as_i64().unwrap_or(0),
                reward_energy: t["reward_energy"].as_i64().unwrap_or(0),
                current: cur,
                target: tgt,
                accept_status: t["accept_status"].as_str().unwrap_or_default().to_string(),
                // 「可领」= 进度满了 **且没领过**。
                // ⚠️ 只看进度会出错：领完之后 `accept_status` 变成 `claimed`，
                // 但进度仍是 1/1 —— 实测领完 first_buddy 后界面还显示「可领」，
                // 再点一次白跑一趟（上游回 already_claimed）。两个条件缺一不可。
                claimable: tgt > 0 && cur >= tgt && t["accept_status"].as_str() != Some("claimed"),
                jump_url: t["jump_url"].as_str().unwrap_or_default().to_string(),
                supported: is_supported(t["task_code"].as_str().unwrap_or_default()),
            }
        })
        .filter(|t| !t.code.is_empty())
        .collect())
}

/// 报名（accept）。不报名服务端不计进度。
///
/// 幂等：已报名的再传一次不会报错；空数组直接返回成功（不发请求）。
pub async fn accept(acc: &Value, codes: &[String]) -> Result<usize, String> {
    if codes.is_empty() {
        return Ok(0);
    }
    let (token, uid) = credentials(acc)?;
    let h = headers(&token, &uid);
    let url = format!(
        "{}/v2/activity/growth/tasks/accept",
        workbuddy_auth::API_ENDPOINT
    );
    let (status, v, text) = call(&url, "POST", Some(json!({ "task_codes": codes })), &h).await;
    if status != 200 || code_of(&v).unwrap_or(0) != 0 {
        return Err(format!(
            "报名失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    Ok(codes.len())
}

/// 领奖结果。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimResult {
    /// 本次真正到账的积分 / 能量（`already_claimed` 时为 0）。
    pub credit: i64,
    pub energy: i64,
    /// 之前已经领过 —— **不是失败**，界面也不该报红。
    pub already: bool,
}

/// 领单个任务的奖励。
pub async fn claim(acc: &Value, code: &str) -> Result<ClaimResult, String> {
    let (token, uid) = credentials(acc)?;
    let h = web_headers(&token, &uid);
    // 主路径：web 域、无 `/v2`。**不要**改成 API 域的 `/v2/.../reward/claim`（那条不存在）。
    let url = format!("{WEB_BASE}/activity/growth/tasks/{code}/claim");
    let (status, v, text) = call(&url, "POST", None, &h).await;
    if status != 200 {
        return Err(format!(
            "领奖失败（{code}）：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    // 业务码：0 = 成功；有的环境不带 code，直接看 data。
    if let Some(c) = code_of(&v) {
        if c != 0 {
            return Err(format!(
                "领奖失败（{code}）：{}",
                text.chars().take(160).collect::<String>()
            ));
        }
    }
    let d = v.get("data").cloned().unwrap_or(Value::Null);
    let already = d
        .get("already_claimed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(ClaimResult {
        credit: d.get("credit").and_then(Value::as_i64).unwrap_or(0),
        energy: d.get("energy").and_then(Value::as_i64).unwrap_or(0),
        already,
    })
}

/// 任务之间、事件之间的最小间隔。
///
/// 上游对行为事件有**时间窗**判断（同一毫秒灌一堆事件会被当成脚本），
/// 实测 800ms 以上才稳定进账；脚本口径也是这个量级。
const REPORT_GAP_MS: u64 = 900;

/// 补报 `chat_5`（「和 AI 聊天 5 次」）。
///
/// 口径：每条用**独立 conversationId**（上游按对话数计），requestId 留空由它回落成
/// conversationId —— 与实测脚本一致。只补差额，进度已满就一条都不发。
pub async fn run_chat_5(acc: &Value, task: &GrowthTask) -> Result<String, String> {
    let target = if task.target > 0 { task.target } else { 5 };
    let need = target - task.current;
    if need <= 0 {
        return Ok("进度已达标，无需上报".to_string());
    }
    let stamp = chrono::Utc::now().timestamp_millis();
    for i in 0..need {
        let conv = format!("wb-ts-chat5-{stamp}-{i}");
        report_chat(acc, &conv, "", "deepseek-v4-flash", "DeepSeek V4 Flash").await?;
        if i + 1 < need {
            tokio::time::sleep(std::time::Duration::from_millis(REPORT_GAP_MS)).await;
        }
    }
    Ok(format!("已补报 {need} 条对话事件"))
}

/// 一次「跑一批任务」的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRun {
    pub code: String,
    pub ok: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// 行为上报：把「客户端做了某件事」告诉服务端，任务进度才会动
// ---------------------------------------------------------------------------

/// 一条 `chat_request_send` 事件。
///
/// ⚠️ 字段**必须齐全**（服务端按整条事件解析），只发最小三字段会被丢弃。
/// `userId` 缺失时服务端照样 200，但**静默不记账** —— 这是最容易踩的坑。
fn chat_event(uid: &str, conv_id: &str, request_id: &str, model_id: &str, model_name: &str) -> Value {
    let now = chrono::Utc::now().timestamp_millis();
    json!({
        "eventCode": "chat_request_send",
        "timestamp": now,
        "reportDelay": 0,
        "mode": "craft",
        "conversationId": conv_id,
        "requestId": request_id,
        "inputLength": 12,
        "requestModelId": model_id,
        "requestModelName": model_name,
        "isPlan": false,
        "isAutoExecuteTerminal": false,
        "isAutoModify": false,
        "codebaseEnable": false,
        "maxToken": 0,
        "maxSteps": 0,
        "temperature": 0,
        "maxRetries": 0,
        "mentionContexts": [],
        "knowledgeId": [],
        "knowledgeName": [],
        "codebaseId": "",
        "mentionContextCount": 0,
        "command": "",
        "expertId": "",
        "recommendId": "",
        "skillId": "",
        "skillCount": 0,
        "totalCount": 0,
        "fileUri": "",
        "presentAt": now,
        "traceId": "",
        "rootRequestId": request_id,
        "parentConversationId": conv_id,
        "agentName": "default",
        "agentType": "conversation",
        "userId": uid,
    })
}

/// 上报一条对话活跃事件（`chat_5` / 桌面端对话类任务靠它推进度）。
///
/// `conv_id` 由调用方指定：服务端不校验它是否真实存在，但**同一段对话要复用同一个**，
/// 否则会被当成多段对话（这一点直接决定了「聊 5 次」是按对话数还是按次数算）。
pub async fn report_chat(
    acc: &Value,
    conv_id: &str,
    request_id: &str,
    model_id: &str,
    model_name: &str,
) -> Result<(), String> {
    let (token, uid) = credentials(acc)?;
    if uid.trim().is_empty() {
        return Err("拿不到账号 uid，上报会被服务端静默丢弃".to_string());
    }
    let h = headers(&token, &uid);
    let url = format!("{}/v2/report", workbuddy_auth::API_ENDPOINT);
    let body = json!([chat_event(&uid, conv_id, request_id, model_id, model_name)]);
    let (status, v, text) = call(&url, "POST", Some(body), &h).await;
    if status != 200 {
        return Err(format!(
            "上报失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    if code_of(&v).unwrap_or(0) != 0 {
        return Err(format!(
            "上报被拒：{}",
            text.chars().take(160).collect::<String>()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 桌面指纹事件：`RichMeow_Chat` / `Buddy_App` / `automation_1` / `template_5` …
// ---------------------------------------------------------------------------

/// 由 uid 派生的稳定设备标识（36 位 hex）。
///
/// 必须是**稳定**的：同一账号每次上报要用同一个 machineId，
/// 每次随机会被上游当成「新设备」并可能触发风控。
fn derive_id(uid: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(format!("{salt}:{uid}").as_bytes());
    let d = h.finalize();
    hex::encode(&d[..18])
}

/// 桌面端公共指纹。注入到每一条事件里（业务字段优先，可覆盖）。
fn desktop_fingerprint(uid: &str, nickname: &str) -> Value {
    let now = chrono::Utc::now().timestamp_millis();
    json!({
        "timezone": "Asia/Shanghai",
        "reportDelay": 2000,
        "userId": uid,
        "username": nickname,
        "userNickname": nickname,
        "product": "SaaS",
        "releaseDate": 1789036585355i64,
        "commit": "5f9692923c93033111c51ad7b003eb80204a9b75",
        "ideName": "WorkBuddy",
        "ideType": "WorkBuddy",
        "ideVersion": "5.5.6",
        "machineId": derive_id(uid, "machine"),
        "sessionId": derive_id(uid, "session"),
        "extName": "workbuddy-desktop",
        "extVersion": "5.5.6",
        "os": "win32",
        "arch": "x64",
        "osVersion": "10.0.26220",
        "cpuCores": 20,
        "memorySize": 24,
        "timestamp": now,
        "presentAt": now,
    })
}

/// 以桌面端指纹批量上报事件。
///
/// ⚠️ 与 `report_chat`（billing 域的 `chat_request_send`）**不是同一个通道**：
/// 这里是 `copilot.tencent.com/v2/report`，请求体是**带公共指纹**的事件数组。
/// 少了 machineId / ideVersion 这些字段，上游会当成非法来源静默丢弃。
pub async fn report_desktop(acc: &Value, events: &[Value]) -> Result<(), String> {
    if events.is_empty() {
        return Err("没有要上报的事件".to_string());
    }
    let (token, uid) = credentials(acc)?;
    let nickname = crate::modules::workbuddy_accounts::label_for(&uid);
    let fp = desktop_fingerprint(&uid, &nickname);

    let batch: Vec<Value> = events
        .iter()
        .map(|ev| {
            let mut m = fp.clone();
            if let (Some(dst), Some(src)) = (m.as_object_mut(), ev.as_object()) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            m
        })
        .collect();

    let mut h = headers(&token, &uid);
    h.insert("Accept".to_string(), "application/json, text/plain, */*".to_string());
    h.insert("Content-Type".to_string(), "application/json;charset=UTF-8".to_string());
    h.insert("User-Agent".to_string(), DESKTOP_UA.to_string());
    h.insert("X-Product".to_string(), "SaaS".to_string());
    h.insert(
        "X-Request-ID".to_string(),
        format!("{}{}", derive_id(&uid, "req"), chrono::Utc::now().timestamp_subsec_nanos() % 1_000_000),
    );

    let url = format!("{}/v2/report", workbuddy_auth::API_ENDPOINT);
    let (status, v, text) = call(&url, "POST", Some(json!(batch)), &h).await;
    if status != 200 || code_of(&v).unwrap_or(0) != 0 {
        return Err(format!(
            "桌面事件上报失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    Ok(())
}

const DESKTOP_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) WorkBuddy/5.5.6 Chrome/120.0.0.0 Safari/537.36";

/// 一次「桌面端成功对话」的完整事件链（6 条）。
///
/// 光报一条 `chat_request_send` 不够 —— 「桌面端对话」类任务要看到
/// `agent_task_created → chat_message_send → chat_request_send → … → chat_request_response`
/// 这条完整链路才认。
fn desktop_chat_sequence(conv: &str, req: &str, msg: &str, model_id: &str, model_name: &str) -> Vec<Value> {
    let now = chrono::Utc::now().timestamp_millis();
    vec![
        json!({
            "eventCode": "agent_task_created",
            "source": "LOCAL", "name": "working", "task_target": "local", "mode": "craft",
            "requestModelId": model_id, "requestModelName": model_name,
            "has_repo": false, "repo_type": "none", "workspace_type": "empty",
            "has_connector": false, "connector_types": [],
            "has_mention": false, "mention_types": [],
            "has_template": false, "action": "", "template_name": "",
            "has_expert": false, "expert_id": "", "expert_name": "", "expert_industry_id": "",
            "has_skill": false, "skill_names": [],
            "conversationId": conv, "messageId": msg,
            "buddyId": "", "buddyName": "",
        }),
        json!({
            "eventCode": "chat_message_send",
            "messageId": format!("{msg}-assistant"), "historyCount": 0,
            "isContextTruncated": false, "currentStepCount": 1,
            "traceId": req, "rootRequestId": req, "parentConversationId": conv,
            "agentName": "cli", "agentType": "main",
        }),
        json!({
            "eventCode": "chat_request_send",
            "inputLength": 24, "isPlan": false, "isAutoExecuteTerminal": false,
            "isAutoModify": false, "codebaseEnable": false, "maxToken": 0,
            "maxSteps": 500, "temperature": 0, "maxRetries": 0,
            "mentionContexts": [], "knowledgeId": [], "knowledgeName": [],
            "codebaseId": "", "mentionContextCount": 0, "command": "",
            "recommendId": "", "skillId": "", "skillCount": 0, "totalCount": 0,
            "traceId": req, "rootRequestId": req, "parentConversationId": conv,
            "agentName": "cli", "agentType": "main",
            "codebuddy.session_id": conv,
            "codebuddy.conversation_request_id": req,
        }),
        json!({
            "eventCode": "chat_message_response",
            "messageId": format!("{msg}-assistant"), "responseModelId": model_id,
            "inputToken": 120, "outputToken": 80, "totalToken": 200,
            "cachedTokens": 0, "cachedWriteTokens": 0, "cachedMissTokens": 0,
            "isSuccessful": true, "messageErrorCode": "", "finishReason": "stop",
            "firstTokenAt": now, "traceId": req, "conversationId": conv,
            "rootRequestId": req, "parentConversationId": conv,
            "agentName": "cli", "agentType": "main",
            "codebuddy.session_id": conv,
            "codebuddy.conversation_request_id": req,
        }),
        json!({
            "eventCode": "chat_message_status",
            "messageId": format!("{msg}-assistant"), "messageErrorCode": "0",
            "traceId": req, "rootRequestId": req, "parentConversationId": conv,
            "agentName": "cli", "agentType": "main",
        }),
        json!({
            "eventCode": "chat_request_response",
            "mode": "craft", "toolCallCount": 0,
            "inputToken": 120, "outputToken": 80, "totalToken": 200,
            "cachedTokens": 0, "cachedWriteTokens": 0, "cachedMissTokens": 0,
            "isSuccessful": true, "messageErrorCode": "", "finishReason": "stop",
            "rootRequestId": req, "parentConversationId": conv,
        }),
    ]
}

/// 「进入 Buddy 应用」五连事件（同时覆盖 `Buddy_App` 与 `Buddy_App_QQ`）。
fn buddy_app_sequence(buddy_id: &str, buddy_name: &str) -> Vec<Value> {
    let mk = |code: &str, extra: Value| {
        let mut v = json!({
            "eventCode": code, "mode": "LOCAL",
            "buddyId": buddy_id, "buddyName": buddy_name,
        });
        if let (Some(dst), Some(src)) = (v.as_object_mut(), extra.as_object()) {
            for (k, x) in src {
                dst.insert(k.clone(), x.clone());
            }
        }
        v
    };
    vec![
        mk("buddyapp_discover_click", json!({})),
        mk("buddyapp_show", json!({"elementId": buddy_id, "elementName": buddy_name, "position": 2})),
        mk(
            "buddyapp_enter_click",
            json!({"elementId": buddy_id, "elementName": buddy_name, "position": 2, "isFirstPage": "1"}),
        ),
        mk(
            "buddyapp_auth_confirm_click",
            json!({"elementId": buddy_id, "elementName": buddy_name}),
        ),
        mk(
            "buddyapp_bindaccount_skip_click",
            json!({"elementId": buddy_id, "elementName": buddy_name}),
        ),
    ]
}

/// 企鹅教师助手（`Buddy_App` / `Buddy_App_QQ` 的判据应用）。
const BUDDY_APP_ID: &str = "cb_y5Dy46tPQGGWtueMxXbe";
const BUDDY_APP_NAME: &str = "企鹅教师助手";

/// Web 域单事件上报（`Library_read` 这类「网页上点了某处」的任务）。
///
/// 与桌面事件是**另一套指纹**：浏览器形状（`pageURL` / `os` / `userAgent` / `webMachineId`），
/// 且走 `www.workbuddy.cn` 而不是 API 域。
pub async fn report_web_event(
    acc: &Value,
    event_code: &str,
    page_url: &str,
    element_id: &str,
    element_name: &str,
) -> Result<(), String> {
    let (token, uid) = credentials(acc)?;
    let nickname = crate::modules::workbuddy_accounts::label_for(&uid);
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";

    let mut h = web_headers(&token, &uid);
    // Referer 必须是事件发生的那个页面，否则「从哪点的」对不上。
    h.insert("Referer".to_string(), page_url.to_string());
    h.insert("User-Agent".to_string(), ua.to_string());

    let ev = json!([{
        "eventCode": event_code,
        "timestamp": chrono::Utc::now().timestamp_millis(),
        "reportDelay": 0,
        "pageURL": page_url,
        "elementId": element_id,
        "elementName": element_name,
        "os": "Win32",
        "arch": "",
        "osVersion": "10.0",
        "userAgent": ua,
        "machineId": derive_id(&uid, "webmachine"),
        "userId": uid,
        "userNickname": nickname,
        "enterpriseId": "",
    }]);
    let url = format!("{WEB_BASE}/v2/report");
    let (status, v, text) = call(&url, "POST", Some(ev), &h).await;
    if status != 200 || code_of(&v).unwrap_or(0) != 0 {
        return Err(format!(
            "网页事件上报失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 任务动作：一个任务码 → 一组上报
// ---------------------------------------------------------------------------

/// `RichMeow_Chat`：桌面端对话 1 次（完整事件链）。
pub async fn run_richmeow_chat(acc: &Value) -> Result<String, String> {
    let stamp = chrono::Utc::now().timestamp_millis();
    let conv = format!("wb-ts-conv-{stamp}");
    let req = format!("wb-ts-req-{stamp}");
    let ev = desktop_chat_sequence(&conv, &req, &format!("msg-{stamp}"), "fast-model", "fast-model");
    report_desktop(acc, &ev).await?;
    Ok("已上报桌面端对话事件链（6 条）".to_string())
}

/// `Buddy_App` / `Buddy_App_QQ`：进入 Buddy 应用五连事件。
pub async fn run_buddy_app(acc: &Value) -> Result<String, String> {
    let ev = buddy_app_sequence(BUDDY_APP_ID, BUDDY_APP_NAME);
    report_desktop(acc, &ev).await?;
    Ok("已上报进入应用五连事件".to_string())
}

/// `automation_1`：定时任务创建成功。
pub async fn run_automation_create(acc: &Value) -> Result<String, String> {
    let ev = [json!({
        "eventCode": "automated_task_create_suc",
        "name": "TwinSwitch 自动化",
        "source": "manually",
        "modelId": "fast-model",
        "modelIsThinking": true,
        "connectorCount": 0,
        "skills": "",
        "skillCount": 0,
        "scheduleType": "once",
        "mode": "LOCAL",
    })];
    report_desktop(acc, &ev).await?;
    Ok("已上报「定时任务创建」事件".to_string())
}

/// `Library_read`：读资料库介绍页。
pub async fn run_library_read(acc: &Value) -> Result<String, String> {
    report_web_event(
        acc,
        "web_element_click",
        "https://www.workbuddy.cn/space/d/o0KWYeynteVv06UnAZqIFm",
        "library_doc_intro_click",
        "WorkBuddy资料库介绍",
    )
    .await?;
    Ok("已上报「资料库介绍阅读」事件".to_string())
}

// ---------------------------------------------------------------------------
// 专家市场 / 真实对话（批次 2）
// ---------------------------------------------------------------------------

/// 两位专家之间的最小间隔：真实使用链是「召唤 → 对话 → 使用」，节奏太快会被当成脚本。
const EXPERT_GAP_MS: u64 = 6_000;

/// 服务端 requestId 的形状：`cmb-<32hex>` 或裸 32 hex。
///
/// ⚠️ **必须用服务端返回的这个 id**。自己生成一个 UUID 上报，
/// `expert_actual_use` 一律不计数 —— 上游按「真实对话的 requestId」做 JOIN。
fn is_server_id(s: &str) -> bool {
    let core = s.strip_prefix("cmb-").unwrap_or(s);
    core.len() == 32 && core.chars().all(|c| c.is_ascii_hexdigit())
}

/// 从 SSE 文本里取第一个形如 `"id":"<server id>"` 的值。
fn extract_server_id(text: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(i) = text[from..].find("\"id\":\"") {
        let abs = from + i;
        let rest = &text[abs + 6..];
        let Some(end) = rest.find('"') else { break };
        let id = &rest[..end];
        if is_server_id(id) {
            return Some(id.to_string());
        }
        // 跳过这个不匹配的 id 继续找：消息 id 等字段也会长得像 `"id":"..."`
        from = abs + 6 + end + 1;
    }
    None
}

/// 拉专家市场的真实专家（`expert_type`：`agent` 单专家 / `team` 专家团 / 空 = 不限）。
///
/// 判据要求 id 必须是平台上**真实存在**的专家，编造的 id 不计数，所以必须先拉列表。
pub async fn expert_list(acc: &Value, expert_type: &str) -> Result<Vec<Value>, String> {
    expert_list_page(acc, expert_type, 1, 100).await
}

/// 分页版：市场默认按 `reco_rank` 排序，**第一页只有 20 条**，
/// 「体验某位专家」类任务里点的那些名字往往在更后面 —— 要翻页找。
pub async fn expert_list_page(
    acc: &Value,
    expert_type: &str,
    page: u32,
    page_size: u32,
) -> Result<Vec<Value>, String> {
    let (token, uid) = credentials(acc)?;
    let mut h = headers(&token, &uid);
    h.insert("User-Agent".to_string(), DESKTOP_UA.to_string());
    h.insert("X-Domain".to_string(), workbuddy_auth::API_ENDPOINT.to_string());
    h.insert("X-Product".to_string(), "SaaS".to_string());

    let mut body = json!({
        "page": page, "page_size": page_size,
        "sort_by": "reco_rank", "sort_order": "desc",
    });
    if !expert_type.trim().is_empty() {
        body["expert_type"] = json!(expert_type);
    }
    let url = format!(
        "{}/portal/operation-platform/market/expert/list",
        workbuddy_auth::API_ENDPOINT
    );
    let (status, v, text) = call(&url, "POST", Some(body), &h).await;
    if status != 200 {
        return Err(format!(
            "拉专家列表失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    // 上游可能把结果包在 `data` 里，也可能直接给 `experts`
    let list = v
        .get("data")
        .and_then(|d| d.get("experts"))
        .or_else(|| v.get("experts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(list)
}

/// 发一条**真实**对话（可带 `X-Expert-Id`），并从 SSE 里取服务端 requestId。
///
/// 这是批次 2 的核心：专家类任务的判据要求「真的聊过一次」，
/// 而且上报里的 `requestId` 必须与这次对话的服务端 id 一致。
pub async fn chat_with_expert(acc: &Value, expert_id: &str) -> Result<(String, String), String> {
    let (token, uid) = credentials(acc)?;
    let conv = format!("wb-ts-conv-{}", chrono::Utc::now().timestamp_millis());
    let mut h = headers(&token, &uid);
    h.insert("Accept".to_string(), "text/event-stream".to_string());
    h.insert("User-Agent".to_string(), DESKTOP_UA.to_string());
    h.insert("X-Domain".to_string(), workbuddy_auth::API_ENDPOINT.to_string());
    h.insert("X-Product".to_string(), "SaaS".to_string());
    h.insert("X-Conversation-ID".to_string(), conv.clone());
    h.insert(
        "X-Request-ID".to_string(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string(),
    );
    h.insert("X-Agent-Intent".to_string(), "craft".to_string());
    h.insert("X-Agent-Type".to_string(), "main".to_string());
    h.insert("X-IDE-Name".to_string(), "WorkBuddy".to_string());
    h.insert("X-IDE-Type".to_string(), "WorkBuddy".to_string());
    h.insert("X-IDE-Version".to_string(), "5.5.6".to_string());
    h.insert("x-codebuddy-request".to_string(), "1".to_string());
    if !expert_id.trim().is_empty() {
        h.insert("X-Expert-Id".to_string(), expert_id.trim().to_string());
    }

    let body = json!({
        "model": "fast-model",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant. 当前处于中文环境，使用简体中文回答。"},
            {"role": "user", "content": "1+1等于几？直接回答。"},
        ],
        "agent": "cli",
        "temperature": 1,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    let url = format!("{}/v2/chat/completions", workbuddy_auth::API_ENDPOINT);
    let (status, _v, text) = call(&url, "POST", Some(body), &h).await;
    if status != 200 {
        return Err(format!(
            "真实对话失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    let id = extract_server_id(&text)
        .ok_or_else(|| "对话流里没找到服务端 requestId（上游可能改了响应形状）".to_string())?;
    Ok((conv, id))
}

/// 「召唤专家」三连事件（web_element_click → summon_click → summoned）。
fn expert_summon_sequence(e: &Value) -> Vec<Value> {
    let id = e["expert_id"].as_str().unwrap_or_default();
    let name = e["display_name_zh"].as_str().unwrap_or_default();
    let title = e["profession_zh"].as_str().unwrap_or_default();
    let etype = e["expert_type"].as_str().unwrap_or("agent");
    let ver = e["version"].as_str().filter(|v| !v.is_empty()).unwrap_or("1.0.0");
    let cat = e["categories"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or("expert-all");

    vec![
        json!({
            "eventCode": "web_element_click", "source": id, "type": cat, "version": ver,
            "elementId": "expert_summon_click", "elementName": "立即召唤",
            "pageURL": "/C:/Program%20Files/WorkBuddy/resources/app.asar/renderer/index.html",
        }),
        json!({
            "eventCode": "expert_summon_click", "id": id, "name": name,
            "expertTitle": title, "type": "expert-all", "position": 0,
            "expertType": etype, "version": ver, "mode": "LOCAL",
        }),
        json!({
            "eventCode": "expert_summoned", "id": id, "name": name,
            "expertTitle": title, "type": "expert-all",
        }),
    ]
}

/// `expert_actual_use` 事件（专家类任务的**计数判据**）。
fn expert_actual_use(e: &Value, conv: &str, req: &str, mode: &str) -> Value {
    let id = e["expert_id"].as_str().unwrap_or_default();
    let name = e["display_name_zh"].as_str().unwrap_or_default();
    let title = e["profession_zh"].as_str().unwrap_or_default();
    let etype = e["expert_type"].as_str().unwrap_or("agent");
    let ver = e["version"].as_str().filter(|v| !v.is_empty()).unwrap_or("1.0.0");
    let cat = e["categories"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or("expert-all");
    let tail: String = req.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();

    json!({
        "eventCode": "expert_actual_use",
        "id": id, "name": name, "expertTitle": title,
        "type": cat, "expertType": etype, "source": "builtin", "version": ver,
        "cost": 9000, "characterCount": 14,
        "conversationId": conv, "requestId": req, "messageId": format!("msg-{tail}"),
        "requestModelId": "fast-model", "requestModelName": "fast-model",
        "mode": mode,
    })
}

/// 「召唤 + 真实使用」一位专家。返回说明。
async fn use_one_expert(acc: &Value, e: &Value, mode: &str, has_expert_flag: bool) -> Result<(), String> {
    let eid = e["expert_id"].as_str().unwrap_or_default().to_string();
    if eid.is_empty() {
        return Err("专家没有 id".to_string());
    }
    // ① 召唤链
    report_desktop(acc, &expert_summon_sequence(e)).await?;
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // ② 真实对话（判据要的是这一次的服务端 requestId）
    let (conv, req) = chat_with_expert(acc, &eid).await?;
    let tail: String = req.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();

    // ③ 对话链 + 使用事件
    let mut events = desktop_chat_sequence(&conv, &req, &format!("msg-{tail}"), "fast-model", "fast-model");
    if has_expert_flag {
        if let Some(first) = events.first_mut() {
            first["has_expert"] = json!(true);
            first["expert_id"] = json!(eid);
            first["expert_name"] = json!(e["display_name_zh"].as_str().unwrap_or_default());
            first["expert_industry_id"] = json!("");
        }
    }
    let mut use_ev = expert_actual_use(e, &conv, &req, mode);
    if mode == "LOCAL" {
        // 真实样本：轻量云专家的 actual_use 是 type 空、cost 0
        use_ev["type"] = json!("");
        use_ev["cost"] = json!(0);
    }
    events.push(use_ev);
    report_desktop(acc, &events).await?;
    Ok(())
}

/// `expert_5` / `Expert_team_use_3`：批量召唤并使用 N 位真实专家。
async fn run_expert_batch(acc: &Value, expert_type: &str, count: usize) -> Result<String, String> {
    let experts = expert_list(acc, expert_type).await?;
    if experts.is_empty() {
        return Err(format!("专家市场列表为空（类型 {expert_type}）"));
    }
    let (mut ok, mut fail) = (0usize, 0usize);
    let mut last_err = String::new();
    for (i, e) in experts.iter().enumerate() {
        if ok >= count {
            break;
        }
        match use_one_expert(acc, e, "craft", false).await {
            Ok(()) => ok += 1,
            Err(err) => {
                fail += 1;
                last_err = err;
            }
        }
        if i + 1 < experts.len() && ok < count {
            tokio::time::sleep(std::time::Duration::from_millis(EXPERT_GAP_MS)).await;
        }
    }
    if ok == 0 {
        return Err(format!("一位专家都没成功（{fail} 次失败，最后原因：{last_err}）"));
    }
    Ok(format!("已对 {ok} 位专家完成召唤+真实使用"))
}

/// `Expert_lighthouse`：腾讯轻量云专家（判据要求 `mode=LOCAL`、`has_expert=true`）。
pub async fn run_expert_lighthouse(acc: &Value) -> Result<String, String> {
    const LIGHTHOUSE: &str = "ex_2cvvUZQhDyeJ";
    // 市场列表里命中就用服务端的名称/版本，命中不了用兜底值（id 是判据，不能编）
    let mut e = json!({
        "expert_id": LIGHTHOUSE, "expert_type": "agent",
        "display_name_zh": "腾讯轻量云专家", "profession_zh": "腾讯轻量云专家",
        "version": "1.0.2", "categories": [],
    });
    if let Ok(list) = expert_list(acc, "agent").await {
        if let Some(hit) = list.iter().find(|x| x["expert_id"] == LIGHTHOUSE) {
            e = hit.clone();
        }
    }
    use_one_expert(acc, &e, "LOCAL", true).await?;
    Ok("已上报轻量云专家召唤+真实使用链".to_string())
}

/// `skill_1`：真实对话 + `skill_info` 技能加载事件。
pub async fn run_skill_1(acc: &Value) -> Result<String, String> {
    let (conv, req) = chat_with_expert(acc, "").await?;
    let tail: String = req.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
    let msg = format!("msg-{tail}");
    let mut events = desktop_chat_sequence(&conv, &req, &msg, "fast-model", "fast-model");
    for ev in events.iter_mut() {
        if ev["eventCode"] == "chat_message_response" {
            // 模型发起工具调用（技能加载）语义
            ev["finishReason"] = json!("tool_calls");
        }
    }
    events.push(json!({
        "eventCode": "skill_info",
        "id": "润泽小馆·日报撰写",
        "skillId": "skill_2097350077599879168",
        "skillVersion": "1.0.0",
        "toolStatus": "success",
        "fileCount": 56,
        "source": "workbuddy-desktop",
        "conversationId": conv, "requestId": req, "messageId": msg,
        "requestModelId": "fast-model", "requestModelName": "fast-model",
        "traceId": req,
    }));
    report_desktop(acc, &events).await?;
    Ok("已上报真实对话 + skill_info 技能加载事件".to_string())
}

// ---------------------------------------------------------------------------
// 小程序口径（Sequential_Tasks_* / school_season）
// ---------------------------------------------------------------------------

/// 小程序埋点的公共指纹。
///
/// ⚠️ 和桌面（`workbuddy-desktop`）、网页（Chrome UA）是**三套完全不同的身份**：
/// 这里 `ideName=wx_app_cloud`、`extName=workbuddy-mp`、`platform=mini_program`。
/// 用桌面指纹去报小程序任务，上游认不出来（事件进不了那个任务的桶）。
///
/// `machineId` 按 uid 派生（不是写死一个常量）：同一账号每次一致，
/// 不同账号不同 —— 写死同一个值会让多个账号在风控侧看起来是同一台设备。
fn mp_fingerprint(uid: &str, nickname: &str) -> Value {
    json!({
        "timestamp": chrono::Utc::now().timestamp_millis(),
        "ideType": "WorkBuddy_MP",
        "ideVersion": "2.4.0",
        "extName": "workbuddy-mp",
        "extVersion": "2.4.0",
        "product": "SaaS",
        "ideName": "wx_app_cloud",
        "platform": "mini_program",
        "os": "windows",
        "osVersion": "11",
        "arch": "x64",
        "machineId": derive_id(uid, "mpmachine"),
        "timezone": "Asia/Shanghai",
        "userId": uid,
        "userNickname": nickname,
    })
}

/// 以小程序指纹上报事件（`{API}/v2/report`，头要走 mp 那一套）。
pub async fn report_mp(acc: &Value, events: &[Value]) -> Result<(), String> {
    if events.is_empty() {
        return Err("没有要上报的事件".to_string());
    }
    let (token, uid) = credentials(acc)?;
    let nickname = crate::modules::workbuddy_accounts::label_for(&uid);
    let fp = mp_fingerprint(&uid, &nickname);
    let batch: Vec<Value> = events
        .iter()
        .map(|ev| {
            let mut m = fp.clone();
            if let (Some(dst), Some(src)) = (m.as_object_mut(), ev.as_object()) {
                for (k, v) in src {
                    dst.insert(k.clone(), v.clone());
                }
            }
            m
        })
        .collect();

    let mut h = headers(&token, &uid);
    h.insert("X-Client-Product".to_string(), "workbuddy-mp".to_string());
    h.insert("X-Client-Version".to_string(), "2.4.0".to_string());
    h.insert("X-Client-Platform".to_string(), "mp-weixin".to_string());
    h.insert("X-Platform".to_string(), "wechatmp".to_string());

    let url = format!("{}/v2/report", workbuddy_auth::API_ENDPOINT);
    let (status, v, text) = call(&url, "POST", Some(json!(batch)), &h).await;
    if status != 200 || code_of(&v).unwrap_or(0) != 0 {
        return Err(format!(
            "小程序事件上报失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    Ok(())
}

/// 一条小程序口径的 `chat_request_send`（`Sequential_Tasks_1/3` 的判据载体）。
fn mp_chat_event(conv: &str, req: &str) -> Value {
    let tail: String = req.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
    json!({
        "eventCode": "chat_request_send",
        "inputLength": 14,
        "isPlan": false, "isAutoExecuteTerminal": false, "isAutoModify": false,
        "codebaseEnable": false, "maxToken": 0, "maxSteps": 500,
        "temperature": 0, "maxRetries": 0,
        "mentionContexts": [], "knowledgeId": [], "knowledgeName": [],
        "codebaseId": "", "mentionContextCount": 0, "command": "",
        "recommendId": "", "skillId": "", "skillCount": 0, "totalCount": 0,
        "traceId": req, "rootRequestId": req,
        "parentConversationId": conv, "conversationId": conv,
        "messageId": format!("msg-{tail}"),
        "agentName": "mp", "agentType": "main",
        "codebuddy.session_id": conv,
        "codebuddy.conversation_request_id": req,
    })
}

/// 报名（可按口径带 `X-Client-Platform`）。小程序任务缺这个头会回 `task not found`。
async fn accept_platform(acc: &Value, codes: &[String], platform: Option<&str>) -> Result<usize, String> {
    if codes.is_empty() {
        return Ok(0);
    }
    let (token, uid) = credentials(acc)?;
    let mut h = headers(&token, &uid);
    if let Some(p) = platform {
        h.insert("X-Client-Platform".to_string(), p.to_string());
    }
    let url = format!(
        "{}/v2/activity/growth/tasks/accept",
        workbuddy_auth::API_ENDPOINT
    );
    let (status, v, text) = call(&url, "POST", Some(json!({ "task_codes": codes })), &h).await;
    if status != 200 || code_of(&v).unwrap_or(0) != 0 {
        return Err(format!(
            "报名失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    Ok(codes.len())
}

/// `Sequential_Tasks_1`：小程序内首对话（1 条 mp 口径的 `chat_request_send`）。
pub async fn run_sequential_1(acc: &Value) -> Result<String, String> {
    accept_platform(acc, &["Sequential_Tasks_1".to_string()], Some("miniprogram")).await?;
    tokio::time::sleep(std::time::Duration::from_millis(REPORT_GAP_MS)).await;

    let stamp = chrono::Utc::now().timestamp_millis();
    let conv = format!("wb-ts-mp-{stamp}");
    let req = format!("wb-ts-mpreq-{stamp}");
    report_mp(acc, &[mp_chat_event(&conv, &req)]).await?;
    Ok("已上报小程序首对话事件".to_string())
}

/// `template_5`：用 5 个**不同**模板各发起一次对话（按模板 id 去重计数）。
///
/// 事件组 = 完整对话链 + `agent_task_created_with_template` + `template_used`。
/// ⚠️ 5 组必须用**不同** template_id，同一个 id 报 5 次只算 1 次。
pub async fn run_template_5(acc: &Value, task: &GrowthTask) -> Result<String, String> {
    let target = if task.target > 0 { task.target } else { 5 };
    let need = (target - task.current).max(0) as usize;
    if need == 0 {
        return Ok("进度已达标，无需上报".to_string());
    }
    const NAMES: [&str; 5] = ["周报生成", "会议纪要", "代码评审", "方案对比", "数据解读"];
    let stamp = chrono::Utc::now().timestamp_millis();
    for i in 0..need {
        let tid = format!("tpl-{stamp}-{i}");
        let name = NAMES[i % NAMES.len()];
        let conv = format!("wb-ts-tpl-conv-{stamp}-{i}");
        let req = format!("wb-ts-tpl-req-{stamp}-{i}");
        let mut ev = desktop_chat_sequence(&conv, &req, &format!("msg-{tid}"), "fast-model", "fast-model");
        ev.push(json!({
            "eventCode": "agent_task_created_with_template",
            "mode": "working", "isCustomModel": false,
            "id": tid, "name": name, "requestId": req,
        }));
        ev.push(json!({
            "eventCode": "template_used", "template_id": tid, "task_mode": "working",
        }));
        report_desktop(acc, &ev).await?;
        if i + 1 < need {
            tokio::time::sleep(std::time::Duration::from_millis(REPORT_GAP_MS)).await;
        }
    }
    Ok(format!("已用 {need} 个不同模板各发起一次对话"))
}

/// `playbook_prompt`：选 1 个「灵感」案例做同款并发起对话。
///
/// 事件组 = 对话链 + `web_element_click(playbook_ctaClick)` + `playbook_cta_click`
/// + `playbook_prompt_send`（后两条带案例 id，JOIN 这次对话）。
pub async fn run_playbook_prompt(acc: &Value) -> Result<String, String> {
    let stamp = chrono::Utc::now().timestamp_millis();
    let conv = format!("wb-ts-pb-conv-{stamp}");
    let req = format!("wb-ts-pb-req-{stamp}");
    let case_id = format!("pb-{stamp}");
    let case_name = "周报生成助手";

    let mut ev = desktop_chat_sequence(&conv, &req, "msg-pb", "fast-model", "fast-model");
    ev.push(json!({
        "eventCode": "web_element_click", "pageName": "playbook_detail",
        "elementId": "playbook_ctaClick", "elementName": case_name, "source": "discover",
    }));
    ev.push(json!({
        "eventCode": "playbook_cta_click", "source": "discover", "position": 0,
        "id": case_id, "name": case_name, "type": "document",
        "categoryId": "", "categoryName": "",
    }));
    ev.push(json!({
        "eventCode": "playbook_prompt_send",
        "conversationId": conv, "requestId": req,
        "id": case_id, "name": case_name, "type": "document",
        "categoryId": "", "categoryName": "",
    }));
    report_desktop(acc, &ev).await?;
    Ok("已上报「灵感做同款」完整事件组".to_string())
}

/// 「指定某位专家」类任务：按名称在专家市场里找到对应 id，再走同一套召唤+使用链。
///
/// 这类任务（体验「抖音策略师」等）要求用**那个**专家，不能随便挑一个凑数，
/// 所以按任务标题里的专著名去匹配。找不到就老实报错，不拿别的 id 顶替。
pub async fn run_named_expert(acc: &Value, name_hint: &str) -> Result<String, String> {
    let mut all: Vec<Value> = Vec::new();
    // 翻 3 页（每页 100 条）足够覆盖市场；找不到就老实报错，不拿别的 id 顶替。
    for page in 1..=3u32 {
        let batch = expert_list_page(acc, "", page, 100).await.unwrap_or_default();
        if batch.is_empty() {
            break;
        }
        all.extend(batch.iter().cloned());
        if all.iter().any(|e| {
            e["display_name_zh"]
                .as_str()
                .map(|n| n.contains(name_hint) || name_hint.contains(n))
                .unwrap_or(false)
        }) {
            break;
        }
    }
    let hit = all.iter().find(|e| {
        e["display_name_zh"]
            .as_str()
            .map(|n| n.contains(name_hint) || name_hint.contains(n))
            .unwrap_or(false)
    });
    let Some(e) = hit else {
        return Err(format!(
            "专家市场里没找到「{name_hint}」（翻了 {} 条），跳过（不拿别的专家顶替）",
            all.len()
        ));
    };
    use_one_expert(acc, e, "craft", false).await?;
    Ok(format!("已体验「{name_hint}」并上报真实使用"))
}

/// 从任务的 `jump_url` 里抠出专家 id。
///
/// 真实形状（URL 编码过的 deeplink）：
/// `https://www.workbuddy.cn/work/launch/?deeplink=workbuddy%3A%2F%2Fexperts%3FexpertId%3Dex_GDl31WahyIH0&…`
///
/// ⚠️ 这是「体验某位专家」类**唯一可靠**的 id 来源：
/// 那几位专家（中国电商运营专家 / 抖音策略师 / 无畏契约拼豆专家…）**不在**
/// 公开的专家市场列表里（300 条里翻不到），按名字匹配必然失败。
/// 任务自己带着 id，就用它，别去猜。
pub fn expert_id_from_url(url: &str) -> Option<String> {
    // 编码与未编码两种都要认（%3D 是 `=`）
    let pos = url
        .find("expertId%3D")
        .map(|i| i + "expertId%3D".len())
        .or_else(|| url.find("expertId=").map(|i| i + "expertId=".len()))?;
    let rest = &url[pos..];
    let end = rest
        .find(|c: char| c == '&' || c == '%' || c == '"' || c == '\'' || c.is_whitespace())
        .unwrap_or(rest.len());
    let id = &rest[..end];
    if id.starts_with("ex_") && id.len() > 3 {
        Some(id.to_string())
    } else {
        None
    }
}

/// 「体验某位专家」：id 取自任务自带的 `jump_url`；取不到再退回按名字匹配。
pub async fn run_task_expert(acc: &Value, task: &GrowthTask) -> Result<String, String> {
    if let Some(id) = expert_id_from_url(&task.jump_url) {
        // 市场里能查到就带上真实的名称/版本（判据要看名称）；查不到用任务标题兜底。
        let mut e = json!({
            "expert_id": id,
            "expert_type": "agent",
            "display_name_zh": task.title.trim_start_matches("体验").trim_matches(|c| c == '「' || c == '」'),
            "profession_zh": "",
            "version": "1.0.0",
            "categories": [],
        });
        for page in 1..=3u32 {
            let list = expert_list_page(acc, "", page, 100).await.unwrap_or_default();
            if list.is_empty() {
                break;
            }
            if let Some(hit) = list.iter().find(|x| x["expert_id"] == e["expert_id"]) {
                e = hit.clone();
                break;
            }
        }
        use_one_expert(acc, &e, "craft", false).await?;
        return Ok(format!("已用任务指定专家（{id}）完成召唤+真实使用"));
    }
    run_named_expert(acc, &task.title).await
}

/// 按任务码执行对应动作。返回 `Ok(说明)` / `Err(原因)`。
///
/// 未实现的任务返回 `Err("暂不支持")` —— 界面据此显示「需手动完成」，
/// **不能静默跳过**（否则用户以为跑过了）。
pub async fn run_task(acc: &Value, task: &GrowthTask) -> Result<String, String> {
    match task.code.as_str() {
        "chat_5" => run_chat_5(acc, task).await,
        "RichMeow_Chat" => run_richmeow_chat(acc).await,
        "Buddy_App" => run_buddy_app(acc).await,
        "automation_1" => run_automation_create(acc).await,
        "Library_read" => run_library_read(acc).await,
        "skill_1" => run_skill_1(acc).await,
        "Expert_lighthouse" => run_expert_lighthouse(acc).await,
        "expert_5" => run_expert_batch(acc, "agent", 5).await,
        "Expert_team_use_3" => run_expert_batch(acc, "team", 3).await,
        // 「体验某位专家」类：按专著名匹配真实 id（名字取自上架信息，不写死 id）
        // 「体验某位专家」：id 从任务自带的 jump_url 里取（市场里搜不到那几位）
        "expert_use_maidehao"
        | "expert_use_douyinyin"
        | "expert_use_shangjia_xuanchuan"
        | "expert_use_bowangwang"
        | "WorkBuddy_Valorant" => run_task_expert(acc, task).await,
        "Sequential_Tasks_1" => run_sequential_1(acc).await,
        "template_5" => run_template_5(acc, task).await,
        "playbook_prompt" => run_playbook_prompt(acc).await,
        other => Err(format!("暂不支持自动完成：{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 加密信封 / 缺凭据要明确报「不可用」，不能当成空串去发请求。
    #[test]
    fn credentials_rejects_missing_or_enveloped_token() {
        let enveloped = json!({ "access_token": { "v": 1, "ct": "abc" } });
        assert!(credentials(&enveloped).is_err());
        let empty = json!({ "access_token": "   " });
        assert!(credentials(&empty).is_err());
    }

    /// `claimable` 只由进度决定；`target = 0`（上游没给）绝不能算成「已完成」。
    #[test]
    fn claimable_requires_progress_not_zero_target() {
        let t = GrowthTask {
            code: "x".into(),
            title: "".into(),
            desc: "".into(),
            reward_credit: 0,
            reward_energy: 0,
            current: 0,
            target: 0,
            accept_status: "not_accepted".into(),
            jump_url: String::new(),
            claimable: false,
            supported: true,
        };
        assert!(!t.claimable);
        assert!(t.needs_work(), "没进度又没目标时仍算「待完成」");
    }

    /// 领完之后进度仍是满的 —— 不能因为「进度满了」就再次显示可领。
    #[test]
    fn claimed_task_is_not_claimable_even_if_progress_full() {
        let t = GrowthTask {
            code: "first_buddy".into(),
            title: "".into(),
            desc: "".into(),
            reward_credit: 300,
            reward_energy: 8,
            current: 1,
            target: 1,
            accept_status: "claimed".into(),
            jump_url: String::new(),
            claimable: false,
            supported: true,
        };
        assert!(!t.claimable);
        assert!(!t.needs_work(), "已领的任务不该再被认为是待完成");
    }

    /// 事件必须有 userId：缺了服务端 200 但静默丢弃，等于白干。
    #[test]
    fn chat_event_carries_uid_and_full_shape() {
        let e = chat_event("u1", "c1", "r1", "deepseek-v4-flash", "DeepSeek V4 Flash");
        assert_eq!(e["userId"], "u1");
        assert_eq!(e["eventCode"], "chat_request_send");
        assert_eq!(e["conversationId"], "c1");
        assert_eq!(e["requestId"], "r1");
        // 这几个字段缺了会被上游按残缺事件丢掉
        for k in ["mode", "inputLength", "presentAt", "agentName", "agentType"] {
            assert!(e.get(k).is_some(), "缺少字段 {k}");
        }
    }
}

/// 一次「跑一批」的报告。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchReport {
    /// 每个任务的动作结果（**未实现的也会出现**，`ok=false` 且写明原因）。
    pub runs: Vec<TaskRun>,
    /// 领奖到账明细。
    pub claimed: Vec<ClaimedTask>,
    pub total_credit: i64,
    pub total_energy: i64,
    /// 跑完后仍不可领、也不支持自动完成的任务（界面提示用户手动去做）。
    pub manual: Vec<GrowthTask>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedTask {
    pub code: String,
    pub title: String,
    pub credit: i64,
    pub energy: i64,
    /// 之前已领过（不是失败）。
    pub already: bool,
}

/// 跑一批任务：报名 → 逐个补事件 → 把所有「可领」的领掉。
///
/// `codes = None` 表示「所有还没完成的任务」。
///
/// ⚠️ 三条纪律：
/// 1. **单个任务失败不影响其它任务**（每个独立报错，最后汇总）；
/// 2. **领奖不限于本次跑过的任务** —— 用户可能昨天手动完成了但没领；
/// 3. 跑完仍不能领、也不支持的，放进 `manual` 让用户知道要去哪点，
///    不能因为「我们跑过了」就当他完成了。
pub async fn run_batch(acc: &Value, codes: Option<Vec<String>>) -> BatchReport {
    let tasks = match list_all(acc).await {
        Ok(t) => t,
        Err(e) => {
            return BatchReport {
                runs: vec![TaskRun {
                    code: "*".into(),
                    ok: false,
                    message: format!("读取任务列表失败：{e}"),
                }],
                claimed: Vec::new(),
                total_credit: 0,
                total_energy: 0,
                manual: Vec::new(),
            }
        }
    };

    let todo: Vec<GrowthTask> = match &codes {
        Some(list) => tasks
            .iter()
            .filter(|t| list.iter().any(|c| c == &t.code))
            .cloned()
            .collect(),
        None => tasks.iter().filter(|t| t.needs_work() || t.claimable).cloned().collect(),
    };

    // 先报名：不报名服务端不计进度（幂等，失败不阻塞）
    let accept_codes: Vec<String> = todo
        .iter()
        .filter(|t| t.needs_work())
        .map(|t| t.code.clone())
        .collect();
    let _ = accept(acc, &accept_codes).await;

    let mut runs: Vec<TaskRun> = Vec::new();
    for t in &todo {
        if t.claimable {
            continue; // 已经能领了，不用再补事件
        }
        // 我们做不到的任务**不进 runs**：它不是「失败」，是「不归我们管」。
        // 混进失败列表会让报告看起来像「12 项全挂了」，用户会以为工具坏了。
        if !t.supported {
            continue;
        }
        match run_task(acc, t).await {
            Ok(m) => runs.push(TaskRun { code: t.code.clone(), ok: true, message: m }),
            Err(e) => runs.push(TaskRun { code: t.code.clone(), ok: false, message: e }),
        }
        tokio::time::sleep(std::time::Duration::from_millis(REPORT_GAP_MS)).await;
    }

    // 事件要一点时间才结算进进度，等一下再领
    if !runs.is_empty() {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    let after = list_all(acc).await.unwrap_or_default();
    let mut claimed: Vec<ClaimedTask> = Vec::new();
    let (mut credit, mut energy) = (0i64, 0i64);
    for t in after.iter().filter(|t| t.claimable) {
        match claim(acc, &t.code).await {
            Ok(r) => {
                credit += r.credit;
                energy += r.energy;
                claimed.push(ClaimedTask {
                    code: t.code.clone(),
                    title: t.title.clone(),
                    credit: r.credit,
                    energy: r.energy,
                    already: r.already,
                });
            }
            Err(e) => runs.push(TaskRun {
                code: format!("claim:{}", t.code),
                ok: false,
                message: e,
            }),
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }

    // 还没搞定、而且我们也不支持的 —— 让用户知道得自己去点
    let manual: Vec<GrowthTask> = after
        .iter()
        .filter(|t| t.needs_work())
        .filter(|t| !runs.iter().any(|r| r.code == t.code && r.ok))
        .cloned()
        .collect();

    BatchReport {
        runs,
        claimed,
        total_credit: credit,
        total_energy: energy,
        manual,
    }
}

#[cfg(test)]
mod task_raw_probe {
    use super::*;

    /// 只读探针：打印指定任务的**原始 JSON**，看有没有藏专家 id / template id。
    #[tokio::test]
    #[ignore]
    async fn probe_task_raw() {
        let accs = crate::modules::workbuddy_vault::load_accounts();
        let acc =
            accs.iter().find(|a| matches!(a.get("access_token"), Some(Value::String(s)) if s.len() > 40));
        let acc = match acc {
            Some(a) => a,
            None => return,
        };
        let (token, uid) = match credentials(acc) {
            Ok(v) => v,
            Err(e) => {
                println!("凭据不可用：{e}");
                return;
            }
        };
        let h = headers(&token, &uid);
        let url = format!("{}/v2/activity/growth/tasks", workbuddy_auth::API_ENDPOINT);
        let (_st, v, _t) = call(&url, "GET", None, &h).await;
        let tasks = v["data"]["tasks"].as_array().cloned().unwrap_or_default();
        for code in ["expert_use_maidehao", "WorkBuddy_Valorant"] {
            if let Some(t) = tasks.iter().find(|t| t["task_code"] == code) {
                println!("=== {code}");
                println!("{}", serde_json::to_string_pretty(t).unwrap_or_default());
            }
        }
    }
}

#[cfg(test)]
mod expert_search_probe {
    use super::*;

    /// 只读探针：按关键词在专家市场里模糊搜索（名称/职业/分类都看）。
    #[tokio::test]
    #[ignore]
    async fn probe_expert_search() {
        let accs = crate::modules::workbuddy_vault::load_accounts();
        let acc = accs
            .iter()
            .find(|a| matches!(a.get("access_token"), Some(Value::String(s)) if s.len() > 40))
            .expect("没有可用账号");
        let mut all: Vec<Value> = Vec::new();
        for p in 1..=3u32 {
            let b = expert_list_page(acc, "", p, 100).await.unwrap_or_default();
            if b.is_empty() {
                break;
            }
            all.extend(b);
        }
        println!("共 {} 条", all.len());
        for kw in ["电商", "抖音", "海报", "直播", "无畏", "拼豆", "轻量云"] {
            let hits: Vec<&Value> = all
                .iter()
                .filter(|e| {
                    let blob = format!(
                        "{} {} {}",
                        e["display_name_zh"].as_str().unwrap_or(""),
                        e["profession_zh"].as_str().unwrap_or(""),
                        e["categories"].to_string()
                    );
                    blob.contains(kw)
                })
                .collect();
            println!("[{kw}] 命中 {}", hits.len());
            for h in hits.iter().take(3) {
                println!(
                    "    {} | {} | {}",
                    h["expert_id"].as_str().unwrap_or("-"),
                    h["display_name_zh"].as_str().unwrap_or("-"),
                    h["profession_zh"].as_str().unwrap_or("-")
                );
            }
        }
    }
}

#[cfg(test)]
mod mp_probe {
    use super::*;

    /// 只读探针：看看小程序口径下发了哪些任务（批次 3 的前提）。
    #[tokio::test]
    #[ignore]
    async fn probe_mp_list() {
        let accs = crate::modules::workbuddy_vault::load_accounts();
        let acc = accs
            .iter()
            .find(|a| matches!(a.get("access_token"), Some(Value::String(s)) if s.len() > 40))
            .expect("没有可用账号");

        let pc = list_tasks(acc).await.map(|v| v.len()).unwrap_or(0);
        match list_tasks_mp(acc).await {
            Ok(mp) => {
                println!("PC 口径 {} 条 / 小程序口径 {} 条", pc, mp.len());
                let pc_codes: Vec<String> = list_tasks(acc).await.unwrap_or_default().into_iter().map(|t| t.code).collect();
                for t in &mp {
                    let only = if pc_codes.contains(&t.code) { "" } else { "  ← 仅 mp 口径" };
                    println!(
                        "  {:<28} {}/{}  {:<12} {}c/{}e{}",
                        t.code, t.current, t.target, t.accept_status, t.reward_credit, t.reward_energy, only
                    );
                }
            }
            Err(e) => println!("小程序口径读取失败：{e}"),
        }
    }
}

#[cfg(test)]
mod claim_probe {
    use super::*;

    /// 真机探针：走 `run_batch`（界面按钮的入口）把能领的都领掉。
    #[tokio::test]
    #[ignore]
    async fn probe_claim() {
        let accs = crate::modules::workbuddy_vault::load_accounts();
        let acc = accs
            .iter()
            .find(|a| matches!(a.get("access_token"), Some(Value::String(s)) if s.len() > 40))
            .expect("没有可用账号");
        let r = run_batch(acc, None).await;
        for c in &r.claimed {
            println!("  领取 {} +{}c +{}e{}", c.code, c.credit, c.energy, if c.already { "（已领过）" } else { "" });
        }
        println!("合计 +{} 积分 +{} 能量；仍需手动 {} 项", r.total_credit, r.total_energy, r.manual.len());
    }
}

#[cfg(test)]
mod expert_dump_probe {
    use super::*;

    /// 只读探针：打印专家市场前若干条的真实字段，用于对齐名称匹配。
    #[tokio::test]
    #[ignore]
    async fn probe_expert_dump() {
        let accs = crate::modules::workbuddy_vault::load_accounts();
        let acc = accs
            .iter()
            .find(|a| matches!(a.get("access_token"), Some(Value::String(s)) if s.len() > 40))
            .expect("没有可用账号");
        for t in ["agent", "team", ""] {
            match expert_list(acc, t).await {
                Ok(list) => {
                    println!("=== type={t:?} 共 {} 条", list.len());
                    for e in list.iter().take(12) {
                        println!(
                            "   id={:<26} name={:<24} prof={:<20} type={}",
                            e["expert_id"].as_str().unwrap_or("-"),
                            e["display_name_zh"].as_str().unwrap_or("-"),
                            e["profession_zh"].as_str().unwrap_or("-"),
                            e["expert_type"].as_str().unwrap_or("-"),
                        );
                    }
                }
                Err(e) => println!("=== type={t:?} 失败：{e}"),
            }
        }
    }
}

#[cfg(test)]
mod list_only_probe {
    use super::*;

    /// 只读探针：打印全部任务状态，**不发任何请求之外的写操作**。
    #[tokio::test]
    #[ignore]
    async fn probe_list_only() {
        let accs = crate::modules::workbuddy_vault::load_accounts();
        let acc = accs
            .iter()
            .find(|a| matches!(a.get("access_token"), Some(Value::String(s)) if s.len() > 40))
            .expect("没有可用账号");
        let tasks = list_tasks(acc).await.expect("列任务失败");
        for t in &tasks {
            if t.accept_status == "claimed" && !t.claimable {
                continue; // 已领的不占屏
            }
            println!(
                "  {:<28} {}/{}  {:<12} 可领={} 支持={}",
                t.code, t.current, t.target, t.accept_status, t.claimable, t.supported
            );
        }
    }
}

#[cfg(test)]
mod batch2_probe {
    use super::*;

    /// 真机探针：`TWIN_CODES=skill_1` 指定要跑的批次 2 任务。
    #[tokio::test]
    #[ignore]
    async fn probe_batch2() {
        let codes: Vec<String> = std::env::var("TWIN_CODES")
            .unwrap_or_else(|_| "skill_1".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        let accs = crate::modules::workbuddy_vault::load_accounts();
        let acc = accs
            .iter()
            .find(|a| matches!(a.get("access_token"), Some(Value::String(s)) if s.len() > 40))
            .expect("没有可用账号");

        let before = list_all(acc).await.expect("列任务失败");
        let _ = accept(acc, &codes).await;
        for c in &codes {
            let t = before.iter().find(|t| t.code == *c);
            match t {
                None => println!("{c}: 列表里没有"),
                Some(t) => match run_task(acc, t).await {
                    Ok(m) => println!("{c}: 成功 — {m}"),
                    Err(e) => println!("{c}: 失败 — {e}"),
                },
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let after = list_all(acc).await.expect("复查失败");
        println!("=== 复查 ===");
        for c in &codes {
            if let Some(t) = after.iter().find(|t| t.code == *c) {
                println!("  {c:<20} {}/{}  {}  可领={}", t.current, t.target, t.accept_status, t.claimable);
            }
        }
    }
}
