//! What a frame read from a file, kept while the file stays as it was.
//!
//! An awake pane draws a frame every few seconds while the database is busy, and each frame used
//! to parse the same logs again: `dreams.jsonl` alone is megabytes. A parse kept here is served
//! again while the file's stamp (device, inode, length, modification time to the nanosecond) is
//! the one it was read at, and for no longer than [`KEEP_FOR`] (plus the gate's floor: see
//! there). An append, a truncate, a
//! replace or a delete changes the stamp, so the next frame reads the file again.
use super::gate::Stamp;
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

/// The longest a kept parse is served without reading the file again. Only a rewrite that leaves
/// the stamp as it was (same length and the same modification time) goes unseen, and for no
/// longer than this plus the gate's floor (30 s): the expiry is not a deadline, so the frame
/// that reads the file again is the next one the gate opens.
pub const KEEP_FOR: Duration = Duration::from_secs(300);

type Kept = (Option<Stamp>, Instant, Rc<dyn Any>);
thread_local! {
    static KEPT: RefCell<HashMap<(PathBuf, String), Kept>> = RefCell::new(HashMap::new());
    static PARSES: Cell<usize> = const { Cell::new(0) };
}

/// `parse(path)`, or what it gave the last time, while `path` is as it was then. `what` names
/// the parse: one file can be read for more than one thing.
pub fn parsed<T: 'static>(path: &Path, what: &str, parse: impl FnOnce(&Path) -> T) -> Rc<T> {
    let key = (path.to_path_buf(), what.to_owned());
    // Stamped before the read: a write during it shows as a change at the next look.
    let stamp = Stamp::of(path);
    let hit = KEPT.with_borrow(|kept| {
        kept.get(&key)
            .filter(|(was, at, _)| *was == stamp && at.elapsed() < KEEP_FOR)
            .and_then(|(_, _, value)| value.clone().downcast::<T>().ok())
    });
    if let Some(hit) = hit {
        return hit;
    }
    PARSES.set(PARSES.get() + 1);
    let value = Rc::new(parse(path));
    let any: Rc<dyn Any> = value.clone();
    KEPT.with_borrow_mut(|kept| {
        kept.insert(key, (stamp, Instant::now(), any));
    });
    value
}

/// How many files this thread parsed through [`parsed`]: what the idle tests count.
pub fn parses() -> usize {
    PARSES.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn count(path: &Path) -> Rc<usize> {
        parsed(path, "lines", |p| {
            fs::read_to_string(p).map_or(0, |text| text.lines().count())
        })
    }

    #[test]
    fn a_file_as_it_was_is_parsed_once_and_each_change_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("log.jsonl");
        assert_eq!(*count(&file), 0, "a missing file");
        let before = parses();
        assert_eq!(*count(&file), 0);
        assert_eq!(parses(), before, "a missing file stays missing: kept");
        fs::write(&file, "a\n").unwrap();
        assert_eq!(*count(&file), 1, "created");
        let before = parses();
        for _ in 0..5 {
            assert_eq!(*count(&file), 1);
        }
        assert_eq!(parses(), before, "five frames, no parse");
        fs::write(&file, "a\nb\n").unwrap();
        assert_eq!(*count(&file), 2, "appended");
        let aside = dir.path().join("aside");
        fs::write(&aside, "x\ny\nz\n").unwrap();
        fs::rename(&aside, &file).unwrap();
        assert_eq!(*count(&file), 3, "replaced");
        fs::remove_file(&file).unwrap();
        assert_eq!(*count(&file), 0, "deleted");
    }

    #[test]
    fn two_parses_of_one_file_are_kept_apart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("log.jsonl");
        fs::write(&file, "a\nb\n").unwrap();
        let lines = parsed(&file, "lines", |p| {
            fs::read_to_string(p).unwrap().lines().count()
        });
        let bytes = parsed(&file, "bytes", |p| fs::read(p).unwrap().len());
        assert_eq!((*lines, *bytes), (2, 4));
    }
}
