use super::*;
use crate::embeddings::EmbeddingEngine;
use crate::search::SearchEngine;
use crate::storage::Storage;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

fn test_engine(storage: Arc<Storage>, root: &Path) -> Engine {
    Engine::from_parts(
        storage,
        Arc::new(EmbeddingEngine::new().unwrap()),
        Arc::new(RwLock::new(SearchEngine::new(64))),
        root.to_path_buf(),
    )
}

#[test]
fn rollout_receipt_uses_the_actual_jsonl_byte_offset() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-offset.jsonl");
    let metadata =
        r#"{"type":"session_meta","payload":{"id":"offset-fixture","cwd":"/tmp/project"}}"#;
    let message = r#"{"type":"message","role":"user","content":"remember this"}"#;
    std::fs::write(&path, format!("{metadata}\n{message}\n")).unwrap();

    let mut observed = Vec::new();
    visit_rollout_messages(&path, |message| {
        observed.push(
            message
                .get("_csr_receipt_ref")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_string(),
        );
        Ok(())
    })
    .unwrap();

    assert_eq!(
        observed,
        vec![format!("{}#byte={}", path.display(), metadata.len() + 1)]
    );
}

#[test]
fn codex_response_channels_distinguish_assistant_from_unclassified_payloads() {
    let mut assistant = Vec::new();
    let mut misses = 0;
    let mut stripped = 0;
    parse_response_item(
        &serde_json::json!({
            "type":"response_item",
            "payload":{"type":"agent_message", "content":"assistant prose"}
        }),
        &mut assistant,
        &mut misses,
        &mut stripped,
    );
    assert_eq!(assistant.len(), 1);
    assert!(assistant[0].get("_csr_channel").is_none());

    let mut unknown = Vec::new();
    parse_response_item(
        &serde_json::json!({
            "type":"response_item",
            "payload":{"type":"future_payload", "text":"unclassified prose"}
        }),
        &mut unknown,
        &mut misses,
        &mut stripped,
    );
    assert_eq!(unknown.len(), 1);
    assert_eq!(
        unknown[0]
            .get("_csr_channel")
            .and_then(serde_json::Value::as_str),
        Some("unclassified")
    );
}

#[test]
fn imported_rollout_chunks_receive_cached_structural_floors() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(source, &target).unwrap();
    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());

    let stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(stats.files_imported, 1);
    let ids = storage
        .get_chunk_ids_for_conversation("codex:modern-fixture")
        .unwrap();
    assert!(!ids.is_empty());
    let tiers = ids
        .iter()
        .map(|id| storage.get_chunk_min_trust(id).unwrap())
        .collect::<Vec<_>>();
    assert!(tiers.contains(&crate::provenance::TrustTier::UserHistory));
    assert!(tiers.contains(&crate::provenance::TrustTier::TrustedTool));
    assert!(tiers.contains(&crate::provenance::TrustTier::Unknown));
}

#[test]
fn rollout_import_takes_no_storage_lock_under_the_index_write_guard() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    std::fs::copy(source, root.path().join("rollout-guard.jsonl")).unwrap();
    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let (_probe, under_guard) = crate::storage::lock_probe::watch_index(engine.search().clone());

    let stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(stats.files_imported, 1);
    let ids = storage
        .get_chunk_ids_for_conversation("codex:modern-fixture")
        .unwrap();
    assert!(!ids.is_empty());
    let index = engine.search().try_read().unwrap();
    assert!(ids.iter().all(|id| index.has_chunk(id)));
    assert_eq!(
        under_guard.get(),
        0,
        "a storage call waited while the index write guard was held"
    );
}

#[test]
fn rollout_chunks_carry_distinct_per_message_timestamps() {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let parsed = parse_rollout(&source).unwrap().expect("fixture parses");
    let chunks = rollout_chunks(&parsed);
    assert!(!chunks.is_empty());
    let timestamps = chunks
        .iter()
        .map(|chunk| chunk.timestamp.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        timestamps.len() >= 3,
        "expected at least 3 distinct chunk timestamps, got {timestamps:?}"
    );
    assert!(
        !timestamps.iter().all(|ts| ts == &parsed.timestamp),
        "chunks must not all collapse onto the session-start timestamp {}",
        parsed.timestamp
    );
}

/// Fixture line 8 is an unknown top-level type (`future_envelope`) that
/// carries its own timestamp (12:00:07), arriving well after the
/// session-start latch (12:00:00) was already set by line 1. The
/// `Some(_)` branch in `visit_rollout_messages_inner` used to pass the
/// latched `timestamp.as_deref()` regardless, so this chunk landed on
/// 12:00:00 instead of its own line's time.
#[test]
fn unknown_type_lines_keep_their_own_timestamp_not_the_session_latch() {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let parsed = parse_rollout(&source).unwrap().expect("fixture parses");
    let chunks = rollout_chunks(&parsed);
    let future_envelope = chunks
        .iter()
        .find(|chunk| chunk.content.contains("UNKNOWN TEXT RETAINED"))
        .expect("future_envelope line produces a chunk");
    assert_eq!(future_envelope.timestamp, "2026-08-06T12:00:07Z");
}

#[test]
fn imported_rollout_chunks_carry_the_messages_own_timestamp_not_the_session_start() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(source, &target).unwrap();
    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());

    let stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(stats.files_imported, 1);
    let ids = storage
        .get_chunk_ids_for_conversation("codex:modern-fixture")
        .unwrap();
    assert!(!ids.is_empty());
    let chunks = storage.get_chunks_by_ids(&ids).unwrap();
    let timestamps = chunks
        .iter()
        .map(|chunk| chunk.timestamp.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        timestamps.len() > 1,
        "expected chunks to carry distinct per-message timestamps, got {timestamps:?}"
    );
    assert!(
        !timestamps.iter().all(|ts| ts == "2026-08-06T12:00:00Z"),
        "chunks must not all collapse onto the session-start timestamp"
    );
}

#[test]
fn restamp_backfill_repairs_collapsed_timestamps_and_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(source, &target).unwrap();
    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());

    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 1);

    let ids = storage
        .get_chunk_ids_for_conversation("codex:modern-fixture")
        .unwrap();
    assert!(!ids.is_empty());
    let chunks = storage.get_chunks_by_ids(&ids).unwrap();

    // Simulate the pre-fix bug directly: collapse every chunk's
    // timestamp onto the session-start time, exactly what the old
    // `timestamp: metadata.timestamp.clone()` produced. Content is
    // unchanged, so the content-guarded setter still matches.
    for chunk in &chunks {
        let touched = storage
            .restamp_chunk_timestamp_if_content_matches(
                &chunk.id,
                &chunk.conversation_id,
                &chunk.content,
                "2026-08-06T12:00:00Z",
            )
            .unwrap();
        assert!(
            touched,
            "setup: content guard should match unchanged content"
        );
    }
    let collapsed = storage.get_chunks_by_ids(&ids).unwrap();
    let collapsed_timestamps: std::collections::BTreeSet<_> =
        collapsed.iter().map(|c| c.timestamp.clone()).collect();
    assert_eq!(
        collapsed_timestamps.len(),
        1,
        "setup: simulated collapse should leave exactly one distinct timestamp"
    );

    let fts_blocks_before = storage
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM chunks_fts_data", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(Into::into)
        })
        .unwrap();

    let restamp_stats = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert!(!restamp_stats.already_applied);
    assert_eq!(restamp_stats.conversations_considered, 1);

    // The content-guarded restamp UPDATE is timestamp-only — with
    // chunks_fts_au scoped to `UPDATE OF content`, it must not touch
    // chunks_fts_data at all (the FTS-bloat regression this fix closes).
    let fts_blocks_after = storage
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM chunks_fts_data", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        fts_blocks_before, fts_blocks_after,
        "restamp must not grow chunks_fts_data"
    );
    assert_eq!(restamp_stats.conversations_updated, 1);
    assert_eq!(restamp_stats.conversations_missing_file, 0);
    assert_eq!(restamp_stats.conversations_id_mismatch, 0);
    assert_eq!(restamp_stats.conversations_unmatched, 0);
    assert_eq!(restamp_stats.chunks_updated, ids.len());
    assert_eq!(restamp_stats.chunks_unmatched, 0);

    let repaired = storage.get_chunks_by_ids(&ids).unwrap();
    let repaired_timestamps: std::collections::BTreeSet<_> =
        repaired.iter().map(|c| c.timestamp.clone()).collect();
    assert!(
        repaired_timestamps.len() > 1,
        "restamp should restore distinct per-message timestamps, got {repaired_timestamps:?}"
    );

    // Idempotent: a second run is a no-op guarded by the meta marker.
    let second = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert!(second.already_applied);
    assert_eq!(second.chunks_updated, 0);
    assert_eq!(second.conversations_considered, 0);
}

#[test]
fn restamp_backfill_counts_missing_files_without_fabricating() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(source, &target).unwrap();
    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());

    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 1);

    // Capture-on-appearance: the source rollout is gone by backfill time.
    std::fs::remove_file(&target).unwrap();

    let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert!(!stats.already_applied);
    assert_eq!(stats.conversations_considered, 1);
    assert_eq!(stats.conversations_missing_file, 1);
    assert_eq!(stats.conversations_updated, 0);
    assert_eq!(stats.chunks_updated, 0);
}

#[test]
fn harness_strip_backfill_invalidates_only_polluted_rows_and_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(source, &target).unwrap();
    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());

    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 1);

    // Simulate the pre-fix pollution directly, the same way the restamp
    // tests simulate the pre-fix timestamp bug: stamp the stored
    // `summary` with harness boilerplate, as if this row had been
    // imported before the WP-A adapter strip existed.
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:modern-fixture'",
                rusqlite::params![
                    "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                ],
            )?;
            Ok(())
        })
        .unwrap();

    let target_key = target.to_string_lossy().to_string();
    let original_mtime = storage
        .get_import_state_mtime(&target_key)
        .unwrap()
        .unwrap();
    assert!(!original_mtime.is_empty());

    let stats = invalidate_polluted_codex_rollouts(&engine).unwrap();
    assert!(!stats.already_applied);
    assert_eq!(stats.conversations_considered, 1);
    assert_eq!(stats.conversations_polluted, 1);
    assert_eq!(stats.conversations_invalidated, 1);
    assert_eq!(stats.conversations_missing_file, 0);

    let cleared_mtime = storage
        .get_import_state_mtime(&target_key)
        .unwrap()
        .unwrap();
    assert_eq!(
        cleared_mtime, "",
        "invalidated row's mtime must no longer match the real file"
    );

    // The next discovery pass re-imports the invalidated row through the
    // (already-fixed) adapter, overwriting the simulated pollution.
    let reimport_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(reimport_stats.files_imported, 1);
    let ids = storage
        .get_chunk_ids_for_conversation("codex:modern-fixture")
        .unwrap();
    let chunks = storage.get_chunks_by_ids(&ids).unwrap();
    assert!(
        chunks.iter().all(|c| !c
            .summary
            .as_deref()
            .unwrap_or_default()
            .starts_with("<recommended_plugins>")),
        "reimport must clear the simulated pollution: {:?}",
        chunks.iter().map(|c| &c.summary).collect::<Vec<_>>()
    );

    // Idempotent: a second backfill run is a no-op guarded by the meta marker.
    let second = invalidate_polluted_codex_rollouts(&engine).unwrap();
    assert!(second.already_applied);
    assert_eq!(second.conversations_considered, 0);
    assert_eq!(second.conversations_invalidated, 0);
}

#[test]
fn harness_strip_backfill_counts_missing_files_without_touching_clean_rows() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    // One polluted row whose file will vanish, one clean row that must be
    // left completely alone.
    let polluted_target = root.path().join("rollout-polluted.jsonl");
    let clean_target = root.path().join("rollout-clean.jsonl");
    std::fs::copy(&source, &polluted_target).unwrap();
    std::fs::copy(&source, &clean_target).unwrap();
    // Distinct conversation ids: the fixture's session_meta id is fixed,
    // so give the second copy its own id by rewriting the file.
    let rewritten = std::fs::read_to_string(&source)
        .unwrap()
        .replace("modern-fixture", "modern-fixture-clean");
    std::fs::write(&clean_target, rewritten).unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 2);

    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:modern-fixture'",
                rusqlite::params!["<environment_context>\ncwd: /tmp\n</environment_context>"],
            )?;
            Ok(())
        })
        .unwrap();
    let clean_mtime_before = storage
        .get_import_state_mtime(&clean_target.to_string_lossy())
        .unwrap()
        .unwrap();

    // Capture-on-appearance: the polluted rollout's source file is gone
    // by backfill time.
    std::fs::remove_file(&polluted_target).unwrap();

    let stats = invalidate_polluted_codex_rollouts(&engine).unwrap();
    assert!(!stats.already_applied);
    assert_eq!(stats.conversations_considered, 2);
    assert_eq!(stats.conversations_polluted, 1);
    assert_eq!(stats.conversations_invalidated, 0);
    assert_eq!(stats.conversations_missing_file, 1);

    // The clean row's mtime must be untouched.
    let clean_mtime_after = storage
        .get_import_state_mtime(&clean_target.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(clean_mtime_before, clean_mtime_after);
}

/// Regression for the wave-2 finding: a harness-polluted conversation
/// (stored content is pre-strip, so the content guard can never match
/// what the fixed chunker re-derives from the on-disk file) must be
/// *skipped*, not counted `conversations_unmatched` — otherwise the done
/// key can never be set while any pollution remains anywhere in the
/// corpus, freezing the fix for every other conversation too. Reverting
/// the `polluted.contains(&conversation_id)` skip in
/// `restamp_codex_chunk_timestamps` makes this fail: `conversations_unmatched`
/// becomes 1, `conversations_skipped_polluted` stays 0, and the meta key
/// is never set.
#[test]
fn restamp_skips_harness_polluted_conversations_and_still_completes_for_the_rest() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let polluted_target = root.path().join("rollout-polluted.jsonl");
    let clean_target = root.path().join("rollout-clean.jsonl");
    std::fs::copy(&source, &polluted_target).unwrap();
    let rewritten = std::fs::read_to_string(&source)
        .unwrap()
        .replace("modern-fixture", "modern-fixture-clean");
    std::fs::write(&clean_target, rewritten).unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 2);

    // Simulate the pre-strip pollution directly (same technique as the
    // harness-strip backfill tests above), but also corrupt the stored
    // `content` on seq 0 so the content guard genuinely cannot match —
    // the on-disk file (already fixed) re-derives clean text, while the
    // stored row still carries the old harness-prefixed content a
    // pre-fix import would have produced.
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:modern-fixture'",
                rusqlite::params![
                    "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                ],
            )?;
            conn.execute(
                "UPDATE chunks SET content = ?1 || content
                     WHERE conversation_id = 'codex:modern-fixture' AND seq = 0",
                rusqlite::params![
                    "<recommended_plugins>\nHere is a list of plugins that are available but not installed\n"
                ],
            )?;
            Ok(())
        })
        .unwrap();

    let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert!(!stats.already_applied);
    assert_eq!(stats.conversations_considered, 2);
    assert_eq!(
        stats.conversations_skipped_polluted, 1,
        "the polluted conversation must be skipped, not re-derived"
    );
    assert_eq!(
        stats.conversations_unmatched, 0,
        "a skipped polluted row must never be counted as drift-unmatched"
    );
    assert_eq!(stats.chunks_unmatched, 0);
    assert_eq!(
        stats.conversations_updated, 1,
        "the clean conversation must still be restamped"
    );
    assert_eq!(stats.attempts, 1);

    assert!(
        storage.get_meta(RESTAMP_META_KEY).unwrap().is_some(),
        "outstanding harness pollution elsewhere must not block the done key \
             for an otherwise-clean pass"
    );
}

/// Regression for the wave-2 finding: genuine (non-harness) content
/// drift must leave the done key unset and must be retried, bounded, on
/// later calls — not silently marked done. Reverting the
/// `if stats.conversations_unmatched == 0 && stats.chunks_unmatched == 0`
/// gate (i.e. restoring the old unconditional `set_meta`) makes the
/// first two assertions fail: the meta key would be set immediately with
/// `conversations_unmatched == 1`.
#[test]
fn restamp_withholds_done_key_while_genuine_drift_leaves_chunks_unmatched() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(&source, &target).unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    import_changed_rollouts(&engine, root.path()).unwrap();

    // Genuine drift, unrelated to harness pollution: the stored content
    // no longer matches what the on-disk file re-derives, and the
    // summary is never touched (not a harness marker), so this row is
    // never in `polluted_codex_conversation_ids`.
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET content = content || ' DRIFTED'
                     WHERE conversation_id = 'codex:modern-fixture'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let first = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert!(!first.already_applied);
    assert_eq!(first.conversations_skipped_polluted, 0);
    assert_eq!(first.conversations_unmatched, 1);
    assert!(first.chunks_unmatched > 0);
    assert_eq!(first.attempts, 1);
    assert!(
        storage.get_meta(RESTAMP_META_KEY).unwrap().is_none(),
        "must not claim done while unmatched chunks remain"
    );
    assert!(storage
        .get_meta(RESTAMP_PROGRESS_META_KEY)
        .unwrap()
        .is_some());

    // A later daemon tick retries and bumps the attempt count, still
    // withholding the done key.
    let second = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert_eq!(second.attempts, 2);
    assert_eq!(second.conversations_unmatched, 1);
    assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_none());
}

/// W3-R high review finding ("the same trap catches transient drift"):
/// a codex rollout that grew on disk since it was last imported (new
/// mtime, new tail content) must never be treated the same as a
/// permanently drifted file. Pre-fix, the new tail message's re-derived
/// chunk has no matching stored row (it was never imported), so it
/// lands in `chunks_unmatched` and blocks the done key forever — a
/// normal `import_changed_rollouts` pass would pick the new mtime up on
/// its own the very next tick, so this is not this restamp's problem to
/// solve, and definitely not an attempt-burning one. Must fail at HEAD
/// (pre-fix: no mtime check before content re-derivation).
#[test]
fn restamp_treats_a_stale_mtime_as_pending_import_not_unmatched() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(&source, &target).unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    import_changed_rollouts(&engine, root.path()).unwrap();

    // The file grows after this import (a real, in-flight session
    // write) — its mtime and tail content both move on, but nothing has
    // reimported it yet.
    let extra_line = r#"{"timestamp":"2026-08-06T12:10:00Z","type":"response_item","payload":{"type":"message","id":"grew-1","role":"user","content":[{"type":"input_text","text":"a later message the stored chunks do not know about yet"}]}}"#;
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&target)
            .unwrap();
        writeln!(file, "{extra_line}").unwrap();
    }

    let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert_eq!(
        stats.conversations_pending_import, 1,
        "a file that changed since the last import must be skipped as pending, not scanned"
    );
    assert_eq!(
        stats.conversations_unmatched, 0,
        "transient mtime drift must never be counted as an unmatched conversation"
    );
    assert_eq!(
        stats.chunks_unmatched, 0,
        "transient mtime drift must never burn a restamp attempt via chunks_unmatched"
    );
    assert!(
        storage.get_meta(RESTAMP_META_KEY).unwrap().is_some(),
        "nothing genuinely unmatched or polluted remains, so this must still reach done"
    );
}

/// Retries are bounded: past `RESTAMP_MAX_ATTEMPTS` the function stops
/// re-scanning the whole corpus on every daemon tick, but still never
/// claims the done key for a file that will never start matching.
#[test]
fn restamp_stops_retrying_after_max_attempts_without_claiming_done() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let target = root.path().join("rollout-copy.jsonl");
    std::fs::copy(&source, &target).unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    import_changed_rollouts(&engine, root.path()).unwrap();
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET content = content || ' DRIFTED'
                     WHERE conversation_id = 'codex:modern-fixture'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    for attempt in 1..=RESTAMP_MAX_ATTEMPTS {
        let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert!(!stats.attempts_exhausted);
        assert_eq!(stats.attempts, attempt);
    }

    let exhausted = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert!(exhausted.attempts_exhausted);
    assert_eq!(exhausted.attempts, RESTAMP_MAX_ATTEMPTS);
    assert_eq!(
        exhausted.conversations_considered, 0,
        "no re-scan once exhausted"
    );
    assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_none());
}

/// The daemon-owned harness-strip job processes bounded batches and is
/// resumable with no separate cursor: each call re-queries what is still
/// polluted, so an interrupted run just leaves more for the next call.
#[test]
fn harness_strip_batch_reimports_in_bounded_batches_and_is_resumable() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let raw = std::fs::read_to_string(&source).unwrap();
    let mut conversation_ids = Vec::new();
    for n in 0..3 {
        let id = format!("modern-fixture-{n}");
        std::fs::write(
            root.path().join(format!("rollout-{n}.jsonl")),
            raw.replace("modern-fixture", &id),
        )
        .unwrap();
        conversation_ids.push(format!("codex:{id}"));
    }

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 3);

    for conversation_id in &conversation_ids {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET summary = ?1 WHERE conversation_id = ?2",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed",
                        conversation_id,
                    ],
                )?;
                conn.execute(
                    "UPDATE chunks SET content = ?1 || content WHERE conversation_id = ?2 AND seq = 0",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed\n",
                        conversation_id,
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    let batch1 = harness_strip_batch(&engine, 1, &AtomicBool::new(false)).unwrap();
    assert!(!batch1.already_applied);
    assert_eq!(batch1.polluted_considered, 3);
    assert_eq!(batch1.reimported, 1);
    assert_eq!(batch1.polluted_remaining, 2);
    assert!(!batch1.done);
    assert!(storage
        .get_meta(HARNESS_STRIP_V2_META_KEY)
        .unwrap()
        .is_none());

    let batch2 = harness_strip_batch(&engine, 1, &AtomicBool::new(false)).unwrap();
    assert_eq!(batch2.polluted_considered, 2);
    assert_eq!(batch2.reimported, 1);
    assert_eq!(batch2.polluted_remaining, 1);
    assert!(!batch2.done);

    // A batch larger than the remaining backlog drains it completely.
    let batch3 = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
    assert_eq!(batch3.reimported, 1);
    assert_eq!(batch3.polluted_remaining, 0);
    assert!(batch3.done);
    assert!(storage
        .get_meta(HARNESS_STRIP_V2_META_KEY)
        .unwrap()
        .is_some());

    let batch4 = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
    assert!(batch4.already_applied);

    // Acceptance: no chunk of any of the three conversations still
    // starts with a harness marker.
    for conversation_id in &conversation_ids {
        let ids = storage
            .get_chunk_ids_for_conversation(conversation_id)
            .unwrap();
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();
        assert!(
            chunks
                .iter()
                .all(|c| !c.content.trim_start().starts_with("<recommended_plugins>")),
            "{conversation_id} still has a harness-prefixed chunk: {:?}",
            chunks.iter().map(|c| &c.content).collect::<Vec<_>>()
        );
    }

    // Cumulative progress persisted across the three batches.
    let progress: HarnessStripProgress = serde_json::from_str(
        &storage
            .get_meta(HARNESS_STRIP_V2_PROGRESS_META_KEY)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(progress.reimported_total, 3);
    assert_eq!(progress.missing_file_total, 0);
}

/// A polluted conversation whose source file is gone must never block
/// completion — it is left polluted forever, the same
/// `conversations_missing_file` convention every other backfill here
/// uses, and `harness_strip_batch` must still reach `done: true` once
/// every *file-having* polluted row is cleared.
#[test]
fn harness_strip_batch_completes_even_when_a_polluted_file_is_gone() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let present = root.path().join("rollout-present.jsonl");
    let gone = root.path().join("rollout-gone.jsonl");
    std::fs::copy(&source, &present).unwrap();
    std::fs::write(
        &gone,
        std::fs::read_to_string(&source)
            .unwrap()
            .replace("modern-fixture", "modern-fixture-gone"),
    )
    .unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    import_changed_rollouts(&engine, root.path()).unwrap();

    for (conversation_id, _) in [
        ("codex:modern-fixture", &present),
        ("codex:modern-fixture-gone", &gone),
    ] {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET summary = ?1 WHERE conversation_id = ?2",
                    rusqlite::params![
                        "<environment_context>\ncwd: /tmp\n</environment_context>",
                        conversation_id,
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }
    std::fs::remove_file(&gone).unwrap();

    let batch = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        batch.polluted_considered, 1,
        "the missing-file row is excluded up front"
    );
    assert_eq!(batch.reimported, 1);
    assert_eq!(batch.polluted_remaining, 0);
    assert!(
        batch.done,
        "a permanently missing-file row must never block completion"
    );
    assert!(storage
        .get_meta(HARNESS_STRIP_V2_META_KEY)
        .unwrap()
        .is_some());
}

/// End-to-end regression for the "medium" wave-2 finding: a rollout
/// whose first user item is a real `<recommended_plugins>` harness block
/// (seeded directly, simulating an import that predates the strip),
/// collapsed onto a single timestamp (simulating the pre-fix
/// timestamp-collapse bug too — both defects at once, as the live
/// corpus actually has them). Running the harness-strip batch job and
/// then the restamp must leave distinct per-message timestamps, no
/// harness-prefixed chunk content, and both meta keys set only once
/// nothing remains to fix.
#[test]
fn harness_strip_then_restamp_clears_a_pre_strip_recommended_plugins_rollout() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-legacy.jsonl");
    let lines = [
        r#"{"timestamp":"2026-09-01T09:00:00Z","type":"session_meta","payload":{"id":"legacy-fixture","timestamp":"2026-09-01T09:00:00Z","cwd":"/workspace/legacy-project","source":"synthetic"}}"#,
        r#"{"timestamp":"2026-09-01T09:00:01Z","type":"response_item","payload":{"type":"message","id":"plugins-1","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>\nHere is a list of plugins that are available but not installed"}]}}"#,
        r#"{"timestamp":"2026-09-01T09:00:02Z","type":"response_item","payload":{"type":"message","id":"user-1","role":"user","content":[{"type":"input_text","text":"real user request"}]}}"#,
        r#"{"timestamp":"2026-09-01T09:00:03Z","type":"response_item","payload":{"type":"message","id":"assistant-1","role":"assistant","content":[{"type":"output_text","text":"real assistant response"}]}}"#,
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 1);

    // The already-fixed adapter strips the `<recommended_plugins>` item
    // at import time, so simulate a conversation imported *before* that
    // fix existed: restore the pollution directly onto the stored rows,
    // and collapse every chunk onto one timestamp — exactly the
    // pre-fix, pre-strip state the live corpus has for real rollouts
    // that predate both fixes.
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:legacy-fixture'",
                rusqlite::params![
                    "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                ],
            )?;
            conn.execute(
                "UPDATE chunks SET content = ?1 WHERE conversation_id = 'codex:legacy-fixture' AND seq = 0",
                rusqlite::params![
                    "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                ],
            )?;
            conn.execute(
                "UPDATE chunks SET timestamp = '2026-09-01T09:00:00Z'
                     WHERE conversation_id = 'codex:legacy-fixture'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let ids = storage
        .get_chunk_ids_for_conversation("codex:legacy-fixture")
        .unwrap();
    let seeded = storage.get_chunks_by_ids(&ids).unwrap();
    let seeded_timestamps: std::collections::BTreeSet<_> =
        seeded.iter().map(|c| c.timestamp.clone()).collect();
    assert_eq!(
        seeded_timestamps.len(),
        1,
        "setup: seeded state is collapsed"
    );

    // Restamp alone cannot fix this conversation — it is still polluted,
    // so it must be skipped, not mismatched.
    let restamp_before_strip = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert_eq!(restamp_before_strip.conversations_skipped_polluted, 1);
    assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_some());

    // The harness-strip batch reimports it directly, which both strips
    // the marker and re-stamps correct per-message timestamps as a side
    // effect of the normal import path.
    let strip = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
    assert_eq!(strip.reimported, 1);
    assert!(strip.done);

    let ids = storage
        .get_chunk_ids_for_conversation("codex:legacy-fixture")
        .unwrap();
    let repaired = storage.get_chunks_by_ids(&ids).unwrap();
    assert!(
        repaired
            .iter()
            .all(|c| !c.content.trim_start().starts_with("<recommended_plugins>")),
        "harness marker must be gone: {:?}",
        repaired.iter().map(|c| &c.content).collect::<Vec<_>>()
    );
    let repaired_timestamps: std::collections::BTreeSet<_> =
        repaired.iter().map(|c| c.timestamp.clone()).collect();
    assert!(
        repaired_timestamps.len() > 1,
        "reimport must restore distinct per-message timestamps, got {repaired_timestamps:?}"
    );
}

/// W3-R high review finding: pollution used to be detected only from a
/// conversation's cached `summary` (the first stored chunk's content). A
/// resumed or multi-turn Codex session can have a real, non-harness
/// first user message but still carry a harness-injected block
/// (`<recommended_plugins>` / `<environment_context>` / etc.) re-injected
/// on a *later* turn — imported before the harness-stripping fix
/// existed, that block is still sitting untouched in stored content.
/// `summary`-only detection never sees it: the restamp can never reach
/// `chunks_unmatched == 0` for that row (content mismatch forever), and
/// `harness_strip_batch` never queues it for reimport (it's not in the
/// polluted set), so it shows a harness preview forever too. Content-
/// based detection (any chunk, not just the first) must catch it. Must
/// fail at HEAD (pre-fix, `polluted_codex_conversation_ids` summary-only).
#[test]
fn polluted_conversation_ids_detects_a_harness_marker_injected_after_a_real_first_message() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-resumed.jsonl");
    let lines = [
        r#"{"timestamp":"2026-07-19T09:00:00Z","type":"session_meta","payload":{"id":"resumed-fixture","timestamp":"2026-07-19T09:00:00Z","cwd":"/workspace/resumed-project","source":"synthetic"}}"#,
        r#"{"timestamp":"2026-07-19T09:00:01Z","type":"response_item","payload":{"type":"message","id":"user-1","role":"user","content":[{"type":"input_text","text":"real user request to start the session"}]}}"#,
        r#"{"timestamp":"2026-07-19T09:00:02Z","type":"response_item","payload":{"type":"message","id":"assistant-1","role":"assistant","content":[{"type":"output_text","text":"real assistant response"}]}}"#,
        r#"{"timestamp":"2026-07-19T09:05:01Z","type":"response_item","payload":{"type":"message","id":"user-2","role":"user","content":[{"type":"input_text","text":"real follow-up request after resume"}]}}"#,
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 1);

    // The now-fixed adapter would strip a genuine `<recommended_plugins>`
    // item at import time, so simulate a conversation imported *before*
    // that fix existed: restore a harness block directly onto the third
    // (resumed-turn) stored chunk, while leaving `summary` (built from
    // the first, real, chunk) untouched. This is the crux of the
    // finding: the conversation's opening turn is real prose.
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET content = ?1
                     WHERE conversation_id = 'codex:resumed-fixture' AND seq = 2",
                rusqlite::params![
                    "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                ],
            )?;
            Ok(())
        })
        .unwrap();

    let (_, summary) = storage
        .codex_conversation_summaries()
        .unwrap()
        .into_iter()
        .find(|(id, _)| id == "codex:resumed-fixture")
        .expect("conversation must be present");
    assert!(
        summary
            .as_deref()
            .is_some_and(|s| !is_harness_context_item(s)),
        "setup: summary must stay real prose — this is exactly what \
             summary-only detection misses"
    );

    let polluted = polluted_codex_conversation_ids(&storage).unwrap();
    assert!(
        polluted.contains("codex:resumed-fixture"),
        "content-based detection must flag the later harness chunk even \
             though summary is real prose"
    );

    let harness_strip = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        harness_strip.reimported, 1,
        "harness_strip_batch must queue and reimport a conversation \
             flagged only by content, not summary"
    );
    assert!(harness_strip.done);

    let ids = storage
        .get_chunk_ids_for_conversation("codex:resumed-fixture")
        .unwrap();
    let repaired = storage.get_chunks_by_ids(&ids).unwrap();
    assert!(
        repaired
            .iter()
            .all(|c| !c.content.trim_start().starts_with("<recommended_plugins>")),
        "harness marker must be gone: {:?}",
        repaired.iter().map(|c| &c.content).collect::<Vec<_>>()
    );

    // The restamp must also reach done for this conversation now that
    // the harness-strip job has reimported it.
    let restamp = restamp_codex_chunk_timestamps(&storage).unwrap();
    assert_eq!(restamp.conversations_unmatched, 0);
    assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_some());
}

/// W3-R low review finding: `harness_strip_batch` used to ignore the
/// daemon's shutdown flag between conversations entirely, and the
/// daemon's shutdown sequence awaits it with no timeout — a launchctl
/// stop (default 20s `ExitTimeOut`) could SIGKILL mid-batch. Must break
/// between conversations (never mid-conversation) once shutdown is
/// requested, leaving the remainder polluted for the next batch to pick
/// up. Must fail at HEAD (pre-fix, no shutdown check in the loop).
#[test]
fn harness_strip_batch_stops_between_conversations_on_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let raw = std::fs::read_to_string(&source).unwrap();
    let mut conversation_ids = Vec::new();
    for n in 0..3 {
        let id = format!("shutdown-fixture-{n}");
        std::fs::write(
            root.path().join(format!("rollout-{n}.jsonl")),
            raw.replace("modern-fixture", &id),
        )
        .unwrap();
        conversation_ids.push(format!("codex:{id}"));
    }

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
    assert_eq!(import_stats.files_imported, 3);

    for conversation_id in &conversation_ids {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET summary = ?1 WHERE conversation_id = ?2",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed",
                        conversation_id,
                    ],
                )?;
                conn.execute(
                    "UPDATE chunks SET content = ?1 || content WHERE conversation_id = ?2 AND seq = 0",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed\n",
                        conversation_id,
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    // Already shut down before the batch even starts: not one
    // conversation should be reimported, and the polluted count must be
    // untouched.
    let shutdown = AtomicBool::new(true);
    let batch = harness_strip_batch(&engine, 10, &shutdown).unwrap();
    assert_eq!(
        batch.reimported, 0,
        "must not reimport any conversation once shutdown is requested"
    );
    assert_eq!(batch.polluted_considered, 3);
    assert_eq!(
        batch.polluted_remaining, 3,
        "nothing was reimported, so nothing should have cleared"
    );
    assert!(!batch.done);
    assert!(storage
        .get_meta(HARNESS_STRIP_V2_META_KEY)
        .unwrap()
        .is_none());

    // A later, non-shutdown call resumes and drains the backlog — this
    // is the resumability the job's doc comment promises.
    let resumed = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
    assert_eq!(resumed.reimported, 3);
    assert!(resumed.done);
}

/// The pass used to propagate the first per-file error, so one unreadable
/// or write-locked rollout cost every file behind it until the next
/// 30-minute daemon cycle.
#[cfg(unix)]
#[test]
fn one_unreadable_rollout_does_not_abort_the_pass() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    std::fs::copy(&source, root.path().join("rollout-good.jsonl")).unwrap();
    let unreadable = root.path().join("rollout-aaa-unreadable.jsonl");
    std::fs::copy(&source, &unreadable).unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let stats = import_changed_rollouts(&engine, root.path()).unwrap();

    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(stats.files_discovered, 2);
    assert_eq!(stats.files_imported, 1);
    assert_eq!(stats.failed, 1);
    assert_eq!(stats.vanished, 0);
    assert!(!storage
        .get_chunk_ids_for_conversation("codex:modern-fixture")
        .unwrap()
        .is_empty());
}

#[test]
fn modern_fixture_parses_messages_cwd_and_scrubs_csr_calls() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let parsed = parse_rollout(&path).unwrap().unwrap();
    let serialized = serde_json::to_string(&parsed.messages).unwrap();

    assert_eq!(parsed.project_name, "synthetic-project");
    assert_eq!(parsed.conversation_id, "codex:modern-fixture");
    assert!(serialized.contains("SYNTHETIC USER REQUEST"));
    assert!(serialized.contains("SYNTHETIC ASSISTANT RESPONSE"));
    assert!(serialized.contains("UNKNOWN TEXT RETAINED"));
    assert!(serialized.contains("NON-CSR TOOL OUTPUT RETAINED"));
    assert!(!serialized.contains("CSR QUERY SUPPRESSED"));
    assert!(!serialized.contains("CSR OUTPUT SUPPRESSED"));
    assert_eq!(parsed.schema_misses, 3);
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 2);
}

#[test]
fn codex_rollout_chunks_redact_secrets_and_report_counts() {
    let secret = format!("{}{}", "ghp_", "D".repeat(36));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-secret.jsonl");
    let line = |value: serde_json::Value| value.to_string();
    std::fs::write(
        &path,
        [
            line(serde_json::json!({"timestamp":"2026-10-14T12:00:00Z","type":"session_meta","payload":{"id":"secret","timestamp":"2026-10-14T12:00:00Z","cwd":"/workspace/secret-project"}})),
            line(serde_json::json!({"timestamp":"2026-10-14T12:00:01Z","type":"response_item","payload":{"type":"message","id":"u1","role":"user","content":[{"type":"input_text","text":format!("deploy with {secret} now")}]}})),
            line(serde_json::json!({"timestamp":"2026-10-14T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o1","call_id":"c1","output":format!("token={secret} printed")}})),
        ]
        .join("\n"),
    )
    .unwrap();

    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    let mut chunks = Vec::new();
    let outcome =
        stream_rollout_chunk_batches(&path, &metadata, ROLLOUT_EMBED_BATCH_SIZE, |batch, _| {
            chunks.extend_from_slice(batch);
            Ok(())
        })
        .unwrap();
    let all = chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!all.contains(&secret), "{all}");
    assert!(all.contains("[secret redacted: github_token]"), "{all}");
    assert_eq!(outcome.suppression.secrets.get("github_token"), 2);
}

/// Codex user, assistant and tool prose is never classified by formatter
/// text, whatever it says: only the structural CSR tool items go.
#[test]
fn codex_prose_and_tool_text_keep_every_byte_even_when_they_look_like_emissions() {
    const RECAP: &str = "recap [2m ago]: Fixed the importer. [[CSR:RECAP]]";
    const DEGRADED: &str = "CSR engine ready (degraded mode).";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-prose.jsonl");
    let line = |value: serde_json::Value| value.to_string();
    std::fs::write(
        &path,
        [
            line(serde_json::json!({"timestamp":"2026-08-06T12:00:00Z","type":"session_meta","payload":{"id":"prose","timestamp":"2026-08-06T12:00:00Z","cwd":"/workspace/prose-project"}})),
            line(serde_json::json!({"timestamp":"2026-08-06T12:00:01Z","type":"response_item","payload":{"type":"message","id":"u1","role":"user","content":[{"type":"input_text","text":format!("{RECAP}\n\nwhy does the importer drop this?")}]}})),
            line(serde_json::json!({"timestamp":"2026-08-06T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t1","call_id":"csr-call","name":"csr_reflect_on_past","input":"{\"query\":\"CSR QUERY SUPPRESSED\"}"}})),
            line(serde_json::json!({"timestamp":"2026-08-06T12:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o1","call_id":"csr-call","output":"CSR OUTPUT SUPPRESSED"}})),
            line(serde_json::json!({"timestamp":"2026-08-06T12:00:04Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t2","call_id":"grep-call","name":"Grep","input":"{\"pattern\":\"recap\"}"}})),
            line(serde_json::json!({"timestamp":"2026-08-06T12:00:05Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o2","call_id":"grep-call","output":format!("src/hooks/session_start.rs:68:{DEGRADED}\n{RECAP}")}})),
            line(serde_json::json!({"timestamp":"2026-08-06T12:00:06Z","type":"response_item","payload":{"type":"message","id":"a1","role":"assistant","content":[{"type":"output_text","text":DEGRADED}]}})),
        ]
        .join("\n"),
    )
    .unwrap();

    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    let mut chunks = Vec::new();
    stream_rollout_chunk_batches(&path, &metadata, ROLLOUT_EMBED_BATCH_SIZE, |batch, _| {
        chunks.extend_from_slice(batch);
        Ok(())
    })
    .unwrap();
    let all = chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    // user prose, tool output and assistant prose: every emission-shaped byte stays
    assert!(
        all.contains(&format!("{RECAP}\n\nwhy does the importer drop this?")),
        "{all}"
    );
    assert!(
        all.contains(&format!(
            "src/hooks/session_start.rs:68:{DEGRADED}\n{RECAP}"
        )),
        "{all}"
    );
    assert!(all.contains(DEGRADED), "{all}");
    // structure still decides: the CSR tool call and its result are gone
    assert!(!all.contains("CSR QUERY SUPPRESSED"));
    assert!(!all.contains("CSR OUTPUT SUPPRESSED"));
    let parsed = parse_rollout(&path).unwrap().unwrap();
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 2);
    assert_eq!(parsed.suppression.csr_hook_wrappers_scrubbed, 0);
}

/// Codex code mode (0.163+) calls CSR inside one `exec` script. A script
/// that calls only CSR is CSR's own traffic and goes, call and output; a
/// script that also runs another tool keeps its output.
#[test]
fn code_mode_csr_scripts_are_suppressed_and_mixed_scripts_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-code-mode.jsonl");
    let line = |value: serde_json::Value| value.to_string();
    let output = |text: &str| {
        serde_json::json!([
            {"type":"input_text","text":"Script completed\nWall time 1.5 seconds\nOutput:\n"},
            {"type":"input_text","text":text}
        ])
    };
    std::fs::write(
        &path,
        [
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:00Z","type":"session_meta","payload":{"id":"code-mode","timestamp":"2026-10-31T12:00:00Z","cwd":"/workspace/code-mode"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:01Z","type":"response_item","payload":{"type":"message","id":"u1","role":"user","content":[{"type":"input_text","text":"why did we drop Qdrant?"}]}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t1","call_id":"csr-exec","name":"exec","input":"const r=await tools.mcp__claude_self_reflect__csr_reflect_on_past({query:\"CSR QUERY SUPPRESSED\",limit:3});text(r);"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o1","call_id":"csr-exec","output":output("🎯 RESULTS: 3 matches CSR OUTPUT SUPPRESSED")}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:04Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t2","call_id":"mixed-exec","name":"exec","input":"text(await tools.mcp__claude_self_reflect__csr_why({target:\"x\"})); text(await tools.exec_command({cmd:\"cat MIXED.md\"}));"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:05Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o2","call_id":"mixed-exec","output":output("MIXED OUTPUT KEPT")}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:06Z","type":"response_item","payload":{"type":"message","id":"a1","role":"assistant","content":[{"type":"output_text","text":"We dropped Qdrant for one binary."}]}})),
        ]
        .join("\n"),
    )
    .unwrap();

    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    let mut chunks = Vec::new();
    stream_rollout_chunk_batches(&path, &metadata, ROLLOUT_EMBED_BATCH_SIZE, |batch, _| {
        chunks.extend_from_slice(batch);
        Ok(())
    })
    .unwrap();
    let all = chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!all.contains("CSR QUERY SUPPRESSED"), "{all}");
    assert!(!all.contains("CSR OUTPUT SUPPRESSED"), "{all}");
    assert!(all.contains("MIXED OUTPUT KEPT"), "{all}");
    assert!(all.contains("why did we drop Qdrant?"), "{all}");
    assert!(all.contains("We dropped Qdrant for one binary."), "{all}");
    let parsed = parse_rollout(&path).unwrap().unwrap();
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 2);
}

/// A script that calls CSR among other work keeps its output, minus each
/// line that is, whole, the printed JSON of one CSR result. Every other
/// line stays, CSR text included when it is printed any other way.
#[test]
fn a_mixed_script_drops_only_its_printed_csr_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-mixed.jsonl");
    let line = |value: serde_json::Value| value.to_string();
    let envelope = |marker: &str| {
        format!(
            "🎯 RESULTS: 1 matches (good relevance, top score: 0.497)\n⚡ PERFORMANCE: 12ms (1 collection searched)\n\n<search>\n  <meta>\n    <q>q</q>\n  </meta>\n  <results>\n    <r rank=\"1\"><excerpt>{marker}</excerpt></r>\n  </results>\n</search>"
        )
    };
    let result = |marker: &str| serde_json::json!({"content":[{"type":"text","text":envelope(marker)}],"isError":false});
    let notice = crate::binary_identity::replacement_notice("2026-10-30T00:37:29Z");
    let cut = serde_json::json!({"i":5,"value":result("CUT SHORT KEPT")}).to_string();
    let printed = [
        result("DIRECT CSR GONE").to_string(),
        serde_json::json!({"i":0,"status":"fulfilled","value":result("SETTLED CSR GONE")})
            .to_string(),
        serde_json::json!({"i":1,"result":{"status":"fulfilled","value":{"content":[{"type":"text","text":envelope("NOTICED CSR GONE")},{"type":"text","text":notice}]}}})
            .to_string(),
        serde_json::json!({"i":2,"status":"fulfilled","value":{"chunk_id":"ab12","exit_code":0,"output":"REAL SHELL OUTPUT KEPT"}})
            .to_string(),
        serde_json::json!({"note":"REAL NOTE BESIDE KEPT","value":result("BESIDE A NOTE")})
            .to_string(),
        serde_json::json!({"content":[{"type":"text","text":envelope("ERROR RESULT KEPT")}],"isError":true})
            .to_string(),
        serde_json::json!({"content":[{"type":"text","text":"OTHER SERVER TEXT KEPT"}]})
            .to_string(),
        serde_json::json!({"value":result("FIRST OF TWO KEPT"),"result":result("SECOND OF TWO")})
            .to_string(),
        // Data printed beside the result keeps the whole line.
        serde_json::json!({"memory":result("BESIDE GIT"),"gitExitCode":1,"hasChanges":true})
            .to_string(),
        serde_json::json!({"i":0,"status":"fulfilled","value":result("BESIDE A FLAG KEPT"),"cached":true})
            .to_string(),
        cut[..cut.len() - 40].to_string(),
        // A `csr_why` form takes any text on its last line: never removed.
        serde_json::json!({"content":[{"type":"text","text":"WHY: storage\n\nNo evidence chain found.\nreceipt: BUILD FAILED KEPT"}]})
            .to_string(),
    ]
    .join("\n");
    let output = serde_json::json!([
        {"type":"input_text","text":"Script completed\nWall time 1.5 seconds\nOutput:\n"},
        {"type":"input_text","text":printed}
    ]);
    let mixed = "const r = await Promise.allSettled([tools.mcp__claude_self_reflect__csr_reflect_on_past({query:\"q\"}), tools.exec_command({cmd:\"ls\"})]); r.forEach((x,i)=>text({i,...x}));";
    std::fs::write(
        &path,
        [
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:00Z","type":"session_meta","payload":{"id":"mixed","timestamp":"2026-11-02T12:00:00Z","cwd":"/workspace/mixed"}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:01Z","type":"response_item","payload":{"type":"message","id":"u1","role":"user","content":[{"type":"input_text","text":"what changed?"}]}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t1","call_id":"mixed-exec","name":"exec","input":mixed}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o1","call_id":"mixed-exec","output":output}})),
            // A plain-string output is scrubbed line by line too.
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:04Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t2","call_id":"string-exec","name":"exec","input":mixed}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:05Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o2","call_id":"string-exec","output":format!("{}\nPLAIN STRING SHELL KEPT", result("STRING CSR GONE"))}})),
            // No CSR call in the script: nothing is scrubbed.
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:06Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t3","call_id":"plain-exec","name":"exec","input":"text(await tools.exec_command({cmd:\"cat saved.json\"}));"}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:07Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o3","call_id":"plain-exec","output":result("NO CSR CALL KEPT").to_string()}})),
            // A CSR name only in a comment, `tools` aliased, or a regex
            // literal the reader cannot place: nothing is scrubbed.
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:08Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t4","call_id":"comment-exec","name":"exec","input":"// tools.mcp__claude_self_reflect__csr_why({})\ntext(JSON.parse((await tools.exec_command({cmd:\"cat f.json\"})).output));"}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:09Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o4","call_id":"comment-exec","output":result("COMMENT ONLY KEPT").to_string()}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:10Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t5","call_id":"alias-exec","name":"exec","input":"const t = tools; text(await t.mcp__claude_self_reflect__csr_why({})); text(await tools.mcp__claude_self_reflect__csr_why({})); text(await tools.exec_command({cmd:\"ls\"}));"}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:11Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o5","call_id":"alias-exec","output":result("ALIAS KEPT").to_string()}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:12Z","type":"response_item","payload":{"type":"custom_tool_call","id":"t6","call_id":"regex-exec","name":"exec","input":"text(await tools.mcp__claude_self_reflect__csr_reflect_on_past({query:\"q\"})); text([\"a\"].filter(x=>/a/.test(x)));"}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:13Z","type":"response_item","payload":{"type":"custom_tool_call_output","id":"o6","call_id":"regex-exec","output":result("REGEX ABSTAIN KEPT").to_string()}})),
            line(serde_json::json!({"timestamp":"2026-11-02T12:00:14Z","type":"response_item","payload":{"type":"message","id":"a1","role":"assistant","content":[{"type":"output_text","text":"Two files changed."}]}})),
        ]
        .join("\n"),
    )
    .unwrap();

    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    let mut chunks = Vec::new();
    stream_rollout_chunk_batches(&path, &metadata, ROLLOUT_EMBED_BATCH_SIZE, |batch, _| {
        chunks.extend_from_slice(batch);
        Ok(())
    })
    .unwrap();
    let all = chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for gone in [
        "DIRECT CSR GONE",
        "SETTLED CSR GONE",
        "NOTICED CSR GONE",
        "STRING CSR GONE",
    ] {
        assert!(!all.contains(gone), "{gone} leaked: {all}");
    }
    for kept in [
        "Script completed",
        "REAL SHELL OUTPUT KEPT",
        "REAL NOTE BESIDE KEPT",
        "ERROR RESULT KEPT",
        "OTHER SERVER TEXT KEPT",
        "FIRST OF TWO KEPT",
        "gitExitCode",
        "BESIDE A FLAG KEPT",
        "CUT SHORT KEPT",
        "PLAIN STRING SHELL KEPT",
        "NO CSR CALL KEPT",
        "BUILD FAILED KEPT",
        "COMMENT ONLY KEPT",
        "ALIAS KEPT",
        "REGEX ABSTAIN KEPT",
        "what changed?",
        "Two files changed.",
    ] {
        assert!(all.contains(kept), "{kept} lost: {all}");
    }
    let parsed = parse_rollout(&path).unwrap().unwrap();
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 4);
}

/// A CSR `exec` with no call id cannot take an id-less output with it.
#[test]
fn an_id_less_csr_call_keeps_every_id_less_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-no-ids.jsonl");
    let line = |value: serde_json::Value| value.to_string();
    std::fs::write(
        &path,
        [
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:00Z","type":"session_meta","payload":{"id":"no-ids","timestamp":"2026-10-31T12:00:00Z","cwd":"/workspace/no-ids"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:01Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(await tools.mcp__claude_self_reflect__csr_why({}));"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:02Z","type":"response_item","payload":{"type":"custom_tool_call_output","output":"CSR RESULT LEAKS"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:03Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(\"REAL NOTE\");"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:04Z","type":"response_item","payload":{"type":"custom_tool_call_output","output":"REAL NOTE KEPT"}})),
            // An explicit id equal to an old stand-in pairs with nothing.
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:05Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"codex-call-without-id","name":"exec","input":"text(await tools.exec_command({cmd:\"ls\"}));"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:06Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"codex-call-without-id","output":"EXPLICIT ID KEPT"}})),
            // An id-less mixed script marks no id, so a later output with
            // any id keeps a printed result line.
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:07Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"await tools.mcp__claude_self_reflect__csr_why({}); text(await tools.exec_command({cmd:\"ls\"}));"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:08Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"codex-call-without-id","name":"exec","input":"text(1);"}})),
            line(serde_json::json!({"timestamp":"2026-10-31T12:00:09Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"codex-call-without-id","output":serde_json::json!({"content":[{"type":"text","text":"🎯 RESULTS: 1 matches (good relevance, top score: 0.497)\n\n<search>\n  <meta>\n  </meta>\n  <results>\n    <r>FIXTURE LINE KEPT</r>\n  </results>\n</search>\n"}]}).to_string()}})),
        ]
        .join("\n"),
    )
    .unwrap();
    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    let mut chunks = Vec::new();
    stream_rollout_chunk_batches(&path, &metadata, ROLLOUT_EMBED_BATCH_SIZE, |batch, _| {
        chunks.extend_from_slice(batch);
        Ok(())
    })
    .unwrap();
    let all = chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("REAL NOTE KEPT"), "{all}");
    assert!(all.contains("EXPLICIT ID KEPT"), "{all}");
    assert!(all.contains("FIXTURE LINE KEPT"), "{all}");
    // Abstain: with no id, nothing proves which output is CSR's.
    assert!(all.contains("CSR RESULT LEAKS"), "{all}");
}

#[test]
fn legacy_fixture_parses_direct_messages_and_falls_back_unscoped() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_legacy.jsonl");
    let parsed = parse_rollout(&path).unwrap().unwrap();
    let serialized = serde_json::to_string(&parsed.messages).unwrap();

    assert_eq!(parsed.project_name, "_unscoped");
    assert_eq!(parsed.conversation_id, "codex:legacy-fixture");
    assert!(serialized.contains("LEGACY USER REQUEST"));
    assert!(serialized.contains("LEGACY ASSISTANT RESPONSE"));
    assert_eq!(parsed.schema_misses, 0);
}

#[test]
#[ignore = "acceptance receipt only: reads the operator's real ~/.codex/sessions corpus, never committed as a running gate"]
fn acceptance_receipt_01a0c5df_real_file_strips_cleanly() {
    let real = std::path::Path::new(
        "/Users/someone/.codex/sessions/2026/09/21/rollout-2026-09-21T14-29-18-01a0c5df-904a-7031-b2b8-dc71f64900bf.jsonl",
    );
    if !real.exists() {
        eprintln!("acceptance receipt skipped: fixture file not present on this machine");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("rollout-01a0c5df.jsonl");
    std::fs::copy(real, &target).unwrap();

    let metadata = scan_rollout_metadata(&target).unwrap().unwrap();
    eprintln!(
        "RECEIPT 01a0c5df summary (after fix): {:?}",
        metadata.summary
    );
    assert!(
        metadata
            .summary
            .as_deref()
            .unwrap_or_default()
            .starts_with("dont look at the literature install graphify"),
        "summary must be the real first user turn, not harness boilerplate: {:?}",
        metadata.summary
    );

    let storage = Arc::new(Storage::open_memory().unwrap());
    let engine = test_engine(storage.clone(), root.path());
    let stats = import_changed_rollouts(&engine, root.path()).unwrap();
    eprintln!("RECEIPT import stats: {stats:?}");
    assert_eq!(stats.files_imported, 1);

    let ids = storage
        .get_chunk_ids_for_conversation("codex:01a0c5df-904a-7031-b2b8-dc71f64900bf")
        .unwrap();
    let chunks = storage.get_chunks_by_ids(&ids).unwrap();
    let boilerplate: Vec<_> = chunks
        .iter()
        .filter(|c| {
            c.summary
                .as_deref()
                .unwrap_or_default()
                .starts_with("<recommended_plugins>")
        })
        .collect();
    eprintln!(
        "RECEIPT: {} chunks total, {} still boilerplate-summarized",
        chunks.len(),
        boilerplate.len()
    );
    assert!(
        boilerplate.is_empty(),
        "no chunk may carry the boilerplate summary after the fix"
    );
}

#[test]
fn codex_harness_context_items_are_stripped_and_counted() {
    // First user message is entirely Codex harness boilerplate (three
    // injected items); second is the real prompt. The harness-only
    // message must emit no chunk at all — not an empty one — and the
    // real prompt becomes the conversation's summary.
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-harness.jsonl");
    let session_meta = r#"{"type":"session_meta","payload":{"id":"harness-fixture","cwd":"/tmp/harness-project"}}"#;
    let injected = serde_json::json!({
        "type": "response_item",
        "timestamp": "2026-09-20T10:00:00Z",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "<recommended_plugins>\nHere is a list of plugins that are available but not installed"},
                {"type": "input_text", "text": "# AGENTS.md instructions\n\n<INSTRUCTIONS>do the agents thing</INSTRUCTIONS>"},
                {"type": "input_text", "text": "<environment_context>\ncwd: /tmp/harness-project\n</environment_context>"}
            ]
        }
    });
    let real = serde_json::json!({
        "type": "response_item",
        "timestamp": "2026-09-20T10:00:05Z",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "dont look at the literature install graphify"}
            ]
        }
    });
    std::fs::write(
        &path,
        format!(
            "{session_meta}\n{}\n{}\n",
            serde_json::to_string(&injected).unwrap(),
            serde_json::to_string(&real).unwrap(),
        ),
    )
    .unwrap();

    let parsed = parse_rollout(&path).unwrap().unwrap();
    let serialized = serde_json::to_string(&parsed.messages).unwrap();

    assert_eq!(
        parsed.harness_context_stripped, 3,
        "all 3 injected items in the first message must be counted"
    );
    assert!(!serialized.contains("<recommended_plugins>"));
    assert!(!serialized.contains("AGENTS.md instructions"));
    assert!(!serialized.contains("<environment_context>"));
    assert!(serialized.contains("dont look at the literature install graphify"));
    assert_eq!(
        parsed.schema_misses, 0,
        "a fully-stripped message is not a schema miss"
    );

    let user_messages: Vec<_> = parsed
        .messages
        .iter()
        .filter(|m| m.get("type").and_then(serde_json::Value::as_str) == Some("user"))
        .collect();
    assert_eq!(
        user_messages.len(),
        1,
        "the harness-only message must emit nothing at all: {user_messages:?}"
    );

    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    assert_eq!(
        metadata.summary.as_deref(),
        Some("dont look at the literature install graphify"),
        "summary must be the first REAL user message, not the harness boilerplate"
    );
}

fn write_rollout_fixture(path: &Path, session_meta: &serde_json::Value) {
    let real = serde_json::json!({
        "type": "response_item",
        "timestamp": "2026-09-20T10:00:05Z",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "real subagent prose"}
            ]
        }
    });
    std::fs::write(
        path,
        format!(
            "{}\n{}\n",
            serde_json::to_string(session_meta).unwrap(),
            serde_json::to_string(&real).unwrap(),
        ),
    )
    .unwrap();
}

#[test]
fn codex_session_meta_subagent_source_sets_is_subagent() {
    // W3-B1 review#2 finding: a `thread_spawn` child rollout's
    // `session_meta.payload.source` carries a `subagent` key. That must
    // flow through to `RolloutMetadata::is_subagent`, not stay
    // hard-coded false.
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-subagent.jsonl");
    let session_meta = serde_json::json!({
        "type": "session_meta",
        "payload": {
            "id": "01a0c5e1-904a-7031-b2b8-dc71f64900bf",
            "cwd": "/tmp/subagent-project",
            "source": {
                "subagent": {
                    "thread_spawn": {
                        "parent_thread_id": "01a0c5df-904a-7031-b2b8-dc71f64900bf"
                    }
                }
            }
        }
    });
    write_rollout_fixture(&path, &session_meta);

    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    assert!(
        metadata.is_subagent,
        "a session_meta with a `source.subagent` key must set is_subagent"
    );
}

#[test]
fn codex_session_meta_without_subagent_source_leaves_is_subagent_false() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-top-level.jsonl");
    let session_meta = serde_json::json!({
        "type": "session_meta",
        "payload": {
            "id": "01a0c5df-904a-7031-b2b8-dc71f64900bf",
            "cwd": "/tmp/subagent-project"
        }
    });
    write_rollout_fixture(&path, &session_meta);

    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    assert!(
        !metadata.is_subagent,
        "a top-level session_meta with no `source.subagent` key must not be flagged"
    );
}

#[test]
fn codex_subagent_rollout_chunks_are_marked_sidechain() {
    // The chunk-level effect of `is_subagent`: every chunk built from a
    // subagent rollout must carry `is_sidechain = true`, not the
    // previously hard-coded `false` — otherwise the Claude-side
    // sidechain exclusion in `get_recent_chunks` never applies to it and
    // a `thread_spawn` child takes its own recent-work slot next to its
    // parent.
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-subagent-chunks.jsonl");
    let session_meta = serde_json::json!({
        "type": "session_meta",
        "payload": {
            "id": "01a0c5e1-904a-7031-b2b8-dc71f64900bf",
            "cwd": "/tmp/subagent-project",
            "source": {"subagent": {"thread_spawn": {"parent_thread_id": "parent-id"}}}
        }
    });
    write_rollout_fixture(&path, &session_meta);

    let parsed = parse_rollout(&path).unwrap().unwrap();
    assert!(parsed.is_subagent);
    let chunks = rollout_chunks(&parsed);
    assert!(
        !chunks.is_empty(),
        "fixture must produce at least one chunk"
    );
    assert!(
        chunks.iter().all(|c| c.is_sidechain),
        "every chunk of a subagent rollout must be marked is_sidechain: {chunks:?}"
    );
}

#[test]
fn codex_harness_mixed_message_keeps_real_prose() {
    // One content item is harness-injected, the other is real prose in
    // the SAME message — only the injected item is dropped.
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rollout-mixed.jsonl");
    let session_meta =
        r#"{"type":"session_meta","payload":{"id":"mixed-fixture","cwd":"/tmp/mixed-project"}}"#;
    let mixed = serde_json::json!({
        "type": "response_item",
        "timestamp": "2026-09-20T11:00:00Z",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "<user_instructions>\nsome harness instructions\n</user_instructions>"},
                {"type": "input_text", "text": "real prose alongside the injected item"}
            ]
        }
    });
    std::fs::write(
        &path,
        format!(
            "{session_meta}\n{}\n",
            serde_json::to_string(&mixed).unwrap()
        ),
    )
    .unwrap();

    let parsed = parse_rollout(&path).unwrap().unwrap();
    let serialized = serde_json::to_string(&parsed.messages).unwrap();

    assert_eq!(parsed.harness_context_stripped, 1);
    assert!(!serialized.contains("user_instructions"));
    assert!(serialized.contains("real prose alongside the injected item"));
    assert_eq!(
        parsed.messages.len(),
        1,
        "the surviving prose must still emit exactly one message"
    );
}

#[test]
fn streamed_chunk_batches_match_single_pass_across_batch_boundaries() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
    let parsed = parse_rollout(&path).unwrap().unwrap();
    let expected = rollout_chunks(&parsed);
    let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
    let mut streamed = Vec::new();
    let outcome = stream_rollout_chunk_batches(&path, &metadata, 2, |batch, _| {
        assert!(batch.len() <= 2);
        streamed.extend_from_slice(batch);
        Ok(())
    })
    .unwrap();

    let signature = |chunk: &ConversationChunk| {
        (
            chunk.id.clone(),
            chunk.conversation_id.clone(),
            chunk.project_name.clone(),
            chunk.timestamp.clone(),
            chunk.content.clone(),
            chunk.message_count,
            chunk.summary.clone(),
            chunk.author,
            chunk.seq,
            chunk.is_sidechain,
        )
    };
    assert_eq!(
        streamed.iter().map(signature).collect::<Vec<_>>(),
        expected.iter().map(signature).collect::<Vec<_>>()
    );
    assert_eq!(outcome.schema_misses, parsed.schema_misses);
    assert_eq!(outcome.suppression, parsed.suppression);
}

#[test]
fn absent_rollout_directory_is_inert() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("does-not-exist");
    assert!(discover_rollouts(&absent, |_| Ok(None)).unwrap().is_empty());
}

#[test]
fn rollout_that_vanishes_before_open_is_a_soft_skip() {
    let dir = tempfile::tempdir().unwrap();
    let vanished = dir.path().join("rollout-vanished.jsonl");
    assert!(parse_rollout(&vanished).unwrap().is_none());
}
