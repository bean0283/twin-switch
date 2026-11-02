//! Qoder 账号包自检：① 导出包是否带机器身份；② 导入时机器身份会不会落地。
//!
//! 用法：`cargo run --release --example wb_qoder_pkg_check`
//!
//! 第 ② 步**在临时家目录里做**（`TWIN_SWITCH_HOME` 指到 temp），
//! 不会碰本机真实的 `~/.twin-switch/qoder-machine.json` —— 那玩意儿写错会让签到更糟。

use serde_json::json;
use wb_switch_core::modules::qoder_realm::QoderRegion;
use wb_switch_core::modules::qoder_tools;

fn main() {
    // ① 导出：包里有没有 machine 段
    match qoder_tools::export(QoderRegion::Global) {
        Ok(v) => {
            let text = v.get("json").and_then(|x| x.as_str()).unwrap_or("");
            let pkg: serde_json::Value = serde_json::from_str(text).unwrap_or(json!({}));
            println!(
                "导出：{} 个账号 | format={:?} version={:?} | machine={} | hasMachine={:?}",
                v.get("count").and_then(|x| x.as_u64()).unwrap_or(0),
                pkg.get("format").and_then(|x| x.as_str()),
                pkg.get("version").and_then(|x| x.as_u64()),
                if pkg.get("machine").map(|m| m.is_object()).unwrap_or(false) {
                    "有"
                } else {
                    "空"
                },
                v.get("hasMachine"),
            );
            if let Some(hint) = v.get("machineHint").and_then(|x| x.as_str()) {
                println!("提示：{hint}");
            }
            let first = pkg
                .pointer("/accounts/0")
                .cloned()
                .unwrap_or_else(|| json!({}));
            println!(
                "  账号字段样例：cosyMachineToken={} cosyMachineType={}",
                first.get("cosyMachineToken").is_some(),
                first.get("cosyMachineType").is_some()
            );
        }
        Err(e) => println!("导出失败（本机可能没有国际版账号）：{e}"),
    }

    // ② 导入：在临时家目录里验证机器身份落地
    let tmp = std::env::temp_dir().join(format!("twinswitch-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("建临时目录失败");
    std::env::set_var("TWIN_SWITCH_HOME", &tmp);

    let domain = wb_switch_core::modules::qoder_realm::realm_spec(QoderRegion::Global).domain;
    let pkg = json!({
        "format": qoder_tools::PKG_FORMAT,
        "version": qoder_tools::PKG_VERSION,
        "region": "global",
        "machine": {
            "format": "twinswitch.qoder-machine",
            "version": 1,
            "machineCode": "probe-code-0123456789",
            "machineToken": "probe-token-0123456789abcdef",
            "machineType": "probe-type-0123456789",
            "machineId": "probe-machine-id",
            "machineHostname": "probe-host",
            "machineOs": "x86_64_win32"
        },
        "accounts": [{
            "uid": "probe-uid", "name": "probe", "domain": domain,
            "accessToken": "probe-token", "source": "desktop-app"
        }]
    })
    .to_string();

    match qoder_tools::import_apply(QoderRegion::Global, &pkg, None) {
        Ok(v) => println!(
            "导入：imported={:?} machine={}",
            v.get("imported"),
            v.get("machine").map(|m| m.to_string()).unwrap_or_default()
        ),
        Err(e) => println!("导入失败：{e}"),
    }

    // 数据目录是 `<home>/.twin-switch`（不是 home 本身）
    let cfg = tmp.join(".twin-switch").join("qoder-machine.json");
    println!(
        "  临时目录里的 qoder-machine.json：{}",
        if cfg.is_file() {
            let t = std::fs::read_to_string(&cfg).unwrap_or_default();
            let v: serde_json::Value = serde_json::from_str(&t).unwrap_or(json!({}));
            format!(
                "已写入（machineId={:?} source={:?}）",
                v.get("machineId").and_then(|x| x.as_str()),
                v.get("source").and_then(|x| x.as_str())
            )
        } else {
            "没有生成".to_string()
        }
    );

    // 收尾：删临时目录，并把真实家目录还原
    std::env::remove_var("TWIN_SWITCH_HOME");
    let _ = std::fs::remove_dir_all(&tmp);
    println!("临时目录已清理（真实 qoder-machine.json 未被触碰）");
}
