//! `CSR_NO_DREAMING` stops dreams, and the terminal pane (a separate process that herdr starts)
//! says so. The app writes the fact into the session's usage file; the pane reads it back as
//! `claude_fields.dreams_stopped`. No test sets process env.
use csr_engine::companion::{
    paths::Paths,
    render::{
        cards::dream_facts,
        instrument::{text_rows, Composition, Open},
    },
    sources,
};
use serde_json::{json, Value};

const NOW: f64 = 1792320000.;
const WORDS: &str = "CSR_NO_DREAMING is set where Claude Code runs";

fn usage_file(extra: Value) -> (tempfile::TempDir, Paths) {
    let d = tempfile::tempdir().unwrap();
    let paths = Paths::new(d.path().into());
    std::fs::create_dir_all(paths.keep.join("usage")).unwrap();
    let mut body = json!({ "at": NOW - 30., "context": { "window": 200000, "tokens": 1000 } });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    std::fs::write(paths.keep.join("usage/s.json"), body.to_string()).unwrap();
    (d, paths)
}

#[test]
fn a_usage_file_that_says_stopped_reaches_the_pane_as_a_claude_field() {
    let (_d, paths) = usage_file(json!({ "dreamsStopped": true }));
    let kept = sources::kept_status(&paths, "s", NOW);
    assert_eq!(kept["fields"]["dreams_stopped"], true);
}

#[test]
fn a_usage_file_without_the_flag_adds_no_field() {
    let (_d, paths) = usage_file(json!({}));
    let kept = sources::kept_status(&paths, "s", NOW);
    assert!(kept["fields"].get("dreams_stopped").is_none());
}

fn data(row_on: bool, stopped: bool) -> Value {
    let mut data = json!({
        "dreams_on": row_on,
        "dreams": [{ "id": "d1", "session": "s", "at": NOW - 50., "line": "Is it a leak?",
            "confidence": 0.5, "answer": "helpful" }],
        "chapter": null,
    });
    if stopped {
        data["claude_fields"] = json!({ "dreams_stopped": true });
    }
    data
}

fn rows_of(data: &Value) -> String {
    let session = json!({ "id": "s", "main": [1], "prompts": [1] });
    let c = Composition::build(data, &session, NOW);
    text_rows(&c, 300, 40, Open::default()).join("\n")
}

#[test]
fn the_dreamer_card_says_stopped_with_the_row_on_or_off_and_offers_nothing() {
    for row_on in [true, false] {
        let rows = rows_of(&data(row_on, true));
        assert!(rows.contains(WORDS));
        // The headline says it once, and no line says the dreamer is at work.
        assert_eq!(rows.matches("Dreams are stopped.").count(), 1);
        assert!(!rows.contains("looks as you work"));
        // With the row off too, unsetting the variable is not enough, and the words say so.
        assert_eq!(rows.contains("also off in /config"), !row_on);
        assert!(!rows.contains("Press 1 to turn dreams on"));
        assert!(!rows.contains("Dreams are off"));
    }
}

#[test]
fn the_dreams_left_line_is_gone_while_stopped_and_back_when_not() {
    assert!(dream_facts(&data(true, true), "s").is_none());
    assert!(dream_facts(&data(true, false), "s").is_some());
}

#[test]
fn without_the_variable_the_row_off_offer_is_unchanged() {
    let rows = rows_of(&data(false, false));
    assert!(rows.contains("Press 1 to turn dreams on"));
    assert!(!rows.contains(WORDS));
}

#[test]
fn the_dreams_page_says_stopped_in_place_of_the_dreams_left_count() {
    use csr_engine::companion::pages::page_main;
    let session = json!({ "id": "s", "turn_end": 1 });
    let right = |data: &Value| {
        let page = page_main(data, &session, NOW, &Value::Null, None);
        let sections = page["sections"].as_array().unwrap();
        let dreams = sections.iter().find(|s| s["name"] == "dreams").unwrap();
        dreams["right"].as_str().unwrap().to_owned()
    };
    assert_eq!(right(&data(true, true)), "stopped");
    assert_eq!(right(&data(false, true)), "stopped");
    assert_ne!(right(&data(true, false)), "stopped");
}
