//! Code-surface slop gate (Phase 2: Code-Surface Anti-Slop Skill Pack).
//!
//! What can be detected reliably from patch text alone, without an AST, is
//! gated here: swallowed errors (empty/bare catches, `except:`, Go
//! `return nil` on err) and decorative or narrating comments (banner rules,
//! `// Step 1:`, ALL-CAPS labels) - the two families where line-level
//! signatures are unambiguous and false positives stay near zero.
//!
//! What is NOT claimed: needless-abstraction, wrapper-layer, and
//! premature-micro-optimization detection. Those need the tree-sitter AST
//! gate (ideas.md Layer 3); until it exists this module reports nothing
//! about them and the rules pack instructs the model instead. The project's
//! own linter (via the verification loop) remains the real code judge.

use std::path::Path;

/// Extensions considered a code surface for auto-binding purposes.
pub const SOURCE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "zig", "c", "h", "cpp", "hpp", "cc",
    "cs", "java", "rb", "php", "kt", "swift", "scala", "lua",
];

#[must_use]
pub fn is_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_EXTS.contains(&e))
}

/// One gate hit: machine kind plus the offending line (truncated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlopFinding {
    pub kind: &'static str,
    pub line: String,
}

impl SlopFinding {
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}: {}", self.kind, self.line)
    }
}

/// Scan the text a patch ADDS to a source file. Findings are advisory by
/// design (the verifier is the hard gate); non-source paths are skipped so
/// docs and configs are never policed by code rules.
#[must_use]
pub fn check_added_source_text(path: &Path, added: &str) -> Vec<SlopFinding> {
    if !is_source_path(path) {
        return Vec::new();
    }
    let lines: Vec<&str> = added.lines().collect();
    let mut out = Vec::new();
    let push = |kind: &'static str, line: &str, out: &mut Vec<SlopFinding>| {
        let mut end = line.trim().len().min(120);
        let t = line.trim();
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        out.push(SlopFinding {
            kind,
            line: t[..end].to_owned(),
        });
    };

    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        let next_code = lines[i + 1..]
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty());

        // --- swallowed errors ------------------------------------------------
        if t.starts_with("except :") || t.starts_with("except:") {
            push("swallowed-error (bare except)", t, &mut out);
            continue;
        }
        if (t.contains("catch (")
            || t.contains("catch(")
            || t.contains("catch {")
            || t.contains("catch{"))
            && catches_nothing(t, next_code)
        {
            push("swallowed-error (empty catch)", t, &mut out);
            continue;
        }
        if t.contains(".catch(") && contains_noop_lambda(t) {
            push("swallowed-error (noop .catch)", t, &mut out);
            continue;
        }
        if t.starts_with("if err != nil") && swallows_go_err(t, next_code) {
            push("swallowed-error (err discarded)", t, &mut out);
            continue;
        }

        // --- decorative / narrating comments ---------------------------------
        let comment_body = comment_body(t);
        if let Some(body) = comment_body {
            if is_banner(body) {
                push("decorative comment", t, &mut out);
                continue;
            }
            if is_step_narration(body) {
                push("workflow narration", t, &mut out);
                continue;
            }
            if is_all_caps_label(body) {
                push("ALL-CAPS label comment", t, &mut out);
                continue;
            }
        }
    }
    out
}

/// True when a catch clause body is empty, comment-only, or `pass`.
/// The body search anchors on the first `{` AFTER `catch`, so code like
/// `try { run(); } catch (e) {}` is judged on the catch body, not the try.
fn catches_nothing(t: &str, next: Option<&str>) -> bool {
    let Some(ci) = t.find("catch") else {
        return false;
    };
    let Some(rel) = t[ci..].find('{') else {
        return false;
    };
    let open = ci + rel;
    let after = t[open + 1..].trim();
    if let Some(inner) = after.strip_suffix('}') {
        let body = inner.trim();
        return body.is_empty() || body.starts_with("//") || body == "pass";
    }
    if after.is_empty() || after.ends_with('{') {
        return matches!(next, Some(n) if n == "}" || n == "pass" || n.starts_with("//"));
    }
    false
}

fn contains_noop_lambda(t: &str) -> bool {
    let norm: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    [
        ".catch(()=>{})",
        ".catch(()=>undefined)",
        ".catch(function(){})",
    ]
    .iter()
    .any(|p| norm.contains(p))
}

fn swallows_go_err(t: &str, next: Option<&str>) -> bool {
    if let Some(rest) = t
        .split_once('{')
        .map(|(_, r)| r.trim().trim_end_matches('}').trim())
    {
        if rest.is_empty() {
            return matches!(next, Some(n) if n.starts_with("return nil") && !n.contains("err"));
        }
        return rest.starts_with("return nil") && !rest.contains("err");
    }
    false
}

/// Strip comment markers to inspect what the comment actually says.
fn comment_body(t: &str) -> Option<&str> {
    for marker in ["//", "/*", "#", "--", ";;", "-- "] {
        if let Some(rest) = t.strip_prefix(marker) {
            return Some(rest.trim().trim_end_matches("*/").trim());
        }
    }
    None
}

fn is_banner(body: &str) -> bool {
    // `====`, `----`, `****`, `~~~~` separators, alone or bracketing a label.
    let core = body.trim_matches(|c: char| "-=*_~ ".contains(c));
    let decor_count = body.chars().filter(|c| "-=*_~".contains(*c)).count();
    decor_count >= 4 && (core.is_empty() || core.chars().count() < body.chars().count() / 2)
}

fn is_step_narration(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    let first_word: String = b
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    let starts_numbered = b.chars().next().is_some_and(|c| c.is_ascii_digit())
        && b.get(..4)
            .is_some_and(|h| h.contains(". ") || h.contains(") "));
    matches!(
        first_word.as_str(),
        "step" | "first" | "next" | "finally" | "lastly"
    ) || starts_numbered
}

fn is_all_caps_label(body: &str) -> bool {
    let b = body.trim_end_matches([':', '.', '!']);
    if b.len() < 4 || b.starts_with("TODO") || b.starts_with("FIXME") {
        return false;
    }
    b.chars().filter(char::is_ascii_uppercase).count() >= 4
        && b.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, ' ' | '_' | '-'))
        && b.chars().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn check(ext: &str, added: &str) -> Vec<SlopFinding> {
        check_added_source_text(&PathBuf::from(format!("src/mod.{ext}")), added)
    }

    fn kinds(v: &[SlopFinding]) -> Vec<&'static str> {
        v.iter().map(|f| f.kind).collect()
    }

    #[test]
    fn python_bare_except_flagged() {
        let f = check("py", "try:\n    run()\nexcept:\n    pass\n");
        assert_eq!(kinds(&f), ["swallowed-error (bare except)"]);
    }

    #[test]
    fn empty_catch_single_and_multi_line() {
        assert_eq!(
            kinds(&check("rs", "fn x() {\n    let y = run();\n}\n")).as_slice(),
            &[] as &[&'static str],
            "no catch at all stays clean"
        );
        assert_eq!(
            kinds(&check("java", "} catch (IOException e) {}")).as_slice(),
            ["swallowed-error (empty catch)"]
        );
        assert_eq!(
            kinds(&check("js", "} catch (err) {\n}\n")).as_slice(),
            ["swallowed-error (empty catch)"]
        );
        // A catch that does something survives.
        assert!(
            check("js", "} catch (err) {\n    console.error(err);\n}\n").is_empty(),
            "handled error must not flag"
        );
    }

    #[test]
    fn js_noop_catch_and_go_swallow() {
        assert_eq!(
            kinds(&check("ts", "promise.catch(() => {});")).as_slice(),
            ["swallowed-error (noop .catch)"]
        );
        assert_eq!(
            kinds(&check("go", "if err != nil {\n    return nil\n}")).as_slice(),
            ["swallowed-error (err discarded)"]
        );
        assert_eq!(
            kinds(&check("go", "if err != nil { return nil }")).as_slice(),
            ["swallowed-error (err discarded)"]
        );
        // Wrapping the error is fine.
        assert!(
            check(
                "go",
                "if err != nil {\n    return fmt.Errorf(\"run: %w\", err)\n}"
            )
            .is_empty()
        );
    }

    #[test]
    fn decorative_and_narration_flags() {
        assert_eq!(
            kinds(&check(
                "rs",
                "// ================= AUTHENTICATION ================="
            ))
            .as_slice(),
            ["decorative comment"]
        );
        assert_eq!(
            kinds(&check("rs", "// ---- workflow ----")).as_slice(),
            ["decorative comment"]
        );
        assert_eq!(
            kinds(&check("rs", "// WORKFLOW")).as_slice(),
            ["ALL-CAPS label comment"]
        );
        assert_eq!(
            kinds(&check("rs", "// Step 1: validate input")).as_slice(),
            ["workflow narration"]
        );
        assert_eq!(
            kinds(&check("py", "# First, parse the file")).as_slice(),
            ["workflow narration"]
        );
        // Real comments survive untouched.
        for ok in [
            "// retries back off exponentially (200ms base)",
            "// TODO: wire this to the metrics sink",
            "# MemAvailable / MemTotal from /proc/meminfo",
            "// SAFETY: the mask is a stack POD",
        ] {
            assert!(check("rs", ok).is_empty(), "must not flag: {ok:?}");
        }
    }

    #[test]
    fn non_source_files_are_ignored() {
        assert!(check("md", "except:\n    pass\n// ===== X =====").is_empty());
        assert!(check("toml", "catch {}").is_empty());
        assert!(!check("rs", "catch {}").is_empty(), "rs IS a code surface");
    }

    #[test]
    fn unicode_lines_truncate_safely() {
        let f = check("py", &format!("except:\n    pass\n// {}", "é".repeat(300)));
        assert!(f.iter().all(|x| x.line.len() <= 120));
    }
}

#[cfg(test)]
mod mixed_line_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn catch_after_try_body_on_one_line() {
        let f = check_added_source_text(
            &PathBuf::from("m.ts"),
            "try { run(); } catch (e) {}\n// ================= SETUP =================",
        );
        let kinds: Vec<&str> = f.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            ["swallowed-error (empty catch)", "decorative comment"]
        );
        // A catch with real handling on the same line passes.
        assert!(
            check_added_source_text(
                &PathBuf::from("m.ts"),
                "try { run(); } catch (e) { log(e); }"
            )
            .is_empty()
        );
    }
}
