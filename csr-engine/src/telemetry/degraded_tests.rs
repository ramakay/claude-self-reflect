use super::*;
use crate::telemetry::prompt_delivery::{report_at, ProjectDelivery};
use std::collections::BTreeMap;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// A report whose 72 h window holds `n` prompt ids, `timed_out` of them timed out.
fn delivery(n: u64, timed_out: u64, installed_hours_ago: Option<i64>) -> PromptDelivery {
    let counts = ProjectDelivery {
        n,
        timed_out,
        delivered: n - timed_out,
        ..Default::default()
    };
    PromptDelivery {
        installed_at: installed_hours_ago.map(|h| now() - Duration::hours(h)),
        last_72h: BTreeMap::from([("project".to_string(), counts)]),
        ..Default::default()
    }
}

fn is_degraded(n: u64, timed_out: u64) -> bool {
    !degraded_signals(&delivery(n, timed_out, Some(2)), &[], now()).is_empty()
}

#[test]
fn four_timeouts_in_eleven_prompts_is_degraded_and_says_so() {
    let signals = degraded_signals(&delivery(11, 4, Some(5)), &[], now());
    assert_eq!(signals.len(), 1);
    assert_eq!(
        signals[0].detail(),
        "prompt_delivery: 4 of 11 prompts since install timed out"
    );
    assert_eq!(signals[0].short(), "prompt delivery 4/11 timed out");
}

#[test]
fn the_rule_boundaries_are_ten_prompts_and_ten_percent() {
    assert!(is_degraded(10, 1), "1 of 10 is exactly 10%");
    assert!(!is_degraded(9, 4), "9 prompts is too few to say");
    assert!(!is_degraded(50, 0), "no timeouts");
    assert!(!is_degraded(100, 9), "9% is below the line");
    assert!(is_degraded(100, 10));
}

#[test]
fn the_window_is_named_by_how_old_the_install_is() {
    let old = degraded_signals(&delivery(20, 5, Some(100)), &[], now());
    assert_eq!(
        old[0].detail(),
        "prompt_delivery: 5 of 20 prompts in the last 72h timed out"
    );
    let unknown = degraded_signals(&delivery(20, 5, None), &[], now());
    assert!(unknown[0].detail().contains("in the last 72h"));
}

#[test]
fn counts_add_up_across_projects() {
    let mut report = delivery(6, 1, Some(2));
    report.last_72h.insert(
        "other".into(),
        ProjectDelivery {
            n: 6,
            timed_out: 1,
            delivered: 5,
            ..Default::default()
        },
    );
    let signals = degraded_signals(&report, &[], now());
    assert_eq!(signals[0].short(), "prompt delivery 2/12 timed out");
}

#[test]
fn missing_hooks_alone_are_degraded() {
    let missing = vec!["PostCompact".to_string(), "Stop".to_string()];
    let signals = degraded_signals(&PromptDelivery::default(), &missing, now());
    assert_eq!(signals, vec![Degraded::HooksMissing(missing)]);
    assert!(signals[0]
        .detail()
        .starts_with("hooks_missing: PostCompact, Stop"));
    assert_eq!(signals[0].short(), "hooks missing PostCompact, Stop");
}

#[test]
fn a_clean_report_has_no_signals_and_keeps_the_health_label() {
    let signals = degraded_signals(&PromptDelivery::default(), &[], now());
    assert!(signals.is_empty());
    assert_eq!(health_label("ok", &signals), "ok");
}

#[test]
fn the_health_label_marks_the_verdict_as_the_database_and_lists_the_signals() {
    let signals = degraded_signals(&delivery(11, 4, Some(5)), &["Stop".to_string()], now());
    assert_eq!(
        health_label("ok", &signals),
        "degraded · database ok · prompt delivery 4/11 timed out; hooks missing Stop"
    );
}

#[test]
fn status_json_gets_degraded_and_healthy_keeps_its_value() {
    let mut report = serde_json::json!({"healthy": true});
    let signals = degraded_signals(&delivery(11, 4, Some(5)), &[], now());
    add_degraded(&mut report, &signals);
    assert_eq!(report["healthy"], serde_json::json!(true));
    assert_eq!(report["state"], "degraded", "healthy db, degraded install");
    assert_eq!(
        report["degraded"],
        serde_json::json!(["prompt_delivery: 4 of 11 prompts since install timed out"])
    );
    add_degraded(&mut report, &[]);
    assert_eq!(report["degraded"], serde_json::json!([]));
}

#[test]
fn the_rule_reads_the_log_through_the_real_report() {
    // 11 prompt ids in the window; 4 carry `transport=unavailable`.
    let log: String = (0..11)
        .map(|id| {
            let transport = if id < 4 { "unavailable" } else { "socket injected=20" };
            format!(
                "2026-11-16T11:00:00Z CSR hook prompt-submit [project]: prompt_id=real-{id} transport={transport}\n"
            )
        })
        .collect();
    let report = report_at(&log, now());
    let signals = degraded_signals(&report, &[], now());
    assert_eq!(signals[0].short(), "prompt delivery 4/11 timed out");
}

#[test]
fn the_stale_line_counts_processes_and_names_the_restart() {
    assert_eq!(stale_line(&[]), None);
    let process = |pid, role: &str| StaleProcess {
        pid,
        role: role.into(),
        started_at: "unknown".into(),
        parent: None,
    };
    let stale = [
        process(1, "mcp_stdio"),
        process(2, crate::stale_report::COMPANION_PANE_ROLE),
        process(3, crate::stale_report::COMPANION_SERVE_ROLE),
    ];
    let line = stale_line(&stale).unwrap();
    assert!(
        line.contains("3 processes on an older binary (2 companion pane/relay)"),
        "{line}"
    );
    assert!(line.contains("hook install --apply"), "{line}");
    assert!(stale_line(&stale[..1]).unwrap().contains("1 process on"));
}

/// The 2026-11-17 footprint case: the database is fine, the daemon started three times in 91
/// seconds, the Codex restamp gave up, and one transcript was never imported.
fn footprint_status() -> serde_json::Value {
    serde_json::json!({
        "healthy": true,
        "daemon": {"up": true, "history": {"starts_last_10m": 3, "starts_last_24h": 4}},
        "aux": {"coverage": {"gap": 388}, "unimported_transcripts": 1},
        "codex_backfill": {"restamp": {"done": false, "attempts": 10, "attempts_exhausted": true}},
    })
}

#[test]
fn restarts_restamp_and_unimported_transcripts_reach_degraded() {
    let signals = status_signals(&footprint_status());
    assert_eq!(
        signals,
        vec![
            Degraded::DaemonRestarts {
                last_10m: 3,
                last_24h: 4
            },
            Degraded::UnimportedTranscripts(1),
            Degraded::RestampExhausted { attempts: 10 },
        ]
    );
    let mut report = footprint_status();
    add_degraded(&mut report, &signals);
    assert_eq!(report["state"], "degraded");
    assert_eq!(report["healthy"], true);
    assert_eq!(report["degraded"].as_array().unwrap().len(), 3);
    assert!(report["degraded"][0]
        .as_str()
        .unwrap()
        .starts_with("daemon_restarts: 3 daemon starts in the last 10 minutes"));
}

#[test]
fn quiet_history_and_a_gap_of_deleted_transcripts_raise_nothing() {
    let report = serde_json::json!({
        "healthy": true,
        "daemon": {"history": {"starts_last_10m": 2, "starts_last_24h": 12}},
        "aux": {"coverage": {"gap": 388}, "unimported_transcripts": 0},
        "codex_backfill": {"restamp": {"done": true, "attempts": 3, "attempts_exhausted": false}},
    });
    assert_eq!(status_signals(&report), vec![]);
    // more than 12 starts in a day is degraded even when spread out
    let busy =
        serde_json::json!({"daemon": {"history": {"starts_last_10m": 1, "starts_last_24h": 13}}});
    assert_eq!(status_signals(&busy).len(), 1);
    // no history file yet (null) raises nothing
    let unknown = serde_json::json!({"daemon": {"history": null}});
    assert!(status_signals(&unknown).is_empty());
}

#[test]
fn state_is_one_word_and_never_ok_while_anything_is_wrong() {
    let one = [Degraded::UnimportedTranscripts(1)];
    assert_eq!(state(Some(true), &[]), "ok");
    assert_eq!(state(Some(true), &one), "degraded");
    assert_eq!(state(Some(false), &one), "unhealthy");
    assert_eq!(state(Some(false), &[]), "unhealthy");
    assert_eq!(state(None, &[]), "unknown");
    assert_eq!(state(None, &one), "degraded");
}

#[test]
fn swiftbar_puts_the_state_first_and_lists_each_signal() {
    let lines = swiftbar_lines(Some(true), &status_signals(&footprint_status()));
    assert_eq!(lines[0], "State: degraded | color=orange");
    assert!(
        lines[1].starts_with("--daemon restarted 3x/10m"),
        "{lines:?}"
    );
    assert_eq!(lines.last().unwrap(), "DB: healthy ✓ | color=green");
    assert_eq!(
        swiftbar_lines(Some(true), &[])[0],
        "State: ok | color=green"
    );
}

/// Studio round 3 (2026-11-19): setup over a running daemon left the plist and no daemon, and
/// status read `healthy: true, state: ok, degraded: []` with `daemon.up: false`.
#[test]
fn an_installed_agent_with_no_daemon_is_degraded_unless_opted_out() {
    let mut report = serde_json::json!({
        "healthy": true,
        "daemon": {"up": false, "agent_installed": true, "pid": null, "history": null},
    });
    let signals = status_signals_with(&report, false);
    assert_eq!(signals, vec![Degraded::DaemonDown]);
    add_degraded(&mut report, &signals);
    assert_eq!(report["state"], "degraded");
    // `healthy` stays the database verdict.
    assert_eq!(report["healthy"], true);
    assert!(report["degraded"][0]
        .as_str()
        .unwrap()
        .starts_with("daemon_down: the launch agent is installed but no daemon is running"));
    assert_eq!(
        swiftbar_lines(Some(true), &signals)[..2],
        [
            "State: degraded | color=orange",
            "--daemon down | font=Menlo"
        ]
    );
    // CSR_NO_DAEMON, a running daemon, or no agent at all raise nothing.
    let quiet = |daemon: serde_json::Value, opted_out: bool| {
        status_signals_with(&serde_json::json!({ "daemon": daemon }), opted_out).is_empty()
    };
    assert!(quiet(
        serde_json::json!({"up": false, "agent_installed": true}),
        true
    ));
    assert!(quiet(
        serde_json::json!({"up": true, "agent_installed": true}),
        false
    ));
    assert!(quiet(
        serde_json::json!({"up": false, "agent_installed": false}),
        false
    ));
    assert!(quiet(serde_json::json!({"up": false}), false));
}
