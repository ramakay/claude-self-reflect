//! The program writer in `tests/support/exec_script.rs`, under the load that broke the plain write.

use crate::exec_script::write_executable;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Other threads fork all the while; every program written here runs at once, first time.
#[test]
fn a_program_written_here_runs_while_other_threads_fork() {
    let dir = tempfile::tempdir().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let forkers: Vec<_> = (0..4)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = Command::new("/bin/sh").args(["-c", ":"]).status();
                }
            })
        })
        .collect();
    let mut failed = 0;
    for i in 0..100 {
        let path = dir.path().join(format!("prog{i}"));
        write_executable(&path, "#!/bin/sh\nexit 0\n");
        if !Command::new(&path).status().is_ok_and(|s| s.success()) {
            failed += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    for forker in forkers {
        forker.join().unwrap();
    }
    assert_eq!(failed, 0);
}
