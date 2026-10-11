//! Specificity of a stored memory's excerpt: how many words it carries that
//! are not the frame its writer wraps every row in.
//!
//! The derived writers (session stories, v3 extracts, heuristic narratives,
//! context caches, convention facts) put the same frame around every row:
//! "Session in <project> project using <tools> (N (M user) messages)",
//! "[Heuristic] Project: … Messages: … Tools: …", "## User Request … ---
//! Signature: {…}", "## Validation [Msg 0] Tests: Passed". A row whose
//! rendered excerpt is all frame tells Claude nothing about the work, yet it
//! still scores like anything else on cosine and was handed over as PAST
//! CONTEXT (live 2026-10-10: two "Session in claude-self-reflect project using
//! Bash, Glob, Read, mcp__…" rows in one injection).
//!
//! The frame is measured, not listed: a term is frame when it occurs in at
//! least [`COMMON_TERM_SHARE`] of the excerpts the hook can render, counted
//! over the person's own reflections table ([`CommonTerms::measure`]). Two
//! structural rules cover what document frequency cannot see: identifier
//! tokens (`mcp__…`, `snake_case`, `CamelCase` tool names) and comma runs of
//! single names (tool and file lists) are names, not words. Everything after a
//! `Signature:` trailer is writer metadata.
//!
//! Measured 2026-10-10 with this code over 10,743 renderable excerpts
//! (scratch clone of the live DB, 117 ms): the count of informative words is
//! bimodal, 6,890 rows at 0 and a trough at 1–3 (210 / 138 / 178 rows) before
//! the content rows begin (228 at 4, rising). [`MIN_INFORMATIVE_WORDS`] sits
//! in that trough. No explicit `store_reflection` memory (54 rows) falls
//! below it.

use std::collections::{HashMap, HashSet};

/// A term occurring in at least this share of renderable excerpts is frame.
pub const COMMON_TERM_SHARE: f64 = 0.005;

/// Floor on the document count that makes a term frame. On a small corpus a
/// share would make every word "common" (1 document of 200 is 0.5%).
pub const MIN_COMMON_DOCS: usize = 20;

/// Below this many excerpts there is nothing to measure frame against; only
/// the structural rules apply.
pub const MIN_MEASURED_EXCERPTS: usize = 200;

/// Fewer informative words than this and the excerpt is frame only.
pub const MIN_INFORMATIVE_WORDS: usize = 3;

/// Terms measured as frame in one store.
#[derive(Debug, Default, Clone)]
pub struct CommonTerms {
    terms: HashSet<String>,
    /// Excerpts the measurement ran over (0 = not measured).
    pub excerpts: usize,
}

impl CommonTerms {
    /// Measure frame terms over `excerpts`: every term found in at least
    /// `max(COMMON_TERM_SHARE × n, MIN_COMMON_DOCS)` of them.
    pub fn measure<'a>(excerpts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut df: HashMap<String, usize> = HashMap::new();
        let mut n = 0usize;
        for excerpt in excerpts {
            n += 1;
            for term in words(excerpt) {
                *df.entry(term).or_default() += 1;
            }
        }
        if n < MIN_MEASURED_EXCERPTS {
            return Self {
                terms: HashSet::new(),
                excerpts: n,
            };
        }
        let floor = ((n as f64 * COMMON_TERM_SHARE).ceil() as usize).max(MIN_COMMON_DOCS);
        Self {
            terms: df
                .into_iter()
                .filter(|(_, docs)| *docs >= floor)
                .map(|(term, _)| term)
                .collect(),
            excerpts: n,
        }
    }

    /// A measurement read back from where it was shared.
    pub fn from_measurement(terms: Vec<String>, excerpts: usize) -> Self {
        Self {
            terms: terms.into_iter().collect(),
            excerpts,
        }
    }

    /// The frame terms, in no particular order.
    pub fn terms(&self) -> impl Iterator<Item = &str> {
        self.terms.iter().map(String::as_str)
    }

    #[cfg(test)]
    pub fn from_terms(terms: &[&str]) -> Self {
        Self {
            terms: terms.iter().map(|t| t.to_string()).collect(),
            excerpts: MIN_MEASURED_EXCERPTS,
        }
    }

    pub fn contains(&self, term: &str) -> bool {
        self.terms.contains(term)
    }
}

/// An identifier, not a word: `mcp__x__y`, `snake_case`, `CamelCase`.
fn is_identifier(token: &str) -> bool {
    token.contains('_')
        || token
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
}

/// Tokens that sit in a run of three or more comma-separated single names
/// (a tool list, a file list). The segment before the run contributes its last
/// token ("using Bash"), the one after its first ("Edit (337 …").
fn listed_names(text: &str) -> HashSet<&str> {
    let segs: Vec<&str> = text.split(',').collect();
    let single: Vec<bool> = segs
        .iter()
        .map(|s| s.split_whitespace().count() == 1)
        .collect();
    let mut names = HashSet::new();
    let mut i = 0;
    while i < segs.len() {
        if !single[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < segs.len() && single[j] {
            j += 1;
        }
        let before = (i > 0)
            .then(|| segs[i - 1].split_whitespace().last())
            .flatten();
        let after = segs.get(j).and_then(|s| s.split_whitespace().next());
        if (j - i) + usize::from(before.is_some()) + usize::from(after.is_some()) >= 3 {
            names.extend(segs[i..j].iter().map(|s| s.trim()));
            names.extend(before);
            names.extend(after);
        }
        i = j;
    }
    names
}

/// Lowercased words (3+ letters) of an excerpt, without identifiers, listed
/// names, or anything after a `Signature:` trailer. URLs count as one word.
pub fn words(excerpt: &str) -> HashSet<String> {
    let body = excerpt
        .find("Signature:")
        .map_or(excerpt, |at| &excerpt[..at]);
    let names = listed_names(body);
    let mut out = HashSet::new();
    for token in body.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '`' | '*' | '|' | '<' | '>'
            )
    }) {
        if token.is_empty() || names.contains(token) {
            continue;
        }
        if token.starts_with("http://") || token.starts_with("https://") {
            out.insert(token.to_ascii_lowercase());
            continue;
        }
        if is_identifier(token) {
            continue;
        }
        for part in token.split(|c: char| !c.is_alphabetic()) {
            if part.chars().count() >= 3 {
                out.insert(part.to_lowercase());
            }
        }
    }
    out
}

/// Words of `excerpt` that are not frame in `common`.
pub fn informative_words(excerpt: &str, common: &CommonTerms) -> usize {
    words(excerpt)
        .into_iter()
        .filter(|w| !common.contains(w))
        .count()
}

/// The excerpt is its writer's frame and nothing else.
pub fn is_generic(excerpt: &str, common: &CommonTerms) -> bool {
    informative_words(excerpt, common) < MIN_INFORMATIVE_WORDS
}

/// Openings of the parts a v3 session story (`extraction::story`) appends
/// after the request it restates: what was modified, the outcome, open issues.
const STORY_FINDING_OPENINGS: [&str; 7] = [
    "Modified ",
    "LANGUAGES:",
    "Completed successfully",
    "Partially completed",
    "Failed — may need retry",
    "Resolved errors during session",
    "Unresolved:",
];

/// The part of a derived row's excerpt that says what happened rather than
/// what was asked. A v3 extract loses its `## User Request` section, a v3
/// session story (`story_` id) the request it opens with, and every row its
/// `Signature:` trailer. Other derived rows (narrated stories, facts) are all
/// account and are kept whole.
pub fn finding_text(id: &str, excerpt: &str) -> String {
    let body = excerpt
        .find("Signature:")
        .map_or(excerpt, |at| &excerpt[..at]);
    if let Some(start) = body.find("## User Request") {
        let after = &body[start + "## User Request".len()..];
        let end = after.find("\n## ").unwrap_or(after.len());
        return format!("{}{}", &body[..start], &after[end..]);
    }
    if id.starts_with("story_") {
        return STORY_FINDING_OPENINGS
            .iter()
            .filter_map(|opening| body.find(opening))
            .min()
            .map_or_else(String::new, |at| body[at..].to_string());
    }
    body.to_string()
}

/// The row records an outcome, a lesson or a state reached, not only the ask
/// that opened its session.
pub fn states_a_finding(id: &str, excerpt: &str, common: &CommonTerms) -> bool {
    informative_words(&finding_text(id, excerpt), common) >= MIN_INFORMATIVE_WORDS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame terms as measured on the live corpus 2026-10-10 (a subset: the
    /// ones these fixtures contain).
    fn measured() -> CommonTerms {
        CommonTerms::from_terms(&[
            "session",
            "project",
            "using",
            "messages",
            "user",
            "with",
            "error",
            "investigation",
            "heuristic",
            "had",
            "errors",
            "yes",
            "files",
            "tools",
            "request",
            "the",
            "claude",
            "self",
            "reflect",
            "anukriti",
            "campaigns",
            "partially",
            "completed",
            "unresolved",
            "validation",
            "msg",
            "tests",
            "passed",
            "convention",
            "pattern",
            "new",
            "file",
            "creation",
            "what",
            "last",
            "our",
            "how",
            "this",
            "and",
            "for",
        ])
    }

    #[test]
    fn writer_frames_from_the_live_injection_are_generic() {
        let common = measured();
        // Handed as PAST CONTEXT on 2026-10-10.
        for frame in [
            "Session in claude-self-reflect project using Bash, Glob, Read, mcp__claude-self-reflect__csr_reflect_on_past, mcp__claude-self-reflect__csr_quick_check, mcp__claude-self-reflect__get_recent_work, mcp__claude-self-reflect__csr_search_by_file, mcp__claude-self-reflect__csr_search_by_concept,...",
            "Session in claude-self-reflect project using Read, Glob, TodoWrite, Edit, AskUserQuestion, Write, Bash, Grep, mcp__claude-self-reflect__csr_reflect_on_past, Skill, mcp__claude-self-reflect__get_embedding_mode, mcp__claude-self-reflect__csr_quick_check, mcp__claude-self-reflect__get_recent_work,...",
            "[Heuristic] Project: anukriti\nMessages: 421 (0 user)\nTools: Task, TaskCreate, TaskUpdate, Read, Bash, Write, Glob, Edit, TaskStop, TaskList, Grep\nFiles: theme.ts, index.ts, styles.ts, package.json, babel.config.js\nHad errors: yes",
            "[Heuristic] Project: Anukriti-Campaigns\nMessages: 2 (2 user)",
            "Session in Anukriti-Campaigns project (3 (1 user) messages).",
            "Partially completed",
            "## Validation\n[Msg 0] Tests: Passed\n[Msg 2] Tests: Passed",
            "[convention] Pattern: New file creation",
            "## User Request\nhow is our last OTA doing\n\n---\nSignature: {\"completion_status\":\"partial\",\"frameworks\":[\"rust\",\"python\"]}",
        ] {
            assert!(is_generic(frame, &common), "{frame}");
        }
    }

    #[test]
    fn rows_that_say_something_pass() {
        let common = measured();
        for content in [
            "When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it - what enrichment is needed to catch \"",
            "Implement the following plan:\n\n# Plan: Replace file-based persistent context with compaction-aware re-injection",
            "```markdown\n## Search Summary\nImplemented SessionStart progressive disclosure pattern for CSR engine with claude-mem learnings integration",
            "RADIO PUSH 08-29 — near-complete, verse_maps DONE. Prod state (all LIVE): - 2 tracks live",
            "compare https://github.com/example/TradingAgents with our agent router",
        ] {
            assert!(!is_generic(content, &common), "{content}");
        }
    }

    #[test]
    fn frame_is_measured_from_the_corpus_not_listed() {
        // 300 rows sharing one frame, each with its own subject word.
        let subject = |i: usize| -> String {
            (0..3)
                .map(|k| char::from(b'a' + ((i / 26usize.pow(k)) % 26) as u8))
                .collect::<String>()
                + "zz"
        };
        let rows: Vec<String> = (0..300)
            .map(|i| {
                format!(
                    "Session in demo project using Bash, Read, Edit about {}",
                    subject(i)
                )
            })
            .collect();
        let common = CommonTerms::measure(rows.iter().map(String::as_str));
        assert!(common.contains("session") && common.contains("project"));
        assert!(
            !common.contains(&subject(7)),
            "a word unique to one row is not frame"
        );
        assert!(is_generic(
            "Session in demo project using Bash, Read, Edit",
            &common
        ));
        // Too few rows to measure: only the structural rules apply.
        let small = CommonTerms::measure(rows.iter().take(10).map(String::as_str));
        assert_eq!(small.excerpts, 10);
        assert!(!small.contains("session"));
    }

    #[test]
    fn identifiers_and_name_lists_are_not_words() {
        assert!(
            words("mcp__claude-self-reflect__csr_quick_check TaskCreate AskUserQuestion")
                .is_empty()
        );
        let w = words("touched prompt_submit.rs, intent.rs, provenance.rs and fixed the gate");
        assert!(w.contains("touched") && w.contains("fixed") && w.contains("gate"));
        assert!(!w.contains("intent") && !w.contains("provenance"));
    }

    #[test]
    fn a_finding_is_what_happened_not_what_was_asked() {
        let common = measured();
        // Blind-labelled relevant, dropped by the 21-day bar (2026-10-11 replay).
        let ota_read = "## User Request\nwhat is the latst on last nights OTA, adoption performance etc/\n\n## Active Issues\n\"=== 1.6.0 binary_status: testflight-only dau: 4\nlast_ota:\n  channel: anonymous-browsing\n  group_\n\n---\nSignature: {\"completion_status\":\"failed\"}";
        assert!(states_a_finding("extracted_v3_x", ota_read, &common));
        let story = "what is the latst on last nights OTA, adoption performance etc/. Failed — may need retry. Resolved errors during session. Unresolved: \"=== 1.6.0 binary_status: testflight-only dau: 4 last_ota: channel: anonymous-browsing --- Signature: {}";
        assert!(states_a_finding("story_x", story, &common));
        let lesson = "Saadhana promo video choppy-slideshow bug traced to audio track: argala stotram was slow, flat, missing surprise arc. Rewrote Suno prompt from sandhya-architecture to neo-soul-surprise";
        assert!(states_a_finding(
            "00000000-0000-4000-8000-0000000000ad",
            lesson,
            &common
        ));
        // Blind-labelled irrelevant request echoes: the ask and nothing else.
        for stub in [
            "## User Request\nreview 3f56dc2b - the prev session.. planning for an OTA, it went in multiple rounds provide a checllist TLDR of what the next moves are for today\n\n---\nSignature: {\"completion_status\":\"success\"}",
            "## User Request\nDreaming, we researched it chatted around it never thought about it v10 of CSR brings dreaming - is this the kilelr feature?\n\n## Active Issues\n\n\n---\nSignature: {}",
            "one aspect of dreams and why it ailed (check csr for recent sessions and discussions ) - gather info before getting biased. Partially completed. Unresolved: --- Signature: {}",
        ] {
            assert!(!states_a_finding(if stub.starts_with("##") { "extracted_v3_x" } else { "story_x" }, stub, &common), "{stub}");
        }
    }
}
