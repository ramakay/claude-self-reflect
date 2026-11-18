//! Child output collection with one deadline for both exit and pipe draining.

use std::collections::HashMap;
use std::io::{self, Read};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
const PID_MAX: u32 = 99_999;
#[cfg(target_os = "linux")]
const PID_MAX: u32 = 4_194_304;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const PID_MAX: u32 = i32::MAX as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineProbe {
    Engine,
    NotEngine,
    Unknown,
}

/// Check executable identities in one ps call; errors must not erase records.
pub(crate) fn probe_engines(pids: &[u32]) -> HashMap<u32, EngineProbe> {
    let mut probes: HashMap<_, _> = pids
        .iter()
        .map(|&pid| {
            let probe = if pid == 0 || pid > PID_MAX {
                EngineProbe::NotEngine
            } else {
                EngineProbe::Unknown
            };
            (pid, probe)
        })
        .collect();
    #[cfg(unix)]
    {
        let valid: Vec<_> = probes
            .iter()
            .filter(|(_, probe)| **probe == EngineProbe::Unknown)
            .map(|(&pid, _)| pid)
            .collect();
        if !valid.is_empty() {
            let selection = valid
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let output = Command::new("ps")
                .args(["-o", "pid=,comm=", "-p", &selection])
                .output_with_deadline(Duration::from_secs(2));
            probes.extend(classify_engine_output(&valid, output));
        }
    }
    #[cfg(not(unix))]
    let _ = &mut probes;
    probes
}

#[cfg(unix)]
fn classify_engine_output(pids: &[u32], output: io::Result<Output>) -> HashMap<u32, EngineProbe> {
    let output = output.ok().filter(|out| match out.status.code() {
        Some(0) => true,
        // ps also uses exit 1 for invalid arguments; only silence means gone.
        Some(1) => String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        _ => false,
    });
    let initial = if output.is_some() {
        EngineProbe::NotEngine
    } else {
        EngineProbe::Unknown
    };
    let mut probes: HashMap<_, _> = pids.iter().map(|&pid| (pid, initial)).collect();
    if let Some(output) = output {
        for row in String::from_utf8_lossy(&output.stdout).lines() {
            let row = row.trim();
            let Some((pid, command)) = row.split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            if let Some(probe) = probes.get_mut(&pid) {
                *probe = if engine_command(command.trim()) {
                    EngineProbe::Engine
                } else {
                    EngineProbe::NotEngine
                };
            }
        }
    }
    probes
}

#[cfg(unix)]
fn engine_command(command: &str) -> bool {
    std::path::Path::new(command)
        .file_name()
        .is_some_and(|name| name == "csr-engine" || name == "csr-engine.exe")
}

/// The binary a hook re-spawns as a detached `csr-engine <subcommand>` child:
/// this process's own executable, except when that executable is a cargo test
/// harness (it lives under `target/<profile>/deps/`). Handing a subcommand to
/// the libtest runner prints `error: Unrecognized option: '<flag>'` into the
/// test output and runs nothing, so a test that drives a hook handler
/// in-process gets `None` and spawns no child.
pub(crate) fn detached_child_exe() -> io::Result<Option<std::path::PathBuf>> {
    std::env::current_exe().map(exe_if_engine)
}

fn exe_if_engine(exe: std::path::PathBuf) -> Option<std::path::PathBuf> {
    (!is_cargo_harness_exe(&exe)).then_some(exe)
}

/// True when `exe` is a cargo test or bench harness. Two layouts exist: the
/// classic `target/<profile>/deps/<bin>` and the newer build-dir layout's
/// `<profile>/build/<pkg>/<hash>/out/<bin>` (what cargo 1.100 writes). An installed
/// `csr-engine` lives in neither.
pub(crate) fn is_cargo_harness_exe(exe: &std::path::Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    let named = |p: Option<&std::path::Path>, want: &str| {
        p.and_then(|p| p.file_name()).is_some_and(|n| n == want)
    };
    named(Some(dir), "deps") || (named(Some(dir), "out") && named(dir.ancestors().nth(3), "build"))
}

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
    use std::os::unix::process::ExitStatusExt;

    fn ps_output(code: i32, stdout: &str, stderr: &str) -> io::Result<Output> {
        Ok(Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    #[test]
    fn engine_probe_classifies_rows_and_absent_pids_for_exit_zero_and_one() {
        for code in [0, 1] {
            let probes = classify_engine_output(
                &[123, 456, 789, 999],
                ps_output(
                    code,
                    "  123 /usr/local/bin/csr-engine\n456 /bin/sleep\n999 csr-engine.exe\n",
                    "",
                ),
            );
            assert_eq!(probes[&123], EngineProbe::Engine);
            assert_eq!(probes[&456], EngineProbe::NotEngine);
            assert_eq!(probes[&789], EngineProbe::NotEngine);
            assert_eq!(probes[&999], EngineProbe::Engine);
            assert_eq!(probes.len(), 4);
        }
    }

    #[test]
    fn engine_probe_exit_one_with_stderr_is_unknown_even_with_live_rows() {
        let probes = classify_engine_output(
            &[123, 456],
            ps_output(1, "123 csr-engine\n", "ps: process id too large: 999999\n"),
        );
        assert_eq!(probes.len(), 2);
        assert!(probes.values().all(|probe| *probe == EngineProbe::Unknown));
    }

    #[test]
    fn engine_probe_exit_one_without_rows_or_diagnostics_means_gone() {
        for stderr in ["", " \t\n"] {
            let probes = classify_engine_output(&[123, 456], ps_output(1, "", stderr));
            assert_eq!(probes.len(), 2);
            assert!(probes
                .values()
                .all(|probe| *probe == EngineProbe::NotEngine));
        }
    }

    #[test]
    fn engine_probe_exit_zero_is_definitive_even_with_stderr() {
        let probes =
            classify_engine_output(&[123, 456], ps_output(0, "123 csr-engine\n", "warning"));
        assert_eq!(probes[&123], EngineProbe::Engine);
        assert_eq!(probes[&456], EngineProbe::NotEngine);
    }

    #[test]
    fn engine_probe_errors_are_unknown_for_every_pid() {
        let signal = Ok(Output {
            status: std::process::ExitStatus::from_raw(libc::SIGTERM),
            stdout: b"123 csr-engine\n".to_vec(),
            stderr: Vec::new(),
        });
        for output in [
            ps_output(2, "123 csr-engine\n", ""),
            signal,
            Err(io::ErrorKind::TimedOut.into()),
            Err(io::ErrorKind::NotFound.into()),
        ] {
            let probes = classify_engine_output(&[123, 456], output);
            assert_eq!(probes.len(), 2);
            assert!(probes.values().all(|probe| *probe == EngineProbe::Unknown));
        }
    }

    #[test]
    fn engine_probe_rejects_invalid_pids_without_ps() {
        let probes = probe_engines(&[0, PID_MAX + 1, u32::MAX]);
        assert_eq!(probes[&0], EngineProbe::NotEngine);
        assert_eq!(probes[&(PID_MAX + 1)], EngineProbe::NotEngine);
        assert_eq!(probes[&u32::MAX], EngineProbe::NotEngine);
        assert!(probe_engines(&[]).is_empty());
    }

    #[test]
    fn engine_probe_rejects_a_live_sleep_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let probes = probe_engines(&[pid]);
        let killed = child.kill();
        let reaped = child.wait();
        killed.unwrap();
        reaped.unwrap();
        assert_eq!(probes[&pid], EngineProbe::NotEngine);
    }

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

#[cfg(test)]
mod detached_child_exe_tests {
    use super::exe_if_engine;
    use std::path::PathBuf;

    #[test]
    fn a_cargo_test_binary_is_never_a_detached_child() {
        assert_eq!(
            exe_if_engine(PathBuf::from(
                "/w/csr-engine/target/release/deps/hooks_integration-0123abcd"
            )),
            None
        );
        assert_eq!(
            exe_if_engine(PathBuf::from("/w/target/debug/deps/csr_engine-ff00")),
            None
        );
    }

    #[test]
    fn a_test_binary_in_the_newer_build_dir_layout_is_never_a_detached_child() {
        for path in [
            "/w/target/debug/build/csr-engine/8d64c12c204e6d9e/out/csr_engine-8d64c12c204e6d9e",
            "/w/target/release/build/csr-engine/0123abcd/out/hooks_integration-0123abcd",
        ] {
            assert_eq!(exe_if_engine(PathBuf::from(path)), None, "{path}");
        }
    }

    #[test]
    fn the_installed_engine_is_a_detached_child() {
        for path in [
            "/usr/local/bin/csr-engine",
            "/w/csr-engine/target/release/csr-engine",
            // The bin cargo builds sits at the top of the profile dir, not under build/.
            "/w/csr-engine/target/debug/csr-engine",
        ] {
            assert_eq!(
                exe_if_engine(PathBuf::from(path)),
                Some(PathBuf::from(path))
            );
        }
    }

    #[test]
    fn this_very_test_binary_is_refused() {
        // The unit-test harness runs from target/<profile>/deps/.
        assert_eq!(super::detached_child_exe().unwrap(), None);
    }
}
