//! 把指定库（默认目标客户端活库）解密为明文副本，供人工排查。
//!
//! 用法：
//!   `cargo run --example wb_probe -- <client_key> <out.db>`
//!   `cargo run --example wb_probe -- <client_key> <out.db> --db <加密库> [--wal <wal>]`
use std::path::{Path, PathBuf};

use wb_switch_core::modules::trae_decrypt::decrypt_database;
use wb_switch_core::modules::trae_delete::merge_wal_into_plain;
use wb_switch_core::modules::trae_discover::{database_path, get_client};
use wb_switch_core::modules::trae_memory_scan::load_saved_key;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let key_name = args.get(1).cloned().unwrap_or_else(|| "solo-cn".into());
    let out = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "C:/Users/11970/AppData/Local/Temp/wb_probe/live.db".into());

    let client = get_client(&key_name).expect("未知客户端");
    let mut db: PathBuf = database_path(client);
    let mut wal = db.with_extension("db-wal");
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--db" if i + 1 < args.len() => {
                db = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--wal" if i + 1 < args.len() => {
                wal = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    println!("加密库: {}", db.display());
    println!("WAL   : {} (存在={})", wal.display(), wal.is_file());
    if !db.is_file() {
        eprintln!("库不存在");
        std::process::exit(1);
    }
    let k = load_saved_key(&key_name).expect("没有存盘密钥");
    if let Some(p) = Path::new(&out).parent() {
        std::fs::create_dir_all(p).ok();
    }
    let _ = std::fs::remove_file(&out);
    let rep = decrypt_database(&db, &k, Path::new(&out), None).expect("解密失败");
    println!("解密完成: {} 页 / {} 字节 / {} 表", rep.pages, rep.total_bytes, rep.tables.len());
    if wal.is_file() {
        match merge_wal_into_plain(Path::new(&out), &wal, &k) {
            Ok(n) => println!("合并 WAL: {n} 个已提交页帧"),
            Err(e) => println!("合并 WAL 失败: {e}"),
        }
    }
    println!("输出: {out}");
}
