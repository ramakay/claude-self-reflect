use super::*;

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

#[test]
fn a_first_start_has_no_previous_run_and_says_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let previous = record_start(&path, 10, "10.1.0", t("2026-11-17T03:30:27Z"));
    assert!(previous.starts_with("unknown"), "{previous}");
    let events = read(&path).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, EventKind::Start);
    assert_eq!(events[0].pid, 10);
    assert_eq!(events[0].version.as_deref(), Some("10.1.0"));
}

#[test]
fn a_run_killed_without_an_exit_line_reads_unknown_at_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    record_start(&path, 10, "10.1.0", t("2026-11-17T03:30:27Z"));
    let previous = record_start(&path, 11, "10.1.0", t("2026-11-17T03:31:28Z"));
    assert_eq!(previous, "unknown (pid 10 ended without an exit line)");
}

#[test]
fn a_run_killed_during_its_graceful_path_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    record_start(&path, 10, "10.1.0", t("2026-11-17T03:30:27Z"));
    record_stop(&path, 10, "SIGTERM", t("2026-11-17T03:31:20Z"));
    let previous = record_start(&path, 11, "10.1.0", t("2026-11-17T03:31:28Z"));
    assert_eq!(
        previous,
        "SIGTERM, then ended before its graceful shutdown finished"
    );
}

#[test]
fn a_clean_exit_is_named_with_its_duration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    record_start(&path, 10, "10.1.0", t("2026-11-17T03:30:27Z"));
    record_stop(&path, 10, "binary replaced", t("2026-11-17T03:31:20Z"));
    record_exit(
        &path,
        10,
        "binary replaced",
        2400,
        t("2026-11-17T03:31:22Z"),
    );
    let previous = record_start(&path, 11, "10.1.0", t("2026-11-17T03:31:28Z"));
    assert_eq!(
        previous,
        "binary replaced, graceful shutdown finished in 2400 ms"
    );
}

#[test]
fn starts_are_counted_per_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    // three starts in 91 seconds, one 2 h earlier, one 2 days earlier
    for (pid, at) in [
        (1, "2026-11-15T03:00:00Z"),
        (2, "2026-11-17T01:30:00Z"),
        (3, "2026-11-17T03:30:27Z"),
        (4, "2026-11-17T03:31:28Z"),
        (5, "2026-11-17T03:31:58Z"),
    ] {
        record_start(&path, pid, "10.1.0", t(at));
    }
    let summary = summary_in_path(&path, t("2026-11-17T03:35:00Z"));
    assert_eq!(summary.starts_last_10m, 3);
    assert_eq!(summary.starts_last_1h, 3);
    assert_eq!(summary.starts_last_24h, 4);
    assert_eq!(
        summary.last_start_at.as_deref(),
        Some("2026-11-17T03:31:58Z")
    );
    assert_eq!(
        summary.previous_run_end.as_deref(),
        Some("unknown (pid 4 ended without an exit line)")
    );
}

fn summary_in_path(path: &Path, now: DateTime<Utc>) -> StartHistory {
    summarize(&read(path).unwrap(), now)
}

#[test]
fn no_file_is_no_history_not_zero_starts() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(summary_in(home.path(), Utc::now()), None);
    std::fs::create_dir_all(home.path().join(".claude-self-reflect")).unwrap();
    record_start(
        &path_in(home.path()),
        7,
        "10.1.0",
        t("2026-11-17T03:30:27Z"),
    );
    let summary = summary_in(home.path(), t("2026-11-17T03:31:00Z")).unwrap();
    assert_eq!(summary.starts_last_10m, 1);
}

#[test]
fn the_file_stays_bounded_and_keeps_the_newest_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    let base = t("2026-11-17T00:00:00Z");
    for i in 0..(MAX_LINES as u32 + 5) {
        record_start(&path, i, "10.1.0", base + Duration::seconds(i64::from(i)));
    }
    let events = read(&path).unwrap();
    assert!(events.len() <= MAX_LINES, "{}", events.len());
    assert!(events.len() >= KEEP_LINES, "{}", events.len());
    assert_eq!(events.last().unwrap().pid, MAX_LINES as u32 + 4);
}

#[test]
fn a_garbled_line_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE_NAME);
    std::fs::write(&path, "not json\n").unwrap();
    let previous = record_start(&path, 3, "10.1.0", t("2026-11-17T03:30:27Z"));
    assert!(previous.starts_with("unknown"), "{previous}");
    assert_eq!(read(&path).unwrap().len(), 1);
}
