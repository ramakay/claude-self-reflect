//! SessionStart hook — searches CSR for relevant past sessions and injects context.
//!
//! When a Ralph session is active, performs 4 searches:
//! 1. Similar tasks (min_score=0.5, limit=2)
//! 2. Similar errors (min_score=0.6, limit=1 each)
//! 3. Anti-patterns from incomplete/abandoned sessions (min_score=0.5, limit=2)
//! 4. Winning strategies from completed sessions (min_score=0.6, limit=1)
//!
//! Anti-patterns are placed FIRST in the output (critical for fast loops).

use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;
use chrono::{DateTime, Utc};
use regex::Regex;

use super::ralph_state::RalphState;
use super::HookInput;
use crate::engine::Engine;
use crate::injection::anti_pattern;
use crate::search::cross_project::resolve_project_from_cwd;
use crate::storage::queries::SessionInfo;
use crate::temporal;

/// Regex for stripping XML-like tags from preview text (e.g. <local-command-caveat>).
/// Capped at 50 chars to prevent ReDoS on malformed input (codex R-10).
static XML_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]{1,50}>").unwrap());

/// Handle the session-start hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block Claude Code (C-1 fix).
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    if let Err(e) = handle_inner(input, ralph, engine, cwd).await {
        eprintln!("CSR: session-start hook error (non-fatal): {}", e);
        // Output minimal context so session gets SOMETHING rather than silent "Success"
        // Keep error details on stderr only — don't leak internal paths to Claude's context
        println!("CSR engine ready (degraded mode).");
    }
    Ok(()) // Always succeed
}

async fn handle_inner(
    _input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    // If no Ralph session, output curated session stories (or fallback to V3 enrichment)
    let ralph = match ralph {
        Some(r) => r,
        None => {
            let project = resolve_project_from_cwd(&cwd.to_string_lossy());
            let project_name = project.as_deref().unwrap_or("unknown");
            let now = Utc::now();

            // Try curated session stories first (Haiku-generated, project-scoped)
            let story_tag = format!("project_{}", project_name);
            let stories = engine
                .storage()
                .get_reflections_by_tag("session_story", 10)
                .unwrap_or_default();
            let project_stories: Vec<_> = stories
                .iter()
                .filter(|(_, _, tags, _)| tags.iter().any(|t| t == &story_tag))
                .take(3)
                .collect();

            let mut output = String::new();
            let story_count = project_stories.len();
            let mut session_count = 0usize;

            if !project_stories.is_empty() {
                // Curated story path — Haiku-generated summaries
                for (i, (_id, content, _tags, timestamp)) in project_stories.iter().enumerate() {
                    let age = relative_time_label(timestamp, &now);
                    if i == 0 {
                        output.push_str(&format!("[{}] {}", age, content));
                    } else {
                        output.push_str(&format!("\n\n[{}] {}", age, content));
                    }
                }
                output.push_str("\n\nFor deeper context, use csr_reflect_on_past(\"topic\").");
            } else {
                // Fallback: V3 enrichment + lookup instructions (no stories yet)
                let sessions = engine
                    .storage()
                    .get_recent_sessions(10, Some(project_name))
                    .unwrap_or_default();
                let displayable: Vec<&SessionInfo> = sessions
                    .iter()
                    .filter(|s| is_displayable(s))
                    .take(4)
                    .collect();
                session_count = displayable.len();

                if !displayable.is_empty() {
                    for session in &displayable {
                        let age = relative_time_label(&session.timestamp, &now);
                        let title = session_title(session);
                        let enrichment_line = session
                            .enrichment
                            .as_deref()
                            .and_then(enrichment_display)
                            .unwrap_or_default();

                        if enrichment_line.is_empty() || enrichment_line == title {
                            output.push_str(&format!("[{}] {} ({} msgs)\n", age, title, session.total_messages));
                        } else {
                            output.push_str(&format!(
                                "[{}] {} ({} msgs)\n  {}\n",
                                age, title, session.total_messages, enrichment_line
                            ));
                        }
                    }
                    output.push_str("\nFor deeper context, use csr_reflect_on_past(\"topic\").");
                } else {
                    let (chunk_count, reflection_count) = {
                        let search = engine.search().read().await;
                        (search.chunk_count(), search.reflection_count())
                    };
                    output.push_str(&format!(
                        "CSR: {} chunks, {} reflections indexed. No recent sessions for this project.",
                        chunk_count, reflection_count
                    ));
                }
            }

            // Log what was injected for diagnostics
            log_session_start_injection(project_name, &output, story_count, session_count);

            println!("{output}");
            return Ok(());
        }
    };

    let storage = engine.storage();
    let embeddings = engine.embeddings();
    let search = engine.search();

    let mut context_parts: Vec<String> = Vec::new();
    let mut anti_pattern_count = 0usize;
    let mut winning_count = 0usize;
    let mut error_count = 0usize;
    let mut similar_count = 0usize;

    // 1. Search for anti-patterns (incomplete/abandoned sessions) — output FIRST
    // Uses shared anti_pattern module (also used by prompt_submit hook)
    let anti_items = anti_pattern::find_anti_patterns(
        storage, embeddings, search, &ralph.task, 0.5, 2,
    )
    .await;

    if !anti_items.is_empty() {
        anti_pattern_count = anti_items.len();
        let mut section = String::from("## DON'T RETRY THESE (Anti-Patterns from Past Sessions)\n\n");
        for item in &anti_items {
            section.push_str(&format!("**[Score: {:.2}]**\n{}\n\n---\n\n", item.score, item.content));
        }
        context_parts.push(section);
    }

    // 2. Search for similar errors
    for (sig, _count) in &ralph.error_signatures {
        let error_query = format!("error blocked solved: {}", sig);
        let results = search_reflections_unfiltered(
            storage, embeddings, search, &error_query, 0.6, 1,
        )
        .await;

        if !results.is_empty() {
            error_count += results.len();
            let mut section = String::from("## Past Error Solutions\n\n");
            for (content, score) in &results {
                section.push_str(&format!(
                    "**Error pattern:** `{}`\n**[Score: {:.2}]**\n{}\n\n---\n\n",
                    sig, score, content
                ));
            }
            context_parts.push(section);
        }
    }

    // 3. Search for winning strategies (completed sessions)
    let win_query = format!("successful solution: {}", ralph.task);
    let win_results = anti_pattern::search_reflections_by_tag(
        storage,
        embeddings,
        search,
        &win_query,
        0.6,
        1,
        &["outcome_completed"],
    )
    .await;

    if !win_results.is_empty() {
        winning_count = win_results.len();
        let mut section = String::from("## Winning Strategies from Past Sessions\n\n");
        for (content, score) in &win_results {
            section.push_str(&format!("**[Score: {:.2}]**\n{}\n\n---\n\n", score, content));
        }
        context_parts.push(section);
    }

    // 4. Search for similar tasks
    let task_query = format!("ralph session: {}", ralph.task);
    let task_results = search_reflections_unfiltered(
        storage, embeddings, search, &task_query, 0.5, 2,
    )
    .await;

    if !task_results.is_empty() {
        similar_count = task_results.len();
        let mut section = String::from("## Similar Past Sessions\n\n");
        for (content, score) in &task_results {
            section.push_str(&format!("**[Score: {:.2}]**\n{}\n\n---\n\n", score, content));
        }
        context_parts.push(section);
    }

    // Write context file if there are any results
    let total_results = anti_pattern_count + winning_count + error_count + similar_count;
    if !context_parts.is_empty() {
        let mut file_content = String::from("# CSR Past Session Context\n\n");
        file_content.push_str(&format!(
            "> Auto-generated by CSR engine for Ralph session `{}`\n\n",
            ralph.session_id,
        ));
        for part in &context_parts {
            file_content.push_str(part);
        }

        let context_path = cwd.join(".ralph_past_sessions.md");
        std::fs::write(&context_path, &file_content)?;
    }

    // Output summary to stdout
    println!(
        "CSR: Found {} relevant results for Ralph session '{}':",
        total_results, ralph.session_id,
    );
    println!("  - Anti-patterns: {}", anti_pattern_count);
    println!("  - Winning strategies: {}", winning_count);
    println!("  - Error matches: {}", error_count);
    println!("  - Similar tasks: {}", similar_count);

    if total_results > 0 {
        println!(
            "  Context written to: {}",
            cwd.join(".ralph_past_sessions.md").display()
        );
    }

    // Log Ralph session-start injection details
    let project = resolve_project_from_cwd(&cwd.to_string_lossy()).unwrap_or_else(|| "unknown".to_string());
    let ralph_summary = format!(
        "ralph=\"{}\" anti={} win={} err={} sim={}",
        ralph.task.chars().take(60).collect::<String>(),
        anti_pattern_count, winning_count, error_count, similar_count,
    );
    log_session_start_injection(&project, &ralph_summary, 0, 0);

    Ok(())
}

/// Search reflections without tag filtering.
async fn search_reflections_unfiltered(
    storage: &std::sync::Arc<crate::storage::Storage>,
    embeddings: &std::sync::Arc<crate::embeddings::EmbeddingEngine>,
    search: &std::sync::Arc<tokio::sync::RwLock<crate::search::SearchEngine>>,
    query: &str,
    min_score: f32,
    limit: usize,
) -> Vec<(String, f32)> {
    let query_vec = match embed_query(embeddings, query).await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let results = {
        let idx = search.read().await;
        idx.search_reflections(&query_vec, limit, min_score)
    };

    let mut enriched = Vec::new();
    for result in &results {
        if let Ok(Some((content, _tags, _ts))) = storage.get_reflection_by_id(&result.id) {
            enriched.push((content, result.score));
        }
    }

    enriched
}

/// Embed a query string via spawn_blocking.
async fn embed_query(
    embeddings: &std::sync::Arc<crate::embeddings::EmbeddingEngine>,
    query: &str,
) -> Result<Vec<f32>> {
    let q = query.to_string();
    let emb = embeddings.clone();
    Ok(tokio::task::spawn_blocking(move || emb.embed_single(&q)).await??)
}

/// Search for cross-project concept matches from the most recent session's enrichment.
/// Returns a one-line note if a match is found in another project.
#[allow(dead_code)]
async fn cross_project_pulse(
    engine: &Engine,
    sessions: &[&SessionInfo],
    current_project: &str,
) -> Option<String> {
    // Use the most recent session's enrichment or summary as the query
    let query_text = sessions.first().and_then(|s| {
        s.enrichment
            .as_deref()
            .or(s.summary.as_deref())
    })?;

    if query_text.trim().is_empty() {
        return None;
    }

    let query_vec = embed_query(engine.embeddings(), query_text).await.ok()?;

    // Search all reflections (unfiltered by project)
    let results = {
        let idx = engine.search().read().await;
        idx.search_reflections(&query_vec, 5, 0.5)
    };

    // Find first result from a DIFFERENT project
    for result in &results {
        if let Ok(Some((content, tags, _ts))) = engine.storage().get_reflection_by_id(&result.id) {
            // Check if this reflection belongs to a different project
            // Heuristic reflections contain "Project: <name>" in first line
            let other_project = content
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("[Heuristic] Project: "))
                .unwrap_or("");

            // Also check tags for project info
            let tag_project = tags.iter().find(|t| t.starts_with("project:")).map(|t| &t[8..]);
            let proj = if !other_project.is_empty() {
                other_project
            } else if let Some(tp) = tag_project {
                tp
            } else {
                continue;
            };

            if !proj.is_empty() && proj != current_project {
                return Some(format!(
                    "Cross-project: similar concepts found in {} ({:.2}) — use reflect_on_past(project:\"all\") to explore",
                    proj, result.score
                ));
            }
        }
    }

    None
}

/// Log session-start injection details to hook-timing.log for diagnostics.
/// Captures: project, stdout size, story count, session count, and content preview.
fn log_session_start_injection(
    project: &str,
    output: &str,
    story_count: usize,
    session_count: usize,
) {
    if let Some(home) = dirs::home_dir() {
        let log_path = home.join(".claude-self-reflect").join("hook-timing.log");
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let preview: String = output
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ")
            .chars()
            .take(200)
            .collect();
        let line = format!(
            "{} CSR session-start inject [{}]: stories={} sessions={} stdout={}B preview=\"{}\"\n",
            ts, project, story_count, session_count, output.len(), preview,
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    }
}

/// Format a relative time label with hour-level granularity for same-day sessions.
/// Uses `temporal::parse_timestamp` for parsing, pure fn with injected `now`.
fn relative_time_label(timestamp: &str, now: &DateTime<Utc>) -> String {
    let ts = match temporal::parse_timestamp(timestamp) {
        Some(t) => t,
        None => return "???".to_string(),
    };
    let diff = *now - ts;
    let total_minutes = diff.num_minutes();
    let days = diff.num_days();

    if days == 0 {
        if total_minutes < 1 {
            "just now".to_string()
        } else if total_minutes < 60 {
            format!("{}m ago", total_minutes)
        } else {
            format!("{}h ago", diff.num_hours())
        }
    } else if days == 1 {
        "yesterday".to_string()
    } else if days < 7 {
        format!("{}d ago", days)
    } else if days < 30 {
        format!("{}w ago", days / 7)
    } else {
        format!("{}mo ago", days / 30)
    }
}

/// Truncate content to max_chars and sanitize to a single line for timeline display.
fn compact_preview(content: &str, max_chars: usize) -> String {
    // Sanitize first: collapse newlines to spaces, strip control chars
    let clean = sanitize_preview(content);
    if clean.len() <= max_chars {
        return clean;
    }
    let boundary = clean.floor_char_boundary(max_chars);
    format!("{}...", &clean[..boundary])
}

/// Regex for stripping inline markdown heading markers (e.g. " ## Context " → " Context ").
static MD_HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#{1,4}\s+").unwrap());

/// Sanitize a preview string for safe stdout injection.
/// Strips XML-like tags, markdown headers, literal \n, collapses newlines, removes control chars.
fn sanitize_preview(s: &str) -> String {
    // Replace literal \n (backslash-n) with space — common in JSONL-sourced content
    let no_literal_nl = s.replace("\\n", " ");
    // Strip XML-like tags (e.g. <local-command-caveat>, <system-reminder>)
    let no_xml = XML_TAG_RE.replace_all(&no_literal_nl, "");
    // Strip all markdown heading markers (both start-of-line and inline after \n collapse)
    let no_md = MD_HEADING_RE.replace_all(&no_xml, "");
    // Collapse to single line, strip whitespace
    let mut result = String::with_capacity(no_md.len());
    for line in no_md.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(trimmed);
        }
    }
    // Collapse multiple spaces into one
    while result.contains("  ") {
        result = result.replace("  ", " ");
    }
    // Strip remaining control characters
    result.retain(|c| !c.is_control() || c == ' ');
    result
}

/// Strip common preamble prefixes that add noise to timeline summaries.
fn strip_preamble(s: &str) -> &str {
    const PREFIXES: &[&str] = &[
        "Implement the following plan:",
        "Execute the following plan:",
        "Follow the following plan:",
        "Here is the plan:",
        "Please implement:",
        "Please execute:",
    ];

    let trimmed = s.trim();
    for prefix in PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let rest = rest.trim();
            // Also strip leading markdown headers
            return rest.trim_start_matches('#').trim();
        }
    }
    // Strip leading markdown headers even without a prefix
    if trimmed.starts_with('#') {
        return trimmed.trim_start_matches('#').trim();
    }
    trimmed
}

/// Patterns in session summaries that indicate the first message was noise, not a real title.
/// After XML tag stripping, these remain as leftover content from system wrappers.
const NOISE_SUMMARIES: &[&str] = &[
    "caveat: the messages below",
    "note: this session",
    "system-reminder",
    "the following context",
    "this is a continuation",
];

/// Extract a clean session title from summary, stripping preamble and sanitizing.
/// Falls back to enrichment-based title if the summary is noise (e.g. caveat text from /clear).
fn session_title(session: &SessionInfo) -> String {
    let raw = session
        .summary
        .as_deref()
        .unwrap_or("(no summary)");
    let stripped = strip_preamble(raw);
    let sanitized = sanitize_preview(stripped);

    // Detect noise: if the sanitized summary matches known noise patterns,
    // try to extract a title from enrichment data instead
    let lower = sanitized.to_lowercase();
    let is_noise = NOISE_SUMMARIES.iter().any(|p| lower.starts_with(p));

    if is_noise {
        // Try enrichment-based title from any enrichment format
        if let Some(enrichment) = session.enrichment.as_deref() {
            // Try heuristic format: structured tools/files
            let fields = parse_enrichment(enrichment);
            if fields.has_edit_tool && !fields.files.is_empty() {
                let file_list: Vec<&str> = fields.files.iter().take(3).map(|s| s.as_str()).collect();
                return format!("Edited {}", file_list.join(", "));
            }
            if !fields.tools.is_empty() {
                let tool_summary: Vec<&str> = fields.tools.iter().take(3).map(|s| s.as_str()).collect();
                return format!("Session using {}", tool_summary.join(", "));
            }
            // Try v3/ai_narrative format: extract search summary
            if let Some(v3_summary) = extract_v3_summary(enrichment) {
                return v3_summary;
            }
        }
        return "(session)".to_string();
    }

    compact_preview(&sanitized, 70)
}

/// Look up the enrichment reflection for a session and return a ~200 char preview.
/// Tries enrichment types in priority order: ai_narrative > v3_extraction > heuristic.
#[allow(dead_code)]
fn get_session_reflection_preview(engine: &Engine, session: &SessionInfo) -> Option<String> {
    let cid = &session.conversation_id;
    let storage = engine.storage();

    for enrichment_type in &["ai_narrative", "v3_extraction", "heuristic"] {
        if let Ok(Some(ref_id)) = storage.get_enrichment_reflection_id(cid, enrichment_type) {
            if let Ok(Some((content, _tags, _ts))) = storage.get_reflection_by_id(&ref_id) {
                let preview: String = content.chars().take(200).collect();
                let sanitized = sanitize_preview(&preview);
                if sanitized.len() > 20 {
                    let truncated = compact_preview(&sanitized, 200);
                    return Some(truncated);
                }
            }
        }
    }
    None
}

/// Check if a session has enough content to be worth displaying.
/// Threshold: >= 6 messages (codex R-1) OR has enrichment data.
fn is_displayable(session: &SessionInfo) -> bool {
    let has_enrichment = session.enrichment.is_some();
    if has_enrichment {
        return true;
    }
    if session.total_messages < 6 {
        return false;
    }
    // Must have a non-empty summary
    session
        .summary
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// Parse heuristic enrichment text to extract structured fields.
/// Format: "[Heuristic] Project: X\nMessages: N (M user)\nTools: ...\nFiles: ...\nHad errors: yes"
struct EnrichmentFields {
    tools: Vec<String>,
    files: Vec<String>,
    has_errors: bool,
    has_edit_tool: bool,
}

fn parse_enrichment(enrichment: &str) -> EnrichmentFields {
    let mut tools = Vec::new();
    let mut files = Vec::new();
    let mut has_errors = false;

    for line in enrichment.lines() {
        if let Some(rest) = line.strip_prefix("Tools: ") {
            tools = rest.split(", ").map(|s| s.trim().to_string()).collect();
        } else if let Some(rest) = line.strip_prefix("Files: ") {
            files = rest.split(", ").map(|s| s.trim().to_string()).collect();
        } else if line.contains("Had errors: yes") {
            has_errors = true;
        }
    }

    let has_edit_tool = tools.iter().any(|t| t == "Edit" || t == "MultiEdit");
    EnrichmentFields {
        tools,
        files,
        has_errors,
        has_edit_tool,
    }
}

/// Build a compact display line from enrichment data (tools + files).
/// Handles both heuristic format (`[Heuristic] Project: X\nTools: ...`)
/// and v3/ai_narrative format (`## Search Summary\nText...`).
/// Returns None if no structured data found.
fn enrichment_display(enrichment: &str) -> Option<String> {
    // Try heuristic format first (structured Tools/Files)
    let fields = parse_enrichment(enrichment);
    let mut parts = Vec::new();

    if !fields.tools.is_empty() {
        let tool_list: Vec<&str> = fields.tools.iter().take(6).map(|s| s.as_str()).collect();
        parts.push(format!("Tools: {}", tool_list.join(", ")));
    }
    if !fields.files.is_empty() {
        let file_list: Vec<&str> = fields.files.iter().take(4).map(|s| s.as_str()).collect();
        parts.push(format!("Files: {}", file_list.join(", ")));
    }
    if fields.has_errors {
        parts.push("Had errors".to_string());
    }

    if !parts.is_empty() {
        return Some(parts.join(" | "));
    }

    // Try v3/ai_narrative format: extract ## Search Summary paragraph
    extract_v3_summary(enrichment)
}

/// Headers to look for in v3/ai_narrative enrichment, in priority order.
const V3_SUMMARY_HEADERS: &[&str] = &[
    "## Search Summary",
    "## User Request",
    "## Problem-Solution Mapping",
    "## Implementation Context",
    "## Context",
];

/// Extract the first meaningful paragraph from v3/ai_narrative enrichment.
/// Tries multiple section headers in priority order.
fn extract_v3_summary(enrichment: &str) -> Option<String> {
    for header in V3_SUMMARY_HEADERS {
        if let Some(text) = extract_section_paragraph(enrichment, header) {
            return Some(text);
        }
    }
    None
}

/// Extract the first non-empty paragraph after a markdown ## header.
/// Applies preamble stripping and sanitization for clean display.
fn extract_section_paragraph(content: &str, header: &str) -> Option<String> {
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == header {
            in_section = true;
            continue;
        }
        // Stop at next section
        if in_section && trimmed.starts_with("## ") {
            break;
        }
        if in_section {
            // Skip empty lines, code fence markers, and short noise
            if trimmed.is_empty() || trimmed.starts_with("```") {
                continue;
            }
            // Strip leading quotes from User Request content
            let unquoted = trimmed.trim_start_matches('"').trim_end_matches('"');
            // Apply preamble stripping ("Implement the following plan:" etc.)
            let stripped = strip_preamble(unquoted);
            // Trim leading literal \n then split on \n\n to extract just the title
            let trimmed_nl = stripped.trim_start_matches("\\n");
            let title_only = trimmed_nl
                .split("\\n\\n")
                .find(|s| !s.is_empty())
                .unwrap_or(trimmed_nl);
            let sanitized = sanitize_preview(title_only);
            if sanitized.len() > 10 {
                return Some(compact_preview(&sanitized, 80));
            }
        }
    }
    None
}

/// Format a session timeline line using enrichment data when available.
#[allow(dead_code)]
fn format_session_line(session: &SessionInfo, now: &DateTime<Utc>) -> String {
    let label = relative_time_label(&session.timestamp, now);

    // Prefer enrichment display for rich context, fall back to session_title
    // (session_title handles noise detection and enrichment-based fallback)
    let display = session
        .enrichment
        .as_deref()
        .and_then(enrichment_display)
        .unwrap_or_else(|| session_title(session));

    format!(
        "{:<9} | {:>3} msgs | {}",
        label, session.total_messages, display
    )
}

/// Infer a suggested next action from enrichment data.
/// Uses structured enrichment fields for better accuracy than keyword matching.
#[allow(dead_code)]
fn infer_next_action_from_session(session: &SessionInfo) -> String {
    // Try enrichment-based inference first
    if let Some(enrichment) = session.enrichment.as_deref() {
        let fields = parse_enrichment(enrichment);

        if fields.has_errors && fields.has_edit_tool {
            return "Fix errors from last session — edits were in progress".to_string();
        }
        if fields.has_errors {
            return "Investigate and fix errors from last session".to_string();
        }
        if fields.has_edit_tool && !fields.files.is_empty() {
            let file_list: Vec<&str> = fields.files.iter().take(3).map(|s| s.as_str()).collect();
            return format!("Continue work on {}", file_list.join(", "));
        }
        if session.total_messages > 200 {
            return "Large session — review progress and continue".to_string();
        }
    }

    // Fall back to keyword-based inference on summary
    let text = session.summary.as_deref().unwrap_or("");
    infer_next_action(text)
}

/// Infer a suggested next action from the last session's content.
/// Uses simple keyword heuristics — no embedding needed, ~0ms.
#[allow(dead_code)]
fn infer_next_action(content: &str) -> String {
    let lower = content.to_lowercase();

    // Check for explicit phase references (generic, not hardcoded — L-1 fix)
    if let Some(pos) = lower.find("phase ") {
        let after = &lower[pos + 6..];
        if let Some(num_end) = after.find(|c: char| !c.is_ascii_digit()) {
            let phase_num = &after[..num_end];
            if !phase_num.is_empty() {
                return format!("Continue with Phase {phase_num} implementation");
            }
        } else if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
            // "phase N" at end of string
            return format!("Continue with Phase {after} implementation");
        }
    }
    // Also handle "next phase" (use lowered string for extraction — M-1 fix)
    if lower.contains("next phase") {
        return "Pick up the next phase discussed in last session".to_string();
    }

    // Check for incomplete work signals
    if lower.contains("todo") || lower.contains("fixme") || lower.contains("wip") {
        return "Continue incomplete work from last session".to_string();
    }
    if lower.contains("failing") || lower.contains("broken") || lower.contains("error") {
        return "Investigate issues from last session".to_string();
    }

    // Check for planning/review patterns
    if lower.contains("plan") && lower.contains("review") {
        return "Review and execute the plan from last session".to_string();
    }
    if lower.contains("plan") {
        return "Execute the plan discussed in last session".to_string();
    }

    // Check for test patterns
    if lower.contains("test") && (lower.contains("add") || lower.contains("write")) {
        return "Continue adding tests from last session".to_string();
    }

    // Check for refactor/cleanup
    if lower.contains("refactor") || lower.contains("cleanup") {
        return "Continue refactoring from last session".to_string();
    }

    // Default: generic continuation
    "Continue where you left off — ask what's next".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- infer_next_action (keyword fallback) tests ---

    #[test]
    fn test_infer_phase_generic() {
        let result = infer_next_action("Phase 4 implementation is next");
        assert!(result.contains("Phase 4"));
        let result = infer_next_action("Starting Phase 12 soon");
        assert!(result.contains("Phase 12"));
    }

    #[test]
    fn test_infer_error() {
        let result = infer_next_action("The build is failing with a linker error");
        assert!(result.contains("Investigate"));
    }

    #[test]
    fn test_infer_plan() {
        let result = infer_next_action("Here is the plan for the new feature");
        assert!(result.contains("plan"));
    }

    #[test]
    fn test_infer_todo() {
        let result = infer_next_action("TODO: finish the import logic");
        assert!(result.contains("incomplete"));
    }

    #[test]
    fn test_infer_default() {
        let result = infer_next_action("Just a normal conversation about Rust");
        assert!(result.contains("Continue where you left off"));
    }

    #[test]
    fn test_infer_refactor() {
        let result = infer_next_action("We started a refactor of the storage layer");
        assert!(result.contains("refactoring"));
    }

    #[test]
    fn test_infer_empty() {
        let result = infer_next_action("");
        assert!(result.contains("Continue where you left off"));
    }

    #[test]
    fn test_infer_priority_phase_over_todo() {
        let result = infer_next_action("TODO: implement Phase 5 features");
        assert!(result.contains("Phase 5"));
    }

    #[test]
    fn test_infer_unicode_no_panic() {
        let result = infer_next_action("İstanbul projesinde çalışmaya devam");
        assert!(!result.is_empty());
    }

    // --- sanitize_preview tests ---

    #[test]
    fn test_sanitize_preview() {
        assert_eq!(sanitize_preview("hello\nworld"), "hello world");
        assert_eq!(sanitize_preview("clean text"), "clean text");
        assert_eq!(sanitize_preview("a\x00b\x01c"), "abc");
    }

    #[test]
    fn test_sanitize_preview_strips_xml_tags() {
        let input = "<local-command-caveat>Caveat: The messages below</local-command-caveat>";
        let result = sanitize_preview(input);
        assert!(!result.contains('<'));
        assert!(!result.contains('>'));
        assert!(result.contains("Caveat: The messages below"));
    }

    #[test]
    fn test_sanitize_preview_strips_markdown_headers() {
        let input = "## Context\nThe CSR hooks inject context";
        let result = sanitize_preview(input);
        assert!(!result.contains("##"));
        assert!(result.contains("Context"));
        assert!(result.contains("The CSR hooks inject context"));
    }

    #[test]
    fn test_sanitize_preview_combined() {
        let input = "<system-reminder>## Important\nDo the thing</system-reminder>";
        let result = sanitize_preview(input);
        assert!(!result.contains('<'));
        assert!(!result.contains("##"));
        assert!(result.contains("Important"));
        assert!(result.contains("Do the thing"));
    }

    // --- relative_time_label tests (Bug 2: hour-level granularity) ---

    #[test]
    fn test_relative_time_just_now() {
        let now = Utc::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_time_label(&ts, &now), "just now");
    }

    #[test]
    fn test_relative_time_minutes_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::minutes(15))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "15m ago");
    }

    #[test]
    fn test_relative_time_hours_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::hours(3))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "3h ago");
    }

    #[test]
    fn test_same_day_differentiation() {
        let now = Utc::now();
        let ts_2h = (now - chrono::Duration::hours(2))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let ts_6h = (now - chrono::Duration::hours(6))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let label_2h = relative_time_label(&ts_2h, &now);
        let label_6h = relative_time_label(&ts_6h, &now);
        // Same-day sessions must be distinguishable
        assert_ne!(label_2h, label_6h);
        assert_eq!(label_2h, "2h ago");
        assert_eq!(label_6h, "6h ago");
    }

    #[test]
    fn test_relative_time_yesterday() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::hours(30))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "yesterday");
    }

    #[test]
    fn test_relative_time_days_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(3))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "3d ago");
    }

    #[test]
    fn test_relative_time_weeks_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(14))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "2w ago");
    }

    #[test]
    fn test_relative_time_months_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(45))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "1mo ago");
    }

    #[test]
    fn test_relative_time_invalid() {
        let now = Utc::now();
        assert_eq!(relative_time_label("not-a-timestamp", &now), "???");
    }

    // --- compact_preview tests ---

    #[test]
    fn test_compact_preview_short() {
        assert_eq!(compact_preview("short text", 55), "short text");
    }

    #[test]
    fn test_compact_preview_truncates() {
        let long = "a".repeat(100);
        let result = compact_preview(&long, 55);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 58);
    }

    #[test]
    fn test_compact_preview_strips_newlines() {
        assert_eq!(compact_preview("hello\nworld\nfoo", 55), "hello world foo");
    }

    #[test]
    fn test_compact_preview_unicode_safe() {
        let content = "\u{1f600}".repeat(30);
        let result = compact_preview(&content, 10);
        assert!(result.ends_with("..."));
    }

    // --- strip_preamble tests (Bug 4) ---

    #[test]
    fn test_strip_preamble_implement() {
        assert_eq!(
            strip_preamble("Implement the following plan: Fix the timeline"),
            "Fix the timeline"
        );
    }

    #[test]
    fn test_strip_preamble_execute() {
        assert_eq!(
            strip_preamble("Execute the following plan: Phase 5 work"),
            "Phase 5 work"
        );
    }

    #[test]
    fn test_strip_preamble_markdown_header() {
        assert_eq!(strip_preamble("## Phase 3 Implementation"), "Phase 3 Implementation");
    }

    #[test]
    fn test_strip_preamble_no_match() {
        assert_eq!(strip_preamble("Normal text here"), "Normal text here");
    }

    #[test]
    fn test_strip_preamble_prefix_then_header() {
        assert_eq!(
            strip_preamble("Implement the following plan: # Big Plan"),
            "Big Plan"
        );
    }

    // --- is_displayable tests (threshold=6 or has enrichment) ---

    #[test]
    fn test_is_displayable_good_session() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: Some("Did some work".to_string()),
            enrichment: None,
        };
        assert!(is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_too_few_messages() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 1,
            chunk_count: 1,
            summary: Some("Short".to_string()),
            enrichment: None,
        };
        assert!(!is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_five_messages_no_enrichment() {
        // 5 messages is below threshold (6) and no enrichment → not displayable
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 5,
            chunk_count: 2,
            summary: Some("Quick chat".to_string()),
            enrichment: None,
        };
        assert!(!is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_six_messages_with_summary() {
        // 6 messages meets threshold → displayable if has summary
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 6,
            chunk_count: 2,
            summary: Some("Debugging session".to_string()),
            enrichment: None,
        };
        assert!(is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_empty_summary_no_enrichment() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: Some("   ".to_string()),
            enrichment: None,
        };
        assert!(!is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_enrichment_only() {
        // Enrichment present → always displayable regardless of message count
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 3,
            chunk_count: 1,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Edit".to_string()),
        };
        assert!(is_displayable(&session));
    }

    // --- session_title tests ---

    #[test]
    fn test_session_title_strips_preamble() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 2,
            summary: Some("Implement the following plan: ## Fix the timeline bugs".to_string()),
            enrichment: None,
        };
        let title = session_title(&session);
        assert!(!title.contains("Implement the following plan"));
        assert!(title.contains("Fix the timeline bugs"));
    }

    #[test]
    fn test_session_title_caveat_detected_as_noise() {
        // Caveat text from /clear commands should be detected as noise
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 10,
            chunk_count: 2,
            summary: Some("<local-command-caveat>Caveat: The messages below were generated by the user while running local commands</local-command-caveat>".to_string()),
            enrichment: None,
        };
        let title = session_title(&session);
        assert!(!title.contains('<'));
        assert!(!title.contains("Caveat"));
        assert_eq!(title, "(session)");
    }

    #[test]
    fn test_session_title_caveat_with_enrichment_fallback() {
        // When caveat noise is detected, fall back to enrichment-based title
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 10,
            chunk_count: 2,
            summary: Some("<local-command-caveat>Caveat: The messages below</local-command-caveat>".to_string()),
            enrichment: Some("[Heuristic] Project: test\nTools: Edit, Read\nFiles: main.rs, lib.rs".to_string()),
        };
        let title = session_title(&session);
        assert!(title.contains("Edited"));
        assert!(title.contains("main.rs"));
    }

    #[test]
    fn test_session_title_no_summary() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 10,
            chunk_count: 2,
            summary: None,
            enrichment: None,
        };
        assert_eq!(session_title(&session), "(no summary)");
    }

    #[test]
    fn test_session_title_system_reminder_noise() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 20,
            chunk_count: 3,
            summary: Some("<system-reminder>Note: this session was started from context</system-reminder>".to_string()),
            enrichment: Some("[Heuristic] Project: test\nTools: Bash, Read".to_string()),
        };
        let title = session_title(&session);
        // Should fall back to enrichment tools since summary is noise
        assert!(title.contains("Session using"));
        assert!(title.contains("Bash"));
    }

    // --- enrichment parsing + inference tests (Bug 6) ---

    #[test]
    fn test_parse_enrichment_full() {
        let enrichment = "[Heuristic] Project: csr\nMessages: 603 (227 user)\nTools: TaskCreate, Edit, Bash, Read\nFiles: mod.rs, engine.rs\nHad errors: yes";
        let fields = parse_enrichment(enrichment);
        assert_eq!(fields.tools, vec!["TaskCreate", "Edit", "Bash", "Read"]);
        assert_eq!(fields.files, vec!["mod.rs", "engine.rs"]);
        assert!(fields.has_errors);
        assert!(fields.has_edit_tool);
    }

    #[test]
    fn test_parse_enrichment_no_errors() {
        let enrichment = "[Heuristic] Project: csr\nMessages: 100 (50 user)\nTools: Read, Grep\nFiles: main.rs";
        let fields = parse_enrichment(enrichment);
        assert!(!fields.has_errors);
        assert!(!fields.has_edit_tool);
    }

    #[test]
    fn test_infer_from_enrichment_errors_with_edit() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 100,
            chunk_count: 5,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Edit, Bash\nFiles: main.rs\nHad errors: yes".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Fix errors"));
        assert!(result.contains("edits were in progress"));
    }

    #[test]
    fn test_infer_from_enrichment_errors_no_edit() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 100,
            chunk_count: 5,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Read, Grep\nHad errors: yes".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Investigate and fix errors"));
    }

    #[test]
    fn test_infer_from_enrichment_files() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Edit, Read\nFiles: session_start.rs, queries.rs".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Continue work on"));
        assert!(result.contains("session_start.rs"));
    }

    #[test]
    fn test_infer_from_enrichment_large_session() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 500,
            chunk_count: 10,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nMessages: 500 (200 user)\nTools: Read, Grep".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Large session"));
    }

    #[test]
    fn test_infer_from_enrichment_fallback_to_keyword() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: Some("Phase 4 implementation".to_string()),
            enrichment: None,
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Phase 4"));
    }

    // --- enrichment_display tests ---

    #[test]
    fn test_enrichment_display_full() {
        let enrichment = "[Heuristic] Project: csr\nTools: Edit, Bash, Read\nFiles: mod.rs, engine.rs\nHad errors: yes";
        let display = enrichment_display(enrichment).unwrap();
        assert!(display.contains("Tools: Edit, Bash, Read"));
        assert!(display.contains("Files: mod.rs, engine.rs"));
        assert!(display.contains("Had errors"));
    }

    #[test]
    fn test_enrichment_display_no_structured_data() {
        let enrichment = "[Heuristic] Project: csr\nMessages: 10 (5 user)";
        assert!(enrichment_display(enrichment).is_none());
    }

    #[test]
    fn test_enrichment_display_v3_format() {
        let enrichment = "```markdown\n## Search Summary\nImplemented Phase 4 code-aware search with tree-sitter AST.\n\n## Problem-Solution Mapping\n**Request**: stuff";
        let display = enrichment_display(enrichment).unwrap();
        assert!(display.contains("Implemented Phase 4"));
        assert!(display.contains("tree-sitter"));
    }

    #[test]
    fn test_enrichment_display_v3_long_truncates() {
        let long_summary = "A".repeat(120);
        let enrichment = format!("## Search Summary\n{long_summary}\n\n## Other");
        let display = enrichment_display(&enrichment).unwrap();
        assert!(display.ends_with("..."));
        assert!(display.len() <= 85); // 80 + "..."
    }

    #[test]
    fn test_extract_v3_summary_empty() {
        assert!(extract_v3_summary("## Search Summary\n\n## Other").is_none());
    }

    #[test]
    fn test_extract_v3_summary_user_request() {
        let enrichment = "## User Request\n\"Fix the session start hook bugs\"\n\"Review the injection output\"\n\n## Solution Pattern\ncreation: file.md";
        let display = extract_v3_summary(enrichment).unwrap();
        assert!(display.contains("Fix the session start hook bugs"));
    }

    #[test]
    fn test_enrichment_display_v3_user_request_fallback() {
        // When v3 has no Search Summary, falls back to User Request
        let enrichment = "## User Request\n\"Implement the new feature\"\n\n## Solution Pattern\ncreation: file.md";
        let display = enrichment_display(enrichment).unwrap();
        assert!(display.contains("Implement the new feature"));
    }

    // --- format_session_line tests ---

    #[test]
    fn test_format_session_line_with_enrichment() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::hours(2))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: ts,
            total_messages: 293,
            chunk_count: 6,
            summary: Some("Some work".to_string()),
            enrichment: Some("[Heuristic] Project: test\nTools: Edit, Read\nFiles: main.rs".to_string()),
        };
        let line = format_session_line(&session, &now);
        assert!(line.contains("2h ago"));
        assert!(line.contains("293 msgs"));
        assert!(line.contains("Tools: Edit, Read"));
        assert!(line.contains("Files: main.rs"));
    }

    #[test]
    fn test_format_session_line_summary_fallback() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(1))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: ts,
            total_messages: 90,
            chunk_count: 2,
            summary: Some("Phase 3 HNSW persistence work".to_string()),
            enrichment: None,
        };
        let line = format_session_line(&session, &now);
        assert!(line.contains("yesterday"));
        assert!(line.contains("90 msgs"));
        assert!(line.contains("Phase 3 HNSW persistence work"));
    }

    #[test]
    fn test_format_session_line_strips_preamble() {
        let now = Utc::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: ts,
            total_messages: 50,
            chunk_count: 2,
            summary: Some("Implement the following plan: Fix the bugs".to_string()),
            enrichment: None,
        };
        let line = format_session_line(&session, &now);
        assert!(line.contains("Fix the bugs"));
        assert!(!line.contains("Implement the following plan"));
    }
}
