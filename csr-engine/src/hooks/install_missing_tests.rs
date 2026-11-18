use super::*;
use serde_json::json;

/// What `hook install` writes, as a settings.json body.
fn installed() -> serde_json::Value {
    generate_hook_config("/usr/local/bin/csr-engine")
}

#[test]
fn a_full_install_misses_nothing() {
    assert!(missing_csr_hook_events_in(&installed()).is_empty());
}

#[test]
fn settings_without_post_compact_name_that_event() {
    let mut settings = installed();
    settings["hooks"]
        .as_object_mut()
        .unwrap()
        .remove("PostCompact");
    assert_eq!(missing_csr_hook_events_in(&settings), vec!["PostCompact"]);
}

#[test]
fn a_missing_second_command_on_an_event_names_the_event() {
    // SessionStart carries session-start and session-briefing.
    let mut settings = installed();
    settings["hooks"]["SessionStart"]
        .as_array_mut()
        .unwrap()
        .remove(1);
    assert_eq!(missing_csr_hook_events_in(&settings), vec!["SessionStart"]);
}

#[test]
fn a_users_own_hook_with_a_csr_looking_name_does_not_count() {
    let mut settings = installed();
    settings["hooks"]["Stop"] = json!([{"hooks": [{
        "type": "command",
        "command": "/Users/me/.claude/hooks/csr-intent-narrator.sh"
    }]}]);
    assert_eq!(missing_csr_hook_events_in(&settings), vec!["Stop"]);
}

#[test]
fn a_file_with_no_hooks_or_no_file_misses_every_event() {
    let every = vec![
        "PostCompact",
        "PostToolUse",
        "PreCompact",
        "SessionEnd",
        "SessionStart",
        "Stop",
        "UserPromptSubmit",
    ];
    assert_eq!(missing_csr_hook_events_in(&json!({})), every);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        missing_csr_hook_events(&dir.path().join("settings.json")),
        every
    );
}

#[test]
fn the_file_reader_reads_only_and_an_unparseable_file_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let mut settings = installed();
    settings["hooks"]
        .as_object_mut()
        .unwrap()
        .remove("PostCompact");
    let body = serde_json::to_string_pretty(&settings).unwrap();
    std::fs::write(&path, &body).unwrap();
    assert_eq!(missing_csr_hook_events(&path), vec!["PostCompact"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    std::fs::write(&path, "{ not json").unwrap();
    assert!(missing_csr_hook_events(&path).is_empty());
}

#[test]
fn status_json_gets_hooks_missing_and_healthy_keeps_its_value() {
    let mut report = json!({"healthy": true, "chunks": 3});
    add_hooks_missing(&mut report, &["PostCompact".to_string()]);
    assert_eq!(report["hooks_missing"], json!(["PostCompact"]));
    assert_eq!(report["healthy"], json!(true));
    assert_eq!(report["chunks"], json!(3));
    add_hooks_missing(&mut report, &[]);
    assert_eq!(report["hooks_missing"], json!([]));
}

#[test]
fn the_telemetry_line_appears_only_when_something_is_missing() {
    assert_eq!(hooks_missing_line(&[]), None);
    let line = hooks_missing_line(&["PostCompact".into(), "Stop".into()]).unwrap();
    assert!(line.contains("MISSING PostCompact, Stop"), "{line}");
    assert!(line.contains("hook install --apply"), "{line}");
}
