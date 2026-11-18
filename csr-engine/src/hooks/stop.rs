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
/// Backslash escapes follow POSIX: outside quotes `\x` is a literal `x` (and
/// `\<newline>` joins lines); inside double quotes a backslash escapes only
/// `$`, `` ` ``, `"`, `\` and newline; inside single quotes it is literal.
fn shell_simple_commands(command: &str) -> Vec<Vec<String>> {
    split_shell(command).0
}

/// [`shell_simple_commands`] plus whether the parse is certain: `false` when a
/// quote is left open or the text ends on a lone backslash. Callers that must
/// not guess (a receipt is a claim) refuse an uncertain parse.
fn split_shell(command: &str) -> (Vec<Vec<String>>, bool) {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut certain = true;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('"') if c == '"' => quote = None,
            Some('"') if c == '\\' => match chars.next() {
                Some(next @ ('$' | '`' | '"' | '\\')) => word.push(next),
                Some('\n') => {}
                Some(next) => {
                    word.push('\\');
                    word.push(next);
                }
                None => {
                    word.push('\\');
                    certain = false;
                }
            },
            Some(_) => word.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => match chars.next() {
                    Some('\n') => {}
                    Some(next) => {
                        word.push(next);
                        in_word = true;
                    }
                    None => certain = false,
                },
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
    if quote.is_some() {
        certain = false;
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    (commands, certain)
}

/// The command `ssh [options] host command...` runs on the other machine,
/// joined back into one string (ssh joins its words with spaces and the remote
/// shell parses the result, so this is what runs there). `None` when the words
/// are not an ssh call that names a remote command, and also whenever the
/// options are not plainly understood: a missed receipt costs little, a false
/// one is worse. Only `ssh` itself is read; `bash -c`, `sh -c` and the like
/// stay unread.
fn ssh_remote_command(words: &[String]) -> Option<String> {
    // Short options that take no value, and those that do (`-p 22`, `-p22`).
    // Options group getopt-style: in `-tp 22` the `p` takes the next word, in
    // `-oFoo=bar` it takes the rest of the word.
    const FLAGS: &str = "46AaCfGgKkMNnqsTtVvXxYy";
    const VALUE_OPTIONS: &str = "BbcDEeFIiJLlmOopQRSWw";
    // Leading `VAR=value` assignments are part of the simple command.
    let mut rest = words
        .iter()
        .skip_while(|w| w.contains('=') && !w.starts_with('-'));
    let program = rest.next()?;
    if program != "ssh" && !program.ends_with("/ssh") {
        return None;
    }
    while let Some(word) = rest.next() {
        if word == "--" {
            let _host = rest.next()?;
            break;
        }
        let Some(group) = word.strip_prefix('-') else {
            break; // `word` is the host
        };
        if group.is_empty() {
            return None;
        }
        for (at, letter) in group.char_indices() {
            if FLAGS.contains(letter) {
                continue;
            }
            if !VALUE_OPTIONS.contains(letter) {
                return None;
            }
            if group[at + letter.len_utf8()..].is_empty() {
                rest.next()?; // the value is the next word
            }
            break; // otherwise the rest of this word is the value
        }
    }
    // Whatever follows the host is the remote command. ssh reads options after
    // the host too, which this does not: a word that looks like one is unsure.
    let remote: Vec<&String> = rest.collect();
    if remote.first().is_none_or(|first| first.starts_with('-')) {
        return None;
    }
    let remote = remote
        .iter()
        .map(|word| word.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    (!remote.trim().is_empty()).then_some(remote)
}

/// Whether a Bash command runs a git subcommand that can print a commit
/// success line: the first non-option word after `git` (global options
/// `-c <k=v>`, `-C <path>`, `--no-pager` and other `--long` options skipped)
/// must be commit, cherry-pick, revert or merge. `git log --grep=commit`
/// does not qualify.
fn is_git_committing_command(command: &str) -> bool {
    let (commands, certain) = split_shell(command);
    commands.iter().any(|words| {
        // `ssh host 'cd repo && git commit ...'` commits on the other machine and
        // prints git's success line back; the quoted command is one word to the
        // splitter, so read it as a command of its own. Only from a parse that
        // is certain at both levels: the output of a listing that follows the
        // text would otherwise read as a new commit.
        if let Some(remote) = ssh_remote_command(words) {
            return certain && split_shell(&remote).1 && is_git_committing_command(&remote);
        }
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

/// Whether `piece` pipes one command into another (a `|` outside quotes).
fn has_unquoted_pipe(piece: &str) -> bool {
    let mut quote: Option<char> = None;
    for c in piece.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '|' => return true,
            None => {}
        }
    }
    false
}

/// Whether `words` are a `git commit` that really makes a commit and uses
/// only flags this reader knows: `-q`/`--quiet`, `-m <msg>` (repeatable),
/// `-a`/`--all`, `-s`/`--signoff`, `-F <file>`, `--amend`, `--no-edit`,
/// `-S`/`--gpg-sign`, also clustered (`-qm <msg>`, `-am <msg>`). Any other
/// flag (`--dry-run`, `--porcelain`, `-n`, `--allow-empty`, `-p`, ...) may
/// stop the commit from being made or change what it is: not plain. Global
/// git options other than `-c <k=v>` and `--no-pager` are not either.
fn is_plain_commit(words: &[String]) -> bool {
    let mut rest = words
        .iter()
        .skip_while(|w| w.contains('=') && !w.starts_with('-'));
    if !rest
        .next()
        .is_some_and(|program| program == "git" || program.ends_with("/git"))
    {
        return false;
    }
    loop {
        match rest.next().map(String::as_str) {
            Some("-c") => {
                if rest.next().is_none() {
                    return false;
                }
            }
            Some("--no-pager") => {}
            Some("commit") => break,
            _ => return false,
        }
    }
    while let Some(word) = rest.next() {
        match word.as_str() {
            "-m" | "-F" => {
                if rest.next().is_none() {
                    return false;
                }
            }
            "--quiet" | "--all" | "--signoff" | "--amend" | "--no-edit" | "--gpg-sign" => {}
            w if w.starts_with("--") || w == "-" => return false,
            w if w.starts_with('-') => {
                // A cluster of single-letter flags; `m` and `F` take the next
                // word and must come last.
                let letters: Vec<char> = w[1..].chars().collect();
                for (at, letter) in letters.iter().enumerate() {
                    match letter {
                        'q' | 'a' | 's' | 'S' => {}
                        'm' | 'F' if at == letters.len() - 1 => {
                            if rest.next().is_none() {
                                return false;
                            }
                        }
                        _ => return false,
                    }
                }
            }
            // A pathspec.
            _ => {}
        }
    }
    true
}

/// Whether a quiet commit's `git log` line is proof of a new HEAD. This is
/// deliberately narrow, and abstains on anything else: the commit and the log
/// are pieces of one `&&` chain (so the log runs only because the commit
/// succeeded), the log is exactly `git log --oneline -1` (or `-n1`, `-n 1`,
/// `-1 --oneline`, in any order) with no other option, revision or path, and
/// nothing in the call can move git to another repository: no `cd` (or
/// `pushd`, `popd`), no `-C`, `--git-dir` or `--work-tree`, no `GIT_*` word,
/// no `export`, `env`, `eval` or `source`.
fn quiet_commit_is_proven(command: &str) -> bool {
    let Some(pieces) = and_chain(command) else {
        return false;
    };
    // Every simple command of every piece (a pipe splits a piece in two).
    let parsed: Vec<Vec<Vec<String>>> = pieces
        .iter()
        .map(|piece| shell_simple_commands(piece))
        .collect();
    let moves_repository = parsed.iter().flatten().any(|words| {
        words.iter().any(|w| {
            w == "-C"
                || w.starts_with("--git-dir")
                || w.starts_with("--work-tree")
                || w.contains("GIT_")
        }) || words
            .iter()
            .find(|w| !(w.contains('=') && !w.starts_with('-')))
            .is_some_and(|program| {
                matches!(
                    program.as_str(),
                    "cd" | "pushd" | "popd" | "export" | "env" | "eval" | "source" | "." | "exec"
                )
            })
    });
    if moves_repository {
        return false;
    }
    // `git log --oneline -1`, nothing else.
    let is_head_oneline = |words: &[String]| -> bool {
        let [program, subcommand, options @ ..] = words else {
            return false;
        };
        if (program != "git" && !program.ends_with("/git")) || subcommand != "log" {
            return false;
        }
        let mut options: Vec<&str> = options.iter().map(String::as_str).collect();
        options.sort_unstable();
        matches!(
            options.as_slice(),
            ["--oneline", "-1"] | ["--oneline", "-n1"] | ["--oneline", "-n", "1"]
        )
    };
    let Some(commit) = pieces
        .iter()
        .position(|piece| is_git_committing_command(piece))
    else {
        return false;
    };
    // The committing piece is a lone `git commit` with only whitelisted flags.
    if !matches!(parsed[commit].as_slice(), [words] if is_plain_commit(words)) {
        return false;
    }
    // The log is a piece of its own: a lone command, not one end of a pipe.
    parsed
        .iter()
        .skip(commit + 1)
        .any(|commands| matches!(commands.as_slice(), [words] if is_head_oneline(words)))
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
    /// The signatures of the other significant commands of a compound call
    /// (`cargo test && cargo clippy` files `cargo clippy` above and
    /// `cargo test` here). A failure of a compound names none of them.
    other_keys: Vec<String>,
    /// Whether a success of the call shows each of its commands passed: a lone
    /// command or a chain joined only by `&&`, with no pipe (a pipe's exit
    /// status is its last command's; `pipefail` is ignored). A `;`, a `||` or a
    /// newline proves nothing about the commands before the last.
    proves_pass: bool,
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
            proves_pass: true,
            ..Default::default()
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
            let keys: Vec<String> = commands
                .iter()
                .filter(|(program, _)| !PROBE_PROGRAMS.contains(&program.as_str()))
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
                })
                .collect();
            let failure_key = keys.last().cloned();
            let mut other_keys: Vec<String> = Vec::new();
            for key in keys.iter().take(keys.len().saturating_sub(1)) {
                if Some(key) != failure_key.as_ref() && !other_keys.contains(key) {
                    other_keys.push(key.clone());
                }
            }
            CallSignature {
                failure_key,
                other_keys,
                proves_pass: and_chain(command)
                    .is_some_and(|pieces| !pieces.iter().any(|p| has_unquoted_pipe(p))),
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

/// The text of a message `content` value: a plain string, or the text blocks
/// of a block array joined by newlines.
fn content_text(content: &serde_json::Value) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    match content {
        serde_json::Value::String(text) => Some(Cow::Borrowed(text.as_str())),
        serde_json::Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .collect();
            (!texts.is_empty()).then(|| Cow::Owned(texts.join("\n")))
        }
        _ => None,
    }
}

/// Whether a raw user row is one the harness injected for a finished task:
/// `promptSource: "system"`, or an `origin` of kind `task-notification`.
fn is_harness_notification_row(raw: &serde_json::Value) -> bool {
    raw.get("promptSource").and_then(|v| v.as_str()) == Some("system")
        || raw
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(|k| k.as_str())
            == Some("task-notification")
}

/// The text of a row that may carry a `<task-notification>`: a queued
/// notification, a queued-command attachment, or the harness's own user row
/// (read from the raw twin, since the search sanitizer blanks those), whether
/// its content is a string or an array of text blocks.
fn notification_text<'a>(
    val: &'a serde_json::Value,
    raw: &'a serde_json::Value,
    msg_type: &str,
) -> Option<std::borrow::Cow<'a, str>> {
    match msg_type {
        "queue-operation" => val.get("content").and_then(content_text),
        "attachment" => val
            .get("attachment")
            .filter(|a| a.get("type").and_then(|t| t.as_str()) == Some("queued_command"))
            .and_then(|a| a.get("prompt"))
            .and_then(content_text),
        // Only the harness's own row: a person's message that quotes a
        // notification is not one.
        "user" | "human" if is_harness_notification_row(raw) => raw
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(content_text),
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

#[path = "stop_anchor.rs"]
mod anchor;
use anchor::{anchor_segment, latest_typed_segment, TaskSegment};

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
        return (!crate::extraction::provenance::starts_with_dreamer_attribution(question)
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
                background.notification(&text);
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
                        let reply = block.get("text").and_then(|v| v.as_str());
                        if let (false, "text", Some(task), Some(reply)) =
                            (sidechain, block_type, segments.last_mut(), reply)
                        {
                            task.assistant_text(reply);
                        }

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
                                if let Some(cleaned) = cleaned {
                                    completed = cut_whole(&cleaned, 300).to_string();
                                    if !sidechain {
                                        // The outcome reads the full cut, past the shown sentence.
                                        closing_text = truncate_str(&cleaned, 300).to_string();
                                    }
                                }
                            }
                        }
                    }
                }
                // Also handle plain string content
                if let Some(text) = content.as_str() {
                    let cleaned = crate::extraction::provenance::extractable(text);
                    if let Some(cleaned) = cleaned {
                        completed = cut_whole(&cleaned, 300).to_string();
                        if !sidechain {
                            // The outcome reads the full cut, past the shown sentence.
                            closing_text = truncate_str(&cleaned, 300).to_string();
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
                                    ..Default::default()
                                },
                            };
                            if let (Some(key), true, true) =
                                (&call.failure_key, failed, !call.other_keys.is_empty())
                            {
                                // A compound failed and the result does not say
                                // which of its commands: none is named, and the
                                // failure stays open until each has passed.
                                open_failures.insert(key.clone());
                                open_failures.extend(call.other_keys.iter().cloned());
                                final_failed = true;
                            } else if let Some(key) = call.failure_key {
                                if failed {
                                    gate_failures.retain(|(open, _)| open != &key);
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
                                    // A pass clears a failure only when the call
                                    // shows its commands passed (`proves_pass`):
                                    // `a || b`, `a; b` and `a | b` may not have
                                    // run or checked `a`.
                                    if call.proves_pass {
                                        for cleared in std::iter::once(&key).chain(&call.other_keys)
                                        {
                                            gate_failures.retain(|(open, _)| open != cleared);
                                            open_failures.remove(cleared);
                                        }
                                    }
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
    let background_open_more = background.more() && background_open_total > 0;
    let reply_of = |index: Option<usize>| index.and_then(|i| segments.get(i)?.first_reply.clone());
    let recap_extras = crate::hooks::recap_evidence::RecapExtras {
        gate_failure: gate_failures.last().map(|(_, failure)| failure.clone()),
        background_open,
        background_open_total,
        background_open_more,
        anchor_reply: reply_of(anchor_index),
        compact_reply: reply_of(latest_typed_segment(&segments).map(|(_, index)| index)),
    };

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
    handle_with(input, engine, cwd, false).await
}

/// The Stop work. `wait_for_close`: the caller is a detached worker, so Claude Code
/// is not held and it can wait, bounded, for the turn's closing row.
pub(crate) async fn handle_with(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
    wait_for_close: bool,
) -> Result<()> {
    // The turn ended: note it first, ahead of any import work, so the PreCompact
    // veto can tell an idle session from one with a turn in flight. A mark never
    // blocks and never fails. A subagent's id is its parent's, so it marks nothing.
    if let Some(session_id) = input
        .session_id
        .as_deref()
        .filter(|_| input.subagent().is_none())
    {
        super::turn_marks::mark_stop(engine.data_dir(), session_id);
    }

    // Stop fires before Claude Code has appended the turn's closing assistant
    // row (measured: a row stamped 180 ms before the worker read was still
    // absent). Reading now would store the previous reply as the last one and
    // call the finished turn unfinished. Wait for it, bounded, then go on. Only
    // a detached worker waits: an inline Stop holds Claude Code, so it reads now.
    if close_wait_applies(input, wait_for_close) {
        if let Some(path) = super::validated_transcript_path(input) {
            await_turn_end(&path, TURN_END_WAIT, TURN_END_POLL).await;
        }
    }

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

/// Whether this Stop waits for the closing row: only a detached worker's, never a
/// subagent's, and not with the kill switch.
fn close_wait_applies(input: &HookInput, wait_for_close: bool) -> bool {
    wait_for_close && input.subagent().is_none() && std::env::var_os("CSR_STOP_NO_WAIT").is_none()
}

/// Longest a Stop worker waits for the closing row of the turn it was told ended.
const TURN_END_WAIT: std::time::Duration = std::time::Duration::from_millis(3000);
/// How often it looks again.
const TURN_END_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// How much of the transcript's end is read per look.
const TURN_END_TAIL_BYTES: u64 = 256 * 1024;

/// Whether the transcript's tail already holds the row that closes the turn: the
/// newest assistant, user or turn-timing row ends a turn (`ends_turn`), and an
/// assistant row that is only thinking is not yet the reply. Rows of other kinds
/// (attachments, api requests, queue operations) and sidechain rows do not
/// decide. A tail with no deciding row, or one that cannot be read, is `false`.
fn tail_closes_turn(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    let start = len.saturating_sub(TURN_END_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut tail = Vec::new();
    if file
        .take(TURN_END_TAIL_BYTES)
        .read_to_end(&mut tail)
        .is_err()
    {
        return false;
    }
    let text = String::from_utf8_lossy(&tail);
    let mut lines = text.lines();
    // A tail read from the middle of the file starts inside a row.
    if start > 0 {
        lines.next();
    }
    let rows: Vec<serde_json::Value> = lines
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    for row in rows.iter().rev() {
        if row.get("isSidechain").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let msg_type = row.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(msg_type, "assistant" | "user" | "human" | "system") {
            continue;
        }
        if msg_type == "system" && !ends_turn(row, msg_type) {
            continue;
        }
        if msg_type == "assistant" {
            let only_thinking = row
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .is_some_and(|blocks| {
                    blocks
                        .iter()
                        .all(|b| b.get("type").and_then(|v| v.as_str()) == Some("thinking"))
                });
            if only_thinking {
                return false;
            }
        }
        return ends_turn(row, msg_type);
    }
    false
}

/// Wait, at most `budget`, until the transcript's tail closes the turn
/// (`tail_closes_turn`). Returns whether it did; a timeout means go on with
/// what is there.
async fn await_turn_end(
    path: &Path,
    budget: std::time::Duration,
    poll: std::time::Duration,
) -> bool {
    let started = std::time::Instant::now();
    loop {
        if tail_closes_turn(path) {
            return true;
        }
        if started.elapsed() >= budget {
            return false;
        }
        tokio::time::sleep(poll).await;
    }
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
/// `s` cut to at most `max_chars` bytes at a sentence or line end, or else at a word end. A fact
/// cut in the middle reads as another fact: "returned 15" is not "returned 1".
fn cut_whole(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let head = truncate_str(s, max_chars);
    let sentence = head
        .char_indices()
        .rev()
        .find(|&(i, c)| {
            c == '\n'
                || (matches!(c, '.' | '!' | '?')
                    && head[i + c.len_utf8()..].starts_with(char::is_whitespace))
        })
        .map(|(i, c)| i + c.len_utf8());
    let end = match sentence {
        Some(end) if end >= max_chars / 2 => end,
        _ if s[head.len()..].starts_with(char::is_whitespace) => head.len(),
        _ => head.rfind(char::is_whitespace).unwrap_or(head.len()),
    };
    head[..end].trim_end()
}

fn truncate_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let boundary = s.floor_char_boundary(max_chars);
    &s[..boundary]
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
