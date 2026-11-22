use super::*;

pub(super) fn recency_window(
    time_range: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> {
    Ok(if let Some(tr) = time_range {
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
    })
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
    validate_page(0, limit)?;
    let (start, end) = recency_window(time_range, since, until)?;

    let start_str = start.to_rfc3339();
    let end_str = end.to_rfc3339();
    // The exact bounds searched, in the header, so a window that is not the one
    // the caller meant ("last month" is the previous calendar month) is seen.
    // `recency_window` reads `time_range` first, so it alone names the window.
    let named = time_range;
    let time_desc = temporal::describe_window(named, &start, &end);

    // Get chunk IDs within time range, then do filtered HNSW search
    let time_ids: HashSet<String> = storage
        .get_chunk_ids_in_timerange(&start_str, &end_str, project)?
        .into_iter()
        .collect();

    if time_ids.is_empty() {
        let hint = page::recency_hint(named, &start, &end, &[]);
        return Ok(format::format_recency_results_hinted(
            &[],
            query,
            &time_desc,
            hint.as_deref(),
        ));
    }

    let query_vec = embed_query(embeddings, query).await?;
    // Overfetch + partition over the full candidate set + single adaptive
    // refetch + truncate — see `fetch_enrich_partition_adaptive`.
    let enriched = {
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks_filtered(&query_vec, n, min_score, &time_ids),
            query,
            limit,
            consumption_mode_for_partition(validity_partition_enabled()),
        )?
    };
    let hint = page::recency_hint(named, &start, &end, &enriched);
    Ok(format::format_recency_results_hinted(
        &enriched,
        query,
        &time_desc,
        hint.as_deref(),
    ))
}
