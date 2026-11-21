//! The shell, as the push guard reads it: port of the shell half of the removed `coach.ts`
//! (heredocs, substitutions, the word splitter, wrappers that run the command after them).
//!
//! This is a safety parser, so it is a literal port: where the TypeScript refuses to guess, so does
//! this. JavaScript's `\s` and `\w` are not the regex crate's (`\s` has U+FEFF and not U+0085; `\w`
//! is ASCII), and the crate has no lookbehind: each is written out by hand here, and the edges are
//! tested at the bottom.

/// A character JavaScript's `\s` matches (and `String.prototype.trim` strips).
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// JavaScript's `\w`: ASCII letters, digits and `_`.
pub fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `text.trim()`.
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `text.split(/\s+/)`: a leading or trailing run of whitespace leaves an empty piece, as in
/// JavaScript.
pub fn split_ws(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut inside = false;
    for (at, c) in text.char_indices() {
        if is_space(c) {
            if !inside {
                out.push(&text[start..at]);
                inside = true;
            }
        } else if inside {
            start = at;
            inside = false;
        }
    }
    out.push(if inside { "" } else { &text[start..] });
    out
}

/// `/(?:^|\s)WORD(?:\s|$)/.test(text)`.
pub fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back().is_none_or(is_space);
        let after = text[at + word.len()..].chars().next().is_none_or(is_space);
        before && after
    })
}

/// What follows the last `/` of `text` (all of it when there is none): `text.split('/').pop()`.
pub fn last_segment(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or("")
}

fn chars_at(line: &[char], at: usize, text: &str) -> bool {
    text.chars()
        .enumerate()
        .all(|(k, c)| line.get(at + k) == Some(&c))
}

fn index_of(line: &[char], text: &str, from: usize) -> Option<usize> {
    (from..line.len()).find(|&at| chars_at(line, at, text))
}

fn text_of(chars: &[char]) -> String {
    chars.iter().collect()
}

// ---------------------------------------------------------------- wrappers and where a command starts

const NONE: &[&str] = &[];

/// Words that run the command after them, each with its options that take the next word as their
/// value, short and long (the removed `coach.ts` `LEADERS`).
fn leader(word: &str) -> Option<&'static [&'static str]> {
    Some(match word {
        "if" | "then" | "else" | "elif" | "while" | "until" | "do" | "!" | "time" | "command"
        | "nohup" | "builtin" => NONE,
        "sudo" => &[
            "-u",
            "-g",
            "-p",
            "-C",
            "-U",
            "-r",
            "-t",
            "-D",
            "-R",
            "-T",
            "--user",
            "--group",
            "--prompt",
            "--close-from",
            "--other-user",
            "--role",
            "--type",
            "--chdir",
            "--chroot",
            "--command-timeout",
        ],
        "env" => &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"],
        "exec" => &["-a"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "nice" => &["-n", "--adjustment"],
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        "xargs" => &[
            "-I",
            "-n",
            "-P",
            "-L",
            "-s",
            "-E",
            "-d",
            "-a",
            "--max-args",
            "--max-procs",
            "--max-lines",
            "--delimiter",
            "--arg-file",
        ],
        _ => return None,
    })
}

/// `timeout [options] DURATION command`: the words a leader takes after its options.
fn leader_then_one(word: &str) -> bool {
    word == "timeout"
}

/// getopt takes a long option by any prefix (`timeout --sig TERM 60`).
fn leader_takes(takes: &[&str], word: &str) -> bool {
    let long = word
        .strip_prefix("--")
        .is_some_and(|rest| rest.chars().count() >= 2 && !rest.contains('='));
    takes.contains(&word) || (long && takes.iter().any(|option| option.starts_with(word)))
}

/// `/^\w+=/`.
fn starts_assignment(word: &str) -> bool {
    let name = word.chars().take_while(|&c| is_word(c)).count();
    name > 0 && word[name..].starts_with('=')
}

/// Where a command's own words start: past `if`, `sudo -u x`, `env -i A=1`, `nohup` and the like
/// (the removed `coach.ts` `headAt`). May be past the end.
pub fn head_at<S: AsRef<str>>(words: &[S]) -> usize {
    let mut i = 0;
    let mut takes: Option<&[&str]> = None; // the options of the last leader, once there is one
    let mut owed = 0; // words a leader takes after its options: timeout's duration
    while i < words.len() {
        let w = words[i].as_ref();
        let found = if owed > 0 { None } else { leader(w) };
        if let Some(options) = found {
            takes = Some(options);
            owed = usize::from(leader_then_one(w));
            i += 1;
        } else if owed == 0 && starts_assignment(w) {
            i += 1;
        } else if let (Some(options), true) = (takes, w.chars().count() > 1 && w.starts_with('-')) {
            i += if leader_takes(options, w) { 2 } else { 1 };
        } else if owed > 0 {
            owed -= 1;
            i += 1;
        } else {
            break;
        }
    }
    i
}

pub fn is_shell(word: &str) -> bool {
    matches!(last_segment(word), "bash" | "sh" | "zsh" | "dash" | "ksh")
}

/// The script a shell is told to run: `bash -c "..."`, `sh -ce '...'`, `zsh -lc ...`, or eval's
/// words; only when the command itself is the shell, never a shell's name inside another command's
/// words (`echo sh -c ...`).
pub fn inner(words: &[String]) -> Vec<String> {
    let i = head_at(words);
    let word = |k: usize| words.get(k).map_or("", String::as_str);
    if word(i) == "eval" {
        return if words.len() > i + 1 {
            vec![words[i + 1..].join(" ")]
        } else {
            Vec::new()
        };
    }
    if !is_shell(word(i)) {
        return Vec::new();
    }
    let mut k = i + 1;
    while k < words.len() {
        let w = words[k].as_str();
        if w == "-o" || w == "+o" {
            k += 1; // bash -o pipefail -c ...
        } else if !w.starts_with('-') {
            return Vec::new();
        } else if clustered_c(w) {
            return match words.get(k + 1) {
                Some(script) if !script.is_empty() => vec![script.clone()],
                _ => Vec::new(),
            };
        }
        k += 1;
    }
    Vec::new()
}

/// `/^-[a-zA-Z]*c[a-zA-Z]*$/`.
fn clustered_c(word: &str) -> bool {
    word.strip_prefix('-')
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_alphabetic()) && rest.contains('c'))
}

// ---------------------------------------------------------------- heredocs

/// A heredoc a line declares: its word, whether any quote or backslash is in it, where its `<<`
/// stands on the line.
#[derive(Debug, PartialEq)]
pub struct HeredocWord {
    pub word: String,
    pub quoted: bool,
    pub at: usize,
}

/// The heredoc words one line declares, in order, as the shell reads the line: every `<<WORD` and
/// `<<-WORD` outside quotes, a comment and arithmetic (`$((1<<x))`, a `((1<<2))` command or
/// `for ((...))`, `$[1<<2]`: shifts); `<<<` is a here-string. A word with any quote or backslash in
/// it is quoted: its body is text, `$(...)` included.
fn heredoc_words(line: &[char]) -> Vec<HeredocWord> {
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                i += 1;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '\\' {
            i += 1;
        } else if c == '#' && (i == 0 || is_space(line[i - 1])) {
            break;
        } else if chars_at(line, i, "$((") || chars_at(line, i, "((") {
            i = index_of(line, "))", i + 2).map_or(line.len(), |end| end + 1);
        } else if chars_at(line, i, "$[") {
            i = index_of(line, "]", i + 2).unwrap_or(line.len());
        } else if chars_at(line, i, "<<<") {
            i += 2;
        } else if chars_at(line, i, "<<") {
            let mut j = i + 2 + usize::from(line.get(i + 2) == Some(&'-'));
            while matches!(line.get(j), Some(' ' | '\t')) {
                j += 1;
            }
            let mut word = String::new();
            let mut quoted = false;
            let mut inside: Option<char> = None;
            while j < line.len() {
                let d = line[j];
                if let Some(q) = inside {
                    if d == q {
                        inside = None;
                    } else {
                        word.push(d);
                    }
                } else if d == '\'' || d == '"' {
                    inside = Some(d);
                    quoted = true;
                } else if d == '\\' {
                    quoted = true;
                    j += 1;
                    if let Some(&next) = line.get(j) {
                        word.push(next);
                    }
                } else if is_space(d) || ";&|<>()".contains(d) {
                    break;
                } else {
                    word.push(d);
                }
                j += 1;
            }
            if !word.is_empty() {
                out.push(HeredocWord {
                    word,
                    quoted,
                    at: i,
                });
            }
            i = j - 1;
        }
        i += 1;
    }
    out
}

/// A heredoc's body, with whether its word was quoted and whether a shell on its line reads it.
#[derive(Debug, PartialEq)]
pub struct Heredoc {
    pub body: String,
    pub quoted: bool,
    pub shell: bool,
}

/// `/(?<!\\)(?:\\\\)*\\$/`: the line ends in an odd run of backslashes (the lookbehind makes the
/// run start where the backslashes start).
pub fn continued(line: &str) -> bool {
    line.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1
}

/// A shell line's heredocs: the source with each body cut (the lines after the line that declares
/// it, to the line that is its word), and each body with whether its word was quoted and whether a
/// shell on that line reads it (`bash <<EOF`, `cat <<'EOF' | sh`: a script, whatever the quotes).
/// Every word a line declares, its bodies one after another, cut one line after another. A line
/// continued by a backslash at its end is one logical line with the next. A body is a script only
/// when a shell is in its own pipeline, never for a shell elsewhere on the line.
pub fn heredocs(src: &str) -> (String, Vec<Heredoc>) {
    let lines: Vec<&str> = src.split('\n').collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut bodies = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut logical = lines[i].to_string();
        kept.push(lines[i]);
        while continued(&logical) && i + 1 < lines.len() {
            logical.pop();
            i += 1;
            logical.push_str(lines[i]);
            kept.push(lines[i]);
        }
        let chars: Vec<char> = logical.chars().collect();
        for HeredocWord { word, quoted, at } in heredoc_words(&chars) {
            let pipeline = pipeline_of(&chars, at);
            let shell = split(&pipeline)
                .iter()
                .any(|words| is_shell(words.get(head_at(words)).map_or("", String::as_str)));
            let mut body: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && trim(lines[i]) != word {
                body.push(lines[i]);
                i += 1;
            }
            bodies.push(Heredoc {
                body: body.join("\n"),
                quoted,
                shell,
            });
        }
        i += 1;
    }
    (kept.join("\n"), bodies)
}

/// The pipeline of a line that holds position `at`: the line cut at `;` `&&` `||` and a lone `&`
/// outside quotes (a pipe `|` keeps a pipeline whole; `>&`, `&>` and `|&` are redirections or
/// pipes).
fn pipeline_of(line: &[char], at: usize) -> String {
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < line.len() {
        let c = line[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '\\' {
            i += 1;
        } else {
            let doubled = (c == '&' || c == '|') && line.get(i + 1) == Some(&c);
            let before = if i == 0 { None } else { line.get(i - 1) };
            let width = if doubled {
                2
            } else if c == ';'
                || (c == '&'
                    && !before.is_some_and(|b| matches!(b, '<' | '>' | '|'))
                    && line.get(i + 1) != Some(&'>'))
            {
                1
            } else {
                0
            };
            if width == 0 {
                i += 1;
                continue;
            }
            if i >= at {
                return text_of(&line[start..i]);
            }
            start = i + width;
            i += width - 1;
        }
        i += 1;
    }
    text_of(line.get(start..).unwrap_or(&[]))
}

// ---------------------------------------------------------------- substitutions

/// Where the `)` that closes a `$(` at `from` is: nested parentheses and quoted ones counted as the
/// shell counts them.
fn closing(text: &[char], from: usize) -> usize {
    let mut depth = 1;
    let mut quote: Option<char> = None;
    let mut i = from;
    while i < text.len() {
        let c = text[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                i += 1;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '\\' {
            i += 1;
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    text.len()
}

/// What `$(...)` and backticks run, read as the shell reads them: never inside single quotes,
/// where they are text (inside double quotes they still run). In a heredoc's body quotes are text
/// too (`quoting` false).
pub fn substitutions(text: &str, quoting: bool) -> Vec<String> {
    let text: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            }
        } else if c == '\\' {
            i += 1;
        } else if quoting && c == '\'' && quote.is_none() {
            quote = Some('\'');
        } else if quoting && c == '"' {
            quote = if quote.is_some() { None } else { Some('"') };
        } else if c == '$' && text.get(i + 1) == Some(&'(') && text.get(i + 2) != Some(&'(') {
            // $(( )) is arithmetic
            let end = closing(&text, i + 2);
            out.push(text_of(&text[(i + 2).min(end)..end]));
            i = end;
        } else if c == '`' {
            let end = index_of(&text, "`", i + 1);
            out.push(text_of(&text[i + 1..end.unwrap_or(text.len())]));
            i = end.unwrap_or(text.len());
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------- words

/// A word of a command, with whether the shell expands something in it (`$NAME`, `${...}`,
/// `$(...)`, a backtick) outside single quotes: a value the check cannot know.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub expands: bool,
}

/// The simple commands of a shell line, each as its words with quotes taken off: split at `;` `&`
/// `|` `(` `)` `{` `}` backticks and newlines outside quotes. Best effort, not a shell: enough to
/// tell `git push` as a command from the words "git push" inside an echo, a quoted message or a
/// heredoc.
#[cfg(test)]
pub fn commands(source: &str) -> Vec<Vec<String>> {
    split(&heredocs(source).0)
}

/// The words of each simple command in shell text with no heredoc bodies in it.
pub fn split(src: &str) -> Vec<Vec<String>> {
    split_words(src)
        .into_iter()
        .map(|words| words.into_iter().map(|word| word.text).collect())
        .collect()
}

pub fn split_words(src: &str) -> Vec<Vec<Word>> {
    #[derive(Default)]
    struct Reading {
        out: Vec<Vec<Word>>,
        words: Vec<Word>,
        word: String,
        has: bool,
        expands: bool,
    }
    impl Reading {
        fn end_word(&mut self) {
            if self.has {
                self.words.push(Word {
                    text: std::mem::take(&mut self.word),
                    expands: self.expands,
                });
            }
            self.word.clear();
            self.has = false;
            self.expands = false;
        }
        fn end_command(&mut self) {
            self.end_word();
            if !self.words.is_empty() {
                self.out.push(std::mem::take(&mut self.words));
            }
        }
    }
    let src: Vec<char> = src.chars().collect();
    let mut r = Reading::default();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' && i + 1 < src.len() {
                i += 1;
                r.word.push(src[i]);
            } else {
                if q == '"' && (c == '$' || c == '`') {
                    r.expands = true;
                }
                r.word.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            r.has = true;
        } else if c == '\\' {
            if src.get(i + 1) == Some(&'\n') {
                i += 1; // a line continued: the two are one
            } else if i + 1 < src.len() {
                i += 1;
                r.word.push(src[i]);
                r.has = true;
            }
        } else if c == '#' && !r.has {
            while i + 1 < src.len() && src[i + 1] != '\n' {
                i += 1;
            }
        } else if c == ' ' || c == '\t' {
            r.end_word();
        } else if ";&|\n()`{}".contains(c) {
            // `X=$(cmd)`, X=`cmd`: X is unknown
            if r.has && (c == '`' || (c == '(' && r.word.ends_with('$'))) {
                r.expands = true;
            }
            r.end_command();
        } else {
            if c == '$' {
                r.expands = true;
            }
            r.word.push(c);
            r.has = true;
        }
        i += 1;
    }
    r.end_command();
    r.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_line_is_continued_only_by_an_odd_run_of_backslashes() {
        // /(?<!\\)(?:\\\\)*\\$/ has no lookbehind in the regex crate: the run is counted.
        assert!(continued("cat <<1 \\"));
        assert!(continued("\\"));
        assert!(continued("a\\\\\\"));
        assert!(!continued("a\\\\"));
        assert!(!continued("a\\ "));
        assert!(!continued(""));
        assert!(!continued("plain"));
        assert!(!continued("\\\\"));
    }

    #[test]
    fn js_whitespace_is_not_the_regex_crates() {
        assert!(is_space('\u{feff}'));
        assert!(!is_space('\u{85}'));
        assert!(is_space('\u{a0}'));
        assert_eq!(trim("\u{feff} a b \u{a0}"), "a b");
        assert_eq!(split_ws(" a  b "), ["", "a", "b", ""]);
        assert_eq!(split_ws(""), [""]);
        assert_eq!(split_ws("ab"), ["ab"]);
        assert!(has_word("git push", "push"));
        assert!(!has_word("git pushed", "push"));
        assert!(!has_word("git unpush", "push"));
        assert!(has_word("push", "push"));
    }

    #[test]
    fn a_long_option_matches_a_leaders_option_by_any_prefix_of_two_or_more_characters() {
        let takes = ["--signal", "--kill-after"];
        assert!(leader_takes(&takes, "--sig"));
        assert!(leader_takes(&takes, "--signal"));
        assert!(!leader_takes(&takes, "--s")); // (?!--[^=]{2,}): one character is not a prefix
        assert!(!leader_takes(&takes, "--sig=TERM"));
        assert!(!leader_takes(&takes, "--nope"));
        assert!(leader_takes(&["-s"], "-s"));
    }

    #[test]
    fn the_head_of_a_command_is_past_its_leaders_assignments_and_their_options() {
        assert_eq!(head_at(&words("git push")), 0);
        assert_eq!(head_at(&words("FOO=1 env BAR=2 timeout 5 git commit")), 5);
        assert_eq!(head_at(&words("sudo -u me git push")), 3);
        assert_eq!(head_at(&words("sudo -S git push")), 2);
        assert_eq!(head_at(&words("timeout --signal TERM 60 git x")), 4);
        assert_eq!(head_at(&words("nice -5 git x")), 2);
        assert_eq!(head_at(&words("echo timeout 60 git push")), 0);
        assert_eq!(head_at(&words("if git x")), 1);
        assert_eq!(head_at::<String>(&[]), 0);
        assert_eq!(head_at(&words("timeout")), 1);
    }

    #[test]
    fn words_split_at_command_separators_outside_quotes() {
        let got = commands("a \"b c\" && d; e | f");
        assert_eq!(got, [vec!["a", "b c"], vec!["d"], vec!["e"], vec!["f"]]);
        assert_eq!(split("x # a comment\ny"), [["x"], ["y"]]);
        assert_eq!(split("a\\\nb c"), [["ab", "c"]]);
        let words = split_words("X=$(cmd) Y='$no' Z=\"$yes\" W=`c`");
        // `$(` and a backtick end the command, and the word before them is a value unknown
        assert_eq!(
            (words[0][0].text.as_str(), words[0][0].expands),
            ("X=$", true)
        );
        assert_eq!(words[1][0].text, "cmd");
        let flags: Vec<_> = words[2].iter().map(|w| w.expands).collect();
        assert_eq!(flags, [false, true, true]);
        assert_eq!(words[2][2].text, "W=");
    }

    #[test]
    fn heredoc_words_skip_shifts_here_strings_comments_and_quotes() {
        let at = |line: &str| -> Vec<(String, bool)> {
            let line: Vec<char> = line.chars().collect();
            heredoc_words(&line)
                .into_iter()
                .map(|h| (h.word, h.quoted))
                .collect()
        };
        assert_eq!(at("cat <<EOF"), [("EOF".into(), false)]);
        assert_eq!(at("cat <<-'EOF'"), [("EOF".into(), true)]);
        assert_eq!(at("bash <<\\EOF"), [("EOF".into(), true)]);
        assert_eq!(at("cat <<E\"O\"F"), [("EOF".into(), true)]);
        assert!(at("cat <<<word").is_empty());
        assert!(at("echo $((1<<x))").is_empty());
        assert!(at("((1<<2))").is_empty());
        assert!(at("x=$[1<<2]").is_empty());
        assert!(at("echo \"a <<b\"").is_empty());
        assert!(at("echo hi # <<EOF").is_empty());
        assert_eq!(at("cat <<'A' <<'B'").len(), 2);
        assert_eq!(at("cat << EOF")[0].0, "EOF");
        assert!(at("cat <<").is_empty());
    }

    #[test]
    fn a_heredoc_body_is_cut_and_is_a_script_only_in_a_shells_own_pipeline() {
        let (text, bodies) = heredocs("cat <<'EOF'; bash -c ':'\nbody\nEOF\nnext");
        assert_eq!(text, "cat <<'EOF'; bash -c ':'\nnext");
        assert_eq!(bodies.len(), 1);
        assert!(!bodies[0].shell);
        assert!(bodies[0].quoted);
        let (_, bodies) = heredocs("cat <<'EOF' | bash -s\nbody\nEOF");
        assert!(bodies[0].shell);
        let (text, bodies) = heredocs("cat <<1 \\\n; git push\nx\n1");
        assert_eq!(text, "cat <<1 \\\n; git push");
        assert_eq!(bodies[0].body, "x");
        let (_, bodies) = heredocs("echo bash <<'EOF'\nbody\nEOF");
        assert!(!bodies[0].shell);
    }

    #[test]
    fn the_pipeline_of_a_position_is_cut_at_separators_but_not_at_pipes_or_redirections() {
        let line = |s: &str| s.chars().collect::<Vec<_>>();
        let l = line("a; cat <<X | sh");
        assert_eq!(pipeline_of(&l, 5), " cat <<X | sh");
        let l = line("cat <<X >&2 && bash");
        assert_eq!(pipeline_of(&l, 4), "cat <<X >&2 ");
        let l = line("cat <<X &> f; sh");
        assert_eq!(pipeline_of(&l, 4), "cat <<X &> f");
        let l = line("echo 'a;b' <<X; sh");
        assert_eq!(pipeline_of(&l, 11), "echo 'a;b' <<X");
        let l = line("cat <<X |& sh");
        assert_eq!(pipeline_of(&l, 4), "cat <<X |& sh");
    }

    #[test]
    fn substitutions_run_where_the_shell_runs_them() {
        assert_eq!(substitutions("echo \"$(a)\"", true), ["a"]);
        assert!(substitutions("echo '$(a)'", true).is_empty());
        assert_eq!(substitutions("echo '$(a)'", false), ["a"]);
        assert_eq!(substitutions("x=$(echo $(a))", true), ["echo $(a)"]);
        assert_eq!(substitutions("`a` b `c`", true), ["a", "c"]);
        assert!(substitutions("echo $((1 + 2))", true).is_empty());
        assert_eq!(substitutions("$(a", true), ["a"]);
        assert_eq!(substitutions("`a", true), ["a"]);
        assert!(substitutions("echo \\$(a)", true).is_empty());
        assert_eq!(substitutions("echo \"it's\" '$(a)'", true).len(), 0);
    }

    #[test]
    fn a_shell_is_told_its_script_by_a_cluster_with_c_or_by_eval() {
        let inner = |s: &str| inner(&split(s).remove(0));
        assert_eq!(inner("bash -c \"git push\""), ["git push"]);
        assert_eq!(inner("sh -ce 'x'"), ["x"]);
        assert_eq!(inner("zsh -lc y"), ["y"]);
        assert_eq!(inner("bash -o pipefail -c z"), ["z"]);
        assert_eq!(inner("eval git push"), ["git push"]);
        assert!(inner("eval").is_empty());
        assert!(inner("echo sh -c x").is_empty());
        assert!(inner("bash script.sh").is_empty());
        assert!(inner("bash --c x").is_empty());
        assert!(inner("bash -c").is_empty());
        assert_eq!(inner("/bin/bash -c a"), ["a"]);
    }
}
