//! Saga reinstatement recall — the proven Phase 0 spike walk, productionized.
//!
//! Query-aware reinstatement: exact-symbol + semantic seeds (hop 1) -> blended
//! re-query + code-graph spread + episode-chain hop (hop 2), fused by max score
//! while retaining every route that reached each candidate. See
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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::provenance::ChunkProvenance;
use crate::search::rerank::{rerank_with, RankCandidate, RankPolicy};
use crate::search::SearchEngine;
use crate::storage::Storage;

/// How an evidence item was reached during the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Via {
    /// Hop-1 direct semantic seed hit.
    Seed,
    /// The single permitted semantic seed in the 0.20..0.30 fallback band.
    SeedLowConfidence,
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
            Via::SeedLowConfidence => "seed-low-confidence",
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
    /// Minimum similarity for blend/reflection candidates. Semantic seed
    /// policy uses the fixed 0.30 primary and 0.20 fallback floors.
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
    /// Route whose score won fusion for ordering.
    pub best_route: Via,
    /// Every route that reached this candidate, retained across deduplication.
    pub routes: BTreeSet<Via>,
    /// Best score observed independently for each route.
    pub route_scores: BTreeMap<Via, f32>,
    pub timestamp: String,
    /// ~200 chars, cleaned (newlines -> spaces).
    pub excerpt: String,
    /// Shadow signal (Phase: ratification divergence collection). Populated
    /// AFTER final ranking/truncation from a batched lookup; never read before
    /// ordering is fixed and never fed into any score/sort. `None` when the
    /// conversation has no ratification row yet (silent, not an error).
    pub ratification: Option<f32>,
}

/// Auditable stage counters for one reinstatement walk. Surfaced counts are
/// intentionally recomputed from final [`EvidenceItem::routes`] by the
/// renderer rather than stored here, preventing receipt drift.
#[derive(Debug, Clone, Default)]
pub struct ReinstateTrace {
    pub scope_projects: Vec<String>,
    pub seeds_selected: usize,
    pub seed_conversations: usize,
    pub symbols_matched: usize,
    pub symbol_names: Vec<String>,
    pub graph_walks: usize,
    pub graph_accepted: usize,
    pub structural_graph_accepted: usize,
    pub episode_links: usize,
    pub episode_resolved: usize,
    pub episode_accepted: usize,
    pub episode_below_cut: usize,
    pub episode_below_threshold: usize,
    pub episode_dangling: usize,
    pub episode_out_of_scope: usize,
    pub episode_unembedded: usize,
}

/// Evidence plus the walk receipt used to render honest stage accounting.
#[derive(Debug, Clone, Default)]
pub struct ReinstateResult {
    pub items: Vec<EvidenceItem>,
    pub trace: ReinstateTrace,
}

impl IntoIterator for ReinstateResult {
    type Item = EvidenceItem;
    type IntoIter = std::vec::IntoIter<EvidenceItem>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a ReinstateResult {
    type Item = &'a EvidenceItem;
    type IntoIter = std::slice::Iter<'a, EvidenceItem>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

fn surfaced_counts(items: &[EvidenceItem]) -> (usize, usize, usize) {
    let seeds = items
        .iter()
        .filter(|item| {
            item.routes.contains(&Via::Seed) || item.routes.contains(&Via::SeedLowConfidence)
        })
        .count();
    let graph = items
        .iter()
        .filter(|item| item.routes.contains(&Via::Graph))
        .count();
    let episodes = items
        .iter()
        .filter(|item| item.routes.contains(&Via::Episode))
        .count();
    (seeds, graph, episodes)
}

/// Render the canonical receipt shared by csr_why and the provenance gate.
pub fn render_receipt(trace: &ReinstateTrace, items: &[EvidenceItem]) -> String {
    let (seed_count, graph_count, episode_count) = surfaced_counts(items);
    format!(
        "receipt: scope={} | seeds selected={} conversations={} surfaced={} | symbols matched={} [{}] | graph walks={} accepted={} surfaced={} | episodes links={} resolved={} accepted={} surfaced={} (below-cut={}, below-threshold={}, dangling={}, out-of-scope={}, unembedded={})",
        trace.scope_projects.join(","),
        trace.seeds_selected,
        trace.seed_conversations,
        seed_count,
        trace.symbols_matched,
        trace.symbol_names.join(","),
        trace.graph_walks,
        trace.graph_accepted,
        graph_count,
        trace.episode_links,
        trace.episode_resolved,
        trace.episode_accepted,
        episode_count,
        trace.episode_below_cut,
        trace.episode_below_threshold,
        trace.episode_dangling,
        trace.episode_out_of_scope,
        trace.episode_unembedded,
    )
}

/// Verify that a rendered receipt's surfaced counters equal the route sets on
/// the final evidence. This catches renderer/gate drift rather than merely
/// checking internal counters.
pub fn rendered_receipt_surface_counts_match(
    receipt: &str,
    trace: &ReinstateTrace,
    items: &[EvidenceItem],
) -> bool {
    let (seeds, graph, episodes) = surfaced_counts(items);
    receipt.contains(&format!(
        "seeds selected={} conversations={} surfaced={seeds}",
        trace.seeds_selected, trace.seed_conversations
    )) && receipt.contains(&format!(
        "graph walks={} accepted={} surfaced={graph}",
        trace.graph_walks, trace.graph_accepted
    )) && receipt.contains(&format!(
        "episodes links={} resolved={} accepted={} surfaced={episodes}",
        trace.episode_links, trace.episode_resolved, trace.episode_accepted
    ))
}

/// Internal fusion candidate — pre-enrichment (no timestamp/excerpt yet, those are
/// batch-filled once at the end so hop-2 stays cheap).
#[derive(Clone)]
struct Candidate {
    id: String,
    conversation_id: String,
    score: f32,
    best_route: Via,
    route_scores: BTreeMap<Via, f32>,
}

impl Candidate {
    fn new(id: String, conversation_id: String, score: f32, via: Via) -> Self {
        Self {
            id,
            conversation_id,
            score,
            best_route: via,
            route_scores: BTreeMap::from([(via, score)]),
        }
    }

    fn routes(&self) -> BTreeSet<Via> {
        self.route_scores.keys().copied().collect()
    }

    fn is_reflection_only(&self) -> bool {
        self.route_scores.len() == 1 && self.route_scores.contains_key(&Via::Reflection)
    }
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
const MAX_IDENTIFIER_CANDIDATES: usize = 16;
const MAX_SYMBOL_NODES: usize = 32;
const MAX_STRUCTURAL_CONVERSATIONS: usize = 64;

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

fn truncate_with_episode_quota(
    ranked: Vec<Candidate>,
    k: usize,
    min_episode_score: f32,
) -> Vec<Candidate> {
    if ranked.len() <= k {
        return ranked;
    }
    let mut surfaced: Vec<Candidate> = ranked.iter().take(k).cloned().collect();
    // Route-diversity quota: the tool's purpose is provenance ROUTES, and a
    // pure-similarity top-k lets the hop-1 blend pool drown every walked
    // route (measured live: Q1 graph walks=13 accepted=3 surfaced=0, eval
    // --provenance 2026-08-09). Reserve the last slots for the best accepted
    // episode and graph candidates when neither survived on score alone —
    // quota only re-seats candidates that genuinely cleared their acceptance
    // floors; it never fabricates or re-scores.
    for (route, slot_from_end) in [(Via::Episode, 1), (Via::Graph, 2)] {
        if surfaced
            .iter()
            .any(|candidate| candidate.route_scores.contains_key(&route))
        {
            continue;
        }
        let replacement = ranked.iter().find(|candidate| {
            candidate
                .route_scores
                .get(&route)
                .is_some_and(|score| *score >= min_episode_score)
        });
        if let Some(candidate) = replacement {
            if k >= slot_from_end {
                surfaced[k - slot_from_end] = candidate.clone();
            }
        }
    }
    surfaced
}

fn push_candidate(pool: &mut HashMap<String, Candidate>, c: Candidate) {
    pool.entry(c.id.clone())
        .and_modify(|e| {
            for (route, score) in &c.route_scores {
                e.route_scores
                    .entry(*route)
                    .and_modify(|existing| *existing = existing.max(*score))
                    .or_insert(*score);
            }
            if c.score > e.score {
                e.score = c.score;
                e.best_route = c.best_route;
                e.conversation_id = c.conversation_id.clone();
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

/// First ~PREVIEW_CHARS chars of content, newlines/carriage-returns flattened to spaces.
fn clean_excerpt(content: &str) -> String {
    let s: String = content.chars().take(crate::format::PREVIEW_CHARS).collect();
    s.replace(['\n', '\r'], " ")
}

/// Best-scoring chunk in `conv` against `query_vec`, by EXACT cosine over that
/// conversation's own embeddings. `None` if the conversation has no chunks or no
/// stored embeddings (never panics — old/unenriched conversations are common).
fn best_chunk_for_conv(
    storage: &Storage,
    query_vec: &[f32],
    conv: &str,
    family_projects: &[String],
) -> Result<Option<(String, f32)>> {
    let ids = storage.with_connection(|conn| {
        crate::storage::queries::get_chunk_ids_for_conversation_in_projects(
            conn,
            conv,
            family_projects,
        )
    })?;
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

enum EpisodeTarget {
    Resolved { chunk_id: String, cosine: f32 },
    Dangling,
    OutOfScope,
    Unembedded,
}

fn resolve_episode_target(
    storage: &Storage,
    query_vec: &[f32],
    conv: &str,
    family_projects: &[String],
) -> Result<EpisodeTarget> {
    let all_ids = storage.get_chunk_ids_for_conversation(conv)?;
    if all_ids.is_empty() {
        return Ok(EpisodeTarget::Dangling);
    }
    let scoped_ids = if family_projects.is_empty() {
        all_ids
    } else {
        storage.with_connection(|conn| {
            crate::storage::queries::get_chunk_ids_for_conversation_in_projects(
                conn,
                conv,
                family_projects,
            )
        })?
    };
    if scoped_ids.is_empty() {
        return Ok(EpisodeTarget::OutOfScope);
    }
    let vectors = storage.get_chunk_vectors_by_ids(&scoped_ids)?;
    let Some((chunk_id, cosine)) = vectors
        .into_iter()
        .map(|(id, vector)| {
            let score = cosine(query_vec, &vector);
            (id, score)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return Ok(EpisodeTarget::Unembedded);
    };
    Ok(EpisodeTarget::Resolved { chunk_id, cosine })
}

fn resolve_family_projects(storage: &Storage, project: Option<&str>) -> Result<Vec<String>> {
    let Some(anchor) = project else {
        return Ok(Vec::new());
    };
    let mut projects =
        storage.with_connection(crate::storage::queries::reinstatement_project_names)?;
    projects
        .retain(|candidate| crate::search::cross_project::same_project_family(anchor, candidate));
    if !projects.iter().any(|candidate| candidate == anchor) {
        projects.push(anchor.to_string());
    }
    projects.sort_by(|a, b| (a != anchor, a).cmp(&(b != anchor, b)));
    projects.dedup();
    Ok(projects)
}

fn add_identifier_variants(token: &str, out: &mut BTreeSet<String>) {
    let token = token.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '?' | '!'
        )
    });
    if token.is_empty()
        || !token
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '/' | '\\' | '.' | '-'))
    {
        return;
    }
    out.insert(token.to_string());
    if token.contains("::") {
        if let Some(bare) = token.rsplit("::").next().filter(|part| !part.is_empty()) {
            out.insert(bare.to_string());
        }
    }
    if token.contains(['/', '\\']) || token.contains('.') {
        for part in token.split(['/', '\\']) {
            let stem = part.split('.').next().unwrap_or(part);
            if !stem.is_empty() {
                out.insert(stem.to_string());
            }
        }
    }
}

/// Extract identifier-shaped query tokens. Plain lowercase prose is ignored;
/// exact node validation is the final authority for every candidate.
fn query_identifier_candidates(query: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut rest = query;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else {
            break;
        };
        add_identifier_variants(&rest[..close], &mut out);
        rest = &rest[close + 1..];
    }
    for raw in query.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        let has_snake = token.contains('_');
        let has_scope_or_path = raw.contains("::") || raw.contains('/') || raw.contains('\\');
        let has_camel =
            token.chars().any(char::is_uppercase) && token.chars().any(char::is_lowercase);
        if has_snake || has_scope_or_path || has_camel {
            add_identifier_variants(raw, &mut out);
        }
    }
    out.into_iter().take(MAX_IDENTIFIER_CANDIDATES).collect()
}

fn select_semantic_seed_hits(
    hits: &[crate::search::SearchResult],
    meta_by_id: &HashMap<&str, &crate::import::ConversationChunk>,
    query_lower: &str,
    limit: usize,
) -> Vec<(crate::search::SearchResult, Via)> {
    if limit == 0 || hits.is_empty() {
        return Vec::new();
    }
    let contents: Vec<Option<&str>> = hits
        .iter()
        .map(|hit| {
            meta_by_id
                .get(hit.id.as_str())
                .map(|chunk| chunk.content.as_str())
        })
        .collect();
    let ordered = select_seed_indexes(&contents, query_lower, hits.len());
    let best_score = ordered
        .iter()
        .find_map(|index| {
            meta_by_id
                .contains_key(hits[*index].id.as_str())
                .then_some(hits[*index].score)
        })
        .unwrap_or(0.0);
    let mut conversations = HashSet::new();
    let mut selected = Vec::new();
    for index in &ordered {
        let hit = &hits[*index];
        let Some(conv) = meta_by_id
            .get(hit.id.as_str())
            .map(|chunk| chunk.conversation_id.as_str())
        else {
            continue;
        };
        if hit.score >= 0.30
            && hit.score >= best_score - 0.12
            && conversations.insert(conv.to_string())
        {
            selected.push((hit.clone(), Via::Seed));
            if selected.len() == limit {
                return selected;
            }
        }
    }
    if selected.len() < limit {
        for index in ordered {
            let hit = &hits[index];
            let Some(conv) = meta_by_id
                .get(hit.id.as_str())
                .map(|chunk| chunk.conversation_id.as_str())
            else {
                continue;
            };
            if (0.20..0.30).contains(&hit.score) && conversations.insert(conv.to_string()) {
                selected.push((hit.clone(), Via::SeedLowConfidence));
                break;
            }
        }
    }
    selected
}

/// Walk the episode chain from `conv`'s session-episode reflection to
/// the conversation id of the PREVIOUS episode, if one exists.
/// `prev_episode_id` in the stored episode JSON is already a
/// conversation/session id — it is written by
/// `hooks::stop::pick_prev_episode`, which returns the `session_id`
/// half of a `(session_id, timestamp)` candidate tuple, never a
/// `reflections.id` — so no further reflection lookup is needed or
/// correct. `None` on any missing link (no episode reflection,
/// malformed JSON, empty prev_episode_id) — episode chains are sparse
/// by design in this phase (not every session gets an episode
/// reflection), this must never error the whole walk.
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
    Ok(v.get("prev_episode_id")
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

/// The proven reinstatement walk: query symbols + semantic seeds (hop 1) ->
/// blend + graph spread + episode chain (hop 2), fused by max score per chunk
/// id while preserving route sets, then truncated to `cfg.k`.
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
) -> Result<ReinstateResult> {
    if cfg.k == 0 {
        return Ok(ReinstateResult::default());
    }

    let query_vec: Vec<f32> = {
        let q = query.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&q)).await??
    };

    // Resolve the family exactly once, then carry the same set through chunks,
    // code_evolution, nodes, edges, and episode target validation.
    let family_projects = resolve_family_projects(storage, project)?;
    let mut trace = ReinstateTrace {
        scope_projects: if project.is_some() {
            family_projects.clone()
        } else {
            vec!["all".to_string()]
        },
        ..ReinstateTrace::default()
    };
    let project_chunk_ids: Option<HashSet<String>> = if project.is_some() {
        Some(
            storage
                .with_connection(|conn| {
                    crate::storage::queries::get_chunk_ids_for_projects(conn, &family_projects)
                })?
                .into_iter()
                .collect(),
        )
    } else {
        None
    };

    // ---- hop 1: chunks (project-scoped if given) + reflections, merged ----
    // 2x over-fetch so seed selection can skip verbatim query echoes and still
    // find cfg.seeds real seeds — on a re-asked question the top hits are all
    // prior askings of it. Final output stays capped at cfg.k after rerank.
    let hop1_k = (cfg.k * 4).max(cfg.seeds * 8);
    let (chunk_hits, reflection_hits) = {
        let idx = search.read().await;
        let chunks = if let Some(ref ids) = project_chunk_ids {
            idx.search_chunks_filtered(&query_vec, hop1_k, 0.20, ids)
        } else {
            idx.search_chunks(&query_vec, hop1_k, 0.20)
        };
        let reflections = idx.search_reflections(&query_vec, cfg.k, cfg.min_score);
        (chunks, reflections)
    };

    let mut pool: HashMap<String, Candidate> = HashMap::new();

    let chunk_ids: Vec<String> = chunk_hits.iter().map(|r| r.id.clone()).collect();
    let chunk_meta = storage.get_chunks_by_ids(&chunk_ids)?;
    let meta_by_id: HashMap<&str, &crate::import::ConversationChunk> =
        chunk_meta.iter().map(|c| (c.id.as_str(), c)).collect();

    for r in &reflection_hits {
        if let Ok(Some((_content, tags, _timestamp))) = storage.get_reflection_by_id(&r.id) {
            let conv = tags
                .iter()
                .find_map(|t| t.strip_prefix("conv_").map(str::to_string))
                .unwrap_or_else(|| format!("refl_{}", r.id));
            push_candidate(
                &mut pool,
                Candidate::new(r.id.clone(), conv, r.score, Via::Reflection),
            );
        }
    }

    // seeds = top-N NON-ECHO chunk hits (hits are already score-sorted by
    // SearchEngine; echoes only fill in when nothing else matched)
    let query_lower = query.to_lowercase();
    let semantic_seeds =
        select_semantic_seed_hits(&chunk_hits, &meta_by_id, &query_lower, cfg.seeds);
    trace.seeds_selected = semantic_seeds.len();
    trace.seed_conversations = semantic_seeds
        .iter()
        .filter_map(|(seed, _)| meta_by_id.get(seed.id.as_str()))
        .map(|chunk| chunk.conversation_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    for (seed, route) in &semantic_seeds {
        if let Some(chunk) = meta_by_id.get(seed.id.as_str()) {
            push_candidate(
                &mut pool,
                Candidate::new(
                    seed.id.clone(),
                    chunk.conversation_id.clone(),
                    seed.score,
                    *route,
                ),
            );
        }
    }

    // Every hop-1 hit competes in the fused pool, not just the selected
    // seeds. Dropping ranks 4+ made reinstate() a SUBSET of plain blend
    // retrieval — measured live (eval --provenance, 2026-08-09): two
    // queries where one-shot kNN found GT sessions and arm B found zero.
    // Reinstatement adds routes; it must never subtract hop-1 evidence.
    for hit in &chunk_hits {
        if let Some(chunk) = meta_by_id.get(hit.id.as_str()) {
            push_candidate(
                &mut pool,
                Candidate::new(
                    hit.id.clone(),
                    chunk.conversation_id.clone(),
                    hit.score,
                    Via::Blend,
                ),
            );
        }
    }

    // Exact query identifiers launch structural seeds from node attribution
    // and incident edge provenance. These bypass the semantic score floor.
    let identifiers = query_identifier_candidates(query);
    let mut symbol_nodes = storage.with_connection(|conn| {
        crate::storage::queries::exact_code_nodes_in_projects(
            conn,
            &identifiers,
            &family_projects,
            project.unwrap_or(""),
            MAX_SYMBOL_NODES,
        )
    })?;
    if let Some(anchor) = project {
        symbol_nodes.sort_by_key(|node| node.project != anchor);
    }
    trace.symbol_names = symbol_nodes
        .iter()
        .map(|node| node.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    trace.symbols_matched = trace.symbol_names.len();

    let mut structural_conversations = BTreeSet::new();
    let node_ids: Vec<String> = symbol_nodes.iter().map(|node| node.id.clone()).collect();
    structural_conversations.extend(storage.with_connection(|conn| {
        crate::storage::queries::transcript_attribution_conversations(conn, &node_ids)
    })?);
    structural_conversations.extend(storage.with_connection(|conn| {
        crate::storage::queries::incident_edge_conversations(
            conn,
            &node_ids,
            MAX_STRUCTURAL_CONVERSATIONS,
        )
    })?);
    let structural_conversations: Vec<String> = structural_conversations
        .into_iter()
        .take(MAX_STRUCTURAL_CONVERSATIONS)
        .collect();

    let mut structural_launches = Vec::new();
    for conv in structural_conversations {
        trace.graph_walks += 1;
        if let Some((id, cos)) = best_chunk_for_conv(storage, &query_vec, &conv, &family_projects)?
        {
            trace.graph_accepted += 1;
            trace.structural_graph_accepted += 1;
            push_candidate(
                &mut pool,
                Candidate::new(id.clone(), conv.clone(), cos * cfg.graph_boost, Via::Graph),
            );
            structural_launches.push((id, conv));
        }
    }

    let seed_ids: Vec<String> = semantic_seeds
        .iter()
        .map(|(seed, _)| seed.id.clone())
        .chain(structural_launches.iter().map(|(id, _)| id.clone()))
        .collect();
    let seed_vecs: HashMap<String, Vec<f32>> = storage
        .get_chunk_vectors_by_ids(&seed_ids)?
        .into_iter()
        .collect();

    let mut launches: Vec<(String, String)> = semantic_seeds
        .iter()
        .filter_map(|(seed, _)| {
            meta_by_id
                .get(seed.id.as_str())
                .map(|chunk| (seed.id.clone(), chunk.conversation_id.clone()))
        })
        .chain(structural_launches)
        .collect();
    let mut launched_conversations = HashSet::new();
    launches.retain(|(_, conv)| launched_conversations.insert(conv.clone()));
    // Widen the walk beyond the selected seeds: up to cfg.seeds additional
    // launch anchors from the remaining hop-1 hits (score order, distinct
    // conversations), preferring conversations that HAVE file attribution —
    // those are the ones a co-edit walk can actually spread from. When the
    // top seeds land in conversations with no attribution and no episode
    // links, the walk stages ran zero times and the machinery sat inert on
    // live prose queries (Q1/Q7, eval --provenance 2026-08-09). Still
    // evidence-driven: every extra anchor was itself retrieved for this
    // query; attribution only orders the scan, it never fabricates a hit.
    const EXTRA_ANCHOR_SCAN: usize = 24;
    let mut attributed: Vec<(String, String)> = Vec::new();
    let mut unattributed: Vec<(String, String)> = Vec::new();
    let mut scanned_conversations = launched_conversations.clone();
    for hit in chunk_hits.iter().take(EXTRA_ANCHOR_SCAN) {
        let Some(chunk) = meta_by_id.get(hit.id.as_str()) else {
            continue;
        };
        if !scanned_conversations.insert(chunk.conversation_id.clone()) {
            continue;
        }
        let has_files = !storage
            .with_connection(|conn| {
                crate::storage::queries::files_for_session_in_projects(
                    conn,
                    &chunk.conversation_id,
                    &family_projects,
                    1,
                )
            })?
            .is_empty();
        let entry = (hit.id.clone(), chunk.conversation_id.clone());
        if has_files {
            attributed.push(entry);
        } else {
            unattributed.push(entry);
        }
    }
    for (chunk_id, conv) in attributed.into_iter().chain(unattributed) {
        if launches.len() >= cfg.seeds * 2 + 2 {
            break;
        }
        if launched_conversations.insert(conv.clone()) {
            launches.push((chunk_id, conv));
        }
    }
    let mut episode_accepted_ids = BTreeSet::new();

    for (seed_id, seed_conv) in launches {
        // (1) blended context vector, second-hop chunk search (project-filtered when scoped)
        if let Some(sv) = seed_vecs.get(&seed_id) {
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
                        Candidate::new(
                            r.id.clone(),
                            c.conversation_id.clone(),
                            r.score,
                            Via::Blend,
                        ),
                    );
                }
            }
        }

        // (2) co-edit spread is the fallback when the query did not resolve a
        // symbol. Every attempted neighbor is a walk; only candidates meeting
        // the structural one-hop cosine floor are accepted.
        if symbol_nodes.is_empty() {
            let mut graph_cands: Vec<Candidate> = Vec::new();
            let files = storage.with_connection(|conn| {
                crate::storage::queries::files_for_session_in_projects(
                    conn,
                    &seed_conv,
                    &family_projects,
                    4,
                )
            })?;
            for file in files {
                let neighbors = storage.with_connection(|conn| {
                    crate::storage::queries::sessions_for_file_in_projects(
                        conn,
                        &file,
                        &seed_conv,
                        &family_projects,
                        12,
                    )
                })?;
                for neighbor in neighbors {
                    trace.graph_walks += 1;
                    if let Some((id, cos)) =
                        best_chunk_for_conv(storage, &query_vec, &neighbor, &family_projects)?
                    {
                        if cos >= 0.30 {
                            trace.graph_accepted += 1;
                            graph_cands.push(Candidate::new(
                                id,
                                neighbor,
                                cos * cfg.graph_boost,
                                Via::Graph,
                            ));
                        }
                    }
                }
            }
            graph_cands.sort_by(|a, b| b.score.total_cmp(&a.score));
            graph_cands.truncate(cfg.graph_cap_per_seed);
            for candidate in graph_cands {
                push_candidate(&mut pool, candidate);
            }
        }

        // (3) episode chain: seed session's episode -> prev episode -> its session
        if let Some(prev_conv) = episode_prev_session(storage, &seed_conv)? {
            trace.episode_links += 1;
            match resolve_episode_target(storage, &query_vec, &prev_conv, &family_projects)? {
                EpisodeTarget::Resolved { chunk_id, cosine } => {
                    trace.episode_resolved += 1;
                    if cosine >= 0.30 {
                        episode_accepted_ids.insert(chunk_id.clone());
                        push_candidate(
                            &mut pool,
                            Candidate::new(
                                chunk_id,
                                prev_conv,
                                cosine * cfg.graph_boost,
                                Via::Episode,
                            ),
                        );
                    } else {
                        trace.episode_below_threshold += 1;
                    }
                }
                EpisodeTarget::Dangling => trace.episode_dangling += 1,
                EpisodeTarget::OutOfScope => trace.episode_out_of_scope += 1,
                EpisodeTarget::Unembedded => trace.episode_unembedded += 1,
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
        .filter(|c| !c.is_reflection_only())
        .map(|c| c.id.clone())
        .collect();
    let mut detail: HashMap<String, CandidateDetail> = HashMap::new();
    for m in storage.get_chunks_by_ids(&chunk_pool_ids)? {
        let prov = storage.get_chunk_provenance(&m.id).ok().flatten();
        detail.insert(m.id, (prov, m.timestamp, m.content));
    }
    for c in fused.iter().filter(|c| c.is_reflection_only()) {
        if let Ok(Some((content, _tags, timestamp))) = storage.get_reflection_by_id(&c.id) {
            detail.insert(c.id.clone(), (None, timestamp, content));
        }
    }
    // Drop candidates whose backing row vanished (pruned chunk, deleted
    // reflection) BEFORE ranking so they don't occupy ranked slots.
    fused.retain(|c| detail.contains_key(&c.id));

    let fused = rerank_pool(fused, &detail, query);
    let fused = truncate_with_episode_quota(fused, cfg.k, 0.30 * cfg.graph_boost);

    // Shadow signal only: fetch after ordering is fully fixed (sort + rerank +
    // truncate). Never used for ranking, filtering, or score mutation.
    let ratification_ids: Vec<String> = fused.iter().map(|c| c.conversation_id.clone()).collect();
    let ratification_scores = storage
        .get_ratification_scores(&ratification_ids)
        .unwrap_or_default();

    let mut items = Vec::with_capacity(fused.len());
    for c in fused {
        let Some((_prov, timestamp, content)) = detail.get(&c.id) else {
            continue;
        };
        let ratification = ratification_scores.get(&c.conversation_id).copied();
        let routes = c.routes();
        items.push(EvidenceItem {
            chunk_id: c.id,
            conversation_id: c.conversation_id,
            score: c.score,
            best_route: c.best_route,
            routes,
            route_scores: c.route_scores,
            timestamp: timestamp.clone(),
            excerpt: clean_excerpt(content),
            ratification,
        });
    }

    trace.episode_accepted = episode_accepted_ids.len();
    let surfaced_episode_ids: HashSet<&str> = items
        .iter()
        .filter(|item| item.routes.contains(&Via::Episode))
        .map(|item| item.chunk_id.as_str())
        .collect();
    trace.episode_below_cut = episode_accepted_ids
        .iter()
        .filter(|chunk_id| !surfaced_episode_ids.contains(chunk_id.as_str()))
        .count();

    let shadow_log: Vec<serde_json::Value> = items
        .iter()
        .enumerate()
        .map(|(rank, it)| {
            serde_json::json!({
                "conv_id": it.conversation_id,
                "rank": rank,
                "score": it.score,
                "ratification_score": it.ratification,
            })
        })
        .collect();
    tracing::debug!(
        target: "ratification_shadow",
        qid = "none",
        results = %serde_json::Value::Array(shadow_log),
        "ratification_shadow"
    );

    Ok(ReinstateResult { items, trace })
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
            Candidate::new("c1".into(), "conv1".into(), 0.5, Via::Seed),
        );
        push_candidate(
            &mut pool,
            Candidate::new("c1".into(), "conv1".into(), 0.8, Via::Blend),
        );
        push_candidate(
            &mut pool,
            Candidate::new("c1".into(), "conv1".into(), 0.3, Via::Graph),
        );
        assert_eq!(pool.len(), 1);
        let c = &pool["c1"];
        assert!((c.score - 0.8).abs() < f32::EPSILON);
        assert_eq!(c.best_route, Via::Blend);
        assert_eq!(
            c.routes(),
            BTreeSet::from([Via::Seed, Via::Blend, Via::Graph])
        );
    }

    #[test]
    fn dedup_keeps_distinct_ids_separate() {
        let mut pool: HashMap<String, Candidate> = HashMap::new();
        push_candidate(
            &mut pool,
            Candidate::new("c1".into(), "conv1".into(), 0.5, Via::Seed),
        );
        push_candidate(
            &mut pool,
            Candidate::new("c2".into(), "conv1".into(), 0.4, Via::Graph),
        );
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn excerpt_truncates_and_cleans_newlines() {
        let long = "a".repeat(600) + "\nline2\r\nline3";
        let e = clean_excerpt(&long);
        assert_eq!(e.chars().count(), crate::format::PREVIEW_CHARS);
        assert!(!e.contains('\n'));
        assert!(!e.contains('\r'));
    }

    fn cand(id: &str, conv: &str, score: f32, via: Via) -> Candidate {
        Candidate::new(id.into(), conv.into(), score, via)
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
        assert_eq!(ranked[0].best_route, Via::Graph);
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
    fn semantic_seeds_are_distinct_banded_and_allow_one_labelled_fallback() {
        let make_chunk = |id: &str, conv: &str| crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conv.into(),
            project_name: "project".into(),
            timestamp: "2026-08-01T00:00:00Z".into(),
            content: "organic evidence".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let chunks = [
            make_chunk("a1", "conv-a"),
            make_chunk("a2", "conv-a"),
            make_chunk("b", "conv-b"),
            make_chunk("outside-band", "conv-c"),
            make_chunk("fallback", "conv-d"),
        ];
        let meta: HashMap<&str, &crate::import::ConversationChunk> = chunks
            .iter()
            .map(|chunk| (chunk.id.as_str(), chunk))
            .collect();
        let hits = vec![
            crate::search::SearchResult {
                id: "a1".into(),
                score: 0.90,
            },
            crate::search::SearchResult {
                id: "a2".into(),
                score: 0.88,
            },
            crate::search::SearchResult {
                id: "b".into(),
                score: 0.80,
            },
            crate::search::SearchResult {
                id: "outside-band".into(),
                score: 0.77,
            },
            crate::search::SearchResult {
                id: "fallback".into(),
                score: 0.25,
            },
        ];

        let selected = select_semantic_seed_hits(&hits, &meta, "unquoted query", 3);
        let selected_ids: Vec<(&str, Via)> = selected
            .iter()
            .map(|(hit, route)| (hit.id.as_str(), *route))
            .collect();
        assert_eq!(
            selected_ids,
            vec![
                ("a1", Via::Seed),
                ("b", Via::Seed),
                ("fallback", Via::SeedLowConfidence),
            ]
        );
    }

    #[test]
    fn identifier_lexer_handles_backticks_snake_camel_scope_and_paths() {
        let got = query_identifier_candidates(
            "why `plain` retire_missing_nodes CamelCase module::symbol src/search/reinstatement.rs",
        );
        for expected in [
            "plain",
            "retire_missing_nodes",
            "CamelCase",
            "module::symbol",
            "symbol",
            "reinstatement",
        ] {
            assert!(got.iter().any(|candidate| candidate == expected), "{got:?}");
        }
    }

    #[test]
    fn rendered_receipt_invariant_detects_surface_counter_drift() {
        let item = EvidenceItem {
            chunk_id: "chunk".into(),
            conversation_id: "conv".into(),
            score: 0.8,
            best_route: Via::Graph,
            routes: BTreeSet::from([Via::Seed, Via::Graph]),
            route_scores: BTreeMap::from([(Via::Seed, 0.7), (Via::Graph, 0.8)]),
            timestamp: "2026-08-01T00:00:00Z".into(),
            excerpt: "evidence".into(),
            ratification: None,
        };
        let trace = ReinstateTrace {
            scope_projects: vec!["project".into()],
            seeds_selected: 1,
            seed_conversations: 1,
            graph_walks: 3,
            graph_accepted: 2,
            ..ReinstateTrace::default()
        };
        let receipt = render_receipt(&trace, std::slice::from_ref(&item));
        assert!(rendered_receipt_surface_counts_match(
            &receipt,
            &trace,
            std::slice::from_ref(&item)
        ));
        let tampered = receipt.replace("accepted=2 surfaced=1", "accepted=2 surfaced=0");
        assert!(!rendered_receipt_surface_counts_match(
            &tampered,
            &trace,
            &[item]
        ));
    }

    #[test]
    fn via_display_is_lowercase() {
        assert_eq!(Via::Seed.to_string(), "seed");
        assert_eq!(Via::SeedLowConfidence.to_string(), "seed-low-confidence");
        assert_eq!(Via::Blend.to_string(), "blend");
        assert_eq!(Via::Graph.to_string(), "graph");
        assert_eq!(Via::Episode.to_string(), "episode");
        assert_eq!(Via::Reflection.to_string(), "reflection");
    }

    #[test]
    fn ratification_is_shadow_only_does_not_affect_order() {
        // Two candidates where similarity order is fixed by rerank_pool
        // (via plain cosine, no provenance/echo signal in play) — "hi" outranks
        // "lo" by raw score. A ratification score that INVERTS this (lo=0.99,
        // hi=0.01) must have zero effect on rerank_pool's output order, because
        // rerank_pool has no access to ratification data at all — proving the
        // signal cannot leak into ordering through this path.
        let detail: HashMap<String, CandidateDetail> = HashMap::new();
        let fused = vec![
            cand("lo", "conv_lo", 0.40, Via::Blend),
            cand("hi", "conv_hi", 0.80, Via::Seed),
        ];
        let ranked = rerank_pool(fused, &detail, "some long enough query text");
        assert_eq!(ranked[0].id, "hi");
        assert_eq!(ranked[1].id, "lo");
        // Simulate what a ratification map WOULD say (inverted vs similarity)
        // purely to document intent — never consulted by rerank_pool above.
        let mut ratification_scores: HashMap<String, f32> = HashMap::new();
        ratification_scores.insert("conv_lo".into(), 0.99);
        ratification_scores.insert("conv_hi".into(), 0.01);
        // Attaching (as reinstate() does, post-ordering) must not change order:
        let final_order: Vec<&str> = ranked.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(final_order, vec!["hi", "lo"]);
        // Ratification values themselves are simply readable, independent of order:
        assert_eq!(ratification_scores.get("conv_lo").copied(), Some(0.99));
        assert_eq!(ratification_scores.get("conv_hi").copied(), Some(0.01));
    }

    #[test]
    fn episode_prev_session_returns_prev_id_directly_not_via_reflection_lookup() {
        // `prev_episode_id` in a stored episode reflection is already a
        // conversation id (written by hooks::stop::pick_prev_episode,
        // which returns a session_id, never a reflections.id) —
        // episode_prev_session must use it directly, not treat it as
        // another reflection to look up. The pre-fix code, which called
        // `storage.get_reflection_by_id(prev_id)`, would return None here
        // because no reflection with id "prev-conv-id" exists — only a
        // conversation by that id is implied.
        let storage = crate::storage::Storage::open_memory().unwrap();
        let content = serde_json::json!({
            "prev_episode_id": "prev-conv-id"
        })
        .to_string();
        storage
            .insert_reflection(
                "refl-current-episode",
                &content,
                &[
                    "session_episode".to_string(),
                    "conv_current-conv".to_string(),
                ],
                &[],
            )
            .unwrap();

        let result = episode_prev_session(&storage, "current-conv").unwrap();
        assert_eq!(result, Some("prev-conv-id".to_string()));
    }

    #[test]
    fn episode_prev_session_none_when_no_episode_reflection() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let result = episode_prev_session(&storage, "no-such-conv").unwrap();
        assert_eq!(result, None);
    }
}
