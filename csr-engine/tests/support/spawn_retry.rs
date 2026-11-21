//! Spawning a binary a test has just written or copied.
//!
//! On Linux a parallel test can fork while the new file's write handle is
//! still open in this process, so exec fails for a moment with ETXTBSY
//! ("Text file busy"). Seen on the arm release runner. Retry that one error
//! for up to a second; any other error fails the test at once.

use std::process::{Child, Command};

pub fn spawn_retrying(command: &mut Command) -> Child {
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Ok(child) => return child,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 50 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("spawn {:?}: {e}", command.get_program()),
        }
    }
}

/// Kills and reaps the test's own child, including when an assertion fails.
pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
