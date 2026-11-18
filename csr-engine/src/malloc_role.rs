//! The macOS malloc setting of a short-lived-burst process, by role: the relay
//! (`companion serve`), the MCP server and the watching pane.
//!
//! These processes do a burst of work (a tool call, a turn's handlers, a frame) and then wait.
//! With the default allocator the freed scratch of each burst stays in the process: a proxied
//! MCP server measured 89 to 105 MB after 12 tool calls, a relay 46 to 68 MB after a day, most
//! of it freed heap. `malloc_zone_pressure_relief` returns nothing on this macOS, so a malloc
//! setting is the lever. malloc reads it only when the process starts, so the entry
//! re-executes itself once with the variable and a guard set, as the daemon does with its own
//! setting (`daemon_start.rs`).
//!
//! Each role has its own setting ([`Role::setting`]). The MCP server and the pane take
//! `MallocSpaceEfficient=1`, which gives freed pages back (a proxied MCP server ended at 9 to
//! 10 MB in the same wall time). The relay takes `MallocLargeCache=0` alone, the daemon's
//! setting: measured on one machine it settled near 5.7 MB against 4.0 MB under
//! `MallocSpaceEfficient=1`, at about half the added latency on a 3 MB event.
//!
//! The re-executed process takes both variables out of its environment before any thread
//! starts, so the programs it starts (`git`, `claude -p`, shells, hooks) start with the
//! allocator as the person set it. Its own execs of itself (the pane resting, waking, following
//! a new session or a new engine) carry the setting and the guard with [`carried`], so they start
//! with the setting and never pay a second exec. A role's variable the person set is left
//! alone, and so inherited as they set it. `CSR_NO_MALLOC_TUNING=1` (or `true`) turns this off.
//! A no-op off macOS.

use std::ffi::{OsStr, OsString};
use std::sync::Mutex;

/// The kill switch.
pub const OFF_SWITCH: &str = "CSR_NO_MALLOC_TUNING";
/// Set on the re-executed process only.
pub const GUARD: &str = "CSR_MALLOC_REEXEC";

/// A burst process, by the malloc setting it starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// `companion serve`, one per session.
    Relay,
    /// The MCP server.
    Mcp,
    /// The watching pane (`--watch`, also `--dormant`).
    Pane,
}

impl Role {
    /// The malloc variable and the value this role starts with.
    pub const fn setting(self) -> (&'static str, &'static str) {
        match self {
            Self::Relay => ("MallocLargeCache", "0"),
            Self::Mcp | Self::Pane => ("MallocSpaceEfficient", "1"),
        }
    }
}

/// The role of a process that started with the setting this module gave it.
static SET_HERE: Mutex<Option<Role>> = Mutex::new(None);

/// What the entry does about the setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Re-execute once with the setting on.
    ReExec,
    /// This is the re-executed process: never again; clear the variables.
    Started,
    /// Switched off, or the person set the variable: do nothing.
    Leave,
}

/// The decision, from the switch, the role's variable and the guard. The guard wins over
/// everything, so a re-executed process can never re-execute.
pub fn plan(off: Option<&str>, variable: Option<&OsStr>, guard: Option<&OsStr>) -> Plan {
    if guard.is_some() {
        Plan::Started
    } else if crate::daemon_start::disabled_from(off) || variable.is_some() {
        Plan::Leave
    } else {
        Plan::ReExec
    }
}

/// The variables an exec of this same program sets so that the new image starts with the
/// setting and does not re-execute: the role's variable and the guard, in a process this module
/// set up; none otherwise (a setting of the person's own is inherited as it is).
pub fn carried() -> Vec<(&'static str, OsString)> {
    carried_for(*SET_HERE.lock().unwrap_or_else(|e| e.into_inner()))
}

fn carried_for(role: Option<Role>) -> Vec<(&'static str, OsString)> {
    match role {
        Some(role) => {
            let (variable, value) = role.setting();
            vec![(variable, value.into()), (GUARD, "1".into())]
        }
        None => vec![],
    }
}

/// Call first thing in the relay, the MCP server and the watching pane, before any thread
/// starts. Returns unless it re-executes; a failed re-exec is reported and the process goes on
/// with the default allocator.
pub fn prepare_burst_process(role: Role) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        let (name, _) = role.setting();
        let off = std::env::var(OFF_SWITCH).ok();
        let variable = std::env::var_os(name);
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
                    .envs(carried_for(Some(role)))
                    .exec();
                eprintln!("CSR malloc: re-exec failed, the default allocator stays: {error}");
            }
            Plan::Started => {
                // No thread runs yet: nothing reads the environment concurrently.
                std::env::remove_var(GUARD);
                std::env::remove_var(name);
                *SET_HERE.lock().unwrap_or_else(|e| e.into_inner()) = Some(role);
            }
            Plan::Leave => {}
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = role;
}

/// The role dispatch `main` makes: the daemon's own setting, or the MCP server's.
pub fn prepare(daemon: bool, mcp_server: bool) {
    if daemon {
        crate::daemon_start::prepare_daemon_process();
    } else if mcp_server {
        prepare_burst_process(Role::Mcp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_wins_so_a_re_executed_process_never_re_executes() {
        let one = OsString::from("1");
        for off in [None, Some("1"), Some("0")] {
            for variable in [None, Some(one.as_os_str())] {
                assert_eq!(plan(off, variable, Some(one.as_os_str())), Plan::Started);
            }
        }
        assert_eq!(plan(None, None, None), Plan::ReExec);
    }

    #[test]
    fn a_setting_of_the_person_s_own_and_the_switch_leave_it_alone() {
        let own = OsString::from("0");
        assert_eq!(plan(None, Some(own.as_os_str()), None), Plan::Leave);
        for off in ["1", "true", "TRUE"] {
            assert_eq!(plan(Some(off), None, None), Plan::Leave, "{off}");
        }
        for off in ["0", "yes", ""] {
            assert_eq!(plan(Some(off), None, None), Plan::ReExec, "{off}");
        }
    }

    #[test]
    fn relays_take_the_large_block_cache_off_alone_and_the_others_space_efficiency() {
        assert_eq!(Role::Relay.setting(), ("MallocLargeCache", "0"));
        assert_eq!(Role::Mcp.setting(), ("MallocSpaceEfficient", "1"));
        assert_eq!(Role::Pane.setting(), ("MallocSpaceEfficient", "1"));
    }

    /// What a program this process starts sees of the malloc variables and the guard.
    #[cfg(target_os = "macos")]
    fn seen_by_a_child() -> String {
        let out = std::process::Command::new("/usr/bin/env").output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| {
                l.starts_with("MallocSpaceEfficient=")
                    || l.starts_with("MallocLargeCache=")
                    || l.starts_with(GUARD)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_child_inherits_the_persons_own_setting_and_never_the_one_this_module_gave() {
        for role in [Role::Pane, Role::Relay] {
            let (name, value) = role.setting();
            let own = if value == "1" { "0" } else { "1" };
            *SET_HERE.lock().unwrap() = None;
            // The person's own value, no guard: left alone, so a child sees it as they set it.
            std::env::set_var(name, own);
            prepare_burst_process(role);
            assert_eq!(seen_by_a_child(), format!("{name}={own}"), "{role:?}");
            assert!(carried().is_empty(), "{role:?}");
            // The re-executed image (both variables set by its parent): it clears them, and a
            // child sees neither, while its own execs are handed both.
            std::env::set_var(name, value);
            std::env::set_var(GUARD, "1");
            prepare_burst_process(role);
            assert_eq!(seen_by_a_child(), "", "{role:?}");
            assert_eq!(carried(), carried_for(Some(role)), "{role:?}");
        }
    }

    #[test]
    fn only_a_process_this_set_up_hands_the_setting_and_the_guard_to_its_own_execs() {
        assert!(carried_for(None).is_empty());
        for (role, variable, value) in [
            (Role::Pane, "MallocSpaceEfficient", "1"),
            (Role::Relay, "MallocLargeCache", "0"),
        ] {
            let handed = carried_for(Some(role));
            assert_eq!(
                handed,
                vec![
                    (variable, OsString::from(value)),
                    (GUARD, OsString::from("1"))
                ]
            );
            // What it hands over is what makes the next image start without a second exec.
            let guard = handed
                .iter()
                .find(|(k, _)| *k == GUARD)
                .map(|(_, v)| v.as_os_str());
            assert_eq!(plan(None, Some(OsStr::new(value)), guard), Plan::Started);
        }
    }
}
