use super::*;
use crate::companion::{dreams::DreamLog, paths::Paths};

/// The notice a pane for session `a` shows on coming back after an hour away, with `rows` as the
/// dream log: read the way a pane reads it (its own session's log), and handed to the picture the
/// way the pane hands it (`dream_records` and `look_stamps`).
fn away_notice(rows: &[Value]) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().into());
    std::fs::create_dir_all(&paths.keep).unwrap();
    let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(paths.keep.join("dreams.jsonl"), text).unwrap();
    let mut log = DreamLog::for_session("a");
    log.poll(&paths);
    let data = json!({
        "dream_records": log.records,
        "look_stamps": log.look_stamps(),
        "limits": {},
    });
    // The session was last active at 1000, then came back at 8200.
    let session = json!({"id":"a","compacts":[],"main":[],"prompts":[1000., 8200.]});
    Composition::build(&data, &session, 8300.).notice
}

#[test]
fn the_away_line_counts_every_look_of_another_session_in_the_same_repo() {
    let look = |session: &str, at: i64| json!({"kind":"look","session":session,"repo":"/r","at":at,"outcome":"none"});
    // Two looks of one other session during the absence, and one before it (not counted).
    let notice = away_notice(&[look("b", 500), look("b", 2000), look("b", 3000)]).unwrap();
    assert!(notice.contains("The dreamer looked 2×."), "{notice}");
    // Another session's, in the same repo: the same total.
    let notice = away_notice(&[look("b", 2000), look("c", 3000), look("a", 4000)]).unwrap();
    assert!(notice.contains("The dreamer looked 3×."), "{notice}");
    // One look, and a look still under way, which is not one.
    let started = json!({"kind":"look","session":"b","repo":"/r","at":5000,"outcome":"started"});
    let notice = away_notice(&[look("b", 2000), started]).unwrap();
    assert!(notice.contains("The dreamer looked 1×."), "{notice}");
}
