//! The `codemap` block of `csr-engine status`.

use super::*;
use crate::codemap::status::{CodemapStatus, RepoStatus, StatusTotals, ViewStatus};
use crate::codemap::testutil::{map_fixture, scan_repo_into};

fn view(scanned_at: Option<i64>, status: &str, truncated: bool) -> ViewStatus {
    ViewStatus {
        path: "/w/v".into(),
        branch: Some("b".into()),
        head: None,
        files: 1,
        dirty: 0,
        untracked: 0,
        truncated,
        scanned_at,
        age_secs: None,
        status: status.into(),
        scan_ms: None,
    }
}

fn repo(enabled: bool, last_poll_at: Option<i64>, views: Vec<ViewStatus>) -> RepoStatus {
    RepoStatus {
        common_dir: "/r/.git".into(),
        main_path: None,
        source: "cli".into(),
        enabled,
        status: "ok".into(),
        last_poll_at,
        views,
    }
}

#[test]
fn lag_is_measured_from_the_oldest_poll_of_an_enabled_repo_and_the_oldest_scan() {
    let report = CodemapStatus {
        initialised: true,
        disabled: false,
        extractor_version: "x".into(),
        repos: vec![
            repo(
                true,
                Some(100),
                vec![view(Some(90), "ok", false), view(Some(30), "ok", true)],
            ),
            repo(true, Some(40), vec![view(None, "error: boom", false)]),
            // Removed repos do not count, however old their poll is.
            repo(false, Some(1), vec![view(Some(5), "ok", false)]),
            repo(true, None, vec![]),
        ],
        totals: StatusTotals {
            repos: 4,
            views: 4,
            view_file_pairs: 7,
            unique_blobs: 5,
            defs: 11,
            fts_rows: 11,
            truncated_views: 1,
            queue_depth: 3,
        },
    };
    let block = CodemapStatusBlock::from_report(&report, 200);
    assert_eq!(
        block,
        CodemapStatusBlock {
            initialised: true,
            disabled: false,
            repos: 3,
            views: 4,
            view_file_pairs: 7,
            unique_blobs: 5,
            defs: 11,
            queue_depth: 3,
            views_truncated: 1,
            views_in_error: 1,
            repos_never_polled: 1,
            poll_lag_secs: Some(160),
            // The removed repo's view (scanned at 5) still counts as a view: the
            // oldest scan is over every view the map holds.
            scan_lag_secs: Some(195),
        }
    );
    // A clock that ran backwards never yields a negative lag.
    let skewed = CodemapStatusBlock::from_report(&report, 0);
    assert_eq!(skewed.poll_lag_secs, Some(0));
    assert_eq!(skewed.scan_lag_secs, Some(0));
}

#[test]
fn an_uninitialised_report_folds_to_the_default_block() {
    let report = CodemapStatus {
        initialised: false,
        disabled: false,
        extractor_version: "x".into(),
        repos: vec![],
        totals: StatusTotals::default(),
    };
    assert_eq!(
        CodemapStatusBlock::from_report(&report, 10),
        CodemapStatusBlock::default()
    );
    assert!(CodemapStatusBlock::default().text_lines().is_empty());
}

#[test]
fn text_lines_show_the_counts_the_lag_and_the_kill_switch() {
    let block = CodemapStatusBlock {
        initialised: true,
        repos: 2,
        views: 11,
        view_file_pairs: 4138,
        unique_blobs: 1284,
        defs: 61000,
        queue_depth: 2,
        views_truncated: 1,
        views_in_error: 0,
        repos_never_polled: 0,
        poll_lag_secs: Some(12),
        scan_lag_secs: None,
        disabled: false,
    };
    assert_eq!(
        block.text_lines(),
        vec![
            "Code map: 2 repos, 11 views, 4138 view-file pairs, 1284 unique blobs, 61000 defs"
                .to_string(),
            "Queue 2, truncated views 1, views in error 0, repos never polled 0".to_string(),
            "Freshness lag: oldest poll 12s, oldest scan n/a".to_string(),
        ]
    );
    let off = CodemapStatusBlock {
        disabled: true,
        ..block
    };
    assert!(off.text_lines()[0].ends_with(" [off: CSR_NO_CODEMAP=1]"));
    let off_empty = CodemapStatusBlock {
        disabled: true,
        ..CodemapStatusBlock::default()
    };
    assert_eq!(
        off_empty.text_lines(),
        vec!["Code map: off (CSR_NO_CODEMAP=1)".to_string()]
    );
}

fn temp_db() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let projects_dir = dir.path().join("projects");
    std::fs::create_dir_all(&projects_dir).unwrap();
    (dir, db_path, projects_dir)
}

#[test]
fn status_json_has_the_codemap_block_for_an_empty_and_a_filled_map() {
    let _switch = crate::codemap::test_switch::force(false);
    let (_dir, db_path, projects_dir) = temp_db();
    let storage = Storage::open(&db_path).unwrap();

    // Tables exist, nothing mapped yet.
    let value =
        serde_json::to_value(gather_status(&db_path, &projects_dir, false).unwrap()).unwrap();
    let empty = &value["codemap"];
    assert_eq!(empty["initialised"], true);
    assert_eq!(empty["disabled"], false);
    assert_eq!(empty["views"], 0);
    assert_eq!(empty["poll_lag_secs"], serde_json::Value::Null);
    assert_eq!(empty["scan_lag_secs"], serde_json::Value::Null);

    // Fill it from a real repo with two linked worktrees.
    let m = map_fixture();
    storage
        .with_connection(|conn| {
            crate::codemap::store::upsert_repo(
                conn,
                &m.main.join(".git").to_string_lossy(),
                Some(&m.main.to_string_lossy()),
                "cli",
                true,
            )?;
            scan_repo_into(conn, &m.fx, &m.main);
            crate::codemap::enqueue(conn, &m.main.join("src/radio.py"))?;
            Ok(())
        })
        .unwrap();
    let value =
        serde_json::to_value(gather_status(&db_path, &projects_dir, false).unwrap()).unwrap();
    let block = &value["codemap"];
    assert_eq!(block["repos"], 1);
    assert_eq!(block["views"], 3);
    // 17 code files in each of 3 views.
    assert_eq!(block["view_file_pairs"], 51);
    assert!(block["unique_blobs"].as_i64().unwrap() > 0);
    assert!(block["defs"].as_i64().unwrap() > 0);
    assert_eq!(block["queue_depth"], 1);
    assert_eq!(block["views_truncated"], 0);
    assert_eq!(block["views_in_error"], 0);
    assert_eq!(block["repos_never_polled"], 0);
    assert!(block["poll_lag_secs"].as_i64().unwrap() >= 0);
    assert!(block["scan_lag_secs"].as_i64().unwrap() >= 0);
    let mut keys: Vec<&str> = block
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "defs",
            "disabled",
            "initialised",
            "poll_lag_secs",
            "queue_depth",
            "repos",
            "repos_never_polled",
            "scan_lag_secs",
            "unique_blobs",
            "view_file_pairs",
            "views",
            "views_in_error",
            "views_truncated",
        ]
    );

    // The statusline path never reads the map.
    let compact = gather_status_with(&db_path, &projects_dir, false, false).unwrap();
    assert_eq!(compact.codemap, CodemapStatusBlock::default());
}

#[test]
fn status_survives_a_database_without_the_codemap_tables() {
    let _switch = crate::codemap::test_switch::force(false);
    let (_dir, db_path, projects_dir) = temp_db();
    let storage = Storage::open(&db_path).unwrap();
    storage
        .with_connection(|conn| {
            conn.execute_batch("DROP TABLE codemap_queue;")?;
            Ok(())
        })
        .unwrap();
    drop(storage);
    let report = gather_status(&db_path, &projects_dir, false).unwrap();
    assert_eq!(report.codemap, CodemapStatusBlock::default());
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["codemap"]["initialised"], false);
}

#[test]
fn status_reports_the_kill_switch_in_the_block() {
    let (_dir, db_path, projects_dir) = temp_db();
    let _storage = Storage::open(&db_path).unwrap();
    let _switch = crate::codemap::test_switch::force(true);
    let report = gather_status(&db_path, &projects_dir, false);
    assert!(report.unwrap().codemap.disabled);
}
