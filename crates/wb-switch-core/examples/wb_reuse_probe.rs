//! 排查探针：直接测「落盘结论复用」读取链路是否命中（2026-11-16 关联组比对）。
//!
//! 用法：`cargo run -p wb-switch-core --example wb_reuse_probe -- <id1> <id2> <id3> <id4>`

use wb_switch_core::modules::workbuddy_source;

fn main() {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    if ids.len() < 2 {
        eprintln!("用法：wb_reuse_probe <session_id> <session_id> [...]");
        std::process::exit(2);
    }
    let pairs: Vec<(String, std::path::PathBuf)> = ids
        .iter()
        .map(|id| {
            let p = workbuddy_source::find_jsonl(id);
            println!(
                "find_jsonl({}) = {:?}",
                &id[..8.min(id.len())],
                p.as_ref().map(|p| p.display().to_string())
            );
            (id.clone(), p)
        })
        .filter_map(|(id, p)| p.map(|p| (id, p)))
        .collect();
    println!("pairs: {}/{}", pairs.len(), ids.len());

    println!("--- verified_after_sync ---");
    match workbuddy_source::verified_after_sync(&pairs) {
        Some((lines, digest)) => println!("HIT: lines={lines} digest={digest}"),
        None => println!("MISS"),
    }
    println!("--- compared_from_disk ---");
    match workbuddy_source::compared_from_disk(&pairs) {
        Some(per) => {
            for (id, v) in ids.iter().zip(per.iter()) {
                match v {
                    Some(s) => println!(
                        "HIT {} lines={} digest={} q_lines={:?} q_digest={:?}",
                        &id[..8.min(id.len())],
                        s.lines,
                        s.digest,
                        s.q_lines,
                        s.q_digest
                    ),
                    None => println!("MISS {}", &id[..8.min(id.len())]),
                }
            }
        }
        None => println!("MISS (整体 None)"),
    }
}
