//! Skill bodies -> bounded prompt sections (Phase 3 -> Phase 4 bridge).
//!
//! [`prompt_set`] turns bound skills into capped text slices for the
//! compositor. Budgets exist because a 13k-token anti-slop catalog would
//! otherwise evict the user's actual question: each skill is clamped, the
//! set is capped in total, and selection is priority-ordered (explicit
//! language triggers first, weak/description matches next, always-on
//! universal rules last, since those govern style while language skills
//! govern correctness).

use std::fs;

use crate::bind::bind;
use crate::fingerprint::Fingerprint;
use crate::registry::{Skill, split_front_matter};

/// One skill's prompt contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSkill {
    pub id: String,
    pub name: String,
    pub body: String,
    pub tokens: usize,
    /// True when the body was clamped to the per-skill cap.
    pub capped: bool,
}

/// Bytes per estimated token used for clamping (openchat-core rule).
const BYTES_PER_TOKEN: usize = 4;

/// Select and load bound skills for prompt injection within budgets.
#[must_use]
pub fn prompt_set(
    fp: &Fingerprint,
    skills: &[Skill],
    per_skill_cap_tokens: usize,
    total_cap_tokens: usize,
) -> Vec<PromptSkill> {
    let (bindings, _gaps) = bind(fp, skills);
    // Stable priority: strong matches (explicit front-matter or id/name)
    // first, description-only hits next, always-on universal rules last.
    let mut ordered: Vec<(u8, &crate::bind::Binding<'_>)> = bindings
        .iter()
        .map(|b| {
            let prio = if b.skill.is_universal() {
                2
            } else {
                b.skill.strength.min(1)
            };
            (prio, b)
        })
        .collect();
    ordered.sort_by_key(|(prio, b)| (*prio, b.skill.id.clone()));

    let cap_bytes = per_skill_cap_tokens.max(64) * BYTES_PER_TOKEN;
    let mut out = Vec::new();
    let mut spent = 0usize;
    for (_, binding) in ordered {
        let skill = binding.skill;
        let Ok(text) = fs::read_to_string(&skill.path) else {
            tracing::debug!(path = %skill.path.display(), "skill body unreadable, skipped");
            continue;
        };
        let (_, body) = split_front_matter(&text);
        let (body, capped) = clamp(body, cap_bytes);
        let tokens = body.len().div_ceil(BYTES_PER_TOKEN);
        if !out.is_empty() && spent + tokens > total_cap_tokens {
            tracing::debug!(skill = %skill.id, "skill budget exhausted, remaining skills dropped");
            break;
        }
        spent += tokens;
        out.push(PromptSkill {
            id: skill.id.clone(),
            name: skill.name.clone(),
            body,
            tokens,
            capped,
        });
    }
    out
}

/// Tail-preserving clamp on a char boundary: for skill docs the *start*
/// (name, workflow intro) matters more than the tail, so keep the head.
fn clamp(body: &str, cap_bytes: usize) -> (String, bool) {
    if body.len() <= cap_bytes {
        return (body.to_owned(), false);
    }
    let mut cut = cap_bytes;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut head = body[..cut].to_owned();
    head.push_str("\n[... skill body truncated ...]");
    (head, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::load_roots;
    use std::path::PathBuf;

    fn write_skill(dir: &std::path::Path, id: &str, front: &str, body: &str) {
        let p = dir.join(format!("{id}.md"));
        std::fs::write(&p, format!("---\n{front}\n---\n{body}")).unwrap();
    }

    fn fp_rust() -> Fingerprint {
        Fingerprint {
            languages: vec![crate::fingerprint::Language::Rust],
            manifests: vec![PathBuf::from("Cargo.toml")],
            counts: std::collections::BTreeMap::new(),
            files_scanned: 1,
        }
    }

    #[test]
    fn loads_bodies_in_priority_order() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "rust",
            "name: Rust\ndescription: rust cargo",
            "rust body text",
        );
        write_skill(
            dir.path(),
            "antislop",
            "name: antislop\ndescription: rules, load always",
            "slop rules body",
        );
        write_skill(
            dir.path(),
            "vue",
            "name: Vue\ndescription: vue apps",
            "unused",
        );
        let skills = load_roots(&[dir.path().to_path_buf()]);
        let set = prompt_set(&fp_rust(), &skills, 1000, 1000);
        let ids: Vec<&str> = set.iter().map(|s| s.id.as_str()).collect();
        // language skill before universal; vue not selected at all
        assert_eq!(ids, ["rust", "antislop"]);
        assert!(set[0].body.contains("rust body text"));
    }

    #[test]
    fn per_skill_cap_truncates_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "big",
            "name: Big\ndescription: rust crate",
            &"x".repeat(4000),
        );
        let skills = load_roots(&[dir.path().to_path_buf()]);
        let set = prompt_set(&fp_rust(), &skills, 100, 100_000);
        assert_eq!(set.len(), 1);
        assert!(set[0].capped);
        assert!(set[0].body.contains("truncated"));
        assert!(set[0].tokens <= 110);
    }

    #[test]
    fn total_cap_drops_lower_priorities_first() {
        let dir = tempfile::tempdir().unwrap();
        // Each body ~500 tokens; total cap fits one plus headroom for the
        // universal only if we spend nothing on second language skill.
        write_skill(
            dir.path(),
            "rust-a",
            "name: Rust A\ndescription: rust",
            &"a".repeat(2000),
        );
        write_skill(
            dir.path(),
            "rust-b",
            "name: Rust B\ndescription: rust",
            &"b".repeat(2000),
        );
        write_skill(
            dir.path(),
            "uni",
            "name: Uni\ndescription: load always",
            &"u".repeat(2000),
        );
        let skills = load_roots(&[dir.path().to_path_buf()]);
        let set = prompt_set(&fp_rust(), &skills, 1000, 700);
        let ids: Vec<&str> = set.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["rust-a"],
            "priority order spends the shared budget first"
        );
    }

    #[test]
    fn strong_matches_starve_before_weak_ones() {
        let dir = tempfile::tempdir().unwrap();
        // Weak: only the description mentions rust.
        write_skill(
            dir.path(),
            "aaa-depot",
            "name: Depot Store\ndescription: transfer engines in rust",
            &"w".repeat(4000),
        );
        // Strong: the name itself is the language.
        write_skill(
            dir.path(),
            "zzz-rust",
            "name: Rust\ndescription: idiomatic code",
            &"s".repeat(4000),
        );
        let skills = load_roots(&[dir.path().to_path_buf()]);
        let set = prompt_set(&fp_rust(), &skills, 1000, 1100);
        let ids: Vec<&str> = set.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["zzz-rust"],
            "name-match outranks description-match despite alpha order"
        );
    }

    #[test]
    fn unreadable_paths_are_skipped() {
        let skills = vec![Skill {
            id: "ghost".into(),
            name: "Ghost".into(),
            description: "rust".into(),
            languages: vec![crate::fingerprint::Language::Rust],
            patterns: vec![],
            path: PathBuf::from("/nonexistent/SKILL.md"),
            tokens: 0,
            explicit_triggers: true,
            universal: false,
            strength: 0,
        }];
        assert!(prompt_set(&fp_rust(), &skills, 100, 100).is_empty());
    }
}
