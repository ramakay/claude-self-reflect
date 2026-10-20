//! The info page (`i`): what the session is, what CSR cost you this week and how it is wired.
//! pane.py's `info_rows`, `info_heading`, `info_line`, `info_sections`, `info_title`, `info_panel`,
//! `draw_info` and the tapes, chart and key lines under them.
use super::{
    array, infinite, masthead, num,
    pages::{lanes_legend, Lines},
    pyfmt::{round, short},
    render::{
        pictures::{Picture, Wanted},
        raster::{ContextFacts, SessionFacts},
        text::*,
    },
    session::Clock,
    string, truthy, value,
};
use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const SESSION: &str = "This session";
pub const COST: &str = "What CSR cost you this week";
pub const WIRED: &str = "How it's wired";
const INFO_LABEL: usize = 15;
const EMBLEM: usize = 6;
const PANEL_ROWS: usize = 5;
const SLOT: usize = 4;
const PANEL_TAPES: [&str; 3] = ["7d", "5h", "cache"];
const CHART_MIN: usize = 10;
const LANE: usize = 9;
const ICON: (usize, usize) = (5, 2);
const FLOOR_ROWS: usize = 5;
const BAND: (f64, f64) = (180_589., 269_178.);
pub(super) const CACHE_LOW: f64 = 600.;
const QUIET_SWITCHES: [&str; 1] = ["RATIFICATION"];
pub(super) const RAIL: char = '│';
pub(super) const WARM: Rgb = [36, 163, 114];
pub(super) const OCHRE: Rgb = [242, 182, 50];
pub(super) const INDIGO: Rgb = [58, 127, 224];
pub(super) const VERMILION: Rgb = [232, 88, 44];
pub(super) const CHALK: Rgb = [238, 245, 250];
pub(super) const NIGHT: Rgb = [14, 15, 17];

/// One row of the page: a label, its value, the severity it is inked by, a faint note under it, and
/// the instrument on the ramp that already says it (the page leaves the row out while it is drawn).
#[derive(Clone, Debug)]
pub struct Fact {
    pub section: &'static str,
    pub label: String,
    pub value: String,
    pub sev: Option<String>,
    pub note: Option<String>,
    pub shown_by: Option<&'static str>,
}
impl Fact {
    fn new(section: &'static str, label: &str, value: impl Into<String>) -> Self {
        Self {
            section,
            label: label.into(),
            value: value.into(),
            sev: None,
            note: None,
            shown_by: None,
        }
    }
    fn sev(mut self, sev: &str) -> Self {
        self.sev = Some(sev.into());
        self
    }
    fn note(mut self, note: &str) -> Self {
        self.note = Some(note.into());
        self
    }
    fn by(mut self, shown_by: &'static str) -> Self {
        self.shown_by = Some(shown_by);
        self
    }
}

fn put(lines: &mut Lines, rows: Vec<String>) {
    lines.rows.extend(rows);
}
pub(super) fn paper_ink(sev: &str) -> &'static str {
    match sev {
        "advisory" => "ctx",
        "amber" => "dream",
        "red" => "turn",
        _ => "you",
    }
}
fn tokens_said(n: f64) -> String {
    if n >= 1e6 {
        format!("{}M", super::pyfmt::fixed(n / 1e6, 1))
    } else {
        short(n)
    }
}
fn hhmm(at: f64) -> String {
    Local
        .timestamp_opt(at as i64, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}
fn center(text: &str, width: usize) -> String {
    let pad = width.saturating_sub(len(text));
    format!("{}{text}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2))
}
fn reset_text(resets: f64, now: f64) -> String {
    masthead::reset_label(resets, now)
}

/// The "What CSR cost you" facts: each figure the value row can show, said in plain words, the ones
/// at zero left out.
pub fn week_rows(v: &Value) -> Vec<Fact> {
    let mut rows = vec![];
    let cost = value::cost_piece(v);
    if !cost.is_empty() {
        let bound = if v["figure"]["bound"] == true {
            " · an upper bound"
        } else {
            ""
        };
        let how = if v["mode"] == "plan" {
            "7D meter × CSR cost ÷ this machine's Claude spend this plan week"
        } else {
            "CSR's model calls at list price, the last 7 days"
        };
        rows.push(Fact::new(COST, "Fuel", cost).note(&format!("{how}{bound}")));
    }
    let calls = num(&v["calls"]) as i64;
    if calls != 0 {
        rows.push(Fact::new(
            COST,
            "Haiku",
            format!("{}, for summaries", value::plural(calls, "call")),
        ));
    }
    let looks = num(&v["looks"]) as i64;
    if looks != 0 {
        rows.push(Fact::new(
            COST,
            "Dreams",
            format!("{} that ran a model", value::plural(looks, "call")),
        ));
    }
    let (dreams, threads) = (num(&v["dreams"]) as i64, num(&v["threads"]) as i64);
    if dreams != 0 || threads != 0 {
        rows.push(Fact::new(
            COST,
            "You took",
            format!(
                "{}, {}",
                value::plural(dreams, "dream"),
                value::plural(threads, "lost thread")
            ),
        ));
    }
    let likes = num(&v["likes"]) as i64;
    if likes != 0 {
        rows.push(Fact::new(COST, "Liked", value::plural(likes, "row")));
    }
    rows
}

fn gauges_of(data: &Value, s: &Value, now: f64) -> Value {
    masthead::gauges_compacting(
        &data["claude_fields"],
        data["claude_at"].as_f64(),
        s,
        now,
        &data["limits"],
        &data["sts"],
        &data["compacting"],
    )
}

/// The page's facts, section by section, the most useful first.
pub fn info_rows(data: &Value, s: &Value, now: f64, width: usize) -> Vec<Fact> {
    let g = gauges_of(data, s, now);
    let mut rows = vec![];
    if truthy(&data["compacting"]) {
        let lines: Vec<String> = infinite::compact_rows(&data["compacting"], width)
            .iter()
            .map(|row| plain(row).trim().to_owned())
            .collect();
        rows.push(
            Fact::new(SESSION, "Compacting", lines[0].clone()).sev(string(&g["payback"]["sev"])),
        );
        rows.extend(
            lines[1..]
                .iter()
                .map(|line| Fact::new(SESSION, "", line.clone())),
        );
    }
    let ctx = &g["ctx"];
    let main = array(&s["main"]);
    if truthy(&ctx["tokens"]) {
        let measured = array(&s["compact_sizes"])
            .iter()
            .filter(|r| r[1].as_f64().is_some_and(|n| n > 0.))
            .count();
        let size = ctx["size"].as_f64().filter(|n| *n != 0.);
        let pieces = [
            format!(
                "{}{}",
                short(num(&ctx["tokens"])),
                size.map_or(String::new(), |n| format!(" of {}", short(n)))
            ),
            if main.is_empty() {
                String::new()
            } else {
                format!(
                    "peak {}",
                    short(main.iter().map(|r| num(&r[1])).fold(f64::MIN, f64::max))
                )
            },
            format!(
                "floor {} {}",
                short(num(&g["floor"]["tokens"])),
                if g["floor"]["measured"] == true {
                    format!("(last {})", FLOOR_ROWS.min(measured))
                } else {
                    "(guess)".into()
                }
            ),
        ];
        rows.push(
            Fact::new(
                SESSION,
                "Context",
                pieces
                    .into_iter()
                    .filter(|x| !x.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · "),
            )
            .sev(string(&ctx["sev"]))
            .by("context"),
        );
    }
    let prompts = array(&s["prompts"]);
    if !prompts.is_empty() {
        let stamps: Vec<f64> = prompts
            .iter()
            .map(num)
            .chain(array(&s["replies"]).iter().map(|r| num(&r[0])))
            .collect();
        let clock = Clock::new(&stamps, now);
        rows.push(
            Fact::new(
                SESSION,
                "Prompts",
                format!("{} · {} active", prompts.len(), masthead::span(clock.total)),
            )
            .by("prompts"),
        );
    }
    let lasting = &data["infinite"];
    let tally = &lasting["tally"];
    if truthy(tally) {
        let paid = num(&tally["payback"]);
        rows.push(
            Fact::new(
                SESSION,
                "Compactions",
                format!(
                    "{} by CSR, {} · {} by you",
                    num(&tally["by_csr"]) as i64,
                    if paid >= 0. {
                        format!("~{} saved", tokens_said(paid))
                    } else {
                        format!("~{} not yet paid back", short(-paid))
                    },
                    num(&tally["by_you"]) as i64
                ),
            )
            .note("net saving, tokens at full input price, against not compacting"),
        );
    } else if truthy(&lasting["story"]) {
        let story = string(&lasting["story"]);
        rows.push(Fact::new(
            SESSION,
            "Compactions",
            story.strip_prefix("this session: ").unwrap_or(story),
        ));
    } else if !array(&s["compacts"]).is_empty() {
        let text = string(&g["payback"]["text"]);
        let payback = if truthy(&data["compacting"]) || text.is_empty() {
            String::new()
        } else {
            format!(" · {text}")
        };
        rows.push(
            Fact::new(
                SESSION,
                "Compactions",
                format!("{}{payback}", array(&s["compacts"]).len()),
            )
            .by("compacts"),
        );
    }
    if truthy(&lasting["plan"]) {
        let plan = string(&lasting["plan"]);
        rows.push(Fact::new(
            SESSION,
            "Compact plan",
            plan.strip_prefix("Compact planned · ").unwrap_or(plan),
        ));
    }
    if g["cache"]["text"] != "CACHE --" {
        rows.push(
            Fact::new(SESSION, "Cache", string(&g["cache"]["long"]))
                .sev(string(&g["cache"]["sev"]))
                .by("cache"),
        );
    }
    for q in array(&g["quotas"]) {
        let code = string(&q["code"]);
        let label = match code {
            "7D" => "Weekly limit".to_owned(),
            "5H" => "5-hour limit".to_owned(),
            other => format!("{other} limit"),
        };
        if q["stale"] == true {
            rows.push(
                Fact::new(SESSION, &label, "-- · its window rolled over").sev(string(&q["sev"])),
            );
        } else if let Some(used) = q["used"].as_f64() {
            let when = q["resets"]
                .as_f64()
                .filter(|r| *r != 0.)
                .map(|r| reset_text(r, now))
                .unwrap_or_default();
            let shown_by: &'static str = if code == "7D" { "7d" } else { "5h" };
            rows.push(
                Fact::new(
                    SESSION,
                    &label,
                    format!(
                        "{}%{}",
                        round(used) as i64,
                        if when.is_empty() {
                            String::new()
                        } else {
                            format!(" ↻ {when}")
                        }
                    ),
                )
                .sev(string(&q["sev"]))
                .by(shown_by),
            );
        }
    }
    rows.extend(week_rows(&data["value"]));
    rows.push(Fact::new(
        WIRED,
        "Model",
        one_line(
            s["model"]
                .as_str()
                .filter(|m| !m.is_empty())
                .unwrap_or("--"),
            60,
        ),
    ));
    if let Some(line) = data["claude_line"].as_str().filter(|l| !l.is_empty()) {
        rows.push(Fact::new(WIRED, "Status line", line));
    }
    let sts = &g["sts"];
    let inop = array(&sts["inop"]);
    let daemon = &sts["daemon"];
    if daemon["up"] == true {
        let since = daemon["since"]
            .as_f64()
            .filter(|n| *n != 0.)
            .map_or(String::new(), |at| format!(" since {}", hhmm(at)));
        rows.push(Fact::new(
            WIRED,
            "Daemon",
            format!("up{since} · pid {}", num(&daemon["pid"]) as i64),
        ));
    }
    for item in inop.iter().filter(|i| i[0] == "DAEMON") {
        rows.push(
            Fact::new(
                WIRED,
                "Daemon",
                string(&item[1]).replacen("DOWN", "down", 1),
            )
            .sev(string(&item[2])),
        );
    }
    let told = &s["told"];
    if truthy(told) {
        rows.push(Fact::new(
            WIRED,
            "Recap",
            format!(
                "injected {} ({})",
                hhmm(num(&told["at"])),
                string(&told["source"])
            ),
        ));
    } else if !s
        .get("transcript")
        .map_or(truthy(&s["prompts"]) || truthy(&s["replies"]), truthy)
    {
        rows.push(Fact::new(WIRED, "Recap", "after your first prompt"));
    } else {
        rows.push(Fact::new(WIRED, "Recap", "none this session"));
    }
    let switches: Vec<_> = inop
        .iter()
        .filter(|i| i[0] != "DAEMON" && !QUIET_SWITCHES.contains(&string(&i[0])))
        .collect();
    if !switches.is_empty() {
        let names: Vec<String> = switches
            .iter()
            .map(|i| string(&i[0]).to_lowercase().replace('_', " "))
            .collect();
        static WHERE: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"\((\w+)\)$").unwrap());
        let notes: Vec<String> = switches
            .iter()
            .map(|i| {
                let place = WHERE
                    .captures(string(&i[1]))
                    .map(|c| match &c[1] {
                        "daemon" => "the daemon's LaunchAgent".to_owned(),
                        "hooks" => "settings.json's env".to_owned(),
                        other => other.to_owned(),
                    })
                    .unwrap_or_else(|| "its env".into());
                format!("CSR_NO_{} in {place}", string(&i[0]))
            })
            .collect();
        let worst = switches
            .iter()
            .map(|i| string(&i[2]))
            .max_by_key(|sev| ["advisory", "amber"].iter().position(|x| x == sev))
            .unwrap_or("advisory");
        rows.push(
            Fact::new(WIRED, "Switched off", names.join(", "))
                .sev(worst)
                .note(&notes.join("; ")),
        );
    }
    rows
}

/// A section's head: a hairline the width of the page over its name in bold capitals; the first
/// section's rule is the ramp's own edge, a blank row in its place.
pub fn info_heading(section: &str, width: usize, first: bool) -> Vec<String> {
    vec![
        if first {
            String::new()
        } else {
            format!(
                " {}{}{OFF}",
                ink("faint"),
                "─".repeat(width.saturating_sub(1).max(1))
            )
        },
        format!(
            " {BOLD}{}{}{OFF}",
            ink("you"),
            fit(&section.to_uppercase(), width.saturating_sub(1))
        ),
    ]
}

/// One fact as rows: its label in pencil in the label column, its value in ink on one line where it
/// fits (wrapped under itself to three rows where it does not), its note faint under the value.
pub fn info_line(fact: &Fact, width: usize) -> Vec<String> {
    let room = width.saturating_sub(INFO_LABEL).max(4);
    let colour = fact
        .sev
        .as_deref()
        .map_or_else(|| ink("you"), |sev| ink(paper_ink(sev)));
    let indent = " ".repeat(INFO_LABEL);
    let head = if fact.label.is_empty() {
        indent.clone()
    } else {
        let label = fit(&fact.label, INFO_LABEL - 3);
        format!(
            " {}{label}{}{OFF}",
            ink("quiet"),
            " ".repeat((INFO_LABEL - 1).saturating_sub(len(&label)))
        )
    };
    let mut rows = if fact.label == "Status line" {
        vec![format!("{head}{}{OFF}", ansi_fit(&fact.value, room))]
    } else {
        let pieces = wrap(&one_line(&fact.value, 400), room, 3);
        let mut rows = vec![format!("{head}{colour}{}{OFF}", pieces[0])];
        rows.extend(
            pieces[1..]
                .iter()
                .map(|p| format!("{indent}{colour}{p}{OFF}")),
        );
        rows
    };
    if let Some(note) = &fact.note {
        rows.extend(
            wrap(&one_line(note, 400), room, 2)
                .iter()
                .map(|p| format!("{indent}{}{p}{OFF}", ink("faint"))),
        );
    }
    rows
}

type LanePictures = Vec<(usize, usize, usize, usize, Wanted)>;

/// The page under the ramp: each section that has a fact (or, for This session, the lanes) as its
/// heading and its rows. A fact an instrument shows is left out.
pub fn info_sections(
    facts: &[Fact],
    width: usize,
    shown: &BTreeSet<&'static str>,
    lanes: &(Vec<String>, LanePictures),
) -> (Lines, LanePictures) {
    let mut out = Lines::default();
    let mut pictures: LanePictures = vec![];
    for section in [SESSION, COST, WIRED] {
        let rows: Vec<&Fact> = facts
            .iter()
            .filter(|f| f.section == section && !f.shown_by.is_some_and(|b| shown.contains(b)))
            .collect();
        let mut lead = 0;
        while lead < rows.len()
            && (rows[lead].label == "Compacting" || (lead > 0 && rows[lead].label.is_empty()))
        {
            lead += 1;
        }
        let has_lanes = section == SESSION && !lanes.0.is_empty();
        if rows.is_empty() && !has_lanes {
            continue;
        }
        let first = out.rows.is_empty();
        put(&mut out, info_heading(section, width, first));
        for fact in &rows[..lead] {
            put(&mut out, info_line(fact, width));
        }
        if has_lanes {
            let base = out.rows.len();
            pictures.extend(
                lanes
                    .1
                    .iter()
                    .map(|(r, c, w, h, p)| (r + base, *c, *w, *h, p.clone())),
            );
            put(&mut out, lanes.0.clone());
        }
        for fact in &rows[lead..] {
            put(&mut out, info_line(fact, width));
        }
    }
    (out, pictures)
}

/// The page's title: whose session, and since when.
pub fn info_title(s: &Value) -> String {
    let mut stamps: Vec<f64> = array(&s["prompts"])
        .iter()
        .map(num)
        .filter(|t| *t != 0.)
        .chain(
            array(&s["replies"])
                .iter()
                .map(|r| num(&r[0]))
                .filter(|t| *t != 0.),
        )
        .chain(
            array(&s["main"])
                .iter()
                .map(|r| num(&r[0]))
                .filter(|t| *t != 0.),
        )
        .collect();
    stamps.sort_by(f64::total_cmp);
    let since = stamps.first().map_or(String::new(), |at| {
        Local
            .timestamp_opt(*at as i64, 0)
            .single()
            .map(|t| t.format("%a %H:%M").to_string())
            .unwrap_or_default()
    });
    let id: String = string(&s["id"]).chars().take(8).collect();
    [
        "Session info".to_owned(),
        id,
        if since.is_empty() {
            String::new()
        } else {
            format!("since {since}")
        },
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Paint {
    None,
    Rail,
    Rgb(Rgb),
}
pub(super) type Cell = (char, Paint, Option<Rgb>);

/// A session's context as `height` rows (top first) of one cell a slice: each slice's highest
/// context in eighths over 0..CTX_RED, in its heat, the part under the floor dimmed.
fn chart_cells(peaks: &[f64], floor: f64, height: usize) -> Vec<Vec<Cell>> {
    let total = (height * 8) as f64;
    let mut rows: Vec<Vec<Cell>> = vec![vec![]; height];
    let f = if floor.is_finite() {
        round((floor / masthead::ctx_red()).clamp(0., 1.) * total)
    } else {
        0.
    };
    for &v in peaks {
        if v == 0. {
            for row in rows.iter_mut() {
                row.push((' ', Paint::None, None));
            }
            continue;
        }
        let h = round((v / masthead::ctx_red()).min(1.) * total).clamp(1., total);
        let bright = heat(v);
        let dim = blend(bright, NIGHT, 0.55);
        for (row, cells) in rows.iter_mut().enumerate() {
            let base = (8 * (height - 1 - row)) as f64;
            let fill = (h - base).clamp(0., 8.) as usize;
            let under = (f - base).clamp(0., 8.) as usize;
            cells.push(if fill == 0 {
                (' ', Paint::None, None)
            } else if under >= fill {
                (EIGHTHS[fill], Paint::Rgb(dim), None)
            } else if under == 0 {
                (EIGHTHS[fill], Paint::Rgb(bright), None)
            } else if fill == 8 {
                (EIGHTHS[under], Paint::Rgb(dim), Some(bright))
            } else {
                (EIGHTHS[fill], Paint::Rgb(bright), None)
            });
        }
    }
    rows
}

struct Series {
    peaks: Vec<f64>,
    ticks: BTreeSet<usize>,
    clock: Clock,
}
/// The session's context squeezed of idle gaps as the lanes draw it, for an `n`-slice chart.
fn session_series(s: &Value, n: usize, now: f64) -> Option<Series> {
    let main: Vec<(f64, f64)> = array(&s["main"])
        .iter()
        .map(|r| (num(&r[0]), num(&r[1])))
        .filter(|(t, c)| *t != 0. && *c != 0.)
        .collect();
    if main.is_empty() || n < 1 {
        return None;
    }
    let stamps: Vec<f64> = array(&s["prompts"])
        .iter()
        .map(num)
        .chain(array(&s["replies"]).iter().map(|r| num(&r[0])))
        .chain(main.iter().map(|r| r.0))
        .collect();
    let clock = Clock::new(&stamps, now);
    let at = |t: f64| (n - 1).min((clock.at(t) * n as f64) as usize);
    let mut peaks = vec![0.; n];
    for (t, c) in &main {
        peaks[at(*t)] = f64::max(peaks[at(*t)], *c);
    }
    let ticks = array(&s["compacts"]).iter().map(|t| at(num(t))).collect();
    Some(Series {
        peaks,
        ticks,
        clock,
    })
}

pub(super) fn level_row(share: Option<f64>, height: usize) -> usize {
    let share = share.filter(|n| n.is_finite()).unwrap_or(0.);
    height - 1 - (height - 1).min((share.clamp(0., 1.) * height as f64) as usize)
}
pub(super) fn bug_cell(fill: usize, paint: Rgb) -> Cell {
    ('━', Paint::Rgb(CHALK), (fill >= 4).then_some(paint))
}
pub(super) fn quota_tape(q: Option<&Value>, height: usize) -> Vec<Cell> {
    let Some(used) = q.and_then(|q| q["used"].as_f64()) else {
        return vec![(RAIL, Paint::Rail, None); height];
    };
    let q = q.unwrap();
    let paint = match string(&q["sev"]) {
        "amber" => OCHRE,
        "red" => VERMILION,
        _ => INDIGO,
    };
    let levels = fill_levels(used / 100., height);
    let bug = q["pace"].as_f64().map(|p| level_row(Some(p), height));
    levels
        .iter()
        .enumerate()
        .map(|(row, fill)| {
            if Some(row) == bug {
                bug_cell(*fill, paint)
            } else if *fill > 0 {
                (EIGHTHS[*fill], Paint::Rgb(paint), None)
            } else {
                (RAIL, Paint::Rail, None)
            }
        })
        .collect()
}
pub(super) fn cache_tape(cache: &Value, height: usize) -> Vec<Cell> {
    let (left, ttl) = (num(&cache["left"]), num(&cache["ttl"]));
    if cache["cold"] == true || left == 0. || ttl == 0. {
        return vec![(RAIL, Paint::Rail, None); height];
    }
    let paint = if left >= CACHE_LOW { WARM } else { OCHRE };
    fill_levels(left / ttl, height)
        .iter()
        .map(|fill| {
            if *fill > 0 {
                (EIGHTHS[*fill], Paint::Rgb(paint), None)
            } else {
                (RAIL, Paint::Rail, None)
            }
        })
        .collect()
}
pub(super) fn cell_text(cell: &Cell, ground: Rgb) -> String {
    let (glyph, paint, back) = cell;
    let ink = match paint {
        Paint::Rail => Some(blend(ground, CHALK, 0.24)),
        Paint::Rgb(c) => Some(*c),
        Paint::None => None,
    };
    format!(
        "{}{}{glyph}",
        bg(back.unwrap_or(ground)),
        ink.map_or(String::new(), fg)
    )
}
/// A tape cell two columns wide for the page: the rail as a thin line at its middle.
fn tape_pair(cell: &Cell, ground: Rgb) -> String {
    let (glyph, paint, back) = cell;
    if *paint == Paint::Rail {
        return format!("{}{}▕▏", bg(ground), fg(blend(ground, CHALK, 0.24)));
    }
    format!(
        "{}{}{}",
        bg(back.unwrap_or(ground)),
        match paint {
            Paint::Rgb(c) => fg(*c),
            _ => String::new(),
        },
        glyph.to_string().repeat(2)
    )
}
fn panel_value(kind: &str, g: &Value) -> (String, String) {
    if kind == "cache" {
        let cache = &g["cache"];
        if cache["cold"] == true {
            return ("cold".into(), "amber".into());
        }
        let left = num(&cache["left"]);
        if left == 0. {
            return ("--".into(), "normal".into());
        }
        let text = if left < 6000. {
            format!("{}m", (left / 60.).floor() as i64)
        } else {
            format!("{}h", round(left / 3600.) as i64)
        };
        return (text, string(&cache["sev"]).into());
    }
    let code = if kind == "7d" { "7D" } else { "5H" };
    let q = array(&g["quotas"]).iter().find(|q| q["code"] == code);
    match q.and_then(|q| q["used"].as_f64()) {
        None => (
            "--".into(),
            q.map_or("normal", |q| string(&q["sev"])).into(),
        ),
        Some(used) => (
            format!("{}%", round(used) as i64),
            string(&q.unwrap()["sev"]).into(),
        ),
    }
}
fn panel_label(kind: &str) -> &'static str {
    match kind {
        "7d" => "7D",
        "5h" => "5H",
        _ => "◷",
    }
}

type RowFn = Box<dyn Fn(Rgb) -> String>;
struct Figure {
    row: usize,
    cols: usize,
    rows: usize,
}
struct Panel {
    rows: Vec<RowFn>,
    shows: BTreeSet<&'static str>,
    figure: Option<Figure>,
}
/// The instruments under the title on the dark ramp: a row of the session's counts, the whole
/// session's context as a chart PANEL_ROWS tall with the 7D, 5H and cache tapes beside it, the
/// compactions ticked and the time named under it, each tape's value and label under it.
fn info_panel(s: &Value, g: &Value, width: usize, now: f64, pixels: bool) -> Panel {
    let none = Panel {
        rows: vec![],
        shows: BTreeSet::new(),
        figure: None,
    };
    let tapes_n = width.saturating_sub(1 + 1 + SLOT * PANEL_TAPES.len());
    let tapes = tapes_n >= CHART_MIN;
    let n = if tapes {
        tapes_n
    } else {
        width.saturating_sub(2)
    };
    let Some(series) = (n >= CHART_MIN)
        .then(|| session_series(s, n, now))
        .flatten()
    else {
        return none;
    };
    let mut shows: BTreeSet<&'static str> = if tapes {
        ["cache", "7d", "5h"].into()
    } else {
        BTreeSet::new()
    };
    for q in array(&g["quotas"]).iter().filter(|q| q["stale"] == true) {
        shows.remove(if q["code"] == "7D" { "7d" } else { "5h" });
    }
    let counts = [
        format!("{} prompts", array(&s["prompts"]).len()),
        format!("{} active", masthead::span(series.clock.total)),
        format!("{} compacts", array(&s["compacts"]).len()),
        if truthy(&g["compacting"]) {
            String::new()
        } else {
            string(&g["payback"]["text"]).into()
        },
    ];
    let said = fit_pieces(
        &counts
            .iter()
            .filter(|x| !x.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · "),
        width - 1,
    );
    if said.contains(&counts[0]) {
        shows.insert("prompts");
    }
    if said.contains(&counts[2]) {
        shows.insert("compacts");
    }
    let mut rows: Vec<RowFn> = vec![];
    if !said.is_empty() {
        let said = said.clone();
        rows.push(Box::new(move |_| {
            format!(" {}{said}{OFF}", fg(sev_rgb("data")))
        }));
    }
    let (figure, chart) = if pixels {
        (
            Some(Figure {
                row: rows.len(),
                cols: n,
                rows: PANEL_ROWS,
            }),
            vec![vec![(' ', Paint::None, None); n]; PANEL_ROWS],
        )
    } else {
        (
            None,
            chart_cells(&series.peaks, num(&g["floor"]["tokens"]), PANEL_ROWS),
        )
    };
    let quota_of = |kind: &str| {
        array(&g["quotas"])
            .iter()
            .find(|q| q["code"] == panel_label(kind))
    };
    let columns: Vec<(&'static str, Vec<Cell>)> = if tapes {
        PANEL_TAPES
            .iter()
            .map(|kind| {
                (
                    *kind,
                    if *kind == "cache" {
                        cache_tape(&g["cache"], PANEL_ROWS)
                    } else {
                        quota_tape(quota_of(kind), PANEL_ROWS)
                    },
                )
            })
            .collect()
    } else {
        vec![]
    };
    for k in 0..PANEL_ROWS {
        let chart_row = chart[k].clone();
        let tape_cells: Vec<Cell> = columns.iter().map(|(_, cells)| cells[k]).collect();
        rows.push(Box::new(move |ground| {
            let mut out = format!(
                " {}",
                chart_row
                    .iter()
                    .map(|c| cell_text(c, ground))
                    .collect::<String>()
            );
            if !tape_cells.is_empty() {
                out.push_str(&format!("{} ", bg(ground)));
                for cell in &tape_cells {
                    out.push_str(&format!(
                        "{} {}{} ",
                        bg(ground),
                        tape_pair(cell, ground),
                        bg(ground)
                    ));
                }
            }
            out.push_str(OFF);
            out
        }));
    }
    let tick_row: String = if figure.is_some() {
        " ".repeat(n)
    } else {
        (0..n)
            .map(|i| {
                if series.ticks.contains(&i) {
                    format!("{}╹", fg(sev_rgb("amber")))
                } else {
                    " ".into()
                }
            })
            .collect()
    };
    let values: Vec<(String, String)> = columns
        .iter()
        .map(|(kind, _)| panel_value(kind, g))
        .collect();
    let has_columns = !columns.is_empty();
    rows.push(Box::new(move |_| {
        let mut out = format!(" {tick_row}{OFF}");
        if has_columns {
            out.push(' ');
            for (text, sev) in &values {
                out.push_str(&slot(&center(text, SLOT), sev));
            }
        }
        out
    }));
    let first = Local
        .timestamp_opt(series.clock.begin as i64, 0)
        .single()
        .map(|t| t.format("%a %H:%M").to_string())
        .unwrap_or_default();
    let last = "now";
    let axis = if len(&first) + len(last) < n {
        format!(
            "{first}{}{last}",
            " ".repeat((n - len(&first) - len(last)).max(1))
        )
    } else {
        fit(&first, n)
    };
    let labels: Vec<&'static str> = columns.iter().map(|(kind, _)| panel_label(kind)).collect();
    rows.push(Box::new(move |_| {
        let mut out = format!(" {}{axis}{OFF}", fg(sev_rgb("normal")));
        if !labels.is_empty() {
            out.push(' ');
            for label in &labels {
                out.push_str(&slot(&center(label, SLOT), "normal"));
            }
        }
        out
    }));
    Panel {
        rows,
        shows,
        figure,
    }
}

/// (handed, opened, asked) this session: the hand-offs CSR made to Claude, how many Claude opened,
/// and the questions Claude asked CSR itself.
fn handed_counts(s: &Value) -> (usize, usize, usize) {
    let given = if truthy(&s["gave"]) {
        array(&s["gave"]["recalls"])
            .iter()
            .filter_map(Value::as_f64)
            .filter(|t| *t != 0.)
            .map(|t| t as i64)
            .collect::<BTreeSet<_>>()
            .len()
            + array(&s["gave"]["carried"]).len()
    } else {
        0
    };
    let items = array(&s["remembered"]);
    (
        given,
        items.iter().filter(|i| truthy(&i["opened"])).count(),
        items.iter().filter(|i| i["kind"] != "recall").count(),
    )
}
/// The page's one line on what CSR handed Claude: 'CSR handed 27 · opened 0 · Claude asked 8×'.
fn handed_line(s: &Value) -> String {
    let (given, opened, asked) = handed_counts(s);
    if given == 0 && opened == 0 && asked == 0 {
        return String::new();
    }
    [
        format!("CSR handed {given}"),
        format!("opened {opened}"),
        if asked > 0 {
            format!("Claude asked {asked}×")
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}
/// One line on the pace and the tapes' marks, shorter wordings before any is cut.
fn tape_key(width: usize) -> String {
    let (amber, hot) = (short(masthead::ctx_amber()), short(masthead::ctx_hot()));
    let forms = [
        format!("» ahead of pace · ━ pace (7·5) or floor (C) · ─ {amber} / {hot}"),
        format!("» ahead of pace · ━ pace or floor · ─ {amber} / {hot}"),
        format!("» ahead of pace · ━ pace/floor · ─ {amber}/{hot}"),
        format!("» ahead · ━ pace/floor · ─ {amber}/{hot}"),
        "» ahead of pace · ━ pace/floor".to_owned(),
        "» ahead · ━ pace/floor".to_owned(),
    ];
    forms
        .iter()
        .find(|f| len(f) <= width)
        .cloned()
        .unwrap_or_else(|| fit_pieces(&forms[5], width))
}

/// The rows of a page laid out tall that the screen shows from `top`; a picture the edge would cut
/// is left out. (shown rows, kept pictures, top).
fn window(
    lines: Vec<String>,
    pictures: Vec<(usize, Wanted)>,
    rows: usize,
    top: usize,
) -> (Vec<String>, Vec<(usize, Wanted)>, usize) {
    let room = rows.saturating_sub(1);
    let top = top.min(lines.len().saturating_sub(room));
    let shown = lines.into_iter().skip(top).take(room).collect();
    let kept = pictures
        .into_iter()
        .filter(|(row, w)| *row >= top && row - top + w.rows <= room)
        .map(|(row, w)| (row - top, w))
        .collect();
    (shown, kept, top)
}

/// The i page: its title and the context on the dark ramp (their first EMBLEM columns kept for the
/// emblem), then the instruments on the ramp below, then on paper the at-a-glance sections. The
/// title rows stay; the rest scrolls by `top`. (lines, top).
pub fn draw_info(
    data: &Value,
    s: &Value,
    cols: usize,
    rows: usize,
    top: usize,
    now: f64,
    pixels: bool,
) -> (Lines, usize) {
    let width = cols.saturating_sub(1).max(24);
    let g = gauges_of(data, s, now);
    let mut title = info_title(s);
    let id: String = string(&s["id"]).chars().take(8).collect();
    if EMBLEM + len(&title) > width {
        title = title
            .split(" · ")
            .filter(|p| *p != id)
            .collect::<Vec<_>>()
            .join(" · ");
    }
    if EMBLEM + len(&title) > width {
        title = title.replace(" · since ", " · ");
    }
    let name: String = title.chars().take(len("Session info")).collect();
    let rest: String = title.chars().skip(len("Session info")).collect();
    let mut head = vec![format!(
        "{}{BOLD}{name}{OFF}{}{}{OFF}",
        " ".repeat(EMBLEM),
        fg(sev_rgb("normal")),
        fit(
            &rest,
            width.saturating_sub(EMBLEM + len("Session info")).max(1)
        )
    )];
    let ctx = &g["ctx"];
    let main = array(&s["main"]);
    let mut shows: BTreeSet<&'static str> = BTreeSet::new();
    if truthy(&ctx["tokens"]) {
        let measured = array(&s["compact_sizes"])
            .iter()
            .filter(|r| r[1].as_f64().is_some_and(|n| n > 0.))
            .count();
        let size = ctx["size"].as_f64().filter(|n| *n != 0.);
        let pieces = [
            format!(
                "{}{}",
                short(num(&ctx["tokens"])),
                size.map_or(String::new(), |n| format!(" of {}", short(n)))
            ),
            if main.is_empty() {
                String::new()
            } else {
                format!(
                    "peak {}",
                    short(main.iter().map(|r| num(&r[1])).fold(f64::MIN, f64::max))
                )
            },
            format!(
                "floor {} {}",
                short(num(&g["floor"]["tokens"])),
                if g["floor"]["measured"] == true {
                    format!("(last {})", FLOOR_ROWS.min(measured))
                } else {
                    "(guess)".into()
                }
            ),
        ]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        head.push(format!(
            "{}{}CONTEXT {}",
            " ".repeat(EMBLEM),
            fg(sev_rgb("normal")),
            slot(
                &fit_pieces(&pieces, width.saturating_sub(EMBLEM + 8)),
                "data"
            )
        ));
        shows.insert("context");
    } else {
        head.push(String::new());
    }
    let panel = info_panel(s, &g, width, now, pixels);
    shows.extend(panel.shows.iter().copied());
    let grounds = dark_steps(head.len() + panel.rows.len());
    let mut out = Lines {
        rows: head
            .iter()
            .zip(&grounds)
            .map(|(line, ground)| on_dark(line, *ground))
            .collect(),
        ..Lines::default()
    };
    let mut body: Vec<String> = panel
        .rows
        .iter()
        .zip(&grounds[head.len()..])
        .map(|(row, ground)| on_dark(&row(*ground), *ground))
        .collect();
    let mut placed: Vec<(usize, Wanted)> = vec![];
    if let Some(figure) = &panel.figure {
        let series: Vec<(f64, f64)> = array(&s["main"])
            .iter()
            .map(|r| (num(&r[0]), num(&r[1])))
            .filter(|(t, c)| *t != 0. && *c != 0.)
            .collect();
        if series.len() >= 2 {
            let from = head.len() + figure.row;
            let mut compacts: Vec<f64> = array(&s["compacts"]).iter().map(num).collect();
            compacts.sort_by(f64::total_cmp);
            placed.push((
                figure.row,
                Wanted {
                    row: 0,
                    col: 1,
                    cols: figure.cols,
                    rows: figure.rows,
                    picture: Picture::Context(ContextFacts {
                        series,
                        prompts: array(&s["prompts"])
                            .iter()
                            .map(num)
                            .chain(array(&s["replies"]).iter().map(|r| num(&r[0])))
                            .collect(),
                        compacts,
                        csr_at: array(&data["infinite"]["csr_at"]).iter().map(num).collect(),
                        floor: num(&g["floor"]["tokens"]),
                        now,
                        band: BAND,
                    }),
                    backs: grounds[from..from + figure.rows].to_vec(),
                },
            ));
        }
    }
    let mut lanes: Vec<String> = vec![];
    let mut drawn: LanePictures = vec![];
    if truthy(&s["prompts"]) || truthy(&s["replies"]) {
        if pixels && width.saturating_sub(LANE) > 8 && truthy(&s["learned"]) {
            drawn.push((
                lanes.len(),
                LANE,
                width - LANE,
                3,
                Wanted {
                    row: 0,
                    col: LANE,
                    cols: width - LANE,
                    rows: 3,
                    picture: Picture::Session(SessionFacts::from_value(s, now)),
                    backs: vec![],
                },
            ));
            lanes.extend(
                ["you", "context", "memory"]
                    .iter()
                    .map(|name| format!(" {}{name:<w$}{OFF}", ink("faint"), w = LANE - 1)),
            );
        }
        let suffix = " · this session";
        let legend = if truthy(&s["learned"]) {
            lanes_legend(s, width.saturating_sub(2 + len(suffix)), false)
        } else {
            String::new()
        };
        if !legend.is_empty() {
            let fits = 2 + len(&plain(&legend)) + len(suffix) <= width;
            lanes.push(ansi_fit(
                &format!(
                    "  {legend}{}",
                    if fits {
                        format!("{}{suffix}{OFF}", ink("quiet"))
                    } else {
                        String::new()
                    }
                ),
                width,
            ));
        }
        let handed = handed_line(s);
        if !handed.is_empty() {
            lanes.push(format!(
                " {}{}{OFF}",
                ink("faint"),
                fit_pieces(&handed, width.saturating_sub(2))
            ));
        }
    }
    let facts = info_rows(data, s, now, width.saturating_sub(INFO_LABEL).max(8));
    let (page, page_pictures) = info_sections(&facts, width, &shows, &(lanes, drawn));
    let base = body.len();
    let mut pictures = placed;
    pictures.extend(page_pictures.into_iter().map(|(r, _, _, _, mut w)| {
        w.row = r;
        (r + base, w)
    }));
    body.extend(page.rows);
    let key = format!(
        "{} · ♻ compact payback · ↻ resets · STS status · ◷ cache",
        if panel.figure.is_some() {
            "─ context · ▬ your band · ╷ compacted, red by CSR".to_owned()
        } else {
            format!("{}▁▃▆ context per turn", if pixels { "" } else { "CTX " })
        }
    );
    body.extend([
        String::new(),
        format!(
            " {}{}{OFF}",
            ink("faint"),
            fit_pieces(&key, width.saturating_sub(2))
        ),
        format!(
            " {}{}{OFF}",
            ink("faint"),
            tape_key(width.saturating_sub(2))
        ),
    ]);
    let panel_len = panel.rows.len();
    let (shown, kept, top) = window(body, pictures, rows.saturating_sub(out.rows.len()), top);
    out.rows.extend(shown);
    let back = json!({"key": "view:main"});
    for r in 0..(head.len() + panel_len.saturating_sub(top)).min(out.rows.len()) {
        out.acts.insert(r, back.clone());
    }
    for (row, mut wanted) in kept {
        wanted.row = row + head.len();
        out.pictures.push(wanted);
    }
    if pixels && head.len() >= ICON.1 {
        out.pictures.insert(
            0,
            Wanted {
                row: 0,
                col: 0,
                cols: ICON.0,
                rows: ICON.1,
                picture: Picture::Emblem,
                backs: grounds[..ICON.1].to_vec(),
            },
        );
    }
    (out, top)
}
