//! T46 · S2「跨账号写入」真机验证。
//!
//! 把一条会话**原样克隆到另一个账号**（`insert` + `target_uid`），然后逐项核对归属是否
//! 真的改到了目标账号 —— 这是 T29 那个坑的正面验证：
//!
//! 客户端按 `session_project` 组织工程下的会话、又用 `chat_session.context` 里的
//! `last_real_project_id` 定位「真实工程」。三者只要有一处还指着**源账号**的工程，
//! 症状就是「在客户端里删不掉这条会话、重启后记录复活」。
//!
//! 所以判据是三条**都**必须成立：
//!   ① `chat_session.project_id` → 目标账号名下的工程
//!   ② `session_project.project_id` → 与 ① 一致
//!   ③ `chat_session.context.last_real_project_id` → 与 ① 一致
//! 外加一条总判据：`edit_doc` 报出来的 `ownerUid` 必须等于目标 uid。
//!
//! 最后把克隆出来的会话删掉 —— 跑完不留痕。
//!
//! ⚠️ 会真的写库、会结束并重启 Trae 客户端。必须显式加 `--yes`。
//!
//! 用法：
//! ```text
//! cargo run -p wb-switch-core --example t46_cross_account_probe -- trae-cn <sessionId|空> [targetUid] [--yes]
//! ```
//! 不给 `targetUid` 时只列出可选账号。

use serde_json::Value;
use wb_switch_core::modules::{trae_delete, trae_discover, trae_export, trae_session_edit, trae_switch};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let yes = args.iter().any(|a| a == "--yes");
    let rest: Vec<String> = args.into_iter().filter(|a| a != "--yes").collect();
    if rest.is_empty() {
        eprintln!("用法: t46_cross_account_probe <clientKey> [sessionId] [targetUid] [--yes]");
        std::process::exit(2);
    }
    let client = rest[0].clone();
    let want_sid = rest.get(1).cloned().unwrap_or_default();
    let want_uid = rest.get(2).cloned().unwrap_or_default();

    // ---- 0. 可选账号清单（只读）----
    let targets = match trae_session_edit::list_target_uids(&client) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[x] 读账号清单失败：{e}");
            std::process::exit(1);
        }
    };
    println!("[x] 可写入的目标账号：");
    let accounts = targets["accounts"].as_array().cloned().unwrap_or_default();
    for a in &accounts {
        println!(
            "    uid={}  {}  会话 {} / 工程 {}",
            a["uid"].as_str().unwrap_or("?"),
            a["label"].as_str().unwrap_or("?"),
            a["sessions"],
            a["projects"]
        );
    }
    if want_uid.is_empty() {
        println!("\n[x] 未指定 targetUid，到此为止（只读）。");
        return;
    }

    // ---- 1. 选源会话：不指定就取第一条 ----
    let sid = if want_sid.is_empty() {
        println!("\n[x] 未指定 sessionId，自动选取第一条会话…");
        match first_session(&client) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[x] 选源会话失败：{e}");
                std::process::exit(1);
            }
        }
    } else {
        want_sid
    };
    let src = match trae_session_edit::edit_doc(&client, &sid) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[x] 读源会话失败：{e}");
            std::process::exit(1);
        }
    };
    let src_owner = src["ownerUid"].as_str().unwrap_or("").to_string();
    println!(
        "\n[x] 源会话 {}《{}》 归属 uid={}",
        sid,
        src["title"].as_str().unwrap_or(""),
        if src_owner.is_empty() { "(无)" } else { &src_owner }
    );
    println!("[x] 目标账号 uid={want_uid}");
    if src_owner == want_uid {
        println!("[x] ⚠️ 源与目标同账号 —— 这次只验证「写了但归属不变」，不是真正的跨账号。");
    }
    let src_pid = cell_str(&src, "chat_session", "project_id");
    println!("[x] 源工程 project_id = {src_pid}");

    if !yes {
        println!("\n[x] 预演结束（未写库）。加 `--yes` 才会真的执行：");
        println!("    克隆到目标账号 → 核对三处归属 → 删掉克隆");
        return;
    }

    // ---- 2. 跨账号克隆（insert + target_uid）----
    println!("\n[x] ===== 跨账号克隆（insert + targetUid）=====");
    let log = |m: &str| println!("   {m}");
    let saved = match trae_session_edit::edit_save(
        &src,
        "insert",
        None,
        Some(&want_uid),
        Some(&log),
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[x] 跨账号写入失败：\n{e}");
            std::process::exit(3);
        }
    };
    let copy_sid = saved["sessionId"].as_str().unwrap_or("").to_string();
    println!(
        "[x] 已写入 {copy_sid}：{} 行，重写 {}/{} 页，targetUid={}",
        saved["rows"], saved["changedPages"], saved["pages"], saved["targetUid"]
    );

    // ---- 3. 读回核对 ----
    let mut bad = 0usize;
    let copy = trae_session_edit::edit_doc(&client, &copy_sid);
    match copy {
        Ok(c) => {
            let owner = c["ownerUid"].as_str().unwrap_or("");
            let pid = cell_str(&c, "chat_session", "project_id");
            let sp_pid = cell_str(&c, "session_project", "project_id");
            let ctx_pid = context_last_real_project_id(&c);

            println!("\n[x] 克隆后的归属：");
            println!("    edit_doc 报的 ownerUid      = {owner}");
            println!("    chat_session.project_id     = {pid}");
            println!("    session_project.project_id  = {sp_pid}");
            println!("    context.last_real_project_id= {}", ctx_pid.as_deref().unwrap_or("(无该键)"));

            if owner == want_uid {
                println!("    ✓ ownerUid 已指向目标账号");
            } else {
                println!("    ✗ ownerUid 不是目标账号（{owner} ≠ {want_uid}）");
                bad += 1;
            }
            if sp_pid == pid {
                println!("    ✓ session_project 与 chat_session 归属一致");
            } else {
                println!("    ✗ session_project 归属不一致 —— 客户端会删不掉这条会话");
                bad += 1;
            }
            if let Some(ctx) = &ctx_pid {
                if *ctx == pid {
                    println!("    ✓ context.last_real_project_id 已跟上");
                } else {
                    println!("    ✗ context.last_real_project_id 仍是旧工程（{ctx} ≠ {pid}）");
                    bad += 1;
                }
            } else {
                println!("    - context 里没有 last_real_project_id，跳过");
            }
            if pid != src_pid {
                println!("    ✓ 工程确实换成了目标账号的另一条（不是复用源工程）");
            } else {
                println!("    ⚠️ 工程 id 与源相同 —— 源/目标可能是同一账号，或库里有同路径工程被复用");
            }
        }
        Err(e) => {
            println!("[x] ✗ 读回克隆失败：{e}");
            bad += 1;
        }
    }

    // ---- 4. 删掉克隆，别留垃圾 ----
    println!("\n[x] 清理克隆…");
    if let Some(c) = trae_discover::get_client(&client) {
        let out = trae_switch::quit_client(c, 60_000);
        println!("    退出客户端：ok={}，残留 {}", out.ok, out.remaining.len());
    }
    match trae_delete::delete_sessions(&client, &[copy_sid.clone()], Some(&log)) {
        Ok(_) => println!("[x] 克隆已删除"),
        Err(e) => {
            println!("[x] ✗ 删除克隆失败：{e}");
            println!("    请手动清理会话 {copy_sid}（`t46_cleanup` 可以）");
            bad += 1;
        }
    }

    println!("\n[x] 结论：{bad} 处不一致");
    if bad == 0 {
        println!("[x] S2 跨账号写入验证通过");
    } else {
        std::process::exit(4);
    }
}

/// 取某表第一行某列的文本值。
fn cell_str(doc: &Value, table: &str, col: &str) -> String {
    doc["tables"]
        .as_array()
        .and_then(|a| a.iter().find(|t| t["name"] == Value::String(table.to_string())))
        .and_then(|t| t["rows"].as_array())
        .and_then(|r| r.first())
        .and_then(|r| r.get(col))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// `chat_session.context` 是 JSON 文本列，读出来已是对象。
fn context_last_real_project_id(doc: &Value) -> Option<String> {
    doc["tables"]
        .as_array()?
        .iter()
        .find(|t| t["name"] == Value::String("chat_session".into()))?
        .get("rows")?
        .as_array()?
        .first()?
        .get("context")?
        .get("last_real_project_id")?
        .as_str()
        .map(str::to_string)
}

/// 读会话列表并挑消息数最多的一条。
fn first_session(client: &str) -> Result<String, String> {
    let list = trae_export::list_sessions(client)?;
    list.into_iter()
        .max_by_key(|s| s.messages)
        .map(|s| s.id)
        .ok_or_else(|| "没有可用会话".into())
}
