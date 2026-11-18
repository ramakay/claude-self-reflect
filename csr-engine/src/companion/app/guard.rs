//! The push guard and the coaching note: port of the removed `coach.ts` and of the TypeScript
//! companion's `guardOf`, `landsOnRelease`, the `tool.call` guard hook, `coachNote` and
//! `addToClaudeMd`.
//!
//! The rules (the shell, git's options, the verdict) are in `guard/`; this file is what touches the
//! machine: the guard files, `git` itself, the person's CLAUDE.md. All of that goes through
//! `Machine`, so a test says what is on the disk and what git answers.
//!
//! The guard is opt-in. Once a guard file applies to the session, the engine hands the relay an
//! armed state with the `guard.arm` call: one regex and one reason. The relay keeps both and, when
//! the engine does not answer a Bash call, denies the call if the regex matches it. The relay holds
//! no other logic.

mod git;
mod shell;
mod verdict;

use super::state::{remember, show, strings, KEPT};
use crate::companion::{
    coach,
    paths::Paths,
    private,
    relay::dispatch::{handler, Ctx, Handler, Reply, Table},
    store::MACHINE,
};
use git::ConfigArg;
use serde_json::{json, Value};
use shell::{is_space, trim};
use std::{
    collections::HashSet,
    io::Read,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use verdict::reason_for;
pub use verdict::{
    git_parse, git_verdict, merge_guards, parse_guard, protected_from_git, tripwire, GitPush,
    Guard, Verdict, AS_TEXT, HOOKS_OFF, NO_VERIFY, RELEASE, UNRESOLVED,
};

/// The person's guard, under HOME.
const GUARD_FILE: &str = ".claude-self-reflect/csr-companion/guard.json";
/// Where a guard file that is no guard is said, under HOME.
const PANE_ERRORS: &str = ".claude-self-reflect/csr-companion/pane-errors.log";
/// What the relay says when it denies a call because the engine did not answer.
pub const ENGINE_DOWN: &str = "The csr guard is on for this repository and the CSR engine is not \
    answering, so this git command is denied. Run /reload-plugins or restart Claude Code, then \
    try it again.";

// ---------------------------------------------------------------- the machine

/// What `git` said: its exit code (none: a signal) and its standard output.
#[derive(Clone, Debug, PartialEq)]
pub struct Ran {
    pub exit: Option<i32>,
    pub out: String,
}

/// What the guard and the coaching note need of the machine they run on.
pub trait Machine: Send + Sync {
    /// The HOME of the person, when it is set.
    fn home(&self) -> Option<String>;
    fn exists(&self, path: &str) -> bool;
    /// The file's text; none when it is not there or cannot be read.
    fn read(&self, path: &str) -> Option<String>;
    /// The person's own file (CLAUDE.md): written as it is.
    fn write(&self, path: &str, text: &str) -> bool;
    /// One of the companion's private files: redacted, mode 0600.
    fn keep(&self, path: &str, text: &str) -> bool;
    /// A variable of this process's environment.
    fn variable(&self, name: &str) -> Option<String>;
    /// `git -C dir <settings> <args>` with `env` over this process's, given 2 s; none when it could
    /// not be run or did not finish.
    fn git(
        &self,
        dir: &str,
        settings: &[String],
        env: &[(String, String)],
        args: &[&str],
    ) -> Option<Ran>;
    /// The terms the person keeps saying that Claude looked up (`now` in seconds).
    fn coach(&self, now: f64) -> Vec<Value>;
}

/// The real machine.
pub struct Real;

/// The real machine, as the band's press needs it.
pub fn real() -> Arc<dyn Machine> {
    Arc::new(Real)
}

const GIT_TIME: Duration = Duration::from_secs(2);

impl Machine for Real {
    fn home(&self) -> Option<String> {
        std::env::var("HOME").ok().filter(|home| !home.is_empty())
    }

    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }

    fn read(&self, path: &str) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn write(&self, path: &str, text: &str) -> bool {
        std::fs::write(path, text).is_ok()
    }

    fn keep(&self, path: &str, text: &str) -> bool {
        let text = crate::redact::redact_private(text);
        private::write_atomic(std::path::Path::new(path), &text, &private::default_root()).is_ok()
    }

    fn variable(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn git(
        &self,
        dir: &str,
        settings: &[String],
        env: &[(String, String)],
        args: &[&str],
    ) -> Option<Ran> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(settings)
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            bytes
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < GIT_TIME => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let bytes = reader.join().ok()?;
        Some(Ran {
            exit: status?.code(),
            out: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    fn coach(&self, now: f64) -> Vec<Value> {
        match serde_json::to_value(coach::coach_terms(&Paths::from_env(), now)) {
            Ok(Value::Array(notes)) => notes,
            _ => Vec::new(),
        }
    }
}

/// Run `work` on a blocking thread; none when that thread died.
async fn off<T: Send + 'static>(
    machine: &Arc<dyn Machine>,
    work: impl FnOnce(&dyn Machine) -> T + Send + 'static,
) -> Option<T> {
    let machine = machine.clone();
    tokio::task::spawn_blocking(move || work(machine.as_ref()))
        .await
        .ok()
}

// ---------------------------------------------------------------- the guard of a session

/// The bad guard files said in the error log since this engine started (the TypeScript companion's
/// `told`).
#[derive(Default)]
struct Told(HashSet<String>);

/// The guard this session runs under: the repository's `<git toplevel>/.claude/csr-guard.json` (the
/// toplevel of the session's folder, a worktree's own) and the person's guard.json beside CSR's
/// other files, every protection of either. A file that is not a guard (not JSON, a key of the
/// wrong kind) counts for nothing and is said once in the pane's error log. Neither file, or
/// neither good: none, and the guard does nothing.
pub async fn guard_of(cx: &Ctx, machine: &Arc<dyn Machine>, cwd: &str) -> Option<Guard> {
    let home = machine
        .home()
        .map(|home| home.trim_end_matches('/').to_string())
        .filter(|home| !home.is_empty());
    let mut top: Option<String> = None;
    let mut dir = cwd.trim_end_matches('/').to_string();
    while !dir.is_empty() {
        let at = format!("{dir}/.git");
        if off(machine, move |m| m.exists(&at)).await.unwrap_or(false) {
            top = Some(dir);
            break;
        }
        match dir.rfind('/') {
            Some(cut) => dir.truncate(cut),
            None => break,
        }
    }
    let paths = [
        top.map(|top| format!("{top}/.claude/csr-guard.json")),
        home.as_ref().map(|home| format!("{home}/{GUARD_FILE}")),
    ];
    let mut guards = Vec::new();
    for path in paths.into_iter().flatten() {
        let at = path.clone();
        if !off(machine, move |m| m.exists(&at)).await.unwrap_or(false) {
            continue;
        }
        let at = path.clone();
        let text = off(machine, move |m| m.read(&at)).await.flatten();
        match text.as_deref().and_then(parse_guard) {
            Some(guard) => guards.push(guard),
            None => {
                let first = cx.kept(|told: &mut Told| told.0.insert(path.clone()));
                if let (true, Some(home)) = (first, &home) {
                    say_no_guard(cx, machine, home, &path).await;
                }
            }
        }
    }
    merge_guards(&guards)
}

/// One line in the pane's error log: this guard file counts for nothing.
async fn say_no_guard(cx: &Ctx, machine: &Arc<dyn Machine>, home: &str, path: &str) {
    let log = format!("{home}/{PANE_ERRORS}");
    let at = chrono::DateTime::from_timestamp_millis(cx.clock.now() as i64)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    let line = format!(
        "{at} csr guard: {path} is no guard (not JSON, or a key of the wrong kind); it counts for \
         nothing\n"
    );
    let _ = off(machine, move |m| {
        let was = m.read(&log).unwrap_or_default();
        let gap = if !was.is_empty() && !was.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        m.keep(&log, &format!("{was}{gap}{line}"))
    })
    .await;
}

// ---------------------------------------------------------------- where a push lands

/// A variable of this process's environment, by any name; none when it is unset or its name is no
/// shell variable's (`/^[A-Za-z_]\w*$/`).
async fn host_variable(machine: &Arc<dyn Machine>, name: &str) -> Option<String> {
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') || !chars.all(shell::is_word) {
        return None;
    }
    let name = name.to_string();
    off(machine, move |m| m.variable(&name)).await.flatten()
}

/// What git is asked, remembering whether any question failed other than by "not set".
struct Asking<'a> {
    machine: &'a Arc<dyn Machine>,
    dir: String,
    settings: Vec<String>,
    env: Vec<(String, String)>,
    failed: bool,
}

impl Asking<'_> {
    /// What git says, "" for "not set" (an exit in `absent`); anything else marks the landing
    /// unknown.
    async fn ask(&mut self, absent: &[i32], args: &[&str]) -> String {
        let (dir, settings, env) = (self.dir.clone(), self.settings.clone(), self.env.clone());
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        let ran = off(self.machine, move |m| {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            m.git(&dir, &settings, &env, &args)
        })
        .await
        .flatten();
        match ran {
            Some(Ran {
                exit: Some(0), out, ..
            }) => trim(&out).to_string(),
            Some(Ran { exit, .. }) if exit.is_some_and(|code| absent.contains(&code)) => {
                String::new()
            }
            _ => {
                self.failed = true;
                String::new()
            }
        }
    }
}

/// Where a push that names no branch lands, asked of git with the command's own -c settings (Codex
/// review #5: a `-c remote.origin.push=...` was left out): the current branch, its upstream, and
/// the push refspecs of the remote it goes to, never another remote's. That remote is the one
/// named, else the branch's push remote, the default push remote, the branch's remote, origin.
/// push.default=matching sends every branch both sides have, when the push names no refspec: git
/// consults push.default only then. A --config-env setting is a -c setting with its variable's
/// value, in its own place among the others (git takes the last of two), read from what the
/// command line sets, else this process's environment. GIT_CONFIG_* the command line sets go with
/// every question. Fail closed: a variable neither holds, settings git refuses, or a question that
/// fails other than by "not set" leave the landing unknown, and the push is denied. The reason to
/// deny, or none.
pub async fn lands_on_release(
    machine: &Arc<dyn Machine>,
    push: &GitPush,
    cwd: &str,
    guard: &Guard,
) -> Option<String> {
    let mut settings = Vec::new();
    for arg in &push.config {
        match arg {
            ConfigArg::Set(text) => {
                settings.push("-c".to_string());
                settings.push(text.clone());
            }
            ConfigArg::Env(text) => {
                let eq = text.find('=').filter(|&at| at > 0);
                let value = match eq {
                    Some(at) => {
                        let name = &text[at + 1..];
                        match push.env.as_ref().and_then(|env| env.get(name)) {
                            Some(value) => Some(value.clone()),
                            None => host_variable(machine, name).await,
                        }
                    }
                    None => None,
                };
                let (Some(at), Some(value)) = (eq, value) else {
                    return Some(UNRESOLVED.to_string());
                };
                settings.push("-c".to_string());
                settings.push(format!("{}={value}", &text[..at]));
            }
        }
    }
    let env: Vec<(String, String)> = push
        .env
        .iter()
        .flat_map(|env| env.0.iter())
        .filter(|(name, _)| name.starts_with("GIT_CONFIG"))
        .cloned()
        .collect();
    let mut asking = Asking {
        machine,
        dir: push.dir.clone().unwrap_or_else(|| cwd.to_string()),
        settings,
        env,
        failed: false,
    };
    if asking
        .ask(&[], &["rev-parse", "--git-dir"])
        .await
        .is_empty()
        || asking.failed
    {
        return Some(UNRESOLVED.to_string());
    }
    let branch = asking
        .ask(&[1], &["symbolic-ref", "-q", "--short", "HEAD"])
        .await;
    let mut remote = push.remote.clone().unwrap_or_default();
    if remote.is_empty() && !branch.is_empty() {
        let key = format!("branch.{branch}.pushRemote");
        remote = asking.ask(&[1], &["config", "--get", &key]).await;
    }
    if remote.is_empty() {
        remote = asking
            .ask(&[1], &["config", "--get", "remote.pushDefault"])
            .await;
    }
    if remote.is_empty() && !branch.is_empty() {
        let key = format!("branch.{branch}.remote");
        remote = asking.ask(&[1], &["config", "--get", &key]).await;
    }
    if remote.is_empty() {
        remote = "origin".to_string();
    }
    let upstream = asking
        .ask(
            &[1, 128], // no upstream: 128
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )
        .await;
    let key = format!("remote.{remote}.push");
    let specs = asking.ask(&[1], &["config", "--get-all", &key]).await;
    let mode = asking.ask(&[1], &["config", "--get", "push.default"]).await;
    if asking.failed {
        return Some(UNRESOLVED.to_string());
    }
    if let Some(found) = protected_from_git(guard, &branch, &upstream, &specs) {
        return Some(found);
    }
    if push.implicit && mode == "matching" {
        return guard
            .protected_push
            .first()
            .map(|glob| reason_for(guard, glob));
    }
    None
}

// ---------------------------------------------------------------- armed for the relay

/// What the relay was last told is armed (null: nothing), so it is told again only on a change.
#[derive(Default)]
struct Armed(Value);

/// A regex class of the characters that are none of a word's (`set` adds to the ones left out).
fn not_in(set: &str) -> String {
    format!("[^A-Za-z0-9_{set}]")
}

/// A regex source, the same in JavaScript and in Rust's `regex` (no lookaround, ASCII classes),
/// that matches any command that could be a git command the guard denies: `git` as a word and one
/// of the guard's verbs as a word, in either order, anywhere in the text, so a push inside a
/// script, a heredoc or a `bash -c` is caught. It errs toward blocking: a command that only
/// mentions both words matches too. The verbs are `push`, and with `deny_no_verify` also `commit`
/// and `merge`, which --no-verify and core.hooksPath skip the hooks of.
pub fn armed_pattern(guard: &Guard) -> String {
    let verbs = if guard.deny_no_verify {
        "(?:push|commit|merge)"
    } else {
        "push"
    };
    let (word, name) = (not_in("-"), not_in(".-"));
    // The character after the first word is the one before the second.
    let git_then = format!("(?:^|{name})git{word}(?:[\\s\\S]*{word})?{verbs}(?:{word}|$)");
    let then_git = format!("(?:^|{word}){verbs}{word}(?:[\\s\\S]*{name})?git(?:{word}|$)");
    format!("{git_then}|{then_git}")
}

/// The armed state for `guard`: null when none applies.
fn armed_state(guard: Option<&Guard>) -> Value {
    guard.map_or(
        Value::Null,
        |guard| json!({ "pattern": armed_pattern(guard), "reason": ENGINE_DOWN }),
    )
}

/// Tell the relay what is armed, when it is not what it was last told. `force`: tell it anyway
/// when something is armed (a respawned engine remembers nothing, and the relay holds the state it
/// had; a guard that went while the engine was down is disarmed on the next change, not here).
async fn arm(cx: &Ctx, guard: Option<&Guard>, force: bool) {
    let now = armed_state(guard);
    let changed = cx.kept(|armed: &mut Armed| {
        let changed = (force && !now.is_null()) || armed.0 != now;
        armed.0 = now.clone();
        changed
    });
    if changed {
        let _ = cx.host.call("guard.arm", vec![now]).await;
    }
}

// ---------------------------------------------------------------- the hooks

/// The `guard` entry: what the repository's .claude/csr-guard.json and the person's guard.json
/// protect, and nothing when neither is there. Every other git command goes through untouched. A
/// push whose destination the words leave to the repository (bare `git push`, `git push origin
/// HEAD`) is settled by asking git where it lands; what cannot be settled is denied.
fn on_guard(machine: Arc<dyn Machine>) -> Handler {
    handler(move |cx, e| {
        let machine = machine.clone();
        async move {
            let cwd = cx.cwd.clone();
            let guard = guard_of(&cx, &machine, &cwd).await;
            arm(&cx, guard.as_ref(), false).await;
            let Some(guard) = guard else {
                return Ok(Reply::none()); // no guard: no git asked anything
            };
            let Some(command) = e["command"].as_str() else {
                return Ok(Reply::none());
            };
            let deny = |reason: String| Ok(Reply::answer(json!({ "deny": reason })));
            match git_verdict(command, Some(&guard), Some(&cwd)) {
                Some(Verdict::Deny(reason)) => return deny(reason),
                Some(Verdict::Check(pushes)) => {
                    for push in &pushes {
                        if let Some(why) = lands_on_release(&machine, push, &cwd, &guard).await {
                            return deny(why);
                        }
                    }
                }
                Some(Verdict::Ask) | None => (),
            }
            Ok(Reply::none())
        }
    })
}

/// The `start` entry: arm the relay, and offer a coaching note apart from the start.
fn on_start(machine: Arc<dyn Machine>) -> Handler {
    handler(move |cx, e| {
        let machine = machine.clone();
        async move {
            let cwd = e["cwd"]
                .as_str()
                .filter(|cwd| !cwd.is_empty())
                .unwrap_or(&cx.cwd)
                .to_string();
            let guard = guard_of(&cx, &machine, &cwd).await;
            arm(&cx, guard.as_ref(), cx.respawn).await;
            if !cx.respawn {
                cx.host.detach(Box::new(move |host| {
                    Box::pin(async move {
                        let cx = Ctx { host, ..cx };
                        coach_note(&cx, &machine).await;
                    })
                }));
            }
            Ok(Reply::none())
        }
    })
}

pub fn register_with(t: &mut Table, machine: Arc<dyn Machine>) {
    t.on("guard", "guard", on_guard(machine.clone()));
    t.on("start", "guard", on_start(machine));
}

pub fn register(t: &mut Table) {
    register_with(t, real());
}

// ---------------------------------------------------------------- the coaching note

/// Coaching: a word you have said in 2 sessions that Claude looked up each time, offered once as a
/// band note (the `coach` mode works it out off the hot path). The note is the view row, tag
/// `Coaching`; nothing is written until the person presses 1, which the band answers with
/// `add_to_claude_md(&real(), &note["text"])`. A note shown is remembered in the machine row
/// `coached`, so the same word is not offered again.
pub async fn coach_note(cx: &Ctx, machine: &Arc<dyn Machine>) {
    let now = cx.clock.now() / 1000.;
    let Some(notes) = off(machine, move |m| m.coach(now)).await else {
        return;
    };
    let skip: HashSet<String> = strings(cx, MACHINE, "coached").into_iter().collect();
    let fresh = notes.iter().find(|note| {
        note["kind"] == "term" && !note["id"].as_str().is_some_and(|id| skip.contains(id))
    });
    let Some(note) = fresh else { return };
    if show(cx, note, "Coaching").unwrap_or(false) {
        let _ = remember(cx, "coached", note["id"].as_str().unwrap_or(""), KEPT);
        let _ = cx.host.call("ui.invalidate", vec![]).await;
    }
}

/// `/^##\s+Terms\s*$/`
fn terms_heading(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("##") else {
        return false;
    };
    let after = rest.trim_start_matches(is_space);
    after.len() < rest.len()
        && after
            .strip_prefix("Terms")
            .is_some_and(|tail| tail.chars().all(is_space))
}

/// `/^#{1,6}\s/`
fn any_heading(row: &str) -> bool {
    let hashes = row.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes) && row[hashes..].chars().next().is_some_and(is_space)
}

/// One line more under "## Terms" of a CLAUDE.md, the heading made if it is not there. Never twice.
pub fn add_term(text: &str, line: &str) -> String {
    if text.contains(line) {
        return text.to_string();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    let Some(at) = lines.iter().position(|row| terms_heading(row)) else {
        let kept = if trim(text).is_empty() {
            String::new()
        } else {
            format!("{}\n\n", text.trim_end_matches('\n'))
        };
        return format!("{kept}## Terms\n{line}\n");
    };
    let mut end = lines.len();
    if let Some(next) = (at + 1..lines.len()).find(|&i| any_heading(lines[i])) {
        end = next;
    }
    while end > at + 1 && trim(lines[end - 1]).is_empty() {
        end -= 1;
    }
    lines.insert(end, line);
    lines.join("\n")
}

/// One line under "## Terms" of the global CLAUDE.md, written only when the person pressed the key.
/// False when HOME is unknown or the file is there and could not be read (never written over), or
/// the write failed.
pub async fn add_to_claude_md(machine: &Arc<dyn Machine>, line: &str) -> bool {
    let Some(home) = machine.home() else {
        return false;
    };
    let file = format!("{home}/.claude/CLAUDE.md");
    let line = line.to_string();
    off(machine, move |m| {
        // Read it first: a file that is there but could not be read is left alone.
        let text = if m.exists(&file) {
            m.read(&file)
        } else {
            Some(String::new())
        };
        match text {
            Some(text) => m.write(&file, &add_term(&text, &line)),
            None => false,
        }
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests;
