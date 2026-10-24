//! Qoder 的两个**区域**及其全部差异（照抄参考实现 `qoder-trae-workbuddy-switch`）。
//!
//! 国内版与国际版是**同一套客户端形态、两套端点常量**：
//! 差别只在端点 / 客户端 id / 本地目录 / 账号体系这几张表上。
//!
//! ⚠️ **不许在别处手写端点或目录名**：一律经 [`realm_spec`] 查表。
//! 参考实现里这条规则写得很重（「路径派生唯一规则」），照做 —— 否则加一个区域
//! 就要满仓库改字符串。

/// Qoder 的区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QoderRegion {
    /// 国内版（`qoder.com.cn` / `gateway.qoder.com.cn`）。
    Cn,
    /// 国际版（`qoder.com` / `api1.qoder.sh`，另有 api2 / api3 故障切换）。
    Global,
}

impl QoderRegion {
    /// 稳定标识（文件名后缀、Tauri 命令入参）。
    pub fn as_str(self) -> &'static str {
        match self {
            QoderRegion::Cn => "cn",
            QoderRegion::Global => "global",
        }
    }

    /// 展示名。
    ///
    /// 用「国内版 / 国际版」而不是客户端原名（`Qoder CN` / `Qoder`）——
    /// 两个版本装在**两台机器上都存在**时，客户端原名极容易看混（用户实测踩过）。
    pub fn display_name(self) -> &'static str {
        match self {
            QoderRegion::Cn => "国内版",
            QoderRegion::Global => "国际版",
        }
    }

    /// 宽容解析（诊断与旧值兼容用）。
    pub fn parse(s: &str) -> Option<QoderRegion> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cn" | "china" | "domestic" | "qoder_cn" | "qoder-cn" => Some(QoderRegion::Cn),
            "global" | "intl" | "international" | "qoder" | "qoder-global" => {
                Some(QoderRegion::Global)
            }
            _ => None,
        }
    }
}

/// 一个区域的全部常量。
#[derive(Debug, Clone, Copy)]
pub struct QoderRealm {
    pub region: QoderRegion,
    pub website: &'static str,
    /// 开放接口基址（账号 / 令牌 / 额度）。
    pub openapi_base: &'static str,
    /// 推理网关主基址。
    pub gateway_base: &'static str,
    /// 网关备用基址（顺序即优先级）。
    pub gateway_fallbacks: &'static [&'static str],
    pub client_id: &'static str,
    pub redirect_uri: &'static str,
    pub domain: &'static str,
    pub user_agent: &'static str,
    /// CLI 本地目录名（`~/.qoder-cn` / `~/.qoder`）。
    pub cli_home_dir: &'static str,
    /// 桌面 App 的 `%APPDATA%` 目录名（Electron userData）。
    pub desktop_app_dir: &'static str,
    /// 设备授权 URL 是否携带 `redirect_uri`（两区不同，实测结论）。
    pub send_redirect_uri: bool,
    /// 授权 URL 里 machine_id / nonce 是否带横线。
    pub nonce_dashed: bool,
}

pub const CN_REALM: QoderRealm = QoderRealm {
    region: QoderRegion::Cn,
    website: "https://qoder.com.cn",
    openapi_base: "https://openapi.qoder.com.cn",
    gateway_base: "https://gateway.qoder.com.cn",
    gateway_fallbacks: &[],
    client_id: "1c5e33e1-364d-4ce6-b02c-acaa81274a5c",
    redirect_uri: "qoder-work-cn://",
    domain: "qoder.com.cn",
    user_agent: "QoderWork/1.1.64",
    cli_home_dir: ".qoder-cn",
    desktop_app_dir: "com.qodercn.app.stable",
    send_redirect_uri: true,
    nonce_dashed: true,
};

pub const GLOBAL_REALM: QoderRealm = QoderRealm {
    region: QoderRegion::Global,
    website: "https://qoder.com",
    openapi_base: "https://openapi.qoder.sh",
    gateway_base: "https://api1.qoder.sh",
    gateway_fallbacks: &["https://api2.qoder.sh", "https://api3.qoder.sh"],
    client_id: "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb",
    redirect_uri: "qoder://aicoding.aicoding-agent/login-success",
    domain: "qoder.com",
    user_agent: "QoderWork/1.1.64",
    cli_home_dir: ".qoder",
    desktop_app_dir: "Qoder",
    send_redirect_uri: false,
    nonce_dashed: false,
};

/// 一个区域的**网关候选主机**（主基址在前，失败按顺序退）。
///
/// 国际版实测 `api1.qoder.sh` 会连不上（`error sending request`），
/// 只试第一个就整条流程失败 ⇒ 国际版模型清单永远「未识别」。
pub fn gateway_candidates(region: QoderRegion) -> Vec<&'static str> {
    let spec = realm_spec(region);
    let mut out = vec![spec.gateway_base];
    out.extend_from_slice(spec.gateway_fallbacks);
    out
}

/// 查表 —— **唯一入口**。
pub fn realm_spec(region: QoderRegion) -> &'static QoderRealm {
    match region {
        QoderRegion::Cn => &CN_REALM,
        QoderRegion::Global => &GLOBAL_REALM,
    }
}

// Qoder 的开放接口路径（照抄参考实现的常量）。
pub const PATH_DEVICE_AUTHORIZE: &str = "/api/v1/deviceToken/authorize";
pub const PATH_DEVICE_TOKEN_POLL: &str = "/api/v1/deviceToken/poll";
pub const PATH_DEVICE_TOKEN_REFRESH: &str = "/api/v1/deviceToken/refresh";
pub const PATH_JOB_TOKEN_EXCHANGE: &str = "/api/v1/jobToken/exchange";
pub const PATH_JOB_TOKEN_REFRESH: &str = "/api/v1/jobToken/refresh";
pub const PATH_QUOTA_USAGE: &str = "/api/v2/quota/usage";
pub const PATH_USER_PLAN: &str = "/api/v2/user/plan";
pub const PATH_MODEL_LIST: &str = "/algo/api/v2/model/list";
pub const PATH_CAMPAIGNS: &str = "/sash/api/v1/me/campaigns";
pub const PATH_CHECKIN_STATUS: &str = "/sash/api/v1/me/daily-check-in/status";
pub const PATH_CHECKIN_CLAIM: &str = "/sash/api/v1/me/daily-check-in/claim";

/// 以 `uid` 与 `salt` 稳定派生一个 32 位十六进制标识（MD5，与官方客户端同构）。
///
/// 参考实现特别注明：**不要**改成自创摘要 —— 这些值会作为
/// `X-Machine-ID` / `X-Session-ID` 发给上游，上游只见过官方形态。
pub fn derive_id(uid: &str, salt: &str) -> String {
    use md5::Digest as _;
    let mut hasher = md5::Md5::new();
    hasher.update(format!("{salt}:{uid}").as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_realms_have_distinct_endpoints_and_clients() {
        assert_ne!(CN_REALM.openapi_base, GLOBAL_REALM.openapi_base);
        assert_ne!(CN_REALM.client_id, GLOBAL_REALM.client_id);
        assert_eq!(realm_spec(QoderRegion::Cn).domain, "qoder.com.cn");
        assert_eq!(realm_spec(QoderRegion::Global).domain, "qoder.com");
    }

    #[test]
    fn cli_dirs_follow_the_reference() {
        assert_eq!(realm_spec(QoderRegion::Cn).cli_home_dir, ".qoder-cn");
        assert_eq!(realm_spec(QoderRegion::Global).cli_home_dir, ".qoder");
    }

    #[test]
    fn derive_id_is_stable_and_salted() {
        let a = derive_id("u-1", "machine");
        assert_eq!(a, derive_id("u-1", "machine"));
        assert_eq!(a.len(), 32);
        assert_ne!(a, derive_id("u-1", "session"), "不同 salt 必须不同");
        assert_ne!(a, derive_id("u-2", "machine"));
    }

    #[test]
    fn region_parse_accepts_scope_form() {
        assert_eq!(QoderRegion::parse("qoder-cn"), Some(QoderRegion::Cn));
        assert_eq!(QoderRegion::parse("qoder-global"), Some(QoderRegion::Global));
        assert_eq!(QoderRegion::parse("nope"), None);
    }
}
