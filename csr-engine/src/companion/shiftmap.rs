//! The Shift Map on the pane's main page: the rows it draws (pane.py `shift_map_marked`), the
//! items the Story section holds for it, when it opens and folds (`MapMotion`), and the refresh
//! the pane starts for it. The same row reader serves the status line's JSON (`hook_json`).
use super::{
    array, db::Db, num, paths::Paths, render::text::fit, session::Session, story::StoryMap, string,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::{Child, Command, Stdio},
};

/// A shift's rows are laid in this many columns when the pane's width is not known.
pub const SHIFT_ROOM: usize = 40;
pub const SHIFT_HINT: &str = "click again to fold";
/// How long a refresh that found nothing new says so.
pub const REFRESH_NOTE_LIFE: f64 = 15.;
const MAP_HOLD_S: f64 = 12.;
const MAP_FRESH_S: f64 = 30.;
const BEFORE_LABEL: &str = "Before: ";
const AFTER_LABEL: &str = "After:  ";
const NEW_LABEL: &str = "New:    ";

/// The map as the page keeps it (`data["story_map"]`): the fields pane.py's dict carries.
pub fn map_value(map: &StoryMap) -> Value {
    json!({
        "rev": map.rev,
        "at": map.at,
        "by": map.by,
        "covered": map.covered,
        "previous_sigs": map.previous_sigs,
        "has_previous": map.has_previous,
        "entries": map.entries,
    })
}

/// Whether the Shift Map is open at full size: its own row, or one of its entries.
pub fn map_open(key: &Value) -> bool {
    key.as_str()
        .is_some_and(|k| k == "shifts" || k.starts_with("shiftlike:"))
}

fn left_tail(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.into();
    }
    format!(
        "…{}",
        text.chars()
            .skip(text.chars().count().saturating_sub(room.saturating_sub(1)))
            .collect::<String>()
    )
}
fn shift_part(part: &Value, level: usize) -> String {
    if let Some(op) = part["op"].as_str() {
        return op.into();
    }
    let word = if level < 2 {
        match string(&part["mark"]) {
            "+" => "added",
            "~" => "changed",
            "-" => "removed",
            "!" => "at risk",
            _ => "",
        }
    } else {
        ""
    };
    let scope = string(&part["scope"]);
    let sym = string(&part["sym"]);
    let text = if !sym.is_empty() && level > 0 {
        sym.into()
    } else if !sym.is_empty() {
        format!("{scope}:{sym}")
    } else {
        scope.into()
    };
    if word.is_empty() {
        text
    } else {
        format!("{word} {text}")
    }
}
fn shift_side(parts: &[Value], room: usize) -> String {
    for level in 0..3 {
        let text = parts
            .iter()
            .map(|p| shift_part(p, level))
            .collect::<Vec<_>>()
            .join(" ");
        if text.chars().count() <= room {
            return text;
        }
    }
    let ends = parts.iter().filter(|p| !p["op"].is_string()).count().max(1);
    let ops: usize = parts
        .iter()
        .filter_map(|p| p["op"].as_str())
        .map(|s| s.chars().count() + 2)
        .sum();
    let each = (room.saturating_sub(ops) / ends).max(6);
    fit(
        &parts
            .iter()
            .map(|p| {
                p["op"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| left_tail(&shift_part(p, 2), each))
            })
            .collect::<Vec<_>>()
            .join(" "),
        room,
    )
}
/// What was checked about a shift and whose words it is: one row, or two when they do not fit
/// side by side.
pub fn shift_source(entry: &Value, by: &str, room: usize) -> Vec<String> {
    if entry["grounded"] != true {
        return vec!["Haiku's words · names not checked".into()];
    }
    let receipts = array(&entry["receipts"]);
    let commit = receipts.iter().filter_map(Value::as_str).find_map(|r| {
        r.strip_prefix("oid:")
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(7).collect::<String>())
    });
    let edits = receipts.iter().any(|r| string(r).starts_with("evo:"));
    let graph = if receipts.iter().any(|r| string(r).starts_with("edge:")) {
        "call graph"
    } else if receipts.iter().any(|r| string(r).starts_with("node:")) {
        "code index"
    } else {
        ""
    };
    let mut found = vec![];
    if let Some(commit) = commit {
        found.push(format!("commit {commit}"));
    }
    if edits {
        found.push("your edits".into());
    }
    if found.is_empty() && !graph.is_empty() {
        found.push(graph.into());
    }
    let found = found.join(" and ");
    if by == "receipts" {
        return vec![if found.is_empty() {
            "checked".into()
        } else {
            format!("from {found}")
        }];
    }
    let words = "Haiku's words · names match";
    if found.is_empty() {
        vec![words.into()]
    } else if words.chars().count() + 1 + found.chars().count() <= room {
        vec![format!("{words} {found}")]
    } else {
        vec![words.into(), found]
    }
}
fn shift_sentence(label: &str, text: &str, room: usize) -> Vec<String> {
    let width = room.saturating_sub(label.len()).max(label.len());
    let mut lines = vec![];
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word = left_tail(word, width);
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(line);
            line = word;
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > 2 {
        let mut words: Vec<_> = lines[1].split(' ').collect();
        while !words.is_empty() && words.join(" ").chars().count() + 1 > width {
            words.pop();
        }
        lines = vec![lines[0].clone(), words.join(" ") + "…"];
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            format!(
                "{}{line}",
                if i == 0 {
                    label.to_owned()
                } else {
                    " ".repeat(label.len())
                }
            )
        })
        .collect()
}
/// One shift as (row, loud) pairs. A 5-field entry: `kind · file:symbol`, the Before and After
/// sentences (New when there was no before), then where the evidence comes from. An entry
/// without was/now (an older map) keeps the claim, the sides and the evidence of its day.
pub fn shift_group(entry: &Value, map: &Value, room: usize) -> Vec<(String, bool)> {
    let by = match string(&map["by"]) {
        "" => "Haiku",
        by => by,
    };
    let new = if entry["moved"] == true && map["has_previous"] == true {
        "new "
    } else {
        ""
    };
    let kind = string(&entry["kind"]);
    if string(&entry["now"]).is_empty() {
        let inner = room.saturating_sub(2);
        let mut rows = vec![(
            fit(&format!("{new}{kind}: {}", string(&entry["claim"])), room),
            false,
        )];
        let before = shift_side(array(&entry["before"]), inner.saturating_sub(4));
        let after = shift_side(
            array(&entry["after"]),
            if before.is_empty() {
                inner
            } else {
                inner.saturating_sub(4)
            },
        );
        if !before.is_empty() && !after.is_empty() {
            let both = format!("{before} > {after}");
            if both.chars().count() <= inner {
                rows.push((format!("  {both}"), false));
            } else {
                rows.push((format!("  was {before}"), false));
                rows.push((format!("  now {after}"), false));
            }
        } else if !before.is_empty() {
            rows.push((format!("  was {before}"), false));
        } else if !after.is_empty() {
            rows.push((format!("  {after}"), false));
        }
        rows.extend(
            shift_source(entry, by, inner)
                .iter()
                .map(|s| (format!("  {}", fit(s, inner)), false)),
        );
        return rows;
    }
    let mut head = format!("{new}{kind}");
    let endpoint = array(&entry["after"])
        .iter()
        .chain(array(&entry["before"]))
        .find(|p| !p["op"].is_string());
    if let Some(p) = endpoint {
        let scope = string(&p["scope"]);
        let sym = string(&p["sym"]);
        let named = if sym.is_empty() {
            scope.into()
        } else {
            format!("{scope}:{sym}")
        };
        if !named.is_empty() {
            let width = room.saturating_sub(head.chars().count() + 3).max(6);
            head.push_str(&format!(" · {}", left_tail(&named, width)));
        }
    }
    let mut rows = vec![(fit(&head, room), false)];
    let now = string(&entry["now"]);
    let sentences = if string(&entry["was"]).is_empty() {
        shift_sentence(NEW_LABEL, now, room)
    } else {
        let mut both = shift_sentence(BEFORE_LABEL, string(&entry["was"]), room);
        both.extend(shift_sentence(AFTER_LABEL, now, room));
        both
    };
    rows.extend(sentences.into_iter().map(|row| (row, true)));
    rows.extend(
        shift_source(entry, by, room)
            .iter()
            .map(|s| (fit(s, room), false)),
    );
    rows
}
/// The Shift Map as plain words, (title, rows with their loudness). `shut` shows the first shift;
/// `budget` caps the rows the shifts take (at least one whole shift is shown). Shifts left out
/// are said in a last row. The title carries how far behind the map is, in transcript messages.
pub fn shift_map_marked(
    map: &Value,
    messages_now: Option<u64>,
    room: usize,
    shut: bool,
    budget: Option<usize>,
    source: Option<&str>,
) -> (String, Vec<(String, bool)>) {
    let stale = match (messages_now, map["covered"].as_u64()) {
        (Some(now), Some(covered)) => {
            let behind = now.saturating_sub(covered);
            if behind == 0 {
                "current".to_owned()
            } else {
                format!(
                    "as of {behind} message{} ago",
                    if behind == 1 { "" } else { "s" }
                )
            }
        }
        _ => String::new(),
    };
    let mut title = "Shift Map".to_owned();
    if !stale.is_empty() {
        title.push_str(&format!(" · {stale}"));
    }
    if let Some(source) = source.filter(|s| !s.is_empty()) {
        title.push_str(&format!(
            " · from {}",
            source.chars().take(8).collect::<String>()
        ));
    }
    let groups: Vec<_> = array(&map["entries"])
        .iter()
        .take(4)
        .map(|entry| shift_group(entry, map, room))
        .collect();
    let (mut keep, mut used) = (0, 0);
    for group in groups.iter().take(if shut { 1 } else { groups.len() }) {
        if keep > 0 && budget.is_some_and(|b| b > 0 && used + group.len() > b) {
            break;
        }
        keep += 1;
        used += group.len();
    }
    let mut rows: Vec<_> = groups.iter().take(keep).flatten().cloned().collect();
    let left = groups.len() - keep;
    if left > 0 {
        let more = format!(
            "+{left} more shift{}{}",
            if left == 1 { "" } else { "s" },
            if shut { " · click or m to expand" } else { "" }
        );
        rows.push((fit(&more, room), false));
    }
    (title, rows)
}
pub fn shift_map_rows(
    map: &Value,
    messages_now: Option<u64>,
    room: usize,
    source: Option<&str>,
) -> (String, Vec<String>) {
    let (title, rows) = shift_map_marked(map, messages_now, room, false, None, source);
    (title, rows.into_iter().map(|(text, _)| text).collect())
}

/// What a story call has cost this person, from their own narrative_usage rows (a failed call
/// was paid for and counts): calls, usd per call (null when a model has no price), tokens per
/// call; None when there is no row or the table cannot be read.
pub fn story_cost(db: &Db) -> Option<Value> {
    let rows = db
        .try_query(
            "select model, count(*), sum(input_tokens), sum(output_tokens), \
            sum(cache_read_tokens), sum(cache_creation_tokens) from narrative_usage \
            where call_site = 'story' group by model",
            &[],
        )
        .ok()?;
    let (mut calls, mut tokens, mut usd) = (0_i64, 0_i64, Some(0.0_f64));
    for row in rows {
        let counts: Vec<i64> = row[2..].iter().map(|n| num(n) as i64).collect();
        calls += row[1].as_i64().unwrap_or(0);
        tokens += counts.iter().sum::<i64>();
        let cost = if counts.iter().any(|n| *n != 0) {
            crate::journal::composer::list_cost_summed(
                string(&row[0]),
                [counts[0], counts[1], counts[2], counts[3], 0],
            )
        } else {
            Some(0.)
        };
        usd = usd.zip(cost).map(|(a, b)| a + b);
    }
    (calls > 0).then(|| {
        json!({
            "calls": calls,
            "usd": usd.map(|u| u / calls as f64),
            "tokens": tokens / calls,
        })
    })
}
fn thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}
/// The refresh control's words: what it costs from this person's own story calls, else the plain
/// fact.
pub fn refresh_label(cost: &Value) -> String {
    let calls = cost["calls"].as_i64().unwrap_or(0);
    if !cost.is_object() || calls == 0 {
        return "↻ refresh the shift map · one Haiku call".into();
    }
    let n = format!("{calls} story call{}", if calls == 1 { "" } else { "s" });
    if let Some(usd) = cost["usd"].as_f64() {
        let cents = usd * 100.;
        let said = if cents < 0.1 {
            "<0.1".to_owned()
        } else {
            format!("{cents:.1}")
        };
        return format!("↻ refresh the shift map · about {said}¢ (avg of your {n})");
    }
    format!(
        "↻ refresh the shift map · about {} tokens (avg of your {n})",
        thousands(cost["tokens"].as_i64().unwrap_or(0))
    )
}
/// Whether there is news for a refresh: you typed something after the map's current telling.
pub fn refresh_news(map: &Value, s: &Value) -> bool {
    map["at"].as_f64().is_some_and(|at| {
        array(&s["prompts"])
            .iter()
            .any(|t| num(t) > at && t.is_number())
    })
}
/// Which telling a Shift Map is: its (rev, at).
pub fn map_stamp(map: &Value) -> Option<(u64, Option<u64>)> {
    map.is_object().then(|| {
        (
            map["rev"].as_u64().unwrap_or(0),
            map["at"].as_f64().map(f64::to_bits),
        )
    })
}

/// The Story section's items for the map: its row (folded to the first shift or open at full
/// size), the entries that take a like once it is open, and the refresh row, note or progress.
pub fn map_items(data: &Value, s: &Value, ui: &Value) -> Vec<Value> {
    let map = &data["story_map"];
    let mut told = vec![];
    if !map.is_object() {
        return told;
    }
    // draw_main hands page_main its own width as ui.cols; without one the rows take SHIFT_ROOM.
    let gutter = if ui["phone"] == true { 2 } else { 6 };
    let room = ui["cols"]
        .as_u64()
        .map(|cols| {
            (cols as usize)
                .saturating_sub(1)
                .max(24)
                .saturating_sub(gutter)
        })
        .unwrap_or(SHIFT_ROOM);
    let open = map_open(&ui["open"]);
    let shut = data["map_view"]["open"] == false && !open;
    let budget = ui["rows"].as_u64().map(|rows| (rows as usize / 2).max(8));
    let from = data["story_map_from"].as_str();
    let messages = if from.is_some() {
        data["story_map_messages"].as_u64()
    } else {
        s["messages"].as_u64()
    };
    let (title, marked) = shift_map_marked(map, messages, room, shut, budget, from);
    if marked.is_empty() {
        return told;
    }
    told.push(json!({
        "key": "shifts",
        "title": title,
        "lines": 1,
        "open_lines": 1,
        "body": marked.iter().map(|(text, _)| text).collect::<Vec<_>>(),
        "fit_body": true,
        "body_loud": marked
            .iter()
            .enumerate()
            .filter(|(_, (_, loud))| *loud)
            .map(|(i, _)| i)
            .collect::<Vec<_>>(),
        "quiet": true,
        "unrated": true,
        "detail": [],
        "send": "",
        "nosend": true,
        "hint": SHIFT_HINT,
    }));
    if open {
        for entry in array(&map["entries"]).iter().take(4) {
            if let Some(sig) = entry["sig"].as_str().filter(|s| !s.is_empty()) {
                if entry["grounded"] != true {
                    continue;
                }
                told.push(json!({
                    "key": format!("shiftlike:{sig}"),
                    "like": format!("shift:{sig}"),
                    "like_source": "story_map",
                    "expand": sig,
                    "title": format!("{}: {}", string(&entry["kind"]), string(&entry["claim"])),
                    "lines": 2,
                    "open_lines": 4,
                    "detail": shift_source(entry, string(&map["by"]), 40),
                    "send": "",
                    "nosend": true,
                    "hint": SHIFT_HINT,
                }));
            }
        }
    }
    let quiet_row = |key: &str, title: &str| {
        json!({
            "key": key, "title": title, "lines": 1, "quiet": true, "unrated": true,
            "detail": [], "send": "", "nosend": true,
        })
    };
    if data["refreshing"] == true {
        told.push(quiet_row("shiftrefreshing", "updating the shift map…"));
    } else if let Some(note) = data["refresh_note"].as_str().filter(|s| !s.is_empty()) {
        told.push(quiet_row("shiftrefreshnote", note));
    } else if data["can_refresh"] == true && refresh_news(map, s) {
        let mut row = quiet_row("shiftrefresh", &refresh_label(&data["story_cost"]));
        row["refresh"] = json!(true);
        told.push(row);
    }
    told
}

/// The Shift Map's arrival: it opens when a new revision lands, holds `MAP_HOLD_S`, then folds
/// to its first shift. A map open by a click or the m key (pinned), or Claude mid-turn (busy),
/// is never folded under the reader; it folds on the first update after they let go.
#[derive(Default)]
pub struct MapMotion {
    seen: Option<(u64, Option<u64>)>,
    arrival: Option<f64>,
}
impl MapMotion {
    pub fn hold_seconds() -> f64 {
        let raw = std::env::var("CSR_MAP_HOLD_S").unwrap_or_default();
        let raw = raw.trim();
        match if raw.is_empty() {
            Some(MAP_HOLD_S)
        } else {
            raw.parse::<f64>().ok()
        } {
            Some(v) if v.is_nan() || v == f64::INFINITY => MAP_HOLD_S,
            Some(v) => v.max(0.),
            None => MAP_HOLD_S,
        }
    }
    /// (open, seconds to the next change by itself).
    pub fn update(
        &mut self,
        map: &Value,
        now: f64,
        pinned: bool,
        busy: bool,
    ) -> (bool, Option<f64>) {
        if !map.is_object() || array(&map["entries"]).is_empty() {
            self.seen = None;
            self.arrival = None;
            return (false, None);
        }
        let ident = map_stamp(map);
        if ident != self.seen {
            let first = self.seen.is_none();
            self.seen = ident;
            let fresh = map["at"].as_f64().is_some_and(|at| now - at < MAP_FRESH_S);
            self.arrival = (fresh || !first).then_some(now);
        }
        let hold = Self::hold_seconds();
        let Some(arrival) = self.arrival.filter(|_| hold > 0.) else {
            self.arrival = None;
            return (pinned, None);
        };
        let elapsed = (now - arrival).max(0.) + 1e-6;
        if elapsed < hold {
            return (true, Some((arrival + hold - now).max(0.)));
        }
        if pinned || busy {
            return (true, None);
        }
        self.arrival = None;
        (false, None)
    }
}

/// The pane's side of the Shift Map: what it shows now, its fold timer, and the refresh it has
/// started.
#[derive(Default)]
pub struct MapState {
    motion: MapMotion,
    /// When the map next folds by itself.
    pub due: Option<f64>,
    /// `data["story_map"]`, `story_map_from`, `story_map_messages` of the last frame.
    pub shown: Value,
    pub from: Option<String>,
    pub messages: Option<u64>,
    sources: HashMap<String, Session>,
    refresh: Option<Child>,
    refresh_from: Option<(u64, Option<u64>)>,
    refresh_note: Option<(String, f64)>,
    pub cost: Option<Value>,
}
impl MapState {
    /// The map to draw: this session's own, else the nearest session it continues that has one.
    /// An inherited map is counted against the transcript it was told from.
    pub fn load(&mut self, db: &Db, paths: &Paths, sid: &str) {
        self.shown = Value::Null;
        self.from = None;
        self.messages = None;
        if let Some((map, source)) = super::story::display_story_map(db, paths, sid) {
            self.shown = map_value(&map);
            if source != sid {
                let session = self
                    .sources
                    .entry(source.clone())
                    .or_insert_with(|| Session::new(&source));
                session.read_transcript(paths);
                self.messages = session.data["messages"].as_u64();
                self.from = Some(source);
            }
        }
    }
    /// The refresh's progress and the note it left; its fields for the page's data.
    pub fn poll(&mut self, now: f64) -> (bool, Option<String>) {
        let ended = match self.refresh.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => None,
                Ok(Some(status)) => Some(status.success()),
                Err(_) => Some(false),
            },
            None => None,
        };
        let refreshing = self.refresh.is_some() && ended.is_none();
        if let Some(success) = ended {
            self.refresh_note = if map_stamp(&self.shown) != self.refresh_from {
                None
            } else if success {
                Some("no new shift map: nothing asked since its telling".to_owned())
            } else {
                Some("the shift map was not updated: the refresh did not finish".to_owned())
            }
            .map(|words| (words, now + REFRESH_NOTE_LIFE));
            self.refresh = None;
        }
        let note = self
            .refresh_note
            .as_ref()
            .filter(|(_, until)| now < *until)
            .map(|(words, _)| words.clone());
        (refreshing, note)
    }
    /// The map's open state at `now`; sets when it next folds.
    pub fn motion(&mut self, now: f64, pinned: bool, busy: bool) -> Option<bool> {
        if !self.shown.is_object() {
            self.motion.update(&self.shown, now, pinned, busy);
            self.due = None;
            return None;
        }
        let (open, due) = self.motion.update(&self.shown, now, pinned, busy);
        self.due = due.map(|d| now + d);
        Some(open)
    }
    /// (seconds for the loop to wait, whether to draw a frame now): a frame once the map's hold
    /// is up and it folds.
    pub fn wait(&self, now: f64) -> (f64, bool) {
        match self.due {
            Some(due) if due <= now => (0.05, true),
            Some(due) => ((due - now).clamp(0.05, 1.0), false),
            None => (1.0, false),
        }
    }
    /// Start the refresh unless one runs: csr-engine tells this conversation's story again,
    /// detached; the pane makes no model call itself.
    pub fn start_refresh(
        &mut self,
        binary: Option<std::path::PathBuf>,
        transcript: Option<&str>,
        cwd: &str,
    ) {
        if self.refresh.is_some() {
            return;
        }
        self.refresh_from = map_stamp(&self.shown);
        self.refresh_note = None;
        let (Some(binary), Some(transcript)) = (binary, transcript.filter(|t| !t.is_empty()))
        else {
            return;
        };
        if cwd.is_empty() {
            return;
        }
        let argv = super::story::refresh_argv(transcript, cwd);
        let mut command = Command::new(binary);
        command
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("CSR_DISABLE_RECURSIVE_HOOKS", "1");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        self.refresh = command.spawn().ok();
    }
}
/// The engine that tells the story again: `CSR_ENGINE_BIN`, else this one.
pub fn engine_binary() -> Option<std::path::PathBuf> {
    std::env::var_os("CSR_ENGINE_BIN")
        .filter(|s| !s.is_empty())
        .map(Into::into)
        .or_else(|| std::env::current_exe().ok())
}

#[cfg(test)]
mod tests;
