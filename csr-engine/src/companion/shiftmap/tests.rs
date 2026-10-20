use super::*;
use crate::companion::{db::Db, paths::Paths, story::parse_story_map};
use serde_json::{json, Value};
use std::sync::Mutex;

const CASES: &str = include_str!("../../../tests/fixtures/companion/mapzoom/shiftmap-cases.json");
const MOTION: &str = include_str!("../../../tests/fixtures/companion/mapzoom/mapmotion.json");
const COSTS: &str = include_str!("../../../tests/fixtures/companion/mapzoom/story-cost.json");
const NEWS: &str = include_str!("../../../tests/fixtures/companion/mapzoom/refresh-news.json");

/// CSR_MAP_HOLD_S is process-wide: the tests that set it take turns.
static ENV: Mutex<()> = Mutex::new(());

fn close(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-9,
        _ => a == b,
    }
}

#[test]
fn test_the_parsed_map_is_the_map_pane_py_draws() {
    let data: Value = serde_json::from_str(CASES).unwrap();
    for (name, shape) in data["maps"].as_object().unwrap() {
        let parsed = map_value(&parse_story_map(&shape["raw"]).unwrap());
        assert_eq!(parsed, shape["map"], "{name}");
    }
}
#[test]
fn test_shift_map_marked_equals_pane_py_for_every_shape_width_and_budget() {
    let data: Value = serde_json::from_str(CASES).unwrap();
    let cases = data["cases"].as_array().unwrap();
    assert!(cases.len() >= 300);
    for case in cases {
        let map = &data["maps"][case["name"].as_str().unwrap()]["map"];
        let (title, rows) = shift_map_marked(
            map,
            case["messages"].as_u64(),
            case["room"].as_u64().unwrap() as usize,
            case["shut"].as_bool().unwrap(),
            case["budget"].as_u64().map(|b| b as usize),
            case["source"].as_str(),
        );
        let rows: Vec<Value> = rows.iter().map(|(t, loud)| json!([t, loud])).collect();
        assert_eq!(
            (json!(title), json!(rows)),
            (case["title"].clone(), case["rows"].clone()),
            "{case}"
        );
        let (_, plain) = shift_map_rows(
            map,
            case["messages"].as_u64(),
            case["room"].as_u64().unwrap() as usize,
            case["source"].as_str(),
        );
        if !case["shut"].as_bool().unwrap() && case["budget"].is_null() {
            let wanted: Vec<&str> = case["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r[0].as_str().unwrap())
                .collect();
            assert_eq!(plain, wanted, "{case}");
        }
    }
}
#[test]
fn test_map_motion_equals_pane_py_for_every_hold_and_scenario() {
    let _turn = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let cases: Vec<Value> = serde_json::from_str(MOTION).unwrap();
    assert_eq!(cases.len(), 24);
    for case in &cases {
        match case["hold"].as_str() {
            Some(hold) => std::env::set_var("CSR_MAP_HOLD_S", hold),
            None => std::env::remove_var("CSR_MAP_HOLD_S"),
        }
        let mut motion = MapMotion::default();
        for (step, out) in case["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(case["out"].as_array().unwrap())
        {
            let (open, due) = motion.update(
                &step["map"],
                step["now"].as_f64().unwrap(),
                step["pinned"].as_bool().unwrap(),
                step["busy"].as_bool().unwrap(),
            );
            assert_eq!(json!(open), out["open"], "{case}");
            assert!(close(&json!(due), &out["due"]), "{case}: due {due:?}");
        }
    }
    std::env::remove_var("CSR_MAP_HOLD_S");
}
#[test]
fn test_the_hold_reads_its_word_the_way_pane_py_does() {
    let _turn = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for (word, want) in [
        ("", 12.),
        ("  ", 12.),
        ("3", 3.),
        ("0", 0.),
        ("-4", 0.),
        ("-inf", 0.),
        ("inf", 12.),
        ("nan", 12.),
        ("junk", 12.),
        (" 2.5 ", 2.5),
    ] {
        std::env::set_var("CSR_MAP_HOLD_S", word);
        assert_eq!(MapMotion::hold_seconds(), want, "{word:?}");
    }
    std::env::remove_var("CSR_MAP_HOLD_S");
    assert_eq!(MapMotion::hold_seconds(), 12.);
}
#[test]
fn test_story_cost_and_the_refresh_words_equal_pane_py() {
    let cases: Vec<Value> = serde_json::from_str(COSTS).unwrap();
    for case in &cases {
        let cost = match case["rows"].as_array() {
            None => None,
            Some(rows) => {
                let dir = tempfile::tempdir().unwrap();
                let paths = Paths::new(dir.path().into());
                std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
                let con = rusqlite::Connection::open(&paths.db).unwrap();
                con.execute_batch(
                    "create table narrative_usage(id integer primary key, ts text, model text, \
                     call_site text, input_tokens integer, output_tokens integer, \
                     cache_read_tokens integer, cache_creation_tokens integer)",
                )
                .unwrap();
                for row in rows {
                    con.execute(
                        "insert into narrative_usage(ts, model, call_site, input_tokens, \
                         output_tokens, cache_read_tokens, cache_creation_tokens) \
                         values(?1,?2,?3,?4,?5,?6,?7)",
                        rusqlite::params![
                            row[0].as_str(),
                            row[1].as_str(),
                            row[2].as_str(),
                            row[3].as_i64(),
                            row[4].as_i64(),
                            row[5].as_i64(),
                            row[6].as_i64()
                        ],
                    )
                    .unwrap();
                }
                drop(con);
                story_cost(&Db::open(&paths).unwrap())
            }
        };
        let cost = cost.unwrap_or(Value::Null);
        for key in ["calls", "usd", "tokens"] {
            if case["cost"].is_null() {
                assert!(cost.is_null(), "{case}");
            } else {
                assert!(
                    close(&cost[key], &case["cost"][key]),
                    "{key}: {cost} {case}"
                );
            }
        }
        assert_eq!(json!(refresh_label(&cost)), case["label"], "{case}");
    }
}
#[test]
fn test_refresh_needs_a_prompt_after_the_telling() {
    let cases: Vec<Value> = serde_json::from_str(NEWS).unwrap();
    assert!(cases.len() >= 8);
    for case in &cases {
        let map = json!({ "at": case["at"] });
        let s = json!({ "prompts": case["prompts"] });
        assert_eq!(json!(refresh_news(&map, &s)), case["out"], "{case}");
    }
    assert!(!refresh_news(&Value::Null, &json!({"prompts": [5.]})));
    assert!(!refresh_news(
        &json!({"at": 1.}),
        &json!({"prompts": ["9", null, true]})
    ));
}
#[test]
fn test_a_map_is_open_for_its_row_and_for_each_entry_it_expands() {
    assert!(map_open(&json!("shifts")));
    assert!(map_open(&json!("shiftlike:aaaa1111")));
    assert!(!map_open(&json!("story")));
    assert!(!map_open(&Value::Null));
}
