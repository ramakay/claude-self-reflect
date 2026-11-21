//! Resume lineage: which session did `claude --resume` / `claude -c` continue?
//!
//! Claude Code starts a NEW session id on resume. The new transcript names no
//! predecessor; the OLD one gets a `{"type":"continued-in","sessionId":<old>,
//! "continuedInSessionId":<new>}` row near its end. This is a bounded,
//! read-only scan for that row. Every I/O or parse problem reads as `None`,
//! and the caller then keeps today's newest-by-timestamp order.
//!
//! The row may not be written yet when SessionStart fires; that case is the
//! same `None`, so nothing here may be relied on for correctness.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Only transcripts touched this recently can have just been continued.
pub(crate) const MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// Most sibling transcripts opened per scan (newest first).
pub(crate) const MAX_FILES: usize = 64;
/// Most bytes read from the end of each transcript.
pub(crate) const MAX_TAIL_BYTES: u64 = 256 * 1024;
/// Most directory entries looked at, so a huge project dir cannot stall startup.
const MAX_DIR_ENTRIES: usize = 8192;

/// The `sessionId` of the sibling transcript whose `continued-in` row names
/// `current_sid` as its successor. `None` on no match or any error.
pub(crate) fn resume_predecessor(transcript_dir: &Path, current_sid: &str) -> Option<String> {
    resume_predecessor_at(transcript_dir, current_sid, SystemTime::now())
}

fn resume_predecessor_at(
    transcript_dir: &Path,
    current_sid: &str,
    now: SystemTime,
) -> Option<String> {
    if current_sid.is_empty() {
        return None;
    }
    let mut candidates: Vec<(SystemTime, std::path::PathBuf)> = std::fs::read_dir(transcript_dir)
        .ok()?
        .take(MAX_DIR_ENTRIES)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                return None;
            }
            // The new session's own transcript cannot hold its own parent's row.
            if path.file_stem().and_then(|s| s.to_str()) == Some(current_sid) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta.modified().ok()?;
            // A future mtime (clock skew) reads as age zero.
            let age = now.duration_since(modified).unwrap_or_default();
            (age <= MAX_AGE).then_some((modified, path))
        })
        .collect();
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    candidates.truncate(MAX_FILES);
    candidates
        .iter()
        .find_map(|(_, path)| continued_in_target(path, current_sid))
}

/// A pane may be opened long after SessionStart. Keep its birth-time window
/// rather than the hook's fifteen-minute budget, using the same bounded reader.
pub(crate) fn pane_predecessor(mine: &Path, current_sid: &str) -> Option<String> {
    let since = mine
        .metadata()
        .ok()?
        .created()
        .ok()
        .and_then(|t| t.checked_sub(Duration::from_secs(120)));
    let mut files: Vec<_> = std::fs::read_dir(mine.parent()?)
        .ok()?
        .take(MAX_DIR_ENTRIES)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path == mine || path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().ok()?;
            if since.is_some_and(|t| modified < t) {
                return None;
            }
            Some((modified, path))
        })
        .collect();
    files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    files
        .into_iter()
        .take(200)
        .find_map(|(_, path)| continued_in_target_checked(&path, current_sid, true))
}

/// The `sessionId` of a `continued-in` row in the tail of `path` whose
/// `continuedInSessionId` is `current_sid`.
fn continued_in_target(path: &Path, current_sid: &str) -> Option<String> {
    continued_in_target_checked(path, current_sid, false)
}

fn continued_in_target_checked(path: &Path, current_sid: &str, owner: bool) -> Option<String> {
    let tail = read_tail(path)?;
    let text = String::from_utf8_lossy(&tail);
    text.lines()
        .rev()
        .filter(|line| line.contains("continued-in"))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .find_map(|row| {
            let is_row = row.get("type").and_then(|v| v.as_str()) == Some("continued-in");
            let names_us =
                row.get("continuedInSessionId").and_then(|v| v.as_str()) == Some(current_sid);
            let old = row.get("sessionId").and_then(|v| v.as_str())?;
            (is_row
                && names_us
                && !old.is_empty()
                && old != current_sid
                && (!owner
                    || (path.file_stem().and_then(|s| s.to_str()) == Some(old)
                        && old
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-'))))
            .then(|| old.to_string())
        })
}

/// The last `MAX_TAIL_BYTES` of the file, starting on a line boundary.
fn read_tail(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.take(MAX_TAIL_BYTES).read_to_end(&mut buf).ok()?;
    if start > 0 {
        // The first line is cut mid-way: drop it.
        let cut = buf
            .iter()
            .position(|&b| b == b'\n')
            .map_or(buf.len(), |i| i + 1);
        buf.drain(..cut);
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn row(old: &str, new: &str) -> String {
        format!(
            "{{\"type\":\"continued-in\",\"sessionId\":\"{old}\",\"continuedInSessionId\":\"{new}\"}}\n"
        )
    }

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        File::create(&path)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        path
    }

    #[test]
    fn matches_the_row_that_names_the_current_session() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "old-1.jsonl",
            &format!("{{\"type\":\"user\"}}\n{}", row("old-1", "new-1")),
        );
        write(dir.path(), "other.jsonl", &row("other", "someone-else"));
        assert_eq!(
            resume_predecessor(dir.path(), "new-1").as_deref(),
            Some("old-1")
        );
    }

    #[test]
    fn no_match_when_no_row_names_the_current_session() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "old-1.jsonl", &row("old-1", "new-1"));
        assert_eq!(resume_predecessor(dir.path(), "new-2"), None);
        assert_eq!(resume_predecessor(dir.path(), ""), None);
    }

    #[test]
    fn a_stale_transcript_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "old-1.jsonl", &row("old-1", "new-1"));
        let later = SystemTime::now() + MAX_AGE + Duration::from_secs(60);
        assert_eq!(resume_predecessor_at(dir.path(), "new-1", later), None);
        let within = SystemTime::now() + MAX_AGE - Duration::from_secs(60);
        assert_eq!(
            resume_predecessor_at(dir.path(), "new-1", within).as_deref(),
            Some("old-1")
        );
    }

    #[test]
    fn garbage_lines_and_wrong_shapes_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "not json continued-in\n\u{0}\u{1}binary\n{{\"type\":\"continued-in\"}}\n\
             {{\"type\":\"user\",\"text\":\"continued-in new-1\"}}\n\
             {{\"type\":\"continued-in\",\"sessionId\":\"\",\"continuedInSessionId\":\"new-1\"}}\n\
             {{\"type\":\"continued-in\",\"sessionId\":7,\"continuedInSessionId\":\"new-1\"}}\n{}",
            row("old-1", "new-1")
        );
        write(dir.path(), "old-1.jsonl", &body);
        assert_eq!(
            resume_predecessor(dir.path(), "new-1").as_deref(),
            Some("old-1")
        );
        let only_garbage = tempfile::tempdir().unwrap();
        std::fs::write(
            only_garbage.path().join("x.jsonl"),
            b"\xff\xfe continued-in\n",
        )
        .unwrap();
        assert_eq!(resume_predecessor(only_garbage.path(), "new-1"), None);
    }

    #[test]
    fn a_missing_dir_or_a_non_jsonl_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resume_predecessor(&dir.path().join("does-not-exist"), "new-1"),
            None
        );
        write(dir.path(), "old-1.txt", &row("old-1", "new-1"));
        assert_eq!(resume_predecessor(dir.path(), "new-1"), None);
    }

    #[test]
    fn only_the_last_256_kib_is_read() {
        let dir = tempfile::tempdir().unwrap();
        // Row first, then more than the tail bound of filler: out of reach.
        let filler = format!("{{\"type\":\"user\",\"x\":\"{}\"}}\n", "a".repeat(1024));
        let big = filler.repeat(300);
        write(
            dir.path(),
            "far.jsonl",
            &format!("{}{big}", row("far", "new-1")),
        );
        assert_eq!(resume_predecessor(dir.path(), "new-1"), None);
        // Row last: found even when the file is far larger than the bound.
        write(
            dir.path(),
            "near.jsonl",
            &format!("{big}{}", row("near", "new-1")),
        );
        assert_eq!(
            resume_predecessor(dir.path(), "new-1").as_deref(),
            Some("near")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_named_jsonl_is_never_followed_and_a_non_file_is_skipped() {
        let outside = tempfile::tempdir().unwrap();
        let target = write(outside.path(), "real.log", &row("old-1", "new-1"));
        let dir = tempfile::tempdir().unwrap();
        // Control: the same row in a regular file inside the dir is found.
        let control = tempfile::tempdir().unwrap();
        write(control.path(), "x.jsonl", &row("old-1", "new-1"));
        assert_eq!(
            resume_predecessor(control.path(), "new-1").as_deref(),
            Some("old-1")
        );
        // A symlink `x.jsonl` to a file outside the dir holding a matching row.
        std::os::unix::fs::symlink(&target, dir.path().join("x.jsonl")).unwrap();
        assert_eq!(resume_predecessor(dir.path(), "new-1"), None);
        // A directory named `*.jsonl` is not a file either.
        let dirs = tempfile::tempdir().unwrap();
        std::fs::create_dir(dirs.path().join("d.jsonl")).unwrap();
        assert_eq!(resume_predecessor(dirs.path(), "new-1"), None);
    }

    #[test]
    fn at_most_64_files_are_opened_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        // The matching file is the OLDEST of 65 recent ones: past the cap.
        let target = write(dir.path(), "target.jsonl", &row("target", "new-1"));
        File::options()
            .write(true)
            .open(&target)
            .unwrap()
            .set_modified(now - Duration::from_secs(600))
            .unwrap();
        for i in 0..MAX_FILES {
            let path = write(dir.path(), &format!("filler-{i}.jsonl"), "{}\n");
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(now - Duration::from_secs(i as u64))
                .unwrap();
        }
        assert_eq!(resume_predecessor_at(dir.path(), "new-1", now), None);
        std::fs::remove_file(dir.path().join("filler-0.jsonl")).unwrap();
        assert_eq!(
            resume_predecessor_at(dir.path(), "new-1", now).as_deref(),
            Some("target")
        );
    }
}

#[cfg(test)]
mod pane_tests {
    use super::*;
    #[test]
    fn the_pane_can_follow_a_resume_after_the_hooks_recent_window() {
        let d = tempfile::tempdir().unwrap();
        let mine = d.path().join("new.jsonl");
        std::fs::write(&mine, "{}\n").unwrap();
        std::fs::write(
            d.path().join("old.jsonl"),
            "{\"type\":\"continued-in\",\"sessionId\":\"old\",\"continuedInSessionId\":\"new\"}\n",
        )
        .unwrap();
        assert_eq!(
            resume_predecessor_at(
                d.path(),
                "new",
                SystemTime::now() + Duration::from_secs(3600)
            ),
            None
        );
        assert_eq!(pane_predecessor(&mine, "new").as_deref(), Some("old"));
    }
    #[test]
    fn a_continued_row_can_only_speak_for_its_own_file() {
        let d = tempfile::tempdir().unwrap();
        let mine = d.path().join("new.jsonl");
        std::fs::write(&mine, "{}\n").unwrap();
        std::fs::write(d.path().join("old.jsonl"),"{\"type\":\"continued-in\",\"sessionId\":\"forged\",\"continuedInSessionId\":\"new\"}\n").unwrap();
        assert_eq!(pane_predecessor(&mine, "new"), None);
    }
}
