//! `csr-engine embed sample|compare|query-bench`: embed a JSONL sample with one
//! of the S4 models, compare two vector files, and time single-query embeds.
//!
//! Vectors-file layout (one file per width `d`):
//! - `<prefix>.d<d>.f32`: raw little-endian f32, row-major, `n x d`, each row
//!   L2-normalised (Matryoshka widths are cut then re-normalised).
//! - `<prefix>.ids`: one `chunk_id` per line, input order. Row i of every
//!   `.f32` file belongs to line i.
//! - `<prefix>.meta.json`: written last; its presence marks a finished run.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::embeddings::onnx_embed::{truncate_mrl, Kind, OnnxEmbedder, DEFAULT_MAX_TOKENS};
use crate::embeddings::EmbeddingEngine;

// ---------------------------------------------------------------- presets

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    GemmaQ8,
    GemmaFp32,
    GraniteInt8,
    GraniteFp32,
    MiniLm,
}

impl Preset {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "gemma-q8" => Preset::GemmaQ8,
            "gemma-fp32" => Preset::GemmaFp32,
            "granite-int8" => Preset::GraniteInt8,
            "granite-fp32" => Preset::GraniteFp32,
            "minilm" => Preset::MiniLm,
            other => bail!(
                "unknown --model {other:?}; expected gemma-q8, gemma-fp32, granite-int8, granite-fp32 or minilm"
            ),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Preset::GemmaQ8 => "gemma-q8",
            Preset::GemmaFp32 => "gemma-fp32",
            Preset::GraniteInt8 => "granite-int8",
            Preset::GraniteFp32 => "granite-fp32",
            Preset::MiniLm => "minilm",
        }
    }

    /// Model directory name under the model root and the graph file inside it.
    /// `None` for MiniLM, which goes through `EmbeddingEngine`.
    pub fn files(self) -> Option<(&'static str, &'static str)> {
        match self {
            Preset::GemmaQ8 => Some(("embeddinggemma-2-onnx", "onnx/model_quantized.onnx")),
            Preset::GemmaFp32 => Some(("embeddinggemma-2-onnx", "onnx/model.onnx")),
            Preset::GraniteInt8 => Some((
                "granite-embedding-97m-multilingual-r2",
                "onnx/model_quint8_avx2.onnx",
            )),
            Preset::GraniteFp32 => {
                Some(("granite-embedding-97m-multilingual-r2", "onnx/model.onnx"))
            }
            Preset::MiniLm => None,
        }
    }

    pub fn native_dim(self) -> usize {
        match self {
            Preset::GemmaQ8 | Preset::GemmaFp32 => 768,
            Preset::GraniteInt8 | Preset::GraniteFp32 | Preset::MiniLm => 384,
        }
    }

    fn needs_prompts(self) -> bool {
        matches!(self, Preset::GemmaQ8 | Preset::GemmaFp32)
    }
}

/// Default model root: `$HOME/.claude-self-reflect/models`.
pub fn default_model_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude-self-reflect")
        .join("models")
}

/// One loaded model behind a single `embed` call.
pub enum Backend {
    Onnx(Box<OnnxEmbedder>),
    MiniLm(Box<EmbeddingEngine>),
}

impl Backend {
    pub fn load(
        preset: Preset,
        model_root: &Path,
        threads: usize,
        max_tokens: usize,
    ) -> Result<Self> {
        match preset.files() {
            None => {
                let engine = EmbeddingEngine::new()?;
                engine.warm()?;
                Ok(Backend::MiniLm(Box::new(engine)))
            }
            Some((dir, file)) => {
                let emb = OnnxEmbedder::load(&model_root.join(dir), file, threads)?
                    .with_max_tokens(max_tokens)?;
                if preset.needs_prompts() && emb.prompts().is_none() {
                    bail!(
                        "{}: config_sentence_transformers.json has no document/query prompts",
                        preset.name()
                    );
                }
                Ok(Backend::Onnx(Box::new(emb)))
            }
        }
    }

    pub fn embed(&self, texts: &[&str], kind: Kind) -> Result<Vec<Vec<f32>>> {
        match self {
            Backend::Onnx(e) => e.embed(texts, kind),
            Backend::MiniLm(e) => {
                let mut rows = e.embed(texts)?;
                for r in rows.iter_mut() {
                    crate::embeddings::onnx_embed::l2_normalize(r);
                }
                Ok(rows)
            }
        }
    }
}

// ---------------------------------------------------------------- vector files

/// Appends rows of one width to a `.f32` file, flushing on demand.
pub struct VectorsWriter {
    out: BufWriter<File>,
    d: usize,
    rows: usize,
}

impl VectorsWriter {
    pub fn create(path: &Path, d: usize) -> Result<Self> {
        let f = File::create(path).with_context(|| format!("create {}", path.display()))?;
        Ok(Self {
            out: BufWriter::new(f),
            d,
            rows: 0,
        })
    }

    pub fn append(&mut self, row: &[f32]) -> Result<()> {
        if row.len() != self.d {
            bail!("row has {} dims, writer expects {}", row.len(), self.d);
        }
        for x in row {
            self.out.write_all(&x.to_le_bytes())?;
        }
        self.rows += 1;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.out.flush()?;
        Ok(())
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
}

/// Read an `n x d` little-endian f32 file. Errors if the size is not a whole
/// number of rows.
pub fn read_vectors(path: &Path, d: usize) -> Result<Vec<Vec<f32>>> {
    let mut buf = Vec::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read_to_end(&mut buf)?;
    if d == 0 || buf.len() % (d * 4) != 0 {
        bail!(
            "{}: {} bytes is not a whole number of {d}-dim f32 rows",
            path.display(),
            buf.len()
        );
    }
    Ok(buf
        .chunks_exact(d * 4)
        .map(|row| {
            row.as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        })
        .collect())
}

pub fn read_ids(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(text.lines().map(str::to_string).collect())
}

// ---------------------------------------------------------------- compare stats

/// Linear-interpolated percentile of an ascending slice, `q` in `[0, 1]`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Per-row cosine between two row-aligned sets (rows matched by chunk_id).
/// Rows present in only one side are skipped and counted.
pub fn compare_stats(
    a: &[Vec<f32>],
    ids_a: &[String],
    b: &[Vec<f32>],
    ids_b: &[String],
) -> Result<Value> {
    if a.len() != ids_a.len() || b.len() != ids_b.len() {
        bail!(
            "ids/vectors length mismatch: a {} rows vs {} ids, b {} rows vs {} ids",
            a.len(),
            ids_a.len(),
            b.len(),
            ids_b.len()
        );
    }
    let mut index_b: HashMap<&str, usize> = HashMap::with_capacity(ids_b.len());
    for (i, id) in ids_b.iter().enumerate() {
        index_b.entry(id.as_str()).or_insert(i);
    }
    let mut cos = Vec::new();
    let mut missing = 0usize;
    for (i, id) in ids_a.iter().enumerate() {
        match index_b.get(id.as_str()) {
            Some(j) => cos.push(cosine(&a[i], &b[*j])),
            None => missing += 1,
        }
    }
    if cos.is_empty() {
        bail!("no chunk_id appears in both id files");
    }
    let below = cos.iter().filter(|c| **c < 0.999).count();
    let mean = cos.iter().sum::<f64>() / cos.len() as f64;
    cos.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    Ok(json!({
        "n": cos.len(),
        "n_a": ids_a.len(),
        "n_b": ids_b.len(),
        "missing_in_b": missing,
        "min": cos[0],
        "p01": percentile(&cos, 0.01),
        "p05": percentile(&cos, 0.05),
        "mean": mean,
        "median": percentile(&cos, 0.5),
        "count_below_0_999": below,
    }))
}

pub struct CompareConfig {
    pub a: PathBuf,
    pub b: PathBuf,
    pub ids_a: PathBuf,
    pub ids_b: PathBuf,
    pub dims: usize,
    pub out: PathBuf,
}

pub fn run_compare(cfg: CompareConfig) -> Result<Value> {
    let a = read_vectors(&cfg.a, cfg.dims)?;
    let b = read_vectors(&cfg.b, cfg.dims)?;
    let ids_a = read_ids(&cfg.ids_a)?;
    let ids_b = read_ids(&cfg.ids_b)?;
    let mut stats = compare_stats(&a, &ids_a, &b, &ids_b)?;
    stats["dims"] = json!(cfg.dims);
    stats["a"] = json!(cfg.a.display().to_string());
    stats["b"] = json!(cfg.b.display().to_string());
    write_json(&cfg.out, &stats)?;
    Ok(stats)
}

// ---------------------------------------------------------------- sample

pub struct SampleConfig {
    pub preset: Preset,
    pub model_root: PathBuf,
    pub dims: Vec<usize>,
    pub input: PathBuf,
    pub out_prefix: PathBuf,
    pub limit: Option<usize>,
    pub batch: usize,
    pub threads: usize,
    pub max_tokens: usize,
    pub force: bool,
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn host_name() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn prefixed(prefix: &Path, suffix: &str) -> PathBuf {
    let mut s = prefix.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

#[derive(serde::Deserialize)]
struct SampleRow {
    chunk_id: String,
    text: String,
}

/// Pass 1: count non-empty lines (up to `limit`). The input sha256 covers the
/// whole file so it identifies the sample regardless of `--limit`.
fn count_rows(path: &Path, limit: Option<usize>) -> Result<(usize, String)> {
    let mut f =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut h = Sha256::new();
    let mut n = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = f.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        h.update(&line);
        if !line.iter().all(u8::is_ascii_whitespace) {
            n += 1;
        }
    }
    let n = limit.map_or(n, |l| n.min(l));
    Ok((n, hex(&h.finalize())))
}

pub fn run_sample(cfg: SampleConfig) -> Result<Value> {
    if cfg.dims.is_empty() {
        bail!("--dims needs at least one width");
    }
    if cfg.batch == 0 {
        bail!("--batch must be at least 1");
    }
    let native = cfg.preset.native_dim();
    for d in &cfg.dims {
        if *d == 0 || *d > native {
            bail!(
                "--dims {d} is outside 1..={native}, the native width of {}",
                cfg.preset.name()
            );
        }
    }
    let meta_path = prefixed(&cfg.out_prefix, ".meta.json");
    if meta_path.exists() && !cfg.force {
        bail!(
            "{} already exists (finished run); pass --force to overwrite",
            meta_path.display()
        );
    }
    if let Some(parent) = cfg.out_prefix.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    // A forced rerun must not leave a stale meta beside half-written vectors.
    if meta_path.exists() {
        std::fs::remove_file(&meta_path)?;
    }

    let (total, input_sha) = count_rows(&cfg.input, cfg.limit)?;
    if total == 0 {
        bail!("{} has no rows", cfg.input.display());
    }

    let t_load = Instant::now();
    let backend = Backend::load(cfg.preset, &cfg.model_root, cfg.threads, cfg.max_tokens)?;
    let load_s = t_load.elapsed().as_secs_f64();

    let mut writers: Vec<(usize, VectorsWriter)> = Vec::new();
    for d in &cfg.dims {
        let path = prefixed(&cfg.out_prefix, &format!(".d{d}.f32"));
        writers.push((*d, VectorsWriter::create(&path, *d)?));
    }
    let ids_path = prefixed(&cfg.out_prefix, ".ids");
    let mut ids_out = BufWriter::new(
        File::create(&ids_path).with_context(|| format!("create {}", ids_path.display()))?,
    );

    let reader = BufReader::new(File::open(&cfg.input)?);
    let mut lines = reader.lines().enumerate();
    let started = Instant::now();
    let mut done = 0usize;
    let mut last_logged = 0usize;
    while done < total {
        let mut batch: Vec<SampleRow> = Vec::with_capacity(cfg.batch);
        while batch.len() < cfg.batch && done + batch.len() < total {
            let Some((i, line)) = lines.next() else { break };
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let row: SampleRow = serde_json::from_str(&line).with_context(|| {
                format!(
                    "{} line {}: need chunk_id and text",
                    cfg.input.display(),
                    i + 1
                )
            })?;
            batch.push(row);
        }
        if batch.is_empty() {
            break;
        }
        // Sort by length inside the batch so padding is small; rows are
        // written back in input order.
        let mut order: Vec<usize> = (0..batch.len()).collect();
        order.sort_by_key(|i| batch[*i].text.len());
        let texts: Vec<&str> = order.iter().map(|i| batch[*i].text.as_str()).collect();
        let sorted_rows = backend.embed(&texts, Kind::Doc)?;
        if sorted_rows.len() != batch.len() {
            bail!(
                "embedder returned {} rows for {} texts",
                sorted_rows.len(),
                batch.len()
            );
        }
        let mut rows: Vec<Vec<f32>> = vec![Vec::new(); batch.len()];
        for (pos, i) in order.iter().enumerate() {
            rows[*i] = sorted_rows[pos].clone();
        }
        for (r, row) in rows.iter().enumerate() {
            if row.len() < *cfg.dims.iter().max().unwrap_or(&0) {
                bail!(
                    "model returned {} dims, fewer than the requested widths",
                    row.len()
                );
            }
            writeln!(ids_out, "{}", batch[r].chunk_id)?;
            for (d, w) in writers.iter_mut() {
                if *d == row.len() {
                    w.append(row)?;
                } else {
                    w.append(&truncate_mrl(row, *d))?;
                }
            }
        }
        for (_, w) in writers.iter_mut() {
            w.flush()?;
        }
        ids_out.flush()?;
        done += batch.len();
        if done / 256 > last_logged / 256 {
            let el = started.elapsed().as_secs_f64().max(1e-9);
            let rate = done as f64 / el;
            eprintln!(
                "progress {done}/{total} {rate:.2} eta {:.0}",
                (total - done) as f64 / rate
            );
        }
        last_logged = done;
    }
    if done != total {
        bail!("input ended after {done} rows, expected {total}");
    }
    let elapsed = started.elapsed().as_secs_f64().max(1e-9);

    let (onnx_file, onnx_sha, onnx_data_sha, pooling, prompts, truncated, tokens) = match &backend {
        Backend::Onnx(e) => {
            let p = e.onnx_path();
            let mut data_name = p.as_os_str().to_os_string();
            data_name.push("_data");
            let data_path = PathBuf::from(data_name);
            let data_sha = if data_path.exists() {
                Some(sha256_file(&data_path)?)
            } else {
                None
            };
            (
                Some(p.display().to_string()),
                Some(sha256_file(p)?),
                data_sha,
                e.pooling().as_str().to_string(),
                e.prompts()
                    .map(|p| json!({"document": p.document, "query": p.query})),
                Some(e.truncated_units()),
                Some(e.tokens_seen()),
            )
        }
        Backend::MiniLm(_) => (None, None, None, "mean".to_string(), None, None, None),
    };
    let meta = json!({
        "model": match cfg.preset.files() {
            Some((dir, _)) => dir,
            None => "all-MiniLM-L6-v2 (fastembed EmbeddingEngine)",
        },
        "preset": cfg.preset.name(),
        "onnx_file": onnx_file,
        "onnx_sha256": onnx_sha,
        "onnx_data_sha256": onnx_data_sha,
        "pooling": pooling,
        "prompts": prompts,
        "dims": cfg.dims,
        "native_dim": native,
        "max_tokens": cfg.max_tokens,
        "truncated_units": truncated,
        "n": done,
        "input": cfg.input.display().to_string(),
        "input_sha256": input_sha,
        "limit": cfg.limit,
        "threads": if cfg.threads == 0 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        } else {
            cfg.threads
        },
        "batch": cfg.batch,
        "load_s": load_s,
        "elapsed_s": elapsed,
        "units_per_s": done as f64 / elapsed,
        "tokens_per_s": tokens.map(|t| t as f64 / elapsed),
        "host": host_name(),
    });
    write_json(&meta_path, &meta)?;
    Ok(meta)
}

// ---------------------------------------------------------------- query-bench

pub struct QueryBenchConfig {
    pub preset: Preset,
    pub model_root: PathBuf,
    pub queries: PathBuf,
    pub dims: Option<usize>,
    pub threads: usize,
    pub max_tokens: usize,
    pub out: PathBuf,
}

/// Query strings from `{"queries": ["..."]}`, `{"queries": [{"query": ".."}]}`,
/// `{"probes": [{"query": ".."}]}` or a bare array of either shape.
pub fn parse_queries(text: &str) -> Result<Vec<String>> {
    let v: Value = serde_json::from_str(text).context("parse queries json")?;
    let arr = match &v {
        Value::Array(a) => a,
        Value::Object(o) => o
            .get("queries")
            .or_else(|| o.get("probes"))
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("queries json needs a \"queries\" or \"probes\" array"))?,
        _ => bail!("queries json must be an array or object"),
    };
    let out: Vec<String> = arr
        .iter()
        .filter_map(|q| match q {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("query").and_then(Value::as_str).map(str::to_string),
            _ => None,
        })
        .collect();
    if out.is_empty() {
        bail!("no queries found");
    }
    Ok(out)
}

pub fn run_query_bench(cfg: QueryBenchConfig) -> Result<Value> {
    let queries = parse_queries(
        &std::fs::read_to_string(&cfg.queries)
            .with_context(|| format!("read {}", cfg.queries.display()))?,
    )?;
    let t_load = Instant::now();
    let backend = Backend::load(cfg.preset, &cfg.model_root, cfg.threads, cfg.max_tokens)?;
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
    let native = cfg.preset.native_dim();
    if let Some(d) = cfg.dims {
        if d == 0 || d > native {
            bail!("--dims {d} is outside 1..={native}");
        }
    }
    let one = |q: &str| -> Result<()> {
        let rows = backend.embed(&[q], Kind::Query)?;
        if let (Some(d), Some(r)) = (cfg.dims, rows.first()) {
            std::hint::black_box(truncate_mrl(r, d));
        }
        Ok(())
    };
    for i in 0..3 {
        one(&queries[i % queries.len()])?;
    }
    let mut ms = Vec::with_capacity(queries.len());
    for q in &queries {
        let t = Instant::now();
        one(q)?;
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let report = json!({
        "model": cfg.preset.name(),
        "n": ms.len(),
        "dims": cfg.dims.unwrap_or(native),
        "p50_ms": percentile(&ms, 0.5),
        "p95_ms": percentile(&ms, 0.95),
        "max_ms": ms.last().copied().unwrap_or(0.0),
        "load_ms": load_ms,
        "threads": if cfg.threads == 0 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        } else {
            cfg.threads
        },
        "host": host_name(),
    });
    write_json(&cfg.out, &report)?;
    Ok(report)
}

/// Re-exported so the CLI default matches the embedder's.
pub const DEFAULT_MAX: usize = DEFAULT_MAX_TOKENS;

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("embed_test_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn embed_vectors_file_roundtrip_keeps_shape_and_order() {
        let dir = tmpdir("roundtrip");
        let path = dir.join("v.d3.f32");
        let rows = vec![vec![1.0f32, 2.0, 3.0], vec![-4.5, 0.0, 6.25]];
        let mut w = VectorsWriter::create(&path, 3).unwrap();
        for r in &rows {
            w.append(r).unwrap();
        }
        w.flush().unwrap();
        assert_eq!(w.rows(), 2);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2 * 3 * 4);
        assert_eq!(read_vectors(&path, 3).unwrap(), rows);
        // First four bytes are the little-endian encoding of the first value.
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(&raw[..4], &1.0f32.to_le_bytes());
        // A wrong width is rejected, not silently reshaped.
        assert!(read_vectors(&path, 4).is_err());
        assert!(w.append(&[1.0, 2.0]).is_err());

        let ids = dir.join("v.ids");
        std::fs::write(&ids, "id-b\nid-a\n").unwrap();
        assert_eq!(read_ids(&ids).unwrap(), vec!["id-b", "id-a"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn embed_compare_stats_exact_on_hand_built_pair() {
        // Cosines against the unit x axis: 1, 0.5, 0, -1 (rows aligned by id).
        let a = vec![vec![1.0f32, 0.0, 0.0, 0.0]; 4];
        // b is in a different order; ids realign it.
        let b = vec![
            vec![-1.0f32, 0.0, 0.0, 0.0], // d
            vec![1.0, 0.0, 0.0, 0.0],     // a
            vec![0.0, 1.0, 0.0, 0.0],     // c
            vec![0.5, 0.5, 0.5, 0.5],     // b
        ];
        let ids_a: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let ids_b: Vec<String> = ["d", "a", "c", "b"].iter().map(|s| s.to_string()).collect();
        let s = compare_stats(&a, &ids_a, &b, &ids_b).unwrap();
        assert_eq!(s["n"], 4);
        assert_eq!(s["min"].as_f64().unwrap(), -1.0);
        // sorted: -1, 0, 0.5, 1
        assert!((s["mean"].as_f64().unwrap() - 0.125).abs() < 1e-12);
        // p01 = -1 + 0.01 * 3 * (0 - -1) = -0.97
        assert!((s["p01"].as_f64().unwrap() - (-0.97)).abs() < 1e-12);
        // p05 = -1 + 0.05 * 3 = -0.85
        assert!((s["p05"].as_f64().unwrap() - (-0.85)).abs() < 1e-12);
        // median = (0 + 0.5) / 2
        assert!((s["median"].as_f64().unwrap() - 0.25).abs() < 1e-12);
        assert_eq!(s["count_below_0_999"], 3);
        // Ids missing from b are counted, not matched by position.
        let s2 = compare_stats(&a, &ids_a, &b[..3], &ids_b[..3]).unwrap();
        assert_eq!(s2["n"], 3);
        assert_eq!(s2["missing_in_b"], 1);
    }

    #[test]
    fn embed_preset_parse_and_paths() {
        assert_eq!(Preset::parse("gemma-q8").unwrap(), Preset::GemmaQ8);
        assert_eq!(
            Preset::GemmaQ8.files(),
            Some(("embeddinggemma-2-onnx", "onnx/model_quantized.onnx"))
        );
        assert_eq!(
            Preset::GraniteInt8.files(),
            Some((
                "granite-embedding-97m-multilingual-r2",
                "onnx/model_quint8_avx2.onnx"
            ))
        );
        assert_eq!(Preset::MiniLm.files(), None);
        assert_eq!(Preset::GemmaFp32.native_dim(), 768);
        assert_eq!(Preset::GraniteFp32.native_dim(), 384);
        assert!(Preset::parse("gemma-fp16").is_err());
    }

    #[test]
    fn embed_parse_queries_accepts_audit_and_probe_shapes() {
        let audit = parse_queries(r#"{"queries": ["one", "two"]}"#).unwrap();
        assert_eq!(audit, vec!["one", "two"]);
        let probes = parse_queries(
            r#"{"schema":"x","probes":[{"id":1,"query":"q1"},{"id":2,"query":"q2"}]}"#,
        )
        .unwrap();
        assert_eq!(probes, vec!["q1", "q2"]);
        assert!(parse_queries("{}").is_err());
    }

    #[test]
    fn embed_percentile_interpolates() {
        let v = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 0.5), 2.5);
        assert_eq!(percentile(&v, 1.0), 4.0);
    }

    /// Needs the Gemma Q8 files under the model root (`$HOME/.claude-self-reflect/models`).
    #[test]
    #[ignore]
    fn embed_gemma_q8_loads_from_model_root() {
        let backend = Backend::load(
            Preset::GemmaQ8,
            &default_model_root(),
            0,
            DEFAULT_MAX_TOKENS,
        )
        .expect("load gemma-q8");
        let Backend::Onnx(e) = &backend else {
            panic!("gemma is an ort model")
        };
        assert!(
            e.prompts().is_some(),
            "gemma prompts come from config_sentence_transformers.json"
        );
        let rows = backend
            .embed(
                &["hello world", "a second, longer sentence about nothing"],
                Kind::Doc,
            )
            .expect("embed");
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.len(), 768);
            let n: f64 = r
                .iter()
                .map(|x| (*x as f64) * (*x as f64))
                .sum::<f64>()
                .sqrt();
            assert!((n - 1.0).abs() < 1e-4, "norm {n}");
        }
    }
}
