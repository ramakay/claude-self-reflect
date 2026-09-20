//! Hook recall replay harness — env-only, drives the real `UserPromptSubmit`
//! hook in-process against a scratch DB clone and scores recall purely from
//! what the hook itself wrote to the database (`rerank_exposure_items` /
//! `retrieval_events`), never by matching stdout.
//!
//! This is the pre-registered proof harness for the hook-project-family fix:
//! it lands as the FIRST commit on this branch so the identical source is
//! built and run against the base build and the fix build.
//!
//! # Honesty about what the DB can show
//!
//! `rerank_exposure_items` holds the RENDERED set the hook actually wrote for
//! a session (post `scored.iter().take(5)`, post dedup, post noise filter,
//! post the 500-token budget) — NOT the 15-result candidate list the search
//! layer produced internally. So `hit_at_15` really means "hit anywhere in
//! the recorded list" (which in practice tops out far below 15 rendered
//! items); `hit_at_5` is the metric this harness treats as operative. See
//! `retrieved_set_semantics` in the emitted summary.
//!
//! # Required env (fail loudly, one-line reason, non-zero exit)
//!
//!   CSR_HOOKRECALL_DB              scratch DB path — MUST canonicalize under
//!                                   /tmp/csr-recall; NEVER the live DB
//!   CSR_HOOKRECALL_PROJECTS        Engine::new's projects dir (empty scratch OK)
//!   CSR_HOOKRECALL_OUT             output dir for scores.ndjson + summary.json
//!   CSR_HOOKRECALL_LABEL           arm label, e.g. "base.hidden"
//!   CSR_HOOKRECALL_SAMPLE_PROJECT  exact stored project_name to sample from
//!   CSR_HOOKRECALL_CWD             directory to ask the hook from (must exist)
//!   CSR_HOOKRECALL_NOW             RFC3339 instant that freezes the sampling
//!                                   age window (must not be more than 2 days
//!                                   behind the wall clock — the 19-day sample
//!                                   window would then cross the hook's own
//!                                   21-day MAX_CHUNK_AGE_DAYS gate, which uses
//!                                   the real `Utc::now()` and cannot be frozen)
//!
//! Optional: CSR_HOOKRECALL_N (default 200), CSR_HOOKRECALL_SEED (default 7).
//!
//! Run:
//!   CSR_HOOKRECALL_DB=/tmp/csr-recall/x/db.sqlite \
//!   CSR_HOOKRECALL_PROJECTS=/tmp/csr-recall/x/projects-empty \
//!   CSR_HOOKRECALL_OUT=/tmp/csr-recall/x/out \
//!   CSR_HOOKRECALL_LABEL=base.hidden \
//!   CSR_HOOKRECALL_SAMPLE_PROJECT=claude-self-reflect-csr-engine \
//!   CSR_HOOKRECALL_CWD=/Users/you/projects/claude-self-reflect/csr-engine \
//!   CSR_HOOKRECALL_NOW=2026-09-20T12:00:00Z \
//!     cargo run --release --example hook_recall_replay

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use csr_engine::engine::Engine;
use csr_engine::extraction::provenance::is_csr_emission;
use csr_engine::hooks::{prompt_submit, HookInput};
use csr_engine::temporal::parse_timestamp;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use uuid::Uuid;

/// Fixed v5 namespace for synthetic session ids. Arbitrary but stable, so two
/// runs derive byte-identical ids from identical (label, seed, index) triples.
const SESSION_NAMESPACE: Uuid = Uuid::from_bytes([
    0xc5, 0x5c, 0xa1, 0x1e, 0x00, 0x0a, 0x40, 0xbe, 0x9b, 0x00, 0x68, 0x6f, 0x6f, 0x6b, 0x5f, 0x72,
]);

/// Mirrors `hooks::prompt_submit::MAX_CHUNK_AGE_DAYS` sample-side upper bound
/// (19, not 21) so the sampled prompt never sits right at the hook's own gate.
const SAMPLE_AGE_MAX_DAYS: f64 = 19.0;
/// Mirrors the char budget used to derive a prompt from a chunk's content.
const PROMPT_TRUNCATE_CHARS: usize = 400;
const CONTENT_LEN_MIN: i64 = 400;
const CONTENT_LEN_MAX: i64 = 1500;
const LIVE_DB_MARKER: &str = ".claude-self-reflect";
const SCRATCH_ROOT: &str = "/tmp/csr-recall";

// ─── Mirrored hook predicates ───
//
// `hooks::prompt_submit::early_exit`, `is_harness_turn`, `HARNESS_BLOCK_OPENINGS`,
// `is_self_referential_noise`'s `NOISE_PATTERNS`, and
// `transcript::instrumentation::is_noisy_steer_text` are all `pub(crate)` or
// private and unreachable from an example. Each is copied here verbatim from
// the source at the time this harness was written (line numbers cited in the
// plan); `extraction::provenance::is_csr_emission` is `pub` and used directly,
// not mirrored.

/// Mirrors `transcript::instrumentation::is_noisy_steer_text` +
/// `hooks::prompt_submit::is_harness_turn`'s extra cross-session-message check.
fn mirror_is_harness_turn(prompt: &str) -> bool {
    const QUEUED_PREFIX: &str = "[queued] ";
    let mut normalized = prompt.trim_start();
    while let Some(stripped) = normalized.strip_prefix(QUEUED_PREFIX) {
        normalized = stripped.trim_start();
    }
    normalized.starts_with("[SYSTEM NOTIFICATION")
        || prompt.contains("<task-notification>")
        || prompt.contains("<local-command-caveat>")
        || prompt.contains("<command-name>")
        || prompt.contains("<cross-session-message")
}

/// Mirrors `hooks::prompt_submit::HARNESS_BLOCK_OPENINGS` (used by
/// `is_self_echo` to drop chunks that open with a harness-written block).
const MIRROR_HARNESS_BLOCK_OPENINGS: [&str; 4] = [
    "<task-notification>",
    "[SYSTEM NOTIFICATION",
    "<cross-session-message",
    "Another Claude session sent a message:",
];

fn mirror_opens_with_harness_block(content: &str) -> bool {
    let opening = content.trim_start();
    MIRROR_HARNESS_BLOCK_OPENINGS
        .iter()
        .any(|block| opening.starts_with(block))
}

/// Mirrors `hooks::prompt_submit::is_self_referential_noise`'s private
/// `NOISE_PATTERNS` list (the `is_csr_emission` branch is not mirrored — it's
/// called directly, since it is `pub`).
const MIRROR_NOISE_PATTERNS: [&str; 11] = [
    "session_start_hook",
    "session_end_hook",
    "prompt_submit_hook",
    "proves the hook",
    "proves the session",
    "proves the integration",
    "Current Ralph State:",
    "hook success",
    "hook error",
    "CSR engine ready",
    "hooks_integration",
];

fn mirror_is_self_referential_noise(content: &str) -> bool {
    if is_csr_emission(content) {
        return true;
    }
    let lower = content.to_lowercase();
    MIRROR_NOISE_PATTERNS
        .iter()
        .any(|pattern| lower.contains(&pattern.to_lowercase()))
}

/// Mirrors `hooks::prompt_submit::is_continuation_prompt`.
fn mirror_is_continuation_prompt(prompt: &str) -> bool {
    let p = prompt
        .trim()
        .trim_end_matches(['.', '!', '…', '?'])
        .trim_end()
        .to_lowercase();
    if p.len() > 60 {
        return false;
    }
    const PHRASES: [&str; 7] = [
        "continue",
        "resume",
        "carry on",
        "keep going",
        "pick up where we left off",
        "continue where we left off",
        "where were we",
    ];
    PHRASES
        .iter()
        .any(|ph| p == *ph || p.starts_with(&format!("{ph} ")))
}

// ─── Config ───

struct Config {
    db_path: PathBuf,
    projects_dir: PathBuf,
    out_dir: PathBuf,
    label: String,
    sample_project: String,
    cwd: PathBuf,
    n: usize,
    seed: u64,
    now: DateTime<Utc>,
    now_raw: String,
}

fn require_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

fn load_config() -> Result<Config, String> {
    let db_path_raw = require_env("CSR_HOOKRECALL_DB")?;
    let projects_dir = PathBuf::from(require_env("CSR_HOOKRECALL_PROJECTS")?);
    let out_dir = PathBuf::from(require_env("CSR_HOOKRECALL_OUT")?);
    let label = require_env("CSR_HOOKRECALL_LABEL")?;
    let sample_project = require_env("CSR_HOOKRECALL_SAMPLE_PROJECT")?;
    let cwd_raw = require_env("CSR_HOOKRECALL_CWD")?;
    let now_raw = require_env("CSR_HOOKRECALL_NOW")?;

    if db_path_raw.contains(LIVE_DB_MARKER) {
        return Err(format!(
            "CSR_HOOKRECALL_DB ({db_path_raw}) contains '{LIVE_DB_MARKER}' — refusing to touch the live store"
        ));
    }
    let db_path = PathBuf::from(&db_path_raw);
    let db_canonical = db_path.canonicalize().map_err(|e| {
        format!("CSR_HOOKRECALL_DB ({db_path_raw}) does not exist or cannot be canonicalized: {e}")
    })?;
    let scratch_root = Path::new(SCRATCH_ROOT);
    let scratch_root_canonical = scratch_root
        .canonicalize()
        .unwrap_or_else(|_| scratch_root.to_path_buf());
    if !db_canonical.starts_with(&scratch_root_canonical) {
        return Err(format!(
            "CSR_HOOKRECALL_DB canonicalizes to {} which is not under {SCRATCH_ROOT}",
            db_canonical.display()
        ));
    }

    let cwd = PathBuf::from(&cwd_raw);
    if !cwd.is_dir() {
        return Err(format!(
            "CSR_HOOKRECALL_CWD ({cwd_raw}) is not an existing directory"
        ));
    }

    let now = parse_timestamp(&now_raw).ok_or_else(|| {
        format!("CSR_HOOKRECALL_NOW ({now_raw}) is not a parseable RFC3339 timestamp")
    })?;
    let wall_now = Utc::now();
    if (wall_now - now).num_days() > 2 {
        return Err(format!(
            "CSR_HOOKRECALL_NOW ({now_raw}) is more than 2 days behind the wall clock ({wall_now}); \
             the 19-day sample window would cross the hook's own 21-day MAX_CHUNK_AGE_DAYS gate, \
             which uses the real Utc::now() and cannot be frozen"
        ));
    }

    let n = match std::env::var("CSR_HOOKRECALL_N") {
        Ok(v) => v
            .parse::<usize>()
            .map_err(|e| format!("CSR_HOOKRECALL_N ({v}) is not a valid usize: {e}"))?,
        Err(_) => 200,
    };
    let seed = match std::env::var("CSR_HOOKRECALL_SEED") {
        Ok(v) => v
            .parse::<u64>()
            .map_err(|e| format!("CSR_HOOKRECALL_SEED ({v}) is not a valid u64: {e}"))?,
        Err(_) => 7,
    };

    Ok(Config {
        db_path,
        projects_dir,
        out_dir,
        label,
        sample_project,
        cwd,
        n,
        seed,
        now,
        now_raw,
    })
}

fn open_ro(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("failed to open {} read-only: {e}", path.display()))
}

// ─── Sampling ───

struct RawCandidate {
    id: String,
    conversation_id: String,
    timestamp: String,
    content: String,
}

fn fetch_raw_candidates(
    conn: &Connection,
    sample_project: &str,
) -> Result<Vec<RawCandidate>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.id, c.conversation_id, c.timestamp, c.content FROM chunks c \
             WHERE c.project_name = ?1 AND c.source = 'conversation' AND c.is_sidechain = 0 \
             AND length(c.content) BETWEEN ?2 AND ?3 \
             AND EXISTS (SELECT 1 FROM chunk_embeddings e WHERE e.chunk_id = c.id) \
             ORDER BY c.id",
        )
        .map_err(|e| format!("prepare candidate query: {e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![sample_project, CONTENT_LEN_MIN, CONTENT_LEN_MAX],
            |row| {
                Ok(RawCandidate {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    timestamp: row.get(2)?,
                    content: row.get(3)?,
                })
            },
        )
        .map_err(|e| format!("query candidates: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("read candidate row: {e}"))?);
    }
    Ok(out)
}

fn fetch_conversation_ids(conn: &Connection) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT conversation_id FROM chunks")
        .map_err(|e| format!("prepare conversation_id query: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("query conversation ids: {e}"))?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.map_err(|e| format!("read conversation_id row: {e}"))?);
    }
    Ok(out)
}

/// A candidate that survived the pool filters (age window, harness-block
/// opening, self-referential noise) and is eligible for the deterministic pick.
struct PoolItem {
    id: String,
    conversation_id: String,
    content: String,
}

#[derive(Default)]
struct SkipCounters {
    excluded_age_window: u64,
    skipped_harness_opening: u64,
    skipped_self_referential: u64,
    skipped_harness_turn_early_exit: u64,
    skipped_continuation: u64,
    skipped_slash: u64,
    skipped_short: u64,
}

impl SkipCounters {
    fn n_skipped(&self) -> u64 {
        self.skipped_harness_opening
            + self.skipped_self_referential
            + self.skipped_harness_turn_early_exit
            + self.skipped_continuation
            + self.skipped_slash
            + self.skipped_short
    }
}

/// A sampled prompt ready to be replayed through the hook.
struct SampleItem {
    chunk_id: String,
    conversation_id: String,
    prompt: String,
}

/// Build the eligible pool (id-ordered, age + harness-block + self-referential
/// filters applied) then walk it in seeded hash order, accepting up to `n`
/// prompts that also survive the mirrored `early_exit` predicates.
fn build_sample(
    raw: Vec<RawCandidate>,
    now: DateTime<Utc>,
    seed: u64,
    n: usize,
    counters: &mut SkipCounters,
) -> Vec<SampleItem> {
    let mut pool: Vec<PoolItem> = Vec::new();
    for c in raw {
        let Some(parsed) = parse_timestamp(&c.timestamp) else {
            counters.excluded_age_window += 1;
            continue;
        };
        let age_days = (now - parsed).num_seconds() as f64 / 86400.0;
        if !(0.0..=SAMPLE_AGE_MAX_DAYS).contains(&age_days) {
            counters.excluded_age_window += 1;
            continue;
        }
        if mirror_opens_with_harness_block(&c.content) {
            counters.skipped_harness_opening += 1;
            continue;
        }
        if mirror_is_self_referential_noise(&c.content) {
            counters.skipped_self_referential += 1;
            continue;
        }
        pool.push(PoolItem {
            id: c.id,
            conversation_id: c.conversation_id,
            content: c.content,
        });
    }

    // Seeded deterministic order: blake3(seed:chunk_id) hex ascending.
    let mut hashed: Vec<(String, PoolItem)> = pool
        .into_iter()
        .map(|item| {
            let h = blake3::hash(format!("{seed}:{}", item.id).as_bytes())
                .to_hex()
                .to_string();
            (h, item)
        })
        .collect();
    hashed.sort_by(|a, b| a.0.cmp(&b.0));

    let mut sample = Vec::new();
    for (_, item) in hashed {
        if sample.len() >= n {
            break;
        }
        let prompt: String = item.content.chars().take(PROMPT_TRUNCATE_CHARS).collect();
        if mirror_is_harness_turn(&prompt) {
            counters.skipped_harness_turn_early_exit += 1;
            continue;
        }
        if mirror_is_continuation_prompt(&prompt) {
            counters.skipped_continuation += 1;
            continue;
        }
        if prompt.starts_with('/') {
            counters.skipped_slash += 1;
            continue;
        }
        if prompt.trim().len() < 3 {
            counters.skipped_short += 1;
            continue;
        }
        sample.push(SampleItem {
            chunk_id: item.id,
            conversation_id: item.conversation_id,
            prompt,
        });
    }
    sample
}

// ─── Readback ───

struct RetrievedRow {
    memory_id: String,
}

fn readback_retrieved(conn: &Connection, session_id: &str) -> Result<Vec<RetrievedRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT it.memory_id \
             FROM rerank_exposure_items it \
             JOIN rerank_exposure_impressions imp ON imp.impression_id = it.impression_id \
             WHERE imp.session_id = ?1 AND imp.surface = 'prompt_submit' \
             ORDER BY imp.shown_at, it.rank",
        )
        .map_err(|e| format!("prepare retrieved query: {e}"))?;
    let rows = stmt
        .query_map([session_id], |row| {
            Ok(RetrievedRow {
                memory_id: row.get(0)?,
            })
        })
        .map_err(|e| format!("query retrieved set: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("read retrieved row: {e}"))?);
    }
    Ok(out)
}

fn readback_rendered(conn: &Connection, session_id: &str) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT memory_id FROM retrieval_events \
             WHERE session_id = ?1 AND hook_phase = 'prompt_submit'",
        )
        .map_err(|e| format!("prepare rendered query: {e}"))?;
    let rows = stmt
        .query_map([session_id], |row| row.get::<_, String>(0))
        .map_err(|e| format!("query rendered set: {e}"))?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.map_err(|e| format!("read rendered row: {e}"))?);
    }
    Ok(out)
}

fn fetch_chunk_projects(
    conn: &Connection,
    ids: &HashSet<String>,
) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let ids: Vec<&String> = ids.iter().collect();
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT id, project_name FROM chunks WHERE id IN ({placeholders})");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("prepare chunk-project lookup: {e}"))?;
    let params: Vec<&dyn rusqlite::ToSql> =
        ids.iter().map(|s| *s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("query chunk-project lookup: {e}"))?;
    for r in rows {
        let (id, project) = r.map_err(|e| format!("read chunk-project row: {e}"))?;
        out.insert(id, project);
    }
    Ok(out)
}

// ─── Output shapes ───

const NOT_A_CHUNK_KEY: &str = "_not_a_chunk";

#[derive(Serialize)]
struct ScoreLine {
    chunk_id: String,
    conversation_id: String,
    stored_project: String,
    retrieved_rank: Option<usize>,
    rendered: bool,
    n_retrieved: usize,
    exposed_projects: BTreeMap<String, i64>,
}

#[derive(Serialize)]
struct SamplingFilters {
    source: &'static str,
    is_sidechain: i64,
    requires_chunk_embedding: bool,
    age_min_days: f64,
    age_max_days: f64,
    content_length_min_chars: i64,
    content_length_max_chars: i64,
    opens_with_harness_block_excluded: bool,
    self_referential_noise_excluded: bool,
    sample_project: String,
    requested_n: usize,
    seed: u64,
    cwd: String,
    projects_dir: String,
    prompt_truncate_chars: usize,
    now: String,
}

#[derive(Serialize)]
struct SummaryBody {
    label: String,
    n_sampled: usize,
    n_skipped: u64,
    skipped_harness_opening: u64,
    skipped_harness_turn_early_exit: u64,
    skipped_self_referential: u64,
    skipped_short: u64,
    skipped_slash: u64,
    skipped_continuation: u64,
    excluded_age_window: u64,
    exposure_rows_missing_sessions: usize,
    hit_at_1: f64,
    hit_at_5: f64,
    hit_at_15: f64,
    mrr: f64,
    rendered_rate: f64,
    exposed_project_totals: BTreeMap<String, i64>,
    source_project_items_exposed: i64,
    sampling_filters: SamplingFilters,
    retrieved_set_semantics: String,
    mirrored_predicates: Vec<String>,
}

#[derive(Serialize)]
struct Provenance {
    build_commit: String,
    build_dirty: bool,
    embedding_model: String,
    db_path: String,
    db_size_bytes: u64,
    index_dir_size_bytes: u64,
    /// `index/manifest.json`'s own `created_at` (empty string if the manifest
    /// is missing or unreadable). Two arms whose clones came from different
    /// live index dumps carry different `created_at` values even when the DB
    /// clone step ran identically, because an HNSW re-dump changes the graph
    /// layout without changing the DB.
    index_manifest_created_at: String,
    index_manifest_chunk_embeddings_expected: u64,
    index_manifest_reflection_embeddings_expected: u64,
    /// blake3 of the raw `manifest.json` bytes — the sturdiest signal, since a
    /// re-dump can leave `created_at` and the `_expected` counts unchanged
    /// while `chunk_id_map` order (and therefore the HNSW graph) still moved.
    /// Empty string if the manifest is missing or unreadable.
    index_manifest_blake3: String,
    now: String,
    seed: u64,
    engine_init_secs: f64,
    results_hash: String,
}

/// Identity fields pulled from `index/manifest.json` so a base/fix arm pair
/// built from different live-index dumps is visible in the results instead
/// of silently producing a shifted HNSW top-k. Read as generic JSON, not the
/// crate's private `search::IndexManifest`, mirroring the read-only access
/// pattern already used by `search::mod`'s own tests.
struct IndexManifestIdentity {
    created_at: String,
    chunk_embeddings_expected: u64,
    reflection_embeddings_expected: u64,
    blake3: String,
}

fn read_index_manifest_identity(index_dir: &Path) -> IndexManifestIdentity {
    let manifest_path = index_dir.join("manifest.json");
    let raw = match std::fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!(
                "hook_recall_replay: no index manifest at {} ({e}) — provenance will record it as absent",
                manifest_path.display()
            );
            return IndexManifestIdentity {
                created_at: String::new(),
                chunk_embeddings_expected: 0,
                reflection_embeddings_expected: 0,
                blake3: String::new(),
            };
        }
    };
    let blake3_hex = blake3::hash(&raw).to_hex().to_string();
    let value: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "hook_recall_replay: index manifest at {} is not valid JSON ({e}) — \
                 created_at/expected counts will be empty, the raw-byte hash is still recorded",
                manifest_path.display()
            );
            return IndexManifestIdentity {
                created_at: String::new(),
                chunk_embeddings_expected: 0,
                reflection_embeddings_expected: 0,
                blake3: blake3_hex,
            };
        }
    };
    IndexManifestIdentity {
        created_at: value["created_at"].as_str().unwrap_or_default().to_string(),
        chunk_embeddings_expected: value["chunk_embeddings_expected"].as_u64().unwrap_or(0),
        reflection_embeddings_expected: value["reflection_embeddings_expected"]
            .as_u64()
            .unwrap_or(0),
        blake3: blake3_hex,
    }
}

#[derive(Serialize)]
struct SummaryOut {
    #[serde(flatten)]
    body: SummaryBody,
    provenance: Provenance,
}

fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.is_dir() {
                total += dir_size_bytes(&path);
            } else {
                total += meta.len();
            }
        }
    }
    total
}

fn mirrored_predicates_list() -> Vec<String> {
    vec![
        "hooks::prompt_submit::is_harness_turn (via transcript::instrumentation::is_noisy_steer_text + cross-session-message check)".to_string(),
        "hooks::prompt_submit::HARNESS_BLOCK_OPENINGS (is_self_echo's opening-prefix check)".to_string(),
        "hooks::prompt_submit::is_self_referential_noise's private NOISE_PATTERNS list (extraction::provenance::is_csr_emission is pub and called directly, not mirrored)".to_string(),
        "hooks::prompt_submit::is_continuation_prompt".to_string(),
        "hooks::prompt_submit::early_exit's slash-command and bare-acknowledgment (trim().len() < 3) guards".to_string(),
    ]
}

// ─── Main ───

#[tokio::main]
async fn main() {
    let config = match load_config() {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("hook_recall_replay: {msg}");
            std::process::exit(1);
        }
    };

    if let Err(msg) = run(config).await {
        eprintln!("hook_recall_replay: {msg}");
        std::process::exit(1);
    }
}

async fn run(config: Config) -> Result<(), String> {
    // Never let dreaming's consumptive delivery claim mutate state under replay.
    std::env::set_var("CSR_NO_DREAM_INJECT", "1");

    std::fs::create_dir_all(&config.out_dir)
        .map_err(|e| format!("create output dir {}: {e}", config.out_dir.display()))?;

    eprintln!(
        "hook_recall_replay: sampling candidates for project {}",
        config.sample_project
    );
    let sample_conn = open_ro(&config.db_path)?;
    let raw = fetch_raw_candidates(&sample_conn, &config.sample_project)?;
    let conversation_ids = fetch_conversation_ids(&sample_conn)?;
    drop(sample_conn);

    let mut counters = SkipCounters::default();
    let sample = build_sample(raw, config.now, config.seed, config.n, &mut counters);
    eprintln!(
        "hook_recall_replay: sampled {} prompts (n_skipped={})",
        sample.len(),
        counters.n_skipped()
    );

    // Derive session ids up front and check disjointness before touching the engine.
    let mut sessions: Vec<(SampleItem, String)> = Vec::new();
    for (i, item) in sample.into_iter().enumerate() {
        let sid = Uuid::new_v5(
            &SESSION_NAMESPACE,
            format!("{}:{}:{}", config.label, config.seed, i).as_bytes(),
        )
        .to_string();
        if conversation_ids.contains(&sid) {
            return Err(format!(
                "synthetic session id {sid} collides with an existing conversation_id — abort"
            ));
        }
        sessions.push((item, sid));
    }

    eprintln!(
        "hook_recall_replay: loading engine ({})",
        config.db_path.display()
    );
    let init_start = Instant::now();
    let engine = Engine::new(&config.db_path, &config.projects_dir)
        .map_err(|e| format!("Engine::new failed: {e}"))?;
    let engine_init_secs = init_start.elapsed().as_secs_f64();
    eprintln!("hook_recall_replay: engine loaded in {engine_init_secs:.3}s");

    // Run every hook call first — a harness-held read transaction during this
    // loop can make the hook's try_record_rerank_exposure return false.
    let cwd_string = config.cwd.to_string_lossy().to_string();
    for (i, (item, sid)) in sessions.iter().enumerate() {
        let input = HookInput {
            session_id: Some(sid.clone()),
            prompt: Some(item.prompt.clone()),
            cwd: Some(cwd_string.clone()),
            transcript_path: None,
            ..Default::default()
        };
        if let Err(e) = prompt_submit::handle(&input, &engine, &config.cwd).await {
            eprintln!(
                "hook_recall_replay: handle() errored for session {sid} (chunk {}): {e}",
                item.chunk_id
            );
        }
        if (i + 1) % 25 == 0 || i + 1 == sessions.len() {
            eprintln!(
                "hook_recall_replay: {}/{} hook calls done",
                i + 1,
                sessions.len()
            );
        }
    }

    // Readback only after every hook call has returned.
    eprintln!("hook_recall_replay: reading back exposure/rendered state");
    let readback_conn = open_ro(&config.db_path)?;

    let mut lines: Vec<ScoreLine> = Vec::new();
    let mut exposure_rows_missing_sessions = 0usize;
    let mut hits_1 = 0usize;
    let mut hits_5 = 0usize;
    let mut hits_15 = 0usize;
    let mut reciprocal_sum = 0f64;
    let mut rendered_count = 0usize;
    let mut exposed_project_totals: BTreeMap<String, i64> = BTreeMap::new();
    let mut source_project_items_exposed: i64 = 0;

    for (item, sid) in &sessions {
        let retrieved = readback_retrieved(&readback_conn, sid)?;
        let rendered_set = readback_rendered(&readback_conn, sid)?;

        let mut ids: HashSet<String> = retrieved.iter().map(|r| r.memory_id.clone()).collect();
        ids.insert(item.chunk_id.clone());
        let chunk_projects = fetch_chunk_projects(&readback_conn, &ids)?;

        let n_retrieved = retrieved.len();
        if n_retrieved == 0 {
            exposure_rows_missing_sessions += 1;
        }

        let retrieved_rank = retrieved.iter().position(|r| r.memory_id == item.chunk_id);
        let rendered = rendered_set.contains(&item.chunk_id);

        if let Some(rank) = retrieved_rank {
            if rank < 1 {
                hits_1 += 1;
            }
            if rank < 5 {
                hits_5 += 1;
            }
            if rank < 15 {
                hits_15 += 1;
            }
            reciprocal_sum += 1.0 / (rank as f64 + 1.0);
        }
        if rendered {
            rendered_count += 1;
        }

        let mut exposed_projects: BTreeMap<String, i64> = BTreeMap::new();
        for r in &retrieved {
            let key = chunk_projects
                .get(&r.memory_id)
                .cloned()
                .unwrap_or_else(|| NOT_A_CHUNK_KEY.to_string());
            *exposed_projects.entry(key.clone()).or_insert(0) += 1;
            *exposed_project_totals.entry(key.clone()).or_insert(0) += 1;
            if key == config.sample_project {
                source_project_items_exposed += 1;
            }
        }

        lines.push(ScoreLine {
            chunk_id: item.chunk_id.clone(),
            conversation_id: item.conversation_id.clone(),
            stored_project: config.sample_project.clone(),
            retrieved_rank,
            rendered,
            n_retrieved,
            exposed_projects,
        });
    }
    drop(readback_conn);

    let n_sampled = sessions.len();
    let denom = n_sampled.max(1) as f64;
    let hit_at_1 = hits_1 as f64 / denom;
    let hit_at_5 = hits_5 as f64 / denom;
    let hit_at_15 = hits_15 as f64 / denom;
    let mrr = reciprocal_sum / denom;
    let rendered_rate = rendered_count as f64 / denom;

    // scores.ndjson — write the exact bytes we will also hash.
    let mut scores_bytes: Vec<u8> = Vec::new();
    for line in &lines {
        let s = serde_json::to_string(line).map_err(|e| format!("serialize score line: {e}"))?;
        scores_bytes.extend_from_slice(s.as_bytes());
        scores_bytes.push(b'\n');
    }
    let scores_path = config.out_dir.join("scores.ndjson");
    std::fs::write(&scores_path, &scores_bytes)
        .map_err(|e| format!("write {}: {e}", scores_path.display()))?;

    let sampling_filters = SamplingFilters {
        source: "conversation",
        is_sidechain: 0,
        requires_chunk_embedding: true,
        age_min_days: 0.0,
        age_max_days: SAMPLE_AGE_MAX_DAYS,
        content_length_min_chars: CONTENT_LEN_MIN,
        content_length_max_chars: CONTENT_LEN_MAX,
        opens_with_harness_block_excluded: true,
        self_referential_noise_excluded: true,
        sample_project: config.sample_project.clone(),
        requested_n: config.n,
        seed: config.seed,
        cwd: cwd_string.clone(),
        projects_dir: config.projects_dir.to_string_lossy().to_string(),
        prompt_truncate_chars: PROMPT_TRUNCATE_CHARS,
        now: config.now_raw.clone(),
    };

    let body = SummaryBody {
        label: config.label.clone(),
        n_sampled,
        n_skipped: counters.n_skipped(),
        skipped_harness_opening: counters.skipped_harness_opening,
        skipped_harness_turn_early_exit: counters.skipped_harness_turn_early_exit,
        skipped_self_referential: counters.skipped_self_referential,
        skipped_short: counters.skipped_short,
        skipped_slash: counters.skipped_slash,
        skipped_continuation: counters.skipped_continuation,
        excluded_age_window: counters.excluded_age_window,
        exposure_rows_missing_sessions,
        hit_at_1,
        hit_at_5,
        hit_at_15,
        mrr,
        rendered_rate,
        exposed_project_totals,
        source_project_items_exposed,
        sampling_filters,
        retrieved_set_semantics: "rerank_exposure_items holds the preface + RENDERED items the \
            hook actually wrote (post scored.iter().take(5), post dedup, post noise filter, \
            post the 500-token PROMPT_TOKEN_BUDGET) — NOT the 15-result internal candidate list. \
            hit_at_15 therefore means 'hit anywhere in the recorded list', not 'hit in the top 15 \
            search candidates'; hit_at_5 is the operative metric."
            .to_string(),
        mirrored_predicates: mirrored_predicates_list(),
    };

    let body_json =
        serde_json::to_string(&body).map_err(|e| format!("serialize summary body: {e}"))?;
    let mut hash_input = body_json.into_bytes();
    hash_input.extend_from_slice(&scores_bytes);
    let results_hash = blake3::hash(&hash_input).to_hex().to_string();

    let db_size_bytes = std::fs::metadata(&config.db_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let index_dir = config
        .db_path
        .parent()
        .map(|p| p.join("index"))
        .unwrap_or_else(|| PathBuf::from("index"));
    let index_dir_size_bytes = dir_size_bytes(&index_dir);
    let index_manifest = read_index_manifest_identity(&index_dir);

    let provenance = Provenance {
        build_commit: env!("CSR_BUILD_GIT_SHA").to_string(),
        build_dirty: env!("CSR_BUILD_GIT_DIRTY") == "true",
        embedding_model: "all-MiniLM-L6-v2".to_string(),
        db_path: config.db_path.to_string_lossy().to_string(),
        db_size_bytes,
        index_dir_size_bytes,
        index_manifest_created_at: index_manifest.created_at,
        index_manifest_chunk_embeddings_expected: index_manifest.chunk_embeddings_expected,
        index_manifest_reflection_embeddings_expected: index_manifest
            .reflection_embeddings_expected,
        index_manifest_blake3: index_manifest.blake3,
        now: config.now_raw.clone(),
        seed: config.seed,
        engine_init_secs,
        results_hash: results_hash.clone(),
    };

    let out = SummaryOut { body, provenance };
    let summary_json =
        serde_json::to_string_pretty(&out).map_err(|e| format!("serialize summary: {e}"))?;
    let summary_path = config.out_dir.join("summary.json");
    let mut f = std::fs::File::create(&summary_path)
        .map_err(|e| format!("create {}: {e}", summary_path.display()))?;
    f.write_all(summary_json.as_bytes())
        .map_err(|e| format!("write {}: {e}", summary_path.display()))?;

    eprintln!(
        "hook_recall_replay: wrote {} lines to {} and summary to {} (results_hash={results_hash})",
        lines.len(),
        scores_path.display(),
        summary_path.display()
    );
    eprintln!(
        "hook_recall_replay: n_sampled={n_sampled} n_skipped={} exposure_rows_missing_sessions={exposure_rows_missing_sessions} \
         hit@1={hit_at_1:.3} hit@5={hit_at_5:.3} hit@15={hit_at_15:.3} mrr={mrr:.3} rendered_rate={rendered_rate:.3}",
        out.body.n_skipped
    );

    Ok(())
}
