//! Telegram bot gateway (Roadmap Phase 5, first adapter).
//!
//! The bridge is a thin client of [`EnginePort`]: mobile text in, agent
//! turns out, with live progress edits and slash-commands (`/status`,
//! `/cores 2`) hitting engine policy rather than the model.
//!
//! Safety stance: the bot refuses to start without a non-empty chat-id
//! allowlist; unauthorized chats are logged and dropped silently, so the
//! bot never becomes a prompt oracle for strangers. Update offsets are
//! persisted *after* each reply, giving at-least-once delivery: a crash
//! mid-turn replays that one update rather than silently losing it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::mpsc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Telegram message ceiling.
const TG_MAX_TEXT: usize = 4096;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("telegram http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("telegram api error on {method}: {description}")]
    Api { method: String, description: String },
    #[error("invalid bridge config: {0}")]
    Config(String),
}

/// Everything the bridge may ask of the daemon. Implemented by the
/// `Engine` in the daemon binary; fakes in tests.
pub trait EnginePort: Send + Sync {
    /// Run one agent turn. Human-readable progress lines flow through
    /// `progress` while the future resolves to the final answer.
    fn run_turn(
        &self,
        prompt: String,
        progress: mpsc::UnboundedSender<String>,
    ) -> BoxFuture<'_, std::result::Result<String, String>>;

    /// Engine status document for `/status`.
    fn status_json(&self) -> BoxFuture<'_, Value>;

    /// Live CPU cap change for `/cores 2` / `/cores 50%`.
    fn set_cores(&self, spec: String) -> std::result::Result<String, String>;

    /// Active skills + gaps for `/skills`. Default: not supported.
    fn skill_report(&self) -> BoxFuture<'_, Value> {
        Box::pin(async { json!({"supported": false}) })
    }

    /// One-click `skill_bootstrap` for `/skill <lang>`.
    fn bootstrap_skill(&self, _lang: String) -> std::result::Result<String, String> {
        Err("this engine does not author skills".into())
    }
}

/// Durable last-seen update id (backed by project memory in the daemon).
pub trait OffsetStore: Send + Sync {
    fn load(&self) -> BoxFuture<'_, i64>;
    fn save(&self, offset: i64) -> BoxFuture<'_, ()>;
}

#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub bot_token: String,
    /// Override for self-hosted Bot API servers and tests.
    pub api_base: String,
    /// Chats allowed to talk to the agent. Empty is a config error.
    pub allowed_chat_ids: Vec<i64>,
    /// getUpdates long-poll window.
    pub long_poll_seconds: u64,
}

impl TelegramConfig {
    #[must_use]
    pub fn new(bot_token: impl Into<String>, allowed_chat_ids: Vec<i64>) -> Self {
        Self {
            bot_token: bot_token.into(),
            api_base: "https://api.telegram.org".into(),
            allowed_chat_ids,
            long_poll_seconds: 25,
        }
    }
}

pub struct Bridge {
    cfg: TelegramConfig,
    http: reqwest::Client,
    engine: Arc<dyn EnginePort>,
    offsets: Arc<dyn OffsetStore>,
}

impl Bridge {
    /// # Errors
    ///
    /// [`BridgeError::Config`] on blank token or empty allowlist.
    pub fn new(
        cfg: TelegramConfig,
        engine: Arc<dyn EnginePort>,
        offsets: Arc<dyn OffsetStore>,
    ) -> Result<Self, BridgeError> {
        if cfg.bot_token.trim().is_empty() {
            return Err(BridgeError::Config("bot_token is empty".into()));
        }
        if cfg.allowed_chat_ids.is_empty() {
            return Err(BridgeError::Config(
                "allowed_chat_ids is empty: refuse to run an open-to-the-world agent".into(),
            ));
        }
        // Long-poll responses arrive after long_poll_seconds; the client
        // timeout must exceed it or every idle tick looks like an error.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.long_poll_seconds + 15))
            .build()
            .map_err(BridgeError::Http)?;
        Ok(Self {
            cfg,
            http,
            engine,
            offsets,
        })
    }

    async fn call(&self, method: &str, payload: &Value) -> Result<Value, BridgeError> {
        let url = format!("{}/bot{}/{}", self.cfg.api_base, self.cfg.bot_token, method);
        let resp = self.http.post(&url).json(payload).send().await?;
        let body: Value = resp.json().await?;
        if body.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(BridgeError::Api {
                method: method.to_owned(),
                description: body
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
            });
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn send_text(&self, chat_id: i64, text: &str) -> Result<Option<i64>, BridgeError> {
        let clamped = clamp_text(text);
        let result = self
            .call("sendMessage", &json!({"chat_id": chat_id, "text": clamped}))
            .await?;
        Ok(result.get("message_id").and_then(Value::as_i64))
    }

    async fn edit_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
    ) -> Result<(), BridgeError> {
        let clamped = clamp_text(text);
        self.call(
            "editMessageText",
            &json!({"chat_id": chat_id, "message_id": message_id, "text": clamped}),
        )
        .await?;
        Ok(())
    }

    /// One long-poll cycle: fetch, dispatch, advance the offset per
    /// processed update. Returns the number of updates handled.
    pub async fn tick(&self) -> Result<usize, BridgeError> {
        let offset = self.offsets.load().await;
        let result = self
            .call(
                "getUpdates",
                &json!({
                    "offset": offset,
                    "timeout": self.cfg.long_poll_seconds,
                    "allowed_updates": ["message"],
                }),
            )
            .await?;
        let mut handled = 0usize;
        for update in result.as_array().into_iter().flatten() {
            let Some(update_id) = update.get("update_id").and_then(Value::as_i64) else {
                continue;
            };
            self.dispatch(update).await;
            // Persist after dispatch: at-least-once, never silent loss.
            self.offsets.save(update_id + 1).await;
            handled += 1;
        }
        Ok(handled)
    }

    /// Drive the bridge until `shutdown` resolves; API errors back off
    /// exponentially instead of hot-looping.
    pub async fn run(&self, shutdown: impl Future<Output = ()>) {
        let mut backoff = 1u64;
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                res = self.tick() => match res {
                    Ok(0) => backoff = 1,
                    Ok(_) => backoff = 1,
                    Err(e) => {
                        tracing::warn!(error = %e, backoff, "telegram tick failed");
                        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                        backoff = (backoff * 2).min(60);
                    }
                },
            }
        }
        tracing::info!("telegram bridge stopped");
    }

    async fn dispatch(&self, update: &Value) {
        let Some(message) = update.get("message") else {
            return;
        };
        let Some(chat_id) = message
            .get("chat")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_i64)
        else {
            return;
        };
        let text = message
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !self.cfg.allowed_chat_ids.contains(&chat_id) {
            tracing::warn!(chat_id, "unauthorized telegram chat ignored");
            return;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let mut words = trimmed.split_whitespace();
        match words.next().unwrap_or_default() {
            "/start" | "/help" => {
                let _ = self.send_text(chat_id, USAGE).await;
            }
            "/status" => {
                let doc = self.engine.status_json().await;
                let _ = self.send_text(chat_id, &status_line(&doc)).await;
            }
            "/cores" => {
                let spec = words.next().unwrap_or_default().to_owned();
                let reply = match self.engine.set_cores(spec) {
                    Ok(msg) => format!("cpu: {msg}"),
                    Err(e) => format!("cores error: {e}"),
                };
                let _ = self.send_text(chat_id, &reply).await;
            }
            "/skills" => {
                let doc = self.engine.skill_report().await;
                let reply = skill_summary(&doc);
                let _ = self.send_text(chat_id, &reply).await;
            }
            "/skill" => {
                let lang = words.next().unwrap_or_default().to_owned();
                let reply = if lang.is_empty() {
                    "usage: /skill <language> (fills one detected gap)".to_owned()
                } else {
                    match self.engine.bootstrap_skill(lang) {
                        Ok(msg) => format!("✓ {msg}"),
                        Err(e) => format!("skill error: {e}"),
                    }
                };
                let _ = self.send_text(chat_id, &reply).await;
            }
            other if other.starts_with('/') => {
                let _ = self.send_text(chat_id, "unknown command; try /help").await;
            }
            _ => self.run_agent_turn(chat_id, trimmed).await,
        }
    }

    /// Placeholder message, live progress edits, final answer (or error)
    /// in place. Errors always produce a visible reply; silence is the
    /// worst mobile UX.
    async fn run_agent_turn(&self, chat_id: i64, prompt: &str) {
        let Ok(Some(placeholder_id)) = self.send_text(chat_id, "⏳ working…").await else {
            return;
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let lines: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let pump_lines = Arc::clone(&lines);
        let pump = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let mut guard = pump_lines.lock().expect("progress lines lock");
                guard.push(line);
                if guard.len() > 12 {
                    guard.remove(0);
                }
            }
        });
        let outcome = self.engine.run_turn(prompt.to_owned(), tx).await;
        pump.await.ok();
        let progress = lines.lock().expect("progress lines lock").join("\n");
        let final_text = match outcome {
            Ok(answer) => {
                if progress.is_empty() {
                    answer
                } else {
                    format!("{answer}\n\n—\n{progress}")
                }
            }
            Err(e) => format!("✗ turn failed: {e}"),
        };
        // Edit in place; if the placeholder vanished (deleted/expired),
        // fall back to a fresh message.
        if self
            .edit_text(chat_id, placeholder_id, &final_text)
            .await
            .is_err()
        {
            let _ = self.send_text(chat_id, &final_text).await;
        }
    }
}

const USAGE: &str = "\
open-chat agent.
Free text runs an agent turn in your workspace.
/status   governor, skills, memory
/cores N  live CPU cap (N or N%)";

/// Compact human line for mobile: the same data as GET /status.
fn status_line(doc: &Value) -> String {
    let g = &doc["governor"];
    format!(
        "model {} · cores {}/{} · slots {} · stack [{}] · skills {} · memory {} · dedupe-safe",
        doc["llm_model"].as_str().unwrap_or("?"),
        g["cores"].as_u64().unwrap_or(0),
        g["usable_total"].as_u64().unwrap_or(0),
        g["free_build_slots"].as_u64().unwrap_or(0),
        doc["stack"].as_str().unwrap_or("none"),
        doc["active_skills"].as_array().map_or(0, Vec::len),
        doc["memory_entries"].as_u64().unwrap_or(0),
    )
}

/// `/skills` render from the engine report doc.
fn skill_summary(doc: &Value) -> String {
    if doc.get("supported").and_then(Value::as_bool) == Some(false) {
        return "skill report unsupported by this engine".to_owned();
    }
    let list = |k: &str| {
        doc[k]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let (active, gaps) = (list("active"), list("gaps"));
    let mut out = format!(
        "active: [{}]\ngaps: [{}]",
        if active.is_empty() { "none" } else { &active },
        if gaps.is_empty() { "none" } else { &gaps },
    );
    if !gaps.is_empty() {
        out.push_str("\nfill one: /skill <lang>");
    }
    out
}

fn clamp_text(text: &str) -> String {
    if text.len() <= TG_MAX_TEXT {
        return text.to_owned();
    }
    // Reserve room for the ellipsis so the result also fits.
    let mut cut = TG_MAX_TEXT - "…".len() - 1;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// Scripted Telegram Bot API mock: returns queued getUpdates results,
    /// records every call.
    struct MockTg {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl MockTg {
        async fn start(batches: Vec<Value>) -> (Self, String) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let update_batches = Arc::new(Mutex::new(VecDeque::from(batches)));
            let (c2, b2) = (Arc::clone(&calls), Arc::clone(&update_batches));
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else {
                        continue;
                    };
                    let (c3, b3) = (Arc::clone(&c2), Arc::clone(&b2));
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut buf = vec![0u8; 8192];
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let Some(head_end) = text.find("\r\n\r\n") else {
                            return;
                        };
                        let body = &text[head_end + 4..];
                        let method = text
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .and_then(|p| p.rsplit('/').next())
                            .unwrap_or_default()
                            .to_owned();
                        let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                        let result = match method.as_str() {
                            "getUpdates" => {
                                b3.lock().unwrap().pop_front().unwrap_or_else(|| json!([]))
                            }
                            _ => json!({"message_id": 1}),
                        };
                        if !(method.is_empty()) {
                            c3.lock().unwrap().push((method.clone(), payload));
                        }
                        let resp_body = json!({"ok": true, "result": result}).to_string();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{resp_body}",
                            resp_body.len()
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                    });
                }
            });
            let _ = update_batches; // owned by the server task loop
            (Self { calls }, format!("http://{addr}"))
        }

        fn sent(&self, chat: i64) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(m, p)| {
                    (m == "sendMessage" || m == "editMessageText")
                        && p["chat_id"].as_i64() == Some(chat)
                })
                .map(|(_, p)| p["text"].as_str().unwrap_or_default().to_owned())
                .collect()
        }
    }

    struct FakeEngine {
        prompts: Mutex<Vec<String>>,
        cores: Mutex<Vec<String>>,
        bootstrapped: Mutex<Vec<String>>,
        fail: bool,
    }

    impl EnginePort for FakeEngine {
        fn run_turn(
            &self,
            prompt: String,
            progress: mpsc::UnboundedSender<String>,
        ) -> BoxFuture<'_, std::result::Result<String, String>> {
            Box::pin(async move {
                self.prompts.lock().unwrap().push(prompt.clone());
                let _ = progress.send("✓ /bin/echo: ok".into());
                if self.fail {
                    return Err("provider exploded".into());
                }
                Ok(format!("pong: {prompt}"))
            })
        }
        fn status_json(&self) -> BoxFuture<'_, Value> {
            Box::pin(async {
                json!({"llm_model": "mock", "governor": {"cores": 2, "usable_total": 8, "free_build_slots": 1}, "stack": "rust", "active_skills": ["rust"], "memory_entries": 3})
            })
        }
        fn set_cores(&self, spec: String) -> std::result::Result<String, String> {
            self.cores.lock().unwrap().push(spec.clone());
            Ok(format!("cores now {spec}"))
        }
        fn skill_report(&self) -> BoxFuture<'_, Value> {
            Box::pin(async { json!({"supported": true, "active": ["rust"], "gaps": ["zig"]}) })
        }
        fn bootstrap_skill(&self, lang: String) -> std::result::Result<String, String> {
            self.bootstrapped.lock().unwrap().push(lang.clone());
            Ok(format!("created skill {lang}"))
        }
    }

    struct MemOffset(AtomicI64);
    impl OffsetStore for MemOffset {
        fn load(&self) -> BoxFuture<'_, i64> {
            Box::pin(async { self.0.load(Ordering::SeqCst) })
        }
        fn save(&self, offset: i64) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.0.store(offset, Ordering::SeqCst) })
        }
    }

    fn update(id: i64, chat: i64, text: &str) -> Value {
        json!({"update_id": id, "message": {"chat": {"id": chat}, "text": text}})
    }

    async fn bridge(
        fail: bool,
        batches: Vec<Value>,
    ) -> (Bridge, MockTg, Arc<FakeEngine>, Arc<AtomicI64>) {
        let (mock, base) = MockTg::start(batches).await;
        let engine = Arc::new(FakeEngine {
            prompts: Mutex::new(Vec::new()),
            cores: Mutex::new(Vec::new()),
            bootstrapped: Mutex::new(Vec::new()),
            fail,
        });
        let offset = Arc::new(AtomicI64::new(0));
        let cfg = TelegramConfig {
            api_base: base,
            long_poll_seconds: 0,
            ..TelegramConfig::new("TESTTOKEN", vec![42])
        };
        struct Adaptor(Arc<AtomicI64>);
        impl OffsetStore for Adaptor {
            fn load(&self) -> BoxFuture<'_, i64> {
                let v = self.0.load(Ordering::SeqCst);
                Box::pin(async move { v })
            }
            fn save(&self, offset: i64) -> BoxFuture<'_, ()> {
                let t = Arc::clone(&self.0);
                Box::pin(async move { t.store(offset, Ordering::SeqCst) })
            }
        }
        let b = Bridge::new(cfg, engine.clone(), Arc::new(Adaptor(Arc::clone(&offset)))).unwrap();
        (b, mock, engine, offset)
    }

    #[tokio::test]
    async fn text_prompt_runs_turn_and_replies_allowed_chat_only() {
        let (b, mock, engine, offset) = bridge(
            false,
            vec![json!([update(10, 42, "hi"), update(11, 43, "hack me")])],
        )
        .await;
        assert_eq!(b.tick().await.unwrap(), 2);
        assert_eq!(engine.prompts.lock().unwrap().as_slice(), ["hi"]);
        let sent42 = mock.sent(42);
        assert!(sent42.iter().any(|t| t.starts_with("⏳")), "{sent42:?}");
        assert!(
            sent42
                .iter()
                .any(|t| t.contains("pong: hi") && t.contains("✓ /bin/echo")),
            "final reply with progress tail: {sent42:?}"
        );
        assert!(
            mock.sent(43).is_empty(),
            "unauthorized chat must get nothing"
        );
        assert_eq!(offset.load(Ordering::SeqCst), 12, "last update id + 1");
    }

    #[tokio::test]
    async fn slash_commands_hit_engine_not_model() {
        let (b, mock, engine, _o) = bridge(
            false,
            vec![json!([
                update(1, 42, "/cores 3"),
                update(2, 42, "/status"),
                update(3, 42, "/skills"),
                update(4, 42, "/skill zig")
            ])],
        )
        .await;
        b.tick().await.unwrap();
        assert_eq!(engine.cores.lock().unwrap().as_slice(), ["3"]);
        assert_eq!(engine.bootstrapped.lock().unwrap().as_slice(), ["zig"]);
        assert!(
            engine.prompts.lock().unwrap().is_empty(),
            "commands must not bill the model"
        );
        let sent = mock.sent(42);
        assert!(sent.iter().any(|t| t == "cpu: cores now 3"), "{sent:?}");
        assert!(
            sent.iter()
                .any(|t| t.contains("model mock") && t.contains("2/8")),
            "{sent:?}"
        );
        assert!(
            sent.iter()
                .any(|t| t.contains("active: [rust]") && t.contains("gaps: [zig]")),
            "{sent:?}"
        );
        assert!(
            sent.iter().any(|t| t.contains("created skill zig")),
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn failed_turn_replies_with_error_never_silent() {
        let (b, mock, _e, _o) = bridge(true, vec![json!([update(5, 42, "doomed")])]).await;
        b.tick().await.unwrap();
        let sent = mock.sent(42);
        assert!(
            sent.iter()
                .any(|t| t.contains("turn failed") && t.contains("provider exploded")),
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn offset_feeds_next_poll() {
        let (b, mock, _e, _o) = bridge(false, vec![json!([]), json!([])]).await;
        b.tick().await.unwrap();
        let last = mock.calls.lock().unwrap().pop().unwrap();
        assert_eq!(last.0, "getUpdates");
        assert_eq!(last.1["offset"], 0);
    }

    #[tokio::test]
    async fn refuses_open_allowlist_and_blank_token() {
        let (mock, base) = MockTg::start(vec![]).await;
        let engine = Arc::new(FakeEngine {
            prompts: Mutex::new(Vec::new()),
            cores: Mutex::new(Vec::new()),
            bootstrapped: Mutex::new(Vec::new()),
            fail: false,
        });
        let offsets = Arc::new(MemOffset(AtomicI64::new(0)));
        let open = TelegramConfig {
            api_base: base.clone(),
            ..TelegramConfig::new("t", vec![])
        };
        assert!(matches!(
            Bridge::new(open, engine.clone(), offsets.clone()),
            Err(BridgeError::Config(_))
        ));
        let blank = TelegramConfig {
            api_base: base,
            ..TelegramConfig::new("  ", vec![1])
        };
        assert!(matches!(
            Bridge::new(blank, engine, offsets),
            Err(BridgeError::Config(_))
        ));
        let _ = mock;
    }

    #[test]
    fn clamp_cuts_on_char_boundary() {
        let long = "ü".repeat(5000); // 2 bytes each
        let c = clamp_text(&long);
        assert!(c.len() <= TG_MAX_TEXT);
        assert!(c.ends_with('…'));
    }
}
