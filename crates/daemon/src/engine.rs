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
use serde_json::json;
use tokio::sync::broadcast;

use crate::config::{LlmSettings, SkillsSettings, load};

/// Dense tier dimensions. v1 pins one embedding model's width for the whole
/// store; per-model migration lands with the LLM layer.
pub const VECTOR_DIMS: usize = 768;

pub struct Engine {
    pub cwd: PathBuf,
    pub governor: Arc<Governor>,
    pub store: SharedMemory,
    pub llm: LlmSettings,
    pub skills: SkillsSettings,
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
        let (governor_config, llm, skills) =
            load(cwd, config_path).context("failed to load open-chat config")?;
        let governor =
            Arc::new(Governor::new(governor_config).context("failed to start resource governor")?);
        let store = Arc::new(tokio::sync::Mutex::new(
            MemoryStore::open(&cwd.join(".openchat").join("memory.sqlite"), VECTOR_DIMS)
                .context("failed to open workspace memory")?,
        ));
        Ok(Self {
            cwd: cwd.to_path_buf(),
            governor,
            store,
            llm,
            skills,
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

        let verifier = self.llm.verify_command.clone().or_else(|| {
            self.cwd
                .join("Cargo.toml")
                .is_file()
                .then(|| vec!["cargo".into(), "check".into(), "--workspace".into()])
        });
        let registry = standard_registry(
            Arc::clone(&self.governor),
            &self.cwd,
            Arc::clone(&self.store),
            verifier,
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

        let mut agent = Agent::new(&provider, &registry, Some(Arc::clone(&self.store)))
            .with_budget(self.llm.max_total_tokens, 8)
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
        })
    }
}
