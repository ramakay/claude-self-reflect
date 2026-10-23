//! Question echoes: result rows that only restate the question.
//!
//! A conversation in which someone asked "why did we X?" leaves several rows
//! that carry the question and nothing else: the raw chunk (the question plus
//! one line of assistant narration), the extraction (`## User Request <q>` and
//! its signature) and the session story (`<q> --- Signature: {...}`). Their
//! words match the next person's identical question better than the answer
//! does, so they outrank it, and the person is shown their own question back.
//!
//! The rule judges the whole row. A row is an echo only when every sentence in
//! it is one of a closed list: a question (it ends with `?`, or it is the
//! query itself, word for word near enough), a session status word, a bare
//! path, or a progress announcement. Nothing is cut off the row before it is
//! judged, so a statement anywhere in it, an answer, a fix, an open issue,
//! keeps it where it is. A row the list does not explain abstains. A repeated
//! question whose row also states an answer is not an echo, and a question
//! the corpus never answered is still recalled (it sinks, it is not dropped).

use std::sync::LazyLock;

use regex::Regex;

/// Longest the stripped body of an echo can be. A longer body is carrying
/// something besides a restated question.
const MAX_ECHO_CHARS: usize = 700;

/// Longest a narration sentence can be and still count as filler.
const MAX_FILLER_CHARS: usize = 160;

/// Prefixes the result renderers put in front of a row's text.
const ROW_PREFIXES: [&str; 5] = [
    "[keyword] ",
    "[story] ",
    "[reflection] ",
    "[narrative] ",
    "[episode] ",
];

/// Rendered tool markers (`[ToolSearch: ...]`, `[mcp__server__tool: ...]`,
/// `[Read: ...]`): the importer's one-line rendering of a call, which says what
/// was looked at and never what was found.
static TOOL_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\[(?:mcp__[A-Za-z0-9_\-]+|ToolSearch|Read|Bash|Grep|Glob|Task|Agent|Skill|WebSearch|WebFetch)(?::[^\]\n]*)?\]",
    )
    .expect("tool marker regex")
});

/// The only words a progress announcement is made of. Closed: a sentence with
/// a word outside it (`Searching the old index took thirty seconds.`) is a
/// statement and may hold the answer.
const FILLER_WORDS: &str = "up|into|at|through|for|in|on|past|prior|previous|earlier|the|our|that|this|it|my|memory|memories|csr|session|sessions|conversation|conversations|history|again|now|more|related|relevant|what|how|why|whether|if|actual|reasoning|answer|details|context|commit";

/// Assistant narration that announces work without reporting a result: a
/// whole line that is a progress phrase (`Checking past sessions for the actual
/// reasoning.`, `Let me look.`, `Searching past sessions...`) and nothing else.
static FILLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)^(?:(?:checking|searching|looking|digging|scanning|querying|consulting|fetching|pulling|loading|grepping)|(?:let me|let\x27s|let us|i\x27ll|i will|i\x27m going to|i am going to) (?:check|look|search|find|see|pull|dig|query|try|run|grep|open|fetch|consult|scan|read))(?: (?:{FILLER_WORDS}))*\s*(?:\x{{2026}}|\.\.\.|[.!])?$|^(?:one moment|one sec|on it|sure|okay|ok)\s*(?:\x{{2026}}|\.\.\.|[.!])?$"
    ))
    .expect("filler regex")
});

/// The outcome words a session story appends to its request. They report that
/// a session ended, not what it found.
static STATUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:completed successfully|partially completed|completed|failed|abandoned|interrupted|in progress|resolved errors during session)\.?$",
    )
    .expect("status regex")
});

/// A line that is only file paths, optionally led by a line count: what a
/// `Read` or `ls` left behind. It names what was looked at, not what was found.
static PATH_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\d+\s+)?/\S+(?:\s+(?:\d+\s+)?/\S+)*$").expect("path-only regex")
});

/// Share of the query's distinct lowercase words a sentence without a question
/// mark must repeat to count as the query typed again. Measured on the live
/// askings of the ten known-answer questions: the earlier typings sit at 0.71 to
/// 1.00 of the query's words; the nearest different question or sentence at
/// 0.62 or below.
const NEAR_QUERY_COVERAGE: f64 = 0.7;

/// Words a restatement may add to the query. It says nothing new: one stray
/// word (a number, a quote, a name) at most.
const MAX_ADDED_WORDS: usize = 1;

/// A query shorter than this has too few words to tell a restatement from a
/// row that merely mentions them: the rule abstains.
const MIN_QUERY_WORDS: usize = 3;

/// Most words a sentence can have and still be the query restated.
const MAX_NEAR_QUERY_WORDS: usize = 30;

fn words(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether `sentence` is the query typed again without its question mark.
fn restates_query(sentence: &str, query: &str) -> bool {
    let sentence = words(sentence);
    let query = words(query);
    if query.len() < MIN_QUERY_WORDS || sentence.is_empty() || sentence.len() > MAX_NEAR_QUERY_WORDS
    {
        return false;
    }
    let shared = sentence.intersection(&query).count();
    let added = sentence.len() - shared;
    added <= MAX_ADDED_WORDS && shared as f64 / query.len() as f64 >= NEAR_QUERY_COVERAGE
}

/// Whether `content`, after its row prefixes, labels, JSON signature and tool
/// markers are set aside, is only a restatement of the question `query` with
/// no answer in it.
pub fn is_question_echo(content: &str, query: &str) -> bool {
    let body = strip_scaffolding(content);
    if body.chars().count() > MAX_ECHO_CHARS {
        return false;
    }
    let sentences = sentences(&body);
    let mut questions = 0;
    for sentence in &sentences {
        let text = sentence.text.as_str();
        if text.trim_end_matches('.').ends_with('?') || restates_query(text, query) {
            questions += 1;
        } else if !(STATUS.is_match(text)
            || PATH_ONLY.is_match(text)
            || (sentence.whole_line && is_filler(text)))
        {
            return false;
        }
    }
    questions > 0
}

/// A progress announcement short enough to carry no finding.
fn is_filler(sentence: &str) -> bool {
    sentence.chars().count() <= MAX_FILLER_CHARS && FILLER.is_match(sentence)
}

/// `content` without its row prefixes, the labels a derivation writes, its JSON
/// signature and rendered tool markers. Nothing else is removed: every
/// sentence that remains is judged. A session episode, stored as a JSON
/// object, is reduced to the text of its slots.
fn strip_scaffolding(content: &str) -> String {
    let mut text = content.trim_start();
    while let Some(rest) = ROW_PREFIXES
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
    {
        text = rest.trim_start();
    }
    if let Some(episode) = episode_text(text) {
        return TOOL_MARKER.replace_all(&episode, " ").into_owned();
    }
    let mut kept = String::new();
    for line in text.lines() {
        let line = without_signature(line);
        let line = without_label(&line);
        if line.chars().all(|c| matches!(c, '-' | '=' | ' ')) {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    let text = kept.trim_end().trim_end_matches('-');
    TOOL_MARKER.replace_all(text, " ").into_owned()
}

/// The labels a derivation puts in front of its content. The label is
/// scaffolding; what follows it on the line and below it is content.
const SCAFFOLD_LABELS: [&str; 6] = [
    "## User Request",
    "## Validation",
    "## Error Recovery",
    "## Solution Pattern",
    "## Code Context",
    "Context:",
];

fn without_label(line: &str) -> &str {
    let mut line = line.trim();
    while let Some(rest) = SCAFFOLD_LABELS
        .iter()
        .find_map(|label| line.strip_prefix(label))
    {
        line = rest.trim_start();
    }
    line
}

/// The first key of the signature JSON a derivation appends, for a signature
/// the chunk boundary cut before it closed.
const SIGNATURE_OPEN: &str = "{\"completion_status\"";

/// `line` without its derivation signature (`Signature: {json}`): only a
/// signature that is JSON (or the start of the one the derivation writes) goes.
/// `Signature: Ed25519 with key id K1.` is prose and stays.
fn without_signature(line: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let Some(at) = line.find("Signature:") else {
        return Cow::Borrowed(line);
    };
    let before = &line[..at];
    let rest = line[at + "Signature:".len()..].trim_start();
    if !rest.starts_with('{') {
        return Cow::Borrowed(line);
    }
    let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<serde_json::Value>();
    match stream.next() {
        Some(Ok(value)) if value.is_object() => {
            Cow::Owned(format!("{before} {}", &rest[stream.byte_offset()..]))
        }
        _ if rest.starts_with(SIGNATURE_OPEN) => Cow::Owned(before.to_string()),
        _ => Cow::Borrowed(line),
    }
}

/// The request, findings and open items of a session episode row (its
/// `investigated` slot lists files looked at, never a finding, so it is left out)
/// (`{"schema":"v2","request":...,"completed":...}`), one per line, or `None`
/// when `text` is not an episode.
fn episode_text(text: &str) -> Option<String> {
    if !text.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    object.get("schema")?;
    let mut out = String::new();
    for slot in [
        "request",
        "investigated",
        "completed",
        "next_steps",
        "blockers",
    ] {
        match object.get(slot) {
            Some(serde_json::Value::String(line)) => {
                out.push_str(line);
                out.push('\n');
            }
            Some(serde_json::Value::Array(lines)) => {
                for line in lines.iter().filter_map(serde_json::Value::as_str) {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    (!out.is_empty()).then_some(out)
}

/// One sentence of a body.
struct Sentence {
    text: String,
    /// The only sentence on its line.
    whole_line: bool,
}

/// The sentences of `body`: split at line breaks and at a `.`, `?` or `!`
/// followed by whitespace, each kept with its terminator.
fn sentences(body: &str) -> Vec<Sentence> {
    let mut out = Vec::new();
    for line in body.lines() {
        let first = out.len();
        let mut current = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            current.push(c);
            let ends =
                matches!(c, '.' | '?' | '!') && chars.peek().is_none_or(|n| n.is_whitespace());
            if ends {
                push_sentence(&mut out, &mut current);
            }
        }
        push_sentence(&mut out, &mut current);
        if out.len() - first == 1 {
            out[first].whole_line = true;
        }
    }
    out
}

fn push_sentence(out: &mut Vec<Sentence>, current: &mut String) {
    let text = current
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\u{201c}' | '\u{201d}' | '-' | '*'))
        .trim()
        .to_string();
    if text.chars().any(char::is_alphanumeric) {
        out.push(Sentence {
            text,
            whole_line: false,
        });
    }
    current.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIG: &str = r#"Signature: {"completion_status":"partial","confidence":"MEDIUM","frameworks":["rust"],"pattern_reusability":"medium","error_recovery":false,"total_edits":0,"iteration_count":2,"tools_used":["ToolSearch"],"files_modified":[],"concepts":[]}"#;
    const Q: &str = "why did we drop Docker and Qdrant for a single Rust binary?";
    /// The query a person types: the question without its question mark.
    const QUERY: &str = "why did we drop Docker and Qdrant for a single Rust binary";

    fn echo(row: &str) -> bool {
        is_question_echo(row, QUERY)
    }

    #[test]
    fn a_story_that_is_the_question_and_its_signature_is_an_echo() {
        assert!(echo(&format!("[story] {Q} --- Signature: {{\"completion_status\":\"partial\",\"confidence\":\"MEDIUM\",\"frameworks\":[\"rust\"],\"pattern_reusability\":\"medium\",\"error_recovery\":fa. Partially completed")));
        assert!(echo(&format!("{Q} --- {SIG}")));
    }

    #[test]
    fn an_extraction_that_is_the_question_and_its_signature_is_an_echo() {
        assert!(echo(&format!(
            "[reflection] ## User Request\n{Q}\n\n\n---\n{SIG}\nContext:\n## Validation"
        )));
    }

    #[test]
    fn a_question_with_assistant_filler_is_an_echo() {
        assert!(echo(&format!(
            "{Q}\n\nChecking past sessions for the actual reasoning."
        )));
        assert!(is_question_echo(
            "[keyword] why did we drop Qdrant\"\n\n[ToolSearch: select:mcp__claude-self-reflect__csr_reflect_on_past]\n\n[mcp__claude-self-reflect__csr_reflect_on_past: why drop Qdrant replace with Rust engine decision reasons]",
            "why did we drop Qdrant"
        ));
    }

    #[test]
    fn the_query_typed_again_without_a_question_mark_is_an_echo() {
        let query = "what did we decide about the hook budget";
        assert!(is_question_echo(query, query));
        assert!(is_question_echo(
            "what did we decide about the hook budget\n\nLooking that up now.",
            query
        ));
        // Near enough: one word of difference in eight.
        assert!(is_question_echo(
            "what did we decide on the hook budget",
            query
        ));
        // A different question without its mark is not this query's echo.
        assert!(!is_question_echo(
            "what did we decide about the retry budget for imports",
            query
        ));
    }

    #[test]
    fn a_question_followed_by_an_answer_is_not_an_echo() {
        assert!(!echo(&format!(
            "{Q} Because installing Docker was too painful and an embedded index was faster."
        )));
        assert!(!echo(&format!(
            "{Q}\n\nInstalling it was too painful, and the scale never needed a database server."
        )));
        assert!(!echo(&format!(
            "[story] {Q} The user chose one binary. --- {SIG}"
        )));
    }

    #[test]
    fn an_answer_that_mentions_a_question_is_not_an_echo() {
        assert!(!echo(
            "Installing it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded index was faster anyway."
        ));
        assert!(!echo(
            "Memory has answer. Qdrant died with whole v7 Python/Docker stack in v8.0 rewrite. Why drop: install friction."
        ));
        assert!(!echo(
            "Decision: drop Qdrant.\n\nwhy did we drop Docker and Qdrant?"
        ));
    }

    /// Codex finding 6: a statement that opens with an interrogative is not
    /// a question, however it is worded.
    #[test]
    fn a_statement_with_an_interrogative_opener_is_never_an_echo() {
        for (row, query) in [
            (
                "How long we retain caches is 300 seconds",
                "how long do we retain caches",
            ),
            (
                "Recall uses SQLite and HNSW",
                "does recall use SQLite and HNSW",
            ),
            (
                "What we changed eliminated network hops",
                "what did we change about network hops",
            ),
            (
                "Why we chose one binary was the install burden",
                "why did we choose one binary",
            ),
            (
                "How the import works is that every file is hashed first",
                "how does the import work",
            ),
            ("Did we ship the fix? Yes, in 9.5.8.", "did we ship the fix"),
        ] {
            assert!(!is_question_echo(row, query), "{row}");
            assert!(
                !is_question_echo(&format!("[reflection] {row}"), query),
                "{row}"
            );
        }
        // Not even when it is the query's own words and says more.
        assert!(!is_question_echo(
            "How long we retain caches is 300 seconds",
            "How long we retain caches"
        ));
    }

    #[test]
    fn a_statement_that_opens_like_a_question_is_not_an_echo() {
        assert!(!echo(
            "[reflection] why did we drop Docker and Qdrant: the install burden and the scale never needed a server."
        ));
        assert!(!echo("why we chose one binary was the install burden."));
    }

    /// Nothing is cut off a row before it is judged: the open issues of a
    /// story or an extraction are sentences like any other, so a row that
    /// lists them is not explained by the question and abstains.
    #[test]
    fn open_issues_in_a_row_keep_it_where_it_is() {
        assert!(!echo(&format!(
            "[story] {Q}. Completed successfully. Unresolved: \"b75ad51a\\nfix(mcp): keep library stdout off the JSON-RPC channel (#"
        )));
        assert!(!echo(&format!(
            "[story] {Q}. Partially completed. Resolved errors during session. Unresolved: --- {SIG}"
        )));
        assert!(!echo(&format!(
            "[reflection] ## User Request\n{Q}\n\n## Active Issues\n\"b75ad51a\\nfix(mcp): keep stdout off the channel (#\n\n---\n{SIG}"
        )));
    }

    /// Codex finding 4: the real fix after `## Active Issues` is content.
    #[test]
    fn a_fix_after_the_active_issues_heading_is_not_cut_off() {
        assert!(!echo(&format!(
            "[reflection] ## User Request\n{Q}\n\n## Active Issues\nThe import timed out. Fix: raise the timeout to 120s and retry the write.\n\n---\n{SIG}"
        )));
        assert!(!echo(&format!(
            "{Q}\nUnresolved: the HNSW file is rebuilt on every start, so hooks were slow."
        )));
        assert!(!echo(&format!(
            "[story] {Q}. Completed successfully. Unresolved: use SQLite and drop the server."
        )));
    }

    #[test]
    fn a_story_that_reports_a_finding_is_not_an_echo() {
        assert!(!echo(&format!(
            "[story] {Q}. Completed successfully. The team chose one binary because Docker was too painful to install."
        )));
    }

    #[test]
    fn an_episode_that_only_restates_the_request_is_an_echo() {
        let episode = |completed: &str| {
            format!(
                "[episode] {{\"schema\":\"v2\",\"session_id\":\"s\",\"request\":\"{Q}\",\"investigated\":[],\"completed\":\"{completed}\",\"next_steps\":null,\"blockers\":null,\"outcome\":\"partial\"}}"
            )
        };
        assert!(echo(&episode(
            "Checking past sessions for the actual reasoning."
        )));
        assert!(!echo(&episode(
            "Installing it was too painful, and the scale never needed a database server."
        )));
    }

    #[test]
    fn an_episode_that_only_lists_files_it_read_is_an_echo() {
        assert!(echo(&format!(
            "{{\"schema\":\"v2\",\"request\":\"{Q}\",\"investigated\":[\"/a/b/mod.rs\"],\"completed\":\"Checking the commit.\"}}"
        )));
    }

    #[test]
    fn a_question_followed_by_filler_and_bare_paths_is_an_echo() {
        assert!(echo(&format!(
            "{Q}\n\nChecking past sessions.\n\n/Users/x/.plans/02-results.md\n\n113 /Users/x/.plans/02-results.md"
        )));
        assert!(!echo(&format!(
            "{Q}\n\nChecking past sessions.\n\nSee /Users/x/.plans/02-results.md for the reasons."
        )));
    }

    #[test]
    fn a_sentence_that_merely_opens_with_a_progress_verb_is_not_filler() {
        assert!(!echo(&format!("{Q} Using SQLite removed network hops.")));
        assert!(!echo(&format!(
            "{Q}\n\nRunning them against a shared database caused lock contention."
        )));
        assert!(!echo(
            "Why did we drop Qdrant? Using SQLite removed network hops."
        ));
        assert!(!echo(&format!(
            "{Q}\n\nSearching the old index was slow because it scanned every row."
        )));
    }

    #[test]
    fn a_declarative_that_opens_with_an_interrogative_is_not_a_question() {
        let query = "what did we decide about the hook budget";
        assert!(!is_question_echo(
            "What fixed the timeout was raising the budget to 120s",
            query
        ));
        assert!(!is_question_echo(
            "How the import works is that every file is hashed first",
            query
        ));
        assert!(is_question_echo(query, query));
    }

    /// Only the query itself (or a question mark) makes a line a question: a
    /// bare line that merely opens like one abstains.
    #[test]
    fn an_unpunctuated_line_that_is_not_the_query_abstains() {
        assert!(!echo(
            "## User Request\nwhy did we drop Qdrant\nwhat replaced it"
        ));
        assert!(!echo(
            "## User Request\nwhy did we drop Qdrant\nwhat fixed it was the embedded index"
        ));
        assert!(echo(&format!("## User Request\n{QUERY}")));
    }

    #[test]
    fn the_fix_after_the_signature_is_content_not_scaffolding() {
        let row = format!(
            "[reflection] ## User Request\nwhy did the import time out?\n\n---\n{SIG}\nContext:\n## Error Recovery\n[Msg 5] Error: timeout\n  Fix: Increased timeout to 120s"
        );
        assert!(!echo(&row));
        let bare = format!(
            "[reflection] ## User Request\nwhy did the import time out?\n\n---\n{SIG}\nContext:\n## Validation"
        );
        assert!(echo(&bare));
    }

    /// Finding 7: filler is a whole line of a closed progress phrase.
    #[test]
    fn a_line_that_reports_something_after_a_progress_verb_is_not_filler() {
        assert!(!echo(
            "Why did we replace the index?\nSearching the old index took thirty seconds."
        ));
        assert!(echo(
            "Why did we replace the index?\nSearching past sessions\u{2026}"
        ));
        assert!(echo("Why did we replace the index?\nLet me look."));
        assert!(echo(&format!(
            "{Q}\n\nChecking past sessions for the actual reasoning."
        )));
    }

    /// Finding 8: a pronoun after the interrogative opens a statement.
    #[test]
    fn what_we_changed_is_a_statement_not_a_question() {
        assert!(!echo("What we changed eliminated network hops"));
        assert!(!echo(
            "why did we drop Qdrant?\nWhat we changed eliminated network hops"
        ));
        assert!(echo("## User Request why did we drop Qdrant?"));
    }

    /// Finding 9: only a JSON signature is stripped.
    #[test]
    fn a_prose_signature_line_is_content() {
        assert!(!echo(
            "why did we pick Ed25519?\nSignature: Ed25519 with key id K1."
        ));
        assert!(echo(&format!("why did we pick Ed25519?\n{SIG}")));
    }

    /// Finding 10: only the known labels are scaffolding, and what follows a
    /// label is content.
    #[test]
    fn a_heading_that_is_not_a_label_is_content() {
        assert!(!echo(
            "why did we drop Qdrant?\n## SQLite removes network hops"
        ));
        assert!(echo("## User Request why did we drop Qdrant?"));
        assert!(!echo(
            "## User Request why did we drop Qdrant? SQLite removes network hops."
        ));
    }

    #[test]
    fn plain_statements_and_empty_text_are_not_echoes() {
        assert!(!echo(""));
        assert!(!echo("[story] --- Signature: {}"));
        assert!(!echo("fix the flaky test in hooks_integration"));
    }
}
