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
pub const AGENT_PROMPT_SIGNATURES: [&str; 3] = [
    "You are CSR Episode Analyst",
    "You are summarizing a coding session",
    "# Ratification Dialog-Act Extraction",
];

/// Block headers CSR emits, one per formatter:
/// - `format_tier0_block` (session_start.rs)
/// - briefing prompt/output (session_briefing.rs)
/// - the /memory-feedback probe report template
/// - SessionStart continuity blocks (session_start.rs)
/// - UserPromptSubmit context blocks (injection/formatter.rs)
/// - agent prompts (AGENT_PROMPT_SIGNATURES, included via EMISSION_HEADERS)
const EMISSION_HEADERS: [&str; 14] = [
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
    // Machine-owned sentinel, scanned across the FULL text before any
    // window-limited or grammar-based heuristic below runs.
    if contains_recap_sentinel(text) {
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

/// Full provenance pipeline for episode/story extraction candidates:
/// strip plumbing → strip mentions → reject CSR emissions → reject empties.
/// Returns the cleaned text safe to carry forward as session content.
pub fn extractable(text: &str) -> Option<String> {
    let unplumbed = strip_plumbing(text);
    // Sentinel check runs before strip_quoted: a blockquote-wrapped recap
    // ("> recap [...]: ... [[CSR:RECAP]]") would otherwise have every
    // quoted line erased before the sentinel is ever seen, letting an
    // unquoted preamble alongside it survive as if it were genuine content.
    if is_csr_emission(&unplumbed) {
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
}
