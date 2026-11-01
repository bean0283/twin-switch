//! WorkBuddy 会话包：跨机器迁移会话。
//!
//! ## 与 Trae 包的差别（结构完全不同，不能共用打包代码）
//!
//! Trae 的会话全在 SQLite 的多张表里；WorkBuddy 是**「库里的行 + 独立 jsonl 正文」**：
//! - 元数据：`~/.workbuddy/workbuddy.db` 的 `sessions` 表
//! - 正文：`~/.workbuddy/projects/<工作区>/<cid>.jsonl`
//!
//! 所以包的形状也不同：
//!
//! ```text
//! twinswitch-sessions-wb_20261101_091800.zip
//! ├── pack.db                  # 只含 sessions 表选中行的裁剪库
//! ├── chats/<工作区>/<cid>.jsonl
//! └── manifest.json
//! ```
//!
//! `chats/` **刻意与 `projects/` 同形**：导入时把它的根当成 `projects/`，
//! `target_body_path_from` 就能算出正确的工作区子目录（否则正文全落到根下，工作区归类丢失）。
//!
//! ## 沿用现有写入路径
//!
//! 导入复用 [`super::workbuddy_sessions::write_copies`]（新 UUID + 新行 + 正文 cid 替换），
//! 与「本机跨账号复制」是同一条代码路径，只是把「源正文的根」换成包里的 `chats/`。
//!
//! ⚠️ 绝不写 `edge-sync-mapping*.db` —— 那是云端迁移映射，
//! 写进去会让 edge-sync 判定「已迁移」从而**跳过上传**（见 `workbuddy_sessions` 模块头）。

use crate::modules::session_pack::{self, Manifest, ManifestSession, PACK_FORMAT, PACK_VERSION};
use crate::modules::workbuddy_source;
use crate::modules::workbuddy_sessions;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 导出会话包。`ids` 为 `None` 时导出全部（未删除的）会话。
pub fn export_pack(
    ids: Option<&[String]>,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    let dir = crate::modules::trae_export::export_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    // ⚠️ 临时目录名必须带 pid + 纳秒：只靠「秒级时间戳」时，并发导出
    // （或同一秒内连点两次）会共用同一个目录，第二个直接撞主键。
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = dir.join(format!(".wb-pack-build-{stamp}-{}-{uniq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("创建临时目录失败: {e}"))?;

    let all = workbuddy_source::list_sessions()?;
    let picked: Vec<_> = match ids {
        Some(want) => all.iter().filter(|s| want.iter().any(|i| i == &s.id)).collect(),
        None => all.iter().filter(|s| !s.deleted).collect(),
    };
    if picked.is_empty() {
        let _ = std::fs::remove_dir_all(&work);
        return Err("没有可导出的 WorkBuddy 会话".into());
    }

    // 1) 裁剪库：只带 sessions 表里的选中行
    let snap = workbuddy_source::snapshot_db()?;
    let pack_db = work.join("pack.db");
    let rows = {
        let pack = Connection::open(&pack_db).map_err(|e| format!("创建包数据库失败: {e}"))?;
        let _ = pack.pragma_update(None, "journal_mode", "DELETE");
        pack.execute("ATTACH DATABASE ?1 AS src", [snap.to_string_lossy().as_ref()])
            .map_err(|e| format!("挂载源库失败: {e}"))?;
        let ddl: Option<String> = pack
            .query_row(
                "SELECT sql FROM src.sqlite_master WHERE type='table' AND name='sessions'",
                [],
                |r| r.get(0),
            )
            .ok();
        let Some(ddl) = ddl else {
            let _ = std::fs::remove_dir_all(&work);
            return Err("WorkBuddy 会话库里没有 sessions 表".into());
        };
        let stmt = ddl.replacen("CREATE TABLE", "CREATE TABLE IF NOT EXISTS", 1);
        pack.execute_batch(&stmt).map_err(|e| format!("建表失败: {e}"))?;
        let in_list = picked
            .iter()
            .map(|s| format!("'{}'", s.id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let n = pack
            .execute(
                &format!("INSERT INTO main.sessions SELECT * FROM src.sessions WHERE id IN ({in_list})"),
                [],
            )
            .map_err(|e| format!("复制 sessions 行失败: {e}"))?;
        let _ = pack.execute_batch("DETACH DATABASE src");
        n
    };

    // 2) 正文：按「projects 相对路径」放进 chats/
    let mut manifest = Manifest {
        format: PACK_FORMAT.to_string(),
        format_version: PACK_VERSION,
        exported_at: chrono::Local::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        source_client: "workbuddy".to_string(),
        source_label: "WorkBuddy".to_string(),
        source_uid: None,
        sessions: Vec::new(),
        workspaces: Vec::new(),
        tables: vec!["sessions".to_string()],
        kind: "workbuddy".to_string(),
    };
    let mut uids: Vec<String> = Vec::new();
    let mut missing_body: Vec<String> = Vec::new();
    let projects = crate::modules::workbuddy_auth::projects_dir();

    for s in &picked {
        let body = workbuddy_source::find_jsonl(&s.id);
        let mut rel_dir = String::new();
        if let Some(src) = &body {
            // 相对 projects/ 的子目录（工作区名），没有子目录就放 chats 根。
            if let Ok(r) = src
                .parent()
                .ok_or(())
                .and_then(|p| p.strip_prefix(&projects).map_err(|_| ()))
            {
                rel_dir = r.to_string_lossy().replace('\\', "/");
            }
            let dst = if rel_dir.is_empty() {
                work.join("chats").join(format!("{}.jsonl", s.id))
            } else {
                work.join("chats").join(&rel_dir).join(format!("{}.jsonl", s.id))
            };
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
            }
            std::fs::copy(src, &dst).map_err(|e| format!("复制正文失败: {e}"))?;
        } else {
            missing_body.push(s.id.clone());
        }
        if !s.user_id.is_empty() && !uids.contains(&s.user_id) {
            uids.push(s.user_id.clone());
        }
        if !s.cwd.is_empty() && !manifest.workspaces.contains(&s.cwd) {
            manifest.workspaces.push(s.cwd.clone());
        }
        manifest.sessions.push(ManifestSession {
            session_id: s.id.clone(),
            title: s.title.clone(),
            messages: 0, // WB 不在这里数消息；正文里自会体现
            workspace: rel_dir.clone(),
            workspace_path: s.cwd.clone(),
            updated: String::new(),
            owner_uid: s.user_id.clone(),
            has_body: body.is_some(),
        });
    }
    manifest.source_uid = uids.first().cloned();
    if !missing_body.is_empty() {
        log(&format!(
            "{} 个会话没有正文文件（导入后会是空会话）",
            missing_body.len()
        ));
    }

    std::fs::write(
        work.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("生成 manifest 失败: {e}"))?,
    )
    .map_err(|e| format!("写入 manifest 失败: {e}"))?;

    // 3) 打包
    let fname = format!("twinswitch-sessions-wb_{stamp}.zip");
    let path = dir.join(&fname);
    let file = std::fs::File::create(&path).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zw = zip::ZipWriter::new(file);
    let opts = zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    write_dir_to_zip(&mut zw, &work, &work, opts)?;
    zw.finish().map_err(|e| format!("完成 zip 失败: {e}"))?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&work);

    log(&format!(
        "已导出 {} 个 WorkBuddy 会话（{:.1} MB）",
        manifest.sessions.len(),
        size as f64 / 1_048_576.0
    ));
    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "filename": fname,
        "sessions": manifest.sessions.len(),
        "rows": rows,
        "bytes": size,
        "missingBody": missing_body,
        "manifest": manifest,
    }))
}

fn write_dir_to_zip<W: Write + std::io::Seek>(
    zw: &mut zip::ZipWriter<W>,
    root: &Path,
    cur: &Path,
    opts: zip::write::FileOptions,
) -> Result<(), String> {
    let entries = std::fs::read_dir(cur).map_err(|e| format!("读目录失败: {e}"))?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            write_dir_to_zip(zw, root, &p, opts)?;
            continue;
        }
        let rel = p
            .strip_prefix(root)
            .map_err(|_| "路径计算失败".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&p).map_err(|e| format!("读取 {rel} 失败: {e}"))?;
        zw.start_file(rel, opts).map_err(|e| format!("写入 zip 失败: {e}"))?;
        zw.write_all(&bytes).map_err(|e| format!("写入 zip 失败: {e}"))?;
    }
    Ok(())
}

/// 预检（只读）。
pub fn preview_pack(zip_path: &str) -> Result<Value, String> {
    let dir = session_pack::unpack_to_temp(zip_path, "wb-preview")?;
    let out = (|| -> Result<Value, String> {
        let manifest = session_pack::read_manifest(&dir, PACK_FORMAT, PACK_VERSION)?;
        session_pack::ensure_kind(&manifest, "workbuddy")?;
        let pack_db = dir.join("pack.db");
        if !pack_db.is_file() {
            return Err("包里缺少 pack.db（不是有效的 WorkBuddy 会话包）".into());
        }
        let conn = Connection::open(&pack_db).map_err(|e| format!("打开包数据库失败: {e}"))?;
        let ids: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT id FROM sessions")
                .map_err(|e| format!("读取包内会话失败: {e}"))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| format!("读取包内会话失败: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取包内会话失败: {e}"))?;
            drop(stmt);
            rows
        };
        let _ = conn.close();
        // 正文是否齐全：缺正文的会话导进去是空壳，必须让用户提前知道。
        let chats = dir.join("chats");
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| find_body_in(&chats, id).is_none())
            .cloned()
            .collect();
        Ok(json!({
            "ok": true,
            "manifest": manifest,
            "sessionCount": ids.len(),
            "sessionIds": ids,
            "missingBody": missing,
            "workspacesMissing": manifest
                .workspaces
                .iter()
                .filter(|w| !Path::new(w).exists())
                .cloned()
                .collect::<Vec<_>>(),
        }))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// 在包内 `chats/` 下找某个会话的正文（与 `find_jsonl` 同一条规则：根或一级子目录）。
fn find_body_in(chats: &Path, cid: &str) -> Option<PathBuf> {
    let name = format!("{cid}.jsonl");
    let direct = chats.join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    for e in std::fs::read_dir(chats).ok()?.flatten() {
        let c = e.path().join(&name);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// 导入会话包到指定账号。
///
/// `dst_uid`：目标账号 uid（WorkBuddy 的 `sessions.user_id`）。
pub fn import_pack(
    zip_path: &str,
    dst_uid: &str,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    if dst_uid.trim().is_empty() {
        return Err("缺少目标账号 uid".into());
    }
    let dir = session_pack::unpack_to_temp(zip_path, "wb-import")?;
    let out = (|| -> Result<Value, String> {
        let manifest = session_pack::read_manifest(&dir, PACK_FORMAT, PACK_VERSION)?;
        session_pack::ensure_kind(&manifest, "workbuddy")?;
        let pack_db = dir.join("pack.db");
        if !pack_db.is_file() {
            return Err("包里缺少 pack.db（不是有效的 WorkBuddy 会话包）".into());
        }
        let chats = dir.join("chats");

        // 从包里读出行，组装成 write_copies 要的 plan。
        let conn = Connection::open(&pack_db).map_err(|e| format!("打开包数据库失败: {e}"))?;
        let mut stmt = conn
            .prepare("SELECT id, ifnull(title,''), ifnull(user_id,'') FROM sessions")
            .map_err(|e| format!("读取包内会话失败: {e}"))?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("读取包内会话失败: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取包内会话失败: {e}"))?;
        drop(stmt);
        let _ = conn.close();
        if rows.is_empty() {
            return Err("包里没有任何会话".into());
        }
        // 源账号：包里第一条会话的归属（多账号混装时只用于关联登记，不影响导入）。
        let src_uid = rows
            .iter()
            .map(|(_, _, u)| u.clone())
            .find(|u| !u.is_empty())
            .unwrap_or_default();
        log(&format!("准备导入 {} 个 WorkBuddy 会话…", rows.len()));

        let mut plan: Vec<workbuddy_sessions::CopyItem> = Vec::new();
        let mut no_body: Vec<String> = Vec::new();
        for (cid, title, _) in &rows {
            match find_body_in(&chats, cid) {
                Some(p) => plan.push(workbuddy_sessions::CopyItem {
                    cid: cid.clone(),
                    title: title.clone(),
                    src_path: p,
                }),
                // 没正文的会话导进去也是空壳，明确跳过并报出来（不静默丢）。
                None => no_body.push(cid.clone()),
            }
        }
        if plan.is_empty() {
            return Err("包里的会话都没有正文文件，无法导入".into());
        }

        // 复用「本机跨账号复制」的写入路径，只把正文的根换成包里的 chats/。
        let r = workbuddy_sessions::import_from_pack(&plan, &chats, &src_uid, dst_uid, Some(&log))?;
        Ok(json!({
            "ok": true,
            "imported": r.get("copied").cloned().unwrap_or(json!(0)),
            "skipped": r.get("skipped").cloned().unwrap_or(json!([])),
            "noBody": no_body,
            "manifest": manifest,
        }))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机探针（`#[ignore]`）：导出真实的 WorkBuddy 会话包，再预检读回来。
    ///
    /// 只读源库、只写导出目录，不动任何会话数据。
    #[test]
    #[ignore]
    fn wb_pack_export_probe() {
        let r = export_pack(None, Some(&|m| println!("   {m}"))).expect("导出失败");
        println!("包路径：{}", r["path"]);
        println!("会话数：{}  行数：{}", r["sessions"], r["rows"]);
        if let Some(mb) = r["missingBody"].as_array() {
            println!("缺正文：{} 个", mb.len());
        }
        assert!(r["sessions"].as_u64().unwrap_or(0) > 0, "至少要导出 1 个会话");

        let p = r["path"].as_str().unwrap();
        let prev = preview_pack(p).expect("预检失败");
        println!("预检会话数：{}", prev["sessionCount"]);
        println!("预检缺正文：{}", prev["missingBody"]);
        assert_eq!(
            prev["sessionCount"].as_u64().unwrap_or(0),
            r["sessions"].as_u64().unwrap_or(0),
            "包内会话数应与导出一致"
        );
    }

    /// 包内正文必须都在 `chats/` 下且能被 `find_body_in` 找到。
    #[test]
    #[ignore]
    fn wb_pack_bodies_are_findable() {
        let r = export_pack(None, None).expect("导出失败");
        let dir = session_pack::unpack_to_temp(r["path"].as_str().unwrap(), "wb-probe")
            .expect("解包失败");
        let chats = dir.join("chats");
        let manifest = session_pack::read_manifest(&dir, PACK_FORMAT, PACK_VERSION).unwrap();
        let mut missing = 0;
        for s in &manifest.sessions {
            if s.has_body && find_body_in(&chats, &s.session_id).is_none() {
                missing += 1;
                println!("找不到正文：{}", s.session_id);
            }
        }
        println!("包里 {} 个会话，找不到正文的 {} 个", manifest.sessions.len(), missing);
        assert_eq!(missing, 0, "标了 has_body 的会话必须能在包里找到正文");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
