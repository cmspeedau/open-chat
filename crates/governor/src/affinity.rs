//! Core-affinity pinning for child processes.
//!
//! On Linux the child is pinned with `sched_setaffinity` between fork and
//! exec, so a governed compiler can physically never touch cores outside the
//! user's budget. Elsewhere (Windows/macOS) pinning is a no-op for now and
//! the env thread caps carry the limit; the README's Windows
//! `SetProcessAffinityMask` path is tracked as a follow-up.

use crate::CpuSet;
use crate::cpu::CpuMask;

/// Pin the calling (post-fork, pre-exec) context to `cores`.
///
/// # Errors
///
/// OS error if the kernel rejects the mask.
#[cfg(target_os = "linux")]
pub fn pin_current_to(cores: &[usize]) -> Result<(), std::io::Error> {
    let mut mask = CpuMask::zeroed();
    for &core in cores {
        // The 128-byte buffer covers 1024 cpus (the kernel default); ids
        // beyond it can never have been granted by CpuSet::resolve on any
        // real box, but guard anyway so a bug degrades into an error
        // instead of memory corruption.
        if core >= 1024 {
            return Err(std::io::Error::other(format!(
                "core id {core} out of range"
            )));
        }
        mask.set_bit(core);
    }
    // SAFETY: runs in the child between fork and exec. `sched_setaffinity`
    // is a direct syscall (async-signal-safe: no locks, no allocator), the
    // mask is a fully-initialized POD on our stack, and the size argument
    // matches the buffer exactly, per the cpu_set(3) contract.
    let rc = unsafe {
        libc::sched_setaffinity(0, std::mem::size_of::<CpuMask>(), (&raw const mask).cast())
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn pin_current_to(_cores: &[usize]) -> Result<(), std::io::Error> {
    Ok(())
}

/// Marker attaching a [`CpuSet`] to spawned children.
#[derive(Debug, Clone, Copy)]
pub struct CoreAffinity<'a>(pub &'a CpuSet);

impl CoreAffinity<'_> {
    /// Apply this affinity to the calling context. See [`pin_current_to`].
    pub fn apply(&self) -> Result<(), std::io::Error> {
        pin_current_to(self.0.cores())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::CpuCores;

    #[test]
    fn pinning_a_governed_child_works_and_survives_exec() {
        // Real end-to-end proof: a child pinned to our cores still runs its
        // exec'd program fine. `/bin/sh` rather than `/bin/true`, because a
        // Nix sandbox has no /bin/true - the point is the exec after
        // sched_setaffinity, not which FHS binary gets to be the witness.
        let set = CpuSet::resolve(CpuCores::Count(1)).unwrap();
        let out = crate::CommandBuilder::new("/bin/sh")
            .govern_to(&set)
            .arg("-c")
            .arg("exit 0")
            .output()
            .expect("spawn governed /bin/sh");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn child_reports_pinned_mask() {
        let set = CpuSet::resolve(CpuCores::Count(2)).unwrap();
        assert!(set.len() <= crate::cpu::usable_core_count());
        // Ask the kernel what affinity it actually gave the child, through
        // procfs instead of `taskset`: no util-linux needed, so this proves
        // the same thing in a Nix sandbox as on a normal distro.
        let child = crate::CommandBuilder::new("/bin/sh")
            .govern_to(&set)
            .arg("-c")
            .arg("cat /proc/self/status")
            .output()
            .expect("spawn sh");
        let text = String::from_utf8_lossy(&child.stdout);
        let listed = text
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .unwrap_or_default()
            .trim();
        let count = listed
            .split(',')
            .flat_map(|part| part.split('-').map(str::trim))
            .filter(|s| !s.is_empty())
            .count();
        assert_eq!(
            count,
            set.len(),
            "child mask {listed:?} != budget {}",
            set.len()
        );
    }
}
