use super::{array, masthead, num, paths::Paths, private, pyfmt, session::Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
pub struct Plan {
    pub ctx: usize,
    pub floor: f64,
    pub measured: bool,
    pub summary: f64,
    pub payback: Option<f64>,
    pub per_turn: Option<f64>,
    pub worth: bool,
    pub trend: Option<f64>,
    pub story: Option<Story>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Story {
    pub text: String,
    pub by_csr: usize,
    pub by_you: usize,
    pub payback: i64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Usual {
    pub at: f64,
    pub median: Option<f64>,
    pub n: usize,
    pub excluded: usize,
}
#[derive(Debug)]
pub struct Realized {
    pub before: f64,
    pub floor: f64,
    pub summary: f64,
    pub cost: f64,
    pub saving: f64,
    pub requests: usize,
    pub net: f64,
    pub even_at: Option<f64>,
    pub even_request: Option<usize>,
    pub series: Vec<(f64, f64)>,
    pub start: f64,
    pub pre: Vec<(f64, f64)>,
}
fn round1(n: f64) -> f64 {
    pyfmt::fixed(n, 1).parse().unwrap_or(n)
}
pub fn compact_records(paths: &Paths, sid: &str) -> Vec<Value> {
    let mut records = vec![];
    super::tail::scan(&paths.keep.join("dreams.jsonl"), |r| {
        if r.is_object() && r["kind"] == "compact" && r["session"] == sid {
            records.push(r);
        }
    });
    records.sort_by(|a, b| num(&a["at"]).total_cmp(&num(&b["at"])));
    records
}
pub fn compaction_realized(
    record: &Value,
    main: &Value,
    compacts: &Value,
    summaries: &Value,
) -> Option<Realized> {
    let at = record["at"].as_f64()?;
    let before = record["ctx_before"].as_f64()?;
    let mut bounds: Vec<_> = array(compacts).iter().map(num).collect();
    bounds.sort_by(f64::total_cmp);
    let start = bounds
        .iter()
        .copied()
        .rfind(|b| at - 600. <= *b && *b <= at + 5.)
        .unwrap_or(at);
    let until = bounds
        .iter()
        .copied()
        .find(|b| *b > at + 5.)
        .unwrap_or(f64::INFINITY);
    let mut ordered: Vec<_> = array(main)
        .iter()
        .map(|r| (num(&r[0]), num(&r[1])))
        .collect();
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let series: Vec<_> = ordered
        .iter()
        .copied()
        .filter(|(t, c)| start < *t && *t < until && *c != 0.)
        .collect();
    let floor = series.first()?.1;
    let earlier = bounds
        .iter()
        .copied()
        .rfind(|b| *b < start - 5.)
        .unwrap_or(0.);
    let mut pre: Vec<_> = ordered
        .into_iter()
        .filter(|(t, c)| earlier < *t && *t <= start && *c != 0.)
        .collect();
    pre.drain(..pre.len().saturating_sub(12));
    let summary = record["ctx_after"]
        .as_f64()
        .filter(|n| *n > 0.)
        .unwrap_or_else(|| masthead::summary_of(summaries).0);
    let cost = masthead::compaction_cost(floor, before, summary);
    let saving = masthead::request_saving(floor, before);
    let even = (0..series.len()).find(|i| saving > 0. && (*i + 1) as f64 * saving >= cost);
    Some(Realized {
        before,
        floor,
        summary,
        cost,
        saving,
        requests: series.len(),
        net: series.len() as f64 * saving - cost,
        even_at: even.map(|i| series[i].0),
        even_request: even.map(|i| i + 1),
        series,
        start,
        pre,
    })
}
pub fn compact_story(
    records: &[Value],
    compacts: &Value,
    sizes: &Value,
    main: &Value,
    context: usize,
    summaries: &Value,
) -> Option<Story> {
    let mut done: Vec<_> = records
        .iter()
        .filter(|r| {
            r["event"] == "compact"
                && r["outcome"] == "compacted"
                && r["at"].is_number()
                && r["ctx_before"].is_number()
        })
        .collect();
    done.sort_by(|a, b| num(&a["at"]).total_cmp(&num(&b["at"])));
    if done.is_empty() {
        return None;
    }
    let floor = masthead::floor_of(sizes).0;
    let net: f64 = done
        .iter()
        .map(|record| {
            compaction_realized(record, main, compacts, summaries)
                .map(|p| p.net)
                .unwrap_or_else(|| {
                    let summary = record["ctx_after"]
                        .as_f64()
                        .filter(|n| *n > 0.)
                        .unwrap_or_else(|| masthead::summary_of(summaries).0);
                    -masthead::compaction_cost(floor, num(&record["ctx_before"]), summary)
                })
        })
        .sum();
    let by_csr = done.len();
    let by_you = array(compacts).len().saturating_sub(by_csr);
    let mut pieces = vec![
        format!(
            "this session: {by_csr} {} by CSR",
            if by_csr == 1 {
                "compaction"
            } else {
                "compactions"
            }
        ),
        format!("{by_you} by you"),
    ];
    if context > 0 {
        pieces.push(format!("context {}", pyfmt::short(context as f64)));
    }
    pieces.push(if net >= 0. {
        format!(
            "net saving so far ~{} tokens",
            if net >= 1e6 {
                format!("{:.1}M", net / 1e6)
            } else {
                pyfmt::short(net)
            }
        )
    } else {
        format!("not yet paid back ~{} tokens", pyfmt::short(-net))
    });
    Some(Story {
        text: pieces.join(" · "),
        by_csr,
        by_you,
        payback: net as i64,
    })
}
pub fn compact_plan_json(paths: &Paths, sid: &str, context: usize) -> Plan {
    let mut session = Session::new(sid);
    if super::paths::valid_id(sid) {
        session.read_transcript(paths);
    }
    plan_from(&session.data, &compact_records(paths, sid), context)
}
pub fn plan_from(s: &Value, records: &[Value], context: usize) -> Plan {
    let (floor, measured) = masthead::floor_of(&s["compact_sizes"]);
    let summary = masthead::summary_of(&s["summary_sizes"]).0;
    let requests = (context > 0)
        .then(|| masthead::payback_requests(floor, context as f64, summary))
        .flatten();
    let per = masthead::requests_per_turn(&s["main"], &s["prompts"]);
    let ends = masthead::turn_ends(&s["main"], &s["prompts"]);
    let since = array(&s["compacts"]).iter().map(num).fold(0., f64::max);
    let vals: Vec<_> = ends
        .iter()
        .filter(|(t, _)| *t > since)
        .map(|(_, c)| *c)
        .collect();
    let diffs: Vec<_> = vals.windows(2).map(|w| w[1] - w[0]).collect();
    let trend = (vals.len() >= 3)
        .then(|| masthead::median_of(&diffs[diffs.len().saturating_sub(5)..]))
        .flatten();
    Plan {
        ctx: context,
        floor,
        measured,
        summary,
        payback: requests.map(round1),
        per_turn: per.map(round1),
        worth: requests.is_some_and(|n| n / per.unwrap_or(1.) <= 15.),
        trend,
        story: compact_story(
            records,
            &s["compacts"],
            &s["compact_sizes"],
            &s["main"],
            context,
            &s["summary_sizes"],
        ),
    }
}
pub fn csr_compaction_times(paths: &Paths) -> Vec<f64> {
    let mut times = vec![];
    for log in [
        paths.keep.join("dreams.jsonl"),
        paths
            .home
            .join(".claude-self-reflect/csr-shifts/dreams.jsonl"),
    ] {
        super::tail::scan(&log, |r| {
            if r["kind"] == "compact" && r["event"] == "compact" {
                if let Some(at) = r["at"].as_f64() {
                    times.push(at);
                }
            }
        });
    }
    times.sort_by(f64::total_cmp);
    times.dedup();
    times
}
pub fn keep_compact_usual(paths: &Paths, now: f64) -> Usual {
    let csr = csr_compaction_times(paths);
    let excluded = csr.len();
    let seconds = super::compaction_scan::CompactionScan::own_compactions(paths, now, csr);
    let kept = Usual {
        at: now,
        median: (seconds.len() >= 3)
            .then(|| masthead::median_of(&seconds))
            .flatten(),
        n: seconds.len(),
        excluded,
    };
    let _ = private::write_atomic(
        &paths.keep.join("compact-usual.json"),
        &serde_json::to_string(&kept).unwrap(),
        &paths.keep,
    );
    kept
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OwnUsual {
    pub median: f64,
    pub n: usize,
}
pub fn compact_usual(paths: &Paths, now: f64, refresh: bool) -> Option<OwnUsual> {
    usual_with(paths, now, refresh, || {
        let paths = paths.clone();
        std::thread::spawn(move || {
            keep_compact_usual(&paths, now);
        });
    })
}
fn usual_with(paths: &Paths, now: f64, refresh: bool, spawn: impl FnOnce()) -> Option<OwnUsual> {
    static ASKED: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, f64>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let kept = super::paths::read_json(&paths.keep.join("compact-usual.json"));
    let stale = kept["at"].as_f64().is_none_or(|at| now - at >= 86400.);
    let start = refresh
        && stale
        && ASKED.lock().is_ok_and(|mut asked| {
            let last = asked.entry(paths.home.clone()).or_insert(0.);
            if now - *last <= 600. {
                return false;
            }
            *last = now;
            true
        });
    if start {
        spawn();
    }
    kept["median"]
        .as_f64()
        .filter(|n| *n > 0.)
        .zip(kept["n"].as_u64())
        .filter(|(_, n)| *n >= 3)
        .map(|(median, n)| OwnUsual {
            median,
            n: n as usize,
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_compact_plan_reads_this_sessions_floor_and_says_whether_it_pays_back() {
        let s = json!({"compact_sizes":[[214000,66000,40000]],"summary_sizes":[20000],
            "main":[[1010,150000],[1110,190000],[1210,214000],[1410,66000],[1510,80000]],
            "prompts":[1000,1100,1200,1400,1500],"compacts":[1300]});
        let plan = plan_from(&s, &[done()], 300000);
        assert_eq!(
            (plan.floor, plan.summary, plan.per_turn),
            (66000., 20000., Some(1.))
        );
        assert!(plan.measured && plan.worth);
        assert!(plan
            .story
            .unwrap()
            .text
            .contains("1 compaction by CSR · 0 by you"));
    }
    #[test]
    fn test_a_session_with_no_compaction_plans_on_the_guessed_floor_and_has_no_story() {
        let plan = plan_from(&Session::new("s").data, &[], 214000);
        assert_eq!((plan.floor, plan.summary), (65000., 21700.));
        assert!(!plan.measured);
        assert!(plan.story.is_none() && plan.payback.is_some());
    }
    fn done() -> Value {
        json!({"kind":"compact","event":"compact","session":"s", "outcome":"compacted",
            "at":1305,"ctx_before":214000})
    }
    #[test]
    fn test_csrs_compactions_and_yours_and_the_payback_so_far() {
        let main = json!([
            [1010, 150000],
            [1110, 190000],
            [1210, 214000],
            [1410, 66000],
            [1510, 80000],
            [1610, 95000]
        ]);
        let story = compact_story(
            &[done()],
            &json!([1300]),
            &json!([[214000, 66000, 40000]]),
            &main,
            95000,
            &Value::Null,
        )
        .unwrap();
        assert_eq!((story.by_csr, story.by_you), (1, 0));
        assert_eq!(
            story.payback,
            (3. * masthead::request_saving(66000., 214000.)
                - masthead::compaction_cost(66000., 214000., 21700.)) as i64
        );
        let later = compact_story(
            &[done()],
            &json!([1300, 1450]),
            &Value::Null,
            &main,
            95000,
            &Value::Null,
        )
        .unwrap();
        assert_eq!(later.by_you, 1);
        assert_eq!(
            later.payback,
            (masthead::request_saving(66000., 214000.)
                - masthead::compaction_cost(66000., 214000., 21700.)) as i64
        );
    }
    #[test]
    fn test_no_compaction_of_csrs_no_story_and_skips_do_not_count() {
        let mut record = done();
        record["event"] = "skip".into();
        assert!(compact_story(
            &[record],
            &json!([1300]),
            &Value::Null,
            &Value::Null,
            95000,
            &Value::Null
        )
        .is_none());
    }
    #[test]
    fn test_the_real_compaction_cost_276k_and_paid_back_at_its_14th_request() {
        let main = Value::Array(
            (0..110)
                .map(|i| json!([1010 + 10 * i, 70300 + 1500 * i]))
                .collect(),
        );
        let record = json!({"at":1000,"ctx_before":278129,"ctx_after":21730});
        let paid = compaction_realized(&record, &main, &json!([995, 5000]), &Value::Null).unwrap();
        assert_eq!(
            (paid.floor, paid.summary, paid.requests),
            (70300., 21730., 110)
        );
        assert_eq!((paid.even_request, paid.even_at), (Some(14), Some(1140.)));
        assert_eq!(pyfmt::round(paid.cost), 277063.);
        assert_eq!(pyfmt::round(paid.net), 2009056.);
        assert!(compaction_realized(
            &record,
            &json!([[990, 278000]]),
            &json!([995]),
            &Value::Null
        )
        .is_none());
    }
    #[test]
    fn test_the_records_are_this_sessions_compact_ones_oldest_first() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        std::fs::create_dir_all(&paths.keep).unwrap();
        std::fs::write(
            paths.keep.join("dreams.jsonl"),
            concat!(
                "{\"kind\":\"compact\",\"session\":\"s\",\"at\":5}\nnot json\n",
                "{\"kind\":\"compact\",\"session\":\"other\",\"at\":1}\n",
                "{\"kind\":\"compact\",\"session\":\"s\",\"at\":2}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            compact_records(&paths, "s")
                .iter()
                .map(|r| num(&r["at"]))
                .collect::<Vec<_>>(),
            [2., 5.]
        );
    }
    #[test]
    fn test_your_own_are_manual_in_the_band_in_30_days_and_never_csrs() {
        use std::{
            fs::{File, FileTimes},
            time::{Duration, UNIX_EPOCH},
        };
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let file = paths.projects.join("p/s.jsonl");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let now = 1800000000.;
        let mut rows = String::new();
        for (ago, pre, seconds, trigger) in [
            (3600., 200000, 80., "manual"),
            (7200., 150000, 60., "manual"),
            (9000., 250000, 70., "auto"),
            (5000., 250000, 140., "manual"),
            (86400., 300000, 100., "manual"),
            (40. * 86400., 250000, 50., "manual"),
            (2. * 86400., 400000, 200., "manual"),
            (3. * 86400., 190000, 95., "manual"),
        ] {
            rows.push_str(
                &json!({"subtype":"compact_boundary",
                "timestamp":chrono::DateTime::from_timestamp((now-ago) as i64,0)
                    .unwrap().to_rfc3339(),"compactMetadata":{
                    "trigger":trigger,"preTokens":pre,"durationMs":seconds*1000.}})
                .to_string(),
            );
            rows.push('\n');
        }
        rows.push_str("{\"subtype\":\"compact_boundary\",\"torn\n");
        std::fs::write(&file, rows).unwrap();
        File::open(&file)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(now as u64)))
            .unwrap();
        std::fs::create_dir_all(&paths.keep).unwrap();
        std::fs::write(
            paths.keep.join("dreams.jsonl"),
            json!({"kind":"compact",
            "event":"compact","at":now-4994.})
            .to_string()
                + "\n",
        )
        .unwrap();
        assert_eq!(
            super::super::compaction_scan::CompactionScan::own_compactions(
                &paths,
                now,
                csr_compaction_times(&paths)
            ),
            [80., 95., 100.]
        );
        let usual = keep_compact_usual(&paths, now);
        assert_eq!((usual.median, usual.n, usual.excluded), (Some(95.), 3, 1));
    }
    #[test]
    fn test_the_card_never_scans_a_day_old_answer_starts_one_process() {
        use std::cell::Cell;
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        std::fs::create_dir_all(&paths.keep).unwrap();
        let now = 1800000000.;
        std::fs::write(
            paths.keep.join("compact-usual.json"),
            json!({"at":now-2.*86400.,"median":89.5,"n":144}).to_string(),
        )
        .unwrap();
        let calls = Cell::new(0);
        let spawn = || calls.set(calls.get() + 1);
        assert_eq!(usual_with(&paths, now, true, spawn).unwrap().n, 144);
        assert_eq!(
            usual_with(&paths, now + 60., true, spawn).unwrap().median,
            89.5
        );
        assert_eq!(calls.get(), 1);
        std::fs::write(
            paths.keep.join("compact-usual.json"),
            json!({"at":now,"median":null,"n":2}).to_string(),
        )
        .unwrap();
        assert!(usual_with(&paths, now + 900., true, spawn).is_none());
        assert_eq!(calls.get(), 1);
    }
}
