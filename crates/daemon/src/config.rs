//! `config.toml` loading: `.openchat/config.toml` (project) merged over
//! `~/.openchat/config.toml` (user), matching the README's `[resources]`
//! contract.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use openchat_governor::{CpuCores, GovernorConfig};

/// On-disk shape; every key optional so partial files inherit defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    resources: RawResources,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResources {
    max_cpu_cores: Option<CoreSpec>,
    max_concurrent_builds: Option<usize>,
    max_worker_memory: Option<String>,
    low_process_priority: Option<bool>,
    memory_low_water: Option<f64>,
}

/// `max_cpu_cores = 4` or `max_cpu_cores = "50%"`.
#[derive(Debug, Deserialize)]
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
    fn merge(self, base: GovernorConfig) -> Result<GovernorConfig> {
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

/// Load the effective governor config, later files overriding earlier ones.
pub fn load(start_dir: &Path, explicit: Option<&Path>) -> Result<GovernorConfig> {
    let files = match explicit {
        Some(path) => vec![path.to_path_buf()],
        None => discover(start_dir),
    };
    let mut config = GovernorConfig::default();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: RawConfig =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        config = raw.merge(config)?;
        tracing::info!(path = %path.display(), "config layer applied");
    }
    Ok(config)
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
        let cfg = raw.merge(GovernorConfig::default()).unwrap();
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
        let cfg = raw.merge(GovernorConfig::default()).unwrap();
        assert_eq!(cfg.max_cpu_cores, CpuCores::Percent(50));
    }

    #[test]
    fn later_layer_overrides_earlier() {
        let base: RawConfig =
            toml::from_str("[resources]\nmax_cpu_cores = 8\nlow_process_priority = false\n")
                .unwrap();
        let top: RawConfig = toml::from_str("[resources]\nmax_cpu_cores = 2\n").unwrap();
        let cfg = top
            .merge(base.merge(GovernorConfig::default()).unwrap())
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
        assert!(raw.merge(GovernorConfig::default()).is_err());
    }
}
