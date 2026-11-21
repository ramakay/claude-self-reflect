use super::*;

#[test]
fn daemon_status_reads_the_lock_not_a_pid_or_a_plist() {
    use fs2::FileExt;
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".claude-self-reflect");
    std::fs::create_dir_all(&data).unwrap();

    let down = gather_daemon(Some(home.path()), true);
    assert!(
        !down.up && down.pid.is_none() && !down.agent_installed,
        "{down:?}"
    );

    // A pid in the file with nobody holding the lock is a daemon that died.
    std::fs::write(data.join("daemon.lock"), std::process::id().to_string()).unwrap();
    assert!(!gather_daemon(Some(home.path()), true).up);

    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(data.join("daemon.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&held).unwrap();
    let agents = home.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("com.ramakay.csr-engine.daemon.plist"), "x").unwrap();
    let up = gather_daemon(Some(home.path()), true);
    assert_eq!(
        up,
        DaemonStatus {
            up: true,
            pid: Some(std::process::id()),
            agent_installed: true,
            ..DaemonStatus::default()
        }
    );
    assert!(
        !gather_daemon(None, true).up,
        "no home proves nothing; it reads as down"
    );
}

#[test]
fn a_daemon_agent_under_another_label_counts_as_installed() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    assert!(!gather_daemon(Some(home.path()), true).agent_installed);
    // unrelated agents and saved copies are not the daemon's agent
    std::fs::write(agents.join("com.other.plist"), "x").unwrap();
    std::fs::write(
        agents.join("mine.plist.bak"),
        "<plist version=\"1.0\"><dict><key>ProgramArguments</key><array><string>/bin/csr-engine</string><string>daemon</string></array></dict></plist>",
    )
    .unwrap();
    assert!(!gather_daemon(Some(home.path()), true).agent_installed);
    std::fs::write(
        agents.join("mine.plist"),
        "<plist version=\"1.0\"><dict><key>Label</key><string>me.csr</string><key>ProgramArguments</key><array><string>/bin/csr-engine</string><string>daemon</string></array></dict></plist>",
    )
    .unwrap();
    assert!(gather_daemon(Some(home.path()), true).agent_installed);
}

/// A home whose daemon lock is held by this test process, with a daemon run
/// record for this pid and three recorded starts.
fn running_daemon_home() -> (tempfile::TempDir, std::fs::File) {
    use fs2::FileExt;
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".claude-self-reflect");
    std::fs::create_dir_all(data.join("run")).unwrap();
    let pid = std::process::id();
    std::fs::write(data.join("daemon.lock"), pid.to_string()).unwrap();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(data.join("daemon.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&held).unwrap();
    std::fs::write(
        data.join("run").join(format!("{pid}.json")),
        serde_json::json!({
            "pid": pid,
            "version": "10.1.0",
            "git_sha": "abc",
            "exe_identity": {"dev": 1, "ino": 2, "len": 3, "mtime_ns": 4, "ctime_ns": 5},
            "role": "daemon",
            "started_at": "2026-11-17T03:31:59+00:00",
            "parent_cmd": "launchd",
        })
        .to_string(),
    )
    .unwrap();
    let history = crate::daemon_history::path_in(home.path());
    for (p, at) in [
        (1, "2026-11-17T03:30:27Z"),
        (2, "2026-11-17T03:31:28Z"),
        (pid, "2026-11-17T03:31:58Z"),
    ] {
        crate::daemon_history::record_start(
            &history,
            p,
            "10.1.0",
            DateTime::parse_from_rfc3339(at)
                .unwrap()
                .with_timezone(&Utc),
        );
    }
    (home, held)
}

#[test]
fn a_running_daemon_reports_its_start_time_uptime_and_restarts() {
    let (home, _held) = running_daemon_home();
    let now = DateTime::parse_from_rfc3339("2026-11-17T03:33:29Z")
        .unwrap()
        .with_timezone(&Utc);
    let status = gather_daemon_at(Some(home.path()), true, now);
    assert!(status.up);
    assert_eq!(
        status.started_at.as_deref(),
        Some("2026-11-17T03:31:59+00:00")
    );
    assert_eq!(status.uptime_secs, Some(90));
    let history = status.history.expect("the history file exists");
    assert_eq!(history.starts_last_10m, 3);
    assert_eq!(history.starts_last_1h, 3);
    assert_eq!(history.starts_last_24h, 3);
    assert_eq!(
        history.previous_run_end.as_deref(),
        Some("unknown (pid 2 ended without an exit line)")
    );
    let json = serde_json::to_value(gather_daemon_at(Some(home.path()), true, now)).unwrap();
    assert_eq!(json["history"]["starts_last_1h"], 3);
    assert_eq!(json["uptime_secs"], 90);
}

#[test]
fn the_compact_gather_reads_no_run_record_and_no_history() {
    let (home, _held) = running_daemon_home();
    let status = gather_daemon(Some(home.path()), false);
    assert!(status.up && status.pid.is_some());
    assert_eq!(status.started_at, None);
    assert_eq!(status.history, None);
}

#[test]
fn a_run_record_for_another_role_is_not_the_daemon_start() {
    let (home, _held) = running_daemon_home();
    let pid = std::process::id();
    let path = home
        .path()
        .join(".claude-self-reflect/run")
        .join(format!("{pid}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"daemon\"", "\"mcp_stdio\"");
    std::fs::write(&path, text).unwrap();
    assert_eq!(gather_daemon(Some(home.path()), true).started_at, None);
}
