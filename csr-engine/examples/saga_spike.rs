//! Saga reinstatement recall spike — Phase 0 evidence, throwaway research code.
//!
//! Hypothesis: two-hop reinstatement recall (seed -> reinstate encoding context via
//! episode chain + code-graph spreading -> second-hop retrieval with blended context
//! vector) surfaces more of a question's true provenance than one-shot kNN at equal
//! result budget.
//!
//! Read-only against the live DB. Run:
//!   cargo run --release --example saga_spike
//!
//! See docs/plans/saga-reinstatement-spike.md for design, metrics, and gates.

use anyhow::Result;
use csr_engine::engine::Engine;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

const K: usize = 10; // equal result budget per arm
const SEEDS: usize = 3; // hop-1 seeds for arm B
const BLEND_Q: f32 = 0.65; // query weight in blended context vector
const MIN_SCORE: f32 = 0.20;
const GRAPH_BOOST: f32 = 1.10; // activation bonus for graph/episode-derived candidates
const GRAPH_CAP_PER_SEED: usize = 6;

struct Q {
    text: &'static str,
    /// Path suffix for ground-truth lookup in code_evolution ("" = judged only).
    target: &'static str,
}

const QUERIES: &[Q] = &[
    Q { text: "why is the sqlite connection wrapped in a mutex for thread safety", target: "src/storage/mod.rs" },
    Q { text: "why are tool mechanic scaffold chunks demoted in search ranking", target: "src/search/rerank.rs" },
    Q { text: "why is integrity check cached in the meta table instead of running pragma integrity_check directly", target: "src/storage/mod.rs" },
    Q { text: "why did AI narrative generation switch from a dated model pin to a model fallback chain", target: "src/narrative/mod.rs" },
    Q { text: "why does import skip conversations that start with CSR agent prompts", target: "src/import/mod.rs" },
    Q { text: "why were tool results dropped from import and how was chunking fixed to embed full conversations", target: "src/import/mod.rs" },
    Q { text: "why does search fall back to exact scan for tiny hnsw indexes", target: "src/search/mod.rs" },
    Q { text: "why is rmcp pinned to version 1.6 instead of upgrading to 1.7", target: "" },
    Q { text: "why do hooks use catch-all wrappers so they never block claude code", target: "src/hooks/mod.rs" },
    Q { text: "why does session start inject a memory manifest header capability claim", target: "src/hooks/session_start.rs" },
    Q { text: "why does prompt submit classify intent with semantic exemplars instead of keywords", target: "src/hooks/intent.rs" },
    Q { text: "why was fts5 keyword fallback added when semantic scores are low", target: "src/mcp/tools.rs" },
];

#[derive(Clone)]
struct Cand {
    id: String,
    conv: String,
    score: f32,
    via: &'static str, // "chunk" | "refl" | "blend" | "graph" | "episode"
    preview: String,
}

fn norm(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

fn blend(q: &[f32], s: &[f32], wq: f32) -> Vec<f32> {
    norm(
        q.iter()
            .zip(s)
            .map(|(a, b)| wq * a + (1.0 - wq) * b)
            .collect(),
    )
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na * nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn clean_preview(content: &str) -> String {
    let s: String = content.chars().take(110).collect();
    s.replace(['\n', '\r'], " ")
}

fn short(conv: &str) -> &str {
    &conv[..conv.len().min(8)]
}

/// Distinct sessions that touched files matching the target suffix (hook-observed edits).
fn ground_truth(raw: &Connection, target: &str) -> Result<HashSet<String>> {
    if target.is_empty() {
        return Ok(HashSet::new());
    }
    let mut stmt = raw
        .prepare("SELECT DISTINCT session_id FROM code_evolution WHERE file_path LIKE '%' || ?1")?;
    let rows = stmt.query_map([target], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn files_for_session(raw: &Connection, session: &str) -> Result<Vec<String>> {
    let mut stmt = raw.prepare(
        "SELECT file_path, COUNT(*) AS n FROM code_evolution WHERE session_id = ?1
         GROUP BY file_path ORDER BY n DESC LIMIT 4",
    )?;
    let rows = stmt.query_map([session], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn sessions_for_file(raw: &Connection, file: &str, exclude: &str) -> Result<Vec<String>> {
    let mut stmt = raw.prepare(
        "SELECT DISTINCT session_id FROM code_evolution
         WHERE file_path = ?1 AND session_id <> ?2 LIMIT 12",
    )?;
    let rows = stmt.query_map([file, exclude], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn chunk_ids_for_conv(raw: &Connection, conv: &str) -> Result<Vec<String>> {
    let mut stmt = raw.prepare("SELECT id FROM chunks WHERE conversation_id = ?1")?;
    let rows = stmt.query_map([conv], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// prev_episode_id -> that episode's session, via the episode reflection chain.
fn episode_prev_session(raw: &Connection, conv: &str) -> Result<Option<String>> {
    let pat = format!("%\"conv_{}\"%", conv);
    let mut stmt = raw.prepare(
        "SELECT content FROM reflections
         WHERE tags LIKE '%\"session_episode\"%' AND tags LIKE ?1
         ORDER BY timestamp DESC LIMIT 1",
    )?;
    let content: Option<String> = stmt.query_row([&pat], |r| r.get(0)).ok();
    let Some(content) = content else {
        return Ok(None);
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(prev_id) = v.get("prev_episode_id").and_then(|p| p.as_str()) else {
        return Ok(None);
    };
    let mut stmt = raw.prepare("SELECT tags FROM reflections WHERE id = ?1")?;
    let tags_json: Option<String> = stmt.query_row([prev_id], |r| r.get(0)).ok();
    let Some(tags_json) = tags_json else {
        return Ok(None);
    };
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    Ok(tags
        .iter()
        .find_map(|t| t.strip_prefix("conv_").map(str::to_string)))
}

fn best_chunk_for_conv(
    raw: &Connection,
    chunk_vecs: &HashMap<String, Vec<f32>>,
    qv: &[f32],
    conv: &str,
) -> Result<Option<(String, f32)>> {
    let ids = chunk_ids_for_conv(raw, conv)?;
    let mut best: Option<(String, f32)> = None;
    for id in ids {
        if let Some(v) = chunk_vecs.get(&id) {
            let c = cosine(qv, v);
            if best.as_ref().map_or(true, |(_, b)| c > *b) {
                best = Some((id, c));
            }
        }
    }
    Ok(best)
}

fn coverage(cands: &[Cand], gt: &HashSet<String>) -> usize {
    cands
        .iter()
        .map(|c| c.conv.as_str())
        .collect::<HashSet<_>>()
        .iter()
        .filter(|c| gt.contains(**c))
        .count()
}

fn diversity(cands: &[Cand]) -> usize {
    cands
        .iter()
        .map(|c| c.conv.as_str())
        .collect::<HashSet<_>>()
        .len()
}

fn push_cand(pool: &mut HashMap<String, Cand>, c: Cand) {
    pool.entry(c.id.clone())
        .and_modify(|e| {
            if c.score > e.score {
                *e = c.clone();
            }
        })
        .or_insert(c);
}

fn finalize(pool: HashMap<String, Cand>) -> Vec<Cand> {
    let mut v: Vec<Cand> = pool.into_values().collect();
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    v.truncate(K);
    v
}

fn print_arm(label: &str, cands: &[Cand], gt: &HashSet<String>) {
    println!("  {label}:");
    for c in cands {
        let hit = if gt.contains(&c.conv) { "*" } else { " " };
        println!(
            "   {hit}[{:<7}] {:.3} conv={} {}",
            c.via,
            c.score,
            short(&c.conv),
            c.preview
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let home = dirs::home_dir().expect("home dir");
    let db_path = home.join(".claude-self-reflect/csr-engine.db");
    let engine = Engine::new(&db_path, &home.join(".claude/projects"))?;
    let raw = Connection::open(&db_path)?;

    eprintln!("loading chunk vectors...");
    let chunk_vecs: HashMap<String, Vec<f32>> = engine
        .storage()
        .load_all_chunk_vectors()?
        .into_iter()
        .collect();
    eprintln!("{} chunk vectors loaded", chunk_vecs.len());

    let mut totals = (0usize, 0usize, 0usize, 0usize); // gt_a, gt_b, div_a, div_b
    let mut gt_possible = 0usize;

    for (qi, q) in QUERIES.iter().enumerate() {
        let gt = ground_truth(&raw, q.target)?;
        let qv = engine.embeddings().embed_single(q.text)?;
        let idx = engine.search().read().await;

        // ---- Arm A: one-shot kNN, chunks + reflections merged ----
        let mut a: Vec<Cand> = Vec::new();
        for r in idx.search_chunks(&qv, K, MIN_SCORE) {
            let meta = engine.storage().get_chunks_by_ids(&[r.id.clone()])?;
            if let Some(ch) = meta.first() {
                a.push(Cand {
                    id: r.id,
                    conv: ch.conversation_id.clone(),
                    score: r.score,
                    via: "chunk",
                    preview: clean_preview(&ch.content),
                });
            }
        }
        for r in idx.search_reflections(&qv, K, MIN_SCORE) {
            if let Some((content, tags, _ts)) = engine.storage().get_reflection_by_id(&r.id)? {
                let conv = tags
                    .iter()
                    .find_map(|t| t.strip_prefix("conv_").map(str::to_string))
                    .unwrap_or_else(|| format!("refl_{}", r.id));
                a.push(Cand {
                    id: r.id,
                    conv,
                    score: r.score,
                    via: "refl",
                    preview: clean_preview(&content),
                });
            }
        }
        a.sort_by(|x, y| y.score.total_cmp(&x.score));
        a.truncate(K);

        // ---- Arm B: reinstatement (hop1 seeds + blend + graph spread + episode chain) ----
        let mut pool: HashMap<String, Cand> = HashMap::new();
        let seeds: Vec<Cand> = a
            .iter()
            .filter(|c| c.via == "chunk")
            .take(SEEDS)
            .cloned()
            .collect();
        for s in &seeds {
            push_cand(&mut pool, s.clone());
        }
        // reflections compete in B too (same information both arms start from)
        for c in a.iter().filter(|c| c.via == "refl") {
            push_cand(&mut pool, c.clone());
        }

        for seed in &seeds {
            // (1) blended context vector, second hop
            if let Some(sv) = chunk_vecs.get(&seed.id) {
                let bv = blend(&qv, sv, BLEND_Q);
                for r in idx.search_chunks(&bv, 5, MIN_SCORE) {
                    let meta = engine.storage().get_chunks_by_ids(&[r.id.clone()])?;
                    if let Some(ch) = meta.first() {
                        push_cand(
                            &mut pool,
                            Cand {
                                id: r.id,
                                conv: ch.conversation_id.clone(),
                                score: r.score,
                                via: "blend",
                                preview: clean_preview(&ch.content),
                            },
                        );
                    }
                }
            }

            // (2) code-graph spread: seed session -> files -> other sessions
            let mut graph_cands: Vec<Cand> = Vec::new();
            for file in files_for_session(&raw, &seed.conv)? {
                for neighbor in sessions_for_file(&raw, &file, &seed.conv)? {
                    if let Some((id, cos)) = best_chunk_for_conv(&raw, &chunk_vecs, &qv, &neighbor)?
                    {
                        let meta = engine.storage().get_chunks_by_ids(&[id.clone()])?;
                        if let Some(ch) = meta.first() {
                            graph_cands.push(Cand {
                                id,
                                conv: neighbor.clone(),
                                score: cos * GRAPH_BOOST,
                                via: "graph",
                                preview: clean_preview(&ch.content),
                            });
                        }
                    }
                }
            }
            graph_cands.sort_by(|x, y| y.score.total_cmp(&x.score));
            graph_cands.truncate(GRAPH_CAP_PER_SEED);
            for c in graph_cands {
                push_cand(&mut pool, c);
            }

            // (3) episode chain: seed session's episode -> prev episode -> its session
            if let Some(prev_conv) = episode_prev_session(&raw, &seed.conv)? {
                if let Some((id, cos)) = best_chunk_for_conv(&raw, &chunk_vecs, &qv, &prev_conv)? {
                    let meta = engine.storage().get_chunks_by_ids(&[id.clone()])?;
                    if let Some(ch) = meta.first() {
                        push_cand(
                            &mut pool,
                            Cand {
                                id,
                                conv: prev_conv,
                                score: cos * GRAPH_BOOST,
                                via: "episode",
                                preview: clean_preview(&ch.content),
                            },
                        );
                    }
                }
            }
        }
        let b = finalize(pool);

        // ---- metrics ----
        let (ga, gb) = (coverage(&a, &gt), coverage(&b, &gt));
        let (da, db) = (diversity(&a), diversity(&b));
        totals.0 += ga;
        totals.1 += gb;
        totals.2 += da;
        totals.3 += db;
        if !gt.is_empty() {
            gt_possible += gt.len();
        }

        println!("\n=== Q{} [{}] {}", qi + 1, q.target, q.text);
        println!(
            "  GT sessions: {} | A coverage {} diversity {} | B coverage {} diversity {}",
            gt.len(),
            ga,
            da,
            gb,
            db
        );
        print_arm("A (kNN)", &a, &gt);
        print_arm("B (reinstatement)", &b, &gt);
    }

    println!("\n================ SUMMARY ================");
    println!(
        "queries: {} | total GT sessions reachable: {}",
        QUERIES.len(),
        gt_possible
    );
    println!(
        "GT coverage    A={} B={} (gate: B >= A + 25%)",
        totals.0, totals.1
    );
    println!("conv diversity A={} B={}", totals.2, totals.3);
    Ok(())
}
