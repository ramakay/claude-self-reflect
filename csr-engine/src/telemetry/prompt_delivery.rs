//! Delivered share of the prompt-submit hook, per project, from hook-timing.log.
//!
//! One run is one `CSR hook prompt-submit [<project>]: ... transport=<t>
//! prompt_id=<id>` line:
//!
//! - delivered: a socket block;
//! - empty: a socket abstention;
//! - undecided: a socket response with neither a decision nor delivered text;
//! - fallback: an in-process answer, excluded from answered;
//! - timed out: `transport=unavailable` (nothing reached stdout).
//!
//! Whether an answer carried text comes from the line's own `injected=<bytes>`
//! when present. Older lines lack it; for those the daemon's
//! `CSR prompt-submit inject:` line directly above the hook line marks a
//! delivered answer; its absence without a decision is undecided (socket only).
//!
//! A process the 500 ms watchdog kills writes its own `transport=unavailable`
//! row (`watchdog=1`) when the hook has not yet written its line.
//!
//! Left out: synthetic ids (`probe*`, `replay-*`, `perf-probe*`), lines whose
//! transport is unknown, and rows without ids. Each id counts once at its
//! worst outcome within the window. `share` is delivered / n; `answered_share`
//! includes abstentions. The live windows start at the running binary's install
//! time; earlier rows are reported separately as history.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::eval::EvalResult;

/// Runs a project needs in the window before the self-check gives a verdict.
pub const MIN_RUNS: u64 = 30;
/// Answered share below which the self-check fails (with `MIN_RUNS` runs).
pub const FLOOR: f64 = 0.90;
/// Window the self-check reads, in hours.
const EVAL_WINDOW_HOURS: i64 = 72;

const HOOK_MARK: &str = " CSR hook prompt-submit [";
const INJECT_MARK: &str = "CSR prompt-submit inject:";
const SYNTHETIC_PREFIXES: [&str; 3] = ["probe", "replay-", "perf-probe"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Delivered,
    Empty,
    Undecided,
    Fallback,
    TimedOut,
}

#[derive(Debug, Clone)]
struct Run {
    at: DateTime<Utc>,
    project: String,
    outcome: Outcome,
}

/// Counts for one project in one window.
#[derive(Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ProjectDelivery {
    pub n: u64,
    pub delivered: u64,
    pub answered: u64,
    pub empty: u64,
    #[serde(default)]
    pub undecided: u64,
    pub fallback: u64,
    pub timed_out: u64,
    /// delivered / n
    pub share: f64,
    pub answered_share: f64,
}

/// The `prompt_delivery` block of `csr-engine status`.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct PromptDelivery {
    pub installed_at: Option<DateTime<Utc>>,
    pub last_24h: BTreeMap<String, ProjectDelivery>,
    pub last_72h: BTreeMap<String, ProjectDelivery>,
    pub history: BTreeMap<String, ProjectDelivery>,
}

/// The value of `key=` among whitespace-separated tokens.
fn token<'a>(rest: &'a str, key: &str) -> Option<&'a str> {
    rest.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
}

/// Runs in log order, synthetic and repeated prompt ids removed.
fn parse_runs(text: &str, since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut after_inject = false;
    for line in text.lines() {
        let stamp = line
            .split_whitespace()
            .next()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|t| t.with_timezone(&Utc));
        let Some(at_mark) = line.find(HOOK_MARK) else {
            // A line without a timestamp continues the one above (a query
            // preview that held a newline): it neither sets nor clears the mark.
            if stamp.is_some() {
                after_inject = line.contains(INJECT_MARK);
            }
            continue;
        };
        let inject_above = std::mem::take(&mut after_inject);
        let Some(at) = stamp else {
            continue;
        };
        if at < since || at > until {
            continue;
        }
        let after = &line[at_mark + HOOK_MARK.len()..];
        let Some((project, rest)) = after.split_once("]:") else {
            continue;
        };
        let Some(prompt_id) = token(rest, "prompt_id").filter(|id| !id.is_empty() && *id != "-")
        else {
            continue;
        };
        if SYNTHETIC_PREFIXES.iter().any(|p| prompt_id.starts_with(p)) {
            continue;
        }
        let outcome = match token(rest, "transport") {
            Some("unavailable") => Outcome::TimedOut,
            // A fallback is a miss even when it produced stdout.
            Some("fallback") => Outcome::Fallback,
            Some("socket" | "ok") => {
                let injected = match token(rest, "injected") {
                    Some(bytes) => bytes.parse::<u64>().map(|b| b > 0).unwrap_or(inject_above),
                    None => inject_above,
                };
                if token(rest, "decided") == Some("abstain") {
                    Outcome::Empty
                } else if injected || token(rest, "decided") == Some("block") {
                    Outcome::Delivered
                } else {
                    Outcome::Undecided
                }
            }
            _ => continue,
        };
        if let Some(&index) = seen.get(prompt_id) {
            if outcome > runs[index].outcome {
                runs[index] = Run {
                    at,
                    project: project.to_owned(),
                    outcome,
                };
            }
            continue;
        }
        seen.insert(prompt_id.to_owned(), runs.len());
        runs.push(Run {
            at,
            project: project.to_owned(),
            outcome,
        });
    }
    runs
}

fn tally(
    runs: &[Run],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> BTreeMap<String, ProjectDelivery> {
    let mut out: BTreeMap<String, ProjectDelivery> = BTreeMap::new();
    for run in runs.iter().filter(|r| r.at >= since && r.at <= until) {
        let p = out.entry(run.project.clone()).or_default();
        p.n += 1;
        match run.outcome {
            Outcome::Delivered => p.delivered += 1,
            Outcome::Empty => p.empty += 1,
            Outcome::Undecided => p.undecided += 1,
            Outcome::Fallback => p.fallback += 1,
            Outcome::TimedOut => p.timed_out += 1,
        }
    }
    for p in out.values_mut() {
        p.share = p.delivered as f64 / p.n as f64;
        p.answered = p.delivered + p.empty;
        p.answered_share = p.answered as f64 / p.n as f64;
    }
    out
}

/// The report for log `text` as of `now`.
pub fn report_at(text: &str, now: DateTime<Utc>) -> PromptDelivery {
    report_since(text, now, None)
}

fn report_since(
    text: &str,
    now: DateTime<Utc>,
    installed_at: Option<DateTime<Utc>>,
) -> PromptDelivery {
    let window = |hours| {
        let since = installed_at
            .unwrap_or(DateTime::<Utc>::MIN_UTC)
            .max(now - Duration::hours(hours));
        tally(&parse_runs(text, since, now), since, now)
    };
    let history_end = installed_at.and_then(|at| at.checked_sub_signed(Duration::nanoseconds(1)));
    PromptDelivery {
        installed_at,
        last_24h: window(24),
        last_72h: window(72),
        history: history_end
            .map(|end| {
                tally(
                    &parse_runs(text, DateTime::<Utc>::MIN_UTC, end.min(now)),
                    DateTime::<Utc>::MIN_UTC,
                    end.min(now),
                )
            })
            .unwrap_or_default(),
    }
}

/// The rotated generation then the live log, as one text. Unreadable: empty.
fn read_log() -> String {
    let read = |p: Option<std::path::PathBuf>| {
        p.and_then(|p| std::fs::read(p).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    let mut text = read(super::rotated_log_path());
    text.push('\n');
    text.push_str(&read(super::default_log_path()));
    text
}

/// The report for the installation's own log, now.
pub fn report_now() -> PromptDelivery {
    report_since(&read_log(), Utc::now(), running_binary_installed_at())
}

/// ctime follows the install/rename even when a copy preserves the build mtime.
pub(super) fn running_binary_installed_at() -> Option<DateTime<Utc>> {
    let metadata = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        DateTime::from_timestamp(metadata.ctime(), metadata.ctime_nsec() as u32)
    }
    #[cfg(not(unix))]
    {
        metadata
            .created()
            .or_else(|_| metadata.modified())
            .ok()
            .map(DateTime::from)
    }
}

/// `status` JSON for `report` with the `prompt_delivery` block added.
pub fn status_json<T: Serialize>(report: &T) -> anyhow::Result<String> {
    let mut value = serde_json::to_value(report)?;
    // Why `healthy` is false and what fixes it, next to the verdict.
    crate::storage::integrity::add_health_reason(&mut value);
    // Hooks `hook install` registers that settings.json lacks; read-only, and
    // not part of `healthy`.
    let missing = crate::hooks::install::missing_csr_hook_events_now();
    crate::hooks::install::add_hooks_missing(&mut value, &missing);
    let delivery = report_now();
    // What `healthy` (database integrity only) does not say; see `degraded`.
    let mut signals = super::degraded::degraded_signals(&delivery, &missing, Utc::now());
    signals.extend(super::degraded::status_signals(&value));
    super::degraded::add_degraded(&mut value, &signals);
    if let Some(object) = value.as_object_mut() {
        object.insert("prompt_delivery".into(), serde_json::to_value(delivery)?);
    }
    Ok(serde_json::to_string_pretty(&value)?)
}

/// The `eval --full` result for `report`'s 72 h window.
fn eval_result_for(report: &PromptDelivery) -> EvalResult {
    const NAME: &str = "Prompt Delivery";
    let (mut failing, mut passing, mut short) = (Vec::new(), Vec::new(), Vec::new());
    for (project, p) in &report.last_72h {
        let text = format!(
            "{project} {:.1}% ({}/{}) delivered={} undecided={} fallback={} timed-out={}",
            p.answered_share * 100.0,
            p.answered,
            p.n,
            p.delivered,
            p.undecided,
            p.fallback,
            p.timed_out
        );
        if p.n < MIN_RUNS {
            short.push(format!("{text} n={}", p.n));
        } else if (p.answered as f64) < FLOOR * p.n as f64 {
            failing.push(text);
        } else {
            passing.push(text);
        }
    }
    let hours = EVAL_WINDOW_HOURS;
    let floor = (FLOOR * 100.0) as u32;
    let no_verdict = if short.is_empty() {
        String::new()
    } else {
        format!("; NO VERDICT (n < {MIN_RUNS}): {}", short.join(", "))
    };
    if !failing.is_empty() {
        let detail = format!(
            "FAIL: below {floor}% over {hours}h: {}{no_verdict}",
            failing.join(", ")
        );
        return EvalResult::fail(NAME, "hooks", 0.0, detail);
    }
    if passing.is_empty() {
        let detail = format!(
            "NO VERDICT: no project has n >= {MIN_RUNS} over {hours}h ({})",
            short.join(", ")
        );
        return EvalResult::no_verdict(NAME, "hooks", detail);
    }
    let detail = format!(
        "at or above {floor}% over {hours}h: {}{no_verdict}",
        passing.join(", ")
    );
    EvalResult::pass(NAME, "hooks", 0.0, detail)
}

/// The `eval --full` check on the installation's own log.
pub fn eval_result() -> EvalResult {
    eval_result_for(&report_now())
}

#[cfg(test)]
#[path = "prompt_delivery_tests.rs"]
mod decision_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T23:40:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn hook(ts: &str, project: &str, fields: &str) -> String {
        format!("{ts} CSR hook prompt-submit [{project}]: stdin=0ms setup=1ms hook=9ms flush=0ms total=10ms {fields} includes_startup=1")
    }

    fn inject(ts: &str) -> String {
        format!("{ts} CSR prompt-submit inject: q_len=1 q_hash=1234abcd items=2 anti=0 ancestry=0 stdout=300B top=[0.5/chunk]")
    }

    /// 12 hook rows: every outcome, a probe id, a replay- id, a repeated id,
    /// two legacy rows without a usable transport, one row outside 24 h.
    fn twelve_rows() -> String {
        let t = "2026-11-09T23:";
        [
            inject(&format!("{t}01:00Z")),
            hook(&format!("{t}01:00Z"), "alpha", "transport=socket prompt_id=a1"),
            hook(&format!("{t}02:00Z"), "alpha", "transport=socket prompt_id=a2"),
            hook(&format!("{t}03:00Z"), "alpha", "transport=fallback prompt_id=a3 injected=412"),
            hook(&format!("{t}04:00Z"), "alpha", "transport=socket prompt_id=a4 injected=0"),
            hook(&format!("{t}05:00Z"), "alpha", "transport=unavailable prompt_id=a5"),
            hook(&format!("{t}06:00Z"), "alpha", "transport=socket prompt_id=probe-1"),
            hook(&format!("{t}07:00Z"), "alpha", "transport=socket prompt_id=replay-9"),
            hook(&format!("{t}08:00Z"), "alpha", "transport=unavailable prompt_id=a1"),
            hook(&format!("{t}09:00Z"), "alpha", "transport=ok"),
            "2026-11-09T23:10:00Z CSR hook prompt-submit [alpha]: stdin=0ms setup=0ms hook=470ms flush=0ms total=471ms includes_startup=1".to_owned(),
            hook(&format!("{t}11:00Z"), "beta", "transport=socket prompt_id=b1 injected=100"),
            hook("2026-11-08T07:00:00Z", "alpha", "transport=socket prompt_id=a6 injected=77"),
        ]
        .join("\n")
    }

    #[test]
    fn twelve_row_log_gives_exact_counts() {
        let report = report_at(&twelve_rows(), now());
        let alpha = |n, delivered, empty, undecided, fallback, timed_out| ProjectDelivery {
            n,
            delivered,
            answered: delivered + empty,
            empty,
            undecided,
            fallback,
            timed_out,
            share: delivered as f64 / n as f64,
            answered_share: (delivered + empty) as f64 / n as f64,
        };
        // a1's timeout wins over its block; a3 is a fallback miss.
        assert_eq!(report.last_24h["alpha"], alpha(5, 0, 0, 2, 1, 2));
        assert_eq!(report.last_24h["beta"], alpha(1, 1, 0, 0, 0, 0));
        // a6 sits 40 h back: inside 72 h, outside 24 h
        assert_eq!(report.last_72h["alpha"], alpha(6, 1, 0, 2, 1, 2));
        assert_eq!(report.last_72h["beta"], alpha(1, 1, 0, 0, 0, 0));
        assert_eq!(report.last_24h.len(), 2);
    }

    #[test]
    fn status_json_adds_the_block_and_keeps_the_report() {
        #[derive(Serialize)]
        struct Fake {
            chunks: u64,
        }
        let json = status_json(&Fake { chunks: 7 }).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["chunks"], 7);
        assert!(value["prompt_delivery"]["last_24h"].is_object());
        assert!(value["prompt_delivery"]["last_72h"].is_object());
        // Present always; its content depends on the machine's own log and settings.
        assert!(value["degraded"].is_array());
        assert!(value.get("health_reason").is_none());
    }

    #[test]
    fn status_json_names_why_the_store_is_unhealthy() {
        #[derive(Serialize)]
        struct Fake {
            healthy: Option<bool>,
            fts_dangling_rowids: Option<u64>,
        }
        let unhealthy = Fake {
            healthy: Some(false),
            fts_dangling_rowids: Some(172),
        };
        let value: serde_json::Value =
            serde_json::from_str(&status_json(&unhealthy).unwrap()).unwrap();
        let reason = value["health_reason"].as_str().unwrap();
        assert!(reason.starts_with("172 keyword-index rows"), "{reason}");
        assert!(reason.contains("csr-engine status --deep"), "{reason}");
        let healthy = Fake {
            healthy: Some(true),
            fts_dangling_rowids: Some(0),
        };
        let value: serde_json::Value =
            serde_json::from_str(&status_json(&healthy).unwrap()).unwrap();
        assert!(value["health_reason"].is_null());
    }

    fn log_of(socket: usize, unavailable: usize) -> String {
        let mut lines = Vec::new();
        for i in 0..socket {
            lines.push(hook(
                "2026-11-09T22:00:00Z",
                "gamma",
                &format!("transport=socket prompt_id=s{i} injected=50"),
            ));
        }
        for i in 0..unavailable {
            lines.push(hook(
                "2026-11-09T22:00:00Z",
                "gamma",
                &format!("transport=unavailable prompt_id=u{i}"),
            ));
        }
        lines.join("\n")
    }

    #[test]
    fn eval_check_fails_at_thirty_runs_and_eighty_percent() {
        let r = eval_result_for(&report_at(&log_of(24, 6), now()));
        assert!(!r.passed, "{}", r.detail);
        assert!(r.detail.contains("gamma 80.0% (24/30)"), "{}", r.detail);
    }

    #[test]
    fn eval_check_gives_no_verdict_under_thirty_and_passes_at_ninety() {
        let few = eval_result_for(&report_at(&log_of(2, 27), now()));
        assert!(
            !few.passed && few.no_verdict && few.format_line().contains("[NO VERDICT]"),
            "{}",
            few.detail
        );
        let ok = eval_result_for(&report_at(&log_of(27, 3), now()));
        assert!(
            ok.passed && !ok.detail.contains("NO VERDICT"),
            "{}",
            ok.detail
        );
        let none = eval_result_for(&PromptDelivery::default());
        assert!(
            !none.passed && none.no_verdict && none.detail.starts_with("NO VERDICT"),
            "{}",
            none.detail
        );
    }

    #[test]
    fn legacy_fallback_rows_are_misses_and_scrubbed_detail_keeps_its_mark() {
        let t = "2026-11-09T23:";
        let text = [
            hook(
                &format!("{t}01:00Z"),
                "delta",
                "transport=fallback prompt_id=d1",
            ),
            format!("{t}02:00Z CSR prompt-submit inject: q_len=22 q_hash=1234abcd items=2 anti=0 ancestry=0 stdout=300B top=[]"),
            hook(
                &format!("{t}02:01Z"),
                "delta",
                "transport=socket prompt_id=d2",
            ),
            hook(
                &format!("{t}03:00Z"),
                "delta",
                "total=500ms transport=unavailable prompt_id=d3 watchdog=1",
            ),
        ]
        .join("\n");
        let delta = &report_at(&text, now()).last_24h["delta"];
        assert_eq!(
            (
                delta.n,
                delta.delivered,
                delta.empty,
                delta.fallback,
                delta.timed_out
            ),
            (3, 1, 0, 1, 1)
        );
    }

    #[test]
    fn each_decision_counts_as_answered_but_only_blocks_are_delivered() {
        let rows = [
            ("block", "on_topic", 100),
            ("abstain", "lookalike_only", 0),
            ("abstain", "below_floor", 0),
            ("abstain", "thin_pool", 0),
            ("abstain", "no_candidates", 0),
            ("abstain", "too_short", 0),
        ];
        for (decision, reason, bytes) in rows {
            let text = hook(
                "2026-11-09T23:00:00Z",
                "project",
                &format!("transport=socket prompt_id=real injected={bytes} decided={decision} reason={reason}"),
            );
            let report = report_at(&text, now());
            let p = &report.last_24h["project"];
            assert_eq!(
                (p.n, p.answered, p.delivered),
                (1, 1, u64::from(bytes > 0)),
                "{reason}"
            );
            assert_eq!(p.answered_share, 1.0, "{reason}");
        }
    }

    #[test]
    fn worst_outcome_wins_in_either_log_order() {
        let fields = [
            "transport=socket injected=200 decided=block reason=on_topic",
            "transport=socket injected=0 decided=abstain reason=below_floor",
            "transport=fallback injected=300",
            "transport=unavailable injected=0 watchdog=1",
        ];
        for (worst, field) in fields.iter().enumerate().skip(1) {
            let block = hook(
                "2026-11-09T23:00:00Z",
                "project",
                &format!("prompt_id=real {}", fields[0]),
            );
            let row = hook(
                "2026-11-09T23:01:00Z",
                "project",
                &format!("prompt_id=real {field}"),
            );
            for text in [format!("{block}\n{row}"), format!("{row}\n{block}")] {
                let report = report_at(&text, now());
                let p = &report.last_24h["project"];
                assert_eq!((p.n, p.delivered), (1, 0));
                assert_eq!(p.answered, u64::from(worst == 1));
                assert_eq!(p.fallback, u64::from(worst == 2));
                assert_eq!(p.timed_out, u64::from(worst == 3));
            }
        }
    }

    #[test]
    fn install_boundary_keeps_older_duplicates_in_history() {
        let install = now() - Duration::minutes(40);
        let text = [
            hook(
                "2026-11-09T22:59:59Z",
                "project",
                "transport=unavailable prompt_id=real watchdog=1",
            ),
            hook(
                "2026-11-09T23:00:00Z",
                "project",
                "transport=socket prompt_id=real injected=100",
            ),
        ]
        .join("\n");
        let report = report_since(&text, now(), Some(install));
        assert_eq!(report.installed_at, Some(install));
        assert_eq!(report.last_24h["project"].delivered, 1);
        assert_eq!(report.last_72h["project"].answered, 1);
        assert_eq!(report.history["project"].timed_out, 1);
        assert_eq!(report.history["project"].n, 1);
    }

    #[test]
    fn each_rolling_window_deduplicates_only_its_own_rows() {
        let text = [
            hook(
                "2026-11-08T07:00:00Z",
                "project",
                "transport=unavailable prompt_id=real",
            ),
            hook(
                "2026-11-09T23:00:00Z",
                "project",
                "transport=socket prompt_id=real injected=100",
            ),
        ]
        .join("\n");
        let report = report_at(&text, now());
        assert_eq!(report.last_24h["project"].delivered, 1);
        assert_eq!(report.last_72h["project"].timed_out, 1);
    }

    #[test]
    fn abstentions_pass_the_answered_gate_at_thirty_runs() {
        let log = (0..30).map(|i| hook(
            "2026-11-09T23:00:00Z", "project",
            &format!("transport=socket prompt_id=real-{i} injected=0 decided=abstain reason=lookalike_only"),
        )).collect::<Vec<_>>().join("\n");
        let report = report_at(&log, now());
        let result = eval_result_for(&report);
        assert!(result.passed && !result.no_verdict, "{}", result.detail);
        assert_eq!(report.last_72h["project"].delivered, 0);
    }

    /// Manual recount: `CSR_RECOUNT_LOG=<file> CSR_RECOUNT_NOW=<rfc3339> cargo test
    /// recount_live_log -- --ignored --nocapture` prints the report as JSON.
    #[test]
    #[ignore]
    fn recount_live_log() {
        let path = std::env::var("CSR_RECOUNT_LOG").expect("CSR_RECOUNT_LOG");
        let at = std::env::var("CSR_RECOUNT_NOW").expect("CSR_RECOUNT_NOW");
        let at = DateTime::parse_from_rfc3339(&at)
            .unwrap()
            .with_timezone(&Utc);
        let text = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        println!("{}", serde_json::to_string(&report_at(&text, at)).unwrap());
    }
}
