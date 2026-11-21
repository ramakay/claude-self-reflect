//! Shift Map grammar parser and signature hashing for session stories.
//!
//! Pure functions only (no I/O, no DB). Splits the optional `@shifts` block
//! from Haiku's story output so prose storage, search, and recaps remain
//! unchanged while valid shift entries are parsed for grounding.

use serde::Serialize;
use sha2::{Digest, Sha256};

// Sized for four full 5-field lines (`kind|before|after|was|now`). A line is at most
// 10 (kind) + 138 (two sides at the longest measured, 69 each) + 160 (was and now at 80
// chars) + 4 pipes = 312 bytes, 313 with its newline; four of them are 1252, so the block
// cap is 1280. The line cap keeps the old 4-field line's headroom over its sides (300 held
// a 72-char claim, so sides could be 215) and swaps the claim for two sentences:
// 300 - 72 + 80 + 80 + 1 = 389, rounded to 400.
const MAX_BLOCK_BYTES: usize = 1280;
const MAX_LINE_BYTES: usize = 400;
const MAX_ENTRIES: usize = 4;
const MAX_SCOPE_CHARS: usize = 48;
const MAX_SYM_CHARS: usize = 40;
const MAX_CLAIM_CHARS: usize = 72;
/// A `was` or `now` sentence: at most this many characters and words.
const MAX_SENTENCE_CHARS: usize = 80;
const MAX_SENTENCE_WORDS: usize = 12;

pub mod derive;
pub mod ground;
pub mod likes;
pub mod store;

/// Kill switch (`CSR_NO_STORY_MAP=1`): disables `@shifts` prompt requests,
/// ledger writes, and Shift Map rendering.
pub fn story_map_disabled() -> bool {
    std::env::var("CSR_NO_STORY_MAP")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Kind of architecture shift entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShiftKind {
    Shift,
    Risk,
    Decision,
    Regression,
}

impl ShiftKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shift => "shift",
            Self::Risk => "risk",
            Self::Decision => "decision",
            Self::Regression => "regression",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shift" => Some(Self::Shift),
            "risk" => Some(Self::Risk),
            "decision" => Some(Self::Decision),
            "regression" => Some(Self::Regression),
            _ => None,
        }
    }
}

/// Connector operator between two endpoints on a side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EdgeOp {
    #[serde(rename = "->")]
    Calls,
    #[serde(rename = "~>")]
    Flows,
    #[serde(rename = "-x->")]
    Severed,
}

impl EdgeOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "->",
            Self::Flows => "~>",
            Self::Severed => "-x->",
        }
    }
}

/// A single node endpoint: `scope[:symbol][state_mark]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Endpoint {
    pub scope: String,
    pub sym: Option<String>,
    pub mark: Option<char>,
}

impl Endpoint {
    pub fn canonical(&self) -> String {
        let mut out = self.scope.clone();
        if let Some(ref s) = self.sym {
            out.push(':');
            out.push_str(s);
        }
        if let Some(m) = self.mark {
            out.push(m);
        }
        out
    }
}

/// One side (`before` or `after`) of a shift transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Side {
    pub from: Endpoint,
    pub edge: Option<(EdgeOp, Endpoint)>,
}

impl Side {
    /// Renders the side back to canonical grammar text with single spaces.
    pub fn canonical(&self) -> String {
        match &self.edge {
            Some((op, to)) => format!(
                "{} {} {}",
                self.from.canonical(),
                op.as_str(),
                to.canonical()
            ),
            None => self.from.canonical(),
        }
    }

    /// Returns the 1 or 2 endpoints on this side in order.
    pub fn endpoints(&self) -> Vec<&Endpoint> {
        match &self.edge {
            Some((_, to)) => vec![&self.from, to],
            None => vec![&self.from],
        }
    }
}

/// A parsed `@shifts` line: `kind|before|after|was|now`, or the older
/// `kind|before|after|claim`.
///
/// `was` is how the thing worked before the change (`None` when the line said `-`, the
/// thing is new, and for every 4-field line); `now` is how it works now (`None` only for
/// a 4-field line). `claim` is `now` for a 5-field line and the line's own claim for a
/// 4-field one, so everything that reads `claim` keeps working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShiftEntry {
    pub kind: ShiftKind,
    pub before: Option<Side>,
    pub after: Option<Side>,
    pub claim: String,
    pub was: Option<String>,
    pub now: Option<String>,
}

impl ShiftEntry {
    /// Renders an optional side to its canonical string (`"-"` when `None`).
    pub fn render_side(side: &Option<Side>) -> String {
        match side {
            Some(s) => s.canonical(),
            None => "-".to_string(),
        }
    }
}

fn is_scope_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '@' | '-')
}

fn is_sym_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.')
}

fn parse_endpoint(raw: &str) -> Option<Endpoint> {
    let s = raw.trim();
    if s.is_empty() || s == "-" {
        return None;
    }
    let (body, mark) = match s.chars().last()? {
        m @ ('+' | '~' | '-' | '!') => (&s[..s.len() - m.len_utf8()], Some(m)),
        _ => (s, None),
    };
    if body.is_empty() {
        return None;
    }
    let (scope_str, sym_opt) = match body.split_once(':') {
        Some((sc, sy)) => (sc, Some(sy)),
        None => (body, None),
    };
    let scope_len = scope_str.chars().count();
    if !(1..=MAX_SCOPE_CHARS).contains(&scope_len) || !scope_str.chars().all(is_scope_char) {
        return None;
    }
    let sym = match sym_opt {
        Some(sy) => {
            let sym_len = sy.chars().count();
            if !(1..=MAX_SYM_CHARS).contains(&sym_len) || !sy.chars().all(is_sym_char) {
                return None;
            }
            Some(sy.to_string())
        }
        None => None,
    };
    Some(Endpoint {
        scope: scope_str.to_string(),
        sym,
        mark,
    })
}

fn split_edge_op(s: &str) -> Option<(&str, EdgeOp, &str)> {
    for (pat, op) in [
        (" -x-> ", EdgeOp::Severed),
        (" -> ", EdgeOp::Calls),
        (" ~> ", EdgeOp::Flows),
    ] {
        if let Some((left, right)) = s.split_once(pat) {
            return Some((left, op, right));
        }
    }
    None
}

fn parse_side(raw: &str) -> Result<Option<Side>, ()> {
    let s = raw.trim();
    if s == "-" {
        return Ok(None);
    }
    if s.is_empty() {
        return Err(());
    }
    if let Some((left, op, right)) = split_edge_op(s) {
        let from = parse_endpoint(left).ok_or(())?;
        let to = parse_endpoint(right).ok_or(())?;
        Ok(Some(Side {
            from,
            edge: Some((op, to)),
        }))
    } else {
        let from = parse_endpoint(s).ok_or(())?;
        Ok(Some(Side { from, edge: None }))
    }
}

/// Words a cut sentence must not end on: a sentence that stops after one reads as broken.
const DANGLING_WORDS: [&str; 17] = [
    "a", "an", "the", "of", "to", "in", "on", "at", "and", "or", "with", "by", "for", "from", "as",
    "that", "is",
];

/// One `was`/`now` sentence: plain printable ASCII, whitespace collapsed, cut at a word
/// boundary to `MAX_SENTENCE_WORDS` words and `MAX_SENTENCE_CHARS` characters (never
/// mid-word, never ending on a dangling article or preposition). `None` when it is empty,
/// not printable ASCII, or the model's `-` placeholder.
fn parse_sentence(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw == "-" || !raw.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return None;
    }
    let mut words: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for word in raw.split_whitespace() {
        let next = used + word.len() + usize::from(!words.is_empty());
        if words.len() >= MAX_SENTENCE_WORDS || next > MAX_SENTENCE_CHARS {
            while let Some(last) = words.last() {
                let bare = last.trim_matches(|c: char| !c.is_ascii_alphanumeric());
                if DANGLING_WORDS.contains(&bare.to_ascii_lowercase().as_str()) {
                    words.pop();
                } else {
                    break;
                }
            }
            break;
        }
        words.push(word);
        used = next;
    }
    let sentence = words.join(" ");
    let sentence = sentence.trim_end_matches([',', ';', ':']).to_string();
    (!sentence.is_empty()).then_some(sentence)
}

/// Parses a single pipe-delimited shift line: `kind|before|after|was|now`, or the older
/// `kind|before|after|claim`.
///
/// `was` is `-` for a thing that is new. A sentence over 80 characters or 12 words is cut
/// at a word boundary. A pipe inside a 4-field claim makes it a 5-field line; a sixth
/// field makes the line invalid.
pub fn parse_shift_line(line: &str) -> Option<ShiftEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_LINE_BYTES {
        return None;
    }
    let mut parts = trimmed.splitn(5, '|');
    let kind = ShiftKind::parse(parts.next()?)?;
    let before = parse_side(parts.next()?).ok()?;
    let after = parse_side(parts.next()?).ok()?;
    let fourth = parts.next()?.trim();
    let fifth = parts.next().map(str::trim);

    if before.is_none() && after.is_none() {
        return None;
    }
    if let Some(now_raw) = fifth {
        if fourth.is_empty() || now_raw.contains('|') {
            return None;
        }
        let was = parse_sentence(fourth);
        if was.is_none() && fourth != "-" {
            return None;
        }
        let now = parse_sentence(now_raw)?;
        return Some(ShiftEntry {
            kind,
            before,
            after,
            claim: now.clone(),
            was,
            now: Some(now),
        });
    }
    if fourth.is_empty() || !fourth.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return None;
    }
    let claim_cut: String = fourth.chars().take(MAX_CLAIM_CHARS).collect();
    let claim = claim_cut.trim_end().to_string();
    if claim.is_empty() {
        return None;
    }
    Some(ShiftEntry {
        kind,
        before,
        after,
        claim,
        was: None,
        now: None,
    })
}

fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if s.len() <= max_bytes {
        return s.len();
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Parses the lines following an `@shifts` sentinel.
///
/// When `cut_off` is true or the block cap (`MAX_BLOCK_BYTES`) cuts a non-whitespace tail,
/// the last non-empty line is dropped. Keeps at most 4 unique `(kind, before, after)` entries.
pub fn parse_shifts_block(block: &str, cut_off: bool) -> Vec<ShiftEntry> {
    let cap = floor_char_boundary(block, MAX_BLOCK_BYTES);
    let capped = &block[..cap];
    let block_cut_line =
        cap < block.len() && !block[cap..].trim().is_empty() && !capped.ends_with('\n');

    let mut non_empty: Vec<&str> = capped
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if (cut_off || block_cut_line) && !non_empty.is_empty() {
        non_empty.pop();
    }

    let mut out: Vec<ShiftEntry> = Vec::with_capacity(MAX_ENTRIES);
    for line in non_empty {
        if let Some(entry) = parse_shift_line(line) {
            let dup = out.iter().any(|e| {
                e.kind == entry.kind && e.before == entry.before && e.after == entry.after
            });
            if !dup {
                out.push(entry);
                if out.len() >= MAX_ENTRIES {
                    break;
                }
            }
        }
    }
    out
}

/// Whether one line is the `@shifts` sentinel, allowing the decoration a model
/// puts around it. A line that merely mentions `@shifts` is not one.
fn is_shifts_sentinel(line: &str) -> bool {
    line.trim_end_matches(['\r', '\n'])
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '#' | '*' | '`'))
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '*' | '`'))
        .eq_ignore_ascii_case("@shifts")
}

/// Splits raw story model output into `(prose, shifts)`.
///
/// If a sentinel line is present (`@shifts`, case-insensitive, once whitespace,
/// leading `#`/`*`/`` ` `` and trailing `:`/`*`/`` ` `` are trimmed off, so
/// `@shifts:`, `**@shifts**`, `### @shifts` and `@Shifts` count), returns the prose
/// before that line (trailing whitespace trimmed) and parsed entries from the
/// remainder. If absent, returns `(raw.to_string(), vec![])` byte-for-byte.
pub fn split_story_output(raw: &str, cut_off: bool) -> (String, Vec<ShiftEntry>) {
    let mut offset = 0usize;
    for segment in raw.split_inclusive('\n') {
        if is_shifts_sentinel(segment) {
            let prose = raw[..offset].trim_end().to_string();
            let rest = &raw[offset + segment.len()..];
            return (prose, parse_shifts_block(rest, cut_off));
        }
        offset += segment.len();
    }
    (raw.to_string(), Vec::new())
}

/// Computes the 16-hex-char lowercase SHA-256 signature for deduplication:
/// `sha256(project + "|" + kind + "|" + canonical(before) + "|" + canonical(after))[0..16]`.
pub fn sig(project: &str, entry: &ShiftEntry) -> String {
    let before = ShiftEntry::render_side(&entry.before);
    let after = ShiftEntry::render_side(&entry.after);
    let input = format!("{project}|{}|{before}|{after}", entry.kind.as_str());
    let digest = Sha256::digest(input.as_bytes());
    let mut s = String::with_capacity(16);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
        if s.len() >= 16 {
            break;
        }
    }
    s.truncate(16);
    s
}

/// The story prose is at most 800 characters (the compaction prompt asks for under 800), then
/// the `@shifts` line and a block of four full 5-field lines (`MAX_BLOCK_BYTES`, 1280):
/// 800 + 9 + 1280 = 2089, rounded to 2100. Was 1600, which held the 900-byte block only
/// after a prose of 700.
pub const RAW_CAP_WITH_MAP: usize = 2100;
pub const STORY_PROSE_CAP: usize = 1000;

/// Prompt continuation appended after the story prose instruction when the
/// Shift Map is enabled (`!story_map_disabled()`).
pub fn shifts_instruction() -> &'static str {
    "After the prose, if the session changed code, add a newline, the line `@shifts`, and 1 to 4 lines in this exact 5-field pipe format:\n\
kind|before|after|was|now\n\
- kind: shift | risk | decision | regression\n\
- before, after: `-` if empty, or `file[:symbol][mark]` optionally joined to a second endpoint by ` -> `, ` ~> `, or ` -x-> ` (marks: `+` added, `~` changed, `-` removed, `!` risk). Use only real file or symbol names from the session.\n\
- was: one sentence on how it worked before the change, or `-` if the thing is new.\n\
- now: one sentence on how it works now.\n\
Write was and now in Simplified Technical English: plain ASCII, no `|`, at most 80 characters and 12 words each, present tense, active voice, one fact per sentence, short common words. Name only real files or symbols from the session. Never write \"it\" or \"this\" for the thing: name it.\n\
Put `@shifts` last. Write no `@shifts` block if no code changed.\n\
Example:\n\
@shifts\n\
shift|pane.py:draw_story_map-|pane.py:shift_map_rows~|The map drew its labels at 9 to 10 px.|The map writes Before and After as text.\n\
decision|-|summarizer.rs:ground_shifts+ -> code_evolution|-|ground_shifts checks each shift against AST evolution rows."
}

/// Applies the capture cap, splits the optional `@shifts` block when `map_on`
/// is true, and caps the returned story prose at `STORY_PROSE_CAP` (1000) chars.
///
/// When `!map_on`, returns the first 1000 characters of `raw_full` unchanged
/// and an empty shift list.
pub fn finish_story_output(raw_full: &str, map_on: bool) -> (String, Vec<ShiftEntry>) {
    if !map_on {
        return (raw_full.chars().take(STORY_PROSE_CAP).collect(), Vec::new());
    }
    let mut chars = raw_full.chars();
    let raw: String = chars.by_ref().take(RAW_CAP_WITH_MAP).collect();
    let cut_off = chars.next().is_some();
    let (prose, entries) = split_story_output(&raw, cut_off);
    let prose_capped: String = prose.chars().take(STORY_PROSE_CAP).collect();
    (prose_capped, entries)
}

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "We split shift grounding out of store_story in summarizer.rs so each compaction appends verified architecture shifts without deleting prior shift reflections. session_start.rs now passes edited file stems into the story prompt, and pane.py renders the before-after map above the story text. Where it stands: engine storage tests pass, pending pane snapshot checks.\n@shifts\nshift|summarizer.rs:store_story~ -> reflections-|summarizer.rs:store_story~ -> reflections:narrative_shift+|Keep prior shift reflections on story rewrite\ndecision|-|summarizer.rs:ground_shifts+ -> code_evolution|Check Haiku shift endpoints against AST evolution rows\nrisk|pane.py:session_story -> reflections|pane.py:session_story~ -> meta:story_map+|Pane must fail open when story_map meta key is absent\n";

    #[test]
    fn parses_the_three_example_lines() {
        let (prose, entries) = split_story_output(EXAMPLE, false);
        assert!(prose.starts_with("We split shift grounding"));
        assert!(!prose.contains("@shifts"));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, ShiftKind::Shift);
        assert_eq!(
            ShiftEntry::render_side(&entries[0].before),
            "summarizer.rs:store_story~ -> reflections-"
        );
        assert_eq!(
            ShiftEntry::render_side(&entries[0].after),
            "summarizer.rs:store_story~ -> reflections:narrative_shift+"
        );
        assert_eq!(entries[1].kind, ShiftKind::Decision);
        assert!(entries[1].before.is_none());
        assert_eq!(
            entries[1].after.as_ref().map(|s| s.endpoints().len()),
            Some(2)
        );
        assert_eq!(entries[2].kind, ShiftKind::Risk);
    }

    #[test]
    fn valid_four_lines_all_kept() {
        let block = "shift|a.rs:f1~|a.rs:f2+|c1\nrisk|b.rs|c.rs!|c2\ndecision|-|d.rs:g+|c3\nregression|e.rs:h-|e.rs:h~|c4\n";
        let entries = parse_shifts_block(block, false);
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn fifth_valid_line_is_dropped() {
        let block = "shift|a.rs|b.rs|1\nshift|c.rs|d.rs|2\nshift|e.rs|f.rs|3\nshift|g.rs|h.rs|4\nshift|i.rs|j.rs|5\n";
        let entries = parse_shifts_block(block, false);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[3].claim, "4");
    }

    #[test]
    fn missing_sentinel_returns_input_unchanged() {
        let raw = "  Untrimmed prose with trailing newline and spaces.  \n\n";
        let (prose, entries) = split_story_output(raw, false);
        assert_eq!(prose, raw);
        assert!(entries.is_empty());
    }

    #[test]
    fn decorated_sentinel_variants_still_cut_the_block_from_the_prose() {
        let block = "shift|a.rs:f~|a.rs:g+|Keep the map\n";
        for variant in [
            "@shifts:",
            "**@shifts**",
            "`@shifts`",
            "### @shifts",
            "@Shifts",
            "@SHIFTS",
            "**@shifts:**",
            "  ## `@Shifts`:  ",
        ] {
            let raw = format!("The story prose.\n{variant}\n{block}");
            let (prose, entries) = split_story_output(&raw, false);
            assert_eq!(prose, "The story prose.", "variant {variant:?}");
            assert_eq!(entries.len(), 1, "variant {variant:?}");
            let (capped, _) = finish_story_output(&raw, true);
            assert!(
                !capped.contains('|'),
                "variant {variant:?} leaked: {capped}"
            );
        }
    }

    #[test]
    fn a_line_that_only_mentions_shifts_is_not_a_sentinel() {
        for raw in [
            "We read @shifts from the model output and kept going.\nshift|a.rs|b.rs|c\n",
            "See `@shifts` for the format.\n",
            "The @shifts: block is optional.\n",
            "@shifts and more\nshift|a.rs|b.rs|c\n",
            "@shiftsX\n",
        ] {
            let (prose, entries) = split_story_output(raw, false);
            assert_eq!(prose, raw);
            assert!(entries.is_empty());
        }
    }

    #[test]
    fn the_kill_switch_path_is_untouched_by_decorated_sentinels() {
        let raw = "Prose.\n**@shifts**\nshift|a.rs|b.rs|c\n";
        let (prose, entries) = finish_story_output(raw, false);
        assert_eq!(prose, raw);
        assert!(entries.is_empty());
    }

    #[test]
    fn sentinel_with_junk_block_strips_prose_and_gives_no_entries() {
        let raw =
            "Clean story prose.   \r\n@shifts\r\nnot|a|valid|line|at|all\r\nbad_kind|a|b|c\r\n";
        let (prose, entries) = split_story_output(raw, false);
        assert_eq!(prose, "Clean story prose.");
        assert!(entries.is_empty());
    }

    #[test]
    fn cut_off_last_line_is_dropped() {
        let block = "shift|a.rs|b.rs|kept\nshift|c.rs|d.rs|truncated_maybe";
        let entries = parse_shifts_block(block, true);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].claim, "kept");
    }

    #[test]
    fn cut_off_false_keeps_last_line() {
        let block = "shift|a.rs|b.rs|kept\nshift|c.rs|d.rs|also_kept";
        let entries = parse_shifts_block(block, false);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].claim, "also_kept");
    }

    #[test]
    fn a_pipe_in_a_four_field_claim_makes_a_five_field_line_and_a_sixth_field_is_invalid() {
        // `claim with | extra pipe` is, by the grammar, was `claim with` and now `extra pipe`.
        let five = parse_shift_line("shift|a.rs|b.rs|claim with | extra pipe").expect("5 fields");
        assert_eq!(five.was.as_deref(), Some("claim with"));
        assert_eq!(five.now.as_deref(), Some("extra pipe"));
        assert!(parse_shift_line("shift|a.rs|b.rs|one|two|three").is_none());
    }

    #[test]
    fn invalid_kind_dropped() {
        assert!(parse_shift_line("note|a.rs|b.rs|valid claim").is_none());
    }

    #[test]
    fn both_sides_empty_dropped() {
        assert!(parse_shift_line("shift|-|-|nothing happened").is_none());
    }

    #[test]
    fn non_ascii_claim_dropped() {
        assert!(parse_shift_line("shift|a.rs|b.rs|non-ascii arrow → here").is_none());
    }

    #[test]
    fn long_claim_cut_at_72() {
        let long = format!("shift|a.rs|b.rs|{}   ", "a".repeat(90));
        let entry = parse_shift_line(&long).expect("valid");
        assert_eq!(entry.claim.len(), 72);
    }

    #[test]
    fn duplicate_entries_kept_once() {
        let block =
            "shift|a.rs:foo~|b.rs:bar+|First claim\nshift|a.rs:foo~|b.rs:bar+|Second claim\n";
        let entries = parse_shifts_block(block, false);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].claim, "First claim");
    }

    #[test]
    fn oversize_line_dropped() {
        let huge = format!("shift|a.rs|b.rs|{}", "x".repeat(MAX_LINE_BYTES));
        assert!(parse_shift_line(&huge).is_none());
    }

    #[test]
    fn sig_is_stable_and_16_hex() {
        let e1 = parse_shift_line("shift|a.rs:f~ -> b.rs-|a.rs:f~ -> c.rs+|claim one").expect("e1");
        let e2 = parse_shift_line("shift|a.rs:f~ -> b.rs-|a.rs:f~ -> c.rs+|different claim")
            .expect("e2");
        let s1 = sig("csr", &e1);
        let s2 = sig("csr", &e2);
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), 16);
        assert!(s1
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn sig_differs_by_project() {
        let e = parse_shift_line("shift|a.rs|b.rs|claim").expect("e");
        assert_ne!(sig("proj_a", &e), sig("proj_b", &e));
    }

    #[test]
    fn kill_switch_reads_env() {
        let _guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_STORY_MAP");
        assert!(!story_map_disabled());
        std::env::set_var("CSR_NO_STORY_MAP", "1");
        assert!(story_map_disabled());
        std::env::set_var("CSR_NO_STORY_MAP", "TRUE");
        assert!(story_map_disabled());
        std::env::set_var("CSR_NO_STORY_MAP", "0");
        assert!(!story_map_disabled());
        std::env::remove_var("CSR_NO_STORY_MAP");
    }

    #[test]
    fn finish_output_without_block_equals_todays_cap() {
        let long_no_sentinel: String = "abcd ".repeat(300); // 1500 chars
        assert_eq!(long_no_sentinel.chars().count(), 1500);
        let (on_prose, on_entries) = finish_story_output(&long_no_sentinel, true);
        let (off_prose, off_entries) = finish_story_output(&long_no_sentinel, false);
        assert!(on_entries.is_empty());
        assert!(off_entries.is_empty());
        assert_eq!(on_prose.chars().count(), 1000);
        assert_eq!(on_prose, off_prose);

        let short_no_sentinel: String = "xyzw ".repeat(80); // 400 chars
        assert_eq!(short_no_sentinel.chars().count(), 400);
        let (on_short, on_short_entries) = finish_story_output(&short_no_sentinel, true);
        let (off_short, off_short_entries) = finish_story_output(&short_no_sentinel, false);
        assert!(on_short_entries.is_empty());
        assert!(off_short_entries.is_empty());
        assert_eq!(on_short, short_no_sentinel);
        assert_eq!(on_short, off_short);
    }

    #[test]
    fn finish_output_splits_prose_from_block() {
        let (prose, entries) = finish_story_output(EXAMPLE, true);
        assert!(prose.starts_with("We split shift grounding"));
        assert!(!prose.contains("@shifts"));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn finish_output_cut_off_at_the_raw_cap_drops_the_last_line() {
        let prose = "p".repeat(1500);
        let block =
            "\n@shifts\nshift|a.rs:f1~|b.rs:f2+|First complete claim\nshift|c.rs:g1~|d.rs:g2+|";
        let total = RAW_CAP_WITH_MAP + 100;
        let tail = "x".repeat(total - prose.len() - block.len());
        let raw_full = format!("{prose}{block}{tail}");
        assert_eq!(raw_full.chars().count(), total);

        let (out_prose, entries) = finish_story_output(&raw_full, true);
        assert_eq!(out_prose.chars().count(), 1000);
        assert!(out_prose.chars().all(|c| c == 'p'));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].claim, "First complete claim");
    }

    #[test]
    fn finish_output_bad_block_is_prose_only() {
        let raw = "The session fixed the parser.\n@shifts\nnot_a_kind|a|b|c\nshift|-|-|empty\n";
        let (prose, entries) = finish_story_output(raw, true);
        assert_eq!(prose, "The session fixed the parser.");
        assert!(entries.is_empty());
    }

    #[test]
    fn finish_output_map_off_returns_block_text_as_today() {
        let (prose, entries) = finish_story_output(EXAMPLE, false);
        assert!(entries.is_empty());
        assert!(prose.contains("@shifts"));
        let expected: String = EXAMPLE.chars().take(1000).collect();
        assert_eq!(prose, expected);
    }

    #[test]
    fn finish_output_caps_prose_in_chars_not_bytes() {
        let multibyte_prose = "é".repeat(1200); // 1200 chars, 2400 bytes
        let raw = format!("{multibyte_prose}\n@shifts\nshift|a.rs:f~|b.rs:g+|Ascii claim\n");
        let (prose, entries) = finish_story_output(&raw, true);
        assert_eq!(prose.chars().count(), 1000);
        assert_eq!(prose.len(), 2000);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].claim, "Ascii claim");
    }

    const SAMPLE_WAS: &str = "The map drew its labels at 9 to 10 px.";
    const SAMPLE_NOW: &str = "The map writes Before and After as text.";

    fn five(was: &str, now: &str) -> String {
        format!("shift|pane.py:draw_story_map-|pane.py:shift_map_rows~|{was}|{now}")
    }

    #[test]
    fn five_field_line_carries_was_and_now_and_claim_is_now() {
        let e = parse_shift_line(&five(SAMPLE_WAS, SAMPLE_NOW)).expect("valid");
        assert_eq!(e.was.as_deref(), Some(SAMPLE_WAS));
        assert_eq!(e.now.as_deref(), Some(SAMPLE_NOW));
        assert_eq!(e.claim, SAMPLE_NOW);
        assert_eq!(e.kind, ShiftKind::Shift);
    }

    #[test]
    fn a_dash_was_means_the_thing_is_new() {
        let e = parse_shift_line(&five("-", SAMPLE_NOW)).expect("valid");
        assert_eq!(e.was, None);
        assert_eq!(e.now.as_deref(), Some(SAMPLE_NOW));
        assert_eq!(e.claim, SAMPLE_NOW);
    }

    #[test]
    fn the_four_field_line_still_parses_without_was_and_now() {
        let e = parse_shift_line("shift|a.rs:f~|a.rs:g+|Keep the map").expect("valid");
        assert_eq!((e.was, e.now), (None, None));
        assert_eq!(e.claim, "Keep the map");
    }

    #[test]
    fn five_field_lines_reject_junk_in_either_sentence() {
        for (was, now) in [
            ("", SAMPLE_NOW),
            (SAMPLE_WAS, ""),
            (SAMPLE_WAS, "-"),
            ("Dropped the arrow \u{2192} here.", SAMPLE_NOW),
            (SAMPLE_WAS, "Tab\there is not printable."),
        ] {
            assert!(
                parse_shift_line(&five(was, now)).is_none(),
                "{was:?} {now:?}"
            );
        }
    }

    #[test]
    fn an_over_long_sentence_is_cut_at_a_word_boundary_within_80_chars_and_12_words() {
        let long_chars = "The summarizer writes the story prose and then the shift lines for every compaction it sees today.";
        assert!(long_chars.len() > MAX_SENTENCE_CHARS);
        let e = parse_shift_line(&five(SAMPLE_WAS, long_chars)).expect("kept, cut");
        let now = e.now.expect("now");
        assert!(now.len() <= MAX_SENTENCE_CHARS, "{now}");
        assert!(
            now.split_whitespace().count() <= MAX_SENTENCE_WORDS,
            "{now}"
        );
        assert!(long_chars.starts_with(&now), "{now}");
        assert_eq!(e.claim, now);
        // 14 words, short: the word cap binds, and the cut does not end on "the".
        let long_words =
            "It reads one two three four five six seven eight nine ten eleven twelve words.";
        let e = parse_shift_line(&five(long_words, SAMPLE_NOW)).expect("kept, cut");
        assert_eq!(
            e.was.as_deref().map(|w| w.split_whitespace().count()),
            Some(12)
        );
        let dangling =
            "Each shift line is checked against all of the code edits in the new rows now";
        let e = parse_shift_line(&five(dangling, SAMPLE_NOW)).expect("kept, cut");
        assert_eq!(
            e.was.as_deref(),
            Some("Each shift line is checked against all of the code edits")
        );
    }

    #[test]
    fn sig_ignores_was_and_now_so_a_rewording_does_not_churn_the_map() {
        let four = parse_shift_line("shift|a.rs:f~|a.rs:g+|old claim").expect("4");
        let five1 = parse_shift_line("shift|a.rs:f~|a.rs:g+|It was slow.|It is fast.").expect("5");
        let five2 =
            parse_shift_line("shift|a.rs:f~|a.rs:g+|It lagged.|It runs quickly.").expect("5");
        assert_eq!(sig("csr", &four), sig("csr", &five1));
        assert_eq!(sig("csr", &five1), sig("csr", &five2));
        let block = "shift|a.rs:f~|a.rs:g+|It was slow.|It is fast.\nshift|a.rs:f~|a.rs:g+|It lagged.|It runs quickly.\n";
        assert_eq!(parse_shifts_block(block, false).len(), 1);
    }

    #[test]
    fn a_story_with_four_full_five_field_lines_is_kept_whole() {
        // The longest sides measured on stored maps are 69 characters; sentences at the cap.
        let side_b = format!(
            "{}:{}~",
            "csr-engine/src/story_map/store.rs", "record_story_map"
        );
        let side_a = format!(
            "{} -> {}",
            "csr-engine/src/story_map.rs:parse_shifts_block", "ledger.rs:plan+"
        );
        assert!(
            side_b.len() <= 69 && side_a.len() <= 69,
            "{} {}",
            side_b.len(),
            side_a.len()
        );
        let sentence =
            "Summarizer keeps four shifts, each with two plain sentences about code changes.";
        assert!(sentence.len() <= MAX_SENTENCE_CHARS);
        let words: Vec<&str> = sentence.split_whitespace().collect();
        assert!(words.len() <= MAX_SENTENCE_WORDS, "{}", words.len());
        let line = |kind: &str| format!("{kind}|{side_b}|{side_a}|{sentence}|{sentence}");
        let lines = ["shift", "decision", "regression", "risk"].map(line);
        assert!(lines.iter().all(|l| l.len() <= MAX_LINE_BYTES));
        let block = lines.join("\n") + "\n";
        assert!(block.len() <= MAX_BLOCK_BYTES, "{} bytes", block.len());
        let prose = "p".repeat(800);
        let raw = format!("{prose}\n@shifts\n{block}");
        assert!(
            raw.chars().count() <= RAW_CAP_WITH_MAP,
            "{}",
            raw.chars().count()
        );
        // Same four lines, distinguished so the dedupe keeps them all.
        let distinct = ["shift", "decision", "regression", "risk"]
            .iter()
            .enumerate()
            .map(|(i, kind)| {
                let side_a = side_a.replace("plan", &format!("plan{i}"));
                format!("{kind}|{side_b}|{side_a}|{sentence}|{sentence}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let raw = format!("{prose}\n@shifts\n{distinct}\n");
        let (out_prose, entries) = finish_story_output(&raw, true);
        assert_eq!(out_prose.len(), 800);
        assert_eq!(entries.len(), 4, "all four lines survive the caps");
    }

    #[test]
    fn the_prompts_own_example_parses_and_asks_for_five_fields() {
        let prompt = shifts_instruction();
        assert!(prompt.contains("kind|before|after|was|now"));
        let from = prompt.find("@shifts\n").expect("example sentinel");
        let example = &prompt[from..];
        let (prose, entries) = split_story_output(&format!("Story.\n{example}"), false);
        assert_eq!(prose, "Story.");
        assert_eq!(entries.len(), 2, "both example lines parse");
        assert_eq!(entries[0].was.as_deref(), Some(SAMPLE_WAS));
        assert_eq!(entries[0].now.as_deref(), Some(SAMPLE_NOW));
        assert_eq!(entries[1].was, None);
        assert!(entries[1].now.is_some());
    }

    #[test]
    fn a_canned_model_reply_mixing_five_four_and_slipped_lines_keeps_what_parses() {
        let reply = format!(
            "We moved the shift map to text.\n@shifts\n{}\nrisk|a.rs:f!|-|Old four-field claim\nshift|-|b.rs:g+|-|{}\nnot|a|valid\ndecision|c.rs:h~|c.rs:h~|{}|{}|extra\n",
            five(SAMPLE_WAS, SAMPLE_NOW),
            "Over-long sentence that goes on and on about nothing in particular for far too many words",
            SAMPLE_WAS,
            SAMPLE_NOW
        );
        let (prose, entries) = finish_story_output(&reply, true);
        assert_eq!(prose, "We moved the shift map to text.");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].now.as_deref(), Some(SAMPLE_NOW));
        assert_eq!(
            (entries[1].was.clone(), entries[1].now.clone()),
            (None, None)
        );
        assert_eq!(entries[1].claim, "Old four-field claim");
        assert_eq!(entries[2].was, None);
        assert!(entries[2].now.as_deref().unwrap().len() <= MAX_SENTENCE_CHARS);
    }

    #[test]
    fn no_panic_on_hostile_input() {
        let mut multibyte_900 = "shift|a.rs|b.rs|ok\n".repeat(46);
        multibyte_900.push_str("🔥🚀✨你好世界");
        let huge_line = format!("Prose\n@shifts\n{}", "a".repeat(100_000));
        let cases = [
            "",
            "@shifts",
            "@shifts\r\n",
            "|||",
            "shift|:::|~~~|   ",
            "prose\r\n@shifts\r\nshift|a.rs:b+|c.rs:d~|crlf line\r\n",
            &multibyte_900,
            &huge_line,
        ];
        for case in cases {
            let _ = split_story_output(case, false);
            let _ = split_story_output(case, true);
            let _ = parse_shifts_block(case, false);
            let _ = parse_shift_line(case);
            let _ = finish_story_output(case, true);
            let _ = finish_story_output(case, false);
        }
    }
}
