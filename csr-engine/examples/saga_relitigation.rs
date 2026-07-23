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
use serde::Deserialize;

/// Matches `ReinstateConfig::default().min_score` / saga_ablation.rs's MIN_SCORE.
const MIN_SCORE: f32 = 0.20;

/// Only the fields this harness needs. Extra grading-metadata fields on each
/// task object are ignored automatically (serde skips unknown fields unless
/// `deny_unknown_fields` is set, which we do not set).
#[derive(Deserialize)]
struct Task {
    task_id: String,
    prompt: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    Reinstatement,
    Knn,
}

impl Arm {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "reinstatement" => Ok(Arm::Reinstatement),
            "knn" => Ok(Arm::Knn),
            other => bail!("--arm must be 'reinstatement' or 'knn', got '{other}'"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Arm::Reinstatement => "reinstatement",
            Arm::Knn => "knn",
        }
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

/// Arm K: similarity-only kNN. `search_chunks` + `search_reflections`, merged
/// by max score per id, sorted, truncated to `k`. No hop-2 spread (blend/graph/
/// episode), no provenance rerank, no echo demotion — the plain-recall baseline.
async fn walk_knn(engine: &Engine, query_vec: &[f32], k: usize) -> Result<Vec<EvidenceRow>> {
    let (chunk_hits, reflection_hits) = {
        let idx = engine.search().read().await;
        let chunks = idx.search_chunks(query_vec, k, MIN_SCORE);
        let reflections = idx.search_reflections(query_vec, k, MIN_SCORE);
        (chunks, reflections)
    };

    let storage = engine.storage();
    let mut pool: HashMap<String, EvidenceRow> = HashMap::new();

    let chunk_ids: Vec<String> = chunk_hits.iter().map(|r| r.id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &csr_engine::import::ConversationChunk> =
        chunk_meta.iter().map(|c| (c.id.as_str(), c)).collect();

    for r in &chunk_hits {
        if let Some(c) = meta_by_id.get(r.id.as_str()) {
            push_max(
                &mut pool,
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
                &mut pool,
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

    let mut rows: Vec<EvidenceRow> = pool.into_values().collect();
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
fn render_context_md(task_id: &str, prompt: &str, rows: &[EvidenceRow]) -> String {
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
        out.push_str(&format!("   {}\n\n", row.excerpt));
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
    let tasks: Vec<Task> = serde_json::from_str(&tasks_raw)
        .context("parse tasks json (expected an array of {task_id, prompt, ...})")?;
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
        let query_vec = engine.embeddings().embed_single(&task.prompt)?;

        let rows = match args.arm {
            Arm::Knn => walk_knn(&engine, &query_vec, args.topk).await?,
            Arm::Reinstatement => walk_reinstatement(&engine, &task.prompt, args.topk).await?,
        };

        let md = render_context_md(&task.task_id, &task.prompt, &rows);
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
