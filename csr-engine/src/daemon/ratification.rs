//! Ratification enrichment — dialog-act extraction + git-ledger corroboration.
//!
//! Produces a per-conversation `ratification_score` = deterministic P(ratified)
//! from LLM-extracted DIRECTS/ACCEPTS/REJECTS/REASKS, optionally capped when
//! local git commits do not corroborate the conversation's files.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Once};
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::import::ConversationChunk;
use crate::storage::{NarrativeUsageRow, RatificationScoreRow, Storage};

const DIGEST_CHAR_CAP: usize = 8000;
const RATIFICATION_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RATIFICATION_PROMPT_SIZE: u64 = 10 * 1024;
const EXTRACTOR_VERSION: &str = "ratification-v2";
const MAX_LEDGER_SHAS: usize = 20;

static DISABLED_LOG: Once = Once::new();

/// Entry-point guard used by the daemon loop. Logs once per process when disabled.
pub fn check_disabled() -> bool {
    let off = crate::narrative::narratives_disabled() || crate::narrative::ratification_disabled();
    if off {
        DISABLED_LOG.call_once(|| {
            tracing::info!(
                "ratification enrichment disabled (CSR_NO_AI_NARRATIVES or CSR_NO_RATIFICATION)"
            );
        });
    }
    off
}

fn load_ratification_prompt() -> String {
    let override_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".claude-self-reflect")
        .join("ratification_prompt.md");

    if override_path.exists() {
        if let Ok(meta) = std::fs::metadata(&override_path) {
            if meta.len() > MAX_RATIFICATION_PROMPT_SIZE {
                tracing::warn!(
                    path = %override_path.display(),
                    size = meta.len(),
                    max = MAX_RATIFICATION_PROMPT_SIZE,
                    "custom ratification prompt too large, using default"
                );
            } else if let Ok(content) = std::fs::read_to_string(&override_path) {
                tracing::info!(
                    path = %override_path.display(),
                    "using custom ratification prompt"
                );
                return content;
            }
        }
    }

    include_str!("../../data/RATIFICATION_PROMPT.md").to_string()
}

fn head_tail(text: &str, char_cap: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= char_cap {
        return text.to_string();
    }
    let head_n = char_cap / 2;
    let tail_n = char_cap / 2;
    let head: String = text.chars().take(head_n).collect();
    let tail: String = text
        .chars()
        .skip(total_chars.saturating_sub(tail_n))
        .collect();
    let omitted = total_chars.saturating_sub(head_n + tail_n);
    format!("{head}\n... [{omitted} chars omitted] ...\n{tail}")
}

/// v2: operator-turn-prioritized digest. Dialog-acts live in user-authored
/// chunks (chunk author = genuine user prose, per import provenance); v1's
/// undifferentiated head+tail sampled mostly assistant/tool text and starved
/// the extractor of operator turns (Gate A' failure, 2026-07-19).
pub fn build_digest(chunks: &[ConversationChunk], char_cap: usize) -> String {
    use crate::provenance::Speaker;
    let user_text: String = chunks
        .iter()
        .filter(|c| c.author == Speaker::User)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n");
    let other_text: String = chunks
        .iter()
        .filter(|c| c.author != Speaker::User)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n");

    if user_text.is_empty() {
        return head_tail(&other_text, char_cap);
    }
    let user_budget = char_cap * 3 / 4;
    let user_part = head_tail(&user_text, user_budget);
    let remaining = char_cap.saturating_sub(user_part.chars().count());
    if remaining < 200 || other_text.is_empty() {
        return format!("=== OPERATOR-TURN EXCERPTS ===\n{user_part}");
    }
    format!(
        "=== OPERATOR-TURN EXCERPTS ===\n{user_part}\n=== OTHER CONTEXT (assistant/tool) ===\n{}",
        head_tail(&other_text, remaining)
    )
}

fn ratification_model_candidates() -> Vec<Option<String>> {
    let mut chain = Vec::with_capacity(3);
    if let Ok(m) = std::env::var("CSR_RATIFICATION_MODEL") {
        let m = m.trim().to_string();
        if !m.is_empty() {
            chain.push(Some(m));
        }
    }
    chain.push(Some("haiku".to_string()));
    chain.push(None);
    chain
}

async fn call_claude_for_acts(prompt: &str) -> Option<crate::narrative::ParsedNarrative> {
    for candidate in ratification_model_candidates() {
        let attempt = tokio::time::timeout(RATIFICATION_TIMEOUT, async {
            let mut cmd = tokio::process::Command::new("claude");
            if let Some(model) = &candidate {
                cmd.args(["--model", model]);
            }
            let mut child = match cmd
                .args(["-p", "-", "--output-format", "json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => return None,
            };

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
                    crate::narrative::AttemptOutcome::Parsed(parsed) => return Some(parsed),
                    crate::narrative::AttemptOutcome::ModelNotFound => continue,
                    crate::narrative::AttemptOutcome::Failed(_) => return None,
                }
            }
            _ => return None,
        }
    }
    None
}

fn strip_json_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```JSON"))
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s)
        .trim();
    s.strip_suffix("```").unwrap_or(s).trim()
}

#[derive(Debug, Deserialize)]
struct ActsPayload {
    acts: Vec<ActItem>,
}

#[derive(Debug, Deserialize)]
struct ActItem {
    #[serde(rename = "type")]
    act_type: String,
    #[allow(dead_code)]
    evidence: Option<String>,
    #[allow(dead_code)]
    msg_hint: Option<String>,
}

fn parse_acts(text: &str) -> Result<(String, u32, u32, u32)> {
    let stripped = strip_json_fences(text);
    let payload: ActsPayload = serde_json::from_str(stripped)
        .or_else(|e| {
            // Models sometimes wrap the JSON in prose; retry on the outermost
            // brace-delimited slice before giving up.
            match (stripped.find('{'), stripped.rfind('}')) {
                (Some(start), Some(end)) if start < end => {
                    serde_json::from_str(&stripped[start..=end])
                }
                _ => Err(e),
            }
        })
        .map_err(|e| anyhow!("ratification acts JSON parse failed: {e}"))?;
    let mut n_directs = 0u32;
    let mut n_accepts = 0u32;
    let mut n_rejects = 0u32;
    for act in &payload.acts {
        match act.act_type.to_uppercase().as_str() {
            "DIRECTS" => n_directs += 1,
            "ACCEPTS" => n_accepts += 1,
            "REJECTS" => n_rejects += 1,
            _ => {}
        }
    }
    let acts_json = serde_json::to_string(&serde_json::json!({
        "acts": payload.acts.iter().map(|a| serde_json::json!({
            "type": a.act_type,
            "evidence": a.evidence.as_deref().unwrap_or(""),
            "msg_hint": a.msg_hint.as_deref().unwrap_or(""),
        })).collect::<Vec<_>>()
    }))
    .unwrap_or_else(|_| stripped.to_string());
    Ok((acts_json, n_directs, n_accepts, n_rejects))
}

/// Basenames too generic to corroborate on their own — these require a
/// parent-dir + basename (last two path components) match instead.
const GENERIC_BASENAMES: &[&str] = &[
    "mod.rs",
    "lib.rs",
    "main.rs",
    "index.ts",
    "index.tsx",
    "index.js",
    "index.jsx",
    "types.ts",
    "utils.ts",
    "utils.py",
    "__init__.py",
    "README.md",
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    "tsconfig.json",
    ".gitignore",
];

fn last_components(path: &str, n: usize) -> Vec<String> {
    Path::new(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .rev()
        .take(n)
        .collect()
}

/// A commit-touched file corroborates a conversation file if the last two
/// path components match, or — for non-generic basenames only — the basename
/// matches (Gate A tweak: generic basenames false-positive across repos).
fn paths_corroborate(commit_file: &str, conv_file: &str) -> bool {
    let c2 = last_components(commit_file, 2);
    let v2 = last_components(conv_file, 2);
    if c2.len() == 2 && c2 == v2 {
        return true;
    }
    let base = match (c2.first(), v2.first()) {
        (Some(a), Some(b)) if a == b => a.clone(),
        _ => return false,
    };
    !GENERIC_BASENAMES.contains(&base.as_str())
}

/// Deterministic corroboration against local git commit ledgers.
async fn ledger_shas(
    conv_files: &[String],
    start_ts: DateTime<Utc>,
    end_ts: DateTime<Utc>,
) -> Vec<String> {
    let repos = match std::env::var("CSR_LEDGER_REPOS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return vec![],
    };

    if conv_files.is_empty() {
        return vec![];
    }

    let since = (start_ts - ChronoDuration::hours(48)).to_rfc3339();
    let until = (end_ts + ChronoDuration::hours(48)).to_rfc3339();

    let mut matched = Vec::new();
    for repo in repos.split(':').filter(|s| !s.is_empty()) {
        if matched.len() >= MAX_LEDGER_SHAS {
            break;
        }
        let output = match tokio::process::Command::new("git")
            .args([
                "-C",
                repo,
                "log",
                &format!("--since={since}"),
                &format!("--until={until}"),
                "--name-only",
                "--format=%H|%cI",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!(repo, error = %e, "ledger git spawn failed; skipping repo");
                continue;
            }
        };
        if !output.status.success() {
            tracing::debug!(
                repo,
                status = %output.status,
                "ledger git log failed; skipping repo"
            );
            continue;
        }
        let stdout = match String::from_utf8(output.stdout) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!(repo, "ledger git log non-UTF8; skipping repo");
                continue;
            }
        };

        let mut current_sha: Option<String> = None;
        let mut current_files: Vec<String> = Vec::new();
        let flush = |sha: Option<String>, files: &mut Vec<String>, out: &mut Vec<String>| {
            if let Some(s) = sha {
                let hit = files
                    .iter()
                    .any(|f| conv_files.iter().any(|c| paths_corroborate(f, c)));
                if hit && !out.contains(&s) && out.len() < MAX_LEDGER_SHAS {
                    out.push(s);
                }
            }
            files.clear();
        };

        for line in stdout.lines() {
            if line.len() >= 41 && line.as_bytes().get(40) == Some(&b'|') {
                let sha_part = &line[..40];
                if sha_part.chars().all(|c| c.is_ascii_hexdigit()) {
                    flush(current_sha.take(), &mut current_files, &mut matched);
                    current_sha = Some(sha_part.to_string());
                    continue;
                }
            }
            if !line.trim().is_empty() {
                current_files.push(line.to_string());
            }
        }
        flush(current_sha.take(), &mut current_files, &mut matched);
    }
    matched
}

/// Pure scoring: Laplace-smoothed positive/(pos+neg+2), capped at 0.6 without ledger corroboration.
pub fn score_ratification(
    n_directs: u32,
    n_accepts: u32,
    n_rejects: u32,
    corroborated: bool,
) -> f32 {
    let pos = n_directs + n_accepts;
    let neg = n_rejects;
    let raw = pos as f32 / (pos + neg + 2) as f32;
    if corroborated {
        raw
    } else {
        raw.min(0.6)
    }
}

/// Process one conversation: extract acts, corroborate, score, persist.
pub async fn process_ratification(storage: &Arc<Storage>, conv_id: &str) -> Result<()> {
    if check_disabled() {
        return Ok(());
    }

    let chunk_ids = storage.get_chunk_ids_for_conversation(conv_id)?;
    if chunk_ids.is_empty() {
        storage.mark_enrichment_completed(conv_id, "ratification", "")?;
        return Ok(());
    }
    let chunks = storage.get_chunks_by_ids(&chunk_ids)?;
    let digest = build_digest(&chunks, DIGEST_CHAR_CAP);
    let prompt = format!(
        "{}\n\nCONVERSATION DIGEST:\n{}",
        load_ratification_prompt(),
        digest
    );

    match call_claude_for_acts(&prompt).await {
        Some(parsed) => match parse_acts(&parsed.text) {
            Ok((acts_json, n_directs, n_accepts, n_rejects)) => {
                let _ = storage.record_narrative_usage(&NarrativeUsageRow {
                    call_site: "ratification".into(),
                    model: parsed.model.clone(),
                    input_tokens: parsed.input_tokens,
                    output_tokens: parsed.output_tokens,
                    cache_read_tokens: parsed.cache_read_tokens,
                    cache_creation_tokens: parsed.cache_creation_tokens,
                    duration_ms: 0,
                    success: true,
                });

                let conv_files = storage.files_for_session(conv_id, 50).unwrap_or_default();
                let (start_ts, end_ts) = conversation_time_span(&chunks);
                let shas = match (start_ts, end_ts) {
                    (Some(s), Some(e)) => ledger_shas(&conv_files, s, e).await,
                    _ => vec![],
                };
                let corroborated = !shas.is_empty();
                let score = score_ratification(n_directs, n_accepts, n_rejects, corroborated);
                let ledger_refs = if shas.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&shas)?)
                };

                storage.upsert_ratification_score(&RatificationScoreRow {
                    conversation_id: conv_id.into(),
                    score,
                    acts_json,
                    ledger_refs,
                    extractor_version: EXTRACTOR_VERSION.into(),
                })?;
                storage.mark_enrichment_completed(conv_id, "ratification", conv_id)?;
                Ok(())
            }
            Err(e) => {
                let _ = storage.record_narrative_usage(&NarrativeUsageRow {
                    call_site: "ratification".into(),
                    model: parsed.model,
                    input_tokens: parsed.input_tokens,
                    output_tokens: parsed.output_tokens,
                    cache_read_tokens: parsed.cache_read_tokens,
                    cache_creation_tokens: parsed.cache_creation_tokens,
                    duration_ms: 0,
                    success: false,
                });
                Err(e)
            }
        },
        None => {
            let _ = storage.record_narrative_usage(&NarrativeUsageRow {
                call_site: "ratification".into(),
                model: "unknown".into(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                duration_ms: 0,
                success: false,
            });
            Err(anyhow!("ratification LLM call failed or unavailable"))
        }
    }
}

fn conversation_time_span(
    chunks: &[ConversationChunk],
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let mut start: Option<DateTime<Utc>> = None;
    let mut end: Option<DateTime<Utc>> = None;
    for c in chunks {
        if let Ok(dt) = DateTime::parse_from_rfc3339(&c.timestamp) {
            let utc = dt.with_timezone(&Utc);
            start = Some(match start {
                Some(s) => s.min(utc),
                None => utc,
            });
            end = Some(match end {
                Some(e) => e.max(utc),
                None => utc,
            });
        }
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_acts_handles_prose_wrapped_json() {
        let (_, d, a, r) = parse_acts(
            "Looking at the digest, here are the acts:\n{\"acts\": [{\"type\": \"DIRECTS\", \"evidence\": \"fix it\", \"msg_hint\": \"early\"}]}\nLet me know if you need more.",
        )
        .unwrap();
        assert_eq!((d, a, r), (1, 0, 0));
        assert!(parse_acts("no json here at all").is_err());
    }

    #[test]
    fn generic_basename_requires_parent_dir_match() {
        assert!(!paths_corroborate(
            "src/storage/mod.rs",
            "src/daemon/mod.rs"
        ));
        assert!(paths_corroborate(
            "src/daemon/mod.rs",
            "a/b/src/daemon/mod.rs"
        ));
        assert!(!paths_corroborate("app/index.ts", "web/index.ts"));
    }

    #[test]
    fn specific_basename_matches_across_dirs() {
        assert!(paths_corroborate(
            "src/daemon/ratification.rs",
            "other/path/ratification.rs"
        ));
        assert!(!paths_corroborate("src/a.rs", "src/b.rs"));
    }

    #[test]
    fn score_empty_conversation_is_zero() {
        assert_eq!(score_ratification(0, 0, 0, false), 0.0);
    }

    #[test]
    fn score_uncorroborated_capped_at_0_6() {
        assert!(score_ratification(3, 0, 0, false) <= 0.6);
        assert!((score_ratification(3, 3, 0, false) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn score_corroborated_uses_raw() {
        let expected = 6.0 / 8.0;
        assert!((score_ratification(3, 3, 0, true) - expected).abs() < 1e-6);
    }

    #[test]
    fn score_rejects_heavy_is_low() {
        assert!(score_ratification(0, 0, 4, false) < 0.2);
    }

    #[test]
    fn ratification_disabled_env_check() {
        std::env::set_var("CSR_NO_RATIFICATION", "1");
        assert!(crate::narrative::ratification_disabled());
        std::env::remove_var("CSR_NO_RATIFICATION");
    }

    #[test]
    fn build_digest_truncates_long_input() {
        let long = "a".repeat(100);
        let chunk = ConversationChunk {
            id: "c1".into(),
            conversation_id: "conv".into(),
            project_name: "p".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: long,
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let digest = build_digest(&[chunk], 40);
        assert!(digest.contains("chars omitted"));
        // v2: user-authored content leads under the operator-turn header,
        // truncated to the 3/4 user budget (head+tail of 30 chars).
        assert!(digest.starts_with("=== OPERATOR-TURN EXCERPTS ===\n"));
        assert!(digest.ends_with("aaaaaaaaaaaaaaa")); // tail half of 30
    }

    #[test]
    fn build_digest_prioritizes_operator_turns() {
        let mk = |id: &str, content: &str, author: crate::provenance::Speaker| ConversationChunk {
            id: id.into(),
            conversation_id: "conv".into(),
            project_name: "p".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author,
            seq: 0,
            is_sidechain: false,
        };
        let chunks = vec![
            mk(
                "c1",
                "assistant explanation text",
                crate::provenance::Speaker::Assistant,
            ),
            mk("c2", "fix the import bug", crate::provenance::Speaker::User),
            mk("c3", "tool output", crate::provenance::Speaker::ToolResult),
        ];
        let digest = build_digest(&chunks, 8000);
        let op = digest.find("OPERATOR-TURN EXCERPTS").unwrap();
        let user_pos = digest.find("fix the import bug").unwrap();
        let other = digest.find("OTHER CONTEXT").unwrap();
        assert!(op < user_pos && user_pos < other);
        assert!(digest.find("assistant explanation text").unwrap() > other);
    }
}
