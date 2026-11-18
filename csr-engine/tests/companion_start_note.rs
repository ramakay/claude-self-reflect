//! The pane's start note is read once, off the frame path: frames after the first do no read.
#[path = "support/pane_home.rs"]
mod pane_home;
use csr_engine::companion::{cli::PaneArgs, pane::Pane, render::kitty::set_cell_override, start};
use pane_home::*;

fn args_with(mode: &str, sid: &str) -> PaneArgs {
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
        mode,
        "--page",
        "main",
        "--cols",
        "44",
        "--rows",
        "30",
        "--sid",
        sid,
    ])
    .pane
}

/// The measured parts the last frame drew: names of the Context card's bar segments.
fn segments(pane: &Pane) -> Vec<String> {
    pane.instrument_metrics
        .as_ref()
        .and_then(|m| m.dash.as_ref())
        .map(|d| d.segments.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn the_start_note_is_read_once_and_never_in_a_frame_after_the_first() {
    set_cell_override(Some((16, 34)));
    // A one-shot run reads it on its first frame, then keeps it.
    let (_home, paths) = state_home("15-fresh-start");
    let mut pane = Pane::new(paths, &args_with("--frame", "sess-now"));
    pane.pixels = true;
    pane.enable_cards();
    let before = start::reads();
    for i in 0..6 {
        pane.frame(44, 30, NOW + i as f64);
    }
    assert_eq!(
        start::reads() - before,
        1,
        "the note was read again in a later frame"
    );
    assert_eq!(segments(&pane), ["CSR injected"], "the note was not used");

    // A running pane takes it from a thread: no read in the frames, and the card gets it.
    let (_home, paths) = state_home("15-fresh-start");
    let mut pane = Pane::new(paths, &args_with("--watch", "sess-now"));
    pane.pixels = true;
    pane.enable_cards();
    let before = start::reads();
    let mut got = false;
    for i in 0..400 {
        pane.frame(44, 30, NOW + i as f64 * 0.01);
        if segments(&pane) == ["CSR injected"] {
            got = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(got, "the running pane never got the note");
    // Frames at the same second, and later ones inside the refresh interval, read nothing.
    for i in 0..6 {
        pane.frame(44, 30, NOW + 1. + i as f64);
    }
    assert_eq!(start::reads() - before, 1, "a frame read the note again");
}
