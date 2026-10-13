//! Child output collection with one deadline for both exit and pipe draining.

use std::io::{self, Read};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) trait CommandDeadline {
    fn output_with_deadline(&mut self, limit: Duration) -> io::Result<Output>;
}

struct ReapOnDrop {
    child: Child,
    complete: bool,
}

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        #[cfg(unix)]
        // SAFETY: this child was spawned in a new process group.
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl CommandDeadline for Command {
    fn output_with_deadline(&mut self, limit: Duration) -> io::Result<Output> {
        let deadline = Instant::now() + limit;
        if limit.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            self.process_group(0);
        }
        let mut child = ReapOnDrop {
            child: self
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
            complete: false,
        };
        fn reader(
            pipe: impl Read + Send + 'static,
        ) -> std::sync::mpsc::Receiver<io::Result<Vec<u8>>> {
            let (send, recv) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let mut pipe = pipe;
                let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
                let _ = send.send(result);
            });
            recv
        }
        let stdout = reader(child.child.stdout.take().expect("piped stdout"));
        let stderr = reader(child.child.stderr.take().expect("piped stderr"));
        let mut status = None;
        let mut out = None;
        let mut err = None;
        loop {
            if status.is_none() {
                status = child.child.try_wait()?;
            }
            if out.is_none() {
                out = stdout.try_recv().ok();
            }
            if err.is_none() {
                err = stderr.try_recv().ok();
            }
            if status.is_some() && out.is_some() && err.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        // Already reaped, and both pipes drained. Do not signal the group again.
        child.complete = true;
        Ok(Output {
            status: status.expect("child exited"),
            stdout: out.expect("stdout drained")?,
            stderr: err.expect("stderr drained")?,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn r5_child_deadline_kills_reaps_and_releases_guard() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let guard = crate::maintenance::shared(dir.path()).unwrap();
        let started = Instant::now();
        let error = Command::new("sh")
            .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "sh"])
            .arg(&pid_file)
            .output_with_deadline(Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: waitpid only checks that our child has already been reaped.
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        drop(guard);
        assert!(crate::maintenance::MaintenanceLock::acquire(dir.path()).is_ok());
    }

    #[test]
    fn r5_child_deadline_includes_inherited_pipes_and_large_output() {
        let started = Instant::now();
        let error = Command::new("sh")
            .args(["-c", "sleep 30 & exit 0"])
            .output_with_deadline(Duration::from_millis(100))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        let out = Command::new("sh")
            .args(["-c", "dd if=/dev/zero bs=65536 count=4; printf err >&2"])
            .output_with_deadline(GIT_TIMEOUT)
            .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 262144);
        assert!(out.stderr.ends_with(b"err"));
    }
}
