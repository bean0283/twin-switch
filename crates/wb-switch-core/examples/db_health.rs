//! **只读**数据库健康探针：把「读视图」（快照 + 合并 WAL）与「纯解密快照」两条路径
//! 分别跑一遍完整性检查与逐表可读性检查，定位 `database disk image is malformed`
//! 是**实时库本身损坏**，还是**我方合并 WAL 时读到了撕裂的镜像**。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example db_health -- solo-cn

use rusqlite::{Connection, OpenFlags};
use std::path::Path;

fn mtime_ns(p: &Path) -> i64 {
    std::fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn probe(label: &str, path: &Path, sid: Option<&str>) {
    println!("\n════════ {label}");
    println!("   路径 {} (size={} mtime_ns={})",
        path.display(),
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        mtime_ns(path));
    if !path.exists() {
        println!("   文件不存在，跳过");
        return;
    }
    let conn = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            println!("   ⚠️ 打开失败: {e}");
            return;
        }
    };
    // SQLite 头里的页数（用于判断文件是否被截断/多页）
    let _ = conn.execute_batch("PRAGMA trusted_schema=OFF;");
    for pragma in ["page_count", "page_size", "freelist_count"] {
        let v: Result<i64, _> = conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0));
        println!("   PRAGMA {pragma} = {v:?}");
    }
    match conn.query_row("PRAGMA quick_check(5)", [], |r| r.get::<_, String>(0)) {
        Ok(s) => println!("   quick_check = {s}"),
        Err(e) => println!("   quick_check 出错 = {e}"),
    }

    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .flatten()
        .collect();
    let mut bad = 0;
    for t in &names {
        let cols: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{t}\")"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        if !cols.iter().any(|c| c == "session_id") {
            continue;
        }
        let cnt: Result<i64, _> = conn.query_row(
            &format!("SELECT count(*) FROM \"{t}\""),
            [],
            |r| r.get(0),
        );
        let sid_cnt: Result<i64, _> = match sid {
            Some(s) => conn.query_row(
                &format!("SELECT count(*) FROM \"{t}\" WHERE session_id=?"),
                [s],
                |r| r.get(0),
            ),
            None => Ok(-1),
        };
        // 真正碰数据页：读出全部列
        let rows: Result<i64, _> = match sid {
            Some(s) => conn.query_row(
                &format!("SELECT count(*) FROM (SELECT * FROM \"{t}\" WHERE session_id=?)"),
                [s],
                |r| r.get(0),
            ),
            None => Ok(-1),
        };
        let flag = if rows.is_err() { bad += 1; " ❌" } else { "" };
        println!(
            "   {t}: 全表 {cnt:?} | WHERE sid {sid_cnt:?} | SELECT* {rows:?}{flag}"
        );
    }
    println!("   —— 出问题的表数 = {bad}");
}

fn main() {
    let client = std::env::args().nth(1).unwrap_or_else(|| "solo-cn".into());
    let sid = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "e60bf5c302083b0ad28baba2".into());

    let client_cfg = wb_switch_core::modules::trae_discover::get_client(&client).expect("客户端");
    let db = wb_switch_core::modules::trae_discover::database_path(client_cfg);
    let wal = std::path::PathBuf::from(format!("{}-wal", db.to_string_lossy()));
    println!("实时库  {} (size={} mtime_ns={})", db.display(),
        std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0), mtime_ns(&db));
    println!("实时WAL {} (size={} mtime_ns={})", wal.display(),
        std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0), mtime_ns(&wal));
    println!("WAL 待合并帧 = {}", wb_switch_core::modules::trae_delete::wal_pending_frames(&wal));

    let view = wb_switch_core::modules::trae_export::reader_plain_path(&client).expect("视图");
    probe("读视图（快照 + 合并 WAL）", &view, Some(&sid));

    let snap = wb_switch_core::modules::trae_export::decrypted_db_path(&client);
    probe("纯解密快照（不合并 WAL，导入流程的比对基准）", &snap, Some(&sid));
}
