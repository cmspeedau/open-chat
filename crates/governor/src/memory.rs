//! Memory headroom guard.
//!
//! Before admitting new heavy work the governor checks free-RAM fraction
//! from `/proc/meminfo` and refuses at build-queue admission when it drops
//! below the low-water mark (README: auto-throttle under 15% free), giving
//! the OOM problem a proactive floor instead of posthumous logs.

/// Headroom telemetry and admission check for new heavy work.
#[derive(Debug, Clone, Copy)]
pub struct MemoryHeadroom {
    low_water: f64,
    budget_bytes: u64,
}

impl MemoryHeadroom {
    #[must_use]
    pub const fn new(low_water: f64) -> Self {
        Self {
            low_water,
            budget_bytes: 0,
        }
    }

    #[must_use]
    pub const fn with_budget(low_water: f64, budget_bytes: u64) -> Self {
        Self {
            low_water,
            budget_bytes,
        }
    }

    /// Per-worker memory ceiling from user config.
    #[must_use]
    pub const fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// Free/total RAM ratio, or `None` where the platform has no cheap
    /// equivalent (Windows path is a tracked follow-up).
    #[must_use]
    pub fn free_ratio(&self) -> Option<f64> {
        read_meminfo().map(|(free, total)| {
            if total == 0 {
                1.0
            } else {
                free as f64 / total as f64
            }
        })
    }

    /// True when new heavy work may start. Platforms without meminfo report
    /// ok; the env caps and build queue still bound the damage.
    #[must_use]
    pub fn has_headroom(&self) -> bool {
        match self.free_ratio() {
            Some(ratio) if ratio < self.low_water => {
                tracing::warn!(
                    free = format!("{ratio:.1}%"),
                    low_water = self.low_water,
                    "memory headroom exhausted, heavy work deferred"
                );
                false
            }
            _ => true,
        }
    }
}

/// (MemAvailable, MemTotal) in kB from /proc/meminfo.
#[cfg(target_os = "linux")]
fn read_meminfo() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo(&text)
}

#[cfg(not(target_os = "linux"))]
fn read_meminfo() -> Option<(u64, u64)> {
    None
}

/// Pure parser, testable everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let mut available = None;
    let mut total = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("MemAvailable:") => available = fields.next()?.parse().ok(),
            Some("MemTotal:") => total = fields.next()?.parse().ok(),
            _ => {}
        }
        if available.is_some() && total.is_some() {
            break;
        }
    }
    Some((available?, total?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
MemTotal:       16384000 kB
MemFree:         1000000 kB
MemAvailable:    8192000 kB
Buffers:          128000 kB
";

    #[test]
    fn parses_available_and_total() {
        assert_eq!(parse_meminfo(SAMPLE), Some((8_192_000, 16_384_000)));
    }

    #[test]
    fn missing_field_is_none() {
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn refuses_below_low_water() {
        let guard = MemoryHeadroom::new(0.99); // wants 99% free
        // Live machine check only meaningful where meminfo exists.
        if cfg!(target_os = "linux") {
            assert!(!guard.has_headroom(), "99% low-water must refuse");
        }
        let permissive = MemoryHeadroom::new(0.0);
        assert!(permissive.has_headroom());
    }

    #[test]
    fn live_meminfo_reads_sane_ratio() {
        if cfg!(target_os = "linux") {
            let guard = MemoryHeadroom::new(0.15);
            let ratio = guard.free_ratio().expect("meminfo on linux");
            assert!((0.0..=1.0).contains(&ratio), "ratio {ratio}");
        }
    }
}
