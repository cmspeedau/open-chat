//! Chat provider abstraction and the OpenAI-compatible SSE client.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type ProviderResult<T> = std::result::Result<T, ProviderError>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider returned {status}: {body}")]
    Api { status: u16, body: String },
    #[error("malformed stream: {0}")]
    MalformedStream(String),
    #[error("invalid endpoint config: {0}")]
    Config(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON argument string as produced by the model.
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// Set when `role == Tool`, linking the result to its call.
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    #[must_use]
    pub fn assistant_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: calls,
            tool_call_id: None,
        }
    }
    #[must_use]
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// Tool declaration sent to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSchema>,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Stop,
    ToolCalls,
    Length,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop: StopReason,
    /// Provider-reported usage when present; the compositor's own estimate
    /// is the source of truth for budgeting until real tokenizers land.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Object-safe chat backend. Streaming happens client-side (SSE deltas are
/// accumulated into one [`ChatResponse`]); per-token push to UIs is a
/// Phase 5 concern layered on top via the same client internals.
pub trait Provider: Send + Sync {
    fn chat(&self, req: ChatRequest) -> BoxFuture<'_, ProviderResult<ChatResponse>>;
}

/// Endpoint settings for OpenAI-compatible `/chat/completions` servers.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// e.g. `https://api.openai.com/v1` (no trailing slash).
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug)]
pub struct OpenAi {
    http: reqwest::Client,
    config: OpenAiConfig,
}

impl OpenAi {
    /// # Errors
    ///
    /// [`ProviderError::Config`] when base_url/model are blank (the key may
    /// legitimately be absent for local servers, so it is not checked).
    pub fn new(config: OpenAiConfig) -> ProviderResult<Self> {
        if config.base_url.trim().is_empty() || config.base_url.trim().eq("http") {
            return Err(ProviderError::Config(
                "base_url must be a non-empty URL".into(),
            ));
        }
        if config.model.trim().is_empty() {
            return Err(ProviderError::Config("model must be non-empty".into()));
        }
        Ok(Self {
            http: reqwest::Client::new(),
            config,
        })
    }

    /// Translate our message model into the OpenAI wire format.
    fn wire_messages(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                let mut obj = serde_json::json!({ "role": role, "content": m.content });
                if !m.tool_calls.is_empty() {
                    obj["tool_calls"] = serde_json::Value::Array(
                        m.tool_calls
                            .iter()
                            .map(|c| {
                                serde_json::json!({
                                    "id": c.id,
                                    "type": "function",
                                    "function": { "name": c.name, "arguments": c.arguments },
                                })
                            })
                            .collect(),
                    );
                }
                if let Some(id) = &m.tool_call_id {
                    obj["tool_call_id"] = serde_json::json!(id);
                }
                obj
            })
            .collect()
    }

    fn wire_tools(tools: &[ToolSchema]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    },
                })
            })
            .collect()
    }
}

impl Provider for OpenAi {
    fn chat(&self, req: ChatRequest) -> BoxFuture<'_, ProviderResult<ChatResponse>> {
        Box::pin(async move {
            let body = serde_json::json!({
                "model": if req.model.is_empty() { &self.config.model } else { &req.model },
                "messages": Self::wire_messages(&req.messages),
                "tools": Self::wire_tools(&req.tools),
                "stream": true,
                "stream_options": { "include_usage": true },
            });
            let mut http = self
                .http
                .post(format!(
                    "{}/chat/completions",
                    self.config.base_url.trim_end_matches('/')
                ))
                .json(&body);
            if !self.config.api_key.is_empty() {
                http = http.bearer_auth(&self.config.api_key);
            }
            let resp = http.send().await?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                let clipped: String = body.chars().take(500).collect();
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    body: clipped,
                });
            }

            use futures_util::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut acc = SseAccumulator::default();
            while let Some(chunk) = stream.next().await {
                acc.feed(&chunk?)?;
            }
            acc.finish()
        })
    }
}

/// Incremental SSE -> ChatResponse parser. Kept free of I/O so fixtures
/// can drive it line by line.
#[derive(Debug, Default)]
pub(crate) struct SseAccumulator {
    text: String,
    /// index -> (id, name, arguments)
    calls: std::collections::BTreeMap<usize, (String, String, String)>,
    stop: Option<StopReason>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    pending: Vec<u8>,
    done: bool,
}

impl SseAccumulator {
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> ProviderResult<()> {
        self.pending.extend_from_slice(chunk);
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            self.apply_line(line.trim_end_matches(['\n', '\r']))?;
        }
        Ok(())
    }

    fn apply_line(&mut self, line: &str) -> ProviderResult<()> {
        if line.is_empty() || self.done {
            return Ok(()); // blank separators; ignore anything after [DONE]
        }
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            return Ok(()); // comments/keepalives and other SSE fields
        };
        if payload == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: serde_json::Value = serde_json::from_str(payload)
            .map_err(|e| ProviderError::MalformedStream(format!("{payload}: {e}")))?;
        if let Some(usage) = v.get("usage") {
            self.input_tokens = usage.get("prompt_tokens").and_then(|x| x.as_u64());
            self.output_tokens = usage.get("completion_tokens").and_then(|x| x.as_u64());
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return Ok(()); // usage-only chunk
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
                self.text.push_str(t);
            }
            if let Some(deltas) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                for d in deltas {
                    let idx = d.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    let entry = self
                        .calls
                        .entry(idx)
                        .or_insert_with(|| (String::new(), String::new(), String::new()));
                    if let Some(id) = d.get("id").and_then(|i| i.as_str()) {
                        entry.0 = id.to_owned();
                    }
                    if let Some(name) = d
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                    {
                        entry.1 = name.to_owned();
                    }
                    if let Some(args) = d
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                    {
                        entry.2.push_str(args);
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            self.stop = Some(match reason {
                "tool_calls" => StopReason::ToolCalls,
                "stop" => StopReason::Stop,
                "length" => StopReason::Length,
                _ => StopReason::Other,
            });
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> ProviderResult<ChatResponse> {
        let tool_calls = self
            .calls
            .into_values()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments,
            })
            .collect::<Vec<_>>();
        let stop = self.stop.unwrap_or(if tool_calls.is_empty() {
            StopReason::Stop
        } else {
            StopReason::ToolCalls
        });
        Ok(ChatResponse {
            text: self.text,
            tool_calls,
            stop,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(json: &str) -> String {
        format!("data: {json}\n\n")
    }

    #[test]
    fn parses_text_deltas_and_done() {
        let mut acc = SseAccumulator::default();
        for part in [
            r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"{"choices":[{"delta":{"content":"lo there"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ] {
            acc.feed(data(part).as_bytes()).unwrap();
        }
        acc.feed(b"data: [DONE]\n\n").unwrap();
        let r = acc.finish().unwrap();
        assert_eq!(r.text, "Hello there");
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.stop, StopReason::Stop);
    }

    #[test]
    fn accumulates_fragmented_tool_calls_by_index() {
        let mut acc = SseAccumulator::default();
        // Split across chunk boundaries mid-JSON to exercise buffering.
        let a = data(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"run_command","arguments":"{\"cmd\":"}}]}}]}"#,
        );
        let b = data(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}},{"index":1,"id":"call_b","function":{"name":"apply_patch","arguments":"{}"}}],"finish_reason":"tool_calls"}}]}"#,
        );
        let raw = format!("{a}{b}");
        let bytes = raw.as_bytes();
        for i in 0..bytes.len() {
            // one byte at a time: worst-case framing
            acc.feed(&bytes[i..i + 1]).unwrap();
        }
        let r = acc.finish().unwrap();
        assert_eq!(r.tool_calls.len(), 2);
        assert_eq!(r.tool_calls[0].id, "call_a");
        assert_eq!(r.tool_calls[0].arguments, r#"{"cmd":"ls"}"#);
        assert_eq!(r.tool_calls[1].name, "apply_patch");
        assert_eq!(r.stop, StopReason::ToolCalls);
    }

    #[test]
    fn usage_and_keepalives_survive() {
        let mut acc = SseAccumulator::default();
        acc.feed(b": keepalive\n\n").unwrap();
        acc.feed(
            data(r#"{"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7}}"#).as_bytes(),
        )
        .unwrap();
        acc.feed(data(r#"{"choices":[{"delta":{"content":"x"}}]}"#).as_bytes())
            .unwrap();
        let r = acc.finish().unwrap();
        assert_eq!(r.input_tokens, Some(42));
        assert_eq!(r.output_tokens, Some(7));
        assert_eq!(r.text, "x");
    }

    #[test]
    fn malformed_payload_is_typed_error() {
        let mut acc = SseAccumulator::default();
        let err = acc.feed(b"data: {oops\n\n").unwrap_err();
        assert!(matches!(err, ProviderError::MalformedStream(_)));
    }

    #[test]
    fn wire_format_maps_roles_and_calls() {
        let msgs = vec![
            ChatMessage::system("s"),
            ChatMessage::assistant_calls(vec![ToolCall {
                id: "1".into(),
                name: "run_command".into(),
                arguments: "{}".into(),
            }]),
            ChatMessage::tool("1", "ok"),
        ];
        let wire = OpenAi::wire_messages(&msgs);
        assert_eq!(wire[0]["role"], "system");
        assert_eq!(wire[1]["tool_calls"][0]["function"]["name"], "run_command");
        assert_eq!(wire[2]["tool_call_id"], "1");
        assert_eq!(wire[2]["role"], "tool");
    }

    #[test]
    fn rejects_blank_config() {
        assert!(
            OpenAi::new(OpenAiConfig {
                base_url: "".into(),
                api_key: "k".into(),
                model: "m".into()
            })
            .is_err()
        );
        assert!(
            OpenAi::new(OpenAiConfig {
                base_url: "http".into(),
                api_key: "k".into(),
                model: "m".into()
            })
            .is_err()
        );
        assert!(
            OpenAi::new(OpenAiConfig {
                base_url: "http://x/v1".into(),
                api_key: "k".into(),
                model: "".into()
            })
            .is_err()
        );
    }

    #[tokio::test]
    async fn end_to_end_against_a_mock_sse_server() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = [
            data(r#"{"choices":[{"delta":{"content":"Mock"}}]}"#),
            data(r#"{"choices":[{"delta":{"content":" answer"},"finish_reason":"stop"}]}"#),
            data("[DONE]"),
        ]
        .concat();
        let srv = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).unwrap(); // drain request (headers+body fit)
            write!(
                sock,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            sock.write_all(body.as_bytes()).unwrap();
            sock.flush().unwrap();
            // Drain any late request bytes non-blocking, then close: an
            // unread receive queue would make drop() send RST and kill our
            // just-flushed response. The response is fully framed by
            // content-length, so no lingering read is needed.
            sock.set_nonblocking(true).ok();
            let mut drain = [0u8; 1024];
            while sock.read(&mut drain).unwrap_or(0) > 0 {}
        });
        let provider = OpenAi::new(OpenAiConfig {
            base_url: format!("http://{addr}"),
            api_key: "test-key".into(),
            model: "mock".into(),
        })
        .unwrap();
        let resp = provider
            .chat(ChatRequest {
                messages: vec![ChatMessage::user("hi")],
                tools: vec![],
                model: String::new(),
            })
            .await
            .unwrap();
        assert_eq!(resp.text, "Mock answer");
        assert_eq!(resp.stop, StopReason::Stop);
        srv.join().unwrap();
    }

    #[tokio::test]
    async fn api_error_is_typed() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).unwrap();
            let body = r#"{"error":{"message":"invalid api key"}}"#;
            write!(
                sock,
                "HTTP/1.1 401 Unauthorized\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            sock.flush().unwrap();
            sock.set_nonblocking(true).ok();
            let mut drain = [0u8; 1024];
            while sock.read(&mut drain).unwrap_or(0) > 0 {}
        });
        let provider = OpenAi::new(OpenAiConfig {
            base_url: format!("http://{addr}"),
            api_key: "bad".into(),
            model: "mock".into(),
        })
        .unwrap();
        let err = provider
            .chat(ChatRequest {
                messages: vec![ChatMessage::user("hi")],
                tools: vec![],
                model: String::new(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Api { status: 401, .. }),
            "{err}"
        );
        srv.join().unwrap();
    }
}
