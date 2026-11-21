//! A process deadline for the hooks that run under Claude Code's hook timeout and have no
//! deadline of their own.
//!
//! Claude Code kills a command hook at its timeout (60 s by default; the installer sets 150 s for
//! `session-briefing`). A killed hook is reported as an error in the session. These hooks do
//! synchronous work (storage, embeddings, index load, `claude -p`) that no Rust timeout can
//! preempt, so a stuck one would run to that kill. [`arm`] starts a thread that ends the process
//! with exit 0 and no output a little before it, so the hook fails open.
//!
//! The hooks that already bound themselves are left alone: `prompt-submit` and `post-compact`
//! (560 ms, `prompt_transport::spawn_watchdog`), `stop` (spools and detaches), `session-end`
//! (re-spawns itself detached, so neither the parent nor the child is armed), `stop-work` (a
//! detached worker nobody waits on), `install` and the retired hooks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Overrides the deadline of every armed hook, in whole seconds (at least 1).
pub const OVERRIDE_ENV: &str = "CSR_HOOK_DEADLINE_SECS";

/// Under Claude Code's default 60 s hook timeout.
const DEFAULT_SECS: u64 = 55;
/// Under the 150 s the installer gives `session-briefing`.
const BRIEFING_SECS: u64 = 145;
/// What is left of the deadline for the timing line, so a log write that hangs cannot hold the exit.
const LOG_WAIT: Duration = Duration::from_millis(200);

static ARMED: AtomicBool = AtomicBool::new(false);

/// How long `hook_name` may run, or `None` when it must not be armed. `override_secs` is the raw
/// value of [`OVERRIDE_ENV`]; a value that is not a whole number of at least 1 is ignored. The
/// override changes the time of a hook that is armed. It never arms another one.
pub fn deadline_for(hook_name: &str, override_secs: Option<&str>) -> Option<Duration> {
    let default = match hook_name {
        "session-start" | "precompact" | "post-tool-use" => DEFAULT_SECS,
        "session-briefing" => BRIEFING_SECS,
        _ => return None,
    };
    let secs = override_secs
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs >= 1)
        .unwrap_or(default);
    Some(Duration::from_secs(secs))
}

/// Start the deadline thread for `hook_name`, if it has one. Call once, first thing in the
/// process. When the time is up the thread writes one timing line and ends the process with
/// exit 0 and no output. A second call does nothing, and so does a failure to start the thread
/// (the hook then runs as it did before).
pub fn arm(hook_name: &str) {
    let override_secs = std::env::var(OVERRIDE_ENV).ok();
    let Some(limit) = deadline_for(hook_name, override_secs.as_deref()) else {
        return;
    };
    if ARMED.swap(true, Ordering::SeqCst) {
        return;
    }
    let name = hook_name.to_string();
    let started = Instant::now();
    let _ = std::thread::Builder::new()
        .name("hook-deadline".into())
        .spawn(move || {
            std::thread::sleep(limit);
            let line = deadline_line(&name, started.elapsed());
            // Opening the log can block (a FIFO nobody reads, a hung disk): bound the write.
            super::prompt_transport::finish_within(
                move || crate::telemetry::append_timing_line(&line),
                LOG_WAIT,
            );
            // Rust's exit cleanup flushes stdout and can deadlock behind a writer that holds
            // its lock. A hook holds no state that needs cleanup (the maintenance lock is
            // released by the OS), so skip it.
            #[cfg(unix)]
            unsafe {
                libc::_exit(0)
            }
            #[cfg(not(unix))]
            std::process::exit(0);
        });
}

/// `total=` is read by key by `telemetry::parser`, so the hook shows up in the latency stats.
fn deadline_line(hook_name: &str, elapsed: Duration) -> String {
    format!(
        "CSR hook {hook_name}: total={}ms deadline=1 includes_startup=1 injected=0",
        elapsed.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(hook: &str, raw: Option<&str>) -> Option<u64> {
        deadline_for(hook, raw).map(|d| d.as_secs())
    }

    #[test]
    fn the_three_open_ended_hooks_get_55_seconds_and_the_briefing_145() {
        for hook in ["session-start", "precompact", "post-tool-use"] {
            assert_eq!(secs(hook, None), Some(55), "{hook}");
        }
        assert_eq!(secs("session-briefing", None), Some(145));
    }

    #[test]
    fn every_other_hook_has_no_deadline() {
        for hook in [
            "prompt-submit",
            "post-compact",
            "stop",
            "session-end",
            "stop-work",
            "install",
            "subagent-stop",
            "",
            "unknown",
        ] {
            assert_eq!(secs(hook, None), None, "{hook}");
            assert_eq!(
                secs(hook, Some("1")),
                None,
                "the override must not arm {hook}"
            );
        }
    }

    /// `hook session-end` runs twice, as itself and as its detached child (the same name,
    /// `CSR_HOOK_DETACHED=1`). Neither may carry a deadline: the child outlives the session on purpose.
    #[test]
    fn session_end_and_its_detached_child_are_not_armed() {
        assert_eq!(deadline_for("session-end", None), None);
        assert_eq!(deadline_for("session-end", Some("5")), None);
    }

    #[test]
    fn the_override_replaces_both_defaults() {
        assert_eq!(secs("session-start", Some("1")), Some(1));
        assert_eq!(secs("precompact", Some("7")), Some(7));
        assert_eq!(secs("post-tool-use", Some(" 30 ")), Some(30));
        assert_eq!(secs("session-briefing", Some("90")), Some(90));
    }

    #[test]
    fn an_invalid_override_is_ignored() {
        for raw in ["", "0", "-3", "abc", "1.5", "5s", "99999999999999999999999"] {
            assert_eq!(secs("session-start", Some(raw)), Some(55), "{raw:?}");
            assert_eq!(secs("session-briefing", Some(raw)), Some(145), "{raw:?}");
        }
    }

    #[test]
    fn arming_a_hook_without_a_deadline_starts_nothing() {
        // An armed name would end this test process after the deadline, so only a no-op name is armed here.
        arm("prompt-submit");
        arm("stop");
        assert!(!ARMED.load(Ordering::SeqCst));
    }

    #[test]
    fn the_timing_line_is_parseable_by_key() {
        let line = deadline_line("session-start", Duration::from_millis(55_012));
        let stamped = crate::telemetry::stamp_timing_line(&line);
        match crate::telemetry::parser::parse_line(&stamped) {
            Some(crate::telemetry::parser::Entry::Hook { name, total_ms, .. }) => {
                assert_eq!(name, "session-start");
                assert_eq!(total_ms, 55_012);
            }
            other => panic!("{other:?}"),
        }
        assert!(line.contains("deadline=1"));
    }
}
