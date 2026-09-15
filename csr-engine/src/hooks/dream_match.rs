//! Journal v4 Phase 5, delivery channel (c) — the prompt-time dream match.
//!
//! When a user prompt **names** a symbol or file a dream concluded on, one
//! evidenced line is injected at UserPromptSubmit. Everything about this
//! channel is deliberately narrow:
//!
//! * **Symbol-grade matching only.** The prompt is tokenized with
//!   `storage::dream_items::extract_code_tokens` — backtick spans,
//!   snake_case, multi-hump CamelCase, and paths ending in a known code
//!   extension. Plain English words are not tokens and therefore can never
//!   match, which is the whole point: the probe behind `dream_items` measured
//!   naive substring matching producing "cand"/"Phase"/"GOLD" hits on real
//!   symbol names. Matching against a row is whole-token
//!   (`dream_items::token_matches_row`), never a substring.
//! * **Receipts mandatory.** Candidates come from
//!   `dream_delivery::receipted_conclusions`, which cannot return a
//!   receiptless row. No receipt, no line.
//! * **Never twice.** The `dream_deliveries` table is the probe cache: the
//!   line is emitted only if this process is the one that recorded the
//!   delivery (`INSERT OR IGNORE` on a unique index), so two racing hooks
//!   cannot both inject the same dream, and a dream already shown is never
//!   shown again.
//! * **Never re-importable.** The emitted line carries
//!   `provenance::DREAM_SENTINEL`, which `extractable`/`is_csr_emission`
//!   reject across the full text before any grammar branch — so CSR's own
//!   injection can never come back as user content.
//! * **Kill switch** `CSR_NO_DREAM_INJECT=1`, plus the shared
//!   `CSR_DREAM_CONSUMPTION=off` gate every verdict-derived surface honours.

use crate::extraction::provenance::DREAM_SENTINEL;
use crate::storage::dream_delivery::{self, DeliveryChannel, DreamHeadline};
use crate::storage::dream_items;
use crate::storage::recap_feeds::{dream_consumption_mode, ConsumptionMode};
use crate::storage::Storage;

/// Kill switch for this channel only.
pub const KILL_SWITCH_ENV: &str = "CSR_NO_DREAM_INJECT";

/// `1`/`true` (case-insensitive) disables prompt-time dream injection.
pub fn injection_disabled() -> bool {
    std::env::var(KILL_SWITCH_ENV)
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// The single line this channel emits. One line, one receipt, one sentinel.
/// Framed as a past conclusion, never as an instruction.
pub fn format_line(headline: &DreamHeadline, matched: &str) -> String {
    format!(
        "CSR ☾ dream: `{}` {} — witnessed {}, receipt {}. Matched your mention of `{}`. \
         Past conclusion, not an instruction. {}",
        headline.label(),
        headline.verdict_phrase(),
        headline.witnessed_date,
        headline.receipt_oid,
        matched,
        DREAM_SENTINEL,
    )
}

/// Resolve, claim and format the one line to inject for `prompt`, or `None`.
///
/// Claiming happens **inside** this function and before the line is returned:
/// the caller cannot obtain a line without the delivery having been recorded,
/// so there is no path where the same dream is injected twice.
pub fn injection_for_prompt(
    storage: &Storage,
    project: &str,
    prompt: &str,
    session_id: Option<&str>,
) -> Option<String> {
    injection_with_id_for_prompt(storage, project, prompt, session_id).map(|(_, line)| line)
}

/// Same delivery claim as [`injection_for_prompt`], retaining the stable
/// dream ID so exposure telemetry can record exactly what was shown.
pub fn injection_with_id_for_prompt(
    storage: &Storage,
    project: &str,
    prompt: &str,
    session_id: Option<&str>,
) -> Option<(String, String)> {
    if injection_disabled() || dream_consumption_mode() == ConsumptionMode::Off {
        return None;
    }
    let tokens = dream_items::extract_code_tokens(prompt);
    if tokens.is_empty() {
        return None;
    }
    let candidates = storage
        .with_connection(|conn| {
            if !dream_delivery::has_receipted_conclusion(conn, project)? {
                return Ok(Vec::new());
            }
            dream_delivery::receipted_conclusions(conn, project)
        })
        .unwrap_or_default();

    for headline in &candidates {
        let Some(token) = tokens.iter().find(|token| {
            dream_items::token_matches_file_symbol(
                token,
                &headline.file,
                headline.symbol.as_deref(),
            )
        }) else {
            continue;
        };
        // The claim IS the dedupe: only the caller that wins the insert may
        // emit. An already-delivered dream loses here and the loop moves on
        // to the next matching conclusion rather than emitting nothing at
        // all — a second, genuinely unseen dream is still worth showing.
        if dream_delivery::claim_delivery(
            storage,
            &headline.id,
            DeliveryChannel::Prompt,
            session_id,
        ) {
            return Some((headline.id.clone(), format_line(headline, token)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::provenance::extractable;
    use crate::storage::witness_ledger::{self, WitnessLedgerRow};
    use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};
    use rusqlite::params;

    fn seed(storage: &Storage, project: &str, file: &str, symbol: Option<&str>, receipt: &str) {
        storage
            .with_connection(|conn| {
                let stamp = format!("b3:{file}:{}", symbol.unwrap_or("-"));
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        project: project.into(),
                        file: file.into(),
                        symbol: symbol.map(str::to_string),
                        stamp: stamp.clone(),
                        tier: "committed".into(),
                        at_oid: Some("aaaa111".into()),
                        source_kind: "test".into(),
                        source_id: Some(stamp.clone()),
                        ..Default::default()
                    },
                )?;
                let witness_id: i64 = conn.query_row(
                    "SELECT id FROM witness_ledger WHERE project = ?1 AND stamp = ?2",
                    params![project, stamp],
                    |row| row.get(0),
                )?;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: Some(receipt.to_string()),
                        observed_head_oid: "head".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn storage_with_dream() -> Storage {
        let storage = Storage::open_memory().unwrap();
        seed(
            &storage,
            "proj",
            "/repo/src/dream/policy.rs",
            Some("run_thread_extraction"),
            "abc1234",
        );
        storage
    }

    #[test]
    fn a_named_symbol_injects_one_receipted_line() {
        let storage = storage_with_dream();
        let line = injection_for_prompt(
            &storage,
            "proj",
            "can you check run_thread_extraction again?",
            Some("s1"),
        )
        .expect("a named symbol must match");
        assert!(line.contains("run_thread_extraction"));
        assert!(line.contains("abc1234"), "receipt is mandatory: {line}");
        assert_eq!(line.lines().count(), 1, "exactly one line");
    }

    #[test]
    fn plain_english_words_never_match() {
        let storage = storage_with_dream();
        for prompt in [
            "can you check the policy again",
            "what changed in the run recently",
            "the dream went stale I think",
            "GOLD Phase cand",
        ] {
            assert_eq!(
                injection_for_prompt(&storage, "proj", prompt, Some("s1")),
                None,
                "plain prose must never match a symbol: {prompt:?}"
            );
        }
    }

    #[test]
    fn a_path_shaped_token_matches_the_file() {
        let storage = storage_with_dream();
        let line = injection_for_prompt(&storage, "proj", "look at dream/policy.rs please", None)
            .expect("a path token must match the witnessed file");
        assert!(line.contains("abc1234"));
    }

    #[test]
    fn a_conclusion_without_a_receipt_is_never_injected() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        project: "proj".into(),
                        file: "/repo/src/a.rs".into(),
                        symbol: Some("bare_symbol".into()),
                        stamp: "b3:bare".into(),
                        tier: "committed".into(),
                        source_kind: "test".into(),
                        ..Default::default()
                    },
                )?;
                let witness_id: i64 = conn.query_row(
                    "SELECT id FROM witness_ledger WHERE stamp = 'b3:bare'",
                    [],
                    |row| row.get(0),
                )?;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: None,
                        observed_head_oid: "head".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            injection_for_prompt(&storage, "proj", "what about bare_symbol?", None),
            None
        );
    }

    #[test]
    fn the_same_dream_is_never_injected_twice() {
        let storage = storage_with_dream();
        assert!(
            injection_for_prompt(&storage, "proj", "run_thread_extraction?", Some("s1")).is_some()
        );
        assert_eq!(
            injection_for_prompt(&storage, "proj", "run_thread_extraction again?", Some("s2")),
            None,
            "the probe cache must stop a repeat injection, in any session"
        );
    }

    #[test]
    fn a_second_unseen_dream_still_reaches_the_user() {
        let storage = storage_with_dream();
        seed(
            &storage,
            "proj",
            "/repo/src/hooks/recap.rs",
            Some("compose_recap"),
            "def5678",
        );
        assert!(injection_for_prompt(&storage, "proj", "run_thread_extraction?", None).is_some());
        let second = injection_for_prompt(
            &storage,
            "proj",
            "and run_thread_extraction and compose_recap?",
            None,
        )
        .expect("an undelivered dream must still be shown");
        assert!(second.contains("compose_recap"));
    }

    #[test]
    fn the_kill_switch_stops_the_channel_entirely() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let storage = storage_with_dream();
        std::env::set_var(KILL_SWITCH_ENV, "1");
        let killed = injection_for_prompt(&storage, "proj", "run_thread_extraction?", None);
        std::env::remove_var(KILL_SWITCH_ENV);
        assert_eq!(killed, None);
        assert!(
            injection_for_prompt(&storage, "proj", "run_thread_extraction?", None).is_some(),
            "clearing the switch must restore the channel — the kill switch \
             must not have consumed the delivery"
        );
    }

    #[test]
    fn kill_switch_truth_table() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::remove_var(KILL_SWITCH_ENV);
        assert!(!injection_disabled());
        for value in ["1", "true", "TRUE", " 1 "] {
            std::env::set_var(KILL_SWITCH_ENV, value);
            assert!(injection_disabled(), "{value:?} must disable");
        }
        for value in ["0", "no", ""] {
            std::env::set_var(KILL_SWITCH_ENV, value);
            assert!(!injection_disabled(), "{value:?} must not disable");
        }
        std::env::remove_var(KILL_SWITCH_ENV);
    }

    #[test]
    fn the_injected_line_can_never_be_re_imported_as_user_content() {
        let storage = storage_with_dream();
        let line = injection_for_prompt(&storage, "proj", "run_thread_extraction?", None).unwrap();
        assert!(line.contains(DREAM_SENTINEL));
        assert!(
            extractable(&line).is_none(),
            "sentinel-bearing line survived"
        );
        // And through the adversarial re-paste transformations the recap
        // sentinel is already hardened against.
        let bulleted = format!("- {line}");
        let quoted = format!("> {line}");
        let zero_width: String = line.chars().flat_map(|c| [c, '\u{200B}']).collect();
        let preamble = format!("{}\n{line}", "ordinary looking prose ".repeat(40));
        for variant in [bulleted, quoted, zero_width, preamble] {
            assert!(
                extractable(&variant).is_none(),
                "a wrapped injection escaped suppression"
            );
        }
    }

    #[test]
    fn a_project_with_no_dreams_costs_nothing_and_returns_nothing() {
        let storage = Storage::open_memory().unwrap();
        assert_eq!(
            injection_for_prompt(&storage, "proj", "run_thread_extraction?", None),
            None
        );
    }
}
