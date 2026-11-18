//! `csr-engine daemon` runs with macOS malloc's large-block cache off.
//!
//! macOS malloc keeps freed large blocks in a per-process cache, and that
//! memory stays dirty and counts in the footprint; the daemon's periodic jobs
//! leave it there after they end. `malloc_zone_pressure_relief` does not flush
//! that cache (measured: 0 bytes). `MallocLargeCache=0` turns it off, but
//! malloc reads it only when the process starts. So the daemon entry, when the
//! variable is unset, re-executes itself once with `MallocLargeCache=0` and a
//! guard variable. The new process sees the guard, never re-executes again,
//! and takes both variables out of its environment so its children (the index
//! build, narrators and the hooks they run) start without them.
//!
//! Only the daemon entry calls this: hooks, the MCP server and the pane never
//! do. A no-op off macOS. A `MallocLargeCache` already set by the person is
//! left alone. `CSR_NO_DAEMON_MALLOC_TUNING=1` (or `true`) turns it off.

use std::ffi::OsStr;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set once in the re-executed daemon, before any thread starts.
static LARGE_BLOCK_CACHE_OFF: AtomicBool = AtomicBool::new(false);

/// True in a process that started with malloc's large-block cache off: the
/// re-executed daemon, or a process whose own environment holds
/// `MallocLargeCache=0`. A freed large block goes back to the system at once
/// there, so `CSR_CE_ARENA=0` can run the cross-encoder without ORT's CPU
/// arena, which keeps the largest run it has seen for the life of the process.
pub fn large_block_cache_off() -> bool {
    LARGE_BLOCK_CACHE_OFF.load(Ordering::Relaxed)
        || std::env::var_os(VARIABLE).is_some_and(|value| value == "0")
}

/// The kill switch.
pub const OFF_SWITCH: &str = "CSR_NO_DAEMON_MALLOC_TUNING";
/// The macOS malloc setting.
pub const VARIABLE: &str = "MallocLargeCache";
/// Set on the re-executed process only.
pub const GUARD: &str = "CSR_DAEMON_MALLOC_REEXEC";
/// The line the re-executed daemon writes to stderr (its error log) once.
pub const STARTED_LINE: &str =
    "CSR daemon malloc: large-block cache off (MallocLargeCache=0, re-executed once)";

/// What the daemon entry does about the setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Re-execute once with the setting on.
    ReExec,
    /// This is the re-executed process: never again; clear the variables.
    Started,
    /// Switched off, or the variable was set by someone else: do nothing.
    Leave,
}

/// Whether the switch's value turns this off: `1` or `true`, any case.
pub fn disabled_from(value: Option<&str>) -> bool {
    value.is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

/// The decision, from the switch, the variable and the guard. The guard wins
/// over everything, so a re-executed process can never re-execute.
pub fn plan(off: Option<&str>, variable: Option<&OsStr>, guard: Option<&OsStr>) -> Plan {
    if guard.is_some() {
        Plan::Started
    } else if disabled_from(off) || variable.is_some() {
        Plan::Leave
    } else {
        Plan::ReExec
    }
}

/// Call first thing in `csr-engine daemon`, before any thread starts. Returns
/// unless it re-executes; a failed re-exec is reported and the daemon goes on
/// with the cache on.
pub fn prepare_daemon_process() {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        let off = std::env::var(OFF_SWITCH).ok();
        let variable = std::env::var_os(VARIABLE);
        let guard = std::env::var_os(GUARD);
        match plan(off.as_deref(), variable.as_deref(), guard.as_deref()) {
            Plan::ReExec => {
                let Ok(exe) = std::env::current_exe() else {
                    return;
                };
                let mut argv = std::env::args_os();
                let arg0 = argv.next().unwrap_or_else(|| exe.clone().into_os_string());
                let error = std::process::Command::new(&exe)
                    .arg0(arg0)
                    .args(argv)
                    .env(VARIABLE, "0")
                    .env(GUARD, "1")
                    .exec();
                eprintln!(
                    "{}",
                    crate::telemetry::stamp_timing_line(&format!(
                        "CSR daemon malloc: re-exec failed, large-block cache stays on: {error}"
                    ))
                );
            }
            Plan::Started => {
                // Before the daemon starts any thread (the runtime, the logger
                // and the engine come later): nothing reads the environment
                // concurrently.
                std::env::remove_var(GUARD);
                std::env::remove_var(VARIABLE);
                LARGE_BLOCK_CACHE_OFF.store(true, Ordering::Relaxed);
                eprintln!("{}", crate::telemetry::stamp_timing_line(STARTED_LINE));
            }
            Plan::Leave => {}
        }
    }
    // Trim an oversized stderr log once, at start.
    crate::daemon_log::rotate_stderr_file();
    // No ONNX thread pool of its own: inference runs at the calling thread's QoS class.
    crate::embeddings::run_inference_on_calling_thread();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn the_guard_wins_so_a_re_executed_daemon_never_re_executes() {
        let one = OsString::from("1");
        let zero = OsString::from("0");
        for off in [None, Some("1"), Some("0")] {
            for variable in [None, Some(zero.as_os_str())] {
                assert_eq!(plan(off, variable, Some(one.as_os_str())), Plan::Started);
            }
        }
        assert_eq!(plan(None, None, None), Plan::ReExec);
    }

    #[test]
    fn the_switch_and_a_setting_of_the_person_s_own_leave_it_alone() {
        for (value, off) in [
            (None, false),
            (Some("0"), false),
            (Some("yes"), false),
            (Some("1"), true),
            (Some("true"), true),
            (Some("TRUE"), true),
        ] {
            assert_eq!(disabled_from(value), off, "{value:?}");
            let expected = if off { Plan::Leave } else { Plan::ReExec };
            assert_eq!(plan(value, None, None), expected, "{value:?}");
        }
        let own = OsString::from("1");
        assert_eq!(plan(None, Some(own.as_os_str()), None), Plan::Leave);
    }
}
