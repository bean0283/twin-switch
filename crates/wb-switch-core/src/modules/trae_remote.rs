//! 云端会话操作（移植自 trae-switch src/sessions.js + traeapi.js）。
//!
//! 用账号凭证访问 remote API（`{remote_host}/api/remote/v1/*`），当前用途：
//! 删除本地会话时**同步删除云端任务列表记录**。
//!
//! 凭证材料来源（按优先级）：
//!   1. oauth 档案（`vault/<client>/<id>/oauth.json`，网页登录落库，字段自包含）；
//!   2. carrier 载体档案 / 当前 live 登录态（`storage.json` 的
//!      `iCubeAuthInfo://icube.cloudide` 登录态 + `iCubeAuthInfo://icube-dc:` 设备密钥）。
//!
//! token 过期时用 refreshToken 走 ExchangeToken 设备签名握手续期
//! （签名串与客户端 `_Te()` 一致：METHOD/PATH/ClientID/RefreshToken/Timestamp/Nonce）。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use p256::pkcs8::DecodePrivateKey;
use serde_json::{json, Value};

use crate::modules::config::{http_request_raw, now_ms};
use crate::modules::trae_discover::{get_client, storage_json_path, TraeClient};
use crate::modules::trae_km::{decrypt_km_json, is_km_value, jwt_exp, jwt_user_id};
use crate::modules::trae_oauth::{
    read_oauth_account, vault_uid_index, OAUTH_APP_VERSION, OAUTH_EXCHANGE_PATH,
};
use crate::modules::trae_vault::{account_dir, KEY_AUTH, REL_STORAGE};

/// storage.json 里的设备密钥前缀（`iCubeAuthInfo://icube-dc:<deviceId>`）。
const PREFIX_DC: &str = "iCubeAuthInfo://icube-dc:";
/// remote API 请求头用户区域。
const USER_REGION: &str = "cn";
/// ExchangeToken 刷新的默认账号域（traeapi.js 同款）。
const EXCHANGE_DEFAULT_HOST: &str = "https://api.trae.cn";
/// 续期门槛（天）：access token 剩余**大于**这个天数时，默认**不刷新** ——
/// 不消耗 refreshToken 的轮换代次（服务端只保留最近两代，反复刷会把前几代提前耗掉）。
/// 与参考实现 `trae_refresh.js` 的 `REFRESH_THRESHOLD_DAYS` 同一个数。
pub const REFRESH_THRESHOLD_DAYS: i64 = 7;
/// jwt / 刷新态里 token 字段的可能键名（服务端历次字段名不统一）。
const TOKEN_KEYS: [&str; 6] = ["token", "Token", "accessToken", "access_token", "Jwt", "JWT"];
const REFRESH_TOKEN_KEYS: [&str; 3] = ["refreshToken", "RefreshToken", "refresh_token"];

/// 续期时**最多**尝试多少种组合（凭据 × 设备身份 × 客户端参数）。
///
/// 参考实现 `trae_refresh.js` 的上限是 6（2 份凭据 × 3 套参数），这里取同一量级再留一点余量：
/// 组合数在账号多的机器上能到几十种，逐个试既慢，又会在服务端留下成片失败记录。
const MAX_REFRESH_ATTEMPTS: usize = 8;

/// 客户端参数（取自 product.json 的 iCubeApp.authConfig，与 traeapi.js 一致）。
/// 两套官方客户端参数：`(标签, ClientID, PlatformCode)`。
///
/// ⚠️ 服务端按**登录时使用的 clientId** 给 refreshToken 做绑定，与账号现在放在哪个客户端
///    目录下无关。混用实测后果（2026-10-12 参考实现 `trae_refresh.js` 复核）：
///    · 用 A 客户端密钥配 B 客户端 ClientID → `401 20403 Token device not match`；
///    · 同一把设备密钥但 clientId 用错 → `400 10101` 无效参数。
const SOLO_PROFILE: (&str, &str, &str) = ("SOLO", "en1oxy7wnw8j9n", "SOLO_PC");
const TRAE_PROFILE: (&str, &str, &str) = ("TRAE", "ono9krqynydwx5", "IDE_PC");

/// 按客户端标识选参数：`solo` 线 → SOLO，其余 → TRAE。
fn profile_of(client_key: &str) -> (&'static str, &'static str, &'static str) {
    if client_key.contains("solo") {
        SOLO_PROFILE
    } else {
        TRAE_PROFILE
    }
}

/// 另一条线：`trae*` ↔ `solo*`。
fn sibling_client_key(key: &str) -> &'static str {
    if key.contains("solo") {
        "trae-cn"
    } else {
        "solo-cn"
    }
}

/// 一套「可尝试的设备身份」。
///
/// ⚠️ 为什么要有多套：服务端把 refreshToken 绑在**登录那一刻的设备公钥**上，而客户端的
///    `icube-dc` 设备密钥**会变**（本机实测：Trae CN 的 icube-dc 公钥从 `098a7bc8…` 换成了
///    `20358a25…`）。于是同一个 RT，用「现在的密钥」去刷就是
///    `401 20403 Token device not match`，用「当年那把」才对。
///    账号档里存的是**登录时那把**（正确），但一并带入 / 重装 / 历史档混在一起时，
///    光看一个来源会挑错 ⇒ 这里把所有「这台机器上出现过的设备身份」都列出来逐个试。
#[derive(Clone)]
struct DeviceCandidate {
    /// 只为日志与错误信息，不参与取值。
    label: String,
    device_id: String,
    machine_id: String,
    private_key_pem: String,
    public_key_pem: String,
}

/// 公钥指纹（前 12 位 hex），用于给设备身份去重。
fn key_fingerprint(pem: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(pem.trim().as_bytes());
    let hex = format!("{d:x}");
    hex[..12.min(hex.len())].to_string()
}

/// 把一套设备身份并入候选表：缺 `deviceId` / 私钥的丢掉；公钥指纹重复的也丢掉。
///
/// 独立成函数是为了能单测（去重与「账号档排第一」是续期能不能命中的关键）。
fn push_device(
    out: &mut Vec<DeviceCandidate>,
    seen: &mut Vec<String>,
    label: &str,
    device_id: &str,
    machine_id: &str,
    private_key_pem: &str,
    public_key_pem: &str,
) {
    if device_id.is_empty() || private_key_pem.is_empty() {
        return;
    }
    let fp = key_fingerprint(public_key_pem);
    if seen.contains(&fp) {
        return;
    }
    seen.push(fp);
    out.push(DeviceCandidate {
        label: label.to_string(),
        device_id: device_id.to_string(),
        machine_id: machine_id.to_string(),
        private_key_pem: private_key_pem.to_string(),
        public_key_pem: public_key_pem.to_string(),
    });
}

/// 短指纹（sha256 前 12 位 hex）—— 只用于**判等**，绝不打印凭据原文。
fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(s.as_bytes()))[..12].to_string()
}

/// 收集可尝试的设备身份（按公钥指纹去重，账号档自己那套排第一）。
///
/// ⚠️ 只收**本账号 + 本机客户端**这两类，**绝不遍历别的账号档里的历史设备**：
///    续期全程必须锁定同一个账号（参考实现 `trae_refresh.js` 的硬规则）。
fn device_candidates(m: &CloudMaterial, client_key: &str) -> Vec<DeviceCandidate> {
    let mut out: Vec<DeviceCandidate> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    // 1) 账号档自己那套 —— 绝大多数情况这一套就是对的。
    push_device(
        &mut out,
        &mut seen,
        "账号档",
        &m.device_id,
        &m.machine_id,
        &m.private_key_pem,
        &m.public_key_pem,
    );

    // 2) 本机客户端现在的设备（登录客户端优先，再账号所在目录、再另一条线）。
    for key in unique_client_keys(&[&m.own_client, client_key, sibling_client_key(client_key)]) {
        if let Ok(dev) = crate::modules::trae_oauth::resolve_oauth_device(Some(key)) {
            // ⚠️ `resolve_oauth_device` 在全机都没有 icube-dc 时会**自建**一套设备身份兜底；
            //    那套跟服务端绑定的 RT 不可能对得上（它不是当年登录用的），试了只会白白多一次
            //    失败请求 ⇒ 按 `source` 把它挡掉。
            if dev.source.starts_with("自建") {
                continue;
            }
            let (Some(priv_pem), Some(pub_pem)) = (dev.private_key_pem.as_deref(), dev.public_key_pem.as_deref()) else {
                continue;
            };
            push_device(
                &mut out,
                &mut seen,
                &format!("本机 {key}"),
                &dev.device_id,
                &dev.machine_id,
                priv_pem,
                pub_pem,
            );
        }
    }
    out
}

/// 去重且保持顺序的客户端标识表（跳过空串）。
fn unique_client_keys(keys: &[&str]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for k in keys {
        if k.is_empty() {
            continue;
        }
        let owned: &'static str = match *k {
            "trae-cn" => "trae-cn",
            "solo-cn" => "solo-cn",
            "trae-intl" => "trae-intl",
            "solo-intl" => "solo-intl",
            _ => continue,
        };
        if !out.contains(&owned) {
            out.push(owned);
        }
    }
    out
}

/// 一份「凭据 + 它自带的设备身份」。
///
/// ⚠️ 这几样必须**同源**：refreshToken、设备密钥对、ClientID 是同一时刻、同一客户端一起写下的。
///    这正是参考实现 `trae_refresh.js` 的模型 —— 它把每个客户端当独立候选，密钥固定取该目录的
///    `icube-dc`，两套 ClientID 轮着试，第一轮用**实时登录态**的 RT、第二轮才用备份档的 RT。
#[derive(Clone)]
struct CredentialSet {
    /// 只为日志与错误信息。
    label: String,
    access_token: String,
    refresh_token: String,
    device: DeviceCandidate,
    /// 这份凭据**登录时用的客户端** —— 决定 ClientID 候选顺序（服务端按它绑定 RT）。
    own_client: String,
    /// 上次刷新成功的那套 ClientID（`oauth.json` 的 `profileClient`）。
    remembered: Option<String>,
}

fn push_credential(
    out: &mut Vec<CredentialSet>,
    seen: &mut Vec<String>,
    label: &str,
    at: &str,
    rt: &str,
    dev: DeviceCandidate,
    own_client: &str,
    remembered: Option<String>,
) {
    if at.is_empty() || rt.is_empty() || dev.private_key_pem.is_empty() || dev.device_id.is_empty() {
        return;
    }
    let k = format!("{}|{}|{}", short_hash(rt), key_fingerprint(&dev.public_key_pem), own_client);
    if seen.contains(&k) {
        return;
    }
    seen.push(k);
    out.push(CredentialSet {
        label: label.to_string(),
        access_token: at.to_string(),
        refresh_token: rt.to_string(),
        device: dev,
        own_client: own_client.to_string(),
        remembered,
    });
}

/// 收集可尝试的凭据（**全部属于同一个 uid**，绝不跨账号）。
///
/// 顺序：账号档 → 客户端**实时登录态** → 账号自己的载体登录态。
///
/// ⚠️ 中间这一档是参考实现的**第一轮**，以前整个漏掉了：账号档里的 RT 可能是很久以前备份的
///    （或者客户端退出时把自己内存里的旧 token 写回 `storage.json`、而账号档更新），这时只有
///    实时登录态那把 RT 刷得动；拿旧 RT 去刷，服务端回的就是
///    `20403 Token device not match`（旧 RT 早被换代，绑定的设备也随之不同）。
fn credential_sets(
    m: &CloudMaterial,
    client_key: &str,
    uid: &str,
    account_id: &str,
) -> Vec<CredentialSet> {
    let mut out: Vec<CredentialSet> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let device_of = |src: &CloudMaterial| DeviceCandidate {
        label: String::new(), // 由调用处填
        device_id: src.device_id.clone(),
        machine_id: src.machine_id.clone(),
        private_key_pem: src.private_key_pem.clone(),
        public_key_pem: src.public_key_pem.clone(),
    };

    // 1) 账号档自己那份（oauth.json 优先，`account_material` 已经挑好了）。
    let mut d = device_of(m);
    d.label = "账号档".into();
    push_credential(
        &mut out,
        &mut seen,
        "账号档",
        &m.access_token,
        &m.refresh_token,
        d,
        &m.own_client,
        m.remembered_profile.clone(),
    );

    // 2) 客户端实时登录态 —— `material_from_storage` 内部会比对 uid，**不同账号直接返回 None**，
    //    这就守住了「绝不借用别的账号的凭据」。
    for key in unique_client_keys(&[&m.own_client, client_key, sibling_client_key(client_key)]) {
        let Some(client) = get_client(key) else { continue };
        let Ok(text) = std::fs::read_to_string(storage_json_path(client)) else {
            continue;
        };
        let Some(src) = material_from_storage(key, &text, uid) else {
            continue;
        };
        let mut d = device_of(&src);
        d.label = format!("实时登录态 {key}");
        push_credential(
            &mut out,
            &mut seen,
            &format!("实时登录态 {key}"),
            &src.access_token,
            &src.refresh_token,
            d,
            key,
            None,
        );
    }

    // 3) 账号自己的载体登录态（`export_account` 搬过来的那份 storage.json）。
    if let Ok(text) = std::fs::read_to_string(account_dir(client_key, account_id).join(REL_STORAGE)) {
        if let Some(src) = material_from_storage(client_key, &text, uid) {
            let mut d = device_of(&src);
            d.label = "载体登录态".into();
            push_credential(
                &mut out,
                &mut seen,
                "载体登录态",
                &src.access_token,
                &src.refresh_token,
                d,
                &src.own_client,
                None,
            );
        }
    }

    out
}


/// 续期时**按顺序**要试的参数表（去重按 ClientID）。
///
/// 顺序 = 上次成功的（写在 `oauth.json` 的 `profileClientId`）→ 账号自己 `client` 字段那套
/// → 另一套。第一候选之所以不是「账号所在的客户端目录」，是因为「一并带入」搬过来的账号
/// `client` 字段记的才是**登录时那个客户端**：`trae-cn/vault/<id>/oauth.json` 里 `client`
/// 写着 `solo-cn` 的账号，用 `IDE_PC` 刷必然 20403（线上实测）。
fn profile_candidates(
    client_key: &str,
    own_client: &str,
    remembered: Option<&str>,
) -> Vec<(&'static str, &'static str, &'static str)> {
    // 三个来源，按可信度排：登录时那个客户端 → 账号所在的客户端目录。
    // ⚠️ `own_client` 为空（老档没写 `client` 字段）时才回落到目录名 —— 反过来就错：
    //    目录名记的是「这把账号现在放在哪」，而服务端绑 RT 用的是「登录时用的 clientId」。
    let own = if own_client.is_empty() {
        profile_of(client_key)
    } else {
        profile_of(own_client)
    };
    let first = remembered.map(profile_of).unwrap_or(own);
    let mut out: Vec<(&'static str, &'static str, &'static str)> = Vec::new();
    for p in [first, own, profile_of(client_key), TRAE_PROFILE, SOLO_PROFILE] {
        if !out.iter().any(|q| q.1 == p.1) {
            out.push(p);
        }
    }
    out
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 云端调用所需凭证材料。
#[derive(Clone)]
pub struct CloudMaterial {
    pub access_token: String,
    pub refresh_token: String,
    pub device_id: String,
    pub machine_id: String,
    pub private_key_pem: String,
    pub public_key_pem: String,
    pub app_version: String,
    pub profile: &'static str,
    pub client_id: &'static str,
    pub platform_code: &'static str,
    /// 这份凭据**登录时用的客户端**（`oauth.json` 的 `client` 字段）。
    /// 它决定该用哪套 ClientID；与账号现在位于哪个客户端目录无关。
    pub own_client: String,
    /// 上次刷新成功的那套 ClientID（`oauth.json` 的 `profileClientId`）——排第一候选，省一轮白试。
    pub remembered_profile: Option<String>,
    /// ExchangeToken 刷新的账号域（api.trae.cn）。
    pub exchange_host: String,
    /// remote API 域（trae-api-cn.mchost.guru）。
    pub remote_host: String,
}

fn remote_host_of(client: &TraeClient) -> String {
    client.remote_host.trim_end_matches('/').to_string()
}

/// 本机该客户端安装目录 `product.json` 的 `appVersion`（读不到返回 `None`）。
fn client_app_version(client_key: &str) -> Option<String> {
    get_client(client_key).and_then(crate::modules::trae_discover::app_version)
}

/// 从 storage.json 文本提取 uid 对应的云端材料（carrier 副本或 live 本体）。
fn material_from_storage(client_key: &str, text: &str, uid: &str) -> Option<CloudMaterial> {
    let obj: Value = serde_json::from_str(text).ok()?;
    let auth_raw = obj.get(KEY_AUTH)?.as_str()?;
    if !is_km_value(auth_raw) {
        return None;
    }
    let auth = decrypt_km_json(auth_raw)?;
    let auth_uid = auth
        .get("uid")
        .and_then(|v| v.as_str().map(String::from))
        .or_else(|| {
            find_value(&auth, &["token", "Token", "accessToken", "access_token", "Jwt", "JWT"])
                .and_then(|t| jwt_user_id(&t))
        })?;
    if auth_uid != uid {
        return None;
    }
    let token = find_value(&auth, &["token", "Token", "accessToken", "access_token", "Jwt", "JWT"])?;
    let refresh = find_value(&auth, &["refreshToken", "RefreshToken", "refresh_token"])?;
    let dc_key = obj.as_object()?.keys().find(|k| k.starts_with(PREFIX_DC))?.clone();
    let dc_raw = obj.get(&dc_key)?.as_str()?;
    if !is_km_value(dc_raw) {
        return None;
    }
    let dc = decrypt_km_json(dc_raw)?;
    let private_key_pem = dc.get("privateKeyPEM").and_then(|v| v.as_str())?.to_string();
    let public_key_pem = dc.get("publicKeyPEM").and_then(|v| v.as_str())?.to_string();
    let device_id = dc_key.strip_prefix(PREFIX_DC)?.to_string();
    if device_id.is_empty() {
        return None;
    }
    let machine_id = obj
        .get("telemetry.machineId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // ⚠️ 发给服务端的 `ClientVersion` / `IDEVersion` 要取**安装目录 product.json 的 `appVersion`**
    //    （客户端自己也是这么发的）。`storage.json` 的 `iCubeLastVersion`（2.3.91xxx）是
    //    tronBuildVersion，拿它顶替会让服务端的设备校验对不上 —— 参考实现实测踩过这个坑。
    let app_version = client_app_version(client_key).unwrap_or_else(|| {
        obj.get("iCubeLastVersion")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| OAUTH_APP_VERSION.to_string())
    });
    let (profile, client_id, platform_code) = profile_of(client_key);
    let client = get_client(client_key)?;
    Some(CloudMaterial {
        access_token: token,
        refresh_token: refresh,
        device_id,
        machine_id,
        private_key_pem,
        public_key_pem,
        app_version,
        profile,
        client_id,
        platform_code,
        // 载体登录态是从**这台机器上这个客户端**的 storage.json 读出来的，
        // 登录客户端就是它自己 —— 没有歧义，也不需要「上次成功」。
        own_client: client_key.to_string(),
        remembered_profile: None,
        exchange_host: EXCHANGE_DEFAULT_HOST.to_string(),
        remote_host: remote_host_of(client),
    })
}

/// 在 JSON 树里找首个非空字符串值（按键名顺序，递归兜底）。
fn find_value(v: &Value, keys: &[&str]) -> Option<String> {
    if v.is_null() {
        return None;
    }
    if let Some(arr) = v.as_array() {
        for x in arr {
            if let Some(hit) = find_value(x, keys) {
                return Some(hit);
            }
        }
        return None;
    }
    if let Some(obj) = v.as_object() {
        for k in keys {
            if let Some(val) = obj.get(*k) {
                if let Some(s) = val.as_str() {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
        for val in obj.values() {
            if let Some(hit) = find_value(val, keys) {
                return Some(hit);
            }
        }
    }
    None
}

/// 从某账号的 `oauth.json`（网页凭证，字段自包含）组装云端材料。
fn material_from_oauth(client_key: &str, id: &str) -> Option<CloudMaterial> {
    let oa = read_oauth_account(client_key, id)?;
    let token = find_value(&oa, &TOKEN_KEYS)?;
    let refresh = find_value(&oa, &REFRESH_TOKEN_KEYS)?;
    let device_id = find_value(&oa, &["deviceId", "DeviceID", "device_id"])?;
    if token.is_empty() || refresh.is_empty() || device_id.is_empty() {
        return None;
    }
    let client = get_client(client_key)?;
    // ⚠️ 第一候选按**账号自己的 `client` 字段**（= 登录时那个客户端），不是按它现在所在的
    //    目录 —— 「一并带入」搬过来的账号两者经常不一样（见 `profile_candidates`）。
    let own_client = find_value(&oa, &["client", "clientKey", "client_key"])
        .unwrap_or_else(|| client_key.to_string());
    let (profile, cid, pc) = profile_of(&own_client);
    Some(CloudMaterial {
        access_token: token,
        refresh_token: refresh,
        device_id,
        machine_id: find_value(&oa, &["machineId", "machine_id"]).unwrap_or_default(),
        private_key_pem: find_value(&oa, &["privateKeyPEM", "private_key_pem"])?,
        public_key_pem: find_value(&oa, &["publicKeyPEM", "public_key_pem"]).unwrap_or_default(),
        // 同上：优先本机安装目录的 `appVersion`；账号档里那份可能很旧（甚至是并入前的 SOLO 版本）。
        app_version: client_app_version(client_key)
            .or_else(|| find_value(&oa, &["appVersion", "app_version"]))
            .unwrap_or_else(|| OAUTH_APP_VERSION.to_string()),
        profile,
        client_id: cid,
        platform_code: pc,
        own_client,
        remembered_profile: find_value(&oa, &["profileClient"]),
        exchange_host: find_value(&oa, &["host"]).unwrap_or_else(|| EXCHANGE_DEFAULT_HOST.to_string()),
        remote_host: remote_host_of(client),
    })
}

/// 解析 uid → 云端材料。
/// oauth 档案优先（凭证自包含）；其次 carrier 副本 / live 登录态（storage.json）。
pub fn cloud_material_for(client_key: &str, uid: &str) -> Option<CloudMaterial> {
    let client = get_client(client_key)?;
    // 1) oauth 档案
    if let Some((id, _)) = vault_uid_index(client_key).get(uid).cloned() {
        if let Some(m) = material_from_oauth(client_key, &id) {
            return Some(m);
        }
    }
    // 2) live 登录态（storage.json 本体）
    if let Ok(text) = std::fs::read_to_string(storage_json_path(client)) {
        if let Some(m) = material_from_storage(client_key, &text, uid) {
            return Some(m);
        }
    }
    // 3) carrier 载体档案（storage.json 副本）
    for id in crate::modules::trae_vault::list_vault_accounts(client_key) {
        let sp = account_dir(client_key, &id).join(REL_STORAGE);
        if let Ok(text) = std::fs::read_to_string(&sp) {
            if let Some(m) = material_from_storage(client_key, &text, uid) {
                return Some(m);
            }
        }
    }
    None
}

/// 按**账号目录 id** 取云端材料 —— 界面「看 token / 续期」用（那里手上只有账号 id，没有 uid）。
///
/// 顺序就是账号自己的两份档：`oauth.json`（网页凭证）→ 自己的载体 `storage.json`。
/// 返回 `(材料, 来源)`，来源用于界面如实说明这份 token 是从哪儿读出来的。
pub fn account_material(client_key: &str, account_id: &str) -> Option<(CloudMaterial, &'static str)> {
    if let Some(m) = material_from_oauth(client_key, account_id) {
        return Some((m, "oauth"));
    }
    let uid = crate::modules::trae_vault::uid_of_account(client_key, account_id)?;
    let sp = account_dir(client_key, account_id).join(REL_STORAGE);
    let text = std::fs::read_to_string(&sp).ok()?;
    material_from_storage(client_key, &text, &uid).map(|m| (m, "carrier"))
}

/// 还剩多少天（**入参是毫秒**）—— 实现只有 `config::days_left_ms` 一份。
///
/// ⚠️ 曾经这个函数叫 `days_left(exp_secs)`，而调用方递进来的却是 `jwt_exp()` 的毫秒值 ⇒
///    算出来的天数大了 1000 倍（界面「还剩 20723318 天」）。
fn days_left_ms(exp_ms: i64) -> i64 {
    crate::modules::config::days_left_ms(exp_ms)
}

/// 设备签名（P-256 ECDSA-SHA256，DER 编码 base64）—— 客户端 `_Te()` 同款。
///
/// ⚠️⚠️ 这里必须用 `sign_prehash`（对**给定的摘要**签名）。
///     用 `Signer::sign(&digest)` 会被当成「消息」再 SHA-256 一次 = **双重哈希** ⇒
///     服务端验签失败，而它回报的错是 `401 20403 Token device not match` —— 看着像
///     「设备换了 / RT 绑在别的机器上」，于是会去换设备密钥、换客户端参数，全都白试。
///     线上实测：同一把 RT + 同一套参数，参考实现 `trae_refresh.js`（`crypto.sign('sha256', …)`）
///     能刷成功，这里就是刷不动。
fn sign_ecdsa_sha256(pem: &str, msg: &str) -> Result<String, String> {
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    use p256::ecdsa::SigningKey;
    use sha2::Digest;
    let sk = SigningKey::from_pkcs8_pem(pem).map_err(|e| format!("解析设备私钥失败: {e}"))?;
    let digest = sha2::Sha256::digest(msg.as_bytes());
    let sig: p256::ecdsa::Signature = sk
        .sign_prehash(digest.as_slice())
        .map_err(|e| format!("设备私钥签名失败: {e}"))?;
    Ok(STANDARD.encode(sig.to_der().as_bytes()))
}

/// 解析 ExchangeToken 刷新响应的 token 字段。
fn parse_refresh_body(body: &Value) -> Option<String> {
    find_value(body, &["Token", "token", "AccessToken", "accessToken"])
        .filter(|s| !s.is_empty())
}

/// 解析刷新响应里的新 refreshToken（服务端有时不返回，此时沿用旧的那一份）。
fn parse_refresh_token(body: &Value) -> Option<String> {
    find_value(body, &REFRESH_TOKEN_KEYS).filter(|s| !s.is_empty())
}

/// 解析刷新响应里的「刷新额度到期」（refreshExpiredAt，秒或毫秒）。
fn parse_refresh_exp(body: &Value) -> Option<i64> {
    let v = find_value(body, &["RefreshExpireAt", "refreshExpireAt", "RefreshExpiredAt", "refreshExpiredAt"])?;
    if let Ok(n) = v.parse::<i64>() {
        return Some(if n > 1_000_000_000_000 { n } else { n * 1000 });
    }
    chrono::DateTime::parse_from_rfc3339(&v).ok().map(|d| d.timestamp_millis())
}

/// 走一次设备签名握手（ExchangeToken）。成功返回响应体，失败返回带 code/message 的原因。
///
/// 与客户端 `_Te()` 同款：签名串 = `METHOD\nPATH\nClientID\nRefreshToken\nTimestamp\nNonce`，
/// P-256 ECDSA-SHA256（DER）。
async fn exchange_token(m: &CloudMaterial) -> Result<Value, String> {
    exchange_token_once(m).await.map_err(|(_, e)| e)
}

/// 一次 ExchangeToken 尝试，**把两类失败分开**（续期时处置完全不同）：
///
/// * `Err((false, e))` = 服务端答复了但拒绝（`400 10101` 参数不被接受 / `401 20403` 设备不匹配）
///   ⇒ 换一套「设备身份 / 客户端参数」值得再试；
/// * `Err((true, e))` = **传输层失败**（`http_request_raw` 的 `status == 0`，请求没发出去或
///   没收到响应）⇒ 换凭据毫无意义，只能退避后原地重发，或直接如实报「网络不通」。
///
/// ⚠️ 以前 `status == 0` 时原因被吞成「未知错误」——本机实测那句真正的
///    `Access is denied. (os error 5)` 就是这么消失的，界面上只剩一句没用的「未知错误」。
async fn exchange_token_once(m: &CloudMaterial) -> Result<Value, (bool, String)> {
    let base = m.exchange_host.trim_end_matches('/');
    let url = format!("{base}{OAUTH_EXCHANGE_PATH}");
    let ts = now_secs();
    let nonce = crate::modules::trae_oauth::random_hex(32);
    let to_sign = format!(
        "POST\n{}\n{}\n{}\n{}\n{}",
        OAUTH_EXCHANGE_PATH, m.client_id, m.refresh_token, ts, nonce
    );
    let signature = match sign_ecdsa_sha256(&m.private_key_pem, &to_sign) {
        Ok(s) => s,
        // 私钥本身有问题也归到「换一套身份再试」那一类，不算网络失败。
        Err(e) => return Err((false, e)),
    };
    let payload = json!({
        "ClientID": m.client_id,
        "ClientSecret": "",
        "RefreshToken": m.refresh_token,
        "DeviceInfo": {
            "DeviceID": m.device_id,
            "MachineID": if m.machine_id.is_empty() { crate::modules::trae_oauth::random_hex(32) } else { m.machine_id.clone() },
            "PlatformCode": m.platform_code,
            "DeviceType": "PC",
            "DeviceName": std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows-PC".into()),
            "DeviceModel": "",
            "ClientVersion": m.app_version,
            "DevicePublicKey": m.public_key_pem.clone(),
            "DeviceBrand": "",
            "DeviceCPU": "",
            "OSInfo": "",
            "OSVersion": "",
        },
        "DeviceProof": { "Signature": signature, "Timestamp": ts, "Nonce": nonce },
        "IDEVersion": m.app_version,
    });
    let mut headers = HashMap::new();
    headers.insert("x-cloudide-token".into(), m.access_token.clone());
    headers.insert("User-Agent".into(), "twin-switch/1.0".into());
    let (status, _, body_text) = http_request_raw(&url, "POST", Some(payload), Some(&headers), None, true).await;
    classify_exchange_response(status, &body_text)
}

/// 把一次 HTTP 往返的结果翻成 `Result<Value, (是否传输层失败, 原因)>`。
///
/// 抽成纯函数是为了能单测：**`status == 0` 时原因必须原样带出来**，不许再吞成「未知错误」。
fn classify_exchange_response(status: u16, body_text: &str) -> Result<Value, (bool, String)> {
    if status >= 200 && status < 300 {
        return Ok(serde_json::from_str(body_text).unwrap_or(Value::Null));
    }
    if status == 0 {
        // `http_request_raw` 在传输层失败时把原因放进 body 位置（不是 JSON），
        // 例如本机那句 `Access is denied. (os error 5)`。
        let why = body_text.trim();
        return Err((
            true,
            if why.is_empty() {
                "连接被中断（没有拿到失败原因文本）".to_string()
            } else {
                why.to_string()
            },
        ));
    }
    let body: Value = serde_json::from_str(body_text).unwrap_or(Value::Null);
    let msg = body
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("Message").and_then(|v| v.as_str()))
        .or_else(|| {
            body.get("ResponseMetadata")
                .and_then(|m| m.get("Error"))
                .and_then(|e| e.get("Message"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("未知错误");
    let code = body
        .get("code")
        .or_else(|| body.get("Code"))
        .or_else(|| body.get("ResponseMetadata").and_then(|m| m.get("Error")).and_then(|e| e.get("Code")))
        .and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|i| i.to_string())))
        .unwrap_or_default();
    Err((
        false,
        format!(
            "HTTP {status}{}：{msg}",
            if code.is_empty() { String::new() } else { format!(" / code {code}") }
        ),
    ))
}

/// 在基准材料上换一套「设备身份 + 客户端参数」，其余（token / refreshToken / 域名）不动。
fn material_variant(
    base: &CloudMaterial,
    dev: &DeviceCandidate,
    prof: (&'static str, &'static str, &'static str),
) -> CloudMaterial {
    let mut m = base.clone();
    m.profile = prof.0;
    m.client_id = prof.1;
    m.platform_code = prof.2;
    m.device_id = dev.device_id.clone();
    // 设备身份自带的 machineId 为空时**回落基准材料**，不要现编一个随机值 ——
    // 服务端对同一台设备的 machineId 稳定性有据可查，随机值只会平白多一个不匹配维度。
    m.machine_id = if dev.machine_id.is_empty() {
        base.machine_id.clone()
    } else {
        dev.machine_id.clone()
    };
    m.private_key_pem = dev.private_key_pem.clone();
    m.public_key_pem = dev.public_key_pem.clone();
    m
}

/// 取可用 token：未过期直接用；过期则 refreshToken 设备签名续期。
pub async fn ensure_cloud_token(m: &CloudMaterial) -> Result<String, String> {
    if jwt_exp(&m.access_token) > now_ms() {
        return Ok(m.access_token.clone());
    }
    let body = exchange_token(m).await?;
    if let Some(t) = parse_refresh_body(&body) {
        return Ok(t);
    }
    Err(format!(
        "ExchangeToken 刷新成功但响应中无 token 字段：{}",
        serde_json::to_string(&body).unwrap_or_default().chars().take(200).collect::<String>()
    ))
}

/// 界面「账号 token」弹窗要的数据：jwt 本体 + 有效期 + 能不能续期。**只读、不联网。**
pub fn account_token_info(client_key: &str, account_id: &str) -> Result<Value, String> {
    let (m, source) = account_material(client_key, account_id).ok_or_else(|| {
        "这个账号没有可读的凭证：既没有「网页登录」凭证（oauth.json），也没有载体登录态（storage.json）。\
         先在客户端登录一次并「备份当前登录态」，或改用「发起网页登录」。"
            .to_string()
    })?;
    let exp = jwt_exp(&m.access_token);
    let has_device_key = !m.private_key_pem.is_empty() && !m.device_id.is_empty();
    Ok(json!({
        "ok": true,
        "clientKey": client_key,
        "id": account_id,
        "uid": jwt_user_id(&m.access_token),
        "token": m.access_token,
        "tokenExp": if exp > 0 { json!(exp) } else { Value::Null },
        "source": source,
        // 续期三件套齐了才谈得上刷新：refreshToken + 设备私钥 + deviceId
        "canRefresh": !m.refresh_token.is_empty() && has_device_key,
        // 续期会用哪套客户端参数 —— 服务端按「登录时的 clientId」绑 RT，这一项对不上就 20403，
        // 界面把它显出来，报错时能一眼看出是不是参数问题。
        "profile": m.profile,
        "profileClientId": m.client_id,
        "thresholdDays": REFRESH_THRESHOLD_DAYS,
        "remainingDays": days_left_ms(exp),
    }))
}

/// 把续期结果写回账号档案（`oauth.json` 与载体 `storage.json` 各一份）。
///
/// ⚠️ 写前把原字节留在内存里，写完**回读校验**（能解出来且 token 一致）；不通过就把原字节写回去。
///    不落 `.bak` 文件：账号目录里的任何文件都会被 `export_account` 一起搬走，留一堆备份反而污染。
/// 返回实际更新的文件名。
fn persist_refreshed(
    client_key: &str,
    account_id: &str,
    token: &str,
    refresh: &str,
    exp_ms: i64,
    refresh_exp_ms: Option<i64>,
    client_id: &str,
) -> Result<Vec<String>, String> {
    let dir = account_dir(client_key, account_id);
    let mut updated = Vec::new();

    // 1) oauth.json（网页凭证）：明文 JSON，直接改字段
    let oauth_path = dir.join("oauth.json");
    if oauth_path.is_file() {
        let original = std::fs::read(&oauth_path).map_err(|e| format!("读 oauth.json 失败: {e}"))?;
        let mut v: Value = serde_json::from_slice(&original).map_err(|e| format!("解析 oauth.json 失败: {e}"))?;
        if let Some(obj) = v.as_object_mut() {
            set_first_key(obj, &TOKEN_KEYS, json!(token), "token");
            set_first_key(obj, &REFRESH_TOKEN_KEYS, json!(refresh), "refreshToken");
            // ★ 记住这次命中的 ClientID：服务端按「登录时的 clientId」绑定 RT，下次直接
            //   排第一候选，省掉那一轮必定失败的尝试（参照 `trae_refresh.js` 的状态文件）。
            obj.insert("profileClient".into(), json!(client_id));
            if exp_ms > 0 {
                obj.insert("tokenExp".into(), json!(exp_ms));
                obj.insert("expiredAt".into(), json!(rfc3339_of_ms(exp_ms)));
            }
            if let Some(r) = refresh_exp_ms {
                obj.insert("refreshExpiredAt".into(), json!(rfc3339_of_ms(r)));
            }
            obj.insert("updatedAt".into(), json!(chrono::Local::now().to_rfc3339()));
        }
        let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        let after: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if find_value(&after, &TOKEN_KEYS).as_deref() != Some(token) {
            return Err("写回 oauth.json 前的自检不通过，已放弃".into());
        }
        write_atomic_verified(&oauth_path, text.as_bytes(), &original)?;
        updated.push("oauth.json".to_string());
    }

    // 2) 载体 storage.json：登录态是 Km 加密的，改完要按同款重新加密
    let storage_path = dir.join(REL_STORAGE);
    if storage_path.is_file() {
        let original = std::fs::read(&storage_path).map_err(|e| format!("读 storage.json 失败: {e}"))?;
        let mut root: Value = serde_json::from_slice(&original).map_err(|e| format!("解析 storage.json 失败: {e}"))?;
        let blob = root.get(KEY_AUTH).and_then(|v| v.as_str()).map(String::from);
        if let Some(blob) = blob {
            if is_km_value(&blob) {
                let mut auth = decrypt_km_json(&blob).ok_or("载体登录态解密失败，未改动")?;
                if let Some(obj) = auth.as_object_mut() {
                    set_first_key(obj, &TOKEN_KEYS, json!(token), "token");
                    set_first_key(obj, &REFRESH_TOKEN_KEYS, json!(refresh), "refreshToken");
                    if exp_ms > 0 {
                        obj.insert("expiredAt".into(), json!(rfc3339_of_ms(exp_ms)));
                    }
                    if let Some(r) = refresh_exp_ms {
                        obj.insert("refreshExpiredAt".into(), json!(rfc3339_of_ms(r)));
                    }
                }
                let enc = crate::modules::trae_km::encrypt_km_json(&auth)?;
                if let Some(obj) = root.as_object_mut() {
                    obj.insert(KEY_AUTH.into(), json!(enc));
                }
                let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
                // 自检：新密文必须能解回同一个 token（密文逐字节不可比，见项目硬规则）
                let check = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v.get(KEY_AUTH).and_then(|x| x.as_str()).map(String::from))
                    .filter(|b| is_km_value(b))
                    .and_then(|b| decrypt_km_json(&b));
                if check.as_ref().and_then(|a| find_value(a, &TOKEN_KEYS)).as_deref() != Some(token) {
                    return Err("写回载体登录态前的自检不通过，已放弃".into());
                }
                write_atomic_verified(&storage_path, text.as_bytes(), &original)?;
                updated.push(REL_STORAGE.to_string());
            }
        }
    }
    Ok(updated)
}

/// 按键名候选写值；一个候选项都不在时用 `fallback` 新建（保证读端一定找得到）。
fn set_first_key(
    obj: &mut serde_json::Map<String, Value>,
    keys: &[&str],
    val: Value,
    fallback: &str,
) -> bool {
    for k in keys {
        if obj.contains_key(*k) {
            obj.insert((*k).to_string(), val);
            return true;
        }
    }
    obj.insert(fallback.to_string(), val);
    true
}

fn rfc3339_of_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

/// 原子写 + 校验；校验失败把原字节写回。
fn write_atomic_verified(path: &std::path::Path, data: &[u8], original: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data).map_err(|e| format!("写临时文件失败: {e}"))?;
    if std::fs::rename(&tmp, path).is_err() {
        // 文件被占用时退回直接覆写（Windows 上 rename 到已存在文件可能失败）
        if let Err(e) = std::fs::write(path, data) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("写入失败: {e}"));
        }
        let _ = std::fs::remove_file(&tmp);
    }
    match std::fs::read(path) {
        Ok(after) if after.as_slice() == data => Ok(()),
        _ => {
            let _ = std::fs::write(path, original);
            Err("写入后回读不一致，已还原原文件".into())
        }
    }
}

/// 续期门槛的**纯判定**（与网络无关，便于单测）。
///
/// 入参 `exp_ms` = access token 到期时间（**毫秒**，来自 `jwt_exp`）。
///
/// 返回 `Some(剩余天数)` = **跳过刷新**（access token 还新鲜，剩余大于门槛）；
/// `None` = 该刷新（已过期 / 剩余不足门槛 / `force`）。
///
/// ⚠️ 「跳过」不是错误：调用方必须把它表达成 `Ok(refreshed=false)`，
///    界面据此弹窗让用户决定要不要强制刷新。
///
/// ⚠️ 单位一致性是这个函数的命门：以前入参叫 `exp_secs` 而实际收到毫秒 ⇒
///    `exp_ms <= now_secs() + 7d` 恒假 ⇒ **永远判「还剩 2000 多万天」、永远不刷新**。
pub fn refresh_skip_days(exp_ms: i64, force: bool) -> Option<i64> {
    if force || exp_ms <= now_ms() + REFRESH_THRESHOLD_DAYS * 86_400_000 {
        return None;
    }
    Some(days_left_ms(exp_ms))
}

/// 刷新账号 token（界面上的「刷新 token 有效期」）。
///
/// 门槛与参考实现一致：access token 剩余 > [`REFRESH_THRESHOLD_DAYS`] 天时**默认不刷新**
/// —— 但**不是报错**，而是 `{ ok: true, refreshed: false, reason: "fresh" }`：
/// 界面据此弹窗让用户自己决定要不要**强制刷新**（`force = true`）。
///
/// ⚠️ 「跳过」与「失败」必须长得不一样：跳过 = `Ok` + `refreshed:false`；
///    真失败（网络 / 服务端拒绝 / 自检不过）一律 `Err`，界面照实原文显示。
pub async fn refresh_account_token(
    client_key: &str,
    account_id: &str,
    force: bool,
) -> Result<Value, String> {
    let (m, source) = account_material(client_key, account_id)
        .ok_or_else(|| "这个账号没有可读的凭证，无法续期。".to_string())?;
    let exp = jwt_exp(&m.access_token);

    if let Some(left) = refresh_skip_days(exp, force) {
        return Ok(json!({
            "ok": true,
            "refreshed": false,
            "reason": "fresh",
            "remainingDays": left,
            "thresholdDays": REFRESH_THRESHOLD_DAYS,
            "token": m.access_token,
            "tokenExp": json!(exp),
            "message": format!("token 还剩 {left} 天（> {REFRESH_THRESHOLD_DAYS} 天），本次不刷新。"),
        }));
    }

    // ⚠️ 三个维度都要试（各失败后果见各自注释）：
    //    · **凭据**：账号档的 RT 可能是旧的一代 —— 参考实现是两轮（先实时登录态、再备份档），
    //      只试账号档那一份 = `401 20403 Token device not match`（旧 RT 早被换代）；
    //    · 设备身份：客户端 icube-dc 密钥**会变**，RT 绑的是登录那一刻那把 ⇒ 只用一个来源
    //      = 同一个 `20403`；
    //    · ClientID：服务端按「登录时的 clientId」绑定 RT，一并带入的账号目录名与登录客户端
    //      经常不是同一个 ⇒ 只按目录选 = `400 10101` / `401 20403`。
    //    贴着参考实现的模型排（凭据自带设备 = 同源优先，再交叉兜底），命中即止。
    let uid = crate::modules::trae_vault::uid_of_account(client_key, account_id)
        .or_else(|| jwt_user_id(&m.access_token))
        .unwrap_or_default();
    let sets = credential_sets(&m, client_key, &uid, account_id);

    // 组装候选（去重后截断）。`label` 只用于报错时让人看清是哪种组合。
    struct Attempt {
        label: String,
        material: CloudMaterial,
    }
    let mut combos: Vec<Attempt> = Vec::new();
    let mut combo_seen: Vec<String> = Vec::new();
    let add_combo = |label: String, mm: CloudMaterial, combos: &mut Vec<Attempt>, seen: &mut Vec<String>| {
        let k = format!(
            "{}|{}|{}",
            short_hash(&mm.refresh_token),
            key_fingerprint(&mm.public_key_pem),
            mm.client_id
        );
        if seen.contains(&k) {
            return;
        }
        seen.push(k);
        combos.push(Attempt {
            label,
            material: mm,
        });
    };

    // 第一轮：每份凭据 × **它自带的**设备身份 × 参数候选（三者同源，最贴近参考实现）。
    for set in &sets {
        let profiles = profile_candidates(client_key, &set.own_client, set.remembered.as_deref());
        for prof in profiles {
            let mut mm = material_variant(&m, &set.device, prof);
            mm.access_token = set.access_token.clone();
            mm.refresh_token = set.refresh_token.clone();
            mm.own_client = set.own_client.clone();
            add_combo(
                format!("{} + {}（{}）", set.label, prof.0, prof.1),
                mm,
                &mut combos,
                &mut combo_seen,
            );
        }
    }
    // 第二轮：账号档凭据 × 本机客户端设备 × 参数候选（账号档里的设备身份缺失时兜底）。
    for dev in device_candidates(&m, client_key) {
        let profiles = profile_candidates(client_key, &m.own_client, m.remembered_profile.as_deref());
        for prof in profiles {
            let mm = material_variant(&m, &dev, prof);
            add_combo(
                format!("账号档 + {}（{}）", prof.0, prof.1),
                mm,
                &mut combos,
                &mut combo_seen,
            );
        }
    }
    let total_combos = combos.len();
    combos.truncate(MAX_REFRESH_ATTEMPTS);

    let mut failures: Vec<String> = Vec::new();
    let mut transport: Option<String> = None;
    let mut hit: Option<((&'static str, &'static str, &'static str), Value)> = None;

    for at in &combos {
        let mm = &at.material;
        // 传输层失败（`status == 0`）时**原地退避重发同一个组合**：换凭据不可能修好网络，
        // 而短时断连在本项目里是已知偶发（积分刷新同款 400/800 ms 退避）。
        let mut last = exchange_token_once(mm).await;
        if matches!(last, Err((true, _))) {
            for wait in [400u64, 800] {
                tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                last = exchange_token_once(mm).await;
                if !matches!(last, Err((true, _))) {
                    break;
                }
            }
        }
        match last {
            Ok(body) => {
                hit = Some(((mm.profile, mm.client_id, mm.platform_code), body));
                break;
            }
            // 三种退避都发不出去 ⇒ 服务端不可达。继续换凭据 / 设备 / 参数纯属白费（每次还要再等
            // 1.2 s 退避），而且会把「网络不通」伪装成「设备不匹配」。
            Err((true, e)) => {
                transport = Some(e);
                break;
            }
            Err((false, e)) => failures.push(format!("{}：{}", at.label, e)),
        }
    }

    if let Some(e) = transport {
        return Err(format!(
            "刷新 token 失败：请求没能与服务端完成一次交互 —— {e}\n\
             ⚠️ 这不是账号或设备的问题：换 refreshToken、换设备密钥、换客户端参数都不会有用。\n\
             常见原因：本机安全软件 / 防火墙拦了出站连接、`HTTP_PROXY` 指向的代理不可用、临时断网。\n\
             处理：确认本机能直接访问 {} 后重试。",
            m.exchange_host.trim_end_matches('/')
        ));
    }

    let Some(((profile, client_id, _), body)) = hit else {
        let tail = if total_combos > combos.len() {
            format!("\n（另有 {} 种组合未尝试：已到上限 {MAX_REFRESH_ATTEMPTS}）", total_combos - combos.len())
        } else {
            String::new()
        };
        return Err(format!(
            "刷新 token 失败：这个账号的 refreshToken 不接受本机任何一套「凭据 + 设备身份 + 客户端参数」。\
             逐套的真实答复 ——\n{}\n{}\
             最常见的原因是：这把 refreshToken 绑定的设备公钥**本机已经没有了**\
             （在别的机器上登录过、或本机客户端换过 icube-dc 密钥）。\
             解决办法：在这个客户端里**重新登录一次**该账号（登录会重建设备绑定），\
             再用「备份当前登录态」把新凭据收进账号库。\n\
             若清单里出现 `20403 Token device not match`：设备不匹配（RT 绑的是别的公钥）；\
             出现 `10101 无效参数`：ClientID 不对（服务端按**登录时用的 clientId** 绑定 RT）。",
            failures.join("\n"),
            tail
        ));
    };

    let new_token = parse_refresh_body(&body)
        .ok_or_else(|| format!("刷新响应里没有 token 字段：{}", serde_json::to_string(&body).unwrap_or_default().chars().take(200).collect::<String>()))?;
    let new_refresh = parse_refresh_token(&body).unwrap_or_else(|| m.refresh_token.clone());
    let new_exp = {
        let e = jwt_exp(&new_token);
        // 新 token 解不出 exp 时按官方口径兜底：access token 14 天。
        if e > 0 { e } else { now_ms() + 14 * 86_400_000 }
    };
    let refresh_exp = parse_refresh_exp(&body);

    let updated = persist_refreshed(client_key, account_id, &new_token, &new_refresh, new_exp, refresh_exp, client_id)
        .map_err(|e| format!("token 已刷新，但写回账号档案失败：{e}"))?;

    Ok(json!({
        "ok": true,
        "refreshed": true,
        "force": force,
        "source": source,
        "token": new_token,
        "tokenExp": new_exp,
        "tokenExpBefore": if exp > 0 { json!(exp) } else { Value::Null },
        "remainingDays": days_left_ms(new_exp),
        "refreshExpiredAt": refresh_exp.map(|v| json!(v)).unwrap_or(Value::Null),
        "updatedFiles": updated,
        // 命中的是哪套客户端参数（已写回 oauth.json，下次直接排第一候选）。
        "profile": profile,
        "profileClientId": client_id,
        "message": format!("已刷新（客户端参数 {profile} / {client_id}），新 token 已写回账号档案（oauth.json / 载体登录态）。"),
    }))
}

/// 删除云端会话（任务列表记录）。成功返回 `{ok, http}`；凭证缺失返回错误。
pub async fn delete_cloud_session(client_key: &str, uid: &str, session_id: &str) -> Result<Value, String> {
    let m = cloud_material_for(client_key, uid)
        .ok_or_else(|| format!("账号（uid …{}）没有可用的云端凭证（未通过工具网页登录或备份过登录态）", &uid[uid.len().saturating_sub(6)..]))?;
    let token = ensure_cloud_token(&m).await?;
    let url = format!("{}/api/remote/v1/chat_sessions/{}", m.remote_host, session_id);
    let mut headers = HashMap::new();
    headers.insert("Authorization".into(), format!("Cloud-IDE-JWT {token}"));
    headers.insert("x-device-id".into(), m.device_id);
    headers.insert("X-User-Region".into(), USER_REGION.into());
    headers.insert("Content-Type".into(), "application/json".into());
    headers.insert("User-Agent".into(), "twin-switch/1.0".into());
    let (status, _, body_text) = http_request_raw(&url, "DELETE", None, Some(&headers), None, true).await;
    if status >= 200 && status < 300 {
        return Ok(json!({ "ok": true, "http": status }));
    }
    let body: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
    let msg = body
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("Message").and_then(|v| v.as_str()))
        .map(String::from)
        .unwrap_or_else(|| body_text.chars().take(200).collect::<String>());
    let code = body
        .get("code")
        .or_else(|| body.get("Code"))
        .and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|i| i.to_string())))
        .unwrap_or_default();
    Err(format!(
        "云端删除失败（HTTP {status}{}）：{}",
        if code.is_empty() { String::new() } else { format!(" / code {code}") },
        msg
    ))
}

/// 该 uid 是否有可用的云端凭证（删除预览提示用，只读不请求）。
pub fn has_cloud_credential(client_key: &str, uid: &str) -> bool {
    cloud_material_for(client_key, uid).is_some()
}

/// 测试用：临时目录辅助。
#[cfg(test)]
pub fn _store_dir() -> std::path::PathBuf {
    crate::modules::config::store_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(n: i64) -> i64 {
        now_ms() + n * 86_400_000
    }

    /// 造一份基准材料（只填续期用得到的字段）。
    fn material(own_client: &str, remembered: Option<&str>) -> CloudMaterial {
        CloudMaterial {
            access_token: "tok".into(),
            refresh_token: "rt".into(),
            device_id: "1000000000000001".into(),
            machine_id: "machine-base".into(),
            private_key_pem: "-----BEGIN PRIVATE KEY-----\nAAA\n-----END PRIVATE KEY-----".into(),
            public_key_pem: "-----BEGIN PUBLIC KEY-----\nAAA\n-----END PUBLIC KEY-----".into(),
            app_version: "3.4.2".into(),
            profile: TRAE_PROFILE.0,
            client_id: TRAE_PROFILE.1,
            platform_code: TRAE_PROFILE.2,
            own_client: own_client.to_string(),
            remembered_profile: remembered.map(String::from),
            exchange_host: EXCHANGE_DEFAULT_HOST.to_string(),
            remote_host: "https://example.invalid".into(),
        }
    }

    /// ⚠️ 续期命中的关键：**账号自己 `client` 字段那套排第一**。
    ///
    /// 「一并带入」搬过来的账号，目录名（`trae-cn`）与登录客户端（`solo-cn`）经常不一致；
    /// 只用目录名选 ClientID ⇒ 线上实测 `401 20403 Token device not match`。
    /// 两个 ClientID 都在候选里（去重后正好 2 套），且登录客户端排最前。
    #[test]
    fn profile_candidates_put_the_login_client_first() {
        let got: Vec<&str> = profile_candidates("trae-cn", "solo-cn", None)
            .into_iter()
            .map(|p| p.0)
            .collect();
        assert_eq!(got, vec!["SOLO", "TRAE"], "登录客户端 SOLO 必须排第一，另一套兜底");

        // 上次成功的那套优先于一切（写在 oauth.json 的 profileClient 里）。
        let got: Vec<&str> = profile_candidates("trae-cn", "solo-cn", Some("ono9krqynydwx5"))
            .into_iter()
            .map(|p| p.0)
            .collect();
        assert_eq!(got, vec!["TRAE", "SOLO"]);

        // 同一套参数只出现一次（按 ClientID 去重）。
        assert_eq!(profile_candidates("trae-cn", "trae-cn", Some("ono9krqynydwx5")).len(), 2);
    }

    /// ⚠️ 设备候选的排序与去重：**账号档那套必须排第一**（它就是当年登录用的那把），
    /// 同一把公钥只留一条（否则会把有限的尝试次数浪费在重复组合上）。
    #[test]
    fn push_device_dedupes_by_public_key_and_drops_incomplete_rows() {
        let mut out: Vec<DeviceCandidate> = Vec::new();
        let mut seen: Vec<String> = Vec::new();

        push_device(&mut out, &mut seen, "账号档", "d1", "m1", "priv-a", "pub-a");
        // 同一把公钥、另一个来源 ⇒ 丢掉。
        push_device(&mut out, &mut seen, "本机 trae-cn", "d1", "m1", "priv-a", "pub-a");
        // 另一把公钥 ⇒ 保留。
        push_device(&mut out, &mut seen, "本机 solo-cn", "d2", "m2", "priv-b", "pub-b");
        // 缺 deviceId / 缺私钥 ⇒ 丢掉（签不了名，试也是白试）。
        push_device(&mut out, &mut seen, "坏档1", "", "m3", "priv-c", "pub-c");
        push_device(&mut out, &mut seen, "坏档2", "d3", "m3", "", "pub-c");

        let labels: Vec<&str> = out.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, vec!["账号档", "本机 solo-cn"]);
        assert_eq!(out[0].machine_id, "m1");
        assert_eq!(out.len(), 2, "去重后只该剩两条");
    }

    /// 客户端标识表：去重、保序、跳过空串与乱七八糟的名字（候选表里出现空串会读错文件）。
    #[test]
    fn unique_client_keys_dedupes_and_skips_junk() {
        assert_eq!(
            unique_client_keys(&["trae-cn", "", "trae-cn", "solo-cn", "不知道"]),
            vec!["trae-cn", "solo-cn"]
        );
        assert!(unique_client_keys(&[""]).is_empty());
    }

    /// 凭据指纹只用于判等：同一串稳定，不同串不同，且长度固定（不泄露原文长度以外的信息）。
    #[test]
    fn short_hash_is_stable_and_hides_the_secret() {
        assert_eq!(short_hash("rt-abc"), short_hash("rt-abc"));
        assert_ne!(short_hash("rt-abc"), short_hash("rt-abd"));
        assert_eq!(short_hash("rt-abc").len(), 12);
        assert!(!short_hash("rt-abc").contains("abc"));
    }

    /// 公钥指纹按 PEM 文本算，且空格/换行差异会被 trim 掉（同一把钥匙不论怎么排版指纹一致）。
    #[test]
    fn key_fingerprint_is_trim_stable() {
        assert_eq!(key_fingerprint("PUB"), key_fingerprint("  PUB\n"));
        assert_ne!(key_fingerprint("PUB"), key_fingerprint("PUB2"));
        assert_eq!(key_fingerprint("PUB").len(), 12);
    }

    /// 换一套「设备身份 + 客户端参数」时，只有这四个字段变；token / refreshToken / 域名一律不动。
    /// 另：候选自身没有 machineId 时**回落基准材料**，不许现编随机值。
    #[test]
    fn material_variant_swaps_only_the_identity() {
        let base = material("trae-cn", None);
        let dev = DeviceCandidate {
            label: "本机 solo-cn".into(),
            device_id: "2000000000000002".into(),
            machine_id: String::new(),
            private_key_pem: "priv-2".into(),
            public_key_pem: "pub-2".into(),
        };
        let v = material_variant(&base, &dev, SOLO_PROFILE);
        assert_eq!(v.device_id, "2000000000000002");
        assert_eq!(v.private_key_pem, "priv-2");
        assert_eq!(v.client_id, SOLO_PROFILE.1);
        assert_eq!(v.platform_code, SOLO_PROFILE.2);
        assert_eq!(v.machine_id, "machine-base", "候选没带 machineId ⇒ 回落基准，不许随机");
        assert_eq!(v.access_token, base.access_token);
        assert_eq!(v.refresh_token, base.refresh_token);
        assert_eq!(v.exchange_host, base.exchange_host);
        assert_eq!(v.own_client, base.own_client);

        // 候选自带 machineId ⇒ 用它自己的（设备身份要成套）。
        let dev2 = DeviceCandidate {
            machine_id: "machine-2".into(),
            ..dev
        };
        assert_eq!(material_variant(&base, &dev2, TRAE_PROFILE).machine_id, "machine-2");
    }

    /// ⚠️ 线上回归：`status == 0`（请求根本没发出去）的原因**必须原样带出来**。
    ///
    /// 旧实现一律按「未知错误」上报，把本机那句真正的 `Access is denied. (os error 5)`
    /// 整个吞掉 ⇒ 界面只剩一句没用的「未知错误」，根因查不到。
    /// 而且必须标成**传输层失败**（`true`）：它和「服务端拒绝」的处置完全不同
    /// ——前者换凭据毫无意义，后者才该换一套身份再试。
    #[test]
    fn transport_failure_keeps_the_real_reason_and_is_flagged() {
        let e = classify_exchange_response(0, "Access is denied. (os error 5)").unwrap_err();
        assert!(e.0, "status=0 必须标成传输层失败");
        assert!(e.1.contains("Access is denied. (os error 5)"), "真原因不许吞掉：{}", e.1);
        assert!(!e.1.contains("未知错误"));

        // 原因文本为空时给一句能看懂的兜底，也不是「未知错误」。
        let e = classify_exchange_response(0, "   ").unwrap_err();
        assert!(e.0);
        assert!(e.1.contains("连接被中断"), "{}", e.1);

        // 服务端答复 ⇒ 不是传输层失败，且 code 要带出来（界面靠它区分 20403 / 10101）。
        let e = classify_exchange_response(
            401,
            r#"{"ResponseMetadata":{"Error":{"Code":"20403","Message":"Token device not match."}}}"#,
        )
        .unwrap_err();
        assert!(!e.0, "服务端答复不算传输层失败");
        assert!(e.1.contains("HTTP 401") && e.1.contains("20403") && e.1.contains("Token device not match"), "{}", e.1);

        let e = classify_exchange_response(200, r#"{"Result":{"Token":"t"}}"#).unwrap();
        assert_eq!(find_value(&e, &["Token"]).as_deref(), Some("t"));
    }

    /// 门槛的三种情形：还新鲜 ⇒ 跳过；不足门槛 / 已过期 ⇒ 该刷；`force` ⇒ 一律刷。
    ///
    /// 这条直接守住界面上那个按钮的语义：**剩余 > 7 天默认不刷，但不是失败**
    /// （跳过走 `Ok(refreshed=false)`，界面据此弹「强制刷新」确认窗）。
    #[test]
    fn refresh_threshold_skips_only_when_still_fresh() {
        assert_eq!(refresh_skip_days(days(13), false), Some(13));
        assert_eq!(refresh_skip_days(days(8), false), Some(8));
        // 正好卡在门槛上 ⇒ 该刷（判据是「大于 7 天」才跳）。
        assert_eq!(refresh_skip_days(days(7), false), None);
        assert_eq!(refresh_skip_days(days(3), false), None);
        assert_eq!(refresh_skip_days(days(-1), false), None);
        assert_eq!(refresh_skip_days(0, false), None);
        // 强制刷新：再新鲜也要刷。
        assert_eq!(refresh_skip_days(days(13), true), None);
        assert_eq!(refresh_skip_days(days(365), true), None);
    }

    /// ⚠️ 线上回归（用户截图）：token 的 `exp` 已是毫秒时，剩余 5 天**必须**刷。
    ///
    /// 旧实现把毫秒当秒 ⇒ `days_left` 报 2000 多万天、`refresh_skip_days` 恒判「新鲜」⇒
    /// 界面上那个按钮**永远不刷新**（点了只会弹「还剩 20723318 天，仍要强制刷新吗？」）。
    ///
    /// 判据**走真实链路**（`jwt_exp` → `refresh_skip_days`），不自己造毫秒常量 ——
    /// 否则把 `jwt_exp` 改回 `exp * 1000` 这条也照过，等于没测到。
    #[test]
    fn millisecond_expiry_does_not_look_like_year_58765() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let exp_ms = now_ms() + 5 * 86_400_000; // 5 天后到期
        let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let body = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp_ms}}}"#).as_bytes());
        let token = format!("{head}.{body}.sig");

        assert_eq!(jwt_exp(&token), exp_ms, "毫秒 exp 必须原样返回，不许再乘 1000");
        assert_eq!(
            refresh_skip_days(jwt_exp(&token), false),
            None,
            "只剩 5 天必须刷新，不许再判「新鲜」"
        );
        assert_eq!(days_left_ms(jwt_exp(&token)), 5);
        // 界面拿到的时间戳必须还在合理区间（2020 ~ 2100 年），不是 5 万年以后。
        assert!((1_577_836_800_000..4_102_444_800_000).contains(&jwt_exp(&token)));
    }

    /// 跳过时的剩余天数不能算错（`days_left_ms` 只对「无 exp」兜底成 -1，其余按真实差值给）。
    #[test]
    fn days_left_never_lies_about_expiry() {
        assert_eq!(days_left_ms(days(13)), 13);
        assert_eq!(days_left_ms(0), -1, "解不出 exp ⇒ -1，不假装是 0");
        assert_eq!(days_left_ms(days(-5)), -5, "已过期就是负的天数，界面据此显示「已过期」");
    }

    /// ⚠️⚠️ 设备签名必须是「对 `SHA-256(签名串)` 的签名」，**不是**对 `SHA-256(SHA-256(签名串))`。
    ///
    /// 这条用**自造密钥**走完整链路：签名 → 用对应公钥按**原消息**验签。
    /// `VerifyingKey::verify` 内部自己会 SHA-256 一次，所以：
    ///   · 正确实现（`sign_prehash(SHA256(msg))`）⇒ 验签通过；
    ///   · 双重哈希（`sign(&digest)` 被当成消息再哈希一次）⇒ 当场失败。
    ///
    /// 线上症状就是后者：服务端验签不过，回报 `401 20403 Token device not match`，
    /// 让人以为是「设备换了 / RT 绑在别的机器上」——换设备密钥、换参数全是白试。
    #[test]
    fn device_signature_is_verifiable_over_the_message() {
        use p256::ecdsa::signature::Verifier;
        use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
        use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};

        let sk = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
        let priv_pem = sk.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
        // 公钥直接从同一把私钥推出来（不绕 PEM，避免测的其实是编码而不是签名）。
        let vk: VerifyingKey = *SigningKey::from_pkcs8_pem(&priv_pem).unwrap().verifying_key();

        let msg = "POST\n/trae/api/v3/oauth/ExchangeToken\nono9krqynydwx5\nRT-abc\n1791000000\ndeadbeef";
        let sig_b64 = sign_ecdsa_sha256(&priv_pem, msg).expect("签名应当成功");

        let raw = STANDARD.decode(&sig_b64).expect("签名应是 base64(DER)");
        let sig = Signature::from_der(&raw).expect("签名应是 DER 编码");
        vk.verify(msg.as_bytes(), &sig)
            .expect("必须能按原消息验签 —— 失败说明签名被双重哈希了");

        // 换个消息就验不过（确认这条判据真的在验内容，不是恒真）。
        assert!(vk.verify(b"other message", &sig).is_err());
    }

    /// 键名候选：命中已有的就改、一个都没有才新建（读端一定找得到）。
    #[test]
    fn set_first_key_reuses_an_existing_name() {
        let mut obj = serde_json::Map::new();
        obj.insert("refreshToken".into(), json!("old"));
        assert!(set_first_key(&mut obj, &REFRESH_TOKEN_KEYS, json!("new"), "refreshToken"));
        assert_eq!(obj.get("refreshToken"), Some(&json!("new")));
        assert_eq!(obj.len(), 1, "不该另建一个同义键");

        let mut empty = serde_json::Map::new();
        assert!(set_first_key(&mut empty, &TOKEN_KEYS, json!("t"), "token"));
        assert_eq!(empty.get("token"), Some(&json!("t")));
    }

    /// 刷新响应解析：Token / RefreshToken 都要能从 `Result` 里挖出来；
    /// 刷新额度到期支持秒与毫秒两种写法。
    #[test]
    fn parses_refresh_response_fields() {
        let body = json!({
            "Result": {
                "Token": "header.payload.sig",
                "RefreshToken": "rt-2",
                "RefreshExpireAt": "1800000000000",
            }
        });
        assert_eq!(parse_refresh_body(&body).as_deref(), Some("header.payload.sig"));
        assert_eq!(parse_refresh_token(&body).as_deref(), Some("rt-2"));
        assert_eq!(parse_refresh_exp(&body), Some(1_800_000_000_000));

        let secs = json!({ "Result": { "RefreshExpiredAt": "1780000000" } });
        assert_eq!(parse_refresh_exp(&secs), Some(1_780_000_000_000));

        let iso = json!({ "Result": { "refreshExpiredAt": "2027-03-27T00:00:00Z" } });
        let want = chrono::DateTime::parse_from_rfc3339("2027-03-27T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(parse_refresh_exp(&iso), Some(want));

        // 服务端不返回新 refreshToken ⇒ 沿用旧的（调用方兜底），这里必须是 None。
        let no_rt = json!({ "Result": { "Token": "x" } });
        assert_eq!(parse_refresh_token(&no_rt), None);
    }
}
