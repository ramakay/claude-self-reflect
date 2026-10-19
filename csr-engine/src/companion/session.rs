use super::{
    db::Db,
    paths::{epoch, folders, read_json, valid_id, Paths},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

pub fn session_record(paths: &Paths, sid: &str) -> Option<Value> {
    if !valid_id(sid) {
        return None;
    }
    folders(&paths.home.join(".claude/sessions")).iter().find_map(|p| {
        let v=read_json(p);
        if v["sessionId"]!=sid || v["cwd"].as_str().is_none_or(str::is_empty) {return None;}
        if let Some(pid)=v["pid"].as_i64() {
            let pid=i32::try_from(pid).ok()?;
            if pid<=1 {return None;}
            // kill(0) probes liveness, including a process owned by someone else.
            if unsafe {libc::kill(pid,0)}!=0 && std::io::Error::last_os_error().raw_os_error()!=Some(libc::EPERM) {return None;}
        }
        Some(json!({"cwd":v["cwd"],"started":v["startedAt"].as_f64()?/1000.,"busy":v["status"]=="busy"}))
    })
}

pub fn lineage(paths: &Paths, sid: &str) -> Vec<String> {
    let mut ids = vec![sid.to_owned()];
    if std::env::var("CSR_NO_CONTINUED")
        .is_ok_and(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true"))
    {
        return ids;
    }
    while ids.len() <= 5 {
        let Some(path) = paths.transcript(ids.last().unwrap()) else {
            break;
        };
        let Some(old) = crate::hooks::resume_lineage::resume_predecessor(
            path.parent().unwrap(),
            ids.last().unwrap(),
        ) else {
            break;
        };
        if !valid_id(&old) || ids.contains(&old) {
            break;
        }
        ids.push(old);
    }
    ids
}

pub struct Session {
    pub id: String,
    pub path: Option<PathBuf>,
    offset: u64,
    main_id: Option<String>,
    measuring: Option<usize>,
    pub data: Value,
    replies: BTreeMap<String, Value>,
}
impl Session {
    pub fn new(sid: &str) -> Self {
        Self {
            id: sid.into(),
            path: None,
            offset: 0,
            main_id: None,
            measuring: None,
            replies: BTreeMap::new(),
            data: json!({"id":sid,"title":null,"main":[],"prompts":[],"compacts":[],"compact_sizes":[],"summary_sizes":[],"asked":[],"questions":[],"tools":{},"files":{},"edited":[],"shell":{},"last":{},"talk":[],"typed":[],"summaries":[],"started":0.,"turn_end":0.,"messages":0,"model":null,"told":null,"last_session":null,"todos":[],"lost":[],"remembered":[]}),
        }
    }
    fn push(&mut self, key: &str, v: Value) {
        self.data[key].as_array_mut().unwrap().push(v);
    }
    pub fn read_transcript(&mut self, paths: &Paths) {
        if self.path.is_none() {
            self.path = paths.transcript(&self.id);
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(mut file) = File::open(path) else {
            return;
        };
        if file.metadata().is_ok_and(|m| m.len() < self.offset) {
            let path = self.path.clone();
            *self = Self::new(&self.id);
            self.path = path;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return;
        }
        let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
            return;
        };
        self.offset += (end + 1) as u64;
        for raw in bytes[..=end].split(|b| *b == b'\n') {
            let Ok(e) = sonic_rs::from_slice::<Value>(raw) else {
                continue;
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
            let Some(t) = when else {
                if kind == "ai-title" && e["aiTitle"].is_string() {
                    self.data["title"] = e["aiTitle"].clone();
                }
                continue;
            };
            let m = &e["message"];
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
                                .unwrap_or(0)
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
                        let path =
                            std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
                        let path = path.to_string_lossy();
                        let count = self.data["files"][path.as_ref()].as_u64().unwrap_or(0) + 1;
                        self.data["files"][path.as_ref()] = json!(count);
                        self.data["last"][path.as_ref()] = json!(t);
                        if matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
                            && !self.data["edited"]
                                .as_array()
                                .unwrap()
                                .contains(&json!(path))
                        {
                            self.push("edited", json!(path));
                        }
                    }
                    if let Some(tool) = name.strip_prefix("mcp__claude-self-reflect__") {
                        self.push("asked", json!(t));
                        let args = &part["input"];
                        let ask = [
                            "query",
                            "concept",
                            "file_path",
                            "session_id",
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
                if text
                    .lines()
                    .any(|s| !s.trim().is_empty() && !s.trim_start().starts_with(['<', '/']))
                {
                    self.push("prompts", json!(t));
                    if !side
                        && !text
                            .trim_start()
                            .starts_with("[Request interrupted by user")
                    {
                        self.push("talk", json!([t, text]));
                        self.push("typed", json!([t, text]));
                    }
                }
            } else if kind == "attachment" {
                let a = &e["attachment"];
                let text = a["prompt"].as_str().unwrap_or("").trim();
                if a["type"] == "queued_command"
                    && a["humanTurn"] == true
                    && !text.is_empty()
                    && !text.starts_with(['<', '/'])
                {
                    self.push("prompts", json!(t));
                }
                if a["type"] == "hook_success"
                    && a["hookEvent"] == "SessionStart"
                    && a["command"].as_str().is_some_and(|s| {
                        s.contains("csr-engine") && s.ends_with("hook session-start")
                    })
                    && a["content"].is_string()
                {
                    let source = a["hookName"]
                        .as_str()
                        .and_then(|s| s.split_once(':'))
                        .map(|(_, s)| s)
                        .unwrap_or("startup");
                    self.data["told"] = json!({"at":t,"source":source,"text":a["content"]});
                }
            } else if kind == "system" && e["subtype"] == "compact_boundary" {
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
        let talk = self.data["talk"].as_array_mut().unwrap();
        if talk.len() > 200 {
            talk.drain(..talk.len() - 200);
        }
    }
    pub fn load(&mut self, paths: &Paths, db: Option<&Db>) -> Value {
        self.read_transcript(paths);
        let mut s = self.data.clone();
        let mut replies: Vec<_> = self.replies.values().cloned().collect();
        replies.sort_by(|a, b| {
            a[0].as_f64()
                .partial_cmp(&b[0].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        s["replies"] = json!(replies);
        let record = session_record(paths, &self.id);
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
            s["learned"] = json!({"chunks":times("select created_at from chunks where conversation_id=?"),"anchors":db.query("select created_at,name,file from episode_anchors where session_id=? order by created_at",&sid),"symbols":db.scalar("select count(*) from code_nodes where last_session_id=?",&sid),"turns":db.query("select ts,kind,quote from intent_events where session_id=? order by ts",&sid),"memories":db.query("select slug from memory_registry where origin_session_id=?",&sid)});
            s["gave"] = json!({"dreams":times("select delivered_at from dream_deliveries where session_id=?"),"recalls":times("select retrieved_at from retrieval_events where session_id=?"),"carried":times("select delivered_at from correction_deliveries where target_session_id=?")});
            s["project"] = db.scalar(
                "select project from session_registry where session_id=?",
                &sid,
            );
            s["sessions"] = db.scalar("select count(*) from session_registry", &[]);
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
        let line = r#"{"type":"assistant","timestamp":"2026-10-08T10:00:00Z","message":{"id":"m","usage":{"input_tokens":100}}}"#;
        std::fs::write(&f, line).unwrap();
        let mut s = Session::new("s");
        s.read_transcript(&p);
        assert_eq!(s.offset, 0);
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
