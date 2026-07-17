//! Saga corpus-contamination harness — 3 EXACT retrieval arms × 8 fixed queries.
//!
//! Research binary: measures corpus-contamination effects on retrieval by freezing
//! the reinstatement walk behind channel flags, but with ALL retrieval as brute-force
//! cosine over every chunk vector in memory (no HNSW / ANN). Paths from env only —
//! never the live user DB. Emits one JSONL line per (arm, query) plus a meta header.
//!
//! Critical: never call `engine.search()`, `SearchEngine::search_chunks`, or any
//! other ANN path. Seeds and blend hops use `exact_top_n` over `load_all_chunk_vectors()`.
//! Graph/episode hops already use exact per-conversation cosine (`best_chunk_for_conv`).
//! Reflections are omitted entirely (C0-era reflections are sparse for this experiment).
//!
//! Run (required env):
//!   CSR_E3_DB=... CSR_E3_PROJECTS=... CSR_E3_OUT=... \
//!     cargo run --release --example saga_contamination

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use csr_engine::engine::Engine;
use csr_engine::provenance::ChunkProvenance;
use csr_engine::search::rerank::{rerank_with, RankCandidate, RankPolicy};
use csr_engine::storage::Storage;
use serde_json::json;

// Defaults match `ReinstateConfig::default()` / saga_ablation.rs.
const K: usize = 10;
const SEEDS: usize = 3;
const BLEND_Q: f32 = 0.65;
const GRAPH_BOOST: f32 = 1.10;
const GRAPH_CAP_PER_SEED: usize = 6;
const MIN_SCORE: f32 = 0.20;
const W_QUERY_ECHO: f32 = 0.35;
const QUERY_ECHO_MIN_LEN: usize = 15;
const HOP1_K: usize = K * 2;

/// Channel flags for one contamination arm. One walk implementation; flags gate
/// hop-2 branches and whether echo-aware seed selection / W_QUERY_ECHO demotion apply.
#[derive(Clone, Copy)]
struct ArmFlags {
    use_blend: bool,
    use_graph: bool,
    use_episode: bool,
    use_rerank: bool,
    use_echo: bool,
}

impl ArmFlags {
    fn hop2(&self) -> bool {
        self.use_blend || self.use_graph || self.use_episode
    }
}

const ARMS: &[(&str, ArmFlags)] = &[
    (
        "knn_exact",
        ArmFlags {
            use_blend: false,
            use_graph: false,
            use_episode: false,
            use_rerank: false,
            use_echo: false,
        },
    ),
    (
        "full_exact",
        ArmFlags {
            use_blend: true,
            use_graph: true,
            use_episode: true,
            use_rerank: true,
            use_echo: true,
        },
    ),
    (
        "full_no_echo_exact",
        ArmFlags {
            use_blend: true,
            use_graph: true,
            use_episode: true,
            use_rerank: true,
            use_echo: false,
        },
    ),
];

const QUERIES: &[(&str, &str)] = &[
    (
        "Q5",
        "why does import skip conversations that start with CSR agent prompts",
    ),
    (
        "Q9",
        "why do hooks use catch-all wrappers so they never block claude code",
    ),
    (
        "Q12",
        "why was fts5 keyword fallback added when semantic scores are low",
    ),
    (
        "A3",
        "why does the command center cache campaign data in a snapshot instead of calling the APIs live on page load",
    ),
    (
        "A5",
        "why was score save instrumented with observability across multiple app runtime versions",
    ),
    (
        "A6",
        "why does the whats running section exist on the command center and what does it monitor",
    ),
    (
        "A7",
        "why was the radio reel video built as a remotion composition with a root of multiple scenes",
    ),
    (
        "A8",
        "why does the lessons page pull lesson analytics from posthog instead of supabase",
    ),
];

/// How an evidence item was reached (Display strings match reinstatement.rs).
/// No Reflection — this harness is chunk-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Seed,
    Blend,
    Graph,
    Episode,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Seed => "seed",
            Via::Blend => "blend",
            Via::Graph => "graph",
            Via::Episode => "episode",
        }
    }
}

#[derive(Clone)]
struct Candidate {
    id: String,
    conversation_id: String,
    score: f32,
    via: Via,
}

/// (provenance, timestamp, content) — same shape as reinstatement.rs.
type CandidateDetail = (Option<ChunkProvenance>, String, String);

/// Exact brute-force cosine over `all_vecs`, top-n by score, min_score filter, sorted desc.
fn exact_top_n(
    query_vec: &[f32],
    all_vecs: &[(String, Vec<f32>)],
    n: usize,
    min_score: f32,
) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = all_vecs
        .iter()
        .map(|(id, v)| (id.clone(), cosine(query_vec, v)))
        .filter(|(_, s)| *s >= min_score)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(n);
    scored
}

fn is_query_echo(content: &str, query_lower: &str) -> bool {
    query_lower.len() >= QUERY_ECHO_MIN_LEN && content.to_lowercase().contains(query_lower)
}

/// Prefer non-echo hop-1 hits for seeds; echo hits fill remaining slots only.
fn select_seed_indexes(hit_contents: &[Option<&str>], query_lower: &str, n: usize) -> Vec<usize> {
    let (non_echo, echo): (Vec<usize>, Vec<usize>) = (0..hit_contents.len())
        .partition(|&i| !hit_contents[i].is_some_and(|c| is_query_echo(c, query_lower)));
    non_echo.into_iter().chain(echo).take(n).collect()
}

fn push_candidate(pool: &mut HashMap<String, Candidate>, c: Candidate) {
    pool.entry(c.id.clone())
        .and_modify(|e| {
            if c.score > e.score {
                *e = c.clone();
            }
        })
        .or_insert(c);
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

fn blend(q: &[f32], s: &[f32], query_weight: f32) -> Vec<f32> {
    norm(
        q.iter()
            .zip(s)
            .map(|(a, b)| query_weight * a + (1.0 - query_weight) * b)
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

/// Exact cosine over one conversation's chunk embeddings — never HNSW-filtered.
fn best_chunk_for_conv(
    storage: &Storage,
    query_vec: &[f32],
    conv: &str,
) -> Result<Option<(String, f32)>> {
    let ids = storage.get_chunk_ids_for_conversation(conv)?;
    if ids.is_empty() {
        return Ok(None);
    }
    let vecs = storage.get_chunk_vectors_by_ids(&ids)?;
    let mut best: Option<(String, f32)> = None;
    for (id, v) in vecs {
        let c = cosine(query_vec, &v);
        if best.as_ref().is_none_or(|(_, b)| c > *b) {
            best = Some((id, c));
        }
    }
    Ok(best)
}

fn episode_prev_session(storage: &Storage, conv: &str) -> Result<Option<String>> {
    let tag_b = format!("conv_{conv}");
    let rows = storage.get_reflections_by_two_tags("session_episode", &tag_b, 1)?;
    let Some((_id, content, _tags, _timestamp)) = rows.into_iter().next() else {
        return Ok(None);
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(prev_id) = v.get("prev_episode_id").and_then(|p| p.as_str()) else {
        return Ok(None);
    };
    let Some((_content, tags, _timestamp)) = storage.get_reflection_by_id(prev_id)? else {
        return Ok(None);
    };
    Ok(tags
        .iter()
        .find_map(|t| t.strip_prefix("conv_").map(str::to_string)))
}

/// Provenance rerank adapter. When `apply_echo_penalty` is false, cosine stays raw
/// (full_no_echo_exact); provenance policy still runs via `rerank_with`.
fn rerank_pool(
    mut fused: Vec<Candidate>,
    detail: &HashMap<String, CandidateDetail>,
    query: &str,
    apply_echo_penalty: bool,
) -> Vec<Candidate> {
    let query_lower = query.to_lowercase();
    let echo_check = apply_echo_penalty && query_lower.len() >= QUERY_ECHO_MIN_LEN;
    let rank_cands: Vec<RankCandidate> = fused
        .iter()
        .map(|c| {
            let (provenance, timestamp, content) = match detail.get(&c.id) {
                Some((p, t, s)) => (p.clone(), Some(t.clone()), s.clone()),
                None => (None, None, String::new()),
            };
            let echo = echo_check && content.to_lowercase().contains(&query_lower);
            RankCandidate {
                id: c.id.clone(),
                cosine: if echo {
                    c.score - W_QUERY_ECHO
                } else {
                    c.score
                },
                content,
                provenance,
                timestamp,
            }
        })
        .collect();
    let order: Vec<String> = rerank_with(rank_cands, RankPolicy::Provenance)
        .into_iter()
        .map(|c| c.id)
        .collect();
    let rank_of = |id: &str| order.iter().position(|x| x == id).unwrap_or(usize::MAX);
    fused.sort_by_key(|c| rank_of(&c.id));
    fused
}

/// One-shot exact kNN: top-K cosine over all vectors, no hop-2, no rerank, no echo.
/// Returns candidates plus content map for the `echo` output field.
fn walk_knn_exact(
    storage: &Storage,
    query_vec: &[f32],
    all_vecs: &[(String, Vec<f32>)],
) -> Result<(Vec<Candidate>, HashMap<String, String>)> {
    let hits = exact_top_n(query_vec, all_vecs, K, MIN_SCORE);
    let chunk_ids: Vec<String> = hits.iter().map(|(id, _)| id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &csr_engine::import::ConversationChunk> =
        chunk_meta.iter().map(|c| (c.id.as_str(), c)).collect();

    let mut fused = Vec::new();
    let mut content_by_id: HashMap<String, String> = HashMap::new();
    for (id, score) in &hits {
        if let Some(c) = meta_by_id.get(id.as_str()) {
            content_by_id.insert(c.id.clone(), c.content.clone());
            fused.push(Candidate {
                id: id.clone(),
                conversation_id: c.conversation_id.clone(),
                score: *score,
                via: Via::Seed,
            });
        }
    }
    Ok((fused, content_by_id))
}

/// Flagged reinstatement walk with EXACT cosine scans (full_exact / full_no_echo_exact).
/// No reflections channel. Project scope is always None in this harness.
fn walk_reinstatement_exact(
    storage: &Storage,
    query: &str,
    query_vec: &[f32],
    all_vecs: &[(String, Vec<f32>)],
    flags: ArmFlags,
) -> Result<(Vec<Candidate>, HashMap<String, String>)> {
    // hop-1: 2x over-fetch when hop-2 runs so echo-aware seeds still find non-echoes
    let hop1_k = if flags.hop2() { HOP1_K } else { K };
    let chunk_hits = exact_top_n(query_vec, all_vecs, hop1_k, MIN_SCORE);

    let mut pool: HashMap<String, Candidate> = HashMap::new();

    let chunk_ids: Vec<String> = chunk_hits.iter().map(|(id, _)| id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &csr_engine::import::ConversationChunk> =
        chunk_meta.iter().map(|c| (c.id.as_str(), c)).collect();

    for (id, score) in &chunk_hits {
        if let Some(c) = meta_by_id.get(id.as_str()) {
            push_candidate(
                &mut pool,
                Candidate {
                    id: id.clone(),
                    conversation_id: c.conversation_id.clone(),
                    score: *score,
                    via: Via::Seed,
                },
            );
        }
    }

    if flags.hop2() {
        let query_lower = query.to_lowercase();
        let hit_contents: Vec<Option<&str>> = chunk_hits
            .iter()
            .map(|(id, _)| meta_by_id.get(id.as_str()).map(|c| c.content.as_str()))
            .collect();
        let seed_idxs: Vec<usize> = if flags.use_echo {
            select_seed_indexes(&hit_contents, &query_lower, SEEDS)
        } else {
            (0..chunk_hits.len()).take(SEEDS).collect()
        };
        let seeds: Vec<(String, f32)> = seed_idxs
            .into_iter()
            .map(|i| chunk_hits[i].clone())
            .collect();
        let seed_ids: Vec<String> = seeds.iter().map(|(id, _)| id.clone()).collect();
        let seed_vecs: HashMap<String, Vec<f32>> = storage
            .get_chunk_vectors_by_ids(&seed_ids)?
            .into_iter()
            .collect();

        for (seed_id, _seed_score) in &seeds {
            let Some(seed_conv) = meta_by_id
                .get(seed_id.as_str())
                .map(|c| c.conversation_id.clone())
            else {
                continue;
            };

            // (1) blended context vector re-query (exact scan)
            if flags.use_blend {
                if let Some(sv) = seed_vecs.get(seed_id) {
                    let bv = blend(query_vec, sv, BLEND_Q);
                    let blend_hits = exact_top_n(&bv, all_vecs, 5, MIN_SCORE);
                    let bids: Vec<String> = blend_hits.iter().map(|(id, _)| id.clone()).collect();
                    let bmeta = storage.get_chunks_by_ids(&bids)?;
                    for (id, score) in &blend_hits {
                        if let Some(c) = bmeta.iter().find(|c| c.id == *id) {
                            push_candidate(
                                &mut pool,
                                Candidate {
                                    id: id.clone(),
                                    conversation_id: c.conversation_id.clone(),
                                    score: *score,
                                    via: Via::Blend,
                                },
                            );
                        }
                    }
                }
            }

            // (2) code-graph spread
            if flags.use_graph {
                let mut graph_cands: Vec<Candidate> = Vec::new();
                for file in storage.files_for_session(&seed_conv, 4)? {
                    for neighbor in storage.sessions_for_file(&file, &seed_conv, None, 12)? {
                        if let Some((id, cos)) = best_chunk_for_conv(storage, query_vec, &neighbor)?
                        {
                            graph_cands.push(Candidate {
                                id,
                                conversation_id: neighbor,
                                score: cos * GRAPH_BOOST,
                                via: Via::Graph,
                            });
                        }
                    }
                }
                graph_cands.sort_by(|a, b| b.score.total_cmp(&a.score));
                graph_cands.truncate(GRAPH_CAP_PER_SEED);
                for c in graph_cands {
                    push_candidate(&mut pool, c);
                }
            }

            // (3) episode chain
            if flags.use_episode {
                if let Some(prev_conv) = episode_prev_session(storage, &seed_conv)? {
                    if let Some((id, cos)) = best_chunk_for_conv(storage, query_vec, &prev_conv)? {
                        push_candidate(
                            &mut pool,
                            Candidate {
                                id,
                                conversation_id: prev_conv,
                                score: cos * GRAPH_BOOST,
                                via: Via::Episode,
                            },
                        );
                    }
                }
            }
        }
    }

    let mut fused: Vec<Candidate> = pool.into_values().collect();
    fused.sort_by(|a, b| b.score.total_cmp(&a.score));

    // Whole-pool detail (not top-K only) so rerank can promote lower raw-score origins.
    let chunk_pool_ids: Vec<String> = fused.iter().map(|c| c.id.clone()).collect();
    let mut detail: HashMap<String, CandidateDetail> = HashMap::new();
    for m in storage.get_chunks_by_ids(&chunk_pool_ids)? {
        let prov = storage.get_chunk_provenance(&m.id).ok().flatten();
        detail.insert(m.id, (prov, m.timestamp, m.content));
    }
    fused.retain(|c| detail.contains_key(&c.id));

    if flags.use_rerank {
        fused = rerank_pool(fused, &detail, query, flags.use_echo);
    } else {
        fused.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    fused.truncate(K);

    let content_by_id: HashMap<String, String> = fused
        .iter()
        .filter_map(|c| {
            detail
                .get(&c.id)
                .map(|(_, _, content)| (c.id.clone(), content.clone()))
        })
        .collect();
    Ok((fused, content_by_id))
}

fn run_arm(
    storage: &Storage,
    query: &str,
    query_vec: &[f32],
    all_vecs: &[(String, Vec<f32>)],
    arm_name: &str,
    flags: ArmFlags,
) -> Result<(Vec<Candidate>, HashMap<String, String>)> {
    if arm_name == "knn_exact" {
        walk_knn_exact(storage, query_vec, all_vecs)
    } else {
        walk_reinstatement_exact(storage, query, query_vec, all_vecs, flags)
    }
}

fn distinct_convs_in_order(cands: &[Candidate]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for c in cands {
        if seen.insert(c.conversation_id.clone()) {
            out.push(c.conversation_id.clone());
        }
    }
    out
}

fn result_line(
    arm: &str,
    qid: &str,
    query: &str,
    cands: &[Candidate],
    content_by_id: &HashMap<String, String>,
) -> serde_json::Value {
    let query_lower = query.to_lowercase();
    let convs = distinct_convs_in_order(cands);
    let chunks: Vec<serde_json::Value> = cands
        .iter()
        .map(|c| {
            let echo = content_by_id
                .get(&c.id)
                .map(|s| s.to_lowercase().contains(&query_lower))
                .unwrap_or(false);
            json!({
                "id": c.id,
                "conv": c.conversation_id,
                "score": c.score,
                "via": c.via.as_str(),
                "echo": echo,
            })
        })
        .collect();
    json!({
        "arm": arm,
        "qid": qid,
        "convs": convs,
        "chunks": chunks,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let db_path = PathBuf::from(
        std::env::var("CSR_E3_DB").context("CSR_E3_DB is required (path to condition DB)")?,
    );
    let projects_dir = PathBuf::from(
        std::env::var("CSR_E3_PROJECTS")
            .context("CSR_E3_PROJECTS is required (condition projects dir; may be empty)")?,
    );
    let out_path = PathBuf::from(
        std::env::var("CSR_E3_OUT").context("CSR_E3_OUT is required (JSONL output path)")?,
    );

    eprintln!("loading engine...");
    let engine = Engine::new(&db_path, &projects_dir)?;

    let imported = engine.import_conversations(None).await?;
    eprintln!("imported {imported} new chunks");

    let all_vecs = engine.storage().load_all_chunk_vectors()?;
    eprintln!("loaded {} chunk vectors for exact scan", all_vecs.len());

    let out_file = File::create(&out_path)
        .with_context(|| format!("create output file {}", out_path.display()))?;
    let mut out = BufWriter::new(out_file);

    let meta = json!({
        "meta": {
            "db": db_path.to_string_lossy(),
            "chunks": all_vecs.len(),
            "imported": imported,
        }
    });
    writeln!(out, "{meta}")?;

    let storage = engine.storage();
    let n_queries = QUERIES.len();
    for (qi, (qid, text)) in QUERIES.iter().enumerate() {
        let query_vec = engine.embeddings().embed_single(text)?;
        for (arm_name, flags) in ARMS {
            let (cands, content_by_id) =
                run_arm(storage, text, &query_vec, &all_vecs, arm_name, *flags)?;
            let line = result_line(arm_name, qid, text, &cands, &content_by_id);
            writeln!(out, "{line}")?;
        }
        eprintln!("Q{}/{} done ({})", qi + 1, n_queries, qid);
    }

    out.flush()?;
    eprintln!(
        "wrote {} lines to {}",
        1 + ARMS.len() * n_queries,
        out_path.display()
    );
    Ok(())
}
