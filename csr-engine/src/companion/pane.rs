use super::{
    array, cli::PaneArgs, db::Db, dreams, feed::Feed, infinite, info, layout, masthead, num, pages,
    paths::Paths, render::text::*, send, session::Session, string, threads, value,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn legend(
    view: &str,
    width: usize,
    reload: Option<u64>,
) -> (String, Vec<(usize, usize, String)>) {
    legend_parts(view, width, reload, Layout::default())
}
/// How the pane is laid out for the legend: on a phone, in column mode (a pane narrower than
/// `COLUMN_UNDER`), or zoomed to a phone's whole screen.
#[derive(Clone, Copy, Default)]
pub struct Layout {
    pub phone: bool,
    pub column: bool,
    pub zoomed: bool,
}
/// The bottom row and where each label sits on it: (text, [(first col, past last col, key)]).
/// On a phone the main page's keys start short, one word each, and f goes; zoomed, `z split`
/// gives the screen back to Claude. In column mode, the keys alone.
pub fn legend_parts(
    view: &str,
    width: usize,
    reload: Option<u64>,
    layout: Layout,
) -> (String, Vec<(usize, usize, String)>) {
    let Layout {
        phone,
        column,
        zoomed,
    } = layout;
    if column {
        let mut keys: Vec<&str> = match view {
            "main" => vec!["l", "s", "i", "q"],
            "summary" | "threads" | "info" => vec!["d", "j/k", "q"],
            _ => vec!["d", "q"],
        };
        while keys.len() > 1 && 1 + keys.join(" ").chars().count() > width {
            keys.remove(keys.len() - 2);
        }
        let mut at = 1;
        let spans = keys
            .iter()
            .map(|key| {
                let span = (at, at + len(key), (*key).to_owned());
                at += len(key) + 1;
                span
            })
            .collect();
        let shown = keys
            .iter()
            .map(|key| format!("{BOLD}{}{key}{OFF}", ink("you")))
            .collect::<Vec<_>>()
            .join(" ");
        return (format!(" {shown}"), spans);
    }
    let mut words: Vec<(String, String)> = if view == "main" && phone {
        let mut keys = vec![("l", "changes"), ("s", "summary"), ("i", "info")];
        if zoomed {
            keys.push(("z", "split"));
        }
        keys.push(("q", "close"));
        keys
    } else if view == "main" {
        vec![
            ("l", "changes since your sessions"),
            ("s", "compaction summary"),
            ("f", "every lost thread"),
            ("i", "session info"),
            ("q", "close"),
        ]
    } else if matches!(view, "summary" | "threads" | "info") {
        vec![("d", "back"), ("j/k", "scroll"), ("q", "close")]
    } else {
        vec![("d", "back"), ("q", "close")]
    }
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect();
    if let Some(seconds) = reload.filter(|_| view == "main") {
        let s = format!("↻ {seconds}s");
        let s = format!("{s}{}", " ".repeat(5_usize.saturating_sub(len(&s))));
        let n = words.len() - 1;
        words.insert(n, ("r".into(), s));
    }
    let wide = |words: &[(String, String)], gap: usize, marks: &str| {
        len(marks)
            + words
                .iter()
                .map(|(k, w)| len(k) + len(w) + 1 + gap)
                .sum::<usize>()
            - gap
            + 1
    };
    let mut marks = if matches!(view, "main" | "summary" | "threads" | "info") {
        ""
    } else {
        "~ changed  - removed  + back   "
    }
    .to_owned();
    if view == "main" && !phone {
        for (i, label) in [
            (0, "changes since your"),
            (0, "changes since"),
            (0, "changes"),
            (1, "summary"),
            (2, "thread"),
            (3, "info"),
        ] {
            if wide(&words, 2, &marks) <= width {
                break;
            }
            words[i].1 = label.into();
        }
    }
    let sep = if if view == "main" {
        wide(&words, 3, &marks) <= width
    } else {
        len(&marks)
            + words
                .iter()
                .map(|(k, w)| len(k) + len(w) + 4)
                .sum::<usize>()
            <= width
    } {
        "   "
    } else {
        "  "
    };
    let total = |words: &[(String, String)], marks: &str| {
        1 + len(marks)
            + words
                .iter()
                .map(|(k, w)| len(k) + 1 + len(w))
                .sum::<usize>()
            + len(sep) * words.len().saturating_sub(1)
    };
    if total(&words, &marks) > width {
        marks.clear();
    }
    while words.len() > 1 && total(&words, &marks) > width {
        let i = ["f", "s", "i", "r", "j/k"]
            .iter()
            .find_map(|name| {
                words
                    .iter()
                    .enumerate()
                    .find(|(i, (key, _))| key == name && *i > 0 && *i < words.len() - 1)
                    .map(|(i, _)| i)
            })
            .unwrap_or(words.len() - 2);
        words.remove(i);
    }
    if total(&words, &marks) > width {
        let (key, said) = words[0].clone();
        words = vec![(
            key.clone(),
            fit(&said, width.saturating_sub(len(&key) + 2).max(1)),
        )];
    }
    let shown = words
        .iter()
        .map(|(key, said)| format!("{BOLD}{}{key}{OFF}{} {said}{OFF}", ink("you"), ink("faint")))
        .collect::<Vec<_>>()
        .join(sep);
    let mut at = 1 + len(&marks);
    let spans = words
        .iter()
        .map(|(key, said)| {
            let start = at;
            let end = at + len(key) + 1 + len(said);
            at = end + len(sep);
            (start, end, key.clone())
        })
        .collect();
    (format!(" {}{marks}{OFF}{shown}", ink("faint")), spans)
}

/// What the info page's reads borrow from the pane while its session is also borrowed.
struct Wide<'a> {
    paths: &'a Paths,
    db: Option<&'a Db>,
    sid: &'a str,
    persist: bool,
}

pub struct Pane {
    paths: Paths,
    persist: bool,
    exporting: bool,
    db: Option<Db>,
    root: std::path::PathBuf,
    limit: usize,
    sid: String,
    agent: Option<String>,
    session: Option<Session>,
    pub view: String,
    pub open: Value,
    pub sent: Value,
    folds: Vec<Value>,
    feed: Feed,
    dreams: dreams::DreamLog,
    pub acts: BTreeMap<usize, Value>,
    pub top: usize,
    pub info_top: usize,
    pub threads_top: usize,
    pub summary_top: usize,
    spend: Option<value::Spend>,
    narrative: Option<(Value, f64, Value)>,
    tall: bool,
    anchor: Option<(String, usize)>,
    pub loaded: f64,
    shifts: Vec<Value>,
    total: usize,
    hold: Option<(Value, f64)>,
    pub pixels: bool,
    pub placed: Vec<super::render::kitty::Placement>,
    picture_cache: super::render::pictures::Cache,
    pub session_picture: Option<super::render::raster::SessionFacts>,
    pub last_picture: Option<super::render::raster::LastFacts>,
    map: super::shiftmap::MapState,
    pub phone: super::phone::Phone,
    /// This pane in herdr; set by the watch loop, absent in frame and test runs.
    pub herdr: Option<super::phone::Herdr>,
    // package C: the Codex rollouts, the readers behind "How Claude read you", and the dream
    // answer the pane keeps in place of the answer rows for six seconds (dream, answer, when)
    codex: super::codex_runs::CodexRolloutIndex,
    readers: super::readings::Readers,
    pub answered: Option<(String, String, f64)>,
    /// Column mode's gauge strip: (first column, first row past it); a click there is on no row.
    edge: Option<(usize, usize)>,
}
impl Pane {
    pub fn new(paths: Paths, args: &PaneArgs) -> Self {
        let sid = args.sid.clone().unwrap_or_default();
        let db = Db::open(&paths).ok();
        let root = db
            .as_ref()
            .and_then(|db| db.pane_root(&args.repo))
            .or_else(|| {
                super::paths::git(&args.repo, &["rev-parse", "--show-toplevel"]).map(Into::into)
            })
            .unwrap_or_else(|| args.repo.canonicalize().unwrap_or(args.repo.clone()));
        let feed = Feed::new(&paths, &sid);
        let pixels = super::render::kitty::pixels_enabled();
        let picture_cache = if pixels {
            super::render::pictures::Cache::with_fonts(super::sources::picture_fonts())
        } else {
            super::render::pictures::Cache::default()
        };
        let codex = super::codex_runs::CodexRolloutIndex::new(&paths.home);
        Self {
            paths,
            persist: !args.frame && args.png.is_none(),
            exporting: args.png.is_some(),
            db,
            root,
            limit: args.limit,
            session: (!sid.is_empty()).then(|| Session::new(&sid)),
            sid,
            agent: args.agent.clone(),
            view: args.page.clone(),
            open: Value::Null,
            sent: Value::Null,
            folds: vec![],
            feed,
            dreams: dreams::DreamLog::default(),
            acts: BTreeMap::new(),
            top: 0,
            info_top: 0,
            threads_top: 0,
            summary_top: 0,
            spend: None,
            narrative: None,
            tall: false,
            anchor: None,
            loaded: 0.,
            shifts: vec![],
            total: 0,
            hold: None,
            pixels,
            placed: vec![],
            picture_cache,
            session_picture: None,
            last_picture: None,
            map: Default::default(),
            phone: Default::default(),
            herdr: None,
            codex,
            readers: Default::default(),
            answered: None,
            edge: None,
        }
    }
    pub fn frame(&mut self, cols: usize, rows: usize, now: f64) -> Vec<String> {
        self.placed.clear();
        let cols = cols.max(1);
        let rows = rows.max(1);
        if self.db.is_none() {
            self.db = Db::open(&self.paths).ok();
            if self.db.is_none() {
                self.acts.clear();
                let said = if self.paths.db.exists() {
                    "could not read csr-engine"
                } else {
                    "CSR is not set up yet: npx claude-self-reflect setup"
                };
                let mut out = vec![format!("{BOLD}CSR{OFF}")];
                out.extend(
                    wrap(said, cols.saturating_sub(1).max(1), 4)
                        .into_iter()
                        .map(|line| format!("{DIM}{line}{OFF}")),
                );
                out.truncate(rows);
                out.resize(rows, String::new());
                return out;
            }
        }
        let phone = self.phone.read(self.herdr.as_ref(), cols, rows, now);
        let column = cols < super::phone::COLUMN_UNDER;
        self.phone.settle(self.herdr.as_ref(), column, phone);
        // column mode's strip takes the pane's last columns; the pages draw left of it
        let tapes = layout::strip_tapes(cols, rows, column);
        let body_cols = cols - layout::strip_width(tapes);
        if self.sent[2].as_f64().is_some_and(|at| now - at > 6.) {
            if self.open == self.sent[0] {
                self.open = Value::Null;
            }
            self.sent = Value::Null;
        }
        if self.answered.as_ref().is_some_and(|a| now - a.2 > 6.) {
            self.answered = None;
        }

        if self.loaded == 0. || now - self.loaded >= 60. {
            self.shifts = self
                .db
                .as_ref()
                .map(|db| db.shifts(&self.root, usize::MAX))
                .unwrap_or_default();
            self.total = self.shifts.len();
            self.shifts.truncate(self.limit);
            self.loaded = now;
            self.map.cost = self.db.as_ref().and_then(super::shiftmap::story_cost);
        }
        if let Some(db) = self.db.as_ref() {
            self.map.load(db, &self.paths, &self.sid);
        }
        let (refreshing, refresh_note) = self.map.poll(now);
        self.dreams.poll(&self.paths);
        let records = &self.dreams.records;
        let root = self.root.to_string_lossy();
        let kept = masthead::kept_status(&self.paths, &self.sid, now);
        let story = self
            .db
            .as_ref()
            .map(|db| pages::session_story(db, &self.paths, &self.sid))
            .unwrap_or(Value::Null);
        let mut data = json!({
            "story_map": self.map.shown,
            "story_map_from": self.map.from,
            "story_map_messages": self.map.messages,
            "refreshing": refreshing,
            "refresh_note": refresh_note,
            "story_cost": self.map.cost,
            "repo": root,
            "shifts": self.shifts,
            "total": self.total,
            "dreams": self.dreams.dreams(&root),
            "chapter": dreams::last_chapter(records, &root, &self.sid),
            "look": dreams::last_look(records, &root),
            "dreams_on": dreams::dreams_on(&self.paths),
            "story": story,
            "claude_fields": kept["fields"],
            "claude_at": kept["at"],
            "limits": masthead::account_limits(&self.paths, now),
            "sts": masthead::csr_status_at(&self.paths, now),
            "compacting": masthead::compacting_info(
                &self.paths,
                &self.sid,
                now,
                self.persist,
            ),
        });
        if let Some(session) = self.session.as_mut() {
            session.read_transcript(&self.paths);
        }
        let summary = match (&self.session, self.view.as_str()) {
            (Some(session), "summary") => threads::newest_summary(session.path.as_deref()),
            _ => Value::Null,
        };
        let every = match (&self.session, self.view.as_str()) {
            (Some(session), "threads") => threads::lost_value(&self.paths, session),
            _ => vec![],
        };
        if let Some(session) = &self.session {
            if session.data["last_session"].is_null() {
                let previous = self.previous(&session.data);
                self.session.as_mut().unwrap().data["last_session"] = previous;
            }
        }
        let can_refresh = self
            .session
            .as_ref()
            .is_some_and(|session| session.path.is_some())
            && super::shiftmap::engine_binary().is_some();
        let mut empty = json!({"id":null});
        let s = self
            .session
            .as_mut()
            .map(|s| s.load(&self.paths, self.db.as_ref()))
            .unwrap_or(&mut empty);
        if let Some(id) = s["last_session"]["id"].as_str().map(str::to_owned) {
            s["last_session"]["running"] =
                json!(super::session::session_record(&self.paths, &id).is_some());
        }
        if s["last_session"]["todos"].is_array() {
            s["todos"] = s["last_session"]["todos"].clone();
        }

        self.session_picture = (!string(&s["id"]).is_empty())
            .then(|| super::render::raster::SessionFacts::from_value(s, now));
        self.last_picture = s["last_session"]
            .is_object()
            .then(|| super::render::raster::LastFacts::from_value(&s["last_session"]));
        data["map_view"] = match self.map.motion(
            now,
            super::shiftmap::map_open(&self.open),
            s["busy"] == true,
        ) {
            Some(open) => json!({ "open": open }),
            None => Value::Null,
        };
        data["can_refresh"] = json!(can_refresh);
        if self.view == "main" {
            // package C: In flight (agents, Codex runs) and How Claude read you
            data["agents"] = json!(super::agents::agents_of(&self.paths, &self.sid));
            data["codex"] = if self.codex.present() {
                self.codex.refresh(now)
            } else {
                Value::Null
            };
            data["readings"] = json!(self.readers.how_claude_read(&self.paths, &self.sid, 2, now));
        }
        let ui = json!({
            "answered": self.answered,
            "pixels": self.pixels,
            "rows": rows,
            "cols": cols,
            "phone": phone,
            "column": column,
            "now": now,
            "open": self.open,
            "sent": self.sent,
            "folds": self.folds,
            "likes": if self.exporting { Value::Null } else { send::read_likes(&self.paths) },
        });
        if self.view == "info" {
            Self::info_data(
                Wide {
                    paths: &self.paths,
                    db: self.db.as_ref(),
                    sid: &self.sid,
                    persist: self.persist,
                },
                (&mut self.narrative, &mut self.spend),
                &mut data,
                s,
                now,
            );
        }
        // the value row above the main page's legend (not in column mode, not in an export)
        let value = if self.view == "main" && !column && !self.exporting {
            Self::week_data(
                &Wide {
                    paths: &self.paths,
                    db: self.db.as_ref(),
                    sid: &self.sid,
                    persist: self.persist,
                },
                (&mut self.narrative, &mut self.spend),
                &mut data,
                now,
            );
            layout::value_rows(&data["value"], cols - 1)
        } else {
            vec![]
        };
        let room = rows.saturating_sub(value.len());
        let extra = matches!(self.view.as_str(), "info" | "summary" | "threads");
        let page = if extra {
            Value::Null
        } else if self.view == "log" {
            pages::page_log(&data)
        } else {
            let page = pages::page_main(&data, s, now, &ui, self.db.as_ref());
            if self.exporting || column {
                page
            } else if s["busy"] == true && self.hold.as_ref().is_some_and(|(_, at)| now - at < 120.)
            {
                let mut page = page;
                // package C: In flight is never held; it is drawn fresh among the held ones
                let fresh: Vec<Value> = array(&page["sections"])
                    .iter()
                    .filter(|s| s["name"] == "flight" && !array(&s["items"]).is_empty())
                    .cloned()
                    .collect();
                let mut sections: Vec<Value> =
                    super::feed::living(array(&self.hold.as_ref().unwrap().0).to_vec(), now)
                        .into_iter()
                        .filter(|s| s["name"] != "flight")
                        .collect();
                let at = usize::from(sections.first().is_some_and(|s| s["name"] == "story"));
                sections.splice(at..at, fresh);
                page["sections"] = json!(sections);
                page
            } else {
                let page = self.feed.page(page, now);
                self.hold = Some((page["sections"].clone(), now));
                page
            }
        };
        self.tall =
            self.view == "main" && !column && (self.tall || self.top > 0 || self.open.is_string());
        let mut strip_grounds = vec![];
        let mut lines = match self.view.as_str() {
            "info" if column => {
                let (lines, grounds, top) =
                    layout::draw_column_info(&data, s, body_cols, rows, self.info_top, now);
                self.info_top = top;
                strip_grounds = grounds;
                lines
            }
            "main" if column => {
                let (lines, grounds) = layout::draw_column_main(&page, body_cols, phone);
                strip_grounds = grounds;
                lines
            }
            "info" => {
                let (lines, top) =
                    info::draw_info(&data, s, cols, rows, self.info_top, now, self.pixels);
                self.info_top = top;
                lines
            }
            "summary" => {
                let (lines, top) =
                    threads::draw_summary(&summary, body_cols, rows, self.summary_top);
                self.summary_top = top;
                lines
            }
            "threads" => {
                let (lines, top) = threads::draw_threads(
                    &every,
                    body_cols,
                    rows,
                    &self.open,
                    &self.sent,
                    self.threads_top,
                    false,
                );
                self.threads_top = top;
                lines
            }
            _ => pages::draw_page(&page, body_cols, if self.tall { 10000 } else { room }, &ui),
        };
        if column && !matches!(self.view.as_str(), "main" | "info") {
            // a page laid out at its narrowest, its words rewrapped
            lines = layout::column_reflow(&lines, body_cols);
        }
        if self.exporting {
            return lines.rows;
        }
        if let Some((key, row)) = self.anchor.take() {
            if let Some(actual) = lines
                .acts
                .iter()
                .filter(|(_, act)| act["key"] == key)
                .map(|(r, _)| *r)
                .min()
            {
                self.top = actual.saturating_sub(row);
            }
        }
        // the rows the page shows: the screen less the value row, the legend's own still counted
        let shown = room.saturating_sub(1);
        let top = if self.view == "main" && !column {
            self.top = self.top.min(lines.rows.len().saturating_sub(shown));
            self.top
        } else {
            0
        };
        if self.view == "main" && !column {
            let names = lines
                .heads
                .iter()
                .filter(|(r, _)| *r >= top && *r < top + shown)
                .map(|(_, n)| n.clone())
                .collect::<Vec<_>>();
            self.feed.shown(&names, now);
            if self.persist {
                self.feed.save(now);
            }
        }
        self.acts = lines
            .acts
            .into_iter()
            .filter(|(r, _)| *r >= top && *r < top + shown)
            .map(|(r, a)| (r - top, a))
            .collect();
        if self.pixels {
            let cell = super::render::kitty::cell_size();
            self.placed = lines
                .pictures
                .iter()
                .filter_map(|wanted| {
                    if wanted.row < top || wanted.row + wanted.rows > top + shown {
                        return None;
                    }
                    let mut wanted = wanted.clone();
                    wanted.row -= top;
                    wanted.place(&mut self.picture_cache, cell)
                })
                .collect();
            let logo = lines
                .pictures
                .iter()
                .any(|p| matches!(p.picture, super::render::pictures::Picture::Logo));
            if top == 0 && logo && !self.placed.iter().any(|p| p.col == 1 && p.cols == 4) {
                if let Some(row) = lines.rows.first_mut() {
                    *row = row.replacen(&slot("    ", "normal"), &slot("CTX ", "normal"), 1);
                }
            }
        }
        let body = lines
            .rows
            .drain(top..)
            .take(shown)
            .map(|s| safe_line(&s))
            .collect::<Vec<_>>();
        let mut body = body;
        body.resize(shown, String::new());
        if !value.is_empty() {
            // a click on the value row opens the i page, which says it in words
            self.acts.insert(shown, json!({"key": "view:info"}));
        }
        body.extend(value);
        if tapes > 0 {
            // after each row, by absolute position: a repainted row repaints its strip cells
            let g = masthead::gauges_compacting(
                &data["claude_fields"],
                data["claude_at"].as_f64(),
                s,
                now,
                &data["limits"],
                &data["sts"],
                &data["compacting"],
            );
            let columns = layout::gauge_strip(&g, tapes, rows - 2);
            for (row, strip) in layout::strip_rows(&columns, cols, &strip_grounds)
                .into_iter()
                .enumerate()
            {
                if let Some(line) = body.get_mut(row) {
                    line.push_str(&strip);
                }
            }
            self.edge = Some((body_cols, rows - 1));
        } else {
            self.edge = None;
        }
        let countdown = layout::reload_in(self.loaded, now);
        let (foot, spans) = legend_parts(
            &self.view,
            cols.saturating_sub(1),
            if self.view == "main" && !column {
                Some(countdown)
            } else {
                None
            },
            Layout {
                phone,
                column,
                zoomed: self.phone.zoomed,
            },
        );
        self.phone.foot = Some((phone, column));
        self.acts.insert(
            rows - 1,
            json!({
                "key": "legend",
                "keys": spans,
            }),
        );
        body.push(foot);
        body
    }
    /// What only the info page reads: the plan's week, what CSR cost in it, the infinite-session
    /// rows and the facts under a running compaction.
    fn info_data(
        env: Wide,
        (narrative_kept, spend_kept): (&mut Option<(Value, f64, Value)>, &mut Option<value::Spend>),
        data: &mut Value,
        s: &Value,
        now: f64,
    ) {
        Self::week_data(&env, (&mut *narrative_kept, &mut *spend_kept), data, now);
        let Wide { paths, db, sid, .. } = env;
        data["infinite"] = infinite::infinite_info(paths, sid, now, s);
        if data["compacting"].is_object() {
            let facts = db
                .map(|db| {
                    infinite::compaction_facts(db, sid, &data["story"], data["dreams_on"] == true)
                })
                .unwrap_or_default();
            data["compacting"]["facts"] = json!(facts);
        }
    }
    /// What the value row and the info page's cost section read: the plan's week, what CSR cost
    /// in it, and the counts of what you took.
    fn week_data(
        env: &Wide,
        (narrative_kept, spend_kept): (&mut Option<(Value, f64, Value)>, &mut Option<value::Spend>),
        data: &mut Value,
        now: f64,
    ) {
        let Wide {
            paths, db, persist, ..
        } = *env;
        let plan = value::plan_of(&data["claude_fields"], &data["limits"], now);
        let week = if plan["mode"] == "plan" {
            plan["start"].clone()
        } else {
            plan["mode"].clone()
        };
        let stale = narrative_kept
            .as_ref()
            .is_none_or(|(key, at, _)| *key != week || now - at >= 60.);
        if stale {
            let narrative = db
                .map(|db| value::narrative_week(db, num(&plan["start"])))
                .unwrap_or_else(|| json!({}));
            *narrative_kept = Some((week, now, narrative));
        }
        let narrative = narrative_kept.as_ref().map_or(json!({}), |n| n.2.clone());
        let mut spent = Value::Null;
        if plan["mode"] == "plan" {
            let start = num(&plan["start"]);
            let spend = spend_kept.get_or_insert_with(|| value::Spend::new(paths, persist));
            let budget = if persist { 0.1 } else { 30. };
            spend.step(start, now, std::time::Duration::from_secs_f64(budget));
            if spend.ready && spend.start() == Some(start) {
                spent = spend.total();
            }
        }
        data["value"] = value::week_value(paths, &plan, &narrative, &spent);
        data["plan"] = plan;
        data["narrative"] = narrative;
        data["spend"] = spent;
    }
    fn previous(&self, s: &Value) -> Value {
        if let Some(path) = self.paths.kept_file("previous", &self.sid) {
            let cached = super::paths::read_json(&path);
            if cached.is_object() {
                return cached;
            }
        }
        let mine = self
            .session
            .as_ref()
            .and_then(|s| s.path.clone())
            .or_else(|| {
                s["cwd"].as_str().map(|cwd| {
                    self.paths
                        .project_folder(cwd)
                        .join(format!("{}.jsonl", self.sid))
                })
            });
        let Some(mine) = mine else {
            return Value::Null;
        };
        let Some(path) = super::sources::previous_session(&self.paths, &mine, num(&s["started"]))
        else {
            return json!({});
        };
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let mut prev = Session::new(id);
        prev.path = Some(path.clone());
        prev.read_transcript(&self.paths);
        let facts = super::session::previous_facts(&self.paths, &prev, self.db.as_ref());
        if self.persist {
            if let Some(path) = self.paths.kept_file("previous", &self.sid) {
                let _ = super::private::write_atomic(
                    &path,
                    &super::private::dumps(&facts),
                    &self.paths.keep,
                );
            }
        }
        facts
    }
    pub fn key(&mut self, key: &str) -> bool {
        match key {
            "q" => return false,
            "d" | "l" => {
                self.view = if key == "d" { "main" } else { "log" }.into();
                self.open = Value::Null;
            }
            "s" | "f" | "i" => {
                let view = match key {
                    "s" => "summary",
                    "f" => "threads",
                    _ => "info",
                };
                self.view = if self.view == view { "main" } else { view }.into();
                self.open = Value::Null;
                match self.view.as_str() {
                    "threads" => self.threads_top = 0,
                    "info" => self.info_top = 0,
                    _ => (),
                }
            }
            "r" => {
                self.loaded = 0.;
                self.hold = None;
            }
            "escape" if matches!(self.view.as_str(), "summary" | "threads" | "info") => {
                self.view = "main".into();
                self.open = Value::Null;
            }
            "escape" => {
                self.open = Value::Null;
                self.top = 0;
                self.tall = false;
            }
            "m" => self.map_key(),
            "z" => {
                self.phone.zoom_key(self.herdr.as_ref());
            }
            "j" | "down" => self.scroll(3),
            "k" | "up" => self.scroll(-3),
            " " => self.scroll(12),
            _ => (),
        }
        true
    }
    /// m: open the Shift Map on the main page at its full size, or fold it again. Nothing
    /// happens with no map to open, or on another page.
    fn map_key(&mut self) {
        if self.view != "main" || !self.map.shown.is_object() {
            return;
        }
        if super::shiftmap::map_open(&self.open) {
            self.open = Value::Null;
            self.sent = Value::Null;
        } else {
            self.open = json!("shifts");
            self.sent = Value::Null;
            self.tall = true;
        }
    }
    /// (seconds for the watch loop's wait, whether to draw a frame now): a frame once the Shift
    /// Map's hold is up and it folds.
    pub fn map_wait(&self, now: f64) -> (f64, bool) {
        self.map.wait(now)
    }
    pub fn scroll(&mut self, step: isize) {
        let top = match self.view.as_str() {
            "main" => &mut self.top,
            "summary" => &mut self.summary_top,
            "threads" => &mut self.threads_top,
            "info" => &mut self.info_top,
            _ => return,
        };
        *top = top.saturating_add_signed(step);
    }
    /// A click on a thread's answers: drop and later are kept in the log; the two asks are typed
    /// into Claude's prompt, unsent, and kept as sent.
    fn lost_click(&mut self, act: &Value, col: usize, now: f64) {
        let pick = array(&act["spans"])
            .iter()
            .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
            .map(|s| string(&s[2]).to_owned());
        let project = threads::project_of(self.session.as_ref());
        match pick.as_deref() {
            Some(pick @ ("drop" | "later")) => threads::keep_lost(
                &self.paths,
                string(&act["lost"]),
                pick,
                now,
                project.as_deref(),
                json!({}),
            ),
            Some(pick @ ("side" | "sub")) => {
                let agent = send::agent_pane(self.agent.as_deref());
                let ask = string(&act["asks"][pick]).to_owned();
                let handoff = if pick == "sub" {
                    threads::new_handoff(&self.paths, &self.sid, string(&act["lost"]))
                } else {
                    None
                };
                let went = match &handoff {
                    Some((_, envelope)) => send::send_draft(
                        agent.as_deref(),
                        &format!("{ask}\n{envelope}"),
                        self.agent.as_deref(),
                    ),
                    None => send::send_text(agent.as_deref(), &ask),
                };
                self.sent = json!([act["item"], went, now]);
                if went {
                    let more = match &handoff {
                        Some((request, _)) => json!({"how": pick, "request": request}),
                        None => json!({"how": pick}),
                    };
                    threads::keep_lost(
                        &self.paths,
                        string(&act["lost"]),
                        "sent",
                        now,
                        project.as_deref(),
                        more,
                    );
                }
                send::focus_back();
            }
            _ => (),
        }
    }
    pub fn click(&mut self, row: usize, col: usize, now: f64) -> bool {
        let act = self.acts.get(&row).cloned();
        // On a phone's narrow column a tap anywhere but the legend and the masthead (the i
        // page) fills the screen.
        if self.phone.foot == Some((true, true))
            && self.view == "main"
            && !act
                .as_ref()
                .is_some_and(|a| a["key"] == "legend" || string(&a["key"]).starts_with("view:"))
        {
            self.phone.zoom_key(self.herdr.as_ref());
            return true;
        }
        // a click on the gauge strip is on no row
        if self
            .edge
            .is_some_and(|(edge, rows)| col >= edge && row < rows)
        {
            return true;
        }
        let Some(act) = act else {
            self.open = Value::Null;
            return true;
        };
        if act["key"] == "legend" {
            if let Some(span) = array(&act["keys"])
                .iter()
                .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
            {
                if matches!(string(&span[2]), "d" | "l" | "r" | "s" | "f" | "i" | "z") {
                    return self.key(string(&span[2]));
                }
                return true;
            }
            return true;
        }
        if let Some(h) = act["heart"].as_array() {
            if col as f64 >= num(&h[0]) && (col as f64) < num(&h[1]) {
                let likes = send::read_likes(&self.paths);
                let key = act["like"].as_str().unwrap_or(string(&act["key"]));
                let _ = send::keep_like(&self.paths, &act, likes[key] != true, now);
                return true;
            }
        }
        if act["lost"].is_string() && act["spans"].is_array() {
            self.lost_click(&act, col, now);
            return true;
        }
        if let Some(span) = array(&act["spans"])
            .iter()
            .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
        {
            let _ = dreams::keep(
                &self.paths,
                &json!({
                    "kind": "answer",
                    "id": act["dream"],
                    "answer": span[2],
                    "at": now,
                    "by": "pane",
                }),
            );
            if span[2] == "ask_claude" {
                let agent = send::agent_pane(self.agent.as_deref());
                let went = send::send_text(agent.as_deref(), string(&act["send"]));
                self.sent = json!([act["key"], went, now]);
                send::focus_back();
            }
            self.answered = Some((string(&act["dream"]).into(), string(&span[2]).into(), now));
            self.hold = None;
            return true;
        }
        if !array(&act["spans"]).is_empty() {
            // A click that misses a row's answers closes whatever is open.
            self.open = Value::Null;
            return true;
        }
        let key = string(&act["key"]);
        if let Some(view) = key.strip_prefix("view:") {
            if matches!(view, "main" | "log" | "info" | "summary" | "threads") {
                self.view = view.into();
                self.open = Value::Null;
                if view == "info" {
                    self.info_top = 0;
                }
            }
            return true;
        }
        if let Some(fold) = key.strip_prefix("fold:") {
            let v = json!(fold);
            if self.folds.contains(&v) {
                self.folds.retain(|s| s != &v);
            } else {
                self.folds.push(v);
            }
            self.hold = None;
            return true;
        }
        if let Some(back) = act["back"].as_str() {
            self.feed.back(back, now);
            self.open = Value::Null;
            self.hold = None;
            self.top = 0;
            return true;
        }
        if act["refresh"] == true {
            // The shift map's refresh: csr-engine tells the story again, detached; the pane
            // waits for a new telling.
            let cwd = self
                .session
                .as_ref()
                .and_then(|s| super::session::session_record(&self.paths, &s.id))
                .and_then(|record| record["cwd"].as_str().map(str::to_owned))
                .filter(|cwd| !cwd.is_empty())
                .unwrap_or_else(|| self.root.to_string_lossy().into_owned());
            let transcript = self
                .session
                .as_ref()
                .and_then(|s| s.path.as_ref())
                .map(|p| p.to_string_lossy().into_owned());
            self.map.start_refresh(
                super::shiftmap::engine_binary(),
                transcript.as_deref(),
                &cwd,
            );
            return true;
        }
        if act["key"] != self.open {
            self.open = act["key"].clone();
            if self.view == "main" {
                self.anchor = Some((key.to_owned(), row));
            }
            self.sent = Value::Null;
            self.hold = None;
            if let Some(sig) = act["expand"].as_str() {
                // Opening an entry is the expand signal: kept once, never sent anywhere.
                let _ = send::keep_shift_signal(&self.paths, sig, string(&act["title"]), now);
            }
        } else if act["nosend"] == true {
            // A report is its words, not yours: a second click folds the row, it sends nothing.
            self.open = if act["expand"].is_string() {
                json!("shifts")
            } else {
                Value::Null
            };
            self.sent = Value::Null;
        } else {
            let agent = send::agent_pane(self.agent.as_deref());
            let went = send::send_text(agent.as_deref(), string(&act["send"]));
            self.sent = json!([act["key"], went, now]);
            if went && act["lost"].is_string() {
                threads::keep_lost(
                    &self.paths,
                    string(&act["lost"]),
                    "sent",
                    now,
                    threads::project_of(self.session.as_ref()).as_deref(),
                    json!({"how": "side"}),
                );
            }
            if went && act["dream"].is_string() {
                let _ = dreams::keep(
                    &self.paths,
                    &json!({
                        "kind": "answer",
                        "id": act["dream"],
                        "answer": "ask_claude",
                        "at": now,
                        "by": "pane",
                    }),
                );
                self.answered = Some((string(&act["dream"]).into(), "ask_claude".into(), now));
                self.hold = None;
            }
            send::focus_back();
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_the_legend_keeps_close_at_phone_width() {
        let (s, spans) = legend("main", 37, Some(42));
        assert!(len(&plain(&s)) <= 37);
        assert!(spans.iter().any(|(_, _, k)| k == "q"));
    }
    #[test]
    fn test_log_marks_go_before_keys_do() {
        let (s, spans) = legend("log", 30, None);
        assert!(!plain(&s).contains("changed"));
        assert_eq!(
            spans.iter().map(|(_, _, s)| s.as_str()).collect::<Vec<_>>(),
            vec!["d", "q"]
        );
    }
}
