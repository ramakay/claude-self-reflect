use super::*;
use crate::search::identifiers::{exact_id_tokens, find_exact_id};

/// Most chunks the identifier lookup reads. An identifier is rare, so a handful
/// of chunks hold it; the cap only bounds a pasted common token.
const EXACT_FETCH: usize = 200;

/// Quick existence check — count + top match only.
pub async fn quick_check(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    let query_vec = {
        let q = query.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&q)).await??
    };

    quick_check_with_vec(storage, search, &query_vec, query, min_score, project).await
}

/// The (at most two) rows `format_quick_check` judges: the hit it may show and
/// the runner-up its margin is read against.
fn quick_check_hits(
    mut enriched: Vec<EnrichedResult>,
    storage: &Storage,
    live: Option<&str>,
    query: &str,
) -> Vec<EnrichedResult> {
    let Some(live) = live else {
        enriched.truncate(2);
        return enriched;
    };
    // `format_quick_check` judges only its first row against the floor, so the
    // floor is applied here, before the row to show is picked: the best
    // qualifying row outside the live session, else the qualifying live one.
    // When nothing qualifies the order stays best-first, so the negative
    // answer reports the best score it rejected.
    let mut ordered = enriched.clone();
    sink_session_hits(&mut ordered, storage, Some(live), query);
    let Some(at) = ordered
        .iter()
        .position(|e| e.score >= format::QUICK_CHECK_FLOOR)
    else {
        enriched.truncate(2);
        return enriched;
    };
    let shown = ordered.remove(at);
    // The runner-up the margin reads against must not outscore the row shown.
    let runner_up = ordered.into_iter().find(|e| e.score <= shown.score);
    std::iter::once(shown).chain(runner_up).collect()
}

/// The stored chunk that holds an identifier of the query literally, if any.
///
/// Similarity search cannot answer this: a long number or an id such as
/// `Bond-ZZHE87751` has no meaning to the embedding, so the nearest vector is
/// some other id-shaped text. Recall finds the right chunk because its keyword
/// lane (`read_keyword_legs` in `tools.rs`) reads the full-text index; this reads the same
/// index and then checks the text itself, so `found` can only mean the
/// identifier is in the evidence.
///
/// A row counts only if it holds an identifier as a whole token
/// ([`find_exact_id`]: not inside a longer number or id) and is not a
/// restatement of the question or CSR's own output, which hold the identifier
/// because the caller just typed it. Rows are ordered answers first, the live
/// chat after other chats, then by how many of the query's identifiers they
/// hold, then by full-text rank.
fn exact_evidence(
    storage: &Arc<Storage>,
    ids: &[String],
    project: Option<&str>,
    live: Option<&str>,
    query: &str,
) -> Result<(Option<format::ExactHit>, bool)> {
    let expr = crate::storage::queries::fts5_or_expr(ids);
    let family = project.map(|p| family_chunk_ids(storage, p)).transpose()?;
    let sink_live = live.filter(|live| !query_names_session(query, live));
    let matches = storage.fts5_search_expr(&expr, EXACT_FETCH, None)?;
    // A full window may hold more matches than were read.
    let window_full = matches.len() >= EXACT_FETCH;
    let mut rows: Vec<(EnrichedResult, Vec<String>, usize)> = matches
        .into_iter()
        .filter(|(chunk, _, _)| family.as_ref().is_none_or(|set| set.contains(&chunk.id)))
        .filter_map(|(chunk, ordinal, _)| {
            let matched: Vec<String> = ids
                .iter()
                .filter(|id| find_exact_id(&chunk.content, id).is_some())
                .cloned()
                .collect();
            let answers = !matched.is_empty() && non_answer_class(&chunk.content, query) == 0;
            answers.then(|| {
                let row = EnrichedResult {
                    score: 0.0,
                    chunk,
                    resolution: None,
                    trust: crate::provenance::TrustTier::Unknown,
                    validity_demoted: false,
                };
                (row, matched, ordinal)
            })
        })
        .collect();
    if rows.is_empty() {
        return Ok((None, window_full));
    }
    rows.sort_by_cached_key(|(row, matched, ordinal)| {
        let in_live = sink_live.is_some_and(|live| is_session_hit(storage, &row.chunk, live));
        (in_live, std::cmp::Reverse(matched.len()), *ordinal)
    });
    let literal_rows = rows.len();
    let (row, matched, _) = rows.swap_remove(0);
    // The labels the similarity path puts on a row: trust, and `[CSR text]`.
    let mut shown = vec![row];
    apply_trust_labels(&mut shown, storage);
    let labeled_chunks: Vec<_> = shown.iter().map(|e| e.chunk.clone()).collect();
    let validity = csr_text_labels_only(storage, &labeled_chunks);
    apply_validity_partition(&mut shown, &validity, false);
    let row = shown.remove(0);
    let missing = ids
        .iter()
        .filter(|id| !matched.contains(id))
        .cloned()
        .collect();
    let hit = format::ExactHit {
        row,
        matched,
        missing,
        literal_rows,
    };
    Ok((Some(hit), window_full))
}

pub(super) async fn quick_check_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    let (effective_project, _) = cross_project::normalize_project_scope(storage, project)?;
    // With a live session the window is wider than the two rows shown, so
    // its sunk hits can give their places to others; without one it is 2.
    let live = live_session_id();
    let window = if live.is_some() { overfetch(2) } else { 2 };
    let results = if let Some(ref project) = effective_project {
        let ids = family_chunk_ids(storage, project)?;
        let idx = search.read().await;
        idx.search_chunks_filtered(query_vec, window, min_score, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(query_vec, window, min_score)
    };

    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&ids)?;

    let mut enriched: Vec<EnrichedResult> = results
        .iter()
        .filter_map(|r| {
            chunks
                .iter()
                .find(|c| c.id == r.id)
                .map(|c| EnrichedResult {
                    score: r.score,
                    chunk: c.clone(),
                    resolution: None,
                    trust: crate::provenance::TrustTier::Unknown,
                    validity_demoted: false,
                })
        })
        .collect();
    apply_trust_labels(&mut enriched, storage);
    let labeled_chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();
    let validity = csr_text_labels_only(storage, &labeled_chunks);
    apply_validity_partition(&mut enriched, &validity, false);
    let enriched = quick_check_hits(enriched, storage, live.as_deref(), query);

    // A query that is, or holds, an exact identifier is answered from the text
    // that holds it. The similarity hit above is only ever shown, labelled, as
    // the nearest related chunk.
    let exact_ids = exact_id_tokens(query);
    if exact_ids.is_empty() {
        return Ok(format::format_quick_check(&enriched, query));
    }
    let (hit, window_full) = exact_evidence(
        storage,
        &exact_ids,
        effective_project.as_deref(),
        live.as_deref(),
        query,
    )?;
    Ok(match hit {
        Some(hit) => format::format_quick_check_exact(&hit),
        None => format::format_quick_check_exact_miss(&enriched, &exact_ids, window_full),
    })
}
