use super::{
    array, cli::PaneArgs, db::Db, dreams, feed::Feed, masthead, num, pages, paths::Paths,
    render::text::*, send, session::Session, string,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn legend(
    view: &str,
    width: usize,
    reload: Option<u64>,
) -> (String, Vec<(usize, usize, String)>) {
    let mut words: Vec<(String, String)> = if view == "main" {
        vec![
            ("l", "changes since your sessions"),
            ("s", "compaction summary"),
            ("f", "every lost thread"),
            ("i", "session info"),
            ("q", "close"),
        ]
    } else {
        vec![("d", "back"), ("q", "close")]
    }
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect();
    if let Some(seconds) = reload {
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
    let mut marks = if view == "main" {
        ""
    } else {
        "~ changed  - removed  + back   "
    }
    .to_owned();
    if view == "main" {
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
    sent: Value,
    folds: Vec<Value>,
    feed: Feed,
    dreams: dreams::DreamLog,
    pub acts: BTreeMap<usize, Value>,
    pub top: usize,
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
        if self.sent[2].as_f64().is_some_and(|at| now - at > 6.) {
            if self.open == self.sent[0] {
                self.open = Value::Null;
            }
            self.sent = Value::Null;
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
        }
        self.dreams.poll(&self.paths);
        let records = &self.dreams.records;
        let root = self.root.to_string_lossy();
        let kept = masthead::kept_status(&self.paths, &self.sid, now);
        let story = self
            .db
            .as_ref()
            .map(|db| pages::session_story(db, &self.paths, &self.sid))
            .unwrap_or(Value::Null);
        let data = json!({
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
        if let Some(session) = &self.session {
            if session.data["last_session"].is_null() {
                let previous = self.previous(&session.data);
                self.session.as_mut().unwrap().data["last_session"] = previous;
            }
        }
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
        let ui = json!({
            "pixels": self.pixels,
            "rows": rows,
            "now": now,
            "open": self.open,
            "sent": self.sent,
            "folds": self.folds,
            "likes": if self.exporting { Value::Null } else { send::read_likes(&self.paths) },
        });
        let page = if self.view == "log" {
            pages::page_log(&data)
        } else {
            let page = pages::page_main(&data, s, now, &ui, self.db.as_ref());
            if self.exporting {
                page
            } else if s["busy"] == true && self.hold.as_ref().is_some_and(|(_, at)| now - at < 120.)
            {
                let mut page = page;
                page["sections"] = json!(super::feed::living(
                    array(&self.hold.as_ref().unwrap().0).to_vec(),
                    now,
                ));
                page
            } else {
                let page = self.feed.page(page, now);
                self.hold = Some((page["sections"].clone(), now));
                page
            }
        };
        self.tall = self.view == "main" && (self.tall || self.top > 0 || self.open.is_string());
        let mut lines = pages::draw_page(&page, cols, if self.tall { 10000 } else { rows }, &ui);
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
        let top = if self.view == "main" {
            self.top = self
                .top
                .min(lines.rows.len().saturating_sub(rows.saturating_sub(1)));
            self.top
        } else {
            0
        };
        if self.view == "main" {
            let names = lines
                .heads
                .iter()
                .filter(|(r, _)| *r >= top && *r < top + rows.saturating_sub(1))
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
            .filter(|(r, _)| *r >= top && *r < top + rows.saturating_sub(1))
            .map(|(r, a)| (r - top, a))
            .collect();
        if self.pixels {
            let cell = super::render::kitty::cell_size();
            self.placed = lines
                .pictures
                .iter()
                .filter_map(|wanted| {
                    if wanted.row < top || wanted.row + wanted.rows > top + rows.saturating_sub(1) {
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
            .take(rows.saturating_sub(1))
            .map(|s| safe_line(&s))
            .collect::<Vec<_>>();
        let mut body = body;
        body.resize(rows.saturating_sub(1), String::new());
        let countdown = (60. - (now - self.loaded)).ceil().max(0.) as u64;
        let (foot, spans) = legend(
            &self.view,
            cols.saturating_sub(1),
            if self.view == "main" {
                Some(countdown)
            } else {
                None
            },
        );
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
        let mut previous = super::session::lineage_paths(&self.paths, &self.sid)
            .get(1)
            .cloned();
        if previous.is_none() {
            let started = num(&s["started"]);
            let mut candidates: Vec<_> = super::paths::folders(mine.parent().unwrap())
                .into_iter()
                .filter_map(|path| {
                    if path == mine
                        || path.extension().is_none_or(|s| s != "jsonl")
                        || path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .is_none_or(|id| self.paths.transcript(id).is_none())
                    {
                        return None;
                    }
                    let t = std::fs::metadata(&path)
                        .ok()?
                        .modified()
                        .ok()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_secs_f64();
                    (t <= started && t > started - 7. * 86400.).then_some((t, path))
                })
                .collect();
            candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
            use std::io::Read;
            previous = candidates.into_iter().take(200).find_map(|(_, path)| {
                let file = std::fs::File::open(&path).ok()?;
                let mut bytes = Vec::new();
                file.take(65536).read_to_end(&mut bytes).ok()?;
                regex::Regex::new(r#""entrypoint"\s*:\s*"cli""#)
                    .unwrap()
                    .is_match(&String::from_utf8_lossy(&bytes))
                    .then_some(path)
            });
        }
        let Some(path) = previous else {
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
            "r" => {
                self.loaded = 0.;
                self.hold = None;
            }
            "escape" => {
                self.open = Value::Null;
                self.top = 0;
                self.tall = false;
            }
            "j" | "down" => self.scroll(3),
            "k" | "up" => self.scroll(-3),
            " " => self.scroll(12),
            _ => (),
        }
        true
    }
    pub fn scroll(&mut self, step: isize) {
        if self.view == "main" {
            self.top = self.top.saturating_add_signed(step);
        }
    }
    pub fn click(&mut self, row: usize, col: usize, now: f64) -> bool {
        let Some(act) = self.acts.get(&row).cloned() else {
            self.open = Value::Null;
            return true;
        };
        if act["key"] == "legend" {
            if let Some(span) = array(&act["keys"])
                .iter()
                .find(|s| num(&s[0]) <= col as f64 && (col as f64) < num(&s[1]))
            {
                if matches!(string(&span[2]), "d" | "l" | "r") {
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
            self.hold = None;
            return true;
        }
        let key = string(&act["key"]);
        if let Some(view) = key.strip_prefix("view:") {
            if matches!(view, "main" | "log") {
                self.view = view.into();
                self.open = Value::Null;
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
        if act["key"] != self.open {
            self.open = act["key"].clone();
            if self.view == "main" {
                self.anchor = Some((key.to_owned(), row));
            }
            self.sent = Value::Null;
            self.hold = None;
        } else {
            let agent = send::agent_pane(self.agent.as_deref());
            let went = send::send_text(agent.as_deref(), string(&act["send"]));
            self.sent = json!([act["key"], went, now]);
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
