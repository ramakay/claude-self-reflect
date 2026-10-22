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
        mood(self.kind, self.lead)
    }
}
fn river_row(at: f64, kind: RiverKind, lead: &'static str, line: String) -> RiverRow {
    RiverRow {
        at,
        when: (at > 0.).then_some(at),
        kind,
        lead,
        line,
    }
}
/// The one word a timeline row says before its line. Fixed table, no model.
pub fn mood(kind: RiverKind, lead: &str) -> &'static str {
    match (kind, lead) {
        (RiverKind::Write | RiverKind::Commit, "! ") => "worried",
        (RiverKind::Prompt, _) => "asked",
        (RiverKind::Dream, _) => "wondered",
        (RiverKind::Compact, _) => "made room",
        (RiverKind::Write, _) => "tinkered",
        (RiverKind::Commit, _) => "landed",
        (RiverKind::Agent, "✓ ") => "heard back",
        (RiverKind::Agent, _) => "delegated",
        (RiverKind::Other, _) => "",
    }
}
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
    /// An observation item shown in full over the pane.
    pub popup: Option<PopupAt>,
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
/// Every look of this session that carries `key`, newest first, one item per `line`. A later look
/// with an empty list never erases what an earlier one found.
fn observations(data: &Value, session: &Value, key: &str) -> Vec<Value> {
    let mut sources: Vec<&Value> = array(&data["dreams"])
        .iter()
        .chain(array(&data["dream_records"]))
        .filter(|r| &r["session"] == session && r[key].is_array())
        .collect();
    sources.sort_by(|a, b| num(&b["at"]).total_cmp(&num(&a["at"])));
    let mut out: Vec<Value> = vec![];
    for item in sources.iter().flat_map(|r| array(&r[key])) {
        if !string(&item["line"]).is_empty()
            && !string(&item["evidence"]).is_empty()
            && !out.iter().any(|o| o["line"] == item["line"])
        {
            out.push(item.clone());
        }
    }
    out.truncate(OBSERVATION_CAP);
    out
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
                    "○ ",
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
        river.extend(array(&data["threads"]).iter().map(|thread| {
            river_row(
                num(&thread["last_seen"]),
                RiverKind::Prompt,
                "↳ ",
                format!(
                    "{} · {}",
                    string(&thread["text"]),
                    crate::companion::threads::thread_group(thread)
                ),
            )
        }));
        river.sort_by(|a, b| b.at.total_cmp(&a.at));
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
        let card = &data["infinite"]["card"];
        let notice_until = notice
            .as_deref()
            .filter(|n| n.starts_with("Compacted."))
            .and(card["at"].as_f64())
            .map(|at| at + 120.);
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
            },
            confusing: observations(data, &s["id"], "confusing_claude"),
            should_be: observations(data, &s["id"], "should_be"),
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
                &drawn_compaction(&data["compacting"]),
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
            now,
        }
    }
    /// The items of one observation card, newest first.
    pub fn items(&self, card: ObsCard) -> &[Value] {
        match card {
            ObsCard::Confusing => &self.confusing,
            ObsCard::ShouldBe => &self.should_be,
        }
    }
    /// The popup's content for `at`, or `None` when that item is no longer on its card.
    pub fn detail(&self, at: PopupAt) -> Option<Detail> {
        observation::detail(self.items(at.card), at)
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
                .filter(|a| agents::is_active(a))
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
/// A card of `rows` one-line observations under its title.
fn observation_card_h(rows: usize) -> f32 {
    P * 2. + theme::TITLE_LINE + S2 + rows as f32 * theme::BODY_LINE
}
pub(super) const CONFUSED_TITLE: &str = "Confused Claude";
pub(super) const SHOULD_BE_TITLE: &str = "Should be a skill / memory / tool";
/// The short label a should-be item carries on its row.
pub(super) fn should_be_kind(item: &Value) -> &'static str {
    match string(&item["kind"]) {
        "skill" => "skill",
        "memory" => "memory",
        "hook" => "hook",
        "CLAUDE.md" | "CLAUDE.md rule" | "claude_md" | "rule" => "CLAUDE.md rule",
        _ => "tool",
    }
}
/// One observation row as the plain-text pane writes it.
pub fn observation_text(item: &Value, should: bool) -> String {
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
pub(super) fn evidence_receipt(item: &Value) -> String {
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
    if let Some(detail) = open.popup.and_then(|at| c.detail(at)) {
        return observation::rows(&detail, cols);
    }
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
    ] {
        if items.is_empty() {
            continue;
        }
        out.extend(wrap(title));
        for item in items {
            out.extend(wrap(&observation_text(item, should)));
        }
    }
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
        out.extend(wrap(&format!("{MONOLOGUE} · {MONOLOGUE_SUB}")));
        details_start = out.len();
        for row in &c.river {
            let when = row.time_label(c.now);
            out.extend(wrap(&format!(
                "{}{}{}{} · {}",
                when,
                if when.is_empty() { "" } else { " · " },
                row.lead,
                row.mood(),
                row.line
            )));
        }
    }
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
/// "Try a question" with as many starter questions as fit between `cursor` and `bottom`.
fn starter_cards(canvas: &mut Canvas, f: &Fonts, mut cursor: f32, bottom: f32, t: &Tokens) {
    let title = sans(theme::TITLE_SIZE, t.bold);
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
    /// Each observation row's card, place on it and centre Y in layout pt: the click targets.
    pub observation_rows: Vec<(ObsCard, usize, f32)>,
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
        };
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
        // A heading, then up to eight bands with a gap between each.
        P * 2. + MONOLOGUE_H + (c.river.len().min(8) as f32 * ROW_PITCH - ROW_GAP).max(40.)
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
    let observed = [
        (CONFUSED_TITLE, &c.confusing, false),
        (SHOULD_BE_TITLE, &c.should_be, true),
    ];
    let observed_h: f32 = observed
        .iter()
        .filter(|(_, items, _)| !items.is_empty())
        .map(|(_, items, _)| observation_card_h(items.len()) + S3)
        .sum();
    let top_core = M
        + hero_h
        + S3
        + observed_h
        + story_h
        + if story_h > 0. { S3 } else { 0. }
        + notice_h
        + notice_gap;
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
    y += hero_h + card_gap;
    let mut observation_rows = vec![];
    for (title_text, items, should) in observed {
        if items.is_empty() {
            continue;
        }
        let h = observation_card_h(items.len());
        panel(&mut canvas, y, h, t);
        label(
            &mut canvas,
            f,
            title_text,
            IX,
            y + P + f.cap(title),
            theme::TITLE_SIZE,
            t.ink,
            t.bold,
        );
        let mut row_y = y + P + theme::TITLE_LINE + S2;
        for (index, item) in items.iter().enumerate() {
            observation_rows.push((
                if should {
                    ObsCard::ShouldBe
                } else {
                    ObsCard::Confusing
                },
                index,
                row_y + theme::BODY_LINE / 2.,
            ));
            let base = row_y + f.cap(body);
            let receipt = evidence_receipt(item);
            let receipt_w = f.width(&receipt, caption);
            let kind_w = if should { 78. } else { 0. };
            if should {
                label(
                    &mut canvas,
                    f,
                    should_be_kind(item),
                    IX,
                    base,
                    theme::CAPTION_SIZE,
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
                theme::BODY_SIZE,
                t.ink,
                t,
            );
            canvas.text(f, &receipt, IX + IW, base, caption, t.quiet, A::R);
            row_y += theme::BODY_LINE;
        }
        y += h + card_gap;
    }
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
            let spine_h = (extra_h - P * 2. - MONOLOGUE_H).max(0.);
            let spine_x = IX + 72.;
            let rows: Vec<_> = c
                .river
                .iter()
                .skip(open.scroll)
                .take(((spine_h + ROW_GAP) / ROW_PITCH) as usize)
                .collect();
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
                single(
                    &mut canvas,
                    f,
                    &row.picture_text(),
                    x,
                    b,
                    IX + IW - x,
                    11.5,
                    t.ink,
                    t,
                );
                b += ROW_PITCH;
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
        figure_region.0 + figure_region.1
    };
    let last_card_bottom_pad = (height - last_card_bottom).max(0.);
    if let Some(detail) = open.popup.and_then(|at| c.detail(at)) {
        observation::overlay(
            &mut canvas,
            f,
            &detail,
            height,
            last_card_bottom.min(height),
            t,
        );
    }
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
            observation_rows,
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

    fn timeline_data() -> (Value, Value) {
        let now = 1_800_000_000.;
        let s = json!({"id":"tl","compacts":[now - 5400.],"main":[],"prompts":[]});
        let entry = |claim: &str, receipt: &str| {
            json!({"sig":claim,"kind":"shift","claim":claim,"now":claim,"was":null,
                "before":[],"after":[{"scope":"summarizer.rs","sym":"store_story","mark":"~"}],
                "receipts":[receipt],"grounded":true,"moved":true})
        };
        let data = json!({
            "story_map": {"at": now - 6200., "entries": [
                entry("Keep prior shift reflections on story rewrite", "oid:aaaaaaa"),
                entry("Pane fails open when the map is absent", "oid:bbbbbbb"),
                entry("Unlanded edit of the ledger", "edit:ledger"),
                entry("Dated by neither", "oid:ccccccc"),
            ]},
            "shifts": [
                {"oid":"aaaaaaa1111","date":"2026-10-21","at": now - 3000.},
                {"oid":"bbbbbbb2222","date":"2026-10-20","at": now - 86400.},
            ],
            "dreams": [{"session":"tl","at": now - 600., "line":"What if recall started from the file?", "answer":"yes"}],
            "threads": [{"text":"Decide the release name","last_seen": now - 9000., "state":"open"}],
        });
        (data, s)
    }

    #[test]
    fn timeline_rows_carry_their_kind_and_their_own_age() {
        let now = 1_800_000_000.;
        let (data, s) = timeline_data();
        let c = Composition::build(&data, &s, now);
        let rows: Vec<_> = c.river.iter().map(|r| (r.kind, r.at)).collect();
        assert_eq!(
            rows,
            vec![
                (RiverKind::Dream, now - 600.),
                (RiverKind::Commit, now - 3000.),
                (RiverKind::Compact, now - 5400.),
                (RiverKind::Write, now - 6200.),
                (RiverKind::Commit, now - 6200.),
                (RiverKind::Prompt, now - 9000.),
                (RiverKind::Commit, now - 86400.),
            ]
        );
        let lines: Vec<_> = c.river.iter().map(|r| r.line.as_str()).collect();
        assert!(lines[0].starts_with("What if"), "{lines:?}");
        assert_eq!(lines[2], "Compacted");
        assert!(lines
            .iter()
            .all(|l| !l.starts_with(['☾', '♻', '↳', '○', '✓'])));
    }

    #[test]
    fn the_threads_drilldown_draws_one_icon_per_row_and_no_gradient() {
        let now = 1_800_000_000.;
        let (data, s) = timeline_data();
        let mut c = Composition::build(&data, &s, now);
        c.river.push(river_row(
            now - 700.,
            RiverKind::Agent,
            "",
            "Scan the repo started".into(),
        ));
        c.river.push(river_row(
            now - 800.,
            RiverKind::Other,
            "",
            "Something else".into(),
        ));
        c.river.sort_by(|a, b| b.at.total_cmp(&a.at));
        c.requests = (0..20)
            .map(|i| (150_000. + f64::from(i) * 1000., 2_000.))
            .collect();
        let open = Open {
            threads: true,
            ..Open::default()
        };
        let png = std::env::var_os("CSR_THREADS_PNG");
        for theme in [Theme::Dark, Theme::Light] {
            let (pm, _) = draw(&c, 1210, 1700, theme, open, false).expect("instrument draws");
            if let Some(dir) = &png {
                let name = format!("{}/threads-{theme:?}.png", dir.to_string_lossy());
                save(&name, &pm);
            }
        }
    }

    fn at_local(h: u32, m: u32, day: u32) -> f64 {
        use chrono::TimeZone;
        chrono::FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, day, h, m, 0)
            .unwrap()
            .timestamp() as f64
    }

    #[test]
    fn time_labels_follow_the_local_day() {
        let tz = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let now = at_local(15, 0, 21);
        let label = |at: f64| time_label_in(at, now, &tz);
        assert_eq!(label(now), "just now");
        assert_eq!(label(now - 59.), "just now");
        assert_eq!(label(now + 30.), "just now");
        assert_eq!(label(now - 60.), "1 min ago");
        assert_eq!(label(now - 59. * 60.), "59 min ago");
        assert_eq!(label(now - 3599.), "59 min ago");
        // An hour or more, still today: the clock time in the local zone.
        assert_eq!(label(now - 3600.), "14:00");
        assert_eq!(label(at_local(0, 5, 21)), "00:05");
        // Yesterday and older carry the date.
        assert_eq!(label(at_local(23, 30, 20)), "Oct 20 23:30");
        assert_eq!(label(at_local(9, 7, 3)), "Oct 3 09:07");
        // Crossing midnight: 1.5 hours ago is still yesterday.
        let early = at_local(0, 30, 21);
        assert_eq!(
            time_label_in(at_local(23, 0, 20), early, &tz),
            "Oct 20 23:00"
        );
        // The same instant reads differently in another zone's day.
        let utc = chrono::Utc;
        assert_eq!(time_label_in(at_local(1, 0, 21), now, &utc), "Oct 20 23:00");
    }

    #[test]
    fn every_row_kind_has_its_mood_word() {
        let word = |kind, lead| mood(kind, lead);
        assert_eq!(word(RiverKind::Prompt, "↳ "), "asked");
        assert_eq!(word(RiverKind::Dream, "☾ "), "wondered");
        assert_eq!(word(RiverKind::Compact, "♻ "), "made room");
        assert_eq!(word(RiverKind::Write, "~ "), "tinkered");
        assert_eq!(word(RiverKind::Commit, "~ "), "landed");
        assert_eq!(word(RiverKind::Agent, "○ "), "delegated");
        assert_eq!(word(RiverKind::Agent, "✓ "), "heard back");
        assert_eq!(word(RiverKind::Write, "! "), "worried");
        assert_eq!(word(RiverKind::Commit, "! "), "worried");
        assert_eq!(word(RiverKind::Other, ""), "");
    }

    fn write_row(data: &Value, s: &Value, now: f64) -> RiverRow {
        let c = Composition::build(data, s, now);
        c.river
            .into_iter()
            .find(|r| r.kind == RiverKind::Write)
            .expect("an unlanded edit row")
    }

    #[test]
    fn a_write_row_is_dated_by_its_files_last_edit_never_the_maps_time() {
        let now = 1_800_000_000.;
        let (data, mut s) = timeline_data();
        // No per-file edit times: no label, but the row keeps its sort position at the map's time.
        let row = write_row(&data, &s, now);
        assert_eq!(row.when, None);
        assert_eq!(row.at, now - 6200.);
        assert_eq!(row.time_label(now), "");
        // The file was last edited 20 minutes ago.
        s["edit_at"] = json!({"/repo/csr-engine/src/summarizer.rs": now - 1200.});
        let row = write_row(&data, &s, now);
        assert_eq!(row.when, Some(now - 1200.));
        assert_eq!(row.at, now - 1200.);
        assert_eq!(row.time_label(now), "20 min ago");
        // A read-only touch in `last` dates nothing.
        s["edit_at"] = json!({});
        s["last"] = json!({"/repo/csr-engine/src/summarizer.rs": now - 60.});
        assert_eq!(write_row(&data, &s, now).when, None);
        // A different file with a similar name does not match.
        s["edit_at"] = json!({"/repo/csr-engine/src/not_summarizer.rs": now - 60.});
        assert_eq!(write_row(&data, &s, now).when, None);
        // Two files answering to one scope are ambiguous: borrow neither.
        s["edit_at"] = json!({"/a/summarizer.rs": now - 60., "/b/summarizer.rs": now - 90.});
        assert_eq!(write_row(&data, &s, now).when, None);
    }

    #[test]
    fn rows_without_a_time_of_their_own_show_none() {
        let now = 1_800_000_000.;
        let (data, s) = timeline_data();
        let c = Composition::build(&data, &s, now);
        // "Dated by neither" names a commit the session never saw: sorted by the map, not labelled.
        let unmatched = c
            .river
            .iter()
            .find(|r| r.line.starts_with("summarizer.rs · Dated by neither"))
            .expect("row");
        assert_eq!(unmatched.kind, RiverKind::Commit);
        assert_eq!(unmatched.when, None);
        // Every other row carries its own time.
        assert!(c
            .river
            .iter()
            .filter(|r| r.when.is_none())
            .all(|r| r.line.contains("Dated by neither") || r.kind == RiverKind::Write));
    }

    #[test]
    fn the_text_pane_reads_monologue_verb_and_time() {
        let now = 1_800_000_000.;
        let (data, mut s) = timeline_data();
        s["edit_at"] = json!({"/r/summarizer.rs": now - 1200.});
        let c = Composition::build(&data, &s, now);
        let open = Open {
            threads: true,
            ..Open::default()
        };
        let text = text_rows(&c, 110, 60, open);
        let head = text
            .iter()
            .position(|l| l == "Monologue · the session thinking out loud")
            .unwrap_or_else(|| panic!("{text:#?}"));
        assert!(text[head + 1].contains("wondered"), "{text:#?}");
        let all = text.join("\n");
        assert!(
            all.contains("☾ wondered · What if recall started from the file?"),
            "{all}"
        );
        assert!(all.contains("10 min ago · ☾"), "{all}");
        assert!(all.contains("♻ made room · Compacted"), "{all}");
        assert!(all.contains("↳ asked · Decide the release name"), "{all}");
        assert!(
            all.contains("20 min ago · ~ tinkered · summarizer.rs"),
            "{all}"
        );
        assert!(!all.contains("h0"), "no durations: {all}");
        // The undated unlanded row has no time prefix at all.
        let (data, s) = timeline_data();
        let c = Composition::build(&data, &s, now);
        let all = text_rows(&c, 110, 60, open).join("\n");
        assert!(
            all.contains("\n~ tinkered · summarizer.rs · Unlanded edit"),
            "{all}"
        );
    }

    fn fresh_with_history(prior: usize) -> Composition {
        let mut c = Composition::build(&json!({}), &json!({"id":"fresh"}), 1_800_000_000.);
        assert!(c.empty_session);
        c.repo_recap = Some(crate::companion::repo_recap::RepoRecap {
            session: "64f9b5b9aaaa".into(),
            at: 1_799_900_000.,
            age: "1d ago".into(),
            text: "you asked: Fix the footer".into(),
            history: (0..prior)
                .map(|i| crate::companion::repo_recap::RepoSessionSummary {
                    session: format!("s{i}"),
                    at: 1_799_000_000. - i as f64,
                    age: format!("{}d ago", i + 2),
                    intent: format!("you asked: Question number {i}"),
                    outcome: if i == 1 { "done" } else { "unknown" }.into(),
                })
                .collect(),
        });
        c
    }

    #[test]
    fn earlier_here_drops_noise_and_fills_spare_room_with_starter_questions() {
        let c = fresh_with_history(4);
        let text = text_rows(&c, 100, 60, Open::default()).join("\n");
        assert!(text.contains("Last time here · 1d ago"), "{text}");
        assert!(!text.contains("64f9b5b9"), "{text}");
        assert!(text.contains("2d ago · Question number 0"), "{text}");
        assert!(text.contains("✓ 3d ago · Question number 1"), "{text}");
        assert!(!text.lines().any(|l| l.starts_with("? ")), "{text}");
        assert!(!text.contains("Question number 0 ·"), "{text}");
        let after = text.split("Earlier here").nth(1).unwrap();
        assert!(after.contains("Try a question"), "{text}");
        assert_eq!(earlier_intent("you asked: x"), "x");
        assert_eq!(earlier_intent("something"), "something");
        assert_eq!(history_mark("unknown"), None);
        let png = std::env::var_os("CSR_FRESH_PNG");
        for (name, n) in [("fresh-session", 19), ("fresh-few", 4)] {
            let c = fresh_with_history(n);
            let (pm, _) = draw(&c, 1210, 1700, Theme::Dark, Open::default(), false).expect("draws");
            if let Some(dir) = &png {
                save(&format!("{}/{name}.png", dir.to_string_lossy()), &pm);
            }
        }
    }

    fn observed_data() -> Value {
        let item = |line: &str, evidence: &str| json!({"line":line,"evidence":evidence});
        let should = |kind: &str, line: &str| json!({"kind":kind,"line":line,"evidence":"Turns 4 and 8: 'x'"});
        json!({
            "dreams": [{"id":"d1","session":"o","at":100.,"line":"What if?",
                "confusing_claude":[item("Misread the pane.","Turn 4: 'pane'")],
                "should_be":[should("hook","Check formatting before commits.")]}],
            "dream_records": [
                {"kind":"look","session":"o","at":300.,"outcome":"none",
                    "confusing_claude":[],"should_be":[]},
                {"kind":"look","session":"o","at":200.,"outcome":"none",
                    "confusing_claude":[item("Repeated a rejected approach.","Turn 12"),
                        item("Misread the pane.","Turn 15")],
                    "should_be":[should("skill","Bump the version in both files.")]},
                {"kind":"look","session":"other","at":400.,"outcome":"none",
                    "confusing_claude":[item("Not this session.","Turn 1")]},
            ],
        })
    }

    #[test]
    fn confused_and_should_be_come_from_every_look_not_just_the_newest() {
        let c = Composition::build(&observed_data(), &json!({"id":"o"}), 1000.);
        let lines = |v: &[Value]| {
            v.iter()
                .map(|i| string(&i["line"]).to_owned())
                .collect::<Vec<_>>()
        };
        // The newest look found nothing; the earlier ones still count, deduped by line.
        assert_eq!(
            lines(&c.confusing),
            ["Repeated a rejected approach.", "Misread the pane."]
        );
        assert_eq!(
            lines(&c.should_be),
            [
                "Bump the version in both files.",
                "Check formatting before commits."
            ]
        );
        let mut many = observed_data();
        many["dream_records"][1]["confusing_claude"] = json!((0..9)
            .map(|i| json!({"line":format!("Line {i}"),"evidence":"Turn 1"}))
            .collect::<Vec<_>>());
        let c = Composition::build(&many, &json!({"id":"o"}), 1000.);
        assert_eq!(c.confusing.len(), OBSERVATION_CAP);
    }

    #[test]
    fn the_two_observation_cards_are_separate_and_leave_the_dreamer_alone() {
        let c = Composition::build(&observed_data(), &json!({"id":"o"}), 1000.);
        let open = Open {
            dreams: true,
            ..Open::default()
        };
        let text = text_rows(&c, 100, 60, open);
        let at = |needle: &str| text.iter().position(|l| l == needle);
        let confused = at(CONFUSED_TITLE).expect("confused card");
        let should = at(SHOULD_BE_TITLE).expect("should-be card");
        assert!(confused < should);
        assert_eq!(
            text[confused + 1],
            "Repeated a rejected approach. · turn 12"
        );
        assert!(
            text.contains(&"skill: Bump the version in both files. · turns 4, 8".to_owned()),
            "{text:?}"
        );
        assert!(text.contains(&"hook: Check formatting before commits. · turns 4, 8".to_owned()));
        // The dreamer's own lines carry no counts and no folded-in findings.
        assert!(text[..confused]
            .iter()
            .all(|l| !l.contains("Confusing Claude") && !l.starts_with("! ")));
        let quiet = Composition::build(&json!({}), &json!({"id":"o"}), 1000.);
        let text = text_rows(&quiet, 100, 60, Open::default());
        assert!(!text
            .iter()
            .any(|l| l == CONFUSED_TITLE || l == SHOULD_BE_TITLE));
        if let Some(dir) = std::env::var_os("CSR_OBS_PNG") {
            for (name, c) in [("observed", &c), ("quiet", &quiet)] {
                let (pm, _) =
                    draw(c, 1210, 1700, Theme::Dark, Open::default(), false).expect("draws");
                save(&format!("{}/{name}.png", dir.to_string_lossy()), &pm);
            }
        }
    }

    #[test]
    fn the_popup_floats_over_the_pane_and_its_text_rows_replace_the_cards() {
        let mut data = observed_data();
        data["dream_records"][1]["should_be"][0]["explain"] =
            json!("You keep restating the release steps.");
        let c = Composition::build(&data, &json!({"id":"o"}), 1000.);
        let at = |card, index| PopupAt {
            card,
            index,
            copied: false,
        };
        // Text: the popup's own rows, and the dreamer's explanation when it wrote one.
        let open = Open {
            popup: Some(at(ObsCard::ShouldBe, 0)),
            ..Open::default()
        };
        let text = text_rows(&c, 80, 40, open);
        assert_eq!(
            text[0],
            "Should be a skill / memory / tool · 1 of 2 · skill"
        );
        assert_eq!(text[1], "Bump the version in both files.");
        assert!(text.contains(&"What happened: You keep restating the release steps.".to_owned()));
        assert!(text
            .iter()
            .any(|l| l.starts_with("Next step for you: Press c to copy a skill draft")));
        // An item that left its card shows the plain pane, not an empty popup.
        let gone = Open {
            popup: Some(at(ObsCard::Confusing, 7)),
            ..Open::default()
        };
        assert_eq!(
            text_rows(&c, 80, 40, gone),
            text_rows(&c, 80, 40, Open::default())
        );
        // Picture: every card row is a click target, and the popup changes the picture.
        let png = std::env::var_os("CSR_POPUP_PNG");
        for theme in [Theme::Dark, Theme::Light] {
            let (plain, metrics) =
                draw(&c, 1210, 1700, theme, Open::default(), false).expect("draws");
            let places: Vec<_> = metrics
                .observation_rows
                .iter()
                .map(|(c, i, _)| (*c, *i))
                .collect();
            assert_eq!(
                places,
                [
                    (ObsCard::Confusing, 0),
                    (ObsCard::Confusing, 1),
                    (ObsCard::ShouldBe, 0),
                    (ObsCard::ShouldBe, 1)
                ]
            );
            let ys: Vec<f32> = metrics.observation_rows.iter().map(|r| r.2).collect();
            assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
            for (name, popup) in [
                ("confusing", at(ObsCard::Confusing, 1)),
                ("should-be", at(ObsCard::ShouldBe, 0)),
                (
                    "copied",
                    PopupAt {
                        copied: true,
                        ..at(ObsCard::ShouldBe, 0)
                    },
                ),
            ] {
                let open = Open {
                    popup: Some(popup),
                    ..Open::default()
                };
                let (shown, with) = draw(&c, 1210, 1700, theme, open, false).expect("draws");
                assert_ne!(
                    shown.data(),
                    plain.data(),
                    "{theme:?} {name}: the popup is drawn"
                );
                // The cards beneath keep their layout: the click targets do not move.
                assert_eq!(with.observation_rows, metrics.observation_rows);
                if let Some(dir) = &png {
                    save(
                        &format!("{}/popup-{name}-{theme:?}.png", dir.to_string_lossy()),
                        &shown,
                    );
                }
            }
            if let Some(dir) = &png {
                save(
                    &format!("{}/cards-{theme:?}.png", dir.to_string_lossy()),
                    &plain,
                );
            }
        }
    }

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

    #[test]
    fn a_running_compaction_redraws_every_ten_seconds_not_every_second() {
        let draw_at = |elapsed: f64| {
            let data = json!({"compacting": {"elapsed": elapsed, "usual": {"high": 120}}});
            let c = Composition::build(&data, &json!({"id":"compacting"}), 1000.);
            draw(&c, 704, 986, Theme::Light, Open::default(), false)
                .expect("instrument draws")
                .0
        };
        // Each repaint re-uploads the whole picture, and the terminal blanks it meanwhile.
        assert!(draw_at(41.).data() == draw_at(49.).data());
        assert!(draw_at(49.).data() != draw_at(50.).data());
    }

    fn threads_open() -> Open {
        Open {
            threads: true,
            ..Open::default()
        }
    }
    #[test]
    fn the_draw_key_holds_until_the_next_change_and_moves_after_it() {
        let (data, s) = timeline_data();
        let open = threads_open();
        let (mut flips, mut edges) = (0, 0);
        for step in 0..300 {
            let now = 1_800_000_000. + step as f64 * 37.;
            let c = Composition::build(&data, &s, now);
            let (key, due) = (c.draw_key(open), c.next_change_at(open).unwrap());
            assert!(due > now);
            // No label moves before the deadline, whenever it is looked at.
            for at in [now + 0.5, (now + due) / 2., due - 0.05] {
                let later = Composition::build(&data, &s, at);
                assert_eq!(later.draw_key(open), key, "key moved at +{}s", at - now);
            }
            // A minute edge of a row's age moves the key; only the day edge may not.
            if due - now < 61. {
                edges += 1;
                flips +=
                    usize::from(Composition::build(&data, &s, due + 0.05).draw_key(open) != key);
            }
        }
        assert!(edges > 20, "{edges}");
        assert_eq!(flips, edges);
        // Without the threads list open no row age is on screen: the key does not move with them.
        let c = Composition::build(&data, &s, 1_800_000_000.);
        let later = Composition::build(&data, &s, 1_800_000_000. + 61.);
        assert_eq!(c.draw_key(Open::default()), later.draw_key(Open::default()));
    }

    #[test]
    fn the_cache_ring_and_the_notice_name_their_own_edges() {
        let now = 1_800_000_000.;
        let mut c = Composition::build(&json!({}), &json!({"id":"edges"}), now);
        c.figure["cache"] = json!({"left": 3599.5, "ttl": 3600., "cold": false});
        // The minute shown changes when 59m reads 58m: at 3540 seconds left.
        let due = c.next_change_at(Open::default()).unwrap();
        assert!((due - (now + 59.5 + 0.01)).abs() < 1e-6, "{}", due - now);
        c.figure["cache"] = json!({"left": 30., "ttl": 300., "cold": false});
        assert!((c.next_change_at(Open::default()).unwrap() - (now + 30.01)).abs() < 1e-6);
        c.figure["cache"] = json!({"left": null, "cold": true});
        assert_eq!(c.next_change_at(Open::default()), None);
        c.clocks.notice_until = Some(now + 7.);
        assert!((c.next_change_at(Open::default()).unwrap() - (now + 7.01)).abs() < 1e-6);
        c.clocks.compacting_elapsed = Some(43.);
        assert!((c.next_change_at(Open::default()).unwrap() - (now + 7.01)).abs() < 1e-6);
        c.clocks.compacting_elapsed = Some(49.);
        assert!((c.next_change_at(Open::default()).unwrap() - (now + 1.01)).abs() < 1e-6);
    }

    #[test]
    fn a_quota_ahead_of_pace_is_the_key_not_the_pace_itself() {
        let now = 1_800_000_000.;
        let quota = |pace: f64| {
            let mut c = Composition::build(&json!({}), &json!({"id":"q"}), now);
            c.figure["quotas"] = json!([{"code":"5H","used":60.,"pace":pace,"resets":now + 9000.}]);
            c.draw_key(Open::default())
        };
        assert_eq!(quota(0.10), quota(0.11));
        assert_ne!(quota(0.10), quota(0.60));
    }

    /// The caption draws `»` when used is past pace, with no margin; the text row's mark has a
    /// five-point one. A pace between them changes the caption and must change the key, and the
    /// schedule must name the moment the pace reaches the used share.
    #[test]
    fn the_key_and_the_schedule_follow_the_caption_s_own_threshold() {
        let now = 1_800_000_000.;
        // A 5H window that is 58% elapsed: it resets in 7560 s.
        let quota = |pace: f64| {
            let mut c = Composition::build(&json!({}), &json!({"id":"q"}), now);
            c.figure["quotas"] = json!([{"code":"5H","used":60.,"pace":pace,"resets":now + 7560.}]);
            c
        };
        let (ahead, caught_up) = (quota(0.58), quota(0.61));
        assert_eq!(quota_caption(&ahead, &ahead.figure["quotas"][0]), "5H »");
        assert_eq!(
            quota_caption(&caught_up, &caught_up.figure["quotas"][0]),
            "5H"
        );
        assert_ne!(
            ahead.draw_key(Open::default()),
            caught_up.draw_key(Open::default())
        );
        // The pace reaches 60% of the window 360 s from now: the `»` goes then.
        let due = ahead.next_change_at(Open::default()).unwrap();
        assert!((due - (now + 360.01)).abs() < 1e-6, "due in {}", due - now);
    }

    /// An agent's age is drawn rounded to the nearest minute, so it moves at half minutes; the
    /// schedule must name those moments, not the whole-minute edges the other ages use.
    #[test]
    fn an_agent_age_is_scheduled_where_its_rounding_changes() {
        let now = 1_800_000_000.;
        let open = Open {
            agents: true,
            ..Open::default()
        };
        let agent = |age: f64| {
            let mut c = Composition::build(&json!({}), &json!({"id":"a"}), now);
            let row = json!({
                "id":"a1","parent":null,"description":"scan","type":"Explore",
                "model":"claude-opus-5","status":"done","started":(now - age) * 1000.,
                "first_seen":(now - age) * 1000.,"last_at":(now - age) * 1000.,"tools":3,
                "tokens":1200,"edits":[],"edits_omitted":0,"overlaps":[],"overlaps_omitted":0,
                "last":"","last_tool":null,"current_turn":null,"completion":null,"listed":true,
            });
            c.agents.agents = vec![serde_json::from_value(row).unwrap()];
            c
        };
        for (age, in_secs) in [(29., 1.), (31., 59.), (0., 30.), (89.5, 0.5), (90.5, 59.5)] {
            let c = agent(age);
            let due = c.next_change_at(open).unwrap();
            assert!(
                (due - (now + in_secs + 0.01)).abs() < 1e-6,
                "age {age}: due in {}",
                due - now
            );
            // And the label really changes there, and not before.
            let label = |at: f64| {
                let mut later = agent(age);
                later.now = at;
                agent_lines(&later)[0].clone()
            };
            assert_ne!(label(due + 0.01), label(now), "age {age}");
            assert_eq!(label(due - 0.02), label(now), "age {age}");
        }
    }

    #[test]
    fn a_fresh_age_reads_under_a_minute_instead_of_ticking_seconds() {
        assert_eq!(age_span(3.), "<1m");
        assert_eq!(age_span(59.), "<1m");
        assert_eq!(age_span(60.), "1m");
    }
}
