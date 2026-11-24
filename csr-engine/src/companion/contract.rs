//! The pane contract: the cards the herdr pane draws, worded once, for any other pane to draw.
//!
//! `companion pane --json` carries `schema`; `--json --cards` adds `cards`, the context and the
//! CSR row as data, and `herdr_pane_live`. Every word here comes from the functions the herdr pane's own renderers call:
//! the instrument's card helpers, `forecast`'s headline, `same_page`'s numbers and words. Where the
//! herdr painter built a string inline, the string-building lives here now and the painter calls
//! it, so the two panes cannot word a card differently.
use super::render::instrument::{
    self, observation_text, shift_row, story_title, Batch, Composition, SHOULD_BE_TITLE,
};
use super::{agents, array, num, pyfmt::short, string};
use serde::Serialize;

/// The `--json` shape's major version. A consumer that meets a higher one draws what it can.
pub const SCHEMA: u32 = 1;

/// One line of a card: its words and how they are drawn.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Line {
    pub text: String,
    /// `body`, `dim`, `gold`, `mint`, `code` or `caption`.
    pub style: &'static str,
}

/// A bar of a card: what it measures, how full (0..1), its figure in words, and its state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub label: String,
    pub value: f64,
    pub text: String,
    /// `ok`, `amber` or `red`.
    pub tone: String,
}

/// The dreamer's token budget: spent of cap, and whether a look runs now (the ring turns).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ring {
    pub spent: f64,
    pub cap: f64,
    pub turning: bool,
}

/// One finished card, in the herdr pane's words.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Card {
    /// `dreamer`, `same_page`, `should_be`, `story`, `limits`, `agents`.
    pub id: &'static str,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    pub lines: Vec<Line>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ring: Option<Ring>,
}

fn line(text: impl Into<String>, style: &'static str) -> Line {
    Line {
        text: text.into(),
        style,
    }
}

// ---- words the herdr painter used to build inline -------------------------------------------------

/// The dreamer card's title.
pub fn dreamer_title(c: &Composition) -> &'static str {
    if c.offer {
        "Dreams are off"
    } else if c.dream.state == Batch::Ready {
        if c.asked_away {
            "The dreamer asks · asked while you were away"
        } else {
            "The dreamer asks"
        }
    } else {
        "Self-Reflect's dreamer"
    }
}

/// The dreamer's state on its title row, in the dreamer's colour; none with a question up.
pub fn dreamer_tag(c: &Composition) -> Option<&'static str> {
    match c.dream.state {
        Batch::Ready => None,
        _ if c.offer => None,
        _ if super::dreams::says_stopped(&c.offer_notice) => Some(super::dreams::STOPPED_TAG),
        Batch::Dreaming => Some("Dreaming"),
        _ => Some("Looks as you work"),
    }
}

/// The limits card's headline and its state: the forecast's sentence, or the API credit's when it
/// runs out within days and warns at least as loudly.
pub fn headline(c: &Composition) -> Option<(String, String)> {
    let g = &c.figure;
    let mut head = g["outlook"]["headline"]["text"].as_str().map(|text| {
        (
            text.to_owned(),
            string(&g["outlook"]["headline"]["sev"]).to_owned(),
        )
    });
    let rank = |sev: &str| match sev {
        "red" => 2,
        "amber" => 1,
        _ => 0,
    };
    if let Some((text, sev)) = c.credit.as_ref().and_then(|credit| credit.headline()) {
        if head.as_ref().is_none_or(|(_, had)| rank(sev) >= rank(had)) {
            head = Some((text, sev.to_owned()));
        }
    }
    head
}

/// One limit window as the limits card draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub code: String,
    pub name: String,
    pub used: f64,
    pub sev: String,
    pub tick: Option<f64>,
    pub line: String,
}

/// The limits card's windows: the forecast's, or with no outlook the bare quotas, with no pace.
pub fn limits(c: &Composition) -> Vec<Limit> {
    let g = &c.figure;
    if let Some(windows) = g["outlook"]["windows"].as_array() {
        return windows
            .iter()
            .map(|w| Limit {
                code: string(&w["code"]).to_owned(),
                name: string(&w["name"]).to_owned(),
                used: num(&w["used"]),
                sev: string(&w["sev"]).to_owned(),
                tick: w["tick"].as_f64(),
                line: string(&w["line"]).to_owned(),
            })
            .collect();
    }
    let mut quotas: Vec<_> = array(&g["quotas"])
        .iter()
        .filter(|q| q["used"].is_number())
        .collect();
    quotas.sort_by_key(|q| if q["code"] == "5H" { 0 } else { 1 });
    quotas
        .into_iter()
        .map(|q| {
            let used = num(&q["used"]);
            let left = q["resets"]
                .as_f64()
                .map(|at| format!(" · resets in {}", super::forecast::until(at - c.now)))
                .unwrap_or_default();
            Limit {
                code: string(&q["code"]).to_owned(),
                name: if q["code"] == "5H" {
                    "5 Hour"
                } else {
                    "Weekly"
                }
                .to_owned(),
                used,
                sev: if used >= 90. {
                    "red"
                } else if used >= 75. {
                    "amber"
                } else {
                    "ok"
                }
                .to_owned(),
                tick: None,
                line: format!("{}% used{left}", used.round() as i64),
            }
        })
        .collect()
}

/// The Codex weekly bar: its percent and its line.
pub fn codex(c: &Composition) -> Option<(f64, String)> {
    c.codex_week["percent"].as_f64().map(|n| {
        let left = c.codex_week["resets_at"]
            .as_f64()
            .filter(|at| *at > c.now)
            .map(|at| format!(" · resets in {}", super::forecast::until(at - c.now)))
            .unwrap_or_default();
        (n, format!("Codex weekly limit{left}"))
    })
}

/// What Self-Reflect did for this session, one part a measured value: (number, label, fill).
pub fn csr_parts(c: &Composition) -> Vec<(String, String, Option<f64>)> {
    let mut out = vec![];
    if let Some(n) = c.tally["by_csr"].as_u64().filter(|n| *n > 0) {
        out.push((
            n.to_string(),
            format!("{n} compaction{}", if n == 1 { "" } else { "s" }),
            None,
        ));
    }
    if let Some(n) = c.tally["payback"].as_f64().filter(|n| *n > 0.) {
        // The tally is requests saved times the per-request saving, minus the compaction's own
        // cost (compact_story). It models the cache; it is not a count, so it says est.
        out.push((
            format!("~{}", short(n)),
            format!("~{} tokens saved (est.)", short(n)),
            None,
        ));
    }
    if let Some(n) = c.figure["asks"].as_u64().filter(|n| *n > 0) {
        out.push((
            n.to_string(),
            format!("{n} recall{} used", if n == 1 { "" } else { "s" }),
            None,
        ));
    }
    if let Some(share) = c.csr_cost.share.filter(|s| *s > 0.) {
        let label = c.csr_cost.share_label();
        // The share of the weekly limit is a real fraction of a real ring.
        out.push((
            label.clone(),
            format!("{label} pts of the weekly limit"),
            Some(share / 100.),
        ));
    }
    out
}

/// The CSR row's lead words.
pub const CSR_LEAD: &str = "Self-Reflect this session:";

/// The agents row: how many run and how many stalled; none when none runs.
pub fn agents_line(c: &Composition) -> Option<String> {
    let active: Vec<_> = c
        .agents
        .agents
        .iter()
        .filter(|a| agents::is_active(a, c.now))
        .collect();
    (!active.is_empty()).then(|| {
        format!(
            "{} in flight · {} stalled",
            active.len(),
            active.iter().filter(|a| agents::stalled(a, c.now)).count()
        )
    })
}

// ---- the cards -----------------------------------------------------------------------------------

fn dreamer(c: &Composition) -> Card {
    let mut lines = vec![];
    if c.dream.state == Batch::Ready {
        lines.push(line(c.question(), "body"));
    }
    // The meta row's parts, the budget aside: it is the ring.
    let meta = instrument::meta(c);
    let rest: Vec<&str> = meta
        .split(" · ")
        .filter(|p| !p.is_empty() && !p.starts_with("dreamer "))
        .collect();
    if !rest.is_empty() {
        lines.push(line(rest.join(" · "), "caption"));
    }
    if c.dream.state != Batch::Ready {
        let status = instrument::status_line(c);
        if !status.is_empty() {
            lines.push(line(status, "dim"));
        }
    }
    let budget = &c.dream.budget;
    let ring = budget["spent"]
        .as_f64()
        .zip(budget["cap"].as_f64())
        .map(|(spent, cap)| Ring {
            spent,
            cap: cap.max(0.),
            turning: c.dream.state == Batch::Dreaming,
        });
    Card {
        id: "dreamer",
        title: dreamer_title(c).into(),
        tag: dreamer_tag(c).map(str::to_owned),
        tone: None,
        lines,
        rows: vec![],
        ring,
    }
}

/// The confusing list as the herdr pane draws it: "Same page?" (numbers, then the model's words)
/// once the dreamer has read the session.
fn same_page(c: &Composition) -> Option<Card> {
    let first = c.confusing.first()?;
    if first["same_page"] != true {
        return None;
    }
    let mut lines = vec![line(string(&first["numbers"]), "body")];
    let words = string(&first["words"]);
    if !words.is_empty() {
        lines.push(line(words, "dim"));
    }
    Some(Card {
        id: "same_page",
        title: super::same_page::TITLE.into(),
        tag: None,
        tone: None,
        lines,
        rows: vec![],
        ring: None,
    })
}

fn should_be(c: &Composition) -> Option<Card> {
    if c.should_be.is_empty() {
        return None;
    }
    Some(Card {
        id: "should_be",
        title: SHOULD_BE_TITLE.into(),
        tag: Some(format!("1 of {}", c.should_be.len())),
        tone: None,
        lines: c
            .should_be
            .iter()
            .map(|item| line(observation_text(item, true), "body"))
            .collect(),
        rows: vec![],
        ring: None,
    })
}

fn story(c: &Composition) -> Option<Card> {
    if c.story.is_empty() && c.shifts.is_empty() {
        return None;
    }
    let mut lines = vec![];
    if !c.story.is_empty() {
        lines.push(line(instrument::first_sentence(&c.story), "body"));
    }
    if !c.learnt.is_empty() {
        lines.push(line(c.learnt.join(" · "), "mint"));
    }
    for v in c.shifts.iter().take(3) {
        lines.push(line(shift_row(v, false), "code"));
    }
    Some(Card {
        id: "story",
        title: story_title(c).into(),
        tag: (!c.story_age.is_empty()).then(|| c.story_age.clone()),
        tone: None,
        lines,
        rows: vec![],
        ring: None,
    })
}

fn limits_card(c: &Composition) -> Option<Card> {
    let windows = limits(c);
    let codex = codex(c);
    let head = headline(c);
    if windows.is_empty() && codex.is_none() && head.is_none() {
        return None;
    }
    let mut rows: Vec<Row> = windows
        .iter()
        .map(|w| Row {
            label: w.name.clone(),
            value: (w.used / 100.).clamp(0., 1.),
            text: format!("{:.0}%", w.used.round()),
            tone: w.sev.clone(),
        })
        .collect();
    let mut lines: Vec<Line> = windows
        .iter()
        .map(|w| line(w.line.clone(), "caption"))
        .collect();
    if let Some((n, words)) = codex {
        rows.push(Row {
            label: "Codex 7D".into(),
            value: (n / 100.).clamp(0., 1.),
            text: format!("{n:.0}%"),
            tone: if n >= 90. {
                "red"
            } else if n >= 75. {
                "amber"
            } else {
                "ok"
            }
            .into(),
        });
        lines.push(line(words, "caption"));
    }
    let (title, tone) = head.map_or((String::new(), None), |(t, s)| (t, Some(s)));
    Some(Card {
        id: "limits",
        title,
        tag: None,
        tone,
        lines,
        rows,
        ring: None,
    })
}

fn agents_card(c: &Composition) -> Option<Card> {
    let head = agents_line(c)?;
    let mut lines = vec![line(head, "body")];
    lines.extend(
        instrument::agent_lines(c)
            .into_iter()
            .map(|l| line(l, "dim")),
    );
    Some(Card {
        id: "agents",
        title: String::new(),
        tag: None,
        tone: None,
        lines,
        rows: vec![],
        ring: None,
    })
}

/// The finished cards in the herdr pane's order.
pub fn cards(c: &Composition) -> Vec<Card> {
    let mut out = vec![dreamer(c)];
    out.extend(same_page(c));
    out.extend(should_be(c));
    out.extend(story(c));
    out.extend(limits_card(c));
    out.extend(agents_card(c));
    out
}

// ---- stage 2: the context window and Self-Reflect's row as data -----------------------------------

/// The Context card's parts with the engine's names, each with its ink (`setup`, `messages`,
/// `tools`, `csr`, `inuse`), and whether they are estimated (from the breakdown) or measured.
pub fn context_parts(c: &Composition) -> (Vec<(&'static str, f64, &'static str)>, bool) {
    let g = &c.figure;
    let tokens = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.);
    let b = &g["breakdown"];
    if b.is_object() {
        let messages = num(&b["text"]) + num(&b["you"]);
        let parts = [
            (
                super::breakdown::floor_name(b["floor_name"] == "Summary + setup"),
                num(&b["floor"]),
                "setup",
            ),
            ("Messages", messages, "messages"),
            ("Tool results", num(&b["tool"]), "tools"),
            ("CSR injected", num(&b["csr"]), "csr"),
        ];
        return (parts.into_iter().filter(|p| p.1 > 0.).collect(), true);
    }
    match tokens {
        Some(tokens) => (vec![("In use", tokens, "inuse")], false),
        None => (vec![], false),
    }
}

/// One part of the context window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Part {
    pub name: &'static str,
    pub tokens: f64,
    pub ink: &'static str,
}

/// The tools / context / other shares and their one word, as the Context card's donut has them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Mix {
    pub shares: [f64; 3],
    pub verdict: &'static str,
}

/// The context window as the herdr pane's Context card reads it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContextFacts {
    pub used: f64,
    pub max: f64,
    pub free: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer: Option<f64>,
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mix: Option<Mix>,
}

/// The context window: the bar spans the window Claude Code compacts to, its last part the buffer.
pub fn context(c: &Composition) -> Option<ContextFacts> {
    let g = &c.figure;
    let used = g["ctx"]["tokens"].as_f64().filter(|n| *n > 0.)?;
    let native = g["ctx"]["window"].as_f64().filter(|w| *w > 0.);
    let max = native.or(g["ctx"]["size"].as_f64().filter(|n| *n > 0.))?;
    let buffer = native.map(|w| super::app::compactor::rules::NATIVE_UNDER.min(w));
    let (named, _) = context_parts(c);
    let ink = [0u8; 4];
    let tagged: Vec<(&'static str, f64, [u8; 4])> =
        named.iter().map(|(n, v, _)| (*n, *v, ink)).collect();
    let mix = instrument::session_mix(&tagged).map(|(shares, verdict)| Mix { shares, verdict });
    Some(ContextFacts {
        used,
        max,
        free: (max - used - buffer.unwrap_or(0.)).max(0.),
        buffer,
        parts: named
            .into_iter()
            .map(|(name, tokens, ink)| Part { name, tokens, ink })
            .collect(),
        mix,
    })
}

/// "Self-Reflect this session:" and its measured parts.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CsrRow {
    pub lead: &'static str,
    pub parts: Vec<String>,
}

/// Self-Reflect's row, or none when nothing was measured.
pub fn csr_row(c: &Composition) -> Option<CsrRow> {
    let parts: Vec<String> = csr_parts(c).into_iter().map(|p| p.1).collect();
    (!parts.is_empty()).then_some(CsrRow {
        lead: CSR_LEAD,
        parts,
    })
}

/// Whether the herdr pane runs for this session: its pid file names a live process.
pub fn herdr_pane_live(paths: &super::paths::Paths, sid: &str) -> bool {
    paths
        .kept_file("panes", sid)
        .and_then(|p| super::paths::read_json(&p)["pid"].as_u64())
        .is_some_and(|pid| crate::codemap::session::pid_alive(pid as u32))
}

/// The keys only `--json --cards` adds. Plain `--json` is the summary and `schema`.
pub const CARD_KEYS: [&str; 4] = ["cards", "csr_row", "context", "herdr_pane_live"];

/// What the contract adds to `--json`.
#[derive(Debug, Serialize)]
pub struct Contract {
    pub schema: u32,
    pub cards: Vec<Card>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csr_row: Option<CsrRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub herdr_pane_live: Option<bool>,
}

/// The contract for a composition.
pub fn contract(c: &Composition) -> Contract {
    Contract {
        schema: SCHEMA,
        cards: cards(c),
        csr_row: csr_row(c),
        context: context(c),
        herdr_pane_live: None,
    }
}

/// `companion pane --json`: the summary and `schema`, as cheap as the summary alone. With
/// `--cards` and a session id, the contract joins it: the herdr pane's own composition (one
/// composing pass on the pane's data, nothing drawn, no week's spend added up), worded into
/// cards, on the one database both read.
pub fn pane_json(
    paths: &super::paths::Paths,
    args: &super::cli::PaneArgs,
    sid: &str,
    now: f64,
) -> serde_json::Value {
    let mut pane =
        (args.cards && !sid.is_empty()).then(|| super::pane::Pane::new(paths.clone(), args));
    let composed = pane.as_mut().and_then(|pane| {
        pane.build_frame(160, 60, now);
        let mut out = pane.composition.take().as_ref().map(contract)?;
        out.herdr_pane_live = Some(herdr_pane_live(paths, sid));
        Some(out)
    });
    let opened;
    let db = match &pane {
        Some(pane) => pane.db.as_ref(),
        None => {
            opened = super::db::Db::open(paths).ok();
            opened.as_ref()
        }
    };
    let summary = super::hook_json::pane_json_in(paths, db, &args.repo, sid, args.limit, now);
    let mut out = serde_json::to_value(summary).unwrap_or_default();
    if let Some(map) = out.as_object_mut() {
        map.insert("schema".into(), SCHEMA.into());
        if let Some(serde_json::Value::Object(fields)) =
            composed.and_then(|c| serde_json::to_value(c).ok())
        {
            map.extend(fields);
        }
    }
    out
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
