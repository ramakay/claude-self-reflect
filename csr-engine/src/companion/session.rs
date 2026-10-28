use super::{
    db::Db,
    paths::{epoch, folders, read_json, valid_id, Paths},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

pub fn session_record(paths: &Paths, sid: &str) -> Option<Value> {
    if !valid_id(sid) {
        return None;
    }
    folders(&paths.home.join(".claude/sessions"))
        .iter()
        .find_map(|p| {
            let v = read_json(p);
            if v["sessionId"] != sid || v["cwd"].as_str().is_none_or(str::is_empty) {
                return None;
            }
            if let Some(pid) = v["pid"].as_i64() {
                let pid = i32::try_from(pid).ok()?;
                if pid <= 1 {
                    return None;
                }
                // kill(0) probes liveness, including a process owned by someone else.
                if unsafe { libc::kill(pid, 0) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
                {
                    return None;
                }
            }
            Some(json!({
                "cwd": v["cwd"],
                "started": v["startedAt"].as_f64()? / 1000.,
                "busy": v["status"] == "busy",
            }))
        })
}

pub fn lineage_paths(paths: &Paths, sid: &str) -> Vec<PathBuf> {
    let Some(mut path) = paths.transcript(sid) else {
        return vec![];
    };
    let mut result = vec![path.clone()];
    let mut ids = vec![sid.to_owned()];
    if std::env::var("CSR_NO_CONTINUED")
        .is_ok_and(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true"))
    {
        return result;
    }
    while result.len() <= 5 {
        let Some(old) = super::sources::continued_from_at(
            paths,
            ids.last().unwrap(),
            Some(&path),
            super::paths::now(),
        ) else {
            break;
        };
        if !valid_id(&old) || ids.contains(&old) || paths.transcript(&old).is_none() {
            break;
        }
        let sibling = path.parent().unwrap().join(format!("{old}.jsonl"));
        if !sibling.is_file() {
            break;
        }
        path = sibling;
        result.push(path.clone());
        ids.push(old);
    }
    result
}
pub fn lineage(paths: &Paths, sid: &str) -> Vec<String> {
    let found = lineage_paths(paths, sid);
    if found.is_empty() {
        return vec![sid.to_owned()];
    }
    found
        .iter()
        .filter_map(|p| p.file_stem()?.to_str().map(str::to_owned))
        .collect()
}

pub struct Session {
    pub id: String,
    pub path: Option<PathBuf>,
    tail: super::tail::Tail,
    dirty: bool,
    looked: f64,
    main_id: Option<String>,
    measuring: Option<usize>,
    pub data: Value,
    pub file_order: Vec<String>,
    pub readings: Vec<super::coach::Reading>,
    reading_waiting: bool,
    track_readings: bool,
    replies: BTreeMap<String, Value>,
    tally: super::breakdown::Tally,
}
impl Session {
    pub fn new(sid: &str) -> Self {
        Self {
            id: sid.into(),
            path: None,
            tail: super::tail::Tail::default(),
            dirty: true,
            looked: 0.,
            main_id: None,
            measuring: None,
            replies: BTreeMap::new(),
            tally: super::breakdown::Tally::default(),
            file_order: vec![],
            readings: vec![],
            reading_waiting: false,
            track_readings: false,
            data: json!({
                "id": sid,
                "title": null,
                "main": [],
                "prompts": [],
                "compacts": [],
                "compact_sizes": [],
                "summary_sizes": [],
                "asked": [],
                "questions": [],
                "tools": {},
                "files": {},
                "edited": [],
                "edit_at": {},
                "shell": {},
                "last": {},
                "talk": [],
                "typed": [],
                "summaries": [],
                "started": 0.,
                "turn_end": 0.,
                "messages": 0,
                "model": null,
                "breakdown": null,
                "told": null,
                "last_session": null,
                "todos": [],
                "lost": [],
                "remembered": [],
            }),
        }
    }
    pub fn with_readings(sid: &str) -> Self {
        Self {
            track_readings: true,
            ..Self::new(sid)
        }
    }
    fn push(&mut self, key: &str, v: Value) {
        self.data[key].as_array_mut().unwrap().push(v);
    }
    fn locate(&mut self, paths: &Paths, now: f64) {
        if self.path.is_some() {
            return;
        }
        if let Some(record) = session_record(paths, &self.id) {
            let guess = paths
                .project_folder(record["cwd"].as_str().unwrap_or(""))
                .join(format!("{}.jsonl", self.id));
            if guess.is_file() {
                self.path = Some(guess);
                return;
            }
        }
        if now - self.looked >= 10. {
            self.looked = now;
            self.path = paths.transcript(&self.id);
        }
    }
    pub fn read_transcript(&mut self, paths: &Paths) {
        self.locate(paths, super::paths::now());
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let path = path.clone();
        let mut tail = std::mem::take(&mut self.tail);
        let _ = tail.poll(
            &path,
            usize::MAX,
            || false,
            |raw| {
                let Some(raw) = raw else {
                    let path = self.path.clone();
                    let tracking = self.track_readings;
                    *self = Self::new(&self.id);
                    self.path = path;
                    self.track_readings = tracking;
                    return;
                };
                self.read_line(raw);
            },
        );
        self.tail = tail;
        let talk = self.data["talk"].as_array_mut().unwrap();
        if talk.len() > 200 {
            talk.drain(..talk.len() - 200);
        }
    }
    fn read_line(&mut self, raw: &[u8]) {
        self.dirty = true;
        let Ok(e) = sonic_rs::from_slice::<Value>(raw) else {
            return;
        };
        let kind = e["type"].as_str().unwrap_or("");
        let when = e["timestamp"].as_str().and_then(epoch);
        if self.data["started"] == 0. {
            if let Some(t) = when {
                self.data["started"] = json!(t);
            }
        }
        if matches!(kind, "human" | "user" | "assistant") {
            self.data["messages"] = json!(self.data["messages"].as_u64().unwrap_or(0) + 1);
        }
        let side = e["isSidechain"] == true;
        if kind == "ai-title" && e["aiTitle"].is_string() {
            self.data["title"] = e["aiTitle"].clone();
        }
        if self.track_readings {
            self.read_readings(raw, &e, when.unwrap_or(0.));
        }
        let Some(t) = when else {
            if kind == "ai-title" && e["aiTitle"].is_string() {
                self.data["title"] = e["aiTitle"].clone();
            }
            return;
        };
        let m = &e["message"];
        if !side {
            // What the context holds, row by row (see `breakdown`).
            self.tally.row(&e);
        }
        if kind == "assistant" {
            let context = [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ]
            .iter()
            .map(|k| m["usage"][k].as_u64().unwrap_or(0))
            .sum::<u64>();
            let id = m["id"].as_str();
            if let Some(id) = id {
                self.replies.insert(
                    id.to_owned(),
                    json!([
                        t,
                        context,
                        m["usage"]["output_tokens"].as_u64().unwrap_or(0),
                        m["usage"]["cache_creation_input_tokens"]
                            .as_u64()
                            .unwrap_or(0),
                        m["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0),
                    ]),
                );
            }
            if m["stop_reason"] == "end_turn" && !side {
                self.data["turn_end"] = json!(t);
            }
            if context > 0 && !side {
                if id.is_some() && id == self.main_id.as_deref() {
                    *self.data["main"]
                        .as_array_mut()
                        .unwrap()
                        .last_mut()
                        .unwrap() = json!([t, context]);
                } else {
                    self.push("main", json!([t, context]));
                }
                self.main_id = id.map(str::to_owned);
                self.data["breakdown"] = self.tally.reply(context as f64);
                if let Some(model) = m["model"]
                    .as_str()
                    .filter(|s| !s.is_empty() && !s.starts_with('<'))
                {
                    self.data["model"] = json!(model);
                }
                if let Some(i) = self.measuring.take() {
                    self.data["compact_sizes"][i][1] = json!(context);
                }
            }
            for part in m["content"].as_array().into_iter().flatten() {
                if part["type"] == "text" && !side {
                    let text = part["text"].as_str().unwrap_or("");
                    self.push("talk", json!([t, text]));
                    self.data["said"] = json!([t, text]);
                }
                if part["type"] != "tool_use" {
                    continue;
                }
                let name = part["name"].as_str().unwrap_or("");
                let count = self.data["tools"][name].as_u64().unwrap_or(0) + 1;
                self.data["tools"][name] = json!(count);
                if let Some(path) = part["input"]["file_path"]
                    .as_str()
                    .filter(|p| p.starts_with('/'))
                {
                    let path = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
                    let path = path.to_string_lossy();
                    if !self.file_order.iter().any(|p| p == path.as_ref()) {
                        self.file_order.push(path.to_string());
                    }
                    let count = self.data["files"][path.as_ref()].as_u64().unwrap_or(0) + 1;
                    self.data["files"][path.as_ref()] = json!(count);
                    self.data["last"][path.as_ref()] = json!(t);
                    let edits = matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit");
                    if edits {
                        // `last` also moves on reads. Only an edit dates a timeline row.
                        self.data["edit_at"][path.as_ref()] = json!(t);
                    }
                    if edits
                        && !self.data["edited"]
                            .as_array()
                            .unwrap()
                            .contains(&json!(path))
                    {
                        self.push("edited", json!(path));
                    }
                }
                if name == "Bash" {
                    static NAMED: std::sync::LazyLock<regex::Regex> =
                        std::sync::LazyLock::new(|| {
                            regex::Regex::new(r"(/(?:Users|home)/[\w./@+-]+)").unwrap()
                        });
                    let command = part["input"]["command"].as_str().unwrap_or("");
                    let names: std::collections::BTreeSet<_> = NAMED
                        .find_iter(command)
                        .map(|m| m.as_str().trim_end_matches(['.', ',', ';', ':']))
                        .collect();
                    for named in names {
                        let count = self.data["shell"][named].as_u64().unwrap_or(0) + 1;
                        self.data["shell"][named] = json!(count);
                        self.data["last"][named] = json!(t);
                    }
                }
                if let Some(tool) = name.strip_prefix("mcp__claude-self-reflect__") {
                    self.push("asked", json!(t));
                    let args = &part["input"];
                    let ask = [
                        "query",
                        "concept",
                        "topic",
                        "file_path",
                        "symbol",
                        "target",
                        "pattern",
                        "conversation_id",
                    ]
                    .iter()
                    .find_map(|k| args[k].as_str().filter(|s| !s.trim().is_empty()))
                    .unwrap_or("");
                    self.push("questions", json!([t, tool, ask]));
                }
            }
        } else if kind == "user" && e["isCompactSummary"] == true {
            let text = message_text(m);
            if !text.trim().is_empty() {
                self.push("summaries", json!([t, text]));
            }
        } else if kind == "user" && e["isMeta"] != true {
            let text = message_text(m);
            let content = &m["content"];
            let parts: Vec<_> = if let Some(text) = content.as_str() {
                vec![text]
            } else {
                content
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect()
            };
            if parts
                .iter()
                .any(|text| !text.trim().is_empty() && !text.trim_start().starts_with(['<', '/']))
            {
                self.push("prompts", json!(t));
                if !side {
                    self.tally.prompt();
                }
                if !side
                    && !text
                        .trim_start()
                        .starts_with("[Request interrupted by user")
                {
                    let own = super::send::own_words(&text);
                    if !own.trim().is_empty() {
                        self.push("talk", json!([t, own]));
                        self.push("typed", json!([t, own]));
                    }
                }
            }
        } else if kind == "attachment" {
            let a = &e["attachment"];
            let typed = a["prompt"]
                .as_str()
                .map(|s| {
                    let s = s.trim();
                    !s.is_empty() && !s.starts_with(['<', '/'])
                })
                .unwrap_or_else(|| super::truthy(&a["prompt"]));
            if a["type"] == "queued_command" && super::truthy(&a["humanTurn"]) && typed {
                self.push("prompts", json!(t));
                self.tally.prompt();
            }
            if a["type"] == "hook_success"
                && a["hookEvent"] == "SessionStart"
                && a["command"]
                    .as_str()
                    .is_some_and(|s| s.contains("csr-engine") && s.ends_with("hook session-start"))
                && a["content"].is_string()
            {
                let source = a["hookName"]
                    .as_str()
                    .and_then(|s| s.split_once(':'))
                    .map(|(_, s)| s)
                    .unwrap_or("startup");
                self.data["told"] = json!({
                    "at": t,
                    "source": source,
                    "text": a["content"],
                });
            }
        } else if kind == "system" && e["subtype"] == "compact_boundary" {
            self.tally.compacted();
            self.data["breakdown"] = Value::Null;
            self.push("compacts", json!(t));
            let meta = &e["compactMetadata"];
            self.push(
                "compact_sizes",
                json!([meta["preTokens"], null, meta["durationMs"]]),
            );
            self.push("summary_sizes", meta["postTokens"].clone());
            self.measuring = Some(self.data["compact_sizes"].as_array().unwrap().len() - 1);
        }
    }

    fn read_readings(&mut self, raw: &[u8], e: &Value, when: f64) {
        let has = |s: &[u8]| memchr::memmem::find(raw, s).is_some();
        if !(has(b"\"isCompactSummary\":true")
            || (has(b"\"type\":\"user\"") && !has(b"\"tool_use_id\""))
            || (self.reading_waiting && has(b"\"type\":\"assistant\"")))
        {
            return;
        }
        if !e["message"].is_object() {
            return;
        }
        let kind = e["type"].as_str().unwrap_or("");
        let text = message_text(&e["message"]);
        if kind == "user" && e["isCompactSummary"] == true {
            for line in text.lines() {
                if let Some(mut row) = super::coach::reading_in(line, false) {
                    if !self.readings.iter().any(|r| r.line == row.line) {
                        row.when = when;
                        row.source = "compaction".into();
                        self.readings.push(row);
                    }
                }
            }
        } else if kind == "user" && e["isMeta"] != true && e["isSidechain"] != true {
            self.reading_waiting =
                !text.trim().is_empty() && !text.trim_start().starts_with(['<', '/']);
        } else if kind == "assistant" && e["isSidechain"] != true && !text.trim().is_empty() {
            self.reading_waiting = false;
            let first = super::coach::sentences(text.trim())
                .into_iter()
                .next()
                .unwrap_or("");
            if let Some(mut row) = super::coach::reading_in(first, true) {
                row.when = when;
                row.source = "reply".into();
                self.readings.push(row);
            }
        }
        self.readings
            .drain(..self.readings.len().saturating_sub(20));
    }
    pub fn load(&mut self, paths: &Paths, db: Option<&Db>) -> &mut Value {
        self.read_transcript(paths);
        let s = &mut self.data;
        if self.dirty {
            let mut replies: Vec<_> = self.replies.values().cloned().collect();
            replies.sort_by(|a, b| {
                a[0].as_f64()
                    .partial_cmp(&b[0].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            s["replies"] = json!(replies);
            self.dirty = false;
        }
        let record = session_record(paths, &self.id);
        if s["started"] == 0. {
            if let Some(record) = &record {
                s["started"] = record["started"].clone();
            }
        }
        s["busy"] = json!(record.as_ref().is_some_and(|r| r["busy"] == true));
        s["cwd"] = record
            .as_ref()
            .map(|r| r["cwd"].clone())
            .unwrap_or(Value::Null);
        s["transcript"] = json!(self.path.is_some());
        if let Some(db) = db {
            let sid = [json!(self.id)];
            let times = |sql| {
                db.query(sql, &sid)
                    .iter()
                    .filter_map(|r| r[0].as_str().and_then(epoch))
                    .collect::<Vec<_>>()
            };
            s["learned"] = json!({
                "chunks": times(
                    "select created_at from chunks where conversation_id=?",
                ),
                "anchors": db.query(
                    "select created_at,name,file from episode_anchors where \
                        session_id=? order by created_at",
                    &sid,
                ),
                "symbols": db.scalar(
                    "select count(*) from code_nodes where last_session_id=?",
                    &sid,
                ),
                "turns": db.query(
                    "select ts,kind,quote from intent_events where session_id=? \
                        order by ts",
                    &sid,
                ),
                "memories": db.query(
                    "select slug from memory_registry where origin_session_id=?",
                    &sid,
                ),
            });
            s["gave"] = json!({
                "dreams": times(
                    "select delivered_at from dream_deliveries where session_id=?",
                ),
                "recalls": times(
                    "select retrieved_at from retrieval_events where session_id=?",
                ),
                "carried": times("select delivered_at from correction_deliveries where \
                    target_session_id=?"),
            });
            s["project"] = db.scalar(
                "select project from session_registry where session_id=?",
                &sid,
            );
            s["sessions"] = db.scalar("select count(*) from session_registry", &[]);
            s["remembered"] = json!(super::recall::remembered_items(db, s));
        }
        s
    }
}
pub fn message_text(message: &Value) -> String {
    if let Some(s) = message["content"].as_str() {
        return s.to_owned();
    }
    message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["type"] == "text")
        .filter_map(|v| v["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct Clock {
    pub starts: Vec<f64>,
    pub offsets: Vec<f64>,
    pub breaks: Vec<f64>,
    pub total: f64,
    pub begin: f64,
}
impl Clock {
    pub fn new(stamps: &[f64], now: f64) -> Self {
        let mut stamps: Vec<_> = stamps
            .iter()
            .copied()
            .filter(|t| *t != 0. && t.is_finite())
            .collect();
        stamps.sort_by(f64::total_cmp);
        let mut c = Self {
            starts: vec![],
            offsets: vec![],
            breaks: vec![],
            total: 0.,
            begin: stamps.first().copied().unwrap_or(now),
        };
        let mut last = None;
        for t in stamps.into_iter().chain([now]) {
            if last.is_none_or(|v| t - v > 900.) {
                if last.is_some() {
                    c.total += 60.;
                    c.breaks.push(c.total);
                }
                c.starts.push(t);
                c.offsets.push(c.total);
            } else {
                c.total += t - last.unwrap();
            }
            last = Some(t);
        }
        c.total = c.total.max(1.);
        c
    }
    pub fn at(&self, t: f64) -> f64 {
        let i = self.starts.partition_point(|v| *v <= t);
        if i == 0 {
            return 0.;
        }
        let i = i - 1;
        let next = self
            .offsets
            .get(i + 1)
            .map(|v| v - 60.)
            .unwrap_or(self.total);
        (self.offsets[i] + t - self.starts[i]).min(next) / self.total
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_partial_lines_wait_for_the_newline() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let f = p.projects.join("p/s.jsonl");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        let line = concat!(
            r#"{"type":"assistant","timestamp":"2026-10-08T10:0"#,
            r#"0:00Z","message":{"id":"m","usage":{"input_token"#,
            r#"s":100}}}"#
        );
        std::fs::write(&f, line).unwrap();
        let mut s = Session::new("s");
        s.read_transcript(&p);
        assert_eq!(s.tail.offset, line.len() as u64);
        use std::io::Write;
        writeln!(std::fs::OpenOptions::new().append(true).open(&f).unwrap()).unwrap();
        s.read_transcript(&p);
        assert_eq!(s.data["main"].as_array().unwrap().len(), 1);
        s.read_transcript(&p);
        assert_eq!(s.data["main"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn test_continued_in_lineage() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let f = p.projects.join("p");
        std::fs::create_dir_all(&f).unwrap();
        std::fs::write(f.join("new.jsonl"), "{}\n").unwrap();
        std::fs::write(
            f.join("old.jsonl"),
            "{\"type\":\"continued-in\",\"sessionId\":\"old\",\"continuedInSessionId\":\"new\"}\n",
        )
        .unwrap();
        assert_eq!(lineage(&p, "new"), vec!["new", "old"]);
    }
    #[test]
    fn test_idle_gaps_are_breaks_not_time() {
        let c = Clock::new(&[100., 160., 4000., 4060.], 4120.);
        assert_eq!(c.breaks, vec![120.]);
        assert_eq!(c.total, 240.);
        assert_eq!(c.at(160.), 0.25);
        assert_eq!(c.at(3000.), 0.25);
    }
}

#[cfg(test)]
mod locating_tests {
    use super::*;
    #[test]
    fn test_without_a_record_the_folders_are_searched_at_most_every_ten_seconds() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let mut s = Session::new("s");
        s.locate(&paths, 1000.);
        let p = paths.projects.join("p/s.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{}\n").unwrap();
        s.locate(&paths, 1001.);
        assert!(s.path.is_none());
        s.locate(&paths, 1010.);
        assert_eq!(s.path, Some(p));
    }
    #[test]
    fn test_a_live_sessions_record_gives_its_cwd_start_and_whether_it_is_busy() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let dir = paths.home.join(".claude/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("mine.json");
        std::fs::write(
            &p,
            json!({
                "sessionId": "s",
                "cwd": "/fixture/repo",
                "startedAt": 1000000,
                "status": "busy",
                "pid": std::process::id(),
            })
            .to_string(),
        )
        .unwrap();
        let record = session_record(&paths, "s").unwrap();
        assert_eq!(record["cwd"], "/fixture/repo");
        assert_eq!(record["started"], 1000.);
        assert_eq!(record["busy"], true);
        assert!(session_record(&paths, "someone-else").is_none());
    }
    #[test]
    fn test_a_pane_opened_before_the_first_prompt_reads_the_transcript_once_it_exists() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let dir = paths.home.join(".claude/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mine.json"),
            json!({
                "sessionId": "s",
                "cwd": "/fixture/repo",
                "startedAt": 1000000,
                "status": "idle",
                "pid": std::process::id(),
            })
            .to_string(),
        )
        .unwrap();
        let mut s = Session::new("s");
        s.locate(&paths, 1000.);
        assert!(s.path.is_none());
        let p = paths.project_folder("/fixture/repo").join("s.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{}\n").unwrap();
        s.locate(&paths, 1001.);
        assert_eq!(s.path, Some(p));
    }
}

pub fn left_todos(paths: &Paths, sid: &str, db: Option<&Db>) -> Vec<Value> {
    if !valid_id(sid) {
        return vec![];
    }
    let mut found = vec![];
    for path in folders(&paths.home.join(".claude/tasks").join(sid)) {
        if path.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let Some(n) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        let task = read_json(&path);
        if matches!(task["status"].as_str(), Some("in_progress" | "pending")) {
            if let Some(subject) = task["subject"].as_str().filter(|s| !s.trim().is_empty()) {
                found.push((
                    n,
                    json!({
                        "subject": super::render::text::one_line(subject, 300),
                        "status": task["status"],
                    }),
                ));
            }
        }
    }
    if found.is_empty() {
        if let Some(db) = db {
            let raw = db.scalar(
                "select content from reflections where tags like \
                    '%session_episode%' and tags like ? order by timestamp desc \
                    limit 1",
                &[json!(format!("%conv_{sid}%"))],
            );
            if let Ok(value) = serde_json::from_str::<Value>(raw.as_str().unwrap_or("")) {
                for (i, task) in super::array(&value["todos"]).iter().enumerate() {
                    if matches!(task["status"].as_str(), Some("in_progress" | "pending")) {
                        if let Some(content) =
                            task["content"].as_str().filter(|s| !s.trim().is_empty())
                        {
                            found.push((
                                i as u64,
                                json!({
                                    "subject": super::render::text::one_line(
                                        content, 300,
                                    ),
                                    "status": task["status"],
                                }),
                            ));
                        }
                    }
                }
            }
        }
    }
    found.sort_by_key(|(n, t)| (t["status"] != "in_progress", *n));
    found.into_iter().map(|(_, task)| task).collect()
}
pub fn thin_series(points: &[Value], cuts: &[f64], limit: usize) -> Vec<Value> {
    if points.len() <= limit || limit == 0 {
        return points.to_vec();
    }
    let step = points.len().div_ceil(limit);
    let mut keep: std::collections::BTreeSet<_> = (0..points.len()).step_by(step).collect();
    let mut cuts = cuts.to_vec();
    cuts.sort_by(f64::total_cmp);
    let mut edges = vec![0];
    edges.extend(
        cuts.iter()
            .map(|c| points.partition_point(|p| super::num(&p[0]) < *c)),
    );
    edges.push(points.len());
    for w in edges.windows(2) {
        if w[1] > w[0] {
            keep.insert(w[0]);
            keep.insert(w[1] - 1);
        }
    }
    keep.into_iter().map(|i| points[i].clone()).collect()
}
pub fn previous_facts(paths: &Paths, prev: &Session, db: Option<&Db>) -> Value {
    use super::{array, num};
    let p = &prev.data;
    let mut main = array(&p["main"]).to_vec();
    main.sort_by(|a, b| {
        num(&a[0])
            .total_cmp(&num(&b[0]))
            .then(num(&a[1]).total_cmp(&num(&b[1])))
    });
    let prompts = array(&p["prompts"]);
    let compacts = array(&p["compacts"]);
    let times: Vec<_> = main
        .iter()
        .map(|r| num(&r[0]))
        .chain(prompts.iter().map(num))
        .collect();
    let last = times
        .iter()
        .copied()
        .chain(compacts.iter().map(num))
        .chain([
            num(&p["said"][0]),
            array(&p["typed"]).last().map(|r| num(&r[0])).unwrap_or(0.),
        ])
        .fold(0., f64::max);
    let (floor, measured) = super::masthead::floor_of(&p["compact_sizes"]);
    let (learned, handed) = db
        .map(|db| {
            let sid = [json!(prev.id)];
            let recalls = db.query(
                "select retrieved_at from retrieval_events where session_id=?",
                &sid,
            );
            let seconds: std::collections::BTreeSet<_> = recalls
                .iter()
                .filter_map(|r| r[0].as_str().and_then(epoch))
                .filter(|t| *t != 0.)
                .map(|t| t as i64)
                .collect();
            (
                db.scalar("select count(*) from chunks where conversation_id=?", &sid),
                json!(
                    seconds.len()
                        + db.scalar(
                            "select count(*) from correction_deliveries where target_session_id=?",
                            &sid,
                        )
                        .as_u64()
                        .unwrap_or(0) as usize
                ),
            )
        })
        .unwrap_or((Value::Null, Value::Null));
    let cuts: Vec<_> = compacts.iter().map(num).collect();
    let series = thin_series(&main, &cuts, 2000);
    let cut = |pair: &Value| {
        if let (Some(at), Some(text)) = (pair[0].as_f64(), pair[1].as_str()) {
            json!([at, super::render::text::one_line(text, 400)])
        } else {
            Value::Null
        }
    };
    json!({
        "id": prev.id,
        "path": prev.path,
        "mtime": prev
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|t| t.as_secs_f64())
            .unwrap_or(0.),
        "title": p["title"],
        "first": p["started"],
        "last": last,
        "active": Clock::new(&times, last).total,
        "prompts": prompts.len(),
        "compacts": compacts.len(),
        "peak": main.iter().map(|r| num(&r[1])).fold(0., f64::max),
        "floor": if measured {
            json!(floor)
        } else {
            Value::Null
        },
        "learned": learned,
        "handed": handed,
        "asked": array(&p["asked"]).len(),
        "series": series,
        "prompt_times": prompts,
        "compactions": compacts
            .iter()
            .zip(array(&p["compact_sizes"]))
            .map(|(t, size)| json!([t, size[0], size[1]]))
            .collect::<Vec<_>>(),
        "todos": left_todos(paths, &prev.id, db),
        "said": cut(&p["said"]),
        "typed": cut(array(&p["typed"]).last().unwrap_or(&Value::Null)),
    })
}

#[cfg(test)]
mod previous_tests {
    use super::*;
    #[test]
    fn test_the_todos_a_session_left_open_come_in_progress_first_then_by_number() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let folder = paths.home.join(".claude/tasks/s");
        std::fs::create_dir_all(&folder).unwrap();
        for (n, status, subject) in [
            (1, "pending", "later"),
            (2, "in_progress", "working"),
            (3, "completed", "done"),
            (4, "pending", ""),
        ] {
            std::fs::write(
                folder.join(format!("{n}.json")),
                json!({
                    "status": status,
                    "subject": subject,
                })
                .to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            left_todos(&paths, "s", None),
            vec![
                json!({
                    "subject": "working",
                    "status": "in_progress"
                }),
                json!({
                    "subject": "later",
                    "status": "pending"
                })
            ]
        );
        assert!(left_todos(&paths, "../s", None).is_empty());
    }
    #[test]
    fn test_a_long_series_is_thinned_but_keeps_the_ends_of_every_stretch() {
        let points = serde_json::json!([
            [10, 1],
            [20, 2],
            [30, 3],
            [40, 4],
            [50, 5],
            [60, 6],
            [70, 7],
        ]);
        let points = points.as_array().unwrap();
        let kept = thin_series(points, &[25., 45.], 3);
        for i in [0, 1, 2, 3, 4, 6] {
            assert!(kept.contains(&points[i]));
        }
        assert!(kept.len() < points.len());
        assert!(kept.len() <= 3 + 2 * 3);
        assert_eq!(thin_series(points, &[25., 45.], 100), *points);
    }
    #[test]
    fn test_the_facts_about_the_last_session_are_counted_once() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let mut prev = Session::new("old");
        prev.data["started"] = json!(1000.);
        prev.data["title"] = json!("Build the thing");
        prev.data["main"] = json!([[1010, 100000], [1100, 120000], [1200, 60000], [1410, 70000],]);
        prev.data["prompts"] = json!([1000, 1100, 1200, 1300, 1400]);
        prev.data["compacts"] = json!([1150, 1350]);
        prev.data["compact_sizes"] = json!([[100000, 60000, 2000], [120000, 70000, 3000]]);
        prev.data["said"] = json!([1410, "words"]);
        prev.data["typed"] = json!([[1400, "the last words I typed"]]);
        let facts = previous_facts(&paths, &prev, None);
        assert_eq!(facts["id"], "old");
        assert_eq!(facts["prompts"], 5);
        assert_eq!(facts["compacts"], 2);
        assert_eq!(facts["peak"], 120000.);
        assert_eq!(facts["floor"], 65000.);
        assert_eq!(facts["active"], 410.);
        assert_eq!(facts["last"], 1410.);
        assert!(facts["learned"].is_null());
        assert!(facts["handed"].is_null());
        assert_eq!(facts["typed"][1], "the last words I typed");
        assert_eq!(facts["said"][1], "words");
    }
}

#[cfg(test)]
mod reader_regressions {
    use super::*;
    fn row(text: &str) -> String {
        json!({"type":"user","timestamp":"2026-10-18T10:00:00Z",
            "message":{"content":text}})
        .to_string()
            + "\n"
    }
    fn jsonl(rows: &[Value]) -> String {
        rows.iter().map(|r| r.to_string() + "\n").collect()
    }
    fn at(sec: u32) -> String {
        format!("2026-10-18T10:00:{sec:02}Z")
    }
    fn reply(id: &str, sec: u32, total: u64) -> Value {
        json!({"type":"assistant","timestamp":at(sec),
            "message":{"id":id,"model":"m","content":[{"type":"text","text":"ok"}],
                "usage":{"input_tokens":1,"cache_read_input_tokens":total-1,"output_tokens":5}}})
    }
    #[test]
    fn the_context_breakdown_follows_the_replies_and_ends_with_a_compaction() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let path = paths.projects.join("p/s.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let prompt = |sec| json!({"type":"user","timestamp":at(sec),"message":{"content":"go on"}});
        let csr = json!({"type":"attachment","timestamp":at(3),"attachment":{"type":"hook_success",
            "hookName":"UserPromptSubmit","command":"/usr/local/bin/csr-engine hook prompt-submit",
            "content":"c".repeat(1100)}});
        let result = json!({"type":"user","timestamp":at(4),"message":{"content":[
            {"type":"tool_result","tool_use_id":"t","content":"r".repeat(6000)}]}});
        let side = json!({"type":"user","isSidechain":true,"timestamp":at(5),"message":{"content":[
            {"type":"tool_result","tool_use_id":"u","content":"s".repeat(90000)}]}});
        std::fs::write(
            &path,
            jsonl(&[
                prompt(1),
                reply("a", 2, 30_000),
                prompt(2),
                csr,
                result,
                side,
                reply("b", 6, 39_000),
            ]),
        )
        .unwrap();
        let mut session = Session::new("s");
        let data = session.load(&paths, None).clone();
        let b = &data["breakdown"];
        assert_eq!(b["total"], json!(39_000.));
        assert_eq!(b["turn"]["from"], json!(30_000.));
        // Only the main thread counts: the side thread's 90k characters are nowhere.
        assert_eq!(b["chars"]["tool"], json!(6000));
        assert_eq!(b["chars"]["csr"], json!(1100));
        assert_eq!(b["turn"]["chars"]["csr"], json!(1100));
        assert!(b["floor"].as_f64().unwrap() > 20_000.);
        // A compaction ends the old context; the next reply starts a new floor.
        use std::io::Write;
        let boundary = json!({"type":"system","subtype":"compact_boundary","timestamp":at(7),
            "compactMetadata":{"preTokens":39000,"postTokens":5000,"durationMs":9}});
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(jsonl(&[boundary]).as_bytes())
            .unwrap();
        assert_eq!(session.load(&paths, None)["breakdown"], Value::Null);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(jsonl(&[reply("c", 8, 12_000)]).as_bytes())
            .unwrap();
        let data = session.load(&paths, None).clone();
        assert_eq!(data["breakdown"]["total"], json!(12_000.));
        assert_eq!(data["breakdown"]["turn"], Value::Null);
    }
    #[test]
    fn load_borrows_histories_and_partial_lines_cross_chunk_boundaries() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let path = paths.projects.join("p/s.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let long = row(&"x".repeat(super::super::tail::CHUNK + 17));
        std::fs::write(&path, &long[..long.len() - 1]).unwrap();
        let mut session = Session::new("s");
        session.read_transcript(&paths);
        assert_eq!(session.tail.offset, (long.len() - 1) as u64);
        assert_eq!(session.data["messages"], 0);
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        session.load(&paths, None);
        assert_eq!(session.data["messages"], 1);
        let history = session.data["typed"].as_array().unwrap().as_ptr();
        let loaded = session.load(&paths, None);
        assert_eq!(loaded["typed"].as_array().unwrap().as_ptr(), history);
    }
    #[test]
    fn replacement_and_truncation_reset_all_transcript_aggregates() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let path = paths.projects.join("p/s.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, row("old")).unwrap();
        let mut session = Session::new("s");
        session.read_transcript(&paths);
        let other = path.with_extension("new");
        std::fs::write(&other, row("new and longer")).unwrap();
        std::fs::rename(other, &path).unwrap();
        session.read_transcript(&paths);
        assert_eq!(session.data["messages"], 1);
        assert_eq!(session.data["typed"][0][1], "new and longer");
        std::fs::write(&path, "").unwrap();
        session.read_transcript(&paths);
        assert_eq!(session.data["messages"], 0);
        assert!(session.data["typed"].as_array().unwrap().is_empty());
    }
    #[test]
    fn duplicate_predecessor_ids_never_cross_project_boundaries() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        for name in ["a", "b"] {
            let folder = paths.projects.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(
                folder.join("old.jsonl"),
                concat!(
                    "{\"type\":\"continued-in\",\"sessionId\":\"old\",",
                    "\"continuedInSessionId\":\"new\"}\n"
                ),
            )
            .unwrap();
        }
        std::fs::write(paths.projects.join("b/new.jsonl"), "{}\n").unwrap();
        assert_eq!(lineage(&paths, "new"), ["new"]);
        assert!(paths.transcript("old").is_none());
    }
}

#[cfg(test)]
mod typed_regressions {
    use super::*;
    #[test]
    fn agent_envelope_only_messages_never_enter_typed_history() {
        let mut session = Session::new("s");
        let envelope = concat!(
            "[[CSR:AGENTS:v1]]\n",
            "{\"kind\":\"context\",\"session\":\"s\",\"at\":1,\"agents\":[]}\n",
            "[[/CSR:AGENTS:v1]]\n"
        );
        let row = json!({"type":"user","timestamp":"2026-10-18T00:00:00Z",
            "message":{"content":envelope}});
        session.read_line(row.to_string().as_bytes());
        assert!(session.data["typed"].as_array().unwrap().is_empty());
    }
    #[test]
    fn queued_commands_with_images_still_count_as_prompts() {
        let mut s = Session::new("s");
        s.read_line(
            json!({"type":"attachment","timestamp":"2026-10-18T00:00:00Z",
            "attachment":{"type":"queued_command","humanTurn":true,"prompt":[
                {"type":"text","text":"Check this image"},
                {"type":"image","source":{"type":"base64","data":"x"}}
            ]}})
            .to_string()
            .as_bytes(),
        );
        assert_eq!(super::super::array(&s.data["prompts"]).len(), 1);
    }
}
