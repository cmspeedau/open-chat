//! [`std::process::Command`] pre-wired with governor limits.

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::{CpuSet, Governor};

/// Builder assembling a governed child command: affinity pin, env thread
/// caps, niceness. Obtain via [`Governor::command`] or build manually with
/// [`CommandBuilder::govern_to`] for tests.
#[derive(Debug)]
pub struct CommandBuilder {
    inner: Command,
}

impl CommandBuilder {
    #[must_use]
    pub fn new(program: &str) -> Self {
        Self {
            inner: Command::new(program),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: &str) -> Self {
        self.inner.arg(arg);
        self
    }

    /// Apply the full governor policy: pin + env caps + niceness.
    #[must_use]
    pub fn govern(self, governor: &Governor) -> Self {
        self.govern_inner(governor.cpus(), governor.child_nice())
    }

    /// Pin + env caps only (no niceness), also used by tests.
    #[must_use]
    pub fn govern_to(self, cpus: &CpuSet) -> Self {
        self.govern_inner(cpus, None)
    }

    fn govern_inner(mut self, cpus: &CpuSet, nice: Option<i32>) -> Self {
        for (key, value) in cpus.env_caps() {
            self.inner.env(key, value);
        }
        let cores = cpus.cores().to_vec();
        #[cfg(target_os = "linux")]
        {
            // SAFETY: `pin_current_to` and `nice` are direct syscalls on
            // values moved into the closure (owned Vec / Copy int, no locks,
            // no allocator), which is exactly what the fork-exec window
            // permits; hooks run fork-to-exec in registration order.
            unsafe {
                self.inner.pre_exec(move || {
                    crate::affinity::pin_current_to(&cores)?;
                    if let Some(n) = nice {
                        if libc::nice(n) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (cores, nice); // env caps remain the enforcement mechanism
        }
        self
    }

    #[must_use]
    pub fn build(self) -> Command {
        self.inner
    }

    /// Spawn and wait, capturing output (convenience for checks/tests).
    ///
    /// # Errors
    ///
    /// OS error if spawn fails.
    pub fn output(&mut self) -> std::io::Result<std::process::Output> {
        self.inner.output()
    }
}
