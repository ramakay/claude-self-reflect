//! Replay harness: compose the SessionStart recap over real transcripts and
//! write one JSON line per session with the composed text and the episode
//! evidence behind each clause, so a run can say which clause dropped and why.
//!
//! Ignored by default. Reads only (`CSR_REPLAY_DIR` of `*.jsonl`), writes only
//! `CSR_REPLAY_OUT`:
//!
//! `CSR_REPLAY_DIR=... CSR_REPLAY_OUT=... cargo test --release --lib recap_replay -- --ignored`

use std::io::Write;

use crate::hooks::recap::{compose_recap, RecapFeeds};
use crate::hooks::stop::extract_episode_from_file;

fn empty_feeds() -> RecapFeeds {
    RecapFeeds {
        settled: vec![],
        still_open: vec![],
        retired_while_away: vec![],
        corrections: vec![],
        open_proposals: 0,
        top_dream: None,
    }
}

#[test]
#[ignore = "reads real transcripts: CSR_REPLAY_DIR, CSR_REPLAY_OUT"]
fn recap_replay_over_transcripts() {
    let dir = std::env::var("CSR_REPLAY_DIR").expect("CSR_REPLAY_DIR");
    let out = std::env::var("CSR_REPLAY_OUT").expect("CSR_REPLAY_OUT");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    let mut sink = std::fs::File::create(out).unwrap();
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let session = name
            .split("__")
            .nth(1)
            .unwrap_or(&name)
            .trim_end_matches(".jsonl");
        let limit = std::fs::metadata(&path).unwrap().len();
        let ep = extract_episode_from_file(&path, limit, session, "replay").unwrap();
        let text = compose_recap(&ep, &empty_feeds(), "1h ago");
        let line = serde_json::json!({
            "file": name,
            "recap": text,
            "episode": serde_json::to_value(&ep).unwrap(),
        });
        writeln!(sink, "{line}").unwrap();
    }
}
