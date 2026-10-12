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
/// - the csr-companion dreamer's "ask Claude" prompt (plugins/csr-companion/hooks/dreamer.ts
///   `askClaude`): sent as a user turn, but the dreamer's words, not the person's
const EMISSION_HEADERS: [&str; 15] = [
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
    "CSR's dreamer asked this while you worked",
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
/// header in the leading window, or ≥2 distinct emission field tokens anywhere.
pub fn is_csr_emission(text: &str) -> bool {
    // Machine-owned sentinels, scanned across the FULL text before any
    // window-limited or grammar-based heuristic below runs.
    if contains_machine_sentinel(text) {
        return true;
    }
    if is_dreamer_prompt(text) {
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

/// True when the WHOLE of `text` is the companion dreamer's "ask Claude"
/// message, in the exact grammar `askClaude` (plugins/csr-companion/hooks/
/// dreamer.ts) writes: an optional one-line question, a blank line, a one-line
/// attribution sentence, a blank line, then the dreamer's reasoning as a
/// blockquote. Every word of it is the dreamer's, not the person's, so a caller
/// must drop the message as a whole: paragraph scrubbing alone would remove
/// only the attribution and keep the question and the reasoning as the
/// person's own words. Prose that merely mentions the marker does not match.
pub fn is_dreamer_prompt(text: &str) -> bool {
    let normalized = text.replace("\r\n", "\n");
    let paragraphs: Vec<&str> = normalized.trim().split("\n\n").collect();
    let Some(at) = paragraphs
        .iter()
        .position(|p| p.trim_start().starts_with(DREAMER_ATTRIBUTION))
    else {
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
const PANE_STORY: &str = "CSR's story of this session, rev ";
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
    let (offset, c) = line[start..].char_indices().nth(PANE_SEND_WIDTH)?;
    (c == '…').then(|| start + offset + c.len_utf8())
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
        // A story is free prose with no grammar of its own: it runs to the
        // end of its line (one_line collapsed it to one).
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
    for header in [
        PANE_HANDED,
        PANE_STORY,
        PANE_LOST,
        PANE_REMEMBERED_SESSION,
        PANE_REMEMBERED,
    ] {
        while let Some(start) = out.find(header) {
            let end = pane_send_end(&out, start, header);
            out.replace_range(start..end, " ");
        }
    }
    if let Some(start) = out.find(DREAMER_ATTRIBUTION) {
        out.truncate(start);
    }
    out
}

/// The person's own words in a prompt that may carry CSR's output back to it:
/// pasted pane send texts (hand-off lists, the session story, a remembered
/// note), recap lines, injected PICKUP / EPISODE INDEX / PAST CONTEXT blocks,
/// the dreamer's message. Each grammar is the emitter's own, so prose that only
/// mentions CSR is left alone. Retrieval must run on what this returns: run on
/// the paste, it finds the items the paste lists.
pub fn strip_csr_emissions(text: &str) -> String {
    if is_dreamer_prompt(text) {
        return String::new();
    }
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
        if line == "<system-reminder>" || line == "</system-reminder>" {
            changed = true;
            continue;
        }
        let line = line.strip_prefix("<system-reminder>").unwrap_or(line);
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
    let prose = strip_quoted(&unplumbed);
    let cleaned = prose.trim();
    if cleaned.is_empty() || is_csr_emission(cleaned) {
        return None;
    }
    Some(cleaned.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
