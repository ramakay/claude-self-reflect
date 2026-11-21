//! Which files a Bash command writes, read from its text.
//!
//! One parser for the live PostToolUse hook and the co-edit backfill, so a
//! session that edits through `sed -i`, a heredoc or a short Python script
//! lands in the file history the way an Edit or Write does. Claude Code's auto
//! mode steers edits to exactly those forms.
//!
//! Literal targets only. A path that holds `$`, a glob, a brace, a backtick or
//! a command substitution is skipped, never guessed, and so is a Python path
//! built by code. The parser does no I/O except one `is_dir` on each
//! candidate, so a call that writes nothing costs microseconds.
//!
//! Forms read:
//! - `sed -i` / `sed -i ''` / `--in-place`, `perl -pi` / `-i`: the file operands
//! - redirects (`> f`, `>> f`, `&> f`, `cat > f <<EOF`) and `tee [-a] f`
//! - Python in a `-c` string or a heredoc body: `open('<lit>', 'w')`,
//!   `Path('<lit>').write_text(..)`, and the read-replace-write idiom where
//!   the path is a name every assignment of which is a plain literal
//! - `sh|bash|zsh -c '<command>'`, parsed again
//!
//! A plain `NAME=value` (or `export NAME=value`) with a literal value, made as
//! its own command at the top level, resolves later `$NAME`, `${NAME}` and
//! `"$NAME"` in the same command text. Everything else about variables stays
//! unknown and skipped: a prefix assignment (`W=x cmd`), an assignment in a
//! loop, branch, pipeline, subshell or `&&` tail, a value from a substitution,
//! a `for` variable, `${NAME:-x}` and the other expansions.
//!
//! `cd <literal>` moves the base directory for the commands after it; a `cd`
//! the parser cannot read (`cd "$X"`, `cd -`) makes later relative paths
//! unresolvable, so they are skipped.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// How deep `bash -c '...'` is followed.
const MAX_SHELL_DEPTH: usize = 2;

/// Files a Bash command writes, in order of appearance, without repeats.
/// `cwd` is the directory the command ran in; an empty `cwd` leaves relative
/// paths unresolvable (absolute ones still count).
pub fn bash_writes(command: &str, cwd: &Path) -> Vec<PathBuf> {
    if !could_write(command) {
        return Vec::new();
    }
    let base = (!cwd.as_os_str().is_empty()).then(|| cwd.to_path_buf());
    let mut out = Vec::new();
    collect(command, base, 0, &mut out);
    out
}

/// Cheap reject before any lexing: a command with none of these substrings
/// writes no file this parser reads.
fn could_write(command: &str) -> bool {
    command.contains('>')
        || command.contains("tee")
        || command.contains("sed")
        || command.contains("perl")
        || command.contains("open(")
        || command.contains("write_")
        || command.contains(" -c")
}

// ─── lexer ───

#[derive(Debug, Clone, Default)]
struct Word {
    text: String,
    /// Holds `$`, a backtick, an unquoted glob or brace: not a literal path.
    dynamic: bool,
}

#[derive(Debug, Default)]
struct Cmd {
    words: Vec<Word>,
    redirects: Vec<Word>,
    heredoc: Option<String>,
    open: bool,
    close: bool,
}

impl Cmd {
    fn is_blank(&self) -> bool {
        self.words.is_empty()
            && self.redirects.is_empty()
            && self.heredoc.is_none()
            && !self.open
            && !self.close
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    /// The next word is a redirect target.
    Target,
    /// The next word is read and dropped (`< in`, `>&2`, `<<<`).
    Ignore,
    /// The next word is a heredoc delimiter (`true`: `<<-`, tabs stripped).
    Delimiter(bool),
}

#[derive(Default)]
struct WordBuf {
    text: String,
    dynamic: bool,
    started: bool,
    tilde: bool,
}

struct Lexer {
    cmds: Vec<Cmd>,
    cur: WordBuf,
    mode: Mode,
    heredocs: Vec<(usize, String, bool)>,
    /// Shell variables with a known literal value.
    vars: HashMap<String, String>,
    /// `vars` as it was at each open `(`, restored at the matching `)`.
    var_stack: Vec<HashMap<String, String>>,
    /// Separators seen since the previous command, `&&` / `|` / `;` ...
    before: String,
    /// Open `for` / `while` / `if` / `case` blocks.
    depth: usize,
}

impl Lexer {
    fn new() -> Self {
        Lexer {
            cmds: vec![Cmd::default()],
            cur: WordBuf::default(),
            mode: Mode::Normal,
            heredocs: Vec::new(),
            vars: HashMap::new(),
            var_stack: Vec::new(),
            before: String::new(),
            depth: 0,
        }
    }

    fn flush(&mut self) {
        if !self.cur.started {
            return;
        }
        let buf = std::mem::take(&mut self.cur);
        let mut text = buf.text;
        let mut dynamic = buf.dynamic;
        if buf.tilde && !dynamic {
            if text == "~" {
                text = home_string();
            } else if let Some(rest) = text.strip_prefix("~/") {
                text = format!("{}/{rest}", home_string());
            } else {
                // `~user/...` names another user's home: not resolvable here.
                dynamic = true;
            }
        }
        let word = Word { text, dynamic };
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.last().words.push(word),
            Mode::Target => self.last().redirects.push(word),
            Mode::Ignore => {}
            Mode::Delimiter(strip) => {
                let idx = self.cmds.len() - 1;
                self.heredocs.push((idx, word.text, strip));
            }
        }
    }

    fn last(&mut self) -> &mut Cmd {
        self.cmds.last_mut().expect("lexer always holds a command")
    }

    /// End the current simple command (`;`, `&&`, `|`, newline).
    fn end_cmd(&mut self, sep: &str) {
        self.flush();
        self.mode = Mode::Normal;
        if self.cmds.last().is_some_and(Cmd::is_blank) {
            self.before.push_str(sep);
        } else {
            self.track_vars(sep);
            self.cmds.push(Cmd::default());
            self.before = sep.to_string();
        }
    }

    /// Update the known variables from the command that just ended. `sep` is
    /// the separator that ended it.
    fn track_vars(&mut self, sep: &str) {
        let words: Vec<Word> = self.last().words.clone();
        let first = words.first().map_or("", |w| w.text.as_str());
        if matches!(first, "done" | "fi" | "esac") {
            self.depth = self.depth.saturating_sub(1);
        }
        let exported = first == "export";
        let operands = if exported { &words[1..] } else { &words[..] };
        let pure = !operands.is_empty()
            && operands
                .iter()
                .all(|w| is_assignment(&w.text) || (exported && is_name(&w.text)));
        let carry = pure
            && self.depth == 0
            && !self.before.contains(['&', '|'])
            && matches!(sep, ";" | "\n" | "&&" | ")");
        match first {
            "eval" | "source" | "." => self.vars.clear(),
            "unset" | "read" | "declare" | "typeset" | "local" | "readonly" | "getopts"
            | "mapfile" | "readarray" | "printf" | "let" | "for" | "select" => {
                for w in &words[1..] {
                    self.vars
                        .remove(assigned_name(&w.text).unwrap_or(w.text.as_str()));
                }
            }
            _ => {}
        }
        for w in &words {
            let Some(name) = assigned_name(&w.text) else {
                continue;
            };
            let value = w.text[name.len()..].strip_prefix('=').unwrap_or("");
            let known = carry
                && !w.dynamic
                && is_assignment(&w.text)
                && !value.is_empty()
                && !value.starts_with('~')
                && !value.contains(['$', '`']);
            if known {
                self.vars.insert(name.to_string(), value.to_string());
            } else {
                self.vars.remove(name);
            }
        }
        if matches!(first, "for" | "while" | "until" | "if" | "case" | "select") {
            self.depth += 1;
        }
    }

    /// Put the value of a known variable into the word being read; an unknown
    /// one leaves `$NAME` in the text and marks the word dynamic.
    fn expand(&mut self, name: &str, quoted: bool) {
        self.cur.started = true;
        match self.vars.get(name) {
            Some(value) => {
                let splits = value
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | '[' | '{' | '}'));
                if splits && !quoted {
                    self.cur.dynamic = true;
                }
                self.cur.text.push_str(value);
            }
            None => {
                self.cur.dynamic = true;
                self.cur.text.push('$');
                self.cur.text.push_str(name);
            }
        }
    }

    fn scope(&mut self, open: bool) {
        self.end_cmd(if open { "(" } else { ")" });
        if open {
            self.var_stack.push(self.vars.clone());
        } else if let Some(saved) = self.var_stack.pop() {
            self.vars = saved;
        }
        let cmd = self.last();
        cmd.open = open;
        cmd.close = !open;
        self.cmds.push(Cmd::default());
    }

    fn push(&mut self, ch: char) {
        self.cur.started = true;
        self.cur.text.push(ch);
    }

    fn mark_dynamic(&mut self, ch: char) {
        self.cur.dynamic = true;
        self.push(ch);
    }

    /// A word that is only digits, sitting right before `>` or `<`, names a
    /// file descriptor (`2>&1`), not an argument.
    fn drop_fd_prefix(&mut self) {
        let fd = self.cur.started
            && !self.cur.dynamic
            && !self.cur.text.is_empty()
            && self.cur.text.chars().all(|c| c.is_ascii_digit());
        if fd {
            self.cur = WordBuf::default();
        } else {
            self.flush();
        }
    }
}

/// `NAME` of a `NAME=value` or `NAME+=value` word.
fn assigned_name(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    let name = name.strip_suffix('+').unwrap_or(name);
    is_name(name).then_some(name)
}

fn is_name(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with(|c: char| c.is_ascii_digit())
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `$NAME` or `${NAME}` at `at`: the name and how many chars it spans.
fn var_ref(chars: &[char], at: usize) -> Option<(String, usize)> {
    let name_char = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
    let braced = chars.get(at + 1) == Some(&'{');
    let start = at + if braced { 2 } else { 1 };
    let name: String = chars[start.min(chars.len())..]
        .iter()
        .take_while(|c| name_char(c))
        .collect();
    if !is_name(&name) {
        return None;
    }
    if braced {
        (chars.get(start + name.len()) == Some(&'}')).then(|| (name.clone(), 3 + name.len()))
    } else {
        Some((name.clone(), 1 + name.len()))
    }
}

fn home_string() -> String {
    dirs::home_dir()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn lex(src: &str) -> Vec<Cmd> {
    let chars: Vec<char> = src.chars().collect();
    let mut lx = Lexer::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();
        let var = if ch == '$' { var_ref(&chars, i) } else { None };
        match ch {
            ' ' | '\t' | '\r' => lx.flush(),
            '\n' => {
                lx.end_cmd("\n");
                if !lx.heredocs.is_empty() {
                    i = read_heredoc_bodies(&chars, i + 1, &mut lx);
                    continue;
                }
            }
            ';' => lx.end_cmd(";"),
            '|' if next == Some('|') => {
                lx.end_cmd("||");
                i += 1;
            }
            '|' => lx.end_cmd("|"),
            '&' if next == Some('>') => {
                lx.flush();
                i += 2;
                if chars.get(i) == Some(&'>') {
                    i += 1;
                }
                lx.mode = Mode::Target;
                continue;
            }
            '&' if next == Some('&') => {
                lx.end_cmd("&&");
                i += 1;
            }
            '&' => lx.end_cmd("&"),
            '(' => lx.scope(true),
            ')' => lx.scope(false),
            '>' => {
                lx.drop_fd_prefix();
                i += 1;
                if chars.get(i) == Some(&'>') || chars.get(i) == Some(&'|') {
                    i += 1;
                }
                lx.mode = if chars.get(i) == Some(&'&') {
                    i += 1;
                    Mode::Ignore
                } else if chars.get(i) == Some(&'(') {
                    Mode::Normal
                } else {
                    Mode::Target
                };
                continue;
            }
            '<' => {
                lx.drop_fd_prefix();
                i += 1;
                if chars.get(i) == Some(&'<') {
                    i += 1;
                    if chars.get(i) == Some(&'<') {
                        i += 1;
                        lx.mode = Mode::Ignore;
                    } else if chars.get(i) == Some(&'-') {
                        i += 1;
                        lx.mode = Mode::Delimiter(true);
                    } else {
                        lx.mode = Mode::Delimiter(false);
                    }
                } else if chars.get(i) == Some(&'(') {
                    lx.mode = Mode::Normal;
                } else {
                    lx.mode = Mode::Ignore;
                }
                continue;
            }
            '#' if !lx.cur.started => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '\\' => match next {
                Some('\n') => i += 1,
                Some(other) => {
                    lx.push(other);
                    i += 1;
                }
                None => {}
            },
            '\'' => {
                lx.cur.started = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    lx.cur.text.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                lx.cur.started = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    match chars[i] {
                        '\\' if matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                            i += 1;
                            lx.cur.text.push(chars[i]);
                        }
                        '$' if var_ref(&chars, i).is_some() => {
                            let (name, len) = var_ref(&chars, i).expect("checked");
                            lx.expand(&name, true);
                            i += len;
                            continue;
                        }
                        '$' | '`' => {
                            lx.cur.dynamic = true;
                            lx.cur.text.push(chars[i]);
                        }
                        other => lx.cur.text.push(other),
                    }
                    i += 1;
                }
            }
            '`' => {
                lx.cur.dynamic = true;
                lx.cur.started = true;
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
            }
            '$' if next == Some('(') && chars.get(i + 2) == Some(&'(') => {
                // `$(( ... ))` is arithmetic; its `>` is a comparison, not a redirect.
                lx.mark_dynamic(ch);
                i += 3;
                while i < chars.len() && !(chars[i] == ')' && chars.get(i + 1) == Some(&')')) {
                    i += 1;
                }
                i += 1;
            }
            '$' if var.is_some() => {
                let (name, len) = var.expect("checked");
                lx.expand(&name, false);
                i += len;
                continue;
            }
            '$' | '*' | '?' | '[' | '{' | '}' => lx.mark_dynamic(ch),
            '~' if !lx.cur.started => {
                lx.cur.tilde = true;
                lx.push(ch);
            }
            other => lx.push(other),
        }
        i += 1;
    }
    lx.flush();
    lx.cmds
}

/// Read the bodies of the heredocs opened on the line that just ended, from
/// `start`. Each goes to the command that opened it. Returns the index of the
/// first char after the last delimiter line.
fn read_heredoc_bodies(chars: &[char], start: usize, lx: &mut Lexer) -> usize {
    let mut i = start;
    for (idx, delim, strip) in std::mem::take(&mut lx.heredocs) {
        let mut body = String::new();
        while i < chars.len() {
            let end = chars[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |p| i + p);
            let line: String = chars[i..end].iter().collect();
            i = (end + 1).min(chars.len());
            let check = if strip {
                line.trim_start_matches('\t')
            } else {
                line.as_str()
            };
            if check == delim {
                break;
            }
            body.push_str(&line);
            body.push('\n');
        }
        if let Some(cmd) = lx.cmds.get_mut(idx) {
            cmd.heredoc = Some(body);
        }
    }
    i
}

// ─── command reading ───

fn collect(src: &str, base: Option<PathBuf>, depth: usize, out: &mut Vec<PathBuf>) {
    let cmds = lex(src);
    let mut cwd = base;
    let mut stack: Vec<Option<PathBuf>> = Vec::new();
    for cmd in &cmds {
        if cmd.open {
            stack.push(cwd.clone());
            continue;
        }
        if cmd.close {
            if let Some(saved) = stack.pop() {
                cwd = saved;
            }
            continue;
        }
        for target in &cmd.redirects {
            add(&target.text, target.dynamic, cwd.as_deref(), out);
        }
        let Some(at) = command_index(&cmd.words) else {
            continue;
        };
        let name = base_name(&cmd.words[at].text);
        let args = &cmd.words[at + 1..];
        match name {
            "cd" => cwd = change_dir(args, cwd.as_deref()),
            "sed" => {
                for w in sed_files(args) {
                    add(&w.text, w.dynamic, cwd.as_deref(), out);
                }
            }
            "perl" => {
                for w in perl_files(args) {
                    add(&w.text, w.dynamic, cwd.as_deref(), out);
                }
            }
            "tee" => {
                for w in args.iter().filter(|w| !w.text.starts_with('-')) {
                    add(&w.text, w.dynamic, cwd.as_deref(), out);
                }
            }
            "sh" | "bash" | "zsh" if depth < MAX_SHELL_DEPTH => {
                if let Some(inner) = shell_command_string(args) {
                    collect(inner, cwd.clone(), depth + 1, out);
                }
            }
            n if is_python(n) => {
                let mut sources: Vec<&str> = Vec::new();
                if let Some(code) = flag_value(args, "-c") {
                    sources.push(code);
                }
                if let Some(body) = cmd.heredoc.as_deref() {
                    sources.push(body);
                }
                for source in sources {
                    for lit in python_writes(source) {
                        add(&lit, false, cwd.as_deref(), out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_python(name: &str) -> bool {
    name.strip_prefix("python")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Index of the word that names the command: past `FOO=bar`, shell keywords
/// and wrappers (`env`, `nice -n 10`, `uv run`).
fn command_index(words: &[Word]) -> Option<usize> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].text.as_str();
        match w {
            "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "time" | "exec"
            | "command" | "sudo" | "nohup" | "env" | "{" | "}" => i += 1,
            "nice" | "caffeinate" | "timeout" => {
                i += 1;
                while words.get(i).is_some_and(|n| {
                    n.text.starts_with('-') || n.text.chars().all(|c| c.is_ascii_digit())
                }) {
                    i += 1;
                }
            }
            "uv" | "poetry" | "pipenv" if words.get(i + 1).is_some_and(|n| n.text == "run") => {
                i += 2;
            }
            _ if is_assignment(w) => i += 1,
            _ => return Some(i),
        }
    }
    None
}

/// The word after `flag`.
fn flag_value<'a>(args: &'a [Word], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|w| w.text == flag)?;
    args.get(at + 1).map(|w| w.text.as_str())
}

/// The command string of `bash -c '...'`, also behind a cluster such as `-lc`.
fn shell_command_string(args: &[Word]) -> Option<&str> {
    let at = args.iter().position(|w| {
        let t = w.text.as_str();
        t.len() >= 2
            && t.starts_with('-')
            && !t.starts_with("--")
            && t.ends_with('c')
            && t.chars().skip(1).all(|c| c.is_ascii_alphabetic())
    })?;
    args.get(at + 1).map(|w| w.text.as_str())
}

fn change_dir(args: &[Word], cwd: Option<&Path>) -> Option<PathBuf> {
    let targets: Vec<&Word> = args.iter().filter(|w| w.text != "--").collect();
    let [target] = targets.as_slice() else {
        return None;
    };
    if target.dynamic || target.text.is_empty() || target.text == "-" {
        return None;
    }
    resolve(&target.text, cwd)
}

/// File operands of `sed -i`; empty when the call is not in place.
fn sed_files(args: &[Word]) -> Vec<&Word> {
    let mut in_place = false;
    let mut script_flag = false;
    let mut positional: Vec<&Word> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].text.as_str();
        i += 1;
        if a == "--" {
            positional.extend(args[i..].iter());
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            match long.split('=').next().unwrap_or("") {
                "in-place" => in_place = true,
                "expression" | "file" => {
                    script_flag = true;
                    if !long.contains('=') {
                        i += 1;
                    }
                }
                _ => {}
            }
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            for (at, flag) in a.char_indices().skip(1) {
                match flag {
                    'i' => {
                        in_place = true;
                        // BSD form: `-i ''` takes the empty suffix as its own word.
                        if at + 1 == a.len() && args.get(i).is_some_and(|n| n.text.is_empty()) {
                            i += 1;
                        }
                        break;
                    }
                    'e' | 'f' => {
                        script_flag = true;
                        if at + 1 == a.len() {
                            i += 1;
                        }
                        break;
                    }
                    _ => {}
                }
            }
            continue;
        }
        positional.push(&args[i - 1]);
    }
    if !in_place {
        return Vec::new();
    }
    if !script_flag && !positional.is_empty() {
        positional.remove(0);
    }
    positional
}

/// File operands of `perl -i` / `-pi`; empty when the call is not in place.
fn perl_files(args: &[Word]) -> Vec<&Word> {
    let mut in_place = false;
    let mut script_flag = false;
    let mut positional: Vec<&Word> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].text.as_str();
        i += 1;
        if a == "--" {
            positional.extend(args[i..].iter());
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for (at, flag) in a.char_indices().skip(1) {
                match flag {
                    'i' => {
                        in_place = true;
                        break;
                    }
                    'e' | 'E' => {
                        script_flag = true;
                        if at + 1 == a.len() {
                            i += 1;
                        }
                        break;
                    }
                    'I' | 'M' | 'm' => break,
                    _ => {}
                }
            }
            continue;
        }
        positional.push(&args[i - 1]);
    }
    if !in_place {
        return Vec::new();
    }
    if !script_flag && !positional.is_empty() {
        positional.remove(0);
    }
    positional
}

// ─── Python ───

static PY_OPEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\bopen\(\s*(?:'([^'\n]+)'|"([^"\n]+)"|([A-Za-z_]\w*))\s*,\s*(?:mode\s*=\s*)?['"]([rwaxbt+]{1,4})['"]"#,
    )
    .expect("static regex")
});
static PY_PATH_WRITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\bPath\(\s*(?:'([^'\n]+)'|"([^"\n]+)"|([A-Za-z_]\w*))\s*\)\s*\.\s*write_(?:text|bytes)\("#,
    )
    .expect("static regex")
});
static PY_NAME_WRITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-Za-z_]\w*)\s*\.\s*write_(?:text|bytes)\(").expect("static regex")
});
static PY_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)(?:^|;)[ \t]*([A-Za-z_]\w*)[ \t]*([-+*/|%@&^]?)=[ \t]*([^=;\n#][^;\n#]*)?")
        .expect("static regex")
});
static PY_LITERAL_RHS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:(?:pathlib\.)?Path\(\s*)?(?:'([^'\n]*)'|"([^"\n]*)")\s*\)?\s*$"#)
        .expect("static regex")
});
static PY_FOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bfor\s+([A-Za-z_]\w*)\s+in\s*(\[[^\]\n]*\]|\([^)\n]*\))?").expect("static regex")
});
static PY_STRING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"'([^'\n]*)'|"([^"\n]*)""#).expect("static regex"));

fn writes_mode(mode: &str) -> bool {
    mode.contains(['w', 'a', 'x', '+'])
}

/// A quoted `[slug]` is a real file name (Next.js routes); `$`, braces, `%`,
/// `*` and `?` may be expanded or formatted into something else, so they are skipped.
fn literal_path(text: &str) -> bool {
    !text.is_empty() && !text.contains(['$', '{', '}', '%', '*', '?', '\n', '\0'])
}

/// Literal paths a Python script writes.
fn python_writes(source: &str) -> Vec<String> {
    if !(source.contains("open(") || source.contains("write_")) {
        return Vec::new();
    }
    let mut literals: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut direct = |caps: &regex::Captures| {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            literals.push(m.as_str().to_string());
            None
        } else {
            caps.get(3).map(|m| m.as_str().to_string())
        }
    };
    for caps in PY_OPEN.captures_iter(source) {
        if writes_mode(&caps[4]) {
            names.extend(direct(&caps));
        }
    }
    for caps in PY_PATH_WRITE.captures_iter(source) {
        names.extend(direct(&caps));
    }
    for caps in PY_NAME_WRITE.captures_iter(source) {
        names.push(caps[1].to_string());
    }
    names.sort();
    names.dedup();
    for name in names {
        literals.extend(literal_values(source, &name));
    }
    literals.retain(|l| literal_path(l));
    literals
}

/// The literals a name can hold, when every assignment and loop over it is a
/// plain literal; empty when any one is computed, since the written value is
/// then unknown.
fn literal_values(source: &str, name: &str) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for caps in PY_ASSIGN.captures_iter(source) {
        if &caps[1] != name {
            continue;
        }
        let rhs = caps.get(3).map_or("", |m| m.as_str().trim());
        let literal = caps[2]
            .is_empty()
            .then(|| PY_LITERAL_RHS.captures(rhs))
            .flatten();
        match literal.and_then(|c| c.get(1).or_else(|| c.get(2))) {
            Some(m) => values.push(m.as_str().to_string()),
            None => return Vec::new(),
        }
    }
    for caps in PY_FOR.captures_iter(source) {
        if &caps[1] != name {
            continue;
        }
        let Some(list) = caps.get(2) else {
            return Vec::new();
        };
        let inner = &list.as_str()[1..list.as_str().len() - 1];
        let rest = PY_STRING.replace_all(inner, "");
        if !rest.chars().all(|c| c == ',' || c.is_whitespace()) {
            return Vec::new();
        }
        for s in PY_STRING.captures_iter(inner) {
            if let Some(m) = s.get(1).or_else(|| s.get(2)) {
                values.push(m.as_str().to_string());
            }
        }
    }
    values
}

// ─── paths ───

fn add(text: &str, dynamic: bool, cwd: Option<&Path>, out: &mut Vec<PathBuf>) {
    if dynamic || !literal_path(text) {
        return;
    }
    let Some(path) = resolve(text, cwd) else {
        return;
    };
    if path.starts_with("/dev") || path.is_dir() || out.contains(&path) {
        return;
    }
    out.push(path);
}

/// `text` against `cwd`, with `.` and `..` folded away and no file access.
fn resolve(text: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let raw = Path::new(text);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd?.join(raw)
    };
    let mut path = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                path.pop();
            }
            Component::CurDir => {}
            other => path.push(other),
        }
    }
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writes(cmd: &str) -> Vec<String> {
        bash_writes(cmd, Path::new("/w/repo"))
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    fn none(cmd: &str) {
        assert_eq!(writes(cmd), Vec::<String>::new(), "{cmd}");
    }

    #[test]
    fn sed_in_place_forms_name_the_file_operands() {
        assert_eq!(writes("sed -i 's/a/b/' src/x.rs"), ["/w/repo/src/x.rs"]);
        assert_eq!(writes("sed -i '' 's/a/b/' src/x.rs"), ["/w/repo/src/x.rs"]);
        assert_eq!(writes("sed -i.bak 's/a/b/' x.rs"), ["/w/repo/x.rs"]);
        assert_eq!(writes("sed --in-place 's/a/b/' x.rs"), ["/w/repo/x.rs"]);
        assert_eq!(
            writes("sed --in-place=.bak -e 's/a/b/' x.rs"),
            ["/w/repo/x.rs"]
        );
        assert_eq!(
            writes("sed -E -i '' -e 's/a/b/' -e 's/c/d/' a.rs b.rs"),
            ["/w/repo/a.rs", "/w/repo/b.rs"]
        );
        assert_eq!(writes("sed -ni 's/a/b/p' x.rs"), ["/w/repo/x.rs"]);
        assert_eq!(writes("sed -i -f script.sed x.rs"), ["/w/repo/x.rs"]);
    }

    #[test]
    fn sed_without_in_place_writes_nothing() {
        none("sed 's/a/b/' x.rs");
        none("sed -n '1,5p' x.rs");
        none("sed -e 's/a/b/' x.rs | head");
    }

    #[test]
    fn perl_in_place_forms_name_the_file_operands() {
        assert_eq!(writes("perl -pi -e 's/a/b/' x.pl"), ["/w/repo/x.pl"]);
        assert_eq!(
            writes("perl -pi.bak -e 's/a/b/' x.pl y.pl"),
            ["/w/repo/x.pl", "/w/repo/y.pl"]
        );
        assert_eq!(writes("perl -i -pe 's/a/b/' x.pl"), ["/w/repo/x.pl"]);
        assert_eq!(writes("perl -0777 -pi -e 's/a/b/s' x.pl"), ["/w/repo/x.pl"]);
        none("perl -pe 's/a/b/' x.pl");
        none("perl script.pl x.pl");
    }

    #[test]
    fn redirects_and_tee_name_their_target() {
        assert_eq!(writes("echo hi > out.txt"), ["/w/repo/out.txt"]);
        assert_eq!(writes("echo hi >> log/out.txt"), ["/w/repo/log/out.txt"]);
        assert_eq!(writes("echo hi >out.txt"), ["/w/repo/out.txt"]);
        assert_eq!(writes("cmd &> all.log"), ["/w/repo/all.log"]);
        assert_eq!(writes("cmd 2> err.log"), ["/w/repo/err.log"]);
        assert_eq!(writes("echo hi | tee out.txt"), ["/w/repo/out.txt"]);
        assert_eq!(
            writes("echo hi | tee -a a.txt b.txt"),
            ["/w/repo/a.txt", "/w/repo/b.txt"]
        );
        assert_eq!(writes("echo hi > '/abs/my file.txt'"), ["/abs/my file.txt"]);
    }

    #[test]
    fn heredoc_to_a_file_names_the_file_and_not_the_body() {
        let cmd =
            "cat > src/new.rs <<'EOF'\nfn a() {}\necho x > not-a-write.txt\nsed -i y z\nEOF\n";
        assert_eq!(writes(cmd), ["/w/repo/src/new.rs"]);
        let two = "cat >> notes.md <<EOF\nbody\nEOF\necho done > done.txt\n";
        assert_eq!(writes(two), ["/w/repo/notes.md", "/w/repo/done.txt"]);
        let tabs = "cat > a.txt <<-EOF\n\tbody\n\tEOF\n";
        assert_eq!(writes(tabs), ["/w/repo/a.txt"]);
    }

    #[test]
    fn redirects_that_are_not_file_writes_are_skipped() {
        none("make 2>&1 | tail");
        none("cmd > /dev/null 2>&1");
        none("cmd 1>&2");
        none("echo 'a > b'");
        none("echo \"a > b\"");
        none("cat < in.txt");
        none("grep x <<< \"a b\"");
        none("echo hi >&2");
    }

    #[test]
    fn dynamic_targets_are_skipped_never_guessed() {
        none("echo hi > \"$OUT\"");
        none("echo hi > $HOME/x.txt");
        none("echo hi > out/*.txt");
        none("echo hi > out/?.txt");
        none("echo hi > out/{a,b}.txt");
        none("echo hi > `mktemp`");
        none("echo hi > $(mktemp)");
        none("sed -i 's/a/b/' \"$f\"");
        none("sed -i 's/a/b/' src/*.rs");
        none("for f in a b; do sed -i 's/x/y/' $f; done");
        none("echo hi | tee \"$LOG\"");
    }

    #[test]
    fn a_directory_is_not_a_write_target() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = format!("echo hi > {}", dir.path().display());
        assert!(bash_writes(&cmd, Path::new("/")).is_empty());
        let file = dir.path().join("f.txt");
        let cmd = format!("echo hi > {}", file.display());
        assert_eq!(bash_writes(&cmd, Path::new("/")), [file]);
    }

    #[test]
    fn python_open_for_write_names_the_literal_path() {
        let cmd = "python3 -c \"open('a/x.txt', 'w').write('hi')\"";
        assert_eq!(writes(cmd), ["/w/repo/a/x.txt"]);
        assert_eq!(
            writes("python -c 'open(\"x.txt\",\"a\").write(1)'"),
            ["/w/repo/x.txt"]
        );
        assert_eq!(
            writes("python3.12 -c \"open('x.bin','wb').write(b'')\""),
            ["/w/repo/x.bin"]
        );
        assert_eq!(
            writes("python3 -c \"open('x.txt', mode='w+')\""),
            ["/w/repo/x.txt"]
        );
        none("python3 -c \"print(open('x.txt').read())\"");
        none("python3 -c \"open('x.txt', 'r').read()\"");
        none("python3 -c \"open('x.txt', 'rb').read()\"");
    }

    #[test]
    fn python_path_write_text_names_the_literal_path() {
        let cmd = "python3 -c \"from pathlib import Path; Path('d/y.md').write_text('x')\"";
        assert_eq!(writes(cmd), ["/w/repo/d/y.md"]);
        let bytes = "python3 -c \"import pathlib; pathlib.Path('y.bin').write_bytes(b'x')\"";
        assert_eq!(writes(bytes), ["/w/repo/y.bin"]);
    }

    #[test]
    fn python_heredoc_body_is_read_as_python() {
        let cmd = "python3 - <<'PY'\nimport json\nwith open('data/out.json', 'w') as f:\n    json.dump({}, f)\nPY\n";
        assert_eq!(writes(cmd), ["/w/repo/data/out.json"]);
        let plain = "python3 <<EOF\nopen('a.txt','w').write('x')\nEOF\n";
        assert_eq!(writes(plain), ["/w/repo/a.txt"]);
    }

    #[test]
    fn python_read_replace_write_through_a_literal_name() {
        let cmd = "python3 - <<'PY'\np = 'bin/pane.py'\ns = open(p).read()\ns = s.replace('a', 'b')\nopen(p, 'w').write(s)\nPY\n";
        assert_eq!(writes(cmd), ["/w/repo/bin/pane.py"]);
        let inline = "python3 -c \"p='bin/pane.py'; s=open(p).read(); open(p,'w').write(s)\"";
        assert_eq!(writes(inline), ["/w/repo/bin/pane.py"]);
        let path = "python3 -c \"from pathlib import Path\np = Path('a.md')\np.write_text(p.read_text())\"";
        assert_eq!(writes(path), ["/w/repo/a.md"]);
        let looped =
            "python3 - <<'PY'\nfor p in ['a.txt', \"b.txt\"]:\n    open(p, 'w').write('x')\nPY\n";
        assert_eq!(writes(looped), ["/w/repo/a.txt", "/w/repo/b.txt"]);
    }

    #[test]
    fn python_computed_paths_are_skipped() {
        none("python3 -c \"open(os.path.join(d, 'x'), 'w')\"");
        none("python3 -c \"open(f'out/{n}.txt', 'w')\"");
        none("python3 -c \"open('out/%s.txt' % n, 'w')\"");
        none("python3 -c \"p = base + '/x.txt'; open(p, 'w')\"");
        none("python3 -c \"p = 'a.txt'; p = p + '.bak'; open(p, 'w')\"");
        none("python3 -c \"for p in files: open(p, 'w')\"");
        none("python3 -c \"for p in ['a', other]: open(p, 'w')\"");
        none("python3 -c \"open('$HOME/x.txt', 'w')\"");
        none("python3 -c \"open('out/*.txt', 'w')\"");
        none("python3 -c \"p = sys.argv[1]; open(p, 'w')\"");
    }

    #[test]
    fn a_python_script_file_is_not_a_write() {
        none("python3 script.py");
        none("python3 -m pytest tests/");
        none("echo \"python3 -c 'open(\\\"x\\\", \\\"w\\\")'\"");
    }

    #[test]
    fn a_literal_variable_carries_into_cd_and_write_targets() {
        assert_eq!(
            writes("W=/abs/p; cd $W && echo a > x.txt"),
            ["/abs/p/x.txt"]
        );
        assert_eq!(
            writes("W=/abs/p && cd \"$W\" && echo a > x.txt"),
            ["/abs/p/x.txt"]
        );
        assert_eq!(
            writes("W=/abs/p; cd ${W}/sub; echo a > x.txt"),
            ["/abs/p/sub/x.txt"]
        );
        assert_eq!(
            writes("D='/abs/my dir'; echo a > \"$D/x.txt\""),
            ["/abs/my dir/x.txt"]
        );
        assert_eq!(writes("F=out.txt\necho a > $F"), ["/w/repo/out.txt"]);
        assert_eq!(
            writes("W=/abs/p\ncd $W\nsed -i '' s/a/b/ f.rs"),
            ["/abs/p/f.rs"]
        );
        assert_eq!(writes("A=/abs; B=\"$A/q\"; echo a > $B/x"), ["/abs/q/x"]);
        assert_eq!(writes("export W=/abs/p; cd $W; echo a > x"), ["/abs/p/x"]);
        assert_eq!(writes("W=/a; W=/b; echo a > $W/x"), ["/b/x"]);
        assert_eq!(writes("W=/a/b.txt; tee $W < in"), ["/a/b.txt"]);
        // A relative value resolves where it is used.
        assert_eq!(writes("W=sub; cd $W && echo a > x"), ["/w/repo/sub/x"]);
    }

    #[test]
    fn a_variable_the_parser_cannot_know_is_skipped() {
        // A prefix assignment scopes to its own command.
        none("W=x echo hi > $W");
        none("W=/abs/p cd /tmp; echo a > $W/x");
        none("W=/abs/p echo; cd $W && echo a > x");
        // Unset and unknown names.
        none("echo a > $W");
        none("cd $W && echo a > x");
        none("W=/a; echo a > $OTHER/x");
        // Values from substitutions, globs and tilde.
        none("W=$(pwd); cd $W && echo a > x");
        none("W=`pwd`; cd $W && echo a > x");
        none("W=/abs/*; echo a > $W");
        none("W=~/p; echo a > $W/x");
        none("W=\"$X/p\"; echo a > $W/x");
        none("W=; echo a > $W/x");
        // Other expansions of a known name.
        none("W=/abs; echo a > ${W:-/z}/x");
        none("W=/abs; echo a > ${W%/}/x");
        none("W=/abs; echo a > ${#W}");
        // Loops: the loop variable, assignments in a body, in a branch.
        none("for W in a b; do cd /abs/$W && echo a > x; done");
        none("W=/ok; for W in a b; do echo a > $W/x; done");
        none("for i in 1 2; do W=/abs/p; echo a > $W/x; done");
        none("W=/old; for i in 1; do W=/new; done; echo a > $W/x");
        none("if true; then W=/abs/p; fi; echo a > $W/x");
        none("while false; do W=/abs/p; done; cd $W; echo a > x");
        // Pipelines, background, `||` and `&&` tails do not carry.
        none("W=/abs/p | cat; echo a > $W/x");
        none("W=/abs/p & echo a > $W/x");
        none("false || W=/abs/p; echo a > $W/x");
        none("true && W=/abs/p; echo a > $W/x");
        none("(W=/abs/p); echo a > $W/x");
        // Reassignment from something unknown, and names the shell may set.
        none("W=/a; W=$X; echo a > $W/x");
        none("W=/a; W+=/b; echo a > $W/x");
        none("W=/a; read W; echo a > $W/x");
        none("W=/a; unset W; echo a > $W/x");
        none("W=/a; eval \"$X\"; echo a > $W/x");
        none("W=/a; export W=$(pwd); echo a > $W/x");
        none("W=/a; declare W=$X; echo a > $W/x");
        // A variable in a single-quoted word is text, never a path.
        none("W=/abs; echo a > '$W/x'");
    }

    #[test]
    fn a_variable_set_inside_a_subshell_does_not_leak_out() {
        assert_eq!(
            writes("(W=/in; echo a > $W/x); echo b > y"),
            ["/in/x", "/w/repo/y"]
        );
        // `$W/y` is unknown after the subshell, so only the inner write counts.
        assert_eq!(writes("(W=/in; echo a > $W/x); echo b > $W/y"), ["/in/x"]);
        assert_eq!(
            writes("W=/out; (W=/in; echo a > $W/x); echo b > $W/y"),
            ["/in/x", "/out/y"]
        );
    }

    #[test]
    fn a_variable_reaches_a_shell_c_string_only_through_double_quotes() {
        assert_eq!(writes("W=/abs/p; bash -c \"echo a > $W/x\""), ["/abs/p/x"]);
        none("W=/abs/p; bash -c 'echo a > $W/x'");
    }

    #[test]
    fn cd_moves_the_base_for_later_commands() {
        assert_eq!(
            writes("cd sub && echo hi > out.txt"),
            ["/w/repo/sub/out.txt"]
        );
        assert_eq!(
            writes("cd /abs/dir; sed -i 's/a/b/' x.rs"),
            ["/abs/dir/x.rs"]
        );
        assert_eq!(writes("cd .. && echo hi > out.txt"), ["/w/out.txt"]);
        assert_eq!(
            writes("(cd sub && echo a > in.txt); echo b > out.txt"),
            ["/w/repo/sub/in.txt", "/w/repo/out.txt"]
        );
        none("cd \"$X\" && echo hi > out.txt");
        none("cd - && echo hi > out.txt");
        assert_eq!(
            writes("cd \"$X\" && echo hi > /abs/out.txt"),
            ["/abs/out.txt"]
        );
    }

    #[test]
    fn an_empty_cwd_leaves_relative_paths_unresolved() {
        assert!(bash_writes("echo hi > out.txt", Path::new("")).is_empty());
        let abs = bash_writes("echo hi > /abs/out.txt", Path::new(""));
        assert_eq!(abs, [PathBuf::from("/abs/out.txt")]);
    }

    #[test]
    fn a_tilde_path_expands_to_the_home_folder() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            bash_writes("echo hi > ~/notes.txt", Path::new("/w")),
            [home.join("notes.txt")]
        );
        none("echo hi > ~other/notes.txt");
    }

    #[test]
    fn a_quoted_bracket_path_is_a_literal_and_an_unquoted_one_is_a_glob() {
        assert_eq!(
            writes("echo hi > 'app/[slug]/page.tsx'"),
            ["/w/repo/app/[slug]/page.tsx"]
        );
        none("echo hi > app/[slug]/page.tsx");
    }

    #[test]
    fn a_shell_c_string_is_parsed_again() {
        assert_eq!(writes("bash -c 'echo hi > out.txt'"), ["/w/repo/out.txt"]);
        assert_eq!(writes("sh -c \"sed -i 's/a/b/' x.rs\""), ["/w/repo/x.rs"]);
    }

    #[test]
    fn wrappers_and_assignments_before_the_command_are_skipped() {
        assert_eq!(writes("FOO=1 sed -i 's/a/b/' x.rs"), ["/w/repo/x.rs"]);
        assert_eq!(writes("env FOO=1 sed -i 's/a/b/' x.rs"), ["/w/repo/x.rs"]);
        assert_eq!(
            writes("uv run python3 -c \"open('a.txt','w')\""),
            ["/w/repo/a.txt"]
        );
        assert_eq!(writes("nice -n 10 sed -i 's/a/b/' x.rs"), ["/w/repo/x.rs"]);
        assert_eq!(
            writes("if true; then sed -i 's/a/b/' x.rs; fi"),
            ["/w/repo/x.rs"]
        );
    }

    #[test]
    fn several_commands_and_repeats_give_each_file_once() {
        let cmd = "sed -i 's/a/b/' x.rs && echo y > x.rs; echo z >> y.rs";
        assert_eq!(writes(cmd), ["/w/repo/x.rs", "/w/repo/y.rs"]);
    }

    #[test]
    fn commands_that_write_nothing_give_nothing() {
        none("git status");
        none("ls -la src");
        none("cargo test --release 2>&1 | tail -20");
        none("grep -rn 'fn main' src/ | head");
        none("echo $((1 > 2))");
        none("cat src/x.rs");
        none("# comment > not-a-file.txt");
        none("");
    }

    #[test]
    fn an_unterminated_quote_or_heredoc_does_not_panic() {
        let _ = writes("echo 'unterminated > x.txt");
        let _ = writes("echo \"unterminated > x.txt");
        let _ = writes("cat > a.txt <<EOF\nno end");
        let _ = writes("cat <<");
        let _ = writes("sed -i");
        let _ = writes("perl -pi");
        let _ = writes(">");
    }
}
