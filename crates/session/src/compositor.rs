//! Context compositor (Roadmap Phase 4 core, anti-slop Phase 2 item 2).
//!
//! Assembles every model request from purpose-tagged sections under a hard
//! token budget. The assembly is deterministic and observable: each section
//! reports its token cost and whether it survived, so the Phase 4 badge UI
//! has real data instead of guesses.

use crate::provider::ChatMessage;

/// The four engine invariants, compiled into the binary and injected into
/// System Frame #0 of every request (README: "Anti-Slop Law"). Not
/// user- or model-overridable: the compositor puts it before every other
/// section, and the tools enforce it independently of what the model
/// believes the rules are.
pub const ANTI_SLOP_LAW: &str = r#"[INVIOLABLE ENGINE LAW - ZERO-TOLERANCE ANTI-SLOP]
1. ZERO CONVERSATIONAL FILLER: Never output pleasantries, confirmations ("Sure, I can help!"),
   pre-action monologues ("I will now check..."), or post-action summaries.
2. SURGICAL DIFF ONLY: Full-file rewrites are strictly forbidden. Only emit minimal
   search-and-replace hunks.
3. SILENT VERIFICATION: Execute compiler and linter checks autonomously. Never ask the user
   to test what you can test.
4. CODE OVER EXPLANATION: If code was requested, emit ONLY code diffs and compiler
   verification.
ENFORCEMENT: the engine strips leading filler from your output before the user sees it,
and patch tools reject whole-file overwrites with a SlopViolation error. Write for the
filter: diffs and one-line results only."#;

/// The workspace-conventions block: short, factual, project-specific.
pub const BASE_SYSTEM: &str = "\
You are open-chat, an agent running on the user's own machine through a \
governed daemon. Tools are CPU-capped and build-serialized; assume shared \
resources and queue rather than retry storms.
Operating notes under the law above:
- Change files ONLY through the apply_patch tool with SEARCH/REPLACE blocks.
- When unsure of a file's current content, read it with run_command (cat/sed), \
never recall it from the conversation.
- A failing verification keeps the turn open; expect the engine to re-run it.";

/// Auto-injected when the memory-fault trap fires (Phase 2: automated
/// memory-safety skill injection). Names the three areas the README
/// mandates - RAII, weak references, pointer hygiene - plus the buffer
/// discipline that prevents the next fault.
pub const MEMORY_SAFETY_RULES: &str = "\
MEMORY-SAFETY DIAGNOSTIC MODE (armed by the engine after a detected memory fault):
- RAII first: every resource gets an owner whose drop releases it; no manual \
free/close on error paths, no late cleanup that an early return skips.
- Cycles: break parent/child reference cycles with Weak; upgrade() at use and \
drop the guard immediately.
- Pointer hygiene: prefer borrows over raw pointers; every unsafe block states \
its // SAFETY invariant and has a test that exercises it.
- Bounded growth: cap every collection fed by untrusted or streamed input; \
chunk large buffers; never size allocations from a raw length field.
- Leaks at exit are real leaks: rerun the sanitizer/leak-check variant named \
in the diagnostic before claiming repair; 'fine after restart' is not a fix.
- Fix the root cause the report cites; do not paper over it by raising limits.";

/// Code-surface ruleset, auto-bound whenever the turn touches source files
/// (Phase 2 Code-Surface Anti-Slop Skill Pack). Distilled from the
/// full-spectrum matrix and the antislop code-comment skill: it targets
/// defensive over-engineering and decorative noise, and is honest about
/// what the runtime gate can and cannot check.
pub const CODE_SURFACE_RULES: &str = "\
CODE-SURFACE RULES (auto-bound: this turn touches source files):
- No defensive over-engineering: no abstraction, trait, wrapper, factory, \
config flag, or generic parameter until a second real caller needs it. \
Solve the case in front of you.
- No speculative generalization: write the concrete function; rename or \
widen later when reality asks. Premature micro-optimization is banned; \
optimize against a measured number and cite it in the commit or comment.
- Never swallow errors: no bare `except:`, empty catch, or `return nil` on \
a real failure. Handle, propagate, or log with context; the engine gate \
flags avoidable ones.
- Wrap only what buys something: a try/catch, layer, or helper that adds \
no behavior is noise - remove it rather than polishing it.
- Comments carry information the code cannot: invariants, units, workarounds, \
WHY. No banner separators, `// Step 1:` narration, ALL-CAPS labels, or \
comments restating the next line.
- Redundant tests are banned: one test per behavior; do not re-test what \
compiler, linter, or an existing test already proves.
- Emit minimal hunks only: change three lines with three lines, never by \
re-emitting the file.";

/// UI/design/mobile ruleset, auto-bound when presentation files are touched
/// or UI intent is detected. Distilled from the reference antislop skill
/// family (`antislop-ui`, `antislop-layoutmobile`, `antislop-human` from
/// miqdadbadjuber/anti-slop); scoped rules and provenance preserved here
/// and in the skill registry.
pub const UI_SURFACE_RULES: &str = "\
UI-SURFACE RULES (auto-bound: this turn touches UI files):
- No template identity: reject the AI defaults - blue/purple gradients, \
glassmorphism everywhere, glow stacks, bento mosaics, cookie-cutter hero + \
3-card rows. Every visual choice needs a one-line reason tied to hierarchy, \
identity, or readability.
- Contrast first: text must pass WCAG AA (4.5:1 body, 3:1 large) across the \
whole area it travels over; never grey-on-grey or white-on-gradient.
- States are the design: empty, loading, error, disabled, and focus states \
ship with the component, not as follow-ups; interactive elements need \
visible focus and cursor affordances.
- Mobile is not an afterthought: no horizontal overflow, tap targets >= 44px, \
reflow before shrink, respect prefers-reduced-motion.
- Semantics beat divs: real button/a/label/alt; no clickable divs, no ARIA \
that contradicts native roles.
- Work from the project's design direction; if none exists, say the result \
is a draft rather than inventing a house style silently.";

/// Copywriting/prose ruleset, auto-bound when prose files are touched or
/// writing intent is detected. Distilled from the reference antislop
/// family (`antislop-copywriting`, `antislop-human`); pairs with the
/// runtime prose gate (buzzwords, emoji spam) that screens results.
pub const COPY_SURFACE_RULES: &str = "\
COPY-SURFACE RULES (auto-bound: this turn touches prose):
- Direct language: lead with what it is and what changed; cut throat-clearing \
('In today's fast-paced world'), hype nouns (seamless, cutting-edge, \
revolutionary, unleash), and symmetric slogan cadence.
- No em dashes in generated text; use commas, colons, periods, or \
parentheses.
- Specific over impressive: numbers only when real and sourced; no invented \
stats, testimonials, or compliance claims.
- Match the project's existing voice; when editing, preserve the author's \
patterns rather than normalizing them away.
- UI copy: label what the control does ('Save token', not 'Submit'); errors \
say what happened and what to do next.";

/// What survived assembly, per section, for telemetry and debugging.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SectionReport {
    pub name: String,
    pub tokens: usize,
    /// False when the budget forced the section out entirely.
    pub kept: bool,
}

#[derive(Debug, Clone)]
pub struct Composed {
    pub messages: Vec<ChatMessage>,
    pub report: Vec<SectionReport>,
    pub total_tokens: usize,
    /// Rule lines dropped as duplicates across the law and skill bodies
    /// (Phase 4: "deduplicates rules"). Code lines are never touched.
    pub deduped_rule_lines: usize,
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
    /// first: memory tail hits, then least-priority skill bodies, then
    /// older transcript bytes, then oldest history messages. System law
    /// and the user turn are never cut; if those alone exceed the budget
    /// we say so honestly in the report by keeping everything
    /// (`kept: true`) and letting the caller raise the cap, rather than
    /// silently amputating instructions.
    #[must_use]
    pub fn compose(
        &self,
        extra_system: &str,
        skills: &[(&str, &str)],
        memory_hits: &[String],
        transcript: &str,
        history: &[ChatMessage],
        user: &str,
    ) -> Composed {
        // System Frame #0: the law leads the first system message, above
        // conventions, skills, and any caller-supplied extra context.
        let mut head = String::from(ANTI_SLOP_LAW);
        head.push('\n');
        head.push_str(BASE_SYSTEM);
        if !extra_system.is_empty() {
            head.push('\n');
            head.push_str(extra_system);
        }

        // Droppable sections, in the order they lose content.
        let mut skills_kept: Vec<(String, String)> = skills
            .iter()
            .map(|(id, b)| ((*id).to_owned(), (*b).to_owned()))
            .collect();
        let mut memory = memory_hits.to_vec();
        let mut transcript = transcript.to_owned();
        let mut history = history.to_vec();

        let estimate = |s: &str| openchat_core::estimate_tokens(s);
        let skills_text = |sk: &[(String, String)]| -> usize {
            sk.iter()
                .map(|(id, b)| estimate(id) + estimate(b) + 8)
                .sum()
        };
        let report = |sk: &[(String, String)],
                      mem: &[String],
                      tr: &str,
                      hist: &[ChatMessage]|
         -> (Vec<SectionReport>, usize) {
            let sections: [(&'static str, usize, bool); 6] = [
                ("system", estimate(&head), true),
                ("skills", skills_text(sk), !sk.is_empty()),
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
                        name: (*n).to_owned(),
                        tokens: *t,
                        kept: *kept,
                    })
                    .collect(),
                total,
            )
        };

        let (mut rep, mut total) = report(&skills_kept, &memory, &transcript, &history);
        // Memory: drop lowest-ranked tail hits first (recall returns best
        // first, so the cheap part of the budget goes to the best evidence).
        while total > self.max_total_tokens && memory.len() > 1 {
            memory.pop();
            (rep, total) = report(&skills_kept, &memory, &transcript, &history);
        }
        if total > self.max_total_tokens && !memory.is_empty() {
            memory.clear();
            (rep, total) = report(&skills_kept, &memory, &transcript, &history);
        }
        // Skills: already priority-ordered by the caller; evict the tail.
        while total > self.max_total_tokens && !skills_kept.is_empty() {
            skills_kept.pop();
            (rep, total) = report(&skills_kept, &memory, &transcript, &history);
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
            (rep, total) = report(&skills_kept, &memory, &transcript, &history);
        }
        // History: evict oldest messages in whole entries.
        while total > self.max_total_tokens && history.len() > 1 {
            history.remove(0);
            (rep, total) = report(&skills_kept, &memory, &transcript, &history);
        }
        if total > self.max_total_tokens {
            history.clear();
            (rep, total) = report(&skills_kept, &memory, &transcript, &history);
        }

        let mut system = head;
        // Rule dedupe: bullet/heading lines already present (the law, the
        // conventions block, an earlier skill) are not repeated. Short
        // lines and anything that is not a rule marker stay untouched so
        // code examples survive verbatim.
        let mut seen_rules: std::collections::HashSet<String> = system
            .lines()
            .map(str::trim)
            .filter(|l| is_rule_line(l))
            .map(str::to_owned)
            .collect();
        let mut deduped_rule_lines = 0usize;
        if !skills_kept.is_empty() {
            system.push_str("\n\n## Loaded skills (binding for this stack)\n");
            for (id, body) in &skills_kept {
                system.push_str(&format!("\n### {id}\n"));
                for line in body.lines() {
                    let t = line.trim();
                    if is_rule_line(t) && !seen_rules.insert(t.to_owned()) {
                        deduped_rule_lines += 1;
                        continue;
                    }
                    system.push_str(line);
                    system.push('\n');
                }
            }
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

        // Report all six sections even at zero tokens: honest telemetry for the badges.
        Composed {
            messages: out,
            report: rep,
            total_tokens: total,
            deduped_rule_lines,
        }
    }
}

/// Heading or bullet lines long enough to be a real rule, not stray
/// punctuation in a code sample.
fn is_rule_line(t: &str) -> bool {
    (t.starts_with('-') || t.starts_with('#')) && t.len() >= 15
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
        let out = c.compose("", &[], &[], "", &[], "hello");
        let s = system_text(&out);
        assert!(s.starts_with(ANTI_SLOP_LAW), "law must be frame 0");
        assert!(s.contains(BASE_SYSTEM));
        assert_eq!(out.messages.last().unwrap().role, Role::User);
        assert_eq!(out.messages.last().unwrap().content, "hello");
    }

    #[test]
    fn ordering_is_system_history_user() {
        let c = Compositor::new(100_000);
        let history = vec![ChatMessage::user("old q"), ChatMessage::assistant("old a")];
        let out = c.compose("X", &[], &["mem".into()], "tr", &history, "new q");
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
        let out = c.compose("", &[], &hits, "", &[], &format!("q {}", big(3)));
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
        let out = c.compose("", &[], &[], &transcript, &[], "q");
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
        let out = c.compose("", &[], &[], "", &[], "hello");
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
            &[("rust", "body text here")],
            &["alpha".into()],
            "beta",
            &[ChatMessage::user("gamma")],
            "delta",
        );
        let sum: usize = out.report.iter().map(|s| s.tokens).sum();
        assert_eq!(sum, out.total_tokens);
        let sys = out
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .unwrap();
        assert!(
            sys.content.contains("### rust"),
            "skills ride in system frame"
        );
        assert!(
            sys.content.find("INVIOLABLE").unwrap() < sys.content.find("Loaded skills").unwrap()
        );
    }

    #[test]
    fn tight_budget_evicts_skill_tail_after_memory() {
        let c = Compositor::new(440);
        let skills = [("first", "S".repeat(400)), ("second", "T".repeat(400))];
        let pairs: Vec<(&str, &str)> = skills.iter().map(|(a, b)| (*a, b.as_str())).collect();
        let out = c.compose("", &pairs, &[], "", &[], "q");
        let sys = out
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .unwrap();
        assert!(sys.content.contains("first"));
        assert!(!sys.content.contains("T T"), "second skill evicted");
        let sk = out.report.iter().find(|s| s.name == "skills").unwrap();
        assert!(out.total_tokens <= 440, "{}", out.total_tokens);
        assert!(sk.kept);
    }

    #[test]
    fn duplicate_rule_lines_are_deduped_code_is_not() {
        let c = Compositor::new(100_000);
        let shared = "- Always handle errors explicitly at every boundary";
        let s1 = format!("{shared}\nlet x: u8 = \"s\"; // same short text never");
        let s2 = format!("{shared}\n- Another distinct rule line for this skill here");
        let out = c.compose(
            "",
            &[("one", s1.as_str()), ("two", s2.as_str())],
            &[],
            "",
            &[],
            "q",
        );
        let sys = &out.messages[0].content;
        assert_eq!(
            sys.matches(shared).count(),
            1,
            "duplicate rule shipped twice"
        );
        assert_eq!(out.deduped_rule_lines, 1);
        assert!(sys.contains("let x: u8"), "code line survived");
        assert!(sys.contains("Another distinct rule"));
    }

    #[test]
    fn all_skills_evicted_reports_not_kept() {
        let c = Compositor::new(360);
        let body = "Z".repeat(2000);
        let out = c.compose("", &[("huge", body.as_str())], &[], "", &[], "q");
        let sk = out.report.iter().find(|s| s.name == "skills").unwrap();
        assert!(!sk.kept);
        assert!(!out.messages[0].content.contains("Loaded skills"));
    }
}
