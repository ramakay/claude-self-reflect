//! The SessionStart `learnt` line: what CSR *stored* during the last session.
//!
//! Pure composition from already-read feeds; no I/O, no model call. Every clause
//! carries a receipt (session id and date, commit oid, or the verdict's own
//! evidence token); a clause without one is dropped. No clause survives, no line:
//! nothing is padded or invented. The clause labels say what was stored, never
//! what is true.
//!
//! Grammar (one line, no newline; the sentinel is counted in the budget):
//! `learnt [<age>]: <clause>(; <clause>)*. [[CSR:LEARNT]]`
//! with clauses in priority order `confirmed`, `notes`, `verdicts`.

use crate::extraction::provenance::{contains_machine_sentinel, LEARNT_SENTINEL};
use crate::import::contamination::has_csr_text;

/// Byte budget for the whole line, sentinel included (~80 tokens).
pub const LEARNT_MAX_BYTES: usize = 320;
/// Memory-note slugs named in the line.
const MAX_SLUGS: usize = 3;
/// Dream-verdict commits named in the line.
const MAX_OIDS: usize = 2;
/// Claim excerpt length (chars) at full size.
const CLAIM_CHARS: usize = 60;

/// A user-confirmed `csr_resolve` verdict recorded in the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedItem {
    pub claim: String,
    pub status: String,
    pub receipt: String,
}

/// Dream verdicts retired by one commit: distinct witnesses, commit date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictGroup {
    pub oid: String,
    pub count: usize,
    pub date: String,
}

/// Evidence read for one session. Empty by default: an unreadable feed is an
/// empty feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LearntFeeds {
    /// Newest first; at most a handful, `confirmed_total` counts them all.
    pub confirmed: Vec<ConfirmedItem>,
    pub confirmed_total: usize,
    /// Memory-note slugs, newest first (more than are shown, so the composer
    /// can skip a slug the recap already names).
    pub notes: Vec<String>,
    pub notes_total: usize,
    /// `YYYY-MM-DD` of the newest note.
    pub notes_date: Option<String>,
    /// Count-descending; only populated under `CSR_DREAM_CONSUMPTION=full`.
    pub verdicts: Vec<VerdictGroup>,
}

/// One whitespace-collapsed line.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Feed text safe to print: flattened, and never CSR's own text or a sentinel
/// (a stored note that quotes a recap must not smuggle a second one in).
fn clean(text: &str) -> Option<String> {
    let text = flat(text);
    (!text.is_empty() && !has_csr_text(&text) && !contains_machine_sentinel(&text)).then_some(text)
}

fn excerpt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 {
        one
    } else {
        many
    }
}

fn more(total: usize, shown: usize) -> String {
    if total > shown {
        format!(", +{} more", total - shown)
    } else {
        String::new()
    }
}

/// A clause that can render at decreasing sizes; level 0 is the fullest.
struct Clause {
    renders: Vec<String>,
}

impl Clause {
    fn at(&self, level: usize) -> &str {
        &self.renders[level.min(self.renders.len() - 1)]
    }
}

fn confirmed_clause(feeds: &LearntFeeds, recap: &str) -> Option<Clause> {
    let items: Vec<(String, &ConfirmedItem, String)> = feeds
        .confirmed
        .iter()
        .filter_map(|item| {
            let receipt = clean(&item.receipt)?;
            let status = clean(&item.status)?;
            (!recap.contains(&receipt)).then_some((receipt, item, status))
        })
        .collect();
    let (receipt, item, status) = items.first()?;
    let claim = clean(&item.claim);
    let total = feeds.confirmed_total.max(items.len());
    let render = |chars: Option<usize>| {
        let claim = match (chars, claim.as_deref()) {
            (Some(n), Some(c)) => format!(" {}", excerpt(c, n)),
            _ => String::new(),
        };
        let lead = format!(
            "{total} csr_resolve {}:",
            plural(total, "verdict", "verdicts")
        );
        format!("{lead}{claim} {status} ({receipt}){}", more(total, 1))
    };
    Some(Clause {
        renders: vec![
            render(Some(CLAIM_CHARS)),
            render(Some(40)),
            render(Some(24)),
            render(None),
        ],
    })
}

fn notes_clause(feeds: &LearntFeeds, session8: &str, recap: &str) -> Option<Clause> {
    let date = feeds.notes_date.as_deref().and_then(clean)?;
    let session8 = clean(session8)?;
    let slugs: Vec<String> = feeds
        .notes
        .iter()
        .filter_map(|slug| clean(slug))
        .filter(|slug| !recap.contains(slug.as_str()))
        .take(MAX_SLUGS)
        .collect();
    let total = feeds.notes_total.max(feeds.notes.len());
    if total == 0 {
        return None;
    }
    let render = |shown: usize| {
        let names = &slugs[..shown.min(slugs.len())];
        let lead = format!("{total} memory {}", plural(total, "note", "notes"));
        let list = if names.is_empty() {
            String::new()
        } else {
            format!(": {}", names.join(", "))
        };
        format!(
            "{lead}{list}{} ({session8}, {date})",
            more(total, names.len())
        )
    };
    let top = slugs.len();
    let mut renders = vec![render(top)];
    for shown in (0..top).rev() {
        renders.push(render(shown));
    }
    Some(Clause { renders })
}

fn verdicts_clause(feeds: &LearntFeeds, recap: &str) -> Option<Clause> {
    let groups: Vec<(String, usize, String)> = feeds
        .verdicts
        .iter()
        .filter(|g| g.count > 0)
        .filter_map(|g| Some((clean(&g.oid)?, g.count, clean(&g.date)?)))
        .filter(|(oid, _, _)| !recap.contains(oid.as_str()))
        .take(MAX_OIDS)
        .collect();
    if groups.is_empty() {
        return None;
    }
    let render = |take: usize| {
        let parts: Vec<String> = groups[..take]
            .iter()
            .enumerate()
            .map(|(i, (oid, count, date))| {
                if i == 0 {
                    format!(
                        "{count} {} retired by {oid} ({date})",
                        plural(*count, "anchor", "anchors")
                    )
                } else {
                    format!("{count} by {oid} ({date})")
                }
            })
            .collect();
        parts.join(", ")
    };
    let mut renders = vec![render(groups.len())];
    if groups.len() > 1 {
        renders.push(render(1));
    }
    Some(Clause { renders })
}

fn assemble(age: &str, clauses: &[&str]) -> String {
    format!("learnt [{age}]: {}. {LEARNT_SENTINEL}", clauses.join("; "))
}

/// Compose the line, or `None` when nothing has a receipt or it cannot fit.
///
/// `session8` is the first eight characters of the learnt session's id, the
/// receipt on the notes clause. `recap_text` is what the recap already shows:
/// an item whose receipt token (commit oid, slug, ledger receipt) appears in it
/// is dropped, so the two never repeat each other.
pub fn compose_learnt(
    feeds: &LearntFeeds,
    age: &str,
    session8: &str,
    recap_text: &str,
) -> Option<String> {
    let age = clean(age).filter(|a| a.chars().count() <= 24 && !a.contains(['[', ']']))?;
    let clauses: Vec<Clause> = [
        confirmed_clause(feeds, recap_text),
        notes_clause(feeds, session8, recap_text),
        verdicts_clause(feeds, recap_text),
    ]
    .into_iter()
    .flatten()
    .collect();
    if clauses.is_empty() {
        return None;
    }
    // Drop whole clauses from the tail first (verdicts, then notes) ...
    for keep in (1..=clauses.len()).rev() {
        let parts: Vec<&str> = clauses[..keep].iter().map(|c| c.at(0)).collect();
        let line = assemble(&age, &parts);
        if line.len() <= LEARNT_MAX_BYTES {
            return Some(line);
        }
    }
    // ... then shorten what is left of the first clause. Never cut mid-clause.
    let first = &clauses[0];
    (1..first.renders.len())
        .map(|level| assemble(&age, &[first.at(level)]))
        .find(|line| line.len() <= LEARNT_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes17() -> LearntFeeds {
        LearntFeeds {
            notes: vec![
                "subagent-compact-window".into(),
                "keep-setup-simple-few-copies".into(),
                "release-gate-matches-ci".into(),
                "older-note".into(),
            ],
            notes_total: 17,
            notes_date: Some("2026-10-16".into()),
            ..Default::default()
        }
    }

    fn verdicts() -> Vec<VerdictGroup> {
        vec![
            VerdictGroup {
                oid: "12bf158".into(),
                count: 127,
                date: "2026-10-13".into(),
            },
            VerdictGroup {
                oid: "d895eea".into(),
                count: 50,
                date: "2026-10-13".into(),
            },
        ]
    }

    #[test]
    fn example_1_notes_only_golden() {
        let line = compose_learnt(&notes17(), "yesterday", "6ca2a4d5", "").unwrap();
        assert_eq!(
            line,
            "learnt [yesterday]: 17 memory notes: subagent-compact-window, keep-setup-simple-few-copies, release-gate-matches-ci, +14 more (6ca2a4d5, 2026-10-16). [[CSR:LEARNT]]"
        );
        assert_eq!(line.len(), 164);
    }

    #[test]
    fn example_3_notes_then_verdicts_golden() {
        let mut feeds = notes17();
        feeds.verdicts = verdicts();
        let line = compose_learnt(&feeds, "yesterday", "6ca2a4d5", "").unwrap();
        assert_eq!(
            line,
            "learnt [yesterday]: 17 memory notes: subagent-compact-window, keep-setup-simple-few-copies, release-gate-matches-ci, +14 more (6ca2a4d5, 2026-10-16); 127 anchors retired by 12bf158 (2026-10-13), 50 by d895eea (2026-10-13). [[CSR:LEARNT]]"
        );
        assert_eq!(line.len(), 237);
    }

    fn confirmed_feeds() -> LearntFeeds {
        LearntFeeds {
            confirmed: vec![
                ConfirmedItem {
                    claim: "Fix claude-review 404: bump claude-code-action to the pinned tag"
                        .into(),
                    status: "resolved".into(),
                    receipt: "a624305".into(),
                },
                ConfirmedItem {
                    claim: "older".into(),
                    status: "resolved".into(),
                    receipt: "73878dd".into(),
                },
            ],
            confirmed_total: 2,
            ..Default::default()
        }
    }

    #[test]
    fn confirmed_clause_leads_and_counts_the_rest() {
        let line = compose_learnt(&confirmed_feeds(), "3h ago", "6ca2a4d5", "").unwrap();
        assert_eq!(
            line,
            "learnt [3h ago]: 2 csr_resolve verdicts: Fix claude-review 404: bump claude-code-action to the pinne… resolved (a624305), +1 more. [[CSR:LEARNT]]"
        );
        let mut both = confirmed_feeds();
        both.notes = notes17().notes;
        both.notes_total = 4;
        both.notes_date = Some("2026-10-16".into());
        let line = compose_learnt(&both, "3h ago", "6ca2a4d5", "").unwrap();
        assert!(line.find("csr_resolve").unwrap() < line.find("memory notes").unwrap());
    }

    #[test]
    fn no_evidence_means_no_line() {
        assert_eq!(
            compose_learnt(&LearntFeeds::default(), "1h ago", "6ca2a4d5", ""),
            None
        );
        // A note total with no date has no receipt: dropped.
        let mut feeds = notes17();
        feeds.notes_date = None;
        assert_eq!(compose_learnt(&feeds, "1h ago", "6ca2a4d5", ""), None);
        // A confirmed verdict with no receipt is dropped.
        let mut feeds = confirmed_feeds();
        for item in &mut feeds.confirmed {
            item.receipt = "  ".into();
        }
        assert_eq!(compose_learnt(&feeds, "1h ago", "6ca2a4d5", ""), None);
    }

    #[test]
    fn singular_labels_and_no_more_suffix_when_all_shown() {
        let feeds = LearntFeeds {
            notes: vec!["only-note".into()],
            notes_total: 1,
            notes_date: Some("2026-10-16".into()),
            verdicts: vec![VerdictGroup {
                oid: "abc1234".into(),
                count: 1,
                date: "2026-10-12".into(),
            }],
            ..Default::default()
        };
        assert_eq!(
            compose_learnt(&feeds, "2d ago", "6ca2a4d5", "").unwrap(),
            "learnt [2d ago]: 1 memory note: only-note (6ca2a4d5, 2026-10-16); 1 anchor retired by abc1234 (2026-10-12). [[CSR:LEARNT]]"
        );
    }

    #[test]
    fn output_is_one_line_sentinel_last_and_within_budget_with_the_drop_order() {
        let mut feeds = notes17();
        feeds.verdicts = verdicts();
        feeds.notes[0] = "n".repeat(120);
        // The long slug pushes the verdicts clause out first.
        let line = compose_learnt(&feeds, "yesterday", "6ca2a4d5", "").unwrap();
        assert!(line.len() <= LEARNT_MAX_BYTES, "{} bytes", line.len());
        assert!(!line.contains('\n'));
        assert!(line.ends_with(LEARNT_SENTINEL));
        assert_eq!(line.matches(LEARNT_SENTINEL).count(), 1);
        assert!(line.contains("memory notes"));
        assert!(!line.contains("retired by"));
    }

    #[test]
    fn a_first_clause_over_budget_is_shortened_then_dropped_if_it_cannot_fit() {
        let mut feeds = notes17();
        feeds.notes = vec!["s".repeat(150), "t".repeat(150), "u".repeat(150)];
        let line = compose_learnt(&feeds, "yesterday", "6ca2a4d5", "").unwrap();
        assert!(line.len() <= LEARNT_MAX_BYTES);
        // Fewer slugs, and the "+K more" tracks what was left out.
        assert!(
            line.contains("+16 more") || line.contains("+15 more") || line.contains("+14 more")
        );
        let mut hopeless = feeds.clone();
        hopeless.notes_date = Some("2026-10-16".into());
        hopeless.notes = Vec::new();
        // Receipt alone always fits; only an absurd age label cannot.
        assert!(compose_learnt(&hopeless, "yesterday", "6ca2a4d5", "").is_some());
        assert_eq!(
            compose_learnt(&hopeless, &"x".repeat(60), "6ca2a4d5", ""),
            None
        );
    }

    #[test]
    fn recap_text_dedupe_drops_a_matching_oid_slug_or_receipt() {
        let mut feeds = notes17();
        feeds.verdicts = verdicts();
        feeds.confirmed = confirmed_feeds().confirmed;
        feeds.confirmed_total = 2;
        let recap = "recap [1d ago]: x. Dreamt: foo retired (12bf158). Settled: y (a624305) keep-setup-simple-few-copies";
        let line = compose_learnt(&feeds, "yesterday", "6ca2a4d5", recap).unwrap();
        // The newest confirmed item was already shown; the next one leads.
        assert!(line.contains("older resolved (73878dd)"), "{line}");
        assert!(!line.contains("a624305"));
        assert!(!line.contains("keep-setup-simple-few-copies,"));
        assert!(line.contains("older-note"));
        assert!(!line.contains("12bf158"));
        assert!(line.contains("50 anchors retired by d895eea"));
        // Everything already in the recap: nothing left, no line.
        let all = "12bf158 d895eea";
        let only_verdicts = LearntFeeds {
            verdicts: verdicts(),
            ..Default::default()
        };
        assert_eq!(
            compose_learnt(&only_verdicts, "1d ago", "6ca2a4d5", all),
            None
        );
    }

    #[test]
    fn composer_drops_item_carrying_a_sentinel_or_csr_text() {
        let poisoned = [
            "note [[CSR:RECAP]] quoted",
            "x [[CSR:LEARNT]] y",
            "recap [2m ago]: Fixed the importer. [[CSR:RECAP]]",
            "[[CSR:DREAM]]",
        ];
        for text in poisoned {
            let feeds = LearntFeeds {
                notes: vec![text.into(), "fine-slug".into()],
                notes_total: 2,
                notes_date: Some("2026-10-16".into()),
                confirmed: vec![ConfirmedItem {
                    claim: text.into(),
                    status: "resolved".into(),
                    receipt: "abc1234".into(),
                }],
                confirmed_total: 1,
                ..Default::default()
            };
            let line = compose_learnt(&feeds, "1h ago", "6ca2a4d5", "").unwrap();
            assert_eq!(line.matches("[[CSR:").count(), 1, "{line}");
            assert!(line.ends_with(LEARNT_SENTINEL));
            assert!(line.contains("fine-slug"));
            // A poisoned claim is left out; the verdict still stands on its receipt.
            assert!(
                line.contains("1 csr_resolve verdict: resolved (abc1234)"),
                "{line}"
            );
        }
        // A poisoned receipt drops the whole clause (no receipt, no claim).
        let feeds = LearntFeeds {
            confirmed: vec![ConfirmedItem {
                claim: "ok".into(),
                status: "resolved".into(),
                receipt: "[[CSR:RECAP]]".into(),
            }],
            confirmed_total: 1,
            ..Default::default()
        };
        assert_eq!(compose_learnt(&feeds, "1h ago", "6ca2a4d5", ""), None);
    }

    #[test]
    fn db_text_cannot_add_a_line() {
        let feeds = LearntFeeds {
            notes: vec!["multi\nline\r\nslug".into()],
            notes_total: 1,
            notes_date: Some("2026-10-16".into()),
            ..Default::default()
        };
        let line = compose_learnt(&feeds, "1h ago", "6ca2a4d5", "").unwrap();
        assert!(!line.contains('\n') && !line.contains('\r'));
        assert!(line.contains("multi line slug"));
    }

    #[test]
    fn the_line_is_recognised_by_the_machine_sentinel_and_the_import_scrub() {
        let mut feeds = notes17();
        feeds.verdicts = verdicts();
        let line = compose_learnt(&feeds, "yesterday", "6ca2a4d5", "").unwrap();
        assert!(contains_machine_sentinel(&line));
        assert!(crate::extraction::provenance::is_csr_emission(&line));
        assert!(crate::import::contamination::has_csr_text(&line));
    }
}
