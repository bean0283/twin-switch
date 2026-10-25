//! 会话的「工作空间」归属 —— Trae / WorkBuddy / Qoder 三个页面共用同一套判定。
//!
//! 为什么必须共用：三个页面显示的是同一件事（这段对话属于哪个空间/项目），各写一份就会
//! 出现「同一个目录在 Trae 页是两个空间、在 WorkBuddy 页是一个」这种对不上的问题。
//!
//! 判据全部来自客户端的真实数据，不猜：
//!
//! | 产品 | 空间来源 |
//! | --- | --- |
//! | WorkBuddy | `sessions.cwd`（`project_id` 实测全为 NULL，`workspaces` 表只有 path） |
//! | Qoder | `chat_sessions.cwd` |
//! | Trae | `project.name`（有就用它）+ `project.absolute_path` |
//!
//! **键**（分组用）一律是「路径归一化后的小写」：分隔符统一正斜杠、折叠重复、去尾斜杠。
//! 本机实测同一个目录会同时以 `D:/htw/签到` 与 `d:\htw\wenku` 两种写法出现，不归一化
//! 就会被拆成两个空间。Windows 路径大小写不敏感，所以小写是安全的。
//!
//! **展示名**取最后一段目录名（客户端「空间」列表显示的也是这一段）；WorkBuddy 给一次性
//! 对话自动建的 `~/WorkBuddy/<YYYY-MM-DD-HH-MM-SS>` 会读成「临时空间 · 10-25 13:51」。
//! 路径为空 ⇒ `("", "（未归属）")`：**空串不许当成一个真实空间**。

/// 归一化路径 → 稳定键。
pub fn workspace_key(path: &str) -> String {
    let mut p = path.trim().replace('\\', "/");
    while p.contains("//") {
        p = p.replace("//", "/");
    }
    p.trim_end_matches('/').to_lowercase()
}

/// 从路径推工作空间：`(键, 展示名)`。
pub fn workspace_of(path: &str) -> (String, String) {
    let key = workspace_key(path);
    if key.is_empty() {
        return (String::new(), "（未归属）".to_string());
    }
    let mut it = key.rsplit('/');
    let label = it.next().filter(|s| !s.is_empty()).unwrap_or(&key);
    let parent = it.next().unwrap_or("");

    // 一次性对话的时间戳目录：只换标签，键仍是完整路径（不同会话不会被并到一起）。
    let label = if parent.eq_ignore_ascii_case("workbuddy") && is_timestamp_dir(label) {
        format!(
            "临时空间 · {}-{} {}:{}",
            &label[5..7],
            &label[8..10],
            &label[11..13],
            &label[14..16]
        )
    } else {
        label.to_string()
    };
    (key, label)
}

/// 带「显式名字」的空间（Trae 的 `project.name`）：名字优先，路径只做键与兜底。
///
/// 名字为空时才退回目录名 —— 不能反过来，否则用户在 Trae 里改过的空间名会丢。
pub fn workspace_with_name(name: &str, path: &str) -> (String, String) {
    let (key, label) = workspace_of(path);
    let n = name.trim();
    if key.is_empty() {
        return (key, label);
    }
    (key, if n.is_empty() { label } else { n.to_string() })
}

/// `2026-10-25-13-51-21` 这种纯时间戳目录名（长度与分隔符位置固定）。
pub fn is_timestamp_dir(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() != 19 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        4 | 7 | 10 | 13 | 16 => *c == b'-',
        _ => c.is_ascii_digit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两种写法必须是同一个键（本机真实存在 `D:/htw/签到` 与 `d:\htw\wenku`）。
    #[test]
    fn normalizes_separators_and_case() {
        let (k1, l1) = workspace_of("D:/htw/签到");
        let (k2, l2) = workspace_of("d:\\htw\\签到\\");
        assert_eq!(k1, "d:/htw/签到");
        assert_eq!(k1, k2, "分隔符/大小写/尾斜杠不同也必须同键");
        assert_eq!(l1, "签到");
        assert_eq!(l2, "签到");
        assert_eq!(workspace_of("D://a///b").0, "d:/a/b");
        assert_eq!(workspace_of("D://a///b").1, "b");
    }

    /// 空路径不许变成「空名空间」。
    #[test]
    fn empty_is_unassigned() {
        assert_eq!(workspace_of(""), (String::new(), "（未归属）".to_string()));
        assert_eq!(workspace_of("   "), (String::new(), "（未归属）".to_string()));
        assert_eq!(workspace_of("/"), (String::new(), "（未归属）".to_string()));
    }

    /// 一次性对话的时间戳目录：只换标签，键仍是完整路径。
    #[test]
    fn timestamp_dir_gets_readable_label() {
        let (k, l) = workspace_of("C:/Users/11970/WorkBuddy/2026-10-25-13-51-21");
        assert_eq!(l, "临时空间 · 10-25 13:51");
        assert_eq!(k, "c:/users/11970/workbuddy/2026-10-25-13-51-21");

        let (k2, l2) = workspace_of("C:/Users/11970/WorkBuddy/2026-10-24-20-55-54");
        assert_ne!(k, k2);
        assert_eq!(l2, "临时空间 · 10-24 20:55");

        // 不在 WorkBuddy 目录下的同名式样目录保持原名（不自作聪明）
        assert_eq!(workspace_of("D:/proj/2026-10-25-13-51-21").1, "2026-10-25-13-51-21");
    }

    /// Trae 的空间名优先于目录名；没有名字才退回目录名。
    #[test]
    fn explicit_name_wins_over_dirname() {
        let (k, l) = workspace_with_name("ZLOS Plugin", "D:/htw/zlos-plugin");
        assert_eq!(k, "d:/htw/zlos-plugin");
        assert_eq!(l, "ZLOS Plugin");
        let (_, l2) = workspace_with_name("  ", "D:/htw/zlos-plugin");
        assert_eq!(l2, "zlos-plugin");
        // 没路径 ⇒ 无归属（即使有名字，也不能当成真实空间键）
        assert_eq!(workspace_with_name("X", ""), (String::new(), "（未归属）".to_string()));
    }
}
