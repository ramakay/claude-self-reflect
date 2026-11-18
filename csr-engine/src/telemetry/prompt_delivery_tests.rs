use super::*;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-09T23:40:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn row(id: usize, fields: &str) -> String {
    format!("2026-11-09T23:00:00Z CSR hook prompt-submit [project]: prompt_id=real-{id} {fields}")
}

#[test]
fn no_decision_is_unanswered_for_socket_and_ok_including_legacy_rows() {
    for fields in [
        "transport=socket injected=0",
        "transport=ok injected=0",
        "transport=socket",
    ] {
        let report = report_at(&row(0, fields), now());
        let p = &report.last_72h["project"];
        assert_eq!((p.n, p.answered, p.delivered, p.undecided), (1, 0, 0, 1));
        assert_eq!((p.empty, p.share, p.answered_share), (0, 0.0, 0.0));
        let gate = eval_result_for(&report);
        assert!(!gate.passed && gate.no_verdict);
        assert!(gate.format_line().contains("[NO VERDICT]"));
        assert!(gate.detail.contains("undecided=1 fallback=0 timed-out=0"));
    }
}

#[test]
fn undecided_rows_cannot_pass_the_gate_at_thirty_runs() {
    let log = (0..30)
        .map(|id| row(id, "transport=socket injected=0"))
        .collect::<Vec<_>>()
        .join("\n");
    let gate = eval_result_for(&report_at(&log, now()));
    assert!(!gate.passed && !gate.no_verdict);
    assert!(gate.detail.contains("project 0.0% (0/30)"));
}

#[test]
fn worst_outcome_includes_undecided_between_abstention_and_fallback() {
    let fields = [
        "transport=socket injected=20",
        "transport=socket injected=0 decided=abstain reason=below_floor",
        "transport=ok injected=0",
        "transport=fallback injected=20",
        "transport=unavailable watchdog=1",
    ];
    for (i, better) in fields.iter().enumerate() {
        for (j, worse) in fields.iter().enumerate().skip(i) {
            for log in [
                format!("{}\n{}", row(0, better), row(0, worse)),
                format!("{}\n{}", row(0, worse), row(0, better)),
            ] {
                let report = report_at(&log, now());
                let p = &report.last_72h["project"];
                assert_eq!(p.n, 1);
                assert_eq!(p.answered, u64::from(j < 2));
                assert_eq!(p.delivered, u64::from(j == 0));
                assert_eq!(p.empty, u64::from(j == 1));
                assert_eq!(p.undecided, u64::from(j == 2));
                assert_eq!(p.fallback, u64::from(j == 3));
                assert_eq!(p.timed_out, u64::from(j == 4));
            }
        }
    }
}

#[test]
fn older_serialized_counts_default_undecided_to_zero() {
    let mut value = serde_json::to_value(ProjectDelivery::default()).unwrap();
    value.as_object_mut().unwrap().remove("undecided");
    let counts: ProjectDelivery = serde_json::from_value(value).unwrap();
    assert_eq!(counts.undecided, 0);
}
