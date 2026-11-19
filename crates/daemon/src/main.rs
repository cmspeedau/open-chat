//! `openchatd`: headless core daemon (Roadmap Phase 1 scaffolding).
//!
//! Starts the resource governor, logs the resolved host budget, and idles
//! until SIGINT/SIGTERM. LLM/session plumbing arrives on top of this shell
//! in later phases.
//!
//! Subcommand `exec -- <cmd...>` runs a single command through the same
//! arbiter the agent loop will use: build slot (or `--light` to skip it),
//! affinity pin, env thread caps, bounded output, one-line digest.

mod config;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use openchat_governor::Governor;
use openchat_memory::MemoryStore;
use openchat_session::agent::{Agent, AgentEvent};
use openchat_session::provider::{OpenAi, OpenAiConfig};
use openchat_session::tools::standard_registry;
use openchat_tools::compact::ToolRecord;
use openchat_tools::exec::{ExecRequest, TaskKind, ToolRunner};
use openchat_watcher::{WatcherConfig, WorkspaceWatcher};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Mutex;

use crate::config::load;

/// Dense tier dimensions. v1 pins one embedding model's width for the whole
/// store; per-model migration lands with the LLM layer (Phase 2+).
const VECTOR_DIMS: usize = 768;

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,openchat_governor=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("--version") => {
            println!("openchatd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("exec") => exec_main(&argv[1..]).await,
        Some("chat") => chat_main(&argv[1..]).await,
        _ => daemon_main(&argv).await,
    }
}

/// Extract `--config <path>` (returns the path) and `--light` (returns
/// whether it was present) from a mutable arg list.
fn extract_flags(argv: &mut Vec<String>) -> (Option<PathBuf>, bool) {
    let mut config = None;
    if let Some(i) = argv.iter().position(|a| a == "--config")
        && i + 1 < argv.len()
    {
        // Remove the value first so no index shifts before this line.
        let value = argv.remove(i + 1);
        argv.remove(i);
        config = Some(PathBuf::from(value));
    }
    let light = argv.iter().position(|a| a == "--light").is_some_and(|i| {
        argv.remove(i);
        true
    });
    (config, light)
}

async fn exec_main(argv: &[String]) -> Result<()> {
    let mut argv = argv.to_vec();
    let (config_path, light) = extract_flags(&mut argv);
    let sep = argv
        .iter()
        .position(|a| a == "--")
        .context("missing `--` before command")?;
    if sep + 1 >= argv.len() {
        anyhow::bail!("usage: openchatd exec [--config PATH] [--light] -- <program> [args...]");
    }
    let program = argv[sep + 1].clone();
    let args: Vec<String> = argv[sep + 2..].to_vec();

    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let (governor_config, _llm) = load(&cwd, config_path.as_deref())?;
    let governor = Governor::new(governor_config).context("failed to start resource governor")?;
    let kind = if light {
        TaskKind::Light
    } else {
        TaskKind::Build
    };
    let req = ExecRequest {
        program,
        args,
        cwd: Some(cwd),
        kind,
        ..ExecRequest::new("", kind)
    };
    let runner = ToolRunner::new(&governor);
    let report = runner.run(&req).await.context("exec failed")?;
    let label = format!("{} {}", req.program, req.args.join(" "));
    eprintln!("{}", ToolRecord::from_report(label, &report).digest());
    // Pass the child's outcome through so CI wrappers see the real code
    // (124 mirrors coreutils timeout convention).
    std::process::exit(
        report
            .exit_code
            .unwrap_or(if report.timed_out { 124 } else { 1 }),
    );
}

/// One-shot agent turn from the command line: composes context, talks to
/// the configured OpenAI-compatible provider, dispatches tools under the
/// governor, and refuses to close until verification is green.
async fn chat_main(argv: &[String]) -> Result<()> {
    let mut argv = argv.to_vec();
    let (config_path, _light) = extract_flags(&mut argv);
    let prompt = argv.join(" ");
    if prompt.trim().is_empty() {
        anyhow::bail!("usage: openchatd chat [--config PATH] \"<request>\"");
    }

    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let (governor_config, llm) =
        load(&cwd, config_path.as_deref()).context("failed to load open-chat config")?;
    let api_key = std::env::var(&llm.api_key_env).unwrap_or_default();
    let provider = OpenAi::new(OpenAiConfig {
        base_url: llm.base_url.clone(),
        api_key,
        model: llm.model.clone(),
    })
    .context("invalid [llm] provider config")?;

    let governor =
        Arc::new(Governor::new(governor_config).context("failed to start resource governor")?);
    let store = Arc::new(Mutex::new(
        MemoryStore::open(&cwd.join(".openchat").join("memory.sqlite"), VECTOR_DIMS)
            .context("failed to open workspace memory")?,
    ));
    // README: verification is cargo check / tsc / pytest per stack. v1
    // auto-detects the Rust case; others come from explicit config.
    let verifier = llm.verify_command.clone().or_else(|| {
        cwd.join("Cargo.toml")
            .is_file()
            .then(|| vec!["cargo".into(), "check".into(), "--workspace".into()])
    });
    let registry = standard_registry(Arc::clone(&governor), &cwd, Arc::clone(&store), verifier);

    let agent = Agent::new(&provider, &registry, Some(store)).with_budget(llm.max_total_tokens, 8);
    let outcome = agent
        .run(&prompt, &mut |event| match event {
            AgentEvent::Iteration { index, composed } => {
                eprintln!(
                    "· iteration {index} ({} prompt tokens est.)",
                    composed.total_tokens
                );
            }
            AgentEvent::ToolCalled { name, ok, digest } => {
                eprintln!("· {} {name}: {digest}", if *ok { "✓" } else { "✗" });
            }
            AgentEvent::FinalVerification { ok } => {
                eprintln!(
                    "· verification {}",
                    if *ok {
                        "green, closing turn"
                    } else {
                        "red, turn stays open"
                    }
                );
            }
            AgentEvent::AssistantText(_) => {} // final text printed once below
        })
        .await
        .context("agent turn failed")?;

    println!("{}", outcome.text);
    let sections = outcome
        .composed
        .report
        .iter()
        .map(|s| format!("{}={}", s.name, s.tokens))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "· turn closed in {} iterations [{sections}]",
        outcome.iterations
    );
    Ok(())
}

async fn daemon_main(argv: &[String]) -> Result<()> {
    let mut argv = argv.to_vec();
    let (config_path, _light) = extract_flags(&mut argv);
    if let Some(unknown) = argv.first() {
        anyhow::bail!(
            "unknown argument {unknown:?} (expected --config, --version, `exec`, or `chat`)"
        );
    }
    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let (governor_config, _llm) =
        load(&cwd, config_path.as_deref()).context("failed to load open-chat config")?;
    let governor = Governor::new(governor_config).context("failed to start resource governor")?;

    let snap = governor.snapshot();
    tracing::info!(
        cores = snap.cores,
        usable = snap.usable_total,
        build_slots = snap.free_build_slots,
        nice = ?snap.nice_applied,
        worker_memory = governor.max_worker_memory(),
        "governor ready"
    );

    let mut sigterm = signal(SignalKind::terminate()).context("failed to bind SIGTERM")?;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    // Embedded dual-tier memory lives in the project `.openchat/` dir.
    let memory = MemoryStore::open(&cwd.join(".openchat").join("memory.sqlite"), VECTOR_DIMS)
        .context("failed to open workspace memory")?;
    tracing::info!(entries = memory.count()?, path = %cwd.join(".openchat/memory.sqlite").display(), "memory online");

    // One shared, debounced watcher for the whole workspace.
    let watcher = WorkspaceWatcher::start(WatcherConfig::new(vec![cwd.clone()]))
        .context("failed to start workspace watcher")?;
    tracing::info!("workspace watcher online");

    // Watcher feed: today we only log stack-relevant files; the skill
    // binder (Phase 3) subscribes to the same bus later.
    let watch_pump = tokio::spawn(async move {
        while let Some(event) = watcher.next().await {
            tracing::debug!(path = %event.path.display(), kind = ?event.kind, "workspace change");
        }
    });

    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM received"),
        res = &mut ctrl_c => res.context("failed to bind SIGINT")?,
    }
    tracing::info!("openchatd shutting down");
    watch_pump.abort();
    // WAL hygiene on the way out so a crash never leaves a fat log.
    memory.compact().context("final memory compaction failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn daemon_boots_with_default_config() {
        // The full main() needs signals/argv; the boot path under test is
        // config-defaults -> governor construction, which must work on any
        // machine the tests run on.
        let cfg = openchat_governor::GovernorConfig::default();
        let governor = Governor::new(cfg).expect("default governor boots");
        assert!(governor.snapshot().cores >= 1);
        assert!(governor.snapshot().free_build_slots >= 1);
    }

    #[test]
    fn flag_extraction_leaves_positionals_in_order() {
        let mut argv = vec![
            "--config".to_string(),
            "/tmp/c.toml".into(),
            "--light".into(),
            "cargo".into(),
            "check".into(),
        ];
        let (cfg, light) = extract_flags(&mut argv);
        assert_eq!(cfg.unwrap().to_str().unwrap(), "/tmp/c.toml");
        assert!(light);
        assert_eq!(argv, ["cargo", "check"]);
    }
}
