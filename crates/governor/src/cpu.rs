//! Core budget accounting: user spec -> resolved CPU set.

use crate::GovernorError;

/// How the user asked to cap cores (`max_cpu_cores` in config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuCores {
    /// Absolute core count, e.g. `4`.
    Count(usize),
    /// Percentage of usable cores, e.g. `"50%"`.
    Percent(u8),
}

/// Cores the governor actually owns after resolving the user spec against
/// what the OS lets this process use (cgroups and `taskset` shrink this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuSet {
    cores: Vec<usize>,
    usable_total: usize,
}

impl CpuSet {
    /// Resolve `spec` against the cores this process may run on.
    ///
    /// # Errors
    ///
    /// [`GovernorError::InvalidCoreSpec`] for zero/over-100 specs,
    /// [`GovernorError::TooManyCores`] when a fixed count exceeds the machine.
    pub fn resolve(spec: CpuCores) -> Result<Self, GovernorError> {
        Self::resolve_against(spec, usable_core_count())
    }

    /// Pure half of [`resolve`](Self::resolve), testable on any platform.
    pub(crate) fn resolve_against(spec: CpuCores, usable: usize) -> Result<Self, GovernorError> {
        let wanted = match spec {
            CpuCores::Count(0) => {
                return Err(GovernorError::InvalidCoreSpec("0 cores".into()));
            }
            CpuCores::Count(n) if n > usable => {
                return Err(GovernorError::TooManyCores { wanted: n, usable });
            }
            CpuCores::Count(n) => n,
            CpuCores::Percent(p) if p == 0 || p > 100 => {
                return Err(GovernorError::InvalidCoreSpec(format!("{p}%")));
            }
            // Percent always fits (and never picks 0 cores), so it degrades
            // gracefully on small machines instead of failing startup.
            CpuCores::Percent(p) => (usable * usize::from(p)).div_ceil(100).max(1),
        };
        Ok(Self {
            cores: (0..wanted).collect(),
            usable_total: usable,
        })
    }

    /// Granted core ids, dense from 0.
    #[must_use]
    pub fn cores(&self) -> &[usize] {
        &self.cores
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cores.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cores.is_empty()
    }

    /// Total cores the OS allows this process (before the user cap).
    #[must_use]
    pub fn usable_total(&self) -> usize {
        self.usable_total
    }

    /// Low 64 bits of the affinity mask, bit i = core i. v1 supports machines
    /// up to 64 cores; larger hosts should use `Percent`, which stays correct.
    #[must_use]
    pub fn mask(&self) -> u64 {
        self.cores.iter().fold(0u64, |m, &c| m | (1u64 << c))
    }

    /// Thread-cap environment values injected into every governed command,
    /// per the README's "Build Tool Thread Injection" contract.
    #[must_use]
    pub fn env_caps(&self) -> [(&'static str, String); 4] {
        let n = self.len().to_string();
        [
            ("CARGO_BUILD_JOBS", n.clone()),
            ("RAYON_NUM_THREADS", n.clone()),
            ("UV_THREAD_POOL_COUNT", n.clone()),
            ("MAKEFLAGS", format!("-j{n}")),
        ]
    }
}

/// An opaque affinity mask buffer handed to the `sched_*affinity` syscalls.
/// `cpu_set_t` is just `__cpu_mask` (a `c_ulong`) for the default 1024-CPU
/// size on glibc, but we pass our own POD buffer so the layout is explicit
/// and independent of libc's type aliases.
#[derive(Clone, Copy)]
#[repr(C, align(8))]
pub(crate) struct CpuMask([u8; 128]);

impl Default for CpuMask {
    fn default() -> Self {
        Self::zeroed()
    }
}

impl CpuMask {
    #[must_use]
    pub(crate) const fn zeroed() -> Self {
        Self([0u8; 128])
    }

    pub(crate) fn set_bit(&mut self, core: usize) {
        self.0[core / 8] |= 1 << (core % 8);
    }

    #[must_use]
    pub(crate) fn is_set(&self, core: usize) -> bool {
        self.0[core / 8] & (1 << (core % 8)) != 0
    }

    /// Core ids present in the mask.
    pub(crate) fn bits(&self) -> impl Iterator<Item = usize> + use<'_> {
        (0..1024).filter(|&c| self.is_set(c))
    }
}

/// Cores this *process* is allowed to run on. On Linux this honors an
/// inherited affinity mask (`taskset`, cgroups); elsewhere it falls back to
/// `available_parallelism`.
#[must_use]
pub fn usable_core_count() -> usize {
    linux::sched_getaffinity_mask().map_or_else(
        || std::thread::available_parallelism().map_or(1, usize::from),
        |mask| mask.bits().count(),
    )
}

#[cfg(target_os = "linux")]
mod linux {
    use super::CpuMask;

    /// This process's affinity mask, if the kernel reports one.
    pub(crate) fn sched_getaffinity_mask() -> Option<CpuMask> {
        let mut mask = CpuMask::default();
        let rc = unsafe {
            // SAFETY: plain syscall reading into a valid buffer whose length
            // we pass explicitly; no invariants beyond that (sched_getaffinity(2)).
            libc::sched_getaffinity(0, std::mem::size_of::<CpuMask>(), (&raw mut mask).cast())
        };
        if rc == 0 { Some(mask) } else { None }
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    use super::CpuMask;

    pub(crate) fn sched_getaffinity_mask() -> Option<CpuMask> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_within_machine() {
        let set = CpuSet::resolve_against(CpuCores::Count(4), 8).unwrap();
        assert_eq!(set.cores(), &[0, 1, 2, 3]);
        assert_eq!(set.usable_total(), 8);
        assert_eq!(set.mask(), 0b0000_1111);
    }

    #[test]
    fn percent_rounds_up_and_floors_at_one() {
        let set = CpuSet::resolve_against(CpuCores::Percent(50), 8).unwrap();
        assert_eq!(set.len(), 4);
        let set = CpuSet::resolve_against(CpuCores::Percent(10), 4).unwrap();
        assert_eq!(set.len(), 1, "tiny slices must still get one core");
    }

    #[test]
    fn percent_never_exceeds_machine() {
        let set = CpuSet::resolve_against(CpuCores::Percent(100), 6).unwrap();
        assert_eq!(set.len(), 6);
    }

    #[test]
    fn rejects_bad_specs() {
        assert!(matches!(
            CpuSet::resolve_against(CpuCores::Count(0), 8),
            Err(GovernorError::InvalidCoreSpec(_))
        ));
        assert!(matches!(
            CpuSet::resolve_against(CpuCores::Percent(0), 8),
            Err(GovernorError::InvalidCoreSpec(_))
        ));
        assert!(matches!(
            CpuSet::resolve_against(CpuCores::Count(9), 8),
            Err(GovernorError::TooManyCores {
                wanted: 9,
                usable: 8
            })
        ));
    }

    #[test]
    fn env_caps_match_core_count() {
        let set = CpuSet::resolve_against(CpuCores::Count(2), 8).unwrap();
        let env = set.env_caps();
        assert_eq!(env[0], ("CARGO_BUILD_JOBS", "2".into()));
        assert_eq!(env[3], ("MAKEFLAGS", "-j2".into()));
    }

    #[test]
    fn resolves_against_live_machine() {
        let usable = usable_core_count();
        assert!((1..=64).contains(&usable) || usable > 64);
        let set = CpuSet::resolve_against(CpuCores::Count(usable.min(2)), usable).unwrap();
        assert!(set.len() <= usable);
    }
}
