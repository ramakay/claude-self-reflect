//! Saga relitigation harness — two-arm retrieval-context generator for a
//! blinded behavioral benchmark.
//!
//! Arm `reinstatement` calls the production two-hop walk
//! (`search::reinstatement::reinstate`) directly through its public API. Arm
//! `knn` is a similarity-only baseline — `search_chunks` + `search_reflections`,
//! max-score merge across the two channels, no hop-2 spread, no rerank —
//! mirroring the `a_knn` / `knn_exact` baseline arm in `examples/saga_ablation.rs`
//! and `examples/saga_contamination.rs`.
//!
//! Output format is IDENTICAL across arms (advisor-binding arm-identity
//! condition): same context.md header, same per-item template, no scores
//! rendered and no arm-specific wording anywhere in the visible text, so a
//! blinded grader cannot infer which arm produced a given file from its
//! formatting alone. Only the TSV (an internal artifact, never shown to a
//! grader) carries the score.
//!
//! Read-only against the DB: never imports, enriches, or stores anything.
//! `Storage` has no read-only open mode (only `Storage::open`, which is
//! writable WAL); this harness simply never calls a mutating method.
//!
//! Run:
//!   cargo run --release --example saga_relitigation -- \
//!     --db <path> --tasks <tasks.json> --arm <reinstatement|knn> \
//!     --out <dir> [--topk 10]

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use csr_engine::engine::Engine;
use csr_engine::search::reinstatement::{reinstate, ReinstateConfig};
use csr_engine::storage::queries::ResolutionEntry;
use serde::Deserialize;

/// Matches `ReinstateConfig::default().min_score` / saga_ablation.rs's MIN_SCORE.
const MIN_SCORE: f32 = 0.20;

/// RRF pool size per channel (vector, FTS) before fusion — see `walk_knn`.
const RRF_POOL_SIZE: usize = 20;

/// RRF's rank-damping constant. Standard value from Cormack et al. 2009
/// ("Reciprocal Rank Fusion outperforms Condorcet..."); also matches the
/// `docs/plans/saga-t3-preregistration.md` merge-rule spec verbatim
/// (`score = Σ 1/(60+rank)`).
const RRF_K: f32 = 60.0;

/// Only the fields this harness needs. Extra grading-metadata fields on each
/// task object are ignored automatically (serde skips unknown fields unless
/// `deny_unknown_fields` is set, which we do not set).
#[derive(Deserialize)]
struct Task {
    task_id: String,
    prompt: String,
}

/// Task-file input shape. Accepts either a bare JSON array of tasks or a
/// `{"tasks": [...]}` wrapper object — both forms have been used by different
/// question-set generation passes for this benchmark family, and the harness
/// should not care which one it's handed. Untagged: serde tries each variant
/// in order and takes the first that parses.
#[derive(Deserialize)]
#[serde(untagged)]
enum TasksFile {
    Wrapped { tasks: Vec<Task> },
    Bare(Vec<Task>),
}

impl TasksFile {
    fn into_tasks(self) -> Vec<Task> {
        match self {
            TasksFile::Wrapped { tasks } => tasks,
            TasksFile::Bare(tasks) => tasks,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arm {
    Reinstatement,
    Knn,
    /// Ablation: arm K's vector channel alone (no FTS, no RRF).
    KnnVec,
    /// Ablation: arm K's FTS channel alone (no vector, no RRF).
    KnnFts,
}

impl Arm {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "reinstatement" => Ok(Arm::Reinstatement),
            "knn" => Ok(Arm::Knn),
            "knn-vec" => Ok(Arm::KnnVec),
            "knn-fts" => Ok(Arm::KnnFts),
            other => {
                bail!("--arm must be 'reinstatement', 'knn', 'knn-vec' or 'knn-fts', got '{other}'")
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Arm::Reinstatement => "reinstatement",
            Arm::Knn => "knn",
            Arm::KnnVec => "knn-vec",
            Arm::KnnFts => "knn-fts",
        }
    }
}

#[cfg(test)]
mod arm_tests {
    use super::Arm;

    #[test]
    fn parse_as_str_round_trips_all_variants() {
        for arm in [Arm::Reinstatement, Arm::Knn, Arm::KnnVec, Arm::KnnFts] {
            assert!(matches!(Arm::parse(arm.as_str()), Ok(a) if a == arm));
        }
    }

    #[test]
    fn invalid_arm_error_lists_ablation_names() {
        let err = Arm::parse("bogus").unwrap_err().to_string();
        assert!(err.contains("knn-vec") && err.contains("knn-fts"));
    }
}

/// One ranked evidence row — the arm-invariant shape rendered into both
/// context.md and the TSV. Neither `score` nor `via` (the reinstatement walk's
/// origin channel) is ever printed into context.md — only used for TSV/sort.
#[derive(Clone)]
struct EvidenceRow {
    id: String,
    conversation_id: String,
    score: f32,
    timestamp: Option<String>,
    excerpt: String,
}

/// First ~200 chars of content, newlines/carriage-returns flattened to spaces.
/// Mirrors the private `clean_excerpt` in `search::reinstatement` so both arms
/// render excerpts identically (reinstatement's own excerpts already went
/// through that exact function; this duplicate keeps the knn arm's excerpts
/// on the same footing without needing a visibility change upstream).
fn clean_excerpt(content: &str) -> String {
    let s: String = content.chars().take(200).collect();
    s.replace(['\n', '\r'], " ")
}

fn push_max(pool: &mut HashMap<String, EvidenceRow>, row: EvidenceRow) {
    pool.entry(row.id.clone())
        .and_modify(|e| {
            if row.score > e.score {
                *e = row.clone();
            }
        })
        .or_insert(row);
}

/// Deterministic total order: score desc, then id asc. HashMap iteration order
/// is process-randomized (Rust's default `RandomState`), so a plain score sort
/// alone could reorder exact-score ties across runs; the id tie-break makes
/// output byte-identical across repeated runs regardless of that randomization.
fn sort_deterministic(rows: &mut [EvidenceRow]) {
    rows.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
}

/// Arm K: similarity-only kNN, PLUS the preregistration's granted FTS/substring
/// access (docs/plans/saga-t3-preregistration.md, "Arms" §2: "K retains
/// substring/FTS access — commit hashes in chunk text are searchable text for
/// K; only *typed edge traversal* is exclusive to R").
///
/// Two independently-ranked channels feed a Reciprocal Rank Fusion merge:
///   1. Vector channel: `search_chunks` + `search_reflections`, max-score
///      merged per id, sorted, pool capped at `RRF_POOL_SIZE`.
///   2. FTS channel: `Storage::fts5_search` (chunks_fts, top `RRF_POOL_SIZE`)
///      over the raw task prompt — this is the "substring/FTS access" the
///      preregistration grants arm K; it is the only channel that can surface
///      a commit hash the vector embedding didn't rank highly.
///
/// Merge rule (preregistration verbatim): `score = Σ 1/(60+rank)` across the
/// two ranked lists (1-indexed rank per list), summed when an id appears in
/// both; deduped by chunk/reflection id; final list truncated to `k`. No
/// hop-2 spread (blend/graph/episode), no provenance rerank, no echo
/// demotion — those remain exclusive to arm R.
/// Which of arm K's channels to run — `Fused` is the pre-registered arm;
/// the single-channel modes exist only for the post-hoc FTS-vs-vector
/// ablation (exploratory, labeled as such in the results doc).
#[derive(Clone, Copy, PartialEq, Eq)]
enum KnnMode {
    Fused,
    VecOnly,
    FtsOnly,
}

async fn walk_knn(
    engine: &Engine,
    query: &str,
    query_vec: &[f32],
    k: usize,
    mode: KnnMode,
) -> Result<Vec<EvidenceRow>> {
    let storage = engine.storage();

    // FTS-only ablation short-circuits before any vector work — no HNSW
    // search, no metadata joins, and (via main's gating) no query embedding.
    if mode == KnnMode::FtsOnly {
        let fts_chunks = storage.fts5_search(query, RRF_POOL_SIZE, None)?;
        let mut rows: Vec<EvidenceRow> = fts_chunks
            .iter()
            .enumerate()
            .map(|(i, c)| EvidenceRow {
                id: c.id.clone(),
                conversation_id: c.conversation_id.clone(),
                // fts.rank order preserved as a descending score; TSV-only.
                score: 1.0 / (i as f32 + 1.0),
                timestamp: Some(c.timestamp.clone()),
                excerpt: clean_excerpt(&c.content),
            })
            .collect();
        rows.truncate(k);
        return Ok(rows);
    }

    let (chunk_hits, reflection_hits) = {
        let idx = engine.search().read().await;
        let chunks = idx.search_chunks(query_vec, RRF_POOL_SIZE, MIN_SCORE);
        let reflections = idx.search_reflections(query_vec, RRF_POOL_SIZE, MIN_SCORE);
        (chunks, reflections)
    };

    let mut vector_pool: HashMap<String, EvidenceRow> = HashMap::new();

    let chunk_ids: Vec<String> = chunk_hits.iter().map(|r| r.id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &csr_engine::import::ConversationChunk> =
        chunk_meta.iter().map(|c| (c.id.as_str(), c)).collect();

    for r in &chunk_hits {
        if let Some(c) = meta_by_id.get(r.id.as_str()) {
            push_max(
                &mut vector_pool,
                EvidenceRow {
                    id: r.id.clone(),
                    conversation_id: c.conversation_id.clone(),
                    score: r.score,
                    timestamp: Some(c.timestamp.clone()),
                    excerpt: clean_excerpt(&c.content),
                },
            );
        }
    }
    for r in &reflection_hits {
        if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(&r.id) {
            let conv = tags
                .iter()
                .find_map(|t| t.strip_prefix("conv_").map(str::to_string))
                .unwrap_or_else(|| format!("refl_{}", r.id));
            push_max(
                &mut vector_pool,
                EvidenceRow {
                    id: r.id.clone(),
                    conversation_id: conv,
                    score: r.score,
                    timestamp: Some(timestamp),
                    excerpt: clean_excerpt(&content),
                },
            );
        }
    }
    let mut vector_rows: Vec<EvidenceRow> = vector_pool.into_values().collect();
    sort_deterministic(&mut vector_rows);
    vector_rows.truncate(RRF_POOL_SIZE);

    if mode == KnnMode::VecOnly {
        vector_rows.truncate(k);
        return Ok(vector_rows);
    }

    // FTS channel — chunks_fts only covers `chunks`, not reflections, so this
    // channel is chunk-only by construction. `fts5_search` already orders by
    // `fts.rank` (best match first), so list position IS the rank.
    let fts_chunks = storage.fts5_search(query, RRF_POOL_SIZE, None)?;

    let mut rrf_score: HashMap<String, f32> = HashMap::new();
    let mut meta: HashMap<String, EvidenceRow> = HashMap::new();

    for (i, row) in vector_rows.iter().enumerate() {
        let rank = (i + 1) as f32;
        *rrf_score.entry(row.id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank);
        meta.entry(row.id.clone()).or_insert_with(|| row.clone());
    }
    for (i, c) in fts_chunks.iter().enumerate() {
        let rank = (i + 1) as f32;
        *rrf_score.entry(c.id.clone()).or_insert(0.0) += 1.0 / (RRF_K + rank);
        meta.entry(c.id.clone()).or_insert_with(|| EvidenceRow {
            id: c.id.clone(),
            conversation_id: c.conversation_id.clone(),
            // Placeholder — overwritten with the fused RRF score below. Only
            // ever used for TSV/sort, never rendered into context.md.
            score: 0.0,
            timestamp: Some(c.timestamp.clone()),
            excerpt: clean_excerpt(&c.content),
        });
    }

    let mut rows: Vec<EvidenceRow> = rrf_score
        .into_iter()
        .filter_map(|(id, fused)| {
            meta.remove(&id).map(|mut row| {
                row.score = fused;
                row
            })
        })
        .collect();
    sort_deterministic(&mut rows);
    rows.truncate(k);
    Ok(rows)
}

/// Arm R: the production reinstatement walk, called through its public API
/// exactly as shipped (no ablation flags) — hop-1 seed retrieval, hop-2
/// blend/graph/episode spread, provenance rerank, echo demotion.
async fn walk_reinstatement(engine: &Engine, query: &str, k: usize) -> Result<Vec<EvidenceRow>> {
    let cfg = ReinstateConfig {
        k,
        ..ReinstateConfig::default()
    };
    // Project scope is always None in this harness (cross-project parity with
    // `reflect_on_past`, matching saga_ablation.rs / saga_contamination.rs).
    let items = reinstate(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        query,
        None,
        &cfg,
    )
    .await?;

    let mut rows: Vec<EvidenceRow> = items
        .into_iter()
        .map(|it| EvidenceRow {
            id: it.chunk_id,
            conversation_id: it.conversation_id,
            score: it.score,
            timestamp: Some(it.timestamp),
            excerpt: it.excerpt,
        })
        .collect();
    sort_deterministic(&mut rows);
    Ok(rows)
}

/// Arm-invariant context block. Same header, same per-item template, no
/// scores, no arm-specific wording — a blinded grader sees identical shape
/// for both arms; only the ranked content (and which conversations appear)
/// differs.
///
/// Per the preregistration ("R: ... with the resolution ledger loaded (T2
/// verdicts written before any arm runs)"), each rendered chunk's ledger
/// verdict (if any) is appended as a `status: <verdict> (<date>)` line. This
/// rendering path is called identically for both arms — the ledger lookup
/// itself is arm-blind (keyed only by chunk id), so there is no arm-
/// conditional branch here, matching the file's advisor-binding arm-identity
/// invariant above.
fn render_context_md(
    task_id: &str,
    prompt: &str,
    rows: &[EvidenceRow],
    ledger: &HashMap<String, ResolutionEntry>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Retrieval Context: {task_id}\n\n"));
    out.push_str(&format!("Query: {prompt}\n\n"));
    out.push_str("## Evidence\n\n");
    for (i, row) in rows.iter().enumerate() {
        let rank = i + 1;
        match &row.timestamp {
            Some(ts) => out.push_str(&format!(
                "{rank}. [{ts}] conversation: {}\n",
                row.conversation_id
            )),
            None => out.push_str(&format!("{rank}. conversation: {}\n", row.conversation_id)),
        }
        out.push_str(&format!("   {}\n", row.excerpt));
        if let Some(entry) = ledger.get(&row.id) {
            let date = if entry.created_at.len() >= 10 {
                &entry.created_at[..10]
            } else {
                entry.created_at.as_str()
            };
            out.push_str(&format!("   status: {} ({date})\n", entry.status));
        }
        out.push('\n');
    }
    out
}

struct Args {
    db: PathBuf,
    tasks: PathBuf,
    arm: Arm,
    out: PathBuf,
    topk: usize,
}

fn parse_args() -> Result<Args> {
    let mut db = None;
    let mut tasks = None;
    let mut arm = None;
    let mut out = None;
    let mut topk = 10usize;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db = Some(PathBuf::from(it.next().context("--db requires a value")?)),
            "--tasks" => {
                tasks = Some(PathBuf::from(
                    it.next().context("--tasks requires a value")?,
                ))
            }
            "--arm" => arm = Some(Arm::parse(&it.next().context("--arm requires a value")?)?),
            "--out" => out = Some(PathBuf::from(it.next().context("--out requires a value")?)),
            "--topk" => {
                topk = it
                    .next()
                    .context("--topk requires a value")?
                    .parse()
                    .context("--topk must be a non-negative integer")?
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    Ok(Args {
        db: db.context("--db is required")?,
        tasks: tasks.context("--tasks is required")?,
        arm: arm.context("--arm is required (reinstatement|knn)")?,
        out: out.context("--out is required")?,
        topk,
    })
}

/// Same default as `main.rs`'s `default_projects_dir` — this harness's CLI has
/// no `--projects` flag (not part of the spec'd interface), and `Engine::new`'s
/// retrieval path here never touches it (no import, no watcher), so pointing
/// at the real default keeps behavior consistent with production without
/// adding a flag that would go unused.
fn default_projects_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = parse_args()?;

    fs::create_dir_all(&args.out)
        .with_context(|| format!("create output dir {}", args.out.display()))?;

    let tasks_raw = fs::read_to_string(&args.tasks)
        .with_context(|| format!("read tasks file {}", args.tasks.display()))?;
    let tasks: Vec<Task> = serde_json::from_str::<TasksFile>(&tasks_raw)
        .context(
            "parse tasks json (expected a bare array of {task_id, prompt, ...} \
             or a {\"tasks\": [...]} wrapper object)",
        )?
        .into_tasks();
    if tasks.is_empty() {
        bail!("tasks file contains zero tasks");
    }

    eprintln!(
        "loading engine (db={}, arm={})...",
        args.db.display(),
        args.arm.as_str()
    );
    let engine = Engine::new(&args.db, &default_projects_dir())?;

    let tsv_path = args
        .out
        .join(format!("retrieval.{}.tsv", args.arm.as_str()));
    let mut tsv =
        File::create(&tsv_path).with_context(|| format!("create {}", tsv_path.display()))?;
    writeln!(tsv, "task_id\trank\tconversation_id\tchunk_id\tscore")?;

    let n = tasks.len();
    for (i, task) in tasks.iter().enumerate() {
        // Only the vector-backed knn arms embed the prompt here: knn-fts must
        // stay embedding-free (that's the ablation), and reinstatement embeds
        // internally via `reinstate`.
        let query_vec = match args.arm {
            Arm::Knn | Arm::KnnVec => engine.embeddings().embed_single(&task.prompt)?,
            Arm::KnnFts | Arm::Reinstatement => Vec::new(),
        };

        let rows = match args.arm {
            Arm::Knn => {
                walk_knn(&engine, &task.prompt, &query_vec, args.topk, KnnMode::Fused).await?
            }
            Arm::KnnVec => {
                walk_knn(
                    &engine,
                    &task.prompt,
                    &query_vec,
                    args.topk,
                    KnnMode::VecOnly,
                )
                .await?
            }
            Arm::KnnFts => {
                walk_knn(
                    &engine,
                    &task.prompt,
                    &query_vec,
                    args.topk,
                    KnnMode::FtsOnly,
                )
                .await?
            }
            Arm::Reinstatement => walk_reinstatement(&engine, &task.prompt, args.topk).await?,
        };

        // Ledger lookup runs identically for both arms (see `render_context_md`
        // doc comment) — the preregistration requires both arms to run "with
        // the resolution ledger loaded", not just R.
        let chunk_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let ledger = engine.storage().get_resolutions_batch(&chunk_ids)?;

        let md = render_context_md(&task.task_id, &task.prompt, &rows, &ledger);
        let md_path = args
            .out
            .join(format!("{}.{}.context.md", task.task_id, args.arm.as_str()));
        fs::write(&md_path, md).with_context(|| format!("write {}", md_path.display()))?;

        for (r, row) in rows.iter().enumerate() {
            writeln!(
                tsv,
                "{}\t{}\t{}\t{}\t{:.6}",
                task.task_id,
                r + 1,
                row.conversation_id,
                row.id,
                row.score
            )?;
        }

        eprintln!("{}/{} done ({})", i + 1, n, task.task_id);
    }

    tsv.flush()?;
    eprintln!(
        "wrote context files + {} to {} ({} tasks, arm={})",
        tsv_path.display(),
        args.out.display(),
        n,
        args.arm.as_str()
    );
    Ok(())
}
