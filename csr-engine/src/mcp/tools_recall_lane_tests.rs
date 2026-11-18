use super::*;
use crate::import::ConversationChunk;
use crate::provenance::Speaker;

fn chunk(id: &str, conversation: &str, content: &str) -> ConversationChunk {
    ConversationChunk {
        id: id.into(),
        conversation_id: conversation.into(),
        project_name: "alpha".into(),
        timestamp: "2026-10-10T00:00:00Z".into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: Speaker::User,
        seq: 0,
        is_sidechain: false,
    }
}

fn unscoped() -> SearchProjectScope {
    SearchProjectScope {
        effective_project: None,
        scope_label: "all".into(),
        current_project_for_all_scope: None,
        family_anchor: None,
        family_projects: HashSet::new(),
        projects_root_override: None,
    }
}

fn file_store(rows: &[(&str, &str, &str)]) -> (tempfile::TempDir, Arc<Storage>) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(&dir.path().join("csr-engine.db")).unwrap());
    for (id, conversation, content) in rows {
        storage
            .insert_chunk(&chunk(id, conversation, content), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
    }
    (dir, storage)
}

fn rows_of(legs: &KeywordLegs) -> Vec<(String, usize, u64)> {
    legs.keyword
        .hits
        .iter()
        .map(|(chunk, ordinal, bm25)| (chunk.id.clone(), *ordinal, bm25.to_bits()))
        .collect()
}

#[tokio::test]
async fn lane_keyword_legs_equal_the_inline_legs() {
    let rows: Vec<(String, String, String)> = (0..12)
        .map(|n| {
            (
                format!("lane-id{n}"),
                format!("lane-conv{}", n % 4),
                format!("quartzlane notes about retries {n} {}", "filler ".repeat(n)),
            )
        })
        .collect();
    let refs: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    let (_dir, storage) = file_store(&refs);
    let scope = unscoped();
    let inline = keyword_legs(
        &storage,
        "quartzlane retries notes",
        30,
        SearchMode::Hybrid,
        None,
    );
    assert!(!inline.keyword.hits.is_empty());
    let lane = spawn_keyword_lane(
        &storage,
        "quartzlane retries notes",
        30,
        &scope,
        SearchMode::Hybrid,
    )
    .expect("a file store has a lane")
    .await
    .unwrap()
    .expect("the lane opened");
    assert_eq!(rows_of(&inline), rows_of(&lane));
    assert_eq!(inline.keyword.window_full, lane.keyword.window_full);
    assert_eq!(inline.postings_hits, lane.postings_hits);
    assert_eq!(
        inline.postings_weight.to_bits(),
        lane.postings_weight.to_bits()
    );
    assert_eq!(inline.reflection_fts_hits, lane.reflection_fts_hits);
    assert_eq!(inline.reflection_prefix_ids, lane.reflection_prefix_ids);
}

#[tokio::test]
async fn a_failed_lane_read_reruns_the_legs_on_the_main_connection() {
    let rows: Vec<(String, String, String)> = (0..6)
        .map(|n| {
            (
                format!("faultlane-id{n}"),
                format!("faultlane-conv{n}"),
                format!("zirconfault notes about retries {n}"),
            )
        })
        .collect();
    let refs: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    let (_dir, storage) = file_store(&refs);
    let scope = unscoped();
    let query = "zirconfault retries notes";
    let inline = keyword_legs(&storage, query, 30, SearchMode::Hybrid, None);
    assert!(!inline.keyword.hits.is_empty());
    // A temp table shadows `chunks_fts` on the lane connection only: its
    // chunk read errors while the main connection stays healthy.
    let lane = spawn_keyword_lane_with(&storage, query, 30, &scope, SearchMode::Hybrid, |lane| {
        lane.with_connection(|conn| {
            conn.execute_batch("CREATE TEMP TABLE chunks_fts (content TEXT);")?;
            Ok(())
        })
        .unwrap();
    })
    .expect("a file store has a lane");
    let legs =
        collect_keyword_legs(Some(lane), &storage, query, 30, SearchMode::Hybrid, None).await;
    assert_eq!(rows_of(&inline), rows_of(&legs));
    assert_eq!(inline.keyword.window_full, legs.keyword.window_full);
}

#[tokio::test]
async fn no_lane_for_a_memory_store_or_a_vector_pass() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let scope = unscoped();
    let lane = spawn_keyword_lane(&storage, "anything", 10, &scope, SearchMode::Hybrid)
        .expect("hybrid spawns")
        .await
        .unwrap();
    assert!(lane.is_none(), "a memory store has no file to open twice");
    assert!(spawn_keyword_lane(&storage, "anything", 10, &scope, SearchMode::Vector).is_none());
}

#[tokio::test]
async fn verdict_prefetch_judges_the_same_sessions_the_filter_would() {
    let marked: Vec<(String, String, String)> = (0..6)
        .map(|n| {
            (
                format!("prefetch-eval-{n}"),
                "prefetch-eval-session".to_string(),
                format!("ran the rig under .testbed/run{n}"),
            )
        })
        .collect();
    let mut refs: Vec<(&str, &str, &str)> = marked
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    refs.push((
        "prefetch-plain-0",
        "prefetch-plain-session",
        "ordinary design talk",
    ));
    let (_dir, storage) = file_store(&refs);
    let chunks = storage
        .get_chunks_by_ids(&refs.iter().map(|r| r.0.to_string()).collect::<Vec<_>>())
        .unwrap();
    spawn_verdict_prefetch(&storage, &chunks)
        .expect("a file store has a lane")
        .await
        .unwrap();
    let candidates: Vec<super::super::synthetic::ChunkRef> = chunks
        .iter()
        .map(|c| super::super::synthetic::ChunkRef {
            id: c.id.clone(),
            conversation_id: c.conversation_id.clone(),
            author: Some(Speaker::User),
        })
        .collect();
    let dropped = super::super::synthetic::synthetic_ids(&storage, &candidates);
    assert_eq!(dropped.len(), 6, "{dropped:?}");
    assert!(dropped.iter().all(|id| id.starts_with("prefetch-eval-")));
}
