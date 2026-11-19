//! Context compositor (Roadmap Phase 4 core, anti-slop Phase 2 item 2).
//!
//! Assembles every model request from purpose-tagged sections under a hard
//! token budget. The assembly is deterministic and observable: each section
//! reports its token cost and whether it survived, so the Phase 4 badge UI
//! has real data instead of guesses.

use crate::provider::ChatMessage;

/// Strict `[during]` guardrails, injected into the system section of every
/// request: the negative constraints that make output surgical rather than
/// conversational. This is the enforcement point for Phase 2 item 2.
pub const ANTI_SLOP_DURING: &str = "\
WORKING MODE [during]: surgical, low-output, high-signal.
- Change files ONLY through the apply_patch tool with SEARCH/REPLACE blocks. \
Never emit full file contents, in chat or in tool arguments.
- No pleasantries, no pre-tool monologue, no post-work summaries. One short \
line of result plus evidence is the entire chat output.
- After any patch, treat the project's verification command as mandatory: it \
runs automatically before the turn closes; a failing verification keeps the \
turn open until fixed.
- Do not claim anything about code you did not execute or read.
- When unsure of a file's current content, read it with run_command (cat/sed), \
never recall it from the conversation.";

/// The workspace-conventions block: short, factual, project-specific.
pub const BASE_SYSTEM: &str = "\
You are open-chat, an agent running on the user's own machine through a \
governed daemon. Tools are CPU-capped and build-serialized; assume shared \
resources and queue rather than retry storms.";

/// What survived assembly, per section, for telemetry and debugging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionReport {
    pub name: &'static str,
    pub tokens: usize,
    /// False when the budget forced the section out entirely.
    pub kept: bool,
}

#[derive(Debug, Clone)]
pub struct Composed {
    pub messages: Vec<ChatMessage>,
    pub report: Vec<SectionReport>,
    pub total_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct Compositor {
    /// Ceiling for the whole request, README "deterministic budgeting".
    pub max_total_tokens: usize,
}

impl Compositor {
    #[must_use]
    pub const fn new(max_total_tokens: usize) -> Self {
        Self { max_total_tokens }
    }

    /// Compose one request. Trimming priority when over budget, worst off
    /// first: memory tail hits, then older transcript bytes, then oldest
    /// history messages. System and the user turn are never cut; if those
    /// alone exceed the budget we say so honestly in the report by
    /// keeping everything (`kept: true`) and letting the caller raise the
    /// cap, rather than silently amputating instructions.
    #[must_use]
    pub fn compose(
        &self,
        extra_system: &str,
        memory_hits: &[String],
        transcript: &str,
        history: &[ChatMessage],
        user: &str,
    ) -> Composed {
        let mut system = String::from(BASE_SYSTEM);
        system.push('\n');
        system.push_str(ANTI_SLOP_DURING);
        if !extra_system.is_empty() {
            system.push('\n');
            system.push_str(extra_system);
        }

        // Droppable sections, in the order they lose content.
        let mut memory = memory_hits.to_vec();
        let mut transcript = transcript.to_owned();
        let mut history = history.to_vec();

        let estimate = |s: &str| openchat_core::estimate_tokens(s);
        let report =
            |mem: &[String], tr: &str, hist: &[ChatMessage]| -> (Vec<SectionReport>, usize) {
                let sections: [(&'static str, usize, bool); 5] = [
                    ("system", estimate(&system), true),
                    (
                        "memory",
                        mem.iter().map(|h| estimate(h) + 2).sum(),
                        !mem.is_empty(),
                    ),
                    ("transcript", estimate(tr), !tr.is_empty()),
                    (
                        "history",
                        hist.iter().map(|m| estimate(&m.content) + 4).sum(),
                        !hist.is_empty(),
                    ),
                    ("user", estimate(user), true),
                ];
                let total = sections.iter().map(|(_, t, _)| t).sum();
                (
                    sections
                        .iter()
                        .map(|(n, t, kept)| SectionReport {
                            name: n,
                            tokens: *t,
                            kept: *kept,
                        })
                        .collect(),
                    total,
                )
            };

        let (mut rep, mut total) = report(&memory, &transcript, &history);
        // Memory: drop lowest-ranked tail hits first (recall returns best
        // first, so the cheap part of the budget goes to the best evidence).
        while total > self.max_total_tokens && memory.len() > 1 {
            memory.pop();
            (rep, total) = report(&memory, &transcript, &history);
        }
        if total > self.max_total_tokens && !memory.is_empty() {
            memory.clear();
            (rep, total) = report(&memory, &transcript, &history);
        }
        // Transcript: keep the freshest tail (that is where live failures
        // are), folding the rest with an explicit marker.
        while total > self.max_total_tokens && !transcript.is_empty() {
            let keep_bytes = (estimate(&transcript).saturating_mul(4) * 3 / 4).max(256);
            if keep_bytes >= transcript.len() {
                break; // marker alone won't fit either; history must yield
            }
            let mut cut = transcript.len() - keep_bytes;
            while !transcript.is_char_boundary(cut) {
                cut += 1;
            }
            transcript = format!("[older output folded]\n{}", &transcript[cut..]);
            (rep, total) = report(&memory, &transcript, &history);
        }
        // History: evict oldest messages in whole entries.
        while total > self.max_total_tokens && history.len() > 1 {
            history.remove(0);
            (rep, total) = report(&memory, &transcript, &history);
        }
        if total > self.max_total_tokens {
            history.clear();
            (rep, total) = report(&memory, &transcript, &history);
        }

        let mut out: Vec<ChatMessage> = Vec::with_capacity(memory.len() + history.len() + 3);
        out.push(ChatMessage::system(system));
        if let Some(mem_block) = render_memory(&memory) {
            out.push(ChatMessage::system(mem_block));
        }
        if !transcript.is_empty() {
            out.push(ChatMessage::system(format!(
                "Recent tool activity:\n{transcript}"
            )));
        }
        out.extend(history.iter().cloned());
        out.push(ChatMessage::user(user.to_owned()));

        rep.retain(|s| s.tokens > 0 || s.name == "system" || s.name == "user");
        Composed {
            messages: out,
            report: rep,
            total_tokens: total,
        }
    }
}

fn render_memory(hits: &[String]) -> Option<String> {
    if hits.is_empty() {
        return None;
    }
    let mut block = String::from("Recalled project memory (authoritative for conventions):\n");
    for hit in hits {
        block.push_str("- ");
        block.push_str(hit);
        block.push('\n');
    }
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;

    const FILLER: &str = "the quick brown fox jumps over the lazy dog "; // 10ish tokens per 45 bytes

    fn big(n: usize) -> String {
        FILLER.repeat(n)
    }

    fn system_text(c: &Composed) -> String {
        c.messages
            .iter()
            .find(|m| m.role == Role::System)
            .expect("has system")
            .content
            .clone()
    }

    #[test]
    fn anti_slop_block_always_present() {
        let c = Compositor::new(100_000);
        let out = c.compose("", &[], "", &[], "hello");
        let s = system_text(&out);
        assert!(s.contains(ANTI_SLOP_DURING), "guardrails missing");
        assert!(s.contains(BASE_SYSTEM));
        assert_eq!(out.messages.last().unwrap().role, Role::User);
        assert_eq!(out.messages.last().unwrap().content, "hello");
    }

    #[test]
    fn ordering_is_system_history_user() {
        let c = Compositor::new(100_000);
        let history = vec![ChatMessage::user("old q"), ChatMessage::assistant("old a")];
        let out = c.compose("X", &["mem".into()], "tr", &history, "new q");
        let roles: Vec<Role> = out.messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                Role::System,
                Role::System,
                Role::System,
                Role::User,
                Role::Assistant,
                Role::User
            ]
        );
        assert!(out.messages[0].content.contains("X"));
        assert_eq!(out.messages[5].content, "new q");
        assert_eq!(out.messages[3].content, "old q");
    }

    #[test]
    fn budget_drops_memory_tail_first() {
        let c = Compositor::new(700);
        let hits: Vec<String> = (0..8).map(|i| format!("hit{i} {}", big(20))).collect();
        let out = c.compose("", &hits, "", &[], &format!("q {}", big(3)));
        let mem = out
            .report
            .iter()
            .find(|s| s.name == "memory")
            .expect("mem section");
        assert!(mem.kept);
        assert!(
            out.total_tokens <= 700,
            "within tolerance: {}",
            out.total_tokens
        );
        // Exactly one hit survives: the best-ranked one.
        assert!(
            out.messages.iter().any(|m| m.content.contains("hit0")),
            "top hit kept"
        );
        assert!(!out.messages.iter().any(|m| m.content.contains("hit1")));
        assert!(!out.messages.iter().any(|m| m.content.contains("hit7")));
    }

    #[test]
    fn transcript_folds_to_freshest_tail() {
        let c = Compositor::new(600);
        let transcript = format!("{}FRESH LINE", big(200));
        let out = c.compose("", &[], &transcript, &[], "q");
        let rendered = out
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\u{1}");
        assert!(rendered.contains("FRESH LINE"), "newest tail must survive");
        assert!(
            rendered.contains("older output folded"),
            "fold marker present"
        );
        assert!(out.total_tokens <= 660, "{}", out.total_tokens);
    }

    #[test]
    fn mandatory_sections_never_get_silently_cut() {
        let c = Compositor::new(10); // impossible budget
        let out = c.compose("", &[], "", &[], "hello");
        assert!(out.messages.iter().any(|m| m.role == Role::System));
        assert_eq!(out.messages.last().unwrap().content, "hello");
        let sys = out.report.iter().find(|s| s.name == "system").unwrap();
        assert!(sys.kept, "system is mandatory and reported as kept");
    }

    #[test]
    fn report_sums_match_estimate() {
        let c = Compositor::new(100_000);
        let out = c.compose(
            "",
            &["alpha".into()],
            "beta",
            &[ChatMessage::user("gamma")],
            "delta",
        );
        let sum: usize = out.report.iter().map(|s| s.tokens).sum();
        assert_eq!(sum, out.total_tokens);
    }
}
