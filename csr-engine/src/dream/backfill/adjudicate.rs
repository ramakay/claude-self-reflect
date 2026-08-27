//! Dream backfill — Stage 4 adjudication (`.plans/dream-backfill-design.md`
//! §3 "Stage 4 — adjudicate", with the D5/D6/D9/D11 round-2 deltas from §8
//! folded in, since that section overrides §3-6 wherever they conflict).
//!
//! One bare JSON-constrained `claude -p` call per [`super::rank`]-queued,
//! `tier = 'unverified'` `dream_relations` candidate — the only stage in the
//! whole backfill pipeline that spends an LLM call. Its output is NEVER
//! trusted directly: [`super::verify`] (Stage 5) re-checks every quote and
//! OID before anything is promoted, and this module's own deterministic leg
//! conjunction (D9) is what decides the relation, not the model.
//!
//! # Unprimed judge (D5)
//!
//! The prompt built by [`build_prompt`] carries ONLY neutral facts: the two
//! episodes' own request/completed/next_steps/blockers/files text plus a
//! couple of session transcript excerpts, and a topic label extracted from
//! `topic_key`. It NEVER states the generator's hypothesis relation
//! (`replaced_by`/`extended_by`), the generator's identity
//! (`ledger`/`era`/`relapse`), or any ledger verdict word
//! (`superseded`/`obsolete`/`witness`/`verdict`) — telling the model what a
//! deterministic generator already believes would defeat the whole point of
//! an independent check. The model returns three binary leg judgments
//! (`quote_a_attests_a`, `quote_b_attests_b`, `incompatible`) plus
//! `extended`, `quote_a`, `quote_b`, `oids` — [`decide_relation`] conjoins
//! them into a relation (or `None` == UNRELATED) entirely in Rust (D9); the
//! model is never asked for a relation label directly.
//!
//! # Budget + circuit breaker
//!
//! [`run_adjudication`] pulls at most `budget_calls` candidates off the
//! queue (`ORDER BY gate_score DESC` — the highest-value candidates are
//! adjudicated first, matching the design's own precision-over-coverage
//! bias). Whatever the queue holds beyond that limit is simply never
//! fetched — it stays `status = 'queued'`/`tier = 'unverified'` in
//! `dream_relations`, which IS the resumable backlog; [`queue_depth`] reads
//! it back, and [`persist_backfill_state`] snapshots it (plus the run's own
//! outcome counts) into `backfill_state` under `stage = 'adjudicate'` so a
//! future `status` integration can read it without re-scanning.
//!
//! # Kill switches
//!
//! [`adjudicate_disabled`] is true under `CSR_NO_AI_NARRATIVES` (design: "the
//! backfill stops before stage 4 and says so" — [`AdjudicateStats::disabled`]
//! is that "says so") or `CSR_NO_DREAMING` (house rule, same kill switch
//! every other backfill stage respects).
//!
//! # Canaries (D5)
//!
//! The INSUFFICIENT_EVIDENCE≈0 tripwire from the first draft is DROPPED (D5:
//! "it alarms on the expected value" — that relation value no longer exists
//! at all under the D9 leg decomposition). The replacement canary is
//! [`canary_suspect`]: the UNRELATED rate across a run must land inside
//! [`CANARY_UNRELATED_MIN`]..=[`CANARY_UNRELATED_MAX`] (10%-40%); outside
//! that band sets [`AdjudicateStats::adjudicator_suspect`], a WARN signal
//! only — the design is explicit that this never aborts a run.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;

use crate::narrative::{self, AttemptOutcome, ParsedNarrative};
use crate::storage::{NarrativeUsageRow, Storage};

use super::pairs::Relation;
use super::verify;

/// `narrative_usage.call_site` for every adjudication attempt.
pub const ADJUDICATE_CALL_SITE: &str = "dream_backfill_adjudicate";

/// Default `--budget-calls` (design §4 / §3 Stage 4) — a future CLI stage's
/// default. Not wired to any flag here; this stage only accepts the number
/// as a parameter.
pub const DEFAULT_BUDGET_CALLS: usize = 150;

/// `backfill_state.stage` value this module owns (crash-safety checkpoint
/// table, `storage::migrations::run`'s doc comment: "cursor is an opaque
/// resume marker private to whichever stage owns it").
pub const BACKFILL_STATE_STAGE: &str = "adjudicate";

/// D5 canary band: a healthy adjudicator disagrees with roughly 10%-40% of
/// the candidates a deterministic generator hands it. Outside that band is
/// a WARN (`adjudicator_suspect`), never an abort.
pub const CANARY_UNRELATED_MIN: f64 = 0.10;
pub const CANARY_UNRELATED_MAX: f64 = 0.40;

const ADJUDICATE_TIMEOUT_SECS: u64 = 120;
/// Backstop cap on rows pulled per run now that budget_calls only limits
/// LLM (era) attempts, not deterministic promotions.
const QUEUE_LOAD_CAP: usize = 10_000;
const ADJUDICATE_SYSTEM_PROMPT: &str = "Output only raw JSON. No prose.";
const TAIL_CHUNK_COUNT: i64 = 1;
/// Symbol-bearing chunks fetched per side for `symbol:` topics — more
/// generous than the blind tail because these are the passages the quote
/// legs must be attested from (see [`load_session_chunks_for_topic`]).
const SYMBOL_CHUNK_COUNT: i64 = 3;
const TAIL_CHUNK_CHAR_CAP: usize = 900;
const PROMPT_CAP_BYTES: usize = 6 * 1024;

/// Whole-stage kill switch (see module doc's "Kill switches" section).
pub fn adjudicate_disabled() -> bool {
    narrative::narratives_disabled() || crate::daemon::dream_cadence::dreaming_disabled()
}

// ---------------------------------------------------------------------
// Queue + episode loading (own narrow row shapes — same convention
// `pairs`/`unfinished` already established: each consumer reads only the
// columns it needs rather than sharing one wide loader).
// ---------------------------------------------------------------------

/// One `dream_relations` row pulled off the adjudication queue. The
/// generator's HYPOTHESIS relation is deliberately NOT loaded here — D9/D5
/// make the LLM+Rust leg-conjunction the sole authority on the final
/// relation, and the unprimed prompt must never see the hypothesis anyway.
#[derive(Debug, Clone)]
pub(super) struct QueuedRelation {
    pub id: i64,
    pub project: String,
    pub ep_a: String,
    pub ep_b: String,
    pub topic_key: String,
    pub generator: String,
    /// The generator's hypothesis relation — consumed ONLY by the
    /// deterministic (non-era) promotion path, where it is itself part of
    /// the machine evidence (a relapse IS extended_by, a supersession IS
    /// replaced_by, by construction). Era candidates never read it; their
    /// relation is decided by the unprimed judge (D5/D9).
    pub relation: String,
    pub load_bearing_oid: Option<String>,
    pub oid_provenance: String,
}

pub(super) fn load_queue(conn: &Connection, limit: usize) -> Result<Vec<QueuedRelation>> {
    let mut stmt = conn.prepare(
        "SELECT id, project, ep_a, ep_b, topic_key, generator, relation, load_bearing_oid, oid_provenance
         FROM dream_relations
         WHERE status = 'queued' AND tier = 'unverified'
         ORDER BY gate_score DESC, id ASC
         LIMIT ?1",
    )?;
    let limit = limit as i64;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(QueuedRelation {
            id: row.get(0)?,
            project: row.get(1)?,
            ep_a: row.get(2)?,
            ep_b: row.get(3)?,
            topic_key: row.get(4)?,
            generator: row.get(5)?,
            relation: row.get(6)?,
            load_bearing_oid: row.get(7)?,
            oid_provenance: row.get(8)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Everything still waiting for adjudication — the resumable backlog the
/// circuit breaker leaves behind (module doc's "Budget + circuit breaker").
pub fn queue_depth(conn: &Connection) -> Result<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM dream_relations WHERE status = 'queued' AND tier = 'unverified'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as usize)
    .map_err(Into::into)
}

/// Neutral episode facts fed to the adjudication prompt and checked against
/// at verify time — the same text in both places, so verify never judges a
/// quote against text the model never saw.
#[derive(Debug, Clone, Default)]
pub(super) struct EpisodeFacts {
    #[allow(dead_code)]
    // kept for shape-fidelity / future debug use; not read today, same convention as `pairs::AnchorRow::node_kind`
    pub episode_id: String,
    pub session_id: String,
    pub ts: String,
    pub request: String,
    pub completed: String,
    pub next_steps: Option<String>,
    pub blockers: Option<String>,
    pub files: Vec<String>,
}

pub(super) fn load_episode(conn: &Connection, episode_id: &str) -> Result<Option<EpisodeFacts>> {
    conn.query_row(
        "SELECT episode_id, session_id, ts, request, completed, next_steps, blockers, files_json
         FROM episode_index WHERE episode_id = ?1",
        params![episode_id],
        |row| {
            let files_json: String = row.get(7)?;
            Ok(EpisodeFacts {
                episode_id: row.get(0)?,
                session_id: row.get(1)?,
                ts: row.get(2)?,
                request: row.get(3)?,
                completed: row.get(4)?,
                next_steps: row.get(5)?,
                blockers: row.get(6)?,
                files: serde_json::from_str(&files_json).unwrap_or_default(),
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// The exact text a quote is checked against — request + completed +
/// next_steps + blockers + (P7) the "Files touched:" line, the same fields
/// AND the same line [`render_episode_block`] renders into the prompt. Before
/// P7 this omitted the files line the model actually saw, so a verbatim
/// quote spanning it could never verify — the checked text must equal the
/// prompted text exactly, not a subset of it.
pub(super) fn episode_record_text(facts: &EpisodeFacts) -> String {
    let mut s = String::new();
    s.push_str(&facts.request);
    s.push(' ');
    s.push_str(&facts.completed);
    if let Some(n) = &facts.next_steps {
        s.push(' ');
        s.push_str(n);
    }
    if let Some(b) = &facts.blockers {
        s.push(' ');
        s.push_str(b);
    }
    if !facts.files.is_empty() {
        s.push_str(" Files touched: ");
        s.push_str(&facts.files.join(", "));
    }
    s
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
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

/// D6: "against the source episode record (or its session chunks, fetched
/// by conversation_id)" — the same chunks are threaded through to
/// [`super::verify`] so the quote check re-inspects exactly what the prompt
/// contained, never a freshly (and possibly differently) re-fetched window.
/// Symbol-topic candidates get chunks that actually CONTAIN the symbol
/// (most recent first, then restored to chronological order) instead of a
/// blind session tail: the first live run proved the tail chunk routinely
/// never mentions the symbol the pair is about (the symbol lives in AST
/// anchors, not in the episode's prose), which starves the judge of
/// quotable evidence and turns every honest verdict into UNRELATED. Falls
/// back to the tail window when the symbol never appears in prose either.
pub(super) fn load_session_chunks_for_topic(
    conn: &Connection,
    session_id: &str,
    topic_key: &str,
) -> Result<Vec<String>> {
    if let Some(symbol) = topic_key.strip_prefix("symbol:") {
        if !symbol.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT content FROM chunks
                 WHERE conversation_id LIKE ?1 AND content LIKE ?2
                 ORDER BY rowid DESC LIMIT ?3",
            )?;
            let pattern = format!("%{symbol}%");
            let mut rows: Vec<String> = stmt
                .query_map(params![session_id, pattern, SYMBOL_CHUNK_COUNT], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !rows.is_empty() {
                rows.reverse();
                for c in rows.iter_mut() {
                    *c = truncate_chars(c, TAIL_CHUNK_CHAR_CAP);
                }
                return Ok(rows);
            }
        }
    }
    let mut stmt = conn.prepare(
        "SELECT content FROM chunks WHERE conversation_id LIKE ?1 ORDER BY rowid DESC LIMIT ?2",
    )?;
    let mut rows: Vec<String> = stmt
        .query_map(params![session_id, TAIL_CHUNK_COUNT], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    for c in rows.iter_mut() {
        *c = truncate_chars(c, TAIL_CHUNK_CHAR_CAP);
    }
    Ok(rows)
}

// ---------------------------------------------------------------------
// Prompt (D5 unprimed judge)
// ---------------------------------------------------------------------

const ADJUDICATE_RULES: &str =
    "You are comparing two development episodes about the same topic to determine how the \
later one's approach relates to the earlier one's: does it contradict it, return to it, or \
neither (the episodes are unrelated beyond the shared topic)?\n\
\n\
Respond with ONLY a single raw JSON object -- no markdown fence, no prose before or after -- \
with exactly these fields:\n\
{\n\
  \"quote_a\": a quote you copy character-for-character from Episode A's text that shows what \
approach or belief Episode A held about the topic. Empty string if no such passage exists.\n\
  \"quote_a_attests_a\": true or false. True ONLY if you found such a verbatim passage and it \
genuinely shows Episode A's approach to the topic.\n\
  \"quote_b\": a quote you copy character-for-character from Episode B's text that shows what \
Episode B actually did about the topic. Empty string if no such passage exists.\n\
  \"quote_b_attests_b\": true or false. True ONLY if you found such a verbatim passage and it \
genuinely shows what Episode B did about the topic.\n\
  \"incompatible\": true or false. True ONLY if Episode B's approach genuinely contradicts, \
replaces, or cannot coexist with Episode A's approach -- not merely that both mention the same \
topic.\n\
  \"same_approach\": true or false. True ONLY if Episode B genuinely returns to, re-touches, \
or continues working with the same mechanism or approach Episode A established for the topic \
-- not merely that both mention the same topic. At most one of incompatible and same_approach \
can be true; if neither holds, set both to false.\n\
  \"extended\": true or false. When incompatible is true: does Episode B build on and continue \
Episode A's direction (extend) rather than reverse or replace it outright? Ignored when \
incompatible is false.\n\
  \"oids\": a JSON array of any git commit hashes literally present in the text below that back \
this relationship. Empty array if none.\n\
}\n\
Be conservative: when genuinely uncertain, set incompatible and same_approach to false.\n";

fn topic_label(topic_key: &str) -> String {
    topic_key
        .strip_prefix("symbol:")
        .map(|s| format!("the code symbol `{s}`"))
        .unwrap_or_else(|| "a topic shared by two development sessions".to_string())
}

fn render_episode_block(label: &str, facts: &EpisodeFacts, chunks: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "=== EPISODE {label} (timestamp {}) ===", facts.ts);
    let _ = writeln!(out, "Request: {}", facts.request);
    let _ = writeln!(out, "Completed: {}", facts.completed);
    if let Some(n) = &facts.next_steps {
        let _ = writeln!(out, "Next steps: {n}");
    }
    if let Some(b) = &facts.blockers {
        let _ = writeln!(out, "Blockers: {b}");
    }
    if !facts.files.is_empty() {
        let _ = writeln!(out, "Files touched: {}", facts.files.join(", "));
    }
    for (i, c) in chunks.iter().enumerate() {
        let _ = writeln!(out, "Transcript excerpt {}: {c}", i + 1);
    }
    out
}

pub(super) fn build_prompt(
    topic_key: &str,
    a: &EpisodeFacts,
    a_chunks: &[String],
    b: &EpisodeFacts,
    b_chunks: &[String],
) -> String {
    let topic = topic_label(topic_key);
    let block_a = render_episode_block("A (earlier)", a, a_chunks);
    let block_b = render_episode_block("B (later)", b, b_chunks);
    let prompt = format!("{ADJUDICATE_RULES}\nTopic: {topic}\n\n{block_a}\n{block_b}");
    truncate_bytes(&prompt, PROMPT_CAP_BYTES)
}

// ---------------------------------------------------------------------
// Raw response + D9 leg-conjunction (deterministic — the LLM never states a
// relation directly)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(super) struct RawVerdict {
    pub quote_a_attests_a: bool,
    pub quote_b_attests_b: bool,
    pub incompatible: bool,
    /// Episode B returns to / continues A's approach — the leg a RELAPSE
    /// candidate must win (a relapse is alignment with a retired approach,
    /// the exact opposite of `incompatible`; the first live run proved a
    /// judge asked only about incompatibility archives every relapse pair
    /// as UNRELATED). Mutually exclusive with `incompatible` by prompt
    /// contract; the deterministic side never rewards both at once.
    pub same_approach: bool,
    pub extended: bool,
    pub quote_a: String,
    pub quote_b: String,
    pub oids: Vec<String>,
}

/// Same fence-stripping idiom as `dream::threads::strip_json_fences` /
/// `daemon::ratification::strip_json_fences`.
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

/// `None` only for genuinely unparseable JSON — a well-formed object with
/// missing fields still parses (`#[serde(default)]`), which conservatively
/// decides UNRELATED via [`decide_relation`] rather than being treated as a
/// process failure.
pub(super) fn parse_verdict(text: &str) -> Option<RawVerdict> {
    serde_json::from_str::<RawVerdict>(strip_json_fences(text)).ok()
}

/// D9: the single call's binary judgments, conjoined deterministically in
/// Rust. `None` == UNRELATED (no relation persists); `Some(relation)` ==
/// REPLACED_BY or EXTENDED_BY, still subject to [`super::verify`] before
/// anything is promoted.
///
/// Which leg the candidate must win depends on the generator's hypothesis
/// SHAPE (never shown to the judge — the prompt asks both questions
/// neutrally): a `relapse` candidate claims B unknowingly RETURNED to A's
/// approach, so it must win `same_approach` (and is inherently ExtendedBy —
/// the whole point is continuation of a retired direction); `ledger`/`era`
/// candidates claim B DISPLACED A's approach, so they must win
/// `incompatible`. A verdict with both legs true violates the prompt's
/// mutual-exclusion contract and is treated as no-verdict (conservative).
pub(super) fn decide_relation(v: &RawVerdict, generator: &str) -> Option<Relation> {
    if !(v.quote_a_attests_a && v.quote_b_attests_b) {
        return None;
    }
    if v.incompatible && v.same_approach {
        return None;
    }
    match generator {
        "relapse" => v.same_approach.then_some(Relation::ExtendedBy),
        _ => {
            if v.incompatible {
                Some(if v.extended {
                    Relation::ExtendedBy
                } else {
                    Relation::ReplacedBy
                })
            } else {
                None
            }
        }
    }
}

/// D5: every backfill generator's LLM-adjudicated ceiling is `witnessed` —
/// none of ledger/era/relapse carries a human-verified `verdict`-tier
/// receipt (that requires `csr_resolve`, which this pipeline never writes),
/// so a successful adjudication+verify always promotes to exactly this
/// tier, regardless of which generator produced the candidate. The design's
/// own wording names ledger and era explicitly ("ledger=witnessed;
/// era=witnessed post-verify"); relapse is a judgment call — treated
/// identically since it is, if anything, MORE directly ledger-derived
/// evidence than era's topical clustering.
pub(super) const TIER_CEILING: &str = "witnessed";

// ---------------------------------------------------------------------
// Actor abstraction (mirrors `dream::threads::NightActor`/`ActorAttempt`)
// ---------------------------------------------------------------------

pub(super) enum AdjudicateAttempt {
    Parsed(ParsedNarrative),
    ModelNotFound,
    Failed(String),
}

pub(super) trait Adjudicator {
    fn invoke(&self, model: Option<&str>, prompt: &str) -> AdjudicateAttempt;
}

impl<F> Adjudicator for F
where
    F: Fn(Option<&str>, &str) -> AdjudicateAttempt,
{
    fn invoke(&self, model: Option<&str>, prompt: &str) -> AdjudicateAttempt {
        self(model, prompt)
    }
}

pub(super) struct ProcessAdjudicator;

impl Adjudicator for ProcessAdjudicator {
    fn invoke(&self, model: Option<&str>, prompt: &str) -> AdjudicateAttempt {
        invoke_claude_p(model, prompt)
    }
}

fn write_minimal_mcp_config() -> Result<PathBuf> {
    let config = serde_json::json!({ "mcpServers": {} });
    let dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".claude-self-reflect");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("dream-backfill-adjudicate-mcp.json");
    std::fs::write(&path, serde_json::to_string(&config)?)?;
    Ok(path)
}

/// A single `claude -p` attempt for one model candidate — bare
/// JSON-constrained call site (D11: "no narrative persona"), same manual
/// poll-timeout process idiom as
/// `hooks::session_briefing::invoke_narrative_briefing` /
/// `dream::threads::invoke_claude_p`.
fn invoke_claude_p(model: Option<&str>, prompt: &str) -> AdjudicateAttempt {
    let mcp_config_path = match write_minimal_mcp_config() {
        Ok(p) => p,
        Err(e) => return AdjudicateAttempt::Failed(format!("mcp config: {e}")),
    };
    let mut cmd = std::process::Command::new("claude");
    cmd.arg("-p")
        .arg(prompt)
        .args(model.map(|m| ["--model", m]).into_iter().flatten())
        .arg("--output-format")
        .arg("json")
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg(&mcp_config_path)
        .arg("--system-prompt")
        .arg(ADJUDICATE_SYSTEM_PROMPT)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .env("CSR_DISABLE_RECURSIVE_HOOKS", "1");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return AdjudicateAttempt::Failed(format!("spawn failed: {e}")),
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed().as_secs() >= ADJUDICATE_TIMEOUT_SECS {
                    let _ = child.kill();
                    return AdjudicateAttempt::Failed(format!(
                        "claude -p timed out after {ADJUDICATE_TIMEOUT_SECS}s"
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return AdjudicateAttempt::Failed(format!("wait failed: {e}")),
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return AdjudicateAttempt::Failed(format!("collect output failed: {e}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    match narrative::classify_attempt(output.status.success(), &stdout, &stderr) {
        AttemptOutcome::Parsed(p) => AdjudicateAttempt::Parsed(p),
        AttemptOutcome::ModelNotFound => AdjudicateAttempt::ModelNotFound,
        AttemptOutcome::Failed(msg) => AdjudicateAttempt::Failed(msg),
    }
}

// ---------------------------------------------------------------------
// Model-chain walk + usage accounting (reuses narrative.rs's chain/parsing
// mechanics; no reservation/nightly-ledger machinery — that belongs to
// `dream::policy`'s nightly cadence, out of scope for backfill's own
// `--budget-calls` counter)
// ---------------------------------------------------------------------

struct ChainAttempt {
    model_label: String,
    success: bool,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
}

struct ChainResult {
    text: Option<String>,
    attempts: Vec<ChainAttempt>,
}

fn invoke_chain(actor: &dyn Adjudicator, prompt: &str) -> ChainResult {
    let mut attempts = Vec::new();
    for candidate in narrative::model_candidates() {
        let label = candidate.clone().unwrap_or_else(|| "default".to_string());
        match actor.invoke(candidate.as_deref(), prompt) {
            AdjudicateAttempt::Parsed(p) => {
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
                    attempts,
                };
            }
            AdjudicateAttempt::ModelNotFound => {
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
            AdjudicateAttempt::Failed(msg) => {
                tracing::warn!(model = %label, error = %msg, "dream backfill adjudication attempt failed");
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
        attempts,
    }
}

/// Best-effort accounting — a failed insert must never fail the run (same
/// convention `hooks::session_briefing` follows for its own narrative spend).
fn record_attempts(storage: &Storage, attempts: &[ChainAttempt]) {
    for a in attempts {
        let _ = storage.record_narrative_usage(&NarrativeUsageRow {
            call_site: ADJUDICATE_CALL_SITE.to_string(),
            model: a.model_label.clone(),
            input_tokens: a.input_tokens,
            output_tokens: a.output_tokens,
            cache_read_tokens: a.cache_read_tokens,
            cache_creation_tokens: a.cache_creation_tokens,
            duration_ms: 0,
            success: a.success,
        });
    }
}

// ---------------------------------------------------------------------
// Canary (D5)
// ---------------------------------------------------------------------

/// D5 canary: the UNRELATED rate across a run's decided candidates
/// (actor failures / malformed responses are excluded from the denominator
/// — they carry no adjudication verdict to judge the model's behavior by)
/// must land inside the 10%-40% band. `denom == 0` (nothing was actually
/// decided this run) can never trip the flag — there is no signal to judge.
pub(super) fn canary_suspect(unrelated: usize, related: usize) -> bool {
    let denom = unrelated + related;
    if denom == 0 {
        return false;
    }
    let rate = unrelated as f64 / denom as f64;
    !(CANARY_UNRELATED_MIN..=CANARY_UNRELATED_MAX).contains(&rate)
}

// ---------------------------------------------------------------------
// backfill_state checkpoint (module doc's "Budget + circuit breaker")
// ---------------------------------------------------------------------

fn persist_backfill_state(conn: &Connection, stats: &AdjudicateStats) -> Result<()> {
    let cursor = serde_json::json!({
        "attempted": stats.attempted,
        "related": stats.related,
        "unrelated": stats.unrelated,
        "verify_passed": stats.verify_passed,
        "verify_failed": stats.verify_failed,
        "backlog": stats.backlog_after,
        "unrelated_rate": stats.unrelated_rate,
        "adjudicator_suspect": stats.adjudicator_suspect,
    })
    .to_string();
    conn.execute(
        "INSERT INTO backfill_state (stage, project, cursor, updated_at)
         VALUES (?1, '', ?2, datetime('now'))
         ON CONFLICT(stage, project) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at",
        params![BACKFILL_STATE_STAGE, cursor],
    )?;
    Ok(())
}

/// Read back the last [`persist_backfill_state`] snapshot — a future
/// `status` integration's read side (design §8 D11: "Backlog count
/// surfaced in `status`"). Returns the raw JSON string; parsing is that
/// future caller's concern, not this stage's.
pub fn read_backfill_state(conn: &Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT cursor FROM backfill_state WHERE stage = ?1 AND project = ''",
        params![BACKFILL_STATE_STAGE],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

// ---------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct AdjudicateStats {
    /// True under `CSR_NO_AI_NARRATIVES`/`CSR_NO_DREAMING` — the run never
    /// started (design: "backfill stops before stage 4 and says so").
    pub disabled: bool,
    pub queued_before: usize,
    /// Candidates actually popped off the queue this run (<= `budget_calls`).
    pub attempted: usize,
    /// Defensive: a queued row whose `episode_index` side vanished since
    /// generation (should not happen in practice — `dream_relations` and
    /// `episode_index` are never both refreshed mid-run — but never a panic).
    pub missing_episode: usize,
    /// The actor never produced a usable reply (every model candidate
    /// failed/timed out) — left `queued` for the next run, same "retry, do
    /// not cache" convention `dream::threads` already follows.
    pub actor_no_reply: usize,
    /// The actor replied, but the reply was not parseable JSON at all —
    /// logged to `backfill_discards`, left `queued` for retry.
    pub malformed: usize,
    /// D9 leg conjunction decided UNRELATED — archived, not a discard (an
    /// honest "no relation" is the expected majority outcome, not a
    /// verification failure).
    pub unrelated: usize,
    /// D9 leg conjunction decided REPLACED_BY/EXTENDED_BY — handed to
    /// [`super::verify`].
    pub related: usize,
    pub verify_passed: usize,
    pub verify_failed: usize,
    /// Ledger/relapse candidates promoted on machine evidence alone (round-5
    /// amendment — see `verify::verify_and_apply_deterministic`): zero LLM
    /// spend, excluded from the canary denominators.
    pub deterministic_promoted: usize,
    /// Ledger/relapse candidates whose machine evidence failed re-check
    /// (ts_ordering / load_bearing_oid_unresolvable) — discarded.
    pub deterministic_discarded: usize,
    /// Still `queued`/`unverified` after this run — the resumable backlog.
    pub backlog_after: usize,
    pub unrelated_rate: f64,
    pub adjudicator_suspect: bool,
}

/// Production entry point — real `claude -p` calls via [`ProcessAdjudicator`].
pub fn run_adjudication(storage: &Storage, budget_calls: usize) -> Result<AdjudicateStats> {
    run_adjudication_with(&ProcessAdjudicator, storage, budget_calls)
}

/// Core adjudication loop, actor-injectable for tests. Never holds the
/// storage connection across an actor invocation — each DB step is its own
/// short [`Storage::with_connection`] call, matching `dream::threads`'s own
/// convention (holding the lock across a subprocess call that can take up
/// to [`ADJUDICATE_TIMEOUT_SECS`] would starve every other MCP request).
pub(super) fn run_adjudication_with(
    actor: &dyn Adjudicator,
    storage: &Storage,
    budget_calls: usize,
) -> Result<AdjudicateStats> {
    let mut stats = AdjudicateStats::default();
    if adjudicate_disabled() {
        stats.disabled = true;
        return Ok(stats);
    }

    stats.queued_before = storage.with_connection(queue_depth)?;
    // Deterministic (non-era) candidates cost no LLM call, so the load is
    // NOT capped by budget_calls — only era candidates consume the budget,
    // enforced inside the loop. QUEUE_LOAD_CAP is a runaway backstop.
    let queue = storage.with_connection(|conn| load_queue(conn, QUEUE_LOAD_CAP))?;

    let mut oid_cache = verify::OidCache::new();

    for candidate in &queue {
        let deterministic = candidate.generator != "era";
        if !deterministic && stats.attempted >= budget_calls {
            continue; // era budget exhausted -- stays queued for next run
        }

        let (a, b) = storage.with_connection(|conn| {
            Ok((
                load_episode(conn, &candidate.ep_a)?,
                load_episode(conn, &candidate.ep_b)?,
            ))
        })?;
        let (Some(a), Some(b)) = (a, b) else {
            stats.missing_episode += 1;
            continue;
        };

        if deterministic {
            let relation = match candidate.relation.as_str() {
                "extended_by" => Relation::ExtendedBy,
                _ => Relation::ReplacedBy,
            };
            let outcome = storage.with_connection(|conn| {
                verify::verify_and_apply_deterministic(
                    conn,
                    candidate,
                    &a,
                    &b,
                    relation,
                    &mut oid_cache,
                )
            })?;
            match outcome {
                verify::VerifyOutcome::Passed => stats.deterministic_promoted += 1,
                verify::VerifyOutcome::Failed(_) => stats.deterministic_discarded += 1,
            }
            continue;
        }
        stats.attempted += 1;

        let (a_chunks, b_chunks) = storage.with_connection(|conn| {
            Ok((
                load_session_chunks_for_topic(conn, &a.session_id, &candidate.topic_key)?,
                load_session_chunks_for_topic(conn, &b.session_id, &candidate.topic_key)?,
            ))
        })?;

        let prompt = build_prompt(&candidate.topic_key, &a, &a_chunks, &b, &b_chunks);
        let result = invoke_chain(actor, &prompt);
        record_attempts(storage, &result.attempts);

        let Some(text) = result.text else {
            stats.actor_no_reply += 1;
            continue;
        };

        let Some(verdict) = parse_verdict(&text) else {
            stats.malformed += 1;
            storage.with_connection(|conn| {
                verify::log_discard(
                    conn,
                    &verify::pair_key(&candidate.project, &candidate.ep_a, &candidate.ep_b),
                    "malformed_response",
                    &text,
                )
            })?;
            continue;
        };

        match decide_relation(&verdict, &candidate.generator) {
            None => {
                stats.unrelated += 1;
                storage.with_connection(|conn| verify::archive_relation(conn, candidate.id))?;
            }
            Some(relation) => {
                stats.related += 1;
                let outcome = storage.with_connection(|conn| {
                    verify::verify_and_apply(
                        conn,
                        candidate,
                        &a,
                        &a_chunks,
                        &b,
                        &b_chunks,
                        relation,
                        &verdict,
                        &mut oid_cache,
                        &text,
                    )
                })?;
                match outcome {
                    verify::VerifyOutcome::Passed => stats.verify_passed += 1,
                    verify::VerifyOutcome::Failed(reason) => {
                        tracing::debug!(
                            reason,
                            ep_a = %candidate.ep_a,
                            ep_b = %candidate.ep_b,
                            "dream backfill candidate failed stage-5 verification"
                        );
                        stats.verify_failed += 1;
                    }
                }
            }
        }
    }

    stats.backlog_after = storage.with_connection(queue_depth)?;
    stats.unrelated_rate = {
        let denom = stats.unrelated + stats.related;
        if denom == 0 {
            0.0
        } else {
            stats.unrelated as f64 / denom as f64
        }
    };
    stats.adjudicator_suspect = canary_suspect(stats.unrelated, stats.related);

    storage.with_connection(|conn| persist_backfill_state(conn, &stats))?;

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock(text: &'static str) -> impl Fn(Option<&str>, &str) -> AdjudicateAttempt {
        move |_model, _prompt| {
            AdjudicateAttempt::Parsed(ParsedNarrative {
                text: text.to_string(),
                model: "mock".to_string(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        }
    }

    // -----------------------------------------------------------------
    // D9 leg-conjunction truth table
    // -----------------------------------------------------------------

    #[test]
    fn leg_conjunction_truth_table() {
        type Case = (&'static str, bool, bool, bool, bool, bool, Option<Relation>);
        let cases: Vec<Case> = vec![
            // (generator, qa, qb, incompatible, same_approach, extended, expected)
            (
                "ledger",
                true,
                true,
                true,
                false,
                false,
                Some(Relation::ReplacedBy),
            ),
            (
                "ledger",
                true,
                true,
                true,
                false,
                true,
                Some(Relation::ExtendedBy),
            ),
            ("ledger", false, true, true, false, false, None),
            ("ledger", true, false, true, false, false, None),
            ("ledger", true, true, false, false, false, None),
            ("ledger", false, false, false, false, false, None),
            ("ledger", false, false, true, false, true, None),
            // relapse must win same_approach, never incompatible
            (
                "relapse",
                true,
                true,
                false,
                true,
                false,
                Some(Relation::ExtendedBy),
            ),
            ("relapse", true, true, true, false, false, None),
            ("relapse", true, false, false, true, false, None),
            ("relapse", false, true, false, true, false, None),
            ("relapse", true, true, false, false, false, None),
            // ledger cannot win via same_approach alone
            ("ledger", true, true, false, true, false, None),
            // both legs true = contract violation = no verdict, either generator
            ("ledger", true, true, true, true, false, None),
            ("relapse", true, true, true, true, false, None),
            // era behaves like ledger
            (
                "era",
                true,
                true,
                true,
                false,
                false,
                Some(Relation::ReplacedBy),
            ),
            ("era", true, true, false, true, false, None),
        ];
        for (generator, qa, qb, incompatible, same_approach, extended, expected) in cases {
            let v = RawVerdict {
                quote_a_attests_a: qa,
                quote_b_attests_b: qb,
                incompatible,
                same_approach,
                extended,
                quote_a: "x".into(),
                quote_b: "y".into(),
                oids: vec![],
            };
            assert_eq!(
                decide_relation(&v, generator),
                expected,
                "gen={generator} qa={qa} qb={qb} incompatible={incompatible} same_approach={same_approach} extended={extended}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Canary band
    // -----------------------------------------------------------------

    #[test]
    fn canary_band_boundaries() {
        // Exactly at the boundaries -- inclusive, never suspect.
        assert!(!canary_suspect(10, 90)); // 10.0% == MIN
        assert!(!canary_suspect(40, 60)); // 40.0% == MAX
                                          // Just outside either edge -- suspect.
        assert!(canary_suspect(9, 91)); // 9.0% < MIN
        assert!(canary_suspect(41, 59)); // 41.0% > MAX
                                         // A comfortable mid-band value -- never suspect.
        assert!(!canary_suspect(25, 75));
        // No decided candidates at all -- never suspect (nothing to judge).
        assert!(!canary_suspect(0, 0));
    }

    // -----------------------------------------------------------------
    // Verdict parsing
    // -----------------------------------------------------------------

    #[test]
    fn parse_verdict_strips_fences_and_defaults_missing_fields() {
        let text = "```json\n{\"quote_a_attests_a\": true, \"quote_a\": \"hi\"}\n```";
        let v = parse_verdict(text).expect("must parse");
        assert!(v.quote_a_attests_a);
        assert_eq!(v.quote_a, "hi");
        assert!(!v.quote_b_attests_b);
        assert!(v.oids.is_empty());
    }

    #[test]
    fn parse_verdict_rejects_garbage() {
        assert!(parse_verdict("not json at all").is_none());
    }

    // -----------------------------------------------------------------
    // Budget circuit breaker leaves a resumable backlog
    // -----------------------------------------------------------------

    fn seed_pair(conn: &Connection, n: usize) -> String {
        let ep_a = format!("ep-{n}-a");
        let ep_b = format!("ep-{n}-b");
        for (id, ts) in [
            (&ep_a, "2020-01-01T00:00:00Z"),
            (&ep_b, "2020-02-01T00:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
                params![
                    id,
                    format!(
                        r#"{{"schema":"v2","session_id":"{id}","project":"p","timestamp":"{ts}","request":"r","completed":"c","outcome":"completed","todos":[],"files_modified":[],"anchors":[]}}"#
                    ),
                    ts
                ],
            )
            .unwrap();
        }
        crate::storage::dream_backfill::materialize_episode_index(conn).unwrap();
        conn.execute(
            "INSERT INTO dream_relations
                (project, ep_a, ep_b, relation, generator, topic_key, tier, gate_score, status)
             VALUES ('p', ?1, ?2, 'replaced_by', 'era', ?3, 'unverified', ?4, 'queued')",
            params![
                ep_a,
                ep_b,
                format!("symbol:sym{n}"),
                1.0 - (n as f64) * 0.01
            ],
        )
        .unwrap();
        ep_a
    }

    #[test]
    fn budget_breaker_leaves_a_resumable_backlog() {
        // Guards against a concurrently-running `CSR_NO_AI_NARRATIVES`/
        // `CSR_NO_DREAMING`-toggling test elsewhere in the crate (e.g. this
        // module's own `disabled_run_never_touches_the_queue`, or
        // `dream::backfill::compose`'s disabled-run tests) racing this
        // test's `run_adjudication_with` call via the shared process-global
        // env var — see `env_test_guard`'s own doc.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                for n in 0..5 {
                    seed_pair(conn, n);
                }
                Ok(())
            })
            .unwrap();

        // Always UNRELATED -- keeps the DB side of this test decoupled from
        // verify.rs's own logic.
        let actor = mock(
            r#"{"quote_a_attests_a": false, "quote_b_attests_b": false, "incompatible": false, "extended": false, "quote_a": "", "quote_b": "", "oids": []}"#,
        );

        let stats = run_adjudication_with(&actor, &storage, 3).unwrap();
        assert_eq!(stats.queued_before, 5);
        assert_eq!(stats.attempted, 3, "only the budget, never the whole queue");
        assert_eq!(stats.unrelated, 3);
        assert_eq!(
            stats.backlog_after, 2,
            "the un-fetched remainder must still be queued"
        );

        let still_queued: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM dream_relations WHERE status = 'queued' AND tier = 'unverified'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(still_queued, 2, "matches backlog_after exactly");

        // A second run with a fresh budget must pick up exactly the
        // remainder, not re-attempt anything already archived.
        let stats2 = run_adjudication_with(&actor, &storage, 10).unwrap();
        assert_eq!(stats2.attempted, 2);
        assert_eq!(stats2.backlog_after, 0);
    }

    // -----------------------------------------------------------------
    // C (conformance review, MED): direct unprimed-judge test -- the built
    // prompt must never leak the generator's identity or the hypothesis
    // vocabulary the D5 unprimed-judge design depends on withholding.
    // -----------------------------------------------------------------

    #[test]
    fn build_prompt_never_leaks_generator_identity_or_hypothesis_words() {
        let a = EpisodeFacts {
            episode_id: "ep-a".into(),
            session_id: "s-a".into(),
            ts: "2020-01-01T00:00:00Z".into(),
            request: "implement frame parsing for the telemetry stream".into(),
            completed: "wrote parse_frame() as a callback-based parser".into(),
            next_steps: None,
            blockers: None,
            files: vec!["src/parser.rs".into()],
        };
        let b = EpisodeFacts {
            episode_id: "ep-b".into(),
            session_id: "s-b".into(),
            ts: "2020-02-01T00:00:00Z".into(),
            request: "the parser drops frames under load; redesign it".into(),
            completed: "replaced the callback parser with a step-based design".into(),
            next_steps: None,
            blockers: None,
            files: vec!["src/parser.rs".into()],
        };
        let prompt = build_prompt("symbol:parse_frame", &a, &[], &b, &[]);
        let lower = prompt.to_lowercase();
        // Word-boundary check, not raw substring: "era" is a real substring
        // of ordinary English words the fixed rules text legitimately uses
        // ("literally" contains "era") -- the leak this test guards against
        // is the GENERATOR LABEL appearing as its own token, not any
        // coincidental substring collision.
        let words: std::collections::HashSet<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        for banned_word in [
            "relapse",
            "ledger",
            "era",
            "superseded",
            "verdict",
            "obsolete",
            "witness",
        ] {
            assert!(
                !words.contains(banned_word),
                "unprimed judge prompt leaked the word {banned_word:?}:\n{prompt}"
            );
        }
        for banned_substring in ["replaced_by", "extended_by"] {
            assert!(
                !lower.contains(banned_substring),
                "unprimed judge prompt leaked {banned_substring:?}:\n{prompt}"
            );
        }
    }

    // -----------------------------------------------------------------
    // B: a duplicate-story collision between two queued candidates must not
    // abort `run_adjudication_with` mid-loop.
    // -----------------------------------------------------------------

    #[test]
    fn run_adjudication_completes_when_two_queued_rows_collide_on_the_same_final_relation() {
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                for (id, ts) in [
                    ("ep-x-a", "2020-01-01T00:00:00Z"),
                    ("ep-x-b", "2020-02-01T00:00:00Z"),
                ] {
                    conn.execute(
                        "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
                        params![
                            id,
                            format!(
                                r#"{{"schema":"v2","session_id":"{id}","project":"p","timestamp":"{ts}","request":"alpha bravo widget","completed":"charlie delta gadget","outcome":"completed","todos":[],"files_modified":[],"anchors":[]}}"#
                            ),
                            ts
                        ],
                    )
                    .unwrap();
                }
                crate::storage::dream_backfill::materialize_episode_index(conn).unwrap();
                // Two twin-hypothesis rows for the SAME (project, ep_a, ep_b)
                // pair, proposed by different generators -- the shape P3's
                // dedup prevents going forward, but a pre-existing pair like
                // this (an earlier run, before P3 shipped) must still not
                // crash adjudication.
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, gate_score, status)
                     VALUES ('p', 'ep-x-a', 'ep-x-b', 'replaced_by', 'ledger', 'symbol:x', 'unverified', 0.90, 'queued')",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, gate_score, status)
                     VALUES ('p', 'ep-x-a', 'ep-x-b', 'extended_by', 'era', 'symbol:x', 'unverified', 0.80, 'queued')",
                    [],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();

        // Both candidates decide REPLACED_BY -- the second one to be
        // promoted must collide with the first and archive as a duplicate,
        // not throw.
        let actor = mock(
            r#"{"quote_a_attests_a": true, "quote_b_attests_b": true, "incompatible": true, "extended": false, "quote_a": "alpha bravo widget", "quote_b": "charlie delta gadget", "oids": []}"#,
        );

        let stats = run_adjudication_with(&actor, &storage, 10).unwrap();
        assert_eq!(
            stats.deterministic_promoted, 1,
            "the ledger row promotes on machine evidence"
        );
        assert_eq!(stats.attempted, 1, "only the era row spends an LLM call");
        assert_eq!(
            stats.related, 1,
            "the era row decided a relation, not UNRELATED"
        );
        assert_eq!(
            stats.backlog_after, 0,
            "the run must complete and drain the queue, never abort mid-loop"
        );

        let promoted: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM dream_relations WHERE project = 'p' AND tier = 'witnessed'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(promoted, 1, "exactly one of the twin rows may be promoted");

        let duplicate_discards: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM backfill_discards WHERE reason = 'duplicate_story'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(duplicate_discards, 1);
    }

    #[test]
    fn disabled_run_never_touches_the_queue() {
        let _g = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_AI_NARRATIVES", "1");
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                seed_pair(conn, 0);
                Ok(())
            })
            .unwrap();
        let actor = mock("{}");
        let stats = run_adjudication_with(&actor, &storage, 10).unwrap();
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
        assert!(stats.disabled);
        assert_eq!(stats.attempted, 0);
    }
}
