//! Why the daemon stops, and the log and history lines that say so.
//!
//! The stop and exit lines used to be `tracing::info!` under the default `warn` filter, so a
//! stopped daemon left nothing in its log. They are now stamped `eprintln!` lines (the daemon's
//! stderr is `daemon.err.log`) and `stop`/`exit` events in `crate::daemon_history`, so the
//! next start can say how this run ended. A SIGKILL leaves neither: the next start then
//! records "ended without an exit line".

use anyhow::Result;

/// The reason recorded when the binary on disk was replaced.
pub const BINARY_REPLACED: &str = "binary replaced";

/// Wait for a termination signal: Ctrl+C (SIGINT — an interactive
/// `csr-engine daemon` run) OR SIGTERM (what `launchctl bootout` and the
/// installer's stop step actually send). Resolves to the signal's name.
///
/// WP-D low: a bare `tokio::signal::ctrl_c().await` only ever resolves on
/// SIGINT. SIGTERM is left at the process's default disposition —
/// immediate termination, no Rust code runs at all — which skips the ENTIRE
/// graceful shutdown sequence in `Daemon::run` (setting the shutdown flag,
/// joining every loop, flushing the HNSW index). Calling
/// `tokio::signal::unix::signal` installs a handler that intercepts SIGTERM
/// for the rest of this process's life instead of the OS default — racing it
/// against `ctrl_c()` makes either signal run the same graceful path.
#[cfg(unix)]
pub(crate) async fn wait_for_termination_signal() -> Result<&'static str> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.map(|()| "SIGINT").map_err(Into::into),
        _ = sigterm.recv() => Ok("SIGTERM"),
    }
}

/// No SIGTERM-specific handling exists on non-unix targets; Ctrl+C is the
/// only termination signal Windows Ctrl handlers map onto via `ctrl_c()`.
#[cfg(not(unix))]
pub(crate) async fn wait_for_termination_signal() -> Result<&'static str> {
    tokio::signal::ctrl_c()
        .await
        .map(|()| "SIGINT")
        .map_err(Into::into)
}

/// Resolves to the process exit code `Daemon::run` should return and the stop
/// reason, once EITHER `signal` (the plain Ctrl+C/SIGTERM wait) or `replaced`
/// (the binary-replacement watch) resolves, whichever comes first: `0` and the
/// signal's name for an ordinary signal-driven shutdown, propagating any error
/// `signal` returns; [`crate::binary_identity::DAEMON_REPLACED_EXIT_CODE`] and
/// [`BINARY_REPLACED`] once `replaced` resolves. `replaced` is `None` when this
/// process never captured its own startup identity (nothing to watch), in which
/// case this just awaits `signal`.
///
/// A seam a test can drive directly with stub futures — a future edit that
/// drops the replacement arm, or maps it to any exit code other than 75,
/// compiles fine and only shows up in production when launchd doesn't restart
/// the daemon after an install (gap-G2 review finding 1).
pub(crate) async fn shutdown_reason<S, R>(
    signal: S,
    replaced: Option<R>,
) -> Result<(i32, &'static str)>
where
    S: std::future::Future<Output = Result<&'static str>>,
    R: std::future::Future,
{
    match replaced {
        Some(replaced) => {
            tokio::pin!(signal);
            tokio::pin!(replaced);
            tokio::select! {
                result = &mut signal => Ok((0, result?)),
                _ = &mut replaced => {
                    Ok((crate::binary_identity::DAEMON_REPLACED_EXIT_CODE, BINARY_REPLACED))
                }
            }
        }
        None => Ok((0, signal.await?)),
    }
}

/// The stamped daemon-log line for a stop that has just begun.
pub(crate) fn stop_line(reason: &str) -> String {
    crate::telemetry::stamp_timing_line(&format!(
        "CSR daemon stop: {reason}; graceful shutdown started"
    ))
}

/// The stamped daemon-log line for a graceful shutdown that finished.
pub(crate) fn exit_line(reason: &str, shutdown_ms: u64, code: i32) -> String {
    crate::telemetry::stamp_timing_line(&format!(
        "CSR daemon exit: {reason}; graceful shutdown finished in {shutdown_ms} ms (exit code {code})"
    ))
}

/// Log the stop and record it in the daemon history.
pub(crate) fn record_stop(reason: &str) {
    eprintln!("{}", stop_line(reason));
    if let Some(path) = crate::daemon_history::default_path() {
        crate::daemon_history::record_stop(&path, std::process::id(), reason, chrono::Utc::now());
    }
}

/// Log the finished shutdown and record it in the daemon history.
pub(crate) fn record_exit(reason: &str, started: std::time::Instant, code: i32) {
    let ms = started.elapsed().as_millis() as u64;
    eprintln!("{}", exit_line(reason, ms, code));
    if let Some(path) = crate::daemon_history::default_path() {
        crate::daemon_history::record_exit(
            &path,
            std::process::id(),
            reason,
            ms,
            chrono::Utc::now(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_stamped(line: &str) -> bool {
        let bytes = line.as_bytes();
        line.len() > 21
            && bytes[4] == b'-'
            && bytes[10] == b'T'
            && bytes[19] == b'Z'
            && bytes[20] == b' '
    }

    #[test]
    fn the_stop_and_exit_lines_are_stamped_and_name_the_reason() {
        let stop = stop_line("SIGTERM");
        assert!(is_stamped(&stop), "{stop}");
        assert!(stop.ends_with("CSR daemon stop: SIGTERM; graceful shutdown started"));
        let exit = exit_line(BINARY_REPLACED, 1234, 75);
        assert!(is_stamped(&exit), "{exit}");
        assert!(exit.ends_with(
            "CSR daemon exit: binary replaced; graceful shutdown finished in 1234 ms (exit code 75)"
        ));
    }

    // --- WP-D low: `Daemon::run` must also stop on SIGTERM, not only
    // SIGINT (`ctrl_c`) — `launchctl bootout` and the installer's stop step
    // send SIGTERM, which the OS's default disposition kills the process
    // on immediately (no Rust code runs, no graceful shutdown/flush). ---

    #[cfg(unix)]
    #[tokio::test]
    async fn wait_for_termination_signal_resolves_on_sigterm_not_just_sigint() {
        // Installing tokio's SIGTERM handler (inside the function under
        // test) intercepts SIGTERM for the rest of THIS test process's
        // life instead of the OS default (terminate) — so raising it below
        // is safe and does not kill the test binary.
        let handle = tokio::spawn(wait_for_termination_signal());
        // Give the handler a moment to install before raising.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // SAFETY: `raise` with a valid signal number sends SIGTERM to this
        // process; the installed tokio handler above intercepts it.
        unsafe {
            libc::raise(libc::SIGTERM);
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect(
                "wait_for_termination_signal must resolve on SIGTERM, not hang \
                 waiting on SIGINT alone",
            )
            .expect("task must not panic");
        assert_eq!(
            result.expect("SIGTERM must resolve as a clean shutdown signal"),
            "SIGTERM",
            "the stop reason names the signal that arrived"
        );
    }

    // --- gap-G2 review finding 1: `shutdown_reason` is the exact mapping
    // `Daemon::run` calls from `signal-vs-replaced resolved first` to the
    // process exit code. These drive it directly (never through the real
    // signal handler, never through the real 30s/3s poll). ---

    #[tokio::test]
    async fn shutdown_reason_returns_75_when_replaced_resolves_first() {
        // Signal side never resolves; replacement side resolves immediately.
        let signal = std::future::pending::<Result<&'static str>>();
        let replaced = std::future::ready(());
        let (code, reason) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            shutdown_reason(signal, Some(replaced)),
        )
        .await
        .expect("must not hang waiting on a signal that never resolves")
        .expect("replaced arm must not propagate an error");
        assert_eq!(
            code,
            crate::binary_identity::DAEMON_REPLACED_EXIT_CODE,
            "a confirmed binary replacement must map to the launchd-restart \
             exit code — launchd's KeepAlive.SuccessfulExit=false only \
             restarts on a NON-zero exit"
        );
        assert_eq!(reason, BINARY_REPLACED);
    }

    #[tokio::test]
    async fn shutdown_reason_returns_0_when_signal_resolves_first() {
        // Replacement side never resolves; signal side resolves immediately.
        let signal = std::future::ready(Ok("SIGTERM"));
        let replaced = std::future::pending::<()>();
        let (code, reason) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            shutdown_reason(signal, Some(replaced)),
        )
        .await
        .expect("must not hang waiting on a replacement watch that never resolves")
        .expect("signal arm must not propagate an error");
        assert_eq!(
            code, 0,
            "an ordinary signal-driven shutdown must exit 0, not the \
             replaced-binary code"
        );
        assert_eq!(reason, "SIGTERM");
    }

    #[tokio::test]
    async fn shutdown_reason_propagates_a_signal_error() {
        let signal = std::future::ready(Err(anyhow::anyhow!("signal handler install failed")));
        let replaced = std::future::pending::<()>();
        let err = shutdown_reason(signal, Some(replaced))
            .await
            .expect_err("an error from the signal future must propagate, not be swallowed");
        assert!(err.to_string().contains("signal handler install failed"));
    }

    #[tokio::test]
    async fn shutdown_reason_falls_back_to_the_plain_signal_when_replaced_is_none() {
        // No startup identity captured -> nothing to watch; must still
        // resolve once the signal does, and never look like a replacement.
        let signal = std::future::ready(Ok("SIGINT"));
        let (code, reason) = shutdown_reason::<_, std::future::Pending<()>>(signal, None)
            .await
            .unwrap();
        assert_eq!((code, reason), (0, "SIGINT"));
    }
}
