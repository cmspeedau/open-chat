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

use crate::compositor::{
    CODE_SURFACE_RULES, COPY_SURFACE_RULES, Composed, Compositor, MEMORY_SAFETY_RULES,
    UI_SURFACE_RULES,
};
use crate::events::{Badges, TurnEvent};
use crate::provider::{ChatMessage, ChatRequest, Provider, ToolCall};
use crate::slop::strip_leading_slop;
use crate::tools::{ApplyPatchTool, Diagnostics, Registry, Surfaces};

/// Compiled rule text per task surface; unknown/blank surfaces contribute
/// nothing (a missing pack is never faked - README: "Missing a skill must
/// not be represented as successful validation"). UI and copy packs are
/// open Phase 2 boxes.
#[must_use]
fn surface_rules(surface: &str) -> Option<&'static str> {
    match surface {
        "code" => Some(CODE_SURFACE_RULES),
        "ui" => Some(UI_SURFACE_RULES),
        "copy" => Some(COPY_SURFACE_RULES),
        _ => None,
    }
}

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
    /// Provider-reported token usage accumulated across all iterations of
    /// this turn.
    pub usage: Usage,
}

pub struct Agent<'a> {
    provider: &'a dyn Provider,
    registry: &'a Registry,
    compositor: Compositor,
    memory: Option<Arc<Mutex<MemoryStore>>>,
    /// Bound skill (id, body) pairs injected into System Frame #0.
    skills: Vec<(String, String)>,
    /// Backend label for usage accounting ("openai-compat" | "opencode").
    via: String,
    model: String,
    max_iterations: usize,
    /// Optional wire-event fan-out for remote interfaces (SSE, bots).
    publisher: Option<tokio::sync::broadcast::Sender<TurnEvent>>,
    /// Daemon-supplied closure completing the badges at turn close.
    badges_fn: Option<BadgesFn>,
    /// Memory-fault ledger shared with the tool registry.
    diagnostics: Option<Arc<Diagnostics>>,
    /// Task-surface ledger driving the code/ui/copy skill packs.
    surfaces: Option<Arc<Surfaces>>,
}

/// Supplies the badges document at turn close from the final composition
/// and the stripped-filler byte count.
pub type BadgesFn = Box<dyn Fn(&Composed, usize, Usage) -> Badges + Send + Sync>;

/// Token usage a single turn actually consumed, as reported by the
/// provider. OpenAI-compatible backends send real counts; gateway-style
/// backends (opencode generate) report none and we say so rather than
/// passing estimates off as measured.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub model: String,
    /// "openai-compat" | "opencode" | ... for per-backend accounting.
    pub via: String,
}

impl Usage {
    #[must_use]
    pub fn reported(&self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some()
    }

    fn merge(&mut self, other: &Usage) {
        self.input_tokens = Some(self.input_tokens.unwrap_or(0) + other.input_tokens.unwrap_or(0));
        self.output_tokens =
            Some(self.output_tokens.unwrap_or(0) + other.output_tokens.unwrap_or(0));
    }
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
            skills: Vec::new(),
            model: String::new(),
            via: "openai-compat".into(),
            max_iterations: 8,
            publisher: None,
            badges_fn: None,
            diagnostics: None,
            surfaces: None,
        }
    }

    /// Publish [`TurnEvent`]s to a broadcast channel in addition to the
    /// in-process callback; used by the SSE server and bot adapters.
    #[must_use]
    pub fn with_publisher(mut self, tx: tokio::sync::broadcast::Sender<TurnEvent>) -> Self {
        self.publisher = Some(tx);
        self
    }

    /// Supplies the final badges value at `TurnClosed` from the last
    /// composition and the turn's stripped-filler byte count.
    #[must_use]
    pub fn with_badges(
        mut self,
        f: impl Fn(&Composed, usize, Usage) -> Badges + Send + Sync + 'static,
    ) -> Self {
        self.badges_fn = Some(Box::new(f));
        self
    }

    /// Receives memory-fault trap escalations from the tools; when armed,
    /// the memory-safety ruleset rides every following prompt automatically.
    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Arc<Diagnostics>) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    /// Surface ledger (code/ui/copy packs); armed surfaces inject their
    /// ruleset into all following prompts.
    #[must_use]
    pub fn with_surfaces(mut self, surfaces: Arc<Surfaces>) -> Self {
        self.surfaces = Some(surfaces);
        self
    }

    /// Attach the bound skill bodies for this stack (from
    /// `openchat_skills::prompt_set`). Idempotent per turn; the compositor
    /// may still evict the tail under budget pressure.
    #[must_use]
    pub fn with_skills(mut self, skills: Vec<(String, String)>) -> Self {
        self.skills = skills;
        self
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

    /// Backend label carried into every usage record.
    #[must_use]
    pub fn with_via(mut self, via: impl Into<String>) -> Self {
        self.via = via.into();
        self
    }

    /// Run one turn to completion (or budget error).
    pub async fn run(
        &self,
        user: &str,
        on_event: &mut (dyn FnMut(&AgentEvent<'_>) + Send),
    ) -> Result<AgentOutcome, AgentError> {
        let mut history: Vec<ChatMessage> = Vec::new();
        let mut transcript = Transcript::new(8 * 1024);
        let mut records: Vec<ToolRecord> = Vec::new();
        let mut last_composed: Option<Composed> = None;
        let mut final_text: Option<String> = None;
        let mut slop_stripped_bytes = 0usize;
        let mut iterations = 0usize;
        let mut usage = Usage {
            model: self.model.clone(),
            via: self.via.clone(),
            ..Default::default()
        };
        let mut usage_seen_any = false;
        self.publish(TurnEvent::TurnStarted {
            prompt: user.to_owned(),
        });

        for iteration in 1..=self.max_iterations {
            iterations = iteration;
            let memory_hits = self.recall_memories(user).await;
            let rendered = transcript.render();
            // Skill pairs: bound catalog, plus the memory-safety ruleset
            // while the trap ledger is armed (Phase 2 auto-injection).
            let mut skill_owned: Vec<(String, String)> = self.skills.clone();
            if self.diagnostics.as_ref().is_some_and(|d| d.is_armed()) {
                skill_owned.push(("memory-safety".into(), MEMORY_SAFETY_RULES.into()));
            }
            // Surface packs: any armed surface with a compiled ruleset.
            for (surface, _) in self.surfaces.as_ref().map(|s| s.list()).unwrap_or_default() {
                if let Some(rules) = surface_rules(&surface)
                    && !skill_owned.iter().any(|(id, _)| id == &surface)
                {
                    skill_owned.push((surface, rules.to_owned()));
                }
            }
            let skill_pairs: Vec<(&str, &str)> = skill_owned
                .iter()
                .map(|(i, b)| (i.as_str(), b.as_str()))
                .collect();
            let composed =
                self.compositor
                    .compose("", &skill_pairs, &memory_hits, &rendered, &history, user);
            on_event(&AgentEvent::Iteration {
                index: iteration,
                composed: &composed,
            });
            self.publish(TurnEvent::Iteration {
                index: iteration,
                total_tokens: composed.total_tokens,
                sections: composed.report.clone(),
            });
            last_composed = Some(composed.clone());

            let resp = match self
                .provider
                .chat(ChatRequest {
                    messages: composed.messages.clone(),
                    tools: if self.provider.supports_tools() {
                        self.registry.schemas()
                    } else {
                        Vec::new()
                    },
                    model: self.model.clone(),
                })
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    self.publish(TurnEvent::TurnFailed {
                        error: e.to_string(),
                    });
                    return Err(e.into());
                }
            };

            let resp_usage = Usage {
                input_tokens: resp.input_tokens,
                output_tokens: resp.output_tokens,
                model: self.model.clone(),
                via: self.via.clone(),
            };
            if resp_usage.reported() {
                if usage_seen_any {
                    usage.merge(&resp_usage);
                } else {
                    usage = resp_usage;
                    usage_seen_any = true;
                }
            }

            // Enforcement point 2: leading filler never reaches history,
            // events, or the user; the byte count is kept as telemetry.
            let (clean_text, stripped) = strip_leading_slop(&resp.text);
            if stripped > 0 {
                slop_stripped_bytes += stripped;
                self.publish(TurnEvent::SlopStripped { bytes: stripped });
                tracing::warn!(bytes = stripped, "slop interceptor stripped model filler");
            }
            if !clean_text.is_empty() {
                on_event(&AgentEvent::AssistantText(clean_text));
                self.publish(TurnEvent::AssistantText {
                    text: clean_text.to_owned(),
                });
            }
            history.push(assistant_message(clean_text, resp.tool_calls.clone()));

            if resp.tool_calls.is_empty() {
                match self.close_gate().await {
                    Gate::Clear(ok_flag) => {
                        if let Some(ok) = ok_flag {
                            on_event(&AgentEvent::FinalVerification { ok });
                            self.publish(TurnEvent::FinalVerification { ok });
                        }
                        // Prose gate (copy surface): screen the final
                        // answer and disclose; never silently rewrite.
                        if self.surfaces.as_ref().is_some_and(|x| x.is_armed("copy"))
                            && !clean_text.is_empty()
                        {
                            let findings: Vec<String> =
                                openchat_tools::slop::check_prose_text(clean_text)
                                    .into_iter()
                                    .map(|f| f.render())
                                    .collect();
                            if !findings.is_empty() {
                                if let Some(x) = &self.surfaces {
                                    x.note_findings(findings.len());
                                }
                                self.publish(TurnEvent::ProseGate { findings });
                            }
                        }
                        final_text = Some(clean_text.to_owned());
                        break;
                    }
                    Gate::Repair(record) => {
                        on_event(&AgentEvent::FinalVerification { ok: false });
                        self.publish(TurnEvent::FinalVerification { ok: false });
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

            let faults_before = self.diagnostics.as_ref().map_or(0, |d| d.count());
            let surfaces_before = self.surfaces.as_ref().map_or(0, |s| s.list().len());
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
                self.publish(TurnEvent::ToolCalled {
                    name: call.name.clone(),
                    ok: outcome.ok,
                    digest: digest.clone(),
                });
                history.push(ChatMessage::tool(call.id.clone(), outcome.text));
            }
            // Surface any trap hits from this batch as wire events; the
            // ruleset rides the *next* compose automatically.
            if let Some(ledger) = &self.diagnostics
                && ledger.count() > faults_before
                && let Some(diag) = ledger.last()
            {
                self.publish(TurnEvent::MemoryFault {
                    fault: diag.fault.as_str().to_owned(),
                    headline: diag.headline.clone(),
                    advice: diag.advice.to_owned(),
                });
            }
            // Newly armed task surfaces ride the next compose; announce now.
            if let Some(ledger) = &self.surfaces {
                for (surface, reason) in ledger.list().into_iter().skip(surfaces_before) {
                    self.publish(TurnEvent::SurfaceArmed { surface, reason });
                }
            }
        }

        let Some(composed) = last_composed else {
            let e = AgentError::BudgetExhausted(self.max_iterations);
            self.publish(TurnEvent::TurnFailed {
                error: e.to_string(),
            });
            return Err(e);
        };
        match final_text {
            Some(text) => {
                let badges = self.build_badges(&composed, slop_stripped_bytes, usage.clone());
                self.publish(TurnEvent::TurnClosed {
                    text: text.clone(),
                    iterations,
                    badges: Box::new(badges),
                });
                Ok(AgentOutcome {
                    text,
                    iterations,
                    records,
                    composed,
                    slop_stripped_bytes,
                    usage,
                })
            }
            None => {
                let e = AgentError::BudgetExhausted(self.max_iterations);
                self.publish(TurnEvent::TurnFailed {
                    error: e.to_string(),
                });
                Err(e)
            }
        }
    }

    fn publish(&self, ev: TurnEvent) {
        if let Some(tx) = &self.publisher {
            // No subscribers is a valid state (CLI runs without a server).
            let _ = tx.send(ev);
        }
    }

    fn build_badges(&self, composed: &Composed, slop: usize, usage: Usage) -> Badges {
        self.badges_fn.as_ref().map_or_else(
            || Badges {
                stack: String::new(),
                skills: Vec::new(),
                prompt_tokens: composed.total_tokens,
                cores: 0,
                usable_cores: 0,
                free_build_slots: 0,
                memory_entries: 0,
                slop_stripped_bytes: slop,
                deduped_rule_lines: composed.deduped_rule_lines,
                usage_input_tokens: None,
                usage_output_tokens: None,
                usage_via: self.via.clone(),
                memory_faults: self.diagnostics.as_ref().map_or(0, |d| d.count()),
                prose_findings: self.surfaces.as_ref().map_or(0, |s| s.findings()),
                checks_run: Vec::new(),
                checks_skipped: Vec::new(),
                armed_surfaces: self
                    .surfaces
                    .as_ref()
                    .map(|s| s.list().into_iter().map(|(name, _)| name).collect())
                    .unwrap_or_default(),
            },
            |f| f(composed, slop, usage),
        )
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

    impl crate::provider::Capabilities for Scripted {}

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
        crate::tools::standard_registry(
            governor,
            dir,
            store,
            verifier,
            Arc::new(crate::tools::Diagnostics::new()),
            Arc::new(crate::tools::Surfaces::new()),
        )
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
    async fn skill_bodies_reach_the_system_frame() {
        let dir = tempfile::tempdir().unwrap();
        let registry = harness(dir.path(), None).await;
        let provider = Scripted::new(vec![text_reply("ok")]);
        let out = Agent::new(&provider, &registry, None)
            .with_skills(vec![(
                "rust".into(),
                "Propagate errors with ? at boundaries.".into(),
            )])
            .run("q", &mut |_| {})
            .await
            .unwrap();
        let seen = provider.seen.lock().await;
        let sys = &seen[0].messages[0].content;
        assert!(sys.contains("### rust"), "skill block missing");
        assert!(sys.contains("Propagate errors"), "skill body missing");
        assert!(
            sys.find("INVIOLABLE ENGINE LAW").unwrap() < sys.find("### rust").unwrap(),
            "law precedes skills"
        );
        assert!(
            out.composed
                .report
                .iter()
                .any(|s| s.name == "skills" && s.kept)
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
            Arc::new(crate::tools::Diagnostics::new()),
            Arc::new(crate::tools::Surfaces::new()),
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
    async fn memory_fault_arms_the_ruleset_and_publishes() {
        use tokio::sync::broadcast;
        let dir = tempfile::tempdir().unwrap();
        // Rebuild with a shared ledger so agent and tools see one truth.
        let gov = Arc::new(
            openchat_governor::Governor::new(openchat_governor::GovernorConfig {
                low_process_priority: false,
                ..Default::default()
            })
            .unwrap(),
        );
        let diag = Arc::new(crate::tools::Diagnostics::new());
        let store = Arc::new(Mutex::new(MemoryStore::open_in_memory(4).unwrap()));
        let registry = crate::tools::standard_registry(
            Arc::clone(&gov),
            dir.path(),
            store,
            None,
            Arc::clone(&diag),
            Arc::new(crate::tools::Surfaces::new()),
        );
        let log = dir.path().join("asan.log");
        std::fs::write(
            &log,
            "==7==ERROR: AddressSanitizer: heap-use-after-free on address 0xfeed\n",
        )
        .unwrap();
        let asan_cmd = tool_reply(
            "c1",
            "run_command",
            json!({"cmd": format!("/bin/cat {}", log.display()), "build": false}),
        );
        let provider = Scripted::new(vec![asan_cmd, text_reply("fixed: owner now drops it")]);
        let (tx, mut rx) = broadcast::channel(64);
        let out = Agent::new(&provider, &registry, None)
            .with_diagnostics(Arc::clone(&diag))
            .with_publisher(tx)
            .run("run it", &mut |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "fixed: owner now drops it");
        let seen = provider.seen.lock().await;
        assert!(
            !seen[0].messages[0]
                .content
                .contains("MEMORY-SAFETY DIAGNOSTIC MODE"),
            "not armed before the fault"
        );
        let second = &seen[1].messages[0].content;
        assert!(
            second.contains("### memory-safety"),
            "ruleset auto-injected"
        );
        assert!(second.contains("RAII first"), "rules content present");
        assert!(diag.is_armed() && diag.count() == 1);
        let mut saw_fault = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, TurnEvent::MemoryFault { ref fault, .. } if fault == "address-sanitizer")
            {
                saw_fault = true;
            }
        }
        assert!(saw_fault, "wire event must surface the trap");
    }

    #[tokio::test]
    async fn code_surface_pack_reaches_following_prompts() {
        use tokio::sync::broadcast;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        let gov = Arc::new(
            openchat_governor::Governor::new(openchat_governor::GovernorConfig {
                low_process_priority: false,
                ..Default::default()
            })
            .unwrap(),
        );
        let surfaces = Arc::new(crate::tools::Surfaces::new());
        let store = Arc::new(Mutex::new(MemoryStore::open_in_memory(4).unwrap()));
        let registry = crate::tools::standard_registry(
            Arc::clone(&gov),
            dir.path(),
            store,
            None,
            Arc::new(crate::tools::Diagnostics::new()),
            Arc::clone(&surfaces),
        );
        let patch = tool_reply(
            "p1",
            "apply_patch",
            json!({"patches": "@@@ a.rs\n<<<<<<< SEARCH\nfn a() {}\n=======\npub fn a() {}\n>>>>>>> REPLACE\n"}),
        );
        let provider = Scripted::new(vec![patch, text_reply("done")]);
        let (tx, mut rx) = broadcast::channel(64);
        Agent::new(&provider, &registry, None)
            .with_surfaces(Arc::clone(&surfaces))
            .with_publisher(tx)
            .run("make a public", &mut |_| {})
            .await
            .unwrap();
        let seen = provider.seen.lock().await;
        assert!(!seen[0].messages[0].content.contains("CODE-SURFACE RULES"));
        let second = &seen[1].messages[0].content;
        assert!(second.contains("### code"), "code pack auto-bound");
        assert!(
            second.contains("No defensive over-engineering"),
            "pack content"
        );
        assert!(surfaces.is_armed("code"));
        let mut announced = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(&ev, TurnEvent::SurfaceArmed { surface, .. } if surface == "code") {
                announced = true;
            }
        }
        assert!(announced, "SurfaceArmed wire event missing");
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
