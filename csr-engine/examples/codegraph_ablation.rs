//! H1/H2 codegraph ablation harness — 5 retrieval arms x 20 provenance queries.
//!
//! Research binary (sibling of examples/saga_ablation.rs — that file is NOT modified).
//! All five arms share the FULL common base from saga_ablation's `b_full` arm (blend +
//! episode + rerank + echo defense all ON, identical seeds/quotas/config constants) and
//! vary ONLY the expansion channel used during hop-2:
//!
//!   S       — no expansion (file spread off, AST spread off)
//!   S_F     — file co-edit spread on (existing "graph" channel: code_evolution)
//!   S_A     — AST structural spread on (new: code_nodes/code_edges, resolved only)
//!   S_Asham — AST spread over a degree-preserving shuffle of the same resolved edges
//!   S_FA    — both file + AST spread on
//!
//! H1: does AST structural spread (S_A) beat the no-expansion base (S) AND beat the
//!     shuffled-edge control (S_Asham)? H2: does file co-edit spread (S_F) beat (S)?
//!
//! AST spread reads code_nodes/code_edges via a second, independent read-only
//! rusqlite::Connection opened directly on the frozen DB clone (Storage's connection is
//! crate-private, not reachable from an example binary) — never the live user DB.
//!
//! Run (required env):
//!   CSR_ABLATION_DB=... CSR_ABLATION_PROJECTS=... CSR_ABLATION_OUT=... \
//!     cargo run --release --example codegraph_ablation
//!
//! Query set (optional — external gold/query file):
//!   By default this harness runs the hardcoded 20-query set below (Q1-Q12 + A1-A8),
//!   byte-identical to prior runs. To run a different query set (e.g. eval-kit/ag's
//!   Anukriti gold), pass its path as CLI arg 1 or via CSR_ABLATION_QUERIES:
//!
//!   CSR_ABLATION_DB=... CSR_ABLATION_PROJECTS=... CSR_ABLATION_OUT=... \
//!     cargo run --release --example codegraph_ablation -- /path/to/queries_or_gold.json
//!
//!   Accepted shapes (mirrors eval-kit/e2's queries.json representation):
//!     - a top-level JSON array of {"id": "...", "text": "..."} objects ("query" is
//!       accepted as an alias for "text", so a combined gold file's own query field
//!       name doesn't need renaming), OR
//!     - a JSON object with a top-level "queries" array of the same item shape (so a
//!       combined gold.json with sibling keys like "mapped"/"grades" can be pointed at
//!       directly — this harness only reads the "queries" array out of it; scoring
//!       reads the rest via eval-kit/ag/score.py).
//!   The frozen-snapshot + sha256 verification workflow is unchanged — CSR_ABLATION_DB
//!   still names the frozen read-only clone; only the query set is parameterized here.

use std::collections::{BTreeSet, HashMap, HashSet};
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
use rusqlite::Connection;
use serde_json::json;

// Defaults match `ReinstateConfig::default()` in reinstatement.rs — identical to
// saga_ablation.rs so the common base is a byte-for-byte match of b_full's constants.
const K: usize = 10;
const SEEDS: usize = 3;
const BLEND_Q: f32 = 0.65;
const GRAPH_BOOST: f32 = 1.10;
const GRAPH_CAP_PER_SEED: usize = 6;
const MIN_SCORE: f32 = 0.20;
const W_QUERY_ECHO: f32 = 0.35;
const QUERY_ECHO_MIN_LEN: usize = 15;
const HOP1_K: usize = K * 2;

/// Fixed seed for the degree-preserving Fisher-Yates shuffle used by S_Asham.
const SHAM_SEED: u64 = 0x5EED_C0DE_2026;

/// Expansion-channel flags for one arm. Base hop-2 machinery (blend, episode, rerank,
/// echo defense) is always on — only these three toggle.
#[derive(Clone, Copy)]
struct ArmFlags {
    use_file_graph: bool,
    use_ast: bool,
    use_ast_sham: bool,
}

const ARMS: &[(&str, ArmFlags)] = &[
    (
        "S",
        ArmFlags {
            use_file_graph: false,
            use_ast: false,
            use_ast_sham: false,
        },
    ),
    (
        "S_F",
        ArmFlags {
            use_file_graph: true,
            use_ast: false,
            use_ast_sham: false,
        },
    ),
    (
        "S_A",
        ArmFlags {
            use_file_graph: false,
            use_ast: true,
            use_ast_sham: false,
        },
    ),
    (
        "S_Asham",
        ArmFlags {
            use_file_graph: false,
            use_ast: false,
            use_ast_sham: true,
        },
    ),
    (
        "S_FA",
        ArmFlags {
            use_file_graph: true,
            use_ast: true,
            use_ast_sham: false,
        },
    ),
];

// Identical 20-query set (Q1-Q12 + A1-A8) to saga_ablation.rs / eval-kit/e1/spec.md.
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

/// How an evidence item was reached (Display strings match reinstatement.rs, plus a new
/// `ast` via for the structural-spread channel this harness adds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Seed,
    Blend,
    Graph,
    Ast,
    Episode,
    Reflection,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Seed => "seed",
            Via::Blend => "blend",
            Via::Graph => "graph",
            Via::Ast => "ast",
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

type CandidateDetail = (Option<ChunkProvenance>, String, String);

fn is_query_echo(content: &str, query_lower: &str) -> bool {
    query_lower.len() >= QUERY_ECHO_MIN_LEN && content.to_lowercase().contains(query_lower)
}

fn select_seed_indexes(hit_contents: &[Option<&str>], query_lower: &str, n: usize) -> Vec<usize> {
    let (non_echo, echo): (Vec<usize>, Vec<usize>) = (0..hit_contents.len())
        .partition(|&i| !hit_contents[i].is_some_and(|c| is_query_echo(c, query_lower)));
    non_echo.into_iter().chain(echo).take(n).collect()
}

/// Total order for ranking candidates: score descending, id ascending as a tie-break.
/// Every candidate pool in this harness is built via a `HashMap<String, Candidate>`
/// (dedup by id) or a `HashSet<String>` (dedup by conv), and both have per-process
/// random iteration order — without a deterministic tie-break, candidates with equal
/// scores land in different relative order run-to-run even though the *set* of
/// candidates and their scores is identical. `total_cmp` is used for the score compare
/// (NaN-safe, matches the harness's existing float-ordering convention) and `id` is
/// compared lexicographically as specified — never leave a HashMap/HashSet iteration
/// order to decide output order.
fn cmp_candidates(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id))
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

/// Provenance rerank adapter — identical to saga_ablation.rs's rerank_pool.
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

// ─── AST structural-spread channel (H1) ───

/// Forward/reverse adjacency over one edge set (real or degree-preserving-shuffled).
struct EdgeIndex {
    by_src: HashMap<String, Vec<String>>,
    by_dst: HashMap<String, Vec<String>>,
}

impl EdgeIndex {
    fn build(edges: &[(String, String)]) -> Self {
        let mut by_src: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_dst: HashMap<String, Vec<String>> = HashMap::new();
        for (s, d) in edges {
            by_src.entry(s.clone()).or_default().push(d.clone());
            by_dst.entry(d.clone()).or_default().push(s.clone());
        }
        Self { by_src, by_dst }
    }
}

/// AST node/edge data loaded once from a read-only connection on the frozen snapshot.
/// `convs_to_nodes` maps a conversation/session id (first_conv_id, last_conv_id, or
/// last_session_id — any of the three) to the code_nodes ids touching it.
/// `node_convs` maps a node id to its (first_conv_id, last_conv_id) pair.
struct AstGraph {
    convs_to_nodes: HashMap<String, Vec<String>>,
    node_convs: HashMap<String, (String, String)>,
    real: EdgeIndex,
    sham: EdgeIndex,
}

/// Minimal, deterministic xorshift64 PRNG — no rand crate in this workspace.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn next_below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Degree-preserving shuffle: Fisher-Yates permutation of the dst column only. Preserves
/// each src's out-degree exactly (same edge count per src) and the global in-degree
/// multiset (same dst values overall, just re-attached) — a configuration-model control
/// for whether AST *structure* (vs raw edge density) carries the retrieval signal.
fn shuffle_dst(edges: &[(String, String)], seed: u64) -> Vec<(String, String)> {
    let mut dsts: Vec<String> = edges.iter().map(|(_, d)| d.clone()).collect();
    let mut rng = XorShift64::new(seed);
    for i in (1..dsts.len()).rev() {
        let j = rng.next_below(i + 1);
        dsts.swap(i, j);
    }
    edges
        .iter()
        .zip(dsts)
        .map(|((s, _), d)| (s.clone(), d))
        .collect()
}

fn load_ast_graph(conn: &Connection) -> Result<AstGraph> {
    // code_nodes: build conv/session -> node-ids index and node -> (first,last) conv map.
    let mut convs_to_nodes: HashMap<String, Vec<String>> = HashMap::new();
    let mut node_convs: HashMap<String, (String, String)> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, first_conv_id, last_conv_id, last_session_id FROM code_nodes")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, first_conv, last_conv, last_session) = row?;
            for key in [&first_conv, &last_conv, &last_session] {
                if !key.is_empty() {
                    let bucket = convs_to_nodes.entry(key.clone()).or_default();
                    if !bucket.contains(&id) {
                        bucket.push(id.clone());
                    }
                }
            }
            node_convs.insert(id, (first_conv, last_conv));
        }
    }

    // code_edges: resolved-only, calls/imports, no self-edges.
    let real_edges: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT src_id, dst_id FROM code_edges
             WHERE resolved = 1 AND kind IN ('calls', 'imports') AND src_id <> dst_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let sham_edges = shuffle_dst(&real_edges, SHAM_SEED);

    Ok(AstGraph {
        convs_to_nodes,
        node_convs,
        real: EdgeIndex::build(&real_edges),
        sham: EdgeIndex::build(&sham_edges),
    })
}

/// Neighbor conversation ids reachable in one AST hop from `seed_conv`, over the given
/// edge index (real or sham). Both directions: src-in-seed -> dst node, and
/// dst-in-seed -> src node. Excludes the seed conversation itself.
fn ast_neighbor_convs(graph: &AstGraph, edges: &EdgeIndex, seed_conv: &str) -> Vec<String> {
    let Some(seed_nodes) = graph.convs_to_nodes.get(seed_conv) else {
        return Vec::new();
    };
    // BTreeSet, not HashSet: this collects into a Vec below, and HashSet iteration order
    // is per-process random. cmp_candidates re-sorts every downstream candidate list by
    // (score, id) so this ordering can't leak into final output today, but keeping the
    // dedup step itself deterministic removes the dependency on that downstream re-sort.
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    let mut collect = |neighbor_id: &str| {
        if let Some((fc, lc)) = graph.node_convs.get(neighbor_id) {
            if !fc.is_empty() && fc != seed_conv {
                candidates.insert(fc.clone());
            }
            if !lc.is_empty() && lc != seed_conv {
                candidates.insert(lc.clone());
            }
        }
    };
    for node in seed_nodes {
        if let Some(dsts) = edges.by_src.get(node) {
            for d in dsts {
                collect(d);
            }
        }
        if let Some(srcs) = edges.by_dst.get(node) {
            for s in srcs {
                collect(s);
            }
        }
    }
    candidates.into_iter().collect()
}

/// One-shot kNN base (unused by the 5 arms here — kept for parity/debugging only).
#[allow(dead_code)]
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
    fused.sort_by(cmp_candidates);
    fused.truncate(K);
    Ok(fused)
}

/// Common-base walk (mirrors saga_ablation.rs's b_full exactly) with the expansion
/// channel gated by `flags`: file co-edit spread, AST spread (real or sham).
async fn walk_common_base(
    engine: &Engine,
    ast_graph: &AstGraph,
    query: &str,
    query_vec: &[f32],
    flags: ArmFlags,
) -> Result<(Vec<Candidate>, bool)> {
    let storage = engine.storage();
    // Diagnostic only (H1 seed-coverage stat): true if ast_neighbor_convs found >=1
    // candidate conv for at least one of this query's seeds, BEFORE cap/score/rerank —
    // i.e. whether the AST channel mechanically fires at all, independent of whether its
    // candidates survive truncation to K. Not part of the retrieval score itself.
    let mut ast_fired = false;

    // hop-2 is always on for this harness (blend + episode always on) — over-fetch hop-1.
    let (chunk_hits, reflection_hits) = {
        let idx = engine.search().read().await;
        let chunks = idx.search_chunks(query_vec, HOP1_K, MIN_SCORE);
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

    // Echo-aware seed selection — common base always applies it (matches b_full).
    let query_lower = query.to_lowercase();
    let hit_contents: Vec<Option<&str>> = chunk_hits
        .iter()
        .map(|r| meta_by_id.get(r.id.as_str()).map(|c| c.content.as_str()))
        .collect();
    let seed_idxs = select_seed_indexes(&hit_contents, &query_lower, SEEDS);
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

        // (1) blended context vector re-query — always on (common base).
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

        // (2) file co-edit spread — H2 channel, gated by arm.
        if flags.use_file_graph {
            let mut graph_cands: Vec<Candidate> = Vec::new();
            for file in storage.files_for_session(&seed_conv, 4)? {
                for neighbor in storage.sessions_for_file(&file, &seed_conv, None, 12)? {
                    if let Some((id, cos)) = best_chunk_for_conv(storage, query_vec, &neighbor)? {
                        graph_cands.push(Candidate {
                            id,
                            conversation_id: neighbor,
                            score: cos * GRAPH_BOOST,
                            via: Via::Graph,
                        });
                    }
                }
            }
            graph_cands.sort_by(cmp_candidates);
            graph_cands.truncate(GRAPH_CAP_PER_SEED);
            for c in graph_cands {
                push_candidate(&mut pool, c);
            }
        }

        // (3) AST structural spread — H1 channel, gated by arm (real or sham).
        if flags.use_ast || flags.use_ast_sham {
            let edges = if flags.use_ast_sham {
                &ast_graph.sham
            } else {
                &ast_graph.real
            };
            let mut ast_cands: Vec<Candidate> = Vec::new();
            let neighbor_convs = ast_neighbor_convs(ast_graph, edges, &seed_conv);
            if !neighbor_convs.is_empty() {
                ast_fired = true;
            }
            for cand_conv in neighbor_convs {
                if let Some((id, cos)) = best_chunk_for_conv(storage, query_vec, &cand_conv)? {
                    ast_cands.push(Candidate {
                        id,
                        conversation_id: cand_conv,
                        score: cos * GRAPH_BOOST,
                        via: Via::Ast,
                    });
                }
            }
            ast_cands.sort_by(cmp_candidates);
            ast_cands.truncate(GRAPH_CAP_PER_SEED);
            for c in ast_cands {
                push_candidate(&mut pool, c);
            }
        }

        // (4) episode chain — always on (common base).
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

    let mut fused: Vec<Candidate> = pool.into_values().collect();
    fused.sort_by(cmp_candidates);

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

    // rerank + echo demotion — always on (common base).
    fused = rerank_pool(fused, &detail, query, true);
    fused.truncate(K);
    Ok((fused, ast_fired))
}

/// Resolves the query set: CLI arg 1, else env var `CSR_ABLATION_QUERIES`, else the
/// hardcoded `QUERIES` default (byte-identical output path — nothing reads a file, and
/// the returned tuples are the same id/text pairs in the same order). Returns the loaded
/// pairs plus a human-readable source description for the startup log line only (never
/// written to the output JSONL, so the default path's output bytes are unaffected by this
/// function's existence).
fn load_queries() -> Result<(Vec<(String, String)>, String)> {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("CSR_ABLATION_QUERIES").ok());

    let Some(path) = path else {
        let defaults = QUERIES
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect();
        return Ok((defaults, "hardcoded 20-query default".to_string()));
    };

    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("read queries file {path}"))?;
    let val: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse queries JSON {path}"))?;
    let arr: Vec<serde_json::Value> = match &val {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(o) => o
            .get("queries")
            .and_then(|v| v.as_array())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{path}: expected a top-level array or an object with a \"queries\" array"
                )
            })?,
        _ => anyhow::bail!(
            "{path}: expected a top-level JSON array or an object with a \"queries\" key"
        ),
    };

    let mut out = Vec::with_capacity(arr.len());
    for item in &arr {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("{path}: query item missing string \"id\": {item}"))?
            .to_string();
        let text = item
            .get("text")
            .or_else(|| item.get("query"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("{path}: query item {id} missing string \"text\"/\"query\"")
            })?
            .to_string();
        out.push((id, text));
    }
    anyhow::ensure!(!out.is_empty(), "{path}: no queries found");
    let n = out.len();
    Ok((out, format!("{path} (n={n})")))
}

fn distinct_convs_in_order(cands: &[Candidate]) -> Vec<String> {
    // HashSet is safe here: it's used only as a seen-before membership test, never
    // iterated — the output order comes entirely from `cands`' own (already
    // deterministic, post-cmp_candidates-sort) order below.
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for c in cands {
        if seen.insert(c.conversation_id.clone()) {
            out.push(c.conversation_id.clone());
        }
    }
    out
}

fn result_line(arm: &str, qid: &str, cands: &[Candidate], ast_fired: bool) -> serde_json::Value {
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
        // Diagnostic only (see walk_common_base's ast_fired doc comment) — meaningful
        // only for arms with use_ast/use_ast_sham on; always false for S/S_F.
        "ast_fired": ast_fired,
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

    eprintln!("loading AST graph (code_nodes/code_edges, read-only connection)...");
    let ast_conn = Connection::open(&db_path)
        .with_context(|| format!("open read-only AST connection on {}", db_path.display()))?;
    let ast_graph = load_ast_graph(&ast_conn)?;
    eprintln!(
        "AST graph: {} conv/session keys, {} nodes indexed, {} real resolved edges (calls+imports, no self-edges)",
        ast_graph.convs_to_nodes.len(),
        ast_graph.node_convs.len(),
        ast_graph.real.by_src.values().map(|v| v.len()).sum::<usize>()
    );

    let out_file = File::create(&out_path)
        .with_context(|| format!("create output file {}", out_path.display()))?;
    let mut out = BufWriter::new(out_file);

    let meta = json!({
        "meta": {
            "db": db_path.to_string_lossy(),
            "chunks_indexed": chunks_indexed,
            "built_at_unix": built_at_unix,
            "ast_real_edges": ast_graph.real.by_src.values().map(|v| v.len()).sum::<usize>(),
        }
    });
    writeln!(out, "{meta}")?;

    let (queries, queries_source) = load_queries()?;
    eprintln!("queries: {} loaded from {}", queries.len(), queries_source);
    let n_queries = queries.len();
    for (qi, (qid, text)) in queries.iter().enumerate() {
        let query_vec = engine.embeddings().embed_single(text)?;
        for (arm_name, flags) in ARMS {
            let (cands, ast_fired) =
                walk_common_base(&engine, &ast_graph, text, &query_vec, *flags).await?;
            let line = result_line(arm_name, qid, &cands, ast_fired);
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
