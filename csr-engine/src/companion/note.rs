use super::{array, db::Db, num, paths, recall, string};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

pub const TOUCH_MAX: u64 = 8 * 1024 * 1024;

#[derive(Default, Debug)]
pub struct Touch {
    pub ranges: Vec<(usize, usize)>,
    pub text: Option<String>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: String,
    pub title: String,
    pub text: String,
    pub sketch: Vec<String>,
    pub symbol: Option<String>,
    pub oid: Option<String>,
    pub conversation: String,
    pub at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earlier: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub what: Option<Value>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct NotesOutput {
    pub notes: Vec<Note>,
    pub candidates: usize,
}

pub fn occurrences(text: &str, needle: &str, every: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    let mut at = 0;
    while let Some(found) = text[at..].find(needle) {
        let start = at + found;
        let first = text[..start].bytes().filter(|b| *b == b'\n').count() + 1;
        let count = needle.bytes().filter(|b| *b == b'\n').count();
        let last = first + count - usize::from(needle.ends_with('\n'));
        out.push((first, last.max(first)));
        if !every {
            break;
        }
        at = start + needle.len();
    }
    out
}

pub fn touch_of(raw: &Value, path: &Path) -> Touch {
    let mut touch = Touch::default();
    if !raw.is_object() || raw["too_big"] == true {
        return touch;
    }
    let integer = |key| raw[key].as_i64();
    let tool = string(&raw["tool"]);
    if tool == "Read" {
        let offset = integer("offset");
        let limit = integer("limit");
        if offset.is_some() || limit.is_some() {
            let first = offset.filter(|v| *v != 0).unwrap_or(1).max(1) as usize;
            let count = limit.filter(|v| *v != 0).unwrap_or(2000).max(1) as usize;
            touch.ranges.push((first, first.saturating_add(count - 1)));
        }
        return touch;
    }
    if !matches!(tool, "Edit" | "MultiEdit" | "Write") {
        return touch;
    }
    let text = if raw.get("before").is_some() {
        touch.text = raw["before"].as_str().map(str::to_owned);
        touch.text.clone()
    } else {
        std::fs::read(path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let Some(text) = text else { return touch };
    if tool == "Write" {
        if let Some(new) = raw["content"].as_str() {
            touch.ranges = changed_lines(&text, new);
        }
    } else {
        let edits = if tool == "MultiEdit" {
            array(&raw["edits"])
        } else {
            std::slice::from_ref(raw)
        };
        for edit in edits {
            if let Some(old) = edit["old_string"].as_str() {
                touch
                    .ranges
                    .extend(occurrences(&text, old, edit["replace_all"] == true));
            }
        }
        touch.ranges.sort_unstable();
        touch.ranges.dedup();
    }
    touch
}

// SequenceMatcher's longest contiguous matches, with its earliest-match tie rule.
// Myers diff chooses different replacement spans for repeated lines.
pub fn changed_lines(old: &str, new: &str) -> Vec<(usize, usize)> {
    use std::collections::{HashMap, HashSet};
    let lines = |s: &str| {
        let mut lines: Vec<String> = s.split('\n').map(str::to_owned).collect();
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    };
    let a = lines(old);
    let b = lines(new);
    let mut positions: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in b.iter().enumerate() {
        positions.entry(s).or_default().push(i);
    }
    let popular: HashSet<_> = if a.len() + b.len() > 8000 && b.len() >= 200 {
        positions
            .iter()
            .filter(|(_, v)| v.len() > b.len() / 100 + 1)
            .map(|(k, _)| *k)
            .collect()
    } else {
        HashSet::new()
    };
    let mut todo = vec![(0, a.len(), 0, b.len())];
    let mut matches = vec![];
    while let Some((alo, ahi, blo, bhi)) = todo.pop() {
        let (mut best_a, mut best_b, mut size) = (alo, blo, 0);
        let mut previous = HashMap::new();
        for (i, line) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut current = HashMap::new();
            if !popular.contains(line.as_str()) {
                for &j in positions.get(line.as_str()).into_iter().flatten() {
                    if j < blo || j >= bhi {
                        continue;
                    }
                    let k = previous.get(&j.wrapping_sub(1)).copied().unwrap_or(0) + 1;
                    current.insert(j, k);
                    if k > size {
                        best_a = i + 1 - k;
                        best_b = j + 1 - k;
                        size = k;
                    }
                }
            }
            previous = current;
        }
        while best_a > alo && best_b > blo && a[best_a - 1] == b[best_b - 1] {
            best_a -= 1;
            best_b -= 1;
            size += 1;
        }
        while best_a + size < ahi && best_b + size < bhi && a[best_a + size] == b[best_b + size] {
            size += 1;
        }
        if size > 0 {
            matches.push((best_a, best_b, size));
            if alo < best_a && blo < best_b {
                todo.push((alo, best_a, blo, best_b));
            }
            if best_a + size < ahi && best_b + size < bhi {
                todo.push((best_a + size, ahi, best_b + size, bhi));
            }
        }
    }
    matches.sort_unstable();
    matches.push((a.len(), b.len(), 0));
    let (mut ai, mut bi) = (0, 0);
    let mut ranges = vec![];
    for (i, j, size) in matches {
        if ai < i {
            ranges.push((ai + 1, i));
        } else if bi < j {
            ranges.push((ai.max(1), ai.max(1)));
        }
        ai = i + size;
        bi = j + size;
    }
    ranges
}

pub fn notes_for(
    db: &Db,
    paths: &paths::Paths,
    root: &Path,
    path: &Path,
    sid: &str,
    before: Option<f64>,
    touch: Option<&Touch>,
) -> NotesOutput {
    let path_s = path.to_string_lossy();
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut mine = db.shifts_for_file(root, path);
    if let Some(common) = paths::git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .filter(|p| p.ends_with("/.git"))
    {
        let main = Path::new(&common).parent().unwrap();
        if main != root {
            mine.extend(
                db.shifts_for_file(main, &main.join(rel.as_ref()))
                    .into_iter()
                    .filter(|s| {
                        s["file"] == rel.as_ref()
                            && paths::git(
                                root,
                                &["merge-base", "--is-ancestor", string(&s["oid"]), "HEAD"],
                            )
                            .is_some()
                    }),
            );
        }
    }
    let choose = |shifts: &[Value]| {
        recall::load_changes(db, paths, shifts, sid, before)
            .into_iter()
            .filter(|c| !c.current || c.earlier.is_some())
            .collect::<Vec<_>>()
    };
    let mut chosen = choose(&mine);
    let mut candidates = chosen.len();
    let top = (!chosen.is_empty())
        .then(|| paths::git(root, &["rev-parse", "--show-toplevel"]))
        .flatten();
    if let Some(touch) = touch {
        let spans = top
            .as_deref()
            .filter(|_| !touch.ranges.is_empty())
            .and_then(|top| {
                crate::dream::symbol_cli::symbol_span(
                    top,
                    &path_s,
                    None,
                    touch.text.as_ref().map(|s| s.as_bytes().to_vec()),
                )
                .ok()
            });
        let filtered: Vec<_> = mine
            .into_iter()
            .filter(|s| {
                let Some(spans) = &spans else { return false };
                let (first, last) = if string(&s["symbol"]).is_empty() {
                    (1, num(&spans["lines"]) as usize)
                } else {
                    let span = &spans["symbols"][string(&s["symbol"])];
                    (num(&span[0]) as usize, num(&span[1]) as usize)
                };
                first > 0 && touch.ranges.iter().any(|(a, b)| *a <= last && first <= *b)
            })
            .collect();
        chosen = choose(&filtered);
    }
    let mut notes = vec![];
    let mut symbols_session = crate::dream::symbol_cli::Session::new();
    for change in chosen {
        let latest = change
            .shifts
            .iter()
            .max_by(|a, b| num(&a["at"]).total_cmp(&num(&b["at"])))
            .unwrap();
        let same: Vec<_> = change
            .shifts
            .iter()
            .filter(|s| s["oid"] == latest["oid"])
            .collect();
        let mut symbols = vec![];
        for s in &same {
            let symbol = s["symbol"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(&name);
            if !symbols.contains(&symbol) {
                symbols.push(symbol);
            }
        }
        let oid = string(&latest["oid"]);
        let verb = match string(&latest["verb"]) {
            "removed" => "was removed",
            "came back" => "came back",
            _ => "changed",
        };
        let what = top
            .as_deref()
            .filter(|_| {
                verb == "changed" && symbols.len() == 1 && !string(&same[0]["symbol"]).is_empty()
            })
            .and_then(|top| {
                crate::dream::symbol_cli::symbol_change_in(
                    &mut symbols_session,
                    &crate::dream::symbol_cli::ChangeRequest {
                        repo: top,
                        file: &path_s,
                        symbol: Some(symbols[0]),
                        oid,
                        from: None,
                        summary: true,
                    },
                )
                .ok()
                .and_then(|v| v["summary"].as_str().map(recall::tidy_what))
                .flatten()
            });
        let talked = recall::date(change.talked, "%m-%d");
        let changed = string(&latest["date"]).get(5..).unwrap_or("");
        let oid7: String = oid.chars().take(7).collect();
        let lead = format!(
            "`{}`{}",
            symbols[0],
            if symbols.len() > 1 {
                format!(" and {} more", symbols.len() - 1)
            } else {
                String::new()
            }
        );
        let them = if symbols.len() > 1 { "them" } else { "it" };
        let mut line = if let Some(earlier) = change.earlier {
            format!(
                "{lead} in {name} {verb} in {oid7} on {changed}, after this session \
                worked on {them} before its {} compaction.",
                recall::date(earlier, "%m-%d %H:%M")
            )
        } else {
            format!(
                "{lead} in {name} {verb} in {oid7} on {changed}, \
                after your {talked} session worked on {them}."
            )
        };
        if let Some(what) = &what {
            line.push_str(&format!(" What changed: {what}."));
        }
        let named = symbols
            .iter()
            .take(4)
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!(
            "On {talked} you asked {}. That session worked on {named}{} in {rel}. \
             On {changed}, commit {oid7} {} {} {them}.{} \
             What that session concluded about {them} may no longer hold, \
             so check the code, not the memory.",
            recall::quoted(&change.title, 90),
            if symbols.len() > 4 { " and more" } else { "" },
            recall::quoted(string(&latest["subject"]), 70),
            verb.trim_start_matches("was "),
            what.as_ref()
                .map(|s| format!(" What changed: {s}."))
                .unwrap_or_default(),
        );
        let sketch = vec![
            format!(
                "{talked}  ● your session  {}",
                recall::quoted(&change.title, 34)
            ),
            format!(
                "{changed}  ▲ {oid7}  {}{}{}",
                symbols
                    .iter()
                    .take(3)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", "),
                if symbols.len() > 3 {
                    format!(" +{}", symbols.len() - 3)
                } else {
                    String::new()
                },
                what.as_ref().map(|s| format!(" · {s}")).unwrap_or_default()
            ),
        ];
        notes.push(Note {
            id: format!(
                "shift:{rel}:{}:{}",
                oid.chars().take(12).collect::<String>(),
                change.conversation.chars().take(8).collect::<String>()
            ),
            kind: "shift".into(),
            file: rel.to_string(),
            line,
            title: format!("Your {talked} work on {} has changed since", symbols[0]),
            text,
            sketch,
            symbol: Some(symbols[0].into()),
            oid: Some(oid.into()),
            conversation: change.conversation,
            at: num(&latest["at"]),
            earlier: Some(change.earlier.is_some()),
            what: Some(json!(what)),
        });
    }
    for row in db.query(
        "select ts,quote,session_id from intent_events where kind='correction' \
         and file is not null and file!='' and (?1=file or ?1 like '%/'||file) \
         order by ts desc limit 3",
        &[json!(path_s)],
    ) {
        if row[2] == sid {
            continue;
        }
        let at = paths::epoch(string(&row[0])).unwrap_or(0.);
        let day = if at != 0. {
            recall::date(at, "%m-%d")
        } else {
            string(&row[0]).chars().take(10).collect()
        };
        let session = string(&row[2]);
        notes.push(Note {
            id: format!(
                "rule:{rel}:{}",
                string(&row[0]).chars().take(19).collect::<String>()
            ),
            kind: "rule".into(),
            file: rel.to_string(),
            line: format!(
                "On {day} you told the agent about {name}: {}",
                recall::quoted(string(&row[1]), 110)
            ),
            title: format!("You set a rule for {name}"),
            text: format!(
                "On {day}, in session {}, you corrected the agent about {rel}: {} \
                CSR keeps that correction; it has not been retracted since.",
                session.chars().take(8).collect::<String>(),
                recall::quoted(string(&row[1]), 240)
            ),
            conversation: session.into(),
            at,
            ..Note::default()
        });
        candidates += 1;
    }
    notes.sort_by(|a, b| {
        (a.kind != "rule")
            .cmp(&(b.kind != "rule"))
            .then(b.at.total_cmp(&a.at))
    });
    NotesOutput { notes, candidates }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = concat!(
        "def f():\n    a = 10\n    return a\n\ndef g():\n",
        "    b = 20\n    return b\n\n\ndef h():\n    return 0\n"
    );
    fn touch(mut input: Value) -> Touch {
        input["before"] = SOURCE.into();
        touch_of(&input, Path::new("/no-such-file"))
    }
    #[test]
    fn test_an_edit_takes_the_first_occurrence_unless_it_replaces_all() {
        assert_eq!(
            touch(json!({"tool":"Edit","old_string":"    return"})).ranges,
            [(3, 3)]
        );
        assert_eq!(
            touch(json!({"tool":"Edit","old_string":"    return","replace_all":true})).ranges,
            [(3, 3), (7, 7), (11, 11)]
        );
    }
    #[test]
    fn test_a_multi_line_old_string_covers_its_lines() {
        assert_eq!(
            touch(json!({"tool":"Edit","old_string":"    return a\n\ndef g():\n"})).ranges,
            [(3, 5)]
        );
    }
    #[test]
    fn test_a_multi_edit_is_the_union_and_skips_what_is_not_found() {
        assert_eq!(
            touch(json!({"tool":"MultiEdit","edits":[
                {"old_string":"a = 10"},{"old_string":"nowhere"},{"old_string":"return 0"}
            ]}))
            .ranges,
            [(2, 2), (11, 11)]
        );
        assert_eq!(
            touch(json!({"tool":"MultiEdit","edits":[
                {"old_string":"a = 10","new_string":"z = 99"},{"old_string":"z = 99"}
            ]}))
            .ranges,
            [(2, 2)]
        );
    }
    #[test]
    fn test_a_write_fires_only_for_the_lines_that_differ() {
        for (new, want) in [
            (SOURCE.replace("b = 20", "b = 21"), vec![(6, 6)]),
            (
                SOURCE.replace("def h():", "# new\n# lines\ndef h():"),
                vec![(9, 9)],
            ),
            (SOURCE.replace("    a = 10\n", ""), vec![(2, 2)]),
            (format!("# header\n{SOURCE}"), vec![(1, 1)]),
            (SOURCE.to_string(), vec![]),
        ] {
            assert_eq!(changed_lines(SOURCE, &new), want);
        }
    }
    #[test]
    fn test_a_write_to_a_file_that_is_not_there_names_no_symbol() {
        assert!(touch_of(
            &json!({"tool":"Write","content":"def f():\n    pass\n"}),
            Path::new("/no-such-file")
        )
        .ranges
        .is_empty());
    }
    #[test]
    fn test_other_tools_and_calls_too_big_name_no_symbol() {
        for input in [
            json!({"tool":"NotebookEdit"}),
            json!({"tool":"Bash"}),
            json!({"tool":"Edit","too_big":true}),
            json!({}),
            json!([]),
            Value::Null,
            json!("Edit"),
        ] {
            let got = touch_of(&input, Path::new("/no-such-file"));
            assert!(got.ranges.is_empty());
            assert!(got.text.is_none());
        }
    }
    #[test]
    fn test_the_file_as_the_tool_found_it_is_what_the_lines_and_the_engine_read() {
        let got = touch(json!({"tool":"Edit","old_string":"b = 20"}));
        assert_eq!(got.ranges, [(6, 6)]);
        assert_eq!(got.text.as_deref(), Some(SOURCE));
        assert!(touch_of(
            &json!({"tool":"Write","before":null,"content":"x"}),
            Path::new("/no-such-file")
        )
        .ranges
        .is_empty());
    }
    #[test]
    fn test_a_read_with_a_window_names_the_symbols_in_it() {
        for (input, want) in [
            (json!({"tool":"Read","offset":4,"limit":3}), (4, 6)),
            (json!({"tool":"Read","limit":2}), (1, 2)),
            (json!({"tool":"Read","offset":6}), (6, 2005)),
        ] {
            assert_eq!(touch(input).ranges, [want]);
        }
        assert!(touch(json!({"tool":"Read"})).ranges.is_empty());
    }
}
