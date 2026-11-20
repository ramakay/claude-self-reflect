//! The daemon step's waits and its words: how long setup waits for launchd to let go of the old
//! job after `bootout`, how it confirms the new daemon runs, and what it prints for each outcome.

use std::path::Path;
use std::time::Duration;

use super::{daemon_stop_wait, DaemonOutcome, DaemonRun, LaunchctlRun};

/// Ask launchd about `target` until it no longer has the job, pausing between looks, for at most
/// [`daemon_stop_wait`] in total. launchd answers `bootout` at once but keeps the job, state
/// "SIGTERMed", until the old daemon has finished its graceful stop (about 20 s), and sends SIGKILL
/// at the plist's `ExitTimeOut`. `Ok` when the job is gone; `Err` with the time waited when not.
/// The time is counted from the pauses, so a test's no-op pause runs the whole budget at once.
pub(super) fn wait_for_unload(
    launchctl: &mut LaunchctlRun,
    target: &str,
    pause: &dyn Fn(Duration),
) -> Result<(), Duration> {
    let budget = daemon_stop_wait();
    let mut waited = Duration::ZERO;
    loop {
        let gap = Duration::from_millis(if waited.is_zero() { 500 } else { 300 });
        pause(gap);
        waited += gap;
        if launchctl(&["print", target]).is_err() {
            return Ok(());
        }
        if waited >= budget {
            return Err(waited);
        }
    }
}

/// True when the daemon step failed in a way setup must not call a success: the agent was meant
/// to run and does not. An opt-out, a system without launchd and a daemon launchd does not own
/// are not failures.
pub(super) fn daemon_step_failed(outcome: &DaemonOutcome) -> bool {
    matches!(outcome, DaemonOutcome::Failed(_))
}

/// The lines setup prints for each outcome (stderr, like every other step).
pub(super) fn daemon_report(run: &DaemonRun, engine: &Path) -> String {
    let label = &run.label;
    let manual = format!(
        "  To run it by hand: {} daemon   (keep it running; it does the overnight dreams)\n",
        engine.display()
    );
    match &run.outcome {
        DaemonOutcome::OptedOut => {
            "  Daemon skipped (CSR_NO_DAEMON is set). Dreams and background enrichment need it.\n"
                .to_owned()
                + &manual
        }
        DaemonOutcome::Manual => {
            "  No launchd on this system, so setup does not start the daemon.\n".to_owned() + &manual
        }
        DaemonOutcome::Running { rewritten } => format!(
            "  Daemon agent {} and started: {label}\n  Stop it with: launchctl bootout gui/$(id -u)/{label}\n",
            if *rewritten { "installed" } else { "already installed" },
        ),
        DaemonOutcome::KeptExisting => {
            format!("  Kept your existing {label} agent and started it.\n")
        }
        DaemonOutcome::AlreadyRunning { pid } => format!(
            "  A daemon is already running (pid {pid}) and launchd does not manage it, so setup added no second one.\n  To have launchd run it: stop that process (kill {pid}), then run: csr-engine setup\n"
        ),
        DaemonOutcome::Failed(why) => format!(
            "  The daemon did not start: {why}\n  Everything else is set up; setup exits non-zero so the daemon is not missed. Retry with: {} setup\n{manual}",
            engine.display()
        ),
    }
}

/// What one look at the daemon shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DaemonProbe {
    /// `pid` in `launchctl print` for the job.
    pub(super) job_pid: Option<u32>,
    /// Something holds `daemon.lock`.
    pub(super) lock_held: bool,
    /// The pid written in `daemon.lock`.
    pub(super) lock_pid: Option<u32>,
}

/// Is this the process setup started? The lock is held by a pid, that pid is
/// the job's, and it is not the pid the job had before setup touched it. A held
/// lock alone proves nothing: an older daemon, or a daemon under another label,
/// holds it too while the job setup started is still loading or failing.
pub(super) fn daemon_confirmed(probe: &DaemonProbe, previous_pid: Option<u32>) -> bool {
    probe.lock_held
        && probe.job_pid.is_some()
        && probe.job_pid == probe.lock_pid
        && probe.job_pid != previous_pid
}

/// Look until the daemon setup started is confirmed, or the polls run out.
pub(super) fn wait_for_daemon(
    previous_pid: Option<u32>,
    polls: u32,
    probe: &mut dyn FnMut() -> DaemonProbe,
    pause: &dyn Fn(),
) -> bool {
    for poll in 0..polls {
        if poll > 0 {
            pause();
        }
        if daemon_confirmed(&probe(), previous_pid) {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[path = "setup_daemon_start_tests.rs"]
mod tests;
