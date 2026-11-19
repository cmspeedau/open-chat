//! One debounced workspace file watcher for all agents (Roadmap Phase 1).
//!
//! OpenChamber spawns a watcher per agent session; `open-chat` runs exactly
//! one [`WorkspaceWatcher`] in the daemon and fans coalesced events out over
//! a channel. Inotify watches are per-process kernel objects, so sharing is
//! what stops the "3x file watchers" contention line in the README.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;
use tokio::sync::mpsc;

/// What changed, coarsely. Non-file semantics map to `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Modified,
    Removed,
    Other,
}

/// A coalesced change to one path after the quiet period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEvent {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("failed to create file watcher: {0}")]
    Init(#[from] notify::Error),
    #[error("failed to watch {path}: {source}")]
    Watch {
        path: PathBuf,
        #[source]
        source: notify::Error,
    },
}

/// Paths never worth waking agents for, matched as whole name components.
pub const DEFAULT_IGNORES: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    ".venv",
    "dist",
    "build",
];

/// Tunables for the shared watcher bus.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Root directories to watch recursively.
    pub paths: Vec<PathBuf>,
    /// Quiet period after the last raw event before an entry is emitted.
    /// 300ms absorbs editor save bursts (tmp-file rename, double-write).
    pub debounce: Duration,
    /// Name components to skip anywhere under a root.
    pub ignores: Arc<Vec<String>>,
}

impl WatcherConfig {
    #[must_use]
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            debounce: Duration::from_millis(300),
            ignores: Arc::new(DEFAULT_IGNORES.iter().map(|s| (*s).to_owned()).collect()),
        }
    }
}

/// Handle to the single shared watcher. Dropping the notify watcher ends
/// the raw stream, which stops the debounce pump.
#[derive(Debug)]
pub struct WorkspaceWatcher {
    // Kept alive so the inotify fds outlive `start`; Drop stops the pump.
    _watcher: RecommendedWatcher,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<WorkspaceEvent>>>,
}

impl WorkspaceWatcher {
    /// Start watching; debounced events arrive via [`next`](Self::next).
    ///
    /// # Errors
    ///
    /// [`WatcherError`] if the OS watcher or a root registration fails.
    pub fn start(config: WatcherConfig) -> Result<Self, WatcherError> {
        let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            // Receiver gone => pump finished => nobody cares.
            let _ = raw_tx.send(res);
        })
        .map_err(WatcherError::Init)?;
        for path in &config.paths {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .map_err(|source| WatcherError::Watch {
                    path: path.clone(),
                    source,
                })?;
        }

        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let WatcherConfig {
            debounce, ignores, ..
        } = config;
        tokio::spawn(async move {
            let mut pending: BTreeMap<PathBuf, (ChangeKind, tokio::time::Instant)> =
                BTreeMap::new();
            loop {
                // Tick at a fixed debounce cadence while anything is
                // pending, so a deadline pushed later by a fresh event is
                // never flushed early; block purely on raw events when idle.
                let tick = if pending.is_empty() {
                    tokio::time::sleep(Duration::from_secs(3600))
                } else {
                    tokio::time::sleep(debounce)
                };
                tokio::select! {
                    biased;
                    maybe = raw_rx.recv() => match maybe {
                        Some(Ok(event)) => {
                            let deadline = tokio::time::Instant::now() + debounce;
                            for path in event.paths {
                                if is_ignored(&path, &ignores) {
                                    continue;
                                }
                                let kind = classify(&event.kind);
                                pending
                                    .entry(path)
                                    .and_modify(|(k, at)| {
                                        *k = merge_kind(*k, kind);
                                        *at = deadline;
                                    })
                                    .or_insert((kind, deadline));
                            }
                        }
                        Some(Err(e)) => tracing::warn!(error = %e, "watcher error event"),
                        // All sender clones gone: the watcher handle dropped.
                        None => break,
                    },
                    () = tick => {}
                }
                let now = tokio::time::Instant::now();
                let ready: Vec<PathBuf> = pending
                    .iter()
                    .filter(|(_, (_, at))| *at <= now)
                    .map(|(path, _)| path.clone())
                    .collect();
                for path in ready {
                    if let Some((kind, _)) = pending.remove(&path) {
                        let _ = ev_tx.send(WorkspaceEvent { path, kind });
                    }
                }
            }
            // Watcher died with entries still pending: flush what we saw.
            for (path, (kind, _)) in pending {
                let _ = ev_tx.send(WorkspaceEvent { path, kind });
            }
        });
        Ok(Self {
            _watcher: watcher,
            rx: Arc::new(tokio::sync::Mutex::new(ev_rx)),
        })
    }

    /// Next debounced event, or `None` once the watcher is dropped.
    pub async fn next(&self) -> Option<WorkspaceEvent> {
        self.rx.lock().await.recv().await
    }
}

fn classify(kind: &EventKind) -> ChangeKind {
    match kind {
        EventKind::Create(_) => ChangeKind::Created,
        EventKind::Remove(_) => ChangeKind::Removed,
        EventKind::Modify(ModifyKind::Metadata(_) | ModifyKind::Any) => ChangeKind::Modified,
        EventKind::Modify(_) => ChangeKind::Modified,
        _ => ChangeKind::Other,
    }
}

/// Severity merge for coalescing a burst on one path: last-writer-wins
/// except that removal dominates (the file is gone) and `Other` only fills
/// a gap.
fn merge_kind(old: ChangeKind, new: ChangeKind) -> ChangeKind {
    use ChangeKind as C;
    match (old, new) {
        (_, C::Other) => old,
        (C::Other, n) => n,
        (C::Removed, _) | (_, C::Removed) => C::Removed,
        (C::Created, C::Modified) => C::Created,
        _ => C::Modified,
    }
}

fn is_ignored(path: &Path, ignores: &[String]) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| ignores.iter().any(|ig| ig == s))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_match_path_components() {
        let ig: Vec<String> = DEFAULT_IGNORES.iter().map(|s| (*s).to_owned()).collect();
        assert!(is_ignored(Path::new("/w/target/x"), &ig));
        assert!(is_ignored(Path::new("/w/node_modules/pkg/index.js"), &ig));
        assert!(!is_ignored(Path::new("/w/src/main.rs"), &ig));
        assert!(
            !is_ignored(Path::new("/w/targets/file.txt"), &ig),
            "component-exact only"
        );
    }

    #[test]
    fn classify_maps_notify_kinds() {
        assert_eq!(
            classify(&EventKind::Create(notify::event::CreateKind::File)),
            ChangeKind::Created
        );
        assert_eq!(
            classify(&EventKind::Remove(notify::event::RemoveKind::File)),
            ChangeKind::Removed
        );
        assert_eq!(classify(&EventKind::Any), ChangeKind::Other);
    }

    #[test]
    fn merge_kind_rules() {
        use ChangeKind as C;
        assert_eq!(merge_kind(C::Created, C::Removed), C::Removed);
        assert_eq!(merge_kind(C::Modified, C::Removed), C::Removed);
        assert_eq!(merge_kind(C::Created, C::Modified), C::Created);
        assert_eq!(merge_kind(C::Created, C::Other), C::Created);
    }

    /// Pull events until one matches `want` (ignore noise like the root
    /// dir's own mtime bumps), or fail the test after `budget`.
    async fn next_matching(w: &WorkspaceWatcher, want: &Path, budget: Duration) -> WorkspaceEvent {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ev = tokio::time::timeout(remaining, w.next())
                .await
                .expect("timed out waiting for event")
                .expect("event stream open");
            if ev.path == want {
                return ev;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn coalesces_bursts_into_one_event_per_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut config = WatcherConfig::new(vec![root.clone()]);
        config.debounce = Duration::from_millis(250);
        let watcher = WorkspaceWatcher::start(config).unwrap();

        let file = root.join("a.rs");
        // Three quick writes must collapse to exactly one debounced event.
        for i in 0..3 {
            std::fs::write(&file, format!("v{i}")).unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let first = next_matching(&watcher, &file, Duration::from_secs(5)).await;
        assert!(matches!(
            first.kind,
            ChangeKind::Created | ChangeKind::Modified
        ));
        // No second event for the same path within another quiet window:
        // anything arriving must be a foreign path (e.g. the root dir).
        let quiet = tokio::time::timeout(Duration::from_millis(600), async {
            loop {
                match watcher.next().await {
                    Some(ev) if ev.path == file => panic!("burst not coalesced: {ev:?}"),
                    Some(_) => continue,
                    None => return,
                }
            }
        })
        .await;
        assert!(quiet.is_err(), "quiet window elapsed");
        drop(watcher);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ignores_do_not_wake_subscribers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut config = WatcherConfig::new(vec![root.clone()]);
        config.debounce = Duration::from_millis(150);
        let watcher = WorkspaceWatcher::start(config).unwrap();

        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/junk.o"), b"x").unwrap();
        std::fs::write(root.join("real.rs"), b"fn main() {}").unwrap();

        let ev = next_matching(&watcher, &root.join("real.rs"), Duration::from_secs(5)).await;
        assert_eq!(ev.kind, ChangeKind::Created);
        // Nothing for target/* ever: collect what remains in a quiet window.
        let mut seen = vec![ev];
        while let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(400), watcher.next()).await
        {
            seen.push(ev);
        }
        assert!(
            seen.iter()
                .all(|e| !e.path.starts_with(root.join("target"))),
            "ignored path emitted: {seen:?}"
        );
        drop(watcher);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removal_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut config = WatcherConfig::new(vec![root.clone()]);
        config.debounce = Duration::from_millis(150);
        let watcher = WorkspaceWatcher::start(config).unwrap();

        let file = root.join("doomed.rs");
        std::fs::write(&file, b"x").unwrap();
        next_matching(&watcher, &file, Duration::from_secs(5)).await;
        std::fs::remove_file(&file).unwrap();
        let ev = next_matching(&watcher, &file, Duration::from_secs(5)).await;
        assert_eq!(ev.kind, ChangeKind::Removed);
        drop(watcher);
    }
}
