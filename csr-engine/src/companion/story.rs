use super::{
    array,
    db::Db,
    num,
    paths::{self, Paths},
    session::Session,
    string,
};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum News {
    Present(StoryNews),
    Absent { story: bool },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct StoryNews {
    pub story: bool,
    pub at: f64,
    pub covered: Option<u64>,
    pub messages: u64,
    pub messages_since: u64,
    pub prompts_since: usize,
    pub receipts: usize,
    #[serde(flatten)]
    pub spend: Spend,
    pub liked_2h: usize,
    pub transcript: Option<std::path::PathBuf>,
}
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct Spend {
    pub calls_24h: usize,
    pub usd_24h: Option<f64>,
    pub tokens_2h: i64,
    pub last_tokens: Option<i64>,
}
#[derive(Debug)]
pub struct StoryMap {
    pub at: Option<f64>,
    pub covered: Option<u64>,
    pub rev: u64,
    pub by: String,
    pub previous_sigs: Vec<String>,
    pub has_previous: bool,
    pub entries: Vec<Value>,
}
fn normalize_side(raw: &Value) -> Option<Vec<Value>> {
    raw.as_array()?
        .iter()
        .map(|part| {
            if !part.is_object() {
                return None;
            }
            let tier = if part["tier"] == "verified" {
                "verified"
            } else {
                "model"
            };
            if let Some(op) = part["op"].as_str().filter(|s| !s.trim().is_empty()) {
                return Some(json!({"op":op.trim(),"tier":tier}));
            }
            let scope = part["scope"].as_str().filter(|s| !s.trim().is_empty())?;
            let sym = part["sym"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::trim);
            let mark = part["mark"]
                .as_str()
                .filter(|s| ["+", "~", "-", "!"].contains(s))
                .unwrap_or("");
            Some(json!({"scope":scope.trim(),"sym":sym,"mark":mark,"tier":tier}))
        })
        .collect()
}
pub fn parse_story_map(raw: &Value) -> Option<StoryMap> {
    raw["v"].as_i64()?;
    let cur = raw["current"].as_object()?;
    let entries = cur.get("entries")?.as_array()?;
    if entries.is_empty() {
        return None;
    }
    let entries = entries
        .iter()
        .take(4)
        .map(|e| {
            let kind = e["kind"].as_str().filter(|s| !s.trim().is_empty())?;
            let claim = e["claim"].as_str().filter(|s| !s.trim().is_empty())?;
            let before = normalize_side(&e["before"])?;
            let after = normalize_side(&e["after"])?;
            let receipts = e["receipts"].as_array()?;
            if receipts.iter().any(|v| !v.is_string()) {
                return None;
            }
            let receipts: Vec<_> = receipts
                .iter()
                .map(|v| string(v).trim())
                .filter(|s| !s.is_empty())
                .collect();
            let now = e["now"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "));
            let was = e["was"]
                .as_str()
                .filter(|s| now.is_some() && !s.trim().is_empty() && s.trim() != "-")
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "));
            Some(json!({
                "sig":e["sig"].as_str().filter(|s| !s.trim().is_empty()).map(str::trim),
                "kind":kind.trim(),"claim":claim.split_whitespace().collect::<Vec<_>>().join(" "),
                "was":was,"now":now,"before":before,"after":after,"receipts":receipts,
                "grounded":super::truthy(&e["grounded"]),"moved":super::truthy(&e["moved"]),
            }))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(StoryMap {
        at: cur.get("at").and_then(Value::as_f64),
        covered: cur.get("covered_msgs").and_then(Value::as_u64),
        rev: cur
            .get("rev")
            .and_then(Value::as_u64)
            .filter(|n| *n >= 1)
            .unwrap_or(1),
        by: cur
            .get("by")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("Haiku")
            .into(),
        previous_sigs: array(&raw["previous"]["sigs"])
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        has_previous: raw["previous"].is_object(),
        entries,
    })
}
pub fn session_story_map(db: &Db, sid: &str) -> Option<StoryMap> {
    if sid.is_empty()
        || std::env::var("CSR_NO_STORY_MAP")
            .is_ok_and(|s| matches!(s.trim().to_lowercase().as_str(), "1" | "true"))
    {
        return None;
    }
    let value = db.scalar(
        "select value from meta where key=?",
        &[json!(format!("story_map:{sid}"))],
    );
    parse_story_map(&serde_json::from_str::<Value>(string(&value)).ok()?)
}
pub fn display_story_map(db: &Db, paths: &Paths, sid: &str) -> Option<(StoryMap, String)> {
    for id in super::session::lineage(paths, sid) {
        if let Some(map) = session_story_map(db, &id) {
            return Some((map, id));
        }
    }
    None
}
fn utc_stamp(at: f64, format: &str) -> String {
    Utc.timestamp_opt(at as i64, 0)
        .single()
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}
pub fn new_receipts(db: &Db, sid: &str, root: Option<&Path>, at: f64) -> anyhow::Result<usize> {
    let rows = db.try_query(
        "select file_path,functions_added,functions_removed,types_added,\
        types_removed from code_evolution where session_id=? and datetime(timestamp)>?",
        &[sid.into(), utc_stamp(at, "%Y-%m-%d %H:%M:%S").into()],
    )?;
    let mut files = HashSet::new();
    let mut symbols = HashSet::new();
    for row in rows {
        let path = string(&row[0]);
        files.insert(path.to_owned());
        for text in &row[1..] {
            if let Ok(Value::Array(names)) = serde_json::from_str::<Value>(string(text)) {
                for name in names {
                    symbols.insert((
                        path.to_owned(),
                        name.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| name.to_string()),
                    ));
                }
            }
        }
    }
    let commits = root
        .and_then(|root| {
            paths::git(
                root,
                &[
                    "rev-list",
                    "--count",
                    &format!("--since={}Z", utc_stamp(at, "%Y-%m-%dT%H:%M:%S")),
                    "HEAD",
                ],
            )
        })
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    Ok(2 * files.len() + symbols.len() + 3 * commits)
}
pub fn story_spend(db: &Db, now: f64) -> anyhow::Result<Spend> {
    let rows = db.try_query(
        "select ts,model,input_tokens,output_tokens,cache_read_tokens,\
        cache_creation_tokens from narrative_usage where call_site='story' and ts>=? \
        order by ts desc",
        &[utc_stamp(now - 86400., "%Y-%m-%dT%H:%M:%S").into()],
    )?;
    let last = db.try_query(
        "select input_tokens+output_tokens+cache_read_tokens+\
        cache_creation_tokens from narrative_usage where call_site='story' \
        order by id desc limit 1",
        &[],
    )?;
    let mut out = Spend {
        calls_24h: rows.len(),
        usd_24h: Some(0.),
        last_tokens: last.first().and_then(|r| r[0].as_i64()),
        ..Spend::default()
    };
    let recent = utc_stamp(now - 7200., "%Y-%m-%dT%H:%M:%S");
    for row in rows {
        let counts: Vec<_> = row[2..].iter().map(|n| num(n) as i64).collect();
        if string(&row[0]) >= recent.as_str() {
            out.tokens_2h += counts.iter().sum::<i64>();
        }
        let cost = if counts.iter().any(|n| *n != 0) {
            crate::journal::composer::model_price(string(&row[1])).map(|(input, output)| {
                (input * (counts[0] as f64 + 0.1 * counts[2] as f64 + 1.25 * counts[3] as f64)
                    + output * counts[1] as f64)
                    / 1e6
            })
        } else {
            Some(0.)
        };
        out.usd_24h = out.usd_24h.zip(cost).map(|(a, b)| a + b);
    }
    Ok(out)
}
pub fn story_news(paths: &Paths, root: Option<&Path>, sid: &str, now: f64) -> News {
    let Ok(db) = Db::open(paths) else {
        return News::Absent { story: false };
    };
    story_news_in(&db, paths, root, sid, now)
}
pub fn story_news_in(db: &Db, paths: &Paths, root: Option<&Path>, sid: &str, now: f64) -> News {
    let absent = || News::Absent { story: false };
    let Some(map) = session_story_map(db, sid) else {
        return absent();
    };
    let Some(at) = map.at else { return absent() };
    let mut session = Session::new(sid);
    session.read_transcript(paths);
    let messages = session.data["messages"].as_u64().unwrap_or(0);
    let mut likes = BTreeMap::new();
    super::tail::scan(&paths.keep.join("likes.jsonl"), |r| {
        if let Some(key) = r["key"].as_str() {
            if r.get("signal").is_none_or(|v| v == "explicit_like") {
                likes.insert(key.to_owned(), (super::truthy(&r["liked"]), num(&r["at"])));
            }
        }
    });
    News::Present(StoryNews {
        story: true,
        at,
        covered: map.covered,
        messages,
        messages_since: map
            .covered
            .map(|c| messages.saturating_sub(c))
            .unwrap_or(messages),
        prompts_since: array(&session.data["prompts"])
            .iter()
            .filter(|t| num(t) > at)
            .count(),
        receipts: new_receipts(db, sid, root, at).unwrap_or(0),
        spend: story_spend(db, now).unwrap_or_default(),
        liked_2h: likes
            .iter()
            .filter(|(key, (liked, when))| {
                key.starts_with("shift:") && *liked && *when >= now - 7200.
            })
            .count(),
        transcript: session.path,
    })
}
