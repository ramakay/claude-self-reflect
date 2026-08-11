//! Journal v3 "Dreams Gateway" Phase 1.5 — night-pass thread extraction
//! (propose-verify), writing `dream_threads`.
//!
//! # Propose-verify
//!
//! **Propose**: one `claude -p` (or a `CSR_NIGHT_ACTOR_CMD` custom actor)
//! call per candidate episode (v2, `outcome IN ('partial','failed')`, at
//! least one `files_modified`/`investigated` entry), fed the episode's
//! request/outcome/completed-summary plus its transcript tail, asked to name
//! up to 4 genuinely unfinished threads with a VERBATIM evidence quote and a
//! files list drawn only from the episode's own files.
//!
//! **Verify** (deterministic, no LLM — see [`verify_reply`]): every
//! evidence_quote must be an exact substring of the prompt text actually
//! sent (one re-prompt retry for the whole episode when any quote fails,
//! then a hard drop of anything still failing); every file must be in the
//! episode's own allowlist; and receipts are joined against
//! `witness_verdicts`/`witness_ledger` to assign a [`ReceiptTier`] —
//! `Verdict` (a verdict-bearing witness matches) beats `Witnessed` (only
//! ledger rows match, no verdict) beats `Unverified` (nothing matches). All
//! three tiers are stored; the future renderer, not this module, decides
//! which tiers reach the homepage. `UNVERIFIED` must never be rendered as
//! "live"/"re-verified" — this module only ever writes it as the honest
//! bottom tier.
//!
//! # Convergence by construction
//!
//! Each candidate episode's `episode_hash` folds its content (session id,
//! request, completed summary, outcome, files, the exact transcript-tail
//! chunk ids used) together with [`THREAD_PROMPT_VERSION`] and the
//! configured target model. A re-run over an unchanged episode either hits
//! rows already stored under that hash (real threads) or a cached sentinel
//! row (`thread = ''`, written when a run produced zero acceptable
//! threads) — either way, [`already_converged`] short-circuits before any
//! actor is invoked, so a frozen corpus costs zero further spend. This
//! mirrors `dream::report::curate_sentences_with`'s `FALLBACK_MODEL_TAG`
//! convergence-by-construction pattern, with the sentinel row standing in
//! for that module's fallback-sentence cache entry.
//!
//! A genuine actor failure (process could not be spawned, timed out, every
//! model candidate exhausted) is deliberately NOT cached — [`verify_reply`]
//! returns `None` and the episode is simply skipped this run, retried on
//! the next cadence tick. Only a reply that the actor DID produce, but that
//! verification reduced to zero acceptable threads, converges via the
//! sentinel.
//!
//! # Kill switches
//!
//! [`threads_disabled`] is true (and the whole pass no-ops) when
//! `CSR_NO_AI_NARRATIVES=1`, `CSR_NO_DREAMING=1`, or `CSR_DREAM_THREADS` is
//! unset/not exactly `"1"` — this is a NEW spend surface layered on top of
//! the deterministic (zero-LLM) `dream` pass, so it defaults OFF.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::narrative::{AttemptOutcome, ParsedNarrative};
use crate::storage::{dream_items, witness_ledger, NarrativeUsageRow, Storage};

/// Bumped whenever the extraction prompt or its output contract changes.
/// Folded into `episode_hash` so every row cached under an older prompt
/// version misses deterministically — no ALTER, no backfill, no dual-read
/// (same idiom as `dream::report::CURATION_PROMPT_VERSION`).
const THREAD_PROMPT_VERSION: u32 = 1;

/// Default `claude -p` model when `CSR_DREAM_THREAD_MODEL` is unset — chosen
/// after the live test's haiku truncated-quote failure (plan §Phase 1.5).
const DEFAULT_THREAD_MODEL: &str = "sonnet-5";

/// Absolute ceiling on candidate episodes considered per run, whatever the
/// effort tier or the `CSR_DREAM_THREADS_CAP` override asks for.
const MAX_CANDIDATE_CAP: usize = 40;

/// Transcript-tail chunks pulled per episode, and the per-chunk char cap —
/// matches the plan's live-tested prompt shape.
const TAIL_CHUNK_COUNT: i64 = 2;
const TAIL_CHUNK_CHAR_CAP: usize = 2400;

/// Hard prompt size ceiling (plan §Phase 1.5).
const PROMPT_CAP_BYTES: usize = 8 * 1024;

/// Threads accepted per episode, matching the rules block's own "max 4".
const MAX_THREADS_PER_EPISODE: usize = 4;

/// Actor process timeout (both the `claude -p` path and the
/// `CSR_NIGHT_ACTOR_CMD` custom-command path).
const ACTOR_TIMEOUT_SECS: u64 = 120;

/// `meta` key: RFC3339 timestamp of the last completed thread-extraction
/// pass (set even when it processed zero candidates — that is still "ran").
pub(crate) const META_LAST_RUN_AT: &str = "dream_threads_last_run_at";
/// `meta` key: `"1"`/`"0"` — did the last pass add zero new rows (real
/// threads or sentinels)? Read by `status` as the `converged` flag.
pub(crate) const META_LAST_CONVERGED: &str = "dream_threads_last_converged";

// ─── kill switches / config ──────────────────────────────────────────────

/// `CSR_DREAM_THREADS` opt-in gate: must be exactly `"1"` (unset or any
/// other value stays off) — a NEW spend surface, default OFF.
pub fn threads_enabled() -> bool {
    std::env::var("CSR_DREAM_THREADS")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// Whole-pass kill switch: `CSR_NO_AI_NARRATIVES`, `CSR_NO_DREAMING`, or the
/// `CSR_DREAM_THREADS` opt-in gate not being on.
pub fn threads_disabled() -> bool {
    crate::narrative::narratives_disabled()
        || crate::daemon::dream_cadence::dreaming_disabled()
        || !threads_enabled()
}

/// Model candidate chain: `CSR_DREAM_THREAD_MODEL` override, then the
/// effort tier's model (Journal v4 P5, locked decision 14 — the tier's
/// "reasoning effort" is delivered as which model reasons, since `claude -p`
/// has no separate effort flag), then `None` (let the `claude` CLI pick its
/// own default) — same shape as `narrative::model_candidates`.
///
/// The default tier (`balanced`) resolves to [`DEFAULT_THREAD_MODEL`], so a
/// user who never sets `CSR_DREAM_EFFORT` sees exactly the pre-P5 chain.
pub fn thread_model_candidates() -> Vec<Option<String>> {
    thread_model_candidates_for(crate::dream::policy::effort_tier())
}

/// Pure core of [`thread_model_candidates`] with the tier passed in.
pub fn thread_model_candidates_for(tier: crate::dream::policy::EffortTier) -> Vec<Option<String>> {
    let mut chain = Vec::with_capacity(3);
    if let Ok(m) = std::env::var("CSR_DREAM_THREAD_MODEL") {
        let m = m.trim().to_string();
        if !m.is_empty() {
            chain.push(Some(m));
        }
    }
    chain.push(Some(tier.model().to_string()));
    chain.push(None);
    chain
}

/// The configured TARGET model (env override or the default) — folded into
/// `episode_hash` as a version tag. Deliberately NOT the model that actually
/// served a given request (that is `model_used`, stored in the `model`
/// column) — the hash must be computable before any invocation happens, so
/// convergence checks never themselves cost a call.
pub fn primary_thread_model() -> String {
    thread_model_candidates()
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| DEFAULT_THREAD_MODEL.to_string())
}

/// Whether a non-Claude night actor is configured via `CSR_NIGHT_ACTOR_CMD`
/// — surfaced in `status` alongside [`primary_thread_model`].
pub fn night_actor_cmd_configured() -> bool {
    std::env::var("CSR_NIGHT_ACTOR_CMD")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn candidate_cap() -> usize {
    candidate_cap_for(
        std::env::var("CSR_DREAM_THREADS_CAP").ok().as_deref(),
        crate::dream::policy::effort_tier(),
    )
}

/// Pure core of [`candidate_cap`]: the explicit `CSR_DREAM_THREADS_CAP`
/// override when it parses as a positive integer, else the effort tier's
/// episodes-per-pass — both clamped to [`MAX_CANDIDATE_CAP`].
pub(crate) fn candidate_cap_for(
    raw: Option<&str>,
    tier: crate::dream::policy::EffortTier,
) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or_else(|| tier.episodes_per_pass())
        .min(MAX_CANDIDATE_CAP)
}

// ─── types ────────────────────────────────────────────────────────────────

/// Which evidence channel backs a stored [`DreamThread`] — see the module
/// doc's verify step. `Ord` derived so `Verdict < Witnessed < Unverified`,
/// matching "verdict beats witnessed beats unverified" as a render-priority
/// ordering (lower = stronger).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReceiptTier {
    Verdict,
    Witnessed,
    Unverified,
}

impl ReceiptTier {
    fn as_str(self) -> &'static str {
        match self {
            ReceiptTier::Verdict => "verdict",
            ReceiptTier::Witnessed => "witnessed",
            ReceiptTier::Unverified => "unverified",
        }
    }

    fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "verdict" => Some(Self::Verdict),
            "witnessed" => Some(Self::Witnessed),
            "unverified" => Some(Self::Unverified),
            _ => None,
        }
    }
}

/// One receipt backing a [`DreamThread`] — either a verdict-bearing witness
/// match or a plain "witnessed, no verdict" file match. `#[serde(untagged)]`
/// so the persisted JSON shape is exactly the plan's two schemas, not a
/// Rust-enum-tagged wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Receipt {
    Verdict {
        symbol: Option<String>,
        verdict: String,
        receipt_oid: Option<String>,
        witnessed_at: String,
    },
    Witnessed {
        file: String,
        witness_count: i64,
    },
}

/// One extracted, verified, receipt-tiered night-pass thread — a candidate
/// card source for the (future) Phase 2 renderer. Read via
/// [`load_dream_threads`], which skips sentinel rows.
#[derive(Debug, Clone, PartialEq)]
pub struct DreamThread {
    pub id: i64,
    /// The convergence hash this thread was extracted under. Doubles as the
    /// `narrative_usage.ref_id` its spend rows carry (Journal v4 P4), so the
    /// composer can total a dream's cost from stored rows instead of a
    /// timestamp window.
    pub episode_hash: String,
    pub session_id: String,
    pub project: String,
    pub thread: String,
    pub evidence_quote: String,
    pub files: Vec<String>,
    pub receipt_tier: ReceiptTier,
    pub receipts: Vec<Receipt>,
    pub model: String,
    pub created_at: String,
}

/// Summary of one [`run_thread_extraction`] pass — daemon logging only.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadExtractionStats {
    pub skipped: bool,
    pub candidates: usize,
    pub threads_stored: usize,
    pub sentinels_stored: usize,
    pub errors: usize,
    /// Candidates never attempted because the pass budget was already spent
    /// — a counted remainder queued for the next pass, never an estimate.
    pub budget_queued: usize,
}

// ─── episode candidates ────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EpisodeRecord {
    session_id: String,
    project: String,
    request: String,
    outcome: String,
    completed: String,
    files_modified: Vec<String>,
    investigated: Vec<String>,
}

struct EpisodeCandidate {
    session_id: String,
    project: String,
    request: Option<String>,
    completed: Option<String>,
    outcome: String,
    /// `files_modified` + `investigated`, deduped, non-blank.
    files: Vec<String>,
}

fn nonblank(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

/// v2 episodes with `outcome IN ('partial','failed')` and at least one
/// `files_modified`/`investigated` entry, newest first, capped at `cap`.
/// Fail-open: a malformed episode row is skipped, not fatal.
fn load_candidate_episodes(conn: &Connection, cap: usize) -> Result<Vec<EpisodeCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT content,
                COALESCE(julianday(json_extract(content, '$.timestamp')), julianday(timestamp), 0.0)
         FROM reflections
         WHERE json_valid(content)
           AND json_extract(content, '$.schema') = 'v2'
           AND json_extract(content, '$.outcome') IN ('partial', 'failed')
         ORDER BY 2 DESC, rowid DESC",
    )?;
    let rows: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::new();
    for content in rows {
        if out.len() >= cap {
            break;
        }
        let Ok(ep) = serde_json::from_str::<EpisodeRecord>(&content) else {
            continue;
        };
        if ep.session_id.trim().is_empty() {
            continue;
        }
        let mut files: Vec<String> = ep
            .files_modified
            .into_iter()
            .chain(ep.investigated)
            .filter(|f| !f.trim().is_empty())
            .collect();
        files.sort();
        files.dedup();
        if files.is_empty() {
            continue;
        }
        out.push(EpisodeCandidate {
            session_id: ep.session_id,
            project: ep.project,
            request: nonblank(ep.request),
            completed: nonblank(ep.completed),
            outcome: ep.outcome,
            files,
        });
    }
    Ok(out)
}

/// How many candidate episodes the corpus holds right now — the measured
/// basis for `setup`'s per-night token estimate (locked decision 15). Counts
/// the same candidates a real pass would consider, without the tier cap
/// applied, so the estimate can show both the corpus number and the bound
/// the tier puts on it. Fail-soft to 0 on a storage error: an estimate built
/// from a failed query would be a guess.
pub fn count_candidate_episodes(storage: &Storage) -> usize {
    storage
        .with_connection(|conn| load_candidate_episodes(conn, usize::MAX))
        .map(|episodes| episodes.len())
        .unwrap_or(0)
}

/// Files eligible for the prompt's FILES allowlist: the episode's own files,
/// minus scratchpad/memory paths (never real evidence of a code thread).
fn filtered_files(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|f| !f.contains("/scratchpad/") && !f.contains("/memory/"))
        .cloned()
        .collect()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Last `TAIL_CHUNK_COUNT` chunks (by insertion order) for `session_id`,
/// chronological, each capped to `TAIL_CHUNK_CHAR_CAP` chars. `LIKE` (not
/// `=`) per the plan's spec — behaves as an exact match for the ordinary
/// case (session ids carry no `%`/`_` wildcard characters) while staying
/// literal to the specified query shape.
fn load_tail_chunks(conn: &Connection, session_id: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, content FROM chunks WHERE conversation_id LIKE ?1 ORDER BY rowid DESC LIMIT ?2",
    )?;
    let mut rows: Vec<(String, String)> = stmt
        .query_map(params![session_id, TAIL_CHUNK_COUNT], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    for (_, content) in rows.iter_mut() {
        *content = truncate_chars(content, TAIL_CHUNK_CHAR_CAP);
    }
    Ok(rows)
}

// ─── episode hash / convergence ────────────────────────────────────────────

/// SHA-256 over the episode's evidence inputs, the exact tail-chunk ids used
/// (so a transcript that has since grown re-derives), [`THREAD_PROMPT_VERSION`],
/// and the configured target `model` — see the module doc's convergence
/// section.
fn episode_hash(ep: &EpisodeCandidate, tail_chunk_ids: &[String], model: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ep.session_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(ep.request.as_deref().unwrap_or("").as_bytes());
    hasher.update([0u8]);
    hasher.update(ep.completed.as_deref().unwrap_or("").as_bytes());
    hasher.update([0u8]);
    hasher.update(ep.outcome.as_bytes());
    hasher.update([0u8]);
    for f in &ep.files {
        hasher.update(f.as_bytes());
        hasher.update([0u8]);
    }
    for id in tail_chunk_ids {
        hasher.update(id.as_bytes());
        hasher.update([0u8]);
    }
    hasher.update(THREAD_PROMPT_VERSION.to_le_bytes());
    hasher.update(model.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

fn already_converged(conn: &Connection, hash: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dream_threads WHERE episode_hash = ?1",
        params![hash],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

// ─── prompt ─────────────────────────────────────────────────────────────

const RULES_BLOCK: &str =
    "You are extracting UNFINISHED work threads from one development session \
for a developer's private overnight journal.\n\
\n\
Rules:\n\
- Return ONLY a JSON array, at most 4 items, no markdown fence, no prose before or after.\n\
- Each item is an object: {\"thread\": <one sentence naming the unfinished work>, \
\"evidence_quote\": <a VERBATIM substring copied exactly, character-for-character, from the \
RECORD or TRANSCRIPT_TAIL text below>, \"files\": [<zero or more paths, ONLY from the FILES \
list below>]}.\n\
- evidence_quote MUST be an exact, contiguous, uninterrupted substring of the text you were \
given below. Do not paraphrase it, do not summarize it, do not truncate it with an ellipsis, \
do not add or remove punctuation.\n\
- files MUST be a subset of the FILES list. Never invent a path that is not listed.\n\
- If nothing genuinely unfinished is evidenced, return an empty array: [].\n";

const RETRY_CORRECTION: &str = "\n\nCORRECTION: one or more of your evidence_quote values were \
NOT exact substrings of the RECORD/TRANSCRIPT_TAIL text above. Re-extract, copying each \
evidence_quote VERBATIM, character-for-character, directly from that text. Return ONLY the \
corrected JSON array.";

/// Builds the extraction prompt: rules block, RECORD (outcome/request/final
/// summary/transcript tail), FILES allowlist — capped at [`PROMPT_CAP_BYTES`].
fn build_prompt(
    ep: &EpisodeCandidate,
    tail_chunks: &[(String, String)],
    files: &[String],
) -> String {
    let record = serde_json::json!({
        "outcome": ep.outcome,
        "request": ep.request,
        "final_summary": ep.completed,
        "transcript_tail": tail_chunks.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>(),
    });
    let prompt = format!(
        "{RULES_BLOCK}\nRECORD:\n{}\n\nFILES:\n{}\n",
        serde_json::to_string(&record).unwrap_or_default(),
        serde_json::to_string(files).unwrap_or_default(),
    );
    if prompt.len() > PROMPT_CAP_BYTES {
        truncate_bytes(&prompt, PROMPT_CAP_BYTES)
    } else {
        prompt
    }
}

// ─── reply parsing / verification ──────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawThread {
    thread: String,
    evidence_quote: String,
    files: Vec<String>,
}

/// Same fence-stripping idiom as `daemon::ratification::strip_json_fences`.
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

/// Parse the actor's raw reply into candidate threads. Fail-open: any shape
/// problem (malformed JSON, wrong top-level type) yields an empty vec, which
/// converges to a sentinel row exactly like a genuine "nothing unfinished"
/// reply — the two cases are deliberately indistinguishable to the caller.
fn parse_threads(text: &str) -> Vec<RawThread> {
    let clean = strip_json_fences(text);
    serde_json::from_str::<Vec<RawThread>>(clean)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !t.thread.trim().is_empty() && !t.evidence_quote.trim().is_empty())
        .collect()
}

fn passes_quote_gate(thread: &RawThread, prompt: &str) -> bool {
    prompt.contains(thread.evidence_quote.as_str())
}

fn passes_file_allowlist(thread: &RawThread, allowlist: &[String]) -> bool {
    thread.files.iter().all(|f| allowlist.contains(f))
}

// ─── actor abstraction ──────────────────────────────────────────────────

/// One raw actor attempt outcome — mirrors `narrative::AttemptOutcome` so
/// the `claude -p` path can reuse `narrative::classify_attempt` directly.
pub(crate) enum ActorAttempt {
    Parsed(ParsedNarrative),
    ModelNotFound,
    Failed(String),
}

/// Abstraction over "run the night actor once and get a reply back".
/// Production invokes a real process (`claude -p` or `CSR_NIGHT_ACTOR_CMD`);
/// tests inject a closure so they never spawn anything. Any
/// `Fn(Option<&str>, &str) -> ActorAttempt` is a [`NightActor`] via the
/// blanket impl below.
pub(crate) trait NightActor {
    fn invoke(&self, model: Option<&str>, prompt: &str) -> ActorAttempt;
}

impl<F> NightActor for F
where
    F: Fn(Option<&str>, &str) -> ActorAttempt,
{
    fn invoke(&self, model: Option<&str>, prompt: &str) -> ActorAttempt {
        self(model, prompt)
    }
}

/// Production actor: dispatches to `CSR_NIGHT_ACTOR_CMD` when configured,
/// else `claude -p`.
pub(crate) struct ProcessActor;

impl NightActor for ProcessActor {
    fn invoke(&self, model: Option<&str>, prompt: &str) -> ActorAttempt {
        match std::env::var("CSR_NIGHT_ACTOR_CMD") {
            Ok(template) if !template.trim().is_empty() => {
                invoke_custom_actor(&template, model, prompt)
            }
            _ => invoke_claude_p(model, prompt),
        }
    }
}

fn dream_threads_data_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".claude-self-reflect")
        .join("dream-threads-tmp");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write_minimal_mcp_config() -> Result<PathBuf> {
    let config = serde_json::json!({ "mcpServers": {} });
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".claude-self-reflect");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("dream-threads-mcp.json");
    std::fs::write(&path, serde_json::to_string(&config)?)?;
    Ok(path)
}

/// Single `claude -p` attempt for one model candidate. Manual poll-timeout
/// loop, same idiom as `hooks::session_briefing::invoke_narrative_briefing`
/// (sync `std::process::Command`, no tokio needed — this runs inside the
/// daemon's `spawn_blocking` dream-cycle closure).
fn invoke_claude_p(model: Option<&str>, prompt: &str) -> ActorAttempt {
    let mcp_config_path = match write_minimal_mcp_config() {
        Ok(p) => p,
        Err(e) => return ActorAttempt::Failed(format!("mcp config: {e}")),
    };
    let mut cmd = std::process::Command::new("claude");
    cmd.arg("-p").arg(prompt);
    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    cmd.arg("--output-format")
        .arg("json")
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg(&mcp_config_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .env("CSR_DISABLE_RECURSIVE_HOOKS", "1");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ActorAttempt::Failed(format!("spawn failed: {e}")),
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed().as_secs() >= ACTOR_TIMEOUT_SECS {
                    let _ = child.kill();
                    return ActorAttempt::Failed(format!(
                        "claude -p timed out after {ACTOR_TIMEOUT_SECS}s"
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return ActorAttempt::Failed(format!("wait failed: {e}")),
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return ActorAttempt::Failed(format!("collect output failed: {e}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    match crate::narrative::classify_attempt(output.status.success(), &stdout, &stderr) {
        AttemptOutcome::Parsed(p) => ActorAttempt::Parsed(p),
        AttemptOutcome::ModelNotFound => ActorAttempt::ModelNotFound,
        AttemptOutcome::Failed(msg) => ActorAttempt::Failed(msg),
    }
}

/// `CSR_NIGHT_ACTOR_CMD` shell-template path: substitute `{model}`
/// `{prompt_file}` `{out_file}`, run via `sh -c`, read `out_file` back as the
/// raw reply text. No token-usage reporting is possible for an arbitrary
/// actor, so usage is recorded as zeros. A single attempt only — an
/// arbitrary command has no "model not found" signal to walk the chain on,
/// so a failure here is terminal (bounded spend per the plan: "bad actors
/// cost bounded spend, never bad data").
fn invoke_custom_actor(template: &str, model: Option<&str>, prompt: &str) -> ActorAttempt {
    let model_label = model.unwrap_or(DEFAULT_THREAD_MODEL);
    let dir = match dream_threads_data_dir() {
        Ok(d) => d,
        Err(e) => return ActorAttempt::Failed(format!("data dir: {e}")),
    };
    let run_id = uuid::Uuid::new_v4();
    let prompt_file = dir.join(format!("prompt-{run_id}.txt"));
    let out_file = dir.join(format!("out-{run_id}.txt"));
    if let Err(e) = std::fs::write(&prompt_file, prompt) {
        return ActorAttempt::Failed(format!("write prompt file: {e}"));
    }

    let command_str = substitute_actor_template(template, model_label, &prompt_file, &out_file);
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(&command_str)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&prompt_file);
            return ActorAttempt::Failed(format!("actor spawn failed: {e}"));
        }
    };

    let start = Instant::now();
    let wait_result = loop {
        match child.try_wait() {
            Ok(Some(_status)) => break Ok(()),
            Ok(None) => {
                if start.elapsed().as_secs() >= ACTOR_TIMEOUT_SECS {
                    let _ = child.kill();
                    break Err(format!(
                        "actor command timed out after {ACTOR_TIMEOUT_SECS}s"
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => break Err(format!("actor wait failed: {e}")),
        }
    };
    let _ = std::fs::remove_file(&prompt_file);
    if let Err(msg) = wait_result {
        let _ = std::fs::remove_file(&out_file);
        return ActorAttempt::Failed(msg);
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_file(&out_file);
            return ActorAttempt::Failed(format!("actor collect output failed: {e}"));
        }
    };
    if !output.status.success() {
        let _ = std::fs::remove_file(&out_file);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ActorAttempt::Failed(format!("actor command failed: {stderr}"));
    }

    let text = match std::fs::read_to_string(&out_file) {
        Ok(t) => t,
        Err(e) => return ActorAttempt::Failed(format!("read out_file failed: {e}")),
    };
    let _ = std::fs::remove_file(&out_file);
    if text.trim().is_empty() {
        return ActorAttempt::Failed("actor produced empty output".to_string());
    }

    ActorAttempt::Parsed(ParsedNarrative {
        text,
        model: model_label.to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    })
}

/// Pure string substitution — unit-testable without spawning anything.
fn substitute_actor_template(
    template: &str,
    model: &str,
    prompt_file: &std::path::Path,
    out_file: &std::path::Path,
) -> String {
    template
        .replace("{model}", model)
        .replace("{prompt_file}", &prompt_file.to_string_lossy())
        .replace("{out_file}", &out_file.to_string_lossy())
}

// ─── model-chain walk + usage accounting ───────────────────────────────────

pub(crate) struct ChainAttempt {
    model_label: String,
    success: bool,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
}

pub(crate) struct ChainResult {
    pub(crate) text: Option<String>,
    pub(crate) model_used: String,
    pub(crate) attempts: Vec<ChainAttempt>,
}

/// Walks `chain`, invoking `actor` for each candidate until one Parses
/// (stop) or the chain is exhausted. `ModelNotFound` continues to the next
/// candidate (like `narrative`'s model chain); any other `Failed` stops the
/// walk immediately — non-model failures (timeouts, spawn errors, a custom
/// actor's terminal failure) must never burn the rest of the chain.
///
/// `pub(crate)` so `journal::composer` (Journal v4 P4) drives its structured
/// plan through this exact walk rather than standing up a second one.
pub(crate) fn invoke_chain(
    actor: &dyn NightActor,
    chain: &[Option<String>],
    prompt: &str,
) -> ChainResult {
    let mut attempts = Vec::new();
    for candidate in chain {
        let label = candidate.clone().unwrap_or_else(|| "default".to_string());
        match actor.invoke(candidate.as_deref(), prompt) {
            ActorAttempt::Parsed(p) => {
                attempts.push(ChainAttempt {
                    model_label: p.model.clone(),
                    success: true,
                    input_tokens: p.input_tokens,
                    output_tokens: p.output_tokens,
                    cache_read_tokens: p.cache_read_tokens,
                    cache_creation_tokens: p.cache_creation_tokens,
                });
                return ChainResult {
                    text: Some(p.text),
                    model_used: p.model,
                    attempts,
                };
            }
            ActorAttempt::ModelNotFound => {
                attempts.push(ChainAttempt {
                    model_label: label,
                    success: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                });
                continue;
            }
            ActorAttempt::Failed(_msg) => {
                attempts.push(ChainAttempt {
                    model_label: label,
                    success: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                });
                break;
            }
        }
    }
    ChainResult {
        text: None,
        model_used: "unknown".to_string(),
        attempts,
    }
}

/// Write one `narrative_usage` row per attempt, tagged with `ref_id` — the
/// convergence hash the work was done under (Journal v4 P4, locked decision
/// 13). `None` leaves the rows unattributed, which is what the tests that do
/// not exercise spend attribution pass.
///
/// `pub(crate)` so `journal::composer` accounts its plan spend through the
/// same writer rather than a second one that could drift.
pub(crate) fn record_attempts(
    storage: &Storage,
    attempts: &[ChainAttempt],
    call_site: &str,
    ref_id: Option<&str>,
) {
    for a in attempts {
        let _ = storage.record_narrative_usage_for(
            &NarrativeUsageRow {
                call_site: call_site.to_string(),
                model: a.model_label.clone(),
                input_tokens: a.input_tokens,
                output_tokens: a.output_tokens,
                cache_read_tokens: a.cache_read_tokens,
                cache_creation_tokens: a.cache_creation_tokens,
                duration_ms: 0,
                success: a.success,
            },
            ref_id,
        );
    }
}

/// Propose, then verify. `None` means the actor never produced a usable
/// reply at all (every model candidate failed/timed out) — the caller must
/// skip this episode without caching anything, so it is retried next run.
/// `Some((threads, model_used))` — `threads` may be empty, which the caller
/// caches as a sentinel (a genuine "the actor replied but verification kept
/// nothing" outcome, indistinguishable from "the actor said nothing was
/// unfinished").
pub(crate) fn verify_reply(
    actor: &dyn NightActor,
    chain: &[Option<String>],
    prompt: &str,
    allowlist: &[String],
    storage: &Storage,
    ref_id: Option<&str>,
) -> Option<(Vec<RawThread>, String)> {
    let first = invoke_chain(actor, chain, prompt);
    record_attempts(storage, &first.attempts, "dream_threads", ref_id);
    let text = first.text?;

    let mut threads = parse_threads(&text);
    threads.truncate(MAX_THREADS_PER_EPISODE);
    let mut model_used = first.model_used;

    if threads.iter().any(|t| !passes_quote_gate(t, prompt)) {
        let retry_prompt = format!("{prompt}{RETRY_CORRECTION}");
        let retry = invoke_chain(actor, chain, &retry_prompt);
        record_attempts(storage, &retry.attempts, "dream_threads", ref_id);
        if let Some(retry_text) = retry.text {
            threads = parse_threads(&retry_text);
            threads.truncate(MAX_THREADS_PER_EPISODE);
            model_used = retry.model_used;
        }
        // Deliberately re-checked against the ORIGINAL prompt (never the
        // retry prompt, which also contains the correction line itself) —
        // evidence must be grounded in the real episode/transcript text.
    }

    threads.retain(|t| passes_quote_gate(t, prompt));
    threads.retain(|t| passes_file_allowlist(t, allowlist));

    Some((threads, model_used))
}

// ─── receipts ───────────────────────────────────────────────────────────

const MAX_RECEIPTS: usize = 8;

fn dedup_receipts(receipts: &mut Vec<Receipt>) {
    let mut seen = std::collections::HashSet::new();
    receipts.retain(|r| seen.insert(r.clone()));
    receipts.truncate(MAX_RECEIPTS);
}

/// Joins a thread's evidence (its `files` and code-grade tokens extracted
/// from its own text) against `witness_verdicts`/`witness_ledger` for
/// `project`, reusing `dream_items`'s file-identity comparator and token
/// extractor. `Verdict` beats `Witnessed` beats `Unverified` — see the
/// module doc.
fn compute_receipts(
    conn: &Connection,
    project: &str,
    thread_text: &str,
    files: &[String],
) -> (ReceiptTier, Vec<Receipt>) {
    let verdict_rows = dream_items::verdict_rows_for_project(conn, project).unwrap_or_default();
    let tokens = dream_items::extract_code_tokens(thread_text);
    let file_keys: Vec<String> = files
        .iter()
        .map(|f| dream_items::last_two_segments(f))
        .collect();

    let mut verdict_receipts: Vec<Receipt> = Vec::new();
    for row in &verdict_rows {
        let file_match = file_keys.contains(&dream_items::last_two_segments(&row.file));
        let token_match = row
            .symbol
            .as_deref()
            .map(|s| tokens.iter().any(|t| t.eq_ignore_ascii_case(s)))
            .unwrap_or(false);
        if file_match || token_match {
            verdict_receipts.push(Receipt::Verdict {
                symbol: row.symbol.clone(),
                verdict: row.verdict.clone(),
                receipt_oid: row.receipt_oid.clone(),
                witnessed_at: row.witnessed_at.clone(),
            });
        }
    }
    if !verdict_receipts.is_empty() {
        dedup_receipts(&mut verdict_receipts);
        return (ReceiptTier::Verdict, verdict_receipts);
    }

    let ledger_rows = witness_ledger::all_witnesses_for_project(conn, project).unwrap_or_default();
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for row in &ledger_rows {
        if file_keys.contains(&dream_items::last_two_segments(&row.file)) {
            *counts.entry(row.file.clone()).or_insert(0) += 1;
        }
    }
    if !counts.is_empty() {
        let mut receipts: Vec<Receipt> = counts
            .into_iter()
            .map(|(file, witness_count)| Receipt::Witnessed {
                file,
                witness_count,
            })
            .collect();
        receipts.truncate(MAX_RECEIPTS);
        return (ReceiptTier::Witnessed, receipts);
    }

    (ReceiptTier::Unverified, Vec::new())
}

// ─── storage ───────────────────────────────────────────────────────────

/// Cache a sentinel row for `hash` — `thread = ''`, `evidence_quote = ''`
/// (allowed ONLY here), `receipt_tier = 'unverified'` — so a run that
/// produced zero acceptable threads still converges (module doc).
/// `INSERT OR IGNORE`: append-only-by-convention, never updated.
fn store_sentinel(
    conn: &Connection,
    hash: &str,
    session_id: &str,
    project: &str,
    model: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO dream_threads
            (episode_hash, session_id, project, thread, evidence_quote, files_json, receipt_tier, receipts_json, model)
         VALUES (?1, ?2, ?3, '', '', '[]', 'unverified', '[]', ?4)",
        params![hash, session_id, project, model],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn store_thread(
    conn: &Connection,
    hash: &str,
    session_id: &str,
    project: &str,
    thread: &str,
    evidence_quote: &str,
    files: &[String],
    tier: ReceiptTier,
    receipts: &[Receipt],
    model: &str,
) -> Result<()> {
    let files_json = serde_json::to_string(files)?;
    let receipts_json = serde_json::to_string(receipts)?;
    conn.execute(
        "INSERT OR IGNORE INTO dream_threads
            (episode_hash, session_id, project, thread, evidence_quote, files_json, receipt_tier, receipts_json, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            hash,
            session_id,
            project,
            thread,
            evidence_quote,
            files_json,
            tier.as_str(),
            receipts_json,
            model,
        ],
    )?;
    Ok(())
}

/// Every stored, non-sentinel [`DreamThread`] — the Phase 2 renderer's read
/// API. Fail-open per row: an unparseable `files_json`/`receipts_json`
/// degrades to empty rather than dropping the whole row.
pub fn load_dream_threads(conn: &Connection) -> Result<Vec<DreamThread>> {
    let mut stmt = conn.prepare(
        "SELECT id, episode_hash, session_id, project, thread, evidence_quote, files_json, receipt_tier, receipts_json, model, created_at
         FROM dream_threads
         WHERE thread != ''
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for (
        id,
        episode_hash,
        session_id,
        project,
        thread,
        evidence_quote,
        files_json,
        tier_str,
        receipts_json,
        model,
        created_at,
    ) in rows
    {
        let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();
        let receipts: Vec<Receipt> = serde_json::from_str(&receipts_json).unwrap_or_default();
        let receipt_tier = ReceiptTier::from_str_opt(&tier_str).unwrap_or(ReceiptTier::Unverified);
        out.push(DreamThread {
            id,
            episode_hash,
            session_id,
            project,
            thread,
            evidence_quote,
            files,
            receipt_tier,
            receipts,
            model,
            created_at,
        });
    }
    Ok(out)
}

// ─── orchestration ─────────────────────────────────────────────────────

fn extract_for_episode(
    storage: &Storage,
    actor: &dyn NightActor,
    ep: &EpisodeCandidate,
    stats: &mut ThreadExtractionStats,
) -> Result<()> {
    let allowlist = filtered_files(&ep.files);
    if allowlist.is_empty() {
        return Ok(());
    }

    let tail_chunks = storage.with_connection(|conn| load_tail_chunks(conn, &ep.session_id))?;
    let tail_chunk_ids: Vec<String> = tail_chunks.iter().map(|(id, _)| id.clone()).collect();
    let target_model = primary_thread_model();
    let hash = episode_hash(ep, &tail_chunk_ids, &target_model);

    if storage.with_connection(|conn| already_converged(conn, &hash))? {
        return Ok(());
    }

    let prompt = build_prompt(ep, &tail_chunks, &allowlist);
    let chain = thread_model_candidates();
    let Some((threads, model_used)) =
        verify_reply(actor, &chain, &prompt, &allowlist, storage, Some(&hash))
    else {
        // Actor never produced a usable reply — retry next run, cache nothing.
        return Ok(());
    };

    if threads.is_empty() {
        storage.with_connection(|conn| {
            store_sentinel(conn, &hash, &ep.session_id, &ep.project, &model_used)
        })?;
        stats.sentinels_stored += 1;
        return Ok(());
    }

    storage.with_connection(|conn| {
        for t in &threads {
            let (tier, receipts) = compute_receipts(conn, &ep.project, &t.thread, &t.files);
            store_thread(
                conn,
                &hash,
                &ep.session_id,
                &ep.project,
                &t.thread,
                &t.evidence_quote,
                &t.files,
                tier,
                &receipts,
                &model_used,
            )?;
        }
        Ok(())
    })?;
    stats.threads_stored += threads.len();
    Ok(())
}

fn record_run_meta(storage: &Storage, stats: &ThreadExtractionStats) {
    let _ = storage.set_meta(META_LAST_RUN_AT, &chrono::Utc::now().to_rfc3339());
    let converged = stats.threads_stored + stats.sentinels_stored == 0;
    let _ = storage.set_meta(META_LAST_CONVERGED, if converged { "1" } else { "0" });
}

/// Run one night-pass thread-extraction cycle with a budget sized from the
/// configured effort tier. See [`run_thread_extraction_with_budget`].
pub fn run_thread_extraction(storage: &Storage) -> ThreadExtractionStats {
    let budget = crate::dream::policy::Budget::for_tier(crate::dream::policy::effort_tier());
    run_thread_extraction_with_budget(storage, &budget)
}

/// Run one night-pass thread-extraction cycle. Fail-open at every level —
/// per-episode errors are logged and counted, never propagated; the whole
/// pass never panics and never returns an `Err` (matches the daemon's
/// "never let one iteration wedge the loop" convention documented on
/// `dream_cadence::tick`).
///
/// `budget` is the pass's hard invocation cap (locked decision 8). It is
/// shared with every other producer in the same pass, so the cap is a
/// per-pass total and not a per-producer one. Episodes are consumed
/// newest-first (that is `load_candidate_episodes`'s own order); once the
/// budget is gone the remainder is counted as queued and left untouched —
/// nothing is cached for them, so the next pass retries them.
pub fn run_thread_extraction_with_budget(
    storage: &Storage,
    budget: &crate::dream::policy::Budget,
) -> ThreadExtractionStats {
    let mut stats = ThreadExtractionStats::default();
    if threads_disabled() {
        stats.skipped = true;
        return stats;
    }

    let cap = candidate_cap();
    let episodes = match storage.with_connection(|conn| load_candidate_episodes(conn, cap)) {
        Ok(e) => e,
        Err(error) => {
            tracing::warn!(%error, "dream-thread candidate load failed (non-fatal)");
            return stats;
        }
    };
    stats.candidates = episodes.len();

    let process_actor = ProcessActor;
    let actor = crate::dream::policy::BudgetedActor::new(&process_actor, budget);
    for ep in &episodes {
        if budget.exhausted() {
            budget.note_queued();
            stats.budget_queued += 1;
            continue;
        }
        if let Err(error) = extract_for_episode(storage, &actor, ep, &mut stats) {
            tracing::warn!(
                %error,
                session = %ep.session_id,
                "dream-thread extraction failed for one episode (non-fatal, continuing)"
            );
            stats.errors += 1;
        }
    }

    record_run_meta(storage, &stats);
    tracing::info!(
        candidates = stats.candidates,
        threads_stored = stats.threads_stored,
        sentinels_stored = stats.sentinels_stored,
        errors = stats.errors,
        budget_queued = stats.budget_queued,
        budget_used = budget.used(),
        budget_cap = budget.cap(),
        "dream-thread extraction pass complete"
    );
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::WitnessLedgerRow;
    use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

    /// Serializes tests that touch process-global env vars this module
    /// reads (`CSR_DREAM_THREADS`, `CSR_DREAM_THREAD_MODEL`,
    /// `CSR_NIGHT_ACTOR_CMD`, `CSR_NO_AI_NARRATIVES`, `CSR_NO_DREAMING`,
    /// `CSR_DREAM_THREADS_CAP`) — same idiom as
    /// `daemon::dream_cadence::env_test_guard`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn clear_env() {
        std::env::remove_var("CSR_DREAM_THREADS");
        std::env::remove_var("CSR_DREAM_THREAD_MODEL");
        std::env::remove_var("CSR_NIGHT_ACTOR_CMD");
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
        std::env::remove_var("CSR_NO_DREAMING");
        std::env::remove_var("CSR_DREAM_THREADS_CAP");
    }

    fn open() -> Storage {
        Storage::open_memory().unwrap()
    }

    fn insert_episode(storage: &Storage, json: &str, timestamp: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
                    params![uuid::Uuid::new_v4().to_string(), json, timestamp],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn insert_chunk(storage: &Storage, id: &str, conversation_id: &str, content: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES (?1, ?2, 'proj', '2026-01-01T00:00:00Z', ?3, 1)",
                    params![id, conversation_id, content],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_witness_with_verdict(
        storage: &Storage,
        project: &str,
        file: &str,
        symbol: Option<&str>,
        at_oid: &str,
        stamp: &str,
        verdict: VerdictKind,
        receipt_oid: &str,
    ) {
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: project.into(),
                        file: file.into(),
                        symbol: symbol.map(|s| s.to_string()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: stamp.into(),
                        tier: "committed".into(),
                        at_oid: Some(at_oid.into()),
                        source_kind: "backfill".into(),
                        source_id: Some(at_oid.into()),
                    },
                )?;
                let witness_id: i64 = conn.query_row(
                    "SELECT id FROM witness_ledger WHERE project = ?1 AND file = ?2 AND stamp = ?3",
                    params![project, file, stamp],
                    |r| r.get(0),
                )?;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id,
                        verdict,
                        successor_witness_id: None,
                        receipt_oid: Some(receipt_oid.into()),
                        observed_head_oid: receipt_oid.into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn episode_json(session_id: &str, outcome: &str, files: &[&str]) -> String {
        serde_json::json!({
            "schema": "v2",
            "session_id": session_id,
            "project": "proj",
            "timestamp": "2026-01-01T00:00:00Z",
            "request": "fix the thing",
            "outcome": outcome,
            "completed": "partly fixed the thing",
            "files_modified": files,
            "investigated": [],
        })
        .to_string()
    }

    // ---- kill switches --------------------------------------------------

    #[test]
    fn threads_enabled_requires_exact_1() {
        let _g = env_guard();
        clear_env();
        assert!(!threads_enabled());
        std::env::set_var("CSR_DREAM_THREADS", "true");
        assert!(!threads_enabled(), "only the literal '1' turns this on");
        std::env::set_var("CSR_DREAM_THREADS", "1");
        assert!(threads_enabled());
        clear_env();
    }

    #[test]
    fn kill_switch_off_skips_without_invocation() {
        let _g = env_guard();
        clear_env(); // CSR_DREAM_THREADS unset -> disabled
        let storage = open();
        insert_episode(
            &storage,
            &episode_json("sess-1", "failed", &["/repo/src/a.rs"]),
            "2026-01-01T00:00:00Z",
        );

        let invoked = std::cell::Cell::new(0u32);
        let actor = |_model: Option<&str>, _prompt: &str| {
            invoked.set(invoked.get() + 1);
            ActorAttempt::Failed("must not be called".into())
        };
        // Directly assert the top-level gate rather than routing a closure
        // through run_thread_extraction (which always uses ProcessActor).
        assert!(threads_disabled());
        let stats = run_thread_extraction(&storage);
        assert!(stats.skipped);
        assert_eq!(stats.threads_stored, 0);
        let _ = actor.invoke(None, ""); // never reached in the real pass
        clear_env();
    }

    // ---- quote gate -------------------------------------------------------

    #[test]
    fn quote_gate_rejects_non_verbatim_and_retries() {
        let _g = env_guard();
        clear_env();
        let storage = open();
        let prompt =
            "RECORD: the quick brown fox jumps over the lazy dog. FILES: [\"/repo/src/a.rs\"]";
        let calls = std::cell::Cell::new(0u32);
        let actor = |_model: Option<&str>, p: &str| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                // First reply's quote is NOT verbatim (paraphrased).
                ActorAttempt::Parsed(ParsedNarrative {
                    text: r#"[{"thread":"finish the fox jump","evidence_quote":"the fast brown fox jumped","files":["/repo/src/a.rs"]}]"#.to_string(),
                    model: "sonnet-5".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                })
            } else {
                assert!(
                    p.contains("CORRECTION"),
                    "retry prompt must carry the correction line"
                );
                // Retry's quote IS verbatim.
                ActorAttempt::Parsed(ParsedNarrative {
                    text: r#"[{"thread":"finish the fox jump","evidence_quote":"the quick brown fox jumps over the lazy dog","files":["/repo/src/a.rs"]}]"#.to_string(),
                    model: "sonnet-5".into(),
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                })
            }
        };

        let allowlist = vec!["/repo/src/a.rs".to_string()];
        let chain = vec![Some("sonnet-5".to_string())];
        let result = verify_reply(&actor, &chain, prompt, &allowlist, &storage, None);
        let (threads, _model) = result.expect("actor produced a reply");
        assert_eq!(
            threads.len(),
            1,
            "the retried, verbatim thread must survive: {threads:?}"
        );
        assert_eq!(calls.get(), 2, "exactly one retry must have fired");
    }

    #[test]
    fn quote_gate_drops_thread_still_failing_after_retry() {
        let _g = env_guard();
        clear_env();
        let storage = open();
        let prompt = "RECORD: alpha beta gamma. FILES: [\"/repo/src/a.rs\"]";
        let actor = |_model: Option<&str>, _p: &str| {
            // Always paraphrased — never verbatim, even on retry.
            ActorAttempt::Parsed(ParsedNarrative {
                text: r#"[{"thread":"finish alpha","evidence_quote":"alpha, beta, and gamma","files":["/repo/src/a.rs"]}]"#.to_string(),
                model: "sonnet-5".into(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let allowlist = vec!["/repo/src/a.rs".to_string()];
        let chain = vec![Some("sonnet-5".to_string())];
        let (threads, _model) =
            verify_reply(&actor, &chain, prompt, &allowlist, &storage, None).unwrap();
        assert!(
            threads.is_empty(),
            "a quote that never becomes verbatim must be dropped: {threads:?}"
        );
    }

    // ---- effort tier + budget cap (Journal v4 P5) ---------------------------

    #[test]
    fn candidate_cap_follows_the_effort_tier_unless_explicitly_overridden() {
        use crate::dream::policy::EffortTier;
        assert_eq!(candidate_cap_for(None, EffortTier::Less), 8);
        assert_eq!(candidate_cap_for(None, EffortTier::Balanced), 20);
        assert_eq!(candidate_cap_for(None, EffortTier::Max), 40);
        assert_eq!(
            candidate_cap_for(Some("5"), EffortTier::Max),
            5,
            "an explicit cap wins over the tier"
        );
        assert_eq!(
            candidate_cap_for(Some("0"), EffortTier::Less),
            8,
            "a zero cap falls back to the tier, not to no work at all"
        );
        assert_eq!(
            candidate_cap_for(Some("9999"), EffortTier::Max),
            MAX_CANDIDATE_CAP,
            "nothing may exceed the absolute ceiling"
        );
    }

    #[test]
    fn the_model_chain_takes_the_tier_model_unless_the_env_overrides_it() {
        use crate::dream::policy::EffortTier;
        let _g = env_guard();
        clear_env();
        assert_eq!(
            thread_model_candidates_for(EffortTier::Balanced),
            vec![Some(DEFAULT_THREAD_MODEL.to_string()), None],
            "the default tier must reproduce the pre-tier chain exactly"
        );
        assert_eq!(
            thread_model_candidates_for(EffortTier::Less)[0],
            Some(EffortTier::Less.model().to_string())
        );
        assert_eq!(
            thread_model_candidates_for(EffortTier::Max)[0],
            Some(EffortTier::Max.model().to_string())
        );
        std::env::set_var("CSR_DREAM_THREAD_MODEL", "my-model");
        let overridden = thread_model_candidates_for(EffortTier::Max);
        assert_eq!(
            overridden[0],
            Some("my-model".to_string()),
            "an explicit model choice is never overridden by a tier"
        );
        clear_env();
    }

    #[test]
    fn the_pass_budget_holds_across_the_verifier_retry() {
        use crate::dream::policy::{Budget, BudgetedActor};
        let _g = env_guard();
        clear_env();
        let storage = open();
        let prompt = "RECORD: alpha beta gamma. FILES: [\"/repo/src/a.rs\"]";
        let calls = std::cell::Cell::new(0_usize);
        // Always paraphrased, so the verifier ALWAYS wants its one retry.
        let inner = |_model: Option<&str>, _p: &str| {
            calls.set(calls.get() + 1);
            ActorAttempt::Parsed(ParsedNarrative {
                text: r#"[{"thread":"finish alpha","evidence_quote":"alpha, beta, and gamma","files":["/repo/src/a.rs"]}]"#.to_string(),
                model: "sonnet-5".into(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let budget = Budget::new(1);
        let actor = BudgetedActor::new(&inner, &budget);
        let allowlist = vec!["/repo/src/a.rs".to_string()];
        let chain = vec![Some("sonnet-5".to_string())];
        let (threads, _model) =
            verify_reply(&actor, &chain, prompt, &allowlist, &storage, None).unwrap();
        assert_eq!(
            calls.get(),
            1,
            "the retry must be refused by the budget, not merely discouraged"
        );
        assert_eq!(budget.used(), 1);
        assert!(budget.exhausted());
        assert!(
            threads.is_empty(),
            "an unverifiable quote is still dropped, never softened: {threads:?}"
        );
    }

    #[test]
    fn an_exhausted_budget_produces_no_reply_at_all_so_nothing_is_cached() {
        use crate::dream::policy::{Budget, BudgetedActor};
        let _g = env_guard();
        clear_env();
        let storage = open();
        let calls = std::cell::Cell::new(0_usize);
        let inner = |_m: Option<&str>, _p: &str| {
            calls.set(calls.get() + 1);
            ActorAttempt::Parsed(ParsedNarrative {
                text: "[]".to_string(),
                model: "sonnet-5".into(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let budget = Budget::new(0);
        let actor = BudgetedActor::new(&inner, &budget);
        let chain = vec![Some("sonnet-5".to_string())];
        assert!(
            verify_reply(&actor, &chain, "RECORD: x", &[], &storage, None).is_none(),
            "no reply means the episode is retried next pass, not cached as empty"
        );
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn a_disabled_pass_reports_skipped_and_spends_nothing() {
        use crate::dream::policy::Budget;
        let _g = env_guard();
        clear_env(); // CSR_DREAM_THREADS unset — the opt-in gate is off.
        let storage = open();
        let budget = Budget::new(25);
        let stats = run_thread_extraction_with_budget(&storage, &budget);
        assert!(stats.skipped);
        assert_eq!(budget.used(), 0);
        assert_eq!(stats.budget_queued, 0);
    }

    // ---- file allowlist -----------------------------------------------------

    #[test]
    fn file_allowlist_rejects_foreign_path() {
        let _g = env_guard();
        clear_env();
        let storage = open();
        let prompt = "RECORD: fix the parser bug. FILES: [\"/repo/src/a.rs\"]";
        let actor = |_model: Option<&str>, _p: &str| {
            ActorAttempt::Parsed(ParsedNarrative {
                text: r#"[{"thread":"fix parser","evidence_quote":"fix the parser bug","files":["/repo/src/OUTSIDE.rs"]}]"#.to_string(),
                model: "sonnet-5".into(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let allowlist = vec!["/repo/src/a.rs".to_string()];
        let chain = vec![Some("sonnet-5".to_string())];
        let (threads, _model) =
            verify_reply(&actor, &chain, prompt, &allowlist, &storage, None).unwrap();
        assert!(
            threads.is_empty(),
            "a thread naming a file outside the allowlist must be dropped: {threads:?}"
        );
    }

    // ---- tier assignment ----------------------------------------------------

    #[test]
    fn tier_assignment_verdict_beats_witnessed_beats_unverified() {
        let _g = env_guard();
        let storage = open();

        // Verdict tier: a witness_verdicts row matches by symbol token.
        insert_witness_with_verdict(
            &storage,
            "proj",
            "/repo/src/parser.rs",
            Some("parse_thing"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );
        let (tier, receipts) = storage
            .with_connection(|conn| {
                Ok(compute_receipts(
                    conn,
                    "proj",
                    "fix `parse_thing` bug",
                    &["/repo/src/other.rs".to_string()],
                ))
            })
            .unwrap();
        assert_eq!(tier, ReceiptTier::Verdict);
        assert!(!receipts.is_empty());

        // Witnessed tier: ledger row matches by file, no verdict for that file.
        storage
            .with_connection(|conn| {
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        symbol: None,
                        span_start: None,
                        span_end: None,
                        stamp: "b3:2".into(),
                        tier: "committed".into(),
                        at_oid: Some("bbb".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("bbb".into()),
                    },
                )
            })
            .unwrap();
        let (tier, receipts) = storage
            .with_connection(|conn| {
                Ok(compute_receipts(
                    conn,
                    "proj",
                    "no code tokens here",
                    &["/repo/src/lib.rs".to_string()],
                ))
            })
            .unwrap();
        assert_eq!(tier, ReceiptTier::Witnessed);
        assert!(!receipts.is_empty());

        // Unverified: nothing matches.
        let (tier, receipts) = storage
            .with_connection(|conn| {
                Ok(compute_receipts(
                    conn,
                    "proj",
                    "no code tokens here",
                    &["/repo/src/never_seen.rs".to_string()],
                ))
            })
            .unwrap();
        assert_eq!(tier, ReceiptTier::Unverified);
        assert!(receipts.is_empty());
    }

    // ---- convergence --------------------------------------------------------

    #[test]
    fn convergence_second_run_zero_invocations_zero_usage_rows() {
        let _g = env_guard();
        clear_env();
        std::env::set_var("CSR_DREAM_THREADS", "1");

        let storage = open();
        insert_episode(
            &storage,
            &episode_json("sess-conv", "failed", &["/repo/src/a.rs"]),
            "2026-01-01T00:00:00Z",
        );
        insert_chunk(
            &storage,
            "chunk-1",
            "sess-conv",
            "some transcript content here",
        );

        let invocations = std::cell::Cell::new(0u32);
        let actor = |_model: Option<&str>, _p: &str| {
            invocations.set(invocations.get() + 1);
            ActorAttempt::Parsed(ParsedNarrative {
                text: "[]".to_string(),
                model: "sonnet-5".into(),
                input_tokens: 3,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };

        let episodes = storage
            .with_connection(|conn| load_candidate_episodes(conn, 40))
            .unwrap();
        assert_eq!(episodes.len(), 1);
        let mut stats = ThreadExtractionStats::default();
        extract_for_episode(&storage, &actor, &episodes[0], &mut stats).unwrap();
        assert_eq!(
            invocations.get(),
            1,
            "first run must invoke the actor exactly once"
        );
        assert_eq!(
            stats.sentinels_stored, 1,
            "an empty reply converges via a sentinel row"
        );

        let usage_count_1: i64 = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM narrative_usage", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(usage_count_1, 1);

        // Second run over the SAME corpus: already converged, zero invocations.
        let mut stats2 = ThreadExtractionStats::default();
        extract_for_episode(&storage, &actor, &episodes[0], &mut stats2).unwrap();
        assert_eq!(
            invocations.get(),
            1,
            "convergence: second run must add zero invocations"
        );
        assert_eq!(stats2.sentinels_stored, 0);
        assert_eq!(stats2.threads_stored, 0);

        let usage_count_2: i64 = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM narrative_usage", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(
            usage_count_2, usage_count_1,
            "convergence: second run must add zero usage rows"
        );
        clear_env();
    }

    // ---- sentinel caching for empty replies ----------------------------------

    #[test]
    fn sentinel_caching_for_empty_replies() {
        let _g = env_guard();
        clear_env();
        let storage = open();
        insert_episode(
            &storage,
            &episode_json("sess-empty", "partial", &["/repo/src/a.rs"]),
            "2026-01-01T00:00:00Z",
        );
        let actor = |_model: Option<&str>, _p: &str| {
            ActorAttempt::Parsed(ParsedNarrative {
                text: "[]".to_string(),
                model: "sonnet-5".into(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let episodes = storage
            .with_connection(|conn| load_candidate_episodes(conn, 40))
            .unwrap();
        let mut stats = ThreadExtractionStats::default();
        extract_for_episode(&storage, &actor, &episodes[0], &mut stats).unwrap();
        assert_eq!(stats.sentinels_stored, 1);

        let sentinel_count: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM dream_threads WHERE thread = '' AND receipt_tier = 'unverified'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(sentinel_count, 1);

        // load_dream_threads must never surface the sentinel.
        let loaded = storage.with_connection(load_dream_threads).unwrap();
        assert!(loaded.is_empty());
    }

    // ---- kill-switch off = no invocation (via extract_for_episode is N/A; test the gate directly) ----

    #[test]
    fn run_thread_extraction_disabled_never_touches_storage_candidates() {
        let _g = env_guard();
        clear_env();
        let storage = open();
        insert_episode(
            &storage,
            &episode_json("sess-x", "failed", &["/repo/src/a.rs"]),
            "2026-01-01T00:00:00Z",
        );
        let stats = run_thread_extraction(&storage); // CSR_DREAM_THREADS unset
        assert!(stats.skipped);
        assert_eq!(stats.candidates, 0);
        let total: i64 = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM dream_threads", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(total, 0);
    }

    // ---- malformed JSON -> sentinel + usage row ------------------------------

    #[test]
    fn malformed_json_yields_sentinel_and_usage_row() {
        let _g = env_guard();
        clear_env();
        let storage = open();
        insert_episode(
            &storage,
            &episode_json("sess-bad", "failed", &["/repo/src/a.rs"]),
            "2026-01-01T00:00:00Z",
        );
        let actor = |_model: Option<&str>, _p: &str| {
            ActorAttempt::Parsed(ParsedNarrative {
                text: "not json at all, sorry".to_string(),
                model: "sonnet-5".into(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let episodes = storage
            .with_connection(|conn| load_candidate_episodes(conn, 40))
            .unwrap();
        let mut stats = ThreadExtractionStats::default();
        extract_for_episode(&storage, &actor, &episodes[0], &mut stats).unwrap();
        assert_eq!(
            stats.sentinels_stored, 1,
            "malformed JSON must converge to a sentinel"
        );

        let usage_count: i64 = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM narrative_usage", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(
            usage_count, 1,
            "the attempt that produced the malformed reply must still be recorded"
        );
    }

    // ---- fence-stripping ------------------------------------------------------

    #[test]
    fn fence_stripping_parses_fenced_json() {
        let text = "```json\n[{\"thread\":\"t\",\"evidence_quote\":\"q\",\"files\":[]}]\n```";
        let threads = parse_threads(text);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread, "t");
    }

    // ---- prompt cap -----------------------------------------------------------

    #[test]
    fn prompt_is_capped_at_8kb() {
        let ep = EpisodeCandidate {
            session_id: "sess".into(),
            project: "proj".into(),
            request: Some("x".repeat(1000)),
            completed: Some("y".repeat(1000)),
            outcome: "failed".into(),
            files: vec!["/repo/a.rs".to_string()],
        };
        let huge_tail = vec![("c1".to_string(), "z".repeat(20_000))];
        let prompt = build_prompt(&ep, &huge_tail, &["/repo/a.rs".to_string()]);
        assert!(
            prompt.len() <= PROMPT_CAP_BYTES,
            "prompt must be capped: {} bytes",
            prompt.len()
        );
    }

    // ---- actor template substitution (string-building only, never executes) --

    #[test]
    fn actor_template_substitution_builds_command_string_only() {
        let prompt_file = std::path::Path::new("/tmp/x/prompt-abc.txt");
        let out_file = std::path::Path::new("/tmp/x/out-abc.txt");
        let cmd = substitute_actor_template(
            "codex exec -m {model} {prompt_file} > {out_file}",
            "sonnet-5",
            prompt_file,
            out_file,
        );
        assert_eq!(
            cmd,
            "codex exec -m sonnet-5 /tmp/x/prompt-abc.txt > /tmp/x/out-abc.txt"
        );
    }

    // ---- model chain ------------------------------------------------------------

    #[test]
    fn thread_model_candidates_chain_env_default_none() {
        let _g = env_guard();
        clear_env();
        let chain = thread_model_candidates();
        assert_eq!(chain, vec![Some(DEFAULT_THREAD_MODEL.to_string()), None]);
        assert_eq!(primary_thread_model(), DEFAULT_THREAD_MODEL);

        std::env::set_var("CSR_DREAM_THREAD_MODEL", "opus");
        let chain = thread_model_candidates();
        assert_eq!(chain[0], Some("opus".to_string()));
        assert_eq!(chain[1], Some(DEFAULT_THREAD_MODEL.to_string()));
        assert_eq!(chain[2], None);
        assert_eq!(primary_thread_model(), "opus");
        clear_env();
    }

    // ---- episode candidate filter (outcome + files gate) -----------------------

    #[test]
    fn candidate_episodes_require_partial_or_failed_and_files() {
        let storage = open();
        insert_episode(
            &storage,
            &episode_json("sess-ok", "failed", &["/repo/a.rs"]),
            "2026-01-01T00:00:00Z",
        );
        insert_episode(
            &storage,
            &episode_json("sess-success", "success", &["/repo/b.rs"]),
            "2026-01-01T00:00:00Z",
        );
        insert_episode(
            &storage,
            &episode_json("sess-nofiles", "partial", &[]),
            "2026-01-01T00:00:00Z",
        );

        let episodes = storage
            .with_connection(|conn| load_candidate_episodes(conn, 40))
            .unwrap();
        assert_eq!(
            episodes.len(),
            1,
            "{:?}",
            episodes.iter().map(|e| &e.session_id).collect::<Vec<_>>()
        );
        assert_eq!(episodes[0].session_id, "sess-ok");
    }

    // ---- files_json/scratchpad filtering ---------------------------------------

    #[test]
    fn scratchpad_and_memory_paths_filtered_from_allowlist() {
        let files = vec![
            "/repo/src/a.rs".to_string(),
            "/tmp/scratchpad/notes.md".to_string(),
            "/home/user/memory/MEMORY.md".to_string(),
        ];
        let filtered = filtered_files(&files);
        assert_eq!(filtered, vec!["/repo/src/a.rs".to_string()]);
    }
}
