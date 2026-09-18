//! fastnote AI 层：OpenAI 兼容的流式客户端。
//!
//! # 设计要点
//!
//! 1. **tokio 运行时懒启动**：进程启动时不创建任何线程或连接池，
//!    第一次真正发起 AI 请求才初始化。这是"50ms 内出窗口"的前提之一。
//! 2. **UI 不需要 async**：请求在后台线程跑，增量 token 通过 channel 推给 UI，
//!    UI 每帧 `try_recv` 取走即可，避免把渲染线程染成 async。
//! 3. **可中断**：每个请求带一个取消标志，用户按 Esc 立刻停止读流。

pub mod config;
pub mod prompt;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

pub use config::AiConfig;
pub use prompt::{Message, Task};

/// 流式过程中推给 UI 的事件。
#[derive(Clone, Debug)]
pub enum AiEvent {
    /// 增量 token
    Delta(String),
    /// 正常结束
    Done,
    /// 出错（网络、鉴权、解析）
    Error(String),
}

/// 懒初始化的后台运行时。
///
/// 两个 worker 线程足够跑流式 HTTP；线程数越少，首次唤起的开销越小。
fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("fastnote-ai")
            .enable_all()
            .build()
            .expect("创建 AI 运行时失败")
    })
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // 连接超时短一点，本地端点没开时能快速报错而不是卡住
            .connect_timeout(Duration::from_secs(8))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .expect("创建 HTTP 客户端失败")
    })
}

/// 一次进行中的生成。丢弃或调用 [`Stream::cancel`] 都会中止请求。
pub struct Stream {
    pub rx: mpsc::UnboundedReceiver<AiEvent>,
    cancel: Arc<AtomicBool>,
}

impl Stream {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 非阻塞取走已到达的所有增量，供 UI 每帧调用。
    pub fn drain(&mut self) -> Vec<AiEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

/// 发起一次流式对话。立即返回，不阻塞调用线程。
pub fn stream_chat(cfg: &AiConfig, messages: Vec<Message>, model: String) -> Stream {
    let (tx, rx) = mpsc::unbounded_channel();
    let cancel = Arc::new(AtomicBool::new(false));

    if !cfg.is_configured() {
        let _ = tx.send(AiEvent::Error(
            "尚未配置 AI 端点。按 Ctrl+, 打开设置，或设置环境变量 FASTNOTE_API_KEY".into(),
        ));
        return Stream { rx, cancel };
    }

    let cfg = cfg.clone();
    let flag = cancel.clone();
    runtime().spawn(async move {
        if let Err(e) = pump(&cfg, &model, &messages, &tx, &flag).await {
            let _ = tx.send(AiEvent::Error(e.to_string()));
        }
    });

    Stream { rx, cancel }
}

/// 用某个预设动作生成内容。
pub fn run_task(cfg: &AiConfig, task: &Task, input: &str) -> Stream {
    let messages = prompt::build(task, input);
    let model = match task {
        Task::Complete => cfg.completion_model().to_string(),
        _ => cfg.model.clone(),
    };
    stream_chat(cfg, messages, model)
}

async fn pump(
    cfg: &AiConfig,
    model: &str,
    messages: &[Message],
    tx: &mpsc::UnboundedSender<AiEvent>,
    cancel: &AtomicBool,
) -> Result<()> {
    let body = ChatRequest {
        model,
        messages,
        stream: true,
        temperature: cfg.temperature,
        max_tokens: cfg.max_tokens,
    };

    let mut req = http()
        .post(cfg.chat_endpoint())
        .header("content-type", "application/json");
    if !cfg.api_key.trim().is_empty() {
        req = req.bearer_auth(cfg.api_key.trim());
    }

    let resp = req.json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{} {}", status.as_u16(), summarize_error(&text)));
    }

    let mut stream = resp.bytes_stream();
    // SSE 的一条消息可能被 TCP 分片切开，需要跨 chunk 缓冲
    let mut buf = String::new();

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            match parse_sse_line(line) {
                SseLine::Skip => {}
                SseLine::Done => {
                    let _ = tx.send(AiEvent::Done);
                    return Ok(());
                }
                SseLine::Delta(d) => {
                    if !d.is_empty() {
                        let _ = tx.send(AiEvent::Delta(d));
                    }
                }
            }
        }
    }

    let _ = tx.send(AiEvent::Done);
    Ok(())
}

enum SseLine {
    Skip,
    Done,
    Delta(String),
}

/// 解析一行 SSE。
///
/// 兼容两种常见形态：标准 `data: {...}`，以及部分本地推理服务
/// （某些 Ollama 版本）直接返回裸 JSON 行。
fn parse_sse_line(line: &str) -> SseLine {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return SseLine::Skip;
    }
    let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
    if payload == "[DONE]" {
        return SseLine::Done;
    }
    if !payload.starts_with('{') {
        return SseLine::Skip;
    }

    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return SseLine::Skip;
    };

    // 上游可能在流中夹带错误对象
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("上游返回错误");
        return SseLine::Delta(format!("\n[错误] {msg}\n"));
    }

    let choice = v.get("choices").and_then(|c| c.get(0));
    if let Some(c) = choice {
        // 流式为 delta.content；个别服务在最后一包用 message.content
        let content = c
            .get("delta")
            .and_then(|d| d.get("content"))
            .or_else(|| c.get("message").and_then(|m| m.get("content")))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if !content.is_empty() {
            return SseLine::Delta(content.to_string());
        }
        if c.get("finish_reason")
            .map(|f| !f.is_null())
            .unwrap_or(false)
        {
            return SseLine::Done;
        }
    }
    SseLine::Skip
}

/// 把上游返回的错误体压成一行可读信息，避免把整段 HTML 塞进 UI。
fn summarize_error(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(m) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return m.to_string();
        }
    }
    let one_line: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(line: &str) -> Option<String> {
        match parse_sse_line(line) {
            SseLine::Delta(d) => Some(d),
            _ => None,
        }
    }

    #[test]
    fn parses_standard_openai_chunk() {
        let l = r#"data: {"choices":[{"delta":{"content":"你好"}}]}"#;
        assert_eq!(delta(l).as_deref(), Some("你好"));
    }

    #[test]
    fn parses_done_sentinel() {
        assert!(matches!(parse_sse_line("data: [DONE]"), SseLine::Done));
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        assert!(matches!(parse_sse_line(""), SseLine::Skip));
        assert!(matches!(parse_sse_line(": ping"), SseLine::Skip));
    }

    #[test]
    fn parses_bare_json_line_from_local_server() {
        let l = r#"{"choices":[{"delta":{"content":"本地"}}]}"#;
        assert_eq!(delta(l).as_deref(), Some("本地"));
    }

    #[test]
    fn finish_reason_ends_stream() {
        let l = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        assert!(matches!(parse_sse_line(l), SseLine::Done));
    }

    #[test]
    fn inline_error_object_surfaces_message() {
        let l = r#"data: {"error":{"message":"额度不足"}}"#;
        assert!(delta(l).unwrap().contains("额度不足"));
    }

    #[test]
    fn error_body_is_summarized_to_message_field() {
        let body = r#"{"error":{"message":"invalid api key","type":"auth"}}"#;
        assert_eq!(summarize_error(body), "invalid api key");
    }

    #[test]
    fn unconfigured_client_reports_instead_of_hanging() {
        let cfg = AiConfig::default(); // 无 key
        let mut s = stream_chat(&cfg, vec![Message::user("hi")], "m".into());
        let evs = s.drain();
        assert!(matches!(evs.first(), Some(AiEvent::Error(_))));
    }
}
