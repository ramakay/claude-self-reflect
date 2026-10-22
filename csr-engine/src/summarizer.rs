//! Haiku-curated session stories via Claude CLI headless mode.
//!
//! Flow: SQLite retrieves context → Haiku formats it into a 2-3 sentence story → stored as reflection.
//! Graceful degradation: returns None if claude CLI unavailable or times out.

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::engine::Engine;
use crate::import;
use crate::search::cross_project::resolve_project_from_cwd;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Context assembled from DB for Haiku's prompt.
pub struct StoryContext {
    pub current_session_excerpt: String,
    pub current_v3_enrichment: Option<String>,
    pub related_sessions: Vec<RelatedSession>,
    pub project_name: String,
}

pub struct RelatedSession {
    pub summary: String,
    pub age_label: String,
    pub score: f32,
}

const HAIKU_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSCRIPT_SAMPLE_MSGS: usize = 20;
const TRANSCRIPT_CHAR_CAP: usize = 30_000;
/// Numbers a session's story: `story_rev_1` is its first.
const STORY_REV_TAG: &str = "story_rev_";
/// The last messages a Haiku story reads beside Claude Code's summary.
const TAIL_MSGS: usize = 20;
/// How much of Claude Code's compaction summary a story keeps: what was asked, then where it
/// stood.
const ASKED_STORY_CHARS: usize = 420;
const STOOD_STORY_CHARS: usize = 420;

/// How a story comes to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryMode {
    /// A session's first story, by Haiku: skipped when it has one.
    First,
    /// After a compaction, from the summary Claude Code wrote of the session. Opted in, Haiku
    /// writes it from that summary and the last messages; otherwise CSR extracts it, with no
    /// model call. Either way every compaction refreshes it.
    FromCompaction,
    /// At the session's end, opted in: Haiku rewrites the story to take in what came after the
    /// last one, when the person asked something since.
    Rewrite,
}

/// Opt-in (`CSR_STORY_REWRITES=1`): Haiku writes the session's story, one metered call each time:
/// after every compaction, and at the session's end when the person asked something since.
pub fn story_rewrites_enabled() -> bool {
    std::env::var("CSR_STORY_REWRITES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Call `claude -p` (model chain: env override → haiku alias → CLI default)
/// to generate a curated session story.
/// Returns None if narratives are disabled, claude CLI is unavailable, times out, or fails.
pub async fn generate_session_story(
    context: &StoryContext,
) -> Option<(String, crate::narrative::ParsedNarrative)> {
    if crate::narrative::narratives_disabled() {
        return None;
    }
    let prompt = build_prompt(context);
    let mut parsed = call_claude_headless(&prompt).await?;
    let (prose, _) = crate::story_map::finish_story_output(
        &parsed.text,
        !crate::story_map::story_map_disabled(),
    );
    parsed.text = prose.clone();
    Some((prose, parsed))
}

/// Build the Haiku prompt from assembled DB context.
fn build_prompt(ctx: &StoryContext) -> String {
    let mut prompt = String::with_capacity(4096);

    prompt.push_str(
        "You are summarizing a coding session for future context restoration after memory compaction.\n\n",
    );

    // Current session excerpt
    if !ctx.current_session_excerpt.is_empty() {
        prompt.push_str("CURRENT SESSION TRANSCRIPT (excerpt):\n");
        prompt.push_str(&ctx.current_session_excerpt);
        prompt.push_str("\n\n");
    }

    // V3 enrichment (structured analysis)
    if let Some(ref v3) = ctx.current_v3_enrichment {
        prompt.push_str("STRUCTURED ANALYSIS OF CURRENT SESSION:\n");
        prompt.push_str(v3);
        prompt.push_str("\n\n");
    }

    // Related past sessions
    if !ctx.related_sessions.is_empty() {
        prompt.push_str("RELATED PAST SESSIONS:\n");
        for rel in &ctx.related_sessions {
            prompt.push_str(&format!(
                "- [{}] (relevance: {:.0}%) {}\n",
                rel.age_label,
                rel.score * 100.0,
                rel.summary
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str(
        "Write 2-3 sentences: what was being worked on, key files/tools used, current state \
         (done/in-progress/blocked), and how it relates to recent past work if applicable. \
         Be specific about technical details. No preamble, no markdown headers.",
    );

    if !crate::story_map::story_map_disabled() {
        prompt.push_str("\n\n");
        prompt.push_str(crate::story_map::shifts_instruction());
    }

    prompt
}

/// The story Haiku writes from Claude Code's compaction summary, which carries the session's
/// whole arc, and its last messages, which the summary never saw (Claude Code keeps them verbatim
/// after it). Of five ways to tell a long session, blind judges ranked this first on four of five
/// sessions (2026-10-10): the summary alone reported finished work as still open, and a sample of
/// the first and last messages lost the middle.
fn summary_story_prompt(summary: &str, tail: &str) -> String {
    let mut prompt = format!(
        "You are summarizing a coding session for future context restoration after memory compaction.\n\n\
         CLAUDE CODE'S SUMMARY OF THE SESSION (written at its latest compaction, from the whole \
         conversation except its last messages):\n{summary}\n\n\
         THE LAST MESSAGES (the summary did not see these; they show where the work actually \
         stood):\n{tail}\n\n\
         Write 3-4 sentences: what the session was about across its whole life and its main \
         threads, the key files/tools used, and where the work stood at the end \
         (done/in-progress/blocked). Where the last messages and the summary differ, trust the last \
         messages. Be specific about technical details. Keep it under 800 characters. No preamble, \
         no markdown headers."
    );
    if !crate::story_map::story_map_disabled() {
        prompt.push_str("\n\n");
        prompt.push_str(crate::story_map::shifts_instruction());
    }
    prompt
}

/// Invoke `claude -p` (model chain: env override → haiku alias → CLI default)
/// with prompt piped via stdin (avoids OS arg length limits).
/// Returns the model's text uncapped: the caller applies `story_map::finish_story_output`.
async fn call_claude_headless(prompt: &str) -> Option<crate::narrative::ParsedNarrative> {
    use tokio::io::AsyncWriteExt;

    // The transcript is embedded in `prompt`, so the analyst needs NO tools. Without
    // an explicit empty MCP config this subprocess inherits the user's full MCP set
    // and blows the context window on tool schemas alone (HTTP 400 prompt_too_long),
    // failing every call before inference. See narrative::minimal_mcp_config.
    let mcp_config_path = crate::narrative::minimal_mcp_config()
        .inspect_err(|e| {
            tracing::warn!(
                "minimal MCP config unavailable; this call inherits the user's MCP set \
                 and may fail with prompt_too_long: {e}"
            )
        })
        .ok();

    for candidate in crate::narrative::model_candidates() {
        let attempt = tokio::time::timeout(HAIKU_TIMEOUT, async {
            let mut cmd = tokio::process::Command::new("claude");
            // Nested sessions inherit the user's hook config — without this guard
            // every narrative call fires all 6 CSR hooks and its Stop hook stores
            // the analyst transcript as a session_episode (meta-episode pollution).
            cmd.env("CSR_DISABLE_RECURSIVE_HOOKS", "1");
            if let Some(model) = &candidate {
                cmd.args(["--model", model]);
            }
            // `--tools ""`: a print-mode child otherwise inherits every
            // built-in tool (Bash, Edit, Write, Agent) under the user's
            // permission mode; this call is a pure text summary.
            cmd.args(["-p", "-", "--output-format", "json", "--tools", ""]);
            // No user settings (plugins, SessionStart hooks) and no transcript
            // left behind for the watcher to re-import.
            cmd.args(crate::narrative::isolation_args());
            // Must come LAST: --mcp-config is variadic in the claude CLI and would
            // consume any positional arg that followed it.
            if let Some(path) = &mcp_config_path {
                cmd.args(["--strict-mcp-config", "--mcp-config"]);
                cmd.arg(path);
            }
            let mut child = match cmd
                .stdout(Stdio::piped())
                // Piped intentionally — required for model-not-found detection on the failure path; do not revert to null().
                .stderr(Stdio::piped())
                .stdin(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => return None, // claude CLI not found — no point walking the chain
            };

            // Write prompt to stdin then close it
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(prompt.as_bytes()).await;
                drop(stdin);
            }

            child.wait_with_output().await.ok()
        })
        .await;

        match attempt {
            Ok(Some(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                match crate::narrative::classify_attempt(output.status.success(), &stdout, &stderr)
                {
                    crate::narrative::AttemptOutcome::Parsed(parsed) => {
                        return Some(parsed);
                    }
                    crate::narrative::AttemptOutcome::ModelNotFound => continue,
                    crate::narrative::AttemptOutcome::Failed(_) => return None,
                }
            }
            _ => return None, // timeout or spawn failure
        }
    }
    None
}

/// Sample a transcript: first N + last N messages, capped at char_cap.
pub fn sample_transcript(messages: &[serde_json::Value], n: usize, char_cap: usize) -> String {
    let total = messages.len();
    let gap_marker = serde_json::json!({"gap": format!("... {} messages omitted ...", total.saturating_sub(n * 2))});
    let selected: Vec<&serde_json::Value> = if total <= n * 2 {
        messages.iter().collect()
    } else {
        let mut s: Vec<&serde_json::Value> = messages[..n].iter().collect();
        s.push(&gap_marker);
        s.extend(messages[total - n..].iter());
        s
    };

    let mut result = String::new();
    for msg in &selected {
        let data = crate::extraction::get_message_data(msg);
        let role = data
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("system");
        let content = crate::extraction::content_to_lower(&data);
        // Take first 500 chars per message to keep prompt manageable
        let preview: String = content.chars().take(500).collect();
        result.push_str(&format!("[{}]: {}\n", role, preview));

        if result.len() >= char_cap {
            result.truncate(result.floor_char_boundary(char_cap));
            result.push_str("\n... (truncated)");
            break;
        }
    }
    result
}

/// The person's own words in `typed`, whitespace collapsed; None when nothing in it was typed.
pub(crate) fn typed_words(typed: &str) -> Option<String> {
    let words = crate::extraction::provenance::extractable(typed)?;
    Some(words.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// What the person typed in these messages, oldest first: their own words, never tool
/// results, compaction summaries, harness rows or CSR's emissions. The last `n`, each cut to
/// `chars`.
pub fn typed_prompts(messages: &[serde_json::Value], n: usize, chars: usize) -> Vec<String> {
    let flag =
        |m: &serde_json::Value, key: &str| m.get(key).and_then(|v| v.as_bool()) == Some(true);
    let mut asked: Vec<String> = messages
        .iter()
        .filter(|m| {
            matches!(
                m.get("type").and_then(|v| v.as_str()),
                Some("user" | "human")
            )
        })
        .filter(|m| !flag(m, "isCompactSummary") && !flag(m, "isMeta"))
        .filter_map(|m| {
            let data = crate::extraction::get_message_data(m);
            let typed = match data.get("content")? {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Array(blocks) => {
                    fn kind(block: &serde_json::Value) -> Option<&str> {
                        block.get("type").and_then(|v| v.as_str())
                    }
                    if blocks.iter().any(|b| kind(b) == Some("tool_result")) {
                        return None;
                    }
                    blocks
                        .iter()
                        .filter(|b| kind(b) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                _ => return None,
            };
            let words = typed_words(&typed)?;
            Some(
                words
                    .chars()
                    .take(chars)
                    .collect::<String>()
                    .trim_end()
                    .to_string(),
            )
        })
        .collect();
    let older = asked.len().saturating_sub(n);
    asked.drain(..older);
    asked
}

/// The story a session already has.
#[derive(Debug, Default)]
pub struct PriorStory {
    /// Every stored copy, so a rewrite replaces them all.
    pub ids: Vec<String>,
    /// How many times the story has been written.
    pub rev: usize,
    /// The session's message count when it was written; None for a story from before the
    /// count was kept.
    pub covered: Option<usize>,
    /// When the story was written, from its stored reflection; None when that is unreadable.
    pub written_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The newest line of the transcript snapshot the story was generated from. A prompt typed
    /// at or before it is told; one typed after it is not, even if it landed before the story
    /// was stored (generation waits on a lock and a model call). None for a story written
    /// before this was kept, which falls back to `written_at`.
    pub told_through: Option<chrono::DateTime<chrono::Utc>>,
}

/// The newest timestamp among the lines of a transcript snapshot: its messages and its queued
/// prompts. None when no line carries a readable time.
fn snapshot_cutoff(
    messages: &[serde_json::Value],
    queued: &[crate::import::QueuedPrompt],
) -> Option<chrono::DateTime<chrono::Utc>> {
    messages
        .iter()
        .filter_map(|m| m.get("timestamp")?.as_str())
        .filter_map(crate::temporal::parse_timestamp)
        .chain(queued.iter().filter_map(|q| q.at))
        .max()
}

fn prior_story(storage: &Storage, conv_id: &str) -> Option<PriorStory> {
    let rows = storage
        .get_reflections_by_two_tags("session_story", &format!("conv_{conv_id}"), 10)
        .ok()?;
    rows.first()?;
    let rev = rows
        .iter()
        .flat_map(|(_, _, tags, _)| tags.iter())
        .filter_map(|tag| tag.strip_prefix(STORY_REV_TAG)?.parse::<usize>().ok())
        .max()
        .unwrap_or(1);
    Some(PriorStory {
        ids: rows.iter().map(|(id, _, _, _)| id.clone()).collect(),
        rev,
        covered: storage.story_messages_covered(conv_id).ok().flatten(),
        written_at: rows
            .first()
            .and_then(|(_, _, _, written)| crate::temporal::parse_timestamp(written)),
        told_through: storage
            .story_told_through(conv_id)
            .ok()
            .flatten()
            .and_then(|at| crate::temporal::parse_timestamp(&at)),
    })
}

/// Whether to write the session's story now. `messages` is the session's message count;
/// `summary_at` is where Claude Code's newest compaction summary sits among them; `asked_since`
/// counts what the person typed after the last story.
///
/// - First: only when the session has no story.
/// - FromCompaction: whenever there is a summary the story has not taken in yet. Nothing caps
///   it: one story per compaction.
/// - Rewrite: when the person asked something since the last story. Recent work decides, not a
///   count or a share of the session, so a long session's ending reaches its story too.
pub fn story_due(
    mode: StoryMode,
    prior: Option<&PriorStory>,
    messages: usize,
    summary_at: Option<usize>,
    asked_since: usize,
) -> Result<(), String> {
    match (mode, prior) {
        (StoryMode::FromCompaction, _) => match (summary_at, prior.and_then(|p| p.covered)) {
            (None, _) => Err("skip:no_compaction_summary".into()),
            (Some(at), Some(covered)) if covered >= at => Err(format!("skip:summary_told({at})")),
            _ => Ok(()),
        },
        (_, None) => Ok(()),
        (StoryMode::First, Some(_)) => Err("skip:story_exists".into()),
        (StoryMode::Rewrite, Some(prior)) => match prior.covered {
            // Whether anything new was asked decides, not whether more messages exist:
            // a prompt typed while Claude was busy can be the only thing after the
            // last message.
            Some(covered) if asked_since == 0 => {
                Err(format!("skip:nothing_asked_since({covered}/{messages})"))
            }
            _ => Ok(()),
        },
    }
}

/// What the person typed after the story covering the first `covered` messages. A prompt typed
/// while Claude was busy has no user row, only a `queued_command` attachment; `queued` holds
/// those, each with its place among the messages. One whose text also arrives as a user row at
/// or after its place is that same prompt, counted once. A prompt the story already told is not
/// a new ask: one queued inside the covered part, or typed before the story was written (a
/// rewrite that found nothing new to cover leaves `covered` where it was, so place alone would
/// count the same prompt on every rewrite). That holds across the story's edge: the user row of
/// a told prompt landing after it is not a new ask either.
fn asked_since(
    messages: &[serde_json::Value],
    queued: &[crate::import::QueuedPrompt],
    prior: Option<&PriorStory>,
) -> usize {
    let covered = prior
        .and_then(|p| p.covered)
        .unwrap_or(0)
        .min(messages.len());
    // The snapshot the story was generated from, or for an older story the time it was stored.
    let told_through = prior.and_then(|p| p.told_through.or(p.written_at));
    let told_by_story = |prompt: &crate::import::QueuedPrompt| {
        prompt.place < covered
            || matches!((prompt.at, told_through), (Some(at), Some(w)) if at <= w)
    };
    let rows_by_text = user_rows_by_text(messages);
    // User rows that are the twin of a prompt the story told.
    let mut told: HashSet<usize> = HashSet::new();
    let mut from_queue = 0;
    let keyed = queued_keys(queued);
    let twins = crate::import::queued_prompts::pair_with_rows(&keyed, &rows_by_text);
    for ((prompt, (_, key)), twin) in queued.iter().zip(&keyed).zip(twins) {
        if key.is_none() {
            continue;
        }
        match twin {
            Some(at) => {
                if told_by_story(prompt) {
                    told.insert(at);
                }
            }
            None if !told_by_story(prompt) => from_queue += 1,
            None => {}
        }
    }
    let rows = rows_by_text
        .values()
        .flatten()
        .filter(|at| **at >= covered && !told.contains(at))
        .count();
    rows + from_queue
}

/// The typed text of each user row, by message index.
fn user_rows_by_text(
    messages: &[serde_json::Value],
) -> std::collections::HashMap<String, Vec<usize>> {
    let mut rows_by_text: std::collections::HashMap<String, Vec<usize>> = Default::default();
    for (at, message) in messages.iter().enumerate() {
        for text in typed_prompts(std::slice::from_ref(message), 1, usize::MAX) {
            rows_by_text.entry(text).or_default().push(at);
        }
    }
    rows_by_text
}

/// Each queued prompt's place and typed words, the key `pair_with_rows` matches on.
fn queued_keys(queued: &[crate::import::QueuedPrompt]) -> Vec<(usize, Option<String>)> {
    queued
        .iter()
        .map(|prompt| (prompt.place, typed_words(&prompt.text)))
        .collect()
}

/// The messages in `range` as the story's prompt shows them: each human-typed prompt that has no
/// user row of its own (typed while Claude was busy) put in as a user line at its place, so the
/// story can tell what was asked mid-turn. The messages themselves, their numbering and the
/// stored `covered` count are not touched; this is text for the prompt only. A prompt queued
/// after the last message belongs to a range that runs to the end.
fn with_queued_prompts(
    messages: &[serde_json::Value],
    queued: &[crate::import::QueuedPrompt],
    range: std::ops::Range<usize>,
) -> Vec<serde_json::Value> {
    let keyed = queued_keys(queued);
    let twins = crate::import::queued_prompts::pair_with_rows(&keyed, &user_rows_by_text(messages));
    let extra: Vec<&crate::import::QueuedPrompt> = queued
        .iter()
        .zip(&keyed)
        .zip(twins)
        .filter(|((_, (_, key)), twin)| key.is_some() && twin.is_none())
        .map(|((prompt, _), _)| prompt)
        .collect();
    let mut shown = Vec::with_capacity(range.len() + extra.len());
    for at in range.start..=range.end {
        if at < range.end || range.end == messages.len() {
            shown.extend(
                extra
                    .iter()
                    .filter(|prompt| prompt.place == at)
                    .map(|prompt| {
                        serde_json::json!({
                            "type": "user",
                            "message": {"role": "user", "content": prompt.text}
                        })
                    }),
            );
        }
        if at < range.end {
            shown.push(messages[at].clone());
        }
    }
    shown
}

/// Claude Code's own summary at the session's newest compaction: where it sits among the
/// messages, and its text.
pub fn newest_compaction_summary(messages: &[serde_json::Value]) -> Option<(usize, String)> {
    let at = messages
        .iter()
        .rposition(|m| m.get("isCompactSummary").and_then(|v| v.as_bool()) == Some(true))?;
    let data = crate::extraction::get_message_data(&messages[at]);
    let text = crate::hooks::stop::extract_text_from_content(data.get("content")?);
    (!text.trim().is_empty()).then_some((at, text))
}

/// A story from Claude Code's compaction summary, with no model call: what was asked, then
/// where the work stood. The summary follows Claude Code's numbered template ("1. Primary
/// Request and Intent:" … "8. Current Work:"); one that does not is read from its start. The
/// bool says whether the template was found.
pub fn story_from_compaction_summary(summary: &str) -> Option<(String, bool)> {
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*\d+\.\s+([A-Z][A-Za-z ]+?):?\s*$").unwrap());
    let body = summary
        .split_once("Summary:")
        .map_or(summary, |(_, rest)| rest);
    let headings: Vec<_> = HEADING.captures_iter(body).collect();
    let section = |name: &str| {
        let at = headings.iter().position(|c| c[1].starts_with(name))?;
        let start = headings[at].get(0)?.end();
        let end = headings
            .get(at + 1)
            .and_then(|c| c.get(0))
            .map_or(body.len(), |m| m.start());
        Some(plain(&body[start..end]))
    };
    let (story, templated) = match (section("Primary Request"), section("Current Work")) {
        (Some(asked), Some(stood)) => {
            let asked = sentences(&asked, ASKED_STORY_CHARS);
            // Claude wrote the summary: its "I" is Claude to whoever reads the story later
            static CLAUDE_SAID_I: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"(^|[.!?]\s+)I ").unwrap());
            let stood = CLAUDE_SAID_I
                .replace_all(&sentences(&stood, STOOD_STORY_CHARS), "${1}Claude ")
                .into_owned();
            (
                [asked, stood]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" Where it stood: "),
                true,
            )
        }
        _ => (
            sentences(&plain(body), ASKED_STORY_CHARS + STOOD_STORY_CHARS),
            false,
        ),
    };
    let story = crate::extraction::provenance::extractable(&story)?;
    (!crate::extraction::provenance::is_csr_emission(&story)).then_some((story, templated))
}

/// Prose from a markdown section: no emphasis, code ticks, bullets or headings, one line.
fn plain(markdown: &str) -> String {
    static MARKS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*(?:[-*+]|\d+\.|#+)\s+|\*\*|__|`").unwrap());
    MARKS
        .replace_all(markdown, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whole sentences from the start of `text`, as many as fit in `cap` characters; the first
/// alone, cut, when it is longer.
fn sentences(text: &str, cap: usize) -> String {
    static END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.!?](?:\s|$)").unwrap());
    let mut kept = 0;
    for end in END.find_iter(text) {
        let stop = end.start() + 1;
        if text[..stop].chars().count() > cap {
            break;
        }
        kept = stop;
    }
    if kept == 0 {
        let cut: String = text.chars().take(cap).collect();
        return if cut.len() < text.len() {
            format!("{}…", cut.trim_end())
        } else {
            cut
        };
    }
    text[..kept].to_string()
}

/// Build story context by retrieving data from the engine's DB + search index.
pub async fn build_story_context(
    engine: &Engine,
    cwd: &str,
    messages: &[serde_json::Value],
    queued: &[crate::import::QueuedPrompt],
) -> StoryContext {
    let project_name = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());

    // 1. Sample transcript
    let excerpt = story_excerpt(messages, queued);

    // 2. Get V3 enrichment for this project's most recent conversation
    let v3 = get_latest_v3_enrichment(engine.storage(), &project_name);

    // 3. HNSW search for related conversations
    let related = find_related_sessions(
        engine.embeddings(),
        engine.search(),
        engine.storage(),
        &excerpt,
    )
    .await;

    StoryContext {
        current_session_excerpt: excerpt,
        current_v3_enrichment: v3,
        related_sessions: related,
        project_name,
    }
}

/// The transcript sample a story is told from, with the prompts typed while Claude was busy at
/// their places.
fn story_excerpt(messages: &[serde_json::Value], queued: &[crate::import::QueuedPrompt]) -> String {
    sample_transcript(
        &with_queued_prompts(messages, queued, 0..messages.len()),
        TRANSCRIPT_SAMPLE_MSGS,
        TRANSCRIPT_CHAR_CAP,
    )
}

/// The messages of `range` a summary story reads beside Claude Code's summary, with the prompts
/// typed while Claude was busy at their places.
fn story_tail(
    messages: &[serde_json::Value],
    queued: &[crate::import::QueuedPrompt],
    range: std::ops::Range<usize>,
) -> String {
    sample_transcript(
        &with_queued_prompts(messages, queued, range),
        TAIL_MSGS,
        TRANSCRIPT_CHAR_CAP,
    )
}

/// Get the latest V3 enrichment reflection for this project.
fn get_latest_v3_enrichment(storage: &Arc<Storage>, project: &str) -> Option<String> {
    let tag = format!("project_{}", project);
    let reflections = storage.get_reflections_by_tag(&tag, 5).ok()?;
    reflections
        .into_iter()
        .find(|(_, _, tags, _)| {
            // a Shift Ledger row is `narrative_shift`: not a session analysis
            !crate::story_map::store::is_ledger_row(tags)
                && tags.iter().any(|t| t.starts_with("narrative_"))
        })
        .map(|(_, content, _, _)| {
            // Cap at 1500 chars for prompt budget
            content.chars().take(1500).collect()
        })
}

/// HNSW search for related sessions using the transcript excerpt as query.
async fn find_related_sessions(
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    storage: &Arc<Storage>,
    excerpt: &str,
) -> Vec<RelatedSession> {
    // Use first 500 chars as query text
    let query_text: String = excerpt.chars().take(500).collect();
    if query_text.len() < 20 {
        return vec![];
    }

    let emb = embeddings.clone();
    let q = query_text.clone();
    let embed_result = tokio::task::spawn_blocking(move || emb.embed(&[q.as_str()])).await;

    let vec = match embed_result {
        Ok(Ok(vecs)) => match vecs.into_iter().next() {
            Some(v) => v,
            None => return vec![],
        },
        _ => return vec![],
    };

    let idx = search.read().await;
    let results = idx.search_reflections(&vec, 3, 0.4);
    drop(idx);

    let now = chrono::Utc::now();
    let mut related = Vec::new();
    for r in results {
        if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(&r.id) {
            // shift ledger rows never feed a story prompt
            if crate::story_map::store::is_ledger_row(&tags) {
                continue;
            }
            let age = crate::temporal::format_relative_time(
                &crate::temporal::parse_timestamp(&timestamp).unwrap_or(now),
            );
            related.push(RelatedSession {
                summary: content.chars().take(300).collect(),
                age_label: age,
                score: r.score,
            });
        }
    }
    related
}

/// Maximum time to wait for the story lock before giving up.
const STORY_LOCK_TIMEOUT: Duration = Duration::from_secs(90);

/// CLI entrypoint for `csr-engine generate-story`, run as a detached process: from SessionEnd
/// (a first story, or the opt-in rewrite) and from SessionStart after a compaction (the story
/// from Claude Code's summary). Reads the transcript, decides whether a story is due, writes it.
///
/// Uses an advisory file lock to serialize concurrent Haiku calls — when multiple sessions end
/// simultaneously, processes queue up instead of competing for the `claude` CLI.
pub async fn generate_story_cli(
    engine: &Engine,
    transcript: &std::path::Path,
    cwd: &str,
    mode: StoryMode,
) -> anyhow::Result<()> {
    let conv_id = transcript
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let project = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());

    if !transcript.exists() {
        log_story_event(&project, &conv_id, "skip:file_missing");
        return Ok(());
    }

    let (mut messages, mut queued) =
        crate::import::parse_jsonl_messages_for_search_with_queued(transcript)?;
    if messages.len() < 5 {
        log_story_event(
            &project,
            &conv_id,
            &format!("skip:too_few_messages({})", messages.len()),
        );
        return Ok(());
    }
    let mut prior = prior_story(engine.storage(), &conv_id);
    let mut due = story_due(
        mode,
        prior.as_ref(),
        messages.len(),
        newest_compaction_summary(&messages).map(|(at, _)| at),
        asked_since(&messages, &queued, prior.as_ref()),
    );
    if mode == StoryMode::FromCompaction && due.is_err() {
        // SessionStart can run a moment before the summary reaches the transcript: read it again once
        tokio::time::sleep(Duration::from_secs(5)).await;
        (messages, queued) =
            crate::import::parse_jsonl_messages_for_search_with_queued(transcript)?;
        prior = prior_story(engine.storage(), &conv_id);
        due = story_due(
            mode,
            prior.as_ref(),
            messages.len(),
            newest_compaction_summary(&messages).map(|(at, _)| at),
            asked_since(&messages, &queued, prior.as_ref()),
        );
    }
    if let Err(skip) = due {
        log_story_event(&project, &conv_id, &skip);
        return Ok(());
    }

    // Acquire advisory lock — serializes concurrent Haiku calls.
    // Blocks until the lock is available or timeout expires.
    let lock_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".claude-self-reflect")
        .join("story-generation.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;

    let acquired = acquire_lock_with_timeout(&lock_file, STORY_LOCK_TIMEOUT);
    if !acquired {
        log_story_event(&project, &conv_id, "skip:lock_timeout(90s)");
        return Ok(());
    }
    log_story_event(&project, &conv_id, "lock_acquired");

    // Again under the lock: retries and races
    let prior = prior_story(engine.storage(), &conv_id);
    let summary = newest_compaction_summary(&messages);
    if let Err(skip) = story_due(
        mode,
        prior.as_ref(),
        messages.len(),
        summary.as_ref().map(|(at, _)| *at),
        asked_since(&messages, &queued, prior.as_ref()),
    ) {
        log_story_event(&project, &conv_id, &skip);
        return Ok(());
    }

    let result = match (mode, summary) {
        (StoryMode::First, _) | (StoryMode::Rewrite, None) => {
            // Lock held — generate story (Haiku call happens here)
            generate_and_store(
                engine,
                cwd,
                &conv_id,
                &project,
                &messages,
                &queued,
                prior.as_ref(),
            )
            .await
        }
        (StoryMode::FromCompaction, None) => Ok(()), // story_due refused it already
        (_, Some((at, text))) => {
            // Beside the summary, Haiku reads what it never saw: the messages just before it after
            // a compaction, everything after it at the session's end
            let by_haiku = mode == StoryMode::Rewrite || story_rewrites_enabled();
            let after = if mode == StoryMode::FromCompaction {
                at.saturating_sub(TAIL_MSGS)..at
            } else {
                (at + 1).min(messages.len())..messages.len()
            };
            let mut told = None;
            if by_haiku {
                let tail = story_tail(&messages, &queued, after);
                told = haiku_story(
                    engine,
                    &summary_story_prompt(&text, &tail),
                    &project,
                    &conv_id,
                )
                .await
                .map(|(story, shifts)| (story, shifts, "story_by_haiku"));
            }
            if told.is_none() && mode == StoryMode::FromCompaction {
                // Not opted in, or Haiku failed: CSR's own extract, no model call
                told = story_from_compaction_summary(&text).map(|(story, templated)| {
                    if !templated {
                        log_story_event(&project, &conv_id, "aux_schema_miss:compaction_summary");
                    }
                    let shifts =
                        crate::story_map::derive::derive_receipt_shifts(engine.storage(), &conv_id);
                    (story, shifts, "story_extracted")
                });
            }
            match told {
                Some((story, shifts, how)) => {
                    let mut tags = vec![how.to_string()];
                    if mode == StoryMode::FromCompaction {
                        tags.push("story_from_compaction".to_string());
                    }
                    let rev = prior.as_ref().map_or(1, |p| p.rev + 1);
                    store_story(
                        engine,
                        cwd,
                        &conv_id,
                        &project,
                        &story,
                        prior.as_ref(),
                        messages.len(),
                        snapshot_cutoff(&messages, &queued),
                        tags,
                    )
                    .await?;
                    let by = if how == "story_by_haiku" {
                        "Haiku"
                    } else {
                        crate::story_map::store::RECEIPTS_BY
                    };
                    crate::story_map::store::record_story_map(
                        engine,
                        &project,
                        &conv_id,
                        rev,
                        messages.len(),
                        by,
                        shifts,
                    )
                    .await;
                    Ok(())
                }
                None => {
                    log_story_event(&project, &conv_id, "skip:summary_unusable");
                    Ok(())
                }
            }
        }
    };

    // Release lock (drop closes the file, releasing the advisory lock)
    drop(lock_file);

    result
}

/// One metered Haiku call for a story: its prose and parsed shift entries, or None when
/// narratives are off or the call failed. A failed call is metered too.
async fn haiku_story(
    engine: &Engine,
    prompt: &str,
    project: &str,
    conv_id: &str,
) -> Option<(String, Vec<crate::story_map::ShiftEntry>)> {
    if crate::narrative::narratives_disabled() {
        log_story_event(project, conv_id, "skip:narratives_disabled");
        return None;
    }
    let parsed = call_claude_headless(prompt).await;
    let row = match &parsed {
        Some(parsed) => crate::storage::NarrativeUsageRow {
            call_site: "story".into(),
            model: parsed.model.clone(),
            input_tokens: parsed.input_tokens,
            output_tokens: parsed.output_tokens,
            cache_read_tokens: parsed.cache_read_tokens,
            cache_creation_tokens: parsed.cache_creation_tokens,
            duration_ms: 0, // HAIKU_TIMEOUT bounds it; wall-clock not tracked on this path
            success: true,
        },
        None => crate::storage::NarrativeUsageRow {
            call_site: "story".into(),
            model: "unknown".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            duration_ms: 0,
            success: false,
        },
    };
    let _ = engine.storage().record_narrative_usage(&row);
    if parsed.is_none() {
        log_story_event(project, conv_id, "skip:haiku_unavailable_or_timeout");
    }
    parsed.map(|parsed| {
        let (prose, entries) = crate::story_map::finish_story_output(
            &parsed.text,
            !crate::story_map::story_map_disabled(),
        );
        if !entries.is_empty() {
            log_story_event(
                project,
                conv_id,
                &format!("shifts_parsed:{}", entries.len()),
            );
        }
        (prose, entries)
    })
}

/// Generate story and store it. Separated so the lock scope is clear.
async fn generate_and_store(
    engine: &Engine,
    cwd: &str,
    conv_id: &str,
    project: &str,
    messages: &[serde_json::Value],
    queued: &[crate::import::QueuedPrompt],
    prior: Option<&PriorStory>,
) -> anyhow::Result<()> {
    let ctx = build_story_context(engine, cwd, messages, queued).await;
    let Some((story, shifts)) = haiku_story(engine, &build_prompt(&ctx), project, conv_id).await
    else {
        return Ok(());
    };
    let rev = prior.map_or(1, |p| p.rev + 1);
    store_story(
        engine,
        cwd,
        conv_id,
        project,
        &story,
        prior,
        messages.len(),
        snapshot_cutoff(messages, queued),
        Vec::new(),
    )
    .await?;
    crate::story_map::store::record_story_map(
        engine,
        project,
        conv_id,
        rev,
        messages.len(),
        "Haiku",
        shifts,
    )
    .await;
    Ok(())
}

/// Store a session's story in place of the one it had, and how many of its messages it covers.
#[allow(clippy::too_many_arguments)]
async fn store_story(
    engine: &Engine,
    cwd: &str,
    conv_id: &str,
    project: &str,
    story: &str,
    prior: Option<&PriorStory>,
    covered: usize,
    told_through: Option<chrono::DateTime<chrono::Utc>>,
    extra_tags: Vec<String>,
) -> anyhow::Result<()> {
    let _operation = engine.storage().operation_lock()?;
    let reflection_id = format!("story_{}", conv_id);
    let rev = prior.map_or(1, |p| p.rev + 1);
    let project_name = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());
    let mut tags = vec![
        "session_story".to_string(),
        format!("project_{project_name}"),
        format!("conv_{}", conv_id),
        format!("{STORY_REV_TAG}{rev}"),
    ];
    tags.extend(extra_tags);

    crate::mcp::tools::store_reflection_internal(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        story,
        &tags,
    )
    .await?;

    // The new story replaces the old: one story per session, the latest
    if let Some(prior) = prior {
        for old in &prior.ids {
            let _ = engine.storage().delete_reflection(old);
        }
        let mut idx = engine.search().write().await;
        for old in &prior.ids {
            idx.remove_reflection(old);
        }
    }

    // Record how much of the session it covers: what the next story is measured against
    let _ = engine
        .storage()
        .mark_story_written(conv_id, &reflection_id, covered, told_through);

    log_story_event(
        project,
        conv_id,
        &format!("stored:rev{rev}:{}chars", story.len()),
    );
    eprintln!("CSR: stored session story ({} chars)", story.len());
    Ok(())
}

/// Try to acquire an exclusive file lock, polling with backoff up to `timeout`.
/// Returns true if acquired, false if timed out.
fn acquire_lock_with_timeout(file: &std::fs::File, timeout: Duration) -> bool {
    use fs2::FileExt;
    let start = std::time::Instant::now();
    let mut sleep_ms = 100u64; // start at 100ms, backoff to 1s

    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return true,
            Err(_) => {
                if start.elapsed() >= timeout {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(sleep_ms));
                sleep_ms = (sleep_ms * 2).min(1000); // exponential backoff, cap at 1s
            }
        }
    }
}

/// Spawn a detached `csr-engine generate-story` process. Returns immediately.
/// The child process outlives the caller — safe for SessionEnd.
pub fn spawn_detached_story_generation(transcript: &str, cwd: &str, mode: StoryMode) {
    if mode != StoryMode::FromCompaction && crate::narrative::narratives_disabled() {
        return;
    }
    let project = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());
    let conv_id = std::path::Path::new(transcript)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let exe = match crate::process::detached_child_exe() {
        Ok(Some(e)) => e,
        // A cargo test harness driving the hook in-process: not an engine.
        Ok(None) => return,
        Err(e) => {
            eprintln!("CSR: cannot find own exe for story generation: {}", e);
            log_story_event(&project, &conv_id, &format!("error:no_exe({})", e));
            return;
        }
    };

    let mut cmd = std::process::Command::new(exe);
    cmd.args(["generate-story", "--transcript", transcript, "--cwd", cwd]);
    match mode {
        StoryMode::First => {}
        StoryMode::FromCompaction => {
            cmd.arg("--from-compaction");
        }
        StoryMode::Rewrite => {
            cmd.arg("--rewrite");
        }
    }
    match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit()) // keep stderr for debug logs
        .spawn()
    {
        Ok(_) => {
            log_story_event(&project, &conv_id, &format!("spawned:{mode:?}"));
            eprintln!("CSR: spawned background story generation");
        }
        Err(e) => {
            log_story_event(&project, &conv_id, &format!("error:spawn_failed({})", e));
            eprintln!("CSR: failed to spawn story generation: {}", e);
        }
    }
}

/// Backfill stories for all conversations that have V3/heuristic enrichment but no story.
/// Tier 1: V3 → synthesize locally (free). Tier 2: Heuristic → template (free).
pub async fn backfill_stories_cli(engine: &Engine, dry_run: bool) -> anyhow::Result<()> {
    let candidates = engine
        .storage()
        .get_conversations_missing_stories(crate::hooks::session_start::MIN_ENRICHED_MESSAGES)?;
    eprintln!("CSR: {} conversations missing stories", candidates.len());

    let mut tier1 = 0usize;
    let mut tier2 = 0usize;

    for (conv_id, enrichment_type, reflection_id) in &candidates {
        // Get the enrichment content
        let content = match engine.storage().get_reflection_by_id(reflection_id)? {
            Some((content, _, _)) => content,
            None => continue, // phantom reference
        };

        // Derived from `import_state.file_path` via the same attribution
        // function the importer uses, not from `chunks.project_name`: a
        // sidechain transcript's chunk row can be permanently stuck on the
        // daemon-side rescope target ("subagents") once its source file is
        // gone, because `rescope_sidechain_conversation` only runs when the
        // file is (re)discovered. Falls back to the chunk row when no
        // recorded import path resolves (e.g. a synthetic/test conversation)
        // OR when the recorded path resolves to "unknown" (a path outside
        // `projects_dir`) — see `project_for_conversation_or_attribution`'s
        // doc for why a bare `.map(...).or_else(...)` was not enough.
        let project = engine
            .storage()
            .import_paths_for_conversations(std::slice::from_ref(conv_id), 1)?
            .get(conv_id)
            .and_then(|paths| paths.first())
            .map(|path| {
                import::project_for_conversation_or_attribution(
                    engine.storage(),
                    engine.projects_dir(),
                    Path::new(path),
                    conv_id,
                )
            })
            .or_else(|| {
                engine
                    .storage()
                    .get_project_for_conversation(conv_id)
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| "unknown".to_string());

        let story = if enrichment_type == "extracted_v3" {
            tier1 += 1;
            crate::extraction::story::synthesize_story_from_v3(&content, &project)
        } else {
            tier2 += 1;
            Some(crate::extraction::story::synthesize_story_from_heuristic(
                &content, &project,
            ))
        };

        if let Some(story_text) = story {
            if !dry_run {
                let story_id = format!("story_{}", conv_id);
                let tags = vec![
                    "session_story".to_string(),
                    format!("project_{}", project),
                    format!("conv_{}", conv_id),
                ];
                crate::mcp::tools::store_reflection_internal(
                    engine.storage(),
                    engine.embeddings(),
                    engine.search(),
                    &story_text,
                    &tags,
                )
                .await?;
                engine
                    .storage()
                    .mark_enrichment_completed(conv_id, "session_story", &story_id)?;
            }
            let cid_short = if conv_id.len() > 8 {
                &conv_id[..8]
            } else {
                conv_id
            };
            eprintln!(
                "  [{}] {} ({}chars)",
                if enrichment_type == "extracted_v3" {
                    "V3"
                } else {
                    "heuristic"
                },
                cid_short,
                story_text.len()
            );
        }
    }

    eprintln!(
        "CSR backfill: {} V3-synthesized, {} heuristic-templated{}",
        tier1,
        tier2,
        if dry_run { " (dry run)" } else { "" }
    );
    Ok(())
}

/// Log story generation events to hook-timing.log for diagnostics.
fn log_story_event(project: &str, conv_id: &str, event: &str) {
    let cid_short = if conv_id.len() > 8 {
        &conv_id[..8]
    } else {
        conv_id
    };
    crate::telemetry::append_timing_line(&format!(
        "CSR story [{}] conv={}: {}",
        project, cid_short, event
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prompt_with_full_context() {
        let ctx = StoryContext {
            current_session_excerpt: "user: fix the bug\nassistant: done".to_string(),
            current_v3_enrichment: Some("Tools: Edit | Files: main.rs".to_string()),
            related_sessions: vec![RelatedSession {
                summary: "Fixed similar bug in auth module".to_string(),
                age_label: "2d ago".to_string(),
                score: 0.72,
            }],
            project_name: "my-project".to_string(),
        };
        let prompt = build_prompt(&ctx);
        assert!(prompt.contains("CURRENT SESSION TRANSCRIPT"));
        assert!(prompt.contains("fix the bug"));
        assert!(prompt.contains("STRUCTURED ANALYSIS"));
        assert!(prompt.contains("Tools: Edit"));
        assert!(prompt.contains("RELATED PAST SESSIONS"));
        assert!(prompt.contains("72%"));
        assert!(prompt.contains("2-3 sentences"));
    }

    #[test]
    fn test_build_prompt_minimal() {
        let ctx = StoryContext {
            current_session_excerpt: "user: hello".to_string(),
            current_v3_enrichment: None,
            related_sessions: vec![],
            project_name: "test".to_string(),
        };
        let prompt = build_prompt(&ctx);
        assert!(prompt.contains("hello"));
        assert!(!prompt.contains("STRUCTURED ANALYSIS"));
        assert!(!prompt.contains("RELATED PAST SESSIONS"));
    }

    fn prior(rev: usize, covered: Option<usize>) -> PriorStory {
        PriorStory {
            rev,
            covered,
            ..PriorStory::default()
        }
    }

    #[test]
    fn a_first_story_is_written_once() {
        assert_eq!(story_due(StoryMode::First, None, 6, None, 0), Ok(()));
        assert_eq!(
            story_due(StoryMode::First, Some(&prior(1, None)), 4736, None, 3),
            Err("skip:story_exists".into())
        );
    }

    #[test]
    fn every_compaction_retells_the_story_and_nothing_caps_it() {
        let mode = StoryMode::FromCompaction;
        assert_eq!(
            story_due(mode, None, 900, None, 0),
            Err("skip:no_compaction_summary".into())
        );
        assert_eq!(story_due(mode, None, 900, Some(880), 0), Ok(()));
        // a story from before the count was kept has not taken any summary in
        assert_eq!(
            story_due(mode, Some(&prior(1, None)), 900, Some(880), 0),
            Ok(())
        );
        // the twelfth compaction is told like the second
        assert_eq!(
            story_due(mode, Some(&prior(11, Some(4200))), 4736, Some(4700), 0),
            Ok(())
        );
        // the same summary is never told twice (a resumed session, a retry)
        assert_eq!(
            story_due(mode, Some(&prior(12, Some(4736))), 4736, Some(4700), 0),
            Err("skip:summary_told(4700)".into())
        );
    }

    #[test]
    fn haiku_retells_the_ending_only_when_the_person_asked_something_since() {
        let mode = StoryMode::Rewrite;
        assert_eq!(story_due(mode, None, 900, None, 0), Ok(()));
        // 4736 of 4800 told, but a new ask came after: the ending is due, however small a share
        assert_eq!(
            story_due(mode, Some(&prior(12, Some(4736))), 4800, None, 1),
            Ok(())
        );
        // the agent worked on, nothing new was asked: the story stands
        assert_eq!(
            story_due(mode, Some(&prior(12, Some(4736))), 4800, None, 0),
            Err("skip:nothing_asked_since(4736/4800)".into())
        );
        // nothing after the last message, but something asked: due all the same
        assert_eq!(
            story_due(mode, Some(&prior(3, Some(4800))), 4800, None, 2),
            Ok(())
        );
        assert_eq!(
            story_due(mode, Some(&prior(3, Some(4800))), 4800, None, 0),
            Err("skip:nothing_asked_since(4800/4800)".into())
        );
        assert_eq!(
            story_due(mode, Some(&prior(1, None)), 4000, None, 0),
            Ok(())
        );
    }

    #[test]
    fn asked_since_counts_only_what_came_after_the_story() {
        let user = |text: &str| serde_json::json!({"type": "user", "message": {"role": "user", "content": text}});
        let messages = vec![user("old ask"), user("told ask"), user("new ask")];
        assert_eq!(asked_since(&messages, &[], None), 3);
        assert_eq!(asked_since(&messages, &[], Some(&prior(1, Some(2)))), 1);
        assert_eq!(asked_since(&messages, &[], Some(&prior(1, Some(9)))), 0);
    }

    /// A transcript with `told` user/assistant rows, then `tail` raw lines.
    fn transcript_with(told: usize, tail: &[serde_json::Value]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut body = String::new();
        for n in 0..told {
            let row = if n % 2 == 0 {
                serde_json::json!({"type": "user", "message": {"role": "user", "content": format!("ask {n}")}})
            } else {
                serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "done"}})
            };
            body.push_str(&format!("{row}\n"));
        }
        for row in tail {
            body.push_str(&format!("{row}\n"));
        }
        std::fs::write(dir.path().join("s.jsonl"), body).unwrap();
        dir
    }

    /// Is a rewrite due for a story told when the last assistant row had not come yet?
    fn rewrite_due(dir: &tempfile::TempDir, told: usize) -> Result<(), String> {
        let (messages, queued) =
            crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
                .unwrap();
        let prior = prior(1, Some(told - 1));
        story_due(
            StoryMode::Rewrite,
            Some(&prior),
            messages.len(),
            None,
            asked_since(&messages, &queued, Some(&prior)),
        )
    }

    fn queued_row(prompt: serde_json::Value, human: bool, at: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "attachment", "timestamp": at,
            "attachment": {"type": "queued_command", "prompt": prompt, "humanTurn": human}
        })
    }

    #[test]
    fn a_prompt_typed_while_claude_was_busy_makes_a_rewrite_due() {
        let dir = transcript_with(
            6,
            &[queued_row(
                serde_json::json!("also fix the pane"),
                true,
                "2026-10-14T20:59:56Z",
            )],
        );
        assert_eq!(rewrite_due(&dir, 6), Ok(()));
        // The same prompt as text blocks.
        let dir = transcript_with(
            6,
            &[queued_row(
                serde_json::json!([{"type": "text", "text": "also fix the pane"}]),
                true,
                "2026-10-14T20:59:56Z",
            )],
        );
        assert_eq!(rewrite_due(&dir, 6), Ok(()));
    }

    #[test]
    fn a_queued_command_nobody_typed_still_skips() {
        let dir = transcript_with(
            6,
            &[
                queued_row(
                    serde_json::json!("<task-notification>build finished</task-notification>"),
                    false,
                    "2026-10-14T20:59:56Z",
                ),
                queued_row(
                    serde_json::json!("not flagged"),
                    false,
                    "2026-10-14T21:00:00Z",
                ),
            ],
        );
        assert_eq!(
            rewrite_due(&dir, 6),
            Err("skip:nothing_asked_since(5/6)".to_string())
        );
    }

    #[test]
    fn a_queued_prompt_that_also_arrives_as_a_user_row_counts_once() {
        let typed = "also fix the pane";
        let queued = queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z");
        let twice = queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z");
        let row =
            serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}});
        let dir = transcript_with(6, &[queued, twice, row]);
        let (messages, queued) =
            crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
                .unwrap();
        assert_eq!(queued.len(), 1, "one enqueue written twice");
        let prior = prior(1, Some(6));
        assert_eq!(asked_since(&messages, &queued, Some(&prior)), 1);
        // and a queued prompt before the story's coverage is not new
        let before = prior_covering_all(&messages);
        assert_eq!(asked_since(&messages, &queued, Some(&before)), 0);
    }

    /// Indexing queued prompts adds chunk rows of their own; it must not move what the
    /// story reads. The messages the summarizer gets carry no queued prompt, so a stored
    /// `covered` count still indexes into them, and `asked_since` counts as it did.
    #[test]
    fn queued_prompts_leave_the_storys_messages_and_counts_alone() {
        let row = |n: usize| {
            if n.is_multiple_of(2) {
                serde_json::json!({"type": "user", "message": {"role": "user", "content": format!("ask {n}")}})
            } else {
                serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "done"}})
            }
        };
        let write = |name: &str, rows: &[serde_json::Value]| {
            let dir = tempfile::tempdir().unwrap();
            let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
            std::fs::write(dir.path().join(name), body).unwrap();
            dir
        };
        let alpha = queued_row(
            serde_json::json!("alpha while busy"),
            true,
            "2026-10-14T20:00:00Z",
        );
        let beta = queued_row(
            serde_json::json!("beta while busy"),
            true,
            "2026-10-14T20:01:00Z",
        );
        let plain = [row(0), row(1), row(2), row(3), row(4), row(5)];
        let with_queued = [
            row(0),
            row(1),
            row(2),
            alpha.clone(),
            row(3),
            row(4),
            row(5),
            beta.clone(),
        ];
        let parse = |dir: &tempfile::TempDir| {
            crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
                .unwrap()
        };
        let without = write("s.jsonl", &plain);
        let with = write("s.jsonl", &with_queued);
        let (plain_messages, plain_queued) = parse(&without);
        let (messages, queued) = parse(&with);
        assert_eq!(messages.len(), 6);
        assert_eq!(
            messages, plain_messages,
            "no queued prompt inside the messages"
        );
        assert!(plain_queued.is_empty());
        assert_eq!(
            queued
                .iter()
                .map(|q| (q.place, q.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(3, "alpha while busy"), (6, "beta while busy")]
        );
        // A story told over the first four messages: one user row after it, the
        // prompt queued inside its coverage was told, the one after the end is new.
        let told = prior(1, Some(4));
        assert_eq!(asked_since(&plain_messages, &[], Some(&told)), 1);
        assert_eq!(asked_since(&messages, &queued, Some(&told)), 2);
    }

    /// Is a rewrite due for a story that covers every message the transcript has?
    fn rewrite_due_covering_all(dir: &tempfile::TempDir) -> Result<(), String> {
        let (messages, queued) =
            crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
                .unwrap();
        let prior = prior_covering_all(&messages);
        story_due(
            StoryMode::Rewrite,
            Some(&prior),
            messages.len(),
            None,
            asked_since(&messages, &queued, Some(&prior)),
        )
    }

    #[test]
    fn a_prompt_queued_after_the_last_message_makes_a_story_covering_everything_due() {
        let human = transcript_with(
            6,
            &[queued_row(
                serde_json::json!("also fix the pane"),
                true,
                "2026-10-14T20:59:56Z",
            )],
        );
        assert_eq!(rewrite_due_covering_all(&human), Ok(()));
        // and nothing typed still skips
        let robot = transcript_with(
            6,
            &[queued_row(
                serde_json::json!("<task-notification>done</task-notification>"),
                false,
                "2026-10-14T20:59:56Z",
            )],
        );
        assert_eq!(
            rewrite_due_covering_all(&robot),
            Err("skip:nothing_asked_since(6/6)".to_string())
        );
    }

    #[test]
    fn a_queued_prompt_holding_a_secret_counts_once_with_its_scrubbed_user_row() {
        let typed = format!("rotate this key {}{}", "ghp_", "Q".repeat(36));
        let queued = queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z");
        let row =
            serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}});
        let dir = transcript_with(6, &[queued, row]);
        let (messages, queued) =
            crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
                .unwrap();
        assert_eq!(queued.len(), 1);
        assert!(
            !queued[0].text.contains("ghp_"),
            "the queued text is scrubbed like the row: {}",
            queued[0].text
        );
        let prior = prior(1, Some(6));
        assert_eq!(asked_since(&messages, &queued, Some(&prior)), 1);
    }

    #[test]
    fn a_prompt_queued_inside_the_story_is_not_asked_again_when_its_user_row_lands_after_it() {
        let typed = "also fix the pane";
        let assistant = |text: &str| serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": text}});
        // Queued after four messages; two assistant rows follow; the story covers six;
        // only then does the prompt arrive as a user row.
        let dir = transcript_with(
            4,
            &[
                queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z"),
                assistant("one"),
                assistant("two"),
                serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}}),
            ],
        );
        let (messages, queued) =
            crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
                .unwrap();
        assert_eq!((messages.len(), queued[0].place), (7, 4));
        let told = prior(1, Some(6));
        assert_eq!(asked_since(&messages, &queued, Some(&told)), 0);
        // The same text typed again, with no queued twin, is a new ask.
        let again =
            serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}});
        let mut more = messages.clone();
        more.push(again);
        assert_eq!(asked_since(&more, &queued, Some(&told)), 1);
        // And a queued prompt whose row came before the edge leaves a later copy new.
        let early = prior(1, Some(7));
        assert_eq!(asked_since(&messages, &queued, Some(&early)), 0);
        assert_eq!(asked_since(&more, &queued, Some(&early)), 1);
    }

    fn written(covered: usize, at: &str) -> PriorStory {
        PriorStory {
            written_at: crate::temporal::parse_timestamp(at),
            ..prior(1, Some(covered))
        }
    }

    fn parsed(
        dir: &tempfile::TempDir,
    ) -> (Vec<serde_json::Value>, Vec<crate::import::QueuedPrompt>) {
        crate::import::parse_jsonl_messages_for_search_with_queued(&dir.path().join("s.jsonl"))
            .unwrap()
    }

    /// A rewrite that has nothing new to cover leaves `covered` where it was. The prompt that
    /// was queued after the last message is told by then, and must not count again.
    #[test]
    fn a_queued_prompt_the_story_was_written_after_is_not_asked_again() {
        let dir = transcript_with(
            6,
            &[queued_row(
                serde_json::json!("also fix the pane"),
                true,
                "2026-10-14T20:59:56Z",
            )],
        );
        let (messages, queued) = parsed(&dir);
        assert_eq!(queued[0].place, 6);
        // First rewrite: the story is older than the prompt, so it is a new ask.
        let before = written(6, "2026-10-14T20:00:00Z");
        assert_eq!(asked_since(&messages, &queued, Some(&before)), 1);
        // After the rewrite: covered is still 6, but the story postdates the prompt.
        let after = written(6, "2026-10-14T21:05:00Z");
        assert_eq!(asked_since(&messages, &queued, Some(&after)), 0);
        assert_eq!(
            story_due(
                StoryMode::Rewrite,
                Some(&after),
                messages.len(),
                None,
                asked_since(&messages, &queued, Some(&after)),
            ),
            Err("skip:nothing_asked_since(6/6)".to_string())
        );
        // A story whose time is unknown cannot rule the prompt out.
        assert_eq!(asked_since(&messages, &queued, Some(&prior(1, Some(6)))), 1);
        // Nor can a prompt whose row carries no time.
        let undated = transcript_with(
            6,
            &[queued_row(
                serde_json::json!("also fix the pane"),
                true,
                "not a time",
            )],
        );
        let (messages, queued) = parsed(&undated);
        assert_eq!(queued[0].at, None);
        assert_eq!(asked_since(&messages, &queued, Some(&after)), 1);
        // A prompt typed after the story was written is new.
        let newer = transcript_with(
            6,
            &[queued_row(
                serde_json::json!("and one more thing"),
                true,
                "2026-10-14T21:10:00Z",
            )],
        );
        let (messages, queued) = parsed(&newer);
        assert_eq!(asked_since(&messages, &queued, Some(&after)), 1);
    }

    /// The story is generated from a snapshot, then waits on a lock and a model call before it
    /// is stored. A prompt typed in that window is after the snapshot's newest line, so the
    /// next rewrite check still sees it as new, though it landed before the story was stored.
    #[test]
    fn a_prompt_typed_while_the_story_was_generated_is_still_a_new_ask() {
        let stamped = |n: usize, at: &str| {
            let role = if n.is_multiple_of(2) {
                "user"
            } else {
                "assistant"
            };
            serde_json::json!({
                "type": role, "timestamp": at,
                "message": {"role": role, "content": format!("line {n}")}
            })
        };
        let snapshot_dir = tempfile::tempdir().unwrap();
        let mut body = String::new();
        for n in 0..6 {
            body.push_str(&format!("{}\n", stamped(n, "2026-01-01T10:00:00Z")));
        }
        std::fs::write(snapshot_dir.path().join("s.jsonl"), &body).unwrap();
        let (messages, queued) = parsed(&snapshot_dir);
        let cutoff = snapshot_cutoff(&messages, &queued);
        assert_eq!(
            cutoff,
            crate::temporal::parse_timestamp("2026-01-01T10:00:00Z")
        );

        // The story is stored now (long after the snapshot), carrying the snapshot's cutoff.
        let storage = Storage::open_memory().unwrap();
        let tags = vec!["session_story".to_string(), "conv_conv-race".to_string()];
        storage
            .insert_reflection("story", "the story", &tags, &[0.0; 384])
            .unwrap();
        storage
            .mark_story_written("conv-race", "story_conv-race", messages.len(), cutoff)
            .unwrap();
        let prior = prior_story(&storage, "conv-race").unwrap();
        assert_eq!(prior.told_through, cutoff);

        // Typed after the snapshot, before the story was stored.
        let mut later = body.clone();
        later.push_str(&format!(
            "{}\n",
            queued_row(
                serde_json::json!("typed during generation"),
                true,
                "2026-01-01T10:05:00Z",
            )
        ));
        std::fs::write(snapshot_dir.path().join("s.jsonl"), later).unwrap();
        let (messages, queued) = parsed(&snapshot_dir);
        assert!(prior.written_at.unwrap() > queued[0].at.unwrap());
        assert_eq!(asked_since(&messages, &queued, Some(&prior)), 1);
        assert_eq!(
            story_due(
                StoryMode::Rewrite,
                Some(&prior),
                messages.len(),
                None,
                asked_since(&messages, &queued, Some(&prior)),
            ),
            Ok(())
        );
        // A story from before the cutoff was kept falls back to its write time.
        let legacy = PriorStory {
            told_through: None,
            ..prior
        };
        assert_eq!(asked_since(&messages, &queued, Some(&legacy)), 0);
    }

    /// The story told the queued prompt; its user row landing after the edge is that same
    /// prompt, not a new ask.
    #[test]
    fn the_user_row_of_a_prompt_the_story_told_by_time_is_not_asked_again() {
        let typed = "also fix the pane";
        let dir = transcript_with(
            6,
            &[
                queued_row(serde_json::json!(typed), true, "2026-10-14T20:59:56Z"),
                serde_json::json!({"type": "user", "message": {"role": "user", "content": typed}}),
            ],
        );
        let (messages, queued) = parsed(&dir);
        assert_eq!(messages.len(), 7);
        assert_eq!(
            asked_since(
                &messages,
                &queued,
                Some(&written(6, "2026-10-14T21:05:00Z"))
            ),
            0
        );
        // Written before the prompt was typed, the row is the prompt arriving: one ask.
        assert_eq!(
            asked_since(
                &messages,
                &queued,
                Some(&written(6, "2026-10-14T20:00:00Z"))
            ),
            1
        );
    }

    #[test]
    fn the_story_a_session_has_remembers_when_it_was_written() {
        let storage = Storage::open_memory().unwrap();
        let tags = vec!["session_story".to_string(), "conv_conv-w".to_string()];
        storage
            .insert_reflection("story", "the story", &tags, &[0.0; 384])
            .unwrap();
        let found = prior_story(&storage, "conv-w").unwrap();
        let written = found.written_at.expect("the reflection has a time");
        assert!(
            (chrono::Utc::now() - written).num_seconds().abs() < 60,
            "{written}"
        );
    }

    /// Haiku reads the prompt typed while Claude was busy, where it was typed. The messages
    /// the numbering rests on are the same as before.
    #[test]
    fn the_story_prompt_shows_prompts_typed_while_claude_was_busy() {
        let dir = transcript_with(
            4,
            &[
                queued_row(
                    serde_json::json!("alpha typed mid turn"),
                    true,
                    "2026-10-14T20:00:00Z",
                ),
                serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "working"}}),
                // This one reaches the transcript as a user row too: shown once.
                queued_row(
                    serde_json::json!("beta typed mid turn"),
                    true,
                    "2026-10-14T20:01:00Z",
                ),
                serde_json::json!({"type": "user", "message": {"role": "user", "content": "beta typed mid turn"}}),
                // A harness notice is not the person.
                queued_row(
                    serde_json::json!("<task-notification>gamma</task-notification>"),
                    false,
                    "2026-10-14T20:02:00Z",
                ),
                // Queued after the last message.
                queued_row(
                    serde_json::json!("delta typed at the end"),
                    true,
                    "2026-10-14T20:03:00Z",
                ),
            ],
        );
        let (messages, queued) = parsed(&dir);
        assert_eq!(messages.len(), 6);
        let before = messages.clone();

        let excerpt = story_excerpt(&messages, &queued);
        assert_eq!(messages, before, "the messages are not touched");
        assert_eq!(excerpt.matches("alpha typed mid turn").count(), 1);
        assert_eq!(excerpt.matches("beta typed mid turn").count(), 1);
        assert_eq!(excerpt.matches("delta typed at the end").count(), 1);
        assert!(!excerpt.contains("gamma"));
        let position = |needle: &str| excerpt.find(needle).unwrap();
        // alpha sits after the fourth message and before the assistant row that follows it.
        assert!(position("ask 2") < position("alpha"));
        assert!(position("alpha") < position("working"));
        assert!(position("beta") > position("working"));
        assert!(position("delta") > position("beta"));
        assert!(
            excerpt.contains("[user]: ") && excerpt.contains("alpha"),
            "shown as a user line: {excerpt}"
        );

        // The summary path reads a range of the messages: the same prompts, by place.
        let tail = story_tail(&messages, &queued, 4..messages.len());
        assert!(tail.contains("alpha") && tail.contains("delta"), "{tail}");
        assert!(!tail.contains("ask 2"));
        let prompt = summary_story_prompt("the summary", &tail);
        assert!(prompt.contains("alpha typed mid turn"));
        // A range that stops short of a prompt's place leaves it out.
        let head = story_tail(&messages, &queued, 0..4);
        assert!(!head.contains("alpha") && !head.contains("delta"), "{head}");
    }

    fn prior_covering_all(messages: &[serde_json::Value]) -> PriorStory {
        prior(1, Some(messages.len()))
    }

    #[test]
    fn a_haiku_story_reads_the_summary_and_the_messages_it_never_saw() {
        let prompt = summary_story_prompt(
            "1. Primary Request and Intent: build the reel",
            "[user]: ship v8",
        );
        let at = |needle: &str| prompt.find(needle).unwrap_or_else(|| panic!("{needle}"));
        assert!(at("build the reel") < at("THE LAST MESSAGES"));
        assert!(at("THE LAST MESSAGES") < at("ship v8"));
        assert!(prompt.contains("trust the last messages"));
        assert!(prompt.contains("under 800 characters"));
    }

    const SUMMARY: &str = "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.

Summary:
1. Primary Request and Intent:
   - The user asked to **refresh** the session story at each compaction in `summarizer.rs`. They want it smart, not capped.
   - A second ask about the pane.

2. Key Technical Concepts:
   - enrichment_state holds coverage.

8. Current Work:
   I was wiring the `--from-compaction` mode into SessionStart. Tests pass. I ran them twice.

9. Optional Next Step:
   Install the binary.
";

    #[test]
    fn a_compaction_summary_becomes_what_was_asked_and_where_it_stood() {
        let (story, templated) = story_from_compaction_summary(SUMMARY).unwrap();
        assert!(templated);
        assert_eq!(
            story,
            "The user asked to refresh the session story at each compaction in summarizer.rs. They want it smart, not capped. A second ask about the pane. Where it stood: Claude was wiring the --from-compaction mode into SessionStart. Tests pass. Claude ran them twice."
        );
    }

    #[test]
    fn a_summary_without_the_template_is_read_from_its_start() {
        let (story, templated) = story_from_compaction_summary(
            "Summary:\nWe built the **feed**. It works. "
                .repeat(80)
                .as_str(),
        )
        .unwrap();
        assert!(!templated);
        assert!(story.starts_with("We built the feed. It works."));
        assert!(story.chars().count() <= ASKED_STORY_CHARS + STOOD_STORY_CHARS);
        // a first sentence longer than the room is cut, not dropped
        let long = format!("Summary:\n{}", "word ".repeat(400));
        let (story, _) = story_from_compaction_summary(&long).unwrap();
        assert!(story.ends_with('…'));
    }

    #[test]
    fn the_newest_compaction_summary_is_found_with_its_place() {
        let row = |text: &str, summary: bool| {
            let mut row =
                serde_json::json!({"type": "user", "message": {"role": "user", "content": text}});
            if summary {
                row["isCompactSummary"] = true.into();
            }
            row
        };
        let messages = vec![
            row("ask", false),
            row("old summary", true),
            row("ask again", false),
            row("new summary", true),
            row("after", false),
        ];
        assert_eq!(
            newest_compaction_summary(&messages),
            Some((3, "new summary".to_string()))
        );
        assert_eq!(newest_compaction_summary(&messages[..1]), None);
    }

    #[test]
    fn only_what_the_person_typed_counts_as_asked() {
        let user = |content: serde_json::Value| serde_json::json!({"type": "user", "message": {"role": "user", "content": content}});
        let mut summary = user(serde_json::json!(
            "This session is being continued from a previous conversation"
        ));
        summary["isCompactSummary"] = true.into();
        let mut meta = user(serde_json::json!(
            "<local-command-caveat>caveat</local-command-caveat>"
        ));
        meta["isMeta"] = true.into();
        let messages = vec![
            user(serde_json::json!("first   ask\n with  spaces")),
            summary,
            meta,
            user(
                serde_json::json!([{"type": "tool_result", "tool_use_id": "t", "content": "cargo test ok"}]),
            ),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": "done"}}),
            user(serde_json::json!([{"type": "text", "text": "second ask"}])),
            user(serde_json::json!("third ask is long enough to cut")),
        ];
        assert_eq!(
            typed_prompts(&messages, 10, 15),
            vec!["first ask with", "second ask", "third ask is lo"]
        );
        // the newest ones when there are more than it reads
        assert_eq!(
            typed_prompts(&messages, 1, 300),
            vec!["third ask is long enough to cut"]
        );
    }

    #[test]
    fn the_story_a_session_has_is_its_newest_with_the_highest_number() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::open(&tmp.path().join("csr.db")).unwrap();
        assert!(prior_story(&storage, "conv-a").is_none());
        let tags = |extra: &[&str]| {
            let mut tags = vec!["session_story".to_string(), "conv_conv-a".to_string()];
            tags.extend(extra.iter().map(|t| t.to_string()));
            tags
        };
        storage
            .insert_reflection("old", "first story", &tags(&[]), &[0.0; 384])
            .unwrap();
        storage
            .insert_reflection(
                "other",
                "another session",
                &["session_story".into(), "conv_conv-b".into()],
                &[0.0; 384],
            )
            .unwrap();
        let found = prior_story(&storage, "conv-a").unwrap();
        assert_eq!((found.rev, found.covered, found.ids.len()), (1, None, 1));
        std::thread::sleep(std::time::Duration::from_millis(5));
        storage
            .insert_reflection("new", "second story", &tags(&["story_rev_2"]), &[0.0; 384])
            .unwrap();
        storage
            .mark_story_written("conv-a", "story_conv-a", 640, None)
            .unwrap();
        let found = prior_story(&storage, "conv-a").unwrap();
        assert_eq!((found.rev, found.covered), (2, Some(640)));
        let mut ids = found.ids.clone();
        ids.sort();
        assert_eq!(ids, vec!["new", "old"]);
    }

    #[test]
    fn a_story_rewrite_never_finds_ledger_rows_in_prior_story() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_reflection(
                "story-1",
                "session story prose",
                &[
                    "session_story".to_string(),
                    "conv_c1".to_string(),
                    "story_rev_1".to_string(),
                ],
                &[0.0f32; 384],
            )
            .unwrap();
        for (id, sig) in [
            ("ledger-1", "1111111111111111"),
            ("ledger-2", "2222222222222222"),
        ] {
            storage
                .insert_reflection(
                    id,
                    "[shift: model-derived, kind=shift, rev=1, receipts=evo:1] a => b - claim",
                    &[
                        "narrative_shift".to_string(),
                        "shift_project:csr".to_string(),
                        "shift_conv:c1".to_string(),
                        "shift_rev_1".to_string(),
                        "shift_kind_shift".to_string(),
                        format!("shift_sig_{sig}"),
                        "shift_grounded".to_string(),
                    ],
                    &[0.0f32; 384],
                )
                .unwrap();
        }
        let found = prior_story(&storage, "c1").expect("story exists");
        assert_eq!(found.ids, vec!["story-1".to_string()]);
    }

    #[test]
    fn a_story_rewrite_replaces_the_story_row_and_keeps_every_ledger_row() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_STORY_MAP");

        let storage = std::sync::Arc::new(Storage::open_memory().unwrap());
        let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
        let search = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::search::SearchEngine::new(100),
        ));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            std::path::PathBuf::from("/tmp"),
        );

        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed
                    ) VALUES
                    ('evo_1', 'conv-keep-ledger', 'csr', '/repo/src/a.rs', 'rust', '2026-10-14T00:00:01Z', '[\"f1\"]', '[]', '[]', '[]'),
                    ('evo_2', 'conv-keep-ledger', 'csr', '/repo/src/b.rs', 'rust', '2026-10-14T00:00:02Z', '[\"f2\"]', '[]', '[]', '[]')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let s1 = crate::story_map::parse_shift_line("shift|-|a.rs:f1+|Added f1").unwrap();
        let s2 = crate::story_map::parse_shift_line("shift|-|b.rs:f2+|Added f2").unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            store_story(
                &engine,
                "/repo/csr",
                "conv-keep-ledger",
                "csr",
                "First session story.",
                None,
                10,
                None,
                vec!["story_by_haiku".to_string()],
            )
            .await
            .unwrap();
            crate::story_map::store::record_story_map(
                &engine,
                "csr",
                "conv-keep-ledger",
                1,
                10,
                "Haiku",
                vec![s1, s2],
            )
            .await;

            let ledger_before: Vec<String> = storage
                .get_ledger_reflections_by_tag("narrative_shift", 10)
                .unwrap()
                .into_iter()
                .map(|(id, _, _, _)| id)
                .collect();
            assert_eq!(ledger_before.len(), 2);

            let prior = prior_story(&storage, "conv-keep-ledger");
            assert!(prior.is_some());
            store_story(
                &engine,
                "/repo/csr",
                "conv-keep-ledger",
                "csr",
                "Second rewritten session story.",
                prior.as_ref(),
                25,
                None,
                vec!["story_by_haiku".to_string()],
            )
            .await
            .unwrap();

            let stories = storage
                .get_reflections_by_two_tags("session_story", "conv_conv-keep-ledger", 10)
                .unwrap();
            assert_eq!(stories.len(), 1);
            assert_eq!(stories[0].1, "Second rewritten session story.");

            let ledger_after: Vec<String> = storage
                .get_ledger_reflections_by_tag("narrative_shift", 10)
                .unwrap()
                .into_iter()
                .map(|(id, _, _, _)| id)
                .collect();
            assert_eq!(ledger_after, ledger_before);
        });
    }

    /// A canned model reply, no `claude -p`: the prompt asks for five fields, the reply is
    /// parsed, grounded and stored, and the stored map and the shift reflection carry the
    /// Before and After sentences. A 4-field line in the same reply still lands.
    #[test]
    fn a_canned_five_field_reply_is_stored_with_before_and_after() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_STORY_MAP");

        let prompt = build_prompt(&StoryContext {
            current_session_excerpt: String::new(),
            current_v3_enrichment: None,
            related_sessions: Vec::new(),
            project_name: "csr".into(),
        });
        assert!(prompt.contains("kind|before|after|was|now"), "{prompt}");
        assert!(summary_story_prompt("s", "t").contains("kind|before|after|was|now"));

        let storage = std::sync::Arc::new(Storage::open_memory().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            std::sync::Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
                100,
            ))),
            std::path::PathBuf::from("/tmp"),
        );
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed
                    ) VALUES
                    ('evo_1', 'conv-canned', 'csr', '/repo/src/a.rs', 'rust', '2026-10-14T00:00:01Z', '[\"f1\"]', '[]', '[]', '[]'),
                    ('evo_2', 'conv-canned', 'csr', '/repo/src/b.rs', 'rust', '2026-10-14T00:00:02Z', '[\"f2\"]', '[]', '[]', '[]')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let reply = "The session moved the map to text.\n@shifts\n\
                     shift|-|a.rs:f1+|-|The map writes Before and After as text.\n\
                     shift|-|b.rs:f2+|The map drew its labels at 9 px.|The map writes labels as text rows.\n";
        let (prose, shifts) = crate::story_map::finish_story_output(reply, true);
        assert_eq!(prose, "The session moved the map to text.");
        assert_eq!(shifts.len(), 2);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(crate::story_map::store::record_story_map(
            &engine,
            "csr",
            "conv-canned",
            1,
            10,
            "Haiku",
            shifts,
        ));

        let map = crate::story_map::store::load_story_map(&storage, "conv-canned").expect("map");
        let entries = &map.current.entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].was, None);
        assert_eq!(
            entries[0].now.as_deref(),
            Some("The map writes Before and After as text.")
        );
        assert_eq!(
            entries[1].was.as_deref(),
            Some("The map drew its labels at 9 px.")
        );
        let said: Vec<String> = storage
            .get_ledger_reflections_by_tag("narrative_shift", 10)
            .unwrap()
            .into_iter()
            .map(|(_, content, _, _)| content)
            .collect();
        assert!(said
            .iter()
            .any(|c| c.ends_with("- New: The map writes Before and After as text.")));
        assert!(said.iter().any(|c| c.ends_with(
            "- Before: The map drew its labels at 9 px. After: The map writes labels as text rows."
        )));
    }

    #[test]
    fn the_v3_lookup_never_takes_a_shift_ledger_row_for_an_analysis() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let tag = |t: &str| t.to_string();
        storage
            .insert_reflection(
                "v3",
                "Tools: Edit | Files: main.rs",
                &[tag("project_csr"), tag("narrative_extracted_v3")],
                &[0.0f32; 384],
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        storage
            .insert_reflection(
                "shift",
                "[shift: model-derived, kind=risk, rev=1, receipts=evo:1] a => b - claim",
                &[tag("project_csr"), tag("narrative_shift")],
                &[0.0f32; 384],
            )
            .unwrap();
        let got = get_latest_v3_enrichment(&storage, "csr").unwrap();
        assert!(got.starts_with("Tools: Edit"), "{got}");
    }

    #[test]
    fn test_sample_transcript_small() {
        let messages: Vec<serde_json::Value> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": format!("message {}", i)}
                })
            })
            .collect();
        let result = sample_transcript(&messages, 20, 50_000);
        // All 5 messages should be included (< 2*20)
        assert!(result.contains("message 0"));
        assert!(result.contains("message 4"));
    }

    #[test]
    fn test_sample_transcript_large_samples_ends() {
        let messages: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": format!("msg-{}", i)}
                })
            })
            .collect();
        let result = sample_transcript(&messages, 5, 50_000);
        // First 5 and last 5 should be present
        assert!(result.contains("msg-0"));
        assert!(result.contains("msg-4"));
        assert!(result.contains("msg-95"));
        assert!(result.contains("msg-99"));
        // Middle should be omitted — gap marker appears as [system] since it lacks role/content
        assert!(!result.contains("msg-50"));
        // 11 entries: 5 first + 1 gap + 5 last
        let line_count = result.lines().count();
        assert_eq!(
            line_count, 11,
            "expected 11 lines (5+1+5), got {}",
            line_count
        );
    }

    #[test]
    fn test_sample_transcript_respects_char_cap() {
        let messages: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": format!("message-{}-{}", i, "x".repeat(200))}
                })
            })
            .collect();
        let result = sample_transcript(&messages, 20, 500);
        assert!(result.len() <= 520); // cap + "... (truncated)"
    }

    #[test]
    fn prompts_ask_for_the_shifts_block_by_default() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_STORY_MAP");

        let ctx = StoryContext {
            current_session_excerpt: "user: hello".to_string(),
            current_v3_enrichment: None,
            related_sessions: vec![],
            project_name: "test".to_string(),
        };
        let first_prompt = build_prompt(&ctx);
        assert!(first_prompt.contains("2-3 sentences"));
        assert!(first_prompt.contains("@shifts"));

        let sum_prompt = summary_story_prompt("summary text", "tail text");
        assert!(sum_prompt.contains("under 800 characters"));
        assert!(sum_prompt.contains("@shifts"));
    }

    #[test]
    fn kill_switch_leaves_both_prompts_byte_identical() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CSR_NO_STORY_MAP", "1");

        let ctx = StoryContext {
            current_session_excerpt: "user: hello".to_string(),
            current_v3_enrichment: None,
            related_sessions: vec![],
            project_name: "test".to_string(),
        };
        let first_prompt = build_prompt(&ctx);
        let sum_prompt = summary_story_prompt("summary text", "tail text");

        std::env::remove_var("CSR_NO_STORY_MAP");

        assert!(!first_prompt.contains("@shifts"));
        assert!(first_prompt.ends_with("No preamble, no markdown headers."));
        assert!(!sum_prompt.contains("@shifts"));
        assert!(sum_prompt.ends_with("no markdown headers."));
        // the whole prompt is today's, nothing more
        assert_eq!(
            sum_prompt,
            "You are summarizing a coding session for future context restoration after memory compaction.\n\n\
             CLAUDE CODE'S SUMMARY OF THE SESSION (written at its latest compaction, from the whole \
             conversation except its last messages):\nsummary text\n\n\
             THE LAST MESSAGES (the summary did not see these; they show where the work actually \
             stood):\ntail text\n\n\
             Write 3-4 sentences: what the session was about across its whole life and its main \
             threads, the key files/tools used, and where the work stood at the end \
             (done/in-progress/blocked). Where the last messages and the summary differ, trust the last \
             messages. Be specific about technical details. Keep it under 800 characters. No preamble, \
             no markdown headers."
        );
    }
}
