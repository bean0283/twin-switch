//! T46 取证探针（**全程只读，绝不写任何库**）：
//! 把若干「加密备份 / 明文库」各自的表行数打出来并逐表对比，用来回答
//! 「会话编辑器 / 删除流程到底动到了什么」。
//!
//! 用法：
//!   cargo run -p wb-switch-core --example t46_backup_diff -- <clientKey> <path> [path...]
//!
//! `path` 可以是 `*.db.bak`（SQLCipher 密文，自动解密到临时目录），也可以是明文 `.db`。
//! 输出：每个输入一份「表 → 行数」，再按第一个输入为基准打印差异。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use wb_switch_core::modules::trae_decrypt::decrypt_database;
use wb_switch_core::modules::trae_memory_scan::load_saved_key;

fn is_plain(p: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    let mut hdr = [0u8; 16];
    f.read_exact(&mut hdr).is_ok() && &hdr == b"SQLite format 3\0"
}

fn table_counts(p: &Path) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    let Ok(c) = Connection::open(p) else {
        println!("   !! 打不开 {}", p.display());
        return out;
    };
    let Ok(mut stmt) = c.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name") else {
        return out;
    };
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(|x| x.ok()).collect())
        .unwrap_or_default();
    drop(stmt);
    for t in names {
        let n: i64 = c
            .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
            .unwrap_or(-1);
        out.insert(t, n);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let client = args.get(1).cloned().unwrap_or_else(|| "solo-cn".into());
    let inputs: Vec<String> = args.iter().skip(2).cloned().collect();
    if inputs.is_empty() {
        println!("用法：cargo run -p wb-switch-core --example t46_backup_diff -- <clientKey> <path> [path...]");
        std::process::exit(2);
    }
    let Some(key) = load_saved_key(&client) else {
        println!("[probe] 没有 {client} 的存盘密钥，无法解密备份");
        std::process::exit(2);
    };
    let tmp = std::env::temp_dir().join("t46-bakdiff");
    let _ = std::fs::create_dir_all(&tmp);

    let mut all: Vec<(String, BTreeMap<String, i64>)> = Vec::new();
    for (i, raw) in inputs.iter().enumerate() {
        let src = PathBuf::from(raw);
        let plain = if is_plain(&src) {
            src.clone()
        } else {
            let dst = tmp.join(format!("plain-{i}.db"));
            let _ = std::fs::remove_file(&dst);
            println!("[decrypt] {} → {}", src.display(), dst.display());
            match decrypt_database(&src, &key, &dst, None) {
                Ok(rep) => println!("          页数 {} 用时 {} ms", rep.pages, rep.elapsed_ms),
                Err(e) => {
                    println!("          解密失败：{e}");
                    continue;
                }
            }
            dst
        };
        let label = src
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.clone());
        let c = table_counts(&plain);
        println!("\n===== {label}（非空表）=====");
        for (t, n) in &c {
            if *n > 0 {
                println!("   {t:<34} {n}");
            }
        }
        all.push((label, c));
    }

    // 以第一个为基准对比
    if let Some((base_label, base)) = all.first().cloned() {
        for (label, c) in all.iter().skip(1) {
            println!("\n===== 差异：{base_label} → {label} =====");
            let mut keys: Vec<String> = base.keys().chain(c.keys()).cloned().collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let a = base.get(&k).copied().unwrap_or(0);
                let b = c.get(&k).copied().unwrap_or(0);
                if a != b {
                    println!("   {k:<34} {a} → {b}   ({:+})", b - a);
                }
            }
        }
    }
}
