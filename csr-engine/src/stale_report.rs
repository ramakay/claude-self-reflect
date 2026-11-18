//! After `setup` and `hook install --apply` write a new binary, name every csr-engine process
//! still running the old one: pid, role, session, project, and what to do about it.
//!
//! The run registry only knows the daemon and the MCP servers, so this takes its own census: one
//! `ps` for every process whose executable is this binary's path, one `lsof` for the inode each
//! has mapped, and one `lsof` for the working folder of each parent (the Claude Code session that
//! started a relay or an MCP server). A process whose mapped inode is not the one on disk now is
//! on an older binary. A process that cannot be read is not reported: nothing is claimed without
//! evidence.
//!
//! Dormant panes are woken with `SIGWINCH`, which makes them `exec` back into the pane, now the
//! new binary. The signal goes only to a pid whose command line, read again just before, is this
//! binary running `companion pane --dormant`.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

/// One process of this binary found in the census.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Raw {
    pid: u32,
    ppid: u32,
    /// The arguments after the executable.
    args: Vec<String>,
}

/// A process on an older binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleEngine {
    pub pid: u32,
    pub role: String,
    pub session: Option<String>,
    pub project: Option<String>,
    pub dormant: bool,
}

/// What follows the executable in a `ps` args field, when that executable is this binary: its
/// exact path, or an absolute path named `csr-engine` that `same_file` says is the same file
/// (the symlink setup stores, such as `~/.local/bin/csr-engine`, is how most processes start).
fn args_after_exe<'a>(
    args: &'a str,
    exe: &str,
    same_file: &dyn Fn(&str) -> bool,
) -> Option<&'a str> {
    let args = args.trim_start();
    if let Some(tail) = args.strip_prefix(exe) {
        if tail.is_empty() || tail.starts_with(' ') {
            return Some(tail);
        }
    }
    let first = args.split(' ').next().unwrap_or("");
    let alias = Path::new(first);
    (alias.is_absolute()
        && alias.file_name().is_some_and(|n| n == "csr-engine")
        && same_file(first))
    .then(|| &args[first.len()..])
}

/// The processes in `ps -axo pid=,ppid=,args=` output whose executable is `exe`, by its own path
/// or through a name `same_file` resolves to it.
fn parse_census(ps: &str, exe: &str, same_file: &dyn Fn(&str) -> bool) -> Vec<Raw> {
    ps.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid, args) = rest.trim_start().split_once(char::is_whitespace)?;
            let tail = args_after_exe(args, exe, same_file)?;
            Some(Raw {
                pid: pid.parse().ok()?,
                ppid: ppid.parse().ok()?,
                args: tail.split_whitespace().map(str::to_owned).collect(),
            })
        })
        .collect()
}

/// `pid -> mapped inode of the executable` from `lsof -a -d txt -Fpin` output.
fn parse_txt(lsof: &str, exe: &str) -> HashMap<u32, u64> {
    let mut found = HashMap::new();
    let (mut pid, mut fd, mut inode) = (None::<u32>, String::new(), None::<u64>);
    for line in lsof.lines() {
        let (tag, value) = line.split_at(line.chars().next().map_or(0, char::len_utf8));
        match tag {
            "p" => (pid, fd, inode) = (value.parse().ok(), String::new(), None),
            "f" => (fd, inode) = (value.to_owned(), None),
            "i" => inode = value.parse().ok(),
            "n" if fd == "txt" => {
                let ours = value == exe
                    || Path::new(value)
                        .file_name()
                        .is_some_and(|n| n == "csr-engine");
                if let (true, Some(pid), Some(inode)) = (ours, pid, inode) {
                    found.entry(pid).or_insert(inode);
                }
            }
            _ => {}
        }
    }
    found
}

/// `pid -> working folder` from `lsof -a -d cwd -Fpn` output.
fn parse_cwd(lsof: &str) -> HashMap<u32, String> {
    let mut found = HashMap::new();
    let mut pid = None::<u32>;
    for line in lsof.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().ok();
        } else if let (Some(value), Some(pid)) = (line.strip_prefix('n'), pid) {
            found.entry(pid).or_insert_with(|| value.to_owned());
        }
    }
    found
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).map(String::as_str)
}

/// Global options that take a value, before the subcommand (`--db-path <db> ... daemon`: that is
/// how setup starts the daemon).
const VALUE_OPTIONS: [&str; 3] = ["--db-path", "--projects-dir", "--limit"];

/// The subcommand and what follows it: leading options, and the values of those that take one,
/// are skipped.
fn after_global_options(args: &[String]) -> &[String] {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if !arg.starts_with("--") {
            break;
        }
        at += if VALUE_OPTIONS.contains(&arg.as_str()) {
            2
        } else {
            1
        };
    }
    args.get(at..).unwrap_or(&[])
}

fn role_of(args: &[String]) -> String {
    let args = after_global_options(args);
    let first = args.first();
    match first.map(String::as_str) {
        Some("companion") => match args.iter().skip_while(|a| *a != "companion").nth(1) {
            Some(sub) if sub == "serve" => "relay".into(),
            Some(sub) if sub == "pane" => "pane".into(),
            Some(sub) => format!("companion {sub}"),
            None => "companion".into(),
        },
        Some("daemon") => "daemon".into(),
        Some("hook") => "hook".into(),
        Some(other) => other.to_owned(),
        None => "mcp".into(),
    }
}

/// The session id a relay's socket is named for: `<first 8 of the id>-<time>.sock`.
fn sock_prefix(args: &[String]) -> Option<String> {
    let name = Path::new(flag(args, "--sock")?).file_name()?.to_str()?;
    Some(name.split('-').next()?.to_owned()).filter(|p| !p.is_empty())
}

fn base(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
}

/// The processes of `raws` whose mapped inode is not `disk`. A process with no session on its
/// command line or from a relay takes its parent's Claude Code session record (`parents`).
fn stale_among(
    raws: &[Raw],
    txt: &HashMap<u32, u64>,
    cwd: &HashMap<u32, String>,
    parents: &HashMap<u32, crate::claude_sessions::ClaudeSession>,
    disk: u64,
) -> Vec<StaleEngine> {
    let full_ids: Vec<&str> = raws.iter().filter_map(|r| flag(&r.args, "--sid")).collect();
    let relay_of_parent: HashMap<u32, String> = raws
        .iter()
        .filter_map(|r| Some((r.ppid, sock_prefix(&r.args)?)))
        .collect();
    let mut out: Vec<StaleEngine> = raws
        .iter()
        .filter(|r| txt.get(&r.pid).is_some_and(|inode| *inode != disk))
        .map(|r| {
            let role = role_of(&r.args);
            let prefix = flag(&r.args, "--sid")
                .map(str::to_owned)
                .or_else(|| sock_prefix(&r.args))
                .or_else(|| {
                    (role == "mcp")
                        .then(|| relay_of_parent.get(&r.ppid).cloned())
                        .flatten()
                });
            let parent = parents.get(&r.ppid);
            let session = prefix
                .map(|p| {
                    full_ids
                        .iter()
                        .find(|id| id.starts_with(p.as_str()))
                        .map_or(p.clone(), |id| (*id).to_owned())
                })
                .or_else(|| parent.map(|s| s.session_id.clone()));
            let project = flag(&r.args, "--repo")
                .and_then(base)
                .or_else(|| cwd.get(&r.ppid).and_then(|p| base(p)))
                .or_else(|| parent.and_then(|s| s.cwd.as_deref()).and_then(base));
            StaleEngine {
                pid: r.pid,
                role: role.clone(),
                session,
                project,
                dormant: role == "pane" && r.args.iter().any(|a| a == "--dormant"),
            }
        })
        .collect();
    out.sort_by_key(|e| e.pid);
    out
}

fn text_of(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    // lsof exits 1 when some pid has nothing to list; its stdout is still the answer.
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A resolver for `args_after_exe`: is the absolute path `name` the file `exe` is?
fn same_file_as(exe: &Path) -> impl Fn(&str) -> bool {
    let canonical = exe.canonicalize().ok();
    move |name: &str| {
        canonical
            .as_ref()
            .is_some_and(|c| Path::new(name).canonicalize().is_ok_and(|n| &n == c))
    }
}

fn census(exe: &Path) -> Vec<StaleEngine> {
    let Some(exe_str) = exe.to_str() else {
        return Vec::new();
    };
    let Ok(disk) = std::fs::metadata(exe).map(|m| m.ino()) else {
        return Vec::new();
    };
    let Some(ps) = text_of("ps", &["-axo", "pid=,ppid=,args="]) else {
        return Vec::new();
    };
    let me = std::process::id();
    let raws: Vec<Raw> = parse_census(&ps, exe_str, &same_file_as(exe))
        .into_iter()
        .filter(|r| r.pid != me)
        .collect();
    if raws.is_empty() {
        return Vec::new();
    }
    let list = |ids: Vec<u32>| ids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let pids = list(raws.iter().map(|r| r.pid).collect());
    let txt = text_of("lsof", &["-a", "-d", "txt", "-p", &pids, "-Fpfin"])
        .map(|t| parse_txt(&t, exe_str))
        .unwrap_or_default();
    let mut parents: Vec<u32> = raws.iter().map(|r| r.ppid).filter(|p| *p > 1).collect();
    parents.sort_unstable();
    parents.dedup();
    let sessions_dir = crate::claude_sessions::sessions_dir();
    let sessions = crate::claude_sessions::sessions_of(sessions_dir.as_deref(), &parents);
    let cwd = if parents.is_empty() {
        HashMap::new()
    } else {
        text_of("lsof", &["-a", "-d", "cwd", "-p", &list(parents), "-Fpn"])
            .map(|t| parse_cwd(&t))
            .unwrap_or_default()
    };
    stale_among(&raws, &txt, &cwd, &sessions, disk)
}

/// Is this `ps` args line this binary running `companion pane --dormant`?
fn is_dormant_pane_line(line: &str, exe: &str, same_file: &dyn Fn(&str) -> bool) -> bool {
    args_after_exe(line.trim(), exe, same_file).is_some_and(|tail| {
        tail.starts_with(' ')
            && tail.contains(" companion pane ")
            && tail.split_whitespace().any(|a| a == "--dormant")
    })
}

/// Is `pid` still this binary running `companion pane --dormant`? Read fresh, just before a signal.
fn is_dormant_pane(pid: u32, exe: &str, same_file: &dyn Fn(&str) -> bool) -> bool {
    text_of("ps", &["-o", "args=", "-p", &pid.to_string()])
        .is_some_and(|line| is_dormant_pane_line(&line, exe, same_file))
}

/// Wake the dormant panes among `stale`; returns the pids signalled.
fn wake_dormant(stale: &[StaleEngine], exe: &Path) -> Vec<u32> {
    let Some(exe_str) = exe.to_str() else {
        return Vec::new();
    };
    let same_file = same_file_as(exe);
    stale
        .iter()
        .filter(|e| e.dormant && e.pid > 1 && is_dormant_pane(e.pid, exe_str, &same_file))
        // SAFETY: kill(2) with a pid read back as this binary's own dormant pane and SIGWINCH,
        // which that process treats as "wake and exec".
        .filter(|e| unsafe { libc::kill(e.pid as libc::pid_t, libc::SIGWINCH) } == 0)
        .map(|e| e.pid)
        .collect()
}

fn render(stale: &[StaleEngine], woken: &[u32]) -> String {
    if stale.is_empty() {
        return String::new();
    }
    let mut text = format!(
        "\nStill running the previous csr-engine ({}):\n",
        stale.len()
    );
    for e in stale {
        let action = if woken.contains(&e.pid) {
            "woken to start the new binary".to_owned()
        } else if e.role == "daemon" {
            format!(
                "restart: launchctl kickstart -k gui/<uid>/{}",
                crate::setup::DAEMON_LABEL
            )
        } else {
            "restart this session".to_owned()
        };
        text.push_str(&format!(
            "  pid {}  {}  session {}  project {}  {}\n",
            e.pid,
            e.role,
            e.session.as_deref().unwrap_or("unknown"),
            e.project.as_deref().unwrap_or("unknown"),
            action
        ));
    }
    text.push('\n');
    text
}

/// The text for the end of `setup` and `hook install --apply`: empty when nothing is on an older
/// binary. Wakes dormant panes as a side effect.
pub fn section() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return String::new();
    };
    let stale = census(&exe);
    let woken = wake_dormant(&stale, &exe);
    render(&stale, &woken)
}

/// `role` in `status` JSON for a companion pane found by the census.
pub const COMPANION_PANE_ROLE: &str = "companion_pane";
/// `role` in `status` JSON for a companion relay (`companion serve`) found by the census.
pub const COMPANION_SERVE_ROLE: &str = "companion_serve";
/// `started_at` for a census process: `ps` args carry no start time, and a guess would be a claim.
const STARTED_UNKNOWN: &str = "unknown";

/// The companion panes and relays among `stale`, as `status` reports a stale process. The daemon
/// and the MCP servers are left out: the run registry reports those. A role the registry does not
/// know gets its own name, so `mcp_binary_stale` (which looks for `mcp_stdio`) is not moved.
fn companions_of(stale: Vec<StaleEngine>) -> Vec<crate::binary_identity::StaleProcess> {
    stale
        .into_iter()
        .filter_map(|e| {
            let role = match e.role.as_str() {
                "pane" => COMPANION_PANE_ROLE,
                "relay" => COMPANION_SERVE_ROLE,
                _ => return None,
            };
            Some(crate::binary_identity::StaleProcess {
                pid: e.pid,
                role: role.to_owned(),
                started_at: STARTED_UNKNOWN.to_owned(),
                parent: None,
            })
        })
        .collect()
}

/// The stale companions in `ps` output, given each pid's mapped inode and the inode on disk.
/// A pid with no entry in `txt` is not claimed. [`census`] is these steps with the real `ps`/`lsof`
/// text; this is the same chain on fixtures.
#[cfg(test)]
fn stale_companions_from(
    ps: &str,
    exe: &str,
    same_file: &dyn Fn(&str) -> bool,
    txt: &HashMap<u32, u64>,
    disk: u64,
) -> Vec<crate::binary_identity::StaleProcess> {
    let raws = parse_census(ps, exe, same_file);
    companions_of(stale_among(
        &raws,
        txt,
        &HashMap::new(),
        &HashMap::new(),
        disk,
    ))
}

/// How long `status` waits for each of the two reads behind [`stale_companions`].
const COMPANION_READ_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What `program args` prints, or None when it cannot start or has not finished within `limit`.
/// On the limit the child it started is killed by its own handle and reaped, so nothing is left
/// running. A reader thread drains stdout, so a long answer cannot fill the pipe and stall the
/// child.
fn text_within(program: &str, args: &[&str], limit: std::time::Duration) -> Option<String> {
    use std::io::Read;
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    let deadline = std::time::Instant::now() + limit;
    match rx.recv_timeout(limit) {
        // End of output is not the end of the process: it can close stdout and keep running.
        Ok(bytes) if exited_by(&mut child, deadline) => {
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        Ok(_) => None,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Whether `child` exits by `deadline`, polled with `try_wait`. When it does not, it is killed
/// by its own handle and reaped here, so no zombie is left, and the answer is false.
fn exited_by(child: &mut std::process::Child, deadline: std::time::Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The companion-only census behind `status`: one `ps`, then one `lsof -d txt` for the panes and
/// relays it found, each read bounded by [`COMPANION_READ_LIMIT`]. No parent, working-folder or
/// session lookup. A read that times out reports nothing for companions.
fn companion_census(exe: &Path) -> Vec<StaleEngine> {
    let Some(exe_str) = exe.to_str() else {
        return Vec::new();
    };
    let Ok(disk) = std::fs::metadata(exe).map(|m| m.ino()) else {
        return Vec::new();
    };
    let Some(ps) = text_within("ps", &["-axo", "pid=,ppid=,args="], COMPANION_READ_LIMIT) else {
        return Vec::new();
    };
    let me = std::process::id();
    let raws: Vec<Raw> = parse_census(&ps, exe_str, &same_file_as(exe))
        .into_iter()
        .filter(|r| r.pid != me && matches!(role_of(&r.args).as_str(), "pane" | "relay"))
        .collect();
    if raws.is_empty() {
        return Vec::new();
    }
    let pids = raws
        .iter()
        .map(|r| r.pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let Some(lsof) = text_within(
        "lsof",
        &["-a", "-d", "txt", "-p", &pids, "-Fpfin"],
        COMPANION_READ_LIMIT,
    ) else {
        return Vec::new();
    };
    let txt = parse_txt(&lsof, exe_str);
    stale_among(&raws, &txt, &HashMap::new(), &HashMap::new(), disk)
}

/// Companion panes and relays still on a superseded binary. Read-only: one `ps` and one `lsof`,
/// each with a deadline, no signal sent (unlike [`section`], which wakes dormant panes).
pub fn stale_companions(exe: &Path) -> Vec<crate::binary_identity::StaleProcess> {
    companions_of(companion_census(exe))
}

/// `registry` first, then the `companions` it does not already list.
pub fn merge_companions(
    mut registry: Vec<crate::binary_identity::StaleProcess>,
    companions: Vec<crate::binary_identity::StaleProcess>,
) -> Vec<crate::binary_identity::StaleProcess> {
    let known: std::collections::HashSet<u32> = registry.iter().map(|p| p.pid).collect();
    registry.extend(companions.into_iter().filter(|p| !known.contains(&p.pid)));
    registry
}

#[cfg(test)]
#[path = "stale_report_companion_tests.rs"]
mod companion_tests;

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/usr/local/bin/csr-engine";

    fn ps() -> String {
        [
            "11792     1 /usr/local/bin/csr-engine daemon",
            "26149 26135 /usr/local/bin/csr-engine",
            "26208 26135 /usr/local/bin/csr-engine companion serve --sock /h/run/0739ec61-mwaa49xt.sock --relay 10.1.0",
            "44101 43668 /usr/local/bin/csr-engine companion pane --repo /p/anukriti-website --watch --sid e22fe55d-a6d6-40a9-b72c-040597c48324 --agent w2:p1 --dormant",
            "43500 44522 /usr/local/bin/csr-engine companion serve --sock /h/run/e22fe55d-mwb0r536.sock --relay 10.1.0",
            "500 1 /usr/local/bin/csr-engine-other daemon",
            "501 1 /tmp/target/csr-engine daemon",
            "502 1 /bin/zsh -c /usr/local/bin/csr-engine daemon",
        ]
        .join("\n")
    }

    #[test]
    fn the_census_keeps_only_this_binarys_processes() {
        let raws = parse_census(&ps(), EXE, &|_| false);
        let pids: Vec<u32> = raws.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![11792, 26149, 26208, 44101, 43500]);
        assert_eq!(raws[1].args, Vec::<String>::new());
    }

    #[test]
    fn a_process_started_through_a_symlink_to_this_binary_is_in_the_census() {
        let lines = [
            "700 1 /Users/me/.local/bin/csr-engine --db-path /h/csr.db --projects-dir /h/p daemon",
            "701 1 /tmp/target/csr-engine daemon",
            "702 1 relative/csr-engine daemon",
        ]
        .join("\n");
        let alias = |name: &str| name == "/Users/me/.local/bin/csr-engine";
        let raws = parse_census(&lines, EXE, &alias);
        assert_eq!(raws.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![700]);
        assert_eq!(role_of(&raws[0].args), "daemon");
        // The same name is what lets a dormant pane be woken.
        let pane = "/Users/me/.local/bin/csr-engine companion pane --repo /p/x --dormant";
        assert!(is_dormant_pane_line(pane, EXE, &alias));
        assert!(!is_dormant_pane_line(pane, EXE, &|_| false));
    }

    #[test]
    fn the_daemon_setup_starts_with_global_options_is_a_daemon() {
        let args = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            role_of(&args(
                "--db-path /h/.claude-self-reflect/csr-engine.db --projects-dir /h/.claude/projects daemon"
            )),
            "daemon"
        );
        assert_eq!(role_of(&args("--db-path=/h/csr.db daemon")), "daemon");
        assert_eq!(role_of(&args("--db-path /h/csr.db")), "mcp");
        assert_eq!(
            role_of(&args("--db-path /h/csr.db companion serve --sock /s")),
            "relay"
        );
    }

    #[test]
    fn roles_sessions_and_projects_are_read_from_the_command_line() {
        let raws = parse_census(&ps(), EXE, &|_| false);
        let txt: HashMap<u32, u64> = raws.iter().map(|r| (r.pid, 7)).collect();
        let cwd = HashMap::from([(26135, "/Users/me/projects/csr".to_owned())]);
        let stale = stale_among(&raws, &txt, &cwd, &HashMap::new(), 9);
        let by = |pid: u32| stale.iter().find(|e| e.pid == pid).unwrap();
        assert_eq!(by(11792).role, "daemon");
        assert_eq!(by(26149).role, "mcp");
        assert_eq!(by(26149).session.as_deref(), Some("0739ec61"));
        assert_eq!(by(26149).project.as_deref(), Some("csr"));
        assert_eq!(by(26208).role, "relay");
        assert_eq!(by(44101).role, "pane");
        assert!(by(44101).dormant);
        assert_eq!(
            by(44101).session.as_deref(),
            Some("e22fe55d-a6d6-40a9-b72c-040597c48324")
        );
        assert_eq!(by(44101).project.as_deref(), Some("anukriti-website"));
        // The relay's short id is widened by the pane that carries the full one.
        assert_eq!(
            by(43500).session.as_deref(),
            Some("e22fe55d-a6d6-40a9-b72c-040597c48324")
        );
    }

    #[test]
    fn only_a_different_inode_is_stale_and_an_unreadable_one_is_not_claimed() {
        let raws = parse_census(&ps(), EXE, &|_| false);
        let txt = HashMap::from([(11792, 9), (26149, 7)]);
        let stale = stale_among(&raws, &txt, &HashMap::new(), &HashMap::new(), 9);
        assert_eq!(stale.iter().map(|e| e.pid).collect::<Vec<_>>(), vec![26149]);
    }

    #[test]
    fn an_mcp_server_with_no_relay_takes_its_parent_s_claude_code_session() {
        let sessions = tempfile::tempdir().unwrap();
        std::fs::write(
            sessions.path().join("5507.json"),
            r#"{"pid":5507,"sessionId":"60b20311-8c1e-4f43-9f0e-2b6a7d1c9e55","cwd":"/Users/me/projects/anukriti"}"#,
        )
        .unwrap();
        let lines = [
            "5600 5507 /usr/local/bin/csr-engine",
            "5601 5508 /usr/local/bin/csr-engine",
        ]
        .join("\n");
        let raws = parse_census(&lines, EXE, &|_| false);
        let txt: HashMap<u32, u64> = raws.iter().map(|r| (r.pid, 7)).collect();
        let parents = crate::claude_sessions::sessions_of(Some(sessions.path()), &[5507, 5508]);
        let stale = stale_among(&raws, &txt, &HashMap::new(), &parents, 9);
        let text = render(&stale, &[]);
        assert!(
            text.contains(
                "pid 5600  mcp  session 60b20311-8c1e-4f43-9f0e-2b6a7d1c9e55  project anukriti"
            ),
            "{text}"
        );
        // No pid file for the other parent: unknown, not an error.
        assert!(
            text.contains("pid 5601  mcp  session unknown  project unknown"),
            "{text}"
        );
    }

    #[test]
    fn lsof_output_is_read_per_process() {
        let txt = "p11792\nftxt\ni42\nn/usr/local/bin/csr-engine\nftxt\ni99\nn/usr/lib/dyld\np26149\nfcwd\ni5\nn/tmp\nftxt\ni43\nn/usr/local/bin/csr-engine\n";
        let found = parse_txt(txt, EXE);
        assert_eq!(found, HashMap::from([(11792, 42), (26149, 43)]));
        let cwd = parse_cwd("p26135\nfcwd\nn/Users/me/projects/csr\n");
        assert_eq!(cwd[&26135], "/Users/me/projects/csr");
    }

    #[test]
    fn the_report_names_pid_role_session_project_and_the_action() {
        let stale = vec![
            StaleEngine {
                pid: 26149,
                role: "mcp".into(),
                session: Some("0739ec61".into()),
                project: Some("csr".into()),
                dormant: false,
            },
            StaleEngine {
                pid: 44101,
                role: "pane".into(),
                session: None,
                project: None,
                dormant: true,
            },
        ];
        let text = render(&stale, &[44101]);
        assert!(text.contains("previous csr-engine (2)"), "{text}");
        assert!(
            text.contains("pid 26149  mcp  session 0739ec61  project csr  restart this session"),
            "{text}"
        );
        assert!(
            text.contains(
                "pid 44101  pane  session unknown  project unknown  woken to start the new binary"
            ),
            "{text}"
        );
        assert_eq!(render(&[], &[]), "");
    }
}
