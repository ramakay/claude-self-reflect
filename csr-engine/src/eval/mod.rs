//! Evaluation framework for csr-engine.
//!
//! Quick mode: 5 core tests plus the persisted trained-reranker gate (<30s)
//! Full mode: 24 results — 20 tests, the persisted trained-reranker gate, the
//! emitter contamination canary, the report-only duplicate rate and the
//! known-item recall probe (~2 min). Any failing result is a nonzero exit.
//! Continuity mode: the North Star gate — CSR must recall its own vision with
//! provenance, beating a grep baseline (`csr-engine eval --continuity`).

pub mod bench;
pub mod codegraph;
pub mod contamination;
pub mod continuity;
pub mod health;
pub mod lessons;
pub mod provenance;
pub mod trained_rerank;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// A single evaluation result.
#[derive(Debug)]
pub struct EvalResult {
    pub name: String,
    pub category: String,
    pub passed: bool,
    pub duration_ms: f64,
    pub detail: String,
}

impl EvalResult {
    pub fn format_line(&self) -> String {
        let status = if self.passed { "PASS" } else { "FAIL" };
        format!(
            "  [{status}] {:<30} ({:.0}ms) {}\n",
            self.name, self.duration_ms, self.detail
        )
    }

    pub(crate) fn pass(name: &str, category: &str, duration_ms: f64, detail: String) -> Self {
        Self {
            name: name.to_string(),
            category: category.to_string(),
            passed: true,
            duration_ms,
            detail,
        }
    }

    pub(crate) fn fail(name: &str, category: &str, duration_ms: f64, detail: String) -> Self {
        Self {
            name: name.to_string(),
            category: category.to_string(),
            passed: false,
            duration_ms,
            detail,
        }
    }
}

/// Full evaluation report.
pub struct EvalReport {
    pub results: Vec<EvalResult>,
    pub total_ms: f64,
}

impl EvalReport {
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.passed).count()
    }

    pub fn total(&self) -> usize {
        self.results.len()
    }

    /// True when any result failed. Amber, abstain and the report-only
    /// duplicate rate pass, so only a real failure gates.
    pub fn has_gating_failure(&self) -> bool {
        self.results.iter().any(|r| !r.passed)
    }

    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        out.push_str("CSR Engine Evaluation Report\n");
        out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\n");

        for r in &self.results {
            out.push_str(&r.format_line());
        }

        out.push_str(&format!(
            "\nResult: {}/{} passed in {:.0}ms\n",
            self.passed(),
            self.total(),
            self.total_ms
        ));
        out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        out
    }
}

/// Run quick evaluation (5 core tests plus the trained-reranker gate).
pub async fn run_quick(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    index_dir: &Path,
) -> EvalReport {
    let start = Instant::now();
    let mut results = Vec::new();

    results.push(test_db_connectivity(storage));
    results.push(test_search_accuracy(storage, embeddings, search).await);
    results.push(test_performance(embeddings, search).await);
    results.push(test_cache_status(index_dir));
    results.push(test_tool_count());
    results.push(trained_rerank::latest_gate_result(storage));

    EvalReport {
        results,
        total_ms: start.elapsed().as_secs_f64() * 1000.0,
    }
}

/// Run full evaluation (24 results). Equivalent to [`run_full_with`] keeping
/// the frozen recall sample.
pub async fn run_full(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    index_dir: &Path,
) -> EvalReport {
    run_full_with(storage, embeddings, search, index_dir, false).await
}

/// Run full evaluation. `refresh_recall_sample` deliberately starts a new
/// recall sample (`eval --full --refresh-recall-sample`); history is kept.
pub async fn run_full_with(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    index_dir: &Path,
    refresh_recall_sample: bool,
) -> EvalReport {
    let start = Instant::now();
    let mut results = Vec::new();

    // Quick tests (5 core + trained-reranker gate)
    results.push(test_db_connectivity(storage));
    results.push(test_search_accuracy(storage, embeddings, search).await);
    results.push(test_performance(embeddings, search).await);
    results.push(test_cache_status(index_dir));
    results.push(test_tool_count());
    results.push(trained_rerank::latest_gate_result(storage));

    // Semantic search tests (5)
    results.push(
        test_semantic_search(
            embeddings,
            search,
            "docker container debugging",
            "semantic_docker",
        )
        .await,
    );
    results.push(
        test_semantic_search(
            embeddings,
            search,
            "MCP tool implementation",
            "semantic_mcp",
        )
        .await,
    );
    results.push(
        test_semantic_search(
            embeddings,
            search,
            "error handling strategies",
            "semantic_errors",
        )
        .await,
    );
    results.push(
        test_semantic_search(
            embeddings,
            search,
            "performance optimization",
            "semantic_perf",
        )
        .await,
    );
    results.push(
        test_semantic_search(
            embeddings,
            search,
            "authentication security",
            "semantic_auth",
        )
        .await,
    );

    // Storage / data quality tests (5)
    results.push(test_chunk_data_quality(storage));
    results.push(test_reflection_data_quality(storage));
    results.push(test_embedding_dimensions(embeddings));
    results.push(test_search_index_consistency(storage, search).await);
    results.push(test_db_integrity(storage));

    // Code-aware enrichment tests (3)
    results.push(test_v3_extraction_works());
    results.push(test_ast_analysis_works());
    results.push(test_quality_analysis_works());

    // Performance deep tests (2)
    results.push(test_batch_embedding_speed(embeddings));
    results.push(test_search_latency_p95(embeddings, search).await);

    // Contamination: every emitter output detected, every mention kept (no DB).
    results.push(contamination::emitter_canary());

    // Memory health: measured now and persisted, then reported as two results.
    let backend = crate::health::probe::EngineBackend {
        storage: storage.clone(),
        embeddings: embeddings.clone(),
        search: search.clone(),
    };
    let run = crate::health::run_health(
        storage,
        search,
        &backend,
        refresh_recall_sample,
        chrono::Utc::now(),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .await
    .map_err(|error| error.to_string())
    .and_then(|run| run.ok_or_else(|| "the measurement was cancelled".to_string()));
    results.push(health::duplicate_rate(&run));
    results.push(health::known_item_recall(&run));

    EvalReport {
        results,
        total_ms: start.elapsed().as_secs_f64() * 1000.0,
    }
}

// --- Quick tests ---

fn test_db_connectivity(storage: &Arc<Storage>) -> EvalResult {
    let t = Instant::now();
    match storage.count_chunk_embeddings() {
        Ok(count) => EvalResult::pass(
            "DB Connectivity",
            "infrastructure",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("{count} chunks indexed"),
        ),
        Err(e) => EvalResult::fail(
            "DB Connectivity",
            "infrastructure",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("Error: {e}"),
        ),
    }
}

async fn test_search_accuracy(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> EvalResult {
    let t = Instant::now();

    let chunk_count = storage.count_chunk_embeddings().unwrap_or(0);
    if chunk_count == 0 {
        return EvalResult::pass(
            "Search Accuracy",
            "search",
            t.elapsed().as_secs_f64() * 1000.0,
            "SKIP: no data indexed yet".to_string(),
        );
    }

    let query_vec = match embeddings.embed_single("docker container debugging") {
        Ok(v) => v,
        Err(e) => {
            return EvalResult::fail(
                "Search Accuracy",
                "search",
                t.elapsed().as_secs_f64() * 1000.0,
                format!("Embed error: {e}"),
            );
        }
    };

    let results = search.read().await.search_chunks(&query_vec, 5, 0.0);
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    if results.is_empty() {
        EvalResult::fail(
            "Search Accuracy",
            "search",
            ms,
            "No results returned".to_string(),
        )
    } else {
        let top_score = results[0].score;
        if top_score > 0.1 {
            EvalResult::pass(
                "Search Accuracy",
                "search",
                ms,
                format!("top score: {top_score:.3}, {n} results", n = results.len()),
            )
        } else {
            EvalResult::fail(
                "Search Accuracy",
                "search",
                ms,
                format!("low relevance: top score {top_score:.3}"),
            )
        }
    }
}

async fn test_performance(
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> EvalResult {
    let t = Instant::now();

    let query_vec = match embeddings.embed_single("test query for performance measurement") {
        Ok(v) => v,
        Err(e) => {
            return EvalResult::fail(
                "Performance Target",
                "performance",
                t.elapsed().as_secs_f64() * 1000.0,
                format!("Embed error: {e}"),
            );
        }
    };

    let search_start = Instant::now();
    let _results = search.read().await.search_chunks(&query_vec, 10, 0.0);
    let search_ms = search_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = t.elapsed().as_secs_f64() * 1000.0;

    if total_ms < 500.0 {
        EvalResult::pass(
            "Performance Target",
            "performance",
            total_ms,
            format!("embed+search: {total_ms:.1}ms (search: {search_ms:.1}ms)"),
        )
    } else {
        EvalResult::fail(
            "Performance Target",
            "performance",
            total_ms,
            format!("too slow: {total_ms:.1}ms (target <500ms)"),
        )
    }
}

fn test_cache_status(index_dir: &Path) -> EvalResult {
    let t = Instant::now();
    let manifest_path = index_dir.join("manifest.json");

    if manifest_path.exists() {
        let metadata = std::fs::metadata(&manifest_path);
        let detail = match metadata {
            Ok(m) => {
                let age = m.modified().ok().and_then(|mt| mt.elapsed().ok());
                match age {
                    Some(d) => format!("warm (age: {}s)", d.as_secs()),
                    None => "warm".to_string(),
                }
            }
            Err(_) => "warm (unreadable metadata)".to_string(),
        };
        EvalResult::pass(
            "Cache Status",
            "infrastructure",
            t.elapsed().as_secs_f64() * 1000.0,
            detail,
        )
    } else {
        EvalResult::pass(
            "Cache Status",
            "infrastructure",
            t.elapsed().as_secs_f64() * 1000.0,
            "cold (no manifest — first run)".to_string(),
        )
    }
}

fn test_tool_count() -> EvalResult {
    let t = Instant::now();
    // Counted from the live rmcp router, not a constant — a hardcoded expectation
    // sat at 14 while the server shipped 15 tools (silently-inert eval).
    // 16 as of csr_transcript (transcript-query-tool-design.md phase 3).
    //
    // A bare count only proves *some* 16 tools exist — it would still pass
    // if csr_transcript were silently dropped and replaced by a duplicate
    // of another tool. Assert the specific named tool is present too
    // (adversarial review finding 6).
    let names = crate::mcp::CsrServer::tool_names();
    let actual = names.len();
    let expected = 16;
    let has_transcript = names.iter().any(|n| n == "csr_transcript");
    let detail = format!(
        "{actual} MCP tools defined (expected {expected}); csr_transcript present={has_transcript}"
    );
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    if actual == expected && has_transcript {
        EvalResult::pass("Tool Count", "infrastructure", ms, detail)
    } else {
        EvalResult::fail("Tool Count", "infrastructure", ms, detail)
    }
}

// --- Semantic search tests ---

async fn test_semantic_search(
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    test_name: &str,
) -> EvalResult {
    let t = Instant::now();

    let query_vec = match embeddings.embed_single(query) {
        Ok(v) => v,
        Err(e) => {
            return EvalResult::fail(
                test_name,
                "semantic",
                t.elapsed().as_secs_f64() * 1000.0,
                format!("Embed error: {e}"),
            );
        }
    };

    let results = search.read().await.search_chunks(&query_vec, 5, 0.0);
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    if results.is_empty() {
        EvalResult::pass(
            test_name,
            "semantic",
            ms,
            "SKIP: no data indexed".to_string(),
        )
    } else {
        let top = results[0].score;
        EvalResult::pass(
            test_name,
            "semantic",
            ms,
            format!("top: {top:.3}, count: {}", results.len()),
        )
    }
}

// --- Data quality tests ---

fn test_chunk_data_quality(storage: &Arc<Storage>) -> EvalResult {
    let t = Instant::now();
    match storage.count_chunk_embeddings() {
        Ok(count) => EvalResult::pass(
            "Chunk Data Quality",
            "data",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("{count} chunks with embeddings"),
        ),
        Err(e) => EvalResult::fail(
            "Chunk Data Quality",
            "data",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("Error: {e}"),
        ),
    }
}

fn test_reflection_data_quality(storage: &Arc<Storage>) -> EvalResult {
    let t = Instant::now();
    match storage.count_reflection_embeddings() {
        Ok(count) => EvalResult::pass(
            "Reflection Data Quality",
            "data",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("{count} reflections with embeddings"),
        ),
        Err(e) => EvalResult::fail(
            "Reflection Data Quality",
            "data",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("Error: {e}"),
        ),
    }
}

fn test_embedding_dimensions(embeddings: &Arc<EmbeddingEngine>) -> EvalResult {
    let t = Instant::now();
    match embeddings.embed_single("dimension test") {
        Ok(v) => {
            let dim = v.len();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if dim == 384 {
                EvalResult::pass(
                    "Embedding Dimensions",
                    "data",
                    ms,
                    format!("{dim}d (FastEmbed)"),
                )
            } else {
                EvalResult::fail(
                    "Embedding Dimensions",
                    "data",
                    ms,
                    format!("unexpected: {dim}d (expected 384)"),
                )
            }
        }
        Err(e) => EvalResult::fail(
            "Embedding Dimensions",
            "data",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("Error: {e}"),
        ),
    }
}

/// A side is consistent when its gap between SQLite and the index is no
/// larger than the changes written after the index's cursor. Those rows are
/// in flight (the index was loaded before they landed), not drift.
fn index_gap_in_flight(db: usize, idx: usize, pending: usize) -> bool {
    db.abs_diff(idx) <= pending
}

async fn test_search_index_consistency(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
) -> EvalResult {
    let t = Instant::now();
    let db_chunks = storage.count_chunk_embeddings().unwrap_or(0);
    let db_reflections = storage.count_reflection_embeddings().unwrap_or(0);

    let se = search.read().await;
    let idx_chunks = se.chunk_count();
    let idx_reflections = se.reflection_count();
    let chunk_cursor = se.side_cursor(false);
    let reflection_cursor = se.side_cursor(true);
    drop(se);

    // Read after the DB counts, so a row landing in between can only widen
    // the allowance, never fail a consistent index.
    let pending_chunks = storage
        .count_embedding_changes_after(false, chunk_cursor)
        .unwrap_or(0);
    let pending_reflections = storage
        .count_embedding_changes_after(true, reflection_cursor)
        .unwrap_or(0);

    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let counts =
        format!("DB={db_chunks}c/{db_reflections}r, HNSW={idx_chunks}c/{idx_reflections}r");

    if db_chunks == idx_chunks && db_reflections == idx_reflections {
        EvalResult::pass("Index Consistency", "data", ms, counts)
    } else if index_gap_in_flight(db_chunks, idx_chunks, pending_chunks)
        && index_gap_in_flight(db_reflections, idx_reflections, pending_reflections)
    {
        EvalResult::pass(
            "Index Consistency",
            "data",
            ms,
            format!(
                "{counts}; gap covered by {pending_chunks}c/{pending_reflections}r changes written after the index was loaded (in flight)"
            ),
        )
    } else {
        EvalResult::fail(
            "Index Consistency",
            "data",
            ms,
            format!(
                "MISMATCH: {counts}; only {pending_chunks}c/{pending_reflections}r changes in flight"
            ),
        )
    }
}

fn test_db_integrity(storage: &Arc<Storage>) -> EvalResult {
    let t = Instant::now();
    match storage.integrity_check() {
        Ok(ok) => {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ok {
                EvalResult::pass(
                    "DB Integrity",
                    "data",
                    ms,
                    "PRAGMA integrity_check: ok".to_string(),
                )
            } else {
                EvalResult::fail(
                    "DB Integrity",
                    "data",
                    ms,
                    "integrity check failed".to_string(),
                )
            }
        }
        Err(e) => EvalResult::fail(
            "DB Integrity",
            "data",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("Error: {e}"),
        ),
    }
}

// --- Code-aware tests ---

fn test_v3_extraction_works() -> EvalResult {
    let t = Instant::now();
    use serde_json::json;
    let messages = vec![
        json!({"role": "user", "content": "Fix the authentication bug in login flow"}),
        json!({"role": "assistant", "content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": "src/auth.rs", "old_string": "old", "new_string": "new replacement code"}}]}),
        json!({"role": "assistant", "content": "Fixed the bug. Build compiled successfully."}),
    ];
    let result = crate::extraction::extract_v3(&messages);
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    if !result.search_index.is_empty() && result.stats.original_messages == 3 {
        EvalResult::pass(
            "V3 Extraction",
            "enrichment",
            ms,
            format!(
                "{}msg -> {}tok index, {} patterns",
                result.stats.original_messages,
                result.stats.search_index_tokens,
                result.stats.patterns_found
            ),
        )
    } else {
        EvalResult::fail(
            "V3 Extraction",
            "enrichment",
            ms,
            "empty output".to_string(),
        )
    }
}

fn test_ast_analysis_works() -> EvalResult {
    let t = Instant::now();
    use crate::extraction::ast_analysis;
    use serde_json::json;

    let messages = vec![json!({
        "role": "assistant",
        "content": [{
            "type": "tool_use",
            "name": "Write",
            "input": {
                "file_path": "src/main.rs",
                "content": "fn dispatch_hook(name: &str) -> Result<()> { Ok(()) }\nstruct Engine { db: Connection }"
            }
        }]
    })];

    let ctx = ast_analysis::extract_code_context(&messages);
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    if !ctx.functions.is_empty() || !ctx.types.is_empty() {
        EvalResult::pass(
            "AST Analysis",
            "enrichment",
            ms,
            format!(
                "fns: {:?}, types: {:?}, langs: {:?}",
                ctx.functions, ctx.types, ctx.languages
            ),
        )
    } else {
        EvalResult::fail(
            "AST Analysis",
            "enrichment",
            ms,
            "no code entities extracted".to_string(),
        )
    }
}

fn test_quality_analysis_works() -> EvalResult {
    let t = Instant::now();
    use crate::extraction::quality;
    use ast_grep_language::SupportLang;

    let source = r#"
fn main() {
    let value = some_result().unwrap();
    panic!("unexpected state");
}
"#;
    let report = quality::analyze_source(source, SupportLang::Rust, "test.rs");
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    if !report.findings.is_empty() && report.score < 100.0 {
        EvalResult::pass(
            "Quality Analysis",
            "enrichment",
            ms,
            format!(
                "score: {:.0}/100, {} findings",
                report.score,
                report.findings.len()
            ),
        )
    } else {
        EvalResult::fail(
            "Quality Analysis",
            "enrichment",
            ms,
            format!(
                "unexpected: score={}, findings={}",
                report.score,
                report.findings.len()
            ),
        )
    }
}

// --- Performance deep tests ---

/// Batch embedding target, ms per text. Over it the check warns, never fails.
const BATCH_EMBED_TARGET_MS_PER_TEXT: f64 = 5.0;
/// Search p95 target, ms. Over it the check warns, never fails.
const SEARCH_P95_TARGET_MS: f64 = 1.0;
/// Timed passes over the query set, after one untimed warm-up pass.
const SEARCH_TIMED_ROUNDS: usize = 5;

/// Grade a batch embed. A timing miss is AMBER and passes (wall-clock noise,
/// not a break); no vectors at all is a broken engine and fails.
fn grade_batch_embed(texts: usize, total_ms: f64) -> (bool, String) {
    if texts == 0 || !total_ms.is_finite() {
        return (
            false,
            format!("embed returned no vectors ({texts} texts, {total_ms}ms)"),
        );
    }
    let per_text = total_ms / texts as f64;
    if per_text < BATCH_EMBED_TARGET_MS_PER_TEXT {
        (
            true,
            format!("{per_text:.1}ms/text ({texts} texts in {total_ms:.0}ms)"),
        )
    } else {
        (
            true,
            format!(
                "AMBER (non-gating): {per_text:.1}ms/text, target <{BATCH_EMBED_TARGET_MS_PER_TEXT:.0}ms/text ({texts} texts in {total_ms:.0}ms)"
            ),
        )
    }
}

fn test_batch_embedding_speed(embeddings: &Arc<EmbeddingEngine>) -> EvalResult {
    let t = Instant::now();
    let texts: Vec<String> = (0..10)
        .map(|i| format!("Test sentence number {i} for batch embedding speed evaluation"))
        .collect();
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

    match embeddings.embed(&text_refs) {
        Ok(vecs) => {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let (passed, detail) = grade_batch_embed(vecs.len(), ms);
            if passed {
                EvalResult::pass("Batch Embed Speed", "performance", ms, detail)
            } else {
                EvalResult::fail("Batch Embed Speed", "performance", ms, detail)
            }
        }
        Err(e) => EvalResult::fail(
            "Batch Embed Speed",
            "performance",
            t.elapsed().as_secs_f64() * 1000.0,
            format!("Error: {e}"),
        ),
    }
}

/// p95 by nearest rank: the value at rank ceil(0.95 * n) of the sorted
/// samples. `None` for no samples (never an underflow).
fn nearest_rank_p95(samples: &mut [f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let rank = (95 * samples.len()).div_ceil(100);
    samples.get(rank.max(1) - 1).copied()
}

/// Grade the search latency. `p95` is `None` when nothing was timed. Over the
/// target is AMBER and passes; zero timed searches (or a non-finite p95) is a
/// broken engine and fails.
fn grade_search_latency(p95: Option<f64>, timed: usize, chunks: usize) -> (bool, String) {
    let p95 = match p95 {
        Some(p) if timed > 0 && p.is_finite() => p,
        _ => {
            return (
                false,
                format!("no timed searches completed ({timed} timed, {chunks} chunks indexed)"),
            )
        }
    };
    if p95 < SEARCH_P95_TARGET_MS {
        (
            true,
            format!("p95: {p95:.2}ms ({timed} timed searches, {chunks} chunks)"),
        )
    } else {
        (
            true,
            format!(
                "AMBER (non-gating): p95 {p95:.2}ms, target <{SEARCH_P95_TARGET_MS:.0}ms ({timed} timed searches, {chunks} chunks indexed)"
            ),
        )
    }
}

async fn test_search_latency_p95(
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> EvalResult {
    let t = Instant::now();
    let queries = [
        "docker debugging",
        "authentication security",
        "performance optimization",
        "error handling patterns",
        "MCP tool development",
        "database migration",
        "testing strategies",
        "deployment automation",
        "code review process",
        "API design patterns",
    ];

    let vecs: Vec<Vec<f32>> = queries
        .iter()
        .filter_map(|q| embeddings.embed_single(q).ok())
        .collect();

    let mut latencies = Vec::with_capacity(vecs.len() * SEARCH_TIMED_ROUNDS);
    let se = search.read().await;
    // One untimed pass over every query warms caches and pages, so the
    // timed searches measure steady state, not the first touch.
    for v in &vecs {
        let _r = se.search_chunks(v, 5, 0.0);
    }
    for _ in 0..SEARCH_TIMED_ROUNDS {
        for v in &vecs {
            let qt = Instant::now();
            let _r = se.search_chunks(v, 5, 0.0);
            latencies.push(qt.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let chunks = se.chunk_count();
    drop(se);

    let timed = latencies.len();
    let p95 = nearest_rank_p95(&mut latencies);
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    let (passed, detail) = grade_search_latency(p95, timed, chunks);
    if passed {
        EvalResult::pass("Search Latency P95", "performance", ms, detail)
    } else {
        EvalResult::fail("Search Latency P95", "performance", ms, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn index_lag_from_changes_after_the_cursor_passes_and_drift_fails() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .with_connection(|conn| {
                conn.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES('c','conv','project','2026-10-22','content',1)", [])?;
                conn.execute("INSERT INTO chunk_embeddings(chunk_id,embedding) VALUES('c',?1)", [vec![0u8; 1536]])?;
                Ok(())
            })
            .unwrap();
        let search = Arc::new(RwLock::new(SearchEngine::new(EmbeddingEngine::dimension())));

        // The index predates the row: one change after its cursor covers the gap.
        let lag = test_search_index_consistency(&storage, &search).await;
        assert!(lag.passed, "{}", lag.detail);
        assert!(lag.detail.contains("in flight"), "{}", lag.detail);

        // The index claims to have applied that change yet lacks the row: drift.
        let seq = storage.latest_embedding_change_seq().unwrap();
        search.write().await.set_change_cursor(seq);
        let drift = test_search_index_consistency(&storage, &search).await;
        assert!(!drift.passed, "{}", drift.detail);
        assert!(drift.detail.starts_with("MISMATCH"), "{}", drift.detail);
    }

    #[test]
    fn index_gap_is_in_flight_only_within_the_pending_changes() {
        assert!(index_gap_in_flight(10, 10, 0));
        assert!(index_gap_in_flight(12, 10, 2));
        assert!(index_gap_in_flight(10, 12, 2));
        assert!(!index_gap_in_flight(13, 10, 2));
    }

    #[tokio::test]
    async fn full_eval_has_twenty_four_named_results() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(EmbeddingEngine::dimension())));
        let dir = tempfile::tempdir().unwrap();
        let report = run_full(&storage, &embeddings, &search, dir.path()).await;
        let names: Vec<&str> = report.results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "DB Connectivity",
                "Search Accuracy",
                "Performance Target",
                "Cache Status",
                "Tool Count",
                "Trained Re-ranker Gate",
                "semantic_docker",
                "semantic_mcp",
                "semantic_errors",
                "semantic_perf",
                "semantic_auth",
                "Chunk Data Quality",
                "Reflection Data Quality",
                "Embedding Dimensions",
                "Index Consistency",
                "DB Integrity",
                "V3 Extraction",
                "AST Analysis",
                "Quality Analysis",
                "Batch Embed Speed",
                "Search Latency P95",
                "Emitter Contamination Canary",
                "Duplicate Rate (Report Only)",
                "Known-Item Recall",
            ]
        );
        assert_eq!(report.total(), 24);
        // a fresh store: nothing to measure, so the probe abstains and no
        // memory result fails the gate
        let recall = report.results.last().unwrap();
        assert!(recall.passed, "{}", recall.detail);
        assert!(recall.detail.starts_with("ABSTAIN:"), "{}", recall.detail);
        let duplicates = &report.results[22];
        assert!(duplicates.passed && duplicates.detail.starts_with("REPORT ONLY:"));
    }

    #[test]
    fn only_failing_results_gate_the_full_eval() {
        let report = |results: Vec<EvalResult>| EvalReport {
            results,
            total_ms: 0.0,
        };
        // report-only, abstaining and amber results pass and never gate
        let quiet = report(vec![
            EvalResult::pass("a", "c", 0.0, "REPORT ONLY: 29 duplicate rows".into()),
            EvalResult::pass("b", "c", 0.0, "ABSTAIN: 4 eligible cases".into()),
            EvalResult::pass("c", "c", 0.0, "AMBER (non-gating): 15/20".into()),
        ]);
        assert!(!quiet.has_gating_failure());
        // a red recall result is a FAIL result, and it gates
        let red = report(vec![
            EvalResult::pass("a", "c", 0.0, "ok".into()),
            EvalResult::fail("Known-Item Recall", "c", 0.0, "RED: 6/20".into()),
        ]);
        assert!(red.has_gating_failure());
    }

    #[test]
    fn search_latency_under_target_is_a_plain_pass() {
        let (passed, detail) = grade_search_latency(Some(0.42), 50, 760_078);
        assert!(passed);
        assert_eq!(detail, "p95: 0.42ms (50 timed searches, 760078 chunks)");
        assert!(!detail.contains("AMBER"));
    }

    #[test]
    fn search_latency_over_target_is_amber_and_passes() {
        // the owner's quiet measurement, then the loaded one, then the boundary
        for p95 in [4.13, 311.95, 1.0] {
            let (passed, detail) = grade_search_latency(Some(p95), 50, 760_078);
            assert!(passed, "{detail}");
            assert!(detail.starts_with("AMBER (non-gating): "), "{detail}");
            assert!(detail.contains(&format!("{p95:.2}ms")), "{detail}");
            assert!(detail.contains("target <1ms"), "{detail}");
            assert!(detail.contains("50 timed searches"), "{detail}");
            assert!(detail.contains("760078 chunks"), "{detail}");
        }
    }

    #[test]
    fn search_latency_with_nothing_timed_fails() {
        for (p95, timed) in [(None, 0), (Some(0.5), 0), (Some(f64::NAN), 10), (None, 10)] {
            let (passed, detail) = grade_search_latency(p95, timed, 12);
            assert!(!passed, "{detail}");
            assert!(detail.starts_with("no timed searches"), "{detail}");
        }
    }

    #[test]
    fn nearest_rank_p95_matches_the_definition_and_never_panics() {
        assert_eq!(nearest_rank_p95(&mut []), None);
        assert_eq!(nearest_rank_p95(&mut [7.0]), Some(7.0));
        // 50 samples: rank ceil(47.5) = 48, so the 48th smallest, not the max
        let mut fifty: Vec<f64> = (1..=50).rev().map(f64::from).collect();
        assert_eq!(nearest_rank_p95(&mut fifty), Some(48.0));
        // 10 samples: rank ceil(9.5) = 10, the max
        let mut ten: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(nearest_rank_p95(&mut ten), Some(10.0));
        // one slow outlier among 50 does not decide the p95
        let mut outlier = vec![0.4; 49];
        outlier.push(300.0);
        assert_eq!(nearest_rank_p95(&mut outlier), Some(0.4));
    }

    #[test]
    fn batch_embed_under_target_is_a_plain_pass() {
        let (passed, detail) = grade_batch_embed(10, 23.0);
        assert!(passed);
        assert_eq!(detail, "2.3ms/text (10 texts in 23ms)");
    }

    #[test]
    fn batch_embed_over_target_is_amber_and_passes() {
        for total_ms in [51.0, 5000.0, 50.0] {
            let (passed, detail) = grade_batch_embed(10, total_ms);
            assert!(passed, "{detail}");
            assert!(detail.starts_with("AMBER (non-gating): "), "{detail}");
            assert!(detail.contains("target <5ms/text"), "{detail}");
            assert!(detail.contains("10 texts"), "{detail}");
        }
        let (_, detail) = grade_batch_embed(10, 51.0);
        assert!(detail.contains("5.1ms/text"), "{detail}");
    }

    #[test]
    fn batch_embed_with_no_vectors_fails() {
        let (passed, detail) = grade_batch_embed(0, 3.0);
        assert!(!passed, "{detail}");
        assert!(detail.contains("no vectors"), "{detail}");
    }

    #[test]
    fn amber_timing_results_never_gate_the_full_eval() {
        let (p_ok, p_detail) = grade_search_latency(Some(311.95), 50, 760_078);
        let (b_ok, b_detail) = grade_batch_embed(10, 51.0);
        let mk = |ok: bool, name: &str, detail: String| {
            if ok {
                EvalResult::pass(name, "performance", 0.0, detail)
            } else {
                EvalResult::fail(name, "performance", 0.0, detail)
            }
        };
        let amber = EvalReport {
            results: vec![
                mk(p_ok, "Search Latency P95", p_detail),
                mk(b_ok, "Batch Embed Speed", b_detail),
            ],
            total_ms: 0.0,
        };
        assert!(!amber.has_gating_failure());
        assert!(!amber.format_text().contains("[FAIL]"));
        // a broken engine still gates
        let (p_ok, p_detail) = grade_search_latency(None, 0, 0);
        let broken = EvalReport {
            results: vec![mk(p_ok, "Search Latency P95", p_detail)],
            total_ms: 0.0,
        };
        assert!(broken.has_gating_failure());
        assert!(broken.format_text().contains("[FAIL]"));
    }

    #[test]
    fn test_eval_result_formatting() {
        let report = EvalReport {
            results: vec![
                EvalResult::pass("Test A", "cat", 1.0, "ok".to_string()),
                EvalResult::fail("Test B", "cat", 2.0, "bad".to_string()),
            ],
            total_ms: 3.0,
        };
        assert_eq!(report.passed(), 1);
        assert_eq!(report.total(), 2);
        let text = report.format_text();
        assert!(text.contains("PASS"));
        assert!(text.contains("FAIL"));
        assert!(text.contains("1/2"));
    }

    #[test]
    fn test_v3_extraction_eval() {
        let result = test_v3_extraction_works();
        assert!(
            result.passed,
            "V3 extraction should pass: {}",
            result.detail
        );
    }

    #[test]
    fn test_ast_analysis_eval() {
        let result = test_ast_analysis_works();
        assert!(result.passed, "AST analysis should pass: {}", result.detail);
    }

    #[test]
    fn test_quality_analysis_eval() {
        let result = test_quality_analysis_works();
        assert!(
            result.passed,
            "Quality analysis should pass: {}",
            result.detail
        );
    }

    // ─── adversarial review finding 6: tool-count gate must name csr_transcript ───

    #[test]
    fn test_tool_count_gate_asserts_csr_transcript_present() {
        let result = test_tool_count();
        assert!(
            result.passed,
            "tool count gate should pass: {}",
            result.detail
        );
        assert!(
            result.detail.contains("csr_transcript present=true"),
            "gate detail must name csr_transcript explicitly, got: {}",
            result.detail
        );
    }

    #[test]
    fn test_csr_transcript_tool_schema_and_annotations() {
        let tool = crate::mcp::CsrServer::find_tool("csr_transcript")
            .expect("csr_transcript must be registered in the rmcp tool router");

        // Schema generation actually ran (not a degenerate/empty schema):
        // the two required params show up as real object-schema properties.
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("csr_transcript input_schema must have a properties object");
        assert!(
            props.contains_key("session"),
            "schema missing 'session' property"
        );
        assert!(props.contains_key("view"), "schema missing 'view' property");

        let annotations = tool
            .annotations
            .as_ref()
            .expect("csr_transcript must declare tool annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));
    }
}
