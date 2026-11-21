//! What `setup` and older installs leave behind that the main `uninstall` steps do not reach:
//!
//! - the binaries the hooks, MCP entries and launch agent name (read before those are removed),
//! - the npm global package `claude-self-reflect`,
//! - the plugin cache `~/.claude/plugins/cache/claude-self-reflect`,
//! - project-scoped (`-s local`) `claude-self-reflect` MCP entries in `~/.claude.json`,
//! - the running csr-engine processes that will recreate the data folder (named, never signalled).
//!
//! Every function here follows the rules of its parent: a plan changes nothing, a result is
//! confirmed by looking again, and one printed line says what happened to each item.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::{
    codex_config, codex_entry_in_config, command_is_csr_engine, edit_json, human, read_config,
    run_cmd, run_cmd_in, tree_size, Edited, CLAUDE_TIMEOUT, MCP_NAME,
};
use crate::companion::relay::emit;
use crate::setup::daemon_agents_on_disk;

const NPM_PACKAGE: &str = "claude-self-reflect";
const NPM_LS_TIMEOUT: Duration = Duration::from_secs(30);
const NPM_UNINSTALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The first `max` words of a shell command line, with `'...'` and `"..."` read as one word. A
/// line with an unclosed quote has no words (it is somebody else's).
fn command_words(command: &str, max: usize) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = command.trim().chars().peekable();
    while words.len() < max {
        while chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut word = String::new();
        while let Some(c) = chars.next_if(|c| !matches!(c, ' ' | '\t')) {
            if c == '\'' || c == '"' {
                loop {
                    match chars.next() {
                        Some(q) if q == c => break,
                        Some(other) => word.push(other),
                        None => return Vec::new(),
                    }
                }
            } else {
                word.push(c);
            }
        }
        words.push(word);
    }
    words
}

/// The absolute csr-engine path a hook command runs, when it is `<csr-engine> hook ...`.
fn hook_binary(command: &str) -> Option<PathBuf> {
    let words = command_words(command, 2);
    let [binary, sub] = words.as_slice() else {
        return None;
    };
    (sub == "hook" && Path::new(binary).is_absolute() && command_is_csr_engine(binary))
        .then(|| PathBuf::from(binary))
}

/// The first `<string>` of `ProgramArguments` in a launch agent plist, XML entities undone.
fn plist_program(text: &str) -> Option<String> {
    let after = &text[text.find("<key>ProgramArguments</key>")?..];
    let after = &after[after.find("<array>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    Some(
        after[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// Absolute csr-engine paths in the `hooks` of a settings file.
fn settings_binaries(settings: &Value) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let groups = settings.get("hooks").and_then(Value::as_object);
    for entry in groups
        .into_iter()
        .flat_map(|g| g.values())
        .filter_map(Value::as_array)
    {
        for hook in entry
            .iter()
            .filter_map(|e| e.get("hooks").and_then(Value::as_array))
            .flatten()
        {
            let command = hook.get("command").and_then(Value::as_str);
            found.extend(command.and_then(hook_binary));
        }
    }
    found
}

/// Absolute csr-engine commands of `claude-self-reflect` MCP entries in `~/.claude.json`: the user
/// scope one and the project-scoped ones.
fn claude_json_binaries(config: &Value) -> Vec<PathBuf> {
    let user = config.get("mcpServers");
    let projects = config
        .get("projects")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|p| p.values().map(|project| project.get("mcpServers")));
    std::iter::once(user)
        .chain(projects)
        .flatten()
        .filter_map(|servers| servers.get(MCP_NAME)?.get("command")?.as_str())
        .filter(|c| Path::new(c).is_absolute() && command_is_csr_engine(c))
        .map(PathBuf::from)
        .collect()
}

/// Every distinct csr-engine path the install registered, plus `current` (the running binary).
/// Read before the steps that remove the hooks, the MCP entries and the launch agent.
pub(super) fn registered_binaries(home: &Path, current: Option<PathBuf>) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = current.into_iter().collect();
    let settings = read_config(&home.join(".claude").join("settings.json"));
    if let Ok(Some((_, settings))) = settings {
        all.extend(settings_binaries(&settings));
    }
    if let Ok(Some((_, config))) = read_config(&home.join(".claude.json")) {
        all.extend(claude_json_binaries(&config));
    }
    if let Some(command) = codex_entry_in_config(&codex_config(home)) {
        if Path::new(&command).is_absolute() && command_is_csr_engine(&command) {
            all.push(PathBuf::from(command));
        }
    }
    for agent in daemon_agents_on_disk(home) {
        let program = std::fs::read_to_string(&agent.path)
            .ok()
            .and_then(|text| plist_program(&text));
        all.extend(
            program
                .filter(|p| Path::new(p).is_absolute())
                .map(PathBuf::from),
        );
    }
    let mut distinct: Vec<PathBuf> = Vec::new();
    for path in all {
        if !distinct.contains(&path) {
            distinct.push(path);
        }
    }
    distinct
}

/// The version npm lists for the package in `npm ls -g <pkg> --json` output. The exit code is not
/// read: npm exits 1 for problems unrelated to this package and prints `{}` when it is absent.
fn npm_version(stdout: &str) -> Option<String> {
    let json: Value = serde_json::from_str(stdout.trim()).ok()?;
    let entry = json.get("dependencies")?.get(NPM_PACKAGE)?;
    Some(
        entry
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown version")
            .to_string(),
    )
}

/// `Ok(Some(version))` when npm lists the package globally, `Ok(None)` when it does not, `Err`
/// when npm could not tell (it hung or died; "not installed" is the quiet answer for a missing npm).
fn npm_installed() -> Result<Option<String>, String> {
    let args = ["ls", "-g", NPM_PACKAGE, "--json", "--depth=0"];
    match run_cmd("npm", &args, NPM_LS_TIMEOUT) {
        Ok(run) => Ok(npm_version(&run.stdout)),
        Err(e) if e.starts_with("could not run npm") => Ok(None),
        Err(e) => Err(e),
    }
}

/// The npm global package: planned when `npm ls -g` lists it, removed with `npm uninstall -g`
/// and checked again. Not removed while the daemon is not confirmed stopped (`held`): the package
/// holds the binary launchd would start.
pub(super) fn npm_step(apply: bool, held: bool, out: &mut Vec<String>) -> bool {
    let version = match npm_installed() {
        Ok(Some(version)) => version,
        Ok(None) => return true,
        Err(e) => {
            out.push(format!(
                "npm: could not tell whether {NPM_PACKAGE} is installed globally ({e}); check \
                 `npm ls -g {NPM_PACKAGE}`"
            ));
            return false;
        }
    };
    let cmd = format!("npm uninstall -g {NPM_PACKAGE}");
    if !apply {
        out.push(format!(
            "npm: would run `{cmd}` ({NPM_PACKAGE}@{version} is installed globally)"
        ));
        return true;
    }
    if held {
        out.push(format!(
            "npm: kept {NPM_PACKAGE}@{version}, because the daemon is not confirmed stopped"
        ));
        return true;
    }
    let ran = run_cmd(
        "npm",
        &["uninstall", "-g", NPM_PACKAGE],
        NPM_UNINSTALL_TIMEOUT,
    );
    match npm_installed() {
        Ok(None) => {
            out.push(format!("npm: removed {NPM_PACKAGE}@{version} (`{cmd}`)"));
            true
        }
        _ => {
            let why = match ran {
                Ok(run) if run.success => "it is still listed afterwards".to_string(),
                Ok(run) => run.first_stderr_line(),
                Err(e) => e,
            };
            out.push(format!(
                "npm: `{cmd}` did not remove {NPM_PACKAGE}@{version} ({why}); run it by hand"
            ));
            false
        }
    }
}

/// The plugin cache Claude Code leaves under `~/.claude/plugins/cache/<marketplace>` after
/// `claude plugin uninstall`. Deleted only when the plugin is confirmed gone (`plugin_gone`),
/// since an installed plugin runs from it. A link there is removed, never what it points at.
pub(super) fn plugin_cache_step(
    home: &Path,
    apply: bool,
    plugin_gone: bool,
    out: &mut Vec<String>,
) -> bool {
    let dir = home
        .join(".claude/plugins/cache")
        .join(emit::MARKETPLACE_NAME);
    let meta = match std::fs::symlink_metadata(&dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push(format!("plugin cache: {} does not exist", dir.display()));
            return true;
        }
        Err(e) => {
            out.push(format!(
                "plugin cache: could not look at {}: {e}",
                dir.display()
            ));
            return false;
        }
    };
    let size = human(tree_size(&dir));
    if !apply {
        out.push(format!(
            "plugin cache: would delete {} ({size}) once the plugin is removed",
            dir.display()
        ));
        return true;
    }
    if !plugin_gone {
        out.push(format!(
            "plugin cache: kept {}, because the plugin is not confirmed removed",
            dir.display()
        ));
        return true;
    }
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(&dir)
    } else {
        std::fs::remove_file(&dir)
    };
    match removed {
        Ok(()) => {
            out.push(format!("plugin cache: removed {} ({size})", dir.display()));
            true
        }
        Err(e) => {
            out.push(format!(
                "plugin cache: could not delete {}: {e}",
                dir.display()
            ));
            false
        }
    }
}

/// `(project path, what the entry runs)` for every project in `~/.claude.json` that has a
/// `claude-self-reflect` server of its own.
fn local_entries(config: &Value) -> Vec<(String, String)> {
    let Some(projects) = config.get("projects").and_then(Value::as_object) else {
        return Vec::new();
    };
    projects
        .iter()
        .filter_map(|(dir, project)| {
            let entry = project.get("mcpServers")?.get(MCP_NAME)?;
            let word = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
            let args: Vec<String> = entry
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let mut runs = word("command").unwrap_or_else(|| "no command".into());
            for arg in args {
                runs.push(' ');
                runs.push_str(&arg);
            }
            Some((dir.clone(), runs))
        })
        .collect()
}

fn has_local_entry(config: &Value, dir: &str) -> bool {
    config
        .get("projects")
        .and_then(|p| p.get(dir))
        .and_then(|p| p.get("mcpServers"))
        .and_then(|m| m.get(MCP_NAME))
        .is_some()
}

/// Project-scoped `claude-self-reflect` entries (an old install's `run-mcp.sh`, or a project
/// `claude mcp add -s local`). Each goes through `claude mcp remove -s local` run from its project
/// folder; if the CLI cannot (the folder is gone, the CLI is missing) or leaves it, only
/// `projects.<dir>.mcpServers.claude-self-reflect` is edited out of the file.
pub(super) fn local_mcp_step(home: &Path, apply: bool, out: &mut Vec<String>) -> bool {
    let path = home.join(".claude.json");
    // An unreadable file is reported by the user-scope step; there is nothing more to say here.
    let Ok(Some((_, config))) = read_config(&path) else {
        return true;
    };
    let mut ok = true;
    for (dir, runs) in local_entries(&config) {
        if !apply {
            out.push(format!(
                "mcp (local): would run `claude mcp remove {MCP_NAME} -s local` in {dir} (the \
                 entry runs {runs})"
            ));
            continue;
        }
        let cli = run_cmd_in(
            "claude",
            &["mcp", "remove", MCP_NAME, "-s", "local"],
            CLAUDE_TIMEOUT,
            Some(Path::new(&dir)),
        );
        match read_config(&path) {
            Err(e) => {
                out.push(format!(
                    "mcp (local): could not confirm the entry for {dir} is gone ({e}); check it \
                     with `claude mcp list` in that folder"
                ));
                ok = false;
                continue;
            }
            Ok(Some((_, now))) if has_local_entry(&now, &dir) => {}
            Ok(_) => {
                out.push(format!(
                    "mcp (local): removed the '{MCP_NAME}' entry for {dir} (claude mcp remove \
                     -s local)"
                ));
                continue;
            }
        }
        let why = match cli {
            Ok(run) if run.success => "the CLI left it".to_string(),
            Ok(run) => format!("claude mcp remove failed: {}", run.first_stderr_line()),
            Err(e) => e,
        };
        let edited = edit_json(&path, |config| {
            config
                .get_mut("projects")
                .and_then(|p| p.get_mut(&dir))
                .and_then(|p| p.get_mut("mcpServers"))
                .and_then(Value::as_object_mut)
                .is_some_and(|servers| servers.remove(MCP_NAME).is_some())
        });
        match edited {
            Ok(Edited::Written(backup)) => out.push(format!(
                "mcp (local): removed the '{MCP_NAME}' entry for {dir} from {} by hand ({why}; \
                 backup {})",
                path.display(),
                backup.display()
            )),
            Ok(Edited::Unchanged | Edited::Missing) => {
                out.push(format!("mcp (local): the entry for {dir} is already gone"))
            }
            Err(e) => {
                out.push(format!(
                    "mcp (local): {e}; entry for {dir} left in place ({why})"
                ));
                ok = false;
            }
        }
    }
    ok
}

/// Lines naming every csr-engine process that runs one of `binaries`, for the end of the output.
/// Empty when none runs. Sends no signal: a session is the user's to restart. When `purge` is set
/// the data folder is being deleted and these processes will write it again. `signalled` is the
/// launchd daemon the agent step sent SIGTERM, if it did, so the header never claims otherwise.
pub(super) fn process_report(
    binaries: &[PathBuf],
    data: &Path,
    purge: bool,
    signalled: Option<u32>,
) -> Vec<String> {
    let mut seen: std::collections::BTreeMap<u32, crate::stale_report::StaleEngine> =
        std::collections::BTreeMap::new();
    for exe in binaries {
        for engine in crate::stale_report::running(exe) {
            seen.entry(engine.pid).or_insert(engine);
        }
    }
    if seen.is_empty() {
        return Vec::new();
    }
    let signals = match signalled.filter(|pid| seen.contains_key(pid)) {
        Some(pid) => format!("only the launchd daemon (pid {pid}) was sent SIGTERM"),
        None => "none was signalled".to_string(),
    };
    let mut lines = vec![format!(
        "csr-engine is still running in {} process(es); {signals}:",
        seen.len()
    )];
    for e in seen.values() {
        lines.push(format!(
            "  pid {}  {}  session {}  project {}",
            e.pid,
            e.role,
            e.session.as_deref().unwrap_or("unknown"),
            e.project.as_deref().unwrap_or("unknown")
        ));
    }
    lines.push(if purge {
        format!(
            "restart these sessions; they will recreate {}",
            data.display()
        )
    } else {
        "restart these sessions; they keep running the removed csr-engine until you do".to_string()
    });
    lines
}

#[cfg(test)]
#[path = "uninstall_residue_tests.rs"]
mod tests;
