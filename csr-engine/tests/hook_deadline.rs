//! The process deadline of the hooks that have none of their own (`hooks/deadline.rs`).
//!
//! Runs the built binary against a temp HOME and data dir, never live data. The hook gets a
//! stdin pipe that stays open and carries nothing, so it would sit in its stdin read (2 s) and
//! then do its work; the deadline has to end it first.

#![cfg(unix)]

#[path = "support/spawn_retry.rs"]
mod spawn_retry;

use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

struct Run {
    status: ExitStatus,
    elapsed: Duration,
    stdout: Vec<u8>,
    timing: String,
}

/// Run the binary once before any timing. The first exec of a freshly built binary can take over
/// a second (macOS scans it), which is not the hook's time.
fn warm_up() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_csr-engine"));
        command
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn_retry::ChildGuard(spawn_retry::spawn_retrying(&mut command));
        let _ = child.0.wait();
    });
}

/// Run `hook <name>` with an open, silent stdin and `envs` added. Fails the test if the hook is
/// still running after `patience`.
fn run_hook(name: &str, envs: &[(&str, &str)], patience: Duration) -> Run {
    warm_up();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(dir.path().join("projects")).unwrap();
    let timing = dir.path().join("timing.log");
    let mut command = Command::new(env!("CARGO_BIN_EXE_csr-engine"));
    command
        .args(["hook", name, "--db-path"])
        .arg(dir.path().join("csr-engine.db"))
        .arg("--projects-dir")
        .arg(dir.path().join("projects"))
        .env("HOME", &home)
        .env("CSR_TIMING_LOG", &timing)
        // An agent session sets this, and the hook then exits at once: the test would pass for the wrong reason.
        .env_remove("CSR_DISABLE_RECURSIVE_HOOKS")
        .env_remove("CSR_HOOK_DEADLINE_SECS")
        .envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let started = Instant::now();
    let mut child = spawn_retry::ChildGuard(spawn_retry::spawn_retrying(&mut command));
    // Held open until the hook is gone: the hook never sees EOF.
    let _stdin = child.0.stdin.take().unwrap();
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < patience,
            "hook {name} still running after {patience:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let elapsed = started.elapsed();
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.0.stdout.take() {
        std::io::Read::read_to_end(&mut pipe, &mut stdout).unwrap();
    }
    Run {
        status,
        elapsed,
        stdout,
        timing: std::fs::read_to_string(&timing).unwrap_or_default(),
    }
}

#[test]
fn session_start_with_a_silent_open_stdin_is_ended_by_its_deadline() {
    let run = run_hook(
        "session-start",
        &[("CSR_HOOK_DEADLINE_SECS", "1")],
        Duration::from_secs(20),
    );
    assert!(run.status.success(), "{:?}", run.status);
    assert!(
        run.elapsed < Duration::from_millis(1800),
        "took {:?}",
        run.elapsed
    );
    assert!(run.elapsed >= Duration::from_secs(1), "{:?}", run.elapsed);
    assert!(
        run.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
    assert!(
        run.timing.contains("CSR hook session-start: total=") && run.timing.contains("deadline=1"),
        "no deadline line in {:?}",
        run.timing
    );
}

/// Control: the same run without the override is not cut at 1 s, so the test above does
/// not pass just because the hook is quick or exits on its own.
#[test]
fn the_same_hook_without_the_override_is_not_ended_by_a_deadline() {
    let run = run_hook("session-start", &[], Duration::from_secs(40));
    assert!(run.status.success(), "{:?}", run.status);
    assert!(
        run.elapsed >= Duration::from_millis(1800),
        "ended early: {:?}",
        run.elapsed
    );
    assert!(!run.timing.contains("deadline=1"), "{:?}", run.timing);
}
