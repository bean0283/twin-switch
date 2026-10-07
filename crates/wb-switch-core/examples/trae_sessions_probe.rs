//! 只读探针：列出 `reader_plain_path()`（快照副本 + 合并 WAL，即客户端真正读到的数据）
//! 里的全部会话，并打印实时库 / WAL / 快照 / view 的时间戳，用于排查
//! 「客户端里删了会话，工具刷新后还在」这类问题。
//!
//! **全程只读**，不触碰实时库。
//!
//! ```bash
//! cargo run -p wb-switch-core --example trae_sessions_probe -- solo-cn
//! ```

use rusqlite::{Connection, OpenFlags};
use std::path::Path;

fn stamp(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(md) => {
            let ns = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("size={} mtime_ns={}", md.len(), ns)
        }
        Err(e) => format!("<{e}>"),
    }
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());

    // 实时库位置
    if let Some(c) = wb_switch_core::modules::trae_discover::get_client(&client) {
        let db = wb_switch_core::modules::trae_discover::database_path(c);
        let wal = std::path::PathBuf::from(format!("{}-wal", db.to_string_lossy()));
        println!("实时库      {}", stamp(&db));
        println!("实时库 WAL  {}", stamp(&wal));
    }
    let snap = wb_switch_core::modules::trae_export::decrypted_db_path(&client);
    let view = wb_switch_core::modules::trae_export::decrypted_view_path(&client);
    let meta = wb_switch_core::modules::trae_export::snapshot_meta_path(&client);
    println!("解密快照    {}", stamp(&snap));
    println!("读取视图    {}", stamp(&view));
    if let Some(m) = wb_switch_core::modules::trae_export::read_snapshot_meta(&client) {
        println!(
            "快照元信息  src_size={} src_mtime_ns={} hdr_page_count={} created_ms={}",
            m.src_size, m.src_mtime_ns, m.hdr_page_count, m.created_ms
        );
    }
    let _ = meta;

    let path = match wb_switch_core::modules::trae_export::reader_plain_path(&client) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("读取视图失败：{e}");
            return;
        }
    };
    println!("\n== 实际读取：{} ==", path.display());
    let c = match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("打开失败：{e}");
            return;
        }
    };
    let n: i64 = c
        .query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0))
        .unwrap_or(-1);
    println!("chat_session 行数 = {n}");
    println!("\nrowid  session_id                  title                           created_at  updated_at");
    let mut stmt = c
        .prepare(
            "SELECT rowid, session_id, session_title, created_at, updated_at \
             FROM chat_session ORDER BY rowid",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, rusqlite::types::Value>(3)?,
                r.get::<_, rusqlite::types::Value>(4)?,
            ))
        })
        .unwrap();
    for r in rows.flatten() {
        println!(
            "{:>5}  {}  {:<30}  {:?}  {:?}",
            r.0, r.1, r.2, r.3, r.4
        );
    }
}
