//! Deterministic identifier extraction and query analysis.
//!
//! The identifier postings table (`storage::identifier_postings`) indexes the
//! names that FTS5's porter/unicode61 tokenizer shreds: paths, `snake_case` and
//! `camelCase` symbols, `a::b` paths, crate names, `--flags`, commit SHAs, PR
//! numbers and acronyms. This module is the one place that decides what such a
//! name looks like, for the chunk side ([`extract_identifiers`]) and the query
//! side ([`analyze_query`]), so both sides always agree.
//!
//! Regex and token rules only, no model, no I/O. Cost is O(content): a fixed
//! number of linear passes, each masking what it consumed so later passes do
//! not re-read it. Plain prose words are never extracted; they stay with FTS5.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

/// Terms kept per chunk. Paths are listed first, so the cap never drops one
/// in favour of another kind.
pub const MAX_TERMS_PER_CHUNK: usize = 48;

/// Longest term stored, in bytes. Longer runs are minified or generated text.
const MAX_TERM_BYTES: usize = 200;
/// Longest symbol/crate/acronym token considered, in bytes.
const MAX_WORD_BYTES: usize = 80;

/// Bare filenames (no `/`) count as paths only with one of these extensions.
/// A whitelist, not a heuristic: `e.g` and `v1.2` never qualify.
const BARE_FILE_EXTENSIONS: &str = "rs|py|ts|tsx|js|jsx|mjs|cjs|md|toml|json|jsonl|yaml|yml|sh|zsh|sql|go|java|kt|swift|c|h|cpp|hpp|css|html|lock|txt|rb|ex|exs|lua|plist|ini|cfg|conf|env|log|csv";

/// All-caps words that are prose, not acronyms.
const CAPS_STOPWORDS: &[&str] = &[
    "AN", "AND", "ANY", "ARE", "AS", "AT", "BE", "BUT", "BY", "CAN", "DID", "DO", "FOR", "FROM",
    "GET", "HAS", "HAD", "HE", "HOW", "IF", "IN", "IS", "IT", "ITS", "NO", "NOT", "NOW", "OF",
    "OFF", "OK", "ON", "ONE", "ONLY", "OR", "OUT", "SO", "THE", "THEN", "THIS", "TO", "TWO", "UP",
    "USE", "WAS", "WE", "WHAT", "WHEN", "WHO", "WHY", "WILL", "WITH", "YES", "YOU",
];

/// What kind of name a term is. The `u8` value is stored in the postings
/// table; never renumber.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentKind {
    Path = 1,
    Symbol = 2,
    Crate = 3,
    Flag = 4,
    Sha = 5,
    PrNumber = 6,
    Acronym = 7,
}

impl IdentKind {
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// One identifier-shaped term of a query with the weight it carries in
/// `search_postings`.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryIdent {
    pub term: String,
    pub kind: IdentKind,
    pub weight: f32,
}

struct Patterns {
    uuid: Regex,
    url: Regex,
    colon_path: Regex,
    slash_token: Regex,
    bare_file: Regex,
    flag: Regex,
    sha: Regex,
    pr: Regex,
    word: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        uuid: Regex::new(
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        )
        .expect("uuid regex"),
        url: Regex::new(r#"[A-Za-z][A-Za-z0-9+.\-]*://[^\s)>\]"'`]+"#).expect("url regex"),
        colon_path: Regex::new(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+")
            .expect("colon path regex"),
        slash_token: Regex::new(r"[A-Za-z0-9_.~@+%/\-]*/[A-Za-z0-9_.~@+%/\-]*")
            .expect("slash token regex"),
        bare_file: Regex::new(&format!(
            r"(?i)\b(?:[A-Za-z0-9_\-]+\.)+(?:{BARE_FILE_EXTENSIONS})\b"
        ))
        .expect("bare file regex"),
        flag: Regex::new(r"(?:^|[^A-Za-z0-9_\-])(--[a-z][a-z0-9]*(?:-[a-z0-9]+)*)")
            .expect("flag regex"),
        sha: Regex::new(r"\b[0-9a-fA-F]{7,40}\b").expect("sha regex"),
        pr: Regex::new(r"(?:^|[^A-Za-z0-9_&#])#([0-9]{1,6})\b").expect("pr regex"),
        word: Regex::new(r"[A-Za-z0-9_]+(?:-[A-Za-z0-9_]+)*").expect("word regex"),
    })
}

/// Replace `buf[start..end]` with spaces of the same byte length.
fn mask(buf: &mut String, spans: &[(usize, usize)]) {
    if spans.is_empty() {
        return;
    }
    let mut out = String::with_capacity(buf.len());
    let mut at = 0;
    for &(start, end) in spans {
        if start < at {
            continue;
        }
        out.push_str(&buf[at..start]);
        out.push_str(&" ".repeat(end - start));
        at = end;
    }
    out.push_str(&buf[at..]);
    *buf = out;
}

fn has_extension(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || ext.is_empty() || ext.len() > 8 {
        return false;
    }
    let mut chars = ext.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

fn is_dotfile(name: &str) -> bool {
    name.len() > 1
        && name.starts_with('.')
        && name[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The lowercased path and its aliases (basename, basename without its
/// extension), or `None` when `token` is not a path.
fn path_terms(token: &str) -> Option<Vec<String>> {
    let trimmed = token.trim_end_matches(['.', '-', '/', '+']);
    let had_trailing_slash = token.trim_end_matches(['.', '-']).ends_with('/');
    if trimmed.is_empty() || trimmed.len() > MAX_TERM_BYTES {
        return None;
    }
    let anchored = trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with("./")
        || trimmed.starts_with("../");
    let segs: Vec<&str> = trimmed
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != ".." && *s != "~")
        .collect();
    let last = *segs.last()?;
    let named = !had_trailing_slash && (has_extension(last) || is_dotfile(last));
    let ok = named || (anchored && segs.len() >= 2);
    if !ok {
        return None;
    }
    let full = trimmed.strip_prefix("./").unwrap_or(trimmed).to_lowercase();
    let mut terms = vec![full];
    if named {
        let base = last.to_lowercase();
        if !terms.contains(&base) {
            terms.push(base.clone());
        }
        if let Some((stem, _)) = base.rsplit_once('.') {
            if stem.len() >= 2 && !terms.iter().any(|t| t == stem) {
                terms.push(stem.to_string());
            }
        }
    }
    Some(terms)
}

fn is_hex_sha(s: &str) -> bool {
    let has_digit = s.bytes().any(|b| b.is_ascii_digit());
    let has_alpha = s.bytes().any(|b| b.is_ascii_alphabetic());
    if !(has_digit && has_alpha) {
        return false;
    }
    // `1e100000` is a number, not an object name.
    let lower = s.to_ascii_lowercase();
    if let Some((a, b)) = lower.split_once('e') {
        if !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
        {
            return false;
        }
    }
    // Hex of one case only: mixed case is a word.
    let upper = s.bytes().any(|b| b.is_ascii_uppercase());
    let lowerc = s.bytes().any(|b| b.is_ascii_lowercase());
    !(upper && lowerc)
}

fn is_snake(word: &str) -> bool {
    let core = word.trim_matches('_');
    core.len() >= 3
        && core.contains('_')
        && !core.bytes().all(|b| b.is_ascii_digit() || b == b'_')
        && !core.contains("__")
}

fn is_camel(word: &str) -> bool {
    if word.len() < 4 || word.contains('_') {
        return false;
    }
    let b = word.as_bytes();
    if !b.iter().all(u8::is_ascii_alphanumeric) {
        return false;
    }
    b.windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
}

fn is_acronym(word: &str) -> bool {
    (2..=6).contains(&word.len())
        && word.bytes().all(|b| b.is_ascii_uppercase())
        && !CAPS_STOPWORDS.contains(&word)
}

fn is_crate(token: &str) -> bool {
    let segments = token.split('-').count();
    token.len() <= 48
        && (2..=4).contains(&segments)
        && token.starts_with(|c: char| c.is_ascii_lowercase())
        && token
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Extract the identifier-shaped terms of `text`: lowercased, deduplicated,
/// at most [`MAX_TERMS_PER_CHUNK`], paths first.
pub fn extract_identifiers(text: &str) -> Vec<(String, IdentKind)> {
    let p = patterns();
    let mut buf = text.to_string();

    // Not identifiers, only noise for the passes below.
    let uuids: Vec<_> = p
        .uuid
        .find_iter(&buf)
        .map(|m| (m.start(), m.end()))
        .collect();
    mask(&mut buf, &uuids);
    let urls: Vec<_> = p
        .url
        .find_iter(&buf)
        .map(|m| (m.start(), m.end()))
        .collect();
    mask(&mut buf, &urls);

    let mut path_full: Vec<String> = Vec::new();
    let mut path_alias: Vec<String> = Vec::new();
    let mut shas: Vec<String> = Vec::new();
    let mut prs: Vec<String> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    let mut symbols: Vec<String> = Vec::new();
    let mut crates: Vec<String> = Vec::new();
    let mut acronyms: Vec<String> = Vec::new();

    // `a::b::c`: the whole path and its last segment.
    let spans: Vec<_> = p
        .colon_path
        .find_iter(&buf)
        .map(|m| (m.start(), m.end(), m.as_str().to_lowercase()))
        .collect();
    for (_, _, term) in &spans {
        if term.len() > MAX_WORD_BYTES {
            continue;
        }
        symbols.push(term.clone());
        if let Some(last) = term.rsplit("::").next() {
            if last.len() >= 4 || last.contains('_') {
                symbols.push(last.to_string());
            }
        }
    }
    mask(
        &mut buf,
        &spans.iter().map(|(s, e, _)| (*s, *e)).collect::<Vec<_>>(),
    );

    // Paths with a `/`.
    let mut spans = Vec::new();
    for m in p.slash_token.find_iter(&buf) {
        if let Some(mut terms) = path_terms(m.as_str()) {
            path_full.push(terms.remove(0));
            path_alias.extend(terms);
            spans.push((m.start(), m.end()));
        }
    }
    mask(&mut buf, &spans);

    // Bare filenames with a whitelisted extension.
    let mut spans = Vec::new();
    for m in p.bare_file.find_iter(&buf) {
        let name = m.as_str().to_lowercase();
        if name.len() > MAX_TERM_BYTES {
            continue;
        }
        if let Some((stem, _)) = name.rsplit_once('.') {
            if stem.len() >= 2 {
                path_alias.push(stem.to_string());
            }
        }
        path_full.push(name);
        spans.push((m.start(), m.end()));
    }
    mask(&mut buf, &spans);

    // `--long-flag`.
    let spans: Vec<_> = p
        .flag
        .captures_iter(&buf)
        .filter_map(|c| c.get(1))
        .map(|m| (m.start(), m.end()))
        .collect();
    for &(s, e) in &spans {
        if e - s <= MAX_WORD_BYTES {
            flags.push(buf[s..e].to_string());
        }
    }
    mask(&mut buf, &spans);

    // Commit SHAs.
    let spans: Vec<_> = p
        .sha
        .find_iter(&buf)
        .filter(|m| is_hex_sha(m.as_str()))
        .map(|m| (m.start(), m.end()))
        .collect();
    for &(s, e) in &spans {
        shas.push(buf[s..e].to_lowercase());
    }
    mask(&mut buf, &spans);

    // `#123`.
    let spans: Vec<_> =
        p.pr.captures_iter(&buf)
            .filter_map(|c| c.get(1))
            .map(|m| (m.start() - 1, m.end()))
            .collect();
    for &(s, e) in &spans {
        prs.push(buf[s..e].to_string());
    }
    mask(&mut buf, &spans);

    // Words: symbols, crates, acronyms.
    for m in p.word.find_iter(&buf) {
        let token = m.as_str();
        if token.len() > MAX_WORD_BYTES {
            continue;
        }
        if token.contains('-') {
            if is_crate(token) {
                crates.push(token.to_string());
                continue;
            }
            for part in token.split('-') {
                classify_word(part, &mut symbols, &mut acronyms);
            }
        } else {
            classify_word(token, &mut symbols, &mut acronyms);
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, IdentKind)> = Vec::new();
    let groups: [(Vec<String>, IdentKind); 8] = [
        (path_full, IdentKind::Path),
        (path_alias, IdentKind::Path),
        (shas, IdentKind::Sha),
        (prs, IdentKind::PrNumber),
        (flags, IdentKind::Flag),
        (symbols, IdentKind::Symbol),
        (crates, IdentKind::Crate),
        (acronyms, IdentKind::Acronym),
    ];
    for (terms, kind) in groups {
        for term in terms {
            if out.len() >= MAX_TERMS_PER_CHUNK {
                return out;
            }
            if seen.insert(term.clone()) {
                out.push((term, kind));
            }
        }
    }
    out
}

fn classify_word(word: &str, symbols: &mut Vec<String>, acronyms: &mut Vec<String>) {
    if is_snake(word) || is_camel(word) {
        symbols.push(word.to_lowercase());
    } else if is_acronym(word) {
        acronyms.push(word.to_lowercase());
    }
}

/// The identifier-shaped terms of a query, weighted for `search_postings`:
/// 3.0 for names, 2.0 for acronyms. Plain words are not returned.
pub fn analyze_query(q: &str) -> Vec<QueryIdent> {
    extract_identifiers(q)
        .into_iter()
        .map(|(term, kind)| QueryIdent {
            term,
            kind,
            weight: if kind == IdentKind::Acronym { 2.0 } else { 3.0 },
        })
        .collect()
}

/// Fewest digits of a bare number (no letters, no separators) that is read as
/// an identifier, not as a count or a year.
pub const EXACT_ID_MIN_DIGITS: usize = 8;
/// Fewest digits of a number written in `-`-separated groups.
const EXACT_ID_MIN_GROUPED_DIGITS: usize = 10;
/// Fewest consecutive digits an alphanumeric token needs to be an id.
const EXACT_ID_MIN_DIGIT_RUN: usize = 3;
/// Exact identifiers read from one query, so a pasted log cannot fan out.
const EXACT_ID_MAX_PER_QUERY: usize = 4;

fn is_uuid_token(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn longest_digit_run(token: &str) -> usize {
    let mut best = 0;
    let mut run = 0;
    for b in token.bytes() {
        if b.is_ascii_digit() {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Whether one whitespace-free token is an exact identifier: something a person
/// types to find that very record and nothing else.
///
/// - a bare run of [`EXACT_ID_MIN_DIGITS`] digits or more, or digits in
///   `-`-separated groups that total [`EXACT_ID_MIN_GROUPED_DIGITS`] or more;
/// - a UUID;
/// - letters and digits with a `-`, `_` or `:` between (`Bond-ZZHE87751`,
///   `INV_2024_0007`) holding a run of three digits or more, or a letter-and-digit
///   token of nine characters or more with a run of five (`ZZHE87751`).
///
/// Version strings, model names and paths do not qualify: a `.` or `/` is not a
/// separator here, and `gpt-4o-mini` or `utf-16` hold no run of three digits.
/// Commit SHAs are read by [`extract_identifiers`] (kind `Sha`), not here.
fn is_exact_id_token(token: &str) -> bool {
    if token.len() < 6 || token.len() > 80 || !token.is_ascii() {
        return false;
    }
    let digits = token.bytes().filter(u8::is_ascii_digit).count();
    let letters = token.bytes().filter(u8::is_ascii_alphabetic).count();
    if letters == 0 {
        if token.bytes().all(|b| b.is_ascii_digit()) {
            return digits >= EXACT_ID_MIN_DIGITS;
        }
        return token.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            && !token.starts_with('-')
            && !token.ends_with('-')
            && !token.contains("--")
            && digits >= EXACT_ID_MIN_GROUPED_DIGITS;
    }
    if is_uuid_token(token) {
        return true;
    }
    if digits == 0
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
    {
        return false;
    }
    let run = longest_digit_run(token);
    let separated = token.bytes().any(|b| matches!(b, b'-' | b'_' | b':'));
    if separated {
        run >= EXACT_ID_MIN_DIGIT_RUN
    } else {
        token.len() >= 9 && run >= 5
    }
}

/// The exact identifiers of `text` (a query), lowercased, deduplicated, in the
/// order they appear, at most four. Tokens are cut at whitespace and at
/// brackets, quotes and commas, and trimmed of edge punctuation. Plus the
/// commit SHAs [`extract_identifiers`] finds. Prose gives an empty list.
///
/// This is the one place that decides what an exact identifier is: the quick
/// check asks it for every query, and a `found` answer must then hold the
/// identifier literally ([`find_exact_id`]). It does not feed the postings
/// table or any ranking.
pub fn exact_id_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |id: String, out: &mut Vec<String>| {
        if out.len() < EXACT_ID_MAX_PER_QUERY && !out.contains(&id) {
            out.push(id);
        }
    };
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\''
            )
    }) {
        let token = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if is_exact_id_token(token) {
            push(token.to_ascii_lowercase(), &mut out);
        }
    }
    for (term, kind) in extract_identifiers(text) {
        if kind == IdentKind::Sha {
            push(term, &mut out);
        }
    }
    out
}

fn is_alnum_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// Byte offset of the first place `content` holds `id` as a whole identifier,
/// ignoring ASCII case: the characters before and after must not be letters or
/// digits, and a `-` or `_` after it must not lead into more letters or
/// digits. So `3256813322194603` is not found inside `32568133221946031`, and
/// `Bond-ZZHE87751` is neither found inside `Bond-ZZHE877512` nor
/// `Bond-ZZHE87751-B`. `id` is expected lowercase (see [`exact_id_tokens`]).
pub fn find_exact_id(content: &str, id: &str) -> Option<usize> {
    if id.is_empty() {
        return None;
    }
    let hay = content.to_ascii_lowercase();
    let bytes = hay.as_bytes();
    let mut from = 0;
    while let Some(found) = hay[from..].find(id) {
        let start = from + found;
        let end = start + id.len();
        let before_ok = start == 0 || !is_alnum_byte(bytes[start - 1]);
        let after_ok = match bytes.get(end) {
            None => true,
            Some(b) if is_alnum_byte(*b) => false,
            Some(b'-') | Some(b'_') => !bytes.get(end + 1).is_some_and(|n| is_alnum_byte(*n)),
            Some(_) => true,
        };
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + 1;
        while from < hay.len() && !hay.is_char_boundary(from) {
            from += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(text: &str) -> Vec<String> {
        extract_identifiers(text)
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    fn kind_of(text: &str, term: &str) -> Option<IdentKind> {
        extract_identifiers(text)
            .into_iter()
            .find(|(t, _)| t == term)
            .map(|(_, k)| k)
    }

    #[test]
    fn identifier_path_gets_basename_and_stem_aliases() {
        let got = extract_identifiers("edit csr-engine/src/storage/queries.rs please");
        assert!(got.contains(&("csr-engine/src/storage/queries.rs".into(), IdentKind::Path)));
        assert!(got.contains(&("queries.rs".into(), IdentKind::Path)));
        assert!(got.contains(&("queries".into(), IdentKind::Path)));
        assert_eq!(got[0].1, IdentKind::Path);
    }

    #[test]
    fn identifier_path_line_suffix_and_anchored_forms() {
        assert!(terms("see src/main.rs:120 now").contains(&"src/main.rs".to_string()));
        assert_eq!(
            kind_of("in /usr/local/bin today", "/usr/local/bin"),
            Some(IdentKind::Path)
        );
        assert_eq!(kind_of("see ./a/b.toml", "a/b.toml"), Some(IdentKind::Path));
        assert_eq!(
            kind_of("bare Cargo.toml here", "cargo.toml"),
            Some(IdentKind::Path)
        );
    }

    #[test]
    fn identifier_slash_prose_is_not_a_path() {
        let got = terms("and/or w/o input/output km/h 10/25/2026 TCP/IP");
        assert!(got.iter().all(|t| !t.contains('/')), "{got:?}");
    }

    #[test]
    fn identifier_urls_are_masked() {
        let got = terms("https://github.com/ramakay/claude-self-reflect/pull/295 ok");
        assert!(got.iter().all(|t| !t.contains("github")));
    }

    #[test]
    fn identifier_crate_names() {
        assert_eq!(
            kind_of("uses ast-grep for it", "ast-grep"),
            Some(IdentKind::Crate)
        );
        assert_eq!(
            kind_of("the id-token permission", "id-token"),
            Some(IdentKind::Crate)
        );
    }

    #[test]
    fn identifier_flags() {
        assert_eq!(
            kind_of("run with --no-verify now", "--no-verify"),
            Some(IdentKind::Flag)
        );
        // A flag is not also a crate.
        assert_eq!(kind_of("--long-flag", "long-flag"), None);
        assert_eq!(kind_of("--long-flag", "--long-flag"), Some(IdentKind::Flag));
    }

    #[test]
    fn identifier_sha_needs_digit_and_letter() {
        assert_eq!(
            kind_of("commit 0ac700f9 landed", "0ac700f9"),
            Some(IdentKind::Sha)
        );
        assert_eq!(
            kind_of(
                "c0b2fa6c1d2e3f405162738495a6b7c8d9e0f1a2",
                "c0b2fa6c1d2e3f405162738495a6b7c8d9e0f1a2"
            ),
            Some(IdentKind::Sha)
        );
        assert!(terms("decade defaced 1234567 deadbeef").is_empty());
        assert!(terms("1e100000").is_empty());
        // UUIDs are not SHAs.
        assert!(terms("id 3f2a9c1e-5b7d-4e8a-9c0b-1a2b3c4d5e6f ok").is_empty());
    }

    #[test]
    fn identifier_pr_numbers() {
        assert_eq!(
            kind_of("merged #295 today", "#295"),
            Some(IdentKind::PrNumber)
        );
        assert!(terms("html &#39; entity").is_empty());
    }

    #[test]
    fn identifier_acronyms_and_stopwords() {
        assert_eq!(kind_of("OIDC and HNSW", "oidc"), Some(IdentKind::Acronym));
        assert_eq!(kind_of("OIDC and HNSW", "hnsw"), Some(IdentKind::Acronym));
        assert!(terms("IT IS OK AND THE END")
            .iter()
            .all(|t| t != "it" && t != "ok"));
        assert!(terms("OVERLONGWORD").is_empty());
    }

    #[test]
    fn identifier_symbols_snake_camel_and_colon_paths() {
        assert_eq!(
            kind_of(
                "call insert_chunk_with_source()",
                "insert_chunk_with_source"
            ),
            Some(IdentKind::Symbol)
        );
        assert_eq!(kind_of("a getFoo call", "getfoo"), Some(IdentKind::Symbol));
        assert_eq!(
            kind_of("ConversationChunk struct", "conversationchunk"),
            Some(IdentKind::Symbol)
        );
        let got = terms("storage::queries::fts5_search_projects(...)");
        assert!(got.contains(&"storage::queries::fts5_search_projects".to_string()));
        assert!(got.contains(&"fts5_search_projects".to_string()));
        assert_eq!(
            kind_of("CSR_NO_DREAMING=1", "csr_no_dreaming"),
            Some(IdentKind::Symbol)
        );
    }

    #[test]
    fn identifier_prose_words_not_extracted() {
        assert!(
            terms("The quick brown fox jumps over the lazy dog. We fixed it today.").is_empty()
        );
        assert!(terms("Hello, World. This is a normal sentence about nothing.").is_empty());
    }

    #[test]
    fn identifier_terms_lowercased_and_deduplicated() {
        let got = terms("OIDC oidc OIDC src/A.rs src/a.rs");
        assert_eq!(got.iter().filter(|t| *t == "oidc").count(), 1);
        assert_eq!(got.iter().filter(|t| *t == "src/a.rs").count(), 1);
        assert!(got.iter().all(|t| *t == t.to_lowercase()));
    }

    #[test]
    fn identifier_cap_keeps_paths() {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("some_symbol_{i} "));
        }
        text.push_str("src/needle/target.rs");
        let got = extract_identifiers(&text);
        assert_eq!(got.len(), MAX_TERMS_PER_CHUNK);
        assert_eq!(got[0], ("src/needle/target.rs".into(), IdentKind::Path));
    }

    #[test]
    fn identifier_extraction_is_deterministic() {
        let text = "src/a/b.rs ast-grep --flag OIDC #12 deadbee1 foo_bar fooBar a::bcde";
        assert_eq!(extract_identifiers(text), extract_identifiers(text));
    }

    #[test]
    fn identifier_non_ascii_text_is_safe() {
        let got = terms("café résumé 日本語 src/ü/x.rs foo_bar");
        assert!(got.contains(&"foo_bar".to_string()));
    }

    #[test]
    fn identifier_analyze_query_weights() {
        let q = analyze_query("why does OIDC publish fail for src/lib.rs and fts5_search");
        let w = |t: &str| q.iter().find(|i| i.term == t).map(|i| i.weight);
        assert_eq!(w("oidc"), Some(2.0));
        assert_eq!(w("src/lib.rs"), Some(3.0));
        assert_eq!(w("fts5_search"), Some(3.0));
        assert!(q.iter().all(|i| i.term != "why" && i.term != "publish"));
    }

    #[test]
    fn exact_ids_are_long_digit_runs_and_separated_alphanumeric_tokens() {
        assert_eq!(
            exact_id_tokens("3256813322194603"),
            vec!["3256813322194603".to_string()]
        );
        assert_eq!(
            exact_id_tokens("did we see Bond-ZZHE87751 before?"),
            vec!["bond-zzhe87751".to_string()]
        );
        assert_eq!(
            exact_id_tokens("ticket INV_2024_0007, and ZZHE87751."),
            vec!["inv_2024_0007".to_string(), "zzhe87751".to_string()]
        );
        assert_eq!(
            exact_id_tokens("call 555-123-4567"),
            vec!["555-123-4567".to_string()]
        );
        assert_eq!(
            exact_id_tokens("commit 568a35da landed"),
            vec!["568a35da".to_string()]
        );
        assert_eq!(
            exact_id_tokens("run 3f2a9c1e-5b7d-4e11-9c3a-0a1b2c3d4e5f"),
            vec!["3f2a9c1e-5b7d-4e11-9c3a-0a1b2c3d4e5f".to_string()]
        );
    }

    #[test]
    fn prose_versions_dates_and_model_names_are_not_exact_ids() {
        for text in [
            "why did the release fail",
            "upgrade to python 3.11.4 today",
            "gpt-4o-mini versus claude-3-5-sonnet",
            "utf-16 and x86-64 builds",
            "meeting on 2026-11-17",
            "1234567 items in 2025",
            "src/lib.rs and fts5_search",
        ] {
            assert!(exact_id_tokens(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn exact_ids_cap_at_four_per_query() {
        let text = "11111111 22222222 33333333 44444444 55555555";
        assert_eq!(exact_id_tokens(text).len(), 4);
    }

    #[test]
    fn find_exact_id_respects_whole_identifier_boundaries() {
        let id = "3256813322194603";
        assert!(find_exact_id("order 3256813322194603 shipped", id).is_some());
        assert!(find_exact_id("order #3256813322194603.", id).is_some());
        // A longer run, a near number and a number inside a word are not it.
        assert!(find_exact_id("32568133221946031", id).is_none());
        assert!(find_exact_id("13256813322194603", id).is_none());
        assert!(find_exact_id("3256813322194604", id).is_none());
        assert!(find_exact_id("x3256813322194603y", id).is_none());
        assert!(find_exact_id("88824670039", id).is_none());

        let id = "bond-zzhe87751";
        assert!(find_exact_id("see BOND-ZZHE87751 now", id).is_some());
        assert!(find_exact_id("Bond-ZZHE877512", id).is_none());
        assert!(find_exact_id("Bond-ZZHE87751-B", id).is_none());
        assert!(find_exact_id("XBond-ZZHE87751", id).is_none());
        assert!(find_exact_id("Bond ZZHE87751", id).is_none());
        assert!(find_exact_id("Bond-ZZHE8775", id).is_none());
        // A later whole occurrence is found after an earlier near miss.
        let text = "Bond-ZZHE877512 then Bond-ZZHE87751 ok";
        assert_eq!(
            find_exact_id(text, id),
            Some(text.find("Bond-ZZHE87751 ok").unwrap())
        );
        assert!(find_exact_id("日本語 Bond-ZZHE87751", id).is_some());
    }
}
