//! `config.toml` loading: `.openchat/config.toml` (project) merged over
//! `~/.openchat/config.toml` (user), matching the README's `[resources]`
//! contract.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use openchat_governor::{CpuCores, GovernorConfig};

/// On-disk shape; every key optional so partial files inherit defaults.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    resources: RawResources,
    #[serde(default)]
    llm: RawLlm,
    #[serde(default)]
    skills: RawSkills,
    #[serde(default)]
    telegram: RawTelegram,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTelegram {
    token_env: Option<String>,
    chat_ids: Option<Vec<i64>>,
    api_base: Option<String>,
}

/// Telegram gateway settings. The key lives in the named env var, never
/// in the file; `chat_ids` is the hard allowlist the bridge demands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramSettings {
    pub token_env: String,
    pub chat_ids: Vec<i64>,
    pub api_base: String,
}

impl Default for TelegramSettings {
    fn default() -> Self {
        Self {
            token_env: "TELEGRAM_BOT_TOKEN".into(),
            chat_ids: Vec::new(),
            api_base: "https://api.telegram.org".into(),
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSkills {
    paths: Option<Vec<String>>,
    per_skill_max_tokens: Option<usize>,
    total_max_tokens: Option<usize>,
}

/// Skill catalog roots and prompt-budget caps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillsSettings {
    pub paths: Vec<PathBuf>,
    /// Clamp for one skill body before it enters the prompt.
    pub per_skill_max_tokens: usize,
    /// Ceiling for the whole skills section (README badge overhead).
    pub total_max_tokens: usize,
}

impl Default for SkillsSettings {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            // Chosen so ~4 focused skills fit a laptop-sized context
            // window while leaving the 24k request mostly user content.
            per_skill_max_tokens: 1_200,
            total_max_tokens: 4_800,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLlm {
    /// "openai-compat" (default) or "opencode" (gateway via opencode serve).
    provider: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
    /// Env var holding the opencode server Basic-auth password.
    password_env: Option<String>,
    verify_command: Option<Vec<String>>,
    max_total_tokens: Option<usize>,
}

/// Effective LLM endpoint settings after layer merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmSettings {
    /// "openai-compat" | "opencode".
    pub provider: String,
    pub base_url: String,
    pub model: String,
    /// Name of the environment variable holding the API key; the key
    /// itself never lives in config files.
    pub api_key_env: String,
    pub password_env: String,
    /// Command proving a patched tree is healthy before a turn closes.
    pub verify_command: Option<Vec<String>>,
    pub max_total_tokens: usize,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: "openai-compat".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            password_env: "OPENCODE_PASSWORD".into(),
            verify_command: None,
            max_total_tokens: 24_000,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResources {
    max_cpu_cores: Option<CoreSpec>,
    max_concurrent_builds: Option<usize>,
    max_worker_memory: Option<String>,
    low_process_priority: Option<bool>,
    memory_low_water: Option<f64>,
}

/// `max_cpu_cores = 4` or `max_cpu_cores = "50%"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum CoreSpec {
    Count(u64),
    Percent(String),
}

/// `max_worker_memory = "4GB"` or `"40%"` of total RAM (v1: absolute only;
/// percentage needs MemTotal and is a tracked follow-up).
fn parse_memory(spec: &str) -> Result<u64> {
    let spec = spec.trim();
    let (num, mult): (&str, u64) = if let Some(n) = spec.strip_suffix("KB") {
        (n, 1024)
    } else if let Some(n) = spec.strip_suffix("MB") {
        (n, 1024 * 1024)
    } else if let Some(n) = spec.strip_suffix("GB") {
        (n, 1024 * 1024 * 1024)
    } else if let Some(n) = spec.strip_suffix("TB") {
        (n, 1024u64.pow(4))
    } else {
        (spec, 1)
    };
    let value: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("invalid memory size {spec:?}"))?;
    Ok((value * mult as f64) as u64)
}

impl RawConfig {
    fn merge_resources(self, base: GovernorConfig) -> Result<GovernorConfig> {
        let r = self.resources;
        let max_cpu_cores = match r.max_cpu_cores {
            None => base.max_cpu_cores,
            Some(CoreSpec::Count(n)) => CpuCores::Count(n as usize),
            Some(CoreSpec::Percent(s)) => {
                let pct = s
                    .trim()
                    .strip_suffix('%')
                    .with_context(|| format!("max_cpu_cores {s:?}: expected `N` or `N%`"))?
                    .trim()
                    .parse::<u8>()
                    .with_context(|| format!("max_cpu_cores {s:?}: bad percentage"))?;
                CpuCores::Percent(pct)
            }
        };
        let memory_low_water = r.memory_low_water.unwrap_or(base.memory_low_water);
        if !(0.0..=1.0).contains(&memory_low_water) {
            anyhow::bail!("memory_low_water must be within 0.0..=1.0, got {memory_low_water}");
        }
        Ok(GovernorConfig {
            max_cpu_cores,
            max_concurrent_builds: r
                .max_concurrent_builds
                .unwrap_or(base.max_concurrent_builds),
            max_worker_memory: match r.max_worker_memory.as_deref() {
                Some(spec) => parse_memory(spec)?,
                None => base.max_worker_memory,
            },
            low_process_priority: r.low_process_priority.unwrap_or(base.low_process_priority),
            memory_low_water,
        })
    }

    fn merge_llm(self, base: LlmSettings) -> LlmSettings {
        let l = self.llm;
        LlmSettings {
            provider: l
                .provider
                .filter(|s| matches!(s.trim(), "openai-compat" | "opencode"))
                .map(|s| s.trim().to_owned())
                .unwrap_or(base.provider),
            base_url: l
                .base_url
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(base.base_url),
            model: l
                .model
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(base.model),
            api_key_env: l
                .api_key_env
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(base.api_key_env),
            password_env: l
                .password_env
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(base.password_env),
            verify_command: l
                .verify_command
                .filter(|v| !v.is_empty())
                .or(base.verify_command),
            max_total_tokens: l
                .max_total_tokens
                .filter(|n| *n > 0)
                .unwrap_or(base.max_total_tokens),
        }
    }

    fn merge_skills(self, base: SkillsSettings) -> SkillsSettings {
        let raw = self.skills;
        SkillsSettings {
            paths: raw
                .paths
                .filter(|v| !v.is_empty())
                .map(|p| p.into_iter().map(PathBuf::from).collect())
                .unwrap_or(base.paths),
            per_skill_max_tokens: raw
                .per_skill_max_tokens
                .filter(|n| *n > 0)
                .unwrap_or(base.per_skill_max_tokens),
            total_max_tokens: raw
                .total_max_tokens
                .filter(|n| *n > 0)
                .unwrap_or(base.total_max_tokens),
        }
    }

    fn merge_telegram(self, base: TelegramSettings) -> TelegramSettings {
        let t = self.telegram;
        TelegramSettings {
            token_env: t
                .token_env
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(base.token_env),
            chat_ids: t
                .chat_ids
                .filter(|v| !v.is_empty())
                .unwrap_or(base.chat_ids),
            api_base: t
                .api_base
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(base.api_base),
        }
    }
}

/// Discover config files: project `.openchat/` over user `~/.openchat/`.
#[must_use]
pub fn discover(start_dir: &Path) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    let project = start_dir.join(".openchat").join("config.toml");
    if project.is_file() {
        found.insert(project);
    }
    if let Some(home) = dirs::home_dir() {
        let user = home.join(".openchat").join("config.toml");
        if user.is_file() {
            found.insert(user);
        }
    }
    found.into_iter().collect()
}

/// Load the effective configs, later files overriding earlier ones.
pub fn load(
    start_dir: &Path,
    explicit: Option<&Path>,
) -> Result<(
    GovernorConfig,
    LlmSettings,
    SkillsSettings,
    TelegramSettings,
)> {
    let files = match explicit {
        Some(path) => vec![path.to_path_buf()],
        None => discover(start_dir),
    };
    let mut config = GovernorConfig::default();
    let mut llm = LlmSettings::default();
    let mut skills = SkillsSettings::default();
    let mut telegram = TelegramSettings::default();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: RawConfig =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        config = raw.clone().merge_resources(config)?;
        llm = raw.clone().merge_llm(llm);
        skills = raw.clone().merge_skills(skills);
        telegram = raw.merge_telegram(telegram);
        tracing::info!(path = %path.display(), "config layer applied");
    }
    Ok((config, llm, skills, telegram))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_readme_example_verbatim() {
        let raw: RawConfig = toml::from_str(
            r#"
[resources]
max_cpu_cores = 4
max_concurrent_builds = 1
max_worker_memory = "4GB"
low_process_priority = true
"#,
        )
        .unwrap();
        let cfg = raw.merge_resources(GovernorConfig::default()).unwrap();
        assert_eq!(cfg.max_cpu_cores, CpuCores::Count(4));
        assert_eq!(cfg.max_concurrent_builds, 1);
        assert_eq!(cfg.max_worker_memory, 4 * 1024 * 1024 * 1024);
        assert!(cfg.low_process_priority);
        assert!(
            (cfg.memory_low_water - 0.15).abs() < f64::EPSILON,
            "default kept"
        );
    }

    #[test]
    fn percent_core_spec_supported() {
        let raw: RawConfig = toml::from_str("[resources]\nmax_cpu_cores = \"50%\"\n").unwrap();
        let cfg = raw.merge_resources(GovernorConfig::default()).unwrap();
        assert_eq!(cfg.max_cpu_cores, CpuCores::Percent(50));
    }

    #[test]
    fn later_layer_overrides_earlier() {
        let base: RawConfig =
            toml::from_str("[resources]\nmax_cpu_cores = 8\nlow_process_priority = false\n")
                .unwrap();
        let top: RawConfig = toml::from_str("[resources]\nmax_cpu_cores = 2\n").unwrap();
        let cfg = top
            .merge_resources(base.merge_resources(GovernorConfig::default()).unwrap())
            .unwrap();
        assert_eq!(cfg.max_cpu_cores, CpuCores::Count(2));
        assert!(!cfg.low_process_priority, "unset keys keep the lower layer");
    }

    #[test]
    fn memory_sizes_parse() {
        assert_eq!(parse_memory("4GB").unwrap(), 4 << 30);
        assert_eq!(parse_memory("512 MB").unwrap(), 512 << 20);
        assert_eq!(parse_memory("8192").unwrap(), 8192);
        assert!(parse_memory("4 pizzas").is_err());
    }

    #[test]
    fn unknown_keys_rejected() {
        let err = toml::from_str::<RawConfig>("[resources]\nmax_cats = 9\n").unwrap_err();
        assert!(err.to_string().contains("max_cats"), "{err}");
    }

    #[test]
    fn bad_low_water_rejected() {
        let raw: RawConfig = toml::from_str("[resources]\nmemory_low_water = 1.5\n").unwrap();
        assert!(raw.merge_resources(GovernorConfig::default()).is_err());
    }

    #[test]
    fn llm_section_parses_with_defaults_kept() {
        let raw: RawConfig = toml::from_str(
            r#"
[llm]
model = "qwen2.5-coder:32b"
base_url = "http://127.0.0.1:11434/v1"
verify_command = ["cargo", "check", "--workspace"]
"#,
        )
        .unwrap();
        let llm = raw.merge_llm(LlmSettings::default());
        assert_eq!(llm.model, "qwen2.5-coder:32b");
        assert_eq!(llm.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(
            llm.verify_command.as_deref().unwrap(),
            ["cargo", "check", "--workspace"]
        );
        assert_eq!(llm.api_key_env, "OPENAI_API_KEY", "untouched default");
        assert_eq!(llm.max_total_tokens, 24_000);
    }

    #[test]
    fn llm_blank_overrides_are_ignored() {
        let raw: RawConfig =
            toml::from_str("[llm]\nmodel = \"  \"\nverify_command = []\n").unwrap();
        let llm = raw.merge_llm(LlmSettings::default());
        assert_eq!(llm.model, "gpt-4o-mini");
        assert!(llm.verify_command.is_none());
    }

    #[test]
    fn unknown_llm_key_rejected() {
        let err = toml::from_str::<RawConfig>("[llm]\ntemperature_9000 = 1\n").unwrap_err();
        assert!(err.to_string().contains("temperature_9000"), "{err}");
    }

    #[test]
    fn llm_provider_selects_opencode_gateway() {
        let raw: RawConfig = toml::from_str(
            "[llm]\nprovider = \"opencode\"\nbase_url = \"http://127.0.0.1:42415\"\nmodel = \"opencode-go/qwen4-max\"\n",
        )
        .unwrap();
        let llm = raw.merge_llm(LlmSettings::default());
        assert_eq!(llm.provider, "opencode");
        assert_eq!(llm.base_url, "http://127.0.0.1:42415");
        assert_eq!(llm.password_env, "OPENCODE_PASSWORD");
        // unknown provider strings fall back to the default, never load
        let raw: RawConfig = toml::from_str("[llm]\nprovider = \"sketchy\"\n").unwrap();
        assert_eq!(
            raw.merge_llm(LlmSettings::default()).provider,
            "openai-compat"
        );
    }

    #[test]
    fn telegram_section_layers() {
        let raw: RawConfig = toml::from_str(
            "[telegram]\nchat_ids = [12345, -678]\napi_base = \"http://127.0.0.1:8081\"\n",
        )
        .unwrap();
        let t = raw.merge_telegram(TelegramSettings::default());
        assert_eq!(
            t.chat_ids,
            vec![12345, -678],
            "negative group ids must survive"
        );
        assert_eq!(t.api_base, "http://127.0.0.1:8081");
        assert_eq!(t.token_env, "TELEGRAM_BOT_TOKEN");
    }
}
