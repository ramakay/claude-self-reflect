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
    super::render::text::fit(
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
fn shift_source(entry: &Value, by: &str, room: usize) -> Vec<String> {
    if entry["grounded"] != true {
        return vec!["Haiku's words · names not checked".into()];
    }
    let receipts = super::array(&entry["receipts"]);
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
    let width = room.saturating_sub(label.len()).max(8);
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
fn shift_group(entry: &Value, map: &super::story::StoryMap, room: usize) -> Vec<String> {
    use super::{array, render::text::fit};
    let new = if entry["moved"] == true && map.has_previous {
        "new "
    } else {
        ""
    };
    let kind = string(&entry["kind"]);
    if entry["now"].is_null() {
        let inner = room - 2;
        let mut rows = vec![fit(
            &format!("{new}{kind}: {}", string(&entry["claim"])),
            room,
        )];
        let before = shift_side(array(&entry["before"]), inner - 4);
        let after = shift_side(
            array(&entry["after"]),
            if before.is_empty() { inner } else { inner - 4 },
        );
        if !before.is_empty() && !after.is_empty() {
            let both = format!("{before} > {after}");
            if both.chars().count() <= inner {
                rows.push(format!("  {both}"));
            } else {
                rows.extend([format!("  was {before}"), format!("  now {after}")]);
            }
        } else if !before.is_empty() {
            rows.push(format!("  was {before}"));
        } else if !after.is_empty() {
            rows.push(format!("  {after}"));
        }
        rows.extend(
            shift_source(entry, &map.by, inner)
                .iter()
                .map(|s| format!("  {}", fit(s, inner))),
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
    let mut rows = vec![fit(&head, room)];
    if let Some(was) = entry["was"].as_str() {
        rows.extend(shift_sentence("Before: ", was, room));
        rows.extend(shift_sentence("After:  ", string(&entry["now"]), room));
    } else {
        rows.extend(shift_sentence("New:    ", string(&entry["now"]), room));
    }
    rows.extend(
        shift_source(entry, &map.by, room)
            .iter()
            .map(|s| fit(s, room)),
    );
    rows
}
pub fn shift_map_json(db: &Db, paths: &paths::Paths, sid: &str) -> Option<ShiftMap> {
    let (map, source) = super::story::display_story_map(db, paths, sid)?;
    let mut session = super::session::Session::new(&source);
    session.read_transcript(paths);
    let messages = session.data["messages"].as_u64().unwrap_or(0);
    let mut title = "Shift Map".to_owned();
    if let Some(covered) = map.covered {
        let behind = messages.saturating_sub(covered);
        title.push_str(&if behind == 0 {
            " · current".into()
        } else {
            format!(
                " · as of {behind} {} ago",
                if behind == 1 { "message" } else { "messages" }
            )
        });
    }
    if source != sid {
        title.push_str(&format!(
            " · from {}",
            source.chars().take(8).collect::<String>()
        ));
    }
    Some(ShiftMap {
        title,
        rows: map
            .entries
            .iter()
            .flat_map(|e| shift_group(e, &map, 40))
            .collect(),
    })
}
