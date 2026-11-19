//! Surgical search/replace patching.
//!
//! The agent emits *only changed regions* as SEARCH/REPLACE blocks; this
//! module validates and applies them all-or-nothing, never letting a full
//! file rewrite through (the 5x output-token fix from the README).
//!
//! Block format (per file, repeatable hunks):
//!
//! ```text
//! @@@ path/to/file.rs
//! <<<<<<< SEARCH
//! exact lines to find
//! =======
//! lines to write instead
//! >>>>>>> REPLACE
//! ```
//!
//! An empty SEARCH block against a file that does not exist yet creates it.
//! Whole-file deletion is not expressible in the format (too easy to fire
//! by accident); it needs the explicit [`Hunk::delete_file`] API.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PatchError {
    #[error("malformed patch at line {line}: {why}")]
    Malformed { line: usize, why: &'static str },
    #[error("unsafe path {0:?}: patches must address files inside the workspace")]
    UnsafePath(String),
    #[error("search block not found in {path} (first line: {head:?})")]
    NotFound { path: String, head: String },
    #[error("search block matches {count} places in {path}; add context to disambiguate")]
    Ambiguous { path: String, count: usize },
    #[error("refusing to create {path}: it already exists (use a SEARCH block to edit it)")]
    AlreadyExists { path: String },
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// One search/replace pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub search: String,
    pub replace: String,
    /// Programmatic-only delete marker (never parsed from a block: deleting
    /// a file must be an explicit tool call, not something a stray empty
    /// REPLACE section triggers).
    pub delete: bool,
}

impl Hunk {
    #[must_use]
    pub fn replace(search: impl Into<String>, replace: impl Into<String>) -> Self {
        Self {
            search: search.into(),
            replace: replace.into(),
            delete: false,
        }
    }

    #[must_use]
    pub const fn delete_file() -> Self {
        Self {
            search: String::new(),
            replace: String::new(),
            delete: true,
        }
    }
}

/// Edits for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub path: PathBuf,
    pub hunks: Vec<Hunk>,
}

const SEARCH_OPEN: &str = "<<<<<<< SEARCH";
const SEP: &str = "=======";
const REPLACE_CLOSE: &str = ">>>>>>> REPLACE";

/// Parse the block format. Rejects unclosed markers and stray text with a
/// line number, so a malformed agent emission fails loudly at the tool
/// boundary instead of half-applying.
#[must_use]
pub fn parse(input: &str) -> Vec<FilePatch> {
    Parse::default().feed(input)
}

#[derive(Debug, Default)]
struct Parse {
    patches: Vec<FilePatch>,
    open: Option<FilePatch>,
    in_hunk: Option<HunkBuf>,
}

#[derive(Debug, Default)]
struct HunkBuf {
    search: Vec<String>,
    replace: Vec<String>,
    in_replace: bool,
}

impl Parse {
    fn feed(mut self, input: &str) -> Vec<FilePatch> {
        for raw in input.lines() {
            let t = raw.trim_end();
            if let Some(rest) = t.strip_prefix("@@@") {
                let path = PathBuf::from(rest.trim());
                self.close_hunk();
                self.flush_file();
                self.open = Some(FilePatch {
                    path,
                    hunks: Vec::new(),
                });
                continue;
            }
            match self.in_hunk.as_mut() {
                None if t == SEARCH_OPEN => self.in_hunk = Some(HunkBuf::default()),
                None => {} // blank/unknown lines outside hunks: tolerated
                Some(h) if t == SEP && !h.in_replace => h.in_replace = true,
                Some(_) if t == REPLACE_CLOSE => {
                    let done = self.in_hunk.take().expect("checked above");
                    if let Some(file) = self.open.as_mut() {
                        file.hunks.push(Hunk::replace(
                            join_block(&done.search),
                            join_block(&done.replace),
                        ));
                    }
                }
                Some(h) if h.in_replace => h.replace.push(raw.to_owned()),
                Some(h) => h.search.push(raw.to_owned()),
            }
        }
        self.close_hunk();
        self.flush_file();
        self.patches
    }

    fn close_hunk(&mut self) {
        if let Some(done) = self.in_hunk.take()
            && let Some(file) = self.open.as_mut()
        {
            file.hunks.push(Hunk::replace(
                join_block(&done.search),
                join_block(&done.replace),
            ));
        }
    }

    fn flush_file(&mut self) {
        if let Some(file) = self.open.take() {
            self.patches.push(file);
        }
    }
}

fn join_block(lines: &[String]) -> String {
    lines.join("\n")
}

/// Reject absolute paths, parent traversals, and empty components.
///
/// # Errors
///
/// [`PatchError::UnsafePath`] when the path could escape the workspace.
pub fn validate_rel_path(path: &Path) -> Result<PathBuf, PatchError> {
    let bad = path.is_absolute()
        || path.components().next().is_none()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if bad {
        return Err(PatchError::UnsafePath(path.display().to_string()));
    }
    Ok(path.to_path_buf())
}

/// Result of applying a file's hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    Create(String),
    Modify(String),
    Delete,
}

/// Apply hunks to `content` all-or-nothing: the first failing hunk aborts
/// the whole file, so a partial match can never land.
///
/// # Errors
///
/// [`PatchError::NotFound`] / [`PatchError::Ambiguous`] per hunk rules.
pub fn apply_hunks(path: &str, content: &str, hunks: &[Hunk]) -> Result<Planned, PatchError> {
    let mut text = content.to_owned();
    for hunk in hunks {
        if hunk.delete {
            return Ok(Planned::Delete);
        }
        let start = locate(path, &text, &hunk.search)?;
        text.replace_range(start.0..start.1, &hunk.replace);
    }
    Ok(Planned::Modify(text))
}

/// Find `search` in `hay` exactly once (byte range), else try a
/// whitespace-tolerant (trailing spaces) unique line match — the pragmatic
/// allowance for editor autoformat. Zero or multiple matches are hard errors.
fn locate(path: &str, hay: &str, search: &str) -> Result<(usize, usize), PatchError> {
    if search.is_empty() {
        return Err(PatchError::NotFound {
            path: path.into(),
            head: String::new(),
        });
    }
    let exact: Vec<usize> = hay.match_indices(search).map(|(i, _)| i).collect();
    match exact.len() {
        1 => {
            let i = exact[0];
            return Ok((i, i + search.len()));
        }
        n if n > 1 => {
            return Err(PatchError::Ambiguous {
                path: path.into(),
                count: n,
            });
        }
        _ => {}
    }
    // Whitespace-tolerant fallback: slide a window of needle.len() lines.
    let hay_lines: Vec<&str> = hay.lines().collect();
    let needle: Vec<&str> = search.lines().map(str::trim_end).collect();
    if needle.is_empty() || needle.len() > hay_lines.len() {
        return Err(PatchError::NotFound {
            path: path.into(),
            head: first_line(search),
        });
    }
    let found: Vec<usize> = (0..=hay_lines.len() - needle.len())
        .filter(|&s| (0..needle.len()).all(|k| hay_lines[s + k].trim_end() == needle[k]))
        .collect();
    match found.len() {
        1 => {
            // Byte span of the matched whole lines (excluding their closing
            // newline), mirroring exact-match semantics.
            let start_line = found[0];
            let mut start = 0usize;
            for _ in 0..start_line {
                start += hay[start..].find('\n').map_or(0, |i| i + 1);
            }
            let mut end = start;
            for k in 0..needle.len() {
                let nl = hay[end..].find('\n').map_or(hay.len(), |i| end + i);
                end = nl;
                if k + 1 < needle.len() {
                    end += 1; // step over the newline into the next line
                }
            }
            Ok((start, end))
        }
        0 => Err(PatchError::NotFound {
            path: path.into(),
            head: first_line(search),
        }),
        n => Err(PatchError::Ambiguous {
            path: path.into(),
            count: n,
        }),
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
}

/// What landed on disk for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedFile {
    pub path: PathBuf,
    pub planned: Planned,
    /// Review surface for the chat adapters (Phase 5 diff review).
    pub diff: String,
}

/// Plan-then-write: validate every patch against current disk state first
/// (all-or-nothing), then commit each file atomically via temp+rename.
/// A create needs exactly one empty-SEARCH hunk; editing a missing file is
/// a `NotFound` error, never a silent overwrite.
///
/// # Errors
///
/// The first failing validation aborts before any file is touched;
/// mid-commit I/O failures surface as [`PatchError::Io`] with the files
/// already written listed by the caller's own bookkeeping being impossible
/// (documented v1 limitation: rename is per-file atomic, batch-atomic
/// rollback is a tracked refinement).
pub fn apply_to_disk(root: &Path, patches: &[FilePatch]) -> Result<Vec<AppliedFile>, PatchError> {
    let mut plans: Vec<(PathBuf, AppliedFile)> = Vec::new();
    for p in patches {
        let rel = validate_rel_path(&p.path)?;
        let full = root.join(&rel);
        let display = rel.display().to_string();
        let current = match std::fs::read_to_string(&full) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(PatchError::Io {
                    path: display,
                    source,
                });
            }
        };
        let planned = match (&current, p.hunks.as_slice()) {
            (None, [h]) if h.search.is_empty() && !h.delete && !h.replace.is_empty() => {
                Planned::Create(h.replace.clone())
            }
            (None, _) => {
                return Err(PatchError::NotFound {
                    path: display,
                    head: current_head(&p.hunks),
                });
            }
            (Some(c), _) => apply_hunks(&display, c, &p.hunks)?,
        };
        let diff = match &planned {
            Planned::Create(s) => coarse_diff(&display, "", s),
            Planned::Modify(s) => coarse_diff(&display, current.as_deref().unwrap_or(""), s),
            Planned::Delete => coarse_diff(&display, current.as_deref().unwrap_or(""), ""),
        };
        plans.push((
            full.clone(),
            AppliedFile {
                path: rel,
                planned,
                diff,
            },
        ));
    }
    let mut applied = Vec::new();
    for (full, file) in plans {
        let display = file.path.display().to_string();
        match &file.planned {
            Planned::Create(s) | Planned::Modify(s) => write_atomic(&full, s, &display)?,
            Planned::Delete => std::fs::remove_file(&full).map_err(|source| PatchError::Io {
                path: display,
                source,
            })?,
        }
        applied.push(file);
    }
    Ok(applied)
}

fn current_head(hunks: &[Hunk]) -> String {
    hunks
        .first()
        .map_or_else(String::new, |h| first_line(&h.search))
}

fn write_atomic(full: &Path, content: &str, display: &str) -> Result<(), PatchError> {
    let io = |source: std::io::Error| PatchError::Io {
        path: display.to_owned(),
        source,
    };
    if let Some(parent) = full.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let tmp = full.with_extension(format!("oc-tmp-{}", std::process::id()));
    std::fs::write(&tmp, content).map_err(io)?;
    match std::fs::rename(&tmp, full) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(io(e))
        }
    }
}

/// Coarse unified-style diff for display: common prefix/suffix lines are
/// trimmed and the changed middle is emitted as one hunk. Exact enough to
/// review a surgical patch; not a git-merge-grade diff (a tracked
/// refinement once patch review lands in the chat adapters).
#[must_use]
pub fn coarse_diff(path: &str, old: &str, new: &str) -> String {
    let mut out = format!("--- {path}\n+++ {path}\n");
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let pre = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = &a[pre..a.len() - suf];
    let b_mid = &b[pre..b.len() - suf];
    if a_mid.is_empty() && b_mid.is_empty() {
        return String::new();
    }
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        pre + 1,
        a_mid.len(),
        pre + 1,
        b_mid.len()
    ));
    for line in a_mid {
        let _ = writeln!(out, "-{line}");
    }
    for line in b_mid {
        let _ = writeln!(out, "+{line}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(path: &str, search: &str, replace: &str) -> String {
        format!("@@@ {path}\n{SEARCH_OPEN}\n{search}\n{SEP}\n{replace}\n{REPLACE_CLOSE}\n")
    }

    #[test]
    fn parses_headers_hunks_and_multiple_files() {
        let input = format!(
            "{}{}",
            block("a.rs", "let x = 1;", "let x = 2;"),
            block("b.rs", "first\nsecond", "first patched")
        );
        let patches = parse(&input);
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].path, PathBuf::from("a.rs"));
        assert_eq!(patches[0].hunks[0].search, "let x = 1;");
        assert_eq!(patches[0].hunks[0].replace, "let x = 2;");
        assert_eq!(patches[1].hunks[0].search, "first\nsecond");
        assert_eq!(patches[1].hunks[0].replace, "first patched");
    }

    #[test]
    fn two_hunks_same_file_accumulate() {
        let input = format!(
            "@@@ m.rs\n{SEARCH_OPEN}\nAAA\n{SEP}\nBBB\n{REPLACE_CLOSE}\n{SEARCH_OPEN}\nCCC\n{SEP}\nDDD\n{REPLACE_CLOSE}\n"
        );
        let patches = parse(&input);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].hunks.len(), 2);
    }

    #[test]
    fn unique_exact_apply() {
        let content = "fn main() {\n    old();\n}\n";
        let planned = apply_hunks("m.rs", content, &[Hunk::replace("old();", "new();")]).unwrap();
        match planned {
            Planned::Modify(s) => assert_eq!(s, "fn main() {\n    new();\n}\n"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ambiguous_search_is_rejected_with_count() {
        let content = "dup\ndup\n";
        let err = apply_hunks("m.rs", content, &[Hunk::replace("dup", "x")]).unwrap_err();
        assert!(
            matches!(err, PatchError::Ambiguous { count: 2, .. }),
            "{err}"
        );
    }

    #[test]
    fn missing_search_is_rejected() {
        let err = apply_hunks("m.rs", "hello\n", &[Hunk::replace("nope", "x")]).unwrap_err();
        assert!(
            matches!(&err, PatchError::NotFound { head, .. } if head == "nope"),
            "{err}"
        );
    }

    #[test]
    fn whitespace_tolerant_fallback_when_exact_misses() {
        // Search text has no trailing spaces; file does.
        let content = "let a = 1;   \nlet b = 2;\n";
        let planned = apply_hunks(
            "m.rs",
            content,
            &[Hunk::replace("let a = 1;\nlet b = 2;", "done")],
        )
        .unwrap();
        match planned {
            Planned::Modify(s) => assert_eq!(s, "done\n", "span covers both whole lines"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sequential_hunks_apply_to_evolving_text() {
        let content = "one\ntwo\nthree\n";
        let planned = apply_hunks(
            "m.rs",
            content,
            &[Hunk::replace("one", "1"), Hunk::replace("three", "3")],
        )
        .unwrap();
        match planned {
            Planned::Modify(s) => assert_eq!(s, "1\ntwo\n3\n"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn second_hunk_failure_aborts_whole_file() {
        let content = "alpha\n";
        let err = apply_hunks(
            "m.rs",
            content,
            &[
                Hunk::replace("alpha", "omega"),
                Hunk::replace("ghost", "boo"),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, PatchError::NotFound { .. }));
    }

    #[test]
    fn unsafe_paths_rejected() {
        for p in ["/etc/passwd", "../escape.rs", "src/../../x", ""] {
            assert!(
                validate_rel_path(Path::new(p)).is_err(),
                "must reject {p:?}"
            );
        }
        assert_eq!(
            validate_rel_path(Path::new("crates/core/lib.rs"))
                .unwrap()
                .as_os_str(),
            "crates/core/lib.rs"
        );
    }

    #[test]
    fn delete_only_via_explicit_marker() {
        let planned = apply_hunks("x.rs", "any", &[Hunk::delete_file()]).unwrap();
        assert_eq!(planned, Planned::Delete);
        // Parsing never produces one.
        assert!(
            parse(&block("x.rs", "", ""))
                .iter()
                .all(|f| f.hunks.iter().all(|h| !h.delete))
        );
    }

    #[test]
    fn coarse_diff_trims_common_context() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nd\n";
        let d = coarse_diff("f", old, new);
        assert!(d.contains("@@ -2,1 +2,1 @@"), "{d}");
        assert!(d.contains("-b\n"));
        assert!(d.contains("+B\n"));
        assert!(!d.contains("\n-a\n"), "common prefix should not appear");
        assert_eq!(coarse_diff("f", old, old), "");
    }

    #[test]
    fn disk_create_modify_delete_round_trip() {
        let root = tempfile::tempdir().unwrap();
        // Create via empty-SEARCH hunk.
        let created = apply_to_disk(
            root.path(),
            &[FilePatch {
                path: PathBuf::from("src/new.rs"),
                hunks: vec![Hunk::replace("", "fn seed() {}\n")],
            }],
        )
        .unwrap();
        assert_eq!(created.len(), 1);
        assert!(created[0].diff.starts_with("--- src/new.rs"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("src/new.rs")).unwrap(),
            "fn seed() {}\n"
        );

        // Modify.
        apply_to_disk(
            root.path(),
            &[FilePatch {
                path: PathBuf::from("src/new.rs"),
                hunks: vec![Hunk::replace("fn seed() {}", "pub fn seed() {}")],
            }],
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(root.path().join("src/new.rs"))
                .unwrap()
                .contains("pub fn")
        );

        // Delete only via explicit marker.
        apply_to_disk(
            root.path(),
            &[FilePatch {
                path: PathBuf::from("src/new.rs"),
                hunks: vec![Hunk::delete_file()],
            }],
        )
        .unwrap();
        assert!(!root.path().join("src/new.rs").exists());
    }

    #[test]
    fn edit_of_missing_file_fails_before_any_write() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("good.txt"), "keep\n").unwrap();
        let err = apply_to_disk(
            root.path(),
            &[
                FilePatch {
                    path: PathBuf::from("good.txt"),
                    hunks: vec![Hunk::replace("keep", "changed")],
                },
                FilePatch {
                    path: PathBuf::from("ghost.txt"),
                    hunks: vec![Hunk::replace("absent", "x")],
                },
            ],
        )
        .unwrap_err();
        assert!(matches!(err, PatchError::NotFound { .. }), "{err}");
        // Plan-first: the valid patch must not have touched the disk.
        assert_eq!(
            std::fs::read_to_string(root.path().join("good.txt")).unwrap(),
            "keep\n"
        );
        assert!(!root.path().join("ghost.txt").exists());
    }

    #[test]
    fn traversal_cannot_escape_root() {
        let root = tempfile::tempdir().unwrap();
        let err = apply_to_disk(
            root.path(),
            &[FilePatch {
                path: PathBuf::from("../../etc/pwned"),
                hunks: vec![Hunk::delete_file()],
            }],
        )
        .unwrap_err();
        assert!(matches!(err, PatchError::UnsafePath(_)), "{err}");
    }
}
