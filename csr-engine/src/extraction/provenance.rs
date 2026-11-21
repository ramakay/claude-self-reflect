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
//! distinctive field tokens) here, in the same commit that adds the formatter,
//! and its exact grammar in `import::contamination::REGISTRY`. The header
//! strings below are the registry's own constants: one place to rename them.
//! This module judges what to *extract*; the registry decides what to *remove*
//! from the corpus, and only by span, owner and channel. The two must not be
//! confused: a mention that extraction declines to learn from is still the
//! person's words.

use crate::import::contamination;

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
///   may echo) and the one-line notice it leaves after compacting
///   (the removed `compact.ts` `KEEP_HEADER`, `NOTICE_HEADER`)
///
/// The companion's forwarded dream and compaction card draft are NOT headers here: the person
/// talks about both in their own words ("Please change CSR's compaction card (the one in the
/// sidebar) ...", "CSR's dreamer asked this while you worked yesterday, but ..."), which a
/// substring match took for the emission (Codex review r2, #6 and #7). They are matched by
/// their emitted grammar instead: [`strip_csr_forwards`].
const EMISSION_HEADERS: [&str; 17] = [
    contamination::MEMORY_MANIFEST_HEADER,
    contamination::CONTINUUM_HEADER,
    contamination::PICKUP_HEADER,
    contamination::EPISODE_INDEX_HEADER_PREFIX,
    contamination::SESSION_INTELLIGENCE_HEADER,
    contamination::MEMORY_FEEDBACK_REPORT_HEADER,
    contamination::MEMORY_FEEDBACK_PROBE_HEADER,
    contamination::CONTINUITY_HEADER_PREFIX,
    contamination::RECENT_SESSIONS_HEADER_PREFIX,
    contamination::RELEVANT_CONTEXT_HEADER_PREFIX,
    contamination::PAST_CONTEXT_HEADER_PREFIX,
    OPEN_BLOCK_HEADER,
    "You are CSR Episode Analyst",
    "You are summarizing a coding session",
    "# Ratification Dialog-Act Extraction",
    // The companion's keep list and notice (the removed `compact.ts` KEEP_HEADER, NOTICE_HEADER): old
    // transcripts carry these two exact strings, so they stay literals.
    "CSR COMPACT KEEP —",
    "CSR compacted at",
];

/// Field tokens CSR emits inside blocks (Tier-0 fields, continuity lines).
/// Case-sensitive on purpose: our emissions are uppercase-exact; genuine prose
/// ("next: fix the todos counter") is not. One token can occur in real text;
/// two or more distinct tokens means the text is echoing our format.
pub(crate) const EMISSION_FIELD_TOKENS: [&str; 9] = [
    "LAST:",
    "NEXT:",
    "TODOS:",
    "ANCHORS:",
    "ANCHORS (last session's files):",
    "CONTINUED FROM [",
    "Past session [",
    "(outcome=",
    "csr_reflect_on_past(",
];

/// First line of the SessionStart `Open` block (`hooks::open_items`): the pending
/// items of the predecessor's newest compact summary, one `- ` bullet each. The
/// bullets are free text from that summary, so the block is recognised by this
/// exact line, anywhere in a text and under any bullet, quote or emphasis
/// wrapping ([`has_open_block_header`]), not by its bullets.
pub const OPEN_BLOCK_HEADER: &str =
    "Open [from the last compact summary, history to check, not tasks]:";

/// Whether any line of `text` is the `Open` block's header, after the bullet,
/// quote and emphasis marks a re-paste adds in front of it.
pub fn has_open_block_header(text: &str) -> bool {
    normalize_for_sentinel_scan(text).lines().any(|line| {
        line.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '>' | '-' | '*' | '_' | '\u{2022}' | '#')
        })
        .starts_with(OPEN_BLOCK_HEADER)
    })
}

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

/// Carried by the SessionStart `learnt [<age>]: …` line (`hooks::learnt`),
/// inside its byte budget. Same contract as [`RECAP_SENTINEL`]: scanned across
/// the FULL normalized text, so the line is rejected as an emission however it
/// is quoted, bulleted or re-pasted, and never becomes evidence for the next one.
pub const LEARNT_SENTINEL: &str = "[[CSR:LEARNT]]";

/// Every machine-owned sentinel whose presence rejects the text outright.
pub(crate) const MACHINE_SENTINELS: [&str; 5] = [
    RECAP_SENTINEL,
    DREAM_SENTINEL,
    LESSONS_SENTINEL,
    LEARNT_SENTINEL,
    crate::transcript::receipt::SENTINEL,
];

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
pub(crate) const INLINE_CODE_KEEP_LEN: usize = 24;

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
    // The `Open` block's header line, at any depth and under any wrapping.
    if has_open_block_header(text) {
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
    // Single lines the formatters write that carry too few field tokens to
    // trip the count below once the outcome label is withheld.
    if INDEX_ENTRY_LINE.is_match(text)
        || CONTINUUM_LAST_LINE.is_match(text)
        || NATIVE_WARN_LINE.is_match(text)
        || COMPACT_POINTER_LINE.is_match(text)
    {
        return true;
    }
    EMISSION_FIELD_TOKENS
        .iter()
        .filter(|t| text.contains(**t))
        .count()
        >= 2
}

/// The attribution sentence the companion dreamer's "ask the agent" message carries today.
pub(crate) const DREAMER_ATTRIBUTION: &str = "Self-Reflect's dreamer asked this while you worked";

/// The attribution sentence before the Self-Reflect rename. Older transcripts carry it, so it is read
/// (suppressed) exactly as the current one is; only [`DREAMER_ATTRIBUTION`] is written.
pub(crate) const DREAMER_ATTRIBUTION_OLD: &str = "CSR's dreamer asked this while you worked";

/// Both wordings, current first.
const DREAMER_ATTRIBUTIONS: [&str; 2] = [DREAMER_ATTRIBUTION, DREAMER_ATTRIBUTION_OLD];

/// True when `text` holds either wording of the dreamer's attribution sentence.
pub(crate) fn has_dreamer_attribution(text: &str) -> bool {
    DREAMER_ATTRIBUTIONS.iter().any(|a| text.contains(a))
}

/// True when `text` opens with either wording of the attribution sentence.
pub(crate) fn starts_with_dreamer_attribution(text: &str) -> bool {
    DREAMER_ATTRIBUTIONS.iter().any(|a| text.starts_with(a))
}

/// The band's attribution paragraph, word for word. Forward grammar v2 (`.plans/release-10.1/forward-grammar.md`)
/// marks its own with `quoted`: [`DREAMER_BAND_QUOTED`]. Legacy (v1) forms already sit in transcripts:
/// `askClaude` (the removed `dreamer.ts`) wrote the first, the recap fixtures in hooks/stop.rs
/// (1d23d675) carry the second. A paragraph that only opens with the attribution's words is the person's: "...
/// while you worked yesterday, but I wrote today's question myself." (Codex review r2, #7).
const DREAMER_BAND_QUOTED: &str =
    "Self-Reflect's dreamer asked this while you worked, quoted. Its reasoning:";
const DREAMER_BAND_QUOTED_OLD: &str =
    "CSR's dreamer asked this while you worked, quoted. Its reasoning:";
const DREAMER_BAND_LEGACY: [&str; 4] = [
    "Self-Reflect's dreamer asked this while you worked. Its reasoning:",
    "Self-Reflect's dreamer asked this while you worked. It is a question, not a task.",
    "CSR's dreamer asked this while you worked. Its reasoning:",
    "CSR's dreamer asked this while you worked. It is a question, not a task.",
];

/// The v2 (`quoted`) band attribution, in either wording.
fn is_band_quoted(line: &str) -> bool {
    line == DREAMER_BAND_QUOTED || line == DREAMER_BAND_QUOTED_OLD
}

fn is_band_attribution(line: &str) -> bool {
    let line = line.trim();
    is_band_quoted(line) || DREAMER_BAND_LEGACY.contains(&line)
}

/// What marks a v2 one-line forward right after its header: its body follows, wrapped in curly quotes. The
/// emitter makes every ” inside a body a straight quote, so the first ” after the opening “ closes it.
const QUOTED_OPEN: &str = ", quoted: \u{201c}";

/// The pane's one-line send of a dream (the removed `pane.py` `dream_send`), at most 700 characters:
/// - v2: `CSR's dreamer asked this while you worked, quoted: “<question>” Its reasoning: “<reasoning>”`, ending at
///   the reasoning's closing ”.
/// - v1: `CSR's dreamer asked this while you worked: “<question>” Its reasoning: <title>. <reasons>`. Never
///   searched for a closing quote: it ends at `one_line`'s cut (`…` as its 700th character), else at the end of
///   its line. A question is at most 240 characters (the removed `dreamer.ts` `parseDream`), so a cut never falls before "Its
///   reasoning:".
const DREAMER_PANE_LEGACY_OPEN: &str = ": \u{201c}";
const DREAMER_PANE_LEGACY_REASONING: &str = "\u{201d} Its reasoning:";
const DREAMER_PANE_QUOTED_REASONING: &str = " Its reasoning: \u{201c}";
const DREAMER_PANE_SEND_CHARS: usize = 700;

/// The companion's compaction card draft a click puts in the prompt (the removed `pane.py` `compact_section`), the stage its
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

/// The session story's draft (the removed `pane.py`), at most 700 characters. v2: `CSR's story of this session, rev <n>, quoted:
/// “<body>”`. v1: `CSR's story of this session, rev <n>: <body>`, ending at its cut or the end of its line.
const STORY_OPEN: &str = "CSR's story of this session, rev ";
const STORY_SEND_CHARS: usize = 700;

/// True when the WHOLE of `text` is the companion dreamer's "ask Claude"
/// message, in the exact grammar `askClaude`
/// (the removed `dreamer.ts`) writes: an optional one-line question, a blank line, the
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

/// Strip pane forwards, including side notes, retaining its paragraph separators.
pub fn strip_companion_forwards(text: &str) -> Option<String> {
    scan_companion_forwards(text, true).map(|found| found.own)
}

/// What [`scan_forwards`] found: the person's own words and where the first forward starts.
struct Forwards {
    /// The person's paragraphs, trimmed, blank ones dropped, joined by a blank line.
    own: String,
    /// Byte offset of the first forward in the CRLF-normalised, trimmed text.
    first: usize,
}

/// Byte offset just past the `…` the removed `pane.py`'s `one_line(text, chars)` appends when it cut a send that starts at
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
    let attribution = DREAMER_ATTRIBUTIONS
        .iter()
        .find(|a| line[start..].starts_with(**a))?;
    let after = start + attribution.len();
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
    let dream = DREAMER_ATTRIBUTIONS
        .iter()
        .filter_map(|attribution| {
            line[from..].match_indices(attribution).find_map(|(at, _)| {
                pane_dream_end(line, from + at).map(|(end, bounded)| (from + at, end, bounded))
            })
        })
        .min_by_key(|found| found.0);
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

/// Split the same-line prefix as P + M + D: D is one touching ASCII digit,
/// M is the longest run of spaces, tabs and quote markers immediately before it.
/// P includes any earlier kept non-whitespace text on this same line, even
/// across another forward. Only a standalone hotkey is cut; prose and
/// multi-digit numbers stay whole.
fn forward_prefix_start(piece: &str, earlier_prose: bool) -> usize {
    let digit = piece.as_bytes().last().is_some_and(u8::is_ascii_digit);
    let before_digit = piece.len() - usize::from(digit);
    let prefix = piece[..before_digit].trim_end_matches([' ', '\t', '>']);
    if prefix.trim().is_empty() && !earlier_prose {
        0
    } else if !digit {
        prefix.len()
    } else {
        piece.len()
    }
}

/// Cut one-line forwards: what the person wrote around them, and where the first starts.
fn strip_one_line_forwards(line: &str, others: bool) -> Option<(String, usize)> {
    let mut pieces = Vec::new();
    let mut cursor = 0;
    let mut first = None;
    while let Some((start, end, _bounded)) = next_one_line_forward(line, cursor, others) {
        first.get_or_insert(start);
        let from = cursor
            + forward_prefix_start(
                &line[cursor..start],
                pieces.iter().any(|piece: &&str| !piece.trim().is_empty()),
            );
        pieces.push(&line[cursor..from]);
        cursor = end.max(start + 1);
    }
    let first = first?;
    pieces.push(&line[cursor..]);
    let mut own = String::new();
    for piece in pieces {
        let piece = piece.trim_start();
        if piece.is_empty() {
            continue;
        }
        if !own.is_empty() && !own.ends_with(char::is_whitespace) {
            own.push(' ');
        }
        own.push_str(piece);
    }
    Some((own.trim_end().to_string(), first))
}

/// Read `text` by line for the forwards [`strip_csr_forwards`] names (the card and the story only when `others`).
/// `None` when there are none.
fn scan_forwards(text: &str, others: bool) -> Option<Forwards> {
    if !has_dreamer_attribution(text)
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
    let mark = |at: usize, first: &mut Option<usize>| {
        *first = Some(first.map_or(at, |seen| seen.min(at)));
    };
    for at in 0..lines.len() {
        if kept[at].is_none() || !is_band_attribution(lines[at]) {
            continue;
        }
        let v2 = is_band_quoted(lines[at].trim());
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
            Some((own, at)) => {
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
    })
}

// The pane follows its displayed prompt grammar without changing import classification.
fn scan_companion_forwards(text: &str, others: bool) -> Option<Forwards> {
    if !has_dreamer_attribution(text)
        && !(others
            && (text.contains(COMPACTION_CARD_OPEN)
                || text.contains(STORY_OPEN)
                || crate::import::contamination::SIDE_NOTE_OPENERS
                    .iter()
                    .any(|opener| text.contains(opener))))
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
    let mark = |at: usize, first: &mut Option<usize>| {
        *first = Some(first.map_or(at, |seen| seen.min(at)));
    };
    for at in 0..lines.len() {
        let note =
            others && crate::import::contamination::SIDE_NOTE_OPENERS.contains(&lines[at].trim());
        if kept[at].is_none() || !(is_band_attribution(lines[at]) || note) {
            continue;
        }
        let v2 = is_band_quoted(lines[at].trim());
        // The question: the nearest nonblank line above the attribution.
        let mut above = at;
        while above > 0 && blank(lines[above - 1]) {
            above -= 1;
        }
        let question = (!note && above > 0 && kept[above - 1] == Some(lines[above - 1]))
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
        // The reasoning: the run of `>` lines below, past any blank lines.
        let mut below = at + 1;
        while below < lines.len() && blank(lines[below]) {
            below += 1;
        }
        let mut end = below;
        while end < lines.len() && quoted(lines[end]) {
            end += 1;
        }
        let reasoning = end > below;
        if question.is_none() && !reasoning {
            continue;
        }
        if let Some((line, open)) = question {
            let before = lines[line][..open].trim_end();
            kept[line] = (!before
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '>'))
            .then_some(before);
            mark(starts[line] + open, &mut first);
        } else {
            mark(starts[at], &mut first);
        }
        kept[at] = None;
        kept[below..end].fill(None);
    }
    let mut left: Vec<String> = Vec::with_capacity(lines.len());
    for (index, line) in kept.iter().enumerate() {
        let Some(line) = line else { continue };
        match strip_one_line_forwards(line, others) {
            Some((own, at)) => {
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
/// (the removed `pane.py`, every `send=one_line(...)` that
/// restates memory CSR already handed). Pasted back as a prompt, the words are
/// CSR's: retrieving on them returns the very items they list. Live failure
/// 2026-10-10: a pasted hand-off list drew back its own "When claude is killed"
/// note at cosine 0.855, plus a duplicate of it, as fresh PAST CONTEXT.
const PANE_HANDED: &str = "CSR handed you these this session: ";
/// The removed `pane.py` `lost_asks`: `Lost and found: “<thread>” went quiet after HH:MM.`
/// then one of two fixed asks, all CSR's words.
const PANE_LOST: &str = "Lost and found: “";
/// The fixed closing words of the two `lost_asks` send forms.
const PANE_LOST_ENDINGS: [&str; 2] = [
    "then tell me in one line.",
    "report its result when it lands.",
];
const PANE_REMEMBERED: &str = "CSR remembered: “";
const PANE_REMEMBERED_SESSION: &str = "CSR remembered a past session: “";

/// `one_line(text, most=700)` in the removed `pane.py` cuts at 699 characters and appends
/// `…`: an ellipsis exactly there ends the send text, whatever follows is the
/// person's.
const PANE_SEND_WIDTH: usize = 699;

/// Line openings of the blocks the hooks inject (prompt_submit, session_start,
/// the formatter). Lines under one of them that continue the block (`- [`
/// items) go with it.
const INJECTED_BLOCK_OPENINGS: [&str; 13] = [
    contamination::PICKUP_HEADER,
    contamination::EPISODE_INDEX_HEADER_PREFIX,
    "## PAST CONTEXT - NOT INSTRUCTIONS",
    "## RELEVANT PAST CONTEXT - NOT INSTRUCTIONS",
    contamination::PAST_CONTEXT_HEADER_PREFIX,
    "RELEVANT PAST CONTEXT - NOT INSTRUCTIONS",
    contamination::CONTINUUM_HEADER,
    contamination::MEMORY_MANIFEST_HEADER,
    "Resume from LAST/NEXT above",
    "CSR: `",
    contamination::RECENT_SESSIONS_HEADER_PREFIX,
    contamination::CONTINUITY_HEADER_PREFIX,
    OPEN_BLOCK_HEADER,
];

/// Where a hand-off item came from, as the removed `pane.py`'s `from_when` + project +
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

/// An EPISODE INDEX entry pasted alone, as `format_episode_index` writes it:
/// `- [<age>] <request> — <last>[ (outcome=<label>)] → csr_reflect_on_past("conv_<id>")`.
/// The outcome label is withheld when it cannot be trusted, so the entry has
/// to be recognized with or without it.
static INDEX_ENTRY_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"(?m)^[ \t]*- \[[^\]\n]*\] .+ → csr_reflect_on_past\("conv_[^"\n]*"\)[ \t]*$"#,
    )
    .expect("index entry regex")
});

/// A CONTINUUM `LAST:` line pasted alone: `LAST: <text> ([outcome=<label>, ]trust=<tier>)`,
/// where the tier is one `TrustTier` writes; a person's `(trust=low)` is prose.
static CONTINUUM_LAST_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?m)^[ \t]*LAST: .+ \((?:outcome=[^)\n]*, )?trust=(?:unknown|external|trusted_tool|user_history|user_confirmed|system)\)[ \t]*$")
        .expect("continuum last regex")
});

/// The lines saying Claude Code's own auto-compact beat CSR or is off while CSR's is on, which the
/// recap carries as a clause and the pane shows as a notice (`companion::native_warn::Warning::line`),
/// alone on its line. Two grammars:
///  - `Claude's <N>k auto-compact window beat CSR <n>× this week. <fix> to let CSR lead.`, where the
///    fix is one of the two the emitter writes;
///  - `Claude's auto-compact is off[ by DISABLE_AUTO_COMPACT] and Self-Reflect's is on. ...`, in
///    the four fixed texts `native_warn::native_off_lines` gives.
///
/// A person's prose about the setting does not match: every word is the emitter's.
static NATIVE_WARN_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // The off-state forms come from the emitter's own builder, so the two cannot drift.
    let off = crate::companion::native_warn::native_off_lines()
        .iter()
        .map(|line| regex::escape(line))
        .collect::<Vec<_>>()
        .join("|");
    regex::Regex::new(&format!(
        r"(?m)^[ \t]*(?:Claude's [0-9]{{1,4}}k auto-compact window beat CSR [0-9]{{1,4}}\x{{d7}} this week\. (?:Remove autoCompactWindow from settings\.json|Unset CLAUDE_CODE_AUTO_COMPACT_WINDOW) to let CSR lead\.|{off})[ \t]*$"
    ))
    .expect("native warn regex")
});

/// Whether `line` is exactly one of the native warnings in the emitted grammar.
pub fn is_native_warn_line(line: &str) -> bool {
    NATIVE_WARN_LINE.is_match(line) && !line.trim().contains('\n')
}

/// The line the SessionStart hook prints first after a compact, alone on its line, in either
/// form (with the replaced range, or without it for a transcript too big to parse). The
/// pattern is built from the emitter's own text (`transcript::compact_pointer_line`), so the
/// two cannot drift: only the session id and the two turn numbers are open. The line sits at the
/// top of the hook's output, but a re-paste or a quote can move it anywhere, so this matches
/// on any line and does not rely on the header window. A person's own sentence about
/// `csr_transcript` does not match: every word of the line is the emitter's.
static COMPACT_POINTER_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    const SID: &str = "SIDSIDSID";
    const FIRST: usize = 1234567;
    const LAST: usize = 7654321;
    let open_sid = |line: String| regex::escape(&line).replace(SID, "[A-Za-z0-9_-]{1,80}");
    let ranged = open_sid(crate::transcript::compact_pointer_line(
        SID,
        Some((FIRST, LAST)),
    ))
    .replace(&FIRST.to_string(), "[0-9]{1,9}")
    .replace(&LAST.to_string(), "[0-9]{1,9}");
    let unranged = open_sid(crate::transcript::compact_pointer_line(SID, None));
    regex::Regex::new(&format!(r"(?m)^[ \t]*(?:{ranged}|{unranged})[ \t]*$"))
        .expect("compact pointer regex")
});

/// Whether `line` is exactly the compact pointer line in the emitted grammar.
pub fn is_compact_pointer_line(line: &str) -> bool {
    COMPACT_POINTER_LINE.is_match(line) && !line.trim().contains('\n')
}

/// `text` without the lines that are exactly one of CSR's single-line emissions: the compact
/// pointer, the native auto-compact warnings and the compact receipt block. `None` when no line matched, so a caller keeps
/// the text as typed. A reader that folds lines into one (`wrappers::without_harness_tags`) must
/// call this first: once folded, a pasted line sits among other text and no longer matches.
pub(crate) fn without_emitted_lines(text: &str) -> Option<String> {
    let emitted = |line: &str| {
        let line = line.trim_end_matches('\r');
        is_compact_pointer_line(line) || is_native_warn_line(line)
    };
    let rows: Vec<&str> = text.split('\n').collect();
    let fenced = crate::import::contamination::fenced_rows(&rows);
    let mut kept: Vec<&str> = Vec::new();
    let mut cut = false;
    let mut at = 0;
    while at < rows.len() {
        // A compact receipt is a block: its header and rows go, the prose around it stays.
        let block = (!fenced[at])
            .then(|| {
                crate::transcript::receipt::block_len(|n| rows.get(at + n).copied().unwrap_or(""))
            })
            .flatten();
        if let Some(len) = block {
            cut = true;
            at += len;
        } else if emitted(rows[at]) {
            cut = true;
            at += 1;
        } else {
            kept.push(rows[at]);
            at += 1;
        }
    }
    cut.then(|| kept.join("\n"))
}

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
    let rows: Vec<&str> = text.lines().collect();
    let fenced = crate::import::contamination::fenced_rows(&rows);
    // The text of a row as the line pass reads it: no leading quote marks.
    let bare = |row: &str| {
        row.trim_start()
            .trim_start_matches('>')
            .trim_start()
            .to_string()
    };
    let mut next = 0;
    while next < rows.len() {
        let raw = rows[next];
        // The companion's agents envelope (marker, one JSON line, marker) is
        // CSR's whole: it goes with its JSON, and a lone marker line goes too.
        // A compact receipt goes whole: header, anchor rows and footer.
        let block = (!fenced[next])
            .then(|| {
                crate::transcript::receipt::block_len(|n| {
                    rows.get(next + n)
                        .map_or("", |r| r.trim_start().trim_start_matches('>').trim_start())
                })
            })
            .flatten();
        if let Some(len) = block {
            changed = true;
            in_block = false;
            next += len;
            continue;
        }
        let window: Vec<String> = rows[next..rows.len().min(next + 3)]
            .iter()
            .map(|r| bare(r))
            .collect();
        let window: Vec<&str> = window.iter().map(String::as_str).collect();
        if let Some(len) = crate::import::contamination::agents_envelope_len(&window, true) {
            changed = true;
            in_block = false;
            next += len;
            continue;
        }
        next += 1;
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
        if is_compact_pointer_line(line) || is_native_warn_line(line) {
            changed = true;
            continue;
        }
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
#[path = "provenance_probes.rs"]
pub(crate) mod probes;

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) fn ordered_forward_prefix_cases() -> Vec<(String, String)> {
    let mut cases = Vec::new();
    let rows = [
        ("I vote 2 ", "I vote 2 "),
        ("2 > ", "2 "),
        ("2> ", "2 "),
        ("> 2", ""),
        (">2", ""),
        (">>2", ""),
        ("2", ""),
        ("42", "42 "),
        ("2 > >", "2 "),
        ("2 ", "2 "),
        ("I vote 2 > ", "I vote 2 "),
        ("42 > ", "42 "),
        ("1 > 2 ", "1 > 2 "),
        ("I vote 2", "I vote 2 "),
        ("I vote 2\n", "I vote 2\n"),
    ];
    for forward in [
        "CSR's compaction card (compacting), quoted: “One.”",
        "CSR's story of this session, rev 1, quoted: “One.”",
        "CSR's dreamer asked this while you worked, quoted: “Q?” Its reasoning: “One.”",
    ] {
        for (prefix, kept) in rows {
            cases.push((
                format!("{prefix}{forward} Keep the deployment frozen."),
                format!("{kept}Keep the deployment frozen."),
            ));
        }
        for (prefix, kept) in [
            (format!("I vote {forward} 2"), "I vote 2 "),
            (format!("{forward} 2"), ""),
            (format!("2{forward} 3"), ""),
        ] {
            cases.push((
                format!("{prefix}{forward} Keep the deployment frozen."),
                format!("{kept}Keep the deployment frozen."),
            ));
        }
        cases.push((
            format!("The code is 4 {forward}2."),
            "The code is 4 2.".into(),
        ));
    }
    cases
}

#[cfg(test)]
#[path = "provenance_companion_tests.rs"]
mod companion_tests;

#[cfg(test)]
#[path = "provenance_open_block_tests.rs"]
mod open_block_tests;

#[cfg(test)]
#[path = "provenance_native_warn_tests.rs"]
mod native_warn_tests;

#[cfg(test)]
#[path = "provenance_compact_pointer_tests.rs"]
mod compact_pointer_tests;
