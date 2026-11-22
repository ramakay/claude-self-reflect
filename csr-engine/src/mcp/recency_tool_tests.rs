//! `search_by_recency` and its keyword fallback must never return a hit dated
//! after `until`, whatever mix of `time_range`, `since` and `until` is given.
//! Fixtures carry fixed timestamps. Only the two shapes that lean on the
//! "last 7 days" default or on a relative `time_range` are built from `now`,
//! and they place every row hours away from every bound.

use super::*;
use crate::import::ConversationChunk;
use crate::mcp::tools::{keyword_fallback, KeywordTool};
use chrono::{Duration, SecondsFormat, Utc};

const QUERY: &str = "recency probe";

struct Rig {
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
}

/// One chunk per `(marker, timestamp)`. Every chunk holds the query words plus
/// its marker, sits in its own conversation, and carries the query's own vector,
/// so every row is a candidate on both the vector and the keyword path.
fn rig(rows: &[(&str, &str)]) -> Rig {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let vector = embeddings.embed_single(QUERY).unwrap();
    let mut engine = SearchEngine::new(vector.len());
    for (seq, (marker, timestamp)) in rows.iter().enumerate() {
        let chunk = ConversationChunk {
            id: format!("recency-{marker}"),
            conversation_id: format!("recency-conv-{marker}"),
            project_name: "recency-proj".into(),
            timestamp: (*timestamp).into(),
            content: format!("{QUERY} {marker}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(chunk.id, vector.clone());
    }
    Rig {
        storage,
        embeddings,
        search: Arc::new(RwLock::new(engine)),
    }
}

async fn vector_path(
    rig: &Rig,
    time_range: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<String> {
    search_by_recency(
        &rig.storage,
        &rig.embeddings,
        &rig.search,
        QUERY,
        time_range,
        since,
        until,
        10,
        0.3,
        None,
    )
    .await
}

async fn keyword_path(
    rig: &Rig,
    time_range: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<String> {
    keyword_fallback(
        &rig.storage,
        &rig.search,
        QUERY,
        10,
        None,
        KeywordTool::Recency {
            time_range,
            since,
            until,
        },
    )
    .await
}

/// Both result paths for one argument shape.
async fn both_paths(
    rig: &Rig,
    time_range: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> [String; 2] {
    [
        vector_path(rig, time_range, since, until).await.unwrap(),
        keyword_path(rig, time_range, since, until).await.unwrap(),
    ]
}

fn stamp(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[tokio::test]
async fn until_alone_as_a_timestamp_excludes_later_hits() {
    let now = Utc::now();
    let rig = rig(&[
        ("alphabefore", &stamp(now - Duration::hours(2))),
        ("alphaafter", &stamp(now - Duration::minutes(30))),
    ]);
    let until = (now - Duration::minutes(90)).to_rfc3339_opts(SecondsFormat::Secs, true);
    for out in both_paths(&rig, None, None, Some(&until)).await {
        assert!(out.contains("alphabefore"));
        assert!(!out.contains("alphaafter"));
    }
}

#[tokio::test]
async fn since_and_until_timestamps_bound_the_window() {
    let rig = rig(&[
        ("alphaearly", "2026-11-09T23:00:00.000Z"),
        ("alphain", "2026-11-10T01:30:00.000Z"),
        ("alphaafter", "2026-11-10T01:47:30.250Z"),
        ("alphalater", "2026-11-12T09:00:00.000Z"),
    ]);
    for out in both_paths(
        &rig,
        None,
        Some("2026-11-10T00:00:00Z"),
        Some("2026-11-10T01:47:00Z"),
    )
    .await
    {
        assert!(out.contains("alphain"));
        assert!(!out.contains("alphaearly"));
        assert!(!out.contains("alphaafter"));
        assert!(!out.contains("alphalater"));
        assert!(out.contains("2026-11-10T00:00:00Z to 2026-11-10T01:47:00Z UTC"));
    }
}

#[tokio::test]
async fn since_and_until_dates_cover_whole_days() {
    let rig = rig(&[
        ("alphaearly", "2026-11-09T23:00:00.000Z"),
        ("alphain", "2026-11-10T23:00:00.000Z"),
        ("alphaafter", "2026-11-11T01:00:00.000Z"),
    ]);
    for out in both_paths(&rig, None, Some("2026-11-10"), Some("2026-11-10")).await {
        assert!(out.contains("alphain"));
        assert!(!out.contains("alphaearly"));
        assert!(!out.contains("alphaafter"));
    }
}

#[tokio::test]
async fn time_range_with_until_applies_the_tighter_end() {
    let rig = rig(&[
        ("alphain", "2026-11-10T11:00:00.000Z"),
        ("alphaafter", "2026-11-10T13:00:00.000Z"),
    ]);
    for out in both_paths(&rig, Some("2026-11-10"), None, Some("2026-11-10T12:00:00Z")).await {
        assert!(out.contains("alphain"));
        assert!(!out.contains("alphaafter"));
        assert!(out.contains("2026-11-10T00:00:00Z to 2026-11-10T12:00:00Z UTC"));
    }
}

#[tokio::test]
async fn natural_time_range_with_until_applies_the_tighter_end() {
    let now = Utc::now();
    let rig = rig(&[
        ("alphabefore", &stamp(now - Duration::hours(2))),
        ("alphaafter", &stamp(now - Duration::minutes(30))),
    ]);
    let until = (now - Duration::minutes(90)).to_rfc3339_opts(SecondsFormat::Secs, true);
    for out in both_paths(&rig, Some("last 7 days"), None, Some(&until)).await {
        assert!(out.contains("alphabefore"));
        assert!(!out.contains("alphaafter"));
    }
}

#[tokio::test]
async fn time_range_with_since_applies_the_tighter_start() {
    let rig = rig(&[
        ("alphaearly", "2026-11-10T11:00:00.000Z"),
        ("alphain", "2026-11-10T13:00:00.000Z"),
    ]);
    for out in both_paths(&rig, Some("2026-11-10"), Some("2026-11-10T12:00:00Z"), None).await {
        assert!(out.contains("alphain"));
        assert!(!out.contains("alphaearly"));
        assert!(out.contains("2026-11-10T12:00:00Z to 2026-11-11T00:00:00Z UTC"));
    }
}

#[tokio::test]
async fn time_range_alone_keeps_its_whole_window() {
    let rig = rig(&[
        ("alphabefore", "2026-11-09T23:30:00.000Z"),
        ("alphamorning", "2026-11-10T00:30:00.000Z"),
        ("alphaevening", "2026-11-10T23:30:00.000Z"),
        ("alphaafter", "2026-11-11T00:30:00.000Z"),
    ]);
    for out in both_paths(&rig, Some("2026-11-10"), None, None).await {
        assert!(out.contains("alphamorning"));
        assert!(out.contains("alphaevening"));
        assert!(!out.contains("alphabefore"));
        assert!(!out.contains("alphaafter"));
        assert!(out.contains("2026-11-10T00:00:00Z to 2026-11-11T00:00:00Z UTC"));
    }
}

/// An `until` that names a time of day stops at that time, not at the end of
/// the day, whichever spelling it uses.
async fn assert_until_stops_at_0147(until: &str) {
    let rig = rig(&[
        ("alphain", "2026-11-10T01:30:00.000Z"),
        ("alphaafter", "2026-11-10T01:48:00.000Z"),
        ("alphaevening", "2026-11-10T22:00:00.000Z"),
    ]);
    for out in both_paths(&rig, None, Some("2026-11-10T00:00:00Z"), Some(until)).await {
        assert!(out.contains("alphain"));
        assert!(!out.contains("alphaafter"));
        assert!(!out.contains("alphaevening"));
        assert!(out.contains("2026-11-10T00:00:00Z to 2026-11-10T01:47:00Z UTC"));
    }
}

#[tokio::test]
async fn until_with_a_space_and_minutes_is_honoured_to_the_minute() {
    assert_until_stops_at_0147("2026-11-10 01:47").await;
}

#[tokio::test]
async fn until_without_a_zone_is_honoured_to_the_second() {
    assert_until_stops_at_0147("2026-11-10T01:47:00").await;
}

#[tokio::test]
async fn until_with_an_offset_is_honoured_to_the_instant() {
    assert_until_stops_at_0147("2026-11-10T03:47:00+02:00").await;
}

/// Both bounds are inclusive to the millisecond for a row stored in the form
/// Claude Code writes (`...mmmZ`): the exact instant is in, one millisecond
/// outside is out.
#[tokio::test]
async fn bounds_are_inclusive_to_the_millisecond_for_stored_rows() {
    let rig = rig(&[
        ("alphaedgebeforesince", "2026-11-09T23:59:59.999Z"),
        ("alphaedgeatsince", "2026-11-10T00:00:00.000Z"),
        ("alphaedgeatuntil", "2026-11-10T01:47:00.000Z"),
        ("alphaedgeafteruntil", "2026-11-10T01:47:00.001Z"),
    ]);
    let since = Some("2026-11-10T00:00:00Z");
    let until = Some("2026-11-10T01:47:00Z");
    let [vector, keyword] = both_paths(&rig, None, since, until).await;
    for out in [vector, keyword] {
        assert!(out.contains("alphaedgeatsince"));
        assert!(out.contains("alphaedgeatuntil"));
        assert!(!out.contains("alphaedgebeforesince"));
        assert!(!out.contains("alphaedgeafteruntil"));
    }
}

/// A row at the exact `until` instant is inside on the vector path, whose bound
/// is compared with the stored timestamp as text.
#[tokio::test]
async fn vector_path_row_at_the_exact_until_instant_is_inside() {
    let rig = rig(&[("alphaedgeatuntil", "2026-11-10T01:47:00.000Z")]);
    let out = vector_path(
        &rig,
        None,
        Some("2026-11-10T00:00:00Z"),
        Some("2026-11-10T01:47:00Z"),
    )
    .await
    .unwrap();
    assert!(out.contains("alphaedgeatuntil"));
}

/// The same on the keyword path, whose bound goes through `julianday`.
#[tokio::test]
async fn keyword_path_row_at_the_exact_until_instant_is_inside() {
    let rig = rig(&[("alphaedgeatuntil", "2026-11-10T01:47:00.000Z")]);
    let out = keyword_path(
        &rig,
        None,
        Some("2026-11-10T00:00:00Z"),
        Some("2026-11-10T01:47:00Z"),
    )
    .await
    .unwrap();
    assert!(out.contains("alphaedgeatuntil"));
}

/// A row stored with `+00:00` (plans and Codex rollouts) that lies hours from
/// both bounds is still inside the window, and one hours past it is not.
#[tokio::test]
async fn rows_stored_with_a_utc_offset_are_found_hours_from_the_bounds() {
    let rig = rig(&[
        ("alphaoffsetin", "2026-11-10T06:00:00+00:00"),
        ("alphaoffsetlater", "2026-11-10T18:00:00+00:00"),
    ]);
    let since = Some("2026-11-10T00:00:00Z");
    let until = Some("2026-11-10T12:00:00Z");
    for out in both_paths(&rig, None, since, until).await {
        assert!(out.contains("alphaoffsetin"));
        assert!(!out.contains("alphaoffsetlater"));
    }
}

/// The same rows at the bounds themselves. The vector path compares text, and
/// `+00:00` sorts before `.000Z`, so a start written `...000Z` drops the row at
/// the `since` instant.
#[tokio::test]
async fn rows_stored_with_a_utc_offset_are_found_at_the_exact_bounds() {
    let rig = rig(&[
        ("alphaoffsetbeforesince", "2026-11-09T23:59:59.999+00:00"),
        ("alphaoffsetatsince", "2026-11-10T00:00:00+00:00"),
        ("alphaoffsetatuntil", "2026-11-10T01:47:00+00:00"),
        ("alphaoffsetafteruntil", "2026-11-10T01:47:00.001+00:00"),
    ]);
    let since = Some("2026-11-10T00:00:00Z");
    let until = Some("2026-11-10T01:47:00Z");
    for out in both_paths(&rig, None, since, until).await {
        assert!(out.contains("alphaoffsetatsince"));
        assert!(out.contains("alphaoffsetatuntil"));
        assert!(!out.contains("alphaoffsetbeforesince"));
        assert!(!out.contains("alphaoffsetafteruntil"));
    }
}

/// A `since` with milliseconds: the row at that instant is inside in both
/// stored forms, and the row one millisecond before it is not.
#[tokio::test]
async fn a_since_with_milliseconds_is_inclusive_for_both_stored_forms() {
    let rig = rig(&[
        ("alphamsbeforeoffset", "2026-11-10T00:00:00.249+00:00"),
        ("alphamsbeforezulu", "2026-11-10T00:00:00.249Z"),
        ("alphamsatoffset", "2026-11-10T00:00:00.250+00:00"),
        ("alphamsatzulu", "2026-11-10T00:00:00.250Z"),
    ]);
    let since = Some("2026-11-10T00:00:00.250Z");
    let until = Some("2026-11-10T01:47:00Z");
    for out in both_paths(&rig, None, since, until).await {
        assert!(out.contains("alphamsatoffset"));
        assert!(out.contains("alphamsatzulu"));
        assert!(!out.contains("alphamsbeforeoffset"));
        assert!(!out.contains("alphamsbeforezulu"));
    }
}

#[tokio::test]
async fn a_window_with_its_start_after_its_end_is_refused() {
    let rig = rig(&[("alphain", "2026-11-10T01:30:00.000Z")]);
    let shapes: [(Option<&str>, Option<&str>, Option<&str>); 3] = [
        (
            None,
            Some("2026-11-10T02:00:00Z"),
            Some("2026-11-10T01:47:00Z"),
        ),
        (Some("2026-11-10"), Some("2026-11-12T00:00:00Z"), None),
        (None, None, Some("2020-01-01T00:00:00Z")),
    ];
    for (time_range, since, until) in shapes {
        let vector = vector_path(&rig, time_range, since, until).await;
        let keyword = keyword_path(&rig, time_range, since, until).await;
        for outcome in [vector, keyword] {
            assert!(outcome.unwrap_err().to_string().contains("empty window"));
        }
    }
}
