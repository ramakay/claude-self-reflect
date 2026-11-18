use super::*;

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
