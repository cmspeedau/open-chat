//! OpenCode gateway provider adapter.
//!
//! Lets open-chat borrow an `opencode serve` instance for what it is
//! genuinely good at: a credential vault fronting OpenCode Zen free
//! models and OpenCode Go (a paid plan, still 30+ models behind one
//! login). We probe the v2 surface on 2026-11-20: there is **no**
//! OpenAI-compatible `/v1/chat/completions` route, so this adapter speaks
//! the native shape instead:
//!
//! - `GET  /api/model`                     -> catalog (id, providerID, name, cost)
//! - `POST /api/experimental/generate/text` -> one-shot completion
//!
//! Auth is HTTP Basic `opencode:<OPENCODE_PASSWORD>`, matching how
//! OpenChamber itself spawns and guards its bundled CLI.
//!
//! Deliberate limits, stated not hidden:
//! - **chat-only**: `generate/text` takes a flat prompt and no tool
//!   definitions, so `supports_tools()` is false and the agent loop sends
//!   none. Patch/exec/memory skills still ride the prompt; the model just
//!   cannot act. Use an OpenAI-compatible provider for tool turns.
//! - **stateless**: each daemon turn is one generate call; multi-turn
//!   context is flattened into the prompt (opencode sessions would mean
//!   their loop owning ours, which is not the plan).

use std::time::Duration;

use openchat_session::provider::{
    BoxFuture, Capabilities, ChatMessage, ChatRequest, ChatResponse, Provider, ProviderError, Role,
    StopReason,
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct OpenCodeConfig {
    pub base: String,
    pub username: String,
    /// The password itself, read from the configured env var by the caller.
    pub password: String,
    pub request_timeout: Duration,
    /// "providerID/modelID" used when a request carries no explicit model.
    pub default_model: Option<String>,
}

#[derive(Debug)]
pub struct OpenCodeProvider {
    http: reqwest::Client,
    cfg: OpenCodeConfig,
}

impl OpenCodeProvider {
    pub fn new(cfg: OpenCodeConfig) -> Result<Self, ProviderError> {
        if cfg.base.trim().is_empty() {
            return Err(ProviderError::Config("opencode base url is empty".into()));
        }
        if cfg.password.trim().is_empty() {
            return Err(ProviderError::Config(
                "opencode gateway password is empty (check the env var)".into(),
            ));
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(cfg.request_timeout)
                .build()
                .map_err(ProviderError::Http)?,
            cfg,
        })
    }

    fn basic(&self) -> String {
        use std::fmt::Write as _;
        // Tiny base64 impl to avoid pulling a codec dep for one header.
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let input = format!("{}:{}", self.cfg.username, self.cfg.password);
        let mut out = String::new();
        for chunk in input.as_bytes().chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            let mut quad = [
                T[(n >> 18) as usize & 63],
                T[(n >> 12) as usize & 63],
                T[(n >> 6) as usize & 63],
                T[n as usize & 63],
            ];
            if chunk.len() == 1 {
                quad[2] = b'=';
                quad[3] = b'=';
            } else if chunk.len() == 2 {
                quad[3] = b'=';
            }
            for q in quad {
                let _ = write!(out, "{}", q as char);
            }
        }
        out
    }

    /// Catalog for the UI picker: "providerID/modelID" display ids plus a
    /// free flag for zero-cost models (Zen :free and Go share the
    /// zero-cost shape).
    pub async fn list_models(&self) -> Result<Vec<(String, bool)>, ProviderError> {
        // opencode answers 503 "plugins did not finish activating" during
        // its own warmup; wait it out briefly instead of failing the picker.
        let mut attempt = 0u8;
        let doc: Value = loop {
            let resp = self
                .http
                .get(format!("{}/api/model", self.cfg.base))
                .header("authorization", format!("Basic {}", self.basic()))
                .send()
                .await?;
            let status = resp.status();
            if status.as_u16() == 503 && attempt < 20 {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(750)).await;
                continue;
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    body: body.chars().take(300).collect(),
                });
            }
            break resp.json().await?;
        };
        let mut out = Vec::new();
        collect_models(&doc, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

fn collect_models(v: &Value, out: &mut Vec<(String, bool)>) {
    match v {
        Value::Object(map) => {
            if let (Some(Value::String(id)), Some(Value::String(prov))) =
                (map.get("id"), map.get("providerID"))
            {
                let zero_cost = map.get("cost").is_some_and(|c| match c {
                    Value::Array(tiers) => tiers.iter().all(|t| {
                        ["input", "output"].iter().all(|k| {
                            t.get(*k)
                                .is_none_or(|x| x.as_f64().is_some_and(|f| f == 0.0))
                        })
                    }),
                    Value::Object(_) => ["input", "output"].iter().all(|k| {
                        c.get(*k)
                            .is_none_or(|x| x.as_f64().is_some_and(|f| f == 0.0))
                    }),
                    _ => false,
                });
                let name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_owned();
                out.push((
                    format!(
                        "{prov}/{id}  ({name})  {}",
                        if zero_cost { "[free]" } else { "" }
                    ),
                    zero_cost,
                ));
            } else {
                for child in map.values() {
                    collect_models(child, out);
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_models(child, out);
            }
        }
        _ => {}
    }
}

impl OpenCodeProvider {
    /// Quota/limits from the gateway (zen request caps, Go rolling/weekly/
    /// monthly percentages). Surfaced verbatim - the panel renders what
    /// the gateway reports, nothing invented.
    pub async fn usage_limits(&self) -> Result<Value, ProviderError> {
        let resp = self
            .http
            .get(format!("{}/api/experimental/usage/limits", self.cfg.base))
            .header("authorization", format!("Basic {}", self.basic()))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: body.chars().take(300).collect(),
            });
        }
        let doc: Value = resp.json().await?;
        Ok(doc.get("data").cloned().unwrap_or(Value::Null))
    }
}

/// Flatten our message list into the single prompt string generate/text
/// wants. System frames keep their weight by staying first, verbatim;
/// transcript/history frames pass through unchanged; only non-system
/// turns get role labels.
pub fn flatten_prompt(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for m in messages {
        if m.role == Role::System {
            out.push_str(&m.content);
            out.push('\n');
        } else {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool-result",
                Role::System => unreachable!(),
            };
            out.push_str(&format!("{role}: {}\n", m.content));
        }
    }
    out
}

/// "providerID/modelID" -> parts, tolerating the picker's display suffix.
fn split_ref(model: &str) -> (Option<String>, Option<String>) {
    match model.split_once('/') {
        Some((p, m)) => (
            Some(p.to_owned()),
            Some(m.split("  (").next().unwrap_or(m).to_owned()),
        ),
        None => (None, Some(model.to_owned())),
    }
}

impl Capabilities for OpenCodeProvider {
    fn supports_tools(&self) -> bool {
        false
    }
}

impl Provider for OpenCodeProvider {
    fn chat(&self, req: ChatRequest) -> BoxFuture<'_, Result<ChatResponse, ProviderError>> {
        Box::pin(async move {
            let reference = if req.model.is_empty() {
                self.cfg.default_model.as_deref().unwrap_or("")
            } else {
                &req.model
            };
            let (provider, id) = split_ref(reference);
            let prompt = flatten_prompt(&req.messages);
            let mut body = serde_json::json!({ "prompt": prompt });
            if let (Some(p), Some(m)) = (provider, id) {
                body["model"] = serde_json::json!({ "id": m, "providerID": p });
            }
            let resp = self
                .http
                .post(format!("{}/api/experimental/generate/text", self.cfg.base))
                .header("authorization", format!("Basic {}", self.basic()))
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    body: text.chars().take(500).collect(),
                });
            }
            let doc: Value = serde_json::from_str(&text)
                .map_err(|e| ProviderError::MalformedStream(format!("generate/text: {e}")))?;
            let answer = doc["data"]["text"]
                .as_str()
                .ok_or_else(|| ProviderError::MalformedStream("no data.text".into()))?
                .to_owned();
            Ok(ChatResponse {
                text: answer,
                tool_calls: Vec::new(),
                stop: StopReason::Stop,
                input_tokens: None,
                output_tokens: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_basic_matches_std() {
        // Our hand-rolled base64 must equal the real thing for tricky
        // padding lengths (1 and 2 byte tails).
        let p = OpenCodeProvider {
            http: reqwest::Client::new(),
            cfg: OpenCodeConfig {
                base: "http://x".into(),
                username: "opencode".into(),
                password: "pw~1".into(),
                request_timeout: Duration::from_secs(1),
                default_model: None,
            },
        };
        // Constants independently produced with python3 -m base64.
        assert_eq!(p.basic(), "b3BlbmNvZGU6cHd+MQ==");
        let p2 = OpenCodeProvider {
            http: reqwest::Client::new(),
            cfg: OpenCodeConfig {
                base: "http://x".into(),
                username: "u".into(),
                password: "é".into(),
                request_timeout: Duration::from_secs(1),
                default_model: None,
            },
        };
        assert_eq!(p2.basic(), "dTrDqQ==");
    }

    #[test]
    fn split_ref_handles_picker_suffix() {
        let (p, m) = split_ref("opencode-go/qwen4-max  (Qwen4 Max)  ");
        assert_eq!(p.as_deref(), Some("opencode-go"));
        assert_eq!(m.as_deref(), Some("qwen4-max"));
        let (p, m) = split_ref("plainmodel");
        assert_eq!(p, None);
        assert_eq!(m.as_deref(), Some("plainmodel"));
    }

    #[test]
    fn flatten_keeps_system_verbatim_first() {
        let msgs = vec![
            ChatMessage::system("LAW\nrules"),
            ChatMessage::user("hi"),
            ChatMessage::assistant("yo"),
        ];
        let flat = flatten_prompt(&msgs);
        assert!(flat.starts_with("LAW\nrules\n"));
        assert!(flat.contains("user: hi"));
        assert!(flat.contains("assistant: yo"));
    }

    #[test]
    fn cost_zero_detection_catches_tiers_and_maps() {
        let mut out = Vec::new();
        let doc = serde_json::json!({
            "models": [
                {"id":"free-m","providerID":"opencode","name":"Free M","cost":[{"input":0,"output":0,"cache":{"read":0,"write":0}}]},
                {"id":"paid-m","providerID":"opencode-go","name":"Paid M","cost":[{"input":1.25,"output":10}]},
            ]
        });
        collect_models(&doc, &mut out);
        assert_eq!(out.len(), 2);
        let free = out.iter().find(|(_, z)| *z).expect("free flagged");
        assert!(free.0.starts_with("opencode/free-m"));
        assert!(
            out.iter()
                .any(|(d, z)| !z && d.starts_with("opencode-go/paid-m"))
        );
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use openchat_session::provider::{ChatMessage, Provider};
    use std::io::{Read as _, Write as _};

    /// Minimal v2-shaped mock: /api/model catalog + generate/text.
    fn spawn_mock() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                let head_ok = text
                    .to_ascii_lowercase()
                    // needle must be lowercase too - the token has letters
                    .contains("authorization: basic b3blbmnvzgu6dgvzdhb3");
                let body = if text.contains("POST /api/experimental/generate/text") {
                    // echo back a fragment of the received prompt for assertions
                    let req = text.split("\r\n\r\n").nth(1).unwrap_or("");
                    let contained =
                        req.contains("INVIOLABLE") && req.contains("user: pick a free model");
                    format!(
                        "{{\"data\":{{\"text\":\"{}\"}}}}",
                        if contained {
                            "GATEWAY-OK"
                        } else {
                            "FLATTEN-MISMATCH"
                        }
                    )
                } else if text.contains("GET /api/model") {
                    "{\"models\":{\"a\":{\"id\":\"tidepool-alpha-free\",\"providerID\":\"opencode\",\"name\":\"Tidepool\",\"cost\":[{\"input\":0,\"output\":0}]},\"b\":{\"id\":\"qwen4-max\",\"providerID\":\"opencode-go\",\"name\":\"Qwen4\",\"cost\":[{\"input\":0.1,\"output\":0.4}]}}}"
                        .to_owned()
                } else {
                    "{}".to_owned()
                };
                let status = if head_ok {
                    "200 OK"
                } else {
                    "401 Unauthorized"
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn provider(base: &str) -> OpenCodeProvider {
        OpenCodeProvider::new(OpenCodeConfig {
            base: base.into(),
            username: "opencode".into(),
            password: "testpw".into(),
            request_timeout: std::time::Duration::from_secs(20),
            default_model: Some("opencode/tidepool-alpha-free".into()),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn catalog_lists_and_flags_free() {
        let p = provider(&spawn_mock());
        let models = p.list_models().await.unwrap();
        assert_eq!(models.len(), 2, "{models:?}");
        assert!(
            models[0].0.starts_with("opencode-go/qwen4-max"),
            "sorted: {}",
            models[0].0
        );
        let free = models.iter().find(|(_, z)| *z).expect("free flagged");
        assert!(free.0.contains("tidepool-alpha-free"));
        assert!(free.0.contains("[free]"));
    }

    #[tokio::test]
    async fn chat_flattens_law_and_user_into_prompt() {
        let p = provider(&spawn_mock());
        assert!(!p.supports_tools(), "gateway is chat-only by design");
        let req = ChatRequest {
            messages: vec![
                ChatMessage::system("[INVIOLABLE ENGINE LAW] no slop"),
                ChatMessage::user("pick a free model"),
            ],
            tools: Vec::new(),
            model: String::new(), // falls back to default_model
        };
        let resp = p.chat(req).await.unwrap();
        assert_eq!(
            resp.text, "GATEWAY-OK",
            "law + user both reached the prompt"
        );
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn wrong_password_surfaces_auth_error() {
        let base = spawn_mock();
        let p = OpenCodeProvider::new(OpenCodeConfig {
            base: base.clone(),
            username: "opencode".into(),
            password: "nope".into(),
            request_timeout: std::time::Duration::from_secs(20),
            default_model: None,
        })
        .unwrap();
        let err = p.list_models().await.unwrap_err();
        assert!(
            matches!(err, ProviderError::Api { status: 401, .. }),
            "{err}"
        );
    }
}
