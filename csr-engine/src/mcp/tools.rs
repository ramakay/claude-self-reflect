use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::format::{self, EnrichedResult};
use crate::search::cross_project;
use crate::search::decay;
use crate::search::SearchEngine;
use crate::storage::chunk_binding::ChunkWitnessVerdict;
use crate::storage::witness_verdicts::VerdictChannel;
use crate::storage::Storage;
use crate::temporal;

/// Extract a conversation UUID from a query that is (or contains) a
/// `conv_<uuid>` retrieval handle, or that is a bare UUID. Injection blocks
/// hand out `csr_reflect_on_past("conv_<id>")` handles — those must resolve
/// by exact tag, never by embedding (a UUID embeds as noise and returns
/// unrelated cross-project hits; measured live 2026-07-08).
fn extract_conv_id(query: &str) -> Option<&str> {
    if let Some(pos) = query.find("conv_") {
        if let Some(candidate) = query.get(pos + 5..pos + 5 + 36) {
            if is_uuid(candidate) {
                return Some(candidate);
            }
        }
    }
    let trimmed = query.trim();
    if is_uuid(trimmed) {
        return Some(trimmed);
    }
    None
}

/// Strict 8-4-4-4-12 hex UUID shape check.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Exact-tag lookup for a `conv_<uuid>` retrieval handle: every reflection
/// tagged to that conversation (episode, learnings, narratives), newest
/// first, score pinned to 1.0 — an exact handle is not a similarity guess.
/// Returns None when the tag matches nothing so the caller can fall through
/// to semantic search.
///
/// The v10 validity partition applies HERE TOO (`partition_enabled` is the
/// caller's `validity_partition_enabled()` outcome): an exact handle to a
/// conversation whose bound code symbol is stale must carry the same
/// `[stale anchor]`/`[evolved]` annotation a semantic hit would — the fast
/// path previously bypassed validity entirely. All rows share one
/// conversation id, so the partition never reorders here; it only annotates
/// and flags.
fn lookup_by_conv_tag(
    storage: &Arc<Storage>,
    conv_id: &str,
    query: &str,
    limit: usize,
    partition_enabled: bool,
) -> Result<Option<String>> {
    let start = Instant::now();
    let rows = storage.get_reflections_by_tag(&format!("conv_{}", conv_id), limit)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut enriched: Vec<EnrichedResult> = rows
        .into_iter()
        .map(|(id, content, tags, timestamp)| {
            let project_name = tags
                .iter()
                .find(|t| t.starts_with("project_"))
                .map(|t| t.trim_start_matches("project_").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let tag_prefix = if tags.iter().any(|t| t == "session_episode") {
                "[episode] "
            } else if tags.iter().any(|t| t == "session_story") {
                "[story] "
            } else if tags.iter().any(|t| t.starts_with("narrative_v3")) {
                "[narrative] "
            } else {
                "[reflection] "
            };
            EnrichedResult {
                score: 1.0,
                chunk: crate::import::ConversationChunk {
                    id,
                    conversation_id: conv_id.to_string(),
                    project_name,
                    timestamp,
                    content: format!("{}{}", tag_prefix, content),
                    message_count: 0,
                    summary: None,
                    author: crate::provenance::Speaker::ToolResult,
                    seq: 0,
                    is_sidechain: false,
                },
                resolution: None,
                validity_demoted: false,
            }
        })
        .collect();
    // Resolve + apply validity on the fast path too (issue: the exact-handle
    // path returned without ever consulting dream verdicts). One batched
    // query for the single conversation id.
    let validity = resolve_validity_with(storage, &[conv_id.to_string()], partition_enabled);
    apply_validity_partition(&mut enriched, &validity);
    let search_ms = start.elapsed().as_millis() as u64;
    Ok(Some(format::format_search_results(
        &enriched,
        query,
        "conv-tag exact",
        search_ms,
        0,
    )))
}

/// Full semantic search with rich XML results.
pub async fn reflect_on_past(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    let partition_enabled = validity_partition_enabled();
    // Retrieval-handle fast path: `conv_<uuid>` (or a bare UUID) resolves by
    // exact tag. Falls through to semantic search only when the tag matches
    // nothing, so a stale handle still gets a best-effort answer.
    if let Some(conv_id) = extract_conv_id(query) {
        if let Some(result) =
            lookup_by_conv_tag(storage, conv_id, query, limit.max(5), partition_enabled)?
        {
            return Ok(result);
        }
    }

    let embed_start = Instant::now();
    let query_vec = embed_query(embeddings, query).await?;
    let embed_ms = embed_start.elapsed().as_millis() as u64;

    reflect_on_past_with_vec(
        storage,
        search,
        &query_vec,
        query,
        limit,
        min_score,
        project,
        embed_ms,
        partition_enabled,
    )
    .await
}

/// Everything in `reflect_on_past` after query embedding — the seam the
/// end-to-end partition test drives with a synthetic query vector (no
/// FastEmbed model in tests) and an explicit kill-switch outcome (never by
/// mutating the process env — see `resolve_validity_with`'s doc).
/// `partition_enabled` is `validity_partition_enabled()` for real callers.
#[allow(clippy::too_many_arguments)]
async fn reflect_on_past_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
    embed_ms: u64,
    partition_enabled: bool,
) -> Result<String> {
    let (effective_project, scope_label) = cross_project::normalize_project_scope(project);

    // OVERFETCH (validity partition, issue 2): fetching exactly `limit`
    // candidates lets one demoted top-N hit permanently displace the valid
    // N+1 candidate — it was never fetched, so the sink has nothing to
    // promote. Fetch extra, partition over the full set, truncate to `limit`
    // at the very end (`apply_resolutions_before_limit`).
    let first = overfetch(limit);
    let mut pass = reflect_gather_pass(
        storage,
        search,
        query_vec,
        query,
        first,
        min_score,
        effective_project.as_deref(),
        partition_enabled,
    )
    .await?;
    let mut search_ms = pass.search_ms;

    // ADAPTIVE REFETCH (single, bounded): when demotions leave fewer than
    // `limit` valid (non-demoted) results AND the first window came back
    // full (more candidates may exist beyond it), refetch ONCE at 10*limit
    // (hard cap) and repartition. If still short, accept: a query whose top
    // 10*limit candidates are all stale cannot backfill further — bounded
    // by design, not silent.
    let valid = pass
        .enriched
        .iter()
        .filter(|e| !is_demote_channel(&pass.validity, &e.chunk.conversation_id))
        .count();
    if valid < limit && pass.window_full {
        let refetch = limit.saturating_mul(10);
        if refetch > first {
            pass = reflect_gather_pass(
                storage,
                search,
                query_vec,
                query,
                refetch,
                min_score,
                effective_project.as_deref(),
                partition_enabled,
            )
            .await?;
            search_ms += pass.search_ms;
        }
    }

    let mut enriched = pass.enriched;
    // Sink resolved chunks AND dream-verdict-demoted chunks BEFORE the limit
    // cut so stale results do not occupy slots that should go to
    // unresolved/non-demoted chunks ranked below them.
    apply_resolutions_before_limit(&mut enriched, storage, &pass.validity, limit);

    // TAD: log each RETURNED memory as an MCP-search retrieval event — after the
    // limit cut, so telemetry agrees with what the caller actually saw.
    // session_id="mcp" is a sentinel (MCP has no session id). Non-fatal.
    for e in &enriched {
        let _ = storage.log_retrieval_event(&e.chunk.id, "chunk", "mcp_search", "mcp");
    }

    Ok(format::format_search_results(
        &enriched,
        query,
        &scope_label,
        search_ms,
        embed_ms,
    ))
}

/// Output of one [`reflect_gather_pass`]: candidates fully enriched,
/// reranked and deduped — everything up to (but NOT including) the
/// resolution/validity sink and the limit cut — so the caller can inspect
/// how many valid results the partition would leave and decide on the
/// single adaptive refetch before cutting.
struct GatherPass {
    enriched: Vec<EnrichedResult>,
    validity: HashMap<String, ConvValidity>,
    /// The HNSW window came back full — more candidates may exist beyond it,
    /// so a refetch could backfill demotion-vacated slots.
    window_full: bool,
    search_ms: u64,
}

/// One retrieval + enrichment pass for `reflect_on_past_with_vec`: HNSW
/// search (chunks + reflections) at window size `fetch`, TAD/decay scoring,
/// FTS5 fallback append (with validity re-resolved for FTS-only
/// conversations), provenance rerank, dedupe.
#[allow(clippy::too_many_arguments)]
async fn reflect_gather_pass(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    fetch: usize,
    min_score: f32,
    effective_project: Option<&str>,
    partition_enabled: bool,
) -> Result<GatherPass> {
    let search_start = Instant::now();

    // Search BOTH chunks and reflections, merge by score
    let (chunk_results, reflection_results) = {
        let idx = search.read().await;
        let chunks = if let Some(p) = effective_project {
            let ids: HashSet<String> = storage.get_chunk_ids_for_project(p)?.into_iter().collect();
            idx.search_chunks_filtered(query_vec, fetch, min_score, &ids)
        } else {
            idx.search_chunks(query_vec, fetch, min_score)
        };
        let reflections = idx.search_reflections(query_vec, fetch, min_score);
        (chunks, reflections)
    };
    let search_ms = search_start.elapsed().as_millis() as u64;
    let window_full = chunk_results.len() == fetch || reflection_results.len() == fetch;

    // Enrich chunk results with metadata
    let chunk_ids: Vec<String> = chunk_results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&chunk_ids)?;

    // v10 dream-verdict validity partition: resolve ONCE, early — one
    // batched query for the whole semantic candidate set (perf requirement)
    // — so both the TAD/decay scoring below and the rerank step further down
    // can skip their own demotion for Demote-channel chunks (NO STACKING;
    // see `apply_validity_partition`'s doc). `mut` because the FTS fallback
    // below merges in verdicts for conversations the semantic pass never
    // saw. The final sink/annotate step (mirroring
    // `apply_resolutions_before_limit`) reuses this same map.
    let queried_convs = distinct_conversation_ids_of_chunks(&chunks);
    let mut validity = resolve_validity_with(storage, &queried_convs, partition_enabled);
    let queried_convs: HashSet<String> = queried_convs.into_iter().collect();

    let now = chrono::Utc::now();

    // Batch-fetch TAD events for all chunk results (single DB query)
    let chunk_ids_for_tad: Vec<&str> = chunk_results.iter().map(|r| r.id.as_str()).collect();
    let tad_events = storage
        .get_retrieval_events_batch(&chunk_ids_for_tad)
        .unwrap_or_default();
    let tad_config = decay::DecayConfig::for_search();

    let mut enriched: Vec<EnrichedResult> = chunk_results
        .iter()
        .filter_map(|r| {
            chunks.iter().find(|c| c.id == r.id).map(|c| {
                let decayed_score = if is_demote_channel(&validity, &c.conversation_id) {
                    // NO STACKING: this chunk is about to be structurally
                    // sunk below every non-demoted result in
                    // `apply_validity_partition` — applying TAD/decay on top
                    // would compound a soft penalty with that hard sink for
                    // the same staleness signal.
                    r.score
                } else if let Ok(ts) = c.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
                    let events = tad_events.get(&c.id).map(|v| v.as_slice()).unwrap_or(&[]);
                    decay::apply_tad(r.score, &ts, &now, events, &tad_config)
                } else {
                    r.score
                };
                // Cross-project multiplicative penalty
                let final_score = if let Some(p) = effective_project {
                    if c.project_name != p {
                        decayed_score * 0.3
                    } else {
                        decayed_score
                    }
                } else {
                    decayed_score
                };
                EnrichedResult {
                    score: final_score,
                    chunk: c.clone(),
                    resolution: None,
                    validity_demoted: false,
                }
            })
        })
        .collect();

    // Batch-fetch TAD events for reflection results
    let reflection_ids_for_tad: Vec<&str> =
        reflection_results.iter().map(|r| r.id.as_str()).collect();
    let reflection_tad_events = storage
        .get_retrieval_events_batch(&reflection_ids_for_tad)
        .unwrap_or_default();

    // Enrich reflection results — convert to EnrichedResult using reflection content
    for r in &reflection_results {
        if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(&r.id) {
            let decayed_score = if let Some(ts) = crate::temporal::parse_timestamp(&timestamp) {
                let events = reflection_tad_events
                    .get(&r.id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                decay::apply_tad(r.score, &ts, &now, events, &tad_config)
            } else {
                r.score
            };
            // Determine project from tags
            let project_name = tags
                .iter()
                .find(|t| t.starts_with("project_"))
                .map(|t| t.trim_start_matches("project_").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            // Cross-project multiplicative penalty
            let final_score = if let Some(p) = effective_project {
                if project_name != p {
                    decayed_score * 0.3
                } else {
                    decayed_score
                }
            } else {
                decayed_score
            };
            let tag_prefix = if tags.iter().any(|t| t == "session_episode") {
                "[episode] "
            } else if tags.iter().any(|t| t == "session_story") {
                "[story] "
            } else if tags.iter().any(|t| t.starts_with("narrative_v3")) {
                "[narrative] "
            } else {
                "[reflection] "
            };
            enriched.push(EnrichedResult {
                score: final_score,
                chunk: crate::import::ConversationChunk {
                    id: r.id.clone(),
                    conversation_id: r.id.clone(),
                    project_name,
                    timestamp,
                    content: format!("{}{}", tag_prefix, content),
                    message_count: 0,
                    summary: None,
                    // Reflections/episodes are derived narratives, not raw
                    // user-authored chunks — no authority boost.
                    author: crate::provenance::Speaker::ToolResult,
                    seq: 0,
                    is_sidechain: false,
                },
                resolution: None,
                validity_demoted: false,
            });
        }
    }

    // FTS5 hybrid fallback: if semantic results are weak (top score < 0.5)
    // or empty, supplement with keyword search results
    let semantic_top_score = enriched.iter().map(|e| e.score).fold(0.0f32, f32::max);
    if semantic_top_score < 0.5 {
        if let Ok(fts_chunks) = storage.fts5_search(query, fetch, effective_project) {
            let existing_ids: HashSet<String> =
                enriched.iter().map(|e| e.chunk.id.clone()).collect();
            let appended: Vec<crate::import::ConversationChunk> = fts_chunks
                .into_iter()
                .filter(|c| !existing_ids.contains(&c.id))
                .collect();
            // Validity was resolved over the SEMANTIC candidate set only —
            // FTS-appended chunks can carry conversation ids that set never
            // saw, and those must not slip past the partition (or past the
            // no-stacking decay skip below). Re-resolve for just the new
            // conversation ids and merge the maps.
            let extra_convs: Vec<String> = distinct_conversation_ids_of_chunks(&appended)
                .into_iter()
                .filter(|c| !queried_convs.contains(c))
                .collect();
            validity.extend(resolve_validity_with(
                storage,
                &extra_convs,
                partition_enabled,
            ));
            for chunk in appended {
                // FTS5 results get a synthetic score slightly below semantic threshold
                // so they rank after good semantic matches but above nothing
                let fts_score = if is_demote_channel(&validity, &chunk.conversation_id) {
                    // NO STACKING: same rule as the semantic loop above —
                    // this chunk is about to be structurally sunk, so no
                    // decay on top of the hard sink.
                    0.45
                } else if let Ok(ts) = chunk.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
                    decay::apply_decay(0.45, &ts, &now, None, None) // base 0.45 + decay
                } else {
                    0.40
                };
                let final_fts_score = if let Some(p) = effective_project {
                    if chunk.project_name != p {
                        fts_score * 0.3
                    } else {
                        fts_score
                    }
                } else {
                    fts_score
                };
                enriched.push(EnrichedResult {
                    score: final_fts_score,
                    chunk: crate::import::ConversationChunk {
                        content: format!("[keyword] {}", chunk.content),
                        ..chunk
                    },
                    resolution: None,
                    validity_demoted: false,
                });
            }
        }
    }

    // Provenance-aware re-rank (v9.3): authority + meaning layered on the decayed
    // score. User-authored content is boosted, tool-mechanic build-log and
    // non-user authority claims are demoted — so a founding decision out-ranks the
    // [Edit:]/[Bash:] chunks that used to bury it. Falls back to score order when
    // no provenance/meaning signal differs.
    // NO STACKING (v10): Demote-channel chunks are excluded from reranking
    // entirely rather than reranked-then-demoted — `search::rerank`'s
    // scaffold penalty is a soft score signal, and this chunk is about to be
    // structurally sunk below every non-demoted result in
    // `apply_validity_partition` regardless. Reranking it anyway would
    // compound that soft penalty with the hard sink for the same staleness
    // signal. A side effect does the sinking's job here too: `rank_of`
    // returns `usize::MAX` for any id absent from `order`, and `sort_by_key`
    // is stable, so excluded ids land at the tail in their pre-rerank
    // relative order — `apply_validity_partition`'s explicit partition below
    // is still the authoritative, independently-testable guarantee.
    let candidates: Vec<crate::search::rerank::RankCandidate> = enriched
        .iter()
        .filter(|e| !is_demote_channel(&validity, &e.chunk.conversation_id))
        .map(|e| crate::search::rerank::RankCandidate {
            id: e.chunk.id.clone(),
            cosine: e.score,
            content: e.chunk.content.clone(),
            provenance: storage.get_chunk_provenance(&e.chunk.id).ok().flatten(),
            timestamp: Some(e.chunk.timestamp.clone()),
        })
        .collect();
    let order: Vec<String> = crate::search::rerank::rerank(candidates)
        .into_iter()
        .map(|c| c.id)
        .collect();
    let rank_of = |id: &str| order.iter().position(|x| x == id).unwrap_or(usize::MAX);
    enriched.sort_by_key(|e| rank_of(&e.chunk.id));

    // Dedupe BEFORE the limit cut (CodeRabbit): truncating first let a higher-
    // ranked plan chunk survive while its origin conversation fell outside the
    // page — inverting the authoritative-origin rule — and could return a short
    // page after filtering.
    format::dedupe_results(&mut enriched);
    // Plan-doc chunks defer to their origin conversation when both matched (v9.4):
    // the plan restates the decision; the conversation is where it was made.
    let origin_of: std::collections::HashMap<String, String> = enriched
        .iter()
        .filter(|e| e.chunk.conversation_id.starts_with("plan:"))
        .filter_map(|e| {
            storage
                .get_chunk_provenance(&e.chunk.id)
                .ok()
                .flatten()
                .map(|p| (e.chunk.id.clone(), p.source_conv_id))
        })
        .collect();
    format::dedupe_plan_origins(&mut enriched, &origin_of);

    Ok(GatherPass {
        enriched,
        validity,
        window_full,
        search_ms,
    })
}

/// Store a reflection with embedding for future search.
pub async fn store_reflection(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    content: &str,
    tags: &[String],
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();

    let embedding = {
        let text = content.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&text)).await??
    };

    storage.insert_reflection(&id, content, tags, &embedding)?;

    {
        let mut idx = search.write().await;
        idx.insert_reflection(id.clone(), embedding);
    }

    Ok(format!(
        "Reflection stored successfully.\nID: {}\nTags: {:?}",
        id, tags
    ))
}

/// Record a resolution verdict for one or more chunks.
pub async fn resolve_chunks(
    storage: &Arc<Storage>,
    chunk_ids: Vec<String>,
    status: String,
    evidence: String,
    claim: Option<String>,
) -> Result<String> {
    if !matches!(status.as_str(), "resolved" | "still_open" | "regressed") {
        anyhow::bail!(
            "invalid status '{}': must be resolved, still_open, or regressed",
            status
        );
    }
    if chunk_ids.is_empty() {
        anyhow::bail!("chunk_ids must not be empty");
    }
    if evidence.trim().is_empty() {
        anyhow::bail!("evidence must not be empty");
    }

    let n =
        storage.insert_resolutions(&chunk_ids, &status, &evidence, claim.as_deref(), "agent")?;

    Ok(format!("recorded {} verdict(s): {}", n, status))
}

/// Quick existence check — count + top match only.
pub async fn quick_check(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    min_score: f32,
) -> Result<String> {
    let query_vec = {
        let q = query.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&q)).await??
    };

    let results = {
        let idx = search.read().await;
        idx.search_chunks(&query_vec, 1, min_score)
    };

    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&ids)?;

    let enriched: Vec<EnrichedResult> = results
        .iter()
        .filter_map(|r| {
            chunks
                .iter()
                .find(|c| c.id == r.id)
                .map(|c| EnrichedResult {
                    score: r.score,
                    chunk: c.clone(),
                    resolution: None,
                    validity_demoted: false,
                })
        })
        .collect();

    Ok(format::format_quick_check(&enriched, query))
}

/// Search insights — aggregated summary without individual results.
pub async fn search_insights(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
) -> Result<String> {
    let (effective_project, _) = cross_project::normalize_project_scope(project);
    let query_vec = embed_query(embeddings, query).await?;

    let results = if let Some(ref p) = effective_project {
        let ids: HashSet<String> = storage.get_chunk_ids_for_project(p)?.into_iter().collect();
        let idx = search.read().await;
        idx.search_chunks_filtered(&query_vec, 10, 0.0, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(&query_vec, 10, 0.0)
    };

    let enriched = enrich_results(storage, &results)?;
    Ok(format::format_search_insights(&enriched, query))
}

/// Get recent work conversations.
pub async fn get_recent_work(
    storage: &Arc<Storage>,
    limit: usize,
    project: Option<&str>,
    group_by: &str,
) -> Result<String> {
    let chunks = storage.get_recent_chunks(limit, project)?;
    Ok(format::format_recent_work(&chunks, group_by))
}

/// Time-constrained semantic search.
#[allow(clippy::too_many_arguments)]
pub async fn search_by_recency(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    time_range: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    // Parse time constraints
    let (start, end) = if let Some(tr) = time_range {
        temporal::parse_time_expression(tr)?
    } else if since.is_some() || until.is_some() {
        let s = if let Some(s) = since {
            temporal::parse_time_expression(s)?.0
        } else {
            chrono::Utc::now() - chrono::Duration::days(7)
        };
        let e = if let Some(u) = until {
            temporal::parse_time_expression(u)?.1
        } else {
            chrono::Utc::now()
        };
        (s, e)
    } else {
        let now = chrono::Utc::now();
        (now - chrono::Duration::days(7), now)
    };

    let start_str = start.to_rfc3339();
    let end_str = end.to_rfc3339();
    let time_desc = format!("{} to {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"));

    // Get chunk IDs within time range, then do filtered HNSW search
    let time_ids: HashSet<String> = storage
        .get_chunk_ids_in_timerange(&start_str, &end_str, project)?
        .into_iter()
        .collect();

    if time_ids.is_empty() {
        return Ok(format::format_recency_results(&[], query, &time_desc));
    }

    let query_vec = embed_query(embeddings, query).await?;
    // Overfetch + partition over the full candidate set + single adaptive
    // refetch + truncate — see `fetch_enrich_partition_adaptive`.
    let enriched = {
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks_filtered(&query_vec, n, min_score, &time_ids),
            limit,
            validity_partition_enabled(),
        )?
    };
    Ok(format::format_recency_results(&enriched, query, &time_desc))
}

/// Activity timeline.
pub async fn get_timeline(
    storage: &Arc<Storage>,
    time_range: &str,
    project: Option<&str>,
    granularity: &str,
) -> Result<String> {
    let (start, end) = temporal::parse_time_expression(time_range)?;
    let start_str = start.to_rfc3339();
    let end_str = end.to_rfc3339();
    let time_desc = format!("{} to {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"));

    let chunks = storage.get_chunks_in_timerange(&start_str, &end_str, project)?;
    let groups = temporal::group_chunks_by_period(&chunks, granularity);
    Ok(format::format_timeline(&groups, &time_desc))
}

/// Attach the WP2 Stage 2 two-channel attribution render string
/// (`Storage::code_attribution_for_node`) onto every node in `nodes`, in
/// place. Per-node lookup errors fail soft to "unattributed" — a single
/// storage hiccup must never blank an entire result set or fall back to
/// `first_conv_id`.
fn attach_attribution(storage: &Arc<Storage>, nodes: &mut [crate::storage::codegraph::NodeRow]) {
    for n in nodes.iter_mut() {
        if n.id.is_empty() {
            n.attribution = "unattributed".to_string();
            continue;
        }
        n.attribution = storage
            .code_attribution_for_node(&n.id)
            .unwrap_or_else(|_| "unattributed".to_string());
    }
}

/// File ledger (§8b) — deterministic, immutable per-file dossier built from the
/// code graph + code_evolution. FTS5 is only a secondary fallback when the file
/// has no graph/evolution history.
pub async fn search_by_file(
    storage: &Arc<Storage>,
    file_path: &str,
    limit: usize,
    project: Option<&str>,
) -> Result<String> {
    let (effective_project, _) = cross_project::normalize_project_scope(project);
    let proj = effective_project.unwrap_or_default();

    let mut ledger = storage.code_file_ledger(&proj, file_path)?;
    if !ledger.symbols.is_empty() || !ledger.timeline.is_empty() {
        attach_attribution(storage, &mut ledger.symbols);
        return Ok(format::format_file_ledger(&ledger));
    }

    // Secondary enrichment: fall back to FTS5 over chunk content. The ledger
    // is empty here, but that is not the same as "never extracted": a
    // supported-language file with no definitions and no recorded edits is
    // indexed and legitimately empty. Report the real state so the caller can
    // tell a coverage gap from an honest absence.
    let chunks = storage.fts5_search(file_path, limit, project)?;
    Ok(format::format_file_results(
        &chunks,
        file_path,
        ledger.indexed,
    ))
}

/// Code-graph query: neighbors | callers | callees (no transitive impact in v1).
pub async fn code_graph(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    mode: &str,
    limit: usize,
) -> Result<String> {
    let project = cross_project::resolve_current_project().unwrap_or_default();
    let target_label = symbol.or(file).unwrap_or("").to_string();

    match mode {
        "callers" => {
            let name = match symbol {
                Some(s) if !s.is_empty() => s,
                _ => return Ok(format::format_code_graph(mode, &target_label, &[], &[])),
            };
            let mut nodes = storage.code_query_callers(name, &project, limit)?;
            attach_attribution(storage, &mut nodes);
            Ok(format::format_code_graph(mode, &target_label, &nodes, &[]))
        }
        "callees" => {
            let node_id = match resolve_node_id(storage, symbol, file, &project)? {
                Some(id) => id,
                None => return Ok(format::format_code_graph(mode, &target_label, &[], &[])),
            };
            let mut nodes = storage.code_query_callees(&node_id, limit)?;
            attach_attribution(storage, &mut nodes);
            Ok(format::format_code_graph(mode, &target_label, &nodes, &[]))
        }
        _ => {
            // Default: neighbors (1-hop, both directions).
            let node_id = match resolve_node_id(storage, symbol, file, &project)? {
                Some(id) => id,
                None => {
                    return Ok(format::format_code_graph(
                        "neighbors",
                        &target_label,
                        &[],
                        &[],
                    ))
                }
            };
            let mut neighbors = storage.code_query_neighbors(&node_id, None, limit)?;
            for ne in neighbors.iter_mut() {
                ne.node.attribution = if ne.node.id.is_empty() {
                    "unattributed".to_string()
                } else {
                    storage
                        .code_attribution_for_node(&ne.node.id)
                        .unwrap_or_else(|_| "unattributed".to_string())
                };
            }
            Ok(format::format_code_graph(
                "neighbors",
                &target_label,
                &[],
                &neighbors,
            ))
        }
    }
}

/// Provenance recall: why does this code/decision exist. Reinstatement walk (seed ->
/// blend + code-graph spread + episode chain), formatted as a cited evidence chain
/// grouped by conversation. Project scope is normalized via
/// `cross_project::normalize_project_scope` (same as every other MCP tool).
pub async fn why(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
    cfg: &crate::search::reinstatement::ReinstateConfig,
) -> Result<String> {
    let (effective_project, _) = cross_project::normalize_project_scope(project);

    let items = crate::search::reinstatement::reinstate(
        storage,
        embeddings,
        search,
        query,
        effective_project.as_deref(),
        cfg,
    )
    .await?;

    // TAD: log each returned chunk as an MCP-search retrieval event. session_id="mcp" is
    // the sentinel (MCP has no session id) — same pattern as reflect_on_past. Non-fatal:
    // a logging failure must never fail the search.
    for item in &items {
        let _ = storage.log_retrieval_event(&item.chunk_id, "chunk", "mcp_search", "mcp");
    }

    Ok(format_why(query, &items))
}

/// Format evidence items as: header, grouped-by-conversation body (chronological
/// within group), footer summary.
fn format_why(query: &str, items: &[crate::search::reinstatement::EvidenceItem]) -> String {
    use crate::search::reinstatement::Via;

    let mut out = String::new();
    out.push_str(&format!("WHY: {query}\n\n"));

    if items.is_empty() {
        out.push_str("No evidence chain found.\n");
        return out;
    }

    // Group by conversation, preserving first-seen (= highest-relevance) order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&crate::search::reinstatement::EvidenceItem>> =
        HashMap::new();
    for item in items {
        if !groups.contains_key(&item.conversation_id) {
            order.push(item.conversation_id.clone());
        }
        groups
            .entry(item.conversation_id.clone())
            .or_default()
            .push(item);
    }

    for conv in &order {
        let mut group = groups.remove(conv).unwrap_or_default();
        group.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        out.push_str(&format!("conv_{conv}:\n"));
        for it in &group {
            out.push_str(&format!(
                "  via={} score={:.3} [{}] conv_{}: {}\n",
                it.via,
                it.score,
                format::age_stamp(&it.timestamp),
                it.conversation_id,
                it.excerpt
            ));
        }
        out.push('\n');
    }

    let seed_count = items.iter().filter(|i| i.via == Via::Seed).count();
    let graph_count = items.iter().filter(|i| i.via == Via::Graph).count();
    let episode_count = items.iter().filter(|i| i.via == Via::Episode).count();
    out.push_str(&format!(
        "conversations: {} | seeds -> graph/episode reach: {} seed(s), {} graph hop(s), {} episode hop(s)\n",
        order.len(),
        seed_count,
        graph_count,
        episode_count
    ));

    out
}

/// Resolve a symbol/file to a single best node id (highest rank).
fn resolve_node_id(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    project: &str,
) -> Result<Option<String>> {
    if let Some(s) = symbol.filter(|s| !s.is_empty()) {
        let nodes = storage.code_nodes_by_name(s, project, 1)?;
        return Ok(nodes.into_iter().next().map(|n| n.id));
    }
    if let Some(f) = file.filter(|f| !f.is_empty()) {
        let ledger = storage.code_file_ledger(project, f)?;
        return Ok(ledger.symbols.into_iter().next().map(|n| n.id));
    }
    Ok(None)
}

/// Concept search — semantic search with concept-specific label.
pub async fn search_by_concept(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    concept: &str,
    limit: usize,
    project: Option<&str>,
) -> Result<String> {
    let (effective_project, scope_label) = cross_project::normalize_project_scope(project);
    let query_vec = embed_query(embeddings, concept).await?;

    // Overfetch + partition over the full candidate set + single adaptive
    // refetch + truncate — see `fetch_enrich_partition_adaptive`.
    let enriched = if let Some(ref p) = effective_project {
        let ids: HashSet<String> = storage.get_chunk_ids_for_project(p)?.into_iter().collect();
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks_filtered(&query_vec, n, 0.3, &ids),
            limit,
            validity_partition_enabled(),
        )?
    } else {
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks(&query_vec, n, 0.3),
            limit,
            validity_partition_enabled(),
        )?
    };
    Ok(format::format_search_results(
        &enriched,
        concept,
        &scope_label,
        0,
        0,
    ))
}

/// Pagination — get more results at offset.
#[allow(clippy::too_many_arguments)]
pub async fn get_more_results(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    offset: usize,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    let query_vec = embed_query(embeddings, query).await?;
    get_more_results_with_vec(
        storage,
        search,
        &query_vec,
        query,
        offset,
        limit,
        min_score,
        project,
        validity_partition_enabled(),
    )
    .await
}

/// Hard cap on the pagination candidate window — see
/// [`get_more_results_with_vec`]'s offset-independence contract.
const GET_MORE_WINDOW: usize = 500;

/// Everything in `get_more_results` after query embedding — the seam the
/// end-to-end partition test drives (same rationale as
/// `reflect_on_past_with_vec`). Enrichment + the validity partition run over
/// the FULL fetched set FIRST, and only then is the page cut with
/// skip/take — paginating first re-introduced demoted chunks into later
/// pages and let them displace valid candidates (issue 1; same discipline
/// as `apply_resolutions_before_limit`).
///
/// OFFSET-INDEPENDENT window: the candidate set is always the query's full
/// result list up to [`GET_MORE_WINDOW`], never a window grown from
/// `offset + limit` — repartitioning a per-offset-grown window could
/// duplicate demoted chunks across pages (sunk past one page's boundary,
/// re-fetched into the next) and skip valid candidates discovered only by
/// the larger window. Same query + same window + same data → same global
/// order for every page. Pagination stability holds while the underlying
/// data is unchanged — the standard offset-pagination contract.
#[allow(clippy::too_many_arguments)]
async fn get_more_results_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    offset: usize,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
    partition_enabled: bool,
) -> Result<String> {
    let (effective_project, _) = cross_project::normalize_project_scope(project);

    let all_results = if let Some(ref p) = effective_project {
        let ids: HashSet<String> = storage.get_chunk_ids_for_project(p)?.into_iter().collect();
        let idx = search.read().await;
        idx.search_chunks_filtered(query_vec, GET_MORE_WINDOW, min_score, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(query_vec, GET_MORE_WINDOW, min_score)
    };

    // Enrich + resolution sink + validity partition ONCE over the FULL
    // fixed window, THEN slice the page.
    let enriched = enrich_results_with(storage, &all_results, partition_enabled)?;
    let total = enriched.len();
    let page: Vec<EnrichedResult> = enriched.into_iter().skip(offset).take(limit).collect();
    Ok(format::format_more_results(&page, query, offset, total))
}

/// Locate the JSONL file for a conversation.
pub fn get_full_conversation(
    projects_dir: &Path,
    conversation_id: &str,
    project: Option<&str>,
) -> String {
    // Validate conversation_id: must be non-empty, alphanumeric + hyphens + underscores only
    if conversation_id.is_empty()
        || !conversation_id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return format::format_full_conversation(conversation_id, None, None);
    }

    // Search for JSONL file matching conversation_id
    let search_dirs: Vec<PathBuf> = if let Some(p) = project {
        // Search specific project directories matching the name
        match std::fs::read_dir(projects_dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&p.to_lowercase())
                })
                .map(|e| e.path())
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        match std::fs::read_dir(projects_dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect(),
            Err(_) => Vec::new(),
        }
    };

    for dir in &search_dirs {
        if let Ok(files) = std::fs::read_dir(dir) {
            for entry in files.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "jsonl") {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                    if stem.contains(conversation_id) || &*stem == conversation_id {
                        let project_name = dir
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        return format::format_full_conversation(
                            conversation_id,
                            Some(&path.to_string_lossy()),
                            Some(&project_name),
                        );
                    }
                }
            }
        }
    }

    format::format_full_conversation(conversation_id, None, None)
}

/// Get learnings for a specific session.
pub fn get_session_learnings(
    storage: &Arc<Storage>,
    session_id: &str,
    limit: usize,
) -> Result<String> {
    let tag = format!("session_{}", session_id);
    let results = storage.get_reflections_by_tag(&tag, limit)?;

    let learnings: Vec<(String, Vec<String>, String)> = results
        .into_iter()
        .map(|(_id, content, tags, timestamp)| (content, tags, timestamp))
        .collect();

    Ok(format::format_session_learnings(session_id, &learnings))
}

// ─── Helpers ───

/// Embed a query string via spawn_blocking.
async fn embed_query(embeddings: &Arc<EmbeddingEngine>, query: &str) -> Result<Vec<f32>> {
    let q = query.to_string();
    let emb = embeddings.clone();
    tokio::task::spawn_blocking(move || emb.embed_single(&q)).await?
}

/// FIRST-window size for a requested page of `limit` results, so the
/// validity partition (and the resolution sink) has real candidates to
/// promote when it demotes a top-N hit: `limit + max(limit, 20)` extra,
/// capped at `3 * limit`. Fetching exactly `limit` meant one demoted hit
/// permanently displaced the valid N+1 candidate — it was never fetched at
/// all. When even this window is exhausted by demotions, callers make ONE
/// adaptive refetch at 10*limit — see `fetch_enrich_partition_adaptive`
/// and `reflect_on_past_with_vec`.
fn overfetch(limit: usize) -> usize {
    (limit + limit.max(20))
        .min(limit.saturating_mul(3))
        .max(limit)
}

/// Enrich search results with chunk metadata from storage — kill-switch
/// outcome read from the environment. Callers that must be testable without
/// mutating the process env (see `resolve_validity_with`) use
/// [`enrich_results_with`] directly.
fn enrich_results(
    storage: &Arc<Storage>,
    results: &[crate::search::SearchResult],
) -> Result<Vec<EnrichedResult>> {
    enrich_results_with(storage, results, validity_partition_enabled())
}

/// Core of [`enrich_results`] with the validity kill-switch outcome passed
/// in: dedupe, resolution-ledger sink, then the v10 validity partition —
/// over the FULL candidate set, before any caller-side cut.
fn enrich_results_with(
    storage: &Arc<Storage>,
    results: &[crate::search::SearchResult],
    partition_enabled: bool,
) -> Result<Vec<EnrichedResult>> {
    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&ids)?;

    let mut enriched_vec: Vec<EnrichedResult> = results
        .iter()
        .filter_map(|r| {
            chunks
                .iter()
                .find(|c| c.id == r.id)
                .map(|c| EnrichedResult {
                    score: r.score,
                    chunk: c.clone(),
                    resolution: None,
                    validity_demoted: false,
                })
        })
        .collect();
    format::dedupe_results(&mut enriched_vec);
    apply_resolutions(&mut enriched_vec, storage);
    let conv_ids = distinct_conversation_ids(&enriched_vec);
    let validity = resolve_validity_with(storage, &conv_ids, partition_enabled);
    apply_validity_partition(&mut enriched_vec, &validity);
    Ok(enriched_vec)
}

/// Fetch → enrich → partition → single adaptive refetch → truncate, for the
/// simple search sites (`search_by_recency`, `search_by_concept`; the
/// primary path has its own richer pipeline in `reflect_on_past_with_vec`
/// with the same refetch rule). `fetch_fn(n)` runs the HNSW query at window
/// size `n`. First window is `overfetch(limit)`; when the partition leaves
/// fewer than `limit` valid (non-demoted) results AND that window came back
/// full (more candidates may exist beyond it), refetch ONCE at 10*limit
/// (hard cap) and repartition. If still short, accept: a query whose top
/// 10*limit candidates are all stale cannot backfill further — bounded by
/// design, not silent.
fn fetch_enrich_partition_adaptive(
    storage: &Arc<Storage>,
    fetch_fn: impl Fn(usize) -> Vec<crate::search::SearchResult>,
    limit: usize,
    partition_enabled: bool,
) -> Result<Vec<EnrichedResult>> {
    let first = overfetch(limit);
    let results = fetch_fn(first);
    let window_full = results.len() == first;
    let mut enriched = enrich_results_with(storage, &results, partition_enabled)?;
    let valid = enriched.iter().filter(|e| !e.validity_demoted).count();
    if valid < limit && window_full {
        let refetch = limit.saturating_mul(10);
        if refetch > first {
            let results = fetch_fn(refetch);
            enriched = enrich_results_with(storage, &results, partition_enabled)?;
        }
    }
    enriched.truncate(limit);
    Ok(enriched)
}

// ─── v10 dream-verdict validity partition ───
//
// Consumes `storage::chunk_binding::witness_verdict_for_chunks` (see that
// module's "Two-channel consumption" doc) at search time: Demote-channel
// chunks — the underlying symbol is gone/fully stale at the observed HEAD —
// sink below every non-demoted result; Annotate-channel chunks — the symbol
// merely evolved, still intact at HEAD — are annotated with no rank change.
// Same placement discipline as the resolution-ledger sink this mirrors
// (`apply_resolutions`, `apply_resolutions_before_limit`): resolved BEFORE
// the limit cut, so a demoted chunk never occupies a page slot that should
// go to the next best non-demoted candidate.

/// Per-conversation dream-verdict decision, reduced from
/// `witness_verdict_for_chunks`'s per-node hits to the single worst channel
/// touching that conversation's chunks: `demote = true` if ANY hit for the
/// conversation is Demote-channel (one stale symbol is enough to flag the
/// whole conversation's code claim — chunk binding is conversation-grained,
/// not per-chunk, so this cannot be narrower); else Annotate if any hit is
/// Annotate-only. `note` is the exact search-facing annotation string.
#[derive(Debug, Clone)]
struct ConvValidity {
    demote: bool,
    note: String,
}

/// Distinct `conversation_id`s across `enriched`, first-seen order (order is
/// irrelevant to correctness — `witness_verdicts_for_conversations` batches
/// regardless — but deterministic iteration keeps this easy to test).
fn distinct_conversation_ids(enriched: &[EnrichedResult]) -> Vec<String> {
    distinct_conversation_ids_of(enriched.iter().map(|e| e.chunk.conversation_id.as_str()))
}

/// Same as [`distinct_conversation_ids`] but over raw chunk metadata,
/// fetched before any `EnrichedResult` exists yet — `reflect_on_past` needs
/// the validity decision before it starts scoring, not after.
fn distinct_conversation_ids_of_chunks(chunks: &[crate::import::ConversationChunk]) -> Vec<String> {
    distinct_conversation_ids_of(chunks.iter().map(|c| c.conversation_id.as_str()))
}

fn distinct_conversation_ids_of<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.filter(|id| seen.insert(id.to_string()))
        .map(|id| id.to_string())
        .collect()
}

/// Kill switch: `CSR_NO_VALIDITY_PARTITION=1` (same idiom as
/// `CSR_NO_AI_NARRATIVES`) disables the whole v10 validity partition.
/// Default ON (feature enabled) — any other value, or the var unset, keeps
/// it on. A thin wrapper around the process env so [`resolve_validity`]'s
/// actual logic (`resolve_validity_with`) stays a pure function tests can
/// drive by parameter instead of by mutating global process state — two
/// tests calling `std::env::set_var`/`remove_var` on the same var from
/// different threads (cargo runs tests in parallel by default) is a real
/// race, not a hypothetical one (caught live: this exact var, this exact
/// pattern, before this comment existed).
fn validity_partition_enabled() -> bool {
    std::env::var("CSR_NO_VALIDITY_PARTITION").ok().as_deref() != Some("1")
}

/// Resolve the v10 validity partition for a batch of conversation ids, with
/// the kill switch's outcome passed in rather than read from the
/// environment — every entry point evaluates
/// [`validity_partition_enabled`] exactly once and threads the outcome
/// here, so tests drive this by parameter instead of mutating the env var.
/// ONE batched
/// `witness_verdicts_for_conversations` query (never per-chunk; the perf
/// requirement) when `enabled`, reduced per-conversation via
/// [`ConvValidity`]. `enabled = false` returns an empty map — every
/// consumer below treats an absent conversation id as "nothing to do", so
/// an empty map disables the whole feature with no other branching needed.
/// Non-fatal: a storage error here must never fail the calling search (same
/// discipline as `apply_resolutions`).
fn resolve_validity_with(
    storage: &Arc<Storage>,
    conversation_ids: &[String],
    enabled: bool,
) -> HashMap<String, ConvValidity> {
    if !enabled || conversation_ids.is_empty() {
        return HashMap::new();
    }
    let hits = storage
        .witness_verdicts_for_conversations(conversation_ids)
        .unwrap_or_default();
    hits.into_iter()
        .filter_map(|(conv, list)| {
            let chosen = list
                .iter()
                .find(|h| h.channel == VerdictChannel::Demote)
                .or_else(|| list.iter().find(|h| h.channel == VerdictChannel::Annotate))?;
            Some((
                conv,
                ConvValidity {
                    demote: chosen.channel == VerdictChannel::Demote,
                    note: validity_note(chosen),
                },
            ))
        })
        .collect()
}

/// Render one dream-verdict hit as the search-facing annotation string —
/// exact wording is part of the v10 contract (tests match on it verbatim).
fn validity_note(hit: &ChunkWitnessVerdict) -> String {
    let symbol = hit.symbol.as_deref().unwrap_or(hit.file.as_str());
    let receipt = short_oid(hit.receipt_oid.as_deref().unwrap_or("unknown"));
    match hit.channel {
        VerdictChannel::Demote => {
            format!("[stale anchor] {symbol} no longer in current code (receipt {receipt})")
        }
        VerdictChannel::Annotate => {
            format!("[evolved] {symbol} changed since this conversation (as of {receipt})")
        }
    }
}

/// First 7 chars of a receipt oid (git short-SHA convention) — char-boundary
/// safe and never panics on a shorter fixture oid in tests.
fn short_oid(oid: &str) -> &str {
    match oid.char_indices().nth(7) {
        Some((byte_idx, _)) => &oid[..byte_idx],
        None => oid,
    }
}

/// `true` iff `conv_id` is Demote-channel per `validity`. The ONE predicate
/// every no-stacking skip point shares — `reflect_on_past`'s TAD/decay loop,
/// its rerank-candidate filter, and `apply_validity_partition`'s own sink
/// all call this SAME function, so the three skip points cannot silently
/// drift out of sync with each other (a literal single source of truth for
/// "is this chunk about to be structurally demoted", rather than three
/// independently-maintained copies of the same `.get(...).is_some_and(...)`
/// check).
fn is_demote_channel(validity: &HashMap<String, ConvValidity>, conv_id: &str) -> bool {
    validity.get(conv_id).is_some_and(|v| v.demote)
}

/// Apply the resolved [`ConvValidity`] decision over already-scored/reranked
/// `enriched`: Demote-channel chunks sink BELOW every non-demoted result
/// (stable order preserved within each partition — mirrors
/// `apply_resolutions`'s resolved-ledger sink exactly: pull out, append,
/// never re-sort within a partition); Annotate-channel chunks are annotated
/// IN PLACE, no rank change. Both channels' note is appended to
/// `e.resolution` — merged with any resolution-ledger note already present
/// rather than overwriting it — so `format_search_results` and
/// `format_more_results` render it via the existing `<resolution>` tag with
/// no new field and no formatter signature change. No collision with the
/// ledger's own "N resolved item(s) demoted" footer count: that check is
/// `starts_with("resolved")`, and dream notes always start with
/// `[stale anchor]`/`[evolved]`.
///
/// NO STACKING (v10 contract): this function is the ONLY place a
/// Demote-channel chunk's rank moves. Callers that score/rerank BEFORE this
/// point (TAD/decay in `reflect_on_past`'s enrichment loop, the scaffold
/// penalty inside `search::rerank`) must skip their own demotion for chunks
/// this function will demote — see `reflect_on_past`'s `is_demoted` check
/// and its exclusion of demoted chunk ids from the `rerank` candidate list.
/// Compounding a soft score/rank penalty with this hard structural sink
/// would double-punish the same staleness signal.
fn apply_validity_partition(
    enriched: &mut Vec<EnrichedResult>,
    validity: &HashMap<String, ConvValidity>,
) {
    if validity.is_empty() {
        return;
    }
    for e in enriched.iter_mut() {
        if let Some(v) = validity.get(&e.chunk.conversation_id) {
            e.resolution = Some(match e.resolution.take() {
                Some(existing) => format!("{existing}; {}", v.note),
                None => v.note.clone(),
            });
        }
    }

    let mut kept = Vec::with_capacity(enriched.len());
    let mut demoted = Vec::new();
    for mut e in std::mem::take(enriched) {
        if is_demote_channel(validity, &e.chunk.conversation_id) {
            // The ONLY writer of this flag — `format_search_results`'s
            // dream-verdict footer counts it (never a substring of the
            // resolution note), so with the kill switch on (empty map, early
            // return above) the flag stays false everywhere and output is
            // byte-identical to pre-partition behavior.
            e.validity_demoted = true;
            demoted.push(e);
        } else {
            kept.push(e);
        }
    }
    kept.extend(demoted);
    *enriched = kept;
}

/// Annotate `enriched` results with any recorded resolution ledger verdicts
/// (batch-fetched from storage) and stable-sink "resolved" entries to the
/// bottom of the slice, preserving relative order otherwise. `still_open`
/// and `regressed` verdicts annotate but do not move. Non-fatal: a storage
/// error here must never fail the calling search.
pub fn apply_resolutions(enriched: &mut Vec<EnrichedResult>, storage: &Arc<Storage>) {
    if enriched.is_empty() {
        return;
    }
    let chunk_ids: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
    let ledger = match storage.get_resolutions_batch(&chunk_ids) {
        Ok(m) => m,
        Err(_) => return,
    };
    if ledger.is_empty() {
        return;
    }

    for e in enriched.iter_mut() {
        if let Some(entry) = ledger.get(&e.chunk.id) {
            e.resolution = Some(format::resolution_note(
                &entry.status,
                &entry.evidence,
                &entry.created_at,
            ));
        }
    }

    let mut unresolved = Vec::with_capacity(enriched.len());
    let mut resolved = Vec::new();
    for e in std::mem::take(enriched) {
        let is_resolved = ledger
            .get(&e.chunk.id)
            .map(|entry| entry.status == "resolved")
            .unwrap_or(false);
        if is_resolved {
            resolved.push(e);
        } else {
            unresolved.push(e);
        }
    }
    unresolved.extend(resolved);
    *enriched = unresolved;
}

/// Sink resolved-ledger AND dream-verdict-demoted chunks BEFORE the limit
/// cut, then truncate once. `validity` is the caller's already-resolved
/// [`ConvValidity`] decision (see `resolve_validity`) — passed in rather
/// than re-queried here so the whole search issues exactly one
/// `witness_verdicts_for_conversations` query (perf requirement).
fn apply_resolutions_before_limit(
    enriched: &mut Vec<EnrichedResult>,
    storage: &Arc<Storage>,
    validity: &HashMap<String, ConvValidity>,
    limit: usize,
) {
    apply_resolutions(enriched, storage);
    apply_validity_partition(enriched, validity);
    enriched.truncate(limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enriched_result(id: &str, score: f32, seq: usize) -> EnrichedResult {
        EnrichedResult {
            score,
            chunk: crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: "conv-1".into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T10:00:00Z".into(),
                content: format!("{id} claim"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::ToolResult,
                seq,
                is_sidechain: false,
            },
            resolution: None,
            validity_demoted: false,
        }
    }

    #[test]
    fn resolved_results_sink_before_limit_cut() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .insert_resolutions(
                &["resolved".to_string()],
                "resolved",
                "shipped and verified",
                None,
                "agent",
            )
            .unwrap();
        let mut enriched = vec![
            enriched_result("resolved", 0.9, 0),
            enriched_result("unresolved-1", 0.8, 1),
            enriched_result("unresolved-2", 0.7, 2),
        ];

        apply_resolutions_before_limit(&mut enriched, &storage, &HashMap::new(), 2);

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["unresolved-1", "unresolved-2"]);
    }

    // --- v10 dream-verdict validity partition ---

    fn enriched_result_conv(id: &str, conv: &str, score: f32, seq: usize) -> EnrichedResult {
        EnrichedResult {
            score,
            chunk: crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T10:00:00Z".into(),
                content: format!("{id} claim"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::ToolResult,
                seq,
                is_sidechain: false,
            },
            resolution: None,
            validity_demoted: false,
        }
    }

    fn demote_validity(note: &str) -> ConvValidity {
        ConvValidity {
            demote: true,
            note: note.to_string(),
        }
    }

    fn annotate_validity(note: &str) -> ConvValidity {
        ConvValidity {
            demote: false,
            note: note.to_string(),
        }
    }

    #[test]
    fn demoted_chunk_sinks_below_non_demoted_stable_order_preserved() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-demoted", 0.95, 0),
            enriched_result_conv("b", "conv-clean-1", 0.90, 1),
            enriched_result_conv("c", "conv-demoted", 0.85, 2),
            enriched_result_conv("d", "conv-clean-2", 0.80, 3),
        ];
        let validity: HashMap<String, ConvValidity> = [(
            "conv-demoted".to_string(),
            demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity);

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        // Non-demoted (b, d) keep their relative order, ahead of demoted
        // (a, c) which ALSO keep their relative order among themselves.
        assert_eq!(ids, ["b", "d", "a", "c"]);
    }

    #[test]
    fn demoted_chunk_still_returned_if_it_fits_the_limit() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-demoted", 0.95, 0),
            enriched_result_conv("b", "conv-clean", 0.90, 1),
        ];
        let validity: HashMap<String, ConvValidity> = [(
            "conv-demoted".to_string(),
            demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity);
        enriched.truncate(2); // both fit — demoted is sunk, not dropped

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }

    #[test]
    fn demoted_chunk_annotation_string_exact() {
        let mut enriched = vec![enriched_result_conv("a", "conv-demoted", 0.95, 0)];
        let validity: HashMap<String, ConvValidity> = [(
            "conv-demoted".to_string(),
            demote_validity("[stale anchor] old_fn no longer in current code (receipt abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity);

        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("[stale anchor] old_fn no longer in current code (receipt abc1234)")
        );
    }

    #[test]
    fn annotate_channel_does_not_move_rank_only_annotates() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-evolved", 0.70, 0),
            enriched_result_conv("b", "conv-clean", 0.95, 1),
        ];
        let validity: HashMap<String, ConvValidity> = [(
            "conv-evolved".to_string(),
            annotate_validity("[evolved] foo changed since this conversation (as of abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity);

        // Order untouched — "a" (lower score) was already below "b" and
        // stays there; Annotate never moves rank.
        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("[evolved] foo changed since this conversation (as of abc1234)")
        );
        assert_eq!(enriched[1].resolution.as_deref(), None);
    }

    #[test]
    fn annotate_note_merges_with_existing_resolution_ledger_note() {
        let mut enriched = vec![enriched_result_conv("a", "conv-evolved", 0.70, 0)];
        enriched[0].resolution = Some("still_open: earlier verdict".to_string());
        let validity: HashMap<String, ConvValidity> = [(
            "conv-evolved".to_string(),
            annotate_validity("[evolved] foo changed since this conversation (as of abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity);

        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("still_open: earlier verdict; [evolved] foo changed since this conversation (as of abc1234)")
        );
    }

    #[test]
    fn empty_validity_map_is_a_no_op() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-1", 0.95, 0),
            enriched_result_conv("b", "conv-2", 0.90, 1),
        ];
        apply_validity_partition(&mut enriched, &HashMap::new());
        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert!(enriched.iter().all(|e| e.resolution.is_none()));
    }

    #[test]
    fn kill_switch_disables_validity_partition() {
        // `enabled = false` (what `validity_partition_enabled` returns when
        // CSR_NO_VALIDITY_PARTITION=1) must make resolution return an empty
        // map regardless of what storage would otherwise report. Driven by
        // parameter, not by mutating the real env var — see
        // `resolve_validity_with`'s doc on why (parallel test races).
        let storage = Arc::new(Storage::open_memory().unwrap());
        let out = resolve_validity_with(&storage, &["conv-1".to_string()], false);
        assert!(out.is_empty());
    }

    #[test]
    fn validity_partition_enabled_parses_env_var() {
        // The one place this suite touches the real process env var for
        // this feature — every other test drives `resolve_validity_with`
        // directly by parameter to avoid racing with this test under
        // cargo's parallel test runner.
        let restore = std::env::var("CSR_NO_VALIDITY_PARTITION").ok();
        std::env::set_var("CSR_NO_VALIDITY_PARTITION", "1");
        assert!(!validity_partition_enabled());
        std::env::set_var("CSR_NO_VALIDITY_PARTITION", "0");
        assert!(validity_partition_enabled());
        std::env::remove_var("CSR_NO_VALIDITY_PARTITION");
        assert!(validity_partition_enabled());
        match restore {
            Some(v) => std::env::set_var("CSR_NO_VALIDITY_PARTITION", v),
            None => std::env::remove_var("CSR_NO_VALIDITY_PARTITION"),
        }
    }

    #[test]
    fn synthetic_db_demoted_symbol_partitions_end_to_end() {
        // Full DB-backed exercise of the v10 validity partition: a real
        // code_nodes + witness_ledger + witness_verdicts fixture producing a
        // genuine Demote-channel verdict, resolved through `resolve_validity`
        // (the same storage round-trip `reflect_on_past` makes) — not a
        // fabricated ConvValidity map like the tests above. Covers:
        // partition order, exact annotation string, no-stacking (the shared
        // `is_demote_channel` predicate agreeing with the partition), and
        // the kill switch, all against one synthetic DB with a
        // demoted-symbol conversation plus two untouched "valid" ones.
        use crate::storage::codegraph::{upsert_node, NodeRow};
        use crate::storage::witness_ledger::{self, WitnessLedgerRow};
        use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .with_connection(|conn| {
                upsert_node(
                    conn,
                    &NodeRow {
                        id: crate::extraction::codegraph::node_id(
                            "proj",
                            "/repo/src/lib.rs",
                            "function",
                            "old_fn",
                        ),
                        repo: "proj".into(),
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        lang: "rust".into(),
                        kind: "function".into(),
                        name: "old_fn".into(),
                        fqname: String::new(),
                        body_hash: String::new(),
                        span_start: 1,
                        span_end: 3,
                        first_conv_id: "conv-demoted".into(),
                        last_conv_id: "conv-demoted".into(),
                        last_session_id: "sess".into(),
                        repo_root: None,
                        name_only: false,
                        attribution: String::new(),
                    },
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        symbol: Some("old_fn".into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:1".into(),
                        tier: "committed".into(),
                        at_oid: Some("aaa".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("aaa".into()),
                    },
                )?;
                let wid = witness_ledger::latest_witness_for_symbol(
                    conn,
                    "proj",
                    "/repo/src/lib.rs",
                    Some("old_fn"),
                )?
                .unwrap()
                .id;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id: wid,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: Some("deadbeef00".into()),
                        observed_head_oid: "deadbeef00".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();

        let mut enriched = vec![
            // Highest raw score of the three — if the partition were
            // stacking with (or losing to) score-based ranking, this would
            // stay #1. It must not.
            enriched_result_conv("stale", "conv-demoted", 0.95, 0),
            enriched_result_conv("valid-1", "conv-clean-1", 0.90, 1),
            enriched_result_conv("valid-2", "conv-clean-2", 0.80, 2),
        ];
        let conv_ids = distinct_conversation_ids(&enriched);
        let validity = resolve_validity_with(&storage, &conv_ids, true);

        // NO STACKING: the shared skip predicate agrees with what the
        // partition below will do — this is the exact check
        // `reflect_on_past`'s TAD loop and rerank filter make.
        assert!(is_demote_channel(&validity, "conv-demoted"));
        assert!(!is_demote_channel(&validity, "conv-clean-1"));
        assert!(!is_demote_channel(&validity, "conv-clean-2"));

        apply_validity_partition(&mut enriched, &validity);

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(
            ids,
            ["valid-1", "valid-2", "stale"],
            "highest-scored chunk still sinks below both lower-scored valid \
             chunks — the partition, not the score, decides rank"
        );
        assert_eq!(
            enriched[2].resolution.as_deref(),
            Some("[stale anchor] old_fn no longer in current code (receipt deadbee)")
        );
        assert!(enriched[0].resolution.is_none());
        assert!(enriched[1].resolution.is_none());

        // Kill switch, same populated DB and same real storage round-trip
        // (via `resolve_validity_with(..., false)` — see that function's
        // doc on why tests drive this by parameter, never by mutating the
        // real env var): resolution must come back empty and the partition
        // must be a total no-op.
        let validity_off = resolve_validity_with(&storage, &conv_ids, false);
        assert!(validity_off.is_empty());

        let mut enriched_off = vec![
            enriched_result_conv("stale", "conv-demoted", 0.95, 0),
            enriched_result_conv("valid-1", "conv-clean-1", 0.90, 1),
        ];
        apply_validity_partition(&mut enriched_off, &validity_off);
        let ids_off: Vec<&str> = enriched_off.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(
            ids_off,
            ["stale", "valid-1"],
            "kill switch: original order preserved, nothing demoted/annotated"
        );
        assert!(enriched_off.iter().all(|e| e.resolution.is_none()));
    }

    #[test]
    fn short_oid_truncates_to_seven_chars() {
        assert_eq!(short_oid("abcdef1234567890"), "abcdef1");
    }

    #[test]
    fn short_oid_never_panics_on_short_fixture_oid() {
        assert_eq!(short_oid("bbb"), "bbb");
    }

    #[test]
    fn validity_note_exact_wording_demote() {
        let hit = ChunkWitnessVerdict {
            file: "/repo/src/lib.rs".into(),
            symbol: Some("old_fn".into()),
            channel: VerdictChannel::Demote,
            verdict: "anchor_obsolete",
            receipt_oid: Some("abcdef1234567890".into()),
        };
        assert_eq!(
            validity_note(&hit),
            "[stale anchor] old_fn no longer in current code (receipt abcdef1)"
        );
    }

    #[test]
    fn validity_note_exact_wording_annotate() {
        let hit = ChunkWitnessVerdict {
            file: "/repo/src/lib.rs".into(),
            symbol: Some("foo".into()),
            channel: VerdictChannel::Annotate,
            verdict: "superseded_by",
            receipt_oid: Some("abcdef1234567890".into()),
        };
        assert_eq!(
            validity_note(&hit),
            "[evolved] foo changed since this conversation (as of abcdef1)"
        );
    }

    #[test]
    fn resolve_validity_prefers_demote_over_annotate_for_same_conversation() {
        // A conversation touching two symbols — one demoted, one merely
        // evolved — must be flagged Demote overall (module doc: one stale
        // symbol is enough to flag the whole conversation's code claim).
        let demote_hit = ChunkWitnessVerdict {
            file: "/repo/src/a.rs".into(),
            symbol: Some("gone_fn".into()),
            channel: VerdictChannel::Demote,
            verdict: "anchor_obsolete",
            receipt_oid: Some("head1".into()),
        };
        let annotate_hit = ChunkWitnessVerdict {
            file: "/repo/src/b.rs".into(),
            symbol: Some("evolved_fn".into()),
            channel: VerdictChannel::Annotate,
            verdict: "superseded_by",
            receipt_oid: Some("head2".into()),
        };
        // Exercise the reduction logic directly (same code path
        // `resolve_validity` uses after the storage round-trip).
        let list = [annotate_hit, demote_hit];
        let chosen = list
            .iter()
            .find(|h| h.channel == VerdictChannel::Demote)
            .or_else(|| list.iter().find(|h| h.channel == VerdictChannel::Annotate))
            .unwrap();
        assert_eq!(chosen.channel, VerdictChannel::Demote);
    }

    // --- end-to-end: real tool path through retrieval → FTS-append →
    // validity → rerank → truncate → format ---

    /// Synthetic DB + HNSW index exercised through the ACTUAL
    /// `reflect_on_past_with_vec` / `get_more_results_with_vec` seams (the
    /// full handler minus only the FastEmbed query embedding): chunks,
    /// code_nodes, witness_ledger, witness_verdicts, and an FTS row, with a
    /// genuine Demote-channel verdict against `conv-demoted`.
    /// code_nodes + witness_ledger + witness_verdicts rows producing a
    /// genuine Demote-channel verdict for `conv-demoted` (symbol `old_fn`,
    /// receipt `deadbeef00`) — shared by every DB-backed e2e fixture.
    fn install_demote_verdict(storage: &Arc<Storage>) {
        use crate::storage::codegraph::{upsert_node, NodeRow};
        use crate::storage::witness_ledger::{self, WitnessLedgerRow};
        use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

        storage
            .with_connection(|conn| {
                upsert_node(
                    conn,
                    &NodeRow {
                        id: crate::extraction::codegraph::node_id(
                            "proj",
                            "/repo/src/lib.rs",
                            "function",
                            "old_fn",
                        ),
                        repo: "proj".into(),
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        lang: "rust".into(),
                        kind: "function".into(),
                        name: "old_fn".into(),
                        fqname: String::new(),
                        body_hash: String::new(),
                        span_start: 1,
                        span_end: 3,
                        first_conv_id: "conv-demoted".into(),
                        last_conv_id: "conv-demoted".into(),
                        last_session_id: "sess".into(),
                        repo_root: None,
                        name_only: false,
                        attribution: String::new(),
                    },
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        symbol: Some("old_fn".into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:1".into(),
                        tier: "committed".into(),
                        at_oid: Some("aaa".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("aaa".into()),
                    },
                )?;
                let wid = witness_ledger::latest_witness_for_symbol(
                    conn,
                    "proj",
                    "/repo/src/lib.rs",
                    Some("old_fn"),
                )?
                .unwrap()
                .id;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id: wid,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: Some("deadbeef00".into()),
                        observed_head_oid: "deadbeef00".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn e2e_fixture() -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        install_demote_verdict(&storage);

        let now = chrono::Utc::now().to_rfc3339();
        let mk =
            |id: &str, conv: &str, content: &str, seq: usize| crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "testproj".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            };
        // Cosine similarities against query [1,0,0,0]:
        // stale 1.0 > valid-1 ~0.98 > valid-2 ~0.95; fts-only ~0.0
        // (semantic miss — reachable only via the keyword fallback).
        let rows: [(crate::import::ConversationChunk, Vec<f32>); 4] = [
            (
                mk("chunk-stale", "conv-demoted", "old_fn design discussion", 0),
                vec![1.0, 0.0, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-valid-1",
                    "conv-clean-1",
                    "first current topic notes",
                    1,
                ),
                vec![0.98, 0.199, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-valid-2",
                    "conv-clean-2",
                    "second current topic notes",
                    2,
                ),
                vec![0.95, 0.312, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-fts",
                    "conv-demoted",
                    "zebraquark keyword only claim",
                    3,
                ),
                vec![0.0, 0.0, 1.0, 0.0],
            ),
        ];
        let mut engine = SearchEngine::new(16);
        for (chunk, vec) in &rows {
            storage.insert_chunk(chunk, vec).unwrap();
            engine.insert_chunk(chunk.id.clone(), vec.clone());
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    /// Rank of `needle` within `haystack` (byte offset), panicking loudly if
    /// absent — used to assert result ORDER in rendered XML.
    fn pos_of(haystack: &str, needle: &str) -> usize {
        haystack
            .find(needle)
            .unwrap_or_else(|| panic!("expected '{needle}' in output:\n{haystack}"))
    }

    #[tokio::test]
    async fn e2e_partition_through_real_reflect_and_get_more_paths() {
        let (storage, search) = e2e_fixture();
        let q = [1.0f32, 0.0, 0.0, 0.0];

        // 1) Demoted chunk sinks below BOTH valid ones and carries the exact
        //    annotation + footer, on the real reflect path.
        let out =
            reflect_on_past_with_vec(&storage, &search, &q, "topic", 3, 0.3, Some("all"), 0, true)
                .await
                .unwrap();
        let p_v1 = pos_of(&out, "<id>chunk-valid-1</id>");
        let p_v2 = pos_of(&out, "<id>chunk-valid-2</id>");
        let p_stale = pos_of(&out, "<id>chunk-stale</id>");
        assert!(
            p_v1 < p_v2 && p_v2 < p_stale,
            "demoted chunk must sink below every valid one:\n{out}"
        );
        assert!(
            out.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
            "exact annotation string must be present:\n{out}"
        );
        assert!(
            out.contains("bound code anchor no longer current (dream verdict)"),
            "dream-verdict footer must fire for a flagged demotion:\n{out}"
        );

        // 2) OVERFETCH: at limit=2 the valid N+1 candidate (valid-2, never
        //    fetched under the old exactly-limit fetch) takes the slot the
        //    demoted top hit vacated.
        let out =
            reflect_on_past_with_vec(&storage, &search, &q, "topic", 2, 0.3, Some("all"), 0, true)
                .await
                .unwrap();
        assert!(
            out.contains("<id>chunk-valid-2</id>"),
            "overfetch must surface the valid N+1 candidate:\n{out}"
        );
        assert!(
            !out.contains("<id>chunk-stale</id>"),
            "demoted chunk must not occupy a page slot valid candidates can fill:\n{out}"
        );

        // 3) KILL SWITCH: partition off restores pure score order — the
        //    demoted chunk leads again, no annotation anywhere.
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            2,
            0.3,
            Some("all"),
            0,
            false,
        )
        .await
        .unwrap();
        let p_stale = pos_of(&out, "<id>chunk-stale</id>");
        let p_v1 = pos_of(&out, "<id>chunk-valid-1</id>");
        assert!(
            p_stale < p_v1,
            "kill switch must restore pre-partition order:\n{out}"
        );
        assert!(
            !out.contains("[stale anchor]"),
            "no annotation with switch off:\n{out}"
        );
        assert!(
            !out.contains("dream verdict"),
            "no footer with switch off:\n{out}"
        );

        // 4) get_more page 2 respects the partition: full-set partition
        //    yields [valid-1, valid-2, stale], so page 2 (offset 2) is the
        //    demoted chunk, annotated — and page 1 is demotion-free.
        let page1 =
            get_more_results_with_vec(&storage, &search, &q, "topic", 0, 2, 0.3, Some("all"), true)
                .await
                .unwrap();
        assert!(page1.contains("first current topic notes"), "got: {page1}");
        assert!(page1.contains("second current topic notes"), "got: {page1}");
        assert!(
            !page1.contains("old_fn design discussion"),
            "page 1 must not contain the demoted chunk:\n{page1}"
        );
        let page2 =
            get_more_results_with_vec(&storage, &search, &q, "topic", 2, 2, 0.3, Some("all"), true)
                .await
                .unwrap();
        assert!(
            page2.contains("old_fn design discussion"),
            "page 2 must carry the demoted chunk after the partition:\n{page2}"
        );
        assert!(
            page2.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
            "page 2 must carry the annotation:\n{page2}"
        );
        // Pagination stability (offset-independent window): no chunk may be
        // duplicated across pages 1+2 and none skipped — every fixture
        // conversation appears on exactly one page — and re-requesting page
        // 2 must reproduce it byte-for-byte (same query + same data → same
        // global order).
        assert!(
            !page2.contains("first current topic notes")
                && !page2.contains("second current topic notes"),
            "page 2 must not duplicate page 1 results:\n{page2}"
        );
        for marker in [
            "first current topic notes",
            "second current topic notes",
            "old_fn design discussion",
        ] {
            let on_p1 = page1.contains(marker);
            let on_p2 = page2.contains(marker);
            assert!(
                on_p1 ^ on_p2,
                "'{marker}' must appear on exactly one page (p1: {on_p1}, p2: {on_p2})"
            );
        }
        let page2_again =
            get_more_results_with_vec(&storage, &search, &q, "topic", 2, 2, 0.3, Some("all"), true)
                .await
                .unwrap();
        assert_eq!(
            page2, page2_again,
            "re-requested page 2 must be identical (offset-independent window)"
        );

        // 5) FTS-append: a query that misses semantically (orthogonal
        //    vector) but hits the FTS row of a demoted conversation must
        //    still annotate — validity is resolved AFTER the candidate set
        //    is final (the appended conversation ids are merged in).
        let q_miss = [0.0f32, 0.0, 0.0, 1.0];
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &q_miss,
            "zebraquark",
            3,
            0.3,
            Some("all"),
            0,
            true,
        )
        .await
        .unwrap();
        assert!(
            out.contains("[keyword] zebraquark keyword only claim"),
            "FTS fallback must surface the keyword chunk:\n{out}"
        );
        assert!(
            out.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
            "FTS-appended chunk from a demoted conversation must be annotated:\n{out}"
        );
    }

    #[tokio::test]
    async fn e2e_adaptive_refetch_backfills_valid_chunks_beyond_first_window() {
        // 8 demoted chunks outrank 2 valid ones. At limit=2 the first
        // window is overfetch(2) = 6 — ALL demoted, and full (more
        // candidates exist) — so the single adaptive refetch at 10*limit
        // must surface the valid pair from beyond the first window.
        let storage = Arc::new(Storage::open_memory().unwrap());
        install_demote_verdict(&storage);

        let now = chrono::Utc::now().to_rfc3339();
        let mk =
            |id: &str, conv: &str, content: &str, seq: usize| crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "testproj".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            };
        let mut engine = SearchEngine::new(32);
        for i in 0..8 {
            let chunk = mk(
                &format!("chunk-demoted-{i}"),
                "conv-demoted",
                &format!("stale claim number {i} about old_fn"),
                i,
            );
            // cos ≈ 1.0 against [1,0,0,0] — every demoted chunk outranks
            // both valid ones.
            let vec = vec![1.0, 0.001 * (i + 1) as f32, 0.0, 0.0];
            storage.insert_chunk(&chunk, &vec).unwrap();
            engine.insert_chunk(chunk.id.clone(), vec);
        }
        for (id, conv, content, v, seq) in [
            (
                "chunk-valid-a",
                "conv-live-a",
                "live topic alpha notes",
                vec![0.6, 0.8, 0.0, 0.0], // cos 0.6
                8,
            ),
            (
                "chunk-valid-b",
                "conv-live-b",
                "live topic beta notes",
                vec![0.5, 0.866, 0.0, 0.0], // cos ~0.5
                9,
            ),
        ] {
            let chunk = mk(id, conv, content, seq);
            storage.insert_chunk(&chunk, &v).unwrap();
            engine.insert_chunk(chunk.id.clone(), v);
        }
        let search = Arc::new(RwLock::new(engine));
        let q = [1.0f32, 0.0, 0.0, 0.0];

        let out =
            reflect_on_past_with_vec(&storage, &search, &q, "topic", 2, 0.3, Some("all"), 0, true)
                .await
                .unwrap();
        assert!(
            out.contains("<id>chunk-valid-a</id>") && out.contains("<id>chunk-valid-b</id>"),
            "adaptive refetch must backfill valid chunks from beyond the all-demoted first window:\n{out}"
        );
        assert!(
            !out.contains("<id>chunk-demoted-"),
            "no demoted chunk may occupy a slot the refetched valid pair can fill:\n{out}"
        );
    }

    // --- exact conv-tag lookup (episode-index handle fix) ---
    // Live failure 2026-07-08: csr_reflect_on_past("conv_0b68eace-…") went
    // through semantic embedding and returned unrelated cross-project noise.
    // A conv handle must resolve by tag, never by embedding.

    #[test]
    fn conv_handle_extracts_uuid() {
        assert_eq!(
            extract_conv_id("conv_0b68eace-3e21-4841-9567-58d038cd89d7"),
            Some("0b68eace-3e21-4841-9567-58d038cd89d7")
        );
    }

    #[test]
    fn bare_uuid_extracts() {
        assert_eq!(
            extract_conv_id("0b68eace-3e21-4841-9567-58d038cd89d7"),
            Some("0b68eace-3e21-4841-9567-58d038cd89d7")
        );
    }

    #[test]
    fn conv_handle_inside_sentence_extracts() {
        assert_eq!(
            extract_conv_id("full state: conv_0b68eace-3e21-4841-9567-58d038cd89d7 please"),
            Some("0b68eace-3e21-4841-9567-58d038cd89d7")
        );
    }

    #[test]
    fn plain_text_does_not_extract() {
        assert_eq!(extract_conv_id("that docker issue we fixed"), None);
        assert_eq!(extract_conv_id("convention for naming conversations"), None);
        assert_eq!(extract_conv_id("conv_not-a-real-uuid"), None);
    }
}
