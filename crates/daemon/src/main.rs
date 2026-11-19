//! `openchatd`: headless core daemon (Roadmap Phase 1 scaffolding).
//!
//! Starts the resource governor, logs the resolved host budget, and idles
//! until SIGINT/SIGTERM. LLM/session plumbing arrives on top of this shell
//! in later phases.

mod config;

use anyhow::{Context, Result};
use openchat_governor::Governor;
use openchat_memory::MemoryStore;
use openchat_watcher::{WatcherConfig, WorkspaceWatcher};
use tokio::signal::unix::{SignalKind, signal};

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

    let mut args = std::env::args().skip(1);
    let mut config_path: Option<std::path::PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                let path = args.next().context("--config requires a path")?;
                config_path = Some(path.into());
            }
            "--version" => {
                println!("openchatd {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other:?} (expected --config or --version)"),
        }
    }

    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let governor_config =
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
}
