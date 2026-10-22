//! Waking a pane: the relay tells a pane its session did something, the pane draws.
//!
//! The relay (`companion serve`) sees every hook of its Claude Code session. On the ones that
//! change what a pane shows it writes one byte to a named pipe the pane keeps at
//! `<keep>/panes/<sid>.<pid>.wake` (one per pane, so a second pane of a session takes nothing
//! from the first):
//!
//! - `N`, at once: a turn ended, a prompt was sent, a compaction began.
//! - `S`, soon: a tool finished, a measurement came. These arrive in bursts, so the pane
//!   waits 300 ms for the burst to gather.
//!
//! A byte only lowers the latency of a redraw. The pane's own checks (`gate.rs`) are correct
//! without it, for the sessions whose relay cannot reach them.
//!
//! No process id is trusted, and the relay never removes anything. It opens each pipe without
//! blocking: with no pane reading it (one that ended, or crashed and left the file) the open
//! fails and nothing is sent, and a process that took the dead pane's pid is never addressed at
//! all.
//!
//! Who is alive is told by a lock, not by a pid. A pane takes `flock(LOCK_EX)` on
//! `<sid>.<pid>.lock` before it makes its pipe and holds it for as long as it lives, and removes
//! both on a clean exit. The kernel drops the lock when the pane dies, however it dies. The next
//! pane to start sweeps the entries it can lock with `LOCK_NB`: that no one else holds them is
//! proved by the lock itself, whatever the pids or pid namespaces. The sweep and the pane remove
//! only files of ours, in a folder of ours.
//!
//! Threat model: the same user. `keep` is CSR's private folder, so a link among its ancestors is
//! accepted (the home folder may be a link to another disk); `panes` itself must be a real folder
//! of ours, made 0700, or no pipe is used.
use super::paths::valid_id;
use std::{
    fs,
    io::{Read, Write},
    os::fd::AsRawFd,
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

fn euid() -> u32 {
    unsafe { libc::geteuid() }
}

/// The folder of the panes' pipes and locks, when it is a real folder (not a link) of ours.
fn panes_dir(keep: &Path) -> Option<PathBuf> {
    let dir = keep.join("panes");
    let info = fs::symlink_metadata(&dir).ok()?;
    (info.is_dir() && info.uid() == euid()).then_some(dir)
}

/// Tell the panes of `sid`, those that are listening. True when the byte reached at least one;
/// a pane that is not there, or not reading, or not keeping up, is no error. Nothing is ever
/// removed from here: a pipe no one reads is a pane's, about to be opened or left by one that
/// crashed, and the next pane sweeps it.
pub fn notify(keep: &Path, sid: &str, kind: Kind) -> bool {
    if panes_dir(keep).is_none() {
        return false;
    }
    let mut sent = false;
    for (path, _) in endpoints(keep, sid) {
        let Ok(mut pipe) = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&path)
        else {
            continue;
        };
        // Whatever sits at the path, a pipe of ours is the only thing a byte goes to.
        let ours = pipe
            .metadata()
            .is_ok_and(|m| m.file_type().is_fifo() && m.uid() == euid());
        sent |= ours && pipe.write_all(&[kind.byte()]).is_ok();
    }
    sent
}

/// Where the lock of the pane `pid` of `sid` is.
fn lock_path(keep: &Path, sid: &str, pid: u32) -> Option<PathBuf> {
    valid_id(sid).then(|| keep.join("panes").join(format!("{sid}.{pid}.lock")))
}

/// `flock(LOCK_EX | LOCK_NB)`: true when this call got the lock.
fn try_lock(file: &fs::File) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Whether `path` still names the file `file` is open on: the lock file of a pane that ended
/// can be removed between the open and the lock.
fn still_there(file: &fs::File, path: &Path) -> bool {
    match (file.metadata(), fs::symlink_metadata(path)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// Take the lock at `path`, making the file. None when someone alive holds it, or the file is
/// not a plain file of ours. A holder that is only sweeping lets go within moments, so a held
/// lock is tried a few times.
fn take_lock(path: &Path) -> Option<fs::File> {
    for _ in 0..5 {
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .ok()?;
        let info = file.metadata().ok()?;
        if !info.is_file() || info.uid() != euid() {
            return None;
        }
        if !try_lock(&file) {
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        if still_there(&file, path) {
            return Some(file);
        }
    }
    None
}

/// Remove what crashed panes left: for each lock in `dir` that can be taken now (so no live
/// pane holds it, whatever its pid or pid namespace) and is a plain file of ours, its pipe if
/// that is a pipe of ours, then the lock, both while holding it. `except` is the caller's own.
fn sweep(dir: &Path, except: &Path) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let lock = entry.path();
        if lock == except || lock.extension().is_none_or(|e| e != "lock") {
            continue;
        }
        let Some(stem) = lock.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some((sid, pid)) = stem.rsplit_once('.') else {
            continue;
        };
        if !valid_id(sid) || pid.parse::<u32>().is_err() {
            continue;
        }
        let Ok(held) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&lock)
        else {
            continue;
        };
        let ours = held
            .metadata()
            .is_ok_and(|m| m.is_file() && m.uid() == euid());
        if !ours || !try_lock(&held) || !still_there(&held, &lock) {
            continue;
        }
        let pipe = dir.join(format!("{stem}.wake"));
        match fs::symlink_metadata(&pipe) {
            Ok(m) if m.file_type().is_fifo() && m.uid() == euid() => {
                let _ = fs::remove_file(&pipe);
            }
            Ok(_) => continue,
            Err(_) => (),
        }
        let _ = fs::remove_file(&lock);
    }
}

/// The pane's end of its pipe. Dropping it removes the pipe, if it is still this one, and the
/// lock.
pub struct Listener {
    path: PathBuf,
    pipe: fs::File,
    id: (u64, u64),
    lock_path: PathBuf,
    lock: fs::File,
}
impl Listener {
    /// Make this process's pipe for `sid` and open it for reading, replacing whatever an earlier
    /// process of the same pid left. None for an id that cannot be a file name, a `panes` folder
    /// that is not ours, or when the system says no.
    pub fn open(keep: &Path, sid: &str) -> Option<Self> {
        Self::open_as(keep, sid, std::process::id())
    }
    fn open_as(keep: &Path, sid: &str, pid: u32) -> Option<Self> {
        let lock_path = lock_path(keep, sid, pid)?;
        super::private::ensure_dir(&keep.join("panes"), keep).ok()?;
        let dir = panes_dir(keep)?;
        // Before the pipe exists, so that a pipe with no live lock behind it is always a dead
        // pane's.
        let lock = take_lock(&lock_path)?;
        let made = (|| {
            sweep(&dir, &lock_path);
            let path = pipe_path(keep, sid, pid)?;
            // Holding the lock, no other process owns this name: it is an earlier image of this
            // very process (a reload by `exec`).
            let _ = fs::remove_file(&path);
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
            if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
                return None;
            }
            // O_CLOEXEC is Rust's default: a reload by `exec` closes the pipe and the lock, and
            // the next process makes its own, under the same pid and so the same names.
            let pipe = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
                .ok()?;
            let info = pipe.metadata().ok()?;
            Some((path, (info.dev(), info.ino()), pipe))
        })();
        let Some((path, id, pipe)) = made else {
            if still_there(&lock, &lock_path) {
                let _ = fs::remove_file(&lock_path);
            }
            return None;
        };
        Some(Self {
            path,
            id,
            pipe,
            lock_path,
            lock,
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
        // Still holding the lock: no pane can have taken the name in between.
        if still_there(&self.lock, &self.lock_path) {
            let _ = fs::remove_file(&self.lock_path);
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
    use std::{os::unix::fs::PermissionsExt, process::Command};

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

    /// What a pane that crashed leaves: a lock no one holds and its pipe.
    fn leave(keep: &Path, sid: &str, pid: u32) -> (PathBuf, PathBuf) {
        let lock = lock_path(keep, sid, pid).unwrap();
        fs::write(&lock, "").unwrap();
        let pipe = pipe_path(keep, sid, pid).unwrap();
        let name = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        (lock, pipe)
    }

    #[test]
    fn the_relay_never_removes_a_pipe_whatever_its_pid_says() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let mut child = Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        // A pid no process has, one a process has (this one: a reused pid looks the same), and
        // one in no namespace this process can see.
        let entries: Vec<_> = [dead, std::process::id(), 4_000_000_000]
            .into_iter()
            .map(|pid| leave(d.path(), "s", pid))
            .collect();
        for _ in 0..3 {
            assert!(!notify(d.path(), "s", Kind::Now));
            assert!(!notify(d.path(), "s", Kind::Burst));
        }
        for (lock, pipe) in &entries {
            assert!(lock.exists() && pipe.exists(), "the relay removed {pipe:?}");
        }
    }

    #[test]
    fn a_crashed_panes_entry_is_swept_by_the_next_pane() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let (lock, pipe) = leave(d.path(), "s", 1_000_001);
        let (other_lock, other_pipe) = leave(d.path(), "t", 1_000_002);
        // This process's own leftovers too: an earlier image of the same pid.
        let mine = leave(d.path(), "s", 1_000_003);
        let mut pane = Listener::open_as(d.path(), "s", 1_000_003).unwrap();
        for gone in [&lock, &pipe, &other_lock, &other_pipe] {
            assert!(!gone.exists(), "{gone:?} was left behind");
        }
        assert!(mine.0.exists() && mine.1.exists(), "the new pane's own");
        assert!(notify(d.path(), "s", Kind::Now));
        assert_eq!(pane.take(), (true, false), "and its pipe is a live one");
    }

    #[test]
    fn a_live_panes_entry_survives_a_sweep() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let mut live = Listener::open_as(d.path(), "s", 1_000_001).unwrap();
        let held = (
            lock_path(d.path(), "s", 1_000_001).unwrap(),
            pipe_path(d.path(), "s", 1_000_001).unwrap(),
        );
        let (stale_lock, stale_pipe) = leave(d.path(), "s", 1_000_002);
        // Starting panes sweep, any number of times; the pid on the lock does not matter.
        let mut next = Listener::open_as(d.path(), "s", 1_000_003).unwrap();
        let _again = Listener::open_as(d.path(), "u", 1_000_004).unwrap();
        assert!(held.0.exists() && held.1.exists(), "a live pane's entry");
        assert!(!stale_lock.exists() && !stale_pipe.exists());
        assert!(notify(d.path(), "s", Kind::Burst));
        assert_eq!(live.take(), (false, true));
        assert_eq!(next.take(), (false, true));
    }

    #[test]
    fn a_pane_that_ends_cleanly_removes_its_pipe_and_its_lock() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let pane = Listener::open_as(d.path(), "s", 1_000_001).unwrap();
        let lock = lock_path(d.path(), "s", 1_000_001).unwrap();
        assert!(lock.exists());
        drop(pane);
        let left: Vec<_> = fs::read_dir(d.path().join("panes")).unwrap().collect();
        assert!(left.is_empty(), "left: {left:?}");
    }

    #[test]
    fn a_pane_of_a_pid_that_a_live_pane_holds_does_not_replace_it() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("panes")).unwrap();
        let mut first = Listener::open_as(d.path(), "s", 1_000_001).unwrap();
        // The same name from another pid namespace: the lock says it is taken.
        assert!(Listener::open_as(d.path(), "s", 1_000_001).is_none());
        assert!(notify(d.path(), "s", Kind::Now));
        assert_eq!(first.take(), (true, false), "the first is untouched");
    }

    #[test]
    fn a_panes_folder_that_is_a_link_or_wide_open_is_not_used_as_it_is() {
        let d = tempfile::tempdir().unwrap();
        let elsewhere = d.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let keep = d.path().join("keep");
        fs::create_dir_all(&keep).unwrap();
        std::os::unix::fs::symlink(&elsewhere, keep.join("panes")).unwrap();
        assert!(Listener::open_as(&keep, "s", 1_000_001).is_none());
        assert!(!notify(&keep, "s", Kind::Now));
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing made");
        // A folder of ours that was made wide is made private again.
        fs::remove_file(keep.join("panes")).unwrap();
        fs::create_dir(keep.join("panes")).unwrap();
        fs::set_permissions(keep.join("panes"), fs::Permissions::from_mode(0o755)).unwrap();
        let _pane = Listener::open_as(&keep, "s", 1_000_001).unwrap();
        let mode = fs::metadata(keep.join("panes"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // And one made by the pane itself starts private.
        let fresh = d.path().join("fresh");
        let _other = Listener::open_as(&fresh, "s", 1_000_002).unwrap();
        let mode = fs::metadata(fresh.join("panes"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
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
