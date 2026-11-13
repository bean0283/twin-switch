//! 网络自修复（**补偿机制**，2026-11-13）。
//!
//! 背景：共享 HTTP 客户端在**首次请求时**按进程环境变量构建（reqwest 默认行为，
//! 保持不变）。环境里的代理（`HTTPS_PROXY` / `ALL_PROXY` 等）若在运行期间失效
//! （外部代理工具退出、端口变更），客户端会一直拿着死代理直到重启应用 ——
//! 表现为所有出站请求 `error sending request for url (...)`。
//!
//! 本模块只在**用户点「尝试修复」后**执行，不参与默认代理逻辑：
//!
//! 1. [`status`]：读环境代理变量 + TCP 探测代理端口。前端据此决定是否亮出
//!    「尝试修复」按钮 —— **显示条件：传输层失败 && 检测到代理 && 代理端口不可达**；
//! 2. [`attempt_repair`]：重建共享客户端（`config::reset_http_client`，下次请求按
//!    当前环境重建，代理逻辑不变，等效「不重启应用就重启网络层」）→ 复测上游；
//!    若代理死了且复测仍失败、直连却可达，降级安装直连客户端
//!    （`config::install_direct_http_client`）。
//!
//! 关键步骤写 diag 日志（error 级别默认就记，诊断开关开着时可完整回溯）。

use serde_json::{json, Value};
use std::net::TcpStream;
use std::time::Duration;

use crate::modules::config::{http_request_raw, install_direct_http_client, reset_http_client};
use crate::modules::diag_log;

/// 上游连通性复测地址：与积分 / 签到链路同一个域，测得通即代表积分链路可恢复。
const PROBE_URL: &str = "https://www.codebuddy.cn/";
/// 代理端口 TCP 探测超时：代理死了最多等 2 秒，别让修复界面卡住。
const TCP_TIMEOUT: Duration = Duration::from_secs(2);

/// 按与 reqwest 相同的优先级找 https 请求的代理变量（找到第一个非空即用）。
fn detect_proxy_env() -> Option<(String, String)> {
    const KEYS: [&str; 6] = [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ];
    for key in KEYS {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some((key.to_string(), value));
            }
        }
    }
    None
}

/// 从代理 URL 里解析 `host:port`（支持 `http://h:p` / `https://h:p` / `h:p`）。
fn proxy_host_port(addr: &str) -> Option<(String, u16)> {
    let rest = addr.split("://").nth(1).unwrap_or(addr);
    let host_port = rest.split('/').next()?;
    let (host, port) = host_port.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

/// TCP 探测代理端口是否有人应答（只测连接，不代理流量）。
fn probe_tcp(host: &str, port: u16) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("地址解析失败: {e}"))?
        .next()
        .ok_or_else(|| "地址解析结果为空".to_string())?;
    TcpStream::connect_timeout(&addr, TCP_TIMEOUT)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 用**当前共享客户端**复测上游。返回 `(status, 错误信息)`：
/// `status > 0` 即传输层连通（任意 HTTP 状态码都算 —— 403/5xx 也证明连接通了）。
async fn probe_upstream() -> (u16, Option<String>) {
    let (st, _headers, text) = http_request_raw(PROBE_URL, "GET", None, None, None, true).await;
    if st == 0 {
        (0, Some(text))
    } else {
        (st, None)
    }
}

/// 修复前的诊断快照：前端用它决定「尝试修复」按钮的显隐与文案。
pub fn status() -> Value {
    let Some((key, value)) = detect_proxy_env() else {
        return json!({
            "hasProxy": false,
            "envKey": Value::Null,
            "proxyUrl": Value::Null,
            "proxyAddr": Value::Null,
            "alive": Value::Null,
            "probeError": Value::Null,
        });
    };
    let (proxy_addr, alive, probe_error) = match proxy_host_port(&value) {
        Some((h, p)) => match probe_tcp(&h, p) {
            Ok(()) => (Some(format!("{h}:{p}")), Some(true), None),
            Err(e) => (Some(format!("{h}:{p}")), Some(false), Some(e)),
        },
        None => (
            None,
            None,
            Some("代理地址解析不出 host:port".to_string()),
        ),
    };
    json!({
        "hasProxy": true,
        "envKey": key,
        "proxyUrl": value,
        "proxyAddr": proxy_addr,
        "alive": alive,
        "probeError": probe_error,
    })
}

/// 执行修复。返回 `{ ok, mode, message, steps, suggestion }`：
/// - `mode = "proxy"`：重建后环境代理生效且上游连通；
/// - `mode = "direct"`：代理不可达，已降级为直连客户端（补偿，默认流程不受影响）；
/// - `mode = "fail"`：无路可走，给出人工处理建议。
pub async fn attempt_repair() -> Value {
    let mut steps: Vec<String> = Vec::new();

    // ① 丢弃旧客户端（它可能在启动时固化了已失效的代理）。构建逻辑不变，
    //    下一次请求按当前环境重建 —— 这就是"重启网络层"。
    reset_http_client();
    steps.push("已丢弃旧的网络客户端（启动时可能固化了失效代理）".to_string());
    diag_log::info("net-repair", "用户触发尝试修复：已重置共享 HTTP 客户端");

    // ② 用重建后的客户端复测上游
    let (st, err) = probe_upstream().await;
    if st > 0 {
        let had_proxy = detect_proxy_env().is_some();
        let message = format!("修复完成：网络层已重建，上游连通（HTTP {st}）");
        steps.push(format!("复测上游连通（HTTP {st}）"));
        diag_log::info("net-repair", &format!("修复成功：上游 HTTP {st}"));
        return json!({
            "ok": true,
            "mode": if had_proxy { "proxy" } else { "direct" },
            "message": message,
            "steps": steps,
            "suggestion": Value::Null,
        });
    }
    steps.push(format!("复测上游仍失败：{}", err.unwrap_or_default()));

    // ③ 环境里有代理且不可达 → 降级直连再测
    match detect_proxy_env() {
        Some((key, value)) => {
            let dead = match proxy_host_port(&value) {
                Some((h, p)) => probe_tcp(&h, p).is_err(),
                None => true,
            };
            if !dead {
                steps.push(format!(
                    "代理 {value}（{key}）端口可达，但走它连不上上游 —— 可能是代理规则问题"
                ));
            } else {
                steps.push(format!("代理 {value}（{key}）不可达，尝试降级为直连"));
                if let Err(e) = install_direct_http_client() {
                    steps.push(e);
                    return repair_failed(steps);
                }
                let (st2, err2) = probe_upstream().await;
                if st2 > 0 {
                    steps.push(format!("直连复测连通（HTTP {st2}）—— 已切换直连链路"));
                    diag_log::info(
                        "net-repair",
                        &format!("修复成功（直连降级）：上游 HTTP {st2}"),
                    );
                    return json!({
                        "ok": true,
                        "mode": "direct",
                        "message": "代理不可用，已切换为直连链路（重建代理环境后可再点修复切回）",
                        "steps": steps,
                        "suggestion": Value::Null,
                    });
                }
                steps.push(format!("直连复测仍失败：{}", err2.unwrap_or_default()));
            }
        }
        None => {
            steps.push("环境里没有代理变量 —— 失败与代理无关，多半是本机网络问题".to_string());
        }
    }
    repair_failed(steps)
}

fn repair_failed(steps: Vec<String>) -> Value {
    diag_log::error("net-repair", "修复未成功：所有候选链路均不可达");
    json!({
        "ok": false,
        "mode": "fail",
        "message": "修复未成功：代理与直连均无法连通上游",
        "steps": steps,
        "suggestion": "请手动重启代理工具后重试；若仍失败，请检查本机网络后重启本应用",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_host_port_parses_common_forms() {
        assert_eq!(
            proxy_host_port("http://127.0.0.1:7890"),
            Some(("127.0.0.1".into(), 7890))
        );
        assert_eq!(
            proxy_host_port("https://proxy.corp.lan:8443/api"),
            Some(("proxy.corp.lan".into(), 8443))
        );
        assert_eq!(
            proxy_host_port("127.0.0.1:24808"),
            Some(("127.0.0.1".into(), 24808))
        );
        assert_eq!(proxy_host_port("socks5://[::1]:1080"), Some(("::1".into(), 1080)));
        assert_eq!(proxy_host_port("http://no-port.example.com"), None);
        assert_eq!(proxy_host_port("http://127.0.0.1:notaport"), None);
    }

    #[test]
    fn detect_proxy_env_none_in_test_env() {
        // 测试进程不设代理变量时必须返回 None（CI / 开发机若真挂了代理变量，
        // 这里会失败 —— 恰好提醒环境里有代理残留）。
        for key in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"] {
            std::env::remove_var(key);
        }
        assert!(detect_proxy_env().is_none());
    }
}
