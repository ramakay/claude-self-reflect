//! Deterministic recap composition from a stored episode and evidence feeds.

use std::sync::LazyLock;

use regex::Regex;

static XML_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]{1,50}>").unwrap());
static MD_HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#{1,4}\s+").unwrap());
/// Bullet markers at the head of a flattened line. Lines are joined with
/// spaces before the recap is composed, so a markdown list would otherwise
/// read as `a: - one - two` in the middle of a sentence.
static MD_BULLET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:[-*+]|\d{1,3}[.)])\s+").unwrap());

/// Maximum open facts accepted by the composer and requested by storage callers.
pub const STILL_OPEN_LIMIT: usize = 3;

/// A resolution associated with the current project.
pub struct SettledFact {
    pub claim: String,
    pub receipt: String,
    pub status: String,
}

/// A previously held line invalidated while the user was away.
pub struct RetiredLine {
    pub label: String,
    pub receipt_oid: String,
    pub date: String,
}

/// Evidence feeds supplied by storage.
///
/// Feed text is trusted, already-curated database content: its semantic content
/// passes through without provenance filtering. Structural whitespace is
/// collapsed so a database value cannot create additional recap lines.
pub struct RecapFeeds {
    pub settled: Vec<SettledFact>,
    pub still_open: Vec<SettledFact>,
    pub retired_while_away: Vec<RetiredLine>,
    pub open_proposals: usize,
}

/// Compose an evidence-backed recap without reading external state.
pub fn compose_recap(
    ep: &crate::hooks::stop::Episode,
    feeds: &RecapFeeds,
    age: &str,
) -> Option<String> {
    const MAX_CHARS: usize = 700;

    use crate::extraction::provenance::extractable;

    let intent = extractable(&ep.request)
        .map(|text| compact_preview(&text, 80))
        .filter(|text| has_substance(text));
    let completed = extractable(&ep.completed)
        .map(|text| conclusion_preview(&text, 120))
        .filter(|text| has_substance(text));
    if intent.is_none() && completed.is_none() {
        return None;
    }

    let mut recap = match (intent.as_deref(), completed.as_deref()) {
        (Some(intent), Some(completed)) => format!(
            "recap [{age}]: {}: {}",
            intent.trim_end().trim_end_matches(TRAILING_JUNK),
            terminate_clause(completed)
        ),
        (Some(intent), None) => format!("recap [{age}]: {}", terminate_clause(intent)),
        (None, Some(completed)) => format!("recap [{age}]: {}", terminate_clause(completed)),
        (None, None) => unreachable!("empty episodes abstain above"),
    };

    let settled_entries: Vec<String> = feeds
        .settled
        .iter()
        .filter(|fact| fact.status == "resolved")
        .filter(|fact| !normalize_feed_text(&fact.receipt).is_empty())
        .take(3)
        .map(format_fact)
        .collect();

    let mut now_parts = Vec::new();
    if let Some(blocker) = ep
        .blockers
        .as_deref()
        .and_then(extractable)
        .map(|text| compact_preview(&text, 120))
        .filter(|text| has_substance(text))
    {
        now_parts.push(blocker);
    }
    now_parts.extend(
        feeds
            .still_open
            .iter()
            .take(STILL_OPEN_LIMIT)
            .filter(|fact| fact.status != "resolved")
            .filter(|fact| !normalize_feed_text(&fact.receipt).is_empty())
            .map(format_fact),
    );
    if feeds.open_proposals > 0 {
        now_parts.push(format!(
            "{} proposals awaiting csr_resolve",
            feeds.open_proposals
        ));
    }
    let open_todos = ep
        .todos
        .iter()
        .filter(|todo| todo.status != "completed")
        .count();
    if open_todos > 0 {
        now_parts.push(format!("{open_todos} todos open"));
    }
    let next = ep.next_steps.as_deref().and_then(next_preview).or_else(|| {
        ep.todos
            .iter()
            .filter(|todo| todo.status != "completed")
            .find_map(|todo| next_preview(&todo.content))
    });

    let retired_entries: Vec<String> = feeds
        .retired_while_away
        .iter()
        .filter(|line| !normalize_feed_text(&line.receipt_oid).is_empty())
        .take(2)
        .map(|line| {
            format!(
                "{} (superseded {}, {})",
                normalize_feed_text(&line.label),
                normalize_feed_text(&line.date),
                normalize_feed_text(&line.receipt_oid)
            )
        })
        .collect();
    let next_line = next.map(|next| format!("Next: {next}."));

    // Reserve one whole entry for every eligible evidence-list clause before
    // using any residual budget for extras. If the minimum set itself cannot
    // fit, drop its largest list clause rather than truncating an entry.
    let mut clauses = [
        ListClause::new("Settled: ", "; ", settled_entries),
        ListClause::new("Now: ", " | ", now_parts),
        ListClause::new("Learnt-then-retired while away: ", "; ", retired_entries),
    ];
    let mut used = recap.chars().count()
        + next_line
            .as_ref()
            .map_or(0, |line| line.chars().count() + 1)
        + clauses
            .iter()
            .map(ListClause::reserved_chars)
            .sum::<usize>();

    while used > MAX_CHARS {
        let Some((index, chars)) = clauses
            .iter()
            .enumerate()
            .filter_map(|(index, clause)| {
                let chars = clause.reserved_chars();
                (chars > 0).then_some((index, chars))
            })
            .max_by_key(|(_, chars)| *chars)
        else {
            break;
        };
        clauses[index].drop_entries();
        used -= chars;
    }

    let mut remaining = MAX_CHARS.saturating_sub(used);
    for clause in &mut clauses {
        clause.add_entries_within(&mut remaining);
    }

    for line in clauses
        .iter()
        .filter_map(ListClause::render)
        .chain(next_line)
    {
        recap.push('\n');
        recap.push_str(&line);
    }

    Some(recap)
}

struct ListClause {
    prefix: &'static str,
    separator: &'static str,
    entries: Vec<String>,
    selected: usize,
}

impl ListClause {
    fn new(prefix: &'static str, separator: &'static str, entries: Vec<String>) -> Self {
        let selected = usize::from(!entries.is_empty());
        Self {
            prefix,
            separator,
            entries,
            selected,
        }
    }

    fn reserved_chars(&self) -> usize {
        self.render().map_or(0, |line| line.chars().count() + 1)
    }

    fn drop_entries(&mut self) {
        self.selected = 0;
    }

    fn add_entries_within(&mut self, remaining: &mut usize) {
        if self.selected == 0 {
            return;
        }
        while let Some(entry) = self.entries.get(self.selected) {
            let needed = self.separator.chars().count() + entry.chars().count();
            if needed > *remaining {
                break;
            }
            *remaining -= needed;
            self.selected += 1;
        }
    }

    fn render(&self) -> Option<String> {
        (self.selected > 0).then(|| {
            format!(
                "{}{}.",
                self.prefix,
                self.entries[..self.selected].join(self.separator)
            )
        })
    }
}

fn format_fact(fact: &SettledFact) -> String {
    let claim = normalize_feed_text(&fact.claim);
    let claim = if claim.is_empty() {
        "evidence".to_string()
    } else {
        claim
    };
    format!("{claim} ({})", normalize_feed_text(&fact.receipt))
}

fn normalize_feed_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_next_prefix(text: &str) -> &str {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();
    for prefix in ["next steps:", "next step:", "next:"] {
        if lower.starts_with(prefix) {
            return trimmed[prefix.len()..].trim_start();
        }
    }
    trimmed
}

fn next_preview(text: &str) -> Option<String> {
    let extracted = crate::extraction::provenance::extractable(text)?;
    let preview = compact_preview(strip_next_prefix(&extracted), 80);
    (!preview.is_empty() && !is_no_evidence_next(&preview)).then_some(preview)
}

/// Evidence requires at least one alphanumeric character: values like "…",
/// "/", or "-*-" are punctuation-only sentinels, not content, and every
/// clause gate must treat them as absent (abstention-first).
fn has_substance(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

fn is_no_evidence_next(text: &str) -> bool {
    if !has_substance(text) {
        return true;
    }
    let normalized = text
        .trim()
        .trim_matches(|character: char| character.is_ascii_punctuation() && character != '/')
        .trim()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "none" | "nothing" | "n/a" | "no next step" | "no next steps"
    )
}

fn compact_preview(content: &str, max_chars: usize) -> String {
    let clean = sanitize_preview(content);
    if clean.chars().count() <= max_chars {
        return clean;
    }
    truncate_at_word_boundary(&clean, max_chars)
}

fn conclusion_preview(content: &str, max_chars: usize) -> String {
    let clean = sanitize_preview(content);
    if clean.chars().count() <= max_chars {
        return clean;
    }

    const ELLIPSIS: &str = " … ";
    let sentences = split_preview_sentences(&clean);
    if sentences.is_empty() {
        return truncate_at_word_boundary(&clean, max_chars);
    }

    let verdict = if let Some((span_start, span_end)) = find_bold_verdict_span(&clean) {
        let (start, end) = sentences
            .iter()
            .copied()
            .find(|&(start, end)| start <= span_start && span_end <= end)
            .unwrap_or((span_start, span_end));
        strip_bold_markers(&clean[start..end])
    } else {
        let (start, end) = sentences[0];
        strip_bold_markers(&clean[start..end])
    };

    let mut result = verdict;
    if result.chars().count() < max_chars * 3 / 5 {
        if let Some(last) = sentences.iter().rev().find_map(|&(start, end)| {
            let sentence = clean[start..end].trim();
            if sentence.is_empty() || is_non_sentence_fragment(sentence) {
                return None;
            }
            let stripped = strip_bold_markers(sentence);
            (stripped != result).then_some(stripped)
        }) {
            result = format!("{result}{ELLIPSIS}{last}");
        }
    }

    if result.chars().count() > max_chars {
        result = truncate_at_word_boundary(&result, max_chars);
    }
    result
}

fn split_preview_sentences(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b'.' | b'!' | b'?') {
            let punctuation = bytes[index];
            let mut end = index + 1;
            while end < bytes.len() && bytes[end] == punctuation {
                end += 1;
            }
            if bytes.get(end) == Some(&b'*') && bytes.get(end + 1) == Some(&b'*') {
                end += 2;
            }
            if end >= bytes.len() || bytes[end] == b' ' {
                let slice = &text[start..end];
                let trim_start = slice.len() - slice.trim_start().len();
                let trim_end = slice.trim_end().len();
                if trim_end > trim_start {
                    sentences.push((start + trim_start, start + trim_end));
                }
                while end < bytes.len() && bytes[end] == b' ' {
                    end += 1;
                }
                start = end;
                index = end;
                continue;
            }
        }
        index += 1;
    }
    if start < text.len() {
        let slice = &text[start..];
        let trim_start = slice.len() - slice.trim_start().len();
        let trim_end = slice.trim_end().len();
        if trim_end > trim_start {
            sentences.push((start + trim_start, start + trim_end));
        }
    }
    sentences
}

fn find_bold_verdict_span(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'*' {
            let inner_start = index + 2;
            let mut close = inner_start;
            while close + 1 < bytes.len() {
                if bytes[close] == b'*' && bytes[close + 1] == b'*' {
                    if text[inner_start..close].trim().chars().count() >= 10 {
                        return Some((index, close + 2));
                    }
                    index = close + 2;
                    break;
                }
                close += 1;
            }
            if close + 1 >= bytes.len() {
                break;
            }
            continue;
        }
        index += 1;
    }
    None
}

fn strip_bold_markers(text: &str) -> String {
    text.replace("**", "")
}

fn is_non_sentence_fragment(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('|') || trimmed.contains("```")
}

fn truncate_at_word_boundary(text: &str, max_chars: usize) -> String {
    const ELLIPSIS: &str = "...";
    let budget = max_chars.saturating_sub(ELLIPSIS.chars().count());
    let byte_limit = text
        .char_indices()
        .nth(budget)
        .map_or(text.len(), |(index, _)| index);
    let window = &text[..byte_limit];
    let cut = window.rfind(char::is_whitespace).unwrap_or(byte_limit);
    // Trailing punctuation before an ellipsis reads as a typo once the
    // clause terminator is appended (`witness ledger,....`), so drop it.
    let kept = window[..cut].trim_end().trim_end_matches(TRAILING_JUNK);
    format!("{kept}{ELLIPSIS}")
}

/// Punctuation that must not survive immediately before an appended
/// ellipsis or clause terminator.
const TRAILING_JUNK: [char; 7] = [',', ';', ':', '-', '—', '–', '.'];

/// Terminate a recap clause with a single `.`, unless it already ends in
/// terminal punctuation or an ellipsis. Without this, a truncated clause
/// renders as `… witness ledger....`.
fn terminate_clause(clause: &str) -> String {
    let trimmed = clause.trim_end();
    if trimmed.ends_with("...")
        || trimmed.ends_with('…')
        || trimmed.ends_with('.')
        || trimmed.ends_with('!')
        || trimmed.ends_with('?')
    {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn sanitize_preview(text: &str) -> String {
    let no_literal_newline = text.replace("\\n", " ");
    let no_xml = XML_TAG_RE.replace_all(&no_literal_newline, "");
    let no_headings = MD_HEADING_RE.replace_all(&no_xml, "");
    let no_markdown = MD_BULLET_RE.replace_all(&no_headings, "");
    let mut result = no_markdown
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    result.retain(|character| !character.is_control() || character == ' ');
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::stop::{Episode, TodoItem};

    fn episode() -> Episode {
        Episode {
            schema: "session_episode/v2".into(),
            session_id: "session-1".into(),
            project: "csr".into(),
            timestamp: "2026-08-07T10:00:00Z".into(),
            request: "Fix\nthe recap composer".into(),
            investigated: vec![],
            completed: "Implemented deterministic recap output".into(),
            next_steps: Some("Run the integration suite".into()),
            blockers: Some("waiting for storage migration".into()),
            outcome: "partial".into(),
            error_signatures: vec![],
            tools_used: vec![],
            files_modified: vec![],
            message_count: 4,
            duration_minutes: 12,
            todos: vec![
                TodoItem {
                    content: "wire session start".into(),
                    status: "pending".into(),
                },
                TodoItem {
                    content: "write composer".into(),
                    status: "completed".into(),
                },
            ],
            approved_plan: None,
            prev_episode_id: None,
            anchors: vec![],
        }
    }

    fn full_feeds() -> RecapFeeds {
        RecapFeeds {
            settled: vec![
                SettledFact {
                    claim: "schema is stable".into(),
                    receipt: "810283b".into(),
                    status: "resolved".into(),
                },
                SettledFact {
                    claim: "queries are indexed".into(),
                    receipt: "T2 2026-07-27".into(),
                    status: "resolved".into(),
                },
            ],
            still_open: vec![SettledFact {
                claim: "Windows path behavior".into(),
                receipt: "91abcde".into(),
                status: "still_open".into(),
            }],
            retired_while_away: vec![RetiredLine {
                label: "old schema assumption".into(),
                receipt_oid: "77fedca".into(),
                date: "2026-08-06".into(),
            }],
            open_proposals: 2,
        }
    }

    #[test]
    fn composes_full_recap_in_causal_clause_order() {
        let got = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
        assert_eq!(
            got,
            "recap [28m ago]: Fix the recap composer: Implemented deterministic recap output.\n\
             Settled: schema is stable (810283b); queries are indexed (T2 2026-07-27).\n\
             Now: waiting for storage migration | Windows path behavior (91abcde) | 2 proposals awaiting csr_resolve | 1 todos open.\n\
             Learnt-then-retired while away: old schema assumption (superseded 2026-08-06, 77fedca).\n\
             Next: Run the integration suite."
        );
    }

    #[test]
    fn abstains_when_request_and_completed_are_provenance_filtered() {
        let mut ep = episode();
        ep.request = "CSR CONTINUUM [2m ago]: prior state".into();
        ep.completed = "## Session Intelligence (CSR v9.2)".into();
        assert_eq!(compose_recap(&ep, &full_feeds(), "2m ago"), None);
    }

    #[test]
    fn omits_next_without_next_steps_or_open_todos() {
        let mut ep = episode();
        ep.next_steps = None;
        ep.todos.clear();
        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(!got.contains("\nNext:"));
    }

    #[test]
    fn rejects_none_next_sentinels_and_uses_later_substantive_todo() {
        let mut ep = episode();
        ep.next_steps = Some("Next: none".into());
        ep.todos = vec![
            TodoItem {
                content: "none".into(),
                status: "pending".into(),
            },
            TodoItem {
                content: "wire session start".into(),
                status: "pending".into(),
            },
        ];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(got.ends_with("Next: wire session start."));
        assert!(!got.contains("Next: none"));
    }

    #[test]
    fn rejects_punctuation_only_next_values_and_uses_later_substantive_todo() {
        let mut ep = episode();
        ep.next_steps = Some(".".into());
        ep.todos = vec![
            TodoItem {
                content: "...".into(),
                status: "pending".into(),
            },
            TodoItem {
                content: "wire session start".into(),
                status: "pending".into(),
            },
        ];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(got.ends_with("Next: wire session start."));
    }

    #[test]
    fn rejects_unicode_punctuation_only_next_values_and_uses_later_substantive_todo() {
        let mut ep = episode();
        ep.next_steps = Some("\u{2026}".into()); // "…" — non-ASCII punctuation
        ep.todos = vec![
            TodoItem {
                content: "/".into(), // preserved by the ASCII trim, still no evidence
                status: "pending".into(),
            },
            TodoItem {
                content: "wire session start".into(),
                status: "pending".into(),
            },
        ];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(got.ends_with("Next: wire session start."));
        assert!(!got.contains("Next: \u{2026}"));
        assert!(!got.contains("Next: /"));
    }

    #[test]
    fn abstains_when_request_and_completed_are_punctuation_only() {
        let mut ep = episode();
        ep.request = "\u{2026}".into();
        ep.completed = "/".into();
        assert!(compose_recap(&ep, &RecapFeeds::empty(), "1h ago").is_none());
    }

    #[test]
    fn omits_punctuation_only_blocker_from_now_clause() {
        let mut ep = episode();
        ep.blockers = Some("\u{2026}".into());
        let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
        assert!(!got.contains('\u{2026}'));
    }

    #[test]
    fn skips_settled_fact_without_mandatory_receipt() {
        let feeds = RecapFeeds {
            settled: vec![
                SettledFact {
                    claim: "unreceipted".into(),
                    receipt: "".into(),
                    status: "resolved".into(),
                },
                SettledFact {
                    claim: "receipted".into(),
                    receipt: "abc1234".into(),
                    status: "resolved".into(),
                },
            ],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("Settled: receipted (abc1234)."));
        assert!(!got.contains("unreceipted"));
    }

    #[test]
    fn enforces_fact_status_partition_at_emission() {
        let feeds = RecapFeeds {
            settled: vec![
                SettledFact {
                    claim: "actually resolved".into(),
                    receipt: "abc1234".into(),
                    status: "resolved".into(),
                },
                SettledFact {
                    claim: "still open in settled feed".into(),
                    receipt: "def5678".into(),
                    status: "still_open".into(),
                },
            ],
            still_open: vec![
                SettledFact {
                    claim: "actually open".into(),
                    receipt: "fed4321".into(),
                    status: "regressed".into(),
                },
                SettledFact {
                    claim: "resolved in open feed".into(),
                    receipt: "cba8765".into(),
                    status: "resolved".into(),
                },
            ],
            ..RecapFeeds::empty()
        };

        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("Settled: actually resolved (abc1234)."));
        assert!(got.contains("Now: waiting for storage migration | actually open (fed4321)"));
        assert!(!got.contains("still open in settled feed"));
        assert!(!got.contains("resolved in open feed"));
    }

    #[test]
    fn bounds_still_open_facts_to_shared_query_limit() {
        let feeds = RecapFeeds {
            still_open: (0..=STILL_OPEN_LIMIT)
                .map(|index| SettledFact {
                    claim: format!("open fact {index}"),
                    receipt: format!("oid{index:04}"),
                    status: "still_open".into(),
                })
                .collect(),
            ..RecapFeeds::empty()
        };

        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains(&format!("open fact {}", STILL_OPEN_LIMIT - 1)));
        assert!(!got.contains(&format!("open fact {STILL_OPEN_LIMIT}")));
    }

    #[test]
    fn formats_at_most_two_retired_lines_with_receipts() {
        let feeds = RecapFeeds {
            retired_while_away: (1..=3)
                .map(|n| RetiredLine {
                    label: format!("line {n}"),
                    receipt_oid: format!("oid000{n}"),
                    date: format!("2026-08-0{n}"),
                })
                .collect(),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains(
            "Learnt-then-retired while away: line 1 (superseded 2026-08-01, oid0001); line 2 (superseded 2026-08-02, oid0002)."
        ));
        assert!(!got.contains("line 3"));
    }

    #[test]
    fn settled_clause_survives_when_other_optional_clauses_are_empty() {
        let mut ep = episode();
        ep.blockers = None;
        ep.next_steps = None;
        ep.todos.clear();
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "done".into(),
                receipt: "abc1234".into(),
                status: "resolved".into(),
            }],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&ep, &feeds, "now").unwrap();
        assert_eq!(got.lines().count(), 2);
        assert!(got.ends_with("Settled: done (abc1234)."));
    }

    #[test]
    fn now_clause_survives_when_other_optional_clauses_are_empty() {
        let mut ep = episode();
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = Some("blocked on fixture".into());
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(got.lines().count(), 2);
        assert!(got.ends_with("Now: blocked on fixture."));
    }

    #[test]
    fn caps_output_and_drops_whole_receipted_entries() {
        let mut ep = episode();
        ep.request = "r".repeat(200);
        ep.completed = "c".repeat(250);
        let feeds = RecapFeeds {
            settled: (0..4)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "settled".repeat(30)),
                    receipt: format!("receipt{n}"),
                    status: "resolved".into(),
                })
                .collect(),
            still_open: (0..4)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "open".repeat(30)),
                    receipt: format!("openoid{n}"),
                    status: "still_open".into(),
                })
                .collect(),
            retired_while_away: vec![RetiredLine {
                label: "retired".repeat(50),
                receipt_oid: "retired-receipt".into(),
                date: "2026-08-07".into(),
            }],
            open_proposals: 8,
        };
        let got = compose_recap(&ep, &feeds, "now").unwrap();
        assert!(got.chars().count() <= 700);
        assert!(got.lines().count() <= 5);
        for fragment in got.split([';', '\n']) {
            if fragment.contains("settledsettled") || fragment.contains("openopen") {
                assert!(fragment.contains(')'));
            }
        }
    }

    #[test]
    fn trusted_feed_text_is_passed_through_without_provenance_filtering() {
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "CSR CONTINUUM retained evidence".into(),
                receipt: "abc1234".into(),
                status: "resolved".into(),
            }],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("CSR CONTINUUM retained evidence (abc1234)"));
    }

    #[test]
    fn next_falls_back_to_first_open_todo_and_filters_episode_meta() {
        let mut ep = episode();
        ep.next_steps = Some("CSR CONTINUUM [2m ago]: noise".into());
        ep.todos = vec![
            TodoItem {
                content: "first completed".into(),
                status: "completed".into(),
            },
            TodoItem {
                content: "ship\nthe recap".into(),
                status: "in_progress".into(),
            },
        ];
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert!(got.ends_with("Next: ship the recap."));
    }

    #[test]
    fn uses_the_surviving_episode_field_without_inventing_a_placeholder() {
        let mut ep = episode();
        ep.request = "CSR CONTINUUM [2m ago]: noise".into();
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert_eq!(got, "recap [now]: Implemented deterministic recap output.");
    }

    #[test]
    fn completed_preview_keeps_verdict_and_closing_state() {
        let mut ep = episode();
        ep.completed = format!(
            "Done. {} Final verdict: 52754 chunks live.",
            "intermediate filler sentence. ".repeat(10)
        );
        ep.next_steps = None;
        ep.todos.clear();
        ep.blockers = None;
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert!(got.contains("Done. … Final verdict: 52754 chunks live."));
        assert!(!got.contains("intermediate filler"));
    }

    #[test]
    fn provenance_filters_csr_meta_blockers_from_now() {
        let mut ep = episode();
        ep.blockers = Some("CSR CONTINUUM [2m ago]: injected state".into());
        ep.next_steps = None;
        ep.todos.clear();
        let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
        assert!(!got.contains("\nNow:"));
        assert!(!got.contains("injected state"));
    }

    #[test]
    fn long_early_lists_preserve_retired_and_next_clauses() {
        let feeds = RecapFeeds {
            settled: (0..3)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "settled evidence ".repeat(10)),
                    receipt: format!("settled{n}"),
                    status: "resolved".into(),
                })
                .collect(),
            still_open: (0..4)
                .map(|n| SettledFact {
                    claim: format!("{}-{n}", "open evidence ".repeat(10)),
                    receipt: format!("openoid{n}"),
                    status: "still_open".into(),
                })
                .collect(),
            retired_while_away: vec![RetiredLine {
                label: "obsolete parser assumption".into(),
                receipt_oid: "77fedca".into(),
                date: "2026-08-06".into(),
            }],
            open_proposals: 3,
        };
        let got = compose_recap(&episode(), &feeds, "28m ago").unwrap();
        assert!(got.contains("\nSettled:"));
        assert!(got.contains("\nNow:"));
        assert!(got.contains("\nLearnt-then-retired while away:"));
        assert!(got.contains("\nNext: Run the integration suite."));
        assert!(got.chars().count() <= 700);
        assert!(got.lines().count() <= 5);
    }

    #[test]
    fn multiline_trusted_feeds_are_structurally_flat_but_not_filtered() {
        let feeds = RecapFeeds {
            settled: vec![SettledFact {
                claim: "CSR CONTINUUM\nretained\tevidence".into(),
                receipt: "abc\n1234".into(),
                status: "resolved".into(),
            }],
            retired_while_away: vec![RetiredLine {
                label: "old\nline".into(),
                receipt_oid: "77f\nedca".into(),
                date: "2026-08-\n06".into(),
            }],
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(got.contains("CSR CONTINUUM retained evidence (abc 1234)"));
        assert!(got.contains("old line (superseded 2026-08- 06, 77f edca)"));
        assert!(got.lines().count() <= 5);
        assert!(got.chars().count() <= 700);
    }

    impl RecapFeeds {
        fn empty() -> Self {
            Self {
                settled: vec![],
                still_open: vec![],
                retired_while_away: vec![],
                open_proposals: 0,
            }
        }
    }

    // ---- readability regressions from the 10.1.0 live smoke ------------
    //
    // The first real recap read `...never thought about it v10 of CSR b...:
    // Current state: Merged to main: - #281 ... witness ledger,....` — a
    // mid-word cut, flattened bullets, and a comma stacked against the
    // ellipsis and clause terminator.

    #[test]
    fn compact_preview_never_cuts_mid_word() {
        let got = compact_preview(
            "dreaming research on v10 of CSR before anything shipped",
            30,
        );
        assert!(got.ends_with("..."), "expected an ellipsis, got {got:?}");
        let body = got.trim_end_matches('.');
        assert!(
            body.split_whitespace().last().is_some_and(|word| {
                "dreaming research on v10 of CSR before anything shipped"
                    .split_whitespace()
                    .any(|whole| whole == word)
            }),
            "last word was cut mid-token: {got:?}"
        );
    }

    #[test]
    fn truncation_drops_trailing_punctuation_before_the_ellipsis() {
        let got = truncate_at_word_boundary("codewitness, witness ledger, deterministic", 26);
        assert!(!got.contains(",..."), "comma survived the cut: {got:?}");
        assert!(got.ends_with("..."), "expected an ellipsis, got {got:?}");
    }

    #[test]
    fn clause_terminator_is_never_doubled() {
        assert_eq!(terminate_clause("witness ledger..."), "witness ledger...");
        assert_eq!(terminate_clause("done already."), "done already.");
        assert_eq!(terminate_clause("shipped it"), "shipped it.");
        assert_eq!(terminate_clause("why not?"), "why not?");
    }

    #[test]
    fn markdown_bullets_do_not_leak_into_the_paragraph() {
        let cleaned =
            sanitize_preview("Merged to main:\n- #281 landed\n- #282 landed\n2. then tag");
        assert!(!cleaned.contains("- #281"), "bullet survived: {cleaned:?}");
        assert!(
            !cleaned.contains("2. then"),
            "ordered marker survived: {cleaned:?}"
        );
        assert!(cleaned.contains("#281 landed"));
    }

    #[test]
    fn live_shaped_episode_reads_as_prose() {
        let mut ep = episode();
        ep.request =
            "Dreaming, we researched it chatted around it never thought about it v10 of CSR being \
             the release that makes it real"
                .into();
        ep.completed = "Current state:\n- Merged to main: #281 v10 dreaming (codewitness, witness \
                        ledger, deterministic supersession)\n- #282 recap layer"
            .into();
        ep.next_steps = None;
        ep.blockers = None;
        ep.todos = vec![];

        let got = compose_recap(&ep, &RecapFeeds::empty(), "2m ago").unwrap();
        assert!(!got.contains("...."), "stacked punctuation: {got:?}");
        assert!(!got.contains(",..."), "comma against ellipsis: {got:?}");
        assert!(!got.contains(": - "), "flattened bullet: {got:?}");
        assert!(!got.contains("Next:"), "next was fabricated: {got:?}");
    }
}
