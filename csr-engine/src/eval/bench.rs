use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::import::chunk_messages;
use crate::mcp::tools::{reflect_for_bench_with_vec, SearchMode};
use crate::provenance::{ChunkProvenance, Speaker};
use crate::search::SearchEngine;
use crate::storage::Storage;

pub const EMBEDDING_MODEL: &str = "sentence-transformers/all-MiniLM-L6-v2 (FastEmbed, 384-d)";
const PROJECT: &str = "csr-bench";

#[derive(Debug, Clone)]
pub struct BenchConfig {
    pub format: BenchFormat,
    pub data: PathBuf,
    pub queries: Option<PathBuf>,
    pub k: usize,
    pub mode: SearchMode,
    pub out: PathBuf,
    pub drop_abstention: bool,
    pub limit: Option<usize>,
    pub stratify: Option<usize>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BenchFormat {
    Agentmemory,
    Longmemeval,
}

impl BenchFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "agentmemory" => Ok(Self::Agentmemory),
            "longmemeval" => Ok(Self::Longmemeval),
            _ => bail!("unknown --format {value:?}; expected agentmemory or longmemeval"),
        }
    }
}

#[derive(Debug, Clone)]
struct BenchSession {
    id: String,
    timestamp: Option<String>,
    messages: Vec<(Speaker, String)>,
}

#[derive(Debug, Clone)]
struct BenchQuestion {
    id: String,
    kind: String,
    question: String,
    gold_session_ids: Vec<String>,
}

#[derive(Debug)]
struct AgentmemoryCorpus {
    dataset: String,
    input_files: Vec<InputFileReceipt>,
    queries_override: Option<String>,
    sessions: Vec<BenchSession>,
    questions: Vec<BenchQuestion>,
}

#[derive(Debug)]
struct LongMemEvalQuestion {
    id: String,
    kind: String,
    question: String,
    gold_session_ids: Vec<String>,
    sessions: Vec<BenchSession>,
}

#[derive(Debug)]
struct LongMemEvalCorpus {
    questions: Vec<LongMemEvalQuestion>,
    dropped_abstention: usize,
    input_files: Vec<InputFileReceipt>,
}

#[derive(Deserialize)]
struct RawSession {
    id: String,
    timestamp: Option<String>,
    content: String,
}

#[derive(Deserialize)]
struct RawAgentQuestion {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    question: String,
    #[serde(rename = "goldSessionIds")]
    gold_session_ids: Vec<String>,
}

#[derive(Deserialize)]
struct RawTurn {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct RawLongMemEval {
    question_id: String,
    question_type: String,
    question: String,
    answer_session_ids: Vec<String>,
    haystack_dates: Option<Vec<String>>,
    haystack_session_ids: Vec<String>,
    haystack_sessions: Vec<Vec<RawTurn>>,
}

fn read_json_with_receipt<T: for<'de> Deserialize<'de>>(
    path: &Path,
    role: &str,
) -> Result<(T, InputFileReceipt)> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("canonicalizing benchmark input {}", path.display()))?;
    let bytes = fs::read(&canonical)
        .with_context(|| format!("reading benchmark input {}", canonical.display()))?;
    let parsed = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing benchmark input {}", canonical.display()))?;
    let receipt = InputFileReceipt {
        role: role.into(),
        path: canonical.display().to_string(),
        blake3: blake3::hash(&bytes).to_hex().to_string(),
    };
    Ok((parsed, receipt))
}

fn read_agentmemory(data: PathBuf, queries: Option<&Path>) -> Result<AgentmemoryCorpus> {
    let data_is_dir = data.is_dir();
    let (session_path, default_query_path, mut dataset) = if data_is_dir {
        let dataset = data
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("agentmemory")
            .to_string();
        (
            data.join("sessions.json"),
            data.join("queries.json"),
            dataset,
        )
    } else {
        let dataset = data
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("agentmemory")
            .to_string();
        (data.clone(), data.with_file_name("queries.json"), dataset)
    };
    let query_path = queries.unwrap_or(&default_query_path);
    let (raw_sessions, sessions_receipt): (Vec<RawSession>, _) =
        read_json_with_receipt(&session_path, "sessions")?;
    let (raw_questions, queries_receipt): (Vec<RawAgentQuestion>, _) =
        read_json_with_receipt(query_path, "queries")?;
    if !data_is_dir {
        dataset = Path::new(&sessions_receipt.path)
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("agentmemory")
            .to_string();
    }
    let queries_override = queries.map(|_| queries_receipt.path.clone());
    Ok(AgentmemoryCorpus {
        dataset,
        queries_override,
        input_files: vec![sessions_receipt, queries_receipt],
        sessions: raw_sessions
            .into_iter()
            .map(|session| BenchSession {
                id: session.id,
                timestamp: session.timestamp,
                messages: vec![(Speaker::User, session.content)],
            })
            .collect(),
        questions: raw_questions
            .into_iter()
            .map(|question| BenchQuestion {
                id: question.id,
                kind: question.kind,
                question: question.question,
                gold_session_ids: question.gold_session_ids,
            })
            .collect(),
    })
}

const ABSTENTION_TYPES: [&str; 4] = [
    "single-session-user_abs",
    "multi-session_abs",
    "knowledge-update_abs",
    "temporal-reasoning_abs",
];

fn read_longmemeval(
    path: &Path,
    drop_abstention: bool,
    limit: Option<usize>,
) -> Result<LongMemEvalCorpus> {
    let (raw, input_receipt): (Vec<RawLongMemEval>, _) =
        read_json_with_receipt(path, "longmemeval")?;
    let dropped_abstention = raw
        .iter()
        .filter(|row| drop_abstention && ABSTENTION_TYPES.contains(&row.question_type.as_str()))
        .count();
    let mut questions = Vec::new();
    for row in raw {
        if drop_abstention && ABSTENTION_TYPES.contains(&row.question_type.as_str()) {
            continue;
        }
        if row.haystack_session_ids.len() != row.haystack_sessions.len() {
            bail!(
                "LongMemEval row {}: haystack_session_ids ({}) and haystack_sessions ({}) length mismatch",
                row.question_id,
                row.haystack_session_ids.len(),
                row.haystack_sessions.len()
            );
        }
        let dates = row.haystack_dates.unwrap_or_default();
        let sessions = row
            .haystack_session_ids
            .into_iter()
            .zip(row.haystack_sessions)
            .enumerate()
            .map(|(index, (id, turns))| BenchSession {
                id,
                timestamp: dates.get(index).cloned(),
                messages: turns
                    .into_iter()
                    .map(|turn| {
                        let speaker = if turn.role.eq_ignore_ascii_case("user") {
                            Speaker::User
                        } else {
                            Speaker::Assistant
                        };
                        (speaker, turn.content)
                    })
                    .collect(),
            })
            .collect();
        questions.push(LongMemEvalQuestion {
            id: row.question_id,
            kind: row.question_type,
            question: row.question,
            gold_session_ids: row.answer_session_ids,
            sessions,
        });
        if limit.is_some_and(|limit| questions.len() >= limit) {
            break;
        }
    }
    Ok(LongMemEvalCorpus {
        questions,
        dropped_abstention,
        input_files: vec![input_receipt],
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentmemoryScoreRow<'a> {
    question_id: &'a str,
    question_type: &'a str,
    adapter: &'a str,
    k: usize,
    precision_at_k: f64,
    recall_at_k: f64,
    hit: bool,
    top_gold_rank: Option<usize>,
    latency_ms: f64,
}

#[derive(Debug, Clone)]
struct BenchScoreRow {
    question_id: String,
    question_type: String,
    adapter: String,
    k: usize,
    precision_at_k: f64,
    recall_at_k: f64,
    hit: bool,
    top_gold_rank: Option<usize>,
    query_latency_ms: f64,
    latency_ms: f64,
    mrr: f64,
    ndcg_at_10: f64,
}

impl BenchScoreRow {
    fn agentmemory(&self) -> AgentmemoryScoreRow<'_> {
        AgentmemoryScoreRow {
            question_id: &self.question_id,
            question_type: &self.question_type,
            adapter: &self.adapter,
            k: self.k,
            precision_at_k: self.precision_at_k,
            recall_at_k: self.recall_at_k,
            hit: self.hit,
            top_gold_rank: self.top_gold_rank,
            latency_ms: self.latency_ms,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn score_row(
    id: &str,
    kind: &str,
    adapter: &str,
    k: usize,
    ranked: &[&str],
    gold: &[&str],
    query_latency_ms: f64,
    score_latency_ms: f64,
) -> BenchScoreRow {
    let gold: HashSet<&str> = gold.iter().copied().collect();
    let hits = ranked
        .iter()
        .take(k)
        .filter(|id| gold.contains(**id))
        .count();
    let relevances = ranked
        .iter()
        .map(|id| f64::from(gold.contains(*id)))
        .collect::<Vec<_>>();
    let mut ndcg_relevances = relevances.clone();
    ndcg_relevances.resize(10, 0.0);
    let retrieved_gold = relevances
        .iter()
        .filter(|relevance| **relevance > 0.0)
        .count();
    ndcg_relevances.extend(std::iter::repeat_n(
        1.0,
        gold.len().saturating_sub(retrieved_gold),
    ));
    BenchScoreRow {
        question_id: id.into(),
        question_type: kind.into(),
        adapter: adapter.into(),
        k,
        precision_at_k: if k == 0 { 0.0 } else { hits as f64 / k as f64 },
        recall_at_k: if gold.is_empty() {
            0.0
        } else {
            hits as f64 / gold.len() as f64
        },
        hit: hits > 0,
        top_gold_rank: ranked
            .iter()
            .take(k)
            .position(|id| gold.contains(*id))
            .map(|rank| rank + 1),
        query_latency_ms,
        latency_ms: score_latency_ms,
        mrr: super::trained_rerank::mrr(&relevances),
        ndcg_at_10: super::trained_rerank::ndcg_at_k(&ndcg_relevances, 10),
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct MemoStats {
    pub hits: usize,
    pub misses: usize,
}

#[derive(Default)]
struct EmbeddingMemo {
    vectors: HashMap<blake3::Hash, Vec<f32>>,
    stats: MemoStats,
}

impl EmbeddingMemo {
    fn embed_missing<F>(&mut self, texts: &[&str], mut embed: F) -> Result<Vec<Vec<f32>>>
    where
        F: FnMut(&[&str]) -> Result<Vec<Vec<f32>>>,
    {
        let mut missing = Vec::<(blake3::Hash, &str)>::new();
        let mut seen_missing = HashSet::new();
        for text in texts {
            let hash = blake3::hash(text.as_bytes());
            if self.vectors.contains_key(&hash) {
                self.stats.hits += 1;
            } else if seen_missing.insert(hash) {
                missing.push((hash, *text));
            } else {
                self.stats.hits += 1;
            }
        }
        if !missing.is_empty() {
            let inputs = missing.iter().map(|(_, text)| *text).collect::<Vec<_>>();
            let vectors = embed(&inputs)?;
            if vectors.len() != inputs.len() {
                bail!(
                    "embedding engine returned {} vectors for {} texts",
                    vectors.len(),
                    inputs.len()
                );
            }
            self.stats.misses += vectors.len();
            for ((hash, _), vector) in missing.into_iter().zip(vectors) {
                self.vectors.insert(hash, vector);
            }
        }
        Ok(texts
            .iter()
            .map(|text| self.vectors[&blake3::hash(text.as_bytes())].clone())
            .collect())
    }

    fn stats(&self) -> MemoStats {
        self.stats
    }
}

impl MemoStats {
    fn since(self, earlier: Self) -> Self {
        Self {
            hits: self.hits.saturating_sub(earlier.hits),
            misses: self.misses.saturating_sub(earlier.misses),
        }
    }
}

static PROCESS_EMBEDDING_MEMO: LazyLock<Mutex<EmbeddingMemo>> =
    LazyLock::new(|| Mutex::new(EmbeddingMemo::default()));

#[derive(Debug, Serialize)]
pub struct InputFileReceipt {
    pub role: String,
    pub path: String,
    pub blake3: String,
}

#[derive(Debug, Serialize)]
pub struct BenchSummary {
    pub dataset: String,
    pub split: String,
    pub build_commit: String,
    pub build_dirty: bool,
    pub cwd_head: Option<String>,
    pub embedding_model: String,
    pub k: usize,
    pub ranking_depth: usize,
    pub mode: String,
    pub input_files: Vec<InputFileReceipt>,
    pub queries_override: Option<String>,
    pub drop_abstention: bool,
    pub limit: Option<usize>,
    pub stratify: Option<usize>,
    pub scored_question_ids: Vec<String>,
    pub scored_questions: usize,
    pub dropped_abstention: usize,
    pub precision_at_k: f64,
    pub recall_at_k_fraction_of_gold: f64,
    pub recall_any_at_k: f64,
    pub mrr: f64,
    pub ndcg_at_10: f64,
    pub query_latency_p50_ms: f64,
    pub init_inclusive_latency_p50_ms: f64,
    pub metric_definitions: BTreeMap<String, String>,
    pub embedding_memo: MemoStats,
}

fn upper_median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.get(sorted.len() / 2).copied().unwrap_or(0.0)
}

#[allow(clippy::too_many_arguments)]
fn summarize(
    dataset: &str,
    split: &str,
    embedding_model: &str,
    k: usize,
    ranking_depth: usize,
    mode: &str,
    rows: &[BenchScoreRow],
    init_inclusive: &[f64],
    dropped_abstention: usize,
    embedding_memo: MemoStats,
    input_files: Vec<InputFileReceipt>,
    drop_abstention: bool,
    limit: Option<usize>,
    stratify: Option<usize>,
    scored_question_ids: Vec<String>,
    queries_override: Option<String>,
) -> BenchSummary {
    let n = rows.len().max(1) as f64;
    BenchSummary {
        dataset: dataset.into(),
        split: split.into(),
        build_commit: env!("CSR_BUILD_GIT_SHA").into(),
        build_dirty: env!("CSR_BUILD_GIT_DIRTY") == "true",
        cwd_head: cwd_head(),
        embedding_model: embedding_model.into(),
        k,
        ranking_depth,
        mode: mode.into(),
        input_files,
        queries_override,
        drop_abstention,
        limit,
        stratify,
        scored_question_ids,
        scored_questions: rows.len(),
        dropped_abstention,
        precision_at_k: rows.iter().map(|row| row.precision_at_k).sum::<f64>() / n,
        recall_at_k_fraction_of_gold: rows.iter().map(|row| row.recall_at_k).sum::<f64>() / n,
        recall_any_at_k: rows.iter().map(|row| f64::from(row.hit)).sum::<f64>() / n,
        mrr: rows.iter().map(|row| row.mrr).sum::<f64>() / n,
        ndcg_at_10: rows.iter().map(|row| row.ndcg_at_10).sum::<f64>() / n,
        query_latency_p50_ms: upper_median(
            &rows
                .iter()
                .map(|row| row.query_latency_ms)
                .collect::<Vec<_>>(),
        ),
        init_inclusive_latency_p50_ms: upper_median(init_inclusive),
        metric_definitions: BTreeMap::from([
            (format!("P@{k}"), "relevant retrieved sessions divided by K".into()),
            (format!("R@{k}"), "relevant retrieved sessions divided by all dataset gold sessions".into()),
            (format!("recall_any@{k}"), "1 when any dataset gold session appears in top K, else 0 (agentmemory headline metric)".into()),
            (format!("MRR@{ranking_depth}"), format!("reciprocal rank of the first dataset gold session within the top {ranking_depth}")),
            ("NDCG@10".into(), "binary-gain normalized discounted cumulative gain at 10".into()),
            ("query_latency_p50_ms".into(), "upper median of query embedding plus retrieval only".into()),
            ("init_inclusive_latency_p50_ms".into(), "upper median of cold-start indexing plus query; per-question re-init applies only to the longmemeval format".into()),
        ]),
        embedding_memo,
    }
}

async fn embed_contents(engine: Arc<EmbeddingEngine>, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let texts = texts.to_vec();
    tokio::task::spawn_blocking(move || {
        let refs = texts.iter().map(String::as_str).collect::<Vec<_>>();
        let mut memo = PROCESS_EMBEDDING_MEMO
            .lock()
            .map_err(|e| anyhow::anyhow!("embedding memo lock: {e}"))?;
        memo.embed_missing(&refs, |missing| {
            let mut vectors = Vec::with_capacity(missing.len());
            for batch in missing.chunks(64) {
                vectors.extend(engine.embed(batch)?);
            }
            Ok(vectors)
        })
    })
    .await
    .context("embedding worker panicked")?
}

async fn query_vector(engine: Option<&Arc<EmbeddingEngine>>, query: &str) -> Result<Vec<f32>> {
    let Some(engine) = engine else {
        return Ok(vec![0.0; EmbeddingEngine::dimension()]);
    };
    let engine = Arc::clone(engine);
    let query = query.to_string();
    tokio::task::spawn_blocking(move || engine.embed_single(&query))
        .await
        .context("query embedding worker panicked")?
}

async fn index_sessions(
    sessions: &[BenchSession],
    engine: Option<&Arc<EmbeddingEngine>>,
) -> Result<(Arc<Storage>, Arc<RwLock<SearchEngine>>)> {
    let storage = Arc::new(Storage::open_memory()?);
    let mut chunks = Vec::new();
    for session in sessions {
        chunks.extend(chunk_messages(
            &session.id,
            PROJECT,
            session
                .timestamp
                .as_deref()
                .unwrap_or("1970-01-01T00:00:00Z"),
            &session.messages,
            false,
        ));
    }
    let contents = chunks
        .iter()
        .map(|chunk| chunk.content.clone())
        .collect::<Vec<_>>();
    let vectors = if let Some(engine) = engine {
        embed_contents(Arc::clone(engine), &contents).await?
    } else {
        vec![vec![0.0; EmbeddingEngine::dimension()]; chunks.len()]
    };
    let mut search = SearchEngine::new(chunks.len().max(16));
    for (chunk, vector) in chunks.into_iter().zip(vectors) {
        storage.insert_chunk(&chunk, &vector)?;
        storage.insert_chunk_provenance(
            &chunk.id,
            &ChunkProvenance {
                author: chunk.author,
                source_conv_id: chunk.conversation_id.clone(),
                supersedes: None,
            },
        )?;
        search.insert_chunk(chunk.id, vector);
    }
    Ok((storage, Arc::new(RwLock::new(search))))
}

async fn query_store(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    engine: Option<&Arc<EmbeddingEngine>>,
    question: &str,
    k: usize,
    mode: SearchMode,
) -> Result<Vec<String>> {
    let vector = query_vector(engine, question).await?;
    let metric_fetch = k.max(20);
    Ok(
        reflect_for_bench_with_vec(storage, search, &vector, question, metric_fetch, mode)
            .await?
            .into_iter()
            .map(|hit| hit.conversation_id)
            .collect(),
    )
}

fn stratified<T, F>(items: Vec<T>, per_type: Option<usize>, kind: F) -> Vec<T>
where
    F: Fn(&T) -> &str,
{
    let Some(per_type) = per_type else {
        return items;
    };
    let mut buckets = BTreeMap::<String, Vec<T>>::new();
    for item in items {
        buckets
            .entry(kind(&item).to_string())
            .or_default()
            .push(item);
    }
    buckets
        .into_values()
        .flat_map(|items| items.into_iter().take(per_type))
        .collect()
}

fn cwd_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn format_table(summary: &BenchSummary) -> String {
    let published = if summary.k == 5 {
        "| agentmemory `565c238` (published) | LongMemEval-S cleaned, abstention excluded | hybrid (BM25 .4/vector .6/graph 0; reranker off) | — | — | 0.952 | — | — | — | — |\n"
    } else {
        ""
    };
    format!("| System | Dataset | Mode | P@{k} | R@{k} (fraction gold) | recall_any@{k} | MRR@{depth} | NDCG@10 | query p50 | init-inclusive p50 |\n|---|---|---:|---:|---:|---:|---:|---:|---:|---:|\n| CSR `{sha}`{dirty} | {dataset} ({split}) | {mode} | {p:.3} | {r:.3} | {hit:.3} | {mrr:.3} | {ndcg:.3} | {query:.1} ms | {init:.1} ms |\n{published}\nReceipt: dataset={dataset}; split={split}; metric headline=recall_any@{k} (any gold in top K), distinct from fraction-of-gold R@{k}; build_commit={sha}; build_dirty={build_dirty}; embedding={model}; K={k}; ranking_depth={depth}; mode={mode}; scored={n}; abstention_dropped={dropped}.\nagentmemory's 95.2% is recall_any@5 at `565c238`, using 512-character session vectors with graph weight 0.\n",
        k=summary.k, depth=summary.ranking_depth, sha=summary.build_commit, dirty=if summary.build_dirty { " (dirty)" } else { "" }, build_dirty=summary.build_dirty, dataset=summary.dataset, split=summary.split, mode=summary.mode, p=summary.precision_at_k, r=summary.recall_at_k_fraction_of_gold, hit=summary.recall_any_at_k, mrr=summary.mrr, ndcg=summary.ndcg_at_10, query=summary.query_latency_p50_ms, init=summary.init_inclusive_latency_p50_ms, model=summary.embedding_model, n=summary.scored_questions, dropped=summary.dropped_abstention)
}

fn validate_config(config: &BenchConfig) -> Result<()> {
    if config.k == 0 {
        bail!("--k must be a positive integer");
    }
    if config.limit == Some(0) {
        bail!("--limit must be a positive integer");
    }
    if config.stratify == Some(0) {
        bail!("--stratify must be a positive integer");
    }
    Ok(())
}

fn write_outputs(
    out: &Path,
    rows: &[BenchScoreRow],
    summary: &BenchSummary,
    table: &str,
) -> Result<()> {
    fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    let ndjson = rows
        .iter()
        .map(|row| serde_json::to_string(&row.agentmemory()))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n");
    fs::write(out.join("scores.ndjson"), format!("{ndjson}\n"))?;
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(summary)?,
    )?;
    fs::write(out.join("table.md"), table)?;
    Ok(())
}

/// Run a benchmark without touching the user's persistent database.
pub async fn run(config: BenchConfig) -> Result<String> {
    validate_config(&config)?;
    let memo_before = PROCESS_EMBEDDING_MEMO
        .lock()
        .map(|memo| memo.stats())
        .unwrap_or_default();
    let engine = if config.mode == SearchMode::Fts {
        None
    } else {
        Some(Arc::new(EmbeddingEngine::new()?))
    };
    let adapter = format!("csr-{}", config.mode.as_str());
    let mut rows = Vec::new();
    let mut init_inclusive = Vec::new();
    let (dataset, split, dropped, input_files, queries_override) = match config.format {
        BenchFormat::Agentmemory => {
            let mut corpus = read_agentmemory(config.data.clone(), config.queries.as_deref())?;
            corpus.questions = stratified(corpus.questions, config.stratify, |q| &q.kind);
            if let Some(limit) = config.limit {
                corpus.questions.truncate(limit);
            }
            let init_start = Instant::now();
            let (storage, search) = index_sessions(&corpus.sessions, engine.as_ref()).await?;
            let init_ms = init_start.elapsed().as_secs_f64() * 1000.0;
            for q in &corpus.questions {
                let start = Instant::now();
                let ranked = query_store(
                    &storage,
                    &search,
                    engine.as_ref(),
                    &q.question,
                    config.k,
                    config.mode,
                )
                .await?;
                let query_ms = start.elapsed().as_secs_f64() * 1000.0;
                let ranked = ranked.iter().map(String::as_str).collect::<Vec<_>>();
                let gold = q
                    .gold_session_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                rows.push(score_row(
                    &q.id, &q.kind, &adapter, config.k, &ranked, &gold, query_ms, query_ms,
                ));
                init_inclusive.push(init_ms + query_ms);
            }
            (
                corpus.dataset,
                "all".to_string(),
                0,
                corpus.input_files,
                corpus.queries_override,
            )
        }
        BenchFormat::Longmemeval => {
            let mut corpus = read_longmemeval(&config.data, config.drop_abstention, None)?;
            corpus.questions = stratified(corpus.questions, config.stratify, |q| &q.kind);
            if let Some(limit) = config.limit {
                corpus.questions.truncate(limit);
            }
            for q in &corpus.questions {
                let init_start = Instant::now();
                let (storage, search) = index_sessions(&q.sessions, engine.as_ref()).await?;
                let start = Instant::now();
                let ranked = query_store(
                    &storage,
                    &search,
                    engine.as_ref(),
                    &q.question,
                    config.k,
                    config.mode,
                )
                .await?;
                let query_ms = start.elapsed().as_secs_f64() * 1000.0;
                let init_inclusive_ms = init_start.elapsed().as_secs_f64() * 1000.0;
                init_inclusive.push(init_inclusive_ms);
                let ranked = ranked.iter().map(String::as_str).collect::<Vec<_>>();
                let gold = q
                    .gold_session_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                rows.push(score_row(
                    &q.id,
                    &q.kind,
                    &adapter,
                    config.k,
                    &ranked,
                    &gold,
                    query_ms,
                    init_inclusive_ms,
                ));
            }
            let split = if config.drop_abstention {
                "non-abstention"
            } else {
                "all"
            };
            let dataset = config
                .data
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("longmemeval")
                .to_string();
            (
                dataset,
                split.to_string(),
                corpus.dropped_abstention,
                corpus.input_files,
                None,
            )
        }
    };
    if rows.is_empty() {
        bail!("benchmark selected zero scored questions");
    }
    let memo_after = PROCESS_EMBEDDING_MEMO
        .lock()
        .map(|memo| memo.stats())
        .unwrap_or_default();
    let summary = summarize(
        &dataset,
        &split,
        if config.mode == SearchMode::Fts {
            "not used (fts-only); vector model is sentence-transformers/all-MiniLM-L6-v2 (FastEmbed, 384-d)"
        } else {
            EMBEDDING_MODEL
        },
        config.k,
        config.k.max(20),
        config.mode.as_str(),
        &rows,
        &init_inclusive,
        dropped,
        memo_after.since(memo_before),
        input_files,
        config.drop_abstention,
        config.limit,
        config.stratify,
        rows.iter().map(|row| row.question_id.clone()).collect(),
        queries_override,
    );
    let table = format_table(&summary);
    write_outputs(&config.out, &rows, &summary, &table)?;
    Ok(format!(
        "Loaded and scored {} questions ({} abstention excluded).\nOutputs: {}\n\n{}",
        summary.scored_questions,
        summary.dropped_abstention,
        config.out.display(),
        table
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bench");

    #[test]
    fn agentmemory_reader_preserves_upstream_ids_types_and_gold() {
        let corpus = read_agentmemory(Path::new(FIXTURES).join("agentmemory"), None).unwrap();
        assert_eq!(corpus.dataset, "agentmemory");
        assert_eq!(corpus.sessions.len(), 3);
        assert_eq!(corpus.questions.len(), 2);
        assert_eq!(corpus.questions[1].id, "q-b");
        assert_eq!(corpus.questions[1].kind, "multi-session");
        assert_eq!(corpus.questions[1].gold_session_ids, ["sess-a", "sess-b"]);
    }

    #[test]
    fn agentmemory_file_form_uses_the_corpus_directory_basename() {
        let temp = tempfile::TempDir::new().unwrap();
        let corpus_dir = temp.path().join("named-corpus");
        fs::create_dir(&corpus_dir).unwrap();
        fs::copy(
            Path::new(FIXTURES).join("agentmemory/sessions.json"),
            corpus_dir.join("sessions.json"),
        )
        .unwrap();
        let override_path = corpus_dir.join("override-queries.json");
        fs::copy(
            Path::new(FIXTURES).join("agentmemory/queries.json"),
            &override_path,
        )
        .unwrap();

        let corpus =
            read_agentmemory(corpus_dir.join("sessions.json"), Some(&override_path)).unwrap();
        assert_eq!(corpus.dataset, "named-corpus");
        let canonical_override = fs::canonicalize(&override_path).unwrap();
        assert_eq!(
            corpus.queries_override.as_deref(),
            Some(canonical_override.to_str().unwrap())
        );
        assert_eq!(
            corpus.input_files[1].path,
            canonical_override.display().to_string()
        );
        assert_eq!(
            corpus.input_files[1].blake3,
            blake3::hash(&fs::read(canonical_override).unwrap())
                .to_hex()
                .to_string()
        );
    }

    #[test]
    fn longmemeval_reader_drops_exact_abs_types_by_default_and_honors_limit() {
        let path = Path::new(FIXTURES).join("longmemeval.json");
        let filtered = read_longmemeval(&path, true, None).unwrap();
        assert_eq!(filtered.questions.len(), 2);
        assert_eq!(filtered.dropped_abstention, 1);
        assert_eq!(filtered.questions[0].sessions[0].id, "lm-s1");
        assert_eq!(filtered.questions[0].sessions[0].messages[1].1, "cobalt");

        let limited = read_longmemeval(&path, false, Some(1)).unwrap();
        assert_eq!(limited.questions.len(), 1);
        assert_eq!(limited.dropped_abstention, 0);
    }

    #[test]
    fn metrics_match_hand_computed_multi_gold_values() {
        let row = score_row(
            "q",
            "multi",
            "csr-hybrid",
            5,
            &["noise", "gold-b", "noise-2", "gold-a", "noise-3"],
            &["gold-a", "gold-b"],
            12.5,
            12.5,
        );
        assert!((row.precision_at_k - 0.4).abs() < 1e-12);
        assert!((row.recall_at_k - 1.0).abs() < 1e-12);
        assert!(row.hit);
        assert_eq!(row.top_gold_rank, Some(2));
        assert!((row.mrr - 0.5).abs() < 1e-12);
        let expected_ndcg =
            (1.0 / 3.0_f64.log2() + 1.0 / 5.0_f64.log2()) / (1.0 + 1.0 / 3.0_f64.log2());
        assert!((row.ndcg_at_10 - expected_ndcg).abs() < 1e-12);
    }

    #[test]
    fn ndcg_denominator_comes_from_all_dataset_gold_not_retrieved_gold() {
        let row = score_row(
            "q",
            "multi",
            "csr-vector",
            5,
            &["gold-a"],
            &["gold-a", "gold-b"],
            1.0,
            1.0,
        );
        let expected = 1.0 / (1.0 + 1.0 / 3.0_f64.log2());
        assert!((row.ndcg_at_10 - expected).abs() < 1e-12);
    }

    #[test]
    fn score_row_json_matches_agentmemory_shape_exactly() {
        let row = score_row("q", "single", "csr-fts", 5, &["gold"], &["gold"], 3.0, 3.0);
        let value = serde_json::to_value(row.agentmemory()).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "adapter",
                "hit",
                "k",
                "latencyMs",
                "precisionAtK",
                "questionId",
                "questionType",
                "recallAtK",
                "topGoldRank"
            ]
        );
    }

    #[test]
    fn embedding_memo_reports_hits_and_only_embeds_unique_content() {
        let mut calls = Vec::new();
        let mut memo = EmbeddingMemo::default();
        let first = memo
            .embed_missing(&["same", "different"], |texts| {
                calls.push(texts.len());
                Ok(texts.iter().map(|text| vec![text.len() as f32]).collect())
            })
            .unwrap();
        let second = memo
            .embed_missing(&["same", "same"], |_| panic!("cache miss"))
            .unwrap();
        assert_eq!(first, [vec![4.0], vec![9.0]]);
        assert_eq!(second, [vec![4.0], vec![4.0]]);
        assert_eq!(calls, [2]);
        assert_eq!(memo.stats().misses, 2);
        assert_eq!(memo.stats().hits, 2);
    }

    #[test]
    fn embedding_memo_counts_same_batch_reuse_as_a_hit() {
        let mut memo = EmbeddingMemo::default();
        let vectors = memo
            .embed_missing(&["same", "same"], |texts| {
                assert_eq!(texts, ["same"]);
                Ok(vec![vec![4.0]])
            })
            .unwrap();
        assert_eq!(vectors, [vec![4.0], vec![4.0]]);
        assert_eq!(memo.stats().misses, 1);
        assert_eq!(memo.stats().hits, 1);
    }

    #[test]
    fn score_row_top_gold_rank_is_bounded_by_k() {
        let row = score_row(
            "q",
            "single",
            "csr-hybrid",
            2,
            &["noise-a", "noise-b", "gold"],
            &["gold"],
            1.0,
            1.0,
        );
        assert_eq!(row.top_gold_rank, None);
        assert!((row.mrr - (1.0 / 3.0)).abs() < 1e-12);
    }

    #[test]
    fn non_five_table_does_not_place_published_at_five_under_another_k() {
        let summary = summarize(
            "fixture",
            "all",
            EMBEDDING_MODEL,
            3,
            20,
            "hybrid",
            &[],
            &[],
            0,
            MemoStats::default(),
            Vec::new(),
            true,
            None,
            None,
            Vec::new(),
            None,
        );
        let table = format_table(&summary);
        assert!(!table.contains("| agentmemory `565c238` (published)"));
        assert!(table.contains("95.2% is recall_any@5"));
    }

    #[test]
    fn zero_limit_and_stratify_are_rejected() {
        let base = BenchConfig {
            format: BenchFormat::Agentmemory,
            data: PathBuf::from("fixture"),
            queries: None,
            k: 5,
            mode: SearchMode::Fts,
            out: PathBuf::from("out"),
            drop_abstention: true,
            limit: None,
            stratify: None,
        };
        let mut zero_limit = base.clone();
        zero_limit.limit = Some(0);
        assert!(validate_config(&zero_limit).is_err());
        let mut zero_stratify = base;
        zero_stratify.stratify = Some(0);
        assert!(validate_config(&zero_stratify).is_err());
    }

    #[tokio::test]
    async fn longmemeval_runner_builds_a_fresh_store_for_each_question() {
        let path = Path::new(FIXTURES).join("longmemeval.json");
        let corpus = read_longmemeval(&path, true, None).unwrap();
        let (first, _) = index_sessions(&corpus.questions[0].sessions, None)
            .await
            .unwrap();
        let (second, _) = index_sessions(&corpus.questions[1].sessions, None)
            .await
            .unwrap();
        assert_eq!(first.count_conversations().unwrap(), 2);
        assert_eq!(second.count_conversations().unwrap(), 2);
        assert!(!std::ptr::eq(Arc::as_ptr(&first), Arc::as_ptr(&second)));
    }

    #[tokio::test]
    async fn scratch_indexes_are_isolated_in_memory() {
        let first = vec![BenchSession {
            id: "first".into(),
            timestamp: None,
            messages: vec![(Speaker::User, "one".into())],
        }];
        let second = vec![BenchSession {
            id: "second".into(),
            timestamp: None,
            messages: vec![(Speaker::User, "two".into())],
        }];
        let (first_store, _) = index_sessions(&first, None).await.unwrap();
        let (second_store, _) = index_sessions(&second, None).await.unwrap();
        assert_eq!(first_store.count_conversations().unwrap(), 1);
        assert_eq!(second_store.count_conversations().unwrap(), 1);
        assert!(first_store
            .get_chunk_ids_for_project(PROJECT)
            .unwrap()
            .iter()
            .all(|id| !second_store
                .get_chunks_by_ids(std::slice::from_ref(id))
                .unwrap()
                .iter()
                .any(|chunk| chunk.conversation_id == "first")));
    }

    #[tokio::test]
    async fn scratch_index_provenance_enables_user_authority_ranking() {
        let sessions = vec![
            BenchSession {
                id: "assistant-conversation".into(),
                timestamp: Some("2026-01-01T00:00:00Z".into()),
                messages: vec![(Speaker::Assistant, "identical authoritytoken claim".into())],
            },
            BenchSession {
                id: "user-conversation".into(),
                timestamp: Some("2026-01-01T00:00:00Z".into()),
                messages: vec![(Speaker::User, "identical authoritytoken claim".into())],
            },
        ];
        let without_storage = Arc::new(Storage::open_memory().unwrap());
        let mut without_search = SearchEngine::new(16);
        for session in &sessions {
            for chunk in chunk_messages(
                &session.id,
                PROJECT,
                session.timestamp.as_deref().unwrap(),
                &session.messages,
                false,
            ) {
                let vector = vec![0.0; EmbeddingEngine::dimension()];
                without_storage.insert_chunk(&chunk, &vector).unwrap();
                without_search.insert_chunk(chunk.id, vector);
            }
        }
        let without_search = Arc::new(RwLock::new(without_search));
        let without_hits = query_store(
            &without_storage,
            &without_search,
            None,
            "authoritytoken",
            2,
            SearchMode::Vector,
        )
        .await
        .unwrap();
        assert_eq!(
            without_hits.first().map(String::as_str),
            Some("assistant-conversation"),
            "without provenance an exact tie retains insertion order"
        );

        let (storage, search) = index_sessions(&sessions, None).await.unwrap();
        let user_chunk_id = storage
            .get_chunk_ids_for_conversation("user-conversation")
            .unwrap()
            .remove(0);
        let provenance = storage
            .get_chunk_provenance(&user_chunk_id)
            .unwrap()
            .expect("scratch chunks must carry real-import provenance");
        assert_eq!(provenance.author, Speaker::User);
        assert_eq!(provenance.source_conv_id, "user-conversation");

        let hits = query_store(
            &storage,
            &search,
            None,
            "authoritytoken",
            2,
            SearchMode::Vector,
        )
        .await
        .unwrap();
        assert_eq!(hits.first().map(String::as_str), Some("user-conversation"));
    }

    #[tokio::test]
    async fn bench_query_collapses_multiple_matching_chunks_by_conversation() {
        let sessions = vec![BenchSession {
            id: "long-session".into(),
            timestamp: None,
            messages: vec![(Speaker::User, format!("needle {} needle", "x".repeat(950)))],
        }];
        let (storage, search) = index_sessions(&sessions, None).await.unwrap();
        let hits = query_store(&storage, &search, None, "needle", 5, SearchMode::Fts)
            .await
            .unwrap();
        assert_eq!(hits, ["long-session"]);
    }

    #[test]
    fn summary_uses_upper_median_for_even_latency_samples() {
        let first = score_row("a", "x", "csr-hybrid", 5, &["g"], &["g"], 1.0, 11.0);
        assert_eq!(first.query_latency_ms, 1.0);
        assert_eq!(first.agentmemory().latency_ms, 11.0);
        let summary = summarize(
            "fixture",
            "test",
            "all-MiniLM-L6-v2",
            5,
            20,
            "hybrid",
            &[
                first,
                score_row("b", "x", "csr-hybrid", 5, &[], &["g"], 9.0, 19.0),
            ],
            &[11.0, 3.0],
            0,
            MemoStats::default(),
            Vec::new(),
            true,
            None,
            None,
            vec!["a".into(), "b".into()],
            None,
        );
        assert_eq!(summary.query_latency_p50_ms, 9.0);
        assert_eq!(summary.init_inclusive_latency_p50_ms, 11.0);
        assert_eq!(summary.recall_any_at_k, 0.5);
    }

    #[test]
    fn summary_receipts_identify_the_built_binary_and_exact_sample() {
        let summary = summarize(
            "fixture",
            "all",
            "model",
            5,
            20,
            "hybrid",
            &[],
            &[],
            0,
            MemoStats::default(),
            vec![InputFileReceipt {
                role: "sessions".into(),
                path: "/absolute/sessions.json".into(),
                blake3: "abc123".into(),
            }],
            true,
            Some(7),
            Some(2),
            vec!["q-2".into(), "q-1".into()],
            Some("/absolute/override-queries.json".into()),
        );
        assert_eq!(summary.build_commit, env!("CSR_BUILD_GIT_SHA"));
        assert_eq!(summary.build_dirty, env!("CSR_BUILD_GIT_DIRTY") == "true");
        assert_eq!(summary.ranking_depth, 20);
        assert_eq!(summary.input_files[0].path, "/absolute/sessions.json");
        assert_eq!(summary.input_files[0].role, "sessions");
        assert_eq!(
            summary.queries_override.as_deref(),
            Some("/absolute/override-queries.json")
        );
        assert_eq!(summary.scored_question_ids, ["q-2", "q-1"]);
        assert_eq!(summary.limit, Some(7));
        assert_eq!(summary.stratify, Some(2));
        assert!(summary.drop_abstention);
        assert!(summary.metric_definitions.contains_key("MRR@20"));
    }

    #[test]
    fn stratification_matches_agentmemory_type_sorted_first_n_behavior() {
        let items = vec![("z", 1), ("a", 1), ("z", 2), ("a", 2)];
        let sampled = stratified(items, Some(1), |item| item.0);
        assert_eq!(sampled, [("a", 1), ("z", 1)]);
    }
}
