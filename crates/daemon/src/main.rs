//! `openchatd`: headless core daemon.
//!
//! Modes:
//! - (default) daemon: governor + memory + watcher live until SIGINT/SIGTERM
//! - `exec [--light] -- cmd`: one governed command through the arbiter
//! - `chat "<prompt>"`: one agent turn, printing the live event stream
//! - `skills [--json]`: fingerprint, bind, gap report, stack memory sync
//! - `serve [--bind ADDR]`: HTTP/SSE front (GET /healthz /status, POST /chat)
//! - `telegram`: Telegram bot gateway (allowlisted chats, /status, /cores)
//! - `--version`

mod config;
mod engine;
mod serve;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use openchat_governor::Governor;
use openchat_session::events::TurnEvent;
use openchat_tools::compact::ToolRecord;
use openchat_tools::exec::{ExecRequest, TaskKind, ToolRunner};
use openchat_watcher::{WatcherConfig, WorkspaceWatcher};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast;

use crate::config::load;
use crate::engine::{Engine, VECTOR_DIMS, skill_roots};

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
        Some("skills") => skills_main(&argv[1..]).await,
        Some("serve") => serve_main(&argv[1..]).await,
        Some("telegram") => telegram_main(&argv[1..]).await,
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
    let (governor_config, _llm, _skills, _telegram) = load(&cwd, config_path.as_deref())?;
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
    let record = ToolRecord::from_report(label, &report);
    eprintln!("{}", record.digest());
    // Diagnostics trap also applies to manual exec runs.
    if let Some(d) = openchat_tools::diagnose::diagnose(
        &record.stdout,
        &record.stderr,
        record.exit_code,
        record.signal,
        record.timed_out,
    ) {
        eprintln!("{}", d.render());
    }
    // Pass the child's outcome through so CI wrappers see the real code:
    // 124 for our timeout kill (coreutils convention), 128+signal for
    // signal deaths, otherwise the child's exit code.
    let code = match (report.exit_code, report.signal, report.timed_out) {
        (Some(c), _, _) => c,
        (None, Some(sig), _) => 128 + sig,
        (None, None, true) => 124,
        (None, None, false) => 1,
    };
    std::process::exit(code);
}

/// Human-readable line for one wire event (the CLI is just another client
/// of the same stream the web UI and bots consume).
fn print_turn_event(ev: &TurnEvent) -> bool {
    match ev {
        TurnEvent::TurnStarted { .. } => false,
        TurnEvent::Iteration {
            index,
            total_tokens,
            ..
        } => {
            eprintln!("· iteration {index} ({total_tokens} prompt tokens est.)");
            false
        }
        TurnEvent::AssistantText { .. } => false, // final text printed on close
        TurnEvent::ToolCalled { name, ok, digest } => {
            eprintln!("· {} {name}: {digest}", if *ok { "✓" } else { "✗" });
            false
        }
        TurnEvent::FinalVerification { ok } => {
            eprintln!(
                "· verification {}",
                if *ok {
                    "green, closing turn"
                } else {
                    "red, turn stays open"
                }
            );
            false
        }
        TurnEvent::SlopStripped { bytes } => {
            eprintln!("· slop interceptor stripped {bytes} bytes of filler");
            false
        }
        TurnEvent::MemoryFault {
            fault, headline, ..
        } => {
            eprintln!("· ⚠ memory diagnostic [{fault}]: {headline}");
            false
        }
        TurnEvent::TurnClosed {
            text,
            iterations,
            badges,
        } => {
            println!("{text}");
            let skills = if badges.skills.is_empty() {
                "none".to_owned()
            } else {
                badges
                    .skills
                    .iter()
                    .map(|s| format!("{}({}t{})", s.id, s.tokens, if s.capped { "*" } else { "" }))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            eprintln!(
                "· closed in {iterations} iterations · stack=[{}] skills=[{skills}] \
                 prompt={}t governor={}/{} cores {} slot(s) memory={} dedup={} slop={}B",
                if badges.stack.is_empty() {
                    "none"
                } else {
                    &badges.stack
                },
                badges.prompt_tokens,
                badges.cores,
                badges.usable_cores,
                badges.free_build_slots,
                badges.memory_entries,
                badges.deduped_rule_lines,
                badges.slop_stripped_bytes,
            );
            true
        }
        TurnEvent::TurnFailed { error } => {
            eprintln!("· turn failed: {error}");
            true
        }
    }
}

/// Handle daemon-local slash commands (no model round-trip needed).
/// Returns Some(()) when the prompt was a command and has been answered.
fn local_command(engine: &Engine, prompt: &str) -> Option<bool> {
    let mut words = prompt.split_whitespace();
    match words.next() {
        Some("/cores") => {
            let spec = words.next().unwrap_or_default();
            match engine.set_cores(spec) {
                Ok(msg) => eprintln!("· {msg}"),
                Err(e) => eprintln!("· cores error: {e}"),
            }
            Some(true)
        }
        Some("/status") => {
            // Status is async-only; handled by the caller's await point.
            Some(false)
        }
        _ => None,
    }
}

async fn chat_main(argv: &[String]) -> Result<()> {
    let mut argv = argv.to_vec();
    let (config_path, _light) = extract_flags(&mut argv);
    let prompt = argv.join(" ");
    if prompt.trim().is_empty() {
        anyhow::bail!("usage: openchatd chat [--config PATH] \"<request>|/cores N|/status\"");
    }
    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let engine = Arc::new(Engine::build(&cwd, config_path.as_deref())?);

    if let Some(handled) = local_command(&engine, &prompt) {
        if handled {
            return Ok(());
        }
        let status = engine.status_json().await;
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    let (tx, mut rx) = broadcast::channel::<TurnEvent>(512);
    let printer = tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if print_turn_event(&ev) {
                break;
            }
        }
    });
    // The agent's published stream is the single source of truth for what
    // the user sees; the returned outcome is only a failure carrier.
    let outcome = engine.run_turn(&prompt, Some(tx), &mut |_| {}).await;
    let _ = printer.await;
    let outcome = outcome.context("agent turn failed")?;
    drop(outcome); // badges already printed from the event stream
    Ok(())
}

/// Fingerprint the workspace, bind the skill catalog, report gaps, and
/// persist the `stack` fact into project memory (Phase 3 control surface).
async fn skills_main(argv: &[String]) -> Result<()> {
    let mut argv = argv.to_vec();
    let (config_path, _light) = extract_flags(&mut argv);
    let json = argv.iter().any(|a| a == "--json");

    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let engine = Engine::build(&cwd, config_path.as_deref())?;
    let roots = skill_roots(&cwd, &engine.skills);
    let fp = openchat_skills::fingerprint(&cwd);
    let registry = openchat_skills::load_roots(&roots);
    let (bindings, gaps) = openchat_skills::bind(&fp, &registry);

    if json {
        let out = serde_json::json!({
            "stack": fp.languages.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
            "files_scanned": fp.files_scanned,
            "registry_size": registry.len(),
            "roots": roots,
            "active_skills": bindings.iter().map(|b| serde_json::json!({
                "id": b.skill.id, "reason": b.reason, "tokens": b.skill.tokens,
                "explicit_triggers": b.skill.explicit_triggers, "strength": b.skill.strength,
            })).collect::<Vec<_>>(),
            "gaps": gaps.iter().map(|g| g.language.as_str()).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).context("serialize")?
        );
    } else {
        let tag = fp.stack_tag();
        println!(
            "stack: {} ({} files scanned, registry {} skills from {} roots)",
            if tag.is_empty() { "none" } else { &tag },
            fp.files_scanned,
            registry.len(),
            roots.len(),
        );
        for b in &bindings {
            println!(
                "  {:<24} {:>5} tok  {}",
                b.skill.id, b.skill.tokens, b.reason
            );
        }
        if gaps.is_empty() {
            println!("gaps: none");
        } else {
            for g in &gaps {
                println!(
                    "gap: no skill for {} -> author .openchat/skills/{}/SKILL.md",
                    g.language.as_str(),
                    g.language.as_str()
                );
            }
        }
    }

    if !fp.languages.is_empty() {
        let store = engine.store.lock().await;
        store
            .store(&openchat_memory::MemoryEntry {
                scope: openchat_memory::MemoryScope::Project,
                category: "stack".into(),
                key: "stack".into(),
                value: fp.stack_tag(),
                embedding: None,
            })
            .context("failed to record stack in memory")?;
        if !json {
            eprintln!("· stack recorded to project memory");
        }
    }
    Ok(())
}

async fn serve_main(argv: &[String]) -> Result<()> {
    let mut rest = argv.to_vec();
    let (config_path, _light) = extract_flags(&mut rest);
    let mut bind = "127.0.0.1:8420".to_owned();
    if let Some(i) = rest.iter().position(|a| a == "--bind")
        && i + 1 < rest.len()
    {
        bind = rest.remove(i + 1);
        rest.remove(i);
    }
    if !rest.is_empty() {
        anyhow::bail!("usage: openchatd serve [--config PATH] [--bind HOST:PORT]");
    }
    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let engine = Arc::new(Engine::build(&cwd, config_path.as_deref())?);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;
    serve::serve(engine, Arc::new(serve::Hub::new()), listener).await
}

/// Mobile control plane: long-poll Telegram, run allowed chats' text as
/// agent turns, expose /status and /cores for live governor control.
async fn telegram_main(argv: &[String]) -> Result<()> {
    let mut argv = argv.to_vec();
    let (config_path, _light) = extract_flags(&mut argv);
    if !argv.is_empty() {
        anyhow::bail!("usage: openchatd telegram [--config PATH]");
    }
    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let engine = Arc::new(Engine::build(&cwd, config_path.as_deref())?);
    let token = std::env::var(&engine.telegram.token_env).with_context(|| {
        format!(
            "{} is not set; create a bot with @BotFather and export its token",
            engine.telegram.token_env
        )
    })?;
    anyhow::ensure!(
        !engine.telegram.chat_ids.is_empty(),
        "[telegram] chat_ids is empty: refusing to expose an agent to any chat; \
         find your id via @userinfobot and list it in .openchat/config.toml"
    );
    let cfg = openchat_telegram::TelegramConfig {
        api_base: engine.telegram.api_base.clone(),
        long_poll_seconds: 25,
        ..openchat_telegram::TelegramConfig::new(token, engine.telegram.chat_ids.clone())
    };
    let bridge = openchat_telegram::Bridge::new(
        cfg,
        Arc::clone(&engine) as Arc<dyn openchat_telegram::EnginePort>,
        Arc::new(engine::MemoryOffsetStore::new(Arc::clone(&engine.store))),
    )
    .context("telegram bridge refused to start")?;

    let mut sigterm = signal(SignalKind::terminate()).context("failed to bind SIGTERM")?;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    tracing::info!(
        chats = ?engine.telegram.chat_ids,
        base = %engine.telegram.api_base,
        "telegram bridge online"
    );
    let shutdown = async {
        tokio::select! {
            _ = sigterm.recv() => tracing::info!("SIGTERM received"),
            res = &mut ctrl_c => {
                res.ok();
                tracing::info!("SIGINT received");
            }
        }
    };
    bridge.run(shutdown).await;
    Ok(())
}

async fn daemon_main(argv: &[String]) -> Result<()> {
    let mut argv = argv.to_vec();
    let (config_path, _light) = extract_flags(&mut argv);
    if let Some(unknown) = argv.first() {
        anyhow::bail!("unknown argument {unknown:?} (expected --config, --version, `exec`)");
    }

    let cwd = std::env::current_dir().context("failed to resolve cwd")?;
    let engine = Engine::build(&cwd, config_path.as_deref())?;
    let governor = &engine.governor;

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

    let memory = engine.store.lock().await;
    tracing::info!(entries = memory.count()?, path = %cwd.join(".openchat/memory.sqlite").display(), "memory online");
    drop(memory);

    // One shared, debounced watcher for the whole workspace.
    let watcher = WorkspaceWatcher::start(WatcherConfig::new(vec![cwd.clone()]))
        .context("failed to start workspace watcher")?;
    tracing::info!("workspace watcher online");

    // Watcher subscribers: debug-log every change, and re-bind skills when
    // a manifest-level change moves the stack (first consumer of the
    // shared bus).
    let watch_root = cwd.clone();
    let db_path = watch_root.join(".openchat").join("memory.sqlite");
    let skill_cfg = engine.skills.clone();
    let watch_pump = tokio::spawn(async move {
        let mut last_tag = openchat_skills::fingerprint(&watch_root).stack_tag();
        while let Some(event) = watcher.next().await {
            tracing::debug!(path = %event.path.display(), kind = ?event.kind, "workspace change");
            if !openchat_skills::bind::affects_fingerprint(&event.path) {
                continue;
            }
            let fp = openchat_skills::fingerprint(&watch_root);
            let tag = fp.stack_tag();
            if tag == last_tag {
                continue;
            }
            tracing::info!(old = %last_tag, new = %tag, "workspace:stack_expanded");
            let catalog = openchat_skills::load_roots(&skill_roots(&watch_root, &skill_cfg));
            let (bindings, gaps) = openchat_skills::bind(&fp, &catalog);
            tracing::info!(
                skills = ?bindings.iter().map(|b| b.skill.id.as_str()).collect::<Vec<_>>(),
                gaps = gaps.len(),
                "skills rebound"
            );
            if let Ok(store) = openchat_memory::MemoryStore::open(&db_path, VECTOR_DIMS)
                && !tag.is_empty()
            {
                let entry = openchat_memory::MemoryEntry {
                    scope: openchat_memory::MemoryScope::Project,
                    category: "stack".into(),
                    key: "stack".into(),
                    value: tag.clone(),
                    embedding: None,
                };
                if let Err(e) = store.store(&entry) {
                    tracing::warn!(error = %e, "stack memory update failed");
                }
            }
            last_tag = tag;
        }
    });

    tokio::select! {
        _ = sigterm.recv() => tracing::info!("SIGTERM received"),
        res = &mut ctrl_c => res.context("failed to bind SIGINT")?,
    }
    tracing::info!("openchatd shutting down");
    watch_pump.abort();
    // WAL hygiene on the way out so a crash never leaves a fat log.
    engine
        .store
        .lock()
        .await
        .compact()
        .context("final memory compaction failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
