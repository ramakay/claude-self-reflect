use super::{
    agents, array, coach,
    db::Db,
    dreams, note, num,
    paths::{self, Paths},
    recall, send,
    session::Session,
    string,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::LazyLock,
};

#[derive(Clone, Debug)]
pub struct Past {
    pub at: f64,
    pub session: String,
    pub project: String,
    pub text: String,
    pub score: f64,
}
const VOICE: &[(&str, &str)] = &[
    ("can we", r"\bcan we\b"),
    ("what if", r"\bwhat if\b"),
    ("should we", r"\bshould we\b"),
    ("i want", r"\bi want\b"),
    (
        "did you consider/check",
        r"\bdid you (?:consider|check|think)\b",
    ),
];
static NEAR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r"(?i)\b(ideally|what if|eventually|imagine|radical|north star|cornerstone|my vision|",
        r"would be (?:great|nice|amazing)|have you (?:thought|considered)|did you consider|",
        r"i wish|someday|long term|in (?:the )?future)\b"
    ))
    .unwrap()
});
pub fn past_lines(path: &Path, session: &str) -> (BTreeMap<String, usize>, Vec<Past>) {
    static NOT_YOURS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)^\s*\[(?:Workflow harness|Request interrupted|CSR)|not typed by this session",
        )
        .unwrap()
    });
    static DASH: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\s+-\s+").unwrap());
    let voice: Vec<_> = VOICE
        .iter()
        .map(|(n, re)| (*n, regex::Regex::new(re).unwrap()))
        .collect();
    let mut counts = BTreeMap::new();
    let mut out = vec![];
    super::tail::scan(path, |entry| {
        let said = string(&entry["display"]);
        if NOT_YOURS.is_match(said) {
            return;
        }
        let text = send::own_words(said);
        let text = text.trim();
        if text.is_empty() || text.starts_with('/') {
            return;
        }
        for (name, re) in &voice {
            if re.is_match(&text.to_lowercase()) {
                *counts.entry((*name).into()).or_default() += 1;
            }
        }
        let sid = string(&entry["sessionId"]);
        if sid.is_empty() || sid == session {
            return;
        }
        for sentence in coach::sentences(text) {
            for part in DASH.split(sentence) {
                let part = part.trim();
                if NEAR.is_match(part) && (30..=240).contains(&part.chars().count()) {
                    out.push(Past {
                        at: num(&entry["timestamp"]) / 1000.,
                        session: sid.into(),
                        project: string(&entry["project"]).into(),
                        text: part.into(),
                        score: 0.,
                    });
                }
            }
        }
    });
    (counts, out)
}
pub fn voice_and_lines(db: &Db, paths: &Paths, sid: &str) -> (BTreeMap<String, usize>, Vec<Past>) {
    let (counts, lines) = past_lines(&paths.home.join(".claude/history.jsonl"), sid);
    let now = db.embeddings(sid, None);
    if now.is_empty() {
        return (counts, vec![]);
    }
    let mut near = vec![];
    for mut line in lines {
        let prefix: String = line.text.chars().take(50).collect();
        let vectors = db.embeddings(&line.session, Some(&prefix));
        if let Some(vector) = vectors.first() {
            let score = now
                .iter()
                .map(|other| vector.iter().zip(other).map(|(a, b)| a * b).sum())
                .max_by(f64::total_cmp)
                .unwrap_or(0.);
            if score >= 0.4 {
                line.score = score;
                line.project = Path::new(line.project.trim_end_matches('/'))
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                near.push(line);
            }
        }
    }
    near.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.at.total_cmp(&a.at))
            .then(b.session.cmp(&a.session))
            .then(b.project.cmp(&a.project))
            .then(b.text.cmp(&a.text))
    });
    near.truncate(4);
    (counts, near)
}
pub fn past_rows(near: &[Past], now: f64) -> Vec<String> {
    if near.is_empty() {
        return vec![];
    }
    let mut rows = vec![
        String::new(),
        concat!(
        "Lines the person typed in past sessions, in their own words, that sit close to this work ",
        "(a line older than 30 days is context only: never the main reason to ask):"
    )
        .into(),
    ];
    for line in near {
        let days = if line.at != 0. {
            ((now - line.at) / 86400.).floor() as i64
        } else {
            0
        };
        rows.push(format!(
            "- {} ({}): {} [session {}]{}",
            recall::date(line.at, "%m-%d"),
            line.project,
            recall::quoted(&line.text, 220),
            line.session.chars().take(8).collect::<String>(),
            if days > 30 {
                format!(" ({days} days old: context only)")
            } else {
                String::new()
            }
        ));
    }
    rows
}
pub fn dream_rows(dreams: &[Value]) -> Vec<String> {
    if dreams.is_empty() {
        return vec![];
    }
    let mut rows = vec![
        String::new(),
        concat!(
            "Dreams already offered here, and what the person answered ",
            "(where they said why, their own words):"
        )
        .into(),
    ];
    rows.extend(dreams.iter().map(|d| {
        let said = d["said"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| format!(": you said {}", recall::quoted(s, 200)))
            .unwrap_or_default();
        format!(
            "- {}: {} [{}{said}]",
            recall::date(num(&d["at"]), "%m-%d"),
            recall::quoted(string(&d["line"]), 200),
            d["answer"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("no answer")
        )
    }));
    rows
}
#[derive(Debug)]
pub struct Built {
    pub kind: String,
    pub name: String,
    pub sessions: BTreeMap<String, String>,
    pub folder: String,
    pub latest: String,
    pub at: String,
    pub excerpt: String,
}
pub fn described(folder: &Path, kind: &str) -> String {
    if kind == "skill" {
        use std::io::Read;
        let mut bytes = vec![];
        let Some(mut file) = std::fs::File::open(folder.join("SKILL.md")).ok() else {
            return String::new();
        };
        if file.by_ref().take(4000).read_to_end(&mut bytes).is_err() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        let re = regex::Regex::new(r"(?m)^description:\s*(.+)$").unwrap();
        return re
            .captures(&text)
            .map(|c| c[1].trim().trim_matches(['"', '\'']).to_owned())
            .unwrap_or_default();
    }
    string(&paths::read_json(&folder.join(".claude-plugin/plugin.json"))["description"]).into()
}
pub fn built_near(db: &Db, sid: &str, talk: &str) -> Vec<Built> {
    let patterns = [
        ("skill", r"/\.claude/skills/([^/]+)/"),
        ("mod", r"/\.claude/dev-mods/[^/]+/([^/]+)/"),
        ("plugin", r"/plugins/([^/]+)/"),
    ];
    let patterns: Vec<_> = patterns
        .into_iter()
        .map(|(k, p)| (k, regex::Regex::new(p).unwrap()))
        .collect();
    let mut things: Vec<Built> = vec![];
    for row in db.query(
        "select session_id,file,max(created_at) from episode_anchors \
        where session_id!=?1 and (file like '%/.claude/skills/%' or file like '%/dev-mods/%' \
        or file like '%/plugins/%') group by session_id,file",
        &[sid.into()],
    ) {
        let file = string(&row[1]);
        for (kind, re) in &patterns {
            let Some(m) = re.captures(file) else { continue };
            if *kind == "plugin" && file[..m.get(0).unwrap().start()].ends_with(".claude") {
                continue;
            }
            let name = &m[1];
            let i = things
                .iter()
                .position(|t| t.kind == *kind && t.name == name)
                .unwrap_or_else(|| {
                    things.push(Built {
                        kind: (*kind).into(),
                        name: name.into(),
                        sessions: BTreeMap::new(),
                        folder: file[..m.get(0).unwrap().end()].into(),
                        latest: String::new(),
                        at: String::new(),
                        excerpt: String::new(),
                    });
                    things.len() - 1
                });
            let at = string(&row[2]).to_owned();
            let entry = things[i]
                .sessions
                .entry(string(&row[0]).into())
                .or_default();
            if at > *entry {
                *entry = at;
            }
            break;
        }
    }
    things.retain(|t| t.name.chars().count() >= 4 && coach::mentions(talk, &t.name));
    for t in &mut things {
        if let Some((sid, at)) = t
            .sessions
            .iter()
            .max_by(|(a, x), (b, y)| x.cmp(y).then(b.cmp(a)))
        {
            t.latest = sid.clone();
            t.at = at.clone();
        }
    }
    things.sort_by(|a, b| b.at.cmp(&a.at));
    things.truncate(3);
    for t in &mut things {
        t.excerpt = described(Path::new(&t.folder), &t.kind)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    }
    things
}
pub fn recent_files(session: &Session, now: f64) -> Vec<String> {
    let mut recent: Vec<_> = session
        .file_order
        .iter()
        .filter_map(|path| {
            let at = num(&session.data["last"][path]);
            (at != 0. && now - at <= 1800.).then(|| (at, path.clone()))
        })
        .collect();
    recent.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    if !recent.is_empty() {
        return recent.into_iter().take(8).map(|(_, p)| p).collect();
    }
    let mut files = session.file_order.clone();
    files.sort_by(|a, b| num(&session.data["files"][b]).total_cmp(&num(&session.data["files"][a])));
    files.truncate(8);
    files
}
pub fn brief(db: &Db, paths: &Paths, root: &Path, sid: &str, now: f64) -> String {
    let mut session = Session::new(sid);
    session.read_transcript(paths);
    let touched = recent_files(&session, now);
    let mut out = vec![];
    if !touched.is_empty() {
        let named = touched
            .iter()
            .map(|path| {
                let shown = Path::new(path)
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| {
                        path.split('/')
                            .rev()
                            .take(3)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                            .join("/")
                    });
                format!("{shown} ({}×)", num(&session.data["files"][path]) as u64)
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "Files Claude handled lately, newest first: {named}."
        ));
    }
    let agents = agents::agents_of(paths, sid);
    let mut heard: Vec<_> = session
        .file_order
        .iter()
        .map(|p| (p.clone(), num(&session.data["last"][p])))
        .collect();
    for row in &agents.agents {
        for path in &row.edits {
            if !Path::new(path).is_absolute() {
                continue;
            }
            if let Some((_, at)) = heard.iter_mut().find(|(p, _)| p == path) {
                *at = at.max(row.last_at / 1000.);
            } else {
                heard.push((path.clone(), row.last_at / 1000.));
            }
        }
    }
    heard.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut roots = HashMap::new();
    let mut tracked = vec![];
    for (path, _) in heard {
        let folder = Path::new(&path).parent().unwrap_or(Path::new(""));
        let owner = roots.entry(folder.to_path_buf()).or_insert_with(|| {
            if folder.is_dir() {
                paths::git(folder, &["rev-parse", "--show-toplevel"])
            } else {
                None
            }
        });
        if let Some(root) = owner {
            tracked.push((path, root.clone()));
        }
    }
    let before = array(&session.data["compacts"])
        .iter()
        .filter_map(Value::as_f64)
        .filter(|n| *n != 0.)
        .max_by(f64::total_cmp);
    let mut notes = vec![];
    let mut seen = HashSet::new();
    for (path, root) in tracked.into_iter().take(6) {
        for note in note::notes_for(
            db,
            paths,
            Path::new(&root),
            Path::new(&path),
            sid,
            before,
            None,
        )
        .notes
        {
            if seen.insert(note.id.clone()) {
                notes.push(note);
            }
        }
    }
    let changed: Vec<_> = notes.iter().filter(|n| n.kind == "shift").take(5).collect();
    let rules: Vec<_> = notes.iter().filter(|n| n.kind == "rule").take(3).collect();
    if !changed.is_empty() {
        out.extend([
            String::new(),
            concat!(
                "Code that changed after one of the person's past ",
                "sessions, or this one before its last compaction, worked on it:"
            )
            .into(),
        ]);
        for n in changed {
            out.push(format!(
                "* {} [commit {}{}]",
                n.line,
                n.oid
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(7)
                    .collect::<String>(),
                if n.earlier == Some(true) {
                    String::new()
                } else {
                    format!(
                        ", session {}",
                        n.conversation.chars().take(8).collect::<String>()
                    )
                }
            ));
        }
    }
    if !rules.is_empty() {
        out.extend([
            String::new(),
            "Rules the person set for these files:".into(),
        ]);
        for n in rules {
            out.push(format!(
                "* {} [session {}]",
                n.line,
                n.conversation.chars().take(8).collect::<String>()
            ));
        }
    }
    let talk = array(&session.data["talk"]);
    let talk = talk
        .iter()
        .skip(talk.len().saturating_sub(60))
        .map(|r| string(&r[1]))
        .collect::<Vec<_>>()
        .join(" ");
    let built = built_near(db, sid, &talk);
    if !built.is_empty() {
        out.extend([
            String::new(),
            "Things the person built in other sessions that came up in this work:".into(),
        ]);
        for t in built {
            let n = t.sessions.len();
            out.push(format!(
                "* {} {}: built over {n} {}, latest {} [session {}]",
                t.kind,
                t.name,
                if n == 1 { "session" } else { "sessions" },
                t.at.get(5..10).unwrap_or(""),
                t.latest.chars().take(8).collect::<String>()
            ));
            if !t.excerpt.is_empty() {
                out.push(format!("  {}", recall::quoted(&t.excerpt, 400)));
            }
        }
    }
    let project = root.file_name().unwrap_or_default().to_string_lossy();
    let corrections: Vec<_> = db
        .query(
            "select ts,quote,session_id,transcript_path,byte_start \
        from intent_events where kind='correction' and session_id!=?1 and \
        (project=?2 or project like ?2||'-%') order by ts desc limit 200",
            &[sid.into(), project.as_ref().into()],
        )
        .into_iter()
        .filter(|r| {
            send::typed_by_you(
                Path::new(string(&r[3])),
                num(&r[4]) as u64,
                Some(string(&r[1])),
            )
        })
        .take(3)
        .collect();
    if !corrections.is_empty() {
        out.extend([
            String::new(),
            "Corrections the person typed in this repo lately, in their own words:".into(),
        ]);
        for r in corrections {
            let date = paths::epoch(string(&r[0]))
                .filter(|n| *n != 0.)
                .map(|n| recall::date(n, "%m-%d"))
                .unwrap_or_else(|| string(&r[0]).chars().take(10).collect());
            out.push(format!(
                "- {date}: {} [session {}]",
                recall::quoted(string(&r[1]), 200),
                string(&r[2]).chars().take(8).collect::<String>()
            ));
        }
    }
    out.extend(past_rows(&voice_and_lines(db, paths, sid).1, now));
    let mut log = dreams::DreamLog::default();
    log.poll(paths);
    let dreams = log.dreams(&root.to_string_lossy());
    out.extend(dream_rows(&dreams[..dreams.len().min(8)]));
    let envelope = agents::context_envelope(&agents, sid);
    if !envelope.is_empty() {
        out.extend([String::new(), envelope]);
    }
    out.join("\n").trim().into()
}
