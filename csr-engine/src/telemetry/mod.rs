//! Telemetry: aggregates hook-timing.log + status into an ops view.
//!
//! `csr-engine telemetry`               — pretty text report (default window: 24h)
//! `csr-engine telemetry --json`        — machine-readable
//! `csr-engine telemetry --since 7d`    — extend window
//! `csr-engine telemetry --tui`         — live ratatui dashboard
//!
//! Source data: `~/.claude-self-reflect/hook-timing.log` (line-oriented) +
//! `StatusReport` (DB-backed).

pub mod aggregator;
pub mod parser;
pub mod render;
pub mod tui;

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::status::StatusReport;

pub use aggregator::{HookStats, StartupStats, TelemetryReport};

/// Window selector parsed from --since (e.g. "24h", "7d", "1h", "all").
#[derive(Debug, Clone, Copy)]
pub enum Window {
    Since(DateTime<Utc>),
    All,
}

impl Window {
    /// Parse strings like `"24h"`, `"7d"`, `"30m"`, `"all"`.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim().to_lowercase();
        if s == "all" {
            return Ok(Window::All);
        }
        let (num_part, unit) = s
            .find(|c: char| c.is_alphabetic())
            .map(|i| s.split_at(i))
            .ok_or_else(|| {
                anyhow::anyhow!("invalid --since '{}': expected '24h', '7d', etc.", s)
            })?;
        let n: i64 = num_part
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid number in --since '{}'", s))?;
        let dur = match unit {
            "m" | "min" | "mins" => Duration::minutes(n),
            "h" | "hr" | "hrs" => Duration::hours(n),
            "d" | "day" | "days" => Duration::days(n),
            _ => anyhow::bail!("unknown unit in --since '{}': use m, h, or d", s),
        };
        Ok(Window::Since(Utc::now() - dur))
    }

    pub fn cutoff(&self) -> Option<DateTime<Utc>> {
        match self {
            Window::Since(t) => Some(*t),
            Window::All => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Window::Since(t) => {
                let age = Utc::now() - *t;
                if age.num_days() >= 1 {
                    format!("last {}d", age.num_days())
                } else if age.num_hours() >= 1 {
                    format!("last {}h", age.num_hours())
                } else {
                    format!("last {}m", age.num_minutes().max(1))
                }
            }
            Window::All => "all-time".to_string(),
        }
    }
}

/// Combined telemetry view returned to renderers.
#[derive(Serialize)]
pub struct Telemetry {
    pub window: String,
    pub status: StatusReport,
    pub report: TelemetryReport,
    pub log_path: String,
    pub log_lines_scanned: usize,
    pub log_lines_in_window: usize,
}

/// Resolve the canonical log path: `~/.claude-self-reflect/hook-timing.log`.
pub fn default_log_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude-self-reflect").join("hook-timing.log"))
}

/// The previous log generation kept after rotation: `hook-timing.log.1`.
pub fn rotated_log_path() -> Option<PathBuf> {
    default_log_path().map(|p| p.with_extension("log.1"))
}

/// Rotate at 10MB — one generation is kept so telemetry windows survive rotation.
const MAX_TIMING_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Append one timestamped line to hook-timing.log, rotating first if oversized.
/// The single write path for all timing/diagnostic log producers. Best-effort:
/// concurrent rotation races lose a rename harmlessly, IO errors are swallowed
/// (logging must never fail a hook).
pub fn append_timing_line(line: &str) {
    let Some(log_path) = default_log_path() else {
        return;
    };
    if let (Ok(md), Some(old)) = (std::fs::metadata(&log_path), rotated_log_path()) {
        if md.len() >= MAX_TIMING_LOG_BYTES {
            let _ = std::fs::rename(&log_path, &old);
        }
    }
    let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let entry = format!("{} {}\n", ts, line);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()));
}

/// CLI entry point.
pub fn handle(
    db_path: &Path,
    projects_dir: &Path,
    since: Option<String>,
    json: bool,
    tui: bool,
) -> Result<()> {
    let window = match since {
        Some(s) => Window::parse(&s)?,
        None => Window::Since(Utc::now() - Duration::hours(24)),
    };

    if tui {
        return tui::run(db_path.to_path_buf(), projects_dir.to_path_buf(), window);
    }

    let telemetry = collect(db_path, projects_dir, window)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&telemetry)?);
    } else {
        render::text::print(&telemetry);
    }
    Ok(())
}

/// Gather a one-shot snapshot — used by both text/json render and the TUI's tick.
pub fn collect(db_path: &Path, projects_dir: &Path, window: Window) -> Result<Telemetry> {
    let status = crate::status::gather_status_public(db_path, projects_dir)?;
    let log_path = default_log_path().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    // Read the rotated generation first (older lines) so a rotation mid-window
    // doesn't truncate the report.
    let (mut entries, mut scanned) = match rotated_log_path().filter(|p| p.exists()) {
        Some(old) => parser::read_log(&old, window.cutoff())?,
        None => (Vec::new(), 0),
    };
    let (current, cur_scanned) = parser::read_log(&log_path, window.cutoff())?;
    entries.extend(current);
    scanned += cur_scanned;
    let report = aggregator::aggregate(&entries);

    Ok(Telemetry {
        window: window.label(),
        status,
        report,
        log_path: log_path.to_string_lossy().into_owned(),
        log_lines_scanned: scanned,
        log_lines_in_window: entries.len(),
    })
}
