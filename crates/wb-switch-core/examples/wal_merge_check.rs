//! **只读**实验探针：判定 `database disk image is malformed` 的来源。
//!
//! 三组对照：
//!   A. 旧快照 + 当前 WAL 合并（`reader_plain_path()` 直接拿到的视图）
//!   B. **强制刷新**快照后 + 当前 WAL 合并
//!   C. 现场把实时库 + WAL 复制一份，**刚解密完就合并**（模拟写路径）
//!
//! 每组都跑 `PRAGMA quick_check` 并只看 `server_history_info` 是否可读。
//!
//! 用法：`cargo run -p wb-switch-core --example wal_merge_check -- solo-cn`

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

fn quick_check(path: &Path) -> String {
    match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => {
            let qc: Result<String, _> =
                c.query_row("PRAGMA quick_check(5)", [], |r| r.get::<_, String>(0));
            let shr: Result<i64, _> = c.query_row(
                "SELECT count(*) FROM server_history_info WHERE session_id=?",
                ["e60bf5c302083b0ad28baba2"],
                |r| r.get(0),
            );
            format!("quick_check={qc:?}\n      server_history_info WHERE sid = {shr:?}")
        }
        Err(e) => format!("打开失败: {e}"),
    }
}

fn stamp(p: &Path) -> String {
    let (sz, mt) = match std::fs::metadata(p) {
        Ok(m) => (
            m.len(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0),
        ),
        Err(_) => (0, 0),
    };
    format!("size={sz} mtime_ms={mt}")
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wbmc-{}-{}", tag, std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let cfg = wb_switch_core::modules::trae_discover::get_client(&client).expect("客户端");
    let db = wb_switch_core::modules::trae_discover::database_path(cfg);
    let wal = PathBuf::from(format!("{}-wal", db.to_string_lossy()));
    let snap = wb_switch_core::modules::trae_export::decrypted_db_path(&client);

    println!("实时库   {}", stamp(&db));
    println!("实时 WAL {}", stamp(&wal));
    println!("解密快照 {}", stamp(&snap));
    println!("待合并帧 = {}", wb_switch_core::modules::trae_delete::wal_pending_frames(&wal));

    // ---- A：旧快照 + 当前 WAL（reader_plain_path 的现状）----
    let view = wb_switch_core::modules::trae_export::reader_plain_path(&client).expect("视图");
    println!("\n== A. reader_plain_path 视图 ==");
    println!("   {}  {}", view.display(), stamp(&view));
    println!("   {}", quick_check(&view));

    // ---- B：强制刷新快照后再取视图 ----
    println!("\n== B. ensure_decrypted 后重新取视图 ==");
    match wb_switch_core::modules::trae_export::ensure_decrypted(&client, None) {
        Ok(o) => println!("   ensure_decrypted ok: {o:?}"),
        Err(e) => println!("   ensure_decrypted 失败: {e}"),
    }
    println!("   解密快照 {}", stamp(&snap));
    let view2 = wb_switch_core::modules::trae_export::reader_plain_path(&client).expect("视图");
    println!("   {}  {}", view2.display(), stamp(&view2));
    println!("   {}", quick_check(&view2));

    // ---- C：现场复制实时库 + WAL，解密后立刻合并 ----
    println!("\n== C. 现场复制 → 解密 → 立刻合并（模拟写路径）==");
    let key = wb_switch_core::modules::trae_memory_scan::load_saved_key(&client).expect("密钥");
    let d = tmp("live");
    let db_c = d.join("database.db");
    let wal_c = d.join("database.db-wal");
    let _ = std::fs::copy(&db, &db_c);
    let _ = std::fs::copy(&wal, &wal_c);
    println!("   实时库副本 {}", stamp(&db_c));
    println!("   WAL 副本   {}", stamp(&wal_c));
    let plain = d.join("plain.db");
    let t0 = std::time::Instant::now();
    match wb_switch_core::modules::trae_decrypt::decrypt_database(&db_c, &key, &plain, None) {
        Ok(r) => println!("   解密完成 pages={} 用时 {:?}", r.pages, t0.elapsed()),
        Err(e) => {
            println!("   解密失败: {e}");
            return;
        }
    }
    println!("   合并前   {}", quick_check(&plain));
    let frames = wb_switch_core::modules::trae_delete::merge_wal_into_plain(&plain, &wal_c, &key);
    println!("   合并帧数 = {frames:?}");
    println!("   合并后   {}", quick_check(&plain));

    // ---- D：把当前 WAL 合并到「旧快照」的副本上（复现 A 的坏法）----
    println!("\n== D. 旧快照副本 + 当前 WAL（复现 A）==");
    let d2 = tmp("snap");
    let snap_c = d2.join("snap.db");
    let wal_c2 = d2.join("snap.db-wal");
    let _ = std::fs::copy(&snap, &snap_c);
    let _ = std::fs::copy(&wal, &wal_c2);
    let f2 = wb_switch_core::modules::trae_delete::merge_wal_into_plain(&snap_c, &wal_c2, &key);
    println!("   合并帧数 = {f2:?}");
    println!("   {}", quick_check(&snap_c));

    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&d2);
}
