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

/// Explicit override for the timing log location. Must be an absolute path — a
/// relative one would resolve against whatever directory the hook happened to run
/// in, scattering the log — so relative values are ignored and the normal path is
/// used instead.
pub const TIMING_LOG_ENV: &str = "CSR_TIMING_LOG";

/// Resolve the canonical log path: `~/.claude-self-reflect/hook-timing.log`.
///
/// Checked in order: the `CSR_TIMING_LOG` override, a test-harness redirect, then
/// the real installation path.
pub fn default_log_path() -> Option<PathBuf> {
    resolve_log_path(
        std::env::var_os(TIMING_LOG_ENV),
        running_under_test_harness(),
        dirs::home_dir(),
    )
}

/// The decision behind [`default_log_path`], with every input passed in.
///
/// Pure on purpose: the alternative is tests that mutate `CSR_TIMING_LOG` in a
/// process-global environment while the rest of the suite reads it on other
/// threads.
fn resolve_log_path(
    override_var: Option<std::ffi::OsString>,
    under_test_harness: bool,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(raw) = override_var {
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            return Some(p);
        }
        // Empty or relative: a mis-set variable, not a request to log to "." —
        // fall through rather than write somewhere unpredictable.
    }
    if under_test_harness {
        // Per-process: `cargo test` runs the lib, hooks and integration harnesses
        // concurrently, and a shared file would let them rotate each other's log.
        return Some(
            std::env::temp_dir().join(format!("csr-test-hook-timing-{}.log", std::process::id())),
        );
    }
    home.map(|h| h.join(".claude-self-reflect").join("hook-timing.log"))
}

/// True when this process is a `cargo test` / `cargo bench` binary.
///
/// Without this, `cargo test` appends to the developer's live hook-timing.log —
/// the same log they debug hooks with, which is how a test run can invent
/// evidence for the bug being investigated.
///
/// Two signals, both required: Cargo exports `CARGO_MANIFEST_DIR` into the
/// processes it launches, and it runs test/bench harnesses out of
/// `target/<profile>/deps/`. An installed `csr-engine` has neither; `cargo run`
/// has the first but not the second, so a hook run by hand still writes where the
/// developer expects.
fn running_under_test_harness() -> bool {
    if std::env::var_os("CARGO_MANIFEST_DIR").is_none() {
        return false;
    }
    std::env::args_os().next().is_some_and(|arg0| {
        Path::new(&arg0)
            .parent()
            .and_then(|d| d.file_name())
            .is_some_and(|d| d == std::ffi::OsStr::new("deps"))
    })
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

#[cfg(test)]
mod log_path_tests {
    use super::*;

    /// The bug this guards: `cargo test` used to append to
    /// `~/.claude-self-reflect/hook-timing.log` — the developer's live log —
    /// because the path resolved straight through `home_dir()`.
    #[test]
    fn test_runs_never_resolve_to_the_live_log() {
        let path = default_log_path().expect("a log path is always resolvable under test");
        let live = dirs::home_dir().map(|h| h.join(".claude-self-reflect").join("hook-timing.log"));
        assert_ne!(
            Some(path.clone()),
            live,
            "a test run must not write to the installed log"
        );
        assert!(
            running_under_test_harness(),
            "the harness signal is what redirects it; path was {}",
            path.display()
        );
    }

    fn home() -> Option<PathBuf> {
        Some(PathBuf::from("/home/dev"))
    }

    fn live() -> Option<PathBuf> {
        Some(PathBuf::from(
            "/home/dev/.claude-self-reflect/hook-timing.log",
        ))
    }

    /// Every input is an argument, so none of these mutate the process
    /// environment — the suite runs in parallel and `set_var` is process-global.
    #[test]
    fn an_absolute_override_wins() {
        let want = PathBuf::from("/var/log/csr/hook-timing.log");
        assert_eq!(
            resolve_log_path(Some(want.clone().into()), false, home()),
            Some(want.clone())
        );
        // Even under the test harness, and even with no home directory at all.
        assert_eq!(
            resolve_log_path(Some(want.clone().into()), true, None),
            Some(want)
        );
    }

    /// Empty or relative is a mis-set variable, not a request to log to "." — it
    /// would follow the hook's working directory and scatter the log.
    #[test]
    fn empty_and_relative_overrides_fall_through() {
        for bad in ["", "hook-timing.log", "./logs/hook-timing.log", ".."] {
            assert_eq!(
                resolve_log_path(Some(bad.into()), false, home()),
                live(),
                "override {bad:?} must not be honoured"
            );
        }
    }

    #[test]
    fn the_harness_redirect_is_per_process_and_not_the_live_log() {
        let path = resolve_log_path(None, true, home()).expect("harness path");
        assert_ne!(Some(path.clone()), live());
        assert!(
            path.to_string_lossy()
                .ends_with(&format!("csr-test-hook-timing-{}.log", std::process::id())),
            "concurrent test binaries must not share one file: {}",
            path.display()
        );
    }

    #[test]
    fn without_an_override_or_harness_it_is_the_installed_path() {
        assert_eq!(resolve_log_path(None, false, home()), live());
        assert_eq!(resolve_log_path(None, false, None), None);
    }
}
