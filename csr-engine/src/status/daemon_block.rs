//! The `daemon` block of `status`: is the daemon up, since when, and how often it has started.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::daemon_history::StartHistory;

/// Is the daemon (`csr-engine daemon`) running? Measured, not configured:
/// `up` is whether a process holds `~/.claude-self-reflect/daemon.lock` (an
/// flock probe, so a recycled pid cannot fake it). `agent_installed` says
/// whether launchd has a plist to start it from; a daemon that is down with no
/// agent is fixed by `csr-engine setup`.
#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct DaemonStatus {
    pub up: bool,
    pub pid: Option<u32>,
    pub agent_installed: bool,
    /// When the running daemon started (RFC 3339, from its run record
    /// `run/<pid>.json`); `null` while it is down or the record is missing.
    pub started_at: Option<String>,
    /// Seconds since `started_at`.
    pub uptime_secs: Option<i64>,
    /// The daemon's own start history (`daemon-history.jsonl`, see
    /// `crate::daemon_history`): starts in the last 10 minutes, 1 hour and
    /// 24 hours, and how the run before the newest start ended. `null` until a
    /// daemon that writes the file has started, and on the compact statusline,
    /// which never reads it.
    pub history: Option<StartHistory>,
}

/// The daemon's state under `home`. The lock path is home-based because the
/// daemon takes it there whatever `--db-path` it serves. `full` reads the run
/// record and the start history too; the compact statusline passes `false`.
pub(super) fn gather_daemon(home: Option<&Path>, full: bool) -> DaemonStatus {
    gather_daemon_at(home, full, Utc::now())
}

pub(super) fn gather_daemon_at(
    home: Option<&Path>,
    full: bool,
    now: DateTime<Utc>,
) -> DaemonStatus {
    let Some(home) = home else {
        return DaemonStatus::default();
    };
    let lock = home.join(".claude-self-reflect/daemon.lock");
    let up = crate::daemon::lock_file_held(&lock);
    let pid: Option<u32> = up
        .then(|| std::fs::read_to_string(&lock).ok())
        .flatten()
        .and_then(|text| text.trim().parse().ok());
    // Ours, or a hand-installed agent that runs the daemon under another label.
    let agent_installed = home
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", crate::setup::DAEMON_LABEL))
        .exists()
        || !crate::setup::daemon_agents_on_disk(home).is_empty();
    if !full {
        return DaemonStatus {
            up,
            pid,
            agent_installed,
            ..DaemonStatus::default()
        };
    }
    let started_at = pid.and_then(|pid| run_record_started_at(home, pid));
    let uptime_secs = started_at
        .as_deref()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| (now - at.with_timezone(&Utc)).num_seconds().max(0));
    DaemonStatus {
        up,
        pid,
        agent_installed,
        started_at,
        uptime_secs,
        history: crate::daemon_history::summary_in(home, now),
    }
}

/// `started_at` of the daemon run record `<home>/.claude-self-reflect/run/<pid>.json`.
fn run_record_started_at(home: &Path, pid: u32) -> Option<String> {
    let path = home
        .join(".claude-self-reflect/run")
        .join(format!("{pid}.json"));
    let record: crate::binary_identity::RunRecord =
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (record.role == "daemon" && record.pid == pid).then_some(record.started_at)
}

#[cfg(test)]
#[path = "daemon_block_tests.rs"]
mod tests;
