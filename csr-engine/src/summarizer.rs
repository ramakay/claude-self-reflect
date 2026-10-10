//! Haiku-curated session stories via Claude CLI headless mode.
//!
//! Flow: SQLite retrieves context → Haiku formats it into a 2-3 sentence story → stored as reflection.
//! Graceful degradation: returns None if claude CLI unavailable or times out.

use std::path::Path;
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
    let parsed = call_claude_headless(&prompt).await?;
    Some((parsed.text.clone(), parsed))
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

    prompt
}

/// The story Haiku writes from Claude Code's compaction summary, which carries the session's
/// whole arc, and its last messages, which the summary never saw (Claude Code keeps them verbatim
/// after it). Of five ways to tell a long session, blind judges ranked this first on four of five
/// sessions (2026-10-10): the summary alone reported finished work as still open, and a sample of
/// the first and last messages lost the middle.
fn summary_story_prompt(summary: &str, tail: &str) -> String {
    format!(
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
    )
}

/// Invoke `claude -p` (model chain: env override → haiku alias → CLI default)
/// with prompt piped via stdin (avoids OS arg length limits).
async fn call_claude_headless(prompt: &str) -> Option<crate::narrative::ParsedNarrative> {
    call_claude_headless_with("claude", prompt, crate::narrative::minimal_mcp_config()).await
}

/// [`call_claude_headless`] with the program and the empty MCP config
/// supplied. The transcript is embedded in `prompt`, so the analyst needs NO
/// tools. Without an explicit empty MCP config the child inherits the user's
/// full MCP set (blowing the context window on tool schemas alone, and handing
/// a text summariser live tools). So when that config cannot be created the
/// narrator does not run at all: callers fall back to CSR's own extract.
async fn call_claude_headless_with(
    program: &str,
    prompt: &str,
    mcp_config: anyhow::Result<std::path::PathBuf>,
) -> Option<crate::narrative::ParsedNarrative> {
    let mcp_config_path = match mcp_config {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!("empty MCP config unavailable; narrator not run: {e}");
            return None;
        }
    };

    for candidate in crate::narrative::model_candidates() {
        let mut cmd = tokio::process::Command::new(program);
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
        cmd.args(["--strict-mcp-config", "--mcp-config"]);
        cmd.arg(&mcp_config_path);

        // The deadline covers the whole exchange and, when it passes, the
        // child is killed and reaped before this returns: the story lock is
        // never released while a narrator lives.
        let output =
            match crate::narrative::run_headless(cmd, prompt.as_bytes(), HAIKU_TIMEOUT).await {
                crate::narrative::HeadlessRun::Output(output) => output,
                // timeout or spawn failure (claude CLI not found): no point walking the chain
                _ => return None,
            };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        match crate::narrative::classify_attempt(output.status.success(), &stdout, &stderr) {
            crate::narrative::AttemptOutcome::Parsed(mut parsed) => {
                // Cap at 1000 chars — stories should be concise
                parsed.text = parsed.text.chars().take(1000).collect();
                return Some(parsed);
            }
            crate::narrative::AttemptOutcome::ModelNotFound => continue,
            crate::narrative::AttemptOutcome::Failed(_) => return None,
        }
    }
    None
}

/// One transcript line: `[role]: first 500 characters`.
fn render_message(msg: &serde_json::Value) -> String {
    let data = crate::extraction::get_message_data(msg);
    let role = data
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("system");
    let content = crate::extraction::content_to_lower(&data);
    // Take first 500 chars per message to keep prompt manageable
    let preview: String = content.chars().take(500).collect();
    format!("[{}]: {}\n", role, preview)
}

/// `messages` in order, no gap, as many whole lines as fit in `char_cap`
/// (always at least one), and how many that is. What the narrator was shown is
/// exactly this prefix, so only it may be recorded as covered.
fn tail_transcript(messages: &[serde_json::Value], char_cap: usize) -> (String, usize) {
    let mut result = String::new();
    let mut shown = 0;
    for msg in messages {
        let line = render_message(msg);
        if shown > 0 && result.len() + line.len() > char_cap {
            break;
        }
        result.push_str(&line);
        shown += 1;
    }
    (result, shown)
}

/// What a story written beside Claude Code's summary (the message at `at`)
/// reads, and how many of the session's leading messages that covers.
///
/// The summary was written from everything except the last messages Claude
/// Code kept verbatim: the `TAIL_MSGS` just before it. So the narrator reads
/// those, which can differ from the summary ("blocked" there, "completed"
/// here) - after a compaction, and again at the session's end together with
/// everything typed after the summary, so a later rewrite cannot restore the
/// summary's stale account. Coverage reaches only as far as the narrator
/// read: a request it did not see is still owed a story.
fn story_input(mode: StoryMode, messages: &[serde_json::Value], at: usize) -> (String, usize) {
    let at = at.min(messages.len().saturating_sub(1));
    let from = at.saturating_sub(TAIL_MSGS);
    let preserved = &messages[from..at];
    let mut segment: Vec<serde_json::Value> = preserved.to_vec();
    if mode == StoryMode::Rewrite {
        segment.extend(messages[at + 1..].iter().cloned());
    }
    let (text, shown) = tail_transcript(&segment, TRANSCRIPT_CHAR_CAP);
    let covered = if shown <= preserved.len() {
        // The summary itself is the only thing read beyond the preserved
        // messages; a cut inside them leaves the summary unread too.
        if shown == preserved.len() {
            at + 1
        } else {
            from + shown
        }
    } else {
        at + 1 + (shown - preserved.len())
    };
    (text, covered)
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
        result.push_str(&render_message(msg));

        if result.len() >= char_cap {
            result.truncate(result.floor_char_boundary(char_cap));
            result.push_str("\n... (truncated)");
            break;
        }
    }
    result
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
            let words = crate::extraction::provenance::extractable(&typed)?;
            Some(
                words
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
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
            Some(covered) if covered >= messages || asked_since == 0 => {
                Err(format!("skip:nothing_asked_since({covered}/{messages})"))
            }
            _ => Ok(()),
        },
    }
}

/// What the person typed after the story covering the first `covered` messages.
fn asked_since(messages: &[serde_json::Value], prior: Option<&PriorStory>) -> usize {
    let covered = prior
        .and_then(|p| p.covered)
        .unwrap_or(0)
        .min(messages.len());
    typed_prompts(&messages[covered..], usize::MAX, 1).len()
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
) -> StoryContext {
    let project_name = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());

    // 1. Sample transcript
    let excerpt = sample_transcript(messages, TRANSCRIPT_SAMPLE_MSGS, TRANSCRIPT_CHAR_CAP);

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

/// Get the latest V3 enrichment reflection for this project.
fn get_latest_v3_enrichment(storage: &Arc<Storage>, project: &str) -> Option<String> {
    let tag = format!("project_{}", project);
    let reflections = storage.get_reflections_by_tag(&tag, 5).ok()?;
    reflections
        .into_iter()
        .find(|(_, _, tags, _)| tags.iter().any(|t| t.starts_with("narrative_")))
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
        if let Ok(Some((content, _tags, timestamp))) = storage.get_reflection_by_id(&r.id) {
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

    let mut messages = crate::import::parse_jsonl_messages_for_search(transcript)?;
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
        asked_since(&messages, prior.as_ref()),
    );
    if mode == StoryMode::FromCompaction && due.is_err() {
        // SessionStart can run a moment before the summary reaches the transcript: read it again once
        tokio::time::sleep(Duration::from_secs(5)).await;
        messages = crate::import::parse_jsonl_messages_for_search(transcript)?;
        prior = prior_story(engine.storage(), &conv_id);
        due = story_due(
            mode,
            prior.as_ref(),
            messages.len(),
            newest_compaction_summary(&messages).map(|(at, _)| at),
            asked_since(&messages, prior.as_ref()),
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
        asked_since(&messages, prior.as_ref()),
    ) {
        log_story_event(&project, &conv_id, &skip);
        return Ok(());
    }

    let result = match (mode, summary) {
        (StoryMode::First, _) | (StoryMode::Rewrite, None) => {
            // Lock held — generate story (Haiku call happens here)
            generate_and_store(engine, cwd, &conv_id, &project, &messages, prior.as_ref()).await
        }
        (StoryMode::FromCompaction, None) => Ok(()), // story_due refused it already
        (_, Some((at, text))) => {
            // Beside the summary, Haiku reads what it never saw: the messages just before it after
            // a compaction, everything after it at the session's end
            let by_haiku = mode == StoryMode::Rewrite || story_rewrites_enabled();
            let (tail, covered) = story_input(mode, &messages, at);
            let mut told = None;
            if by_haiku {
                told = haiku_story(
                    engine,
                    &summary_story_prompt(&text, &tail),
                    &project,
                    &conv_id,
                )
                .await
                .map(|story| (story, "story_by_haiku"));
            }
            if told.is_none() && mode == StoryMode::FromCompaction {
                // Not opted in, or Haiku failed: CSR's own extract, no model call
                told = story_from_compaction_summary(&text).map(|(story, templated)| {
                    if !templated {
                        log_story_event(&project, &conv_id, "aux_schema_miss:compaction_summary");
                    }
                    (story, "story_extracted")
                });
            }
            match told {
                Some((story, how)) => {
                    // CSR's own extract read only the summary; Haiku read the
                    // messages beside it too.
                    let covered = if how == "story_by_haiku" {
                        covered
                    } else {
                        at + 1
                    };
                    let mut tags = vec![how.to_string()];
                    if mode == StoryMode::FromCompaction {
                        tags.push("story_from_compaction".to_string());
                    }
                    store_story(
                        engine,
                        cwd,
                        &conv_id,
                        &project,
                        &story,
                        prior.as_ref(),
                        covered,
                        tags,
                    )
                    .await
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

/// One metered Haiku call for a story: its text, or None when narratives are off or the call
/// failed. A failed call is metered too.
async fn haiku_story(
    engine: &Engine,
    prompt: &str,
    project: &str,
    conv_id: &str,
) -> Option<String> {
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
    parsed.map(|parsed| parsed.text)
}

/// Generate story and store it. Separated so the lock scope is clear.
async fn generate_and_store(
    engine: &Engine,
    cwd: &str,
    conv_id: &str,
    project: &str,
    messages: &[serde_json::Value],
    prior: Option<&PriorStory>,
) -> anyhow::Result<()> {
    let ctx = build_story_context(engine, cwd, messages).await;
    let Some(story) = haiku_story(engine, &build_prompt(&ctx), project, conv_id).await else {
        return Ok(());
    };
    store_story(
        engine,
        cwd,
        conv_id,
        project,
        &story,
        prior,
        messages.len(),
        Vec::new(),
    )
    .await
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
    extra_tags: Vec<String>,
) -> anyhow::Result<()> {
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
        let mut idx = engine.search().write().await;
        for old in &prior.ids {
            let _ = engine.storage().delete_reflection(old);
            idx.remove_reflection(old);
        }
    }

    // Record how much of the session it covers: what the next story is measured against
    let _ = engine
        .storage()
        .mark_story_written(conv_id, &reflection_id, covered);

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

    let exe = match std::env::current_exe() {
        Ok(e) => e,
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
        assert_eq!(
            story_due(mode, Some(&prior(3, Some(4800))), 4800, None, 2),
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
        assert_eq!(asked_since(&messages, None), 3);
        assert_eq!(asked_since(&messages, Some(&prior(1, Some(2)))), 1);
        assert_eq!(asked_since(&messages, Some(&prior(1, Some(9)))), 0);
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
            .mark_story_written("conv-a", "story_conv-a", 640)
            .unwrap();
        let found = prior_story(&storage, "conv-a").unwrap();
        assert_eq!((found.rev, found.covered), (2, Some(640)));
        let mut ids = found.ids.clone();
        ids.sort();
        assert_eq!(ids, vec!["new", "old"]);
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

    fn row(role: &str, text: &str) -> serde_json::Value {
        serde_json::json!({"type": role, "message": {"role": role, "content": text}})
    }

    /// `n` ordinary messages, then Claude Code's summary (index `n`), then the
    /// `after` messages.
    fn session(n: usize, summary: &str, after: &[(&str, String)]) -> Vec<serde_json::Value> {
        let mut messages: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                row(
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("message {i}"),
                )
            })
            .collect();
        messages[n - 2] = row(
            "assistant",
            "COMPLETED: the migration landed and the tests pass",
        );
        let mut summary = row("user", summary);
        summary["isCompactSummary"] = true.into();
        messages.push(summary);
        for (role, text) in after {
            messages.push(row(role, text));
        }
        messages
    }

    #[test]
    fn a_compaction_story_covers_only_what_its_narrator_read() {
        let after: Vec<(&str, String)> = vec![
            ("user", "please rename the flag".into()),
            ("assistant", "renamed it".into()),
            ("user", "now add a changelog line".into()),
            ("assistant", "added".into()),
        ];
        let messages = session(30, "summary: the migration is blocked", &after);
        let at = 30;
        let (tail, covered) = story_input(StoryMode::FromCompaction, &messages, at);
        assert_eq!(
            covered,
            at + 1,
            "through the summary, not the {} messages",
            messages.len()
        );
        assert!(tail.contains("the migration landed"), "{tail}");
        assert!(!tail.contains("rename the flag"), "{tail}");

        // The requests typed after the summary are still owed a story at the end.
        let prior = PriorStory {
            covered: Some(covered),
            ..PriorStory::default()
        };
        assert_eq!(asked_since(&messages, Some(&prior)), 2);
        assert_eq!(
            story_due(
                StoryMode::Rewrite,
                Some(&prior),
                messages.len(),
                Some(at),
                2
            ),
            Ok(())
        );
        // The old accounting (the whole transcript) would have skipped them.
        let wrong = PriorStory {
            covered: Some(messages.len()),
            ..PriorStory::default()
        };
        assert!(story_due(
            StoryMode::Rewrite,
            Some(&wrong),
            messages.len(),
            Some(at),
            0
        )
        .is_err());
    }

    #[test]
    fn an_ending_rewrite_reads_the_preserved_segment_beside_the_new_requests() {
        let after: Vec<(&str, String)> = vec![("user", "unrelated: rename the flag".into())];
        let messages = session(30, "summary: the migration is blocked", &after);
        let (tail, covered) = story_input(StoryMode::Rewrite, &messages, 30);
        // The preserved pre-summary message that says "completed" is read, so
        // the rewrite cannot restore the summary's stale "blocked".
        assert!(tail.contains("completed: the migration landed"), "{tail}");
        assert!(tail.contains("rename the flag"), "{tail}");
        assert_eq!(covered, messages.len());
    }

    #[test]
    fn a_rewrite_that_cannot_read_everything_records_only_what_it_read() {
        let after: Vec<(&str, String)> = (0..200)
            .map(|i| ("user", format!("request {i}: {}", "x".repeat(480))))
            .collect();
        let messages = session(30, "summary: s", &after);
        let (tail, covered) = story_input(StoryMode::Rewrite, &messages, 30);
        assert!(tail.len() <= TRANSCRIPT_CHAR_CAP + 600, "{}", tail.len());
        assert!(
            covered > 31,
            "some post-summary requests were read: {covered}"
        );
        assert!(covered < messages.len(), "the rest were not: {covered}");
        // exactly the post-summary messages shown, no more
        let shown_requests = tail.matches("request ").count();
        assert_eq!(covered, 31 + shown_requests);
    }

    #[tokio::test]
    async fn the_narrator_is_not_run_when_its_empty_mcp_config_cannot_be_made() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let program = dir.path().join("fake-claude");
        std::fs::write(
            &program,
            format!("#!/bin/sh\ntouch {}\ncat >/dev/null\n", marker.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let none = call_claude_headless_with(
            program.to_str().unwrap(),
            "prompt",
            Err(anyhow::anyhow!("disk full")),
        )
        .await;
        assert!(none.is_none());
        assert!(!marker.exists(), "the narrator ran without MCP isolation");
    }
}
