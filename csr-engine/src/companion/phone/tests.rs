use super::*;
use crate::companion::pane::{legend_parts, Layout};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const READS: &str = include_str!("../../../tests/fixtures/companion/mapzoom/phone-reads.json");
const LEGENDS: &str = include_str!("../../../tests/fixtures/companion/mapzoom/legend-phone.json");

type Log = Arc<Mutex<Vec<String>>>;

/// A herdr that answers `layout` with a tab `width` wide and says yes to everything else, unless
/// `refuse` names a word its argv holds; every call it gets is kept.
fn fake(width: u64, refuse: &'static str) -> (Herdr, Log) {
    let log: Log = Arc::default();
    let seen = log.clone();
    let herdr = Herdr::with(
        "pane-7",
        Path::new("/nonexistent/herdr.toml"),
        Box::new(move |argv: &[&str]| {
            seen.lock().unwrap().push(argv.join(" "));
            if !refuse.is_empty() && argv.contains(&refuse) {
                return None;
            }
            if argv.get(1) == Some(&"layout") {
                return Some(
                    json!({"result": {"layout": {"area": {"width": width, "height": 40}}}})
                        .to_string(),
                );
            }
            Some(String::new())
        }),
    );
    (herdr, log)
}
fn calls(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

#[test]
fn test_is_mobile_matches_pane_py_on_every_recorded_layout() {
    let data: Value = serde_json::from_str(READS).unwrap();
    let cases = data["is_mobile"].as_array().unwrap();
    assert_eq!(cases.len(), 39);
    for case in cases {
        assert_eq!(
            is_mobile(
                case["layout"].as_str().unwrap(),
                case["threshold"].as_u64().unwrap()
            ),
            case["out"].as_bool().unwrap(),
            "{case}"
        );
    }
}
#[test]
fn test_the_threshold_reads_herdr_config_and_falls_back_on_any_doubt() {
    let data: Value = serde_json::from_str(READS).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    for case in data["threshold"].as_array().unwrap() {
        match case["config"].as_str() {
            Some(text) => std::fs::write(&path, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        assert_eq!(
            mobile_threshold(&path),
            case["out"].as_u64().unwrap(),
            "{case}"
        );
    }
}
#[test]
fn test_the_legend_matches_pane_py_for_every_phone_column_and_zoom_state() {
    let cases: Vec<Value> = serde_json::from_str(LEGENDS).unwrap();
    assert_eq!(cases.len(), 256);
    for case in &cases {
        let (text, spans) = legend_parts(
            case["view"].as_str().unwrap(),
            case["width"].as_u64().unwrap() as usize,
            case["reload"].as_u64(),
            Layout {
                phone: case["phone"].as_bool().unwrap(),
                column: case["column"].as_bool().unwrap(),
                zoomed: case["zoomed"].as_bool().unwrap(),
            },
        );
        let spans: Vec<Value> = spans.iter().map(|(a, b, k)| json!([a, b, k])).collect();
        assert_eq!(
            (json!(text), json!(spans)),
            (case["text"].clone(), case["spans"].clone()),
            "{}",
            case.to_string().chars().take(160).collect::<String>()
        );
    }
}
#[test]
fn test_a_desktop_never_zooms() {
    let (herdr, log) = fake(122, "");
    let mut phone = Phone::default();
    assert!(!phone.read(Some(&herdr), 46, 30, 0.));
    assert!(!phone.zoom_key(Some(&herdr)));
    assert!(!phone.zoomed);
    assert!(
        calls(&log).iter().all(|c| !c.contains("zoom")),
        "{:?}",
        calls(&log)
    );
}
#[test]
fn test_a_phone_zooms_on_and_gives_the_split_back_with_focus_on_claude() {
    let (herdr, log) = fake(50, "");
    let mut phone = Phone::default();
    assert!(phone.read(Some(&herdr), 17, 40, 0.));
    assert!(phone.zoom_key(Some(&herdr)));
    assert!(phone.zoomed);
    assert!(phone.zoom_key(Some(&herdr)));
    assert!(!phone.zoomed);
    assert_eq!(
        calls(&log),
        [
            "pane layout --pane pane-7",
            "pane zoom --pane pane-7 --on",
            "pane zoom --pane pane-7 --off",
            "pane focus --pane pane-7 --direction left",
        ]
    );
}
#[test]
fn test_a_herdr_that_refuses_the_zoom_leaves_the_pane_as_it_was() {
    let (herdr, log) = fake(50, "--on");
    let mut phone = Phone::default();
    phone.read(Some(&herdr), 17, 40, 0.);
    assert!(!phone.zoom_key(Some(&herdr)));
    assert!(!phone.zoomed);
    assert_eq!(calls(&log).len(), 2);
}
#[test]
fn test_outside_herdr_nothing_is_a_phone_and_z_does_nothing() {
    let mut phone = Phone::default();
    assert!(!phone.read(None, 17, 40, 0.));
    assert!(!phone.zoom_key(None));
}
#[test]
fn test_the_layout_is_read_again_on_a_new_size_or_after_five_seconds() {
    let (herdr, log) = fake(50, "");
    let mut phone = Phone::default();
    phone.read(Some(&herdr), 17, 40, 0.);
    phone.read(Some(&herdr), 17, 40, 4.9);
    assert_eq!(calls(&log).len(), 1);
    phone.read(Some(&herdr), 18, 40, 4.9);
    assert_eq!(calls(&log).len(), 2);
    phone.read(Some(&herdr), 18, 40, 10.);
    assert_eq!(calls(&log).len(), 3);
}
#[test]
fn test_a_desktop_taking_the_tab_unzooms_it_and_a_split_needs_no_call() {
    let (herdr, log) = fake(50, "");
    let mut phone = Phone::default();
    phone.read(Some(&herdr), 17, 40, 0.);
    phone.zoom_key(Some(&herdr));
    log.lock().unwrap().clear();
    phone.settle(Some(&herdr), false, false);
    assert!(!phone.zoomed);
    assert_eq!(calls(&log), ["pane zoom --pane pane-7 --off"]);
    phone.zoomed = true;
    log.lock().unwrap().clear();
    phone.settle(Some(&herdr), true, true);
    assert!(!phone.zoomed);
    assert!(calls(&log).is_empty());
    phone.zoomed = true;
    phone.settle(Some(&herdr), false, true);
    assert!(phone.zoomed, "a zoomed phone keeps its zoom");
}
