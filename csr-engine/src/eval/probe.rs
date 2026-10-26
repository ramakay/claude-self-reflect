//! `csr-engine bench --probe`: the known-answer retrieval scorecard (side B).
//!
//! Opens the live database read-only, loads the persisted HNSW cache without
//! writing it, and sends the 12 probes (plus optional audit queries) through
//! the same function the `csr_reflect_on_past` MCP tool calls. No import, no
//! watcher, no rebuild, no persist.
//!
//! Judge rule (identical to the A-side Python client):
//!   a block runs from each `<r rank="N">` to the next `<r rank=` or the end of
//!   the text; a block is a HIT when at least one `must` regex matches it and no
//!   `must_not` regex matches it; hit_rank is the smallest N <= limit whose
//!   block is a hit. rank1 counts probes with hit_rank 1; mrr5 is the mean of
//!   1/hit_rank over all probes (0 when there is no hit).

use anyhow::{anyhow, bail, Context, Result};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::search::SearchEngine;
use crate::storage::Storage;

const DEFAULT_PROBES: &str = include_str!("../../benches/probe/probes.json");
const PROBES_SCHEMA: &str = "csr-probes/1";
const SCORECARD_SCHEMA: &str = "csr-probe-scorecard/1";
const WARMUP_QUERY: &str = "throwaway warm-up query for the probe client";
/// The MCP default for `min_score` (src/mcp/mod.rs, csr_reflect_on_past).
const MCP_MIN_SCORE: f32 = 0.3;
/// Generous lock budget: a daemon publishing the index must not make the
/// probe fall back to a rebuild.
const INDEX_LOAD_BUDGET: Duration = Duration::from_secs(60);

/// Inputs of one probe run.
pub struct ProbeConfig {
    pub db_path: PathBuf,
    pub probes: Option<PathBuf>,
    pub audit: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub dump: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ProbeFile {
    schema: String,
    limit: usize,
    probes: Vec<ProbeSpec>,
}

#[derive(Debug, Deserialize)]
struct ProbeSpec {
    id: u32,
    query: String,
    must: Vec<String>,
    #[serde(default)]
    must_not: Vec<String>,
    /// Sessions (conversation ids) that hold the answer. Optional: a probe
    /// without it has no session-level column.
    #[serde(default)]
    expect_sessions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuditFile {
    queries: Vec<String>,
}

struct CompiledProbe {
    id: u32,
    query: String,
    must: Vec<Regex>,
    must_not: Vec<Regex>,
    expect_sessions: Vec<String>,
}

fn compile(patterns: &[String]) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|p| {
            RegexBuilder::new(p)
                .case_insensitive(true)
                .build()
                .with_context(|| format!("bad probe regex {p:?}"))
        })
        .collect()
}

fn load_probes(text: &str) -> Result<(usize, Vec<CompiledProbe>)> {
    let file: ProbeFile = serde_json::from_str(text).context("parse probes json")?;
    if file.schema != PROBES_SCHEMA {
        bail!(
            "unexpected probes schema {:?}, expected {PROBES_SCHEMA}",
            file.schema
        );
    }
    let mut probes = file
        .probes
        .into_iter()
        .map(|p| {
            Ok(CompiledProbe {
                id: p.id,
                query: p.query,
                must: compile(&p.must)?,
                must_not: compile(&p.must_not)?,
                expect_sessions: p.expect_sessions,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    probes.sort_by_key(|p| p.id);
    Ok((file.limit, probes))
}

/// `(rank, block text)` for every top-level `<r rank="N">` block in the response.
///
/// A result excerpt can echo an earlier CSR response, so `<r rank=` tags also
/// occur as text inside a block. Those are not boundaries. The top-level
/// blocks are the ones `<summary count="N">` announces: walk ranks N..1 and
/// take, for each rank, its last tag that lies before the start already
/// chosen for rank k+1. Without a summary, N is the highest rank seen.
fn split_blocks(text: &str) -> Vec<(usize, &str)> {
    let marker = Regex::new(r#"<r rank="(\d+)""#).expect("static regex");
    let found: Vec<(usize, usize)> = marker
        .captures_iter(text)
        .filter_map(|c| {
            let whole = c.get(0)?;
            let n = c.get(1)?.as_str().parse::<usize>().ok()?;
            Some((n, whole.start()))
        })
        .collect();
    let Some(max_rank) = found.iter().map(|(n, _)| *n).max() else {
        return Vec::new();
    };
    let top = summary_count(text).map_or(max_rank, |c| c.min(max_rank));
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut limit = text.len() + 1;
    for k in (1..=top).rev() {
        if let Some(pos) = found
            .iter()
            .filter(|(n, pos)| *n == k && *pos < limit)
            .map(|(_, pos)| *pos)
            .next_back()
        {
            limit = pos;
            starts.push((k, pos));
        }
    }
    starts.reverse();
    starts
        .iter()
        .enumerate()
        .map(|(i, (n, start))| {
            let end = starts.get(i + 1).map_or(text.len(), |next| next.1);
            (*n, &text[*start..end])
        })
        .collect()
}

/// The `count` of the first `<summary count="N"`: the top-level one, since the
/// renderer writes it before any result and escapes everything it echoes.
fn summary_count(text: &str) -> Option<usize> {
    let marker = Regex::new(r#"<summary count="(\d+)""#).expect("static regex");
    marker.captures(text)?.get(1)?.as_str().parse().ok()
}

/// The session a result block belongs to: its `session` attribute, else its
/// `<cid>` (the conversation id, which is the session when no episode channel
/// ran).
fn block_session(block: &str) -> Option<String> {
    let head = block.split('>').next().unwrap_or("");
    if let Some(pos) = head.find("session=\"") {
        let rest = &head[pos + "session=\"".len()..];
        return rest.split('"').next().map(str::to_string);
    }
    tag_text(block, "cid")
}

/// The text of `<tag>` in a result block. The renderer writes `cid`/`id`
/// after the excerpt, and an excerpt may echo another response that carries
/// its own `<cid>`/`<id>`, so read only what follows the last `</excerpt>`.
fn tag_text(block: &str, tag: &str) -> Option<String> {
    let tail = block
        .rfind("</excerpt>")
        .map_or(block, |at| &block[at + "</excerpt>".len()..]);
    let open = format!("<{tag}>");
    let start = tail.find(&open)? + open.len();
    let end = tail[start..].find(&format!("</{tag}>"))?;
    Some(tail[start..start + end].to_string())
}

fn block_eid(block: &str) -> Option<String> {
    let head = block.split('>').next().unwrap_or("");
    let pos = head.find("eid=\"")?;
    head[pos + "eid=\"".len()..]
        .split('"')
        .next()
        .map(str::to_string)
}

/// Smallest block rank (<= `limit`) whose session is in `expect`; `None` when
/// `expect` is empty or no block matches.
pub fn session_hit_rank(text: &str, expect: &[String], limit: usize) -> Option<usize> {
    if expect.is_empty() {
        return None;
    }
    split_blocks(text)
        .into_iter()
        .filter(|(n, _)| *n <= limit)
        .find(|(_, block)| block_session(block).is_some_and(|s| expect.contains(&s)))
        .map(|(n, _)| n)
}

/// Per-block shown identity for the ndjson dump.
fn dump_blocks(text: &str) -> Vec<Value> {
    split_blocks(text)
        .into_iter()
        .map(|(n, block)| {
            json!({
                "rank": n,
                "cid": tag_text(block, "cid"),
                "id": tag_text(block, "id"),
                "eid": block_eid(block),
                "session": block_session(block),
            })
        })
        .collect()
}

fn is_hit(block: &str, must: &[Regex], must_not: &[Regex]) -> bool {
    must.iter().any(|r| r.is_match(block)) && !must_not.iter().any(|r| r.is_match(block))
}

/// The smallest rank `<= limit` whose block is a hit, if any.
pub fn judge(
    response_xml: &str,
    must: &[Regex],
    must_not: &[Regex],
    limit: usize,
) -> Option<usize> {
    split_blocks(response_xml)
        .into_iter()
        .filter(|(n, block)| *n <= limit && is_hit(block, must, must_not))
        .map(|(n, _)| n)
        .min()
}

/// Nearest-rank percentile of an ascending-sorted slice; `p` is 0..=100.
/// Empty input gives 0.0.
pub fn nearest_rank_percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted_ms.len() as f64).ceil() as usize;
    sorted_ms[rank.max(1).min(sorted_ms.len()) - 1]
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn pct_json(values: &[f64], p: f64) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    json!(round3(nearest_rank_percentile(&sorted, p)))
}

/// `(rank1 count, ids with hit_rank 1, mrr)` over `(id, hit_rank)` pairs.
fn summarize(ranks: &[(u32, Option<usize>)]) -> (usize, Vec<u32>, f64) {
    let ids: Vec<u32> = ranks
        .iter()
        .filter(|(_, r)| *r == Some(1))
        .map(|(id, _)| *id)
        .collect();
    let mrr = if ranks.is_empty() {
        0.0
    } else {
        ranks
            .iter()
            .map(|(_, r)| r.map_or(0.0, |n| 1.0 / n as f64))
            .sum::<f64>()
            / ranks.len() as f64
    };
    (ids.len(), ids, (mrr * 1e6).round() / 1e6)
}

fn rank1_block(text: &str) -> Option<&str> {
    split_blocks(text)
        .into_iter()
        .find(|(n, _)| *n == 1)
        .map(|(_, b)| b)
}

/// First 12 hex of the sha256 of the rank-1 block; None without a block.
fn top1_fingerprint(text: &str) -> Option<String> {
    let block = rank1_block(text)?;
    Some(hex(&Sha256::digest(block.as_bytes()))[..12].to_string())
}

fn rank1_score(text: &str) -> Option<f64> {
    let block = rank1_block(text)?;
    let open = block.find("<s")?;
    let rest = &block[open..];
    let gt = rest.find('>')?;
    let rest = &rest[gt + 1..];
    let end = rest.find("</s>")?;
    let body = &rest[..end];
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    body.parse().ok()
}

struct ProbeRow {
    id: u32,
    hit_rank: Option<usize>,
    /// `Some` only for a probe that carries `expect_sessions`.
    session_scored: bool,
    session_hit_rank: Option<usize>,
    rank1_score: Option<f64>,
    /// The chunk id (`<id>`) shown at rank 1.
    rank1_id: Option<String>,
    wall_ms: f64,
}

struct AuditRow {
    wall_ms: f64,
    fingerprint: Option<String>,
    /// First 12 hex of the sha256 of the query text: the key `probe_ab
    /// compare` aligns two scorecards on (never the position).
    query_key: String,
    /// The chunk id (`<id>`) shown at rank 1.
    top1_id: Option<String>,
}

/// First 12 hex of the sha256 of a query, the alignment key of an audit row.
fn query_key(query: &str) -> String {
    hex(&Sha256::digest(query.as_bytes()))[..12].to_string()
}

/// The chunk id shown in the rank-1 block.
fn top1_id(text: &str) -> Option<String> {
    tag_text(rank1_block(text)?, "id")
}

struct CardInputs<'a> {
    binary_path: String,
    binary_sha256: String,
    git_sha: &'a str,
    git_dirty: &'a str,
    db_path: String,
    chunk_count: usize,
    index_chunk_count: usize,
    started_at: String,
    limit: usize,
    cold_start_ms: f64,
    peak_rss_bytes: u64,
    errors: usize,
    probes: &'a [ProbeRow],
    audit: Option<&'a [AuditRow]>,
}

fn build_scorecard(i: &CardInputs) -> Value {
    let ranks: Vec<(u32, Option<usize>)> = i.probes.iter().map(|p| (p.id, p.hit_rank)).collect();
    let (rank1, rank1_ids, mrr5) = summarize(&ranks);
    let session_ranks: Vec<(u32, Option<usize>)> = i
        .probes
        .iter()
        .filter(|p| p.session_scored)
        .map(|p| (p.id, p.session_hit_rank))
        .collect();
    let (session_rank1, session_rank1_ids, session_mrr5) = summarize(&session_ranks);
    let per_probe: Vec<Value> = i
        .probes
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "hit_rank": p.hit_rank,
                "session_hit_rank": p.session_hit_rank,
                "rank1_score": p.rank1_score,
                "rank1_id": p.rank1_id,
                "wall_ms": round3(p.wall_ms),
            })
        })
        .collect();
    let mut walls: Vec<f64> = i.probes.iter().map(|p| p.wall_ms).collect();
    let audit = i.audit.map(|rows| {
        let aw: Vec<f64> = rows.iter().map(|r| r.wall_ms).collect();
        walls.extend(&aw);
        json!({
            "n": rows.len(),
            "empty": rows.iter().filter(|r| r.fingerprint.is_none()).count(),
            "p50_ms": pct_json(&aw, 50.0),
            "p95_ms": pct_json(&aw, 95.0),
            "top1_fp": rows.iter().map(|r| r.fingerprint.clone()).collect::<Vec<_>>(),
            "query_keys": rows.iter().map(|r| r.query_key.clone()).collect::<Vec<_>>(),
            "top1_ids": rows.iter().map(|r| r.top1_id.clone()).collect::<Vec<_>>(),
        })
    });
    json!({
        "schema": SCORECARD_SCHEMA,
        "side": "B",
        "binary": {
            "path": i.binary_path,
            "version": format!("csr-engine {}", env!("CARGO_PKG_VERSION")),
            "sha256": i.binary_sha256,
            "git_sha": i.git_sha,
            "git_dirty": i.git_dirty,
        },
        "db": {
            "path": i.db_path,
            "chunk_count": i.chunk_count,
            "index_chunk_count": i.index_chunk_count,
        },
        "started_at": i.started_at,
        "limit": i.limit,
        "cold_start_ms": round3(i.cold_start_ms),
        "peak_rss_bytes": i.peak_rss_bytes,
        "errors": i.errors,
        "probes": {
            "n": i.probes.len(),
            "rank1": rank1,
            "rank1_ids": rank1_ids,
            "mrr5": mrr5,
            "session_n": session_ranks.len(),
            "session_rank1": session_rank1,
            "session_rank1_ids": session_rank1_ids,
            "session_mrr5": session_mrr5,
            "per_probe": per_probe,
        },
        "audit": audit,
        "latency": {
            "n": walls.len(),
            "p50_ms": pct_json(&walls, 50.0),
            "p95_ms": pct_json(&walls, 95.0),
        },
        "cross_encoder": cross_encoder_json(
            crate::search::cross_encoder::RetrievalPolicy::from_env(),
            crate::search::cross_encoder::ce_stats(),
        ),
    })
}

/// The `cross_encoder` block of the run JSON: the retrieval policy in force
/// and the process-wide cross-encoder counters at the time the card is built.
fn cross_encoder_json(
    policy: crate::search::cross_encoder::RetrievalPolicy,
    stats: crate::search::cross_encoder::CeStats,
) -> Value {
    let policy = match policy {
        crate::search::cross_encoder::RetrievalPolicy::Legacy => "legacy",
        crate::search::cross_encoder::RetrievalPolicy::CrossEncoder => "cross_encoder",
    };
    json!({
        "policy": policy,
        "loaded": stats.loaded,
        "calls": stats.calls,
        "pairs": stats.pairs,
        "total_ms": stats.total_ms,
    })
}

/// Peak resident set size of this process in bytes.
fn peak_rss_bytes() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: `getrusage` fills the zeroed struct we hand it.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
        if rc != 0 {
            return 0;
        }
        let raw = usage.ru_maxrss.max(0) as u64;
        // macOS reports bytes, Linux kilobytes.
        if cfg!(target_os = "macos") {
            raw
        } else {
            raw * 1024
        }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_of_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// One call through the MCP search function. An error is returned to the
/// caller as text so a single failing query never aborts the run.
async fn call(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    limit: usize,
) -> (String, f64, Option<String>) {
    let t = Instant::now();
    let result = crate::mcp::tools::reflect_on_past(
        storage,
        embeddings,
        search,
        query,
        limit,
        MCP_MIN_SCORE,
        None,
    )
    .await;
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    match result {
        Ok(text) => (text, ms, None),
        Err(e) => (String::new(), ms, Some(format!("{e:#}"))),
    }
}

fn dump_line(
    dump: &mut Option<std::io::BufWriter<std::fs::File>>,
    kind: &str,
    key: (&str, u64),
    wall_ms: f64,
    response: &str,
    error: Option<&str>,
) -> Result<()> {
    if let Some(w) = dump.as_mut() {
        let mut line = json!({
            "kind": kind,
            "wall_ms": round3(wall_ms),
            "response": response,
            "blocks": dump_blocks(response),
        });
        line[key.0] = json!(key.1);
        if let Some(e) = error {
            line["error"] = json!(e);
        }
        writeln!(w, "{line}")?;
    }
    Ok(())
}

/// Re-score a stored `--dump` ndjson with the current splitter and judge, so
/// a fixed scorer can restate a number without re-running the retrieval that
/// produced the responses. Reads each `kind: "probe"` record's `response`.
pub fn rescore_dump(dump_ndjson: &str, probes_json: &str) -> Result<Value> {
    let (limit, probes) = load_probes(probes_json)?;
    let mut responses: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for line in dump_ndjson.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).context("parse dump line")?;
        if rec["kind"] != "probe" {
            continue;
        }
        let id = rec["id"].as_u64().context("probe record without id")? as u32;
        let text = rec["response"].as_str().unwrap_or_default().to_string();
        responses.insert(id, text);
    }
    let mut ranks = Vec::with_capacity(probes.len());
    let mut per_probe = Vec::with_capacity(probes.len());
    for p in &probes {
        let text = responses
            .get(&p.id)
            .ok_or_else(|| anyhow!("dump has no response for probe {}", p.id))?;
        let hit_rank = judge(text, &p.must, &p.must_not, limit);
        ranks.push((p.id, hit_rank));
        per_probe.push(json!({
            "id": p.id,
            "hit_rank": hit_rank,
            "session_hit_rank": session_hit_rank(text, &p.expect_sessions, limit),
            "rank1_id": top1_id(text),
            "blocks": split_blocks(text).len(),
        }));
    }
    let (rank1, rank1_ids, mrr5) = summarize(&ranks);
    Ok(json!({
        "rank1": rank1,
        "rank1_ids": rank1_ids,
        "mrr5": mrr5,
        "per_probe": per_probe,
    }))
}

/// Run the probe suite against the live database and return the scorecard.
pub async fn run_probe(cfg: ProbeConfig) -> Result<Value> {
    let t_start = Instant::now();
    let started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let probes_text = match &cfg.probes {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("read probes {}", path.display()))?,
        None => DEFAULT_PROBES.to_string(),
    };
    let (limit, probes) = load_probes(&probes_text)?;
    let audit_queries = match &cfg.audit {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("read audit {}", path.display()))?;
            let file: AuditFile = serde_json::from_str(&text).context("parse audit json")?;
            Some(file.queries)
        }
        None => None,
    };

    let db_dir = cfg
        .db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let storage = Arc::new(
        Storage::open_read_only(&cfg.db_path)
            .with_context(|| format!("open {} read-only", cfg.db_path.display()))?,
    );
    let chunk_count = storage.count_chunk_embeddings()?;
    let reflection_count = storage.count_reflection_embeddings()?;
    let embeddings = Arc::new(EmbeddingEngine::new()?.with_probe_dir(&db_dir));
    embeddings.warm()?;

    let index_dir = db_dir.join("index-v3");
    let engine = SearchEngine::load_from_disk_with_budget(
        &index_dir,
        chunk_count,
        reflection_count,
        INDEX_LOAD_BUDGET,
    )?
    .ok_or_else(|| {
        anyhow!(
            "no usable HNSW cache in {}; refusing to rebuild (read-only probe)",
            index_dir.display()
        )
    })?;
    let index_chunk_count = engine.chunk_count();
    let search = Arc::new(RwLock::new(engine));

    // One throwaway query so first-touch costs stay out of the timed calls.
    let _ = call(&storage, &embeddings, &search, WARMUP_QUERY, limit).await;
    let cold_start_ms = t_start.elapsed().as_secs_f64() * 1000.0;

    let mut dump = match &cfg.dump {
        Some(path) => Some(std::io::BufWriter::new(
            std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?,
        )),
        None => None,
    };

    let mut errors = 0usize;
    let mut probe_rows = Vec::with_capacity(probes.len());
    for p in &probes {
        let (text, ms, err) = call(&storage, &embeddings, &search, &p.query, limit).await;
        if let Some(e) = &err {
            errors += 1;
            eprintln!("probe {} failed: {e}", p.id);
        }
        dump_line(
            &mut dump,
            "probe",
            ("id", p.id.into()),
            ms,
            &text,
            err.as_deref(),
        )?;
        probe_rows.push(ProbeRow {
            id: p.id,
            hit_rank: judge(&text, &p.must, &p.must_not, limit),
            session_scored: !p.expect_sessions.is_empty(),
            session_hit_rank: session_hit_rank(&text, &p.expect_sessions, limit),
            rank1_score: rank1_score(&text),
            rank1_id: top1_id(&text),
            wall_ms: ms,
        });
    }

    let audit_rows = match &audit_queries {
        Some(queries) => {
            let mut rows = Vec::with_capacity(queries.len());
            for (idx, q) in queries.iter().enumerate() {
                let (text, ms, err) = call(&storage, &embeddings, &search, q, limit).await;
                if let Some(e) = &err {
                    errors += 1;
                    eprintln!("audit query {idx} failed: {e}");
                }
                dump_line(
                    &mut dump,
                    "audit",
                    ("idx", idx as u64),
                    ms,
                    &text,
                    err.as_deref(),
                )?;
                rows.push(AuditRow {
                    wall_ms: ms,
                    fingerprint: top1_fingerprint(&text),
                    query_key: query_key(q),
                    top1_id: top1_id(&text),
                });
            }
            Some(rows)
        }
        None => None,
    };
    if let Some(w) = dump.as_mut() {
        w.flush()?;
    }

    let exe = std::env::current_exe().context("locate own binary")?;
    let card = build_scorecard(&CardInputs {
        binary_path: exe.display().to_string(),
        binary_sha256: sha256_of_file(&exe)?,
        git_sha: env!("CSR_BUILD_GIT_SHA"),
        git_dirty: env!("CSR_BUILD_GIT_DIRTY"),
        db_path: cfg.db_path.display().to_string(),
        chunk_count,
        index_chunk_count,
        started_at,
        limit,
        cold_start_ms,
        peak_rss_bytes: peak_rss_bytes(),
        errors,
        probes: &probe_rows,
        audit: audit_rows.as_deref(),
    });

    if let Some(path) = &cfg.out {
        let mut text = serde_json::to_string_pretty(&card)?;
        text.push('\n');
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(patterns: &[&str]) -> Vec<Regex> {
        compile(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    const XML: &str = r#"<search><r rank="1"><s>0.61</s>unrelated text</r><r rank="2"><s>0.58</s>Admin Merge works</r><r rank="3">admin merge but unmergeable</r></search>"#;

    #[test]
    fn judge_finds_the_hit_at_rank_two_case_insensitively() {
        let must = res(&["admin merge"]);
        assert_eq!(judge(XML, &must, &[], 5), Some(2));
        assert_eq!(judge(XML, &must, &[], 1), None);
    }

    #[test]
    fn judge_must_not_vetoes_a_block() {
        let must = res(&["admin merge"]);
        let veto = res(&["works"]);
        // rank 2 is vetoed, rank 3 matches `must` and is not vetoed by "works".
        assert_eq!(judge(XML, &must, &veto, 5), Some(3));
        let veto_all = res(&["works", "unmergeable"]);
        assert_eq!(judge(XML, &must, &veto_all, 5), None);
    }

    #[test]
    fn judge_without_rank_blocks_is_none() {
        let must = res(&["admin"]);
        assert_eq!(
            judge("admin merge but no result blocks", &must, &[], 5),
            None
        );
        assert_eq!(judge("", &must, &[], 5), None);
    }

    #[test]
    fn judge_scores_a_truncated_last_block() {
        let must = res(&["admin"]);
        let cut = r#"<r rank="1">nothing</r><r rank="2"><s>0.5</s>admin merg"#;
        assert_eq!(judge(cut, &must, &[], 5), Some(2));
        // A marker cut before its digits is not a block.
        assert_eq!(judge(r#"admin <r rank=""#, &must, &[], 5), None);
    }

    #[test]
    fn mrr_and_rank1_math() {
        let ranks = [(1, Some(1)), (2, Some(2)), (3, None), (4, Some(4))];
        let (n, ids, mrr) = summarize(&ranks);
        assert_eq!((n, ids), (1, vec![1]));
        assert!((mrr - (1.0 + 0.5 + 0.0 + 0.25) / 4.0).abs() < 1e-9);
        assert_eq!(summarize(&[]).2, 0.0);
    }

    #[test]
    fn nearest_rank_percentile_matches_the_definition() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(nearest_rank_percentile(&v, 50.0), 5.0);
        assert_eq!(nearest_rank_percentile(&v, 95.0), 10.0);
        assert_eq!(nearest_rank_percentile(&v, 10.0), 1.0);
        assert_eq!(nearest_rank_percentile(&[7.0], 95.0), 7.0);
        assert_eq!(nearest_rank_percentile(&[], 95.0), 0.0);
    }

    #[test]
    fn bundled_probes_parse_to_twelve_with_compiling_regexes() {
        let (limit, probes) = load_probes(DEFAULT_PROBES).unwrap();
        assert_eq!(limit, 5);
        assert_eq!(probes.len(), 12);
        assert!(probes.iter().all(|p| !p.must.is_empty()));
        let ids: Vec<u32> = probes.iter().map(|p| p.id).collect();
        assert_eq!(ids, (1..=12).collect::<Vec<u32>>());
    }

    #[test]
    fn rank1_helpers_read_the_first_block() {
        assert_eq!(rank1_score(XML), Some(0.61));
        let fp = top1_fingerprint(XML).unwrap();
        assert_eq!(fp.len(), 12);
        assert_eq!(top1_fingerprint("no blocks"), None);
    }

    #[test]
    fn probe_session_hit_rank_reads_session_attr() {
        let xml = r#"<search><r rank="1" session="s-a"><cid>c-a</cid></r><r rank="2" session="s-b" eid="ep:s-b:3"><cid>s-b</cid></r><r rank="3"><cid>s-c</cid></r></search>"#;
        let want = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(session_hit_rank(xml, &want(&["s-b"]), 5), Some(2));
        assert_eq!(session_hit_rank(xml, &want(&["s-b"]), 1), None);
        // The cid stands in for a block with no session attribute.
        assert_eq!(session_hit_rank(xml, &want(&["s-c"]), 5), Some(3));
        // The cid of a block that does carry a session attribute does not count.
        assert_eq!(session_hit_rank(xml, &want(&["c-a"]), 5), None);
        assert_eq!(session_hit_rank(xml, &[], 5), None);
        let blocks = dump_blocks(xml);
        assert_eq!(blocks[1]["eid"], "ep:s-b:3");
        assert_eq!(blocks[1]["session"], "s-b");
        assert_eq!(blocks[0]["cid"], "c-a");
        assert!(blocks[2]["eid"].is_null());
    }

    #[test]
    fn probes_without_expect_sessions_still_load() {
        let (_, probes) = load_probes(DEFAULT_PROBES).unwrap();
        assert_eq!(probes.len(), 12);
        assert!(probes.iter().all(|p| p.expect_sessions.is_empty()));
        let with = r#"{"schema":"PLACEHOLDER","limit":5,"probes":[{"id":1,"query":"q","must":["x"],"expect_sessions":["s1"]}]}"#
            .replace("PLACEHOLDER", PROBES_SCHEMA);
        let (_, loaded) = load_probes(&with).unwrap();
        assert_eq!(loaded[0].expect_sessions, vec!["s1".to_string()]);
    }

    #[test]
    fn scorecard_has_every_schema_key() {
        let probes = [
            ProbeRow {
                id: 1,
                hit_rank: Some(1),
                session_scored: true,
                session_hit_rank: Some(2),
                rank1_score: Some(0.7),
                rank1_id: None,
                wall_ms: 10.0,
            },
            ProbeRow {
                id: 2,
                hit_rank: None,
                session_scored: false,
                session_hit_rank: None,
                rank1_score: None,
                rank1_id: None,
                wall_ms: 20.0,
            },
        ];
        let audit = [AuditRow {
            wall_ms: 5.0,
            fingerprint: None,
            query_key: query_key("q"),
            top1_id: None,
        }];
        let card = build_scorecard(&CardInputs {
            binary_path: "/x".into(),
            binary_sha256: "abc".into(),
            git_sha: "deadbeef",
            git_dirty: "false",
            db_path: "/db".into(),
            chunk_count: 3,
            index_chunk_count: 3,
            started_at: "2026-10-26T00:00:00Z".into(),
            limit: 5,
            cold_start_ms: 1.0,
            peak_rss_bytes: 1,
            errors: 0,
            probes: &probes,
            audit: Some(&audit),
        });
        for key in [
            "schema",
            "side",
            "binary",
            "db",
            "started_at",
            "limit",
            "cold_start_ms",
            "peak_rss_bytes",
            "probes",
            "audit",
            "latency",
            "cross_encoder",
        ] {
            assert!(card.get(key).is_some(), "missing {key}");
        }
        assert_eq!(card["side"], "B");
        assert_eq!(card["schema"], SCORECARD_SCHEMA);
        for key in ["path", "version", "sha256"] {
            assert!(card["binary"].get(key).is_some(), "binary.{key}");
        }
        for key in ["path", "chunk_count"] {
            assert!(card["db"].get(key).is_some(), "db.{key}");
        }
        for key in [
            "n",
            "rank1",
            "rank1_ids",
            "mrr5",
            "session_rank1",
            "session_rank1_ids",
            "session_mrr5",
            "per_probe",
        ] {
            assert!(card["probes"].get(key).is_some(), "probes.{key}");
        }
        for key in [
            "n",
            "empty",
            "p50_ms",
            "p95_ms",
            "top1_fp",
            "query_keys",
            "top1_ids",
        ] {
            assert!(card["audit"].get(key).is_some(), "audit.{key}");
        }
        for key in ["n", "p50_ms", "p95_ms"] {
            assert!(card["latency"].get(key).is_some(), "latency.{key}");
        }
        for key in ["policy", "loaded", "calls", "pairs", "total_ms"] {
            assert!(
                card["cross_encoder"].get(key).is_some(),
                "cross_encoder.{key}"
            );
        }
        assert_eq!(card["probes"]["rank1"], 1);
        assert_eq!(card["latency"]["n"], 3);
        let no_audit = build_scorecard(&CardInputs {
            binary_path: String::new(),
            binary_sha256: String::new(),
            git_sha: "",
            git_dirty: "",
            db_path: String::new(),
            chunk_count: 0,
            index_chunk_count: 0,
            started_at: String::new(),
            limit: 5,
            cold_start_ms: 0.0,
            peak_rss_bytes: 0,
            errors: 0,
            probes: &probes,
            audit: None,
        });
        assert!(no_audit["audit"].is_null());
    }

    #[test]
    fn cross_encoder_block_reports_policy_and_counters() {
        use crate::search::cross_encoder::{CeStats, RetrievalPolicy};
        let stats = CeStats {
            loaded: true,
            calls: 2,
            pairs: 48,
            total_ms: 910,
        };
        let on = cross_encoder_json(RetrievalPolicy::CrossEncoder, stats);
        assert_eq!(on["policy"], "cross_encoder");
        assert_eq!(on["loaded"], true);
        assert_eq!(on["calls"], 2);
        assert_eq!(on["pairs"], 48);
        assert_eq!(on["total_ms"], 910);
        let off = cross_encoder_json(RetrievalPolicy::Legacy, CeStats::default());
        assert_eq!(off["policy"], "legacy");
        assert_eq!(off["loaded"], false);
        assert_eq!(off["pairs"], 0);
    }
    /// A real-shaped response: 3 top-level blocks, and block 1's excerpt echoes
    /// an earlier response that carries its own `<r rank="1">` and `<r rank="2">`.
    fn echoing_response() -> String {
        let echoed = r#"=== earlier run
<search>
  <summary count="2" relevance="high">
  <results>
    <r rank="1">
      <excerpt><![CDATA[echoed top hit: admin merge works]]></excerpt>
      <cid>echo-cid</cid>
      <id>echo-id-1</id>
    </r>
    <r rank="2">
      <excerpt><![CDATA[echoed second]]></excerpt>
      <cid>echo-cid-2</cid>
      <id>echo-id-2</id>
    </r>
  </results>
</search>"#;
        format!(
            r#"RESULTS
<search>
  <summary count="3" relevance="partial">
    <preview>p</preview>
  </summary>
  <results>
    <r rank="1">
      <excerpt><![CDATA[{echoed}]]></excerpt>
      <cid>real-cid-1</cid>
      <id>real-id-1</id>
    </r>
    <r rank="2">
      <excerpt><![CDATA[plain second]]></excerpt>
      <cid>real-cid-2</cid>
      <id>real-id-2</id>
    </r>
    <r rank="3">
      <excerpt><![CDATA[plain third]]></excerpt>
      <cid>real-cid-3</cid>
      <id>real-id-3</id>
    </r>
  </results>
</search>"#
        )
    }

    #[test]
    fn nested_echoed_rank_tags_do_not_split_a_block() {
        let text = echoing_response();
        let blocks = split_blocks(&text);
        let ranks: Vec<usize> = blocks.iter().map(|(n, _)| *n).collect();
        assert_eq!(ranks, vec![1, 2, 3], "echoed tags must not add blocks");
        // The echo stays inside block 1 as text.
        assert!(blocks[0].1.contains("echoed top hit"));
        assert!(blocks[0].1.contains("echoed second"));
        assert!(!blocks[1].1.contains("echoed"));
        // Identity comes from the real tags after the excerpt, not the echo.
        assert_eq!(tag_text(blocks[0].1, "id").as_deref(), Some("real-id-1"));
        assert_eq!(tag_text(blocks[0].1, "cid").as_deref(), Some("real-cid-1"));
        assert_eq!(top1_id(&text).as_deref(), Some("real-id-1"));
        let dumped = dump_blocks(&text);
        assert_eq!(dumped.len(), 3);
        assert_eq!(dumped[0]["id"], "real-id-1");
    }

    #[test]
    fn echoed_rank_one_cannot_pose_as_the_hit_at_rank_one() {
        // Only the echoed text inside block 1 says "admin merge works", and the
        // real rank-2 block holds nothing: the probe must see the hit at rank 1
        // (the echo is part of block 1), never a phantom extra block.
        let text = echoing_response();
        let must = res(&["plain third"]);
        assert_eq!(judge(&text, &must, &[], 5), Some(3));
        let must = res(&["echoed top hit"]);
        assert_eq!(judge(&text, &must, &[], 5), Some(1));
        // The fingerprint is the whole real block 1, not the first echoed tag.
        let fp = top1_fingerprint(&text).unwrap();
        let whole = split_blocks(&text)[0].1;
        assert_eq!(fp, hex(&Sha256::digest(whole.as_bytes()))[..12]);
    }

    #[test]
    fn responses_without_echo_split_exactly_as_before() {
        let blocks = split_blocks(XML);
        assert_eq!(
            blocks.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // A summary count larger than what is shown does not invent blocks.
        let short = r#"<summary count="9"><r rank="1">a</r><r rank="2">b</r>"#;
        assert_eq!(split_blocks(short).len(), 2);
    }

    #[test]
    fn audit_row_keys_do_not_depend_on_position() {
        assert_eq!(query_key("same query"), query_key("same query"));
        assert_ne!(query_key("a"), query_key("b"));
        assert_eq!(query_key("a").len(), 12);
    }
    #[test]
    fn rescore_dump_scores_stored_responses_with_the_fixed_splitter() {
        let probes = format!(
            r#"{{"schema":"{PROBES_SCHEMA}","limit":5,"probes":[{{"id":1,"query":"q1","must":["echoed top hit"]}},{{"id":2,"query":"q2","must":["plain third"]}}]}}"#
        );
        let echo = echoing_response();
        let dump = [
            json!({"kind":"probe","id":1,"response":echo}),
            json!({"kind":"probe","id":2,"response":echo}),
            json!({"kind":"audit","idx":0,"response":"x"}),
        ]
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        let out = rescore_dump(&dump, &probes).unwrap();
        assert_eq!(out["per_probe"][0]["hit_rank"], 1);
        assert_eq!(out["per_probe"][1]["hit_rank"], 3);
        assert_eq!(out["per_probe"][0]["rank1_id"], "real-id-1");
        assert_eq!(out["per_probe"][0]["blocks"], 3);
        assert_eq!(out["rank1"], 1);
        // A dump that lacks a probe is an error, not a zero.
        let short = json!({"kind":"probe","id":1,"response":echo}).to_string();
        assert!(rescore_dump(&short, &probes).is_err());
    }

    /// Manual: `CSR_RESCORE_DUMP=<dump.ndjson> CSR_RESCORE_PROBES=<probes.json>
    /// cargo test --release --lib rescore_dump_from_env -- --ignored --nocapture`
    /// prints the rescored JSON for a stored dump.
    #[test]
    #[ignore]
    fn rescore_dump_from_env() {
        let dump = std::env::var("CSR_RESCORE_DUMP").expect("CSR_RESCORE_DUMP");
        let probes = std::env::var("CSR_RESCORE_PROBES").expect("CSR_RESCORE_PROBES");
        let out = rescore_dump(
            &std::fs::read_to_string(dump).unwrap(),
            &std::fs::read_to_string(probes).unwrap(),
        )
        .unwrap();
        println!("RESCORED {out}");
    }
}
