//! Deterministic stack fingerprinting (Phase 3 item 1).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Languages with manifest or extension detection. The list is a policy
/// point: adding one here lights up gap detection for it everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Zig,
    Mojo,
    Gleam,
}

impl Language {
    pub const ALL: &[Language] = &[
        Language::Rust,
        Language::TypeScript,
        Language::JavaScript,
        Language::Python,
        Language::Go,
        Language::Zig,
        Language::Mojo,
        Language::Gleam,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Zig => "zig",
            Self::Mojo => "mojo",
            Self::Gleam => "gleam",
        }
    }

    /// Extension/manifest token -> language, used by both the scanner and
    /// the SKILL.md text heuristic.
    #[must_use]
    pub const fn tokens(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["rust", "cargo", ".rs"],
            Self::TypeScript => &["typescript", "tsx", ".ts", "react", "vite"],
            Self::JavaScript => &["javascript", "node.js", "npm", ".js"],
            Self::Python => &["python", "pytest", "pip", ".py"],
            Self::Go => &["golang", "go.mod", ".go"],
            Self::Zig => &["zig", "build.zig", "ziglang"],
            Self::Mojo => &["mojo", "modular", ".mojo"],
            Self::Gleam => &["gleam", "gleam.toml"],
        }
    }

    /// Whole-word token match in a lowercased string ("go" must not match
    /// inside "cargo").
    #[must_use]
    pub fn matches_text(self, lower: &str) -> bool {
        let is_word_char = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
        self.tokens().iter().any(|tok| {
            lower.match_indices(tok).any(|(i, m)| {
                !lower[..i].ends_with(is_word_char)
                    && !lower[i + m.len()..].starts_with(is_word_char)
            })
        })
    }
}

/// The detected stack of one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub languages: Vec<Language>,
    /// Manifest files found, relative to root.
    pub manifests: Vec<PathBuf>,
    /// Extension -> file count (only tracked extensions).
    pub counts: BTreeMap<&'static str, usize>,
    pub files_scanned: usize,
}

impl Fingerprint {
    /// Comma list for the project-memory `stack` key ("rust,typescript").
    #[must_use]
    pub fn stack_tag(&self) -> String {
        self.languages
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn manifest_language(name: &str) -> Option<Language> {
    Some(match name {
        "Cargo.toml" => Language::Rust,
        "go.mod" | "go.sum" => Language::Go,
        "pyproject.toml" | "setup.py" | "requirements.txt" => Language::Python,
        "build.zig" | "build.zig.zon" => Language::Zig,
        "gleam.toml" => Language::Gleam,
        _ => return None,
    })
}

fn tracked_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rs",
        "ts" => "ts",
        "tsx" => "tsx",
        "js" => "js",
        "jsx" => "jsx",
        "mjs" => "mjs",
        "cjs" => "cjs",
        "py" => "py",
        "go" => "go",
        "zig" => "zig",
        "mojo" => "mojo",
        "gleam" => "gleam",
        _ => return None,
    })
}

/// Directories that never carry signal (build output, vendored deps) and
/// could dominate the scan budget.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
];

/// Scan `root` for stack evidence. Bounded walk: depth and file-count caps
/// keep even a huge home directory cheap (this runs on watcher events).
#[must_use]
pub fn fingerprint(root: &Path) -> Fingerprint {
    fingerprint_bounded(root, 8, 20_000)
}

#[must_use]
pub fn fingerprint_bounded(root: &Path, max_depth: usize, max_files: usize) -> Fingerprint {
    let mut langs = std::collections::BTreeSet::new();
    let mut manifests = Vec::new();
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut scanned = 0usize;

    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > max_depth || scanned >= max_files {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if scanned >= max_files {
                break;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                let hidden = name.starts_with('.') && depth != 0;
                if SKIP_DIRS.contains(&name) || hidden {
                    continue;
                }
                stack.push((path, depth + 1));
                continue;
            }
            scanned += 1;
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if let Some(lang) = manifest_language(name) {
                langs.insert(lang);
                manifests.push(rel);
                continue;
            }
            if name == "package.json" {
                // Real stack evidence, but JS-vs-TS stays extension-driven.
                manifests.push(rel);
            }
            if let Some(ext) = path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(tracked_ext)
            {
                *counts.entry(ext).or_default() += 1;
            }
        }
    }

    // Extension evidence adds languages; package.json alone is ambiguous,
    // so JS vs TS comes purely from extensions.
    let n = |ext: &str| counts.get(ext).copied().unwrap_or(0);
    if n("rs") > 0 {
        langs.insert(Language::Rust);
    }
    if n("ts") + n("tsx") > 0 {
        langs.insert(Language::TypeScript);
    }
    if n("js") + n("jsx") + n("mjs") + n("cjs") > 0 {
        langs.insert(Language::JavaScript);
    }
    if n("py") > 0 {
        langs.insert(Language::Python);
    }
    if n("go") > 0 {
        langs.insert(Language::Go);
    }
    if n("zig") > 0 {
        langs.insert(Language::Zig);
    }
    if n("mojo") > 0 {
        langs.insert(Language::Mojo);
    }
    if n("gleam") > 0 {
        langs.insert(Language::Gleam);
    }

    Fingerprint {
        languages: langs.into_iter().collect(),
        manifests,
        counts,
        files_scanned: scanned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "x\n").unwrap();
    }

    #[test]
    fn detects_mixed_stack_from_manifests_and_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "Cargo.toml");
        touch(root, "src/main.rs");
        touch(root, "ui/package.json");
        touch(root, "ui/app.tsx");
        touch(root, "tools/util.py");
        let fp = fingerprint(root);
        assert_eq!(
            fp.languages,
            vec![Language::Rust, Language::TypeScript, Language::Python],
            "BTreeSet order = declaration order"
        );
        assert_eq!(
            fp.stack_tag(),
            "rust,typescript,python",
            "no .js file -> no JavaScript"
        );
        assert!(fp.manifests.contains(&PathBuf::from("Cargo.toml")));
        assert!(fp.manifests.contains(&PathBuf::from("ui/package.json")));
    }

    #[test]
    fn package_json_alone_is_still_ambiguous_but_lists_as_manifest() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "package.json");
        let fp = fingerprint(dir.path());
        assert!(fp.languages.is_empty(), "no extension evidence yet");
        assert_eq!(fp.manifests, vec![PathBuf::from("package.json")]);
    }

    #[test]
    fn skips_heavy_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "src/lib.rs");
        touch(root, "target/debug/generated.rs");
        touch(root, "node_modules/pkg/index.d.ts");
        touch(root, ".git/hooks/commit.go");
        let fp = fingerprint(root);
        assert_eq!(fp.languages, vec![Language::Rust]);
        assert_eq!(fp.counts.get("rs"), Some(&1), "target/ excluded");
    }

    #[test]
    fn bounded_walk_respects_file_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            touch(dir.path(), &format!("deep/dir{i}/f.rs"));
        }
        let fp = fingerprint_bounded(dir.path(), 8, 10);
        assert!(fp.files_scanned <= 10, "{}", fp.files_scanned);
    }

    #[test]
    fn language_token_match_is_word_bounded() {
        let lower = "idiomatic rust with cargo and .rs files";
        assert!(Language::Rust.matches_text(lower));
        assert!(
            !Language::Go.matches_text(lower),
            "\"cargo\" must not match go"
        );
        assert!(Language::Go.matches_text("backend in go, see go.mod"));
        assert!(!Language::Go.matches_text("let's go over the agenda"));
    }
}
