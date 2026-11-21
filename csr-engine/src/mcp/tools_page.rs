use super::*;

/// How many rows page assembly left out because another row of the pool holds
/// the same text. Carried with the pool so every page of one query reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Collapsed {
    /// Codex rollout rows whose text a Claude session row also holds.
    pub cross_source: usize,
    /// Rows whose text an earlier-ranked row already holds, in any conversation.
    pub same_text: usize,
}

/// Rows shorter than this (in characters, after trimming) are never collapsed:
/// a short reply such as "yes" is not one memory repeated.
const SAME_TEXT_MIN_CHARS: usize = 24;

/// `CSR_NO_SAME_TEXT_COLLAPSE=1` keeps every copy, for comparing runs.
fn same_text_collapse_enabled() -> bool {
    !std::env::var("CSR_NO_SAME_TEXT_COLLAPSE").is_ok_and(|value| value == "1")
}

/// Leave out every row whose text (whitespace and case folded, the
/// `[keyword] ` marker ignored) an earlier row already holds, whichever
/// conversation either belongs to. The earlier row is the better ranked one and
/// stays, so the first row of the page, the order of the rest and which
/// conversation ranks first are unchanged; rows below the copy move up. Runs
/// before the page cut, so the freed places refill from the pool. Rows shorter
/// than [`SAME_TEXT_MIN_CHARS`] stay. Returns how many rows it left out.
///
/// Within one conversation `format::dedupe_results` already does this on a
/// 200-character prefix, and `collapse_cross_source_duplicates` does it for a
/// Codex copy of a Claude row. What remains is the same text in two Claude
/// conversations: a pasted brief, a compaction summary, a skill body.
pub(super) fn collapse_same_text(enriched: &mut Vec<EnrichedResult>) -> usize {
    if !same_text_collapse_enabled() {
        return 0;
    }
    let before = enriched.len();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    enriched.retain(|row| same_text_key(&row.chunk.content).is_none_or(|key| seen.insert(key)));
    before - enriched.len()
}

/// The key [`collapse_same_text`] groups a row by: its text with whitespace and
/// case folded and the `[keyword] ` marker ignored. `None` for a row shorter
/// than [`SAME_TEXT_MIN_CHARS`], which is never collapsed.
pub(super) fn same_text_key(content: &str) -> Option<[u8; 32]> {
    let text = content.strip_prefix("[keyword] ").unwrap_or(content);
    (text.trim().chars().count() >= SAME_TEXT_MIN_CHARS).then(|| cross_source_key(content))
}

/// How many of `rows` the page could show once [`collapse_same_text`] has run:
/// every row whose text no earlier one of `rows` holds, and every short row.
/// With `CSR_NO_SAME_TEXT_COLLAPSE=1` every row counts. The adaptive refetch
/// counts these, not the rows, so a window filled by copies of one text widens
/// and the page fills from the distinct rows beyond it.
pub(super) fn distinct_rows<'a>(rows: impl Iterator<Item = &'a EnrichedResult>) -> usize {
    let collapse = same_text_collapse_enabled();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    rows.filter(|row| {
        !collapse || same_text_key(&row.chunk.content).is_none_or(|key| seen.insert(key))
    })
    .count()
}

/// Say so in `<meta>` when the page holds fewer than `limit` rows because the
/// same-text collapse removed copies AND the last candidate window came back
/// full, so distinct rows may sit beyond it. The adaptive refetch reads one
/// wider window (three times the first) when copies crowd the first; copies
/// that fill even that window leave the page short, and this line keeps it
/// from reading as "nothing more". Absent otherwise: no byte changes.
pub(super) fn annotate_short_page(xml: String, limit: usize, page: &FinalPage) -> String {
    let shown = page.enriched.len();
    if shown >= limit || page.exhausted || page.cross_source_collapsed.same_text == 0 {
        return xml;
    }
    xml.replacen(
        "  </meta>\n",
        &format!(
            "    <short-page>{shown} of {limit} rows: copies of one text filled the candidate window, so distinct rows may sit beyond it and are not shown</short-page>\n  </meta>\n"
        ),
        1,
    )
}

/// Add the per-search collapse counters to the `<meta>` block of a rendered
/// page. Each line is absent when its count is zero, so a page without the
/// event is byte for byte what it was.
pub(super) fn annotate_collapsed(xml: String, collapsed: Collapsed) -> String {
    let mut lines = String::new();
    if collapsed.cross_source > 0 {
        lines.push_str(&format!(
            "    <cross-source-collapsed>{}</cross-source-collapsed>\n",
            collapsed.cross_source
        ));
    }
    if collapsed.same_text > 0 {
        lines.push_str(&format!(
            "    <same-text-collapsed>{}</same-text-collapsed>\n",
            collapsed.same_text
        ));
    }
    if lines.is_empty() {
        return xml;
    }
    xml.replacen("  </meta>\n", &format!("{lines}  </meta>\n"), 1)
}

/// What a recency page tells the caller when its window holds nothing that
/// clears the relevance floor: that only this window was searched, and which
/// window is the next wider one. `None` when a row clears the floor. Rows with
/// no similarity score (keyword fallback) cannot clear it, so a page of them is
/// told its window only when it is empty.
pub(super) fn recency_hint(
    time_range: Option<&str>,
    start: &chrono::DateTime<chrono::Utc>,
    end: &chrono::DateTime<chrono::Utc>,
    results: &[EnrichedResult],
) -> Option<String> {
    let keyword = results
        .first()
        .is_some_and(|row| row.chunk.content.starts_with("[keyword] "));
    let clears = results
        .iter()
        .any(|row| !keyword && row.score >= format::QUICK_CHECK_FLOOR);
    if clears || (keyword && !results.is_empty()) {
        return None;
    }
    Some(window_hint(
        time_range,
        start,
        end,
        results.is_empty(),
        chrono::Utc::now(),
    ))
}

fn window_hint(
    time_range: Option<&str>,
    start: &chrono::DateTime<chrono::Utc>,
    end: &chrono::DateTime<chrono::Utc>,
    empty: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let searched = temporal::describe_window(time_range, start, end);
    let lead = if empty {
        format!("Only {searched} was searched; nothing outside it is shown.")
    } else {
        format!(
            "No result in {searched} clears the relevance floor of {:.2}, so the rows here are weak leads, not matches. Only this window was searched.",
            format::QUICK_CHECK_FLOOR
        )
    };
    let wider = time_range
        .and_then(temporal::next_wider_expression)
        .and_then(|expr| {
            let (from, to) = temporal::parse_time_expression_at(&expr, now).ok()?;
            Some(format!(
                "The next wider window is {expr} ({}); search again with time_range={expr}.",
                temporal::describe_window(Some(&expr), &from, &to)
            ))
        })
        .unwrap_or_else(|| "Widen time_range, since or until to look further back.".to_string());
    format!("{lead} {wider}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d6_an_empty_last_month_names_its_bounds_and_the_next_wider_window() {
        use chrono::TimeZone;
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 11, 17, 18, 30, 0)
            .unwrap();
        let (start, end) = temporal::parse_time_expression_at("last month", now).unwrap();
        let hint = window_hint(Some("last month"), &start, &end, true, now);
        assert!(
            hint.contains("2026-10-01T00:00:00Z to 2026-11-01T00:00:00Z UTC"),
            "{hint}"
        );
        assert!(hint.contains("previous calendar month"), "{hint}");
        assert!(hint.contains("last 2 months"), "{hint}");
        assert!(
            hint.contains("2026-09-01T00:00:00Z to 2026-11-17T18:30:00Z"),
            "{hint}"
        );
    }

    #[test]
    fn d6_only_a_row_over_the_floor_silences_the_hint() {
        let (start, end) = temporal::parse_time_expression("last month").unwrap();
        let under = vec![row("a", "c", "an older related thread", 0.40)];
        let over = vec![row("a", "c", "a real match in the window", 0.60)];
        assert!(recency_hint(Some("last month"), &start, &end, &under)
            .unwrap()
            .contains("clears the relevance floor"));
        assert!(recency_hint(Some("last month"), &start, &end, &over).is_none());
        assert!(recency_hint(Some("last month"), &start, &end, &[])
            .unwrap()
            .contains("nothing outside it is shown"));
        // A window given by since and until has no named wider window.
        let hint = recency_hint(None, &start, &end, &[]).unwrap();
        assert!(hint.contains("Widen time_range, since or until"), "{hint}");
    }

    fn row(id: &str, conversation: &str, content: &str, score: f32) -> EnrichedResult {
        EnrichedResult {
            score,
            chunk: crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation.into(),
                project_name: "p".into(),
                timestamp: "2026-11-01T00:00:00Z".into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            },
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        }
    }

    const BRIEF: &str = "Standing brief: keep receipts for every claim you make";

    #[test]
    fn identical_text_in_two_conversations_keeps_only_the_better_ranked_row() {
        let mut rows = vec![
            row("a", "conv-1", BRIEF, 0.9),
            row(
                "b",
                "conv-2",
                "A different topic that is long enough to keep",
                0.8,
            ),
            row("c", "conv-3", &BRIEF.to_uppercase(), 0.7),
            row("d", "conv-4", &format!("[keyword] {BRIEF}"), 0.4),
        ];
        let dropped = collapse_same_text(&mut rows);
        let ids: Vec<&str> = rows.iter().map(|r| r.chunk.id.as_str()).collect();
        assert_eq!(dropped, 2);
        assert_eq!(ids, ["a", "b"], "rank 1 and the order of the rest hold");
    }

    #[test]
    fn short_rows_and_distinct_rows_are_never_collapsed() {
        let mut rows = vec![
            row("a", "conv-1", "yes", 0.9),
            row("b", "conv-2", "yes", 0.8),
            row("c", "conv-3", "one distinct memory with its own words", 0.7),
            row(
                "d",
                "conv-4",
                "another distinct memory with other words",
                0.6,
            ),
        ];
        assert_eq!(collapse_same_text(&mut rows), 0);
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn collapse_notes_are_added_to_meta_only_when_something_collapsed() {
        let page = "<search>\n  <meta>\n    <q>x</q>\n  </meta>\n</search>\n".to_string();
        assert_eq!(annotate_collapsed(page.clone(), Collapsed::default()), page);
        let both = annotate_collapsed(
            page.clone(),
            Collapsed {
                cross_source: 1,
                same_text: 3,
            },
        );
        assert!(both.contains(
            "    <cross-source-collapsed>1</cross-source-collapsed>\n    <same-text-collapsed>3</same-text-collapsed>\n  </meta>\n"
        ));
        let only = annotate_collapsed(
            page,
            Collapsed {
                cross_source: 0,
                same_text: 2,
            },
        );
        assert!(!only.contains("cross-source"));
        assert!(only.contains("<same-text-collapsed>2</same-text-collapsed>"));
    }
}
