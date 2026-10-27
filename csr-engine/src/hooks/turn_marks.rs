//! Per-session turn marks: when a session last submitted a prompt, last
//! stopped, and last had a Stop vetoed.
//!
//! One small file per session under `<data_dir>/hook-state/`. A write is a
//! temp file renamed over the old one, so a reader sees the whole old file or
//! the whole new one. A mark never blocks and never fails its caller: every
//! error is swallowed, and a missing or unreadable file reads as "never".

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnMarks {
    pub last_prompt: Option<SystemTime>,
    pub last_stop: Option<SystemTime>,
    pub last_veto: Option<SystemTime>,
}

#[derive(Clone, Copy)]
enum Mark {
    Prompt,
    Stop,
    Veto,
}

pub fn mark_prompt(data_dir: &Path, session_id: &str) {
    write_mark(data_dir, session_id, Mark::Prompt);
}

pub fn mark_stop(data_dir: &Path, session_id: &str) {
    write_mark(data_dir, session_id, Mark::Stop);
}

pub fn mark_veto(data_dir: &Path, session_id: &str) {
    write_mark(data_dir, session_id, Mark::Veto);
}

pub fn read(data_dir: &Path, session_id: &str) -> TurnMarks {
    let Some(path) = mark_path(data_dir, session_id) else {
        return TurnMarks::default();
    };
    std::fs::read_to_string(path)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

fn state_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("hook-state")
}

/// The file for a session. The name is a hash of the id, so no id can climb
/// out of the directory or collide through character filtering.
fn mark_path(data_dir: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() {
        return None;
    }
    let digest = blake3::hash(session_id.as_bytes()).to_hex();
    Some(state_dir(data_dir).join(format!("turn-{}.marks", &digest.as_str()[..32])))
}

fn millis(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn parse(text: &str) -> TurnMarks {
    let mut marks = TurnMarks::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Ok(ms) = value.trim().parse::<u64>() else {
            continue;
        };
        let time = UNIX_EPOCH + Duration::from_millis(ms);
        match key.trim() {
            "prompt" => marks.last_prompt = Some(time),
            "stop" => marks.last_stop = Some(time),
            "veto" => marks.last_veto = Some(time),
            _ => {}
        }
    }
    marks
}

fn render(marks: &TurnMarks) -> String {
    let mut out = String::new();
    for (key, time) in [
        ("prompt", marks.last_prompt),
        ("stop", marks.last_stop),
        ("veto", marks.last_veto),
    ] {
        if let Some(time) = time {
            out.push_str(&format!("{key}={}\n", millis(time)));
        }
    }
    out
}

fn write_mark(data_dir: &Path, session_id: &str, mark: Mark) {
    let _ = try_write_mark(data_dir, session_id, mark);
}

fn try_write_mark(data_dir: &Path, session_id: &str, mark: Mark) -> std::io::Result<()> {
    let path = mark_path(data_dir, session_id)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let dir = state_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let mut marks = read(data_dir, session_id);
    let now = Some(SystemTime::now());
    match mark {
        Mark::Prompt => marks.last_prompt = now,
        Mark::Stop => marks.last_stop = now,
        Mark::Veto => marks.last_veto = now,
    }
    // A name per writer: two hooks marking one session never share a temp file.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = dir.join(format!(
        ".turn-{}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&temp, render(&marks))?;
    std::fs::rename(&temp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_start_empty_and_each_kind_is_kept_separately() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), "s1"), TurnMarks::default());
        mark_prompt(dir.path(), "s1");
        let after_prompt = read(dir.path(), "s1");
        assert!(after_prompt.last_prompt.is_some());
        assert!(after_prompt.last_stop.is_none() && after_prompt.last_veto.is_none());
        mark_stop(dir.path(), "s1");
        mark_veto(dir.path(), "s1");
        let all = read(dir.path(), "s1");
        assert_eq!(all.last_prompt, after_prompt.last_prompt);
        assert!(all.last_stop.is_some() && all.last_veto.is_some());
        // Another session is untouched.
        assert_eq!(read(dir.path(), "s2"), TurnMarks::default());
    }

    #[test]
    fn marks_survive_odd_ids_and_unwritable_directories() {
        let dir = tempfile::tempdir().unwrap();
        mark_prompt(dir.path(), "../../escape");
        assert!(read(dir.path(), "../../escape").last_prompt.is_some());
        assert!(!dir.path().join("..").join("escape").exists());
        mark_prompt(dir.path(), "");
        assert_eq!(read(dir.path(), ""), TurnMarks::default());
        // A data dir that is a file: the mark is dropped, nothing panics.
        let file = dir.path().join("plain");
        std::fs::write(&file, "x").unwrap();
        mark_stop(&file, "s1");
        assert_eq!(read(&file, "s1"), TurnMarks::default());
        // A garbled file reads as never.
        let path = mark_path(dir.path(), "bad").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "prompt=nope\n\u{0}garbage").unwrap();
        assert_eq!(read(dir.path(), "bad"), TurnMarks::default());
    }
}
