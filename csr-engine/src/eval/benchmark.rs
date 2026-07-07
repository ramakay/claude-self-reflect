//! Labeled retrieval benchmark for embedding model comparison.
//!
//! Unlike the smoke tests in `eval::run_quick`/`run_full`, this benchmark has
//! ground truth: a corpus of synthetic coding-session memory chunks and
//! queries labeled with the chunk(s) that answer them. It measures retrieval
//! quality (Recall@k, MRR) rather than "did search return anything", so
//! candidate embedding models can be compared head-to-head.
//!
//! Corpus: `src/eval/data/retrieval_v1.json` (embedded at compile time).

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use fastembed::EmbeddingModel;
use serde::Deserialize;

use crate::embeddings::EmbeddingEngine;

const CORPUS_JSON: &str = include_str!("data/retrieval_v1.json");

#[derive(Debug, Deserialize)]
pub struct Corpus {
    pub documents: Vec<Document>,
    pub queries: Vec<Query>,
}

#[derive(Debug, Deserialize)]
pub struct Document {
    pub id: String,
    #[allow(dead_code)]
    pub topic: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct Query {
    #[allow(dead_code)]
    pub id: String,
    pub text: String,
    pub relevant: Vec<String>,
    pub category: String,
}

/// A benchmarkable model: CLI name, fastembed variant, and the retrieval
/// prefixes its training expects (fastembed does not add these itself).
pub struct ModelSpec {
    pub name: &'static str,
    pub model: EmbeddingModel,
    pub query_prefix: &'static str,
    pub passage_prefix: &'static str,
}

/// Models runnable by name via `csr-engine eval --benchmark --models <names>`.
pub fn supported_models() -> Vec<ModelSpec> {
    const BGE_QUERY: &str = "Represent this sentence for searching relevant passages: ";
    vec![
        ModelSpec {
            name: "minilm-l6",
            model: EmbeddingModel::AllMiniLML6V2,
            query_prefix: "",
            passage_prefix: "",
        },
        ModelSpec {
            name: "minilm-l12",
            model: EmbeddingModel::AllMiniLML12V2,
            query_prefix: "",
            passage_prefix: "",
        },
        ModelSpec {
            name: "bge-small",
            model: EmbeddingModel::BGESmallENV15,
            query_prefix: BGE_QUERY,
            passage_prefix: "",
        },
        ModelSpec {
            name: "bge-base",
            model: EmbeddingModel::BGEBaseENV15,
            query_prefix: BGE_QUERY,
            passage_prefix: "",
        },
        ModelSpec {
            name: "arctic-xs",
            model: EmbeddingModel::SnowflakeArcticEmbedXS,
            query_prefix: BGE_QUERY,
            passage_prefix: "",
        },
        ModelSpec {
            name: "e5-small",
            model: EmbeddingModel::MultilingualE5Small,
            query_prefix: "query: ",
            passage_prefix: "passage: ",
        },
        ModelSpec {
            name: "jina-code",
            model: EmbeddingModel::JinaEmbeddingsV2BaseCode,
            query_prefix: "",
            passage_prefix: "",
        },
    ]
}

#[derive(Debug)]
pub struct BenchmarkReport {
    pub model_name: String,
    pub dim: usize,
    pub num_docs: usize,
    pub num_queries: usize,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub mrr_at_10: f64,
    pub per_category: Vec<(String, usize, f64, f64)>, // (category, n, recall@1, recall@5)
    pub embed_docs_ms: f64,
    pub embed_queries_ms: f64,
}

impl BenchmarkReport {
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Model: {} ({}d) — {} docs, {} queries\n",
            self.model_name, self.dim, self.num_docs, self.num_queries
        ));
        out.push_str(&format!(
            "  Recall@1: {:.3}   Recall@5: {:.3}   MRR@10: {:.3}\n",
            self.recall_at_1, self.recall_at_5, self.mrr_at_10
        ));
        out.push_str(&format!(
            "  Embed: docs {:.0}ms, queries {:.0}ms\n",
            self.embed_docs_ms, self.embed_queries_ms
        ));
        for (cat, n, r1, r5) in &self.per_category {
            out.push_str(&format!(
                "    {cat:<16} (n={n:<2})  R@1: {r1:.3}  R@5: {r5:.3}\n"
            ));
        }
        out
    }
}

pub fn load_corpus() -> Result<Corpus> {
    let corpus: Corpus =
        serde_json::from_str(CORPUS_JSON).context("parse embedded retrieval corpus")?;
    validate_corpus(&corpus)?;
    Ok(corpus)
}

fn validate_corpus(corpus: &Corpus) -> Result<()> {
    let ids: std::collections::HashSet<&str> =
        corpus.documents.iter().map(|d| d.id.as_str()).collect();
    if ids.len() != corpus.documents.len() {
        bail!("duplicate document ids in corpus");
    }
    for q in &corpus.queries {
        if q.relevant.is_empty() {
            bail!("query {} has no relevant docs", q.id);
        }
        for r in &q.relevant {
            if !ids.contains(r.as_str()) {
                bail!("query {} references unknown doc {r}", q.id);
            }
        }
    }
    Ok(())
}

/// Run the retrieval benchmark for one model.
pub fn run_model(spec: &ModelSpec, corpus: &Corpus) -> Result<BenchmarkReport> {
    let engine = EmbeddingEngine::new_with_model(spec.model.clone())?;

    let doc_texts: Vec<String> = corpus
        .documents
        .iter()
        .map(|d| format!("{}{}", spec.passage_prefix, d.text))
        .collect();
    let doc_refs: Vec<&str> = doc_texts.iter().map(|s| s.as_str()).collect();
    let t = Instant::now();
    let doc_vecs = engine.embed(&doc_refs)?;
    let embed_docs_ms = t.elapsed().as_secs_f64() * 1000.0;

    let query_texts: Vec<String> = corpus
        .queries
        .iter()
        .map(|q| format!("{}{}", spec.query_prefix, q.text))
        .collect();
    let query_refs: Vec<&str> = query_texts.iter().map(|s| s.as_str()).collect();
    let t = Instant::now();
    let query_vecs = engine.embed(&query_refs)?;
    let embed_queries_ms = t.elapsed().as_secs_f64() * 1000.0;

    let doc_norms: Vec<Vec<f32>> = doc_vecs.iter().map(|v| normalize(v)).collect();

    let mut recall1 = 0.0;
    let mut recall5 = 0.0;
    let mut mrr10 = 0.0;
    // category -> (n, recall@1 hits, recall@5 hits)
    let mut cats: HashMap<String, (usize, f64, f64)> = HashMap::new();

    for (qi, query) in corpus.queries.iter().enumerate() {
        let qv = normalize(&query_vecs[qi]);
        let mut scored: Vec<(usize, f32)> = doc_norms
            .iter()
            .enumerate()
            .map(|(di, dv)| (di, dot(&qv, dv)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let is_relevant = |di: usize| query.relevant.iter().any(|r| r == &corpus.documents[di].id);

        let hit1 = scored
            .first()
            .map(|&(di, _)| is_relevant(di))
            .unwrap_or(false);
        let hit5 = scored.iter().take(5).any(|&(di, _)| is_relevant(di));
        let rank = scored
            .iter()
            .take(10)
            .position(|&(di, _)| is_relevant(di))
            .map(|p| p + 1);

        let r1 = if hit1 { 1.0 } else { 0.0 };
        let r5 = if hit5 { 1.0 } else { 0.0 };
        recall1 += r1;
        recall5 += r5;
        mrr10 += rank.map(|r| 1.0 / r as f64).unwrap_or(0.0);

        let entry = cats.entry(query.category.clone()).or_insert((0, 0.0, 0.0));
        entry.0 += 1;
        entry.1 += r1;
        entry.2 += r5;
    }

    let n = corpus.queries.len() as f64;
    let mut per_category: Vec<(String, usize, f64, f64)> = cats
        .into_iter()
        .map(|(cat, (cn, r1, r5))| (cat, cn, r1 / cn as f64, r5 / cn as f64))
        .collect();
    per_category.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(BenchmarkReport {
        model_name: spec.name.to_string(),
        dim: engine.dim(),
        num_docs: corpus.documents.len(),
        num_queries: corpus.queries.len(),
        recall_at_1: recall1 / n,
        recall_at_5: recall5 / n,
        mrr_at_10: mrr10 / n,
        per_category,
        embed_docs_ms,
        embed_queries_ms,
    })
}

/// Run the benchmark for a comma-separated list of model names.
pub fn run(model_names: &str) -> Result<String> {
    let corpus = load_corpus()?;
    let specs = supported_models();
    let mut out = String::new();
    out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    out.push_str("CSR Retrieval Benchmark (labeled, ground-truth)\n");
    out.push_str(&format!(
        "corpus v1: {} docs, {} queries\n",
        corpus.documents.len(),
        corpus.queries.len()
    ));
    out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\n");

    let mut summary: Vec<(String, f64, f64, f64)> = Vec::new();
    for name in model_names
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        let Some(spec) = specs.iter().find(|s| s.name == name) else {
            let known: Vec<&str> = specs.iter().map(|s| s.name).collect();
            bail!("unknown model '{name}'; supported: {}", known.join(", "));
        };
        let report = run_model(spec, &corpus)?;
        summary.push((
            report.model_name.clone(),
            report.recall_at_1,
            report.recall_at_5,
            report.mrr_at_10,
        ));
        out.push_str(&report.format_text());
        out.push('\n');
    }

    if summary.len() > 1 {
        out.push_str("Summary (higher is better):\n");
        out.push_str(&format!(
            "  {:<12} {:>8} {:>8} {:>8}\n",
            "model", "R@1", "R@5", "MRR@10"
        ));
        for (name, r1, r5, mrr) in &summary {
            out.push_str(&format!("  {name:<12} {r1:>8.3} {r5:>8.3} {mrr:>8.3}\n"));
        }
    }
    out.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    Ok(out)
}

fn normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_parses_and_validates() {
        let corpus = load_corpus().expect("embedded corpus must be valid");
        assert!(corpus.documents.len() >= 100, "corpus too small");
        assert!(corpus.queries.len() >= 50, "too few queries");
    }

    #[test]
    fn model_names_are_unique() {
        let specs = supported_models();
        let mut names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), specs.len());
    }

    #[test]
    fn normalize_and_dot() {
        let v = normalize(&[3.0, 4.0]);
        assert!((dot(&v, &v) - 1.0).abs() < 1e-6);
    }
}
