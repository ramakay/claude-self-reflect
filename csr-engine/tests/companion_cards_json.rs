//! `companion pane --json` and `--json --cards`: plain `--json` is the summary and `schema` and
//! composes no pane (every caller of `--json` pays for it); the cards are an explicit ask.
#[path = "support/pane_home.rs"]
mod pane_home;
use csr_engine::companion::{cli::PaneArgs, contract, paths::Paths};
use pane_home::*;
use serde_json::Value;
use std::process::Command;

const SID: &str = "sess-now";

fn run(paths: &Paths, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", &paths.home)
        .env("CSR_COMPANION_NOW", NOW.to_string())
        .args(["companion", "pane", "--repo", repo().to_str().unwrap()])
        .args(["--sid", SID])
        .args(extra)
        .output()
        .unwrap()
}

fn answer(paths: &Paths, extra: &[&str]) -> Value {
    let out = run(paths, extra);
    assert!(
        out.status.success(),
        "{extra:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn json_args(extra: &[&str]) -> PaneArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct TestArgs {
        #[command(flatten)]
        pane: PaneArgs,
    }
    let repo = repo();
    let mut argv = vec!["test", "--repo", repo.to_str().unwrap(), "--sid", SID];
    argv.extend(extra);
    TestArgs::parse_from(argv).pane
}

/// Fractions to six places: the token split is a ratio of character counts,
/// and those counts hold the temp dir's path, so the last digits move with
/// the checkout's path length.
fn rounded(value: &Value) -> Value {
    match value {
        Value::Number(n) if n.is_f64() => n
            .as_f64()
            .and_then(|f| serde_json::Number::from_f64((f * 1e6).round() / 1e6))
            .map_or_else(|| value.clone(), Value::Number),
        Value::Array(items) => Value::Array(items.iter().map(rounded).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), rounded(v))).collect())
        }
        _ => value.clone(),
    }
}

/// Home paths in the answer are the temp dir's: the golden says `<HOME>` and `<REPO>`.
fn normalized(value: &Value, paths: &Paths) -> String {
    serde_json::to_string_pretty(&rounded(value))
        .unwrap()
        .replace(paths.home.to_str().unwrap(), "<HOME>")
        .replace(repo().to_str().unwrap(), "<REPO>")
}

#[test]
fn plain_json_carries_no_cards() {
    let (_home, paths) = rich_home();
    let plain = answer(&paths, &["--json"]);
    assert_eq!(plain["schema"], 1);
    for key in contract::CARD_KEYS {
        assert!(plain.get(key).is_none(), "plain --json carries {key}");
    }
}

#[test]
fn the_cards_ask_adds_the_contract_to_the_same_summary() {
    let (_home, paths) = rich_home();
    let plain = answer(&paths, &["--json"]);
    let mut cards = answer(&paths, &["--json", "--cards"]);
    let ids: Vec<&str> = cards["cards"]
        .as_array()
        .expect("cards")
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert_eq!(ids.first(), Some(&"dreamer"), "{ids:?}");
    assert_eq!(cards["herdr_pane_live"], false);
    let map = cards.as_object_mut().unwrap();
    for key in contract::CARD_KEYS {
        map.remove(key);
    }
    assert_eq!(cards, plain);
}

/// The status text reads the session's usage file: the app writes `dreamsStopped` there while
/// `CSR_NO_DREAMING` is set where Claude Code runs.
#[test]
fn the_status_text_does_not_read_as_on_while_the_usage_file_says_stopped() {
    let (_home, paths) = rich_home();
    let status = |paths: &Paths| {
        let got = contract::pane_json(paths, &json_args(&["--json"]), SID, NOW);
        got["status"].as_str().unwrap().to_owned()
    };
    assert!(status(&paths).ends_with("4 dreams this week"));
    let file = paths.keep.join("usage").join(format!("{SID}.json"));
    let mut usage: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    usage["dreamsStopped"] = Value::Bool(true);
    std::fs::write(&file, usage.to_string()).unwrap();
    let stopped = status(&paths);
    assert!(stopped.ends_with("dreams stopped by CSR_NO_DREAMING"));
    assert!(!stopped.contains("dreams this week"));
}

#[test]
fn cards_without_json_is_refused() {
    let (_home, paths) = rich_home();
    let out = run(&paths, &["--cards"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--json"));
}

/// The cards answer for the rich session, kept as a golden: `CSR_BLESS=1` writes it again.
#[test]
fn the_cards_answer_matches_its_golden() {
    let (_home, paths) = rich_home();
    let got = contract::pane_json(&paths, &json_args(&["--json", "--cards"]), SID, NOW);
    let got = normalized(&got, &paths) + "\n";
    let path = fixture().join("json-cards.json");
    if std::env::var_os("CSR_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        got,
        want,
        "golden {} is stale: run with CSR_BLESS=1",
        path.display()
    );
}
