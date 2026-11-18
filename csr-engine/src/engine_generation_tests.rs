//! Serving chunks from vector generations only: a verified serving generation
//! retires the index-v3 chunk graph and, after a grace period, the previous
//! generation; a generation that will not open is repaired by a new generation,
//! never by writing a chunk graph again.

use std::path::Path;

use tokio::sync::RwLock;

use super::tests::{vg_fixture, vg_vector, VG_N};
use super::{load_generation_served, load_reconciled_gated, regen_tick, RegenCfg, Tick};
use crate::index_gen::regen as rg;
use crate::index_gen::retire::{measure, storage_lines, sweep_with_grace};
use crate::index_gen::{self, GenManifest};
use crate::search::retire::{chunk_graph_files, retire_chunk_graph_until, StopAt};
use crate::search::{cleanup_stale_index_files, SearchEngine};
use crate::storage::Storage;

const GRACE: i64 = 3600;

fn manifest_json(index_dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(index_dir.join("manifest.json")).unwrap()).unwrap()
}

fn corrupt_ids(index_dir: &Path, generation: u64) {
    let ids = index_gen::ids_path(index_dir, &index_gen::basename(generation));
    let mut bytes = std::fs::read(&ids).unwrap();
    bytes[0] = b'X';
    std::fs::write(&ids, bytes).unwrap();
}

fn serving(generation: u64, since: i64) -> rg::RegenState {
    rg::RegenState {
        generation,
        serving_since_unix: since,
        ..Default::default()
    }
}

fn cfg() -> RegenCfg {
    RegenCfg {
        threshold: 10_000,
        floor: std::time::Duration::ZERO,
    }
}

/// One sweep at `now` of an engine serving `generation`.
fn sweep_at(
    storage: &Storage,
    index_dir: &Path,
    generation: u64,
    state: &mut rg::RegenState,
    now: i64,
) -> index_gen::retire::Swept {
    sweep_with_grace(storage, index_dir, Some(generation), state, now, GRACE)
}

/// The fixture's generation g1 serving, its chunk graph retired.
fn retired_fixture() -> (tempfile::TempDir, std::path::PathBuf, Storage) {
    let (tmp, index_dir, storage) = vg_fixture(true);
    let swept = sweep_at(&storage, &index_dir, 1, &mut serving(1, 1_000), 1_000);
    assert_eq!(swept.graph_files, 2);
    (tmp, index_dir, storage)
}

fn build_g2(storage: &Storage, index_dir: &Path) -> GenManifest {
    rg::build_next_generation(storage, index_dir, 2).unwrap()
}

#[test]
fn vector_gen_engine_serves_from_the_generation_when_the_watermark_verifies() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    let loaded = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert!(loaded.search.is_generation_served());
    assert_eq!((loaded.chunks_added, loaded.reflections_added), (0, 0));
    assert_eq!(loaded.search.chunk_count(), VG_N);
    assert!(loaded.search.has_reflection("refl-0"));
    let hit = &loaded.search.search_chunks(&vg_vector(4), 1, 0.0)[0];
    assert_eq!(hit.id, "chunk-4");
    assert!(hit.score > 0.99);
    // Reconciliation did not mark the (unused) chunk side for compaction.
    assert!(!crate::search::needs_compaction(&index_dir));
}

#[test]
fn a_verified_serving_generation_retires_the_chunk_graph_and_the_id_map() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    assert_eq!(chunk_graph_files(&index_dir).len(), 2);
    assert_eq!(
        manifest_json(&index_dir)["chunk_id_map"]
            .as_array()
            .unwrap()
            .len(),
        VG_N
    );
    let before = measure(&index_dir);
    assert!(before.dead_files == 2 && before.dead > 0);

    let mut state = serving(1, 1_000);
    let swept = sweep_at(&storage, &index_dir, 1, &mut state, 1_000);
    assert_eq!(swept.graph_files, 2);
    assert!(swept.graph_bytes > 0);

    assert!(chunk_graph_files(&index_dir).is_empty());
    let manifest = manifest_json(&index_dir);
    // The key stays, empty: a binary from before this change still parses it.
    assert_eq!(manifest["chunk_id_map"], serde_json::json!([]));
    assert_eq!(manifest["chunk_basename"], "chunks");
    assert_eq!(manifest["chunk_embeddings_expected"], 0);
    assert!(manifest["files"]
        .as_object()
        .unwrap()
        .keys()
        .all(|name| name.starts_with("reflections")));
    assert_eq!(measure(&index_dir).dead_files, 0);

    // The generation and the reflection side are untouched.
    assert!(index_gen::usearch_path(&index_dir, "chunks-g1").exists());
    let loaded = SearchEngine::load_with_generation(&index_dir, &storage, 1)
        .unwrap()
        .unwrap();
    assert!(loaded.is_generation_served());
    assert_eq!(loaded.chunk_count(), VG_N);
    assert!(loaded.has_reflection("refl-0"));
}

#[test]
fn nothing_is_retired_while_the_serving_generation_fails_its_full_check() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    corrupt_ids(&index_dir, 1);
    let mut state = serving(1, 1_000);
    let swept = sweep_at(&storage, &index_dir, 1, &mut state, 1_000);
    assert_eq!(swept.graph_files, 0);
    assert_eq!(chunk_graph_files(&index_dir).len(), 2);
    assert_eq!(
        manifest_json(&index_dir)["chunk_id_map"]
            .as_array()
            .unwrap()
            .len(),
        VG_N
    );
    // Nor when the engine serves no generation at all.
    let mut state = serving(0, 0);
    sweep_with_grace(&storage, &index_dir, None, &mut state, 1_000, GRACE);
    assert_eq!(chunk_graph_files(&index_dir).len(), 2);
}

#[test]
fn a_manifest_without_chunk_id_map_loads_the_reflection_side() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    let mut manifest = manifest_json(&index_dir);
    manifest.as_object_mut().unwrap().remove("chunk_id_map");
    std::fs::write(
        index_dir.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let loaded = SearchEngine::load_with_generation(&index_dir, &storage, 1)
        .unwrap()
        .unwrap();
    assert!(loaded.is_generation_served());
    assert!(loaded.has_reflection("refl-0"));
}

#[test]
fn an_older_binary_still_parses_the_retired_manifest_and_gets_an_empty_chunk_side() {
    /// The fields a binary from before this change requires of `manifest.json`.
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct OlderManifest {
        version: u32,
        files: std::collections::HashMap<String, serde_json::Value>,
        created_at: String,
        chunk_id_map: Vec<String>,
        reflection_id_map: Vec<String>,
        chunk_embeddings_expected: usize,
        reflection_embeddings_expected: usize,
        active_reflection_count: usize,
        chunk_basename: String,
        reflection_basename: String,
    }
    let (_tmp, index_dir, storage) = retired_fixture();
    let raw = std::fs::read(index_dir.join("manifest.json")).unwrap();
    let older: OlderManifest = serde_json::from_slice(&raw).unwrap();
    assert!(older.chunk_id_map.is_empty());
    assert_eq!(older.reflection_id_map.len(), 1);

    // Its own index-v3 path (a failed generation open) finds an empty chunk
    // side and no file to open: no crash, no half graph.
    let index_v3 = SearchEngine::load_from_disk(&index_dir, VG_N, 1)
        .unwrap()
        .unwrap();
    assert_eq!(index_v3.chunk_count(), 0);
    assert!(index_v3.has_reflection("refl-0"));
    // A reader that only serves the generation is unaffected.
    let served = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, false, None)
        .unwrap()
        .unwrap();
    assert!(served.search.is_generation_served());
    assert_eq!(served.search.chunk_count(), VG_N);
}

#[test]
fn the_kill_switch_builds_the_chunk_graph_from_sqlite_on_demand() {
    let (_tmp, index_dir, storage) = retired_fixture();
    let off = load_reconciled_gated(false, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert!(!off.search.is_generation_served());
    assert!(!off.search.is_awaiting_generation());
    assert_eq!(off.search.chunk_count(), VG_N);
    assert_eq!(off.chunks_added, VG_N);
    let hit = &off.search.search_chunks(&vg_vector(4), 1, 0.0)[0];
    assert_eq!(hit.id, "chunk-4");
}

#[test]
fn a_failed_open_does_not_load_the_old_chunk_graph() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    corrupt_ids(&index_dir, 1);
    let loaded = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert!(!loaded.search.is_generation_served());
    assert!(loaded.search.is_awaiting_generation());
    assert!(loaded.search.is_generation_mode());
    assert_eq!(loaded.search.chunk_count(), 0);
    assert_eq!((loaded.chunks_added, loaded.reflections_added), (0, 0));
    assert!(loaded.search.has_reflection("refl-0"));
    assert!(loaded
        .search
        .search_chunks(&vg_vector(4), 1, 0.0)
        .is_empty());
}

#[test]
fn a_damaged_reflection_file_does_not_cost_the_healthy_chunk_generation() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    let basename = manifest_json(&index_dir)["reflection_basename"]
        .as_str()
        .unwrap()
        .to_string();
    let data = index_dir.join(format!("{basename}.hnsw.data"));
    let mut bytes = std::fs::read(&data).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&data, bytes).unwrap();
    // The reflection side fails its hash; the generation is fine.
    assert!(SearchEngine::load_with_generation(&index_dir, &storage, 1).is_err());

    let loaded = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.search.serving_generation(), Some(1));
    assert!(!loaded.search.is_awaiting_generation());
    assert_eq!(loaded.search.chunk_count(), VG_N);
    // The reflection side starts empty and reconciliation fills it from SQLite.
    assert_eq!(loaded.reflections_added, 1);
    assert!(loaded.search.has_reflection("refl-0"));

    // Nothing for the daemon to repair: no generation is built.
    let search = RwLock::new(loaded.search);
    let built = std::cell::Cell::new(0);
    let builder = || {
        built.set(built.get() + 1);
        rg::build_next_generation(&storage, &index_dir, 2)
    };
    let tick = regen_tick(&storage, &search, &index_dir, &cfg(), false, &builder).unwrap();
    assert_eq!((tick, built.get()), (Tick::Idle, 0));
}

#[test]
fn a_lock_timeout_is_reported_by_the_caller_not_served_as_a_failed_generation() {
    use fs2::FileExt;
    let (_tmp, index_dir, storage) = vg_fixture(true);
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(index_dir.join("index.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&held).unwrap();
    let budget = std::time::Duration::from_millis(30);
    assert!(load_generation_served(true, &storage, &index_dir, 1, budget).is_none());
    drop(held);
    let served = load_generation_served(true, &storage, &index_dir, 1, budget).unwrap();
    assert!(served.is_generation_served());
}

#[test]
fn a_failed_open_is_repaired_by_a_new_generation_and_writes_no_chunk_graph() {
    let (_tmp, index_dir, storage) = retired_fixture();
    corrupt_ids(&index_dir, 1);
    let loaded = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    let mut engine = loaded.search;
    assert!(engine.is_awaiting_generation());
    // Imports during the repair land in SQLite only: no graph, nothing dirty.
    engine.insert_chunk("late".into(), vg_vector(30));
    engine.remove_chunk("chunk-3");
    assert!(!engine.is_chunk_dirty());
    assert_eq!(engine.chunk_count(), 0);
    engine.dump_to_disk(&index_dir, VG_N, 1).unwrap();
    assert!(chunk_graph_files(&index_dir).is_empty());

    let search = RwLock::new(engine);
    let builder = || rg::build_next_generation(&storage, &index_dir, 2);
    let tick = regen_tick(&storage, &search, &index_dir, &cfg(), false, &builder).unwrap();
    assert_eq!(tick, Tick::Regenerated(2));
    {
        let index = search.blocking_read();
        assert_eq!(index.serving_generation(), Some(2));
        assert!(!index.is_awaiting_generation());
        assert_eq!(index.chunk_count(), VG_N);
        assert_eq!(index.search_chunks(&vg_vector(4), 1, 0.0)[0].id, "chunk-4");
    }
    assert!(chunk_graph_files(&index_dir).is_empty());
    assert!(!rg::read_state(&index_dir).unwrap().repair_pending);
}

#[test]
fn a_rebuild_request_on_an_engine_awaiting_a_generation_builds_a_generation() {
    let (_tmp, index_dir, storage) = retired_fixture();
    corrupt_ids(&index_dir, 1);
    let loaded = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    // `Engine::rebuild_search_index` takes the generation branch on exactly this.
    assert!(loaded.search.is_generation_mode());
    let search = RwLock::new(loaded.search);
    let builder = || rg::build_next_generation(&storage, &index_dir, 2);
    let forced = RegenCfg {
        threshold: 1,
        floor: std::time::Duration::ZERO,
    };
    assert!(matches!(
        regen_tick(&storage, &search, &index_dir, &forced, true, &builder).unwrap(),
        Tick::Regenerated(_)
    ));
    assert!(chunk_graph_files(&index_dir).is_empty());
}

/// The gate in `Engine::rebuild_search_index`: an engine that is awaiting a
/// generation takes the generation branch (a build child, here the test seam
/// that builds in process), never the index-v3 rebuild that streams every
/// vector into a new chunk graph and dumps it. On the old gate the seam is
/// never called and a chunk graph lands in `index_dir`.
#[tokio::test]
async fn a_rebuild_request_through_the_engine_takes_the_generation_branch_and_writes_no_graph() {
    let (_tmp, index_dir, storage) = retired_fixture();
    corrupt_ids(&index_dir, 1);
    let loaded = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert!(loaded.search.is_awaiting_generation());
    assert!(!loaded.search.is_generation_served());
    let storage = std::sync::Arc::new(storage);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let (storage, index_dir, calls) = (storage.clone(), index_dir.clone(), calls.clone());
        *rg::BUILD_CHILD_HOOK.lock().unwrap() = Some(Box::new(move |_db_path| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            rg::build_next_generation(&storage, &index_dir, 2)
        }));
    }
    let engine = super::Engine::from_parts_with_index_dir(
        storage.clone(),
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::disabled()),
        std::sync::Arc::new(RwLock::new(loaded.search)),
        index_dir.clone(),
        index_dir.clone(),
    );
    let outcome = engine.rebuild_search_index().await;
    *rg::BUILD_CHILD_HOOK.lock().unwrap() = None;
    outcome.unwrap();
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the rebuild request must start one generation build"
    );
    assert!(
        chunk_graph_files(&index_dir).is_empty(),
        "no chunk graph may be written for a rebuild request"
    );
    // The regeneration tick followed the new generation: chunk search is back.
    let serving = engine.search().read().await.serving_generation();
    assert_eq!(serving, Some(2));
}

#[test]
fn a_failed_open_rolls_back_to_the_previous_generation() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    build_g2(&storage, &index_dir);
    assert_eq!(rg::list_generations(&index_dir), vec![1, 2]);
    corrupt_ids(&index_dir, 2);
    let loaded = load_reconciled_gated(true, &storage, &index_dir, VG_N, 1, true, None)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.search.serving_generation(), Some(1));
    assert!(!loaded.search.is_awaiting_generation());
    assert_eq!(loaded.search.chunk_count(), VG_N);
    let hit = &loaded.search.search_chunks(&vg_vector(4), 1, 0.0)[0];
    assert_eq!(hit.id, "chunk-4");
    assert!(loaded.search.has_reflection("refl-0"));
}

#[test]
fn the_previous_generation_survives_the_grace_period_and_goes_after_it() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    build_g2(&storage, &index_dir);
    let mut state = serving(2, 1_000);

    // Inside the grace period: the chunk graph goes, the rollback stays.
    let early = sweep_at(&storage, &index_dir, 2, &mut state, 1_000 + GRACE - 1);
    assert_eq!(early.graph_files, 2);
    assert!(early.pruned.is_empty());
    assert_eq!(rg::list_generations(&index_dir), vec![1, 2]);

    // Past it, but the current generation is being repaired: still kept.
    state.repair_pending = true;
    let repairing = sweep_at(&storage, &index_dir, 2, &mut state, 1_000 + 2 * GRACE);
    assert!(repairing.pruned.is_empty());
    assert_eq!(rg::list_generations(&index_dir), vec![1, 2]);

    // Past it, and the current generation fails its full check: still kept.
    state.repair_pending = false;
    corrupt_ids(&index_dir, 2);
    let damaged = sweep_at(&storage, &index_dir, 2, &mut state, 1_000 + 2 * GRACE);
    assert!(damaged.pruned.is_empty());
    assert_eq!(rg::list_generations(&index_dir), vec![1, 2]);
}

#[test]
fn the_previous_generation_is_pruned_once_the_grace_period_has_passed() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    build_g2(&storage, &index_dir);
    let mut state = serving(2, 1_000);
    let swept = sweep_at(&storage, &index_dir, 2, &mut state, 1_000 + GRACE);
    assert_eq!(swept.pruned, vec![1]);
    assert_eq!(rg::list_generations(&index_dir), vec![2]);
    for path in [
        index_gen::usearch_path(&index_dir, "chunks-g1"),
        index_gen::ids_path(&index_dir, "chunks-g1"),
        index_gen::manifest_path(&index_dir, "chunks-g1"),
        index_gen::stamp_path(&index_dir, "chunks-g1"),
    ] {
        assert!(!path.exists(), "{}", path.display());
    }
    assert!(index_gen::usearch_path(&index_dir, "chunks-g2").exists());
    // And the survivor still serves.
    let loaded = SearchEngine::load_with_generation(&index_dir, &storage, 1)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.serving_generation(), Some(2));
}

#[test]
fn the_grace_clock_starts_when_a_generation_begins_serving() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    build_g2(&storage, &index_dir);
    // The state says g1 served until now; g2 begins serving at t=5000.
    let mut state = serving(1, 100);
    sweep_at(&storage, &index_dir, 2, &mut state, 5_000);
    assert_eq!(state.serving_since_unix, 5_000);
    assert_eq!(rg::list_generations(&index_dir), vec![1, 2]);
    // Seen again by a later tick (state.generation catches up): unchanged.
    state.generation = 2;
    sweep_at(&storage, &index_dir, 2, &mut state, 5_100);
    assert_eq!(state.serving_since_unix, 5_000);
}

#[test]
fn an_interrupted_retirement_is_finished_by_the_next_start() {
    for stop in [
        StopAt::BeforeManifest,
        StopAt::AfterManifest,
        StopAt::AfterFirstFile,
    ] {
        let (_tmp, index_dir, storage) = vg_fixture(true);
        retire_chunk_graph_until(&index_dir, stop).unwrap();
        let files = chunk_graph_files(&index_dir).len();
        let map = manifest_json(&index_dir)["chunk_id_map"]
            .as_array()
            .unwrap()
            .len();
        match stop {
            StopAt::BeforeManifest => assert_eq!((files, map), (2, VG_N), "{stop:?}"),
            StopAt::AfterManifest => assert_eq!((files, map), (2, 0), "{stop:?}"),
            _ => assert_eq!((files, map), (1, 0), "{stop:?}"),
        }
        // Any reader in between gets a loadable state, never an error.
        let reader = SearchEngine::load_from_disk(&index_dir, VG_N, 1)
            .unwrap()
            .unwrap();
        let before_manifest = stop == StopAt::BeforeManifest;
        assert_eq!(reader.chunk_count(), if before_manifest { VG_N } else { 0 });

        // Start: engine startup runs the stale-file cleanup under index.lock.
        cleanup_stale_index_files(&index_dir);
        let left = chunk_graph_files(&index_dir).len();
        assert_eq!(left, if before_manifest { 2 } else { 0 }, "{stop:?}");

        // The next tick finishes whatever the cleanup does not own.
        let mut state = serving(1, 1_000);
        sweep_at(&storage, &index_dir, 1, &mut state, 1_000);
        assert!(chunk_graph_files(&index_dir).is_empty(), "{stop:?}");
        let map = &manifest_json(&index_dir)["chunk_id_map"];
        assert_eq!(map, &serde_json::json!([]));
        let loaded = SearchEngine::load_with_generation(&index_dir, &storage, 1)
            .unwrap()
            .unwrap();
        assert!(loaded.has_reflection("refl-0"), "{stop:?}");
    }
}

#[test]
fn retirement_tolerates_a_graph_file_that_is_already_gone() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    let first = chunk_graph_files(&index_dir).remove(0);
    std::fs::remove_file(first).unwrap();
    let mut state = serving(1, 1_000);
    let swept = sweep_at(&storage, &index_dir, 1, &mut state, 1_000);
    assert_eq!(swept.graph_files, 1);
    assert!(chunk_graph_files(&index_dir).is_empty());
    // A second sweep has nothing left to do and costs no hash check.
    let again = sweep_at(&storage, &index_dir, 1, &mut state, 1_001);
    assert_eq!(again, index_gen::retire::Swept::default());
}

#[test]
fn the_storage_report_names_each_purpose_and_what_is_dead() {
    let (_tmp, index_dir, storage) = vg_fixture(true);
    build_g2(&storage, &index_dir);
    let before = storage_lines(&index_dir).join("\n");
    assert!(!before.contains("dead: none"), "{before}");

    let mut state = serving(2, 1_000);
    sweep_at(&storage, &index_dir, 2, &mut state, 1_000);
    rg::write_state(&index_dir, &state).unwrap();
    let report = storage_lines(&index_dir);
    let text = report.join("\n");
    assert!(report[0].starts_with("index-v3 storage: "), "{text}");
    assert!(text.contains("serving: g2"), "{text}");
    assert!(text.contains("previous: g1"), "{text}");
    assert!(text.contains("kept until "), "{text}");
    assert!(text.contains("reflections: "), "{text}");
    assert!(text.contains("dead: none"), "{text}");

    let mut state = serving(2, 1_000);
    sweep_at(&storage, &index_dir, 2, &mut state, 1_000 + GRACE);
    rg::write_state(&index_dir, &state).unwrap();
    let text = storage_lines(&index_dir).join("\n");
    assert!(text.contains("previous: none kept"), "{text}");
    assert!(!text.contains("previous: g1"), "{text}");
}

#[test]
fn the_storage_report_says_so_when_chunk_search_is_waiting_for_a_generation() {
    let (_tmp, index_dir, _storage) = retired_fixture();
    let state = rg::RegenState {
        generation: 0,
        repair_pending: true,
        ..Default::default()
    };
    rg::write_state(&index_dir, &state).unwrap();
    let text = storage_lines(&index_dir).join("\n");
    assert!(text.contains("serving: none"), "{text}");
    assert!(text.contains("keyword-only"), "{text}");
}
