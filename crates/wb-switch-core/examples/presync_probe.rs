//! **只读**探针：解密「同步前备份」，分别 dump「不合并 WAL」与「合并 WAL」两种状态，
//! 用来判定同步当时**实际读到**的源会话长什么样、以及 WAL 里到底有什么。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example presync_probe -- solo-cn

use std::path::{Path, PathBuf};

/// 自动挑 `import_backup` 下最新的 `*-sync-*` 目录（同步会写下
/// `{client}-sync-{时间戳}` 备份，含同步前的主库 + WAL）。
fn newest_sync_backup() -> Option<PathBuf> {
    let base = wb_switch_core::modules::config::store_dir()
        .join("trae")
        .join("import_backup");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&base).ok()? {
        let p = e.ok()?.path();
        if !p.file_name()?.to_string_lossy().contains("-sync-") {
            continue;
        }
        let t = std::fs::metadata(&p).ok()?.modified().ok()?;
        if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
            best = Some((t, p));
        }
    }
    best.map(|(_, p)| p)
}

fn dump(conn: &rusqlite::Connection, tag: &str) {
    let total: i64 = conn
        .query_row("select count(*) from chat_message", [], |r| r.get(0))
        .unwrap_or(-1);
    println!("\n########## [{}] 全库 chat_message = {}", tag, total);
    for sid in ["6ac3a1a461373ef8900b49cd", "2908be72bbcf78fa054f6577"] {
        let n: i64 = conn
            .query_row(
                "select count(*) from chat_message where session_id=?",
                [sid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        let nt: i64 = conn
            .query_row(
                "select count(*) from chat_turn where session_id=?",
                [sid],
                |r| r.get(0),
            )
            .unwrap_or(-1);
        println!("\n--- sid {}  messages={} turns={}", &sid[..8], n, nt);
        let mut st = conn
            .prepare(
                "select message_id, message_type, message_role, message_index, \
                 reply_to_message_id, created_at from chat_message where session_id=? order by message_index",
            )
            .unwrap();
        let rows = st
            .query_map([sid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .unwrap();
        for row in rows.flatten() {
            println!(
                "   idx={:<2} {:<8} {:<9} mid={} reply={} created={}",
                row.3, row.1, row.2, row.0, row.4, row.5
            );
        }
        let mut st2 = conn
            .prepare(
                "select turn_id, reply_to_message_id, response_message_id from chat_turn \
                 where session_id=? order by id",
            )
            .unwrap();
        let rows2 = st2
            .query_map([sid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap();
        for row in rows2.flatten() {
            println!("   TURN {} reply={} resp={}", row.0, row.1, row.2);
        }
    }
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let key = wb_switch_core::modules::trae_memory_scan::load_saved_key(&client).expect("无密钥");
    let dir_owned: PathBuf = match std::env::args().nth(2) {
        Some(p) => PathBuf::from(p),
        None => match newest_sync_backup() {
            Some(p) => p,
            None => {
                println!(
                    "没找到同步备份目录。显式传入：\n  \
                     cargo run -p wb-switch-core --example presync_probe -- solo-cn <备份目录>"
                );
                return;
            }
        },
    };
    let dir: &Path = dir_owned.as_path();
    let src = dir.join("database.db");
    let wal = dir.join("database.db-wal");
    let out = std::env::temp_dir().join("tw-presync-plain.db");
    let _ = std::fs::remove_file(&out);

    let rep = wb_switch_core::modules::trae_decrypt::decrypt_database(&src, &key, &out, None)
        .expect("解密失败");
    println!("解密完成：{} 页；WAL 大小 {}", rep.pages, std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0));

    {
        let conn = rusqlite::Connection::open(&out).unwrap();
        dump(&conn, "同步当时真实读到的内容（仅主库，未合并 WAL）");
    }
    let merged =
        wb_switch_core::modules::trae_delete::merge_wal_into_plain(&out, &wal, &key).unwrap_or(0);
    println!("\n>>> 合并 WAL 帧数 = {}", merged);
    {
        let conn = rusqlite::Connection::open(&out).unwrap();
        dump(&conn, "客户端眼中的内容（主库 + WAL）");
    }
}
