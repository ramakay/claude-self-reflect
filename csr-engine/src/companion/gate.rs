//! Gate 1 of the pane's idle cost: whether a frame has anything new to read.
//!
//! A frame reads files, the database and the clock, then draws. When none of them moved since
//! the last frame, and nothing the last frame drew is due to change, the watch loop skips the
//! frame altogether. What it checks, cheaply and at most once a second:
//!
//! - a [`Snapshot`] of the files the frame reads: device, inode, length and modification time
//!   to the nanosecond, a folder as a folder so a member that comes or goes shows, and this
//!   session's own file inside each. The pane's heartbeat files are not in it: they are touched
//!   every five seconds by the pane itself.
//! - SQLite's `data_version` on the pane's retained connection, which moves when another
//!   connection commits, WAL commits included, with the main file untouched.
//! - work still in flight, the draw's own next deadline, and a floor so nothing goes unread for
//!   long.
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
/// The database is busy all day with other sessions' imports. A change there redraws no sooner
/// than this after the last read.
pub const DATABASE: f64 = 5.;

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
    /// What the frame for `sid` reads: the keep folder (but not `panes/`, the heartbeats, nor the
    /// relay's own store), each of its folders and `<folder>/<sid>.json`, the transcript, the
    /// settings and the daemon's lock, and which database file this is.
    pub fn take(paths: &Paths, sid: &str, transcript: Option<&Path>) -> Self {
        let mut out = vec![];
        let mut put = |path: PathBuf| {
            let stamp = Stamp::of(&path);
            out.push((path, stamp));
        };
        put(paths.keep.clone());
        let mut members: Vec<PathBuf> = fs::read_dir(&paths.keep)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        members.sort();
        for path in members {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name == "panes" || name.starts_with("companion.db") {
                continue;
            }
            let folder = path.is_dir();
            put(path.clone());
            if folder && valid_id(sid) {
                put(path.join(format!("{sid}.json")));
            }
        }
        if let Some(path) = transcript {
            put(path.to_path_buf());
        }
        put(paths.home.join(".claude/settings.json"));
        put(paths.home.join(".claude-self-reflect/daemon.lock"));
        let db = Stamp::identity(&paths.db);
        out.push((paths.db.clone(), db));
        Self(out)
    }
}

/// What the last frame saw, and what it left running.
pub struct Seen {
    snapshot: Snapshot,
    version: Option<i64>,
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
    pub fn saw(&mut self, snapshot: Snapshot, version: Option<i64>, at: f64, settled: bool) {
        self.seen = Some(Seen {
            snapshot,
            version,
            at,
            settled,
        });
    }
    /// Whether a frame at `now` would read anything the last one did not. `deadline` is when the
    /// last frame's drawing next changes by itself. `look` takes the snapshot and the database
    /// version; it runs at most once a `PROBE`.
    pub fn due(
        &mut self,
        now: f64,
        deadline: Option<f64>,
        look: impl FnOnce() -> (Snapshot, Option<i64>),
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
        let (snapshot, version) = look();
        snapshot != seen.snapshot || (version != seen.version && now - seen.at >= DATABASE)
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
    fn append_truncate_replace_delete_and_a_new_member_each_change_it() {
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
        fs::write(p.keep.join("agents/other.json"), "{}").unwrap();
        moved("a new member of a folder", &mut last);
        fs::create_dir_all(p.keep.join("usage")).unwrap();
        moved("a new folder", &mut last);
    }

    #[test]
    fn a_gate_looks_once_a_second_and_the_database_waits_its_turn() {
        let (_d, p) = paths();
        let mut gate = Gate::default();
        assert!(gate.due(100., None, || unreachable!()), "nothing seen yet");
        gate.saw(take(&p), Some(1), 100., true);
        let looks = std::cell::Cell::new(0);
        let look = |v: i64| {
            looks.set(looks.get() + 1);
            (take(&p), Some(v))
        };
        assert!(!gate.due(101., None, || look(1)));
        assert!(!gate.due(101.5, None, || unreachable!()), "once a second");
        // Another connection committed, but the read was only 2 s ago.
        assert!(!gate.due(102., None, || look(2)));
        assert!(gate.due(105., None, || look(2)));
        assert_eq!(looks.get(), 3);
        // The deadline, the floor and work in flight each open it without looking.
        gate.saw(take(&p), Some(2), 200., true);
        assert!(!gate.due(210., Some(220.), || look(2)));
        assert!(gate.due(220., Some(220.), || unreachable!()));
        assert!(gate.due(230., None, || unreachable!()));
        gate.saw(take(&p), Some(2), 300., false);
        assert!(gate.due(300.1, None, || unreachable!()));
    }
}
