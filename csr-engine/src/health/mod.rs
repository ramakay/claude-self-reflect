//! Memory health: a stable known-item recall probe, a report-only duplicate
//! rate, and the cached results `status` and `eval --full` read.
//!
//! Everything persisted here is diagnostic `meta` text (three keys, no new
//! table, no migration). The probe never writes a chunk, reflection,
//! embedding, retrieval event, exposure or reaction label.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::{RwLock, Semaphore};

use crate::search::SearchEngine;
use crate::storage::{queries, Storage};

pub mod probe;

/// Latest canary, duplicate and probe results, with timestamps.
pub const META_HEALTH: &str = "memory_health_v1";
/// The frozen probe sample and its queries.
pub const META_SAMPLE: &str = "memory_health_probe_sample_v1";
/// The last seven completed runs.
pub const META_RUNS: &str = "memory_health_runs_v1";

/// How many completed runs the bounded history keeps.
pub const RUN_HISTORY: usize = 7;
/// Miss diagnostics kept per history record.
pub const MISSES_PER_RUN: usize = 20;
/// The measurement cadence: a run is due once the last attempt is this old.
pub const HEALTH_INTERVAL_HOURS: i64 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeGrade {
    Green,
    Amber,
    Red,
    Abstain,
    Error,
}

impl ProbeGrade {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeGrade::Green => "green",
            ProbeGrade::Amber => "amber",
            ProbeGrade::Red => "red",
            ProbeGrade::Abstain => "abstain",
            ProbeGrade::Error => "error",
        }
    }
}

/// Why an expected conversation was not in the first five results. Never a
/// claim that ranking regressed: `NotInTop5` only says it was absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissCause {
    /// No SQLite embedding for the sampled chunk.
    EmbeddingMissing,
    /// An embedding exists but the search index does not hold the chunk.
    IndexMissing,
    /// Embedding and index entry exist; the conversation was not in the page.
    NotInTop5,
}

/// A miss: identifiers and the returned conversation ids, never source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeMiss {
    pub chunk_id: String,
    pub conversation_id: String,
    pub returned: Vec<String>,
    pub cause: MissCause,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeCase {
    pub chunk_id: String,
    pub conversation_id: String,
    pub parent_conversation_id: Option<String>,
    pub content_hash: String,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSample {
    pub version: u32,
    pub sample_id: String,
    pub created_at: String,
    pub cutoff_at: String,
    pub cases: Vec<ProbeCase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    pub at: String,
    pub sample_id: Option<String>,
    pub hits: usize,
    pub n: usize,
    pub invalidated: usize,
    pub grade: ProbeGrade,
    pub misses: Vec<ProbeMiss>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicateReport {
    pub at: String,
    pub total: u64,
    pub distinct: u64,
    pub duplicate_rows: u64,
    /// `None` on an empty population: there is no rate to report.
    pub rate: Option<f64>,
}

/// The canary verdict for the binary that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanaryRecord {
    pub at: String,
    pub passed: bool,
    pub detail: String,
}

/// What `memory_health_v1` holds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HealthState {
    pub version: u32,
    /// When the last measurement attempt ran, completed or not. Drives cadence.
    pub last_attempt_at: Option<String>,
    /// Engine version and search configuration the numbers were taken under.
    pub config: Option<String>,
    pub canary: Option<CanaryRecord>,
    pub duplicates: Option<DuplicateReport>,
    /// The latest COMPLETED probe (green, amber, red or abstain). An error
    /// never replaces it.
    pub probe: Option<ProbeReport>,
    /// The latest attempt when it errored; cleared by the next completed run.
    pub last_probe_error: Option<ProbeReport>,
}

/// One entry of the bounded history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub at: String,
    pub sample_id: Option<String>,
    pub hits: usize,
    pub n: usize,
    pub invalidated: usize,
    pub grade: ProbeGrade,
    pub misses: Vec<ProbeMiss>,
    pub duplicate_rate: Option<f64>,
}

impl RunRecord {
    pub fn from_probe(report: &ProbeReport, duplicate_rate: Option<f64>) -> Self {
        Self {
            at: report.at.clone(),
            sample_id: report.sample_id.clone(),
            hits: report.hits,
            n: report.n,
            invalidated: report.invalidated,
            grade: report.grade,
            misses: report.misses.iter().take(MISSES_PER_RUN).cloned().collect(),
            duplicate_rate,
        }
    }
}

/// Everything the three meta keys hold.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HealthSnapshot {
    pub state: HealthState,
    pub runs: Vec<RunRecord>,
    pub sample: Option<ProbeSample>,
}

fn read_json<T: for<'de> Deserialize<'de>>(conn: &Connection, key: &str) -> Result<Option<T>> {
    match queries::get_meta(conn, key)? {
        Some(raw) => Ok(Some(
            serde_json::from_str(&raw).with_context(|| format!("meta {key} is not valid"))?,
        )),
        None => Ok(None),
    }
}

/// Read the cached health meta. Missing keys are an empty snapshot, never an
/// error: a fresh install has measured nothing.
pub fn load_health(storage: &Storage) -> Result<HealthSnapshot> {
    storage.with_connection(|conn| {
        Ok(HealthSnapshot {
            state: read_json(conn, META_HEALTH)?.unwrap_or_default(),
            runs: read_json(conn, META_RUNS)?.unwrap_or_default(),
            sample: read_json(conn, META_SAMPLE)?,
        })
    })
}

/// Persist the health meta in one transaction. The history is bounded to the
/// last seven records here, so no caller can grow it.
pub fn save_health(storage: &Storage, snapshot: &HealthSnapshot) -> Result<()> {
    let mut runs = snapshot.runs.clone();
    if runs.len() > RUN_HISTORY {
        runs.drain(..runs.len() - RUN_HISTORY);
    }
    let state = serde_json::to_string(&snapshot.state)?;
    let runs = serde_json::to_string(&runs)?;
    let sample = snapshot
        .sample
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    storage.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        queries::set_meta(&tx, META_HEALTH, &state)?;
        queries::set_meta(&tx, META_RUNS, &runs)?;
        if let Some(sample) = &sample {
            queries::set_meta(&tx, META_SAMPLE, sample)?;
        }
        tx.commit()?;
        Ok(())
    })
}

/// Exact-content duplicate rate over every `chunks` row (SQLite BINARY
/// equality: no trimming, no normalization). Report only: nothing is deleted
/// or changed. An empty table has zero counts and a null rate.
pub fn duplicate_report(storage: &Storage) -> Result<DuplicateReport> {
    let (total, distinct): (i64, i64) = storage.with_connection(|conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT content COLLATE BINARY) FROM chunks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    })?;
    let total = total.max(0) as u64;
    let distinct = (distinct.max(0) as u64).min(total);
    let duplicate_rows = total - distinct;
    Ok(DuplicateReport {
        at: Utc::now().to_rfc3339(),
        total,
        distinct,
        duplicate_rows,
        rate: (total > 0).then(|| duplicate_rows as f64 / total as f64),
    })
}

/// Whether a measurement is due at `now`: never attempted, or the last
/// attempt (completed or errored) is at least six hours old. Read from the
/// persisted meta, so a restart neither re-runs early nor skips a due run.
pub fn health_due(storage: &Storage, now: DateTime<Utc>) -> bool {
    let attempted = load_health(storage)
        .ok()
        .and_then(|snapshot| snapshot.state.last_attempt_at)
        .and_then(|at| DateTime::parse_from_rfc3339(&at).ok())
        .map(|at| at.with_timezone(&Utc));
    match attempted {
        Some(at) => now - at >= Duration::hours(HEALTH_INTERVAL_HOURS),
        None => true,
    }
}

/// Engine version and the search settings the numbers were taken under.
pub fn config_identity() -> String {
    format!(
        "engine={};search_mode={};trained_rerank={}",
        env!("CARGO_PKG_VERSION"),
        std::env::var("CSR_SEARCH_MODE").unwrap_or_else(|_| "default".into()),
        crate::search::trained_rerank::trained_rerank_requested()
    )
}

/// One full measurement: the canary for this binary, the duplicate rate and
/// the recall probe, persisted together. A probe error is recorded beside the
/// previous completed result and never overwrites it. `refresh_sample`
/// deliberately creates a new sample (see `probe::load_or_create_sample`).
pub async fn run_health(
    storage: &Storage,
    search: &Arc<RwLock<SearchEngine>>,
    backend: &dyn probe::ProbeBackend,
    refresh_sample: bool,
    now: DateTime<Utc>,
) -> Result<HealthRun> {
    let mut snapshot = load_health(storage)?;
    let at = now.to_rfc3339();

    let canary = crate::eval::contamination::emitter_canary();
    snapshot.state.canary = Some(CanaryRecord {
        at: at.clone(),
        passed: canary.passed,
        detail: canary.detail,
    });
    match duplicate_report(storage) {
        Ok(mut report) => {
            report.at = at.clone();
            snapshot.state.duplicates = Some(report);
        }
        Err(error) => tracing::warn!(%error, "duplicate-rate measurement failed"),
    }

    let load = probe::load_or_create_sample(storage, &snapshot, refresh_sample, now)?;
    if let Some(note) = &load.note {
        tracing::warn!("{note}");
    }
    let sample_id = load.frozen.then(|| load.sample.sample_id.clone());
    let report = probe::run_probe(
        storage,
        search,
        backend,
        sample_id.as_deref(),
        &load.sample.cases,
        now,
    )
    .await;
    if load.created {
        snapshot.sample = Some(load.sample);
    }
    snapshot.state.version = 1;
    snapshot.state.last_attempt_at = Some(at);
    snapshot.state.config = Some(config_identity());
    if report.grade == ProbeGrade::Error {
        snapshot.state.last_probe_error = Some(report.clone());
    } else {
        let rate = snapshot.state.duplicates.as_ref().and_then(|d| d.rate);
        snapshot.runs.push(RunRecord::from_probe(&report, rate));
        snapshot.state.probe = Some(report.clone());
        snapshot.state.last_probe_error = None;
    }
    save_health(storage, &snapshot)?;
    Ok(HealthRun {
        snapshot,
        probe: report,
    })
}

/// How many five-minute ticks a due measurement waits for a due dream pass
/// before running anyway: dreaming goes first, but a stuck dream pass must not
/// starve the health measurement.
pub const MAX_DREAM_DEFERRALS: u32 = 12;
/// Seconds between cadence checks.
const TICK_SECS: u64 = 300;
/// Seconds the loop waits after daemon start before its first check, so the
/// watcher's first import batches are not competing with it.
const START_DELAY_SECS: u64 = 120;

/// Whether a dream pass is due and will want the heavy-work permit.
pub fn dream_pending(storage: &Storage, now: DateTime<Utc>) -> bool {
    use crate::daemon::dream_cadence as dc;
    !dc::dreaming_disabled()
        && !dc::consent_declined(storage)
        && dc::is_due(dc::read_last_run(storage), now, dc::interval_secs())
}

/// A due measurement yields to a due dream pass, a bounded number of ticks.
pub fn defer_for_dream(dream_pending: bool, deferred_ticks: u32) -> bool {
    dream_pending && deferred_ticks < MAX_DREAM_DEFERRALS
}

/// Persist the canary verdict for this binary (daemon start). Pure and
/// database-free to evaluate; only the record is written.
pub fn record_canary(storage: &Storage, now: DateTime<Utc>) -> Result<()> {
    let canary = crate::eval::contamination::emitter_canary();
    let mut snapshot = load_health(storage)?;
    snapshot.state.version = 1;
    snapshot.state.canary = Some(CanaryRecord {
        at: now.to_rfc3339(),
        passed: canary.passed,
        detail: canary.detail,
    });
    save_health(storage, &snapshot)
}

/// What one cadence check did.
#[derive(Debug)]
pub enum Tick {
    NotDue,
    /// Due, but yielding to a due dream pass.
    Deferred,
    Ran(Box<HealthRun>),
    Failed(String),
    Shutdown,
}

/// One cadence check: measure when six hours have passed since the last
/// attempt, behind the shared heavy-work permit and after a due dream pass.
/// With dreaming disabled the measurement runs when due all the same.
pub async fn health_tick(
    storage: &Storage,
    search: &Arc<RwLock<SearchEngine>>,
    backend: &dyn probe::ProbeBackend,
    heavy_work: &Arc<Semaphore>,
    shutdown: &AtomicBool,
    now: DateTime<Utc>,
    deferred_ticks: u32,
) -> Tick {
    if shutdown.load(Ordering::SeqCst) {
        return Tick::Shutdown;
    }
    if !health_due(storage, now) {
        return Tick::NotDue;
    }
    if defer_for_dream(dream_pending(storage, now), deferred_ticks) {
        return Tick::Deferred;
    }
    let Ok(permit) = heavy_work.clone().acquire_owned().await else {
        return Tick::Shutdown;
    };
    if shutdown.load(Ordering::SeqCst) {
        return Tick::Shutdown;
    }
    let outcome = run_health(storage, search, backend, false, now).await;
    drop(permit);
    match outcome {
        Ok(run) => Tick::Ran(Box::new(run)),
        Err(error) => Tick::Failed(error.to_string()),
    }
}

async fn pause(seconds: u64, shutdown: &AtomicBool) -> bool {
    for _ in 0..seconds.div_ceil(10) {
        if shutdown.load(Ordering::SeqCst) {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
    !shutdown.load(Ordering::SeqCst)
}

/// The daemon's health cadence: canary at start, then the six-hour
/// measurement on the shared heavy-work permit. Restart-safe: the schedule is
/// the persisted last attempt, not process memory.
pub async fn health_loop(
    storage: Arc<Storage>,
    embeddings: Arc<crate::embeddings::EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    heavy_work: Arc<Semaphore>,
    shutdown: Arc<AtomicBool>,
) {
    if let Err(error) = record_canary(&storage, Utc::now()) {
        tracing::warn!(%error, "could not record the canary verdict");
    }
    let backend = probe::EngineBackend {
        storage: storage.clone(),
        embeddings,
        search: search.clone(),
    };
    if !pause(START_DELAY_SECS, &shutdown).await {
        return;
    }
    let mut deferred = 0;
    loop {
        match health_tick(
            &storage,
            &search,
            &backend,
            &heavy_work,
            &shutdown,
            Utc::now(),
            deferred,
        )
        .await
        {
            Tick::Shutdown => return,
            Tick::Deferred => deferred += 1,
            Tick::NotDue => deferred = 0,
            Tick::Ran(run) => {
                deferred = 0;
                tracing::info!(
                    grade = run.probe.grade.as_str(),
                    hits = run.probe.hits,
                    n = run.probe.n,
                    "memory health measured"
                );
            }
            Tick::Failed(error) => {
                deferred = 0;
                tracing::warn!(%error, "memory health measurement failed");
            }
        }
        if !pause(TICK_SECS, &shutdown).await {
            return;
        }
    }
}

/// What one `run_health` measured, beside everything now persisted.
#[derive(Debug, Clone)]
pub struct HealthRun {
    pub snapshot: HealthSnapshot,
    /// This run's probe report, including an `Error` one (which is not
    /// stored as the latest completed result).
    pub probe: ProbeReport,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;

    fn chunk(id: &str, conversation: &str, content: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "p".into(),
            timestamp: "2026-10-01T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        }
    }

    fn insert(storage: &Storage, id: &str, content: &str) {
        storage
            .insert_chunk(&chunk(id, &format!("conv-{id}"), content), &[0.0; 4])
            .unwrap();
    }

    #[test]
    fn duplicate_rate_is_exact_content_report_only() {
        let storage = Storage::open_memory().unwrap();
        insert(&storage, "a", "same words here");
        insert(&storage, "b", "same words here");
        insert(&storage, "c", "same words here");
        insert(&storage, "d", "Same words here"); // case differs: not a duplicate
        insert(&storage, "e", "same words here "); // trailing space: not a duplicate
        insert(&storage, "f", "unique");
        let before = storage.with_connection(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))?)
        });
        let report = duplicate_report(&storage).unwrap();
        assert_eq!(report.total, 6);
        assert_eq!(report.distinct, 4);
        assert_eq!(report.duplicate_rows, 2);
        assert!((report.rate.unwrap() - 2.0 / 6.0).abs() < 1e-12);
        // report only: nothing was removed or written
        let after = storage.with_connection(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))?)
        });
        assert_eq!(before.unwrap(), after.unwrap());
        assert!(storage.get_meta(META_HEALTH).unwrap().is_none());
    }

    #[test]
    fn empty_duplicate_population_has_null_rate() {
        let storage = Storage::open_memory().unwrap();
        let report = duplicate_report(&storage).unwrap();
        assert_eq!(
            (report.total, report.distinct, report.duplicate_rows),
            (0, 0, 0)
        );
        assert_eq!(report.rate, None);
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["rate"].is_null());
    }

    fn record(n: usize) -> RunRecord {
        RunRecord {
            at: format!("2026-10-{:02}T00:00:00Z", n + 1),
            sample_id: Some("s1".into()),
            hits: n,
            n: 20,
            invalidated: 0,
            grade: ProbeGrade::Red,
            misses: (0..30)
                .map(|i| ProbeMiss {
                    chunk_id: format!("c{i}"),
                    conversation_id: format!("v{i}"),
                    returned: vec!["x".into()],
                    cause: MissCause::NotInTop5,
                })
                .collect(),
            duplicate_rate: None,
        }
    }

    #[test]
    fn health_meta_round_trip_keeps_last_seven_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        let storage = Storage::open(&path).unwrap();
        let mut snapshot = HealthSnapshot::default();
        snapshot.state.version = 1;
        snapshot.state.duplicates = Some(DuplicateReport {
            at: "2026-10-13T00:00:00Z".into(),
            total: 4,
            distinct: 3,
            duplicate_rows: 1,
            rate: Some(0.25),
        });
        snapshot.runs = (0..10).map(|n| record(n).clone()).collect();
        save_health(&storage, &snapshot).unwrap();
        let loaded = load_health(&storage).unwrap();
        assert_eq!(loaded.state, snapshot.state);
        assert_eq!(loaded.runs.len(), RUN_HISTORY);
        assert_eq!(loaded.runs[0].hits, 3, "the oldest three were dropped");
        assert_eq!(loaded.runs[6].hits, 9);
        // only the full record round-trips; the in-memory record keeps no
        // more than the stored cap of misses once built from a probe report
        let probe = ProbeReport {
            at: "2026-10-13T00:00:00Z".into(),
            sample_id: Some("s".into()),
            hits: 1,
            n: 20,
            invalidated: 0,
            grade: ProbeGrade::Red,
            misses: record(0).misses,
            error: None,
        };
        assert_eq!(
            RunRecord::from_probe(&probe, None).misses.len(),
            MISSES_PER_RUN
        );
        // a fresh store has measured nothing
        let fresh = Storage::open_memory().unwrap();
        assert_eq!(load_health(&fresh).unwrap(), HealthSnapshot::default());
    }

    #[test]
    fn health_due_is_six_hour_and_restart_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        let now = DateTime::parse_from_rfc3339("2026-10-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        {
            let storage = Storage::open(&path).unwrap();
            assert!(health_due(&storage, now), "never attempted is due");
            let mut snapshot = HealthSnapshot::default();
            snapshot.state.last_attempt_at = Some(now.to_rfc3339());
            save_health(&storage, &snapshot).unwrap();
            assert!(!health_due(&storage, now));
        }
        // a restart reads the same persisted time
        let storage = Storage::open(&path).unwrap();
        assert!(!health_due(
            &storage,
            now + Duration::hours(5) + Duration::minutes(59)
        ));
        assert!(health_due(&storage, now + Duration::hours(6)));
    }
    struct Silent;

    #[async_trait::async_trait]
    impl probe::ProbeBackend for Silent {
        async fn final_page(&self, _query: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
    }

    fn search() -> Arc<RwLock<SearchEngine>> {
        Arc::new(RwLock::new(SearchEngine::new(8)))
    }

    #[test]
    fn dream_goes_first_but_only_for_a_bounded_time() {
        assert!(defer_for_dream(true, 0));
        assert!(defer_for_dream(true, MAX_DREAM_DEFERRALS - 1));
        assert!(
            !defer_for_dream(true, MAX_DREAM_DEFERRALS),
            "a stuck dream cannot starve health"
        );
        assert!(
            !defer_for_dream(false, 0),
            "with no dream due, health runs when due"
        );
    }

    /// The dream cadence reads the process env, so the shared test guard is
    /// held for the whole test, outside the async body.
    #[test]
    fn health_tick_runs_when_due_on_the_shared_permit_and_then_waits_six_hours() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(
                health_tick_runs_when_due_on_the_shared_permit_and_then_waits_six_hours_body(),
            );
    }

    async fn health_tick_runs_when_due_on_the_shared_permit_and_then_waits_six_hours_body() {
        let storage = Storage::open_memory().unwrap();
        let heavy = Arc::new(Semaphore::new(1));
        let shutdown = AtomicBool::new(false);
        let now = Utc::now();
        // pretend dreaming already ran, so only health is due
        storage
            .set_meta(
                crate::daemon::dream_cadence::META_LAST_RUN_AT,
                &now.to_rfc3339(),
            )
            .unwrap();

        let first = health_tick(&storage, &search(), &Silent, &heavy, &shutdown, now, 0).await;
        let Tick::Ran(run) = first else {
            panic!("expected a run, got {first:?}")
        };
        assert_eq!(run.probe.grade, ProbeGrade::Abstain);
        assert_eq!(run.probe.n, 0);
        assert_eq!(heavy.available_permits(), 1, "the permit is released");
        assert!(load_health(&storage).unwrap().state.canary.unwrap().passed);

        let soon = now + Duration::hours(5);
        assert!(matches!(
            health_tick(&storage, &search(), &Silent, &heavy, &shutdown, soon, 0).await,
            Tick::NotDue
        ));
        let later = now + Duration::hours(6);
        // the dream pass is due again by then; it has run, so only health is due
        storage
            .set_meta(
                crate::daemon::dream_cadence::META_LAST_RUN_AT,
                &later.to_rfc3339(),
            )
            .unwrap();
        assert!(matches!(
            health_tick(&storage, &search(), &Silent, &heavy, &shutdown, later, 0).await,
            Tick::Ran(_)
        ));
    }

    /// The dream cadence reads the process env, so the shared test guard is
    /// held for the whole test, outside the async body.
    #[test]
    fn health_tick_yields_to_a_due_dream_then_runs_and_respects_shutdown() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(health_tick_yields_to_a_due_dream_then_runs_and_respects_shutdown_body());
    }

    async fn health_tick_yields_to_a_due_dream_then_runs_and_respects_shutdown_body() {
        let storage = Storage::open_memory().unwrap();
        let heavy = Arc::new(Semaphore::new(1));
        let shutdown = AtomicBool::new(false);
        let now = Utc::now();
        // never dreamed: no dream is pending, health is not held back
        assert!(!dream_pending(&storage, now));
        // last dreamed seven hours ago: one is due, so health defers, boundedly
        storage
            .set_meta(
                crate::daemon::dream_cadence::META_LAST_RUN_AT,
                &(now - Duration::hours(7)).to_rfc3339(),
            )
            .unwrap();
        assert!(dream_pending(&storage, now));
        assert!(matches!(
            health_tick(&storage, &search(), &Silent, &heavy, &shutdown, now, 0).await,
            Tick::Deferred
        ));
        assert!(matches!(
            health_tick(
                &storage,
                &search(),
                &Silent,
                &heavy,
                &shutdown,
                now,
                MAX_DREAM_DEFERRALS
            )
            .await,
            Tick::Ran(_)
        ));

        // a held permit blocks the run; shutdown while waiting abstains
        let held = heavy.clone().acquire_owned().await.unwrap();
        let later = now + Duration::hours(7);
        let shutdown = Arc::new(AtomicBool::new(false));
        let waiter = {
            let storage = Arc::new(Storage::open_memory().unwrap());
            let heavy = heavy.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                health_tick(
                    &storage,
                    &search(),
                    &Silent,
                    &heavy,
                    &shutdown,
                    later,
                    MAX_DREAM_DEFERRALS,
                )
                .await
            })
        };
        tokio::task::yield_now().await;
        shutdown.store(true, Ordering::SeqCst);
        drop(held);
        assert!(matches!(waiter.await.unwrap(), Tick::Shutdown));
        // nothing was written by the abstained tick's store (a separate one),
        // and the canary-only record path writes just the health key
        let other = Storage::open_memory().unwrap();
        record_canary(&other, now).unwrap();
        assert!(other.get_meta(META_HEALTH).unwrap().is_some());
        assert!(other.get_meta(META_SAMPLE).unwrap().is_none());
    }
}
