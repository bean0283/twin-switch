//! 无头自检（`TwinSwitch.exe --self-check`）。
//!
//! ## 为什么需要它
//!
//! 项目原有的深度自查是 `tools/render-check/driver.mjs`（起真窗口、逐页截图、比对文案），
//! 一轮 30 分钟起 —— 重到「不敢跑」，等于没有。
//! 这里补的是它覆盖不到的**另一半**：命令接线、真实路径解析、账号库读写、serde 形状。
//! 全部只读，秒级跑完，逐行 `OK` / `FAIL` / `SKIP`，末尾 `SELF-CHECK OK` + 退出码 0。
//!
//! ⛔ **绝不调用 `switch_now` / `stop_client` / 任何写类命令** —— 自检不能改变用户现场，
//! 更不许把用户正在用的客户端杀掉。这条和测试里的纪律是同一条。
//!
//! ## 为什么同时写日志
//!
//! release 构建是 `windows_subsystem = "windows"`：没有亲和的控制台，`println!` 在双击
//! 运行时无处可去。所以报告同时落到 `~/.twin-switch/selfcheck.log`，用户粘给我们就行。

use std::io::Write;
use std::path::PathBuf;

use wb_switch_core::modules::{
    config, gateway_server, qoder_accounts, qoder_auth_dat, qoder_realm::QoderRegion,
    qoder_sessions, settings, trae_discover, workbuddy_switch, workbuddy_vault,
};

/// 一行结果。
enum Line {
    Ok(String),
    /// 本机没有这东西 —— **不是失败**（没装客户端是常态）。
    Skip(String),
    Fail(String),
    /// 只是陈述事实，不参与判定。
    Info(String),
}

fn main_run() -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();

    out.push(Line::Info(format!(
        "TwinSwitch {} · {} · {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )));

    // ① 数据目录可写 —— 后面所有功能的前提，失败就是 FAIL。
    let dir = config::store_dir();
    match std::fs::create_dir_all(&dir).and_then(|_| {
        let probe = dir.join(".selfcheck-probe");
        std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe))
    }) {
        Ok(()) => out.push(Line::Ok(format!("数据目录可写：{}", dir.display()))),
        Err(e) => out.push(Line::Fail(format!("数据目录不可写（{}）：{e}", dir.display()))),
    }

    // ② 设置能读出来（不是「有文件」，是「解析成功」）。
    let s = settings::load();
    out.push(Line::Ok(format!(
        "设置可读：写库前备份={} 同步定时任务={}",
        s.backup_before_write, s.sync_workbuddy_automations
    )));

    // ③ Trae 客户端（国内外各一档）：探测主程序，没装则 SKIP。
    for key in ["trae-cn", "trae-intl"] {
        match trae_discover::get_client(key) {
            Some(c) => match trae_discover::detect_exe(c) {
                Some(p) => out.push(Line::Ok(format!("{} 主程序：{}", c.label, p.display()))),
                None => out.push(Line::Skip(format!("{} 未安装 / 未找到主程序", c.label))),
            },
            None => out.push(Line::Fail(format!("客户端常量里没有 key={key}"))),
        }
    }

    // ④ WorkBuddy：主程序 + 账号库。
    match workbuddy_switch::resolve_exe() {
        Some(p) => out.push(Line::Ok(format!("WorkBuddy 主程序：{}", p.display()))),
        None => out.push(Line::Skip("WorkBuddy 未安装 / 未找到主程序".to_string())),
    }
    let wb = workbuddy_vault::load_accounts();
    out.push(Line::Ok(format!("WorkBuddy 账号库：{} 个账号", wb.len())));

    // ⑤ Qoder 两区域：账号库 + 会话库。
    for region in [QoderRegion::Cn, QoderRegion::Global] {
        let name = region.display_name();
        match qoder_accounts::list(region) {
            Ok(list) => out.push(Line::Ok(format!("{name} 账号库：{} 个账号", list.len()))),
            Err(e) => out.push(Line::Skip(format!("{name} 账号库读不到：{e}"))),
        }
        match qoder_sessions::db_path(region) {
            Some(p) => match qoder_sessions::list(region, true) {
                Ok(rows) => out.push(Line::Ok(format!(
                    "{name} 会话库可读：{} 条会话（{}）",
                    rows.len(),
                    p.display()
                ))),
                Err(e) => out.push(Line::Fail(format!("{name} 会话库读失败：{e}"))),
            },
            None => out.push(Line::Skip(format!("{name} 没有 main.sqlite（未登录过）"))),
        }
    }

    // ⑥ 托管判定：只报结论，不参与判定（它取决于「这次是怎么启动的」）。
    for region in [QoderRegion::Cn, QoderRegion::Global] {
        let p = qoder_auth_dat::placement(region);
        out.push(Line::Info(format!(
            "托管判定 {}：{}",
            region.as_str(),
            match p {
                qoder_auth_dat::Placement::Outside => "不在客户端进程树里（可安全切换）".to_string(),
                qoder_auth_dat::Placement::Hosted { evidence } =>
                    format!("在客户端进程树里（{evidence}）⇒ 切号会被拒绝"),
                qoder_auth_dat::Placement::Unknown { reason } =>
                    format!("判不出来（{reason}）⇒ 切号会被拒绝"),
            }
        )));
    }

    // ⑦ 积分网关：只读状态，**不启动**。
    let st = gateway_server::status();
    let running = st.get("running").and_then(|v| v.as_bool()).unwrap_or(false);
    out.push(Line::Info(format!(
        "积分网关：{} · 端口 {} · 成功 {} / 被拒 {}",
        if running { "运行中" } else { "未启动" },
        st.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
        st.get("requests").and_then(|v| v.as_u64()).unwrap_or(0),
        st.get("rejected").and_then(|v| v.as_u64()).unwrap_or(0),
    )));

    // ⑧ 模型清单：形状是 `{作用域: [模型名…]}`，按作用域累加（不是 `{models:[…]}`）。
    let models = config::store_dir().join("models.json");
    match std::fs::read_to_string(&models) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(map)) => {
                let mut total = 0usize;
                let mut scopes: Vec<String> = Vec::new();
                for (scope, list) in &map {
                    if let Some(arr) = list.as_array() {
                        total += arr.len();
                        scopes.push(format!("{scope}={}", arr.len()));
                    }
                }
                scopes.sort();
                out.push(Line::Ok(format!(
                    "模型清单可解析：{total} 个模型（{}）",
                    scopes.join(" ")
                )));
            }
            Ok(_) => out.push(Line::Fail("模型清单顶层不是对象（应为 {作用域: [模型]}）".into())),
            Err(e) => out.push(Line::Fail(format!("模型清单不是合法 JSON：{e}"))),
        },
        Err(_) => out.push(Line::Skip(format!("还没有 {}（首次识别后生成）", models.display()))),
    }

    out
}

/// 跑一遍自检并返回进程退出码（0 = 全过，1 = 有 FAIL）。
pub fn run() -> i32 {
    let lines = main_run();
    let mut text = String::new();
    let mut failed = 0usize;
    for l in &lines {
        let row = match l {
            Line::Ok(m) => format!("OK    {m}"),
            Line::Skip(m) => format!("SKIP  {m}"),
            Line::Fail(m) => {
                failed += 1;
                format!("FAIL  {m}")
            }
            Line::Info(m) => format!("INFO  {m}"),
        };
        println!("{row}");
        text.push_str(&row);
        text.push('\n');
    }
    let verdict = if failed == 0 {
        "SELF-CHECK OK".to_string()
    } else {
        format!("SELF-CHECK FAILED（{failed} 项失败）")
    };
    println!("{verdict}");
    text.push_str(&verdict);
    text.push('\n');

    // 报告落盘：GUI 构建没有亲和控制台，用户能把这份直接发回来。
    let path: PathBuf = config::store_dir().join("selfcheck.log");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = f.write_all(text.as_bytes());
        println!("报告：{}", path.display());
    }

    if failed == 0 {
        0
    } else {
        1
    }
}
