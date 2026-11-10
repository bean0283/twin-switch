//! Qoder 会话包：导出 / 预检 / 导入（跨机器迁移用）。
//!
//! ## 和「会话包」另外两个产品的关系
//!
//! Trae 与 WorkBuddy 的包是**裁剪后的 SQLite**（`pack.db`），因为它们的存储就是
//! 多表 SQLite，`INSERT ... SELECT` 能零误差搬运。Qoder 这边改用 **JSON 逐行**，
//! 理由：
//!
//! 1. Qoder 的 `chat_sessions` 有 35 列、`chat_session_messages` 有 24 列，其中
//!    大半是客户端内部状态（`side_chat_*`、`payload_schema_version` 之类）。
//!    手写 `INSERT ... SELECT` 等于把 59 个列名硬编码两遍，上游一改列就静默错位。
//! 2. 按**列名**读写，导入时只写目标库里真实存在的列 —— 两台机器客户端版本
//!    不一致也能导，多出来的列直接忽略，而不是整个失败。
//! 3. 体积：zip 对 JSON 的压缩率不输 SQLite，包大小差别可以忽略。
//!
//! ## 包结构
//!
//! ```text
//! twinswitch-qoder-global_20261110_124500.zip
//! ├── manifest.json                 # 版本、来源、清单（与另两个产品同一套结构）
//! └── sessions/<新 session_id>.json # { "session": {列→值}, "messages": [ {列→值}, … ] }
//! ```
//!
//! ## 导出时就分配新 id
//!
//! 包里的 `session_id` **不是**原会话的 id，而是新建的 uuid。这样：
//!
//! - 导入回同一台机器不会覆盖原会话（用户「导出来看看」是安全的）；
//! - 导入侧 `INSERT` 不需要再改写主键，少一处出错点。
//!
//! 原 id 记在 manifest 的 `sessionId` 里，排障时能对上。

use rusqlite::types::Value as SqlVal;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::PathBuf;

use super::session_pack::{self, Manifest, ManifestSession, PACK_FORMAT, PACK_VERSION};
use crate::modules::qoder_realm::QoderRegion;
use crate::modules::qoder_sessions;

const PACK_KIND: &str = "qoder";

fn export_dir() -> PathBuf {
    crate::modules::config::store_dir()
        .join("qoder")
        .join("export")
}

fn sql_to_json(v: SqlVal) -> Value {
    match v {
        SqlVal::Null => Value::Null,
        SqlVal::Integer(i) => json!(i),
        SqlVal::Real(f) => json!(f),
        SqlVal::Text(s) => Value::String(s),
        // 实测两张表没有 BLOB 列；真遇到就置空而不是塞 base64 ——
        // 编解码回来一旦出错，客户端打开会话会直接崩。
        SqlVal::Blob(_) => Value::Null,
    }
}

fn json_to_sql(v: &Value) -> SqlVal {
    match v {
        Value::Null => SqlVal::Null,
        Value::Bool(b) => SqlVal::Integer(*b as i64),
        Value::Number(n) => n
            .as_i64()
            .map(SqlVal::Integer)
            .or_else(|| n.as_f64().map(SqlVal::Real))
            .unwrap_or(SqlVal::Null),
        Value::String(s) => SqlVal::Text(s.clone()),
        // 数组 / 对象不该出现在这两张表的列里；出现了就按字符串存，至少不丢数据。
        other => SqlVal::Text(other.to_string()),
    }
}

/// `SELECT *` 的通用读法：列名 → 值，不依赖任何硬编码列序。
fn read_rows(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Map<String, Value>>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| format!("查询失败：{e}"))?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt
        .query_map(params, |r| {
            let mut m = Map::new();
            for (i, name) in names.iter().enumerate() {
                let v: SqlVal = r.get(i)?;
                m.insert(name.clone(), sql_to_json(v));
            }
            Ok(m)
        })
        .map_err(|e| format!("查询失败：{e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("读取行失败：{e}"))?);
    }
    Ok(out)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| format!("读取表结构失败：{e}"))?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("读取表结构失败：{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取表结构失败：{e}"))?;
    Ok(cols)
}

/// 导出会话包。`session_ids` 为 `None` 时导出该版本全部未删除会话。
pub fn export_pack(
    region: &str,
    session_ids: Option<&[String]>,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    let r = QoderRegion::parse(region).ok_or_else(|| format!("未知的 Qoder 版本：{region}"))?;
    let conn = qoder_sessions::open(r)?;

    let mut sessions = read_rows(
        &conn,
        "select * from chat_sessions where deleted_at is null order by updated_at desc",
        &[],
    )?;
    if let Some(ids) = session_ids {
        sessions.retain(|row| {
            row.get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| ids.iter().any(|i| i == s))
                .unwrap_or(false)
        });
    }
    if sessions.is_empty() {
        return Err("没有可导出的会话（可能是没勾选、或所选会话已被删除）".into());
    }

    let dir = export_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败：{e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = dir.join(format!(".pack-build-{stamp}-{}-{uniq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("sessions")).map_err(|e| format!("创建临时目录失败：{e}"))?;

    let mut manifest = Manifest {
        format: PACK_FORMAT.to_string(),
        format_version: PACK_VERSION,
        exported_at: chrono::Local::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        source_client: format!("qoder-{}", r.as_str()),
        source_label: format!("Qoder {}", r.display_name()),
        source_uid: None,
        sessions: Vec::new(),
        workspaces: Vec::new(),
        tables: vec!["chat_sessions".into(), "chat_session_messages".into()],
        kind: PACK_KIND.to_string(),
    };

    let mut total_rows = 0usize;
    for row in sessions {
        let old_id = row
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if old_id.is_empty() {
            continue;
        }
        let new_id = uuid::Uuid::new_v4().to_string();

        let mut sess = row.clone();
        sess.insert("session_id".into(), Value::String(new_id.clone()));
        // 指向别的会话的引用：指向自己就改成新 id，指向别人就置空
        // （否则导进来的会话会挂在一个不存在的 origin 上）。
        for key in ["origin_session_id", "owner_session_id"] {
            if let Some(v) = sess.get(key).and_then(|v| v.as_str()).map(str::to_string) {
                if v == old_id {
                    sess.insert(key.to_string(), Value::String(new_id.clone()));
                } else if !v.is_empty() {
                    sess.insert(key.to_string(), Value::Null);
                }
            }
        }
        // 导出的都是「活着」的会话，回收站状态不能跟着走。
        if sess.contains_key("deleted_at") {
            sess.insert("deleted_at".into(), Value::Null);
        }

        let mut msgs = read_rows(
            &conn,
            "select * from chat_session_messages where session_id = ?1 order by sequence, rowid",
            &[&old_id],
        )?;
        for m in msgs.iter_mut() {
            m.insert("session_id".into(), Value::String(new_id.clone()));
        }

        let title = sess
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let workspace = sess
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let updated = sess
            .get("updated_at")
            .and_then(|v| v.as_i64())
            .map(|ms| {
                chrono::DateTime::from_timestamp_millis(ms)
                    .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        let body = json!({ "session": sess, "messages": msgs });
        std::fs::write(
            work.join("sessions").join(format!("{new_id}.json")),
            serde_json::to_vec(&body).map_err(|e| format!("写入会话失败：{e}"))?,
        )
        .map_err(|e| format!("写入会话失败：{e}"))?;

        total_rows += 1 + msgs.len();
        if !workspace.is_empty() && !manifest.workspaces.contains(&workspace) {
            manifest.workspaces.push(workspace.clone());
        }
        manifest.sessions.push(ManifestSession {
            session_id: new_id,
            title: title.clone(),
            messages: msgs.len() as i64,
            workspace: workspace.clone(),
            workspace_path: workspace,
            updated,
            owner_uid: String::new(),
            has_body: !msgs.is_empty(),
        });
        log(&format!("已写入「{title}」（{} 条消息）", msgs.len()));
    }

    if manifest.sessions.is_empty() {
        let _ = std::fs::remove_dir_all(&work);
        return Err("没有导出任何会话".into());
    }

    std::fs::write(
        work.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("生成 manifest 失败：{e}"))?,
    )
    .map_err(|e| format!("写入 manifest 失败：{e}"))?;

    let fname = format!("twinswitch-qoder-{}_{stamp}.zip", r.as_str());
    let path = dir.join(&fname);
    let file = std::fs::File::create(&path).map_err(|e| format!("创建 zip 失败：{e}"))?;
    let mut zw = zip::ZipWriter::new(file);
    let opts =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("manifest.json", opts)
        .map_err(|e| format!("写入 zip 失败：{e}"))?;
    zw.write_all(
        &std::fs::read(work.join("manifest.json")).map_err(|e| format!("读取 manifest 失败：{e}"))?,
    )
    .map_err(|e| format!("写入 zip 失败：{e}"))?;
    let mut names: Vec<String> = manifest.sessions.iter().map(|s| s.session_id.clone()).collect();
    names.sort();
    for sid in names {
        let fp = work.join("sessions").join(format!("{sid}.json"));
        let bytes = std::fs::read(&fp).map_err(|e| format!("读取会话文件失败：{e}"))?;
        zw.start_file(format!("sessions/{sid}.json"), opts)
            .map_err(|e| format!("写入 zip 失败：{e}"))?;
        zw.write_all(&bytes).map_err(|e| format!("写入 zip 失败：{e}"))?;
    }
    zw.finish().map_err(|e| format!("完成 zip 失败：{e}"))?;

    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&work);
    log(&format!(
        "已导出 {} 个会话（{:.1} MB）",
        manifest.sessions.len(),
        size as f64 / 1_048_576.0
    ));
    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy(),
        "filename": fname,
        "sessions": manifest.sessions.len(),
        "rows": total_rows,
        "bytes": size,
        "workspaces": manifest.workspaces,
        "manifest": manifest,
    }))
}

/// 预检：只读包，不解压到正式目录、不碰客户端。
pub fn preview_pack(zip_path: &str) -> Result<Value, String> {
    let dir = session_pack::unpack_to_temp(zip_path, "qoder-preview")?;
    let res = (|| -> Result<Value, String> {
        let m = session_pack::read_manifest(&dir, PACK_FORMAT, PACK_VERSION)?;
        session_pack::ensure_kind(&m, PACK_KIND)?;
        let mut exists = 0usize;
        let mut titles: Vec<String> = Vec::new();
        for s in &m.sessions {
            if dir.join("sessions").join(format!("{}.json", s.session_id)).is_file() {
                exists += 1;
            }
            titles.push(s.title.clone());
        }
        // 字段名与 Trae / WorkBuddy 的预检结果对齐，前端那个弹窗才能三端共用。
        Ok(json!({
            "ok": true,
            "manifest": m,
            "sessionCount": m.sessions.len(),
            "sessionIds": m.sessions.iter().map(|s| s.session_id.clone()).collect::<Vec<_>>(),
            "missingBody": m
                .sessions
                .iter()
                .filter(|s| !s.has_body)
                .map(|s| s.title.clone())
                .collect::<Vec<_>>(),
            "workspacesMissing": [],
            "readableSessions": exists,
            "titles": titles,
            "workspaces": m.workspaces,
        }))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res
}

/// 导入会话包到指定版本。**写库前会关客户端并整库备份**。
pub fn import_pack(
    zip_path: &str,
    region: &str,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    let r = QoderRegion::parse(region).ok_or_else(|| format!("未知的 Qoder 版本：{region}"))?;
    let dir = session_pack::unpack_to_temp(zip_path, "qoder-import")?;
    let res = (|| -> Result<Value, String> {
        let m = session_pack::read_manifest(&dir, PACK_FORMAT, PACK_VERSION)?;
        session_pack::ensure_kind(&m, PACK_KIND)?;

        let _resume = qoder_sessions::close_client_for_write(r)?;
        let backup = qoder_sessions::backup_db(r)?;
        let conn = qoder_sessions::open_rw(r)?;

        let sess_cols = table_columns(&conn, "chat_sessions")?;
        let msg_cols = table_columns(&conn, "chat_session_messages")?;

        let mut imported = 0usize;
        let mut skipped: Vec<String> = Vec::new();
        let mut messages = 0usize;

        for s in &m.sessions {
            let fp = dir.join("sessions").join(format!("{}.json", s.session_id));
            if !fp.is_file() {
                skipped.push(format!("{}（包里没有正文）", s.title));
                continue;
            }
            let Ok(raw) = std::fs::read(&fp) else {
                skipped.push(format!("{}（读取失败）", s.title));
                continue;
            };
            let Ok(body) = serde_json::from_slice::<Value>(&raw) else {
                skipped.push(format!("{}（格式损坏）", s.title));
                continue;
            };
            let Some(sess) = body.get("session").and_then(|v| v.as_object()) else {
                skipped.push(format!("{}（缺少会话行）", s.title));
                continue;
            };

            // 会话 id 冲突：包里是新 uuid，理论上不会；真撞了就换成新的再写。
            let mut sess = sess.clone();
            let want_id = sess
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or(&s.session_id)
                .to_string();
            if id_taken(&conn, &want_id)? {
                let fresh = uuid::Uuid::new_v4().to_string();
                log(&format!("「{}」的 id 已存在，改用 {fresh}", s.title));
                sess.insert("session_id".into(), Value::String(fresh.clone()));
            }
            let final_id = sess
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            insert_row(&conn, "chat_sessions", &sess, &sess_cols)?;
            imported += 1;

            let rows = body
                .get("messages")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for mrow in rows {
                let Some(obj) = mrow.as_object() else { continue };
                let mut obj = obj.clone();
                obj.insert("session_id".into(), Value::String(final_id.clone()));
                insert_row(&conn, "chat_session_messages", &obj, &msg_cols)?;
                messages += 1;
            }
            log(&format!("已导入「{}」（{messages} 条消息累计）", s.title));
        }

        Ok(json!({
            "ok": imported > 0,
            "imported": imported,
            "messages": messages,
            "skipped": skipped,
            "backup": backup.display().to_string(),
            "region": r.as_str(),
            "note": "新会话已写入本机库（使用新 id，不会覆盖原有会话），客户端会自动重新启动",
        }))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res
}

fn id_taken(conn: &Connection, session_id: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            "select count(*) from chat_sessions where session_id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("查询会话失败：{e}"))?;
    Ok(n > 0)
}

/// 按**列名**插入一行；目标表没有的列自动忽略（跨版本导入的关键）。
fn insert_row(
    conn: &Connection,
    table: &str,
    row: &Map<String, Value>,
    allowed: &[String],
) -> Result<(), String> {
    let cols: Vec<&String> = row.keys().filter(|k| allowed.iter().any(|a| a == *k)).collect();
    if cols.is_empty() {
        return Err(format!("{table} 这一行没有任何可写入的列"));
    }
    let quoted: Vec<String> = cols.iter().map(|c| format!("\"{c}\"")).collect();
    let ph: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "insert into {table} ({}) values ({})",
        quoted.join(","),
        ph.join(",")
    );
    let vals: Vec<SqlVal> = cols.iter().map(|c| json_to_sql(&row[*c])).collect();
    let refs: Vec<&dyn rusqlite::ToSql> = vals.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, refs.as_slice())
        .map_err(|e| format!("写入 {table} 失败：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 值往返：TEXT / INTEGER / REAL / NULL 都不能变味。
    #[test]
    fn json_sql_roundtrip_keeps_types() {
        for v in [
            Value::Null,
            json!(12i64),
            json!(1.5f64),
            Value::String("会话".into()),
        ] {
            let back = sql_to_json(json_to_sql(&v));
            assert_eq!(back, v);
        }
    }

    /// 目标表缺列时按名字过滤 —— 跨客户端版本导入就靠这个。
    #[test]
    fn insert_row_filters_unknown_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("create table t (a text, b integer)").unwrap();
        let mut row = Map::new();
        row.insert("a".into(), Value::String("x".into()));
        row.insert("b".into(), json!(2));
        row.insert("c_unknown".into(), json!(true));
        let cols = vec!["a".to_string(), "b".to_string()];
        insert_row(&conn, "t", &row, &cols).unwrap();
        let got: (String, i64) = conn
            .query_row("select a, b from t", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(got, ("x".to_string(), 2));
    }
}
