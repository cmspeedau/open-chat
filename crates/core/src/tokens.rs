//! Rough token accounting shared by the compositor and memory budgets.

/// Cheap, stable token estimate: cl100k averages ~4 bytes/token for prose.
/// Budget math here only needs determinism, not exactness; the real
/// tokenizer is a provider-reported-usage refinement (Phase 4 badges).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_up_quarters() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}
