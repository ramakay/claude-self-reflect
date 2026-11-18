//! A pane with no session id yet must not show another session's story: the lookup used to run
//! `tags like '%conv_%'`, which matches the newest story of any conversation in any project.
#[path = "support/pane_home.rs"]
mod pane_home;
use csr_engine::companion::pane::Pane;
use pane_home::*;

const WORDS: &str = "Text parity is ready for review.";

fn shown(sid: &str) -> String {
    let (_home, paths) = state_home("02-running");
    let mut pane = Pane::new(paths, &args("main", sid, 76, 67));
    pane.pixels = false;
    pane.frame(76, 67, NOW).join("\n")
}

#[test]
fn a_pane_with_no_session_id_shows_no_story() {
    // The control: the session whose story it is shows it.
    let control = shown("sess-now");
    assert!(control.contains(WORDS), "control: {control}");
    let none = shown("");
    assert!(!none.contains(WORDS), "an empty sid showed a story: {none}");
    assert!(!none.contains("Story ·"), "{none}");
}
