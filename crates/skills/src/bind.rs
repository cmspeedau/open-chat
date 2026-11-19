//! Binding: fingerprint x registry -> active skills + gaps (Phase 3).

use std::path::Path;

use crate::fingerprint::{Fingerprint, Language};
use crate::registry::Skill;

/// A skill selected for this stack and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding<'a> {
    pub skill: &'a Skill,
    pub reason: String,
}

/// A detected language with no skill coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    pub language: Language,
}

/// Bind `fp` against `skills`:
/// - universal skills always bind (they govern behavior, not a language),
/// - language/pattern overlap binds with the matched evidence,
/// - every detected language left uncovered becomes a [`Gap`] (the
///   skill-gap prompt's input).
#[must_use]
pub fn bind<'a>(fp: &Fingerprint, skills: &'a [Skill]) -> (Vec<Binding<'a>>, Vec<Gap>) {
    let mut bindings = Vec::new();
    let mut covered: Vec<Language> = Vec::new();
    for skill in skills {
        if skill.is_universal() {
            bindings.push(Binding {
                skill,
                reason: "universal: no language triggers, always active".into(),
            });
            continue;
        }
        let mut hits: Vec<String> = Vec::new();
        for lang in &skill.languages {
            if fp.languages.contains(lang) {
                hits.push(lang.as_str().to_owned());
                covered.push(*lang);
            }
        }
        for pat in &skill.patterns {
            // v1 globs are `*.ext`; anything else is stored but unmatched.
            if let Some(ext) = pat.strip_prefix("*.")
                && fp.counts.keys().any(|k| *k == ext)
            {
                hits.push(pat.clone());
            }
        }
        if !hits.is_empty() {
            bindings.push(Binding {
                skill,
                reason: format!("matches {}", hits.join(", ")),
            });
        }
    }
    let gaps = fp
        .languages
        .iter()
        .filter(|l| !covered.contains(l))
        .cloned()
        .map(|language| Gap { language })
        .collect();
    (bindings, gaps)
}

/// Watcher predicate: does a debounced path change invalidate the current
/// fingerprint? Manifest files and tracked extensions say yes, everything
/// else (markdown edits, logs) does not, so the bus stays quiet for
/// ordinary editing.
#[must_use]
pub fn affects_fingerprint(path: &Path) -> bool {
    const MANIFESTS: &[&str] = &[
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        "requirements.txt",
        "package.json",
        "build.zig",
        "build.zig.zon",
        "gleam.toml",
    ];
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if MANIFESTS.contains(&name) {
        return true;
    }
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        Language::ALL
            .iter()
            .any(|l| l.tokens().iter().any(|t| t == &format!(".{e}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::parse_skill;
    use std::path::PathBuf;

    fn fp(langs: &[Language]) -> Fingerprint {
        let mut counts = std::collections::BTreeMap::new();
        for l in langs {
            let ext = match l {
                Language::Rust => "rs",
                Language::Python => "py",
                Language::Zig => "zig",
                _ => "x",
            };
            counts.insert(ext, 3usize);
        }
        Fingerprint {
            languages: langs.to_vec(),
            manifests: vec![PathBuf::from("Cargo.toml")],
            counts,
            files_scanned: 42,
        }
    }

    fn skill(id: &str, front: &str) -> Skill {
        parse_skill(id, Path::new(id), &format!("---\n{front}\n---\nbody\n"))
    }

    #[test]
    fn binds_universal_plus_language_and_reports_gaps() {
        let skills = vec![
            skill(
                "antislop",
                "name: antislop\ndescription: rules for agents, load always",
            ),
            skill("rust", "name: Rust\ndescription: idiomatic rust and cargo"),
            skill("vue", "name: Vue\ndescription: Vue 3 and Nuxt apps"),
        ];
        let (bindings, gaps) = bind(&fp(&[Language::Rust, Language::Zig]), &skills);
        let ids: Vec<&str> = bindings.iter().map(|b| b.skill.id.as_str()).collect();
        assert_eq!(ids, ["antislop", "rust"], "{bindings:?}");
        assert_eq!(
            gaps,
            vec![Gap {
                language: Language::Zig
            }]
        );
    }

    #[test]
    fn patterns_bind_by_extension_count() {
        let skills = vec![skill(
            "dep",
            "name: Depot\nfile_patterns: [*.rs]\ndescription: content-addressed depots",
        )];
        let (bindings, gaps) = bind(&fp(&[Language::Rust]), &skills);
        assert_eq!(bindings.len(), 1);
        assert!(bindings[0].reason.contains("*.rs"));
        assert!(gaps.is_empty());
    }

    #[test]
    fn empty_stack_binds_only_universals() {
        let skills = vec![
            skill("antislop", "name: antislop\ndescription: load always"),
            skill("rust", "name: Rust\ndescription: rust cargo"),
        ];
        let (bindings, gaps) = bind(&fp(&[]), &skills);
        assert_eq!(bindings.len(), 1);
        assert!(gaps.is_empty());
    }

    #[test]
    fn watcher_predicate_is_precise() {
        assert!(affects_fingerprint(Path::new("/w/Cargo.toml")));
        assert!(affects_fingerprint(Path::new("/w/ui/package.json")));
        assert!(affects_fingerprint(Path::new("/w/src/main.rs")));
        assert!(!affects_fingerprint(Path::new("/w/README.md")));
        assert!(!affects_fingerprint(Path::new(
            "/w/.openchat/memory.sqlite"
        )));
    }
}
