//! `status --compact` is the statusline's refresh: cheap state only.

use super::*;
use crate::search::heavy_loads;

fn seeded_db() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("csr-engine.db");
    let projects_dir = dir.path().join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    let storage = Storage::open(&db_path).unwrap();
    for i in 0..12 {
        let chunk = crate::import::ConversationChunk {
            id: format!("c{i}"),
            conversation_id: format!("conv-{}", i % 4),
            project_name: format!("p{}", i % 2),
            timestamp: "2026-10-12T00:00:00Z".into(),
            content: "x".repeat(300),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
    }
    (dir, db_path, projects_dir)
}

fn gather(db_path: &Path, projects_dir: &Path, deep: bool, scope: Scope) -> StatusReport {
    gather_status_with(
        db_path,
        projects_dir,
        deep,
        scope,
        Some(&test_run_dir(db_path)),
        Some(&test_settings(db_path)),
    )
    .unwrap()
}

/// The guard on the statusline's latency: it runs on every refresh, so it may
/// never open the vector index or load the embedding model (6 s on a large
/// corpus). The counter is real: the control below shows the index loader
/// moves it.
#[test]
fn compact_status_never_loads_the_index_or_the_model() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (dir, db_path, projects_dir) = seeded_db();

    let before = heavy_loads::count();
    let _ = crate::search::SearchEngine::load_from_disk(dir.path(), 0, 0);
    assert_eq!(
        heavy_loads::count(),
        before + 1,
        "control: the index loader must be counted, or this guard proves nothing"
    );

    let before = heavy_loads::count();
    let report = gather(&db_path, &projects_dir, false, Scope::Compact);
    print_compact(&report);
    assert_eq!(
        heavy_loads::count(),
        before,
        "`status --compact` opened the vector index or loaded the model"
    );
    assert_eq!(report.conversations, 4);
    assert_eq!(report.projects, 2);
}

/// The statusline never runs the integrity check, even when asked.
#[test]
fn compact_status_ignores_deep() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();
    // Compact first: nothing has cached a verdict yet, so any `Some` would
    // have come from running the check.
    assert_eq!(
        gather(&db_path, &projects_dir, true, Scope::Compact).healthy,
        None,
        "deep must not run the integrity check on the compact path"
    );
    assert_eq!(
        gather(&db_path, &projects_dir, true, Scope::Full).healthy,
        Some(true)
    );
}

/// The provenance segment comes from the daemon's stored measurement: absent
/// before the daemon has made one (nothing is scanned to fill the gap), and
/// the same text the live gather prints once it has.
#[test]
fn compact_status_reads_provenance_from_the_daemon_row_and_prints_the_same_line() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();

    let before = gather(&db_path, &projects_dir, false, Scope::Compact);
    assert_eq!(before.provenance_coverage.chunks_total, 0);
    assert!(!format_compact(&before, 0).contains("prov "));

    let storage = Storage::open(&db_path).unwrap();
    refresh_provenance_cache(&storage).unwrap();
    assert!(storage
        .get_meta(PROVENANCE_CACHE_META_KEY)
        .unwrap()
        .is_some());
    drop(storage);

    let compact = gather(&db_path, &projects_dir, false, Scope::Compact);
    let full = gather(&db_path, &projects_dir, false, Scope::Full);
    assert_eq!(compact.provenance_coverage.chunks_total, 12);
    let line = format_compact(&compact, 0);
    assert!(line.contains("| prov "), "{line}");
    assert_eq!(
        line,
        format_compact(&full, 0),
        "compact and full gathers must print the same statusline"
    );
}

/// A garbled row reads as "not measured", never as an error or a zero.
#[test]
fn compact_status_treats_a_garbled_provenance_row_as_not_measured() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();
    let storage = Storage::open(&db_path).unwrap();
    storage
        .set_meta(PROVENANCE_CACHE_META_KEY, "{not json")
        .unwrap();
    drop(storage);
    let report = gather(&db_path, &projects_dir, false, Scope::Compact);
    assert_eq!(
        report.provenance_coverage,
        ProvenanceCoverageStatus::default()
    );
}
