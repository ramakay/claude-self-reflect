//! Stop hook — imports transcript and maintains rolling session summary.
//!
//! For all sessions, imports the current transcript so content is searchable.
//! Also writes a rolling "session_latest" reflection so SessionStart has
//! a summary even if session-end (Haiku story generation) never fires
//! (e.g. Ctrl+C kills the session).
//!
//! v9.2: Extracts structured "episodes" — JSON objects capturing what happened
//! in a session (request, files investigated/modified, tools, outcome, errors).
//! Always returns Ok(()) — never blocks Claude Code.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::HookInput;
use crate::engine::Engine;
use crate::search::cross_project::resolve_project_from_cwd;

/// Matches Claude Code TaskCreate tool_result text: "Task #N created successfully".
static TASK_CREATED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Task #(\d+) created successfully").unwrap());

/// A single todo item captured from the last TodoWrite call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

/// A structured episode capturing what happened in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub schema: String,
    pub session_id: String,
    pub project: String,
    pub timestamp: String,
    pub request: String,
    pub investigated: Vec<String>,
    pub completed: String,
    pub next_steps: Option<String>,
    pub blockers: Option<String>,
    pub outcome: String,
    pub error_signatures: Vec<String>,
    pub tools_used: Vec<String>,
    pub files_modified: Vec<String>,
    pub message_count: usize,
    pub duration_minutes: u32,

    // v2 working-state fields. `#[serde(default)]` keeps v1 episodes deserializable.
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub approved_plan: Option<String>,
    #[serde(default)]
    pub prev_episode_id: Option<String>,
    #[serde(default)]
    pub anchors: Vec<crate::extraction::anchors::FunctionAnchor>,

    // Journal v2 instrumentation feeds (plan `.plans/journal-v2-mailbox-plan.md`
    // §3.3/§4.1). `Option<u32>`, not `u32`: `None` means "never measured"
    // (no transcript pass ran — old episode, oversized transcript, read
    // failure); `Some(0)` means "measured, and there were none". This
    // distinction is the entire honest-degradation story and must survive to
    // the renderer unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_count: Option<u32>,
    #[serde(default)]
    pub top_errors: Vec<crate::transcript::instrumentation::ErrorEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_count: Option<u32>,
    #[serde(default)]
    pub steers: Vec<crate::transcript::instrumentation::SteerEvent>,
    /// Which steer-filter generation measured `steer_count`/`steers`.
    /// `None` (legacy episodes) means the stored total may include harness
    /// noise the current filter would reject — the renderer must then derive
    /// the displayed count from surviving quotes only, never trust this total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrumentation_version: Option<u32>,
    /// Number of deterministic Correction events measured in this transcript.
    /// `None` means intent capture was disabled or could not be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_count: Option<u32>,

    // Recap evidence (hooks::recap). `request` stays the session's FIRST
    // prompt for its other readers; the recap must not use it as the current
    // intent of a long session, which is what these fields are for.
    /// Which extractor generation wrote `anchor_request`/`completions`.
    /// `None` = an episode written before they existed: the recap cannot tell
    /// "no intent" from "never measured" and treats it as legacy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recap_evidence_version: Option<u32>,
    /// The session's anchor task (`anchor_segment`): its first task, else the
    /// segment with the most work, as whole leading sentences
    /// (`hooks::recap::clean_intent`). `None` when neither has a clean
    /// rendering — the recap then abstains. Episodes written by the first
    /// version stored the latest task here.
    #[serde(
        default,
        alias = "latest_request",
        skip_serializing_if = "Option::is_none"
    )]
    pub anchor_request: Option<String>,
    /// The session's latest completions: commits it made and tasks it marked
    /// completed, each with its receipt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completions: Vec<CompletionReceipt>,
    /// True when the anchor is not the session's latest task segment, so the
    /// receipts are not its completion and the recap must not render them as
    /// such. `false` (the default) for episodes written before this existed,
    /// whose completions were the anchor's own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub completions_untied: bool,
    /// The indices into `completions` of the receipts produced under the
    /// anchor task: the only ones the recap may show beside the anchor's
    /// intent, since a commit made under a later or earlier ask says nothing
    /// about this one. `None` for episodes written before this existed, which
    /// show their receipts only when `completions_untied` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_receipts: Option<Vec<usize>>,
    /// Latest renderable typed task after the first, for same-session compact
    /// recaps only. Additive v2 JSON field: old rows default to today's anchor;
    /// no database migration or transcript read at SessionStart is needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_anchor: Option<CompactAnchor>,
    /// Whether the session's latest task still has a failed tool call that no
    /// later call cleared (`outcome_signature`): a failed `cargo test` that was
    /// run again and passed is cleared, a `grep` that found nothing never
    /// counts. `error_signatures` spans the whole session and any tool
    /// output, which for a long one is nearly always non-empty (a Read of
    /// source that says `panic`, a quoted test name), so the outcome reads
    /// only this. `false` for episodes written before this existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub late_errors: bool,
    /// Whether the latest task ended on a failure rather than recovering from
    /// one: its last significant call failed, or a test, build or lint it ran
    /// failed and no run of the same command passed after.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub final_failed: bool,
    /// Whether the latest task committed, merged a PR or installed something
    /// (a call that succeeded): proof of progress that needs no phrase.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closing_receipt: bool,
    /// Whether the transcript ends on a finished assistant turn: not cut off
    /// mid-work, not interrupted by the person.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closing_ended: bool,
    /// Which rule wrote `outcome`. `None`: an episode from before the rule
    /// read the signals above, whose label was wrong for most sessions
    /// checked by hand; it is not shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_version: Option<u32>,
    /// Whether the assistant's closing words under the session's latest task
    /// read as a success. The outcome reads these, not the session-wide
    /// `completed`, so a success said under an early task cannot vouch for a
    /// later one; the task-directory override reuses it for the same reason.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closing_success: bool,
    /// The literal last typed message of the session, as the person wrote it
    /// (plumbing and CSR emissions stripped). `None` when the last typed row
    /// was a forwarded note, or there was none. Never walks back to an older
    /// message: the recap only names it after the anchor when it reads as an
    /// ask, and says nothing when it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_ask: Option<String>,
    /// The time of the last user, assistant or system row this reader saw
    /// (RFC 3339): where in the transcript the episode stands. The Stop reader
    /// and the PreCompact reader record it the same way, so two readings of
    /// one session can be ordered by it, which their wall clocks cannot do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<String>,
    /// Deterministic recap evidence read from the transcript at Stop time (see
    /// `hooks::recap_evidence`): the verification failure the latest task left
    /// open, background tasks never reported finished, and the markers the
    /// closing message opens with. Empty for episodes written before it existed.
    #[serde(
        default,
        skip_serializing_if = "crate::hooks::recap_evidence::RecapExtras::is_empty"
    )]
    pub recap_extras: crate::hooks::recap_evidence::RecapExtras,
}

/// A compact intent and whether the displayed receipts belong to that task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactAnchor {
    pub request: String,
    pub completions_untied: bool,
    /// The receipts produced under this anchor (see `Episode::anchor_receipts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipts: Option<Vec<usize>>,
}

/// A completion with the evidence that proves it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompletionReceipt {
    /// What was completed: a commit subject or a task subject.
    pub claim: String,
    /// The proof: a short commit SHA, or `task #N completed` / `todo completed`.
    pub receipt: String,
}

/// Current generation of the recap evidence fields.
pub const RECAP_EVIDENCE_VERSION: u32 = 1;

/// Current generation of the outcome rule (`compute_outcome`).
pub const OUTCOME_VERSION: u32 = 1;

/// `git commit` / `cherry-pick` / `revert` / `merge` success line:
/// `[<branch> <sha>] <subject>` (also `[<branch> (root-commit) <sha>]`,
/// `[detached HEAD <sha>]`).
static GIT_COMMIT_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\[[^\]\n]+? ([0-9a-f]{7,40})\] (\S[^\n]*)$").unwrap());

/// Split a shell command into simple commands, each a list of words.
/// Quote-aware (single and double quotes group words and are removed);
/// unquoted `;`, `&`, `|`, `(`, `)` and newlines end a simple command.
fn shell_simple_commands(command: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                ';' | '&' | '|' | '(' | ')' | '\n' => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                    if !words.is_empty() {
                        commands.push(std::mem::take(&mut words));
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                c => {
                    word.push(c);
                    in_word = true;
                }
            },
        }
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

/// Whether a Bash command runs a git subcommand that can print a commit
/// success line: the first non-option word after `git` (global options
/// `-c <k=v>`, `-C <path>`, `--no-pager` and other `--long` options skipped)
/// must be commit, cherry-pick, revert or merge. `git log --grep=commit`
/// does not qualify.
fn is_git_committing_command(command: &str) -> bool {
    shell_simple_commands(command).iter().any(|words| {
        // Leading `VAR=value` assignments are part of the simple command.
        let mut rest = words
            .iter()
            .skip_while(|w| w.contains('=') && !w.starts_with('-'));
        let Some(program) = rest.next() else {
            return false;
        };
        if program != "git" && !program.ends_with("/git") {
            return false;
        }
        while let Some(word) = rest.next() {
            match word.as_str() {
                "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                    rest.next();
                }
                w if w.starts_with('-') => {}
                w => return matches!(w, "commit" | "cherry-pick" | "revert" | "merge"),
            }
        }
        false
    })
}

/// A `<sha> <subject>` line, as `git log --oneline` prints it.
static ONELINE_COMMIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^([0-9a-f]{7,40}) (\S[^\n]*)$").unwrap());

/// What `gh pr view --json state,mergeCommit` prints for a merged PR:
/// `MERGED <sha>` or `MERGED <mergedAt> <sha>`.
static MERGED_PR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^MERGED[ \t]+(?:\S+Z[ \t]+)?([0-9a-f]{7,40})\b").unwrap());

/// The pieces of a command joined only by `&&`, so that each runs only if the
/// one before it succeeded. A `|` stays inside its piece (`quiet_commit_is_proven`
/// refuses one on the commit itself). `None` for any other operator: `;`, a
/// newline, `||`, a lone `&`, a subshell or command substitution.
fn and_chain(command: &str) -> Option<Vec<String>> {
    let mut pieces = vec![String::new()];
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        let piece = pieces.last_mut()?;
        match quote {
            Some(q) => {
                piece.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    piece.push(c);
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    pieces.push(String::new());
                }
                // `2>&1`: the `&` belongs to a redirection.
                '&' if piece.ends_with('>') => piece.push(c),
                ';' | '&' | '\n' | '(' | ')' | '`' => return None,
                '|' if chars.peek() == Some(&'|') => return None,
                '$' if chars.peek() == Some(&'(') => return None,
                _ => piece.push(c),
            },
        }
    }
    quote.is_none().then_some(pieces)
}

/// Whether a quiet commit's `git log` line is proof of a new HEAD: the
/// command is a chain of `&&` where a committing git command comes before a
/// `git log` or `git show`, so the log runs only because the commit
/// succeeded. A commit joined to its log by `;`, a newline or `||` runs the
/// log whether the commit worked or not and shows an older HEAD when it did
/// not; that proves nothing.
fn quiet_commit_is_proven(command: &str) -> bool {
    let Some(pieces) = and_chain(command) else {
        return false;
    };
    // A `git log` or `git show` that names no revision other than HEAD: it
    // prints the commit just made, not its parent or another branch.
    let shows_head = |piece: &str| -> bool {
        let Some(words) = shell_simple_commands(piece).into_iter().next() else {
            return false;
        };
        let mut rest = words
            .iter()
            .skip_while(|w| w.contains('=') && !w.starts_with('-'));
        if !rest
            .next()
            .is_some_and(|p| p == "git" || p.ends_with("/git"))
        {
            return false;
        }
        let mut subcommand = None;
        while let Some(word) = rest.next() {
            match word.as_str() {
                "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace"
                    if subcommand.is_none() =>
                {
                    rest.next();
                }
                w if subcommand.is_none() && w.starts_with('-') => {}
                w if subcommand.is_none() => subcommand = Some(w.to_string()),
                // Options that take their value as the next word.
                "-n" | "--max-count" | "--format" | "--pretty" | "--date" | "--skip" => {
                    rest.next();
                }
                "--" => return false,
                w if w.starts_with('-') => {}
                "HEAD" => {}
                _ => return false,
            }
        }
        matches!(subcommand.as_deref(), Some("log" | "show"))
    };
    let Some(commit) = pieces
        .iter()
        .position(|piece| is_git_committing_command(piece) && !piece.contains('|'))
    else {
        return false;
    };
    pieces[commit + 1..].iter().any(|piece| shows_head(piece))
}

/// The commits one successful committing Bash call made, as `(sha, subject)`.
///
/// The `[<branch> <sha>] <subject>` line git prints is proof by itself. A quiet
/// commit (`git commit -q`, the usual form here) prints no such line; its call
/// then ends with `git log --oneline -1` or similar, which prints
/// `<sha> <subject>`. That line counts only when the command proves the log ran
/// because the commit succeeded (`quiet_commit_is_proven`) and the first
/// line it printed is HEAD with a subject the command names; a listing of
/// older commits gives no receipt. A message the shell quoted differently from
/// how git prints it simply gives none.
fn commits_made(command: &str, output: &str) -> Vec<(String, String)> {
    let printed: Vec<(String, String)> = GIT_COMMIT_LINE_RE
        .captures_iter(output)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    if !printed.is_empty() {
        return printed;
    }
    if !quiet_commit_is_proven(command) {
        return Vec::new();
    }
    ONELINE_COMMIT_RE
        .captures(output)
        .filter(|c| command.contains(&c[2]))
        .map(|c| vec![(c[1].to_string(), c[2].to_string())])
        .unwrap_or_default()
}

/// The PR number of the one `gh pr merge <N>` a Bash command runs, or `None`
/// when it runs none or several (the output could not be told apart).
fn merge_command_pr(command: &str) -> Option<String> {
    let mut numbers = shell_simple_commands(command)
        .into_iter()
        .filter_map(|words| {
            let at = words.iter().position(|w| w == "gh" || w.ends_with("/gh"))?;
            let rest = &words[at + 1..];
            (rest.first().map(String::as_str) == Some("pr")
                && rest.get(1).map(String::as_str) == Some("merge"))
            .then(|| {
                rest[2..]
                    .iter()
                    .find(|w| !w.starts_with('-'))
                    .map(|w| w.rsplit('/').next().unwrap_or(w).to_string())
                    .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
        });
    let first = numbers.next()??;
    numbers.next().is_none().then_some(first)
}

/// The merge commit a successful call reports for the PR it merged, as
/// `(sha, claim)`.
fn merged_pr(command: &str, output: &str) -> Option<(String, String)> {
    let number = merge_command_pr(command)?;
    let sha = MERGED_PR_RE.captures(output)?[1].to_string();
    Some((sha, format!("merged PR #{number}")))
}

/// What a main-thread tool call's result will mean for the outcome.
#[derive(Default)]
struct CallSignature {
    /// The signature a failure of this call is filed under, and a later
    /// success of any call with the same signature clears. `None`: a probe,
    /// whose failure says nothing about the task.
    failure_key: Option<String>,
    /// Whether the call installs something; a successful one is progress.
    installs: bool,
}

/// Programs that report "nothing there" or "not reachable" with a failing exit
/// status while working as intended: a `grep` that finds nothing, an `ls` of a
/// missing path, a `curl` of a page that is down. A failure of these is a
/// finding, not the task failing.
const PROBE_PROGRAMS: &[&str] = &[
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ls",
    "cat",
    "head",
    "tail",
    "find",
    "fd",
    "which",
    "whereis",
    "type",
    "command",
    "test",
    "[",
    "[[",
    "diff",
    "cmp",
    "curl",
    "wget",
    "wc",
    "awk",
    "sed",
    "sort",
    "uniq",
    "cut",
    "tr",
    "stat",
    "file",
    "jq",
    "xargs",
    "ps",
    "pgrep",
    "pkill",
    "lsof",
    "df",
    "du",
    "pwd",
    "date",
    "whoami",
    "uname",
    "id",
    "env",
    "printenv",
    "open",
    "readlink",
    "realpath",
    "basename",
    "dirname",
    "tee",
    "mdfind",
    "plutil",
    "defaults",
    "osascript",
    "sqlite3",
    "gh",
    "launchctl",
    "tmux",
    "herdr",
    "kill",
    "ssh",
    "scp",
    "rsync",
    "tree",
    "less",
    "more",
    "column",
    "nl",
    "od",
    "xxd",
    "hexdump",
    "strings",
    "nm",
    "otool",
    "lipo",
    "codesign",
    "spctl",
    "ping",
    "nslookup",
    "dig",
    "host",
    "nc",
    "netstat",
    "ifconfig",
];

/// Shell plumbing that is not the command whose status matters.
const NOISE_PROGRAMS: &[&str] = &[
    "cd", "export", "source", ".", "set", "unset", "true", "false", "echo", "printf", "sleep",
    "exit", "pushd", "popd", "wait", "read", "trap", "umask", "ulimit", "alias", "mkdir", "touch",
    "rm", "mv", "cp", "ln", "chmod", "chown",
];

/// Words that wrap another command: the wrapped command is the one that counts.
const WRAPPER_PROGRAMS: &[&str] = &[
    "env",
    "sudo",
    "time",
    "nohup",
    "caffeinate",
    "exec",
    "command",
];

/// Programs whose first argument is the operation, so a failed `cargo test`
/// is not cleared by a `cargo build` that passed.
const SUBCOMMAND_PROGRAMS: &[&str] = &[
    "git",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "docker",
    "brew",
    "xcrun",
    "swift",
    "go",
    "uv",
    "pip",
    "pip3",
    "kubectl",
    "railway",
    "gcloud",
    "rustup",
    "make",
    "csr-engine",
];

/// `git` subcommands that only look.
const GIT_PROBE_SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "grep",
    "ls-files",
    "rev-parse",
    "branch",
    "remote",
    "config",
    "blame",
    "cat-file",
    "ls-tree",
    "describe",
    "shortlog",
    "reflog",
    "rev-list",
    "merge-base",
    "worktree",
    "tag",
    "stash",
];

fn program_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The commands of a Bash call that can matter to its outcome, in order: each
/// as `(program, arguments)`. Heredoc bodies and redirections are not commands.
fn significant_commands(command: &str) -> Vec<(String, Vec<String>)> {
    let command = command.split("<<").next().unwrap_or(command);
    shell_simple_commands(command)
        .iter()
        .filter_map(|words| significant_command(words))
        .collect()
}

/// The program and arguments of one simple command, or `None` when it is
/// noise (an assignment, a redirection, a shell builtin that proves nothing).
fn significant_command(words: &[String]) -> Option<(String, Vec<String>)> {
    let mut rest = words
        .iter()
        .skip_while(|w| w.contains('=') && !w.starts_with('-') && !w.starts_with('/'))
        .map(String::as_str)
        .peekable();
    // `time cargo test`, `timeout 60 make`: the wrapped command counts.
    while let Some(first) = rest.peek().copied() {
        if WRAPPER_PROGRAMS.contains(&program_name(first)) {
            rest.next();
        } else if program_name(first) == "timeout" {
            rest.next();
            rest.next();
        } else {
            break;
        }
    }
    let program = rest.next()?;
    let name = program_name(program);
    let real = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '.')
        && !name.chars().all(|c| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| !matches!(c, '>' | '<' | '$' | '`' | '{' | '}' | '*'));
    if !real || NOISE_PROGRAMS.contains(&name) {
        return None;
    }
    Some((name.to_string(), rest.map(str::to_string).collect()))
}

/// Whether one command installs something: an `install` program or script,
/// or an `install` operation (`cargo install`, `npm install`,
/// `csr-engine hook install`, `devicectl device install app`).
fn command_installs(program: &str, args: &[String]) -> bool {
    if PROBE_PROGRAMS.contains(&program) {
        return false;
    }
    let is_install_name = |name: &str| {
        let name = program_name(name);
        name == "install"
            || name.starts_with("install-")
            || name.starts_with("install_")
            || name.starts_with("install.")
    };
    if is_install_name(program) {
        return true;
    }
    let operands: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect();
    if matches!(
        program,
        "bash" | "sh" | "zsh" | "python3" | "python" | "node"
    ) {
        return operands.first().is_some_and(|a| is_install_name(a));
    }
    operands.iter().take(3).any(|a| *a == "install")
}

/// Prefix of a failure key that belongs to a verification command.
const VERIFY_KEY_PREFIX: &str = "verify:";

/// Whether a command verifies the work: a test run, a build, a type check, a
/// lint. Its failure is the work failing, and only a run of the same command
/// passing resolves it.
fn is_verification(program: &str, args: &[String]) -> bool {
    let operands: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-') && !a.starts_with('+'))
        .collect();
    let first = operands.first().copied();
    match program {
        "cargo" => matches!(
            first,
            Some("test" | "nextest" | "build" | "check" | "clippy" | "bench")
        ),
        "npm" | "pnpm" | "yarn" | "bun" => {
            let target = if first == Some("run") {
                operands.get(1).copied()
            } else {
                first
            };
            target.is_some_and(|t| {
                ["test", "build", "lint", "typecheck", "check", "tsc"]
                    .iter()
                    .any(|v| t == *v || t.starts_with(&format!("{v}:")))
            })
        }
        "go" => matches!(first, Some("test" | "build" | "vet")),
        "swift" => matches!(first, Some("test" | "build")),
        "python" | "python3" => args.windows(2).any(|w| {
            w[0] == "-m" && matches!(w[1].as_str(), "pytest" | "mypy" | "unittest" | "ruff")
        }),
        "make" | "gmake" | "ninja" | "xcodebuild" | "gradle" | "gradlew" | "mvn" | "tsc"
        | "pytest" | "py.test" | "jest" | "vitest" | "mypy" | "ruff" | "eslint" | "tox" | "nox"
        | "rspec" | "ctest" | "bazel" => true,
        _ => false,
    }
}

/// Flags that change how a verification command prints or runs, not what it
/// checks. `true`: the flag takes the next word as its value. Build-profile
/// selectors (`--release`, `--profile`, `--features`, `--target`) are not
/// here: a pass under another profile is another run.
fn verification_noise_flag(word: &str) -> Option<bool> {
    match word {
        "-q" | "--quiet" | "-v" | "-vv" | "--verbose" | "--locked" | "--offline" | "--frozen"
        | "--nocapture" | "--no-fail-fast" | "--show-output" => Some(false),
        "--color" | "-j" | "--jobs" | "--message-format" => Some(true),
        w if w.starts_with("--color=")
            || w.starts_with("--jobs=")
            || w.starts_with("--message-format=") =>
        {
            Some(false)
        }
        w if w.len() > 2 && w.starts_with("-j") && w[2..].chars().all(|c| c.is_ascii_digit()) => {
            Some(false)
        }
        _ => None,
    }
}

/// `path` with `.` and `..` resolved lexically (no filesystem access). A
/// relative path keeps leading `..` it cannot resolve.
fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ if absolute => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The directory a verification command runs in: the call's own directory,
/// moved by every `cd` that comes before the command in the chain (a `cd`
/// after it changes nothing), normalized lexically.
fn command_directory(command: &str, program: &str, args: &[String], cwd: Option<&str>) -> String {
    let command = command.split("<<").next().unwrap_or(command);
    let mut dir: Option<String> = cwd.map(str::to_string);
    let mut at_command: Option<Option<String>> = None;
    for words in shell_simple_commands(command) {
        if words.first().map(String::as_str) == Some("cd") {
            let target = words.get(1).map(String::as_str).unwrap_or("~");
            dir = Some(match (target, &dir) {
                (t, _) if t.starts_with('/') => t.to_string(),
                (t, Some(base)) if !t.starts_with('~') && t != "-" => format!("{base}/{t}"),
                (t, None) if !t.starts_with('~') && t != "-" => t.to_string(),
                // `cd`, `cd ~/x`, `cd -`: somewhere this cannot name.
                (t, _) => format!("?{t}"),
            });
        } else if let Some((name, found_args)) = significant_command(&words) {
            if name == program && found_args == args {
                at_command = Some(dir.clone());
            }
        }
    }
    normalize_path(&at_command.unwrap_or(dir).unwrap_or_default())
}

/// The failure key of a verification command: what it checks (program, every
/// argument that selects a target or a filter) and where it runs. A run of
/// `cargo test --test healthy` does not resolve a failed `cargo test --test
/// broken`, and neither does the same command in another directory.
fn verification_key(command: &str, program: &str, args: &[String], cwd: Option<&str>) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        // Redirections (`2>&1`, `> log`) are not arguments.
        if !word.is_empty()
            && word
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .starts_with('>')
        {
            if word
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches('>')
                .is_empty()
            {
                words.next();
            }
            continue;
        }
        match verification_noise_flag(word) {
            Some(true) => {
                words.next();
            }
            Some(false) => {}
            // `-r` is cargo's `--release`.
            None if word == "-r" => kept.push("--release"),
            None => kept.push(word),
        }
    }
    format!(
        "{VERIFY_KEY_PREFIX}{program} {} @{}",
        kept.join(" "),
        command_directory(command, program, args, cwd)
    )
}

/// What a main-thread tool call's result will mean (see [`CallSignature`]).
///
/// An edit is filed under its file, so a later successful edit or write of the
/// same file clears a failed one. A Bash call is filed under the last
/// significant command in its chain (`cd x && cargo test | tail` is
/// `cargo test`); a chain of probes and plumbing has none. Every other tool
/// (reads, searches, fetches, CSR's own) is a probe.
fn call_signature(name: &str, input: &serde_json::Value, cwd: Option<&str>) -> CallSignature {
    match name {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => CallSignature {
            failure_key: input
                .get("file_path")
                .or_else(|| input.get("notebook_path"))
                .and_then(|v| v.as_str())
                .map(|path| format!("edit:{path}")),
            installs: false,
        },
        "Bash" => {
            let command = input.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let commands: Vec<(String, Vec<String>)> = significant_commands(command)
                .into_iter()
                .filter(|(program, args)| {
                    let looks = program == "git"
                        && args
                            .iter()
                            .find(|a| !a.starts_with('-'))
                            .is_some_and(|sub| GIT_PROBE_SUBCOMMANDS.contains(&sub.as_str()));
                    !looks
                })
                .collect();
            let installs = commands
                .iter()
                .any(|(program, args)| command_installs(program, args));
            let failure_key = commands
                .iter()
                .rev()
                .find(|(program, _)| !PROBE_PROGRAMS.contains(&program.as_str()))
                .map(|(program, args)| {
                    if is_verification(program, args) {
                        return verification_key(command, program, args, cwd);
                    }
                    let operation = SUBCOMMAND_PROGRAMS
                        .contains(&program.as_str())
                        .then(|| args.iter().find(|a| !a.starts_with('-')))
                        .flatten();
                    match operation {
                        Some(operation) => format!("{program} {operation}"),
                        None => program.clone(),
                    }
                });
            CallSignature {
                failure_key,
                installs,
            }
        }
        _ => CallSignature::default(),
    }
}

/// Rows Claude Code writes into the user channel as plain text. Nobody typed
/// them, so they are never a task.
const HARNESS_USER_ROW_PREFIXES: [&str; 4] = [
    "[Request interrupted",
    "[SYSTEM NOTIFICATION",
    "Caveat: The messages below were generated by the user while running local commands",
    "This session is being continued from a previous conversation",
];

/// Longest closing message kept whole for its marker lines, in bytes.
const CLOSING_MESSAGE_MAX_BYTES: usize = 12_000;

/// The first 8 characters of a row's `uuid`: the receipt a quoted line carries.
fn row_uuid(val: &serde_json::Value) -> Option<String> {
    let uuid = val.get("uuid").and_then(|v| v.as_str())?;
    let short: String = uuid.chars().take(8).collect();
    (short.len() == 8).then_some(short)
}

/// The text of a row that may carry a `<task-notification>`: a queued
/// notification, a queued-command attachment, or the harness's own user row
/// (read from the raw twin, since the search sanitizer blanks those).
fn notification_text<'a>(
    val: &'a serde_json::Value,
    raw: &'a serde_json::Value,
    msg_type: &str,
) -> Option<&'a str> {
    match msg_type {
        "queue-operation" => val.get("content").and_then(|v| v.as_str()),
        "attachment" => val
            .get("attachment")
            .filter(|a| a.get("type").and_then(|t| t.as_str()) == Some("queued_command"))
            .and_then(|a| a.get("prompt"))
            .and_then(|v| v.as_str()),
        "user" | "human" => raw
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        _ => None,
    }
}

/// Most completions kept per session for each receipt kind (commits, and
/// task/todo receipts); the recap renders at most two.
const MAX_COMPLETIONS: usize = 5;

/// Weight of one commit receipt, in tool calls. Across the newest 1,500
/// transcripts (main sessions and subagents) the median work behind one
/// commit was ~50 tool calls (quartiles 33/50/92). A receipt adds a fifth of
/// that: enough to favour the segment that landed something over one of
/// similar size, too little for a short "commit it" segment (2-3 calls) to
/// outweigh a median segment of real work.
const COMMIT_WEIGHT: u32 = 10;

/// Longest gap between two mid-turn prompts for the second to be the first
/// one's text continued (a message sent in two halves). Live split messages
/// arrived 24 s and 57 s apart; mid-turn additions ("ok one more now ...")
/// 24 and 34 minutes into the work. A later mid-turn prompt still belongs
/// to the segment and its work counts, but its words stay out of the
/// anchor text.
const JOIN_WINDOW_SECS: i64 = 120;

/// A task-state change a tool call asks for. It is applied only when a
/// matching tool_result succeeds (or at once when the call has no id).
enum TaskOp {
    /// TodoWrite: a full-list rewrite.
    Write(Vec<TodoItem>),
    /// TaskCreate: a new pending task with this subject.
    Create(String),
    /// TaskUpdate: status and/or subject of task `task_id`.
    Update {
        task_id: String,
        status: Option<String>,
        subject: Option<String>,
    },
}

impl TaskOp {
    fn from_tool_use(name: &str, block: &serde_json::Value) -> Option<Self> {
        let input = block.get("input")?;
        match name {
            "TodoWrite" => {
                let items = input.get("todos")?.as_array()?;
                Some(TaskOp::Write(
                    items
                        .iter()
                        .filter_map(|t| {
                            Some(TodoItem {
                                content: t.get("content")?.as_str()?.to_string(),
                                status: t
                                    .get("status")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("pending")
                                    .to_string(),
                            })
                        })
                        .take(10)
                        .collect(),
                ))
            }
            "TaskCreate" => Some(TaskOp::Create(input.get("subject")?.as_str()?.to_string())),
            "TaskUpdate" => {
                let task_id = input.get("taskId").and_then(|v| {
                    if let Some(s) = v.as_str() {
                        Some(s.to_string())
                    } else if let Some(n) = v.as_i64() {
                        Some(n.to_string())
                    } else {
                        v.as_u64().map(|n| n.to_string())
                    }
                })?;
                Some(TaskOp::Update {
                    task_id,
                    status: input
                        .get("status")
                        .and_then(|s| s.as_str())
                        .map(str::to_string),
                    subject: input
                        .get("subject")
                        .and_then(|s| s.as_str())
                        .map(str::to_string),
                })
            }
            _ => None,
        }
    }
}

/// The main session's task list as its successful tool calls left it.
#[derive(Default)]
struct TaskState {
    todos: Vec<TodoItem>,
    /// The task segment each entry of `todos` was created under, by position.
    /// Identity, not subject: two TaskCreate tasks both named "Run tests"
    /// belong to the asks they were created for.
    origins: Vec<Option<usize>>,
    /// Numeric task id -> index into `todos`. Ids come only from tool_result
    /// text ("Task #N created successfully"), never from ordinal position.
    task_id_map: HashMap<String, usize>,
    /// Creates applied without a tool_use id, awaiting an unlabeled result's
    /// id binding (FIFO).
    unbound_creates: Vec<usize>,
    /// Calls awaiting their tool_result, by tool_use id.
    pending: HashMap<String, TaskOp>,
}

impl TaskState {
    /// Apply a successful change made under task segment `segment`. `result`
    /// is the tool_result text when there was one. Returns the completions it
    /// evidences, as (claim, receipt, the segment the task was created under).
    fn apply(
        &mut self,
        op: TaskOp,
        result: Option<&str>,
        segment: Option<usize>,
    ) -> Vec<(String, String, Option<usize>)> {
        match op {
            TaskOp::Write(items) => {
                // Full-list rewrite: replaces TaskCreate-derived state and
                // invalidates id bindings (indices no longer apply). A todo has
                // no id, so an item of the same words in two lists cannot be
                // told from a new one: a completion belongs to the segment it
                // was written under, never to an earlier ask.
                let previous = std::mem::replace(&mut self.todos, items);
                self.origins = vec![segment; self.todos.len()];
                self.task_id_map.clear();
                self.unbound_creates.clear();
                self.todos
                    .iter()
                    .zip(&self.origins)
                    .filter(|(t, _)| {
                        t.status == "completed"
                            && !previous
                                .iter()
                                .any(|p| p.content == t.content && p.status == "completed")
                    })
                    .map(|(t, origin)| (t.content.clone(), "todo completed".to_string(), *origin))
                    .collect()
            }
            TaskOp::Create(subject) => {
                // Shared cap with TodoWrite: overflow is silently dropped.
                if self.todos.len() >= 10 {
                    return Vec::new();
                }
                let index = self.todos.len();
                self.todos.push(TodoItem {
                    content: subject,
                    status: "pending".to_string(),
                });
                self.origins.push(segment);
                match result.and_then(|text| TASK_CREATED_RE.captures(text)) {
                    Some(caps) => {
                        self.task_id_map.insert(caps[1].to_string(), index);
                    }
                    None if result.is_none() => self.unbound_creates.push(index),
                    None => {}
                }
                Vec::new()
            }
            TaskOp::Update {
                task_id,
                status,
                subject,
            } => {
                let mut done = Vec::new();
                let Some(index) = self.task_id_map.get(&task_id).copied() else {
                    return done;
                };
                let origin = self.origins.get(index).copied().unwrap_or(segment);
                let Some(item) = self.todos.get_mut(index) else {
                    return done;
                };
                if let Some(status) = status {
                    if status == "completed" && item.status != "completed" {
                        done.push((
                            item.content.clone(),
                            format!("task #{task_id} completed"),
                            origin,
                        ));
                    }
                    item.status = status;
                }
                if let Some(subject) = subject {
                    item.content = subject;
                }
                done
            }
        }
    }

    /// Bind a "Task #N created successfully" result that carries no
    /// tool_use_id to the oldest create applied without one.
    fn bind_unlabeled(&mut self, result: &str) {
        let Some(caps) = TASK_CREATED_RE.captures(result) else {
            return;
        };
        if !self.unbound_creates.is_empty() {
            let index = self.unbound_creates.remove(0);
            self.task_id_map.insert(caps[1].to_string(), index);
        }
    }
}

/// One task: a task prompt, its mid-turn continuations, and the work done
/// until the next task prompt.
#[derive(Default)]
struct TaskSegment {
    prose: String,
    /// A forwarded side-agent note or dreamer what-if: its own task, never
    /// joined with the prompts around it.
    note: bool,
    /// When the segment's last JOINED prompt was sent, if the row had a time.
    last_prompt_at: Option<chrono::DateTime<chrono::Utc>>,
    tool_calls: u32,
    commits: u32,
}

impl TaskSegment {
    fn weight(&self) -> u32 {
        self.tool_calls + COMMIT_WEIGHT * self.commits
    }
}

/// Fewest words a heaviest-segment fallback intent may have.
const MIN_FALLBACK_WORDS: usize = 6;

/// The anchor task and its segment index. The first task anchors the recap
/// (blind labels picked it in 9 of 11 sessions, the most-work segment in 7).
/// If it has no clean rendering, the heaviest other segment with any work is
/// tried; otherwise the intent abstains. Same-session compact recaps instead
/// use the latest renderable typed segment captured by `latest_typed_segment`.
fn anchor_segment(segments: &[TaskSegment]) -> (Option<String>, Option<usize>) {
    if segments.is_empty() {
        return (None, None);
    }
    // Heaviest other segment; on equal weight the later one wins.
    let heaviest = (1..segments.len())
        .filter(|&i| segments[i].weight() > 0)
        .max_by(|&a, &b| {
            segments[a]
                .weight()
                .cmp(&segments[b].weight())
                .then(a.cmp(&b))
        });
    // The first task may be cut at a clause when it is over-long.
    if let Some(intent) = crate::hooks::recap::anchor_intent(&segments[0].prose) {
        return (Some(intent), Some(0));
    }
    // A fallback must say enough to stand for the session on its own: a
    // short side request ("stopped check now.") abstains instead.
    if let Some(index) = heaviest {
        if let Some(intent) = crate::hooks::recap::clean_intent(&segments[index].prose)
            .filter(|intent| intent.split_whitespace().count() >= MIN_FALLBACK_WORDS)
        {
            return (Some(intent), Some(index));
        }
    }
    (None, None)
}

/// Compact resumes need the current task, not the session's first one.
/// Skip notes and thin side requests; allow the same clause cut as the first
/// anchor. If no later segment renders, keep `anchor_segment` unchanged.
fn latest_typed_segment(segments: &[TaskSegment]) -> Option<(String, usize)> {
    segments
        .iter()
        .enumerate()
        .skip(1)
        .rev()
        .find_map(|(index, task)| {
            if task.note {
                return None;
            }
            crate::hooks::recap::anchor_intent(&task.prose)
                .filter(|intent| intent.split_whitespace().count() >= MIN_FALLBACK_WORDS)
                .map(|intent| (intent, index))
        })
}

/// A typed prompt's text for the recap, and whether it is a forwarded note.
struct TypedPrompt {
    prose: String,
    note: bool,
}

/// Opener of a Claude Code side agent's note forwarded into the prompt. The
/// import scrub keys on the shared `provenance::SIDE_AGENT_NOTE_PREFIX`; this
/// stays the full YSK line because the recap reads RAW rows.
const SIDE_AGENT_NOTE_OPENER: &str = "Here is a note offered by a side agent:";
/// Lead-in of a side agent note's one-line summary.
const HEADS_UP: &str = "Heads up · ";

/// The headline of a forwarded side-agent note or a dreamer what-if, or
/// `None` when `text` is neither. A side-agent note's headline is its bold
/// title line, else the first sentence after "Heads up ·". A dreamer
/// what-if's headline is its question line. The words are the note's, not
/// the person's, so callers render them behind a "note: " prefix.
fn forwarded_note_headline(text: &str) -> Option<String> {
    // Recognition and extraction must see the same text: a CRLF prompt has
    // "\r\n\r\n" paragraph breaks that a "\n\n" split would miss.
    let normalized = text.replace("\r\n", "\n");
    let text = normalized.as_str();
    if let Some(rest) = text.trim_start().strip_prefix(SIDE_AGENT_NOTE_OPENER) {
        let quoted: Vec<&str> = rest
            .lines()
            .filter_map(|line| line.trim().strip_prefix('>'))
            .map(str::trim)
            .collect();
        // A forwarded note that carries CSR's own output (a recap, an
        // injected block, any machine sentinel) is CSR's words. Clipping its
        // headline would drop the sentinel and launder it as a note.
        let body: String = quoted
            .iter()
            .map(|line| line.trim_matches('*').trim())
            .collect::<Vec<_>>()
            .join("\n");
        if crate::extraction::provenance::contains_machine_sentinel(rest)
            || crate::extraction::provenance::is_csr_emission(&body)
            || quoted.iter().any(|line| {
                crate::extraction::provenance::is_csr_emission(line.trim_matches('*').trim())
            })
        {
            return None;
        }
        let title = quoted.iter().find_map(|line| {
            let inner = line.strip_prefix("**")?.strip_suffix("**")?.trim();
            (!inner.contains("**") && inner.chars().any(char::is_alphanumeric))
                .then(|| inner.to_string())
        });
        return title.or_else(|| {
            quoted
                .iter()
                .find_map(|line| line.strip_prefix(HEADS_UP))
                .and_then(crate::hooks::recap::first_sentence)
        });
    }
    if crate::extraction::provenance::is_dreamer_prompt(text) {
        let question = text.trim().split("\n\n").next()?.trim();
        // Forward grammar v2 wraps the band's question in curly quotes.
        let question = question
            .strip_prefix('\u{201c}')
            .and_then(|inner| inner.strip_suffix('\u{201d}'))
            .unwrap_or(question);
        return (!question.starts_with(crate::extraction::provenance::DREAMER_ATTRIBUTION)
            && !question.contains('\n'))
        .then(|| question.to_string());
    }
    None
}

/// A typed row that forwards a side-agent note or a dreamer what-if, as
/// "note: <headline>". Read from the RAW row: the search sanitizer blanks a
/// dreamer prompt, since every word of it is CSR's.
fn forwarded_note(val: &serde_json::Value) -> Option<TypedPrompt> {
    // Rows the harness wrote (task notifications, peer messages) are never
    // a forwarded note, whatever they quote.
    let harness = val.get("promptSource").and_then(|v| v.as_str()) == Some("system")
        || matches!(
            val.get("origin")
                .and_then(|o| o.get("kind"))
                .and_then(|k| k.as_str()),
            Some("task-notification" | "peer")
        );
    if harness {
        return None;
    }
    let text = typed_row_text(val)?;
    let headline = forwarded_note_headline(&crate::extraction::provenance::strip_plumbing(&text))?;
    Some(TypedPrompt {
        prose: format!("note: {headline}"),
        note: true,
    })
}

/// The text of a typed user row, or `None` for tool results, injected/meta
/// rows, compact summaries and harness notices.
fn typed_row_text(val: &serde_json::Value) -> Option<String> {
    let flagged = ["isMeta", "isCompactSummary", "isSidechain"]
        .iter()
        .any(|key| val.get(key).and_then(|v| v.as_bool()).unwrap_or(false));
    // Scheduled /loop, ScheduleWakeup and cron re-fires are isMeta rows;
    // system-origin harness messages are not typed even without that flag.
    let harness = val.get("promptSource").and_then(|v| v.as_str()) == Some("system")
        || matches!(
            val.get("origin")
                .and_then(|o| o.get("kind"))
                .and_then(|k| k.as_str()),
            Some("task-notification" | "peer")
        );
    if flagged || harness {
        return None;
    }
    let content = val.get("message").and_then(|m| m.get("content"))?;
    let text = if let Some(text) = content.as_str() {
        text.to_string()
    } else {
        let blocks = content.as_array()?;
        if blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        {
            return None;
        }
        blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let trimmed = text.trim_start();
    (!HARNESS_USER_ROW_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix)))
    .then_some(text)
}

/// The prompt's text for the recap, or `None` when the row is not a typed
/// prompt: tool results, injected/meta rows, compact summaries, harness
/// notices, command plumbing, and CSR's own emissions pasted back. A
/// forwarded side-agent note or dreamer what-if is a real task and comes
/// back as "note: <headline>".
fn typed_prompt(val: &serde_json::Value) -> Option<TypedPrompt> {
    let text = typed_row_text(val)?;
    // Plumbing first: strip_csr_emissions drops `<system-reminder>` tag lines
    // but keeps what sits between them, so run it after the whole block is gone.
    let unplumbed = crate::extraction::provenance::strip_plumbing(&text);
    let own = crate::extraction::provenance::strip_csr_emissions(&unplumbed);
    crate::extraction::provenance::extractable(&own).map(|prose| TypedPrompt { prose, note: false })
}

/// Whether this row closes an assistant turn: a `turn_duration` or
/// `stop_hook_summary` system row, or an assistant message that ended its
/// turn. Assistant rows with NO `stop_reason` key (older transcript formats)
/// count as finished, so a transcript without turn markers never chains
/// its prompts together. An explicit `null` is a row still streaming inside
/// an open turn, not a finish.
fn ends_turn(val: &serde_json::Value, msg_type: &str) -> bool {
    match msg_type {
        "system" => matches!(
            val.get("subtype").and_then(|v| v.as_str()),
            Some("turn_duration" | "stop_hook_summary")
        ),
        "user" | "human" => val
            .get("message")
            .and_then(|m| m.get("content"))
            .is_some_and(|content| {
                // A tool result quoting this notice does not interrupt the
                // main turn. Only a harness text row is a boundary.
                if content.as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block.get("type").and_then(|v| v.as_str()) == Some("tool_result")
                    })
                }) {
                    return false;
                }
                let text = extract_text_from_content(content);
                text.trim_start().starts_with("[Request interrupted")
            }),
        "assistant" => match val.get("message").and_then(|m| m.get("stop_reason")) {
            Some(reason) => reason.as_str() == Some("end_turn"),
            None => true,
        },
        _ => false,
    }
}

/// `text` whole when it fits `max` bytes, else its leading paragraphs that do
/// (cut only at a blank line), else `None`. The recap renders the first
/// paragraph only, so a cut inside it would show a truncated ask: abstain.
fn whole_paragraphs(text: &str, max: usize) -> Option<String> {
    if text.len() <= max {
        return Some(text.to_string());
    }
    let cut = text.floor_char_boundary(max);
    let at = text[..cut].rfind("\n\n")?;
    let head = text[..at].trim_end();
    (!head.is_empty()).then(|| head.to_string())
}

/// Longest stored `latest_ask`, in bytes. The recap renders at most
/// `INTENT_MAX_CHARS` of it, so the cut never reaches a rendered sentence.
const LATEST_ASK_MAX_BYTES: usize = 400;

/// Whether a typed prompt opens a new task, as opposed to an acknowledgement
/// ("go", "ok do it") that continues the one before it.
fn is_task_prompt(prose: &str) -> bool {
    crate::extraction::provenance::is_substantive(prose)
        && !crate::hooks::recap::is_acknowledgement(prose)
}

/// Extract a structured episode from JSONL transcript lines.
///
/// Pure function — no I/O, no engine access. Parses each line as JSON and
/// extracts fields from the Claude Code transcript format.
pub fn extract_episode(lines: &[&str], session_id: &str, project: &str) -> Episode {
    let mut sanitizer = crate::import::RowSanitizer::for_derived();
    extract_episode_from_rows(
        lines
            .iter()
            .filter_map(|line| episode_row(line, &mut sanitizer)),
        session_id,
        project,
    )
}

/// One transcript line as the episode extraction reads it: the row sanitized
/// for search, and the untouched row, which only user rows need (forwarded
/// notes and dreamer what-ifs, which the sanitizer blanks as CSR's own
/// words). Other rows carry `Null` as their raw twin: the extraction never
/// reads it for them, so they are not cloned.
fn episode_row(
    line: &str,
    sanitizer: &mut crate::import::RowSanitizer,
) -> Option<(serde_json::Value, serde_json::Value)> {
    let mut row: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let user_row = matches!(
        row.get("type").and_then(|v| v.as_str()),
        Some("user" | "human")
    );
    let raw = if user_row {
        row.clone()
    } else {
        serde_json::Value::Null
    };
    sanitizer.sanitize(&mut row);
    Some((row, raw))
}

/// [`extract_episode`] over the first `limit` bytes of a transcript file, one
/// line in memory at a time. The caller takes `limit` from the file's length
/// once, so a transcript Claude Code is still appending to cannot show this
/// reader rows the provenance reader never saw.
/// Same episode as reading the file whole, without holding the file or its
/// parsed rows. Errors on an unreadable file or invalid UTF-8, as
/// `read_to_string` did.
pub(crate) fn extract_episode_from_file(
    path: &Path,
    limit: u64,
    session_id: &str,
    project: &str,
) -> Result<Episode> {
    use std::io::{BufRead, Read};
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?.take(limit));
    let mut sanitizer = crate::import::RowSanitizer::for_derived();
    let mut failure: Option<std::io::Error> = None;
    let mut buf = Vec::new();
    let rows = std::iter::from_fn(|| loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(error) => {
                failure = Some(error);
                return None;
            }
        }
        match std::str::from_utf8(&buf) {
            Ok(line) => {
                if let Some(row) = episode_row(line, &mut sanitizer) {
                    return Some(row);
                }
            }
            Err(error) => {
                failure = Some(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
                return None;
            }
        }
    });
    let episode = extract_episode_from_rows(rows, session_id, project);
    match failure {
        Some(error) => Err(error.into()),
        None => Ok(episode),
    }
}

/// `messages` are sanitized for search; `raw` are the same rows untouched
/// (index-aligned), read only to recognise forwarded notes and dreamer
/// what-ifs, which the sanitizer blanks as CSR's own words.
pub(crate) fn extract_episode_from_messages(
    messages: &[serde_json::Value],
    raw: &[serde_json::Value],
    session_id: &str,
    project: &str,
) -> Episode {
    extract_episode_from_rows(
        messages
            .iter()
            .enumerate()
            .map(|(index, row)| (row, raw.get(index).unwrap_or(row))),
        session_id,
        project,
    )
}

/// The extraction proper: `(sanitized, raw)` row pairs in transcript order.
fn extract_episode_from_rows<S, R>(
    rows: impl IntoIterator<Item = (S, R)>,
    session_id: &str,
    project: &str,
) -> Episode
where
    S: std::borrow::Borrow<serde_json::Value>,
    R: std::borrow::Borrow<serde_json::Value>,
{
    let mut request = String::new();
    let mut investigated = HashSet::new();
    let mut files_modified = HashSet::new();
    let mut tools_used = HashSet::new();
    let mut completed = String::new();
    let mut error_signatures = Vec::new();
    let mut message_count: usize = 0;
    let mut approved_plan: Option<String> = None;
    // Main-session task state (TodoWrite / TaskCreate / TaskUpdate). A call
    // changes it only once a matching tool_result succeeds; sidechain rows
    // never touch it.
    let mut tasks = TaskState::default();
    // Recap evidence: the latest typed task, and what was provably completed
    // after it (reset whenever a newer task is asked).
    let mut segments: Vec<TaskSegment> = Vec::new();
    // Whether the first typed row was seen, and if so whether it was a CSR
    // agent prompt (then the transcript has no tasks at all).
    let mut opening_seen: Option<bool> = None;
    // Whether the assistant finished a turn since the latest task was asked.
    // A task prompt arriving while that turn is still open continues it.
    let mut turn_finished = true;
    // Bash tool_use ids whose command can commit or merge, with the command.
    let mut git_calls: HashMap<String, String> = HashMap::new();
    // The session's receipts, latest last, each with the index of the task
    // segment it was produced under (None before the first task).
    let mut completions: Vec<(CompletionReceipt, Option<usize>)> = Vec::new();
    // First and last row times, for the session's real duration.
    let mut first_row_at: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut last_row_at: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut watermark: Option<chrono::DateTime<chrono::Utc>> = None;
    // The latest task's failures: signatures of failed significant calls that
    // no later call with the same signature cleared, whether the last
    // significant call failed, and whether the task committed, merged or
    // installed. `calls` maps tool_use ids to what their result will mean;
    // `csr_calls` holds CSR's own tool calls, which mean nothing.
    let mut open_failures: HashSet<String> = HashSet::new();
    let mut final_failed = false;
    let mut closing_receipt = false;
    let mut calls: HashMap<String, CallSignature> = HashMap::new();
    let mut csr_calls: HashSet<String> = HashSet::new();
    // Whether the closing task is still going: a request is waiting for its
    // answer, or the assistant is mid-turn. `clean_end`: the last turn ended
    // as an assistant turn, not as an interruption. A remark or an
    // acknowledgement typed after a finished turn opens neither.
    let mut request_pending = false;
    let mut turn_in_flight = false;
    let mut clean_end = false;
    // The assistant's latest main-session text since the latest task began.
    let mut closing_text = String::new();
    // The literal last typed message (a forwarded note clears it).
    let mut latest_ask: Option<String> = None;
    // Recap evidence (`hooks::recap_evidence`): background tasks and what
    // closed them; the failed verification commands of the latest task, with
    // their exit codes, oldest first; the assistant's latest main-session text
    // whole, with its row's uuid (`None` when that text was CSR's own words).
    let mut background = crate::hooks::recap_evidence::BackgroundTracker::default();
    let mut gate_failures: Vec<(String, crate::hooks::recap_evidence::GateFailure)> = Vec::new();
    let mut closing_message: Option<(String, Option<String>)> = None;

    // Tool names whose file_path inputs count as "investigated"
    let read_tools: HashSet<&str> = ["Read", "Glob", "Grep"].into_iter().collect();
    // Tool names whose file_path inputs count as "modified"
    let write_tools: HashSet<&str> = ["Edit", "Write", "MultiEdit"].into_iter().collect();

    // Error signature patterns
    let error_patterns: &[&str] = &["error[", "Error:", "panic", "FAIL", "FAILED", "Exception"];

    for (row, raw_row) in rows {
        let val: &serde_json::Value = row.borrow();
        let raw_row: &serde_json::Value = raw_row.borrow();
        let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        message_count += 1;
        if let Some(at) = val
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(crate::temporal::parse_timestamp)
        {
            first_row_at = Some(first_row_at.map_or(at, |first| first.min(at)));
            last_row_at = Some(last_row_at.map_or(at, |last| last.max(at)));
            if matches!(msg_type, "user" | "human" | "assistant" | "system") {
                watermark = Some(watermark.map_or(at, |last| last.max(at)));
            }
        }
        // A legacy in-file subagent row. It still counts toward tools/files,
        // but never toward the recap's task, turn or completion state.
        let sidechain = val.get("isSidechain").and_then(|v| v.as_bool()) == Some(true);
        if !sidechain {
            if let Some(text) = notification_text(val, raw_row, msg_type) {
                background.notification(text);
            }
        }

        // Extract request from the first user message that survives provenance
        // filtering — command wrappers, caveats, and pasted CSR output are
        // plumbing, not the user's request (see extraction::provenance).
        if (msg_type == "user" || msg_type == "human") && request.is_empty() {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                let text = extract_text_from_content(content);
                if let Some(cleaned) = crate::extraction::provenance::extractable(&text) {
                    request = truncate_str(&cleaned, 200).to_string();
                }
            }
        }

        // Task segments: a typed prompt that opens a task starts a segment;
        // the work and receipts that follow belong to it. A prompt sent
        // mid-turn (no finished turn since the last task) is the same task
        // continued, so it joins the current segment's text instead.
        if msg_type == "user" || msg_type == "human" {
            // A transcript that opens with a CSR agent prompt (narrator,
            // analyst, labeler child) is CSR talking to itself: none of its
            // rows is the person's task. The importer skips it whole too.
            // The opening prompt is found the way the importer finds it
            // (opening_prompt_candidate: injected rows and harness wrappers
            // skipped), then any remaining hook wrapper is stripped, so a
            // `<system-reminder>` ahead of the agent signature cannot hide it.
            if opening_seen.is_none() && !sidechain {
                if let Some(text) = crate::import::opening_prompt_candidate(raw_row)
                    .map(|text| crate::extraction::provenance::strip_plumbing(&text))
                    .filter(|text| !text.trim().is_empty())
                {
                    opening_seen = Some(crate::import::is_csr_agent_prompt(&text));
                }
            }
            let typed = forwarded_note(raw_row).or_else(|| typed_prompt(val));
            // The literal last typed row decides. One that leaves no usable
            // prose (a pasted recap, a command-only wrapper) clears the
            // candidate: an older question must not stand in for it.
            // Only a real typed message can clear it, judged on the row as
            // written: not a tool-result carrier (the sanitizer may leave one
            // with an empty content array), and not a row with nothing in it.
            // A command wrapper the sanitizer empties out is still typed.
            let real_typed_row =
                typed_row_text(raw_row).is_some_and(|text| !text.trim().is_empty());
            if !sidechain && opening_seen != Some(true) && real_typed_row {
                latest_ask = match &typed {
                    Some(TypedPrompt { prose, note: false }) => {
                        whole_paragraphs(prose, LATEST_ASK_MAX_BYTES)
                    }
                    _ => None,
                };
            }
            if let Some(TypedPrompt { prose, note }) = typed {
                let is_request_prompt = !note && crate::hooks::recap::is_request(&prose);
                if opening_seen != Some(true) && (note || is_task_prompt(&prose)) {
                    let sent_at = val
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .and_then(crate::temporal::parse_timestamp);
                    match segments.last_mut() {
                        Some(task) if !turn_finished && !note && !task.note => {
                            // Without both times, assume a split message.
                            let quick = match (task.last_prompt_at, sent_at) {
                                (Some(before), Some(now)) => {
                                    (now - before).num_seconds() <= JOIN_WINDOW_SECS
                                }
                                _ => true,
                            };
                            // Only a prompt that joined moves the window: an
                            // excluded late addition must not let its own
                            // quick follow-up into the task's text.
                            if quick {
                                task.prose.push(' ');
                                task.prose.push_str(prose.trim());
                                task.last_prompt_at = sent_at.or(task.last_prompt_at);
                            }
                        }
                        _ => {
                            // Only a message the recap's own request check reads
                            // as an ask starts the outcome over. Anything else
                            // ("thanks for fixing the build") neither undoes
                            // the success said before it nor forgives a
                            // failure; a task it misses inherits the last
                            // one's state, the cost of one fail-closed rule.
                            if crate::hooks::recap::is_request(&prose) {
                                open_failures.clear();
                                gate_failures.clear();
                                final_failed = false;
                                closing_receipt = false;
                                closing_text.clear();
                            }
                            segments.push(TaskSegment {
                                prose,
                                note,
                                last_prompt_at: sent_at,
                                ..TaskSegment::default()
                            });
                        }
                    }
                }
                // Any typed prompt, an ack included, opens a turn: "go" after
                // a finished turn restarts work on the same task.
                turn_finished = false;
                if !sidechain && opening_seen != Some(true) && is_request_prompt {
                    request_pending = true;
                }
            }
        }
        let turn_ends = !sidechain && ends_turn(val, msg_type);
        if turn_ends {
            turn_finished = true;
            request_pending = false;
            turn_in_flight = false;
            // A user row here is "[Request interrupted by user]".
            clean_end = !matches!(msg_type, "user" | "human");
        } else if msg_type == "assistant" && !sidechain {
            turn_in_flight = true;
        }

        // Extract tool_use information
        if msg_type == "assistant" {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");

                        if block_type == "tool_use" {
                            if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                                if crate::import::is_csr_tool_use(block, name) {
                                    if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                                        csr_calls.insert(id.to_string());
                                    }
                                    continue;
                                }
                                if let (false, Some(id), Some(input)) = (
                                    sidechain,
                                    block.get("id").and_then(|v| v.as_str()),
                                    block.get("input"),
                                ) {
                                    calls.insert(
                                        id.to_string(),
                                        call_signature(
                                            name,
                                            input,
                                            val.get("cwd").and_then(|v| v.as_str()),
                                        ),
                                    );
                                }
                                tools_used.insert(name.to_string());
                                if !sidechain {
                                    background.tool_use(block);
                                }
                                if let (false, Some(task)) = (sidechain, segments.last_mut()) {
                                    task.tool_calls += 1;
                                }

                                if name == "Bash" && !sidechain {
                                    let command = block
                                        .get("input")
                                        .and_then(|i| i.get("command"))
                                        .and_then(|c| c.as_str());
                                    if let (Some(command), Some(id)) =
                                        (command, block.get("id").and_then(|v| v.as_str()))
                                    {
                                        if is_git_committing_command(command)
                                            || merge_command_pr(command).is_some()
                                        {
                                            git_calls.insert(id.to_string(), command.to_string());
                                        }
                                    }
                                }

                                if !sidechain {
                                    if let Some(op) = TaskOp::from_tool_use(name, block) {
                                        match block.get("id").and_then(|v| v.as_str()) {
                                            Some(id) => {
                                                tasks.pending.insert(id.to_string(), op);
                                            }
                                            // No id, so no result can ever answer
                                            // it (synthetic or legacy rows): apply.
                                            None => {
                                                let current = segments.len().checked_sub(1);
                                                for (claim, receipt, origin) in
                                                    tasks.apply(op, None, current)
                                                {
                                                    push_completion(
                                                        &mut completions,
                                                        origin,
                                                        &claim,
                                                        &receipt,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                if name == "ExitPlanMode" {
                                    if let Some(plan) = block
                                        .get("input")
                                        .and_then(|i| i.get("plan"))
                                        .and_then(|p| p.as_str())
                                    {
                                        approved_plan = Some(truncate_str(plan, 1500).to_string());
                                    }
                                }

                                if let Some(input) = block.get("input") {
                                    if let Some(fp) =
                                        input.get("file_path").and_then(|v| v.as_str())
                                    {
                                        if read_tools.contains(name) {
                                            investigated.insert(fp.to_string());
                                        }
                                        if write_tools.contains(name) {
                                            files_modified.insert(fp.to_string());
                                        }
                                    }
                                }
                            }
                        }

                        // Track last assistant text for `completed`. Provenance
                        // filtering strips quoted/mentioned text first, so an
                        // assistant message quoting CSR's own injected blocks
                        // contributes only its genuine prose (or nothing).
                        if block_type == "text" {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                let cleaned = crate::extraction::provenance::extractable(text);
                                if !sidechain {
                                    closing_message = cleaned.as_ref().map(|whole| {
                                        (
                                            truncate_str(whole, CLOSING_MESSAGE_MAX_BYTES)
                                                .to_string(),
                                            row_uuid(val),
                                        )
                                    });
                                }
                                if let Some(cleaned) = cleaned {
                                    completed = truncate_str(&cleaned, 300).to_string();
                                    if !sidechain {
                                        closing_text = completed.clone();
                                    }
                                }
                            }
                        }
                    }
                }
                // Also handle plain string content
                if let Some(text) = content.as_str() {
                    let cleaned = crate::extraction::provenance::extractable(text);
                    if !sidechain {
                        closing_message = cleaned.as_ref().map(|whole| {
                            (
                                truncate_str(whole, CLOSING_MESSAGE_MAX_BYTES).to_string(),
                                row_uuid(val),
                            )
                        });
                    }
                    if let Some(cleaned) = cleaned {
                        completed = truncate_str(&cleaned, 300).to_string();
                        if !sidechain {
                            closing_text = completed.clone();
                        }
                    }
                }
            }
        }

        // Extract error signatures from tool_result blocks; also bind
        // TaskCreate numeric ids from per-block tool_result text.
        if msg_type == "tool_result" || msg_type == "user" {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                let text = extract_text_from_content(content);
                for pattern in error_patterns {
                    if text.contains(pattern) {
                        // Extract a short context around the error
                        if let Some(pos) = text.find(pattern) {
                            let start = pos.saturating_sub(20);
                            let end = (pos + pattern.len() + 60).min(text.len());
                            // Safe UTF-8 boundary handling
                            let start = text.floor_char_boundary(start);
                            let end = text.floor_char_boundary(end);
                            let sig = text[start..end].trim().to_string();
                            if !error_signatures.contains(&sig) {
                                error_signatures.push(sig);
                            }
                        }
                    }
                }

                // Per-block TaskCreate id binding — needs tool_use_id, which the
                // merged text path above discards. Only the tool_result content
                // matching "Task #N created successfully" is relevant here.
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
                            continue;
                        }
                        let block_text = block
                            .get("content")
                            .map(extract_text_from_content)
                            .unwrap_or_default();
                        let git_command = block
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .and_then(|id| git_calls.get(id));
                        let failed = block.get("is_error").and_then(|v| v.as_bool()) == Some(true);
                        // A tool call that FAILED. Output that merely contains
                        // error words (a Read or Grep of source, a quoted test
                        // name) is not an error: that flagged most long sessions.
                        // A failed call with a message the patterns do not know
                        // ("Exit code 1", "File not found") still counts, unless
                        // it is a probe (`call_signature`), and a later call
                        // with the same signature that succeeded clears it.
                        if !sidechain {
                            let id = block.get("tool_use_id").and_then(|v| v.as_str());
                            let call = match id.and_then(|id| calls.remove(id)) {
                                Some(call) => call,
                                None if id.is_some_and(|id| csr_calls.contains(id)) => {
                                    CallSignature::default()
                                }
                                // A result whose call is not in the transcript.
                                None => CallSignature {
                                    failure_key: Some("?".into()),
                                    installs: false,
                                },
                            };
                            if let Some(key) = call.failure_key {
                                gate_failures.retain(|(open, _)| open != &key);
                                if failed {
                                    if let (Some(command), Some(exit_code)) = (
                                        crate::hooks::recap_evidence::gate_command(&key),
                                        crate::hooks::recap_evidence::exit_code(&block_text),
                                    ) {
                                        gate_failures.push((
                                            key.clone(),
                                            crate::hooks::recap_evidence::GateFailure {
                                                command,
                                                exit_code,
                                            },
                                        ));
                                    }
                                    open_failures.insert(key);
                                    final_failed = true;
                                } else {
                                    open_failures.remove(&key);
                                    final_failed = false;
                                }
                            }
                            if call.installs && !failed {
                                closing_receipt = true;
                            }
                        }
                        if let (Some(command), false, false) = (git_command, failed, sidechain) {
                            let made = commits_made(command, &block_text)
                                .into_iter()
                                .chain(merged_pr(command, &block_text));
                            for (sha, subject) in made {
                                closing_receipt = true;
                                if let Some(task) = segments.last_mut() {
                                    task.commits += 1;
                                }
                                push_completion(
                                    &mut completions,
                                    segments.len().checked_sub(1),
                                    &subject,
                                    &sha[..7],
                                );
                            }
                        }
                        if sidechain {
                            continue;
                        }
                        let tool_use_id = block.get("tool_use_id").and_then(|v| v.as_str());
                        if let Some(id) = tool_use_id {
                            background.tool_result(id, &block_text, failed);
                        }
                        if let Some(op) = tool_use_id.and_then(|id| tasks.pending.remove(id)) {
                            if !failed {
                                let current = segments.len().checked_sub(1);
                                for (claim, receipt, origin) in
                                    tasks.apply(op, Some(&block_text), current)
                                {
                                    push_completion(&mut completions, origin, &claim, &receipt);
                                }
                            }
                        } else if tool_use_id.is_none() && !failed {
                            // A result with no tool_use_id at all: bind it FIFO
                            // to a create that was applied without an id.
                            tasks.bind_unlabeled(&block_text);
                        }
                    }
                }
            }
        }
    }

    // Deleted tasks are not open work: the dir loader skips them, so the
    // transcript path must too, or an all-deleted session gets capped at
    // "partial" on one path and not the other (CodeRabbit). Filtered after the
    // event loop because task_id_map holds indices into `todos` during it.
    let mut todos = tasks.todos;
    todos.retain(|t| t.status != "deleted");

    // next_steps comes ONLY from structured task/todo state (first non-completed
    // item from TodoWrite or TaskCreate/TaskUpdate). Keyword-snippet harvesting
    // of free prose was the recurring self-pollution channel: in sessions about
    // CSR itself, prose legitimately mentions "next:"/"todo:" while describing
    // the extractor, and no content filter can tell that apart from a real next
    // step. Structured todos are authored as tasks, so they can't be mentions.
    let next_steps = todos
        .iter()
        .find(|t| t.status != "completed")
        .map(|t| t.content.clone());

    let (anchor_request, anchor_index) = anchor_segment(&segments);
    let (completions, origins): (Vec<CompletionReceipt>, Vec<Option<usize>>) =
        completions.into_iter().unzip();
    // Receipts read as the anchor's result only when every receipt the recap
    // shows was produced under the anchor segment; otherwise "Latest:".
    let displayed = crate::hooks::recap::displayed_receipts(&completions);
    let untied = |anchor: Option<usize>| {
        displayed
            .iter()
            .any(|(index, _)| anchor.is_none() || origins[*index] != anchor)
    };
    let completions_untied = untied(anchor_index);
    let made_under = |anchor: Option<usize>| -> Vec<usize> {
        origins
            .iter()
            .enumerate()
            .filter(|(_, origin)| anchor.is_some() && **origin == anchor)
            .map(|(index, _)| index)
            .collect()
    };
    let anchor_receipts = Some(made_under(anchor_index));
    let compact_anchor = latest_typed_segment(&segments).map(|(request, index)| CompactAnchor {
        request,
        completions_untied: untied(Some(index)),
        receipts: Some(made_under(Some(index))),
    });

    // Determine outcome (task-aware: incomplete todos cap success at partial)
    let closing_success = crate::extraction::has_success_signal(&closing_text);
    let late_errors = !open_failures.is_empty();
    // A test, build or lint that failed and was never run again to pass still
    // fails the work, whatever unrelated call came after it.
    let final_failed = final_failed
        || open_failures
            .iter()
            .any(|key| key.starts_with(VERIFY_KEY_PREFIX));
    let closing_ended = clean_end && !request_pending && !turn_in_flight;
    let outcome = compute_outcome(
        message_count,
        &OutcomeSignals {
            final_failed,
            proven: closing_receipt || closing_success,
            ended: closing_ended,
        },
        &todos,
    );
    let duration_minutes = match (first_row_at, last_row_at) {
        (Some(first), Some(last)) => {
            u32::try_from((last - first).num_minutes().max(0)).unwrap_or(u32::MAX)
        }
        _ => 0,
    };

    let (background_open, background_open_total) = background.open();
    let mut recap_extras = crate::hooks::recap_evidence::RecapExtras {
        gate_failure: gate_failures.last().map(|(_, failure)| failure.clone()),
        background_open,
        background_open_total,
        ..Default::default()
    };
    // Only a turn that ended cleanly has a closing message to quote.
    if let (true, Some((text, Some(row)))) = (closing_ended, closing_message) {
        if let Some((kind, text)) = crate::hooks::recap_evidence::closing_marker(&text) {
            let line = crate::hooks::recap_evidence::ClosingLine { text, row };
            match kind {
                crate::hooks::recap_evidence::ClosingKind::Next => {
                    recap_extras.closing_next = Some(line);
                }
                crate::hooks::recap_evidence::ClosingKind::Open => {
                    recap_extras.closing_open = Some(line);
                }
            }
        }
    }

    let mut investigated_vec: Vec<String> = investigated.into_iter().collect();
    investigated_vec.sort();
    let mut files_modified_vec: Vec<String> = files_modified.into_iter().collect();
    files_modified_vec.sort();
    let mut tools_used_vec: Vec<String> = tools_used.into_iter().collect();
    tools_used_vec.sort();

    Episode {
        schema: "v2".to_string(),
        session_id: session_id.to_string(),
        project: project.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        request,
        investigated: investigated_vec,
        completed,
        next_steps,
        blockers: None,
        outcome,
        error_signatures,
        tools_used: tools_used_vec,
        files_modified: files_modified_vec,
        message_count,
        duration_minutes,
        todos,
        approved_plan,
        prev_episode_id: None,
        anchors: Vec::new(),
        error_count: None,
        top_errors: Vec::new(),
        steer_count: None,
        steers: Vec::new(),
        instrumentation_version: None,
        correction_count: None,
        recap_evidence_version: Some(RECAP_EVIDENCE_VERSION),
        anchor_request,
        completions,
        completions_untied,
        anchor_receipts,
        compact_anchor,
        late_errors,
        final_failed,
        closing_receipt,
        closing_ended,
        outcome_version: Some(OUTCOME_VERSION),
        closing_success,
        latest_ask,
        watermark: watermark.map(|at| at.to_rfc3339()),
        recap_extras,
    }
}

/// Record a completion. Commit and task/todo receipts are capped
/// separately (`MAX_COMPLETIONS` each, oldest dropped), so a run of todo
/// completions can never evict the session's commits.
fn push_completion(
    completions: &mut Vec<(CompletionReceipt, Option<usize>)>,
    segment: Option<usize>,
    claim: &str,
    receipt: &str,
) {
    let claim = claim.split_whitespace().collect::<Vec<_>>().join(" ");
    if claim.is_empty() {
        return;
    }
    let kind = crate::hooks::recap::receipt_kind(receipt);
    // The same commit seen twice (a log run again, an amend printed twice) is
    // one receipt.
    if kind == crate::hooks::recap::ReceiptKind::Commit
        && completions.iter().any(|(done, _)| done.receipt == receipt)
    {
        return;
    }
    completions.push((
        CompletionReceipt {
            claim,
            receipt: receipt.to_string(),
        },
        segment,
    ));
    let same_kind = |(done, _): &(CompletionReceipt, Option<usize>)| {
        crate::hooks::recap::receipt_kind(&done.receipt) == kind
    };
    if completions.iter().filter(|entry| same_kind(entry)).count() > MAX_COMPLETIONS {
        if let Some(oldest) = completions.iter().position(same_kind) {
            completions.remove(oldest);
        }
    }
}

/// What the closing task left behind, as `compute_outcome` reads it.
struct OutcomeSignals {
    /// The last significant call failed.
    final_failed: bool,
    /// Progress was proven: a commit, a merge, an install, or a closing success
    /// phrase.
    proven: bool,
    /// The transcript ends on a finished assistant turn.
    ended: bool,
}

/// Infer the session outcome from the closing task: how it ended, what it left
/// failing, what it proved, and the structured task state.
///
/// - `failed`: it ended on a failing call and proved nothing.
/// - `partial`: it ended on a failing call but proved progress, or it was cut
///   off, or tasks are open.
/// - `success`: it ended on a finished turn whose last significant call did not
///   fail and nothing is open. A failure the session moved past is not held
///   against it; `late_errors` still records it. Failures a later call cleared
///   (the same command run again and passing) and probes (a `grep` with no
///   match) are not failing at all.
///
/// Incomplete todos cap the result at `"partial"` even when the rest reads as
/// success: unresolved task state means the session is not a clean win.
fn compute_outcome(message_count: usize, signals: &OutcomeSignals, todos: &[TodoItem]) -> String {
    if message_count < 3 {
        return "interrupted".to_string();
    }
    let open_todos = todos.iter().any(|t| t.status != "completed");
    let outcome = if signals.final_failed && !signals.proven {
        "failed"
    } else if signals.final_failed || !signals.ended || open_todos {
        "partial"
    } else {
        "success"
    };
    outcome.to_string()
}

/// Choose the most recent episode session that is not the current one.
pub fn pick_prev_episode(candidates: &[(String, String)], current_session: &str) -> Option<String> {
    candidates
        .iter()
        .filter(|(sid, _)| sid != current_session)
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(sid, _)| sid.clone())
}

/// Generate tags for an episode reflection.
pub fn episode_tags(episode: &Episode) -> Vec<String> {
    vec![
        "session_episode".to_string(),
        "schema_v2".to_string(),
        format!("project_{}", episode.project),
        format!("conv_{}", episode.session_id),
    ]
}

/// Store an episode as a reflection, replacing any existing episode for the same session.
pub async fn store_episode(engine: &Engine, episode: &Episode) -> Result<()> {
    store_episode_supported(
        engine,
        episode,
        crate::storage::artifact_provenance::InputEnvelope::default(),
        None,
    )
    .await
}

async fn store_episode_supported(
    engine: &Engine,
    episode: &Episode,
    inputs: crate::storage::artifact_provenance::InputEnvelope,
    ranges: Option<Vec<(usize, usize)>>,
) -> Result<()> {
    let _operation = engine.storage().operation_lock()?;
    let tags = episode_tags(episode);
    let conv_tag = format!("conv_{}", episode.session_id);

    // Find and delete existing episodes for this session
    let existing = engine
        .storage()
        .get_reflections_by_tag(&conv_tag, 10)
        .unwrap_or_default();
    for (id, _content, existing_tags, _ts) in &existing {
        if existing_tags.iter().any(|t| t == "session_episode") {
            let _ = engine.storage().delete_reflection(id);
        }
    }

    // Serialize episode to JSON
    let content = serde_json::to_string(episode)?;

    // Embed the episode content
    let emb = engine.embeddings().clone();
    let text_for_embed = content.clone();
    let embedding =
        tokio::task::spawn_blocking(move || emb.embed_single(&text_for_embed)).await??;

    // Generate a new ID and insert
    let id = uuid::Uuid::new_v4().to_string();
    engine.storage().insert_derived_reflection_ranges(
        &id,
        &content,
        &tags,
        &embedding,
        &inputs,
        &ranges.unwrap_or_else(|| vec![(0, content.chars().count()); inputs.inputs().len()]),
    )?;

    // Also insert into the in-memory search index. Nothing here reads the
    // acknowledgement, so a proxied hook does not wait for one: the daemon
    // applies the episode at its next poll, and the hook's one Sync is the
    // import's.
    {
        let mut idx = engine.search().write().await;
        idx.insert_reflection_unacked(id, embedding);
    }

    Ok(())
}

fn episode_support(
    episode: &Episode,
    content: &str,
    mut inputs: crate::storage::artifact_provenance::InputEnvelope,
    transcript_path: &Path,
    tasks: Option<crate::storage::artifact_provenance::InputEnvelope>,
) -> (
    crate::storage::artifact_provenance::InputEnvelope,
    Vec<(usize, usize)>,
) {
    use crate::storage::artifact_provenance::ArtifactInput;
    let mut ranges = vec![(0, content.chars().count()); inputs.inputs().len()];
    let field_range = |field: &str| {
        let value = serde_json::to_value(episode)
            .ok()
            .and_then(|v| v.get(field).cloned())
            .unwrap_or_default();
        let needle = format!("\"{field}\":{value}");
        content
            .find(&needle)
            .map(|at| {
                let start = content[..at].chars().count() + field.chars().count() + 3;
                (start, start + value.to_string().chars().count())
            })
            .unwrap_or((0, content.chars().count()))
    };
    for (field, text, channel) in [
        ("request", episode.request.as_str(), "user_message"),
        ("completed", episode.completed.as_str(), "assistant_message"),
    ] {
        if text.is_empty() {
            continue;
        }
        let quoted = inputs
            .inputs()
            .iter()
            .filter(|i| i.channel() == channel)
            .find_map(|i| i.quoted_span(text, None))
            .unwrap_or_else(|| ArtifactInput::unknown(text));
        inputs.push(quoted);
        ranges.push(field_range(field));
    }
    if let Some(plan) = &episode.approved_plan {
        // `extract_episode` observed an ExitPlanMode tool call. This records
        // that local observation, never an approval or user confirmation.
        inputs.push(crate::storage::artifact_backfill::local_observation(
            "observed_exit_plan_mode",
            "jsonl_tool_call",
            &transcript_path.to_string_lossy(),
            plan,
        ));
        ranges.push(field_range("approved_plan"));
    }
    if let Some(tasks) = tasks {
        for field in ["todos", "next_steps", "outcome"] {
            for input in tasks.inputs() {
                inputs.push(input.clone());
                ranges.push(field_range(field));
            }
        }
    }
    (inputs, ranges)
}

/// Best-effort instrumentation scan over the transcript at `transcript_path`.
/// Returns `None` on ANY failure — oversized file, the file having vanished
/// between the caller's existence check and this scan, or a parse error —
/// so the caller can leave the episode's instrumentation fields at `None`
/// ("never measured") without the Stop hook itself ever failing.
fn scan_instrumentation(
    transcript_path: &Path,
    limit: u64,
) -> Option<crate::transcript::instrumentation::SessionInstrumentation> {
    if limit > crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES {
        return None;
    }
    let parsed = crate::transcript::parse_transcript_limited(transcript_path, limit).ok()?;
    Some(crate::transcript::instrumentation::from_parsed(&parsed))
}

/// Read transcript, extract episode, and store it. Non-fatal wrapper.
pub async fn extract_and_store_episode(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    let session_id = input
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no session_id"))?;
    let transcript_path = input
        .transcript_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no transcript_path"))?;

    let tp = std::path::PathBuf::from(transcript_path);
    if !tp.exists() {
        anyhow::bail!("transcript not found: {}", transcript_path);
    }

    let project = resolve_project_from_cwd(&cwd.to_string_lossy());
    let project_name = project.as_deref().unwrap_or("unknown");

    // The transcript can be hundreds of MB: it is streamed a line at a time
    // here, never held whole (see `extract_episode_from_file`).
    // One snapshot length for both reads: Claude Code appends to the live
    // transcript, and a row seen by only one of them would break the match
    // between the episode and its provenance.
    let snapshot_len = std::fs::metadata(&tp)?.len();
    let mut provenance_inputs =
        crate::import::transcript_inputs_limited(engine.storage(), &tp, session_id, snapshot_len)
            .or_else(|_| {
            use std::io::Read;
            let mut raw = String::new();
            std::fs::File::open(&tp)?
                .take(snapshot_len)
                .read_to_string(&mut raw)?;
            Ok::<_, anyhow::Error>(crate::storage::artifact_provenance::InputEnvelope::new(
                vec![crate::storage::artifact_provenance::ArtifactInput::unknown(
                    &raw,
                )],
            ))
        })?;
    let mut task_inputs = None;

    let mut episode = extract_episode_from_file(&tp, snapshot_len, session_id, project_name)?;

    // Journal v2 instrumentation feeds (plan `.plans/journal-v2-mailbox-plan.md`
    // §3.3/§4.1): one extra streaming pass over the same transcript file
    // (already in page cache from the streaming read above for anything
    // under the size cap). Wrapped so ANY failure — oversized file,
    // transcript deleted between the existence check above and this scan,
    // malformed content — leaves the fields `None` ("never measured")
    // rather than ever failing the hook (repo CLAUDE.md: hooks must never
    // block Claude Code; plan §8 R4).
    if let Some(instrumentation) = scan_instrumentation(&tp, snapshot_len) {
        episode.error_count = Some(instrumentation.error_count);
        episode.top_errors = instrumentation.top_errors;
        episode.steer_count = Some(instrumentation.steer_count);
        episode.steers = instrumentation.steers;
        episode.instrumentation_version =
            Some(crate::transcript::instrumentation::STEER_FILTER_VERSION);
    }

    let intent_size_allowed = std::fs::metadata(&tp).is_ok_and(|metadata| {
        metadata.len() <= crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES
    });
    if std::env::var("CSR_NO_INTENT_CAPTURE").as_deref() != Ok("1")
        && !stop_hook_is_active(input)
        && intent_size_allowed
    {
        let high_water_key = format!("intent_high_water:{session_id}");
        let high_water = engine
            .storage()
            .get_meta(&high_water_key)?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        match crate::transcript::intent_events::extract_intent_events_incremental(
            &tp,
            session_id,
            project_name,
            engine.embeddings(),
            &high_water,
            snapshot_len,
        )
        .await
        {
            Ok(extraction) => {
                if let Err(error) = engine.storage().insert_intent_events(&extraction.events) {
                    eprintln!("CSR: intent event persist error (non-fatal): {error}");
                } else {
                    let high_water =
                        crate::transcript::intent_events::IntentHighWater::from(&extraction);
                    engine
                        .storage()
                        .set_meta(&high_water_key, &serde_json::to_string(&high_water)?)?;
                    episode.correction_count = Some(
                        engine
                            .storage()
                            .count_session_intent_events(
                                session_id,
                                crate::transcript::intent_events::IntentEventKind::Correction,
                            )?
                            .try_into()
                            .unwrap_or(u32::MAX),
                    );
                }
            }
            Err(error) => eprintln!("CSR: intent extraction error (non-fatal): {error}"),
        }
    }

    // Authoritative on-disk task directory overrides transcript-mined todos
    // when present and non-empty (Claude Code's ~/.claude/tasks/<session_id>/).
    // Empty vec is "no signal" — leave transcript-derived state alone.
    let task_dir_state = load_task_dir_state(session_id);
    if task_dir_state.is_none() {
        // None with the dir actually present = unreadable/format churn, the
        // silent-rot channel this release exists to close. Missing dir is the
        // normal no-tasks case and does not count.
        let dir_exists = dirs::home_dir()
            .map(|h| h.join(".claude/tasks").join(session_id).is_dir())
            .unwrap_or(false);
        if dir_exists {
            let _ = engine.storage().bump_aux_counter("tasks");
        }
    }
    if let Some(state) = task_dir_state {
        if state.parse_failures > 0 {
            // Files existed but didn't parse — the schema-drift signal (Codex:
            // all-failures used to read as "no tasks" and stay invisible).
            let _ = engine.storage().bump_aux_counter("tasks");
        }
        // Authoritative whenever the dir actually held task files — INCLUDING an
        // all-deleted session, which must override stale transcript tasks rather
        // than let them set next_steps and cap the outcome (Codex). A dir with
        // zero task files carries no signal; transcript state stands.
        if state.files_seen > state.parse_failures {
            task_inputs = Some(state.inputs);
            apply_task_dir_todos(&mut episode, state.todos);
        }
    }

    // Task-derived resolution proposals: a completed task whose subject matches a
    // chunk carrying a still-open verdict suggests that item is now resolved.
    // Proposals only — never verdicts (see Storage::insert_resolution_proposal).
    // Kill switch mirrors the narrative one. Fail-soft: proposal errors never
    // block the Stop hook.
    if std::env::var("CSR_NO_TASK_RESOLUTION").is_err() {
        propose_task_resolutions(engine.storage(), &episode.todos, project_name, session_id);
    }

    // AST anchors for modified files (cap 10 files; relative paths resolve to cwd)
    for f in episode.files_modified.iter().take(10) {
        let p = std::path::Path::new(f);
        let path = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        episode
            .anchors
            .extend(crate::extraction::anchors::capture_file_anchors(&path));
        if let Ok(text) = std::fs::read_to_string(&path) {
            provenance_inputs.push(crate::storage::artifact_backfill::local_observation(
                "local_file",
                "file_content",
                &path.to_string_lossy(),
                &text,
            ));
        } else {
            provenance_inputs.push(crate::storage::artifact_provenance::ArtifactInput::unknown(
                &path.to_string_lossy(),
            ));
        }
    }

    // Chain link: most recent episode for this project, excluding this session.
    // Exact project-tag match avoids LIKE '%project_foo%' matching 'project_foobar'.
    let project_tag = format!("project_{}", project_name);
    if let Ok(existing) = engine.storage().get_reflections_by_tag(&project_tag, 50) {
        let candidates: Vec<(String, String)> = existing
            .iter()
            .filter(|(_, _, tags, _)| {
                tags.iter().any(|t| t == "session_episode")
                    && tags.iter().any(|t| t == &project_tag)
            })
            .filter_map(|(_, content, _, ts)| {
                let v: serde_json::Value = serde_json::from_str(content).ok()?;
                Some((v.get("session_id")?.as_str()?.to_string(), ts.clone()))
            })
            .collect();
        episode.prev_episode_id = pick_prev_episode(&candidates, session_id);
        if let Some(parent) = &episode.prev_episode_id {
            for (id, content, _, _) in &existing {
                if serde_json::from_str::<serde_json::Value>(content)
                    .ok()
                    .is_some_and(|v| v["session_id"].as_str() == Some(parent.as_str()))
                {
                    provenance_inputs.push(engine.storage().artifact_input(
                        crate::storage::artifact_provenance::ArtifactKind::Reflection,
                        id,
                    )?);
                }
            }
        }
    }

    let content = serde_json::to_string(&episode)?;
    let (inputs, ranges) = episode_support(&episode, &content, provenance_inputs, &tp, task_inputs);
    store_episode_supported(engine, &episode, inputs, Some(ranges)).await?;

    // Persist anchors for fast birth-time symbol join (non-fatal)
    if let Err(e) =
        engine
            .storage()
            .replace_session_anchors(session_id, project_name, &episode.anchors)
    {
        eprintln!("CSR: anchor persist error (non-fatal): {}", e);
    }

    eprintln!(
        "CSR: episode stored (outcome={}, msgs={}, tools={}, anchors={})",
        episode.outcome,
        episode.message_count,
        episode.tools_used.len(),
        episode.anchors.len()
    );

    Ok(())
}

/// Upper bound on the still-open candidate set read per Stop hook.
const STILL_OPEN_CANDIDATE_LIMIT: usize = 500;

/// Significant tokens of a task subject: lowercase alphanumeric words longer
/// than three characters.
fn subject_tokens(subject: &str) -> Vec<String> {
    subject
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 3)
        .collect()
}

/// For each completed task, look for a chunk in this project whose latest
/// user-confirmed verdict is `still_open` and whose content carries the task
/// subject; write a proposal row for the best match (most subject tokens, ties
/// to the newest chunk). A human promotes a proposal with `csr_resolve`.
///
/// The probe starts from the verdicts, not the tasks: one indexed read of the
/// project's still-open chunks (at most [`STILL_OPEN_CANDIDATE_LIMIT`], newest
/// verdict first), and the function returns at once when there are none. A
/// project with no confirmed still-open verdict, the usual case, therefore does
/// no per-task work. This replaced an FTS5 search per task over the whole
/// project on every Stop (10.9 s in one live hook, 2,112 of 2,580 samples).
/// Each task is matched in memory against the candidates with the same rule
/// as before: at least 60% of the subject's significant tokens must appear
/// verbatim, identity rather than similarity.
///
/// The candidate set is a superset of what the FTS top-3 reached: a still-open
/// chunk ranked fourth by bm25 is now found and proposed. Bounded to 10
/// completed tasks — the transcript path caps todos at 10, but dir-loaded task
/// state is uncapped (CodeRabbit).
fn propose_task_resolutions(
    storage: &crate::storage::Storage,
    todos: &[TodoItem],
    project_name: &str,
    session_id: &str,
) {
    let completed: Vec<&TodoItem> = todos
        .iter()
        .filter(|t| t.status == "completed")
        .take(10)
        .collect();
    if completed.is_empty() {
        return;
    }
    let Ok(candidates) = storage.get_still_open_chunks(project_name, STILL_OPEN_CANDIDATE_LIMIT)
    else {
        return;
    };
    if candidates.is_empty() {
        return; // no confirmed still-open verdict — nothing can be proposed
    }
    let candidate_tokens: Vec<std::collections::HashSet<String>> = candidates
        .iter()
        .map(|c| {
            c.content
                .split(|ch: char| !ch.is_alphanumeric())
                .map(|w| w.to_lowercase())
                .collect()
        })
        .collect();
    for todo in completed {
        let subject_tokens = subject_tokens(&todo.content);
        if subject_tokens.is_empty() {
            continue;
        }
        // Require most of the subject's significant tokens verbatim in the
        // chunk before proposing — identity, not similarity (Codex:
        // single-word matches attached completed tasks to unrelated
        // still-open chunks).
        let best = candidates
            .iter()
            .zip(&candidate_tokens)
            .filter_map(|(chunk, tokens)| {
                let matched = subject_tokens
                    .iter()
                    .filter(|t| tokens.contains(*t))
                    .count();
                (matched * 10 >= subject_tokens.len() * 6) // >= 60% overlap
                    .then_some((matched, &chunk.timestamp, chunk))
            })
            .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let Some((_, _, hit)) = best else {
            continue;
        };
        let evidence = format!("task completed in session {session_id}: {}", todo.content);
        let _ =
            storage.insert_resolution_proposal(&hit.id, Some(&todo.content), &evidence, session_id);
    }
}

/// Authoritative final task state from `~/.claude/tasks/<session_id>/`.
/// None on any error (missing dir, unreadable) — fail-soft so the Stop hook
/// never errors or panics because of task-dir I/O.
fn load_task_dir_state(session_id: &str) -> Option<TaskDirState> {
    let dir = dirs::home_dir()?.join(".claude/tasks").join(session_id);
    load_task_state_from_dir(&dir)
}

/// Let the on-disk task list replace the transcript-mined todos, then redo the
/// todo-dependent parts of the episode. The outcome reads the closing task's own
/// success signal (`closing_success`), never the session-wide `completed`: a
/// success said under an early task must not come back as the session's result.
fn apply_task_dir_todos(episode: &mut Episode, todos: Vec<TodoItem>) {
    episode.todos = todos;
    episode.next_steps = episode
        .todos
        .iter()
        .find(|t| t.status != "completed")
        .map(|t| t.content.clone());
    episode.outcome = compute_outcome(
        episode.message_count,
        &OutcomeSignals {
            final_failed: episode.final_failed,
            proven: episode.closing_receipt || episode.closing_success,
            ended: episode.closing_ended,
        },
        &episode.todos,
    );
    episode.outcome_version = Some(OUTCOME_VERSION);
}

/// What a task-directory read found. `files_seen` counts task JSON files present
/// (including deleted ones); `parse_failures` counts files that existed but
/// could not be read/parsed — the schema-drift signal the aux counter needs
/// (Codex: `Some([])` from all-failures used to be indistinguishable from
/// "no tasks", leaving drift invisible).
struct TaskDirState {
    todos: Vec<TodoItem>,
    files_seen: usize,
    parse_failures: usize,
    inputs: crate::storage::artifact_provenance::InputEnvelope,
}

/// Read numeric `N.json` task files from a directory. Testable helper used by
/// `load_task_dir_state`; returns `None` only when the directory itself cannot
/// be read. Per-file parse failures are skipped (caller with Storage/Engine
/// could `bump_aux_counter("tasks")` — this fn stays Storage-free).
fn load_task_state_from_dir(dir: &Path) -> Option<TaskDirState> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut files: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "json" {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(n) = stem.parse::<u32>() else {
            continue;
        };
        files.push((n, path));
    }
    files.sort_by_key(|(n, _)| *n);

    let files_seen = files.len();
    let mut parse_failures = 0usize;
    let mut todos = Vec::new();
    let mut inputs = crate::storage::artifact_provenance::InputEnvelope::default();
    for (_, path) in files {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            parse_failures += 1;
            inputs.push(crate::storage::artifact_provenance::ArtifactInput::unknown(
                &path.to_string_lossy(),
            ));
            continue;
        };
        inputs.push(crate::storage::artifact_backfill::local_observation(
            "task_file",
            "file_content",
            &path.to_string_lossy(),
            &raw,
        ));
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            parse_failures += 1;
            continue;
        };
        let Some(subject) = val.get("subject").and_then(|s| s.as_str()) else {
            parse_failures += 1;
            continue;
        };
        let Some(status) = val.get("status").and_then(|s| s.as_str()) else {
            parse_failures += 1;
            continue;
        };
        if status == "deleted" {
            continue;
        }
        todos.push(TodoItem {
            content: subject.to_string(),
            status: status.to_string(),
        });
    }
    Some(TaskDirState {
        todos,
        files_seen,
        parse_failures,
        inputs,
    })
}

/// Handle the stop hook.
/// Always returns Ok(()) to never block Claude Code (C-1 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Import growing transcript for ALL sessions (real-time searchability)
    super::import_current_transcript(input, engine, cwd).await;

    // Write rolling session summary — survives Ctrl+C (session-end may not fire)
    if let Err(e) = write_rolling_summary(input, engine, cwd) {
        eprintln!("CSR: rolling summary error (non-fatal): {}", e);
    }

    // Extract and store structured episode (non-fatal)
    if let Err(e) = extract_and_store_episode(input, engine, cwd).await {
        eprintln!("CSR: episode extraction error (non-fatal): {}", e);
    }

    Ok(())
}

fn stop_hook_is_active(input: &HookInput) -> bool {
    input.stop_hook_active == Some(true)
}

/// Write a rolling "session_latest" reflection with current session state.
/// This is overwritten on every stop event, ensuring SessionStart always has
/// *something* to show even if the Haiku story generation never runs.
fn write_rolling_summary(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    if input.session_id.is_none() {
        return Ok(()); // No session ID → nothing to summarize
    }

    let project = resolve_project_from_cwd(&cwd.to_string_lossy());
    let project_name = project.as_deref().unwrap_or("unknown");

    // Build a minimal rolling summary from the latest enrichment
    let sessions = engine
        .storage()
        .get_recent_sessions(1, Some(project_name))
        .unwrap_or_default();

    let summary = if let Some(session) = sessions.first() {
        let title = session.summary.as_deref().unwrap_or("(active session)");
        // Don't roll up a meta/probe or bare-question session ("what were we
        // discussing recently?") — it would surface as a self-referential
        // RECENT SESSIONS line. Same gate the live continuity paths use.
        if !crate::extraction::provenance::is_substantive(title) || title.contains('?') {
            return Ok(());
        }
        let msg_count = session.total_messages;
        let enrichment_hint = session
            .enrichment
            .as_deref()
            .and_then(|e| {
                // Extract tools/files from heuristic enrichment
                let tools_line = e.lines().find(|l| l.starts_with("Tools: "));
                let files_line = e.lines().find(|l| l.starts_with("Files: "));
                match (tools_line, files_line) {
                    (Some(t), Some(f)) => Some(format!("{}\n{}", t, f)),
                    (Some(t), None) => Some(t.to_string()),
                    (None, Some(f)) => Some(f.to_string()),
                    _ => None,
                }
            })
            .unwrap_or_default();

        format!(
            "[Rolling] {} ({} msgs)\n{}",
            title, msg_count, enrichment_hint
        )
    } else {
        return Ok(()); // No sessions to summarize
    };

    // Write rolling summary to a well-known file that SessionStart can read.
    // Simpler than the reflection system — no embeddings needed, just text.
    // S-1 fix: sanitize project_name to prevent directory traversal
    let safe_name: String = project_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(255)
        .collect();
    let path = engine
        .data_dir()
        .join(format!("rolling-summary-{}.txt", safe_name));
    let _ = std::fs::write(&path, &summary);

    Ok(())
}

/// Extract text content from a Claude message content field.
/// Handles both string content and array-of-blocks format.
pub(crate) fn extract_text_from_content(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
            // Also extract tool_result content
            if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                if let Some(text) = block.get("content").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
        }
        return parts.join("\n");
    }
    String::new()
}

/// Truncate a string to at most `max_chars` characters at a valid UTF-8 boundary.
fn truncate_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let boundary = s.floor_char_boundary(max_chars);
    &s[..boundary]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propose_task_resolutions_only_for_still_open_matches() {
        use crate::import::ConversationChunk;
        use crate::provenance::Speaker;
        let storage = crate::storage::Storage::open_memory().unwrap();
        let mk = |id: &str, content: &str, status: Option<&str>| {
            let chunk = ConversationChunk {
                id: id.into(),
                conversation_id: format!("conv-{id}"),
                project_name: "proj".into(),
                timestamp: "2026-07-27T12:00:00Z".into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
            if let Some(s) = status {
                storage
                    .insert_resolutions(&[id.to_string()], s, "seed", None, "user_confirmed")
                    .unwrap();
            }
        };
        mk(
            "open",
            "fix registry offset checkpoint races",
            Some("still_open"),
        );
        mk(
            "done",
            "narrative cache invalidation stale",
            Some("resolved"),
        );
        mk("naked", "unrelated quantum topic", None);

        let todos = vec![
            TodoItem {
                content: "fix registry offset checkpoint races".into(),
                status: "completed".into(),
            },
            TodoItem {
                content: "narrative cache invalidation stale".into(),
                status: "completed".into(),
            },
            TodoItem {
                content: "unrelated quantum topic".into(),
                status: "pending".into(), // not completed — never proposes
            },
        ];
        propose_task_resolutions(&storage, &todos, "proj", "sess-1");
        // Only the still_open match yields a proposal; resolved + pending don't.
        assert_eq!(storage.count_resolution_proposals().unwrap(), 1);
        // Idempotent per (chunk, session).
        propose_task_resolutions(&storage, &todos, "proj", "sess-1");
        assert_eq!(storage.count_resolution_proposals().unwrap(), 1);
    }

    /// Seed one chunk in `project`; `verdict` is `(status, source)` for the ledger.
    fn seed_chunk(
        storage: &crate::storage::Storage,
        id: &str,
        project: &str,
        timestamp: &str,
        content: &str,
        verdict: Option<(&str, &str)>,
    ) {
        use crate::import::ConversationChunk;
        use crate::provenance::Speaker;
        let chunk = ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: project.into(),
            timestamp: timestamp.into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
        if let Some((status, source)) = verdict {
            storage
                .insert_resolutions(&[id.to_string()], status, "seed", None, source)
                .unwrap();
        }
    }

    fn completed(subject: &str) -> TodoItem {
        TodoItem {
            content: subject.into(),
            status: "completed".into(),
        }
    }

    #[test]
    fn propose_task_resolutions_finds_still_open_chunk_ranked_below_the_top_three() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let subject = "fix registry offset checkpoint races";
        // Four verdict-free chunks that out-rank the open one on bm25: short and
        // exact. FTS top-3 saw only these.
        for i in 0..4 {
            seed_chunk(
                &storage,
                &format!("decoy-{i}"),
                "proj",
                "2026-07-27T12:00:00Z",
                subject,
                None,
            );
        }
        seed_chunk(
            &storage,
            "open",
            "proj",
            "2026-07-27T12:00:00Z",
            "fix registry offset checkpoint races after the watcher restarts, with a long \
             tail of unrelated words about narrative caches, hooks, daemons and sockets",
            Some(("still_open", "user_confirmed")),
        );
        propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
        assert_eq!(
            storage.resolution_proposal_chunk_ids().unwrap(),
            vec!["open".to_string()]
        );
    }

    #[test]
    fn propose_task_resolutions_ignores_resolved_agent_and_retired_verdicts() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let ts = "2026-07-27T12:00:00Z";
        let subject = "fix registry offset checkpoint races";
        let confirmed_open = Some(("still_open", "user_confirmed"));
        seed_chunk(
            &storage,
            "resolved",
            "proj",
            ts,
            subject,
            Some(("resolved", "user_confirmed")),
        );
        seed_chunk(
            &storage,
            "agent-open",
            "proj",
            ts,
            subject,
            Some(("still_open", "agent")),
        );
        // Confirmed still_open, then confirmed resolved: the latest row wins.
        seed_chunk(&storage, "retired", "proj", ts, subject, confirmed_open);
        storage
            .insert_resolutions(
                &["retired".to_string()],
                "resolved",
                "seed",
                None,
                "user_confirmed",
            )
            .unwrap();
        // An agent row after a confirmed still_open does not retire it.
        seed_chunk(&storage, "kept", "proj", ts, subject, confirmed_open);
        storage
            .insert_resolutions(&["kept".to_string()], "resolved", "seed", None, "agent")
            .unwrap();
        // Another project's confirmed still_open is out of scope.
        seed_chunk(&storage, "elsewhere", "other", ts, subject, confirmed_open);
        propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
        assert_eq!(
            storage.resolution_proposal_chunk_ids().unwrap(),
            vec!["kept".to_string()]
        );
    }

    #[test]
    fn propose_task_resolutions_picks_best_overlap_then_newest() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let open = Some(("still_open", "user_confirmed"));
        let subject = "fix registry offset checkpoint races";
        // 3 of 4 significant tokens: passes 60% but loses to the full match.
        seed_chunk(
            &storage,
            "partial",
            "proj",
            "2026-07-30T00:00:00Z",
            "registry offset checkpoint",
            open,
        );
        seed_chunk(
            &storage,
            "full-old",
            "proj",
            "2026-07-01T00:00:00Z",
            subject,
            open,
        );
        seed_chunk(
            &storage,
            "full-new",
            "proj",
            "2026-07-20T00:00:00Z",
            subject,
            open,
        );
        propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
        assert_eq!(
            storage.resolution_proposal_chunk_ids().unwrap(),
            vec!["full-new".to_string()]
        );
    }

    #[test]
    fn propose_task_resolutions_has_no_candidates_without_confirmed_still_open() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let subject = "fix registry offset checkpoint races";
        for i in 0..50 {
            // Agent rows and resolved verdicts only: nothing is a candidate.
            let verdict = if i % 2 == 0 {
                ("still_open", "agent")
            } else {
                ("resolved", "user_confirmed")
            };
            seed_chunk(
                &storage,
                &format!("c{i}"),
                "proj",
                "2026-07-27T12:00:00Z",
                subject,
                Some(verdict),
            );
        }
        // The candidate read is empty, so the function returns before any
        // per-task work.
        assert!(storage
            .get_still_open_chunks("proj", 500)
            .unwrap()
            .is_empty());
        propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
        assert_eq!(storage.count_resolution_proposals().unwrap(), 0);
    }

    /// Timing evidence for the Stop hook: `STOPFTS_CHUNKS` chunks (default
    /// 20,000) of shared vocabulary and ten completed tasks.
    /// `cargo test --release --lib stop_probe_timing -- --nocapture`.
    #[test]
    fn stop_probe_timing_on_a_large_project() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let n: usize = std::env::var("STOPFTS_CHUNKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20_000);
        let vocab = [
            "registry",
            "offset",
            "checkpoint",
            "races",
            "narrative",
            "cache",
            "invalidation",
            "stale",
            "daemon",
            "socket",
            "watcher",
            "restart",
            "hook",
            "index",
            "vector",
            "sync",
        ];
        for i in 0..n {
            let content = format!(
                "{} {} {} {} {} {} chunk {i}",
                vocab[i % 16],
                vocab[(i / 3) % 16],
                vocab[(i / 7) % 16],
                vocab[(i / 11) % 16],
                vocab[(i / 13) % 16],
                vocab[(i / 17) % 16],
            );
            let id = format!("c{i}");
            seed_chunk(
                &storage,
                &id,
                "proj",
                "2026-07-27T12:00:00Z",
                &content,
                None,
            );
        }
        let todos: Vec<TodoItem> = (0..10)
            .map(|i| {
                completed(&format!(
                    "fix {} {} {} {} for session {i}",
                    vocab[i % 16],
                    vocab[(i + 3) % 16],
                    vocab[(i + 5) % 16],
                    vocab[(i + 9) % 16],
                ))
            })
            .collect();
        let started = std::time::Instant::now();
        propose_task_resolutions(&storage, &todos, "proj", "sess-1");
        eprintln!(
            "STOPFTS timing: {n} chunks, 10 tasks, no verdicts: {:?}",
            started.elapsed()
        );
        assert_eq!(storage.count_resolution_proposals().unwrap(), 0);
        // One confirmed still-open chunk that matches the first task.
        seed_chunk(
            &storage,
            "open",
            "proj",
            "2026-07-27T12:00:00Z",
            "fix registry races cache socket for session 0 noted",
            Some(("still_open", "user_confirmed")),
        );
        let started = std::time::Instant::now();
        propose_task_resolutions(&storage, &todos, "proj", "sess-1");
        eprintln!(
            "STOPFTS timing: {n} chunks, 10 tasks, 1 still-open verdict: {:?}",
            started.elapsed()
        );
    }

    /// Build a JSONL line for a user message.
    fn user_line(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "content": [{"type": "text", "text": text}]
            }
        })
        .to_string()
    }

    /// Build a JSONL line for an assistant message with tool_use blocks.
    fn assistant_tool_line(tools: &[(&str, &str)]) -> String {
        let mut blocks: Vec<serde_json::Value> = Vec::new();
        for (name, file_path) in tools {
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "name": name,
                "input": {"file_path": file_path}
            }));
        }
        serde_json::json!({
            "type": "assistant",
            "message": {
                "content": blocks
            }
        })
        .to_string()
    }

    /// Build a JSONL line for an assistant message with text.
    fn assistant_text_line(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": text}]
            }
        })
        .to_string()
    }

    /// Build a JSONL line for a tool_result with error content.
    #[test]
    fn test_extract_episode_from_transcript() {
        let lines_owned = [
            user_line("Please fix the authentication bug in the login handler"),
            assistant_tool_line(&[("Read", "/src/auth/login.rs")]),
            assistant_tool_line(&[("Grep", "/src/auth/mod.rs")]),
            assistant_tool_line(&[("Edit", "/src/auth/login.rs")]),
            assistant_text_line(
                "I've fixed the authentication bug. The issue was done and complete.",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

        let ep = extract_episode(&lines, "sess-123", "my-project");

        assert_eq!(ep.schema, "v2");
        assert_eq!(ep.session_id, "sess-123");
        assert_eq!(ep.project, "my-project");
        assert!(ep.request.contains("fix the authentication bug"));
        assert!(ep.investigated.contains(&"/src/auth/login.rs".to_string()));
        assert!(ep.investigated.contains(&"/src/auth/mod.rs".to_string()));
        assert!(ep
            .files_modified
            .contains(&"/src/auth/login.rs".to_string()));
        assert!(ep.tools_used.contains(&"Read".to_string()));
        assert!(ep.tools_used.contains(&"Grep".to_string()));
        assert!(ep.tools_used.contains(&"Edit".to_string()));
        assert!(ep.completed.contains("fixed the authentication bug"));
        assert_eq!(ep.outcome, "success"); // "fixed" and "complete" in completed text
        assert_eq!(ep.message_count, 5);
        assert!(ep.error_signatures.is_empty());
    }

    #[test]
    fn test_extract_episode_short_session() {
        let lines_owned = [user_line("Hello")];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

        let ep = extract_episode(&lines, "sess-short", "test-proj");

        assert_eq!(ep.outcome, "interrupted");
        assert_eq!(ep.message_count, 1);
        assert_eq!(ep.request, "Hello");
    }

    #[test]
    fn test_extract_episode_with_errors() {
        let lines_owned = [
            user_line("Build the project"),
            assistant_text_line("Let me try building..."),
            assistant_tool_line(&[("Read", "/Cargo.toml")]),
            dur_result(
                "error[E0308]: mismatched types\n  --> src/main.rs:42",
                true,
                "2026-10-21T10:00:00Z",
            ),
            assistant_text_line("There was a compilation error."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

        let ep = extract_episode(&lines, "sess-err", "my-project");

        assert_eq!(ep.outcome, "failed");
        assert!(!ep.error_signatures.is_empty());
        assert!(ep
            .error_signatures
            .iter()
            .any(|s| s.contains("error[E0308]")));
    }

    fn dur_timed(row: serde_json::Value, at: &str) -> String {
        let mut row = row;
        row["timestamp"] = serde_json::json!(at);
        row.to_string()
    }

    fn dur_typed(text: &str, at: &str) -> String {
        dur_timed(
            serde_json::json!({"type": "user", "promptSource": "typed", "message": {"content": text}}),
            at,
        )
    }

    fn dur_result(text: &str, is_error: bool, at: &str) -> String {
        dur_timed(
            serde_json::json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "toolu_x", "content": text, "is_error": is_error}
            ]}}),
            at,
        )
    }

    fn dur_done(at: &str) -> String {
        dur_timed(
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "text", "text": "I looked at the output."}],
                "stop_reason": "end_turn"
            }}),
            at,
        )
    }

    #[test]
    fn duration_is_the_span_between_the_first_and_last_row_times() {
        let ep = extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:30Z",
            ),
            dur_done("2026-10-21T10:05:00Z"),
            dur_typed(
                "Now update the changelog for that fix",
                "2026-10-21T12:15:00Z",
            ),
            dur_done("2026-10-21T12:20:45Z"),
        ]);
        assert_eq!(ep.duration_minutes, 140);
        // Rows with no time say nothing: 0, never a guess.
        let ep = extract(&[
            typed_line("Fix the login validation before the release", "typed"),
            assistant_text_line("Done."),
        ]);
        assert_eq!(ep.duration_minutes, 0);
    }

    #[test]
    fn a_long_session_is_not_failed_by_errors_it_worked_through() {
        let early_error = || {
            [
                dur_typed(
                    "Build the project and fix what breaks",
                    "2026-10-21T10:00:00Z",
                ),
                dur_result(
                    "error[E0308]: mismatched types",
                    true,
                    "2026-10-21T10:01:00Z",
                ),
                dur_done("2026-10-21T10:02:00Z"),
                dur_typed(
                    "Now update the changelog for that fix",
                    "2026-10-21T11:00:00Z",
                ),
            ]
        };
        // Errors only under the first task, a clean close: not "failed".
        let mut rows = early_error().to_vec();
        rows.push(dur_result(
            "changelog written",
            false,
            "2026-10-21T11:01:00Z",
        ));
        rows.push(dur_timed(
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "text", "text": "Updated."}], "stop_reason": "end_turn"
            }}),
            "2026-10-21T11:02:00Z",
        ));
        let ep = extract(&rows);
        assert!(!ep.error_signatures.is_empty(), "the signatures stay");
        assert!(!ep.late_errors);
        // The closing task finished and left nothing failing.
        assert_eq!(ep.outcome, "success");
        // An error under the closing task, no success: still "failed".
        let mut rows = early_error().to_vec();
        rows.push(dur_result(
            "error[E0599]: no method named x",
            true,
            "2026-10-21T11:01:00Z",
        ));
        let ep = extract(&rows);
        assert!(ep.late_errors);
        assert_eq!(ep.outcome, "failed");
        // Output that merely contains the words (a Read of source) is not a failed call.
        let mut rows = early_error().to_vec();
        rows.push(dur_result(
            "// Never panics. FAILED is a test name; Error: is a variant",
            false,
            "2026-10-21T11:01:00Z",
        ));
        assert!(!extract(&rows).late_errors);
        // A typed prompt that merely mentions FAILED is the person's words.
        let ep = extract(&[
            dur_typed(
                "Why does the suite print FAILED on windows",
                "2026-10-21T11:00:00Z",
            ),
            dur_done("2026-10-21T11:01:00Z"),
        ]);
        assert!(!ep.late_errors);
    }

    #[test]
    fn a_failed_call_counts_whatever_its_message_says() {
        // A shell failure prints no error[/Error:/panic pattern at all.
        let ep = extract(&[
            dur_typed("Run the release script and report", "2026-10-21T10:00:00Z"),
            dur_result("Exit code 1", true, "2026-10-21T10:01:00Z"),
            dur_result("File not found", true, "2026-10-21T10:02:00Z"),
            dur_done("2026-10-21T10:03:00Z"),
        ]);
        assert!(ep.late_errors);
        assert_eq!(ep.outcome, "failed");
    }

    #[test]
    fn a_success_said_under_an_earlier_task_does_not_close_the_session() {
        let ep = extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:00Z",
            ),
            dur_timed(
                serde_json::json!({"type": "assistant", "message": {
                    "content": [{"type": "text", "text": "Fixed. All tests pass."}],
                    "stop_reason": "end_turn"
                }}),
                "2026-10-21T10:02:00Z",
            ),
            dur_typed(
                "Now update the changelog for that fix",
                "2026-10-21T11:00:00Z",
            ),
            dur_result("changelog opened", false, "2026-10-21T11:01:00Z"),
        ]);
        assert!(!ep.closing_success, "the latest task has said nothing yet");
        assert_ne!(ep.outcome, "success");
        // The same words said under the closing task do count.
        let ep = extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:00Z",
            ),
            dur_done("2026-10-21T10:02:00Z"),
            dur_typed(
                "Now update the changelog for that fix",
                "2026-10-21T11:00:00Z",
            ),
            dur_timed(
                serde_json::json!({"type": "assistant", "message": {
                    "content": [{"type": "text", "text": "Done. The changelog is updated."}],
                    "stop_reason": "end_turn"
                }}),
                "2026-10-21T11:02:00Z",
            ),
        ]);
        assert!(ep.closing_success);
        assert_eq!(ep.outcome, "success");
    }

    #[test]
    fn the_task_dir_override_keeps_the_closing_tasks_success_not_the_sessions() {
        let mut ep = extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:00Z",
            ),
            dur_timed(
                serde_json::json!({"type": "assistant", "message": {
                    "content": [{"type": "text", "text": "Fixed. All tests pass."}],
                    "stop_reason": "end_turn"
                }}),
                "2026-10-21T10:02:00Z",
            ),
            dur_typed(
                "Now update the changelog for that fix",
                "2026-10-21T11:00:00Z",
            ),
            dur_result("changelog opened", false, "2026-10-21T11:01:00Z"),
        ]);
        // The session-wide `completed` still holds the early success.
        assert!(crate::extraction::has_success_signal(&ep.completed) || ep.completed.is_empty());
        apply_task_dir_todos(&mut ep, Vec::new());
        assert_ne!(ep.outcome, "success");
        apply_task_dir_todos(
            &mut ep,
            vec![TodoItem {
                content: "write the changelog".into(),
                status: "pending".into(),
            }],
        );
        assert_eq!(ep.next_steps.as_deref(), Some("write the changelog"));
        assert_ne!(ep.outcome, "success");
    }

    #[test]
    fn latest_ask_is_the_literal_last_typed_message_and_never_walks_back() {
        let ep = extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:00Z",
            ),
            dur_done("2026-10-21T10:02:00Z"),
            dur_typed(
                "Why did the second worker finish so early",
                "2026-10-21T10:30:00Z",
            ),
            dur_done("2026-10-21T10:32:00Z"),
        ]);
        assert_eq!(
            ep.latest_ask.as_deref(),
            Some("Why did the second worker finish so early")
        );
        // An acknowledgement is still the last thing typed: no walking back.
        let ep = extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:00Z",
            ),
            dur_done("2026-10-21T10:02:00Z"),
            dur_typed("ok go", "2026-10-21T10:30:00Z"),
        ]);
        assert_eq!(ep.latest_ask.as_deref(), Some("ok go"));
        // A sidechain row and a tool result are not typed messages.
        let mut side = serde_json::json!({"type": "user", "promptSource": "typed",
            "isSidechain": true, "message": {"content": "Summarize the diff for the agent"}});
        side["timestamp"] = serde_json::json!("2026-10-21T10:40:00Z");
        let ep = extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:00Z",
            ),
            dur_done("2026-10-21T10:02:00Z"),
            side.to_string(),
            dur_result("done", false, "2026-10-21T10:41:00Z"),
        ]);
        assert_eq!(
            ep.latest_ask.as_deref(),
            Some("Fix the login validation before the release")
        );
        // No typed row at all: nothing.
        assert!(extract(&[dur_done("2026-10-21T10:02:00Z")])
            .latest_ask
            .is_none());
    }

    #[test]
    fn a_last_typed_row_without_prose_clears_the_ask_it_never_walks_back() {
        let ask = "Why did the second worker finish so early?";
        // A command-only wrapper typed after the question.
        let ep = extract(&[
            dur_typed(ask, "2026-10-21T10:00:00Z"),
            dur_done("2026-10-21T10:02:00Z"),
            dur_typed(
                "<command-name>/clear</command-name>\n<command-message>clear</command-message>",
                "2026-10-21T10:30:00Z",
            ),
        ]);
        assert!(ep.latest_ask.is_none(), "{:?}", ep.latest_ask);
        // A recap pasted back.
        let ep = extract(&[
            dur_typed(ask, "2026-10-21T10:00:00Z"),
            dur_done("2026-10-21T10:02:00Z"),
            dur_typed(
                "recap [3m ago]: Fix the composer. [[CSR:RECAP]]",
                "2026-10-21T10:30:00Z",
            ),
        ]);
        assert!(ep.latest_ask.is_none(), "{:?}", ep.latest_ask);
        // A tool result is not a typed row: the question stands.
        let ep = extract(&[
            dur_typed(ask, "2026-10-21T10:00:00Z"),
            dur_result("ok", false, "2026-10-21T10:02:00Z"),
        ]);
        assert_eq!(ep.latest_ask.as_deref(), Some(ask));
    }

    #[test]
    fn a_sanitized_tool_result_carrier_does_not_clear_the_ask() {
        let ask = "Why did the second worker finish so early?";
        let csr_call = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "csr-1", "name": "csr_reflect_on_past", "input": {"query": "history"}}
        ], "stop_reason": "end_turn"}});
        // The sanitizer drops this CSR-only result and leaves an empty content array.
        let carrier = serde_json::json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "csr-1", "content": "Error: not found in CSR memory"}
        ]}});
        let ep = extract(&[
            dur_typed(ask, "2026-10-21T10:00:00Z"),
            dur_timed(csr_call, "2026-10-21T10:01:00Z"),
            dur_timed(carrier, "2026-10-21T10:02:00Z"),
        ]);
        assert_eq!(ep.latest_ask.as_deref(), Some(ask));
        // A typed row with nothing in it is not a message either.
        let ep = extract(&[
            dur_typed(ask, "2026-10-21T10:00:00Z"),
            dur_typed("   ", "2026-10-21T10:02:00Z"),
        ]);
        assert_eq!(ep.latest_ask.as_deref(), Some(ask));
    }

    #[test]
    fn a_cut_ask_is_never_stored() {
        let long = format!("Can you check {}?", "界".repeat(150));
        assert!(long.len() > LATEST_ASK_MAX_BYTES);
        let ep = extract(&[
            dur_typed(&long, "2026-10-21T10:00:00Z"),
            dur_done("2026-10-21T10:02:00Z"),
        ]);
        assert!(ep.latest_ask.is_none(), "{:?}", ep.latest_ask);
        // A short first paragraph in front of a long message is kept whole.
        let text = format!(
            "Why did the second worker finish so early?\n\n{}",
            "detail ".repeat(100)
        );
        let ep = extract(&[
            dur_typed(&text, "2026-10-21T10:00:00Z"),
            dur_done("2026-10-21T10:02:00Z"),
        ]);
        assert_eq!(
            ep.latest_ask.as_deref(),
            Some("Why did the second worker finish so early?")
        );
    }

    #[test]
    fn only_a_message_the_request_check_accepts_resets_the_outcome() {
        use crate::hooks::recap::is_request;
        let closing = |remark: &str| {
            extract(&[
                dur_typed(
                    "Fix the login validation before the release",
                    "2026-10-21T10:00:00Z",
                ),
                dur_timed(
                    serde_json::json!({"type": "assistant", "message": {
                        "content": [{"type": "text", "text": "Fixed. All tests pass."}],
                        "stop_reason": "end_turn"
                    }}),
                    "2026-10-21T10:02:00Z",
                ),
                dur_typed(remark, "2026-10-21T10:10:00Z"),
            ])
        };
        // Remarks: the request check says no, so the success stands.
        for remark in [
            "Thanks for fixing the build",
            "Great work helping us fix the build",
        ] {
            assert!(!is_request(remark), "{remark}");
            let ep = closing(remark);
            assert!(ep.closing_success, "{remark}");
            assert_eq!(ep.outcome, "success", "{remark}");
        }
        // An ask the check accepts starts over.
        let ask = "Can you bump the version?";
        assert!(is_request(ask));
        assert!(!closing(ask).closing_success);
        // "Great now fix the tests": whatever the check says decides it, with
        // no second rule to override it.
        let mixed = "Great now fix the tests";
        assert_eq!(!is_request(mixed), closing(mixed).closing_success);
        // A remark after a failure does not forgive it.
        let ep = extract(&[
            dur_typed("Run the release script and report", "2026-10-21T10:00:00Z"),
            dur_result("Exit code 1", true, "2026-10-21T10:01:00Z"),
            dur_done("2026-10-21T10:02:00Z"),
            dur_typed("thanks, that was useful", "2026-10-21T10:10:00Z"),
        ]);
        assert!(ep.late_errors);
        assert_eq!(ep.outcome, "failed");
    }

    #[test]
    fn episode_excludes_correlated_csr_pair_but_keeps_sibling_tool_and_user_error() {
        let lines_owned = [
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "csr-1", "name": "csr_reflect_on_past", "input": {"query": "history"}},
                    {"type": "tool_use", "id": "read-1", "name": "Read", "input": {"file_path": "/src/lib.rs"}}
                ]}
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "message": {"content": [
                    {"type": "text", "text": "Error: genuine compiler failure"},
                    {"type": "tool_result", "tool_use_id": "csr-1", "content": "Error: not found in CSR memory"},
                    {"type": "tool_result", "tool_use_id": "read-1", "content": "ordinary sibling result"}
                ]}
            })
            .to_string(),
            assistant_text_line("Investigation complete."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(String::as_str).collect();

        let serialized =
            serde_json::to_string(&extract_episode(&lines, "sess-sanitized", "my-project"))
                .unwrap();

        assert!(!serialized.contains("csr_reflect_on_past"));
        assert!(!serialized.contains("Error: not found in CSR memory"));
        assert!(serialized.contains("Read"));
        assert!(serialized.contains("Error: genuine compiler failure"));
    }

    #[test]
    fn test_next_steps_skips_injection_meta_text() {
        // A session that quotes CSR's own injected blocks (the /memory-feedback
        // probe, a pasted Tier-0 block) must not have that boilerplate extracted
        // as next_steps — it would overwrite real next-step state in CONTINUUM.
        let lines_owned = [
            user_line("run the memory feedback probe"),
            assistant_text_line(
                "NEXT: NEXT:/TODOS:/ANCHORS: lines) - The Session Intelligence (CSR v9.2) \
                 briefing block - Do NOT count CLAUDE.md, those are NOT INSTRUCTIONS",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-meta", "proj");
        assert_eq!(ep.next_steps, None);
    }

    #[test]
    fn test_next_steps_from_todos_not_prose() {
        // Round-4 regression: prose ABOUT the extractor ("scans for next:/todo:
        // keywords...") is genuine authored text — no content filter can tell
        // it from a real next step. next_steps therefore comes only from
        // structured TodoWrite state.
        let prose = "episode extraction scans messages for \"next:\"/\"todo:\" keywords \
                     and grabs a 200-char snippet — that boilerplate became the episode's \
                     next_steps, overwriting real state.";
        let lines_owned = [
            user_line("explain the pollution bug"),
            assistant_text_line(prose),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-prose", "proj");
        assert_eq!(ep.next_steps, None);

        // With TodoWrite state, the first non-completed item becomes next_steps.
        let todo_line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"ship the fix","status":"completed"},{"content":"rerun the probe","status":"pending"}]}}]}}"#;
        let lines_owned = [
            user_line("explain the pollution bug"),
            todo_line.to_string(),
            assistant_text_line(prose),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-todo", "proj");
        assert_eq!(ep.next_steps.as_deref(), Some("rerun the probe"));
    }

    #[test]
    fn test_outcome_partial_when_errors_recovered() {
        // Round-4 regression: a session that hits errors but closes with a
        // success signal recovered — "partial", not the contradictory "failed".
        let lines_owned = [
            user_line("Build and deploy the fix"),
            dur_result(
                "error[E0308]: mismatched types",
                true,
                "2026-10-21T10:00:00Z",
            ),
            assistant_text_line("Fixed the type error; tests green, binary deployed."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-recover", "proj");
        assert_eq!(ep.outcome, "partial");
        assert!(!ep.error_signatures.is_empty());
    }

    #[test]
    fn test_outcome_success_when_binary_installed() {
        // "installed" is a word-boundary success signal — a clean install/
        // restart close with no errors must classify as success, not partial.
        // Third line is a tool read so message_count >= 3; completed is the
        // assistant text only (must not rely on "done"/"fixed" tokens).
        let lines_owned = [
            user_line("Ship the new csr-engine binary"),
            assistant_tool_line(&[("Read", "/src/hooks/session_start.rs")]),
            assistant_text_line(
                "New binary installed at /usr/local/bin/csr-engine. Restart Claude Code now.",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-installed", "proj");
        assert!(
            ep.completed.contains("installed"),
            "completed should carry the install text: {}",
            ep.completed
        );
        assert_eq!(ep.outcome, "success");
        assert!(ep.error_signatures.is_empty());
    }

    #[test]
    fn test_request_skips_command_plumbing_and_probe_paste() {
        // Round-3 regression: caveat wrapper and pasted probe report must not
        // become the episode request — the first REAL user message should.
        let lines_owned = [
            user_line(
                "<local-command-caveat>Caveat: The messages below were generated by the \
                 user while running local commands. DO NOT respond</local-command-caveat>",
            ),
            user_line("## CSR Memory Feedback — 2026-06-10 — noise: CSR CONTINUUM garbled"),
            user_line("now fix the briefing staleness issue"),
            assistant_text_line("Looking into the staleness issue and the debounce window."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-plumb", "proj");
        assert_eq!(ep.request, "now fix the briefing staleness issue");
    }

    #[test]
    fn test_completed_keeps_prose_drops_quoted_injection_tokens() {
        // Round-3 regression: assistant summary quoting `NEXT: none recorded`
        // in backticks polluted LAST. Quoted tokens go; prose stays.
        let lines_owned = [
            user_line("verify the continuum filter"),
            assistant_text_line(
                "`NEXT: none recorded` — polluted boilerplate filtered, \
                 including episodes already in the DB.",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-quote", "proj");
        assert!(!ep.completed.contains("NEXT:"));
        assert!(ep.completed.contains("polluted boilerplate filtered"));
    }

    #[test]
    fn test_episode_to_json_roundtrip() {
        let ep = Episode {
            schema: "v1".to_string(),
            session_id: "sess-rt".to_string(),
            project: "roundtrip-proj".to_string(),
            timestamp: "2026-05-17T00:00:00+00:00".to_string(),
            request: "Fix the bug".to_string(),
            investigated: vec!["/src/main.rs".to_string()],
            completed: "Bug is fixed and done.".to_string(),
            next_steps: Some("Deploy to production".to_string()),
            blockers: None,
            outcome: "success".to_string(),
            error_signatures: vec![],
            tools_used: vec!["Read".to_string(), "Edit".to_string()],
            files_modified: vec!["/src/main.rs".to_string()],
            message_count: 10,
            duration_minutes: 5,
            todos: vec![],
            approved_plan: None,
            prev_episode_id: None,
            anchors: vec![],
            error_count: None,
            top_errors: vec![],
            steer_count: None,
            steers: vec![],
            instrumentation_version: None,
            correction_count: None,
            recap_evidence_version: None,
            anchor_request: None,
            completions: vec![],
            completions_untied: false,
            anchor_receipts: None,
            compact_anchor: None,
            late_errors: false,
            closing_success: false,
            closing_ended: false,
            final_failed: false,
            closing_receipt: false,
            outcome_version: None,
            latest_ask: None,
            watermark: None,
            recap_extras: Default::default(),
        };

        let json = serde_json::to_string(&ep).unwrap();
        let deserialized: Episode = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.schema, "v1");
        assert_eq!(deserialized.session_id, "sess-rt");
        assert_eq!(deserialized.project, "roundtrip-proj");
        assert_eq!(deserialized.request, "Fix the bug");
        assert_eq!(deserialized.investigated, vec!["/src/main.rs"]);
        assert_eq!(deserialized.completed, "Bug is fixed and done.");
        assert_eq!(
            deserialized.next_steps,
            Some("Deploy to production".to_string())
        );
        assert_eq!(deserialized.blockers, None);
        assert_eq!(deserialized.outcome, "success");
        assert!(deserialized.error_signatures.is_empty());
        assert_eq!(deserialized.tools_used, vec!["Read", "Edit"]);
        assert_eq!(deserialized.files_modified, vec!["/src/main.rs"]);
        assert_eq!(deserialized.message_count, 10);
        assert_eq!(deserialized.duration_minutes, 5);
    }

    #[test]
    fn test_episode_tags() {
        let ep = Episode {
            schema: "v1".to_string(),
            session_id: "sess-abc".to_string(),
            project: "cool-project".to_string(),
            timestamp: String::new(),
            request: String::new(),
            investigated: vec![],
            completed: String::new(),
            next_steps: None,
            blockers: None,
            outcome: "partial".to_string(),
            error_signatures: vec![],
            tools_used: vec![],
            files_modified: vec![],
            message_count: 0,
            duration_minutes: 0,
            todos: vec![],
            approved_plan: None,
            prev_episode_id: None,
            anchors: vec![],
            error_count: None,
            top_errors: vec![],
            steer_count: None,
            steers: vec![],
            instrumentation_version: None,
            correction_count: None,
            recap_evidence_version: None,
            anchor_request: None,
            completions: vec![],
            completions_untied: false,
            anchor_receipts: None,
            compact_anchor: None,
            late_errors: false,
            closing_success: false,
            closing_ended: false,
            final_failed: false,
            closing_receipt: false,
            outcome_version: None,
            latest_ask: None,
            watermark: None,
            recap_extras: Default::default(),
        };

        let tags = episode_tags(&ep);

        assert_eq!(tags.len(), 4);
        assert!(tags.contains(&"session_episode".to_string()));
        assert!(tags.contains(&"schema_v2".to_string()));
        assert!(tags.contains(&"project_cool-project".to_string()));
        assert!(tags.contains(&"conv_sess-abc".to_string()));
    }

    #[test]
    fn episode_v2_extracts_todos_and_plan() {
        let lines = vec![
            r#"{"type":"user","message":{"content":"fix the auth bug"}}"#,
            r###"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"## Plan\n1. Fix validate_token\n2. Add test"}}]}}"###,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"fix validate_token","status":"completed"},{"content":"add regression test","status":"pending"}]}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed the validation."}]}}"#,
        ];
        let ep = extract_episode(&lines, "sess-1", "proj");
        assert_eq!(ep.schema, "v2");
        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[1].content, "add regression test");
        assert_eq!(ep.todos[1].status, "pending");
        let plan = ep.approved_plan.expect("plan captured");
        assert!(plan.contains("Fix validate_token"));
        assert!(ep.prev_episode_id.is_none());
        assert!(ep.anchors.is_empty());
    }

    #[test]
    fn episode_support_keeps_intent_span_and_observed_plan_below_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let raw = concat!(
            r#"{"type":"user","uuid":"u","message":{"content":"fix auth"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"Fix auth then test"}}]}}"#,
            "\n"
        );
        std::fs::write(&path, raw).unwrap();
        let storage = crate::storage::Storage::open_memory().unwrap();
        let episode = extract_episode(&raw.lines().collect::<Vec<_>>(), "session", "project");
        let inputs = crate::import::transcript_inputs(&storage, &path, "session").unwrap();
        let content = serde_json::to_string(&episode).unwrap();
        let (support, ranges) = episode_support(&episode, &content, inputs, &path, None);
        let plan = support
            .inputs()
            .iter()
            .position(|i| i.channel() == "observed_exit_plan_mode")
            .unwrap();
        assert_eq!(
            support.inputs()[plan].trust(),
            crate::provenance::TrustTier::TrustedTool
        );
        assert!(content
            .chars()
            .skip(ranges[plan].0)
            .take(ranges[plan].1 - ranges[plan].0)
            .collect::<String>()
            .contains("Fix auth"));
        assert!(support
            .inputs()
            .iter()
            .zip(&ranges)
            .any(|(i, r)| i.channel() == "user_message" && i.text() == "fix auth" && r.0 > 0));
        assert!(support.floor() <= crate::provenance::TrustTier::TrustedTool);
    }

    #[test]
    fn episode_v2_keeps_last_todowrite_only() {
        let lines = vec![
            r#"{"type":"user","message":{"content":"task"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"old","status":"pending"}]}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"new","status":"in_progress"}]}}]}}"#,
        ];
        let ep = extract_episode(&lines, "sess-1", "proj");
        assert_eq!(ep.todos.len(), 1);
        assert_eq!(ep.todos[0].content, "new");
    }

    #[test]
    fn chain_link_finds_previous_episode_id() {
        // Pure-logic test for the helper that picks prev episode from candidates:
        // (session_id, timestamp) pairs, excluding the current session.
        let candidates = vec![
            ("sess-old".to_string(), "2026-06-09T10:00:00Z".to_string()),
            (
                "sess-current".to_string(),
                "2026-06-10T09:00:00Z".to_string(),
            ),
            ("sess-mid".to_string(), "2026-06-10T08:00:00Z".to_string()),
        ];
        assert_eq!(
            pick_prev_episode(&candidates, "sess-current"),
            Some("sess-mid".to_string())
        );
        assert_eq!(pick_prev_episode(&[], "x"), None);
    }

    #[test]
    fn episode_v1_and_v2_without_instrumentation_still_deserialize() {
        // v1: no todos/approved_plan/prev_episode_id/anchors, and (this
        // extension) no instrumentation fields either.
        let v1 = r#"{"schema":"v1","session_id":"s","project":"p","timestamp":"t",
            "request":"r","investigated":[],"completed":"c","next_steps":null,
            "blockers":null,"outcome":"partial","error_signatures":[],"tools_used":[],
            "files_modified":[],"message_count":1,"duration_minutes":0}"#;
        let ep: Episode = serde_json::from_str(v1).expect("v1 compat");
        assert!(ep.todos.is_empty());
        assert!(ep.approved_plan.is_none());
        assert!(ep.error_count.is_none());
        assert!(ep.top_errors.is_empty());
        assert!(ep.steer_count.is_none());
        assert!(ep.steers.is_empty());

        // v2: has the working-state fields but predates instrumentation
        // (plan §4.1) — must still deserialize, with the four new fields
        // defaulting to "never measured", not a deserialize error.
        let v2 = r#"{"schema":"v2","session_id":"s","project":"p","timestamp":"t",
            "request":"r","investigated":[],"completed":"c","next_steps":null,
            "blockers":null,"outcome":"success","error_signatures":[],"tools_used":[],
            "files_modified":[],"message_count":1,"duration_minutes":0,
            "todos":[{"content":"x","status":"completed"}],"approved_plan":null,
            "prev_episode_id":null,"anchors":[]}"#;
        let ep2: Episode = serde_json::from_str(v2).expect("v2 (pre-instrumentation) compat");
        assert_eq!(ep2.todos.len(), 1);
        assert!(ep2.error_count.is_none());
        assert!(ep2.top_errors.is_empty());
        assert!(ep2.steer_count.is_none());
        assert!(ep2.steers.is_empty());
        assert!(ep2.correction_count.is_none());
    }

    // --- extract_and_store_episode instrumentation wiring (plan §4.1) ---

    fn instrumentation_test_engine() -> (Engine, std::sync::Arc<crate::storage::Storage>) {
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
        let search = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::search::SearchEngine::new(100),
        ));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            std::path::PathBuf::from("/tmp"),
        );
        (engine, storage)
    }

    /// Fetch and deserialize the stored `session_episode` reflection for
    /// `session_id`, panicking with a descriptive message if none was
    /// stored — tests call this only after asserting `extract_and_store_episode`
    /// returned `Ok`.
    fn stored_episode(storage: &crate::storage::Storage, session_id: &str) -> Episode {
        let rows = storage
            .get_reflections_by_tag(&format!("conv_{session_id}"), 10)
            .unwrap();
        let (_, content, _, _) = rows
            .into_iter()
            .find(|(_, _, tags, _)| tags.iter().any(|t| t == "session_episode"))
            .expect("episode reflection must have been stored");
        serde_json::from_str(&content).expect("stored episode must deserialize")
    }

    #[tokio::test]
    async fn episode_carries_instrumentation_when_transcript_is_readable() {
        let (engine, storage) = instrumentation_test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let transcript = tmp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","message":{"content":"make the podcast episode"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ffmpeg"}}]}}"#,
                "\n",
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","is_error":true,"content":"ffmpeg exit 1"}]}}"#,
                "\n",
                r#"{"type":"user","message":{"content":"no — hindi, not english"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Retried with the hindi voice id."}]}}"#,
                "\n",
            ),
        )
        .unwrap();

        let session_id = "aaaaaaaa-1111-2222-3333-444444444444";
        let input = HookInput {
            transcript_path: Some(transcript.to_string_lossy().to_string()),
            session_id: Some(session_id.to_string()),
            cwd: Some(tmp.path().to_string_lossy().to_string()),
            ..Default::default()
        };

        extract_and_store_episode(&input, &engine, tmp.path())
            .await
            .expect("readable transcript must store an episode");

        let ep = stored_episode(&storage, session_id);
        assert_eq!(ep.error_count, Some(1));
        assert_eq!(ep.top_errors.len(), 1);
        assert_eq!(ep.top_errors[0].tool, "Bash");
        assert_eq!(ep.steer_count, Some(1));
        assert_eq!(ep.steers.len(), 1);
        assert!(ep.steers[0].text.contains("hindi"));
    }

    #[tokio::test]
    async fn stop_capture_persists_a_correction_with_an_exact_byte_receipt() {
        let (engine, storage) = instrumentation_test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let transcript = tmp.path().join("correction.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","timestamp":"2026-09-01T10:00:00Z","message":{"content":"Implement the parser"}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-09-01T10:01:00Z","message":{"content":"I changed `old_parser`."}}"#,
                "\n",
                r#"{"type":"user","timestamp":"2026-09-01T10:02:00Z","message":{"content":"no, that is not what I asked for"}}"#,
                "\n",
            ),
        )
        .unwrap();
        let session_id = "cccccccc-1111-2222-3333-444444444444";
        let input = HookInput {
            transcript_path: Some(transcript.to_string_lossy().to_string()),
            session_id: Some(session_id.to_string()),
            cwd: Some(tmp.path().to_string_lossy().to_string()),
            ..Default::default()
        };

        extract_and_store_episode(&input, &engine, tmp.path())
            .await
            .unwrap();

        let episode = stored_episode(&storage, session_id);
        assert_eq!(episode.correction_count, Some(1));
        let mut events = storage.list_intent_events(None, None).unwrap();
        assert_eq!(events.len(), 1);
        let bytes = std::fs::read(&transcript).unwrap();
        assert_eq!(
            &bytes[events[0].byte_start..events[0].byte_end],
            events[0].quote.as_bytes()
        );

        let first_high_water: crate::transcript::intent_events::IntentHighWater =
            serde_json::from_str(
                &storage
                    .get_meta(&format!("intent_high_water:{session_id}"))
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(first_high_water.last_turn, 3);
        assert_eq!(
            first_high_water.byte_offset,
            std::fs::metadata(&transcript).unwrap().len() as usize
        );
        extract_and_store_episode(&input, &engine, tmp.path())
            .await
            .unwrap();
        assert_eq!(storage.list_intent_events(None, None).unwrap().len(), 1);

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","timestamp":"2026-09-01T10:03:00Z","message":{{"content":"I used the root config."}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"user","timestamp":"2026-09-01T10:04:00Z","message":{{"content":"wrong file. use `config/app.toml`."}}}}"#
        )
        .unwrap();
        extract_and_store_episode(&input, &engine, tmp.path())
            .await
            .unwrap();
        events = storage.list_intent_events(None, None).unwrap();
        assert_eq!(events.len(), 2);
        let second_high_water: crate::transcript::intent_events::IntentHighWater =
            serde_json::from_str(
                &storage
                    .get_meta(&format!("intent_high_water:{session_id}"))
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(second_high_water.last_turn, 5);
        assert!(second_high_water.byte_offset > first_high_water.byte_offset);
    }

    #[test]
    fn active_stop_hook_is_skipped() {
        let input = HookInput {
            stop_hook_active: Some(true),
            ..Default::default()
        };
        assert!(stop_hook_is_active(&input));
    }

    #[tokio::test]
    async fn oversized_transcript_is_skipped_without_error() {
        let (engine, storage) = instrumentation_test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let transcript = tmp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            r#"{"type":"user","message":{"content":"make the podcast episode"}}"#,
        )
        .unwrap();
        // Stub an over-cap size without writing 64 MiB of real bytes: a
        // sparse `set_len` bump makes `metadata().len()` read as huge while
        // the file's real content — and disk usage — stay tiny.
        {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&transcript)
                .unwrap();
            f.set_len(crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES + 1)
                .unwrap();
        }
        assert!(
            std::fs::metadata(&transcript).unwrap().len()
                > crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES,
            "fixture must actually exceed the cap for this test to mean anything"
        );

        let session_id = "bbbbbbbb-1111-2222-3333-444444444444";
        let input = HookInput {
            transcript_path: Some(transcript.to_string_lossy().to_string()),
            session_id: Some(session_id.to_string()),
            cwd: Some(tmp.path().to_string_lossy().to_string()),
            ..Default::default()
        };

        extract_and_store_episode(&input, &engine, tmp.path())
            .await
            .expect("an oversized transcript must not fail the hook");

        let ep = stored_episode(&storage, session_id);
        assert!(
            ep.error_count.is_none(),
            "oversized scan must leave error_count None"
        );
        assert!(ep.top_errors.is_empty());
        assert!(
            ep.steer_count.is_none(),
            "oversized scan must leave steer_count None"
        );
        assert_eq!(
            storage
                .get_meta(&format!("intent_high_water:{session_id}"))
                .unwrap(),
            None,
            "oversized Stop transcript must not enter intent extraction"
        );
        assert!(ep.steers.is_empty());
    }

    #[test]
    fn missing_transcript_never_fails_the_hook() {
        let tmp = tempfile::TempDir::new().unwrap();
        let transcript = tmp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            r#"{"type":"user","message":{"content":"do the work"}}"#,
        )
        .unwrap();

        // File present at the point the caller would normally scan it.
        assert!(scan_instrumentation(&transcript, 1 << 20).is_some());

        // The transcript vanishes between the hook's initial existence
        // check/read and a later instrumentation scan attempt (real-world
        // race: session rotation, watcher move, Ctrl+C mid-write).
        // `scan_instrumentation` must degrade to `None` — never panic, never
        // propagate an I/O error — which is exactly what lets its caller
        // (`extract_and_store_episode`) leave the instrumentation fields at
        // `None` while still storing the rest of the episode. Hooks must
        // never block Claude Code (repo CLAUDE.md).
        std::fs::remove_file(&transcript).unwrap();
        assert!(scan_instrumentation(&transcript, 1 << 20).is_none());
    }

    /// Assistant tool_use line with an explicit block id and free-form input.
    fn assistant_named_tool(name: &str, tool_use_id: &str, input: serde_json::Value) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": name,
                    "input": input
                }]
            }
        })
        .to_string()
    }

    /// User message carrying a tool_result block bound to a tool_use_id.
    fn tool_result_bound(tool_use_id: &str, content: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn episode_v2_extracts_taskcreate_taskupdate() {
        let lines_owned = [
            user_line("do the work"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_1",
                serde_json::json!({"subject": "first task"}),
            ),
            tool_result_bound("toolu_1", "Task #1 created successfully."),
            assistant_named_tool(
                "TaskCreate",
                "toolu_2",
                serde_json::json!({"subject": "second task"}),
            ),
            tool_result_bound("toolu_2", "Task #2 created successfully."),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_3",
                serde_json::json!({"taskId": "2", "status": "completed"}),
            ),
            // Task state changes only on a matching successful result.
            tool_result_bound("toolu_3", "Updated task #2 status"),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-tc", "proj");

        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[0].content, "first task");
        assert_eq!(ep.todos[0].status, "pending");
        assert_eq!(ep.todos[1].content, "second task");
        assert_eq!(ep.todos[1].status, "completed");
        // next_steps is the first non-completed item (id 1 / first task)
        assert_eq!(ep.next_steps.as_deref(), Some("first task"));
    }

    #[test]
    fn taskupdate_unknown_id_ignored() {
        let lines_owned = [
            user_line("update a phantom task"),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_x",
                serde_json::json!({"taskId": "9", "status": "completed"}),
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-unk", "proj");
        assert!(ep.todos.is_empty());
        assert_eq!(ep.next_steps, None);
    }

    #[test]
    fn taskcreate_result_id_binding_nonsequential() {
        // Non-sequential ids prove we bind from tool_result text, not ordinal index.
        let lines_owned = [
            user_line("resume prior tasks"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_a",
                serde_json::json!({"subject": "item three"}),
            ),
            tool_result_bound("toolu_a", "Task #3 created successfully."),
            assistant_named_tool(
                "TaskCreate",
                "toolu_b",
                serde_json::json!({"subject": "item seven"}),
            ),
            tool_result_bound("toolu_b", "Task #7 created successfully."),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_c",
                serde_json::json!({"taskId": "7", "status": "completed"}),
            ),
            tool_result_bound("toolu_c", "Updated task #7 status"),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-ns", "proj");

        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[0].content, "item three");
        assert_eq!(ep.todos[0].status, "pending");
        assert_eq!(ep.todos[1].content, "item seven");
        assert_eq!(ep.todos[1].status, "completed");
    }

    #[test]
    fn todowrite_after_taskcreate_replaces() {
        let lines_owned = [
            user_line("mix task systems"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_1",
                serde_json::json!({"subject": "from taskcreate"}),
            ),
            tool_result_bound("toolu_1", "Task #1 created successfully."),
            assistant_named_tool(
                "TodoWrite",
                "toolu_2",
                serde_json::json!({
                    "todos": [
                        {"content": "from todowrite a", "status": "pending"},
                        {"content": "from todowrite b", "status": "in_progress"}
                    ]
                }),
            ),
            tool_result_bound("toolu_2", "Todos have been modified successfully"),
            // Stale id from the cleared TaskCreate map must not corrupt the list.
            assistant_named_tool(
                "TaskUpdate",
                "toolu_3",
                serde_json::json!({"taskId": "1", "status": "completed"}),
            ),
            tool_result_bound("toolu_3", "Updated task #1 status"),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-prec", "proj");

        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[0].content, "from todowrite a");
        assert_eq!(ep.todos[0].status, "pending");
        assert_eq!(ep.todos[1].content, "from todowrite b");
        assert_eq!(ep.todos[1].status, "in_progress");
    }

    #[test]
    fn load_task_dir_state_reads_numeric_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("1.json"),
            r#"{"subject":"first","status":"pending"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("2.json"),
            r#"{"subject":"second","status":"completed"}"#,
        )
        .unwrap();
        // Non-json / non-numeric names must be skipped without error.
        std::fs::write(dir.path().join(".lock"), "busy").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();

        let state = load_task_state_from_dir(dir.path()).expect("dir readable");
        assert_eq!(state.files_seen, 2);
        assert_eq!(state.parse_failures, 0);
        assert_eq!(
            state.todos,
            vec![
                TodoItem {
                    content: "first".to_string(),
                    status: "pending".to_string(),
                },
                TodoItem {
                    content: "second".to_string(),
                    status: "completed".to_string(),
                },
            ]
        );
    }

    #[test]
    fn task_dir_all_deleted_is_authoritative_empty_and_failures_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("1.json"),
            r#"{"subject":"gone","status":"deleted"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("2.json"), "not json at all").unwrap();
        let state = load_task_state_from_dir(dir.path()).expect("dir readable");
        // Deleted task counts as a seen file (authority signal) but not a todo;
        // the garbage file counts as a parse failure (schema-drift signal).
        assert_eq!(state.files_seen, 2);
        assert_eq!(state.parse_failures, 1);
        assert!(state.todos.is_empty());
    }

    #[test]
    fn outcome_pending_task_caps_at_partial() {
        let todos = [TodoItem {
            content: "x".into(),
            status: "pending".into(),
        }];
        let signals = OutcomeSignals {
            final_failed: false,
            proven: true,
            ended: true,
        };
        let outcome = compute_outcome(5, &signals, &todos);
        assert_eq!(outcome, "partial");
    }

    // ---- recap garble regressions (2026-10-12, sessions 6ca2a4d5 / c95fa1a1) ----
    //
    // Sanitized from the live transcript: a 3-day, multi-topic session whose
    // recap kept the FIRST prompt (cut at 77 chars, ellipsis then trimmed, so it
    // ended "…can:") and the last assistant prose as "completed", including a
    // future-tense plan ("I'll recreate it headless…").

    const FIRST_PROMPT: &str = "anthropics codemod, when github links are bought up or \
        disccussed, can\n  present a side pane (enhanced for herdr users) - thoughts?";

    fn typed_line(text: &str, source: &str) -> String {
        serde_json::json!({
            "type": "user",
            "promptSource": source,
            "message": {"content": text}
        })
        .to_string()
    }

    fn flagged_user_line(text: &str, flag: &str) -> String {
        let mut line = serde_json::json!({"type": "user", "message": {"content": text}});
        line[flag] = serde_json::Value::Bool(true);
        line.to_string()
    }

    fn task_notification_line() -> String {
        serde_json::json!({
            "type": "user",
            "promptSource": "system",
            "origin": {"kind": "task-notification"},
            "message": {"content": "<task-notification><task-id>a1</task-id> the build agent finished its work and reported back</task-notification>"}
        })
        .to_string()
    }

    fn git_commit(id: &str) -> String {
        assistant_named_tool(
            "Bash",
            id,
            serde_json::json!({"command": "git -c user.name=x commit -m 'fix'"}),
        )
    }

    fn multi_topic_session() -> Vec<String> {
        let mut lines = vec![
            typed_line(FIRST_PROMPT, "typed"),
            assistant_text_line("Here is how a side pane could work."),
            git_commit("toolu_old"),
            tool_result_bound(
                "toolu_old",
                "[main 0aa11bb] feat(pane): first topic commit\n 1 file changed",
            ),
            typed_line("How do we show cost as % of max plan", "queued"),
            task_notification_line(),
            flagged_user_line(
                "This session is being continued from a previous conversation. Summary: the first prompt was about a codemod side pane.",
                "isCompactSummary",
            ),
            flagged_user_line("Base directory for this skill: /x/skill with a long body", "isMeta"),
            user_line("[Request interrupted by user for tool use]"),
            typed_line("go", "typed"),
        ];
        // The latest task did real work too (6 calls vs the first topic's
        // 1 call + 1 commit = 11), so it anchors under the half-weight rule.
        lines.extend(work("cost", 6));
        lines.extend([assistant_text_line(
            "How the share works: your 7D meter × CSR's cost ÷ everything this machine \
                 spent with Claude in the same plan week.",
        )]);
        lines
    }

    fn extract(lines: &[String]) -> Episode {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        extract_episode(&refs, "6ca2a4d5", "claude-self-reflect")
    }

    #[test]
    fn r8_interruption_closes_main_turn_without_becoming_anchor() {
        for notice in [
            "[Request interrupted by user]",
            "[Request interrupted by user for tool use]",
            "[Request interrupted]",
        ] {
            assert!(ends_turn(
                &serde_json::json!({"message":{"content":notice}}),
                "user"
            ));
            assert!(!ends_turn(
                &serde_json::json!({"message":{"content":[{"type":"tool_result","content":notice}]}}),
                "user"
            ));
            for sidechain in [false, true] {
                let rows = [
                    serde_json::json!({"type":"user", "timestamp":"2026-10-13T00:00:00Z", "message":{"content":"Design the discussion side pane for users."}}),
                    serde_json::json!({"type":"assistant", "message":{"content":[], "stop_reason":"tool_use"}}),
                    serde_json::json!({"type":"user", "isSidechain":sidechain, "message":{"content":[{"type":"text","text":notice}]}}),
                    serde_json::json!({"type":"user", "timestamp":"2026-10-13T01:00:00Z", "message":{"content":"Fix the database durability regression before release."}}),
                    serde_json::json!({"type":"assistant", "message":{"content":"Done.","stop_reason":"end_turn"}}),
                ];
                let ep = extract(&rows.iter().map(ToString::to_string).collect::<Vec<_>>());
                if sidechain {
                    assert!(ep.compact_anchor.is_none());
                } else {
                    assert_eq!(
                        ep.compact_anchor.as_ref().map(|a| a.request.as_str()),
                        Some("Fix the database durability regression before release.")
                    );
                }
                assert!(!ep
                    .anchor_request
                    .unwrap_or_default()
                    .contains("interrupted"));
            }
        }
    }

    #[test]
    fn compact_anchor_allows_clause_cut_and_tracks_receipt_origins() {
        let first = "Design the discussion side pane for users.";
        let latest = "Drive release readiness and verify all candidate checks";
        let long = format!(
            "{latest}, {}",
            "with extensive detailed verification ".repeat(20)
        );
        assert!(crate::hooks::recap::clean_intent(&long).is_none());
        for old_receipt in [false, true] {
            let mut lines = segment(first, "first", 1);
            if old_receipt {
                lines.extend(committed("old", "abcdef1", "feat(pane): add side pane"));
                lines.push(turn_end_line());
            }
            lines.extend(segment(&long, "latest", 1));
            lines.extend(committed(
                "new",
                "abcdef2",
                "fix(release): verify candidate",
            ));
            let ep = extract(&lines);
            let anchor = ep.compact_anchor.as_ref().unwrap();
            assert_eq!(anchor.request, latest);
            assert_eq!(anchor.completions_untied, old_receipt);
            assert_eq!(ep.anchor_request.as_deref(), Some(first));
            assert!(ep.completions_untied);
        }
    }

    #[test]
    fn anchor_is_the_first_task_whole_never_cut() {
        let ep = extract(&multi_topic_session());
        assert!(ep.request.starts_with("anthropics codemod"));
        assert_eq!(ep.recap_evidence_version, Some(RECAP_EVIDENCE_VERSION));
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some(
                "anthropics codemod, when github links are bought up or disccussed, can \
                 present a side pane (enhanced for herdr users) - thoughts?"
            )
        );
    }

    #[test]
    fn receipts_produced_under_the_anchor_stay_tied_to_it() {
        // The only commit came under the first topic, which is the anchor:
        // it is the anchor's own result even though a later task followed.
        let ep = extract(&multi_topic_session());
        assert_eq!(
            ep.completions,
            vec![CompletionReceipt {
                claim: "feat(pane): first topic commit".into(),
                receipt: "0aa11bb".into(),
            }]
        );
        assert!(!ep.completions_untied);
    }

    #[test]
    fn single_prompt_intent_keeps_whole_sentence_across_soft_wrap() {
        let ep = extract(&[typed_line(FIRST_PROMPT, "typed")]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some(
                "anthropics codemod, when github links are bought up or disccussed, can \
                 present a side pane (enhanced for herdr users) - thoughts?"
            )
        );
    }

    #[test]
    fn dreamer_what_if_is_a_task_rendered_as_a_note() {
        let ep = extract(&[
            typed_line(
                "What if Claude opening a memory note also pulled in its session?\n\n\
                 CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                 > Notes are where Claude looks first.",
                "typed",
            ),
            // Sent mid-turn, yet nothing joins onto a note.
            typed_line("and check the pane width while you are at it", "queued"),
            typed_line("ok, do it please", "typed"),
        ]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("note: What if Claude opening a memory note also pulled in its session?")
        );
    }

    #[test]
    fn a_v2_dreamer_what_if_is_a_note_without_its_quote_envelope() {
        let ep = extract(&[typed_line(
            "\u{201c}What if Claude opening a memory note also pulled in its session?\u{201d}\n\n\
             CSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n\
             > Notes are where Claude looks first.",
            "typed",
        )]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("note: What if Claude opening a memory note also pulled in its session?")
        );
    }

    #[test]
    fn note_opener_shares_the_import_prefix() {
        assert!(SIDE_AGENT_NOTE_OPENER
            .starts_with(crate::extraction::provenance::SIDE_AGENT_NOTE_PREFIX));
    }

    #[test]
    fn forwarded_note_renders_its_bold_title() {
        let ep = extract(&[typed_line(
            "Here is a note offered by a side agent:\n\n\
             > Heads up · Logic's bar lines in this project are a 120 BPM placeholder, not the song's real beat. A region you drag can snap off the music.\n\
             >\n\
             > **Logic's bar lines don't match the song's beat**\n\
             > - The top bar shows **120 KEEP**. That means the project runs at a fixed 120 BPM.",
            "typed",
        )]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("note: Logic's bar lines don't match the song's beat")
        );
    }

    #[test]
    fn forwarded_note_without_a_title_uses_its_first_heads_up_sentence() {
        let ep = extract(&[typed_line(
            "Here is a note offered by a side agent:\n\n\
             > Heads up · The recap came out garbled twice today. Both runs cut the prompt.",
            "typed",
        )]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("note: The recap came out garbled twice today.")
        );
    }

    #[test]
    fn rendered_notes_pass_the_guard_but_the_raw_dreamer_prompt_never_does() {
        use crate::extraction::provenance::{extractable, is_csr_emission};
        let raw = "What if Claude opening a memory note also pulled in its session?\n\n\
                   CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                   > Notes are where Claude looks first.";
        assert!(is_csr_emission(raw));
        assert!(extractable(raw).is_none());
        let rendered = "note: What if Claude opening a memory note also pulled in its session?";
        assert!(!is_csr_emission(rendered));
        assert_eq!(extractable(rendered).as_deref(), Some(rendered));
    }

    #[test]
    fn future_tense_plan_is_never_a_completion() {
        let ep = extract(&[
            typed_line(
                "We need a screen recording of the warp terminal even if recreated as part of this",
                "queued",
            ),
            assistant_text_line(
                "I'll recreate it headless: a real claude session runs in a private tmux, and \
                 its real frames replay inside a browser.",
            ),
        ]);
        assert!(ep.completions.is_empty());
        let got = crate::hooks::recap::compose_recap(
            &ep,
            &crate::hooks::recap::RecapFeeds {
                settled: vec![],
                still_open: vec![],
                retired_while_away: vec![],
                corrections: vec![],
                open_proposals: 0,
                top_dream: None,
            },
            "2m ago",
        )
        .unwrap();
        assert_eq!(
            got,
            "recap [2m ago]: We need a screen recording of the warp terminal even if recreated \
             as part of this. [[CSR:RECAP]]"
        );
    }

    #[test]
    fn commit_after_latest_task_is_a_receipted_completion() {
        let ep = extract(&[
            typed_line("fix the garbled recap", "typed"),
            git_commit("toolu_c1"),
            tool_result_bound(
                "toolu_c1",
                "[fix/recap-garble 1a2b3c4d] fix(recap): intent from the latest task\n 2 files changed, 9 insertions(+)",
            ),
            // A failed commit and a non-git command printing the same shape
            // are not completions.
            git_commit("toolu_c2"),
            serde_json::json!({"type": "user", "message": {"content": [{
                "type": "tool_result", "tool_use_id": "toolu_c2", "is_error": true,
                "content": "[main 9f9f9f9] would-be subject"
            }]}})
            .to_string(),
            assistant_named_tool("Bash", "toolu_cat", serde_json::json!({"command": "cat log.txt"})),
            tool_result_bound("toolu_cat", "[main 7e7e7e7] quoted from a log"),
        ]);
        assert_eq!(
            ep.completions,
            vec![CompletionReceipt {
                claim: "fix(recap): intent from the latest task".into(),
                receipt: "1a2b3c4".into(),
            }]
        );
    }

    #[test]
    fn task_completions_after_latest_task_carry_their_receipt() {
        let ep = extract(&[
            typed_line("ship the recap fix", "typed"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_t1",
                serde_json::json!({"subject": "Write regression tests"}),
            ),
            tool_result_bound("toolu_t1", "Task #3 created successfully"),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_t2",
                serde_json::json!({"taskId": "3", "status": "completed"}),
            ),
            tool_result_bound("toolu_t2", "Updated task #3 status"),
            assistant_named_tool(
                "TodoWrite",
                "toolu_t3",
                serde_json::json!({"todos": [
                    {"content": "Run the gates", "status": "completed"},
                    {"content": "Commit", "status": "pending"}
                ]}),
            ),
            tool_result_bound("toolu_t3", "Todos have been modified successfully"),
        ]);
        assert_eq!(
            ep.completions,
            vec![
                CompletionReceipt {
                    claim: "Write regression tests".into(),
                    receipt: "task #3 completed".into(),
                },
                CompletionReceipt {
                    claim: "Run the gates".into(),
                    receipt: "todo completed".into(),
                },
            ]
        );
    }

    #[test]
    fn recap_for_the_live_garble_shape_names_the_latest_task_and_no_prose() {
        let ep = extract(&multi_topic_session());
        let got = crate::hooks::recap::compose_recap(
            &ep,
            &crate::hooks::recap::RecapFeeds {
                settled: vec![],
                still_open: vec![],
                retired_while_away: vec![],
                corrections: vec![],
                open_proposals: 0,
                top_dream: None,
            },
            "3h ago",
        )
        .unwrap();
        // The latest substantive ask is the intent; the first topic's commit is
        // a session fact on its own line, never this ask's result.
        let head = got.lines().next().unwrap_or_default();
        assert!(
            head.starts_with("recap [3h ago]: How do we show cost as % of max plan"),
            "{got}"
        );
        assert!(!head.contains("0aa11bb"), "{got}");
        assert!(
            got.contains("Settled: feat(pane): first topic commit (0aa11bb)"),
            "{got}"
        );
        assert!(!got.contains("How the share works"));
    }

    // ---- mid-turn continuation (case 1 follow-up) ----

    fn turn_end_line() -> String {
        serde_json::json!({"type": "system", "subtype": "turn_duration", "durationMs": 1000})
            .to_string()
    }

    fn assistant_stop_line(text: &str, stop_reason: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}], "stop_reason": stop_reason}
        })
        .to_string()
    }

    #[test]
    fn mid_turn_continuation_joins_the_prompt_it_continues() {
        // Live shape (lines 28444/28462): the second message arrived while the
        // first turn was cancelled ("remote-cancel"), never finished.
        let ep = extract(&[
            typed_line("How do we show cost as % of mac", "queued"),
            assistant_stop_line("remote-cancel", "stop_sequence"),
            typed_line("% of max plan if they have it or cents", "queued"),
        ]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("How do we show cost as % of mac % of max plan if they have it or cents")
        );
    }

    #[test]
    fn a_finished_turn_separates_tasks() {
        for finished in [
            turn_end_line(),
            assistant_stop_line("Here is the answer.", "end_turn"),
        ] {
            let mut lines = vec![
                typed_line("How do we show cost as a share of the plan", "typed"),
                finished,
                typed_line("now fix the recap garble", "typed"),
            ];
            lines.extend(committed("b", "1a2b3c4", "fix(recap): garble"));
            let ep = extract(&lines);
            assert_eq!(
                ep.anchor_request.as_deref(),
                Some("How do we show cost as a share of the plan")
            );
            // The receipt belongs to a later, separate task.
            assert!(ep.completions_untied);
        }
    }

    #[test]
    fn joined_continuation_that_fails_clean_intent_abstains() {
        let long = format!("{} end", "word ".repeat(30));
        let ep = extract(&[
            turn_end_line(),
            typed_line(&long, "typed"),
            assistant_stop_line("working", "tool_use"),
            typed_line("and also the side pane width", "queued"),
        ]);
        assert_eq!(ep.anchor_request, None);
    }

    // ---- astra review round (findings 1-6) ----

    fn assistant_null_stop_line(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}], "stop_reason": null}
        })
        .to_string()
    }

    #[test]
    fn explicit_null_stop_reason_is_not_a_finished_turn() {
        // Only a MISSING stop_reason is a legacy finish; streamed rows carry
        // an explicit null while the turn is still running.
        let ep = extract(&[
            turn_end_line(),
            typed_line("How do we show cost as % of mac", "typed"),
            assistant_null_stop_line("thinking about it"),
            typed_line("% of max plan if they have it or cents", "queued"),
        ]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("How do we show cost as % of mac % of max plan if they have it or cents")
        );
    }

    #[test]
    fn an_ack_reopens_the_turn_so_a_clarification_joins_the_task() {
        let ep = extract(&[
            typed_line("Fix login validation", "typed"),
            assistant_stop_line("Plan: tighten the email check. Go?", "end_turn"),
            typed_line("go", "typed"),
            assistant_stop_line("editing", "tool_use"),
            typed_line("only for the signup form", "queued"),
        ]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("Fix login validation only for the signup form")
        );
    }

    #[test]
    fn system_reminder_contents_never_become_the_intent() {
        let ep = extract(&[typed_line(
            "<system-reminder>\nBuild failed: missing foo.rs.\n</system-reminder>\n\nFix login validation.",
            "typed",
        )]);
        assert_eq!(ep.anchor_request.as_deref(), Some("Fix login validation."));
    }

    fn error_result(tool_use_id: &str, content: &str) -> String {
        serde_json::json!({"type": "user", "message": {"content": [{
            "type": "tool_result", "tool_use_id": tool_use_id, "is_error": true,
            "content": content
        }]}})
        .to_string()
    }

    #[test]
    fn task_completion_counts_only_on_a_successful_tool_result() {
        let task = |result: String| {
            extract(&[
                typed_line("ship the deploy", "typed"),
                assistant_named_tool(
                    "TaskCreate",
                    "toolu_c",
                    serde_json::json!({"subject": "Deploy"}),
                ),
                tool_result_bound("toolu_c", "Task #3 created successfully"),
                assistant_named_tool(
                    "TaskUpdate",
                    "toolu_u",
                    serde_json::json!({"taskId": "3", "status": "completed"}),
                ),
                result,
            ])
        };
        assert!(task(error_result("toolu_u", "Task not found"))
            .completions
            .is_empty());
        assert_eq!(
            task(tool_result_bound("toolu_u", "Updated task #3 status")).completions,
            vec![CompletionReceipt {
                claim: "Deploy".into(),
                receipt: "task #3 completed".into(),
            }]
        );
    }

    #[test]
    fn todo_completion_counts_only_on_a_successful_tool_result() {
        let todo = |result: String| {
            extract(&[
                typed_line("ship the deploy", "typed"),
                assistant_named_tool(
                    "TodoWrite",
                    "toolu_w",
                    serde_json::json!({"todos": [{"content": "Deploy", "status": "completed"}]}),
                ),
                result,
            ])
        };
        assert!(todo(error_result("toolu_w", "InputValidationError"))
            .completions
            .is_empty());
        assert_eq!(
            todo(tool_result_bound(
                "toolu_w",
                "Todos have been modified successfully"
            ))
            .completions,
            vec![CompletionReceipt {
                claim: "Deploy".into(),
                receipt: "todo completed".into(),
            }]
        );
    }

    fn commit_receipts_for(command: &str) -> Vec<CompletionReceipt> {
        extract(&[
            typed_line("fix the garbled recap", "typed"),
            assistant_named_tool("Bash", "toolu_g", serde_json::json!({"command": command})),
            tool_result_bound("toolu_g", "[main 1a2b3c4] fix(recap): subject"),
        ])
        .completions
    }

    #[test]
    fn only_a_committing_git_subcommand_yields_a_commit_receipt() {
        assert!(commit_receipts_for("git log -1 --format='[main %h] %s' --grep=commit").is_empty());
        let one = vec![CompletionReceipt {
            claim: "fix(recap): subject".into(),
            receipt: "1a2b3c4".into(),
        }];
        assert_eq!(
            commit_receipts_for(
                r#"git -c user.name="Rama Annaswamy" -c user.email=x@y commit -m "fix""#
            ),
            one
        );
        assert_eq!(commit_receipts_for("cd x && git commit -m y"), one);
        assert_eq!(
            commit_receipts_for("git --no-pager -C /repo cherry-pick abc1234"),
            one
        );
        assert!(commit_receipts_for("echo git commit").is_empty());
    }

    fn bash_receipts(command: &str, output: &str, failed: bool) -> Vec<CompletionReceipt> {
        extract(&[
            typed_line("land the recap fix", "typed"),
            assistant_named_tool("Bash", "toolu_g", serde_json::json!({"command": command})),
            if failed {
                error_result("toolu_g", output)
            } else {
                tool_result_bound("toolu_g", output)
            },
        ])
        .completions
    }

    fn receipt(claim: &str, receipt: &str) -> CompletionReceipt {
        CompletionReceipt {
            claim: claim.into(),
            receipt: receipt.into(),
        }
    }

    #[test]
    fn a_quiet_commit_is_a_receipt_when_the_log_line_names_its_own_subject() {
        let command =
            r#"git add -A && git commit -q -m "fix(recap): subject" && git log --oneline -1"#;
        assert_eq!(
            bash_receipts(command, "1a2b3c4 fix(recap): subject", false),
            vec![receipt("fix(recap): subject", "1a2b3c4")]
        );
        // A heredoc commit followed by a separate line proves nothing: the
        // log runs whether or not the commit did.
        let heredoc =
            "git commit -q -F - <<'EOF'\nfix(recap): subject\n\nbody\nEOF\ngit log --oneline -1";
        assert!(bash_receipts(heredoc, "1a2b3c4 fix(recap): subject", false).is_empty());
        // Hook chatter before the line changes nothing.
        assert_eq!(
            bash_receipts(
                command,
                "no leaks found\n1a2b3c4 fix(recap): subject",
                false
            ),
            vec![receipt("fix(recap): subject", "1a2b3c4")]
        );
    }

    #[test]
    fn a_quiet_commit_gives_no_receipt_for_a_commit_it_did_not_make() {
        // The commit failed behind a `;`: the log shows the older HEAD.
        let command = r#"git commit -q -m "feat: new thing"; git log --oneline -1"#;
        assert!(bash_receipts(command, "9d8c7b6 fix: older commit", false).is_empty());
        // A longer log lists older commits too; only the one in the command counts.
        let command = r#"git commit -q -m "feat: new thing" && git log --oneline -3"#;
        assert_eq!(
            bash_receipts(
                command,
                "aaa1111 feat: new thing\nbbb2222 fix: older commit\nccc3333 chore: older still",
                false
            ),
            vec![receipt("feat: new thing", "aaa1111")]
        );
        // A call that failed is no receipt even when the line is there.
        let command = r#"git commit -q -m "feat: new thing" && git log --oneline -1"#;
        assert!(bash_receipts(command, "aaa1111 feat: new thing", true).is_empty());
        // A log line without a committing command in the call is history.
        assert!(bash_receipts("git log --oneline -1", "aaa1111 feat: new thing", false).is_empty());
    }

    #[test]
    fn a_quiet_commit_that_failed_is_no_receipt_when_an_older_head_has_the_same_subject() {
        // Nothing was staged, so the commit failed; the log that follows shows
        // the older HEAD, whose subject is the same. Only a log that runs
        // because the commit succeeded (`&&`) is proof of a new HEAD.
        for command in [
            "git commit -q -m 'fix: x'; git log --oneline -1",
            "git commit -q -m 'fix: x'\ngit log --oneline -1",
            "git commit -q -m 'fix: x' || true; git log --oneline -1",
            "git log --oneline -1 && git commit -q -m 'fix: x'",
            "git commit -q -m 'fix: x' | cat && git log --oneline -1",
        ] {
            assert!(
                bash_receipts(command, "9d8c7b6 fix: x", false).is_empty(),
                "{command}"
            );
        }
        assert_eq!(
            bash_receipts(
                "git commit -q -m 'fix: x' && git log --oneline -1 | cat",
                "9d8c7b6 fix: x",
                false
            ),
            vec![receipt("fix: x", "9d8c7b6")]
        );
    }

    #[test]
    fn a_log_of_another_revision_is_no_receipt_for_the_commit() {
        let out = "9d8c7b6 fix: x";
        for command in [
            "git commit -q -m 'fix: x' && git log --oneline -1 HEAD~1",
            "git commit -q -m 'fix: x' && git log --oneline -1 HEAD^",
            "git commit -q -m 'fix: x' && git log --oneline -1 main",
            "git commit -q -m 'fix: x' && git show --oneline -s abc1234",
            "git commit -q -m 'fix: x' && git log --oneline -1 -- src/a.rs",
            "git commit -q -m 'fix: x' && git log --oneline -1 origin/main..HEAD",
        ] {
            assert!(bash_receipts(command, out, false).is_empty(), "{command}");
        }
        for command in [
            "git commit -q -m 'fix: x' && git log --oneline -1",
            "git commit -q -m 'fix: x' && git log --oneline -1 HEAD",
            "git commit -q -m 'fix: x' && git log --oneline -n 1",
            "git commit -q -m 'fix: x' && git show -s --oneline",
            "git commit -q -m 'fix: x' && git log --format='%h %s' -1",
        ] {
            assert_eq!(
                bash_receipts(command, out, false),
                vec![receipt("fix: x", "9d8c7b6")],
                "{command}"
            );
        }
    }

    #[test]
    fn the_same_commit_seen_twice_is_one_receipt() {
        let ep = extract(&[
            typed_line("land the recap fix", "typed"),
            assistant_named_tool(
                "Bash",
                "c1",
                serde_json::json!({"command": "git commit -m x"}),
            ),
            tool_result_bound("c1", "[main 1a2b3c4] fix(recap): subject"),
            assistant_named_tool(
                "Bash",
                "c2",
                serde_json::json!({"command": "git commit --amend --no-edit"}),
            ),
            tool_result_bound("c2", "[main 1a2b3c4] fix(recap): subject"),
        ]);
        assert_eq!(
            ep.completions,
            vec![receipt("fix(recap): subject", "1a2b3c4")]
        );
    }

    #[test]
    fn a_merged_pr_is_a_receipt_only_with_the_merge_commit_in_the_output() {
        let command = "gh pr merge 76 --squash 2>&1 | tail -3; gh pr view 76 --json state,mergeCommit -q '.state+\" \"+.mergeCommit.oid'";
        assert_eq!(
            bash_receipts(
                command,
                "MERGED a8f845882e5068d3eed6d2ca160b182a49c23b06",
                false
            ),
            vec![receipt("merged PR #76", "a8f8458")]
        );
        assert_eq!(
            bash_receipts(command, "MERGED 2026-10-21T00:22:31Z 5b1a3b1", false),
            vec![receipt("merged PR #76", "5b1a3b1")]
        );
        // Still open, or the call failed: nothing was merged.
        assert!(bash_receipts(command, "OPEN ", false).is_empty());
        assert!(bash_receipts(command, "MERGED a8f8458", true).is_empty());
        // Two merges in one call cannot be told apart.
        let two =
            "gh pr merge 1 --squash; gh pr merge 2 --squash; gh pr view 2 --json state,mergeCommit";
        assert!(bash_receipts(two, "MERGED a8f8458", false).is_empty());
        // A view of an already merged PR is not a merge by this session.
        assert!(bash_receipts(
            "gh pr view 76 --json state,mergeCommit",
            "MERGED a8f8458",
            false
        )
        .is_empty());
    }

    #[test]
    fn edits_alone_are_no_receipt() {
        let edit = |id: &str, path: &str| {
            assistant_named_tool("Edit", id, serde_json::json!({"file_path": path}))
        };
        // A session that only edited files finished nothing a receipt proves.
        let ep = extract(&[
            typed_line("tighten the recap composer", "typed"),
            edit("e1", "/repo/src/hooks/stop.rs"),
            tool_result_bound("e1", "The file has been updated"),
        ]);
        assert!(ep.completions.is_empty());
        assert!(crate::hooks::recap::displayed_receipts(&ep.completions).is_empty());
    }

    /// One Bash call and its result.
    fn bash_step(id: &str, command: &str, output: &str, failed: bool) -> Vec<String> {
        vec![
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "tool_use", "id": id, "name": "Bash",
                             "input": {"command": command}}],
                "stop_reason": "tool_use"
            }})
            .to_string(),
            if failed {
                error_result(id, output)
            } else {
                tool_result_bound(id, output)
            },
        ]
    }

    /// A session that asks for `ask`, runs `steps`, and closes with `closing`.
    fn closing_run(steps: Vec<Vec<String>>, closing: &str) -> Episode {
        let mut rows = vec![typed_line("run the release checks and report", "typed")];
        rows.extend(steps.into_iter().flatten());
        rows.push(assistant_stop_line(closing, "end_turn"));
        extract(&rows)
    }

    #[test]
    fn a_session_that_committed_composes_a_recap_from_the_commit() {
        let ep = closing_run(
            vec![bash_step(
                "c",
                "git commit -q -m 'fix(recap): name the commit'",
                "[main 1a2b3c4] fix(recap): name the commit\n 1 file changed",
                false,
            )],
            "Committed.",
        );
        let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
        assert_eq!(
            got,
            "recap [now]: run the release checks and report. Latest: fix(recap): name the commit (1a2b3c4). [[CSR:RECAP]]"
        );
        // Everything it says is re-import suppressed.
        assert!(crate::extraction::provenance::is_csr_emission(&got));
        assert!(crate::extraction::provenance::extractable(&got).is_none());
    }

    #[test]
    fn a_commit_is_shown_beside_the_ask_it_was_made_under_and_not_the_other() {
        let ep = extract(&[
            typed_line("run the release checks and report", "typed"),
            assistant_stop_line("The checks pass.", "end_turn"),
            typed_line("now land the fix for the stale lock in the daemon", "typed"),
            assistant_named_tool(
                "Bash",
                "c",
                serde_json::json!({"command": "git commit -m 'fix(daemon): stale lock'"}),
            ),
            tool_result_bound("c", "[main 1a2b3c4] fix(daemon): stale lock"),
            assistant_stop_line("Committed.", "end_turn"),
        ]);
        assert_eq!(
            ep.completions,
            vec![receipt("fix(daemon): stale lock", "1a2b3c4")]
        );
        // The first ask owns no receipt; the latest ask owns the commit.
        assert_eq!(ep.anchor_receipts, Some(vec![]));
        assert_eq!(
            ep.compact_anchor.as_ref().and_then(|a| a.receipts.clone()),
            Some(vec![0])
        );
        let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
        let head = got.lines().next().unwrap_or_default();
        assert!(
            head.starts_with("recap [now]: now land the fix for the stale lock in the daemon"),
            "{got}"
        );
        assert!(head.contains("fix(daemon): stale lock (1a2b3c4)"), "{got}");
        assert!(!got.contains("release checks"), "{got}");
        // The commit stays on the episode for the companion's own reading.
        assert_eq!(
            crate::hooks::recap::displayed_receipts(&ep.completions).len(),
            1
        );
    }

    #[test]
    fn a_commit_made_under_the_earlier_ask_is_settled_not_the_latest_asks_result() {
        let ep = extract(&[
            typed_line("land the fix for the stale lock in the daemon", "typed"),
            assistant_named_tool(
                "Bash",
                "c",
                serde_json::json!({"command": "git commit -m 'fix(daemon): stale lock'"}),
            ),
            tool_result_bound("c", "[main 1a2b3c4] fix(daemon): stale lock"),
            assistant_stop_line("Committed.", "end_turn"),
            typed_line("now explain how the watcher seals a conversation", "typed"),
            assistant_stop_line("It seals on the next idle tick.", "end_turn"),
        ]);
        let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
        let head = got.lines().next().unwrap_or_default();
        assert!(head.contains("explain how the watcher seals"), "{got}");
        assert!(!head.contains("1a2b3c4"), "{got}");
        assert!(
            got.contains("Settled: fix(daemon): stale lock (1a2b3c4)"),
            "{got}"
        );
    }

    #[test]
    fn a_task_created_under_an_earlier_ask_is_not_the_later_asks_result() {
        let task = |name: &str, id: &str, input: serde_json::Value, result: &str| {
            vec![
                assistant_named_tool(name, id, input),
                tool_result_bound(id, result),
            ]
        };
        let mut rows = vec![typed_line(
            "plan the daemon rewrite for the stale lock",
            "typed",
        )];
        rows.extend(task(
            "TaskCreate",
            "t1",
            serde_json::json!({"subject": "Review the lock fix", "description": "d"}),
            "Task #1 created successfully: Review the lock fix",
        ));
        rows.push(assistant_stop_line("Planned.", "end_turn"));
        rows.push(typed_line(
            "now run the full release checks and report",
            "typed",
        ));
        rows.extend(task(
            "TaskUpdate",
            "t2",
            serde_json::json!({"taskId": "1", "status": "completed"}),
            "Updated task #1 status",
        ));
        rows.push(assistant_stop_line("Done.", "end_turn"));
        let ep = extract(&rows);
        assert_eq!(
            ep.completions,
            vec![receipt("Review the lock fix", "task #1 completed")]
        );
        // The task was created for the first ask, not the one that closed it.
        assert_eq!(ep.anchor_receipts, Some(vec![0]));
    }

    #[test]
    fn two_tasks_with_one_subject_each_belong_to_their_own_ask() {
        let create = |id: &str, n: u32| {
            vec![
                assistant_named_tool(
                    "TaskCreate",
                    id,
                    serde_json::json!({"subject": "Run tests", "description": "d"}),
                ),
                tool_result_bound(id, &format!("Task #{n} created successfully: Run tests")),
            ]
        };
        let complete = |id: &str, n: u32| {
            vec![
                assistant_named_tool(
                    "TaskUpdate",
                    id,
                    serde_json::json!({"taskId": n.to_string(), "status": "completed"}),
                ),
                tool_result_bound(id, "Updated task"),
            ]
        };
        let mut rows = vec![typed_line(
            "plan the daemon rewrite for the stale lock",
            "typed",
        )];
        rows.extend(create("a1", 1));
        rows.push(assistant_stop_line("Planned.", "end_turn"));
        rows.push(typed_line(
            "now run the full release checks and report",
            "typed",
        ));
        rows.extend(create("b1", 2));
        rows.extend(complete("b2", 2));
        rows.push(assistant_stop_line("Done.", "end_turn"));
        let ep = extract(&rows);
        assert_eq!(
            ep.completions,
            vec![receipt("Run tests", "task #2 completed")]
        );
        // Task 2 was created and finished under the second ask; the first
        // ask's recap must not claim it.
        assert_eq!(ep.anchor_receipts, Some(vec![]));
        assert_eq!(
            ep.compact_anchor.as_ref().and_then(|a| a.receipts.clone()),
            Some(vec![0])
        );
        // The same holds for a rewritten todo list.
        let write = |id: &str, status: &str| {
            vec![
                assistant_named_tool(
                    "TodoWrite",
                    id,
                    serde_json::json!({"todos": [{"content": "Run tests", "status": status}]}),
                ),
                tool_result_bound(id, "ok"),
            ]
        };
        let mut rows = vec![typed_line(
            "plan the daemon rewrite for the stale lock",
            "typed",
        )];
        rows.extend(write("w1", "pending"));
        rows.push(assistant_stop_line("Planned.", "end_turn"));
        rows.push(typed_line(
            "now run the full release checks and report",
            "typed",
        ));
        rows.extend(write("w2", "pending"));
        rows.extend(write("w3", "completed"));
        rows.push(assistant_stop_line("Done.", "end_turn"));
        let ep = extract(&rows);
        assert_eq!(ep.completions, vec![receipt("Run tests", "todo completed")]);
        assert_eq!(ep.anchor_receipts, Some(vec![]));
    }

    #[test]
    fn a_session_with_no_receipt_composes_no_completed_clause() {
        // A question answered from reads: nothing was committed, edited or
        // finished, so the recap says what was asked and nothing more.
        let ep = closing_run(
            vec![bash_step(
                "r",
                "git log --oneline -3",
                "1a2b3c4 older",
                false,
            )],
            "The last three commits are listed.",
        );
        assert!(ep.completions.is_empty());
        let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
        assert_eq!(
            got,
            "recap [now]: run the release checks and report. [[CSR:RECAP]]"
        );
        // And with no typed request at all there is nothing to compose.
        let mut empty = ep.clone();
        empty.request = String::new();
        empty.anchor_request = None;
        empty.latest_ask = None;
        empty.completed = String::new();
        assert!(crate::hooks::recap::compose_recap(&empty, &no_feeds(), "now").is_none());
    }

    #[test]
    fn a_failed_call_that_was_run_again_and_passed_is_not_a_failure() {
        let ep = closing_run(
            vec![
                bash_step(
                    "a",
                    "cd csr-engine && cargo test --lib 2>&1 | tail -5",
                    "Exit code 101",
                    true,
                ),
                bash_step(
                    "b",
                    "cd csr-engine && cargo test --lib",
                    "test result: ok",
                    false,
                ),
            ],
            "The tests pass after the fix.",
        );
        assert!(!ep.late_errors && !ep.final_failed);
        assert_eq!(ep.outcome, "success");
        assert_eq!(ep.outcome_version, Some(OUTCOME_VERSION));
        // The same failure with no second run is a failure the session ended on.
        let ep = closing_run(
            vec![bash_step("a", "cargo test --lib", "Exit code 101", true)],
            "The tests are red.",
        );
        assert!(ep.late_errors && ep.final_failed);
        assert_eq!(ep.outcome, "failed");
        // A different operation of the same tool does not clear it.
        let ep = closing_run(
            vec![
                bash_step("a", "cargo test --lib", "Exit code 101", true),
                bash_step("b", "cargo build --release", "Finished", false),
            ],
            "Built.",
        );
        // A failed test run is not cleared by a build that passed: the tests
        // still fail.
        assert!(ep.late_errors && ep.final_failed);
        assert_eq!(ep.outcome, "failed");
    }

    #[test]
    fn a_failed_edit_is_cleared_by_a_later_edit_of_the_same_file() {
        let edit = |id: &str, path: &str, failed: bool| {
            vec![
                assistant_named_tool("Edit", id, serde_json::json!({"file_path": path})),
                if failed {
                    error_result(id, "String to replace not found")
                } else {
                    tool_result_bound(id, "The file has been updated")
                },
            ]
        };
        let ep = closing_run(
            vec![edit("a", "/r/a.rs", true), edit("b", "/r/a.rs", false)],
            "Done.",
        );
        assert_eq!(ep.outcome, "success");
        let ep = closing_run(
            vec![edit("a", "/r/a.rs", true), edit("b", "/r/b.rs", false)],
            "Done.",
        );
        // The failed edit stays on the record; the session went on and finished.
        assert!(ep.late_errors && !ep.final_failed);
        assert_eq!(ep.outcome, "success");
    }

    #[test]
    fn a_probe_that_finds_nothing_is_not_a_failure() {
        let ep = closing_run(
            vec![
                bash_step("a", "grep -rn flaky_test src/", "", true),
                bash_step("b", "ls /tmp/missing 2>&1", "No such file", true),
                bash_step(
                    "c",
                    "curl -sf https://example.test/health",
                    "Exit code 22",
                    true,
                ),
                bash_step("d", "git log --oneline -3 | grep nothing", "", true),
            ],
            "Nothing matches.",
        );
        assert!(!ep.late_errors && !ep.final_failed, "{ep:?}");
        assert_eq!(ep.outcome, "success");
    }

    #[test]
    fn an_unresolved_test_failure_survives_a_later_unrelated_success() {
        let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
        let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
        let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
        // Commit WIP, fail the tests, format successfully, stop.
        let ep = closing_run(
            vec![commit(), red("t", "cargo test"), green("f", "cargo fmt")],
            "Committed; the tests still fail.",
        );
        assert!(ep.final_failed && ep.closing_receipt);
        assert_eq!(ep.outcome, "partial");
        // Without a commit the same ending is a failure.
        let ep = closing_run(
            vec![red("t", "cargo test"), green("f", "cargo fmt")],
            "The tests still fail.",
        );
        assert_eq!(ep.outcome, "failed");
        // The same tests run again and passing resolve it.
        let ep = closing_run(
            vec![
                commit(),
                red("t", "cargo test"),
                green("f", "cargo fmt"),
                green("g", "cargo test"),
            ],
            "Fixed and green.",
        );
        assert!(!ep.final_failed);
        assert_eq!(ep.outcome, "success");
        // A failed build or lint stays open the same way.
        for command in [
            "cargo clippy -- -D warnings",
            "cargo build --release",
            "npm test",
        ] {
            let ep = closing_run(
                vec![commit(), red("t", command), green("f", "cargo fmt")],
                "Done.",
            );
            assert_eq!(ep.outcome, "partial", "{command}");
        }
    }

    #[test]
    fn a_test_pass_clears_only_the_failure_of_the_same_target_and_directory() {
        let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
        let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
        let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
        // A different test target is a different test.
        let ep = closing_run(
            vec![
                commit(),
                red("a", "cargo test --test broken"),
                green("b", "cargo test --test healthy"),
            ],
            "Done.",
        );
        assert!(ep.final_failed);
        assert_eq!(ep.outcome, "partial");
        // So is a different filter, or the same command in another directory.
        for (failing, passing) in [
            ("cargo test parser", "cargo test lexer"),
            ("cd /repo/a && cargo test", "cd /repo/b && cargo test"),
            ("cargo test -p engine", "cargo test -p companion"),
        ] {
            let ep = closing_run(
                vec![commit(), red("a", failing), green("b", passing)],
                "Done.",
            );
            assert_eq!(ep.outcome, "partial", "{failing} then {passing}");
        }
        // The same target run again and passing clears it, whatever the noise
        // flags, redirections and pipes around it.
        for (failing, passing) in [
            ("cargo test --test broken", "cargo test --test broken"),
            (
                "cargo test --release --test broken 2>&1 | tail -5",
                "cargo test --release --test broken -q -j4 --color always > /tmp/out.log",
            ),
            ("cd /repo/a && cargo test", "cd /repo/a && cargo test"),
        ] {
            let ep = closing_run(
                vec![commit(), red("a", failing), green("b", passing)],
                "Fixed.",
            );
            assert!(!ep.final_failed, "{failing} then {passing}");
            assert_eq!(ep.outcome, "success", "{failing} then {passing}");
        }
    }

    #[test]
    fn a_pass_under_another_build_profile_does_not_clear_a_failure() {
        let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
        let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
        let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
        for (failing, passing) in [
            ("cargo test", "cargo test --release"),
            ("cargo test --release", "cargo test"),
            ("cargo test", "cargo test -r"),
            ("cargo test", "cargo test --profile ci"),
            ("cargo test", "cargo test --features slow"),
            ("cargo test", "cargo test --all-features"),
            ("cargo test", "cargo test --no-default-features"),
            ("cargo test", "cargo test --target aarch64-apple-darwin"),
        ] {
            let ep = closing_run(
                vec![commit(), red("a", failing), green("b", passing)],
                "Done.",
            );
            assert!(ep.final_failed, "{failing} then {passing}");
            assert_eq!(ep.outcome, "partial", "{failing} then {passing}");
        }
        // Output-shaping flags and redirections are still noise.
        let ep = closing_run(
            vec![
                commit(),
                red("a", "cargo test --release"),
                green("b", "cargo test -r -q -j 4 --color=always > /tmp/o 2>&1"),
            ],
            "Fixed.",
        );
        assert_eq!(ep.outcome, "success");
    }

    #[test]
    fn the_directory_is_the_one_at_the_verification_command_not_the_end_of_the_chain() {
        let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
        let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
        let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
        // A pass in /repo/b, with a trailing cd back to /repo/a, is not a pass in a.
        let ep = closing_run(
            vec![
                commit(),
                red("a", "cd /repo/a && cargo test"),
                green("b", "cd /repo/b && cargo test && cd /repo/a"),
            ],
            "Done.",
        );
        assert_eq!(ep.outcome, "partial");
        // A cd after the command does not move a failure either.
        let key = |command: &str, cwd: Option<&str>| {
            call_signature("Bash", &serde_json::json!({"command": command}), cwd).failure_key
        };
        assert_eq!(
            key("cd /repo/a && cargo test && cd /repo/b", None),
            key("cd /repo/a && cargo test", None)
        );
        assert_ne!(
            key("cd /repo/b && cargo test && cd /repo/a", None),
            key("cd /repo/a && cargo test", None)
        );
    }

    #[test]
    fn directories_are_compared_lexically_after_dot_and_dotdot() {
        let key = |command: &str, cwd: Option<&str>| {
            call_signature("Bash", &serde_json::json!({"command": command}), cwd).failure_key
        };
        let here = key("cargo test", Some("/repo"));
        assert_eq!(key("cd .. && cargo test", Some("/repo/tests")), here);
        assert_eq!(key("cd ./tests/.. && cargo test", Some("/repo")), here);
        assert_eq!(key("cd /repo/tests/../ && cargo test", None), here);
        assert_eq!(key("cargo test", Some("/repo/")), here);
        assert_ne!(key("cd tests && cargo test", Some("/repo")), here);
        assert_ne!(key("cd ../other && cargo test", Some("/repo")), here);
    }

    #[test]
    fn a_commit_a_merge_or_an_install_lifts_a_failed_ending_to_partial() {
        let failing = || bash_step("a", "cargo test --lib", "Exit code 101", true);
        let ep = closing_run(vec![failing()], "Red.");
        assert_eq!(ep.outcome, "failed");
        // The session then committed what it had.
        let ep = closing_run(
            vec![
                failing(),
                bash_step("c", "git commit -m wip", "[main 1a2b3c4] wip", false),
            ],
            "Committed the work in progress.",
        );
        assert!(ep.closing_receipt && ep.final_failed);
        assert_eq!(ep.outcome, "partial");
        // Or installed the build, and the failure it ended on is a later install.
        let ep = closing_run(
            vec![
                bash_step("i", "csr-engine hook install --apply", "installed", false),
                bash_step("f", "cargo test --lib", "Exit code 101", true),
            ],
            "Red after the install.",
        );
        assert!(ep.final_failed && ep.closing_receipt);
        assert_eq!(ep.outcome, "partial");
        // A merged PR proves progress the same way.
        let merge = "gh pr merge 76 --squash 2>&1 | tail -3; gh pr view 76 --json state,mergeCommit -q '.state+\" \"+.mergeCommit.oid'";
        let ep = closing_run(
            vec![
                failing(),
                bash_step(
                    "m",
                    merge,
                    "MERGED a8f845882e5068d3eed6d2ca160b182a49c23b06",
                    false,
                ),
            ],
            "Merged.",
        );
        assert!(ep.closing_receipt && ep.final_failed);
        assert_eq!(ep.outcome, "partial");
    }

    #[test]
    fn a_session_cut_off_or_interrupted_is_partial() {
        // The last row is a tool result: the turn never finished.
        let mut rows = vec![typed_line("run the release checks and report", "typed")];
        rows.extend(bash_step("a", "cargo test --lib", "test result: ok", false));
        let ep = extract(&rows);
        assert!(!ep.closing_ended);
        assert_eq!(ep.outcome, "partial");
        // The person interrupted it.
        rows.push(
            serde_json::json!({"type": "user", "message": {"content": [
                {"type": "text", "text": "[Request interrupted by user]"}
            ]}})
            .to_string(),
        );
        let ep = extract(&rows);
        assert!(!ep.closing_ended);
        assert_eq!(ep.outcome, "partial");
    }

    #[test]
    fn a_failure_under_an_earlier_request_does_not_follow_the_session() {
        let mut rows = vec![typed_line("run the release checks and report", "typed")];
        rows.extend(bash_step("a", "cargo test --lib", "Exit code 101", true));
        rows.push(assistant_stop_line("Red.", "end_turn"));
        rows.push(typed_line(
            "what is still running on this machine?",
            "typed",
        ));
        rows.extend(bash_step("b", "ps aux | head", "PID ...", false));
        rows.push(assistant_stop_line("Nothing of ours.", "end_turn"));
        let ep = extract(&rows);
        assert!(!ep.late_errors && !ep.final_failed);
        assert_eq!(ep.outcome, "success");
    }

    #[test]
    fn call_signatures_name_the_command_that_matters() {
        let key = |command: &str| {
            call_signature("Bash", &serde_json::json!({"command": command}), None).failure_key
        };
        assert_eq!(
            key("cd csr-engine && cargo test --lib 2>&1 | tail -5").as_deref(),
            Some("verify:cargo test --lib @csr-engine")
        );
        assert_eq!(
            key("export A=1; time cargo build --release").as_deref(),
            Some("verify:cargo build --release @")
        );
        assert_eq!(key("git push origin main").as_deref(), Some("git push"));
        assert_eq!(
            key("python3 - <<'EOF'\nprint(1)\nEOF").as_deref(),
            Some("python3")
        );
        assert_eq!(key("./gate.sh --quick").as_deref(), Some("gate.sh"));
        assert_eq!(key("grep -rn x src | head"), None);
        assert_eq!(key("git log --oneline -1 && git status"), None);
        assert_eq!(key("cd /tmp && ls"), None);
        let installs = |command: &str| {
            call_signature("Bash", &serde_json::json!({"command": command}), None).installs
        };
        for yes in [
            "./install-on-phone.sh",
            "cargo install --path .",
            "csr-engine hook install --apply",
            "bash scripts/install.sh",
            "npm install -g @openai/codex",
            "xcrun devicectl device install app --device X App.app",
        ] {
            assert!(installs(yes), "{yes}");
        }
        for no in [
            "git commit -m \"install fix\"",
            "echo install",
            "grep -r install .",
            "cat install.sh",
            "cargo test install_paths",
        ] {
            assert!(!installs(no), "{no}");
        }
        assert_eq!(
            call_signature("Read", &serde_json::json!({"file_path": "/r/a.rs"}), None).failure_key,
            None
        );
    }

    fn sidechain(line: String) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
        v["isSidechain"] = serde_json::Value::Bool(true);
        v.to_string()
    }

    #[test]
    fn sidechain_rows_never_touch_recap_state() {
        let ep = extract(&[
            typed_line("Fix login validation", "typed"),
            assistant_stop_line("editing", "tool_use"),
            // A legacy in-file subagent: its turn end, prompt, commit and task
            // completion are not the main session's.
            sidechain(assistant_stop_line("subagent done", "end_turn")),
            sidechain(typed_line("Refactor the whole billing module", "typed")),
            sidechain(git_commit("toolu_side")),
            sidechain(tool_result_bound(
                "toolu_side",
                "[main 5e5e5e5] refactor(billing): subagent commit",
            )),
            sidechain(assistant_named_tool(
                "TodoWrite",
                "toolu_side_todo",
                serde_json::json!({"todos": [{"content": "Billing", "status": "completed"}]}),
            )),
            sidechain(tool_result_bound(
                "toolu_side_todo",
                "Todos have been modified successfully",
            )),
            typed_line("only for the signup form", "queued"),
        ]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("Fix login validation only for the signup form")
        );
        assert!(ep.completions.is_empty(), "{:?}", ep.completions);
        // tools_used keeps its existing behaviour: sidechain tools still count.
        assert!(ep.tools_used.iter().any(|t| t == "Bash"));
    }

    // ---- round 4: anchor = the task segment with the most work ----

    /// `n` successful non-git Bash calls, as the main session makes them.
    fn work(prefix: &str, n: usize) -> Vec<String> {
        (0..n)
            .flat_map(|i| {
                let id = format!("toolu_{prefix}_{i}");
                // A live tool call row: the turn stays open (stop_reason tool_use).
                let mut call: serde_json::Value = serde_json::from_str(&assistant_named_tool(
                    "Bash",
                    &id,
                    serde_json::json!({"command": "ls"}),
                ))
                .unwrap();
                call["message"]["stop_reason"] = serde_json::Value::String("tool_use".into());
                [call.to_string(), tool_result_bound(&id, "ok")]
            })
            .collect()
    }

    fn segment(prompt: &str, prefix: &str, calls: usize) -> Vec<String> {
        let mut lines = vec![typed_line(prompt, "typed")];
        lines.extend(work(prefix, calls));
        lines.push(assistant_stop_line("Done.", "end_turn"));
        lines
    }

    fn committed(prefix: &str, sha: &str, subject: &str) -> Vec<String> {
        let id = format!("toolu_{prefix}_commit");
        vec![
            git_commit(&id),
            tool_result_bound(&id, &format!("[main {sha}] {subject}")),
        ]
    }

    #[test]
    fn anchor_is_the_heaviest_segment_when_the_latest_is_light() {
        let mut lines = segment("build me a vox explainer video for the above", "a", 10);
        lines.extend(segment(
            "Will the audio cause linkedin to not play it",
            "b",
            1,
        ));
        let ep = extract(&lines);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("build me a vox explainer video for the above")
        );
    }

    #[test]
    fn an_unclean_anchor_falls_back_to_the_next_heaviest() {
        let long = format!("{} end", "word ".repeat(40));
        let mut lines = segment(&long, "a", 20);
        lines.extend(segment(
            "build me a vox explainer video for the above",
            "b",
            8,
        ));
        lines.extend(segment(
            "Will the audio cause linkedin to not play it",
            "c",
            1,
        ));
        assert_eq!(
            extract(&lines).anchor_request.as_deref(),
            Some("build me a vox explainer video for the above")
        );
        // No second segment with work: abstain.
        let mut lines = segment(&long, "a", 20);
        lines.push(typed_line(
            "Will the audio cause linkedin to not play it",
            "typed",
        ));
        assert_eq!(extract(&lines).anchor_request, None);
    }

    #[test]
    fn a_lost_and_found_pane_paste_is_not_a_task() {
        // Session 3002ab28: the only typed prompt was a pane send text.
        let ep = extract(&[
            typed_line(
                "Lost and found: “The IG post time is a soft follow-up. Then ask for the release go-ahead (allowed, since publishing needs approval).” went quiet after 19:38. Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line.",
                "typed",
            ),
            assistant_stop_line("Closed: it was already live.", "end_turn"),
        ]);
        assert_eq!(ep.anchor_request, None);
    }

    fn typed_at(text: &str, source: &str, timestamp: &str) -> String {
        let mut line: serde_json::Value = serde_json::from_str(&typed_line(text, source)).unwrap();
        line["timestamp"] = serde_json::Value::String(timestamp.into());
        line.to_string()
    }

    #[test]
    fn only_a_quick_mid_turn_follow_up_joins_the_task_text() {
        // this-ai-life 76e2ddd2: an addition queued 24 minutes into the work
        // stays in the segment (its work counts) but not in the anchor text.
        let mut lines = vec![typed_at(
            "in the pigment skill, can we add a vox style - review this gh repo for style etc and add it https://github.com/example/vox-director",
            "typed",
            "2026-10-11T18:52:19.078Z",
        )];
        lines.extend(work("a", 5));
        lines.push(typed_at(
            "ok one more now its called anatomy - use the anatomy repo as the second reference for this style",
            "queued",
            "2026-10-11T19:16:28.448Z",
        ));
        lines.extend(work("b", 5));
        lines.push(assistant_stop_line("Done.", "end_turn"));
        lines.extend(segment(
            "tell csr to go ahead with the jukebox build",
            "c",
            1,
        ));
        assert_eq!(
            extract(&lines).anchor_request.as_deref(),
            Some("in the pigment skill, can we add a vox style - review this gh repo for style etc and add it https://github.com/example/vox-director")
        );

        // Case 1 (lines 28444/28462): a split message 24 s apart still joins.
        let ep = extract(&[
            typed_at(
                "How do we show cost as % of mac",
                "queued",
                "2026-10-11T21:26:26.000Z",
            ),
            assistant_stop_line("remote-cancel", "stop_sequence"),
            typed_at(
                "% of max plan if they have it or cents",
                "queued",
                "2026-10-11T21:26:50.000Z",
            ),
        ]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("How do we show cost as % of mac % of max plan if they have it or cents")
        );
    }

    // ---- round 4 pivot: anchor = the first task (blind labels 9/11) ----

    #[test]
    fn pivot_first_task_anchors_whatever_work_follows() {
        let mut lines = segment(
            "whats the next song to attempt, with trap sophistication",
            "a",
            1,
        );
        lines.extend(segment("thats a copyright trap what else", "b", 30));
        assert_eq!(
            extract(&lines).anchor_request.as_deref(),
            Some("whats the next song to attempt, with trap sophistication")
        );
    }

    #[test]
    fn pivot_unclean_first_task_falls_back_to_the_heaviest_segment() {
        let long = format!("{} end", "word ".repeat(40));
        let mut lines = segment(&long, "a", 2);
        lines.extend(segment("clean up the duplicate main layouts", "b", 12));
        lines.push(typed_line("commit the cleanup now and tag it", "typed"));
        lines.extend(work("c", 1));
        lines.extend(committed("c", "1a2b3c4", "fix(app): one main layout"));
        lines.push(assistant_stop_line("Committed.", "end_turn"));
        // 12 calls vs 2 calls + one receipt (2 + COMMIT_WEIGHT = 12, later wins).
        assert_eq!(
            extract(&lines).anchor_request.as_deref(),
            Some("commit the cleanup now and tag it")
        );
        // No segment with work besides the unclean first: abstain.
        let mut lines = segment(&long, "a", 20);
        lines.push(typed_line(
            "Will the audio cause linkedin to not play it",
            "typed",
        ));
        assert_eq!(extract(&lines).anchor_request, None);
    }

    #[test]
    fn pivot_completed_is_the_sessions_latest_receipts_tied_only_to_a_latest_anchor() {
        let mut lines = segment("clean up the duplicate main layouts", "a", 3);
        lines.push(typed_line("and ship the radio reorder too", "typed"));
        lines.extend(committed("b", "5e5e5e5", "fix(app): one main layout"));
        lines.extend(committed(
            "c",
            "6f6f6f6",
            "feat(radio): sundarakanda second",
        ));
        lines.extend(committed("d", "7a7a7a7", "chore: release notes"));
        lines.push(assistant_stop_line("Done.", "end_turn"));
        let ep = extract(&lines);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("clean up the duplicate main layouts")
        );
        let receipts: Vec<&str> = ep.completions.iter().map(|c| c.receipt.as_str()).collect();
        assert_eq!(receipts, ["5e5e5e5", "6f6f6f6", "7a7a7a7"]);
        // The anchor is not the latest segment: the receipts are not its.
        assert!(ep.completions_untied);

        let mut lines = segment("clean up the duplicate main layouts", "a", 3);
        lines.extend(committed("b", "5e5e5e5", "fix(app): one main layout"));
        let ep = extract(&lines);
        assert!(!ep.completions_untied);
    }

    #[test]
    fn an_over_long_first_task_anchors_on_its_clause_not_a_fallback() {
        let mut lines = segment(
            "This ia new Reycital challenge - there are two videos here /Users/r/Downloads/IMG_8550.mov /Users/r/Downloads/IMG_8546.mov - 8550 is the melodica, which is the primary melody and 8546 is the piano - your mission if you chose to take it on is to import both into logic.",
            "a",
            5,
        );
        lines.extend(segment("stopped check now", "b", 40));
        assert_eq!(
            extract(&lines).anchor_request.as_deref(),
            Some("This ia new Reycital challenge")
        );
    }

    #[test]
    fn a_short_fallback_abstains_on_every_path() {
        // The first task has no clean rendering and no clause to cut; the
        // heaviest other segment is a 3-word side request: abstain.
        let long = format!("{} end", "word ".repeat(40));
        let mut lines = segment(&long, "a", 5);
        lines.extend(segment("stopped check now", "b", 40));
        let ep = extract(&lines);
        assert_eq!(ep.anchor_request, None);
        // The recap, CONTINUUM fallback and EPISODE INDEX (which the pickup
        // renders through) all read the same anchor.
        assert_eq!(crate::hooks::recap::recap_intent(&ep), None);
        let continuum = crate::hooks::session_start::format_tier0_block(&ep, &[], "now");
        assert!(
            continuum.starts_with("CSR CONTINUUM [now]: (no clean intent)\n"),
            "{continuum}"
        );
        let index =
            crate::hooks::session_start::format_episode_index(&[(ep.clone(), "now".into())]);
        assert!(index.contains("- [now] (no clean intent) — "), "{index}");
        assert!(!index.contains("stopped check now"), "{index}");

        // Six words or more: the fallback stands.
        let mut lines = segment(&long, "a", 5);
        lines.extend(segment("stopped, check the render log now", "b", 40));
        assert_eq!(
            extract(&lines).anchor_request.as_deref(),
            Some("stopped, check the render log now")
        );
    }

    #[test]
    fn a_csr_agent_transcript_has_no_task() {
        // CSR's own narrator children open with an agent-prompt signature;
        // their later rows are CSR's inputs, never the person's task. Before
        // this, the clause cut turned "TRANSCRIPT EXCERPT: ..." into an anchor.
        let ep = extract(&[
            typed_line(
                "[[CSR:RECAP]] machine-marker: this is a CSR narrator side-session, never corpus material.\nYou are a post-turn rubber-duck narrator for a coding agent.",
                "typed",
            ),
            assistant_stop_line("ok", "end_turn"),
            typed_line(
                "TRANSCRIPT EXCERPT: cal link targets: True missing: [] headings identical: True lines new 145 old 145 lines over 200 chars: 17 longest 481 273 5, plus the rest of the excerpt that runs on",
                "typed",
            ),
        ]);
        assert_eq!(ep.anchor_request, None);
        assert!(ep.completions.is_empty());
    }

    // ---- astra review r5 (1cace86..5257ed3) ----

    fn no_feeds() -> crate::hooks::recap::RecapFeeds {
        crate::hooks::recap::RecapFeeds {
            settled: vec![],
            still_open: vec![],
            retired_while_away: vec![],
            corrections: vec![],
            open_proposals: 0,
            top_dream: None,
        }
    }

    #[test]
    fn astra_r5_1_a_forwarded_note_holding_a_recap_is_never_a_note() {
        let ep = extract(&[typed_line(
            "Here is a note offered by a side agent:\n\n> **recap [1h ago]: Fix login validation. Latest: fix(auth): preserve form errors until the next submitted validation request (abc1234); test(auth): cover retries and stale error clearing when the validation request changes (def5678). [[CSR:RECAP]]**",
            "typed",
        )]);
        assert_eq!(ep.anchor_request, None);
    }

    #[test]
    fn astra_r5_2_an_excluded_late_prompt_does_not_carry_its_follow_up_in() {
        let ep = extract(&[
            typed_at(
                "Fix the checkout validation bug",
                "typed",
                "2026-10-12T12:00:00Z",
            ),
            typed_at(
                "Also build the billing dashboard",
                "queued",
                "2026-10-12T12:24:00Z",
            ),
            typed_at("and show failed invoices", "queued", "2026-10-12T12:24:30Z"),
        ]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("Fix the checkout validation bug")
        );
    }

    #[test]
    fn astra_r5_3_earlier_receipts_never_read_as_a_fallback_anchors_result() {
        let long = "word ".repeat(40);
        let mut lines = vec![typed_line(&long, "typed")];
        lines.extend(committed("a", "abc1234", "fix(auth): validate checkout"));
        lines.push(assistant_stop_line("Done.", "end_turn"));
        lines.push(typed_line(
            "Review the new billing dashboard implementation",
            "typed",
        ));
        lines.extend(work("b", 1));
        lines.push(assistant_stop_line("Reviewed.", "end_turn"));
        let ep = extract(&lines);
        let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
        assert_eq!(
            got.lines().next().unwrap_or_default(),
            "recap [now]: Review the new billing dashboard implementation. [[CSR:RECAP]]"
        );
        // The earlier commit is shown as a session fact, never as this ask's result.
        assert!(
            got.contains("Settled: fix(auth): validate checkout (abc1234)"),
            "{got}"
        );
    }

    #[test]
    fn astra_r5_5_a_wrapped_csr_agent_opening_still_has_no_task() {
        let ep = extract(&[
            typed_line(
                "<system-reminder>CSR ENDLESS MEMORY ACTIVE\npast context</system-reminder>\n\nYou are CSR Episode Analyst. Generate a brief.",
                "typed",
            ),
            assistant_stop_line("ok", "end_turn"),
            typed_line("Summarize the release notes for the narrator.", "typed"),
        ]);
        assert_eq!(ep.anchor_request, None);
    }

    #[test]
    fn astra_r5_7_commit_receipts_outrank_todo_receipts() {
        let todo_write = |id: &str, items: Vec<&str>| {
            let todos: Vec<serde_json::Value> = items
                .iter()
                .map(|c| serde_json::json!({"content": c, "status": "completed"}))
                .collect();
            vec![
                assistant_named_tool("TodoWrite", id, serde_json::json!({ "todos": todos })),
                tool_result_bound(id, "Todos have been modified successfully"),
            ]
        };
        let mut lines = vec![typed_line("ship the auth fixes", "typed")];
        lines.extend(committed("a", "1111111", "fix(auth): one"));
        lines.extend(committed("b", "2222222", "fix(auth): two"));
        lines.extend(todo_write(
            "toolu_t",
            vec!["Write regression tests", "Run gates"],
        ));
        let got = crate::hooks::recap::compose_recap(&extract(&lines), &no_feeds(), "now").unwrap();
        assert_eq!(
            got,
            "recap [now]: ship the auth fixes. Latest: fix(auth): one (1111111); fix(auth): two (2222222). [[CSR:RECAP]]"
        );
        // Five or more todo receipts never evict the stored commits.
        let mut lines = vec![typed_line("ship the auth fixes", "typed")];
        lines.extend(committed("a", "1111111", "fix(auth): one"));
        lines.extend(committed("b", "2222222", "fix(auth): two"));
        lines.extend(todo_write(
            "toolu_t",
            vec![
                "a1 task", "a2 task", "a3 task", "a4 task", "a5 task", "a6 task",
            ],
        ));
        let receipts: Vec<String> = extract(&lines)
            .completions
            .iter()
            .map(|c| c.receipt.clone())
            .collect();
        assert!(receipts.contains(&"1111111".to_string()), "{receipts:?}");
        assert!(receipts.contains(&"2222222".to_string()), "{receipts:?}");
    }

    #[test]
    fn astra_r5_8_failed_task_creation_never_becomes_next() {
        let dreamer = "What if memory notes included their originating session?\n\n\
                       CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                       > Notes should show their origin.";
        let ep = extract(&[
            typed_line(dreamer, "typed"),
            sidechain(assistant_named_tool(
                "TaskCreate",
                "toolu_side_create",
                serde_json::json!({"subject": "Publish package"}),
            )),
            sidechain(error_result("toolu_side_create", "permission denied")),
        ]);
        assert!(ep.todos.is_empty(), "{:?}", ep.todos);
        assert_eq!(ep.next_steps, None);
        let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
        assert!(!got.contains("Next:"), "{got}");
        assert!(!got.contains("todos open"), "{got}");
        // The main session too: a create that failed is no task.
        let ep = extract(&[
            typed_line("publish the package", "typed"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_create",
                serde_json::json!({"subject": "Publish package"}),
            ),
            error_result("toolu_create", "permission denied"),
        ]);
        assert!(ep.todos.is_empty(), "{:?}", ep.todos);
    }

    #[test]
    fn astra_r5_9_a_crlf_dreamer_prompt_keeps_its_headline() {
        let ep = extract(&[typed_line(
            "What if memory notes included their originating session?\r\n\r\nCSR's dreamer asked this while you worked. It is a question, not a task.\r\n\r\n> Notes should show their origin.",
            "typed",
        )]);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("note: What if memory notes included their originating session?")
        );
    }
    #[test]
    fn a_handoff_envelope_in_a_prompt_does_not_reach_the_derived_request_or_tasks() {
        let ask = "Lost and found: “Run source-gone validation” went quiet after 14:30. Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands.";
        let envelope = crate::eval::contamination::agents_document()
            .fixtures
            .remove("handoff")
            .expect("handoff fixture")
            .text;
        let row = |text: &str| {
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-10-13T10:00:00Z",
                "message": {"role": "user", "content": text},
            })
        };
        // the pane's send alone is CSR's, not a task
        assert!(typed_prompt(&row(&format!("{ask}\n{envelope}"))).is_none());
        // with the person's words, only the words are the task
        let typed = typed_prompt(&row(&format!("fix the importer first\n{ask}\n{envelope}")))
            .expect("the person's words are a prompt");
        assert_eq!(typed.prose, "fix the importer first");
        // and the episode built from the transcript carries none of it
        let lines = [
            row("fix the importer first").to_string(),
            row(&format!("{ask}\n{envelope}")).to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let episode = extract_episode(&refs, "sid", "proj");
        let derived = serde_json::to_string(&episode).unwrap();
        for leaked in [
            "\\\"kind\\\":\\\"handoff\\\"",
            "[[CSR:AGENTS",
            "req-2026-10-13-0001",
        ] {
            assert!(
                !derived.contains(leaked),
                "{leaked} reached the episode: {derived}"
            );
        }
    }
    // ---- streaming equivalence (Stop memory) ----

    /// The extraction as it ran before the Stop hook streamed: every line
    /// parsed into one Vec, a full sanitized copy beside it.
    fn episode_whole_file(raw: &str) -> String {
        let messages: Vec<serde_json::Value> = raw
            .lines()
            .filter_map(|line| serde_json::from_str(line.trim()).ok())
            .collect();
        let (sanitized, _) = crate::import::sanitize_messages_for_search(&messages);
        let mut episode = extract_episode_from_messages(&sanitized, &messages, "sid", "proj");
        episode.timestamp.clear();
        serde_json::to_string(&episode).unwrap()
    }

    fn episode_streamed(path: &Path) -> String {
        let mut episode =
            extract_episode_from_file(path, std::fs::metadata(path).unwrap().len(), "sid", "proj")
                .unwrap();
        episode.timestamp.clear();
        serde_json::to_string(&episode).unwrap()
    }

    #[test]
    fn streamed_episode_equals_whole_file_episode() {
        let dreamer = "What if memory notes included their originating session?\n\n\
                       CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                       > Notes should show their origin.";
        let mut lines = multi_topic_session();
        lines.extend([
            typed_line(dreamer, "typed"),
            "this line is not json".to_string(),
            String::new(),
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "csr-1", "name": "csr_reflect_on_past", "input": {"query": "history"}},
                    {"type": "tool_use", "id": "read-1", "name": "Read", "input": {"file_path": "/src/lib.rs"}}
                ]}
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "message": {"content": [
                    {"type": "text", "text": "Error: genuine compiler failure"},
                    {"type": "tool_result", "tool_use_id": "csr-1", "content": "Error: not found in CSR memory"},
                    {"type": "tool_result", "tool_use_id": "read-1", "content": "ordinary sibling result"}
                ]}
            })
            .to_string(),
            assistant_text_line("Investigation complete."),
        ]);
        let dir = tempfile::tempdir().unwrap();
        for (name, joiner) in [("lf.jsonl", "\n"), ("crlf.jsonl", "\r\n")] {
            let raw = lines.join(joiner);
            for tail in ["", joiner] {
                let path = dir.path().join(name);
                std::fs::write(&path, format!("{raw}{tail}")).unwrap();
                let whole = episode_whole_file(&std::fs::read_to_string(&path).unwrap());
                assert_eq!(episode_streamed(&path), whole, "{name} tail={tail:?}");
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                let mut by_lines = extract_episode(&refs, "sid", "proj");
                by_lines.timestamp.clear();
                assert_eq!(serde_json::to_string(&by_lines).unwrap(), whole);
            }
        }
        // The fixture actually exercised the paths: notes, tasks, receipts.
        let whole: serde_json::Value =
            serde_json::from_str(&episode_whole_file(&lines.join("\n"))).unwrap();
        assert!(whole["message_count"].as_u64().unwrap() > 10);
        assert!(!whole["tools_used"].as_array().unwrap().is_empty());
    }

    #[test]
    fn streamed_episode_rejects_invalid_utf8_like_read_to_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");
        let mut bytes = user_line("hello").into_bytes();
        bytes.extend_from_slice(b"\n\xff\xfe\n");
        std::fs::write(&path, bytes).unwrap();
        assert!(std::fs::read_to_string(&path).is_err());
        assert!(extract_episode_from_file(
            &path,
            std::fs::metadata(&path).unwrap().len(),
            "sid",
            "proj"
        )
        .is_err());
    }

    /// Manual check on a real transcript:
    /// `CSR_EQUIV_TRANSCRIPT=/path/x.jsonl cargo test --release --lib -- --ignored streamed_episode_real`
    #[test]
    #[ignore]
    fn streamed_episode_real_transcript_equals_whole_file_episode() {
        let path = std::path::PathBuf::from(std::env::var("CSR_EQUIV_TRANSCRIPT").unwrap());
        let whole = episode_whole_file(&std::fs::read_to_string(&path).unwrap());
        let streamed = episode_streamed(&path);
        assert_eq!(streamed, whole);
        println!("equivalent: {} bytes of episode JSON", streamed.len());
    }

    /// Claude Code appends to the live transcript. Both reads of the Stop hook
    /// stop at one snapshot length, so a row (or garbage) appended between
    /// them shows up in neither the episode nor the provenance inputs.
    #[test]
    fn rows_appended_between_the_two_reads_reach_neither() {
        use std::io::Write;
        let base = [
            user_line("fix the importer"),
            assistant_tool_line(&[("Edit", "/src/a.rs")]),
            assistant_text_line("Fixed and complete."),
        ];
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::storage::Storage::open_memory().unwrap();
        let pristine = dir.path().join("pristine.jsonl");
        std::fs::write(&pristine, format!("{}\n", base.join("\n"))).unwrap();
        let mut expected_episode =
            extract_episode_from_file(&pristine, u64::MAX, "s", "p").unwrap();
        expected_episode.timestamp.clear();
        let expected_episode = serde_json::to_string(&expected_episode).unwrap();

        let appended: [&[u8]; 4] = [
            // a whole new row that would add a tool, a file and a message
            br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"Write","input":{"file_path":"/src/late.rs"}}]}}
"#,
            b"\xff\xfe\n",
            // a failed tool result that instrumentation would count
            br#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"late","is_error":true,"content":"late failure"}]}}
"#,
            // the start of a row still being written
            br#"{"type":"user","message":{"content":"half"#,
        ];
        for tail in appended {
            let live = dir.path().join("live.jsonl");
            std::fs::write(&live, format!("{}\n", base.join("\n"))).unwrap();
            let snapshot_len = std::fs::metadata(&live).unwrap().len();
            // Receipts name the file, so the expected inputs are read from the
            // same path, before anything is appended.
            let expected_inputs = format!(
                "{:?}",
                crate::import::transcript_inputs(&storage, &live, "s").unwrap()
            );
            let scan = |path: &Path, limit: u64| {
                let found = scan_instrumentation(path, limit).unwrap();
                (found.error_count, format!("{:?}", found.top_errors))
            };
            let expected_scan = scan(&live, snapshot_len);
            let inputs =
                crate::import::transcript_inputs_limited(&storage, &live, "s", snapshot_len)
                    .unwrap();
            // Claude Code appends between the hook's two reads.
            std::fs::OpenOptions::new()
                .append(true)
                .open(&live)
                .unwrap()
                .write_all(tail)
                .unwrap();
            let mut episode = extract_episode_from_file(&live, snapshot_len, "s", "p").unwrap();
            episode.timestamp.clear();
            assert_eq!(serde_json::to_string(&episode).unwrap(), expected_episode);
            assert_eq!(format!("{inputs:?}"), expected_inputs);
            assert_eq!(scan(&live, snapshot_len), expected_scan);
            // and a provenance read taken after the append is also capped
            let late = crate::import::transcript_inputs_limited(&storage, &live, "s", snapshot_len)
                .unwrap();
            assert_eq!(format!("{late:?}"), expected_inputs);
        }
    }

    /// Recap evidence read at Stop time (`hooks::recap_evidence`).
    mod recap_extras {
        use super::*;
        use crate::hooks::recap_evidence::{BackgroundTask, ClosingLine, GateFailure};

        fn assistant_row(text: &str, uuid: &str, stop_reason: &str) -> String {
            serde_json::json!({
                "type": "assistant",
                "uuid": uuid,
                "message": {"content": [{"type": "text", "text": text}], "stop_reason": stop_reason}
            })
            .to_string()
        }

        fn with_closing(text: &str) -> Episode {
            extract(&[
                typed_line("run the release checks and report", "typed"),
                assistant_row(text, "3f9a1c2b-0000-4000-8000-000000000001", "end_turn"),
            ])
        }

        #[test]
        fn a_failed_gate_is_kept_with_its_command_and_exit_code() {
            let ep = closing_run(
                vec![bash_step(
                    "a",
                    "cd /repo && cargo test --release --lib 2>&1 | tail -5",
                    "Exit code 101\nerror[E0432]: unresolved import",
                    true,
                )],
                "The tests are red.",
            );
            assert_eq!(
                ep.recap_extras.gate_failure,
                Some(GateFailure {
                    command: "cargo test --release --lib".into(),
                    exit_code: 101,
                })
            );
        }

        #[test]
        fn a_green_rerun_a_later_task_or_a_probe_leaves_no_gate_failure() {
            let red = |id: &str| bash_step(id, "cargo test --lib", "Exit code 101", true);
            let green = |id: &str| bash_step(id, "cargo test --lib", "test result: ok", false);
            assert_eq!(
                closing_run(vec![red("a"), green("b")], "ok")
                    .recap_extras
                    .gate_failure,
                None
            );
            // A failure from an earlier task is not the latest task's.
            let mut rows = vec![typed_line("run the release checks and report", "typed")];
            rows.extend(red("a"));
            rows.push(assistant_stop_line("red", "end_turn"));
            rows.push(typed_line("now rename the module please", "typed"));
            rows.push(assistant_stop_line("renamed", "end_turn"));
            assert_eq!(extract(&rows).recap_extras.gate_failure, None);
            // A grep that finds nothing is a finding, not a failed gate.
            let ep = closing_run(
                vec![bash_step("a", "grep -rn foo src", "Exit code 1", true)],
                "none",
            );
            assert_eq!(ep.recap_extras.gate_failure, None);
            // No exit code printed: no receipt, no clause.
            let ep = closing_run(
                vec![bash_step(
                    "a",
                    "cargo test --lib",
                    "something went wrong",
                    true,
                )],
                "red",
            );
            assert_eq!(ep.recap_extras.gate_failure, None);
        }

        fn background_launch(id: &str, task: &str) -> Vec<String> {
            vec![
                serde_json::json!({"type": "assistant", "message": {
                    "content": [{"type": "tool_use", "id": id, "name": "Bash",
                                 "input": {"command": "sleep 900", "description": "upload build",
                                           "run_in_background": true}}],
                    "stop_reason": "tool_use"
                }})
                .to_string(),
                tool_result_bound(
                    id,
                    &format!("Command running in background with ID: {task}. Output is being written to: /tmp/x.output"),
                ),
            ]
        }

        fn notification(task: &str) -> String {
            format!(
                "<task-notification>\n<task-id>{task}</task-id>\n<status>completed</status>\n<summary>done</summary>\n</task-notification>"
            )
        }

        #[test]
        fn a_background_task_never_reported_finished_is_kept_with_its_id() {
            let ep = closing_run(vec![background_launch("b1", "bq2")], "Started the upload.");
            assert_eq!(
                ep.recap_extras.background_open,
                vec![BackgroundTask {
                    id: "bq2".into(),
                    label: "upload build".into()
                }]
            );
            assert_eq!(ep.recap_extras.background_open_total, 1);
        }

        #[test]
        fn each_notification_shape_closes_a_background_task() {
            let queued = serde_json::json!({"type": "queue-operation", "operation": "enqueue",
                "content": notification("bq2")})
            .to_string();
            let attached = serde_json::json!({"type": "attachment", "attachment": {
                "type": "queued_command", "prompt": notification("bq2")}})
            .to_string();
            let user_row = serde_json::json!({"type": "user", "promptSource": "system",
                "origin": {"kind": "task-notification"},
                "message": {"content": notification("bq2")}})
            .to_string();
            for row in [queued, attached, user_row] {
                let ep = closing_run(
                    vec![background_launch("b1", "bq2"), vec![row.clone()]],
                    "Upload finished.",
                );
                assert!(ep.recap_extras.background_open.is_empty(), "{row}");
            }
            // A notification for another task closes nothing here.
            let other =
                serde_json::json!({"type": "queue-operation", "content": notification("zz9")})
                    .to_string();
            let ep = closing_run(vec![background_launch("b1", "bq2"), vec![other]], "x");
            assert_eq!(ep.recap_extras.background_open_total, 1);
        }

        #[test]
        fn a_closing_next_line_is_kept_with_the_row_it_came_from() {
            let ep = with_closing("Built and installed.\n\n- **Next:** pull the tap log from the phone after you use it.");
            assert_eq!(
                ep.recap_extras.closing_next,
                Some(ClosingLine {
                    text: "pull the tap log from the phone after you use it.".into(),
                    row: "3f9a1c2b".into(),
                })
            );
            let ep = with_closing("Done.\n- **Still open:** the Part VII post link.");
            assert_eq!(
                ep.recap_extras.closing_open,
                Some(ClosingLine {
                    text: "the Part VII post link.".into(),
                    row: "3f9a1c2b".into(),
                })
            );
            assert_eq!(ep.recap_extras.closing_next, None);
        }

        #[test]
        fn a_marker_in_prose_code_or_a_cut_off_turn_is_not_quoted() {
            // Mid-sentence, as CSR-on-CSR sessions write it.
            assert_eq!(
                with_closing("The recap's Next: clause is reserved above the list clauses.")
                    .recap_extras,
                Default::default()
            );
            // Inside a code block.
            assert_eq!(
                with_closing("Here is the grammar:\n\n```\nNext: <step>\nSettled: <claim>\n```\n")
                    .recap_extras,
                Default::default()
            );
            // The turn did not end: the assistant was cut off mid-work.
            let ep = extract(&[
                typed_line("run the release checks and report", "typed"),
                assistant_row(
                    "Next: ship it.",
                    "3f9a1c2b-0000-4000-8000-000000000001",
                    "tool_use",
                ),
            ]);
            assert_eq!(ep.recap_extras.closing_next, None);
            // The row has no uuid: no receipt, no quote.
            let ep = extract(&[
                typed_line("run the release checks and report", "typed"),
                assistant_stop_line("Next: ship it.", "end_turn"),
            ]);
            assert_eq!(ep.recap_extras.closing_next, None);
        }

        #[test]
        fn an_older_marker_is_not_quoted_once_a_later_message_has_none() {
            let ep = extract(&[
                typed_line("run the release checks and report", "typed"),
                assistant_row(
                    "Next: ship it.",
                    "aaaaaaaa-0000-4000-8000-000000000001",
                    "end_turn",
                ),
                typed_line("what did you run last", "typed"),
                assistant_row(
                    "I ran the lib tests.",
                    "bbbbbbbb-0000-4000-8000-000000000002",
                    "end_turn",
                ),
            ]);
            assert_eq!(ep.recap_extras.closing_next, None);
        }

        #[test]
        fn extras_serialize_only_when_present_and_old_episodes_still_load() {
            let ep = with_closing("Nothing special here.");
            let json = serde_json::to_string(&ep).unwrap();
            assert!(!json.contains("recap_extras"), "{json}");
            let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
            value.as_object_mut().unwrap().remove("recap_extras");
            let back: Episode = serde_json::from_value(value).unwrap();
            assert!(back.recap_extras.is_empty());
            let ep = with_closing("Done.\n**Next:** ship the build.");
            let json = serde_json::to_string(&ep).unwrap();
            let back: Episode = serde_json::from_str(&json).unwrap();
            assert_eq!(back.recap_extras, ep.recap_extras);
        }
    }
}
