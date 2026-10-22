//! What CSR cost in the plan's week and what you took up: pane.py's `plan_of`, `narrative_week`,
//! `week_value` and `Spend`, behind the info page's "What CSR cost you this week" facts.
use super::{array, db::Db, masthead, num, paths::Paths, private, pyfmt, string};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const WEEK: f64 = 604800.;
const BOUND_MODEL: &str = "opus-5-5";
const SCAN_LIST_EVERY: f64 = 60.;
const SPEND_EVERY: f64 = 30.;
const SCAN_LINES: usize = 256;
const SPEND_FILE: &str = "spend.json";

/// The window the value line counts in: `{mode, start, used}`. A subscription's status line
/// carries `rate_limits.seven_day` (the session's own, else the account's newest): mode `plan`,
/// the week that resets at its `resets_at`. With none, mode `api`: the last seven days.
pub fn plan_of(fields: &Value, limits: &Value, now: f64) -> Value {
    for source in [&fields["rate_limits"], limits] {
        let week = &source["seven_day"];
        let Some(mut resets) = week
            .is_object()
            .then(|| masthead::seconds_of(&week["resets_at"]))
            .flatten()
        else {
            continue;
        };
        let mut used = masthead::percent_of(&week["used_percentage"]);
        if resets <= now {
            resets += WEEK * (1. + ((now - resets) / WEEK).floor());
            used = None;
        }
        return json!({"mode": "plan", "start": now.min(resets - WEEK), "used": used});
    }
    json!({"mode": "api", "start": now - WEEK, "used": null})
}

/// US$ at list price for one request's counts; None when the model has no price.
pub fn cost_of(model: &str, counts: [i64; 5]) -> Option<f64> {
    crate::journal::composer::list_cost(model, counts)
}

/// (input, output, cache read, cache write, of it kept an hour) from a usage in the API's spelling.
pub fn usage_counts(usage: &Value) -> [i64; 5] {
    let count = |v: &Value| -> i64 {
        if v.is_number() {
            (v.as_f64().unwrap_or(0.) as i64).max(0)
        } else {
            0
        }
    };
    [
        count(&usage["input_tokens"]),
        count(&usage["output_tokens"]),
        count(&usage["cache_read_input_tokens"]),
        count(&usage["cache_creation_input_tokens"]),
        count(&usage["cache_creation"]["ephemeral_1h_input_tokens"]),
    ]
}

/// csr-engine's own model calls since `start`, from `narrative_usage`: `{calls, tokens, usd,
/// unpriced}`. `{}` when the table cannot be read, as pane.py's fail_open left it.
pub fn narrative_week(db: &Db, start: f64) -> Value {
    let since = chrono::DateTime::from_timestamp(start as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
        .unwrap_or_default();
    let Ok(rows) = db.try_query(
        "select model, count(*), sum(input_tokens), sum(output_tokens), sum(cache_read_tokens), \
         sum(cache_creation_tokens) from narrative_usage where ts >= ? group by model",
        &[json!(since)],
    ) else {
        return json!({});
    };
    let (mut calls, mut tokens, mut usd) = (0, 0, 0.);
    let mut unpriced = vec![];
    for row in rows {
        let model = string(&row[0]);
        let counts = [2, 3, 4, 5].map(|i| num(&row[i]) as i64);
        if model.to_lowercase().contains("haiku") {
            calls += num(&row[1]) as i64;
            tokens += counts.iter().sum::<i64>();
        }
        let cost = if counts.iter().any(|n| *n != 0) {
            crate::journal::composer::list_cost_summed(
                model,
                [counts[0], counts[1], counts[2], counts[3], 0],
            )
        } else {
            Some(0.)
        };
        match cost {
            Some(c) => usd += c,
            None => unpriced.push(model.to_owned()),
        }
    }
    json!({
        "calls": calls,
        "tokens": tokens,
        "usd": if unpriced.is_empty() { json!(usd) } else { Value::Null },
        "unpriced": unpriced,
    })
}

/// Each line of a JSONL log that is an object; a missing log, or a line that is not one, gives
/// nothing.
pub fn log_lines(path: &Path) -> Vec<Value> {
    std::fs::read(path)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(Value::is_object)
                .collect()
        })
        .unwrap_or_default()
}
fn dream_logs(paths: &Paths) -> Vec<PathBuf> {
    let one = paths.keep.join("dreams.jsonl");
    let former = paths
        .home
        .join(".claude-self-reflect/csr-shifts/dreams.jsonl");
    if one == former {
        vec![one]
    } else {
        vec![one, former]
    }
}
fn float_of(v: &Value) -> Option<f64> {
    match v {
        Value::Null => Some(0.),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}
fn since(record: &Value, start: f64) -> bool {
    float_of(&record["at"]).is_some_and(|at| at >= start)
}
fn dreamer_lines(paths: &Paths, start: f64) -> Vec<Value> {
    dream_logs(paths)
        .iter()
        .flat_map(|path| log_lines(path))
        .filter(|r| matches!(string(&r["kind"]), "look" | "dream") && since(r, start))
        .collect()
}
fn look_tokens(record: &Value) -> i64 {
    match &record["tokens"] {
        Value::Number(n) => (n.as_f64().unwrap_or(0.) as i64).max(0),
        Value::String(s) => s.trim().parse::<i64>().unwrap_or(0).max(0),
        _ => 0,
    }
}
fn called_model(record: &Value) -> bool {
    record["usage"].is_object() || look_tokens(record) > 0
}
fn look_cost(record: &Value) -> (Option<f64>, bool) {
    if record["usage"].is_object() {
        return (
            cost_of(string(&record["model"]), usage_counts(&record["usage"])),
            false,
        );
    }
    let tokens = look_tokens(record);
    if tokens > 0 {
        let price = crate::journal::composer::model_price(BOUND_MODEL).map_or(0., |p| p.0);
        (Some(tokens as f64 * price / 1e6), true)
    } else {
        (Some(0.), false)
    }
}
fn looks_cost(lines: &[Value]) -> (Option<f64>, bool) {
    let (mut total, mut bound) = (0., false);
    for record in lines {
        match look_cost(record) {
            (Some(cost), upper) => {
                total += cost;
                bound |= upper;
            }
            (None, _) => return (None, false),
        }
    }
    (Some(total), bound)
}
/// The dreams you took up since `start` (helpful, or handed to Claude), each once. One you said no
/// to after is not counted.
fn dreams_taken(paths: &Paths, start: f64) -> usize {
    let answers: Vec<Value> = dream_logs(paths)
        .iter()
        .flat_map(|path| log_lines(path))
        .filter(|r| r["kind"] == "answer" && super::truthy(&r["id"]))
        .collect();
    let id = |r: &Value| r["id"].to_string();
    let rejected: BTreeSet<_> = answers
        .iter()
        .filter(|r| r["answer"] == "rejected")
        .map(id)
        .collect();
    answers
        .iter()
        .filter(|r| super::dreams::ADOPTED.contains(&string(&r["answer"])) && since(r, start))
        .map(id)
        .collect::<BTreeSet<_>>()
        .difference(&rejected)
        .count()
}
/// The Lost and found threads whose ask you typed into Claude's prompt since `start`, each once.
fn threads_taken(paths: &Paths, start: f64) -> usize {
    log_lines(&paths.keep.join("lost-found.jsonl"))
        .iter()
        .filter(|r| r["answer"] == "sent" && super::truthy(&r["key"]) && since(r, start))
        .map(|r| r["key"].to_string())
        .collect::<BTreeSet<_>>()
        .len()
}
/// The items liked since `start` and still liked: the last line per key wins, an expand is no like.
fn likes_since(paths: &Paths, start: f64) -> usize {
    let mut last: BTreeMap<String, (bool, f64)> = BTreeMap::new();
    for r in log_lines(&paths.keep.join("likes.jsonl")) {
        let signal = r.get("signal").map_or("explicit_like", |v| string(v));
        if let (Some(key), "explicit_like") = (r["key"].as_str(), signal) {
            if let Some(at) = float_of(&r["at"]) {
                last.insert(key.to_owned(), (super::truthy(&r["liked"]), at));
            }
        }
    }
    last.values()
        .filter(|(liked, at)| *liked && *at >= start)
        .count()
}
/// The share of the weekly limit CSR used, in points of the plan's meter.
fn plan_share(used: f64, csr: f64, local: f64, narrative: f64, looks: f64) -> Option<f64> {
    let seen = local + narrative + looks;
    (seen > 0.).then(|| used * csr / seen)
}
fn week_figure(plan: &Value, narrative: &Value, lines: &[Value], spend: &Value) -> Value {
    let (looks, bound) = looks_cost(lines);
    let (Some(looks), Some(narrative_usd)) = (looks, narrative["usd"].as_f64()) else {
        return Value::Null;
    };
    let usd = narrative_usd + looks;
    let share = (plan["mode"] == "plan"
        && !plan["used"].is_null()
        && num(&spend["messages"]) != 0.
        && array(&spend["unpriced"]).is_empty()
        && usd > 0.)
        .then(|| {
            plan_share(
                num(&plan["used"]),
                usd,
                num(&spend["usd"]),
                narrative_usd,
                looks,
            )
        })
        .flatten();
    json!({"usd": usd, "bound": bound, "share": share})
}
/// What the value line counts, all in the plan's one window: `{mode, calls, looks, tokens, dreams,
/// threads, likes, figure}`.
pub fn week_value(paths: &Paths, plan: &Value, narrative: &Value, spend: &Value) -> Value {
    let start = num(&plan["start"]);
    let lines = dreamer_lines(paths, start);
    json!({
        "mode": plan["mode"],
        "calls": num(&narrative["calls"]) as i64,
        "looks": lines.iter().filter(|r| called_model(r)).count(),
        "tokens": num(&narrative["tokens"]) as i64 + lines.iter().map(look_tokens).sum::<i64>(),
        "dreams": dreams_taken(paths, start),
        "threads": threads_taken(paths, start),
        "likes": likes_since(paths, start),
        "figure": week_figure(plan, narrative, &lines, spend),
    })
}

pub fn plural(n: i64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}
pub fn money_text(usd: Option<f64>, bound: bool) -> String {
    let Some(usd) = usd.filter(|u| *u > 0.) else {
        return String::new();
    };
    if bound {
        let cents = ((usd * 100. * 1e6).round_ties_even() / 1e6).ceil();
        return if cents >= 100. {
            format!("≤${}", pyfmt::fixed(cents / 100., 2))
        } else {
            format!("≤{}¢", cents as i64)
        };
    }
    let cents = pyfmt::round(usd * 100.);
    if cents >= 100. {
        format!("${}", pyfmt::fixed(usd, 2))
    } else if cents < 1. {
        "<1¢".into()
    } else {
        format!("{}¢", cents as i64)
    }
}
pub fn share_text(percent: Option<f64>, bound: bool) -> String {
    let Some(percent) = percent.filter(|p| *p > 0.) else {
        return String::new();
    };
    let mark = if bound { "≤" } else { "≈" };
    let digits = if percent >= 1. {
        1
    } else {
        -(percent.log10().floor() as i32)
    };
    let scale = 10f64.powi(digits);
    let shown = if bound {
        ((percent * scale * 1e6).round_ties_even() / 1e6).ceil()
    } else {
        pyfmt::round(percent * scale)
    } / scale;
    if shown >= 1. {
        return format!("{mark}{}%", pyfmt::fixed(shown, 1));
    }
    let digits = -((shown.log10() + 1e-9).floor() as i32);
    format!("{mark}{}%", pyfmt::fixed(shown, digits.max(0) as usize))
}

/// Presentation of the existing weekly figure; no accounting or extra reads happen here.
#[derive(Clone, Debug)]
pub struct InstrumentCost {
    pub usd: Option<f64>,
    pub share: Option<f64>,
    pub bound: bool,
    pub calls: i64,
}
impl InstrumentCost {
    pub fn from_week(week: &Value) -> Self {
        let figure = &week["figure"];
        let usd = figure["usd"].as_f64().filter(|n| n.is_finite() && *n >= 0.);
        Self {
            usd,
            share: if usd == Some(0.) {
                Some(0.)
            } else {
                usd.and_then(|_| figure["share"].as_f64())
                    .filter(|n| n.is_finite() && *n >= 0.)
            },
            bound: figure["bound"] == true,
            calls: (num(&week["calls"]) + num(&week["looks"])) as i64,
        }
    }
    pub fn share_label(&self) -> String {
        match self.share {
            None => "—".into(),
            Some(0.) => "0".into(),
            Some(n) => share_text(Some(n), self.bound)
                .trim_start_matches('≈')
                .trim_end_matches('%')
                .into(),
        }
    }
    pub fn money_line(&self) -> String {
        match self.usd {
            None => "CSR —".into(),
            Some(0.) => "$0 this week".into(),
            usd => format!("{} this week", money_text(usd, self.bound)),
        }
    }
    pub fn api_line(&self) -> String {
        let cost = if self.usd == Some(0.) {
            "CSR 0".into()
        } else {
            self.money_line()
        };
        format!("{cost} · {}", plural(self.calls, "call"))
    }
    /// Fractions of the same full meter as the quota; never exaggerate a tiny share.
    pub fn segment(&self, used: f64) -> Option<(f32, f32)> {
        self.share
            .filter(|share| *share > 0.)
            .map(|share| (0., (share.min(used.max(0.)) / 100.).clamp(0., 1.) as f32))
    }
}
/// What CSR cost, the figure alone: `≈0.04%` of the weekly limit on a subscription, `$1.69` at list
/// price with an API key; empty when it cannot be said.
pub fn fuel_text(v: &Value) -> String {
    let figure = &v["figure"];
    if v["mode"] == "plan" {
        share_text(figure["share"].as_f64(), figure["bound"] == true)
    } else {
        money_text(figure["usd"].as_f64(), figure["bound"] == true)
    }
}
/// What CSR cost, as the plan counts it.
pub fn cost_piece(v: &Value) -> String {
    let said = fuel_text(v);
    if said.is_empty() {
        String::new()
    } else if v["mode"] == "plan" {
        format!("{said} of your weekly limit")
    } else {
        format!("{said} at list price")
    }
}

/// What this machine's sessions spent in the plan's week, from Claude Code's own transcripts, read
/// a little at a time and kept in `spend.json` so a restarted pane goes on from where it stopped.
pub struct Spend {
    path: PathBuf,
    projects: PathBuf,
    persist: bool,
    start: Option<f64>,
    since: String,
    files: BTreeMap<String, Entry>,
    queue: Vec<String>,
    listed: Option<f64>,
    saved: f64,
    dirty: bool,
    pub ready: bool,
    sums: Option<Value>,
}
#[derive(Default)]
struct Entry {
    inode: u64,
    offset: u64,
    msgs: BTreeMap<String, (Value, [i64; 5])>,
}
impl Spend {
    pub fn new(paths: &Paths, persist: bool) -> Self {
        Self {
            path: paths.keep.join(SPEND_FILE),
            projects: paths.projects.clone(),
            persist,
            start: None,
            since: String::new(),
            files: BTreeMap::new(),
            queue: vec![],
            listed: None,
            saved: 0.,
            dirty: false,
            ready: false,
            sums: None,
        }
    }
    pub fn start(&self) -> Option<f64> {
        self.start
    }
    fn begin(&mut self, start: f64) {
        self.start = Some(start);
        self.files.clear();
        self.queue.clear();
        self.listed = None;
        self.ready = false;
        self.sums = None;
        self.since = chrono::DateTime::from_timestamp(start as i64, 0)
            .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_default();
        let kept = super::paths::read_json(&self.path);
        if kept["start"].as_f64() == Some(start) {
            for (path, entry) in kept["files"].as_object().into_iter().flatten() {
                if let Some(msgs) = entry["msgs"].as_object() {
                    self.files.insert(
                        path.clone(),
                        Entry {
                            inode: entry["inode"].as_u64().unwrap_or(0),
                            offset: entry["offset"].as_u64().unwrap_or(0),
                            msgs: msgs
                                .iter()
                                .filter_map(|(key, row)| {
                                    let row = row.as_array()?;
                                    let mut counts = [0i64; 5];
                                    for (i, count) in counts.iter_mut().enumerate() {
                                        *count = row.get(i + 1)?.as_i64()?;
                                    }
                                    Some((key.clone(), (row[0].clone(), counts)))
                                })
                                .collect(),
                        },
                    );
                }
            }
        }
    }
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                Self::walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    /// The transcripts written since the week began, each queued while there is more of it to read.
    fn list(&mut self) {
        self.queue.clear();
        let start = self.start.unwrap_or(0.);
        let mut found = vec![];
        Self::walk(&self.projects, &mut found);
        for path in found {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let modified = meta.mtime() as f64 + meta.mtime_nsec() as f64 / 1e9;
            if modified < start {
                continue;
            }
            let key = path.to_string_lossy().into_owned();
            let stale = self
                .files
                .get(&key)
                .is_none_or(|e| e.inode != meta.ino() || meta.len() < e.offset);
            if stale {
                self.files.insert(
                    key.clone(),
                    Entry {
                        inode: meta.ino(),
                        ..Entry::default()
                    },
                );
                self.sums = None;
                self.dirty = true;
            }
            if meta.len() > self.files[&key].offset {
                self.queue.push(key);
            }
        }
    }
    /// One transcript line: kept when it is an assistant message of this week.
    fn take(since: &str, entry: &mut Entry, line: &[u8]) -> bool {
        const STAMP: &[u8] = b"\"timestamp\":\"";
        if !line.windows(11).any(|w| w == b"\"assistant\"") {
            return false;
        }
        if let Some(at) = line.windows(STAMP.len()).rposition(|w| w == STAMP) {
            let from = at + STAMP.len();
            if let Some(stamp) = line.get(from..from + 19) {
                if String::from_utf8_lossy(stamp).as_ref() < since {
                    return false;
                }
            }
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            return false;
        };
        let message = &record["message"];
        if record["type"] != "assistant"
            || !message.is_object()
            || !message["usage"].is_object()
            || message["model"] == "<synthetic>"
            || string(&record["timestamp"]) < since
        {
            return false;
        }
        let py = |v: &Value| match v {
            Value::Null => "None".to_owned(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let key = format!("{}|{}", py(&message["id"]), py(&record["requestId"]));
        let counts = usage_counts(&message["usage"]);
        let merged = match entry.msgs.get(&key) {
            Some((_, old)) => std::array::from_fn(|i| old[i].max(counts[i])),
            None => counts,
        };
        entry.msgs.insert(key, (message["model"].clone(), merged));
        true
    }
    /// Read one transcript on from its place until its end or the budget's; true at its end. A
    /// last line still being written (no newline yet) waits for the next read.
    fn read(&mut self, path: &str, began: Instant, budget: Duration) -> bool {
        let since = self.since.clone();
        let Some(entry) = self.files.get_mut(path) else {
            return true;
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return true;
        };
        if file.seek(SeekFrom::Start(entry.offset)).is_err() {
            return true;
        }
        let mut reader = std::io::BufReader::new(file);
        let mut line = vec![];
        let mut changed = false;
        let mut k = 0;
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !line.ends_with(b"\n") {
                self.sums = self.sums.take().filter(|_| !changed);
                self.dirty |= changed;
                return true;
            }
            entry.offset += line.len() as u64;
            changed |= Self::take(&since, entry, &line);
            k += 1;
            if k % SCAN_LINES == 0 && began.elapsed() >= budget {
                if changed {
                    self.sums = None;
                    self.dirty = true;
                }
                return false;
            }
        }
        if changed {
            self.sums = None;
        }
        self.dirty = true;
        true
    }
    /// Read on for at most `budget`; true once every transcript has been read to its end.
    /// When the transcripts are listed again: a new file, or a long-quiet one written to, is only
    /// found then, and nothing in the pane's gate sees them.
    pub fn next_listing(&self) -> Option<f64> {
        self.listed.map(|at| at + SCAN_LIST_EVERY)
    }
    pub fn step(&mut self, start: f64, now: f64, budget: Duration) -> bool {
        let began = Instant::now();
        if self.start != Some(start) {
            self.begin(start);
        }
        if self.listed.is_none_or(|at| now - at >= SCAN_LIST_EVERY) {
            self.list();
            self.listed = Some(now);
        }
        while let Some(path) = self.queue.first().cloned() {
            if began.elapsed() >= budget {
                break;
            }
            if self.read(&path, began, budget) {
                self.queue.remove(0);
            }
        }
        let caught = !self.ready && self.queue.is_empty();
        self.ready = self.ready || self.queue.is_empty();
        self.save(now, caught);
        self.ready
    }
    fn save(&mut self, now: f64, force: bool) {
        if !self.persist || !self.dirty || (!force && now - self.saved < SPEND_EVERY) {
            return;
        }
        let files: serde_json::Map<String, Value> = self
            .files
            .iter()
            .map(|(path, e)| {
                let msgs: serde_json::Map<String, Value> = e
                    .msgs
                    .iter()
                    .map(|(key, (model, c))| {
                        (key.clone(), json!([model, c[0], c[1], c[2], c[3], c[4]]))
                    })
                    .collect();
                (
                    path.clone(),
                    json!({"inode": e.inode, "offset": e.offset, "msgs": msgs}),
                )
            })
            .collect();
        let body = json!({"start": self.start, "files": files}).to_string();
        if private::write_atomic(
            &self.path,
            &body,
            self.path.parent().unwrap_or(Path::new("/")),
        )
        .is_ok()
        {
            self.saved = now;
            self.dirty = false;
        }
    }
    /// `{usd, messages, unpriced}`: the week's spend so far, each message once across every file.
    pub fn total(&mut self) -> Value {
        if self.sums.is_none() {
            let mut merged: BTreeMap<&String, (Value, [i64; 5])> = BTreeMap::new();
            for entry in self.files.values() {
                for (key, (model, counts)) in &entry.msgs {
                    merged
                        .entry(key)
                        .and_modify(|old| {
                            *old = (
                                model.clone(),
                                std::array::from_fn(|i| old.1[i].max(counts[i])),
                            )
                        })
                        .or_insert((model.clone(), *counts));
                }
            }
            let (mut usd, mut unpriced) = (0., BTreeSet::new());
            for (model, counts) in merged.values() {
                let name = match model {
                    Value::Null => "None",
                    other => other.as_str().unwrap_or(""),
                };
                match cost_of(name, *counts) {
                    Some(cost) => usd += cost,
                    None => {
                        unpriced.insert(name.to_owned());
                    }
                }
            }
            self.sums = Some(json!({
                "usd": usd,
                "messages": merged.len(),
                "unpriced": unpriced,
            }));
        }
        self.sums.clone().unwrap_or(Value::Null)
    }
}
