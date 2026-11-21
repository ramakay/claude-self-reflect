//! The handoff for a thread that went quiet (the Lost and found popup): what CSR holds about the
//! conversation the thread came from, shown in the popup and written as the prompt a fresh Claude
//! Code session is given.
//!
//! Every line is read from a stored row or a file: the summary from the episode CSR stored, the
//! numbers from the transcript through `crate::transcript`, the places from the disk and the
//! `transcript_mirror` table. What is missing is said in words, never left out. The prompt is the
//! person's to send, so it is not CSR's emission: it must import like any other prompt (see the
//! test), and it carries no dream marker because no dream produced it. Most of its words are
//! CSR's, though, so it ends with one source tag, `↳ csr-handoff <conversation id>`
//! (`storage::handoff_marker`): search labels the stored paste, and a derived artifact drops
//! everything but the ask and what the person typed around it.
use super::{
    db::Db,
    num,
    paths::{self, Paths},
    recall, string,
};
use crate::journal::composer::{push_stated_section, EpisodeFacts};
use crate::storage::handoff_marker::{
    self, HANDOFF_ASK_HEADER, HANDOFF_LEAD, HANDOFF_NUMBERS_TITLE, HANDOFF_OPENING,
    HANDOFF_PLACES_TITLE, HANDOFF_READ_TITLE, HANDOFF_SUMMARY_TITLE, HANDOFF_THREAD_TITLE,
    MAX_WHOLE_DRAFT_BYTES,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Most characters of one summary field the popup and the prompt carry.
const FIELD_CHARS: usize = 700;

/// The rule a fresh session is given, word for word.
const RULE: &str = "CSR's rule: read a session through the csr_transcript tool, never by \
                    hand-rolling jq or Python over the raw JSONL.";

/// One headed block of the popup.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Section {
    pub heading: &'static str,
    pub lines: Vec<String>,
}

/// The popup's content for one thread.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct View {
    pub title: String,
    /// One line under the heading: when it went quiet and how it got here.
    pub kicker: String,
    pub sections: Vec<Section>,
    /// The transcript is still being read: the numbers are not there yet.
    pub reading: bool,
    /// The session's shape (Tools, Context, Other shares and the verdict word), when measured:
    /// the picture draws it as the Context card's donut on the "Shape:" line.
    pub mix: Option<([f64; 3], &'static str)>,
}

/// What a session is made of, from the byte counts `session_facts` made in its one pass, read
/// by the same shares, verdict words and half threshold as the Context card's donut.
fn shape_of(stats: &Value) -> Option<([f64; 3], &'static str)> {
    stats.is_object().then_some(())?;
    super::render::instrument::session_mix_of(
        num(&stats["tool_bytes"]),
        num(&stats["context_bytes"]),
        num(&stats["other_bytes"]),
    )
}

/// The one "Shape:" line of the popup and the draft.
pub const SHAPE_PREFIX: &str = "Shape: ";

fn shape_line(stats: &Value) -> String {
    match shape_of(stats) {
        Some((shares, verdict)) => format!(
            "{SHAPE_PREFIX}{verdict} (Tools {:.0}%, Context {:.0}%, Other {:.0}%)",
            shares[0] * 100.,
            shares[1] * 100.,
            shares[2] * 100.
        ),
        None => format!("{SHAPE_PREFIX}not measured"),
    }
}

/// A `LIKE` pattern that matches `text` literally: `_` and `%` are wildcards, and ids hold `_`.
fn like_literal(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// The session id a transcript path (relative to the projects folder) belongs to:
/// `project/<sid>.jsonl` and `project/<sid>/subagents/agent-x.jsonl` are both session `<sid>`.
fn session_of(rel: &str) -> Option<(&str, &str)> {
    let mut parts = rel.split('/');
    let project = parts.next().filter(|p| !p.is_empty())?;
    let second = parts.next().filter(|p| !p.is_empty())?;
    Some((project, second.strip_suffix(".jsonl").unwrap_or(second)))
}

/// Where a thread's conversation is: its id, its transcript path relative to the projects folder
/// (the first component is the project), and what could not be settled.
#[derive(Default)]
struct Origin {
    sid: Option<String>,
    rel: Option<String>,
    /// The eight-character prefix of a thread that names no full id and matches more than one
    /// conversation, live or archived: none is chosen.
    ambiguous: Option<String>,
    /// More than one mirror row could be this conversation's: none is claimed.
    mirror_ambiguous: bool,
}

fn rel_of(paths: &Paths, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(&paths.projects).ok()?;
    let parts: Vec<_> = rel.iter().filter_map(|p| p.to_str()).collect();
    (parts.len() >= 2).then(|| parts.join("/"))
}

/// Every conversation whose id starts with `prefix`: a live transcript or session folder, or a
/// mirror row (an archived one). Keyed by id, each with the main transcript's relative path.
fn prefix_candidates(
    paths: &Paths,
    db: Option<&Db>,
    prefix: &str,
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let mut found: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        Default::default();
    for dir in paths::folders(&paths.projects) {
        let Some(project) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        for entry in paths::folders(&dir) {
            let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let id = name.strip_suffix(".jsonl").unwrap_or(name);
            let session = entry.is_dir() || name.ends_with(".jsonl");
            if session && id.starts_with(prefix) && paths::valid_id(id) {
                found
                    .entry(id.to_owned())
                    .or_default()
                    .insert(format!("{project}/{id}.jsonl"));
            }
        }
    }
    if let Some(db) = db {
        for row in db.query(
            "SELECT rel_path FROM transcript_mirror WHERE rel_path LIKE ?1 ESCAPE '\\'",
            &[json!(format!("%/{}%", like_literal(prefix)))],
        ) {
            if let Some((project, id)) = session_of(string(&row[0])) {
                if id.starts_with(prefix) && paths::valid_id(id) {
                    found
                        .entry(id.to_owned())
                        .or_default()
                        .insert(format!("{project}/{id}.jsonl"));
                }
            }
        }
    }
    found
}

fn resolve(paths: &Paths, db: Option<&Db>, item: &Value) -> Origin {
    let mut out = Origin {
        sid: Some(string(&item["origin_sid"]))
            .filter(|s| paths::valid_id(s))
            .map(str::to_owned),
        ..Origin::default()
    };
    // The path kept with the thread names its project and file, whether or not it is still there.
    let kept = string(&item["origin_path"]);
    if !kept.is_empty() {
        out.rel = rel_of(paths, Path::new(kept));
    }
    // A thread kept before it named its conversation holds only the first eight characters.
    if out.sid.is_none() {
        let prefix = string(&item["came_from"]);
        if prefix.len() >= 6 && paths::valid_id(prefix) {
            let found = prefix_candidates(paths, db, prefix);
            let mut ids = found.iter();
            match (ids.next(), ids.next()) {
                (Some((id, mains)), None) if mains.len() == 1 => {
                    out.sid = Some(id.clone());
                    out.rel = out.rel.take().or_else(|| mains.iter().next().cloned());
                }
                (None, _) => (),
                _ => out.ambiguous = Some(prefix.to_owned()),
            }
        }
        return out;
    }
    if out.rel.is_none() {
        let sid = out.sid.clone().unwrap_or_default();
        if let Some(live) = paths.transcript(&sid) {
            out.rel = rel_of(paths, &live);
        } else if let Some(db) = db {
            let rows = db.query(
                "SELECT rel_path FROM transcript_mirror WHERE rel_path LIKE ?1 ESCAPE '\\'",
                &[json!(format!("%/{}.jsonl", like_literal(&sid)))],
            );
            // `project/<sid>.jsonl` only: a subagent run's own file is not the session's.
            let mains: Vec<_> = rows
                .iter()
                .map(|r| string(&r[0]).to_owned())
                .filter(|r| {
                    session_of(r).is_some_and(|(_, id)| id == sid) && r.matches('/').count() == 1
                })
                .collect();
            match mains.len() {
                0 => (),
                1 => out.rel = mains.into_iter().next(),
                _ => out.mirror_ambiguous = true,
            }
        }
    }
    out
}

fn size(bytes: f64) -> String {
    if bytes >= 1_048_576. {
        format!("{:.1} MB", bytes / 1_048_576.)
    } else {
        format!("{:.0} KB", (bytes / 1024.).max(1.))
    }
}

fn span(secs: f64) -> String {
    let (h, m) = (
        (secs / 3600.).floor() as u64,
        ((secs % 3600.) / 60.).floor() as u64,
    );
    match (h, m) {
        (0, 0) => "under a minute".into(),
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// What CSR holds about the conversation `item` went quiet in. Reads the disk, the database and
/// the transcript: it belongs off the draw path. `{conv, project, path, path_exists, mirror,
/// summary, stats, stats_note}`; a part that cannot be read is null.
pub fn gather(paths: &Paths, item: &Value) -> Value {
    let db = Db::open(paths).ok();
    let origin = resolve(paths, db.as_ref(), item);
    let sid = origin.sid.clone();
    let rel = origin.rel.clone();
    let path = rel.as_ref().map(|r| paths.projects.join(r)).or_else(|| {
        // a kept path outside the projects folder is shown as it was kept
        Some(string(&item["origin_path"]))
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
    });
    let project = rel
        .as_deref()
        .and_then(|r| r.split('/').next())
        .map(str::to_owned);
    // The mirror row is looked up by the exact relative path, never by a suffix of it.
    let row: Option<Vec<Value>> = rel.as_ref().zip(db.as_ref()).and_then(|(rel, db)| {
        db.query(
            "SELECT rel_path, source_bytes, zst_bytes, frames, sealed, updated_at, \
             source_gone_at FROM transcript_mirror WHERE rel_path = ?1",
            &[json!(rel)],
        )
        .into_iter()
        .next()
    });
    let data_dir = paths.db.parent().map(Path::to_path_buf).unwrap_or_default();
    let zst = rel
        .as_ref()
        .map(|r| crate::mirror::transcripts_dir(&data_dir).join(format!("{r}.zst")));
    let mirror = zst.as_ref().map(|z| {
        json!({
            "zst": z.display().to_string(),
            "exists": z.is_file(),
            "row": row.as_ref().map(|r| json!({
                "source_bytes": r[1], "zst_bytes": r[2], "frames": r[3],
                "sealed": r[4], "updated_at": r[5], "source_gone_at": r[6],
            })),
        })
    });
    let summary = sid
        .as_deref()
        .zip(db.as_ref())
        .and_then(|(sid, db)| db.episode_facts(sid))
        .map(|e: EpisodeFacts| {
            json!({"request": e.request, "completed": e.completed, "outcome": e.outcome,
                   "files": e.files})
        });
    // The numbers come from the transcript the `csr_transcript` stats view reads: the JSONL,
    // else the mirror's copy.
    let source = path
        .clone()
        .filter(|p| p.is_file())
        .or_else(|| zst.clone().filter(|z| z.is_file()));
    let (stats, stats_note) = match source {
        None => (
            Value::Null,
            "No transcript on disk and no mirror copy to read.".to_owned(),
        ),
        Some(source) => {
            let parsed = if source.extension().is_some_and(|e| e == "zst") {
                crate::transcript::parse_mirrored_transcript(&source)
            } else {
                crate::transcript::parse_transcript(&source)
            };
            match parsed {
                Ok(parsed) => (
                    serde_json::to_value(crate::transcript::query::session_facts(&parsed))
                        .unwrap_or(Value::Null),
                    String::new(),
                ),
                Err(e) => (
                    Value::Null,
                    format!("The transcript could not be read: {e}"),
                ),
            }
        }
    };
    json!({
        "conv": sid,
        "project": project,
        "path": path.as_ref().map(|p| p.display().to_string()),
        "path_exists": path.as_ref().is_some_and(|p| p.is_file()),
        "db": paths.db.display().to_string(),
        "mirror": mirror,
        "ambiguous": origin.ambiguous,
        "mirror_ambiguous": origin.mirror_ambiguous,
        "summary": summary,
        "stats": stats,
        "stats_note": stats_note,
    })
}

/// Stored words with any secret redacted, whole and before anything is cut from them: a cut can
/// leave half a token the scrub no longer recognises. The scrub is `redact::scrub_inline`, which
/// also takes a `NAME_TOKEN=value` after a timestamp, a bullet or a few words.
pub(crate) fn scrub(text: &str) -> String {
    crate::redact::scrub_inline(text).into_owned()
}

/// `scrub`, one line cut to `cap` characters.
fn stored_line(text: &str, cap: usize) -> String {
    clip_to(&scrub(text), cap)
}

/// The draft is the person's prompt and must import like one, but two distinct emission field
/// tokens in one text (`NEXT:`, `LAST:`, `(outcome=`) read as CSR echoing its own format. The
/// draft's own fixed words carry none (`csr_reflect_on_past` is written without its opening
/// parenthesis), so only stored words can trip the guard, and a draft that does not trip it is
/// left byte for byte as stored. One that would gets the least change: all but one distinct token
/// get a space before their last character, outside backtick spans and quoted strings first; the
/// inside of those only when that was not enough. The guard itself is untouched.
pub(crate) fn import_fit(md: String) -> String {
    use crate::extraction::provenance::{is_csr_emission, EMISSION_FIELD_TOKENS};
    if !is_csr_emission(&md) {
        return md;
    }
    let present: Vec<&str> = EMISSION_FIELD_TOKENS
        .iter()
        .copied()
        .filter(|t| md.contains(t))
        .collect();
    // flagged for some other reason: nothing here to rewrite
    if present.len() < 2 {
        return md;
    }
    // the token kept whole is one that sits inside a span or quote, where a rewrite is worst
    let ranges = protected_ranges(&md);
    let keep = present
        .iter()
        .copied()
        .find(|t| {
            md.match_indices(t)
                .any(|(at, _)| ranges.iter().any(|(from, to)| (*from..*to).contains(&at)))
        })
        .unwrap_or(present[0]);
    let outside = defang_tokens(&md, &present, keep, false);
    if !is_csr_emission(&outside) {
        return outside;
    }
    defang_tokens(&md, &present, keep, true)
}

/// Byte ranges of the backtick spans and quoted strings of `text`, each on one line. An
/// apostrophe inside a word (`Claude's`) opens nothing, and a quote never closed is no span.
fn protected_ranges(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = vec![];
    let mut open: Option<(usize, char)> = None;
    for (i, &(at, c)) in chars.iter().enumerate() {
        if c == '\n' {
            open = None;
            continue;
        }
        match open {
            Some((from, quote)) if c == quote => {
                out.push((from, at + c.len_utf8()));
                open = None;
            }
            Some(_) => (),
            None if matches!(c, '`' | '\'' | '"') => {
                let apostrophe = c == '\''
                    && i > 0
                    && chars[i - 1].1.is_alphanumeric()
                    && chars.get(i + 1).is_some_and(|n| n.1.is_alphanumeric());
                if !apostrophe {
                    open = Some((at, c));
                }
            }
            None => (),
        }
    }
    out
}

/// `text` with every token of `tokens` but `keep` given a space before its last character; the
/// ones inside a code span or quote only when `inside`.
fn defang_tokens(text: &str, tokens: &[&str], keep: &str, inside: bool) -> String {
    let mut out = text.to_owned();
    for token in tokens.iter().filter(|t| **t != keep) {
        let ranges = protected_ranges(&out);
        let split = token.len() - token.chars().last().map_or(1, char::len_utf8);
        let mut next = String::with_capacity(out.len() + 4);
        let mut from = 0;
        for (at, _) in out.match_indices(token) {
            let guarded = ranges.iter().any(|(a, b)| (*a..*b).contains(&at));
            next.push_str(&out[from..at]);
            if guarded && !inside {
                next.push_str(token);
            } else {
                next.push_str(&token[..split]);
                next.push(' ');
                next.push_str(&token[split..]);
            }
            from = at + token.len();
        }
        next.push_str(&out[from..]);
        out = next;
    }
    out
}

/// One line: whitespace folded, cut at `FIELD_CHARS` with an ellipsis.
fn clip(text: &str) -> String {
    clip_to(text, FIELD_CHARS)
}

/// [`clip`] to `cap` characters.
fn clip_to(text: &str, cap: usize) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&one, cap)
}

/// `text` as one line (a newline is a space), cut at `cap` characters with an ellipsis; whitespace
/// is otherwise kept as it is.
fn cut(text: &str, cap: usize) -> String {
    let one = text.replace(['\n', '\r'], " ");
    if one.chars().count() <= cap {
        return one;
    }
    let mut cut: String = one.chars().take(cap.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The popup's words without markdown emphasis: stored summaries carry `**bold**` and `` `code` ``.
fn plain(text: &str) -> String {
    text.replace("**", "").replace("__", "").replace('`', "")
}

/// CSR's stored summary of the conversation as lines, or none when it stored no text. `draft`
/// keeps the markdown and defangs emission tokens; the popup preview strips the markdown.
/// `draft` is the per-field cap in characters when the lines are for the draft, `None` for the popup.
fn summary_lines(facts: &Value, draft: Option<usize>) -> Vec<String> {
    let s = &facts["summary"];
    [
        ("Asked", "request"),
        ("Done before it stopped", "completed"),
        ("Outcome", "outcome"),
    ]
    .iter()
    .filter_map(|(label, key)| {
        let text = crate::extraction::wrappers::without_harness_tags(string(&s[*key]));
        let text = text.trim();
        (!text.is_empty()).then(|| {
            if let Some(cap) = draft {
                format!("{label}: {}", stored_line(text, cap))
            } else {
                format!("{label}: {}", clip(&plain(&scrub(text))))
            }
        })
    })
    .collect()
}

fn session_lines(item: &Value) -> Vec<String> {
    let facts = &item["handoff"];
    if item["reading"] == true {
        return vec!["Reading the transcript…".into(), shape_line(&Value::Null)];
    }
    let st = &facts["stats"];
    if !st.is_object() {
        let note = string(&facts["stats_note"]);
        return vec![
            if note.is_empty() {
                "Not on record: the transcript was not read.".to_owned()
            } else {
                note.to_owned()
            },
            shape_line(&Value::Null),
        ];
    }
    let when = |key: &str| {
        paths::epoch(string(&st[key]))
            .map_or_else(|| "-".to_owned(), |t| recall::date(t, "%Y-%m-%d %H:%M"))
    };
    let mut out = vec![format!(
        "{} turns ({} yours, {} Claude's){}",
        st["turns"],
        st["user_turns"],
        st["assistant_turns"],
        st["duration_secs"]
            .as_f64()
            .map(|d| format!(" over {}", span(d)))
            .unwrap_or_default()
    )];
    if st["first_ts"].is_string() {
        out.push(format!("From {} to {}", when("first_ts"), when("last_ts")));
    }
    out.push(format!(
        "{} tool calls ({} failed), {} files touched, {} compaction{}",
        st["tool_calls"],
        st["tool_errors"],
        st["files_touched"],
        st["compactions"],
        if st["compactions"] == 1 { "" } else { "s" }
    ));
    out.push(shape_line(st));
    if st["from_mirror"] == true {
        out.push("Read from the mirror copy: the JSONL is gone.".into());
    }
    if let Some(e) = st["read_error"].as_str() {
        out.push(format!("Incomplete: the mirror stopped decoding ({e})."));
    }
    out
}

fn where_lines(item: &Value) -> Vec<String> {
    let facts = &item["handoff"];
    if !facts.is_object() {
        return vec!["Not read yet.".into()];
    }
    let mut out = vec![];
    match string(&facts["conv"]) {
        "" => out.push(match facts["ambiguous"].as_str() {
            Some(prefix) => format!(
                "Conversation: more than one conversation starts with {prefix}; none is chosen."
            ),
            None => "Conversation: no id on record for this thread.".to_owned(),
        }),
        id => out.push(format!(
            "Conversation: {id}{}",
            facts["project"]
                .as_str()
                .map(|p| format!(" (project folder {p})"))
                .unwrap_or_default()
        )),
    }
    out.push(match facts["path"].as_str() {
        Some(p) if facts["path_exists"] == true => format!("Transcript: {p}"),
        Some(p) => format!("Transcript: {p} (gone from disk)"),
        None => "Transcript: no JSONL found on disk.".to_owned(),
    });
    let m = &facts["mirror"];
    if facts["mirror_ambiguous"] == true {
        out.push(
            "Mirror: more than one transcript_mirror row could be this conversation's; none is claimed."
                .to_owned(),
        );
    }
    out.push(match (m["exists"] == true, m["row"].is_object()) {
        (true, true) => {
            let r = &m["row"];
            format!(
                "Mirror: {} ({} from {} of transcript, {} frames, {}, updated {})",
                string(&m["zst"]),
                size(num(&r["zst_bytes"])),
                size(num(&r["source_bytes"])),
                r["frames"],
                if r["sealed"] == 1 {
                    "sealed"
                } else {
                    "not sealed"
                },
                string(&r["updated_at"])
            )
        }
        (true, false) => format!(
            "Mirror: {} (file exists, no transcript_mirror row)",
            string(&m["zst"])
        ),
        (false, true) => format!(
            "Mirror: transcript_mirror row present but {} is missing.",
            string(&m["zst"])
        ),
        (false, false) => {
            "Mirror missing: no copy of this transcript and no transcript_mirror row.".to_owned()
        }
    });
    out.push(match string(&facts["conv"]) {
        "" => format!("Database: {}", string(&facts["db"])),
        conv => format!(
            "Database: {} (conv_{conv} chunks, episodes and summary)",
            string(&facts["db"])
        ),
    });
    out
}

/// What the popup shows for a thread item (`{key, text, state, last_seen, times, why, ...}` plus
/// `handoff`, once read).
/// What the send key types, under the thread.
pub const HANDS_OVER: &str = "s types this into Claude, with CSR's notes on it.";
/// A thread as sentences: a summary's "a; b; c" is read as three things left open, each starting
/// with a capital unless it starts with a code name.
pub fn thread_lines(text: &str) -> Vec<String> {
    plain(&scrub(text))
        .split("; ")
        .map(|clause| clause.trim().trim_end_matches([';', '.']).trim())
        .filter(|clause| !clause.is_empty())
        .map(|clause| {
            let word = clause.split_whitespace().next().unwrap_or("");
            if word.chars().skip(1).any(char::is_uppercase) {
                return clause.to_owned();
            }
            let mut chars = clause.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

pub fn view(item: &Value) -> View {
    let facts = &item["handoff"];
    let lines = summary_lines(facts, None);
    let times = item["times"].as_u64().unwrap_or(0);
    let mut kicker = vec![format!(
        "{} {}",
        if string(&item["state"]) == "carried" {
            "Carried in, last seen"
        } else {
            "Quiet since"
        },
        recall::date(num(&item["last_seen"]), "%m-%d %H:%M")
    )];
    if times > 0 {
        kicker.push(format!(
            "carried through {times} compaction{}",
            if times == 1 { "" } else { "s" }
        ));
    }
    let mut v = View {
        title: plain(&scrub(string(&item["text"]))),
        kicker: kicker.join(" · "),
        reading: item["reading"] == true,
        mix: (item["reading"] != true)
            .then(|| shape_of(&item["handoff"]["stats"]))
            .flatten(),
        sections: vec![
            Section {
                heading: "CSR's summary",
                lines: if lines.is_empty() {
                    vec![if facts.is_object() && facts["conv"].is_string() {
                        "No CSR summary yet.".to_owned()
                    } else if facts.is_object() {
                        "No CSR summary yet: the conversation could not be identified.".to_owned()
                    } else {
                        "Reading what CSR holds…".to_owned()
                    }]
                } else {
                    lines
                },
            },
            Section {
                heading: "The session",
                lines: session_lines(item),
            },
            Section {
                heading: "Where CSR keeps it",
                lines: where_lines(item),
            },
        ],
    };
    for section in &mut v.sections {
        for line in &mut section.lines {
            *line = scrub(line);
        }
    }
    v.kicker = scrub(&v.kicker);
    v
}

/// The prompt a fresh Claude Code session is given: the thread, CSR's summary, the numbers, where
/// CSR keeps the conversation, the tools to read it with and the ask. `None` while the facts are
/// still being read.
///
/// It ends with the source marker and is at most `MAX_WHOLE_DRAFT_BYTES` long: the importer keeps
/// a draft whole as one chunk only up to that size, so when a field-by-field draft would be
/// longer the per-field cap shrinks (700, 350, 175, 90, 40 characters) until it fits. The
/// marker always stays.
pub fn draft(item: &Value) -> Option<String> {
    if item["reading"] == true || !item["handoff"].is_object() {
        return None;
    }
    let marker = handoff_marker::marker_line(item["handoff"]["conv"].as_str());
    let mut fitted = None;
    for cap in [FIELD_CHARS, 350, 175, 90, 40] {
        let body = compose(item, cap)?;
        let whole = format!("{body}{marker}\n");
        let fits = whole.len() <= MAX_WHOLE_DRAFT_BYTES;
        fitted = Some(whole);
        if fits {
            break;
        }
    }
    fitted
}

/// The draft without its marker, every stored field cut to `cap` characters.
fn compose(item: &Value, cap: usize) -> Option<String> {
    let facts = &item["handoff"];
    let conv = facts["conv"].as_str();
    let title = clip_to(&scrub(string(&item["text"])), cap);
    let mut md = format!("{HANDOFF_OPENING}{title}\n\n");
    md.push_str(&format!(
        "{HANDOFF_LEAD}{} in {}. Everything below is copied from CSR's stored \
         rows or read from the files named; nothing is inferred.\n\n",
        recall::date(num(&item["last_seen"]), "%Y-%m-%d %H:%M"),
        match (conv, facts["project"].as_str()) {
            (Some(c), Some(p)) => format!("conversation `{c}` (project `{}`)", cut(p, cap)),
            (Some(c), None) => format!("conversation `{c}`"),
            _ => match facts["ambiguous"].as_str() {
                Some(prefix) => format!(
                    "a conversation CSR could not identify (more than one starts with {})",
                    cut(prefix, cap)
                ),
                None => "a conversation CSR could not identify".to_owned(),
            },
        }
    ));
    md.push_str(&format!("### {HANDOFF_THREAD_TITLE}\n\n"));
    md.push_str(&format!("{title}\n\n"));
    for line in std::iter::once(string(&item["why"]))
        .chain(
            item["detail"]
                .as_array()
                .into_iter()
                .flatten()
                .take(3)
                .map(string),
        )
        .filter(|l| !l.is_empty())
    {
        md.push_str(&format!("- {}\n", stored_line(line, cap)));
    }
    md.push('\n');
    let summary = summary_lines(facts, Some(cap));
    push_stated_section(
        &mut md,
        HANDOFF_SUMMARY_TITLE,
        (!summary.is_empty())
            .then(|| summary.join("\n\n"))
            .as_deref(),
        "No CSR summary yet — CSR stored no episode summary for this conversation. Nothing \
         is claimed about what it concluded; read it with the tools below.",
    );
    md.push_str(&format!("### {HANDOFF_NUMBERS_TITLE}\n\n"));
    for line in session_lines(item) {
        md.push_str(&format!("- {}\n", cut(&line, cap)));
    }
    md.push_str(&format!("\n### {HANDOFF_PLACES_TITLE}\n\n"));
    for line in where_lines(item) {
        md.push_str(&format!("- {}\n", cut(&line, cap)));
    }
    md.push_str(&format!("\n### {HANDOFF_READ_TITLE}\n\n"));
    md.push_str(RULE);
    md.push_str("\n\n");
    match conv {
        Some(c) => {
            md.push_str(&format!(
                "1. csr_transcript(session=\"{c}\", view=\"stats\"), then view=\"prompts\", then \
                 view=\"slice\" with last=20 for the end of it.\n\
                 2. csr_reflect_on_past with the query \"conv_{c}\" for CSR's stored view of it.\n\
                 3. get_full_conversation(conversation_id=\"{c}\") only if those are not enough.\n\n"
            ));
        }
        None => md.push_str(
            "No conversation id is on record. Find it with csr_reflect_on_past using the thread \
             title, then read the hit with csr_transcript (view=\"stats\", then \"prompts\").\n\n",
        ),
    }
    md.push_str(HANDOFF_ASK_HEADER);
    md.push_str("\n\n");
    md.push_str(
        "Revisit this thread as a side task: finish or close it in a few steps without \
         derailing the current work, then tell me in one line.\n",
    );
    // Whatever else went in, nothing leaves this function unscrubbed. The source tag is added by
    // `draft` after the scrub and the import fit, so neither can touch it: it is a fixed prefix
    // and an id cut to id characters, and it closes the draft exactly once.
    Some(import_fit(scrub(&md)))
}

/// A draft over an invented thread, for the contamination canary.
pub(crate) fn canary_draft() -> String {
    let sid = "3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f";
    let item = json!({
        "key": "k1", "text": "Retry the upload once the hub is back",
        "state": "carried", "times": 2, "last_seen": 1_761_990_000.,
        "why": "session 3002ab28, quiet when it ended",
        "detail": ["10-31 14:02 you: retry it later"],
        "came_from": "3002ab28", "origin_sid": sid,
        "handoff": {
            "conv": sid, "project": "-Users-x-projects-site",
            "path": format!("/Users/x/.claude/projects/-Users-x-projects-site/{sid}.jsonl"),
            "path_exists": true, "db": "/Users/x/.claude-self-reflect/csr-engine.db",
            "mirror": null,
            "summary": {"request": "Fix the upload", "completed": "Wrote the retry; the hub was down.",
                        "outcome": "partial", "files": []},
            "stats": null, "stats_note": "",
        },
    });
    draft(&item).expect("the invented thread has its facts")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f";

    fn item(handoff: Value) -> Value {
        json!({
            "key": "k1", "text": "Retry the upload once the hub is back",
            "state": "carried", "times": 2, "last_seen": 1_761_990_000.,
            "why": "session 3002ab28, quiet when it ended",
            "detail": ["10-31 14:02 you: retry it later", "10-31 14:05 Claude: the hub is down"],
            "came_from": "3002ab28", "origin_sid": SID,
            "handoff": handoff,
        })
    }

    fn facts() -> Value {
        json!({
            "conv": SID, "project": "-Users-x-projects-site",
            "path": format!("/Users/x/.claude/projects/-Users-x-projects-site/{SID}.jsonl"),
            "path_exists": true, "db": "/Users/x/.claude-self-reflect/csr-engine.db",
            "mirror": {"zst": format!("/Users/x/.claude-self-reflect/mirror/transcripts/-Users-x-projects-site/{SID}.jsonl.zst"),
                       "exists": true,
                       "row": {"source_bytes": 2_235_464, "zst_bytes": 438_255, "frames": 12,
                               "sealed": 0, "updated_at": "2026-11-03T15:11:32Z", "source_gone_at": null}},
            "summary": {"request": "Fix the upload", "completed": "Wrote the retry; the hub was down.",
                        "outcome": "partial", "files": []},
            "stats": {"turns": 412, "user_turns": 180, "assistant_turns": 232, "system_turns": 0,
                      "first_ts": "2026-10-31T10:02:00Z", "last_ts": "2026-10-31T13:22:00Z",
                      "duration_secs": 12000., "tool_calls": 37, "tool_errors": 2,
                      "files_touched": 12, "compactions": 3, "from_mirror": false,
                      "tool_bytes": 580_000, "context_bytes": 310_000, "other_bytes": 110_000,
                      "read_error": null},
            "stats_note": "",
        })
    }

    #[test]
    fn the_draft_names_the_tools_the_rule_the_ids_and_the_places() {
        let d = draft(&item(facts())).unwrap();
        for want in [
            "## Pick up a quiet thread: Retry the upload once the hub is back",
            &format!("conversation `{SID}` (project `-Users-x-projects-site`)"),
            "Asked: Fix the upload",
            "Done before it stopped: Wrote the retry; the hub was down.",
            "Outcome: partial",
            "412 turns (180 yours, 232 Claude's) over 3 h 20 min",
            "37 tool calls (2 failed), 12 files touched, 3 compactions",
            "Shape: tool-heavy (Tools 58%, Context 31%, Other 11%)",
            &format!("csr_transcript(session=\"{SID}\", view=\"stats\")"),
            &format!("csr_reflect_on_past with the query \"conv_{SID}\""),
            &format!("get_full_conversation(conversation_id=\"{SID}\")"),
            "never by hand-rolling jq or Python over the raw JSONL",
            "mirror/transcripts/-Users-x-projects-site/",
            "csr-engine.db",
            "Revisit this thread as a side task",
        ] {
            assert!(d.contains(want), "{want:?} missing from:\n{d}");
        }
    }

    #[test]
    fn the_shape_line_reads_the_context_cards_verdict_or_says_not_measured() {
        let v = view(&item(facts()));
        assert_eq!(v.mix.map(|m| m.1), Some("tool-heavy"));
        assert!(v.sections[1]
            .lines
            .iter()
            .any(|l| l == "Shape: tool-heavy (Tools 58%, Context 31%, Other 11%)"));
        let mut mixed = facts();
        mixed["stats"]["tool_bytes"] = json!(300);
        mixed["stats"]["context_bytes"] = json!(300);
        mixed["stats"]["other_bytes"] = json!(400);
        let d = draft(&item(mixed)).unwrap();
        assert!(
            d.contains("- Shape: mixed (Tools 30%, Context 30%, Other 40%)"),
            "{d}"
        );
        // no facts yet, no transcript, or a transcript of nothing: no donut, and it says so
        let mut loading = item(facts());
        loading["reading"] = json!(true);
        for it in [
            loading,
            item(json!({"conv": SID, "stats": null, "stats_note": "gone"})),
        ] {
            let v = view(&it);
            assert!(v.mix.is_none());
            assert!(
                v.sections[1]
                    .lines
                    .iter()
                    .any(|l| l == "Shape: not measured"),
                "{:?}",
                v.sections[1].lines
            );
        }
        let mut empty = facts();
        for k in ["tool_bytes", "context_bytes", "other_bytes"] {
            empty["stats"][k] = json!(0);
        }
        let v = view(&item(empty));
        assert!(v.mix.is_none());
        assert!(v.sections[1]
            .lines
            .iter()
            .any(|l| l == "Shape: not measured"));
    }

    #[test]
    fn a_missing_row_is_said_in_words_never_left_out() {
        let mut bare = facts();
        bare["summary"] = json!({"request": null, "completed": null, "outcome": null, "files": []});
        bare["stats"] = Value::Null;
        bare["stats_note"] = json!("No transcript on disk and no mirror copy to read.");
        bare["mirror"] =
            json!({"zst": "/d/mirror/transcripts/p/x.jsonl.zst", "exists": false, "row": null});
        bare["path_exists"] = json!(false);
        let d = draft(&item(bare)).unwrap();
        for want in [
            "### CSR's summary of that conversation",
            "No CSR summary yet — CSR stored no episode summary",
            "No transcript on disk and no mirror copy to read.",
            "Mirror missing: no copy of this transcript and no transcript_mirror row.",
            "(gone from disk)",
        ] {
            assert!(d.contains(want), "{want:?} missing from:\n{d}");
        }
        // and the popup says the same things
        let v = view(&item(
            json!({"conv": SID, "summary": null, "stats": null, "mirror": null}),
        ));
        assert_eq!(v.sections[0].heading, "CSR's summary");
        assert_eq!(v.sections[0].lines, ["No CSR summary yet."]);
    }

    fn asked_in(request: &str) -> (String, String) {
        let mut f = facts();
        f["summary"]["request"] = json!(request);
        let popup = view(&item(f.clone())).sections[0]
            .lines
            .iter()
            .find(|l| l.starts_with("Asked: "))
            .cloned()
            .unwrap_or_default();
        let whole = draft(&item(f)).unwrap();
        let drafted = whole
            .lines()
            .find(|l| l.starts_with("Asked: "))
            .unwrap_or_default()
            .to_owned();
        (popup, drafted)
    }

    #[test]
    fn no_harness_wrapper_tag_reaches_the_popup_or_the_draft() {
        // the live shape: the whole first message was a paste, cut at 200 bytes, never closed
        let (popup, drafted) =
            asked_in("<pasted_content id=\"818d\">\nspawn a worker to fix the feature earlier h");
        assert_eq!(popup, "Asked: spawn a worker to fix the feature earlier h");
        assert_eq!(drafted, popup);
        // whole message pasted and closed
        let (popup, drafted) =
            asked_in("<pasted_content id=\"818d\">\nspawn a worker\n</pasted_content>");
        assert_eq!(
            (popup.as_str(), drafted.as_str()),
            ("Asked: spawn a worker", "Asked: spawn a worker")
        );
        // typed words outside a closed block win
        let (popup, drafted) = asked_in(
            "investigate this > <pasted_content id=\"ce0b\">\nsomeone else's words\n</pasted_content>",
        );
        assert_eq!(popup, "Asked: investigate this >");
        assert_eq!(drafted, popup);
        // an id on the close tag
        let (popup, drafted) =
            asked_in("<pasted_content id=\"818d\">spawn</pasted_content id=\"818d\"> now");
        assert_eq!(popup, "Asked: now");
        assert_eq!(drafted, popup);
        // a typed ask before an unterminated paste
        let (popup, drafted) = asked_in(
            "look at this <pasted_content id=\"1ae4\">\nfamiliarize yourself with our pai",
        );
        assert_eq!(popup, "Asked: look at this");
        assert_eq!(drafted, popup);
        // the other two fields too
        let mut f = facts();
        f["summary"]["completed"] =
            json!("<system-reminder>injected</system-reminder>Wrote the retry");
        f["summary"]["outcome"] = json!("<pasted_content id=\"a0e3\">partial");
        let v = view(&item(f.clone()));
        let d = draft(&item(f)).unwrap();
        for text in v.sections[0].lines.iter().chain(std::iter::once(&d)) {
            assert!(
                !text.contains("<pasted_content") && !text.contains("<system-reminder"),
                "{text}"
            );
        }
        assert!(v.sections[0].lines.contains(&"Outcome: partial".to_owned()));
    }

    #[test]
    fn plain_stored_words_are_unchanged_by_the_wrapper_strip() {
        for plain in [
            "Fix the upload",
            "Use Vec<String> and a < b > c, then stop.",
        ] {
            let (popup, drafted) = asked_in(plain);
            assert_eq!(popup, format!("Asked: {plain}"));
            assert_eq!(drafted, popup);
        }
    }

    #[test]
    fn a_thread_with_no_known_conversation_gets_a_search_not_a_made_up_id() {
        let mut unknown = facts();
        unknown["conv"] = Value::Null;
        unknown["project"] = Value::Null;
        let mut it = item(unknown);
        it["origin_sid"] = Value::Null;
        let d = draft(&it).unwrap();
        assert!(d.contains("a conversation CSR could not identify"), "{d}");
        assert!(d.contains("No conversation id is on record."), "{d}");
        assert!(!d.contains("get_full_conversation(conversation_id="), "{d}");
    }

    #[test]
    fn the_draft_waits_for_the_facts() {
        assert!(draft(&json!({"key": "k", "text": "t"})).is_none());
        let mut reading = item(facts());
        reading["reading"] = json!(true);
        assert!(draft(&reading).is_none());
        let v = view(&reading);
        assert!(v.reading);
        assert_eq!(
            v.sections[1].lines,
            ["Reading the transcript…", "Shape: not measured"]
        );
    }

    #[test]
    fn a_pasted_draft_imports_like_any_other_prompt() {
        // The person sends this into a new session. If either guard read it as CSR's own
        // emission, that session would lose the prompt from the corpus.
        use crate::extraction::provenance::{
            contains_machine_sentinel, extractable, is_csr_emission,
        };
        for it in [item(facts()), {
            let mut bare = facts();
            bare["summary"] = Value::Null;
            bare["stats"] = Value::Null;
            bare["mirror"] = Value::Null;
            item(bare)
        }] {
            let d = draft(&it).unwrap();
            assert!(!is_csr_emission(&d), "{d}");
            assert!(!contains_machine_sentinel(&d), "{d}");
            // CSR's words in it are labeled (search, hooks), as a dream card's are
            assert_eq!(
                crate::import::contamination::first_reason(&d),
                Some(crate::import::contamination::ContaminationReason::HandoffDraft),
                "{d}"
            );
            let kept = extractable(&d).expect("a pasted handoff must survive import");
            assert!(
                kept.contains(SID),
                "and its ids must still be there: {kept}"
            );
        }
    }

    #[test]
    fn the_draft_ends_with_one_marker_line_carrying_the_conversation_id_only() {
        use crate::storage::handoff_marker::{HANDOFF_MARKER_PREFIX, HANDOFF_NO_ID};
        let d = draft(&item(facts())).unwrap();
        assert_eq!(d.matches(HANDOFF_MARKER_PREFIX).count(), 1, "{d}");
        assert_eq!(
            d.lines().last(),
            Some(format!("{HANDOFF_MARKER_PREFIX}{SID}").as_str()),
            "{d}"
        );
        assert!(d.ends_with('\n'));
        // no id on record: the fixed placeholder, never a title or a path
        let mut bare = facts();
        bare["conv"] = Value::Null;
        bare["project"] = Value::Null;
        let d = draft(&item(bare)).unwrap();
        assert_eq!(
            d.lines().last(),
            Some(format!("{HANDOFF_MARKER_PREFIX}{HANDOFF_NO_ID}").as_str()),
            "{d}"
        );
        importable(&d);
    }

    #[test]
    fn the_marker_survives_the_scrub_and_the_import_fit_even_when_the_words_trip_the_guard() {
        // two distinct field tokens in a stored word make `import_fit` rewrite the draft; the
        // marker is added after it and must come through untouched
        let mut f = facts();
        f["summary"]["completed"] = json!("NEXT: ship it LAST: done");
        let d = draft(&item(f)).unwrap();
        assert_eq!(
            d.lines().last(),
            Some(format!("↳ csr-handoff {SID}").as_str()),
            "{d}"
        );
        importable(&d);
    }

    const ASK_ONLY: &str = "### The ask\n\nRevisit this thread as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line.\n";

    /// The recogniser finds this whole draft, search and derived text agree, and derived text is
    /// the ask alone.
    fn recognised(d: &str) {
        use crate::import::contamination::{detect, ContaminationReason};
        let hits = detect(d);
        assert_eq!(hits.len(), 1, "{hits:?}\n{d}");
        assert_eq!(hits[0].reason, ContaminationReason::HandoffDraft, "{d}");
        assert_eq!(hits[0].start_byte, 0, "{d}");
        let ranges = handoff_marker::draft_ranges(d);
        assert_eq!(ranges.len(), 1, "{d}");
        assert_eq!(ranges[0].0, 0);
        assert_eq!(
            &d[ranges[0].1..],
            "\n",
            "the range ends at the marker:\n{d}"
        );
        assert_eq!(handoff_marker::strip_handoff(d), ASK_ONLY, "{d}");
    }

    #[test]
    fn every_variant_of_the_draft_is_recognised_and_strips_to_the_ask() {
        let mut variants: Vec<(&str, Value)> = vec![("full", facts())];
        let mut f = facts();
        f["summary"] = Value::Null;
        variants.push(("no summary", f));
        let mut f = facts();
        f["stats"] = Value::Null;
        f["stats_note"] = json!("The transcript could not be read.");
        variants.push(("no stats", f));
        let mut f = facts();
        f["conv"] = Value::Null;
        f["project"] = Value::Null;
        variants.push(("no id", f));
        let mut f = facts();
        f["conv"] = Value::Null;
        f["ambiguous"] = json!("3002ab");
        variants.push(("ambiguous", f));
        let mut f = facts();
        f["project"] = Value::Null;
        f["path"] = Value::Null;
        f["mirror"] = Value::Null;
        variants.push(("no project, path or mirror", f));
        let mut f = facts();
        f["summary"] = json!({
            "request": "NEXT: retry the upload",
            "completed": "LAST: wrote the retry. TODOS: none. ANCHORS: upload.rs (outcome=partial)",
            "outcome": "partial (outcome=partial)", "files": []});
        variants.push(("import_fit rewrite", f));
        let mut f = facts();
        f["stats"]["from_mirror"] = json!(true);
        f["stats"]["read_error"] = json!("bad frame");
        variants.push(("mirror read error", f));
        for (name, facts) in variants {
            let mut it = item(facts);
            if name == "import_fit rewrite" {
                it["text"] = json!("NEXT: retry the upload once the hub is back");
                it["detail"] = json!(["LAST: Claude said the hub is down"]);
            }
            let d = draft(&it).unwrap();
            eprintln!("variant {name}: {} bytes", d.len());
            recognised(&d);
        }
        recognised(&canary_draft());
    }

    /// A whole-component formatter (a memory-feedback report) claims the entire message in
    /// search, but a draft inside it is still found for the strip and for chunking.
    #[test]
    fn a_draft_inside_a_whole_component_message_is_still_found() {
        let d = draft(&json!({"text": "Retry upload", "handoff": {}})).unwrap();
        let input = format!("## CSR Memory Feedback — test\n\n{d}");
        assert_eq!(
            crate::import::contamination::detect(&input)[0].reason,
            crate::import::contamination::ContaminationReason::MemoryFeedback,
            "premise: search sees one whole-component hit"
        );
        assert_eq!(
            handoff_marker::strip_handoff(&input),
            format!("## CSR Memory Feedback — test\n\n{ASK_ONLY}")
        );
        let ranges = handoff_marker::draft_ranges(&input);
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert!(input[ranges[0].0..].starts_with(HANDOFF_OPENING));
        assert!(input[..ranges[0].1].ends_with("csr-handoff unidentified"));
    }

    #[test]
    fn the_largest_draft_the_fields_allow_fits_the_whole_chunk_cap_and_keeps_its_marker() {
        for (name, ch) in [("ascii", "x"), ("four-byte", "\u{1D538}")] {
            let big = |n: usize| ch.repeat(n);
            let mut f = facts();
            f["project"] = json!(big(255));
            f["path"] = json!(format!("/{}", big(4096)));
            f["db"] = json!(format!("/{}", big(4096)));
            f["mirror"]["zst"] = json!(format!("/{}", big(4096)));
            f["summary"] = json!({"request": big(900), "completed": big(900),
                                  "outcome": big(900), "files": []});
            f["stats"]["read_error"] = json!(big(4000));
            let mut it = item(f);
            it["text"] = json!(big(900));
            it["why"] = json!(big(900));
            it["detail"] = json!([big(900), big(900), big(900), big(900)]);
            let d = draft(&it).unwrap();
            assert!(
                d.len() <= MAX_WHOLE_DRAFT_BYTES,
                "{name}: {} bytes over {MAX_WHOLE_DRAFT_BYTES}",
                d.len()
            );
            assert_eq!(
                d.lines().last(),
                Some(format!("↳ csr-handoff {SID}").as_str()),
                "{name}"
            );
            recognised(&d);
        }
        // an ordinary draft is not trimmed at all
        let d = draft(&item(facts())).unwrap();
        assert!(d.contains("Wrote the retry; the hub was down."));
        assert!(d.len() < MAX_WHOLE_DRAFT_BYTES / 4, "{}", d.len());
    }

    #[test]
    fn gather_reads_the_transcript_the_stats_view_reads() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path().to_path_buf());
        let dir = paths.projects.join("-Users-x-projects-site");
        std::fs::create_dir_all(&dir).unwrap();
        let rows = [
            json!({"type":"user","uuid":"u1","timestamp":"2026-10-31T10:00:00Z","message":{"role":"user","content":"retry the upload"}}),
            json!({"type":"assistant","uuid":"a1","timestamp":"2026-10-31T10:30:00Z","message":{"role":"assistant","content":[
                {"type":"text","text":"on it"},
                {"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/x/upload.rs"}},
                {"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/x/upload.rs"}},
                {"type":"tool_use","id":"t3","name":"Bash","input":{"command":"ls"}}]}}),
            json!({"type":"user","uuid":"u2","timestamp":"2026-10-31T11:00:00Z","isCompactSummary":true,"message":{"role":"user","content":"This session is being continued from a previous conversation"}}),
        ];
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(dir.join(format!("{SID}.jsonl")), text).unwrap();
        let got = gather(&paths, &json!({"origin_sid": SID}));
        assert_eq!(got["conv"], SID);
        assert_eq!(got["project"], "-Users-x-projects-site");
        assert_eq!(got["path_exists"], true);
        assert_eq!(got["stats"]["turns"], 3);
        assert_eq!(got["stats"]["tool_calls"], 3);
        assert_eq!(got["stats"]["files_touched"], 1);
        assert_eq!(got["stats"]["compactions"], 1);
        // what was said and asked, no tool result in this one: the shape is all Context
        assert!(got["stats"]["context_bytes"].as_u64().unwrap() > 0);
        assert!(got["stats"]["other_bytes"].as_u64().unwrap() > 0);
        assert_eq!(got["stats"]["tool_bytes"], 0);
        assert_eq!(got["stats"]["duration_secs"], 3600.);
        // there is no database and no mirror here: both are said to be missing
        assert!(got["summary"].is_null());
        assert_eq!(got["mirror"]["exists"], false);
        // a thread kept with only the eight-character prefix finds the same conversation
        let by_prefix = gather(&paths, &json!({"came_from": "3002ab28"}));
        assert_eq!(by_prefix["conv"], SID);
        assert_eq!(by_prefix["stats"]["turns"], 3);
    }

    fn home(rows: &[&str]) -> (tempfile::TempDir, Paths) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path().to_path_buf());
        std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        let con = rusqlite::Connection::open(&paths.db).unwrap();
        con.execute_batch(
            "CREATE TABLE transcript_mirror (rel_path TEXT PRIMARY KEY, source_bytes INTEGER NOT NULL,
             tail_hash TEXT NOT NULL, zst_bytes INTEGER NOT NULL, frames INTEGER NOT NULL,
             sealed INTEGER NOT NULL DEFAULT 0, updated_at TEXT NOT NULL, source_gone_at TEXT);",
        )
        .unwrap();
        for (i, rel) in rows.iter().enumerate() {
            con.execute(
                "INSERT INTO transcript_mirror VALUES (?1, 100, 'h', 10, 1, 0, ?2, NULL)",
                rusqlite::params![rel, format!("2026-11-0{}T00:00:00Z", i + 1)],
            )
            .unwrap();
        }
        (home, paths)
    }

    fn live(paths: &Paths, rel: &str) {
        let file = paths.projects.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            file,
            "{\"type\":\"user\",\"uuid\":\"u\",\"timestamp\":\"2026-10-31T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();
    }

    const A: &str = "3002ab28-aaaa-4c61-9d2e-0a1b2c3d4e5f";
    const B: &str = "3002ab28-bbbb-4c61-9d2e-0a1b2c3d4e5f";

    #[test]
    fn a_token_in_a_thread_or_a_summary_never_reaches_the_draft_or_the_popup() {
        let gh = format!("ghp_{}", "A1b2".repeat(9));
        let sk = format!("sk-{}", "q7Zk".repeat(12));
        let mut f = facts();
        // a token that a cut would split: the scrub runs on the whole text first
        f["summary"]["completed"] = json!(format!("{} {gh} and more", "word ".repeat(138)));
        f["summary"]["request"] = json!(format!("use {sk} to log in"));
        let mut it = item(f);
        it["detail"] = json!([format!("10-31 14:02 you: my token is {gh}")]);
        it["why"] = json!(format!("quiet; key {sk}"));
        it["text"] = json!(format!("Rotate {gh}"));
        let d = draft(&it).unwrap();
        let v = view(&it);
        let popup = format!("{} {} {:?}", v.title, v.kicker, v.sections);
        for out in [&d, &popup] {
            for leak in [&gh[..8], &sk[..8], &gh[4..], &sk[3..]] {
                assert!(!out.contains(leak), "{leak} leaked:\n{out}");
            }
        }
        assert!(d.contains("REDACTED") || !d.contains("ghp_"), "{d}");
    }

    fn importable(d: &str) {
        use crate::extraction::provenance::{
            contains_machine_sentinel, extractable, is_csr_emission,
        };
        assert!(!is_csr_emission(d), "emission:\n{d}");
        assert!(!contains_machine_sentinel(d), "sentinel:\n{d}");
        // the draft is a prompt the person sends and it imports; only the one label is allowed
        let hit = crate::import::contamination::first_reason(d);
        assert_eq!(
            hit,
            Some(crate::import::contamination::ContaminationReason::HandoffDraft),
            "contamination {hit:?}:\n{d}"
        );
        assert!(
            extractable(d).is_some(),
            "a pasted handoff must survive import:\n{d}"
        );
    }

    #[test]
    fn the_drafts_own_words_carry_no_emission_field_token() {
        let d = draft(&item(facts())).unwrap();
        for token in crate::extraction::provenance::EMISSION_FIELD_TOKENS {
            assert!(!d.contains(token), "{token} is in the fixed words:\n{d}");
        }
        importable(&d);
    }

    #[test]
    fn a_stored_word_with_one_field_token_comes_through_byte_exact() {
        let mut f = facts();
        f["summary"]["request"] = json!("run `grep -F 'NEXT:' transcript.txt` and report");
        let mut it = item(f);
        it["text"] = json!("LAST: Claude said the hub is down");
        it["detail"] = json!(["10-31 14:02 you: grep -F \"NEXT:\" x"]);
        // NEXT: twice, LAST: once: two distinct tokens, so the draft needs a change ...
        let d = draft(&it).unwrap();
        importable(&d);
        // ... and the one inside the code span is the one kept byte for byte
        assert!(d.contains("`grep -F 'NEXT:' transcript.txt`"), "{d}");
        assert!(d.contains("grep -F \"NEXT:\" x"), "{d}");
        assert!(d.contains("LAST : Claude said the hub is down"), "{d}");
        // one token alone changes nothing
        let mut alone = facts();
        alone["summary"]["request"] = json!("run `grep -F 'NEXT:' transcript.txt`");
        let before = draft(&item(alone)).unwrap();
        assert!(
            before.contains("`grep -F 'NEXT:' transcript.txt`"),
            "{before}"
        );
        assert!(!before.contains(" :"), "{before}");
    }

    #[test]
    fn a_next_and_last_pair_keeps_the_draft_importable_with_the_least_change() {
        let mut f = facts();
        f["summary"] = json!({
            "request": "NEXT: retry the upload",
            "completed": "LAST: wrote the retry. TODOS: none. ANCHORS: upload.rs (outcome=partial)",
            "outcome": "partial (outcome=partial)", "files": []});
        let mut it = item(f);
        it["text"] = json!("NEXT: retry the upload once the hub is back");
        it["detail"] = json!([
            "LAST: Claude said the hub is down",
            "CONTINUED FROM [x] and Past session [y]"
        ]);
        let d = draft(&it).unwrap();
        importable(&d);
        let kept = crate::extraction::provenance::extractable(&d).unwrap();
        assert!(kept.contains("retry the upload"), "{kept}");
        // exactly one distinct token is left whole
        let whole = crate::extraction::provenance::EMISSION_FIELD_TOKENS
            .iter()
            .filter(|t| d.contains(**t))
            .count();
        assert_eq!(whole, 1, "{d}");
        // and the guard still catches the real thing
        assert!(crate::extraction::provenance::is_csr_emission(
            "LAST: wrote it\nNEXT: ship it"
        ));
        // every token inside quotes: the inside is rewritten only because it had to be
        let mut f = facts();
        f["summary"]["request"] = json!("grep 'NEXT:' a and grep 'LAST:' b");
        let d = draft(&item(f)).unwrap();
        importable(&d);
        assert!(d.contains("'NEXT:'") != d.contains("'LAST:'"), "{d}");
    }

    #[test]
    fn an_inline_credential_never_reaches_the_draft_or_the_popup() {
        let secret = format!("super{}123", "secret");
        let mut f = facts();
        f["summary"]["request"] = json!(format!(
            "set up with export DB_PASSWORD=\"{secret} two\" first"
        ));
        let mut it = item(f);
        it["text"] = json!(format!("rotate API_TOKEN={secret} today"));
        it["why"] = json!(format!("- API_TOKEN={secret}"));
        it["detail"] = json!([
            format!("10-31 14:02 you: API_TOKEN={secret}"),
            format!("then I ran API_TOKEN={secret} curl x and TOKEN_COUNT=3"),
        ]);
        let d = draft(&it).unwrap();
        let v = view(&it);
        let popup = format!("{} {} {:?}", v.title, v.kicker, v.sections);
        for out in [&d, &popup] {
            assert!(!out.contains(&secret), "{out}");
        }
        // the short value is not a secret and stays
        assert!(d.contains("TOKEN_COUNT=3"), "{d}");
        assert!(d.contains("API_TOKEN="), "{d}");
    }

    #[test]
    fn an_archived_conversation_and_a_live_one_sharing_a_prefix_are_ambiguous() {
        let (_h, paths) = home(&[&format!("-p/{A}.jsonl")]);
        live(&paths, &format!("-p/{B}.jsonl"));
        let got = gather(&paths, &json!({"came_from": "3002ab28"}));
        assert!(got["conv"].is_null(), "{got}");
        assert_eq!(got["ambiguous"], "3002ab28");
        let mut it = item(got);
        it["origin_sid"] = Value::Null;
        let d = draft(&it).unwrap();
        assert!(d.contains("No conversation id is on record."), "{d}");
        assert!(d.contains("more than one starts with 3002ab28"), "{d}");
        assert!(!d.contains(B) && !d.contains(A), "{d}");
        // the same prefix with only one of them left is that one
        let (_h2, paths2) = home(&[]);
        live(&paths2, &format!("-p/{B}.jsonl"));
        assert_eq!(
            gather(&paths2, &json!({"came_from": "3002ab28"}))["conv"],
            B
        );
    }

    #[test]
    fn an_archived_only_conversation_is_found_by_its_prefix_and_a_subagent_folder_counts() {
        let (_h, paths) = home(&[&format!("-p/{A}.jsonl")]);
        let got = gather(&paths, &json!({"came_from": "3002ab28"}));
        assert_eq!(got["conv"], A);
        assert_eq!(got["project"], "-p");
        assert_eq!(got["mirror"]["row"]["frames"], 1);
        // a session whose main file is gone but whose subagent runs are live is still a candidate
        let (_h, paths) = home(&[&format!("-p/{A}.jsonl")]);
        live(&paths, &format!("-p/{B}/subagents/agent-x.jsonl"));
        let got = gather(&paths, &json!({"came_from": "3002ab28"}));
        assert_eq!(got["ambiguous"], "3002ab28", "{got}");
    }

    #[test]
    fn the_mirror_row_is_found_by_its_exact_path_and_underscores_are_not_wildcards() {
        let (_h, paths) = home(&["-q/ab_cd.jsonl", "-q/abXcd.jsonl", "-r/ab_cd.jsonl"]);
        // no path kept, two rows with the same id: none is claimed
        let got = gather(&paths, &json!({"origin_sid": "ab_cd"}));
        assert_eq!(got["mirror_ambiguous"], true, "{got}");
        assert!(got["mirror"].is_null());
        // `_` matched `X` before: only the one row of this id may answer
        let (_h, paths) = home(&["-q/abXcd.jsonl", "-r/ab_cd.jsonl"]);
        let got = gather(&paths, &json!({"origin_sid": "ab_cd"}));
        assert_eq!(got["project"], "-r");
        assert_eq!(got["mirror"]["row"]["frames"], 1);
        // the path kept with the thread picks its own row, newer rows of other projects aside
        let (_h, paths) = home(&["-q/ab_cd.jsonl", "-r/ab_cd.jsonl"]);
        let kept = paths.projects.join("-q/ab_cd.jsonl");
        let got = gather(
            &paths,
            &json!({"origin_sid": "ab_cd", "origin_path": kept.display().to_string()}),
        );
        assert_eq!(got["project"], "-q");
        assert!(got["mirror"]["zst"]
            .as_str()
            .unwrap()
            .ends_with("-q/ab_cd.jsonl.zst"));
        assert_eq!(got["mirror_ambiguous"], false);
    }

    #[test]
    fn a_subagent_transcript_keeps_its_whole_path_and_its_project() {
        let (_h, paths) = home(&[]);
        let rel = format!("-p/{A}/subagents/agent-x.jsonl");
        live(&paths, &rel);
        let kept = paths.projects.join(&rel);
        let got = gather(
            &paths,
            &json!({"origin_sid": A, "origin_path": kept.display().to_string()}),
        );
        assert_eq!(got["project"], "-p");
        assert_eq!(got["path"], kept.display().to_string());
        assert_eq!(got["path_exists"], true);
        assert!(got["mirror"]["zst"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{rel}.zst")));
    }

    #[test]
    fn the_popup_shows_no_markdown_emphasis_and_the_draft_keeps_it() {
        let mut f = facts();
        f["summary"]["completed"] = json!("- **Last 7 days:** 283 looks, see `snapshot.json`");
        let it = item(f);
        let popup: Vec<String> = view(&it).sections[0].lines.clone();
        let flat = popup.join("\n");
        assert!(
            flat.contains("Last 7 days: 283 looks, see snapshot.json"),
            "{flat}"
        );
        assert!(!flat.contains("**") && !flat.contains('`'), "{flat}");
        assert!(draft(&it).unwrap().contains("**Last 7 days:**"));
    }
}

#[cfg(test)]
mod thread_line_tests {
    use super::thread_lines;

    #[test]
    fn a_summarys_list_reads_as_sentences_and_code_names_keep_their_case() {
        assert_eq!(
            thread_lines("skillListingBudgetFraction 0.005 awaits owner go; first real receipt:true; report reach."),
            [
                "skillListingBudgetFraction 0.005 awaits owner go",
                "First real receipt:true",
                "Report reach",
            ]
        );
    }
}
