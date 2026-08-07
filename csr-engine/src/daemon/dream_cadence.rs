//! Dream cadence — scheduling policy + persistence for the daemon's
//! periodic v10 "dreaming" cycle (`crate::dream::run_dream`).
//!
//! Pure decision functions ([`is_due`], [`should_catch_up`],
//! [`first_cycle_due_at`], [`next_due`]) plus the storage-only [`decide`]
//! gate are unit-tested directly against synthetic timestamps and a temp
//! `Storage` — no test in this module spawns a real multi-hour timer; the
//! async [`dream_loop`] wakes on a short poll interval and only actually
//! dreams when [`decide`] says so.
//!
//! # Persistence
//!
//! The daemon's own last-completed-cycle timestamp lives in the `meta`
//! key/value table (the same mechanism `Storage::integrity_check_cached`
//! uses for its TTL cache) under [`META_LAST_RUN_AT`] — deliberately
//! separate from `witness_verdicts::last_dream_run` (the newest EVENT's
//! timestamp). A cycle that runs and writes zero new events (the common
//! case re-running at an unchanged HEAD) would leave that value frozen
//! forever; the daemon needs its own record of when it last ACTED so
//! cadence math has something to measure from, and restarts don't
//! re-dream immediately.
//!
//! # Kill switch
//!
//! `CSR_NO_DREAMING=1` (or `true`) disables the cycle entirely — same idiom
//! as `CSR_NO_AI_NARRATIVES` (`crate::narrative::narratives_disabled`).
//!
//! # Cost discipline
//!
//! `run_dream` is pure local CPU + SQLite + git — no API tokens, so there
//! is no token budget to protect (unlike the narrator loop's Batch API
//! spend). The governor concern here is CPU/IO contention instead: a due
//! cycle joins the shared semaphore's fair queue behind any active heavy
//! pass (the file watcher importing a debounced batch, or the plans loop
//! importing plan docs).
//!
//! # Single-flight
//!
//! `dream_running` guards against a cycle still executing (in the
//! `spawn_blocking` that `tick` awaits directly) when a concurrent caller
//! races the same storage. On any
//! outcome (success, failure, or task join error) the flag is always
//! cleared so a stuck flag can never permanently wedge the cycle off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::engine::Engine;
use crate::storage::witness_ledger;
use crate::storage::Storage;

/// Default cadence between daemon dream cycles: every 6 hours.
pub const DEFAULT_INTERVAL_SECS: u64 = 6 * 60 * 60;
/// Largest supported cadence override: seven days.
pub const MAX_INTERVAL_SECS: u64 = 7 * 24 * 60 * 60;

/// Startup catch-up delay when dreaming has never run and there is
/// meaningful pre-existing evidence — see [`should_catch_up`].
pub const CATCHUP_DELAY_SECS: u64 = 5 * 60;

/// How often the loop wakes to check due-ness. Independent of the cadence
/// interval itself: waking often is cheap (one `meta` read), and dreaming
/// itself only happens when [`decide`] says a cycle is actually due.
const POLL_INTERVAL_SECS: u64 = 5 * 60;
/// How often a dream waiting for heavy work re-reads its environment kill
/// switch. Shutdown is polled more frequently by [`wait_for_shutdown`].
const WAIT_KILL_SWITCH_RECHECK_SECS: u64 = 1;

/// `meta` key: RFC3339 timestamp of the daemon's last COMPLETED dream cycle.
pub const META_LAST_RUN_AT: &str = "dream_daemon_last_run_at";
/// `meta` key: small JSON stats summary from the last completed cycle
/// (informational only — never read back by cadence math, only by humans
/// debugging via `sqlite3 ... "select value from meta where key = ...`).
pub const META_LAST_STATS: &str = "dream_daemon_last_stats";

/// `CSR_NO_DREAMING` kill switch — same "1"/"true" (case-insensitive) idiom
/// as `crate::narrative::narratives_disabled`.
pub fn dreaming_disabled() -> bool {
    std::env::var("CSR_NO_DREAMING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Configured cadence interval: `CSR_DREAM_INTERVAL_SECS` override (must
/// parse as a positive integer) or [`DEFAULT_INTERVAL_SECS`].
pub fn interval_secs() -> u64 {
    let Ok(raw) = std::env::var("CSR_DREAM_INTERVAL_SECS") else {
        return DEFAULT_INTERVAL_SECS;
    };
    let parsed = raw
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|&value| value > 0 && value <= MAX_INTERVAL_SECS)
        .and_then(|value| i64::try_from(value).ok().map(|_| value));
    match parsed {
        Some(value) => value,
        None => {
            tracing::warn!(
                value = %raw,
                max_secs = MAX_INTERVAL_SECS,
                default_secs = DEFAULT_INTERVAL_SECS,
                "invalid CSR_DREAM_INTERVAL_SECS; using default"
            );
            DEFAULT_INTERVAL_SECS
        }
    }
}

/// Has enough time passed since `last_run` for a cycle to be due at `now`?
/// `last_run = None` is never due through this function alone — the very
/// first cycle's timing is decided by [`should_catch_up`] +
/// [`first_cycle_due_at`], not here.
pub fn is_due(last_run: Option<DateTime<Utc>>, now: DateTime<Utc>, interval_secs: u64) -> bool {
    match last_run {
        Some(t) => {
            let seconds = i64::try_from(interval_secs).unwrap_or(i64::MAX);
            now.signed_duration_since(t) >= Duration::seconds(seconds)
        }
        None => false,
    }
}

/// Should the very first dream cycle be a startup catch-up (short delay)
/// instead of waiting the full cadence interval? True only when dreaming
/// has never completed a cycle before AND `witness_verdicts` is empty AND
/// the witness ledger holds real pre-existing evidence to judge — i.e. an
/// upgrade onto a rich, never-dreamed corpus that shouldn't sit idle for
/// hours before its first pass. A fresh install with an empty ledger has
/// nothing to catch up on, so it follows the normal interval from startup
/// instead (see [`first_cycle_due_at`]).
pub fn should_catch_up(never_run: bool, verdicts_empty: bool, ledger_non_trivial: bool) -> bool {
    never_run && verdicts_empty && ledger_non_trivial
}

/// The first cycle's due time, computed relative to `process_start`
/// (captured once when [`dream_loop`] starts).
pub fn first_cycle_due_at(
    process_start: DateTime<Utc>,
    catch_up: bool,
    interval_secs: u64,
) -> DateTime<Utc> {
    let delay = if catch_up {
        CATCHUP_DELAY_SECS
    } else {
        interval_secs
    };
    process_start + Duration::seconds(i64::try_from(delay).unwrap_or(i64::MAX))
}

/// `next_due` for `status`'s dream block. `None` when dreaming has never
/// completed a cycle — the exact first-cycle timing depends on daemon
/// process-start state that a stateless `status` read doesn't have (see
/// [`first_cycle_due_at`]); otherwise `last_run + interval`.
pub fn next_due(last_run: Option<DateTime<Utc>>, interval_secs: u64) -> Option<DateTime<Utc>> {
    last_run.map(|t| t + Duration::seconds(i64::try_from(interval_secs).unwrap_or(i64::MAX)))
}

/// Monotonic restart delay derived once from persisted UTC state. A future
/// timestamp (for example after the wall clock moves backward) is capped at
/// one interval rather than postponing work by the size of the clock jump.
pub fn restart_delay(
    last_run: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    interval_secs: u64,
    catch_up: bool,
) -> StdDuration {
    let delay = match last_run {
        None if catch_up => CATCHUP_DELAY_SECS,
        None => interval_secs,
        Some(last) => {
            let elapsed = now.signed_duration_since(last).num_seconds();
            if elapsed < 0 {
                interval_secs
            } else {
                interval_secs.saturating_sub(elapsed as u64)
            }
        }
    };
    StdDuration::from_secs(delay.min(interval_secs))
}

/// Retry delay after a failed cycle: 5m, 10m, 20m, 40m, then 1h cap.
pub fn failure_backoff(consecutive_failures: u32) -> StdDuration {
    let exponent = consecutive_failures.saturating_sub(1).min(4);
    let seconds = POLL_INTERVAL_SECS.saturating_mul(1_u64 << exponent);
    StdDuration::from_secs(seconds.min(60 * 60))
}

/// Read the persisted last-run timestamp, if any. Fail-soft: a parse error
/// or missing key both read as `None` (never dreamed) rather than an error
/// — cadence math must never crash a daemon loop.
pub fn read_last_run(storage: &Storage) -> Option<DateTime<Utc>> {
    storage
        .get_meta(META_LAST_RUN_AT)
        .ok()
        .flatten()
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// Is a cycle due right now, given persisted state read fresh from
/// `storage`? Combines [`is_due`] (once a cycle has ever completed) with
/// the catch-up decision (before the first one ever has).
#[cfg(test)]
fn cycle_due(storage: &Storage, process_start: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let interval = interval_secs();
    match read_last_run(storage) {
        Some(t) => is_due(Some(t), now, interval),
        None => {
            let verdicts_empty = storage
                .dream_event_totals()
                .map(|(o, s, r)| o + s + r == 0)
                .unwrap_or(true);
            let ledger_non_trivial = storage
                .with_connection(witness_ledger::count_all)
                .map(|c| c > 0)
                .unwrap_or(false);
            let catch_up = should_catch_up(true, verdicts_empty, ledger_non_trivial);
            now >= first_cycle_due_at(process_start, catch_up, interval)
        }
    }
}

/// Why a tick did not start a dream cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `CSR_NO_DREAMING` is set.
    Disabled,
    /// Cadence math says it isn't time yet (steady-state — not logged).
    NotDue,
    /// A previous cycle is still executing (single-flight).
    AlreadyRunning,
    /// Daemon shutdown was requested while waiting for heavy work.
    Shutdown,
}

/// Decision gate for a due tick. A successful decision atomically owns the
/// heavy-work permit and claims `dream_running`; the caller must retain the
/// permit for the whole cycle and clear the flag after completion.
pub async fn decide(
    dream_running: &AtomicBool,
    heavy_work: &Arc<Semaphore>,
    due: bool,
    shutdown: &AtomicBool,
) -> std::result::Result<OwnedSemaphorePermit, SkipReason> {
    if dreaming_disabled() {
        return Err(SkipReason::Disabled);
    }
    if !due {
        return Err(SkipReason::NotDue);
    }
    if dream_running.load(Ordering::SeqCst) {
        return Err(SkipReason::AlreadyRunning);
    }

    let acquire = heavy_work.clone().acquire_owned();
    tokio::pin!(acquire);
    let shutdown_requested = wait_for_shutdown(shutdown);
    tokio::pin!(shutdown_requested);
    let mut kill_switch_recheck =
        tokio::time::interval(StdDuration::from_secs(WAIT_KILL_SWITCH_RECHECK_SECS));
    kill_switch_recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let permit = loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_requested => return Err(SkipReason::Shutdown),
            _ = kill_switch_recheck.tick() => {
                if dreaming_disabled() {
                    return Err(SkipReason::Disabled);
                }
            }
            result = &mut acquire => {
                break result.map_err(|_| SkipReason::Shutdown)?;
            }
        }
    };

    // A signal can race permit delivery between select polls. Never claim
    // single-flight after either cancellation source has become active.
    if shutdown.load(Ordering::SeqCst) {
        return Err(SkipReason::Shutdown);
    }
    if dreaming_disabled() {
        return Err(SkipReason::Disabled);
    }
    if dream_running.swap(true, Ordering::SeqCst) {
        return Err(SkipReason::AlreadyRunning);
    }
    Ok(permit)
}

async fn wait_for_shutdown(shutdown: &AtomicBool) {
    let mut recheck = tokio::time::interval(StdDuration::from_millis(100));
    recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        recheck.tick().await;
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TickOutcome {
    Success,
    Failed,
    Cancelled,
    Skipped(SkipReason),
}

fn record_completed_cycle(
    storage: &Storage,
    stats: &crate::dream::DreamStats,
    completed_at: DateTime<Utc>,
) -> Result<()> {
    storage.set_meta(META_LAST_RUN_AT, &completed_at.to_rfc3339())?;
    let summary = serde_json::json!({
        "anchors_considered": stats.anchors_considered,
        "witnesses_considered": stats.witnesses_considered,
        "superseded": stats.superseded,
        "obsolete": stats.obsolete,
        "reinstated": stats.reinstated,
        "events_written": stats.events_written,
        "events_deduped": stats.events_deduped,
    })
    .to_string();
    if let Err(error) = storage.set_meta(META_LAST_STATS, &summary) {
        tracing::warn!(%error, "dream cycle stats persistence failed (non-fatal)");
    }
    Ok(())
}

/// One cadence-checked tick: dream if due, respecting the kill switch, the
/// heavy-pass guard, and single-flight (see [`decide`]). Never propagates
/// an error — every failure is logged and swallowed, matching every other
/// daemon loop's non-fatal-iteration convention.
async fn tick(
    engine: &Arc<Engine>,
    dream_running: &Arc<AtomicBool>,
    heavy_work: &Arc<Semaphore>,
    shutdown: &Arc<AtomicBool>,
) -> TickOutcome {
    let permit = match decide(dream_running, heavy_work, true, shutdown).await {
        Ok(permit) => permit,
        Err(SkipReason::Disabled) | Err(SkipReason::NotDue) => {
            return TickOutcome::Skipped(if dreaming_disabled() {
                SkipReason::Disabled
            } else {
                SkipReason::NotDue
            });
        }
        Err(SkipReason::AlreadyRunning) => {
            tracing::info!("dream cycle skipped: previous cycle still running");
            return TickOutcome::Skipped(SkipReason::AlreadyRunning);
        }
        Err(SkipReason::Shutdown) => return TickOutcome::Cancelled,
    };

    // `decide` already set `dream_running = true` and returned the owned
    // heavy-work permit. Move both the permit and cancellation token into
    // the blocking SQLite/git work, then inspect its result for backoff.
    let eng = engine.clone();
    let cancellation = crate::dream::DreamCancellation::new(shutdown.clone());
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::dream::run_dream_with_cancellation(&eng, None, false, &cancellation)
    })
    .await;
    let outcome = match result {
        Ok(Ok(crate::dream::DreamRunResult::Complete(stats))) => {
            match record_completed_cycle(engine.storage(), &stats, Utc::now()) {
                Ok(()) => {
                    let completed_at = Utc::now();
                    tracing::info!(
                        anchors = stats.anchors_considered,
                        witnesses = stats.witnesses_considered,
                        superseded = stats.superseded,
                        obsolete = stats.obsolete,
                        reinstated = stats.reinstated,
                        events_written = stats.events_written,
                        events_deduped = stats.events_deduped,
                        completed_at = %completed_at,
                        "dream cycle complete"
                    );
                    TickOutcome::Success
                }
                Err(error) => {
                    tracing::warn!(%error, "dream cycle completion persistence failed");
                    TickOutcome::Failed
                }
            }
        }
        Ok(Ok(crate::dream::DreamRunResult::Cancelled(stats))) => {
            tracing::info!(
                anchors = stats.anchors_considered,
                witnesses = stats.witnesses_considered,
                events_written = stats.events_written,
                "dream cycle cancelled with partial progress"
            );
            TickOutcome::Cancelled
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "dream cycle failed (non-fatal)");
            TickOutcome::Failed
        }
        Err(e) => {
            tracing::warn!(error = %e, "dream cycle task join error (non-fatal)");
            TickOutcome::Failed
        }
    };
    dream_running.store(false, Ordering::SeqCst);
    outcome
}

/// Background loop: on the daemon's own cadence, run a dream cycle when
/// due. Wakes every [`POLL_INTERVAL_SECS`] to check due-ness cheaply;
/// actual dreaming only happens when [`decide`] (via [`tick`]) says yes.
/// No-ops for the whole daemon lifetime if [`dreaming_disabled`] — logged
/// once, matching `daemon::ratification::check_disabled`'s idiom.
pub async fn dream_loop(
    engine: Arc<Engine>,
    heavy_work: Arc<Semaphore>,
    shutdown: Arc<AtomicBool>,
) {
    if dreaming_disabled() {
        tracing::info!("dream cycle disabled via CSR_NO_DREAMING");
        return;
    }

    let dream_running = Arc::new(AtomicBool::new(false));
    let process_start = Utc::now();
    let interval = interval_secs();
    let last_run = read_last_run(engine.storage());
    let catch_up = if last_run.is_none() {
        let verdicts_empty = engine
            .storage()
            .dream_event_totals()
            .map(|(o, s, r)| o + s + r == 0)
            .unwrap_or(true);
        let ledger_non_trivial = engine
            .storage()
            .with_connection(witness_ledger::count_all)
            .map(|count| count > 0)
            .unwrap_or(false);
        should_catch_up(true, verdicts_empty, ledger_non_trivial)
    } else {
        false
    };
    let mut deadline =
        tokio::time::Instant::now() + restart_delay(last_run, process_start, interval, catch_up);
    let mut consecutive_failures = 0_u32;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("dream loop: shutdown signal received");
            break;
        }
        if dreaming_disabled() {
            tracing::info!("dream loop disabled via CSR_NO_DREAMING");
            break;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            match tick(&engine, &dream_running, &heavy_work, &shutdown).await {
                TickOutcome::Success => {
                    consecutive_failures = 0;
                    deadline = tokio::time::Instant::now() + StdDuration::from_secs(interval);
                }
                TickOutcome::Failed => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    let backoff = failure_backoff(consecutive_failures);
                    tracing::warn!(
                        consecutive_failures,
                        retry_secs = backoff.as_secs(),
                        "dream cycle retry scheduled"
                    );
                    deadline = tokio::time::Instant::now() + backoff;
                }
                TickOutcome::Cancelled | TickOutcome::Skipped(SkipReason::Disabled) => break,
                TickOutcome::Skipped(_) => {
                    deadline =
                        tokio::time::Instant::now() + StdDuration::from_secs(POLL_INTERVAL_SECS);
                }
            }
        }
        let until_deadline = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(until_deadline.min(StdDuration::from_secs(10))).await;
    }
}

/// Serializes every test (in this module AND `status`'s) that reads or
/// mutates the process-global env vars `dream_cadence` consults
/// (`CSR_DREAM_INTERVAL_SECS`, `CSR_NO_DREAMING`). `cargo test` runs unit
/// tests within one binary across multiple threads by default, and env
/// vars are process state, not thread-local — without this, two such tests
/// running concurrently (including across module boundaries — `status`'s
/// `gather_dream` calls `interval_secs()` too) would stomp each other's
/// `set_var`/`remove_var` and produce flaky failures. Poison-tolerant: one
/// panicking test must never permanently wedge every later one.
#[cfg(test)]
static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn env_test_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(offset_secs: i64) -> DateTime<Utc> {
        Utc::now() + Duration::seconds(offset_secs)
    }

    use super::env_test_guard as env_guard;

    // ── cadence math (due / not-due from persisted last-run) ──

    #[test]
    fn not_due_before_interval_elapses() {
        let last = ts(-100);
        assert!(!is_due(Some(last), ts(0), 200));
    }

    #[test]
    fn due_once_interval_elapses() {
        let last = ts(-300);
        assert!(is_due(Some(last), ts(0), 200));
    }

    #[test]
    fn due_at_exact_boundary() {
        let last = ts(-200);
        assert!(is_due(Some(last), ts(0), 200));
    }

    #[test]
    fn never_run_is_not_due_through_is_due_alone() {
        // `is_due(None, ..)` deliberately never fires on its own — the
        // first-cycle decision routes through `should_catch_up` +
        // `first_cycle_due_at` instead (see `cycle_due`'s two-armed match).
        assert!(!is_due(None, ts(0), 200));
    }

    #[test]
    fn interval_secs_reads_env_override() {
        let _guard = env_guard();
        std::env::set_var("CSR_DREAM_INTERVAL_SECS", "42");
        assert_eq!(interval_secs(), 42);
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");
    }

    #[test]
    fn interval_secs_falls_back_on_garbage() {
        let _guard = env_guard();
        std::env::set_var("CSR_DREAM_INTERVAL_SECS", "not-a-number");
        assert_eq!(interval_secs(), DEFAULT_INTERVAL_SECS);
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");
    }

    #[test]
    fn interval_secs_falls_back_on_zero() {
        let _guard = env_guard();
        std::env::set_var("CSR_DREAM_INTERVAL_SECS", "0");
        assert_eq!(interval_secs(), DEFAULT_INTERVAL_SECS);
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");
    }

    #[test]
    fn interval_secs_falls_back_when_value_exceeds_documented_maximum() {
        let _guard = env_guard();
        std::env::set_var(
            "CSR_DREAM_INTERVAL_SECS",
            (MAX_INTERVAL_SECS + 1).to_string(),
        );
        assert_eq!(interval_secs(), DEFAULT_INTERVAL_SECS);
        std::env::set_var("CSR_DREAM_INTERVAL_SECS", u64::MAX.to_string());
        assert_eq!(interval_secs(), DEFAULT_INTERVAL_SECS);
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");
    }

    #[test]
    fn failed_cycle_backoff_doubles_and_caps_at_one_hour() {
        assert_eq!(failure_backoff(1), std::time::Duration::from_secs(300));
        assert_eq!(failure_backoff(2), std::time::Duration::from_secs(600));
        assert_eq!(failure_backoff(3), std::time::Duration::from_secs(1200));
        assert_eq!(failure_backoff(4), std::time::Duration::from_secs(2400));
        assert_eq!(failure_backoff(5), std::time::Duration::from_secs(3600));
        assert_eq!(failure_backoff(20), std::time::Duration::from_secs(3600));
    }

    #[test]
    fn backward_wall_clock_jump_never_delays_restart_more_than_one_interval() {
        let now = ts(0);
        let future_last_run = now + Duration::hours(24);
        assert_eq!(
            restart_delay(Some(future_last_run), now, 600, false),
            std::time::Duration::from_secs(600)
        );
    }

    #[test]
    fn completion_is_not_recorded_when_persistence_fails() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                conn.execute("DROP TABLE meta", [])?;
                Ok(())
            })
            .unwrap();
        let result = record_completed_cycle(&storage, &crate::dream::DreamStats::default(), ts(0));
        assert!(result.is_err());
        assert!(read_last_run(&storage).is_none());
    }

    #[test]
    fn interval_secs_default_without_override() {
        let _guard = env_guard();
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");
        assert_eq!(interval_secs(), DEFAULT_INTERVAL_SECS);
    }

    #[test]
    fn next_due_is_none_when_never_run() {
        assert_eq!(next_due(None, 100), None);
    }

    #[test]
    fn next_due_is_last_run_plus_interval() {
        let last = ts(0);
        assert_eq!(
            next_due(Some(last), 500),
            Some(last + Duration::seconds(500))
        );
    }

    // ── kill switch ──

    #[test]
    fn kill_switch_off_by_default() {
        let _guard = env_guard();
        std::env::remove_var("CSR_NO_DREAMING");
        assert!(!dreaming_disabled());
    }

    #[test]
    fn kill_switch_recognizes_1_and_true_case_insensitively() {
        let _guard = env_guard();
        std::env::set_var("CSR_NO_DREAMING", "1");
        assert!(dreaming_disabled());
        std::env::set_var("CSR_NO_DREAMING", "true");
        assert!(dreaming_disabled());
        std::env::set_var("CSR_NO_DREAMING", "TRUE");
        assert!(dreaming_disabled());
        std::env::set_var("CSR_NO_DREAMING", "0");
        assert!(!dreaming_disabled());
        std::env::remove_var("CSR_NO_DREAMING");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn decide_skips_when_kill_switch_set_even_if_due() {
        let _guard = env_guard();
        std::env::set_var("CSR_NO_DREAMING", "1");
        let dream_running = AtomicBool::new(false);
        let heavy = Arc::new(Semaphore::new(1));
        let shutdown = AtomicBool::new(false);
        let result = decide(&dream_running, &heavy, true, &shutdown).await;
        std::env::remove_var("CSR_NO_DREAMING");
        assert!(matches!(result, Err(SkipReason::Disabled)));
        assert!(
            !dream_running.load(Ordering::SeqCst),
            "kill switch must not claim the single-flight flag"
        );
    }

    // ── catch-up trigger ──

    #[test]
    fn catch_up_only_when_never_run_and_empty_verdicts_and_nontrivial_ledger() {
        assert!(should_catch_up(true, true, true));
        assert!(!should_catch_up(false, true, true), "already ran once");
        assert!(
            !should_catch_up(true, false, true),
            "verdicts already exist"
        );
        assert!(
            !should_catch_up(true, true, false),
            "nothing to catch up on"
        );
    }

    #[test]
    fn first_cycle_due_at_uses_short_delay_on_catch_up() {
        let start = ts(0);
        let due = first_cycle_due_at(start, true, DEFAULT_INTERVAL_SECS);
        assert_eq!(due, start + Duration::seconds(CATCHUP_DELAY_SECS as i64));
    }

    #[test]
    fn first_cycle_due_at_uses_full_interval_without_catch_up() {
        let start = ts(0);
        let due = first_cycle_due_at(start, false, 500);
        assert_eq!(due, start + Duration::seconds(500));
    }

    #[test]
    fn cycle_due_catches_up_on_rich_never_dreamed_corpus() {
        let storage = Storage::open_memory().unwrap();
        // Seed a non-trivial witness_ledger with no witness_verdicts at all
        // — the "upgrade onto a rich, never-dreamed corpus" scenario.
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(
                    conn,
                    &witness_ledger::WitnessLedgerRow {
                        project: "proj".into(),
                        file: "/tmp/x.rs".into(),
                        stamp: "b3:1".into(),
                        tier: "committed".into(),
                        source_kind: "backfill".into(),
                        ..Default::default()
                    },
                )
            })
            .unwrap();

        let process_start = ts(-30); // daemon "started" 30s ago.
        assert!(
            !cycle_due(&storage, process_start, ts(0)),
            "catch-up delay hasn't elapsed yet"
        );
        assert!(
            cycle_due(
                &storage,
                process_start,
                process_start + Duration::seconds(CATCHUP_DELAY_SECS as i64 + 1)
            ),
            "catch-up delay elapsed — due"
        );
    }

    #[test]
    fn cycle_due_waits_full_interval_on_fresh_empty_install() {
        let _guard = env_guard();
        std::env::set_var("CSR_DREAM_INTERVAL_SECS", "1000");
        let storage = Storage::open_memory().unwrap(); // empty ledger, empty verdicts.
        let process_start = ts(0);
        let not_yet = !cycle_due(
            &storage,
            process_start,
            process_start + Duration::seconds(CATCHUP_DELAY_SECS as i64 + 1),
        );
        let now_due = cycle_due(
            &storage,
            process_start,
            process_start + Duration::seconds(1001),
        );
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");
        assert!(
            not_yet,
            "nothing to catch up on — must wait the full interval, not the short delay"
        );
        assert!(now_due);
    }

    #[test]
    fn cycle_due_reads_persisted_last_run_once_present() {
        let _guard = env_guard();
        std::env::set_var("CSR_DREAM_INTERVAL_SECS", "100");
        let storage = Storage::open_memory().unwrap();
        storage
            .set_meta(META_LAST_RUN_AT, &ts(-50).to_rfc3339())
            .unwrap();
        let too_soon = !cycle_due(&storage, ts(-1000), ts(0));
        storage
            .set_meta(META_LAST_RUN_AT, &ts(-150).to_rfc3339())
            .unwrap();
        let overdue = cycle_due(&storage, ts(-1000), ts(0));
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");
        assert!(too_soon, "last run 50s ago, 100s interval — not due yet");
        assert!(overdue, "last run 150s ago, 100s interval — due");
    }

    // ── single-flight skip ──

    #[tokio::test]
    async fn decide_skips_when_already_running() {
        let dream_running = AtomicBool::new(true); // a cycle is already mid-flight.
        let heavy = Arc::new(tokio::sync::Semaphore::new(1));
        let shutdown = AtomicBool::new(false);
        let result = decide(&dream_running, &heavy, true, &shutdown).await;
        assert!(matches!(result, Err(SkipReason::AlreadyRunning)));
        assert!(
            dream_running.load(Ordering::SeqCst),
            "single-flight must not clear a flag it didn't set"
        );
    }

    #[tokio::test]
    async fn decide_claims_the_flag_on_success() {
        let dream_running = AtomicBool::new(false);
        let heavy = Arc::new(Semaphore::new(1));
        let shutdown = AtomicBool::new(false);
        let result = decide(&dream_running, &heavy, true, &shutdown).await;
        assert!(result.is_ok());
        assert!(
            dream_running.load(Ordering::SeqCst),
            "a successful decision claims single-flight for its caller"
        );
    }

    #[tokio::test]
    async fn decide_waits_for_heavy_pass_without_claiming_single_flight_early() {
        let dream_running = Arc::new(AtomicBool::new(false));
        let heavy = Arc::new(Semaphore::new(1));
        let held = heavy.clone().try_acquire_owned().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let waiter = {
            let dream_running = dream_running.clone();
            let heavy = heavy.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move { decide(&dream_running, &heavy, true, &shutdown).await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        assert!(!dream_running.load(Ordering::SeqCst));
        drop(held);
        let permit = waiter
            .await
            .unwrap()
            .expect("dream should acquire after heavy pass");
        assert!(dream_running.load(Ordering::SeqCst));
        drop(permit);
    }

    #[tokio::test]
    async fn decide_not_due_leaves_flag_untouched() {
        let dream_running = AtomicBool::new(false);
        let heavy = Arc::new(tokio::sync::Semaphore::new(1));
        let shutdown = AtomicBool::new(false);
        let result = decide(&dream_running, &heavy, false, &shutdown).await;
        assert!(matches!(result, Err(SkipReason::NotDue)));
        assert!(!dream_running.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn watcher_waits_until_dream_releases_heavy_work_permit() {
        let heavy = Arc::new(tokio::sync::Semaphore::new(1));
        let dream_permit = heavy.clone().try_acquire_owned().unwrap();
        let waiter = tokio::spawn(crate::import::watcher::acquire_heavy_work_permit(
            heavy.clone(),
        ));
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(dream_permit);
        let watcher_permit = waiter.await.unwrap();
        assert_eq!(heavy.available_permits(), 0);
        drop(watcher_permit);
        assert_eq!(heavy.available_permits(), 1);
    }

    #[tokio::test]
    async fn overdue_dream_joins_fair_queue_under_watcher_churn() {
        let heavy = Arc::new(Semaphore::new(1));
        let initial_watcher =
            crate::import::watcher::acquire_heavy_work_permit(heavy.clone()).await;
        let dream_running = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let dream = {
            let heavy = heavy.clone();
            let dream_running = dream_running.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move { decide(&dream_running, &heavy, true, &shutdown).await })
        };
        tokio::task::yield_now().await;

        let watcher_churn = {
            let heavy = heavy.clone();
            tokio::spawn(async move {
                for _ in 0..32 {
                    let permit =
                        crate::import::watcher::acquire_heavy_work_permit(heavy.clone()).await;
                    tokio::task::yield_now().await;
                    drop(permit);
                }
            })
        };
        drop(initial_watcher);

        // Await the dream directly under a generous wall-clock bound instead of a
        // fixed yield budget: on loaded CI runners 64 yields is not enough for the
        // fair queue to drain the watcher churn, and the count is not the invariant —
        // "finishes at all, promptly" is.
        let dream_permit = tokio::time::timeout(std::time::Duration::from_secs(30), dream)
            .await
            .expect("overdue dream must not starve behind watcher churn")
            .unwrap()
            .expect("overdue dream should acquire");
        assert!(dream_running.load(Ordering::SeqCst));
        drop(dream_permit);
        watcher_churn.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn queued_dream_abandons_wait_when_kill_switch_turns_on() {
        let _guard = env_guard();
        std::env::remove_var("CSR_NO_DREAMING");
        let heavy = Arc::new(Semaphore::new(1));
        let held = heavy.clone().try_acquire_owned().unwrap();
        let dream_running = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let waiter = {
            let heavy = heavy.clone();
            let dream_running = dream_running.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move { decide(&dream_running, &heavy, true, &shutdown).await })
        };
        tokio::task::yield_now().await;
        std::env::set_var("CSR_NO_DREAMING", "1");

        let result = tokio::time::timeout(StdDuration::from_secs(2), waiter)
            .await
            .expect("kill-switch recheck must bound the queued wait")
            .unwrap();
        assert!(matches!(result, Err(SkipReason::Disabled)));
        assert!(!dream_running.load(Ordering::SeqCst));
        assert_eq!(heavy.available_permits(), 0);
        drop(held);
        std::env::remove_var("CSR_NO_DREAMING");
    }

    #[test]
    fn cancellation_rechecks_shutdown_and_kill_switch_during_a_run() {
        let _guard = env_guard();
        std::env::remove_var("CSR_NO_DREAMING");
        let shutdown = Arc::new(AtomicBool::new(false));
        let cancellation = crate::dream::DreamCancellation::new(shutdown.clone());
        assert!(!cancellation.is_cancelled());
        shutdown.store(true, Ordering::SeqCst);
        assert!(cancellation.is_cancelled());
        shutdown.store(false, Ordering::SeqCst);
        std::env::set_var("CSR_NO_DREAMING", "1");
        assert!(cancellation.is_cancelled());
        std::env::remove_var("CSR_NO_DREAMING");
    }

    #[test]
    fn cancellation_stops_between_anchors_and_reports_partial_progress() {
        let storage = Storage::open_memory().unwrap();
        for file in ["/missing/one.rs", "/missing/two.rs"] {
            for (stamp, oid) in [("b3:a", "a"), ("b3:b", "b")] {
                storage
                    .insert_witness(&witness_ledger::WitnessLedgerRow {
                        project: "project".into(),
                        file: file.into(),
                        symbol: Some("symbol".into()),
                        stamp: stamp.into(),
                        tier: "committed".into(),
                        at_oid: Some(oid.into()),
                        source_kind: "test".into(),
                        source_id: Some(format!("{file}:{oid}")),
                        ..Default::default()
                    })
                    .unwrap();
            }
        }
        let checks = std::cell::Cell::new(0_u32);
        let cancel_after_first_anchor = || {
            let current = checks.get();
            checks.set(current + 1);
            current >= 1
        };
        let mut stats = crate::dream::DreamStats::default();
        let cancelled = storage
            .with_connection(|conn| {
                crate::dream::dream_join_cancellable(
                    conn,
                    None,
                    false,
                    &mut stats,
                    Some(&cancel_after_first_anchor),
                )
            })
            .unwrap();
        assert!(cancelled);
        assert_eq!(stats.abstained_no_repo, 1, "first anchor completed");
        assert_eq!(checks.get(), 2, "cancelled at the next anchor checkpoint");
    }
}
