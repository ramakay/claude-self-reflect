//! Codex runs beside the session, read from Codex's own rollout files and nothing else.
//!
//!   <HOME>/.codex/sessions/**/rollout-*.jsonl
//!
//! Three kinds of record are read, for their metadata only, and no conversation text is kept:
//!   session_meta  id, start time, cwd (and a model or effort, where an older Codex wrote one there)
//!   turn_context  the latest model, effort and cwd
//!   event_msg / token_count  the run's cumulative total_tokens, and the plan's rate limits
//!
//! No Codex program is started and no process is inspected: there is no database, no network and
//! no model call. A run is live while its rollout was written less than `LIVE_SECONDS` ago. The
//! week line is the newest valid reading of the "codex" limit's 10080-minute window, which Codex
//! writes as `primary` or `secondary` (it is not assumed to be either); percentages of different
//! runs are never added.
//!
//! Files are read incrementally: each remembers where its last read stopped, only whole lines are
//! taken (a line still being written waits for its newline), a file that shrank or changed is read
//! again from its start, and a line that is not JSON is skipped. One scan reads at most about
//! `READ_BUDGET` bytes, newest file first, and the rest waits for the next scan.
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::LazyLock,
};

const LIVE_SECONDS: f64 = 60.;
const WEEK_MINUTES: f64 = 10080.;
/// Percent of the week at which the line turns amber, then red.
const AMBER: f64 = 75.;
const RED: f64 = 90.;
const LIMIT_ID: &str = "codex";
/// A rollout older than this tells nothing of the week and of no live run.
const KEEP_DAYS: f64 = 8.;
/// Seconds between two scans of the folder; a frame in between reads what the last found.
const EVERY: f64 = 2.;
/// A line longer than this (a tool's whole output) is skipped unread.
const MAX_LINE: usize = 1 << 20;
/// Characters of a line looked at to tell whether it is a record worth parsing.
const HEAD: usize = 400;
/// Bytes one scan reads at most, newest file first; a line that crosses it is finished.
const READ_BUDGET: u64 = 32 << 20;

static KINDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""type"\s*:\s*"(session_meta|turn_context|event_msg)""#).unwrap()
});
static COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""type"\s*:\s*"token_count""#).unwrap());

/// A time given as epoch seconds (or milliseconds), or an ISO date and time, as seconds.
fn epoch(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().map(|v| if v > 1e11 { v / 1000. } else { v }),
        Value::String(s) if !s.trim().is_empty() => super::paths::epoch(s),
        _ => None,
    }
}
/// A short string from a record, no control in it; None when it is not a non-empty string.
fn text_of(value: &Value, most: usize) -> Option<String> {
    let text = value.as_str().filter(|s| !s.trim().is_empty())?;
    let clean: String = text
        .chars()
        .filter(|c| !matches!(*c as u32, 0..=31 | 127..=159))
        .collect();
    let out: String = clean.trim().chars().take(most).collect();
    (!out.is_empty()).then_some(out)
}
fn count_of(value: &Value) -> Option<u64> {
    let n = value.as_f64().filter(|_| value.is_number())?;
    (0. ..1e15).contains(&n).then_some(n as u64)
}
fn percent_of(value: &Value) -> Option<f64> {
    let n = value.as_f64().filter(|_| value.is_number())?;
    (0. ..=1000.).contains(&n).then_some(n)
}
/// (time, percent, resets_at) from a token_count's rate_limits when it is the codex limit with a
/// 10080-minute window, in `primary` or `secondary`; else None.
fn week_reading(limits: &Value, at: Option<f64>) -> Option<(f64, f64, Option<f64>)> {
    if limits["limit_id"] != LIMIT_ID {
        return None;
    }
    for name in ["primary", "secondary"] {
        let window = &limits[name];
        if !window.is_object() || window["window_minutes"].as_f64() != Some(WEEK_MINUTES) {
            continue;
        }
        let Some(percent) = percent_of(&window["used_percent"]) else {
            continue;
        };
        let mut resets = epoch(&window["resets_at"]);
        if resets.is_none() {
            if let (Some(at), Some(secs)) = (at, window["resets_in_seconds"].as_f64()) {
                resets = Some(at + secs);
            }
        }
        return Some((at.unwrap_or(0.), percent, resets));
    }
    None
}

/// What is known of one rollout file. No text of the conversation is held.
#[derive(Default)]
struct Run {
    offset: u64,
    mtime: f64,
    inode: Option<u64>,
    identity: Option<String>,
    started: Option<f64>,
    model: Option<String>,
    effort: Option<String>,
    cwd: Option<String>,
    meta_model: Option<String>,
    meta_effort: Option<String>,
    meta_cwd: Option<String>,
    tokens: Option<u64>,
    /// (record time, percent, resets_at or None)
    week: Option<(f64, f64, Option<f64>)>,
}
impl Run {
    fn take(&mut self, record: &Value) {
        let kind = record["type"].as_str().unwrap_or("");
        let payload = &record["payload"];
        if !payload.is_object() {
            return;
        }
        let at = epoch(&record["timestamp"]);
        match kind {
            "session_meta" => {
                self.identity = text_of(&payload["id"], 100)
                    .or_else(|| text_of(&payload["session_id"], 100))
                    .or(self.identity.take());
                self.started = epoch(&payload["timestamp"])
                    .filter(|t| *t != 0.)
                    .or(at.filter(|t| *t != 0.))
                    .or(self.started);
                self.meta_cwd = text_of(&payload["cwd"], 400).or(self.meta_cwd.take());
                self.meta_model = text_of(&payload["model"], 80).or(self.meta_model.take());
                self.meta_effort = text_of(&payload["effort"], 20)
                    .or_else(|| text_of(&payload["reasoning_effort"], 20))
                    .or(self.meta_effort.take());
            }
            "turn_context" => {
                self.model = text_of(&payload["model"], 80).or(self.model.take());
                self.effort = text_of(&payload["effort"], 20)
                    .or_else(|| text_of(&payload["reasoning_effort"], 20))
                    .or(self.effort.take());
                self.cwd = text_of(&payload["cwd"], 400).or(self.cwd.take());
            }
            "event_msg" if payload["type"] == "token_count" => {
                let total = &payload["info"]["total_token_usage"];
                if let Some(tokens) = count_of(&total["total_tokens"]) {
                    // cumulative: the latest is the run's, never a sum
                    self.tokens = Some(tokens);
                }
                if let Some(reading) = week_reading(&payload["rate_limits"], at) {
                    if self.week.is_none_or(|w| reading.0 >= w.0) {
                        self.week = Some(reading);
                    }
                }
            }
            _ => (),
        }
    }
    fn started(&self) -> f64 {
        self.started.filter(|s| *s != 0.).unwrap_or(self.mtime)
    }
}

/// The rollout files under <home>/.codex/sessions, read a little at a time and kept as metadata.
pub struct CodexRolloutIndex {
    root: PathBuf,
    sessions: PathBuf,
    runs: BTreeMap<PathBuf, Run>,
    checked: Option<f64>,
    budget: u64,
}
impl CodexRolloutIndex {
    pub fn new(home: &Path) -> Self {
        let root = home.join(".codex");
        Self {
            sessions: root.join("sessions"),
            root,
            runs: BTreeMap::new(),
            checked: None,
            budget: READ_BUDGET,
        }
    }
    #[cfg(test)]
    fn with_budget(home: &Path, budget: u64) -> Self {
        Self {
            budget,
            ..Self::new(home)
        }
    }
    pub fn present(&self) -> bool {
        self.root.is_dir()
    }
    /// The rollout files worth reading: written within `KEEP_DAYS`, found without following
    /// links. Date folders (YYYY/MM/DD) older than that are not entered.
    fn paths(&self, now: f64) -> Vec<(PathBuf, fs::Metadata)> {
        let cutoff = now - KEEP_DAYS * 86400.;
        let floor = Local
            .timestamp_opt(cutoff as i64, 0)
            .single()
            .map(|t| t.date_naive() - chrono::Days::new(1))
            .unwrap_or(NaiveDate::MIN);
        let mut found = vec![];
        let mut stack = vec![(self.sessions.clone(), Vec::<String>::new())];
        while let Some((folder, chain)) = stack.pop() {
            let Ok(entries) = fs::read_dir(&folder) else {
                continue;
            };
            let mut dirs = vec![];
            let mut files = vec![];
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(info) = fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if info.is_dir() {
                    if !old(&chain, &name, floor) {
                        dirs.push(name);
                    }
                } else if info.is_file()
                    && name.starts_with("rollout-")
                    && name.ends_with(".jsonl")
                    && info.mtime() as f64 + info.mtime_nsec() as f64 / 1e9 >= cutoff
                {
                    files.push((entry.path(), info));
                }
            }
            files.sort_by(|a, b| a.0.cmp(&b.0));
            found.extend(files);
            dirs.sort();
            // the stack pops the last pushed, so push in reverse to walk the sorted order
            for name in dirs.into_iter().rev() {
                let mut chain = chain.clone();
                chain.push(name.clone());
                stack.push((folder.join(name), chain));
            }
        }
        found
    }
    /// New whole lines of one rollout into its Run, as far as the scan's `budget` allows; with
    /// the budget spent the file is recorded and left unread.
    fn read(&mut self, path: &Path, info: &fs::Metadata, budget: &mut u64) {
        let mtime = info.mtime() as f64 + info.mtime_nsec() as f64 / 1e9;
        let run = self.runs.remove(path);
        let mut run = match run {
            Some(run) if run.inode == Some(info.ino()) && info.len() >= run.offset => run,
            // a file new, replaced or truncated is read from its start
            _ => Run::default(),
        };
        run.inode = Some(info.ino());
        run.mtime = mtime;
        if info.len() != run.offset && *budget > 0 {
            read_lines(path, &mut run, budget);
        }
        self.runs.insert(path.to_path_buf(), run);
    }
    /// Scan, at most every `EVERY` seconds; the runs seen, and the week.
    pub fn refresh(&mut self, now: f64) -> Value {
        if self
            .checked
            .is_some_and(|at| (0. ..EVERY).contains(&(now - at)))
        {
            return self.snapshot(now);
        }
        self.checked = Some(now);
        let mut seen = std::collections::BTreeSet::new();
        let mut found = self.paths(now);
        // newest first, so a spent budget leaves the oldest files for the next scan
        found.sort_by(|a, b| {
            let stamp = |i: &fs::Metadata| i.mtime() as f64 + i.mtime_nsec() as f64 / 1e9;
            stamp(&b.1)
                .total_cmp(&stamp(&a.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut budget = self.budget;
        for (path, info) in found {
            self.read(&path, &info, &mut budget);
            seen.insert(path);
        }
        // a file gone is forgotten
        self.runs.retain(|path, _| seen.contains(path));
        self.snapshot(now)
    }
    /// {rows: the live runs, live: K, week: {percent, resets_at, level} or null}
    fn snapshot(&self, now: f64) -> Value {
        let mut newest: BTreeMap<String, &Run> = BTreeMap::new();
        for (path, run) in &self.runs {
            let key = run
                .identity
                .clone()
                .unwrap_or_else(|| format!("\0{}", path.display()));
            // one session found in two files is one run
            if newest.get(&key).is_none_or(|have| run.mtime > have.mtime) {
                newest.insert(key, run);
            }
        }
        let mut live: Vec<&Run> = newest
            .into_values()
            .filter(|run| (0. ..LIVE_SECONDS).contains(&(now - run.mtime)))
            .collect();
        live.sort_by(|a, b| b.started().total_cmp(&a.started()));
        let rows: Vec<Value> = live
            .iter()
            .map(|run| {
                let started = run.started();
                json!({
                    "identity": run.identity,
                    "started": started,
                    "model": run.model.as_ref().or(run.meta_model.as_ref()),
                    "effort": run.effort.as_ref().or(run.meta_effort.as_ref()),
                    "cwd": run.cwd.as_ref().or(run.meta_cwd.as_ref()),
                    "tokens": run.tokens,
                    "minutes": if started != 0. {
                        json!(((now - started) / 60.).floor().max(0.) as i64)
                    } else {
                        Value::Null
                    },
                })
            })
            .collect();
        let mut week: Option<(f64, f64, Option<f64>)> = None;
        for reading in self.runs.values().filter_map(|run| run.week) {
            if week.is_none_or(|w| reading.0 > w.0) {
                week = Some(reading);
            }
        }
        let reading = week
            // past its reset the figure is the last week's: unavailable
            .filter(|w| w.2.is_none_or(|resets| resets > now))
            .map(|(_, percent, resets)| {
                json!({
                    "percent": percent,
                    "resets_at": resets,
                    "level": if percent >= RED {
                        json!("red")
                    } else if percent >= AMBER {
                        json!("amber")
                    } else {
                        Value::Null
                    },
                })
            });
        json!({"rows": rows, "live": live.len(), "week": reading})
    }
}
/// A date folder (year, then month, then day, by depth) wholly before `floor`; any other folder
/// is not.
fn old(chain: &[String], name: &str, floor: NaiveDate) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if chain.len() > 2 || !digits(name) || !chain.iter().all(|p| digits(p)) {
        return false;
    }
    let key: Vec<i64> = chain
        .iter()
        .map(String::as_str)
        .chain([name])
        .map(|p| p.parse().unwrap_or(i64::MAX))
        .collect();
    let bound = [
        floor.year() as i64,
        floor.month() as i64,
        floor.day() as i64,
    ];
    key[..] < bound[..key.len()]
}
/// Reads whole lines from the run's offset, taking their size from `budget`; stops at the first
/// line boundary at or past it, so the line that crosses it is finished.
fn read_lines(path: &Path, run: &mut Run, budget: &mut u64) {
    let Ok(file) = File::open(path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(run.offset)).is_err() {
        return;
    }
    let mut line = Vec::new();
    while *budget > 0 {
        line.clear();
        let got = (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut line);
        let Ok(mut taken) = got else { return };
        if taken == 0 {
            return;
        }
        if line.len() > MAX_LINE && !line.ends_with(b"\n") {
            // a line too long to read: past its end, unparsed
            let mut rest = Vec::new();
            loop {
                rest.clear();
                let more = (&mut reader)
                    .take(MAX_LINE as u64)
                    .read_until(b'\n', &mut rest)
                    .unwrap_or(0);
                if more == 0 {
                    // its newline is not here yet: wait for it
                    return;
                }
                taken += more;
                if rest.ends_with(b"\n") {
                    break;
                }
            }
            run.offset += taken as u64;
            *budget = budget.saturating_sub(taken as u64);
            continue;
        }
        if !line.ends_with(b"\n") {
            // a line still being written: its newline is not here yet
            return;
        }
        run.offset += taken as u64;
        *budget = budget.saturating_sub(taken as u64);
        let head = String::from_utf8_lossy(&line[..line.len().min(HEAD)]).into_owned();
        let Some(found) = KINDS.captures(&head) else {
            continue;
        };
        if &found[1] == "event_msg" && !COUNT.is_match(&head) {
            continue;
        }
        if let Ok(record) = serde_json::from_slice::<Value>(&line) {
            if record.is_object() {
                run.take(&record);
            }
        }
    }
}

/// 'Codex week 43% · resets Sun 09:00 · 2 live'; 'Codex week unavailable' while there is no
/// current reading.
pub fn week_line(week: &Value, live: u64) -> String {
    if week.is_null() {
        return format!("Codex week unavailable · {live} live");
    }
    let resets = week["resets_at"]
        .as_f64()
        .filter(|r| *r != 0.)
        .and_then(|r| Local.timestamp_opt(r as i64, 0).single())
        .map(|t| format!(" · resets {}", t.format("%a %H:%M")))
        .unwrap_or_default();
    format!(
        "Codex week {}%{resets} · {live} live",
        super::pyfmt::fixed(week["percent"].as_f64().unwrap_or(0.), 0)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: f64 = 1_792_320_000.;
    fn write(home: &Path, name: &str, lines: &[Value], mtime: f64) -> PathBuf {
        let folder = home.join(".codex/sessions/2026/10/18");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join(name);
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(&path, text).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(mtime))
            .unwrap();
        path
    }
    fn meta(id: &str, at: f64) -> Value {
        json!({"type":"session_meta","timestamp":at,"payload":{"id":id,"timestamp":at,"cwd":"/w/a/b"}})
    }
    fn tokens(at: f64, total: u64, used: f64, resets: f64) -> Value {
        json!({"type":"event_msg","timestamp":at,"payload":{"type":"token_count",
            "info":{"total_token_usage":{"total_tokens":total}},
            "rate_limits":{"limit_id":"codex",
                "primary":{"window_minutes":300,"used_percent":3},
                "secondary":{"window_minutes":10080,"used_percent":used,"resets_at":resets}}}})
    }
    #[test]
    fn test_no_codex_folder_is_not_present() {
        let d = tempfile::tempdir().unwrap();
        assert!(!CodexRolloutIndex::new(d.path()).present());
    }
    #[test]
    fn test_a_live_run_has_minutes_tokens_and_the_week_is_the_newest_reading() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "rollout-a.jsonl",
            &[
                meta("run-a", NOW - 600.),
                json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"medium","cwd":"/w/x/y"}}),
                tokens(NOW - 20., 51_000, 41.4, NOW + 86400.),
            ],
            NOW - 10.,
        );
        write(
            d.path(),
            "rollout-old.jsonl",
            &[meta("run-old", NOW - 90_000.)],
            NOW - 7000.,
        );
        let mut index = CodexRolloutIndex::new(d.path());
        let got = index.refresh(NOW);
        assert_eq!(got["live"], 1);
        let row = &got["rows"][0];
        assert_eq!(row["identity"], "run-a");
        assert_eq!(row["model"], "gpt-6-luna");
        assert_eq!(row["effort"], "medium");
        assert_eq!(row["cwd"], "/w/x/y");
        assert_eq!(row["tokens"], 51_000);
        assert_eq!(row["minutes"], 10);
        assert_eq!(got["week"]["percent"], 41.4);
        assert!(got["week"]["level"].is_null());
        assert_eq!(
            week_line(&got["week"], 1).split(" · ").next().unwrap(),
            "Codex week 41%"
        );
    }
    #[test]
    fn test_a_week_past_its_reset_is_unavailable_and_levels_follow_the_percent() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "rollout-a.jsonl",
            &[meta("run-a", NOW - 60.), tokens(NOW - 5., 1, 92., NOW + 5.)],
            NOW - 5.,
        );
        let mut index = CodexRolloutIndex::new(d.path());
        assert_eq!(index.refresh(NOW)["week"]["level"], "red");
        index.checked = None;
        let late = index.refresh(NOW + 10.);
        assert!(late["week"].is_null());
        assert_eq!(
            week_line(&late["week"], 0),
            "Codex week unavailable · 0 live"
        );
    }
    #[test]
    fn test_a_partial_last_line_waits_for_its_newline_and_a_scan_waits_two_seconds() {
        let d = tempfile::tempdir().unwrap();
        let path = write(
            d.path(),
            "rollout-a.jsonl",
            &[meta("run-a", NOW - 60.)],
            NOW - 5.,
        );
        let mut text = fs::read_to_string(&path).unwrap();
        let line = tokens(NOW - 5., 7, 10., NOW + 100.).to_string();
        text.push_str(&line[..line.len() / 2]);
        fs::write(&path, &text).unwrap();
        let mut index = CodexRolloutIndex::new(d.path());
        assert!(index.refresh(NOW)["rows"][0]["tokens"].is_null());
        text.truncate(text.len() - line.len() / 2);
        text.push_str(&format!("{line}\n"));
        fs::write(&path, &text).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(NOW - 5.))
            .unwrap();
        // inside the two seconds the last scan's runs are still what a frame sees
        assert!(index.refresh(NOW + 1.)["rows"][0]["tokens"].is_null());
        assert_eq!(index.refresh(NOW + 3.)["rows"][0]["tokens"], 7);
    }
    #[test]
    fn test_date_folders_before_the_cutoff_are_not_entered() {
        let floor = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
        let chain = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(old(&chain(&[]), "2025", floor));
        assert!(!old(&chain(&[]), "2026", floor));
        assert!(old(&chain(&["2026"]), "09", floor));
        assert!(!old(&chain(&["2026"]), "10", floor));
        assert!(old(&chain(&["2026", "10"]), "09", floor));
        assert!(!old(&chain(&["2026", "10"]), "10", floor));
        assert!(!old(&chain(&["2026", "10"]), "notes", floor));
    }
    /// Three rollouts, newest first by mtime: run-new, run-mid, run-old (not live).
    fn three(home: &Path) -> [PathBuf; 3] {
        let lines = |id: &str, base: f64, used: f64| {
            vec![
                meta(id, base - 600.),
                tokens(base - 30., 100, used - 2., NOW + 86400.),
                tokens(base - 20., 200, used - 1., NOW + 86400.),
                tokens(base - 10., 300, used, NOW + 86400.),
            ]
        };
        [
            write(
                home,
                "rollout-new.jsonl",
                &lines("run-new", NOW, 41.),
                NOW - 5.,
            ),
            write(
                home,
                "rollout-mid.jsonl",
                &lines("run-mid", NOW - 3., 30.),
                NOW - 8.,
            ),
            write(
                home,
                "rollout-old.jsonl",
                &lines("run-old", NOW - 5000., 10.),
                NOW - 5000.,
            ),
        ]
    }
    #[test]
    fn test_a_small_budget_reads_the_newest_file_first_and_later_scans_finish_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let [new, mid, old] = three(d.path());
        let size = |p: &Path| fs::metadata(p).unwrap().len();
        let mut index = CodexRolloutIndex::with_budget(d.path(), size(&new));
        let first = index.refresh(NOW);
        // the newest file is whole, the others are recorded and not read
        assert_eq!(index.runs[&new].offset, size(&new));
        assert_eq!(index.runs[&mid].offset, 0);
        assert_eq!(index.runs[&old].offset, 0);
        assert_eq!(index.runs.len(), 3);
        assert_eq!(first["week"]["percent"], 41.);
        let rows = first["rows"].as_array().unwrap();
        let known: Vec<&Value> = rows.iter().filter(|r| r["identity"] == "run-new").collect();
        assert_eq!(known.len(), 1);
        assert_eq!(known[0]["tokens"], 300);
        assert!(rows.iter().all(|r| r["identity"] != "run-mid"));
        // each later scan takes a little more, from the newest unfinished file
        let mut now = NOW;
        let mut last = first;
        for _ in 0..12 {
            now += 2.5;
            last = index.refresh(now);
        }
        assert_eq!(index.runs[&mid].offset, size(&mid));
        assert_eq!(index.runs[&old].offset, size(&old));
        let mut whole = CodexRolloutIndex::new(d.path());
        assert_eq!(last, whole.refresh(now));
    }
    #[test]
    fn test_a_file_left_unread_by_the_budget_is_kept_and_read_on_a_later_scan() {
        let d = tempfile::tempdir().unwrap();
        let [new, mid, _old] = three(d.path());
        let mut index = CodexRolloutIndex::with_budget(d.path(), 1);
        index.refresh(NOW);
        // one line of the newest file, nothing of the others
        assert!(index.runs[&new].offset > 0);
        assert!(index.runs[&new].offset < fs::metadata(&new).unwrap().len());
        assert_eq!(index.runs[&mid].offset, 0);
        assert_eq!(
            index.runs[&mid].inode,
            Some(fs::metadata(&mid).unwrap().ino())
        );
        let mut now = NOW;
        for _ in 0..12 {
            now += 2.5;
            index.refresh(now);
        }
        assert_eq!(index.runs[&mid].offset, fs::metadata(&mid).unwrap().len());
        assert_eq!(index.runs[&mid].identity.as_deref(), Some("run-mid"));
    }
    #[test]
    fn test_a_file_with_nothing_new_costs_no_budget() {
        let d = tempfile::tempdir().unwrap();
        let [new, mid, _old] = three(d.path());
        let size = |p: &Path| fs::metadata(p).unwrap().len();
        let mut index = CodexRolloutIndex::with_budget(d.path(), size(&new));
        index.refresh(NOW);
        index.refresh(NOW + 3.);
        // the newest file is whole and free, so the budget goes to the next file
        assert_eq!(index.runs[&new].offset, size(&new));
        assert!(index.runs[&mid].offset > 0);
    }
}
