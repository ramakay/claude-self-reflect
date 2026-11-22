//! The program writer in `tests/support/exec_script.rs`, under the load that broke the plain write.

use crate::exec_script::write_executable;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Threads that fork all the while. Dropping this sets `stop` and joins every thread started so
/// far, so a panic in the test body cannot leave them starting shells under the rest of the suite.
struct Forkers {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Forkers {
    fn new(stop: Arc<AtomicBool>) -> Self {
        Self {
            stop,
            threads: Vec::new(),
        }
    }

    fn start_one(&mut self) {
        let stop = self.stop.clone();
        self.threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = Command::new("/bin/sh").args(["-c", ":"]).status();
            }
        }));
    }
}

impl Drop for Forkers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Other threads fork all the while; every program written here runs at once, first time.
#[test]
fn a_program_written_here_runs_while_other_threads_fork() {
    let dir = tempfile::tempdir().unwrap();
    let mut forkers = Forkers::new(Arc::new(AtomicBool::new(false)));
    for _ in 0..4 {
        forkers.start_one();
    }
    let mut failed = 0;
    for i in 0..100 {
        let path = dir.path().join(format!("prog{i}"));
        write_executable(&path, "#!/bin/sh\nexit 0\n");
        if !Command::new(&path).status().is_ok_and(|s| s.success()) {
            failed += 1;
        }
    }
    drop(forkers);
    assert_eq!(failed, 0);
}

/// A panic in the body must not leave the threads running: the guard stops and joins them.
#[test]
fn a_panic_in_the_body_still_stops_and_joins_the_forkers() {
    let stop = Arc::new(AtomicBool::new(false));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut forkers = Forkers::new(stop.clone());
        forkers.start_one();
        forkers.start_one();
        panic!("the body failed");
    }));
    assert!(outcome.is_err());
    assert!(stop.load(Ordering::Relaxed));
    // Each thread held a clone of the flag; joined threads have dropped theirs.
    assert_eq!(Arc::strong_count(&stop), 1);
}
