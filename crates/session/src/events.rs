//! Wire-level turn events (Phase 4 telemetry -> Phase 5 transports).
//!
//! [`TurnEvent`] is the single JSON contract every interface speaks: the
//! CLI, the SSE server, Telegram, Discord. [`crate::agent::AgentEvent`] is
//! the in-process view; the agent publishes both, so a client can attach
//! mid-turn by replaying buffered events (idempotency + replay live on
//! the server side, see daemon `serve`).

use serde::{Deserialize, Serialize};

/// One skill's contribution, as reported on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillBadge {
    pub id: String,
    pub tokens: usize,
    pub capped: bool,
}

/// Everything observable about a closed turn, on one serializable value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Badges {
    pub stack: String,
    pub skills: Vec<SkillBadge>,
    pub prompt_tokens: usize,
    pub cores: usize,
    pub usable_cores: usize,
    pub free_build_slots: usize,
    pub memory_entries: usize,
    pub slop_stripped_bytes: usize,
    /// Rule lines deduplicated out of skill bodies by the compositor.
    pub deduped_rule_lines: usize,
    /// Memory-fault trap hits this turn (diagnostics armed or not).
    pub memory_faults: usize,
    /// Task surfaces auto-bound this turn ("code", later "ui", "copy").
    pub armed_surfaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    TurnStarted {
        prompt: String,
    },
    Iteration {
        index: usize,
        total_tokens: usize,
        sections: Vec<crate::compositor::SectionReport>,
    },
    AssistantText {
        text: String,
    },
    ToolCalled {
        name: String,
        ok: bool,
        digest: String,
    },
    FinalVerification {
        ok: bool,
    },
    SlopStripped {
        bytes: usize,
    },
    /// The memory-fault diagnostic trap fired this turn (sanitizer report,
    /// OOM kill, segfault...). The memory-safety ruleset is auto-injected
    /// into the following prompts until the turn closes.
    MemoryFault {
        fault: String,
        headline: String,
        advice: String,
    },
    /// A task surface (code, later ui/copy) was auto-bound for this turn;
    /// its ruleset rides every following prompt.
    SurfaceArmed {
        surface: String,
        reason: String,
    },
    /// Turn finished; `text` is the clean final answer.
    TurnClosed {
        text: String,
        iterations: usize,
        badges: Badges,
    },
    /// Turn failed (provider error, budget). Message is user-presentable.
    TurnFailed {
        error: String,
    },
}
