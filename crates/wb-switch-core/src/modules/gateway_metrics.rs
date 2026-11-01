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
//! | `queuedRetries` / `queuedWaitMs` | 排队重试次数 / 累计等待 —— **排队的细节都在这**，不往对话里推 |
//! | `failovers` | 这条请求换了几个号（>0 说明上一个号失败了，总时长被放大） |
//! | `prompt_tokens` / `completion_tokens` | 上游给的用量；缺就写 `null`，**绝不写 0 冒充** |
//! | `cachedTokens` | 上下文缓存命中数；`null` = 上游根本没给这项统计（如 Trae 渠道） |
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
    /// 是否撞上过上游排队。
    pub queued: bool,
    /// 排队重试了几次（一次排队至少 1 次）。
    ///
    /// ⚠️ 客户端**只该看到一条**排队提示（流式文本只能追加，每重试一次就多一行，
    /// 2026-11-01 用户截图里堆了 4 行「⏳ 上游排队中」）。所以排队的**过程细节**
    /// 全部记在这里，由网关的指标去看，不再往对话里灌。
    pub queued_retries: usize,
    /// 排队累计等待毫秒（真实耗掉的时间，区别于 `total_ms` 里混着的上游生成时间）。
    pub queued_wait_ms: i64,
    /// 上游报的「上下文缓存命中 token 数」。
    ///
    /// `None` = **上游没给这个字段**（Trae 渠道实测就不给），不是「命中 0」。
    /// 两者必须能分开：写成 0 会让人以为缓存失效了，而实际上可能只是没有这项统计。
    pub cached_tokens: Option<i64>,
    /// 这次请求用的是哪把网关 Key（用于按 Key 记额度）。
    ///
    /// 由 `chat()` 在校验 Key 之后填。换号重试时 clone 出去的每份都带着它 ——
    /// 但只有最终落盘的那份会 `finish()`，所以用量只记一次，不会因为换号重复计。
    pub key_id: Option<String>,
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
            queued_retries: 0,
            queued_wait_ms: 0,
            cached_tokens: None,
            key_id: None,
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

    /// 记一次「排队等了一轮」：次数 +1、累计等待加上 `wait_secs`。
    ///
    /// 与 [`Trace::note_queued`] 分开：那个只标「撞过排队」，这个记**过程**。
    pub fn note_queue_wait(&mut self, wait_secs: u64) {
        self.queued = true;
        self.queued_retries += 1;
        self.queued_wait_ms += (wait_secs as i64).saturating_mul(1000);
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
        // 缓存命中：三种叫法都认（OpenAI / DeepSeek / Anthropic）。
        // 一个都没有就保持 None —— 不许写 0 冒充「命中了 0」。
        self.cached_tokens = u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_i64)
            .or_else(|| u.get("prompt_cache_hit_tokens").and_then(Value::as_i64))
            .or_else(|| u.get("cache_read_input_tokens").and_then(Value::as_i64));
    }

    /// 收尾并落盘（同一份 Trace 只会写一次）。
    pub fn finish(&mut self, ok: bool, status: u16, err: &str) {
        if self.logged {
            return;
        }
        self.logged = true;
        // 按 Key 记额度：**唯一收口**。流式与非流式、成功与失败最后都走这里，
        // 所以放在 `finish` 里既不会漏记，也不会因为换号重试而重复记。
        // 拿不到用量时 `record_usage` 自己会跳过（不让失败请求消耗额度）。
        if let Some(kid) = self.key_id.as_deref() {
            crate::modules::gateway_keys::record_usage(
                kid,
                self.prompt_tokens,
                self.completion_tokens,
            );
        }
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
            "key_id": self.key_id,
            "stream": self.stream,
            "ok": ok,
            "status": status,
            "ttft_ms": self.first_byte_ms,
            "total_ms": self.elapsed_ms(),
            "queued": self.queued,
            "queuedRetries": self.queued_retries,
            "queuedWaitMs": self.queued_wait_ms,
            // `null` = 上游没给这项统计；0 是明确的「命中了 0」。两者不可混淆。
            "cachedTokens": self.cached_tokens,
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
