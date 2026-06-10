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
    let (entries, scanned) = parser::read_log(&log_path, window.cutoff())?;
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
