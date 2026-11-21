//! One deterministic line saying WHAT a commit changed inside a symbol.
//!
//! Input: the symbol's span text before and after the authoring commit. Output:
//! a clause list such as `isFreeStation → isUnwalledStation; now uses
//! isFullyFreeStation`, or `None` when no clean summary exists. No model, no
//! clock, no I/O.
//!
//! 1. A bounded line-level LCS diff of `before` vs `after` gives hunks.
//! 2. Identifiers are read from code only: string-literal contents and
//!    comments are blanked first (over the whole span, so a multi-line string
//!    or block comment is blanked line by line), language keywords dropped.
//! 3. A hunk's removed identifiers that appear NOWHERE in `after` are
//!    removed-new; its added identifiers that appear NOWHERE in `before` are
//!    added-new. An identifier only moved, kept, or reordered says nothing.
//! 4. One removed-new and one added-new in a hunk read as a rename
//!    (`a → b`); otherwise added-new reads `now uses a, b` and removed-new
//!    `no longer uses c`. Identifiers are named once (first appearance wins),
//!    and adjacent `uses` / `no longer uses` clauses merge (renames stay one
//!    clause each, so the cap can drop them one at a time).
//! 5. Clauses are joined with `; ` and capped at [`MAX_CHARS`] with whole
//!    clauses only, ending `; +N more` when some were dropped.
//!
//! Abstention-first: whitespace, comment, string-literal, or reorder-only
//! changes have no clause and answer `None`; so does a span of more than
//! [`MAX_LINES`] lines on either side, and a first clause that cannot fit.

use std::collections::HashSet;

/// Longest summary, in characters.
pub(crate) const MAX_CHARS: usize = 100;
/// Longest span, in lines per side, that is diffed at all.
pub(crate) const MAX_LINES: usize = 2_000;

/// How a language writes comments and strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Dialect {
    /// TypeScript / TSX / JavaScript: `//`, `/* */`, `'`/`"` strings, template
    /// literals with `${}` code holes.
    Ts,
    /// Rust: `//`, `/* */`, `"` strings, raw strings, `'x'` chars vs lifetimes.
    Rust,
    /// Python: `#`, `"""`/`'''` and single-line strings.
    Python,
    /// Go: `//`, `/* */`, `"` strings, raw backtick strings, `'x'` runes.
    Go,
}

/// The dialect for a file path (the languages the symbol extractor reads).
pub(crate) fn dialect_of(path: &str) -> Dialect {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "rs" => Dialect::Rust,
        "py" => Dialect::Python,
        "go" => Dialect::Go,
        _ => Dialect::Ts,
    }
}

const KEYWORDS: &[&str] = &[
    "if",
    "else",
    "return",
    "const",
    "let",
    "var",
    "function",
    "true",
    "false",
    "null",
    "undefined",
    "new",
    "this",
    "await",
    "async",
    "for",
    "while",
    "in",
    "of",
    "import",
    "from",
    "export",
    "default",
    "class",
    "extends",
    "type",
    "interface",
    "fn",
    "pub",
    "mut",
    "impl",
    "self",
    "Self",
    "match",
    "struct",
    "enum",
    "use",
    "mod",
    "def",
    "None",
    "True",
    "False",
    "and",
    "or",
    "not",
    "is",
    "lambda",
    "pass",
    "yield",
    "with",
    "as",
    "elif",
    "try",
    "except",
    "finally",
    "raise",
    "func",
    "package",
    "go",
    "defer",
    "chan",
    "map",
    "range",
    // The rest of "etc.": other control words and primitive type names.
    "break",
    "continue",
    "switch",
    "case",
    "throw",
    "catch",
    "typeof",
    "void",
    "static",
    "public",
    "private",
    "protected",
    "readonly",
    "abstract",
    "implements",
    "super",
    "delete",
    "instanceof",
    "do",
    "loop",
    "crate",
    "ref",
    "move",
    "dyn",
    "unsafe",
    "trait",
    "where",
    "nil",
    "select",
    "global",
    "nonlocal",
    "assert",
    "del",
    "string",
    "number",
    "boolean",
    "any",
    "unknown",
    "never",
    "object",
    "bool",
    "str",
    "char",
    "u8",
    "u16",
    "u32",
    "u64",
    "usize",
    "i8",
    "i16",
    "i32",
    "i64",
    "isize",
    "f32",
    "f64",
];

fn is_ident_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// `text` with the contents of comments and string literals replaced by spaces
/// and every newline kept, so the result has the same line structure.
fn blank_non_code(text: &str, dialect: Dialect) -> String {
    #[derive(Clone, Copy)]
    enum St {
        Code,
        Line,
        Block,
        Str {
            quote: char,
            triple: bool,
            multi: bool,
            escapes: bool,
        },
        Raw {
            hashes: usize,
        },
        Template,
    }
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut st = St::Code;
    // Brace depth inside each open `${` hole of a template literal.
    let mut holes: Vec<usize> = Vec::new();
    let mut i = 0;
    let at = |i: usize| c.get(i).copied();
    let blank = |out: &mut String, ch: char| out.push(if ch == '\n' { '\n' } else { ' ' });
    while i < c.len() {
        let ch = c[i];
        match st {
            St::Code => {
                let next = at(i + 1);
                let hash_comment = dialect == Dialect::Python && ch == '#';
                let slash_comment = dialect != Dialect::Python && ch == '/' && next == Some('/');
                if hash_comment || slash_comment {
                    st = St::Line;
                    blank(&mut out, ch);
                    i += 1;
                } else if dialect != Dialect::Python && ch == '/' && next == Some('*') {
                    st = St::Block;
                    out.push_str("  ");
                    i += 2;
                } else if ch == '"'
                    || (ch == '\'' && matches!(dialect, Dialect::Ts | Dialect::Python))
                {
                    let triple = dialect == Dialect::Python
                        && at(i + 1) == Some(ch)
                        && at(i + 2) == Some(ch);
                    st = St::Str {
                        quote: ch,
                        triple,
                        multi: triple,
                        escapes: true,
                    };
                    let width = if triple { 3 } else { 1 };
                    out.push_str(&" ".repeat(width));
                    i += width;
                } else if ch == '\'' {
                    // Rust / Go: a char or rune literal, else a lifetime (code).
                    let literal_end = if next == Some('\\') {
                        (i + 3..(i + 12).min(c.len())).find(|&j| c[j] == '\'')
                    } else if at(i + 2) == Some('\'') {
                        Some(i + 2)
                    } else {
                        None
                    };
                    match literal_end {
                        Some(end) => {
                            out.push_str(&" ".repeat(end - i + 1));
                            i = end + 1;
                        }
                        None => {
                            out.push(ch);
                            i += 1;
                        }
                    }
                } else if ch == '`' && dialect == Dialect::Ts {
                    st = St::Template;
                    out.push(' ');
                    i += 1;
                } else if ch == '`' && dialect == Dialect::Go {
                    st = St::Str {
                        quote: '`',
                        triple: false,
                        multi: true,
                        escapes: false,
                    };
                    out.push(' ');
                    i += 1;
                } else if ch == 'r'
                    && dialect == Dialect::Rust
                    && !(i > 0 && is_ident_part(c[i - 1]))
                    && matches!(next, Some('"') | Some('#'))
                {
                    let mut j = i + 1;
                    while at(j) == Some('#') {
                        j += 1;
                    }
                    if at(j) == Some('"') {
                        st = St::Raw { hashes: j - i - 1 };
                        out.push_str(&" ".repeat(j - i + 1));
                        i = j + 1;
                    } else {
                        out.push(ch);
                        i += 1;
                    }
                } else {
                    if let Some(depth) = holes.last_mut() {
                        if ch == '{' {
                            *depth += 1;
                        } else if ch == '}' {
                            if *depth == 0 {
                                holes.pop();
                                st = St::Template;
                                out.push(' ');
                                i += 1;
                                continue;
                            }
                            *depth -= 1;
                        }
                    }
                    out.push(ch);
                    i += 1;
                }
            }
            St::Line => {
                if ch == '\n' {
                    st = St::Code;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            St::Block => {
                if ch == '*' && at(i + 1) == Some('/') {
                    st = St::Code;
                    out.push_str("  ");
                    i += 2;
                } else {
                    blank(&mut out, ch);
                    i += 1;
                }
            }
            St::Str {
                quote,
                triple,
                multi,
                escapes,
            } => {
                if escapes && ch == '\\' {
                    out.push(' ');
                    if let Some(escaped) = at(i + 1) {
                        blank(&mut out, escaped);
                    }
                    i += 2;
                } else if triple
                    && ch == quote
                    && at(i + 1) == Some(quote)
                    && at(i + 2) == Some(quote)
                {
                    st = St::Code;
                    out.push_str("   ");
                    i += 3;
                } else if !triple && ch == quote {
                    st = St::Code;
                    out.push(' ');
                    i += 1;
                } else if ch == '\n' && !multi {
                    st = St::Code;
                    out.push('\n');
                    i += 1;
                } else {
                    blank(&mut out, ch);
                    i += 1;
                }
            }
            St::Raw { hashes } => {
                let closes = ch == '"' && (1..=hashes).all(|k| at(i + k) == Some('#'));
                if closes {
                    st = St::Code;
                    out.push_str(&" ".repeat(hashes + 1));
                    i += hashes + 1;
                } else {
                    blank(&mut out, ch);
                    i += 1;
                }
            }
            St::Template => {
                if ch == '\\' {
                    out.push(' ');
                    if let Some(escaped) = at(i + 1) {
                        blank(&mut out, escaped);
                    }
                    i += 2;
                } else if ch == '`' {
                    st = St::Code;
                    out.push(' ');
                    i += 1;
                } else if ch == '$' && at(i + 1) == Some('{') {
                    holes.push(0);
                    st = St::Code;
                    out.push_str("  ");
                    i += 2;
                } else {
                    blank(&mut out, ch);
                    i += 1;
                }
            }
        }
    }
    out
}

/// Identifiers of already-blanked code, in order of appearance, keywords and
/// number literals (`0x1F`, `1e5`) dropped.
fn identifiers(code: &str) -> Vec<String> {
    let keywords: HashSet<&str> = KEYWORDS.iter().copied().collect();
    let mut found = Vec::new();
    let mut word = String::new();
    let mut numeric = false;
    let mut flush = |word: &mut String, numeric: &mut bool| {
        if !word.is_empty() && !*numeric && !keywords.contains(word.as_str()) {
            found.push(std::mem::take(word));
        }
        word.clear();
        *numeric = false;
    };
    for ch in code.chars() {
        if is_ident_part(ch) {
            if word.is_empty() {
                numeric = ch.is_ascii_digit();
            }
            word.push(ch);
        } else {
            flush(&mut word, &mut numeric);
        }
    }
    flush(&mut word, &mut numeric);
    found
}

/// A run of changed lines between two runs of common ones:
/// `(removed rows in before, added rows in after)`.
type Hunk = (std::ops::Range<usize>, std::ops::Range<usize>);

/// Line-level LCS hunks of `a` vs `b` (lines compared trimmed). `None` when
/// either side is over [`MAX_LINES`].
fn diff_hunks(a: &[&str], b: &[&str]) -> Option<Vec<Hunk>> {
    if a.len() > MAX_LINES || b.len() > MAX_LINES {
        return None;
    }
    let same = |i: usize, j: usize| a[i].trim() == b[j].trim();
    let (mut lo, mut hi_a, mut hi_b) = (0, a.len(), b.len());
    while lo < hi_a && lo < hi_b && same(lo, lo) {
        lo += 1;
    }
    while hi_a > lo && hi_b > lo && same(hi_a - 1, hi_b - 1) {
        hi_a -= 1;
        hi_b -= 1;
    }
    let (n, m) = (hi_a - lo, hi_b - lo);
    // table[i][j]: LCS length of a[lo+i..hi_a] and b[lo+j..hi_b].
    let width = m + 1;
    let mut table = vec![0u16; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if same(lo + i, lo + j) {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    let (mut i, mut j) = (0, 0);
    let close = |open: &mut Option<(usize, usize)>, i: usize, j: usize, hunks: &mut Vec<Hunk>| {
        if let Some((si, sj)) = open.take() {
            hunks.push((lo + si..lo + i, lo + sj..lo + j));
        }
    };
    while i < n || j < m {
        if i < n && j < m && same(lo + i, lo + j) {
            close(&mut open, i, j, &mut hunks);
            i += 1;
            j += 1;
            continue;
        }
        open.get_or_insert((i, j));
        if j >= m || (i < n && table[(i + 1) * width + j] >= table[i * width + j + 1]) {
            i += 1;
        } else {
            j += 1;
        }
    }
    close(&mut open, i, j, &mut hunks);
    Some(hunks)
}

#[derive(PartialEq, Eq, Debug)]
enum Clause {
    /// Never merged with its neighbours: each rename can be dropped on its own by the cap.
    Renamed(String, String),
    Uses(Vec<String>),
    Drops(Vec<String>),
}

impl Clause {
    fn render(&self) -> String {
        match self {
            Clause::Renamed(from, to) => format!("{from} → {to}"),
            Clause::Uses(names) => format!("now uses {}", names.join(", ")),
            Clause::Drops(names) => format!("no longer uses {}", names.join(", ")),
        }
    }

    /// Fold `next` into `self` when both are the same list kind (uses / drops).
    fn absorb(&mut self, next: Clause) -> Option<Clause> {
        match (self, next) {
            (Clause::Uses(a), Clause::Uses(b)) | (Clause::Drops(a), Clause::Drops(b)) => {
                a.extend(b)
            }
            (_, other) => return Some(other),
        }
        None
    }
}

/// Distinct entries of `names`, first appearance wins, minus those in `skip`.
fn distinct_new(names: &[String], skip: &HashSet<&str>, seen: &mut HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        if !skip.contains(name.as_str()) && seen.insert(name.clone()) {
            out.push(name.clone());
        }
    }
    out
}

/// The one-line summary of `before` -> `after`, or `None`. See the module doc.
pub(crate) fn summarize(before: &str, after: &str, dialect: Dialect) -> Option<String> {
    let before_code = blank_non_code(before, dialect);
    let after_code = blank_non_code(after, dialect);
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let before_code_lines: Vec<&str> = before_code.lines().collect();
    let after_code_lines: Vec<&str> = after_code.lines().collect();
    if before_code_lines.len() != before_lines.len() || after_code_lines.len() != after_lines.len()
    {
        return None;
    }
    let hunks = diff_hunks(&before_lines, &after_lines)?;
    let before_ids = identifiers(&before_code);
    let after_ids = identifiers(&after_code);
    let in_before: HashSet<&str> = before_ids.iter().map(String::as_str).collect();
    let in_after: HashSet<&str> = after_ids.iter().map(String::as_str).collect();

    let (mut seen_removed, mut seen_added) = (HashSet::new(), HashSet::new());
    let mut clauses: Vec<Clause> = Vec::new();
    let push = |clause: Clause, clauses: &mut Vec<Clause>| {
        let rest = match clauses.last_mut() {
            Some(last) => last.absorb(clause),
            None => Some(clause),
        };
        if let Some(clause) = rest {
            clauses.push(clause);
        }
    };
    for (removed, added) in hunks {
        let removed_ids = identifiers(&before_code_lines[removed].join("\n"));
        let added_ids = identifiers(&after_code_lines[added].join("\n"));
        let gone = distinct_new(&removed_ids, &in_after, &mut seen_removed);
        let new = distinct_new(&added_ids, &in_before, &mut seen_added);
        if gone.len() == 1 && new.len() == 1 {
            push(
                Clause::Renamed(gone[0].clone(), new[0].clone()),
                &mut clauses,
            );
            continue;
        }
        if !new.is_empty() {
            push(Clause::Uses(new), &mut clauses);
        }
        if !gone.is_empty() {
            push(Clause::Drops(gone), &mut clauses);
        }
    }
    cap(&clauses)
}

/// The clauses joined with `; ` within [`MAX_CHARS`], whole clauses only,
/// ending `; +N more` when the tail was dropped.
fn cap(clauses: &[Clause]) -> Option<String> {
    let rendered: Vec<String> = clauses.iter().map(Clause::render).collect();
    for kept in (1..=rendered.len()).rev() {
        let mut line = rendered[..kept].join("; ");
        if kept < rendered.len() {
            line.push_str(&format!("; +{} more", rendered.len() - kept));
        }
        if line.chars().count() <= MAX_CHARS {
            return Some(line);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATION_BEFORE: &str = "function StationCard({ station }) {
  const { isEffectivelySubscribed } = useSubscription();
  return (
    <View>
      {!isFreeStation(station) ? (
        <Lock />
      ) : null}
      <Text>{station.name}</Text>
      {!isEffectivelySubscribed ? (
        <Upsell />
      ) : null}
    </View>
  );
}";

    fn station_after() -> String {
        STATION_BEFORE
            .replace("isFreeStation", "isUnwalledStation")
            .replace(
                "{!isEffectivelySubscribed ? (",
                "{!isEffectivelySubscribed && !isFullyFreeStation(station) ? (",
            )
    }

    #[test]
    fn a_rename_and_a_new_call_read_as_the_card_would_say_it() {
        assert_eq!(
            summarize(STATION_BEFORE, &station_after(), Dialect::Ts).as_deref(),
            Some("isFreeStation → isUnwalledStation; now uses isFullyFreeStation")
        );
    }

    #[test]
    fn whitespace_only_changes_have_no_summary() {
        let after = STATION_BEFORE
            .replace("\n  ", "\n      ")
            .replace("= useSubscription", "=   useSubscription");
        assert_eq!(summarize(STATION_BEFORE, &after, Dialect::Ts), None);
        assert_eq!(summarize(STATION_BEFORE, STATION_BEFORE, Dialect::Ts), None);
    }

    #[test]
    fn comment_only_changes_have_no_summary() {
        let before =
            "function f() {\n  // old note about legacyThing\n  return 1; /* keep oldHelper */\n}";
        let after = "function f() {\n  // new note about brandNewThing\n  return 1; /* keep otherHelper */\n}";
        assert_eq!(summarize(before, after, Dialect::Ts), None);
        let py_before = "def f():\n    # uses legacy_thing\n    return 1\n";
        let py_after = "def f():\n    # uses fresh_thing\n    return 1\n";
        assert_eq!(summarize(py_before, py_after, Dialect::Python), None);
    }

    #[test]
    fn string_literal_only_changes_have_no_summary() {
        let before = "function f() {\n  return label(\"free station\", 'it\\'s old', `plain ${count} text`);\n}";
        let after = "function f() {\n  return label(\"unwalled radio\", 'it\\'s new', `other ${count} words`);\n}";
        assert_eq!(summarize(before, after, Dialect::Ts), None);
    }

    #[test]
    fn identifiers_in_template_holes_count_but_template_text_does_not() {
        let before = "function f() {\n  return `hello ${oldName} there`;\n}";
        let after = "function f() {\n  return `bye ${newName} friend`;\n}";
        assert_eq!(
            summarize(before, after, Dialect::Ts).as_deref(),
            Some("oldName → newName")
        );
    }

    #[test]
    fn a_reorder_or_a_moved_identifier_has_no_summary() {
        let before = "function f(a, b) {\n  first(a);\n  second(b);\n}";
        let after = "function f(a, b) {\n  second(b);\n  first(a);\n}";
        assert_eq!(summarize(before, after, Dialect::Ts), None);
    }

    #[test]
    fn a_rust_change_names_what_was_swapped_and_ignores_lifetimes_and_strings() {
        let before = "fn load<'a>(path: &'a str) -> Result<Config, Error> {\n    let raw = read_file(path)?;\n    let note = \"read_file is slow\";\n    parse_config(&raw)\n}";
        let after = "fn load<'a>(path: &'a str) -> Result<Config, Error> {\n    let raw = read_text(path)?;\n    let note = \"now read_text\";\n    parse_config(&raw)\n}";
        assert_eq!(
            summarize(before, after, Dialect::Rust).as_deref(),
            Some("read_file → read_text")
        );
        let raw_before = "fn f() -> &'static str {\n    r#\"has \"quotes\" old_word\"#\n}";
        let raw_after = "fn f() -> &'static str {\n    r#\"has \"quotes\" new_word\"#\n}";
        assert_eq!(summarize(raw_before, raw_after, Dialect::Rust), None);
        let chars_before = "fn f(c: char) -> bool {\n    c == 'x'\n}";
        let chars_after = "fn f(c: char) -> bool {\n    c == 'y'\n}";
        assert_eq!(summarize(chars_before, chars_after, Dialect::Rust), None);
    }

    #[test]
    fn a_python_change_ignores_docstrings_and_hash_comments() {
        let before = "def total(items):\n    \"\"\"Sum old_name items.\"\"\"\n    # old_note\n    return sum_all(items)\n";
        let after = "def total(items):\n    \"\"\"Sum new_name items.\"\"\"\n    # new_note\n    return sum_prices(items)\n";
        assert_eq!(
            summarize(before, after, Dialect::Python).as_deref(),
            Some("sum_all → sum_prices")
        );
    }

    #[test]
    fn a_pure_deletion_says_no_longer_uses() {
        let before = "function f() {\n  audit(event);\n  save(event);\n}";
        let after = "function f() {\n  save(event);\n}";
        assert_eq!(
            summarize(before, after, Dialect::Ts).as_deref(),
            Some("no longer uses audit")
        );
    }

    #[test]
    fn many_changes_are_cut_to_whole_clauses_with_a_count() {
        let mut before = String::from("function f() {\n");
        let mut after = String::from("function f() {\n");
        for n in 0..8 {
            before.push_str(&format!("  keep{n}();\n  oldCallNumber{n}();\n"));
            after.push_str(&format!("  keep{n}();\n  newCallNumber{n}();\n"));
        }
        before.push('}');
        after.push('}');
        let summary = summarize(&before, &after, Dialect::Ts).unwrap();
        assert!(summary.chars().count() <= MAX_CHARS, "{summary}");
        assert_eq!(
            summary,
            "oldCallNumber0 → newCallNumber0; oldCallNumber1 → newCallNumber1; +6 more"
        );
    }

    #[test]
    fn dropped_clauses_are_counted_in_the_suffix() {
        // Three clauses (rename, long `now uses`, rename); the long middle one forces the cut.
        let before = "function f() {\n  a1();\n  keep();\n  keep2();\n  c1();\n}";
        let long = "veryLongIdentifierNumberOne, veryLongIdentifierNumberTwo, veryLongIdentifierNumberThree";
        let after = format!(
            "function f() {{\n  a2();\n  keep();\n  {}\n  keep2();\n  c2();\n}}",
            long.split(", ")
                .map(|n| format!("{n}();"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let summary = summarize(before, &after, Dialect::Ts).unwrap();
        assert!(summary.chars().count() <= MAX_CHARS, "{summary}");
        assert!(summary.starts_with("a1 → a2"), "{summary}");
        assert!(summary.ends_with("; +2 more"), "{summary}");
    }

    #[test]
    fn a_first_clause_over_the_cap_has_no_summary() {
        let before = "function f() {\n  old();\n}";
        let names: Vec<String> = (0..12)
            .map(|n| format!("aVeryLongNewFunctionName{n}"))
            .collect();
        let after = format!(
            "function f() {{\n  {}\n}}",
            names.iter().map(|n| format!("{n}();")).collect::<String>()
        );
        assert_eq!(summarize(before, &after, Dialect::Ts), None);
    }

    #[test]
    fn a_span_over_the_line_cap_has_no_summary() {
        let big = "x();\n".repeat(MAX_LINES + 1);
        assert_eq!(summarize(&big, "y();\n", Dialect::Ts), None);
        assert_eq!(summarize("y();\n", &big, Dialect::Ts), None);
        let edge = "x();\n".repeat(MAX_LINES);
        assert!(summarize(&edge, "x();\nz();\n", Dialect::Ts).is_some());
    }

    #[test]
    fn numbers_and_keywords_are_not_identifiers() {
        let ids = identifiers("const x1 = 0x1F + 1e5 + y; return this.value ?? null;");
        assert_eq!(ids, ["x1", "y", "value"]);
    }

    #[test]
    fn the_dialect_follows_the_extension() {
        assert_eq!(dialect_of("a/b.rs"), Dialect::Rust);
        assert_eq!(dialect_of("a/b.PY"), Dialect::Python);
        assert_eq!(dialect_of("a/b.go"), Dialect::Go);
        assert_eq!(dialect_of("a/b.tsx"), Dialect::Ts);
    }
}
