//! Adapts the memory-health measurement into `EvalResult`s for `eval --full`.

use std::time::Instant;

use crate::eval::EvalResult;
use crate::health::{HealthRun, MissCause, ProbeGrade, ProbeReport};

const CATEGORY: &str = "memory-health";

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// `Duplicate Rate (Report Only)`: exact-content duplicates over every chunk.
/// Always non-gating, even when the measurement is unavailable.
pub fn duplicate_rate(run: &Result<HealthRun, String>) -> EvalResult {
    let start = Instant::now();
    let name = "Duplicate Rate (Report Only)";
    let detail = match run {
        Err(error) => format!("REPORT ONLY: unavailable: {error}"),
        Ok(run) => match &run.snapshot.state.duplicates {
            None => "REPORT ONLY: unavailable: no measurement was recorded".to_string(),
            Some(report) => match report.rate {
                None => "REPORT ONLY: no chunks; rate not applicable (0 rows, 0 duplicates)"
                    .to_string(),
                Some(rate) => format!(
                    "REPORT ONLY: {} duplicate rows of {} chunks ({:.2}%), {} distinct; nothing changed",
                    report.duplicate_rows,
                    report.total,
                    rate * 100.0,
                    report.distinct
                ),
            },
        },
    };
    EvalResult::pass(name, CATEGORY, ms(start), detail)
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
/// and amber pass, labeled as such.
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
        ProbeGrade::Abstain => EvalResult::pass(
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
        ProbeGrade::Amber => EvalResult::pass(
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
        assert!(amber.passed && amber.detail.starts_with("AMBER (non-gating):"));
        let abstain = known_item_recall(&run(ProbeGrade::Abstain, 0, 0, None));
        assert!(
            abstain.passed && abstain.detail.starts_with("ABSTAIN:"),
            "{}",
            abstain.detail
        );
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
    fn duplicate_result_never_fails() {
        let with_rate = duplicate_rate(&run(ProbeGrade::Red, 1, 20, Some(0.1)));
        assert!(with_rate.passed);
        assert!(
            with_rate.detail.starts_with("REPORT ONLY:"),
            "{}",
            with_rate.detail
        );
        assert!(with_rate.detail.contains("10.00%"));
        let empty = duplicate_rate(&run(ProbeGrade::Abstain, 0, 0, None));
        assert!(empty.passed && empty.detail.starts_with("REPORT ONLY:"));
        let unavailable = duplicate_rate(&Err("boom".into()));
        assert!(unavailable.passed && unavailable.detail.starts_with("REPORT ONLY: unavailable"));
    }
}
