//! Reaction-judge shadow mode.
//!
//! Scores every interactive (assistant tail, next human turn) pair in
//! `<projects_dir>/*/*.jsonl` with each student model installed under
//! `~/.claude-self-reflect/models/reaction-shadow/<name>/` (`model.onnx`,
//! `tokenizer.json`, `spec.json`) and logs both probabilities to
//! `reaction_shadow_scores`. Daemon-side only: nothing here is read by hooks,
//! search or injection, so behaviour does not change. No model directory means
//! every cycle is a no-op; `CSR_NO_REACTION_SHADOW=1` stops the loop.
//!
//! The pair rule, tail rule and truncation mirror the Python pool builder the
//! students were trained on (`build_pool.py` `parse()` + its state shaping) and
//! the token packing mirrors `squeeze_job.py` `Packer`. `csr-engine
//! reaction-shadow parity` checks the packing and the probabilities against a
//! fixture written by the Python side.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::Storage;

/// Injected (non-human) user-role prefixes; a turn starting with one is not a reaction.
const HARNESS: &[&str] = &[
    "This session is being continued",
    "Another Claude session sent a message",
    "The coordinator sent a message",
    "<cross-session-message",
    "<agent-message",
    "<task-notification",
    "<local-command",
    "<command-name",
    "<command-message",
    "Caveat:",
    "[Request interrupted",
    "Base directory for this skill",
    "<system-reminder",
    "<bash-",
    "<user-prompt-submit-hook",
];
const TAIL_CHARS: usize = 600;
const TURN_CHARS: usize = 1200;
const PREV_CHARS: usize = 300;
/// `pack == "s3"` keeps the human turn's first 224 tokens regardless of maxlen.
const S3_HUMAN_TOKENS: usize = 224;
const ENTRYPOINT_PROBE_LINES: usize = 50;
pub const DEFAULT_MAX_SCORES_PER_CYCLE: usize = 400;
pub const DEFAULT_WINDOW_DAYS: u64 = 14;
/// A pass stops starting new work after this long; unfinished files stay incomplete.
const PASS_WALL_CLOCK: Duration = Duration::from_secs(120);
/// Transcripts above this size are skipped (a stat per pass, never parsed).
const MAX_TRANSCRIPT_BYTES: u64 = 1 << 30;
/// A single JSONL record above this size is drained unparsed and counted as bad.
const MAX_LINE_BYTES: u64 = 32 << 20;
/// Parsing checks shutdown and the pass deadline every this many lines.
const STOP_CHECK_LINES: usize = 256;

/// Shutdown or the pass deadline stopped a parse; the file is not checkpointed.
#[derive(Debug)]
pub struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reaction shadow pass interrupted")
    }
}

impl std::error::Error for Interrupted {}

/// Read the next record (without its newline) into `buf`. `None` at EOF,
/// `Some(false)` for a record longer than `MAX_LINE_BYTES` (drained, not kept).
fn next_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    buf.clear();
    let n = std::io::Read::take(&mut *reader, MAX_LINE_BYTES).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(Some(true));
    }
    if (n as u64) < MAX_LINE_BYTES {
        return Ok(Some(true));
    }
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            break;
        }
        let len = available.len();
        reader.consume(len);
    }
    buf.clear();
    Ok(Some(false))
}

pub fn disabled() -> bool {
    std::env::var("CSR_NO_REACTION_SHADOW")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CSR_REACTION_SHADOW_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| {
        h.join(".claude-self-reflect")
            .join("models")
            .join("reaction-shadow")
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    pub name: String,
    pub pack: String,
    pub order: String,
    pub maxlen: usize,
    pub hfirst: usize,
    pub hlast: usize,
    #[serde(default)]
    pub prev: usize,
    pub cls_id: u32,
    pub sep_id: u32,
    pub inputs: Vec<String>,
    pub output: String,
    pub model_sha256: String,
    /// Dev threshold giving >= 0.90 correction precision (reporting only).
    #[serde(default)]
    pub thr_correction: Option<f64>,
}

impl ModelSpec {
    fn validate(&self) -> Result<()> {
        match self.pack.as_str() {
            "s3" => {}
            "split" if self.hlast > 0 => {}
            "split" => bail!("{}: split packing needs hlast > 0", self.name),
            other => bail!("{}: unknown pack {other}", self.name),
        }
        if self.order != "HA" && self.order != "AH" {
            bail!("{}: unknown order {}", self.name, self.order);
        }
        if self.maxlen <= 3 {
            bail!("{}: maxlen {} too small", self.name, self.maxlen);
        }
        Ok(())
    }
}

/// Installed model specs, sorted by name. Empty when nothing is installed.
pub fn installed_specs(dir: &Path) -> Vec<(PathBuf, ModelSpec)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, ModelSpec)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("model.onnx").is_file() && p.join("tokenizer.json").is_file())
        .filter_map(|p| {
            let spec: ModelSpec =
                serde_json::from_slice(&std::fs::read(p.join("spec.json")).ok()?).ok()?;
            spec.validate().ok()?;
            Some((p, spec))
        })
        .collect();
    out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairState {
    pub previous_human_request: String,
    pub assistant_last_message_tail: String,
    pub human_next_turn: String,
}

fn home_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(/Users/|/home/|C:\\Users\\)[^/\\\s"']+"#).expect("static regex")
    })
}

fn scrub(s: &str) -> String {
    home_re().replace_all(s, "${1}USER").into_owned()
}

fn squash_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn last_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

/// State shaping of the training pool: prev = first 300 chars of the
/// whitespace-squashed previous request, tail = last 600 chars of the
/// whitespace-squashed assistant message, turn = first 1200 chars; home paths scrubbed.
pub fn build_state(prev: &str, tail: &str, turn: &str) -> PairState {
    PairState {
        previous_human_request: scrub(&first_chars(&squash_ws(prev), PREV_CHARS)),
        assistant_last_message_tail: scrub(&last_chars(&squash_ws(tail), TAIL_CHARS)),
        human_next_turn: scrub(&first_chars(turn, TURN_CHARS)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptPair {
    pub turn_uuid: String,
    pub session_id: String,
    pub ts: Option<String>,
    pub state: PairState,
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

fn block_type(b: &Value) -> Option<&str> {
    b.get("type").and_then(Value::as_str)
}

fn joined_text_blocks(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| block_type(b) == Some("text"))
        .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn user_text(content: Option<&Value>) -> (String, bool) {
    match content {
        Some(Value::String(s)) => (s.clone(), false),
        Some(Value::Array(blocks)) => (
            joined_text_blocks(blocks),
            blocks.iter().any(|b| block_type(b) == Some("tool_result")),
        ),
        _ => (String::new(), false),
    }
}

/// Python's `open(..., errors="ignore")`: invalid UTF-8 bytes are dropped, not
/// replaced, before the line reaches the JSON parser.
fn decode_ignoring_invalid(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => Cow::Owned(bytes.utf8_chunks().map(|c| c.valid()).collect()),
    }
}

/// First `entrypoint` in the transcript's opening lines (`cli`, `claude-desktop`,
/// `sdk-cli`, ...). `None` when absent; read errors propagate so a transient
/// failure is never checkpointed as "headless".
pub fn probe_entrypoint(path: &Path) -> Result<Option<String>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut buf = Vec::new();
    for _ in 0..ENTRYPOINT_PROBE_LINES {
        let Some(whole) = next_line(&mut reader, &mut buf)? else {
            break;
        };
        if !whole {
            continue;
        }
        let Ok(d) = serde_json::from_str::<Value>(&decode_ignoring_invalid(&buf)) else {
            continue;
        };
        if let Some(ep) = d.get("entrypoint").and_then(Value::as_str) {
            return Ok(Some(ep.to_string()));
        }
    }
    Ok(None)
}

/// Interactive transcripts only; headless `claude -p` children are `sdk-*`.
pub fn is_interactive(entrypoint: Option<&str>) -> bool {
    entrypoint.is_some_and(|ep| !ep.starts_with("sdk"))
}

/// (previous request, last assistant text message, next human turn) pairs, with
/// the training pool's rules: sidechain and meta records skipped; the pair's tail
/// is the LAST assistant text message before the turn; tool results, empty turns,
/// compact summaries and harness-injected turns are not human turns.
/// Returns the pairs and the number of lines that were not valid JSON (or were
/// oversize). `stop` is polled every few hundred lines; when it fires the parse
/// fails with [`Interrupted`] so nothing partial is checkpointed.
pub fn transcript_pairs(
    path: &Path,
    stop: &dyn Fn() -> bool,
) -> Result<(Vec<TranscriptPair>, usize)> {
    let mut reader = BufReader::new(File::open(path)?);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut last_asst: Option<String> = None;
    let mut prev_human = String::new();
    let mut pending_prev = String::new();
    let mut pairs = Vec::new();
    let mut bad_lines = 0usize;
    let mut buf = Vec::new();
    let mut lines = 0usize;
    while let Some(whole) = next_line(&mut reader, &mut buf)? {
        lines += 1;
        if lines.is_multiple_of(STOP_CHECK_LINES) && stop() {
            return Err(Interrupted.into());
        }
        if !whole {
            bad_lines += 1;
            continue;
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(d) = serde_json::from_str::<Value>(&decode_ignoring_invalid(&buf)) else {
            bad_lines += 1;
            continue;
        };
        if truthy(d.get("isSidechain")) || truthy(d.get("isMeta")) {
            continue;
        }
        let content = d.get("message").and_then(|m| m.get("content"));
        match d.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                if let Some(Value::Array(blocks)) = content {
                    let txt = joined_text_blocks(blocks);
                    let txt = txt.trim();
                    if !txt.is_empty() {
                        if last_asst.is_none() {
                            pending_prev = prev_human.clone();
                        }
                        last_asst = Some(txt.to_string());
                    }
                }
            }
            Some("user") => {
                let (txt, has_tool) = user_text(content);
                let txt = txt.trim();
                if has_tool
                    || txt.is_empty()
                    || truthy(d.get("isCompactSummary"))
                    || HARNESS.iter().any(|p| txt.starts_with(p))
                {
                    continue;
                }
                if let Some(tail) = last_asst.take() {
                    let state = build_state(&pending_prev, &tail, txt);
                    let session_id = d
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .map_or_else(|| stem.clone(), String::from);
                    let ts = d.get("timestamp").and_then(Value::as_str).map(String::from);
                    let turn_uuid = d.get("uuid").and_then(Value::as_str).map_or_else(
                        || {
                            blake3::hash(
                                format!("{session_id}\n{}\n{txt}", ts.as_deref().unwrap_or(""))
                                    .as_bytes(),
                            )
                            .to_hex()[..24]
                                .to_string()
                        },
                        String::from,
                    );
                    pairs.push(TranscriptPair {
                        turn_uuid,
                        session_id,
                        ts,
                        state,
                    });
                }
                prev_human = txt.to_string();
            }
            _ => {}
        }
    }
    Ok((pairs, bad_lines))
}

pub struct ShadowModel {
    pub spec: ModelSpec,
    tokenizer: tokenizers::Tokenizer,
    session: ort::session::Session,
}

impl ShadowModel {
    pub fn load(dir: &Path) -> Result<Self> {
        let spec: ModelSpec = serde_json::from_slice(
            &std::fs::read(dir.join("spec.json")).context("reading spec.json")?,
        )
        .context("parsing spec.json")?;
        spec.validate()?;
        let mut tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow!("{}: tokenizer: {e}", spec.name))?;
        tokenizer
            .with_truncation(None)
            .map_err(|e| anyhow!("{}: tokenizer truncation: {e}", spec.name))?;
        tokenizer.with_padding(None);
        let session = ort::session::Session::builder()
            .map_err(|e| anyhow!(e.to_string()))?
            .with_intra_threads(1)
            .map_err(|e| anyhow!(e.to_string()))?
            .with_inter_threads(1)
            .map_err(|e| anyhow!(e.to_string()))?
            .commit_from_file(dir.join("model.onnx"))
            .map_err(|e| anyhow!("{}: onnx: {e}", spec.name))?;
        let have: HashSet<&str> = session.inputs().iter().map(|i| i.name()).collect();
        for name in &spec.inputs {
            if !have.contains(name.as_str()) {
                bail!("{}: model has no input {name}", spec.name);
            }
        }
        Ok(Self {
            spec,
            tokenizer,
            session,
        })
    }

    fn encode(&self, text: &str) -> Result<Vec<u32>> {
        Ok(self
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow!("{}: encode: {e}", self.spec.name))?
            .get_ids()
            .to_vec())
    }

    /// `[CLS] s1 [SEP] s2 [SEP]` and `len(s1)`, as `squeeze_job.py` `Packer.one`.
    pub fn pack(&self, st: &PairState) -> Result<(Vec<u32>, usize)> {
        let a = &self.spec;
        let mut h = self.encode(&st.human_next_turn)?;
        let t = self.encode(&st.assistant_last_message_tail)?;
        let room_total = a.maxlen - 3;
        let seg_t: Vec<u32> = if a.pack == "s3" {
            h.truncate(S3_HUMAN_TOKENS);
            let room = room_total.saturating_sub(h.len());
            t[..room.min(t.len())].to_vec()
        } else {
            if h.len() > a.hfirst + a.hlast {
                let tail_start = h.len() - a.hlast;
                let mut kept = h[..a.hfirst].to_vec();
                kept.extend_from_slice(&h[tail_start..]);
                h = kept;
            }
            let mut pv = if a.prev > 0 {
                let mut p = self.encode(&st.previous_human_request)?;
                p.truncate(a.prev);
                p
            } else {
                Vec::new()
            };
            if !pv.is_empty() {
                pv.push(a.sep_id);
            }
            let room = room_total.saturating_sub(h.len() + pv.len());
            let mut seg = pv;
            if room > 0 {
                seg.extend_from_slice(&t[t.len().saturating_sub(room)..]);
            }
            seg
        };
        let (s1, s2) = if a.order == "HA" {
            (h, seg_t)
        } else {
            (seg_t, h)
        };
        let mut ids = Vec::with_capacity(s1.len() + s2.len() + 3);
        ids.push(a.cls_id);
        ids.extend_from_slice(&s1);
        ids.push(a.sep_id);
        ids.extend_from_slice(&s2);
        ids.push(a.sep_id);
        Ok((ids, s1.len()))
    }

    /// `[p_correction, p_acceptance]` at batch 1, plus the packed length.
    pub fn score(&mut self, st: &PairState) -> Result<([f32; 2], usize)> {
        let (ids, l1) = self.pack(st)?;
        let n = ids.len();
        let mut inputs: Vec<(Cow<'_, str>, ort::session::SessionInputValue<'_>)> = Vec::new();
        for name in &self.spec.inputs {
            let data: Vec<i64> = match name.as_str() {
                "input_ids" => ids.iter().map(|&x| i64::from(x)).collect(),
                "attention_mask" => vec![1; n],
                "token_type_ids" => (0..n).map(|i| i64::from(i >= l1 + 2)).collect(),
                other => bail!("{}: unsupported input {other}", self.spec.name),
            };
            let tensor = ort::value::Tensor::from_array((vec![1i64, n as i64], data))
                .map_err(|e| anyhow!(e.to_string()))?;
            inputs.push((Cow::Owned(name.clone()), tensor.into()));
        }
        let output = self.spec.output.clone();
        let name = self.spec.name.clone();
        let outputs = self
            .session
            .run(inputs)
            .map_err(|e| anyhow!("{name}: run: {e}"))?;
        let value = outputs
            .get(output.as_str())
            .ok_or_else(|| anyhow!("{name}: no output {output}"))?;
        let (_, probs) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("{name}: extract: {e}"))?;
        if probs.len() < 2 {
            bail!("{name}: output has {} values", probs.len());
        }
        Ok(([probs[0], probs[1]], n))
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CycleSummary {
    pub models: Vec<String>,
    pub files_seen: usize,
    pub files_unchanged: usize,
    pub files_headless: usize,
    pub files_parsed: usize,
    pub files_too_large: usize,
    pub pairs_seen: usize,
    pub scores_written: usize,
    pub budget_exhausted: bool,
    /// Wall clock or shutdown ended the pass early.
    pub interrupted: bool,
    pub errors: usize,
    /// Transcript lines that were not valid JSON (skipped, as in the Python builder).
    pub bad_lines: usize,
}

fn models_fingerprint(specs: &[(PathBuf, ModelSpec)]) -> String {
    specs
        .iter()
        .map(|(_, s)| format!("{}:{}", s.name, s.model_sha256))
        .collect::<Vec<_>>()
        .join(",")
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

struct FileRow {
    mtime_ns: i64,
    size: i64,
    entrypoint: Option<String>,
    fingerprint: String,
    complete: bool,
}

fn file_row(storage: &Storage, path: &str) -> Result<Option<FileRow>> {
    storage.with_connection(|conn| {
        Ok(conn
            .query_row(
                "SELECT mtime_ns, size, entrypoint, models, complete
                   FROM reaction_shadow_files WHERE path = ?1",
                params![path],
                |r| {
                    Ok(FileRow {
                        mtime_ns: r.get(0)?,
                        size: r.get(1)?,
                        entrypoint: r.get(2)?,
                        fingerprint: r.get(3)?,
                        complete: r.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()?)
    })
}

fn upsert_file(
    storage: &Storage,
    path: &str,
    meta: (i64, i64),
    entrypoint: Option<&str>,
    fingerprint: &str,
    pairs: usize,
    complete: bool,
) -> Result<()> {
    storage.with_connection(|conn| {
        conn.execute(
            "INSERT INTO reaction_shadow_files
                (path, mtime_ns, size, entrypoint, models, pairs, complete, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(path) DO UPDATE SET
                mtime_ns = excluded.mtime_ns, size = excluded.size,
                entrypoint = excluded.entrypoint, models = excluded.models,
                pairs = excluded.pairs, complete = excluded.complete,
                scanned_at = excluded.scanned_at",
            params![
                path,
                meta.0,
                meta.1,
                entrypoint,
                fingerprint,
                pairs as i64,
                i64::from(complete),
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    })
}

fn scored_keys(storage: &Storage, session_id: &str) -> Result<HashSet<(String, String)>> {
    storage.with_connection(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT turn_uuid, model_sha256 FROM reaction_shadow_scores WHERE session_id = ?1",
        )?;
        let rows = stmt.query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
    })
}

struct ScoreRow<'a> {
    pair: &'a TranscriptPair,
    project_dir: &'a str,
    model: &'a ModelSpec,
    probs: [f32; 2],
    n_tokens: usize,
}

fn insert_scores(storage: &Storage, rows: &[ScoreRow<'_>]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let now = chrono::Utc::now().to_rfc3339();
    storage.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        let mut written = 0;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO reaction_shadow_scores
                    (turn_uuid, model, model_sha256, session_id, project_dir, ts,
                     p_correction, p_acceptance, n_tokens, previous_human_request,
                     assistant_tail, human_turn, scored_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?;
            for r in rows {
                written += stmt.execute(params![
                    r.pair.turn_uuid,
                    r.model.name,
                    r.model.model_sha256,
                    r.pair.session_id,
                    r.project_dir,
                    r.pair.ts,
                    f64::from(r.probs[0]),
                    f64::from(r.probs[1]),
                    r.n_tokens as i64,
                    r.pair.state.previous_human_request,
                    r.pair.state.assistant_last_message_tail,
                    r.pair.state.human_next_turn,
                    now
                ])?;
            }
        }
        tx.commit()?;
        Ok(written)
    })
}

/// One pass: walk `<projects_dir>/*/*.jsonl` modified within `window_days`,
/// skip unchanged and headless transcripts, score new pairs with every
/// installed model, stop after `max_scores` model scorings. Models load lazily
/// and are dropped when the pass ends, so the daemon holds no ONNX session between passes.
pub fn run_cycle(
    storage: &Storage,
    projects_dir: &Path,
    models_root: &Path,
    max_scores: usize,
    window_days: u64,
    shutdown: Option<&AtomicBool>,
) -> Result<CycleSummary> {
    let specs = installed_specs(models_root);
    let mut summary = CycleSummary {
        models: specs.iter().map(|(_, s)| s.name.clone()).collect(),
        ..CycleSummary::default()
    };
    if specs.is_empty() {
        return Ok(summary);
    }
    let fingerprint = models_fingerprint(&specs);
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(window_days.saturating_mul(86_400)))
        .unwrap_or(UNIX_EPOCH);
    let mut files: Vec<(PathBuf, String, std::fs::Metadata)> = Vec::new();
    for project in std::fs::read_dir(projects_dir)
        .with_context(|| format!("reading {}", projects_dir.display()))?
        .flatten()
    {
        let project_path = project.path();
        if !project_path.is_dir() {
            continue;
        }
        let project_dir = project.file_name().to_string_lossy().into_owned();
        let Ok(entries) = std::fs::read_dir(&project_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || meta.modified().map_or(true, |m| m < cutoff) {
                continue;
            }
            files.push((path, project_dir.clone(), meta));
        }
    }
    // Oldest first so a budget-limited backfill converges on history before the live session.
    files.sort_by_key(|(_, _, meta)| mtime_ns(meta));
    let mut models: Option<Vec<ShadowModel>> = None;
    let mut budget = max_scores;
    let deadline = std::time::Instant::now() + PASS_WALL_CLOCK;
    for (path, project_dir, meta) in files {
        if budget == 0 {
            break;
        }
        if shutdown.is_some_and(|s| s.load(Ordering::SeqCst))
            || std::time::Instant::now() >= deadline
        {
            summary.interrupted = true;
            break;
        }
        summary.files_seen += 1;
        let key = path.to_string_lossy().into_owned();
        let stat = (
            mtime_ns(&meta),
            i64::try_from(meta.len()).unwrap_or(i64::MAX),
        );
        let previous = file_row(storage, &key)?;
        if let Some(row) = &previous {
            let same_bytes = row.mtime_ns == stat.0 && row.size == stat.1;
            if same_bytes && !is_interactive(row.entrypoint.as_deref()) {
                summary.files_unchanged += 1;
                continue;
            }
            if same_bytes && row.complete && row.fingerprint == fingerprint {
                summary.files_unchanged += 1;
                continue;
            }
        }
        let entrypoint = match previous.as_ref().and_then(|r| r.entrypoint.clone()) {
            Some(ep) => Some(ep),
            None => match probe_entrypoint(&path) {
                Ok(ep) => ep,
                Err(e) => {
                    tracing::debug!("reaction shadow probe: {}: {e}", path.display());
                    summary.errors += 1;
                    continue;
                }
            },
        };
        if !is_interactive(entrypoint.as_deref()) {
            summary.files_headless += 1;
            upsert_file(
                storage,
                &key,
                stat,
                entrypoint.as_deref(),
                &fingerprint,
                0,
                true,
            )?;
            continue;
        }
        if meta.len() > MAX_TRANSCRIPT_BYTES {
            summary.files_too_large += 1;
            continue;
        }
        let stop = || {
            shutdown.is_some_and(|s| s.load(Ordering::SeqCst))
                || std::time::Instant::now() >= deadline
        };
        let pairs = match transcript_pairs(&path, &stop) {
            Ok((p, bad)) => {
                summary.bad_lines += bad;
                p
            }
            Err(e) if e.is::<Interrupted>() => {
                summary.interrupted = true;
                break;
            }
            Err(e) => {
                tracing::debug!("reaction shadow: {}: {e}", path.display());
                summary.errors += 1;
                continue;
            }
        };
        summary.files_parsed += 1;
        summary.pairs_seen += pairs.len();
        let mut done: HashSet<(String, String)> = HashSet::new();
        for session in pairs
            .iter()
            .map(|p| p.session_id.as_str())
            .collect::<HashSet<_>>()
        {
            done.extend(scored_keys(storage, session)?);
        }
        let mut todo: Vec<(&TranscriptPair, usize)> = Vec::new();
        for pair in &pairs {
            for (index, (_, spec)) in specs.iter().enumerate() {
                if !done.contains(&(pair.turn_uuid.clone(), spec.model_sha256.clone())) {
                    todo.push((pair, index));
                }
            }
        }
        let mut complete = todo.len() <= budget;
        todo.truncate(budget);
        if !todo.is_empty() && models.is_none() {
            let mut loaded = Vec::with_capacity(specs.len());
            for (dir, _) in &specs {
                loaded.push(ShadowModel::load(dir)?);
            }
            models = Some(loaded);
        }
        let mut rows = Vec::with_capacity(todo.len());
        if let Some(models) = models.as_mut() {
            for (pair, index) in &todo {
                if shutdown.is_some_and(|s| s.load(Ordering::SeqCst))
                    || std::time::Instant::now() >= deadline
                {
                    summary.interrupted = true;
                    complete = false;
                    break;
                }
                budget -= 1;
                match models[*index].score(&pair.state) {
                    Ok((probs, n_tokens)) => rows.push(ScoreRow {
                        pair,
                        project_dir: &project_dir,
                        model: &specs[*index].1,
                        probs,
                        n_tokens,
                    }),
                    Err(e) => {
                        tracing::debug!("reaction shadow score: {e}");
                        summary.errors += 1;
                        complete = false;
                    }
                }
            }
        }
        summary.scores_written += insert_scores(storage, &rows)?;
        upsert_file(
            storage,
            &key,
            stat,
            entrypoint.as_deref(),
            &fingerprint,
            pairs.len(),
            complete,
        )?;
    }
    if budget == 0 {
        summary.budget_exhausted = true;
    }
    Ok(summary)
}

/// Per-model counts plus head-to-head agreement for `csr-engine reaction-shadow status`.
pub fn status(storage: &Storage, models_root: &Path) -> Result<Value> {
    let specs = installed_specs(models_root);
    let thresholds: BTreeMap<String, f64> = specs
        .iter()
        .filter_map(|(_, s)| s.thr_correction.map(|t| (s.name.clone(), t)))
        .collect();
    storage.with_connection(|conn| {
        let mut per_model = serde_json::Map::new();
        let mut stmt = conn.prepare(
            "SELECT model, model_sha256, COUNT(*), AVG(p_correction), AVG(p_acceptance),
                    SUM(p_correction >= 0.5), SUM(p_acceptance >= 0.5), MIN(ts), MAX(ts), MAX(scored_at)
               FROM reaction_shadow_scores GROUP BY model, model_sha256 ORDER BY model",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })?;
        for row in rows {
            let (model, sha, n, pc, pa, c50, a50, first, last, scored) = row?;
            let over_thr = match thresholds.get(&model) {
                Some(t) => conn.query_row(
                    "SELECT COUNT(*) FROM reaction_shadow_scores
                      WHERE model = ?1 AND model_sha256 = ?2 AND p_correction >= ?3",
                    params![model, sha, t],
                    |r| r.get::<_, i64>(0),
                )?,
                None => -1,
            };
            per_model.insert(
                format!("{model}@{}", &sha[..sha.len().min(12)]),
                serde_json::json!({
                    "pairs": n, "mean_p_correction": pc, "mean_p_acceptance": pa,
                    "correction_ge_0_5": c50, "acceptance_ge_0_5": a50,
                    "thr_correction": thresholds.get(&model),
                    "correction_ge_thr": if over_thr >= 0 { Some(over_thr) } else { None },
                    "first_ts": first, "last_ts": last, "last_scored_at": scored,
                }),
            );
        }
        let head_to_head = if specs.len() == 2 {
            let (a, b) = (&specs[0].1, &specs[1].1);
            let (ta, tb) = (a.thr_correction.unwrap_or(0.5), b.thr_correction.unwrap_or(0.5));
            let (n, both, only_a, only_b, abs_diff): (i64, Option<i64>, Option<i64>, Option<i64>, Option<f64>) = conn.query_row(
                "SELECT COUNT(*),
                        SUM(x.p_correction >= ?3 AND y.p_correction >= ?4),
                        SUM(x.p_correction >= ?3 AND y.p_correction < ?4),
                        SUM(x.p_correction < ?3 AND y.p_correction >= ?4),
                        AVG(ABS(x.p_correction - y.p_correction))
                   FROM reaction_shadow_scores x
                   JOIN reaction_shadow_scores y ON y.turn_uuid = x.turn_uuid
                  WHERE x.model_sha256 = ?1 AND y.model_sha256 = ?2",
                params![a.model_sha256, b.model_sha256, ta, tb],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            serde_json::json!({
                "a": a.name, "b": b.name, "pairs_scored_by_both": n,
                "correction_flag_both": both, "correction_flag_only_a": only_a,
                "correction_flag_only_b": only_b, "mean_abs_diff_p_correction": abs_diff,
                "thresholds": {"a": ta, "b": tb},
            })
        } else {
            Value::Null
        };
        let (files, interactive, incomplete): (i64, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT COUNT(*), SUM(entrypoint IS NOT NULL AND entrypoint NOT LIKE 'sdk%'),
                    SUM(complete = 0)
               FROM reaction_shadow_files",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(serde_json::json!({
            "disabled": disabled(),
            "models_dir": models_root.display().to_string(),
            "installed_models": specs.iter().map(|(_, s)| &s.name).collect::<Vec<_>>(),
            "files_tracked": files, "interactive_files": interactive, "incomplete_files": incomplete,
            "models": per_model,
            "head_to_head": head_to_head,
        }))
    })
}

#[derive(Debug, Deserialize)]
struct ParityRow {
    pid: String,
    state: PairState,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

/// Compare the Rust packing and probabilities with the Python fixture written by
/// `shadow_prep.py` (`<name>` = expected probs, `<name>_ids` = expected ids).
/// The Rust probabilities are also written to `<fixture>.rust.jsonl`.
pub fn parity(models_root: &Path, fixture: &Path) -> Result<Value> {
    let specs = installed_specs(models_root);
    if specs.is_empty() {
        bail!("no models installed under {}", models_root.display());
    }
    let rows: Vec<ParityRow> = std::fs::read_to_string(fixture)
        .with_context(|| format!("reading {}", fixture.display()))?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    let mut report = serde_json::Map::new();
    let mut scored: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
    for (dir, spec) in &specs {
        let mut model = ShadowModel::load(dir)?;
        let (mut n, mut id_mismatch, mut max_dp, mut sum_dp) = (0usize, Vec::new(), 0f64, 0f64);
        for row in &rows {
            let (Some(expected), Some(expected_ids)) = (
                row.rest.get(&spec.name).and_then(Value::as_array),
                row.rest
                    .get(&format!("{}_ids", spec.name))
                    .and_then(Value::as_array),
            ) else {
                continue;
            };
            n += 1;
            let (ids, _) = model.pack(&row.state)?;
            let want: Vec<u64> = expected_ids.iter().filter_map(Value::as_u64).collect();
            if ids.iter().map(|&x| u64::from(x)).collect::<Vec<_>>() != want {
                id_mismatch.push(row.pid.clone());
            }
            let (probs, _) = model.score(&row.state)?;
            for (k, p) in probs.iter().enumerate() {
                let e = expected.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
                max_dp = max_dp.max((f64::from(*p) - e).abs());
                sum_dp += (f64::from(*p) - e).abs();
            }
            scored
                .entry(row.pid.clone())
                .or_default()
                .insert(spec.name.clone(), serde_json::json!(probs));
        }
        report.insert(
            spec.name.clone(),
            serde_json::json!({"rows": n, "id_mismatches": id_mismatch, "max_abs_dp": max_dp,
                               "mean_abs_dp": if n > 0 { sum_dp / (2 * n) as f64 } else { 0.0 }}),
        );
    }
    let mut out = String::new();
    for (pid, mut probs) in scored {
        probs.insert("pid".into(), Value::String(pid));
        out.push_str(&Value::Object(probs).to_string());
        out.push('\n');
    }
    std::fs::write(fixture.with_extension("rust.jsonl"), out)?;
    Ok(Value::Object(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    #[test]
    fn state_shaping_matches_the_training_pool() {
        let tail = format!("{}  end\tof   answer /Users/alice/x", "x".repeat(700));
        let st = build_state("  do the  thing\n", &tail, "  keep leading space");
        assert_eq!(st.previous_human_request, "do the thing");
        assert_eq!(st.assistant_last_message_tail.chars().count(), 599);
        assert!(st
            .assistant_last_message_tail
            .ends_with("end of answer /Users/USER/x"));
        assert_eq!(st.human_next_turn, "  keep leading space");
        let long: String = "é".repeat(1500);
        assert_eq!(
            build_state("", "", &long).human_next_turn.chars().count(),
            TURN_CHARS
        );
    }

    #[test]
    fn pairs_follow_the_pool_rules() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let asst = |t: &str| {
            line(
                serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": t}]}}),
            )
        };
        let user = |t: &str, uuid: &str| {
            line(
                serde_json::json!({"type": "user", "uuid": uuid, "sessionId": "s1",
                "timestamp": "2026-09-26T00:00:00Z", "message": {"content": t}}),
            )
        };
        let body = [
            line(serde_json::json!({"type": "user", "entrypoint": "cli", "message": {"content": "first request"}})),
            asst("working"),
            line(serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "ok"}]}})),
            asst("done, all green"),
            line(serde_json::json!({"type": "assistant", "isSidechain": true, "message": {"content": [{"type": "text", "text": "side"}]}})),
            user("<system-reminder>injected</system-reminder>", "u-h"),
            user("that is wrong, still failing", "u1"),
            user("hello again", "u-no-asst"),
            asst("fixed"),
            line(serde_json::json!({"type": "user", "isMeta": true, "message": {"content": "meta"}})),
            user("thanks", "u2"),
        ]
        .concat();
        f.write_all(body.as_bytes()).unwrap();
        let (pairs, bad) = transcript_pairs(f.path(), &|| false).unwrap();
        assert_eq!(bad, 0);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].turn_uuid, "u1");
        assert_eq!(
            pairs[0].state.assistant_last_message_tail,
            "done, all green"
        );
        assert_eq!(pairs[0].state.previous_human_request, "first request");
        assert_eq!(pairs[1].turn_uuid, "u2");
        assert_eq!(pairs[1].state.assistant_last_message_tail, "fixed");
        assert_eq!(pairs[1].state.previous_human_request, "hello again");
        assert_eq!(probe_entrypoint(f.path()).unwrap().as_deref(), Some("cli"));
    }

    #[test]
    fn invalid_utf8_bytes_are_dropped_like_python_errors_ignore() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut body = Vec::new();
        body.extend_from_slice(
            br#"{"type":"assistant","message":{"content":[{"type":"text","text":"done "#,
        );
        body.extend_from_slice(&[0xff, 0xfe]);
        body.extend_from_slice(br#" fixed"}]}}"#);
        body.push(b'\n');
        body.extend_from_slice(br#"{"type":"user","uuid":"u1","message":{"content":"thanks"}}"#);
        body.extend_from_slice(b"\nnot json\n");
        f.write_all(&body).unwrap();
        let (pairs, bad) = transcript_pairs(f.path(), &|| false).unwrap();
        assert_eq!(bad, 1);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].state.assistant_last_message_tail, "done fixed");
    }

    #[test]
    fn unreadable_transcripts_are_errors_not_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.jsonl");
        assert!(probe_entrypoint(&missing).is_err());
        assert!(transcript_pairs(&missing, &|| false).is_err());
    }

    #[test]
    fn a_stop_request_interrupts_a_parse_without_a_result() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for i in 0..(STOP_CHECK_LINES * 2) {
            writeln!(
                f,
                r#"{{"type":"user","uuid":"u{i}","message":{{"content":"turn {i}"}}}}"#
            )
            .unwrap();
        }
        let err = transcript_pairs(f.path(), &|| true).unwrap_err();
        assert!(err.is::<Interrupted>());
        assert!(transcript_pairs(f.path(), &|| false).is_ok());
    }

    #[test]
    fn oversize_records_are_drained_and_counted() {
        let mut reader = std::io::Cursor::new(
            [
                vec![b'x'; MAX_LINE_BYTES as usize + 10],
                b"\nnext\n".to_vec(),
            ]
            .concat(),
        );
        let mut buf = Vec::new();
        assert_eq!(next_line(&mut reader, &mut buf).unwrap(), Some(false));
        assert_eq!(next_line(&mut reader, &mut buf).unwrap(), Some(true));
        assert_eq!(buf, b"next");
        assert_eq!(next_line(&mut reader, &mut buf).unwrap(), None);
    }

    #[test]
    fn headless_children_are_not_interactive() {
        assert!(is_interactive(Some("cli")));
        assert!(is_interactive(Some("claude-desktop")));
        assert!(!is_interactive(Some("sdk-cli")));
        assert!(!is_interactive(None));
    }

    #[test]
    fn a_cycle_without_models_is_a_no_op() {
        let storage = Storage::open_memory().unwrap();
        let empty = tempfile::tempdir().unwrap();
        let s = run_cycle(&storage, empty.path(), empty.path(), 10, 14, None).unwrap();
        assert!(s.models.is_empty());
        assert_eq!(s.files_seen, 0);
    }

    /// Needs the installed models and the Python fixture; run with `--ignored`.
    #[test]
    #[ignore]
    fn installed_models_match_the_python_fixture() {
        let root = models_dir().unwrap();
        let report = parity(&root, &root.join("parity_dev40.jsonl")).unwrap();
        for (name, r) in report.as_object().unwrap() {
            assert!(r["rows"].as_u64().unwrap() > 0, "{name}");
            assert!(
                r["id_mismatches"].as_array().unwrap().is_empty(),
                "{name}: {r}"
            );
            assert!(r["max_abs_dp"].as_f64().unwrap() < 0.02, "{name}: {r}");
        }
    }
}
