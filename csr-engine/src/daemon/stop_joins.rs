//! The daemon's graceful stop, bounded by one deadline.
//!
//! launchd sends SIGTERM, waits the job's `ExitTimeOut`, then sends SIGKILL. The graceful path
//! used to await some loops with no bound and give each of about twenty others 5 s in turn, so
//! a stop could outlast any exit timeout and end in SIGKILL with nothing logged. Here every
//! loop is awaited against one shared deadline ([`JOIN_BUDGET`]). A loop that is still running
//! at the deadline is left behind; the process exits right after.
//!
//! Loops that write the HNSW index (`writes_index`) are never left running while the index is
//! flushed: if one is late, the flush is skipped and the log says so. The index on disk then
//! stays at its last dump, and the next start's additive backfill brings it level with the
//! database, which is the source of truth.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;

/// How long the graceful path waits for the daemon's loops, in total. Under the plist's
/// `ExitTimeOut` (30 s, see `setup::render_daemon_plist`), which also has to cover the index
/// flush and the last bookkeeping.
pub const JOIN_BUDGET: Duration = Duration::from_secs(20);

/// One loop to wait for.
pub struct Join {
    pub name: &'static str,
    /// The loop can mutate the HNSW index: the flush waits for it or does not run.
    pub writes_index: bool,
    task: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Join {
    /// A loop's join handle (any output; it is discarded).
    pub fn new<T: Send + 'static>(
        name: &'static str,
        writes_index: bool,
        handle: tokio::task::JoinHandle<T>,
    ) -> Self {
        Self::from_future(name, writes_index, async move {
            let _ = handle.await;
        })
    }

    /// Any future, for tests and for loops that are not spawned tasks.
    pub fn from_future(
        name: &'static str,
        writes_index: bool,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> Self {
        Self {
            name,
            writes_index,
            task: Box::pin(task),
        }
    }
}

/// What the joins left behind.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct JoinOutcome {
    /// Loops still running at the deadline.
    pub late: Vec<&'static str>,
    /// True when a late loop writes the index: the flush must not run.
    pub index_writer_late: bool,
}

impl JoinOutcome {
    pub fn flush_is_safe(&self) -> bool {
        !self.index_writer_late
    }
}

/// Await every join until `deadline`. The loops all saw the shutdown flag before this is
/// called and run concurrently, so awaiting them one after another against the same deadline
/// takes as long as the slowest one, never longer than the deadline.
pub async fn join_until(deadline: Instant, joins: Vec<Join>) -> JoinOutcome {
    let mut outcome = JoinOutcome::default();
    for join in joins {
        if tokio::time::timeout_at(deadline, join.task).await.is_err() {
            outcome.late.push(join.name);
            outcome.index_writer_late |= join.writes_index;
        }
    }
    outcome
}

/// The stamped daemon-log line for `outcome`, or `None` when every loop finished.
pub fn late_line(outcome: &JoinOutcome) -> Option<String> {
    if outcome.late.is_empty() {
        return None;
    }
    let flush = if outcome.flush_is_safe() {
        "the index flush still runs"
    } else {
        "the index flush is skipped; the next start brings the index level with the database"
    };
    Some(crate::telemetry::stamp_timing_line(&format!(
        "CSR daemon stop: {} still running after {} s ({}); {flush}",
        outcome.late.len(),
        JOIN_BUDGET.as_secs(),
        outcome.late.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slow(name: &'static str, writes_index: bool) -> Join {
        Join::from_future(name, writes_index, std::future::pending::<()>())
    }

    fn quick(name: &'static str, writes_index: bool) -> Join {
        Join::from_future(name, writes_index, async {})
    }

    #[tokio::test]
    async fn a_slow_loop_cannot_hold_the_stop_past_the_deadline() {
        let started = std::time::Instant::now();
        let deadline = Instant::now() + Duration::from_millis(200);
        let outcome = join_until(
            deadline,
            vec![
                quick("prompt", false),
                slow("journal", false),
                slow("regen", false),
                quick("plans", true),
            ],
        )
        .await;
        let took = started.elapsed();
        assert!(took < Duration::from_secs(2), "took {took:?}");
        assert_eq!(outcome.late, vec!["journal", "regen"]);
        assert!(outcome.flush_is_safe(), "no index writer is late");
        let line = late_line(&outcome).unwrap();
        assert!(line.contains("2 still running"), "{line}");
        assert!(line.contains("journal, regen"), "{line}");
        assert!(line.contains("the index flush still runs"), "{line}");
    }

    #[tokio::test]
    async fn a_late_index_writer_skips_the_flush() {
        let deadline = Instant::now() + Duration::from_millis(100);
        let outcome =
            join_until(deadline, vec![slow("catchup", true), quick("dream", false)]).await;
        assert_eq!(outcome.late, vec!["catchup"]);
        assert!(!outcome.flush_is_safe());
        assert!(late_line(&outcome).unwrap().contains("flush is skipped"));
    }

    #[tokio::test]
    async fn twenty_slow_loops_share_one_deadline_not_twenty() {
        // The old path gave each loop its own 5 s: twenty slow loops took 100 s.
        let started = std::time::Instant::now();
        let deadline = Instant::now() + Duration::from_millis(150);
        let joins = (0..20).map(|_| slow("loop", false)).collect();
        let outcome = join_until(deadline, joins).await;
        assert_eq!(outcome.late.len(), 20);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn finished_loops_leave_nothing_to_report() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let handle = tokio::spawn(async { 7 });
        let outcome = join_until(deadline, vec![Join::new("task", true, handle)]).await;
        assert_eq!(outcome, JoinOutcome::default());
        assert_eq!(late_line(&outcome), None);
    }

    #[test]
    fn the_budget_fits_under_the_plist_exit_timeout_with_room_for_the_flush() {
        assert!(JOIN_BUDGET + Duration::from_secs(5) <= crate::setup::DAEMON_EXIT_TIMEOUT);
    }
}
