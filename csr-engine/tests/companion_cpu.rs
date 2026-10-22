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
