//! WP-A stage probe: where does a target conversation drop out of
//! `csr_reflect_on_past`, and does the miss generalize across the corpus?
//!
//! Runs the REAL pipeline (`csr_engine::mcp::tools::reflect_on_past`)
//! instrumented via `csr_engine::mcp::stage_probe`, never a reimplementation
//! of it — see that module's doc for why a reimplementation would risk
//! silently drifting from production behavior. Read-only against whatever
//! `--db` points at; this repo's convention is a `/tmp` CLONE
//! (`VACUUM INTO` + a copy of the index dir), never the live database.
//!
//!   cargo run --release --example probe_codex_recall -- \
//!       --db /tmp/csr-wpa/csr-engine.db \
//!       --projects-dir /Users/ramakrishnanannaswamy/.claude/projects \
//!       --sample 20
//!
//! Optional `--paraphrases <path>`: a `{"<conversation_id>": "<paraphrase>"}`
//! JSON map (gitignored, lives under /tmp). Conversations missing an entry
//! fall back to a proxy built from their own first chunk — clearly labeled
//! `self-text-proxy` in the output, NOT a paraphrase. A self-text query is
//! an upper bound on hit@6 (near-identical text is easier to recall than a
//! genuine reformulation), not a paraphrase-robustness measurement; treat
//! `self-text-proxy` rows as optimistic relative to hand-authored ones.
//!
//! # Reviewer-fix notes (second pass over the WP-A probe)
//!
//! - `losing_stage` used to fire only on a present->absent transition, which
//!   `rerank`/`fuse_rrf` (reorder, never remove) can never produce — every
//!   loss got misattributed to `final_page`. It's now: the first stage
//!   (in pipeline order) where the target's best rank is missing OR has
//!   reached/passed `LIMIT`, UNLESS the target is present on the actual
//!   final page (that overrides any earlier dip — a later stage can and
//!   does recover rank).
//! - `in_page` used to substring-match the target's raw UUID against the
//!   rendered output, which any OTHER result quoting the query text (e.g. a
//!   conversation ABOUT this exact investigation) can satisfy even when the
//!   target itself is absent. It's now read directly off the `final_page`
//!   stage sample's `target_rank`.
//! - The target query is measured BOTH contaminated (full corpus, as-is)
//!   and decontaminated (excluding conversations whose content contains the
//!   target's own uuid, or whose first-seen timestamp is after the target
//!   conversation's own last timestamp — i.e. conversations that could only
//!   be ABOUT this investigation, not a genuine pre-existing corpus hit).
//!   Decontamination is measured at the brute-force ground-truth layer
//!   (fully under this probe's control); the live `reflect_on_past` pass
//!   still runs over the whole (contaminated) corpus, since excluding
//!   conversations mid-pipeline would mean forking production code — the
//!   contamination receipt below makes that gap explicit rather than
//!   silently laundering a self-referential number.
//! - Brute-force ground truth now runs for every sampled conversation, not
//!   only the target, so an `hnsw_fetch` miss splits into an ANN-recall miss
//!   (brute force says the target WAS within the fetched window's rank
//!   budget; HNSW's approximation missed it) versus a semantic miss (brute
//!   force agrees nothing within that budget was close enough).
//! - The rerank stage now prints the per-term score breakdown (W_USER,
//!   supersedes, mechanic, scaffold, poison, primacy) for the target's best
//!   chunk and every chunk now ranked above it.
//! - `fuse_rrf` prints the target's FTS-side rank alongside its fused rank.

use anyhow::Result;
use csr_engine::engine::Engine;
use csr_engine::mcp::stage_probe::{self, StageSample};
use csr_engine::search::rerank::ScoreBreakdown;
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The conversation and query WP-A's diagnosis centers on.
const TARGET_CONVERSATION_ID: &str = "codex:01a0c0d5-9783-7fa1-8032-d12e19f919ea";
const TARGET_QUERY: &str = "codex blocked revival 2026 iOS deploy blocker where stuck";
const TARGET_PROJECT: &str = "EnhanceMe";
/// Short form of the target's uuid — what a transcript discussing this exact
/// investigation would quote, and what identifies a contaminating session.
const TARGET_UUID_SUBSTRING: &str = "01a0c0d5";

/// Fixed seed for the deterministic sample selection (blake3 of
/// `"{seed}:{conversation_id}"`, sorted ascending, first N per source).
/// Changing this changes which conversations get sampled — keep it stable
/// across probe runs so before/after tables are directly comparable.
const SAMPLE_SEED: &str = "wpa-recall-sample-v1";

/// `reflect_on_past`'s page size for every probe query — must match what the
/// stage-loss table and hit@N summary below both key off.
const LIMIT: usize = 6;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn sample_key(id: &str) -> [u8; 32] {
    blake3::hash(format!("{SAMPLE_SEED}:{id}").as_bytes()).into()
}

/// One row of the full corpus's embedding table, loaded ONCE and reused for
/// every brute-force ground-truth computation below (the target query plus
/// every sampled conversation) — the corpus doesn't change between queries,
/// only the query vector does.
struct EmbeddingRow {
    conversation_id: String,
    vector: Vec<f32>,
}

fn load_all_embeddings(conn: &Connection) -> Result<Vec<EmbeddingRow>> {
    let mut stmt = conn.prepare(
        "SELECT c.conversation_id, e.embedding FROM chunks c
         JOIN chunk_embeddings e ON e.chunk_id = c.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(EmbeddingRow {
            conversation_id: row.get::<_, String>(0)?,
            vector: blob_to_vec(&row.get::<_, Vec<u8>>(1)?),
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

struct BruteForce {
    /// Chunks considered (after any exclusion).
    total: usize,
    rank: Option<usize>,
    score: Option<f32>,
}

/// Brute-force exact-cosine ground truth over `rows`, excluding any chunk
/// whose conversation id is in `exclude`. `rank`/`score` are for the target
/// conversation's best (highest-cosine) chunk.
fn brute_force_rank(
    rows: &[EmbeddingRow],
    query_vec: &[f32],
    target_conv: &str,
    exclude: &HashSet<String>,
) -> BruteForce {
    let mut scored: Vec<(f32, &str)> = rows
        .iter()
        .filter(|row| !exclude.contains(&row.conversation_id))
        .map(|row| (cosine(query_vec, &row.vector), row.conversation_id.as_str()))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let rank = scored.iter().position(|(_, conv)| *conv == target_conv);
    BruteForce {
        total: scored.len(),
        rank,
        score: rank.map(|r| scored[r].0),
    }
}

/// Conversations that could only be ABOUT the target investigation, not a
/// genuine pre-existing corpus hit: their content quotes the target's own
/// uuid, or they first appear after the target conversation's own last
/// timestamp (so they postdate it and can plausibly only be discussing it).
struct Contamination {
    /// `(conversation_id, first_timestamp, matched_substring)`.
    rows: Vec<(String, String, bool)>,
    conv_ids: HashSet<String>,
}

fn detect_contamination(conn: &Connection, target_conv: &str) -> Result<Contamination> {
    let cutoff: String = conn.query_row(
        "SELECT MAX(timestamp) FROM chunks WHERE conversation_id = ?1",
        [target_conv],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT conversation_id, MIN(timestamp) AS first_ts,
                MAX(CASE WHEN content LIKE '%' || ?1 || '%' THEN 1 ELSE 0 END) AS matched
         FROM chunks
         WHERE conversation_id != ?2
         GROUP BY conversation_id
         HAVING matched = 1 OR first_ts > ?3
         ORDER BY first_ts ASC",
    )?;
    let rows: Vec<(String, String, bool)> = stmt
        .query_map((TARGET_UUID_SUBSTRING, target_conv, &cutoff), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? == 1,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    let conv_ids = rows.iter().map(|(id, _, _)| id.clone()).collect();
    Ok(Contamination { rows, conv_ids })
}

/// Sessions produced by this wave-2 investigation itself, which quote its
/// probe queries verbatim (recorded in `docs/`/agent journals: sessions
/// `51a3c211`, `432ec120` and their workflow sidechains — every one first
/// seen after this cutoff). Unlike [`detect_contamination`] (which is
/// target-specific: it also matches on a target's own uuid substring), this
/// is the CHEAP, target-independent half of the same rule — "exclude
/// conversations whose first timestamp is after 2026-09-22T14:00Z" — usable
/// for every sampled conversation in the generalization sweep without a
/// per-conversation contamination scan.
const GLOBAL_CONTAMINATION_CUTOFF: &str = "2026-09-22T14:00:00Z";

/// Conversation ids whose earliest chunk postdates
/// [`GLOBAL_CONTAMINATION_CUTOFF`] — the target-independent half of
/// contamination, computed once and reused as the `exclude` set for every
/// sampled conversation's decontaminated brute-force rank.
fn global_contamination_conv_ids(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id FROM chunks
         GROUP BY conversation_id
         HAVING MIN(timestamp) > ?1",
    )?;
    let ids = stmt
        .query_map([GLOBAL_CONTAMINATION_CUTOFF], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    Ok(ids)
}

/// Mirrors `import::codex_rollout::CODEX_HARNESS_MARKERS` (private to that
/// module): Codex harness-injected boilerplate (plugin recommendations,
/// AGENTS.md instructions, environment-context dumps) that can occupy a
/// conversation's earliest chunk(s) entirely. Discovered while building this
/// probe's sample: for several sampled `codex_rollout` conversations, the
/// literal seq=0 chunk was `<recommended_plugins>\nHere is a list of
/// plugins...` — querying with THAT text is not testing recall at all, it's
/// testing whether the corpus has other harness dumps. The harness-strip fix
/// (`canonical_message`, ba8f18c) removes these markers from messages that
/// MIX boilerplate with real content, but a message that is ENTIRELY
/// boilerplate still becomes its own, real, boilerplate-only chunk — this
/// list exists so query-text SELECTION (not import) skips those chunks.
const CODEX_HARNESS_MARKERS: &[&str] = &[
    "<recommended_plugins>",
    "# AGENTS.md instructions",
    "<environment_context>",
    "<user_instructions>",
    "<permissions instructions>",
    "<codex_internal_context>",
    "<skills_instructions>",
];

fn is_harness_boilerplate(content: &str) -> bool {
    let trimmed = content.trim_start();
    CODEX_HARNESS_MARKERS
        .iter()
        .any(|marker| trimmed.starts_with(marker))
}

/// One row per `conversation_id` for `source`: its project name and its
/// earliest NON-boilerplate chunk's content (seq then rowid order), used as
/// the self-text proxy query when no hand-authored paraphrase is supplied.
/// Falls back to the literal first chunk only when every chunk in the
/// conversation is harness boilerplate (so the sample never silently drops a
/// conversation).
fn first_chunks_for_source(
    conn: &Connection,
    source: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id, project_name, content FROM chunks
         WHERE source = ?1
         ORDER BY conversation_id ASC, seq ASC, rowid ASC",
    )?;
    let rows = stmt.query_map([source], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let all: Vec<(String, String, String)> = rows.collect::<std::result::Result<Vec<_>, _>>()?;

    let mut first_real: HashMap<String, (String, String)> = HashMap::new(); // conv -> (project, content)
    let mut first_any: HashMap<String, (String, String)> = HashMap::new();
    for (conv, project, content) in all {
        first_any
            .entry(conv.clone())
            .or_insert_with(|| (project.clone(), content.clone()));
        if !is_harness_boilerplate(&content) {
            first_real.entry(conv).or_insert((project, content));
        }
    }
    let mut out: Vec<(String, String, String)> = Vec::with_capacity(first_any.len());
    for (conv, (any_project, any_content)) in first_any {
        let (project, content) = first_real
            .remove(&conv)
            .unwrap_or((any_project, any_content));
        out.push((conv, project, content));
    }
    Ok(out)
}

/// A proxy query text built from a conversation's own opening content: strip
/// obvious tool/JSON boilerplate characters, collapse whitespace, take the
/// first ~220 chars. Deliberately NOT a paraphrase.
fn self_text_proxy(content: &str) -> String {
    let cleaned: String = content
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(220).collect()
}

struct SampleConv {
    conversation_id: String,
    project_name: String,
    query_text: String,
    query_source: &'static str,
}

fn build_sample(
    conn: &Connection,
    source: &str,
    n: usize,
    paraphrases: &HashMap<String, String>,
) -> Result<Vec<SampleConv>> {
    let mut rows = first_chunks_for_source(conn, source)?;
    rows.sort_by_key(|a| sample_key(&a.0));
    Ok(rows
        .into_iter()
        .take(n)
        .map(|(conversation_id, project_name, content)| {
            if let Some(paraphrase) = paraphrases.get(&conversation_id) {
                SampleConv {
                    conversation_id,
                    project_name,
                    query_text: paraphrase.clone(),
                    query_source: "paraphrase",
                }
            } else {
                let query_text = self_text_proxy(&content);
                SampleConv {
                    conversation_id,
                    project_name,
                    query_text,
                    query_source: "self-text-proxy",
                }
            }
        })
        .collect())
}

fn fmt_breakdown(b: &ScoreBreakdown, primacy: f32) -> String {
    format!(
        "cos={:.3} user={:+.2} supersedes={:+.2} mechanic={:+.2} scaffold={:+.2} poison={:+.2} primacy={:+.2} total={:.3}",
        b.cosine, b.user_boost, b.supersedes_boost, b.mechanic_penalty, b.scaffold_penalty,
        b.poison_penalty, primacy, b.total + primacy,
    )
}

fn print_stage_table(label: &str, samples: &[StageSample]) {
    println!("  [{label}]");
    for sample in samples {
        let rank = sample
            .target_rank
            .map(|r| (r + 1).to_string())
            .unwrap_or_else(|| "LOST".to_string());
        let score = sample
            .target_score
            .map(|s| format!("{s:.4}"))
            .unwrap_or_else(|| "-".to_string());
        let fts_rank = sample
            .target_fts_rank
            .map(|r| format!(" fts_rank={}", r + 1))
            .unwrap_or_default();
        println!(
            "    {:<16} total={:<5} rank={:<5} score={}{}",
            sample.stage, sample.total_candidates, rank, score, fts_rank
        );
        if !sample.rerank_breakdown.is_empty() {
            for row in &sample.rerank_breakdown {
                let tag = if row.is_target {
                    "TARGET  "
                } else {
                    "displace"
                };
                println!(
                    "        {tag} {:<24} {}",
                    &row.chunk_id[..row.chunk_id.len().min(24)],
                    fmt_breakdown(&row.breakdown, row.primacy_boost)
                );
            }
        }
    }
}

/// The stage at which the target's best rank stopped being on the page.
///
/// Definition (fix for the reviewer-caught bug in the previous version): the
/// FIRST stage, in pipeline order, whose target rank is missing or has
/// reached/passed `limit` — UNLESS the target is present on the actual final
/// page, in which case a later stage recovered it and there is no losing
/// stage at all. `rerank`/`fuse_rrf` reorder candidates without removing any,
/// so treating "present but ranked >= limit" as already off the page (at
/// that stage) is what lets those stages ever get convicted; the prior
/// present->absent-only definition could never attribute a loss to them.
fn losing_stage(samples: &[StageSample], limit: usize) -> String {
    if samples.is_empty() {
        return "no stages recorded (probe scope errored before any checkpoint)".to_string();
    }
    if samples.last().is_some_and(|s| s.target_rank.is_some()) {
        return "none (present on final page)".to_string();
    }
    for sample in samples {
        let lost = match sample.target_rank {
            None => true,
            Some(rank) => rank >= limit,
        };
        if lost {
            return sample.stage.to_string();
        }
    }
    // Every recorded stage had rank < limit yet the target is absent from
    // the final page — should be unreachable (final_page's own vec is
    // already limit-truncated, so a rank < limit there is a hit), but don't
    // silently mislabel if some future stage violates that invariant.
    "final_page".to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut db = PathBuf::from("/tmp/csr-wpa/csr-engine.db");
    let mut projects_dir = PathBuf::from("/Users/ramakrishnanannaswamy/.claude/projects");
    let mut sample_n = 20usize;
    let mut paraphrases_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = PathBuf::from(args.next().expect("--db needs a value")),
            "--projects-dir" => {
                projects_dir = PathBuf::from(args.next().expect("--projects-dir needs a value"))
            }
            "--sample" => {
                sample_n = args
                    .next()
                    .expect("--sample needs a value")
                    .parse()
                    .expect("--sample must be a number")
            }
            "--paraphrases" => {
                paraphrases_path = Some(PathBuf::from(
                    args.next().expect("--paraphrases needs a value"),
                ))
            }
            other => anyhow::bail!("unrecognized argument: {other}"),
        }
    }

    anyhow::ensure!(
        db != Path::new("/Users/ramakrishnanannaswamy/.claude-self-reflect/csr-engine.db"),
        "refusing to run against the live database — pass a /tmp clone via --db"
    );

    let paraphrases: HashMap<String, String> = match &paraphrases_path {
        Some(path) => {
            let raw = std::fs::read_to_string(path)?;
            serde_json::from_str(&raw)?
        }
        None => HashMap::new(),
    };

    println!("=== WP-A codex recall stage probe ===");
    println!("db: {}", db.display());
    println!(
        "paraphrases: {}",
        paraphrases_path
            .as_ref()
            .map(|p| format!("{} ({} entries)", p.display(), paraphrases.len()))
            .unwrap_or_else(|| "none supplied — all rows use self-text-proxy".to_string())
    );

    let engine = Engine::new(&db, &projects_dir)?;
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;

    println!("\nloading full embedding corpus for brute-force ground truth...");
    let all_rows = load_all_embeddings(&conn)?;
    println!("  {} chunk embeddings loaded", all_rows.len());

    // ── Part 1: the single target query, project-scoped and unscoped ──
    println!(
        "\n--- target: {TARGET_CONVERSATION_ID} | query: {TARGET_QUERY:?} | project: {TARGET_PROJECT}/all ---"
    );

    {
        let target_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE conversation_id = ?1",
            [TARGET_CONVERSATION_ID],
            |row| row.get(0),
        )?;
        println!("target chunk count in this db: {target_count}");
    }

    let contamination = detect_contamination(&conn, TARGET_CONVERSATION_ID)?;
    println!(
        "\ncontamination receipt: {} conversation(s) quote the target's uuid or postdate its own last timestamp",
        contamination.rows.len()
    );
    for (conv, first_ts, matched_substring) in &contamination.rows {
        println!("  {conv:<50} first_seen={first_ts} quotes_uuid={matched_substring}");
    }
    println!(
        "note: `reflect_on_past` below still runs over the FULL (contaminated) corpus — \
         excluding these conversations mid-pipeline would mean forking production code. \
         The brute-force ground truth immediately below is measured BOTH ways."
    );

    // Ground truth (stage b): brute-force cosine over EVERY stored chunk
    // embedding, compared against the HNSW-served hnsw_fetch stage below —
    // this is the only stage that does NOT come from the real pipeline
    // (there is no public brute-force entry point on `SearchEngine`), so it
    // is computed independently here rather than instrumented in-process.
    let query_vec = engine.embeddings().embed_single(TARGET_QUERY)?;
    {
        let contaminated = brute_force_rank(
            &all_rows,
            &query_vec,
            TARGET_CONVERSATION_ID,
            &HashSet::new(),
        );
        let decontaminated = brute_force_rank(
            &all_rows,
            &query_vec,
            TARGET_CONVERSATION_ID,
            &contamination.conv_ids,
        );
        for (label, bf) in [
            ("contaminated", &contaminated),
            ("decontaminated", &decontaminated),
        ] {
            println!(
                "  [brute_force_ground_truth:{label}] total={} target_rank={} target_score={}",
                bf.total,
                bf.rank
                    .map(|r| (r + 1).to_string())
                    .unwrap_or_else(|| "LOST".to_string()),
                bf.score
                    .map(|s| format!("{s:.4}"))
                    .unwrap_or_else(|| "-".to_string()),
            );
        }
    }

    for (label, project) in [
        ("project=EnhanceMe", Some(TARGET_PROJECT)),
        ("project=all", None),
    ] {
        let (result, samples) = stage_probe::scope(TARGET_CONVERSATION_ID, || {
            csr_engine::mcp::tools::reflect_on_past(
                engine.storage(),
                engine.embeddings(),
                engine.search(),
                TARGET_QUERY,
                LIMIT,
                0.1,
                project,
            )
        })
        .await;
        match result {
            Ok(_out) => {
                // `in_page` is read directly off the (limit-truncated)
                // final_page stage sample, NOT an output substring — any OTHER
                // result quoting the query text (a conversation ABOUT this
                // investigation) could previously satisfy a substring check
                // even with the target itself absent.
                let in_page = samples
                    .last()
                    .is_some_and(|sample| sample.target_rank.is_some());
                println!("  reflect_on_past({label}): target in final page={in_page}");
            }
            Err(error) => println!("  reflect_on_past({label}): ERROR {error}"),
        }
        print_stage_table(label, &samples);
        println!("  losing stage: {}", losing_stage(&samples, LIMIT));
    }

    // ── Part 2: generalization — >=N codex + >=N Claude conversations ──
    let codex_sample = build_sample(&conn, "codex_rollout", sample_n, &paraphrases)?;
    let claude_sample = build_sample(&conn, "conversation", sample_n, &paraphrases)?;
    let global_contamination = global_contamination_conv_ids(&conn)?;
    println!(
        "\nglobal contamination set (first_seen > {GLOBAL_CONTAMINATION_CUTOFF}): {} conversation(s)",
        global_contamination.len()
    );
    println!(
        "\n--- generalization sample: {} codex, {} claude (seed={SAMPLE_SEED:?}) ---",
        codex_sample.len(),
        claude_sample.len()
    );
    println!(
        "codex ids: {:?}",
        codex_sample
            .iter()
            .map(|c| &c.conversation_id)
            .collect::<Vec<_>>()
    );
    println!(
        "claude ids: {:?}",
        claude_sample
            .iter()
            .map(|c| &c.conversation_id)
            .collect::<Vec<_>>()
    );

    let mut hit_counts: HashMap<&str, (usize, usize)> = HashMap::new(); // source -> (hits, total)
    let mut miss_stage_counts: HashMap<(&str, String), usize> = HashMap::new();
    // Decontaminated hit@6, brute-force ground truth (same methodology and
    // same caveat as the target's own decontaminated number above: measured
    // outside the live pipeline, which has no exclusion parameter to fork
    // production code for). `None` when the sampled conversation is itself
    // in the global contamination set — its own recall can't be scored
    // meaningfully once its content is what "decontaminated" would exclude.
    let mut decontam_hit_counts: HashMap<&str, (usize, usize)> = HashMap::new();

    for (source_label, sample) in [("codex", &codex_sample), ("claude", &claude_sample)] {
        for conv in sample {
            let conv_query_vec = engine.embeddings().embed_single(&conv.query_text).ok();
            let decontam_hit = if global_contamination.contains(&conv.conversation_id) {
                None
            } else {
                conv_query_vec.as_deref().map(|qv| {
                    let bf = brute_force_rank(
                        &all_rows,
                        qv,
                        &conv.conversation_id,
                        &global_contamination,
                    );
                    bf.rank.is_some_and(|r| r < LIMIT)
                })
            };
            match decontam_hit {
                Some(hit) => {
                    let entry = decontam_hit_counts.entry(source_label).or_insert((0, 0));
                    entry.1 += 1;
                    if hit {
                        entry.0 += 1;
                    }
                    println!(
                        "  {source_label:<6} {} decontam_hit@6(brute_force)={hit}",
                        conv.conversation_id
                    );
                }
                None => println!(
                    "  {source_label:<6} {} decontam_hit@6(brute_force)=n/a (self-contaminated)",
                    conv.conversation_id
                ),
            }
            for (scope_label, project) in [
                ("scoped", Some(conv.project_name.as_str())),
                ("unscoped", None),
            ] {
                let (result, samples) = stage_probe::scope(&conv.conversation_id, || {
                    csr_engine::mcp::tools::reflect_on_past(
                        engine.storage(),
                        engine.embeddings(),
                        engine.search(),
                        &conv.query_text,
                        LIMIT,
                        0.1,
                        project,
                    )
                })
                .await;
                if let Err(error) = result {
                    println!(
                        "  {source_label} {} [{scope_label}]: ERROR {error} — treated as unknown, not a miss",
                        conv.conversation_id
                    );
                    continue;
                }
                let final_hit = samples
                    .last()
                    .is_some_and(|sample| sample.target_rank.is_some());
                let entry = hit_counts.entry(source_label).or_insert((0, 0));
                entry.1 += 1;
                let mut stage_label = String::new();
                if final_hit {
                    entry.0 += 1;
                } else {
                    let stage = losing_stage(&samples, LIMIT);
                    // Split an hnsw_fetch loss into ANN-recall-miss (brute
                    // force says it WAS within the fetched window's rank
                    // budget — HNSW's approximation missed it) versus
                    // semantic-miss (brute force agrees nothing that close
                    // existed within budget) using the ground truth loaded
                    // once at startup.
                    let classified = if stage == "hnsw_fetch" {
                        if let (Some(hnsw_sample), Some(qv)) = (
                            samples.iter().find(|s| s.stage == "hnsw_fetch"),
                            conv_query_vec.as_deref(),
                        ) {
                            let bf = brute_force_rank(
                                &all_rows,
                                qv,
                                &conv.conversation_id,
                                &HashSet::new(),
                            );
                            let window = hnsw_sample.total_candidates;
                            match bf.rank {
                                Some(r) if r < window => format!("{stage} (ann_recall_miss)"),
                                Some(_) => format!("{stage} (semantic_miss, outside window)"),
                                None => format!("{stage} (semantic_miss, no embedding match)"),
                            }
                        } else {
                            stage.clone()
                        }
                    } else {
                        stage.clone()
                    };
                    stage_label = classified.clone();
                    *miss_stage_counts
                        .entry((source_label, classified))
                        .or_insert(0) += 1;
                }
                println!(
                    "  {source_label:<6} {} [{scope_label:<8} q={:<16}] hit@6={} losing_stage={}",
                    conv.conversation_id,
                    conv.query_source,
                    final_hit,
                    if final_hit {
                        "-".to_string()
                    } else {
                        stage_label
                    },
                );
            }
        }
    }

    println!("\n--- hit@6 summary ---");
    for source in ["codex", "claude"] {
        if let Some((hits, total)) = hit_counts.get(source) {
            println!(
                "  {source}: {hits}/{total} = {:.1}%",
                100.0 * (*hits as f64) / (*total as f64).max(1.0)
            );
        }
    }
    println!(
        "\n--- decontaminated hit@6 summary (brute-force ground truth, excludes global \
         contamination set) ---"
    );
    for source in ["codex", "claude"] {
        if let Some((hits, total)) = decontam_hit_counts.get(source) {
            println!(
                "  {source}: {hits}/{total} = {:.1}%",
                100.0 * (*hits as f64) / (*total as f64).max(1.0)
            );
        }
    }
    println!("\n--- miss stage histogram ---");
    let mut miss_rows: Vec<_> = miss_stage_counts.into_iter().collect();
    miss_rows.sort_by_key(|a| std::cmp::Reverse(a.1));
    for ((source, stage), count) in miss_rows {
        println!("  {source:<6} {stage:<40} {count}");
    }

    Ok(())
}
