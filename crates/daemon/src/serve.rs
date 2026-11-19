//! Minimal HTTP front door: health, status, and SSE turn streaming
//! (Phase 5's transport spine).
//!
//! Routes:
//! - `GET  /healthz` -> `{"ok":true}`
//! - `GET  /status`  -> engine status document
//! - `POST /chat`    -> body = prompt; response = `text/event-stream` of
//!   [`TurnEvent`]s. `X-Idempotency-Key` (or `?key=`) makes a retry safe:
//!   a completed turn replays its buffered events and ends; an in-flight
//!   turn is joined live instead of started twice.
//!
//! Deliberately dependency-free (raw HTTP/1.1 over tokio TCP): the request
//! surface is four routes, and every byte in/out is visible for tests.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use crate::engine::Engine;
use openchat_session::events::TurnEvent;

const MAX_BODY: usize = 64 * 1024;
const REPLAY_WINDOW: usize = 32;
const CHANNEL_CAPACITY: usize = 512;

/// Event fan-out plus a bounded replay buffer keyed by idempotency key.
pub struct Hub {
    tx: broadcast::Sender<(String, TurnEvent)>,
    completed: Mutex<BTreeMap<String, Vec<TurnEvent>>>,
    order: Mutex<VecDeque<String>>,
    started: Mutex<BTreeMap<String, bool>>,
    counter: std::sync::atomic::AtomicU64,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            tx,
            completed: Mutex::new(BTreeMap::new()),
            order: Mutex::new(VecDeque::new()),
            started: Mutex::new(BTreeMap::new()),
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn next_id(&self) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!(
            "t{:x}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64),
            n
        )
    }

    /// Claim a turn id: first caller starts the run, later callers join.
    fn claim(&self, id: &str) -> bool {
        self.started
            .lock()
            .expect("hub lock")
            .insert(id.to_owned(), true)
            .is_none()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(String, TurnEvent)> {
        self.tx.subscribe()
    }

    fn publish(&self, id: &str, ev: TurnEvent) {
        let _ = self.tx.send((id.to_owned(), ev));
    }

    fn record_done(&self, id: &str, events: Vec<TurnEvent>) {
        let mut done = self.completed.lock().expect("hub lock");
        done.insert(id.to_owned(), events);
        let mut order = self.order.lock().expect("hub lock");
        order.push_back(id.to_owned());
        while order.len() > REPLAY_WINDOW {
            if let Some(old) = order.pop_front() {
                done.remove(&old);
                self.started.lock().expect("hub lock").remove(&old);
            }
        }
    }
}

/// Serve until the process ends. Bind failures propagate; accepted-
/// connection errors only close that connection.
pub async fn serve(
    engine: Arc<Engine>,
    hub: Arc<Hub>,
    listener: TcpListener,
) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "openchatd serving");
    loop {
        let (sock, peer) = listener.accept().await?;
        let (engine, hub) = (Arc::clone(&engine), Arc::clone(&hub));
        tokio::spawn(async move {
            if let Err(e) = handle(sock, engine, hub).await {
                tracing::debug!(%peer, error = %e, "connection ended with error");
            }
        });
    }
}

async fn handle(mut sock: TcpStream, engine: Arc<Engine>, hub: Arc<Hub>) -> anyhow::Result<()> {
    let (method, path, headers, body) = read_request(&mut sock).await?;
    let route = path.split('?').next().unwrap_or("/").to_owned();
    let query = path
        .split_once('?')
        .map(|(_, q)| q.to_owned())
        .unwrap_or_default();
    match (method.as_str(), route.as_str()) {
        ("GET", "/healthz") => send_json(&mut sock, 200, &json!({"ok": true})).await,
        ("GET", "/status") => {
            let doc = engine.status_json().await;
            send_json(&mut sock, 200, &doc).await
        }
        ("POST", "/chat") => {
            if body.trim().is_empty() {
                return send_json(&mut sock, 400, &json!({"error": "empty prompt body"})).await;
            }
            let id = header(&headers, "x-idempotency-key")
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("key=").map(str::to_owned))
                })
                .unwrap_or_else(|| hub.next_id());
            chat_stream(&mut sock, engine, hub, id, body).await
        }
        _ => send_json(&mut sock, 404, &json!({"error": "not found"})).await,
    }
}

async fn chat_stream(
    sock: &mut TcpStream,
    engine: Arc<Engine>,
    hub: Arc<Hub>,
    id: String,
    prompt: String,
) -> anyhow::Result<()> {
    // Completed replay: no new run, straight buffer dump. (Clone the
    // buffer with the lock released before any await.)
    let replay = hub.completed.lock().expect("hub lock").get(&id).cloned();
    if let Some(events) = replay {
        sse_headers(sock).await?;
        for ev in &events {
            sse_event(sock, &id, ev).await?;
        }
        return Ok(());
    }
    let first = hub.claim(&id);
    let mut rx = hub.subscribe();
    if first {
        let eng = Arc::clone(&engine);
        let h = Arc::clone(&hub);
        let id_run = id.clone();
        let prompt_run = prompt.clone();
        tokio::spawn(async move {
            let (ftx, mut frx) = broadcast::channel::<TurnEvent>(CHANNEL_CAPACITY);
            // Collector starts before the run: forwards to the hub and
            // buffers the whole stream for idempotent replay.
            let h2 = Arc::clone(&h);
            let id2 = id_run.clone();
            let collector = tokio::spawn(async move {
                let mut buf = Vec::new();
                while let Ok(ev) = frx.recv().await {
                    let terminal = matches!(
                        ev,
                        TurnEvent::TurnClosed { .. } | TurnEvent::TurnFailed { .. }
                    );
                    buf.push(ev.clone());
                    h2.publish(&id2, ev);
                    if terminal {
                        break;
                    }
                }
                h2.record_done(&id2, buf);
            });
            let err_tx = ftx.clone();
            if let Err(e) = eng.run_turn(&prompt_run, Some(ftx), &mut |_| {}).await {
                // Errors raised before the agent runs (bad provider config)
                // never reach the publisher; guarantee a terminal event.
                let _ = err_tx.send(TurnEvent::TurnFailed {
                    error: e.to_string(),
                });
            }
            let _ = collector.await;
        });
    }
    sse_headers(sock).await?;
    loop {
        match rx.recv().await {
            Ok((ev_id, ev)) if ev_id == id => {
                let terminal = matches!(
                    ev,
                    TurnEvent::TurnClosed { .. } | TurnEvent::TurnFailed { .. }
                );
                sse_event(sock, &id, &ev).await?;
                if terminal {
                    return Ok(());
                }
            }
            Ok(_) => {} // other turn
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let ev = TurnEvent::TurnFailed {
                    error: "event stream lagged".into(),
                };
                sse_event(sock, &id, &ev).await?;
                return Ok(());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

fn header(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    headers.get(name).cloned()
}

async fn read_request(
    sock: &mut TcpStream,
) -> anyhow::Result<(String, String, BTreeMap<String, String>, String)> {
    let mut buf = Vec::with_capacity(4096);
    let mut scratch = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = sock.read(&mut scratch).await?;
        if n == 0 {
            anyhow::bail!("client closed before headers");
        }
        buf.extend_from_slice(&scratch[..n]);
        anyhow::ensure!(buf.len() <= MAX_BODY + 8192, "request headers too large");
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let want = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    anyhow::ensure!(want <= MAX_BODY, "body too large");
    let mut body = buf[head_end..].to_vec();
    while body.len() < want {
        let n = sock.read(&mut scratch).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&scratch[..n]);
    }
    body.truncate(want);
    Ok((
        method,
        path,
        headers,
        String::from_utf8_lossy(&body).into_owned(),
    ))
}

async fn send_json(
    sock: &mut TcpStream,
    status: u16,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    let text = body.to_string();
    let reason = if status == 200 { "OK" } else { "Error" };
    sock.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
            text.len()
        )
        .as_bytes(),
    )
    .await?;
    sock.flush().await?;
    Ok(())
}

async fn sse_headers(sock: &mut TcpStream) -> anyhow::Result<()> {
    sock.write_all(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: keep-alive\r\n\r\n",
    )
    .await?;
    sock.flush().await?;
    Ok(())
}

async fn sse_event(sock: &mut TcpStream, id: &str, ev: &TurnEvent) -> anyhow::Result<()> {
    let payload = serde_json::to_string(ev)?;
    sock.write_all(format!("id: {id}\ndata: {payload}\n\n").as_bytes())
        .await?;
    sock.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// One-shot OpenAI-compatible mock answering with the text "served".
    fn spawn_mock_openai() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let body = concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"served\"}}]}\n\n",
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            );
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf);
                write!(
                    sock,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                sock.flush().unwrap();
                sock.set_nonblocking(true).ok();
                let mut drain = [0u8; 512];
                while sock.read(&mut drain).unwrap_or(0) > 0 {}
            }
        });
        format!("http://{addr}")
    }

    async fn harness() -> (Arc<Engine>, Arc<Hub>, String) {
        let ws = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(ws.join(".openchat")).unwrap();
        let base = spawn_mock_openai();
        std::fs::write(
            ws.join(".openchat/config.toml"),
            format!("[llm]\nbase_url = \"{base}/v1\"\nmodel = \"mock\"\n[skills]\npaths = []\n"),
        )
        .unwrap();
        let engine = Arc::new(Engine::build(&ws, None).unwrap());
        let hub = Arc::new(Hub::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(serve(Arc::clone(&engine), Arc::clone(&hub), listener));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (engine, hub, addr)
    }

    fn sse_data(raw: &str) -> Vec<TurnEvent> {
        raw.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[tokio::test]
    async fn healthz_and_status() {
        let (_engine, _hub, addr) = harness().await;
        let client = reqwest::Client::new();
        let health: serde_json::Value = client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(health["ok"], true);
        let status: serde_json::Value = client
            .get(format!("http://{addr}/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
        assert!(status["governor"]["cores"].as_u64().unwrap() >= 1);
        assert!(status["llm_model"] == "mock");
    }

    #[tokio::test]
    async fn chat_streams_turn_events() {
        let (_engine, _hub, addr) = harness().await;
        let client = reqwest::Client::new();
        let raw = client
            .post(format!("http://{addr}/chat"))
            .body("say hi")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let events = sse_data(&raw);
        let types: Vec<&str> = events
            .iter()
            .map(|e| match e {
                TurnEvent::TurnStarted { .. } => "started",
                TurnEvent::Iteration { .. } => "iteration",
                TurnEvent::TurnClosed { .. } => "closed",
                _ => "other",
            })
            .collect();
        assert_eq!(types.first().copied(), Some("started"), "{types:?}");
        assert!(types.contains(&"iteration"), "{types:?}");
        assert_eq!(types.last().copied(), Some("closed"), "{types:?}");
        let TurnEvent::TurnClosed { text, badges, .. } = events.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(text, "served");
        assert!(badges.cores >= 1);
        assert_eq!(badges.free_build_slots, 1);
    }

    #[tokio::test]
    async fn idempotency_key_replays_completed_turn() {
        let (_engine, _hub, addr) = harness().await;
        let client = reqwest::Client::new();
        let first = client
            .post(format!("http://{addr}/chat"))
            .header("x-idempotency-key", "k1")
            .body("say hi")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let second = client
            .post(format!("http://{addr}/chat"))
            .header("x-idempotency-key", "k1")
            .body("say hi")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(sse_data(&first).len(), sse_data(&second).len());
        assert!(
            sse_data(&second)
                .last()
                .is_some_and(|e| matches!(e, TurnEvent::TurnClosed { .. }))
        );
        // A second, different key runs a fresh turn (provider was called twice).
        let third = client
            .post(format!("http://{addr}/chat"))
            .header("x-idempotency-key", "k2")
            .body("again")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            third.contains("TurnClosed")
                || sse_data(&third)
                    .iter()
                    .any(|e| matches!(e, TurnEvent::TurnClosed { .. }))
        );
    }

    #[tokio::test]
    async fn empty_prompt_is_400() {
        let (_engine, _hub, addr) = harness().await;
        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/chat"))
            .body("   ")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
    }
}
