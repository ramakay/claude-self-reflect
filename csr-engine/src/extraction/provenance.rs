//! Provenance filtering — keeps CSR's own output out of CSR's input.
//!
//! CSR injects context into sessions, then extracts episodes/stories from those
//! same sessions. Without provenance tracking the system eats its own output:
//! probe reports, injected blocks, and agent prompts get re-extracted as
//! "session content" and re-injected next session (recursive garbage).
//!
//! This module is the SINGLE registry for self-reference detection. The old
//! approach — scattered vocabulary blocklists in stop.rs, session_briefing.rs,
//! import/mod.rs, prompt_submit.rs — was whack-a-mole: every session about CSR
//! coins new vocabulary, and each fix's own description text became the next
//! leak. Instead, detection here is structural:
//!
//! 1. **Transcript plumbing** (`strip_plumbing`): Claude Code's wrapper tags
//!    around command invocations and hook output. Removed as tag segments —
//!    a real prompt that merely carries a `<system-reminder>` keeps its prose.
//! 2. **Use–mention separation** (`strip_quoted`): code fences, inline code,
//!    and blockquotes are *mentions*, not session content. Stripped before any
//!    keyword extraction, so quoting an injected block can never pollute.
//! 3. **Emission registry** (`is_csr_emission`): the exact headers and field
//!    tokens CSR's formatters emit. Each entry is traceable to the code that
//!    emits it. A text matching one header (leading window) or ≥2 distinct
//!    field tokens is CSR's own output echoed back.
//! 4. **Forwards** (`strip_csr_forwards`): what the companion puts in the
//!    prompt for the person (a dream, the compaction card's draft), cut by its
//!    emitted grammar wherever it sits, the person's words around it kept.
//!
//! RULE: any new injected block format MUST register its header (and any
//! distinctive field tokens) here, in the same commit that adds the formatter.

/// Prompt signatures of CSR's own agent subprocesses (briefing analyst,
/// compaction summarizer, ratification extractor). A transcript *starting*
/// with one of these is CSR talking to itself. Shared by import (skip whole
/// conversation) and briefing (skip episode).
///
/// RULE: every `claude -p` spawn site (summarizer, session_briefing,
/// ratification) MUST have its prompt's opening line registered here in the
/// same commit — a missing entry let 4,700 ratification-extractor transcripts
/// into the corpus (82% of all conversations) before detection.
///
/// The last entry is the preamble out-of-tree narrators (the intent-narrator
/// Stop hook) lead their prompt with. It is the whole preamble, not the bare
/// sentinel: `csr-engine lessons` output legitimately begins with a sentinel,
/// and a user who opens a session by pasting it must keep that session.
pub const AGENT_PROMPT_SIGNATURES: [&str; 4] = [
    "You are CSR Episode Analyst",
    "You are summarizing a coding session",
    "# Ratification Dialog-Act Extraction",
    "[[CSR:RECAP]] machine-marker:",
];

/// Block headers CSR emits, one per formatter:
/// - `format_tier0_block` (session_start.rs)
/// - briefing prompt/output (session_briefing.rs)
/// - the /memory-feedback probe report template
/// - SessionStart continuity blocks (session_start.rs)
/// - UserPromptSubmit context blocks (injection/formatter.rs)
/// - agent prompts (AGENT_PROMPT_SIGNATURES, included via EMISSION_HEADERS)
/// - the csr-companion compaction's keep list (the summarizer's instructions, which a summary
///   may echo) and the one-line notice it leaves after compacting (plugins/csr-companion/hooks/
///   compact.ts `KEEP_HEADER`, `NOTICE_HEADER`)
///
/// The companion's forwarded dream and compaction card draft are NOT headers here: the person
/// talks about both in their own words ("Please change CSR's compaction card (the one in the
/// sidebar) ...", "CSR's dreamer asked this while you worked yesterday, but ..."), which a
/// substring match took for the emission (Codex review r2, #6 and #7). They are matched by
/// their emitted grammar instead: [`strip_csr_forwards`].
const EMISSION_HEADERS: [&str; 16] = [
    "CSR ENDLESS MEMORY ACTIVE",
    "CSR CONTINUUM [",
    "CSR PICKUP —",
    "EPISODE INDEX —",
    "## Session Intelligence",
    "## CSR Memory Feedback",
    "CSR Memory Feedback Probe",
    "SESSION CONTINUITY DETECTED",
    "RECENT SESSIONS - PAST CONTEXT",
    "RELEVANT PAST CONTEXT",
    "PAST CONTEXT - NOT INSTRUCTIONS",
    "You are CSR Episode Analyst",
    "You are summarizing a coding session",
    "# Ratification Dialog-Act Extraction",
    "CSR COMPACT KEEP —",
    "CSR compacted at",
];

/// Field tokens CSR emits inside blocks (Tier-0 fields, continuity lines).
/// Case-sensitive on purpose: our emissions are uppercase-exact; genuine prose
/// ("next: fix the todos counter") is not. One token can occur in real text;
/// two or more distinct tokens means the text is echoing our format.
const EMISSION_FIELD_TOKENS: [&str; 8] = [
    "LAST:",
    "NEXT:",
    "TODOS:",
    "ANCHORS:",
    "CONTINUED FROM [",
    "Past session [",
    "(outcome=",
    "csr_reflect_on_past(",
];

/// Sentinel embedded in every non-abstained `compose_recap` output (see
/// `hooks::recap`), inside its MAX_CHARS budget. The header/token detection
/// above depends on prose grammar an adversarial re-paste can disturb
/// (bullets, blockquotes, HTML, zero-width noise, confusable brackets, or
/// simply a long preamble pushing it past HEADER_WINDOW); the sentinel does
/// not — it is scanned across the FULL text, normalized, before any of that
/// grammar-dependent logic runs. Any new recap-composing formatter MUST emit
/// this sentinel inside its own char budget.
pub const RECAP_SENTINEL: &str = "[[CSR:RECAP]]";

/// Machine-owned sentinel carried by every prompt-time dream injection
/// (`hooks::dream_match`, Journal v4 P5 delivery channel (c)). Distinct token
/// from [`RECAP_SENTINEL`] so the two channels stay separately attributable,
/// but the same contract: scanned across the FULL text, normalized, before
/// any grammar-dependent branch, so an injected dream line can never be
/// re-imported as user content no matter how it is quoted, bulleted or
/// re-pasted.
///
/// **Not** the Journal v4 P4b *attribution* marker. That marker (a short
/// dream id appended to a copy block the user pastes into a NEW session) must
/// be RETAINED and indexed; this one causes rejection. They must never share
/// a token.
pub const DREAM_SENTINEL: &str = "[[CSR:DREAM]]";

/// Marks machine-generated `csr-engine lessons` candidate blocks so pasted
/// output cannot become new transcript evidence.
pub const LESSONS_SENTINEL: &str = "[[CSR:LESSONS]]";

/// Every machine-owned sentinel whose presence rejects the text outright.
pub(crate) const MACHINE_SENTINELS: [&str; 3] = [RECAP_SENTINEL, DREAM_SENTINEL, LESSONS_SENTINEL];

/// Code points that render as zero-width/invisible. An adversarial re-paste
/// can interleave these around or inside the sentinel to defeat a naive
/// substring search; stripping them first restores contiguity regardless of
/// where they were inserted.
const ZERO_WIDTH_CHARS: [char; 6] = [
    '\u{200B}', // ZERO WIDTH SPACE
    '\u{200C}', // ZERO WIDTH NON-JOINER
    '\u{200D}', // ZERO WIDTH JOINER
    '\u{2060}', // WORD JOINER
    '\u{FEFF}', // ZERO WIDTH NO-BREAK SPACE / BOM
    '\u{00AD}', // SOFT HYPHEN
];

/// Fold bracket code points visually confusable with ASCII `[`/`]` to their
/// ASCII form. Hand-rolled rather than pulling in a full Unicode-
/// normalization crate (out of scope here) — the sentinel only needs its own
/// two bracket characters protected.
fn fold_confusable_bracket(c: char) -> char {
    match c {
        '\u{FF3B}' | '\u{27E6}' | '\u{2E28}' => '[', // fullwidth / math white / stacked LEFT
        '\u{FF3D}' | '\u{27E7}' | '\u{2E29}' => ']', // fullwidth / math white / stacked RIGHT
        other => other,
    }
}

/// Normalize `text` for sentinel scanning: drop zero-width characters, fold
/// confusable brackets to ASCII. Allocates once; no external dependency.
fn normalize_for_sentinel_scan(text: &str) -> String {
    text.chars()
        .filter(|c| !ZERO_WIDTH_CHARS.contains(c))
        .map(fold_confusable_bracket)
        .collect()
}

/// True if `text` — after stripping zero-width noise and folding confusable
/// brackets — contains the machine-owned recap sentinel anywhere. Scans the
/// FULL text, not a HEADER_WINDOW-limited head: a sentinel can sit behind an
/// arbitrarily long preamble and must still be caught.
pub fn contains_recap_sentinel(text: &str) -> bool {
    normalize_for_sentinel_scan(text).contains(RECAP_SENTINEL)
}

/// True if `text` carries ANY machine-owned sentinel ([`RECAP_SENTINEL`] or
/// [`DREAM_SENTINEL`]) — same normalization, same full-text scan. Every
/// rejection path uses this; [`contains_recap_sentinel`] stays recap-specific
/// so callers that mean "a recap, specifically" keep saying so.
pub fn contains_machine_sentinel(text: &str) -> bool {
    let normalized = normalize_for_sentinel_scan(text);
    MACHINE_SENTINELS
        .iter()
        .any(|sentinel| normalized.contains(sentinel))
}

/// Claude Code transcript wrapper tags. Their contents are command plumbing or
/// hook output — never session content authored by the user or assistant.
const PLUMBING_TAGS: [&str; 6] = [
    "command-message",
    "command-name",
    "command-args",
    "local-command-caveat",
    "local-command-stdout",
    "system-reminder",
];

/// Window (bytes) scanned for emission headers. Headers sit at the top of the
/// blocks they introduce; scanning further would misclassify long real sessions
/// that discuss an injected block partway through.
const HEADER_WINDOW: usize = 400;

/// Remove Claude Code transcript plumbing tag segments (`<tag ...>…</tag>`,
/// or `<tag` to end-of-text when unclosed). Keeps surrounding prose.
pub fn strip_plumbing(text: &str) -> String {
    let mut out = text.to_string();
    for tag in PLUMBING_TAGS {
        let open = format!("<{}", tag);
        let close = format!("</{}>", tag);
        while let Some(start) = out.find(&open) {
            match out[start..].find(&close) {
                Some(rel_end) => {
                    out.replace_range(start..start + rel_end + close.len(), " ");
                }
                None => {
                    out.truncate(start);
                    break;
                }
            }
        }
    }
    out
}

/// Longest inline-code span kept verbatim. Short spans are identifiers —
/// commit hashes, file names, function names — that genuine prose needs
/// ("commit `4343b50`, deployed" must not become "commit , deployed").
/// Long spans are quoted blocks-in-miniature and stay stripped.
const INLINE_CODE_KEEP_LEN: usize = 24;

/// Remove mentioned (quoted) text: fenced code blocks, inline backtick spans,
/// and blockquote lines. What survives is prose the author actually wrote.
/// Short inline-code spans keep their content (identifiers used by the prose)
/// unless they contain a CSR emission field token — then they are mentions of
/// our own format and are dropped.
pub fn strip_quoted(text: &str) -> String {
    // Fenced blocks: drop odd-indexed segments between ``` markers.
    // An unclosed fence drops the tail (conservative: quoted until proven not).
    let mut fenceless = String::with_capacity(text.len());
    for (i, seg) in text.split("```").enumerate() {
        if i % 2 == 0 {
            fenceless.push_str(seg);
        }
    }
    // Inline code: same scheme with single backticks, keeping short
    // identifier-like spans that carry no emission tokens.
    let mut unquoted = String::with_capacity(fenceless.len());
    for (i, seg) in fenceless.split('`').enumerate() {
        let keep = i % 2 == 0
            || (seg.len() <= INLINE_CODE_KEEP_LEN
                && !EMISSION_FIELD_TOKENS.iter().any(|t| seg.contains(t)));
        if keep {
            unquoted.push_str(seg);
        }
    }
    // Blockquote lines.
    unquoted
        .lines()
        .filter(|l| !l.trim_start().starts_with('>'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// True if `text` is CSR's own emitted output (or an echo of it): an emission
/// header or a forward ([`strip_csr_forwards`]'s grammar) in the leading
/// window, or ≥2 distinct emission field tokens anywhere.
pub fn is_csr_emission(text: &str) -> bool {
    // Machine-owned sentinels, scanned across the FULL text before any
    // window-limited or grammar-based heuristic below runs.
    if contains_machine_sentinel(text) {
        return true;
    }
    if is_dreamer_prompt(text) {
        return true;
    }
    // A forwarded dream or compaction card that starts in the leading window,
    // matched by its emitted grammar (the window contract the headers keep).
    if scan_forwards(text, true).is_some_and(|found| found.first < HEADER_WINDOW) {
        return true;
    }

    let head_end = text.floor_char_boundary(HEADER_WINDOW.min(text.len()));
    let head = &text[..head_end];
    if EMISSION_HEADERS.iter().any(|h| head.contains(h)) {
        return true;
    }
    // Injected recaps open with `recap [<age>]: ` (hooks::recap). Match that
    // exact grammar — short bracket then "]: " — instead of a bare "recap ["
    // substring, so genuine prose like "recap [the auth changes], then
    // prioritize them" stays extractable (a contains-header would eat it).
    if let Some(rest) = head.trim_start().strip_prefix("recap [") {
        if rest.find("]: ").is_some_and(|i| i <= 24) {
            return true;
        }
    }
    EMISSION_FIELD_TOKENS
        .iter()
        .filter(|t| text.contains(**t))
        .count()
        >= 2
}

/// The attribution sentence the companion dreamer's "ask Claude" message carries.
pub(crate) const DREAMER_ATTRIBUTION: &str = "CSR's dreamer asked this while you worked";

/// The band's attribution paragraph, word for word. Forward grammar v2 (`.plans/release-10.1/forward-grammar.md`)
/// marks its own with `quoted`: [`DREAMER_BAND_QUOTED`]. Legacy (v1) forms already sit in transcripts:
/// `askClaude` (plugins/csr-companion/hooks/dreamer.ts) wrote the first, the recap fixtures in hooks/stop.rs
/// (1d23d675) carry the second. A paragraph that only opens with the attribution's words is the person's: "...
/// while you worked yesterday, but I wrote today's question myself." (Codex review r2, #7).
const DREAMER_BAND_QUOTED: &str =
    "CSR's dreamer asked this while you worked, quoted. Its reasoning:";
const DREAMER_BAND_LEGACY: [&str; 2] = [
    "CSR's dreamer asked this while you worked. Its reasoning:",
    "CSR's dreamer asked this while you worked. It is a question, not a task.",
];

fn is_band_attribution(line: &str) -> bool {
    let line = line.trim();
    line == DREAMER_BAND_QUOTED || DREAMER_BAND_LEGACY.contains(&line)
}

/// What marks a v2 one-line forward right after its header: its body follows, wrapped in curly quotes. The
/// emitter makes every ” inside a body a straight quote, so the first ” after the opening “ closes it.
const QUOTED_OPEN: &str = ", quoted: \u{201c}";

/// The pane's one-line send of a dream (plugins/csr-companion/bin/pane.py `dream_send`), at most 700 characters:
/// - v2: `CSR's dreamer asked this while you worked, quoted: “<question>” Its reasoning: “<reasoning>”`, ending at
///   the reasoning's closing ”.
/// - v1: `CSR's dreamer asked this while you worked: “<question>” Its reasoning: <title>. <reasons>`. Never
///   searched for a closing quote: it ends at `one_line`'s cut (`…` as its 700th character), else at the end of
///   its line. A question is at most 240 characters (dreamer.ts `parseDream`), so a cut never falls before "Its
///   reasoning:".
const DREAMER_PANE_LEGACY_OPEN: &str = ": \u{201c}";
const DREAMER_PANE_LEGACY_REASONING: &str = "\u{201d} Its reasoning:";
const DREAMER_PANE_QUOTED_REASONING: &str = " Its reasoning: \u{201c}";
const DREAMER_PANE_SEND_CHARS: usize = 700;

/// The companion's compaction card draft a click puts in the prompt (pane.py `compact_section`), the stage its
/// heading lowercased, at most 1,200 characters. v2: `CSR's compaction card (<stage>), quoted: “<body>”`. v1:
/// `CSR's compaction card (<stage>): <title>. <rows>.`, ending at its cut or the end of its line.
const COMPACTION_CARD_OPEN: &str = "CSR's compaction card (";
const COMPACTION_CARD_STAGES: [&str; 4] = [
    "compacting soon",
    "compacting",
    "compaction skipped",
    "compacted by csr",
];
const COMPACTION_CARD_SEND_CHARS: usize = 1200;

/// The session story's draft (pane.py), at most 700 characters. v2: `CSR's story of this session, rev <n>, quoted:
/// “<body>”`. v1: `CSR's story of this session, rev <n>: <body>`, ending at its cut or the end of its line.
const STORY_OPEN: &str = "CSR's story of this session, rev ";
const STORY_SEND_CHARS: usize = 700;

/// True when the WHOLE of `text` is the companion dreamer's "ask Claude"
/// message, in the exact grammar `askClaude` (plugins/csr-companion/hooks/
/// dreamer.ts) writes: an optional one-line question, a blank line, the
/// attribution paragraph (v2 or legacy), a blank line, then the dreamer's
/// reasoning as a blockquote. Every word of it is the dreamer's, not the
/// person's, so a caller must drop the message as a whole: paragraph scrubbing
/// alone would remove only the attribution and keep the question and the
/// reasoning as the person's own words. Prose that merely mentions the marker,
/// or opens with its words, does not match.
pub fn is_dreamer_prompt(text: &str) -> bool {
    let normalized = text.replace("\r\n", "\n");
    let paragraphs: Vec<&str> = normalized.trim().split("\n\n").collect();
    let Some(at) = paragraphs.iter().position(|p| is_band_attribution(p)) else {
        return false;
    };
    let one_line = |p: &str| !p.trim().is_empty() && !p.trim().contains('\n');
    let quoted = |p: &str| {
        !p.trim().is_empty()
            && p.lines()
                .all(|line| line.trim_start().starts_with('>') || line.trim().is_empty())
    };
    at <= 1
        && paragraphs[..at].iter().all(|p| one_line(p))
        && one_line(paragraphs[at])
        && paragraphs.len() > at + 1
        && paragraphs[at + 1..].iter().all(|p| quoted(p))
}

/// If `text` carries the companion dreamer's "ask Claude" message, the person's own words around it (empty when
/// there are none); `None` when it carries none. See [`strip_csr_forwards`] for the grammar; this reads dreams
/// only, never the compaction card or the story.
pub fn strip_dreamer_prompt(text: &str) -> Option<String> {
    scan_forwards(text, false).map(|found| found.own)
}

/// If `text` carries words the companion forwards into the prompt for the person, the person's own words around
/// them (empty when there are none); `None` when it carries none. Forward grammar v2
/// (`.plans/release-10.1/forward-grammar.md`). Every forward is cut, wherever it sits (Codex review r2, #8), as a
/// bounded span read by its emitted grammar, never by a substring or a byte window; the person's words before and
/// after a span, on its lines too, stay:
/// - one-line forwards (the pane's dream, the compaction card, the session story): v2, marked `quoted` in its
///   header, ends at the closing ” of its quoted body (for the dream, its reasoning's), searched only inside the
///   send's character limit; v1 (the header followed directly by `:`) is never searched for a closing quote and
///   ends at its known cut (`…` exactly at the limit: 700 for the dream and the story, 1,200 for the card), else at
///   the end of its line (astra #4, #9). A card needs a stage the card has (#6).
/// - the band's dream, read by line: the attribution line exactly, with the question above it (past a blank line)
///   and/or the run of `>` lines under it (past a blank line); the first line without `>` is the person's (astra
///   #10). v2's question is the `“…”` span that ends the line above, from its last `“`, so words before it on that
///   line stay; v1's question line ends in a question mark and goes whole (it cannot be split, legacy only). The
///   attribution's words alone are never enough (#7).
///
/// A band hotkey digit or a `>` directly before a one-line forward goes with it.
pub fn strip_csr_forwards(text: &str) -> Option<String> {
    scan_forwards(text, true).map(|found| found.own)
}

/// What [`scan_forwards`] found: the person's own words and where the first forward starts.
struct Forwards {
    /// The person's paragraphs, trimmed, blank ones dropped, joined by a blank line.
    own: String,
    /// Byte offset of the first forward in the CRLF-normalised, trimmed text.
    first: usize,
    spans: Vec<ForwardSpan>,
}

/// Byte offset just past the `…` pane.py's `one_line(text, chars)` appends when it cut a send that starts at
/// `start` (it keeps `chars - 1` characters), if it did.
fn send_truncation_end(line: &str, start: usize, chars: usize) -> Option<usize> {
    let (offset, c) = line[start..].char_indices().nth(chars - 1)?;
    (c == '\u{2026}').then(|| start + offset + c.len_utf8())
}

/// Byte offset of the character `chars` past `start`, or the end of the line: where a send of at most `chars`
/// characters must have ended.
fn send_limit(line: &str, start: usize, chars: usize) -> usize {
    line[start..]
        .char_indices()
        .nth(chars)
        .map_or(line.len(), |(offset, _)| start + offset)
}

/// Just past the first ” at or after `from`, if one falls before `limit`.
fn closing_quote(line: &str, from: usize, limit: usize) -> Option<usize> {
    line[from..limit.max(from)]
        .find('\u{201d}')
        .map(|at| from + at + '\u{201d}'.len_utf8())
}

/// End of a v1 (legacy) one-line send that starts at `start`: its cut, else the end of its line.
fn legacy_end(line: &str, start: usize, chars: usize) -> (usize, bool) {
    let end = send_truncation_end(line, start, chars);
    (end.unwrap_or(line.len()), end.is_some())
}

/// A one-line forward whose header ends at `header` (the stage, or the story's number): its end, v2 or v1, or
/// `None` when what follows is neither form.
fn body_forward_end(
    line: &str,
    start: usize,
    header: usize,
    chars: usize,
) -> Option<(usize, bool)> {
    let rest = &line[header..];
    if rest.starts_with(QUOTED_OPEN) {
        let limit = send_limit(line, start, chars);
        let end = closing_quote(line, header + QUOTED_OPEN.len(), limit);
        return Some((end.unwrap_or(line.len()), end.is_some()));
    }
    rest.starts_with(':')
        .then(|| legacy_end(line, start, chars))
}

/// The pane's dream starting at `start` (where the attribution sentence is): its end, or `None`.
fn pane_dream_end(line: &str, start: usize) -> Option<(usize, bool)> {
    let after = start + DREAMER_ATTRIBUTION.len();
    let rest = &line[after..];
    if rest.starts_with(QUOTED_OPEN) {
        let limit = send_limit(line, start, DREAMER_PANE_SEND_CHARS);
        let question = closing_quote(line, after + QUOTED_OPEN.len(), limit);
        let reasoning = question
            .filter(|&end| line[end..].starts_with(DREAMER_PANE_QUOTED_REASONING))
            .and_then(|end| closing_quote(line, end + DREAMER_PANE_QUOTED_REASONING.len(), limit));
        return Some((reasoning.unwrap_or(line.len()), reasoning.is_some()));
    }
    let question = rest.strip_prefix(DREAMER_PANE_LEGACY_OPEN)?;
    question
        .contains(DREAMER_PANE_LEGACY_REASONING)
        .then(|| legacy_end(line, start, DREAMER_PANE_SEND_CHARS))
}

/// The first one-line forward in `line` at or after byte `from`: its `(start, end)`. The card and the story count
/// only when `others`.
fn next_one_line_forward(line: &str, from: usize, others: bool) -> Option<(usize, usize, bool)> {
    let dream = line[from..]
        .match_indices(DREAMER_ATTRIBUTION)
        .find_map(|(at, _)| {
            pane_dream_end(line, from + at).map(|(end, bounded)| (from + at, end, bounded))
        });
    let card = || {
        line[from..]
            .match_indices(COMPACTION_CARD_OPEN)
            .find_map(|(at, open)| {
                let start = from + at;
                let stage = start + open.len();
                COMPACTION_CARD_STAGES.iter().find_map(|known| {
                    line[stage..]
                        .strip_prefix(known)
                        .filter(|after| after.starts_with(')'))
                        .and_then(|_| {
                            let header = stage + known.len() + ")".len();
                            body_forward_end(line, start, header, COMPACTION_CARD_SEND_CHARS)
                        })
                        .map(|(end, bounded)| (start, end, bounded))
                })
            })
    };
    let story = || {
        line[from..]
            .match_indices(STORY_OPEN)
            .find_map(|(at, open)| {
                let start = from + at;
                let number = start + open.len();
                let digits = line[number..]
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(line.len() - number);
                (digits > 0)
                    .then(|| body_forward_end(line, start, number + digits, STORY_SEND_CHARS))
                    .flatten()
                    .map(|(end, bounded)| (start, end, bounded))
            })
    };
    [
        dream,
        others.then(card).flatten(),
        others.then(story).flatten(),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|&(start, _, _)| start)
}

/// Start of an optional quote marker and single hotkey digit before a header.
/// A multi-digit number belongs to the person and is preserved whole.
fn forward_prefix_start(piece: &str) -> usize {
    let trimmed = piece.trim_end();
    let digit_start = trimmed.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let digits = trimmed.len() - digit_start;
    if digits > 1 {
        return piece.len();
    }
    let mut cut = digit_start;
    let mut marker = false;
    loop {
        cut = piece[..cut].trim_end().len();
        if piece[..cut].ends_with('>') {
            cut -= 1;
            marker = true;
        } else {
            break;
        }
    }
    if digits == 1 && !marker && cut == digit_start && cut > 0 {
        // A digit attached to a word is not a band hotkey.
        return piece.len();
    }
    cut
}

/// A forward's byte range, and whether its end relies on the message ending.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ForwardSpan {
    pub start: usize,
    pub end: usize,
    pub needs_message_end: bool,
}

/// Cut one-line forwards using the same prefix and span grammar as scrub.
fn strip_one_line_forwards(line: &str, others: bool) -> Option<(String, usize, Vec<ForwardSpan>)> {
    let mut pieces = Vec::new();
    let mut spans = Vec::new();
    let mut cursor = 0;
    let mut first = None;
    while let Some((start, end, bounded)) = next_one_line_forward(line, cursor, others) {
        first.get_or_insert(start);
        let from = cursor + forward_prefix_start(&line[cursor..start]);
        pieces.push(line[cursor..from].trim());
        spans.push(ForwardSpan {
            start: from,
            end,
            needs_message_end: end == line.len() && !bounded,
        });
        cursor = end.max(start + 1);
    }
    let first = first?;
    pieces.push(line[cursor..].trim());
    Some((
        pieces
            .into_iter()
            .filter(|piece| !piece.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        first,
        spans,
    ))
}

/// All forward cuts in original byte coordinates, including band forwards.
/// Extraction and scrub obtain their cuts from the same complete-message parser.
pub(crate) fn forward_spans(text: &str) -> Vec<ForwardSpan> {
    let Some(found) = scan_forwards(text, true) else {
        return Vec::new();
    };
    let normalized = text.replace("\r\n", "\n");
    let leading = normalized.len() - normalized.trim_start().len();
    let bytes = text.as_bytes();
    let mut offsets = (0..bytes.len())
        .filter(|&at| bytes[at] != b'\r' || bytes.get(at + 1) != Some(&b'\n'))
        .collect::<Vec<_>>();
    offsets.push(text.len());
    found
        .spans
        .into_iter()
        .map(|span| ForwardSpan {
            start: offsets[leading + span.start],
            end: offsets[leading + span.end],
            // A newline present in the stored text supplies the line's end even
            // when trimming removed it before parsing.
            needs_message_end: span.needs_message_end
                && !normalized[leading + span.end..].contains('\n'),
        })
        .collect()
}

/// Read `text` by line for the forwards [`strip_csr_forwards`] names (the card and the story only when `others`).
/// `None` when there are none.
fn scan_forwards(text: &str, others: bool) -> Option<Forwards> {
    if !text.contains(DREAMER_ATTRIBUTION)
        && !(others && (text.contains(COMPACTION_CARD_OPEN) || text.contains(STORY_OPEN)))
    {
        return None;
    }
    let normalized = text.replace("\r\n", "\n");
    let body = normalized.trim();
    let lines: Vec<&str> = body.split('\n').collect();
    let mut starts = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in &lines {
        starts.push(offset);
        offset += line.len() + 1;
    }
    let blank = |line: &str| line.trim().is_empty();
    let asks = |line: &str| line.trim().ends_with(['?', '\u{ff1f}', '\u{61f}']);
    let quoted = |line: &str| line.trim_start().starts_with('>');
    // `None`: the line is cut whole; `Some(text)`: what is left of it.
    let mut kept: Vec<Option<&str>> = lines.iter().map(|line| Some(*line)).collect();
    let mut first: Option<usize> = None;
    let mut spans = Vec::new();
    let mark = |at: usize, first: &mut Option<usize>| {
        *first = Some(first.map_or(at, |seen| seen.min(at)));
    };
    for at in 0..lines.len() {
        if kept[at].is_none() || !is_band_attribution(lines[at]) {
            continue;
        }
        let v2 = lines[at].trim() == DREAMER_BAND_QUOTED;
        // The question: the line above, past at least one blank line.
        let mut above = at;
        while above > 0 && blank(lines[above - 1]) {
            above -= 1;
        }
        let question = (above < at && above > 0 && kept[above - 1] == Some(lines[above - 1]))
            .then(|| above - 1)
            .and_then(|line| {
                let text = lines[line].trim_end();
                if v2 {
                    // The `“…”` span that ends the line, from its last “: what is before it stays.
                    let open = text
                        .ends_with('\u{201d}')
                        .then(|| text.rfind('\u{201c}'))
                        .flatten()?;
                    Some((line, open))
                } else {
                    asks(text).then_some((line, 0))
                }
            });
        // The reasoning: the run of `>` lines below, past at least one blank line.
        let mut below = at + 1;
        while below < lines.len() && blank(lines[below]) {
            below += 1;
        }
        let mut end = below;
        if below > at + 1 {
            while end < lines.len() && quoted(lines[end]) {
                end += 1;
            }
        }
        let reasoning = end > below;
        if question.is_none() && !reasoning {
            continue;
        }
        let to = if reasoning { end } else { at + 1 };
        let span_start = question.map_or(starts[at], |(line, open)| starts[line] + open);
        let span_end = if to < lines.len() {
            starts[to]
        } else {
            body.len()
        };
        spans.push(ForwardSpan {
            start: span_start,
            end: span_end,
            needs_message_end: to == lines.len(),
        });
        let from = match question {
            Some((line, open)) => {
                let before = lines[line][..open].trim_end();
                kept[line] = (!before.is_empty()).then_some(before);
                mark(starts[line] + open, &mut first);
                line + 1
            }
            None => {
                mark(starts[at], &mut first);
                at
            }
        };
        kept[from..to].fill(None);
    }
    let mut left: Vec<String> = Vec::with_capacity(lines.len());
    for (index, line) in kept.iter().enumerate() {
        let Some(line) = line else { continue };
        match strip_one_line_forwards(line, others) {
            Some((own, at, cuts)) => {
                spans.extend(cuts.into_iter().map(|cut| ForwardSpan {
                    start: starts[index] + cut.start,
                    end: starts[index] + cut.end,
                    needs_message_end: cut.needs_message_end && index + 1 == lines.len(),
                }));
                mark(starts[index] + at, &mut first);
                if !own.is_empty() {
                    left.push(own);
                }
            }
            None => left.push((*line).to_string()),
        }
    }
    let first = first?;
    // Paragraphs of what is left: a cut line vanishes, a blank line still parts two paragraphs.
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in left.into_iter().chain([String::new()]) {
        if !blank(&line) {
            current.push(line);
        } else if !current.is_empty() {
            paragraphs.push(current.join("\n").trim().to_string());
            current.clear();
        }
    }
    Some(Forwards {
        own: paragraphs.join("\n\n"),
        first,
        spans,
    })
}

/// Opener shared by every side-agent note a person can forward into the
/// prompt: Claude Code's "You should know" agent writes `... offered by a side
/// agent:` and CSR's own band writes `... offered by CSR (claude-self-reflect):`.
pub const SIDE_AGENT_NOTE_PREFIX: &str = "Here is a note offered by";

/// If `text` is a forwarded side-agent note, the person's own words after it.
///
/// A forward is the opener line (starts with [`SIDE_AGENT_NOTE_PREFIX`], ends
/// with `:`) followed by the note as a contiguous blockquote. Blank lines and
/// bare `>` lines between and inside the quote belong to it. The opener and
/// the quote are the side agent's words, not the person's, so they are cut;
/// any unquoted prose after them is the person's own and is returned (empty
/// when there is none). `None` means `text` is not a forward at all, so a
/// sentence that merely mentions the phrase is never touched. CRLF is
/// normalised first, so the remainder uses `\n`.
pub fn strip_forwarded_note(text: &str) -> Option<String> {
    let normalized = text.replace("\r\n", "\n");
    let trimmed = normalized.trim();
    if !trimmed.starts_with(SIDE_AGENT_NOTE_PREFIX) {
        return None;
    }
    let mut lines = trimmed.lines();
    if !lines.next()?.trim_end().ends_with(':') {
        return None;
    }
    let rest: Vec<&str> = lines
        .skip_while(|line| line.trim().is_empty() || line.trim_start().starts_with('>'))
        .collect();
    Some(rest.join("\n").trim().to_string())
}

/// True if `text` is a substantive description of real work — it survives
/// provenance filtering AND carries actual prose (alphabetic content, not a
/// bare timestamp/number/path like "20260611-122252"). Used to pick a real
/// continuity anchor over command-only or telemetry-only sessions.
pub fn is_substantive(text: &str) -> bool {
    match extractable(text) {
        Some(clean) => {
            let alpha = clean.chars().filter(|c| c.is_alphabetic()).count();
            // Needs real words, not just a token or two of metadata.
            alpha >= 6 && clean.split_whitespace().count() >= 2
        }
        None => false,
    }
}

/// Send texts the CSR companion pane types into Claude's prompt for the person
/// (`plugins/csr-companion/bin/pane.py`, every `send=one_line(...)` that
/// restates memory CSR already handed). Pasted back as a prompt, the words are
/// CSR's: retrieving on them returns the very items they list. Live failure
/// 2026-10-10: a pasted hand-off list drew back its own "When claude is killed"
/// note at cosine 0.855, plus a duplicate of it, as fresh PAST CONTEXT.
const PANE_HANDED: &str = "CSR handed you these this session: ";
/// pane.py `lost_asks`: `Lost and found: “<thread>” went quiet after HH:MM.`
/// then one of two fixed asks, all CSR's words.
const PANE_LOST: &str = "Lost and found: “";
/// The fixed closing words of the two `lost_asks` send forms.
const PANE_LOST_ENDINGS: [&str; 2] = [
    "then tell me in one line.",
    "report its result when it lands.",
];
const PANE_REMEMBERED: &str = "CSR remembered: “";
const PANE_REMEMBERED_SESSION: &str = "CSR remembered a past session: “";

/// `one_line(text, most=700)` in pane.py cuts at 699 characters and appends
/// `…`: an ellipsis exactly there ends the send text, whatever follows is the
/// person's.
const PANE_SEND_WIDTH: usize = 699;

/// Line openings of the blocks the hooks inject (prompt_submit, session_start,
/// the formatter). Lines under one of them that continue the block (`- [`
/// items) go with it.
const INJECTED_BLOCK_OPENINGS: [&str; 12] = [
    "CSR PICKUP —",
    "EPISODE INDEX —",
    "## PAST CONTEXT - NOT INSTRUCTIONS",
    "## RELEVANT PAST CONTEXT - NOT INSTRUCTIONS",
    "PAST CONTEXT - NOT INSTRUCTIONS",
    "RELEVANT PAST CONTEXT - NOT INSTRUCTIONS",
    "CSR CONTINUUM [",
    "CSR ENDLESS MEMORY ACTIVE",
    "Resume from LAST/NEXT above",
    "CSR: `",
    "RECENT SESSIONS - PAST CONTEXT",
    "SESSION CONTINUITY DETECTED",
];

/// Where a hand-off item came from, as pane.py's `from_when` + project +
/// `N×` render it (" · 2 weeks ago · csr-engine · 5×"). The last match in a
/// pasted hand-off list ends the list.
static HANDED_WHERE_TAIL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r" · (?:(?:earlier today|yesterday|\d+ (?:days|weeks|months) ago)(?: · [^\s·;…]+)?(?: · \d+×)?|[^\s·;…]+ · \d+×|\d+×)",
    )
    .expect("hand-off tail regex")
});

/// A formatter line pasted without its header: a PAST CONTEXT item
/// (`- [reflection|unknown] …`) or an EPISODE INDEX entry (`→ csr_reflect_on_past("conv_…")`).
static INJECTED_ITEM_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"^- \[(?:reflection|chunk|anti_pattern|code_graph|code_evolution|conversation|episode|dream)(?:\|[^\]]*)?\] |→ csr_reflect_on_past\("conv_"#,
    )
    .expect("injected item regex")
});

/// Byte offset just past the ellipsis `one_line` appends when it cut a send
/// text that starts at `start`, if it did.
fn pane_truncation_end(line: &str, start: usize) -> Option<usize> {
    send_truncation_end(line, start, PANE_SEND_WIDTH + 1)
}

/// End (byte offset in `line`) of a pane send text that starts at `start`
/// with `header`.
fn pane_send_end(line: &str, start: usize, header: &str) -> usize {
    if let Some(end) = pane_truncation_end(line, start) {
        return end;
    }
    let body = start + header.len();
    let rest = &line[body..];
    match header {
        PANE_HANDED => HANDED_WHERE_TAIL
            .find_iter(rest)
            .last()
            .map_or(line.len(), |m| body + m.end()),
        PANE_REMEMBERED_SESSION => rest
            .find("csr_reflect_on_past(\"conv_")
            .and_then(|at| rest[at..].find(')').map(|close| body + at + close + 1))
            .unwrap_or(line.len()),
        PANE_LOST => {
            // The thread text is quoted; the send ends at whichever fixed ask
            // follows the close quote, or at the line end if a re-paste lost it.
            let after_quote = rest.find("” went quiet after ").unwrap_or(0);
            PANE_LOST_ENDINGS
                .iter()
                .filter_map(|ending| {
                    rest[after_quote..]
                        .find(ending)
                        .map(|at| body + after_quote + at + ending.len())
                })
                .min()
                .unwrap_or(line.len())
        }
        PANE_REMEMBERED => match rest.find('”') {
            Some(close) => {
                let after = body + close + '”'.len_utf8();
                // " (<where>)." follows when the note has a date or project.
                let tail = &line[after..];
                if tail.starts_with(" (") {
                    tail.find(").").map_or(after, |p| after + p + 2)
                } else {
                    after + usize::from(tail.starts_with('.'))
                }
            }
            None => line.len(),
        },
        // Any other send text runs to the end of its line (one_line
        // collapsed it to one).
        _ => line.len(),
    }
}

/// Cut every CSR span out of one line, returning what the person wrote.
fn strip_line_emissions(line: &str) -> String {
    let mut out = line.to_string();
    // Recaps: `recap [<age>]: …` (hooks::recap grammar, the same short-bracket
    // test `is_csr_emission` uses) up to and including the sentinel, or to the
    // end of the line when a re-paste lost it.
    let mut from = 0;
    while let Some(rel) = out[from..].find("recap [") {
        let lead = from + rel;
        let rest = &out[lead + "recap [".len()..];
        if !rest.find("]: ").is_some_and(|i| i <= 24) {
            from = lead + "recap [".len();
            continue;
        }
        let end = out[lead..]
            .find(RECAP_SENTINEL)
            .map_or(out.len(), |at| lead + at + RECAP_SENTINEL.len());
        out.replace_range(lead..end, " ");
        from = lead;
    }
    // The session story is a forward: `strip_csr_forwards` cuts it by its
    // grammar (its revision number, then `:` or `, quoted: “`). Cut here by its
    // opening words alone, it took the person's sentence about a story with it.
    for header in [
        PANE_HANDED,
        PANE_LOST,
        PANE_REMEMBERED_SESSION,
        PANE_REMEMBERED,
    ] {
        while let Some(start) = out.find(header) {
            let end = pane_send_end(&out, start, header);
            out.replace_range(start..end, " ");
        }
    }
    out
}

/// The person's own words in a prompt that may carry CSR's output back to it:
/// pasted pane send texts (hand-off lists, the session story, a remembered
/// note), recap lines, injected PICKUP / EPISODE INDEX / PAST CONTEXT blocks,
/// the dreamer's message, the compaction card's draft. Each grammar is the
/// emitter's own, so prose that only mentions CSR is left alone. Retrieval must
/// run on what this returns: run on the paste, it finds the items the paste lists.
pub fn strip_csr_emissions(text: &str) -> String {
    // The dreamer's message and the card's draft are cut by their shape first,
    // every one of them; a line-by-line pass would keep a dream's question.
    let own;
    let text = match strip_csr_forwards(text) {
        Some(words) if words.is_empty() => return String::new(),
        Some(words) => {
            own = words;
            own.as_str()
        }
        None => text,
    };
    strip_emission_lines(text, true)
}

/// The line pass of [`strip_csr_emissions`]: injected block lines, item lines, recap spans, pane send texts and
/// sentinel lines go; every other line stays as typed. `reminder_tags` also drops bare `<system-reminder>` tag
/// lines (the query side); the import scrub keeps them, since it removes CSR's own reminders whole and a reminder
/// Claude Code wrote is not CSR's to edit. Returns `text` unchanged when nothing on it is CSR's.
pub(crate) fn strip_emission_lines(text: &str, reminder_tags: bool) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut changed = false;
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim_start().trim_start_matches('>').trim_start();
        if line.is_empty() {
            in_block = false;
            kept.push(raw.to_string());
            continue;
        }
        if reminder_tags && (line == "<system-reminder>" || line == "</system-reminder>") {
            changed = true;
            continue;
        }
        let line = if reminder_tags {
            line.strip_prefix("<system-reminder>").unwrap_or(line)
        } else {
            line
        };
        if INJECTED_BLOCK_OPENINGS.iter().any(|h| line.starts_with(h)) {
            in_block = true;
            changed = true;
            continue;
        }
        if INJECTED_ITEM_LINE.is_match(line) || (in_block && line.starts_with("- ")) {
            changed = true;
            continue;
        }
        in_block = false;
        let words = strip_line_emissions(raw);
        if contains_machine_sentinel(&words) {
            // A dream or lessons line, or a recap whose grammar a re-paste
            // broke: machine-owned end to end.
            changed = true;
            continue;
        }
        changed |= words != raw;
        kept.push(words);
    }
    if !changed {
        // Nothing of CSR's: the prompt goes on exactly as typed.
        return text.to_string();
    }
    kept.join("\n").trim().to_string()
}

/// Full provenance pipeline for episode/story extraction candidates:
/// strip plumbing → strip mentions → reject CSR emissions → reject empties.
/// Returns the cleaned text safe to carry forward as session content.
pub fn extractable(text: &str) -> Option<String> {
    let unplumbed = strip_plumbing(text);
    // Recap sentinel only (not full is_csr_emission) runs before strip_quoted:
    // a blockquote-wrapped recap ("> recap [...]: ... [[CSR:RECAP]]") would
    // otherwise have every quoted line erased before the sentinel is ever
    // seen, letting an unquoted preamble alongside it survive as if it were
    // genuine content. Historical header/field-token branches still apply
    // after strip_quoted on cleaned prose — quoted echoes alone must not
    // reject genuine unquoted content co-occurring with them.
    if contains_machine_sentinel(&unplumbed) {
        return None;
    }
    // The dreamer's message is read by its blockquote, which strip_quoted
    // erases: cut first, by its shape, or a long question pushes the marker
    // past HEADER_WINDOW and the question is kept as the person's. The
    // compaction card's draft goes with it. What the person wrote around them
    // goes on.
    let unplumbed = match strip_csr_forwards(&unplumbed) {
        Some(own) if own.trim().is_empty() => return None,
        Some(own) => own,
        None => unplumbed,
    };
    let prose = strip_quoted(&unplumbed);
    let cleaned = prose.trim();
    if cleaned.is_empty() || is_csr_emission(cleaned) {
        return None;
    }
    Some(cleaned.to_string())
}

/// Codex review r2 inputs (#6, #7, #8), shared by these tests and the import scrub's.
#[cfg(test)]
pub(crate) mod probes {
    /// #6: the person asks about the card; a header matched as a substring took it for the card's draft.
    pub(crate) const CARD_REQUEST: &str =
        "Please change CSR's compaction card (the one in the sidebar) to show savings in tokens.";

    /// #7: the person's own sentence that opens with the attribution's words, past HEADER_WINDOW.
    pub(crate) fn attribution_talk() -> String {
        format!(
            "{}\n\nCSR's dreamer asked this while you worked yesterday, but I wrote today's question myself.",
            "This is an actual observation that I wrote myself. ".repeat(10)
        )
    }

    /// #8: the band's forward of a 223-character Devanagari dream (553 bytes, past HEADER_WINDOW).
    pub(crate) fn hindi_forward() -> String {
        let question = format!(
            "\u{915}\u{94d}\u{92f}\u{93e} \u{939}\u{94b} \u{905}\u{917}\u{930} {}?",
            "\u{939}\u{92e} \u{92a}\u{947}\u{928} \u{915}\u{947} \u{930}\u{902}\u{917} \u{921}\u{947}\u{915} \u{915}\u{940} \u{938}\u{94d}\u{92f}\u{93e}\u{939}\u{940} \u{938}\u{947} \u{932}\u{947}\u{902} "
                .repeat(6)
        );
        format!(
            "{question}\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**\n> * Two surfaces read the same inks."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_r2_6_a_request_about_the_compaction_card_stays_the_persons() {
        let ask = probes::CARD_REQUEST;
        assert!(!is_csr_emission(ask));
        assert_eq!(extractable(ask).as_deref(), Some(ask));
        assert_eq!(strip_csr_emissions(ask), ask);
        // No known stage and no "):" after it: the person's words, whatever follows.
        let wrong =
            "CSR's compaction card (compacted by csr) is wrong; never drop my pending task.";
        assert!(!is_csr_emission(wrong));
        assert_eq!(extractable(wrong).as_deref(), Some(wrong));
        // The card's draft goes; the person's paragraphs before and after it stay.
        let card = "CSR's compaction card (compacted by csr): Context: 274k \u{2192} 70k tokens. Learnt: Never skip validation.";
        let around = format!("Look at this one.\n\n{card}\n\nWhy is the payback so slow?");
        let mine = "Look at this one.\n\nWhy is the payback so slow?";
        assert_eq!(extractable(&around).as_deref(), Some(mine));
        assert_eq!(strip_csr_emissions(&around), mine);
        // pane.py's one_line puts the draft on one line: the next line is the person's.
        assert_eq!(
            extractable(&format!("{card}\nthat payback is wrong")).as_deref(),
            Some("that payback is wrong")
        );
        // A band hotkey that landed in the box before it is no word of the person's.
        let keyed = "2CSR's compaction card (compacting soon): 278k \u{b7} past your band.";
        assert!(is_csr_emission(keyed));
        assert!(extractable(keyed).is_none());
        assert_eq!(strip_csr_emissions(keyed), "");
    }

    #[test]
    fn codex_r2_7_talk_that_opens_with_the_attribution_is_the_persons() {
        let talk = probes::attribution_talk();
        assert!(talk.find("CSR's dreamer").unwrap() > HEADER_WINDOW);
        assert_eq!(strip_dreamer_prompt(&talk), None);
        assert_eq!(extractable(&talk).as_deref(), Some(talk.as_str()));
        assert_eq!(strip_csr_emissions(&talk), talk);
        // Inside the header window too: a prefix of the attribution is not the forward.
        let short = "CSR's dreamer asked this while you worked yesterday, but I wrote today's question myself.";
        assert!(!is_csr_emission(short));
        assert_eq!(extractable(short).as_deref(), Some(short));
        assert_eq!(strip_csr_emissions(short), short);
        let asked = format!("Did you see it?\n\n{short}\n\n> my own quote");
        assert_eq!(strip_dreamer_prompt(&asked), None);
        assert_eq!(
            extractable(&asked).as_deref(),
            Some(format!("Did you see it?\n\n{short}").as_str())
        );
    }

    #[test]
    fn codex_r2_8_every_forwarded_dream_goes_and_the_words_between_stay() {
        let f = probes::hindi_forward();
        let text = format!("{f}\n\nNo, keep the current palette.\n\n{f}");
        let mine = Some("No, keep the current palette.");
        assert_eq!(strip_dreamer_prompt(&text).as_deref(), mine);
        assert_eq!(extractable(&text).as_deref(), mine);
        assert_eq!(Some(strip_csr_emissions(&text).as_str()), mine);
        let after = format!("{text}\n\nAnd stop asking about colours.");
        let both = Some("No, keep the current palette.\n\nAnd stop asking about colours.");
        assert_eq!(extractable(&after).as_deref(), both);
        assert_eq!(Some(strip_csr_emissions(&after).as_str()), both);
        // The pane's one-line send and the band's forward in one prompt.
        let pane = "CSR's dreamer asked this while you worked: \u{201c}What if the recap led with the blocker?\u{201d} Its reasoning: Recap ordering.";
        let mixed = format!("{pane}\n\nNot now.\n\n{f}");
        assert_eq!(extractable(&mixed).as_deref(), Some("Not now."));
        assert_eq!(strip_csr_emissions(&mixed), "Not now.");
        // A band hotkey before the pane's send is not the person's; words typed before it on its line are.
        assert_eq!(
            extractable(&format!("2{pane}\nnot now")).as_deref(),
            Some("not now")
        );
        assert_eq!(
            extractable(&format!("Look at this: {pane}")).as_deref(),
            Some("Look at this:")
        );
    }

    /// Every reader of the person's words agrees on `input`: extraction, the
    /// import scrub, and the query-side emission stripper.
    fn own_words_agree(input: &str, mine: Option<&str>) {
        assert_eq!(extractable(input).as_deref(), mine, "extractable: {input}");
        assert_eq!(
            crate::import::scrub_contaminated_text(input).as_deref(),
            mine,
            "scrub: {input}"
        );
        assert_eq!(
            strip_csr_emissions(input),
            mine.unwrap_or(""),
            "strip_csr_emissions: {input}"
        );
    }

    /// The band's forward as `askClaude` writes it.
    const BAND: &str = "What if we reuse the palette?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Reuse**\n> Shared colours.";

    #[test]
    fn astra_4_words_after_a_truncated_legacy_send_are_the_persons() {
        // pane.py one_line(.., 700) cut the send at 699 characters and added an ellipsis: the send ended there.
        let head =
            "CSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: ";
        let sent = format!("{head}{}\u{2026}", "x".repeat(699 - head.chars().count()));
        assert_eq!(sent.chars().count(), 700);
        own_words_agree(
            &format!("{sent} Keep the deployment frozen."),
            Some("Keep the deployment frozen."),
        );
        own_words_agree(
            &format!("Look: {sent} Keep the deployment frozen."),
            Some("Look: Keep the deployment frozen."),
        );
        // The card's send is cut at 1,199 characters the same way.
        let card = "CSR's compaction card (compacted by csr): ";
        let sent = format!("{card}{}\u{2026}", "y".repeat(1199 - card.chars().count()));
        own_words_agree(
            &format!("{sent} Keep the deployment frozen."),
            Some("Keep the deployment frozen."),
        );
        // Untruncated legacy sends still run to the end of their line.
        own_words_agree(
            "CSR's compaction card (compacting): Summarizing 278k. ok\nKeep it.",
            Some("Keep it."),
        );
    }

    #[test]
    fn astra_9_a_card_after_the_persons_words_on_its_line_is_cut() {
        own_words_agree(
            "Please check this: CSR's compaction card (compacting): Summarizing 278k.",
            Some("Please check this:"),
        );
    }

    #[test]
    fn astra_10_the_band_forward_is_cut_by_line_inside_the_persons_paragraph() {
        // A: the person's line directly above the question.
        let above = format!("Keep the deployment frozen.\n{BAND}");
        own_words_agree(&above, Some("Keep the deployment frozen."));
        assert_eq!(
            strip_dreamer_prompt(&above).as_deref(),
            Some("Keep the deployment frozen.")
        );
        // B: the person's reply directly under the reasoning.
        let below = format!("{BAND}\nNo, keep the current palette.");
        own_words_agree(&below, Some("No, keep the current palette."));
        assert_eq!(
            strip_dreamer_prompt(&below).as_deref(),
            Some("No, keep the current palette.")
        );
    }

    /// The v2 emitters' normalization: an inner ” becomes a straight quote.
    fn straight(text: &str) -> String {
        text.replace('\u{201d}', "\"")
    }

    /// `<head>“<body>”` within `most` characters, the body cut with `…` (still closed) when it does not fit.
    fn envelope(head: &str, body: &str, most: usize) -> String {
        let body = straight(body);
        let room = most - head.chars().count() - 2;
        let body = if body.chars().count() > room {
            format!(
                "{}\u{2026}",
                body.chars().take(room - 1).collect::<String>()
            )
        } else {
            body
        };
        format!("{head}\u{201c}{body}\u{201d}")
    }

    fn v2_pane(question: &str, reasoning: &str) -> String {
        let head = format!(
            "CSR's dreamer asked this while you worked, quoted: \u{201c}{}\u{201d} Its reasoning: ",
            straight(question)
        );
        envelope(&head, reasoning, 700)
    }

    fn v2_card(stage: &str, body: &str) -> String {
        envelope(
            &format!("CSR's compaction card ({stage}), quoted: "),
            body,
            1200,
        )
    }

    fn v2_story(rev: usize, body: &str) -> String {
        envelope(
            &format!("CSR's story of this session, rev {rev}, quoted: "),
            body,
            700,
        )
    }

    fn v2_band(question: &str, title: &str, reasoning: &str) -> String {
        format!(
            "\u{201c}{}\u{201d}\n\nCSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n> **{title}**\n> {reasoning}",
            straight(question)
        )
    }

    #[test]
    fn v2_forwards_round_trip_to_exactly_the_typed_reply() {
        let long =
            "Two surfaces already read the same inks, and the \u{201c}deck\u{201d} does too. "
                .repeat(30);
        let sends = [
            v2_pane("What if we cache it?", "Two callers read the same file."),
            v2_pane("What if we cache it?", &long),
            v2_card(
                "compacted by csr",
                "Context: 274k \u{2192} 70k tokens. Learnt: \u{201c}Never\u{201d} skip validation.",
            ),
            v2_card("compacting", &long),
            v2_story(3, "The session covered iOS fixes for Bold."),
            v2_story(3, &long),
        ];
        for send in &sends {
            own_words_agree(send, None);
            own_words_agree(
                &format!("Before I send this: {send} and after it, my reply."),
                Some("Before I send this: and after it, my reply."),
            );
            own_words_agree(&format!("2{send}\nmy reply"), Some("my reply"));
            own_words_agree(&format!("> {send} Keep it."), Some("Keep it."));
        }
        // The cut ones end on `…”` inside their limit.
        assert_eq!(sends[1].chars().count(), 700);
        assert!(sends[1].ends_with("\u{2026}\u{201d}"));
        assert_eq!(sends[3].chars().count(), 1200);
        // Two forwards on one line, the person's words between them.
        own_words_agree(&format!("{} then {}", sends[0], sends[2]), Some("then"));

        let band = v2_band("What if we reuse the palette?", "Reuse", "Shared colours.");
        own_words_agree(&band, None);
        own_words_agree(
            &format!("Earlier note.\n\n{band}\n\nNo, keep the current palette."),
            Some("Earlier note.\n\nNo, keep the current palette."),
        );
    }

    #[test]
    fn v2_band_keeps_the_persons_words_before_the_question_on_its_line() {
        let band = v2_band("What if we reuse the palette?", "Reuse", "Shared colours.");
        let typed = format!("Keep the deployment frozen. {band}\nNo, keep the current palette.");
        own_words_agree(
            &typed,
            Some("Keep the deployment frozen.\nNo, keep the current palette."),
        );
        assert_eq!(
            strip_dreamer_prompt(&typed).as_deref(),
            Some("Keep the deployment frozen.\nNo, keep the current palette.")
        );
        // The whole v2 message is still the dreamer's prompt.
        assert!(is_dreamer_prompt(&band));
        assert!(is_csr_emission(&band));
    }

    #[test]
    fn v1_legacy_forwards_never_look_for_a_closing_quote() {
        // A legacy body that opens with a quoted phrase goes whole (astra r3 #6).
        for legacy in [
            "CSR's compaction card (compacted by csr): \u{201c}Queue\u{201d} was moved. Never skip validation.",
            "CSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: \u{201c}Queue\u{201d} was moved. Never skip validation.",
            "CSR's story of this session, rev 2: \u{201c}Queue\u{201d} was moved. Never skip validation.",
        ] {
            own_words_agree(legacy, None);
            own_words_agree(&format!("{legacy}\nKeep it."), Some("Keep it."));
            own_words_agree(&format!("Look: {legacy}"), Some("Look:"));
        }
        // A 700-character legacy send ends at its cut; the reply after it, curly quotes and all, is the person's
        // (astra r3 #5: a quote search crossed the cut).
        let head =
            "CSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: ";
        let sent = format!("{head}{}\u{2026}", "x".repeat(699 - head.chars().count()));
        own_words_agree(
            &format!("{sent} Keep \u{201c}my words\u{201d} exactly."),
            Some("Keep \u{201c}my words\u{201d} exactly."),
        );
        // The legacy band, line by line; personal words before its question on that line cannot be told apart
        // and go with it (a documented legacy-only limit).
        own_words_agree(BAND, None);
        own_words_agree(&format!("Keep it. {BAND}"), None);
    }

    #[test]
    fn a_question_with_curly_quotes_of_its_own_is_cut_whole() {
        let send = v2_pane(
            "What if the \u{201c}Queue\u{201d} view moved?",
            "It sits beside \u{201c}Lost and found\u{201d}.",
        );
        assert!(send.contains("\u{201c}Queue\" view"));
        own_words_agree(&format!("{send} No."), Some("No."));
    }

    #[test]
    fn cjk_forwards_are_cut_by_the_same_grammar() {
        let card = v2_card(
            "compacted by csr",
            "\u{30b3}\u{30f3}\u{30c6}\u{30ad}\u{30b9}\u{30c8}\u{ff1a}274k \u{2192} 70k\u{3002}\u{691c}\u{8a3c}\u{3092}\u{98db}\u{3070}\u{3055}\u{306a}\u{3044}\u{3002}",
        );
        own_words_agree(&card, None);
        own_words_agree(
            &format!("\u{898b}\u{3066}\u{ff1a}{card}\u{8fd4}\u{4e8b}\u{3067}\u{3059}\u{3002}"),
            Some("\u{898b}\u{3066}\u{ff1a} \u{8fd4}\u{4e8b}\u{3067}\u{3059}\u{3002}"),
        );
        let question = "\u{30ad}\u{30e3}\u{30c3}\u{30b7}\u{30e5}\u{3092}\u{5171}\u{6709}\u{3057}\u{305f}\u{3089}\u{3069}\u{3046}\u{3067}\u{3057}\u{3087}\u{3046}\u{304b}\u{ff1f}";
        let dream = v2_pane(question, &"\u{4e8c}\u{3064}\u{306e}\u{753b}\u{9762}\u{304c}\u{540c}\u{3058}\u{8272}\u{3092}\u{8aad}\u{3080}\u{3002}".repeat(80));
        assert_eq!(dream.chars().count(), 700);
        own_words_agree(
            &format!("{dream}\u{3044}\u{3044}\u{3048}\u{3002}"),
            Some("\u{3044}\u{3044}\u{3048}\u{3002}"),
        );
        own_words_agree(
            &v2_band(question, "\u{8272}", "\u{540c}\u{3058}\u{8272}\u{3002}"),
            None,
        );
        // A legacy card cut at 1,200 CJK characters, then the person's reply.
        let head = "CSR's compaction card (compacting): ";
        let legacy = format!(
            "{head}{}\u{2026}",
            "\u{8981}".repeat(1199 - head.chars().count())
        );
        own_words_agree(
            &format!("{legacy}\u{3044}\u{3044}\u{3048}"),
            Some("\u{3044}\u{3044}\u{3048}"),
        );
    }

    #[test]
    fn astra_r2_2_feedback_about_a_story_is_the_persons() {
        // The broad line pass cut from the story's opening words to the end of the line, without its
        // revision delimiter, and took the person's sentence with it.
        let own = "CSR's story of this session, rev 3 is wrong; keep my rollback plan.";
        let input = format!(
            "{own}\n\nCSR's compaction card (compacting), quoted: \u{201c}Summarizing.\u{201d}"
        );
        own_words_agree(&input, Some(own));
        own_words_agree(own, Some(own));
    }

    #[test]
    fn astra_r2_8_a_number_between_two_forwards_is_the_persons() {
        let card = |body: &str| {
            format!("CSR's compaction card (compacting), quoted: \u{201c}{body}\u{201d}")
        };
        own_words_agree(&format!("{} 42 {}", card("One."), card("Two.")), Some("42"));
        // The single hotkey digit the band leaves before a send still goes with it.
        own_words_agree(&format!("2{}\nmy reply", card("One.")), Some("my reply"));
        own_words_agree(&format!("42{}", card("One.")), Some("42"));
    }

    #[test]
    fn astra_5_a_sentinel_line_goes_and_the_line_beside_it_stays() {
        let input = "Keep the deployment frozen.\n[[CSR:RECAP]] generated recap";
        assert_eq!(
            crate::import::scrub_contaminated_text(input).as_deref(),
            Some("Keep the deployment frozen.")
        );
        assert_eq!(strip_csr_emissions(input), "Keep the deployment frozen.");
        // A recap line inside the person's paragraph, without its sentinel.
        let recap = "Keep the deployment frozen.\nrecap [2h ago]: shipped the cap. Next: tag it.";
        assert_eq!(
            crate::import::scrub_contaminated_text(recap).as_deref(),
            Some("Keep the deployment frozen.")
        );
    }

    #[test]
    fn astra_8_a_forward_past_the_header_window_marks_the_text_contaminated() {
        let input = format!(
            "{}\n\nCSR's compaction card (compacting): Summarizing 278k.",
            "Keep this user requirement. ".repeat(16)
        );
        assert!(input.find("CSR's compaction card").unwrap() > HEADER_WINDOW);
        assert!(crate::import::contamination_reason(&input).is_some());
    }

    #[test]
    fn strip_forwarded_note_cuts_opener_and_quote_keeps_prose() {
        let note = "Here is a note offered by a side agent:\n\n> **T**\n>\n> body [commit f154ead]\n\n> second quote";
        assert_eq!(strip_forwarded_note(note).as_deref(), Some(""));
        let with_prose = format!("{note}\n\nwhat now?\nand more");
        assert_eq!(
            strip_forwarded_note(&with_prose).as_deref(),
            Some("what now?\nand more")
        );
        let csr = "Here is a note offered by CSR (claude-self-reflect):\n> x";
        assert_eq!(strip_forwarded_note(csr).as_deref(), Some(""));
    }

    #[test]
    fn strip_forwarded_note_is_none_for_non_forwards() {
        assert_eq!(strip_forwarded_note("plain prose"), None);
        assert_eq!(
            strip_forwarded_note("I wonder why here is a note offered by YSK shows up"),
            None
        );
        // opener line does not end with a colon: not a forward
        assert_eq!(
            strip_forwarded_note("Here is a note offered by a side agent\n> x"),
            None
        );
    }

    // --- strip_plumbing ---

    #[test]
    fn plumbing_strips_command_wrappers_keeps_nothing() {
        let text = "<command-message>memory-feedback</command-message>\n\
                    <command-name>/memory-feedback</command-name>";
        assert!(strip_plumbing(text).trim().is_empty());
    }

    #[test]
    fn plumbing_strips_unclosed_caveat_to_end() {
        // Round-3 regression: CONTINUUM request showed the raw caveat wrapper.
        let text = "<local-command-caveat>Caveat: The messages below were generated \
                    by the user while running local commands. DO NOT respond";
        assert!(strip_plumbing(text).trim().is_empty());
    }

    #[test]
    fn plumbing_keeps_real_prompt_around_reminder() {
        let text = "fix the auth bug <system-reminder>hook noise</system-reminder> in login.rs";
        let out = strip_plumbing(text);
        assert!(out.contains("fix the auth bug"));
        assert!(out.contains("in login.rs"));
        assert!(!out.contains("hook noise"));
    }

    // --- strip_quoted ---

    #[test]
    fn quoted_strips_fences_inline_and_blockquotes() {
        let text = "Fixed the parser.\n```\nNEXT: garbage\n```\nSee `LAST: token` and\n> TODOS: quoted\nDone.";
        let out = strip_quoted(text);
        assert!(out.contains("Fixed the parser."));
        assert!(out.contains("Done."));
        assert!(!out.contains("garbage"));
        assert!(!out.contains("token"));
        assert!(!out.contains("quoted"));
    }

    #[test]
    fn quoted_drops_unclosed_fence_tail() {
        let out = strip_quoted("Real prose.\n```\nNEXT: never closed");
        assert!(out.contains("Real prose."));
        assert!(!out.contains("never closed"));
    }

    // --- is_csr_emission ---

    #[test]
    fn emission_detects_headers() {
        assert!(is_csr_emission("CSR CONTINUUM [2m ago]: stuff"));
        assert!(is_csr_emission("## Session Intelligence (CSR v9.2)\n..."));
        assert!(is_csr_emission("## CSR Memory Feedback — 2026-06-10"));
        assert!(is_csr_emission("You are CSR Episode Analyst. Summarize..."));
    }

    #[test]
    fn emission_detects_the_dreamers_prompt_sent_as_a_user_turn() {
        // The dreamer's question leads; its marker sentence follows within the header window.
        let sent = "What if the pane took its colours from pigment's nyt inks?\n\n\
                    CSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**";
        assert!(is_csr_emission(sent));
        assert!(!is_csr_emission(
            "what if the dreamer asked fewer questions while I work?"
        ));
    }

    #[test]
    fn dreamer_prompt_is_recognised_only_in_its_exact_whole_message_grammar() {
        let sent = "What if the pane took its colours from pigment's nyt inks?\n\n\
                    CSR's dreamer asked this while you worked. Its reasoning:\n\n\
                    > **Share the palette**\n>\n> Two surfaces already read the same inks.";
        assert!(is_dreamer_prompt(sent));
        assert!(is_dreamer_prompt(&sent.replace('\n', "\r\n")));
        // the question may be absent
        assert!(is_dreamer_prompt(
            "CSR's dreamer asked this while you worked. Its reasoning:\n\n> **t**\n> body"
        ));
        // prose that mentions it, extra person-typed prose, or an unquoted tail is not the grammar
        assert!(!is_dreamer_prompt(
            "what if the dreamer asked fewer questions while I work?"
        ));
        assert!(!is_dreamer_prompt(
            "I wrote this myself.\n\nSecond thought of mine.\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> q"
        ));
        assert!(!is_dreamer_prompt(
            "Q?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\nand then I typed more"
        ));
        assert!(!is_dreamer_prompt(
            "Q?\n\nCSR's dreamer asked this while you worked. Its reasoning:"
        ));
    }

    #[test]
    fn a_reply_under_a_long_hindi_dream_keeps_only_the_persons_words() {
        // Codex review 10-12, #14: the person's reply under a forwarded dream breaks the whole-message grammar, and
        // a 223-character Devanagari question (553 bytes) puts the attribution past HEADER_WINDOW: the question was
        // kept as the person's prose, and strip_csr_emissions cut at the attribution, dropping the reply instead.
        let question = format!(
            "\u{915}\u{94d}\u{92f}\u{93e} \u{939}\u{94b} \u{905}\u{917}\u{930} {}?",
            "\u{939}\u{92e} \u{92a}\u{947}\u{928} \u{915}\u{947} \u{930}\u{902}\u{917} \u{921}\u{947}\u{915} \u{915}\u{940} \u{938}\u{94d}\u{92f}\u{93e}\u{939}\u{940} \u{938}\u{947} \u{932}\u{947}\u{902} "
                .repeat(6)
        );
        assert!(question.len() > HEADER_WINDOW);
        let forward = format!(
            "{question}\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**\n> * Two surfaces read the same inks."
        );
        let reply = format!("{forward}\n\nNo, keep the current palette.");
        let mine = Some("No, keep the current palette.");
        assert_eq!(strip_dreamer_prompt(&reply).as_deref(), mine);
        assert_eq!(extractable(&reply).as_deref(), mine);
        assert_eq!(Some(strip_csr_emissions(&reply).trim()), mine);
        assert_eq!(
            crate::import::scrub_contaminated_text(&reply).as_deref(),
            mine
        );
        assert_eq!(strip_dreamer_prompt(&forward).as_deref(), Some(""));
        assert!(extractable(&forward).is_none());
        assert_eq!(strip_csr_emissions(&forward), "");
        // the pane's one-line send, words above and below it
        let pane = "Look.\n\nCSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: x.\n\nNot now.";
        assert_eq!(
            strip_dreamer_prompt(pane).as_deref(),
            Some("Look.\n\nNot now.")
        );
        // no dreamer message at all: None, and the text is left to the other checks
        assert_eq!(
            strip_dreamer_prompt("what if the dreamer asked less?"),
            None
        );
    }

    #[test]
    fn emission_detects_the_compaction_cards_draft_a_click_puts_in_the_prompt() {
        // Codex review 10-12, #13: a double click on the companion's done compaction card puts this draft in the
        // prompt box; sent, it was imported as the person's words, its "Learnt:" lines as their own.
        let card = "CSR's compaction card (compacted by csr): Context: 274k \u{2192} 70k tokens. \
                    Each API call now sends 204k fewer tokens. Learnt: Never skip validation.";
        assert!(is_csr_emission(card));
        assert!(extractable(card).is_none());
        for stage in ["compacting soon", "compacting", "compaction skipped"] {
            assert!(is_csr_emission(&format!(
                "CSR's compaction card ({stage}): 278k"
            )));
        }
        // the person's own words about the card stay theirs
        assert!(!is_csr_emission(
            "the compaction card should say it in prompts"
        ));
    }

    #[test]
    fn dreamer_prompt_in_any_language_is_held_past_the_header_window() {
        // The dreamer writes in the person's language. Devanagari is 3 bytes a character, so a long Hindi question
        // pushes the attribution past HEADER_WINDOW; the whole-message grammar must still hold it as CSR's own.
        let hindi = "क्या होगा अगर पैन अपने रंग पिगमेंट की स्याही से ले, ताकि दोनों सतहें एक ही रंग पढ़ें और हर बार नया रंग चुनने की ज़रूरत न पड़े, और पुराने सत्रों में जो रंग तय हुए थे वे भी वहीं से आएँ?";
        let japanese = "ペインの色をピグメントのインクから取ったら、二つの画面が同じ色を読み、毎回新しい色を選ぶ必要がなくなり、過去のセッションで決めた色もそこから来て、ステータス行や情報ページの図も同じインクで描かれ、利用者が以前に一度だけ選んだ配色がすべての画面で保たれるのではないでしょうか？";
        let mixed = "क्या होगा अगर pane अपने colours pigment की nyt inks से ले, ताकि दोनों surfaces एक ही palette पढ़ें?";
        for question in [hindi, japanese, mixed] {
            let sent = format!(
                "{question}\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **रंग साझा करें**\n>\n> दो सतहें पहले से वही स्याही पढ़ती हैं।"
            );
            assert!(
                question.len() > HEADER_WINDOW || question == mixed,
                "{}",
                question.len()
            );
            assert!(is_dreamer_prompt(&sent), "{question}");
            assert!(is_csr_emission(&sent), "{question}");
            assert_eq!(
                crate::import::scrub_contaminated_text(&sent),
                None,
                "{question}"
            );
        }
        // The person's own non-English question is still theirs.
        assert!(!is_csr_emission(hindi));
        assert!(!is_dreamer_prompt(japanese));
    }

    #[test]
    fn emission_detects_the_companions_compaction_keep_list_and_notice() {
        assert!(is_csr_emission(
            "CSR COMPACT KEEP — in the summary, keep these, in this order, ids and SHAs word for word:\n\
             1. Running in the background: subagent a427c126 (Explore CapCut)."
        ));
        assert!(is_csr_emission(
            "CSR compacted at 312k → 66k · pays back in 3 turns · kept: 2 running agents, 1 open decision"
        ));
        // The owner's own words about compacting stay theirs.
        assert!(!is_csr_emission(
            "compact at 250k from now on, and keep the agents running"
        ));
    }

    #[test]
    fn emission_detects_field_token_echo() {
        // Round-2 regression: probe instruction text extracted as next_steps.
        assert!(is_csr_emission(
            "NEXT: NEXT:/TODOS:/ANCHORS: lines) - briefing block - Do NOT count CLAUDE.md"
        ));
    }

    #[test]
    fn emission_allows_single_token_in_prose() {
        assert!(!is_csr_emission(
            "next: fix the TODOS: counter display in the statusline"
        ));
        assert!(!is_csr_emission("next step: deploy to production"));
        assert!(!is_csr_emission(
            "Fixed token validation, added regression test"
        ));
    }

    #[test]
    fn emission_header_only_matters_in_leading_window() {
        let mut text = "a".repeat(500);
        text.push_str("CSR CONTINUUM [old]");
        assert!(!is_csr_emission(&text));
    }

    // --- extractable (full pipeline, probe-round regressions) ---

    #[test]
    fn extractable_rejects_pasted_probe_report() {
        let report = "## CSR Memory Feedback — 2026-06-10 — session in claude-self-reflect\n\
                      - **variant**: unknown\n- **noise**: CSR CONTINUUM block garbled";
        assert_eq!(extractable(report), None);
    }

    #[test]
    fn extractable_rejects_caveat_only_message() {
        assert_eq!(
            extractable("<local-command-caveat>Caveat: The messages below were generated"),
            None
        );
    }

    #[test]
    fn extractable_keeps_prose_drops_quoted_tokens() {
        // Round-3 regression: assistant summary quoting injected fields in
        // backticks. The quoted tokens go; the genuine prose stays.
        let text = "`NEXT: none recorded` — polluted boilerplate filtered, \
                    including episodes already in the DB. Commit:";
        let out = extractable(text).expect("prose should survive");
        assert!(!out.contains("NEXT:"));
        assert!(out.contains("polluted boilerplate filtered"));
    }

    #[test]
    fn quoted_keeps_short_identifier_spans() {
        // Round-4 regression: "commit `4343b50`, deployed" rendered as
        // "commit , deployed". Short identifier spans keep their content.
        let out = strip_quoted("One-shot fix done — commit `4343b50`, deployed to `/usr/local`.");
        assert!(out.contains("commit 4343b50, deployed"));
        // ...but spans carrying emission tokens stay stripped regardless.
        let out = strip_quoted("shows `NEXT: garbage` now");
        assert!(!out.contains("NEXT:"));
        // ...and long spans are mini quoted blocks, stripped.
        let long = format!("see `{}` for details", "x".repeat(80));
        assert!(!strip_quoted(&long).contains("xxx"));
    }

    #[test]
    fn extractable_rejects_probe_command_text() {
        // The /memory-feedback command body after plumbing-tag stripping.
        let probe = "CSR Memory Feedback Probe You are reporting on the quality \
                     of the memory context CSR injected into THIS session.";
        assert_eq!(extractable(probe), None);
    }

    #[test]
    fn substantive_rejects_metadata_and_keeps_prose() {
        // Round-6: bare timestamp request and command-only rolling summary.
        assert!(!is_substantive("20260611-122252"));
        assert!(!is_substantive("memory-feedback"));
        assert!(!is_substantive(""));
        assert!(!is_substantive(
            "<command-message>memory-feedback</command-message>"
        ));
        // Real work descriptions pass.
        assert!(is_substantive("fix the auth bug in login.rs"));
        assert!(is_substantive(
            "Refactored self-reference filtering into provenance"
        ));
    }

    #[test]
    fn extractable_keeps_normal_session_text() {
        let out = extractable("Fix the authentication bug in the login handler");
        assert_eq!(
            out.as_deref(),
            Some("Fix the authentication bug in the login handler")
        );
    }

    // --- RECAP_SENTINEL ---

    #[test]
    fn sentinel_detected_regardless_of_position_or_preamble() {
        let long_preamble = "a".repeat(HEADER_WINDOW + 200);
        let text = format!("{long_preamble} trailing note {RECAP_SENTINEL} more text");
        assert!(is_csr_emission(&text));
    }

    #[test]
    fn lessons_sentinel_rejects_machine_generated_candidates() {
        let text = "human-looking preamble\n[[CSR:LESSONS]]\n- Never bypass checks";
        assert!(is_csr_emission(text));
        assert!(extractable(text).is_none());
    }

    #[test]
    fn sentinel_survives_zero_width_noise() {
        let noisy: String = format!("preamble {RECAP_SENTINEL} tail")
            .chars()
            .flat_map(|c| [c, '\u{200B}'])
            .collect();
        assert!(is_csr_emission(&noisy));
    }

    #[test]
    fn sentinel_survives_confusable_bracket_substitution() {
        let swapped = RECAP_SENTINEL
            .replace('[', "\u{FF3B}")
            .replace(']', "\u{FF3D}");
        let text = format!("some prose then {swapped} more prose");
        assert!(is_csr_emission(&text));
    }

    #[test]
    fn sentinel_absent_from_genuine_prose_about_recaps() {
        assert!(!is_csr_emission(
            "Let's recap what we discussed: the login bug is fixed and deployed."
        ));
        assert!(!is_csr_emission(
            "The recap feature works great, though sometimes it's a bit verbose."
        ));
        assert!(!is_csr_emission(
            "I want to recap [the auth changes], then prioritize them."
        ));
    }

    // --- extractable ordering: sentinel must be seen before strip_quoted ---

    #[test]
    fn extractable_rejects_blockquoted_recap_alongside_unquoted_preamble() {
        // Regression: if strip_quoted ran first, it would erase every
        // blockquoted line (including the sentinel), leaving the unquoted
        // preamble looking like genuine session content and leaking it
        // through as Some(..).
        let quoted_recap = format!(
            "> recap [2h ago]: fixed the bug: shipped it. {RECAP_SENTINEL}\n> Next: ship it."
        );
        let text = format!(
            "some unrelated preamble text that reads like genuine session content\n{quoted_recap}"
        );
        assert!(extractable(&text).is_none());
    }

    #[test]
    fn extractable_keeps_unquoted_prose_alongside_quoted_historical_header() {
        // Regression: full is_csr_emission before strip_quoted over-rejected
        // genuine unquoted content that merely co-occurred with a quoted
        // historical EMISSION_HEADERS echo (no sentinel present).
        let text = "> SESSION CONTINUITY DETECTED: old injected context\n\
                    I actually fixed the login bug today, deployed and verified.";
        let got = extractable(text).expect("unquoted genuine prose must survive");
        assert_eq!(
            got.trim(),
            "I actually fixed the login bug today, deployed and verified."
        );
    }

    // --- strip_csr_emissions (query side of the self-echo guard) ---

    /// The prompt pasted live on 2026-10-10 (the pane's hand-off send text plus
    /// the person's question); the session id inside is replaced by a dummy.
    const PASTED_HANDOFF: &str = "CSR handed you these this session: Corpora ingestion and watcher setup · 2 weeks ago · csr-engine · 5×; Hairline skill retrieval and agent proposal · 2 days ago · csr-engine · 3×; Implement the following plan: · 7 months ago · 2×; Implement the following plan: · 7 months ago; When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it - what enrichment is needed to catch \" · 5 months ago <<< This Handed to Claude seems not useful thoughts?";

    #[test]
    fn pasted_handoff_list_keeps_only_the_persons_question() {
        assert_eq!(
            strip_csr_emissions(PASTED_HANDOFF),
            "<<< This Handed to Claude seems not useful thoughts?"
        );
        // Words before the paste are the person's too.
        let both = strip_csr_emissions(&format!("why did you get these? {PASTED_HANDOFF}"));
        assert_eq!(
            both.split_whitespace().collect::<Vec<_>>().join(" "),
            "why did you get these? <<< This Handed to Claude seems not useful thoughts?"
        );
    }

    #[test]
    fn truncated_send_text_ends_at_the_panes_ellipsis() {
        // one_line(text, 700) cuts at 699 characters and appends an ellipsis.
        let item = "Assembled a seven-track channel slate from samples · 8 weeks ago · Campaigns; ";
        let body: String = format!("{PANE_HANDED}{}", item.repeat(20))
            .chars()
            .take(PANE_SEND_WIDTH)
            .collect();
        let sent = format!("{body}… are any of these useful for the reel?");
        assert_eq!(
            strip_csr_emissions(&sent),
            "are any of these useful for the reel?"
        );
    }

    /// pane.py `lost_asks`: both send forms, as typed into the prompt. The
    /// thread text is from session 3002ab28 (anukriti-website).
    const LOST_SIDE: &str = "Lost and found: “The IG post time is a soft follow-up. Then ask for the release go-ahead (allowed, since publishing needs approval). For example: on \"ship\", I'll pull the coda from the hub (or keep it if the app is updated first), merge PR #30, run IndexNo…” went quiet after 19:38. Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line.";

    #[test]
    fn lost_and_found_sends_are_cut_out() {
        assert_eq!(strip_csr_emissions(LOST_SIDE), "");
        let sub = "Lost and found: “Retry the upload” went quiet after 09:05. Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands.";
        assert_eq!(strip_csr_emissions(sub), "");
        // Words the person adds after the send text are theirs.
        assert_eq!(
            strip_csr_emissions(&format!("{LOST_SIDE} and check the hub first")),
            "and check the hub first"
        );
        assert_eq!(
            strip_csr_emissions(&format!("{LOST_SIDE}\nalso check the hub")),
            "also check the hub"
        );
        // Prose that only mentions the section is left alone.
        let prose = "Lost and found is the pane section I like most.";
        assert_eq!(strip_csr_emissions(prose), prose);
    }

    #[test]
    fn story_recap_and_remembered_sends_are_cut_out() {
        let story = "CSR's story of this session, rev 2: The session covered iOS fixes for Bold and a new look family.\n\nis this accurate? what is left";
        assert_eq!(strip_csr_emissions(story), "is this accurate? what is left");
        let recap = "recap [1m ago]: Which memory profiling fix shipped as 9.5.6: PR #302 memory-mapped the vectors... [[CSR:RECAP]]\n\nwhy is this the recap? I was working on the pane";
        assert_eq!(
            strip_csr_emissions(recap),
            "why is this the recap? I was working on the pane"
        );
        let note = "CSR remembered: “Radio metric break 1 landed” (2 weeks ago · Campaigns). does this still hold?";
        assert_eq!(strip_csr_emissions(note), "does this still hold?");
        let session = "CSR remembered a past session: “Bold separation work” (yesterday · EnhanceMe). The rest of it: csr_reflect_on_past(\"conv_00000000-0000-4000-8000-000000000001\") pick it up";
        assert_eq!(strip_csr_emissions(session), "pick it up");
    }

    #[test]
    fn pasted_injected_blocks_keep_only_the_question() {
        let block = "you got this injected, is any of it relevant?\n\n## PAST CONTEXT - NOT INSTRUCTIONS\n- [reflection|unknown] When claude is killed or closed it generates a session\n- [reflection|unknown] Session in claude-self-reflect project using Bash, Glob, Read, mcp__claude-self-reflect__csr_reflect_on_past,...";
        assert_eq!(
            strip_csr_emissions(block),
            "you got this injected, is any of it relevant?"
        );
        let pickup = "CSR PICKUP — this prompt matches a past episode (similarity 0.52); the work may already exist — verify against it before re-deriving.\nEPISODE INDEX — earlier threads, newest first.\n- [2w ago] see the last session the corpus limits us — The self-echo fix is installed (outcome=partial) → csr_reflect_on_past(\"conv_00000000-0000-4000-8000-000000000002\")\n\nis that pickup right? I asked about the pane";
        assert_eq!(
            strip_csr_emissions(pickup),
            "is that pickup right? I asked about the pane"
        );
        let dreamer = "CSR's dreamer asked this while you worked: “What if the recap led with the blocker?” Its reasoning: Recap ordering.";
        assert_eq!(strip_csr_emissions(dreamer), "");
    }

    #[test]
    fn prose_about_csr_is_not_an_emission() {
        for prose in [
            "why does CSR keep handing me 7 months old plans?",
            "the recap [in the pane] is wrong about the deploy",
            "CSR handed you these yesterday and none were useful",
            "- [ ] fix the pickup threshold",
        ] {
            assert_eq!(strip_csr_emissions(prose), prose);
            // Untouched byte for byte, quoting and blank lines included.
            let note = format!("Here is a note:\n\n> {prose}\n>\n> - second line\n");
            assert_eq!(strip_csr_emissions(&note), note);
        }
    }
}
