//! Path resolution, per-view presence and exact-name lookup.

use rusqlite::Connection;

use super::files::{lookup_files, presence_summary};
use super::testutil::{db, map_fixture, scan_repo_into, MapFixture};
use super::{files_for, query_exact};

fn scanned() -> (MapFixture, Connection) {
    let m = map_fixture();
    let conn = db();
    scan_repo_into(&conn, &m.fx, &m.main);
    (m, conn)
}

fn views(file: &super::CodemapFile) -> Vec<(String, bool, i64)> {
    file.presence
        .iter()
        .map(|p| (p.view.clone(), p.same, p.dirty))
        .collect()
}

#[test]
fn an_absolute_path_resolves_through_its_view_and_groups_presence_by_blob() {
    let (m, conn) = scanned();
    // The path is given inside the edited worktree; the answer is repo-wide.
    let arg = m.diff.join("src/radio.py");
    let files = files_for(&conn, &arg.to_string_lossy(), 10).unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    let f = &files[0];
    assert_eq!(f.path, "src/radio.py");
    assert_eq!(f.repo, m.main.join(".git").to_string_lossy());
    assert_eq!(
        views(f),
        vec![
            ("main".to_string(), true, 0),
            ("mapfx-diff".to_string(), false, 1),
            ("mapfx-same".to_string(), true, 0),
        ],
        "main first, then by worktree path; the edited one differs and is dirty"
    );
    // Definitions come from the blob most views hold, not the edited one.
    let names: Vec<&str> = f.defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["alpha_one", "beta_two"]);
    assert_eq!(f.defs_total, 2);
    assert_eq!(f.defs[0].kind, "function");
    assert_eq!((f.defs[0].span_start, f.defs[0].span_end), (0, 1));
    assert_eq!(
        presence_summary(&f.presence),
        "same blob in main, mapfx-same; differs in mapfx-diff (dirty)"
    );

    // The main checkout's own absolute path gives the same answer.
    let again = files_for(&conn, &m.main.join("src/radio.py").to_string_lossy(), 10).unwrap();
    assert_eq!(again, files);
    // `..` is resolved lexically.
    let dotted = format!("{}/lib/../src/radio.py", m.main.display());
    assert_eq!(files_for(&conn, &dotted, 10).unwrap(), files);
}

#[test]
fn an_absolute_path_the_map_does_not_hold_resolves_to_nothing() {
    let (m, conn) = scanned();
    assert!(files_for(&conn, "/somewhere/else/src/radio.py", 10)
        .unwrap()
        .is_empty());
    let missing = m.main.join("src/not_there.py");
    assert!(files_for(&conn, &missing.to_string_lossy(), 10)
        .unwrap()
        .is_empty());
    // A directory is not a file.
    assert!(files_for(&conn, &m.main.join("src").to_string_lossy(), 10)
        .unwrap()
        .is_empty());
}

#[test]
fn a_repo_relative_path_matches_exactly() {
    let (_m, conn) = scanned();
    for arg in ["src/radio.py", "./src/radio.py", "  src/radio.py "] {
        let files = files_for(&conn, arg, 10).unwrap();
        assert_eq!(files.len(), 1, "{arg}");
        assert_eq!(files[0].path, "src/radio.py");
    }
}

#[test]
fn a_suffix_matches_on_a_path_boundary_only() {
    let (_m, conn) = scanned();
    let by_name = files_for(&conn, "radio.py", 10).unwrap();
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].path, "src/radio.py");
    let by_dirs = files_for(&conn, "util/helpers.py", 10).unwrap();
    assert_eq!(by_dirs[0].path, "lib/util/helpers.py");
    // `adio.py` ends the path but not at a `/`.
    assert!(files_for(&conn, "adio.py", 10).unwrap().is_empty());
    // Case matters, like the filesystem on Linux and git itself.
    assert!(files_for(&conn, "RADIO.PY", 10).unwrap().is_empty());
    // LIKE wildcards in the argument are literal.
    assert!(files_for(&conn, "r%o.py", 10).unwrap().is_empty());
    assert!(files_for(&conn, "r_dio.py", 10).unwrap().is_empty());
}

#[test]
fn an_ambiguous_suffix_is_capped_and_counted() {
    let (_m, conn) = scanned();
    let all = lookup_files(&conn, "dup.py", 10).unwrap();
    assert_eq!(all.files.len(), 2);
    assert_eq!(all.total_paths, 2);
    assert_eq!(all.files[0].path, "pkg_a/dup.py");
    assert_eq!(all.files[1].path, "pkg_b/dup.py");

    let capped = lookup_files(&conn, "dup.py", 1).unwrap();
    assert_eq!(capped.files.len(), 1);
    assert_eq!(
        capped.total_paths, 2,
        "the cap does not hide the match count"
    );
}

#[test]
fn unknown_paths_and_a_missing_or_empty_map_resolve_to_nothing() {
    let (_m, conn) = scanned();
    assert!(files_for(&conn, "nope/never.py", 10).unwrap().is_empty());
    assert!(files_for(&conn, "", 10).unwrap().is_empty());
    assert!(files_for(&conn, "   ", 10).unwrap().is_empty());
    assert!(files_for(&conn, "src/radio.py", 0).unwrap().is_empty());
    // Migrated but never filled.
    assert!(files_for(&db(), "src/radio.py", 10).unwrap().is_empty());
    // Tables absent (a database from an older binary).
    let bare = Connection::open_in_memory().unwrap();
    assert!(files_for(&bare, "src/radio.py", 10).unwrap().is_empty());
    assert!(query_exact(&bare, "alpha_one", 10).unwrap().is_empty());
}

#[test]
fn presence_summary_names_one_view_and_separates_several_differing_blobs() {
    use super::ViewPresence;
    let p = |view: &str, blob: &str, same: bool, dirty: i64| ViewPresence {
        view: view.into(),
        worktree_path: format!("/w/{view}"),
        branch: None,
        blob_oid: blob.into(),
        same,
        dirty,
    };
    assert_eq!(
        presence_summary(&[p("main", "aaaaaaaaaa", true, 0)]),
        "only in main"
    );
    assert_eq!(
        presence_summary(&[
            p("main", "aaaaaaaaaa", true, 0),
            p("b", "bbbbbbbbbb", false, 2),
            p("c", "cccccccccc", false, 1),
            p("d", "bbbbbbbbbb", false, 0),
        ]),
        "same blob in main; differs in b (untracked), d [bbbbbbb]; differs in c (dirty) [ccccccc]"
    );
    let many: Vec<ViewPresence> = (0..9)
        .map(|i| p(&format!("v{i}"), "aaaaaaaaaa", true, 0))
        .collect();
    assert_eq!(
        presence_summary(&many),
        "same blob in v0, v1, v2, v3, v4, v5, +3 more"
    );
}

#[test]
fn exact_name_lookup_ignores_longer_names_and_case() {
    let (_m, conn) = scanned();
    let hits = query_exact(&conn, "alpha_one", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "src/radio.py");
    assert_eq!(
        hits[0].same_in,
        vec!["main".to_string(), "mapfx-same".to_string()]
    );
    assert_eq!(hits[0].differs_in, vec!["mapfx-diff*".to_string()]);
    // The fuzzy search finds fragments; the exact lookup does not.
    assert!(!super::query(&conn, "alpha", 10).unwrap().is_empty());
    assert!(query_exact(&conn, "alpha", 10).unwrap().is_empty());
    assert!(query_exact(&conn, "ALPHA_ONE", 10).unwrap().is_empty());
    assert!(query_exact(&conn, "", 10).unwrap().is_empty());
    // A definition only the edited view has.
    let delta = query_exact(&conn, "delta_four", 10).unwrap();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].same_in, vec!["mapfx-diff*".to_string()]);
}
