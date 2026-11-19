//! Stream-level slop interception (Anti-Slop Law enforcement point 2).
//!
//! Models routinely front-load answers with filler ("Sure! Here is the
//! code:"). The law forbids it; this module makes that structural: leading
//! filler *sentences* are detected with a prefix trie of known opener
//! phrases and stripped before the text reaches history, events, or the
//! user. Only leading sentences are touched; everything after the first
//! substantive line ships verbatim, and code-bearing text is never
//! trimmed.

use std::collections::BTreeMap;

/// Known filler openers. Case-insensitive prefix match at a sentence
/// start; a sentence qualifies only when short and code-free.
const FILLER_PREFIXES: &[&str] = &[
    "sure",
    "certainly",
    "of course",
    "absolutely",
    "happy to",
    "glad to",
    "no problem",
    "great question",
    "good question",
    "i can help",
    "i'd be happy",
    "i will now",
    "i'll now",
    "i'll help",
    "i can",
    "let me",
    "here is",
    "here are",
    "here's",
    "below is",
    "below are",
    "first,",
    "to do this,",
    "simply put",
];

#[derive(Default)]
struct TrieNode {
    children: BTreeMap<char, TrieNode>,
    terminal: bool,
}

/// Character trie over [`FILLER_PREFIXES`]; `longest_prefix_len` answers
/// "does a normalized line start with any filler phrase, and how long is
/// it?" in O(line) without rescanning the list per candidate.
#[derive(Default)]
pub struct PrefixTrie {
    root: TrieNode,
}

impl PrefixTrie {
    #[must_use]
    pub fn new(words: &[&str]) -> Self {
        let mut trie = Self::default();
        for word in words {
            trie.insert(word);
        }
        trie
    }

    fn insert(&mut self, word: &str) {
        let mut node = &mut self.root;
        for ch in word.trim_end().chars() {
            node = node.children.entry(ch).or_default();
        }
        node.terminal = true;
    }

    /// Longest inserted prefix of `line` (compared lowercased,
    /// alphanumeric/space normalized), in bytes of the input.
    #[must_use]
    pub fn longest_prefix_len(&self, line: &str) -> Option<usize> {
        let mut node = &self.root;
        let mut best = None;
        let mut in_word_gap = false;
        for (idx, ch) in line.char_indices() {
            let norm = if ch.is_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if matches!(ch, ' ' | '\'' | ',') {
                None // phrase separators match any single space/apostrophe
            } else {
                break;
            };
            let next = match norm {
                Some(c) => node.children.get(&c),
                None => node.children.get(&' '),
            };
            let Some(child) = next else {
                // Punctuation ends a phrase cleanly ("Sure!" -> "sure").
                if node.terminal && idx > 0 && !in_word_gap {
                    best = Some(idx);
                }
                break;
            };
            node = child;
            let end = idx + ch.len_utf8();
            in_word_gap = norm.is_none();
            if node.terminal && !in_word_gap {
                best = Some(end);
            }
        }
        best
    }
}

fn global_trie() -> &'static PrefixTrie {
    use std::sync::OnceLock;
    static TRIE: OnceLock<PrefixTrie> = OnceLock::new();
    TRIE.get_or_init(|| PrefixTrie::new(FILLER_PREFIXES))
}

/// A sentence is filler when it starts with a known phrase, is short, ends
/// at sentence punctuation, and carries no code or path markers.
fn is_filler_sentence(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true; // blank separators inside the preamble go with it
    }
    if t.len() > 100 {
        return false;
    }
    if t.contains("```")
        || t.contains("@@@")
        || t.contains("<<<<<<<")
        || t.contains("=======")
        || t.contains(">>>>>>>")
        || t.contains("->")
        || t.contains("::")
        || t.contains("./")
    {
        return false;
    }
    if !(t.ends_with('.') || t.ends_with('!') || t.ends_with('?') || t.ends_with(':')) {
        return false;
    }
    let after = global_trie().longest_prefix_len(t);
    match after {
        // The phrase must be followed by sentence end or trivial residue
        // ("Here is the updated code:"), not by real content.
        Some(n) => {
            let rest = t[n..].trim_start_matches([':', ',', ' ']);
            let rest = rest.trim_end_matches(['.', '!', '?']);
            let words = rest.split_whitespace().count();
            let tail = rest.trim();
            tail.is_empty()
                || words <= 5 && !tail.contains('(') && !tail.contains('`') && !tail.contains('.')
        }
        None => false,
    }
}

/// Split leading text into candidate sentences on `.`, `!`, `?` or line
/// breaks. Returns (filler prefix end byte, rest start). Conservative:
/// on any ambiguity, nothing is stripped.
#[must_use]
pub fn strip_leading_slop(text: &str) -> (&str, usize) {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let mut stripped = 0usize;
    // Next sentence: up to and including terminator + following ws.
    // `:` is deliberately NOT a terminator: a colon introduces real
    // content ("Here's the fix: line 12 becomes..."), so cutting there
    // would eat what the model actually said.
    while let Some(rel) = bytes[pos..]
        .iter()
        .position(|b| matches!(b, b'.' | b'!' | b'?' | b'\n'))
    {
        let term = pos + rel;
        let mut next = term + 1;
        while next < bytes.len()
            && (bytes[next] == b' ' || bytes[next] == b'\n' || bytes[next] == b'\r')
        {
            next += 1;
        }
        let sentence = &text[pos..next.min(text.len())];
        if !is_filler_sentence(sentence) {
            break;
        }
        stripped = next;
        pos = next;
        if pos >= text.len() {
            break;
        }
    }
    if stripped == 0 {
        return (text, 0);
    }
    (&text[stripped..], stripped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trie_matches_phrases_with_punctuation() {
        let trie = global_trie();
        assert!(trie.longest_prefix_len("Sure, I can help").is_some());
        assert!(trie.longest_prefix_len("sure! ").is_some());
        assert!(
            trie.longest_prefix_len("Of course —").is_some(),
            "em-dash cuts at space gap"
        );
        assert_eq!(trie.longest_prefix_len("Nothing filler"), None);
    }

    #[test]
    fn strips_filler_preamble_keeps_substance() {
        let (rest, n) = strip_leading_slop("Sure! Here is the code.\npub fn x() {}\n");
        assert_eq!(rest, "pub fn x() {}\n");
        assert!(n > 0);
    }

    #[test]
    fn multiple_filler_sentences_all_go() {
        let (rest, _) = strip_leading_slop(
            "Certainly. Great question! Let me help with that. The fix is in main.rs:",
        );
        // The last segment is real-ish? "The fix is in main.rs:" is not a
        // known opener -> stop before it.
        assert!(rest.starts_with("The fix"), "{rest:?}");
    }

    #[test]
    fn never_strips_code_or_diffs() {
        let t = "@@@ src/main.rs\n<<<<<<< SEARCH\nfn a() {}\n=======\nfn b() {}\n>>>>>>> REPLACE\n";
        let (rest, n) = strip_leading_slop(t);
        assert_eq!(rest, t);
        assert_eq!(n, 0);
        let t2 = "I can explain:: pub fn do_thing(x: i32) -> i32 { x }\n";
        let (r2, n2) = strip_leading_slop(t2);
        assert_eq!(n2, 0, "code-bearing sentence survives: {r2:?}");
    }

    #[test]
    fn substantive_openers_are_kept() {
        for t in [
            "cargo check passes on the workspace.",
            "2 + 2 = 4.",
            "Fixed: the affinity mask was applied before exec.",
            "No changes were needed; the test suite is green.",
        ] {
            let (rest, n) = strip_leading_slop(t);
            assert_eq!(n, 0, "must not strip {t:?} -> {rest:?}");
        }
    }

    #[test]
    fn all_filler_text_yields_empty_and_counts() {
        let (rest, n) = strip_leading_slop("Of course! Sure.");
        assert_eq!(rest, "");
        assert_eq!(n, 16);
    }
}
