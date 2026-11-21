//! Gate 1 of the pane's idle cost: whether a frame has anything new to read.
//!
//! A frame reads files, the database and the clock, then draws. When none of them moved since
//! the last frame, and nothing the last frame drew is due to change, the watch loop skips the
//! frame altogether. What it checks, cheaply and at most once a second:
//!
//! - a [`Snapshot`] of the files this pane's own session writes: device, inode, length and
//!   modification time to the nanosecond, its file inside each folder of the keep folder, its
//!   transcript, the settings and the daemon's lock. The files every session shares (dreams, usage,
//!   spend) are not in it, nor are the pane's heartbeat files: any session's hook moves the
//!   first, and the pane itself touches the second every five seconds. What they hold shows at
//!   the floor.
//! - work still in flight, the draw's own next deadline, and a floor so nothing goes unread for
//!   long.
//!
//! The database is not watched. It commits every few seconds all day, mostly other sessions'
//! imports, and a frame for each commit kept an idle pane drawing about every 8 s. What the
//! session's own rows show changes with its own transcript and files (both in the snapshot); a
//! commit that reaches only the database shows at the floor.
use super::paths::{valid_id, Paths};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// The longest a pane goes without reading anything, however quiet the files are: liveness
/// checks (the daemon's pid, herdr's layout) and anything that ages out of its source live here.
pub const FLOOR: f64 = 30.;
/// How often the cheap checks run.
pub const PROBE: f64 = 1.;

/// A file's or folder's identity and state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: i128,
}
impl Stamp {
    pub fn of(path: &Path) -> Option<Self> {
        let m = fs::metadata(path).ok()?;
        Some(Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
        })
    }
    /// Only which file this is: a database is rewritten all day, but replacing it is news.
    fn identity(path: &Path) -> Option<Self> {
        Self::of(path).map(|s| Self {
            len: 0,
            mtime_ns: 0,
            ..s
        })
    }
}

/// The files a frame reads, as they were.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snapshot(Vec<(PathBuf, Option<Stamp>)>);
impl Snapshot {
    /// What changes the frame for `sid`, and only that: `<folder>/<sid>.json` (and the drafts
    /// beside it) in each folder of the keep folder, the transcript, the settings and the
    /// daemon's lock, and which database file this is. Not the keep folder or its folders as
    /// folders, and not the files every
    /// session shares (`dreams.jsonl`, `spend.json`, the usage log and the rest): another
    /// session's hook moves those, and it would redraw every pane about every two seconds while
    /// any session works. A change in one of those, or a commit in the database, shows within
    /// `FLOOR`, or sooner through the relay's wake. A file that is not there is not listed, so a folder that
    /// comes or goes shows only through this session's own file in it.
    pub fn take(paths: &Paths, sid: &str, transcript: Option<&Path>) -> Self {
        let mut out = vec![];
        let entry = |path: PathBuf| {
            let stamp = Stamp::of(&path);
            (path, stamp)
        };
        if valid_id(sid) {
            let mut folders: Vec<PathBuf> = fs::read_dir(&paths.keep)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    let name = e.file_name();
                    name != "panes" && !name.to_string_lossy().starts_with("companion.db")
                })
                .map(|e| e.path())
                .collect();
            folders.sort();
            for folder in folders {
                // `handoffs` keeps the session's drafts beside its state.
                for name in [format!("{sid}.json"), format!("{sid}.drafts.jsonl")] {
                    let own = entry(folder.join(name));
                    if own.1.is_some() {
                        out.push(own);
                    }
                }
            }
        }
        if let Some(path) = transcript {
            out.push(entry(path.to_path_buf()));
        }
        out.push(entry(paths.home.join(".claude/settings.json")));
        out.push(entry(paths.home.join(".claude-self-reflect/daemon.lock")));
        let db = Stamp::identity(&paths.db);
        out.push((paths.db.clone(), db));
        Self(out)
    }
    /// Also a number that stands for something outside the files above (the rollouts of another
    /// tool, say): the snapshot differs when it does.
    pub fn including(mut self, what: &str, fingerprint: u64) -> Self {
        self.0.retain(|(path, _)| path != Path::new(what));
        self.0.push((
            PathBuf::from(what),
            Some(Stamp {
                dev: fingerprint,
                ino: 0,
                len: 0,
                mtime_ns: 0,
            }),
        ));
        self
    }
}

/// What the last frame saw, and what it left running.
pub struct Seen {
    snapshot: Snapshot,
    at: f64,
    /// The frame was a plain main-page frame with nothing in flight: only then may a later one
    /// be skipped.
    settled: bool,
}
#[derive(Default)]
pub struct Gate {
    seen: Option<Seen>,
    probed: f64,
}
impl Gate {
    pub fn saw(&mut self, snapshot: Snapshot, at: f64, settled: bool) {
        self.seen = Some(Seen {
            snapshot,
            at,
            settled,
        });
    }
    /// Whether a frame at `now` would read anything the last one did not. `deadline` is when the
    /// last frame's drawing next changes by itself. `look` takes the snapshot; it runs at most
    /// once a `PROBE`.
    pub fn due(
        &mut self,
        now: f64,
        deadline: Option<f64>,
        look: impl FnOnce() -> Snapshot,
    ) -> bool {
        let Some(seen) = &self.seen else {
            return true;
        };
        if !seen.settled
            || deadline.is_some_and(|t| now >= t)
            || !(0. ..FLOOR).contains(&(now - seen.at))
        {
            return true;
        }
        if (0. ..PROBE).contains(&(now - self.probed)) {
            return false;
        }
        self.probed = now;
        look() != seen.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        fs::create_dir_all(p.keep.join("agents")).unwrap();
        fs::create_dir_all(p.keep.join("panes")).unwrap();
        (d, p)
    }
    fn take(p: &Paths) -> Snapshot {
        Snapshot::take(p, "s", None)
    }

    #[test]
    fn pane_heartbeats_and_the_relays_store_are_not_inputs() {
        let (_d, p) = paths();
        fs::write(p.keep.join("panes/s.json"), "{}").unwrap();
        fs::write(p.keep.join("companion.db-wal"), "x").unwrap();
        let before = take(&p);
        // Written in place, as a heartbeat and a store are.
        fs::write(p.keep.join("panes/s.json"), "{\"pid\":1}").unwrap();
        fs::write(p.keep.join("panes/other.json"), "{}").unwrap();
        fs::write(p.keep.join("companion.db-wal"), "xyz").unwrap();
        assert_eq!(take(&p), before);
    }

    #[test]
    fn append_truncate_replace_delete_and_recreate_of_the_sessions_own_file_each_change_it() {
        let (_d, p) = paths();
        let file = p.keep.join("agents/s.json");
        fs::write(&file, "{}").unwrap();
        let mut last = take(&p);
        let moved = |what: &str, last: &mut Snapshot| {
            let now = take(&p);
            assert_ne!(&now, last, "{what}");
            *last = now;
        };
        fs::write(&file, "{\"a\":1}").unwrap();
        moved("append", &mut last);
        fs::write(&file, "{}").unwrap();
        moved("truncate", &mut last);
        let aside = p.keep.join("agents/s.json.tmp");
        fs::write(&aside, "{}").unwrap();
        fs::rename(&aside, &file).unwrap();
        moved("atomic replace", &mut last);
        fs::remove_file(&file).unwrap();
        moved("delete", &mut last);
        fs::write(&file, "{}").unwrap();
        moved("recreate", &mut last);
        // The same file in a folder that comes later is this session's too.
        fs::create_dir_all(p.keep.join("usage")).unwrap();
        fs::write(p.keep.join("usage/s.json"), "{}").unwrap();
        moved("the session's file in a new folder", &mut last);
        fs::create_dir_all(p.keep.join("handoffs")).unwrap();
        fs::write(p.keep.join("handoffs/s.drafts.jsonl"), "{}\n").unwrap();
        moved("the session's drafts", &mut last);
        fs::write(p.keep.join("handoffs/other.drafts.jsonl"), "{}\n").unwrap();
        assert_eq!(take(&p), last, "another session's drafts");
    }

    #[test]
    fn another_sessions_files_and_the_shared_files_wake_no_other_pane() {
        let (_d, p) = paths();
        fs::create_dir_all(p.keep.join("usage")).unwrap();
        fs::write(p.keep.join("agents/s.json"), "{}").unwrap();
        fs::write(p.keep.join("dreams.jsonl"), "{}\n").unwrap();
        fs::write(p.keep.join("spend.json"), "{}").unwrap();
        let before = take(&p);
        // Session B's files, the folders' own modification times, and the fleet's shared files.
        fs::write(p.keep.join("agents/other.json"), "{}").unwrap();
        fs::write(p.keep.join("usage/other.json"), "{}").unwrap();
        fs::write(p.keep.join("usage-history.jsonl"), "{}\n").unwrap();
        fs::write(p.keep.join("dreams.jsonl"), "{}\n{}\n").unwrap();
        fs::write(p.keep.join("spend.json"), "{\"week\":1}").unwrap();
        fs::write(p.keep.join("likes.jsonl"), "{}\n").unwrap();
        fs::create_dir_all(p.keep.join("dreamer-lease")).unwrap();
        fs::write(p.keep.join("dreamer-lease/other.json"), "{}").unwrap();
        fs::remove_file(p.keep.join("agents/other.json")).unwrap();
        assert_eq!(take(&p), before);
    }

    #[test]
    fn the_sessions_transcript_and_the_settings_and_the_lock_each_change_it() {
        let (d, p) = paths();
        let transcript = d.path().join("s.jsonl");
        fs::write(&transcript, "{}\n").unwrap();
        fs::create_dir_all(p.home.join(".claude")).unwrap();
        fs::create_dir_all(p.home.join(".claude-self-reflect")).unwrap();
        let at = |t: Option<&Path>| Snapshot::take(&p, "s", t);
        let before = at(Some(&transcript));
        assert_eq!(at(Some(&transcript)), before);
        fs::write(&transcript, "{}\n{}\n").unwrap();
        let after = at(Some(&transcript));
        assert_ne!(after, before, "an append to the transcript");
        fs::write(p.home.join(".claude/settings.json"), "{}").unwrap();
        let settings = at(Some(&transcript));
        assert_ne!(settings, after, "the settings");
        fs::write(p.home.join(".claude-self-reflect/daemon.lock"), "1").unwrap();
        assert_ne!(at(Some(&transcript)), settings, "the daemon's lock");
    }

    #[test]
    fn a_gate_looks_once_a_second_and_opens_for_a_changed_file() {
        let (_d, p) = paths();
        let mut gate = Gate::default();
        assert!(gate.due(100., None, || unreachable!()), "nothing seen yet");
        gate.saw(take(&p), 100., true);
        let looks = std::cell::Cell::new(0);
        let look = || {
            looks.set(looks.get() + 1);
            take(&p)
        };
        assert!(!gate.due(101., None, look));
        assert!(!gate.due(101.5, None, || unreachable!()), "once a second");
        assert!(!gate.due(102., None, look));
        // The session's own file moved: the next look sees it.
        fs::write(p.keep.join("agents/s.json"), "{}").unwrap();
        assert!(gate.due(103., None, look));
        assert_eq!(looks.get(), 3);
        // The deadline, the floor and work in flight each open it without looking.
        gate.saw(take(&p), 200., true);
        assert!(!gate.due(210., Some(220.), look));
        assert!(gate.due(220., Some(220.), || unreachable!()));
        assert!(gate.due(230., None, || unreachable!()));
        gate.saw(take(&p), 300., false);
        assert!(gate.due(300.1, None, || unreachable!()));
    }
}
