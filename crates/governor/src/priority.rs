//! Subprocess priority (niceness).

/// Applies `low_process_priority`: children are `nice`d so agents never beat
/// the user's desktop for the scheduler.
///
/// Note: on Linux `nice(2)` is per-process, so the drop is applied inside
/// governed children at spawn (fork-then-nice-then-exec). Raising the
/// daemon's own priority back is a later refinement, tracked with the
/// Windows `BELOW_NORMAL_PRIORITY_CLASS` path.
#[derive(Debug, Clone, Copy)]
pub struct Priority {
    enabled: bool,
}

/// Nudge given to governed children when enabled. Gentle on purpose: +10 is
/// clearly below interactive yet recovers fast when the machine idles.
pub(crate) const CHILD_NICE: i32 = 10;

impl Priority {
    #[must_use]
    pub const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Nice value to apply to a child, or `None` when disabled.
    #[must_use]
    pub const fn child_nice(&self) -> Option<i32> {
        if self.enabled { Some(CHILD_NICE) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_yields_no_nudge() {
        assert_eq!(Priority::new(false).child_nice(), None);
        assert_eq!(Priority::new(true).child_nice(), Some(CHILD_NICE));
    }
}
