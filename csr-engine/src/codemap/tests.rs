//! Scanner, store and query tests against real git repos in a temp dir.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use super::identity::{self, git_blob_oid, identify_with, list_views_with, ViewId};
use super::scan::{
    collect_walk, rescan_paths, scan_repo, view_changed, walk_visit_budget, RepoScanReport,
    ScanOpts,
};
use super::store;
use super::testutil::{count, db, py, write, Fx};
use super::{extractor_version, query};

fn opts(fx: &Fx) -> ScanOpts {
    ScanOpts {
        home: Some(fx.home.clone()),
        ..Default::default()
    }
}

fn common_of(repo: &Path) -> PathBuf {
    repo.join(".git")
}

fn scan(conn: &Connection, fx: &Fx, repo: &Path) -> RepoScanReport {
    scan_repo(conn, &common_of(repo), &opts(fx)).unwrap()
}

fn view_id(conn: &Connection, wt: &Path) -> i64 {
    store::stored_view_by_path(conn, &wt.to_string_lossy())
        .unwrap()
        .unwrap_or_else(|| panic!("no stored view for {}", wt.display()))
        .id
}

fn rows(conn: &Connection, wt: &Path) -> Vec<(String, String, i64)> {
    store::stored_files(conn, view_id(conn, wt))
        .unwrap()
        .into_iter()
        .map(|f| (f.path, f.blob_oid, f.dirty))
        .collect()
}

fn fts_count_for_blob(conn: &Connection, blob: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM codemap_fts WHERE blob_oid = ?1",
        params![blob],
        |r| r.get(0),
    )
    .unwrap()
}

/// FTS rows == distinct (path, blob, def) over held pairs, and no orphan blobs.
fn assert_invariants(conn: &Connection) {
    let expected: i64 = count(
        conn,
        &format!(
            "SELECT COUNT(*) FROM (
                SELECT DISTINCT f.path, d.blob_oid, d.kind, d.name, d.span_start, d.span_end
                FROM codemap_view_files f JOIN codemap_defs d ON d.blob_oid = f.blob_oid
                WHERE d.extractor_version = '{}'
            )",
            extractor_version()
        ),
    );
    assert_eq!(count(conn, "SELECT COUNT(*) FROM codemap_fts"), expected);
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM codemap_blobs
             WHERE blob_oid NOT IN (SELECT blob_oid FROM codemap_view_files)"
        ),
        0,
        "unreferenced blobs must be collected"
    );
    assert_eq!(
        count(
            conn,
            "SELECT COUNT(*) FROM codemap_defs
             WHERE blob_oid NOT IN (SELECT blob_oid FROM codemap_blobs)"
        ),
        0,
        "defs of collected blobs must be gone"
    );
}

// ---- identity -------------------------------------------------------------------

#[test]
fn main_and_linked_worktree_share_one_common_dir() {
    let fx = Fx::new();
    let main = fx.init_repo("a");
    write(&main, "a.py", &py(&["alpha"]));
    fx.commit_all(&main, "init");
    let wt = fx.add_worktree(&main, &fx.root.join("a-wt"), "feat");
    write(&wt, "deep/dir/x.py", &py(&["xx"]));

    let m = identify_with(&main, Some(&fx.home)).unwrap();
    let l = identify_with(&wt, Some(&fx.home)).unwrap();
    let from_file = identify_with(&wt.join("deep/dir/x.py"), Some(&fx.home)).unwrap();
    assert_eq!(m.common_dir, common_of(&main));
    assert_eq!(l.common_dir, m.common_dir);
    assert_eq!(from_file, l);
    assert_eq!(m.worktree_root, main);
    assert_eq!(l.worktree_root, wt);

    let listing = list_views_with(&m.common_dir, Some(&fx.home)).unwrap();
    let paths: Vec<_> = listing
        .views
        .iter()
        .map(|v| v.worktree_path.clone())
        .collect();
    assert_eq!(listing.stale, 0);
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&main) && paths.contains(&wt));
    assert!(
        listing
            .views
            .iter()
            .find(|v| v.worktree_path == main)
            .unwrap()
            .is_main
    );
}

#[test]
fn stale_worktree_entry_is_skipped_and_counted() {
    let fx = Fx::new();
    let main = fx.init_repo("a");
    write(&main, "a.py", &py(&["alpha"]));
    fx.commit_all(&main, "init");
    let wt = fx.add_worktree(&main, &fx.root.join("gone"), "gone");
    std::fs::remove_dir_all(&wt).unwrap();

    let listing = list_views_with(&common_of(&main), Some(&fx.home)).unwrap();
    assert_eq!(listing.views.len(), 1);
    assert_eq!(listing.views[0].worktree_path, main);
    assert_eq!(listing.stale, 1);
}

#[test]
fn home_itself_and_non_repos_are_refused() {
    let fx = Fx::new();
    // fx.home is not a repo; a repo that IS the home directory is refused too.
    std::fs::create_dir_all(fx.home.join(".git")).unwrap();
    let err = identify_with(&fx.home, Some(&fx.home))
        .unwrap_err()
        .to_string();
    assert!(err.contains("home directory"), "{err}");
    let plain = fx.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(
        identify_with(&plain, Some(&fx.home)).is_err() || {
            // The temp dir may itself sit inside a repo on some machines; a path
            // below $HOME must never resolve to $HOME's own repo.
            identify_with(&plain, Some(&fx.home)).unwrap().common_dir != fx.home.join(".git")
        }
    );
}

#[test]
fn blob_hash_equals_git_hash_object() {
    let fx = Fx::new();
    let dir = fx.init_repo("h");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty.py", Vec::new()),
        ("text.py", b"def foo(x):\n    return x\n".to_vec()),
        ("crlf.py", b"a = 1\r\nb = 2\r\n".to_vec()),
        ("bin.py", (0u8..=255).collect()),
        ("big.py", vec![b'x'; 300_000]),
    ];
    for (name, bytes) in cases {
        std::fs::write(dir.join(name), &bytes).unwrap();
        let want = fx.git(&dir, &["hash-object", name]);
        assert_eq!(git_blob_oid(&bytes), want, "{name}");
    }
}

// ---- classification and caps ------------------------------------------------------

#[test]
fn clean_modified_and_untracked_files_are_classified() {
    let fx = Fx::new();
    let main = fx.init_repo("c");
    write(&main, "clean.py", &py(&["one"]));
    write(&main, "edited.py", &py(&["two"]));
    write(&main, "notes.txt", "not code");
    write(
        &main,
        "node_modules/pkg/skip.js",
        "function skipme(a) { return a }\n",
    );
    write(&main, "build/out.py", &py(&["built"]));
    fx.commit_all(&main, "init");
    write(&main, "edited.py", &py(&["two", "two_more"]));
    write(&main, "fresh.py", &py(&["three"]));
    write(&main, ".gitignore", "ignored_*.py\n");
    write(&main, "ignored_x.py", &py(&["nope"]));
    write(&main, ".plans/h.py", &py(&["hid"]));
    write(&main, ".venv/lib/v.py", &py(&["venv_fn"]));

    let conn = db();
    let report = scan(&conn, &fx, &main);
    assert_eq!(report.views.len(), 1);
    let v = &report.views[0];
    assert_eq!(v.status, "ok");
    assert_eq!(v.branch.as_deref(), Some("main"));
    assert_eq!(v.dirty, 1, "edited.py");
    assert_eq!(
        v.untracked, 2,
        "fresh.py and .plans/h.py; gitignored and .venv stay out"
    );

    let got = rows(&conn, &main);
    let by_path = |p: &str| got.iter().find(|r| r.0 == p).cloned();
    assert_eq!(by_path("clean.py").unwrap().2, 0);
    assert_eq!(by_path("edited.py").unwrap().2, 1);
    assert_eq!(by_path("fresh.py").unwrap().2, 2);
    assert!(by_path("notes.txt").is_none());
    assert!(by_path("node_modules/pkg/skip.js").is_none());
    assert!(by_path("build/out.py").is_none());
    assert!(by_path("ignored_x.py").is_none());
    assert_eq!(
        by_path(".plans/h.py").unwrap().2,
        2,
        "hidden dirs are walked"
    );
    assert!(by_path(".venv/lib/v.py").is_none());
    // The stored oid is the content on disk.
    let edited_oid = fx.git(&main, &["hash-object", "edited.py"]);
    assert_eq!(by_path("edited.py").unwrap().1, edited_oid);
    assert_invariants(&conn);
}

#[test]
fn stat_proof_of_cleanliness_needs_an_older_matching_entry() {
    use super::identity::IndexEntry;
    let entry = IndexEntry {
        path: "a.py".into(),
        oid: "x".into(),
        mtime_secs: 100,
        mtime_nsecs: 5,
        size: 10,
    };
    let st = |secs, nsecs, size| super::scan::test_stat(secs, nsecs, size);
    let idx = Some((200, 0));
    assert!(super::scan::clean_by_stat(&entry, &st(100, 5, 10), idx));
    assert!(!super::scan::clean_by_stat(&entry, &st(100, 6, 10), idx));
    assert!(!super::scan::clean_by_stat(&entry, &st(100, 5, 11), idx));
    assert!(!super::scan::clean_by_stat(&entry, &st(101, 5, 10), idx));
    // Racy: written in the same second the index was.
    assert!(!super::scan::clean_by_stat(
        &entry,
        &st(100, 5, 10),
        Some((100, 9))
    ));
    assert!(!super::scan::clean_by_stat(&entry, &st(100, 5, 10), None));
}

#[test]
fn a_file_whose_stat_matches_an_older_index_entry_is_trusted_clean() {
    let fx = Fx::new();
    let main = fx.init_repo("t");
    write(&main, "a.py", &py(&["alpha_one"]));
    write(&main, "b.py", &py(&["beta_one"]));
    // Entries must predate the index file's own second to count as proven clean.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.git(&main, &["add", "-A"]);
    fx.git(&main, &["commit", "-q", "-m", "init"]);
    let conn = db();
    scan(&conn, &fx, &main);
    assert!(rows(&conn, &main).iter().all(|r| r.2 == 0));

    // Same size, different bytes, same mtime: git's own stat check trusts it, so
    // does the scan (it never reads the file).
    let a = main.join("a.py");
    let mtime = std::fs::metadata(&a).unwrap().modified().unwrap();
    let body = std::fs::read_to_string(&a)
        .unwrap()
        .replace("alpha_one", "alpha_two");
    std::fs::write(&a, body).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&a)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    // A genuinely different mtime is hashed and found modified.
    let b = main.join("b.py");
    let body = std::fs::read_to_string(&b)
        .unwrap()
        .replace("beta_one", "beta_two");
    std::fs::write(&b, body).unwrap();

    scan(&conn, &fx, &main);
    let got = rows(&conn, &main);
    assert_eq!(got.iter().find(|r| r.0 == "a.py").unwrap().2, 0);
    assert_eq!(got.iter().find(|r| r.0 == "b.py").unwrap().2, 1);
}

#[test]
fn nested_worktree_directory_is_not_walked_by_the_main_view() {
    let fx = Fx::new();
    let main = fx.init_repo("n");
    write(&main, "top.py", &py(&["top"]));
    fx.commit_all(&main, "init");
    let wt = fx.add_worktree(&main, &main.join("trees/inner"), "inner");
    write(&wt, "only_in_inner.py", &py(&["inner_only"]));

    let conn = db();
    let report = scan(&conn, &fx, &main);
    assert_eq!(report.views.len(), 2);
    let main_rows = rows(&conn, &main);
    assert!(
        main_rows.iter().all(|r| !r.0.starts_with("trees/")),
        "main view walked into the nested worktree: {main_rows:?}"
    );
    let inner_rows = rows(&conn, &wt);
    assert!(inner_rows
        .iter()
        .any(|r| r.0 == "only_in_inner.py" && r.2 == 2));
    assert!(inner_rows.iter().any(|r| r.0 == "top.py" && r.2 == 0));
}

#[test]
fn view_cap_sets_truncated() {
    let fx = Fx::new();
    let main = fx.init_repo("cap");
    for i in 0..5 {
        write(&main, &format!("f{i}.py"), &py(&[&format!("fn_{i}")]));
    }
    fx.commit_all(&main, "init");
    write(&main, "extra.py", &py(&["extra"]));
    let conn = db();
    let mut o = opts(&fx);
    o.max_files_per_view = 3;
    let report = scan_repo(&conn, &common_of(&main), &o).unwrap();
    assert_eq!(report.views[0].files, 3);
    assert!(report.views[0].truncated);
    let stored = store::stored_views(&conn, None).unwrap();
    assert!(stored[0].truncated);
    assert_eq!(stored[0].files, 3);

    // Exactly at the cap is not truncated.
    o.max_files_per_view = 6;
    let report = scan_repo(&conn, &common_of(&main), &o).unwrap();
    assert_eq!(report.views[0].files, 6);
    assert!(!report.views[0].truncated);
    assert_invariants(&conn);
}

// ---- dedupe, edits, removal ----------------------------------------------------------

#[test]
fn two_views_holding_the_same_file_parse_it_once() {
    let fx = Fx::new();
    let main = fx.init_repo("d");
    write(&main, "a.py", &py(&["alpha_one"]));
    write(&main, "b.py", &py(&["beta_one"]));
    fx.commit_all(&main, "init");
    let _wt = fx.add_worktree(&main, &fx.root.join("d-wt"), "feat");

    let conn = db();
    let first = scan(&conn, &fx, &main);
    assert_eq!(first.views.len(), 2);
    assert_eq!(first.totals.file_pairs, 4);
    assert_eq!(first.totals.unique_blobs_seen, 2);
    assert_eq!(first.totals.blobs_parsed, 2);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM codemap_blobs"), 2);
    // FTS holds one row per (path, blob, def), not one per view.
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM codemap_fts"),
        count(&conn, "SELECT COUNT(*) FROM codemap_defs")
    );
    assert_invariants(&conn);

    let second = scan(&conn, &fx, &main);
    assert_eq!(second.totals.blobs_parsed, 0);
    assert_eq!(second.totals.blobs_reused, 2);
    assert_eq!(second.totals.defs_written, 0);
    assert_invariants(&conn);
}

#[test]
fn editing_a_file_in_one_view_changes_only_that_views_blob() {
    let fx = Fx::new();
    let main = fx.init_repo("e");
    write(&main, "a.py", &py(&["alpha_one"]));
    fx.commit_all(&main, "init");
    let wt = fx.add_worktree(&main, &fx.root.join("e-wt"), "feat");

    let conn = db();
    scan(&conn, &fx, &main);
    let before_main = rows(&conn, &main);
    let blob_a = before_main[0].1.clone();
    let fts_a = fts_count_for_blob(&conn, &blob_a);
    assert!(fts_a >= 1);

    write(&wt, "a.py", &py(&["alpha_one", "alpha_two"]));
    let report = scan(&conn, &fx, &main);
    assert_eq!(report.totals.blobs_parsed, 1);

    assert_eq!(rows(&conn, &main), before_main, "main view untouched");
    let wt_rows = rows(&conn, &wt);
    assert_eq!(wt_rows.len(), 1);
    assert_ne!(wt_rows[0].1, blob_a);
    assert_eq!(wt_rows[0].2, 1);
    assert_eq!(
        fts_count_for_blob(&conn, &blob_a),
        fts_a,
        "main's FTS rows kept"
    );
    assert!(fts_count_for_blob(&conn, &wt_rows[0].1) >= 2);
    assert_invariants(&conn);

    // Revert: the edited blob is no longer held by anyone and is collected.
    write(&wt, "a.py", &py(&["alpha_one"]));
    let report = scan(&conn, &fx, &main);
    assert!(report.totals.gc_blobs >= 1);
    assert_eq!(fts_count_for_blob(&conn, &wt_rows[0].1), 0);
    assert_invariants(&conn);
}

#[test]
fn removing_a_worktree_drops_its_view_and_garbage_collects_its_rows() {
    let fx = Fx::new();
    let main = fx.init_repo("r");
    write(&main, "a.py", &py(&["alpha_one"]));
    fx.commit_all(&main, "init");
    let wt = fx.add_worktree(&main, &fx.root.join("r-wt"), "feat");
    write(&wt, "only_wt.py", &py(&["wt_only_fn"]));

    let conn = db();
    scan(&conn, &fx, &main);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM codemap_views"), 2);
    let only_wt_blob = rows(&conn, &wt)
        .into_iter()
        .find(|r| r.0 == "only_wt.py")
        .unwrap()
        .1;
    assert!(fts_count_for_blob(&conn, &only_wt_blob) >= 1);

    fx.git(
        &main,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    let report = scan(&conn, &fx, &main);
    assert_eq!(report.removed_views, 1);
    assert!(report.totals.gc_blobs >= 1);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM codemap_views"), 1);
    assert_eq!(fts_count_for_blob(&conn, &only_wt_blob), 0);
    assert_eq!(
        count(
            &conn,
            &format!("SELECT COUNT(*) FROM codemap_blobs WHERE blob_oid = '{only_wt_blob}'")
        ),
        0
    );
    assert_invariants(&conn);
}

#[test]
fn an_extractor_bump_reparses_and_collects_the_old_rows() {
    let fx = Fx::new();
    let main = fx.init_repo("v");
    write(&main, "a.py", &py(&["alpha_one"]));
    fx.commit_all(&main, "init");
    let conn = db();
    scan(&conn, &fx, &main);
    let views = identity::list_views_with(&common_of(&main), Some(&fx.home)).unwrap();
    assert!(!view_changed(&conn, &views.views[0]).unwrap());

    // Pretend an older extractor made every row.
    conn.execute("UPDATE codemap_blobs SET extractor_version = 'old'", [])
        .unwrap();
    conn.execute("UPDATE codemap_defs SET extractor_version = 'old'", [])
        .unwrap();
    assert!(
        view_changed(&conn, &views.views[0]).unwrap(),
        "blobs the current extractor has not parsed make the view stale"
    );
    let report = scan(&conn, &fx, &main);
    assert_eq!(report.totals.blobs_parsed, 1);
    assert!(report.totals.gc_blobs >= 1);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM codemap_blobs WHERE extractor_version = 'old'"
        ),
        0
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM codemap_defs WHERE extractor_version = 'old'"
        ),
        0
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM codemap_blobs WHERE extractor_version = '{}'",
                extractor_version()
            )
        ),
        1
    );
    assert_invariants(&conn);
}

// ---- partial rescan and the change probe ----------------------------------------------

#[test]
fn rescan_paths_routes_a_nested_worktree_file_to_its_own_view() {
    let fx = Fx::new();
    let main = fx.init_repo("p");
    write(&main, "a.py", &py(&["alpha_one"]));
    write(&main, ".gitignore", "ignored_*.py\n");
    fx.commit_all(&main, "init");
    let wt = fx.add_worktree(&main, &main.join("trees/inner"), "inner");

    let conn = db();
    scan(&conn, &fx, &main);
    let main_before = rows(&conn, &main);
    let wt_before = rows(&conn, &wt);

    // Edit in the nested worktree.
    write(&wt, "a.py", &py(&["alpha_one", "alpha_changed"]));
    let report = rescan_paths(&conn, &[wt.join("a.py")]).unwrap();
    assert_eq!(report.views_touched, vec![wt.to_string_lossy().to_string()]);
    assert_eq!(report.updated, 1);
    assert_eq!(report.blobs_parsed, 1);
    assert_eq!(rows(&conn, &main), main_before, "main view row untouched");
    let wt_after = rows(&conn, &wt);
    let a_before = wt_before.iter().find(|r| r.0 == "a.py").unwrap();
    let a_after = wt_after.iter().find(|r| r.0 == "a.py").unwrap();
    assert_ne!(a_before.1, a_after.1);
    assert_eq!(a_after.2, 1, "modified");
    assert_eq!(wt_after.len(), wt_before.len());
    assert_invariants(&conn);

    // Edited again: still dirty, no stale blob left behind.
    write(&wt, "a.py", &py(&["alpha_one", "alpha_again"]));
    rescan_paths(&conn, &[wt.join("a.py")]).unwrap();
    assert_invariants(&conn);

    // Reverted to the committed content: clean again.
    write(&wt, "a.py", &py(&["alpha_one"]));
    rescan_paths(&conn, &[wt.join("a.py")]).unwrap();
    let a = rows(&conn, &wt)
        .into_iter()
        .find(|r| r.0 == "a.py")
        .unwrap();
    assert_eq!((a.1.as_str(), a.2), (a_before.1.as_str(), 0));
    assert_invariants(&conn);

    // A new untracked file is added; an ignored one and a non-code one are not.
    write(&wt, "new_one.py", &py(&["brand_new"]));
    write(&wt, "ignored_one.py", &py(&["not_me"]));
    write(&wt, "readme.txt", "x");
    let report = rescan_paths(
        &conn,
        &[
            wt.join("new_one.py"),
            wt.join("ignored_one.py"),
            wt.join("readme.txt"),
        ],
    )
    .unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(report.skipped_ignored, 1);
    assert_eq!(report.skipped_caps, 1);
    let new_row = rows(&conn, &wt)
        .into_iter()
        .find(|r| r.0 == "new_one.py")
        .unwrap();
    assert_eq!(new_row.2, 2);

    // Deleting a file drops its row and collects its blob.
    std::fs::remove_file(wt.join("new_one.py")).unwrap();
    let report = rescan_paths(&conn, &[wt.join("new_one.py")]).unwrap();
    assert_eq!(report.removed, 1);
    assert!(rows(&conn, &wt).iter().all(|r| r.0 != "new_one.py"));
    assert!(report.gc_blobs >= 1);
    assert_invariants(&conn);

    // Outside every view, and protected folders, are skipped.
    let report = rescan_paths(
        &conn,
        &[
            fx.root.join("elsewhere.py"),
            PathBuf::from("/Users/nobody-here/Desktop/x.py"),
        ],
    )
    .unwrap();
    assert_eq!(report.skipped_outside_views, 1);
    assert_eq!(report.skipped_protected, 1);
    assert!(report.views_touched.is_empty());
}

#[test]
fn view_changed_is_false_when_untouched_and_true_after_a_commit_or_an_add() {
    let fx = Fx::new();
    let main = fx.init_repo("w");
    write(&main, "a.py", &py(&["alpha_one"]));
    fx.commit_all(&main, "init");
    let conn = db();
    let view = |main: &Path| -> ViewId {
        identity::list_views_with(&common_of(main), Some(&fx.home))
            .unwrap()
            .views
            .remove(0)
    };
    assert!(view_changed(&conn, &view(&main)).unwrap(), "never scanned");
    scan(&conn, &fx, &main);
    assert!(!view_changed(&conn, &view(&main)).unwrap());
    assert!(!view_changed(&conn, &view(&main)).unwrap());

    // A commit moves HEAD.
    fx.git(&main, &["commit", "-q", "--allow-empty", "-m", "empty"]);
    assert!(view_changed(&conn, &view(&main)).unwrap());
    scan(&conn, &fx, &main);
    assert!(!view_changed(&conn, &view(&main)).unwrap());

    // `git add` rewrites the index.
    write(&main, "b.py", &py(&["beta_one"]));
    fx.git(&main, &["add", "b.py"]);
    assert!(view_changed(&conn, &view(&main)).unwrap());
    let report = scan(&conn, &fx, &main);
    assert_eq!(report.views[0].files, 2);
    assert!(!view_changed(&conn, &view(&main)).unwrap());

    // A branch switch with the same commit changes the branch name.
    fx.git(&main, &["checkout", "-q", "-b", "other"]);
    assert!(view_changed(&conn, &view(&main)).unwrap());
}

// ---- safety and wiring -----------------------------------------------------------------

#[test]
fn scanning_a_repo_in_a_protected_folder_is_refused() {
    let conn = db();
    let err = scan_repo(
        &conn,
        Path::new("/Users/nobody-here/Documents/proj/.git"),
        &ScanOpts::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("macOS-protected"), "{err}");
}

#[test]
fn migration_creates_every_codemap_table() {
    let conn = db();
    assert!(store::tables_ready(&conn));
    for table in [
        "codemap_repos",
        "codemap_views",
        "codemap_view_files",
        "codemap_blobs",
        "codemap_defs",
        "codemap_queue",
        "codemap_signals",
        "codemap_fts",
    ] {
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM sqlite_master WHERE name = '{table}'")
            ),
            1,
            "{table}"
        );
    }
}

// ---- query ------------------------------------------------------------------------------

#[test]
fn query_groups_presence_by_blob() {
    let fx = Fx::new();
    let main = fx.init_repo("q");
    write(&main, "app/radio.py", &py(&["alpha_one", "beta_two"]));
    write(&main, "app/other.py", &py(&["gamma_three"]));
    fx.commit_all(&main, "init");
    let same = fx.add_worktree(&main, &fx.root.join("q-same"), "same");
    let diff = fx.add_worktree(&main, &fx.root.join("q-diff"), "diff");
    write(
        &diff,
        "app/radio.py",
        &py(&["alpha_one", "beta_two", "delta_four"]),
    );

    let conn = db();
    scan(&conn, &fx, &main);
    assert_invariants(&conn);

    let hits = query(&conn, "alpha_one", 10).unwrap();
    assert_eq!(hits.len(), 1, "one definition across three views: {hits:?}");
    let h = &hits[0];
    assert_eq!(h.path, "app/radio.py");
    assert_eq!(h.name, "alpha_one");
    assert_eq!(h.kind, "function");
    assert_eq!(h.repo, common_of(&main).to_string_lossy());
    assert_eq!(h.same_in, vec!["main".to_string(), "q-same".to_string()]);
    assert_eq!(h.differs_in, vec!["q-diff*".to_string()]);
    assert_eq!(h.presence.len(), 3);
    assert_eq!(h.span_start, 0);
    assert_eq!(h.signature, "def alpha_one(x):");
    let line = h.render();
    assert!(
        line.contains("app/radio.py:1-2")
            && line.contains("same blob in main, q-same")
            && line.contains("differs in q-diff*"),
        "{line}"
    );

    // A definition only the edited view has: its blob is the primary, and the
    // views holding the same path without it are the ones that differ.
    let hits = query(&conn, "delta_four", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].same_in, vec!["q-diff*".to_string()]);
    assert_eq!(
        hits[0].differs_in,
        vec!["main".to_string(), "q-same".to_string()]
    );

    // Prefix and mid-word fragments both find it.
    assert_eq!(query(&conn, "gamma", 10).unwrap().len(), 1);
    assert_eq!(query(&conn, "three", 10).unwrap().len(), 1);
    // Path words hit too.
    assert!(!query(&conn, "radio", 10).unwrap().is_empty());
    assert!(query(&conn, "nothing_like_this", 10).unwrap().is_empty());
    let _ = same;
}

#[test]
fn status_report_counts_what_the_scan_wrote() {
    let fx = Fx::new();
    let main = fx.init_repo("s");
    write(&main, "a.py", &py(&["alpha_one"]));
    fx.commit_all(&main, "init");
    write(&main, "u.py", &py(&["untracked_fn"]));
    let conn = db();
    assert_eq!(super::status_report(&conn).unwrap().totals.views, 0);
    scan(&conn, &fx, &main);
    let s = super::status_report(&conn).unwrap();
    assert!(s.initialised);
    assert_eq!(s.totals.views, 1);
    assert_eq!(s.totals.view_file_pairs, 2);
    assert_eq!(s.totals.unique_blobs, 2);
    assert_eq!(s.repos[0].views[0].untracked, 1);
    assert_eq!(s.repos[0].views[0].files, 2);
    assert_eq!(s.totals.fts_rows, s.totals.defs);
}

// ---- blob statuses that depend on the path ---------------------------------------

const TS_SRC: &str = "export function swiftie(x: number) { return x; }\n";
const JS_SRC: &str = "function shorty(a) { return a }\n";

fn hit_paths(conn: &Connection, name: &str) -> Vec<String> {
    query(conn, name, 20)
        .unwrap()
        .into_iter()
        .map(|h| h.path)
        .collect()
}

fn blob_status_of(conn: &Connection, bytes: &str) -> Option<String> {
    store::blob_status(conn, &git_blob_oid(bytes.as_bytes())).unwrap()
}

fn fts_rows_for_path(conn: &Connection, path: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM codemap_fts WHERE path = ?1",
        params![path],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn identical_bytes_first_seen_under_a_path_with_no_parser_still_give_the_ts_file_its_defs() {
    // Both scan orders: the blob is first seen under a.swift (no parser), or
    // under the .ts file.
    for (first, second, ts_path) in [("a.swift", "b.ts", "b.ts"), ("x.ts", "y.swift", "x.ts")] {
        let fx = Fx::new();
        let main = fx.init_repo("sw");
        write(&main, first, TS_SRC);
        write(&main, second, TS_SRC);
        fx.commit_all(&main, "init");
        let conn = db();
        let r = scan(&conn, &fx, &main);
        assert_eq!(r.views[0].files, 2, "{first} {second}");
        assert!(
            hit_paths(&conn, "swiftie").contains(&ts_path.to_string()),
            "{first}/{second}: {:?}",
            hit_paths(&conn, "swiftie")
        );
        assert!(fts_rows_for_path(&conn, ts_path) > 0, "{first}/{second}");
        assert_eq!(blob_status_of(&conn, TS_SRC).as_deref(), Some("ok"));
        assert_invariants(&conn);
        // Nothing is reparsed on the next scan.
        let again = scan(&conn, &fx, &main);
        assert_eq!(again.totals.blobs_parsed, 0, "{first}/{second}");
    }
}

#[test]
fn a_partial_rescan_of_a_path_with_a_parser_upgrades_a_no_lang_blob() {
    let fx = Fx::new();
    let main = fx.init_repo("sw2");
    write(&main, "a.swift", TS_SRC);
    fx.commit_all(&main, "init");
    let conn = db();
    scan(&conn, &fx, &main);
    assert_eq!(blob_status_of(&conn, TS_SRC).as_deref(), Some("no_lang"));
    assert!(hit_paths(&conn, "swiftie").is_empty());

    // The same bytes under a .ts name, picked up by the watcher / hook path.
    write(&main, "b.ts", TS_SRC);
    let report = rescan_paths(&conn, &[main.join("b.ts")]).unwrap();
    assert_eq!(report.updated, 1);
    assert!(hit_paths(&conn, "swiftie").contains(&"b.ts".to_string()));
    assert_eq!(blob_status_of(&conn, TS_SRC).as_deref(), Some("ok"));
    assert_invariants(&conn);

    // A stored ok row is never downgraded: after the .ts copy goes away the
    // blob is still held by a.swift, still ok.
    std::fs::remove_file(main.join("b.ts")).unwrap();
    rescan_paths(&conn, &[main.join("b.ts")]).unwrap();
    assert_eq!(blob_status_of(&conn, TS_SRC).as_deref(), Some("ok"));
    assert_invariants(&conn);
    scan(&conn, &fx, &main);
    assert_eq!(blob_status_of(&conn, TS_SRC).as_deref(), Some("ok"));
    assert_invariants(&conn);
}

#[test]
fn a_min_js_name_does_not_hide_the_same_bytes_under_a_normal_name() {
    for (first, second, js_path, parses) in [
        ("a.min.js", "b.js", "b.js", 2),
        ("x.js", "y.min.js", "x.js", 1),
    ] {
        let fx = Fx::new();
        let main = fx.init_repo("mj");
        write(&main, first, JS_SRC);
        write(&main, second, JS_SRC);
        fx.commit_all(&main, "init");
        let conn = db();
        let r = scan(&conn, &fx, &main);
        assert_eq!(r.totals.blobs_parsed, parses, "{first}/{second}");
        assert!(
            hit_paths(&conn, "shorty").contains(&js_path.to_string()),
            "{first}/{second}: {:?}",
            hit_paths(&conn, "shorty")
        );
        assert!(fts_rows_for_path(&conn, js_path) > 0);
        assert_eq!(blob_status_of(&conn, JS_SRC).as_deref(), Some("ok"));
        assert_invariants(&conn);
    }
}

#[test]
fn a_blob_minified_by_content_is_final_and_is_not_reparsed() {
    let fx = Fx::new();
    let main = fx.init_repo("mc");
    let long = format!("function f(a){{return a}}{}\n", "/*x*/".repeat(500));
    write(&main, "c.js", &long);
    write(&main, "d.js", &long); // same bytes, a normal name
    write(&main, "e.swift", TS_SRC); // path-dependent no_lang, no path can do better
    fx.commit_all(&main, "init");
    let conn = db();
    let first = scan(&conn, &fx, &main);
    assert_eq!(
        first.totals.blobs_parsed, 2,
        "the long blob once, the swift blob once"
    );
    assert_eq!(blob_status_of(&conn, &long).as_deref(), Some("minified"));
    assert_eq!(blob_status_of(&conn, TS_SRC).as_deref(), Some("no_lang"));
    assert!(hit_paths(&conn, "f")
        .iter()
        .all(|p| p != "c.js" && p != "d.js"));
    let second = scan(&conn, &fx, &main);
    assert_eq!(
        second.totals.blobs_parsed, 0,
        "neither the content-minified nor the no_lang blob is parsed again"
    );
    assert_invariants(&conn);
}

// ---- the untracked walk honours the file cap and a visit budget ----------------------

#[test]
fn the_untracked_walk_stops_at_the_file_cap_and_marks_the_view_truncated() {
    let fx = Fx::new();
    let main = fx.init_repo("capwalk");
    for i in 0..10 {
        write(&main, &format!("t{i}.py"), &py(&[&format!("tracked_{i}")]));
    }
    fx.commit_all(&main, "init");
    for i in 0..300 {
        write(
            &main,
            &format!("u{i:03}.py"),
            &py(&[&format!("untracked_{i}")]),
        );
    }
    let conn = db();
    let mut o = opts(&fx);
    o.max_files_per_view = 50;
    let report = scan_repo(&conn, &common_of(&main), &o).unwrap();
    assert_eq!(report.views[0].files, 50);
    assert_eq!(report.views[0].untracked, 40);
    assert!(report.views[0].truncated);
    let stored = store::stored_views(&conn, None).unwrap();
    assert_eq!(stored[0].files, 50);
    assert!(stored[0].truncated);
    assert_eq!(rows(&conn, &main).len(), 50);
    assert_invariants(&conn);
    // The scan asked the walk for the 40 places left, not for all 300 files.
    let (walked, visited, stopped) = super::scan::test_hooks::last_walk().unwrap();
    assert_eq!(walked, 40);
    assert!(stopped);
    assert!(visited <= walk_visit_budget(50), "visited {visited}");
    assert!(visited < 300, "visited {visited}");

    // The walk itself stops one file past its allowance instead of listing all
    // 300, and never visits more entries than its budget.
    let budget = walk_visit_budget(50);
    let found = collect_walk(
        &main,
        &common_of(&main),
        None,
        |rel| rel.starts_with('u'),
        40,
        budget,
    );
    assert_eq!(found.files.len(), 40);
    assert!(found.stopped);
    assert!(found.visited <= budget, "{} > {budget}", found.visited);
    assert!(found.visited < 300, "visited {}", found.visited);
}

#[test]
fn a_walk_that_fits_the_cap_and_the_budget_is_not_truncated() {
    let fx = Fx::new();
    let main = fx.init_repo("fits");
    write(&main, "a.py", &py(&["alpha"]));
    fx.commit_all(&main, "init");
    for i in 0..6 {
        write(&main, &format!("n{i}.txt"), "not code\n");
    }
    write(&main, "b.py", &py(&["beta"]));
    let conn = db();
    let mut o = opts(&fx);
    o.max_files_per_view = 5;
    let report = scan_repo(&conn, &common_of(&main), &o).unwrap();
    assert_eq!(report.views[0].files, 2);
    assert!(!report.views[0].truncated);
}

#[test]
fn the_visit_budget_stops_a_walk_over_many_non_code_entries_and_marks_the_view_truncated() {
    let fx = Fx::new();
    let main = fx.init_repo("visits");
    write(&main, "a.py", &py(&["alpha"]));
    fx.commit_all(&main, "init");
    // 150 untracked entries that are walkable but never code files, spread over
    // directories, plus a code file that sorts after all of them.
    for d in 0..15 {
        for f in 0..10 {
            write(&main, &format!("docs{d:02}/n{f}.txt"), "not code\n");
        }
    }
    write(&main, "zzz_late.py", &py(&["late"]));
    let conn = db();
    let mut o = opts(&fx);
    o.max_files_per_view = 5;
    let budget = walk_visit_budget(o.max_files_per_view);
    assert_eq!(budget, 100);

    let found = collect_walk(
        &main,
        &common_of(&main),
        None,
        |rel| rel != "a.py",
        usize::MAX,
        budget,
    );
    assert!(found.stopped);
    assert_eq!(found.visited, budget);
    assert!(found.files.is_empty(), "{:?}", found.files);

    let report = scan_repo(&conn, &common_of(&main), &o).unwrap();
    assert!(report.views[0].truncated);
    assert_eq!(report.views[0].files, 1, "only the tracked file");
    assert_eq!(
        super::scan::test_hooks::last_walk(),
        Some((0, budget, true)),
        "the scan's own walk stopped at the budget"
    );
    assert_invariants(&conn);
}

// ---- a link inside the repo never carries a scan into a protected folder ----------------

/// Move `<repo>/<dir>` into `<home>/Documents/<name>` and leave a symlink to it.
fn swap_dir_for_link_into_documents(fx: &Fx, repo: &Path, dir: &str, name: &str) -> PathBuf {
    let docs = fx.home.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let target = docs.join(name);
    std::fs::rename(repo.join(dir), &target).unwrap();
    std::os::unix::fs::symlink(&target, repo.join(dir)).unwrap();
    target
}

#[test]
fn a_tracked_file_under_a_directory_swapped_for_a_link_into_a_protected_folder_is_never_read() {
    let fx = Fx::new();
    let main = fx.init_repo("lnk");
    write(&main, "top.py", &py(&["top_fn"]));
    write(&main, "src/a.py", &py(&["protected_secret_fn"]));
    write(&main, "src/deep/b.py", &py(&["protected_deep_fn"]));
    write(&main, "keep/c.py", &py(&["kept_fn"]));
    fx.commit_all(&main, "init");
    swap_dir_for_link_into_documents(&fx, &main, "src", "source");

    // Readable on purpose: the old scan stat'ed and read it, and parsed its defs.
    let conn = db();
    let report = scan(&conn, &fx, &main);
    let paths: Vec<String> = rows(&conn, &main).into_iter().map(|r| r.0).collect();
    assert_eq!(paths, vec!["keep/c.py", "top.py"], "{paths:?}");
    assert_eq!(report.views[0].files, 2);
    assert!(hit_paths(&conn, "protected_secret_fn").is_empty());
    assert!(hit_paths(&conn, "protected_deep_fn").is_empty());
    assert_eq!(hit_paths(&conn, "kept_fn"), vec!["keep/c.py"]);
    assert_invariants(&conn);

    // Locked: the scan still succeeds without that file and without an error.
    let _lock = super::testutil::Locked::new(&fx.home.join("Documents"));
    let conn = db();
    let report = scan(&conn, &fx, &main);
    assert_eq!(report.views[0].status, "ok");
    assert_eq!(report.views[0].files, 2);
}

#[test]
fn a_directory_that_is_a_link_to_an_ordinary_place_is_still_scanned() {
    let fx = Fx::new();
    let main = fx.init_repo("lnk-ok");
    write(&main, "lib/b.py", &py(&["linked_fn"]));
    fx.commit_all(&main, "init");
    let outside = fx.root.join("outside");
    std::fs::rename(main.join("lib"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, main.join("lib")).unwrap();
    let conn = db();
    scan(&conn, &fx, &main);
    let paths: Vec<String> = rows(&conn, &main).into_iter().map(|r| r.0).collect();
    assert_eq!(paths, vec!["lib/b.py"]);
}

#[test]
fn a_directory_event_is_resolved_before_it_is_stat_ed() {
    let fx = Fx::new();
    let main = fx.init_repo("evt");
    write(&main, "src/lib/x.py", &py(&["event_fn"]));
    write(&main, "other/y.py", &py(&["other_fn"]));
    fx.commit_all(&main, "init");
    let conn = db();
    scan(&conn, &fx, &main);

    // Control: a real directory is stat'ed once.
    let _ = super::scan::test_hooks::take_lstats();
    let home = Some(fx.home.as_path());
    let got = super::scan::expand_dir_events_with(&conn, &[main.join("other")], home).unwrap();
    assert_eq!(got.dir_events, 1);
    assert_eq!(
        super::scan::test_hooks::take_lstats(),
        vec![main.join("other")]
    );

    // `src` is swapped for a link into the fake Documents (locked, so a stat
    // inside it would fail). The event for `src/lib` stats nothing.
    swap_dir_for_link_into_documents(&fx, &main, "src", "source");
    let _lock = super::testutil::Locked::new(&fx.home.join("Documents"));
    let got = super::scan::expand_dir_events_with(&conn, &[main.join("src/lib")], home).unwrap();
    assert_eq!(got.dir_events, 1);
    assert_eq!(
        super::scan::test_hooks::take_lstats(),
        Vec::<PathBuf>::new(),
        "no lstat of anything reachable through the protected link"
    );
    // The stored row below it is still handed on, so rescan_paths drops it.
    assert_eq!(got.paths, vec![main.join("src/lib/x.py")]);
}

// ---- session signals --------------------------------------------------------------------

#[test]
fn the_signals_table_has_its_columns_and_a_migration_rerun_adds_nothing() {
    let conn = db();
    assert!(store::signals_ready(&conn));
    let cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('codemap_signals') ORDER BY cid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(cols, ["cwd", "last_at", "source", "common_dir", "status"]);
    store::signal_upsert(&conn, "/w/a", "session_start", 10).unwrap();
    let schema_before: Vec<String> = conn
        .prepare("SELECT sql FROM sqlite_master ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .map(|r| r.unwrap().unwrap_or_default())
        .collect();
    crate::storage::migrations::run(&conn).unwrap();
    let schema_after: Vec<String> = conn
        .prepare("SELECT sql FROM sqlite_master ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .map(|r| r.unwrap().unwrap_or_default())
        .collect();
    assert_eq!(schema_before, schema_after, "the rerun changes no schema");
    assert_eq!(
        store::list_signals(&conn, 0).unwrap().len(),
        1,
        "and keeps the rows"
    );
}

#[test]
fn a_signal_upsert_is_one_row_per_cwd_and_keeps_the_newest_time_and_the_daemon_columns() {
    let conn = db();
    store::signal_upsert(&conn, "/w/a", "session_start", 1_000).unwrap();
    store::signal_upsert(&conn, "/w/b", "post_tool_use", 2_000).unwrap();
    store::signal_resolve(&conn, "/w/a", Some("/w/a/.git"), "repo").unwrap();
    // A later fire for /w/a: newer time, new source, the resolved columns stay.
    store::signal_upsert(&conn, "/w/a", "post_tool_use", 3_000).unwrap();
    // A late, out-of-order fire never moves the time back.
    store::signal_upsert(&conn, "/w/a", "session_start", 500).unwrap();
    let all = store::list_signals(&conn, 0).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].cwd, "/w/a");
    assert_eq!(all[0].last_at, 3_000);
    assert_eq!(all[0].common_dir.as_deref(), Some("/w/a/.git"));
    assert_eq!(all[0].status.as_deref(), Some("repo"));
    assert_eq!(all[1].cwd, "/w/b");
    assert_eq!(
        (all[1].common_dir.clone(), all[1].status.clone()),
        (None, None)
    );
    // The window is inclusive at its start.
    assert_eq!(store::list_signals(&conn, 2_000).unwrap().len(), 2);
    assert_eq!(store::list_signals(&conn, 2_001).unwrap().len(), 1);
}

#[test]
fn resolving_a_signal_to_the_same_answer_writes_nothing_and_gc_drops_old_rows() {
    let conn = db();
    store::signal_upsert(&conn, "/w/a", "session_start", 1_000).unwrap();
    store::signal_resolve(&conn, "/w/a", Some("/w/a/.git"), "repo").unwrap();
    let before = conn.total_changes();
    store::signal_resolve(&conn, "/w/a", Some("/w/a/.git"), "repo").unwrap();
    assert_eq!(conn.total_changes(), before, "unchanged answer, no write");
    store::signal_resolve(&conn, "/w/a", None, "not-a-repo").unwrap();
    assert_eq!(conn.total_changes(), before + 1);
    store::signal_upsert(&conn, "/w/new", "session_start", 9_000).unwrap();
    assert_eq!(store::signal_gc(&conn, 5_000).unwrap(), 1);
    let left = store::list_signals(&conn, 0).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].cwd, "/w/new");
}

#[test]
fn a_database_without_the_signals_table_reads_as_no_signals() {
    let conn = db();
    conn.execute_batch("DROP TABLE codemap_signals").unwrap();
    assert!(!store::signals_ready(&conn));
    assert!(store::list_signals(&conn, 0).unwrap().is_empty());
    assert_eq!(store::signal_gc(&conn, 10).unwrap(), 0);
}

#[test]
fn record_signal_normalizes_the_cwd_lexically_and_skips_relative_and_disabled() {
    let conn = db();
    let rec = |cwd: &str, source: &str, at: i64, disabled: bool| {
        super::record_signal_at(&conn, Path::new(cwd), source, at, disabled).unwrap()
    };
    rec("/w/repo/./sub/..", "session_start", 100, false);
    rec("relative/dir", "session_start", 100, false);
    rec("/w/off", "session_start", 100, true);
    let got = store::list_signals(&conn, 0).unwrap();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].cwd, "/w/repo");
    assert_eq!(got[0].source, "session_start");
    assert_eq!(got[0].last_at, 100);
    // A protected cwd is recorded too: the signal does no check and no stat; the
    // daemon decides what to do with it.
    rec("/Users/nobody/Documents/proj", "post_tool_use", 200, false);
    assert_eq!(store::list_signals(&conn, 0).unwrap().len(), 2);
}
