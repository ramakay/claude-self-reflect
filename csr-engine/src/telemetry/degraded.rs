//! What `healthy` does not say: the status signals that mean "the database is fine, something
//! else is not".
//!
//! `healthy` covers database integrity only, and other code reads it that way, so it keeps that
//! meaning. A separate `degraded` list in `status` JSON, and a `degraded:` tail on the telemetry
//! health label, name the rest. The rules, declared here:
//!
//! - prompt delivery: across projects, in the 72 h window of the prompt-delivery report (which
//!   starts at the running binary's install when that is newer), at least [`MIN_PROMPTS`] prompt
//!   ids, and at least [`TIMED_OUT_PERCENT`] percent of them timed out;
//! - hooks missing: any hook `hook install` registers that `settings.json` lacks;
//! - daemon restarts: [`RESTARTS_10M`] or more daemon starts in 10 minutes, or more than
//!   [`RESTARTS_24H`] in 24 hours (from the daemon's own `daemon-history.jsonl`). 12 in 24 h:
//!   the busiest install day on record restarted it about 5 times (5 large-block-cache start
//!   lines in `daemon.err.log` over 29 h, 2026-11-16/17), so more than 12 is not upgrades;
//! - unimported transcripts: any `aux.unimported_transcripts` (a settled transcript on disk the
//!   importer never recorded; the raw `aux.coverage.gap` is mostly deleted transcripts);
//! - restamp exhausted: the Codex timestamp restamp gave up (`attempts_exhausted`, not done);
//! - daemon down: the launch agent plist is installed (`daemon.agent_installed`) and no daemon
//!   holds the lock (`daemon.up` false), unless `CSR_NO_DAEMON` opts out. An install that left the
//!   plist but no daemon read `healthy: true, state: ok` before (Studio, 2026-11-19). It is a
//!   signal here, not `healthy: false`: `healthy` is the database verdict, and its readers (the
//!   telemetry `UNHEALTHY (reason)` line, SwiftBar's `DB:` line) would name a database fault.
//!
//! `state` is the one-word verdict a reader sees first: `unhealthy` when the database integrity
//! check failed, else `degraded` when any signal is present, else `unknown` while no integrity
//! verdict exists yet, else `ok`. So `healthy: true` beside a non-empty `degraded` reads as
//! `state: degraded`, never as fine. `unknown` is a fourth word on purpose: a fresh install
//! with no integrity verdict has not been shown to be ok.
//!
//! Every function here is pure over a report, so the rules are tested on fixtures.

use chrono::{DateTime, Duration, Utc};

use super::prompt_delivery::PromptDelivery;
use crate::binary_identity::StaleProcess;

/// Prompt ids needed in the window before a timed-out share is reported at all.
pub const MIN_PROMPTS: u64 = 10;
/// Timed-out share, in percent of the prompt ids, at which prompt delivery counts as degraded.
pub const TIMED_OUT_PERCENT: u64 = 10;
/// The prompt-delivery window read, in hours (the report's `last_72h`).
const WINDOW_HOURS: i64 = 72;
/// Daemon starts in 10 minutes that count as a restart loop.
pub const RESTARTS_10M: u64 = 3;
/// Daemon starts in 24 hours above which restarts count as degraded.
pub const RESTARTS_24H: u64 = 12;

/// How the prompt-delivery window should be named in a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// The running binary was installed inside the window: the window is "since install".
    SinceInstall,
    /// The install is older than the window, or its time is unknown.
    Last72h,
}

/// One reason `status` is degraded while `healthy` may be true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degraded {
    PromptDelivery { span: Span, n: u64, timed_out: u64 },
    HooksMissing(Vec<String>),
    DaemonRestarts { last_10m: u64, last_24h: u64 },
    UnimportedTranscripts(u64),
    RestampExhausted { attempts: u64 },
    DaemonDown,
}

impl Degraded {
    /// The `degraded` entry in `status` JSON.
    pub fn detail(&self) -> String {
        match self {
            Degraded::PromptDelivery {
                span,
                n,
                timed_out,
            } => {
                let when = match span {
                    Span::SinceInstall => "since install",
                    Span::Last72h => "in the last 72h",
                };
                format!("prompt_delivery: {timed_out} of {n} prompts {when} timed out")
            }
            Degraded::HooksMissing(events) => format!(
                "hooks_missing: {} not registered in settings.json (run `csr-engine hook install --apply`)",
                events.join(", ")
            ),
            Degraded::DaemonRestarts { last_10m, last_24h } => format!(
                "daemon_restarts: {last_10m} daemon starts in the last 10 minutes, {last_24h} in 24 hours (see daemon.history.previous_run_end and logs/daemon.err.log)"
            ),
            Degraded::UnimportedTranscripts(n) => format!(
                "unimported_transcripts: {n} transcripts on disk were never imported (the watcher or the catch-up sweep missed them)"
            ),
            Degraded::RestampExhausted { attempts } => format!(
                "codex_restamp: gave up after {attempts} attempts with the pass unfinished (see codex_backfill.restamp)"
            ),
            Degraded::DaemonDown => "daemon_down: the launch agent is installed but no daemon is running, so dreams and enrichment are stopped (run `csr-engine setup`; see logs/daemon.err.log)".to_string(),
        }
    }

    /// The segment after `degraded:` in the telemetry health label.
    pub fn short(&self) -> String {
        match self {
            Degraded::PromptDelivery { n, timed_out, .. } => {
                format!("prompt delivery {timed_out}/{n} timed out")
            }
            Degraded::HooksMissing(events) => format!("hooks missing {}", events.join(", ")),
            Degraded::DaemonRestarts { last_10m, last_24h } => {
                format!("daemon restarted {last_10m}x/10m {last_24h}x/24h")
            }
            Degraded::UnimportedTranscripts(n) => format!("{n} transcripts not imported"),
            Degraded::RestampExhausted { .. } => "codex restamp gave up".to_string(),
            Degraded::DaemonDown => "daemon down".to_string(),
        }
    }
}

/// The signals the `status` report itself carries, read from its JSON so any report shape
/// that serializes these keys works: `daemon.history`, `aux.unimported_transcripts`,
/// `codex_backfill.restamp`, `daemon.agent_installed` with `daemon.up`. A key that is absent or
/// null raises nothing. `CSR_NO_DAEMON` is read here (see [`status_signals_with`]).
pub fn status_signals(report: &serde_json::Value) -> Vec<Degraded> {
    let opted_out = std::env::var("CSR_NO_DAEMON").is_ok_and(|v| matches!(v.trim(), "1" | "true"));
    status_signals_with(report, opted_out)
}

/// [`status_signals`] with the `CSR_NO_DAEMON` opt-out given, so the rules stay pure.
pub fn status_signals_with(report: &serde_json::Value, daemon_opted_out: bool) -> Vec<Degraded> {
    let num = |path: &[&str]| -> Option<u64> {
        path.iter()
            .try_fold(report, |value, key| value.get(key))
            .and_then(serde_json::Value::as_u64)
    };
    let mut out = Vec::new();
    if let (Some(last_10m), Some(last_24h)) = (
        num(&["daemon", "history", "starts_last_10m"]),
        num(&["daemon", "history", "starts_last_24h"]),
    ) {
        if last_10m >= RESTARTS_10M || last_24h > RESTARTS_24H {
            out.push(Degraded::DaemonRestarts { last_10m, last_24h });
        }
    }
    if let Some(n) = num(&["aux", "unimported_transcripts"]).filter(|n| *n > 0) {
        out.push(Degraded::UnimportedTranscripts(n));
    }
    let restamp = report.get("codex_backfill").and_then(|c| c.get("restamp"));
    let exhausted = restamp
        .and_then(|r| r.get("attempts_exhausted"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if exhausted {
        out.push(Degraded::RestampExhausted {
            attempts: num(&["codex_backfill", "restamp", "attempts"]).unwrap_or(0),
        });
    }
    let daemon = report.get("daemon");
    let flag = |key: &str| {
        daemon
            .and_then(|d| d.get(key))
            .and_then(serde_json::Value::as_bool)
    };
    if !daemon_opted_out && flag("agent_installed") == Some(true) && flag("up") == Some(false) {
        out.push(Degraded::DaemonDown);
    }
    out
}

/// The one-word verdict: `unhealthy`, `degraded`, `unknown` or `ok` (see the module doc).
pub fn state(healthy: Option<bool>, signals: &[Degraded]) -> &'static str {
    match healthy {
        Some(false) => "unhealthy",
        _ if !signals.is_empty() => "degraded",
        None => "unknown",
        Some(true) => "ok",
    }
}

/// Every signal for a status report: prompt delivery and hooks (read from the hook log and
/// `settings.json`) plus the report's own ([`status_signals`]).
pub fn signals_for_report(report: &serde_json::Value) -> Vec<Degraded> {
    let mut signals = degraded_now();
    signals.extend(status_signals(report));
    signals
}

/// The degraded signals in `delivery` and `hooks_missing`, as of `now`.
pub fn degraded_signals(
    delivery: &PromptDelivery,
    hooks_missing: &[String],
    now: DateTime<Utc>,
) -> Vec<Degraded> {
    let mut out = Vec::new();
    let n: u64 = delivery.last_72h.values().map(|p| p.n).sum();
    let timed_out: u64 = delivery.last_72h.values().map(|p| p.timed_out).sum();
    if n >= MIN_PROMPTS && timed_out * 100 >= n * TIMED_OUT_PERCENT {
        let since_install = delivery
            .installed_at
            .is_some_and(|at| now - at <= Duration::hours(WINDOW_HOURS));
        out.push(Degraded::PromptDelivery {
            span: if since_install {
                Span::SinceInstall
            } else {
                Span::Last72h
            },
            n,
            timed_out,
        });
    }
    if !hooks_missing.is_empty() {
        out.push(Degraded::HooksMissing(hooks_missing.to_vec()));
    }
    out
}

/// The signals for this installation, now. Reads the hook-timing log and `settings.json`.
pub fn degraded_now() -> Vec<Degraded> {
    degraded_signals(
        &super::prompt_delivery::report_now(),
        &crate::hooks::install::missing_csr_hook_events_now(),
        Utc::now(),
    )
}

/// Put `degraded` (the entries, `[]` when there are none) and `state` (see [`state`]) into a
/// status JSON object. `healthy` is left as it is: it keeps its database-only meaning.
pub fn add_degraded(report: &mut serde_json::Value, signals: &[Degraded]) {
    if let Some(object) = report.as_object_mut() {
        let healthy = object.get("healthy").and_then(serde_json::Value::as_bool);
        object.insert("state".into(), serde_json::json!(state(healthy, signals)));
        let lines: Vec<String> = signals.iter().map(Degraded::detail).collect();
        object.insert("degraded".into(), serde_json::json!(lines));
    }
}

/// The telemetry `health=` value: `health` as is when nothing is degraded, else the word
/// `degraded` first, then the database verdict and the signals.
pub fn health_label(health: &str, signals: &[Degraded]) -> String {
    if signals.is_empty() {
        return health.to_string();
    }
    let shorts: Vec<String> = signals.iter().map(Degraded::short).collect();
    format!("degraded · database {health} · {}", shorts.join("; "))
}

/// The SwiftBar health lines: the state first, coloured, then one submenu line per signal and
/// the database verdict.
pub fn swiftbar_lines(healthy: Option<bool>, signals: &[Degraded]) -> Vec<String> {
    let color = match state(healthy, signals) {
        "ok" => "green",
        "unhealthy" => "red",
        "degraded" => "orange",
        _ => "gray",
    };
    let mut lines = vec![format!(
        "State: {} | color={color}",
        state(healthy, signals)
    )];
    lines.extend(
        signals
            .iter()
            .map(|s| format!("--{} | font=Menlo", s.short())),
    );
    lines.push(
        match healthy {
            Some(true) => "DB: healthy ✓ | color=green",
            Some(false) => "DB: unhealthy ✗ | color=red",
            None => "DB: integrity unknown | color=gray",
        }
        .to_string(),
    );
    lines
}

/// The one telemetry line for processes on a superseded binary; None when there are none.
pub fn stale_line(stale: &[StaleProcess]) -> Option<String> {
    if stale.is_empty() {
        return None;
    }
    let companions = stale
        .iter()
        .filter(|p| {
            p.role == crate::stale_report::COMPANION_PANE_ROLE
                || p.role == crate::stale_report::COMPANION_SERVE_ROLE
        })
        .count();
    let noun = if stale.len() == 1 {
        "process"
    } else {
        "processes"
    };
    Some(format!(
        "  Stale   {} {noun} on an older binary ({companions} companion pane/relay): restart those sessions, or run `csr-engine hook install --apply` to wake dormant panes",
        stale.len()
    ))
}

#[cfg(test)]
#[path = "degraded_tests.rs"]
mod tests;
