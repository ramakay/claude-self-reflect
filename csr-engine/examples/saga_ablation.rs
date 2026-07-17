//! Saga reinstatement ablation harness — 7 retrieval arms × 20 provenance queries.
//!
//! Research binary: freezes the walk behind channel flags, runs against a frozen DB
//! clone (paths from env only — never the live user DB), and emits one JSONL line per
//! (arm, query) for external scoring. Mirrors `src/search/reinstatement.rs` via the
//! crate's public API; does not call production `reinstate()` so arm flags can gate
//! hop-2 branches / rerank / echo demotion without touching production code.
//!
//! Run (required env):
//!   CSR_ABLATION_DB=... CSR_ABLATION_PROJECTS=... CSR_ABLATION_OUT=... \
//!     cargo run --release --example saga_ablation

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use csr_engine::engine::Engine;
use csr_engine::provenance::ChunkProvenance;
use csr_engine::search::rerank::{rerank_with, RankCandidate, RankPolicy};
use csr_engine::search::SearchResult;
use csr_engine::storage::Storage;
use serde_json::json;

// Defaults match `ReinstateConfig::default()` in reinstatement.rs.
const K: usize = 10;
const SEEDS: usize = 3;
const BLEND_Q: f32 = 0.65;
const GRAPH_BOOST: f32 = 1.10;
const GRAPH_CAP_PER_SEED: usize = 6;
const MIN_SCORE: f32 = 0.20;
const W_QUERY_ECHO: f32 = 0.35;
const QUERY_ECHO_MIN_LEN: usize = 15;
const HOP1_K: usize = K * 2;

/// Channel flags for one ablation arm. One walk implementation; flags gate hop-2
/// branches and whether echo-aware seed selection / W_QUERY_ECHO demotion apply.
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
        "a_knn",
        ArmFlags {
            use_blend: false,
            use_graph: false,
            use_episode: false,
            use_rerank: false,
            use_echo: false,
        },
    ),
    (
        "b_full",
        ArmFlags {
            use_blend: true,
            use_graph: true,
            use_episode: true,
            use_rerank: true,
            use_echo: true,
        },
    ),
    (
        "c_blend_only",
        ArmFlags {
            use_blend: true,
            use_graph: false,
            use_episode: false,
            use_rerank: true,
            use_echo: true,
        },
    ),
    (
        "d_graph_only",
        ArmFlags {
            use_blend: false,
            use_graph: true,
            use_episode: false,
            use_rerank: true,
            use_echo: true,
        },
    ),
    (
        "e_episode_only",
        ArmFlags {
            use_blend: false,
            use_graph: false,
            use_episode: true,
            use_rerank: true,
            use_echo: true,
        },
    ),
    (
        "f_no_rerank",
        ArmFlags {
            use_blend: true,
            use_graph: true,
            use_episode: true,
            use_rerank: false,
            use_echo: false,
        },
    ),
    (
        "g_no_echo",
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
        "Q1",
        "why is the sqlite connection wrapped in a mutex for thread safety",
    ),
    (
        "Q2",
        "why are tool mechanic scaffold chunks demoted in search ranking",
    ),
    (
        "Q3",
        "why is integrity check cached in the meta table instead of running pragma integrity_check directly",
    ),
    (
        "Q4",
        "why did AI narrative generation switch from a dated model pin to a model fallback chain",
    ),
    (
        "Q5",
        "why does import skip conversations that start with CSR agent prompts",
    ),
    (
        "Q6",
        "why were tool results dropped from import and how was chunking fixed to embed full conversations",
    ),
    (
        "Q7",
        "why does search fall back to exact scan for tiny hnsw indexes",
    ),
    (
        "Q8",
        "why is rmcp pinned to version 1.6 instead of upgrading to 1.7",
    ),
    (
        "Q9",
        "why do hooks use catch-all wrappers so they never block claude code",
    ),
    (
        "Q10",
        "why does session start inject a memory manifest header capability claim",
    ),
    (
        "Q11",
        "why does prompt submit classify intent with semantic exemplars instead of keywords",
    ),
    (
        "Q12",
        "why was fts5 keyword fallback added when semantic scores are low",
    ),
    (
        "A1",
        "why did sign in switch from Clerk Core 3 finalize to legacy setActive in the expo app",
    ),
    (
        "A2",
        "why does the expo app defer sign in with an auth intent service instead of prompting immediately",
    ),
    (
        "A3",
        "why does the command center cache campaign data in a snapshot instead of calling the APIs live on page load",
    ),
    (
        "A4",
        "why do returning user and anonymous user counts differ in the posthog numbers on the command center",
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Seed,
    Blend,
    Graph,
    Episode,
    Reflection,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Seed => "seed",
            Via::Blend => "blend",
            Via::Graph => "graph",
            Via::Episode => "episode",
            Via::Reflection => "reflection",
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
/// (g_no_echo); provenance policy still runs via `rerank_with`.
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

/// One-shot kNN arm: chunks + reflections, max-score merge, score-sort, truncate K.
/// Mirrors saga_spike arm A (no hop-2, no rerank, no echo demotion).
async fn walk_knn(engine: &Engine, query_vec: &[f32]) -> Result<Vec<Candidate>> {
    let (chunk_hits, reflection_hits) = {
        let idx = engine.search().read().await;
        let chunks = idx.search_chunks(query_vec, K, MIN_SCORE);
        let reflections = idx.search_reflections(query_vec, K, MIN_SCORE);
        (chunks, reflections)
    };

    let mut pool: HashMap<String, Candidate> = HashMap::new();
    let storage = engine.storage();

    let chunk_ids: Vec<String> = chunk_hits.iter().map(|r| r.id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &csr_engine::import::ConversationChunk> =
        chunk_meta.iter().map(|c| (c.id.as_str(), c)).collect();

    for r in &chunk_hits {
        if let Some(c) = meta_by_id.get(r.id.as_str()) {
            push_candidate(
                &mut pool,
                Candidate {
                    id: r.id.clone(),
                    conversation_id: c.conversation_id.clone(),
                    score: r.score,
                    via: Via::Seed,
                },
            );
        }
    }
    for r in &reflection_hits {
        if let Ok(Some((_content, tags, _timestamp))) = storage.get_reflection_by_id(&r.id) {
            let conv = tags
                .iter()
                .find_map(|t| t.strip_prefix("conv_").map(str::to_string))
                .unwrap_or_else(|| format!("refl_{}", r.id));
            push_candidate(
                &mut pool,
                Candidate {
                    id: r.id.clone(),
                    conversation_id: conv,
                    score: r.score,
                    via: Via::Reflection,
                },
            );
        }
    }

    let mut fused: Vec<Candidate> = pool.into_values().collect();
    fused.sort_by(|a, b| b.score.total_cmp(&a.score));
    fused.truncate(K);
    Ok(fused)
}

/// Flagged reinstatement walk (b_full mirror + channel ablations).
/// Project scope is always None in this harness.
async fn walk_reinstatement(
    engine: &Engine,
    query: &str,
    query_vec: &[f32],
    flags: ArmFlags,
) -> Result<Vec<Candidate>> {
    let storage = engine.storage();

    // hop-1: 2x over-fetch when hop-2 runs so echo-aware seeds still find non-echoes
    let hop1_k = if flags.hop2() { HOP1_K } else { K };
    let (chunk_hits, reflection_hits) = {
        let idx = engine.search().read().await;
        let chunks = idx.search_chunks(query_vec, hop1_k, MIN_SCORE);
        let reflections = idx.search_reflections(query_vec, K, MIN_SCORE);
        (chunks, reflections)
    };

    let mut pool: HashMap<String, Candidate> = HashMap::new();

    let chunk_ids: Vec<String> = chunk_hits.iter().map(|r| r.id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &csr_engine::import::ConversationChunk> =
        chunk_meta.iter().map(|c| (c.id.as_str(), c)).collect();

    for r in &chunk_hits {
        if let Some(c) = meta_by_id.get(r.id.as_str()) {
            push_candidate(
                &mut pool,
                Candidate {
                    id: r.id.clone(),
                    conversation_id: c.conversation_id.clone(),
                    score: r.score,
                    via: Via::Seed,
                },
            );
        }
    }
    for r in &reflection_hits {
        if let Ok(Some((_content, tags, _timestamp))) = storage.get_reflection_by_id(&r.id) {
            let conv = tags
                .iter()
                .find_map(|t| t.strip_prefix("conv_").map(str::to_string))
                .unwrap_or_else(|| format!("refl_{}", r.id));
            push_candidate(
                &mut pool,
                Candidate {
                    id: r.id.clone(),
                    conversation_id: conv,
                    score: r.score,
                    via: Via::Reflection,
                },
            );
        }
    }

    if flags.hop2() {
        let query_lower = query.to_lowercase();
        let hit_contents: Vec<Option<&str>> = chunk_hits
            .iter()
            .map(|r| meta_by_id.get(r.id.as_str()).map(|c| c.content.as_str()))
            .collect();
        let seed_idxs: Vec<usize> = if flags.use_echo {
            select_seed_indexes(&hit_contents, &query_lower, SEEDS)
        } else {
            (0..chunk_hits.len()).take(SEEDS).collect()
        };
        let seeds: Vec<SearchResult> = seed_idxs
            .into_iter()
            .map(|i| chunk_hits[i].clone())
            .collect();
        let seed_ids: Vec<String> = seeds.iter().map(|s| s.id.clone()).collect();
        let seed_vecs: HashMap<String, Vec<f32>> = storage
            .get_chunk_vectors_by_ids(&seed_ids)?
            .into_iter()
            .collect();

        for seed in &seeds {
            let Some(seed_conv) = meta_by_id
                .get(seed.id.as_str())
                .map(|c| c.conversation_id.clone())
            else {
                continue;
            };

            // (1) blended context vector re-query
            if flags.use_blend {
                if let Some(sv) = seed_vecs.get(&seed.id) {
                    let bv = blend(query_vec, sv, BLEND_Q);
                    let blend_hits = {
                        let idx = engine.search().read().await;
                        idx.search_chunks(&bv, 5, MIN_SCORE)
                    };
                    let bids: Vec<String> = blend_hits.iter().map(|r| r.id.clone()).collect();
                    let bmeta = storage.get_chunks_by_ids(&bids)?;
                    for r in &blend_hits {
                        if let Some(c) = bmeta.iter().find(|c| c.id == r.id) {
                            push_candidate(
                                &mut pool,
                                Candidate {
                                    id: r.id.clone(),
                                    conversation_id: c.conversation_id.clone(),
                                    score: r.score,
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
    let chunk_pool_ids: Vec<String> = fused
        .iter()
        .filter(|c| c.via != Via::Reflection)
        .map(|c| c.id.clone())
        .collect();
    let mut detail: HashMap<String, CandidateDetail> = HashMap::new();
    for m in storage.get_chunks_by_ids(&chunk_pool_ids)? {
        let prov = storage.get_chunk_provenance(&m.id).ok().flatten();
        detail.insert(m.id, (prov, m.timestamp, m.content));
    }
    for c in fused.iter().filter(|c| c.via == Via::Reflection) {
        if let Ok(Some((content, _tags, timestamp))) = storage.get_reflection_by_id(&c.id) {
            detail.insert(c.id.clone(), (None, timestamp, content));
        }
    }
    fused.retain(|c| detail.contains_key(&c.id));

    if flags.use_rerank {
        fused = rerank_pool(fused, &detail, query, flags.use_echo);
    } else {
        fused.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    fused.truncate(K);
    Ok(fused)
}

async fn run_arm(
    engine: &Engine,
    query: &str,
    query_vec: &[f32],
    arm_name: &str,
    flags: ArmFlags,
) -> Result<Vec<Candidate>> {
    if arm_name == "a_knn" {
        walk_knn(engine, query_vec).await
    } else {
        walk_reinstatement(engine, query, query_vec, flags).await
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

fn result_line(arm: &str, qid: &str, cands: &[Candidate]) -> serde_json::Value {
    let convs = distinct_convs_in_order(cands);
    let chunks: Vec<serde_json::Value> = cands
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "conv": c.conversation_id,
                "score": c.score,
                "via": c.via.as_str(),
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
        std::env::var("CSR_ABLATION_DB")
            .context("CSR_ABLATION_DB is required (path to frozen eval DB clone)")?,
    );
    let projects_dir = PathBuf::from(
        std::env::var("CSR_ABLATION_PROJECTS")
            .context("CSR_ABLATION_PROJECTS is required (empty projects dir for Engine::new)")?,
    );
    let out_path = PathBuf::from(
        std::env::var("CSR_ABLATION_OUT")
            .context("CSR_ABLATION_OUT is required (JSONL output path)")?,
    );

    eprintln!("loading engine...");
    let engine = Engine::new(&db_path, &projects_dir)?;
    let chunks_indexed = {
        let idx = engine.search().read().await;
        idx.chunk_count()
    };
    let built_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let out_file = File::create(&out_path)
        .with_context(|| format!("create output file {}", out_path.display()))?;
    let mut out = BufWriter::new(out_file);

    let meta = json!({
        "meta": {
            "db": db_path.to_string_lossy(),
            "chunks_indexed": chunks_indexed,
            "built_at_unix": built_at_unix,
        }
    });
    writeln!(out, "{meta}")?;

    let n_queries = QUERIES.len();
    for (qi, (qid, text)) in QUERIES.iter().enumerate() {
        let query_vec = engine.embeddings().embed_single(text)?;
        for (arm_name, flags) in ARMS {
            let cands = run_arm(&engine, text, &query_vec, arm_name, *flags).await?;
            let line = result_line(arm_name, qid, &cands);
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
