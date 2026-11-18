//! Subagent task supervision: typed results, heartbeats, stall detection.
//!
//! Directly targets the OpenChamber failure where a parent session hangs
//! forever waiting on a child (issues #690, #869): every supervised task
//! gets a [`Heartbeat`] handle, and unless it beats within the idle budget
//! the supervisor ends it with a typed [`TaskError::Stalled`] instead of an
//! infinite wait. A second, total ceiling caps even chatty tasks.

use std::future::Future;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TaskError {
    /// No progress signal within the idle budget; the task was dropped.
    #[error("subagent stalled: no heartbeat for {idle:?} (total budget {budget:?})")]
    Stalled { idle: Duration, budget: Duration },
    /// Hard wall-clock ceiling exceeded; the task was dropped.
    #[error("subagent exceeded total budget of {0:?}")]
    TimedOut(Duration),
    /// The task ended without a usable result.
    #[error("subagent failed: {0}")]
    Failed(String),
}

/// Terminal outcome of a supervised task.
#[derive(Debug)]
pub enum TaskOutcome<T> {
    Done(T),
    Err(TaskError),
}

impl<T> TaskOutcome<T> {
    /// Unwrap success or take the supervision error.
    pub fn result(self) -> Result<T, TaskError> {
        match self {
            Self::Done(v) => Ok(v),
            Self::Err(e) => Err(e),
        }
    }
}

/// Clone-and-signal handle a subagent uses to report progress.
#[derive(Clone, Debug)]
pub struct Heartbeat(tokio::sync::mpsc::UnboundedSender<()>);

impl Heartbeat {
    /// Note progress. Errors only when the supervisor has given up.
    pub fn beat(&self) -> Result<(), TaskError> {
        self.0
            .send(())
            .map_err(|_| TaskError::Failed("supervisor detached".into()))
    }
}

/// Watch `future`, resetting the idle clock on every beat received from
/// `beats`. Cancels the task when the idle budget or the total ceiling
/// trips, or when every `Heartbeat` sender is dropped without completion.
///
/// `async fn`: no timers are armed until the returned future is awaited.
pub async fn supervise<F>(
    future: F,
    mut beats: tokio::sync::mpsc::UnboundedReceiver<()>,
    idle: Duration,
    total: Duration,
) -> TaskOutcome<F::Output>
where
    F: Future,
{
    let pin = std::pin::pin!(future);
    tokio::pin!(pin);
    let total_deadline = tokio::time::sleep(total);
    tokio::pin!(total_deadline);
    let idle_timer = tokio::time::sleep(idle);
    tokio::pin!(idle_timer);
    loop {
        tokio::select! {
            biased;
            // Completion first: never report a stall for a finished task.
            v = &mut pin => return TaskOutcome::Done(v),
            // A beat resets idle; channel closure (all senders dropped
            // without completion) counts as silence and stalls too,
            // because the recv arm below will never yield again.
            Some(()) = beats.recv() => {
                idle_timer.as_mut().reset(tokio::time::Instant::now() + idle);
            }
            () = &mut idle_timer => {
                return TaskOutcome::Err(TaskError::Stalled { idle, budget: total });
            }
            () = &mut total_deadline => {
                return TaskOutcome::Err(TaskError::TimedOut(total));
            }
        }
    }
}

/// Spawn supervised work on the runtime. `work` receives its heartbeat
/// handle; the join handle yields the supervised outcome. Dropping the join
/// handle cancels the child.
#[must_use = "supervised output should be collected or detached explicitly"]
pub fn spawn_supervised<T, F, Fut>(
    idle: Duration,
    total: Duration,
    work: F,
) -> tokio::task::JoinHandle<TaskOutcome<T>>
where
    T: Send + 'static,
    F: FnOnce(Heartbeat) -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
{
    let (beat_tx, beat_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let task = work(Heartbeat(beat_tx));
        supervise(task, beat_rx, idle, total).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct-`supervise` helper mirroring what `spawn_supervised` wires.
    async fn watched<T, F, Fut>(idle: Duration, total: Duration, work: F) -> TaskOutcome<T>
    where
        T: Send + 'static,
        F: FnOnce(Heartbeat) -> Fut,
        Fut: Future<Output = T>,
    {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        supervise(work(Heartbeat(tx)), rx, idle, total).await
    }

    #[tokio::test(start_paused = true)]
    async fn completes_fast_task() {
        let outcome = watched(Duration::from_secs(5), Duration::from_secs(9), |_| async {
            7u8
        })
        .await;
        assert_eq!(outcome.result().unwrap(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn stalls_silent_task() {
        let outcome = watched(
            Duration::from_millis(10),
            Duration::from_secs(3600),
            |hb| async move {
                drop(hb);
                tokio::time::sleep(Duration::from_secs(60)).await;
                1u8
            },
        )
        .await;
        assert!(matches!(
            outcome.result(),
            Err(TaskError::Stalled { idle, .. }) if idle == Duration::from_millis(10)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeats_hold_off_the_stall_clock() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let hb = Heartbeat(tx.clone());
        let second = Heartbeat(tx);
        let fut = async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            hb.beat().unwrap();
            tokio::time::sleep(Duration::from_millis(25)).await;
            second.beat().unwrap();
            // Idle budget is 30ms; without the beats this would have stalled.
            tokio::time::sleep(Duration::from_millis(20)).await;
            42u8
        };
        let outcome = supervise(
            fut,
            rx,
            Duration::from_millis(30),
            Duration::from_secs(3600),
        )
        .await;
        assert_eq!(outcome.result().unwrap(), 42);
    }

    #[tokio::test(start_paused = true)]
    async fn total_ceiling_wins_over_beats() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let hb = Heartbeat(tx);
        let fut = async move {
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = hb.beat();
            }
            0u8
        };
        let outcome = supervise(
            fut,
            rx,
            Duration::from_millis(50),
            Duration::from_millis(45),
        )
        .await;
        assert!(matches!(outcome.result(), Err(TaskError::TimedOut(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_heartbeat_sender_stalls() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(tx);
        let fut = async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            0u8
        };
        let outcome = supervise(
            fut,
            rx,
            Duration::from_millis(10),
            Duration::from_secs(3600),
        )
        .await;
        assert!(matches!(outcome.result(), Err(TaskError::Stalled { .. })));
    }

    #[tokio::test]
    async fn spawn_supervised_end_to_end() {
        let handle = spawn_supervised(
            Duration::from_secs(30),
            Duration::from_secs(60),
            |hb| async move {
                hb.beat().unwrap();
                5u32 + 5
            },
        );
        let outcome = handle.await.expect("child task ran");
        assert_eq!(outcome.result().unwrap(), 10);
    }
}
