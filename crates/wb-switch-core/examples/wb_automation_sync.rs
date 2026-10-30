//! 真机自检：把当前账号的定时任务同步给一个**临时假账号**，打印结果后原样清理。
//!
//! 用法：`cargo run --release --example wb_automation_sync [目标uid]`
//!
//! 为什么要有：`sync_copies_runs_and_state` 用的是内存库 + 自建 schema，
//! 只能证明逻辑对；这条跑的是**真实 32 列的库**，能证明字段对得上、客户端加列也不会炸。
//! 全程只动 `owner_user_id = 假账号` 的行，跑完删干净（含运行记录与运行状态）。

use chrono::TimeZone;
use rusqlite::Connection;

fn main() {
    let Some(from) = wb_switch_core::modules::workbuddy_auth::current_uid() else {
        eprintln!("没有当前登录账号，跳过");
        std::process::exit(2);
    };
    let to = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("twinswitch-probe-{}", uuid_like()));
    println!("源账号 {from} → 假账号 {to}");

    match wb_switch_core::modules::workbuddy_automations::sync_owner(&from, &to) {
        Ok(v) => println!(
            "第一次：copied={:?} skipped={:?} runs={} states={}",
            v.get("copied"),
            v.get("skippedNames"),
            v.get("copiedRuns").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("copiedRuntimeStates").and_then(|x| x.as_u64()).unwrap_or(0),
        ),
        Err(e) => {
            eprintln!("同步失败：{e}");
            std::process::exit(1);
        }
    }
    match wb_switch_core::modules::workbuddy_automations::sync_owner(&from, &to) {
        Ok(v) => println!(
            "第二次（应全跳过）：copied={:?} skipped={:?}",
            v.get("copied"),
            v.get("skippedNames"),
        ),
        Err(e) => eprintln!("第二次同步失败：{e}"),
    }

    // 把复制出来的东西摊开看一眼，再清理
    if let Some(path) = wb_switch_core::modules::workbuddy_automations::db_path() {
        match Connection::open(&path) {
            Ok(conn) => {
                let mut stmt = conn
                    .prepare(
                        "select id, name, next_run_at, last_run_at, status
                         from automations where owner_user_id = ?1",
                    )
                    .expect("查询失败");
                let rows: Vec<(String, String, Option<i64>, Option<i64>, String)> = stmt
                    .query_map([to.as_str()], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })
                    .expect("读取失败")
                    .filter_map(Result::ok)
                    .collect();
                let now = chrono::Utc::now().timestamp_millis();
                for (id, name, next, last, status) in &rows {
                    let runs: i64 = conn
                        .query_row(
                            "select count(*) from automation_runs where automation_id=?1",
                            [id.as_str()],
                            |r| r.get(0),
                        )
                        .unwrap_or(0);
                    let running: Option<i64> = conn
                        .query_row(
                            "select running from automation_runtime_state where automation_id=?1",
                            [id.as_str()],
                            |r| r.get(0),
                        )
                        .ok();
                    let future = next.map(|t| t > now).unwrap_or(false);
                    println!(
                        "  - {name} | status={status} | 运行记录={runs} 条 | running={running:?} | \
                         next_run_at={} | last_run_at={:?} | 未来时间={future}",
                        next.map(|t| chrono::Local
                            .timestamp_millis_opt(t)
                            .single()
                            .map(|d| d.format("%m-%d %H:%M").to_string())
                            .unwrap_or_default())
                            .unwrap_or_else(|| "null".into()),
                        last.map(|t| chrono::Local
                            .timestamp_millis_opt(t)
                            .single()
                            .map(|d| d.format("%m-%d %H:%M").to_string())
                            .unwrap_or_default())
                    );
                }
                let ids: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
                for id in &ids {
                    let _ = conn.execute("delete from automation_runs where automation_id=?1", [id.as_str()]);
                    let _ = conn.execute(
                        "delete from automation_runtime_state where automation_id=?1",
                        [id.as_str()],
                    );
                }
                let n = conn
                    .execute("delete from automations where owner_user_id=?1", [to.as_str()])
                    .unwrap_or(0);
                println!("已清理 {n} 条临时任务（含运行记录/状态）");
            }
            Err(e) => eprintln!("清理阶段打不开库：{e}"),
        }
    }
}

fn uuid_like() -> String {
    // 不想为此引入 uuid 依赖到 example 之外的语义，直接用简单随机串
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{n}")
}
