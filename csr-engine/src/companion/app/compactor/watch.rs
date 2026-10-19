//! What the hooks read of the session's tools, and what the keep list reads of the repository:
//! `commitDir`, the patterns that find a task's id and a commit in a tool's answer, and
//! `keepOf` / `workOf` of register.tsx.

use super::{
    outside::Outside,
    rules::{js_space, last_question, Keep, Running},
    Compactor,
};
use crate::companion::relay::dispatch::Ctx;
use regex::Regex;
use std::sync::LazyLock;

/// A JavaScript `\s`, as a class of the regex crate.
const S: &str = r"[\s\x{FEFF}--\x{85}]";

/// `git commit`'s own "[branch 6369e6b] subject".
pub static COMMIT_OUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?mR)^\[[^\]\n]*?{S}([0-9a-f]{{7,40}})\]")).unwrap());

/// A background task's id in the result of the call that started it: a shell's "with ID: b...",
/// an agent's "agentId: a...". The Stop hook's task list carries the same ids.
pub static TASK_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?-u:\b)(?:agentId|ID|task_id|taskId):{S}*([A-Za-z0-9_-]{{6,}})"
    ))
    .unwrap()
});

pub static GIT_COMMIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)git(?-u:\b)[^\n|;&]*(?-u:\b)commit(?-u:\b)").unwrap());

pub static LAUNCHED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u:launched|in the background)").unwrap());

pub static TN_TOOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<tool-use-id>([^<]+)</tool-use-id>").unwrap());
pub static TN_TASK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<task-id>([^<]+)</task-id>").unwrap());
pub static TN_DONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<status>(?:completed|failed|killed|stopped|cancelled|error)</status>").unwrap()
});

/// The row of `/config` whose value is the setting: `csr-companion.autoCompact`, or
/// `csr-companion@<where>.autoCompact`.
pub static AUTO_ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^csr-companion(?:@[^.]+)?\.autoCompact$").unwrap());

enum Token {
    Space,
    Break,
    Word(String),
}

/// The token at `at`, as `/[^\S\n]+|\n|&&|\|\||;|'[^']*'|"[^"\\$`]*"|[^\s'";&|()<>\\$`]+/y`
/// matches it; none where nothing does.
fn token_at(chars: &[char], at: usize) -> Option<(Token, usize)> {
    let c = chars[at];
    let blank = |c: char| js_space(c) && c != '\n';
    if blank(c) {
        let n = chars[at..].iter().take_while(|c| blank(**c)).count();
        return Some((Token::Space, at + n));
    }
    if c == '\n' || c == ';' {
        return Some((Token::Break, at + 1));
    }
    if (c == '&' || c == '|') && chars.get(at + 1) == Some(&c) {
        return Some((Token::Break, at + 2));
    }
    let text = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    if c == '\'' {
        let close = chars[at + 1..].iter().position(|c| *c == '\'')?;
        let end = at + 1 + close + 1;
        return Some((Token::Word(text(at, end)), end));
    }
    if c == '"' {
        let stop = |c: &char| matches!(c, '"' | '\\' | '$' | '`');
        let close = chars[at + 1..].iter().position(stop)?;
        if chars[at + 1 + close] != '"' {
            return None;
        }
        let end = at + 1 + close + 1;
        return Some((Token::Word(text(at, end)), end));
    }
    let word = |c: char| !js_space(c) && !"'\";&|()<>\\$`".contains(c);
    let n = chars[at..].iter().take_while(|c| word(**c)).count();
    (n > 0).then(|| (Token::Word(text(at, at + n)), at + n))
}

/// `word.replace(/^['"]|['"]$/g, '')`.
fn unquoted(word: &str) -> String {
    let quote = |c: char| c == '\'' || c == '"';
    let word = word.strip_prefix(quote).unwrap_or(word);
    word.strip_suffix(quote).unwrap_or(word).to_string()
}

/// A path as `resolve` of commitDir makes it: relative to `base`, `..` and `.` folded when
/// `logical` (git follows links before resolving `..`, so a `-C` path is left as it is).
fn resolve(base: &str, dir: &str, logical: bool) -> Option<String> {
    if dir.is_empty() || dir.starts_with('~') {
        return None;
    }
    let path = if dir.starts_with('/') {
        dir.to_string()
    } else {
        format!("{base}/{dir}")
    };
    if !logical {
        return Some(path);
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part == ".." {
            parts.pop();
        } else if !part.is_empty() && part != "." {
            parts.push(part);
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// The folder a command's one `git commit` ran in: resolve only a simple chain whose commit and
/// directory are unambiguous.
pub fn commit_dir(command: &str, cwd: &str) -> Option<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    let mut end = 0;
    while end < chars.len() {
        let Some((token, next)) = token_at(&chars, end) else {
            break;
        };
        end = next;
        match token {
            Token::Space => (),
            Token::Break => segments.push(Vec::new()),
            Token::Word(word) => segments.last_mut()?.push(unquoted(&word)),
        }
    }
    if end != chars.len() {
        return None;
    }
    let mut directory = cwd.to_string();
    let mut committed: Option<String> = None;
    for words in &segments {
        let Some(first) = words.first() else { continue };
        if first == "cd" {
            if words.len() != 2 {
                return None;
            }
            directory = resolve(&directory, &words[1], true)?;
            continue;
        }
        if first != "git" {
            return None;
        }
        let mut dir = directory.clone();
        let mut i = 1;
        while words.get(i).is_some_and(|word| word.starts_with('-')) {
            let option = &words[i];
            i += 1;
            if option == "-C" {
                let next = words.get(i);
                i += 1;
                dir = resolve(&dir, next?, false)?;
            } else if option == "-c" {
                let next = words.get(i);
                i += 1;
                if next.is_none_or(String::is_empty) {
                    return None;
                }
            } else {
                return None;
            }
        }
        if words.get(i).is_some_and(|word| word == "commit") {
            if committed.is_some() {
                return None;
            }
            committed = Some(dir);
        }
    }
    committed
}

/// A `git status --porcelain` line's path, from the repository's top: the new name of a rename,
/// quotes off.
fn porcelain_path(line: &str) -> String {
    let rest: String = line.chars().skip(3).collect();
    let name = rest.rsplit(" -> ").next().unwrap_or("");
    let name = name.strip_prefix('"').unwrap_or(name);
    name.strip_suffix('"').unwrap_or(name).to_string()
}

/// Unfinished work that is this session's: uncommitted changes to files it wrote (its Edit and
/// Write calls), or in a repository its main thread committed in, and that repository's commits
/// not pushed. Another session's changes in the same checkout are not this one's. Nothing of its
/// own: no section.
async fn work_of(cx: &Ctx, out: &dyn Outside, cwd: &str) -> Vec<String> {
    let Some(top) = out.git(cwd, &["rev-parse", "--show-toplevel"]).await else {
        return Vec::new();
    };
    if top.is_empty() {
        return Vec::new();
    }
    let dirs: Vec<String> = cx.kept(|s: &mut Compactor| s.committed_in.iter().cloned().collect());
    let mut committed = false;
    for dir in &dirs {
        let found = out.git(dir, &["rev-parse", "--show-toplevel"]).await;
        committed |= found.as_deref() == Some(top.as_str());
    }
    let wrote = |line: &str| {
        let path = format!("{top}/{}", porcelain_path(line));
        cx.kept(|s: &mut Compactor| {
            if path.ends_with('/') {
                s.wrote.iter().any(|file| file.starts_with(&path))
            } else {
                s.wrote.contains(&path)
            }
        })
    };
    let mut work: Vec<String> = Vec::new();
    let status = out.git(cwd, &["status", "--porcelain"]).await;
    let dirty: Vec<&str> = status
        .as_deref()
        .map(|text| text.split('\n').collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|line| !line.is_empty())
        .filter(|line| committed || wrote(line))
        .collect();
    if !dirty.is_empty() {
        let names: Vec<String> = dirty
            .iter()
            .take(3)
            .map(|line| {
                let path = porcelain_path(line);
                let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
                let from = parts.len().saturating_sub(2);
                let slash = if line.ends_with('/') { "/" } else { "" };
                format!("{}{slash}", parts[from..].join("/"))
            })
            .collect();
        let n = dirty.len();
        work.push(format!(
            "{n} uncommitted {} ({}{})",
            if n == 1 { "file" } else { "files" },
            names.join(", "),
            if n > 3 { ", …" } else { "" }
        ));
    }
    let ahead: Vec<String> = if committed {
        out.git(cwd, &["log", "--format=%h", "@{u}..HEAD"])
            .await
            .map(|text| text.split('\n').map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    if !ahead.is_empty() {
        let more = if ahead.len() > 4 {
            format!(" and {} more", ahead.len() - 4)
        } else {
            String::new()
        };
        let shown: Vec<&str> = ahead.iter().take(4).map(String::as_str).collect();
        work.push(format!("not pushed: {}{more}", shown.join(", ")));
    }
    if !work.is_empty() {
        let head = out.git(cwd, &["rev-parse", "--short", "HEAD"]).await;
        let branch = out
            .git(cwd, &["symbolic-ref", "-q", "--short", "HEAD"])
            .await;
        if let Some(head) = head.filter(|head| !head.is_empty()) {
            let at = match branch.filter(|branch| !branch.is_empty()) {
                Some(branch) => format!("branch {branch} at"),
                None => "HEAD".to_string(),
            };
            work.insert(0, format!("{at} {head}"));
        }
    }
    work
}

/// What the summary must keep: background work with ids, open decisions, unfinished work with
/// SHAs, rules. Running: the main thread's own tasks; a shell a subagent started is that
/// subagent's, which is kept itself. The open decision is the question this turn ended on.
/// Lost and found's threads stay out: the threads set aside for later reach the summary by
/// `carry`, which a compaction of CSR's runs too.
pub async fn keep_of(cx: &Ctx, out: &dyn Outside, answer: &str) -> Keep {
    let asked = last_question(answer);
    let work = work_of(cx, out, &cx.cwd).await;
    let (running, rules) = cx.kept(|s: &mut Compactor| {
        let running: Vec<Running> = (s.running().into_iter())
            .filter(|task| !s.theirs.contains_key(&task.id))
            .collect();
        let from = s.rules.len().saturating_sub(super::RULES_KEPT);
        (running, s.rules[from..].to_vec())
    });
    Keep {
        running,
        decisions: asked.into_iter().collect(),
        work,
        rules,
    }
}
