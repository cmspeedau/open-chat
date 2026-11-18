//! Multi-agent host resource arbiter (Roadmap Phase 1).
//!
//! Everything an agent spawns (compilers, linters, tests) goes through a
//! [`Governor`], which enforces:
//!
//! 1. a user-configurable CPU core cap (`max_cpu_cores`, count or percentage),
//! 2. OS core-affinity pinning of child processes (Linux; portable elsewhere),
//! 3. build-tool thread-cap environment injection (`CARGO_BUILD_JOBS`, …),
//! 4. a serialized build/lint queue (`max_concurrent_builds`),
//! 5. lowered subprocess priority (`nice`),
//! 6. a memory headroom guard that refuses new heavy work when free RAM is
//!    below the low-water mark.

mod affinity;
mod builder;
mod cpu;
mod memory;
mod priority;
mod queue;

pub use affinity::{CoreAffinity, pin_current_to};
pub use builder::CommandBuilder;
pub use cpu::{CpuCores, CpuSet, usable_core_count};
pub use memory::MemoryHeadroom;
pub use priority::Priority;
pub use queue::{BuildGuard, BuildQueue};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GovernorError {
    #[error("invalid core spec: {0}")]
    InvalidCoreSpec(String),
    #[error("cpu limit {wanted} exceeds usable cores {usable}")]
    TooManyCores { wanted: usize, usable: usize },
    #[error("os error while applying resource limits: {0}")]
    Os(#[from] std::io::Error),
}

/// Tunables mirroring the `[resources]` table in `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct GovernorConfig {
    /// `max_cpu_cores`: absolute count or percentage of usable cores.
    pub max_cpu_cores: CpuCores,
    /// `max_concurrent_builds`: parallel compiler/build subprocess slots.
    pub max_concurrent_builds: usize,
    /// `max_worker_memory`: ceiling used by the headroom guard, e.g. 4 GiB.
    pub max_worker_memory: u64,
    /// `low_process_priority`: nice the children down.
    pub low_process_priority: bool,
    /// Free-RAM fraction below which heavy work is refused (README: 15%).
    pub memory_low_water: f64,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            // Leave desktop responsive: half the machine, never all of it.
            max_cpu_cores: CpuCores::Percent(50),
            max_concurrent_builds: 1,
            max_worker_memory: 4 * 1024 * 1024 * 1024,
            low_process_priority: true,
            memory_low_water: 0.15,
        }
    }
}

/// The arbiter: owns the core budget, the shared build semaphore, the
/// priority policy, and the memory headroom guard.
#[derive(Debug)]
pub struct Governor {
    cpu: CpuSet,
    queue: BuildQueue,
    priority: Priority,
    headroom: MemoryHeadroom,
}

impl Governor {
    /// Resolve the config against the live machine (usable core count, etc.).
    pub fn new(config: GovernorConfig) -> Result<Self, GovernorError> {
        let cpu = CpuSet::resolve(config.max_cpu_cores)?;
        tracing::info!(
            cores = cpu.len(),
            builds = config.max_concurrent_builds,
            "governor online"
        );
        Ok(Self {
            cpu,
            queue: BuildQueue::new(config.max_concurrent_builds.max(1)),
            priority: Priority::new(config.low_process_priority),
            headroom: MemoryHeadroom::with_budget(
                config.memory_low_water,
                config.max_worker_memory,
            ),
        })
    }

    /// Cores the governor has granted itself after user cap + OS availability.
    #[must_use]
    pub fn cpus(&self) -> &CpuSet {
        &self.cpu
    }

    /// Per-process memory ceiling from config.
    #[must_use]
    pub fn max_worker_memory(&self) -> u64 {
        self.headroom.budget_bytes()
    }

    /// Wait for a build slot. Holds a permit until the returned guard drops.
    pub async fn acquire_build(&self) -> BuildGuard<'_> {
        self.queue.acquire().await
    }

    /// Non-blocking build-slot probe for interactive paths.
    #[must_use]
    pub fn try_acquire_build(&self) -> Option<BuildGuard<'_>> {
        self.queue.try_acquire()
    }

    /// Nice nudge governed children receive, per `low_process_priority`.
    #[must_use]
    pub fn child_nice(&self) -> Option<i32> {
        self.priority.child_nice()
    }

    /// Build a [`std::process::Command`] pre-wired with affinity, env thread
    /// caps, and niceness for `program`. If the OS refuses the pin at spawn
    /// time, the spawn itself surfaces the error; env caps are applied
    /// regardless, so on platforms without pinning the limits still hold.
    pub fn command(&self, program: &str) -> std::process::Command {
        CommandBuilder::new(program).govern(self).build()
    }

    /// True when the machine has enough free RAM to start new heavy work.
    pub fn memory_ok(&self) -> bool {
        self.headroom.has_headroom()
    }

    /// Per-turn telemetry snapshot for the observability badges (Phase 4).
    #[must_use]
    pub fn snapshot(&self) -> GovernorSnapshot {
        GovernorSnapshot {
            cores: self.cpu.len(),
            usable_total: self.cpu.usable_total(),
            free_build_slots: self.queue.available_slots(),
            memory_free_ratio: self.headroom.free_ratio(),
            nice_applied: self.priority.child_nice(),
        }
    }
}

/// Plain-data view of arbiter state, safe to serialize into status feeds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GovernorSnapshot {
    pub cores: usize,
    pub usable_total: usize,
    pub free_build_slots: usize,
    pub memory_free_ratio: Option<f64>,
    pub nice_applied: Option<i32>,
}
