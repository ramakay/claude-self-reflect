use super::{db::Db, dreams, num, paths, recall, string};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub struct Shift {
    pub file: String,
    pub symbol: Option<String>,
    pub verb: String,
    pub oid: String,
    pub date: String,
    pub subject: String,
}
impl From<&Value> for Shift {
    fn from(s: &Value) -> Self {
        Self {
            file: string(&s["file"]).into(),
            symbol: s["symbol"].as_str().map(str::to_owned),
            verb: string(&s["verb"]).into(),
            oid: string(&s["oid"]).into(),
            date: string(&s["date"]).into(),
            subject: string(&s["subject"]).into(),
        }
    }
}
#[derive(Serialize, Deserialize)]
pub struct Change {
    pub conversation: String,
    pub title: String,
    pub current: bool,
    pub talked: String,
    pub commits: usize,
    pub shifts: Vec<Shift>,
}
#[derive(Serialize, Deserialize)]
pub struct Dream {
    pub id: String,
    pub at: f64,
    pub line: String,
    pub title: Option<String>,
    pub answer: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct Sample {
    pub line: String,
    pub at: f64,
    pub answer: String,
    pub project: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct ShiftMap {
    pub title: String,
    pub rows: Vec<String>,
}
#[derive(Serialize, Deserialize)]
pub struct PaneJson {
    pub repo: Option<String>,
    pub total: usize,
    pub shifts: Vec<Shift>,
    pub changes: Vec<Change>,
    pub changes_total: usize,
    pub dreams: Vec<Dream>,
    pub dreams_on: bool,
    pub recalled: u64,
    pub dreams_7d: usize,
    pub dream_tokens_7d: f64,
    pub reminded_7d: u64,
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrections_carried_7d: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shifts_told_7d: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample: Option<Option<Sample>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shift_map: Option<Option<ShiftMap>>,
}

pub fn pane_json(paths: &paths::Paths, repo: &Path, sid: &str, limit: usize, now: f64) -> PaneJson {
    let db = Db::open(paths).ok();
    let git_root = paths::git(repo, &["rev-parse", "--show-toplevel"]);
    let root = git_root.clone().or_else(|| {
        db.as_ref()?
            .pane_root(repo)
            .map(|p| p.to_string_lossy().into_owned())
    });
    let on = dreams::dreams_on(paths);
    let mut out = PaneJson {
        repo: None,
        total: 0,
        shifts: vec![],
        changes: vec![],
        changes_total: 0,
        dreams: vec![],
        dreams_on: on,
        recalled: 0,
        dreams_7d: 0,
        dream_tokens_7d: 0.,
        reminded_7d: 0,
        status: root
            .as_ref()
            .map(|_| "☾ CSR is not set up yet: npx claude-self-reflect setup".into()),
        corrections_carried_7d: None,
        shifts_told_7d: None,
        git: None,
        sample: None,
        shift_map: None,
    };
    let (Some(root), Some(db)) = (root, db) else {
        return out;
    };
    let shifts = db.shifts(Path::new(&root), usize::MAX);
    let changes = recall::load_changes(&db, paths, &shifts, sid, None);
    let mut log = dreams::DreamLog::default();
    log.poll(paths);
    let all_dreams = log.dreams(&root);
    let week: Vec<_> = all_dreams
        .iter()
        .filter(|d| num(&d["at"]) >= now - 7. * 86400.)
        .collect();
    out.dreams_7d = week.len();
    out.dream_tokens_7d = week.iter().map(|d| num(&d["tokens"])).sum();
    out.dreams = all_dreams
        .iter()
        .take(20)
        .map(|d| Dream {
            id: string(&d["id"]).into(),
            at: num(&d["at"]),
            line: string(&d["line"]).into(),
            title: d["title"].as_str().map(str::to_owned),
            answer: d["answer"].as_str().map(str::to_owned),
        })
        .collect();
    out.total = shifts.len();
    out.shifts = shifts.iter().take(limit).map(Shift::from).collect();
    out.changes_total = changes.len();
    out.changes = changes
        .iter()
        .take(20)
        .map(|c| Change {
            conversation: c.conversation.clone(),
            title: c.title.clone(),
            current: c.current,
            talked: recall::date(c.talked, "%Y-%m-%d"),
            commits: c.commits,
            shifts: c.shifts.iter().take(12).map(Shift::from).collect(),
        })
        .collect();
    let floor = db.scalar(
        "select coalesce((select cast(value as integer) from meta \
        where key='witness_verdicts_v2_floor'),0)",
        &[],
    );
    let start = db.scalar(
        "select min(created_at) from witness_verdicts where id>?",
        &[floor],
    );
    let carried = db
        .scalar(
            "select count(*) from correction_deliveries \
        where delivered_at >= datetime('now','-7 days')",
            &[],
        )
        .as_u64()
        .unwrap_or(0);
    let told = db
        .scalar(
            "select count(*) from dream_deliveries \
        where delivered_at >= datetime('now','-7 days') and delivered_at>=?",
            &[if start.is_null() {
                json!("9999")
            } else {
                start
            }],
        )
        .as_u64()
        .unwrap_or(0);
    out.corrections_carried_7d = Some(carried);
    out.shifts_told_7d = Some(told);
    out.reminded_7d = carried + told;
    for id in if sid.is_empty() {
        vec![String::new()]
    } else {
        super::session::lineage(paths, sid)
    } {
        out.recalled += db
            .scalar(
                "select count(*) from retrieval_events where session_id=?",
                &[json!(id)],
            )
            .as_u64()
            .unwrap_or(0);
    }
    out.status = Some(format!(
        "☾ CSR remembered {}× this session · {}",
        out.recalled,
        if on {
            format!(
                "{} {} this week",
                out.dreams_7d,
                if out.dreams_7d == 1 {
                    "dream"
                } else {
                    "dreams"
                }
            )
        } else {
            "dreams off".into()
        }
    ));
    out.git = Some(git_root.is_some());
    out.sample = Some(sample(paths));
    out.shift_map = Some(shift_map_json(&db, paths, sid));
    out.repo = Some(root);
    out
}

fn sample(paths: &paths::Paths) -> Option<Sample> {
    let mut taken = vec![];
    for path in [
        paths.keep.join("dreams.jsonl"),
        paths
            .home
            .join(".claude-self-reflect/csr-shifts/dreams.jsonl"),
    ] {
        let mut records = vec![];
        super::tail::scan(&path, |r| records.push(r));
        for d in dreams::load_dream_log(&records, "") {
            if dreams::ADOPTED.contains(&string(&d["answer"]))
                && !taken.iter().any(|v: &Value| v["id"] == d["id"])
            {
                taken.push(d);
            }
        }
    }
    let best = taken.iter().max_by(|a, b| {
        (a["answer"] == "helpful")
            .cmp(&(b["answer"] == "helpful"))
            .then(num(&a["at"]).total_cmp(&num(&b["at"])))
    })?;
    Some(Sample {
        line: string(&best["line"]).into(),
        at: num(&best["at"]),
        answer: string(&best["answer"]).into(),
        project: Path::new(string(&best["repo"]))
            .file_name()
            .and_then(|p| p.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    })
}

pub fn shift_map_json(db: &Db, paths: &paths::Paths, sid: &str) -> Option<ShiftMap> {
    let (map, source) = super::story::display_story_map(db, paths, sid)?;
    let mut session = super::session::Session::new(&source);
    session.read_transcript(paths);
    let messages = session.data["messages"].as_u64().unwrap_or(0);
    let (title, rows) = super::shiftmap::shift_map_rows(
        &super::shiftmap::map_value(&map),
        Some(messages),
        super::shiftmap::SHIFT_ROOM,
        (source != sid).then_some(source.as_str()),
    );
    Some(ShiftMap { title, rows })
}
