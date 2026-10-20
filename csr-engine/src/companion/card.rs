//! The compaction card on the main page and the masthead's infinite-session rows: what CSR's own
//! compaction kept, what it cost and what it has paid back (pane.py's `compaction_card`,
//! `card_payback`, `payback_rows`, `took_row`, `kept_row`, `compact_section`, `piece_rows`,
//! `infinite_rows`). The numbers come from `compact.rs`; nothing is read twice.
use super::{array, compact, masthead, num, pyfmt::short, render::text::*, send, string, truthy};
use chrono::TimeZone;
use serde_json::{json, Value};

/// A compaction's card stays this long after it ends.
const CARD_LIFE: f64 = 30. * 60.;
/// A marker older than this is from a compaction that never reported its end.
const MARKER_LIFE: f64 = 10. * 60.;
const CARD_EVENTS: [&str; 5] = ["countdown", "start", "compact", "skip", "cancel"];
/// The compaction card's rows fit this many columns at their longest numbers.
const CARD_COLS: usize = 48;
/// The card's picture: four rows of the line, one for the axis words.
const PAYBACK_ROWS: usize = 5;
const CARD_SEND_MOST: usize = 1200;
/// Tokens of summary a compaction writes when this session has measured none.
const SUMMARY_GUESS: f64 = 21700.;
/// The summary sections that say what the window learnt.
const LEARNT: [&str; 1] = ["Errors and fixes"];

fn number(v: &Value) -> bool {
    v.is_number()
}
/// Python's `str(x)` for what a record may hold.
fn said(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// What the summary says its window learnt: the heads of its Errors and fixes, the person's
/// corrections among them.
pub fn learnt_items(text: &str, most: usize) -> Vec<String> {
    let mut out = vec![];
    for name in LEARNT {
        for (mut head, nested) in
            super::lost::section_items(&super::lost::summary_section(text, name))
        {
            if head.ends_with(':') && !nested.is_empty() {
                head = format!("{head} {}", nested[0]);
            }
            if !head.is_empty() {
                out.push(head);
            }
        }
    }
    out.truncate(most);
    out
}

fn realized(r: &compact::Realized) -> Value {
    json!({
        "before": r.before,
        "floor": r.floor,
        "summary": r.summary,
        "cost": r.cost,
        "saving": r.saving,
        "requests": r.requests,
        "net": r.net,
        "even_at": r.even_at,
        "even_request": r.even_request,
        "start": r.start,
    })
}

/// CSR's newest compaction in this session as the pane's card, `{stage, ...}`, or null: soon
/// (counting down), running (the summary runs, with what it keeps), done (the sizes, what it kept,
/// what the window learnt, and what it has paid so far) or skipped. A cancel takes it down; a done
/// or skipped one goes `CARD_LIFE` after it ended.
pub fn compaction_card(
    records: &[Value],
    summaries: &Value,
    now: f64,
    main: &Value,
    compacts: &Value,
    summary_sizes: &Value,
) -> Value {
    let episode: Vec<&Value> = records
        .iter()
        .filter(|r| {
            r["event"]
                .as_str()
                .is_some_and(|e| CARD_EVENTS.contains(&e))
                && number(&r["at"])
        })
        .collect();
    let Some(last) = episode.last() else {
        return Value::Null;
    };
    let at = num(&last["at"]);
    let event = string(&last["event"]);
    let seam = if last["seam"].is_object() {
        &last["seam"]
    } else {
        &Value::Null
    };
    let keep = if last["keep"].is_object() {
        &last["keep"]
    } else {
        &Value::Null
    };
    let rows = |name: &str| -> Vec<String> {
        array(&keep[name])
            .iter()
            .filter(|x| !said(x).trim().is_empty())
            .map(|x| one_line(&said(x), 300))
            .collect()
    };
    let why = if truthy(&last["why"]) {
        said(&last["why"])
    } else if truthy(&seam["why"]) {
        said(&seam["why"])
    } else {
        String::new()
    };
    let mut keeping = rows("running");
    keeping.extend(rows("decisions"));
    keeping.extend(rows("work"));
    keeping.extend(rows("rules"));
    let card = json!({
        "at": last["at"],
        "why": one_line(&why, 120),
        "before": if number(&last["ctx_before"]) { last["ctx_before"].clone() } else { Value::Null },
        "keeping": keeping,
    });
    let with = |extra: Value| {
        let mut card = card.clone();
        for (k, v) in extra.as_object().into_iter().flatten() {
            card[k] = v.clone();
        }
        card
    };
    match event {
        "countdown" => {
            let Some(ends) = last["ends_at"].as_f64().filter(|e| *e >= now - 5.) else {
                return Value::Null;
            };
            with(json!({"stage": "soon", "left": (ends - now).ceil().max(0.) as i64}))
        }
        "start" => {
            if now - at < MARKER_LIFE {
                with(json!({"stage": "running", "elapsed": (now - at).max(0.) as i64}))
            } else {
                Value::Null
            }
        }
        _ if event == "cancel" || now - at > CARD_LIFE => Value::Null,
        "skip" => with(json!({
            "stage": "skipped",
            "reason": one_line(
                &if truthy(&last["reason"]) {
                    said(&last["reason"])
                } else {
                    "a hook said no".into()
                },
                200,
            ),
        })),
        _ => {
            let took = last["took_ms"].as_f64().map(|ms| ms / 1000.);
            let began = at - took.unwrap_or(0.);
            let summary = array(summaries)
                .iter()
                .rev()
                .find(|r| began - 30. <= num(&r[0]) && num(&r[0]) <= at + 300.)
                .map(|r| string(&r[1]).to_owned());
            let paid = compact::compaction_realized(last, main, compacts, summary_sizes);
            let after = paid
                .as_ref()
                .map(|p| json!(p.floor))
                .or_else(|| number(&last["floor"]).then(|| last["floor"].clone()))
                .unwrap_or(Value::Null);
            with(json!({
                "stage": "done",
                "after": after,
                "measured": paid.is_some(),
                "took": took,
                "summary": if num(&last["ctx_after"]) > 0. { last["ctx_after"].clone() } else { Value::Null },
                "payback": if number(&last["payback"]) { last["payback"].clone() } else { Value::Null },
                "per_turn": if number(&last["per_turn"]) { last["per_turn"].clone() } else { Value::Null },
                "paid": paid.as_ref().map_or(Value::Null, realized),
                "usual": null,
                "learnt": summary.map_or(vec![], |t| learnt_items(&t, 4)),
            }))
        }
    }
}

/// "2.0M", "276k": a payback's tokens, as the status line's story says them.
pub fn tokens_said(n: f64) -> String {
    if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else {
        short(n)
    }
}

/// The card's payback as one set of numbers, so its rows and its picture agree, in input tokens
/// at the full price. None when the sizes are unknown.
#[derive(Debug, PartialEq)]
pub struct Money {
    pub before: f64,
    pub floor: f64,
    pub summary: f64,
    pub cost: f64,
    pub saving: f64,
    pub need: Option<i64>,
    pub done: i64,
    pub net: f64,
    pub earned: f64,
}
pub fn card_payback(card: &Value) -> Option<Money> {
    let (before, floor, summary, cost, saving, done, net);
    let paid = &card["paid"];
    if paid.is_object() {
        before = num(&paid["before"]);
        floor = num(&paid["floor"]);
        summary = num(&paid["summary"]);
        cost = num(&paid["cost"]);
        saving = num(&paid["saving"]);
        done = num(&paid["requests"]) as i64;
        net = num(&paid["net"]);
    } else {
        if !number(&card["before"]) || !number(&card["after"]) {
            return None;
        }
        before = num(&card["before"]);
        floor = num(&card["after"]);
        summary = if number(&card["summary"]) {
            num(&card["summary"])
        } else {
            SUMMARY_GUESS
        };
        cost = masthead::compaction_cost(floor, before, summary);
        saving = masthead::request_saving(floor, before);
        done = 0;
        net = -cost;
    }
    let need = (saving > 0.).then(|| (cost / saving).ceil() as i64);
    Some(Money {
        before,
        floor,
        summary,
        cost,
        saving,
        need,
        done,
        net,
        earned: cost + net,
    })
}

/// The card's payback rows, a fact a sentence, by its state: just compacted, the one-time cost
/// and the call that pays it back; paying back, the calls done and to go and what is earned
/// back; paid back, where it did and the net saving.
pub fn payback_rows(money: &Option<Money>) -> Vec<String> {
    let Some(money) = money else {
        return vec![];
    };
    let Some(need) = money.need.filter(|n| *n != 0) else {
        return vec![
            "This compaction does not pay back.".into(),
            "The context did not get smaller.".into(),
        ];
    };
    let cost = short(money.cost);
    let done = money.done;
    if done == 0 {
        return vec![
            format!("One-time cost: {cost} tokens at full input price."),
            format!(
                "Each call saves {}. Paid back at call {need}.",
                short(money.saving)
            ),
        ];
    }
    if done < need {
        let left = need - done;
        return vec![
            format!(
                "Paid back: {done} of {need} calls. {left} {} to go.",
                if left == 1 { "call" } else { "calls" }
            ),
            format!(
                "Earned back: {} of {cost} (full input price).",
                short(money.earned)
            ),
        ];
    }
    vec![
        format!("Paid back at call {need}. Now at call {done}."),
        format!(
            "Net saving: {} tokens at full input price.",
            tokens_said(money.net)
        ),
    ]
}

/// "Took 1:29. Your /compact: 1:27 (median of 136).": how long it took beside how long yours
/// take, "of 136" gone when the row would pass `CARD_COLS`.
pub fn took_row(took: Option<f64>, usual: Option<(f64, usize)>) -> String {
    let Some(took) = took.filter(|t| *t != 0.) else {
        return String::new();
    };
    let said = format!("Took {}.", masthead::clock_text(took));
    let Some((median, n)) = usual else {
        return said;
    };
    let yours = format!("{said} Your /compact: {}", masthead::clock_text(median));
    let full = format!("{yours} (median of {n}).");
    if len(&full) <= CARD_COLS {
        full
    } else {
        format!("{yours} (median).")
    }
}

/// "Kept 2 items, 2 lessons. Click to read.": what the summary carried, counted; empty when it
/// carried nothing.
pub fn kept_row(kept: usize, learnt: usize) -> String {
    let halves: Vec<String> = [(kept, "item"), (learnt, "lesson")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}{}", if n == 1 { "" } else { "s" }))
        .collect();
    if halves.is_empty() {
        String::new()
    } else {
        format!("Kept {}. Click to read.", halves.join(", "))
    }
}

/// "2 waiting in Lost and found.": the card says what the notice on the phone says.
pub fn lost_row(waiting: usize) -> String {
    if waiting > 0 {
        format!("{waiting} waiting in Lost and found.")
    } else {
        String::new()
    }
}

/// How many threads Lost and found shows waiting on you: not set aside, lost or open, and
/// waiting. All of them, not its top three.
pub fn lost_waiting(threads: &Value) -> usize {
    array(threads)
        .iter()
        .filter(|t| {
            !truthy(&t["hidden"]) && t["rank"].as_u64().unwrap_or(9) < 3 && truthy(&t["waiting"])
        })
        .count()
}

/// The card as a section of the page: its stage in the heading, the sizes or the reason on the
/// title row, what it learnt and kept under it, and once done how many threads wait on you in Lost
/// and found; a click hands Claude all of it.
pub fn compact_section(card: &Value, compacting: &Value, waiting: usize) -> Option<Value> {
    if !card.is_object() {
        return None;
    }
    let stage = string(&card["stage"]);
    let keeping: Vec<String> = array(&card["keeping"])
        .iter()
        .map(|x| string(x).to_owned())
        .collect();
    let size = if truthy(&card["before"]) {
        short(num(&card["before"]))
    } else {
        String::new()
    };
    let join = |parts: Vec<String>| {
        parts
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let why = string(&card["why"]).to_owned();
    let (title, right, heading);
    let (mut body, mut detail, mut lede): (Vec<String>, Vec<String>, Vec<String>) =
        (vec![], vec![], vec![]);
    let mut picture = Value::Null;
    let mut loud = 0;
    let brief;
    match stage {
        "soon" => {
            title = join(vec![size, why]);
            right = format!("in {}s", card["left"]);
            heading = "Compacting soon";
            brief = title.clone();
        }
        "running" => {
            let usual = &compacting["usual"];
            let elapsed = if compacting["elapsed"].is_number() {
                num(&compacting["elapsed"])
            } else {
                num(&card["elapsed"])
            };
            right = join(vec![
                masthead::clock_text(elapsed),
                if truthy(usual) {
                    format!(
                        "usually {}–{}",
                        masthead::clock_text(num(&usual["low"])),
                        masthead::clock_text(num(&usual["high"]))
                    )
                } else {
                    String::new()
                },
            ]);
            title = join(vec![
                if size.is_empty() {
                    "Summarizing".into()
                } else {
                    format!("Summarizing {size}")
                },
                why,
            ]);
            body = keeping
                .iter()
                .take(3)
                .map(|x| format!("Keeping: {x}"))
                .collect();
            detail = keeping
                .iter()
                .skip(3)
                .map(|x| format!("Keeping: {x}"))
                .collect();
            heading = "Compacting";
            brief = title.clone();
        }
        "skipped" => {
            title = format!("Did not run: {}", string(&card["reason"]));
            right = String::new();
            heading = "Compaction skipped";
            brief = title.clone();
        }
        _ => {
            let money = card_payback(card);
            let usual = card["usual"].is_object().then(|| {
                (
                    num(&card["usual"]["median"]),
                    num(&card["usual"]["n"]) as usize,
                )
            });
            let after = truthy(&card["after"]).then(|| {
                format!(
                    "{}{}",
                    if card["measured"] == true { "" } else { "~" },
                    short(num(&card["after"]))
                )
            });
            title = match (size.is_empty(), &after) {
                (false, Some(after)) => format!("Context: {size} → {after} tokens."),
                (false, None) => format!("Context: {size} tokens."),
                _ => String::new(),
            };
            if let Some(m) = money.as_ref().filter(|m| m.need.is_some_and(|n| n != 0)) {
                lede = vec![format!(
                    "Each API call now sends {} fewer tokens.",
                    short(m.before - m.floor)
                )];
            }
            let told = payback_rows(&money);
            if let Some(m) = money.as_ref().filter(|m| m.need.is_some_and(|n| n != 0)) {
                picture = json!({
                    "kind": "payback",
                    "rows": PAYBACK_ROWS,
                    "facts": {
                        "at": card["at"],
                        "per": card["per_turn"],
                        "cost": m.cost,
                        "saving": m.saving,
                        "need": m.need,
                        "done": m.done,
                        "net": m.net,
                    },
                });
            }
            let lessons = array(&card["learnt"]).len();
            body = told.clone();
            body.extend(
                [
                    took_row(card["took"].as_f64(), usual),
                    kept_row(keeping.len(), lessons),
                    lost_row(waiting),
                ]
                .into_iter()
                .filter(|x| !x.is_empty()),
            );
            detail = keeping.iter().map(|x| format!("Kept: {x}")).collect();
            detail.extend(
                array(&card["learnt"])
                    .iter()
                    .map(|x| format!("Learnt: {}", string(x))),
            );
            if let Some(m) = money.as_ref().filter(|m| m.need.is_some_and(|n| n != 0)) {
                detail.push(format!(
                    "Cost: cache {} + summary {} + read {}.",
                    short(m.floor * 2.),
                    short(m.summary * 5.),
                    short(m.before * 0.1)
                ));
            }
            right = chrono::Local
                .timestamp_opt(num(&card["at"]) as i64, 0)
                .single()
                .map(|t| t.format("%H:%M").to_string())
                .unwrap_or_default();
            heading = "Compacted by CSR";
            loud = told.len();
            brief = told.first().cloned().unwrap_or_else(|| title.clone());
        }
    }
    let done = !["soon", "running", "skipped"].contains(&stage);
    let live = matches!(stage, "soon" | "running");
    let said = lede
        .iter()
        .filter(|_| done)
        .chain(&body)
        .chain(&detail)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let item = json!({
        "key": format!("compacted:{}", card["at"]),
        "title": if title.is_empty() { heading.to_owned() } else { title.clone() },
        "lines": 2,
        "plain_body": true,
        "body": body,
        "lede": if done { lede } else { vec![] },
        "loud": if done { loud } else { 0 },
        "whole_detail": done,
        "picture": if done { picture } else { Value::Null },
        "unrated": live,
        "detail": detail,
        "send": send::closed_send(
            &format!("CSR's compaction card ({}), quoted:", heading.to_lowercase()),
            &format!(
                "{}.{}",
                title.trim_end_matches('.'),
                if said.is_empty() { String::new() } else { format!(" {said}.") }
            ),
            CARD_SEND_MOST,
        ),
    });
    Some(json!({
        "name": "compacted",
        "title": heading,
        "right": right,
        "items": [item],
        "empty": "",
        "at": card["at"],
        "live": live,
        "brief": if done { brief } else { title },
    }))
}

/// The text's " · "-joined pieces packed into rows of `width`: a piece goes whole onto the row it
/// fits, else starts the next (one wider than a row wraps by its words). At most `most` rows;
/// only past them is the last one cut.
pub fn piece_rows(text: &str, width: usize, most: usize) -> Vec<String> {
    let mut rows: Vec<String> = vec![];
    for piece in text.split(" · ") {
        match rows.last_mut() {
            Some(last) if len(last) + 3 + len(piece) <= width => {
                last.push_str(" · ");
                last.push_str(piece);
            }
            _ => rows.extend(wrap(piece, width, most)),
        }
    }
    if rows.len() > most {
        rows.truncate(most);
        let cut = format!("{} …", rows[most - 1]);
        rows[most - 1] = fit(&cut, width);
    }
    rows
}

/// The plan (bold) and the story (dim) under the masthead's gauges; nothing when there is
/// neither. The plan wraps by its pieces, so its payback is never cut.
pub fn infinite_rows(info: &Value, width: usize) -> Vec<String> {
    if !info.is_object() {
        return vec![];
    }
    let mut rows: Vec<String> = if truthy(&info["plan"]) {
        piece_rows(string(&info["plan"]), width.saturating_sub(1), 3)
            .into_iter()
            .map(|row| format!(" {BOLD}{row}{OFF}"))
            .collect()
    } else {
        vec![]
    };
    if truthy(&info["story"]) {
        rows.push(format!(
            " {DIM}{}{OFF}",
            fit(string(&info["story"]), width.saturating_sub(1))
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(extra: Value) -> Value {
        let mut r = json!({"kind": "compact", "session": "s", "event": "compact", "at": 1000.});
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        r
    }

    #[test]
    fn test_a_countdown_nothing_followed_gives_no_card() {
        let none = json!([]);
        let r = record(json!({"event": "countdown", "ends_at": 990.}));
        assert!(compaction_card(&[r], &none, 1000., &none, &none, &none).is_null());
        let r = record(json!({"event": "countdown", "ends_at": 1012.5, "ctx_before": 200000}));
        let card = compaction_card(&[r], &none, 1000., &none, &none, &none);
        assert_eq!(
            (card["stage"].as_str(), card["left"].as_i64()),
            (Some("soon"), Some(13))
        );
        let section = compact_section(&card, &Value::Null, 0).unwrap();
        assert_eq!(section["title"], "Compacting soon");
        assert_eq!(section["right"], "in 13s");
        assert_eq!(section["items"][0]["title"], "200k");
        assert_eq!(section["live"], true);
        assert_eq!(section["items"][0]["unrated"], true);
    }

    #[test]
    fn test_a_running_compaction_shows_what_it_keeps_and_how_long_it_usually_takes() {
        let none = json!([]);
        let r = record(json!({
            "event": "start", "at": 990., "ctx_before": 190000, "why": "the context is full",
            "keep": {"running": ["a", "b"], "decisions": ["c", "d"]},
        }));
        let card = compaction_card(std::slice::from_ref(&r), &none, 1000., &none, &none, &none);
        assert_eq!(
            (card["stage"].as_str(), card["elapsed"].as_i64()),
            (Some("running"), Some(10))
        );
        let compacting = json!({"elapsed": 12, "usual": {"low": 60, "high": 150}});
        let section = compact_section(&card, &compacting, 0).unwrap();
        assert_eq!(section["title"], "Compacting");
        assert_eq!(section["right"], "0:12 · usually 1:00–2:30");
        let item = &section["items"][0];
        assert_eq!(item["title"], "Summarizing 190k · the context is full");
        assert_eq!(
            item["body"],
            json!(["Keeping: a", "Keeping: b", "Keeping: c"])
        );
        assert_eq!(item["detail"], json!(["Keeping: d"]));
        // a marker the compaction never ended is not a card
        assert!(compaction_card(&[r], &none, 1000. + 700., &none, &none, &none).is_null());
    }

    #[test]
    fn test_a_cancel_or_an_old_ending_takes_the_card_down() {
        let none = json!([]);
        let cancel = record(json!({"event": "cancel"}));
        assert!(compaction_card(&[cancel], &none, 1001., &none, &none, &none).is_null());
        let done = record(json!({"ctx_before": 200000, "floor": 70000}));
        assert!(!compaction_card(
            std::slice::from_ref(&done),
            &none,
            1000. + 1800.,
            &none,
            &none,
            &none
        )
        .is_null());
        assert!(compaction_card(&[done], &none, 1000. + 1801., &none, &none, &none).is_null());
    }

    #[test]
    fn test_a_skip_says_why_it_did_not_run() {
        let none = json!([]);
        let r = record(json!({"event": "skip", "reason": "an agent is running"}));
        let card = compaction_card(&[r], &none, 1010., &none, &none, &none);
        let section = compact_section(&card, &Value::Null, 0).unwrap();
        assert_eq!(section["title"], "Compaction skipped");
        assert_eq!(
            section["items"][0]["title"],
            "Did not run: an agent is running"
        );
        let other = record(json!({"event": "skip"}));
        let card = compaction_card(&[other], &none, 1010., &none, &none, &none);
        assert_eq!(card["reason"], "a hook said no");
    }

    #[test]
    fn test_before_the_first_request_the_payback_is_the_plans_guess() {
        let none = json!([]);
        let r = record(json!({"ctx_before": 274073, "floor": 70026, "ctx_after": 20881}));
        let card = compaction_card(&[r], &none, 1100., &none, &none, &none);
        assert_eq!(card["measured"], false);
        let money = card_payback(&card).unwrap();
        assert_eq!(money.done, 0);
        assert_eq!(money.need, Some(14));
        assert_eq!(
            payback_rows(&Some(money)),
            [
                "One-time cost: 272k tokens at full input price.",
                "Each call saves 20k. Paid back at call 14."
            ]
        );
        let section = compact_section(&card, &Value::Null, 2).unwrap();
        assert_eq!(section["items"][0]["title"], "Context: 274k → ~70k tokens.");
        assert_eq!(
            section["items"][0]["body"][2],
            "2 waiting in Lost and found."
        );
    }

    #[test]
    fn test_a_compaction_that_never_pays_back_says_so() {
        let money = Some(Money {
            before: 90000.,
            floor: 90000.,
            summary: 20000.,
            cost: 300000.,
            saving: 0.,
            need: None,
            done: 0,
            net: -300000.,
            earned: 0.,
        });
        assert_eq!(
            payback_rows(&money),
            [
                "This compaction does not pay back.",
                "The context did not get smaller."
            ]
        );
    }

    #[test]
    fn test_the_payback_rows_by_state() {
        let m = |done, net| {
            Some(Money {
                before: 200000.,
                floor: 70000.,
                summary: 20000.,
                cost: 267000.,
                saving: 13000.,
                need: Some(21),
                done,
                net,
                earned: 267000. + net,
            })
        };
        assert_eq!(
            payback_rows(&m(20, -7000.))[0],
            "Paid back: 20 of 21 calls. 1 call to go."
        );
        assert_eq!(
            payback_rows(&m(3, -228000.))[1],
            "Earned back: 39k of 267k (full input price)."
        );
        assert_eq!(
            payback_rows(&m(110, 2_000_000.)),
            [
                "Paid back at call 21. Now at call 110.",
                "Net saving: 2.0M tokens at full input price."
            ]
        );
    }

    #[test]
    fn test_the_took_row_drops_the_count_before_it_passes_the_cards_width() {
        assert_eq!(took_row(None, Some((87., 12))), "");
        assert_eq!(took_row(Some(89.), None), "Took 1:29.");
        assert_eq!(
            took_row(Some(89.), Some((87., 136))),
            "Took 1:29. Your /compact: 1:27 (median of 136)."
        );
        assert_eq!(
            took_row(Some(689.), Some((687., 1360))),
            "Took 11:29. Your /compact: 11:27 (median)."
        );
    }

    #[test]
    fn test_the_kept_row_counts_items_and_lessons() {
        assert_eq!(kept_row(0, 0), "");
        assert_eq!(kept_row(1, 0), "Kept 1 item. Click to read.");
        assert_eq!(kept_row(4, 3), "Kept 4 items, 3 lessons. Click to read.");
        assert_eq!(lost_row(0), "");
        assert_eq!(lost_row(3), "3 waiting in Lost and found.");
    }

    #[test]
    fn test_only_threads_not_set_aside_that_are_lost_or_open_and_waiting_count() {
        let threads = json!([
            {"hidden": null, "rank": 0, "waiting": true},
            {"hidden": "drop", "rank": 0, "waiting": true},
            {"hidden": null, "rank": 3, "waiting": true},
            {"hidden": null, "rank": 1, "waiting": false},
            {"hidden": null, "rank": 1, "waiting": true},
        ]);
        assert_eq!(lost_waiting(&threads), 2);
    }

    #[test]
    fn test_the_plan_wraps_by_its_pieces_and_the_story_is_cut_not_wrapped() {
        let text = "Compacting in 30s · pays back in 8 calls · trims 120k";
        assert_eq!(piece_rows(text, 60, 3), [text]);
        // a piece goes whole onto the row it fits, else starts the next
        assert_eq!(
            piece_rows(text, 40, 3),
            ["Compacting in 30s · pays back in 8 calls", "trims 120k"]
        );
        let info = json!({"plan": text, "story": "this session: 1 compaction by CSR"});
        let rows = infinite_rows(&info, 100);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].starts_with(" \x1b[1m"));
        assert!(rows[1].starts_with(" \x1b[2m"));
        assert!(infinite_rows(&Value::Null, 100).is_empty());
    }
}
