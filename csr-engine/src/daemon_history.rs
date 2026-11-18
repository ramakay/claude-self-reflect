//! The daemon's own start and stop history: `~/.claude-self-reflect/daemon-history.jsonl`.
//!
//! launchd counts runs, but `launchctl print` is not a stable interface, and a SIGKILL gives the
//! victim no chance to log. So the daemon keeps its own record, one JSON object per line:
//!
//! - `start`: written once the daemon holds the daemon lock (a second daemon that loses the
//!   lock writes nothing). Its `reason` says how the run before it ended, as far as this file
//!   knows: the previous run's stop reason, or `unknown` when that run left no stop line.
//! - `stop`: a stop signal arrived (`SIGTERM`, `SIGINT`) or the binary on disk was replaced.
//! - `exit`: the graceful path finished, with how long it took.
//!
//! A run with a `stop` but no `exit` was ended during its graceful path (launchd's exit timeout
//! sends SIGKILL). A run with neither ended without an exit line: SIGKILL, a crash, a power cut.
//!
//! Only the daemon writes the file; `status` only reads it. It is bounded: past [`MAX_LINES`]
//! lines it is rewritten to its newest [`KEEP_LINES`]. Every write is best-effort and never
//! stops the daemon.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// The history file's name, in the data dir beside `daemon.lock`.
pub const FILE_NAME: &str = "daemon-history.jsonl";
/// Past this many lines the file is trimmed.
pub const MAX_LINES: usize = 400;
/// What a trim keeps: the newest lines.
pub const KEEP_LINES: usize = 200;
/// The `reason` of a start whose previous run left no stop line.
pub const UNKNOWN: &str = "unknown";

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Start,
    Stop,
    Exit,
}

/// One line of the history file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub event: EventKind,
    /// RFC 3339, UTC, second precision.
    pub at: String,
    pub pid: u32,
    /// The engine version (starts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// start: how the previous run ended. stop and exit: why this run stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// exit: how long the graceful path took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_ms: Option<u64>,
}

impl Event {
    fn at(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.at)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }
}

/// `<home>/.claude-self-reflect/daemon-history.jsonl`, the path the daemon writes and
/// `status` reads (home-based, like `daemon.lock`, whatever `--db-path` says).
pub fn path_in(home: &Path) -> PathBuf {
    home.join(".claude-self-reflect").join(FILE_NAME)
}

/// [`path_in`] for this user's home.
pub fn default_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| path_in(&home))
}

/// Every line of `path` that parses; a missing file is `None`, so a reader can tell "no
/// history yet" (an older binary) from "no starts".
pub fn read(path: &Path) -> Option<Vec<Event>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
    )
}

/// How the newest run in `events` ended, as its next start records it.
pub fn previous_run_end(events: &[Event]) -> String {
    let Some(start) = events.iter().rposition(|e| e.event == EventKind::Start) else {
        return format!("{UNKNOWN} (no earlier start recorded)");
    };
    let pid = events[start].pid;
    let after = || events[start + 1..].iter().filter(move |e| e.pid == pid);
    if let Some(exit) = after().rfind(|e| e.event == EventKind::Exit) {
        let reason = exit.reason.as_deref().unwrap_or(UNKNOWN);
        return match exit.shutdown_ms {
            Some(ms) => format!("{reason}, graceful shutdown finished in {ms} ms"),
            None => format!("{reason}, graceful shutdown finished"),
        };
    }
    if let Some(stop) = after().rfind(|e| e.event == EventKind::Stop) {
        let reason = stop.reason.as_deref().unwrap_or(UNKNOWN);
        return format!("{reason}, then ended before its graceful shutdown finished");
    }
    format!("{UNKNOWN} (pid {pid} ended without an exit line)")
}

fn stamp(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Append `event` to `path`, then trim the file when it is over [`MAX_LINES`].
pub fn append(path: &Path, event: &Event) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")?;
    drop(file);
    trim(path)
}

fn trim(path: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_LINES {
        return Ok(());
    }
    let kept = lines[lines.len() - KEEP_LINES..].join("\n") + "\n";
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, kept)?;
    std::fs::rename(&tmp, path)
}

/// Record a start of `pid` and return how the previous run ended.
pub fn record_start(path: &Path, pid: u32, version: &str, now: DateTime<Utc>) -> String {
    let previous = previous_run_end(&read(path).unwrap_or_default());
    let _ = append(
        path,
        &Event {
            event: EventKind::Start,
            at: stamp(now),
            pid,
            version: Some(version.to_string()),
            reason: Some(previous.clone()),
            shutdown_ms: None,
        },
    );
    previous
}

/// Record that `pid` is stopping, and why.
pub fn record_stop(path: &Path, pid: u32, reason: &str, now: DateTime<Utc>) {
    let _ = append(
        path,
        &Event {
            event: EventKind::Stop,
            at: stamp(now),
            pid,
            version: None,
            reason: Some(reason.to_string()),
            shutdown_ms: None,
        },
    );
}

/// Record that `pid` finished its graceful path in `shutdown_ms`.
pub fn record_exit(path: &Path, pid: u32, reason: &str, shutdown_ms: u64, now: DateTime<Utc>) {
    let _ = append(
        path,
        &Event {
            event: EventKind::Exit,
            at: stamp(now),
            pid,
            version: None,
            reason: Some(reason.to_string()),
            shutdown_ms: Some(shutdown_ms),
        },
    );
}

/// The daemon's start at the top of its run: records it in this user's history file and
/// prints one stamped line to the daemon log saying how the previous run ended.
pub fn record_start_now() {
    let Some(path) = default_path() else {
        return;
    };
    let pid = std::process::id();
    let previous = record_start(&path, pid, env!("CARGO_PKG_VERSION"), Utc::now());
    eprintln!(
        "{}",
        crate::telemetry::stamp_timing_line(&format!(
            "CSR daemon start: pid {pid}, version {}; previous run: {previous}",
            env!("CARGO_PKG_VERSION")
        ))
    );
}

/// Start counts and the last start, as `status` reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StartHistory {
    pub starts_last_10m: u32,
    pub starts_last_1h: u32,
    pub starts_last_24h: u32,
    /// The newest recorded start.
    pub last_start_at: Option<String>,
    /// How the run before the newest start ended (that start's `reason`).
    pub previous_run_end: Option<String>,
}

/// Summarize `events` as of `now`.
pub fn summarize(events: &[Event], now: DateTime<Utc>) -> StartHistory {
    let starts: Vec<&Event> = events
        .iter()
        .filter(|e| e.event == EventKind::Start)
        .collect();
    let within = |window: Duration| -> u32 {
        starts
            .iter()
            .filter_map(|e| e.at())
            .filter(|at| *at <= now && now - *at <= window)
            .count() as u32
    };
    let newest = starts.last();
    StartHistory {
        starts_last_10m: within(Duration::minutes(10)),
        starts_last_1h: within(Duration::hours(1)),
        starts_last_24h: within(Duration::hours(24)),
        last_start_at: newest.map(|e| e.at.clone()),
        previous_run_end: newest.and_then(|e| e.reason.clone()),
    }
}

/// [`summarize`] for the history under `home`; `None` when no history file exists yet.
pub fn summary_in(home: &Path, now: DateTime<Utc>) -> Option<StartHistory> {
    read(&path_in(home)).map(|events| summarize(&events, now))
}

#[cfg(test)]
#[path = "daemon_history_tests.rs"]
mod tests;
