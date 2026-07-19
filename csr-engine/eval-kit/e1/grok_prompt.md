You are writing ONE new Rust file: `csr-engine/examples/saga_ablation.rs` (relative to
repo root `$HOME/projects/claude-self-reflect/csr-engine`). This is
the ONLY file you may create or modify. Do NOT touch src/**, Cargo.toml, or any other file.
Do NOT run `cargo fmt` on the whole tree — just make this one file rustfmt-clean.

## OBJECTIVE
An example research-harness binary that runs 20 provenance queries through 7 retrieval
"arms" (channel-flag variants of the production reinstatement walk) against a frozen DB
clone, and emits one JSONL line per (arm, query) for external scoring. It must NOT change
any production code — it duplicates/mirrors logic locally, exactly like the existing
`examples/saga_spike.rs` does (that file already re-implements a simplified 2-arm version
of the walk against raw SQL; you are writing a more complete 7-arm version that reuses the
crate's PUBLIC API directly instead of raw SQL where possible).

## REFERENCE MATERIAL (read carefully, mirror logic exactly)

### File 1: `csr-engine/src/search/reinstatement.rs` — the production algorithm you are
mirroring. Full contents below. Pay special attention to:
- `select_seed_indexes` (lines ~136-140): echo-aware seed selection.
- hop-2 blend (`blend()` fn + its use in `reinstate()`, the block starting
  `// (1) blended context vector...`).
- hop-2 graph spread (block `// (2) code-graph spread...`).
- hop-2 episode chain (block `// (3) episode chain...`).
- fusion (`push_candidate`, max-score-per-id dedupe) and `rerank_pool` (provenance rerank
  adapter around `rerank_with(..., RankPolicy::Provenance)`, including the W_QUERY_ECHO
  demotion applied to `RankCandidate.cosine` before ranking).
- `best_chunk_for_conv` (exact cosine over a conversation's own chunk vectors — NEVER use
  `search_chunks_filtered` for a single-conversation candidate set, it is not designed for
  tiny allow-lists and is far too slow for that).
- `episode_prev_session`.

```rust
//! Saga reinstatement recall — the proven Phase 0 spike walk, productionized.
//!
//! Two-hop reinstatement: seed retrieval (hop 1) -> blended re-query + code-graph
//! spread + episode-chain hop (hop 2), fused and deduped by max score. See
//! `docs/plans/saga-reinstatement-spike.md` and `examples/saga_spike.rs` for the
//! design and the Phase 0 evidence (+53% provenance coverage vs one-shot kNN).
//!
//! Per-conversation best-chunk selection during graph/episode hops uses EXACT
//! cosine over that conversation's own chunk embeddings
//! (`get_chunk_ids_for_conversation` -> `get_chunk_vectors_by_ids`), never
//! `SearchEngine::search_chunks_filtered` — a tiny per-conversation allowed-id set
//! escalates that method to a near-full-index HNSW search (see
//! `src/search/mod.rs` `search_chunks_filtered`'s adaptive over-fetch), which blows
//! the latency budget. Conversations average ~136 chunks; exact cosine over that is
//! microseconds.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::provenance::ChunkProvenance;
use crate::search::rerank::{rerank_with, RankCandidate, RankPolicy};
use crate::search::SearchEngine;
use crate::storage::Storage;

/// How an evidence item was reached during the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// Hop-1 direct semantic seed hit.
    Seed,
    /// Hop-2 blended query+seed vector re-query.
    Blend,
    /// Hop-2 code-graph spread (seed session -> shared files -> neighbor sessions).
    Graph,
    /// Hop-2 episode chain (seed session's episode -> prev episode's session).
    Episode,
    /// Hop-1 reflection/episode hit competing directly in the fused pool.
    Reflection,
}

impl std::fmt::Display for Via {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Via::Seed => "seed",
            Via::Blend => "blend",
            Via::Graph => "graph",
            Via::Episode => "episode",
            Via::Reflection => "reflection",
        };
        f.write_str(s)
    }
}

/// Tunables for the reinstatement walk. Defaults match the Phase 0 spike gate.
#[derive(Debug, Clone)]
pub struct ReinstateConfig {
    /// Result budget (final fused + truncated list length).
    pub k: usize,
    /// Number of hop-1 seeds that get a hop-2 walk.
    pub seeds: usize,
    /// Query weight in the blended context vector (seed gets `1.0 - this`).
    pub blend_query_weight: f32,
    /// Multiplicative activation bonus for graph/episode-derived candidates.
    pub graph_boost: f32,
    /// Max graph candidates kept per seed (post-sort, pre-fusion).
    pub graph_cap_per_seed: usize,
    /// Minimum similarity score for a candidate to be considered.
    pub min_score: f32,
}

impl Default for ReinstateConfig {
    fn default() -> Self {
        Self {
            k: 10,
            seeds: 3,
            blend_query_weight: 0.65,
            graph_boost: 1.10,
            graph_cap_per_seed: 6,
            min_score: 0.20,
        }
    }
}

/// One piece of cited evidence in a reinstatement recall answer.
#[derive(Debug, Clone)]
pub struct EvidenceItem {
    pub chunk_id: String,
    pub conversation_id: String,
    pub score: f32,
    pub via: Via,
    pub timestamp: String,
    /// ~200 chars, cleaned (newlines -> spaces).
    pub excerpt: String,
}

/// Internal fusion candidate — pre-enrichment (no timestamp/excerpt yet, those are
/// batch-filled once at the end so hop-2 stays cheap).
#[derive(Clone)]
struct Candidate {
    id: String,
    conversation_id: String,
    score: f32,
    via: Via,
}

/// Per-candidate detail for provenance rerank and final item assembly:
/// (provenance, timestamp, content). Reflection-sourced candidates carry
/// provenance=None — rerank's content-based penalties (scaffold/CSR-echo)
/// still apply to them.
type CandidateDetail = (Option<ChunkProvenance>, String, String);

/// Observer-effect defense: a chunk that quotes the query VERBATIM is a session
/// that *asked* the question (or an imported eval/test transcript), not the one
/// that decided the answer — origin conversations predate the question's exact
/// wording. Applied only for queries long enough that a verbatim hit can't be
/// coincidence.
const W_QUERY_ECHO: f32 = 0.35;
const QUERY_ECHO_MIN_LEN: usize = 15;

/// True when `content` quotes the query verbatim (both lowercased) and the
/// query is long enough for that to be a signal rather than coincidence.
fn is_query_echo(content: &str, query_lower: &str) -> bool {
    query_lower.len() >= QUERY_ECHO_MIN_LEN && content.to_lowercase().contains(query_lower)
}

/// Hop-1 seed selection, observer-effect aware: prefer the top `n` hits whose
/// content does NOT quote the query verbatim — an echo seed launches hop-2 from
/// the session that ASKED the question, so blend/graph/episode all spread from
/// the wrong neighborhood and the origin never enters the pool (a re-asked
/// question drowned the walk in its own prior askings). Echo hits fill
/// remaining slots only when non-echo hits run out. Returns indexes into
/// `hits`, best-first.
fn select_seed_indexes(hit_contents: &[Option<&str>], query_lower: &str, n: usize) -> Vec<usize> {
    let (non_echo, echo): (Vec<usize>, Vec<usize>) = (0..hit_contents.len())
        .partition(|&i| !hit_contents[i].is_some_and(|c| is_query_echo(c, query_lower)));
    non_echo.into_iter().chain(echo).take(n).collect()
}

/// Phase 1.5: provenance-aware ordering of the fused pool. `reflect_on_past`
/// has had this layer since v9.3; without it, raw-cosine fusion lets imported
/// transcripts that QUOTE a query verbatim (eval self-contamination, scaffold
/// echoes) outrank the origin conversation. The ordering policy lives in
/// rerank.rs and runs under [`RankPolicy::Provenance`] — mechanic build-log
/// chunks stay UNdemoted here because they are evidence a session shaped the
/// code (demoting them evicted GT session 219ef49f on eval Q5). On top of
/// that, verbatim query echoes are demoted (see [`W_QUERY_ECHO`]). This
/// adapter maps fused candidates onto `RankCandidate` and sorts by the
/// returned order. A candidate missing from `detail` competes on raw score
/// alone (empty content: no boost, no penalty).
fn rerank_pool(
    mut fused: Vec<Candidate>,
    detail: &HashMap<String, CandidateDetail>,
    query: &str,
) -> Vec<Candidate> {
    let query_lower = query.to_lowercase();
    let echo_check = query_lower.len() >= QUERY_ECHO_MIN_LEN;
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

fn push_candidate(pool: &mut HashMap<String, Candidate>, c: Candidate) {
    pool.entry(c.id.clone())
        .and_modify(|e| {
            if c.score > e.score {
                *e = c.clone();
            }
        })
        .or_insert(c);
}

/// L2-normalize a vector in place, returning it.
fn norm(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

/// Weighted blend of query and seed vectors, renormalized.
fn blend(q: &[f32], s: &[f32], query_weight: f32) -> Vec<f32> {
    norm(
        q.iter()
            .zip(s)
            .map(|(a, b)| query_weight * a + (1.0 - query_weight) * b)
            .collect(),
    )
}

/// Plain cosine similarity. 0.0 if either vector is all-zero.
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

/// First ~200 chars of content, newlines/carriage-returns flattened to spaces.
fn clean_excerpt(content: &str) -> String {
    let s: String = content.chars().take(200).collect();
    s.replace(['\n', '\r'], " ")
}

/// Best-scoring chunk in `conv` against `query_vec`, by EXACT cosine over that
/// conversation's own embeddings. `None` if the conversation has no chunks or no
/// stored embeddings (never panics — old/unenriched conversations are common).
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

/// Walk the episode chain from `conv`'s session-episode reflection to the session
/// id of the PREVIOUS episode, if one exists. `None` on any missing link (no
/// episode reflection, malformed JSON, dangling prev_episode_id) — episode chains
/// are sparse by design in this phase, this must never error the whole walk.
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

/// The proven reinstatement walk: seed (hop 1) -> blend + graph spread + episode
/// chain (hop 2), fused by max score per chunk id and truncated to `cfg.k`.
/// Reflections are intentionally global (never project-filtered — parity with
/// `reflect_on_past`); callers pass `project: "all"` normalized to `None` upstream
/// (in `crate::search::cross_project::normalize_project_scope`) as the cross-project
/// escape hatch.
///
/// Async only because `SearchEngine` sits behind a `tokio::RwLock`; each HNSW
/// search takes its own short-lived read guard rather than holding one for the
/// whole call, so storage-heavy hop-2 work never blocks writers.
pub async fn reinstate(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
    cfg: &ReinstateConfig,
) -> Result<Vec<EvidenceItem>> {
    if cfg.k == 0 {
        return Ok(Vec::new());
    }

    let query_vec: Vec<f32> = {
        let q = query.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&q)).await??
    };

    // Project chunk-id set, hoisted so hop-1 seed search and hop-2 blend share one
    // lookup. Reflections stay unfiltered (global).
    let project_chunk_ids: Option<std::collections::HashSet<String>> = match project {
        Some(p) => Some(storage.get_chunk_ids_for_project(p)?.into_iter().collect()),
        None => None,
    };

    // ---- hop 1: chunks (project-scoped if given) + reflections, merged ----
    // 2x over-fetch so seed selection can skip verbatim query echoes and still
    // find cfg.seeds real seeds — on a re-asked question the top hits are all
    // prior askings of it. Final output stays capped at cfg.k after rerank.
    let hop1_k = cfg.k * 2;
    let (chunk_hits, reflection_hits) = {
        let idx = search.read().await;
        let chunks = if let Some(ref ids) = project_chunk_ids {
            idx.search_chunks_filtered(&query_vec, hop1_k, cfg.min_score, ids)
        } else {
            idx.search_chunks(&query_vec, hop1_k, cfg.min_score)
        };
        let reflections = idx.search_reflections(&query_vec, cfg.k, cfg.min_score);
        (chunks, reflections)
    };

    let mut pool: HashMap<String, Candidate> = HashMap::new();

    let chunk_ids: Vec<String> = chunk_hits.iter().map(|r| r.id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &crate::import::ConversationChunk> =
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

    // seeds = top-N NON-ECHO chunk hits (hits are already score-sorted by
    // SearchEngine; echoes only fill in when nothing else matched)
    let query_lower = query.to_lowercase();
    let hit_contents: Vec<Option<&str>> = chunk_hits
        .iter()
        .map(|r| meta_by_id.get(r.id.as_str()).map(|c| c.content.as_str()))
        .collect();
    let seeds: Vec<crate::search::SearchResult> =
        select_seed_indexes(&hit_contents, &query_lower, cfg.seeds)
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

        // (1) blended context vector, second-hop chunk search (project-filtered when scoped)
        if let Some(sv) = seed_vecs.get(&seed.id) {
            let bv = blend(&query_vec, sv, cfg.blend_query_weight);
            let blend_hits = {
                let idx = search.read().await;
                if let Some(ref ids) = project_chunk_ids {
                    idx.search_chunks_filtered(&bv, 5, cfg.min_score, ids)
                } else {
                    idx.search_chunks(&bv, 5, cfg.min_score)
                }
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

        // (2) code-graph spread: seed session -> shared files -> neighbor sessions
        // Neighbor lookup is project-scoped when `project` is Some, so graph spread
        // cannot leak across projects that share a file path.
        let mut graph_cands: Vec<Candidate> = Vec::new();
        for file in storage.files_for_session(&seed_conv, 4)? {
            for neighbor in storage.sessions_for_file(&file, &seed_conv, project, 12)? {
                if let Some((id, cos)) = best_chunk_for_conv(storage, &query_vec, &neighbor)? {
                    graph_cands.push(Candidate {
                        id,
                        conversation_id: neighbor,
                        score: cos * cfg.graph_boost,
                        via: Via::Graph,
                    });
                }
            }
        }
        graph_cands.sort_by(|a, b| b.score.total_cmp(&a.score));
        graph_cands.truncate(cfg.graph_cap_per_seed);
        for c in graph_cands {
            push_candidate(&mut pool, c);
        }

        // (3) episode chain: seed session's episode -> prev episode -> its session
        if let Some(prev_conv) = episode_prev_session(storage, &seed_conv)? {
            if let Some((id, cos)) = best_chunk_for_conv(storage, &query_vec, &prev_conv)? {
                push_candidate(
                    &mut pool,
                    Candidate {
                        id,
                        conversation_id: prev_conv,
                        score: cos * cfg.graph_boost,
                        via: Via::Episode,
                    },
                );
            }
        }
    }

    let mut fused: Vec<Candidate> = pool.into_values().collect();
    fused.sort_by(|a, b| b.score.total_cmp(&a.score));

    // Detail (provenance/timestamp/content) for the WHOLE pool, not just top-k:
    // the pool is bounded by cfg (k + seeds*(5 + graph_cap_per_seed + 1), ~46 at
    // defaults) so one batch chunk fetch stays cheap, and rerank must be able to
    // promote an origin conversation sitting below the raw-score cut. Chunk-
    // sourced detail batches in one call; reflection-sourced items (rare, capped
    // by cfg.seeds) resolve individually since they live in a different table.
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
    // Drop candidates whose backing row vanished (pruned chunk, deleted
    // reflection) BEFORE ranking so they don't occupy ranked slots.
    fused.retain(|c| detail.contains_key(&c.id));

    let mut fused = rerank_pool(fused, &detail, query);
    fused.truncate(cfg.k);

    let mut items = Vec::with_capacity(fused.len());
    for c in fused {
        let Some((_prov, timestamp, content)) = detail.get(&c.id) else {
            continue;
        };
        items.push(EvidenceItem {
            chunk_id: c.id,
            conversation_id: c.conversation_id,
            score: c.score,
            via: c.via,
            timestamp: timestamp.clone(),
            excerpt: clean_excerpt(content),
        });
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_match_spec() {
        let cfg = ReinstateConfig::default();
        assert_eq!(cfg.k, 10);
        assert_eq!(cfg.seeds, 3);
        assert!((cfg.blend_query_weight - 0.65).abs() < f32::EPSILON);
        assert!((cfg.graph_boost - 1.10).abs() < f32::EPSILON);
        assert_eq!(cfg.graph_cap_per_seed, 6);
        assert!((cfg.min_score - 0.20).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_identical_vectors_is_one() {
        let a = vec![1.0_f32, 0.0, 0.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_vector_is_zero_not_nan() {
        let a = vec![0.0_f32, 0.0];
        let b = vec![1.0_f32, 0.0];
        assert_eq!(cosine(&a, &b), 0.0);
    }

    #[test]
    fn blend_weighted_toward_query() {
        let q = norm(vec![1.0_f32, 0.0]);
        let s = norm(vec![0.0_f32, 1.0]);
        let b = blend(&q, &s, 0.9);
        assert!(cosine(&b, &q) > cosine(&b, &s));
    }

    #[test]
    fn blend_weighted_toward_seed_when_low_query_weight() {
        let q = norm(vec![1.0_f32, 0.0]);
        let s = norm(vec![0.0_f32, 1.0]);
        let b = blend(&q, &s, 0.1);
        assert!(cosine(&b, &s) > cosine(&b, &q));
    }

    #[test]
    fn dedup_keeps_max_score() {
        let mut pool: HashMap<String, Candidate> = HashMap::new();
        push_candidate(
            &mut pool,
            Candidate {
                id: "c1".into(),
                conversation_id: "conv1".into(),
                score: 0.5,
                via: Via::Seed,
            },
        );
        push_candidate(
            &mut pool,
            Candidate {
                id: "c1".into(),
                conversation_id: "conv1".into(),
                score: 0.8,
                via: Via::Blend,
            },
        );
        push_candidate(
            &mut pool,
            Candidate {
                id: "c1".into(),
                conversation_id: "conv1".into(),
                score: 0.3,
                via: Via::Graph,
            },
        );
        assert_eq!(pool.len(), 1);
        let c = &pool["c1"];
        assert!((c.score - 0.8).abs() < f32::EPSILON);
        assert_eq!(c.via, Via::Blend);
    }

    #[test]
    fn dedup_keeps_distinct_ids_separate() {
        let mut pool: HashMap<String, Candidate> = HashMap::new();
        push_candidate(
            &mut pool,
            Candidate {
                id: "c1".into(),
                conversation_id: "conv1".into(),
                score: 0.5,
                via: Via::Seed,
            },
        );
        push_candidate(
            &mut pool,
            Candidate {
                id: "c2".into(),
                conversation_id: "conv1".into(),
                score: 0.4,
                via: Via::Graph,
            },
        );
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn excerpt_truncates_and_cleans_newlines() {
        let long = "a".repeat(300) + "\nline2\r\nline3";
        let e = clean_excerpt(&long);
        assert_eq!(e.chars().count(), 200);
        assert!(!e.contains('\n'));
        assert!(!e.contains('\r'));
    }

    fn cand(id: &str, conv: &str, score: f32, via: Via) -> Candidate {
        Candidate {
            id: id.into(),
            conversation_id: conv.into(),
            score,
            via,
        }
    }

    fn detail_entry(
        author: Option<crate::provenance::Speaker>,
        ts: &str,
        content: &str,
    ) -> CandidateDetail {
        (
            author.map(|a| ChunkProvenance {
                author: a,
                source_conv_id: "src".into(),
                supersedes: None,
            }),
            ts.into(),
            content.into(),
        )
    }

    #[test]
    fn rerank_pool_demotes_contaminated_echo_below_origin() {
        // The observer-effect case: an imported transcript quoting the eval
        // query inside a CSR report frame out-cosines the origin conversation.
        // Content-based scaffold demotion must push it below organic prose even
        // though the echo has no enriched provenance yet.
        use crate::provenance::Speaker;
        let mut detail: HashMap<String, CandidateDetail> = HashMap::new();
        detail.insert(
            "echo".into(),
            detail_entry(
                None,
                "2026-07-15T00:00:00Z",
                "━━━ CSR REPORT ━━━ why did we drop Qdrant ━━━ end ━━━",
            ),
        );
        detail.insert(
            "origin".into(),
            detail_entry(
                Some(Speaker::User),
                "2026-05-01T00:00:00Z",
                "decision: drop Qdrant, single Rust binary with local HNSW",
            ),
        );
        let fused = vec![
            cand("echo", "later", 0.95, Via::Seed),
            cand("origin", "founding", 0.60, Via::Graph),
        ];
        let ranked = rerank_pool(fused, &detail, "unrelated query wording");
        assert_eq!(ranked[0].id, "origin");
        // via/score survive reordering untouched.
        assert_eq!(ranked[0].via, Via::Graph);
        assert!((ranked[0].score - 0.60).abs() < f32::EPSILON);
    }

    #[test]
    fn rerank_pool_demotes_verbatim_query_echo() {
        // A session that ASKED the exact question is not the origin that
        // decided it — verbatim query quote gets the echo penalty.
        use crate::provenance::Speaker;
        let query = "why did we drop qdrant from the stack";
        let mut detail: HashMap<String, CandidateDetail> = HashMap::new();
        detail.insert(
            "asker".into(),
            detail_entry(
                Some(Speaker::User),
                "2026-07-15T00:00:00Z",
                "Why did we drop Qdrant from the stack? Let me search past sessions.",
            ),
        );
        detail.insert(
            "origin".into(),
            detail_entry(
                Some(Speaker::User),
                "2026-04-01T00:00:00Z",
                "decision: replace the Python/Docker/Qdrant stack with one Rust binary",
            ),
        );
        let fused = vec![
            cand("asker", "later", 0.95, Via::Seed),
            cand("origin", "founding", 0.70, Via::Blend),
        ];
        let ranked = rerank_pool(fused, &detail, query);
        assert_eq!(ranked[0].id, "origin");
    }

    #[test]
    fn rerank_pool_short_query_no_echo_penalty() {
        // Sub-threshold queries ("qdrant") match everywhere — echo check off.
        let mut detail: HashMap<String, CandidateDetail> = HashMap::new();
        detail.insert(
            "a".into(),
            detail_entry(None, "2026-07-15T00:00:00Z", "qdrant memory issue"),
        );
        detail.insert(
            "b".into(),
            detail_entry(None, "2026-07-14T00:00:00Z", "hnsw rebuild"),
        );
        let fused = vec![
            cand("a", "c1", 0.80, Via::Seed),
            cand("b", "c2", 0.60, Via::Blend),
        ];
        let ranked = rerank_pool(fused, &detail, "qdrant");
        assert_eq!(ranked[0].id, "a");
    }

    #[test]
    fn rerank_pool_missing_detail_competes_on_raw_score() {
        // No detail row: empty content, no provenance — pure cosine ordering
        // must hold (no panic, no accidental demotion).
        let detail: HashMap<String, CandidateDetail> = HashMap::new();
        let fused = vec![
            cand("lo", "c1", 0.40, Via::Blend),
            cand("hi", "c2", 0.80, Via::Seed),
        ];
        let ranked = rerank_pool(fused, &detail, "some long enough query text");
        assert_eq!(ranked[0].id, "hi");
        assert_eq!(ranked[1].id, "lo");
    }

    #[test]
    fn seed_selection_skips_echoes_keeps_score_order() {
        let q = "why did we drop qdrant";
        let contents = [
            Some("why did we drop Qdrant\" [ToolSearch...]"), // echo, best score
            Some("decision: single Rust binary replaces qdrant"),
            Some("Why did we drop Qdrant? searching..."), // echo
            Some("hnsw index build notes"),
            Some("import chunking fix"),
        ];
        let picked = select_seed_indexes(&contents, q, 3);
        assert_eq!(picked, vec![1, 3, 4]);
    }

    #[test]
    fn seed_selection_falls_back_to_echoes_when_starved() {
        let q = "why did we drop qdrant";
        let contents = [
            Some("why did we drop qdrant again"),
            Some("asked: why did we drop qdrant"),
        ];
        // Only echoes exist — still returns seeds rather than an empty walk.
        let picked = select_seed_indexes(&contents, q, 3);
        assert_eq!(picked, vec![0, 1]);
    }

    #[test]
    fn seed_selection_short_query_takes_top_hits() {
        let contents = [Some("qdrant notes"), Some("other")];
        let picked = select_seed_indexes(&contents, "qdrant", 2);
        assert_eq!(picked, vec![0, 1]);
    }

    #[test]
    fn seed_selection_missing_meta_counts_as_non_echo() {
        let q = "why did we drop qdrant";
        let contents = [None, Some("why did we drop qdrant echo")];
        let picked = select_seed_indexes(&contents, q, 1);
        assert_eq!(picked, vec![0]);
    }

    #[test]
    fn via_display_is_lowercase() {
        assert_eq!(Via::Seed.to_string(), "seed");
        assert_eq!(Via::Blend.to_string(), "blend");
        assert_eq!(Via::Graph.to_string(), "graph");
        assert_eq!(Via::Episode.to_string(), "episode");
        assert_eq!(Via::Reflection.to_string(), "reflection");
    }
}
```

### File 2: `csr-engine/examples/saga_spike.rs` — the harness pattern to follow (Engine
construction, imports, main-loop shape, JSON output can be simpler than this file's
println-based reporting — you are switching output to JSONL). Full contents below:

```rust
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
            if best.as_ref().is_none_or(|(_, b)| c > *b) {
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
            let meta = engine
                .storage()
                .get_chunks_by_ids(std::slice::from_ref(&r.id))?;
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
                    let meta = engine
                        .storage()
                        .get_chunks_by_ids(std::slice::from_ref(&r.id))?;
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
                        let meta = engine
                            .storage()
                            .get_chunks_by_ids(std::slice::from_ref(&id))?;
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
                    let meta = engine
                        .storage()
                        .get_chunks_by_ids(std::slice::from_ref(&id))?;
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
```

## VERIFIED PUBLIC API (do not invent signatures — these are exact, taken from the current
source tree)

```rust
// src/engine.rs
impl Engine {
    pub fn new(db_path: &Path, projects_dir: &Path) -> Result<Self>;
    pub fn storage(&self) -> &Arc<Storage>;
    pub fn embeddings(&self) -> &Arc<EmbeddingEngine>;
    pub fn search(&self) -> &Arc<RwLock<SearchEngine>>; // tokio::sync::RwLock
}

// src/embeddings/mod.rs
impl EmbeddingEngine {
    pub fn embed_single(&self, text: &str) -> Result<Vec<f32>>;
}

// src/search/mod.rs
pub struct SearchResult { pub id: String, pub score: f32 } // Clone
impl SearchEngine {
    pub fn search_chunks(&self, query_vec: &[f32], limit: usize, min_score: f32) -> Vec<SearchResult>;
    pub fn search_reflections(&self, query_vec: &[f32], limit: usize, min_score: f32) -> Vec<SearchResult>;
    pub fn search_chunks_filtered(&self, query_vec: &[f32], limit: usize, min_score: f32, allowed_ids: &std::collections::HashSet<String>) -> Vec<SearchResult>;
}

// src/storage/mod.rs (Storage) — all take &self, internally lock a Mutex<Connection>
pub fn get_chunks_by_ids(&self, ids: &[String]) -> Result<Vec<ConversationChunk>>; // ConversationChunk has .id, .conversation_id, .content, .timestamp (all String)
pub fn get_chunk_ids_for_conversation(&self, conversation_id: &str) -> Result<Vec<String>>;
pub fn get_chunk_vectors_by_ids(&self, ids: &[String]) -> Result<Vec<(String, Vec<f32>)>>;
pub fn files_for_session(&self, session_id: &str, limit: usize) -> Result<Vec<String>>;
pub fn sessions_for_file(&self, file_path: &str, exclude_session: &str, project: Option<&str>, limit: usize) -> Result<Vec<String>>;
pub fn get_reflections_by_two_tags(&self, tag_a: &str, tag_b: &str, limit: usize) -> Result<Vec<(String,String,Vec<String>,String)>>; // (id, content, tags, timestamp)
pub fn get_reflection_by_id(&self, id: &str) -> Result<Option<(String, Vec<String>, String)>>; // (content, tags, timestamp)
pub fn get_chunk_provenance(&self, chunk_id: &str) -> Result<Option<ChunkProvenance>>; // exact return type: check the file, it may be Result<Option<ChunkProvenance>> — treat as fallible/optional like reinstatement.rs does with `.ok().flatten()`
pub fn get_chunk_ids_for_project(&self, project: &str) -> Result<Vec<String>>; // not needed here since project scoping is None-only in this task

// src/search/rerank.rs
pub struct RankCandidate { pub id: String, pub cosine: f32, pub content: String, pub provenance: Option<ChunkProvenance>, pub timestamp: Option<String> }
pub enum RankPolicy { Recall, Provenance }
pub fn rerank_with(cands: Vec<RankCandidate>, policy: RankPolicy) -> Vec<RankCandidate>;
```

Crate root is `csr_engine` (see saga_spike.rs's `use csr_engine::engine::Engine;`). You will
also need `use csr_engine::search::rerank::{rerank_with, RankCandidate, RankPolicy};` and
`use csr_engine::provenance::ChunkProvenance;` (for the detail map type, mirroring
reinstatement.rs's `CandidateDetail` type alias) plus whatever storage/search/embeddings
types you need — check `src/lib.rs` or top of reinstatement.rs for the exact module paths
already used successfully (`crate::embeddings::EmbeddingEngine`, `crate::search::SearchEngine`,
`crate::storage::Storage`, `crate::provenance::ChunkProvenance` become
`csr_engine::embeddings::EmbeddingEngine`, `csr_engine::search::SearchEngine`,
`csr_engine::storage::Storage`, `csr_engine::provenance::ChunkProvenance` from the example).

Only use APIs listed above or already used in saga_spike.rs. Do not guess at additional
storage methods.

## CONFIG CONSTANTS (must match production defaults exactly — copy from
`ReinstateConfig::default()` in reinstatement.rs, not from saga_spike.rs's slightly
different local consts)
```
K: usize = 10
SEEDS: usize = 3
BLEND_Q: f32 = 0.65
GRAPH_BOOST: f32 = 1.10
GRAPH_CAP_PER_SEED: usize = 6
MIN_SCORE: f32 = 0.20
W_QUERY_ECHO: f32 = 0.35
QUERY_ECHO_MIN_LEN: usize = 15
hop1_k (hop-1 over-fetch) = 2 * K
```

## ENV / IO CONTRACT
- `CSR_ABLATION_DB` (required, no default): path to the sqlite DB. Error (return
  `anyhow::bail!` or similar, exit non-zero) if unset — use `std::env::var(...)` mapped to
  an error, NOT `.unwrap_or_default()`.
- `CSR_ABLATION_PROJECTS` (required, no default): path to an empty projects dir, passed as
  the second arg to `Engine::new`. Error if unset. This MUST be the value passed to
  `Engine::new` — never fall back to `dirs::home_dir()` like saga_spike.rs does; this
  example must never be able to touch the user's live `~/.claude-self-reflect` DB.
- `CSR_ABLATION_OUT` (required, no default): output file path for JSONL. Error if unset.
  Write with `std::fs::File::create` + `std::io::Write` (or `BufWriter`), one JSON line per
  `writeln!`.

## THE 7 ARMS — one shared walk implementation, driven by a small flags struct
Implement the walk ONCE as a function/struct taking flags
`{ use_blend: bool, use_graph: bool, use_episode: bool, use_rerank: bool, use_echo: bool }`,
then instantiate it 7 times per query with these flag combinations. Do not copy-paste 7
near-duplicate walk functions — one walk, flags gate which hop-2 branches run and whether
rerank/echo apply, mirroring reinstatement.rs's structure as closely as possible while
reading the flags.

1. `a_knn` — hop-1 ONLY: chunk hits (`search_chunks`, limit=K, min_score) + reflection hits
   (`search_reflections`, limit=K, min_score) merged into one pool keyed by max-score
   dedupe (same `push_candidate` semantics as reinstatement.rs), sorted by score
   descending, truncated to K. No hop-2 of any kind. No rerank. No echo-aware seed
   selection (irrelevant since there's no hop-2, but also no echo demotion in ordering).
   This must exactly mirror saga_spike.rs's arm A logic (just JSONL output instead of
   println).
2. `b_full` — ALL flags true. Must be a faithful mirror of `reinstate()` end-to-end:
   - hop-1: `search_chunks` (or `search_chunks_filtered` if project scoped — not used here,
     project is always None) with limit=hop1_k=2*K, min_score=MIN_SCORE, PLUS
     `search_reflections` with limit=K, min_score=MIN_SCORE, both merged into the pool via
     max-score push (reflection candidates compete directly, `via="reflection"`).
   - seed selection: `select_seed_indexes` logic — non-echo hits first, echo hits fill
     remaining slots, from the CHUNK hits only (not reflections), same as
     reinstatement.rs's `hit_contents`/`select_seed_indexes` call, using SEEDS=3.
   - for each of the up-to-3 seeds: (1) blend hop via `blend()` fn + `search_chunks`
     limit=5, min_score=MIN_SCORE, via="blend"; (2) graph spread via `files_for_session`
     (limit 4) -> `sessions_for_file` (project=None, limit 12) -> `best_chunk_for_conv`
     (exact cosine, NEVER `search_chunks_filtered`) -> score * GRAPH_BOOST, sort desc,
     truncate to GRAPH_CAP_PER_SEED, via="graph"; (3) episode chain via
     `episode_prev_session` -> `best_chunk_for_conv` -> score * GRAPH_BOOST, via="episode".
   - whole-pool detail fetch: `get_chunks_by_ids` for all chunk-sourced pool ids (batched
     one call) + `get_chunk_provenance` per id, PLUS `get_reflection_by_id` per
     reflection-sourced pool id — exactly like reinstatement.rs's detail-building block.
     Then `fused.retain(|c| detail.contains_key(&c.id))` (drop candidates whose row
     vanished) BEFORE ranking.
   - rerank: build `RankCandidate` per fused item with the W_QUERY_ECHO demotion applied to
     `.cosine` when content contains the lowercased query verbatim and
     `query.len() >= QUERY_ECHO_MIN_LEN` (exact copy of `rerank_pool`'s logic), call
     `rerank_with(cands, RankPolicy::Provenance)`, reorder `fused` by the returned id order.
   - truncate to K AFTER rerank.
3. `c_blend_only` — use_blend=true, use_graph=false, use_episode=false, use_rerank=true,
   use_echo=true. Seed selection is echo-aware (use_echo=true gates rerank's echo
   demotion AND seed selection echo-awareness together, see g_no_echo for the split
   case). Only the blend hop-2 branch runs (no graph, no episode). Rerank still applies.
4. `d_graph_only` — use_graph=true only (blend off, episode off), rerank+echo ON.
5. `e_episode_only` — use_episode=true only (blend off, graph off), rerank+echo ON.
6. `f_no_rerank` — blend+graph+episode ON, use_rerank=false: skip the rerank step
   entirely, keep the pool in raw max-score-fusion order (sort by score desc only).
   use_echo=false for THIS arm specifically per spec: seed selection also becomes
   plain top-N (no echo-awareness) — i.e. seed selection just takes the first N chunk
   hits in score order, ignoring is_query_echo.
7. `g_no_echo` — blend+graph+episode+rerank ON, use_echo=false: seed selection is plain
   top-N (same as f's seed selection), AND the rerank step must skip the W_QUERY_ECHO
   demotion (build `RankCandidate.cosine` as the raw fused score, no echo penalty) but
   still call `rerank_with(..., RankPolicy::Provenance)` for the rest of the provenance
   ordering.

So concretely: `use_echo` controls TWO things together whenever it's referenced: (a) seed
selection strategy (echo-aware vs plain top-N) and (b) whether the rerank adapter applies
the W_QUERY_ECHO cosine penalty. Every arm above tells you what use_echo is; wire both
behaviors off the same flag.

Project scoping: always `None` (unfiltered `search_chunks`, never `search_chunks_filtered`)
for every arm in this harness.

## QUERIES (fixed order, Q1..Q12 then A1..A8 — 20 total)
Q1..Q12 = copy the `QUERIES` array text field values from saga_spike.rs verbatim, labeled
Q1..Q12 in that order (targets from saga_spike.rs are not needed for this harness's
output — this harness does not compute ground-truth coverage, only emits ranked lists).

A1..A8 (label qid "A1".."A8", in this order; these have no numeric/file target, just plain
query text — targets are not used in this harness at all):
- A1: "why did sign in switch from Clerk Core 3 finalize to legacy setActive in the expo app"
- A2: "why does the expo app defer sign in with an auth intent service instead of prompting immediately"
- A3: "why does the command center cache campaign data in a snapshot instead of calling the APIs live on page load"
- A4: "why do returning user and anonymous user counts differ in the posthog numbers on the command center"
- A5: "why was score save instrumented with observability across multiple app runtime versions"
- A6: "why does the whats running section exist on the command center and what does it monitor"
- A7: "why was the radio reel video built as a remotion composition with a root of multiple scenes"
- A8: "why does the lessons page pull lesson analytics from posthog instead of supabase"

## OUTPUT FORMAT (JSONL via serde_json, one `writeln!` per line)
First line (header, emitted once before any query results):
```json
{"meta":{"db":"<CSR_ABLATION_DB value>","chunks_indexed":<N>,"built_at_unix":<unix seconds>}}
```
`chunks_indexed` = whatever count of indexed chunks you can cheaply obtain from the engine
after construction (e.g. `engine.search().read().await` then a chunk-count accessor if one
exists on `SearchEngine` — check `src/search/mod.rs` for a `chunk_count()` or similar public
method; if none exists, use `storage.count_chunk_embeddings()` which reinstatement.rs's own
neighborhood already relies on for staleness checks — it is public, verified in engine.rs).
`built_at_unix` via `std::time::SystemTime::now().duration_since(UNIX_EPOCH)`.

Then for every (arm, query) pair, one line each, 7 arms x 20 queries = 140 lines total (plus
the 1 meta line = 141 lines):
```json
{"arm":"b_full","qid":"Q1","convs":["<conv1>","<conv2>",...],"chunks":[{"id":"...","conv":"...","score":0.812,"via":"seed"},...]}
```
- `convs`: distinct conversation_ids in final ranked order — first-occurrence dedupe when
  walking the truncated top-K chunk list top to bottom (i.e. iterate the final K items in
  rank order, push each conversation_id to `convs` only if not already present).
- `chunks`: the final (post-truncate) K items in rank order, each with `id` (chunk or
  reflection id), `conv` (conversation_id), `score` (f32, the FUSED/raw score the item
  carried into fusion — reinstatement.rs's `Candidate.score`, i.e. do NOT overwrite it with
  the rerank-adjusted score, rerank only REORDERS, matching reinstatement.rs's `EvidenceItem.score`
  semantics where score/via survive reordering untouched), `via` (string: "seed"|"blend"|
  "graph"|"episode"|"reflection", matching reinstatement.rs's `Via` enum `Display` impl).

Iterate: for each of the 20 queries (outer loop), for each of the 7 arms (inner loop), run
the walk and emit one JSONL line — single Engine instance constructed once before the loop,
single process, one HNSW index build total. Order of arms per query: a_knn, b_full,
c_blend_only, d_graph_only, e_episode_only, f_no_rerank, g_no_echo (matches the order given
above).

Print occasional `eprintln!` progress (e.g. "loading engine...", "Q3/20 done") to stderr —
do not put progress noise in the JSONL output file.

## CONSTRAINTS
- Rust only. No new crate dependencies — only what's already available: `anyhow`,
  `csr_engine` (the lib crate), `serde_json`, `tokio` (features already include "full" per
  workspace Cargo.toml), `dirs` (not actually needed here since projects dir comes from env,
  but available if useful). Do not add anything to Cargo.toml.
- `#[tokio::main]` (or `#[tokio::main(flavor = "current_thread")]` like saga_spike.rs) since
  `engine.search().read().await` is async (tokio RwLock read guard).
- The file itself must be rustfmt-clean (imagine `cargo fmt -- --check` passing on just this
  file) — but do not invoke `cargo fmt` on the whole repo.
- Determinism: single Engine construction, single process, all 7 arms x 20 queries inside
  one nested loop. No wall-clock/timestamps influence retrieval logic — the only timestamp
  used anywhere is the one meta header field for provenance bookkeeping.
- Mirror-fidelity for `b_full` is the acceptance bar: it must produce the same candidate
  flow as `reinstate()` in reinstatement.rs — including reflection candidates competing in
  the hop-1 pool, whole-pool detail fetch (not just top-K) before rerank, dropping
  candidates whose detail row is missing, and truncating AFTER rerank, not before.

## VERIFICATION (I will run this myself after you're done — you do not need to run it, but
your code must compile and behave correctly under it)
```
export CSR_ABLATION_DB=$SCRATCH/e1/eval-clone/csr-engine.db
export CSR_ABLATION_PROJECTS=$SCRATCH/e1/eval-clone/empty-projects
export CSR_ABLATION_OUT=$SCRATCH/e1/ablation.jsonl
cd $HOME/projects/claude-self-reflect/csr-engine
source ~/.cargo/env
cargo build --release --example saga_ablation          # must compile clean, zero warnings preferred
cargo run --release --example saga_ablation             # full run (index build over ~70k chunks + embedding model load, this takes several minutes — that's expected)
wc -l $CSR_ABLATION_OUT                                  # expect 141 (1 meta + 7*20)
python3 -c "import json;[json.loads(l) for l in open('$CSR_ABLATION_OUT')]"  # all lines must parse as JSON
```
Sanity checks that must hold on the output: for at least 15 of the 20 queries, the a_knn
arm's `convs` list differs from the b_full arm's `convs` list (i.e. the channels actually do
something different — if a_knn == b_full for most queries, something is wired wrong); every
JSONL result line has between 1 and 10 entries in `convs`; the b_full result for Q1 contains
more than 1 distinct conversation_id in `convs`.

Please write the complete file now at
`$HOME/projects/claude-self-reflect/csr-engine/examples/saga_ablation.rs`.
Do not create or modify any other file. When done, briefly summarize what you wrote (you do
not need to run cargo build/test yourself — I will verify independently).
