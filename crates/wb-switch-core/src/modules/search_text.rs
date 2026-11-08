//! 会话正文搜索的公共原语：LIKE 转义、片段截取、大小写不敏感匹配。
//!
//! 三个产品的会话页（Trae / WorkBuddy / Qoder）都要「搜对话内容」，但数据面完全不同：
//! Trae 与 Qoder 走 SQLite（`LIKE` 预筛 + Rust 复核），WorkBuddy 走 1.7 GB 的 JSONL
//! （字节级匹配后再解析）。**判定口径必须一致**，否则同一个词在三个页面里给出不同结果，
//! 用户没法判断是「真没有」还是「这个页面搜不到」。所以三个原语只在这里实现一份。
//!
//! 三条硬规矩：
//! 1. **大小写只对 ASCII 生效**：中文没有大小写，英文/路径要忽略大小写（与 SQLite
//!    `LIKE` 的观感对齐）；
//! 2. **片段按字符切**，不按字节切 —— 按字节切会把汉字切出乱码；
//! 3. **转义 `% _ \`**：用户输入一个 `%` 不该等于「搜全部」。

/// 把用户输入变成 `LIKE '%…%'` 的参数，转义 `% _ \`（配 `ESCAPE '\'` 使用）。
pub fn like_pattern(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len() + 8);
    for c in query.chars() {
        match c {
            '\\' | '%' | '_' => {
                escaped.push('\\');
                escaped.push(c);
            }
            _ => escaped.push(c),
        }
    }
    format!("%{escaped}%")
}

/// 取命中处前后各 `ctx` 个**字符**的片段；没命中返回 `None`。
///
/// `query_lower` 必须是已经转小写的关键词（调用方算一次，别在循环里重复算）。
pub fn snippet_of(text: &str, query_lower: &str, ctx: usize) -> Option<String> {
    if query_lower.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let pos = lower.find(query_lower)?;
    // `find` 给的是字节下标，换算成字符下标再切，否则会切坏多字节字符。
    let start_char = lower[..pos].chars().count();
    let match_chars = query_lower.chars().count();
    let chars: Vec<char> = text.chars().collect();
    let start = start_char.saturating_sub(ctx);
    let end = (start_char + match_chars + ctx).min(chars.len());
    let mut s: String = chars[start..end].iter().collect();
    // 压成单行：列表里一行显示，换行会把行高撑开。
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    Some(s)
}

/// 大小写不敏感（仅 ASCII）的子串判断，给已解码的文本用。
pub fn contains_ci(text: &str, needle: &str) -> bool {
    contains_ci_bytes(text.as_bytes(), needle)
}

/// 大小写不敏感（仅 ASCII）的**字节**子串判断。
///
/// WorkBuddy 的正文是几十 MB 的 JSONL，先按字节筛再解析：命中首字节只做一次
/// `memchr`，仅在候选位置做整段比较 —— `windows()` 那种 O(n·m) 写法会慢一个数量级。
pub fn contains_ci_bytes(hay: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > hay.len() {
        return false;
    }
    let first = needle[0];
    let len = needle.len();
    let mut i = 0usize;
    while i + len <= hay.len() {
        let found = if first.is_ascii_alphabetic() {
            memchr::memchr2(first.to_ascii_lowercase(), first.to_ascii_uppercase(), &hay[i..])
        } else {
            memchr::memchr(first, &hay[i..])
        };
        let Some(pos) = found else { return false };
        let at = i + pos;
        if at + len > hay.len() {
            return false;
        }
        if hay[at..at + len].eq_ignore_ascii_case(needle) {
            return true;
        }
        i = at + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户输入里的 `%` / `_` 必须按字面匹配，否则一个 `%` 就变成「搜全部」。
    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("abc"), "%abc%");
        assert_eq!(like_pattern("100%_x"), "%100\\%\\_x%");
        // 反斜杠自己也要转义，否则 ESCAPE 把后面的字符吃掉。
        assert_eq!(like_pattern("a\\b"), "%a\\\\b%");
    }

    /// 中文按**字符**截断：按字节切会把汉字切成乱码。
    #[test]
    fn snippet_cuts_by_chars_not_bytes() {
        let text = "前面很长的一段中文用来测试截断位置是否安全助手回答在这里后面还有很多内容也要截掉一些才对";
        let s = snippet_of(text, "助手回答", 4).expect("应命中");
        assert!(s.contains("助手回答"), "命中词必须留在片段里：{s}");
        assert_eq!(s.chars().count(), 4 + 4 + 4 + 2, "实际片段：{s}");
        assert!(s.starts_with('…') && s.ends_with('…'));
    }

    #[test]
    fn snippet_omits_ellipsis_at_edges() {
        assert_eq!(snippet_of("hello world", "hello", 10).unwrap(), "hello world");
        assert_eq!(snippet_of("hello world", "world", 10).unwrap(), "hello world");
        assert!(snippet_of("hello", "absent", 10).is_none());
    }

    /// 大小写不敏感只作用于 ASCII；中文按原样匹配。
    #[test]
    fn contains_ci_is_ascii_case_insensitive() {
        assert!(contains_ci("Hello World", "hello"));
        assert!(contains_ci("HELLO", "hello"));
        assert!(contains_ci_bytes(b"path/to/Trae.exe", "trae.exe"));
        assert!(contains_ci("这里有个签到任务", "签到"));
        assert!(!contains_ci("abc", "abd"));
        // 跨字节边界不能误判（先找候选再整段比较）。
        assert!(!contains_ci_bytes(b"ab", "abc"));
    }
}
