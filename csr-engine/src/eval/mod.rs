//! Evaluation framework for csr-engine.
//!
//! Quick mode: 5 core tests (<30s)
//! Full mode: 20 tests (~2 min)
//! Continuity mode: the North Star gate — CSR must recall its own vision with
//! provenance, beating a grep baseline (`csr-engine eval --continuity`).

pub mod codegraph;
pub mod continuity;
pub mod provenance;

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

    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        out.push_str("CSR Engine Evaluation Report\n");
        out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\n");

        for r in &self.results {
            let status = if r.passed { "PASS" } else { "FAIL" };
            out.push_str(&format!(
                "  [{status}] {:<30} ({:.0}ms) {}\n",
                r.name, r.duration_ms, r.detail
            ));
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

/// Run quick evaluation (5 tests).
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

    EvalReport {
        results,
        total_ms: start.elapsed().as_secs_f64() * 1000.0,
    }
}

/// Run full evaluation (20 tests).
pub async fn run_full(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    index_dir: &Path,
) -> EvalReport {
    let start = Instant::now();
    let mut results = Vec::new();

    // Quick tests (5)
    results.push(test_db_connectivity(storage));
    results.push(test_search_accuracy(storage, embeddings, search).await);
    results.push(test_performance(embeddings, search).await);
    results.push(test_cache_status(index_dir));
    results.push(test_tool_count());

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
    let manifest_path = index_dir.join("index_manifest.json");

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
    drop(se);

    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let chunks_match = db_chunks == idx_chunks;
    let reflections_match = db_reflections == idx_reflections;

    if chunks_match && reflections_match {
        EvalResult::pass(
            "Index Consistency",
            "data",
            ms,
            format!("DB={db_chunks}c/{db_reflections}r, HNSW={idx_chunks}c/{idx_reflections}r"),
        )
    } else {
        EvalResult::fail(
            "Index Consistency",
            "data",
            ms,
            format!(
                "MISMATCH: DB={db_chunks}c/{db_reflections}r, HNSW={idx_chunks}c/{idx_reflections}r"
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

fn test_batch_embedding_speed(embeddings: &Arc<EmbeddingEngine>) -> EvalResult {
    let t = Instant::now();
    let texts: Vec<String> = (0..10)
        .map(|i| format!("Test sentence number {i} for batch embedding speed evaluation"))
        .collect();
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

    match embeddings.embed(&text_refs) {
        Ok(vecs) => {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let per_text = ms / vecs.len() as f64;
            if per_text < 5.0 {
                EvalResult::pass(
                    "Batch Embed Speed",
                    "performance",
                    ms,
                    format!(
                        "{:.1}ms/text ({} texts in {:.0}ms)",
                        per_text,
                        vecs.len(),
                        ms
                    ),
                )
            } else {
                EvalResult::fail(
                    "Batch Embed Speed",
                    "performance",
                    ms,
                    format!("slow: {per_text:.1}ms/text (target <5ms)"),
                )
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

    let mut latencies = Vec::new();
    let se = search.read().await;
    for v in &vecs {
        let qt = Instant::now();
        let _r = se.search_chunks(v, 5, 0.0);
        latencies.push(qt.elapsed().as_secs_f64() * 1000.0);
    }
    drop(se);

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95_idx = (latencies.len() as f64 * 0.95).ceil() as usize - 1;
    let p95 = latencies
        .get(p95_idx.min(latencies.len() - 1))
        .copied()
        .unwrap_or(0.0);
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    if p95 < 10.0 {
        EvalResult::pass(
            "Search Latency P95",
            "performance",
            ms,
            format!("p95: {p95:.2}ms ({} queries)", latencies.len()),
        )
    } else {
        EvalResult::fail(
            "Search Latency P95",
            "performance",
            ms,
            format!("p95: {p95:.2}ms (target <10ms)"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
