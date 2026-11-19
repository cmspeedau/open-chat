//! Rolling context compactor (Roadmap Phase 1, last item).
//!
//! Every tool call enters a [`Transcript`] with its full retained output.
//! When the transcript outgrows its byte budget, the *oldest* raw entries
//! collapse into one-line digests — command, exit status, duration, and the
//! most diagnostic line — so a 30-call session stays prompt-sized while the
//! failure trails of recent calls survive untouched.

use std::collections::VecDeque;
use std::fmt::Write as _;

use crate::exec::ExecReport;

/// One recorded tool call, raw or collapsed.
#[derive(Debug, Clone)]
pub struct ToolRecord {
    /// Human command line, e.g. `cargo check --workspace`.
    pub label: String,
    pub exit_code: Option<i32>,
    /// Terminating signal number when the child died by signal (Unix).
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    /// Bytes lost to the ring-buffer cap (kept for the digester).
    pub dropped: u64,
}

impl ToolRecord {
    #[must_use]
    pub fn from_report(label: impl Into<String>, report: &ExecReport) -> Self {
        Self {
            label: label.into(),
            exit_code: report.exit_code,
            signal: report.signal,
            timed_out: report.timed_out,
            duration_ms: report.duration.as_millis().try_into().unwrap_or(u64::MAX),
            stdout: report.stdout.clone(),
            stderr: report.stderr.clone(),
            dropped: report.stdout_dropped + report.stderr_dropped,
        }
    }

    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }

    /// Bytes the raw form would cost if rendered.
    #[must_use]
    pub fn raw_len(&self) -> usize {
        self.stdout.len() + self.stderr.len() + self.label.len()
    }

    /// Semantic one-line digest: status, timing, and the most diagnostic
    /// line found in the output (first `error`/`panicked`/`FAILED` hit,
    /// else the first non-empty line of whatever exists).
    #[must_use]
    pub fn digest(&self) -> String {
        let status = if self.timed_out {
            "TIMEOUT".to_owned()
        } else {
            match self.exit_code {
                Some(0) => "ok".to_owned(),
                Some(code) => format!("exit {code}"),
                None => "killed".to_owned(),
            }
        };
        let mut out = format!("$ {} → {status} in {}ms", self.label, self.duration_ms);
        if self.dropped > 0 {
            let _ = write!(out, " ({} bytes truncated)", self.dropped);
        }
        if let Some(signal) =
            diagnostic_line(&self.stderr).or_else(|| diagnostic_line(&self.stdout))
        {
            let _ = write!(out, " | {signal}");
        }
        out
    }
}

/// First line worth remembering when the bulk of a log gets folded away:
/// build/test failures carry stable prefixes; otherwise any non-empty line.
fn diagnostic_line(text: &str) -> Option<String> {
    let mut fallback = None;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lowered = t.to_ascii_lowercase();
        if lowered.starts_with("error")
            || lowered.contains("error[")
            || lowered.starts_with("panic")
            || lowered.contains("FAILED")
            || lowered.starts_with("test result:")
        {
            return Some(shorten(t, 160));
        }
        fallback.get_or_insert_with(|| shorten(t, 160));
    }
    fallback
}

fn shorten(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        // Cut back to a char boundary.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[derive(Debug)]
enum Entry {
    Raw(ToolRecord),
    Digest(String),
}

/// Budgeted transcript of tool calls with automatic oldest-first collapse.
#[derive(Debug)]
pub struct Transcript {
    entries: VecDeque<Entry>,
    raw_bytes: usize,
    budget: usize,
}

impl Transcript {
    /// `budget` is the total raw-output bytes allowed before collapse.
    /// README context: history should stay compact between turns.
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            raw_bytes: 0,
            budget,
        }
    }

    pub fn push(&mut self, record: ToolRecord) {
        self.raw_bytes += record.raw_len();
        self.entries.push_back(Entry::Raw(record));
        self.collapse_oldest();
    }

    /// Number of entries (raw + digest) currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render for prompt assembly: digests as single lines, raw entries as
    /// a header line plus indented output.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            match entry {
                Entry::Digest(line) => {
                    let _ = writeln!(out, "{line}");
                }
                Entry::Raw(r) => {
                    let _ = writeln!(out, "{}", r.digest());
                    for line in r.stdout.lines() {
                        let _ = writeln!(out, "  | {line}");
                    }
                    for line in r.stderr.lines() {
                        let _ = writeln!(out, "  ! {line}");
                    }
                }
            }
        }
        out
    }

    fn collapse_oldest(&mut self) {
        // Never collapse the newest entry: the turn that just ran must stay
        // readable in full, that is where failures live.
        let last = self.entries.len().saturating_sub(1);
        while self.raw_bytes > self.budget {
            let Some(idx) = self.entries.iter().position(|e| matches!(e, Entry::Raw(_))) else {
                break; // nothing raw left but the newest; budget is a floor of one
            };
            if idx >= last {
                break;
            }
            if let Some(Entry::Raw(record)) = self.entries.remove(idx) {
                self.raw_bytes -= record.raw_len();
                self.entries.insert(idx, Entry::Digest(record.digest()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(label: &str, stdout: &str, stderr: &str, exit: Option<i32>) -> ToolRecord {
        ToolRecord {
            label: label.into(),
            exit_code: exit,
            signal: None,
            timed_out: false,
            duration_ms: 123,
            stdout: stdout.into(),
            stderr: stderr.into(),
            dropped: 0,
        }
    }

    #[test]
    fn digest_carries_status_and_error_line() {
        let r = record(
            "cargo check",
            "",
            "warning: unused\nerror[E0308]: mismatched types\n  --> src/main.rs:3:5",
            Some(101),
        );
        let d = r.digest();
        assert!(d.starts_with("$ cargo check → exit 101 in 123ms"), "{d}");
        assert!(d.contains("error[E0308]"), "{d}");
    }

    #[test]
    fn digest_prefers_stderr_over_stdout() {
        let r = record("cmd", "all good output", "FAILED: one test", None);
        let d = r.digest();
        assert!(d.contains("FAILED: one test"), "{d}");
        assert!(!d.contains("all good"), "{d}");
    }

    #[test]
    fn timeout_and_truncation_surface() {
        let mut r = record("tsc", "", "", None);
        r.timed_out = true;
        r.dropped = 4096;
        let d = r.digest();
        assert!(d.contains("TIMEOUT"), "{d}");
        assert!(d.contains("4096 bytes truncated"), "{d}");
        assert_eq!(d.matches('|').count(), 0, "no output => no diagnostic");
    }

    #[test]
    fn collapses_oldest_first_keeps_newest_raw() {
        let mut t = Transcript::new(100);
        t.push(record("one", &"a".repeat(80), "", Some(0)));
        t.push(record("two", &"b".repeat(80), "", Some(0)));
        t.push(record("three", &"c".repeat(80), "", Some(0)));
        let rendered = t.render();
        // Oldest collapsed to a single line without raw body.
        assert!(rendered.contains("$ one → ok"), "{rendered}");
        assert!(!rendered.contains("  | aaaa"), "oldest must be folded");
        // Newest keeps its body.
        assert!(rendered.contains("  | cccc"), "{rendered}");
        // Order preserved: one, two, three.
        let pos = |s: &str| rendered.find(s).unwrap_or_else(|| panic!("missing {s}"));
        assert!(pos("$ one") < pos("$ two"));
        assert!(pos("$ two") < pos("$ three"));
    }

    #[test]
    fn single_oversized_entry_stays_raw() {
        let mut t = Transcript::new(10);
        t.push(record("big", &"x".repeat(500), "", Some(0)));
        assert!(t.render().contains("  | xxx"), "floor of one raw entry");
    }

    #[test]
    fn long_diagnostic_lines_are_cut() {
        let r = record("c", "", &format!("error: {}", "y".repeat(400)), Some(1));
        let d = r.digest();
        assert!(
            d.chars().count() < 400,
            "digest must stay one line: {}",
            d.len()
        );
        assert!(d.ends_with('…'), "{d}");
    }
}
