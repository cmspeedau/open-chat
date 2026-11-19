//! The assembled engine: governor + memory + skills + provider, ready to
//! run turns. One builder shared by every front door (`chat`, `serve`,
//! `telegram`) so all interfaces execute identical policy.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use openchat_governor::Governor;
use openchat_memory::MemoryStore;
use openchat_session::agent::{Agent, AgentError, AgentEvent, AgentOutcome};
use openchat_session::events::{Badges, SkillBadge, TurnEvent};
use openchat_session::provider::{OpenAi, OpenAiConfig};
use openchat_session::tools::{SharedMemory, standard_registry};
use openchat_telegram as tg;
use openchat_telegram::EnginePort;
use serde_json::json;
use tokio::sync::broadcast;

use crate::config::{LlmSettings, SkillsSettings, TelegramSettings, load};

/// Dense tier dimensions. v1 pins one embedding model's width for the whole
/// store; per-model migration lands with the LLM layer.
pub const VECTOR_DIMS: usize = 768;

pub struct Engine {
    pub cwd: PathBuf,
    pub governor: Arc<Governor>,
    pub store: SharedMemory,
    pub llm: LlmSettings,
    pub skills: SkillsSettings,
    pub telegram: TelegramSettings,
    /// Selected verification command (label + argv), if the machine can run one.
    pub verifier: Option<(String, Vec<String>)>,
    /// Stack-implied checks unavailable on this host, disclosed not faked.
    pub checks_skipped: Vec<String>,
}

/// Pick the verification command from the stack, only choosing tools the
/// machine actually has; every implied-but-missing check is disclosed.
/// An explicit `[llm] verify_command` always wins.
#[must_use]
pub fn detect_verifier(
    cwd: &std::path::Path,
    fp: &openchat_skills::Fingerprint,
    override_cmd: Option<&Vec<String>>,
) -> (Option<(String, Vec<String>)>, Vec<String>) {
    if let Some(cmd) = override_cmd {
        return (Some((cmd.join(" "), cmd.clone())), Vec::new());
    }
    let mut skipped = Vec::new();
    for lang in &fp.languages {
        let (label, cmd, has): (&str, Vec<&str>, bool) = match lang {
            openchat_skills::Language::Rust => (
                "cargo check --workspace",
                vec!["cargo", "check", "--workspace"],
                openchat_tools::format::which("cargo"),
            ),
            openchat_skills::Language::TypeScript | openchat_skills::Language::JavaScript => (
                "tsc --noEmit",
                vec!["npx", "--no-install", "tsc", "--noEmit"],
                cwd.join("tsconfig.json").is_file()
                    && (cwd.join("node_modules/.bin/tsc").exists()
                        || openchat_tools::format::which("tsc")),
            ),
            openchat_skills::Language::Go => (
                "go vet ./...",
                vec!["go", "vet", "./..."],
                openchat_tools::format::which("go"),
            ),
            openchat_skills::Language::Python => (
                "ruff check .",
                vec!["ruff", "check", "."],
                openchat_tools::format::which("ruff"),
            ),
            other => {
                skipped.push(format!("{}: no verifier implemented", other.as_str()));
                continue;
            }
        };
        if has {
            return (
                Some((
                    label.to_owned(),
                    cmd.into_iter().map(str::to_owned).collect(),
                )),
                skipped,
            );
        }
        skipped.push(format!("{}: {label} unavailable", lang.as_str()));
    }
    (None, skipped)
}

/// Keyword-based intent routing: prose/design asks arm their surface even
/// before a file is touched ("intent- and manifest-based routing beyond
/// extensions").
#[must_use]
pub fn intent_surfaces(prompt: &str) -> Vec<(&'static str, &'static str)> {
    let p = prompt.to_ascii_lowercase();
    let mut hits = Vec::new();
    if [
        "design",
        "css",
        "layout",
        "styling",
        "frontend",
        "component",
        "landing",
        "responsive",
    ]
    .iter()
    .any(|k| p.contains(k))
    {
        hits.push(("ui", "prompt intent suggests UI work"));
    }
    if [
        "copywriting",
        "headline",
        "tagline",
        "prose",
        "blog",
        "readme",
        "docs page",
        "marketing",
    ]
    .iter()
    .any(|k| p.contains(k))
    {
        hits.push(("copy", "prompt intent suggests prose work"));
    }
    hits
}

/// Skill catalog search roots: configured paths, then project-local, then
/// user-global (`load_roots` layers override by id, so project wins).
pub fn skill_roots(cwd: &Path, cfg: &SkillsSettings) -> Vec<PathBuf> {
    let mut roots = cfg.paths.clone();
    roots.push(cwd.join(".openchat").join("skills"));
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".openchat").join("skills"));
    }
    roots
}

impl Engine {
    pub fn build(cwd: &Path, config_path: Option<&Path>) -> Result<Self> {
        let (governor_config, llm, skills, telegram) =
            load(cwd, config_path).context("failed to load open-chat config")?;
        let governor =
            Arc::new(Governor::new(governor_config).context("failed to start resource governor")?);
        let store = Arc::new(tokio::sync::Mutex::new(
            MemoryStore::open(&cwd.join(".openchat").join("memory.sqlite"), VECTOR_DIMS)
                .context("failed to open workspace memory")?,
        ));
        let fp = openchat_skills::fingerprint(cwd);
        let (verifier, checks_skipped) = detect_verifier(cwd, &fp, llm.verify_command.as_ref());
        if let Some((label, _)) = &verifier {
            tracing::info!(verifier = %label, "verification command selected");
        }
        for note in &checks_skipped {
            tracing::warn!(check = %note, "verifier unavailable, disclosing instead of faking");
        }
        Ok(Self {
            cwd: cwd.to_path_buf(),
            governor,
            store,
            llm,
            skills,
            telegram,
            verifier,
            checks_skipped,
        })
    }

    /// Run one full agent turn. `publisher` receives the wire-level
    /// [`TurnEvent`] stream (SSE bots attach); `on_event` is the
    /// in-process view for interactive CLIs.
    pub async fn run_turn(
        &self,
        prompt: &str,
        publisher: Option<broadcast::Sender<TurnEvent>>,
        on_event: &mut (dyn FnMut(&AgentEvent<'_>) + Send),
    ) -> std::result::Result<AgentOutcome, AgentError> {
        let api_key = std::env::var(&self.llm.api_key_env).unwrap_or_default();
        let provider = OpenAi::new(OpenAiConfig {
            base_url: self.llm.base_url.clone(),
            api_key,
            model: self.llm.model.clone(),
        })
        .map_err(AgentError::Provider)?;

        // Stack -> skills, resolved at turn start.
        let fp = openchat_skills::fingerprint(&self.cwd);
        let catalog = openchat_skills::load_roots(&skill_roots(&self.cwd, &self.skills));
        let prompt_skills = openchat_skills::prompt_set(
            &fp,
            &catalog,
            self.skills.per_skill_max_tokens,
            self.skills.total_max_tokens,
        );

        let diagnostics = Arc::new(openchat_session::tools::Diagnostics::new());
        let surfaces = Arc::new(openchat_session::tools::Surfaces::new());
        // Code stack => the code-surface pack is applicable before the
        // first patch even lands; patches arm it too (belt and braces).
        if !fp.languages.is_empty() {
            surfaces.arm("code", format!("detected code stack: {}", fp.stack_tag()));
        }
        // UI manifest evidence (components/styles present) pre-binds the UI pack.
        let ui_evidence = ["tsx", "jsx", "css", "scss", "html"]
            .iter()
            .any(|e| fp.counts.get(*e).is_some_and(|n| *n > 0));
        if ui_evidence {
            surfaces.arm("ui", "workspace contains UI files");
        }
        for (surface, reason) in intent_surfaces(prompt) {
            surfaces.arm(surface, reason);
        }
        let verifier = self.verifier.as_ref().map(|(_, args)| args.clone());
        let registry = standard_registry(
            Arc::clone(&self.governor),
            &self.cwd,
            Arc::clone(&self.store),
            verifier,
            Arc::clone(&diagnostics),
            Arc::clone(&surfaces),
        );

        let badges_stack = fp.stack_tag();
        let badge_skills: Vec<SkillBadge> = prompt_skills
            .iter()
            .map(|s| SkillBadge {
                id: s.id.clone(),
                tokens: s.tokens,
                capped: s.capped,
            })
            .collect();
        let governor = Arc::clone(&self.governor);
        let store = Arc::clone(&self.store);
        let badges_diag = Arc::clone(&diagnostics);
        let badges_surfaces = Arc::clone(&surfaces);
        let badges_checks_run: Vec<String> = self
            .verifier
            .as_ref()
            .map(|(label, _)| vec![label.clone()])
            .unwrap_or_default();
        let badges_checks_skipped = self.checks_skipped.clone();

        let mut agent = Agent::new(&provider, &registry, Some(Arc::clone(&self.store)))
            .with_budget(self.llm.max_total_tokens, 8)
            .with_diagnostics(Arc::clone(&diagnostics))
            .with_surfaces(Arc::clone(&surfaces))
            .with_skills(
                prompt_skills
                    .iter()
                    .map(|s| (s.id.clone(), s.body.clone()))
                    .collect(),
            )
            .with_badges(move |composed, slop| {
                let snap = governor.snapshot();
                Badges {
                    stack: badges_stack.clone(),
                    skills: badge_skills.clone(),
                    prompt_tokens: composed.total_tokens,
                    cores: snap.cores,
                    usable_cores: snap.usable_total,
                    free_build_slots: snap.free_build_slots,
                    memory_entries: store.try_lock().map_or(0, |m| m.count().unwrap_or(0)),
                    slop_stripped_bytes: slop,
                    deduped_rule_lines: composed.deduped_rule_lines,
                    memory_faults: badges_diag.count(),
                    prose_findings: badges_surfaces.findings(),
                    checks_run: badges_checks_run.clone(),
                    checks_skipped: badges_checks_skipped.clone(),
                    armed_surfaces: badges_surfaces
                        .list()
                        .into_iter()
                        .map(|(name, _)| name)
                        .collect(),
                }
            });
        if let Some(tx) = publisher {
            agent = agent.with_publisher(tx);
        }
        agent.run(prompt, on_event).await
    }

    /// Live core-budget change (`/cores 2` from any interface).
    ///
    /// # Errors
    ///
    /// Text reason when the spec is invalid or exceeds the machine.
    pub fn set_cores(&self, spec: &str) -> std::result::Result<String, String> {
        use openchat_governor::CpuCores;
        let parsed = if let Some(p) = spec.trim().strip_suffix('%') {
            CpuCores::Percent(
                p.trim()
                    .parse::<u8>()
                    .map_err(|_| "percent must be 1-100")?,
            )
        } else {
            CpuCores::Count(
                spec.trim()
                    .parse::<usize>()
                    .map_err(|_| "expected a core count or N%")?,
            )
        };
        self.governor.rescope(parsed).map_err(|e| e.to_string())?;
        Ok(format!("cpu cap now {} cores", self.governor.cpus().len()))
    }

    /// Status document for `GET /status` and `/status` chat commands.
    pub async fn status_json(&self) -> serde_json::Value {
        let snap = self.governor.snapshot();
        let fp = openchat_skills::fingerprint(&self.cwd);
        let catalog = openchat_skills::load_roots(&skill_roots(&self.cwd, &self.skills));
        let (bindings, gaps) = openchat_skills::bind(&fp, &catalog);
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "governor": {
                "cores": snap.cores,
                "usable_total": snap.usable_total,
                "free_build_slots": snap.free_build_slots,
                "memory_free_ratio": snap.memory_free_ratio,
                "nice_applied": snap.nice_applied,
            },
            "memory_entries": self.store.lock().await.count().unwrap_or(usize::MAX),
            "stack": fp.stack_tag(),
            "active_skills": bindings.iter().map(|b| b.skill.id.as_str()).collect::<Vec<_>>(),
            "skill_gaps": gaps.iter().map(|g| g.language.as_str()).collect::<Vec<_>>(),
            "llm_model": self.llm.model,
            "verifier": self.verifier.as_ref().map(|(label, _)| label.clone()),
            "checks_skipped": self.checks_skipped,
        })
    }
}

/// Telegram-facing view of the engine: turns report progress lines as
/// they happen so the bridge can edit its live status message.
impl EnginePort for Engine {
    fn run_turn(
        &self,
        prompt: String,
        progress: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> tg::BoxFuture<'_, std::result::Result<String, String>> {
        Box::pin(async move {
            let mut on_event = |ev: &AgentEvent<'_>| {
                let line = match ev {
                    AgentEvent::Iteration { index, composed } => {
                        format!("… iteration {index} ({}t prompt)", composed.total_tokens)
                    }
                    AgentEvent::ToolCalled { name, ok, digest } => {
                        format!("{} {name}: {digest}", if *ok { "✓" } else { "✗" })
                    }
                    AgentEvent::FinalVerification { ok } => format!(
                        "verification {}",
                        if *ok { "green" } else { "RED, repair demanded" }
                    ),
                    AgentEvent::AssistantText(_) => return,
                };
                let _ = progress.send(line);
            };
            match Engine::run_turn(self, &prompt, None, &mut on_event).await {
                Ok(out) => Ok(out.text),
                Err(e) => Err(e.to_string()),
            }
        })
    }

    fn status_json(&self) -> tg::BoxFuture<'_, serde_json::Value> {
        Box::pin(Engine::status_json(self))
    }

    fn set_cores(&self, spec: String) -> std::result::Result<String, String> {
        Engine::set_cores(self, &spec)
    }

    fn skill_report(&self) -> tg::BoxFuture<'_, serde_json::Value> {
        Box::pin(async {
            let fp = openchat_skills::fingerprint(&self.cwd);
            let catalog = openchat_skills::load_roots(&skill_roots(&self.cwd, &self.skills));
            let (bindings, gaps) = openchat_skills::bind(&fp, &catalog);
            serde_json::json!({
                "supported": true,
                "stack": fp.stack_tag(),
                "active": bindings.iter().map(|b| b.skill.id.as_str()).collect::<Vec<_>>(),
                "gaps": gaps.iter().map(|g| g.language.as_str()).collect::<Vec<_>>(),
            })
        })
    }

    fn bootstrap_skill(&self, lang: String) -> std::result::Result<String, String> {
        let found = openchat_skills::Language::ALL
            .iter()
            .copied()
            .find(|l| l.as_str() == lang.trim().to_ascii_lowercase())
            .ok_or_else(|| format!("unknown language {lang:?}"))?;
        openchat_skills::bootstrap(&self.cwd, found)
            .map(|p| format!("created {}", p.display()))
            .map_err(|e| e.to_string())
    }
}

/// Update offset persisted in project memory: survives restarts alongside
/// everything else the daemon remembers.
pub struct MemoryOffsetStore {
    store: SharedMemory,
}

impl MemoryOffsetStore {
    #[must_use]
    pub const fn new(store: SharedMemory) -> Self {
        Self { store }
    }
}

impl tg::OffsetStore for MemoryOffsetStore {
    fn load(&self) -> tg::BoxFuture<'_, i64> {
        Box::pin(async {
            let store = self.store.lock().await;
            match store.get(openchat_memory::MemoryScope::Project, "telegram-offset") {
                Ok(Some(entry)) => entry.value.parse().unwrap_or(0),
                _ => 0,
            }
        })
    }

    fn save(&self, offset: i64) -> tg::BoxFuture<'_, ()> {
        Box::pin(async move {
            let store = self.store.lock().await;
            let entry = openchat_memory::MemoryEntry {
                scope: openchat_memory::MemoryScope::Project,
                category: "telegram".into(),
                key: "telegram-offset".into(),
                value: offset.to_string(),
                embedding: None,
            };
            if let Err(e) = store.store(&entry) {
                tracing::warn!(error = %e, "telegram offset persist failed");
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openchat_skills::{Fingerprint, Language};
    use std::collections::BTreeMap;

    fn fp(langs: &[Language]) -> Fingerprint {
        Fingerprint {
            languages: langs.to_vec(),
            manifests: vec![],
            counts: BTreeMap::new(),
            files_scanned: 0,
        }
    }

    #[test]
    fn explicit_verify_wins() {
        let (v, skipped) = detect_verifier(
            std::path::Path::new("/nowhere"),
            &fp(&[Language::Rust]),
            Some(&vec!["true".into()]),
        );
        assert_eq!(v.unwrap().0, "true");
        assert!(skipped.is_empty());
    }

    #[test]
    fn rust_stack_picks_cargo_when_present() {
        let (v, skipped) =
            detect_verifier(std::path::Path::new("/tmp"), &fp(&[Language::Rust]), None);
        if openchat_tools::format::which("cargo") {
            assert_eq!(v.unwrap().0, "cargo check --workspace");
            assert!(skipped.is_empty());
        } else {
            assert!(v.is_none());
            assert_eq!(skipped, ["rust: cargo check --workspace unavailable"]);
        }
    }

    #[test]
    fn missing_tools_are_disclosed_not_faked() {
        // Zig has no implemented verifier: disclosure, never a silent pass.
        let (v, skipped) =
            detect_verifier(std::path::Path::new("/tmp"), &fp(&[Language::Zig]), None);
        assert!(v.is_none());
        assert_eq!(skipped, ["zig: no verifier implemented"]);
    }

    #[test]
    fn intent_routing_flags_ui_and_copy() {
        assert!(
            intent_surfaces("redesign the landing page styling")
                .contains(&("ui", "prompt intent suggests UI work"))
        );
        assert!(
            intent_surfaces("write the blog post headline")
                .iter()
                .any(|(s, _)| *s == "copy")
        );
        assert!(intent_surfaces("fix the parser loop").is_empty());
    }
}
