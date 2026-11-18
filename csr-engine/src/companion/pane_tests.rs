#[test]
fn the_lost_card_row_is_the_whole_thread_after_the_title() {
    let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let title = lines(&["Lost and found"]);
    // The thread repeats the title: the embedded title is not the card's.
    let wrapped = lines(&["Investigate X", "Lost and found", "Investigate X"]);
    let rows = lines(&[
        "Lost and found",
        "Investigate X",
        "Lost and found",
        "Investigate X",
        "keys",
    ]);
    assert_eq!(lost_card_first_row(&rows, &title, &wrapped), Some(1));
    // An observation list above the card reads the same: the card is the last match.
    let one = lines(&["Investigate X"]);
    let rows = lines(&[
        "Lost and found",
        "Investigate X",
        "Lost and found",
        "Investigate X",
    ]);
    assert_eq!(lost_card_first_row(&rows, &title, &one), Some(3));
    // A card cut short keeps its title and first line.
    let long = lines(&["Investigate X", "and the rest"]);
    let rows = lines(&["Lost and found", "Investigate X"]);
    assert_eq!(lost_card_first_row(&rows, &title, &long), Some(1));
    // No such card, or an empty thread.
    assert_eq!(lost_card_first_row(&lines(&["keys"]), &title, &one), None);
    assert_eq!(lost_card_first_row(&rows, &title, &[]), None);
}

use super::*;

#[test]
fn the_road_is_off_unless_the_env_or_the_plugin_option_turns_it_on() {
    let none = json!({});
    assert!(!road_on(&none, None));
    assert!(!road_on(&none, Some("0")));
    assert!(road_on(&none, Some("1")));
    let option = |name: &str, v: Value| json!({"pluginConfigs": {name: {"options": {"road": v}}}});
    assert!(road_on(&option("csr-companion@csr", json!(true)), None));
    assert!(road_on(&option("csr-companion", json!(true)), None));
    assert!(!road_on(&option("csr-companion@csr", json!(false)), None));
    assert!(!road_on(&option("csr-companion@csr", json!("yes")), None));
    assert!(!road_on(&option("other@csr", json!(true)), None));
    assert!(!road_on(
        &json!({"pluginConfigs": {"csr-companion@csr": {"options": {"dreams": true}}}}),
        None
    ));
}
#[test]
fn test_the_legend_keeps_close_at_phone_width() {
    let (s, spans) = legend("main", 37, Some(42));
    assert!(len(&plain(&s)) <= 37);
    assert!(spans.iter().any(|(_, _, k)| k == "q"));
}
#[test]
fn test_log_marks_go_before_keys_do() {
    let (s, spans) = legend("log", 30, None);
    assert!(!plain(&s).contains("changed"));
    assert_eq!(
        spans.iter().map(|(_, _, s)| s.as_str()).collect::<Vec<_>>(),
        vec!["d", "q"]
    );
}

fn quiet_pane() -> (tempfile::TempDir, Pane) {
    use clap::Parser;
    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        pane: PaneArgs,
    }
    let home = tempfile::tempdir().unwrap();
    let args = T::parse_from([
        "t",
        "--watch",
        "--page",
        "main",
        "--sid",
        "sess-x",
        "--repo",
        home.path().to_str().unwrap(),
    ])
    .pane;
    let pane = Pane::new(Paths::new(home.path().to_path_buf()), &args);
    (home, pane)
}

fn lost_popup(key: &str) -> Popup {
    Popup {
        card: ObsCard::Lost,
        line: key.to_owned(),
        index: 0,
        copied: false,
        note: None,
        scroll: 0,
    }
}

#[test]
fn r_and_s_on_the_handed_popup_type_only_what_a_drawn_frame_made() {
    let (_home, mut pane) = quiet_pane();
    pane.obs_lines[ObsCard::Handed as usize] = vec!["handed".into()];
    pane.popup = Some(Popup {
        card: ObsCard::Handed,
        line: "handed".into(),
        index: 0,
        copied: false,
        note: None,
        scroll: 0,
    });
    pane.popup_send = Some("resume text".into());
    // No frame has drawn this popup: nothing is typed and nothing is said.
    for key in ["r", "s"] {
        assert!(pane.key(key), "{key} belongs to the popup");
        assert!(pane.popup.as_ref().unwrap().note.is_none(), "{key} typed");
    }
    // Once a frame has drawn it, `r` runs the send path (no Claude pane here: it says so).
    pane.popup_for = Some((ObsCard::Handed, "handed".into()));
    assert!(pane.key("r"));
    assert!(
        pane.popup.as_ref().unwrap().note.is_some(),
        "r ran the send path"
    );
    pane.popup.as_mut().unwrap().note = None;
    assert!(pane.key("s"));
    assert!(pane.popup.as_ref().unwrap().note.is_some(), "s still works");
    // `x` does not dismiss it, and the digits do not open another item
    pane.key("x");
    pane.key("2");
    assert_eq!(pane.popup.as_ref().unwrap().card, ObsCard::Handed);
}

#[test]
fn observe_four_is_what_csr_told_claude() {
    assert_eq!(observe_key("observe:4:0"), Some((ObsCard::Handed, 0)));
    assert_eq!(observe_key("observe:3:2"), Some((ObsCard::Lost, 2)));
    assert_eq!(observe_key("observe:2:1"), Some((ObsCard::More, 1)));
}

#[test]
fn a_conversation_read_nobody_waits_for_is_dropped_and_leaves_the_pane_idle() {
    let (_home, mut pane) = quiet_pane();
    let (send, receive) = std::sync::mpsc::channel::<Value>();
    pane.handoff_wait = Some(("k1".into(), receive));
    pane.popup = Some(lost_popup("k1"));
    pane.poll_handoff();
    assert!(
        pane.handoff_pending(),
        "its popup is open: it keeps waiting"
    );
    // the popup closes while the read runs: the next frame ends the wait, finished or not
    pane.popup = None;
    pane.frame(40, 20, 1_800_000_000.);
    assert!(!pane.handoff_pending());
    assert!(!pane.busy());
    assert!(send.send(json!({})).is_err(), "nothing listens any more");
    // a finished read of the open popup is taken, and the pane is idle again
    let (send, receive) = std::sync::mpsc::channel::<Value>();
    pane.handoff_wait = Some(("k2".into(), receive));
    pane.popup = Some(lost_popup("k2"));
    send.send(json!({"conv": "c"})).unwrap();
    pane.poll_handoff();
    assert!(!pane.handoff_pending());
    assert_eq!(pane.handoff_done["k2"]["conv"], "c");
    // a read for a thread the popup has moved off is dropped
    let (_send, receive) = std::sync::mpsc::channel::<Value>();
    pane.handoff_wait = Some(("k3".into(), receive));
    pane.poll_handoff();
    assert!(!pane.handoff_pending());
}

#[test]
fn opening_the_same_lost_popup_again_leaves_no_draft_for_c_or_s_to_use() {
    let (_home, mut pane) = quiet_pane();
    pane.obs_lines[ObsCard::Lost as usize] = vec!["k1".into()];
    pane.popup = Some(lost_popup("k1"));
    pane.popup_copy = Some("the old draft".into());
    pane.popup_send = Some("the old draft".into());
    pane.popup_for = Some((ObsCard::Lost, "k1".into()));
    pane.handoff_done
        .insert("k1".into(), json!({"conv": "old"}));
    let (_send, receive) = std::sync::mpsc::channel::<Value>();
    pane.handoff_wait = Some(("k1".into(), receive));
    // Tab on a one-thread list opens the same popup again
    assert!(pane.open_observation(ObsCard::Lost, 0));
    assert!(pane.popup_copy.is_none() && pane.popup_send.is_none());
    assert!(!pane.popup_drawn());
    assert!(!pane.handoff_done.contains_key("k1"));
    assert!(
        !pane.handoff_pending(),
        "the read before it is not the one wanted now"
    );
    pane.key("c");
    assert!(pane.take_copy().is_none());
    pane.key("s");
    assert!(
        pane.popup.as_ref().unwrap().note.is_none(),
        "nothing was typed"
    );
}

#[test]
fn a_draft_still_being_read_is_no_text_to_copy_or_type() {
    let items = vec![json!({"key": "k1", "text": "T", "line": "T", "reading": true})];
    let at = PopupAt {
        card: ObsCard::Lost,
        index: 0,
        copied: false,
        note: None,
        scroll: 0,
    };
    let d = observation::detail(&items, at).unwrap();
    assert_eq!(action_texts(Some(&d)), (None, None));
    assert_eq!(action_texts(None), (None, None));
    let ready = vec![json!({"key": "k1", "text": "T", "line": "T",
                            "handoff": {"conv": "c-1", "summary": null, "stats": null}})];
    let d = observation::detail(&ready, at).unwrap();
    let (copy, send) = action_texts(Some(&d));
    assert!(copy
        .as_deref()
        .is_some_and(|c| c.contains("csr_transcript")));
    assert_eq!(copy, send);
}

#[test]
fn opening_the_same_lost_popup_again_changes_the_page_mark_and_starts_the_read_at_once() {
    let (_home, mut pane) = quiet_pane();
    let thread = json!({"key": "k1", "text": "T", "line": "T", "origin_sid": "sid-1"});
    pane.lost_items = vec![thread];
    pane.obs_lines[ObsCard::Lost as usize] = vec!["k1".into()];
    assert!(pane.open_observation(ObsCard::Lost, 0));
    // a frame reads the conversation and settles: the facts are kept, nothing waits
    let first = pane.lost_with_handoff("k1");
    while pane.handoff_pending() {
        std::thread::sleep(std::time::Duration::from_millis(5));
        pane.poll_handoff();
    }
    assert!(pane.handoff_done.contains_key("k1"), "{first:?}");
    // the watch loop draws when the mark changed over a key: Tab on a one-thread list is one
    let mark = pane.ui_mark();
    pane.key("tab");
    assert_ne!(pane.ui_mark(), mark, "the loop would not redraw");
    assert!(
        !pane.handoff_done.contains_key("k1"),
        "the old read is gone"
    );
    // that redraw starts the read again, and the popup shows it reading
    let again = pane.lost_with_handoff("k1");
    assert!(pane.handoff_pending() || pane.handoff_done.contains_key("k1"));
    assert!(
        again[0]["reading"] == true || again[0]["handoff"].is_object(),
        "{again:?}"
    );
    // and every further Tab is another change
    let mark = pane.ui_mark();
    pane.key("tab");
    assert_ne!(pane.ui_mark(), mark);
}

#[test]
fn a_scroll_that_woke_a_resting_pane_moves_the_page_once() {
    use crate::companion::wake_input;
    // What the watch loop does with each event it replays: a wheel moves the page by
    // `scroll_step`, nothing else moves it.
    fn replay(pane: &mut Pane) {
        for event in wake_input::take_events() {
            if let crossterm::event::Event::Mouse(m) = event {
                pane.scroll(wake_input::scroll_step(m.kind));
            }
        }
    }
    let (_home, mut pane) = quiet_pane();
    assert_eq!(pane.top, 0);
    // The resting pane woke on a wheel (Shift held: button 69), then a key was typed.
    std::env::set_var(wake_input::VARIABLE, wake_input::encode(b"\x1b[<69;3;4Mb"));
    wake_input::take_from_env();
    replay(&mut pane);
    assert_eq!(
        pane.top, 3,
        "one notch is one step down, the key moves nothing"
    );
    // A later exec of the pane (a rest, a new engine) carries nothing, and a second look at
    // the same bytes finds none: the page stays where the person left it.
    wake_input::take_from_env();
    replay(&mut pane);
    assert_eq!(pane.top, 3, "the scroll is not delivered twice");
    // Two notches are two steps: the count is the scrolls, not one per wake.
    std::env::set_var(
        wake_input::VARIABLE,
        wake_input::encode(b"\x1b[<65;3;4M\x1b[<65;3;4M"),
    );
    wake_input::take_from_env();
    replay(&mut pane);
    assert_eq!(pane.top, 9);
}
