//! Adapts the memory-health measurement into `EvalResult`s for `eval --full`.

use std::time::Instant;

use crate::eval::EvalResult;
use crate::health::page_repeats::PageRepeats;
use crate::health::{HealthRun, MissCause, ProbeGrade, ProbeReport};

const CATEGORY: &str = "memory-health";

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// `Duplicate Rate (Report Only)`, two figures on one line: identical chunk
/// rows in storage (byte-equal content over the whole `chunks` table), and
/// repeated text on result pages (rows of the probe's pages whose normalized
/// text an earlier row of the same page holds). Always non-gating, even when a
/// measurement is unavailable.
pub fn duplicate_rate(run: &Result<HealthRun, String>, pages: &PageRepeats) -> EvalResult {
    let start = Instant::now();
    let name = "Duplicate Rate (Report Only)";
    let stored = match run {
        Err(error) => format!("identical chunk rows in storage unavailable: {error}"),
        Ok(run) => match &run.snapshot.state.duplicates {
            None => "identical chunk rows in storage unavailable: no measurement was recorded"
                .to_string(),
            Some(report) => match report.rate {
                None => {
                    "no chunks in storage; rate not applicable (0 rows, 0 duplicates)".to_string()
                }
                Some(rate) => format!(
                    "identical chunk rows in storage: {} of {} ({:.2}%), {} distinct",
                    report.duplicate_rows,
                    report.total,
                    rate * 100.0,
                    report.distinct
                ),
            },
        },
    };
    let on_pages = match pages.rate() {
        None => "repeated text on result pages: no probe page was read".to_string(),
        Some(rate) => format!(
            "repeated text on result pages after the same-text collapse: {} of {} rows ({:.2}%) over {} probe queries, {} copies left out by the collapse",
            pages.repeats,
            pages.rows,
            rate * 100.0,
            pages.pages,
            pages.collapsed
        ),
    };
    EvalResult::pass(
        name,
        CATEGORY,
        ms(start),
        format!("REPORT ONLY: {stored}; {on_pages}; nothing changed"),
    )
}

fn cause_counts(report: &ProbeReport) -> String {
    let count = |cause: MissCause| report.misses.iter().filter(|m| m.cause == cause).count();
    format!(
        "embedding_missing={} index_missing={} not_in_top5={}",
        count(MissCause::EmbeddingMissing),
        count(MissCause::IndexMissing),
        count(MissCause::NotInTop5)
    )
}

/// `Known-Item Recall`. Red and an execution error fail the gate; abstain
/// prints `[ABSTAIN]` and amber prints `[AMBER]`, neither failing the run.
pub fn known_item_recall(run: &Result<HealthRun, String>) -> EvalResult {
    let start = Instant::now();
    let name = "Known-Item Recall";
    let run = match run {
        Ok(run) => run,
        Err(error) => {
            return EvalResult::fail(name, CATEGORY, ms(start), format!("ERROR: {error}"));
        }
    };
    let report = &run.probe;
    let sample = report.sample_id.as_deref().unwrap_or("none");
    let note = run
        .note
        .as_deref()
        .map(|note| format!("; note: {note}"))
        .unwrap_or_default();
    let invalidated = if report.invalidated > 0 {
        format!(", {} invalidated", report.invalidated)
    } else {
        String::new()
    };
    match report.grade {
        ProbeGrade::Abstain => EvalResult::abstain(
            name,
            CATEGORY,
            ms(start),
            format!(
                "ABSTAIN: {} eligible cases (needs {}); no recall claim made{invalidated}{note}",
                report.n,
                crate::health::probe::SAMPLE_MIN
            ),
        ),
        ProbeGrade::Green => EvalResult::pass(
            name,
            CATEGORY,
            ms(start),
            format!(
                "GREEN: {}/{} found in the top {} (sample {sample}{invalidated}){note}",
                report.hits,
                report.n,
                crate::health::probe::TOP_K
            ),
        ),
        ProbeGrade::Amber => EvalResult::amber(
            name,
            CATEGORY,
            ms(start),
            format!(
                "AMBER (non-gating): {}/{} found in the top {} (sample {sample}{invalidated}); {}{note}",
                report.hits,
                report.n,
                crate::health::probe::TOP_K,
                cause_counts(report)
            ),
        ),
        ProbeGrade::Red => EvalResult::fail(
            name,
            CATEGORY,
            ms(start),
            format!(
                "RED: {}/{} found in the top {} (sample {sample}{invalidated}); {}{note}",
                report.hits,
                report.n,
                crate::health::probe::TOP_K,
                cause_counts(report)
            ),
        ),
        ProbeGrade::Error => EvalResult::fail(
            name,
            CATEGORY,
            ms(start),
            format!(
                "ERROR: {} (denominator {}, sample {sample}){note}",
                report.error.as_deref().unwrap_or("probe failed"),
                report.n
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{DuplicateReport, HealthSnapshot, ProbeMiss};

    fn run(
        grade: ProbeGrade,
        hits: usize,
        n: usize,
        rate: Option<f64>,
    ) -> Result<HealthRun, String> {
        let mut snapshot = HealthSnapshot::default();
        snapshot.state.duplicates = Some(DuplicateReport {
            at: "t".into(),
            total: if rate.is_some() { 100 } else { 0 },
            distinct: if rate.is_some() { 90 } else { 0 },
            duplicate_rows: if rate.is_some() { 10 } else { 0 },
            rate,
        });
        Ok(HealthRun {
            snapshot,
            probe: ProbeReport {
                at: "t".into(),
                sample_id: Some("ps-1".into()),
                hits,
                n,
                invalidated: 0,
                grade,
                misses: vec![ProbeMiss {
                    chunk_id: "c".into(),
                    conversation_id: "v".into(),
                    returned: vec![],
                    cause: MissCause::IndexMissing,
                }],
                error: (grade == ProbeGrade::Error).then(|| "embedding unavailable".to_string()),
            },
            note: None,
        })
    }

    #[test]
    fn grades_map_to_gating_results() {
        let green = known_item_recall(&run(ProbeGrade::Green, 18, 20, None));
        assert!(green.passed && green.detail.starts_with("GREEN:"));
        let amber = known_item_recall(&run(ProbeGrade::Amber, 15, 20, None));
        assert!(amber.passed && amber.amber && amber.detail.starts_with("AMBER (non-gating):"));
        let abstain = known_item_recall(&run(ProbeGrade::Abstain, 0, 0, None));
        assert!(
            abstain.passed && abstain.abstain && abstain.detail.starts_with("ABSTAIN:"),
            "{}",
            abstain.detail
        );
        let line = abstain.format_line();
        assert!(line.starts_with("  [ABSTAIN] Known-Item Recall"), "{line}");
        assert!(!line.contains("[PASS]"), "{line}");
        let red = known_item_recall(&run(ProbeGrade::Red, 5, 20, None));
        assert!(
            !red.passed && red.detail.starts_with("RED: 5/20"),
            "{}",
            red.detail
        );
        assert!(red.detail.contains("index_missing=1"));
        let error = known_item_recall(&run(ProbeGrade::Error, 2, 20, None));
        assert!(!error.passed && error.detail.contains("denominator 20"));
        let unavailable = known_item_recall(&Err("db locked".into()));
        assert!(!unavailable.passed);
    }

    #[test]
    fn amber_recall_prints_amber_and_counts_apart_from_pass() {
        let amber = known_item_recall(&run(ProbeGrade::Amber, 15, 20, None));
        let line = amber.format_line();
        assert!(line.starts_with("  [AMBER] Known-Item Recall"), "{line}");
        assert!(!line.contains("[PASS]"), "{line}");

        let report = crate::eval::EvalReport {
            results: vec![
                known_item_recall(&run(ProbeGrade::Green, 18, 20, None)),
                amber,
                known_item_recall(&run(ProbeGrade::Red, 5, 20, None)),
            ],
            total_ms: 7.0,
        };
        let text = report.format_text();
        assert!(
            text.contains(
                "Result: 1 pass, 1 amber, 0 abstain, 0 no verdict, 1 fail (3 results) in 7ms"
            ),
            "{text}"
        );
    }

    #[test]
    fn duplicate_result_never_fails() {
        let none = PageRepeats::default();
        let with_rate = duplicate_rate(&run(ProbeGrade::Red, 1, 20, Some(0.1)), &none);
        assert!(with_rate.passed);
        assert!(
            with_rate.detail.starts_with("REPORT ONLY:"),
            "{}",
            with_rate.detail
        );
        assert!(with_rate.detail.contains("10.00%"));
        let empty = duplicate_rate(&run(ProbeGrade::Abstain, 0, 0, None), &none);
        assert!(empty.passed && empty.detail.starts_with("REPORT ONLY:"));
        let unavailable = duplicate_rate(&Err("boom".into()), &none);
        assert!(
            unavailable.passed
                && unavailable
                    .detail
                    .starts_with("REPORT ONLY: identical chunk rows in storage unavailable"),
            "{}",
            unavailable.detail
        );
    }

    /// The storage figure and the page figure are labelled apart: 10% of
    /// stored rows are byte-identical copies, while 1 of 20 rows the probe's
    /// pages showed repeats an earlier row of its page, and the collapse took
    /// 7 copies out of the pools those pages were cut from.
    #[test]
    fn the_duplicate_line_labels_storage_rows_and_page_rows_apart() {
        let pages = PageRepeats {
            pages: 4,
            rows: 20,
            repeats: 1,
            collapsed: 7,
        };
        let line = duplicate_rate(&run(ProbeGrade::Green, 18, 20, Some(0.1)), &pages).detail;
        assert_eq!(
            line,
            "REPORT ONLY: identical chunk rows in storage: 10 of 100 (10.00%), 90 distinct; \
             repeated text on result pages after the same-text collapse: 1 of 20 rows (5.00%) \
             over 4 probe queries, 7 copies left out by the collapse; nothing changed"
        );
        let unread = duplicate_rate(
            &run(ProbeGrade::Abstain, 0, 0, Some(0.1)),
            &PageRepeats::default(),
        );
        assert!(
            unread
                .detail
                .contains("repeated text on result pages: no probe page was read"),
            "{}",
            unread.detail
        );
    }
}
