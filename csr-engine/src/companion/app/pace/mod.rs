//! How often the dreamer looks: a feed with a limit, not a fixed number a day and not unlimited.
//! Port of `hooks/pace.ts` and of the dreamer's bookkeeping in `register.tsx`: `paceOf` and
//! `changePace` (486-506), the chapters (526-593) and the per-session key sweep (881-907).
//!
//! Four rules, each decided without a model call:
//!  1. A chapter holds five dreams. A chapter runs from the session's start, or its last
//!     compaction, to the next compaction; one that runs two hours without one starts over too. A
//!     dream you adopt (helpful, ask Claude) gives its place back. Looks stop at twice the
//!     chapter's dreams.
//!  2. The dreams left are spread over the chapter. The first look comes after 6 of Claude's
//!     steps; each later one after the steps a chapter of yours usually runs (the median of your
//!     last chapters), less those gone, over the dreams left. A look that finds nothing does not
//!     slow the next.
//!  3. Something new. A look happens only when CSR's brief holds a line about the work in hand (a
//!     `* ` line) the last look did not have.
//!  4. A share of the work. Before each look the dreamer checks that what it has spent, plus what
//!     the last look cost, stays within 1 token for every 10 the main session has spent. The
//!     check is a guess: a fork takes no token ceiling, so one look can cost more than the last
//!     and end over the share. That is not forgiven: the excess is owed, every look after it
//!     waits, and the wait ends when Claude's own spending brings the total back within the share.
//!
//! The rows. A pace is the store row `(sid, "pace")` and a chapter `(sid, "chapter")`: the
//! TypeScript's `pace:<sid>` and `chapter:<sid>`. The machine's rows are `chapterSteps` (how long
//! chapters ran) and, in the sweep, the unkeyed keys older versions wrote.
//!
//! Entries (the map's handlers 5, 8, 10, 17):
//! - `turn_complete` (Notify): `closeTurn` of the turn, when dreams are on.
//! - `step` (Stream `/after`): `countStep`, then the chapter's `steps += 1`. What the dreamer's
//!   `step` entry, which runs right after this one, needs is left in `Pace` (`fresh` and
//!   `chapter`), reached with `cx.kept`; `fresh(cx)` and `counted_chapter(cx)` read it.
//! - `chapter` (Gate): `newChapter` after a compaction that went through.
//! - `start` (Start): the sweep.
//!
//! The TypeScript's one queue per kind of change (`serially`, `paceQueue`) is a lock per kind on
//! the process-wide state: changes are read, made and written one at a time, in the order asked.
//!
//! Differences from the TypeScript, each from the engine having no plugin around it:
//! - `$.session.id()` is `cx.sid`; the options (`dreamsOn`) are read from `/config` where they are
//!   needed, as `state::Options` does.
//! - `isPerson` is `state::is_person`, the one every feature asks: the session's `interactive` from
//!   its start, else where it draws (`session.surfaces`), asked once.
//! - The `--keep` calls of pane.py are `Keep`, the in-process twin of the pane's `--keep` mode.
//! - A pace's `turns` is kept as a list of `[key, receipt]` pairs: the store's JSON objects have
//!   no order here, and the order is what the trimming of old turns goes by. A row written as an
//!   object is read as it comes.
//! - The sweep reads the sessions that have a chapter with a read-only query of the store's own
//!   file: `Store` has no call that lists sessions.

use super::state::{config_rows, is_person, k, Options};
use crate::companion::{
    db::Db,
    dreams,
    paths::{self, Paths},
    relay::{
        dispatch::{handler, Ctx, Handler, Reply, Table},
        host::BoxFuture,
    },
    store::MACHINE,
};
use anyhow::{Context, Result};
use serde::{ser::SerializeSeq, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Map, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const FIRST_EVERY: f64 = 6.;
pub const PER_CHAPTER: f64 = 5.;
pub const CHAPTER_MS: f64 = 2. * 60. * 60. * 1000.;
/// A chapter's steps, until there are chapters of yours to learn from.
pub const USUAL_STEPS: f64 = 120.;
pub const SHARE: f64 = 0.1;
/// Ended turns remembered (oldest go first): they only guard a replay of a step.
pub const TURNS_KEPT: usize = 60;
/// A safety bound on turns that never ended; far above any real run.
pub const OPEN_KEPT: usize = 1000;
/// How many chapter lengths the machine remembers.
pub const CHAPTERS_KEPT: usize = 20;
/// Sessions whose keys are kept; older ones are swept at a session's start.
pub const SESSIONS_KEPT: usize = 20;
/// The step the next look is due at, once a chapter has no dreams left (JavaScript's
/// `Number.MAX_SAFE_INTEGER`).
pub const NEVER: f64 = 9_007_199_254_740_991.;

/// The keys that belong to one session (`PER_SESSION`, register.tsx 101).
const PER_SESSION: [&str; 9] = [
    "pace",
    "chapter",
    "briefFacts",
    "looksToSkip",
    "typedPastInARow",
    "asked",
    "sentDream",
    "agentLedger",
    "storyStarts",
];
/// The ones the sweep of old sessions takes (the TypeScript's pattern leaves `storyStarts` out).
const SWEPT: [&str; 8] = [
    "pace",
    "chapter",
    "briefFacts",
    "looksToSkip",
    "typedPastInARow",
    "asked",
    "sentDream",
    "agentLedger",
];

// ---------------------------------------------------------------- numbers as JavaScript keeps them

/// A number the way `JSON.stringify` writes it: a whole one without a fraction.
fn num_value(n: f64) -> Value {
    if n.fract() == 0. && n.abs() <= NEVER {
        json!(n as i64)
    } else {
        json!(n)
    }
}

fn num<S: Serializer>(n: &f64, s: S) -> std::result::Result<S::Ok, S::Error> {
    num_value(*n).serialize(s)
}

fn opt_num<S: Serializer>(n: &Option<f64>, s: S) -> std::result::Result<S::Ok, S::Error> {
    match n {
        Some(n) => num(n, s),
        None => s.serialize_none(),
    }
}

fn nums<S: Serializer>(list: &[f64], s: S) -> std::result::Result<S::Ok, S::Error> {
    let mut seq = s.serialize_seq(Some(list.len()))?;
    for n in list {
        seq.serialize_element(&num_value(*n))?;
    }
    seq.end()
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------- pace

/// The step indexes of one main-session turn already counted, and whether the turn has ended.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    #[serde(default, serialize_with = "nums")]
    pub steps: Vec<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub closed: bool,
}

/// A pace's receipts, in the order they were last touched (the oldest are trimmed first).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Turns(pub Vec<(String, Receipt)>);

impl Turns {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Turns {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for (key, receipt) in &self.0 {
            seq.serialize_element(&(key, receipt))?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Turns {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error;
        let mut turns = Vec::new();
        match Value::deserialize(d)? {
            Value::Null => (),
            Value::Array(items) => {
                for item in items {
                    turns.push(serde_json::from_value(item).map_err(D::Error::custom)?);
                }
            }
            Value::Object(rows) => {
                for (key, row) in rows {
                    let receipt = serde_json::from_value(row).map_err(D::Error::custom)?;
                    turns.push((key, receipt));
                }
            }
            other => return Err(D::Error::custom(format!("turns cannot be {other}"))),
        }
        Ok(Self(turns))
    }
}

/// The session's pace, row `(sid, "pace")` (pace.ts `Pace`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaceRow {
    pub session: String,
    /// Tokens the main session spent, counted step by step.
    #[serde(default, serialize_with = "num")]
    pub claude: f64,
    /// Tokens the dreamer's looks spent.
    #[serde(default, serialize_with = "num")]
    pub dreamer: f64,
    #[serde(default, serialize_with = "num")]
    pub looks: f64,
    #[serde(default, serialize_with = "num")]
    pub dreams: f64,
    /// Tokens the last look cost: the estimate for the next.
    #[serde(default, rename = "lastLook", serialize_with = "num")]
    pub last_look: f64,
    /// Tokens the main session's last step spent: the estimate for a look no step of its own
    /// started.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_num"
    )]
    pub step: Option<f64>,
    /// The main session's steps already counted, by `[session, turn]`, so a replay or a reload
    /// never counts twice.
    #[serde(default, skip_serializing_if = "Turns::is_empty")]
    pub turns: Turns,
    /// Whatever else the row holds: a row is read and written back whole.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PaceRow {
    /// pace.ts `freshPace`.
    pub fn fresh(session: &str) -> Self {
        Self {
            session: session.to_string(),
            claude: 0.,
            dreamer: 0.,
            looks: 0.,
            dreams: 0.,
            last_look: 0.,
            step: None,
            turns: Turns::default(),
            extra: Map::new(),
        }
    }
}

/// The key a turn's receipt goes by (pace.ts `turnKey`): `["session","turn"]`.
pub fn turn_key(session: &str, turn: &str) -> String {
    json!([session, turn]).to_string()
}

/// An open receipt is what stops a replayed step counting again, so none is evicted while its turn
/// can still step: only ended turns are trimmed, oldest first (`OPEN_KEPT` bounds the turns that
/// never ended).
fn keep_turns(turns: Vec<(String, Receipt)>) -> Vec<(String, Receipt)> {
    let key_of = |closed: bool| -> Vec<String> {
        (turns.iter())
            .filter(|(_, receipt)| receipt.closed == closed)
            .map(|(key, _)| key.clone())
            .collect()
    };
    let (closed, open) = (key_of(true), key_of(false));
    let open = open[open.len().saturating_sub(OPEN_KEPT)..].to_vec();
    let dropped: HashSet<String> = closed[..closed.len().saturating_sub(TURNS_KEPT)]
        .iter()
        .cloned()
        .collect();
    let keep: HashSet<String> = (open.into_iter().chain(closed))
        .filter(|key| !dropped.contains(key))
        .collect();
    (turns.into_iter())
        .filter(|(key, _)| keep.contains(key))
        .collect()
}

/// A step, counted once: its tokens are Claude's spend and it is one step of the chapter (the
/// caller advances that when this returns a new pace). A step already counted (same session, turn
/// and index) returns `None`: the pace is unchanged.
pub fn count_step(pace: &PaceRow, turn: &str, index: f64, tokens: f64) -> Option<PaceRow> {
    let key = turn_key(&pace.session, turn);
    let seen = (pace.turns.0.iter())
        .find(|(k, _)| *k == key)
        .map(|(_, receipt)| receipt.clone());
    if seen.as_ref().is_some_and(|r| r.steps.contains(&index)) {
        return None;
    }
    let mut receipt = seen.unwrap_or_default();
    receipt.steps.push(index);
    // The counted turn moves to the end: it is the newest.
    let mut turns: Vec<_> = (pace.turns.0.iter())
        .filter(|(k, _)| *k != key)
        .cloned()
        .collect();
    turns.push((key, receipt));
    let mut next = pace.clone();
    next.claude += tokens;
    if tokens != 0. {
        next.step = Some(tokens);
    }
    next.turns = Turns(keep_turns(turns));
    Some(next)
}

/// The turn ended: its receipt may now be trimmed. Nothing else about it changes.
pub fn close_turn(pace: PaceRow, turn: &str) -> PaceRow {
    let key = turn_key(&pace.session, turn);
    let mut next = pace.clone();
    match next.turns.0.iter_mut().find(|(k, _)| *k == key) {
        Some((_, receipt)) if !receipt.closed => receipt.closed = true,
        _ => return pace,
    }
    next.turns = Turns(keep_turns(next.turns.0));
    next
}

// ---------------------------------------------------------------- chapter

/// A chapter of the session, row `(sid, "chapter")` (pace.ts `Chapter`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub session: String,
    /// Milliseconds.
    #[serde(serialize_with = "num")]
    pub started: f64,
    /// Claude's steps in this chapter.
    #[serde(serialize_with = "num")]
    pub steps: f64,
    /// The step the next look is due at.
    #[serde(serialize_with = "num")]
    pub due: f64,
    #[serde(serialize_with = "num")]
    pub looks: f64,
    #[serde(serialize_with = "num")]
    pub shown: f64,
    #[serde(serialize_with = "num")]
    pub adopted: f64,
    /// Counts the session's chapters: a look that outlived its chapter knows it, whatever the
    /// clock says.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "opt_num"
    )]
    pub gen: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Chapter {
    /// pace.ts `freshChapter`.
    pub fn fresh(session: &str, started: f64) -> Self {
        Self {
            session: session.to_string(),
            started,
            steps: 0.,
            due: FIRST_EVERY,
            looks: 0.,
            shown: 0.,
            adopted: 0.,
            gen: None,
            extra: Map::new(),
        }
    }
}

pub fn allowance(chapter: &Chapter) -> f64 {
    PER_CHAPTER + chapter.adopted
}

pub fn dreams_left(chapter: &Chapter) -> f64 {
    (allowance(chapter) - chapter.shown).max(0.)
}

pub fn may_look(chapter: &Chapter) -> bool {
    dreams_left(chapter) > 0. && chapter.looks < 2. * allowance(chapter)
}

/// `Math.round`: halves go up.
fn js_round(n: f64) -> f64 {
    (n + 0.5).floor()
}

/// Steps until the next look: the rest of a usual chapter over the dreams left, never under 6.
pub fn spacing(chapter: &Chapter, usual: f64) -> f64 {
    let left = dreams_left(chapter);
    if left == 0. {
        return 0.;
    }
    FIRST_EVERY.max(js_round((usual - chapter.steps) / left))
}

pub fn median(values: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = (values.iter())
        .copied()
        .filter(|v| v.is_finite() && *v > 0.)
        .collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    if sorted.is_empty() {
        return USUAL_STEPS;
    }
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        js_round((sorted[middle - 1] + sorted[middle]) / 2.)
    }
}

/// The brief's lines about the work in hand, the ones that start with `* `. Past lines, what CSR
/// handed Claude and dreams already offered never count: they move without the work moving.
pub fn facts(brief: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in brief.split('\n').filter(|line| line.starts_with("* ")) {
        let line = line.trim().to_string();
        if !out.contains(&line) {
            out.push(line);
        }
    }
    out
}

pub fn has_news(now: &[String], before: &[String]) -> bool {
    now.iter().any(|line| !before.contains(line))
}

/// Tokens the dreamer has spent past its share: 0 while it is within it. A look that cost more
/// than the guess ends here with a number above 0, and it stays until `claude` has grown enough
/// to cover it.
pub fn owed(pace: &PaceRow) -> f64 {
    (pace.dreamer - SHARE * pace.claude).max(0.)
}

/// The cost of a spend that is not known (a fork that threw may have been paid for): charged as
/// the guess.
pub fn guess(pace: &PaceRow, last_step: f64) -> f64 {
    if pace.last_look != 0. {
        pace.last_look
    } else {
        last_step
    }
}

/// What the next look would cost, guessed from the last look, else from the main session's last
/// step (a fork re-reads the same context). Nothing runs while anything is owed.
pub fn can_afford(pace: &PaceRow, last_step: f64) -> bool {
    owed(pace) == 0. && pace.dreamer + guess(pace, last_step) <= SHARE * pace.claude
}

pub fn pace_text(pace: Option<&PaceRow>, chapter: Option<&Chapter>, on: bool) -> String {
    if !on {
        return "Dreams are off. Turn them on in /config: csr-companion, Dreams.".to_string();
    }
    let now = match chapter {
        Some(chapter) if dreams_left(chapter) > 0. => format!(
            "{} of {} dreams left in this chapter; the next look in about {} of Claude's steps, \
             if CSR has something new.",
            dreams_left(chapter),
            allowance(chapter),
            (chapter.due - chapter.steps).max(0.)
        ),
        Some(_) => {
            "No dreams left in this chapter; it starts over at the next compaction.".to_string()
        }
        None => format!("{PER_CHAPTER} dreams to a chapter, which starts over at each compaction."),
    };
    let Some(pace) = pace.filter(|pace| pace.looks != 0.) else {
        return format!(
            "Dreams are on. {now} The dreamer looks only when CSR has something new about the \
             work in hand, and checks before each look that its spend stays within 1 token for \
             every 10 Claude spends. No looks yet this session."
        );
    };
    let share = if pace.claude != 0. {
        js_round(pace.dreamer / pace.claude * 100.)
    } else {
        0.
    };
    let over = owed(pace);
    let debt = if over > 0. {
        format!(
            " A look ran over its share by {}; the next waits until Claude's spending has caught \
             up.",
            k(over)
        )
    } else {
        String::new()
    };
    let plural =
        |n: f64, one: &str, many: &str| format!("{n} {}", if n == 1. { one } else { many });
    format!(
        "Dreams are on. {now} This session: {}, {}, {} tokens ({share}% of Claude's {}).{debt}",
        plural(pace.looks, "look", "looks"),
        plural(pace.dreams, "dream", "dreams"),
        k(pace.dreamer),
        k(pace.claude)
    )
}

/// The next look, counted from the step that started this one (Claude's steps go on while a look
/// runs): soon after a look that cost nothing.
pub fn soon(base: f64) -> impl FnOnce(Chapter) -> Chapter {
    move |chapter| Chapter {
        due: base + FIRST_EVERY,
        ..chapter
    }
}

/// The same, else the chapter's dreams left spread over the rest of it.
pub fn spread(base: f64, usual: f64) -> impl FnOnce(Chapter) -> Chapter {
    move |chapter| {
        let due = if may_look(&chapter) {
            let from = Chapter {
                steps: base,
                ..chapter.clone()
            };
            base + spacing(&from, usual)
        } else {
            NEVER
        };
        Chapter { due, ..chapter }
    }
}

/// A chapter left as it is, except that its dreams spent end the look schedule (a completion's
/// look used one).
pub fn kept(chapter: Chapter) -> Chapter {
    if may_look(&chapter) {
        chapter
    } else {
        Chapter {
            due: NEVER,
            ..chapter
        }
    }
}

/// The two sides of the spend check, kept on every look: `budget` can be checked against what
/// Claude really spent.
pub fn sides_of(pace: &PaceRow, last_step: f64) -> Value {
    json!({
        "claude": num_value(pace.claude), "dreamer": num_value(pace.dreamer),
        "next": num_value(guess(pace, last_step)), "owed": num_value(owed(pace)),
    })
}

/// The four counts of a usage (register.tsx `tokensOf`).
pub fn tokens_of(usage: &Value) -> f64 {
    [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .map(|name| usage[*name].as_f64().unwrap_or(0.))
    .sum()
}

// ---------------------------------------------------------------- the process's state

/// Where `--keep` records go: `(cwd, record)`, as pane.py's `--repo cwd --keep JSON`.
pub type Keep = Arc<dyn Fn(String, Value) -> BoxFuture<()> + Send + Sync>;

/// What pace keeps for the life of the process (`cx.kept`), and what the dreamer's `step` entry
/// reads right after this package's.
#[derive(Default)]
pub struct Pace {
    /// The step just handled was a new one of the main session: counted in the pace and the
    /// chapter. False for a replay, a subagent's step, a child's, and while dreams are off.
    pub fresh: bool,
    /// The chapter after that step's count (`changeChapter`'s result), when `fresh`.
    pub chapter: Option<Chapter>,
    keep: Option<Keep>,
    pace_queue: Arc<tokio::sync::Mutex<()>>,
    chapter_queue: Arc<tokio::sync::Mutex<()>>,
}

/// Whether the step just handled was a new step of the main session (`fresh` in the TypeScript).
pub fn fresh(cx: &Ctx) -> bool {
    cx.kept(|p: &mut Pace| p.fresh)
}

/// The chapter after the step just counted, if one was.
pub fn counted_chapter(cx: &Ctx) -> Option<Chapter> {
    cx.kept(|p: &mut Pace| p.chapter.clone())
}

/// Use `keep` for this process's `--keep` records (the entries below install theirs each call;
/// the dreamer may install its own).
pub fn install_keep(cx: &Ctx, keep: &Keep) {
    cx.kept(|p: &mut Pace| p.keep = Some(keep.clone()));
}

/// `--keep` in this process: the pane's own `--keep` mode, which adds the repo's root to a record
/// of a kind that has one and appends it to the dream log.
pub fn keep_record(cwd: &str, record: Value) -> Result<()> {
    keep_record_in(&Paths::from_env(), cwd, record)
}

/// `keep_record` under `paths` (HOME, but a test's own).
pub fn keep_record_in(paths: &Paths, cwd: &str, mut record: Value) -> Result<()> {
    let kinds = ["dream", "look", "chapter", "ysk", "compact"];
    if kinds.contains(&record["kind"].as_str().unwrap_or("")) {
        let repo = Path::new(cwd);
        let root = paths::git(repo, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
            .or_else(|| Db::open(paths).ok()?.pane_root(repo));
        record["repo"] = serde_json::to_value(root)?;
    }
    dreams::keep(paths, &record)?;
    Ok(())
}

pub fn real_keep() -> Keep {
    Arc::new(|cwd, record| {
        Box::pin(async move {
            let _ = tokio::task::spawn_blocking(move || keep_record(&cwd, record)).await;
        })
    })
}

/// What a process that was given no `Keep` uses: the real one, and in the crate's own tests none,
/// so no test writes to a dream log by accident.
fn default_keep() -> Keep {
    #[cfg(test)]
    return Arc::new(|_, _| Box::pin(async {}));
    #[cfg(not(test))]
    real_keep()
}

async fn write_keep(cx: &Ctx, record: Value) {
    let keep = (cx.kept(|p: &mut Pace| p.keep.clone())).unwrap_or_else(default_keep);
    keep(cx.cwd.clone(), record).await;
}

// ---------------------------------------------------------------- pace changes

/// This session's pace (register.tsx `paceOf`): its row, or a fresh one.
pub fn pace_of(cx: &Ctx) -> PaceRow {
    let fresh = PaceRow::fresh(&cx.sid);
    let Some(Value::Object(row)) = cx.store.get(&cx.sid, "pace").ok().flatten() else {
        return fresh;
    };
    // A row that does not name this session is not this session's (`value?.session === session`).
    if row.get("session") != Some(&json!(cx.sid)) {
        return fresh;
    }
    let mut base = serde_json::to_value(&fresh).unwrap_or(Value::Null);
    if let Some(fields) = base.as_object_mut() {
        fields.extend(row);
    }
    serde_json::from_value::<PaceRow>(base)
        .ok()
        .filter(|pace| pace.session == cx.sid)
        .unwrap_or(fresh)
}

/// A check on whether a change is still wanted, made after the pace is read (`held`).
pub type Held<'a> = Option<&'a (dyn Fn() -> bool + Send + Sync)>;

/// Change the pace as one step (register.tsx `changePace`). Every change goes through one queue:
/// Claude's steps count what they spend while a look writes what it spent, and a change that read
/// the pace before the other wrote it would drop the other's tokens. `held` false: the pace is
/// returned as it is.
pub async fn change_pace(
    cx: &Ctx,
    change: impl FnOnce(PaceRow) -> PaceRow + Send,
    held: Held<'_>,
) -> Result<PaceRow> {
    let queue = cx.kept(|p: &mut Pace| p.pace_queue.clone());
    let _turn = queue.lock().await;
    let current = pace_of(cx);
    if held.is_some_and(|held| !held()) {
        return Ok(current);
    }
    let pace = change(current);
    cx.store
        .set(&pace.session, "pace", &serde_json::to_value(&pace)?)?;
    Ok(pace)
}

// ---------------------------------------------------------------- chapters

fn chapter_row(cx: &Ctx) -> Option<Value> {
    cx.store.get(&cx.sid, "chapter").ok().flatten()
}

/// This chapter of the session (register.tsx `chapterOf`): a fresh one for a new session, and
/// after two hours without a compaction.
pub async fn chapter_of(cx: &Ctx) -> Result<Chapter> {
    let value = chapter_row(cx);
    let now = cx.clock.now();
    let mine = value
        .as_ref()
        .is_some_and(|v| v["session"] == cx.sid.as_str());
    if let (true, Some(Value::Object(row))) = (mine, &value) {
        let mut base = serde_json::to_value(Chapter::fresh(&cx.sid, now))?;
        let fields = base.as_object_mut().context("a chapter is an object")?;
        fields.extend(row.clone());
        let live = row["started"]
            .as_f64()
            .is_some_and(|at| now - at < CHAPTER_MS);
        if let (true, Ok(chapter)) = (live, serde_json::from_value::<Chapter>(base)) {
            return Ok(chapter);
        }
    }
    new_chapter(cx, if mine { "two hours" } else { "start" }).await
}

/// A chapter starts over: five dreams again, the first look soon. A chapter a compaction ended
/// teaches how long your chapters run; the dream log gets a line, so the pane counts from here.
pub async fn new_chapter(cx: &Ctx, why: &str) -> Result<Chapter> {
    let old = chapter_row(cx).filter(|old| old["session"] == cx.sid.as_str());
    let steps = old
        .as_ref()
        .and_then(|old| old["steps"].as_f64())
        .unwrap_or(0.);
    if why == "compact" && steps > 0. {
        let mut lengths = match cx.store.get(MACHINE, "chapterSteps").ok().flatten() {
            Some(Value::Array(lengths)) => lengths,
            _ => Vec::new(),
        };
        lengths.push(num_value(steps));
        let from = lengths.len().saturating_sub(CHAPTERS_KEPT);
        let kept = Value::Array(lengths[from..].to_vec());
        cx.store.set(MACHINE, "chapterSteps", &kept)?;
    }
    let at = cx.clock.now();
    let before = old
        .as_ref()
        .map_or(0., |old| old["gen"].as_f64().unwrap_or(0.));
    let chapter = Chapter {
        gen: Some(before + 1.),
        ..Chapter::fresh(&cx.sid, at)
    };
    cx.store
        .set(&cx.sid, "chapter", &serde_json::to_value(&chapter)?)?;
    let interactive = cx.session.lock().ok().and_then(|s| s.interactive);
    let record = json!({
        "kind": "chapter", "at": at / 1000., "session": cx.sid, "why": why,
        "left": num_value(dreams_left(&chapter)), "of": num_value(allowance(&chapter)),
        "interactive": interactive, // who started it: null where it was never learnt
    });
    write_keep(cx, record).await;
    Ok(chapter)
}

/// `new_chapter` through the chapter's queue (register.tsx `serially(() => newChapter(..))`).
pub async fn new_chapter_serially(cx: &Ctx, why: &str) -> Result<Chapter> {
    let queue = cx.kept(|p: &mut Pace| p.chapter_queue.clone());
    let _turn = queue.lock().await;
    new_chapter(cx, why).await
}

/// The chapter as it is now, changed: read fresh inside the queue (register.tsx `changeChapter`).
pub async fn change_chapter(
    cx: &Ctx,
    change: impl FnOnce(Chapter) -> Chapter + Send,
    held: Held<'_>,
) -> Result<Chapter> {
    let queue = cx.kept(|p: &mut Pace| p.chapter_queue.clone());
    let _turn = queue.lock().await;
    let current = chapter_of(cx).await?;
    if held.is_some_and(|held| !held()) {
        return Ok(current);
    }
    let chapter = change(current);
    cx.store
        .set(&cx.sid, "chapter", &serde_json::to_value(&chapter)?)?;
    Ok(chapter)
}

/// How long your chapters usually run: the median of the lengths the machine remembers.
pub fn usual_steps(cx: &Ctx) -> f64 {
    let lengths = match cx.store.get(MACHINE, "chapterSteps").ok().flatten() {
        Some(Value::Array(lengths)) => lengths,
        _ => Vec::new(),
    };
    let lengths: Vec<f64> = (lengths.iter())
        .map(|n| n.as_f64().unwrap_or(f64::NAN))
        .collect();
    median(&lengths)
}

// ---------------------------------------------------------------- the sweep

/// The sessions that have a chapter, and when it started (0 where the row says nothing).
fn session_chapters(cx: &Ctx) -> Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for sid in cx.store.sids()? {
        if sid == MACHINE
            || !cx
                .store
                .keys(&sid, "chapter")?
                .iter()
                .any(|k| k == "chapter")
        {
            continue;
        }
        let row = cx.store.get(&sid, "chapter").ok().flatten();
        let started = row.and_then(|row| row["started"].as_f64());
        out.push((sid, started.unwrap_or(0.)));
    }
    Ok(out)
}

/// The per-session keys of all but the newest sessions go, and the unkeyed ones every process once
/// shared (register.tsx `sweepSessions`). Only a session whose chapter is known and past the
/// newest `SESSIONS_KEPT` goes: one with no chapter yet may be live in another window, its first
/// look not taken.
pub fn sweep_sessions(cx: &Ctx) -> Result<()> {
    for key in PER_SESSION {
        cx.store.delete(MACHINE, key)?;
    }
    let mut started = session_chapters(cx)?;
    started.sort_by(|a, b| b.1.total_cmp(&a.1)); // newest first; ties stay in order
    let gone: Vec<String> = (started.into_iter())
        .skip(SESSIONS_KEPT)
        .map(|(sid, _)| sid)
        .filter(|sid| *sid != cx.sid)
        .collect();
    for sid in &gone {
        for key in SWEPT {
            cx.store.delete(sid, key)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- the entries

/// Whether dreams are on: unless the person turned them off in `/config`.
async fn dreams_on(cx: &Ctx) -> bool {
    Options::of(&config_rows(cx).await).dreams
}

fn has_agent(e: &Value) -> bool {
    !e["agentId"].is_null()
}

/// `turn_complete` (register.tsx 1918): the turn ends, its receipt may now be trimmed.
fn on_turn_complete(keep: Keep) -> Handler {
    handler(move |cx, e| {
        let keep = keep.clone();
        async move {
            install_keep(&cx, &keep);
            // An agent's turn is the fleet's; the pace is the main session's.
            if has_agent(&e) {
                return Ok(Reply::none());
            }
            if dreams_on(&cx).await && is_person(&cx).await {
                let turn = e["turnId"].as_str().unwrap_or_default().to_string();
                let _ = change_pace(&cx, |pace| close_turn(pace, &turn), None).await;
            }
            Ok(Reply::none())
        }
    })
}

/// `step` (register.tsx 1988-2000): `e` is `{ e, result }`. A step is one index of one turn,
/// counted once in the pace's receipts: a replay of it adds no usage and is no step of the
/// chapter.
fn on_step(keep: Keep) -> Handler {
    handler(move |cx, told| {
        let keep = keep.clone();
        async move {
            install_keep(&cx, &keep);
            cx.kept(|p: &mut Pace| {
                p.fresh = false;
                p.chapter = None;
            });
            let (e, result) = (&told["e"], &told["result"]);
            // A subagent's step is no spend of Claude's and no step of the chapter, and starts no
            // look of the main schedule.
            if has_agent(e) {
                return Ok(Reply::none());
            }
            if !(dreams_on(&cx).await && is_person(&cx).await) {
                return Ok(Reply::none());
            }
            let spent = match &result["usage"] {
                usage if usage.is_object() => tokens_of(usage),
                _ => 0.,
            };
            let turn = (result["turnId"].as_str())
                .or(e["turnId"].as_str())
                .unwrap_or_default();
            let index = (result["index"].as_f64())
                .or(e["index"].as_f64())
                .unwrap_or(-1.);
            let mut counted = false;
            change_pace(
                &cx,
                |pace| match count_step(&pace, turn, index, spent) {
                    Some(next) => {
                        counted = true;
                        next
                    }
                    None => pace,
                },
                None,
            )
            .await?;
            if !counted {
                return Ok(Reply::none());
            }
            // Steps counted in the store, so a restart of this process does not start the count
            // over.
            let chapter = change_chapter(
                &cx,
                |chapter| Chapter {
                    steps: chapter.steps + 1.,
                    ..chapter
                },
                None,
            )
            .await?;
            cx.kept(|p: &mut Pace| {
                p.fresh = true;
                p.chapter = Some(chapter);
            });
            Ok(Reply::none())
        }
    })
}

/// `chapter`, before the compaction (register.tsx 2047): what only the event knows.
fn on_compact_event() -> Handler {
    handler(|_cx, e| async move {
        let skip = has_agent(&e) || e["trigger"] == "precompute";
        Ok(Reply {
            left: Some(json!({ "skip": skip })),
            ..Reply::none()
        })
    })
}

/// `chapter`, after it (register.tsx 2047-2070): a compaction that went through starts the next
/// chapter. `e` is what the compaction returned.
fn on_compact_after(keep: Keep) -> Handler {
    handler(move |cx, result| {
        let keep = keep.clone();
        async move {
            install_keep(&cx, &keep);
            let skip = cx.left.as_ref().is_some_and(|left| left["skip"] == true);
            if skip || !result["skip"].is_null() || !dreams_on(&cx).await {
                return Ok(Reply::none());
            }
            if !is_person(&cx).await {
                return Ok(Reply::none());
            }
            new_chapter_serially(&cx, "compact").await?;
            Ok(Reply::none())
        }
    })
}

/// `start` (register.tsx 2394): a start that is not a person's does nothing here.
fn on_start(keep: Keep) -> Handler {
    handler(move |cx, e| {
        let keep = keep.clone();
        async move {
            install_keep(&cx, &keep);
            if e["isInteractive"] != true {
                return Ok(Reply::none());
            }
            sweep_sessions(&cx)?;
            Ok(Reply::none())
        }
    })
}

pub fn register_with(t: &mut Table, keep: Keep) {
    t.on("turn_complete", "pace", on_turn_complete(keep.clone()));
    t.after("step", "pace", on_step(keep.clone()));
    let (before, after) = (on_compact_event(), on_compact_after(keep.clone()));
    t.gate("chapter", "pace", before, after);
    t.on("start", "pace", on_start(keep));
}

pub fn register(t: &mut Table) {
    register_with(t, real_keep());
}

#[cfg(test)]
mod tests;
