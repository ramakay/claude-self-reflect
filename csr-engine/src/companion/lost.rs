use std::sync::LazyLock;
static NUMBERED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"^(?:#+\s*)?\**(\d+)\.\s+\**[A-Z][A-Za-z /&,'-]{2,50}\**:?\**\s*$"#)
        .unwrap()
});
static BULLET: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"^(\s*)(?:[-*•]|\d+[.)])\s+(.*)$"#).unwrap());
static WAITING: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r#"(?i)\b(awaiting|waiting (?:on|for) (?:you|the user)|wait for the user|needs?"#,
        r#" (?:your|their|the user'?s) (?:go|ok|approval|call|decision)|approval|the us"#,
        r#"er'?s (?:answer|go|pick|picks|decision|decisions|call|pasted)|the user needs"#,
        r#"? to|asks? the user)\b"#,
    ))
    .unwrap()
});
static OWED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r#"(?i)\b(wait|waiting|await|awaiting|confirm|verify|check|update|run|fix|build"#,
        r#"|built|install|merge|push|pushed|publish|ask|decide|need|needs|needing|not ("#,
        r#"?:yet )?(?:started|built|applied|pushed|done)|pending|todo|next|after|then|b"#,
        r#"efore|still|should|must|owed|deferred|proposal|proposals|proposed|open quest"#,
        r#"ion|without|never)\b"#,
    ))
    .unwrap()
});
static BOOKKEEPING: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r#"(?i)^(?:(?:send|write|give|post|deliver)\b.{0,40}\b(?:final|brief|short|sett"#,
        r#"le)\b.{0,30}\b(?:reply|report|summary|message)|(?:brief|short|final)\b.{0,20"#,
        r#"}\b(?:reply|report|summary)|report\b.*\bto the user|(?:tell|let) the user\b|"#,
        r#"report\b|confirm the background|responding to\b|(?:the )?messages after this"#,
        r#" summary)"#,
    ))
    .unwrap()
});
static APPROVED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r#"(?i)^\W*(?:go|yes|yep|ok|okay|install|proceed|approved?|do it|ship it|push i"#,
        r#"t)\b|\bis a go\b"#,
    ))
    .unwrap()
});
static REMINDERS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(?i)reminders from csr"#).unwrap());
static HANDED_OFF: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?s)\s*Lost and found: “(.+)” went quiet after \d{1,2}:\d\d\.(.*)"#)
        .unwrap()
});
static ASKS_YOU: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(concat!(
        r#"(?i)\?\s*$|\b(?:say go|when you say|your go|needs? your|want me to|should i|"#,
        r#"shall i|do you want|which (?:one|do you)|approve|your call|your pick)\b"#,
    ))
    .unwrap()
});
use super::{
    agents, array, num,
    paths::{self, Paths},
    recall,
    render::text,
    session::Session,
    string,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub text: String,
    pub detail: Vec<String>,
    pub first_seen: f64,
    pub last_seen: f64,
    pub times: usize,
    pub state: String,
    pub waiting: bool,
    pub rank: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    pub key: String,
    #[serde(default)]
    pub hidden: Option<String>,
    #[serde(default)]
    pub later: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub came_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handed_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlinked: Option<bool>,
}
fn plain(s: &str) -> String {
    s.replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn stem(word: &str) -> &str {
    for end in ["ing", "ed", "es", "s"] {
        if word.len() > end.len() + 3 && word.ends_with(end) {
            return &word[..word.len() - end.len()];
        }
    }
    word
}
pub fn words_of(s: &str) -> BTreeSet<String> {
    static WORD: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new("[a-z0-9_]+").unwrap());
    const IGNORED: &str = "the and for with then that this when its are was from into not has \
        have will any all one new now yet but you your user";
    WORD.find_iter(&s.to_lowercase())
        .map(|m| m.as_str())
        .filter(|s| s.len() > 2 && !IGNORED.split_whitespace().any(|w| w == *s))
        .map(|s| stem(s).to_owned())
        .collect()
}
pub fn same_words(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let shared = a.intersection(b).count();
    shared as f64 / a.union(b).count() as f64 >= 0.5
        || (shared >= 3 && shared as f64 / a.len().min(b.len()) as f64 >= 0.6)
}
fn replies_to(s: &str, words: &BTreeSet<String>) -> bool {
    const GENERIC: &str = "session claude pane csr mod code make need thing work just like \
        what \
        should also more some here there them they it can did does how out see get got let use \
        way \
        go yes ok okay please thank thanks good install run push merge build fix check \
        verify update \
        approval approve deploy commit test start stop add remove open close send ship \
        proceed await \
        wait user next now then done with before after still needs need";
    words_of(s)
        .intersection(words)
        .any(|w| w.len() > 3 && !GENERIC.split_whitespace().any(|g| g == w))
}
pub fn summary_section(text: &str, name: &str) -> Vec<String> {
    let (mut found, mut number) = (false, 0);
    let mut out = vec![];
    let section_names = [
        "primary request and intent",
        "key technical concepts",
        "files and code sections",
        "errors and fixes",
        "problem solving",
        "all user messages",
        "pending tasks",
        "current work",
        "optional next step",
    ];
    for line in text.lines() {
        if found && REMINDERS.is_match(line) {
            break;
        }
        let m = if line.starts_with(' ') {
            None
        } else {
            NUMBERED.captures(line.trim())
        };
        if !found {
            if let Some(m) = m {
                if line.to_lowercase().contains(&name.to_lowercase()) {
                    found = true;
                    number = m[1].parse().unwrap_or(0);
                }
            }
            continue;
        }
        if let Some(m) = m {
            let title = line.trim().trim_start_matches('#').trim().replace('*', "");
            let title = title
                .split_once(". ")
                .map(|(_, s)| s)
                .unwrap_or(&title)
                .trim();
            if m[1].parse::<usize>().unwrap_or(0) > number
                && (title.ends_with(':') || section_names.contains(&title.to_lowercase().as_str()))
            {
                break;
            }
        }
        out.push(line.to_owned());
    }
    out
}
pub fn section_items(lines: &[String]) -> Vec<(String, Vec<String>)> {
    let top = lines
        .iter()
        .filter_map(|l| BULLET.captures(l).map(|c| c[1].chars().count()))
        .min();
    let Some(top) = top else {
        static PARAGRAPHS: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"\n\s*\n").unwrap());
        return PARAGRAPHS
            .split(&lines.join("\n"))
            .map(plain)
            .filter(|s| !s.is_empty())
            .map(|s| (s, vec![]))
            .collect();
    };
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for line in lines {
        if let Some(m) = BULLET.captures(line) {
            if m[1].chars().count() <= top {
                out.push((m[2].to_owned(), vec![]));
            } else if let Some((_, nested)) = out.last_mut() {
                nested.push(m[2].to_owned());
            }
        } else if !line.trim().is_empty() {
            if let Some((head, nested)) = out.last_mut() {
                let tail = nested.last_mut().unwrap_or(head);
                tail.push(' ');
                tail.push_str(line.trim());
            }
        }
    }
    out.into_iter()
        .map(|(h, n)| (plain(&h), n.iter().map(|s| plain(s)).collect()))
        .collect()
}
/// Claude Code's own closing paragraph of a compaction summary ("If you need specific details
/// from before compaction ... read the full transcript at: ..."). It is not something owed.
pub fn is_summary_footer(text: &str) -> bool {
    text.to_lowercase()
        .contains("specific details from before compaction")
}
pub fn summary_items(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out = vec![];
    for name in ["Pending Tasks", "Optional Next Step"] {
        for (mut head, mut nested) in section_items(&summary_section(text, name)) {
            if head.ends_with(':') && !nested.is_empty() {
                head.push(' ');
                head.push_str(
                    &nested
                        .iter()
                        .take(2)
                        .map(|s| s.trim_end_matches([';', '.']))
                        .collect::<Vec<_>>()
                        .join("; "),
                );
                nested.drain(..nested.len().min(2));
            }
            if !head.is_empty()
                && !is_summary_footer(&head)
                && !BOOKKEEPING.is_match(&head)
                && OWED.is_match(&head)
            {
                out.push((head, nested));
            }
        }
    }
    out
}
pub fn pairs(value: &Value) -> Vec<(f64, String)> {
    array(value)
        .iter()
        .map(|r| (num(&r[0]), string(&r[1]).into()))
        .collect()
}
pub fn lost_threads(summaries: &[(f64, String)], typed: &[(f64, String)]) -> Vec<Thread> {
    struct Working {
        row: Thread,
        words: BTreeSet<String>,
        first_words: String,
        carried: HashSet<usize>,
    }
    let mut summaries = summaries.to_vec();
    summaries.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut threads: Vec<Working> = vec![];
    for (index, (when, text)) in summaries.iter().enumerate() {
        for (head, nested) in summary_items(text) {
            let words = words_of(&head);
            let score = |t: &Working| {
                let union = words.union(&t.words).count();
                if union == 0 {
                    0.
                } else {
                    words.intersection(&t.words).count() as f64 / union as f64
                }
            };
            let best = threads
                .iter()
                .enumerate()
                .max_by(|(i, a), (j, b)| score(a).total_cmp(&score(b)).then(j.cmp(i)))
                .map(|(i, _)| i)
                .filter(|i| same_words(&words, &threads[*i].words));
            let i = best.unwrap_or_else(|| {
                threads.push(Working {
                    row: Thread {
                        first_seen: *when,
                        ..Thread::default()
                    },
                    first_words: words.iter().cloned().collect::<Vec<_>>().join(" "),
                    words: words.clone(),
                    carried: HashSet::new(),
                });
                threads.len() - 1
            });
            let t = &mut threads[i];
            t.row.text = head;
            t.row.detail = nested;
            t.row.last_seen = *when;
            t.words = words;
            t.carried.insert(index);
        }
    }
    let mut typed = typed.to_vec();
    typed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out = vec![];
    for mut t in threads {
        let row = &mut t.row;
        let open = t.carried.iter().max().copied() == summaries.len().checked_sub(1);
        row.state = if open { "open" } else { "lost" }.into();
        row.waiting = WAITING.is_match(&format!("{} {}", row.text, row.detail.join(" ")));
        let said = typed
            .iter()
            .filter(|(when, words)| *when > row.last_seen && !HANDED_OFF.is_match(words))
            .take(2)
            .find(|(_, words)| APPROVED.is_match(words) && replies_to(words, &t.words));
        row.why = Some(if open {
            "in the newest summary".into()
        } else {
            format!("quiet since {}", recall::date(row.last_seen, "%m-%d %H:%M"))
        });
        if let Some((when, words)) = said.filter(|_| !open && row.waiting) {
            row.state = "answered".into();
            row.why = Some(format!(
                "guessed done {}: you said “{}”",
                recall::date(*when, "%m-%d %H:%M"),
                text::fit(&text::one_line(words, 700), 60)
            ));
        } else if !open && !row.waiting && t.carried.len() < 2 {
            row.state = "faded".into();
            row.why = Some("in one summary, asked you nothing".into());
        }
        row.rank = match row.state.as_str() {
            "lost" => usize::from(!row.waiting),
            "open" => 2 + usize::from(!row.waiting),
            "faded" => 3,
            _ => 4,
        };
        row.times = t.carried.len();
        use sha1::Digest;
        row.key = sha1::Sha1::digest(t.first_words.as_bytes())
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect();
        out.push(t.row);
    }
    sort_threads(&mut out);
    out
}
fn sort_threads(threads: &mut [Thread]) {
    threads.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then(b.last_seen.total_cmp(&a.last_seen))
    });
}
pub fn lost_log(paths: &Paths, project: Option<&str>) -> HashMap<String, (String, f64)> {
    let mut said = HashMap::new();
    super::tail::scan(&paths.keep.join("lost-found.jsonl"), |r| {
        if (r["project"].is_null() || r["project"].as_str() == project)
            && ["drop", "later"].contains(&string(&r["answer"]))
        {
            if let (Some(key), Some(at)) = (r["key"].as_str(), r["at"].as_f64()) {
                said.insert(key.into(), (string(&r["answer"]).into(), at));
            }
        }
    });
    said
}
fn handed_why(at: f64, how: &str) -> String {
    let how = match how {
        "sub" => "to a subagent",
        "side" => "as a side task",
        _ => "to Claude",
    };
    format!("handed {how} {}", recall::date(at, "%H:%M"))
}
pub fn lost_found(
    paths: &Paths,
    summaries: &[(f64, String)],
    typed: &[(f64, String)],
    carried: &[Thread],
    project: Option<&str>,
    handoffs: &[agents::Handoff],
) -> Vec<Thread> {
    let mut threads = lost_threads(summaries, typed);
    let mut said = lost_log(paths, project);
    let own = threads.len();
    for c in carried {
        if let Some(matched) = threads[..own]
            .iter()
            .find(|t| same_words(&words_of(&c.text), &words_of(&t.text)))
        {
            if let Some(answer) = said.get(&c.key).cloned() {
                said.entry(matched.key.clone()).or_insert(answer);
            }
        } else {
            threads.push(c.clone());
        }
    }
    sort_threads(&mut threads);
    let mut handed = HashMap::new();
    for (when, text) in typed {
        let Some(m) = HANDED_OFF.captures(text) else {
            continue;
        };
        let quoted = words_of(m[1].trim_end_matches('…'));
        let best = threads
            .iter()
            .enumerate()
            .filter(|(_, t)| t.first_seen < *when && same_words(&quoted, &words_of(&t.text)))
            .max_by(|(i, a), (j, b)| {
                let score = |t: &Thread| {
                    let w = words_of(&t.text);
                    quoted.intersection(&w).count() as f64 / quoted.union(&w).count() as f64
                };
                score(a).total_cmp(&score(b)).then(j.cmp(i))
            });
        if let Some((i, _)) = best {
            let how = if m[2].contains("background subagent") {
                "sub"
            } else if m[2].contains("side task") {
                "side"
            } else {
                ""
            };
            handed.insert(i, (*when, how));
        }
    }
    let newest = summaries.iter().map(|r| r.0).fold(0., f64::max);
    for (i, t) in threads.iter_mut().enumerate() {
        let (answer, at) = said
            .get(&t.key)
            .map(|(a, t)| (a.as_str(), *t))
            .unwrap_or(("", 0.));
        t.hidden =
            (answer == "drop" || (answer == "later" && at > newest)).then(|| answer.to_owned());
        t.later = answer == "later";
        let requests: Vec<_> = handoffs
            .iter()
            .filter(|h| h.thread == t.key)
            .cloned()
            .collect();
        if let Some((word, h)) = agents::decide(&requests).filter(|_| answer != "drop") {
            let at = h.submitted_at.filter(|n| *n != 0.).unwrap_or(h.drafted_at);
            match word {
                "handed" => {
                    t.hidden = Some("handed".into());
                    t.state = "handed".into();
                    t.why = Some(
                        handed_why(at, "sub")
                            + if h.outcome == "succeeded" {
                                " · it reported"
                            } else {
                                ""
                            },
                    );
                }
                "failed" => {
                    t.hidden = None;
                    t.later = false;
                    t.state = "lost".into();
                    t.rank = usize::from(!t.waiting);
                    t.handed_at = Some(at);
                    t.failed_at = h.ended_at;
                    t.why = Some(format!(
                        "{}, which failed {}: {}",
                        handed_why(at, "sub"),
                        h.ended_at
                            .map(|t| recall::date(t, "%m-%d %H:%M"))
                            .unwrap_or_default(),
                        h.reason
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("no reason")
                    ));
                }
                _ => {
                    t.unlinked = Some(true);
                    t.why = Some("handoff not linked".into());
                    if let Some(reason) = h.reason.as_ref().filter(|s| !s.is_empty()) {
                        t.why.as_mut().unwrap().push_str(&format!(" ({reason})"));
                    }
                }
            }
        } else if answer != "drop" && requests.is_empty() {
            if let Some((at, how)) = handed.get(&i) {
                t.hidden = Some("handed".into());
                t.state = "handed".into();
                t.why = Some(handed_why(*at, how));
            }
        }
    }
    threads
}
pub fn lost_json(paths: &Paths, sid: &str) -> Vec<Thread> {
    let mut session = Session::new(sid);
    if !sid.is_empty() {
        session.read_transcript(paths);
    }
    let carried = paths
        .kept_file("carried", sid)
        .map(|p| paths::read_json(&p))
        .and_then(|v| serde_json::from_value::<Vec<Thread>>(v).ok())
        .unwrap_or_default();
    let project = session
        .path
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str());
    lost_found(
        paths,
        &pairs(&session.data["summaries"]),
        &pairs(&session.data["typed"]),
        &carried,
        project,
        &agents::handoffs_of(paths, sid),
    )
    .into_iter()
    .filter(|t| !matches!(t.hidden.as_deref(), Some("drop" | "handed")))
    .collect()
}
use std::path::Path;

pub fn last_ask(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text
        .chars()
        .skip(text.chars().count().saturating_sub(900))
        .collect();
    let mut start = 0;
    let mut out = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() && text[..i].ends_with(['.', '?', '!']) {
            let sentence = text[start..i].trim();
            if ASKS_YOU.is_match(sentence) {
                out = Some(sentence.to_owned());
            }
            start = i;
        }
    }
    let sentence = text[start..].trim();
    if ASKS_YOU.is_match(sentence) {
        out = Some(sentence.to_owned());
    }
    out
}
pub fn stopped_at(prev: &Session) -> Option<Thread> {
    let p = &prev.data;
    let since = array(&p["summaries"])
        .last()
        .map(|r| num(&r[0]))
        .unwrap_or(0.);
    let typed: Vec<_> = pairs(&p["typed"])
        .into_iter()
        .filter(|(_, words)| !HANDED_OFF.is_match(words))
        .collect();
    let after: Vec<_> = typed
        .iter()
        .filter(|(when, s)| *when > since && !s.trim().is_empty())
        .collect();
    let last = typed.last();
    let said = p["said"]
        .as_array()
        .map(|_| (num(&p["said"][0]), string(&p["said"][1])));
    let ask = said.and_then(|(_, s)| last_ask(s));
    let (text, waiting, rank) =
        if let Some((at, s)) = last.filter(|(at, _)| *at > since && *at > num(&p["turn_end"])) {
            let _ = at;
            (
                format!(
                    "Cut off before Claude finished answering: “{}”",
                    text::fit(&text::one_line(s, 700), 200)
                ),
                true,
                0,
            )
        } else if let Some(ask) = ask
            .filter(|_| said.is_some_and(|(at, _)| at > since && last.is_none_or(|(t, _)| at > *t)))
        {
            (
                format!(
                    "Claude asked last, no answer came: “{}”",
                    text::fit(&ask, 200)
                ),
                true,
                0,
            )
        } else if let Some((_, s)) = after.last() {
            (
                format!(
                    "Last asked before it ended: “{}”",
                    text::fit(&text::one_line(s, 700), 200)
                ),
                false,
                3,
            )
        } else {
            return None;
        };
    let mut detail: Vec<_> = after
        .iter()
        .skip(after.len().saturating_sub(3))
        .map(|(t, s)| {
            format!(
                "{} you: {}",
                recall::date(*t, "%m-%d %H:%M"),
                text::fit(&text::one_line(s, 700), 160)
            )
        })
        .collect();
    if let Some((t, s)) = said {
        detail.push(format!(
            "{} Claude: {}",
            recall::date(t, "%m-%d %H:%M"),
            text::fit(&text::one_line(s, 700), 200)
        ));
    }
    let ended = said
        .map(|r| r.0)
        .unwrap_or(0.)
        .max(last.map(|r| r.0).unwrap_or(0.));
    let from: String = prev.id.chars().take(8).collect();
    use sha2::Digest;
    Some(Thread {
        text,
        detail,
        first_seen: after.first().map(|r| r.0).unwrap_or(ended),
        last_seen: ended,
        state: "carried".into(),
        waiting,
        rank,
        came_from: Some(from.clone()),
        why: Some(format!(
            "session {from}, after its last summary; ended {}",
            recall::date(ended, "%m-%d %H:%M")
        )),
        key: sha2::Sha256::digest(format!("stopped {}", prev.id))
            .iter()
            .take(6)
            .map(|b| format!("{b:02x}"))
            .collect(),
        ..Thread::default()
    })
}
pub fn carried_threads(paths: &Paths, prev: &Session) -> Vec<Thread> {
    let before = prev.path.as_deref();
    let project = before
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str());
    let said = lost_log(paths, project);
    let carried: Vec<Thread> = paths
        .kept_file("carried", &prev.id)
        .map(|p| paths::read_json(&p))
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut out = vec![];
    for mut t in lost_found(
        paths,
        &pairs(&prev.data["summaries"]),
        &pairs(&prev.data["typed"]),
        &carried,
        project,
        &agents::handoffs_of(paths, &prev.id),
    ) {
        if matches!(t.hidden.as_deref(), Some("drop" | "handed"))
            || said.get(&t.key).is_some_and(|(a, _)| a == "drop")
        {
            continue;
        }
        if matches!(t.state.as_str(), "open" | "lost") {
            t.why = Some(format!(
                "session {}, {} when it ended",
                prev.id.chars().take(8).collect::<String>(),
                if t.state == "open" { "open" } else { "quiet" }
            ));
            t.state = "carried".into();
            t.rank = usize::from(!t.waiting);
            t.came_from = Some(prev.id.chars().take(8).collect());
            out.push(t);
        } else if t.state == "carried" {
            t.hidden = None;
            t.later = false;
            out.push(t);
        }
    }
    if let Some(tail) = stopped_at(prev) {
        out.push(tail);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compaction_summary_footer_is_never_a_thread() {
        let footer = "If you need specific details from before compaction (like exact code \
                      snippets, error messages, or content you generated), read the full \
                      transcript at: /tmp/a.jsonl";
        assert!(is_summary_footer(footer));
        assert!(!is_summary_footer("Still need to push the branch"));
        let summary = format!(
            "6. Pending Tasks:\n   - Still need to push the branch\n\n7. Optional Next Step:\n\n{footer}\n"
        );
        let heads: Vec<_> = summary_items(&summary)
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(heads, vec!["Still need to push the branch".to_owned()]);
    }
}
