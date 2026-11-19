//! SKILL.md registry: index skill packages from the configured roots
//! (Phase 3 item 2).
//!
//! Layouts supported, matching the real-world catalogs in use:
//!
//! - `<root>/<id>/SKILL.md` (directory skills, front-matter metadata)
//! - `<root>/<id>.md` (single-file skills)
//!
//! Triggers: explicit `languages:`/`tags:`/`file_patterns:` front-matter
//! when present; otherwise a whole-word keyword heuristic over name +
//! description. A skill that hits nothing is treated as universal
//! (anti-slop-style "load always" rules), which is also what the
//! description itself says to do.

use std::fs;
use std::path::{Path, PathBuf};

use crate::fingerprint::Language;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Empty => universal (bound to every stack).
    pub languages: Vec<Language>,
    /// `file_patterns` globs from front-matter, e.g. `*.rs`.
    pub patterns: Vec<String>,
    pub path: PathBuf,
    /// Body size cost in estimated tokens, for the compositor budget.
    pub tokens: usize,
    /// True when triggers came from front-matter, not the heuristic.
    pub explicit_triggers: bool,
    /// Trigger-less but declares itself always-on (see [`ALWAYS_MARKERS`]).
    pub universal: bool,
    /// Match quality: 0 = explicit front-matter or id/name says the
    /// language, 1 = description-only hit, 2 = universal. Prompt-budget
    /// allocation prefers 0 so the stack's core skill is never starved by
    /// tangential description matches.
    pub strength: u8,
}

/// Front-matter/description markers that declare a skill stack-agnostic.
/// Only with one of these does a trigger-less skill bind everywhere;
/// anything else that hits no language stays inactive rather than flooding
/// the prompt with skills for stacks the workspace does not use.
const ALWAYS_MARKERS: &[&str] = &[
    "load always",
    "always load",
    "all languages",
    "every language",
    "any language",
];

impl Skill {
    #[must_use]
    pub fn is_universal(&self) -> bool {
        self.universal
    }
}

/// Load every skill from all roots. Later roots override earlier ones by
/// id (project-local `.openchat/skills` wins over the global catalog).
#[must_use]
pub fn load_roots(roots: &[PathBuf]) -> Vec<Skill> {
    let mut by_id: std::collections::BTreeMap<String, Skill> = std::collections::BTreeMap::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let skill_file = if path.is_dir() {
                path.join("SKILL.md")
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            {
                path.clone()
            } else {
                continue;
            };
            let Ok(text) = fs::read_to_string(&skill_file) else {
                continue;
            };
            let id = if path.is_dir() {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            } else {
                skill_file
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            by_id.insert(id.clone(), parse_skill(&id, &skill_file, &text));
        }
    }
    by_id.into_values().collect()
}

/// Parse one SKILL.md: front-matter first, heuristic fallback.
#[must_use]
pub fn parse_skill(id: &str, path: &Path, text: &str) -> Skill {
    let (meta, body) = split_front_matter(text);
    let name = meta
        .get("name")
        .cloned()
        .unwrap_or_else(|| id.replace(['-', '_'], " "));
    let description = meta.get("description").cloned().unwrap_or_default();

    fn bracketless(t: &str) -> &str {
        t.trim().trim_matches(['[', ']', '"', '\''])
    }
    let langs_raw = meta
        .get("languages")
        .into_iter()
        .chain(meta.get("tags"))
        .flat_map(|v| v.split([',', ' ']))
        .map(bracketless)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();
    let explicit_pats: Vec<String> = meta
        .get("file_patterns")
        .map(|v| {
            v.split([',', ' '])
                .map(bracketless)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let mut explicit_triggers = false;
    let mut languages: Vec<Language> = Vec::new();
    for tok in &langs_raw {
        if let Some(lang) = Language::ALL.iter().copied().find(|l| &l.as_str() == tok) {
            if !languages.contains(&lang) {
                languages.push(lang);
                explicit_triggers = true;
            }
        }
    }
    // `file_patterns` also imply languages (*.rs => Rust).
    for pat in &explicit_pats {
        if let Some(ext) = pat.as_str().rsplit('.').next() {
            if let Some(lang) = ext_lang(ext) {
                if !languages.contains(&lang) {
                    languages.push(lang);
                    explicit_triggers = true;
                }
            }
        }
    }

    let mut strength = if explicit_triggers { 0 } else { 2 };
    if languages.is_empty() {
        // Skill files named after a language are that language, full stop.
        languages = Language::ALL
            .iter()
            .copied()
            .filter(|l| id.eq_ignore_ascii_case(l.as_str()))
            .collect();
        if !languages.is_empty() {
            strength = 0;
        }
    }
    if languages.is_empty() {
        // Strong pass: whole-word hits in id + name only. A language
        // merely *mentioned* in a description is usually a cross-reference
        // ("pair with the rust-testing skill"), not a trigger.
        let strong = format!("{} {}", id.replace(['-', '_'], " "), name).to_ascii_lowercase();
        languages = Language::ALL
            .iter()
            .copied()
            .filter(|l| l.matches_text(&strong))
            .collect();
        if !languages.is_empty() {
            strength = 0;
        }
    }
    if languages.is_empty() {
        // Weak pass: description, used only when the name said nothing.
        let weak = description.to_ascii_lowercase();
        languages = Language::ALL
            .iter()
            .copied()
            .filter(|l| l.matches_text(&weak))
            .collect();
        if !languages.is_empty() {
            strength = 1;
        }
    }

    let haystack = format!("{id} {name} {description}").to_ascii_lowercase();
    let universal = languages.is_empty()
        && explicit_pats.is_empty()
        && ALWAYS_MARKERS.iter().any(|m| haystack.contains(m));
    let final_strength = if languages.is_empty() { 2 } else { strength };

    Skill {
        id: id.to_owned(),
        name,
        description,
        languages,
        patterns: explicit_pats,
        path: path.to_path_buf(),
        tokens: openchat_tokens(body),
        explicit_triggers,
        universal,
        strength: final_strength,
    }
}

fn ext_lang(ext: &str) -> Option<Language> {
    Some(match ext {
        "rs" => Language::Rust,
        "ts" | "tsx" => Language::TypeScript,
        "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
        "py" => Language::Python,
        "go" => Language::Go,
        "zig" => Language::Zig,
        "mojo" => Language::Mojo,
        "gleam" => Language::Gleam,
        _ => return None,
    })
}

/// Token estimate, duplicated tiny-local to keep this crate dependency-
/// light (same 4 bytes/token rule as openchat-core).
fn openchat_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Returns (front-matter map, body). A deliberately flat parser: only
/// top-level `key: value` pairs (inline or `- item` lists joined), enough
/// for trigger metadata without a YAML dependency.
pub(crate) fn split_front_matter(text: &str) -> (std::collections::BTreeMap<String, String>, &str) {
    let mut meta = std::collections::BTreeMap::new();
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (meta, text);
    };
    let Some(end_rel) = find_closing_fence(rest) else {
        return (meta, text);
    };
    let front = &rest[..end_rel.0];
    let body = &rest[end_rel.1..];

    let mut current_list_key: Option<String> = None;
    for line in front.lines() {
        let trimmed = line.trim_end();
        if let Some(item) = trimmed
            .strip_prefix("  - ")
            .or_else(|| trimmed.strip_prefix("- "))
            && let Some(key) = &current_list_key
        {
            let entry = meta.get_mut(key).expect("key exists");
            if !entry.is_empty() {
                entry.push(',');
            }
            entry.push_str(item.trim());
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            current_list_key = None;
            continue;
        };
        let key = key.trim().to_owned();
        let value = value.trim().trim_matches('"').trim_matches('\'').to_owned();
        if value.is_empty() {
            current_list_key = Some(key.clone());
            meta.entry(key).or_default();
        } else {
            current_list_key = None;
            meta.insert(key, value);
        }
    }
    (meta, body)
}

fn find_closing_fence(rest: &str) -> Option<(usize, usize)> {
    let mut idx = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some((idx, idx + line.len()));
        }
        idx += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directory_and_flat_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("rust-idioms")).unwrap();
        fs::write(
            root.join("rust-idioms/SKILL.md"),
            "---\nname: Rust Idiologies\ndescription: idiomatic Rust, cargo, clippy\nlanguages: [rust]\nfile_patterns: [*.rs]\n---\nbody text\n",
        )
        .unwrap();
        fs::write(
            root.join("python.md"),
            "---\nname: Python\ndescription: \"Python coding and debugging\"\n---\nbig body here\n",
        )
        .unwrap();
        let skills = load_roots(&[root.to_path_buf()]);
        assert_eq!(skills.len(), 2);
        let rust = skills.iter().find(|s| s.id == "rust-idioms").unwrap();
        assert_eq!(rust.languages, vec![Language::Rust]);
        assert!(rust.explicit_triggers);
        assert_eq!(rust.patterns, vec!["*.rs"]);
        let py = skills.iter().find(|s| s.id == "python").unwrap();
        assert_eq!(
            py.languages,
            vec![Language::Python],
            "heuristic from description"
        );
        assert!(!py.explicit_triggers);
    }

    #[test]
    fn dash_list_front_matter_supported() {
        let (meta, _) = split_front_matter(
            "---\nname: X\nlanguages:\n  - rust\n  - typescript\ndescription: d\n---\nbody",
        );
        assert_eq!(
            meta.get("languages").map(String::as_str),
            Some("rust,typescript")
        );
        assert_eq!(meta.get("name").map(String::as_str), Some("X"));
    }

    #[test]
    fn no_triggers_is_universal() {
        let s = parse_skill(
            "antislop",
            Path::new("x"),
            "---\nname: antislop\ndescription: Anti Slop rules for AI coding agents. Load always.\n---\nrules\n",
        );
        assert!(s.languages.is_empty());
        assert!(s.is_universal());
        // No always-marker, no language hit -> dormant, not universal.
        let dormant = parse_skill(
            "vue",
            Path::new("y"),
            "---\nname: Vue\ndescription: Vue 3 and Nuxt applications\n---\nx\n",
        );
        assert!(!dormant.is_universal());
        assert!(dormant.languages.is_empty());
    }

    #[test]
    fn description_cross_reference_does_not_bind() {
        let s = parse_skill(
            "py-tests",
            Path::new("z"),
            "---\nname: Python Testing\ndescription: pytest fixtures; for the sibling see the rust-testing skill\n---\nx\n",
        );
        assert_eq!(
            s.languages,
            vec![Language::Python],
            "rust mention is a cross-ref, not a trigger"
        );
    }

    #[test]
    fn file_named_after_language_binds_it() {
        let s = parse_skill(
            "go",
            Path::new("go.md"),
            "---\ndescription: backend services\n---\nx\n",
        );
        assert_eq!(s.languages, vec![Language::Go]);
    }

    #[test]
    fn quoted_multiline_name_and_missing_front_matter() {
        let s = parse_skill("bare", Path::new("x"), "no front matter at all\ncontent");
        assert_eq!(s.name, "bare");
        assert!(s.description.is_empty());
        assert_eq!(s.tokens, openchat_tokens("no front matter at all\ncontent"));
    }

    #[test]
    fn later_roots_override_by_id() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fs::write(a.path().join("shared.md"), "---\nname: A\n---\na a\n").unwrap();
        fs::write(b.path().join("shared.md"), "---\nname: B\n---\nb b b\n").unwrap();
        let skills = load_roots(&[a.path().to_path_buf(), b.path().to_path_buf()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "B");
    }
}
