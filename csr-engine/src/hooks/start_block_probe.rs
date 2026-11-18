//! Before/after of the SessionStart block over one real transcript. Ignored; reads
//! `CSR_SB_DIR` (a directory holding the one session JSONL), writes nothing but stdout.
//!
//! Needs `CSR_SB_DATA` (head/briefing/extra/learnt/rest .txt of the real block).
//!
//! `before`: the stored episode as the old Stop worker saw it (`CSR_SB_STALE_LINES`
//! rows), the stored first-task lead (`source=clear` keeps it), the cached briefing,
//! no `Open` block. `after`: the episode as the fixed worker sees it
//! (`CSR_SB_LINES` rows), the newest-ask lead, the briefing gate, the `Open` block.
//! The remaining lines of the block (header, learnt line, episode index, footer) are the
//! real block's own and are read from `CSR_SB_DATA` so both sides share them.
//!
//! `CSR_SB_DIR=... CSR_SB_SESSION=... cargo test --release --lib start_block_probe -- --ignored --nocapture`

use super::*;

fn read_lines(path: &Path, n: usize) -> std::path::PathBuf {
    let text = std::fs::read_to_string(path).unwrap();
    let cut: String = text.lines().take(n).map(|l| format!("{l}\n")).collect();
    let dir = std::env::temp_dir().join(format!("sb-probe-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(path.file_name().unwrap());
    std::fs::write(&out, cut).unwrap();
    out
}

#[test]
#[ignore = "reads a real transcript: CSR_SB_DIR, CSR_SB_SESSION"]
fn start_block_probe() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    std::env::remove_var("CSR_NO_RECAP");
    let dir = std::path::PathBuf::from(std::env::var("CSR_SB_DIR").unwrap());
    let sid = std::env::var("CSR_SB_SESSION").unwrap();
    let full = dir.join(format!("{sid}.jsonl"));
    let lines: usize = std::env::var("CSR_SB_LINES").unwrap().parse().unwrap();
    let stale: usize = std::env::var("CSR_SB_STALE_LINES")
        .unwrap()
        .parse()
        .unwrap();
    let briefing = std::fs::read_to_string(
        std::path::Path::new(&std::env::var("CSR_SB_DATA").unwrap()).join("briefing.txt"),
    )
    .unwrap();
    let head = std::fs::read_to_string(
        std::path::Path::new(&std::env::var("CSR_SB_DATA").unwrap()).join("head.txt"),
    )
    .unwrap();
    let read = |name: &str| {
        std::fs::read_to_string(
            std::path::Path::new(&std::env::var("CSR_SB_DATA").unwrap()).join(name),
        )
        .unwrap()
    };
    let (learnt, rest) = (read("learnt.txt"), read("rest.txt"));
    let written = "2026-11-05T05:00:00Z";
    let now = temporal::parse_timestamp("2026-11-06T14:57:19Z").unwrap();
    let storage = crate::storage::Storage::open_memory().unwrap();
    let trust = crate::provenance::TrustTier::Unknown;

    let episode = |n: usize, stamp: &str| {
        let part = read_lines(&full, n);
        let len = std::fs::metadata(&part).unwrap().len();
        let mut ep =
            crate::hooks::stop::extract_episode_from_file(&part, len, &sid, "csr-engine").unwrap();
        ep.timestamp = stamp.to_string();
        ep
    };
    let verdicts = vec![
        ("a".to_string(), AnchorVerdict::Intact),
        ("b".to_string(), AnchorVerdict::Intact),
    ];
    let age = relative_time_label("2026-11-06T01:46:07Z", &now);
    let feeds = crate::hooks::recap::RecapFeeds {
        settled: vec![],
        still_open: vec![],
        retired_while_away: vec![],
        corrections: vec![crate::hooks::recap::CorrectionLine {
            quote: "I said yes monitor it".into(),
            session8: "2bb8ba03".into(),
            turn: 127,
            date: "2026-11-02".into(),
        }],
        open_proposals: 0,
        native_warn: None,
        top_dream: None,
    };
    let tier0 = |ep: &Episode, source: &str| {
        let recap_ep = recap_episode(ep, Some(source), Some("new-session"));
        let ctx = crate::hooks::recap::RecapContext::default();
        let composed = crate::hooks::recap::compose_recap_in_context(&recap_ep, &feeds, &age, &ctx);
        format_tier0_block_with_recap(
            ep,
            &verdicts,
            &age,
            composed.as_ref().map(|c| c.text.as_str()),
            trust,
        )
    };

    // before
    let ep_before = episode(stale, "2026-11-06T01:46:07Z");
    let before = format!(
        "{head}{}{}{learnt}{rest}",
        format_briefing_written(trust, &briefing, &relative_time_label(written, &now)),
        tier0(&ep_before, "clear")
    );

    // after
    let ep_after = episode(lines, "2026-11-06T01:46:07Z");
    let input = HookInput {
        session_id: Some("new-session".into()),
        transcript_path: Some(dir.join("new-session.jsonl").to_string_lossy().into_owned()),
        source: Some("resume".into()),
        ..Default::default()
    };
    let open = open_block(&input, &storage, &ep_after).unwrap_or_default();
    let kept = if briefing_still_true(&briefing, written, Some(&ep_after), &now) {
        format_briefing_written(trust, &briefing, &relative_time_label(written, &now))
    } else {
        String::new()
    };
    let after = format!(
        "{head}{kept}{}{learnt}{open}{rest}",
        tier0(&ep_after, "resume")
    );

    println!("BEFORE>>>{}\n{before}\n<<<BEFORE", before.chars().count());
    println!("AFTER>>>{}\n{after}\n<<<AFTER", after.chars().count());
}
