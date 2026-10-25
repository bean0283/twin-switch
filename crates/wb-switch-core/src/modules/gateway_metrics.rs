//! 网关可观测性：每个请求落一行 `gateway-metrics.jsonl`。
//!
//! 为什么非要有这个文件：Trae / Qoder 慢起来时，界面上只有「已完成 7 s」和时间戳，
//! 到底是**排队**、**首字慢**、**整段慢**还是**换号重来**，从外面一点都看不出来。
//! 之前排查「同一个任务 WB 10 分钟、走 Trae 1 小时」全靠手翻 `gateway-last-request.json`
//! 和猜（2026-10-24）。
//!
//! 记的字段：
//!
//! | 字段 | 用途 |
//! | --- | --- |
//! | `ttft_ms` | 首字延迟 —— 真流式改造后这个值会**远小于** `total_ms`，一眼看出有没有生效 |
//! | `total_ms` | 整段耗时 |
//! | `queued` | 是否撞上上游排队（10605 / isQueued） |
//! | `failovers` | 这条请求换了几个号（>0 说明上一个号失败了，总时长被放大） |
//! | `prompt_tokens` / `completion_tokens` | 上游给的用量；缺就写 `null`，**绝不写 0 冒充** |
//! | `msgs` / `tools` / `chars` / `folded` | 请求体规模 + 折叠掉的历史工具结果条数 |
//!
//! ⚠️ 纪律：`ok:false` 的行必须带 `error` 原文；用量没拿到就写 `null`。
//! 「空」和「失败」不许长得一样，这条在日志里同样成立。

use serde_json::{json, Value};

/// 一次请求的追踪记录（`Clone`：换号重试时每个候选拿一份副本，只有真正落盘的那份会写）。
#[derive(Clone, Debug)]
pub struct Trace {
    pub model: String,
    pub scope: String,
    pub account: String,
    pub stream: bool,
    pub started: std::time::Instant,
    /// 首字（正文或工具调用）延迟，毫秒。
    pub first_byte_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub msgs: usize,
    pub tools: usize,
    /// `messages` 序列化后的字符数（不含工具 schema）。
    pub chars: usize,
    /// 工具 schema 的字符数。**单独记**：WorkBuddy 一次带 27 个工具，schema 可能比
    /// 历史还大，只记 `chars` 会严重低估真实请求体（判断「要不要瘦身」全靠这个数）。
    pub tools_chars: usize,
    /// 被折叠（截断）的历史工具结果条数。
    pub folded: usize,
    pub failovers: usize,
    pub queued: bool,
    /// 已经写过盘，避免同一份记录落两行。
    ///
    /// ⚠️ 故意**不用** `Arc<AtomicBool>`：换号重试时每次尝试都是 `Clone` 出来的一份，
    /// 共享一个「已写」标记会让「第 1 个号失败、第 2 个号成功」里的成功那条**永远不落盘**
    /// （第一份日志已经把标记置上了）。每次尝试各自记各自的结果才对。
    logged: bool,
}

impl Trace {
    pub fn new(model: &str, stream: bool) -> Self {
        Self {
            model: model.to_string(),
            scope: String::new(),
            account: String::new(),
            stream,
            started: std::time::Instant::now(),
            first_byte_ms: None,
            prompt_tokens: None,
            completion_tokens: None,
            msgs: 0,
            tools: 0,
            chars: 0,
            tools_chars: 0,
            folded: 0,
            failovers: 0,
            queued: false,
            logged: false,
        }
    }

    pub fn elapsed_ms(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }

    /// 这次请求落到了哪个作用域 / 哪个账号。
    pub fn hit(&mut self, scope: &str, account: &str) {
        self.scope = scope.to_string();
        self.account = account.to_string();
    }

    pub fn note_failover(&mut self) {
        self.failovers += 1;
    }

    pub fn note_queued(&mut self) {
        self.queued = true;
    }

    /// 记首字（只记第一次）。
    pub fn first_byte(&mut self) {
        if self.first_byte_ms.is_none() {
            self.first_byte_ms = Some(self.elapsed_ms());
        }
    }

    /// 上游用量。缺字段就保持 `None` —— 不许拿 0 冒充「上游说没有」。
    pub fn usage(&mut self, u: &Value) {
        if let Some(v) = u.get("prompt_tokens").and_then(Value::as_i64) {
            self.prompt_tokens = Some(v);
        }
        if let Some(v) = u.get("completion_tokens").and_then(Value::as_i64) {
            self.completion_tokens = Some(v);
        }
    }

    /// 收尾并落盘（同一份 Trace 只会写一次）。
    pub fn finish(&mut self, ok: bool, status: u16, err: &str) {
        if self.logged {
            return;
        }
        self.logged = true;
        let flat: String = err
            .replace(['\n', '\r', '\t'], " ")
            .chars()
            .take(300)
            .collect();
        let line = json!({
            "ts": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            "model": self.model,
            "scope": self.scope,
            "account": self.account,
            "stream": self.stream,
            "ok": ok,
            "status": status,
            "ttft_ms": self.first_byte_ms,
            "total_ms": self.elapsed_ms(),
            "queued": self.queued,
            "failovers": self.failovers,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "msgs": self.msgs,
            "tools": self.tools,
            "chars": self.chars,
            "tools_chars": self.tools_chars,
            "folded": self.folded,
            "error": if flat.is_empty() { Value::Null } else { json!(flat) },
        });
        append(&line.to_string());
    }
}

/// 追加一行到 `~/.twin-switch/gateway-metrics.jsonl`（超过 1 MB 只留尾部一半）。
fn append(line: &str) {
    let path = crate::modules::config::store_dir().join("gateway-metrics.jsonl");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 1024 * 1024 {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let mut cut = text.len().saturating_sub(512 * 1024);
                // 不能切在多字节字符中间（模型名 / 错误文案都有中文）。
                while cut < text.len() && !text.is_char_boundary(cut) {
                    cut += 1;
                }
                let _ = std::fs::write(&path, &text[cut..]);
            }
        }
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}
