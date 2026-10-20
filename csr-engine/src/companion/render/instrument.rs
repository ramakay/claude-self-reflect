//! The single companion surface. Both renderers consume this composition, built from live feeds.
#![allow(clippy::too_many_arguments)]
use super::{
    instrument_gfx::*,
    text,
    theme::{Theme, Tokens},
};
use crate::companion::{agents, array, dreams, masthead, num, pyfmt::short, string};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Batch {
    Ready,
    Dreaming,
    Nothing,
    Empty,
}
#[derive(Clone, Debug)]
pub struct DreamCard {
    pub state: Batch,
    pub allowance: (usize, usize),
    pub handed: usize,
    pub top: Value,
    pub queue: Vec<Value>,
    pub budget: Value,
    pub look: Value,
    pub looks: usize,
    pub none: usize,
    pub carried: bool,
    pub confusing: Vec<Value>,
    pub should_be: Vec<Value>,
}
#[derive(Clone, Debug)]
pub struct Composition {
    pub empty_session: bool,
    pub learnt: Vec<String>,
    pub requests: Vec<(f64, f64)>,
    pub codex_week: Value,
    pub dream: DreamCard,
    pub story: String,
    pub story_age: String,
    pub shifts: Vec<Value>,
    pub figure: Value,
    pub tally: Value,
    pub agents: agents::Projection,
    pub river: Vec<(f64, String)>,
    pub notice: Option<String>,
    pub asked_away: bool,
    pub now: f64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Open {
    pub scroll: usize,
    pub agents: bool,
    pub figure: bool,
    pub changes: bool,
    pub threads: bool,
    pub dreams: bool,
}
fn observation(v: &Value) -> Vec<Value> {
    array(v)
        .iter()
        .filter(|x| !string(&x["line"]).is_empty() && !string(&x["evidence"]).is_empty())
        .take(2)
        .cloned()
        .collect()
}
impl Composition {
    pub fn build(data: &Value, s: &Value, now: f64) -> Self {
        let fed = dreams::feed(array(&data["dreams"]), &data["chapter"], string(&s["id"]));
        let mut pending = array(&fed["queue"]).to_vec();
        if !fed["star"].is_null() {
            pending.insert(0, fed["star"].clone());
        }
        pending.sort_by(|a, b| num(&b["at"]).total_cmp(&num(&a["at"])));
        let top = pending.first().cloned().unwrap_or(Value::Null);
        let missing = Value::Null;
        let look = if data["look"]["session"] == s["id"] {
            &data["look"]
        } else {
            &missing
        };
        // A dream queue can be empty in a busy session. Usage can arrive before its transcript.
        let empty_session =
            masthead::ctx_now(&data["claude_fields"], data["claude_at"].as_f64(), s)
                .0
                .is_none_or(|n| n <= 0.)
                && array(&s["main"]).is_empty()
                && array(&s["prompts"]).is_empty();
        let state = if !top.is_null() {
            Batch::Ready
        } else if matches!(string(&look["outcome"]), "dreaming" | "started") {
            Batch::Dreaming
        } else if look["outcome"] == "none" || !empty_session {
            Batch::Nothing
        } else {
            Batch::Empty
        };
        let records = array(&data["dream_records"]);
        let looks = records
            .iter()
            .filter(|r| {
                r["session"] == s["id"]
                    && r["kind"] == "look"
                    && !matches!(string(&r["outcome"]), "dreaming" | "started")
            })
            .count();
        let none = records
            .iter()
            .filter(|r| r["session"] == s["id"] && r["kind"] == "look" && r["outcome"] == "none")
            .count();
        let source = if top.is_null() { look } else { &top };
        let mut river: Vec<_> = array(&data["dreams"])
            .iter()
            .filter(|d| d["session"] == s["id"])
            .map(|d| {
                (
                    num(&d["at"]),
                    format!(
                        "☾ {}{}",
                        string(&d["line"]),
                        d["answer"]
                            .as_str()
                            .map(|a| format!(" · {}", dreams::answer_word(a)))
                            .unwrap_or_default()
                    ),
                )
            })
            .collect();
        river.extend(
            array(&s["compacts"])
                .iter()
                .map(|v| (num(v), "♻ Compacted".into())),
        );
        let raw_story = string(&data["story"]["now"]).trim();
        let stripped_story = ["Work stands at:", "Where the work stands:", "Story:"]
            .iter()
            .find_map(|prefix| raw_story.strip_prefix(prefix));
        let story = if let Some(stripped) = stripped_story {
            let mut chars = stripped.trim().chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        } else {
            raw_story.to_owned()
        };
        let age = data["story"]["at"]
            .as_f64()
            .map(|at| format!("refreshed {} ago", age_span(now - at)))
            .unwrap_or_default();
        let projection: agents::Projection =
            serde_json::from_value(data["agents"].clone()).unwrap_or_default();
        let shifts = array(&data["story_map"]["entries"])
            .iter()
            .map(|entry| {
                let mut entry = entry.clone();
                let place = array(&entry["after"])
                    .first()
                    .or_else(|| array(&entry["before"]).first())
                    .cloned()
                    .unwrap_or(Value::Null);
                for key in ["scope", "sym", "mark"] {
                    if entry[key].is_null() {
                        entry[key] = place[key].clone();
                    }
                }
                entry
            })
            .collect::<Vec<_>>();
        for shift in &shifts {
            river.push((
                num(&data["story_map"]["at"]),
                format!(
                    "{} {} · {}",
                    mark(shift),
                    string(&shift["scope"]),
                    string(&shift["now"])
                ),
            ));
        }
        for agent in &projection.agents {
            if let Some(start) = agent.started {
                river.push((start / 1000., format!("○ {} started", agent.description)));
            }
            if let Some(done) = &agent.completion {
                river.push((done.at / 1000., format!("✓ {} finished", agent.description)));
            }
        }
        river.extend(array(&data["threads"]).iter().map(|thread| {
            (
                num(&thread["last_seen"]),
                format!(
                    "↳ {} · {}",
                    string(&thread["text"]),
                    crate::companion::threads::thread_group(thread)
                ),
            )
        }));
        river.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut stamps: Vec<_> = array(&s["main"])
            .iter()
            .map(|r| num(&r[0]))
            .chain(array(&s["prompts"]).iter().map(num))
            .collect();
        stamps.sort_by(f64::total_cmp);
        let gap = stamps.windows(2).rev().find(|w| w[1] - w[0] > 3600.);
        let notice = gap
            .map(|w| {
                let since = w[0];
                let looks = records
                    .iter()
                    .filter(|r| {
                        r["kind"] == "look"
                            && num(&r["at"]) > since
                            && !matches!(string(&r["outcome"]), "started" | "dreaming")
                    })
                    .count();
                let fresh = array(&data["dreams"])
                    .iter()
                    .filter(|d| d["session"] == s["id"] && num(&d["at"]) > since)
                    .count();
                format!(
                    "Away {}. The dreamer looked {looks}×. {fresh} new what-if{}.",
                    masthead::span(w[1] - since),
                    if fresh == 1 { "" } else { "s" }
                )
            })
            .or_else(|| {
                let card = &data["infinite"]["card"];
                (card["stage"] == "done" && card["at"].as_f64().is_some_and(|at| now - at < 120.))
                    .then(|| {
                        let tally = &data["infinite"]["tally"];
                        let mut line = "Compacted.".to_owned();
                        if let Some(n) = tally["by_csr"].as_u64() {
                            line.push_str(&format!(" {n} by CSR."));
                        }
                        if let Some(n) = tally["payback"].as_f64().filter(|n| *n > 0.) {
                            line.push_str(&format!(" Saved ~{} so far.", short(n)));
                        }
                        line
                    })
            });
        Self {
            empty_session,
            learnt: array(&data["learnt"])
                .iter()
                .filter_map(|v| {
                    Some(format!(
                        "{} {}",
                        v[0].as_u64()?,
                        if v[0] == 1 && v[1] == "facts retired" {
                            "fact retired"
                        } else {
                            v[1].as_str()?
                        }
                    ))
                })
                .collect(),
            requests: super::cards::request_bars(s)
                .0
                .into_iter()
                .rev()
                .take(24)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            codex_week: data["codex"]["week"].clone(),
            dream: DreamCard {
                state,
                handed: array(&data["dreams"])
                    .iter()
                    .filter(|d| {
                        d["session"] == s["id"]
                            && matches!(string(&d["answer"]), "ask_claude" | "sent")
                    })
                    .count(),
                allowance: (
                    fed["left"].as_u64().unwrap_or(0) as usize,
                    fed["of"].as_u64().unwrap_or(0) as usize,
                ),
                carried: top["carried"] == true,
                top: top.clone(),
                queue: pending.into_iter().skip(1).collect(),
                budget: if top["budget"].is_object() {
                    top["budget"].clone()
                } else {
                    look["budget"].clone()
                },
                look: look.clone(),
                looks,
                none,
                confusing: observation(&source["confusing_claude"]),
                should_be: observation(&source["should_be"]),
            },
            story,
            story_age: age,
            shifts,
            figure: masthead::gauges_compacting(
                &data["claude_fields"],
                data["claude_at"].as_f64(),
                s,
                now,
                &data["limits"],
                &data["sts"],
                &data["compacting"],
            ),
            tally: if notice
                .as_deref()
                .is_some_and(|s| s.starts_with("Compacted."))
            {
                Value::Null
            } else {
                data["infinite"]["tally"].clone()
            },
            agents: projection,
            river,
            asked_away: gap.is_some_and(|w| num(&top["at"]) > w[0] && num(&top["at"]) < w[1]),
            notice,
            now,
        }
    }
    pub fn question(&self) -> String {
        match self.dream.state {
            Batch::Ready => string(&self.dream.top["line"]).into(),
            Batch::Dreaming => "Dreaming.".into(),
            Batch::Nothing | Batch::Empty => "The dreamer looks as you work.".into(),
        }
    }
}
fn mark(v: &Value) -> &str {
    match string(&v["mark"]) {
        "!" | "+" | "-" | "~" => string(&v["mark"]),
        _ => match string(&v["kind"]) {
            "risk" => "!",
            "added" => "+",
            "removed" => "-",
            _ => "~",
        },
    }
}
fn receipt(v: &Value) -> String {
    if let Some(r) = v["receipt"].as_str().filter(|s| !s.is_empty()) {
        return r.into();
    }
    array(&v["receipts"])
        .iter()
        .filter_map(Value::as_str)
        .find_map(|r| {
            r.strip_prefix("oid:")
                .map(|id| format!("⌗{}", id.chars().take(8).collect::<String>()))
        })
        .unwrap_or_else(|| "unverified".into())
}
fn budget(v: &Value) -> Option<String> {
    Some(format!(
        "{} of {}",
        short(v["spent"].as_f64()?),
        short(v["cap"].as_f64()?.max(0.))
    ))
}
pub fn shift_row(v: &Value, expanded: bool) -> String {
    let place = format!(
        "{}{}",
        string(&v["scope"]),
        v["sym"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!(" · {s}"))
            .unwrap_or_default()
    );
    if expanded {
        format!(
            "{} {place} — {} · {}",
            mark(v),
            string(&v["now"]),
            receipt(v)
        )
    } else {
        format!("{} {place}  {}", mark(v), receipt(v))
    }
}
fn age_span(seconds: f64) -> String {
    let seconds = seconds.max(0.);
    if seconds < 60. {
        format!("{seconds:.0}s")
    } else {
        masthead::span(seconds)
    }
}
fn status_line(c: &Composition) -> String {
    match c.dream.state {
        Batch::Nothing if c.dream.looks > 0 => format!(
            "The last look found nothing. {} of {} looks end this way.",
            c.dream.none, c.dream.looks
        ),
        Batch::Nothing if c.dream.look.is_object() => {
            "The dreamer looked; no open what-if remains.".into()
        }
        Batch::Nothing => "No open what-if yet.".into(),
        Batch::Dreaming => {
            let names = array(&c.dream.look["threads"])
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let age = c.dream.look["at"]
                .as_f64()
                .map(|at| format!("started {} ago", age_span(c.now - at)))
                .unwrap_or_default();
            if names.is_empty() {
                age
            } else {
                format!(
                    "Following {} threads: {} · {age}",
                    names.len(),
                    names.join(", ")
                )
            }
        }
        Batch::Empty => "It asks a what-if from your past sessions.".into(),
        _ => String::new(),
    }
}
fn meta(c: &Composition) -> String {
    let d = &c.dream;
    let mut bits = vec![];
    if let Some(conf) = d.top["confidence"].as_f64() {
        bits.push(format!("{conf:.2}"));
    }
    if let Some(thread) = d.top["thread"].as_str().filter(|s| !s.is_empty()) {
        bits.push(thread.into());
    }
    if d.top["receipt"].is_string() {
        bits.push(receipt(&d.top));
    }
    if !d.queue.is_empty() {
        bits.push(format!("+{} more", d.queue.len()));
    }
    if let Some(b) = budget(&d.budget) {
        bits.push(format!("dreamer {b}"));
    }
    if d.handed > 0 {
        bits.push(format!("{} asked Claude", d.handed));
    }
    if d.carried {
        bits.push("carried".into());
    }
    bits.join(" · ")
}
fn observation_title(item: &Value, should: bool) -> String {
    if !should {
        return "Confusing Claude".into();
    }
    match string(&item["kind"]) {
        "skill" => "Should be a skill",
        "memory" => "Should be a memory",
        "hook" => "Should be a hook",
        "CLAUDE.md" | "CLAUDE.md rule" | "claude_md" | "rule" => "Should be a CLAUDE.md rule",
        _ => "Should be kept",
    }
    .into()
}
fn evidence_receipt(item: &Value) -> String {
    let evidence = string(&item["evidence"]);
    let before_quote = evidence.split(':').next().unwrap_or(evidence);
    let turns = before_quote
        .split(|c: char| !c.is_ascii_digit())
        .filter(|n| !n.is_empty())
        .collect::<Vec<_>>();
    if turns.is_empty() {
        "evidence".into()
    } else {
        format!(
            "{} {}",
            if turns.len() == 1 { "turn" } else { "turns" },
            turns.join(", ")
        )
    }
}
fn extras(c: &Composition, counts: bool) -> Vec<String> {
    [(false, &c.dream.confusing), (true, &c.dream.should_be)]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(should, items)| {
            let title = observation_title(&items[0], should);
            if counts {
                format!("{title} · {}", items.len())
            } else {
                format!(
                    "{title}: {}",
                    items
                        .iter()
                        .map(|i| format!("{} · {}", string(&i["line"]), evidence_receipt(i)))
                        .collect::<Vec<_>>()
                        .join(" · ")
                )
            }
        })
        .collect()
}
fn csr_status(g: &Value) -> Option<(String, &str)> {
    let sev = g["sts"]["sev"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != "green")?;
    let down = array(&g["sts"]["inop"])
        .iter()
        .filter_map(|i| {
            let name = i[0].as_str()?.to_lowercase().replace('_', " ");
            let state = if string(&i[1]).starts_with("INHIBITED") {
                "off"
            } else {
                "down"
            };
            Some(format!("{name} {state}"))
        })
        .collect::<Vec<_>>();
    Some((
        if down.is_empty() {
            format!("CSR check {sev}")
        } else {
            format!("CSR: {}", down.join(", "))
        },
        sev,
    ))
}
fn payback(g: &Value) -> String {
    if g["compacting"].is_object() {
        let info = &g["compacting"];
        let mut line = format!("◷ Compacting · {:.0}s", num(&info["elapsed"]));
        if let Some(high) = info["usual"]["high"].as_f64().filter(|n| *n > 0.) {
            line.push_str(&format!(" of ~{high:.0}s"));
        }
        return line;
    }
    if !g["ctx"]["tokens"].as_f64().is_some_and(|n| n > 0.) {
        return String::new();
    }
    let mut line = string(&g["payback"]["text"]).to_owned();
    if let Some(alt) = array(&g["payback"]["forms"])
        .get(1)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        line.push_str(" · ");
        line.push_str(alt.strip_prefix("♻ ").unwrap_or(alt));
    }
    line
}
pub fn footer(c: &Composition) -> String {
    format!(
        "{}l changes  t threads  i figure  b theme  q close",
        if c.dream.queue.is_empty() {
            ""
        } else {
            "d queue  "
        }
    )
}
/// Keep each instrument or metadata fact intact; an oversized fact is omitted at tiny widths.
fn fact_rows(s: &str, cols: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for piece in s.split(" · ").filter(|piece| !piece.is_empty()) {
        if text::len(piece) > cols {
            continue;
        }
        if !row.is_empty() && text::len(&row) + 3 + text::len(piece) > cols {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push_str(" · ");
        }
        row.push_str(piece);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}
/// Semantic text rows, also retained beneath pixel placements for accessibility and exports.
pub fn text_rows(c: &Composition, cols: usize, rows: usize, open: Open) -> Vec<String> {
    rows_with_figure(c, cols, rows, open, true)
}
pub fn rows_with_figure(
    c: &Composition,
    cols: usize,
    rows: usize,
    open: Open,
    show_figure: bool,
) -> Vec<String> {
    let mut out = vec![];
    let cap = rows * 3 / 10;
    let wrap = |s: &str| text::wrap(s, cols.max(1), 1000);
    let mut collapsed_hero = false;
    let mut hero = wrap(&format!("☾ {}", c.question()));
    if hero.len() > cap.saturating_sub(2) {
        hero = wrap(&c.question());
    }
    let metadata = meta(c);
    if !metadata.is_empty() {
        hero.extend(fact_rows(&metadata, cols));
    }
    if c.dream.state == Batch::Ready {
        hero.extend(fact_rows(
            "1 helpful · 2 thought of it · 3 not relevant · 4 ask Claude",
            cols,
        ));
    }
    let details = extras(c, false);
    let details_count = details.iter().flat_map(|s| wrap(s)).count();
    for line in extras(c, hero.len() + details_count > cap) {
        hero.extend(wrap(&line));
    }
    // Tiny terminals show the complete question in its drill-down. Never cut a rendered line.
    if hero.len() > cap {
        collapsed_hero = true;
        hero = wrap("☾ what-if · d");
        for line in extras(c, true) {
            hero.extend(wrap(&line));
        }
        if hero.len() > cap {
            hero = wrap("☾ d");
            if !c.dream.confusing.is_empty() {
                hero.push(format!("! {}", c.dream.confusing.len()));
            }
            if !c.dream.should_be.is_empty() {
                hero.push(format!("↳ {}", c.dream.should_be.len()));
            }
            hero.truncate(cap);
        }
    }
    out.extend(hero);
    if c.dream.state != Batch::Ready {
        out.extend(wrap(&status_line(c)));
    }
    if c.asked_away {
        out.extend(wrap("asked while you were away"));
    }
    if !c.learnt.is_empty() {
        out.extend(fact_rows(&c.learnt.join(" · "), cols));
    }
    if let Some(n) = &c.notice {
        out.extend(wrap(n));
    }
    if c.empty_session {
        for line in [
            "Ask about past work. CSR finds it.",
            "Code changed? CSR warns Claude.",
            "CSR keeps the threads you dropped.",
        ] {
            out.extend(wrap(line));
        }
    }
    if !c.story.is_empty() {
        out.extend(fact_rows(&format!("Story · {}", c.story_age), cols));
        out.extend(wrap(first_sentence(&c.story)));
    }
    let mut details_start = out.len();
    for v in c
        .shifts
        .iter()
        .take(if open.changes { usize::MAX } else { 3 })
    {
        out.extend(wrap(&shift_row(v, open.changes)));
        if open.changes && !string(&v["was"]).is_empty() {
            out.extend(wrap(&format!("Was: {}", string(&v["was"]))));
        }
    }
    if !open.changes {
        details_start = out.len();
    }
    if open.threads {
        for (at, line) in &c.river {
            out.extend(wrap(&format!(
                "{} · {line}",
                masthead::span((c.now - at).max(0.))
            )));
        }
    }
    if open.dreams {
        if collapsed_hero {
            out.extend(wrap(&c.question()));
        }
        out.extend(wrap(string(&c.dream.top["text"])));
        for line in extras(c, false) {
            out.extend(wrap(&line));
        }
        for item in c.dream.confusing.iter().chain(&c.dream.should_be) {
            out.extend(wrap(string(&item["evidence"])));
        }
        for d in &c.dream.queue {
            out.extend(wrap(string(&d["line"])));
        }
    }
    if open.agents {
        for line in agent_lines(c) {
            out.extend(wrap(&line));
        }
    }
    if open.changes || open.threads || open.dreams || open.agents {
        let details = out.drain(details_start..).collect::<Vec<_>>();
        let figure_rows = if show_figure {
            figure_lines(c)
                .iter()
                .flat_map(|line| fact_rows(line, cols))
                .count()
        } else {
            4
        };
        let reserved = figure_rows
            + 1
            + if show_figure { 0 } else { 7 }
            + usize::from(c.agents.agents.iter().any(agents::is_active));
        let room = rows.saturating_sub(out.len() + reserved);
        out.extend(details.into_iter().skip(open.scroll).take(room));
    }
    if show_figure {
        for line in figure_lines(c) {
            out.extend(fact_rows(&line, cols));
        }
    } else {
        let mut extra = vec![];
        if let Some(n) = c.codex_week["percent"].as_f64() {
            extra.push(format!("Codex 7D {n:.0}%"));
        }
        if let Some((line, _)) = csr_status(&c.figure) {
            extra.push(format!("● {line}"));
        }
        if c.figure["cache"]["cold"] == true {
            let line = string(&c.figure["cache"]["long"]);
            if let Some(line) = line.strip_prefix("COLD · ") {
                extra.push(line.into());
            }
        }
        if let Some(alternate) = array(&c.figure["payback"]["forms"])
            .get(1)
            .and_then(Value::as_str)
        {
            extra.push(alternate.strip_prefix("♻ ").unwrap_or(alternate).into());
        }
        if !extra.is_empty() {
            out.extend(fact_rows(&extra.join(" · "), cols));
        }
    }
    let active: Vec<_> = c
        .agents
        .agents
        .iter()
        .filter(|a| agents::is_active(a))
        .collect();
    if !active.is_empty() {
        out.extend(wrap(&format!(
            "{} in flight · {} stalled",
            active.len(),
            active.iter().filter(|a| agents::stalled(a, c.now)).count()
        )));
    }
    out
}
fn agent_lines(c: &Composition) -> Vec<String> {
    c.agents
        .agents
        .iter()
        .map(|a| {
            let minutes = a
                .started
                .map(|s| format!(" · {:.0}m", ((c.now * 1000. - s) / 60000.).max(0.)))
                .unwrap_or_default();
            format!(
                "○ {} · {}{minutes} · {} tools",
                a.description,
                if agents::stalled(a, c.now) {
                    "stalled"
                } else {
                    &a.status
                },
                a.tools
            )
        })
        .collect()
}
fn first_sentence(s: &str) -> &str {
    s.find(". ").map_or(s, |i| &s[..=i])
}
fn figure_lines(c: &Composition) -> Vec<String> {
    let g = &c.figure;
    let mut top = vec![];
    if let Some(n) = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.) {
        top.push(format!(
            "CTX {}{}",
            short(n),
            g["ctx"]["size"]
                .as_f64()
                .map(|s| format!("/{}", short(s)))
                .unwrap_or_default()
        ));
        if let Some(t) = g["trend"].as_f64() {
            top.push(format!(
                "{}{}/turn",
                if t >= 0. { "+" } else { "−" },
                short(t.abs())
            ));
        }
    }
    let pay = payback(g);
    if !pay.is_empty() {
        top.push(pay);
    }
    if let Some(n) = c.tally["by_csr"].as_u64().filter(|n| *n > 0) {
        top.push(format!("{n} by CSR"));
    }
    let mut quotas: Vec<_> = array(&g["quotas"])
        .iter()
        .filter(|q| q["used"].is_number())
        .map(text::paced)
        .collect();
    if g["cache"]["left"].is_number() || g["cache"]["cold"] == true {
        quotas.push(string(&g["cache"]["text"]).replace("CACHE ", "◷ "));
    }
    if let Some(n) = c.codex_week["percent"].as_f64() {
        quotas.push(format!("Codex 7D {n:.0}%"));
    }
    if let Some((line, _)) = csr_status(g) {
        quotas.push(format!("● {line}"));
    }
    [top.join(" · "), quotas.join(" · ")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect()
}

// The geometry and primitives below are the approved B prototype's glass, serif and ring design.
const M: f32 = 16.;
const P: f32 = 22.;
const CW: f32 = W - 2. * M;
const IX: f32 = M + P;
const IW: f32 = CW - 2. * P;
fn label(c: &mut Canvas, f: &Fonts, s: &str, x: f32, y: f32, size: f32, col: Rgba, w: f32) {
    c.text(f, s, x, y, sans(size, w), col, A::L);
}
fn panel(c: &mut Canvas, y: f32, h: f32, t: &Tokens) {
    c.glass(
        M,
        y,
        CW,
        h,
        28.,
        lin(
            0.,
            y,
            0.,
            y + h,
            &[(t.linear_at[0], t.glass[0]), (t.linear_at[1], t.glass[1])],
        ),
    );
    c.rrect_line(M, y, CW, h, 28., t.hair, t.hair_width);
}
fn single(
    c: &mut Canvas,
    f: &Fonts,
    s: &str,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    col: Rgba,
    t: &Tokens,
) {
    let st = sans(size, t.regular);
    let s = f.fit(s, st, width);
    c.text(f, &s, x, y, st, col, A::L);
}
#[derive(Clone, Debug)]
pub struct Metrics {
    pub content_left: f32,
    pub content_width: f32,
    pub dream: f32,
    pub answer_y: Option<f32>,
    pub answer_spans: Vec<(f32, f32, String)>,
    pub story_region: Option<(f32, f32)>,
    pub shift_hits: Vec<f32>,
    pub agents_region: Option<(f32, f32)>,
    pub figure_region: (f32, f32),
    pub height: f32,
    pub question_lines: Vec<String>,
    pub sections_collapsed: bool,
}
fn question_layout(f: &Fonts, q: &str, limit: f32, t: &Tokens) -> (St, Vec<String>) {
    let mut size = 24.;
    loop {
        let st = serif(size, t.medium);
        let lines = f.wrap(q, st, IW);
        if lines.len() <= 3 && (lines.len() as f32 * size * 1.28) <= limit {
            return (st, lines);
        }
        size -= 0.5;
        if size < 9. {
            return (st, lines);
        }
    }
}
fn fonts() -> Option<std::sync::MutexGuard<'static, Fonts>> {
    static FONTS: std::sync::LazyLock<std::sync::Mutex<Fonts>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Fonts::load()));
    FONTS.lock().ok()
}
/// Draw one actual pane image. No fixture-only renderer or invented data enters this path.
pub fn draw(
    c: &Composition,
    width: u32,
    available_height: u32,
    theme: Theme,
    open: Open,
    narrow: bool,
) -> Option<(tiny_skia::Pixmap, Metrics)> {
    if width < 160 || available_height < 120 {
        return None;
    }
    let fonts = fonts()?;
    let f = &*fonts;
    let t = theme.tokens();
    let ratio = width as f32 / (W * S);
    // Lay out the complete instrument before fitting both dimensions. Wide, short panes get
    // centered side margins rather than an undersized hero budget and a text-only fallback.
    let maxh = (available_height as f32 / (S * ratio)).max(800.);
    let learn_h = if c.learnt.is_empty() { 0. } else { 20. };
    let story_support = if c.story.is_empty() && c.shifts.is_empty() {
        if c.learnt.is_empty() {
            0.
        } else {
            P * 2. + learn_h + 12.
        }
    } else if c.shifts.is_empty() {
        P + 38. + P + 12.
    } else {
        P + 62. + c.shifts.len().min(3).saturating_sub(1) as f32 * 16. + P + 12.
    };
    let figure_support = if c.figure["ctx"]["tokens"].as_f64().is_none_or(|n| n <= 0.) {
        160.
    } else {
        360.
    };
    let support =
        M + 12. + story_support + 42. + figure_support + if c.empty_session { 94. } else { 0. };
    let hero_limit = (maxh * 0.30)
        .min((support * 3. / 7.).floor())
        .floor()
        .min(if c.empty_session { 136. } else { 240. });
    let has_extra = !c.dream.confusing.is_empty() || !c.dream.should_be.is_empty();
    let full_extras = extras(c, false);
    let collapsed = has_extra
        && full_extras
            .iter()
            .any(|s| f.width(s, sans(11.5, t.regular)) > IW * 2.);
    let extra_lines = extras(c, collapsed);
    let extras_h = extra_lines
        .iter()
        .map(|s| f.wrap(s, sans(11.5, t.regular), IW).len() as f32 * 16.)
        .sum::<f32>();
    let status = status_line(c);
    let status_lines = f.wrap(&status, sans(11.5, t.regular), IW);
    let status_h = if status.is_empty() {
        0.
    } else {
        status_lines.len() as f32 * 17.
    };
    let overhead = P * 2.
        + 22.
        + if c.dream.state == Batch::Ready {
            48.
        } else {
            12.
        }
        + extras_h
        + status_h;
    let (qs, question) = question_layout(f, &c.question(), (hero_limit - overhead).max(25.), t);
    let qh = question.len() as f32 * qs.size * 1.28;
    let hero_h = overhead + qh;
    if hero_h > hero_limit {
        return None;
    }
    let story_h = if c.story.is_empty() && c.shifts.is_empty() {
        if c.learnt.is_empty() {
            0.
        } else {
            P * 2.
        }
    } else {
        if c.shifts.is_empty() {
            P + 38. + P
        } else {
            P + 62. + c.shifts.len().min(3).saturating_sub(1) as f32 * 16. + P
        }
    };
    let story_h = if story_h > 0. {
        story_h + learn_h
    } else {
        story_h
    };
    let active = c.agents.agents.iter().any(agents::is_active);
    let agents_h = if active { 52. } else { 0. };
    let requested_extra_h = if open.changes {
        c.shifts.len() as f32 * 58.
    } else if open.threads {
        (c.river.len().min(8) as f32 * 33.).max(60.)
    } else if open.dreams {
        c.dream.queue.len().min(4) as f32 * 38.
    } else {
        0.
    };
    let empty_help = if c.empty_session { 94. } else { 0. };
    let mut drill_lines = vec![];
    if open.changes {
        for v in &c.shifts {
            drill_lines.extend(f.wrap(&shift_row(v, true), sans(11.5, t.regular), IW));
            if !string(&v["was"]).is_empty() {
                drill_lines.extend(f.wrap(
                    &format!("Was: {}", string(&v["was"])),
                    sans(11.5, t.regular),
                    IW,
                ));
            }
        }
    } else if open.agents {
        for line in agent_lines(c) {
            drill_lines.extend(f.wrap(&line, sans(11.5, t.regular), IW));
        }
    } else if open.dreams {
        drill_lines.extend(f.wrap(string(&c.dream.top["text"]), sans(11.5, t.regular), IW));
        for s in extras(c, false) {
            drill_lines.extend(f.wrap(&s, sans(11.5, t.regular), IW));
        }
        for item in c.dream.confusing.iter().chain(&c.dream.should_be) {
            drill_lines.extend(f.wrap(string(&item["evidence"]), sans(11.5, t.regular), IW));
        }
        for d in &c.dream.queue {
            drill_lines.extend(f.wrap(string(&d["line"]), sans(11.5, t.regular), IW));
        }
    }
    let notice_h = if c.notice.is_some() { 38. } else { 0. };
    let notice_gap = if c.notice.is_some() { 12. } else { 0. };
    let base = M
        + hero_h
        + 12.
        + story_h
        + if story_h > 0. { 12. } else { 0. }
        + agents_h
        + notice_h
        + notice_gap
        + empty_help
        + 42.;
    let available_extra = (maxh - base - 110.).max(0.);
    let extra_h = if open.threads {
        requested_extra_h.min(available_extra)
    } else {
        let lines = drill_lines
            .len()
            .saturating_sub(open.scroll)
            .min(((available_extra - P * 2.).max(0.) / 17.) as usize);
        if lines == 0 {
            0.
        } else {
            lines as f32 * 17. + P * 2.
        }
    };
    let fixed = base + extra_h;
    let figure_h = if c.figure["ctx"]["tokens"].as_f64().is_none_or(|n| n <= 0.) {
        (maxh - fixed).clamp(110., 160.)
    } else {
        (maxh - fixed).clamp(110., 360.)
    };
    let height = (fixed + figure_h).min(maxh);
    let mut canvas = Canvas::new(W, height);
    canvas.pm.fill(tiny_skia::Color::from_rgba8(
        t.ground[0],
        t.ground[1],
        t.ground[2],
        t.ground[3],
    ));
    let full = rrect_path(0., 0., W, height, 0.);
    canvas.fill_shader(&full, radial(W * 0.82, 40., 260., 1.4, 1., t.aurora[0]));
    let figure_bottom = M
        + hero_h
        + 12.
        + notice_h
        + notice_gap
        + story_h
        + if story_h > 0. { 12. } else { 0. }
        + extra_h
        + figure_h;
    canvas.fill_shader(
        &full,
        radial(
            W / 2.,
            figure_bottom,
            figure_h * 1.05,
            1.25,
            1.,
            t.aurora[1],
        ),
    );
    canvas.fill_shader(
        &full,
        radial(
            W / 2.,
            figure_bottom + 10.,
            figure_h * 0.62,
            1.5,
            0.8,
            t.aurora[2],
        ),
    );
    canvas.fill_shader(
        &full,
        radial(
            W / 2.,
            figure_bottom + 30.,
            figure_h * 0.34,
            2.,
            0.7,
            t.aurora[3],
        ),
    );
    let mut y = M;
    panel(&mut canvas, y, hero_h, t);
    label(
        &mut canvas,
        f,
        if c.dream.state == Batch::Ready {
            if c.asked_away {
                "The dreamer asks · asked while you were away"
            } else {
                "The dreamer asks"
            }
        } else {
            "CSR's dreamer"
        },
        IX,
        y + P,
        13.,
        t.ink,
        t.bold,
    );
    if c.dream.carried {
        label(
            &mut canvas,
            f,
            "CARRIED",
            IX + 132.,
            y + P,
            9.5,
            t.dream,
            t.bold,
        );
    }
    canvas.circle(W - 54., y + P + 2., 15., t.chip);
    canvas.text(
        f,
        "☾",
        W - 54.,
        y + P + 8.,
        sans(18., t.medium),
        t.dream,
        A::M,
    );
    let (left, total) = c.dream.allowance;
    if total > 0 {
        canvas.dots(W - 54., y + P + 2., 21., total.min(24), 1.5, &|i| {
            if i < left {
                t.dream
            } else {
                t.band
            }
        });
    }
    let mut b = y + P + 24. + f.cap(qs);
    for line in &question {
        canvas.text(f, line, IX, b, qs, t.ink, A::L);
        b += qs.size * 1.28;
    }
    b += 2.;
    let metadata = meta(c);
    if !metadata.is_empty() {
        let parts = metadata
            .split(" · ")
            .filter(|p| *p != "carried")
            .collect::<Vec<_>>();
        let mut size = 11.5;
        while parts
            .iter()
            .map(|p| {
                f.width(
                    p,
                    if p.starts_with('⌗') {
                        mono(size, t.medium)
                    } else {
                        sans(size, t.regular)
                    },
                ) + 12.
                    + if p.starts_with("dreamer ") { 34. } else { 0. }
            })
            .sum::<f32>()
            > IW - 18.
            && size > 8.
        {
            size -= 0.5;
        }
        let mut x = IX
            + if c.dream.top["confidence"].is_number() {
                18.
            } else {
                0.
            };
        if let Some(conf) = c.dream.top["confidence"].as_f64() {
            canvas.ring(
                IX + 5.,
                b - 4.,
                5.,
                t.thin_ring,
                t.track,
                &[(0., conf.clamp(0., 1.) as f32, t.dream)],
            );
        }
        for part in parts {
            if part.starts_with("dreamer ") {
                let spent = num(&c.dream.budget["spent"]);
                let cap = num(&c.dream.budget["cap"]).max(1.);
                canvas.rrect(x, b - 5., 28., 3., 1.5, t.track);
                canvas.rrect(
                    x,
                    b - 5.,
                    28. * (spent / cap).clamp(0., 1.) as f32,
                    3.,
                    1.5,
                    t.dream,
                );
                x += 34.;
                canvas.text(f, "dreamer ", x, b, sans(size, t.regular), t.quiet, A::L);
                x += f.width("dreamer ", sans(size, t.regular));
                let numbers = part.strip_prefix("dreamer ").unwrap();
                x += canvas.text(f, numbers, x, b, sans(size, t.regular), t.ink, A::L) + 12.;
                continue;
            }
            let st = if part.starts_with('⌗') {
                mono(size, t.medium)
            } else {
                sans(size, t.regular)
            };
            let w = f.width(part, st);
            if part.starts_with('⌗') {
                canvas.rrect(x - 4., b - 11., w + 8., 16., 8., t.chip);
            }
            canvas.text(
                f,
                part,
                x,
                b,
                st,
                if part.starts_with('⌗') {
                    t.dream
                } else {
                    t.quiet
                },
                A::L,
            );
            x += w + 12.;
        }
        b += 19.;
    }
    let mut answer_spans = vec![];
    let answer_y = (c.dream.state == Batch::Ready).then_some(b);
    if c.dream.state == Batch::Ready {
        let mut x = IX;
        for (label, answer) in [
            ("1 ✓ helpful", "helpful"),
            ("2 = thought of it", "thought_of_it"),
            ("3 ✗ not relevant", "not_relevant"),
            ("4 → ask Claude", "ask_claude"),
        ] {
            let w = canvas.text(
                f,
                label,
                x,
                b,
                sans(11.5, t.medium),
                if answer == "ask_claude" {
                    t.asked
                } else {
                    t.quiet
                },
                A::L,
            );
            answer_spans.push((x, x + w, answer.into()));
            x += w + 16.;
        }
        b += 21.;
    }
    if !status.is_empty() {
        for line in &status_lines {
            label(&mut canvas, f, line, IX, b, 11.5, t.quiet, t.regular);
            b += 17.;
        }
    }
    for line in extra_lines {
        let (title, body) = line.split_once(": ").unwrap_or((&line, ""));
        let st = sans(11.5, t.regular);
        let title = format!("{title}:");
        // Measure the label at its actual weight, then add one body-space advance.
        let tw = f.width(&title, sans(11.5, t.medium)) + f.width(" ", st);
        label(&mut canvas, f, &title, IX, b, 11.5, t.dream, t.medium);
        let lines = f.wrap(body, st, IW - tw);
        for (i, row) in lines.iter().enumerate() {
            // Evidence is a small mono receipt; the full quote stays in the reasons drill-down.
            let (body, receipt) = row.split_once(" · ").unwrap_or((row, ""));
            label(
                &mut canvas,
                f,
                body,
                IX + if i == 0 { tw } else { 0. },
                b,
                11.5,
                t.ink,
                t.regular,
            );
            if !receipt.is_empty() {
                let x = IX + if i == 0 { tw } else { 0. } + f.width(body, st) + 6.;
                let rw = f.width(receipt, mono(9.5, t.medium));
                canvas.rrect(x - 3., b - 10., rw + 6., 14., 5., t.chip);
                canvas.text(f, receipt, x, b, mono(9.5, t.medium), t.quiet, A::L);
            }
            b += 16.;
        }
    }
    y += hero_h + 12.;
    if let Some(notice) = &c.notice {
        panel(&mut canvas, y, notice_h, t);
        single(&mut canvas, f, notice, IX, y + 24., IW, 12., t.ink, t);
        y += notice_h + notice_gap;
    }
    let mut shift_hits = vec![];
    let story_region = (story_h > 0.).then_some((y, story_h));
    if story_h > 0. {
        panel(&mut canvas, y, story_h, t);
        label(
            &mut canvas,
            f,
            if c.story.is_empty() && c.shifts.is_empty() {
                "This session"
            } else {
                "Where the work stands"
            },
            IX,
            y + P,
            13.,
            t.ink,
            t.bold,
        );
        single(
            &mut canvas,
            f,
            &c.story_age,
            IX,
            y + P + 17.,
            IW,
            11.,
            t.quiet,
            t,
        );
        single(
            &mut canvas,
            f,
            first_sentence(&c.story),
            IX,
            y + P + 38.,
            IW,
            12.5,
            t.ink,
            t,
        );
        if !c.learnt.is_empty() {
            single(
                &mut canvas,
                f,
                &c.learnt.join(" · "),
                IX,
                y + P
                    + if c.story.is_empty() && c.shifts.is_empty() {
                        20.
                    } else {
                        58.
                    },
                IW,
                12.,
                t.mint,
                t,
            );
        }
        for (k, v) in c.shifts.iter().take(3).enumerate() {
            shift_hits.push(y + P + 62. + learn_h + k as f32 * 16. - 5.);
            single(
                &mut canvas,
                f,
                &shift_row(v, false),
                IX,
                y + P + 62. + learn_h + k as f32 * 16.,
                IW,
                10.5,
                if mark(v) == "!" { t.risk } else { t.mint },
                t,
            );
        }
        y += story_h + 12.;
    }
    if extra_h > 0. {
        panel(&mut canvas, y, extra_h, t);
        let mut b = y + P;
        if open.changes || open.dreams || open.agents {
            for line in drill_lines
                .iter()
                .skip(open.scroll)
                .take(((extra_h - P * 2.) / 17.).round() as usize)
            {
                label(&mut canvas, f, line, IX, b, 11.5, t.ink, t.regular);
                b += 17.;
            }
        } else if open.threads {
            let spine_h = (extra_h - P * 2.).max(0.);
            canvas.vhair(IX + 55., y + P, spine_h, t.band);
            if c.requests.len() > 1 {
                let max = c.requests.iter().map(|(a, b)| a + b).fold(1., f64::max);
                let mut path = tiny_skia::PathBuilder::new();
                path.move_to(IX + 55., y + P);
                for (i, (cached, fresh)) in c.requests.iter().rev().enumerate() {
                    path.line_to(
                        IX + 55. - ((cached + fresh) / max * 36.) as f32,
                        y + P + i as f32 / (c.requests.len() - 1) as f32 * spine_h,
                    );
                }
                path.line_to(IX + 55., y + P + spine_h);
                path.close();
                if let Some(path) = path.finish() {
                    canvas.fill_shader(
                        &path,
                        lin(
                            IX + 19.,
                            y + P,
                            IX + 55.,
                            y + P + spine_h,
                            &[(t.linear_at[0], t.mint), (t.linear_at[1], t.sky)],
                        ),
                    );
                }
            }
            for (at, line) in c
                .river
                .iter()
                .skip(open.scroll)
                .take(((extra_h - P * 2.).max(0.) / 33.) as usize)
            {
                canvas.circle(IX + 55., b - 4., 3., t.dream);
                single(
                    &mut canvas,
                    f,
                    &masthead::span((c.now - at).max(0.)),
                    IX,
                    b,
                    48.,
                    9.5,
                    t.quiet,
                    t,
                );
                single(&mut canvas, f, line, IX + 70., b, IW - 70., 11.5, t.ink, t);
                b += 33.;
            }
        } else {
            for d in c.dream.queue.iter().take(4) {
                single(&mut canvas, f, string(&d["line"]), IX, b, IW, 12., t.ink, t);
                b += 38.;
            }
        }
        y += extra_h;
    }
    let figure_region = (y, figure_h);
    figure(&mut canvas, f, c, y, figure_h, t, open.figure, narrow);
    y += figure_h + 12.;
    let agents_region = active.then_some((y, agents_h));
    if active {
        panel(&mut canvas, y, agents_h, t);
        let mut x = IX + 8.;
        let mut stalled = 0;
        for a in c
            .agents
            .agents
            .iter()
            .filter(|a| agents::is_active(a))
            .take(10)
        {
            let is_stalled = agents::stalled(a, c.now);
            stalled += usize::from(is_stalled);
            canvas.ring(
                x,
                y + 26.,
                8.,
                t.ring,
                t.track,
                &[(0., 0.86, if is_stalled { t.amber } else { t.mint })],
            );
            x += 27.;
        }
        let live = c
            .agents
            .agents
            .iter()
            .find(|a| agents::is_active(a) && !agents::stalled(a, c.now))
            .map(|a| a.description.as_str())
            .unwrap_or("");
        single(
            &mut canvas,
            f,
            &format!("{stalled} stalled · {live}"),
            x + 8.,
            y + 30.,
            W - x - 40.,
            11.5,
            t.amber,
            t,
        );
        y += agents_h;
    }
    if empty_help > 0. {
        panel(&mut canvas, y, empty_help, t);
        for (k, s) in [
            "Ask about past work. CSR finds it.",
            "Code changed? CSR warns Claude.",
            "CSR keeps the threads you dropped.",
        ]
        .iter()
        .enumerate()
        {
            label(
                &mut canvas,
                f,
                s,
                IX,
                y + P + k as f32 * 22.,
                12.,
                t.quiet,
                t.regular,
            );
        }
    }
    let raw = image::RgbaImage::from_raw(
        canvas.pm.width(),
        canvas.pm.height(),
        canvas.pm.data().to_vec(),
    )?;
    let scale = (width as f32 / W).min(available_height as f32 / height);
    let fitted_width = (W * scale).round().min(width as f32) as u32;
    let fitted_height = (height * scale).round().min(available_height as f32) as u32;
    let fitted = image::imageops::resize(
        &raw,
        fitted_width,
        fitted_height,
        image::imageops::FilterType::Lanczos3,
    );
    let left = (width - fitted_width) / 2;
    let mut output = image::RgbaImage::from_pixel(width, fitted_height, image::Rgba(t.ground));
    image::imageops::overlay(&mut output, &fitted, i64::from(left), 0);
    let pm = tiny_skia::Pixmap::from_vec(
        output.into_raw(),
        tiny_skia::IntSize::from_wh(width, fitted_height)?,
    )?;
    Some((
        pm,
        Metrics {
            content_left: left as f32 / width as f32,
            content_width: fitted_width as f32 / width as f32,
            dream: hero_h,
            answer_y,
            answer_spans,
            story_region,
            shift_hits,
            agents_region,
            figure_region,
            height,
            question_lines: question,
            sections_collapsed: collapsed,
        },
    ))
}
fn figure(
    c: &mut Canvas,
    f: &Fonts,
    data: &Composition,
    y: f32,
    h: f32,
    t: &Tokens,
    expanded: bool,
    narrow: bool,
) {
    panel(c, y, h, t);
    let g = &data.figure;
    if narrow || (h < 250. && g["ctx"]["tokens"].as_f64().is_some_and(|n| n > 0.)) {
        let lines = figure_lines(data);
        let ctx = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
        let mut x = IX;
        if let (Some(n), Some(size)) = (ctx, g["ctx"]["size"].as_f64().filter(|n| *n > 0.)) {
            let (cx, cy) = (IX + 28., y + h / 2.);
            let mut segs = vec![(0., (n / size).clamp(0., 1.) as f32, t.mint)];
            if let (Some(low), Some(high)) = (g["band"]["low"].as_f64(), g["band"]["high"].as_f64())
            {
                segs.push(((low / size) as f32, (high / size) as f32, t.band));
            }
            c.ring(cx, cy, 25., t.ring, t.track, &segs);
            c.text(f, &short(n), cx, cy, serif(14., t.medium), t.ink, A::M);
            c.text(
                f,
                &format!("of {}", short(size)),
                cx,
                cy + 13.,
                sans(8., t.regular),
                t.quiet,
                A::M,
            );
            x += 72.;
        }
        for (k, line) in lines.iter().enumerate() {
            let line = if k == 0 && ctx.is_some() {
                line.split(" · ").skip(1).collect::<Vec<_>>().join(" · ")
            } else {
                line.clone()
            };
            single(
                c,
                f,
                &line,
                x,
                y + P + 10. + k as f32 * 24.,
                W - x - P - M,
                11.5,
                t.ink,
                t,
            );
        }
        for (i, q) in array(&g["quotas"])
            .iter()
            .filter(|q| q["used"].is_number())
            .enumerate()
        {
            let bx = x + i as f32 * 100.;
            c.rrect(bx, y + h - 17., 80., 3., 1.5, t.track);
            c.rrect(
                bx,
                y + h - 17.,
                80. * (num(&q["used"]) / 100.).clamp(0., 1.) as f32,
                3.,
                1.5,
                t.sky,
            );
        }
        return;
    }
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    let size = g["ctx"]["size"].as_f64().filter(|n| *n > 0.);
    label(
        c,
        f,
        if tokens.is_some() {
            "Context"
        } else {
            "Quotas"
        },
        IX,
        y + P,
        13.,
        t.ink,
        t.bold,
    );
    if let Some(trend) = g["trend"].as_f64().filter(|_| tokens.is_some()) {
        label(
            c,
            f,
            &format!(
                "{}{}/turn",
                if trend >= 0. { "+" } else { "−" },
                short(trend.abs())
            ),
            IX,
            if expanded { y + 210. } else { y + P + 17. },
            11.5,
            t.quiet,
            t.regular,
        );
    }
    if let Some(n) = data.tally["by_csr"].as_u64().filter(|n| *n > 0) {
        c.text(
            f,
            &format!("{n}"),
            W - 100.,
            y + P + 7.,
            serif(24., t.medium),
            t.ink,
            A::M,
        );
        label(c, f, "by CSR", W - 84., y + P + 7., 10., t.quiet, t.regular);
    }
    if let Some((line, sev)) = csr_status(g) {
        let col = if sev == "red" { t.risk } else { t.amber };
        c.circle(IX + 87., y + P - 4., 2.5, col);
        single(c, f, &line, IX + 95., y + P, 230., 10., t.quiet, t);
    }
    let cy = y + 126.;
    if let (Some(n), Some(size)) = (tokens, size) {
        if expanded {
            let bars = &data.requests;
            let max = bars
                .iter()
                .map(|(cached, fresh)| cached + fresh)
                .fold(1., f64::max)
                .max(num(&g["band"]["high"]));
            let bw = IW / bars.len().max(1) as f32;
            if let (Some(low), Some(high)) = (g["band"]["low"].as_f64(), g["band"]["high"].as_f64())
            {
                let y1 = y + 195. - (high / max * 100.) as f32;
                let y2 = y + 195. - (low / max * 100.) as f32;
                c.rect(IX, y1, IW, (y2 - y1).max(0.), t.band);
            }
            for (i, (cached, fresh)) in bars.iter().enumerate() {
                let bh = ((cached + fresh) / max * 100.) as f32;
                let fh = (fresh / max * 100.) as f32;
                c.rect(IX + i as f32 * bw, y + 195. - bh, bw * 0.7, bh, t.sky);
                c.rect(IX + i as f32 * bw, y + 195. - bh, bw * 0.7, fh, t.fresh);
            }
            label(
                c,
                f,
                "pink = fresh input",
                W - 155.,
                y + 210.,
                9.,
                t.quiet,
                t.regular,
            );
            c.text(f, &short(n), IX, y + 60., serif(25., t.medium), t.ink, A::L);
            label(
                c,
                f,
                &format!("of {}", short(size)),
                IX + 72.,
                y + 60.,
                10.5,
                t.quiet,
                t.regular,
            );
        } else {
            let (cx, r) = (W / 2., 78.);
            c.dots(cx, cy, r + 12., 72, 0.65, &|_| t.band);
            let mut segs = vec![(0., (n / size).clamp(0., 1.) as f32, t.mint)];
            if let (Some(low), Some(high)) = (g["band"]["low"].as_f64(), g["band"]["high"].as_f64())
            {
                segs.push(((low / size) as f32, (high / size) as f32, t.band));
            }
            c.ring(cx, cy, r, t.ring, t.track, &segs);
            if let Some(trend) = g["trend"].as_f64() {
                c.tick(
                    cx,
                    cy,
                    r - 4.,
                    r + 5.,
                    ((n + trend) / size).clamp(0., 1.) as f32,
                    t.thin_ring,
                    t.mint,
                );
            }
            c.text(f, &short(n), cx, cy + 7., serif(44., t.medium), t.ink, A::M);
            c.text(
                f,
                &format!("of {}", short(size)),
                cx,
                cy + 28.,
                sans(10.5, t.medium),
                t.quiet,
                A::M,
            );
        }
    }
    let pay = payback(g);
    if g["compacting"].is_object() {
        if let Some(high) = g["compacting"]["usual"]["high"]
            .as_f64()
            .filter(|n| *n > 0.)
        {
            c.rrect(W / 2. - 60., y + h - 93., 120., 3., 1.5, t.track);
            c.rrect(
                W / 2. - 60.,
                y + h - 93.,
                120. * (num(&g["compacting"]["elapsed"]) / high).clamp(0., 1.) as f32,
                3.,
                1.5,
                t.mint,
            );
        }
    }
    if !pay.is_empty() {
        c.text(
            f,
            &pay,
            W / 2.,
            y + h - 103.,
            sans(13., t.medium),
            if g["payback"]["sev"] == "amber" {
                t.amber
            } else {
                t.mint
            },
            A::M,
        );
    }
    let mut satellites: Vec<_> = array(&g["quotas"])
        .iter()
        .filter(|q| q["used"].is_number())
        .collect();
    satellites.sort_by_key(|q| if q["code"] == "5H" { 0 } else { 1 });
    let codex = data.codex_week["percent"].as_f64();
    let cache = g["cache"]["left"].is_number() || g["cache"]["cold"] == true;
    let count = satellites.len() + usize::from(cache) + usize::from(codex.is_some());
    let step = IW / (count + 1) as f32;
    for (i, q) in satellites.iter().enumerate() {
        let x = IX + step * (i + 1) as f32;
        let cy = y + h - 53.;
        let v = num(&q["used"]);
        let col = t.sky;
        c.ring(
            x,
            cy,
            30.,
            t.ring,
            t.track,
            &[(0., (v / 100.).clamp(0., 1.) as f32, col)],
        );
        c.text(
            f,
            &format!("{v:.0}%"),
            x,
            cy + 4.,
            serif(18., t.medium),
            t.ink,
            A::M,
        );
        let pace = q["pace"].as_f64().is_some_and(|p| v / 100. > p);
        c.text(
            f,
            &format!("{}{}", string(&q["code"]), if pace { " »" } else { "" }),
            x,
            cy + 46.,
            sans(10., t.medium),
            if pace { t.dream } else { t.quiet },
            A::M,
        );
    }
    if let Some(left) = g["cache"]["left"]
        .as_f64()
        .or_else(|| (g["cache"]["cold"] == true).then_some(0.))
    {
        let x = IX + step * (satellites.len() + 1) as f32;
        let cy = y + h - 53.;
        let cold = g["cache"]["cold"] == true || left <= 0.;
        let ttl = g["cache"]["ttl"].as_f64().unwrap_or(3600.);
        c.ring(
            x,
            cy,
            30.,
            t.ring,
            t.track,
            &[(
                0.,
                (left / ttl).clamp(0., 1.) as f32,
                if cold { t.amber } else { t.mint },
            )],
        );
        c.text(
            f,
            &if cold {
                "COLD".into()
            } else {
                masthead::span(left)
            },
            x,
            cy + 4.,
            serif(18., t.medium),
            t.ink,
            A::M,
        );
        c.text(
            f,
            if cold {
                string(&g["cache"]["long"])
                    .strip_prefix("COLD · ")
                    .unwrap_or("◷ cache")
            } else {
                "◷ cache"
            },
            x,
            cy + 46.,
            sans(10., t.medium),
            t.quiet,
            A::M,
        );
    }
    if let Some(v) = codex {
        let x = IX + step * count as f32;
        let cy = y + h - 53.;
        c.ring(
            x,
            cy,
            30.,
            t.ring,
            t.track,
            &[(0., (v / 100.).clamp(0., 1.) as f32, t.asked)],
        );
        c.text(
            f,
            &format!("{v:.0}%"),
            x,
            cy + 4.,
            serif(18., t.medium),
            t.ink,
            A::M,
        );
        c.text(
            f,
            "Codex 7D",
            x,
            cy + 46.,
            sans(10., t.medium),
            t.quiet,
            A::M,
        );
    }
}

/// Click rows are derived from the same live composition used by both renderers.
pub fn acts(c: &Composition, cols: usize) -> BTreeMap<usize, Value> {
    let mut out = BTreeMap::new();
    if c.dream.state == Batch::Ready {
        let send = crate::companion::send::closed_send(
            &format!(
                "CSR's dreamer asked this while you worked, quoted: “{}” Its reasoning:",
                crate::companion::send::straight(string(&c.dream.top["line"]))
            ),
            &format!(
                "{}. {}",
                string(&c.dream.top["title"]),
                string(&c.dream.top["text"])
            ),
            700,
        );
        let row = text::wrap(&format!("☾ {}", c.question()), cols.max(1), 1000).len()
            + usize::from(!meta(c).is_empty());
        out.insert(row,json!({"key":format!("dream:{}",string(&c.dream.top["id"])),"dream":c.dream.top["id"],"send":send,
            "spans":[[0,cols/4,"helpful"],[cols/4,cols/2,"thought_of_it"],[cols/2,cols*3/4,"not_relevant"],[cols*3/4,cols,"ask_claude"]]}));
    }
    out
}

/// A's bottom instrument strip at the terminal's actual width. Body text stays in native cells.
pub fn strip(
    data: &Composition,
    width: u32,
    theme: Theme,
    expanded: bool,
) -> Option<tiny_skia::Pixmap> {
    let w = width as f32 / S;
    if w < 220. {
        return None;
    }
    let fonts = fonts()?;
    let f = &*fonts;
    let t = theme.tokens();
    let g = &data.figure;
    let h = 104.;
    let mut c = Canvas::new(w, h);
    c.pm.fill(tiny_skia::Color::from_rgba8(
        t.ground[0],
        t.ground[1],
        t.ground[2],
        t.ground[3],
    ));
    c.glass(
        0.,
        0.,
        w,
        h,
        16.,
        lin(
            0.,
            0.,
            0.,
            h,
            &[(t.linear_at[0], t.glass[0]), (t.linear_at[1], t.glass[1])],
        ),
    );
    let ctx = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    if let (Some(n), Some(size)) = (ctx, g["ctx"]["size"].as_f64().filter(|n| *n > 0.)) {
        if expanded && !data.requests.is_empty() {
            let max = data.requests.iter().map(|(a, b)| a + b).fold(1., f64::max);
            let bw = 48. / data.requests.len() as f32;
            for (i, (a, b)) in data.requests.iter().enumerate() {
                let bar = ((a + b) / max * 28.) as f32;
                c.rect(12. + i as f32 * bw, 58. - bar, bw * 0.7, bar, t.sky);
            }
        } else {
            let mut segs = vec![(0., (n / size).clamp(0., 1.) as f32, t.mint)];
            if let (Some(low), Some(high)) = (g["band"]["low"].as_f64(), g["band"]["high"].as_f64())
            {
                segs.push(((low / size) as f32, (high / size) as f32, t.band));
            }
            c.ring(36., 45., 24., t.ring, t.track, &segs);
        }
        c.text(f, &short(n), 36., 49., serif(14., t.medium), t.ink, A::M);
        c.text(
            f,
            &format!("of {}", short(size)),
            36.,
            if expanded { 95. } else { 65. },
            sans(8., t.regular),
            t.quiet,
            A::M,
        );
        if let Some(trend) = g["trend"].as_f64() {
            c.text(
                f,
                &format!(
                    "{}{}/turn",
                    if trend >= 0. { "+" } else { "−" },
                    short(trend.abs())
                ),
                76.,
                18.,
                sans(9., t.regular),
                t.quiet,
                A::L,
            );
        }
    }
    if let Some(n) = data.tally["by_csr"].as_u64().filter(|n| *n > 0) {
        c.text(
            f,
            &format!("{n} by CSR"),
            w - 10.,
            18.,
            sans(9., t.medium),
            t.ink,
            A::R,
        );
    }
    let pay = payback(g);
    if !pay.is_empty() {
        let st = sans(10., t.medium);
        c.text(f, &f.fit(&pay, st, w - 86.), 76., 37., st, t.mint, A::L);
    }
    let mut quotas = array(&g["quotas"])
        .iter()
        .filter(|q| q["used"].is_number())
        .collect::<Vec<_>>();
    quotas.sort_by_key(|q| if q["code"] == "5H" { 0 } else { 1 });
    let cache = g["cache"]["left"].is_number() || g["cache"]["cold"] == true;
    let count = quotas.len() + usize::from(cache);
    let step = (w - 76. - 10.) / count.max(1) as f32;
    for (i, q) in quotas.iter().enumerate() {
        let x = 76. + step * (i as f32 + 0.5);
        let used = num(&q["used"]);
        let accent = t.sky;
        c.ring(
            x,
            69.,
            16.,
            t.ring,
            t.track,
            &[(0., (used / 100.).clamp(0., 1.) as f32, accent)],
        );
        c.text(
            f,
            &format!("{used:.0}%"),
            x,
            73.,
            serif(10., t.medium),
            t.ink,
            A::M,
        );
        let pace = q["pace"].as_f64().is_some_and(|p| used / 100. > p);
        c.text(
            f,
            &format!("{}{}", string(&q["code"]), if pace { " »" } else { "" }),
            x,
            97.,
            sans(8., t.medium),
            t.quiet,
            A::M,
        );
    }
    if cache {
        let x = 76. + step * (count as f32 - 0.5);
        let cold = g["cache"]["cold"] == true;
        let left = num(&g["cache"]["left"]);
        let ttl = g["cache"]["ttl"]
            .as_f64()
            .filter(|t| *t > 0.)
            .unwrap_or(3600.);
        c.ring(
            x,
            69.,
            16.,
            t.ring,
            t.track,
            &[(
                0.,
                (left / ttl).clamp(0., 1.) as f32,
                if cold { t.amber } else { t.mint },
            )],
        );
        c.text(
            f,
            &if cold {
                "COLD".into()
            } else {
                masthead::span(left)
            },
            x,
            73.,
            serif(10., t.medium),
            t.ink,
            A::M,
        );
        c.text(f, "◷", x, 97., sans(8., t.medium), t.quiet, A::M);
    }
    Some(c.pm)
}
