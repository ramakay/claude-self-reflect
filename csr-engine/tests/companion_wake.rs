//! The relay's wake of a pane trusts no process id.
use csr_engine::companion::wake::{notify, Kind};
use std::{fs, process::Command};

/// A pane that crashed leaves a fresh-looking heartbeat behind, and its pid can be anyone's by
/// the time a hook fires. Whatever the file claims, no process is signalled, killed or hurt.
#[test]
fn a_crashed_panes_heartbeat_never_points_the_relay_at_an_unrelated_process() {
    let keep = tempfile::tempdir().unwrap();
    // The process that now holds the dead pane's pid: it dies of SIGUSR1/SIGUSR2/SIGTERM.
    let mut stranger = Command::new("sleep").arg("30").spawn().unwrap();
    fs::create_dir_all(keep.path().join("panes")).unwrap();
    fs::write(
        keep.path().join("panes/sess.json"),
        format!("{{\"pid\":{},\"wake\":1}}", stranger.id()),
    )
    .unwrap();
    for kind in [Kind::Now, Kind::Burst] {
        for _ in 0..3 {
            notify(keep.path(), "sess", kind);
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        stranger.try_wait().unwrap().is_none(),
        "the relay signalled a process only because a pid file named it"
    );
    let _ = stranger.kill();
    let _ = stranger.wait();
}
