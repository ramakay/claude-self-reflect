//! A pane whose terminal goes away ends, and a pane always answers SIGTERM.
//!
//! The pane runs on a real pty here. A hung-up terminal reads as end of file, and crossterm's
//! reader takes a read of zero bytes for "nothing yet" and asks again at once, so a pane that
//! only polls for events never comes back to its own loop: it burns a core and ignores signals.
#![cfg(unix)]
use std::{
    fs,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SID: &str = "hup-test";
/// The tests run side by side. A child that inherited another test's master would keep that
/// terminal alive, so the pty and the child it feeds are made one test at a time, with the
/// master and slave closed on exec.
static SPAWN: Mutex<()> = Mutex::new(());

struct Pane {
    child: Child,
    // `Some` until the terminal is closed.
    master: Option<(JoinHandle<fs::File>, Arc<AtomicBool>)>,
    // Everything the pane wrote to its terminal.
    shown: Arc<Mutex<Vec<u8>>>,
    home: tempfile::TempDir,
}
impl Drop for Pane {
    fn drop(&mut self) {
        // Only this test's own child is ever signalled, including after a failed assertion.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Pane {
    /// Start `companion pane --watch` on a fresh pty. With `controlling`, the pty is the pane's
    /// controlling terminal in a session of its own, as in a terminal emulator: its hangup then
    /// also sends SIGHUP.
    fn start(controlling: bool) -> Self {
        Self::start_with(controlling, &[])
    }
    /// With `envs` set in the pane's environment: the seams of the tests that make its loop stick
    /// (`CSR_PANE_TEST_STICK`), or arm an alarm before a reload (`CSR_PANE_TEST_ALARM`).
    fn start_with(controlling: bool, envs: &[(&str, &str)]) -> Self {
        let spawning = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let (mut master, mut slave) = (0, 0);
        let mut size = libc::winsize {
            ws_row: 30,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
        for fd in [master, slave] {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let mut command = Command::new(env!("CARGO_BIN_EXE_csr-engine"));
        command
            .env("HOME", home.path())
            .env("TERM_PROGRAM", "kitty")
            .env_remove("CSR_COMPANION_PIXELS")
            .env_remove("CSR_PANE_HERDR_EVENTS")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("CSR_PANE_HOME_ENGINE")
            .args([
                "companion",
                "pane",
                "--repo",
                env!("CARGO_MANIFEST_DIR"),
                "--watch",
                "--sid",
                SID,
            ])
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap());
        command.envs(envs.iter().copied());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HERDR") {
                command.env_remove(key);
            }
        }
        if controlling {
            unsafe {
                command.pre_exec(|| {
                    libc::setsid();
                    libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                    Ok(())
                });
            }
        }
        let child = command.spawn().unwrap();
        drop((slave, command));
        drop(spawning);
        // Nobody reads a terminal's output but its emulator: keep draining, or the pane blocks
        // on a full pty.
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        let stop = Arc::new(AtomicBool::new(false));
        let ended = stop.clone();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let kept = shown.clone();
        let drain = thread::spawn(move || {
            let mut master = master;
            let mut sink = [0u8; 8192];
            while !ended.load(Ordering::Relaxed) {
                match master.read(&mut sink) {
                    Ok(n @ 1..) => kept.lock().unwrap().extend_from_slice(&sink[..n]),
                    _ => thread::sleep(Duration::from_millis(5)),
                }
            }
            master
        });
        let pane = Self {
            child,
            master: Some((drain, stop)),
            shown,
            home,
        };
        let up = Instant::now();
        while !pane.wake().exists() && up.elapsed() < Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(pane.wake().exists(), "the pane came up");
        // Raw mode and the first frames come just after.
        thread::sleep(Duration::from_millis(500));
        pane
    }
    fn panes(&self) -> PathBuf {
        self.home
            .path()
            .join(".claude-self-reflect/csr-companion/panes")
    }
    fn wake(&self) -> PathBuf {
        self.panes().join(format!("{SID}.{}.wake", self.child.id()))
    }
    fn lock(&self) -> PathBuf {
        self.panes().join(format!("{SID}.{}.lock", self.child.id()))
    }
    fn pid_file(&self) -> PathBuf {
        self.panes().join(format!("{SID}.json"))
    }
    /// The terminal goes away: its emulator (or multiplexer) dies and the master is closed.
    fn close_terminal(&mut self) {
        let (drain, stop) = self.master.take().unwrap();
        stop.store(true, Ordering::Relaxed);
        drop(drain.join().unwrap());
    }
    fn signal(&self, signal: libc::c_int) {
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }
    fn terminate(&self) {
        self.signal(libc::SIGTERM);
    }
    /// Whether the pane gave the terminal back (cursor shown again) before it ended.
    fn restored_the_terminal(&self) -> bool {
        let shown = self.shown.lock().unwrap();
        shown.windows(6).any(|w| w == b"\x1b[?25h")
    }
    /// The exit status, if the pane ends within `within`.
    fn ends_within(&mut self, within: Duration) -> Option<ExitStatus> {
        let start = Instant::now();
        while start.elapsed() < within {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.child.try_wait().unwrap()
    }
    /// CPU seconds the pane has used so far.
    fn cpu(&self) -> f64 {
        let out = Command::new("ps")
            .args(["-o", "cputime=", "-p", &self.child.id().to_string()])
            .stderr(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .split(':')
            .fold(0., |total, part| {
                total * 60. + part.parse::<f64>().unwrap_or(0.)
            })
    }
    fn left_nothing_behind(&self) {
        for left in [self.wake(), self.lock(), self.pid_file()] {
            assert!(!exists(&left), "{} was left behind", left.display());
        }
    }
}
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[test]
fn a_pane_whose_terminal_hung_up_ends_and_cleans_up() {
    let mut pane = Pane::start(false);
    pane.close_terminal();
    assert!(
        pane.ends_within(Duration::from_secs(3)).is_some(),
        "the pane outlived its terminal"
    );
    pane.left_nothing_behind();
}

#[test]
fn a_pane_whose_controlling_terminal_hung_up_ends_and_cleans_up() {
    let mut pane = Pane::start(true);
    pane.close_terminal();
    assert!(
        pane.ends_within(Duration::from_secs(3)).is_some(),
        "the pane outlived its controlling terminal"
    );
    pane.left_nothing_behind();
}

#[test]
fn sigterm_ends_an_idle_pane_and_cleans_up() {
    let mut pane = Pane::start(false);
    pane.terminate();
    assert!(
        pane.ends_within(Duration::from_secs(2)).is_some(),
        "SIGTERM did not end an idle pane"
    );
    pane.left_nothing_behind();
    thread::sleep(Duration::from_millis(100));
    assert!(
        pane.restored_the_terminal(),
        "a live terminal was left as the pane had it"
    );
}

/// A SIGHUP to a pane whose terminal is alive (a wrapper that forwards it) is an end like any
/// other, and the terminal still comes back.
#[test]
fn sighup_ends_a_pane_on_a_live_terminal_and_gives_it_back() {
    let mut pane = Pane::start(false);
    pane.signal(libc::SIGHUP);
    assert!(
        pane.ends_within(Duration::from_secs(2)).is_some(),
        "SIGHUP did not end the pane"
    );
    pane.left_nothing_behind();
    thread::sleep(Duration::from_millis(100));
    assert!(pane.restored_the_terminal());
}

/// The panes found spinning under launchd had been asked to end and had not: the hangup first,
/// the SIGTERM straight after it.
#[test]
fn sigterm_straight_after_the_hangup_ends_the_pane() {
    let mut pane = Pane::start(false);
    pane.close_terminal();
    pane.terminate();
    assert!(
        pane.ends_within(Duration::from_secs(3)).is_some(),
        "SIGTERM straight after the hangup did not end the pane"
    );
    pane.left_nothing_behind();
}

#[test]
fn sigterm_ends_a_pane_whose_terminal_is_gone() {
    let mut pane = Pane::start(false);
    pane.close_terminal();
    // Long enough for the pane to be inside its read of the dead terminal.
    thread::sleep(Duration::from_millis(700));
    pane.terminate();
    assert!(
        pane.ends_within(Duration::from_secs(2)).is_some(),
        "SIGTERM did not end a pane whose terminal is gone"
    );
    pane.left_nothing_behind();
}

/// A pane on a live terminal never ends of its own accord, and an idle one costs next to nothing.
#[test]
fn a_pane_on_a_live_terminal_stays_up_at_low_cpu() {
    let mut pane = Pane::start(true);
    let cpu = pane.cpu();
    let start = Instant::now();
    assert!(
        pane.ends_within(Duration::from_secs(6)).is_none(),
        "a pane on a live terminal ended"
    );
    let used = pane.cpu() - cpu;
    assert!(
        used < 1.5,
        "{used:.2}s of CPU in {:?} on an idle pane",
        start.elapsed()
    );
    assert!(exists(&pane.wake()) && exists(&pane.pid_file()));
}

/// The loop cannot end the pane when something inside it never returns (crossterm's reader on a
/// hangup that came after the loop looked). The watchdog ends it, and cleans up as a clean exit does.
#[test]
fn a_pane_stuck_after_its_terminal_hung_up_is_ended_by_the_watchdog() {
    let mut pane = Pane::start_with(false, &[("CSR_PANE_TEST_STICK", "ending")]);
    pane.close_terminal();
    assert!(
        pane.ends_within(Duration::from_secs(6)).is_some(),
        "a stuck pane outlived its terminal"
    );
    pane.left_nothing_behind();
}

#[test]
fn a_stuck_pane_is_ended_by_the_watchdog_on_sigterm_and_gives_the_terminal_back() {
    let mut pane = Pane::start_with(false, &[("CSR_PANE_TEST_STICK", "ending")]);
    pane.terminate();
    assert!(
        pane.ends_within(Duration::from_secs(6)).is_some(),
        "SIGTERM did not end a stuck pane"
    );
    pane.left_nothing_behind();
    thread::sleep(Duration::from_millis(100));
    assert!(pane.restored_the_terminal());
}

/// A pane that is only busy, with a live terminal and no end asked for, is never ended.
#[test]
fn a_busy_pane_on_a_live_terminal_is_left_alone() {
    let mut pane = Pane::start_with(false, &[("CSR_PANE_TEST_STICK", "busy")]);
    assert!(
        pane.ends_within(Duration::from_secs(8)).is_none(),
        "the watchdog ended a pane that was only busy"
    );
}

/// The executable a process runs, as `ps` names it.
fn running(pid: u32) -> String {
    let out = Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// An alarm armed before a reload (as the watchdog arms one to bound its write to the terminal)
/// survives the exec: it must not be left to kill the engine that replaces the pane.
#[test]
fn a_reload_with_an_alarm_armed_leaves_the_new_engine_alive() {
    let mut pane = Pane::start_with(false, &[("CSR_PANE_TEST_ALARM", "1")]);
    let engine = pane.home.path().join("csr-engine-reloaded");
    fs::copy(env!("CARGO_BIN_EXE_csr-engine"), &engine).unwrap();
    // Run the copy once and wait for it to exit before the pane is pointed at it: macOS scans a new
    // file the first time it runs (1.5 to 4.9 s on the dev laptop), and the pane gives its version
    // probe of the copy only 5 s.
    let warmed = Command::new(&engine)
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(warmed.success());
    fs::write(
        pane.home
            .path()
            .join(".claude-self-reflect/csr-companion/pane-engine"),
        engine.to_string_lossy().as_bytes(),
    )
    .unwrap();
    let pid = pane.child.id();
    let start = Instant::now();
    while !running(pid).ends_with("csr-engine-reloaded")
        && start.elapsed() < Duration::from_secs(15)
    {
        assert!(pane.child.try_wait().unwrap().is_none(), "the pane died");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        running(pid).ends_with("csr-engine-reloaded"),
        "the pane did not reload"
    );
    assert!(
        pane.ends_within(Duration::from_secs(3)).is_none(),
        "the engine that replaced the pane was killed by the old one's alarm"
    );
}
