//! Question echoes: result rows that only restate the question.
//!
//! A conversation in which someone asked "why did we X?" leaves several rows
//! that carry the question and nothing else: the raw chunk (the question plus
//! one line of assistant narration), the extraction (`## User Request <q>` and
//! its signature) and the session story (`<q> --- Signature: {...}`). Their
//! words match the next person's identical question better than the answer
//! does, so they outrank it, and the person is shown their own question back.
//!
//! The rule reads the row's own text and nothing else: strip the derivation
//! scaffolding, then ask whether what is left is only question sentences and
//! process narration. Similarity to the query plays no part, so a repeated
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

/// Assistant narration that announces work without reporting a result.
static FILLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:checking|searching|looking|let me|let\x27s|let us|i\x27ll|i will|i\x27m|i am|one moment|on it|sure|okay|ok|now|first|running|reading|fetching|pulling|querying|consulting|digging|scanning|grepping|opening|trying|asking|calling|using|loading)\b",
    )
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

/// Openers of a question written without its question mark.
static QUESTION_OPENER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:why|what|how|when|where|who|whom|whose|which|did|do|does|is|are|was|were|can|could|should|would|will|have|has|had|remind me|tell me|explain|recall|remember)\b",
    )
    .expect("question opener regex")
});

/// Whether `content`, after the derivation scaffolding is stripped, is only a
/// restatement of a question with no answer in it.
pub fn is_question_echo(content: &str) -> bool {
    let body = strip_scaffolding(content);
    if body.chars().count() > MAX_ECHO_CHARS {
        return false;
    }
    let sentences = sentences(&body);
    let mut questions = 0;
    for (at, sentence) in sentences.iter().enumerate() {
        if sentence.trim_end_matches('.').ends_with('?')
            || (at == 0 && is_unpunctuated_question(sentence))
        {
            questions += 1;
        } else if !(STATUS.is_match(sentence)
            || PATH_ONLY.is_match(sentence)
            || (FILLER.is_match(sentence) && sentence.chars().count() <= MAX_FILLER_CHARS))
        {
            return false;
        }
    }
    questions > 0
}

/// A question typed without its question mark: it opens with an interrogative
/// and carries no punctuation that would make it a statement (a closing period,
/// or a colon introducing the answer, as in a memory titled `why X: because Y`).
fn is_unpunctuated_question(sentence: &str) -> bool {
    QUESTION_OPENER.is_match(sentence)
        && sentence.chars().count() <= MAX_FILLER_CHARS * 2
        && !sentence.ends_with(['.', '!', ':', ';'])
        && !sentence.contains(':')
}

/// `content` without its row prefixes, `## User Request` header, the slots a
/// derivation fills with open issues (`## Active Issues`, `Unresolved:`), the
/// signature (and whatever follows it) and rendered tool markers. A session
/// episode, stored as a JSON object, is reduced to the text of its slots.
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
    let text = text.strip_prefix("## User Request").unwrap_or(text);
    let mut end = text.len();
    for marker in ["Signature:", "## Active Issues", "Unresolved:"] {
        if let Some(at) = text.find(marker) {
            end = end.min(at);
        }
    }
    let text = text[..end].trim_end().trim_end_matches('-');
    TOOL_MARKER.replace_all(text, " ").into_owned()
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

/// The sentences of `body`: split at line breaks and at a `.`, `?` or `!`
/// followed by whitespace, each kept with its terminator.
fn sentences(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
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
    }
    out
}

fn push_sentence(out: &mut Vec<String>, current: &mut String) {
    let sentence = current
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\u{201c}' | '\u{201d}' | '-' | '*'))
        .trim()
        .to_string();
    if sentence.chars().any(char::is_alphanumeric) {
        out.push(sentence);
    }
    current.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIG: &str = r#"Signature: {"completion_status":"partial","confidence":"MEDIUM","frameworks":["rust"],"pattern_reusability":"medium","error_recovery":false,"total_edits":0,"iteration_count":2,"tools_used":["ToolSearch"],"files_modified":[],"concepts":[]}"#;
    const Q: &str = "why did we drop Docker and Qdrant for a single Rust binary?";

    #[test]
    fn a_story_that_is_the_question_and_its_signature_is_an_echo() {
        assert!(is_question_echo(&format!("[story] {Q} --- Signature: {{\"completion_status\":\"partial\",\"confidence\":\"MEDIUM\",\"frameworks\":[\"rust\"],\"pattern_reusability\":\"medium\",\"error_recovery\":fa. Partially completed")));
        assert!(is_question_echo(&format!("{Q} --- {SIG}")));
    }

    #[test]
    fn an_extraction_that_is_the_question_and_its_signature_is_an_echo() {
        assert!(is_question_echo(&format!(
            "[reflection] ## User Request\n{Q}\n\n\n---\n{SIG}\nContext:\n## Validation"
        )));
    }

    #[test]
    fn a_question_with_assistant_filler_is_an_echo() {
        assert!(is_question_echo(&format!(
            "{Q}\n\nChecking past sessions for the actual reasoning."
        )));
        assert!(is_question_echo(
            "[keyword] why did we drop Qdrant\"\n\n[ToolSearch: select:mcp__claude-self-reflect__csr_reflect_on_past]\n\n[mcp__claude-self-reflect__csr_reflect_on_past: why drop Qdrant replace with Rust engine decision reasons]"
        ));
    }

    #[test]
    fn a_question_without_a_question_mark_is_still_a_question() {
        assert!(is_question_echo("what did we decide about the hook budget"));
        assert!(is_question_echo(
            "what did we decide about the hook budget\n\nLooking that up now."
        ));
    }

    #[test]
    fn a_question_followed_by_an_answer_is_not_an_echo() {
        assert!(!is_question_echo(&format!(
            "{Q} Because installing Docker was too painful and an embedded index was faster."
        )));
        assert!(!is_question_echo(&format!(
            "{Q}\n\nInstalling it was too painful, and the scale never needed a database server."
        )));
        assert!(!is_question_echo(&format!(
            "[story] {Q} The user chose one binary. --- {SIG}"
        )));
    }

    #[test]
    fn an_answer_that_mentions_a_question_is_not_an_echo() {
        assert!(!is_question_echo(
            "Installing it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded index was faster anyway."
        ));
        assert!(!is_question_echo(
            "Memory has answer. Qdrant died with whole v7 Python/Docker stack in v8.0 rewrite. Why drop: install friction."
        ));
        assert!(!is_question_echo(
            "Decision: drop Qdrant.\n\nwhy did we drop Docker and Qdrant?"
        ));
    }

    #[test]
    fn a_statement_that_opens_like_a_question_is_not_an_echo() {
        assert!(!is_question_echo(
            "[reflection] why did we drop Docker and Qdrant: the install burden and the scale never needed a server."
        ));
        assert!(!is_question_echo(
            "why we chose one binary was the install burden."
        ));
    }

    #[test]
    fn a_story_with_only_an_outcome_and_open_issues_is_an_echo() {
        assert!(is_question_echo(&format!(
            "[story] {Q}. Completed successfully. Unresolved: \"b75ad51a\\nfix(mcp): keep library stdout off the JSON-RPC channel (#"
        )));
        assert!(is_question_echo(&format!(
            "[story] {Q}. Partially completed. Resolved errors during session. Unresolved: --- {SIG}"
        )));
    }

    #[test]
    fn an_extraction_whose_active_issues_are_noise_is_an_echo() {
        assert!(is_question_echo(&format!(
            "[reflection] ## User Request\n{Q}\n\n## Active Issues\n\"b75ad51a\\nfix(mcp): keep stdout off the channel (#\n\n---\n{SIG}"
        )));
    }

    #[test]
    fn a_story_that_reports_a_finding_is_not_an_echo() {
        assert!(!is_question_echo(&format!(
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
        assert!(is_question_echo(&episode(
            "Checking past sessions for the actual reasoning."
        )));
        assert!(!is_question_echo(&episode(
            "Installing it was too painful, and the scale never needed a database server."
        )));
    }

    #[test]
    fn an_episode_that_only_lists_files_it_read_is_an_echo() {
        assert!(is_question_echo(&format!(
            "{{\"schema\":\"v2\",\"request\":\"{Q}\",\"investigated\":[\"/a/b/mod.rs\"],\"completed\":\"Checking the commit.\"}}"
        )));
    }

    #[test]
    fn a_question_followed_by_filler_and_bare_paths_is_an_echo() {
        assert!(is_question_echo(&format!(
            "{Q}\n\nChecking past sessions.\n\n/Users/x/.plans/02-results.md\n\n113 /Users/x/.plans/02-results.md"
        )));
        assert!(!is_question_echo(&format!(
            "{Q}\n\nChecking past sessions.\n\nSee /Users/x/.plans/02-results.md for the reasons."
        )));
    }

    #[test]
    fn plain_statements_and_empty_text_are_not_echoes() {
        assert!(!is_question_echo(""));
        assert!(!is_question_echo("[story] --- Signature: {}"));
        assert!(!is_question_echo("fix the flaky test in hooks_integration"));
    }
}
