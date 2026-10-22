//! The pane's idle cost: a frame whose inputs did not change builds, rasters and encodes nothing.
use csr_engine::companion::{cli::PaneArgs, pane::Pane, paths::Paths};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, SystemTime},
};

const NOW: f64 = 1792320000.;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion")
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
fn copy_tree(from: &Path, to: &Path, home: &str) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let e = entry.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target, home);
        } else {
            let bytes = fs::read(e.path()).unwrap();
            match std::str::from_utf8(&bytes) {
                Ok(text) => fs::write(&target, fill(text, home)).unwrap(),
                Err(_) => fs::write(&target, &bytes).unwrap(),
            }
        }
    }
}
fn fill(text: &str, home: &str) -> String {
    text.replace("<REPO>", &repo().to_string_lossy())
        .replace("<HOME>", home)
}
fn utc() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| std::env::set_var("TZ", "UTC"));
}
/// The rich private HOME of the instrument tests: a session with a story, shifts and dreams.
fn rich_home() -> (tempfile::TempDir, Paths) {
    utc();
    let rich = fixture().join("rich");
    let d = tempfile::tempdir_in(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target")).unwrap();
    let home = d.path().to_string_lossy().into_owned();
    copy_tree(&fixture().join("home"), d.path(), &home);
    copy_tree(&rich.join("home"), d.path(), &home);
    let paths = Paths::new(d.path().into());
    let _ = fs::remove_file(&paths.db);
    let con = rusqlite::Connection::open(&paths.db).unwrap();
    con.execute_batch(include_str!("fixtures/companion/home.sql"))
        .unwrap();
    con.execute(
        "update intent_events set transcript_path=?",
        [paths
            .projects
            .join("fixture/sess-now.jsonl")
            .to_string_lossy()
            .as_ref()],
    )
    .unwrap();
    for (id, file) in [(1, "csr-engine/src/main.rs"), (2, "csr-engine/src/lib.rs")] {
        con.execute(
            "update witness_ledger set file=?1 where id=?2",
            rusqlite::params![repo().join(file).to_string_lossy(), id],
        )
        .unwrap();
    }
    con.execute_batch(&fill(
        &fs::read_to_string(rich.join("home.sql")).unwrap(),
        &home,
    ))
    .unwrap();
    let pins: Value =
        serde_json::from_str(&fs::read_to_string(rich.join("mtimes.json")).unwrap()).unwrap();
    for (rel, secs) in pins.as_object().unwrap() {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(d.path().join(rel))
            .unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(secs.as_f64().unwrap()))
            .unwrap();
    }
    (d, paths)
}
fn args(cols: usize, rows: usize) -> PaneArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct TestArgs {
        #[command(flatten)]
        pane: PaneArgs,
    }
    TestArgs::parse_from([
        "test",
        "--repo",
        repo().to_str().unwrap(),
        "--frame",
        "--page",
        "main",
        "--cols",
        &cols.to_string(),
        "--rows",
        &rows.to_string(),
        "--sid",
        "sess-now",
    ])
    .pane
}
fn pane_in(paths: &Paths) -> Pane {
    let mut pane = Pane::new(paths.clone(), &args(76, 67));
    pane.pixels = true;
    pane.enable_cards();
    pane
}

#[test]
fn the_same_inputs_raster_and_encode_once() {
    let (_home, paths) = rich_home();
    let mut pane = pane_in(&paths);
    let first = pane.frame(76, 67, NOW);
    assert_eq!(pane.placed.len(), 1, "the main frame is a picture");
    assert_eq!((pane.rasters, pane.encodes, pane.reused), (1, 1, 0));
    let (png, acts) = (pane.placed[0].png.clone(), pane.acts.clone());
    // Seconds go by with nothing the picture shows changing: nothing is drawn again.
    let until = pane.next_change.unwrap_or(NOW + 30.);
    let mut at = NOW + 1.;
    let mut frames = 0;
    while at < until.min(NOW + 30.) {
        let again = pane.frame(76, 67, at);
        assert_eq!(again, first);
        assert!(std::sync::Arc::ptr_eq(&png, &pane.placed[0].png));
        assert_eq!(pane.acts, acts, "cached hit rows match cached pixels");
        at += 1.;
        frames += 1;
    }
    assert!(frames > 0);
    assert_eq!((pane.rasters, pane.encodes), (1, 1));
    assert_eq!(pane.reused, frames);
}

#[test]
fn a_change_the_picture_shows_draws_it_again() {
    let (_home, paths) = rich_home();
    let mut pane = pane_in(&paths);
    pane.frame(76, 67, NOW);
    for key in ["b", "i", "l", "t"] {
        let before = pane.rasters;
        pane.key(key);
        pane.frame(76, 67, NOW);
        assert_eq!(pane.rasters, before + 1, "key {key} changes the picture");
    }
    // A different size, too.
    let before = pane.rasters;
    pane.frame(60, 50, NOW);
    assert_eq!(pane.rasters, before + 1);
    // And an ignored key changes nothing.
    let before = pane.rasters;
    pane.key("w");
    pane.frame(60, 50, NOW);
    assert_eq!(pane.rasters, before);
}

/// The key must miss no change: across hours, a pane that reuses its picture shows the PNG a
/// pane with no memory draws at that moment.
#[test]
fn a_reused_picture_is_the_picture_a_fresh_pane_draws() {
    let (_home, paths) = rich_home();
    let mut kept = pane_in(&paths);
    let mut at = NOW;
    let mut reused = 0;
    for step in 0..40 {
        kept.frame(76, 67, at);
        if kept.reused > reused {
            reused = kept.reused;
            let mut fresh = pane_in(&paths);
            fresh.frame(76, 67, at);
            assert_eq!(
                kept.placed[0].png,
                fresh.placed[0].png,
                "step {step} at +{}s reused a stale picture",
                at - NOW
            );
        }
        at += [7., 53., 240., 700.][step % 4];
    }
    assert!(reused > 0);
}

/// Frames from `NOW` on in one-second steps until `due` says so or `limit` seconds have gone.
fn idle_until_due(pane: &mut Pane, limit: f64) -> f64 {
    let mut at = NOW + 1.;
    while at < NOW + limit && !pane.due(at) {
        at += 1.;
    }
    at
}

#[test]
fn a_quiet_pane_is_not_due_and_reads_nothing_until_a_deadline() {
    let (_home, paths) = rich_home();
    let mut pane = pane_in(&paths);
    pane.frame(76, 67, NOW);
    let (queries, rasters, encodes) = (pane.queries(), pane.rasters, pane.encodes);
    let due = idle_until_due(&mut pane, 25.);
    // Quiet files, one connection, nothing in flight: not due until the picture's own deadline
    // (the fixture's next change is a minute edge or later) or the floor.
    let named = pane.next_change.map_or(f64::MAX, |t| t - NOW);
    assert!(
        (due - NOW - named.min(30.)).abs() <= 1.01 || due - NOW >= 25.,
        "due at +{}, deadline +{named}",
        due - NOW
    );
    assert_eq!(pane.queries(), queries, "the probe is not a query");
    assert_eq!((pane.rasters, pane.encodes), (rasters, encodes));
}

#[test]
fn a_change_to_what_the_frame_reads_makes_the_pane_due() {
    let (_home, paths) = rich_home();
    let mut pane = pane_in(&paths);
    pane.frame(76, 67, NOW);
    let transcript = paths.projects.join("fixture/sess-now.jsonl");
    let keep_agents = paths.keep.join("agents");
    let step = |pane: &mut Pane, at: f64, what: &str| {
        assert!(pane.due(at), "{what} is news");
        pane.frame(76, 67, at);
        assert!(!pane.due(at + 1.5), "{what} read once is quiet again");
    };
    let mut at = NOW + 2.;
    let original = fs::read(&transcript).unwrap();
    let mut longer = original.clone();
    longer.extend_from_slice(b"\n");
    fs::write(&transcript, &longer).unwrap();
    step(&mut pane, at, "an append");
    at += 3.;
    fs::write(&transcript, &original[..original.len() - 1]).unwrap();
    step(&mut pane, at, "a truncate");
    at += 3.;
    let aside = paths.projects.join("fixture/aside");
    fs::write(&aside, &original).unwrap();
    fs::rename(&aside, &transcript).unwrap();
    step(&mut pane, at, "an atomic replace");
    at += 3.;
    fs::remove_file(&transcript).unwrap();
    step(&mut pane, at, "a delete");
    at += 3.;
    fs::write(&transcript, &original).unwrap();
    step(&mut pane, at, "a recreate");
    at += 3.;
    fs::create_dir_all(&keep_agents).unwrap();
    fs::write(keep_agents.join("someone-else.json"), "{}").unwrap();
    step(&mut pane, at, "a new member of a kept folder");
}

#[test]
fn the_pane_heartbeat_is_not_news() {
    let (_home, paths) = rich_home();
    fs::create_dir_all(paths.keep.join("panes")).unwrap();
    let mut pane = pane_in(&paths);
    pane.frame(76, 67, NOW);
    for beat in 0..4 {
        fs::write(paths.keep.join("panes/sess-now.json"), format!("{beat}")).unwrap();
        assert!(!pane.due(NOW + 2. + beat as f64 * 2.));
    }
}

#[test]
fn a_commit_that_only_reaches_the_wal_makes_the_pane_due() {
    let (_home, paths) = rich_home();
    let writer = rusqlite::Connection::open(&paths.db).unwrap();
    let mode: String = writer
        .query_row("pragma journal_mode=wal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer.execute_batch("pragma wal_autocheckpoint=0").unwrap();
    let mut pane = pane_in(&paths);
    pane.frame(76, 67, NOW);
    let main = fs::metadata(&paths.db).unwrap();
    assert!(!pane.due(NOW + 6.));
    writer
        .execute_batch("create table if not exists wal_only(x); insert into wal_only values (1);")
        .unwrap();
    let after = fs::metadata(&paths.db).unwrap();
    assert_eq!(
        (main.len(), main.modified().unwrap()),
        (after.len(), after.modified().unwrap()),
        "the main file did not move"
    );
    assert!(pane.due(NOW + 8.), "the retained connection saw a commit");
    pane.frame(76, 67, NOW + 8.);
    assert!(!pane.due(NOW + 10.));
}

#[test]
fn work_in_flight_keeps_the_pane_due_until_it_lands() {
    let (_home, paths) = rich_home();
    let mut args = args(76, 67);
    args.frame = false;
    let mut pane = Pane::new(paths.clone(), &args);
    pane.pixels = true;
    pane.enable_cards();
    let release = pane.hold_carry();
    pane.frame(76, 67, NOW);
    for beat in 1..4 {
        assert!(
            pane.due(NOW + beat as f64 * 1.1),
            "the carry is still working"
        );
    }
    drop(release);
    let mut at = NOW + 5.;
    let landed = loop {
        pane.frame(76, 67, at);
        if !pane.due(at + 1.5) || at > NOW + 30. {
            break at;
        }
        at += 1.5;
    };
    assert!(landed <= NOW + 30., "the carry landed and the pane settled");
}

#[test]
fn a_deadline_makes_the_pane_due_and_the_floor_bounds_every_wait() {
    let (_home, paths) = rich_home();
    let mut pane = pane_in(&paths);
    pane.frame(76, 67, NOW);
    assert!(pane.due(NOW + 31.), "nothing goes unread for half a minute");
    if let Some(t) = pane.next_change.filter(|t| *t < NOW + 25.) {
        assert!(pane.due(t));
    }
}

#[test]
fn a_page_that_is_not_the_main_one_is_drawn_every_second_as_before() {
    let (_home, paths) = rich_home();
    let mut pane = pane_in(&paths);
    pane.key("s");
    assert_eq!(pane.view, "summary");
    pane.frame(76, 67, NOW);
    assert!(pane.due(NOW + 1.1));
}

#[test]
fn an_ignored_key_changes_nothing_the_page_shows() {
    let (_home, paths) = rich_home();
    let mut pane = pane_in(&paths);
    pane.frame(76, 67, NOW);
    let mark = pane.ui_mark();
    for key in ["w", "x", "9", "y"] {
        pane.key(key);
    }
    assert_eq!(pane.ui_mark(), mark);
    pane.key("b");
    assert_ne!(pane.ui_mark(), mark);
}
