//! The decision journal of a resting pane: one JSON line for each time a pane rests, wakes,
//! stays awake or reads past something that could have kept it awake, so that "why is this pane
//! not asleep" has an answer in a file. It is always on and cheap: one small append at a decision
//! (never in a loop), in `<keep>/panes/<sid>.sleep.jsonl`, private like the other keep files, and
//! cut back to its last lines when it passes `KEEP_LINES`.
//!
//! The same cause comes at most once a minute (with a count of how many it stands for): a wake
//! byte per tool call or an agent that stays "working" would otherwise fill it. Nothing here
//! reads `render` or `pane`: a resting pane links this and stays small.
//!
//! The journal never holds a pane back. It is written only if what is at its path is a plain
//! file (a link, a pipe or a folder there is skipped, and the file is opened without waiting, so
//! that a pipe with no reader cannot stop a wake), and two panes of one session share it under
//! a lock that is tried for a moment and then given up on: a line is lost before a pane waits.
use super::{dormant::Focus, paths::valid_id, private};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, Write},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// The lines a journal grows to before it is cut back.
pub const KEEP_LINES: usize = 200;
/// And the lines it is cut back to.
const CUT_TO: usize = 100;
/// The same cause is written at most this often.
pub const SAME_CAUSE: Duration = Duration::from_secs(60);
/// A journal bigger than this is not read: it is replaced by its newest lines.
const MAX_READ: u64 = 1 << 20;
/// A journal of a pane that has not written for this long is removed when a pane starts.
const STALE: Duration = Duration::from_secs(14 * 86_400);

/// What the pane did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Went to rest.
    Rest,
    /// Woke from rest.
    Wake,
    /// Was due to rest, or had its idle clock reset, and stayed up.
    StayAwake,
    /// Saw something that looks like activity and read past it.
    Ignore,
    /// Ended.
    Exit,
}
impl Decision {
    fn word(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Wake => "wake",
            Self::StayAwake => "stay_awake",
            Self::Ignore => "ignore",
            Self::Exit => "exit",
        }
    }
}

/// What a look at one herdr pane came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Probe {
    /// herdr described the pane.
    Ok,
    /// herdr answered and has no such pane.
    Absent,
    /// herdr could not be asked, or its answer was not understood.
    Failed,
    /// There was no pane id to ask about.
    NoId,
}
impl Probe {
    fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Absent => "absent",
            Self::Failed => "failed",
            Self::NoId => "no_id",
        }
    }
}
/// What herdr answered about the agent's pane and about this one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Asked {
    pub agent: Probe,
    pub own: Probe,
}

/// Where the lock of the journal of `sid` is: whoever holds it is writing or cutting the journal.
pub fn lock_path(keep: &Path, sid: &str) -> Option<PathBuf> {
    valid_id(sid).then(|| keep.join("panes").join(format!("{sid}.sleep.lock")))
}
/// How long a lock is tried for, all told, before the line is let go.
const LOCK_TRIES: u32 = 20;
const LOCK_WAIT: Duration = Duration::from_millis(5);

/// `path` opened for reading, and for appending or writing, if it is a plain file: never through
/// a link, never waiting on a pipe, and the opened thing is looked at again. None for anything
/// else, and for a file that is not there when `create` is false.
fn open_plain(path: &Path, create: bool, append: bool) -> Option<fs::File> {
    match fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_file() => return None,
        Ok(_) => (),
        Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return None,
    }
    let mut open = fs::OpenOptions::new();
    open.read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    if append {
        open.append(true);
    } else if create {
        open.write(true);
    }
    if create {
        open.create(true).mode(0o600);
    }
    let file = open.open(path).ok()?;
    file.metadata().ok()?.file_type().is_file().then_some(file)
}
/// The lock on `path`: held while the returned file is. None if it is not a plain file or
/// someone holds it for longer than a moment.
fn take_lock(path: &Path) -> Option<fs::File> {
    let file = open_plain(path, true, false)?;
    for _ in 0..LOCK_TRIES {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        std::thread::sleep(LOCK_WAIT);
    }
    None
}

/// Where the journal of `sid` is.
pub fn path(keep: &Path, sid: &str) -> Option<PathBuf> {
    valid_id(sid).then(|| keep.join("panes").join(format!("{sid}.sleep.jsonl")))
}

/// Remove the journals nobody has written to for two weeks: their sessions are over.
pub fn prune(keep: &Path) {
    let now = SystemTime::now();
    for entry in std::fs::read_dir(keep.join("panes"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".sleep.jsonl") && !name.ends_with(".sleep.lock") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age >= STALE);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub struct Journal {
    keep: PathBuf,
    path: Option<PathBuf>,
    lock: Option<PathBuf>,
    /// For each cause written lately: when, and how many came since.
    lately: HashMap<String, (Instant, u32)>,
}
impl Journal {
    pub fn new(keep: &Path, sid: &str) -> Self {
        Self {
            keep: keep.to_path_buf(),
            path: path(keep, sid),
            lock: lock_path(keep, sid),
            lately: HashMap::new(),
        }
    }
    /// One line, now. A failure to write, or a path that is not a plain file, or a lock that
    /// does not come, is nobody's business: the line is not written.
    pub fn write(
        &mut self,
        decision: Decision,
        cause: &str,
        asked: Option<Asked>,
        focus: Focus,
        count: u32,
    ) -> bool {
        let (Some(path), Some(lock)) = (self.path.as_deref(), self.lock.as_deref()) else {
            return false;
        };
        let mut line = json!({
            "t": (super::paths::now() * 1000.0).round() / 1000.0,
            "pid": std::process::id(),
            "decision": decision.word(),
            "cause": cause,
            "herdr": asked.map_or(Value::Null, |a| json!({
                "agent": a.agent.word(),
                "own": a.own.word(),
            })),
            "focus": focus.word(),
        });
        if count > 1 {
            line["count"] = json!(count);
        }
        let line = format!("{line}\n");
        if let Some(panes) = path.parent() {
            if private::ensure_dir(panes, &self.keep).is_err() {
                return false;
            }
        }
        // Held for the whole append, and for the whole read, cut and rename: another pane of the
        // session cannot append between a cut's read and its rename and lose its line.
        let Some(_held) = take_lock(lock) else {
            return false;
        };
        let Some(mut file) = open_plain(path, true, true) else {
            return false;
        };
        let mut text = String::new();
        let size = file.metadata().map_or(0, |m| m.len());
        if size <= MAX_READ {
            let _ = file.rewind();
            let _ = (&file).take(MAX_READ).read_to_string(&mut text);
        }
        if file.write_all(line.as_bytes()).is_err() {
            return false;
        }
        let lines = text.matches('\n').count() + 1;
        if lines > KEEP_LINES || size > MAX_READ {
            text.push_str(&line);
            let all: Vec<&str> = text.lines().collect();
            let kept = &all[all.len().saturating_sub(CUT_TO)..];
            let mut out = kept.join("\n");
            out.push('\n');
            let _ = private::write_atomic(path, &out, &self.keep);
        }
        true
    }
    /// Like `write`, but the same decision and cause come at most once a minute; the line that
    /// finally comes says how many it stands for.
    pub fn note(
        &mut self,
        now: Instant,
        decision: Decision,
        cause: &str,
        asked: Option<Asked>,
        focus: Focus,
    ) {
        let key = format!("{}:{cause}", decision.word());
        match self.lately.get_mut(&key) {
            Some((at, held)) if now.saturating_duration_since(*at) < SAME_CAUSE => {
                *held += 1;
            }
            Some((at, held)) => {
                let count = *held + 1;
                *at = now;
                *held = 0;
                let _ = self.write(decision, cause, asked, focus, count);
            }
            None => {
                self.lately.insert(key, (now, 0));
                let _ = self.write(decision, cause, asked, focus, 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn keep() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("panes")).unwrap();
        dir
    }
    fn lines(keep: &Path, sid: &str) -> Vec<Value> {
        std::fs::read_to_string(path(keep, sid).unwrap())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_line_says_what_was_decided_why_and_what_herdr_and_the_terminal_said() {
        let d = keep();
        let mut journal = Journal::new(d.path(), "abc");
        journal.write(
            Decision::StayAwake,
            "herdr says agent pane focused",
            Some(Asked {
                agent: Probe::Ok,
                own: Probe::Failed,
            }),
            Focus::Unknown,
            1,
        );
        journal.write(Decision::Wake, "pipe byte", None, Focus::Out, 3);
        let seen = lines(d.path(), "abc");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0]["decision"], "stay_awake");
        assert_eq!(seen[0]["cause"], "herdr says agent pane focused");
        assert_eq!(seen[0]["herdr"]["agent"], "ok");
        assert_eq!(seen[0]["herdr"]["own"], "failed");
        assert_eq!(seen[0]["focus"], "unknown");
        assert!(seen[0]["t"].as_f64().unwrap() > 1.0e9);
        assert!(seen[0]["pid"].is_u64());
        assert!(seen[0].get("count").is_none());
        assert_eq!(seen[1]["decision"], "wake");
        assert!(seen[1]["herdr"].is_null());
        assert_eq!(seen[1]["focus"], "out");
        assert_eq!(seen[1]["count"], 3);
        let mode = std::fs::metadata(path(d.path(), "abc").unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn the_same_cause_is_written_once_a_minute_with_the_count_it_stands_for() {
        let d = keep();
        let mut journal = Journal::new(d.path(), "abc");
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        for s in 0..30 {
            journal.note(
                at(s),
                Decision::StayAwake,
                "idle clock reset by wake byte",
                None,
                Focus::In,
            );
        }
        // Another cause is its own.
        journal.note(
            at(5),
            Decision::Ignore,
            "transcript ignored: bridge-session",
            None,
            Focus::In,
        );
        assert_eq!(lines(d.path(), "abc").len(), 2, "one line per cause so far");
        journal.note(
            at(61),
            Decision::StayAwake,
            "idle clock reset by wake byte",
            None,
            Focus::In,
        );
        let seen = lines(d.path(), "abc");
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[2]["count"], 30, "29 held back and this one");
        journal.note(
            at(125),
            Decision::StayAwake,
            "idle clock reset by wake byte",
            None,
            Focus::In,
        );
        let seen = lines(d.path(), "abc");
        assert_eq!(seen.len(), 4);
        assert!(seen[3].get("count").is_none());
    }

    #[test]
    fn the_file_is_cut_back_to_its_last_lines_when_it_grows_past_the_cap() {
        let d = keep();
        let mut journal = Journal::new(d.path(), "abc");
        for n in 0..450 {
            journal.write(Decision::Wake, &format!("n{n}"), None, Focus::Unknown, 1);
        }
        let seen = lines(d.path(), "abc");
        assert!(seen.len() <= KEEP_LINES, "{}", seen.len());
        assert!(seen.len() >= CUT_TO);
        assert_eq!(seen.last().unwrap()["cause"], "n449", "the newest stays");
        // A pane that starts again (the exec) counts what is there and goes on.
        let mut next = Journal::new(d.path(), "abc");
        for n in 0..300 {
            next.write(Decision::Wake, &format!("m{n}"), None, Focus::Unknown, 1);
        }
        let seen = lines(d.path(), "abc");
        assert!(seen.len() <= KEEP_LINES);
        assert_eq!(seen.last().unwrap()["cause"], "m299");
    }

    #[test]
    fn an_id_that_cannot_be_a_file_name_writes_nothing() {
        let d = keep();
        let mut journal = Journal::new(d.path(), "../escape");
        journal.write(Decision::Rest, "x", None, Focus::Unknown, 1);
        assert!(path(d.path(), "../escape").is_none());
        assert_eq!(
            std::fs::read_dir(d.path().join("panes")).unwrap().count(),
            0
        );
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn journals_of_long_dead_sessions_are_removed_and_others_stay() {
        let d = keep();
        let old = path(d.path(), "old").unwrap();
        let new = path(d.path(), "new").unwrap();
        let lock = d.path().join("panes").join("old.1.lock");
        for p in [&old, &new, &lock] {
            std::fs::write(p, "{}\n").unwrap();
        }
        let long_ago = SystemTime::now() - Duration::from_secs(20 * 86_400);
        for p in [&old, &lock] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        prune(d.path());
        assert!(!old.exists());
        assert!(new.exists());
        assert!(lock.exists(), "only journals are touched");
    }

    /// Run `f` where it may hang: false if it did not finish in time.
    fn finishes_within(secs: u64, f: impl FnOnce() + Send + 'static) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(secs)).is_ok()
    }
    fn mkfifo(path: &Path) {
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    }

    #[test]
    fn a_pipe_where_the_journal_goes_does_not_hold_a_pane_back() {
        use std::os::unix::fs::FileTypeExt;
        let d = keep();
        let journal = path(d.path(), "abc").unwrap();
        mkfifo(&journal);
        let keep_dir = d.path().to_path_buf();
        assert!(
            finishes_within(3, move || {
                let mut j = Journal::new(&keep_dir, "abc");
                j.write(Decision::Wake, "x", None, Focus::Unknown, 1);
                j.note(Instant::now(), Decision::Ignore, "y", None, Focus::Unknown);
            }),
            "a write to a journal that is a pipe with no reader hung"
        );
        assert!(fs::symlink_metadata(&journal)
            .unwrap()
            .file_type()
            .is_fifo());
        // The lock being a pipe stops nothing either.
        let d = keep();
        mkfifo(&lock_path(d.path(), "abc").unwrap());
        let keep_dir = d.path().to_path_buf();
        assert!(finishes_within(3, move || {
            Journal::new(&keep_dir, "abc").write(Decision::Wake, "x", None, Focus::Unknown, 1);
        }));
        assert!(lines(d.path(), "abc").is_empty());
    }
    #[test]
    fn a_link_or_a_folder_where_the_journal_goes_is_left_alone() {
        let d = keep();
        let target = d.path().join("target.txt");
        fs::write(&target, "mine\n").unwrap();
        let journal = path(d.path(), "abc").unwrap();
        std::os::unix::fs::symlink(&target, &journal).unwrap();
        Journal::new(d.path(), "abc").write(Decision::Wake, "x", None, Focus::Unknown, 1);
        assert_eq!(fs::read_to_string(&target).unwrap(), "mine\n");
        assert!(fs::symlink_metadata(&journal)
            .unwrap()
            .file_type()
            .is_symlink());
        let d = keep();
        fs::create_dir(path(d.path(), "abc").unwrap()).unwrap();
        Journal::new(d.path(), "abc").write(Decision::Wake, "x", None, Focus::Unknown, 1);
        assert!(path(d.path(), "abc").unwrap().is_dir());
    }
    #[test]
    fn a_journal_that_is_cut_waits_for_nobody_and_loses_nothing_to_another_pane() {
        let d = keep();
        let lock = lock_path(d.path(), "abc").unwrap();
        let mut j = Journal::new(d.path(), "abc");
        j.write(Decision::Wake, "first", None, Focus::Unknown, 1);
        // Another pane is in the middle of a cut: it holds the lock.
        let held = fs::OpenOptions::new().write(true).open(&lock).unwrap();
        assert_eq!(
            unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let keep_dir = d.path().to_path_buf();
        let started = Instant::now();
        assert!(finishes_within(3, move || {
            Journal::new(&keep_dir, "abc").write(Decision::Wake, "second", None, Focus::Unknown, 1);
        }));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "gives up, never waits long"
        );
        let seen = lines(d.path(), "abc");
        assert_eq!(seen.len(), 1, "no line is written under another pane's cut");
        drop(held);
        crate::maintenance::wait_flock_free_for_test(&lock);
        j.write(Decision::Wake, "third", None, Focus::Unknown, 1);
        assert_eq!(lines(d.path(), "abc").len(), 2);
    }
    #[test]
    fn panes_of_one_session_writing_together_leave_no_gap_in_any_ones_lines() {
        let d = keep();
        let mut workers = vec![];
        for who in 0..3 {
            let keep_dir = d.path().to_path_buf();
            workers.push(std::thread::spawn(move || {
                let mut j = Journal::new(&keep_dir, "abc");
                for n in 0..150 {
                    while !j.write(
                        Decision::Wake,
                        &format!("{who}:{n}"),
                        None,
                        Focus::Unknown,
                        1,
                    ) {}
                }
            }));
        }
        for w in workers {
            w.join().unwrap();
        }
        let seen = lines(d.path(), "abc");
        assert!(seen.len() <= KEEP_LINES);
        for who in 0..3 {
            let mine: Vec<u32> = seen
                .iter()
                .filter_map(|l| {
                    l["cause"]
                        .as_str()?
                        .strip_prefix(&format!("{who}:"))?
                        .parse()
                        .ok()
                })
                .collect();
            assert!(
                mine.windows(2).all(|w| w[1] == w[0] + 1),
                "a line of pane {who} was lost: {mine:?}"
            );
        }
    }
}
