use super::{array, note::Note, paths::Paths, private, send, string};
use serde::{
    de::{MapAccess, Visitor},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path, sync::LazyLock, time::Instant};

#[derive(Default, Clone)]
pub struct Terms(pub Vec<(String, String)>);
impl Serialize for Terms {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (term, said) in &self.0 {
            map.serialize_entry(term, said)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for Terms {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Terms;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of terms to sentences")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Terms, M::Error> {
                let mut out = vec![];
                while let Some(pair) = map.next_entry()? {
                    out.push(pair);
                }
                Ok(Terms(out))
            }
        }
        deserializer.deserialize_map(Read)
    }
}
const LOOKED_IN: &[&str] = &[
    "/.claude/",
    "claude.md",
    "agent_docs",
    "/agents",
    "/skills",
    "memory",
];
const PLAIN: &str = "the and for that this with from have what when your you are was but not can \
    will all any how why who its it's our out has had does did get got let just into over \
    then than \
    them they there their here more most some such only also very been being would could should \
    about after before again once make made want need like use used using see look now new one two \
    add fix run file files code claude memory agent agents skill skills session sessions build \
    test tests work works thing things please yes yeah okay done message messages name text";
static TERM: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[a-z][a-z0-9_-]{2,}").unwrap());
pub fn mentions(text: &str, term: &str) -> bool {
    let text = text.to_lowercase();
    let term = term.to_lowercase();
    text.match_indices(&term).any(|(at, _)| {
        let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
        text[..at].chars().next_back().is_none_or(|c| !word(c))
            && text[at + term.len()..]
                .chars()
                .next()
                .is_none_or(|c| !word(c))
    })
}
pub fn quoted_in(asked: &str, term: &str, pattern: Option<&str>) -> bool {
    if pattern.is_some_and(|p| {
        p.trim().eq_ignore_ascii_case(term)
            || p.trim().eq_ignore_ascii_case(&format!(r"\b{term}\b"))
    }) {
        return true;
    }
    let pattern = format!(r#"(?i)["'`](?:-\w+\s+)?{}["'`]"#, regex::escape(term));
    regex::Regex::new(&pattern).unwrap().is_match(asked)
}
pub fn sentences(text: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut previous = '\0';
    for (at, c) in text.char_indices() {
        if c == '\n' || (c.is_whitespace() && matches!(previous, '.' | '!' | '?')) {
            out.push(&text[start..at]);
            start = at + c.len_utf8();
        }
        previous = c;
    }
    out.push(&text[start..]);
    out
}
pub fn terms_looked_up(path: &Path) -> Terms {
    let mut out = Terms::default();
    let mut prompt = Terms::default();
    let mut calls = 6;
    let mut tail = super::tail::Tail::default();
    let _ = tail.poll(
        path,
        usize::MAX,
        || false,
        |raw| {
            let Some(raw) = raw else { return };
            let has = |needle: &[u8]| memchr::memmem::find(raw, needle).is_some();
            if has(b"\"type\":\"user\"")
                && !has(b"\"tool_use_id\"")
                && !has(b"\"isMeta\":true")
                && !has(b"\"isSidechain\":true")
            {
                let Ok(e) = sonic_rs::from_slice::<Value>(raw) else {
                    return;
                };
                if !e["message"].is_object() {
                    return;
                }
                let content = &e["message"]["content"];
                let said = content.as_str().map(str::to_owned).unwrap_or_else(|| {
                    array(content)
                        .iter()
                        .filter(|p| p["type"] == "text")
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                let text = send::own_words(&said);
                if e["isCompactSummary"] == true
                    || text.trim().is_empty()
                    || text.trim_start().starts_with(['<', '/'])
                {
                    return;
                }
                prompt.0.clear();
                for term in TERM.find_iter(&text.to_lowercase()).map(|m| m.as_str()) {
                    if !PLAIN.split_whitespace().any(|p| p == term)
                        && !prompt.0.iter().any(|(t, _)| t == term)
                    {
                        prompt.0.push((term.into(), text.clone()));
                    }
                }
                calls = 0;
            } else if calls < 6
                && !prompt.0.is_empty()
                && has(b"\"type\":\"tool_use\"")
                && !has(b"\"isSidechain\":true")
            {
                calls += memchr::memmem::find_iter(raw, b"\"type\":\"tool_use\"").count();
                let lower: Vec<_> = raw.iter().map(u8::to_ascii_lowercase).collect();
                if !LOOKED_IN
                    .iter()
                    .any(|s| memchr::memmem::find(&lower, s.as_bytes()).is_some())
                {
                    return;
                }
                let Ok(e) = sonic_rs::from_slice::<Value>(raw) else {
                    return;
                };
                for part in array(&e["message"]["content"]) {
                    if part["type"] != "tool_use" {
                        continue;
                    }
                    let args = &part["input"];
                    let asked = ["pattern", "command", "query", "file_path", "path"]
                        .iter()
                        .map(|k| string(&args[k]))
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !LOOKED_IN.iter().any(|s| asked.to_lowercase().contains(s)) {
                        continue;
                    }
                    for (term, said) in &prompt.0 {
                        if out.0.iter().any(|(t, _)| t == term)
                            || !quoted_in(&asked, term, args["pattern"].as_str())
                        {
                            continue;
                        }
                        let sentence = sentences(said)
                            .into_iter()
                            .find(|s| mentions(s, term))
                            .unwrap_or(said);
                        let sentence = sentence
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(90)
                            .collect();
                        out.0.push((term.clone(), sentence));
                    }
                }
            }
        },
    );
    out
}
pub fn coach_terms(paths: &Paths, now: f64) -> Vec<Note> {
    let cache = paths.keep.join("coach.json");
    let mut kept: BTreeMap<String, (Option<u64>, Terms)> = std::fs::read(&cache)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let files = super::sources::newest_transcripts(paths, 60);
    let started = Instant::now();
    let mut sessions: Vec<(String, Vec<String>)> = vec![];
    for (_, path) in &files {
        let stamp = super::sources::mtime(path);
        let key = path.to_string_lossy().to_string();
        if kept.get(&key).is_none_or(|(mtime, _)| *mtime != stamp) {
            if started.elapsed().as_secs_f64() > 12. {
                continue;
            }
            kept.insert(key.clone(), (stamp, terms_looked_up(path)));
        }
        for (term, said) in &kept[&key].1 .0 {
            if let Some((_, all)) = sessions.iter_mut().find(|(t, _)| t == term) {
                all.push(said.clone());
            } else {
                sessions.push((term.clone(), vec![said.clone()]));
            }
        }
    }
    kept.retain(|p, _| files.iter().any(|(_, path)| path.to_string_lossy() == *p));
    let mut redacted = kept.clone();
    for (_, terms) in redacted.values_mut() {
        for (_, said) in &mut terms.0 {
            *said = crate::redact::redact_private(said).into_owned();
        }
    }
    if let Ok(text) = serde_json::to_string(&redacted) {
        let _ = private::write_atomic(&cache, &text, &paths.keep);
    }
    let defined = std::fs::read_to_string(paths.home.join(".claude/CLAUDE.md")).unwrap_or_default();
    sessions.sort_by_key(|(_, s)| std::cmp::Reverse(s.len()));
    sessions
        .into_iter()
        .filter(|(term, said)| said.len() >= 2 && !mentions(&defined, term))
        .take(3)
        .map(|(term, said)| {
            let n = said.len();
            Note {
                id: format!("term:{term}"),
                kind: "term".into(),
                at: now * 1000.,
                line: format!(
                    "You said “{term}” in {n} sessions and Claude looked it up each time. \
                    Define it once?"
                ),
                title: format!("Define “{term}” once"),
                text: format!(
                    "- {term}: said in {n} sessions, each time Claude looked it up \
                    (e.g. “{}”). Edit this line to say what you mean.",
                    said[0]
                ),
                ..Note::default()
            }
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reading {
    pub words: String,
    pub read: String,
    pub line: String,
    pub when: f64,
    pub source: String,
}
pub fn reading_in(line: &str, reply: bool) -> Option<Reading> {
    let line = line
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_start_matches(['-', '*', '•', ' '])
        .trim()
        .to_owned();

    static MARKERS: LazyLock<[regex::Regex; 2]> = LazyLock::new(|| {
        [
            r"Interpreted as:|Read as:|\(unclear\)|most likely a typo for",
            concat!(
                r#"checking what [“"][^”"]+[”"] maps to|I['’]ll interpret [“"][^”"]+[”"] as|"#,
                r"reading (?:this|it) as|most plausibly refers to|typo for"
            ),
        ]
        .map(|p| {
            regex::RegexBuilder::new(p)
                .case_insensitive(true)
                .build()
                .unwrap()
        })
    });
    if !MARKERS[usize::from(reply)].is_match(&line) {
        return None;
    }
    static PATTERNS: LazyLock<[regex::Regex; 4]> = LazyLock::new(|| {
        let quote = r#"[“"]([^”"]{1,120})[”"]"#;
        [
            format!(r"{quote}\s+(?:is|was)\s+most\s+likely\s+a\s+typo\s+for\s+{quote}"),
            format!(r"I['’]ll interpret {quote} as\s+(.+)"),
            format!(r"{quote}\s*[—–-]*\s*(?:interpreted|read)\s+as:?\s*(.+)"),
            format!(r"{quote}\s*\(unclear\)"),
        ]
        .map(|p| {
            regex::RegexBuilder::new(&p)
                .case_insensitive(true)
                .build()
                .unwrap()
        })
    });
    let mut row = Reading {
        words: String::new(),
        read: String::new(),
        line: line.clone(),
        when: 0.,
        source: String::new(),
    };
    for (i, pattern) in PATTERNS.iter().enumerate() {
        if let Some(m) = pattern.captures(&line) {
            row.words = m[1].trim().into();
            if i < 3 {
                let text = &m[2];
                row.read = if i == 0 {
                    text.trim().into()
                } else {
                    text.split(". ")
                        .next()
                        .unwrap_or("")
                        .trim_end_matches('.')
                        .trim()
                        .into()
                };
            }
            break;
        }
    }
    Some(row)
}
pub fn how_claude_read(paths: &Paths, sid: &str, most: usize) -> Vec<Reading> {
    for id in super::session::lineage(paths, sid) {
        let mut session = super::session::Session::with_readings(&id);
        session.read_transcript(paths);
        if !session.readings.is_empty() {
            return session.readings.into_iter().rev().take(most).collect();
        }
    }
    vec![]
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::{self, File, FileTimes};
    #[test]
    fn test_two_sessions_not_in_claude_md_give_one_note_and_it_is_cached_by_mtime() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let folder = paths.projects.join("p");
        fs::create_dir_all(&folder).unwrap();
        let mut rows = String::new();
        for term in ["quokka", "osprey"] {
            for row in [
                json!({"type":"user","message":{"content":format!("call {term} now")}}),
                json!({"type":"assistant","message":{"content":[{"type":"tool_use",
                    "input":{"command":format!("grep -ril \"{term}\" /home/u/.claude/memory")}}]}}),
            ] {
                rows.push_str(&row.to_string());
                rows.push('\n');
            }
        }
        for sid in ["s1", "s2"] {
            fs::write(folder.join(format!("{sid}.jsonl")), &rows).unwrap();
        }
        fs::write(
            paths.home.join(".claude/CLAUDE.md"),
            "# Prefs\nosprey = a seabird\n",
        )
        .unwrap();
        let notes = coach_terms(&paths, 5.);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, "term:quokka");
        assert!(notes[0].line.contains("2 sessions"));
        for sid in ["s1", "s2"] {
            let path = folder.join(format!("{sid}.jsonl"));
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            fs::write(&path, "{}\n").unwrap();
            File::open(&path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(modified))
                .unwrap();
        }
        assert_eq!(coach_terms(&paths, 6.)[0].id, "term:quokka");
    }
    #[test]
    fn test_a_kept_file_of_another_shape_is_read_again() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        fs::create_dir_all(&paths.keep).unwrap();
        for bad in ["[1,2]", r#"{"/x.jsonl":5}"#, r#"{"/x.jsonl":[1,[]]}"#] {
            fs::write(paths.keep.join("coach.json"), bad).unwrap();
            assert!(coach_terms(&paths, 1.).is_empty());
        }
    }
}
