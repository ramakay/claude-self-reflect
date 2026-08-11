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
/// `meta` key: which trigger started the last completed cycle — `"idle"` or
/// `"nightly_floor"` (see [`Trigger`]). Written only on completion, so it
/// always describes a pass that actually happened.
pub const META_LAST_TRIGGER: &str = "dream_daemon_last_trigger";
/// `meta` key: JSON `{cap, used, queued}` from the last completed cycle's
/// invocation budget (`dream::policy::Budget`).
pub const META_LAST_BUDGET: &str = "dream_daemon_last_budget";
/// `meta` key: small JSON stats summary from the last completed cycle
/// (informational only — never read back by cadence math, only by humans
/// debugging via `sqlite3 ... "select value from meta where key = ...`).
pub const META_LAST_STATS: &str = "dream_daemon_last_stats";

/// `meta` key: the user's answer to the setup consent screen (locked
/// decision 15) — `"granted"` or `"declined"`. Absent means never asked,
/// which is NOT a decline: dreaming is on by default and the toggle is
/// presented pre-selected, so only an explicit decline turns it off.
pub const META_CONSENT: &str = "dream_consent";
/// Value written when the user declines dreaming at setup.
pub const CONSENT_DECLINED: &str = "declined";
/// Value written when the user accepts (or accepts the default).
pub const CONSENT_GRANTED: &str = "granted";

/// Did the user explicitly decline dreaming at setup? Absence of a record is
/// never read as a decline.
pub fn consent_declined(storage: &Storage) -> bool {
    storage
        .get_meta(META_CONSENT)
        .ok()
        .flatten()
        .map(|value| value.trim() == CONSENT_DECLINED)
        .unwrap_or(false)
}

/// Record the setup consent decision.
pub fn record_consent(storage: &Storage, granted: bool) -> Result<()> {
    storage.set_meta(
        META_CONSENT,
        if granted {
            CONSENT_GRANTED
        } else {
            CONSENT_DECLINED
        },
    )
}

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

// ─── idle detection + nightly floor (Journal v4 P5, locked decision 5) ────
//
// A pass must not land in the middle of a working session, so the primary
// trigger is IDLENESS: no session/transcript write for `idle_secs`. Idleness
// is derived from state CSR already keeps — `import_state.file_mtime` (the
// mtime of every transcript the watcher has imported) and
// `session_registry.last_ts` (the history spine's newest prompt) — rather
// than from a new tracker that could disagree with them.
//
// Idleness alone would never fire on a machine that is never quiet, so a
// NIGHTLY FLOOR marks a pass OWED once the local floor hour has passed with
// no completed cycle since. Owed is not the same as running: the floor pass
// still waits for a witnessed idle interval before it starts, because git,
// SQLite, AST and (opt-in) model work landing in the middle of a live session
// is exactly what the daemon-safety gate forbids. A floor pass that is owed
// while the machine is busy is DEFERRED and reported as overdue in
// `csr-engine status` (`cadence.floor_deferred_active_session`) — the honest
// statement is "owed, waiting for quiet", never a pass forced into a working
// session.

/// Default idleness required before an idle-triggered pass: 30 minutes.
pub const DEFAULT_IDLE_MINS: u64 = 30;
/// Documented override for [`DEFAULT_IDLE_MINS`].
pub const IDLE_ENV: &str = "CSR_DREAM_IDLE_MINS";
/// Default local hour of the nightly floor pass (03:00 local).
pub const DEFAULT_FLOOR_HOUR: u32 = 3;
/// Documented override for [`DEFAULT_FLOOR_HOUR`] (0–23).
pub const FLOOR_HOUR_ENV: &str = "CSR_DREAM_FLOOR_HOUR";

/// What started a cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Claude Code has been quiet for at least the idle threshold.
    Idle,
    /// No pass has completed since the most recent nightly floor boundary.
    NightlyFloor,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Idle => "idle",
            Trigger::NightlyFloor => "nightly_floor",
        }
    }
}

/// Configured idle threshold in seconds. A non-positive or unparseable value
/// falls back to [`DEFAULT_IDLE_MINS`].
pub fn idle_secs() -> u64 {
    idle_secs_from(std::env::var(IDLE_ENV).ok().as_deref())
}

/// Pure core of [`idle_secs`].
pub fn idle_secs_from(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|&mins| mins > 0 && mins <= 24 * 60)
        .unwrap_or(DEFAULT_IDLE_MINS)
        * 60
}

/// Configured nightly floor hour, local time.
pub fn floor_hour() -> u32 {
    floor_hour_from(std::env::var(FLOOR_HOUR_ENV).ok().as_deref())
}

/// Pure core of [`floor_hour`]. Anything outside 0–23 falls back.
pub fn floor_hour_from(raw: Option<&str>) -> u32 {
    raw.and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|&hour| hour < 24)
        .unwrap_or(DEFAULT_FLOOR_HOUR)
}

/// The newest session/transcript write CSR has observed, from state it
/// already keeps: `MAX(import_state.file_mtime)` and
/// `MAX(session_registry.last_ts)`. `None` when neither table has a parseable
/// timestamp — which is NOT read as "idle" (see [`is_idle`]).
pub fn last_activity_at(storage: &Storage) -> Option<DateTime<Utc>> {
    let raw: Vec<String> = storage
        .with_connection(|conn| {
            let mut out = Vec::new();
            for sql in [
                "SELECT MAX(file_mtime) FROM import_state",
                "SELECT MAX(last_ts) FROM session_registry",
            ] {
                // Fail-soft per source: a pre-migration schema gap must not
                // wedge the cadence decision.
                if let Ok(Some(value)) =
                    conn.query_row(sql, [], |row| row.get::<_, Option<String>>(0))
                {
                    out.push(value);
                }
            }
            Ok(out)
        })
        .unwrap_or_default();
    raw.iter()
        .filter_map(|value| crate::temporal::parse_timestamp(value.trim()))
        .max()
}

/// Has Claude Code been quiet long enough? `last_activity = None` is
/// deliberately **not** idle: no observed write is absence of evidence, and
/// the nightly floor already guarantees a pass without having to infer
/// quiet from silence.
pub fn is_idle(last_activity: Option<DateTime<Utc>>, now: DateTime<Utc>, idle_secs: u64) -> bool {
    let Some(last) = last_activity else {
        return false;
    };
    let seconds = i64::try_from(idle_secs).unwrap_or(i64::MAX);
    now.signed_duration_since(last) >= Duration::seconds(seconds)
}

/// The most recent occurrence of `floor_hour` at or before `now`, in whatever
/// timescale `now` is expressed in (the caller passes local time).
pub fn floor_boundary(now: chrono::NaiveDateTime, floor_hour: u32) -> chrono::NaiveDateTime {
    let today = now
        .date()
        .and_hms_opt(floor_hour.min(23), 0, 0)
        .unwrap_or(now);
    if today <= now {
        today
    } else {
        today - Duration::days(1)
    }
}

/// Is the nightly floor pass owed? True when no cycle has completed since the
/// most recent floor boundary — including a machine that has never completed
/// one at all, which by definition has not passed since that boundary.
pub fn floor_due(
    last_run: Option<chrono::NaiveDateTime>,
    now: chrono::NaiveDateTime,
    floor_hour: u32,
) -> bool {
    match last_run {
        Some(last) => last < floor_boundary(now, floor_hour),
        None => true,
    }
}

/// Config for [`choose_trigger`], resolved once per decision.
#[derive(Debug, Clone, Copy)]
pub struct CadenceConfig {
    pub interval_secs: u64,
    pub idle_secs: u64,
    pub floor_hour: u32,
}

impl CadenceConfig {
    /// Read every knob from the environment.
    pub fn from_env() -> Self {
        Self {
            interval_secs: interval_secs(),
            idle_secs: idle_secs(),
            floor_hour: floor_hour(),
        }
    }
}

/// What the cadence decided, including the case that is neither "run" nor
/// "nothing due": a floor pass that is owed but must wait for quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CadenceDecision {
    /// A pass may start now, on this trigger.
    Run(Trigger),
    /// The nightly floor is owed and the machine is NOT idle. Nothing starts;
    /// the debt is real and is surfaced as overdue in `status` until an idle
    /// window lets it run.
    FloorOwedDeferred,
    /// Nothing is due.
    Wait,
}

impl CadenceDecision {
    /// The trigger to run on, if any.
    pub fn trigger(self) -> Option<Trigger> {
        match self {
            CadenceDecision::Run(trigger) => Some(trigger),
            _ => None,
        }
    }
}

/// The whole cadence decision, pure and testable: which trigger (if any)
/// makes a cycle due right now.
///
/// * **Idle** requires BOTH the cadence interval to have elapsed since the
///   last completed cycle AND the machine to have been quiet for
///   `idle_secs`. Mid-session therefore never fires.
/// * **Nightly floor** becomes OWED when no cycle has completed since the
///   most recent floor boundary — but it runs only once the machine has also
///   been quiet for `idle_secs`. An owed floor pass on a busy machine returns
///   [`CadenceDecision::FloorOwedDeferred`]: no git/SQLite/AST/model work is
///   started underneath a live session, and the debt is reported rather than
///   forced. It stays owed (`floor_due` keeps returning true) until a pass
///   actually completes, so nothing is lost — only postponed to quiet.
///
/// `now_local` is the same instant as `now` expressed in local time; it is
/// passed in rather than computed so the decision is deterministic under
/// test.
pub fn choose_trigger(
    last_activity: Option<DateTime<Utc>>,
    last_run: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    now_local: chrono::NaiveDateTime,
    last_run_local: Option<chrono::NaiveDateTime>,
    config: CadenceConfig,
) -> CadenceDecision {
    let idle = is_idle(last_activity, now, config.idle_secs);
    if is_due(last_run, now, config.interval_secs) && idle {
        return CadenceDecision::Run(Trigger::Idle);
    }
    if floor_due(last_run_local, now_local, config.floor_hour) {
        return if idle {
            CadenceDecision::Run(Trigger::NightlyFloor)
        } else {
            CadenceDecision::FloorOwedDeferred
        };
    }
    CadenceDecision::Wait
}

/// [`choose_trigger`] against live storage and the system clock.
pub fn current_trigger(storage: &Storage, config: CadenceConfig) -> CadenceDecision {
    let now = Utc::now();
    let last_run = read_last_run(storage);
    choose_trigger(
        last_activity_at(storage),
        last_run,
        now,
        now.with_timezone(&chrono::Local).naive_local(),
        last_run.map(|t| t.with_timezone(&chrono::Local).naive_local()),
        config,
    )
}

/// Is the nightly floor pass owed but held back by an active session? Both
/// inputs are measured (`floor_due` from the persisted last completed cycle,
/// idleness from observed transcript/registry writes) — this is never
/// rendered from absence of evidence, and `last_activity = None` is not idle.
pub fn floor_deferred_for_activity(
    last_activity: Option<DateTime<Utc>>,
    last_run_local: Option<chrono::NaiveDateTime>,
    now: DateTime<Utc>,
    now_local: chrono::NaiveDateTime,
    config: CadenceConfig,
) -> bool {
    floor_due(last_run_local, now_local, config.floor_hour)
        && !is_idle(last_activity, now, config.idle_secs)
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
    trigger: Trigger,
    budget: &crate::dream::policy::BudgetSnapshot,
) -> Result<()> {
    storage.set_meta(META_LAST_RUN_AT, &completed_at.to_rfc3339())?;
    if let Err(error) = storage.set_meta(META_LAST_TRIGGER, trigger.as_str()) {
        tracing::warn!(%error, "dream cycle trigger persistence failed (non-fatal)");
    }
    let budget_summary = serde_json::json!({
        "cap": budget.cap,
        "used": budget.used,
        "queued": budget.queued,
    })
    .to_string();
    if let Err(error) = storage.set_meta(META_LAST_BUDGET, &budget_summary) {
        tracing::warn!(%error, "dream cycle budget persistence failed (non-fatal)");
    }
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
    trigger: Trigger,
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
    // One budget per pass, shared by every model-invoking producer below, so
    // the cap in locked decision 8 is a pass total rather than a per-producer
    // one — and every invocation through it is debited from the durable NIGHT
    // ledger (`dream::policy::Budget::for_night`), so several ticks in one
    // night, and retries after a daemon restart, all draw down ONE nightly
    // allowance instead of each receiving a fresh cap. The deterministic
    // dream cycle itself spends nothing — it invokes no model at all.
    let tier = crate::dream::policy::effort_tier_counted(engine.storage());
    let night = crate::dream::policy::current_night_key();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let budget = crate::dream::policy::Budget::for_night(eng.storage(), tier, &night);
        let dream_result =
            crate::dream::run_dream_with_cancellation(&eng, None, false, &cancellation);
        // Journal v3 Phase 1.5 — night-pass thread extraction. Runs on the
        // same cadence tick as the deterministic dream cycle, best-effort
        // and independent of its outcome: `run_thread_extraction` is
        // fail-open internally (its own kill switches, per-episode error
        // handling) and must never mask or block the dream cycle's result.
        // Skipped only when this tick is itself being cancelled.
        if !cancellation.is_cancelled() {
            crate::dream::threads::run_thread_extraction_with_budget(eng.storage(), &budget);
        }
        // Journal v4 Phase 4 — structured plan proposals, on the same pass
        // and the same budget, gated by the same opt-in switches.
        if !cancellation.is_cancelled() {
            crate::journal::composer::run_plan_pass_with_budget(eng.storage(), &budget);
        }
        // The badge baseline is deliberately NOT refreshed here: a pass that
        // fails must not publish a fresh "measured" timestamp. It is
        // refreshed only after `DreamRunResult::Complete` and successful
        // completion-metadata persistence, below.
        (dream_result, budget.snapshot())
    })
    .await;
    let (result, budget) = match result {
        Ok((dream_result, snapshot)) => (Ok(dream_result), snapshot),
        Err(join_error) => (
            Err(join_error),
            crate::dream::policy::BudgetSnapshot::default(),
        ),
    };
    let outcome = match result {
        Ok(Ok(crate::dream::DreamRunResult::Complete(stats))) => {
            match record_completed_cycle(engine.storage(), &stats, Utc::now(), trigger, &budget) {
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
    // Journal v4 Phase 5 — the statusline badge baseline is published here
    // and nowhere else, because its timestamp and count assert that a PASS
    // measured them. A pass that failed, was cancelled, or could not persist
    // its own completion has measured nothing, and must leave the previous
    // baseline exactly as it was (see `refresh_badge_after`).
    let badge_engine = engine.clone();
    if let Err(error) =
        tokio::task::spawn_blocking(move || refresh_badge_after(badge_engine.storage(), outcome))
            .await
    {
        tracing::warn!(%error, "dream badge baseline refresh failed (non-fatal)");
    }
    dream_running.store(false, Ordering::SeqCst);
    outcome
}

/// Refresh the statusline badge baseline **only** for a pass that completed
/// and whose completion metadata persisted ([`TickOutcome::Success`]).
///
/// The badge carries a measured timestamp and count; publishing one after a
/// failed pass would claim that unread conclusions were measured by a pass
/// that never finished. Every other outcome leaves the previous baseline
/// untouched — stale and honestly dated, rather than fresh and unfounded.
fn refresh_badge_after(storage: &Storage, outcome: TickOutcome) {
    if outcome != TickOutcome::Success {
        return;
    }
    crate::storage::dream_delivery::refresh_badge_baseline(storage);
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
    if consent_declined(engine.storage()) {
        tracing::info!("dream cycle disabled: dreaming was declined at setup");
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
            // The monotonic deadline bounds how often we even LOOK; the
            // trigger decision (idle vs nightly floor) decides whether a pass
            // may actually land right now. A deadline that has elapsed while
            // the user is mid-session simply re-arms on the poll interval.
            let decision = current_trigger(engine.storage(), CadenceConfig::from_env());
            if decision == CadenceDecision::FloorOwedDeferred {
                // Owed, not abandoned: `floor_due` keeps returning true, so
                // the next poll that finds the machine quiet runs it. Logged
                // at debug because it repeats every poll while a session is
                // live; `status` carries the standing overdue state.
                tracing::debug!(
                    "nightly dream floor is owed but the session is active; deferring to the next idle window"
                );
            }
            let Some(trigger) = decision.trigger() else {
                deadline = tokio::time::Instant::now() + StdDuration::from_secs(POLL_INTERVAL_SECS);
                tokio::time::sleep(
                    deadline
                        .saturating_duration_since(tokio::time::Instant::now())
                        .min(StdDuration::from_secs(10)),
                )
                .await;
                continue;
            };
            match tick(&engine, &dream_running, &heavy_work, &shutdown, trigger).await {
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
        let result = record_completed_cycle(
            &storage,
            &crate::dream::DreamStats::default(),
            ts(0),
            Trigger::NightlyFloor,
            &crate::dream::policy::Budget::new(1).snapshot(),
        );
        assert!(result.is_err());
        assert!(read_last_run(&storage).is_none());
    }

    #[test]
    fn a_failed_pass_leaves_the_badge_baseline_untouched() {
        use crate::storage::dream_delivery::{badge_measured_at, badge_unread};
        let storage = Storage::open_memory().unwrap();

        // Nothing has ever measured a baseline.
        assert_eq!(badge_measured_at(&storage), None);
        for outcome in [
            TickOutcome::Failed,
            TickOutcome::Cancelled,
            TickOutcome::Skipped(SkipReason::AlreadyRunning),
            TickOutcome::Skipped(SkipReason::NotDue),
        ] {
            refresh_badge_after(&storage, outcome);
            assert_eq!(
                badge_measured_at(&storage),
                None,
                "{outcome:?} published a baseline no pass measured"
            );
            assert_eq!(badge_unread(&storage), None);
        }

        // A completed pass may publish one.
        refresh_badge_after(&storage, TickOutcome::Success);
        let measured = badge_measured_at(&storage).expect("a completed pass measures a baseline");

        // A later failure must not re-date it.
        refresh_badge_after(&storage, TickOutcome::Failed);
        assert_eq!(
            badge_measured_at(&storage),
            Some(measured),
            "a failing pass must not restamp a baseline it did not measure"
        );
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

    // ── idle detection + nightly floor (Journal v4 P5) ──

    fn naive(text: &str) -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn cfg(interval_secs: u64, idle_secs: u64) -> CadenceConfig {
        CadenceConfig {
            interval_secs,
            idle_secs,
            floor_hour: 3,
        }
    }

    #[test]
    fn is_idle_requires_a_witnessed_quiet_period() {
        let now = ts(0);
        assert!(
            !is_idle(Some(now - Duration::seconds(60)), now, 1800),
            "one minute of quiet is not idle"
        );
        assert!(is_idle(Some(now - Duration::seconds(1800)), now, 1800));
        assert!(
            !is_idle(None, now, 1800),
            "no observed activity is not evidence of idleness"
        );
    }

    #[test]
    fn idle_threshold_and_floor_hour_fall_back_on_garbage() {
        assert_eq!(idle_secs_from(None), DEFAULT_IDLE_MINS * 60);
        assert_eq!(idle_secs_from(Some("5")), 300);
        assert_eq!(idle_secs_from(Some("0")), DEFAULT_IDLE_MINS * 60);
        assert_eq!(idle_secs_from(Some("nope")), DEFAULT_IDLE_MINS * 60);
        assert_eq!(
            idle_secs_from(Some("100000")),
            DEFAULT_IDLE_MINS * 60,
            "beyond a day is not a plausible idle threshold"
        );
        assert_eq!(floor_hour_from(None), DEFAULT_FLOOR_HOUR);
        assert_eq!(floor_hour_from(Some("4")), 4);
        assert_eq!(floor_hour_from(Some("24")), DEFAULT_FLOOR_HOUR);
        assert_eq!(floor_hour_from(Some("-1")), DEFAULT_FLOOR_HOUR);
    }

    #[test]
    fn floor_boundary_is_the_most_recent_local_floor_hour() {
        assert_eq!(
            floor_boundary(naive("2026-08-11 09:00:00"), 3),
            naive("2026-08-11 03:00:00"),
        );
        assert_eq!(
            floor_boundary(naive("2026-08-11 01:00:00"), 3),
            naive("2026-08-10 03:00:00"),
            "before today's hour, the boundary is yesterday's"
        );
        assert_eq!(
            floor_boundary(naive("2026-08-11 03:00:00"), 3),
            naive("2026-08-11 03:00:00"),
            "exactly on the hour counts as today's boundary"
        );
    }

    #[test]
    fn an_idle_machine_past_its_interval_dreams_on_the_idle_trigger() {
        let now = ts(0);
        let trigger = choose_trigger(
            Some(now - Duration::hours(2)), // quiet for two hours
            Some(now - Duration::hours(7)), // last pass seven hours ago
            now,
            naive("2026-08-11 09:00:00"),
            Some(naive("2026-08-11 04:00:00")), // after today's 03:00 floor
            cfg(6 * 3600, 1800),
        );
        assert_eq!(trigger, CadenceDecision::Run(Trigger::Idle));
    }

    #[test]
    fn a_mid_session_machine_never_fires_the_idle_trigger() {
        let now = ts(0);
        let trigger = choose_trigger(
            Some(now - Duration::seconds(30)), // typing right now
            Some(now - Duration::hours(7)),    // cadence is long overdue
            now,
            naive("2026-08-11 09:00:00"),
            Some(naive("2026-08-11 04:00:00")), // floor already satisfied today
            cfg(6 * 3600, 1800),
        );
        assert_eq!(
            trigger,
            CadenceDecision::Wait,
            "an overdue interval must not drag a pass into a live session"
        );
    }

    #[test]
    fn the_nightly_floor_fires_once_the_machine_goes_quiet() {
        let now = ts(0);
        // Inside the cadence interval (7h of an 8h interval), so the idle
        // trigger cannot fire — only the floor can.
        let trigger = choose_trigger(
            Some(now - Duration::hours(1)), // quiet for an hour
            Some(now - Duration::hours(7)),
            now,
            naive("2026-08-11 09:00:00"),
            Some(naive("2026-08-11 02:00:00")), // before today's 03:00 floor
            cfg(8 * 3600, 1800),
        );
        assert_eq!(
            trigger,
            CadenceDecision::Run(Trigger::NightlyFloor),
            "an owed floor pass must run at the first witnessed idle window"
        );
    }

    #[test]
    fn an_owed_floor_pass_defers_instead_of_landing_in_a_live_session() {
        let now = ts(0);
        // 7h into an 8h cadence interval, past today's 03:00 floor: the floor
        // is owed and nothing else can fire.
        let last_run_local = Some(naive("2026-08-11 02:00:00"));
        let busy = choose_trigger(
            Some(now - Duration::seconds(5)), // typing right now
            Some(now - Duration::hours(7)),
            now,
            naive("2026-08-11 09:00:00"),
            last_run_local,
            cfg(8 * 3600, 1800),
        );
        assert_eq!(
            busy,
            CadenceDecision::FloorOwedDeferred,
            "the floor boundary must not start git/SQLite/AST/model work under a live session"
        );
        assert_eq!(busy.trigger(), None, "a deferred pass must not start");
        assert!(
            floor_deferred_for_activity(
                Some(now - Duration::seconds(5)),
                last_run_local,
                now,
                naive("2026-08-11 09:00:00"),
                cfg(8 * 3600, 1800),
            ),
            "status must be able to report the debt while it is deferred"
        );

        // The debt survives: the same state, once quiet, runs the owed pass.
        let quiet = choose_trigger(
            Some(now - Duration::hours(2)),
            Some(now - Duration::hours(7)),
            now,
            naive("2026-08-11 09:00:00"),
            last_run_local,
            cfg(8 * 3600, 1800),
        );
        assert_eq!(quiet, CadenceDecision::Run(Trigger::NightlyFloor));
        assert!(!floor_deferred_for_activity(
            Some(now - Duration::hours(2)),
            last_run_local,
            now,
            naive("2026-08-11 09:00:00"),
            cfg(8 * 3600, 1800),
        ));
    }

    #[test]
    fn an_unobserved_machine_is_never_treated_as_quiet_enough_for_the_floor() {
        let now = ts(0);
        let trigger = choose_trigger(
            None, // nothing observed at all
            Some(now - Duration::hours(20)),
            now,
            naive("2026-08-11 09:00:00"),
            Some(naive("2026-08-10 13:00:00")),
            cfg(6 * 3600, 1800),
        );
        assert_eq!(
            trigger,
            CadenceDecision::FloorOwedDeferred,
            "absence of observed activity is not evidence of idleness"
        );
    }

    #[test]
    fn a_machine_that_has_never_dreamed_is_owed_the_floor_pass() {
        assert!(floor_due(None, naive("2026-08-11 09:00:00"), 3));
        assert!(!floor_due(
            Some(naive("2026-08-11 04:00:00")),
            naive("2026-08-11 09:00:00"),
            3
        ));
        assert!(floor_due(
            Some(naive("2026-08-11 02:59:59")),
            naive("2026-08-11 09:00:00"),
            3
        ));
    }

    #[test]
    fn an_idle_machine_inside_its_cadence_interval_waits() {
        let now = ts(0);
        let trigger = choose_trigger(
            Some(now - Duration::hours(3)),   // idle
            Some(now - Duration::minutes(5)), // but it just dreamed
            now,
            naive("2026-08-11 09:00:00"),
            Some(naive("2026-08-11 08:55:00")),
            cfg(6 * 3600, 1800),
        );
        assert_eq!(
            trigger,
            CadenceDecision::Wait,
            "idleness does not override the cadence interval"
        );
    }

    #[test]
    fn last_activity_reads_the_newest_of_import_state_and_the_registry() {
        let storage = Storage::open_memory().unwrap();
        assert!(
            last_activity_at(&storage).is_none(),
            "an empty database witnesses no activity"
        );
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO import_state (file_path, conversation_id, chunks_imported, file_mtime)
                     VALUES ('/tmp/a.jsonl', 'conv-a', 1, '2026-08-11T09:00:00Z')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO session_registry (session_id, project, first_ts, last_ts, prompt_count)
                     VALUES ('s1', 'proj', '2026-08-11T07:00:00Z', '2026-08-11T11:30:00Z', 3)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let newest = last_activity_at(&storage).expect("both sources are parseable");
        assert_eq!(newest.to_rfc3339(), "2026-08-11T11:30:00+00:00");
    }

    #[test]
    fn the_completed_cycle_records_its_trigger_and_budget() {
        let storage = Storage::open_memory().unwrap();
        let budget = crate::dream::policy::Budget::new(4);
        assert!(budget.try_spend());
        budget.note_queued();
        record_completed_cycle(
            &storage,
            &crate::dream::DreamStats::default(),
            ts(0),
            Trigger::Idle,
            &budget.snapshot(),
        )
        .unwrap();
        assert_eq!(
            storage.get_meta(META_LAST_TRIGGER).unwrap(),
            Some("idle".to_string())
        );
        let recorded = storage.get_meta(META_LAST_BUDGET).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&recorded).unwrap();
        assert_eq!(parsed["cap"], 4);
        assert_eq!(parsed["used"], 1);
        assert_eq!(parsed["queued"], 1);
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
    #[allow(clippy::await_holding_lock)]
    async fn decide_skips_when_already_running() {
        let _guard = env_guard();
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
    #[allow(clippy::await_holding_lock)]
    async fn decide_claims_the_flag_on_success() {
        let _guard = env_guard();
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
    #[allow(clippy::await_holding_lock)]
    async fn decide_waits_for_heavy_pass_without_claiming_single_flight_early() {
        let _guard = env_guard();
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
    #[allow(clippy::await_holding_lock)]
    async fn decide_not_due_leaves_flag_untouched() {
        let _guard = env_guard();
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
    #[allow(clippy::await_holding_lock)]
    async fn overdue_dream_joins_fair_queue_under_watcher_churn() {
        // decide() reads the process-global CSR_NO_DREAMING kill switch; hold
        // the env lock so a parallel kill-switch test can't flip it mid-run
        // (this is exactly the CI failure mode: Disabled returned instead of
        // a permit).
        let _guard = env_guard();
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
