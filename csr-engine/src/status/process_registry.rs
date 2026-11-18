use super::*;

/// Every live process (daemon or MCP stdio server) whose recorded exe
/// identity differs from the binary on disk right now. Best-effort: an
/// unreadable current identity (can't resolve `current_exe`) or a missing
/// run dir (nothing has ever registered) both report no stale processes
/// rather than erroring the whole status gather.
fn gather_stale_processes(run_dir: Option<&Path>) -> Vec<crate::binary_identity::StaleProcess> {
    let (Some(current), Some(run_dir)) = (crate::binary_identity::current_exe_identity(), run_dir)
    else {
        return Vec::new();
    };
    crate::binary_identity::stale_processes(run_dir, &current)
}

/// The registry's stale processes and, for the full report, the companion
/// panes and relays that never register. The census is one `ps` and one
/// `lsof`, so the statusline's compact scope skips it; so does a test
/// harness, which has no installed binary of its own to compare against.
pub(super) fn stale_processes_for(
    run_dir: Option<&Path>,
    scope: Scope,
) -> Vec<crate::binary_identity::StaleProcess> {
    let registry = gather_stale_processes(run_dir);
    match (scope, crate::process::detached_child_exe()) {
        (Scope::Full, Ok(Some(exe))) => {
            let companions = crate::stale_report::stale_companions(&exe);
            crate::stale_report::merge_companions(registry, companions)
        }
        _ => registry,
    }
}

/// True when any live MCP stdio server is running a superseded binary (gap-G2
/// review finding 5: derive this from the multi-process registry, not the
/// retired single last-writer-wins stamp).
pub(super) fn mcp_binary_stale_from(stale: &[crate::binary_identity::StaleProcess]) -> bool {
    stale.iter().any(|p| p.role == "mcp_stdio")
}

/// `csr-engine status --stale` — a DB-free fast path that ONLY reads the run
/// registry (`~/.claude-self-reflect/run/*.json`) and checks each live
/// process's executable identity in one ps call: no SQLite open, no DB gather at
/// all. Prints one line per stale process, `pid <n> (<role>, started <ts>,
/// parent <p>)`; nothing when there are none.
///
/// Exists because the full `status` gather can take 8-31s on a large, busy
/// DB (measured on a 563k-chunk corpus under concurrent load) — installers
/// call `status` right after writing a fresh binary, when codesign
/// verification and a cold page cache make the first exec of that binary
/// even slower, so a short fixed timeout around the full command frequently
/// fires and the installer silently drops the stale-process report spec 2e
/// asks for (gap-G2 review finding 2). Plain per-line text rather than JSON
/// so `install.sh` (POSIX `sh`, no `jq`) can consume it without a parser,
/// and so it can never be truncated the way `grep -A3` on pretty JSON was
/// (gap-G2 review finding 3).
pub fn handle_stale() -> Result<()> {
    let run_dir = crate::binary_identity::default_run_dir();
    let stale = gather_stale_processes(run_dir.as_deref());
    let sessions = stale_sessions(&stale);
    for line in format_stale_processes_with_sessions(&stale, &sessions) {
        println!("{line}");
    }
    Ok(())
}

/// The Claude Code session of each stale process, from its parent's pid file
/// (`claude_sessions`). One `ps` call for the parents; any failure is "no session".
fn stale_sessions(
    stale: &[crate::binary_identity::StaleProcess],
) -> std::collections::HashMap<u32, String> {
    let pids: Vec<String> = stale.iter().map(|p| p.pid.to_string()).collect();
    if pids.is_empty() {
        return Default::default();
    }
    let Ok(out) = std::process::Command::new("ps")
        .args(["-o", "pid=,ppid=", "-p", &pids.join(",")])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Default::default();
    };
    let parent_of: Vec<(u32, u32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let parents: Vec<u32> = parent_of.iter().map(|(_, ppid)| *ppid).collect();
    let dir = crate::claude_sessions::sessions_dir();
    let found = crate::claude_sessions::sessions_of(dir.as_deref(), &parents);
    parent_of
        .into_iter()
        .filter_map(|(pid, ppid)| Some((pid, found.get(&ppid)?.session_id.clone())))
        .collect()
}

/// [`format_stale_processes`] with `, session <id>` inside the parentheses for
/// each process whose Claude Code session is known.
pub(super) fn format_stale_processes_with_sessions(
    stale: &[crate::binary_identity::StaleProcess],
    sessions: &std::collections::HashMap<u32, String>,
) -> Vec<String> {
    format_stale_processes(stale)
        .into_iter()
        .zip(stale)
        .map(|(line, p)| match sessions.get(&p.pid) {
            Some(id) => format!("{}, session {id})", &line[..line.len() - 1]),
            None => line,
        })
        .collect()
}

#[cfg(test)]
#[test]
fn status_stale_names_the_session_when_its_parent_s_pid_file_is_known() {
    let stale = vec![crate::binary_identity::StaleProcess {
        pid: 1234,
        role: "mcp_stdio".into(),
        started_at: "2026-09-22T10:06:00Z".into(),
        parent: Some("claude".into()),
    }];
    let sessions = std::collections::HashMap::from([(1234, "60b20311-8c1e".to_string())]);
    assert_eq!(
        format_stale_processes_with_sessions(&stale, &sessions),
        vec!["pid 1234 (mcp_stdio, started 2026-09-22T10:06:00Z, parent claude, session 60b20311-8c1e)"]
    );
    assert_eq!(
        format_stale_processes_with_sessions(&stale, &Default::default()),
        format_stale_processes(&stale)
    );
}

/// Pure formatting for [`handle_stale`]: one `pid <n> (<role>, started <ts>,
/// parent <p>)` line per entry, `parent` omitted when unknown. Extracted so
/// the exact text both installers grep/print (`installer/postinstall.js`,
/// `scripts/install.sh`) is directly unit-testable without depending on
/// `~/.claude-self-reflect/run` (this process's real, unoverridable
/// `HOME`) — see `format_stale_processes_matches_the_pid_role_started_parent_contract`.
pub(super) fn format_stale_processes(
    stale: &[crate::binary_identity::StaleProcess],
) -> Vec<String> {
    stale
        .iter()
        .map(|p| {
            format!(
                "pid {} ({}, started {}{})",
                p.pid,
                p.role,
                p.started_at,
                match &p.parent {
                    Some(parent) => format!(", parent {parent}"),
                    None => String::new(),
                }
            )
        })
        .collect()
}
