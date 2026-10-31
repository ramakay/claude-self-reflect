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
        native_warn: None,
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

/// Before/after over real sessions. `CSR_REPLAY_PAIRS` holds `<session>.json` (the
/// stored episode, as the installed build wrote it) and, optionally,
/// `<session>.jsonl` (its transcript). For each session it writes the recap the
/// old composer gave the stored episode, what SessionStart now prints for that
/// same episode on a compaction, and what it prints once the episode is read
/// again from the transcript by the current extractor.
///
/// `CSR_REPLAY_PAIRS=... CSR_REPLAY_OUT=... cargo test --release --lib recap_before_after -- --ignored`
#[test]
#[ignore = "reads real episodes and transcripts: CSR_REPLAY_PAIRS, CSR_REPLAY_OUT"]
fn recap_before_after_over_stored_episodes() {
    use crate::hooks::stop::Episode;
    let dir = std::env::var("CSR_REPLAY_PAIRS").expect("CSR_REPLAY_PAIRS");
    let out = std::env::var("CSR_REPLAY_OUT").expect("CSR_REPLAY_OUT");
    let mut stored: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    stored.sort();
    let storage = crate::storage::Storage::open_memory().unwrap();
    let first_lines = |block: &str| -> String {
        block
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(4)
            .collect::<Vec<_>>()
            .join("\n    ")
    };
    let mut sink = std::fs::File::create(out).unwrap();
    for path in stored {
        let ep: Episode = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let sid = ep.session_id.clone();
        let before = compose_recap(&ep, &empty_feeds(), "1h ago");
        let after_stored = crate::hooks::session_start::format_session_start_tier0_for_session(
            &storage,
            &ep.project,
            &ep,
            &[],
            "1h ago",
            Some(&sid),
            Some("compact"),
        );
        let mut report = format!(
            "== {} ({}, {} messages)\n  before (stored episode, old composer):\n    {}\n  after, same stored episode:\n    {}\n",
            &sid[..8.min(sid.len())],
            ep.project,
            ep.message_count,
            before.as_deref().map_or("(none)".to_string(), |t| t.replace('\n', "\n    ")),
            first_lines(&after_stored),
        );
        let transcript = path.with_extension("jsonl");
        if transcript.exists() {
            let limit = std::fs::metadata(&transcript).unwrap().len();
            let fresh = extract_episode_from_file(&transcript, limit, &sid, &ep.project).unwrap();
            let after_fresh = crate::hooks::session_start::format_session_start_tier0_for_session(
                &storage,
                &ep.project,
                &fresh,
                &[],
                "1h ago",
                Some(&sid),
                Some("compact"),
            );
            report.push_str(&format!(
                "  after, episode re-read from the transcript (commits seen: {}):\n    {}\n",
                fresh.completions.len(),
                first_lines(&after_fresh)
            ));
        }
        writeln!(sink, "{report}").unwrap();
    }
}
