//! A hook's timing line is printed to stderr, which Claude Code keeps in the transcript's
//! hook record, and written to the timing log. The two must be the same bytes, timestamp
//! included, so a session start in the log can be joined to its transcript by equality.
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn the_session_start_timing_line_on_stderr_is_the_logged_line_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(dir.path().join("projects")).unwrap();
    let timing = dir.path().join("timing.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .args(["hook", "session-start", "--db-path"])
        .arg(dir.path().join("csr-engine.db"))
        .arg("--projects-dir")
        .arg(dir.path().join("projects"))
        .env("HOME", &home)
        .env("CSR_TIMING_LOG", &timing)
        .env_remove("CSR_DISABLE_RECURSIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"join-1","cwd":"/w/join-probe/repo","source":"startup"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let marker = "CSR hook session-start [";
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let printed: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| line.contains(marker))
        .collect();
    let log = std::fs::read_to_string(&timing).unwrap();
    let logged: Vec<&str> = log
        .lines()
        .map(str::trim_end)
        .filter(|line| line.contains(marker))
        .collect();
    assert_eq!(logged.len(), 1, "{log}");
    assert_eq!(printed, logged, "stderr:\n{stderr}\nlog:\n{log}");
}
