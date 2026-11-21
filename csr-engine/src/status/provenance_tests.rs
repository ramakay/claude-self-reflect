//! Plain `status` serves the daemon's provenance measurement with its age;
//! `--deep` measures live.

use super::compact_tests::{gather, seeded_db};
use super::*;

/// The seeded store holds 12 chunks. The stored row says 99 so a test can tell
/// which source the report read.
const STORED_TOTAL: i64 = 99;
const LIVE_TOTAL: i64 = 12;

fn store_row(db_path: &Path, measured_at: &str) {
    let storage = Storage::open(db_path).unwrap();
    let row = serde_json::json!({
        "chunks_with_spans": 90, "chunks_unknown": 7, "chunks_total": STORED_TOTAL,
        "tool_result_share_mean": 0.5, "source_missing": 3,
        "source_unparsed": 2, "source_unmatched": 1, "measured_at": measured_at,
    });
    storage
        .set_meta(PROVENANCE_CACHE_META_KEY, &row.to_string())
        .unwrap();
}

fn measured(age: chrono::Duration) -> String {
    (chrono::Utc::now() - age).to_rfc3339()
}

/// The slow part of status is the two provenance scans. A plain full status
/// must read the stored row, name when it was measured, and still fill the
/// per-family artifact histogram from the small tables. The old gather ran
/// both scans on every call, so it reported the live 12 here.
#[test]
fn plain_status_serves_the_stored_measurement_and_prints_its_age() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();
    let stamp = measured(chrono::Duration::minutes(40));
    store_row(&db_path, &stamp);

    let report = gather(&db_path, &projects_dir, false, Scope::Full);
    let prov = &report.provenance_coverage;
    assert_eq!(prov.chunks_total, STORED_TOTAL);
    assert_eq!(prov.chunks_with_spans, 90);
    assert_eq!(prov.source_missing, 3);
    let ProvenanceBasis::Stored {
        measured_at,
        age_secs,
    } = &prov.basis
    else {
        panic!("expected a stored basis, got {:?}", prov.basis);
    };
    assert_eq!(measured_at, &stamp);
    assert!((2400..2460).contains(age_secs), "age {age_secs}");
    let note = prov.basis.note().expect("the age is printed");
    assert!(note.contains(&stamp), "{note}");
    assert!(
        prov.artifacts.iter().any(|f| f.kind == "reflection"),
        "artifact families stay live"
    );
}

/// `--deep` keeps its meaning: exact counts, never the stored row, and no age
/// note because nothing was cached.
#[test]
fn deep_status_measures_live_and_reports_no_age() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();
    store_row(&db_path, &measured(chrono::Duration::minutes(5)));

    let report = gather(&db_path, &projects_dir, true, Scope::Full);
    let prov = &report.provenance_coverage;
    assert_eq!(prov.chunks_total, LIVE_TOTAL);
    assert_eq!(prov.basis, ProvenanceBasis::Live);
    assert_eq!(prov.basis.note(), None);
}

/// Plain status never scans the store. With the daemon stopped the last row is
/// served however old it is, with its age; a missing, future-dated or
/// unparsable row reads as unavailable (zero counts) rather than triggering the
/// six-second live scan. The seeded store holds 12 chunks, so a total of 0
/// proves no scan ran.
#[test]
fn plain_status_never_scans_it_serves_the_last_row_or_says_unavailable() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();

    let absent = gather(&db_path, &projects_dir, false, Scope::Full);
    assert_eq!(absent.provenance_coverage.chunks_total, 0);
    assert_eq!(
        absent.provenance_coverage.basis,
        ProvenanceBasis::Unavailable
    );
    assert!(absent.provenance_coverage.basis.note().is_some());

    store_row(&db_path, &measured(chrono::Duration::hours(30)));
    let stale = gather(&db_path, &projects_dir, false, Scope::Full);
    assert_eq!(stale.provenance_coverage.chunks_total, STORED_TOTAL);
    assert!(matches!(
        stale.provenance_coverage.basis,
        ProvenanceBasis::Stored { age_secs, .. } if age_secs >= 30 * 3600
    ));

    store_row(&db_path, &measured(-chrono::Duration::hours(2)));
    let future = gather(&db_path, &projects_dir, false, Scope::Full);
    assert_eq!(future.provenance_coverage.chunks_total, 0);
    assert_eq!(
        future.provenance_coverage.basis,
        ProvenanceBasis::Unavailable
    );

    store_row(&db_path, "yesterday-ish");
    let unparsable = gather(&db_path, &projects_dir, false, Scope::Full);
    assert_eq!(unparsable.provenance_coverage.chunks_total, 0);
    assert_eq!(
        unparsable.provenance_coverage.basis,
        ProvenanceBasis::Unavailable
    );
}

/// The provenance block prints exactly the keys the live block always printed,
/// whether it was served, unavailable or measured by `--deep`. The age is
/// reported on stderr, not in the JSON.
#[test]
fn provenance_block_keeps_every_json_key() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();
    let old_keys = [
        "artifacts",
        "chunks_total",
        "chunks_unknown",
        "chunks_with_spans",
        "source_missing",
        "source_unmatched",
        "source_unparsed",
        "tool_result_share_mean",
    ];
    let keys = |deep: bool| -> Vec<String> {
        let report = gather(&db_path, &projects_dir, deep, Scope::Full);
        let json = serde_json::to_value(&report.provenance_coverage).unwrap();
        let mut keys: Vec<String> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };

    assert_eq!(keys(false), old_keys, "unavailable");
    store_row(&db_path, &measured(chrono::Duration::minutes(1)));
    assert_eq!(keys(true), old_keys, "deep");
    assert_eq!(keys(false), old_keys, "served");
}

/// The statusline and the plain report read the same row, so they print the
/// same `prov` segment.
#[test]
fn plain_and_compact_status_agree_on_the_stored_row() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let (_dir, db_path, projects_dir) = seeded_db();
    store_row(&db_path, &measured(chrono::Duration::minutes(10)));
    let full = gather(&db_path, &projects_dir, false, Scope::Full);
    let compact = gather(&db_path, &projects_dir, false, Scope::Compact);
    assert_eq!(
        format_compact(&full, 0),
        format_compact(&compact, 0),
        "same row, same line"
    );
    assert_eq!(
        full.provenance_coverage.chunks_total,
        compact.provenance_coverage.chunks_total
    );
}
