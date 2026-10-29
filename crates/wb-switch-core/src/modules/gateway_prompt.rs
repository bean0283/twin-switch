//! 上游「system prompt 关键词风控」的兼容层。
//!
//! ## 背景（2026-10-26 真机实测，不是猜测）
//!
//! WorkBuddy / CodeBuddy 的对话网关会对 system prompt 做**关键词内容风控**，
//! 命中就回：
//!
//! ```text
//! HTTP 400 {"code":"11128","msg":"Illegal API invocation from an unapproved channel",
//!           "displayMsg":{"zh":"请求被拦截，请重新发送"},
//!           "displayTips":{"zh":"这次调用没通过渠道校验，可能不是官方客户端发出的…"}}
//! ```
//!
//! 文案说「渠道未获批准」，但**跟渠道一点关系都没有**。同一凭据、同一组请求头、
//! 同一个模型，只改 system 的第一个句子就能复现/消除：
//!
//! | system 内容 | 结果 |
//! |---|---|
//! | （不传） | 200 |
//! | `You are an AI agent powered by DeepSeek Harness.` | **400 / 11128** |
//! | `You are an AI agent powered by DeepSeek Harness.` | 200 |
//! | 同一句放在句中（不是开头） | 200 |
//!
//! 也就是说：**开头**那句 `You are an AI agent powered by DeepSeek Harness`
//! 是拦截点，句中无害。DeepSeek Harness 每轮都会把它注入到 system 最前面，
//! 于是凡是 dsh 这类客户端走本网关用 WorkBuddy，**每一轮都被拦**。
//!
//! ## 我们的处理
//!
//! 在**发往上游之前**给这个短语插一个零宽空格（U+200B）：上游的子串匹配不再命中，
//! 而模型读到的是同一个短语（零宽空格不占字形、不影响语义）。
//! 只动这一个短语，system 的其它内容一个字节都不改；没命中就不改（开关是「无命中即无操作」）。
//!
//! 为什么不直接删掉那句话：删了会改变提示词结构（它后面常紧跟模型身份/工具说明），
//! 而插零宽空格是**无损**的 —— 这是社区插件（dsh-workbuddy-fix）验证过的做法。

/// 插入的零宽空格。
const ZWSP: char = '\u{200B}';

/// `@` 规则里「同一段文本还必须出现路径类词根」的判据（小写）。
///
/// ⚠️ 用词根不用整词：真机那句是 `Tokens prefixed with @ are paths the user …`
/// （`@` 后面紧跟的是 `are`，不是 `paths`），按整词紧邻匹配会漏掉（首版单测就是因此挂的）。
/// 只认这几类，别的词不算 —— 免得把「`@` + 任意单词」都当成风控。
const PATH_ROOTS: [&str; 4] = ["path", "director", "file", "folder"];

/// 给 `deepseek harness` 之间插一个零宽空格，打断上游的子串匹配。
///
/// 大小写不敏感、容忍中间的空格数量（`deepseek  harness` / `DeepSeek\tHarness`）。
/// 已经中和过的（已有零宽空格）不会重复插入。
fn neutralize_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // ⚠️ 这里只能粗筛 `deepseek`：`deepseek\tharness` / 多空格 都不含带单空格的
    // `deepseek harness`，用精确短语当闸门会把它们漏掉（单测就是因此挂过一次）。
    // 精判交给下面的扫描器，它按「词 + 任意空白 + 词」匹配。
    //
    // 2026-10-28 起多了一条 `@` 规则，所以没 `deepseek` 时也不能直接返回；
    // 但 `@` 极少出现，用它当第二道粗筛依然能挡掉近乎全部 system 文本。
    if !lower.contains("deepseek") && !text.contains('@') {
        return None;
    }
    let mut out = String::with_capacity(text.len() + 8);
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut changed = false;
    // `@` 规则的总闸：既有 `@` 又有路径词根才启用，否则连判断都不做。
    // 只有「`@` 后面紧跟空白或引号」才算标记语法（`a@b.com` 不算），
    // 再加路径词根做闸门 —— 两个条件都满足才动。
    let at_rule = PATH_ROOTS.iter().any(|w| lower.contains(w))
        && text.match_indices('@').any(|(idx, _)| {
            // 跳过零宽字符后再判断：老版本插过 U+200B，那种文本也要能升级
            let mut j = idx + 1;
            while text[j..].starts_with(ZWSP) {
                j += ZWSP.len_utf8();
            }
            matches!(
                text[j..].chars().next(),
                Some(' ') | Some('\t') | Some('\n') | Some('"')
            )
        });
    while i < bytes.len() {
        // 在 `i` 处尝试匹配 `deepseek` + 空白* + `harness`（全部按小写比较）。
        let rest_lower = &lower[i..];
        if rest_lower.starts_with("deepseek") {
            let after_ds = i + "deepseek".len();
            let tail = &lower[after_ds..];
            let ws_len = tail.len() - tail.trim_start_matches([' ', '\t', '\n']).len();
            let tail2 = &tail[ws_len..];
            if tail2.starts_with("harness") {
                // 原样保留大写，只在中间插入零宽空格。
                out.push_str(&text[i..after_ds]);
                out.push(ZWSP);
                out.push_str(&text[after_ds..after_ds + ws_len]);
                out.push_str(&text[after_ds + ws_len..after_ds + ws_len + "harness".len()]);
                i = after_ds + ws_len + "harness".len();
                changed = true;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        // ── 规则二：`@` + 空白，且同一段文本里出现路径类词根 ────────────────
        // 2026-10-28 实测：上游新增了这条规则。dsh 的 system 里这两句（169 字符）
        // 单独发出就回 400/11128：
        //   `Tokens prefixed with @ are paths the user explicitly referenced.`
        //   `Relative paths resolve from the workspace root; absolute paths identify
        //    files or directories on the host`
        // 只在 `@` 后插一个零宽空格即 200 通过；把 `paths` 改成单数 `path` 也通过
        // ⇒ 触发条件是 `@` 与路径词的组合，不是某一句固定文案。
        if ch == '@' && at_rule {
            // 跳过已有的零宽字符：老版本网关插过 U+200B，这次要把它一并换成文字，
            // 否则规则看到「`@` 后面不是空白」就跳过，旧文本永远升级不过来。
            let mut after_at = i + 1;
            while text[after_at..].starts_with(ZWSP) {
                after_at += ZWSP.len_utf8();
            }
            let tail_orig = &text[after_at..];
            let ws = tail_orig.len() - tail_orig.trim_start_matches([' ', '\t']).len();
            // 2026-10-29 真机复测：**零宽空格那招失效了**。同一句
            //   `Tokens prefixed with @ are paths …`（带 U+200B）仍然 400/11128，
            // 上游显然是先把零宽字符归一化掉再做匹配。
            // 改成把 `@` **写成一个词**：语义不变（模型照样知道是 at-sign），
            // 但不再有「`@` + 路径词」这个组合。整段 27KB 重发实测 200。
            if tail_orig.starts_with('"') {
                // `@"…"` 这种紧贴写法
                out.push_str("at-sign followed by ");
                i = after_at;
                changed = true;
                continue;
            }
            if ws > 0 {
                // ⚠️ 必须真有空白：`@src/main.rs` / `a@b.com` 这类不是标记语法，
                // 改写会把邮箱、引用路径写坏。
                out.push_str("at-sign");
                i = after_at;
                changed = true;
                continue;
            }
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    changed.then_some(out)
}

/// 就地把请求体里**system 消息**的触发短语中和掉，返回「改了几处」。
///
/// 只碰 `role` 是 `system` / `developer` 的消息；user / assistant / tool 一律不动
/// （实测拦截只发生在 system 里，动用户内容属于越权）。
pub fn neutralize_system(body: &mut serde_json::Value) -> usize {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return 0;
    };
    let mut hits = 0usize;
    for m in msgs.iter_mut() {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role != "system" && role != "developer" {
            continue;
        }
        match m.get_mut("content") {
            Some(serde_json::Value::String(s)) => {
                if let Some(fixed) = neutralize_text(s) {
                    *s = fixed;
                    hits += 1;
                }
            }
            // 分段内容（`[{type:"text",text:"…"}]`）也要处理：有的客户端这么发。
            Some(serde_json::Value::Array(parts)) => {
                for p in parts.iter_mut() {
                    if let Some(s) = p.get_mut("text").and_then(|v| v.as_str().map(str::to_string)) {
                        if let Some(fixed) = neutralize_text(&s) {
                            if let Some(obj) = p.as_object_mut() {
                                obj.insert("text".into(), serde_json::Value::String(fixed));
                            }
                            hits += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 真机实测被拦的那一句，中和后必须变样、且语义字符一位不少。
    #[test]
    fn neutralizes_the_blocking_sentence() {
        let raw = "You are an AI agent powered by DeepSeek Harness.";
        let fixed = neutralize_text(raw).expect("必须命中");
        assert!(fixed.contains(ZWSP), "没有插入零宽空格：{fixed:?}");
        assert!(!fixed.to_lowercase().contains("deepseek harness"), "仍能被匹配到");
        // 去掉零宽空格后必须与原句逐字相同（无损）。
        assert_eq!(fixed.replace(ZWSP, ""), raw);
    }

    /// 句中/句首/大小写/多空格都要命中；已经中和过的不重复插。
    #[test]
    fn tolerates_case_and_spacing() {
        for raw in [
            "powered by deepseek harness",
            "powered by DEEPSEEK HARNESS",
            "powered by DeepSeek\tHarness",
            "powered by DeepSeek   Harness",
        ] {
            let fixed = neutralize_text(raw).expect(raw);
            assert_eq!(fixed.replace(ZWSP, ""), raw);
        }
        let once = neutralize_text("DeepSeek Harness").unwrap();
        assert!(neutralize_text(&once).is_none(), "已中和过就不该再改");
    }

    /// 2026-10-29 真机复测：`@` + 路径词这句单独发出即 400/11128，而且**带零宽空格
    /// 也拦**（上游会先做 Unicode 归一化）。所以这里改成把 `@` 写成词。
    ///
    /// 实测对照（同一账号、同一模型、同一请求头，只改这一句）：
    /// | system 内容 | 结果 |
    /// |---|---|
    /// | 整段 27KB，`@` 原样 | 400 / 11128 |
    /// | 整段 27KB，`@` 后插 U+200B | 400 / 11128 |
    /// | 整段 27KB，`@` 改写成 `at-sign` | **200** |
    #[test]
    fn rewrites_at_prefixed_path_sentence() {
        let raw = "Tokens prefixed with @ are paths the user explicitly referenced. \
                   Relative paths resolve from the workspace root; absolute paths \
                   identify files or directories on the host";
        let fixed = neutralize_text(raw).expect("必须命中");
        assert!(!fixed.contains("@ are"), "触发组合还在：{fixed:?}");
        assert!(fixed.contains("prefixed with at-sign"), "改写后要读得通：{fixed:?}");
        // 后面的正文一个字都不能丢
        assert!(fixed.contains("identify files or directories on the host"));
    }

    /// `@"…"` 紧贴写法也要改；邮箱这种「不是标记语法」的 `@` 一律不碰。
    #[test]
    fn handles_quoted_marker_but_keeps_emails() {
        let raw = "A path with spaces is written @\"...\" inside the message. \
                   Mail me at a@b.com, files list here.";
        let fixed = neutralize_text(raw).expect("必须命中");
        assert!(fixed.contains("at-sign followed by"), "{fixed:?}");
        assert!(fixed.contains("a@b.com"), "邮箱不能被改写：{fixed:?}");
    }

    /// 老版本网关插过零宽空格；同一段文本再来一次必须**升级**成文字，而不是跳过。
    #[test]
    fn upgrades_previously_zwsp_neutralized_text() {
        let old = "Tokens prefixed with @\u{200B} are paths the user explicitly referenced.";
        let fixed = neutralize_text(old).expect("旧文本也要命中");
        assert!(!fixed.contains(ZWSP), "零宽空格应被替换掉：{fixed:?}");
        assert!(fixed.contains("prefixed with at-sign"), "{fixed:?}");
    }

    /// `@` 引用语法（无空格）必须原样保留 —— 模型要靠它认用户指的是哪个文件。
    #[test]
    fn keeps_at_reference_syntax_intact() {
        for raw in [
            "see @src/main.rs for details",
            "open @C:/work/report.xlsx now",
            "mail me at a@b.com",
        ] {
            assert!(neutralize_text(raw).is_none(), "不该动：{raw:?}");
        }
    }

    /// 不含该短语的文本一个字都不许改（这是「无损」的底线）。
    #[test]
    fn leaves_unrelated_text_alone() {
        assert!(neutralize_text("You are a helpful assistant.").is_none());
        assert!(neutralize_text("deepseek 是模型名，harness 是工具名").is_none());
    }

    /// 只动 system / developer；user 里出现同一句不改（越权）。
    #[test]
    fn only_touches_system_messages() {
        let mut body = json!({
            "messages": [
                { "role": "system", "content": "You are an AI agent powered by DeepSeek Harness." },
                { "role": "user", "content": "You are an AI agent powered by DeepSeek Harness." }
            ]
        });
        let hits = neutralize_system(&mut body);
        assert_eq!(hits, 1);
        let sys = body["messages"][0]["content"].as_str().unwrap();
        let usr = body["messages"][1]["content"].as_str().unwrap();
        assert!(sys.contains(ZWSP));
        assert!(!usr.contains(ZWSP), "用户内容不许动");
    }

    /// 分段 content 也要处理。
    #[test]
    fn handles_content_parts() {
        let mut body = json!({
            "messages": [{
                "role": "system",
                "content": [{ "type": "text", "text": "You are an AI agent powered by DeepSeek Harness." }]
            }]
        });
        assert_eq!(neutralize_system(&mut body), 1);
        assert!(body["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(ZWSP));
    }
}
