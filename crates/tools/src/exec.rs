//! Governed command execution.
//!
//! One funnel for every tool subprocess: build-slot queue, memory
//! headroom admission, affinity/env-caps/niceness from the governor, and
//! stdout+stderr streamed into bounded ring buffers so a chatty compiler
//! can never blow up daemon RAM.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use openchat_core::RingBuffer;
use openchat_governor::{CommandBuilder, Governor};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

/// README default: max 256KB retained per stream per process run.
pub const DEFAULT_OUTPUT_CAP: usize = 256 * 1024;

/// Whether the command competes for a serialized build slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// Shell, git, grep: runs immediately, still pinned and capped.
    Light,
    /// cargo/tsc/pytest-class work: takes a `max_concurrent_builds` slot
    /// and refuses to start under memory pressure.
    Build,
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub kind: TaskKind,
    /// Wall-clock ceiling; the child is killed when it trips.
    pub timeout: Duration,
    /// Ring-buffer cap for each stream.
    pub output_cap: usize,
}

impl ExecRequest {
    #[must_use]
    pub fn new(program: &str, kind: TaskKind) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            kind,
            timeout: Duration::from_secs(120),
            output_cap: DEFAULT_OUTPUT_CAP,
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_owned());
        self
    }
}

/// Completed run. `stdout`/`stderr` are the retained tails, with a
/// truncation notice when older bytes were evicted.
#[derive(Debug, Clone)]
pub struct ExecReport {
    /// Child exit code, `None` when killed by a signal or by timeout.
    pub exit_code: Option<i32>,
    /// Signal number that killed the child (Unix), when applicable. The
    /// memory diagnostics trap keys on SIGKILL/SIGSEGV/SIGABRT here.
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
    pub stdout: String,
    pub stderr: String,
    pub stdout_dropped: u64,
    pub stderr_dropped: u64,
}

impl ExecReport {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("failed to spawn {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("free RAM below low-water mark; build work refused (retry after pressure drops)")]
    NoHeadroom,
}

/// Executes [`ExecRequest`]s under one [`Governor`].
#[derive(Debug)]
pub struct ToolRunner<'g> {
    governor: &'g Governor,
}

impl<'g> ToolRunner<'g> {
    #[must_use]
    pub const fn new(governor: &'g Governor) -> Self {
        Self { governor }
    }

    /// Run to completion (or timeout kill). `Build` requests hold a queue
    /// slot for the child's whole lifetime, so N agents never mean N
    /// concurrent compilers.
    pub async fn run(&self, req: &ExecRequest) -> Result<ExecReport, ExecError> {
        let _slot = match req.kind {
            TaskKind::Light => None,
            TaskKind::Build => {
                if !self.governor.memory_ok() {
                    return Err(ExecError::NoHeadroom);
                }
                tracing::debug!(program = %req.program, "waiting for build slot");
                Some(self.governor.acquire_build().await)
            }
        };

        let mut std_cmd = CommandBuilder::new(&req.program)
            .govern(self.governor)
            .build();
        for arg in &req.args {
            std_cmd.arg(arg);
        }
        if let Some(cwd) = &req.cwd {
            std_cmd.current_dir(cwd);
        }
        std_cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| ExecError::Spawn {
                program: req.program.clone(),
                source,
            })?;
        // Pipes were taken right after spawn; unwrap is infallible here.
        let out_pipe = child.stdout.take().expect("stdout piped");
        let err_pipe = child.stderr.take().expect("stderr piped");

        let started = tokio::time::Instant::now();
        let cap = req.output_cap.max(64);
        let pump = tokio::spawn(async move {
            // Both streams drain concurrently so a full stdout pipe can
            // never deadlock a child busy on stderr (and vice versa).
            let (out, err) = tokio::join!(pump_stream(out_pipe, cap), pump_stream(err_pipe, cap));
            (out, err)
        });

        let status = tokio::time::timeout(req.timeout, child.wait()).await;
        let timed_out = status.is_err();
        if timed_out {
            let _ = child.start_kill();
        }
        // Reap in both paths: after a kill, wait resolves promptly.
        let status = match status {
            Ok(res) => res,
            Err(_elapsed) => child.wait().await,
        };
        let (stdout_buf, stderr_buf) = pump.await.expect("pump task never panics");
        let (exit_code, signal) = match status {
            Ok(st) => (st.code(), exit_signal(&st)),
            Err(_) => (None, None),
        };
        Ok(ExecReport {
            exit_code,
            signal,
            timed_out,
            duration: started.elapsed(),
            stdout: stdout_buf.to_string_lossy(),
            stderr: stderr_buf.to_string_lossy(),
            stdout_dropped: stdout_buf.dropped_bytes(),
            stderr_dropped: stderr_buf.dropped_bytes(),
        })
    }
}

/// Signal number that terminated the child, where the platform reports one.
#[cfg(unix)]
fn exit_signal(st: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    st.signal()
}

#[cfg(not(unix))]
fn exit_signal(_st: &std::process::ExitStatus) -> Option<i32> {
    None
}

async fn pump_stream<R: AsyncRead + Unpin>(reader: R, cap: usize) -> RingBuffer {
    let mut ring = RingBuffer::new(cap);
    let mut reader = reader;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => ring.push(&chunk[..n]),
            // A read error mid-stream must not lose the bytes we kept.
            Err(e) => {
                tracing::debug!(error = %e, "stream read error, stopping pump");
                break;
            }
        }
    }
    ring
}

#[cfg(test)]
mod tests {
    use super::*;

    fn governor(builds: usize) -> Governor {
        let cfg = openchat_governor::GovernorConfig {
            max_concurrent_builds: builds,
            low_process_priority: false, // CI sandboxes may forbid nice
            ..Default::default()
        };
        Governor::new(cfg).unwrap()
    }

    #[tokio::test]
    async fn light_command_round_trips_output() {
        let g = governor(2);
        let runner = ToolRunner::new(&g);
        let req = ExecRequest::new("echo", TaskKind::Light).arg("hello");
        let report = runner.run(&req).await.unwrap();
        assert!(report.succeeded());
        assert_eq!(report.stdout.trim(), "hello");
        assert_eq!(report.stderr_dropped, 0);
    }

    #[tokio::test]
    async fn env_thread_caps_reach_the_child() {
        let g = governor(2);
        let runner = ToolRunner::new(&g);
        let req = ExecRequest::new("/bin/sh", TaskKind::Light)
            .arg("-c")
            .arg("printf %s \"$CARGO_BUILD_JOBS$RAYON_NUM_THREADS\"");
        let report = runner.run(&req).await.unwrap();
        let expected = format!("{}{}", g.cpus().len(), g.cpus().len());
        assert_eq!(report.stdout.trim(), expected);
    }

    #[tokio::test]
    async fn huge_output_is_bounded_to_the_tail() {
        let g = governor(2);
        let runner = ToolRunner::new(&g);
        let mut req = ExecRequest::new("/bin/sh", TaskKind::Light);
        req.output_cap = 4096;
        let req = req.arg("-c").arg("yes 0123456789 | head -n 40000"); // ~360KB
        let report = runner.run(&req).await.unwrap();
        assert!(report.succeeded(), "{}", report.stderr);
        assert!(
            report.stdout_dropped > 300_000,
            "dropped {}",
            report.stdout_dropped
        );
        assert!(report.stdout.contains("bytes truncated"));
        assert!(report.stdout.ends_with("0123456789\n"));
        assert!(
            report.stdout.len() < 4096 + 64,
            "kept size {}",
            report.stdout.len()
        );
    }

    #[tokio::test]
    async fn timeout_kills_and_reports() {
        let g = governor(2);
        let runner = ToolRunner::new(&g);
        let mut req = ExecRequest::new("sleep", TaskKind::Light);
        req.timeout = Duration::from_millis(150);
        let req = req.arg("30");
        let started = std::time::Instant::now();
        let report = runner.run(&req).await.unwrap();
        assert!(report.timed_out);
        assert_eq!(report.exit_code, None);
        assert!(!report.succeeded());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "child was actually killed"
        );
    }

    #[tokio::test]
    async fn build_requests_are_serialized() {
        let g = governor(1);
        let runner = ToolRunner::new(&g);
        let mk = || {
            ExecRequest::new("/bin/sh", TaskKind::Build)
                .arg("-c")
                .arg("sleep 0.3")
        };
        let started = std::time::Instant::now();
        let req_a = mk();
        let req_b = mk();
        // Two build jobs, one slot: wall time must cover both sleeps.
        let (a, b) = tokio::join!(runner.run(&req_a), runner.run(&req_b));
        assert!(a.unwrap().succeeded() && b.unwrap().succeeded());
        assert!(
            started.elapsed() >= Duration::from_millis(550),
            "serialized builds took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn signal_death_is_captured() {
        // The diagnostics trap needs the raw signal, not just "no exit
        // code": make the child SIGKILL itself and check we see it.
        let g = governor(2);
        let runner = ToolRunner::new(&g);
        let req = ExecRequest::new("/bin/sh", TaskKind::Light)
            .arg("-c")
            .arg("kill -9 $$");
        let report = runner.run(&req).await.unwrap();
        assert_eq!(report.signal, Some(9), "SIGKILL must surface");
        assert_eq!(report.exit_code, None);
        assert!(!report.timed_out);
    }

    #[tokio::test]
    async fn spawn_failure_is_typed() {
        let g = governor(1);
        let runner = ToolRunner::new(&g);
        let req = ExecRequest::new("/definitely/not/here", TaskKind::Light);
        let err = runner.run(&req).await.unwrap_err();
        assert!(matches!(err, ExecError::Spawn { .. }), "{err}");
    }
}
