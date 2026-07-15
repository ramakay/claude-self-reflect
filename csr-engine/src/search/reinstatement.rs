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

    // ---- hop 1: chunks (project-scoped if given) + reflections, merged ----
    let (chunk_hits, reflection_hits) = {
        let idx = search.read().await;
        let chunks = if let Some(p) = project {
            let ids: std::collections::HashSet<String> =
                storage.get_chunk_ids_for_project(p)?.into_iter().collect();
            idx.search_chunks_filtered(&query_vec, cfg.k, cfg.min_score, &ids)
        } else {
            idx.search_chunks(&query_vec, cfg.k, cfg.min_score)
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

    // seeds = top-N chunk hits (already score-sorted by SearchEngine)
    let seeds: Vec<crate::search::SearchResult> =
        chunk_hits.iter().take(cfg.seeds).cloned().collect();
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

        // (1) blended context vector, second-hop chunk search
        if let Some(sv) = seed_vecs.get(&seed.id) {
            let bv = blend(&query_vec, sv, cfg.blend_query_weight);
            let blend_hits = {
                let idx = search.read().await;
                idx.search_chunks(&bv, 5, cfg.min_score)
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
        let mut graph_cands: Vec<Candidate> = Vec::new();
        for file in storage.files_for_session(&seed_conv, 4)? {
            for neighbor in storage.sessions_for_file(&file, &seed_conv, 12)? {
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
    fused.truncate(cfg.k);

    // Batch-fill timestamp/excerpt for chunk-sourced items in one call; reflection-
    // sourced items (rare, capped by `cfg.seeds`) resolve individually since they
    // live in a different table.
    let chunk_final_ids: Vec<String> = fused
        .iter()
        .filter(|c| c.via != Via::Reflection)
        .map(|c| c.id.clone())
        .collect();
    let chunk_final_meta = storage.get_chunks_by_ids(&chunk_final_ids)?;
    let chunk_final_map: HashMap<&str, &crate::import::ConversationChunk> = chunk_final_meta
        .iter()
        .map(|c| (c.id.as_str(), c))
        .collect();

    let mut items = Vec::with_capacity(fused.len());
    for c in fused {
        let (timestamp, content) = if c.via == Via::Reflection {
            match storage.get_reflection_by_id(&c.id)? {
                Some((content, _tags, timestamp)) => (timestamp, content),
                None => continue,
            }
        } else {
            match chunk_final_map.get(c.id.as_str()) {
                Some(m) => (m.timestamp.clone(), m.content.clone()),
                None => continue,
            }
        };
        items.push(EvidenceItem {
            chunk_id: c.id,
            conversation_id: c.conversation_id,
            score: c.score,
            via: c.via,
            timestamp,
            excerpt: clean_excerpt(&content),
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

    #[test]
    fn via_display_is_lowercase() {
        assert_eq!(Via::Seed.to_string(), "seed");
        assert_eq!(Via::Blend.to_string(), "blend");
        assert_eq!(Via::Graph.to_string(), "graph");
        assert_eq!(Via::Episode.to_string(), "episode");
        assert_eq!(Via::Reflection.to_string(), "reflection");
    }
}
