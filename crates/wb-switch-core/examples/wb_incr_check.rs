//! 真机验证「增量加密回写」：在**真实 279 MB 库**上跑一遍并逐页校验。
//!
//! 流程（全部在临时目录里做，不触碰线上库）：
//!   1. 解密实时库 → `ref.db`（= 导入流程里快照的角色）；
//!   2. 复制为 `new.db`，用 rusqlite 做一次**真实的小改动**（改 3 行会话标题）；
//!   3. `write_db_incremental(实时库, new.db → out.db)`，统计重写了多少页；
//!   4. 把 `out.db` 整库解密成 `check.db`，与 `new.db` **逐字节比对**。
//!
//! 用法：`cargo run --example wb_incr_check -- <client_key> [工作目录]`

use std::path::PathBuf;

use wb_switch_core::modules::{trae_decrypt, trae_discover, trae_export, trae_import, trae_memory_scan};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let client_key = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "solo-cn".to_string());
    let work: PathBuf = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("wb_incr_check"));
    std::fs::create_dir_all(&work).expect("创建工作目录失败");

    let client = trae_discover::get_client(&client_key).expect("未知客户端");
    let live = trae_discover::database_path(client);
    let key = trae_memory_scan::load_saved_key(&client_key).expect("没有已存密钥，先跑一次扫描解密");

    let ref_db = work.join("ref.db");
    let new_db = work.join("new.db");
    let out_db = work.join("out.db");
    let check_db = work.join("check.db");

    println!("实时库 : {}", live.display());
    println!("工作目录: {}", work.display());

    // 1) 解密实时库作为基准明文
    println!("\n[1] 解密实时库 → ref.db");
    let rep = trae_decrypt::decrypt_database(&live, &key, &ref_db, None).expect("解密失败");
    println!("    {} 页 / {} 表 / {} ms", rep.pages, rep.tables.len(), rep.elapsed_ms);

    // 2) 复制并做一次真实的小改动
    println!("\n[2] 复制为 new.db 并做一次真实改动（改 3 行会话标题）");
    std::fs::copy(&ref_db, &new_db).expect("复制失败");
    trae_import::patch_reserved_field(&new_db).expect("补 reserved 字段失败");
    {
        let conn = rusqlite::Connection::open(&new_db).expect("打开 new.db 失败");
        conn.execute_batch("PRAGMA synchronous=OFF;").unwrap();
        let n = conn
            .execute(
                "UPDATE chat_session SET session_title = session_title || '' \
                 WHERE session_id IN (SELECT session_id FROM chat_session LIMIT 3)",
                [],
            )
            .expect("改动失败");
        conn.execute_batch("PRAGMA optimize;").ok();
        drop(conn);
        println!("    SQLite 报告受影响行数：{n}");
    }
    let new_len = std::fs::metadata(&new_db).unwrap().len();
    println!("    new.db 页数：{}", new_len / 4096);

    // 3) 增量回写
    println!("\n[3] write_db_incremental(实时库 + new.db → out.db)");
    let t0 = std::time::Instant::now();
    let stats = trae_import::write_db_incremental(
        &key,
        &live,
        &new_db,
        &out_db,
        Some(&|m| println!("    {m}")),
    )
    .expect("增量回写失败");
    println!(
        "    全库 {} 页（原 {} 页）｜重写 {} 页（新增 {} 页）｜{:.2} MB｜{} ms（含只读扫描）",
        stats.pages,
        stats.src_pages,
        stats.changed_pages,
        stats.appended_pages,
        stats.changed_bytes as f64 / 1_048_576.0,
        stats.elapsed_ms
    );
    println!("    外层计时：{} ms", t0.elapsed().as_millis());

    // 4) 自检：out.db 解密后必须与 new.db 逐字节一致
    println!("\n[4] 解密 out.db 并与 new.db 逐字节比对");
    trae_decrypt::decrypt_database(&out_db, &key, &check_db, None).expect("解密 out.db 失败");
    let a = std::fs::read(&new_db).expect("读 new.db 失败");
    let b = std::fs::read(&check_db).expect("读 check.db 失败");
    if a.len() != b.len() {
        println!("    ✗ 大小不一致：new={} check={}", a.len(), b.len());
        std::process::exit(1);
    }
    let mut bad = 0usize;
    let mut diff_pages = 0usize;
    for (i, (x, y)) in a.chunks(4096).zip(b.chunks(4096)).enumerate() {
        if x != y {
            diff_pages += 1;
            if bad < 5 {
                println!("    ✗ 第 {} 页不一致", i + 1);
            }
            bad += 1;
        }
    }
    if bad == 0 {
        println!("    ✓ 全部 {} 页逐字节一致（{} 字节）", a.len() / 4096, a.len());
    } else {
        println!("    ✗ {diff_pages} 页不一致");
        std::process::exit(1);
    }

    // 5) 顺带量一下快照复用（ensure_decrypted）与元信息
    println!("\n[5] ensure_decrypted（快照与实时库一致时应零解密）");
    let t = std::time::Instant::now();
    match trae_export::ensure_decrypted(&client_key, None) {
        Ok(out) => println!(
            "    reused={} pages={} {} ms",
            out.reused,
            out.pages,
            t.elapsed().as_millis()
        ),
        Err(e) => println!("    （实时库已被改动，需重新解密）{e}"),
    }

    println!("\n完成。工作目录 {}（可自行删除）", work.display());
}
