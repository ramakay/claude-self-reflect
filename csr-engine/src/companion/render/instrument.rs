//! The single companion surface. Both renderers consume this composition, built from live feeds.
#![allow(clippy::too_many_arguments)]
use super::{
    instrument_gfx::*,
    instrument_icons::{self as icons, Icon},
    text,
    theme::{self, Theme, Tokens, ICON_GAP, S1, S2, S3, S4, S5},
};
use crate::companion::{
    agents, array, dreams, masthead, num, pyfmt::short, string, value::InstrumentCost,
};
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
    pub repo_recap: Option<crate::companion::repo_recap::RepoRecap>,
    pub request_compactions: Vec<usize>,
    /// Dreams are off in settings: show the one-press offer plus what CSR can do.
    pub offer: bool,
    /// Notice when enabling dreams from the offer failed.
    pub offer_notice: Option<String>,
    pub learnt: Vec<String>,
    pub requests: Vec<(f64, f64)>,
    pub codex_week: Value,
    pub csr_cost: InstrumentCost,
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
        let offer = data["dreams_on"].as_bool() == Some(false);
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
        // Only the latest return counts: an hour-long gap earlier in the session is old news.
        let gap = stamps.windows(2).last().filter(|w| w[1] - w[0] > 3600.);
        let notice = gap
            .and_then(|w| {
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
                // Nothing happened while away: say nothing rather than "looked 0×".
                (looks > 0 || fresh > 0).then(|| {
                    format!(
                        "Away {}. The dreamer looked {looks}×. {fresh} new what-if{}.",
                        masthead::span(w[1] - since),
                        if fresh == 1 { "" } else { "s" }
                    )
                })
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
            repo_recap: serde_json::from_value(data["repo_recap"].clone())
                .ok()
                .filter(|_: &crate::companion::repo_recap::RepoRecap| empty_session),
            request_compactions: recent_request_compactions(s),
            offer,
            offer_notice: data["offer_notice"].as_str().map(str::to_owned),
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
            csr_cost: InstrumentCost::from_week(&data["value"]),
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
        if self.offer {
            return "Turn dreams on.".into();
        }
        match self.dream.state {
            Batch::Ready => string(&self.dream.top["line"]).into(),
            Batch::Dreaming => "Dreaming.".into(),
            Batch::Nothing | Batch::Empty => "The dreamer looks as you work.".into(),
        }
    }
}

/// What CSR can do here: the sell lines under an empty session or the dreams-off offer.
fn sells() -> [&'static str; 3] {
    [
        "Ask about past work. CSR finds it.",
        "Code changed? CSR warns Claude.",
        "CSR keeps the threads you dropped.",
    ]
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
    if let Some(notice) = &c.offer_notice {
        return notice.clone();
    }
    // Offer CTA owns the enable instruction; status stays empty while the offer is up.
    if c.offer {
        return String::new();
    }
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
    let mut history_insert = None;
    let mut pitch_insert = None;
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
    if c.offer {
        out.extend(wrap("Press 1 to turn dreams on in all your sessions."));
    }
    if c.dream.state != Batch::Ready {
        let status = status_line(c);
        if !status.is_empty() {
            out.extend(wrap(&status));
        }
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
    if let Some(recap) = &c.repo_recap {
        out.extend(wrap(&format!(
            "Last time here · {} · {}",
            recap.age,
            recap.session.chars().take(8).collect::<String>()
        )));
        for line in recap.text.lines() {
            out.extend(wrap(line));
        }
        history_insert = Some(out.len());
    } else if c.empty_session || c.offer {
        for line in sells() {
            out.extend(wrap(line));
        }
        if c.empty_session {
            pitch_insert = Some(out.len());
        }
    }
    if !c.story.is_empty() {
        out.extend(fact_rows(&format!("Story · {}", c.story_age), cols));
        out.extend(story_text_lines(first_sentence(&c.story), cols, 4));
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
        if let Some(q) = array(&c.figure["quotas"])
            .iter()
            .find(|q| q["code"] == "7D" && q["used"].is_number())
        {
            extra.push(quota_caption(c, q));
        } else {
            extra.push(format!("CSR cost: {}", c.csr_cost.api_line()));
        }
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
    if let (Some(at), Some(recap)) = (history_insert, &c.repo_recap) {
        let room = rows.saturating_sub(1 + out.len());
        if room > 1 && !recap.history.is_empty() {
            let mut history = vec!["Earlier here".into()];
            history.extend(recap.history.iter().take(room - 1).map(|prior| {
                text::fit(
                    &format!(
                        "{} {} · {}",
                        history_mark(&prior.outcome),
                        prior.age,
                        prior.intent
                    ),
                    cols,
                )
            }));
            out.splice(at..at, history);
        }
    }
    if let Some(at) = pitch_insert {
        let mut room = rows.saturating_sub(1 + out.len());
        let mut examples = vec![];
        if room > 2 {
            examples.push("Try a question".into());
            room -= 1;
            for (question, hint) in starter_questions() {
                let lines = wrap(&format!("{question} {hint}"));
                if lines.len() > room {
                    break;
                }
                room -= lines.len();
                examples.extend(lines);
            }
            if examples.len() > 1 {
                out.splice(at..at, examples);
            }
        }
    }
    out
}
fn starter_questions() -> [(&'static str, &'static str); 8] {
    [
        (
            "What did we decide about this repo?",
            "Find a decision and its source session.",
        ),
        (
            "What was unfinished last time?",
            "Recover the next steps from earlier work.",
        ),
        (
            "Why did we change this file?",
            "Follow the conversation behind the code.",
        ),
        (
            "Where did we fix this bug before?",
            "Find earlier attempts and the final fix.",
        ),
        (
            "What did I ask Claude to remember?",
            "Look up saved instructions and preferences.",
        ),
        (
            "Show the history of this function.",
            "Find prior edits and their context.",
        ),
        (
            "What changed in the last session?",
            "Review the session's evidenced work.",
        ),
        (
            "Which approach did we reject?",
            "Retrieve alternatives and their reasons.",
        ),
    ]
}
fn history_mark(outcome: &str) -> &'static str {
    match outcome {
        "done" => "✓",
        "open" => "→",
        _ => "?",
    }
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
        .map(|q| {
            let line = text::paced(q);
            if q["code"] == "7D" {
                format!("{line} · CSR {} pts", c.csr_cost.share_label())
            } else {
                line
            }
        })
        .collect();
    if !has_week(c) {
        quotas.push(format!("CSR cost: {}", c.csr_cost.api_line()));
    }
    if g["cache"]["left"].is_number() || g["cache"]["cold"] == true {
        quotas.push(string(&g["cache"]["text"]).replace("CACHE ", "◷ "));
    }
    if let Some(n) = c.codex_week["percent"].as_f64() {
        quotas.push(format!("Codex 7D {n:.0}%"));
    }
    if let Some((line, _)) = csr_status(g) {
        quotas.push(format!("● {line}"));
    }
    [
        top.join(" · "),
        quotas.join(" · "),
        if has_week(c) && c.csr_cost.usd.is_some() {
            c.csr_cost.money_line()
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect()
}

fn has_week(data: &Composition) -> bool {
    array(&data.figure["quotas"])
        .iter()
        .any(|q| q["code"] == "7D" && q["used"].is_number())
}

/// The same caption is used by every pixel quota layout.
pub fn quota_caption(data: &Composition, q: &Value) -> String {
    if q["code"] == "7D" {
        format!("7D · CSR {} pts", data.csr_cost.share_label())
    } else {
        let pace = q["pace"]
            .as_f64()
            .is_some_and(|p| num(&q["used"]) / 100. > p);
        format!("{}{}", string(&q["code"]), if pace { " »" } else { "" })
    }
}

fn quota_ring(c: &mut Canvas, data: &Composition, q: &Value, x: f32, y: f32, r: f32, t: &Tokens) {
    let used = (num(&q["used"]) / 100.).clamp(0., 1.) as f32;
    let csr = (q["code"] == "7D")
        .then_some(data.csr_cost.share)
        .flatten()
        .filter(|share| *share > 0.)
        .map(|share| {
            // Keep the number honest; the six-degree floor is a visibility aid only.
            let end = used.max(6. / 360.);
            let sweep = (share as f32 / 100.).max(6. / 360.).min(end);
            let from = end - sweep;
            let gap = 1. / (std::f32::consts::TAU * r * c.output_scale);
            ((from - gap).max(0.), from, end)
        });
    if let Some((rest_end, from, to)) = csr {
        // Butt caps preserve the one-output-pixel notch without round-cap overlap.
        c.arc(x, y, r, 0., 1., t.ring, t.track, false);
        c.arc(x, y, r, 0., rest_end, t.ring, t.sky, false);
        c.arc(x, y, r, from, to, t.ring, t.csr_share, false);
    } else {
        c.ring(x, y, r, t.ring, t.track, &[(0., used, t.sky)]);
    }
    c.quota_gauges.push(QuotaGauge {
        code: string(&q["code"]).into(),
        center: (x, y),
        radius: r,
        csr_segment: csr,
    });
}

fn fitted_caption(
    c: &mut Canvas,
    f: &Fonts,
    line: &str,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    t: &Tokens,
) {
    fitted_caption_ink(c, f, line, x, y, width, size, t.quiet, t);
}
fn fitted_caption_ink(
    c: &mut Canvas,
    f: &Fonts,
    line: &str,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    ink: Rgba,
    t: &Tokens,
) {
    let st = sans(size, t.medium);
    let size = (size * width / f.width(line, st).max(width)).min(size);
    c.text(f, line, x, y, sans(size, t.medium), ink, A::M);
}

/// Weekly identity and CSR attribution keep separate inks on the same measured line.
fn weekly_caption(
    c: &mut Canvas,
    f: &Fonts,
    data: &Composition,
    q: &Value,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    t: &Tokens,
) {
    let line = quota_caption(data, q);
    let st = sans(size, t.medium);
    let st = sans(
        (size * width / f.width(&line, st).max(width)).min(size),
        t.medium,
    );
    let prefix = "7D · ";
    let left = x - f.width(&line, st) / 2.;
    let prefix_w = c.text(f, prefix, left, y, st, t.quiet, A::L);
    c.text(
        f,
        line.strip_prefix(prefix).unwrap_or(&line),
        left + prefix_w,
        y,
        st,
        t.csr_share,
        A::L,
    );
}

fn quota_money_caption(f: &Fonts, line: &str, width: f32, size: f32, t: &Tokens) -> String {
    if f.width(line, sans(size, t.medium)) > width {
        line.strip_suffix(" this week").unwrap_or(line).into()
    } else {
        line.into()
    }
}

/// Caption fitting never affects the equal-width gauge slots.
fn quota_label(
    c: &mut Canvas,
    f: &Fonts,
    line: &str,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    ink: Rgba,
    t: &Tokens,
) {
    let st = sans(size, t.medium);
    for (i, line) in f.wrap(line, st, width).iter().take(2).enumerate() {
        c.text(
            f,
            line,
            x,
            y + i as f32 * theme::CAPTION_LINE,
            st,
            ink,
            A::M,
        );
    }
}

/// Quota row shared by full, mid-height and narrow text-mode strips.
fn quota_row(
    c: &mut Canvas,
    f: &Fonts,
    data: &Composition,
    left: f32,
    width: f32,
    cy: f32,
    r: f32,
    label_y: f32,
    caption_size: f32,
    t: &Tokens,
) -> (f32, f32) {
    let g = &data.figure;
    let mut quotas = array(&g["quotas"])
        .iter()
        .filter(|q| q["used"].is_number())
        .collect::<Vec<_>>();
    quotas.sort_by_key(|q| if q["code"] == "5H" { 0 } else { 1 });
    let cache = g["cache"]["left"].is_number() || g["cache"]["cold"] == true;
    let codex = data.codex_week["percent"].as_f64();
    let api = !has_week(data);
    let slots = quotas.len() + usize::from(cache) + usize::from(codex.is_some()) + usize::from(api);
    let unit = width / slots.max(1) as f32;
    let mut cursor = left;
    let mut bounds = (left + width, left);
    for q in quotas {
        let weekly = q["code"] == "7D";
        let slot = unit;
        let x = cursor + slot / 2.;
        cursor += slot;
        bounds.0 = bounds.0.min(x - r);
        bounds.1 = bounds.1.max(x + r);
        quota_ring(c, data, q, x, cy, r, t);
        c.text(
            f,
            &format!("{:.0}%", num(&q["used"])),
            x,
            cy + 4.,
            serif((r * 0.6).clamp(10., 18.), t.medium),
            t.ink,
            A::M,
        );
        if weekly {
            weekly_caption(c, f, data, q, x, label_y, slot - S2, caption_size, t);
        } else {
            quota_label(
                c,
                f,
                &quota_caption(data, q),
                x,
                label_y,
                slot - S2,
                caption_size,
                t.quiet,
                t,
            );
        }
        if weekly && data.csr_cost.usd.is_some() {
            let money =
                quota_money_caption(f, &data.csr_cost.money_line(), slot - S2, caption_size, t);
            fitted_caption(
                c,
                f,
                &money,
                x,
                label_y + theme::CAPTION_LINE,
                slot - S2,
                caption_size,
                t,
            );
        }
    }
    if cache {
        let x = cursor + unit / 2.;
        cursor += unit;
        bounds.0 = bounds.0.min(x - r);
        bounds.1 = bounds.1.max(x + r);
        c.quota_gauges.push(QuotaGauge {
            code: "cache".into(),
            center: (x, cy),
            radius: r,
            csr_segment: None,
        });
        // The minute shown is the one drawn: a seconds sweep repaints the whole picture.
        let left = (num(&g["cache"]["left"]) / 60.).floor() * 60.;
        let cold = g["cache"]["cold"] == true || num(&g["cache"]["left"]) <= 0.;
        let ttl = g["cache"]["ttl"]
            .as_f64()
            .filter(|n| *n > 0.)
            .unwrap_or(3600.);
        c.ring(
            x,
            cy,
            r,
            t.ring,
            t.track,
            &[(
                0.,
                (left / ttl).clamp(0., 1.) as f32,
                if cold { t.amber } else { t.mint },
            )],
        );
        let cache_value = if cold {
            "COLD".into()
        } else {
            masthead::span(left)
        };
        let st = serif((r * 0.6).clamp(10., 18.), t.medium);
        let st = serif(
            st.size * (unit - S2) / f.width(&cache_value, st).max(unit - S2),
            t.medium,
        );
        c.text(f, &cache_value, x, cy + 4., st, t.ink, A::M);
        let caption = if cold {
            string(&g["cache"]["long"])
                .strip_prefix("COLD · ")
                .unwrap_or("◷ cache")
        } else {
            "◷ cache"
        };
        quota_label(
            c,
            f,
            caption,
            x,
            label_y,
            unit - S2,
            caption_size,
            t.quiet,
            t,
        );
    }
    if let Some(used) = codex {
        let x = cursor + unit / 2.;
        cursor += unit;
        bounds.0 = bounds.0.min(x - r);
        bounds.1 = bounds.1.max(x + r);
        c.quota_gauges.push(QuotaGauge {
            code: "Codex 7D".into(),
            center: (x, cy),
            radius: r,
            csr_segment: None,
        });
        c.ring(
            x,
            cy,
            r,
            t.ring,
            t.track,
            &[(0., (used / 100.).clamp(0., 1.) as f32, t.asked)],
        );
        c.text(
            f,
            &format!("{used:.0}%"),
            x,
            cy + 4.,
            serif((r * 0.6).clamp(10., 18.), t.medium),
            t.ink,
            A::M,
        );
        fitted_caption(
            c,
            f,
            "\u{e181} Codex 7D",
            x,
            label_y,
            unit - S2,
            caption_size,
            t,
        );
    }
    if api {
        let slot = unit;
        let x = cursor + slot / 2.;
        c.quota_gauges.push(QuotaGauge {
            code: "CSR cost".into(),
            center: (x, cy),
            radius: 0.,
            csr_segment: None,
        });
        let card_w = (slot - S2).min(180.);
        let top = cy - r;
        let bottom = label_y + theme::CAPTION_LINE;
        c.rrect(x - card_w / 2., top, card_w, bottom - top, S2, t.chip);
        c.rrect_line(
            x - card_w / 2.,
            top,
            card_w,
            bottom - top,
            S2,
            t.hair,
            t.hair_width,
        );
        fitted_caption(
            c,
            f,
            "CSR cost",
            x,
            top + theme::CAPTION_LINE,
            card_w - S2,
            caption_size,
            t,
        );
        let api_line = data.csr_cost.api_line();
        let style = sans(caption_size, t.medium);
        let api_line = if f.width(&api_line, style) > card_w - S2 {
            api_line.replace(" this week", "")
        } else {
            api_line
        };
        let lines = f.wrap(&api_line, style, card_w - S2);
        if lines.len() <= 2 {
            quota_label(
                c,
                f,
                &api_line,
                x,
                cy + S2,
                card_w - S2,
                caption_size,
                t.quiet,
                t,
            );
        } else {
            // The native-text fallback's tiny strip still preserves the whole cost/call line.
            fitted_caption(c, f, &api_line, x, cy + S2, card_w - S2, caption_size, t);
        }
        bounds.0 = bounds.0.min(x - card_w / 2.);
        bounds.1 = bounds.1.max(x + card_w / 2.);
    }
    bounds
}

// The geometry and primitives below are the approved B prototype's glass, serif and ring design.
const M: f32 = S4;
const P: f32 = S4;
const CW: f32 = W - 2. * M;
const IX: f32 = M + P;
const IW: f32 = CW - 2. * P;
fn label(c: &mut Canvas, f: &Fonts, s: &str, x: f32, y: f32, size: f32, col: Rgba, w: f32) {
    let title_icon = match s {
        "Context" => Some(Icon::Gauge),
        "Quotas" => Some(Icon::Hourglass),
        "Where the work stands" | "This session" => Some(Icon::ScrollText),
        _ => None,
    };
    let x = if let Some(glyph) = title_icon {
        c.icon(f, glyph, x, y, 14., size, col);
        x + 14. + ICON_GAP
    } else {
        x
    };
    c.text(f, &icons::line(s), x, y, sans(size, w), col, A::L);
}
fn panel(c: &mut Canvas, y: f32, h: f32, t: &Tokens) {
    c.shadow(M, y, CW, h, 24., t.ambient_shadow);
    c.shadow(M, y, CW, h, 24., t.contact_shadow);
    c.glass(
        M,
        y,
        CW,
        h,
        24.,
        lin(
            0.,
            y,
            0.,
            y + h,
            &[(t.linear_at[0], t.glass[0]), (t.linear_at[1], t.glass[1])],
        ),
    );
    c.rrect_line(M, y, CW, h, 24., t.hair, t.hair_width);
    c.line(
        M + 24.,
        y + 0.5 / S,
        M + CW - 24.,
        y + 0.5 / S,
        1. / S,
        t.top_highlight,
    );
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
    let s = f.fit(&icons::line(s), st, width);
    c.text(f, &s, x, y, st, col, A::L);
}

/// Match token → kept punctuation (trailing spaces dropped so spaced emdash keeps " —").
const STORY_BOUNDARIES: &[(&str, &str)] = &[
    (". ", "."),
    ("; ", ";"),
    (": ", ":"),
    (", ", ","),
    (" — ", " —"),
    (" – ", " –"),
];

/// Candidates that end on a clause boundary with an ellipsis appended. Shared by pixel and text.
fn story_clause_candidates(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in s.char_indices() {
        let rest = &s[i..];
        for &(needle, keep) in STORY_BOUNDARIES {
            if rest.starts_with(needle) {
                out.push(format!("{}{keep}…", &s[..i]));
            }
        }
    }
    out
}

/// Fit `s` into at most `n` wrapped lines. Full text when it fits; else the longest clause-boundary
/// candidate that fits; else a standalone ellipsis. Never keeps an arbitrary first line.
fn fit_story_lines(
    s: &str,
    n: usize,
    wrap: &dyn Fn(&str) -> Vec<String>,
    line_fits: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let lines = wrap(s);
    let overflows = lines.len() > n || lines.iter().any(|l| !line_fits(l));
    if !overflows {
        return lines;
    }
    let mut best: Option<String> = None;
    for candidate in story_clause_candidates(s) {
        let wrapped = wrap(&candidate);
        if wrapped.len() <= n && wrapped.iter().all(|l| line_fits(l)) {
            best = Some(candidate);
        }
    }
    if let Some(candidate) = best {
        return wrap(&candidate);
    }
    vec!["…".into()]
}

/// Wrap story for the pixel path.
fn wrap_clauses(f: &Fonts, s: &str, st: St, maxw: f32, n: usize) -> Vec<String> {
    fit_story_lines(s, n, &|c| f.wrap(c, st, maxw), &|l| {
        f.width(l, st) <= maxw + 0.5
    })
}

/// Text-fallback story lines with the same clause-boundary rule as the pixel path.
pub fn story_text_lines(s: &str, cols: usize, max_lines: usize) -> Vec<String> {
    let cols = cols.max(1);
    fit_story_lines(s, max_lines, &|c| text::wrap(c, cols, 1000), &|l| {
        text::len(l) <= cols
    })
}

/// Fresh input plus cache-write, normalised to the visible maximum. Flat history abstains.
pub fn fresh_bar_heights(bars: &[(f64, f64)]) -> Option<Vec<f32>> {
    if bars.len() < 2 {
        return None;
    }
    let max = bars.iter().map(|(_, fresh)| *fresh).fold(0., f64::max);
    let min = bars
        .iter()
        .map(|(_, fresh)| *fresh)
        .fold(f64::INFINITY, f64::min);
    if max <= 0. || max < min * 1.5 {
        return None;
    }
    Some(
        bars.iter()
            .map(|(_, fresh)| (fresh / max).clamp(0., 1.) as f32)
            .collect(),
    )
}

// Match the same ordered, valid request rows as cards::request_bars, before trimming the tail.
fn recent_request_compactions(s: &Value) -> Vec<usize> {
    let mut stamps: Vec<_> = array(&s["replies"])
        .iter()
        .filter(|row| num(&row[0]) != 0. && num(&row[1]) > 0.)
        .map(|row| num(&row[0]))
        .collect();
    if stamps.len() < 2 {
        return Vec::new();
    }
    stamps.sort_by(f64::total_cmp);
    let first_visible = stamps.len().saturating_sub(24);
    let mut indices: Vec<_> = array(&s["compacts"])
        .iter()
        .filter_map(|boundary| {
            let at = boundary.as_f64()?;
            if at < stamps[0] {
                return None;
            }
            let i = stamps.partition_point(|stamp| *stamp < at);
            (i >= first_visible && i < stamps.len()).then(|| i - first_visible)
        })
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

#[derive(Clone, Debug)]
pub struct Metrics {
    pub csr_cost: InstrumentCost,
    pub quota_gauges: Vec<QuotaGauge>,
    pub fresh_plots: Vec<FreshPlot>,
    pub question_size: f32,
    pub question_line_height: f32,
    /// Only missing or system-fallback glyphs; bundled cross-family coverage is primary.
    pub font_fallbacks: BTreeMap<char, String>,
    pub content_left: f32,
    pub content_width: f32,
    pub dream: f32,
    pub answer_y: Option<f32>,
    pub answer_spans: Vec<(f32, f32, String)>,
    /// Offer hotkey hit target in layout pt: (y, x0, x1).
    pub offer_hit: Option<(f32, f32, f32)>,
    pub story_region: Option<(f32, f32)>,
    pub shift_hits: Vec<f32>,
    pub agents_region: Option<(f32, f32)>,
    pub figure_region: (f32, f32),
    pub help_region: Option<(f32, f32)>,
    pub history_rows: usize,
    pub history_bottom: Option<f32>,
    /// Context-window ceiling hairline Y in layout pt, when the expanded bars are drawn.
    pub ceiling_y: Option<f32>,
    /// Collapsed context ring: (cx, cy, radius). Absent when expanded, quotas-only, or compact fallback.
    pub ring_bounds: Option<(f32, f32, f32)>,
    /// Collapsed ring+payback+gauge group top/bottom in layout pt (includes label descenders).
    pub ring_group: Option<(f32, f32)>,
    /// `available_height` fed to `r = clamp(78, available_height/2.8, 118)` for the collapsed ring.
    pub ring_formula_avail: Option<f32>,
    /// Mid-height Context layout bounds (side ring + small gauges). Absent for strip / full ring / expanded.
    pub mid_layout: Option<MidLayoutBounds>,
    pub story_line_count: usize,
    /// Layout pt between the last card's bottom edge and the pixmap bottom (above the key row).
    pub last_card_bottom_pad: f32,
    /// Distance to the key row after fitting the pane, in output pixels.
    pub last_card_gap_px: f32,
    pub height: f32,
    pub question_lines: Vec<String>,
    pub sections_collapsed: bool,
}
fn question_layout(f: &Fonts, q: &str, width: u32, t: &Tokens) -> (St, Vec<String>) {
    let size = (24. + (width as f32 - 704.) / 256. * 6.).clamp(22., 30.);
    let st = serif(size, t.bold);
    (st, f.wrap_balanced(q, st, IW))
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
    f.used.borrow_mut().clear();
    let t = theme.tokens();
    let ratio = width as f32 / (W * S);
    let mut maxh = (available_height as f32 / (S * ratio)).max(800.);
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    let body = sans(theme::BODY_SIZE, t.regular);
    let title = sans(theme::TITLE_SIZE, t.bold);
    let story_st = body;
    let story_lines_fit = if c.story.is_empty() {
        Vec::new()
    } else {
        wrap_clauses(f, first_sentence(&c.story), story_st, IW, 4)
    };
    let story_line_rows = story_lines_fit.len();
    let has_story = !c.story.is_empty() || !c.shifts.is_empty() || !c.learnt.is_empty();
    let story_age =
        has_story && !c.story_age.is_empty() && (!c.story.is_empty() || !c.shifts.is_empty());
    let story_h = if !has_story {
        0.
    } else {
        P * 2.
            + theme::TITLE_LINE
            + S2
            + if story_age {
                theme::CAPTION_LINE + S3
            } else {
                0.
            }
            + (story_line_rows + usize::from(!c.learnt.is_empty())) as f32 * theme::BODY_LINE
            + if c.shifts.is_empty() {
                0.
            } else {
                S3 + c.shifts.len().min(3) as f32 * theme::CAPTION_LINE
            }
    };
    let full_extras = extras(c, false);
    let collapsed = full_extras
        .iter()
        .any(|line| f.width(line, caption) > IW * 2.);
    let extra_lines = extras(c, collapsed);
    let extra_layout = extra_lines
        .iter()
        .map(|line| {
            let (name, body) = line.split_once(": ").unwrap_or((line, ""));
            let glyph = if name.starts_with("Confusing") {
                Icon::CircleHelp
            } else {
                Icon::Sparkles
            };
            let name = format!("{} {name}:", glyph.glyph());
            let tw = f.width(&name, sans(theme::CAPTION_SIZE, t.medium)) + f.width(" ", caption);
            let rows = f.wrap(body, caption, IW - tw);
            (name, tw, rows)
        })
        .collect::<Vec<_>>();
    let extras_h = extra_layout
        .iter()
        .map(|(_, _, lines)| lines.len() as f32 * theme::CAPTION_LINE)
        .sum::<f32>();
    let metadata = meta(c);
    let mut meta_rows: Vec<Vec<&str>> = vec![];
    let mut row = vec![];
    let mut row_width = 0.;
    for part in metadata
        .split(" · ")
        .filter(|p| !p.is_empty() && *p != "carried")
    {
        let st = if part.starts_with('⌗') {
            mono(theme::CHIP_SIZE, t.regular)
        } else {
            caption
        };
        let part_width = f.width(part, st)
            + if part.starts_with("dreamer ") {
                S5 + ICON_GAP
            } else {
                0.
            };
        if !row.is_empty() && row_width + S3 + part_width > IW {
            meta_rows.push(std::mem::take(&mut row));
            row_width = 0.;
        }
        if !row.is_empty() {
            row_width += S3;
        }
        row.push(part);
        row_width += part_width;
    }
    if !row.is_empty() {
        meta_rows.push(row);
    }
    let status = status_line(c);
    let status_lines = f.wrap(&status, caption, IW);
    let (qs, question) = question_layout(f, &c.question(), width, t);
    let question_line_height = (qs.size * 1.25).round();
    let qh = question.len() as f32 * question_line_height;
    let has_answers = c.dream.state == Batch::Ready || c.offer;
    let hero_h = P * 2.
        + theme::TITLE_LINE
        + S2
        + qh
        + if meta_rows.is_empty() {
            0.
        } else {
            S3 + meta_rows.len() as f32 * theme::CAPTION_LINE
        }
        + if has_answers {
            S1 + theme::CAPTION_LINE
        } else {
            0.
        }
        + if status.is_empty() {
            0.
        } else {
            S2 + status_lines.len() as f32 * theme::CAPTION_LINE
        }
        + if extras_h > 0. { S3 + extras_h } else { 0. };
    // Keep the full specified headline size and the hero's 30% budget together in short panes.
    maxh = maxh.max((hero_h / 0.30).ceil());
    let active = c.agents.agents.iter().any(agents::is_active);
    let agents_h = if active {
        P * 2. + theme::BODY_LINE
    } else {
        0.
    };
    let recap_lines = c
        .repo_recap
        .as_ref()
        .map(|recap| {
            recap
                .text
                .lines()
                .flat_map(|line| f.wrap(line, body, IW))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let sell_h = if c.repo_recap.is_some() {
        P * 2.
            + theme::TITLE_LINE
            + S2
            + theme::CAPTION_LINE
            + S3
            + recap_lines.len() as f32 * theme::BODY_LINE
    } else if c.empty_session || c.offer {
        P * 2. + theme::TITLE_LINE + S3 + 3. * (theme::BODY_LINE + theme::CAPTION_LINE) + 2. * S4
    } else {
        0.
    };
    let requested_extra_h = if open.changes {
        c.shifts.len() as f32 * 58.
    } else if open.threads {
        (c.river.len().min(8) as f32 * 33.).max(60.)
    } else if open.dreams {
        c.dream.queue.len().min(4) as f32 * 38.
    } else {
        0.
    };
    let empty_help = sell_h;
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
    let notice_h = if c.notice.is_some() {
        P * 2. + theme::BODY_LINE
    } else {
        0.
    };
    let notice_gap = if c.notice.is_some() { S3 } else { 0. };
    let quotas_only = c.figure["ctx"]["tokens"].as_f64().is_none_or(|n| n <= 0.);
    let figure_min = if quotas_only {
        154.
    } else if open.figure {
        // Chart (95 + 40), chart labels/payback (72), gauge offset (84).
        291.
    } else {
        110.
    };
    let top_core =
        M + hero_h + S3 + story_h + if story_h > 0. { S3 } else { 0. } + notice_h + notice_gap;
    let tail_core = agents_h
        + if active { S3 } else { 0. }
        + empty_help
        + if empty_help > 0. { S3 } else { 0. };
    // Layout points may scale to fit a narrow/wide terminal. Reserve S3 in final pixels.
    let min_core = top_core + tail_core + figure_min;
    maxh = maxh.max(min_core / (1. - S3 / available_height as f32));
    let key_gap = S3 * maxh / available_height as f32;
    let available_extra = (maxh - min_core - key_gap - S3).max(0.);
    let extra_h = if open.threads {
        requested_extra_h.min(available_extra)
    } else {
        let lines = drill_lines
            .len()
            .saturating_sub(open.scroll)
            .min(((available_extra - P * 2.).max(0.) / theme::BODY_LINE) as usize);
        if lines == 0 {
            0.
        } else {
            lines as f32 * theme::BODY_LINE + P * 2.
        }
    };
    let top_core = top_core + extra_h + if extra_h > 0. { S3 } else { 0. };
    let card_gap = S3;
    let (figure_h, help_h) = if quotas_only && empty_help > 0. {
        let available = maxh - top_core - tail_core - key_gap;
        let figure_h = available.min(empty_quota_height(c)).max(figure_min);
        // Cards always stack from the top. The final card owns surplus height.
        (figure_h, empty_help + (available - figure_h).max(0.))
    } else {
        (maxh - top_core - tail_core - key_gap, empty_help)
    };
    let height = maxh;
    let mut canvas = Canvas::new(W, height);
    canvas.output_scale = (width as f32 / W).min(available_height as f32 / height);
    canvas.background(t);
    let mut y = M;
    panel(&mut canvas, y, hero_h, t);
    let title_y = y + P + f.cap(title);
    label(
        &mut canvas,
        f,
        if c.offer {
            "Dreams are off"
        } else if c.dream.state == Batch::Ready {
            if c.asked_away {
                "The dreamer asks · asked while you were away"
            } else {
                "The dreamer asks"
            }
        } else {
            "CSR's dreamer"
        },
        IX,
        title_y,
        theme::TITLE_SIZE,
        t.ink,
        t.bold,
    );
    if c.dream.carried {
        label(
            &mut canvas,
            f,
            "CARRIED",
            IX + f.width("The dreamer asks", title) + S3,
            title_y,
            theme::CAPTION_SIZE,
            t.dream,
            t.medium,
        );
    }
    canvas.icon(
        f,
        if c.offer { Icon::Moon } else { Icon::MoonStar },
        W - M - P - 18.,
        title_y,
        18.,
        theme::TITLE_SIZE,
        t.dream,
    );
    let mut cursor = y + P + theme::TITLE_LINE + S2;
    for line in &question {
        canvas.text(f, line, IX, cursor + f.cap(qs), qs, t.ink, A::L);
        cursor += question_line_height;
    }
    if !meta_rows.is_empty() {
        cursor += S3;
    }
    for parts in &meta_rows {
        let b = cursor + f.cap(caption);
        let mut x = IX;
        for part in parts {
            if part.starts_with("dreamer ") {
                let spent = num(&c.dream.budget["spent"]);
                let cap = num(&c.dream.budget["cap"]).max(1.);
                canvas.rrect(x, b - f.cap(caption) / 2., S5, 3., 1.5, t.track);
                canvas.rrect(
                    x,
                    b - f.cap(caption) / 2.,
                    S5 * (spent / cap).clamp(0., 1.) as f32,
                    3.,
                    1.5,
                    t.dream,
                );
                x += S5 + ICON_GAP;
            }
            let st = if part.starts_with('⌗') {
                mono(theme::CHIP_SIZE, t.regular)
            } else {
                caption
            };
            let w = f.width(part, st);
            if part.starts_with('⌗') {
                canvas.rrect(
                    x - S1,
                    b - f.cap(st) - S1 / 2.,
                    w + S2,
                    theme::CHIP_LINE,
                    S1,
                    t.chip,
                );
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
            x += w + S3;
        }
        cursor += theme::CAPTION_LINE;
    }
    let mut answer_spans = vec![];
    let mut offer_hit = None;
    let mut answer_y = None;
    if has_answers {
        cursor += S1;
    }
    if c.dream.state == Batch::Ready {
        let b = cursor + f.cap(caption);
        answer_y = Some(b);
        let mut x = IX;
        for (label, answer) in [
            ("1 ✓ helpful", "helpful"),
            ("2 \u{e1c2} thought of it", "thought_of_it"),
            ("3 ✗ not relevant", "not_relevant"),
            ("4 → ask Claude", "ask_claude"),
        ] {
            let w = canvas.text(
                f,
                label,
                x,
                b,
                sans(theme::CAPTION_SIZE, t.medium),
                if answer == "ask_claude" {
                    t.asked
                } else {
                    t.quiet
                },
                A::L,
            );
            answer_spans.push((x, x + w, answer.into()));
            x += w + S3;
        }
        cursor += theme::CAPTION_LINE;
    } else if c.offer {
        let b = cursor + f.cap(caption);
        let mut x = IX;
        x += canvas.text(f, "Press", x, b, caption, t.quiet, A::L) + ICON_GAP;
        let key_w = f.width("1", caption);
        canvas.rrect(
            x - S1,
            b - f.cap(caption) - S1 / 2.,
            key_w + S2,
            theme::CAPTION_LINE,
            S1,
            t.chip,
        );
        canvas.text(f, "1", x, b, caption, t.dream, A::L);
        x += key_w + ICON_GAP;
        x += canvas.text(
            f,
            "to turn dreams on in all your sessions.",
            x,
            b,
            caption,
            t.quiet,
            A::L,
        );
        offer_hit = Some((b, IX, x));
        cursor += theme::CAPTION_LINE;
    }
    if !status.is_empty() {
        cursor += S2;
        for line in &status_lines {
            label(
                &mut canvas,
                f,
                line,
                IX,
                cursor + f.cap(caption),
                theme::CAPTION_SIZE,
                t.quiet,
                t.regular,
            );
            cursor += theme::CAPTION_LINE;
        }
    }
    if extras_h > 0. {
        cursor += S3;
    }
    for (title_text, tw, lines) in &extra_layout {
        let b = cursor + f.cap(caption);
        label(
            &mut canvas,
            f,
            title_text,
            IX,
            b,
            theme::CAPTION_SIZE,
            t.dream,
            t.medium,
        );
        for (i, row) in lines.iter().enumerate() {
            let b = cursor + f.cap(caption);
            let (body_text, receipt) = row.split_once(" · ").unwrap_or((row, ""));
            let x = IX + if i == 0 { *tw } else { 0. };
            label(
                &mut canvas,
                f,
                body_text,
                x,
                b,
                theme::CAPTION_SIZE,
                t.ink,
                t.regular,
            );
            if !receipt.is_empty() {
                let x = x + f.width(body_text, caption) + ICON_GAP;
                let st = mono(theme::CHIP_SIZE, t.regular);
                let rw = f.width(receipt, st);
                canvas.rrect(
                    x - S1,
                    b - f.cap(st) - S1 / 2.,
                    rw + S2,
                    theme::CHIP_LINE,
                    S1,
                    t.chip,
                );
                canvas.text(f, receipt, x, b, st, t.quiet, A::L);
            }
            cursor += theme::CAPTION_LINE;
        }
    }
    y += hero_h + card_gap;
    if let Some(notice) = &c.notice {
        panel(&mut canvas, y, notice_h, t);
        single(
            &mut canvas,
            f,
            notice,
            IX,
            y + P + f.cap(body),
            IW,
            theme::BODY_SIZE,
            t.ink,
            t,
        );
        y += notice_h + card_gap;
    }
    let mut shift_hits = vec![];
    let mut story_line_count = 0usize;
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
            y + P + f.cap(title),
            theme::TITLE_SIZE,
            t.ink,
            t.bold,
        );
        let mut cursor = y + P + theme::TITLE_LINE + S2;
        if story_age {
            single(
                &mut canvas,
                f,
                &c.story_age,
                IX,
                cursor + f.cap(caption),
                IW,
                theme::CAPTION_SIZE,
                t.quiet,
                t,
            );
            cursor += theme::CAPTION_LINE + S3;
        }
        story_line_count = story_lines_fit.len();
        for line in &story_lines_fit {
            canvas.text(f, line, IX, cursor + f.cap(body), body, t.ink, A::L);
            cursor += theme::BODY_LINE;
        }
        if !c.learnt.is_empty() {
            single(
                &mut canvas,
                f,
                &c.learnt.join(" · "),
                IX,
                cursor + f.cap(body),
                IW,
                theme::BODY_SIZE,
                t.mint,
                t,
            );
            cursor += theme::BODY_LINE;
        }
        if !c.shifts.is_empty() {
            cursor += S3;
        }
        for v in c.shifts.iter().take(3) {
            let b = cursor + f.cap(caption);
            shift_hits.push(b - f.cap(caption) / 2.);
            single(
                &mut canvas,
                f,
                &shift_row(v, false),
                IX,
                b,
                IW,
                theme::CAPTION_SIZE,
                if mark(v) == "!" { t.risk } else { t.mint },
                t,
            );
            cursor += theme::CAPTION_LINE;
        }
        y += story_h + card_gap;
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
        y += extra_h + card_gap;
    }
    let mut ceiling_y = None;
    let mut ring_bounds = None;
    let mut ring_group = None;
    let mut ring_formula_avail = None;
    let mut mid_layout = None;
    let figure_draw_h = figure_h;
    let figure_layout_h = figure_h;
    let figure_region = (y, figure_draw_h);
    let fig_info = figure(
        &mut canvas,
        f,
        c,
        y,
        figure_layout_h,
        t,
        open.figure,
        narrow,
    );
    if let Some((cy, ring, group, avail, mid)) = fig_info {
        ceiling_y = cy;
        ring_bounds = ring;
        ring_group = group;
        ring_formula_avail = avail;
        mid_layout = mid;
    }
    y += figure_draw_h;
    let needs_tail = active || help_h > 0.;
    if needs_tail {
        y += card_gap;
    }
    let agents_region = active.then_some((y, agents_h));
    if active {
        let agents_draw_h = agents_h;
        panel(&mut canvas, y, agents_draw_h, t);
        let mut x = IX;
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
            canvas.icon(
                f,
                Icon::Bot,
                x,
                y + P + f.cap(body),
                14.,
                11.5,
                if is_stalled { t.amber } else { t.mint },
            );
            x += S5;
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
            x + S2,
            y + P + f.cap(body),
            W - x - 40.,
            11.5,
            t.amber,
            t,
        );
        y += agents_draw_h;
        if help_h > 0. {
            y += card_gap;
        }
    }
    let mut history_rows = 0;
    let mut history_bottom = None;
    let help_region = (help_h > 0.).then_some((y, help_h));
    if let Some((help_y, help_draw_h)) = help_region {
        panel(&mut canvas, help_y, help_draw_h, t);
        if let Some(recap) = &c.repo_recap {
            let mut cursor = help_y + P;
            label(
                &mut canvas,
                f,
                "Last time here",
                IX,
                cursor + f.cap(title),
                theme::TITLE_SIZE,
                t.ink,
                t.bold,
            );
            cursor += theme::TITLE_LINE + S2;
            label(
                &mut canvas,
                f,
                &format!(
                    "{} · session {}",
                    recap.age,
                    recap.session.chars().take(8).collect::<String>()
                ),
                IX,
                cursor + f.cap(caption),
                theme::CAPTION_SIZE,
                t.quiet,
                t.regular,
            );
            cursor += theme::CAPTION_LINE + S3;
            for line in &recap_lines {
                label(
                    &mut canvas,
                    f,
                    line,
                    IX,
                    cursor + f.cap(body),
                    theme::BODY_SIZE,
                    t.ink,
                    t.regular,
                );
                cursor += theme::BODY_LINE;
            }
            let bottom = help_y + help_draw_h - P;
            let available_rows = ((bottom - cursor - S4 - theme::TITLE_LINE - S2)
                / theme::BODY_LINE)
                .floor()
                .max(0.) as usize;
            if available_rows > 0 && !recap.history.is_empty() {
                cursor += S4;
                label(
                    &mut canvas,
                    f,
                    "Earlier here",
                    IX,
                    cursor + f.cap(title),
                    theme::TITLE_SIZE,
                    t.ink,
                    t.bold,
                );
                cursor += theme::TITLE_LINE + S2;
                for prior in recap.history.iter().take(available_rows) {
                    let age = format!("{}  {}", history_mark(&prior.outcome), prior.age);
                    let age_w = f.width(&age, caption).max(88.);
                    label(
                        &mut canvas,
                        f,
                        &age,
                        IX,
                        cursor + f.cap(body),
                        theme::CAPTION_SIZE,
                        t.quiet,
                        t.regular,
                    );
                    single(
                        &mut canvas,
                        f,
                        &prior.intent,
                        IX + age_w + S3,
                        cursor + f.cap(body),
                        IW - age_w - S3,
                        theme::BODY_SIZE,
                        t.ink,
                        t,
                    );
                    cursor += theme::BODY_LINE;
                    history_rows += 1;
                }
                history_bottom = Some(cursor);
            }
        } else {
            let mut cursor = help_y + P;
            label(
                &mut canvas,
                f,
                "What CSR can do",
                IX,
                cursor + f.cap(title),
                theme::TITLE_SIZE,
                t.ink,
                t.bold,
            );
            cursor += theme::TITLE_LINE + S3;
            let features = [
                (
                    Icon::Brain,
                    "Find decisions and examples in your past conversations.",
                ),
                (
                    Icon::FileWarning,
                    "Surface stale context before it sends the work off course.",
                ),
                (
                    Icon::ScrollText,
                    "Bring unfinished questions back when they matter.",
                ),
            ];
            for (line, (icon, caption_text)) in sells().iter().zip(features) {
                let base = cursor + f.cap(body);
                canvas.icon(f, icon, IX, base, 14., theme::BODY_SIZE, t.mint);
                let x = IX + 14. + ICON_GAP;
                label(
                    &mut canvas,
                    f,
                    line,
                    x,
                    base,
                    theme::BODY_SIZE,
                    t.ink,
                    t.medium,
                );
                label(
                    &mut canvas,
                    f,
                    caption_text,
                    x,
                    cursor + theme::BODY_LINE + f.cap(caption),
                    theme::CAPTION_SIZE,
                    t.quiet,
                    t.regular,
                );
                cursor += theme::BODY_LINE + theme::CAPTION_LINE + S4;
            }
            let bottom = help_y + help_draw_h - P;
            if c.empty_session
                && bottom - cursor
                    >= theme::TITLE_LINE + S3 + theme::BODY_LINE + theme::CAPTION_LINE
            {
                label(
                    &mut canvas,
                    f,
                    "Try a question",
                    IX,
                    cursor + f.cap(title),
                    theme::TITLE_SIZE,
                    t.ink,
                    t.bold,
                );
                cursor += theme::TITLE_LINE + S3;
                for (question, hint) in starter_questions() {
                    if cursor + theme::BODY_LINE + theme::CAPTION_LINE > bottom {
                        break;
                    }
                    single(
                        &mut canvas,
                        f,
                        question,
                        IX,
                        cursor + f.cap(body),
                        IW,
                        theme::BODY_SIZE,
                        t.ink,
                        t,
                    );
                    single(
                        &mut canvas,
                        f,
                        hint,
                        IX,
                        cursor + theme::BODY_LINE + f.cap(caption),
                        IW,
                        theme::CAPTION_SIZE,
                        t.quiet,
                        t,
                    );
                    cursor += theme::BODY_LINE + theme::CAPTION_LINE + S3;
                }
            }
        }
        y = help_y + help_draw_h;
    }
    let last_card_bottom = if let Some((hy, hh)) = help_region {
        hy + hh
    } else if active {
        agents_region.map(|(ay, ah)| ay + ah).unwrap_or(y)
    } else {
        figure_region.0 + figure_region.1
    };
    let last_card_bottom_pad = (height - last_card_bottom).max(0.);
    let raw = image::RgbaImage::from_raw(
        canvas.pm.width(),
        canvas.pm.height(),
        canvas.pm.data().to_vec(),
    )?;
    let scale = (width as f32 / W).min(available_height as f32 / height);
    let fitted_width = (W * scale).round().min(width as f32) as u32;
    let fitted_height = available_height;
    let content_h = (height * scale).round().min(available_height as f32) as u32;
    let fitted = image::imageops::resize(
        &raw,
        fitted_width,
        content_h.max(1),
        image::imageops::FilterType::Lanczos3,
    );
    let left = (width - fitted_width) / 2;
    let mut backdrop = Canvas::new(width as f32 / S, fitted_height as f32 / S);
    backdrop.background(t);
    let mut output = image::RgbaImage::from_raw(width, fitted_height, backdrop.pm.data().to_vec())?;
    image::imageops::overlay(&mut output, &fitted, i64::from(left), 0);
    let pm = tiny_skia::Pixmap::from_vec(
        output.into_raw(),
        tiny_skia::IntSize::from_wh(width, fitted_height)?,
    )?;
    let font_fallbacks = f.used.borrow().clone();
    Some((
        pm,
        Metrics {
            csr_cost: c.csr_cost.clone(),
            quota_gauges: canvas.quota_gauges.clone(),
            fresh_plots: canvas.fresh_plots.clone(),
            question_size: qs.size,
            question_line_height,
            font_fallbacks,
            content_left: left as f32 / width as f32,
            content_width: fitted_width as f32 / width as f32,
            dream: hero_h,
            answer_y,
            answer_spans,
            offer_hit,
            story_region,
            shift_hits,
            agents_region,
            figure_region,
            help_region,
            history_rows,
            history_bottom,
            ceiling_y,
            ring_bounds,
            ring_group,
            ring_formula_avail,
            mid_layout,
            story_line_count,
            last_card_bottom_pad,
            last_card_gap_px: available_height as f32
                - last_card_bottom * content_h as f32 / height,
            height,
            question_lines: question,
            sections_collapsed: collapsed,
        },
    ))
}

/// Mid-height Context card geometry for tests and space-usage checks.
#[derive(Clone, Debug)]
pub struct MidLayoutBounds {
    /// Main context ring (cx, cy, radius).
    pub ring: (f32, f32, f32),
    /// Vertically centred content group top/bottom in layout pt.
    pub group: (f32, f32),
    /// Small quota/cache/Codex ring radius.
    pub gauge_r: f32,
    pub gauges_cy: f32,
    /// Baseline of gauge code labels.
    pub gauge_label_y: f32,
    /// Compact trend/payback/by-CSR line baseline.
    pub details_y: f32,
    /// Unclamped `(h - title - 2P) / 2` before `clamp(25, …, 64)`.
    pub radius_inner: f32,
    pub radius_max: f32,
    /// Left edge of the first small gauge (for overlap checks vs the main ring).
    pub gauges_left: f32,
    /// Right edge of the last small gauge.
    pub gauges_right: f32,
}

/// (ceiling_y, ring_bounds, ring_group_top_bottom, ring_formula_avail, mid_layout)
type FigureInfo = (
    Option<f32>,
    Option<(f32, f32, f32)>,
    Option<(f32, f32)>,
    Option<f32>,
    Option<MidLayoutBounds>,
);

fn empty_quota_radius(c: &Composition) -> f32 {
    let count = array(&c.figure["quotas"])
        .iter()
        .filter(|q| q["used"].is_number())
        .count()
        + usize::from(c.figure["cache"]["left"].is_number() || c.figure["cache"]["cold"] == true)
        + usize::from(c.codex_week["percent"].is_number())
        + usize::from(!has_week(c));
    ((IW / count.max(1) as f32 - S3) / 2.).clamp(30., 64.)
}
fn empty_quota_height(c: &Composition) -> f32 {
    P + theme::TITLE_LINE + S2 + 2. * empty_quota_radius(c) + 46. + theme::CAPTION_LINE + P
}

fn context_title_height(g: &Value) -> f32 {
    P + theme::TITLE_LINE
        + S2
        + if g["trend"].as_f64().is_some() {
            theme::CAPTION_LINE + S3
        } else {
            0.
        }
}

// Radius uses the available-height formula; the card then hugs the measured group.
fn collapsed_context_layout(g: &Value, h: f32) -> (f32, f32, f32) {
    let title = context_title_height(g);
    let below_ring = if payback(g).is_empty() {
        S3
    } else {
        S4 + theme::BODY_LINE + S3
    } + 30.
        + 46.
        + theme::CAPTION_LINE
        + S1;
    let available = (h - P - title - below_ring).max(0.);
    let radius = (available / 2.8).clamp(78., 118.);
    (
        radius,
        available,
        title + 2. * (radius + S3) + below_ring + P,
    )
}

fn collapsed_context_fits(g: &Value, h: f32) -> bool {
    let (_, _, needed) = collapsed_context_layout(g, h);
    needed <= h + 0.5
}

/// Fresh-only history. Cached context never sets the bar height or its vertical scale.
fn history_plot(c: &mut Canvas, f: &Fonts, data: &Composition, y: f32, h: f32, t: &Tokens) {
    let Some(normalized) = fresh_bar_heights(&data.requests) else {
        return;
    };
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    let base = y + S3 + f.cap(caption);
    c.text(f, "Fresh tokens / turn", IX, base, caption, t.quiet, A::L);
    let max = data
        .requests
        .iter()
        .map(|(_, fresh)| *fresh)
        .fold(0., f64::max);
    let legend = format!(
        "input + cache write · max {}{}",
        short(max),
        if data.request_compactions.is_empty() {
            ""
        } else {
            " · | compacted"
        }
    );
    c.text(f, &legend, W - IX, base, caption, t.quiet, A::R);
    let top = y + S3 + theme::CAPTION_LINE + S1;
    let bottom = y + h - S2;
    let plot_h = (bottom - top).max(1.);
    let step = IW / normalized.len() as f32;
    c.hair(IX, bottom, IW, t.track);
    let heights: Vec<_> = normalized.iter().map(|v| v * plot_h).collect();
    for (i, height) in heights.iter().enumerate() {
        c.rect(
            IX + i as f32 * step,
            bottom - height,
            step * 0.7,
            *height,
            t.fresh,
        );
    }
    for i in &data.request_compactions {
        c.line(
            IX + *i as f32 * step,
            top,
            IX + *i as f32 * step,
            bottom,
            t.hair_width,
            t.amber,
        );
    }
    c.fresh_plots.push(FreshPlot {
        region: (IX, top, IW, plot_h),
        heights,
        compactions: data.request_compactions.clone(),
    });
}

/// Draw the figure card. Returns (ceiling_y, ring_bounds) for layout metrics.
fn figure(
    c: &mut Canvas,
    f: &Fonts,
    data: &Composition,
    y: f32,
    h: f32,
    t: &Tokens,
    expanded: bool,
    narrow: bool,
) -> Option<FigureInfo> {
    let expanded = expanded && fresh_bar_heights(&data.requests).is_some();
    let g = &data.figure;
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    let size = g["ctx"]["size"].as_f64().filter(|n| *n > 0.);
    let collapsed_scaled_fits = collapsed_context_fits(g, h);
    // Strip only for narrow panes or truly strip-height cards. Mid-height cards get their own layout.
    const STRIP_H: f32 = 110.;
    let use_strip = tokens.is_some() && (narrow || h <= STRIP_H);
    let use_mid = tokens.is_some() && !expanded && !use_strip && !collapsed_scaled_fits;
    panel(c, y, h, t);
    if use_strip {
        return Some(draw_context_strip(c, f, data, y, h, t, tokens, size));
    }
    if use_mid {
        return Some(draw_context_mid(c, f, data, y, h, t, tokens, size));
    }
    let ceiling_y = None;
    let mut ring_bounds = None;
    let mut ring_group = None;
    let mut ring_formula_avail = None;
    let mut collapsed_pay_y = None;
    let mut collapsed_sat_cy = None;
    let mut context_quota_radius = 30.;
    label(
        c,
        f,
        if tokens.is_some() {
            "Context"
        } else {
            "Quotas"
        },
        IX,
        y + P + f.cap(sans(theme::TITLE_SIZE, t.bold)),
        theme::TITLE_SIZE,
        t.ink,
        t.bold,
    );
    if let Some(trend) = g["trend"]
        .as_f64()
        .filter(|_| tokens.is_some() && !expanded)
    {
        label(
            c,
            f,
            &format!(
                "{}{}/turn",
                if trend >= 0. { "+" } else { "−" },
                short(trend.abs())
            ),
            IX,
            y + P + theme::TITLE_LINE + S2 + f.cap(sans(theme::CAPTION_SIZE, t.regular)),
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
            y + P + f.cap(serif(24., t.medium)),
            serif(24., t.medium),
            t.ink,
            A::M,
        );
        label(
            c,
            f,
            "by CSR",
            W - 84.,
            y + P + f.cap(serif(24., t.medium)),
            10.,
            t.quiet,
            t.regular,
        );
    }
    if let Some((line, sev)) = csr_status(g) {
        let col = if sev == "red" { t.risk } else { t.amber };
        c.icon(
            f,
            Icon::CircleAlert,
            IX + 87.,
            y + P + f.cap(sans(theme::TITLE_SIZE, t.bold)),
            12.,
            10.,
            col,
        );
        single(
            c,
            f,
            &line,
            IX + 105.,
            y + P + f.cap(sans(theme::TITLE_SIZE, t.bold)),
            220.,
            10.,
            t.quiet,
            t,
        );
    }
    let mut expanded_chart_bottom = y + 195.;
    if let (Some(n), Some(size)) = (tokens, size) {
        if expanded {
            // Fresh input has its own scale. A context-window ceiling/band would be a different unit.
            let gauge_cy = y + h - 84.;
            let chart_bottom = (gauge_cy - 72.).max(y + 135.);
            expanded_chart_bottom = chart_bottom;
            history_plot(c, f, data, y + 70., chart_bottom - (y + 70.) + S2, t);
            if let Some(trend) = g["trend"].as_f64() {
                label(
                    c,
                    f,
                    &format!(
                        "{}{}/turn",
                        if trend >= 0. { "+" } else { "−" },
                        short(trend.abs())
                    ),
                    IX,
                    chart_bottom + 15.,
                    11.5,
                    t.quiet,
                    t.regular,
                );
            }
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
            // Collapsed: r = clamp(78, available_height/2.8, 118) after fixed payback/gauge reserve.
            // Compact fallback already handled above when the min group cannot fit.
            const GAP_RING_PAY: f32 = 16.;
            const PAY_H: f32 = 18.;
            const GAP_PAY_GAUGE: f32 = 12.;
            const LABEL_DESCENT: f32 = theme::CAPTION_LINE + 4.;
            let title_bottom = y + context_title_height(g);
            let (r, available_height, _) = collapsed_context_layout(g, h);
            let ring_extent = r + S3;
            let needed = collapsed_context_layout(g, h).2;
            let slack = (h - needed).max(0.);
            let history_h = if fresh_bar_heights(&data.requests).is_some() && slack >= 50. {
                slack
            } else {
                0.
            };
            if history_h == 0. {
                context_quota_radius = (30. + slack / 4.).min(54.).min(empty_quota_radius(data));
            }
            let gauge_growth = 2. * (context_quota_radius - 30.);
            let group_top = title_bottom
                + if history_h > 0. {
                    0.
                } else {
                    (slack - gauge_growth) / 2.
                };
            let cy = group_top + ring_extent;
            let cx = W / 2.;
            if history_h > 0. {
                history_plot(c, f, data, cy + ring_extent, history_h, t);
            }
            let pay_y = cy + ring_extent + history_h + GAP_RING_PAY;
            let sat_cy = if payback(g).is_empty() {
                cy + ring_extent + history_h + S3 + context_quota_radius
            } else {
                pay_y + PAY_H + GAP_PAY_GAUGE + context_quota_radius
            };
            let group_bottom = sat_cy + context_quota_radius + S4 + LABEL_DESCENT;
            ring_bounds = Some((cx, cy, r));
            ring_group = Some((group_top, group_bottom));
            ring_formula_avail = Some(available_height);
            collapsed_pay_y = Some(pay_y);
            collapsed_sat_cy = Some(sat_cy);
            c.dots(cx, cy, ring_extent, 72, 0.65, &|_| t.band);
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
            let num_size = (r * 0.55).clamp(28., 44.);
            c.text(
                f,
                &short(n),
                cx,
                cy + 7.,
                serif(num_size, t.medium),
                t.ink,
                A::M,
            );
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
    // Gauge labels stay >= 24pt above the card bottom; collapsed uses the exact group geometry.
    let quota_radius = if tokens.is_none() {
        ((h - (P + theme::TITLE_LINE + S2 + 46. + theme::CAPTION_LINE + P)) / 2.)
            .clamp(30., empty_quota_radius(data))
    } else {
        context_quota_radius
    };
    let (sat_cy, pay_y) = if tokens.is_none() {
        (
            y + P + theme::TITLE_LINE + S2 + quota_radius,
            y + P + theme::TITLE_LINE,
        )
    } else if expanded {
        let sat = y + h - 84.;
        let below_chart = expanded_chart_bottom + 33.;
        let above_gauges = sat - 36.;
        (
            sat,
            below_chart
                .min(above_gauges)
                .max(expanded_chart_bottom + 18.),
        )
    } else if let (Some(pay), Some(sat)) = (collapsed_pay_y, collapsed_sat_cy) {
        (sat, pay)
    } else {
        (y + h - 84., y + h - 120.)
    };
    if g["compacting"].is_object() {
        if let Some(high) = g["compacting"]["usual"]["high"]
            .as_f64()
            .filter(|n| *n > 0.)
        {
            let bar_y = pay_y + 10.;
            c.rrect(W / 2. - 60., bar_y, 120., 3., 1.5, t.track);
            c.rrect(
                W / 2. - 60.,
                bar_y,
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
            pay_y,
            sans(13., t.medium),
            if g["payback"]["sev"] == "amber" {
                t.amber
            } else {
                t.mint
            },
            A::M,
        );
    }
    quota_row(
        c,
        f,
        data,
        IX,
        IW,
        sat_cy,
        quota_radius,
        sat_cy + quota_radius + S4,
        theme::CAPTION_SIZE,
        t,
    );
    Some((ceiling_y, ring_bounds, ring_group, ring_formula_avail, None))
}

/// Compact side-ring strip for narrow panes or cards ≤ ~110 pt.
fn draw_context_strip(
    c: &mut Canvas,
    f: &Fonts,
    data: &Composition,
    y: f32,
    h: f32,
    t: &Tokens,
    tokens: Option<f64>,
    size: Option<f64>,
) -> FigureInfo {
    let g = &data.figure;
    let lines = figure_lines(data);
    let ctx = tokens;
    let mut x = IX;
    if let (Some(n), Some(size)) = (ctx, size) {
        let (cx, cy) = (IX + 28., y + h / 2.);
        let mut segs = vec![(0., (n / size).clamp(0., 1.) as f32, t.mint)];
        if let (Some(low), Some(high)) = (g["band"]["low"].as_f64(), g["band"]["high"].as_f64()) {
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
    if let Some(line) = lines.first() {
        let line = if ctx.is_some() {
            line.split(" · ").skip(1).collect::<Vec<_>>().join(" · ")
        } else {
            line.clone()
        };
        single(c, f, &line, x, y + P + 10., W - x - P - M, 11.5, t.ink, t);
    }
    quota_row(
        c,
        f,
        data,
        x,
        W - x - P - M,
        y + 48.,
        16.,
        y + h - 28.,
        10.,
        t,
    );
    (None, None, None, None, None)
}

/// Mid-height Context: left ring + right small gauges + compact details, vertically centred.
fn draw_context_mid(
    c: &mut Canvas,
    f: &Fonts,
    data: &Composition,
    y: f32,
    h: f32,
    t: &Tokens,
    tokens: Option<f64>,
    size: Option<f64>,
) -> FigureInfo {
    let g = &data.figure;
    const TITLE: f32 = theme::TITLE_LINE + S2;
    const BOTTOM_CLEAR: f32 = S4;
    label(
        c,
        f,
        "Context",
        IX,
        y + P + f.cap(sans(theme::TITLE_SIZE, t.bold)),
        theme::TITLE_SIZE,
        t.ink,
        t.bold,
    );
    if let Some((line, sev)) = csr_status(g) {
        let col = if sev == "red" { t.risk } else { t.amber };
        c.icon(
            f,
            Icon::CircleAlert,
            IX + 87.,
            y + P + f.cap(sans(theme::TITLE_SIZE, t.bold)),
            12.,
            10.,
            col,
        );
        single(
            c,
            f,
            &line,
            IX + 105.,
            y + P + f.cap(sans(theme::TITLE_SIZE, t.bold)),
            220.,
            10.,
            t.quiet,
            t,
        );
    }
    let (Some(n), Some(size)) = (tokens, size) else {
        return (None, None, None, None, None);
    };
    let radius_inner = (h - TITLE - 2. * P) / 2. - S1;
    let variable_history = fresh_bar_heights(&data.requests).is_some();
    let radius_max = if variable_history { 64. } else { 96. };
    let r = radius_inner.clamp(25., radius_max);

    let title_bottom = y + P + TITLE;
    let slot_bottom = y + h - BOTTOM_CLEAR;
    let slot = (slot_bottom - title_bottom).max(0.);
    let gauge_r = (r * 0.32).clamp(16., if variable_history { 20. } else { 30. });

    let label_below = gauge_r + 12.;
    let details_gap = S3;
    let details_h = 6.;
    let right_h = gauge_r + label_below + theme::CAPTION_LINE + details_gap + details_h;
    let core_h = (2. * r).max(right_h);
    let history_h = if fresh_bar_heights(&data.requests).is_some() && slot - core_h - S3 >= 50. {
        slot - core_h - S3
    } else {
        0.
    };
    let group_h = core_h + if history_h > 0. { S3 + history_h } else { 0. };
    let group_top = title_bottom + (slot - group_h).max(0.) / 2.;
    let group_bottom = group_top + group_h;
    let ring_cx = IX + r;
    let ring_cy = group_top + core_h / 2.;

    let right_x0 = ring_cx + r + S4;
    let right_w = (W - M - P - right_x0).max(80.);
    let right_top = group_top + (core_h - right_h).max(0.) / 2.;
    let gauges_cy = right_top + gauge_r;
    let gauge_label_y = gauges_cy + label_below;
    let details_y = gauge_label_y + theme::CAPTION_LINE + details_gap + 2.;

    let mut segs = vec![(0., (n / size).clamp(0., 1.) as f32, t.mint)];
    if let (Some(low), Some(high)) = (g["band"]["low"].as_f64(), g["band"]["high"].as_f64()) {
        segs.push(((low / size) as f32, (high / size) as f32, t.band));
    }
    c.ring(ring_cx, ring_cy, r, t.ring, t.track, &segs);
    if let Some(trend) = g["trend"].as_f64() {
        c.tick(
            ring_cx,
            ring_cy,
            r - 3.,
            r + 4.,
            ((n + trend) / size).clamp(0., 1.) as f32,
            t.thin_ring,
            t.mint,
        );
    }
    let num_size = (r * 0.56).clamp(12., 28.);
    c.text(
        f,
        &short(n),
        ring_cx,
        ring_cy + 5.,
        serif(num_size, t.medium),
        t.ink,
        A::M,
    );
    c.text(
        f,
        &format!("of {}", short(size)),
        ring_cx,
        ring_cy + num_size * 0.55 + 4.,
        sans((r * 0.28).clamp(7.5, 11.), t.medium),
        t.quiet,
        A::M,
    );

    let (gauges_left, gauges_right) = quota_row(
        c,
        f,
        data,
        right_x0,
        right_w,
        gauges_cy,
        gauge_r,
        gauge_label_y,
        theme::CAPTION_SIZE,
        t,
    );

    let mut details = Vec::new();
    if let Some(trend) = g["trend"].as_f64() {
        details.push(format!(
            "{}{}/turn",
            if trend >= 0. { "+" } else { "−" },
            short(trend.abs())
        ));
    }
    let pay = payback(g);
    if !pay.is_empty() {
        details.push(pay);
    }
    if let Some(n) = data.tally["by_csr"].as_u64().filter(|n| *n > 0) {
        details.push(format!("{n} by CSR"));
    }
    if !details.is_empty() {
        let line = details.join(" · ");
        let col = if g["payback"]["sev"] == "amber" {
            t.amber
        } else if g["compacting"].is_object() {
            t.mint
        } else {
            t.quiet
        };
        single(c, f, &line, right_x0, details_y, right_w, 10.5, col, t);
    }
    if g["compacting"].is_object() {
        if let Some(high) = g["compacting"]["usual"]["high"]
            .as_f64()
            .filter(|n| *n > 0.)
        {
            let bar_y = details_y + 8.;
            let bar_w = right_w.min(120.);
            c.rrect(right_x0, bar_y, bar_w, 3., 1.5, t.track);
            c.rrect(
                right_x0,
                bar_y,
                bar_w * (num(&g["compacting"]["elapsed"]) / high).clamp(0., 1.) as f32,
                3.,
                1.5,
                t.mint,
            );
        }
    }

    if history_h > 0. {
        history_plot(c, f, data, group_top + core_h + S3, history_h, t);
    }
    let mid = MidLayoutBounds {
        ring: (ring_cx, ring_cy, r),
        group: (group_top, group_bottom),
        gauge_r,
        gauges_cy,
        gauge_label_y,
        details_y,
        radius_inner,
        radius_max,
        gauges_left,
        gauges_right,
    };
    (None, None, None, None, Some(mid))
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
    } else if c.offer {
        let row = text::wrap(&format!("☾ {}", c.question()), cols.max(1), 1000).len()
            + usize::from(!meta(c).is_empty());
        out.insert(
            row,
            json!({
                "key": "instrument:dreams-on",
                "spans": [[0, cols.min(18), "dreams-on"]],
            }),
        );
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
    let h = 118.;
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
        if let Some(heights) = fresh_bar_heights(&data.requests).filter(|_| expanded) {
            let bw = 48. / heights.len() as f32;
            for (i, height) in heights.iter().enumerate() {
                let bar = height * 28.;
                c.rect(12. + i as f32 * bw, 58. - bar, bw * 0.7, bar, t.fresh);
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
    quota_row(&mut c, f, data, 76., w - 86., 65., 16., 93., 8., t);
    Some(c.pm)
}

#[cfg(test)]
mod tests {
    #[test]
    fn weekly_caption_raster_uses_label_and_csr_inks_on_one_line() {
        let f = Fonts::load();
        let mut data = Composition::build(&json!({}), &json!({"id":"caption"}), 1000.);
        data.csr_cost.share = Some(0.9);
        let q = json!({"code":"7D","used":57});
        assert_eq!(quota_caption(&data, &q), "7D · CSR 0.9 pts");
        for theme in [Theme::Dark, Theme::Light] {
            let t = theme.tokens();
            let mut c = Canvas::new(240., 60.);
            c.background(t);
            weekly_caption(&mut c, &f, &data, &q, 120., 30., 200., 12., t);
            for ink in [t.quiet, t.csr_share] {
                let pixels = c
                    .pm
                    .data()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| (0..3).all(|i| (i16::from(p[i]) - i16::from(ink[i])).abs() < 20))
                    .count();
                assert!(pixels > 3, "{theme:?}: missing caption ink {ink:?}");
            }
        }
    }

    use super::*;

    #[test]
    fn quota_money_drops_this_week_before_a_caption_can_hit_its_neighbour() {
        let fonts = Fonts::load();
        for theme in [Theme::Dark, Theme::Light] {
            let t = theme.tokens();
            let line = "$18.22 this week";
            let natural = fonts.width(line, sans(theme::CAPTION_SIZE, t.medium));
            assert_eq!(
                quota_money_caption(&fonts, line, natural + 1., theme::CAPTION_SIZE, t),
                line
            );
            assert_eq!(
                quota_money_caption(&fonts, line, natural - 1., theme::CAPTION_SIZE, t),
                "$18.22"
            );
        }
    }

    #[test]
    fn fresh_series_uses_uncached_input_and_writes_and_suppresses_flat_history() {
        let bars = [(188_000., 2_000.), (180_000., 10_000.), (150_000., 40_000.)];
        let heights = fresh_bar_heights(&bars).unwrap();
        assert!((heights[2] - heights[0]) >= 0.4);
        assert_eq!(heights[2], 1.);
        assert!(fresh_bar_heights(&[(188_000., 2000.), (187_900., 2100.)]).is_none());
        assert!(fresh_bar_heights(&[(190_000., 0.), (190_000., 0.)]).is_none());
        assert!(fresh_bar_heights(&[(190_000., 0.), (150_000., 40000.)]).is_some());
    }

    #[test]
    fn compaction_markers_attach_to_equal_timestamp_requests_before_tail_clipping() {
        let s = json!({"replies":(1..=30).map(|t|json!([t,190000,100,1000,188000])).collect::<Vec<_>>(),"compacts":[2.,7.,20.,31.]});
        assert_eq!(recent_request_compactions(&s), vec![0, 13]);
    }

    #[test]
    fn story_fit_preserves_full_text_and_cuts_at_longest_boundary() {
        let fonts = Fonts::load();
        let st = sans(12.5, 400.);
        let short = "Text parity is ready for review.";
        assert_eq!(wrap_clauses(&fonts, short, st, IW, 4).join(" "), short);
        assert_eq!(story_text_lines(short, 80, 4).join(" "), short);

        let long = "First clause ends here. Second clause continues for a while. Third clause still goes. Fourth would overflow the pane budget entirely.";
        let pixel = wrap_clauses(&fonts, long, st, 220., 2).join(" ");
        let text_path = story_text_lines(long, 40, 2).join(" ");
        for joined in [&pixel, &text_path] {
            assert!(joined.ends_with('…'), "{joined}");
            assert!(joined.contains("First clause ends here."));
            assert!(
                !joined.contains("overflow the pane"),
                "must drop the overflowing clause: {joined}"
            );
        }
        // Longest fitting boundary: prefer keeping more clauses when width allows.
        let wider = wrap_clauses(&fonts, long, st, 400., 2).join(" ");
        assert!(
            wider.contains("Second clause") || wider.contains("First clause ends here."),
            "{wider}"
        );
    }

    #[test]
    fn story_fit_handles_emdash_non_ascii_and_clause_free_overflow() {
        let fonts = Fonts::load();
        let st = sans(12.5, 400.);
        let em = "Alpha stays — beta is dropped because it is far too long for the budget here.";
        let cut = wrap_clauses(&fonts, em, st, 160., 1).join(" ");
        assert!(
            cut.ends_with("—…") || cut.ends_with("—…"),
            "keep spaced emdash: {cut}"
        );
        assert!(!cut.contains("beta is dropped"), "{cut}");
        let text_em = story_text_lines(em, 28, 1).join(" ");
        assert!(text_em.contains('—') || text_em == "…", "{text_em}");

        let ja = "最初の文です。次の文は長くて枠に入りませんでした。";
        let ja_cut = story_text_lines(ja, 12, 1).join(" ");
        assert!(ja_cut.ends_with('…') || ja_cut.ends_with("。…"), "{ja_cut}");

        // Many short words (no punctuation) so wrap exceeds the line budget without a clause cut.
        let no_clause = "aa bb cc dd ee ff gg hh ii jj kk ll mm nn oo pp qq rr ss tt";
        assert_eq!(
            wrap_clauses(&fonts, no_clause, st, 60., 1),
            vec!["…".to_string()]
        );
        assert_eq!(story_text_lines(no_clause, 8, 1), vec!["…".to_string()]);
        assert!(wrap_clauses(&fonts, no_clause, st, 60., 0).is_empty());
        assert!(story_text_lines(no_clause, 8, 0).is_empty());
    }

    #[test]
    fn the_cache_ring_redraws_once_a_minute_not_every_second() {
        let draw_at = |left: f64| {
            let mut c = Composition::build(&json!({}), &json!({"id":"cache"}), 1000.);
            c.figure["cache"] = json!({"left": left, "ttl": 3600., "cold": false, "text": "CACHE"});
            draw(&c, 704, 986, Theme::Light, Open::default(), false)
                .expect("instrument draws")
                .0
        };
        // Both read "59m": a second's sweep must not repaint the whole picture.
        assert!(draw_at(3599.).data() == draw_at(3540.).data());
        assert!(draw_at(3540.).data() != draw_at(3539.).data());
    }
}
