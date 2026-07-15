use std::collections::HashSet;
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
fn lookup_by_conv_tag(
    storage: &Arc<Storage>,
    conv_id: &str,
    query: &str,
    limit: usize,
) -> Result<Option<String>> {
    let start = Instant::now();
    let rows = storage.get_reflections_by_tag(&format!("conv_{}", conv_id), limit)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let enriched: Vec<EnrichedResult> = rows
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
            }
        })
        .collect();
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
    // Retrieval-handle fast path: `conv_<uuid>` (or a bare UUID) resolves by
    // exact tag. Falls through to semantic search only when the tag matches
    // nothing, so a stale handle still gets a best-effort answer.
    if let Some(conv_id) = extract_conv_id(query) {
        if let Some(result) = lookup_by_conv_tag(storage, conv_id, query, limit.max(5))? {
            return Ok(result);
        }
    }

    let (effective_project, scope_label) = cross_project::normalize_project_scope(project);

    let embed_start = Instant::now();
    let query_vec = embed_query(embeddings, query).await?;
    let embed_ms = embed_start.elapsed().as_millis() as u64;

    let search_start = Instant::now();

    // Search BOTH chunks and reflections, merge by score
    let (chunk_results, reflection_results) = {
        let idx = search.read().await;
        let chunks = if let Some(ref p) = effective_project {
            let ids: HashSet<String> = storage.get_chunk_ids_for_project(p)?.into_iter().collect();
            idx.search_chunks_filtered(&query_vec, limit, min_score, &ids)
        } else {
            idx.search_chunks(&query_vec, limit, min_score)
        };
        let reflections = idx.search_reflections(&query_vec, limit, min_score);
        (chunks, reflections)
    };
    let search_ms = search_start.elapsed().as_millis() as u64;

    // Enrich chunk results with metadata
    let chunk_ids: Vec<String> = chunk_results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&chunk_ids)?;

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
                let decayed_score =
                    if let Ok(ts) = c.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
                        let events = tad_events.get(&c.id).map(|v| v.as_slice()).unwrap_or(&[]);
                        decay::apply_tad(r.score, &ts, &now, events, &tad_config)
                    } else {
                        r.score
                    };
                // Cross-project multiplicative penalty
                let final_score = if let Some(ref p) = effective_project {
                    if c.project_name != *p {
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
            let final_score = if let Some(ref p) = effective_project {
                if project_name != *p {
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
            });
        }
    }

    // FTS5 hybrid fallback: if semantic results are weak (top score < 0.5)
    // or empty, supplement with keyword search results
    let semantic_top_score = enriched.iter().map(|e| e.score).fold(0.0f32, f32::max);
    if semantic_top_score < 0.5 {
        let fts_project = effective_project.as_deref();
        if let Ok(fts_chunks) = storage.fts5_search(query, limit, fts_project) {
            let existing_ids: HashSet<String> =
                enriched.iter().map(|e| e.chunk.id.clone()).collect();
            for chunk in fts_chunks {
                if existing_ids.contains(&chunk.id) {
                    continue; // already in semantic results
                }
                // FTS5 results get a synthetic score slightly below semantic threshold
                // so they rank after good semantic matches but above nothing
                let fts_score =
                    if let Ok(ts) = chunk.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
                        decay::apply_decay(0.45, &ts, &now, None, None) // base 0.45 + decay
                    } else {
                        0.40
                    };
                let final_fts_score = if let Some(ref p) = effective_project {
                    if chunk.project_name != *p {
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
                });
            }
        }
    }

    // Provenance-aware re-rank (v9.3): authority + meaning layered on the decayed
    // score. User-authored content is boosted, tool-mechanic build-log and
    // non-user authority claims are demoted — so a founding decision out-ranks the
    // [Edit:]/[Bash:] chunks that used to bury it. Falls back to score order when
    // no provenance/meaning signal differs.
    let candidates: Vec<crate::search::rerank::RankCandidate> = enriched
        .iter()
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
    enriched.truncate(limit);

    // TAD: log each returned memory as an MCP-search retrieval event. session_id="mcp" is a
    // sentinel (MCP has no session id) — distinguishable from hook-driven sessions for future
    // decay work. Non-fatal: a logging failure must never fail the search.
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
    let results = {
        let idx = search.read().await;
        idx.search_chunks_filtered(&query_vec, limit, min_score, &time_ids)
    };

    let enriched = enrich_results(storage, &results)?;
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

    let ledger = storage.code_file_ledger(&proj, file_path)?;
    if !ledger.symbols.is_empty() || !ledger.timeline.is_empty() {
        return Ok(format::format_file_ledger(&ledger));
    }

    // Secondary enrichment: fall back to FTS5 over chunk content.
    let chunks = storage.fts5_search(file_path, limit, project)?;
    Ok(format::format_file_results(&chunks, file_path))
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
            let nodes = storage.code_query_callers(name, &project, limit)?;
            Ok(format::format_code_graph(mode, &target_label, &nodes, &[]))
        }
        "callees" => {
            let node_id = match resolve_node_id(storage, symbol, file, &project)? {
                Some(id) => id,
                None => return Ok(format::format_code_graph(mode, &target_label, &[], &[])),
            };
            let nodes = storage.code_query_callees(&node_id, limit)?;
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
            let neighbors = storage.code_query_neighbors(&node_id, None, limit)?;
            Ok(format::format_code_graph(
                "neighbors",
                &target_label,
                &[],
                &neighbors,
            ))
        }
    }
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

    let results = if let Some(ref p) = effective_project {
        let ids: HashSet<String> = storage.get_chunk_ids_for_project(p)?.into_iter().collect();
        let idx = search.read().await;
        idx.search_chunks_filtered(&query_vec, limit, 0.3, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(&query_vec, limit, 0.3)
    };

    let enriched = enrich_results(storage, &results)?;
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
    let (effective_project, _) = cross_project::normalize_project_scope(project);
    let query_vec = embed_query(embeddings, query).await?;
    let fetch = offset + limit;

    let all_results = if let Some(ref p) = effective_project {
        let ids: HashSet<String> = storage.get_chunk_ids_for_project(p)?.into_iter().collect();
        let idx = search.read().await;
        idx.search_chunks_filtered(&query_vec, fetch, min_score, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(&query_vec, fetch, min_score)
    };

    let total = all_results.len();
    let page: Vec<_> = all_results.into_iter().skip(offset).take(limit).collect();
    let enriched = enrich_results(storage, &page)?;
    Ok(format::format_more_results(&enriched, query, offset, total))
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

/// Enrich search results with chunk metadata from storage.
fn enrich_results(
    storage: &Arc<Storage>,
    results: &[crate::search::SearchResult],
) -> Result<Vec<EnrichedResult>> {
    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&ids)?;

    Ok(results
        .iter()
        .filter_map(|r| {
            chunks
                .iter()
                .find(|c| c.id == r.id)
                .map(|c| EnrichedResult {
                    score: r.score,
                    chunk: c.clone(),
                })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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
