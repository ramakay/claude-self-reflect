//! The single companion surface. Both renderers consume this composition, built from live feeds.
#![allow(clippy::too_many_arguments)]
use super::{
    instrument_gfx::*,
    instrument_icons::{self as icons, Icon},
    observation::{self, Detail, ObsCard, PopupAt},
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
}
/// What a threads-timeline row is about. Set where the row is built, never parsed from its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiverKind {
    /// Code changed with no commit receipt yet.
    Write,
    Compact,
    /// Something the user raised.
    Prompt,
    Dream,
    Agent,
    /// A shift backed by a commit.
    Commit,
    Other,
}
#[derive(Clone, Debug)]
pub struct RiverRow {
    /// Where the row sorts. For an undated row this is a stand-in and is never shown.
    pub at: f64,
    /// When the row happened, if the data says. `None` draws no time label: a row never shows a
    /// time borrowed from somewhere else.
    pub when: Option<f64>,
    pub kind: RiverKind,
    /// The glyph the plain-text pane leads the row with. The picture draws the kind's icon instead.
    pub lead: &'static str,
    pub line: String,
    /// The mood word. Fixed at build time: a thread's word depends on its state, which the kind
    /// and lead do not carry.
    pub mood: &'static str,
    /// The thread behind a Lost and found row (lost, open or carried): its popup is the handoff.
    pub thread: Option<Value>,
}
impl RiverRow {
    /// The picture's text: the kind's icon replaces the lead glyph, except a shift at risk.
    fn picture_text(&self) -> String {
        if self.lead == "! " {
            format!("{}{}", self.lead, self.line)
        } else {
            self.line.clone()
        }
    }
    /// The row's time label against `now`, or empty when the row has no time of its own.
    pub fn time_label(&self, now: f64) -> String {
        self.when.map(|at| time_label(at, now)).unwrap_or_default()
    }
    /// The row's past-tense mood word, from a fixed table.
    pub fn mood(&self) -> &'static str {
        self.mood
    }
}
fn river_row(at: f64, kind: RiverKind, lead: &'static str, line: String) -> RiverRow {
    RiverRow {
        at,
        when: (at > 0.).then_some(at),
        kind,
        lead,
        line,
        mood: mood(kind, lead),
        thread: None,
    }
}
pub use super::instrument_mood::{mood, started_lead, thread_mood};
/// A timeline row's time against `now`, in `tz`: "just now" under a minute, "N min ago" under an
/// hour, "HH:MM" on the same local day, else "Mon D HH:MM".
pub fn time_label_in<Tz: chrono::TimeZone>(at: f64, now: f64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let ago = now - at;
    if ago < 60. {
        return "just now".into();
    }
    if ago < 3600. {
        return format!("{} min ago", (ago / 60.).floor() as i64);
    }
    let Some(then) = tz.timestamp_opt(at.floor() as i64, 0).single() else {
        return String::new();
    };
    let today = tz.timestamp_opt(now.floor() as i64, 0).single();
    if today.is_some_and(|t| t.date_naive() == then.date_naive()) {
        then.format("%H:%M").to_string()
    } else {
        then.format("%b %-d %H:%M").to_string()
    }
}
pub fn time_label(at: f64, now: f64) -> String {
    time_label_in(at, now, &chrono::Local)
}
/// The latest edit of the one file in this session that `scope` names. `s["edit_at"]` maps each
/// edited path to its last edit time; a scope matches a path equal to it or ending in `/scope`.
/// Two files answering to one scope is ambiguous, so it dates nothing.
fn edit_time(scope: &str, s: &Value) -> Option<f64> {
    let files = s["edit_at"].as_object()?;
    let scope = scope.trim_start_matches("./");
    if scope.is_empty() {
        return None;
    }
    let suffix = format!("/{scope}");
    let mut hits = files
        .iter()
        .filter(|(path, _)| path.as_str() == scope || path.ends_with(&suffix));
    let (_, at) = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    at.as_f64().filter(|at| *at > 0.)
}
/// What a shift row is and when it happened. A shift that names a commit (`oid:` receipt) is a
/// commit row, dated by that commit among the session's commit shifts (`at` is the commit time;
/// `date` is only its day, used when `at` is missing). Any other shift is an unlanded code edit.
fn shift_when(entry: &Value, commits: &[Value]) -> (RiverKind, Option<f64>) {
    let oid = array(&entry["receipts"])
        .iter()
        .filter_map(Value::as_str)
        .find_map(|r| r.strip_prefix("oid:"))
        .filter(|oid| !oid.is_empty());
    let Some(oid) = oid else {
        return (RiverKind::Write, None);
    };
    let at = commits
        .iter()
        .filter(|c| string(&c["oid"]).starts_with(oid))
        .find_map(|c| {
            c["at"]
                .as_f64()
                .filter(|at| *at > 0.)
                .or_else(|| crate::companion::paths::epoch(string(&c["date"])))
        });
    (RiverKind::Commit, at)
}
/// The raw times behind a few of the labels the picture shows. The labels are what is drawn; these
/// only say when a label next changes, so they stay out of the draw key.
#[derive(Clone, Debug, Default)]
pub struct Clocks {
    /// When the story was told.
    pub story_at: Option<f64>,
    /// When the "Compacted." notice goes away.
    pub notice_until: Option<f64>,
    /// Seconds the running compaction has taken, as read.
    pub compacting_elapsed: Option<f64>,
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
    /// What confused Claude, across every look of this session, newest first.
    pub confusing: Vec<Value>,
    /// What should be a skill, memory or tool, across every look of this session.
    pub should_be: Vec<Value>,
    /// The rows the last frame drew shortened, each `{title, line}` with its words in full. The
    /// pane fills this from the frame it drew; a popup on one of them reads it.
    pub more: Vec<Value>,
    /// The Lost and found threads the last frame drew, each with what the handoff popup needs.
    pub lost: Vec<Value>,
    /// What CSR told this session, as the "What CSR told Claude" popup reads it: one item, or none.
    pub handed: Vec<Value>,
    /// This session's open Lost and found threads, newest first: the card on the main view.
    pub open_lost: Vec<Value>,
    pub story: String,
    pub story_age: String,
    pub shifts: Vec<Value>,
    pub figure: Value,
    pub tally: Value,
    pub agents: agents::Projection,
    pub river: Vec<RiverRow>,
    pub notice: Option<String>,
    pub asked_away: bool,
    pub clocks: Clocks,
    /// The live eval of a running trial, when one is on show: the CSR performance card.
    pub soak: Option<crate::companion::live_eval::Card>,
    /// The API credit left by the api-lanes ledger, when it keeps one.
    pub credit: Option<crate::companion::api_credit::Credit>,
    pub now: f64,
}
/// The layout width in pt that the pictures' x positions are measured in.
pub const LAYOUT_WIDTH: f32 = W;
/// One of the circles of the quota row. A tap on a circle opens the `i` page cut to its facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gauge {
    FiveHour,
    Week,
    Cache,
    Codex,
}
impl Gauge {
    /// The code the circle carries in `Metrics.quota_gauges` and in a pane action.
    pub fn code(self) -> &'static str {
        match self {
            Gauge::FiveHour => "5H",
            Gauge::Week => "7D",
            Gauge::Cache => "cache",
            Gauge::Codex => "Codex 7D",
        }
    }
    pub fn from_code(code: &str) -> Option<Gauge> {
        [Gauge::FiveHour, Gauge::Week, Gauge::Cache, Gauge::Codex]
            .into_iter()
            .find(|g| g.code() == code)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Open {
    pub scroll: usize,
    pub agents: bool,
    pub figure: bool,
    /// With `figure`: the one circle whose facts the page shows. `None` lists every fact.
    pub gauge: Option<Gauge>,
    pub changes: bool,
    pub threads: bool,
    pub dreams: bool,
    /// An observation item shown in full over the pane.
    pub popup: Option<PopupAt>,
    /// The stacked card open under the stack row, as a `STACKABLE` index.
    pub stack: Option<usize>,
}
/// The threads timeline's heading and its subtitle.
const MONOLOGUE: &str = "Monologue";
const MONOLOGUE_SUB: &str = "the session thinking out loud";
/// Room the Monologue heading and subtitle take above the first row, in layout points.
const MONOLOGUE_H: f32 = 42.;
/// A timeline row's band height and the distance from one band to the next.
const ROW_BAND: f32 = 29.;
const ROW_PITCH: f32 = 33.;
const ROW_GAP: f32 = ROW_PITCH - ROW_BAND;
/// How strongly a row's kind colour tints its band over the card.
const BAND_ALPHA: f32 = 0.10;
/// Shear of the fake italic the mood word is drawn in.
const ITALIC_SLANT: f32 = 0.2;
/// How many confused-Claude or should-be items a card keeps.
const OBSERVATION_CAP: usize = 5;
/// Every look of this session that carries `key`, newest first, one item per event, less what was
/// dismissed. A later look with an empty list never erases what an earlier one found. Each look
/// words the same event anew, so rows that tell one event (`observation::same_event`) are one
/// item: the newest wording, with its own evidence. An event is dismissed when any wording of it
/// was.
fn observations(data: &Value, session: &Value, key: &str) -> Vec<Value> {
    let mut sources: Vec<&Value> = array(&data["dreams"])
        .iter()
        .chain(array(&data["dream_records"]))
        .filter(|r| &r["session"] == session && r[key].is_array())
        .collect();
    // The later record wins a tie on `at`: the list is in append order and this sort is stable.
    sources.reverse();
    sources.sort_by(|a, b| num(&b["at"]).total_cmp(&num(&a["at"])));
    // Each event: its newest wording, every wording's key, and whether one was dismissed.
    let mut events: Vec<(Value, Vec<observation::EventKey>, bool)> = vec![];
    for item in sources.iter().flat_map(|r| array(&r[key])) {
        if string(&item["line"]).is_empty() || string(&item["evidence"]).is_empty() {
            continue;
        }
        let gone = data["dismissed"]
            [observation::dismiss_key(string(session), key, string(&item["line"]))]
            == true;
        let this = observation::event_key(item);
        // A row can link events that an older wording has not joined yet: it makes them one, under
        // the newest head, so the order of the looks does not decide the grouping.
        let known: Vec<usize> = (events.iter().enumerate())
            .filter(|(_, (head, keys, _))| {
                head["line"] == item["line"]
                    || keys.iter().any(|k| observation::same_event(k, &this))
            })
            .map(|(at, _)| at)
            .collect();
        match known.split_first() {
            Some((&first, rest)) => {
                for &at in rest.iter().rev() {
                    if observation::turns_conflict(&events[first].1, &events[at].1) {
                        continue;
                    }
                    let (_, keys, dismissed) = events.remove(at);
                    events[first].1.extend(keys);
                    events[first].2 |= dismissed;
                }
                events[first].1.push(this);
                events[first].2 |= gone;
            }
            None => events.push((item.clone(), vec![this], gone)),
        }
    }
    events
        .into_iter()
        .filter(|(_, _, dismissed)| !dismissed)
        .map(|(head, _, _)| head)
        .take(OBSERVATION_CAP)
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
        let offer = dreams::offered(data);
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
        let mut river: Vec<_> = array(&data["dreams"])
            .iter()
            .filter(|d| d["session"] == s["id"])
            .map(|d| {
                river_row(
                    num(&d["at"]),
                    RiverKind::Dream,
                    "☾ ",
                    format!(
                        "{}{}",
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
                .map(|v| river_row(num(v), RiverKind::Compact, "♻ ", "Compacted".into())),
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
        let commits = array(&data["shifts"]);
        for shift in &shifts {
            let (kind, commit_at) = shift_when(shift, commits);
            // An unlanded edit is dated by its file's last edit in this session. The map's own
            // time stands in for sorting only: it belongs to the whole map, not to this row.
            let at = commit_at.or_else(|| {
                (kind == RiverKind::Write)
                    .then(|| edit_time(string(&shift["scope"]), s))
                    .flatten()
            });
            let mut row = river_row(
                at.unwrap_or_else(|| num(&data["story_map"]["at"])),
                kind,
                match mark(shift) {
                    "!" => "! ",
                    "+" => "+ ",
                    "-" => "- ",
                    _ => "~ ",
                },
                format!("{} · {}", string(&shift["scope"]), string(&shift["now"])),
            );
            if at.is_none() {
                row.when = None;
            }
            river.push(row);
        }
        for agent in &projection.agents {
            if let Some(start) = agent.started {
                river.push(river_row(
                    start / 1000.,
                    RiverKind::Agent,
                    started_lead(agent, now),
                    format!("{} started", agent.description),
                ));
            }
            if let Some(done) = &agent.completion {
                river.push(river_row(
                    done.at / 1000.,
                    RiverKind::Agent,
                    "✓ ",
                    format!("{} finished", agent.description),
                ));
            }
        }
        river.extend(
            array(&data["threads"])
                .iter()
                .filter(|thread| {
                    !crate::companion::lost::is_summary_footer(string(&thread["text"]))
                })
                .map(|thread| {
                    let mut row = river_row(
                        num(&thread["last_seen"]),
                        RiverKind::Prompt,
                        "↳ ",
                        format!(
                            "{} · {}",
                            string(&thread["text"]),
                            crate::companion::threads::thread_group(thread)
                        ),
                    );
                    row.mood = thread_mood(thread);
                    if matches!(string(&thread["state"]), "lost" | "open" | "carried") {
                        let mut item = thread.clone();
                        item["line"] = thread["text"].clone();
                        row.thread = Some(item);
                    }
                    row
                }),
        );
        river.sort_by(|a, b| b.at.total_cmp(&a.at));
        let mut stamps: Vec<_> = array(&s["main"])
            .iter()
            .map(|r| num(&r[0]))
            .chain(array(&s["prompts"]).iter().map(num))
            .collect();
        stamps.sort_by(f64::total_cmp);
        // Only the latest return counts: an hour-long gap earlier in the session is old news.
        let gap = stamps.windows(2).last().filter(|w| w[1] - w[0] > 3600.);
        // CSR's one-a-day warning that Claude Code's own window beat it: first, while it is up.
        let native_warn = data["native_warn"]["line"]
            .as_str()
            .filter(|line| !line.is_empty())
            .map(str::to_owned);
        let other_notice = gap
            .and_then(|w| {
                let since = w[0];
                let looks = dreams::looks_since(records, array(&data["look_stamps"]), since);
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
        let native_until = native_warn
            .as_ref()
            .and(data["native_warn"]["until"].as_f64());
        let notice = native_warn.or(other_notice);
        let card = &data["infinite"]["card"];
        let notice_until = native_until.or_else(|| {
            notice
                .as_deref()
                .filter(|n| n.starts_with("Compacted."))
                .and(card["at"].as_f64())
                .map(|at| at + 120.)
        });
        let clocks = Clocks {
            story_at: data["story"]["at"].as_f64(),
            notice_until,
            compacting_elapsed: data["compacting"]["elapsed"]
                .as_f64()
                .filter(|_| data["compacting"].is_object()),
        };
        Self {
            empty_session,
            repo_recap: serde_json::from_value(data["repo_recap"].clone())
                .ok()
                .filter(|_: &crate::companion::repo_recap::RepoRecap| empty_session),
            request_compactions: recent_request_compactions(s),
            offer,
            offer_notice: dreams::offer_notice(data),
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
            },
            confusing: crate::companion::same_page::card(
                data,
                s,
                observations(data, &s["id"], "confusing_claude"),
            ),
            should_be: observations(data, &s["id"], "should_be"),
            more: vec![],
            lost: vec![],
            handed: crate::companion::handed::item_of(s).into_iter().collect(),
            open_lost: crate::companion::start::open_threads(&s["lost"]),
            story,
            story_age: age,
            shifts,
            figure: with_layout(
                with_forecast(
                    masthead::gauges_compacting(
                        &data["claude_fields"],
                        data["claude_at"].as_f64(),
                        s,
                        now,
                        &data["limits"],
                        &data["sts"],
                        &drawn_compaction(&data["compacting"]),
                    ),
                    &data["forecast"],
                ),
                &data["rings"],
                &data["native_window"],
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
            clocks,
            soak: serde_json::from_value(data["soak"].clone()).ok(),
            credit: serde_json::from_value(data["api_credit"].clone()).ok(),
            now,
        }
    }
    /// The items of one observation card, newest first.
    pub fn items(&self, card: ObsCard) -> &[Value] {
        match card {
            ObsCard::Confusing => &self.confusing,
            ObsCard::ShouldBe => &self.should_be,
            ObsCard::More => &self.more,
            ObsCard::Lost => &self.lost,
            ObsCard::Handed => &self.handed,
        }
    }
    /// The popup's content for `at`, or `None` when that item is no longer on its card.
    pub fn detail(&self, at: PopupAt) -> Option<Detail> {
        observation::detail(self.items(at.card), at)
    }
    pub fn question(&self) -> String {
        match self.dream.state {
            _ if self.offer => "Turn dreams on.".into(),
            Batch::Ready => string(&self.dream.top["line"]).into(),
            _ if dreams::says_stopped(&self.offer_notice) => dreams::STOPPED_HEAD.into(),
            Batch::Dreaming => "Dreaming.".into(),
            Batch::Nothing | Batch::Empty => "The dreamer looks as you work.".into(),
        }
    }
    /// What the picture and the text rows show that depends on `now`, as the words and flags they
    /// show it in. Two compositions that differ only in `now` and agree here draw the same.
    fn time_labels(&self, open: Open) -> Vec<String> {
        let mut out = vec![];
        if open.threads {
            out.extend(self.river.iter().map(|row| row.time_label(self.now)));
        }
        if self.dream.state == Batch::Dreaming {
            out.push(status_line(self));
        }
        out.extend(
            self.agents
                .agents
                .iter()
                .filter(|a| agents::is_active(a, self.now))
                .map(|a| agents::stalled(a, self.now).to_string()),
        );
        out.extend(agent_lines(self));
        out
    }
    /// A hash of everything `draw`, `strip` and the text rows consume. Raw `now` is not in it: the
    /// labels it shows are, and the figure's continuous values are cut to what is drawn (the cache
    /// ring in minutes, the quotas' pace as ahead or not).
    pub fn draw_key(&self, open: Open) -> u64 {
        use std::{
            fmt::Write,
            hash::{Hash, Hasher},
        };
        struct Sink(std::collections::hash_map::DefaultHasher);
        impl Write for Sink {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                self.0.write(s.as_bytes());
                Ok(())
            }
        }
        let mut cut = self.clone();
        cut.now = 0.;
        cut.clocks = Clocks::default();
        if let Some(cache) = cut.figure["cache"].as_object_mut() {
            for key in ["left", "ttl"] {
                if let Some(n) = cache.get(key).and_then(Value::as_f64) {
                    cache.insert(key.into(), json!((n / 60.).floor() * 60.));
                }
            }
        }
        if let Some(quotas) = cut.figure["quotas"].as_array_mut() {
            for q in quotas {
                // Both predicates are drawn: the caption's `»` (used past pace) and the text
                // row's mark (used past pace by five points). Each is a bool in the key.
                if let Some(pace) = q["pace"].as_f64() {
                    let caption = num(&q["used"]) / 100. > pace;
                    q["pace"] = json!([caption, text::ahead_of_pace(q)]);
                }
            }
        }
        let mut sink = Sink(Default::default());
        let _ = write!(sink, "{cut:?}");
        self.time_labels(open).hash(&mut sink.0);
        sink.0.finish()
    }
    /// The first moment after `now` that something drawn changes with no new data: a minute
    /// edge of an age on screen, the cache ring's minute, the compaction's ten seconds, an agent
    /// turning stalled, a notice leaving, a quota passing its reset or its pace, midnight.
    pub fn next_change_at(&self, open: Open) -> Option<f64> {
        /// A deadline lands a hair after the edge so the label has changed when it is checked.
        const EDGE: f64 = 0.01;
        let now = self.now;
        let minute = |at: f64| at + 60. * (((now - at).max(0.) / 60.).floor() + 1.);
        let mut due: Vec<f64> = vec![];
        if open.threads {
            for at in self.river.iter().filter_map(|row| row.when) {
                due.push(if now - at < 3600. {
                    minute(at)
                } else {
                    next_midnight(now)
                });
            }
        }
        if self.dream.state == Batch::Dreaming {
            due.extend(self.dream.look["at"].as_f64().map(minute));
        }
        if !self.story_age.is_empty() {
            due.extend(self.clocks.story_at.map(minute));
        }
        due.extend(self.clocks.notice_until);
        due.extend(
            self.clocks
                .compacting_elapsed
                .map(|e| now + 10. - e.rem_euclid(10.)),
        );
        for a in self.agents.agents.iter() {
            if a.status == "running" && !agents::stalled(a, now) {
                due.push(a.last_at / 1000. + 600.);
            }
            // A waiting row drops out of the bar once it has been quiet for two hours.
            if a.status == "waiting" && agents::is_active(a, now) {
                due.push(a.last_at / 1000. + agents::WAITING_STALE);
            }
            if open.agents {
                // The label rounds to the nearest minute (`{:.0}m`): it moves at 30 s, 90 s, ...
                due.extend(a.started.map(|s| {
                    let began = s / 1000.;
                    let minutes = ((now - began) / 60.).max(0.);
                    began + 60. * ((minutes + 0.5).floor() + 0.5)
                }));
            }
        }
        let cache = &self.figure["cache"];
        if cache["cold"] != true {
            if let Some(left) = cache["left"].as_f64().filter(|n| *n > 0.) {
                due.push(now + left - 60. * (left / 60.).floor());
            }
        }
        // The outlook's lines count minutes: they move on the minute.
        if self.figure["outlook"]["windows"]
            .as_array()
            .is_some_and(|w| !w.is_empty())
        {
            due.push((now / 60.).floor() * 60. + 60.);
        }
        let quotas = array(&self.figure["quotas"]);
        for q in quotas {
            let Some(resets) = q["resets"].as_f64() else {
                continue;
            };
            due.push(resets);
            let len = match string(&q["code"]) {
                "5H" => 18000.,
                "7D" => 604800.,
                _ => continue,
            };
            // Pace appears when the window opens; "ahead" ends when the pace catches up.
            due.push(resets - len);
            // Each mark ends when the pace reaches the figure it is measured against: the
            // caption's `»` at the used share, the text row's mark five points below it.
            if let Some(used) = q["used"].as_f64() {
                due.push(resets - len * (1. - used / 100.));
                due.push(resets - len * (1. - (used / 100. - 0.05)));
            }
        }
        due.extend(self.soak.as_ref().and_then(|card| card.next_change));
        // The Codex week passes its reset on its own clock too: the figure it shows goes with it.
        due.extend(self.codex_week["resets_at"].as_f64());
        if quotas.iter().any(|q| q["resets"].is_number())
            || (open.threads && !self.river.is_empty())
        {
            due.push(next_midnight(now));
        }
        due.into_iter()
            .filter(|t| t.is_finite() && *t > now)
            .min_by(f64::total_cmp)
            .map(|t| t + EDGE)
    }
}
/// The first local midnight after `now`: a time label's date part changes there.
fn next_midnight(now: f64) -> f64 {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(now as i64, 0)
        .single()
        .and_then(|t| t.date_naive().succ_opt())
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|d| Local.from_local_datetime(&d).earliest())
        .map_or(now + 3600., |t| t.timestamp() as f64)
}

/// What CSR can do here: the sell lines under an empty session or the dreams-off offer.
fn sells() -> [&'static str; 3] {
    [
        "Ask about past work. CSR finds it.",
        "Code changed? CSR warns Claude.",
        "CSR keeps the threads you dropped.",
    ]
}
/// The colour a timeline kind wears: its icon, its band and its left rule.
fn river_color(kind: RiverKind, t: &Tokens) -> Rgba {
    match kind {
        RiverKind::Write => t.sky,
        RiverKind::Compact => t.amber,
        RiverKind::Dream => t.dream,
        RiverKind::Agent => t.asked,
        RiverKind::Commit => t.mint,
        RiverKind::Prompt => t.csr_share,
        RiverKind::Other => t.faint,
    }
}
/// One timeline icon centred on (`cx`, `cy`). Lucide has no quote glyph in the bundled subset, so
/// the prompt mark is drawn; every other kind is a bundled Lucide glyph.
fn river_icon(canvas: &mut Canvas, f: &Fonts, kind: RiverKind, cx: f32, cy: f32, t: &Tokens) {
    const SIZE: f32 = 12.;
    let glyph = |canvas: &mut Canvas, which: Icon, col: Rgba| {
        let st = icon(SIZE);
        let value = which.glyph().to_string();
        let (l, r, top, bottom) = f.ink(&value, st);
        canvas.text(
            f,
            &value,
            cx - (l + r) / 2.,
            cy - (top + bottom) / 2.,
            st,
            col,
            A::L,
        );
    };
    let col = river_color(kind, t);
    match kind {
        RiverKind::Write => glyph(canvas, Icon::FilePen, col),
        RiverKind::Compact => glyph(canvas, Icon::Recycle, col),
        RiverKind::Dream => glyph(canvas, Icon::MoonStar, col),
        RiverKind::Agent => glyph(canvas, Icon::Bot, col),
        RiverKind::Commit => glyph(canvas, Icon::GitCommit, col),
        RiverKind::Prompt => {
            for dx in [-3.2, 2.6] {
                let (x, y) = (cx + dx, cy - 1.8);
                canvas.circle(x, y, 2.1, col);
                let mut tail = tiny_skia::PathBuilder::new();
                tail.move_to(x + 1.6, y + 0.4);
                tail.quad_to(x + 1.4, y + 3.2, x - 1.8, y + 5.2);
                if let Some(tail) = tail.finish() {
                    canvas.stroke(&tail, 1.5, col, true);
                }
            }
        }
        RiverKind::Other => canvas.circle(cx, cy, 2.5, col),
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
/// A change with everything the pane knows of it: its place, what it is now, what it replaced.
pub fn shift_full(v: &Value) -> String {
    let mut line = shift_row(v, true);
    if !string(&v["was"]).is_empty() {
        line.push_str(&format!(" · Was: {}", string(&v["was"])));
    }
    line
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
/// Ages count in minutes: every repaint re-uploads the whole picture, so a seconds count would
/// blank the pane once a second for the first minute after each refresh.
fn age_span(seconds: f64) -> String {
    let seconds = seconds.max(0.);
    if seconds < 60. {
        "<1m".to_owned()
    } else {
        masthead::span(seconds)
    }
}
pub(crate) fn status_line(c: &Composition) -> String {
    if let Some(notice) = &c.offer_notice {
        return dreams::status_words(notice, c.dream.state == Batch::Ready);
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
pub(crate) fn meta(c: &Composition) -> String {
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
        bits.push(format!("{} asked the agent", d.handed));
    }
    if d.carried {
        bits.push("carried".into());
    }
    bits.join(" · ")
}
/// A card's tier (PANE_RULES.md #3): 0 is never folded, 1 never folds and stays small, 2 folds
/// into the stack row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardSpec {
    /// The titles a `CardRegion` of this card carries.
    pub titles: &'static [&'static str],
    pub tier: u8,
    /// Its `STACKABLE` index when it folds into the stack.
    pub stack: Option<usize>,
}
/// Every card the picture draws. A card not listed here fails the pane contract suite.
pub const CARD_REGISTRY: &[CardSpec] = &[
    CardSpec {
        titles: &["hero"],
        tier: 0,
        stack: None,
    },
    CardSpec {
        titles: &["stack"],
        tier: 0,
        stack: None,
    },
    CardSpec {
        titles: &["Context"],
        tier: 0,
        stack: None,
    },
    CardSpec {
        titles: &["Agents", "Dreams"],
        tier: 0,
        stack: None,
    },
    CardSpec {
        titles: &[MONOLOGUE],
        tier: 0,
        stack: None,
    },
    CardSpec {
        titles: &[CHANGES],
        tier: 0,
        stack: None,
    },
    CardSpec {
        titles: &["figure facts"],
        tier: 0,
        stack: None,
    },
    CardSpec {
        titles: &["notice"],
        tier: 1,
        stack: None,
    },
    CardSpec {
        titles: &["agents"],
        tier: 1,
        stack: None,
    },
    CardSpec {
        titles: &["help"],
        tier: 1,
        stack: None,
    },
    CardSpec {
        titles: &[CONFUSED_TITLE],
        tier: 2,
        stack: Some(0),
    },
    CardSpec {
        titles: &[SHOULD_BE_TITLE],
        tier: 2,
        stack: Some(1),
    },
    CardSpec {
        titles: &[LOST_TITLE],
        tier: 2,
        stack: None,
    },
    CardSpec {
        titles: &["Where the work stands", "This session"],
        tier: 2,
        stack: Some(2),
    },
    CardSpec {
        titles: &["CSR performance"],
        tier: 2,
        stack: Some(3),
    },
];
/// The stack row as drawn: where it is, the stackable cards it holds (`STACKABLE` indexes), the
/// span of each title drawn (left, right, kind) for taps, and the card open under it.
#[derive(Clone, Debug, Default)]
pub struct StackRow {
    pub top: f32,
    pub height: f32,
    pub kinds: Vec<usize>,
    pub titles: Vec<&'static str>,
    pub spans: Vec<(f32, f32, usize)>,
    pub open: Option<usize>,
}
/// Draw the stack row at `y`: a slim card with two sheet edges under it, "N more" and each folded
/// card's title, the open one in ink, and the key that opens it at the right.
fn draw_stack_row(
    c: &mut Canvas,
    f: &Fonts,
    kinds: &[usize],
    open: Option<usize>,
    y: f32,
    t: &Tokens,
) -> StackRow {
    let body_h = STACK_H - 8.;
    // The sheets of the folded cards peek out under the row.
    for (inset, drop) in [(20., 8.), (10., 4.)] {
        c.rrect(
            M + inset,
            y + drop,
            CW - 2. * inset,
            body_h,
            14.,
            al(t.glass[1], 0.9),
        );
        c.rrect_line(
            M + inset,
            y + drop,
            CW - 2. * inset,
            body_h,
            14.,
            t.hair,
            t.hair_width,
        );
    }
    c.shadow(M, y, CW, body_h, 14., t.contact_shadow);
    c.glass(
        M,
        y,
        CW,
        body_h,
        14.,
        lin(
            0.,
            y,
            0.,
            y + body_h,
            &[(t.linear_at[0], t.glass[0]), (t.linear_at[1], t.glass[1])],
        ),
    );
    c.rrect_line(M, y, CW, body_h, 14., t.hair, t.hair_width);
    let medium = sans(theme::CAPTION_SIZE, t.medium);
    let regular = sans(theme::CAPTION_SIZE, t.regular);
    let base = y + body_h / 2. + f.cap(medium) / 2.;
    let key = if open.is_some() { "e close" } else { "e open" };
    let key_w = f.width(key, medium);
    c.rrect(
        IX + IW - key_w - S1,
        base - f.cap(medium) - S1 / 2. - 1.,
        key_w + S2,
        theme::CHIP_LINE,
        S1,
        t.chip,
    );
    c.text(f, key, IX + IW + S1 - S1, base, medium, t.dream, A::R);
    let right = IX + IW - key_w - S3;
    let mut x = IX;
    x += c.text(
        f,
        &format!("{} more", kinds.len()),
        x,
        base,
        medium,
        t.ink,
        A::L,
    );
    let mut row = StackRow {
        top: y,
        height: STACK_H,
        kinds: kinds.to_vec(),
        titles: kinds.iter().map(|k| STACKABLE[*k].1).collect(),
        spans: vec![],
        open,
    };
    for &kind in kinds {
        let name = STACKABLE[kind].1;
        let sep = " · ";
        let w = f.width(sep, regular) + f.width(name, medium);
        if x + w > right {
            c.text(f, " …", x, base, regular, t.quiet, A::L);
            break;
        }
        x += c.text(f, sep, x, base, regular, t.quiet, A::L);
        let is_open = open == Some(kind);
        let st = if is_open {
            sans(theme::CAPTION_SIZE, t.bold)
        } else {
            medium
        };
        let w = c.text(
            f,
            name,
            x,
            base,
            st,
            if is_open { t.dream } else { t.quiet },
            A::L,
        );
        if is_open {
            c.rect(x, base + 2.5, w, 1.5, t.dream);
        }
        row.spans.push((x, x + w, kind));
        x += w;
    }
    row
}
/// The radius of the dreamer's token ring on its meta row, in layout pt.
fn budget_ring_r() -> f32 {
    theme::pick(6.5, 5.0)
}
/// The width one part of the dreamer card's meta row takes: the token ring takes room before the
/// budget's words, which drop their "dreamer" (the card says it).
fn meta_part_width(f: &Fonts, part: &str, t: &Tokens) -> f32 {
    if let Some(rest) = part.strip_prefix("dreamer ") {
        2. * budget_ring_r() + ICON_GAP + f.width(rest, sans(theme::row_caption_size(), t.regular))
    } else if part.starts_with('⌗') {
        f.width(part, mono(theme::row_chip_size(), t.regular))
    } else {
        f.width(part, sans(theme::row_caption_size(), t.regular))
    }
    .min(IW)
}
/// The rows a list's card can take: an item a row, or the "Same page?" card's words.
fn card_rows(items: &[Value]) -> usize {
    if items.first().is_some_and(|i| i["same_page"] == true) {
        crate::companion::same_page::ROWS
    } else {
        items.len()
    }
}
/// The rows a list's card takes at `cap`. The "Same page?" card keeps two: its numbers alone are
/// not the card.
fn rows_at(items: &[Value], cap: usize) -> usize {
    let same_page = items.first().is_some_and(|i| i["same_page"] == true);
    card_rows(items).min(if same_page { cap.max(2) } else { cap })
}
/// The 8-bit Claude: 11 by 8 pixels, its eyes by how the session is going. It returns its width.
fn sprite(
    c: &mut Canvas,
    x: f32,
    center: f32,
    tall: f32,
    mood: crate::companion::same_page::Mood,
    body: Rgba,
    ink: Rgba,
) -> f32 {
    use crate::companion::same_page::Mood;
    const BODY: [&str; 8] = [
        "..#######..",
        "..#######..",
        "###########",
        "###########",
        "###########",
        "..#######..",
        "..#.#.#.#..",
        "..#.#.#.#..",
    ];
    // Eyes: (column, top row, rows).
    let eyes: [(usize, usize, usize); 2] = match mood {
        Mood::Fine => [(3, 1, 2), (7, 1, 2)],
        Mood::Unsure => [(3, 1, 2), (7, 2, 1)],
        Mood::Lost => [(3, 2, 1), (7, 2, 1)],
    };
    let pixel = (tall / 8.).round().max(1.);
    let top = center - 4. * pixel;
    for (row, cells) in BODY.iter().enumerate() {
        for (column, cell) in cells.bytes().enumerate() {
            if cell == b'#' {
                let (px, py) = (x + column as f32 * pixel, top + row as f32 * pixel);
                c.rect(px, py, pixel, pixel, body);
            }
        }
    }
    for (column, row, rows) in eyes {
        let (px, py) = (x + column as f32 * pixel, top + row as f32 * pixel);
        c.rect(px, py, pixel, rows as f32 * pixel, ink);
    }
    11. * pixel
}
/// A card of `rows` one-line observations under its title.
fn observation_card_h(rows: usize) -> f32 {
    P * 2. + theme::row_title_line() + S2 + rows as f32 * theme::row_body_line()
}

/// Line pitch of an open section's text card.
const DRILL_LINE: f32 = 17.;
/// The figure card's height for the expanded chart (95 + 40), its labels and payback (72) and the
/// gauge offset (84).
const FIGURE_EXPANDED_BASE: f32 = 291.;
/// The expanded figure's height: the history chart keeps its room when the notes line is drawn.
fn figure_expanded_h(g: &Value) -> f32 {
    FIGURE_EXPANDED_BASE + notes_h(g)
}
/// The history plot's height at the expanded figure's full height, and the least it is shrunk to
/// so the notes line stays: its title row, a few bars and the gap under them.
const EXPANDED_PLOT_FULL: f32 = 73.;
const EXPANDED_PLOT_MIN: f32 = 48.;
/// The least height the expanded figure draws in: the chart gives back what the notes line needs.
/// A shorter card drops the chart and draws the collapsed ring.
fn figure_expanded_min_h(g: &Value) -> f32 {
    figure_expanded_h(g) - (EXPANDED_PLOT_FULL - EXPANDED_PLOT_MIN)
}
/// Float noise in a card's height that never makes it the strip: a closed pane's card is laid
/// out at `mid_min_h` to within rounding.
const MID_MIN_SLOP: f32 = 0.5;
/// The least height of a Context card in the mid layout: the title, the small circles with their
/// captions and the notes line, and the bottom clearance. A card shorter than this is the strip.
fn mid_min_h(g: &Value) -> f32 {
    const MIN_GAUGE_R: f32 = 16.;
    const LABEL_BELOW: f32 = MIN_GAUGE_R + 12.;
    P + theme::TITLE_LINE + S2 + MIN_GAUGE_R + LABEL_BELOW + theme::CAPTION_LINE + notes_h(g) + S4
}
/// Fewest lines a clipped section keeps, its "+N more" line included.
const DRILL_MIN_LINES: usize = 4;
/// Fewest Monologue rows a page keeps on screen, however short the pane.
const PAGE_MIN_ROWS: usize = 3;
/// The Changes page's heading.
const CHANGES: &str = "Changes";
/// The footer of a page that holds more rows than fit: which rows are shown, and how to scroll.
fn scroll_footer(from: usize, shown: usize, total: usize) -> String {
    format!(
        "{}\u{2013}{} of {total} \u{b7} j/k scroll",
        from + 1,
        from + shown
    )
}
/// The line that ends a section clipped to the room it was given.
fn more_line(hidden: usize) -> String {
    format!("+{hidden} more")
}
pub(super) const CONFUSED_TITLE: &str = "What confused the agent";
pub(crate) const SHOULD_BE_TITLE: &str = "Should be a skill / memory / tool";
/// The card of this session's open Lost and found threads, after the two lists.
pub(crate) const LOST_TITLE: &str = "Lost and found";
/// The fold bit that hides each card of `observed`, in the order they are drawn. The Lost and found
/// card never folds into the stack: a step either draws it or drops it (`FOLD_LOST`).
const OBS_BITS: [u8; 3] = [FOLD_CONFUSED, FOLD_SHOULD, FOLD_LOST];
/// Every `kind` the dreamer may write on a should-be item, and the label its row carries. Any
/// other kind reads "tool". The pane contract draws every row of this table.
pub const SHOULD_BE_KINDS: [(&str, &str); 9] = [
    ("skill", "skill"),
    ("memory", "memory"),
    ("hook", "hook"),
    ("CLAUDE.md", "CLAUDE.md rule"),
    ("CLAUDE.md rule", "CLAUDE.md rule"),
    ("claude_md", "CLAUDE.md rule"),
    ("rule", "CLAUDE.md rule"),
    ("tool", "tool"),
    ("", "tool"),
];
/// The short label a should-be item carries on its row.
pub(super) fn should_be_kind(item: &Value) -> &'static str {
    let kind = string(&item["kind"]);
    SHOULD_BE_KINDS
        .iter()
        .find(|(raw, _)| *raw == kind)
        .map_or("tool", |(_, label)| label)
}
/// One observation row as the plain-text pane writes it.
pub fn observation_text(item: &Value, should: bool) -> String {
    // The "Same page?" card's words stand on their own: their turns are in its popup.
    if item["same_page"] == true {
        return string(&item["line"]).to_owned();
    }
    format!(
        "{}{} · {}",
        if should {
            format!("{}: ", should_be_kind(item))
        } else {
            String::new()
        },
        string(&item["line"]),
        evidence_receipt(item)
    )
}
/// The label an observation row carries for its evidence: the turn numbers the dreamer cited
/// ("Turn 12: ...", "Turns 4 and 12: ..."), or "evidence" when it only quoted.
pub(super) fn evidence_receipt(item: &Value) -> String {
    let turns = cited_turns(string(&item["evidence"]));
    if turns.is_empty() {
        "evidence".into()
    } else {
        format!(
            "{} {}",
            if turns.len() == 1 && !turns[0].contains('-') {
                "turn"
            } else {
                "turns"
            },
            turns.join(", ")
        )
    }
}
/// The turn numbers an evidence string cites, in order, each once. Only a number list that
/// follows the word "turn" or "turns" outside quotation marks counts; a number anywhere else
/// (a size, a commit id, a time, a count inside a quote) is not a turn and is never read as one.
fn cited_turns(evidence: &str) -> Vec<String> {
    let chars: Vec<char> = evidence.chars().collect();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut turns: Vec<String> = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(close) = quote {
            if c == close {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\u{201c}' {
            quote = Some(if c == '"' { '"' } else { '\u{201d}' });
            i += 1;
            continue;
        }
        let at_word_start = (c == 't' || c == 'T') && (i == 0 || !word(chars[i - 1]));
        let marker = if at_word_start {
            let letters: String = chars[i..].iter().take(5).collect::<String>().to_lowercase();
            if letters == "turns" {
                5
            } else if letters.starts_with("turn") {
                4
            } else {
                0
            }
        } else {
            0
        };
        if marker == 0 || chars.get(i + marker).is_some_and(|c| word(*c)) {
            i += 1;
            continue;
        }
        i += marker;
        loop {
            while chars.get(i).is_some_and(|c| *c == ' ' || *c == '#') {
                i += 1;
            }
            let Some((number, next)) = turn_number(&chars, i) else {
                break;
            };
            if !turns.contains(&number) {
                turns.push(number);
            }
            i = next;
            let mut j = i;
            while chars.get(j) == Some(&' ') {
                j += 1;
            }
            let rest: String = chars[j..].iter().take(4).collect::<String>().to_lowercase();
            j += if chars.get(j).is_some_and(|c| matches!(c, ',' | '&' | '/')) {
                1
            } else if rest.starts_with("and ") {
                3
            } else {
                break;
            };
            i = j;
        }
    }
    turns
}
/// One turn number ("12") or turn range ("12-14", "12..14") starting at `at`, and where it ends.
fn turn_number(chars: &[char], at: usize) -> Option<(String, usize)> {
    let digits = |from: usize| {
        let mut end = from;
        while chars.get(end).is_some_and(|c| c.is_ascii_digit()) {
            end += 1;
        }
        (end > from && end - from <= 6 && chars[from] != '0').then_some(end)
    };
    let first_end = digits(at)?;
    let mut end = first_end;
    let mut text: String = chars[at..first_end].iter().collect();
    let dots = chars.get(first_end) == Some(&'.') && chars.get(first_end + 1) == Some(&'.');
    let sep = if dots { 2 } else { 1 };
    if dots || matches!(chars.get(first_end), Some('-' | '\u{2013}')) {
        if let Some(second_end) = digits(first_end + sep) {
            let second: String = chars[first_end + sep..second_end].iter().collect();
            if second.parse::<u32>().ok()? > text.parse::<u32>().ok()? {
                text = format!("{text}-{second}");
                end = second_end;
            }
        }
    }
    // "turn 4GB", "turn 4.5", "turn 9:14" are not turn numbers.
    let after = chars.get(end).copied();
    let decimal =
        matches!(after, Some('.' | ':')) && chars.get(end + 1).is_some_and(|c| c.is_ascii_digit());
    (!after.is_some_and(|c| c.is_alphanumeric() || c == '_') && !decimal).then_some((text, end))
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
/// The compaction timer the picture shows, in ten-second steps: every repaint re-uploads the
/// whole picture, so a seconds counter would blank the pane once a second.
fn drawn_compaction(compacting: &Value) -> Value {
    let mut info = compacting.clone();
    if info.is_object() {
        info["elapsed"] = json!((num(&compacting["elapsed"]) / 10.).floor() * 10.);
    }
    info
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
/// The last first row the open popup can show as text, with a row left for the keys.
pub fn text_popup_max(c: &Composition, cols: usize, rows: usize, open: Open) -> usize {
    open.popup.and_then(|at| c.detail(at)).map_or(0, |d| {
        observation::rows(&d, cols)
            .len()
            .saturating_sub(rows.saturating_sub(1))
    })
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
    if let Some((at, detail)) = open.popup.and_then(|at| c.detail(at).map(|d| (at, d))) {
        let mut out = observation::rows(&detail, cols);
        let max = out.len().saturating_sub(rows.saturating_sub(1));
        out.drain(..at.scroll.min(max));
        return out;
    }
    let (out, short) = assemble(c, cols, rows, open, show_figure, false);
    if short {
        // An open section never gets squeezed out: fold the two observation cards to a row each.
        assemble(c, cols, rows, open, show_figure, true).0
    } else {
        out
    }
}
/// The heading of the story card.
pub fn story_title(c: &Composition) -> &'static str {
    if c.story.is_empty() && c.shifts.is_empty() {
        "This session"
    } else {
        "Where the work stands"
    }
}
/// Every word the story card holds: the story, or with none, what was learnt and what changed.
pub fn story_full(c: &Composition) -> String {
    if c.story.is_empty() {
        c.learnt
            .iter()
            .cloned()
            .chain(c.shifts.iter().map(shift_full))
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        c.story.clone()
    }
}
/// What the Changes page says it holds: how many changes there are, each with what it replaced.
fn changes_note(c: &Composition) -> String {
    match c.shifts.len() {
        0 => "No changes yet in this session.".into(),
        n => format!(
            "{n} change{}, each with what it replaced.",
            if n == 1 { "" } else { "s" }
        ),
    }
}
const NO_THREADS: &str = "No threads yet: the session has said nothing out loud.";
/// The figure's text rows: the closed pane's facts, or with `i` open one fact to a row.
pub fn figure_text_rows(
    c: &Composition,
    cols: usize,
    open_figure: bool,
    gauge: Option<Gauge>,
) -> Vec<String> {
    if open_figure {
        figure_detail_lines(c, gauge)
            .iter()
            .flat_map(|line| {
                // A fact whose pieces do not fit a row wraps; fact_rows would drop them.
                if line.split(" · ").any(|piece| text::len(piece) > cols) {
                    text::wrap(line.trim(), cols.max(1), 1000)
                } else {
                    fact_rows(line, cols)
                }
            })
            .collect()
    } else {
        figure_lines(c)
            .iter()
            .flat_map(|line| fact_rows(line, cols))
            .collect()
    }
}
/// The text pane's page: the Changes or the Monologue and nothing of the dashboard, scrolled by
/// `open.scroll`, with a footer when it holds more rows than fit.
fn page_text_rows(c: &Composition, cols: usize, rows: usize, open: Open) -> Vec<String> {
    page_text_rows_tagged(c, cols, rows, open).0
}
/// For each row of the open Monologue's text page, which river row (an index of `c.river`) it
/// belongs to, or none for a heading, the footer or any other page. The same rows the text pane
/// draws, wrapping and scrolling included.
pub fn river_row_tags(c: &Composition, cols: usize, rows: usize, open: Open) -> Vec<Option<usize>> {
    if !open.threads {
        return vec![];
    }
    page_text_rows_tagged(c, cols, rows, open).1
}
/// [`page_text_rows`] and, row by row, the river row each belongs to.
fn page_text_rows_tagged(
    c: &Composition,
    cols: usize,
    rows: usize,
    open: Open,
) -> (Vec<String>, Vec<Option<usize>>) {
    let wrap = |s: &str| text::wrap(s, cols.max(1), 1000);
    let (mut out, body): (Vec<String>, Vec<(Option<usize>, String)>) = if open.threads {
        let mut body: Vec<(Option<usize>, String)> = vec![];
        if c.river.is_empty() {
            body.extend(wrap(NO_THREADS).into_iter().map(|l| (None, l)));
        }
        for (at, row) in c.river.iter().enumerate() {
            let when = row.time_label(c.now);
            body.extend(
                wrap(&format!(
                    "{}{}{}{} · {}",
                    when,
                    if when.is_empty() { "" } else { " · " },
                    row.lead,
                    row.mood(),
                    row.line
                ))
                .into_iter()
                .map(|l| (Some(at), l)),
            );
        }
        (wrap(&format!("{MONOLOGUE} · {MONOLOGUE_SUB}")), body)
    } else {
        let mut body: Vec<(Option<usize>, String)> = vec![];
        for v in &c.shifts {
            body.extend(wrap(&shift_row(v, true)).into_iter().map(|l| (None, l)));
            if !string(&v["was"]).is_empty() {
                body.extend(
                    wrap(&format!("Was: {}", string(&v["was"])))
                        .into_iter()
                        .map(|l| (None, l)),
                );
            }
        }
        let mut head = wrap(CHANGES);
        head.extend(wrap(&changes_note(c)));
        (head, body)
    };
    let mut tags: Vec<Option<usize>> = vec![None; out.len()];
    // The key row keeps the last line of the pane.
    let room = rows.saturating_sub(1 + out.len()).max(1);
    if body.len() <= room {
        for (at, line) in body {
            tags.push(at);
            out.push(line);
        }
    } else {
        let shown = (room - 1).max(1);
        let from = open.scroll.min(body.len() - shown);
        for (at, line) in body.iter().skip(from).take(shown) {
            tags.push(*at);
            out.push(line.clone());
        }
        for line in wrap(&scroll_footer(from, shown, body.len())) {
            tags.push(None);
            out.push(line);
        }
    }
    (out, tags)
}
/// The text rows, and whether an open section was cut for want of room.
fn assemble(
    c: &Composition,
    cols: usize,
    rows: usize,
    open: Open,
    show_figure: bool,
    fold: bool,
) -> (Vec<String>, bool) {
    let mut short = false;
    let mut out = vec![];
    let mut history_insert = None;
    let mut pitch_insert = None;
    let cap = rows * 3 / 10;
    let wrap = |s: &str| text::wrap(s, cols.max(1), 1000);
    if open.threads || open.changes {
        return (page_text_rows(c, cols, rows, open), false);
    }
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
            "1 helpful · 2 thought of it · 3 not relevant · 4 ask the agent",
            cols,
        ));
    }
    // Tiny terminals show the complete question in its drill-down. Never cut a rendered line.
    if hero.len() > cap {
        collapsed_hero = true;
        hero = wrap("☾ what-if · d");
        if hero.len() > cap {
            hero = wrap("☾ d");
            hero.truncate(cap);
        }
    }
    out.extend(hero);
    for (title, items, should) in [
        (CONFUSED_TITLE, &c.confusing, false),
        (SHOULD_BE_TITLE, &c.should_be, true),
        (LOST_TITLE, &c.open_lost, false),
    ] {
        if items.is_empty() {
            continue;
        }
        // A list squeezed for room is left out, never folded to a title row.
        if fold {
            continue;
        }
        out.extend(wrap(if items[0]["same_page"] == true {
            crate::companion::same_page::TITLE
        } else {
            title
        }));
        // The Lost and found card is one row: the newest open thread.
        for item in items
            .iter()
            .take(if title == LOST_TITLE { 1 } else { usize::MAX })
        {
            out.extend(wrap(&if title == LOST_TITLE {
                string(&item["text"]).to_owned()
            } else {
                observation_text(item, should)
            }));
        }
    }
    let obs_end = out.len();
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
        out.extend(wrap(&format!("Last time here · {}", recap.age)));
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
    out.extend(text_story_rows(c, cols));
    let story_end = out.len();
    for v in c.shifts.iter().take(3) {
        out.extend(wrap(&shift_row(v, false)));
    }
    let details_start = out.len();
    if open.dreams {
        if collapsed_hero {
            out.extend(wrap(&c.question()));
        }
        out.extend(wrap(string(&c.dream.top["text"])));
        for d in &c.dream.queue {
            out.extend(wrap(string(&d["line"])));
        }
    }
    if open.agents {
        for line in agent_lines(c) {
            out.extend(wrap(&line));
        }
    }
    // `i` opens the figure: one fact to a row, then the history in words.
    let fig_lines = figure_text_rows(c, cols, open.figure, open.gauge);
    if open.dreams || open.agents {
        let details = out.drain(details_start..).collect::<Vec<_>>();
        let figure_rows = if show_figure { fig_lines.len() } else { 4 };
        let reserved = figure_rows
            + 1
            + if show_figure { 0 } else { 7 }
            + usize::from(c.agents.agents.iter().any(|a| agents::is_active(a, c.now)));
        let room = rows.saturating_sub(out.len() + reserved);
        let wanted = details.len().saturating_sub(open.scroll);
        // The note or heading always shows, whatever else is squeezed.
        let take = if fold { room.max(wanted.min(3)) } else { room };
        short = take < wanted.min(8);
        out.extend(details.into_iter().skip(open.scroll).take(take));
    }
    // The CSR performance card, as two lines (and any failing check) above the figure's rows.
    if let Some(card) = &c.soak {
        out.extend(wrap(&card.line1));
        if !card.line2.is_empty() {
            out.extend(wrap(&card.line2));
        }
        for row in card.rows.iter().filter(|r| r.mark == "✗") {
            out.extend(wrap(&format!("{} {}", row.mark, row.text)));
        }
        if let Some(road) = &card.road {
            out.extend(wrap(&format!("{} {}", road.mark, road.text)));
        }
        if let Some(warning) = &card.warning {
            out.extend(wrap(warning));
        }
    }
    if let Some(credit) = &c.credit {
        out.extend(fact_rows(&credit.row(), cols));
    }
    if show_figure {
        out.extend(fig_lines);
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
        .filter(|a| agents::is_active(a, c.now))
        .collect();
    // Past the pane's last row the tail is cut. An open section must not be what goes: fold the
    // observation cards (a second pass), then give up the rows between them and the open section.
    let over = (out.len() + usize::from(!active.is_empty())).saturating_sub(rows.saturating_sub(1));
    if over > 0 && (open.dreams || open.agents || open.figure) {
        if fold {
            let cut = over.min(story_end.saturating_sub(obs_end));
            out.drain(obs_end..obs_end + cut);
            history_insert = None;
            pitch_insert = None;
        } else {
            short = true;
        }
    }
    if let Some(line) = crate::companion::contract::agents_line(c) {
        out.extend(wrap(&line));
    }
    if let (Some(at), Some(recap)) = (history_insert, &c.repo_recap) {
        let room = rows.saturating_sub(1 + out.len());
        if room > 1 && !recap.history.is_empty() {
            let mut history = vec!["Earlier here".into()];
            history.extend(recap.history.iter().take(room - 1).map(|prior| {
                text::fit(
                    &format!(
                        "{}{} · {}",
                        history_mark(&prior.outcome)
                            .map(|m| format!("{m} "))
                            .unwrap_or_default(),
                        prior.age,
                        earlier_intent(&prior.intent)
                    ),
                    cols,
                )
            }));
            // Starter questions follow the list when the pane has room left over.
            if c.empty_session {
                pitch_insert = Some(at + history.len());
            }
            out.splice(at..at, history);
        } else if c.empty_session {
            pitch_insert = Some(at);
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
    (out, short)
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
            "What did I ask the agent to remember?",
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
/// "Try a question" with as many starter questions as fit between `cursor` and `bottom`.
fn starter_cards(canvas: &mut Canvas, f: &Fonts, mut cursor: f32, bottom: f32, t: &Tokens) {
    let title = heading(t);
    let body = sans(theme::BODY_SIZE, t.regular);
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    if bottom - cursor < theme::TITLE_LINE + S3 + theme::BODY_LINE + theme::CAPTION_LINE {
        return;
    }
    label(
        canvas,
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
            canvas,
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
            canvas,
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
/// A mark only where the outcome is known; an unknown outcome says nothing.
fn history_mark(outcome: &str) -> Option<&'static str> {
    match outcome {
        "done" => Some("✓"),
        "open" => Some("→"),
        _ => None,
    }
}
/// Under an "Earlier here" heading every row was asked, so the lead-in is noise.
fn earlier_intent(intent: &str) -> &str {
    intent.strip_prefix("you asked: ").unwrap_or(intent)
}
/// A figure that asks for the older ring layouts when `rings` is true.
fn with_layout(mut figure: Value, rings: &Value, native_window: &Value) -> Value {
    if *rings == true {
        figure["layout"] = json!("rings");
    }
    // The window Claude Code's own auto-compact works to, when settings or its env var set one.
    if figure["ctx"].is_object() && native_window.as_f64().is_some_and(|w| w > 0.) {
        figure["ctx"]["window"] = native_window.clone();
    }
    figure
}
/// A figure carrying the outlook of its windows (`forecast::outlook`'s shape): the dashboard's
/// headline and window lines, and for the circles of the older layouts a short caption each.
fn with_forecast(mut figure: Value, forecast: &Value) -> Value {
    if let Some(quotas) = figure["quotas"].as_array_mut() {
        for q in quotas {
            let window = forecast["quota"][string(&q["code"])].clone();
            if q["used"].is_number() && window["text"].is_string() {
                q["forecast"] = window;
            }
            // An early window has no caption under its circle, but its words still go in the
            // facts: what the last window peaked at, or that it is too soon to tell.
            let early = array(&forecast["windows"])
                .iter()
                .find(|w| w["code"] == q["code"] && w["early"] == true)
                .and_then(|w| w["line"].as_str())
                .map(|l| l.split(" · resets in ").next().unwrap_or(l).to_owned());
            if let Some(line) = early.filter(|_| q["used"].is_number()) {
                q["early"] = json!(line);
            }
        }
    }
    if forecast["windows"].is_array() {
        figure["outlook"] = json!({
            "headline": forecast["headline"],
            "windows": forecast["windows"],
        });
    }
    figure
}
/// One window's line in the facts: its words, then where it is headed when that is known.
fn quota_fact(q: &Value) -> String {
    let line = text::paced(q);
    match q["forecast"]["text"].as_str().or(q["early"].as_str()) {
        Some(forecast) => format!("{line} · {forecast}"),
        None => line,
    }
}
/// The name an agent shows under, when it has one.
fn agent_name(a: &agents::Agent) -> Option<&str> {
    Some(a.description.trim()).filter(|n| !n.is_empty())
}
/// The agents bar's words, and whether they warn. Nothing stalled: how many agents run and the
/// name of the newest, in plain ink. One or more stalled: the count and the name of a stalled
/// agent (the one stuck longest), in the warning. An agent with no description has no name to
/// show, and then no dot is drawn either.
fn agents_bar(agents: &[agents::Agent], now: f64) -> (String, bool) {
    let active = || agents.iter().filter(|a| agents::is_active(a, now));
    let stalled: Vec<_> = active().filter(|a| agents::stalled(a, now)).collect();
    let (count, word, shown) = if stalled.is_empty() {
        let newest = active()
            .filter(|a| agent_name(a).is_some())
            .max_by(|a, b| a.first_seen.total_cmp(&b.first_seen));
        (
            active().count(),
            "running",
            newest.and_then(|a| agent_name(a)),
        )
    } else {
        let mut by_age = stalled.clone();
        by_age.sort_by(|a, b| a.last_at.total_cmp(&b.last_at));
        let longest = by_age.iter().find_map(|a| agent_name(a));
        (stalled.len(), "stalled", longest)
    };
    let text = match shown {
        Some(name) => format!("{count} {word} · {name}"),
        None => format!("{count} {word}"),
    };
    (text, !stalled.is_empty())
}
pub(crate) fn agent_lines(c: &Composition) -> Vec<String> {
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
pub(crate) fn first_sentence(s: &str) -> &str {
    s.find(". ").map_or(s, |i| &s[..=i])
}
/// The figure's facts, one per entry: the context row, the quota row and the money line.
fn figure_facts(c: &Composition) -> (Vec<String>, Vec<String>) {
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
            let line = quota_fact(q);
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
    (top, quotas)
}

/// The facts of one circle of the quota row, a line each. Empty when that circle is not drawn.
fn gauge_facts(c: &Composition, gauge: Gauge) -> Vec<String> {
    let g = &c.figure;
    let quota = |code: &str| {
        array(&g["quotas"])
            .iter()
            .find(|q| q["code"] == code && q["used"].is_number())
    };
    match gauge {
        Gauge::FiveHour => quota("5H").map(|q| vec![quota_fact(q)]).unwrap_or_default(),
        Gauge::Week => quota("7D")
            .map(|q| {
                let mut out = vec![format!(
                    "{} · CSR {} pts",
                    quota_fact(q),
                    c.csr_cost.share_label()
                )];
                let money = money_fact(c);
                if !money.is_empty() {
                    out.push(money);
                }
                out
            })
            .unwrap_or_default(),
        Gauge::Cache => {
            if g["cache"]["left"].is_number() || g["cache"]["cold"] == true {
                vec![string(&g["cache"]["text"]).replace("CACHE ", "◷ ")]
            } else {
                vec![]
            }
        }
        Gauge::Codex => c.codex_week["percent"]
            .as_f64()
            .map(|n| vec![format!("Codex 7D {n:.0}%")])
            .unwrap_or_default(),
    }
}

fn money_fact(c: &Composition) -> String {
    if has_week(c) && c.csr_cost.usd.is_some() {
        c.csr_cost.money_line()
    } else {
        String::new()
    }
}

fn figure_lines(c: &Composition) -> Vec<String> {
    let (top, quotas) = figure_facts(c);
    [top.join(" · "), quotas.join(" · "), money_fact(c)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect()
}

/// What `i` opens: every fact of the figure on a line of its own, then the per-turn history in
/// words. Says so when there is nothing to show.
fn figure_detail_lines(c: &Composition, gauge: Option<Gauge>) -> Vec<String> {
    // A tapped circle shows its own facts; a circle with nothing to say falls back to the list.
    if let Some(own) = gauge.map(|g| gauge_facts(c, g)).filter(|f| !f.is_empty()) {
        return own;
    }
    let (top, quotas) = figure_facts(c);
    let g = &c.figure;
    // The cost line alone is not a measurement: with only that, the pane still says nothing was measured.
    let measured = !top.is_empty()
        || array(&g["quotas"]).iter().any(|q| q["used"].is_number())
        || g["cache"]["left"].is_number()
        || c.requests.iter().any(|(_, fresh)| *fresh > 0.);
    let mut out: Vec<String> = vec![];
    if !measured {
        out.push("No figures yet: nothing has been measured in this session.".into());
    }
    out.extend(top);
    // The words about the last turn, where the text pane has a row to spare for them.
    out.extend(
        breakdown_of(g)
            .and_then(|b| b["caption"].as_str())
            .map(str::to_owned),
    );
    out.extend(quotas);
    let money = money_fact(c);
    if !money.is_empty() {
        out.push(money);
    }
    let fresh: Vec<f64> = c.requests.iter().map(|(_, fresh)| *fresh).collect();
    if let Some(max) = fresh.iter().copied().reduce(f64::max).filter(|m| *m > 0.) {
        out.push(format!(
            "Fresh tokens per turn: last {} turns, newest {}, max {}",
            fresh.len(),
            short(*fresh.last().unwrap_or(&0.)),
            short(max)
        ));
    }
    out
}

/// The breakdown of what fills the context, when the figure has one.
fn breakdown_of(g: &Value) -> Option<&Value> {
    g["breakdown"].is_object().then(|| &g["breakdown"])
}
/// The Context card's line about the change per turn: the last turn's parts when they are known,
/// else the median change.
fn turn_line(g: &Value) -> Option<String> {
    if let Some(caption) = breakdown_of(g).and_then(|b| b["caption"].as_str()) {
        return Some(caption.to_owned());
    }
    g["trend"].as_f64().map(|trend| {
        format!(
            "{}{}/turn",
            if trend >= 0. { "+" } else { "−" },
            short(trend.abs())
        )
    })
}
/// Height of the breakdown bar in the mid layout, and the room it takes under the lines.
const BREAKDOWN_BAR_H: f32 = 8.;
const BREAKDOWN_ROOM: f32 = 12.;
/// What fills the context as one bar over the whole window: the floor under the conversation
/// (system prompt, tools, memory), the messages, the tool results, CSR's injections, and what is
/// free. A part that exists is never thinner than a hairline, so CSR's share stays visible.
fn breakdown_bar(c: &mut Canvas, g: &Value, x: f32, y: f32, w: f32, t: &Tokens) {
    let Some(b) = breakdown_of(g) else {
        return;
    };
    let size = num(&b["size"]);
    if size <= 0. || w <= 0. {
        return;
    }
    c.rrect(x, y, w, BREAKDOWN_BAR_H, 2., t.track);
    let wide = |key: &str| {
        let n = num(&b[key]);
        if n > 0. {
            ((n / size) as f32 * w).max(1.5)
        } else {
            0.
        }
    };
    let (text, tool, csr) = (wide("text"), wide("tool"), wide("csr"));
    let used = ((num(&b["total"]) / size) as f32 * w).max(text + tool + csr);
    let floor = (used - text - tool - csr).max(0.);
    c.rrect(x, y, floor + 2., BREAKDOWN_BAR_H, 2., t.faint);
    let mut at = x + floor;
    for (part, ink) in [(text, t.sky), (tool, t.mint), (csr, t.csr_share)] {
        if part > 0. {
            c.rect(at, y, part, BREAKDOWN_BAR_H, ink);
            at += part;
        }
    }
    c.breakdown_bars.push((x, y, w, BREAKDOWN_BAR_H));
}

/// Whether a line of notes sits under the quota captions: a window with a forecast has one.
fn has_notes(g: &Value) -> bool {
    array(&g["quotas"])
        .iter()
        .any(|q| q["used"].is_number() && q["forecast"]["text"].is_string())
}
fn notes_h(g: &Value) -> f32 {
    if has_notes(g) {
        NOTES_H
    } else {
        0.
    }
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
        c.arc(x, y, r, 0., rest_end, t.ring, t.claude, false);
        c.arc(x, y, r, from, to, t.ring, t.csr_share, false);
    } else {
        c.ring(x, y, r, t.ring, t.track, &[(0., used, t.claude)]);
    }
    c.quota_gauges.push(QuotaGauge {
        code: string(&q["code"]).into(),
        center: (x, y),
        radius: r,
        csr_segment: csr,
        tap: None,
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

/// Weekly identity and CSR attribution keep separate inks on the same measured line, led by the
/// swatch of the ring they belong to.
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
    let room = width - SWATCH_LEAD;
    let st = sans(
        (size * room / f.width(&line, st).max(room)).min(size),
        t.medium,
    );
    let prefix = "7D · ";
    let w = f.width(&line, st);
    let x = caption_swatch(c, x, w, y, f.cap(st), Swatch::Disc(t.claude));
    let left = x - w / 2.;
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

/// Caption fitting never affects the equal-width gauge slots. A swatch leads the first line, and
/// the two are centred together.
fn quota_label(
    c: &mut Canvas,
    f: &Fonts,
    line: &str,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    ink: Rgba,
    swatch: Option<Swatch>,
    t: &Tokens,
) {
    let st = sans(size, t.medium);
    let room = width - if swatch.is_some() { SWATCH_LEAD } else { 0. };
    for (i, line) in f.wrap(line, st, room).iter().take(2).enumerate() {
        let base = y + i as f32 * theme::CAPTION_LINE;
        let x = match swatch.filter(|_| i == 0) {
            Some(swatch) => caption_swatch(c, x, f.width(line, st), base, f.cap(st), swatch),
            None => x,
        };
        c.text(f, line, x, base, st, ink, A::M);
    }
}

/// `fitted_caption`, led by a swatch.
fn swatch_caption(
    c: &mut Canvas,
    f: &Fonts,
    line: &str,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    swatch: Swatch,
    t: &Tokens,
) {
    let st = sans(size, t.medium);
    let room = width - SWATCH_LEAD;
    let size = (size * room / f.width(line, st).max(room)).min(size);
    let st = sans(size, t.medium);
    let x = caption_swatch(c, x, f.width(line, st), y, f.cap(st), swatch);
    c.text(f, line, x, y, st, t.quiet, A::M);
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
    notes: bool,
    t: &Tokens,
) -> (f32, f32) {
    let g = &data.figure;
    // The tap reaches down over the line of notes when one is drawn.
    let tap_to = label_y + if notes { theme::CAPTION_LINE } else { 0. };
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
        if let Some(gauge) = c.quota_gauges.last_mut() {
            gauge.tap = Some(tap_box(x, unit, cy, r, tap_to));
        }
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
                Some(Swatch::Disc(t.claude)),
                t,
            );
        }
        if notes {
            if let Some(text) = q["forecast"]["text"].as_str() {
                let at = label_y + 2. * theme::CAPTION_LINE;
                let ink = if q["forecast"]["out"] == true {
                    t.amber
                } else {
                    t.quiet
                };
                fitted_caption_ink(c, f, text, x, at, slot - S2, caption_size, ink, t);
                if c.note_rows.last() != Some(&at) {
                    c.note_rows.push(at);
                }
            }
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
            tap: Some(tap_box(x, unit, cy, r, tap_to)),
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
            Some(Swatch::Disc(if cold { t.amber } else { t.mint })),
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
            tap: Some(tap_box(x, unit, cy, r, tap_to)),
        });
        // Black on a light pane, white on a dark one: Codex is the one provider drawn in ink.
        c.ring(
            x,
            cy,
            r,
            t.ring,
            t.track,
            &[(0., (used / 100.).clamp(0., 1.) as f32, t.ink)],
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
        swatch_caption(
            c,
            f,
            "\u{e181} Codex 7D",
            x,
            label_y,
            unit - S2,
            caption_size,
            Swatch::Half(t.ink, t.track),
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
            tap: None,
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
                None,
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

/// Height a layout gives the line of notes under the quota captions.
pub(super) const NOTES_H: f32 = theme::CAPTION_LINE + S2;
/// A circle's tap: its whole slot, from the top of the ring to below its second caption line.
fn tap_box(x: f32, slot: f32, cy: f32, r: f32, label_y: f32) -> (f32, f32, f32, f32) {
    (
        x - slot / 2.,
        cy - r,
        x + slot / 2.,
        label_y + theme::CAPTION_LINE + S1,
    )
}

/// One swatch beside a caption: a filled shape in the colour its circle is drawn in.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Swatch {
    Disc(Rgba),
    /// Codex: half ink, half track, as the ring is black or white on its track.
    Half(Rgba, Rgba),
}
/// A swatch's width, and the gap between it and its caption.
const SWATCH: f32 = 8.;
const SWATCH_GAP: f32 = S1 + 1.;
/// What a swatch adds to the width of the caption it leads.
const SWATCH_LEAD: f32 = SWATCH + SWATCH_GAP;
/// Draw `swatch` at the left of a caption of width `w` that is centred on `x`, level with the
/// middle of its capital letters. Returns the x the caption is now centred on: the swatch and the
/// words are centred as one.
fn caption_swatch(c: &mut Canvas, x: f32, w: f32, baseline: f32, cap: f32, swatch: Swatch) -> f32 {
    let left = x - (w + SWATCH_LEAD) / 2.;
    let mid = baseline - cap / 2.;
    match swatch {
        Swatch::Disc(col) => c.circle(left + SWATCH / 2., mid, SWATCH / 2., col),
        Swatch::Half(a, b) => {
            c.circle(left + SWATCH / 2., mid, SWATCH / 2., a);
            // The right half-disc: a stroke as wide as the disc's radius, around its middle.
            c.arc(
                left + SWATCH / 2.,
                mid,
                SWATCH / 4.,
                0.,
                0.5,
                SWATCH / 2.,
                b,
                false,
            );
        }
    }
    x + SWATCH_LEAD / 2.
}

// The geometry and primitives below are the approved B prototype's glass, serif and ring design.
/// The most lines a notice takes in the picture.
const NOTICE_LINES: usize = 3;
pub(super) const M: f32 = S4;
pub(super) const P: f32 = S4;
pub(super) const CW: f32 = W - 2. * M;
const IX: f32 = M + P;
const IW: f32 = CW - 2. * P;
fn label(c: &mut Canvas, f: &Fonts, s: &str, x: f32, y: f32, size: f32, col: Rgba, w: f32) {
    let title_icon = match s {
        "Context" => Some(Icon::Gauge),
        "Quotas" => Some(Icon::Hourglass),
        "Where the work stands" | "This session" => Some(Icon::ScrollText),
        CONFUSED_TITLE => Some(Icon::CircleHelp),
        SHOULD_BE_TITLE => Some(Icon::Lightbulb),
        _ => None,
    };
    // A heading is the pane's serif; the rows' larger heading size carries a larger icon.
    let is_heading = size == theme::TITLE_SIZE || size == theme::row_title_size();
    let k = if is_heading {
        size / theme::TITLE_SIZE
    } else {
        1.
    };
    let x = if let Some(glyph) = title_icon {
        c.icon(f, glyph, x, y, 14. * k, size, col);
        x + 14. * k + ICON_GAP
    } else {
        x
    };
    let st = if is_heading {
        serif(size, w)
    } else {
        sans(size, w)
    };
    c.text(f, &icons::line(s), x, y, st, col, A::L);
}
/// How far a "Context" heading reaches from the card's inner left edge: its icon, then its word.
fn context_title_w(f: &Fonts, t: &Tokens) -> f32 {
    14. + ICON_GAP + f.width("Context", heading(t))
}
/// The style of every card heading: the serif the dreamer's question is set in, at one size.
fn heading(t: &Tokens) -> St {
    serif(theme::TITLE_SIZE, t.bold)
}
/// The Context card's headings, at the rows' larger heading size.
fn dash_heading(t: &Tokens) -> St {
    serif(theme::row_title_size(), t.bold)
}
fn panel(c: &mut Canvas, y: f32, h: f32, t: &Tokens) {
    c.panels.push((y, h));
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
    single_as(c, f, s, s, x, y, width, size, col, t);
}
/// `single`, where a click on the row shows `full` when `s` is drawn with a trailing "…".
fn single_as(
    c: &mut Canvas,
    f: &Fonts,
    s: &str,
    full: &str,
    x: f32,
    y: f32,
    width: f32,
    size: f32,
    col: Rgba,
    t: &Tokens,
) {
    let st = sans(size, t.regular);
    let line = icons::line(s);
    let shown = f.fit(&line, st, width);
    if shown != line && !c.dry && !c.cut_title.is_empty() {
        c.cuts.push(Cut {
            y,
            size,
            title: c.cut_title,
            full: full.to_owned(),
            lost: None,
            handed: false,
        });
    }
    c.text(f, &shown, x, y, st, col, A::L);
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
/// The story's rows in the text pane: its age, then its first sentence. None when there is no story.
pub fn text_story_rows(c: &Composition, cols: usize) -> Vec<String> {
    if c.story.is_empty() {
        return vec![];
    }
    let mut out = fact_rows(&format!("Story · {}", c.story_age), cols);
    out.extend(story_text_lines(first_sentence(&c.story), cols, 4));
    out
}
/// The story's rows in the text pane when they end in "…", with the whole story: what a click on
/// them opens. `None` when the story is shown whole, or there is none.
pub fn text_story_cut(c: &Composition, cols: usize) -> Option<(Vec<String>, String)> {
    if c.story.is_empty() {
        return None;
    }
    let lines = story_text_lines(first_sentence(&c.story), cols, 4);
    lines
        .last()
        .is_some_and(|l| l.ends_with('…'))
        .then(|| (lines, c.story.clone()))
}
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

/// A card the picture drew: its title, top and height in layout pt, and how many rows of body it
/// holds under the title. A card with no body is never drawn.
#[derive(Clone, Debug)]
pub struct CardRegion {
    pub title: &'static str,
    pub top: f32,
    pub height: f32,
    pub body_rows: usize,
}
impl CardRegion {
    fn new(title: &'static str, top: f32, height: f32, body_rows: usize) -> Self {
        Self {
            title,
            top,
            height,
            body_rows,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Metrics {
    /// Every card drawn, top to bottom.
    pub cards: Vec<CardRegion>,
    /// Each card whose content was drawn past its own bottom: (title, where the content ends,
    /// where the card ends), in layout pt.
    pub overdrawn: Vec<(&'static str, f32, f32)>,
    /// The pane's height in layout pt: the canvas is never taller.
    pub limit: f32,
    /// What the fit needed past its last step, when that was more than `limit`: the picture is
    /// cut at the pane's bottom.
    pub clipped: Option<f32>,
    /// The stack row, when cards are folded into one.
    pub stack: Option<StackRow>,
    /// Every run of text drawn: left, top, right, bottom in layout pt, and the words.
    pub text_runs: Vec<(f32, f32, f32, f32, String)>,
    /// Top and height of every card panel drawn (`panel`): the contract matches each to a
    /// registered card region, so a card cannot draw without registering.
    pub panels: Vec<(f32, f32)>,
    pub csr_cost: InstrumentCost,
    pub quota_gauges: Vec<QuotaGauge>,
    /// Baseline of the line of notes drawn under the quota circles, in layout pt.
    pub note_rows: Vec<f32>,
    /// Left, top, width and height of each context breakdown bar drawn, in layout pt.
    pub breakdown_bars: Vec<(f32, f32, f32, f32)>,
    /// The Context card's dashboard: its lines, bars and legend, when it is the layout.
    pub dash: Option<Dash>,
    /// Top and height of the CSR performance card, in layout pt, when one is drawn.
    pub soak_region: Option<(f32, f32)>,
    /// Top and bottom of each "N by CSR" caption drawn, in layout pt.
    pub by_csr: Vec<(f32, f32)>,
    pub fresh_plots: Vec<FreshPlot>,
    pub question_size: f32,
    pub question_line_height: f32,
    /// The line one list row takes, in layout pt (the rows' size depends on the pane).
    pub list_row_line: f32,
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
    /// Each observation row's card, place on it and centre Y in layout pt: the click targets.
    pub observation_rows: Vec<(ObsCard, usize, f32)>,
    /// Each observation card's title line: its card, top and bottom in layout pt. A click on it
    /// opens the card's first item.
    pub observation_titles: Vec<(ObsCard, f32, f32)>,
    /// The story card's title and text: top and bottom in layout pt, its heading and every word
    /// of it. A click on those rows opens it in full, folded or not.
    pub story_open: Option<(f32, f32, String, String)>,
    /// Where the popup's footer keys sit in layout pt, when a popup is drawn.
    pub popup_keys: Vec<observation::KeyBox>,
    /// The most the "What CSR told Claude" popup can be scrolled (rows), when it is drawn.
    pub popup_scroll_max: usize,
    /// With a page open: the most the first row shown may be, so a scroll never runs past the
    /// last row.
    pub page_scroll_max: Option<usize>,
    /// Every row drawn with a trailing "…", with its words in full: the click targets that open
    /// the popup.
    pub cuts: Vec<Cut>,
    pub agents_region: Option<(f32, f32)>,
    /// Top and height of the card an open `l`, `t`, `d` or `a` draws, in layout pt.
    pub drill_region: Option<(f32, f32)>,
    /// Top and height of the facts card an open `i` draws under the figure.
    pub figure_detail_region: Option<(f32, f32)>,
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
}
fn question_layout(f: &Fonts, q: &str, width: u32, t: &Tokens) -> (St, Vec<String>) {
    let size = (24. + (width as f32 - 704.) / 256. * 6.).clamp(22., 30.);
    let st = serif(size, t.bold);
    (st, f.wrap_balanced(q, st, IW))
}

/// One set of fonts per drawing thread. A shared set would hold every other thread for a whole
/// frame, because `draw` keeps the fonts for all of it. The pane draws on one thread.
fn fonts() -> Option<std::rc::Rc<Fonts>> {
    thread_local! {
        static FONTS: std::rc::Rc<Fonts> = std::rc::Rc::new(Fonts::load());
    }
    Some(FONTS.with(std::rc::Rc::clone))
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
    // `l` and `t` open a page: one card, the Changes or the Monologue, over the whole pane, and
    // none of the dashboard's cards.
    let page = open.threads || open.changes;
    let ratio = width as f32 / (W * S);
    let raw_h = available_height as f32 / (S * ratio);
    let maxh = raw_h.max(800.);
    // A pane at the layout's height floor cannot hold the larger rows: it keeps the sizes it had.
    let _rows = theme::RowsGuard::new(raw_h > 800.5);
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    let body = sans(theme::BODY_SIZE, t.regular);
    let title = heading(t);
    let story_st = body;
    let story_lines_fit = if c.story.is_empty() {
        Vec::new()
    } else {
        wrap_clauses(f, first_sentence(&c.story), story_st, IW, 4)
    };
    let story_line_rows = story_lines_fit.len();
    let has_story = !page && (!c.story.is_empty() || !c.shifts.is_empty() || !c.learnt.is_empty());
    let story_age =
        has_story && !c.story_age.is_empty() && (!c.story.is_empty() || !c.shifts.is_empty());
    // The story card's height with at most `lines` story rows and `shifts` change rows.
    let story_shift_rows = c.shifts.len().min(3);
    let story_h_with = |lines: usize, shifts: usize| -> f32 {
        if !has_story {
            return 0.;
        }
        let shifts = shifts.min(story_shift_rows);
        P * 2.
            + theme::TITLE_LINE
            + S2
            + if story_age {
                theme::CAPTION_LINE + S3
            } else {
                0.
            }
            + (lines.min(story_line_rows) + usize::from(!c.learnt.is_empty())) as f32
                * theme::BODY_LINE
            + if shifts == 0 {
                0.
            } else {
                S3 + shifts as f32 * theme::CAPTION_LINE
            }
    };
    let htitle = serif(theme::row_title_size(), t.bold);
    let hcap = sans(theme::row_caption_size(), t.regular);
    // The dreamer card. A question takes the big serif; every other state is two rows: the title
    // with the state in words, and one meta row of the token ring, the budget and the status.
    let compact_hero = !c.offer && c.dream.state != Batch::Ready;
    let status = status_line(c);
    let mut parts: Vec<String> = meta(c)
        .split(" · ")
        .filter(|p| !p.is_empty() && *p != "carried")
        .map(str::to_owned)
        .collect();
    if compact_hero {
        let wrap = |s: &str| f.wrap(s, hcap, IW);
        parts.extend(dreams::meta_parts(&c.offer_notice, &status, wrap));
        if parts.is_empty() {
            parts.push("It asks a what-if from your past sessions.".into());
        }
    }
    let mut meta_rows: Vec<Vec<String>> = vec![];
    let mut row: Vec<String> = vec![];
    let mut row_width = 0.;
    for part in parts {
        let part_width = meta_part_width(f, &part, t);
        if !row.is_empty() && row_width + S3 + part_width > IW {
            meta_rows.push(std::mem::take(&mut row));
            row_width = 0.;
        }
        if !row.is_empty() {
            row_width += S3;
        }
        row_width += part_width;
        row.push(part);
    }
    if !row.is_empty() {
        meta_rows.push(row);
    }
    let mut status_lines = if compact_hero {
        vec![]
    } else {
        f.wrap(&status, hcap, IW)
    };
    let (qs, question) = question_layout(f, &c.question(), width, t);
    let mut question = if compact_hero { vec![] } else { question };
    let question_line_height = (qs.size * 1.25).round();
    let has_answers = c.dream.state == Batch::Ready || c.offer;
    // The hero keeps to 30% of the pane-derived height: a long question or status loses its
    // last lines ("…") instead of growing the canvas, which would shrink every font (rule 2).
    let hero_rest = |meta: usize| {
        P * 2.
            + theme::row_title_line()
            + S2
            + if meta == 0 {
                0.
            } else {
                (if compact_hero { 0. } else { S3 }) + meta as f32 * theme::row_caption_line()
            }
            + if has_answers {
                S1 + theme::row_caption_line()
            } else {
                0.
            }
    };
    let hero_budget = (maxh * 0.30).floor();
    let hero_text_h = |q: usize, s: usize| {
        q as f32 * question_line_height
            + if s == 0 {
                0.
            } else {
                S2 + s as f32 * theme::row_caption_line()
            }
    };
    let (full_q, full_s, full_m) = (question.len(), status_lines.len(), meta_rows.len());
    while hero_rest(meta_rows.len()) + hero_text_h(question.len(), status_lines.len()) > hero_budget
    {
        if status_lines.len() > 1 {
            status_lines.pop();
        } else if question.len() > 1 {
            question.pop();
        } else if meta_rows.len() > 1 {
            meta_rows.pop();
        } else {
            break;
        }
    }
    // Meta rows cut: the last kept part says so.
    if meta_rows.len() < full_m {
        if let Some(last) = meta_rows.last_mut().and_then(|row| row.last_mut()) {
            *last = "…".into();
        }
    }
    let ellipsize = |lines: &mut Vec<String>, full: usize, st: St| {
        if lines.len() < full {
            if let Some(last) = lines.last_mut() {
                *last = f.fit(&format!("{}…", last.trim_end()), st, IW);
            }
        }
    };
    ellipsize(&mut question, full_q, qs);
    ellipsize(&mut status_lines, full_s, hcap);
    let qh = question.len() as f32 * question_line_height;
    let hero_h = P * 2.
        + theme::row_title_line()
        + S2
        + qh
        + if meta_rows.is_empty() {
            0.
        } else {
            (if compact_hero { 0. } else { S3 })
                + meta_rows.len() as f32 * theme::row_caption_line()
        }
        + if has_answers {
            S1 + theme::row_caption_line()
        } else {
            0.
        }
        + if status_lines.is_empty() {
            0.
        } else {
            S2 + status_lines.len() as f32 * theme::row_caption_line()
        };
    let hero_full_h = hero_h;
    let hero_h = if page { 0. } else { hero_full_h };
    // What the hero gives up when the pane is too short for anything else to: a question's third
    // line and every meta row but the first.
    let hero_lean_cut = if page {
        0.
    } else {
        question.len().saturating_sub(2) as f32 * question_line_height
            + meta_rows.len().saturating_sub(1) as f32 * theme::row_caption_line()
    };
    let active = !page && c.agents.agents.iter().any(|a| agents::is_active(a, c.now));
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
    let empty_help = if page { 0. } else { sell_h };
    // With an accordion open, the "What CSR can do" card keeps its first line only: it never
    // folds (T1), but it yields its other two lines to the card the reader asked for.
    let help_lean = if c.repo_recap.is_none() && empty_help > 0. {
        P * 2. + theme::TITLE_LINE + S3 + theme::BODY_LINE + theme::CAPTION_LINE
    } else {
        empty_help
    };
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
        for d in &c.dream.queue {
            drill_lines.extend(f.wrap(string(&d["line"]), sans(11.5, t.regular), IW));
        }
    }
    // The room an open section asks for, and the least it is given. A page takes the pane and
    // asks for nothing: the dashboard's sections are the agents and the dreams.
    let drill_total = drill_lines.len().saturating_sub(open.scroll);
    let line_card = |lines: usize| lines as f32 * DRILL_LINE + P * 2.;
    let (drill_want, drill_least) = if page || drill_lines.is_empty() {
        (0., 0.)
    } else {
        (
            line_card(drill_total.max(1)),
            line_card(drill_total.clamp(1, DRILL_MIN_LINES)),
        )
    };
    let notice = c.notice.as_ref().filter(|_| !page);
    // A notice wraps to NOTICE_LINES at most: the warning that Claude Code's window beat CSR
    // carries its advice in its last words, which one cut line would lose. It keeps its first
    // line only (the "lean" notice) when the fit has no other room: with a section or a card
    // open, or when every card has folded and the picture would still be cut.
    let notice_lines_of = |lines: usize| -> Vec<String> {
        notice
            .map(|n| f.wrap_max(&icons::line(n), body, IW, lines))
            .unwrap_or_default()
    };
    let notice_height = |lines: &[String]| -> f32 {
        if lines.is_empty() {
            0.
        } else {
            P * 2. + theme::BODY_LINE * lines.len() as f32
        }
    };
    let notice_full = notice_lines_of(NOTICE_LINES);
    let notice_lean = notice_lines_of(1);
    let (notice_full_h, notice_lean_h) = (notice_height(&notice_full), notice_height(&notice_lean));
    let notice_gap = if notice.is_some() { S3 } else { 0. };
    let ctx_missing = c.figure["ctx"]["tokens"].as_f64().is_none_or(|n| n <= 0.);
    let mut dash = if page { None } else { dash_plan(f, c, t) };
    // A new session with no reading yet: its dashboard has no chart to open, so `i` keeps the
    // figure's facts card under it, as it does for the quotas alone.
    let no_reading = dash.as_ref().is_some_and(|plan| plan.ctx.is_none());
    // No reading and no start to tell: the card is the quotas alone, as rings.
    let quotas_only = ctx_missing && dash.is_none();
    // The figure card's smallest height: its one-row gauge strip (or the quotas row).
    let figure_floor: f32 = if page {
        0.
    } else if quotas_only {
        154.
    } else {
        110.
    };
    // A closed pane keeps the Context card tall enough for the small circles, their notes line and
    // the caption; a card an open section squeezed below that is the strip.
    let figure_closed_floor: f32 = if page {
        0.
    } else if let Some(plan) = &dash {
        figure_floor.max(dash_compact_h(plan))
    } else if quotas_only || narrow {
        figure_floor
    } else {
        figure_floor.max(mid_min_h(&c.figure))
    };
    // `i` adds a card of the figure's facts, one to a line, under the figure.
    let fig_detail_all: Vec<String> = if open.figure && !page && (dash.is_none() || no_reading) {
        figure_detail_lines(c, open.gauge)
            .iter()
            .flat_map(|line| f.wrap(line, sans(11.5, t.regular), IW))
            .collect()
    } else {
        vec![]
    };
    // The dashboard draws no card of its own under the figure, but `i` asks the figure for the room
    // of its chart: the cards above give way for it.
    let figure_wants_chart =
        dash.is_some() && open.figure && fresh_bar_heights(&c.requests).is_some();
    let (detail_want, detail_least) = if fig_detail_all.is_empty() {
        (0., 0.)
    } else {
        (
            line_card(fig_detail_all.len()),
            line_card(fig_detail_all.len().min(DRILL_MIN_LINES)),
        )
    };
    let no_items: [Value; 0] = [];
    let observed: [(&'static str, &[Value], bool); 3] = if page {
        [
            (CONFUSED_TITLE, &no_items[..], false),
            (SHOULD_BE_TITLE, &no_items[..], true),
            (LOST_TITLE, &no_items[..], false),
        ]
    } else {
        [
            (CONFUSED_TITLE, &c.confusing[..], false),
            (SHOULD_BE_TITLE, &c.should_be[..], true),
            (LOST_TITLE, &c.open_lost[..], false),
        ]
    };
    let tail_base = agents_h
        + if active { S3 } else { 0. }
        + empty_help
        + if empty_help > 0. { S3 } else { 0. };
    let tail_lean = tail_base - empty_help + help_lean;
    // Text keeps one size: the width fit, `limit` pt of layout for this pane (PANE_RULES.md).
    // Content that does not fit gives way in a fixed order, the first to go first:
    //   1. the CSR performance card's check rows (a failing check stays),
    //   2. its clock and progress bar (the heading and the headline stay),
    //   3. the lists' rows, down to 3,
    //   4. the lists' rows, down to 1,
    //   5. the API credit row,
    //   6. "Self-Reflect this session" from rings to one line of words,
    //   7. the Context card's "this turn" note and cache line,
    //   8. both lists fold into the stack row,
    //   9. the story ("Where the work stands") folds into the stack row,
    //  10. the CSR performance card folds into the stack row,
    //  11. with a section open: the Context card to its floor,
    //  12. with a section open: the section to its least rows.
    // The hero, the Context card, an open section and the stack row are never folded. A folded
    // card is one title in the stack row, and `e` (or a tap on its title) opens it in place as
    // an accordion, over the room of the cards below it. Nothing makes the canvas taller than
    // the pane: past the last step, the picture is cut at the pane's bottom (`clipped`).
    let limit = maxh;
    let has_soak = !page && c.soak.is_some();
    let has_obs = [
        !observed[0].1.is_empty(),
        !observed[1].1.is_empty(),
        !observed[2].1.is_empty(),
    ];
    // What a fold puts into the stack: the stackable cards with something to show.
    let stack_kinds = |fold: u8| -> Vec<usize> {
        STACKABLE
            .iter()
            .enumerate()
            .filter(|(_, (bit, _))| fold & bit != 0)
            .filter(|(i, _)| match i {
                0 | 1 => has_obs[*i],
                2 => has_story,
                _ => has_soak,
            })
            .map(|(i, _)| i)
            .collect()
    };
    let stack_with = |fold: u8| -> f32 {
        if stack_kinds(fold).is_empty() {
            0.
        } else {
            STACK_H + S3
        }
    };
    let top_with =
        |observed_h: f32, story_h: f32, soak_h: f32, stack_h: f32, notice_h: f32, bare: bool| {
            M + hero_h - if bare { hero_lean_cut } else { 0. }
                + S3
                + stack_h
                + observed_h
                + story_h
                + if story_h > 0. { S3 } else { 0. }
                + notice_h
                + notice_gap
                + soak_h
                + if soak_h > 0. { S3 } else { 0. }
        };
    let observed_with = |cap: usize, fold: u8| -> f32 {
        observed
            .iter()
            .enumerate()
            .filter(|(i, (_, items, _))| !items.is_empty() && cap > 0 && fold & OBS_BITS[*i] == 0)
            .map(|(i, (_, items, _))| {
                // The Lost and found card is one row, whatever the cap.
                observation_card_h(if i == 2 { 1 } else { rows_at(items, cap) }) + S3
            })
            .sum()
    };
    let gaps = if drill_want > 0. { S3 } else { 0. } + if detail_want > 0. { S3 } else { 0. };
    let want = drill_want + detail_want + gaps;
    let section_open = want > 0. || figure_wants_chart;
    // The figure card: the height it is laid out at for each choice of dashboard parts.
    let figure_target_with = |credit: bool, compact: bool, lean: u8| -> f32 {
        if quotas_only {
            empty_quota_height(c)
        } else if let Some(plan) = &dash {
            dash_heights_with(plan, true, credit, compact, lean).4
                + if figure_wants_chart {
                    DASH_CHART_WANT
                } else {
                    0.
                }
        } else if open.figure && fresh_bar_heights(&c.requests).is_some() {
            figure_expanded_h(&c.figure)
        } else {
            collapsed_context_layout(&c.figure, 0.).2
        }
    };
    // What the card keeps while cards can still give way: its richest layout. Taken before the
    // fit trims the dashboard's parts.
    let figure_richest = figure_target_with(true, false, 0);
    let figure_floor_with = |credit: bool, compact: bool, lean: u8| -> f32 {
        match &dash {
            Some(plan) => figure_floor.max(dash_heights_with(plan, false, credit, compact, lean).4),
            None => figure_closed_floor,
        }
    };
    let step_soak_h = |step: &FitStep| -> f32 {
        if step.fold & FOLD_PERF != 0 {
            0.
        } else {
            c.soak.as_ref().filter(|_| !page).map_or(0., |card| {
                soak_plan(f, card, t, step.soak, step.soak_rows).h
            })
        }
    };
    let step_story_h = |step: &FitStep| -> f32 {
        if step.fold & FOLD_STORY != 0 {
            0.
        } else {
            story_h_with(step.story_lines, step.story_shifts)
        }
    };
    // `open_bit` is the open accordion card's fold bit (0 with none): the stack row stays drawn
    // while it is open, listing it, so its close control never disappears.
    // The least Context card height at which the collapsed ring layout fits (none: only a
    // dashboard or a layout that is not the ring, which the card may share a pane with).
    let ring_min_h: f32 = if page || quotas_only || narrow || dash.is_some() {
        0.
    } else {
        (0..500)
            .map(|i| 100. + i as f32)
            .find(|h| collapsed_context_fits(&c.figure, *h))
            .unwrap_or(0.)
    };
    let need = |step: &FitStep, open_bit: u8, lean: bool| {
        let accordion = open_bit != 0;
        let notice_h = if lean || accordion || section_open {
            notice_lean_h
        } else {
            notice_full_h
        };
        let section = if step.least {
            drill_least + detail_least + gaps
        } else {
            want
        };
        let figure =
            if section_open && !accordion {
                if step.squeeze {
                    figure_floor
                } else {
                    figure_floor_with(step.credit, step.compact, step.level())
                        .max(figure_target_with(step.credit, step.compact, step.level()))
                }
            } else {
                figure_floor_with(step.credit, step.compact, step.level())
            };
        // The Lost and found card never costs the Context card its collapsed ring: with the card,
        // the figure keeps the height at which that layout fits.
        let figure = if step.fold & FOLD_LOST == 0 && !c.open_lost.is_empty() && !accordion {
            figure.max(ring_min_h)
        } else {
            figure
        };
        (top_with(
            observed_with(step.cap, step.fold),
            step_story_h(step),
            step_soak_h(step),
            stack_with(step.fold | open_bit),
            notice_h,
            step.bare,
        ) + if accordion { tail_lean } else { tail_base }
            + figure
            + if section_open && !accordion {
                section
            } else {
                0.
            })
            / (1. - S3 / available_height as f32)
    };
    let (closed_fit, closed_lean) = if page {
        // A page is one card; nothing is traded for it.
        (FIT_STEPS[FIT_STEPS.len() - 1], false)
    } else {
        // A closed pane keeps the Context card at its closed floor; only an open section may
        // squeeze it, so the last steps repeat the one before them without one.
        let steps = if section_open {
            &FIT_STEPS[..]
        } else {
            &FIT_STEPS[..FIT_STEPS.len() - 2]
        };
        // The notice keeps its lines while any step fits with them; past the last step it gives
        // up all but its first.
        let fits = |lean: bool| steps.iter().find(|step| need(step, 0, lean) <= limit);
        match fits(false) {
            Some(step) => (*step, false),
            None => (*fits(true).unwrap_or(&steps[steps.len() - 1]), true),
        }
    };
    // The accordion: one stacked card open under the stack row. Every other stackable card
    // folds, the Context card goes to its floor, and the open card takes its richest form that
    // fits the room left.
    let accordion_kind = open
        .stack
        .filter(|_| !page && !section_open)
        // Any stackable card with content opens on request, whether or not the closed layout
        // folds it: `e` from an open section clears the section first, and the card it offered
        // must still open.
        .filter(|k| stack_kinds(FOLD_ALL).contains(k));
    let fit = if let Some(kind) = accordion_kind {
        let bit = STACKABLE[kind].0;
        let base_step = step(0, usize::MAX, false, true, FOLD_ALL & !bit, false, false);
        // The card keeps the Context card lean; when even that overflows, bare.
        let choices_of = |base: FitStep| -> Vec<FitStep> {
            let mut choices: Vec<FitStep> = vec![];
            match kind {
                0 | 1 => {
                    for cap in (1..=card_rows(observed[kind].1)).rev() {
                        choices.push(FitStep { cap, ..base });
                    }
                }
                2 => {
                    for shifts in (0..=story_shift_rows).rev() {
                        choices.push(FitStep {
                            story_shifts: shifts,
                            ..base
                        });
                    }
                    for lines in (1..story_line_rows).rev() {
                        choices.push(FitStep {
                            story_shifts: 0,
                            story_lines: lines,
                            ..base
                        });
                    }
                }
                _ => {
                    for level in (0..=2).rev() {
                        choices.push(FitStep {
                            soak: level,
                            ..base
                        });
                    }
                    // Past its leanest level the card cuts its failing rows, "+N more" last.
                    let failing = c.soak.as_ref().map_or(0, |card| soak_rows(card, 0).count());
                    for rows in (1..failing).rev() {
                        choices.push(FitStep {
                            soak: 0,
                            soak_rows: rows,
                            ..base
                        });
                    }
                }
            }
            // A story of only changes keeps one change row: a card is never its title alone.
            if kind == 2 && story_line_rows == 0 && c.learnt.is_empty() {
                choices.retain(|s| s.story_shifts > 0);
            }
            choices
        };
        let lean_choices = choices_of(lean(base_step));
        let bare_choices = choices_of(bare(base_step));
        let last = *bare_choices.last().unwrap_or(&bare(base_step));
        *lean_choices
            .iter()
            .chain(bare_choices.iter())
            .find(|s| need(s, bit, true) <= limit)
            .unwrap_or(&last)
    } else {
        closed_fit
    };
    let accordion = accordion_kind.is_some();
    let (empty_help, tail_base) = if accordion {
        (help_lean, tail_lean)
    } else {
        (empty_help, tail_base)
    };
    let open_bit = accordion_kind.map_or(0, |k| STACKABLE[k].0);
    let lean_notice = closed_lean || accordion || section_open;
    let fit_need = need(&fit, open_bit, lean_notice);
    let (notice_lines, notice_h) = if lean_notice {
        (notice_lean, notice_lean_h)
    } else {
        (notice_full, notice_full_h)
    };
    let clipped = !page && fit_need > limit + 0.5;
    let FitStep {
        soak: soak_level,
        cap: obs_cap,
        credit: keep_credit,
        compact: rings_compact,
        lean: keep_lean,
        bare: keep_bare,
        fold,
        story_lines: story_line_cap,
        story_shifts: story_shift_cap,
        soak_rows: soak_row_cap,
        squeeze,
        least: section_least,
    } = fit;
    let keep_level = u8::from(keep_lean) + u8::from(keep_bare);
    // The bare hero, the last room a pane has: a question keeps two lines and the meta rows one.
    let hero_h = if keep_bare {
        hero_h - hero_lean_cut
    } else {
        hero_h
    };
    if keep_bare && !page {
        if question.len() > 2 {
            question.truncate(2);
            if let Some(last) = question.last_mut() {
                *last = f.fit(&format!("{}…", last.trim_end()), qs, IW);
            }
        }
        if meta_rows.len() > 1 {
            meta_rows.truncate(1);
            if let Some(last) = meta_rows.last_mut().and_then(|row| row.last_mut()) {
                *last = dreams::cut_part(&c.offer_notice, &status, compact_hero);
            }
        }
    }
    let story_hidden = fold & FOLD_STORY != 0;
    let stack_list = if page {
        vec![]
    } else {
        stack_kinds(fold | open_bit)
    };
    let stack_h = if stack_list.is_empty() {
        0.
    } else {
        STACK_H + S3
    };
    let figure_target = if accordion {
        figure_floor_with(keep_credit, rings_compact, keep_level)
    } else {
        figure_target_with(keep_credit, rings_compact, keep_level)
    };
    if let Some(plan) = dash.as_mut() {
        plan.fit(DashFit {
            credit: keep_credit,
            compact_rings: rings_compact,
            lean: keep_lean,
            bare: keep_bare,
        });
    }
    // The CSR performance card sits right above the Context card.
    let soak = c
        .soak
        .as_ref()
        .filter(|_| !page && fold & FOLD_PERF == 0)
        .map(|card| soak_plan(f, card, t, soak_level, soak_row_cap));
    let soak_h = soak.as_ref().map_or(0., |p| p.h);
    let observed_full_h = observed_with(obs_cap, fold);
    let story_h = if story_hidden {
        0.
    } else {
        story_h_with(story_line_cap, story_shift_cap)
    };
    // Everything above the figure for one choice of observation cards and story card.
    let top_for = |observed_h: f32, story_h: f32| {
        top_with(observed_h, story_h, soak_h, stack_h, notice_h, keep_bare)
    };
    // A page keeps at least the rows its heading promises, whatever the pane.
    let maxh = if page {
        maxh.max(
            (M + if open.threads {
                P * 2. + MONOLOGUE_H + (PAGE_MIN_ROWS as f32 * ROW_PITCH - ROW_GAP) + DRILL_LINE
            } else {
                P * 2. + MONOLOGUE_H + DRILL_MIN_LINES as f32 * DRILL_LINE
            }) / (1. - S3 / available_height as f32),
        )
    } else {
        maxh
    };
    let key_gap = S3 * maxh / available_height as f32;
    // The height the figure keeps while cards can still give way: the closed pane's, or the least
    // its layout needs (the expanded chart for `i`, the full ring, the quotas row), whichever is
    // less.
    let closed_figure_h = {
        let available = maxh - top_for(observed_full_h, story_h) - tail_base - key_gap;
        if ctx_missing && empty_help > 0. {
            available.min(figure_richest).max(figure_floor)
        } else {
            available
        }
    };
    let figure_pref = closed_figure_h.min(figure_target).max(figure_floor);
    let mut room = maxh - key_gap - top_for(observed_full_h, story_h) - tail_base - figure_pref;
    if squeeze {
        room += figure_pref - figure_floor;
    }
    let observed_h = observed_full_h;
    let story_card_h = story_h;
    // The drill card is placed first; the facts card takes what it leaves.
    let space = room - gaps;
    let drill_given = if drill_want > 0. {
        if section_least {
            drill_least
        } else {
            drill_want.min(drill_least.max(space - detail_least))
        }
    } else {
        0.
    };
    let detail_given = if detail_want > 0. {
        if section_least {
            detail_least
        } else {
            detail_want.min(detail_least.max(space - drill_given))
        }
    } else {
        0.
    };
    // Lines that fit a given height; one point over, so float rounding never costs the last line.
    let lines_in = |given: f32| ((given - P * 2.) / DRILL_LINE + 0.001).max(0.) as usize;
    // The lines a clipped section draws: fewer than all of them end with a "+N more" line.
    let clip = |all: &[String], skip: usize, given: f32, want: f32| -> Vec<String> {
        let rest: Vec<String> = all.iter().skip(skip).cloned().collect();
        if given >= want || rest.len() <= lines_in(given) {
            return rest;
        }
        let shown = lines_in(given).max(DRILL_MIN_LINES.min(rest.len())) - 1;
        let mut out: Vec<String> = rest.iter().take(shown).cloned().collect();
        out.push(more_line(rest.len() - shown));
        out
    };
    // A page is one card to the bottom of the pane. It shows the rows that fit and scrolls through
    // the rest with j and k: (the first row shown, the most the first row may be, a footer).
    let page_h = if page { maxh - M - key_gap } else { 0. };
    let (page_rows, page_from, page_scroll_max, page_footer) = if open.threads {
        let spine = page_h - P * 2. - MONOLOGUE_H;
        let total = c.river.len();
        let all = ((spine + ROW_GAP) / ROW_PITCH + 0.001).max(0.) as usize;
        if total <= all {
            (total, 0, 0, String::new())
        } else {
            let fit = (((spine - DRILL_LINE + ROW_GAP) / ROW_PITCH + 0.001).max(0.) as usize)
                .max(PAGE_MIN_ROWS)
                .min(total);
            let max = total - fit;
            let from = open.scroll.min(max);
            (fit, from, max, scroll_footer(from, fit, total))
        }
    } else if open.changes {
        let total = drill_lines.len();
        let all = lines_in(page_h - MONOLOGUE_H);
        if total <= all {
            (total, 0, 0, String::new())
        } else {
            let fit = all.saturating_sub(1).max(DRILL_MIN_LINES - 1).min(total);
            let max = total - fit;
            let from = open.scroll.min(max);
            (fit, from, max, scroll_footer(from, fit, total))
        }
    } else {
        (0, 0, 0, String::new())
    };
    let drill_rows: Vec<String> = if open.changes {
        drill_lines
            .iter()
            .skip(page_from)
            .take(page_rows)
            .cloned()
            .chain((!page_footer.is_empty()).then(|| page_footer.clone()))
            .collect()
    } else if drill_lines.is_empty() {
        vec![]
    } else {
        clip(&drill_lines, open.scroll, drill_given, drill_want)
    };
    let thread_bands = if open.threads { page_rows } else { 0 };
    let extra_h = if page {
        page_h
    } else if drill_rows.is_empty() {
        0.
    } else {
        line_card(drill_rows.len())
    };
    let fig_detail_lines = if open.figure && !page {
        clip(&fig_detail_all, 0, detail_given, detail_want)
    } else {
        vec![]
    };
    let fig_detail_h = if fig_detail_lines.is_empty() {
        0.
    } else {
        line_card(fig_detail_lines.len())
    };
    let tail_core = tail_base
        + if fig_detail_h > 0. {
            fig_detail_h + S3
        } else {
            0.
        };
    let top_core = top_for(observed_h, story_card_h) + extra_h + if extra_h > 0. { S3 } else { 0. };
    let card_gap = S3;
    let (figure_h, help_h) = if page {
        (0., 0.)
    } else if ctx_missing && empty_help > 0. {
        let available = maxh - top_core - tail_core - key_gap;
        let capped = if quotas_only {
            empty_quota_height(c)
        } else {
            figure_target
        };
        let figure_h = available.min(capped).max(figure_floor);
        // Cards always stack from the top. The final card owns surplus height.
        (figure_h, empty_help + (available - figure_h).max(0.))
    } else {
        // Past the last fit step (`clipped`) this can go below zero; never draw a negative card.
        let room = (maxh - top_core - tail_core - key_gap).max(0.);
        // A new session's card hugs its content: with nothing below it to take the surplus, a
        // taller card would be a hollow tail (PANE_RULES.md #14). The live card is not capped:
        // its chart takes what is spare.
        let hug = dash
            .as_ref()
            .is_some_and(|plan| plan.start.is_some() && !figure_wants_chart);
        (
            if hug {
                room.min(figure_target.max(figure_floor))
            } else {
                room
            },
            empty_help,
        )
    };
    let height = maxh;
    let mut canvas = Canvas::pooled(W, height);
    canvas.output_scale = (width as f32 / W).min(available_height as f32 / height);
    canvas.background(t);
    let mut y = M;
    let mut cards: Vec<CardRegion> = vec![];
    let mut overdrawn: Vec<(&'static str, f32, f32)> = vec![];
    let mut answer_spans = vec![];
    let mut offer_hit = None;
    let mut answer_y = None;
    if !page {
        cards.push(CardRegion::new(
            "hero",
            y,
            hero_h,
            if compact_hero {
                meta_rows.len()
            } else {
                question.len()
            },
        ));
        panel(&mut canvas, y, hero_h, t);
        let title_y = y + P + f.cap(htitle);
        let title_text = crate::companion::contract::dreamer_title(c);
        label(
            &mut canvas,
            f,
            title_text,
            IX,
            title_y,
            theme::row_title_size(),
            t.ink,
            t.bold,
        );
        if compact_hero {
            // The state in words, on the title row, in the dreamer's colour.
            let tag = crate::companion::contract::dreamer_tag(c).unwrap_or("Looks as you work");
            let said = &format!(" · {tag}");
            let x = IX + f.width(title_text, htitle);
            single(
                &mut canvas,
                f,
                said,
                x,
                title_y,
                W - M - P - 24. - S2 - x,
                theme::row_body_size(),
                t.dream,
                t,
            );
        }
        if c.dream.carried {
            label(
                &mut canvas,
                f,
                "CARRIED",
                IX + f.width("The dreamer asks", htitle) + S3,
                title_y,
                theme::row_caption_size(),
                t.dream,
                t.medium,
            );
        }
        canvas.icon(
            f,
            if c.offer { Icon::Moon } else { Icon::MoonStar },
            W - M - P - 24.,
            title_y,
            24.,
            theme::row_title_size(),
            t.dream,
        );
        let mut cursor = y + P + theme::row_title_line() + S2;
        for line in &question {
            canvas.text(f, line, IX, cursor + f.cap(qs), qs, t.ink, A::L);
            cursor += question_line_height;
        }
        if !meta_rows.is_empty() && !compact_hero {
            cursor += S3;
        }
        for parts in &meta_rows {
            let b = cursor + f.cap(hcap);
            let mut x = IX;
            for part in parts {
                let mut shown: &str = part;
                if let Some(rest) = part.strip_prefix("dreamer ") {
                    // The dreamer's share of its token budget as a small ring.
                    let spent = num(&c.dream.budget["spent"]);
                    let cap = num(&c.dream.budget["cap"]).max(1.);
                    let frac = (spent / cap).clamp(0., 1.) as f32;
                    let cy = b - f.cap(hcap) / 2.;
                    canvas.ring(
                        x + budget_ring_r(),
                        cy,
                        budget_ring_r(),
                        2.,
                        t.track,
                        &[(0., frac.max(4. / 360.), t.dream)],
                    );
                    x += 2. * budget_ring_r() + ICON_GAP;
                    shown = rest;
                }
                let st = if shown.starts_with('⌗') {
                    mono(theme::row_chip_size(), t.regular)
                } else {
                    hcap
                };
                let room = IX + IW - x;
                let w = f.width(shown, st).min(room);
                if shown.starts_with('⌗') {
                    canvas.rrect(
                        x - S1,
                        b - f.cap(st) - S1 / 2.,
                        w + S2,
                        theme::row_chip_line(),
                        S1,
                        t.chip,
                    );
                    canvas.text(f, shown, x, b, st, t.dream, A::L);
                } else {
                    single(
                        &mut canvas,
                        f,
                        shown,
                        x,
                        b,
                        room,
                        theme::row_caption_size(),
                        t.quiet,
                        t,
                    );
                }
                x += w + S3;
            }
            cursor += theme::row_caption_line();
        }
        if has_answers {
            cursor += S1;
        }
        if c.dream.state == Batch::Ready {
            let b = cursor + f.cap(hcap);
            answer_y = Some(b);
            let mut x = IX;
            for (label, answer) in [
                ("1 ✓ helpful", "helpful"),
                ("2 \u{e1c2} thought of it", "thought_of_it"),
                ("3 ✗ not relevant", "not_relevant"),
                ("4 → ask the agent", "ask_claude"),
            ] {
                let w = canvas.text(
                    f,
                    label,
                    x,
                    b,
                    sans(theme::row_caption_size(), t.medium),
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
            cursor += theme::row_caption_line();
        } else if c.offer {
            let b = cursor + f.cap(hcap);
            let mut x = IX;
            x += canvas.text(f, "Press", x, b, hcap, t.quiet, A::L) + ICON_GAP;
            let key_w = f.width("1", hcap);
            canvas.rrect(
                x - S1,
                b - f.cap(hcap) - S1 / 2.,
                key_w + S2,
                theme::row_caption_line(),
                S1,
                t.chip,
            );
            canvas.text(f, "1", x, b, hcap, t.dream, A::L);
            x += key_w + ICON_GAP;
            x += canvas.text(
                f,
                "to turn dreams on in all your sessions.",
                x,
                b,
                hcap,
                t.quiet,
                A::L,
            );
            offer_hit = Some((b, IX, x));
            cursor += theme::row_caption_line();
        }
        if !status_lines.is_empty() {
            cursor += S2;
            for line in &status_lines {
                label(
                    &mut canvas,
                    f,
                    line,
                    IX,
                    cursor + f.cap(hcap),
                    theme::row_caption_size(),
                    t.quiet,
                    t.regular,
                );
                cursor += theme::row_caption_line();
            }
        }
        if cursor + P > y + hero_h + 0.5 {
            overdrawn.push(("hero", cursor + P, y + hero_h));
        }
    }
    if !page {
        y += hero_h + card_gap;
    }
    // The stack row: every folded card as one title, under the hero. A tap on a title, or `e`,
    // opens that card under it.
    let mut stack_row = None;
    if !stack_list.is_empty() {
        let open_kind = if accordion { accordion_kind } else { None };
        cards.push(CardRegion::new("stack", y, STACK_H, stack_list.len()));
        stack_row = Some(draw_stack_row(&mut canvas, f, &stack_list, open_kind, y, t));
        y += STACK_H + card_gap;
    }
    let mut observation_rows = vec![];
    let mut observation_titles = vec![];
    // The observation rows have their own popup: their cut rows are not recorded as shortened rows.
    let recording = std::mem::take(&mut canvas.cut_title);
    for (which, (title_text, items, should)) in observed.into_iter().enumerate() {
        // A list with nothing to show, or folded into the stack, is not drawn here.
        if items.is_empty() || obs_cap == 0 || fold & OBS_BITS[which] != 0 {
            continue;
        }
        let lost_card = which == 2;
        let same_page =
            !should && !lost_card && items.first().is_some_and(|i| i["same_page"] == true);
        let shown = if lost_card {
            1
        } else {
            rows_at(items, obs_cap)
        };
        let h = observation_card_h(shown);
        let (row_title, row_body, row_caption) = (
            serif(theme::row_title_size(), t.bold),
            sans(theme::row_body_size(), t.regular),
            sans(theme::row_caption_size(), t.regular),
        );
        cards.push(CardRegion::new(title_text, y, h, shown));
        panel(&mut canvas, y, h, t);
        // The "Same page?" card's mark is its sprite, a little taller than its title.
        let title_x = if same_page {
            let tall = f.cap(row_title) * 1.5;
            let center = y + P + f.cap(row_title) / 2.;
            let mood = crate::companion::same_page::mood_of(&items[0]);
            IX + sprite(
                &mut canvas,
                IX,
                center,
                tall,
                mood,
                theme::SPRITE_CLAY,
                t.ground,
            ) + ICON_GAP
        } else {
            IX
        };
        label(
            &mut canvas,
            f,
            if same_page {
                crate::companion::same_page::TITLE
            } else {
                title_text
            },
            title_x,
            y + P + f.cap(row_title),
            theme::row_title_size(),
            t.ink,
            t.bold,
        );
        if lost_card {
            // One row, the newest thread: a tap on it opens its handoff popup.
            canvas.text(
                f,
                &if items.len() > 1 {
                    format!("1 of {} · tap opens", items.len())
                } else {
                    "tap opens".to_owned()
                },
                IX + IW,
                y + P + f.cap(row_caption),
                row_caption,
                t.quiet,
                A::R,
            );
            let row_y = y + P + theme::row_title_line() + S2;
            let base = row_y + f.cap(row_body);
            let text = string(&items[0]["text"]);
            single(
                &mut canvas,
                f,
                text,
                IX,
                base,
                IW,
                theme::row_body_size(),
                t.ink,
                t,
            );
            if !canvas.dry {
                canvas.cuts.push(Cut {
                    y: row_y + theme::row_body_line() / 2.,
                    size: theme::row_body_line() / 2.,
                    title: LOST_TITLE,
                    full: text.to_owned(),
                    lost: Some(items[0].clone()),
                    handed: false,
                });
            }
            y += h + card_gap;
            continue;
        }
        if same_page {
            canvas.text(
                f,
                "tap opens",
                IX + IW,
                y + P + f.cap(row_caption),
                row_caption,
                t.quiet,
                A::R,
            );
            observation_titles.push((ObsCard::Confusing, y + P, y + P + theme::row_title_line()));
            let mut row_y = y + P + theme::row_title_line() + S2;
            // The numbers keep a row of their own; the editorial takes the rows left.
            let mut words = vec![observation::one_line(
                f,
                string(&items[0]["numbers"]),
                row_body,
                IW,
            )];
            words.extend(f.wrap_max(
                string(&items[0]["words"]),
                row_body,
                IW,
                shown.saturating_sub(1),
            ));
            for (at, words) in words.iter().enumerate() {
                observation_rows.push((ObsCard::Confusing, 0, row_y + theme::row_body_line() / 2.));
                let base = row_y + f.cap(row_body);
                // The first row says the numbers, in the ink that carries weight.
                let ink = if at == 0 { t.ink } else { t.quiet };
                canvas.text(f, words, IX, base, row_body, ink, A::L);
                row_y += theme::row_body_line();
            }
            y += h + card_gap;
            continue;
        }
        if shown < items.len() {
            canvas.text(
                f,
                &format!("{shown} of {} · o opens", items.len()),
                IX + IW,
                y + P + f.cap(row_caption),
                row_caption,
                t.quiet,
                A::R,
            );
        }
        observation_titles.push((
            if should {
                ObsCard::ShouldBe
            } else {
                ObsCard::Confusing
            },
            y + P,
            y + P + theme::row_title_line(),
        ));
        let mut row_y = y + P + theme::row_title_line() + S2;
        // The kind column is as wide as the widest label it holds, measured in the face it is
        // drawn in, so "CLAUDE.md rule" never runs into its row's text.
        let kind_w = if should {
            let widest = items
                .iter()
                .take(shown)
                .map(|item| {
                    f.width(
                        should_be_kind(item),
                        sans(theme::row_caption_size(), t.medium),
                    )
                })
                .fold(0., f32::max);
            // Never narrower than the column the short labels always had.
            (widest + S3).max(78. * theme::row_caption_size() / theme::CAPTION_SIZE)
        } else {
            0.
        };
        for (index, item) in items.iter().take(shown).enumerate() {
            observation_rows.push((
                if should {
                    ObsCard::ShouldBe
                } else {
                    ObsCard::Confusing
                },
                index,
                row_y + theme::row_body_line() / 2.,
            ));
            let base = row_y + f.cap(row_body);
            let receipt = evidence_receipt(item);
            let receipt_w = f.width(&receipt, row_caption);
            if should {
                label(
                    &mut canvas,
                    f,
                    should_be_kind(item),
                    IX,
                    base,
                    theme::row_caption_size(),
                    t.dream,
                    t.medium,
                );
            }
            single(
                &mut canvas,
                f,
                string(&item["line"]),
                IX + kind_w,
                base,
                IW - kind_w - receipt_w - S3,
                theme::row_body_size(),
                t.ink,
                t,
            );
            canvas.text(f, &receipt, IX + IW, base, row_caption, t.quiet, A::R);
            row_y += theme::row_body_line();
        }
        y += h + card_gap;
    }
    canvas.cut_title = recording;
    if notice.is_some() {
        cards.push(CardRegion::new("notice", y, notice_h, notice_lines.len()));
        panel(&mut canvas, y, notice_h, t);
        for (i, line) in notice_lines.iter().enumerate() {
            single(
                &mut canvas,
                f,
                line,
                IX,
                y + P + f.cap(body) + i as f32 * theme::BODY_LINE,
                IW,
                theme::BODY_SIZE,
                t.ink,
                t,
            );
        }
        y += notice_h + card_gap;
    }
    let mut shift_hits = vec![];
    let mut story_line_count = 0usize;
    let story_region = (story_h > 0.).then_some((y, story_card_h));
    let mut story_open = None;
    let story_title = story_title(c);
    if story_h > 0. {
        cards.push(CardRegion::new(
            story_title,
            y,
            story_h,
            story_line_cap.min(story_line_rows)
                + usize::from(!c.learnt.is_empty())
                + story_shift_cap.min(story_shift_rows),
        ));
        panel(&mut canvas, y, story_h, t);
        label(
            &mut canvas,
            f,
            story_title,
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
        // The accordion may cut the story to fewer rows: its last row then ends in "…".
        let mut story_shown: Vec<String> = story_lines_fit
            .iter()
            .take(story_line_cap)
            .cloned()
            .collect();
        if story_shown.len() < story_lines_fit.len() {
            if let Some(last) = story_shown.last_mut() {
                *last = f.fit(&format!("{} …", last.trim_end_matches('…')), body, IW);
                if !last.ends_with('…') {
                    last.push('…');
                }
            }
        }
        story_line_count = story_shown.len();
        // A story cut at a clause is drawn with its "…": every row of it opens the whole story.
        let story_cut = story_shown.last().is_some_and(|l| l.ends_with('…'));
        for line in &story_shown {
            if story_cut && !canvas.dry {
                canvas.cuts.push(Cut {
                    y: cursor + f.cap(body),
                    size: theme::BODY_SIZE,
                    title: "Where the work stands",
                    full: c.story.clone(),
                    lost: None,
                    handed: false,
                });
            }
            canvas.text(f, line, IX, cursor + f.cap(body), body, t.ink, A::L);
            cursor += theme::BODY_LINE;
        }
        story_open = Some((y, cursor, story_title.into(), story_full(c)));
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
        let shifts_shown = story_shift_cap.min(story_shift_rows);
        if shifts_shown > 0 {
            cursor += S3;
        }
        for v in c.shifts.iter().take(shifts_shown) {
            let b = cursor + f.cap(caption);
            shift_hits.push(b - f.cap(caption) / 2.);
            canvas.cut_title = "A change";
            single_as(
                &mut canvas,
                f,
                &shift_row(v, false),
                &shift_full(v),
                IX,
                b,
                IW,
                theme::CAPTION_SIZE,
                if mark(v) == "!" { t.risk } else { t.mint },
                t,
            );
            canvas.cut_title = recording;
            cursor += theme::CAPTION_LINE;
        }
        y += story_h + card_gap;
    }
    let drill_region = (extra_h > 0.).then_some((y, extra_h));
    if extra_h > 0. {
        cards.push(CardRegion::new(
            if open.threads {
                MONOLOGUE
            } else if open.changes {
                CHANGES
            } else if open.agents {
                "Agents"
            } else {
                "Dreams"
            },
            y,
            extra_h,
            if open.threads {
                thread_bands
            } else {
                drill_rows.len()
            },
        ));
        panel(&mut canvas, y, extra_h, t);
        let mut b = y + P;
        if open.changes {
            // The page: a heading and what the page holds, then each change with what it
            // replaced.
            let title_st = serif(20., t.bold);
            let head = y + P + f.cap(title_st);
            canvas.text(f, CHANGES, IX, head, title_st, t.ink, A::L);
            single(
                &mut canvas,
                f,
                &changes_note(c),
                IX,
                head + 16.,
                IW,
                theme::CAPTION_SIZE,
                t.quiet,
                t,
            );
            b += MONOLOGUE_H;
        }
        if open.changes || open.dreams || open.agents {
            for line in &drill_rows {
                b += 12.;
                label(&mut canvas, f, line, IX, b, 11.5, t.ink, t.regular);
                b += DRILL_LINE - 12.;
            }
        } else if open.threads {
            let title_st = serif(20., t.bold);
            let head = y + P + f.cap(title_st);
            canvas.text(f, MONOLOGUE, IX, head, title_st, t.ink, A::L);
            single(
                &mut canvas,
                f,
                MONOLOGUE_SUB,
                IX,
                head + 16.,
                IW,
                theme::CAPTION_SIZE,
                t.quiet,
                t,
            );
            let top = y + P + MONOLOGUE_H;
            if c.river.is_empty() {
                single(
                    &mut canvas,
                    f,
                    NO_THREADS,
                    IX,
                    top + 12.,
                    IW,
                    11.5,
                    t.ink,
                    t,
                );
            }
            let spine_x = IX + 72.;
            let rows: Vec<_> = c.river.iter().skip(page_from).take(thread_bands).collect();
            // The first baseline sits half a band plus a cap-height below the first band's top.
            let b = top + ROW_BAND / 2. + 4.;
            // The spine runs between the icons, never through them, and ends at the last one.
            let mut from = b - 4. + 9.;
            for i in 1..rows.len() {
                let mid = b + i as f32 * ROW_PITCH - 4.;
                canvas.vhair(spine_x, from, (mid - 9. - from).max(0.), t.band);
                from = mid + 9.;
            }
            let mut b = b;
            for row in rows {
                let kind_col = river_color(row.kind, t);
                let band_y = b - 4. - ROW_BAND / 2.;
                canvas.rrect(
                    IX - 8.,
                    band_y,
                    IW + 16.,
                    ROW_BAND,
                    5.,
                    al(kind_col, BAND_ALPHA),
                );
                canvas.rect(IX - 8., band_y + 3., 1., ROW_BAND - 6., kind_col);
                river_icon(&mut canvas, f, row.kind, spine_x, b - 4., t);
                let when = row.time_label(c.now);
                if !when.is_empty() {
                    single(&mut canvas, f, &when, IX, b, 62., 9.5, t.quiet, t);
                }
                let mut x = IX + 88.;
                let mood = row.mood();
                if !mood.is_empty() {
                    let st = sans(10., t.regular);
                    let w = canvas.text_slanted(f, mood, x, b, st, t.quiet, A::L, ITALIC_SLANT);
                    x += w + 6.;
                }
                // A Lost and found row opens its handoff whole or cut; any other row opens its
                // words when it is cut.
                canvas.cut_title = if row.thread.is_some() {
                    ""
                } else {
                    "Monologue"
                };
                let full = format!(
                    "{}{}{}",
                    if when.is_empty() {
                        String::new()
                    } else {
                        format!("{when} · ")
                    },
                    if mood.is_empty() {
                        String::new()
                    } else {
                        format!("{mood} · ")
                    },
                    row.picture_text()
                );
                single_as(
                    &mut canvas,
                    f,
                    &row.picture_text(),
                    &full,
                    x,
                    b,
                    IX + IW - x,
                    11.5,
                    t.ink,
                    t,
                );
                if let Some(thread) = row.thread.as_ref().filter(|_| !canvas.dry) {
                    // The whole band is the target: its middle and half its height.
                    canvas.cuts.push(Cut {
                        y: b - 4.,
                        size: ROW_BAND / 2.,
                        title: "Lost and found",
                        full: row.picture_text(),
                        lost: Some(thread.clone()),
                        handed: false,
                    });
                }
                canvas.cut_title = recording;
                b += ROW_PITCH;
            }
            if !page_footer.is_empty() {
                let line_top = y + extra_h - P - DRILL_LINE;
                label(
                    &mut canvas,
                    f,
                    &page_footer,
                    IX,
                    line_top + 12.,
                    11.5,
                    t.quiet,
                    t.regular,
                );
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
    let mut soak_region = None;
    if let (Some(card), Some(plan)) = (&c.soak, &soak) {
        cards.push(CardRegion::new(
            "CSR performance",
            y,
            plan.h,
            plan.head.len() + plan.shown,
        ));
        draw_soak(&mut canvas, f, card, plan, y, t);
        soak_region = Some((y, plan.h));
        y += plan.h + card_gap;
    }
    let figure_draw_h = figure_h;
    let figure_layout_h = figure_h;
    if !page {
        cards.push(CardRegion::new("Context", y, figure_h, 1));
    }
    let figure_region = (y, figure_draw_h);
    let fig_info = if page {
        None
    } else {
        figure(
            &mut canvas,
            f,
            c,
            y,
            figure_layout_h,
            t,
            // The dashboard draws its chart in whatever room it gets (DASH_CHART_MIN at least);
            // the ring layouts need FIGURE_EXPANDED_BASE.
            figure_wants_chart || (open.figure && figure_h >= FIGURE_EXPANDED_BASE),
            narrow,
            DashFit {
                credit: keep_credit,
                compact_rings: rings_compact,
                lean: keep_lean,
                bare: keep_bare,
            },
        )
    };
    if let Some(d) = canvas.dash.as_ref().filter(|_| !page) {
        if d.rows_bottom > d.card_bottom + 0.5 {
            overdrawn.push(("Context", d.rows_bottom, d.card_bottom));
        }
    }
    if let Some((cy, ring, group, avail, mid)) = fig_info {
        ceiling_y = cy;
        ring_bounds = ring;
        ring_group = group;
        ring_formula_avail = avail;
        mid_layout = mid;
    }
    y += figure_draw_h;
    let mut figure_bottom = if page { y - card_gap } else { y };
    let figure_detail_region = (fig_detail_h > 0.).then_some((y + card_gap, fig_detail_h));
    if fig_detail_h > 0. {
        y += card_gap;
        cards.push(CardRegion::new(
            "figure facts",
            y,
            fig_detail_h,
            fig_detail_lines.len(),
        ));
        panel(&mut canvas, y, fig_detail_h, t);
        let mut b = y + P;
        for line in &fig_detail_lines {
            b += 12.;
            label(&mut canvas, f, line, IX, b, 11.5, t.ink, t.regular);
            b += DRILL_LINE - 12.;
        }
        y += fig_detail_h;
        figure_bottom = y;
    }
    let needs_tail = active || help_h > 0.;
    if needs_tail {
        y += card_gap;
    }
    let agents_region = active.then_some((y, agents_h));
    if active {
        let agents_draw_h = agents_h;
        cards.push(CardRegion::new("agents", y, agents_draw_h, 1));
        panel(&mut canvas, y, agents_draw_h, t);
        let mut x = IX;
        canvas.cut_title = "Agent in flight";
        for a in c
            .agents
            .agents
            .iter()
            .filter(|a| agents::is_active(a, c.now))
            .take(10)
        {
            let is_stalled = agents::stalled(a, c.now);
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
        let (words, warns) = agents_bar(&c.agents.agents, c.now);
        let (words, ink) = if warns {
            (format!("{} {words}", Icon::TriangleAlert.glyph()), t.amber)
        } else {
            (words, t.quiet)
        };
        single(
            &mut canvas,
            f,
            &words,
            x + S2,
            y + P + f.cap(body),
            W - x - 40.,
            11.5,
            ink,
            t,
        );
        canvas.cut_title = recording;
        y += agents_draw_h;
        if help_h > 0. {
            y += card_gap;
        }
    }
    let mut history_rows = 0;
    let mut history_bottom = None;
    let help_region = (help_h > 0.).then_some((y, help_h));
    if let Some((help_y, help_draw_h)) = help_region {
        cards.push(CardRegion::new("help", help_y, help_draw_h, 1));
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
                &recap.age,
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
                // Marks sit in their own gutter, only when some row has one.
                let gutter = if recap
                    .history
                    .iter()
                    .take(available_rows)
                    .any(|prior| history_mark(&prior.outcome).is_some())
                {
                    16.
                } else {
                    0.
                };
                for prior in recap.history.iter().take(available_rows) {
                    if let Some(mark) = history_mark(&prior.outcome) {
                        label(
                            &mut canvas,
                            f,
                            mark,
                            IX,
                            cursor + f.cap(body),
                            theme::CAPTION_SIZE,
                            t.quiet,
                            t.regular,
                        );
                    }
                    let age = &prior.age;
                    let age_w = (f.width(age, caption) + gutter).max(88.);
                    label(
                        &mut canvas,
                        f,
                        age,
                        IX + gutter,
                        cursor + f.cap(body),
                        theme::CAPTION_SIZE,
                        t.quiet,
                        t.regular,
                    );
                    single(
                        &mut canvas,
                        f,
                        earlier_intent(&prior.intent),
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
            if c.empty_session {
                starter_cards(&mut canvas, f, cursor + S4, help_y + help_draw_h - P, t);
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
            let sell_lines = if accordion { 1 } else { features.len() };
            for (line, (icon, caption_text)) in sells().iter().zip(features).take(sell_lines) {
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
            if c.empty_session {
                starter_cards(&mut canvas, f, cursor, help_y + help_draw_h - P, t);
            }
        }
        y = help_y + help_draw_h;
    }
    let last_card_bottom = if let Some((hy, hh)) = help_region {
        hy + hh
    } else if active {
        agents_region.map(|(ay, ah)| ay + ah).unwrap_or(y)
    } else {
        figure_bottom
    };
    let last_card_bottom_pad = (height - last_card_bottom).max(0.);
    let mut popup_keys = vec![];
    let mut popup_scroll_max = 0;
    if let Some(detail) = open.popup.and_then(|at| c.detail(at)) {
        popup_scroll_max = observation::handed_scroll_max(f, &detail, height, t);
        popup_keys = observation::overlay(
            &mut canvas,
            f,
            &detail,
            height,
            last_card_bottom.min(height),
            t,
        );
    }
    let scale = (width as f32 / W).min(available_height as f32 / height);
    let fitted_width = (W * scale).round().min(width as f32) as u32;
    let fitted_height = available_height;
    let content_h = (height * scale).round().min(available_height as f32) as u32;
    // Lanczos3 a row at a time: no copy of the canvas, no whole-picture f32 buffer (`resample`).
    let size = (canvas.pm.width(), canvas.pm.height());
    let to = (fitted_width, content_h.max(1));
    let fitted = super::resample::lanczos3_resize(canvas.pm.data(), size, to)?;
    // The canvas is spent: its buffer, and the resized one below, wait for the next frame.
    let spent = std::mem::replace(&mut canvas.pm, tiny_skia::Pixmap::new(1, 1)?);
    super::bufpool::keep(spent.take());
    let left = (width - fitted_width) / 2;
    let mut backdrop = Canvas::pooled(width as f32 / S, fitted_height as f32 / S);
    backdrop.background(t);
    let mut output = image::RgbaImage::from_raw(width, fitted_height, backdrop.pm.take())?;
    image::imageops::overlay(&mut output, &fitted, i64::from(left), 0);
    super::bufpool::keep(fitted.into_raw());
    let pm = tiny_skia::Pixmap::from_vec(
        output.into_raw(),
        tiny_skia::IntSize::from_wh(width, fitted_height)?,
    )?;
    let font_fallbacks = f.used.borrow().clone();
    Some((
        pm,
        Metrics {
            cards,
            overdrawn,
            limit,
            clipped: clipped.then_some(fit_need),
            stack: stack_row,
            text_runs: std::mem::take(&mut canvas.runs),
            panels: std::mem::take(&mut canvas.panels),
            csr_cost: c.csr_cost.clone(),
            quota_gauges: canvas.quota_gauges.clone(),
            note_rows: canvas.note_rows.clone(),
            breakdown_bars: canvas.breakdown_bars.clone(),
            dash: canvas.dash.clone(),
            soak_region,
            by_csr: canvas.by_csr.clone(),
            fresh_plots: canvas.fresh_plots.clone(),
            question_size: qs.size,
            question_line_height,
            list_row_line: theme::row_body_line(),
            font_fallbacks,
            content_left: left as f32 / width as f32,
            content_width: fitted_width as f32 / width as f32,
            dream: hero_h,
            answer_y,
            answer_spans,
            offer_hit,
            story_region,
            shift_hits,
            observation_rows,
            observation_titles,
            story_open,
            popup_keys,
            popup_scroll_max,
            page_scroll_max: page.then_some(page_scroll_max),
            cuts: canvas.cuts.clone(),
            agents_region,
            drill_region,
            figure_detail_region,
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
    P + theme::TITLE_LINE
        + S2
        + 2. * empty_quota_radius(c)
        + 46.
        + theme::CAPTION_LINE
        + notes_h(&c.figure)
        + P
}

fn context_title_height(g: &Value) -> f32 {
    P + theme::TITLE_LINE
        + S2
        + if turn_line(g).is_some() {
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

/// "N by CSR" at the top right of the card that starts at `y`: CSR's own work this session.
fn by_csr_mark(c: &mut Canvas, f: &Fonts, t: &Tokens, n: u64, y: f32) {
    let st = serif(24., t.medium);
    let base = y + P + f.cap(st);
    c.by_csr.push((y + P, base));
    c.text(f, &format!("{n}"), W - 100., base, st, t.ink, A::M);
    label(c, f, "by CSR", W - 84., base, 10., t.quiet, t.regular);
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
    dash_fit: DashFit,
) -> Option<FigureInfo> {
    let g = &data.figure;
    if let Some(mut plan) = dash_plan(f, data, t) {
        plan.fit(dash_fit);
        if h + 0.5 >= dash_compact_h(&plan) {
            panel(c, y, h, t);
            draw_dashboard(c, f, data, &plan, y, h, expanded, t);
            return Some((None, None, None, None, None));
        }
    }
    let expanded =
        expanded && fresh_bar_heights(&data.requests).is_some() && h >= figure_expanded_min_h(g);
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    let size = g["ctx"]["size"].as_f64().filter(|n| *n > 0.);
    let collapsed_scaled_fits = collapsed_context_fits(g, h);
    // Strip only for narrow panes or truly strip-height cards. Mid-height cards get their own layout.
    const STRIP_H: f32 = 110.;
    let use_strip = tokens.is_some()
        && (narrow
            || h <= STRIP_H
            || (!expanded && !collapsed_scaled_fits && h < mid_min_h(g) - MID_MIN_SLOP));
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
    // Whether the quota row's notes line has room. The expanded chart shrinks to make it; the
    // collapsed ring may have none to spare: then the notes line is not drawn.
    let mut notes_drawn = true;
    let notes_gap = if notes_drawn { notes_h(g) } else { 0. };
    label(
        c,
        f,
        if tokens.is_some() {
            "Context"
        } else {
            "Quotas"
        },
        IX,
        y + P + f.cap(heading(t)),
        theme::TITLE_SIZE,
        t.ink,
        t.bold,
    );
    if let Some(line) = turn_line(g).filter(|_| tokens.is_some() && !expanded) {
        let caption_y = y + P + theme::TITLE_LINE + S2;
        single(
            c,
            f,
            &line,
            IX,
            caption_y + f.cap(sans(theme::CAPTION_SIZE, t.regular)),
            IW,
            11.5,
            t.quiet,
            t,
        );
        breakdown_bar(c, g, IX, caption_y + theme::CAPTION_LINE + S1, IW, t);
    }
    if let Some(n) = data.tally["by_csr"].as_u64().filter(|n| *n > 0) {
        by_csr_mark(c, f, t, n, y);
    }
    if let Some((line, sev)) = csr_status(g) {
        let col = if sev == "red" { t.risk } else { t.amber };
        c.icon(
            f,
            Icon::CircleAlert,
            IX + context_title_w(f, t) + S3,
            y + P + f.cap(heading(t)),
            12.,
            10.,
            col,
        );
        single(
            c,
            f,
            &line,
            IX + context_title_w(f, t) + S3 + 18.,
            y + P + f.cap(heading(t)),
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
            let gauge_cy = y + h - 84. - notes_gap;
            let chart_bottom = (gauge_cy - 72.).max(y + 70. - S2 + EXPANDED_PLOT_MIN);
            expanded_chart_bottom = chart_bottom;
            history_plot(c, f, data, y + 70., chart_bottom - (y + 70.) + S2, t);
            if let Some(line) = turn_line(g) {
                single(c, f, &line, IX, chart_bottom + 15., IW, 11.5, t.quiet, t);
            }
            breakdown_bar(c, g, IX + 130., y + 49., IW - 130., t);
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
            // The closed pane's height is the least the ring needs, so the notes line takes only
            // room that is spare: from the history plot's, which keeps its own minimum, or from
            // the slack around the group. The notes line matters more than the plot: a plot that
            // would fall under its minimum is dropped, and the slack goes to the notes line.
            let spare = notes_h(g);
            let notes_floor = spare + S2;
            let mut history_h = if fresh_bar_heights(&data.requests).is_some() && slack >= 50. {
                slack
            } else {
                0.
            };
            if history_h > 0. && history_h - spare >= 50. {
                history_h -= spare;
            } else if history_h > 0. {
                history_h = 0.;
            }
            let quota_radius_in = |room: f32| {
                (30. + room / 4.)
                    .min(54.)
                    .min(empty_quota_radius(data))
                    .max(30.)
            };
            if history_h == 0. {
                context_quota_radius = quota_radius_in(slack);
            }
            let mut gauge_growth = 2. * (context_quota_radius - 30.);
            let mut loose = if history_h > 0. {
                0.
            } else {
                slack - gauge_growth
            };
            if history_h == 0. && spare > 0. && slack >= notes_floor && loose < notes_floor {
                // The circles grew into the notes line's room: they grow from what is left of it.
                context_quota_radius = quota_radius_in(slack - notes_floor);
                gauge_growth = 2. * (context_quota_radius - 30.);
                loose = slack - gauge_growth;
            }
            let notes_room = if history_h > 0. {
                notes_drawn
            } else {
                loose >= spare + S2
            };
            notes_drawn = notes_drawn && notes_room;
            let group_top = title_bottom
                + if history_h > 0. {
                    0.
                } else {
                    (loose - if notes_drawn { spare } else { 0. }) / 2.
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
        ((h - (P + theme::TITLE_LINE + S2 + 46. + theme::CAPTION_LINE + notes_h(g) + P)) / 2.)
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
        let sat = y + h - 84. - notes_gap;
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
        (y + h - 84. - notes_gap, y + h - 120. - notes_gap)
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
        has_notes(g) && notes_drawn,
        t,
    );
    Some((ceiling_y, ring_bounds, ring_group, ring_formula_avail, None))
}

// ---------------------------------------------------------------------------------------------
// The Context card as a dashboard that reads in words, top to bottom: one headline sentence for
// where the limits are headed, the 5 Hour and Weekly bars with a tick at their forecast, the
// context with a labelled bar and a row per part, what CSR did this session, then the fresh
// tokens per turn. The older ring layouts stay for a card too short to hold it.
// ---------------------------------------------------------------------------------------------
const DASH_HEAD_LINE: f32 = theme::TITLE_LINE;
const DASH_ROW: f32 = 14.;
/// The Context card's rows are drawn at the rows' scale (`theme::ROW_*`): its row pitch, its row
/// names and its lead lines, and a name's line.
fn drow() -> f32 {
    theme::row_caption_line()
}
fn dash_name() -> f32 {
    theme::pick(15.0, 11.5)
}
fn dash_lead() -> f32 {
    theme::pick(14.5, 11.0)
}
/// A lead row's height: a compact "Self-Reflect this session" line.
fn dash_lead_line() -> f32 {
    theme::pick(25., 20.)
}
fn dash_name_line() -> f32 {
    theme::pick(20.0, 16.0)
}
const DASH_CHART_MIN: f32 = 64.;
/// The height the fresh-tokens chart is given when the card is expanded and has room.
const DASH_CHART_WANT: f32 = 92.;
/// The context parts' segmented bar: its height and the gap between two segments.
const PARTS_BAR_H: f32 = 8.;
const PARTS_GAP: f32 = 2.;
/// The radius of a legend's filled circle.
const LEGEND_DOT_R: f32 = 3.5;
/// A context part's name in the one-line legend.
fn part_short_name(name: &str) -> &str {
    match name {
        "Tool results" => "Tools",
        "CSR injected" => "CSR",
        other => other,
    }
}
/// The session mix the Context card's donut shows, from the breakdown's parts alone (no new
/// read): Tools = tool results; Context = messages and system text (`breakdown.rs` counts tool
/// calls' input into the message text, so they land here); Other = the rest (CSR injected).
/// Shares are of the three together. The verdict names a slice holding at least
/// `MIX_HEAVY` (half) of it, else "mixed".
pub fn session_mix(parts: &[(&'static str, f64, Rgba)]) -> Option<([f64; 3], &'static str)> {
    let mut mix = [0f64; 3];
    for (name, n, _) in parts {
        let slot = match *name {
            "Tool results" => 0,
            "Setup" | "Summary + setup" | "Messages" => 1,
            "In use" => return None,
            _ => 2,
        };
        mix[slot] += n.max(0.);
    }
    let total: f64 = mix.iter().sum();
    if total <= 0. {
        return None;
    }
    let shares = mix.map(|n| n / total);
    let verdict = if shares[0] >= MIX_HEAVY {
        "tool-heavy"
    } else if shares[1] >= MIX_HEAVY {
        "context-heavy"
    } else {
        "mixed"
    };
    Some((shares, verdict))
}
/// `session_mix` for a session whose three sizes are known directly (a transcript's bytes, not
/// the live breakdown's parts): the same shares and the same verdict words and threshold.
pub fn session_mix_of(tools: f64, context: f64, other: f64) -> Option<([f64; 3], &'static str)> {
    let ink = [0u8; 4];
    session_mix(&[
        ("Tool results", tools, ink),
        ("Messages", context, ink),
        ("CSR injected", other, ink),
    ])
}
/// The session-mix donut at (`cx`, `cy`): a track and one arc per slice (Tools, Context, Other)
/// with a hairline gap between slices, as between the parts bar's segments.
pub(crate) fn draw_mix_donut(c: &mut Canvas, cx: f32, cy: f32, shares: &[f64; 3], t: &Tokens) {
    draw_mix_donut_sized(c, cx, cy, MIX_R, MIX_W, shares, t);
}
/// [`draw_mix_donut`] at another size: ring radius `r` and stroke `w`, in pt. The slice inks
/// (Tools, Context, Other) are the Context card's.
pub(crate) fn draw_mix_donut_sized(
    c: &mut Canvas,
    cx: f32,
    cy: f32,
    r: f32,
    w: f32,
    shares: &[f64; 3],
    t: &Tokens,
) {
    c.arc(cx, cy, r, 0., 1., w, t.track, false);
    let mut from = 0f32;
    for (share, ink) in shares.iter().zip(mix_inks(t)) {
        let to = from + *share as f32;
        // a hairline gap between slices, as between the parts bar's segments
        c.arc(cx, cy, r, from, (to - 0.012).max(from), w, ink, false);
        from = to;
    }
}
/// The slices' inks: Tools, Context, Other.
pub(crate) fn mix_inks(t: &Tokens) -> [Rgba; 3] {
    [t.asked, t.sky, t.csr_share]
}
/// A slice is "heavy" at half the session or more.
const MIX_HEAVY: f64 = 0.5;
/// The session-mix donut on the parts bar's line: its radius and stroke, in pt. Its outer edge
/// (radius + half the stroke = 7 pt) stays inside the S1 gap above the bar and the S2 gap below.
pub(crate) const MIX_R: f32 = 5.25;
pub(crate) const MIX_W: f32 = 3.5;
/// The parts on each line of the legend: one line, two only when one cannot hold them.
fn legend_lines(f: &Fonts, parts: &[(&'static str, f64, Rgba)], t: &Tokens) -> Vec<Vec<usize>> {
    let caption = sans(theme::row_caption_size(), t.regular);
    let number = sans(theme::row_caption_size(), t.medium);
    let mut lines: Vec<Vec<usize>> = vec![];
    let mut width = 0.;
    for (i, (name, n, _)) in parts.iter().enumerate() {
        let w = 2. * LEGEND_DOT_R
            + S1
            + f.width(part_short_name(name), caption)
            + S1
            + f.width(&short(*n), number);
        match lines.last_mut() {
            Some(line) if width + S3 + w <= IW => {
                line.push(i);
                width += S3 + w;
            }
            _ => {
                lines.push(vec![i]);
                width = w;
            }
        }
    }
    lines
}

/// The words and numbers the dashboard draws, from the composition alone.
/// The CSR performance card's lines, fitted, and its height.
struct SoakPlan {
    head: Vec<String>,
    h: f32,
    /// Whether the clock line and its progress bar are drawn.
    clock: bool,
    /// 2: every check row. 1 and 0: only a failing check; 0 also drops the clock and bar.
    level: u8,
    /// The check rows drawn, and how many `level` has: fewer drawn ends in a "+N more" row.
    shown: usize,
    total: usize,
    /// Whether the road line is drawn under the checks.
    road: bool,
}
/// The height of the progress bar under the card's clock line.
const SOAK_BAR_H: f32 = 6.;

fn soak_plan(
    f: &Fonts,
    card: &crate::companion::live_eval::Card,
    t: &Tokens,
    level: u8,
    max_rows: usize,
) -> SoakPlan {
    let head: Vec<String> = f
        .wrap(&card.line1, heading(t), IW)
        .into_iter()
        .take(2)
        .collect();
    let clock = level >= 1 && !card.line2.is_empty();
    let total = soak_rows(card, level).count();
    let shown = total.min(max_rows.max(1));
    let road = level >= 1 && card.road.is_some();
    let rows = shown + usize::from(card.warning.is_some()) + usize::from(road);
    let h = P
        + theme::TITLE_LINE
        + S2
        + head.len() as f32 * DASH_HEAD_LINE
        + if clock {
            S1 + theme::CAPTION_LINE + S1 + SOAK_BAR_H
        } else {
            0.
        }
        + if rows > 0 { S2 } else { 0. }
        + rows as f32 * DASH_ROW
        + if road { S1 } else { 0. }
        + P;
    SoakPlan {
        head,
        h,
        clock,
        level,
        shown,
        total,
        road,
    }
}
/// The check rows a card shows at `level`: all of them at 2, only the failing ones below.
fn soak_rows(
    card: &crate::companion::live_eval::Card,
    level: u8,
) -> impl Iterator<Item = &crate::companion::live_eval::Row> {
    card.rows
        .iter()
        .filter(move |row| level >= 2 || row.sev == "red")
}

/// The trial label `"<words> <sha>"` fitted to `room` pt: unchanged when it fits, else its words
/// are cut with an ellipsis and the trailing short sha stays whole. `None` when not even the sha
/// fits.
fn fit_candidate(f: &Fonts, candidate: &str, st: St, room: f32) -> Option<String> {
    if candidate.is_empty() || room <= 0. {
        return None;
    }
    if f.width(candidate, st) <= room {
        return Some(candidate.to_owned());
    }
    let Some((words, sha)) = candidate
        .rsplit_once(' ')
        .filter(|(_, s)| s.len() >= 7 && s.chars().all(|ch| ch.is_ascii_hexdigit()))
    else {
        // No sha to keep: cut the label itself.
        let shown = f.fit(candidate, st, room);
        return (shown != "\u{2026}").then_some(shown);
    };
    if f.width(sha, st) > room {
        return None;
    }
    let tail = format!(" {sha}");
    let cut = f.fit(words, st, room - f.width(&tail, st));
    Some(
        if cut == "\u{2026}" && f.width(&format!("\u{2026}{tail}"), st) > room {
            sha.to_owned()
        } else {
            format!("{cut}{tail}")
        },
    )
}

/// The CSR performance card at `y`: its heading, the big number and headline, the clock with its
/// progress bar, then a line to each check and the stale-data warning.
fn draw_soak(
    c: &mut Canvas,
    f: &Fonts,
    card: &crate::companion::live_eval::Card,
    plan: &SoakPlan,
    y: f32,
    t: &Tokens,
) {
    panel(c, y, plan.h, t);
    let caption = sans(theme::CAPTION_SIZE, t.regular);
    let ink = dash_sev_ink(&card.sev, t);
    label(
        c,
        f,
        &card.title,
        IX,
        y + P + f.cap(heading(t)),
        theme::TITLE_SIZE,
        t.ink,
        t.bold,
    );
    // The trial label sits right of the title, in what room the title leaves. A long label loses
    // words from its end, never its trailing short sha; with no room at all it is dropped.
    let room = IW - f.width(&icons::line(&card.title), heading(t)) - S3;
    if let Some(shown) = fit_candidate(f, &card.candidate, caption, room) {
        c.text(
            f,
            &shown,
            IX + IW,
            y + P + f.cap(caption),
            caption,
            t.quiet,
            A::R,
        );
    }
    let mut at = y + P + theme::TITLE_LINE + S2;
    let st = heading(t);
    for (i, line) in plan.head.iter().enumerate() {
        let shown = if i + 1 == plan.head.len() && plan.head.len() == 2 {
            f.fit(line, st, IW)
        } else {
            line.clone()
        };
        c.text(f, &shown, IX, at + f.cap(st), st, ink, A::L);
        at += DASH_HEAD_LINE;
    }
    if plan.clock {
        at += S1;
        single(
            c,
            f,
            &card.line2,
            IX,
            at + f.cap(caption),
            IW,
            theme::CAPTION_SIZE,
            t.quiet,
            t,
        );
        at += theme::CAPTION_LINE + S1;
        c.rrect(IX, at, IW, SOAK_BAR_H, SOAK_BAR_H / 2., t.track);
        if card.progress > 0. {
            let w = (IW * card.progress as f32).clamp(SOAK_BAR_H, IW);
            c.rrect(IX, at, w, SOAK_BAR_H, SOAK_BAR_H / 2., ink);
        }
        at += SOAK_BAR_H;
    }
    if plan.shown > 0 || card.warning.is_some() || plan.road {
        at += S2;
    }
    let row_st = sans(11., t.regular);
    let cut = plan.shown < plan.total;
    let kept = if cut { plan.shown - 1 } else { plan.shown };
    for row in soak_rows(card, plan.level).take(kept) {
        let mark = match row.sev.as_str() {
            "ok" => t.mint,
            "red" => t.risk,
            _ => t.quiet,
        };
        let base = at + f.cap(row_st);
        c.text(f, &row.mark, IX, base, sans(11., t.bold), mark, A::L);
        single(
            c,
            f,
            &row.text,
            IX + 16.,
            base,
            IW - 16.,
            11.,
            if row.sev == "quiet" { t.quiet } else { t.ink },
            t,
        );
        at += DASH_ROW;
    }
    if cut {
        c.text(
            f,
            &more_line(plan.total - kept),
            IX + 16.,
            at + f.cap(row_st),
            row_st,
            t.quiet,
            A::L,
        );
        at += DASH_ROW;
    }
    if let Some(road) = card.road.as_ref().filter(|_| plan.road) {
        // A hairline in the air above, so the road reads as its own track and not as one more check.
        c.rect(IX, at + S1 / 2., IW, t.hair_width, t.hair);
        at += S1;
        let mark = match road.sev.as_str() {
            "ok" => t.mint,
            "red" => t.risk,
            _ => t.quiet,
        };
        let base = at + f.cap(row_st);
        c.text(f, &road.mark, IX, base, sans(11., t.bold), mark, A::L);
        single(c, f, &road.text, IX + 16., base, IW - 16., 11., t.ink, t);
        at += DASH_ROW;
    }
    if let Some(warning) = &card.warning {
        c.icon(
            f,
            Icon::CircleAlert,
            IX,
            at + f.cap(caption) + 1.,
            12.,
            10.,
            t.amber,
        );
        single(
            c,
            f,
            warning,
            IX + 18.,
            at + f.cap(caption) + 1.,
            IW - 18.,
            theme::CAPTION_SIZE,
            t.amber,
            t,
        );
    }
}

struct DashPlan {
    head: Vec<String>,
    head_sev: String,
    /// The API credit row, wrapped, and whether it warns.
    credit: Vec<String>,
    credit_warns: bool,
    /// Every quota window, as the limits chart draws it (limits_chart.rs).
    limits: Vec<crate::companion::contract::Limit>,
    ctx: Option<(f64, f64, &'static str)>,
    /// Claude Code's auto-compact buffer at the window's end, when its window is known.
    buffer: Option<f64>,
    parts: Vec<(&'static str, f64, Rgba)>,
    /// The parts' one-line legend: the parts on each line, one line unless it cannot fit.
    legend: Vec<Vec<usize>>,
    estimated: bool,
    turn: Option<String>,
    cache: Option<(String, bool)>,
    csr: Vec<CsrRing>,
    /// The most lines any ring's label wraps to.
    csr_label_lines: usize,
    /// "Self-Reflect this session" as one line of words, for a pane too short for its rings.
    csr_compact: bool,
    /// Drop the Context section's "this turn" note and cache line.
    lean: bool,
    /// Drop "Self-Reflect this session" as well.
    bare: bool,
    status: Option<(String, bool)>,
    /// What the session started with and what CSR handed it, while the session is new.
    start: Option<StartPlan>,
    /// After a compact (or past the first replies) the start block is gone, but CSR's text is
    /// not: "CSR handed over: <clause>", above the Self-Reflect row.
    told_row: Option<Handed>,
    /// CSR told this session something: the Self-Reflect rows open what it said.
    has_handed: bool,
}

/// What a new session's Context card tells: what it started with, from real data only, and what
/// CSR handed it. A part nobody measured is named "unmeasured", never sized.
struct StartPlan {
    /// The card has a live context reading (a reply, or the status line's): its Context section
    /// is drawn above and the start is one line under it.
    live: bool,
    /// The first reply's total: the one real reading of the start. None: not read yet.
    reading: Option<f64>,
    /// The measured parts (tokens estimated from characters), in the order drawn.
    parts: Vec<(&'static str, f64, Rgba)>,
    /// The names of the parts nobody measured.
    unmeasured: Vec<&'static str>,
    /// With a reading: what the measured parts do not explain.
    rest: Option<f64>,
    /// The parts' one-line legend, as the live card's.
    legend: Vec<Vec<usize>>,
    /// The session mix of the measured parts: shares and the verdict word.
    mix: Option<([f64; 3], &'static str)>,
    /// What CSR handed this session: a row each, and the Lost and found thread a row opens.
    handed: Vec<Handed>,
    /// The window the bar spans, when it is known: the same scale the live section uses.
    window: Option<f64>,
}
/// One row of what CSR handed a session: its lead words, its text, an extra that is dropped when
/// the row is too narrow for it, and the Lost and found thread it opens.
struct Handed {
    lead: &'static str,
    text: String,
    tail: String,
    thread: Option<Value>,
    /// A click on the row opens what CSR told Claude.
    opens_handed: bool,
}
/// The height of a handed-over row.
fn handed_row() -> f32 {
    theme::pick(25.0, 20.0)
}
impl StartPlan {
    /// Whether the start has a block of its own (the card has no reading yet, so it is the
    /// Context section); with a live reading that section is above and the start is one line.
    fn own_block(&self) -> bool {
        !self.live
    }
    /// The block's height. `compact` drops the note under the legend.
    fn height(&self, compact: bool) -> f32 {
        let block = if self.own_block() {
            S2 + dash_name_line()
                + S1
                + PARTS_BAR_H
                + S2
                + self.legend.len() as f32 * drow()
                + if compact { 0. } else { drow() }
                + S2
        } else {
            drow()
        };
        // The rows end in a gap, unless the block's own bottom gap is the only one needed.
        block
            + self.handed.len() as f32 * handed_row()
            + if self.handed.is_empty() && self.own_block() {
                0.
            } else {
                S3
            }
    }
    /// The note under the legend: sizes are estimates, and what nobody measured.
    fn note(&self) -> String {
        if self.unmeasured.is_empty() {
            "Part sizes est. from the transcript's text.".to_owned()
        } else if self.parts.is_empty() {
            format!("Nothing measured yet: {}", self.unmeasured.join(", "))
        } else {
            format!("est. · unmeasured: {}", self.unmeasured.join(", "))
        }
    }
    /// The one line of a session that has its reading: where it started and what was in it.
    fn started_line(&self) -> String {
        let mut bits: Vec<String> = self
            .parts
            .iter()
            .map(|(name, t, _)| format!("{} {}", part_short_name(name), short(*t)))
            .collect();
        if let Some(rest) = self.rest {
            bits.push(format!("rest {}", short(rest)));
        }
        let unmeasured = if self.unmeasured.is_empty() {
            String::new()
        } else {
            format!(" · unmeasured: {}", self.unmeasured.join(", "))
        };
        format!(
            "{}: {}{unmeasured} (est.)",
            match self.reading {
                Some(n) => format!("Started at {}", short(n)),
                None => "Started with".to_owned(),
            },
            if bits.is_empty() {
                "no part measured".to_owned()
            } else {
                bits.join(" · ")
            }
        )
    }
}

/// "CSR handed over: <clause>" for a session whose start block is gone: the clause of the recap
/// (or of the compact receipt) CSR's text carries.
fn told_row(c: &Composition) -> Option<Handed> {
    let item = c.handed.first()?;
    let text = item["text"].as_str().unwrap_or("");
    let clause = if item["summary_only"] == true {
        Some(text.to_owned()).filter(|t| !t.is_empty())
    } else {
        crate::companion::start::recap_clause(text)
    }?;
    Some(Handed {
        lead: "CSR handed over",
        text: clause,
        tail: String::new(),
        thread: None,
        opens_handed: true,
    })
}

/// The start of a new session for the Context card: from the figure's `start` facts and the
/// Lost and found threads of the river. None when the session is past its first replies.
fn start_plan(f: &Fonts, c: &Composition, t: &Tokens) -> Option<StartPlan> {
    let st = c.figure["start"].as_object()?;
    let start = &c.figure["start"];
    let reading = st
        .get("reading")
        .and_then(Value::as_f64)
        .filter(|n| *n > 0.);
    let mut parts: Vec<(&'static str, f64, Rgba)> = vec![];
    let mut unmeasured: Vec<&'static str> = vec![];
    for part in array(&start["parts"]) {
        let (name, ink): (&'static str, Rgba) = match string(&part["key"]) {
            "csr" => ("CSR injected", t.csr_share),
            "memory" => ("Memory", t.sky),
            "skills" => ("Skills", t.mint),
            "system" => ("System prompt", t.faint),
            _ => continue,
        };
        match part["tokens"].as_f64() {
            Some(n) if n > 0. => parts.push((name, n, ink)),
            Some(_) => {}
            None => unmeasured.push(name),
        }
    }
    let rest = start["rest"].as_f64().filter(|n| *n > 0.);
    // The donut reads the measured parts as the live card reads its breakdown's: the text CSR
    // injected is Other, everything else is Context.
    let mixed: Vec<(&'static str, f64, Rgba)> = parts
        .iter()
        .map(|(name, n, ink)| {
            (
                if *name == "CSR injected" {
                    *name
                } else {
                    "Messages"
                },
                *n,
                *ink,
            )
        })
        .collect();
    let mix = session_mix(&mixed);
    let legend = legend_lines(f, &parts, t);
    let mut handed = vec![];
    if let Some(clause) = start["recap"].as_str().filter(|s| !s.is_empty()) {
        let episodes = start["episodes"].as_u64().unwrap_or(0);
        let tail = if episodes > 0 {
            format!(" · {episodes} earlier threads indexed")
        } else {
            String::new()
        };
        handed.push(Handed {
            lead: "CSR handed over",
            text: clause.to_owned(),
            tail,
            thread: None,
            opens_handed: !c.handed.is_empty(),
        });
    }
    // The Lost and found card on the main view says the threads: the start block does not say
    // them twice.
    let threads = if c.open_lost.is_empty() {
        array(&start["threads"])
    } else {
        &[][..]
    };
    if let Some(first) = threads.first() {
        handed.push(Handed {
            lead: "Lost and found",
            text: format!("{} open · {}", threads.len(), string(&first["text"])),
            tail: String::new(),
            thread: Some(first.clone()),
            opens_handed: false,
        });
    }
    Some(StartPlan {
        live: false,
        reading,
        parts,
        unmeasured,
        rest,
        legend,
        mix,
        handed,
        window: None,
    })
}

/// One choice of what the dashboard keeps, richest first in `FIT_STEPS`.
#[derive(Clone, Copy)]
struct FitStep {
    /// The performance card's level: 2 every check row, 1 only a failing one, 0 no clock or bar.
    soak: u8,
    /// Most rows a list keeps.
    cap: usize,
    /// Keep the API credit row.
    credit: bool,
    /// "Self-Reflect this session" as one line of words instead of rings.
    compact: bool,
    /// The Context card without its "this turn" note and its cache line: the last room a short
    /// pane has once every card has folded.
    lean: bool,
    /// The Context card also without its "Self-Reflect this session" row: the last room.
    bare: bool,
    /// The stackable cards folded into the stack row (`FOLD_*` bits).
    fold: u8,
    /// Most story rows and change rows the story card keeps (the accordion cuts them).
    story_lines: usize,
    story_shifts: usize,
    /// Most check rows the performance card keeps (the accordion cuts them, "+N more" last).
    soak_rows: usize,
    /// An open section squeezes the Context card to its floor.
    squeeze: bool,
    /// An open section is cut to its least rows.
    least: bool,
}
const fn step(
    soak: u8,
    cap: usize,
    credit: bool,
    compact: bool,
    fold: u8,
    squeeze: bool,
    least: bool,
) -> FitStep {
    FitStep {
        soak,
        cap,
        credit,
        compact,
        lean: false,
        bare: false,
        fold,
        story_lines: usize::MAX,
        story_shifts: usize::MAX,
        soak_rows: usize::MAX,
        squeeze,
        least,
    }
}
/// The cards a stack row can hold, in the order it lists them: the bit that folds each, and its
/// name in the row.
pub const STACKABLE: [(u8, &str); 4] = [
    (FOLD_CONFUSED, "What confused the agent"),
    (FOLD_SHOULD, "Should be"),
    (FOLD_STORY, "Story"),
    (FOLD_PERF, "Performance"),
];
const FOLD_CONFUSED: u8 = 1;
const FOLD_SHOULD: u8 = 2;
/// Not a fold into the stack: the Lost and found card is dropped from a step that sets this bit.
const FOLD_LOST: u8 = 16;
const FOLD_LISTS: u8 = FOLD_CONFUSED | FOLD_SHOULD;
const FOLD_STORY: u8 = 4;
const FOLD_PERF: u8 = 8;
const FOLD_ALL: u8 = FOLD_LISTS | FOLD_STORY | FOLD_PERF | FOLD_LOST;
/// The stack row's height, its two sheet edges included, in layout pt.
pub const STACK_H: f32 = 38.;
/// What a dashboard keeps, most first: the order content goes as the pane gets shorter. The
/// doc of the fit in `draw` names each step.
/// `step` with the Context card's lean layout.
const fn lean(mut s: FitStep) -> FitStep {
    s.lean = true;
    s
}
/// `lean` and without the Context card's "Self-Reflect this session" row.
const fn bare(mut s: FitStep) -> FitStep {
    s.lean = true;
    s.bare = true;
    s
}
impl FitStep {
    /// 0 the full Context card, 1 lean, 2 bare.
    fn level(&self) -> u8 {
        u8::from(self.lean) + u8::from(self.bare)
    }
}
/// The steps in the order content goes. `FIT_STEPS` first tries the first seven with the Lost and
/// found card, down to the Context card's compact rings, so the card outlasts the lists' extra
/// rows, the credit row and the full rings. Then it runs every step without the card, with one
/// more try with the card at the Context card's lean layout. A session with no open threads lays
/// out as it did before the card existed.
const BASE_STEPS: [FitStep; 14] = [
    step(2, usize::MAX, true, false, 0, false, false),
    step(1, usize::MAX, true, false, 0, false, false),
    step(0, usize::MAX, true, false, 0, false, false),
    step(0, 3, true, false, 0, false, false),
    step(0, 1, true, false, 0, false, false),
    step(0, 1, false, false, 0, false, false),
    step(0, 1, false, true, 0, false, false),
    lean(step(0, 1, false, true, 0, false, false)),
    lean(step(0, 1, false, true, FOLD_LISTS, false, false)),
    lean(step(
        0,
        1,
        false,
        true,
        FOLD_LISTS | FOLD_STORY,
        false,
        false,
    )),
    lean(step(0, 1, false, true, FOLD_ALL, false, false)),
    bare(step(0, 1, false, true, FOLD_ALL, false, false)),
    bare(step(0, 1, false, true, FOLD_ALL, true, false)),
    bare(step(0, 1, false, true, FOLD_ALL, true, true)),
];
/// How many of `BASE_STEPS` draw the Lost and found card before any step goes without it.
const STEPS_LOST_FIRST: usize = 7;
const FIT_STEPS: [FitStep; STEPS_LOST_FIRST + 1 + BASE_STEPS.len()] = with_lost_first();
const fn with_lost_first() -> [FitStep; STEPS_LOST_FIRST + 1 + BASE_STEPS.len()] {
    let mut out = [BASE_STEPS[0]; STEPS_LOST_FIRST + 1 + BASE_STEPS.len()];
    let (mut i, mut n) = (0, 0);
    while i < STEPS_LOST_FIRST {
        out[n] = BASE_STEPS[i];
        n += 1;
        i += 1;
    }
    let mut i = 0;
    while i < BASE_STEPS.len() {
        if i == STEPS_LOST_FIRST {
            out[n] = BASE_STEPS[i];
            n += 1;
        }
        let mut s = BASE_STEPS[i];
        s.fold |= FOLD_LOST;
        out[n] = s;
        n += 1;
        i += 1;
    }
    out
}

/// What the layout dropped from the dashboard to keep the text size.
#[derive(Clone, Copy)]
struct DashFit {
    credit: bool,
    compact_rings: bool,
    lean: bool,
    bare: bool,
}
impl DashPlan {
    fn fit(&mut self, fit: DashFit) {
        if !fit.credit {
            self.credit.clear();
        }
        self.csr_compact = fit.compact_rings;
        self.lean = fit.lean;
        self.bare = fit.bare;
    }
}

/// One ring of "Self-Reflect this session": the number in it, the words under it, and the fraction of the
/// ring it fills when that fraction means something. A count has none: it is a full ring.
#[derive(Clone, Debug)]
struct CsrRing {
    number: String,
    label: String,
    fill: Option<f64>,
}
/// The radius of a "Self-Reflect this session" ring, in layout points.
const CSR_RING_R: f32 = 24.;

/// The API credit row as one-line items: its parts (cut at " · ") packed into lines that each
/// fit `w`, a part never split across two lines. At most `max` lines; a part still wider than
/// the line ends in "…", and so does the last line when parts were left out.
fn credit_lines(f: &Fonts, row: &str, st: St, w: f32, max: usize) -> Vec<String> {
    let parts: Vec<&str> = row.split(" · ").collect();
    let last = parts.len().saturating_sub(1);
    let tokens: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if i < last {
                format!("{p} ·")
            } else {
                (*p).to_owned()
            }
        })
        .collect();
    let mut lines: Vec<String> = vec![];
    let mut cur = String::new();
    for token in tokens {
        let joined = if cur.is_empty() {
            token.clone()
        } else {
            format!("{cur} {token}")
        };
        if f.width(&joined, st) <= w || cur.is_empty() {
            cur = joined;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = token;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    let cut = lines.len() > max;
    lines.truncate(max);
    if cut {
        if let Some(l) = lines.last_mut() {
            l.push('…');
        }
    }
    lines.iter().map(|l| f.fit(l, st, w)).collect()
}

fn dash_sev_ink(sev: &str, t: &Tokens) -> Rgba {
    match sev {
        "red" => t.risk,
        "amber" => t.amber,
        "quiet" => t.quiet,
        _ => t.mint,
    }
}

fn dash_plan(f: &Fonts, c: &Composition, t: &Tokens) -> Option<DashPlan> {
    let g = &c.figure;
    // A figure can ask for the older ring layouts, which the checks of their geometry draw.
    if g["layout"] == "rings" {
        return None;
    }
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    let model_size = g["ctx"]["size"].as_f64().filter(|n| *n > 0.);
    // No reading yet: the dashboard is for a new session that tells its start, else the older
    // layout (the quotas alone) stays.
    let mut start = start_plan(f, c, t);
    if tokens.is_none() && start.is_none() || tokens.is_some() && model_size.is_none() {
        return None;
    }
    if let Some(start) = start.as_mut() {
        start.live = tokens.is_some();
    }
    // The bar spans the window Claude Code compacts to (`autoCompactWindow` or its env var, as
    // the compactor reads it), as /context does; its last NATIVE_UNDER is the auto-compact
    // buffer. With none set, the model's own window and no buffer.
    // Any positive window is the one configured (as `native_window_of` accepts it); a window of
    // 33k or less is all buffer.
    let native = g["ctx"]["window"].as_f64().filter(|w| *w > 0.);
    let size = native.or(model_size);
    if let Some(start) = start.as_mut() {
        start.window = size;
    }
    let buffer = native.map(|w| crate::companion::app::compactor::rules::NATIVE_UNDER.min(w));
    // The headline is worded once (contract.rs), for this pane and any other.
    let credit_head = c.credit.as_ref().and_then(|credit| credit.headline());
    let (head, head_sev) = match crate::companion::contract::headline(c) {
        Some((text, sev)) => (
            f.wrap(&text, dash_heading(t), IW)
                .into_iter()
                .take(2)
                .collect(),
            sev,
        ),
        None => (vec![], String::new()),
    };
    let credit = c
        .credit
        .as_ref()
        .map(|credit| {
            credit_lines(
                f,
                &credit.row(),
                sans(theme::row_caption_size(), t.regular),
                IW,
                3,
            )
        })
        .unwrap_or_default();
    // The windows (a figure with no outlook still shows its windows, with no pace), the Codex
    // week and agy's are worded once (contract.rs), for this pane and any other.
    let limits = crate::companion::contract::limits(c);
    let ctx_state = match string(&g["ctx"]["sev"]) {
        "red" => "red",
        "amber" => "amber",
        _ => "ok",
    };
    // The parts are named once (contract.rs); each ink maps to this theme's colour.
    let (named, estimated) = crate::companion::contract::context_parts(c);
    let parts: Vec<(&'static str, f64, Rgba)> = named
        .into_iter()
        .map(|(name, n, ink)| {
            let colour = match ink {
                "setup" => t.faint,
                "messages" => t.sky,
                "tools" => t.asked,
                "csr" => t.csr_share,
                _ => t.mint,
            };
            (name, n, colour)
        })
        .collect();
    let turn = breakdown_of(g)
        .and_then(|b| b["caption"].as_str())
        .map(str::to_owned);
    // The prompt cache as the status line measured it: warm with its time left, or cold with what
    // re-reading would cost. No claim that CSR kept it warm: no feed says so.
    let cache = g["cache"]["long"]
        .as_str()
        .filter(|l| *l != "--" && !l.is_empty())
        .map(|l| {
            let words = l.replacen("WARM", "warm", 1).replacen("COLD", "cold", 1);
            (format!("Cache {words}"), g["cache"]["cold"] == true)
        });
    // Only what was measured gets a ring. A count has no fraction of anything: a full ring. The
    // words are the contract's (contract.rs), the same for every pane.
    let csr: Vec<CsrRing> = crate::companion::contract::csr_parts(c)
        .into_iter()
        .map(|(number, label, fill)| CsrRing {
            number,
            label,
            fill,
        })
        .collect();
    let slot = IW / csr.len().max(1) as f32 - S2;
    let csr_label_lines = csr
        .iter()
        .map(|r| {
            f.wrap(
                &r.label,
                sans(theme::row_caption_size(), t.medium),
                slot - SWATCH_LEAD,
            )
            .len()
            .min(2)
        })
        .max()
        .unwrap_or(0);
    let status = csr_status(g).map(|(line, sev)| (line, sev == "red"));
    let legend = legend_lines(f, &parts, t);
    Some(DashPlan {
        head,
        head_sev,
        credit,
        credit_warns: credit_head.is_some(),
        limits,
        ctx: tokens
            .zip(size)
            .map(|(tokens, size)| (tokens, size, ctx_state)),
        buffer,
        parts,
        legend,
        estimated,
        turn,
        cache,
        csr,
        csr_label_lines,
        csr_compact: false,
        lean: false,
        bare: false,
        status,
        told_row: start.is_none().then(|| told_row(c)).flatten(),
        has_handed: !c.handed.is_empty(),
        start,
    })
}

/// The heights of the dashboard's sections: (headline, limits, context, CSR) and their sum with
/// the card's padding. `full` lists the parts one to a row; the compact layout sets two to a row.
fn dash_heights(p: &DashPlan, full: bool) -> (f32, f32, f32, f32, f32) {
    dash_heights_with(
        p,
        full,
        true,
        p.csr_compact,
        u8::from(p.lean) + u8::from(p.bare),
    )
}
/// `dash_heights` with the API credit row and the rings of "Self-Reflect this session" kept or dropped.
fn dash_heights_with(
    p: &DashPlan,
    full: bool,
    credit: bool,
    compact: bool,
    level: u8,
) -> (f32, f32, f32, f32, f32) {
    let (lean, bare) = (level > 0, level > 1);
    let head = if p.head.is_empty() {
        0.
    } else {
        p.head.len().min(if lean { 1 } else { 2 }) as f32 * theme::row_title_line() + S2
    };
    let limits = limits_chart::height(&p.limits, lean)
        + if p.credit.is_empty() || !credit {
            0.
        } else {
            p.credit.len() as f32 * drow() + S1
        };
    let _ = full;
    let rows = if lean { 0. } else { p.legend.len() as f32 };
    let note = !lean && (p.turn.is_some() || (p.estimated && p.turn.is_none()));
    // As `draw_dashboard` draws it: the gap, the heading, the bar, the legend lines, the notes.
    let ctx = if p.ctx.is_some() {
        S2 + dash_name_line()
            + S1
            + PARTS_BAR_H
            + S2
            + rows * drow()
            + if note { drow() } else { 0. }
            + if p.cache.is_some() && !lean {
                drow()
            } else {
                0.
            }
            + S2
    } else {
        0.
    };
    // The start of a new session: its own Context block (with the cache line the live block
    // would hold) or one line under the live one, and what CSR handed over.
    let ctx = ctx
        + p.start.as_ref().map_or(0., |start| {
            start.height(compact)
                + if start.own_block() && p.cache.is_some() {
                    drow()
                } else {
                    0.
                }
        })
        // The row of CSR's text after a compact: the first thing the Context card gives up
        // (with its note and cache line), before the Self-Reflect row.
        + if p.start.is_none() && p.told_row.is_some() && !lean {
            handed_row()
        } else {
            0.
        };
    let csr = if p.csr.is_empty() || bare {
        0.
    } else if compact {
        dash_lead_line()
    } else {
        theme::row_title_line()
            + S2
            + 2. * CSR_RING_R
            + S2
            + p.csr_label_lines.max(1) as f32 * theme::row_caption_line()
            + S2
    } + if p.status.is_some() { drow() + S1 } else { 0. };
    (head, limits, ctx, csr, P + head + limits + ctx + csr + P)
}

fn dash_full_h(p: &DashPlan) -> f32 {
    dash_heights(p, true).4
}
fn dash_compact_h(p: &DashPlan) -> f32 {
    dash_heights(p, false).4
}

/// The session-mix donut and its verdict at the right end of a parts bar's line (no row of its
/// own), when there is a mix. Returns the width the bar then has.
fn draw_bar_mix(
    c: &mut Canvas,
    f: &Fonts,
    dash: &mut Dash,
    mix: Option<([f64; 3], &'static str)>,
    bar_y: f32,
    t: &Tokens,
) -> f32 {
    let caption = sans(theme::row_caption_size(), t.regular);
    match mix {
        Some((shares, verdict)) => {
            let vw = f.width(verdict, caption);
            let cx = IX + IW - vw - S1 - MIX_R - MIX_W / 2.;
            let cy = bar_y + PARTS_BAR_H / 2.;
            draw_mix_donut(c, cx, cy, &shares, t);
            c.text(
                f,
                verdict,
                IX + IW,
                cy + f.cap(caption) / 2.,
                caption,
                t.quiet,
                A::R,
            );
            dash.mix = Some((
                verdict.to_owned(),
                shares.map(|s| s as f32),
                (cx, cy, MIX_R + MIX_W / 2.),
            ));
            (cx - MIX_R - MIX_W / 2. - S3 - IX).max(IW / 2.)
        }
        None => IW,
    }
}
/// The parts' legend from `at` down: a small filled circle, the part's short name and size, the
/// parts of a line side by side. Returns each part's legend words (its index and text).
fn draw_legend(
    c: &mut Canvas,
    f: &Fonts,
    parts: &[(&'static str, f64, Rgba)],
    legend: &[Vec<usize>],
    at: f32,
    t: &Tokens,
) -> Vec<(usize, String)> {
    let caption = sans(theme::row_caption_size(), t.regular);
    let mut out = vec![];
    for (line, items) in legend.iter().enumerate() {
        let base = at + line as f32 * drow() + f.cap(caption) + 1.;
        let mut lx = IX;
        for &i in items {
            let (name, n, ink) = &parts[i];
            c.circle(
                lx + LEGEND_DOT_R,
                base - f.cap(caption) / 2.,
                LEGEND_DOT_R,
                *ink,
            );
            lx += 2. * LEGEND_DOT_R + S1;
            let short_name = part_short_name(name);
            lx += c.text(f, short_name, lx, base, caption, t.quiet, A::L) + S1;
            lx += c.text(
                f,
                &short(*n),
                lx,
                base,
                sans(theme::row_caption_size(), t.medium),
                t.ink,
                A::L,
            ) + S3;
            out.push((i, format!("{short_name} {}", short(*n))));
        }
    }
    out
}

/// Draw the dashboard in the card at `y` of height `h`. `chart` asks for the fresh-tokens chart
/// in what room is left under the rows.
/// One row of what CSR handed the session: a bar, its lead words and its text. A Lost and found
/// row opens the handoff popup of its thread, as the monologue's rows do; a "CSR handed over" row
/// opens what CSR told Claude.
fn draw_handed_row(c: &mut Canvas, f: &Fonts, dash: &mut Dash, t: &Tokens, row: &Handed, at: f32) {
    let st = sans(dash_lead(), t.medium);
    let lead_st = sans(dash_lead(), t.bold);
    let lead_text = format!("{}: ", row.lead);
    let lead_w = f.width(&lead_text, lead_st);
    let base = at + 2. + f.cap(st);
    let room = IW - 10. - lead_w;
    let whole = icons::line(&format!("{}{}", row.text, row.tail));
    let said = if f.width(&whole, st) <= room {
        whole
    } else {
        icons::line(&row.text)
    };
    c.rrect(IX, at, 3., theme::pick(20., 15.), 1.5, t.csr_share);
    c.text(f, &lead_text, IX + 10., base, lead_st, t.csr_share, A::L);
    let rest = f.fit(&said, st, room);
    c.text(f, &rest, IX + 10. + lead_w, base, st, t.ink, A::L);
    dash.handed.push(format!("{lead_text}{rest}"));
    if let Some(thread) = row.thread.as_ref().filter(|_| !c.dry) {
        c.cuts.push(Cut {
            y: base - 4.,
            size: 8.,
            title: "Lost and found",
            full: row.text.clone(),
            lost: Some(thread.clone()),
            handed: false,
        });
    } else if row.opens_handed && !c.dry {
        c.cuts.push(Cut {
            y: base - 4.,
            size: 8.,
            title: "CSR handed over",
            full: row.text.clone(),
            lost: None,
            handed: true,
        });
    }
}

fn draw_dashboard(
    c: &mut Canvas,
    f: &Fonts,
    data: &Composition,
    p: &DashPlan,
    y: f32,
    h: f32,
    chart: bool,
    t: &Tokens,
) {
    let full = h + 0.5 >= dash_full_h(p);
    let (head_h, limits_h, ctx_h, csr_h, total) = dash_heights(p, full);
    // A lean card cuts its headline to one line, but not when the card has room for the second:
    // a cut headline loses the words that say how it ends.
    let whole_head = p.lean && p.head.len() > 1 && h - total >= theme::row_title_line();
    let head_h = head_h
        + if whole_head {
            theme::row_title_line()
        } else {
            0.
        };
    let total = total
        + if whole_head {
            theme::row_title_line()
        } else {
            0.
        };
    let slack = (h - total).max(0.);
    // The spare room goes between the sections, never into one section's rows.
    let chart_room = if chart && fresh_bar_heights(&data.requests).is_some() {
        DASH_CHART_WANT
    } else {
        0.
    };
    let gap = ((slack - chart_room) / 3.).clamp(0., S4);
    let mut dash = Dash {
        full,
        ..Dash::default()
    };
    let mut at = y + P;
    // A. the headline
    if !p.head.is_empty() {
        let st = dash_heading(t);
        let ink = dash_sev_ink(&p.head_sev, t);
        let head: Vec<String> = if p.lean && p.head.len() > 1 && !whole_head {
            vec![f.fit(&p.head.join(" "), st, IW)]
        } else {
            p.head.clone()
        };
        for (i, line) in head.iter().enumerate() {
            let shown = if i + 1 == head.len() && head.len() == 2 {
                f.fit(line, st, IW)
            } else {
                line.clone()
            };
            c.text(
                f,
                &shown,
                IX,
                at + f.cap(st) + i as f32 * theme::row_title_line(),
                st,
                ink,
                A::L,
            );
        }
        dash.headline = Some((p.head.join(" "), p.head_sev.clone()));
        at += head_h + gap;
    }
    // B. the limits: every quota window as a column, grouped by whose it is (limits_chart.rs).
    let name_st = sans(dash_name(), t.medium);
    let caption = sans(theme::row_caption_size(), t.regular);
    at = limits_chart::draw(c, f, t, &p.limits, p.lean, at, &mut dash.limits);
    // The API credit, under the bars it sits with: a warning once it runs out within days.
    if !p.credit.is_empty() {
        let ink = if p.credit_warns { t.amber } else { t.quiet };
        for (i, line) in p.credit.iter().enumerate() {
            c.text(
                f,
                line,
                IX,
                at + f.cap(caption) + i as f32 * drow(),
                caption,
                ink,
                A::L,
            );
        }
        dash.credit_row = Some(p.credit.join(" "));
        at += p.credit.len() as f32 * drow() + S1;
    }
    at += gap;
    // C. the context
    if let Some((tokens, size, state)) = p.ctx {
        at += S2;
        let base = at + f.cap(name_st);
        c.text(f, "Context", IX, base, name_st, t.ink, A::L);
        // As /context says it: what is used of the window, and what is free short of the
        // auto-compact buffer.
        let used = format!(
            "{} of {} ({:.0}%)",
            short(tokens),
            short(size),
            (tokens / size * 100.).round()
        );
        let name_w = f.width("Context", name_st);
        c.text(
            f,
            &used,
            IX + name_w + S2,
            base,
            sans(dash_name(), t.bold),
            dash_sev_ink(state, t),
            A::L,
        );
        let buffer = p.buffer.unwrap_or(0.);
        let free = match p.buffer {
            Some(b) => format!(
                "{} free · {} buffer",
                short((size - tokens - b).max(0.)),
                short(b)
            ),
            None => format!("{} free", short((size - tokens).max(0.))),
        };
        c.text(f, &free, IX + IW, base, caption, t.quiet, A::R);
        dash.context_head = Some(format!("{used} · {free}"));
        at += dash_name_line();
        // One bar of the window cut into the parts, each its own segment with a gap between: no
        // pace tick and square ends, unlike the rounded limit bars above it.
        let bar_y = at + S1;
        // The session mix: a small donut and its one-word verdict at the bar's right end, on
        // the bar's own line (no row of its own).
        let mix = if p.estimated {
            session_mix(&p.parts)
        } else {
            None
        };
        let bar_w = draw_bar_mix(c, f, &mut dash, mix, bar_y, t);
        // Claude Code's auto-compact buffer: the window's last part, dimmed and hatched, drawn
        // first so the parts lie over it once the context reaches it.
        let buffer_x = IX + ((size - buffer) / size) as f32 * bar_w;
        if buffer > 0. {
            c.rrect(
                buffer_x,
                bar_y,
                IX + bar_w - buffer_x,
                PARTS_BAR_H,
                1.5,
                t.track,
            );
            let mut hx = buffer_x - PARTS_BAR_H;
            while hx < IX + bar_w {
                // One diagonal per 3 pt, cut to the segment.
                let (x0, x1) = (hx.max(buffer_x), (hx + PARTS_BAR_H).min(IX + bar_w));
                if x1 > x0 {
                    c.line(
                        x0,
                        bar_y + PARTS_BAR_H - (x0 - hx),
                        x1,
                        bar_y + PARTS_BAR_H - (x1 - hx),
                        0.75,
                        t.faint,
                    );
                }
                hx += 3.;
            }
            dash.buffer = Some((buffer_x, IX + bar_w - buffer_x));
        }
        let mut x = IX;
        let mut segs = vec![];
        for (name, n, ink) in &p.parts {
            // The least visible width first, then the bar's end: a context past the window, or a
            // tiny part at its end, never runs a segment off the bar.
            let w = ((*n / size) as f32 * bar_w)
                .max(PARTS_GAP + 1.5)
                .min((IX + bar_w - x).max(0.));
            if w > PARTS_GAP {
                c.rrect(x, bar_y, w - PARTS_GAP, PARTS_BAR_H, 1.5, *ink);
            }
            segs.push(DashSegment {
                name: (*name).into(),
                tokens: *n,
                x,
                w,
                legend: None,
            });
            x += w;
        }
        let track_end = if buffer > 0. {
            buffer_x - PARTS_GAP
        } else {
            IX + bar_w
        };
        if x < track_end - 1. {
            c.rrect(x, bar_y, track_end - x, PARTS_BAR_H, 1.5, t.track);
        }
        // Where Claude Code compacts on its own: a tick at the buffer's start, in the gaps above
        // and below the bar (CSR's own band ends before it).
        if buffer > 0. {
            c.line(
                buffer_x,
                bar_y - 3.,
                buffer_x,
                bar_y + PARTS_BAR_H + 3.,
                1.25,
                t.quiet,
            );
            dash.window = Some(size);
        }
        dash.context_bar = Some((IX, bar_y, bar_w, PARTS_BAR_H));
        at = bar_y + PARTS_BAR_H + S2;
        // The legend: a small filled circle and the part's short name and size, on one line.
        // A lean card keeps the bar and its heading and drops the legend under it.
        let legend: &[Vec<usize>] = if p.lean { &[] } else { &p.legend };
        for (i, text) in draw_legend(c, f, &p.parts, legend, at, t) {
            segs[i].legend = Some(text);
        }
        dash.segments = segs;
        at += legend.len() as f32 * drow();
        match (&p.turn, p.estimated && !p.lean) {
            (Some(line), _) if !p.lean => {
                single(
                    c,
                    f,
                    line,
                    IX,
                    at + 1. + f.cap(caption),
                    IW,
                    theme::row_caption_size(),
                    t.quiet,
                    t,
                );
                dash.est_marks += line.matches("est.").count();
                dash.turn = Some(line.clone());
                at += drow();
            }
            (None, true) => {
                let line = "Part sizes est., scaled to the real total.";
                single(
                    c,
                    f,
                    line,
                    IX,
                    at + 1. + f.cap(caption),
                    IW,
                    theme::row_caption_size(),
                    t.quiet,
                    t,
                );
                dash.est_marks += 1;
                at += drow();
            }
            _ => {}
        }
        if let Some((line, cold)) = p.cache.as_ref().filter(|_| !p.lean) {
            single(
                c,
                f,
                line,
                IX,
                at + 1. + f.cap(caption),
                IW,
                theme::row_caption_size(),
                if *cold { t.amber } else { t.quiet },
                t,
            );
            dash.cache_row = Some(line.clone());
            at += drow();
        }
        at += S2 + gap;
        let _ = ctx_h;
    }
    // C2. A new session's start. With no reading yet it is the Context section itself: the parts
    // the transcript and CSR's hook measured, as a bar, a legend and one note of what is
    // estimated and what is unmeasured. With a reading it is one line under the live section.
    // Then what CSR handed this session, a row each.
    if let Some(start) = &p.start {
        if start.own_block() {
            at += S2;
            let base = at + f.cap(name_st);
            c.text(f, "Context", IX, base, name_st, t.ink, A::L);
            let name_w = f.width("Context", name_st);
            let head = "no reading yet";
            let head_w = c.text(
                f,
                head,
                IX + name_w + S2,
                base,
                sans(dash_name(), t.bold),
                t.quiet,
                A::L,
            );
            // Every size shown is an estimate, in every layout: the marker rides the heading, so
            // it stays when the pane is too short for the note under the legend. A short pane
            // also says how many parts are unmeasured, as the note names them.
            let mut marks = String::new();
            if !start.parts.is_empty() {
                marks.push_str(" · sizes est.");
            }
            if p.csr_compact && !start.unmeasured.is_empty() {
                marks.push_str(&format!(" · {} unmeasured", start.unmeasured.len()));
            }
            if !marks.is_empty() {
                c.text(
                    f,
                    &marks,
                    IX + name_w + S2 + head_w,
                    base,
                    sans(theme::row_caption_size(), t.regular),
                    t.quiet,
                    A::L,
                );
            }
            dash.est_marks += marks.matches("est.").count();
            dash.context_head = Some(format!("Context · {head}{marks}"));
            dash.no_reading = true;
            at += dash_name_line();
            let bar_y = at + S1;
            let bar_w = draw_bar_mix(c, f, &mut dash, start.mix, bar_y, t);
            c.rrect(IX, bar_y, bar_w, PARTS_BAR_H, 1.5, t.track);
            // The bar spans the window as the live section's does. With no window known, two
            // measured parts or more share the bar between them; one alone is a sliver, never a
            // bar that reads as the whole context.
            let total: f64 = start.parts.iter().map(|part| part.1).sum();
            let scale = start
                .window
                .or_else(|| (start.parts.len() >= 2).then_some(total))
                .unwrap_or(f64::INFINITY);
            let mut x = IX;
            let mut segs = vec![];
            for (name, n, ink) in &start.parts {
                let w = ((*n / scale) as f32 * bar_w)
                    .max(PARTS_GAP + 1.5)
                    .min((IX + bar_w - x).max(0.));
                if w > PARTS_GAP {
                    c.rrect(x, bar_y, w - PARTS_GAP, PARTS_BAR_H, 1.5, *ink);
                }
                segs.push(DashSegment {
                    name: (*name).into(),
                    tokens: *n,
                    x,
                    w,
                    legend: None,
                });
                x += w;
            }
            dash.context_bar = Some((IX, bar_y, bar_w, PARTS_BAR_H));
            at = bar_y + PARTS_BAR_H + S2;
            for (i, text) in draw_legend(c, f, &start.parts, &start.legend, at, t) {
                segs[i].legend = Some(text);
            }
            dash.segments = segs;
            at += start.legend.len() as f32 * drow();
            if !p.csr_compact {
                let note = start.note();
                single(
                    c,
                    f,
                    &note,
                    IX,
                    at + 1. + f.cap(caption),
                    IW,
                    theme::row_caption_size(),
                    t.quiet,
                    t,
                );
                dash.est_marks += note.matches("est.").count();
                dash.start_note = Some(note);
                at += drow();
            }
            if let Some((line, cold)) = &p.cache {
                single(
                    c,
                    f,
                    line,
                    IX,
                    at + 1. + f.cap(caption),
                    IW,
                    theme::row_caption_size(),
                    if *cold { t.amber } else { t.quiet },
                    t,
                );
                dash.cache_row = Some(line.clone());
                at += drow();
            }
            at += S2 + gap;
        } else {
            let line = start.started_line();
            single(
                c,
                f,
                &line,
                IX,
                at + 1. + f.cap(caption),
                IW,
                theme::row_caption_size(),
                t.quiet,
                t,
            );
            dash.est_marks += line.matches("est.").count();
            dash.start_row = Some(line);
            at += drow();
        }
        for row in &start.handed {
            draw_handed_row(c, f, &mut dash, t, row, at);
            at += handed_row();
        }
        if !start.handed.is_empty() || !start.own_block() {
            at += S3;
        }
    }
    // After a compact the start block is gone but CSR's text is not: its first clause, above the
    // Self-Reflect row.
    if p.start.is_none() && !p.lean {
        if let Some(row) = &p.told_row {
            draw_handed_row(c, f, &mut dash, t, row, at);
            at += handed_row();
        }
    }
    // D. what CSR did this session: a ring to each measured value, its words under it
    if !p.csr.is_empty() && !p.bare && p.csr_compact {
        let st = sans(dash_lead(), t.medium);
        let lead = "Self-Reflect this session: ";
        let lead_w = f.width(lead, sans(dash_lead(), t.bold));
        let mut parts: Vec<&str> = p.csr.iter().map(|r| r.label.as_str()).collect();
        while parts.len() > 1 && lead_w + f.width(&parts.join(" · "), st) > IW - 10. {
            parts.pop();
        }
        let base = at + 2. + f.cap(st);
        c.rrect(IX, at, 3., theme::pick(20., 15.), 1.5, t.csr_share);
        c.text(
            f,
            lead,
            IX + 10.,
            base,
            sans(dash_lead(), t.bold),
            t.csr_share,
            A::L,
        );
        let rest = f.fit(&parts.join(" · "), st, IW - 10. - lead_w);
        c.text(f, &rest, IX + 10. + lead_w, base, st, t.ink, A::L);
        dash.csr_row = Some(format!("{lead}{rest}"));
        if p.has_handed && !c.dry {
            c.cuts.push(Cut {
                y: base - 4.,
                size: 8.,
                title: "Self-Reflect this session",
                full: format!("{lead}{rest}"),
                lost: None,
                handed: true,
            });
        }
        at += dash_lead_line();
    } else if !p.csr.is_empty() && !p.bare {
        label(
            c,
            f,
            "Self-Reflect this session",
            IX,
            at + f.cap(dash_heading(t)),
            theme::row_title_size(),
            t.ink,
            t.bold,
        );
        if p.has_handed && !c.dry {
            c.cuts.push(Cut {
                y: at + theme::row_title_line() / 2.,
                size: theme::row_title_line() / 2.,
                title: "Self-Reflect this session",
                full: "Self-Reflect this session".to_owned(),
                lost: None,
                handed: true,
            });
        }
        at += theme::row_title_line() + S2;
        let slot = IW / p.csr.len() as f32;
        let cy = at + CSR_RING_R;
        for (i, ring) in p.csr.iter().enumerate() {
            let x = IX + slot * (i as f32 + 0.5);
            let fill = ring.fill.map_or(1., |n| n.clamp(0., 1.) as f32);
            // A sliver is still seen; a count fills the ring.
            let drawn = if ring.fill.is_some() {
                fill.max(6. / 360.)
            } else {
                fill
            };
            c.ring(
                x,
                cy,
                CSR_RING_R,
                t.ring,
                t.track,
                &[(0., drawn, t.csr_share)],
            );
            let st = serif((CSR_RING_R * 0.6).clamp(10., 18.), t.medium);
            let room = 2. * CSR_RING_R - 2. * S2;
            let st = serif(
                st.size * room / f.width(&ring.number, st).max(room),
                t.medium,
            );
            c.text(f, &ring.number, x, cy + st.size * 0.3, st, t.ink, A::M);
            quota_label(
                c,
                f,
                &ring.label,
                x,
                cy + CSR_RING_R + S2 + f.cap(caption),
                slot - S2,
                theme::row_caption_size(),
                t.quiet,
                Some(Swatch::Disc(t.csr_share)),
                t,
            );
            dash.csr_rings.push(DashRing {
                number: ring.number.clone(),
                label: ring.label.clone(),
                fill: ring.fill.map(|n| n.clamp(0., 1.) as f32),
                center: (x, cy),
                radius: CSR_RING_R,
            });
        }
        dash.csr_row = Some(format!(
            "Self-Reflect this session: {}",
            p.csr
                .iter()
                .map(|r| r.label.as_str())
                .collect::<Vec<_>>()
                .join(" · ")
        ));
        at +=
            2. * CSR_RING_R + S2 + p.csr_label_lines.max(1) as f32 * theme::row_caption_line() + S2;
    }
    if let Some((line, red)) = &p.status {
        c.icon(
            f,
            Icon::CircleAlert,
            IX,
            at + f.cap(caption) + 1.,
            12.,
            10.,
            if *red { t.risk } else { t.amber },
        );
        single(
            c,
            f,
            line,
            IX + 18.,
            at + f.cap(caption) + 1.,
            IW - 18.,
            theme::row_caption_size(),
            t.quiet,
            t,
        );
        at += drow() + S1;
    }
    let _ = (head_h, limits_h, csr_h);
    dash.rows_bottom = at + P;
    dash.card_bottom = y + h;
    // E. the fresh tokens per turn, under it all
    if chart && fresh_bar_heights(&data.requests).is_some() {
        let top = at;
        let room = y + h - top - S2;
        if room >= DASH_CHART_MIN {
            let given = room.min(DASH_CHART_WANT + S5 * 3.);
            history_plot(c, f, data, top, given, t);
            dash.chart = true;
        }
    }
    c.dash = Some(dash);
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
        false,
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
        y + P + f.cap(heading(t)),
        theme::TITLE_SIZE,
        t.ink,
        t.bold,
    );
    if let Some((line, sev)) = csr_status(g) {
        let col = if sev == "red" { t.risk } else { t.amber };
        c.icon(
            f,
            Icon::CircleAlert,
            IX + context_title_w(f, t) + S3,
            y + P + f.cap(heading(t)),
            12.,
            10.,
            col,
        );
        single(
            c,
            f,
            &line,
            IX + context_title_w(f, t) + S3 + 18.,
            y + P + f.cap(heading(t)),
            220.,
            10.,
            t.quiet,
            t,
        );
    }
    if let Some(n) = data.tally["by_csr"].as_u64().filter(|n| *n > 0) {
        by_csr_mark(c, f, t, n, y);
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
    // Under a forecast line the lines are already apart: the gap shrinks so the bar keeps its room.
    let details_gap = if has_notes(g) { S1 } else { S3 };
    let details_h = 6.;
    // The trend and payback line, under the notes line. "N by CSR" sits in the title row instead.
    // The breakdown's words take a line of their own; the median change and the payback share one.
    let pay = payback(g);
    let broken = breakdown_of(g).is_some_and(|b| b["caption"].is_string());
    let details: Vec<String> = if broken {
        turn_line(g)
            .into_iter()
            .chain((!pay.is_empty()).then_some(pay))
            .collect()
    } else {
        let line: Vec<_> = turn_line(g)
            .into_iter()
            .chain((!pay.is_empty()).then_some(pay))
            .collect();
        if line.is_empty() {
            vec![]
        } else {
            vec![line.join(" · ")]
        }
    };
    let compacting = g["compacting"].is_object();
    let bar_wanted = breakdown_of(g).is_some() && !compacting;
    let right_bare = gauge_r + label_below + theme::CAPTION_LINE + notes_h(g);
    let right_for = |lines: usize, bar: bool| {
        right_bare
            + details_gap
            + details_h
            + lines.saturating_sub(1) as f32 * theme::CAPTION_LINE
            + if bar { BREAKDOWN_ROOM } else { 0. }
    };
    let right_full = right_for(1, false);
    // The lines take their room from the card or are not drawn: they never hang below the card.
    // The richest set that fits wins: the lines and the bar, the lines, the first line and the
    // bar, the first line. The compaction bar sits where the line would, so it keeps the room too.
    let wants_row = !details.is_empty() || compacting || bar_wanted;
    let first = details.len().min(1);
    let (shown_lines, shown_bar) = [
        (details.len(), bar_wanted),
        (details.len(), false),
        (first, bar_wanted),
        (first, false),
    ]
    .into_iter()
    .find(|(n, bar)| (2. * r).max(right_for((*n).max(1), *bar)) <= slot + 0.5)
    .unwrap_or((0, false));
    let show_details =
        wants_row && (2. * r).max(right_for(shown_lines.max(1), shown_bar)) <= slot + 0.5;
    let right_h = if show_details {
        right_for(shown_lines.max(1), shown_bar)
    } else if wants_row && details.is_empty() && !bar_wanted {
        right_full
    } else {
        right_bare
    };
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
    let details_y = if right_h == right_bare {
        group_bottom
    } else {
        gauge_label_y + theme::CAPTION_LINE + notes_h(g) + details_gap + 2.
    };

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
        has_notes(g),
        t,
    );

    if show_details {
        for (i, line) in details.iter().take(shown_lines).enumerate() {
            // The payback keeps its warning ink; the words about the turn stay quiet.
            let is_payback = !broken || i + 1 == details.len() && details.len() > 1;
            let col = if is_payback && g["payback"]["sev"] == "amber" {
                t.amber
            } else if is_payback && compacting {
                t.mint
            } else {
                t.quiet
            };
            let at = details_y + i as f32 * theme::CAPTION_LINE;
            single(c, f, line, right_x0, at, right_w, 10.5, col, t);
        }
        if shown_bar {
            let top = details_y
                + shown_lines.saturating_sub(1) as f32 * theme::CAPTION_LINE
                + if shown_lines > 0 { 4. } else { -6. };
            breakdown_bar(c, g, right_x0, top, right_w.min(240.), t);
        }
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
                "Self-Reflect's dreamer asked this while you worked, quoted: “{}” Its reasoning:",
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
    quota_row(&mut c, f, data, 76., w - 86., 65., 16., 93., 8., false, t);
    Some(c.pm)
}

#[path = "limits_chart.rs"]
mod limits_chart;
#[cfg(test)]
#[path = "instrument_tests.rs"]
mod tests;
