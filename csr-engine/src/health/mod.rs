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

/// Write the three keys on `conn`, the history bounded to the last seven
/// records. The caller owns the transaction.
fn write_snapshot(conn: &Connection, snapshot: &HealthSnapshot) -> Result<()> {
    let mut runs = snapshot.runs.clone();
    if runs.len() > RUN_HISTORY {
        runs.drain(..runs.len() - RUN_HISTORY);
    }
    queries::set_meta(conn, META_HEALTH, &serde_json::to_string(&snapshot.state)?)?;
    queries::set_meta(conn, META_RUNS, &serde_json::to_string(&runs)?)?;
    if let Some(sample) = &snapshot.sample {
        queries::set_meta(conn, META_SAMPLE, &serde_json::to_string(sample)?)?;
    }
    Ok(())
}

/// Persist the health meta in one transaction. The history is bounded to the
/// last seven records here, so no caller can grow it. This overwrites what is
/// stored; a measurement that ran concurrently with another process goes
/// through [`update_health`] instead.
pub fn save_health(storage: &Storage, snapshot: &HealthSnapshot) -> Result<()> {
    storage.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        write_snapshot(&tx, snapshot)?;
        tx.commit()?;
        Ok(())
    })
}

/// Read, change and write the health meta in one write transaction taken up
/// front (`BEGIN IMMEDIATE`), so a second process cannot slip a write between
/// the read and the write. `change` sees what is stored now, not what this
/// process read earlier, and returns `None` to write nothing.
pub fn update_health<T>(
    storage: &Storage,
    change: impl FnOnce(&mut HealthSnapshot) -> Option<T>,
) -> Result<Option<T>> {
    storage.with_connection(|conn| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let mut snapshot = HealthSnapshot {
            state: read_json(&tx, META_HEALTH)?.unwrap_or_default(),
            runs: read_json(&tx, META_RUNS)?.unwrap_or_default(),
            sample: read_json(&tx, META_SAMPLE)?,
        };
        let Some(out) = change(&mut snapshot) else {
            return Ok(None);
        };
        write_snapshot(&tx, &snapshot)?;
        tx.commit()?;
        Ok(Some(out))
    })
}

/// The id of a frozen sample: one with enough cases to have been stored.
fn frozen_id(sample: Option<&ProbeSample>) -> Option<String> {
    sample
        .filter(|sample| sample.cases.len() >= probe::SAMPLE_MIN)
        .map(|sample| sample.sample_id.clone())
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

/// What one measurement found, before it is written.
#[derive(Debug, Clone)]
pub(crate) struct Measured {
    at: String,
    canary: CanaryRecord,
    duplicates: Option<DuplicateReport>,
    probe: ProbeReport,
    /// The frozen sample stored when this run began (`None` when there was
    /// none). The result is only valid while that is still the stored one.
    base_sample: Option<String>,
    /// A sample this run created and means to store.
    new_sample: Option<ProbeSample>,
    /// Why the sample is not the stored one, when that is worth saying (an
    /// old-format sample was replaced).
    note: Option<String>,
}

/// Measure: the canary for this binary, the duplicate rate and the recall
/// probe. Nothing is written. `None` when `cancel` was raised.
pub(crate) async fn measure_health(
    storage: &Storage,
    search: &Arc<RwLock<SearchEngine>>,
    backend: &dyn probe::ProbeBackend,
    refresh_sample: bool,
    now: DateTime<Utc>,
    cancel: &AtomicBool,
) -> Result<Option<Measured>> {
    let snapshot = load_health(storage)?;
    let at = now.to_rfc3339();

    let canary = crate::eval::contamination::emitter_canary();
    let canary = CanaryRecord {
        at: at.clone(),
        passed: canary.passed,
        detail: canary.detail,
    };
    if cancel.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let duplicates = match duplicate_report(storage) {
        Ok(mut report) => {
            report.at = at.clone();
            Some(report)
        }
        Err(error) => {
            tracing::warn!(%error, "duplicate-rate measurement failed");
            None
        }
    };

    let load = probe::load_or_create_sample(storage, &snapshot, refresh_sample, now)?;
    if let Some(note) = &load.note {
        tracing::warn!("{note}");
    }
    let sample_id = load.frozen.then(|| load.sample.sample_id.clone());
    let Some(probe) = probe::run_probe(
        storage,
        search,
        backend,
        sample_id.as_deref(),
        &load.sample.cases,
        now,
        cancel,
    )
    .await
    else {
        return Ok(None);
    };
    if cancel.load(Ordering::SeqCst) {
        return Ok(None);
    }
    Ok(Some(Measured {
        at,
        canary,
        duplicates,
        probe,
        base_sample: frozen_id(snapshot.sample.as_ref()),
        new_sample: load.created.then_some(load.sample),
        note: load.note,
    }))
}

/// Write a measurement into what is stored NOW, in one write transaction.
/// `None` (and nothing written) when the stored sample is no longer the one
/// the run began with: another process refreshed it, or enrolled one first.
/// A new sample is stored only where none was; a verdict is stored only for
/// the sample it was measured against.
pub(crate) fn commit_measured(storage: &Storage, measured: &Measured) -> Result<Option<HealthRun>> {
    update_health(storage, |snapshot| {
        if frozen_id(snapshot.sample.as_ref()) != measured.base_sample {
            return None;
        }
        if let Some(sample) = &measured.new_sample {
            snapshot.sample = Some(sample.clone());
        }
        let state = &mut snapshot.state;
        state.version = 1;
        state.last_attempt_at = Some(measured.at.clone());
        state.config = Some(config_identity());
        state.canary = Some(measured.canary.clone());
        if measured.duplicates.is_some() {
            state.duplicates = measured.duplicates.clone();
        }
        let report = measured.probe.clone();
        if report.grade == ProbeGrade::Error {
            state.last_probe_error = Some(report.clone());
        } else {
            let rate = state.duplicates.as_ref().and_then(|d| d.rate);
            state.probe = Some(report.clone());
            state.last_probe_error = None;
            snapshot.runs.push(RunRecord::from_probe(&report, rate));
        }
        Some(HealthRun {
            snapshot: snapshot.clone(),
            probe: report,
            note: measured.note.clone(),
        })
    })
}

/// One full measurement, persisted. A probe error is recorded beside the
/// previous completed result and never overwrites it. `refresh_sample`
/// deliberately creates a new sample (see `probe::load_or_create_sample`).
/// If another process changed the stored sample meanwhile, this run's verdict
/// is dropped and the measurement is repeated once against the stored sample;
/// a second conflict is an error. `None` when `cancel` was raised.
pub async fn run_health(
    storage: &Storage,
    search: &Arc<RwLock<SearchEngine>>,
    backend: &dyn probe::ProbeBackend,
    refresh_sample: bool,
    now: DateTime<Utc>,
    cancel: &AtomicBool,
) -> Result<Option<HealthRun>> {
    for attempt in 0..2 {
        // a repeat has nothing to refresh: the other process already did
        let refresh = refresh_sample && attempt == 0;
        let Some(measured) = measure_health(storage, search, backend, refresh, now, cancel).await?
        else {
            return Ok(None);
        };
        if let Some(run) = commit_measured(storage, &measured)? {
            return Ok(Some(run));
        }
        tracing::warn!("the stored recall sample changed during the measurement; measuring again");
    }
    anyhow::bail!("the stored recall sample kept changing during the measurement")
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
    update_health(storage, |snapshot| {
        snapshot.state.version = 1;
        snapshot.state.canary = Some(CanaryRecord {
            at: now.to_rfc3339(),
            passed: canary.passed,
            detail: canary.detail,
        });
        Some(())
    })?;
    Ok(())
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
    // Wait for the permit, but never past shutdown: another heavy job can hold
    // it for a long time, and the daemon awaits this task to finish.
    let acquire = heavy_work.clone().acquire_owned();
    tokio::pin!(acquire);
    let permit = loop {
        tokio::select! {
            acquired = &mut acquire => match acquired {
                Ok(permit) => break permit,
                Err(_) => return Tick::Shutdown,
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                if shutdown.load(Ordering::SeqCst) {
                    return Tick::Shutdown;
                }
            }
        }
    };
    if shutdown.load(Ordering::SeqCst) {
        return Tick::Shutdown;
    }
    let outcome = run_health(storage, search, backend, false, now, shutdown).await;
    drop(permit);
    match outcome {
        Ok(Some(run)) => Tick::Ran(Box::new(run)),
        Ok(None) => Tick::Shutdown,
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

/// Whether `CSR_NO_HEALTH` switches the daemon's health loop off: `1` or
/// `true`, any case, as for `CSR_NO_DREAMING`. A pure function of the value so
/// no test sets process env.
pub fn health_disabled_from(value: Option<&str>) -> bool {
    value.is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

/// The daemon's health cadence: canary at start, then the six-hour
/// measurement on the shared heavy-work permit. Restart-safe: the schedule is
/// the persisted last attempt, not process memory.
///
/// `CSR_NO_HEALTH=1` turns the loop off, read once when the daemon spawns it:
/// no canary record, no duplicate scan, no recall probe, one log line saying so.
/// It is the daemon's switch only. `csr-engine eval --full` still measures when
/// the person runs it, and `status` still shows whatever was last measured (an
/// old red result stops raising the chip after twelve hours, so with the loop
/// off it clears by itself).
pub async fn health_loop(
    storage: Arc<Storage>,
    embeddings: Arc<crate::embeddings::EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    heavy_work: Arc<Semaphore>,
    shutdown: Arc<AtomicBool>,
) {
    let off = health_disabled_from(std::env::var("CSR_NO_HEALTH").ok().as_deref());
    health_loop_unless(off, storage, embeddings, search, heavy_work, shutdown).await
}

/// [`health_loop`] with the switch decided by the caller.
pub async fn health_loop_unless(
    off: bool,
    storage: Arc<Storage>,
    embeddings: Arc<crate::embeddings::EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    heavy_work: Arc<Semaphore>,
    shutdown: Arc<AtomicBool>,
) {
    if off {
        tracing::info!("memory health loop off via CSR_NO_HEALTH");
        return;
    }
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
    /// Set when this run replaced an old-format sample, and why.
    pub note: Option<String>,
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
    #[test]
    fn shutdown_while_another_holder_keeps_the_permit_returns_promptly() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(shutdown_while_another_holder_keeps_the_permit_body());
    }

    async fn shutdown_while_another_holder_keeps_the_permit_body() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let heavy = Arc::new(Semaphore::new(1));
        // another heavy job holds the permit for the whole test
        let _held = heavy.clone().acquire_owned().await.unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (storage, heavy, shutdown) = (storage.clone(), heavy.clone(), shutdown.clone());
            tokio::spawn(async move {
                health_tick(
                    &storage,
                    &search(),
                    &Silent,
                    &heavy,
                    &shutdown,
                    Utc::now(),
                    MAX_DREAM_DEFERRALS,
                )
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        shutdown.store(true, Ordering::SeqCst);
        let tick = tokio::time::timeout(std::time::Duration::from_secs(3), waiter)
            .await
            .expect("health_tick must notice shutdown while it waits for the permit")
            .unwrap();
        assert!(matches!(tick, Tick::Shutdown), "{tick:?}");
        assert_eq!(
            heavy.available_permits(),
            0,
            "the other holder still has it"
        );
        assert!(storage.get_meta(META_HEALTH).unwrap().is_none());
    }
    #[test]
    fn csr_no_health_is_decided_from_the_env_value() {
        for on in ["1", "true", "TRUE", "True"] {
            assert!(health_disabled_from(Some(on)), "{on}");
        }
        for off in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("yes"),
            Some("2"),
        ] {
            assert!(!health_disabled_from(off), "{off:?}");
        }
    }

    #[test]
    fn a_disabled_health_loop_measures_nothing_and_an_enabled_one_starts_with_the_canary() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let embeddings = Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
                let heavy = Arc::new(Semaphore::new(1));
                // shutdown is already raised, so the enabled loop returns after
                // its first step instead of waiting out its start delay
                let shutdown = Arc::new(AtomicBool::new(true));
                for (off, recorded) in [(true, false), (false, true)] {
                    let storage = Arc::new(Storage::open_memory().unwrap());
                    health_loop_unless(
                        off,
                        storage.clone(),
                        embeddings.clone(),
                        search(),
                        heavy.clone(),
                        shutdown.clone(),
                    )
                    .await;
                    let snapshot = load_health(&storage).unwrap();
                    assert_eq!(snapshot.state.canary.is_some(), recorded, "off={off}");
                    assert!(snapshot.state.duplicates.is_none() && snapshot.state.probe.is_none());
                    assert!(snapshot.sample.is_none(), "off={off}");
                    assert_eq!(heavy.available_permits(), 1);
                }
            });
    }
}
