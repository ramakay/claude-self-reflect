use super::*;

#[cfg(test)]
#[path = "recency_tool_tests.rs"]
mod tests;

type Instant = chrono::DateTime<chrono::Utc>;

/// An exact moment: RFC 3339 with any offset, or a bare date and time to the
/// minute or the second, read as UTC. `None` for a date or a phrase.
fn exact_instant(expr: &str) -> Option<Instant> {
    let expr = expr.trim();
    temporal::parse_timestamp(expr).or_else(|| {
        ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
            .iter()
            .find_map(|format| chrono::NaiveDateTime::parse_from_str(expr, format).ok())
            .map(|naive| naive.and_utc())
    })
}

/// Where a `since` begins. A moment is that moment; anything else is the start
/// of the window it names.
fn since_bound(expr: &str) -> Result<Instant> {
    match exact_instant(expr) {
        Some(at) => Ok(at),
        None => Ok(temporal::parse_time_expression(expr)?.0),
    }
}

/// Where an `until` ends. A moment is that moment, to the second, never the end
/// of its day; a date or a phrase is the end of the window it names.
fn until_bound(expr: &str) -> Result<Instant> {
    match exact_instant(expr) {
        Some(at) => Ok(at),
        None => Ok(temporal::parse_time_expression(expr)?.1),
    }
}

/// The window searched. `time_range` names one; `since` and `until` narrow it,
/// so the later start and the earlier end win and no bound is dropped. Without
/// `time_range`, `since` and `until` set the bounds and the rest is the last 7
/// days up to now. A window that ends before it starts is refused.
///
/// The third value is the `time_range` that names the window, for the header:
/// only when `since` and `until` left it whole. A narrowed window is told by its
/// bounds alone.
pub(super) fn recency_window<'a>(
    time_range: Option<&'a str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<(Instant, Instant, Option<&'a str>)> {
    let now = chrono::Utc::now();
    let range = time_range
        .map(temporal::parse_time_expression)
        .transpose()?;
    let start = match (since, range) {
        (Some(s), Some((from, _))) => from.max(since_bound(s)?),
        (Some(s), None) => since_bound(s)?,
        (None, Some((from, _))) => from,
        (None, None) => now - chrono::Duration::days(7),
    };
    let end = match (until, range) {
        (Some(u), Some((_, to))) => to.min(until_bound(u)?),
        (Some(u), None) => until_bound(u)?,
        (None, Some((_, to))) => to,
        (None, None) => now,
    };
    if (since.is_some() || until.is_some()) && start > end {
        anyhow::bail!(
            "empty window: it would start at {} and end at {}. Check since and until; \
             until alone searches back 7 days from now, so pass since for an older until.",
            start.to_rfc3339(),
            end.to_rfc3339()
        );
    }
    let named = time_range.filter(|_| since.is_none() && until.is_none());
    Ok((start, end, named))
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
    let (start, end, named) = recency_window(time_range, since, until)?;

    let start_str = start.to_rfc3339();
    let end_str = end.to_rfc3339();
    // The exact bounds searched, in the header, so a window that is not the one
    // the caller meant ("last month" is the previous calendar month) is seen.
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
