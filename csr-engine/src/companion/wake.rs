//! Waking a pane: the relay tells a pane its session did something, the pane draws.
//!
//! The relay (`companion serve`) sees every hook of its Claude Code session. On the ones that
//! change what a pane shows it writes one byte to a named pipe the pane keeps at
//! `<keep>/panes/<sid>.wake`:
//!
//! - `N`, at once: a turn ended, a prompt was sent, a compaction began.
//! - `S`, soon: a tool finished, a measurement came. These arrive in bursts, so the pane
//!   waits 300 ms for the burst to gather.
//!
//! A byte only lowers the latency of a redraw. The pane's own checks (`gate.rs`) are correct
//! without it, for the sessions whose relay cannot reach them.
//!
//! No process id is trusted. The relay opens the pipe without blocking: with no pane reading it
//! (one that ended, or crashed and left the file) the open fails and nothing is sent, and a
//! process that took the dead pane's pid is never addressed at all.
use super::paths::valid_id;
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// What a hook means for a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Draw now.
    Now,
    /// Draw soon: more of the same are likely to follow.
    Burst,
}
impl Kind {
    fn byte(self) -> u8 {
        match self {
            Kind::Now => b'N',
            Kind::Burst => b'S',
        }
    }
}

/// What a relay request is to a pane: `path` is `/event` or `/after`, `key` the hook's key.
pub fn classify(path: &str, key: &str) -> Option<Kind> {
    match (path, key) {
        // Stop, prompt submit, PreCompact, and a subagent's end.
        (
            "/event",
            "stop" | "turn_start" | "turn_complete" | "seen" | "digit" | "chapter" | "marker",
        )
        | ("/after", "spawned") => Some(Kind::Now),
        // PostToolUse, and the usage figures that follow a reply.
        ("/after", "tool") | ("/event", "measure") => Some(Kind::Burst),
        _ => None,
    }
}

/// Where the pane of `sid` with process id `pid` listens. Every pane has an endpoint of its own,
/// so a second pane of the same session never takes the first one's.
pub fn pipe_path(keep: &Path, sid: &str, pid: u32) -> Option<PathBuf> {
    valid_id(sid).then(|| keep.join("panes").join(format!("{sid}.{pid}.wake")))
}

/// The endpoints of the panes of `sid`: `<sid>.<pid>.wake` (an id has no dot, so the name splits
/// cleanly).
fn endpoints(keep: &Path, sid: &str) -> Vec<(PathBuf, u32)> {
    if !valid_id(sid) {
        return vec![];
    }
    fs::read_dir(keep.join("panes"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let pid = name
                .strip_prefix(sid)?
                .strip_prefix('.')?
                .strip_suffix(".wake")?
                .parse::<u32>()
                .ok()?;
            Some((entry.path(), pid))
        })
        .collect()
}

/// Whether no process has this pid. Only used to tidy a pipe up, never to decide who is sent
/// anything.
fn gone(pid: u32) -> bool {
    pid == 0
        || (unsafe { libc::kill(pid as i32, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
}

/// Tell the panes of `sid`, those that are listening. True when the byte reached at least one;
/// a pane that is not there, or not reading, or not keeping up, is no error. The pipe of a pane
/// whose process is gone is removed.
pub fn notify(keep: &Path, sid: &str, kind: Kind) -> bool {
    let mut sent = false;
    for (path, pid) in endpoints(keep, sid) {
        let Ok(mut pipe) = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&path)
        else {
            // Nobody reads it: it is a pane's pipe in the moment before it opens it, or a dead
            // pane's.
            if gone(pid) && fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_fifo()) {
                let _ = fs::remove_file(&path);
            }
            continue;
        };
        // Whatever sits at the path, a pipe of ours is the only thing a byte goes to.
        let ours = pipe
            .metadata()
            .is_ok_and(|m| m.file_type().is_fifo() && m.uid() == unsafe { libc::geteuid() });
        sent |= ours && pipe.write_all(&[kind.byte()]).is_ok();
    }
    sent
}

/// The pane's end of its pipe. Dropping it removes the pipe, if it is still this one.
pub struct Listener {
    path: PathBuf,
    pipe: fs::File,
    id: (u64, u64),
}
impl Listener {
    /// Make this process's pipe for `sid` and open it for reading, replacing whatever an earlier
    /// process of the same pid left. None for an id that cannot be a file name, or when the
    /// system says no.
    pub fn open(keep: &Path, sid: &str) -> Option<Self> {
        Self::open_as(keep, sid, std::process::id())
    }
    fn open_as(keep: &Path, sid: &str, pid: u32) -> Option<Self> {
        let path = pipe_path(keep, sid, pid)?;
        let _ = fs::remove_file(&path);
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return None;
        }
        // O_CLOEXEC is Rust's default: a reload by `exec` closes it and the next process makes
        // its own, under the same pid and so the same name.
        let pipe = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .ok()?;
        let info = pipe.metadata().ok()?;
        Some(Self {
            path,
            id: (info.dev(), info.ino()),
            pipe,
        })
    }
    /// The wakes that came since the last call: (at once, soon).
    pub fn take(&mut self) -> (bool, bool) {
        let (mut now, mut soon) = (false, false);
        let mut buffer = [0u8; 256];
        while let Ok(n @ 1..) = self.pipe.read(&mut buffer) {
            for byte in &buffer[..n] {
                match byte {
                    b'N' => now = true,
                    b'S' => soon = true,
                    _ => (),
                }
            }
        }
        (now, soon)
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.id) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// How long a burst gathers before its draw.
pub const GATHER: Duration = Duration::from_millis(300);
/// No more than one wake's draw in this long.
pub const SPACING: Duration = Duration::from_secs(1);

/// When the signals a pane got become draws: a leading edge for `Now`, a trailing one for
/// `Burst`, and never closer than `SPACING`. A pure clock-in, answer-out struct.
#[derive(Default)]
pub struct Debounce {
    want: Option<Instant>,
    drew: Option<Instant>,
}
impl Debounce {
    pub fn signal(&mut self, at: Instant, kind: Kind) {
        let due = match kind {
            Kind::Now => at,
            Kind::Burst => at + GATHER,
        };
        self.want = Some(self.want.map_or(due, |w| w.min(due)));
    }
    /// A draw happened at `at`, for whatever reason.
    pub fn drew(&mut self, at: Instant) {
        self.drew = Some(at);
    }
    /// Whether a wake's draw is due at `at`. True once per wake.
    pub fn ready(&mut self, at: Instant) -> bool {
        let Some(want) = self.want else {
            return false;
        };
        let open = self
            .drew
            .is_none_or(|d| at.saturating_duration_since(d) >= SPACING);
        if want > at || !open {
            return false;
        }
        self.want = None;
        true
    }
    /// When to look again: the wake's draw is due, or the spacing ends.
    pub fn next(&self) -> Option<Instant> {
        let want = self.want?;
        Some(self.drew.map_or(want, |d| want.max(d + SPACING)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn the_hooks_that_change_a_pane_are_told_apart_by_how_soon() {
        for key in ["stop", "seen", "digit", "turn_start", "chapter", "marker"] {
            assert_eq!(classify("/event", key), Some(Kind::Now), "{key}");
        }
        assert_eq!(classify("/after", "spawned"), Some(Kind::Now));
        assert_eq!(classify("/after", "tool"), Some(Kind::Burst));
        assert_eq!(classify("/event", "measure"), Some(Kind::Burst));
        // Before a tool runs, a render, a config: nothing new to show.
        for (path, key) in [
            ("/event", "tool"),
            ("/event", "band"),
            ("/after", "stop"),
            ("/step", "step"),
            ("/event", "config"),
        ] {
            assert_eq!(classify(path, key), None, "{path} {key}");
        }
    }

    #[test]
    fn fifty_wakes_through_the_pipe_in_a_second_draw_at_most_twice() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let mut pipe = Listener::open(d.path(), "s").unwrap();
        let t0 = Instant::now();
        let mut wake = Debounce::default();
        let mut draws = 0;
        let mut sent = 0;
        for tick in 0..=1200 {
            let at = t0 + Duration::from_millis(tick);
            if tick < 1000 && tick % 20 == 0 {
                assert!(notify(d.path(), "s", Kind::Now), "wake {sent} is written");
                sent += 1;
            }
            // What the watch loop does each pass: the pipe's bytes into the debounce.
            let (now, soon) = pipe.take();
            if now {
                wake.signal(at, Kind::Now);
            }
            if soon {
                wake.signal(at, Kind::Burst);
            }
            if wake.ready(at) {
                wake.drew(at);
                draws += 1;
            }
        }
        assert_eq!(sent, 50);
        assert_eq!(
            draws, 2,
            "the first at once, the rest together a second later"
        );
    }

    #[test]
    fn a_wake_reaches_the_pane_that_listens_and_says_which_kind() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let mut pipe = Listener::open(d.path(), "s").unwrap();
        assert_eq!(pipe.take(), (false, false));
        assert!(notify(d.path(), "s", Kind::Now));
        assert_eq!(pipe.take(), (true, false));
        assert!(notify(d.path(), "s", Kind::Burst));
        assert_eq!(pipe.take(), (false, true));
        assert!(notify(d.path(), "s", Kind::Burst));
        assert!(notify(d.path(), "s", Kind::Now));
        assert_eq!(pipe.take(), (true, true));
        assert!(!notify(d.path(), "other", Kind::Now), "another session");
        assert!(!notify(d.path(), "../escape", Kind::Now));
    }

    #[test]
    fn nobody_listening_is_a_silent_nothing() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        assert!(!notify(d.path(), "s", Kind::Now), "no pipe");
        // A pane that crashed leaves its pipe behind with nobody to read it.
        drop(Listener::open(d.path(), "s").unwrap());
        let left = pipe_path(d.path(), "s", std::process::id()).unwrap();
        assert!(!left.exists(), "a pane that ends takes its pipe with it");
        let name = std::ffi::CString::new(left.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(!notify(d.path(), "s", Kind::Now), "left behind, unread");
        // Nothing but a pipe is written to.
        fs::remove_file(&left).unwrap();
        fs::write(&left, "keep me").unwrap();
        assert!(!notify(d.path(), "s", Kind::Now));
        assert_eq!(fs::read_to_string(&left).unwrap(), "keep me");
        fs::remove_file(&left).unwrap();
        std::os::unix::fs::symlink(d.path().join("elsewhere"), &left).unwrap();
        assert!(!notify(d.path(), "s", Kind::Now));
        assert!(!d.path().join("elsewhere").exists());
    }

    #[test]
    fn a_pane_that_does_not_read_never_holds_the_relay() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let _pipe = Listener::open(d.path(), "s").unwrap();
        let began = Instant::now();
        let written = (0..70_000)
            .filter(|_| notify(d.path(), "s", Kind::Burst))
            .count();
        assert!(
            written > 0 && written < 70_000,
            "the pipe filled: {written}"
        );
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "and nothing blocked"
        );
    }

    #[test]
    fn two_panes_of_one_session_each_get_the_wake_and_one_ending_costs_the_other_nothing() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let mut older = Listener::open_as(d.path(), "s", 1_000_001).unwrap();
        let mut newer = Listener::open_as(d.path(), "s", 1_000_002).unwrap();
        let mut other = Listener::open_as(d.path(), "t", 1_000_003).unwrap();
        assert!(notify(d.path(), "s", Kind::Now));
        assert_eq!(older.take(), (true, false), "the first pane still listens");
        assert_eq!(newer.take(), (true, false));
        assert_eq!(other.take(), (false, false), "another session's pane");
        let older_pipe = pipe_path(d.path(), "s", 1_000_001).unwrap();
        drop(older);
        assert!(!older_pipe.exists());
        assert!(notify(d.path(), "s", Kind::Burst));
        assert_eq!(newer.take(), (false, true), "the newer pane goes on");
        drop(newer);
        assert!(!notify(d.path(), "s", Kind::Now), "nobody left");
    }

    #[test]
    fn the_pipe_of_a_process_that_is_gone_is_removed_and_a_live_ones_is_not() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let mut child = Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let make = |pid: u32| {
            let path = pipe_path(d.path(), "s", pid).unwrap();
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            path
        };
        let left = make(dead);
        let alive = make(std::process::id());
        assert!(!notify(d.path(), "s", Kind::Now));
        assert!(!left.exists(), "a dead pane's pipe is tidied away");
        assert!(
            alive.exists(),
            "a live process may be about to read its own"
        );
    }

    #[test]
    fn a_burst_of_tools_gathers_for_300_ms_and_a_turn_end_does_not_wait() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut wake = Debounce::default();
        wake.signal(ms(0), Kind::Burst);
        wake.signal(ms(100), Kind::Burst);
        assert!(!wake.ready(ms(299)));
        assert!(wake.ready(ms(300)));
        wake.drew(ms(300));
        assert!(!wake.ready(ms(301)), "nothing left to draw");
        // A Stop wins over a gathering burst.
        wake.signal(ms(2000), Kind::Burst);
        wake.signal(ms(2010), Kind::Now);
        assert!(wake.ready(ms(2010)));
    }

    #[test]
    fn a_wake_never_draws_closer_than_a_second_to_the_last_draw() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut wake = Debounce::default();
        wake.drew(ms(0));
        wake.signal(ms(100), Kind::Now);
        assert!(!wake.ready(ms(999)));
        assert_eq!(wake.next(), Some(ms(1000)));
        assert!(wake.ready(ms(1000)));
    }
}
