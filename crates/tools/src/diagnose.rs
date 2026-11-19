//! Runtime memory-bug diagnostics (Roadmap Phase 2, last item).
//!
//! The "diagnostic trap": every governed command output is scanned for the
//! signatures of memory failure - sanitizer reports (ASan/LSan/TSan/MSan),
//! allocator exhaustion, and OOM-style signal deaths that exit codes alone
//! hide (SIGKILL from the kernel OOM killer, SIGSEGV, SIGABRT, and their
//! shell-reported 128+N twins). A hit produces a typed [`Diagnosis`] with
//! the headline line and a repair pointer; the session layer then arms the
//! memory-safety skill injection automatically.

/// The classified failure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryFault {
    /// ASan / HWASan report.
    AddressSanitizer,
    /// LeakSanitizer (standalone or via ASan).
    LeakSanitizer,
    ThreadSanitizer,
    MemorySanitizer,
    /// "memory allocation of N bytes failed" / capacity overflow.
    AllocationFailed,
    /// Killed by SIGKILL with no timeout we caused: kernel OOM killer or an
    /// external `kill -9`.
    KilledSigkill,
    Segfault,
    /// SIGABRT without a sanitizer banner (glibc heap corruption, C++
    /// terminate, double free).
    Abort,
    /// The governor's own headroom guard refused the spawn.
    HeadroomRefused,
}

impl MemoryFault {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AddressSanitizer => "address-sanitizer",
            Self::LeakSanitizer => "leak-sanitizer",
            Self::ThreadSanitizer => "thread-sanitizer",
            Self::MemorySanitizer => "memory-sanitizer",
            Self::AllocationFailed => "allocation-failed",
            Self::KilledSigkill => "oom-suspect-sigkill",
            Self::Segfault => "segfault",
            Self::Abort => "abort",
            Self::HeadroomRefused => "headroom-refused",
        }
    }

    #[must_use]
    pub fn advice(&self) -> &'static str {
        match self {
            Self::AddressSanitizer => {
                "fix the reported access at the cited frame; re-run the same command built with the project's sanitizer profile before claiming repair"
            }
            Self::LeakSanitizer => {
                "the leak site is in the report: give the allocation an owner whose drop frees it; leaks at exit are real leaks, 'fine after restart' is not a fix"
            }
            Self::ThreadSanitizer => {
                "the raced pair of accesses is in the report: establish one owner or a lock/channel discipline; do not paper over with sleeps"
            }
            Self::MemorySanitizer => {
                "uninitialized read: initialize on every path; the report names the stack that allocated the buffer"
            }
            Self::AllocationFailed => {
                "an unbounded or hostile-size allocation: bound the collection with input-derived sizes; reject absurd lengths at the edge"
            }
            Self::KilledSigkill => {
                "no exit code, killed by SIGKILL outside our timeout: treat as OOM. Check what grew (caches, buffers, concurrency) and cap it; run `dmesg | tail` to confirm the oom-killer"
            }
            Self::Segfault => {
                "SIGSEGV: inspect unsafe blocks and FFI boundaries first; every raw pointer use needs a // SAFETY invariant and a test that exercises it"
            }
            Self::Abort => {
                "SIGABRT without a sanitizer banner: likely heap corruption or double free; run the sanitizer build to get the allocation/free pair"
            }
            Self::HeadroomRefused => {
                "the governor refused this command because free RAM is under the low-water mark; free memory or lower max_concurrent_builds, then retry"
            }
        }
    }
}

/// A trap hit: what happened, the evidence line, and what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub fault: MemoryFault,
    /// The matching output line (truncated), or a synthetic one for signals.
    pub headline: String,
    pub advice: &'static str,
}

impl Diagnosis {
    /// One-line form for event streams.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "[memory-diagnostic] {}: {}",
            self.fault.as_str(),
            self.headline
        )
    }

    /// Two-line form injected into the model-facing tool output.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "[memory-diagnostic] fault={} evidence={:?}\nnext step: {}",
            self.fault.as_str(),
            self.headline,
            self.advice,
        )
    }
}

/// Sanitizer banners, most-specific needle first (HWASan contains ASan's
/// substring, so it must be tested before plain ASan).
const SANITIZERS: &[(&str, MemoryFault)] = &[
    ("HWADDRESSSANITIZER", MemoryFault::AddressSanitizer),
    ("ERROR: ADDRESSSANITIZER", MemoryFault::AddressSanitizer),
    ("LEAKSANITIZER", MemoryFault::LeakSanitizer),
    ("DETECTED MEMORY LEAKS", MemoryFault::LeakSanitizer),
    ("ERROR: THREADSANITIZER", MemoryFault::ThreadSanitizer),
    ("DATA RACE", MemoryFault::ThreadSanitizer),
    ("ERROR: MEMORYSANITIZER", MemoryFault::MemorySanitizer),
];

const ALLOC_NEEDLES: &[&str] = &[
    "MEMORY ALLOCATION OF",
    "CAPACITY OVERFLOW",
    "OUT OF MEMORY",
    "STD::BAD_ALLOC",
];

const SIGKILL: i32 = 9;
const SIGSEGV: i32 = 11;
const SIGABRT: i32 = 6;

/// Classify one finished command. `timed_out` suppresses signal/exit-code
/// faulting: a timeout kill is our own deliberate action, not a bug
/// signature. Sanitizer banners outrank signals because sanitizer runs die
/// by SEGV/ABRT on purpose.
#[must_use]
pub fn diagnose(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
) -> Option<Diagnosis> {
    let haystack = format!("{}\n{stderr}", tail_of(stdout, 16 * 1024),);
    let upper = haystack.to_ascii_uppercase();
    if let Some((needle, fault)) = SANITIZERS.iter().find(|(n, _)| upper.contains(n)) {
        let headline = matching_line(&haystack, needle);
        return Some(build(*fault, headline));
    }
    if let Some(needle) = ALLOC_NEEDLES.iter().find(|n| upper.contains(**n)) {
        return Some(build(
            MemoryFault::AllocationFailed,
            matching_line(&haystack, needle),
        ));
    }
    if timed_out {
        return None;
    }
    if let Some(sig) = signal {
        let fault = match sig {
            SIGKILL => MemoryFault::KilledSigkill,
            SIGSEGV => MemoryFault::Segfault,
            SIGABRT => MemoryFault::Abort,
            _ => return None,
        };
        return Some(build(fault, format!("killed by signal {sig}")));
    }
    // Shell-reported 128+signal when a wrapper ate the raw status.
    if let Some(code) = exit_code {
        let fault = match code {
            137 => MemoryFault::KilledSigkill,
            139 => MemoryFault::Segfault,
            134 => MemoryFault::Abort,
            _ => return None,
        };
        return Some(build(
            fault,
            format!("exit code {code} (128+signal convention)"),
        ));
    }
    None
}

/// Diagnosis for the governor refusing work at the memory low-water mark.
#[must_use]
pub fn headroom_refused() -> Diagnosis {
    build(
        MemoryFault::HeadroomRefused,
        "governor: free RAM below low-water mark".into(),
    )
}

fn build(fault: MemoryFault, headline: String) -> Diagnosis {
    Diagnosis {
        fault,
        headline: truncate_line(&headline),
        advice: fault.advice(),
    }
}

fn tail_of(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut cut = s.len() - max;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    &s[cut..]
}

/// First line containing `needle` (case-insensitive), for evidence.
fn matching_line(haystack: &str, needle_upper: &str) -> String {
    haystack
        .lines()
        .find(|l| l.to_ascii_uppercase().contains(needle_upper))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .unwrap_or(needle_upper)
        .to_owned()
}

fn truncate_line(l: &str) -> String {
    const MAX: usize = 200;
    let mut end = l.len().min(MAX);
    while !l.is_char_boundary(end) {
        end -= 1;
    }
    l[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(sig: &str) -> Option<Diagnosis> {
        diagnose("", sig, Some(1), None, false)
    }

    #[test]
    fn asan_report_classified_with_headline() {
        let diag =
            d("==123==ERROR: AddressSanitizer: heap-use-after-free on address 0xdead (pc=0x1)")
                .expect("trap");
        assert_eq!(diag.fault, MemoryFault::AddressSanitizer);
        assert!(
            diag.headline.contains("heap-use-after-free"),
            "{}",
            diag.headline
        );
        assert!(diag.advice.contains("sanitizer profile"));
    }

    #[test]
    fn hwasan_is_not_misreported_as_asan_ordering() {
        let diag = d("ERROR: HWAddressSanitizer: tag-mismatch").unwrap();
        assert_eq!(diag.fault, MemoryFault::AddressSanitizer);
        assert!(diag.headline.contains("HWAddressSanitizer"));
    }

    #[test]
    fn leaks_and_races() {
        assert_eq!(
            d("ERROR: LeakSanitizer: detected memory leaks")
                .unwrap()
                .fault,
            MemoryFault::LeakSanitizer
        );
        assert_eq!(
            d("WARNING: ThreadSanitizer: data race (pid 7)")
                .unwrap()
                .fault,
            MemoryFault::ThreadSanitizer
        );
        assert_eq!(
            d("SUMMARY: ThreadSanitizer: data race foo.rs:3")
                .unwrap()
                .fault,
            MemoryFault::ThreadSanitizer
        );
    }

    #[test]
    fn allocator_oom_text() {
        let diag =
            d("thread 'main' panicked at 'memory allocation of 18446744073709551615 bytes failed'")
                .unwrap();
        assert_eq!(diag.fault, MemoryFault::AllocationFailed);
    }

    #[test]
    fn signals_classified_when_no_banner() {
        let diag = diagnose("working", "", None, Some(9), false).unwrap();
        assert_eq!(diag.fault, MemoryFault::KilledSigkill);
        assert_eq!(diag.headline, "killed by signal 9");
        assert_eq!(
            diagnose("", "", None, Some(11), false).unwrap().fault,
            MemoryFault::Segfault
        );
        assert_eq!(
            diagnose("", "", None, Some(6), false).unwrap().fault,
            MemoryFault::Abort
        );
        assert_eq!(
            diagnose("", "", Some(137), None, false).unwrap().fault,
            MemoryFault::KilledSigkill
        );
        assert_eq!(
            diagnose("", "", Some(139), None, false).unwrap().fault,
            MemoryFault::Segfault
        );
    }

    #[test]
    fn timeout_kill_is_ours_not_a_fault() {
        assert!(diagnose("", "", None, Some(9), true).is_none());
        assert!(diagnose("", "", Some(124), None, true).is_none());
    }

    #[test]
    fn sanitizer_banner_outranks_death_signal() {
        // ASan exits via SEGV deliberately; the banner is the diagnosis.
        let diag = diagnose(
            "",
            "ERROR: AddressSanitizer: stack-overflow",
            None,
            Some(11),
            false,
        )
        .unwrap();
        assert_eq!(diag.fault, MemoryFault::AddressSanitizer);
    }

    #[test]
    fn healthy_runs_are_quiet() {
        assert_eq!(
            diagnose("test result: ok. 140 passed", "", Some(0), None, false),
            None
        );
        assert_eq!(
            diagnose("", "warning: unused variable", Some(0), None, false),
            None
        );
        // "memory" in prose must not fire anything.
        assert_eq!(
            diagnose("memory usage looks fine", "", Some(0), None, false),
            None
        );
    }

    #[test]
    fn long_lines_and_unicode_survive_truncation() {
        let long = format!("ERROR: AddressSanitizer: {}", "ü".repeat(500));
        let diag = d(&long).unwrap();
        assert!(diag.headline.chars().count() <= 201);
        assert!(diag.render().contains("fault=address-sanitizer"));
    }

    #[test]
    fn headroom_helper() {
        assert_eq!(headroom_refused().fault, MemoryFault::HeadroomRefused);
    }
}
