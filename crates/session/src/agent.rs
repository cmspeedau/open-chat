//! The agent loop: compose -> model -> tools -> (verify) -> close.
//!
//! Closes Phase 2 item 3 at the turn level. `apply_patch` verifies inline,
//! but the loop adds the gate the README demands: when a patch left
//! verification failing, the turn may not close on a plain "done" — the
//! verifier re-runs, and a failure is injected back as a repair nudge,
//! bounded by the iteration budget (no infinite repair spirals: the
//! watchdog pattern applied to the agent itself).

use std::sync::Arc;

use openchat_memory::MemoryStore;
use openchat_tools::compact::{ToolRecord, Transcript};
use tokio::sync::Mutex;

use crate::compositor::{Composed, Compositor};
use crate::provider::{ChatMessage, ChatRequest, Provider, ToolCall};
use crate::slop::strip_leading_slop;
use crate::tools::{ApplyPatchTool, Registry};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("provider failed: {0}")]
    Provider(#[from] crate::provider::ProviderError),
    #[error(
        "iteration budget ({0}) exhausted without a verified clean close; last state stays in the transcript"
    )]
    BudgetExhausted(usize),
}

/// Progress events for whoever drives the loop (daemon CLI today, SSE
/// fan-out in Phase 5).
#[derive(Debug)]
pub enum AgentEvent<'a> {
    Iteration {
        index: usize,
        composed: &'a Composed,
    },
    AssistantText(&'a str),
    ToolCalled {
        name: &'a str,
        ok: bool,
        digest: &'a str,
    },
    FinalVerification {
        ok: bool,
    },
}

#[derive(Debug)]
pub struct AgentOutcome {
    pub text: String,
    pub iterations: usize,
    pub records: Vec<ToolRecord>,
    /// The last composition, for the token-telemetry report.
    pub composed: Composed,
    /// Bytes of conversational filler the stream interceptor removed from
    /// model output this turn (Anti-Slop Law telemetry).
    pub slop_stripped_bytes: usize,
}

pub struct Agent<'a> {
    provider: &'a dyn Provider,
    registry: &'a Registry,
    compositor: Compositor,
    memory: Option<Arc<Mutex<MemoryStore>>>,
    model: String,
    max_iterations: usize,
}

impl<'a> Agent<'a> {
    #[must_use]
    pub fn new(
        provider: &'a dyn Provider,
        registry: &'a Registry,
        memory: Option<Arc<Mutex<MemoryStore>>>,
    ) -> Self {
        Self {
            provider,
            registry,
            compositor: Compositor::new(24_000),
            memory,
            model: String::new(),
            max_iterations: 8,
        }
    }

    /// Builder knobs; defaults suit a laptop daemon (24k-token window,
    /// eight model round-trips per turn).
    #[must_use]
    pub fn with_budget(mut self, max_total_tokens: usize, max_iterations: usize) -> Self {
        self.compositor = Compositor::new(max_total_tokens);
        self.max_iterations = max_iterations.max(1);
        self
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Run one turn to completion (or budget error).
    pub async fn run(
        &self,
        user: &str,
        on_event: &mut dyn FnMut(&AgentEvent<'_>),
    ) -> Result<AgentOutcome, AgentError> {
        let mut history: Vec<ChatMessage> = Vec::new();
        let mut transcript = Transcript::new(8 * 1024);
        let mut records: Vec<ToolRecord> = Vec::new();
        let mut last_composed: Option<Composed> = None;
        let mut final_text: Option<String> = None;
        let mut slop_stripped_bytes = 0usize;
        let mut iterations = 0usize;

        for iteration in 1..=self.max_iterations {
            iterations = iteration;
            let memory_hits = self.recall_memories(user).await;
            let rendered = transcript.render();
            let composed = self
                .compositor
                .compose("", &memory_hits, &rendered, &history, user);
            on_event(&AgentEvent::Iteration {
                index: iteration,
                composed: &composed,
            });
            last_composed = Some(composed.clone());

            let resp = self
                .provider
                .chat(ChatRequest {
                    messages: composed.messages.clone(),
                    tools: self.registry.schemas(),
                    model: self.model.clone(),
                })
                .await?;

            // Enforcement point 2: leading filler never reaches history,
            // events, or the user; the byte count is kept as telemetry.
            let (clean_text, stripped) = strip_leading_slop(&resp.text);
            if stripped > 0 {
                slop_stripped_bytes += stripped;
                tracing::warn!(bytes = stripped, "slop interceptor stripped model filler");
            }
            if !clean_text.is_empty() {
                on_event(&AgentEvent::AssistantText(clean_text));
            }
            history.push(assistant_message(clean_text, resp.tool_calls.clone()));

            if resp.tool_calls.is_empty() {
                match self.close_gate().await {
                    Gate::Clear(ok_flag) => {
                        if let Some(ok) = ok_flag {
                            on_event(&AgentEvent::FinalVerification { ok });
                        }
                        final_text = Some(clean_text.to_owned());
                        break;
                    }
                    Gate::Repair(record) => {
                        on_event(&AgentEvent::FinalVerification { ok: false });
                        records.push(record.clone());
                        transcript.push(record.clone());
                        history.push(ChatMessage::user(format!(
                            "verification failed after your edits:\n{}\n\
                             Fix the code with apply_patch; do not close the turn until it passes.",
                            render_failure(&record)
                        )));
                        continue;
                    }
                }
            }

            for call in &resp.tool_calls {
                let outcome = self.registry.dispatch(call).await;
                let digest = outcome.record.as_ref().map_or_else(
                    || outcome.text.lines().next().unwrap_or_default().to_owned(),
                    ToolRecord::digest,
                );
                if let Some(record) = outcome.record.clone() {
                    records.push(record.clone());
                    transcript.push(record);
                }
                on_event(&AgentEvent::ToolCalled {
                    name: &call.name,
                    ok: outcome.ok,
                    digest: &digest,
                });
                history.push(ChatMessage::tool(call.id.clone(), outcome.text));
            }
        }

        let composed = last_composed.ok_or(AgentError::BudgetExhausted(self.max_iterations))?;
        match final_text {
            Some(text) => Ok(AgentOutcome {
                text,
                iterations,
                records,
                composed,
                slop_stripped_bytes,
            }),
            None => Err(AgentError::BudgetExhausted(self.max_iterations)),
        }
    }

    /// May the turn close? If a patch left verification pending, re-run the
    /// verifier: pass -> clear and allow; fail -> demand repair.
    async fn close_gate(&self) -> Gate {
        let patch_tools: Vec<&ApplyPatchTool> = self
            .registry
            .iter()
            .filter_map(|t| t.as_any().downcast_ref::<ApplyPatchTool>())
            .collect();
        let dirty = patch_tools.iter().any(|t| t.is_dirty());
        if !dirty {
            return Gate::Clear(None);
        }
        for tool in &patch_tools {
            if let Some(record) = tool.verify().await {
                if record.succeeded() {
                    tool.clear_dirty();
                    return Gate::Clear(Some(true));
                }
                return Gate::Repair(record);
            }
        }
        // Dirty but no verifier: nothing left to prove, unblock the turn.
        for tool in &patch_tools {
            tool.clear_dirty();
        }
        Gate::Clear(Some(true))
    }

    /// Best-effort automatic recall: memory being down must never block a
    /// turn, so errors degrade to no-context with a debug line.
    async fn recall_memories(&self, user: &str) -> Vec<String> {
        let Some(store) = &self.memory else {
            return Vec::new();
        };
        match store.lock().await.recall(user, None, 800) {
            Ok(hits) => hits
                .iter()
                .map(|h| format!("{} ({}): {}", h.entry.key, h.entry.category, h.entry.value))
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "memory recall degraded");
                Vec::new()
            }
        }
    }
}

enum Gate {
    /// Turn may close; carries Some(ok) when a verification ran.
    Clear(Option<bool>),
    /// Verification pending/failed: carries the record to show the model.
    Repair(ToolRecord),
}

fn assistant_message(text: &str, calls: Vec<ToolCall>) -> ChatMessage {
    let mut msg = if calls.is_empty() {
        ChatMessage::assistant(text)
    } else {
        ChatMessage::assistant_calls(calls)
    };
    if msg.content.is_empty() {
        msg.content = text.to_owned();
    }
    msg
}

/// Failure evidence for the repair nudge: the digest plus a stderr tail.
fn render_failure(record: &ToolRecord) -> String {
    fn tail(s: &str, n: usize) -> String {
        if s.len() <= n {
            s.to_owned()
        } else {
            let mut cut = s.len() - n;
            while !s.is_char_boundary(cut) {
                cut += 1;
            }
            s[cut..].to_owned()
        }
    }
    format!(
        "{}\nstderr tail: {}",
        record.digest(),
        tail(&record.stderr, 1024)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{BoxFuture, ChatResponse, ProviderError, StopReason};
    use openchat_governor::{Governor, GovernorConfig};
    use serde_json::json;

    /// Provider that replays canned responses; the last one repeats so a
    /// stubborn model can be scripted without sizing the queue exactly.
    struct Scripted {
        replies: Mutex<Vec<ChatResponse>>,
        seen: Mutex<Vec<ChatRequest>>,
    }

    impl Scripted {
        fn new(replies: Vec<ChatResponse>) -> Self {
            Self {
                replies: Mutex::new(replies),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl Provider for Scripted {
        fn chat(&self, req: ChatRequest) -> BoxFuture<'_, Result<ChatResponse, ProviderError>> {
            Box::pin(async move {
                self.seen.lock().await.push(req);
                let mut replies = self.replies.lock().await;
                if replies.len() == 1 {
                    return Ok(replies[0].clone());
                }
                Ok(replies.remove(0))
            })
        }
    }

    fn text_reply(t: &str) -> ChatResponse {
        ChatResponse {
            text: t.into(),
            tool_calls: Vec::new(),
            stop: StopReason::Stop,
            input_tokens: None,
            output_tokens: None,
        }
    }

    fn tool_reply(id: &str, name: &str, args: serde_json::Value) -> ChatResponse {
        ChatResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args.to_string(),
            }],
            stop: StopReason::ToolCalls,
            input_tokens: None,
            output_tokens: None,
        }
    }

    async fn harness(dir: &std::path::Path, verifier: Option<Vec<String>>) -> Registry {
        let governor = Arc::new(
            Governor::new(GovernorConfig {
                low_process_priority: false,
                ..Default::default()
            })
            .unwrap(),
        );
        let store = Arc::new(Mutex::new(MemoryStore::open_in_memory(4).unwrap()));
        crate::tools::standard_registry(governor, dir, store, verifier)
    }

    #[tokio::test]
    async fn plain_answer_needs_no_tools() {
        let dir = tempfile::tempdir().unwrap();
        let registry = harness(dir.path(), None).await;
        let provider = Scripted::new(vec![text_reply("2 + 2 = 4")]);
        let agent = Agent::new(&provider, &registry, None);
        let out = agent.run("what is 2+2", &mut |_| {}).await.unwrap();
        assert_eq!(out.text, "2 + 2 = 4");
        let seen = provider.seen.lock().await;
        assert!(
            seen[0].messages[0]
                .content
                .contains("INVIOLABLE ENGINE LAW")
        );
    }

    #[tokio::test]
    async fn filler_is_stripped_from_final_text() {
        let dir = tempfile::tempdir().unwrap();
        let registry = harness(dir.path(), None).await;
        let provider = Scripted::new(vec![text_reply(
            "Sure! I can certainly help with that. The parser now rejects ambiguous hunks.",
        )]);
        let out = Agent::new(&provider, &registry, None)
            .run("status?", &mut |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "The parser now rejects ambiguous hunks.");
        assert!(out.slop_stripped_bytes > 30, "{}", out.slop_stripped_bytes);
        let seen = provider.seen.lock().await;
        let final_msgs = &seen[seen.len() - 1].messages;
        assert!(
            !final_msgs.iter().any(|m| m.content.contains("Sure!")),
            "stripped filler must not round-trip upstream"
        );
    }

    #[tokio::test]
    async fn tool_round_trip_executes_command() {
        let dir = tempfile::tempdir().unwrap();
        let registry = harness(dir.path(), None).await;
        let provider = Scripted::new(vec![
            tool_reply(
                "c1",
                "run_command",
                json!({"cmd": "/bin/echo seeded", "build": false}),
            ),
            text_reply("done"),
        ]);
        let agent = Agent::new(&provider, &registry, None);
        let mut events = Vec::<String>::new();
        let out = agent
            .run("run echo please", &mut |e| match e {
                AgentEvent::ToolCalled { name, ok, .. } => events.push(format!("{name}:{ok}")),
                AgentEvent::AssistantText(t) => events.push(format!("text:{t}")),
                _ => {}
            })
            .await
            .unwrap();
        assert_eq!(out.text, "done");
        assert!(
            events.contains(&"run_command:true".to_string()),
            "{events:?}"
        );
        let seen = provider.seen.lock().await;
        let second = &seen[1].messages;
        assert!(
            second
                .iter()
                .any(|m| m.role == crate::provider::Role::Tool && m.content.contains("seeded")),
            "tool output fed back"
        );
    }

    #[tokio::test]
    async fn failing_verification_keeps_turn_open_until_fixed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("v.txt"), "alpha\n").unwrap();
        let registry = harness(
            dir.path(),
            Some(vec![
                "/bin/grep".into(),
                "-q".into(),
                "^beta$".into(),
                "v.txt".into(),
            ]),
        )
        .await;
        let block = |from: &str, to: &str| {
            tool_reply(
                "p",
                "apply_patch",
                json!({"patches": format!("@@@ v.txt\n<<<<<<< SEARCH\n{from}\n=======\n{to}\n>>>>>>> REPLACE\n")}),
            )
        };
        let provider = Scripted::new(vec![
            block("alpha", "alpha-typo"),       // patch applies, inline verify fails
            text_reply("hmm, let me fix that"), // close gate re-verifies, fails, nudges
            block("alpha-typo", "beta"),        // now verify passes inline
            text_reply("closed clean"),
        ]);
        let agent = Agent::new(&provider, &registry, None);
        let out = agent.run("make it say beta", &mut |_| {}).await.unwrap();
        assert_eq!(out.text, "closed clean");
        assert!(
            out.iterations >= 4,
            "gate forced extra rounds: {}",
            out.iterations
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("v.txt")).unwrap(),
            "beta\n"
        );
    }

    #[tokio::test]
    async fn stubborn_model_hits_the_iteration_budget() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("v.txt"), "alpha\n").unwrap();
        let registry = harness(
            dir.path(),
            Some(vec![
                "/bin/grep".into(),
                "-q".into(),
                "never".into(),
                "v.txt".into(),
            ]),
        )
        .await;
        let block = tool_reply(
            "p1",
            "apply_patch",
            json!({"patches": "@@@ v.txt\n<<<<<<< SEARCH\nalpha\n=======\nbeta\n>>>>>>> REPLACE\n"}),
        );
        // "all done" saturates: model never actually fixes the verifier.
        let provider = Scripted::new(vec![block, text_reply("all done")]);
        let agent = Agent::new(&provider, &registry, None).with_budget(24_000, 3);
        let err = agent.run("break it", &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, AgentError::BudgetExhausted(3)), "{err}");
    }

    #[tokio::test]
    async fn memory_injection_is_automatic() {
        let dir = tempfile::tempdir().unwrap();
        let governor = Arc::new(
            Governor::new(GovernorConfig {
                low_process_priority: false,
                ..Default::default()
            })
            .unwrap(),
        );
        let store = Arc::new(Mutex::new(MemoryStore::open_in_memory(4).unwrap()));
        store
            .lock()
            .await
            .store(&openchat_memory::MemoryEntry {
                scope: openchat_memory::MemoryScope::Project,
                category: "conventions".into(),
                key: "fmt".into(),
                value: "always use tabs for indentation".into(),
                embedding: None,
            })
            .unwrap();
        let registry = crate::tools::standard_registry(
            Arc::clone(&governor),
            dir.path(),
            Arc::clone(&store),
            None,
        );
        let provider = Scripted::new(vec![text_reply("noted, tabs it is")]);
        let agent = Agent::new(&provider, &registry, Some(store));
        agent
            .run("how should indentation be formatted", &mut |_| {})
            .await
            .unwrap();
        let seen = provider.seen.lock().await;
        assert!(
            seen[0]
                .messages
                .iter()
                .any(|m| m.content.contains("tabs for indentation")),
            "recall injected without the model asking"
        );
    }

    #[tokio::test]
    async fn unknown_tool_loops_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let registry = harness(dir.path(), None).await;
        let provider = Scripted::new(vec![
            tool_reply("x", "make_coffee", json!({})),
            text_reply("sorry, no coffee tool"),
        ]);
        let out = Agent::new(&provider, &registry, None)
            .run("coffee?", &mut |_| {})
            .await
            .unwrap();
        assert!(out.text.contains("no coffee"));
    }
}
