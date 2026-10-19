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

// ---- When the story may be told again on its own (port of `hooks/story.ts`) ----
//
// The Shift Map's automatic refresh, off unless CSR_STORY_REWRITES=1. One model call each time, so
// four gates, each decided without a model call from what `story_news` reads:
//  1. News. At least 4 new receipts (2 a file your edits touched, 1 a symbol they added or
//     removed, 3 a commit) and at least 2 prompts of yours since the map's current telling.
//  2. Spacing. A 2 hour chapter allows 3 refreshes, 1 more for each shift you liked in it, 6 at
//     most. The messages since the last telling must reach what the news earns: 80 usual messages
//     to a chapter, shared over the refreshes left, more when the news is thin (never under 12).
//  3. A share of the work. What story calls spent in the chapter, plus the guess for the next (the
//     last one, else 1600 tokens), stays within 2 tokens for every 100 Claude spends. Where
//     Claude's tokens were not counted (dreams off), this gate has nothing to compare and the
//     caps below stand alone.
//  4. Caps. 60 seconds between tellings, 40 calls and $0.10 in a day. A day's spend that is
//     unknown stops the refresh; the person's own button is unchanged.
// The first reason a gate fails is the answer, so a test (and a person asking why) can read it.
//
// The caller (the `turn.complete` group of the pace package) keeps `starts`, when each automatic
// refresh began, and `claude`, the tokens it counted Claude spending.

pub const NEWS_RECEIPTS: f64 = 4.;
pub const NEWS_PROMPTS: f64 = 2.;
pub const CHAPTER_MS: f64 = 2. * 60. * 60. * 1000.;
pub const BASE_REFRESHES: f64 = 3.;
pub const MOST_REFRESHES: f64 = 6.;
pub const USUAL_MESSAGES: f64 = 80.;
pub const LEAST_MESSAGES: f64 = 12.;
pub const STORY_SHARE: f64 = 0.02;
pub const FIRST_GUESS: f64 = 1600.;
pub const COOLDOWN_MS: f64 = 60_000.;
pub const DAY_CALLS: f64 = 40.;
pub const DAY_USD: f64 = 0.1;

/// What `story_news` says, as `decide` reads it: every count is zero where it is missing, as the
/// TypeScript's `count` reads one.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Facts {
    /// False: no map yet (or the kill switch), so nothing to refresh.
    pub story: bool,
    /// Seconds: when the current telling was written.
    pub at: Option<f64>,
    pub messages_since: f64,
    pub prompts_since: f64,
    pub receipts: f64,
    pub calls_24h: f64,
    /// None: a call's model has no price.
    pub usd_24h: Option<f64>,
    /// What story calls spent in the last two hours.
    pub tokens_2h: f64,
    pub last_tokens: f64,
    pub liked_2h: f64,
    pub has_transcript: bool,
}

/// Why `decide` said what it did: `Go`, or the first gate that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Go,
    NoStory,
    NoTranscript,
    NoNews,
    Cooldown,
    ChapterCap,
    Spacing,
    Share,
    SpendUnknown,
    DayCap,
}

impl Why {
    /// The reason as the TypeScript names it.
    pub fn name(self) -> &'static str {
        match self {
            Why::Go => "go",
            Why::NoStory => "no-story",
            Why::NoTranscript => "no-transcript",
            Why::NoNews => "no-news",
            Why::Cooldown => "cooldown",
            Why::ChapterCap => "chapter-cap",
            Why::Spacing => "spacing",
            Why::Share => "share",
            Why::SpendUnknown => "spend-unknown",
            Why::DayCap => "day-cap",
        }
    }
}

fn count(value: &Value) -> f64 {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n > 0.)
        .unwrap_or(0.)
}

/// What `story_news` printed (or any text that is not one): no story unless it says `story: true`.
pub fn parse_news(text: Option<&str>) -> Facts {
    let value: Value = text
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null);
    if value["story"] != true {
        return Facts::default();
    }
    Facts {
        story: true,
        at: value["at"].as_f64(),
        messages_since: count(&value["messages_since"]),
        prompts_since: count(&value["prompts_since"]),
        receipts: count(&value["receipts"]),
        calls_24h: count(&value["calls_24h"]),
        usd_24h: value["usd_24h"].as_f64().filter(|n| n.is_finite()),
        tokens_2h: count(&value["tokens_2h"]),
        last_tokens: count(&value["last_tokens"]),
        liked_2h: count(&value["liked_2h"]),
        has_transcript: value["transcript"].as_str().is_some_and(|t| !t.is_empty()),
    }
}

impl Facts {
    /// What the engine's own `story_news` found, in the shape `decide` reads.
    pub fn of(news: &News) -> Self {
        match news {
            News::Absent { .. } => Self::default(),
            News::Present(news) => Self {
                story: news.story,
                at: Some(news.at),
                messages_since: news.messages_since as f64,
                prompts_since: news.prompts_since as f64,
                receipts: news.receipts as f64,
                calls_24h: news.spend.calls_24h as f64,
                usd_24h: news.spend.usd_24h.filter(|n| n.is_finite()),
                tokens_2h: news.spend.tokens_2h.max(0) as f64,
                last_tokens: news.spend.last_tokens.unwrap_or(0).max(0) as f64,
                liked_2h: news.liked_2h as f64,
                has_transcript: news.transcript.is_some(),
            },
        }
    }
}

/// How many refreshes a chapter allows: 3, and 1 more for each shift you liked in it, 6 at most.
pub fn allowance(liked: f64) -> f64 {
    let liked = liked.max(0.);
    MOST_REFRESHES.min(BASE_REFRESHES + (MOST_REFRESHES - BASE_REFRESHES).min(liked))
}

/// Messages since the last telling that the news earns a refresh at: the usual chapter over the
/// refreshes left, scaled by how thin the news is (4 receipts: the most; 12 or more: the least).
pub fn messages_due(receipts: f64, left: f64) -> f64 {
    let thin = 6. / receipts.clamp(1., 12.);
    LEAST_MESSAGES.max((USUAL_MESSAGES / left.max(1.) * thin).round())
}

/// May the story be told again now? `starts`: when (ms) each automatic refresh was started this
/// session. `claude`: tokens counted Claude spending (0 when they were not counted).
pub fn decide(news: &Facts, starts: &[f64], now: f64, claude: f64) -> Why {
    let Some(at) = news.at.filter(|_| news.story) else {
        return Why::NoStory;
    };
    if !news.has_transcript {
        return Why::NoTranscript;
    }
    if news.receipts < NEWS_RECEIPTS || news.prompts_since < NEWS_PROMPTS {
        return Why::NoNews;
    }
    if starts.iter().any(|s| now - s < COOLDOWN_MS) || now - at * 1000. < COOLDOWN_MS {
        return Why::Cooldown;
    }
    let recent = starts.iter().filter(|s| now - **s < CHAPTER_MS).count() as f64;
    let left = allowance(news.liked_2h) - recent;
    if left <= 0. {
        return Why::ChapterCap;
    }
    if news.messages_since < messages_due(news.receipts, left) {
        return Why::Spacing;
    }
    if claude > 0. {
        let next = if news.last_tokens > 0. {
            news.last_tokens
        } else {
            FIRST_GUESS
        };
        if news.tokens_2h + next > STORY_SHARE * claude {
            return Why::Share;
        }
    }
    // Spend that cannot be known is not free: an unpriced model, or a ledger that could not be
    // read, holds the refresh back.
    let Some(usd) = news.usd_24h else {
        return Why::SpendUnknown;
    };
    if news.calls_24h >= DAY_CALLS || usd >= DAY_USD {
        return Why::DayCap;
    }
    Why::Go
}

/// The refresh as an argv list: csr-engine tells this conversation's story again, and skips it
/// itself when nothing was asked since the last telling.
pub fn refresh_argv(transcript: &str, cwd: &str) -> Vec<String> {
    let argv = [
        "csr-engine",
        "generate-story",
        "--transcript",
        transcript,
        "--cwd",
        cwd,
        "--rewrite",
    ];
    argv.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_000_000_000.;

    /// A telling an hour old with the news for a refresh, a quiet day, and the room to spend.
    fn news() -> Facts {
        Facts {
            story: true,
            at: Some(NOW / 1000. - 3600.),
            messages_since: 80.,
            prompts_since: 3.,
            receipts: 6.,
            usd_24h: Some(0.),
            has_transcript: true,
            ..Facts::default()
        }
    }

    #[test]
    fn news_that_is_not_a_story_is_no_story() {
        assert_eq!(parse_news(None), Facts::default());
        assert_eq!(parse_news(Some("")), Facts::default());
        assert_eq!(parse_news(Some("Traceback")), Facts::default());
        assert_eq!(parse_news(Some(r#"{"story":false}"#)), Facts::default());
        let said = r#"{"story":true,"at":5,"receipts":"x","transcript":"/t.jsonl","usd_24h":null}"#;
        let got = parse_news(Some(said));
        assert_eq!(
            (got.story, got.at, got.receipts, got.usd_24h),
            (true, Some(5.), 0., None)
        );
        assert!(got.has_transcript);
    }

    #[test]
    fn the_first_gate_that_fails_is_the_reason_in_the_gates_order() {
        let go = |news: &Facts| decide(news, &[], NOW, 0.);
        assert_eq!(go(&news()), Why::Go);
        assert_eq!(go(&Facts::default()), Why::NoStory);
        assert_eq!(go(&Facts { at: None, ..news() }), Why::NoStory);
        let no_transcript = Facts {
            has_transcript: false,
            ..news()
        };
        assert_eq!(go(&no_transcript), Why::NoTranscript);
        assert_eq!(
            go(&Facts {
                receipts: 3.,
                ..news()
            }),
            Why::NoNews
        );
        assert_eq!(
            go(&Facts {
                prompts_since: 1.,
                ..news()
            }),
            Why::NoNews
        );
        let fresh = Facts {
            at: Some(NOW / 1000. - 30.),
            ..news()
        };
        assert_eq!(go(&fresh), Why::Cooldown);
        assert_eq!(
            go(&Facts {
                messages_since: 20.,
                ..news()
            }),
            Why::Spacing
        );
        assert_eq!(
            go(&Facts {
                usd_24h: None,
                ..news()
            }),
            Why::SpendUnknown
        );
        assert_eq!(
            go(&Facts {
                calls_24h: 40.,
                ..news()
            }),
            Why::DayCap
        );
        assert_eq!(
            go(&Facts {
                usd_24h: Some(0.1),
                ..news()
            }),
            Why::DayCap
        );
    }

    #[test]
    fn a_refresh_started_a_minute_ago_waits_and_a_full_chapter_stops() {
        let recent = [NOW - 30_000.];
        assert_eq!(decide(&news(), &recent, NOW, 0.), Why::Cooldown);
        let three = [NOW - 3_000_000., NOW - 2_000_000., NOW - 1_000_000.];
        assert_eq!(decide(&news(), &three, NOW, 0.), Why::ChapterCap);
        let liked = Facts {
            liked_2h: 2.,
            ..news()
        };
        assert_eq!(
            decide(&liked, &three, NOW, 0.),
            Why::Go,
            "2 liked earn 2 more"
        );
        let old = [NOW - CHAPTER_MS - 1., NOW - CHAPTER_MS - 2.];
        assert_eq!(
            decide(&news(), &old, NOW, 0.),
            Why::Go,
            "past the chapter: not counted"
        );
    }

    #[test]
    fn the_share_gate_compares_spend_and_the_guess_with_two_in_a_hundred_of_claude() {
        let spent = Facts {
            tokens_2h: 3000.,
            last_tokens: 1000.,
            ..news()
        };
        assert_eq!(
            decide(&spent, &[], NOW, 200_000.),
            Why::Go,
            "4000 is exactly 2%"
        );
        assert_eq!(decide(&spent, &[], NOW, 199_000.), Why::Share);
        assert_eq!(
            decide(&news(), &[], NOW, 70_000.),
            Why::Share,
            "no last call: 1600"
        );
        assert_eq!(decide(&news(), &[], NOW, 80_000.), Why::Go);
        assert_eq!(
            decide(&spent, &[], NOW, 0.),
            Why::Go,
            "Claude not counted: no share"
        );
    }

    #[test]
    fn the_allowance_and_the_messages_due_follow_the_news() {
        assert_eq!(allowance(0.), 3.);
        assert_eq!(allowance(2.), 5.);
        assert_eq!(allowance(99.), 6.);
        assert_eq!(allowance(-4.), 3.);
        assert_eq!(messages_due(4., 3.), 40.); // 80 / 3 * 6 / 4
        assert_eq!(messages_due(12., 3.), 13.); // 80 / 3 * 0.5
        assert_eq!(messages_due(40., 6.), 12., "never under 12");
        assert_eq!(
            messages_due(0., 0.),
            480.,
            "no receipts and no refresh left count as 1"
        );
    }

    #[test]
    fn the_refresh_is_the_engines_own_generate_story_with_the_rewrite_flag() {
        assert_eq!(
            refresh_argv("/t/a.jsonl", "/repo"),
            [
                "csr-engine",
                "generate-story",
                "--transcript",
                "/t/a.jsonl",
                "--cwd",
                "/repo",
                "--rewrite"
            ]
        );
        assert_eq!(Why::DayCap.name(), "day-cap");
        assert_eq!(Why::Go.name(), "go");
    }

    #[test]
    fn the_engines_own_news_reads_as_facts() {
        assert_eq!(Facts::of(&News::Absent { story: false }), Facts::default());
        let present = News::Present(StoryNews {
            story: true,
            at: 7.,
            covered: Some(3),
            messages: 90,
            messages_since: 80,
            prompts_since: 3,
            receipts: 6,
            spend: Spend {
                calls_24h: 2,
                usd_24h: Some(0.02),
                tokens_2h: 900,
                last_tokens: Some(450),
            },
            liked_2h: 1,
            transcript: Some("/t.jsonl".into()),
        });
        let facts = Facts::of(&present);
        assert_eq!(
            (facts.at, facts.receipts, facts.last_tokens),
            (Some(7.), 6., 450.)
        );
        assert!(facts.has_transcript && facts.story);
    }
}
