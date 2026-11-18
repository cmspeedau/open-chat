//! Serialized build/lint queue.
//!
//! Heavy subprocess work (`cargo check`, `tsc`, test runs) across *all*
//! agents flows through one semaphore, so three concurrent agents can never
//! launch three full compiler runs.

use std::sync::Arc;

use tokio::sync::{Semaphore, SemaphorePermit};

/// Shared semaphore behind cheap `Governor` handles.
#[derive(Debug)]
pub struct BuildQueue {
    slots: Arc<Semaphore>,
}

/// RAII hold on one build slot; the next build starts when it drops.
#[derive(Debug)]
pub struct BuildGuard<'q> {
    _permit: SemaphorePermit<'q>,
}

impl BuildQueue {
    #[must_use]
    pub fn new(max_concurrent_builds: usize) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(max_concurrent_builds.max(1))),
        }
    }

    /// Wait for the next free build slot.
    pub async fn acquire(&self) -> BuildGuard<'_> {
        // A closed semaphore would mean a logic bug (we never close it);
        // unreachable in practice, so a clear panic beats silently lying.
        let permit = self
            .slots
            .acquire()
            .await
            .expect("build queue never closes");
        BuildGuard { _permit: permit }
    }

    /// Non-blocking try, for interactive paths that must not queue.
    #[must_use]
    pub fn try_acquire(&self) -> Option<BuildGuard<'_>> {
        self.slots
            .try_acquire()
            .ok()
            .map(|_permit| BuildGuard { _permit })
    }

    #[must_use]
    pub fn available_slots(&self) -> usize {
        self.slots.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serializes_single_slot() {
        let queue = BuildQueue::new(1);
        let g1 = queue.acquire().await;
        assert!(queue.try_acquire().is_none(), "second build must wait");
        drop(g1);
        assert!(queue.try_acquire().is_some());
    }

    #[tokio::test]
    async fn guards_release_slots() {
        let queue = BuildQueue::new(2);
        assert_eq!(queue.available_slots(), 2);
        {
            let _a = queue.acquire().await;
            let _b = queue.acquire().await;
            assert_eq!(queue.available_slots(), 0);
        }
        assert_eq!(queue.available_slots(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiter_wakes_when_slot_frees() {
        let queue = Arc::new(BuildQueue::new(1));
        let held = queue.acquire().await;
        let q2 = Arc::clone(&queue);
        let joiner = tokio::spawn(async move {
            let _g = q2.acquire().await;
        });
        drop(held);
        joiner.await.expect("waiter got its slot");
    }
}
