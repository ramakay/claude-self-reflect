//! Setup over a running daemon: launchd keeps the old job ("SIGTERMed") through the daemon's
//! ~20 s graceful stop. On the Studio (round 3, 2026-11-19) setup gave up after about 3 s, left
//! no daemon running, and still exited 0.

use super::super::{install_daemon_agent, DaemonAgentSpec};
use super::*;
use std::cell::Cell;
use std::path::Path;

fn spec(home: &Path) -> DaemonAgentSpec {
    DaemonAgentSpec {
        engine: "/opt/csr/bin/csr-engine".into(),
        home: home.to_path_buf(),
        db_path: home.join(".claude-self-reflect/csr-engine.db"),
        projects_dir: home.join(".claude/projects"),
        claude_dir: None,
        env: Default::default(),
        lock_holder: None,
    }
}

/// A loaded job that stays loaded for `linger` `print` calls after `bootout` (forever with
/// `None`). Returns the outcome, every call, and the time the pauses added up to.
fn run_over_loaded_job(linger: Option<usize>) -> (DaemonOutcome, Vec<String>, Duration, bool) {
    let home = tempfile::tempdir().unwrap();
    let spec = spec(home.path());
    let mut calls: Vec<String> = Vec::new();
    let mut booted = false;
    let mut loaded = true;
    let mut left = linger;
    let paused = Cell::new(Duration::ZERO);
    let run = install_daemon_agent(
        &spec,
        501,
        &mut |args: &[&str]| {
            calls.push(args.join(" "));
            match args[0] {
                "print" if loaded && booted => match left.as_mut() {
                    Some(0) => {
                        loaded = false;
                        Err("Could not find service".into())
                    }
                    Some(n) => {
                        *n -= 1;
                        Ok("\tstate = SIGTERMed\n\tpid = 4242\n".into())
                    }
                    None => Ok("\tstate = SIGTERMed\n\tpid = 4242\n".into()),
                },
                "print" if loaded => Ok("\tstate = running\n\tpid = 4242\n".into()),
                "print" => Err("Could not find service".into()),
                "bootout" => {
                    booted = true;
                    Ok(String::new())
                }
                "bootstrap" => {
                    loaded = true;
                    booted = false;
                    Ok(String::new())
                }
                _ => Ok(String::new()),
            }
        },
        &|gap| paused.set(paused.get() + gap),
    );
    let written = spec.plist_path().exists();
    (run.outcome, calls, paused.get(), written)
}

#[test]
fn setup_waits_through_the_old_daemons_graceful_stop_then_bootstraps() {
    // 60 looks at 300 ms: about 18 s, the length of the stop the Studio logged (20.1 s).
    let (outcome, calls, paused, written) = run_over_loaded_job(Some(60));
    assert_eq!(
        outcome,
        DaemonOutcome::Running { rewritten: true },
        "{calls:#?}"
    );
    assert!(written, "the new plist is written once the old job is gone");
    assert!(paused >= Duration::from_secs(18), "waited {paused:?}");
    let bootout = calls.iter().position(|c| c.starts_with("bootout")).unwrap();
    let bootstrap = calls
        .iter()
        .position(|c| c.starts_with("bootstrap"))
        .unwrap();
    let last_look = calls.iter().rposition(|c| c.starts_with("print")).unwrap();
    assert!(bootout < last_look && last_look < bootstrap, "{calls:#?}");
    assert!(!daemon_step_failed(&outcome));
}

#[test]
fn a_job_that_never_lets_go_fails_after_the_bounded_wait_and_writes_nothing() {
    let (outcome, calls, paused, written) = run_over_loaded_job(None);
    let budget = daemon_stop_wait();
    assert!(
        paused >= budget && paused <= budget + Duration::from_millis(500),
        "waited {paused:?} for a budget of {budget:?}"
    );
    let DaemonOutcome::Failed(why) = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        why.contains("still loaded") && why.contains("launchctl bootout gui/501/"),
        "{why}"
    );
    assert!(
        why.contains(&format!("{} s after bootout", paused.as_secs())),
        "{why}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("bootstrap")),
        "{calls:#?}"
    );
    assert!(!written, "the file never gets ahead of the job");
    assert!(daemon_step_failed(&outcome));
}

#[test]
fn the_wait_covers_launchds_own_kill_timeout() {
    if std::env::var_os(super::super::daemon_plist::DAEMON_STOP_WAIT_ENV).is_none() {
        assert!(daemon_stop_wait() > super::super::DAEMON_EXIT_TIMEOUT);
    }
}

#[test]
fn only_a_failed_start_fails_setup_and_its_report_names_the_retry() {
    for outcome in [
        DaemonOutcome::OptedOut,
        DaemonOutcome::Manual,
        DaemonOutcome::Running { rewritten: true },
        DaemonOutcome::KeptExisting,
        DaemonOutcome::AlreadyRunning { pid: 7 },
    ] {
        assert!(!daemon_step_failed(&outcome), "{outcome:?}");
    }
    let run = DaemonRun {
        outcome: DaemonOutcome::Failed("launchd still loaded x".into()),
        label: "com.ramakay.csr-engine.daemon".into(),
        previous_pid: None,
        notes: Vec::new(),
    };
    let text = daemon_report(&run, Path::new("/Users/me/.local/bin/csr-engine"));
    assert!(
        text.contains("The daemon did not start: launchd still loaded x"),
        "{text}"
    );
    assert!(
        text.contains("Retry with: /Users/me/.local/bin/csr-engine setup"),
        "{text}"
    );
    assert!(text.contains("exits non-zero"), "{text}");
}
