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

use anyhow::Result;
use csr_engine::engine::Engine;
use csr_engine::mcp::stage_probe::{self, StageSample};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The conversation and query WP-A's diagnosis centers on.
const TARGET_CONVERSATION_ID: &str = "codex:01a0c0d5-9783-7fa1-8032-d12e19f919ea";
const TARGET_QUERY: &str = "codex blocked revival 2026 iOS deploy blocker where stuck";
const TARGET_PROJECT: &str = "EnhanceMe";

/// Fixed seed for the deterministic sample selection (blake3 of
/// `"{seed}:{conversation_id}"`, sorted ascending, first N per source).
/// Changing this changes which conversations get sampled — keep it stable
/// across probe runs so before/after tables are directly comparable.
const SAMPLE_SEED: &str = "wpa-recall-sample-v1";

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

/// One row per `conversation_id` for `source`: its project name and its
/// earliest chunk's content (seq then rowid order), used as the self-text
/// proxy query when no hand-authored paraphrase is supplied.
fn first_chunks_for_source(
    conn: &Connection,
    source: &str,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id, project_name, content FROM (
            SELECT conversation_id, project_name, content,
                   ROW_NUMBER() OVER (
                       PARTITION BY conversation_id ORDER BY seq ASC, rowid ASC
                   ) AS rn
            FROM chunks WHERE source = ?1
         ) WHERE rn = 1",
    )?;
    let rows = stmt.query_map([source], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
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
    rows.sort_by(|a, b| sample_key(&a.0).cmp(&sample_key(&b.0)));
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
        println!(
            "    {:<16} total={:<5} rank={:<5} score={}",
            sample.stage, sample.total_candidates, rank, score
        );
    }
}

/// The stage at which the target first goes from present to absent, or
/// "never present" / "present through final_page" as appropriate.
fn losing_stage(samples: &[StageSample]) -> String {
    let mut previous_present = true; // before hnsw_fetch, "present" trivially
    for sample in samples {
        let present = sample.target_rank.is_some();
        if previous_present && !present {
            return sample.stage.to_string();
        }
        previous_present = present;
    }
    if samples.last().is_some_and(|s| s.target_rank.is_some()) {
        "none (survives to final_page)".to_string()
    } else {
        "never recovered by hnsw_fetch".to_string()
    }
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

    // Ground truth (stage b): brute-force cosine over EVERY stored chunk
    // embedding, compared against the HNSW-served hnsw_fetch stage below —
    // this is the only stage that does NOT come from the real pipeline
    // (there is no public brute-force entry point on `SearchEngine`), so it
    // is computed independently here rather than instrumented in-process.
    {
        let query_vec = engine.embeddings().embed_single(TARGET_QUERY)?;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, e.embedding FROM chunks c
             JOIN chunk_embeddings e ON e.chunk_id = c.id",
        )?;
        let rows: Vec<(String, String, Vec<f32>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    blob_to_vec(&row.get::<_, Vec<u8>>(2)?),
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut scored: Vec<(f32, &str, &str)> = rows
            .iter()
            .map(|(id, conv, vec)| (cosine(&query_vec, vec), id.as_str(), conv.as_str()))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let target_rank = scored
            .iter()
            .position(|(_, _, conv)| *conv == TARGET_CONVERSATION_ID);
        println!(
            "  [brute_force_ground_truth] total={} target_rank={} target_score={}",
            scored.len(),
            target_rank
                .map(|r| (r + 1).to_string())
                .unwrap_or_else(|| "LOST".to_string()),
            target_rank
                .map(|r| format!("{:.4}", scored[r].0))
                .unwrap_or_else(|| "-".to_string()),
        );
        println!(
            "  top3 overall: {:?}",
            scored
                .iter()
                .take(3)
                .map(|(s, id, _)| format!("{s:.3}:{}", &id[..8.min(id.len())]))
                .collect::<Vec<_>>()
        );
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
                6,
                0.1,
                project,
            )
        })
        .await;
        match result {
            Ok(out) => {
                let in_page = out.contains(&TARGET_CONVERSATION_ID.replace("codex:", ""))
                    || out.contains(TARGET_CONVERSATION_ID);
                println!("  reflect_on_past({label}): target text present in output={in_page}");
            }
            Err(error) => println!("  reflect_on_past({label}): ERROR {error}"),
        }
        print_stage_table(label, &samples);
        println!("  losing stage: {}", losing_stage(&samples));
    }

    // ── Part 2: generalization — >=N codex + >=N Claude conversations ──
    let codex_sample = build_sample(&conn, "codex_rollout", sample_n, &paraphrases)?;
    let claude_sample = build_sample(&conn, "conversation", sample_n, &paraphrases)?;
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

    for (source_label, sample) in [("codex", &codex_sample), ("claude", &claude_sample)] {
        for conv in sample {
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
                        6,
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
                if final_hit {
                    entry.0 += 1;
                } else {
                    let stage = losing_stage(&samples);
                    *miss_stage_counts.entry((source_label, stage)).or_insert(0) += 1;
                }
                println!(
                    "  {source_label:<6} {} [{scope_label:<8} q={:<16}] hit@6={} losing_stage={}",
                    conv.conversation_id,
                    conv.query_source,
                    final_hit,
                    if final_hit {
                        "-".to_string()
                    } else {
                        losing_stage(&samples)
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
    println!("\n--- miss stage histogram ---");
    let mut miss_rows: Vec<_> = miss_stage_counts.into_iter().collect();
    miss_rows.sort_by(|a, b| b.1.cmp(&a.1));
    for ((source, stage), count) in miss_rows {
        println!("  {source:<6} {stage:<32} {count}");
    }

    Ok(())
}
