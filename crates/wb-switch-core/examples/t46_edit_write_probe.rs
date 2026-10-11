//! T46 会话编辑器：**写入路径**真机验证。
//!
//! 只读探针（`t46_edit_probe`）证不了写入。这个探针走两遍完整往返，且**不碰原会话内容**：
//!
//! **场景 A —— 新增（`insert`）**：把源会话拷成一条新会话 → 读回比对 → 删掉副本。
//! **场景 B —— 就地覆写（`overwrite`）**：只改标题、且只标 `chat_session` 一张表，
//! 验证「那 74 MB 的 `chat_message_task` 一行都没动」，最后把标题改回去。
//!
//! 四条判据（都跟 id 无关，因为副本的 id 本来就是新的）：
//!
//! 1. 每张表行数一致；
//! 2. 内容按「id 无关归一化」后逐表相等（把 hex/UUID 形态的 id 统一替换成 `<id>`）；
//! 3. **新旧「列级」id 集合不相交** —— 这才是不撞 UNIQUE、不跨会话串数据的硬保证。
//!    （JSON 载荷里嵌的服务端 id 不在判据内：它们从来不是列值、进不了重映射表，
//!    既有 `copy_session` 也原样照抄 —— 见 `collect_ids` 的说明。）
//! 4. 最大的大字段两边全文逐字段相等（这是「74 MB 有没有原样搬过去」的直接证据）。
//!
//! ⚠️ 会真的写库，也会**结束并重启 Trae 客户端**（写入路径的既定行为）。
//!    必须显式加 `--yes` 才会执行。
//!
//! 用法：`cargo run -p wb-switch-core --example t46_edit_write_probe -- trae-cn <sessionId|空> --yes`

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use wb_switch_core::modules::{trae_delete, trae_discover, trae_export, trae_session_edit, trae_switch};

/// hex（20~24 位）或 UUID 形态 ⇒ 当它是 id。
fn is_idish(s: &str) -> bool {
    (20..=24).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
        || (s.len() == 36 && s.matches('-').count() == 4)
}

/// 「与源会话共享、**故意不重映射**」的列：比对与「id 不相交」检查都要放过它们。
///
/// - `project_id` / `user_id`：账号归属维度，跨账号才是另一套语义（S2）。
/// - `agent_run_id` / `server_task_id`：指向**服务端** agent run 的引用。既有
///   `copy_session` 也只改 `history_v2_id` 这个唯一键、原样照抄这两列；而且本机
///   `agent_run` 表里这些会话**一行都没有**（引用本来就是悬空的）。在这里重映射
///   只会造出一批同样悬空、又对不上源会话的新 id —— 纯属自找麻烦。
fn is_account_col(k: &str) -> bool {
    matches!(
        k,
        "project_id" | "user_id" | "agent_run_id" | "server_task_id"
    )
}

/// id 无关归一化：自增主键、引用的 `pk`、所有 id 形态的字符串都抹平。
fn norm(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, x) in o {
                if k == "id" {
                    continue;
                }
                if k == "$jsonRef" {
                    if let Some(inner) = x.as_object() {
                        let mut im = Map::new();
                        for (ik, iv) in inner {
                            if ik != "pk" {
                                im.insert(ik.clone(), iv.clone());
                            }
                        }
                        m.insert(k.clone(), Value::Object(im));
                        continue;
                    }
                }
                m.insert(k.clone(), norm(x));
            }
            Value::Object(m)
        }
        Value::Array(a) => Value::Array(a.iter().map(norm).collect()),
        Value::String(s) if is_idish(s) => json!("<id>"),
        other => other.clone(),
    }
}

/// doc 里出现的全部**列级** id 形态字符串（排除账号/共享引用维度的列）。
///
/// ⚠️ 只看**行对象的顶层值**，不递归进 JSON 列/大字段载荷。为什么：
///
/// - 真正会造成 UNIQUE 冲突、跨会话串数据的，只有**列级 id**（`message_id` /
///   `history_v2_id` / `reply_to_message_id` …）。这一层的「新旧不相交」才是硬保证。
/// - JSON 载荷（`chat_message_task.content`、`history_v2.messages`）里嵌的 id
///   是**服务端内部引用**：它们从来不是任何列的值，因而不在重映射表里，
///   也不可能被「按列值全局替换」改到。既有 `copy_session` 同样是原样照抄 ——
///   这是**既有语义**，不是本轮的缺陷。真机上实测残留 14 个，全部属于这一类。
fn collect_ids(doc: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for t in doc["tables"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        for r in t["rows"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let Some(obj) = r.as_object() else { continue };
            for (k, v) in obj {
                if k == "$jsonRef" || is_account_col(k) {
                    continue;
                }
                if let Value::String(s) = v {
                    if is_idish(s) {
                        out.insert(s.clone());
                    }
                }
            }
        }
    }
    out
}

/// doc 里体积最大的大字段引用：(表名, 行号, 列名, pk, 字节数)。
fn biggest_ref(doc: &Value) -> Option<(String, usize, String, Value, u64)> {
    let mut best: Option<(String, usize, String, Value, u64)> = None;
    for t in doc["tables"].as_array()? {
        let tname = t["name"].as_str()?.to_string();
        for (i, r) in t["rows"].as_array()?.iter().enumerate() {
            for (c, v) in r.as_object()? {
                if let Some(m) = v.get("$jsonRef").and_then(|x| x.as_object()) {
                    let bytes = m.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0);
                    if best.as_ref().map(|b| bytes > b.4).unwrap_or(true) {
                        best = Some((
                            tname.clone(),
                            i,
                            c.clone(),
                            m.get("pk").cloned().unwrap_or(Value::Null),
                            bytes,
                        ));
                    }
                }
            }
        }
    }
    best
}

fn table<'a>(doc: &'a Value, name: &str) -> Option<&'a Value> {
    doc["tables"]
        .as_array()?
        .iter()
        .find(|t| t["name"].as_str() == Some(name))
}

/// 逐表比对，返回不一致处数。
fn compare_tables(src: &Value, copy: &Value, tag: &str, skip: &[&str]) -> usize {
    let mut bad = 0usize;
    let st = src["tables"].as_array().cloned().unwrap_or_default();
    let ct = copy["tables"].as_array().cloned().unwrap_or_default();
    if st.len() != ct.len() {
        println!("   ✗ 表数量不同：{} vs {}", st.len(), ct.len());
        return 1;
    }
    for (a, b) in st.iter().zip(ct.iter()) {
        let an = a["name"].as_str().unwrap_or("?");
        let ar = a["rows"].as_array().map(|x| x.len()).unwrap_or(0);
        let br = b["rows"].as_array().map(|x| x.len()).unwrap_or(0);
        if an != b["name"].as_str().unwrap_or("?") || ar != br {
            println!("   ✗ {an}：行数 {ar} vs {br}");
            bad += 1;
            continue;
        }
        if ar == 0 {
            continue;
        }
        // 比对时把「预期就会变」的表整个跳过（例如 B 场景里改过的标题）。
        if skip.contains(&an) {
            println!("   ~ {an}：{ar} 行（{tag}预期变化，跳过内容比对）");
            continue;
        }
        if norm(a) == norm(b) {
            println!("   ✓ {an}：{ar} 行一致");
        } else {
            println!("   ✗ {an}：{ar} 行内容不一致");
            if let (Some(x0), Some(y0)) = (a["rows"].as_array(), b["rows"].as_array()) {
                for (i, (x, y)) in x0.iter().zip(y0.iter()).enumerate() {
                    if norm(x) != norm(y) {
                        println!("      首个差异在第 {} 行", i + 1);
                        break;
                    }
                }
            }
            bad += 1;
        }
    }
    bad
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let client = args.get(1).cloned().unwrap_or_else(|| "trae-cn".into());
    let yes = args.iter().any(|a| a == "--yes");
    let log = |m: &str| println!("   {m}");

    let sessions = match trae_export::list_sessions(&client) {
        Ok(v) => v,
        Err(e) => {
            println!("[write] list_sessions 失败：{e}");
            std::process::exit(2);
        }
    };
    // 挑一条中等体量的：太小碰不到大字段，太大（164 条消息 / 74 MB）跑一轮太久。
    let mut sorted: Vec<_> = sessions.iter().collect();
    sorted.sort_by_key(|s| s.messages);
    let sid = match args.get(2).filter(|s| !s.is_empty()) {
        Some(s) => s.clone(),
        None => match sorted.iter().find(|s| s.messages >= 20).or_else(|| sorted.last()) {
            Some(s) => s.id.clone(),
            None => {
                println!("[write] 没有可用会话");
                std::process::exit(3);
            }
        },
    };
    let n_msg = sorted.iter().find(|s| s.id == sid).map(|s| s.messages).unwrap_or(-1);
    println!("[write] 源会话 {sid}（{n_msg} 条消息）");

    let src = match trae_session_edit::edit_doc(&client, &sid) {
        Ok(d) => d,
        Err(e) => {
            println!("[write] edit_doc 失败：{e}");
            std::process::exit(4);
        }
    };
    let src_ids = collect_ids(&src);
    println!(
        "[write] 源 doc：{} 张表，大字段引用 {}，id 值 {} 个",
        src["tables"].as_array().map(|a| a.len()).unwrap_or(0),
        src["refCount"],
        src_ids.len()
    );
    let Some((bref_table, bref_row, bref_col, bref_pk, bref_bytes)) = biggest_ref(&src) else {
        println!("[write] 这条会话没有大字段引用，换一条消息更多的会更能说明问题");
        std::process::exit(5);
    };
    println!(
        "[write] 最大的大字段：{bref_table} 第 {} 行 .{bref_col}，{} 字节",
        bref_row + 1,
        bref_bytes
    );

    if !yes {
        println!("\n[write] 预演结束（未写库）。加 `--yes` 才会真的执行：");
        println!("  A) insert 克隆 → 比对 → 删副本");
        println!("  B) overwrite 只改标题（只标 chat_session 一张表）→ 验证大表未被重写 → 改回标题");
        return;
    }

    let mut bad = 0usize;

    // ======================= 场景 A：新增（insert） =======================
    println!("\n[write] ===== A) insert 克隆 =====");
    let saved = match trae_session_edit::edit_save(&src, "insert", None, None, Some(&log)) {
        Ok(v) => v,
        Err(e) => {
            println!("[write] edit_save 失败：\n{e}");
            std::process::exit(6);
        }
    };
    let copy_sid = saved["sessionId"].as_str().unwrap_or("").to_string();
    println!(
        "[write] 副本 {copy_sid}：写入 {} 行，重写 {}/{} 页",
        saved["rows"], saved["changedPages"], saved["pages"]
    );

    let copy = match trae_session_edit::edit_doc(&client, &copy_sid) {
        Ok(d) => d,
        Err(e) => {
            println!("[write] 读回副本失败：{e}\n副本 {copy_sid} 未删除，请手动清理");
            std::process::exit(7);
        }
    };
    println!("[write] 逐表比对（id 无关归一化）：");
    bad += compare_tables(&src, &copy, "", &[]);

    // (3) 新旧 id 集合必须不相交 —— 撞了就是 UNIQUE 冲突或跨会话串数据
    let copy_ids = collect_ids(&copy);
    let overlap: Vec<&String> = src_ids.intersection(&copy_ids).collect();
    if overlap.is_empty() {
        println!("   ✓ 新旧 id 集合不相交（{} 个全新 id）", copy_ids.len());
    } else {
        println!("   ✗ 有 {} 个 id 被原样留下：{:?}", overlap.len(), &overlap[..overlap.len().min(5)]);
        bad += 1;
    }

    // (4) 最大的大字段全文比对
    if let Some(cpk) = table(&copy, &bref_table)
        .and_then(|t| t["rows"].as_array())
        .and_then(|r| r.get(bref_row))
        .and_then(|r| r.get(&bref_col))
        .and_then(|v| v.get("$jsonRef"))
        .and_then(|m| m.get("pk"))
        .cloned()
    {
        let a = trae_session_edit::edit_cell(&client, &sid, &bref_table, &bref_col, &bref_pk);
        let b = trae_session_edit::edit_cell(&client, &copy_sid, &bref_table, &bref_col, &cpk);
        match (a, b) {
            (Ok(x), Ok(y)) => {
                if x["value"] == y["value"] {
                    println!("   ✓ 最大大字段全文一致（{} 字节，逐字段相等）", x["bytes"]);
                } else {
                    println!("   ✗ 最大大字段全文不一致：{} vs {} 字节", x["bytes"], y["bytes"]);
                    bad += 1;
                }
            }
            (x, y) => {
                println!("   ✗ 大字段拉取失败：{:?} / {:?}", x.err(), y.err());
                bad += 1;
            }
        }
    } else {
        println!("   ✗ 副本里找不到对应的大字段引用");
        bad += 1;
    }

    // 副本自己也得能通过校验（说明引用结构是自洽的）
    let rep = trae_session_edit::validate(&copy);
    if rep["ok"] == json!(true) {
        println!("   ✓ 副本通过分级校验（0 处硬性问题）");
    } else {
        println!("   ✗ 副本校验未通过：{}", rep["errors"]);
        bad += 1;
    }

    // 上一个场景刚把客户端拉起来，立刻再杀会撞上 Electron 的**启动竞态**：
    // 产品删除链路只等 10 s，实测会报「客户端未能退出，无法替换数据库」。
    // 探针先用产品自己的 `quit_client` 给足退出时间，再走删除链路 ——
    // 这样测的是「删除」本身，而不是「退出超时」。
    println!("\n[write] 先等刚被重启的客户端退出（最多 60 s）…");
    if let Some(c) = trae_discover::get_client(&client) {
        let out = trae_switch::quit_client(c, 60_000);
        println!(
            "   退出结果：ok={}，结束 {} 轮，命中 {} 个映像，残留 {}，被重新拉起={}",
            out.ok,
            out.attempts,
            out.killed.len(),
            out.remaining.len(),
            out.restarted
        );
        if !out.ok {
            println!("   {}", out.hint("Trae 客户端"));
        }
    }

    println!("\n[write] 删除副本…");
    match trae_delete::delete_sessions(&client, &[copy_sid.clone()], Some(&log)) {
        Ok(_) => println!("[write] 副本已删除"),
        Err(e) => println!("[write] 删除副本失败：{e}\n请手动清理会话 {copy_sid}"),
    }

    // ======================= 场景 B：就地覆写（overwrite） =======================
    println!("\n[write] ===== B) overwrite 只改标题 =====");
    let orig_title = src["title"].as_str().unwrap_or("").to_string();
    let probe_title = format!("{orig_title} [T46探针]");
    let mut edited = src.clone();
    if let Some(t) = edited["tables"]
        .as_array_mut()
        .and_then(|a| a.iter_mut().find(|t| t["name"] == json!("chat_session")))
    {
        if let Some(r) = t["rows"].as_array_mut().and_then(|a| a.first_mut()) {
            r["session_title"] = json!(probe_title);
        }
    } else {
        println!("[write] doc 里没有 chat_session，跳过场景 B");
        std::process::exit(9);
    }

    let only = vec!["chat_session".to_string()];
    let s1 = match trae_session_edit::edit_save(&edited, "overwrite", Some(&only), None, Some(&log)) {
        Ok(v) => v,
        Err(e) => {
            println!("[write] 覆写失败：\n{e}");
            std::process::exit(10);
        }
    };
    println!(
        "[write] 覆写完成：写入 {} 行，清除旧数据 {} 行，重写 {}/{} 页",
        s1["rows"], s1["removedRows"], s1["changedPages"], s1["pages"]
    );
    let skipped: Vec<String> = s1["skippedTables"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    println!("[write] 原样保留的 {} 张表：{}", skipped.len(), skipped.join(", "));
    if skipped.iter().any(|s| s == "chat_message_task") && skipped.iter().any(|s| s == "history_v2") {
        println!("   ✓ 最大的那几张表（含 chat_message_task）确实没被重写");
    } else {
        println!("   ✗ 大表被重写了 —— 「局部修改」没生效");
        bad += 1;
    }

    let after = trae_session_edit::edit_doc(&client, &sid).expect("读回原会话");
    if after["title"].as_str() == Some(probe_title.as_str()) {
        println!("   ✓ 标题已改：{}", after["title"]);
    } else {
        println!("   ✗ 标题没改上：{:?}", after["title"]);
        bad += 1;
    }
    // 除标题外，其余内容必须一模一样。
    //
    // ⚠️ `fts_session_title` 传进 skip 是**对的、不是放水**：标题变了，检索标题条目
    //    当然要跟着变 —— `trg_session_title_ai`（AFTER INSERT ON chat_session）
    //    会自动用**新标题**补一条，所以这里两边必然不同。
    println!("[write] 覆写后其余内容比对：");
    bad += compare_tables(&src, &after, "", &["chat_session", "fts_session_title"]);

    // 改回标题
    println!("\n[write] 复原标题…");
    let mut restore = after.clone();
    if let Some(t) = restore["tables"]
        .as_array_mut()
        .and_then(|a| a.iter_mut().find(|t| t["name"] == json!("chat_session")))
    {
        if let Some(r) = t["rows"].as_array_mut().and_then(|a| a.first_mut()) {
            r["session_title"] = json!(orig_title);
        }
    }
    match trae_session_edit::edit_save(&restore, "overwrite", Some(&only), None, Some(&log)) {
        Ok(_) => {
            let back = trae_session_edit::edit_doc(&client, &sid).expect("读回");
            if back["title"].as_str() == Some(orig_title.as_str()) {
                println!("   ✓ 标题已复原：{orig_title}");
            } else {
                println!("   ✗ 标题复原失败：{:?}", back["title"]);
                bad += 1;
            }
        }
        Err(e) => {
            println!("   ✗ 复原失败（请手动把标题改回「{orig_title}」）：{e}");
            bad += 1;
        }
    }

    println!("\n[write] 结论：{bad} 处不一致");
    if bad == 0 {
        println!("[write] 往返全部通过 —— 写入路径（大字段回查 / id 重映射 / 脏表跳过 / 增量回写）验证通过");
    } else {
        std::process::exit(8);
    }
}
