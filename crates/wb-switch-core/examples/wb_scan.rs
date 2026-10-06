//! 临时诊断：列出本机 WorkBuddy 会话并解析一个会话的回合结构。
//! 用法：cargo run -p wb-switch-core --example wb_scan

use wb_switch_core::modules::workbuddy_source;

fn main() {
    println!("数据根: {:?}", workbuddy_source::data_root());
    println!("可用: {}", workbuddy_source::is_available());
    let sessions = match workbuddy_source::list_sessions() {
        Ok(s) => s,
        Err(e) => {
            println!("列出失败: {e}");
            return;
        }
    };
    println!("会话数: {}", sessions.len());
    for s in sessions.iter().take(25) {
        let uid_tail: String = s.user_id.chars().skip(s.user_id.chars().count().saturating_sub(6)).collect();
        println!(
            "  [{:>8} bytes] deleted={} uid=…{} | {} | {} | {}",
            s.body_bytes, s.deleted, uid_tail, s.updated_at, s.cwd, s.title
        );
    }
    // 解析一个有正文的会话
    if let Some(target) = sessions.iter().find(|s| s.has_body && !s.deleted) {
        println!("\n=== 解析会话 {} ({}) ===", target.id, target.title);
        match workbuddy_source::load_body(&target.id) {
            Ok(body) => {
                println!("ai_title: {:?}", body.ai_title);
                println!("回合数: {}", body.turns.len());
                for (i, t) in body.turns.iter().enumerate().take(3) {
                    println!(
                        "  回合 {i}: user={} 字 / assistant={} 字 / 过程事件 {} 条",
                        t.user_text.chars().count(),
                        t.assistant_text.chars().count(),
                        t.events.len()
                    );
                    println!("     user 前80: {}", t.user_text.chars().take(80).collect::<String>());
                    println!(
                        "     asst 前80: {}",
                        t.assistant_text.chars().take(80).collect::<String>()
                    );
                    for e in t.events.iter().take(4) {
                        println!(
                            "     · {:22} name={:12} status={:9} text前60={}",
                            e.kind,
                            e.name,
                            e.status,
                            e.text.chars().take(60).collect::<String>()
                        );
                    }
                }
            }
            Err(e) => println!("解析失败: {e}"),
        }
    }
}
