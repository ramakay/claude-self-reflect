//! What a git command comes to under a guard: port of the removed `coach.ts` from `parseGuard` to
//! `protectedFromGit` (the guard file, the tripwire over the raw text, the parse, the verdict).
//!
//! The guard is opt-in: a repository says what it protects in `<git toplevel>/.claude/
//! csr-guard.json`, a person for every repository in csr-companion's `guard.json`; the two are one
//! guard. Neither there: no guard, no git asked anything.

use super::{
    git::{
        assigned, destination, git_call, git_env, git_options, last_word, off_remote, push_words,
        ConfigArg, Ordered, COMMIT_OPTIONS, COMMIT_SHORTS, MERGE_OPTIONS, MERGE_SHORTS,
    },
    shell::{has_word, head_at, heredocs, inner, split_words, split_ws, substitutions, trim, Word},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    panic::{catch_unwind, AssertUnwindSafe},
};

pub const NO_VERIFY: &str =
    "--no-verify skips the pre-commit gate. Fix what the gate flags instead.";
/// A reason a guard file may give for release/v* (the guard's own tests use it); a push the guard
/// protects by a glob with no reason of its own is told PROTECTED. This repository ships no guard.
pub const RELEASE: &str = "Pushing release/v* publishes to npm. Push a backport/... branch instead, or ask the person to push it.";
pub const HOOKS_OFF: &str = "core.hooksPath set for one git call skips the pre-commit gate. Fix what the gate flags instead.";
pub const UNRESOLVED: &str = "This git command depends on something the check cannot resolve (a $variable or substitution in a setting, a variable it cannot read, or settings git refuses), so it is denied. Write the values out, or ask the person to run it.";
pub const AS_TEXT: &str =
    "If this is only text (a commit message, a doc), put it in a file and use git commit -F <file>.";

pub fn protected_text(glob: &str) -> String {
    format!(
        "Pushing to {glob} is blocked by csr-guard.json. Push another branch, or ask the person to push it."
    )
}

// ---------------------------------------------------------------- what the guard guards

/// - `deny_no_verify`: a commit, merge or push that skips the pre-commit gate (--no-verify, -n,
///   core.hooksPath)
/// - `protected_push`: globs on the branch a push lands on (`release/v*`, `release/*`: `*` runs
///   across `/`)
/// - `reason`: what to tell Claude, per glob
#[derive(Clone, Debug, PartialEq)]
pub struct Guard {
    pub deny_no_verify: bool,
    pub protected_push: Vec<String>,
    pub reason: HashMap<String, String>,
}

/// A guard file's words, or none when they are not one (not JSON, or a key of the wrong kind).
pub fn parse_guard(text: &str) -> Option<Guard> {
    let Value::Object(value) = serde_json::from_str::<Value>(text).ok()? else {
        return None;
    };
    // An absent key takes its default; a present one of the wrong kind (null included) is no guard.
    let deny_no_verify = match value.get("denyNoVerify") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return None,
    };
    let protected_push = match value.get("protectedPush") {
        None => Vec::new(),
        Some(Value::Array(globs)) => {
            let mut out = Vec::new();
            for glob in globs {
                match glob.as_str() {
                    Some(text) if !text.is_empty() => out.push(text.to_string()),
                    _ => return None,
                }
            }
            out
        }
        Some(_) => return None,
    };
    let reason = match value.get("reason") {
        None => HashMap::new(),
        Some(Value::Object(said)) => {
            let mut out = HashMap::new();
            for (glob, why) in said {
                out.insert(glob.clone(), why.as_str()?.to_string());
            }
            out
        }
        Some(_) => return None,
    };
    Some(Guard {
        deny_no_verify,
        protected_push,
        reason,
    })
}

/// Every protection of each guard; none when there is none at all.
pub fn merge_guards(guards: &[Guard]) -> Option<Guard> {
    let mut merged = Guard {
        deny_no_verify: guards.iter().any(|g| g.deny_no_verify),
        protected_push: Vec::new(),
        reason: HashMap::new(),
    };
    for guard in guards {
        for glob in &guard.protected_push {
            if !merged.protected_push.contains(glob) {
                merged.protected_push.push(glob.clone());
            }
        }
    }
    for guard in guards.iter().rev() {
        merged.reason.extend(guard.reason.clone()); // the first one's wins
    }
    (merged.deny_no_verify || !merged.protected_push.is_empty()).then_some(merged)
}

/// `^GLOB$` with `*` for `.*` and `?` for `.` and nothing else special; JavaScript's `.` does not
/// match a line end.
fn glob_matches(glob: &str, branch: &str) -> bool {
    let line_end = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    let glob: Vec<char> = glob.chars().collect();
    let text: Vec<char> = branch.chars().collect();
    // matches[j]: glob[..i] can match text[..j]
    let mut matches = vec![false; text.len() + 1];
    matches[0] = true;
    for &g in &glob {
        let mut next = vec![false; text.len() + 1];
        match g {
            '*' => {
                next[0] = matches[0];
                for j in 1..=text.len() {
                    next[j] = matches[j] || (next[j - 1] && !line_end(text[j - 1]));
                }
            }
            '?' => {
                for j in 1..=text.len() {
                    next[j] = matches[j - 1] && !line_end(text[j - 1]);
                }
            }
            literal => {
                for j in 1..=text.len() {
                    next[j] = matches[j - 1] && text[j - 1] == literal;
                }
            }
        }
        matches = next;
    }
    matches[text.len()]
}

/// The glob a branch name is protected by, or none.
pub fn protected_by<'a>(guard: &'a Guard, branch: &str) -> Option<&'a str> {
    guard
        .protected_push
        .iter()
        .find(|glob| glob_matches(glob, branch))
        .map(String::as_str)
}

pub fn reason_for(guard: &Guard, glob: &str) -> String {
    guard
        .reason
        .get(glob)
        .cloned()
        .unwrap_or_else(|| protected_text(glob))
}

/// Every branch: the first glob's.
fn any_protected(guard: &Guard) -> String {
    reason_for(
        guard,
        guard.protected_push.first().map_or("", String::as_str),
    )
}

// ---------------------------------------------------------------- the tripwire

/// `/(?:^|[^\w.-])git(?![\w-])/.test(command)`: git as a word.
pub fn git_word(command: &str) -> bool {
    command.match_indices("git").any(|(at, _)| {
        let before = command[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')));
        let after = command[at + 3..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')));
        before && after
    })
}

/// `command.split(PIECES)`, PIECES = `\n|;|&&|\|\||\||&|\(|\)|\{|\}|`|\$\(`.
fn split_pieces(command: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < command.len() {
        let rest = &command[at..];
        let width = if rest.starts_with("&&") || rest.starts_with("||") || rest.starts_with("$(") {
            2
        } else if rest.starts_with(['\n', ';', '|', '&', '(', ')', '{', '}', '`']) {
            1
        } else {
            0
        };
        if width == 0 {
            at += rest.chars().next().map_or(1, char::len_utf8);
        } else {
            out.push(&command[start..at]);
            at += width;
            start = at;
        }
    }
    out.push(&command[start..]);
    out
}

/// The pieces of a command where git stands as the command: the text cut at newlines `;` `&&`
/// `||` `|` `&` `(` `)` `{` `}` backticks and `$(`, quotes ignored, each piece's head past its
/// assignments and wrappers (env, sudo, nohup, timeout <n>, xargs, ...).
pub fn git_pieces(command: &str) -> Vec<&str> {
    split_pieces(command)
        .into_iter()
        .filter(|piece| {
            let words: Vec<String> = split_ws(trim(piece))
                .into_iter()
                .map(|word| word.replace(['\'', '"', '\\'], ""))
                .filter(|word| !word.is_empty())
                .collect();
            let head = words.get(head_at(&words)).map_or("", String::as_str);
            head == "git" || head.ends_with("/git")
        })
        .collect()
}

/// Before any parsing, over the raw text: a git command with words that are never right in it is
/// denied. The parser cannot be the only gate. It reads only where git stands as the command, so
/// a heredoc's line that starts with git trips it (accepted), and `codex exec "... git ...
/// --no-verify ..."` or `grep 'release/v' f && git status` do not. The words are the guard's:
/// --no-verify and hooksPath for `deny_no_verify`; for `protected_push`, each glob's fixed part
/// (`release/v` of `release/v*`), --mirror, and a ':' or '+:' push refspec. A denial the parse
/// does not confirm adds AS_TEXT.
pub fn tripwire(command: &str, guard: &Guard) -> Option<String> {
    for piece in git_pieces(command) {
        if guard.deny_no_verify && piece.contains("--no-verify") {
            return Some(NO_VERIFY.into());
        }
        if guard.deny_no_verify && piece.to_ascii_lowercase().contains("hookspath") {
            return Some(HOOKS_OFF.into());
        }
        if guard.protected_push.is_empty() {
            continue;
        }
        for glob in &guard.protected_push {
            let fixed = glob.split(['*', '?', '[']).next().unwrap_or("");
            let trips = if fixed.is_empty() {
                has_word(piece, "push")
            } else {
                piece.contains(fixed)
            };
            if trips {
                return Some(reason_for(guard, glob));
            }
        }
        let matching = split_ws(piece).into_iter().any(|word| {
            let word = word.trim_start_matches(['\'', '"']);
            word.strip_prefix('+').unwrap_or(word).starts_with(':')
        });
        if piece.contains("--mirror") || (has_word(piece, "push") && matching) {
            return Some(any_protected(guard));
        }
    }
    None
}

// ---------------------------------------------------------------- the verdict

/// A push whose landing the repository decides: its directory, the remote it names, its git config
/// arguments in the order given, and the variables the command line sets that git reads: those
/// --config-env names, and GIT_CONFIG_* (GIT_CONFIG_COUNT/KEY_n/VALUE_n). `implicit`: it names no
/// refspec, so git takes one from the remote's push refspecs or push.default.
#[derive(Clone, Debug, PartialEq)]
pub struct GitPush {
    pub dir: Option<String>,
    pub remote: Option<String>,
    pub config: Vec<ConfigArg>,
    pub env: Option<Ordered<String>>,
    pub implicit: bool,
}

/// `deny`: say no, with this reason. `Check`: where a push lands depends on the repository (no
/// refspec, HEAD, a configured push), which the hook settles by asking git. `Ask`: any other push
/// (the hook lets it through).
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Deny(String),
    Ask,
    Check(Vec<GitPush>),
}

pub type GitVerdict = Option<Verdict>;

fn deny(reason: &str) -> GitVerdict {
    Some(Verdict::Deny(reason.to_string()))
}

/// Where the commands of a line run: unknown after a `cd` the check cannot follow.
#[derive(Clone, Debug, PartialEq)]
enum Here {
    Unset,
    Unknown,
    Dir(String),
}

impl Here {
    fn of(cwd: Option<&str>) -> Self {
        cwd.filter(|dir| !dir.is_empty())
            .map_or(Self::Unset, |dir| Self::Dir(dir.to_string()))
    }
}

/// The worst verdict wins; two checks are one with both pushes in it.
fn keep(worst: &mut GitVerdict, found: GitVerdict) {
    let Some(found) = found else { return };
    if let (Some(Verdict::Check(was)), Verdict::Check(more)) = (worst.as_mut(), &found) {
        was.extend(more.iter().cloned());
        return;
    }
    let replace = matches!(
        (worst.as_ref(), &found),
        (None, _) | (_, Verdict::Deny(_)) | (Some(Verdict::Ask), Verdict::Check(_))
    );
    if replace {
        *worst = Some(found);
    }
}

fn is_commit(sub: &str) -> bool {
    matches!(sub, "commit" | "merge" | "push")
}

/// `/^\+?:$/`: the matching refspec.
fn matching_spec(spec: &str) -> bool {
    spec.strip_prefix('+').unwrap_or(spec) == ":"
}

fn parsed_verdict(command: &str, depth: usize, cwd: &Here, guard: &Guard) -> GitVerdict {
    let protects = !guard.protected_push.is_empty();
    let mut worst: GitVerdict = None;
    if depth < 3 {
        for text in scripts_within(command) {
            let found = parsed_verdict(&text, depth + 1, cwd, guard);
            if matches!(found, Some(Verdict::Deny(_))) {
                return found;
            }
            keep(&mut worst, found);
        }
    }
    let mut here = cwd.clone();
    // `export DEST=...;` or `DEST=...;`: what the commands after it may read
    let mut set: Ordered<super::git::Assigned> = Ordered::default();
    for tokens in split_words(&heredocs(command).0) {
        let words: Vec<String> = tokens.iter().map(|t| t.text.clone()).collect();
        if words.first().map(String::as_str) == Some("cd") {
            // where the commands after it run: a relative one from where it was, $ ~ - unknown
            here = match tokens.get(1) {
                Some(to) if !to.expands && !to.text.starts_with(['~', '-']) => {
                    if to.text.starts_with('/') {
                        Here::Dir(to.text.clone())
                    } else if let Here::Dir(from) = &here {
                        Here::Dir(format!("{from}/{}", to.text))
                    } else {
                        here.clone()
                    }
                }
                _ => Here::Unknown,
            };
        }
        let plain: &[Word] = if words.first().map(String::as_str) == Some("export") {
            &tokens[1..]
        } else {
            &tokens
        };
        if !plain.is_empty()
            && plain
                .iter()
                .all(|w| super::git::assignment(&w.text).is_some())
        {
            for (name, value) in assigned(plain).0 {
                set.set(&name, value);
            }
        }
        if depth < 3 {
            for text in inner(&words) {
                let found = parsed_verdict(&text, depth + 1, &here, guard);
                if matches!(found, Some(Verdict::Deny(_))) {
                    return found;
                }
                keep(&mut worst, found);
            }
        }
        let Some(call) = git_call(&tokens) else {
            continue;
        };
        let (sub, rest) = (call.sub.as_str(), &call.rest);
        let gate = guard.deny_no_verify;
        if gate && call.hooks_off && is_commit(sub) {
            return deny(HOOKS_OFF);
        }
        let mut readable = set.clone();
        for (name, value) in &call.env.0 {
            readable.set(name, value.clone());
        }
        let (env, unknown) = git_env(&call.config, &readable);
        // its settings unknown: a hooksPath could be among them, or where a push lands
        let asks_git = if gate {
            is_commit(sub)
        } else {
            protects && sub == "push"
        };
        if asks_git && (call.config_expands || unknown) {
            return deny(UNRESOLVED);
        }
        if gate && sub == "commit" && git_options(rest, &COMMIT_OPTIONS, &COMMIT_SHORTS).skip {
            return deny(NO_VERIFY);
        }
        if gate && sub == "merge" && git_options(rest, &MERGE_OPTIONS, &MERGE_SHORTS).skip {
            return deny(NO_VERIFY);
        }
        if sub != "push" {
            continue;
        }
        let push = push_words(rest);
        if gate && push.skip {
            return deny(NO_VERIFY);
        }
        if push.dry || !protects {
            continue; // nothing is sent, or no branch is protected
        }
        // every branch, a wildcard, or the matching refspec ':' (every branch both sides have, a
        // protected one among them)
        if push.every
            || push
                .specs
                .iter()
                .any(|s| s.contains('*') || matching_spec(s))
        {
            return Some(Verdict::Deny(any_protected(guard)));
        }
        let hit = push
            .specs
            .iter()
            .find_map(|s| protected_by(guard, &destination(s)));
        if let Some(glob) = hit {
            return Some(Verdict::Deny(reason_for(guard, glob)));
        }
        let to_head = push
            .specs
            .iter()
            .any(|s| matches!(destination(s).as_str(), "HEAD" | "@"));
        if push.specs.is_empty() || to_head {
            let absolute =
                call.dir.as_deref().is_some_and(|d| d.starts_with('/')) && !call.dir_expands;
            if call.dir_expands || (!absolute && here == Here::Unknown) {
                return deny(UNRESOLVED); // where it runs: unknown
            }
            let dir = match (call.dir.as_deref().filter(|d| !d.is_empty()), &here) {
                (Some(dir), Here::Dir(from)) if !absolute => Some(format!("{from}/{dir}")),
                (Some(dir), _) => Some(dir.to_string()),
                (None, Here::Dir(from)) => Some(from.clone()),
                (None, _) => None,
            };
            keep(
                &mut worst,
                Some(Verdict::Check(vec![GitPush {
                    dir,
                    remote: push.remote.clone(),
                    config: call.config.clone(),
                    env: (!env.is_empty()).then_some(env),
                    implicit: push.specs.is_empty(),
                }])),
            );
        } else {
            keep(&mut worst, Some(Verdict::Ask));
        }
    }
    worst
}

/// The scripts a shell reads from a heredoc (`bash <<EOF ... EOF`), and what `$(...)` or backticks
/// run: both are commands, though `commands()` reads the one as text and the other as a word
/// inside quotes. One reading of the heredocs for both: a shell's heredoc is its script whatever
/// its word's quotes.
fn scripts_within(src: &str) -> Vec<String> {
    let (text, bodies) = heredocs(src);
    let mut out = substitutions(&text, true);
    for body in bodies {
        if body.shell {
            out.push(body.body.clone());
        }
        if !body.quoted {
            out.extend(substitutions(&body.body, false));
        }
    }
    out
}

/// What a git command should come to under a guard: deny what skips the pre-commit gate
/// (`deny_no_verify`) and a push that lands on a protected branch (`protected_push`); "check" when
/// where a push lands depends on the repository (no refspec, HEAD, a configured push), which the
/// hook settles by asking git; "ask" for any other push (the hook lets it through); none for
/// nothing to say. No guard, or one that protects nothing: none, always.
///
/// The tripwire runs first over the raw text, and the parse can only make its denial more precise;
/// a parse that fails denies a command with git in it. Fail closed.
pub fn git_verdict(command: &str, guard: Option<&Guard>, cwd: Option<&str>) -> GitVerdict {
    let guard = guard.filter(|g| g.deny_no_verify || !g.protected_push.is_empty())?;
    // The TypeScript's `try`: what throws there panics here, and either way a command with git in
    // it is denied.
    let read = catch_unwind(AssertUnwindSafe(|| {
        let tripped = tripwire(command, guard);
        let parsed = parsed_verdict(command, 0, &Here::of(cwd), guard);
        (tripped, parsed)
    }));
    let (tripped, parsed) = match read {
        Ok(both) => both,
        Err(_) => {
            return if git_word(command) {
                deny(UNRESOLVED)
            } else {
                None
            }
        }
    };
    match tripped {
        Some(said) if !matches!(parsed, Some(Verdict::Deny(_))) => {
            Some(Verdict::Deny(format!("{said} {AS_TEXT}")))
        }
        _ => parsed,
    }
}

/// The parse alone, under the tripwire: what the commands are, for the guard's own tests.
pub fn git_parse(command: &str, guard: &Guard, cwd: Option<&str>) -> GitVerdict {
    parsed_verdict(command, 0, &Here::of(cwd), guard)
}

/// What a "check" push lands on, from git itself: the current branch, its upstream, and the push
/// refspecs set for the remote it goes to. The reason to deny when any of them is a protected
/// branch (or a refspec is a wildcard, which may be one), or none.
pub fn protected_from_git(
    guard: &Guard,
    branch: &str,
    upstream: &str,
    push_config: &str,
) -> Option<String> {
    let hit = protected_by(guard, trim(branch))
        .or_else(|| protected_by(guard, off_remote(trim(upstream))));
    if let Some(glob) = hit {
        return Some(reason_for(guard, glob));
    }
    for line in push_config.split('\n') {
        let spec = last_word(line);
        if spec.contains('*') {
            return Some(any_protected(guard));
        }
        let found = if spec.is_empty() {
            None
        } else {
            protected_by(guard, &destination(spec))
        };
        if let Some(glob) = found {
            return Some(reason_for(guard, glob));
        }
    }
    None
}
