//! Skill-gap synthesizer (Phase 3: `skill_bootstrap` authoring).
//!
//! Detection already exists in [`crate::bind`] (gaps: stack languages with
//! no covering skill). This module closes the loop: one call writes a
//! honest starter SKILL.md into the project's `.openchat/skills/<lang>/`,
//! pre-triggered for that language so the next fingerprint pass binds it.
//! The template is a skeleton with real structure, labeled as generated -
//! not a scraped-docs paste, and the user is told to refine it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::fingerprint::Language;

#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error("skill already exists at {0}; refusing to overwrite (edit it instead)")]
    Exists(PathBuf),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The starter body for a language, chosen for what actually differs
/// between stacks; deliberately short so it costs ~150 prompt tokens
/// until the owner extends it.
fn section_for(lang: Language) -> (&'static str, &'static str, Vec<&'static str>) {
    (
        match lang {
            Language::Rust => "Rust",
            Language::TypeScript => "TypeScript",
            Language::JavaScript => "JavaScript",
            Language::Python => "Python",
            Language::Go => "Go",
            Language::Zig => "Zig",
            Language::Mojo => "Mojo",
            Language::Gleam => "Gleam",
        },
        match lang {
            Language::Rust => "cargo check -q && cargo clippy -D warnings",
            Language::TypeScript | Language::JavaScript => "npx tsc --noEmit",
            Language::Python => "python -m compileall -q . && ruff check .",
            Language::Go => "go vet ./...",
            Language::Zig => "zig build",
            Language::Mojo => "mojo test .",
            Language::Gleam => "gleam check",
        },
        match lang {
            Language::Rust => vec!["rust-toolchain.toml", "Cargo.toml"],
            Language::TypeScript | Language::JavaScript => {
                vec!["tsconfig.json", "package.json"]
            }
            Language::Python => vec!["pyproject.toml", "requirements.txt"],
            Language::Go => vec!["go.mod"],
            Language::Zig => vec!["build.zig", "build.zig.zon"],
            Language::Mojo => vec!["*.mojo"],
            Language::Gleam => vec!["gleam.toml"],
        },
    )
}

/// Render the starter SKILL.md content.
#[must_use]
pub fn template(lang: Language) -> String {
    let (label, check, manifests) = section_for(lang);
    let mut out = String::new();
    let _ = writeln!(out, "---");
    let _ = writeln!(
        out,
        "name: {label} Patterns\ndescription: Working conventions for {label} in this project (skill_bootstrap starter).\nlanguages: [{}]\nfile_patterns: [{}]\n---",
        lang.as_str(),
        manifests
            .iter()
            .map(|m| format!("*{m}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(out, "\n# {label}\n");
    let _ = writeln!(
        out,
        "Generated skeleton - refine this file; it is yours to edit.\n"
    );
    let _ = writeln!(out, "\n## Verify\n```bash\n{check}\n```\n");
    let _ = writeln!(
        out,
        "\n## Conventions\n- Follow existing project style; read a sibling file before writing.\n- Small, surgical changes; no whole-file rewrites.\n- (add project-specific rules here as you learn them)"
    );
    out
}

/// Write the starter skill into `<root>/.openchat/skills/<lang>/SKILL.md`.
///
/// # Errors
///
/// [`BootstrapError::Exists`] when a skill is already authored there
/// (never overwrite user work), or I/O failures.
pub fn bootstrap(root: &Path, lang: Language) -> Result<PathBuf, BootstrapError> {
    let dir = root.join(".openchat").join("skills").join(lang.as_str());
    let file = dir.join("SKILL.md");
    if file.exists() {
        return Err(BootstrapError::Exists(file));
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&file, template(lang))?;
    tracing::info!(path = %file.display(), "skill bootstrapped");
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bind, fingerprint, load_roots};

    #[test]
    fn template_is_honest_and_valid() {
        let t = template(Language::Zig);
        assert!(t.contains("Generated skeleton"), "provenance stated");
        assert!(t.contains("languages: [zig]"));
        assert!(t.contains("build.zig"));
        assert!(
            !t.contains("seamless") && !t.contains("cutting-edge"),
            "no AI-tell prose"
        );
    }

    #[test]
    fn bootstrapped_skill_binds_next_time() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = bootstrap(root, Language::Zig).unwrap();
        assert!(file.exists());
        // Same flow as the daemon: fingerprint -> load roots -> bind.
        let skills = load_roots(&[root.join(".openchat").join("skills")]);
        let fp = fingerprint::Fingerprint {
            languages: vec![Language::Zig],
            manifests: vec![PathBuf::from("build.zig")],
            counts: std::collections::BTreeMap::new(),
            files_scanned: 1,
        };
        let (bindings, gaps) = bind::bind(&fp, &skills);
        assert!(bindings.iter().any(|b| b.skill.id == "zig"), "{bindings:?}");
        assert!(gaps.is_empty(), "gap filled by bootstrap: {gaps:?}");
    }

    #[test]
    fn never_overwrites_user_work() {
        let dir = tempfile::tempdir().unwrap();
        bootstrap(dir.path(), Language::Go).unwrap();
        let err = bootstrap(dir.path(), Language::Go).unwrap_err();
        assert!(matches!(err, BootstrapError::Exists(_)), "{err}");
        // User edits survive: content untouched on second attempt.
        let kept =
            std::fs::read_to_string(dir.path().join(".openchat/skills/go/SKILL.md")).unwrap();
        assert!(kept.contains("Generated skeleton"));
    }
}
