//! JWT 无验签解析（只读 payload）。
//!
//! 两个产品的登录态都是 JWT，解析方式是同一套，因此**只有这一份实现**：
//! Trae 侧 `trae_km::{jwt_payload, jwt_exp, jwt_user_id}` 与 WorkBuddy 侧
//! `workbuddy_credits` 全都在这里取数（见各自模块的薄封装）。
//!
//! ## ⚠️ 单位是这个模块存在的理由
//!
//! JWT 规范里 `exp` 是**秒**，但真实世界两种都发：Trae 老账号发秒（10 位），
//! 新账号发**毫秒**（13 位）。曾经这里写死 `exp * 1000`，于是毫秒的 token 被再乘
//! 1000 —— 界面显示成「有效期 58765/4/20，还剩 20723318 天」（1000 倍）。
//! 所以本模块**只出口毫秒**，并按量级归一化：谁都不许再自己乘 1000。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;

/// 解 JWT 中间段（base64url）。失败返回 `None`。
pub fn payload(token: &str) -> Option<Value> {
    // 值上可能带 `Bearer ` / `Cloud-IDE-JWT ` 前缀，一并剥掉。
    let token = token.trim();
    let token = token
        .strip_prefix("Cloud-IDE-JWT ")
        .or_else(|| token.strip_prefix("Bearer "))
        .unwrap_or(token)
        .trim();
    let seg = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(seg.trim()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 把「秒 / 毫秒 / 微秒 / 纳秒」时间戳统一成**毫秒**。
///
/// 判据按量级分段（各段的「所属年」互不重叠，容错到 2100 年前后）：
/// - `< 1e11`      → 秒（最大 = 5138 年）
/// - `1e11 .. 1e14`→ 毫秒（1973 年 ~ 5138 年）
/// - `1e14 .. 1e17`→ 微秒（1973 年 ~ 5138 年）
/// - `>= 1e17`     → 纳秒
///
/// `<= 0` 原样返回（调用方以 `> 0` 判「有没有值」）；超出 i64 安全区的值不硬凑，
/// 走 `saturating_mul` 收敛到 `i64::MAX`。
pub fn normalize_ms(ts: i64) -> i64 {
    match ts {
        ts if ts <= 0 => ts,
        ts if ts < 100_000_000_000 => ts.saturating_mul(1000),         // 秒 → 毫秒
        ts if ts < 100_000_000_000_000 => ts,                          // 毫秒
        ts if ts < 100_000_000_000_000_000 => ts / 1000,               // 微秒 → 毫秒
        ts => ts / 1_000_000,                                          // 纳秒 → 毫秒
    }
}

/// 取 JWT 的过期时间（**毫秒**）；没有 `exp` / 不是 JWT 时返回 `0`。
///
/// `exp` 是字符串形式的数字也认（部分签发方会写成 `"1792892088"`）。
pub fn exp_ms(token: &str) -> i64 {
    let Some(v) = payload(token).and_then(|p| p.get("exp").cloned()) else {
        return 0;
    };
    let raw = v
        .as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()).map(|f| f as i64));
    raw.map(normalize_ms).unwrap_or(0)
}

/// JWT 的签发时间（**毫秒**）；没有 `iat` 返回 `0`。
pub fn iat_ms(token: &str) -> i64 {
    payload(token)
        .and_then(|p| p.get("iat")?.as_i64())
        .map(normalize_ms)
        .unwrap_or(0)
}

/// 从 Cloud-IDE-JWT 里取所属账号 id（`payload.data.id` / `user_id`）。
pub fn user_id(token: &str) -> Option<String> {
    let p = payload(token)?;
    let d = p.get("data")?;
    let uid = d.get("id").or_else(|| d.get("user_id"))?;
    uid.as_i64()
        .map(|v| v.to_string())
        .or_else(|| uid.as_str().map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    /// 造一个 payload 任意的 JWT（不签名，解析用不到）。
    fn make_jwt(exp: Value) -> String {
        let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let body = URL_SAFE_NO_PAD.encode(format!(r#"{{"data":{{"id":"842681600717036"}},"exp":{}}}"#, exp).as_bytes());
        format!("{head}.{body}.sig")
    }

    /// ⚠️ 这条就是线上事故的回归：`exp` 已经是毫秒时**不许**再乘 1000。
    #[test]
    fn expires_in_milliseconds_is_not_multiplied_again() {
        let want = 1_792_286_574_000_i64; // 2026-10-18T01:22:54Z
        assert_eq!(exp_ms(&make_jwt(serde_json::json!(want))), want);
        // 旧写法 exp*1000 会得到 1.792e15（界面显示成 58765 年），必须挡住。
        assert!(exp_ms(&make_jwt(serde_json::json!(want))) < 2_000_000_000_000);
    }

    /// 秒（Trae 老账号）→ 毫秒，逐字对齐原实现 `exp * 1000`。
    #[test]
    fn expires_in_seconds_is_converted() {
        assert_eq!(exp_ms(&make_jwt(serde_json::json!(1_792_892_088_i64))), 1_792_892_088_000);
    }

    /// 字符串数字、微秒、纳秒都要能落回同一个瞬间。
    #[test]
    fn other_units_land_on_the_same_instant() {
        let ms = 1_792_286_574_000_i64;
        assert_eq!(exp_ms(&make_jwt(serde_json::json!("1792286574"))), ms);
        assert_eq!(exp_ms(&make_jwt(serde_json::json!(ms * 1000))), ms);
        assert_eq!(exp_ms(&make_jwt(serde_json::json!(ms * 1_000_000))), ms);
        // 无 exp / 不是 JWT ⇒ 0（调用方据此显示「解不出」，不假装有值）
        assert_eq!(exp_ms(&make_jwt(serde_json::Value::Null)), 0);
        assert_eq!(exp_ms("not-a-jwt"), 0);
        assert_eq!(exp_ms(""), 0);
        assert_eq!(normalize_ms(0), 0);
        assert_eq!(normalize_ms(-5), -5);
    }

    #[test]
    fn strips_common_token_prefixes() {
        let t = make_jwt(serde_json::json!(1_792_892_088_i64));
        assert_eq!(user_id(&format!("Cloud-IDE-JWT {t}")).as_deref(), Some("842681600717036"));
        assert_eq!(user_id(&format!("Bearer {t}")).as_deref(), Some("842681600717036"));
    }
}
