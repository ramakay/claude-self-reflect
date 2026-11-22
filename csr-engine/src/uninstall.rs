//! `csr-engine uninstall [--dry-run] [--yes] [--purge] [--purge-mirror]`
//!
//! Takes back what `setup` and `hook install --apply` wrote, and only that:
//!
//! 1. CSR's hooks in `~/.claude/settings.json` (a hook is CSR's when it runs a `csr-engine hook`
//!    command; every other hook, matcher and key stays; a backup is written first and the file
//!    is replaced whole, so it is valid JSON after or untouched),
//! 2. the `claude-self-reflect` MCP entry in `~/.claude.json` (through `claude mcp remove`, and
//!    by hand only if the CLI left it; the file is read again right before the edit), and any
//!    project-scoped entry of that name (`-s local`, run from the project folder),
//! 3. the same entry in Codex's config (through `codex mcp remove`, config backed up first),
//! 4. the companion plugin and its marketplace (through `claude plugin ...`), then the plugin
//!    cache `~/.claude/plugins/cache/claude-self-reflect`,
//! 5. the launch agent setup wrote (a plist without setup's marker is listed and left alone):
//!    booted out, then its plist deleted at once, so nothing starts the daemon again at login,
//!    then a bounded wait for the daemon to exit (launchd's `ExitTimeOut` plus a margin); a
//!    daemon still running then gets one SIGTERM, sent only to the pid both launchd and
//!    `daemon.lock` name. Nothing after this step is deleted while the daemon is not confirmed
//!    stopped, and the last line says what is left and the command that finishes,
//! 6. the npm global package `claude-self-reflect`, when `npm ls -g` lists it,
//! 7. the data folder `~/.claude-self-reflect`: KEPT by default (its path and size are printed);
//!    `--purge` deletes it except the transcript mirror, `--purge-mirror` (implies `--purge`)
//!    deletes the mirror too. `--keep-data` is still accepted and does nothing,
//! 8. the model cache `~/Library/Caches/csr-engine` (downloaded models, not user data, so it is
//!    removed without `--purge`),
//! 9. last, every csr-engine binary the hooks, MCP entries, launch agent and this process name:
//!    one in `~/.local/bin` is deleted, any other gets its exact `rm` line printed.
//!
//! Every step prints what it removed or kept. The running csr-engine processes (sessions, relays)
//! are listed at the end and never signalled (only the launchd daemon of step 5 can be): they
//! recreate the data folder until restarted.
//!
//! Never touched: `~/.claude/projects`, any other agent, any other hook. Without `--yes` (or with
//! `--dry-run`) nothing changes, no program that changes anything is run (the plan reads files
//! and runs only the read-only probes `npm ls` and `ps`), and the plan is printed.

#[path = "uninstall_residue.rs"]
mod residue;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::companion::relay::emit;
use crate::hooks::statusline_retire;
use crate::setup::{
    daemon_agents_on_disk, daemon_lock_holder, daemon_stop_wait as stop_wait, DAEMON_LABEL,
    DAEMON_PLIST_MARKER,
};

const MCP_NAME: &str = "claude-self-reflect";
/// How often the wait for the daemon asks launchd again.
const STOP_POLL: Duration = Duration::from_millis(200);
/// How long the daemon gets after uninstall's own SIGTERM.
const SIGTERM_GRACE: Duration = Duration::from_secs(5);
const DATA_DIR: &str = ".claude-self-reflect";
const CONFIG_CAP: u64 = 32 * 1024 * 1024;
const OUTPUT_CAP: usize = 1024 * 1024;
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(60);
const CODEX_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(clap::Args, Debug, Clone)]
pub struct UninstallArgs {
    /// Print what would be removed and change nothing (also the default without --yes)
    #[arg(long)]
    pub dry_run: bool,
    /// Do it. Without this flag the plan is printed and nothing changes
    #[arg(long)]
    pub yes: bool,
    /// Also delete the data folder ~/.claude-self-reflect, except the transcript mirror. Without
    /// this flag the data is kept and its path and size are printed
    #[arg(long)]
    pub purge: bool,
    /// Also delete the transcript mirror (the only copy of transcripts Claude Code deleted);
    /// implies --purge
    #[arg(long)]
    pub purge_mirror: bool,
    /// Accepted for old command lines; does nothing, because the data is kept by default now
    #[arg(long, hide = true, conflicts_with_all = ["purge", "purge_mirror"])]
    pub keep_data: bool,
}

impl UninstallArgs {
    /// True when the data folder is to be deleted (`--purge`, or `--purge-mirror` which implies it).
    fn purges(&self) -> bool {
        self.purge || self.purge_mirror
    }
}

/// Entry point for the subcommand. Returns the process exit code. `db_path` is the global
/// `--db-path`: uninstall only knows the default data folder, so any other value is refused
/// before anything is read or changed, instead of being ignored.
pub fn run(args: &UninstallArgs, db_path: &Path) -> i32 {
    let Some(home) = dirs::home_dir().filter(|h| h.is_absolute()) else {
        eprintln!("csr-engine uninstall: cannot determine the home directory");
        return 1;
    };
    let default_db = home.join(DATA_DIR).join("csr-engine.db");
    if db_path != default_db {
        eprintln!(
            "csr-engine uninstall: --db-path {} is not the default {}. Uninstall removes only the \
             default data folder and does not honour --db-path; nothing was changed. Remove the \
             files of a custom location by hand",
            db_path.display(),
            default_db.display()
        );
        return 2;
    }
    let apply = args.yes && !args.dry_run;
    let mut out = Vec::new();
    let mut ok = true;
    // Read before the steps below remove the hooks, MCP entries and launch agent that name them.
    let binaries = residue::registered_binaries(&home, std::env::current_exe().ok());
    ok &= settings_step(&home, apply, &mut out);
    ok &= mcp_step(&home, apply, &mut out);
    ok &= residue::local_mcp_step(&home, apply, &mut out);
    ok &= codex_step(&home, apply, &mut out);
    let plugin_gone = plugin_step(apply, &mut out);
    ok &= plugin_gone;
    ok &= residue::plugin_cache_step(&home, apply, plugin_gone, &mut out);
    let mut agents = agent_step(&home, apply, &mut out);
    ok &= agents.ok;
    // The plist is not the daemon: whatever happened to it (deleted by an earlier run, never
    // there, not ours), nothing is deleted while launchd runs a CSR daemon.
    let mut hold_data = agents.hold_data;
    if apply && !hold_data && daemon_still_loaded(&home, &mut agents, &mut out) {
        hold_data = true;
        ok = false;
    }
    ok &= residue::npm_step(apply, hold_data, &mut out);
    if hold_data && args.purges() {
        out.push(
            "data: kept, because the daemon is not confirmed stopped. Stop it, then run \
             `csr-engine uninstall --yes --purge` again"
                .into(),
        );
    } else {
        ok &= data_step(&home, apply, args, &mut out);
    }
    ok &= cache_step(apply, &mut out);
    let processes = residue::process_report(
        &binaries,
        &home.join(DATA_DIR),
        args.purges() && !hold_data,
        agents.signalled,
    );
    // Last: nothing after this may need the binary.
    ok &= binary_step(&home, apply, hold_data, &binaries, &mut out);
    if apply && hold_data {
        out.push(finish_line(args, agents.hold));
    }
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    let _ = writeln!(
        w,
        "csr-engine uninstall{}",
        if apply {
            ""
        } else {
            " (plan only, nothing changed)"
        }
    );
    for line in &out {
        let _ = writeln!(w, "  {line}");
    }
    for line in &processes {
        let _ = writeln!(w, "{line}");
    }
    if !apply && !args.dry_run {
        let _ = writeln!(w, "Run again with --yes to do this.");
    }
    if apply {
        let _ = writeln!(
            w,
            "Open Claude Code sessions keep their old MCP server and hooks until restarted."
        );
    }
    if ok {
        0
    } else {
        1
    }
}

/// The last line of a run that left things behind because the daemon is not confirmed stopped:
/// what to wait for, then the exact command that removes the rest. With no GUI domain and only a
/// daemon outside launchd to wait for, `launchctl print` never says "cannot find the service", so
/// the line names the daemon instead.
fn finish_line(args: &UninstallArgs, hold: Option<Hold>) -> String {
    let exe = std::env::current_exe()
        .map(|p| format!("\"{}\"", p.display()))
        .unwrap_or_else(|_| "csr-engine".to_string());
    let purge = if args.purge_mirror {
        " --purge-mirror"
    } else if args.purge {
        " --purge"
    } else {
        ""
    };
    if let Some(Hold::OutsideDaemon(pid)) = hold {
        let daemon = match pid {
            Some(pid) => format!("the csr-engine daemon (pid {pid})"),
            None => "the csr-engine daemon".to_string(),
        };
        return format!(
            "not finished: what the lines above keep stays until the daemon has stopped. Stop \
             {daemon}, then finish with: {exe} uninstall --yes{purge}"
        );
    }
    let job = format!("gui/{}/{DAEMON_LABEL}", unsafe { libc::getuid() });
    format!(
        "not finished: what the lines above keep stays until the daemon has stopped. Once \
         `launchctl print {job}` says it cannot find the service, finish with: {exe} uninstall \
         --yes{purge}"
    )
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bytes under `path`, never following a link.
fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        std::fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        meta.len()
    }
}

fn human(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GiB", mb / 1024.0)
    } else {
        format!("{mb:.1} MiB")
    }
}

/// Copy `text` to a new `<file>.csr-uninstall-<secs>` beside `file`, owner-only. Created
/// exclusively, so an earlier backup is never overwritten and a link at the name is not followed.
fn write_backup(file: &Path, text: &str) -> Result<PathBuf, String> {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let secs = unix_secs();
    for n in 0..100u32 {
        let suffix = if n == 0 {
            format!("{name}.csr-uninstall-{secs}")
        } else {
            format!("{name}.csr-uninstall-{secs}-{n}")
        };
        let path = file.with_file_name(suffix);
        match create_private(&path) {
            Ok(mut f) => {
                return f
                    .write_all(text.as_bytes())
                    .map(|_| path.clone())
                    .map_err(|e| format!("could not write backup {}: {e}", path.display()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write backup {}: {e}", path.display())),
        }
    }
    Err(format!(
        "could not pick a free backup name beside {}",
        file.display()
    ))
}

/// A new file at `path`, owner-only. Fails when anything is there already, a link included:
/// the open is exclusive and never follows a link at the final name.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Write `body` to a fresh temp file beside `target`, with `target`'s permissions. The name is
/// never reused (exclusive create, retried under a new name), so nothing planted there is written.
fn write_temp(target: &Path, body: &str) -> Result<PathBuf, String> {
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for n in 0..100u32 {
        let tmp = target.with_file_name(format!(
            ".{name}.csr-uninstall-{}-{stamp}-{n}.tmp",
            std::process::id()
        ));
        match create_private(&tmp) {
            Ok(mut f) => {
                let written = (|| {
                    if let Ok(meta) = std::fs::metadata(target) {
                        f.set_permissions(meta.permissions())?;
                    }
                    f.write_all(body.as_bytes())?;
                    f.sync_all()
                })();
                return match written {
                    Ok(()) => Ok(tmp),
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        Err(format!("could not write {}: {e}", tmp.display()))
                    }
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not write {}: {e}", tmp.display())),
        }
    }
    Err(format!(
        "could not pick a free temp name beside {}",
        target.display()
    ))
}

/// Read a JSON config file, capped. `Ok(None)` when it does not exist.
fn read_config(path: &Path) -> Result<Option<(String, Value)>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    if !meta.is_file() || meta.len() > CONFIG_CAP {
        return Err(format!(
            "{} is not a regular file under 32 MiB",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON ({e}); left untouched", path.display()))?;
    Ok(Some((text, value)))
}

/// What `edit_json` did.
enum Edited {
    /// The file does not exist.
    Missing,
    /// The edit changed nothing, so nothing was written.
    Unchanged,
    /// The file was replaced; the backup of what it held is here.
    Written(PathBuf),
}

/// Read `path` fresh, let `edit` change the JSON (it returns false when there is nothing to
/// change), back the file up and replace it. A link is followed, so a dotfiles link keeps
/// pointing where it did. The file is read once more right before the rename: if something wrote
/// to it meanwhile (Claude Code rewrites `~/.claude.json` all the time) the edit starts again
/// from what is there now, so a newer write is never overwritten with an older copy.
fn edit_json(path: &Path, mut edit: impl FnMut(&mut Value) -> bool) -> Result<Edited, String> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for _ in 0..5 {
        let Some((text, mut value)) = read_config(&real)? else {
            return Ok(Edited::Missing);
        };
        if !edit(&mut value) {
            return Ok(Edited::Unchanged);
        }
        let body = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        let backup = write_backup(path, &text)?;
        let tmp = match write_temp(&real, &body) {
            Ok(tmp) => tmp,
            Err(e) => {
                let _ = std::fs::remove_file(&backup);
                return Err(e);
            }
        };
        if std::fs::read(&real).is_ok_and(|now| now == text.as_bytes()) {
            return match std::fs::rename(&tmp, &real) {
                Ok(()) => Ok(Edited::Written(backup)),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    let _ = std::fs::remove_file(&backup);
                    Err(format!("could not replace {}: {e}", real.display()))
                }
            };
        }
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&backup);
    }
    Err(format!(
        "{} kept changing while it was edited; nothing was written, run again",
        path.display()
    ))
}

/// Remove CSR's hooks from `settings` and put a retired status line wrapper back. Returns the
/// hooks removed and the status line outcome.
fn strip_settings(settings: &mut Value, home: &Path) -> (usize, statusline_retire::Outcome) {
    let removed = crate::hooks::install::remove_csr_hooks(settings);
    (removed, statusline_retire::restore_in_place(settings, home))
}

fn settings_step(home: &Path, apply: bool, out: &mut Vec<String>) -> bool {
    let path = home.join(".claude").join("settings.json");
    let (removed, outcome) = if apply {
        let mut tally = (0, statusline_retire::Outcome::NotWrapped);
        let edited = edit_json(&path, |settings| {
            tally = strip_settings(settings, home);
            tally.0 > 0 || matches!(tally.1, statusline_retire::Outcome::Restored(_))
        });
        match edited {
            Ok(Edited::Missing) => {
                out.push("hooks: no settings.json, nothing to remove".into());
                return true;
            }
            Ok(Edited::Written(backup)) => {
                out.extend(statusline_retire::report(&tally.1, &path, false));
                out.push(format!(
                    "hooks: removed {} CSR hook(s) from {} (backup {})",
                    tally.0,
                    path.display(),
                    backup.display()
                ));
                return true;
            }
            Ok(Edited::Unchanged) => tally,
            Err(e) => {
                out.push(format!("hooks: {e}; settings.json not changed"));
                return false;
            }
        }
    } else {
        match read_config(&path) {
            Ok(Some((_, mut settings))) => strip_settings(&mut settings, home),
            Ok(None) => {
                out.push("hooks: no settings.json, nothing to remove".into());
                return true;
            }
            Err(e) => {
                out.push(format!("hooks: {e}"));
                return false;
            }
        }
    };
    out.extend(statusline_retire::report(&outcome, &path, !apply));
    if removed == 0 && !matches!(outcome, statusline_retire::Outcome::Restored(_)) {
        out.push(format!("hooks: none of CSR's in {}", path.display()));
    } else {
        out.push(format!(
            "hooks: would remove {removed} CSR hook(s) from {} (backup first)",
            path.display()
        ));
    }
    true
}

/// What a finished child printed.
struct Run {
    success: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn first_stderr_line(&self) -> String {
        self.stderr.lines().next().unwrap_or("").to_string()
    }
    fn says(&self, needle: &str) -> bool {
        self.stderr.contains(needle) || self.stdout.contains(needle)
    }
}

/// Read a pipe on its own thread so a chatty child never blocks on a full pipe. Keeps the first
/// megabyte and discards the rest.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut pipe) = pipe {
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if kept.len() < OUTPUT_CAP {
                    kept.extend_from_slice(&buf[..n]);
                }
            }
        }
        let _ = tx.send(kept);
    });
    rx
}

/// Run `program args` with a stopwatch, stdin closed and output captured. Err when it could not
/// start (not installed) or had to be stopped. Only the child this function started is killed.
fn run_cmd(program: &str, args: &[&str], limit: Duration) -> Result<Run, String> {
    run_cmd_in(program, args, limit, None)
}

/// [`run_cmd`] with `cwd` as the child's working folder. A folder that does not exist is an Err
/// (the child cannot start), like a missing program.
fn run_cmd_in(
    program: &str,
    args: &[&str],
    limit: Duration,
    cwd: Option<&Path>,
) -> Result<Run, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} {} did not finish in time",
                    args.join(" ")
                ));
            }
            Err(e) => return Err(format!("could not wait for {program}: {e}")),
        }
    };
    let grace = Duration::from_secs(2);
    let text = |rx: Receiver<Vec<u8>>| {
        String::from_utf8_lossy(&rx.recv_timeout(grace).unwrap_or_default()).into_owned()
    };
    Ok(Run {
        success: status.success(),
        code: status.code(),
        stdout: text(out_rx),
        stderr: text(err_rx),
    })
}

fn mcp_entry_is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(command_is_csr_engine)
}

fn command_is_csr_engine(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .is_some_and(|n| n == "csr-engine" || n == "csr-engine.exe")
}

/// `Some(entry)` when `config` holds a `claude-self-reflect` server.
fn mcp_entry(config: &Value) -> Option<&Value> {
    config.get("mcpServers").and_then(|m| m.get(MCP_NAME))
}

fn mcp_step(home: &Path, apply: bool, out: &mut Vec<String>) -> bool {
    let path = home.join(".claude.json");
    let config = match read_config(&path) {
        Ok(Some((_, config))) => config,
        Ok(None) => {
            out.push("mcp: no ~/.claude.json, nothing to remove".into());
            return true;
        }
        Err(e) => {
            out.push(format!("mcp: {e}"));
            return false;
        }
    };
    let Some(entry) = mcp_entry(&config) else {
        out.push(format!("mcp: no '{MCP_NAME}' entry in {}", path.display()));
        return true;
    };
    if !mcp_entry_is_ours(entry) {
        out.push(format!(
            "mcp: the '{MCP_NAME}' entry does not run csr-engine; left in place"
        ));
        return true;
    }
    if !apply {
        out.push(format!(
            "mcp: would run `claude mcp remove {MCP_NAME} -s user` ({})",
            path.display()
        ));
        return true;
    }
    let cli = run_cmd(
        "claude",
        &["mcp", "remove", MCP_NAME, "-s", "user"],
        CLAUDE_TIMEOUT,
    );
    // The CLI's exit code is not trusted: the file says whether the entry is gone. A file that
    // cannot be read or parsed says nothing, and is never read as "gone".
    match read_config(&path) {
        Err(e) => {
            out.push(format!(
                "mcp: could not confirm the '{MCP_NAME}' entry is gone ({e}); check it with \
                 `claude mcp list`"
            ));
            return false;
        }
        Ok(None) => {
            out.push(format!(
                "mcp: removed the '{MCP_NAME}' entry (claude mcp remove)"
            ));
            return true;
        }
        Ok(Some((_, now))) if mcp_entry(&now).is_none() => {
            out.push(format!(
                "mcp: removed the '{MCP_NAME}' entry (claude mcp remove)"
            ));
            return true;
        }
        Ok(Some(_)) => {}
    }
    // The CLI is missing, failed, or left it. Edit the file as it is now (not as it was before
    // the CLI ran), take only CSR's key, back up first.
    let why = match cli {
        Ok(run) if run.success => "the CLI left it".to_string(),
        Ok(run) => format!("claude mcp remove failed: {}", run.first_stderr_line()),
        Err(e) => e,
    };
    let edited = edit_json(&path, |config| {
        if !mcp_entry(config).is_some_and(mcp_entry_is_ours) {
            return false;
        }
        config
            .get_mut("mcpServers")
            .and_then(Value::as_object_mut)
            .is_some_and(|servers| servers.remove(MCP_NAME).is_some())
    });
    match edited {
        Ok(Edited::Written(backup)) => {
            out.push(format!(
                "mcp: removed the '{MCP_NAME}' entry from {} by hand ({why}; backup {})",
                path.display(),
                backup.display()
            ));
            true
        }
        Ok(Edited::Unchanged | Edited::Missing) => {
            out.push(format!(
                "mcp: the '{MCP_NAME}' entry is gone or no longer runs csr-engine; left alone"
            ));
            true
        }
        Err(e) => {
            out.push(format!("mcp: {e}; entry left in place ({why})"));
            false
        }
    }
}

/// Codex's config file: `$CODEX_HOME/config.toml`, else `<home>/.codex/config.toml`.
fn codex_config(home: &Path) -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("config.toml"),
        None => home.join(".codex/config.toml"),
    }
}

/// The `command` of the `claude-self-reflect` table in Codex's config, if there is one.
fn codex_entry_in_config(config: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    let entry = doc.get("mcp_servers")?.get(MCP_NAME)?;
    Some(
        entry
            .get("command")
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_string(),
    )
}

/// The stdio command of a `codex mcp get --json` answer.
fn codex_get_command(stdout: &str) -> Option<String> {
    let parsed = |text: &str| serde_json::from_str::<Value>(text.trim()).ok();
    let entry = parsed(stdout).or_else(|| stdout.lines().find_map(parsed))?;
    entry
        .get("transport")?
        .get("command")?
        .as_str()
        .map(str::to_string)
}

const CODEX_ABSENT: &str = "No MCP server named";

fn codex_step(home: &Path, apply: bool, out: &mut Vec<String>) -> bool {
    let config = codex_config(home);
    let in_config = codex_entry_in_config(&config);
    let remove_cmd = format!("codex mcp remove {MCP_NAME}");
    if !apply {
        match in_config {
            Some(cmd) if command_is_csr_engine(&cmd) => out.push(format!(
                "codex: would run `{remove_cmd}` ({}; config backed up first)",
                config.display()
            )),
            Some(_) => out.push(format!(
                "codex: the '{MCP_NAME}' entry in {} does not run csr-engine; left in place",
                config.display()
            )),
            None => {}
        }
        return true;
    }
    let get = || run_cmd("codex", &["mcp", "get", MCP_NAME, "--json"], CODEX_TIMEOUT);
    let current = match get() {
        Err(e) => {
            // The config registers CSR and nothing can take it out: that is not a clean uninstall.
            if in_config.as_deref().is_some_and(command_is_csr_engine) {
                out.push(format!(
                    "codex: {} registers '{MCP_NAME}' and `codex` could not be run ({e}); NOT \
                     removed. Run `{remove_cmd}` once Codex is back",
                    config.display()
                ));
                return false;
            }
            return true;
        }
        Ok(run) if run.success => codex_get_command(&run.stdout),
        Ok(run) if run.says(CODEX_ABSENT) => return true,
        Ok(run) => {
            out.push(format!(
                "codex: `codex mcp get` failed ({}); entry NOT removed",
                run.first_stderr_line()
            ));
            return false;
        }
    };
    if !current.as_deref().is_some_and(command_is_csr_engine) {
        out.push(format!(
            "codex: the '{MCP_NAME}' entry does not run csr-engine; left in place"
        ));
        return true;
    }
    let backup = match std::fs::read_to_string(&config) {
        Ok(text) => match write_backup(&config, &text) {
            Ok(path) => Some(path),
            Err(e) => {
                out.push(format!("codex: {e}; entry left in place"));
                return false;
            }
        },
        Err(_) => None,
    };
    let kept = backup
        .map(|b| format!("; backup {}", b.display()))
        .unwrap_or_default();
    let removed = run_cmd("codex", &["mcp", "remove", MCP_NAME], CODEX_TIMEOUT);
    match (removed, get()) {
        (_, Ok(after)) if !after.success && after.says(CODEX_ABSENT) => {
            out.push(format!("codex: removed the '{MCP_NAME}' entry{kept}"));
            true
        }
        (removed, _) => {
            let why = match removed {
                Ok(run) if run.success => "the entry is still there afterwards".to_string(),
                Ok(run) => run.first_stderr_line(),
                Err(e) => e,
            };
            out.push(format!(
                "codex: `{remove_cmd}` did not remove the entry ({why}); run it by hand"
            ));
            false
        }
    }
}

/// Remove the companion plugin and its marketplace, then ask Claude Code whether the plugin is
/// still installed: a failed or silent `claude plugin` call is not proof of either.
fn plugin_step(apply: bool, out: &mut Vec<String>) -> bool {
    let id = format!("{}@{}", emit::PLUGIN_NAME, emit::MARKETPLACE_NAME);
    let steps: [Vec<&str>; 2] = [
        vec!["plugin", "uninstall", id.as_str()],
        vec!["plugin", "marketplace", "remove", emit::MARKETPLACE_NAME],
    ];
    for args in steps {
        let shown = format!("claude {}", args.join(" "));
        if !apply {
            out.push(format!("plugin: would run `{shown}`"));
            continue;
        }
        match run_cmd("claude", &args, CLAUDE_TIMEOUT) {
            Ok(run) if run.success => out.push(format!("plugin: ran `{shown}`")),
            Ok(run) => out.push(format!(
                "plugin: `{shown}` did not succeed ({})",
                run.first_stderr_line()
            )),
            Err(e) => out.push(format!("plugin: {e}; run `{shown}` by hand")),
        }
    }
    if !apply {
        return true;
    }
    let plugin_gone = match run_cmd("claude", &["plugin", "list", "--json"], CLAUDE_TIMEOUT) {
        Ok(run) if run.success && run.stdout.contains(&id) => {
            out.push(format!(
                "plugin: {id} is still installed; run `claude plugin uninstall {id}` by hand"
            ));
            false
        }
        Ok(run) if run.success => true,
        Ok(run) => {
            out.push(format!(
                "plugin: could not confirm the plugin is gone ({}); check `claude plugin list`",
                run.first_stderr_line()
            ));
            false
        }
        // No `claude` to ask (or it hung): the removal is unverified, so it is not success.
        Err(e) => {
            out.push(format!(
                "plugin: could not confirm the plugin is gone ({e}); check `claude plugin list`"
            ));
            false
        }
    };
    // Both are asked, so both outcomes are printed.
    let market_gone = marketplace_gone(out);
    plugin_gone && market_gone
}

/// Ask Claude Code whether CSR's marketplace is still registered. True when it is gone.
fn marketplace_gone(out: &mut Vec<String>) -> bool {
    let name = emit::MARKETPLACE_NAME;
    match run_cmd("claude", &["plugin", "marketplace", "list"], CLAUDE_TIMEOUT) {
        Ok(run) if run.success && run.stdout.contains(name) => {
            out.push(format!(
                "plugin: marketplace {name} is still registered; run `claude plugin marketplace \
                 remove {name}` by hand"
            ));
            false
        }
        Ok(run) if run.success => true,
        Ok(run) => {
            out.push(format!(
                "plugin: could not confirm marketplace {name} is gone ({}); check `claude plugin \
                 marketplace list`",
                run.first_stderr_line()
            ));
            false
        }
        Err(e) => {
            out.push(format!(
                "plugin: could not confirm marketplace {name} is gone ({e})"
            ));
            false
        }
    }
}

/// Where launchd stands on one label.
enum Launchd {
    /// No such job.
    NotLoaded,
    /// A job with this label, loaded from this very plist, with the pid launchd gives its
    /// process (none while it is not running).
    LoadedHere(Option<u32>),
    /// A job with this label, loaded from another file.
    LoadedElsewhere,
    /// launchd has no GUI domain for this user (an account that never logged in at the screen,
    /// such as an ssh-only one): no agent can be loaded there.
    NoGuiDomain,
    /// launchd could not be asked, or did not answer clearly.
    Unknown(String),
}

/// `launchctl` exit status for a domain that does not exist for this user.
const NO_DOMAIN_EXIT: i32 = 125;
const NO_DOMAIN_TEXT: &str = "Domain does not support specified action";

fn launchd_state(job: &str, here: &str) -> Launchd {
    match run_cmd("launchctl", &["print", job], Duration::from_secs(20)) {
        Err(e) => Launchd::Unknown(e),
        Ok(run) if run.success => {
            let from_here = run.stdout.lines().any(|l| {
                l.trim()
                    .strip_prefix("path = ")
                    .is_some_and(|p| p.trim() == here)
            });
            if from_here {
                let pid = run.stdout.lines().find_map(|l| {
                    l.trim()
                        .strip_prefix("pid = ")
                        .and_then(|p| p.trim().parse::<u32>().ok())
                });
                Launchd::LoadedHere(pid)
            } else {
                Launchd::LoadedElsewhere
            }
        }
        Ok(run) if run.says("Could not find service") => Launchd::NotLoaded,
        // Both the exit status and the text, so no other launchctl failure reads as "no domain".
        Ok(run) if run.code == Some(NO_DOMAIN_EXIT) && run.says(NO_DOMAIN_TEXT) => {
            Launchd::NoGuiDomain
        }
        Ok(run) => Launchd::Unknown(run.first_stderr_line()),
    }
}

/// No GUI domain, so launchd holds no agent. A daemon started outside launchd can still run:
/// its lock tells. Returns the hold reason when one runs. A lock that cannot be read counts as a
/// daemon that may run: only a missing file or a free lock says there is none.
fn daemon_outside_launchd(home: &Path) -> Option<(String, Option<u32>)> {
    use crate::daemon::{lock_file_probe, LockProbe};
    let lock = home.join(DATA_DIR).join("daemon.lock");
    match lock_file_probe(&lock) {
        LockProbe::Free => None,
        LockProbe::Held => Some(match daemon_lock_holder(home) {
            Some(pid) => (
                format!("a csr-engine daemon is running (pid {pid})"),
                Some(pid),
            ),
            None => ("a csr-engine daemon is running".to_string(), None),
        }),
        LockProbe::Unreadable(why) => Some((
            format!(
                "the daemon lock could not be read ({why}), so a csr-engine daemon may be running"
            ),
            None,
        )),
    }
}

/// True when launchd still runs a CSR daemon, or cannot be asked, so the data folder must stay.
/// Asks about the standard label and about every daemon plist still on disk (one that is not
/// ours, or one the agent step left), whatever the plists say. With no plist on disk, a machine
/// without `launchctl` has no launch agent and counts as clear.
fn daemon_still_loaded(home: &Path, agents: &mut Agents, out: &mut Vec<String>) -> bool {
    let mut labels: Vec<(String, Option<PathBuf>)> = vec![(DAEMON_LABEL.to_string(), None)];
    for agent in daemon_agents_on_disk(home) {
        match labels.iter_mut().find(|(l, _)| *l == agent.label) {
            Some(known) => known.1 = Some(agent.path),
            None => labels.push((agent.label, Some(agent.path))),
        }
    }
    let uid = unsafe { libc::getuid() };
    let mut held = false;
    for (label, plist) in labels {
        let job = format!("gui/{uid}/{label}");
        let here = plist
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        match launchd_state(&job, &here) {
            Launchd::NotLoaded => {}
            Launchd::Unknown(why)
                if plist.is_none() && why.starts_with("could not run launchctl") => {}
            Launchd::LoadedHere(_) | Launchd::LoadedElsewhere => {
                out.push(format!(
                    "launch agent: a job named {label} is still loaded in launchd; stop it with \
                     `launchctl bootout {job}`"
                ));
                agents.note_hold(Hold::Launchd);
                held = true;
            }
            Launchd::NoGuiDomain => {
                if let Some((why, pid)) = daemon_outside_launchd(home) {
                    out.push(format!(
                        "launch agent: {why}; stop it, then run uninstall again"
                    ));
                    agents.note_hold(Hold::OutsideDaemon(pid));
                    held = true;
                }
            }
            Launchd::Unknown(why) => {
                out.push(format!(
                    "launch agent: could not tell whether {label} is running ({why})"
                ));
                agents.note_hold(Hold::Launchd);
                held = true;
            }
        }
    }
    held
}

/// What the agent step decided.
struct Agents {
    /// False when something that should have been removed or stopped was not.
    ok: bool,
    /// True when a daemon may still be running: the data folder must stay.
    hold_data: bool,
    /// The daemon pid this step sent SIGTERM, if it had to.
    signalled: Option<u32>,
    /// What kept `hold_data` set, for the closing line. None while nothing holds.
    hold: Option<Hold>,
}

/// What a held run waits for.
#[derive(Clone, Copy)]
enum Hold {
    /// A job launchd has, or an answer from launchctl that cannot be read: `launchctl` tells.
    Launchd,
    /// Only a daemon started outside launchd on an account with no GUI domain, with its pid when
    /// `daemon.lock` names one. `launchctl` cannot say anything useful there.
    OutsideDaemon(Option<u32>),
}

impl Agents {
    /// Record a hold. Two kinds of hold in one run read as `Launchd`, the broader line.
    fn note_hold(&mut self, kind: Hold) {
        self.hold = Some(match (self.hold, kind) {
            (None, kind) => kind,
            (Some(Hold::OutsideDaemon(pid)), Hold::OutsideDaemon(_)) => Hold::OutsideDaemon(pid),
            _ => Hold::Launchd,
        });
    }
}

/// Ask launchd until it no longer has `job`, for at most `limit`. True when it is gone.
fn wait_until_unloaded(job: &str, here: &str, limit: Duration) -> bool {
    let start = Instant::now();
    loop {
        if matches!(launchd_state(job, here), Launchd::NotLoaded) {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(STOP_POLL.min(limit.saturating_sub(start.elapsed())));
    }
}

/// After a successful bootout (the plist already deleted): wait for the daemon to exit. launchd
/// keeps the job, state "SIGTERMed", until the process is gone, and the daemon's graceful stop
/// takes up to 20 s. Still there after the wait: one SIGTERM to `recorded`, and only while
/// launchd and `daemon.lock` both still name that pid, so a reused pid is never signalled; then
/// a short last wait. `None` when the daemon is gone, else why not.
fn wait_for_daemon_exit(
    home: &Path,
    job: &str,
    here: &str,
    recorded: Option<u32>,
    agents: &mut Agents,
    out: &mut Vec<String>,
) -> Option<String> {
    let wait = stop_wait();
    if wait_until_unloaded(job, here, wait) {
        return None;
    }
    let waited = format!("{:.1} s", wait.as_secs_f64());
    let Some(pid) = recorded else {
        return Some(format!(
            "it is still running {waited} after the stop, and no pid was recorded to signal"
        ));
    };
    let still_ours = matches!(launchd_state(job, here), Launchd::LoadedHere(Some(p)) if p == pid)
        && daemon_lock_holder(home) == Some(pid);
    if still_ours && pid > 1 {
        // SAFETY: kill(2) with a pid launchd and the daemon's own lock both name right now.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
            agents.signalled = Some(pid);
            out.push(format!(
                "launch agent: the daemon (pid {pid}) was still running {waited} after the stop; \
                 sent it SIGTERM"
            ));
        }
    }
    if wait_until_unloaded(job, here, SIGTERM_GRACE.min(wait)) {
        return None;
    }
    Some(format!(
        "pid {pid} is still running {waited} after the stop"
    ))
}

fn agent_step(home: &Path, apply: bool, out: &mut Vec<String>) -> Agents {
    let mut agents = Agents {
        ok: true,
        hold_data: false,
        signalled: None,
        hold: None,
    };
    let found = daemon_agents_on_disk(home);
    if found.is_empty() {
        out.push("launch agent: none found".into());
        return agents;
    }
    for agent in found {
        let owned = std::fs::read_to_string(&agent.path)
            .map(|t| t.contains(DAEMON_PLIST_MARKER))
            .unwrap_or(false);
        if !owned {
            out.push(format!(
                "launch agent: {} was not written by setup; left in place",
                agent.path.display()
            ));
            continue;
        }
        if !apply {
            out.push(format!(
                "launch agent: would stop {} and delete {}, then wait up to {} s for the daemon \
                 to exit",
                agent.label,
                agent.path.display(),
                stop_wait().as_secs()
            ));
            continue;
        }
        // launchd names jobs by label, not by file: stop the loaded job only when it was loaded
        // from this very file, so another account's or another copy's daemon is never stopped.
        // The plist is deleted in every case, right after any bootout and before any wait: with
        // RunAtLoad it would start the daemon again at the next login. A retry still finds a
        // daemon that keeps running by its label (`daemon_still_loaded`).
        let job = format!("gui/{}/{}", unsafe { libc::getuid() }, agent.label);
        let here = agent.path.to_string_lossy().into_owned();
        let hold = |agents: &mut Agents, kind: Hold| {
            agents.ok = false;
            agents.hold_data = true;
            agents.note_hold(kind);
        };
        match launchd_state(&job, &here) {
            Launchd::NotLoaded => {
                delete_plist(&agent.path, &mut agents, out);
            }
            Launchd::LoadedElsewhere => {
                out.push(format!(
                    "launch agent: a job named {} is loaded from another file and keeps running; \
                     stop it with `launchctl bootout {job}` if it is yours",
                    agent.label
                ));
                hold(&mut agents, Hold::Launchd);
                delete_plist(&agent.path, &mut agents, out);
            }
            Launchd::NoGuiDomain => {
                delete_plist(&agent.path, &mut agents, out);
                match daemon_outside_launchd(home) {
                    Some((why, pid)) => {
                        out.push(format!(
                            "launch agent: {why}; stop it, then run uninstall again"
                        ));
                        hold(&mut agents, Hold::OutsideDaemon(pid));
                    }
                    None => out.push(format!(
                        "launch agent: this account has no login session, so {} was not loaded \
                         and there was nothing to unload",
                        agent.label
                    )),
                }
            }
            Launchd::Unknown(why) => {
                delete_plist(&agent.path, &mut agents, out);
                out.push(format!(
                    "launch agent: could not tell whether {} is running ({why}); if it is, stop \
                     it with `launchctl bootout {job}`",
                    agent.label
                ));
                hold(&mut agents, Hold::Launchd);
            }
            Launchd::LoadedHere(launchd_pid) => {
                // The daemon's pid, recorded before the stop: launchd's and the lock's must agree.
                let recorded = launchd_pid.filter(|pid| daemon_lock_holder(home) == Some(*pid));
                let booted = run_cmd(
                    "launchctl",
                    &["bootout", job.as_str()],
                    Duration::from_secs(60),
                );
                delete_plist(&agent.path, &mut agents, out);
                let failure = match booted {
                    Ok(run) if run.success => {
                        wait_for_daemon_exit(home, &job, &here, recorded, &mut agents, out)
                    }
                    Ok(run) => Some(run.first_stderr_line()),
                    Err(e) => Some(e),
                };
                match failure {
                    None => out.push(format!("launch agent: stopped {}", agent.label)),
                    Some(why) => {
                        out.push(format!(
                            "launch agent: could not stop {} ({why}); stop it with `launchctl \
                             bootout {job}`",
                            agent.label
                        ));
                        hold(&mut agents, Hold::Launchd);
                    }
                }
            }
        }
    }
    agents
}

/// Delete a launch agent plist setup wrote, and say so.
fn delete_plist(path: &Path, agents: &mut Agents, out: &mut Vec<String>) {
    match std::fs::remove_file(path) {
        Ok(()) => out.push(format!("launch agent: deleted {}", path.display())),
        Err(e) => {
            out.push(format!(
                "launch agent: could not delete {}: {e}",
                path.display()
            ));
            agents.ok = false;
        }
    }
}

fn data_step(home: &Path, apply: bool, args: &UninstallArgs, out: &mut Vec<String>) -> bool {
    let data = home.join(DATA_DIR);
    let mirror = crate::mirror::mirror_root(&data);
    let meta = match std::fs::symlink_metadata(&data) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push(format!("data: {} does not exist", data.display()));
            return true;
        }
        Err(e) => {
            out.push(format!("data: could not look at {}: {e}", data.display()));
            return false;
        }
    };
    let mirror_note = |kept: bool| {
        let size = human(tree_size(&mirror));
        if kept {
            format!(
                "data: transcript mirror kept at {} ({size})",
                mirror.display()
            )
        } else {
            format!("data: transcript mirror {} ({size})", mirror.display())
        }
    };
    if !args.purges() {
        out.push(format!(
            "data: {} ({}): {}; --purge deletes it",
            data.display(),
            human(tree_size(&data)),
            if apply { "kept" } else { "would be kept" }
        ));
        if mirror.exists() {
            out.push(mirror_note(true));
        }
        return true;
    }
    if meta.file_type().is_symlink() {
        // Someone's own arrangement: remove the link, never what it points at.
        if apply {
            return match std::fs::remove_file(&data) {
                Ok(()) => {
                    out.push(format!("data: removed the link {}", data.display()));
                    true
                }
                Err(e) => {
                    out.push(format!("data: could not remove {}: {e}", data.display()));
                    false
                }
            };
        }
        out.push(format!(
            "data: would remove the link {} only",
            data.display()
        ));
        return true;
    }
    let keep_mirror = !args.purge_mirror;
    // A folder that cannot be listed is reported, never read as empty.
    let mut children: Vec<PathBuf> = Vec::new();
    let mut unlisted = false;
    match std::fs::read_dir(&data) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => children.push(entry.path()),
                    Err(e) => {
                        out.push(format!(
                            "data: could not read an entry of {}: {e}",
                            data.display()
                        ));
                        unlisted = true;
                    }
                }
            }
        }
        Err(e) => {
            out.push(format!(
                "data: could not read {}: {e}; nothing was deleted",
                data.display()
            ));
            return false;
        }
    }
    let doomed: Vec<&PathBuf> = children
        .iter()
        .filter(|p| !(keep_mirror && **p == mirror))
        .collect();
    let bytes: u64 = doomed.iter().map(|p| tree_size(p)).sum();
    if !apply {
        out.push(format!(
            "data: would delete {} entries ({}) under {}",
            doomed.len(),
            human(bytes),
            data.display()
        ));
        if mirror.exists() {
            out.push(mirror_note(keep_mirror));
        }
        return !unlisted;
    }
    let mut ok = !unlisted;
    let mirror_report = mirror.exists().then(|| mirror_note(keep_mirror));
    for child in &doomed {
        let removed = match std::fs::symlink_metadata(child) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(child),
            Ok(_) => std::fs::remove_file(child),
            Err(e) => Err(e),
        };
        if let Err(e) = removed {
            out.push(format!("data: could not delete {}: {e}", child.display()));
            ok = false;
        }
    }
    if keep_mirror {
        // The folder stays because the mirror lives in it.
        out.push(format!(
            "data: deleted {} ({}), kept the mirror",
            data.display(),
            human(bytes)
        ));
    } else if let Err(e) = std::fs::remove_dir(&data) {
        out.push(format!(
            "data: could not remove the folder {}: {e}",
            data.display()
        ));
        ok = false;
    } else {
        out.push(format!(
            "data: deleted {} ({})",
            data.display(),
            human(bytes)
        ));
    }
    out.extend(mirror_report);
    ok
}

/// The model cache, `<cache dir>/csr-engine` (`~/Library/Caches/csr-engine` on macOS): the
/// embedding model fastembed downloaded and reranker files. Not user data, so it is removed in
/// every mode, with or without `--purge`. A link there is removed, never what it points at.
fn cache_step(apply: bool, out: &mut Vec<String>) -> bool {
    let Some(cache) = dirs::cache_dir().map(|dir| dir.join("csr-engine")) else {
        out.push("model cache: this system has no cache folder".into());
        return true;
    };
    let meta = match std::fs::symlink_metadata(&cache) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push(format!("model cache: {} does not exist", cache.display()));
            return true;
        }
        Err(e) => {
            out.push(format!(
                "model cache: could not look at {}: {e}",
                cache.display()
            ));
            return false;
        }
    };
    let link = meta.file_type().is_symlink();
    let size = human(tree_size(&cache));
    if !apply {
        let what = if link {
            format!("the link {}", cache.display())
        } else {
            cache.display().to_string()
        };
        out.push(format!(
            "model cache: would delete {what} ({size}); downloaded models, not your data, so it \
             goes without --purge too"
        ));
        return true;
    }
    let removed = if link || !meta.is_dir() {
        std::fs::remove_file(&cache)
    } else {
        std::fs::remove_dir_all(&cache)
    };
    match removed {
        Ok(()) => {
            out.push(format!("model cache: deleted {} ({size})", cache.display()));
            true
        }
        Err(e) => {
            out.push(format!(
                "model cache: could not delete {}: {e}",
                cache.display()
            ));
            false
        }
    }
}

/// The binaries, last: this one and every one the hooks, MCP entries and launch agent named
/// (`binaries`, read before those were removed). One that sits in `~/.local/bin` is deleted, where
/// the install script and `setup` put it; one anywhere else (npm's folder, `/usr/local/bin`)
/// belongs to whoever put it there, so the exact `rm` line is printed instead. A path that no
/// longer exists is skipped. Kept while the daemon is not confirmed stopped (`held`), since
/// launchd would start it again.
fn binary_step(
    home: &Path,
    apply: bool,
    held: bool,
    binaries: &[PathBuf],
    out: &mut Vec<String>,
) -> bool {
    if std::env::current_exe().is_err() {
        out.push("binary: could not tell where this binary is; remove csr-engine by hand".into());
    }
    let local_bin = home.join(".local").join("bin");
    let mut ok = true;
    for exe in binaries {
        if std::fs::symlink_metadata(exe).is_err() {
            continue;
        }
        let ours = exe.parent() == Some(local_bin.as_path())
            && exe.file_name().is_some_and(|name| name == "csr-engine");
        if !ours {
            out.push(format!(
                "binary: {} was not installed into {}; remove it yourself: rm \"{}\"",
                exe.display(),
                local_bin.display(),
                exe.display()
            ));
            continue;
        }
        if !apply {
            out.push(format!("binary: would delete {} last", exe.display()));
            continue;
        }
        if held {
            out.push(format!(
                "binary: kept {}, because the daemon is not confirmed stopped",
                exe.display()
            ));
            continue;
        }
        match std::fs::remove_file(exe) {
            Ok(()) => out.push(format!("binary: deleted {}", exe.display())),
            Err(e) => {
                out.push(format!("binary: could not delete {}: {e}", exe.display()));
                ok = false;
            }
        }
    }
    ok
}
