//! The compaction rules, apart from the hooks that use them: port of the removed `compact.ts`.
//! Nothing here asks Claude Code or the machine for anything.
//!
//! The numbers are one person's habit, mined from 249 typed `/compact`s (2026-10-12): they compact
//! from 181k (p10) to 269k (p50). The turn before a `/compact` had nothing running in the
//! background and did not end on a question 1.83 times as often as other turn ends in that range;
//! the median gap from that turn's end to the `/compact` was 252 s, so the countdown is 20 s.
//!
//! Lengths are the TypeScript's: UTF-16 code units. A cut that would fall inside a surrogate pair
//! leaves the character out (a Rust string holds no lone surrogate).

use crate::companion::{
    app::state::{k, BAND_HIGH, BAND_LOW},
    send::own_words,
};
use regex::Regex;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{json, Value};
use std::sync::LazyLock;

pub const DRIFT: f64 = 407_443.;
pub const COUNTDOWN_MS: f64 = 20_000.;
/// A countdown this far past its end is not resumed after a reload.
pub const RESUME_LATE_MS: f64 = 60_000.;
/// After a cancel: the next seam and this much more context.
pub const REARM_TOKENS: f64 = 20_000.;
/// A prompt typed at the terminal this recently: someone is there.
pub const ATTENDED_MS: f64 = 10. * 60. * 1000.;
/// A payback longer than this many turns is not worth planning for.
pub const PAYBACK_UNDER: f64 = 15.;
/// Claude Code's own auto-compact fires this far under its window: seen at 266k to 271k with
/// `autoCompactWindow` 300000.
pub const NATIVE_UNDER: f64 = 33_000.;
/// CSR acts, seam or not, this far before Claude Code's own auto-compact would: room for one
/// more turn end, since CSR compacts only at one.
pub const NATIVE_MARGIN: f64 = 20_000.;
/// Descriptions and other sections fit around the ids.
pub const KEEP_MAX: usize = 600;
/// Whole ids can extend the list this far.
pub const KEEP_IDS_MAX: usize = 1200;

/// Every word CSR puts into the conversation around a compaction opens with one of these, and
/// csr-engine's provenance guard holds both.
pub const KEEP_HEADER: &str = "CSR COMPACT KEEP —";
pub const NOTICE_HEADER: &str = "CSR compacted at";

// ---------------------------------------------------------------- JavaScript's strings

/// JavaScript's whitespace (`\s`, `trim`): not U+0085, and U+FEFF.
pub(crate) fn js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{FEFF}'
}

pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(js_space)
}

pub(crate) fn js_trim_end(text: &str) -> &str {
    text.trim_end_matches(js_space)
}

/// `text.replace(/\s+/g, ' ').trim()`.
pub(crate) fn one_line(text: &str) -> String {
    let words: Vec<&str> = text.split(js_space).filter(|w| !w.is_empty()).collect();
    words.join(" ")
}

/// `text.length`: UTF-16 code units.
pub(crate) fn width(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `units` UTF-16 units of `text`; a character that would be cut in two is left out.
pub(crate) fn prefix_units(text: &str, units: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        used += c.len_utf16();
        if used > units {
            break;
        }
        out.push(c);
    }
    out
}

/// `text` in at most `room` units, the cut marked by an ellipsis (the removed `compact.ts` `cut`).
pub(crate) fn cut(text: &str, room: i64) -> String {
    if width(text) as i64 <= room {
        return text.to_string();
    }
    let kept = prefix_units(text, (room - 1).max(0) as usize);
    format!("{}…", js_trim_end(&kept))
}

/// A whole number as JSON prints it (`214000`, not `214000.0`).
pub fn num(x: f64) -> Value {
    if x.fract() == 0. && x.abs() < 9e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

pub fn ser_num<S: Serializer>(x: &f64, s: S) -> Result<S::Ok, S::Error> {
    if x.fract() == 0. && x.abs() < 9e15 {
        s.serialize_i64(*x as i64)
    } else {
        s.serialize_f64(*x)
    }
}

pub fn ser_opt_num<S: Serializer>(x: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match x {
        Some(x) => ser_num(x, s),
        None => s.serialize_none(),
    }
}

// ---------------------------------------------------------------- the setting and the mode

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Setting {
    Off,
    #[default]
    Suggest,
    Auto,
}

impl Setting {
    /// What a `/config` value comes to: anything but `off` and `auto` is `suggest`.
    pub fn of(value: &Value) -> Self {
        match value.as_str() {
            Some("off") => Self::Off,
            Some("auto") => Self::Auto,
            _ => Self::Suggest,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Suggest => "suggest",
            Self::Auto => "auto",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Attended,
    Unattended,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attended => "attended",
            Self::Unattended => "unattended",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Composer,
    Bridge,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Human {
    pub kind: Source,
    pub at: f64,
}

/// Attended: the last prompt a person sent was typed at the terminal less than 10 minutes ago.
/// Unattended: it came through Remote Control, or nobody has typed at the terminal for 10 minutes
/// while turns go on. A module that has seen no prompt yet counts from when it loaded.
pub fn mode_of(last: Option<Human>, now: f64, loaded_at: f64) -> Mode {
    if last.is_some_and(|human| human.kind == Source::Bridge) {
        return Mode::Unattended;
    }
    if now - last.map_or(loaded_at, |human| human.at) >= ATTENDED_MS {
        Mode::Unattended
    } else {
        Mode::Attended
    }
}

// ---------------------------------------------------------------- after a reload

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    Countdown,
    Offer,
}

/// What `resume_of` reads of the band's view.
#[derive(Clone, Copy, Debug)]
pub struct ShownView {
    pub kind: ViewKind,
    pub ends_at: Option<f64>,
    pub has_armed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resume {
    None,
    Drop,
    Offer,
    Countdown { left_ms: f64 },
}

/// A countdown or an offer a reload left behind: a view with nothing to compact with, or a
/// countdown more than `RESUME_LATE_MS` past its end, is dropped. The setting decides too: off
/// drops it, suggest makes it an offer.
pub fn resume_of(view: Option<ShownView>, now: f64, setting: Setting) -> Resume {
    let Some(view) = view else {
        return Resume::None;
    };
    if !view.has_armed || setting == Setting::Off {
        return Resume::Drop;
    }
    if view.kind == ViewKind::Offer || setting == Setting::Suggest {
        return Resume::Offer;
    }
    let left = view.ends_at.unwrap_or(0.) - now;
    if left < -RESUME_LATE_MS {
        Resume::Drop
    } else {
        Resume::Countdown {
            left_ms: left.max(0.),
        }
    }
}

// ---------------------------------------------------------------- the seam

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Running {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub description: String,
}

impl Running {
    pub fn new(kind: &str, id: &str, description: &str) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            description: description.into(),
        }
    }
}

/// What a turn end was: the signs a seam is read from.
pub struct TurnEnd<'a> {
    pub answer: &'a str,
    /// An AskUserQuestion still open.
    pub asking: bool,
    /// Background work in flight at the turn's end; none when no Stop said.
    pub running: Option<&'a [Running]>,
    pub commit: Option<&'a str>,
    pub gate: bool,
    pub edits: u64,
}

/// What a turn end was, as the log keeps it: whether it is a seam, its why, and the signs it was
/// read from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Seam {
    pub is_seam: bool,
    pub why: String,
    pub question: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<u64>,
    #[serde(default)]
    pub running: usize,
}

/// The final text ends on a question: its last character, markdown and closing quotes aside, is
/// `?`.
pub fn ends_on_question(text: &str) -> bool {
    strip_tail(text).ends_with('?')
}

fn strip_tail(text: &str) -> &str {
    let closers = "*_`)]>\"'”’";
    js_trim_end(text).trim_end_matches(|c: char| js_space(c) || closers.contains(c))
}

/// The question a turn ended on: its last sentence, as the owner would read it.
pub fn last_question(text: &str) -> Option<String> {
    if !ends_on_question(text) {
        return None;
    }
    let tail = strip_tail(text);
    let body = tail.strip_suffix('?').unwrap_or(tail);
    let from = body.rfind(['.', '!', '?', '\n']).map_or(0, |at| {
        at + body[at..].chars().next().map_or(1, char::len_utf8)
    });
    let found = format!("{}?", &body[from..]);
    let plain: String = found.chars().filter(|c| !"*_`#>".contains(*c)).collect();
    let line = one_line(&plain);
    (!line.is_empty()).then_some(line)
}

/// A JavaScript `\s`, as a class of the regex crate.
const S: &str = r"[\s\x{FEFF}--\x{85}]";

static GATE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    let word = r"(?i-u:tests?|gates?|checks?)";
    let pass = r"(?i-u:pass|green)";
    let all = format!(r"(?i-u:all){S}+(?:[0-9]+{S}+)?{word}{S}+{pass}");
    let tests = format!(r"(?i-u:tests?){S}+{pass}");
    let gates = format!(r"(?i-u:gates?){S}+{pass}");
    let passed = format!(r"[0-9]+{S}+(?i-u:passed)");
    let pattern = format!(r"(?-u:\b)(?:{all}|{tests}|{gates}|{passed}|(?i-u:test result: ok))");
    Regex::new(&pattern).unwrap()
});

/// A commit SHA named next to commit/landed/merged/HEAD (agents commit; the main thread reports
/// it): `commit(ted)`, `landed`, `merged`, `SHA` or `HEAD`, a gap of up to 40 units on its line,
/// then a token of 7 to 40 hex digits that holds a letter and a digit, at word edges.
pub fn commit_in(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let edge = |at: usize| {
        let before = at > 0 && word(chars[at - 1]);
        let after = at < chars.len() && word(chars[at]);
        before != after
    };
    let hex = |c: char| c.is_ascii_hexdigit();
    let lower = |at: usize, text: &str| {
        let want: Vec<char> = text.chars().collect();
        at + want.len() <= chars.len()
            && chars[at..at + want.len()]
                .iter()
                .zip(&want)
                .all(|(c, w)| c.to_ascii_lowercase() == *w)
    };
    for start in 0..chars.len() {
        if !edge(start) {
            continue;
        }
        for keyword in ["committed", "commit", "landed", "merged", "sha", "head"] {
            let end = start + keyword.len();
            if !lower(start, keyword) || !edge(end) {
                continue;
            }
            let mut at = end;
            let mut units = 0;
            loop {
                if at < chars.len() && edge(at) && hex(chars[at]) {
                    let run = chars[at..].iter().take_while(|c| hex(**c)).count();
                    let token = &chars[at..at + run];
                    let letter = token.iter().any(|c| c.is_ascii_alphabetic());
                    let digit = token.iter().any(char::is_ascii_digit);
                    if (7..=40).contains(&run) && letter && digit && edge(at + run) {
                        let seven: String = token[..7].iter().collect();
                        return Some(seven.to_ascii_lowercase());
                    }
                }
                if at >= chars.len() || chars[at] == '\n' {
                    break;
                }
                units += chars[at].len_utf16();
                if units > 40 {
                    break;
                }
                at += 1;
            }
        }
    }
    None
}

/// The words a passing gate is reported in.
pub fn gate_in(text: &str) -> bool {
    GATE_WORDS.is_match(text)
}

/// Background work that keeps a turn end from being a seam: a running or pending task of any type
/// but a monitor (a watcher that never ends on its own).
pub fn blocking(running: &[Running]) -> Vec<&Running> {
    running
        .iter()
        .filter(|task| task.kind != "monitor")
        .collect()
}

fn plural(n: u64, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// A seam: what preceded the owner's own compactions. Nothing running in the background and no
/// question waiting. `why` is what the band and the log say about it, the strongest sign first.
pub fn seam_of(end: &TurnEnd) -> Seam {
    let question = end.asking || ends_on_question(end.answer);
    let busy = end.running.map(blocking);
    let commit = (end.commit.map(str::to_string))
        .or_else(|| commit_in(end.answer))
        .filter(|commit| !commit.is_empty());
    let gate = end.gate || gate_in(end.answer);
    let why = if let Some(commit) = &commit {
        format!("commit {commit} landed")
    } else if gate {
        "gates passed".to_string()
    } else if end.edits > 0 {
        format!(
            "{} {} saved",
            end.edits,
            if end.edits == 1 { "edit" } else { "edits" }
        )
    } else {
        "nothing running in the background".to_string()
    };
    let mut seam = Seam {
        why,
        question,
        commit,
        gate: gate.then_some(true),
        edits: (end.edits > 0).then_some(end.edits),
        running: busy.as_ref().map_or(0, Vec::len),
        ..Seam::default()
    };
    if question {
        let wait = if end.asking {
            "a question is open"
        } else {
            "your answer"
        };
        seam.wait = Some(wait.into());
        return seam;
    }
    let Some(busy) = busy else {
        seam.wait = Some("background work unknown".into());
        return seam;
    };
    if !busy.is_empty() {
        let agents = busy.iter().filter(|task| task.kind != "shell").count() as u64;
        let shells = busy.len() as u64 - agents;
        let parts = [
            (agents > 0).then(|| plural(agents, "agent")),
            (shells > 0).then(|| plural(shells, "shell")),
        ];
        let said: Vec<String> = parts.into_iter().flatten().collect();
        seam.wait = Some(format!("{} running", said.join(" and ")));
        return seam;
    }
    seam.is_seam = true;
    seam
}

// ---------------------------------------------------------------- the decision

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Act {
    None,
    Plan,
    Offer,
    Countdown,
    Compact,
}

impl Act {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Plan => "plan",
            Self::Offer => "offer",
            Self::Countdown => "countdown",
            Self::Compact => "compact",
        }
    }
}

/// Compacting now pays back after this many requests (an API call each; a turn makes many), at
/// this session's requests a turn (none before it has two whole turns).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payback {
    #[serde(serialize_with = "ser_num")]
    pub requests: f64,
    #[serde(default, serialize_with = "ser_opt_num")]
    pub per_turn: Option<f64>,
}

/// What the pane answered of the payback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pay {
    /// The removed `pane.py` did not answer.
    Unanswered,
    /// It never would pay back.
    Never,
    Is(Payback),
}

impl Pay {
    pub fn payback(self) -> Option<Payback> {
        match self {
            Self::Is(payback) => Some(payback),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub act: Act,
    pub wait: Option<String>,
    pub forced: bool,
}

#[derive(Clone, Copy)]
pub struct Ask<'a> {
    pub setting: Setting,
    pub mode: Mode,
    pub ctx: f64,
    pub payback: Pay,
    pub seam: &'a Seam,
    /// The context when the owner last cancelled, in this chapter.
    pub cancelled_at: Option<f64>,
    /// The window Claude Code's own auto-compact works to, when one is set and it is on.
    pub native_window: Option<f64>,
}

/// The window Claude Code's own auto-compact works to: `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, else
/// `autoCompactWindow` in settings.json. None when neither is set (it then fires near the
/// model's own window, far past the band) or when it is off.
pub fn native_window_of(settings: &Value, env_window: Option<&str>, disabled: bool) -> Option<f64> {
    if disabled || settings["autoCompactEnabled"] == false {
        return None;
    }
    env_window
        .and_then(|text| text.trim().parse::<f64>().ok())
        .or_else(|| settings["autoCompactWindow"].as_f64())
        .filter(|window| *window > 0.)
}

/// Where CSR acts at a turn end, seam or not: the band's high edge, or sooner when Claude Code's
/// own auto-compact would come first. Never under the band.
pub fn force_at(native_window: Option<f64>) -> f64 {
    native_window
        .map_or(BAND_HIGH, |window| {
            BAND_HIGH.min(window - NATIVE_UNDER - NATIVE_MARGIN)
        })
        .max(BAND_LOW)
}

/// The payback in turns; unmeasured, each request taken as a turn (the cautious reading).
pub fn payback_turns(payback: &Payback) -> f64 {
    match payback.per_turn {
        Some(per) if per != 0. => payback.requests / per,
        _ => payback.requests,
    }
}

/// What to do at a main-loop turn end.
///  - off, below the band, or a compaction that would not pay back within `PAYBACK_UNDER` turns:
///    nothing. With no payback at all only the high edge acts.
///  - in the band: a plan; at a seam, act. At or past the high edge, or `NATIVE_MARGIN` before
///    Claude Code's own auto-compact: act, seam or not.
///  - a question waits on the owner: plan only; unattended past the high edge it no longer holds.
///  - after a cancel: the next seam and 20k more context.
///  - act: suggest offers a press; attended counts down 20 s with a cancel; unattended now.
pub fn decide(ask: &Ask) -> Verdict {
    let forced = ask.ctx >= force_at(ask.native_window);
    let said = |act, wait: Option<String>, forced| Verdict { act, wait, forced };
    if ask.setting == Setting::Off || ask.ctx < BAND_LOW {
        return said(Act::None, None, false);
    }
    let late = match ask.payback {
        Pay::Never => true,
        Pay::Is(payback) => payback_turns(&payback) > PAYBACK_UNDER,
        Pay::Unanswered => !forced,
    };
    if late {
        return said(Act::None, None, false);
    }
    if ask.seam.question && !(ask.mode == Mode::Unattended && forced) {
        return said(Act::Plan, ask.seam.wait.clone(), forced);
    }
    if let Some(cancelled) = ask.cancelled_at {
        if ask.ctx < cancelled + REARM_TOKENS {
            let wait = format!("cancelled · waits for {}", k(cancelled + REARM_TOKENS));
            return said(Act::Plan, Some(wait), forced);
        }
    }
    if !ask.seam.is_seam && !forced {
        return said(Act::Plan, ask.seam.wait.clone(), forced);
    }
    if ask.setting == Setting::Suggest {
        return said(Act::Offer, None, forced);
    }
    let act = if ask.mode == Mode::Attended {
        Act::Countdown
    } else {
        Act::Compact
    };
    said(act, None, forced)
}

// ---------------------------------------------------------------- what CSR says

/// The payback in your prompts: its API calls over this session's calls a prompt, rounded up (a
/// prompt under way counts whole); none while the calls a prompt are unmeasured.
pub fn payback_prompts(payback: &Payback) -> Option<u64> {
    match payback.per_turn {
        Some(per) if per != 0. => Some(((payback.requests / per).ceil()).max(1.) as u64),
        _ => None,
    }
}

/// `pays back in ~14 API calls (~2 prompts)`, as the pane says it.
pub fn payback_text(payback: Option<&Payback>) -> String {
    let Some(payback) = payback else {
        return String::new();
    };
    let calls = payback.requests.round().max(1.) as u64;
    let said = format!("pays back in ~{}", plural(calls, "API call"));
    match payback_prompts(payback) {
        Some(prompts) => format!("{said} (~{})", plural(prompts, "prompt")),
        None => said,
    }
}

fn band_edges() -> (String, String) {
    (k(BAND_LOW), k(BAND_HIGH))
}

/// The pane's plan row: `Compact planned · 214k of your 181k–269k band · pays back in ~14 API
/// calls (~2 prompts) · waiting for a seam`.
pub fn plan_text(ctx: f64, payback: Option<&Payback>, wait: Option<&str>, forced: bool) -> String {
    let (low, high) = band_edges();
    let where_ = if forced && ctx >= BAND_HIGH {
        format!("{}, past your {low}–{high} band", k(ctx))
    } else if forced {
        format!("{}, close to Claude Code's own auto-compact", k(ctx))
    } else {
        format!("{} of your {low}–{high} band", k(ctx))
    };
    let waiting = match wait {
        Some(wait) if !wait.is_empty() => {
            if wait.starts_with("cancelled") {
                wait.to_string()
            } else {
                format!("waiting: {wait}")
            }
        }
        _ => "waiting for a seam".to_string(),
    };
    let parts = [
        "Compact planned".to_string(),
        where_,
        payback_text(payback),
        waiting,
    ];
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Why it acts, for the band and the plan row: at a seam, its strongest sign; past the high edge
/// without one, the edge and what it compacts over.
pub fn act_why(seam: &Seam) -> String {
    if seam.is_seam {
        return seam.why.clone();
    }
    let over = if seam.running > 0 {
        let wait = seam.wait.as_deref().unwrap_or("");
        let said = wait.strip_suffix(" running").unwrap_or(wait);
        Some(format!("{said} kept"))
    } else if seam.question {
        Some("question kept".to_string())
    } else {
        seam.wait.clone()
    };
    let parts = [Some(format!("over {}", k(BAND_HIGH))), over];
    let parts: Vec<String> = parts
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect();
    parts.join(" · ")
}

/// The band above the prompt while it counts down: `Compacting in 20s · 214k · commit 6369e6b
/// landed`.
pub fn countdown_text(left: f64, ctx: f64, why: &str) -> String {
    format!(
        "Compacting in {}s · {} · {why}",
        (left / 1000.).ceil().max(0.) as i64,
        k(ctx)
    )
}

/// In suggest mode, an offer instead: a press compacts.
pub fn offer_text(ctx: f64, payback: Option<&Payback>, why: &str) -> String {
    let parts = [
        "Compact now?".to_string(),
        k(ctx),
        payback_text(payback),
        why.to_string(),
    ];
    let parts: Vec<String> = parts.into_iter().filter(|part| !part.is_empty()).collect();
    parts.join(" · ")
}

// ---------------------------------------------------------------- what the summary keeps

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keep {
    /// In-flight background work, with ids.
    pub running: Vec<Running>,
    /// Open questions waiting on the owner.
    pub decisions: Vec<String>,
    /// Uncommitted or unpushed work, with SHAs.
    pub work: Vec<String>,
    /// Binding rules the owner stated this session.
    pub rules: Vec<String>,
}

/// The keep list as the pane's card shows it, a line each.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KeepRows {
    pub running: Vec<String>,
    pub decisions: Vec<String>,
    pub work: Vec<String>,
    pub rules: Vec<String>,
}

pub fn keep_rows(keep: &Keep) -> KeepRows {
    let line = |text: &String| cut(&one_line(text), 160);
    let running = keep
        .running
        .iter()
        .map(|task| {
            let id = format!("{} {}", task.kind, task.id);
            let room = (157 - width(&id) as i64).max(0);
            let description = cut(&one_line(&task.description), room);
            if !description.is_empty() && width(&id) < 157 {
                format!("{id} · {description}")
            } else {
                id
            }
        })
        .collect();
    KeepRows {
        running,
        decisions: keep.decisions.iter().map(line).collect(),
        work: keep.work.iter().map(line).collect(),
        rules: keep.rules.iter().map(line).collect(),
    }
}

/// Whole running ids come first. Descriptions and other sections share the room left below
/// `KEEP_MAX`. If the ids exceed `KEEP_IDS_MAX`, Claude Code lists the remaining tasks after
/// compaction.
pub fn keep_list(keep: &Keep) -> String {
    let head = format!(
        "{KEEP_HEADER} in the summary, keep these, in this order, ids and SHAs word for word:"
    );
    let rules: Vec<&String> = (keep.rules.iter())
        .filter(|rule| !js_trim(&own_words(rule)).is_empty())
        .collect();
    let mut types: Vec<&str> = Vec::new();
    for task in &keep.running {
        if !types.contains(&task.kind.as_str()) {
            types.push(&task.kind);
        }
    }
    let tasks: Vec<&Running> = types
        .iter()
        .flat_map(|kind| keep.running.iter().filter(move |task| task.kind == *kind))
        .collect();
    let packed = |items: &[&Running], descriptions: &[String]| {
        let mut parts: Vec<(&str, Vec<String>)> = Vec::new();
        for (i, task) in items.iter().enumerate() {
            let id = format!(
                "{}{}",
                task.id,
                descriptions.get(i).map_or("", String::as_str)
            );
            match parts.iter_mut().find(|(kind, _)| *kind == task.kind) {
                Some((_, ids)) => ids.push(id),
                None => parts.push((&task.kind, vec![id])),
            }
        }
        let said: Vec<String> = parts
            .iter()
            .map(|(kind, ids)| format!("{kind} {}", ids.join(",")))
            .collect();
        said.join("; ")
    };
    let running_line = |items: &[&Running], descriptions: &[String]| {
        let omitted = tasks.len() - items.len();
        let more = if omitted > 0 {
            format!(" +{omitted} more running (Claude Code lists them after compaction)")
        } else {
            String::new()
        };
        format!(
            "Running in the background: {}{more}.",
            packed(items, descriptions)
        )
    };
    let over =
        |items: &[&Running]| width(&head) + 4 + width(&running_line(items, &[])) > KEEP_IDS_MAX;
    let mut kept: Vec<&Running> = tasks.clone();
    if over(&kept) {
        kept.clear();
        for task in &tasks {
            kept.push(task);
            if over(&kept) {
                kept.pop();
                break;
            }
        }
    }
    let running = if tasks.is_empty() {
        String::new()
    } else {
        running_line(&kept, &[])
    };
    let join = |items: &[&String], by: &str| {
        let said: Vec<String> = items.iter().map(|item| one_line(item)).collect();
        said.join(by)
    };
    let decisions: Vec<&String> = keep.decisions.iter().collect();
    let work: Vec<&String> = keep.work.iter().collect();
    let others = [
        if decisions.is_empty() {
            String::new()
        } else {
            format!("Waiting on the owner: {}", join(&decisions, " / "))
        },
        if work.is_empty() {
            String::new()
        } else {
            format!("Unfinished work: {}.", join(&work, "; "))
        },
        if rules.is_empty() {
            String::new()
        } else {
            format!("The owner's rules this session: {}", join(&rules, " / "))
        },
    ];
    let mut sections: Vec<&String> = vec![&running];
    sections.extend(others.iter());
    sections.retain(|section| !section.is_empty());
    if sections.is_empty() {
        let none = format!(
            "{KEEP_HEADER} nothing runs in the background and nothing waits on the owner: keep the task in hand and its next step."
        );
        return cut(&none, KEEP_MAX as i64);
    }
    let number = |text: &str, i: usize| format!("{}. {text}", i + 1);
    // Fair shares of the room the ids leave: each line keeps what it needs up to an even split;
    // what a short one leaves goes on. The running line's share beyond its ids goes to its
    // descriptions.
    let first = if running.is_empty() {
        0
    } else {
        width(&number(&running, 0))
    };
    let mut room = KEEP_MAX as i64 - width(&head) as i64 - sections.len() as i64 - first as i64;
    let rest: Vec<&String> = others.iter().filter(|text| !text.is_empty()).collect();
    let offset = usize::from(!running.is_empty());
    let mut fitted: Vec<String> = vec![String::new(); rest.len()];
    let described: Vec<String> = (kept.iter())
        .filter(|task| !task.description.is_empty())
        .map(|task| format!(" ({})", one_line(&task.description)))
        .collect();
    let wants: usize = described.iter().map(|text| width(text)).sum();
    // The descriptions take part as one more line, of the length they want.
    let mut order: Vec<(usize, Option<usize>)> = rest
        .iter()
        .enumerate()
        .map(|(i, text)| (width(&number(text, i + offset)), Some(i)))
        .collect();
    order.sort_by_key(|(length, _)| *length);
    if wants > 0 {
        order.push((wants, None));
    }
    order.sort_by_key(|(length, _)| *length);
    let share_of = |left: i64, n: usize| left.max(0) / n as i64;
    let mut for_descriptions: i64 = 0;
    let lines = order.len();
    for (n, (length, at)) in order.iter().enumerate() {
        let share = share_of(room, lines - n);
        match at {
            None => {
                for_descriptions = (*length as i64).min(share);
                room -= for_descriptions;
            }
            Some(i) => {
                fitted[*i] = if room > 0 {
                    cut(&number(rest[*i], i + offset), (*length as i64).min(share))
                } else {
                    String::new()
                };
                room -= width(&fitted[*i]) as i64;
            }
        }
    }
    // The running line: every id, each description cut to an even split of the room left.
    let mut line = running.clone();
    if !running.is_empty() && for_descriptions > 0 {
        let mut left = for_descriptions;
        let pieces: Vec<String> = (0..kept.len())
            .map(|at| {
                let task = kept[at];
                if task.description.is_empty() {
                    return String::new();
                }
                let others = kept[at..]
                    .iter()
                    .filter(|next| !next.description.is_empty())
                    .count();
                let room = (share_of(left, others) - 3).max(0);
                let said = cut(&one_line(&task.description), room);
                let piece = if !said.is_empty() && said != "…" {
                    format!(" ({said})")
                } else {
                    String::new()
                };
                left -= width(&piece) as i64;
                piece
            })
            .collect();
        line = running_line(&kept, &pieces);
    }
    let mut out = vec![head];
    if !running.is_empty() {
        out.push(number(&line, 0));
    }
    out.extend(fitted.into_iter().filter(|text| !text.is_empty()));
    out.join("\n")
}

/// The line the phone shows after CSR compacts: `CSR compacted at 312k → 66k · pays back in ~14
/// API calls (~2 prompts) · kept: 2 running agents, 1 open decision · 2 waiting in Lost and
/// found`.
pub fn notice_line(
    before: f64,
    after: Option<f64>,
    payback: Option<&Payback>,
    keep: &Keep,
    waiting: usize,
) -> String {
    let shells = keep
        .running
        .iter()
        .filter(|task| task.kind == "shell")
        .count() as u64;
    let agents = keep.running.len() as u64 - shells;
    let mut kept: Vec<String> = Vec::new();
    if agents > 0 {
        kept.push(format!(
            "{agents} running {}",
            if agents == 1 { "agent" } else { "agents" }
        ));
    }
    if shells > 0 {
        kept.push(format!(
            "{shells} running {}",
            if shells == 1 { "shell" } else { "shells" }
        ));
    }
    let decisions = keep.decisions.len();
    if decisions > 0 {
        kept.push(format!(
            "{decisions} open {}",
            if decisions == 1 {
                "decision"
            } else {
                "decisions"
            }
        ));
    }
    if !keep.work.is_empty() {
        kept.push("unfinished work".to_string());
    }
    let arrow = match after {
        Some(after) if after != 0. => format!(" → {}", k(after)),
        _ => String::new(),
    };
    let parts = [
        format!("{NOTICE_HEADER} {}{arrow}", k(before)),
        payback_text(payback),
        if kept.is_empty() {
            String::new()
        } else {
            format!("kept: {}", kept.join(", "))
        },
        if waiting > 0 {
            format!("{waiting} waiting in Lost and found")
        } else {
            String::new()
        },
    ];
    let parts: Vec<String> = parts.into_iter().filter(|part| !part.is_empty()).collect();
    parts.join(" · ")
}

/// Fenced code (``` or ~~~, to a closing fence of the same kind at least as long, or to the end
/// when none closes it): an example the owner shows, never a rule they set.
pub fn unfenced(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut fence = String::new();
    for line in text.split('\n') {
        let mark = fence_of(line);
        if !fence.is_empty() {
            if let Some(mark) = mark {
                let alike = mark.chars().next() == fence.chars().next();
                let rest = js_trim(line)[mark.len()..].trim_matches(js_space);
                if alike && mark.len() >= fence.len() && rest.is_empty() {
                    fence.clear();
                }
            }
        } else if let Some(mark) = mark {
            fence = mark.to_string();
        } else {
            kept.push(line);
        }
    }
    kept.join("\n")
}

/// `^ {0,3}(`{3,}|~{3,})`: the fence a line opens or closes with.
fn fence_of(line: &str) -> Option<&str> {
    let spaces = line.chars().take_while(|c| *c == ' ').count();
    if spaces > 3 {
        return None;
    }
    let rest = &line[spaces..];
    let first = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = rest.chars().take_while(|c| *c == first).count();
    (run >= 3).then(|| &rest[..run])
}

static BINDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u:\b(?:never|always|don'?t|do not|must|no more)\b)").unwrap()
});

/// `text.split(/(?<=[.!?])\s+|\n+/)`.
fn sentences(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let (mut from, mut i) = (0, 0);
    while i < chars.len() {
        let (pos, c) = chars[i];
        let stop_before = i > 0 && ".!?".contains(chars[i - 1].1);
        let inside: Option<fn(char) -> bool> = if stop_before && js_space(c) {
            Some(js_space)
        } else if c == '\n' {
            Some(|c| c == '\n')
        } else {
            None
        };
        let Some(inside) = inside else {
            i += 1;
            continue;
        };
        let mut j = i;
        while j < chars.len() && inside(chars[j].1) {
            j += 1;
        }
        out.push(&text[from..pos]);
        from = chars.get(j).map_or(text.len(), |(p, _)| *p);
        i = j;
    }
    out.push(&text[from..]);
    out
}

/// A sentence of the owner's that binds: never / always / don't / must, in their own words (what
/// they pasted, forwarded and quoted out) and outside code examples. At most 160 characters.
pub fn rule_in(text: &str) -> Option<String> {
    let plain = unfenced(&own_words(text));
    for sentence in sentences(&plain) {
        let line = one_line(sentence);
        if width(&line) >= 8 && BINDING.is_match(&line) {
            return Some(cut(&line, 160));
        }
    }
    None
}
