//! Post-patch formatters (Phase 3 runtime surface filtering).
//!
//! After a patch lands, files get one pass with the formatter the project
//! actually has - never a tool the machine lacks (a missing formatter is
//! reported, not faked). Formatters are cheap and bounded, so they run
//! directly; heavy per-stack verification stays in the governed loop.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatOutcome {
    Formatted(&'static str),
    /// No formatter exists for this file type (normal for .css/.html).
    NoFormatter,
    /// A formatter was found but is not installed / refused to run.
    Unavailable(String),
    Failed(String),
}

/// Run formatters over the given files. `cwd` supplies project context
/// (rust edition, node_modules). Returns (relative label, outcome) pairs
/// in input order.
#[must_use]
pub fn format_files(root: &Path, files: &[PathBuf]) -> Vec<(String, FormatOutcome)> {
    files
        .iter()
        .map(|rel| {
            let label = rel.display().to_string();
            let outcome = format_one(root, rel);
            (label, outcome)
        })
        .collect()
}

fn format_one(root: &Path, rel: &Path) -> FormatOutcome {
    let ext = rel
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let full = root.join(rel);
    match ext.as_str() {
        "rs" => {
            let edition = rust_edition(root);
            run("rustfmt", &[&format!("--edition={edition}"), "--"], &full)
        }
        "go" => run("gofmt", &["-w"], &full),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "json" | "css" | "scss" | "html" | "md"
        | "yaml" | "yml" => {
            let prettier = root.join("node_modules/.bin/prettier");
            if prettier.exists() {
                run_os(&prettier, "prettier", &["--write"], &full)
            } else if which("prettier") {
                run("prettier", &["--write"], &full)
            } else {
                FormatOutcome::NoFormatter
            }
        }
        _ => FormatOutcome::NoFormatter,
    }
}

fn run(program: &'static str, args: &[&str], file: &Path) -> FormatOutcome {
    if !which(program) {
        return FormatOutcome::Unavailable(program.to_owned());
    }
    run_os(Path::new(program), program, args, file)
}

fn run_os(program: &Path, label: &'static str, args: &[&str], file: &Path) -> FormatOutcome {
    let mut cmd = Command::new(program);
    cmd.args(args).arg(file);
    match cmd.output() {
        Ok(out) if out.status.success() => FormatOutcome::Formatted(label),
        Ok(out) => FormatOutcome::Failed(
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(200)
                .collect(),
        ),
        Err(e) => FormatOutcome::Unavailable(format!("{}: {e}", program.display())),
    }
}

/// `command -v` equivalent via PATH, no shell.
#[must_use]
pub fn which(program: &str) -> bool {
    if Path::new(program).is_file() {
        return true;
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Edition for `rustfmt`: nearest workspace Cargo.toml, default 2024.
fn rust_edition(root: &Path) -> &'static str {
    const EDITIONS: [&str; 4] = ["2015", "2018", "2021", "2024"];
    let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return "2024";
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("edition")
            && let Some(v) = rest.split('=').nth(1)
        {
            let v = v.trim().trim_matches('"');
            if let Some(e) = EDITIONS.iter().find(|e| **e == v) {
                return e;
            }
        }
    }
    "2024"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustfmt_runs_and_reformats() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname=\"t\"\nedition=\"2021\"\n",
        )
        .unwrap();
        let messy = "fn  main( )  {\nlet x=1 ;\n}\n";
        std::fs::write(dir.path().join("a.rs"), messy).unwrap();
        let out = format_files(dir.path(), &[PathBuf::from("a.rs")]);
        assert_eq!(out.len(), 1);
        assert!(
            matches!(out[0].1, FormatOutcome::Formatted("rustfmt")),
            "{:?}",
            out[0]
        );
        let now = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert!(now.contains("fn main() {"), "{now}");
        assert_ne!(now, messy);
    }

    #[test]
    fn unknown_types_report_no_formatter() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), "x").unwrap();
        let out = format_files(dir.path(), &[PathBuf::from("data.bin")]);
        assert_eq!(out[0].1, FormatOutcome::NoFormatter);
    }

    #[test]
    fn broken_rust_reports_failure_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.rs"), "fn main() { ((( }").unwrap();
        let out = format_files(dir.path(), &[PathBuf::from("bad.rs")]);
        assert!(
            matches!(
                out[0].1,
                FormatOutcome::Failed(_) | FormatOutcome::Unavailable(_)
            ),
            "{:?}",
            out[0]
        );
    }

    #[test]
    fn edition_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "edition = \"2018\"\n").unwrap();
        assert_eq!(rust_edition(dir.path()), "2018");
        std::fs::write(dir.path().join("Cargo.toml"), "name = \"x\"\n").unwrap();
        assert_eq!(rust_edition(dir.path()), "2024");
        assert_eq!(rust_edition(Path::new("/nonexistent-dir-xyz")), "2024");
    }
}
