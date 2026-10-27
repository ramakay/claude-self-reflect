//! S8 partition before ranking: which candidate units later evidence retired.
//!
//! Pure module. The caller reads the assertion rows (one batched query) and the
//! claim rules, builds one [`Unit`] per candidate (chunk rows and episode rows
//! alike), and gets back the units that must leave every fused channel
//! (`sunk`) and a receipt note for each unit that carries an assertion
//! (`notes`). Nothing rises: a superseded unit only sinks, so session max and
//! the cross-encoder pool never see it.
//!
//! A unit sinks when one of these holds:
//!
//! * its id has a `superseded` chunk assertion;
//! * its session has a `superseded` session assertion;
//! * a user-decision claim matches its text (the frozen rule: every subject
//!   term, one superseded phrase, no current phrase).
//!
//! Precedence per unit is superseded, then annotate, then proposed. Inside one
//! state a chunk assertion beats a session assertion beats a claim. A note
//! names the key and the receipt only; no claim or current-claim text enters it.

use std::collections::{HashMap, HashSet};

use crate::storage::assertions::{Assertion, ClaimAssertion};

/// One candidate unit: a chunk row or an episode row.
#[derive(Debug, Clone, Copy)]
pub struct Unit<'a> {
    pub id: &'a str,
    pub session: &'a str,
    pub text: &'a str,
}

/// What the partition decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartitionOutcome {
    /// Unit ids that leave every fused channel (state `superseded`).
    pub sunk: HashSet<String>,
    /// One receipt note per unit that carries a superseded, annotate or
    /// proposed assertion.
    pub notes: HashMap<String, String>,
}

/// Kill switch: `CSR_NO_ASSERTIONS=1` (the exact value) skips the whole stage.
pub fn assertions_enabled_from(value: Option<&str>) -> bool {
    value != Some("1")
}

/// Strength of a state; lower is stronger. `None` for a state the table's
/// CHECK constraint forbids, which is ignored.
fn state_strength(state: &str) -> Option<u8> {
    match state {
        "superseded" => Some(0),
        "annotate" => Some(1),
        "proposed" => Some(2),
        _ => None,
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The receipt note of one assertion: `[superseded] <key> (receipt <receipt>)`,
/// `[annotated] ...` or `[proposed] ...`. An empty key falls back to the
/// source name. Whitespace is folded so a note is one line.
pub fn note_for(source: &str, state: &str, key: &str, receipt: &str) -> String {
    let label = match state {
        "superseded" => "superseded",
        "annotate" => "annotated",
        "proposed" => "proposed",
        other => other,
    };
    let key = one_line(key);
    let key = if key.is_empty() {
        one_line(source)
    } else {
        key
    };
    format!("[{label}] {key} (receipt {})", one_line(receipt))
}

/// The frozen claim rule over a text already lowercased once. Mirrors
/// `ClaimAssertion::matches`, which lowercases on every call.
fn claim_matches_lower(claim: &ClaimAssertion, hay: &str) -> bool {
    let has = |p: &String| {
        let p = p.trim().to_lowercase();
        !p.is_empty() && hay.contains(&p)
    };
    claim
        .subject_terms
        .iter()
        .filter(|t| !t.trim().is_empty())
        .all(has)
        && claim.superseded_phrases.iter().any(has)
        && !claim.current_phrases.iter().any(has)
}

/// Decide which units sink and which carry a note. `by_target` is the result
/// of `Storage::assertions_for_targets` for the units' ids and sessions.
pub fn partition(
    units: &[Unit<'_>],
    by_target: &HashMap<String, Assertion>,
    claims: &[ClaimAssertion],
) -> PartitionOutcome {
    let mut out = PartitionOutcome::default();
    for unit in units {
        // (state strength, tier inside the state, note)
        let mut best: Option<(u8, u8, String)> = None;
        let mut offer = |strength: u8, tier: u8, note: String| {
            let better = match &best {
                None => true,
                Some((s, t, _)) => (strength, tier) < (*s, *t),
            };
            if better {
                best = Some((strength, tier, note));
            }
        };
        let from_row = |a: &Assertion| {
            state_strength(&a.state).map(|strength| {
                (
                    strength,
                    note_for(&a.source, &a.state, &a.source, &a.receipt),
                )
            })
        };
        if let Some(a) = by_target.get(unit.id).filter(|a| a.target_kind == "chunk") {
            if let Some((strength, note)) = from_row(a) {
                offer(strength, 0, note);
            }
        }
        if let Some(a) = by_target
            .get(unit.session)
            .filter(|a| a.target_kind == "session")
        {
            if let Some((strength, note)) = from_row(a) {
                offer(strength, 1, note);
            }
        }
        if !claims.is_empty() && !unit.text.is_empty() {
            let hay = unit.text.to_lowercase();
            if let Some(claim) = claims.iter().find(|c| claim_matches_lower(c, &hay)) {
                offer(
                    0,
                    2,
                    note_for("user_decision", "superseded", &claim.key, &claim.receipt),
                );
            }
        }
        if let Some((strength, _, note)) = best {
            if strength == 0 {
                out.sunk.insert(unit.id.to_string());
            }
            out.notes.insert(unit.id.to_string(), note);
        }
    }
    out
}

/// Drop the sunk ids from one ranking, keeping the order of the rest.
pub fn without_sunk(order: &[String], sunk: &HashSet<String>) -> Vec<String> {
    if sunk.is_empty() {
        return order.to_vec();
    }
    order
        .iter()
        .filter(|id| !sunk.contains(*id))
        .cloned()
        .collect()
}

/// Stable partition of one ranking: the rest first, the sunk ids after them,
/// each part in its original order.
pub fn sunk_to_tail(order: Vec<String>, sunk: &HashSet<String>) -> Vec<String> {
    if sunk.is_empty() {
        return order;
    }
    let (kept, tail): (Vec<String>, Vec<String>) =
        order.into_iter().partition(|id| !sunk.contains(id));
    kept.into_iter().chain(tail).collect()
}

/// Append the sunk units to a session-collapsed list. `fused` is the fused
/// order of every unit before the partition. A session already represented in
/// `collapsed` is not shown again; a session made only of sunk units gets one
/// slot, held by its first unit in fused order. The sessions trail in the
/// order their first unit was fused.
pub fn trail_sunk(
    mut collapsed: Vec<String>,
    fused: &[String],
    sunk: &HashSet<String>,
    session_of: impl Fn(&str) -> String,
) -> Vec<String> {
    if sunk.is_empty() {
        return collapsed;
    }
    let mut seen: HashSet<String> = collapsed.iter().map(|id| session_of(id)).collect();
    for id in fused {
        if sunk.contains(id) && seen.insert(session_of(id)) {
            collapsed.push(id.clone());
        }
    }
    collapsed
}

/// Keep the sunk units that were in the pre-partition top `limit` on the page,
/// so the `[superseded] ... (receipt ...)` note they carry is still read.
/// `shown` is the ranking with the sunk units trailing ([`trail_sunk`],
/// [`sunk_to_tail`]); `fused` is the order of every unit before the partition.
///
/// A unit already inside the first `limit` of `shown` is left alone. The
/// others fill the page's last slots, best pre-partition rank first, so the
/// worst of them holds the last shown slot. A unit counts even when a healthy
/// unit of its session is on the page: the note is on the retired row, not on
/// its session. At most `limit / 2` slots (one at least) go to retired units,
/// and the first slot never does: a page of one has no such slot, and a page
/// with no healthy unit is left as it is.
pub fn keep_sunk_in_page(
    shown: Vec<String>,
    fused: &[String],
    sunk: &HashSet<String>,
    limit: usize,
) -> Vec<String> {
    if sunk.is_empty() || limit < 2 {
        return shown;
    }
    let on_page: HashSet<&String> = shown.iter().take(limit).collect();
    let wanted: Vec<String> = fused
        .iter()
        .take(limit)
        .filter(|id| sunk.contains(*id) && !on_page.contains(id))
        .take((limit / 2).max(1))
        .cloned()
        .collect();
    if wanted.is_empty() {
        return shown;
    }
    let mut rest: Vec<String> = shown
        .into_iter()
        .filter(|id| !wanted.contains(id))
        .collect();
    if rest.is_empty() {
        return wanted;
    }
    let first_slot = limit - wanted.len();
    for (i, id) in wanted.into_iter().enumerate() {
        let at = (first_slot + i).min(rest.len()).max(1);
        rest.insert(at, id);
    }
    rest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assertion(
        source: &str,
        kind: &str,
        target: &str,
        state: &str,
        claim: Option<&str>,
    ) -> Assertion {
        Assertion {
            source: source.into(),
            target_kind: kind.into(),
            target_id: target.into(),
            state: state.into(),
            claim: claim.map(str::to_string),
            receipt: format!("rcpt-{target}"),
        }
    }

    fn map(rows: Vec<Assertion>) -> HashMap<String, Assertion> {
        rows.into_iter().map(|a| (a.target_id.clone(), a)).collect()
    }

    fn unit<'a>(id: &'a str, session: &'a str, text: &'a str) -> Unit<'a> {
        Unit { id, session, text }
    }

    fn decision() -> ClaimAssertion {
        ClaimAssertion {
            key: "merge-mode".into(),
            subject_terms: vec!["merge".into(), "admin".into()],
            superseded_phrases: vec!["carrier branch".into()],
            current_phrases: vec!["--admin --squash works".into()],
            receipt: "decided-2026-08-09".into(),
        }
    }

    #[test]
    fn id_target_sinks_only_that_unit() {
        let by_target = map(vec![assertion(
            "csr_resolve",
            "chunk",
            "c1",
            "superseded",
            None,
        )]);
        let units = [unit("c1", "s1", "x"), unit("c2", "s1", "y")];
        let out = partition(&units, &by_target, &[]);
        assert_eq!(out.sunk, HashSet::from(["c1".to_string()]));
        assert_eq!(
            out.notes["c1"],
            "[superseded] csr_resolve (receipt rcpt-c1)"
        );
        assert!(!out.notes.contains_key("c2"));
    }

    #[test]
    fn session_target_sinks_every_unit_of_the_session() {
        let by_target = map(vec![assertion(
            "witness",
            "session",
            "s1",
            "superseded",
            None,
        )]);
        let units = [
            unit("c1", "s1", "x"),
            unit("c2", "s1", "y"),
            unit("c3", "s2", "z"),
        ];
        let out = partition(&units, &by_target, &[]);
        assert_eq!(
            out.sunk,
            HashSet::from(["c1".to_string(), "c2".to_string()])
        );
        assert!(!out.notes.contains_key("c3"));
    }

    #[test]
    fn a_session_row_does_not_apply_to_a_chunk_id_and_back() {
        // The same string as a chunk id and a session key: the row's kind decides.
        let by_target = map(vec![assertion(
            "witness",
            "session",
            "x",
            "superseded",
            None,
        )]);
        let out = partition(&[unit("x", "other", "t")], &by_target, &[]);
        assert!(out.sunk.is_empty());
        let by_target = map(vec![assertion("witness", "chunk", "s", "superseded", None)]);
        let out = partition(&[unit("c", "s", "t")], &by_target, &[]);
        assert!(out.sunk.is_empty());
    }

    #[test]
    fn claim_rule_needs_subject_and_superseded_phrase_and_no_current_phrase() {
        let claims = [decision()];
        let hit = unit(
            "a",
            "s",
            "We must merge via the Carrier Branch because admin is blocked",
        );
        let no_subject = unit("b", "s", "the carrier branch is a git concept");
        let current = unit(
            "c",
            "s",
            "merge with carrier branch? no, --admin --squash works for admin",
        );
        let out = partition(&[hit, no_subject, current], &HashMap::new(), &claims);
        assert_eq!(out.sunk, HashSet::from(["a".to_string()]));
        assert_eq!(
            out.notes["a"],
            "[superseded] merge-mode (receipt decided-2026-08-09)"
        );
        assert!(!out.notes.contains_key("b"));
        assert!(
            !out.notes.contains_key("c"),
            "current phrase blocks the claim"
        );
    }

    #[test]
    fn lowercase_once_rule_agrees_with_the_storage_rule() {
        let claim = decision();
        for text in [
            "merge admin carrier branch",
            "MERGE ADMIN CARRIER BRANCH",
            "merge admin carrier branch --admin --squash works",
            "merge carrier branch",
            "",
        ] {
            assert_eq!(
                claim_matches_lower(&claim, &text.to_lowercase()),
                claim.matches(text),
                "{text}"
            );
        }
    }

    #[test]
    fn superseded_beats_annotate_beats_proposed() {
        let units = [unit("c1", "s1", "t")];
        // Chunk annotate vs session superseded: superseded wins and sinks.
        let by_target = map(vec![
            assertion("csr_resolve", "chunk", "c1", "annotate", None),
            assertion("witness", "session", "s1", "superseded", None),
        ]);
        let out = partition(&units, &by_target, &[]);
        assert!(out.sunk.contains("c1"));
        assert!(out.notes["c1"].starts_with("[superseded] witness"));
        // Chunk proposed vs session annotate: annotate wins, nothing sinks.
        let by_target = map(vec![
            assertion("task", "chunk", "c1", "proposed", None),
            assertion("witness", "session", "s1", "annotate", None),
        ]);
        let out = partition(&units, &by_target, &[]);
        assert!(out.sunk.is_empty());
        assert!(out.notes["c1"].starts_with("[annotated] witness"));
        // Proposed alone: a note, no sink.
        let by_target = map(vec![assertion("task", "chunk", "c1", "proposed", None)]);
        let out = partition(&units, &by_target, &[]);
        assert!(out.sunk.is_empty());
        assert!(out.notes["c1"].starts_with("[proposed] task"));
        // A claim match (superseded) beats a chunk annotate.
        let by_target = map(vec![assertion(
            "csr_resolve",
            "chunk",
            "c1",
            "annotate",
            None,
        )]);
        let matched = [unit("c1", "s1", "merge admin carrier branch")];
        let out = partition(&matched, &by_target, &[decision()]);
        assert!(out.sunk.contains("c1"));
        assert!(out.notes["c1"].starts_with("[superseded] merge-mode"));
        // Inside one state a chunk row beats a session row.
        let by_target = map(vec![
            assertion("csr_resolve", "chunk", "c1", "superseded", None),
            assertion("witness", "session", "s1", "superseded", None),
        ]);
        let out = partition(&units, &by_target, &[]);
        assert!(out.notes["c1"].starts_with("[superseded] csr_resolve"));
    }

    #[test]
    fn notes_never_carry_claim_text() {
        let secret = "USE-THE-ADMIN-FLAG-SECRET-ANSWER";
        let by_target = map(vec![
            assertion("csr_resolve", "chunk", "c1", "superseded", Some(secret)),
            assertion("witness", "chunk", "c2", "annotate", Some(secret)),
            assertion("task", "chunk", "c3", "proposed", Some(secret)),
        ]);
        let units = [
            unit("c1", "s", "t"),
            unit("c2", "s", "t"),
            unit("c3", "s", "t"),
        ];
        let out = partition(&units, &by_target, &[decision()]);
        assert_eq!(out.notes.len(), 3);
        for note in out.notes.values() {
            assert!(!note.contains(secret), "{note}");
            assert!(!note.contains('\n'), "{note}");
        }
        assert_eq!(
            note_for("user_decision", "superseded", "k", "line one\nline two"),
            "[superseded] k (receipt line one line two)"
        );
        assert_eq!(
            note_for("witness", "annotate", "", "r"),
            "[annotated] witness (receipt r)"
        );
    }

    #[test]
    fn unknown_states_are_ignored() {
        let by_target = map(vec![assertion("task", "chunk", "c1", "weird", None)]);
        let out = partition(&[unit("c1", "s", "t")], &by_target, &[]);
        assert_eq!(out, PartitionOutcome::default());
    }

    #[test]
    fn kill_switch_value_is_exactly_one() {
        assert!(!assertions_enabled_from(Some("1")));
        assert!(assertions_enabled_from(None));
        assert!(assertions_enabled_from(Some("0")));
        assert!(assertions_enabled_from(Some("true")));
        assert!(assertions_enabled_from(Some("")));
    }

    #[test]
    fn list_helpers_keep_order() {
        let order: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let sunk = HashSet::from(["b".to_string(), "d".to_string()]);
        assert_eq!(without_sunk(&order, &sunk), vec!["a", "c"]);
        assert_eq!(sunk_to_tail(order.clone(), &sunk), vec!["a", "c", "b", "d"]);
        assert_eq!(sunk_to_tail(order.clone(), &HashSet::new()), order);
    }

    #[test]
    fn trail_sunk_gives_one_slot_per_unrepresented_session() {
        let session_of = |id: &str| id.split('-').next().unwrap().to_string();
        let collapsed: Vec<String> = vec!["a-1".into(), "b-1".into()];
        // Fused order before the partition; a-0 outranked a-1 but is sunk.
        let fused: Vec<String> = ["a-0", "c-0", "b-0", "c-1", "a-1", "d-0", "b-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let sunk: HashSet<String> = ["a-0", "c-0", "b-0", "c-1", "d-0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = trail_sunk(collapsed, &fused, &sunk, session_of);
        // a and b are represented already; c and d trail, c first (fused first).
        assert_eq!(out, vec!["a-1", "b-1", "c-0", "d-0"]);
    }

    // ---- F1b: a sunk unit that was in the top `limit` stays on the page ----

    fn f1b_ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// Pre-partition fused order a b c d e f g; `c` is the sunk user-decision
    /// unit. After the partition the page is a b d e f g with `c` trailed.
    fn f1b_case() -> (Vec<String>, Vec<String>, HashSet<String>, PartitionOutcome) {
        let texts = [
            ("a", "alpha"),
            ("b", "beta"),
            ("c", "no fork PR can merge, not even with --admin"),
            ("d", "delta"),
            ("e", "eps"),
            ("f", "zeta"),
            ("g", "eta"),
        ];
        let units: Vec<Unit<'_>> = texts
            .iter()
            .map(|(id, text)| Unit {
                id,
                session: id,
                text,
            })
            .collect();
        let claims = vec![ClaimAssertion {
            key: "fork-pr-admin-merge".into(),
            subject_terms: vec!["fork".into(), "PR".into()],
            superseded_phrases: vec!["no fork PR can merge".into()],
            current_phrases: vec!["--admin --squash".into()],
            receipt: "memory fork_pr_admin_merge_works.md".into(),
        }];
        let outcome = partition(&units, &HashMap::new(), &claims);
        let fused = f1b_ids(&["a", "b", "c", "d", "e", "f", "g"]);
        let collapsed = f1b_ids(&["a", "b", "d", "e", "f", "g"]);
        let shown = trail_sunk(collapsed, &fused, &outcome.sunk, |id| id.to_string());
        (shown, fused, outcome.sunk.clone(), outcome)
    }

    #[test]
    fn f1b_assert_sunk_unit_in_topk_renders_last_with_note() {
        let (shown, fused, sunk, outcome) = f1b_case();
        // Before: the tail of the trailed list is past a limit-5 page.
        assert_eq!(shown.iter().position(|id| id == "c"), Some(6));
        let page = keep_sunk_in_page(shown, &fused, &sunk, 5);
        assert_eq!(
            page.iter().position(|id| id == "c"),
            Some(4),
            "last shown slot"
        );
        assert_eq!(page[..4], f1b_ids(&["a", "b", "d", "e"])[..]);
        let note = &outcome.notes["c"];
        assert!(
            note.starts_with("[superseded] fork-pr-admin-merge (receipt "),
            "{note}"
        );
        // A unit that was not in the pre-partition top 5 is left at the tail.
        let far = f1b_ids(&["a", "b", "d", "e", "f", "g", "c"]);
        let fused_far = f1b_ids(&["a", "b", "d", "e", "f", "g", "c"]);
        let page = keep_sunk_in_page(far.clone(), &fused_far, &sunk, 5);
        assert_eq!(page, far);
        // Already on the page: untouched.
        let near = f1b_ids(&["a", "c", "b"]);
        let page = keep_sunk_in_page(near.clone(), &fused, &sunk, 5);
        assert_eq!(page, near);
        // A healthy unit of the same session on the page does not hide the
        // retired row: the note is on the row. Two retired units in the top 5
        // fill the last two slots, best pre-partition rank first; the cap for
        // a page of 5 is 2, so the third stays out.
        let sunk3: HashSet<String> = ["c", "d", "e"].iter().map(|s| s.to_string()).collect();
        let shown3 = f1b_ids(&["a", "b", "f", "g", "h", "c", "d", "e"]);
        let page = keep_sunk_in_page(shown3, &fused, &sunk3, 5);
        assert_eq!(page[..5], f1b_ids(&["a", "b", "f", "c", "d"])[..]);
        assert!(!page[..5].contains(&"e".to_string()));
    }

    #[test]
    fn f1b_assert_sunk_unit_never_rank1() {
        let (shown, _, sunk, _) = f1b_case();
        // The sunk unit led the pre-partition order.
        let fused = f1b_ids(&["c", "a", "b", "d", "e", "f", "g"]);
        let page = keep_sunk_in_page(shown.clone(), &fused, &sunk, 5);
        assert_eq!(page.iter().position(|id| id == "c"), Some(4));
        assert_ne!(page[0], "c");
        // A page of one has no slot that is not rank 1: the unit stays out.
        let page = keep_sunk_in_page(shown.clone(), &fused, &sunk, 1);
        assert_eq!(page, shown);
        // So does a page of zero.
        let page = keep_sunk_in_page(shown.clone(), &fused, &sunk, 0);
        assert_eq!(page, shown);
        // Page of two: slot 2 (index 1), still not rank 1.
        let page = keep_sunk_in_page(shown, &fused, &sunk, 2);
        assert_eq!(page.iter().position(|id| id == "c"), Some(1));
        // Nothing healthy to lead the page: left as it is.
        let only = f1b_ids(&["c"]);
        let page = keep_sunk_in_page(only.clone(), &fused, &sunk, 5);
        assert_eq!(page, only);
        // A short list puts the unit after the healthy ones, never first.
        let short = f1b_ids(&["a"]);
        let page = keep_sunk_in_page(short, &f1b_ids(&["a", "c"]), &sunk, 5);
        assert_eq!(page, f1b_ids(&["a", "c"]));
    }
}
