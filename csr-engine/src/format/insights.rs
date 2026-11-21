//! Bounded aggregates for `csr_search_insights`: the top concepts, files,
//! weeks and conversations of the matched chunks, each with a count. At most
//! [`TOP`] of each, so the section stays small however many chunks match.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use super::{parse_timestamp, truncate_chars, xml_escape, EnrichedResult};

/// Entries kept per aggregate.
const TOP: usize = 5;
/// Longest term or path shown.
const SHOWN_CHARS: usize = 80;

static FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9_./-]*[A-Za-z0-9_-]\.(?:rs|py|ts|tsx|js|jsx|json|toml|md|sh|yml|yaml|sql|go|java|swift|css|html)\b")
        .expect("file pattern")
});

static TERM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z][a-z0-9_]{3,23}").expect("term pattern"));

/// The marker the scrubber leaves in place of a credential. It is not a theme.
static REDACTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[secret redacted: [a-z_]+\]").expect("marker pattern"));

/// Words too common to be a concept.
#[rustfmt::skip]
const STOP: &[&str] = &[
    "about", "after", "also", "because", "been", "before", "being", "could", "does",
    "done", "each", "from", "have", "here", "into", "just", "like", "make", "more", "most",
    "need", "only", "other", "over", "same", "should", "some", "such", "than", "that", "their",
    "them", "then", "there", "these", "they", "this", "those", "through", "using", "very",
    "want", "were", "what", "when", "where", "which", "while", "will", "with", "would", "your",
    "keyword", "true", "false", "none", "null", "user", "assistant", "tool", "result",
];

/// The top entries of `counts`: highest count first, ties by name.
fn top(counts: BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(TOP);
    rows
}

fn section(out: &mut String, name: &str, item: &str, rows: &[(String, usize)]) {
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("  <{name}>\n"));
    for (label, count) in rows {
        out.push_str(&format!(
            "    <{item} count=\"{count}\">{}</{item}>\n",
            xml_escape(truncate_chars(label, SHOWN_CHARS))
        ));
    }
    out.push_str(&format!("  </{name}>\n"));
}

/// The aggregate sections for `results`, ready to sit inside `<search_summary>`.
pub(super) fn aggregates(results: &[EnrichedResult]) -> String {
    let mut concepts: BTreeMap<String, usize> = BTreeMap::new();
    let mut files: BTreeMap<String, usize> = BTreeMap::new();
    let mut weeks: BTreeMap<String, usize> = BTreeMap::new();
    let mut conversations: BTreeMap<String, usize> = BTreeMap::new();
    for result in results {
        let raw = result
            .chunk
            .content
            .strip_prefix("[keyword] ")
            .unwrap_or(&result.chunk.content);
        // Terms and paths are read from the chunk, so scrub it first: a chunk
        // stored before a pattern existed, or a `NAME=value` in the middle of a
        // line that import's line-anchored pass leaves, still holds the value.
        // Once its `NAME=` context is gone, the final redaction of the reply
        // cannot tell a bare value from a word, and a value repeated across
        // chunks would be listed as a concept.
        let scrubbed = crate::redact::scrub_inline(raw);
        let cleaned = REDACTED.replace_all(&scrubbed, " ");
        let text: &str = &cleaned;
        let lower = text.to_lowercase();
        let terms: BTreeSet<&str> = TERM
            .find_iter(&lower)
            .map(|m| m.as_str())
            .filter(|term| !STOP.contains(term))
            .collect();
        for term in terms {
            *concepts.entry(term.to_string()).or_default() += 1;
        }
        let paths: BTreeSet<&str> = FILE.find_iter(text).map(|m| m.as_str()).collect();
        for path in paths {
            *files.entry(path.to_string()).or_default() += 1;
        }
        if let Some(when) = parse_timestamp(&result.chunk.timestamp) {
            *weeks.entry(when.format("%G-W%V").to_string()).or_default() += 1;
        }
        *conversations
            .entry(result.chunk.conversation_id.clone())
            .or_default() += 1;
    }
    // A term seen in one chunk only is not a theme of the matches.
    concepts.retain(|_, count| *count >= 2);
    let mut out = String::new();
    section(&mut out, "top_concepts", "concept", &top(concepts));
    section(&mut out, "top_files", "file", &top(files));
    section(&mut out, "top_weeks", "week", &top(weeks));
    if !conversations.is_empty() {
        out.push_str("  <top_conversations>\n");
        for (cid, count) in top(conversations) {
            out.push_str(&format!(
                "    <conversation matches=\"{count}\"><cid>{}</cid></conversation>\n",
                xml_escape(truncate_chars(&cid, SHOWN_CHARS))
            ));
        }
        out.push_str("  </top_conversations>\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ConversationChunk;

    fn row(id: &str, conversation: &str, timestamp: &str, content: &str) -> EnrichedResult {
        EnrichedResult {
            score: 0.5,
            chunk: ConversationChunk {
                id: id.into(),
                conversation_id: conversation.into(),
                project_name: "p".into(),
                timestamp: timestamp.into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            },
            resolution: None,
            validity_demoted: false,
            trust: crate::provenance::TrustTier::Unknown,
        }
    }

    #[test]
    fn aggregates_count_concepts_files_weeks_and_conversations() {
        let results = vec![
            row(
                "1",
                "conv-a",
                "2026-11-03T10:00:00Z",
                "rerank scoring in src/search/rerank.rs",
            ),
            row(
                "2",
                "conv-a",
                "2026-11-04T10:00:00Z",
                "rerank pool edits to src/search/rerank.rs",
            ),
            row(
                "3",
                "conv-b",
                "2026-11-10T10:00:00Z",
                "timeline grouping in src/temporal/mod.rs",
            ),
        ];
        let out = aggregates(&results);
        assert!(
            out.contains("<concept count=\"2\">rerank</concept>"),
            "{out}"
        );
        assert!(
            out.contains("<file count=\"2\">src/search/rerank.rs</file>"),
            "{out}"
        );
        assert!(out.contains("<week count=\"2\">2026-W45</week>"), "{out}");
        assert!(out.contains("<week count=\"1\">2026-W46</week>"), "{out}");
        assert!(
            out.contains("<conversation matches=\"2\"><cid>conv-a</cid></conversation>"),
            "{out}"
        );
    }

    #[test]
    fn a_secret_repeated_across_chunks_never_becomes_a_concept() {
        // Mid-line `NAME=value`: import scrubs only at the start of a line, so
        // both chunks can hold the value as stored.
        let results = vec![
            row(
                "1",
                "conv-a",
                "2026-11-03T10:00:00Z",
                "rerank scoring: run it with API_TOKEN=zqvsecretvalue99 set",
            ),
            row(
                "2",
                "conv-b",
                "2026-11-04T10:00:00Z",
                "rerank pool: then API_TOKEN=zqvsecretvalue99 was exported again",
            ),
        ];
        let out = aggregates(&results);
        assert!(!out.contains("zqvsecretvalue99"), "{out}");
        assert!(!out.contains("redacted"), "{out}");
        assert!(
            out.contains("<concept count=\"2\">rerank</concept>"),
            "{out}"
        );
    }

    #[test]
    fn aggregates_stay_bounded() {
        let results: Vec<_> = (0..40)
            .map(|i| {
                row(
                    &format!("{i}"),
                    &format!("conv-{i}"),
                    "2026-11-03T10:00:00Z",
                    &format!("shared term{i} alpha{i} file{i}.rs shared_concept"),
                )
            })
            .collect();
        let out = aggregates(&results);
        assert!(out.matches("<conversation ").count() <= TOP, "{out}");
        assert!(out.matches("<file ").count() <= TOP, "{out}");
        assert!(out.matches("<concept ").count() <= TOP, "{out}");
        assert!(out.len() < 2_000, "{} bytes", out.len());
    }
}
