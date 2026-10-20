//! The infinite-session facts the info page reads: the mod's compaction plan, the story of CSR's
//! own compactions in this session, which transcript boundaries were CSR's, and the compaction
//! timer's rows.
use super::{array, compact, db::Db, masthead, num, paths::Paths, render::text::*, string};
use serde_json::{json, Value};

const PLAN_LIFE: f64 = 7200.;
const PER_CHAPTER: usize = 5;

/// The mod's plan for this session's next compaction, `{state, text}`, or null: none written,
/// taken down (state none), or two hours old. A countdown's seconds are worked out here from when
/// it ends.
pub fn compact_plan_of(paths: &Paths, sid: &str, now: f64) -> Value {
    let Some(file) = paths.kept_file("compact-plan", sid) else {
        return Value::Null;
    };
    let plan = super::paths::read_json(&file);
    if !plan.is_object() {
        return Value::Null;
    }
    let at = num(&plan["at"]);
    let state = string(&plan["state"]);
    let Some(text) = plan["text"].as_str() else {
        return Value::Null;
    };
    if state == "none" || text.trim().is_empty() || now - at > PLAN_LIFE {
        return Value::Null;
    }
    let mut text = text.to_owned();
    if let (true, Some(ends)) = (state == "countdown", plan["ends_at"].as_f64()) {
        static COUNTING: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"^Compacting in \d+s").unwrap());
        let left = (ends - now).ceil().max(0.) as i64;
        text = COUNTING
            .replace(&text, format!("Compacting in {left}s"))
            .into_owned();
    }
    json!({"state": state, "text": one_line(&text, 200)})
}

/// The compact boundaries CSR's compactions wrote, oldest first: for each compacted record, the
/// newest boundary in the ten minutes before it.
pub fn csr_boundaries(records: &[Value], compacts: &Value) -> Vec<f64> {
    let mut bounds: Vec<f64> = array(compacts).iter().map(num).collect();
    bounds.sort_by(f64::total_cmp);
    let mut out: Vec<f64> = vec![];
    for r in records {
        if let (true, Some(at)) = (
            r["event"] == "compact" && r["outcome"] == "compacted",
            r["at"].as_f64(),
        ) {
            if let Some(own) = bounds
                .iter()
                .copied()
                .rfind(|b| at - 600. <= *b && *b <= at + 5.)
            {
                if !out.contains(&own) {
                    out.push(own);
                }
            }
        }
    }
    out.sort_by(f64::total_cmp);
    out
}

/// The infinite-session rows for the info page: `{plan, state, story, card, tally, csr_at}`, null
/// when there is neither a plan, a story nor a card of CSR's to tell. The card is the main page's
/// compaction card (`card.rs`).
pub fn infinite_info(paths: &Paths, sid: &str, now: f64, s: &Value) -> Value {
    let plan = compact_plan_of(paths, sid, now);
    let (mut story, mut tally, mut csr_at) = (Value::Null, Value::Null, vec![]);
    let mut card = Value::Null;
    if s.as_object().is_some_and(|o| !o.is_empty()) {
        let main = &s["main"];
        let records = compact::compact_records(paths, sid);
        let context = array(main)
            .iter()
            .map(|r| (num(&r[0]), num(&r[1])))
            .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)))
            .map_or(0, |r| r.1 as usize);
        if let Some(made) = compact::compact_story(
            &records,
            &s["compacts"],
            &s["compact_sizes"],
            main,
            context,
            &s["summary_sizes"],
        ) {
            story = json!(made.text);
            tally = json!({"by_csr": made.by_csr, "by_you": made.by_you, "payback": made.payback});
        }
        // the card reads the two newest summaries, which hold what the window learnt
        let summaries = array(&s["summaries"]);
        let newest = json!(&summaries[summaries.len().saturating_sub(2)..]);
        card = super::card::compaction_card(
            &records,
            &newest,
            now,
            main,
            &s["compacts"],
            &s["summary_sizes"],
        );
        if card["stage"] == "done" && card["took"].as_f64().is_some_and(|t| t != 0.) {
            // how long your own /compact takes, as last kept (never scanned here)
            card["usual"] = compact::compact_usual(paths, now, false)
                .and_then(|u| serde_json::to_value(u).ok())
                .unwrap_or(Value::Null);
        }
        csr_at = csr_boundaries(&records, &s["compacts"]);
    }
    if plan.is_null() && story.is_null() && card.is_null() {
        return Value::Null;
    }
    json!({
        "plan": plan["text"],
        "state": plan["state"],
        "story": story,
        "card": card,
        "tally": tally,
        "csr_at": csr_at,
    })
}

/// What CSR keeps from the part being folded, one to three lines, each with a number behind it.
pub fn compaction_facts(db: &Db, sid: &str, story: &Value, dreams_on: bool) -> Vec<String> {
    let mut lines = vec![];
    let chunks = num(&db.scalar(
        "select count(*) from chunks where conversation_id=?",
        &[json!(sid)],
    )) as i64;
    if chunks != 0 {
        let digits = chunks.to_string();
        let mut grouped = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(c);
        }
        lines.push(format!(
            "CSR keeps {grouped} chunk{} of this session",
            if chunks == 1 { "" } else { "s" }
        ));
    }
    if let Some(rev) = story["rev"].as_i64().filter(|r| *r != 0) {
        lines.push(format!("story rev {rev} → {} after this", rev + 1));
    }
    if dreams_on {
        lines.push(format!(
            "a new chapter of dreams after this ({PER_CHAPTER} looks)"
        ));
    }
    lines
}

/// The strip at the top of the page while a compaction runs: the timer against the usual time, a
/// bar, and the facts under it.
pub fn compact_rows(info: &Value, width: usize) -> Vec<String> {
    if !info.is_object() {
        return vec![];
    }
    let usual = &info["usual"];
    let usual = usual.is_object().then_some(usual);
    let elapsed = num(&info["elapsed"]);
    let over = usual.is_some_and(|u| elapsed > num(&u["high"]));
    let note = usual.map_or(String::new(), |u| {
        if over {
            "taking longer than usual".into()
        } else {
            format!(
                "usually {} to {}",
                masthead::clock_text(num(&u["low"])),
                masthead::clock_text(num(&u["high"]))
            )
        }
    });
    let kind = if info["trigger"] == "auto" {
        " (auto)"
    } else {
        ""
    };
    let mut words = [
        format!("Compacting{kind}"),
        masthead::clock_text(elapsed),
        note.clone(),
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let mut rows;
    if len(&words) > width.saturating_sub(1) && !note.is_empty() {
        words = format!("Compacting{kind} · {}", masthead::clock_text(elapsed));
        rows = vec![
            format!(" {BOLD}{}{OFF}", fit(&words, width.saturating_sub(1))),
            format!(" {}", fit(&note, width.saturating_sub(1))),
        ];
    } else {
        rows = vec![format!(
            " {BOLD}{}{OFF}",
            fit(&words, width.saturating_sub(1))
        )];
    }
    if let Some(u) = usual {
        let hue = fg(if over { [232, 88, 44] } else { [58, 127, 224] });
        rows.push(format!(
            " {hue}{}{OFF}",
            masthead::compaction_bar(
                elapsed,
                num(&u["high"]),
                8.max(40.min(width.saturating_sub(2)))
            )
        ));
    }
    rows.extend(
        array(&info["facts"])
            .iter()
            .take(3)
            .map(|line| format!(" {DIM}{}{OFF}", fit(string(line), width.saturating_sub(1)))),
    );
    rows
}
