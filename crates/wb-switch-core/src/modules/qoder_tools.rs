//! Qoder 账号包导出 / 导入 / 每日签到。
//!
//! 三件事放一个文件，因为它们都以「账号库」为操作对象、且体量都不大。
//!
//! ## 导出：文件里是**明文凭据**
//!
//! 导出是凭据库的快照（`accessToken` / `refreshToken` 都是明文），拿到文件就能冒充这些账号。
//! 所以：**不做静默脱敏**（脱敏后搬不回新机器，用户以为自己备份成功了 —— 比泄露更隐蔽的失败），
//! 但界面必须给警告。
//!
//! ## 导入：先预览，再按**下标**勾选
//!
//! 用下标而不是 uid：外部文件可能缺 uid、也可能两条记录同 uid，按下标选才不会
//! 「勾了三条只进一条」。`indexes=None` = 全部，`Some([])` = 一条都不导 —— 两者语义必须不同。
//!
//! **跨区条目一律拒**（而不是塞进 skipped）：把国际版账号写进国内版库，用户看到的是
//! 「账号在，但登录不了」，错误无声、排查昂贵。
//!
//! ## 签到
//!
//! `GET/POST {openapi_base}/sash/api/v1/me/daily-check-in/{status,claim}`，
//! 头用 openapi 那套（普通鉴权 + 机器码，不是 COSY）。
//! 404/405/410 表示该区域没有签到功能 —— **如实说「没有」**，不当成失败。

use serde_json::{json, Map, Value};

use crate::modules::qoder_accounts::{self, QoderAccount};
use crate::modules::qoder_realm::{realm_spec, QoderRegion};


fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("构造 HTTP 客户端失败：{e}"))
}

// ---------------------------------------------------------------- 导出

/// 导出账号包（**含明文凭据**）。返回 `{ count, filename, json, dir }`。
///
/// 落盘到 `~/.twin-switch/qoder/exports/`，同时把 JSON 文本回给前端（方便用户直接复制）。
pub fn export(region: QoderRegion) -> Result<Value, String> {
    let list = qoder_accounts::list(region)?;
    if list.is_empty() {
        return Err(format!("{} 还没有账号，没什么可导出的", region.as_str()));
    }

    let items: Vec<Value> = list
        .iter()
        .map(|a| {
            json!({
                "uid": a.uid,
                "name": a.name,
                "email": a.email,
                "domain": realm_spec(region).domain,
                "accessToken": a.access_token,
                "refreshToken": a.refresh_token,
                "source": a.source,
                "plan": a.plan,
                "credits": a.credits,
                "creditsExpireAt": a.credits_expire_at,
                "addedAt": a.created_at,
            })
        })
        .collect();

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let filename = format!("qoder-accounts-{}-{stamp}.json", region.as_str());
    let dir = crate::modules::config::store_dir().join("qoder").join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let path = dir.join(&filename);
    let text = serde_json::to_string_pretty(&items).map_err(|e| format!("序列化失败：{e}"))?;
    std::fs::write(&path, text.as_bytes()).map_err(|e| format!("写入失败：{e}"))?;

    Ok(json!({
        "count": items.len(),
        "filename": filename,
        "path": path.display().to_string(),
        "json": text,
    }))
}

// ---------------------------------------------------------------- 导入

/// 脱敏预览：只给展示字段与「有没有凭据」，**不回令牌明文**
/// （用户常把界面截图发出来报障，明文上屏就等于泄露）。
pub fn import_preview(region: QoderRegion, json_text: &str) -> Result<Value, String> {
    let items = parse_items(json_text)?;
    let want = realm_spec(region).domain;

    let mut rows = Vec::new();
    let mut rejected = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let uid = str_of(item, "uid");
        let name = str_of(item, "name");
        let domain = str_of(item, "domain");
        let has_token = !str_of(item, "accessToken").is_empty();
        let region_ok = domain_matches(&domain, want);
        if !region_ok {
            rejected.push(json!({
                "index": index,
                "uid": uid,
                "reason": format!("domain = {domain:?}，与当前区域 {want} 不符"),
            }));
            continue;
        }
        rows.push(json!({
            "index": index,
            "uid": uid,
            "name": name,
            "hasToken": has_token,
        }));
    }

    Ok(json!({
        "region": region.as_str(),
        "total": items.len(),
        "items": rows,
        "rejected": rejected,
    }))
}

/// 按下标导入。`indexes = None` 表示「全部可导入的条目」；`Some([])` 表示一条都不导。
pub fn import_apply(
    region: QoderRegion,
    json_text: &str,
    indexes: Option<Vec<usize>>,
) -> Result<Value, String> {
    let items = parse_items(json_text)?;
    let want = realm_spec(region).domain.to_string();

    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut rejected: Vec<Value> = Vec::new();

    let existing = qoder_accounts::list(region).unwrap_or_default();

    for (index, item) in items.iter().enumerate() {
        if let Some(sel) = &indexes {
            if !sel.contains(&index) {
                continue;
            }
        }
        let uid = str_of(item, "uid");
        let domain = str_of(item, "domain");
        if !domain_matches(&domain, &want) {
            // 跨区拒绝：不塞进 skipped，否则用户会以为「只是被去重了」。
            rejected.push(json!({
                "index": index,
                "uid": uid,
                "reason": format!("domain = {domain:?}，与当前区域 {want} 不符"),
            }));
            continue;
        }
        if uid.is_empty() {
            rejected.push(json!({ "index": index, "uid": "", "reason": "缺少 uid" }));
            continue;
        }
        let token = str_of(item, "accessToken");
        if token.is_empty() {
            skipped += 1;
            continue;
        }

        match existing.iter().find(|a| a.uid == uid) {
            Some(old) => {
                let id = old.id.clone();
                let name = str_of(item, "name");
                let refresh = str_of(item, "refreshToken");
                qoder_accounts::update(region, &id, |a| {
                    a.access_token = Some(token.clone());
                    if !refresh.is_empty() {
                        a.refresh_token = Some(refresh.clone());
                    }
                    if !name.is_empty() {
                        a.name = name.clone();
                    }
                    a.source = qoder_accounts::SOURCE_DESKTOP_APP.to_string();
                })?;
                updated += 1;
            }
            None => {
                let mut acc = QoderAccount::new(region, &uid, str_of(item, "name"));
                acc.access_token = Some(token);
                let refresh = str_of(item, "refreshToken");
                acc.refresh_token = (!refresh.is_empty()).then_some(refresh);
                acc.source = qoder_accounts::SOURCE_DESKTOP_APP.to_string();
                qoder_accounts::add(acc)?;
                imported += 1;
            }
        }
    }

    Ok(json!({
        "imported": imported,
        "updated": updated,
        "skipped": skipped,
        "rejected": rejected,
    }))
}

fn parse_items(text: &str) -> Result<Vec<Map<String, Value>>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("不是合法 JSON：{e}"))?;
    let arr = v
        .as_array()
        .ok_or_else(|| "账号包应该是一个 JSON 数组".to_string())?;
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            return Err(format!("第 {i} 条不是对象"));
        };
        out.push(obj.clone());
    }
    if out.is_empty() {
        return Err("账号包是空数组".into());
    }
    Ok(out)
}

fn str_of(item: &Map<String, Value>, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 域名字段可能是 `qoder.com.cn` 或完整 URL；空域名按 **fail-closed** 处理。
fn domain_matches(domain: &str, want: &str) -> bool {
    let d = domain.trim().to_ascii_lowercase();
    (d == want || d.ends_with(&format!(".{want}"))) && !d.is_empty()
}

// ---------------------------------------------------------------- 签到（活动制）

const PATH_CAMPAIGNS: &str = "/sash/api/v1/me/campaigns";

/// 活动型签到用的请求头：`openapi_headers` + 客户端的 `cosy-machine*` 设备身份。
///
/// ⚠️ 这组头**不是 COSY 签名**（`authorization` 仍是普通 `Bearer dt-…`），
/// 只是把设备身份带上去。抓包实测：缺了它们 `/me/campaigns` 不返回活动。
/// `cosy-version` 用客户端版本 `0.4.8`（实测通过），UA 也必须是 `Qoder`。
fn campaign_headers(region: QoderRegion, account: &QoderAccount, token: &str) -> Vec<(String, String)> {
    // ⚠️⚠️ 设备头的取舍（照参考实现 `campaign.rs::desktop_headers` 的注释，实测踩坑）：
    //
    // **发派生假头会被服务端判定为非官方客户端，把 CLAIMABLE 的「每日领取 100 Credits」
    // 整条过滤掉**（表现为活动列表为空）—— 比不发更糟。「不发」是正确行为，不是缺省。
    //
    // 所以只有两种形态：
    // - 账号库里存了**真实**设备身份（用户从抓包粘贴的 `cosy-machinetoken/type`）→ 全套照发；
    // - 没存 → 一个 `cosy-machine*` 头都不发，只带 `cosy-clienttype` / `cosy-version`。
    let stored_token = account
        .cosy_machine_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let stored_type = account
        .cosy_machine_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let native = stored_token.zip(stored_type);

    let mut out: Vec<(String, String)> = vec![
        ("accept".into(), "application/json".into()),
        ("authorization".into(), format!("Bearer {token}")),
        ("cosy-clienttype".into(), "10".into()),
        ("cosy-version".into(), "0.4.8".into()),
        ("user-agent".into(), "Qoder".into()),
    ];

    // ★★ 必须带**全套真实机器头**，否则服务端不下发「每天领 100 Credits」：
    //    只带上面那 4 个 → claimable=false、只有 2 个 VIEW_DETAILS；
    //    带全套 6 个 → claimable=true、3 个活动含 CLAIM_BENEFIT/CLAIMABLE/100。
    //    （真机对照见 `qoder_machine` 模块文档）
    if let Some(machine) = crate::modules::qoder_machine::identity(region) {
        out.extend(machine.headers());
    }

    if let Some((mtok, mtype)) = native {
        let id = crate::modules::qoder_realm::derive_id(&account.uid, "machine");
        out.push(("cosy-machineid".into(), id.clone()));
        out.push(("cosy-machinetoken".into(), mtok.to_string()));
        out.push(("cosy-machinetype".into(), mtype.to_string()));
        out.push(("cosy-machinecode".into(), id.chars().take(20).collect()));
        out.push(("cosy-machineos".into(), "x86_64_win32".into()));
        out.push((
            "cosy-machinehostname".into(),
            std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into()),
        ));
    }
    let _ = region;
    out
}

fn token_of<'a>(account: &'a QoderAccount) -> Result<&'a str, String> {
    account
        .access_token
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "这个账号还没有凭据".to_string())
}

/// 拉活动列表（签到就在这里面）。
pub async fn fetch_campaigns(region: QoderRegion, account: &QoderAccount) -> Result<Value, String> {
    let token = token_of(account)?;
    let spec = realm_spec(region);
    let mut req = client()?
        .get(format!("{}{PATH_CAMPAIGNS}", spec.openapi_base))
        .timeout(std::time::Duration::from_secs(20));
    for (k, v) in campaign_headers(region, account, token) {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| format!("活动列表请求失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "活动列表请求失败：HTTP {} {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ));
    }
    serde_json::from_str(&body).map_err(|e| format!("活动列表不是 JSON：{e}"))
}

/// 领某个活动。返回 `{ok, already, message, grant}`。
pub async fn claim_campaign(
    region: QoderRegion,
    account: &QoderAccount,
    campaign_id: &str,
) -> Result<Value, String> {
    let token = token_of(account)?;
    let spec = realm_spec(region);
    // ⚠️ 抓包实测客户端发的是**空 body**（content-length: 0），不是 `{}` —— 照抄。
    let mut req = client()?
        .post(format!(
            "{}/sash/api/v1/me/campaigns/{campaign_id}/claim",
            spec.openapi_base
        ))
        .timeout(std::time::Duration::from_secs(20));
    for (k, v) in campaign_headers(region, account, token) {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| format!("领取请求失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Ok(json!({
            "ok": false,
            "message": format!("HTTP {} {}", status.as_u16(), body.chars().take(200).collect::<String>()),
        }));
    }
    let v: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
    // `replayed: true` = 之前已经领过，这次是**幂等重放**（不是又发了一份）。
    let replayed = v.get("replayed").and_then(Value::as_bool).unwrap_or(false);
    let amount = v.pointer("/benefit/amount").and_then(Value::as_f64);
    Ok(json!({
        "ok": true,
        "already": replayed,
        "gained": if replayed { Value::Null } else { json!(amount) },
        "message": if replayed {
            format!("今日已领过{}", amount.map(|a| format!("（{a:.0} credits）")).unwrap_or_default())
        } else {
            format!("领取成功{}", amount.map(|a| format!("，+{a:.0} credits")).unwrap_or_default())
        },
        "grant": v,
    }))
}

/// 签到状态（活动制）。
pub async fn checkin_status(region: QoderRegion, account: &QoderAccount) -> Result<Value, String> {
    let payload = fetch_campaigns(region, account).await?;
    let camps = payload
        .get("campaigns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let claimable: Vec<&Value> = camps
        .iter()
        .filter(|c| c.get("claimStatus").and_then(Value::as_str) == Some("CLAIMABLE"))
        .collect();
    Ok(json!({
        "available": true,
        "claimable": payload.get("claimable").cloned().unwrap_or(json!(false)),
        "claimableCount": claimable.len(),
        "campaigns": camps.iter().map(|c| json!({
            "key": c.get("campaignKey"),
            "status": c.get("claimStatus"),
            "kind": c.pointer("/benefit/kind"),
            "amount": c.pointer("/benefit/amount"),
            "endAt": c.get("endAt"),
            "unavailableReason": c.get("unavailableReason"),
        })).collect::<Vec<_>>(),
        "raw": payload,
    }))
}

/// 签到：**活动制**，且只认 `actionType == "CLAIM_BENEFIT"`。
///
/// ⚠️ 必须**按 actionType 过滤**（2026-10-21 用户实测踩坑）：国际版有一个
/// `act-20261020-944`，`actionType = VIEW_DETAILS`、**没有 benefit** ——
/// 它是"看详情"引导，根本无积分可领；但它的 `claimStatus` 也会走到 `CLAIMED`，
/// 旧实现把它当「签到成功」⇒ 弹窗显示「领取成功」而客户端里什么都没领到。
///
/// 三档：
/// - 有 `CLAIM_BENEFIT` 且 `CLAIMABLE` → 领，`replayed` 视为「今日已领」；
/// - `CLAIM_BENEFIT` 已全部 `CLAIMED` → `ok:true, already:true`（这次才是真的已签）；
/// - 其余（只有 VIEW_DETAILS / 前置条件未达成 / 无活动）→ `ok:false, blocked:true`
///   —— **既不是成功也不是失败**。
/// 签到（带**重试**）。
///
/// ## 为什么要重试
///
/// 实测（用户抓包 + 真机）：同一个账号，工具查询有时只拿到 `VIEW_DETAILS`，
/// 几分钟后客户端一查就有 `CLAIM_BENEFIT`（每天 100 Credits，`claimable: true`）。
/// 服务端下发活动有个**时间差**，而抓包里三次 `GET /me/campaigns` 都是**同一个接口**、
/// 第一次就带 `CLAIM_BENEFIT` —— 所以**没有别的"下发接口"**可调，只能等它下发。
///
/// 因此遇到「有活动但都不是领积分型」时**重试 3 轮**（间隔 3 秒），别直接报「无活动」。
pub async fn checkin_claim(region: QoderRegion, account: &QoderAccount) -> Result<Value, String> {
    let mut last = Value::Null;
    for attempt in 0..3 {
        let r = checkin_claim_once(region, account).await?;
        let retryable = r.get("reason").and_then(Value::as_str) == Some("NO_BENEFIT_CAMPAIGN");
        last = r;
        if !retryable || attempt == 2 {
            return Ok(last);
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    Ok(last)
}

/// 单次尝试（不重试）。见 [`checkin_claim`] 的说明。
pub async fn checkin_claim_once(
    region: QoderRegion,
    account: &QoderAccount,
) -> Result<Value, String> {
    let payload = fetch_campaigns(region, account).await?;
    let empty = Vec::new();
    let camps = payload
        .get("campaigns")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    // ⚠️ 判据照参考实现（`campaign.rs::classify` / `is_credit_kind`），两个坑：
    //
    // ① `actionType` **为空也要算可领**：老活动 / 部分区域不写 actionType，只写 benefit。
    //    只认字面上的 `CLAIM_BENEFIT` 会把它们全过滤掉（国际版实测）。
    // ② **`CLAIMED` 要先判**：官方在**领取后**会把 actionType 降级成 `VIEW_DETAILS`，
    //    先按 actionType 过滤就会把「今天已经领过」误报成「没有可领积分的活动」。
    let actionable = |c: &&Value| {
        let at = c.get("actionType").and_then(Value::as_str).unwrap_or("");
        at.is_empty() || at.eq_ignore_ascii_case("CLAIM_BENEFIT")
    };
    let status_of = |c: &&Value| {
        c.get("claimStatus")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_uppercase()
    };
    let amount_of = |c: &&Value| {
        c.pointer("/benefit/amount")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    // 已领：`CLAIMED` 且（有奖励 或 actionType 仍是可领类）。
    let already: Vec<&Value> = camps
        .iter()
        .filter(|c| status_of(c) == "CLAIMED" && (amount_of(c) > 0.0 || actionable(c)))
        .collect();

    // 可领 / 待领：只看 actionType 可领类。
    let benefit: Vec<&Value> = camps.iter().filter(|c| actionable(c)).collect();

    if !already.is_empty() {
        let amount = already.iter().map(|c| amount_of(c)).fold(0.0_f64, f64::max);
        return Ok(json!({
            "ok": true,
            "already": true,
            "gained": Value::Null,
            "message": format!(
                "今日已签到{}",
                if amount > 0.0 {
                    format!("（{amount:.0} credits，每日 10:00 后刷新）")
                } else {
                    String::new()
                }
            ),
        }));
    }

    if benefit.is_empty() {
        // 两种情况要分开说：服务端**根本没下发**活动（新号/资格未开放，camps 为空），
        // 与「有活动但都不是领积分型」（全是 VIEW_DETAILS 之类）。
        let reason = if camps.is_empty() {
            "NO_CAMPAIGNS_FOR_ACCOUNT"
        } else {
            "NO_BENEFIT_CAMPAIGN"
        };
        let message: String = if camps.is_empty() {
            "上游没有给这个账号下发任何活动（新号或活动资格未开放；以客户端内实际显示为准）".to_string()
        } else {
            // 参考实现的三态语义：**有活动平台但没积分类活动** ≠ 失败，也 ≠ 已签。
            // 国际版目前只下发 VIEW_DETAILS 型社区活动（如「1024 创造力集结」），
            // 官方本来就没有每日签到积分；如实说清，别让用户以为是我们坏了。
            let kinds: Vec<String> = camps
                .iter()
                .filter_map(|c| c.get("actionType").and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            let uniq: Vec<String> = {
                let mut v = kinds;
                v.sort();
                v.dedup();
                v
            };
            format!(
                "上游下发了 {} 个活动，但都不是「领积分」型（类型：{}）——国际版目前没有每日签到积分，这是官方行为",
                camps.len(),
                if uniq.is_empty() { "未标注".to_string() } else { uniq.join(" / ") }
            )
        };
        return Ok(json!({ "ok": false, "blocked": true, "reason": reason, "message": message }));
    }

    let claimable: Vec<&Value> = benefit
        .iter()
        .copied()
        .filter(|c| status_of(c) == "CLAIMABLE")
        .collect();

    if let Some(c) = claimable.first() {
        let Some(id) = c.get("campaignId").and_then(Value::as_str) else {
            return Ok(json!({ "ok": false, "message": "活动缺少 campaignId" }));
        };
        return claim_campaign(region, account, id).await;
    }



    let why = benefit
        .iter()
        .find_map(|c| c.get("unavailableReason").and_then(Value::as_str))
        .unwrap_or("NO_CLAIMABLE_CAMPAIGN");
    Ok(json!({
        "ok": false,
        "blocked": true,
        "reason": why,
        "message": format!("有积分活动但暂时领不了（{why}）"),
    }))
}

/// Qoder 的**模型清单**（COSY 签名走推理网关，不是 openapi）。
///
/// 端点与参考实现一致：`GET {gateway_base}/algo/api/v2/model/list?Encode=1`，
/// body 是 COSY 编码后的 `{}`，**签名覆盖的 path 不带 query**（`/algo` 前缀也要剥掉，
/// `headers_for` 内部已处理）。响应里 `chat[].key` 才是发给上游的模型名，
/// `display_name` 只是界面显示。
pub async fn fetch_models(
    region: QoderRegion,
    account: &QoderAccount,
) -> Result<Vec<(String, String)>, String> {
    let body = crate::modules::qoder_cosy::qoder_encode(b"{}");
    let sign_path = "/algo/api/v2/model/list";
    let headers = crate::modules::qoder_cosy::headers_for(account, &body, sign_path, None, false)?;
    // 逐个候选主机试（国际版 api1 常常连不上，只试第一个就整条失败）。
    let path = "/algo/api/v2/model/list?Encode=1";
    let mut last_err = String::new();
    let mut text = String::new();
    let mut ok = false;
    for host in crate::modules::qoder_realm::gateway_candidates(region) {
        let mut req = client()?
            .get(format!("{host}{path}"))
            .body(body.clone())
            .timeout(std::time::Duration::from_secs(15));
        for (k, v) in headers.iter() {
            req = req.header(k.as_str(), v.as_str());
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    text = body_text;
                    ok = true;
                    break;
                }
                last_err = format!("HTTP {} {}", status.as_u16(), body_text.chars().take(160).collect::<String>());
            }
            Err(e) => last_err = format!("{host}：{e}"),
        }
    }
    if !ok {
        return Err(format!("模型清单拉取失败（已试全部候选主机）：{last_err}"));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("模型清单不是 JSON：{e}"))?;
    let mut out = Vec::new();
    if let Some(chat) = v.get("chat").and_then(Value::as_array) {
        for item in chat {
            let Some(key) = item.get("key").and_then(Value::as_str) else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            let display = item
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_string();
            out.push((key.to_string(), display));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- 对话（最小验证）
//
// Qoder 的对话是**信封 SSE**：请求体 = 模板（`qoder_baseprompt.json`）+ 填充，
// COSY 编码后 POST 到 `/algo/api/v2/service/pro/sse/agent_chat_generation?Encode=1`。
// 这里先做「能发出去、能拿到响应」的最小实现，用于验证协议；完整信封解包另做。

/// 请求体模板（`include_str!` 编译进二进制，解析失败即错误，不兜底）。
pub fn chat_template() -> Result<Value, String> {
    static T: std::sync::OnceLock<Result<Value, String>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        serde_json::from_str::<Value>(include_str!("qoder_baseprompt.json"))
            .map_err(|e| format!("解析 Qoder 请求骨架失败：{e}"))
    })
    .clone()
}

/// 用模板拼一个最小对话请求体。
pub fn build_chat_body(model_key: &str, display_name: &str, prompt: &str) -> Result<Value, String> {
    let mut body = chat_template()?;
    let now_id = uuid::Uuid::new_v4().to_string();
    body["request_id"] = Value::String(now_id.clone());
    body["chat_record_id"] = Value::String(now_id);
    body["request_set_id"] = Value::String(uuid::Uuid::new_v4().to_string());
    body["session_id"] = Value::String(uuid::Uuid::new_v4().to_string());
    body["stream"] = Value::Bool(true);
    body["model_config"]["key"] = Value::String(model_key.to_string());
    body["model_config"]["display_name"] = Value::String(display_name.to_string());
    // 模板自带 system prompt（`messages[0]`），只追加用户消息。
    if let Some(arr) = body.get_mut("messages").and_then(Value::as_array_mut) {
        arr.push(json!({ "role": "user", "content": prompt }));
    }
    // 最新用户提示词（上游用它做高亮/会话名）；缺它上游可能直接拒。
    if let Some(text) = body
        .get_mut("chat_context")
        .and_then(|c| c.get_mut("text"))
        .and_then(Value::as_object_mut)
    {
        text.insert("text".into(), Value::String(prompt.to_string()));
    }
    Ok(body)
}

/// 发一次对话，返回**原始响应文本**（前 `take` 字符）——用于确认信封格式。
pub async fn chat_probe(
    region: QoderRegion,
    account: &QoderAccount,
    model_key: &str,
    display_name: &str,
    prompt: &str,
    take: usize,
) -> Result<String, String> {
    let body_value = build_chat_body(model_key, display_name, prompt)?;
    let plain = body_value.to_string();
    let encoded = crate::modules::qoder_cosy::qoder_encode(plain.as_bytes());
    let sign_path = "/algo/api/v2/service/pro/sse/agent_chat_generation";
    let headers = crate::modules::qoder_cosy::headers_for(account, &encoded, sign_path, Some(model_key), true)?;
    let url = format!(
        "{}{}?Encode=1",
        crate::modules::qoder_cosy::gateway_base(region),
        sign_path
    );
    let mut req = client()?
        .post(&url)
        .body(encoded)
        .timeout(std::time::Duration::from_secs(120));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| format!("对话请求失败：{e}"))?;
    let status = resp.status();
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut text = String::new();
    // 只读前若干块就返回（上游流不会自己关，text() 会挂住）。
    for _ in 0..6 {
        match tokio::time::timeout(std::time::Duration::from_secs(20), stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                text.push_str(&String::from_utf8_lossy(&chunk));
                if text.len() >= take {
                    break;
                }
            }
            Ok(Some(Err(e))) => {
                text.push_str(&format!("[读流出错 {e}]"));
                break;
            }
            Ok(None) => break,
            Err(_) => {
                text.push_str("[20s 无新数据]");
                break;
            }
        }
    }
    Ok(format!(
        "HTTP {} | 前 {} 字符：{}",
        status.as_u16(),
        take,
        text.chars().take(take).collect::<String>()
    ))
}

/// 完整跑一次 Qoder 对话，返回**标准 OpenAI completion**（非流式聚合）。
///
/// 上游是**信封 SSE**，每行 `data:{headers, body, statusCode}`，`body` 是内层
/// OpenAI chunk 的 JSON **字符串**（真机确认，2026-10-22）：
///
/// ```text
/// data:{"headers":{"Content-Type":["application/json"]},"body":"{\"choices\":[{\"delta\":{...}}]}","statusCode":"OK"}
/// ```
///
/// 所以：逐行剥 `data:` → 解析信封 → 二次解析 `body` 字符串 → 累加 `delta.content`。
pub async fn chat_complete(
    region: QoderRegion,
    account: &QoderAccount,
    model_key: &str,
    display_name: &str,
    messages: &Value,
) -> Result<Value, String> {
    chat_complete_ex(region, account, model_key, display_name, messages, None).await
}

/// [`chat_complete`] + **工具定义透传**。
///
/// ⚠️ 2026-10-24 之前这里**从来没把 `tools` 发给上游**：`build_chat_body` 只拼
/// `messages`，模板里 `tools` 永远是 `[]`。于是客户端带工具（尤其带文件、
/// 让模型读表）时，模型看不到任何工具，只能凭空吐一段
/// `<function_calls><invoke …>` 正文 —— 经 [`lift_qoder_tool_calls`] 提升后
/// `content` 变 `null`，客户端拿到的就是「已完成但一片空白」。
///
/// 模板里 `model_config.format = "openai"`，所以 `tools` 按 OpenAI 形状塞：
/// `[{type:"function",function:{name,description,parameters}}]`。
///
/// 带工具失败（非排队）时**降级重试一次不带工具**：某些模型/账号不支持工具，
/// 不能让一个格式差异把整家 Qoder 变成不可用。降级不是静默 —— 错误原文留在
/// 最后一次失败里，`degraded` 会带回去让上层能说清「这轮没工具」。
pub async fn chat_complete_ex(
    region: QoderRegion,
    account: &QoderAccount,
    model_key: &str,
    display_name: &str,
    messages: &Value,
    tools: Option<&Value>,
) -> Result<Value, String> {
    let empty = |t: &Option<&Value>| {
        t.and_then(Value::as_array)
            .map(|a| a.is_empty())
            .unwrap_or(true)
    };
    let mut send_tools: Option<&Value> = if empty(&tools) { None } else { tools };
    let mut degraded = false;
    // ⚠️ 排队按**官方语义**处理（2026-10-24 改造）：
    //   官方客户端收到 10605 不是「失败」，而是 sleep(retryAfterSeconds) → 查 queue/status →
    //   没空位就继续等，**不放弃、也不换号**（队列是模型级的，换号只会把几个号一起打进冷却）。
    //   我们以前一律砍到 12 秒 × 3 轮 = 36 秒就放弃，而热门模型真实排队是 60 秒到几分钟，
    //   于是日志里一大片 429「排队中」。
    let budget = queue_budget_secs();
    let started = std::time::Instant::now();
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match chat_once(region, account, model_key, display_name, messages, send_tools).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if let Some(rest) = queue_marker(&e) {
                    let mut parts = rest.splitn(2, '|');
                    let wait: u64 = parts.next().unwrap_or("30").trim().parse().unwrap_or(30);
                    let msg = parts.next().unwrap_or("上游排队中").to_string();
                    let spent = started.elapsed().as_secs();
                    let nap = wait.clamp(1, MAX_QUEUE_SLEEP);
                    if attempts <= MAX_QUEUE_ATTEMPTS && spent + nap <= budget {
                        tokio::time::sleep(std::time::Duration::from_secs(nap)).await;
                        continue;
                    }
                    return Err(format!(
                        // ⚠️ 文案里**不许出现代号**（`qmodel` / `dfmodel`）：界面和复制都
                        // 已经只用客户端里的名字，提示再给代号等于让用户去认一个界面
                        // 上根本看不到的字符串（2026-10-24）。
                        "QUEUED|{wait}|{msg}（已等 {spent} 秒仍在排队 —— 这是上游排队，不是额度不足，\
                         也不是账号坏了；换号没用，队列是模型级的。可换冷门模型试试，如 GLM-5.3 / Qwen3.8-Max）"
                    ));
                }
                // 非排队失败：第一次先把 tools 摘掉重试（不支持工具的模型/账号），
                // 还失败就如实报错。
                if send_tools.is_some() && !degraded {
                    degraded = true;
                    send_tools = None;
                    // 降级重试也要说清：错误原文留在最后一次失败里，不吞。
                    eprintln!("[qoder] 带工具失败，降级为不带工具重试一次：{e}");
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// 排队总预算（秒）：默认 240，可用 `TWIN_TS_QUEUE_BUDGET` 覆盖。
///
/// 给到 4 分钟的理由：客户端自己愿意等 10 分钟，我们只等 36 秒等于**主动放弃**
/// 一个本来能成的请求，然后把失败写进冷却、还换个号再排一次。
fn queue_budget_secs() -> u64 {
    std::env::var("TWIN_TS_QUEUE_BUDGET")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(240)
}

/// 单次睡眠上限：照上游给的 `retryAfterSeconds` 等，但一次别睡太久
/// （中途可能就排到了，也可能用户已经关掉了客户端）。
const MAX_QUEUE_SLEEP: u64 = 45;
/// 最多问几次（预算是主闸，这个是兜底防止上游一直回极短的 wait 把循环跑成死转）。
const MAX_QUEUE_ATTEMPTS: usize = 12;

/// 从错误文本里取排队标记后面的内容（`<秒>|<文案>`）。
///
/// 标记不一定在开头：`chat_once` 会把它包成 `上游报错：QUEUED|30|…`。
/// 只看开头就等于「自动等待重试」永远不生效（线上踩过）。
fn queue_marker(err: &str) -> Option<&str> {
    const MARK: &str = "QUEUED|";
    err.find(MARK).map(|i| &err[i + MARK.len()..])
}

/// 单次尝试（不做重试）。解析见 [`chat_complete`]。
/// 拼出**完整的** Qoder 对话请求体（模板 + 工具 + 全部消息 + `chat_context`）。
///
/// `session_id` 用**指纹派生**（同一段对话每轮复用同一个 ID）：上游才认得出「还是这个会话」，
/// 前缀缓存才有机会命中。`request_id` / `chat_record_id` 仍然每次新生成 ——
/// 它们是**请求**身份而不是会话身份，复用会让上游把两次请求当成同一次重试。
fn full_body(
    model_key: &str,
    display_name: &str,
    messages: &Value,
    tools: Option<&Value>,
    uid: &str,
) -> Result<Value, String> {
    let mut body = build_chat_body(model_key, display_name, "")?;
    // 工具定义按 OpenAI 形状塞进模板的 `tools`（模板默认 `[]`，等于「没有工具」）。
    // 不塞的话模型看不到任何工具，只会把调用写成 XML 正文 —— 客户端就只剩空白。
    if let Some(list) = tools.and_then(Value::as_array) {
        if !list.is_empty() {
            body["tools"] = Value::Array(list.clone());
        }
    }
    if let Some(arr) = messages.as_array() {
        if let Some(list) = body.get_mut("messages").and_then(Value::as_array_mut) {
            // 模板自带 system；客户的 messages 全部接在后面（含 system 时它会变成两条，可接受）。
            for m in arr {
                list.push(m.clone());
            }
        }
        // chat_context.text 用最后一条 user 的文本。
        if let Some(last_user) = arr
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        {
            let text = last_user
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(t) = body
                .get_mut("chat_context")
                .and_then(|c| c.get_mut("text"))
                .and_then(Value::as_object_mut)
            {
                t.insert("text".into(), Value::String(text.to_string()));
            }
        }
    }
    let seed = crate::modules::gateway_stream::session_seed(model_key, uid, messages, tools);
    body["session_id"] = json!(crate::modules::gateway_stream::session_id(&seed, "qoder-sess"));
    Ok(body)
}

/// 发出去并返回**未读的响应**（状态码已判）。
///
/// `timeout_s` 分两档：聚合路径 180 s，流式路径 900 s。流式下 reqwest 的超时管的是
/// **整条流**，用 180 s 卡会把「模型在慢慢写」当成「上游挂了」。
async fn post_stream(
    region: QoderRegion,
    account: &QoderAccount,
    plain: &str,
    model_key: &str,
    timeout_s: u64,
) -> Result<reqwest::Response, String> {
    let encoded = crate::modules::qoder_cosy::qoder_encode(plain.as_bytes());
    let sign_path = "/algo/api/v2/service/pro/sse/agent_chat_generation";
    let headers =
        crate::modules::qoder_cosy::headers_for(account, &encoded, sign_path, Some(model_key), true)?;
    // 逐个候选主机试（与模型清单同一套 fallback）。
    let mut resp = None;
    let mut last_err = String::new();
    for host in crate::modules::qoder_realm::gateway_candidates(region) {
        let mut req = client()?
            .post(format!("{host}{sign_path}?Encode=1"))
            .body(encoded.clone())
            .timeout(std::time::Duration::from_secs(timeout_s));
        for (k, v) in headers.iter() {
            req = req.header(k.as_str(), v.as_str());
        }
        match req.send().await {
            Ok(r) => {
                resp = Some(r);
                break;
            }
            Err(e) => last_err = format!("{host}：{e}"),
        }
    }
    let resp = resp.ok_or_else(|| format!("对话请求失败（已试全部候选主机）：{last_err}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "上游 HTTP {}：{}",
            status.as_u16(),
            text.chars().take(200).collect::<String>()
        ));
    }
    Ok(resp)
}

/// 打开 Qoder 的流式对话：把**没读过的响应**交给网关，由 `gateway_stream` 边读边发。
///
/// 为什么不在这里聚合：聚合等于「等整段生成完再回」，首字延迟 = 整段生成时间，
/// 客户端十几分钟看不到一个字就会判定卡住、重试，再排一次队（报告 §3.1）。
pub async fn chat_open(
    region: QoderRegion,
    account: &QoderAccount,
    model_key: &str,
    display_name: &str,
    messages: &Value,
    tools: Option<&Value>,
) -> Result<reqwest::Response, String> {
    let body = full_body(model_key, display_name, messages, tools, &account.uid)?;
    post_stream(region, account, &body.to_string(), model_key, 900).await
}

pub async fn chat_once(
    region: QoderRegion,
    account: &QoderAccount,
    model_key: &str,
    display_name: &str,
    messages: &Value,
    tools: Option<&Value>,
) -> Result<Value, String> {
    let body = full_body(model_key, display_name, messages, tools, &account.uid)?;
    let resp = post_stream(region, account, &body.to_string(), model_key, 180).await?;

    // ⚠️ **必须逐块读**：上游是长连接 SSE，`resp.text()` 会等整条流结束 ⇒ 一直挂着
    // （真机：curl 90s 超时、WorkBuddy 报「模型服务响应超时」）。
    // 这里逐块累积、按行解析，看到 `finish_reason` / `[DONE]` 立即收尾。
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut prompt_tokens = 0i64;
    let mut completion_tokens = 0i64;
    let mut upstream_model = String::new();
    let mut errors: Vec<String> = Vec::new();
    let mut finished = false;
    // ⚠️ 工具调用在 `delta.tool_calls` 里（按 index 分片），以前**完全不收**：
    //    带 tools 的请求上游 2 秒内返回空正文 + 两个 tool_call 分片，我们只拿到
    //    「正文 = ""」，返回 200 ⇒ 客户端「已完成 + 一片空白」（2026-10-24 实测）。
    let mut calls: Vec<crate::modules::gateway_server::ToolCallAcc> = Vec::new();
    let mut finish: Option<String> = None;

    let handle_line = |line: &str,
                       content: &mut String,
                       reasoning: &mut String,
                       prompt_tokens: &mut i64,
                       completion_tokens: &mut i64,
                       upstream_model: &mut String,
                       errors: &mut Vec<String>,
                       finished: &mut bool,
                       calls: &mut Vec<crate::modules::gateway_server::ToolCallAcc>,
                       finish: &mut Option<String>| {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("data:") else {
            return;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            return;
        }
        if rest == "[DONE]" {
            *finished = true;
            return;
        }
        let Ok(envelope) = serde_json::from_str::<Value>(rest) else {
            return;
        };
        // ★ 信封自带状态：403 + body 里 `isQueued` 是**上游排队**（10605 CALL_COUNT_LIMIT，
        //   真机实测 dfmodel 排了 2328 人 / 64 秒）。必须**立刻收尾并说清**，
        //   否则会像以前那样傻等到超时，客户端只看到「服务超时」。
        if let Some(code) = envelope.get("statusCodeValue").and_then(Value::as_i64) {
            if code != 200 {
                *finished = true;
                let raw = envelope
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let note = if raw.contains("isQueued") || raw.contains("10605") {
                    // 排队人数与等待秒数**必须**从信封里读出来：读不到就退化成默认 30 秒，
                    // 而真实队列可能是 64 秒甚至 300 秒（两种转义形态都要认，
                    // 唯一出口 `gateway_stream::queue_wait_of`）。
                    let (wait, count) = crate::modules::gateway_stream::queue_wait_of(&raw);
                    // 约定：`QUEUED|<秒>|<可读文案>` —— 外层 chat_complete 据此等待重试。
                    format!(
                        "QUEUED|{}|上游排队中（前面约 {} 人，预计等 {} 秒）",
                        wait,
                        count.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        wait
                    )
                } else {
                    format!("上游 HTTP {code}：{}", raw.chars().take(160).collect::<String>())
                };
                errors.push(note);
                return;
            }
        }
        // 信封里的 body 是**字符串**；直接是对象时也兼容。
        let inner: Value = match envelope.get("body") {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
            Some(v @ Value::Object(_)) => v.clone(),
            _ => return,
        };
        if let Some(err) = inner
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            errors.push(err.to_string());
        }
        if upstream_model.is_empty() {
            if let Some(m) = inner.get("model").and_then(Value::as_str) {
                *upstream_model = m.to_string();
            }
        }
        if let Some(u) = inner.get("usage") {
            *prompt_tokens = u
                .get("prompt_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(*prompt_tokens);
            *completion_tokens = u
                .get("completion_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(*completion_tokens);
        }
        if let Some(choices) = inner.get("choices").and_then(Value::as_array) {
            for c in choices {
                if let Some(d) = c.get("delta") {
                    if let Some(t) = d.get("content").and_then(Value::as_str) {
                        content.push_str(t);
                    }
                    if let Some(t) = d.get("reasoning_content").and_then(Value::as_str) {
                        reasoning.push_str(t);
                    }
                    // 工具调用：OpenAI 分片形态（同一个 index 多片，名字只在第一片）。
                    if let Some(list) = d.get("tool_calls").and_then(Value::as_array) {
                        for one in list {
                            crate::modules::gateway_server::absorb_tool_call(calls, one);
                        }
                    }
                }
                if let Some(m) = c.get("message") {
                    if let Some(t) = m.get("content").and_then(Value::as_str) {
                        content.push_str(t);
                    }
                    // 非流式形态的完整调用也收（有的模型直接一次给全）。
                    if let Some(list) = m.get("tool_calls").and_then(Value::as_array) {
                        for one in list {
                            crate::modules::gateway_server::absorb_tool_call(calls, one);
                        }
                    }
                }
                if let Some(fr) = c.get("finish_reason").and_then(Value::as_str) {
                    *finish = Some(fr.to_string());
                }
                // 有 finish_reason 就说明这一轮说完了，不必等连接关闭。
                if c.get("finish_reason").map(|v| !v.is_null()).unwrap_or(false) {
                    *finished = true;
                }
            }
        }
    };

    // ⚠️ Qoder 的流**不会自己关**（agent 事件流，答完还保持连接）⇒ 只靠 finish_reason
    // 会一直等到总超时（真机：qmodel 等了 122s）。所以用「空闲超时」：
    // 已经有正文后，连续 3 秒没有新 chunk 就认为这一轮说完了。
    while let Some(item) = tokio::time::timeout(
        std::time::Duration::from_secs(
            if content.is_empty() && reasoning.is_empty() && calls.is_empty() {
                90
            } else {
                3
            },
        ),
        stream.next(),
    )
    .await
    .unwrap_or(None)
    {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                if content.is_empty() && reasoning.is_empty() && calls.is_empty() {
                    return Err(format!("读取上游流出错：{e}"));
                }
                break;
            }
        };
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            handle_line(
                &line,
                &mut content,
                &mut reasoning,
                &mut prompt_tokens,
                &mut completion_tokens,
                &mut upstream_model,
                &mut errors,
                &mut finished,
                &mut calls,
                &mut finish,
            );
        }
        if finished {
            break;
        }
    }
    // 收尾：最后一行可能没有换行符。
    if !buf.is_empty() {
        handle_line(
            &buf,
            &mut content,
            &mut reasoning,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut upstream_model,
            &mut errors,
            &mut finished,
            &mut calls,
            &mut finish,
        );
    }
    drop(stream);

    if content.is_empty() && calls.is_empty() && !errors.is_empty() {
        return Err(format!("上游报错：{}", errors.join("；")));
    }
    // 「只有思考没有正文」也要当失败，不能返回空串 —— 客户端那边「空」和「成功」
    // 长得一模一样，用户只会看到「已完成」加一片空白（2026-10-24 带文件实测）。
    if content.is_empty() && calls.is_empty() {
        if reasoning.trim().is_empty() {
            return Err("上游没有返回内容（这条流里没有可解析的 chunk）".to_string());
        }
        return Err(format!(
            "模型只返回了思考过程（{} 字），没有正文 —— 这次请求可能带了上游不认的附件或参数",
            reasoning.chars().count()
        ));
    }

    let tool_calls = crate::modules::gateway_server::finalize_tool_calls(&calls);
    let has_calls = !tool_calls.is_empty();
    let mut message = json!({ "role": "assistant", "content": content });
    if has_calls {
        // 有调用时正文给 null：空串会被部分客户端当成「空回答」直接吞掉。
        if content.trim().is_empty() {
            message["content"] = Value::Null;
        }
        message["tool_calls"] = json!(tool_calls);
    }

    Ok(json!({
        "id": format!("chatcmpl-qoder-{}", uuid::Uuid::new_v4()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": if upstream_model.is_empty() { model_key.to_string() } else { upstream_model },
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": crate::modules::gateway_server::finish_with_calls(finish, has_calls),
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    }))
}


/// 该区域的客户端是否装了（按 `install.ini` 的 installDir；不跨区兜底）。
fn client_present(region: crate::modules::qoder_realm::QoderRegion) -> bool {
    let ini = match region {
        crate::modules::qoder_realm::QoderRegion::Cn => {
            std::env::var_os("LOCALAPPDATA").map(|b| {
                std::path::PathBuf::from(b)
                    .join("Qoder CN")
                    .join("Qoder CN Launcher")
                    .join("install.ini")
            })
        }
        crate::modules::qoder_realm::QoderRegion::Global => std::env::var_os("LOCALAPPDATA").map(|b| {
            std::path::PathBuf::from(b)
                .join("Qoder")
                .join("Qoder Launcher")
                .join("install.ini")
        }),
    };
    ini.map(|p| p.is_file()).unwrap_or(false)
}

/// 首页用的 Qoder 概况（**只读**，不联网）：账号数 / 积分合计 / 会话数 / 客户端是否在跑。
pub fn home_summary() -> Value {
    use crate::modules::qoder_realm::QoderRegion;
    let mut regions = Vec::new();
    let mut total_accounts = 0usize;
    let mut total_credits = 0f64;
    let mut credits_known = 0usize;
    for region in [QoderRegion::Cn, QoderRegion::Global] {
        let accounts = crate::modules::qoder_accounts::list(region).unwrap_or_default();
        let usable = accounts.iter().filter(|a| a.is_usable()).count();
        let credits: f64 = accounts.iter().filter_map(|a| a.credits).sum();
        let known = accounts.iter().filter(|a| a.credits.is_some()).count();
        total_accounts += accounts.len();
        total_credits += credits;
        credits_known += known;
        regions.push(json!({
            "region": region.as_str(),
            "label": region.display_name(),
            "accounts": accounts.len(),
            "usable": usable,
            "credits": if known > 0 { json!(credits) } else { Value::Null },
            "hasClient": client_present(region),
        }));
    }
    let sessions = crate::modules::qoder_sessions::counts();
    let running = crate::modules::process_list::find_by_names(&[
        "Qoder.exe".to_string(),
        "Qoder CN.exe".to_string(),
        "Qoder Launcher.exe".to_string(),
        "Qoder CN Launcher.exe".to_string(),
    ])
    .len();
    json!({
        "accounts": total_accounts,
        "credits": if credits_known > 0 { json!(total_credits) } else { Value::Null },
        "regions": regions,
        "sessions": sessions,
        "running": running > 0,
        "processCount": running,
    })
}

#[cfg(test)]
mod tests {
    /// 把两个区域的模型清单**合并写进** `~/.twin-switch/models.json`（保留其他作用域）。
    #[tokio::test]
    #[ignore]
    async fn sync_models_json() {
        let path = crate::modules::config::store_dir().join("models.json");
        let mut map: std::collections::BTreeMap<String, Vec<String>> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        for region in [QoderRegion::Cn, QoderRegion::Global] {
            let list = crate::modules::qoder_accounts::list(region).unwrap_or_default();
            let Some(acc) = list.iter().find(|a| a.is_usable()) else { continue };
            match super::fetch_models(region, acc).await {
                Ok(models) => {
                    let keys: Vec<String> = models.into_iter().map(|(k, _)| k).collect();
                    eprintln!("[{}] {} 个：{:?}", region.as_str(), keys.len(), keys);
                    map.insert(format!("qoder-{}", region.as_str()), keys);
                }
                Err(e) => eprintln!("[{}] 失败：{e}", region.as_str()),
            }
        }
        if let Ok(text) = serde_json::to_string_pretty(&map) {
            let _ = std::fs::write(&path, text);
            eprintln!("已写 {}", path.display());
        }
    }

    /// 真机探测：国际版签到（验证机器头是否让服务端下发 CLAIM_BENEFIT）。
    #[tokio::test]
    #[ignore]
    async fn probe_global_checkin() {
        let region = QoderRegion::Global;
        let list = crate::modules::qoder_accounts::list(region).unwrap_or_default();
        let Some(acc) = list.iter().find(|a| a.is_usable()) else {
            eprintln!("国际版没有可用账号");
            return;
        };
        eprintln!("机器身份就绪: {}", crate::modules::qoder_machine::status()["ready"]);
        match super::checkin_claim(region, acc).await {
            Ok(v) => eprintln!("签到结果: {}", serde_json::to_string(&v).unwrap_or_default()),
            Err(e) => eprintln!("失败: {e}"),
        }
    }

    /// 真机探测：完整跑一次对话并聚合出正文。
    #[tokio::test]
    #[ignore]
    async fn probe_chat_complete() {
        let region = QoderRegion::Cn;
        let list = crate::modules::qoder_accounts::list(region).unwrap_or_default();
        let Some(acc) = list.iter().find(|a| a.is_usable()) else {
            eprintln!("没有可用账号");
            return;
        };
        let msgs = serde_json::json!([{ "role": "user", "content": "只回复两个字母：ok" }]);
        for (key, display) in [("dfmodel", "DeepSeek"), ("qmodel", "Qwen")] {
            match super::chat_complete(region, acc, key, display, &msgs).await {
                Ok(v) => eprintln!(
                    "[{key}] 正文={:?} | model={} | usage={}",
                    v.pointer("/choices/0/message/content").and_then(Value::as_str).unwrap_or(""),
                    v.get("model").and_then(Value::as_str).unwrap_or("?"),
                    v.get("usage").cloned().unwrap_or_default()
                ),
                Err(e) => eprintln!("[{key}] 失败：{e}"),
            }
        }
    }

    /// 真机探测：发一次最小对话，看能不能拿到信封响应（会消耗极少量额度）。
    #[tokio::test]
    #[ignore]
    async fn probe_chat_once() {
        let region = QoderRegion::Cn;
        let list = crate::modules::qoder_accounts::list(region).unwrap_or_default();
        let Some(acc) = list.iter().find(|a| a.is_usable()) else {
            eprintln!("没有可用账号");
            return;
        };
        for (key, display) in [("dfmodel", "DeepSeek-F"), ("qmodel", "Qwen")] {
            match super::chat_probe(region, acc, key, display, "回复 ok 两个字母", 900).await {
                Ok(s) => eprintln!("[{key}] {s}"),
                Err(e) => eprintln!("[{key}] 失败：{e}"),
            }
        }
    }

    /// 真机探测：用本机账号拉一次模型清单（只读，不发对话）。
    #[tokio::test]
    #[ignore]
    async fn probe_model_list() {
        for region in [QoderRegion::Cn, QoderRegion::Global] {
            let list = crate::modules::qoder_accounts::list(region).unwrap_or_default();
            let Some(acc) = list.iter().find(|a| a.is_usable()) else {
                eprintln!("[{:?}] 没有可用账号", region.as_str());
                continue;
            };
            match super::fetch_models(region, acc).await {
                Ok(models) => {
                    eprintln!("[{}] 拉到 {} 个模型：{:?}", region.as_str(), models.len(),
                        models.iter().take(8).map(|(k, _)| k.as_str()).collect::<Vec<_>>());
                }
                Err(e) => eprintln!("[{}] 失败：{e}", region.as_str()),
            }
        }
    }

    use super::*;

    #[test]
    fn domain_must_match_the_realm() {
        assert!(domain_matches("qoder.com.cn", "qoder.com.cn"));
        assert!(domain_matches("QODER.COM.CN", "qoder.com.cn"));
        assert!(!domain_matches("qoder.com", "qoder.com.cn"));
        assert!(!domain_matches("", "qoder.com.cn"), "空域名必须 fail-closed");
    }

    #[test]
    fn preview_never_returns_tokens() {
        let text = r#"[{"uid":"u1","name":"甲","domain":"qoder.com.cn","accessToken":"SECRET-TOKEN"}]"#;
        let v = import_preview(QoderRegion::Cn, text).unwrap();
        let s = v.to_string();
        assert!(!s.contains("SECRET-TOKEN"), "预览绝不能回令牌明文");
        assert_eq!(v["items"][0]["hasToken"], json!(true));
    }

    #[test]
    fn cross_region_items_are_rejected_not_skipped() {
        let text = r#"[{"uid":"u1","name":"甲","domain":"qoder.com","accessToken":"t"}]"#;
        let v = import_preview(QoderRegion::Cn, text).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
        assert_eq!(v["rejected"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn empty_array_is_an_error_not_zero_result() {
        assert!(parse_items("[]").is_err());
        assert!(parse_items("{}").is_err());
        assert!(parse_items("nope").is_err());
    }
}
