//! Cross-encoder scorer for `cross-encoder/ettin-reranker-32m-v1`
//! (ModernBERT, INT8 arm64 ONNX) plus the single retrieval-policy switch.
//!
//! The ONNX graph stops at `last_hidden_state`. The sentence-transformers head
//! (CLS pooling, Dense+GELU, LayerNorm, Dense -> logit) is applied here in Rust
//! from the small safetensors files that ship with the model.
//!
//! The tuned model (`assets/models/ettin-tuned-run2`) is compiled into the
//! binary and loaded from memory: nothing is downloaded or written to disk.
//! `CSR_CE_MODEL_DIR`, when set to a non-empty path, loads that directory
//! instead; `global()` then returns `None` if its files are absent.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use ort::session::builder::AutoDevicePolicy;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{
    EncodeInput, Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy,
};

pub const CE_MODEL_ID: &str = "cross-encoder/ettin-reranker-32m-v1";
/// Candidate pool size handed to the cross-encoder per query.
pub const CE_POOL: usize = 24;
/// Max tokens per (query, unit) pair; the unit is truncated, never the query.
pub const CE_MAX_TOKENS: usize = 512;
/// Pairs per `session.run`. ORT's CPU arena grows to the largest run it has
/// seen and never gives it back, and attention memory is linear in the batch,
/// so a run of 4 keeps the arena near a sixth of a run of the whole pool.
pub const CE_RUN_BATCH: usize = 4;

const HIDDEN: usize = 384;
const LN_EPS: f32 = 1e-5;

/// True iff `CSR_RETRIEVAL_LEGACY == "1"`.
pub fn retrieval_legacy() -> bool {
    RetrievalPolicy::from_env() == RetrievalPolicy::Legacy
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetrievalPolicy {
    Legacy,
    CrossEncoder,
}

impl RetrievalPolicy {
    /// The only place `CSR_RETRIEVAL_LEGACY` is read.
    pub fn from_env() -> Self {
        Self::parse(std::env::var("CSR_RETRIEVAL_LEGACY").ok().as_deref())
    }

    pub(crate) fn parse(value: Option<&str>) -> Self {
        if value == Some("1") {
            Self::Legacy
        } else {
            Self::CrossEncoder
        }
    }
}

/// The tuned model, compiled in. Directory layout is the sentence-transformers
/// one, so the same files also load through `CSR_CE_MODEL_DIR`.
mod embedded {
    pub static ONNX: &[u8] =
        include_bytes!("../../assets/models/ettin-tuned-run2/model_qint8_arm64.onnx");
    pub static TOKENIZER: &[u8] =
        include_bytes!("../../assets/models/ettin-tuned-run2/tokenizer.json");
    pub static DENSE1: &[u8] =
        include_bytes!("../../assets/models/ettin-tuned-run2/2_Dense/model.safetensors");
    pub static LAYER_NORM: &[u8] =
        include_bytes!("../../assets/models/ettin-tuned-run2/3_LayerNorm/model.safetensors");
    pub static DENSE2: &[u8] =
        include_bytes!("../../assets/models/ettin-tuned-run2/4_Dense/model.safetensors");
}

/// The ONNX graph compiled into the binary (what the label bands are keyed on
/// when no override directory is set).
pub fn embedded_onnx() -> &'static [u8] {
    embedded::ONNX
}

/// Where the reranker files come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelSource {
    /// Compiled into the binary.
    Embedded,
    /// `CSR_CE_MODEL_DIR`.
    Dir(PathBuf),
}

impl ModelSource {
    /// `CSR_CE_MODEL_DIR` when set and non-empty, else the compiled-in model.
    pub fn from_env() -> Self {
        match std::env::var_os("CSR_CE_MODEL_DIR") {
            Some(dir) if !dir.is_empty() => Self::Dir(PathBuf::from(dir)),
            _ => Self::Embedded,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Embedded => "embedded".to_string(),
            Self::Dir(dir) => format!("dir:{}", dir.display()),
        }
    }
}

/// Where the ONNX graph comes from.
enum OnnxSource {
    /// Compiled in; handed to ORT from memory.
    Embedded,
    /// A file in an override directory.
    File(PathBuf),
}

/// The files a scorer needs: the graph by source, the rest as bytes.
struct ModelBytes {
    onnx: OnnxSource,
    tokenizer: Cow<'static, [u8]>,
    dense1: Cow<'static, [u8]>,
    layer_norm: Cow<'static, [u8]>,
    dense2: Cow<'static, [u8]>,
}

impl ModelBytes {
    fn embedded() -> Self {
        Self {
            onnx: OnnxSource::Embedded,
            tokenizer: Cow::Borrowed(embedded::TOKENIZER),
            dense1: Cow::Borrowed(embedded::DENSE1),
            layer_norm: Cow::Borrowed(embedded::LAYER_NORM),
            dense2: Cow::Borrowed(embedded::DENSE2),
        }
    }

    fn from_dir(dir: &Path) -> Result<Self> {
        let read = |rel: &str| -> Result<Cow<'static, [u8]>> {
            let path = dir.join(rel);
            std::fs::read(&path)
                .map(Cow::Owned)
                .with_context(|| format!("read {}", path.display()))
        };
        Ok(Self {
            onnx: OnnxSource::File(dir.join("model_qint8_arm64.onnx")),
            tokenizer: read("tokenizer.json")?,
            dense1: read("2_Dense/model.safetensors")?,
            layer_norm: read("3_LayerNorm/model.safetensors")?,
            dense2: read("4_Dense/model.safetensors")?,
        })
    }

    fn load(source: &ModelSource) -> Result<Self> {
        match source {
            ModelSource::Embedded => Ok(Self::embedded()),
            ModelSource::Dir(dir) => Self::from_dir(dir),
        }
    }
}

// ---------------------------------------------------------------- stats

static LOADED: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicU64 = AtomicU64::new(0);
static PAIRS: AtomicU64 = AtomicU64::new(0);
static TOTAL_MS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CeStats {
    pub loaded: bool,
    pub calls: u64,
    pub pairs: u64,
    pub total_ms: u64,
}

pub fn ce_stats() -> CeStats {
    CeStats {
        loaded: LOADED.load(Ordering::Relaxed),
        calls: CALLS.load(Ordering::Relaxed),
        pairs: PAIRS.load(Ordering::Relaxed),
        total_ms: TOTAL_MS.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------- head math

/// Exact GELU (erf form, torch default).
fn gelu(x: f32) -> f32 {
    let x = x as f64;
    (0.5 * x * (1.0 + erf(x / std::f64::consts::SQRT_2))) as f32
}

/// erf via the Maclaurin series for small |x| and a continued fraction for the
/// tail; accurate to well under 1e-9 over the range GELU needs.
fn erf(x: f64) -> f64 {
    let ax = x.abs();
    let v = if ax < 2.5 {
        // erf(x) = 2/sqrt(pi) * sum_{n} (-1)^n x^(2n+1) / (n! (2n+1))
        let mut term = ax;
        let mut sum = ax;
        let x2 = ax * ax;
        for n in 1..80 {
            term *= -x2 / n as f64;
            let add = term / (2 * n + 1) as f64;
            sum += add;
            if add.abs() < 1e-17 {
                break;
            }
        }
        sum * 2.0 / std::f64::consts::PI.sqrt()
    } else if ax > 6.0 {
        1.0
    } else {
        // erfc(x) = exp(-x^2)/(x sqrt(pi)) * 1/(1+ 1/(2x^2)/(1+2/(2x^2)/(1+...)))
        let mut f = 0.0;
        for k in (1..=60).rev() {
            f = (k as f64 / 2.0) / (ax + f);
        }
        let erfc = (-ax * ax).exp() / (std::f64::consts::PI.sqrt() * (ax + f));
        1.0 - erfc
    };
    if x < 0.0 {
        -v
    } else {
        v
    }
}

/// Biased-variance LayerNorm with affine params.
fn layer_norm(x: &[f32], weight: &[f32], bias: &[f32], eps: f32) -> Vec<f32> {
    let n = x.len() as f32;
    let mean = x.iter().sum::<f32>() / n;
    let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
    let inv = 1.0 / (var + eps).sqrt();
    x.iter()
        .zip(weight.iter().zip(bias.iter()))
        .map(|(v, (w, b))| (v - mean) * inv * w + b)
        .collect()
}

/// y = W x for row-major W [out, inp].
fn matvec(w: &[f32], x: &[f32], out: usize) -> Vec<f32> {
    let inp = x.len();
    (0..out)
        .map(|i| {
            w[i * inp..(i + 1) * inp]
                .iter()
                .zip(x.iter())
                .map(|(a, b)| a * b)
                .sum()
        })
        .collect()
}

// ---------------------------------------------------------------- safetensors

struct Tensor32 {
    shape: Vec<usize>,
    data: Vec<f32>,
}

/// Minimal safetensors reader: F32 tensors only.
fn parse_safetensors(buf: &[u8]) -> Result<HashMap<String, Tensor32>> {
    if buf.len() < 8 {
        bail!("safetensors: file shorter than header length field");
    }
    let n = u64::from_le_bytes(buf[..8].try_into().expect("8 bytes")) as usize;
    let header_end = 8usize
        .checked_add(n)
        .filter(|e| *e <= buf.len())
        .ok_or_else(|| anyhow!("safetensors: header length {n} exceeds file"))?;
    let header: serde_json::Value =
        serde_json::from_slice(&buf[8..header_end]).context("safetensors: header is not JSON")?;
    let obj = header
        .as_object()
        .ok_or_else(|| anyhow!("safetensors: header is not an object"))?;
    let body = &buf[header_end..];
    let mut out = HashMap::new();
    for (name, meta) in obj {
        if name == "__metadata__" {
            continue;
        }
        if meta["dtype"].as_str() != Some("F32") {
            bail!("safetensors: tensor {name} is not F32");
        }
        let shape: Vec<usize> = meta["shape"]
            .as_array()
            .ok_or_else(|| anyhow!("safetensors: {name} has no shape"))?
            .iter()
            .map(|v| v.as_u64().map(|u| u as usize))
            .collect::<Option<_>>()
            .ok_or_else(|| anyhow!("safetensors: {name} bad shape"))?;
        let offs = meta["data_offsets"]
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(|| anyhow!("safetensors: {name} has no data_offsets"))?;
        let (s, e) = match (offs[0].as_u64(), offs[1].as_u64()) {
            (Some(s), Some(e)) => (s as usize, e as usize),
            _ => bail!("safetensors: {name} bad data_offsets"),
        };
        if s > e || e > body.len() || (e - s) % 4 != 0 {
            bail!("safetensors: {name} offsets out of range");
        }
        let data: Vec<f32> = body[s..e]
            .chunks(4)
            .map(|c| f32::from_le_bytes(c.try_into().expect("4 bytes")))
            .collect();
        if data.len() != shape.iter().product::<usize>() {
            bail!("safetensors: {name} shape does not match byte length");
        }
        out.insert(name.clone(), Tensor32 { shape, data });
    }
    Ok(out)
}

fn take(
    map: &mut HashMap<String, Tensor32>,
    name: &str,
    shape: &[usize],
    file: &str,
) -> Result<Vec<f32>> {
    let t = map
        .remove(name)
        .ok_or_else(|| anyhow!("{file}: missing tensor {name}"))?;
    if t.shape != shape {
        bail!("{file}: {name} has shape {:?}, expected {shape:?}", t.shape);
    }
    Ok(t.data)
}

fn read_st(bytes: &[u8], file: &str) -> Result<HashMap<String, Tensor32>> {
    parse_safetensors(bytes).with_context(|| format!("parse {file}"))
}

struct Head {
    dense1_w: Vec<f32>,
    ln_w: Vec<f32>,
    ln_b: Vec<f32>,
    dense2_w: Vec<f32>,
    dense2_b: f32,
}

impl Head {
    fn load(files: &ModelBytes) -> Result<Self> {
        let mut d1 = read_st(&files.dense1, "2_Dense/model.safetensors")?;
        let mut ln = read_st(&files.layer_norm, "3_LayerNorm/model.safetensors")?;
        let mut d2 = read_st(&files.dense2, "4_Dense/model.safetensors")?;
        Ok(Self {
            dense1_w: take(&mut d1, "linear.weight", &[HIDDEN, HIDDEN], "2_Dense")?,
            ln_w: take(&mut ln, "norm.weight", &[HIDDEN], "3_LayerNorm")?,
            ln_b: take(&mut ln, "norm.bias", &[HIDDEN], "3_LayerNorm")?,
            dense2_w: take(&mut d2, "linear.weight", &[1, HIDDEN], "4_Dense")?,
            dense2_b: take(&mut d2, "linear.bias", &[1], "4_Dense")?[0],
        })
    }

    /// CLS embedding -> raw logit.
    fn logit(&self, cls: &[f32]) -> f32 {
        let h: Vec<f32> = matvec(&self.dense1_w, cls, HIDDEN)
            .into_iter()
            .map(gelu)
            .collect();
        let n = layer_norm(&h, &self.ln_w, &self.ln_b, LN_EPS);
        matvec(&self.dense2_w, &n, 1)[0] + self.dense2_b
    }
}

// ---------------------------------------------------------------- scorer

/// ort errors are not `Send + Sync`; flatten them to a message.
fn ort_err<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow!("ort: {e}")
}

/// The switch that keeps the arena at its largest run (`CSR_CE_ARENA_SHRINK=0`).
const SHRINK_OFF: &str = "CSR_CE_ARENA_SHRINK";

/// Whether the session runs without ORT's CPU arena: `CSR_CE_ARENA=0`, where malloc's
/// large-block cache is off (the daemon).
fn arena_off() -> bool {
    std::env::var("CSR_CE_ARENA").is_ok_and(|v| v == "0")
        && crate::daemon_start::large_block_cache_off()
}

/// Whether each run gives the CPU arena's grown chunks back when it ends. With no arena there
/// is nothing to shrink, and ORT refuses the option ("did not find an arena based allocator").
fn shrinks(value: Option<&str>, arena_off: bool) -> bool {
    value != Some("0") && !arena_off
}

/// The options of one rerank run: shrink the CPU arena back after it, so the arena does not
/// keep the largest run it has seen for the life of the daemon. The arena itself stays on
/// (turning it off doubled rerank time). In the daemon, where malloc's large-block cache is
/// off, the chunks shrinkage frees leave the footprint at once.
fn run_options() -> Result<ort::session::RunOptions> {
    let mut options = ort::session::RunOptions::new().map_err(ort_err)?;
    if shrinks(std::env::var(SHRINK_OFF).ok().as_deref(), arena_off()) {
        options
            .set("memory.enable_memory_arena_shrinkage", "cpu:0")
            .map_err(ort_err)?;
    }
    Ok(options)
}

/// Build the ORT session. The compiled-in graph goes in from memory; an
/// override directory's graph goes in by path, because ORT resolves external
/// initializer files (a sibling `weights.data`) relative to that path and a
/// memory load gives it none.
fn build_session(onnx: &OnnxSource) -> Result<Session> {
    let builder = Session::builder().map_err(ort_err)?;
    let mut builder = builder
        .with_auto_device(AutoDevicePolicy::PreferCPU)
        .map_err(ort_err)?;
    // Memory pattern off. Every distinct (batch, sequence) shape plants its
    // own planned buffer in ORT's CPU arena and keeps it for the life of
    // the process; recall batches change shape on every query. Measured on
    // 12 varied 24-pair batches: 1294 MB footprint with it, 665 MB without,
    // bit-identical scores. Shrinking the arena after each run
    // (`memory.enable_memory_arena_shrinkage`), turning the arena off and
    // `malloc_zone_pressure_relief` freed nothing on macOS while malloc's
    // large-block cache was on. With the daemon on `MallocLargeCache=0`,
    // the shrinkage every run now asks for (`run_options`) does free: 150
    // against 238 MB after 400 searches on a small scratch corpus.
    builder = builder.with_memory_pattern(false).map_err(ort_err)?;
    // `CSR_CE_ARENA=0` turns the arena off where malloc's large-block cache
    // is off (the daemon). The arena keeps about 130 MB of activations after
    // the first health probe and grows with longer candidates, for the life
    // of the daemon; without it each run's buffers are freed, and with that
    // cache off a freed large block leaves the footprint at once. It stays
    // on by default: without it a 24-pair rerank took 2.2 times as long
    // (mean 663 ms against 308 ms over 126 calls, Studio, 2026-11-16).
    // Elsewhere freed blocks would stay in the cache, so the switch does
    // nothing there.
    if arena_off() {
        builder = builder
            .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
            .map_err(ort_err)?;
    }
    let session = match onnx {
        OnnxSource::Embedded => builder.commit_from_memory(embedded::ONNX),
        OnnxSource::File(path) => builder.commit_from_file(path),
    }
    .map_err(ort_err)?;
    Ok(session)
}

pub struct CrossEncoder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    head: Head,
    pad_id: i64,
}

impl CrossEncoder {
    /// Load from a directory laid out like `assets/models/ettin-tuned-run2`.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::from_bytes(&ModelBytes::from_dir(dir)?)
    }

    /// Load the model compiled into the binary.
    pub fn load_embedded() -> Result<Self> {
        Self::from_bytes(&ModelBytes::embedded())
    }

    fn from_bytes(files: &ModelBytes) -> Result<Self> {
        let mut tokenizer = Tokenizer::from_bytes(&files.tokenizer[..])
            .map_err(|e| anyhow!("tokenizer load: {e}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                direction: TruncationDirection::Right,
                max_length: CE_MAX_TOKENS,
                strategy: TruncationStrategy::OnlySecond,
                stride: 0,
            }))
            .map_err(|e| anyhow!("tokenizer truncation: {e}"))?;
        // Padding is done manually to the longest sequence in the batch.
        tokenizer.with_padding(None);
        let pad_id = tokenizer
            .token_to_id("[PAD]")
            .or_else(|| tokenizer.token_to_id("<pad>"))
            .unwrap_or(0) as i64;
        let head = Head::load(files)?;
        let session = build_session(&files.onnx)?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            head,
            pad_id,
        })
    }

    /// One raw logit per unit, same order. The units go through the session in
    /// runs of at most [`CE_RUN_BATCH`] pairs, each padded to its own longest
    /// sequence, so ORT's CPU arena is sized for 4 pairs and not for the whole
    /// candidate list. `CALLS`, `PAIRS` and `TOTAL_MS` are counted once per call.
    pub fn score(&self, query: &str, units: &[&str]) -> Result<Vec<f32>> {
        if units.is_empty() {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        let scores = score_in_chunks(units, |chunk| self.run_chunk(query, chunk))?;
        CALLS.fetch_add(1, Ordering::Relaxed);
        PAIRS.fetch_add(units.len() as u64, Ordering::Relaxed);
        TOTAL_MS.fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        Ok(scores)
    }

    /// One `session.run` over `units`, padded to their longest sequence. The
    /// session lock is held for the run and the read of its output only, as
    /// before; tokenizing happens outside it.
    fn run_chunk(&self, query: &str, units: &[&str]) -> Result<Vec<f32>> {
        let inputs: Vec<EncodeInput> = units
            .iter()
            .map(|u| EncodeInput::Dual(query.into(), (*u).into()))
            .collect();
        let encodings = self
            .tokenizer
            .encode_batch(inputs, true)
            .map_err(|e| anyhow!("tokenize: {e}"))?;
        let b = encodings.len();
        let seq = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);
        if seq == 0 {
            bail!("tokenizer produced empty sequences");
        }
        crate::mcp::stage_timing::count("ce_pairs", b as u64);
        crate::mcp::stage_timing::max_count("ce_max_tokens", seq as u64);
        let mut ids = vec![self.pad_id; b * seq];
        let mut mask = vec![0i64; b * seq];
        for (r, e) in encodings.iter().enumerate() {
            for (c, id) in e.get_ids().iter().enumerate() {
                ids[r * seq + c] = *id as i64;
                mask[r * seq + c] = 1;
            }
        }
        let ids_t = Tensor::from_array(([b, seq], ids.into_boxed_slice())).map_err(ort_err)?;
        let mask_t = Tensor::from_array(([b, seq], mask.into_boxed_slice())).map_err(ort_err)?;
        #[cfg(test)]
        RUN_SIZES.with(|sizes| sizes.borrow_mut().push(b));
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow!("cross-encoder session mutex poisoned"))?;
        let options = run_options()?;
        let outputs = session
            .run_with_options(
                ort::inputs![
                    "input_ids" => ids_t,
                    "attention_mask" => mask_t,
                ],
                &options,
            )
            .map_err(ort_err)?;
        let (shape, data) = outputs["last_hidden_state"]
            .try_extract_tensor::<f32>()
            .map_err(ort_err)?;
        if shape.len() != 3 || shape[0] as usize != b || shape[2] as usize != HIDDEN {
            bail!("unexpected last_hidden_state shape {:?}", &**shape);
        }
        let s = shape[1] as usize;
        let scores: Vec<f32> = (0..b)
            .map(|r| {
                self.head
                    .logit(&data[r * s * HIDDEN..r * s * HIDDEN + HIDDEN])
            })
            .collect();
        drop(outputs);
        drop(session);
        Ok(scores)
    }
}

/// Run `run` over `units` in chunks of at most [`CE_RUN_BATCH`] and join the
/// outputs in input order. One output per input: a runner that returns a
/// different count for its chunk is an error, nothing is padded or dropped.
fn score_in_chunks(
    units: &[&str],
    mut run: impl FnMut(&[&str]) -> Result<Vec<f32>>,
) -> Result<Vec<f32>> {
    let mut scores = Vec::with_capacity(units.len());
    for chunk in units.chunks(CE_RUN_BATCH) {
        let part = run(chunk)?;
        if part.len() != chunk.len() {
            bail!(
                "cross-encoder run returned {} scores for {} pairs",
                part.len(),
                chunk.len()
            );
        }
        scores.extend(part);
    }
    Ok(scores)
}

#[cfg(test)]
thread_local! {
    /// Pair count of every `session.run` made by the current thread, in order.
    static RUN_SIZES: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

static CE: OnceLock<Option<CrossEncoder>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// Calls to [`global`] made by the current thread: lets a test prove a
    /// code path never reached for the in-process model.
    static GLOBAL_TOUCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times the current thread has called [`global`].
#[cfg(test)]
pub(crate) fn global_touches() -> usize {
    GLOBAL_TOUCHES.with(|touches| touches.get())
}

/// Lazily loaded process-wide scorer: the compiled-in model, or the
/// `CSR_CE_MODEL_DIR` directory when set. `None` when an override directory is
/// missing files or the model fails to load (one warning, never a panic,
/// never a download).
///
/// An MCP server that forwards to the daemon must not call this: the model
/// and the ORT arena it grows stay resident for the life of the process.
/// Only the daemon, the CLI and servers without a daemon score in process.
pub fn global() -> Option<&'static CrossEncoder> {
    #[cfg(test)]
    GLOBAL_TOUCHES.with(|touches| touches.set(touches.get() + 1));
    CE.get_or_init(|| {
        let source = ModelSource::from_env();
        match ModelBytes::load(&source).and_then(|files| CrossEncoder::from_bytes(&files)) {
            Ok(ce) => {
                LOADED.store(true, Ordering::Relaxed);
                Some(ce)
            }
            Err(e) => {
                tracing::warn!(
                    "cross-encoder {CE_MODEL_ID} unavailable ({}): {e:#}",
                    source.label()
                );
                None
            }
        }
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rerank_run_shrinks_the_arena_unless_switched_off() {
        assert!(shrinks(None, false));
        assert!(shrinks(Some("1"), false));
        assert!(!shrinks(Some("0"), false));
        // With no arena there is nothing to shrink, and ORT refuses the option.
        assert!(!shrinks(None, true));
        let mut options = run_options().unwrap();
        if std::env::var(SHRINK_OFF).ok().as_deref() != Some("0") && !arena_off() {
            assert_eq!(
                options
                    .get("memory.enable_memory_arena_shrinkage")
                    .as_deref(),
                Some("cpu:0")
            );
        }
    }

    #[test]
    fn gelu_matches_reference_values() {
        assert!(gelu(0.0).abs() < 1e-5);
        assert!((gelu(1.0) - 0.841_344_8).abs() < 1e-5);
        assert!((gelu(-1.0) - (-0.158_655_3)).abs() < 1e-5);
        assert!((gelu(3.0) - 2.995_95).abs() < 1e-5);
        assert!(gelu(-8.0).abs() < 1e-5);
    }

    #[test]
    fn layer_norm_known_vector() {
        let x = [1.0f32, 2.0, 3.0, 4.0];
        let out = layer_norm(&x, &[1.0; 4], &[0.0; 4], 1e-5);
        // mean 2.5, var 1.25 -> (x-2.5)/sqrt(1.25+1e-5)
        let inv = 1.0 / (1.25f32 + 1e-5).sqrt();
        for (o, v) in out.iter().zip(x.iter()) {
            assert!((o - (v - 2.5) * inv).abs() < 1e-6);
        }
        let sum: f32 = out.iter().sum();
        assert!(sum.abs() < 1e-5);
        let affine = layer_norm(&x, &[2.0; 4], &[0.5; 4], 1e-5);
        assert!((affine[0] - (out[0] * 2.0 + 0.5)).abs() < 1e-6);
    }

    fn build_st(name: &str, shape: &[usize], vals: &[f32]) -> Vec<u8> {
        let header = format!(
            r#"{{"__metadata__":{{"format":"pt"}},"{name}":{{"dtype":"F32","shape":{shape:?},"data_offsets":[0,{}]}}}}"#,
            vals.len() * 4
        );
        let mut buf = (header.len() as u64).to_le_bytes().to_vec();
        buf.extend_from_slice(header.as_bytes());
        for v in vals {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        buf
    }

    #[test]
    fn safetensors_header_parse_hand_built() {
        let buf = build_st("linear.weight", &[2, 3], &[1.0, 2.0, 3.0, 4.0, 5.0, -6.5]);
        let map = parse_safetensors(&buf).unwrap();
        let t = &map["linear.weight"];
        assert_eq!(t.shape, vec![2, 3]);
        assert_eq!(t.data, vec![1.0, 2.0, 3.0, 4.0, 5.0, -6.5]);
        assert!(!map.contains_key("__metadata__"));
    }

    #[test]
    fn safetensors_rejects_bad_input() {
        assert!(parse_safetensors(&[1, 2, 3]).is_err());
        let mut buf = build_st("w", &[2], &[1.0, 2.0]);
        buf.truncate(buf.len() - 4);
        assert!(parse_safetensors(&buf).is_err());
        let mut huge = u64::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(parse_safetensors(&huge).is_err());
    }

    #[test]
    fn retrieval_policy_parse() {
        assert_eq!(RetrievalPolicy::parse(Some("1")), RetrievalPolicy::Legacy);
        assert_eq!(
            RetrievalPolicy::parse(Some("0")),
            RetrievalPolicy::CrossEncoder
        );
        assert_eq!(
            RetrievalPolicy::parse(Some("")),
            RetrievalPolicy::CrossEncoder
        );
        assert_eq!(RetrievalPolicy::parse(None), RetrievalPolicy::CrossEncoder);
    }

    #[test]
    fn load_missing_dir_errors_without_panic() {
        assert!(CrossEncoder::load(Path::new("/nonexistent/csr-ce-test")).is_err());
    }

    // ---- hand-encoded ONNX protobuf: y = x + w, with w in an external file ----

    fn pb_varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }

    fn pb_bytes(field: u64, body: &[u8]) -> Vec<u8> {
        let mut out = pb_varint(field << 3 | 2);
        out.extend(pb_varint(body.len() as u64));
        out.extend_from_slice(body);
        out
    }

    fn pb_int(field: u64, v: u64) -> Vec<u8> {
        let mut out = pb_varint(field << 3);
        out.extend(pb_varint(v));
        out
    }

    fn pb_value_info(name: &str) -> Vec<u8> {
        let dim = pb_bytes(1, &pb_int(1, 3));
        let tensor = [pb_int(1, 1), pb_bytes(2, &dim)].concat();
        let ty = pb_bytes(1, &tensor);
        [pb_bytes(1, name.as_bytes()), pb_bytes(2, &ty)].concat()
    }

    /// `y = Add(x, w)` over f32[3]; `w` lives in `weights.data` beside the model.
    fn external_data_add_model() -> Vec<u8> {
        let entry = |k: &str, v: &str| {
            pb_bytes(
                13,
                &[pb_bytes(1, k.as_bytes()), pb_bytes(2, v.as_bytes())].concat(),
            )
        };
        let weights = [
            pb_int(1, 3),
            pb_int(2, 1),
            pb_bytes(8, b"w"),
            entry("location", "weights.data"),
            entry("offset", "0"),
            entry("length", "12"),
            pb_int(14, 1),
        ]
        .concat();
        let node = [
            pb_bytes(1, b"x"),
            pb_bytes(1, b"w"),
            pb_bytes(2, b"y"),
            pb_bytes(3, b"add"),
            pb_bytes(4, b"Add"),
        ]
        .concat();
        let graph = [
            pb_bytes(1, &node),
            pb_bytes(2, b"g"),
            pb_bytes(5, &weights),
            pb_bytes(11, &pb_value_info("x")),
            pb_bytes(12, &pb_value_info("y")),
        ]
        .concat();
        [
            pb_int(1, 8),
            pb_bytes(7, &graph),
            pb_bytes(8, &pb_int(2, 13)),
        ]
        .concat()
    }

    #[test]
    fn override_dir_onnx_loads_by_path_so_external_initializers_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let onnx = dir.path().join("model_qint8_arm64.onnx");
        std::fs::write(&onnx, external_data_add_model()).unwrap();
        let weights: Vec<u8> = [1.0f32, 2.0, 3.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        std::fs::write(dir.path().join("weights.data"), weights).unwrap();

        let mut session = build_session(&OnnxSource::File(onnx)).expect("override graph loads");
        let x =
            Tensor::from_array(([3usize], vec![10.0f32, 20.0, 30.0].into_boxed_slice())).unwrap();
        let outputs = session.run(ort::inputs!["x" => x]).unwrap();
        let (_, y) = outputs["y"].try_extract_tensor::<f32>().unwrap();
        assert_eq!(y, &[11.0, 22.0, 33.0]);
    }

    #[test]
    fn model_source_reads_the_override_dir_only_when_non_empty() {
        // Parse-level check without touching the process environment.
        assert_eq!(ModelSource::Embedded.label(), "embedded");
        assert_eq!(ModelSource::Dir(PathBuf::from("/x/y")).label(), "dir:/x/y");
    }

    #[test]
    fn embedded_files_are_the_tuned_run2_model() {
        // The bands table is keyed on this hash: the compiled-in bytes must be
        // the model the bands were fit on.
        assert_eq!(
            crate::format::sha256_hex(embedded_onnx()),
            "73b9d13ac00020e657dc43319a9717fbf0fc10d7243ad5710d48837c6137abab"
        );
        let files = ModelBytes::embedded();
        assert!(files.tokenizer.len() > 1_000_000);
        Head::load(&files).expect("embedded head parses");
    }

    #[test]
    fn embedded_model_loads_from_memory_and_ranks_the_answer_first() {
        let ce = CrossEncoder::load_embedded().expect("load embedded model");
        let query = "Which planet is known as the Red Planet?";
        let passages = [
            "Venus is often called Earth's twin because of its similar size and proximity.",
            "Mars, known for its reddish appearance, is often referred to as the Red Planet.",
            "Jupiter, the largest planet in our solar system, has a prominent red spot.",
            "Saturn, famous for its rings, is sometimes mistaken for the Red Planet.",
        ];
        let scores = ce.score(query, &passages).expect("score");
        assert_eq!(scores.len(), 4);
        assert!(scores.iter().all(|s| s.is_finite()), "{scores:?}");
        let best = (0..4)
            .max_by(|a, b| scores[*a].partial_cmp(&scores[*b]).unwrap())
            .unwrap();
        assert_eq!(best, 1, "Mars must rank first: {scores:?}");
        // Same bytes, same batch: bit-identical scores.
        assert_eq!(scores, ce.score(query, &passages).unwrap());
        assert!(ce.score(query, &[]).unwrap().is_empty());
    }

    fn expected_chunk_sizes(n: usize) -> Vec<usize> {
        // The literal 4 pins the shipped value: a change to the constant must
        // be a deliberate edit of this test.
        let mut sizes = vec![4; n / 4];
        if !n.is_multiple_of(4) {
            sizes.push(n % 4);
        }
        sizes
    }

    #[test]
    fn chunking_keeps_order_and_length() {
        assert_eq!(CE_RUN_BATCH, 4);
        for n in [0usize, 1, 3, 4, 5, 24, 25] {
            let owned: Vec<String> = (0..n).map(|i| i.to_string()).collect();
            let units: Vec<&str> = owned.iter().map(String::as_str).collect();
            let mut sizes = Vec::new();
            let out = score_in_chunks(&units, |chunk| {
                sizes.push(chunk.len());
                // The "score" of a unit is its own index, so order is visible.
                Ok(chunk.iter().map(|u| u.parse::<f32>().unwrap()).collect())
            })
            .unwrap();
            let want: Vec<f32> = (0..n).map(|i| i as f32).collect();
            assert_eq!(out, want, "n={n}: order and length");
            assert_eq!(sizes, expected_chunk_sizes(n), "n={n}: chunk sizes");
        }
    }

    #[test]
    fn chunking_rejects_a_runner_that_drops_or_adds_scores() {
        let units = ["a", "b", "c", "d", "e"];
        assert!(score_in_chunks(&units, |chunk| Ok(vec![0.0; chunk.len() - 1])).is_err());
        assert!(score_in_chunks(&units, |chunk| Ok(vec![0.0; chunk.len() + 1])).is_err());
    }

    /// A fixed list of 24 pairs: one direct answer and 23 distractors of
    /// varied length (a few long enough to reach the 512-token cut).
    fn fixed_pool_24() -> (String, Vec<String>) {
        let query = "Which planet is known as the Red Planet?".to_string();
        let filler = "The committee reviewed the quarterly budget, the hiring plan and the \
                      vendor contracts before adjourning for lunch. ";
        let mut pool = Vec::new();
        for i in 0..24usize {
            let text = match i {
                9 => "Mars, known for its reddish appearance, is often referred to as the \
                      Red Planet."
                    .to_string(),
                3 => "Jupiter, the largest planet in our solar system, has a prominent red \
                      spot."
                    .to_string(),
                17 => "Saturn, famous for its rings, is sometimes mistaken for the Red Planet."
                    .to_string(),
                _ => format!("Note {i}. {}", filler.repeat(1 + (i * 7) % 29)),
            };
            pool.push(text);
        }
        (query, pool)
    }

    #[test]
    fn embedded_model_runs_in_chunks_of_four_and_keeps_order_and_length() {
        let ce = CrossEncoder::load_embedded().expect("load embedded model");
        let (query, pool) = fixed_pool_24();
        for n in [0usize, 1, 3, 4, 5, 24, 25] {
            let owned: Vec<String> = (0..n).map(|i| pool[i % pool.len()].clone()).collect();
            let units: Vec<&str> = owned.iter().map(String::as_str).collect();
            RUN_SIZES.with(|s| s.borrow_mut().clear());
            let out = ce.score(&query, &units).expect("score");
            let sizes = RUN_SIZES.with(|s| s.borrow().clone());
            assert_eq!(out.len(), n, "n={n}: one score per pair");
            assert_eq!(sizes, expected_chunk_sizes(n), "n={n}: session.run sizes");
            assert!(out.iter().all(|s| s.is_finite()), "n={n}: {out:?}");
        }
    }

    #[test]
    fn embedded_model_one_run_of_24_vs_six_runs_of_4() {
        let ce = CrossEncoder::load_embedded().expect("load embedded model");
        let (query, pool) = fixed_pool_24();
        let units: Vec<&str> = pool.iter().map(String::as_str).collect();
        // One run of all 24, padded to the longest of the 24.
        let whole = ce.run_chunk(&query, &units).expect("one run");
        let chunked = ce.score(&query, &units).expect("chunked");
        assert_eq!(whole.len(), 24);
        assert_eq!(chunked.len(), 24);
        let max_diff = whole
            .iter()
            .zip(&chunked)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        let rank = |s: &[f32]| {
            let mut order: Vec<usize> = (0..s.len()).collect();
            order.sort_by(|a, b| s[*b].partial_cmp(&s[*a]).unwrap());
            order
        };
        let (ow, oc) = (rank(&whole), rank(&chunked));
        let top5_w: std::collections::BTreeSet<usize> = ow[..5].iter().copied().collect();
        let top5_c: std::collections::BTreeSet<usize> = oc[..5].iter().copied().collect();
        println!("c11d1 max_abs_logit_diff={max_diff}");
        println!(
            "c11d1 rank1_minus_rank2 one_run={} chunks_of_4={}",
            whole[ow[0]] - whole[ow[1]],
            chunked[oc[0]] - chunked[oc[1]]
        );
        println!("c11d1 rank1 one_run={} chunks_of_4={}", ow[0], oc[0]);
        println!("c11d1 top5 one_run={ow:?}", ow = &ow[..5]);
        println!("c11d1 top5 chunks_of_4={oc:?}", oc = &oc[..5]);
        println!("c11d1 top5_set_equal={}", top5_w == top5_c);
        println!("c11d1 whole={whole:?}");
        println!("c11d1 chunked={chunked:?}");
        assert_eq!(
            ow[0], oc[0],
            "rank 1 must not move: {whole:?} vs {chunked:?}"
        );
    }

    #[test]
    #[ignore = "needs the shipped (untuned) Ettin reranker files in CSR_CE_MODEL_DIR"]
    fn ettin_red_planet_parity() {
        let dir = match ModelSource::from_env() {
            ModelSource::Dir(dir) => dir,
            ModelSource::Embedded => panic!("set CSR_CE_MODEL_DIR to the shipped model"),
        };
        let ce = CrossEncoder::load(&dir).expect("load model");
        let query = "Which planet is known as the Red Planet?";
        let passages = [
            "Venus is often called Earth's twin because of its similar size and proximity.",
            "Mars, known for its reddish appearance, is often referred to as the Red Planet.",
            "Jupiter, the largest planet in our solar system, has a prominent red spot.",
            "Saturn, famous for its rings, is sometimes mistaken for the Red Planet.",
        ];
        let scores = ce.score(query, &passages).expect("score");
        let reference = [6.21875f32, 10.8125, 8.5625, 9.875];
        println!("scores={scores:?} reference={reference:?}");
        let mut order: Vec<usize> = (0..4).collect();
        order.sort_by(|a, b| scores[*b].partial_cmp(&scores[*a]).unwrap());
        assert_eq!(order, vec![1, 3, 2, 0], "ranking order");
        for (s, r) in scores.iter().zip(reference.iter()) {
            assert!((s - r).abs() < 1.0, "score {s} vs reference {r}");
        }
        assert!(ce.score(query, &[]).unwrap().is_empty());

        let long: String = "The quick brown fox jumps over the lazy dog near the red planet. "
            .repeat(14)
            .chars()
            .take(900)
            .collect();
        let units: Vec<&str> = (0..CE_POOL).map(|_| long.as_str()).collect();
        let t = Instant::now();
        let out = ce.score(query, &units).expect("batch score");
        println!("ce_24_pairs_ms={}", t.elapsed().as_millis());
        assert_eq!(out.len(), CE_POOL);
        let st = ce_stats();
        assert!(st.calls >= 2 && st.pairs >= 28);
    }

    /// This process's phys_footprint in KiB (what `footprint` and the
    /// sampler read).
    #[cfg(target_os = "macos")]
    fn footprint_kib() -> u64 {
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V2,
                (&mut info as *mut libc::rusage_info_v2).cast(),
            )
        };
        assert_eq!(rc, 0, "proc_pid_rusage");
        info.ri_phys_footprint / 1024
    }

    /// Run by `a_daemon_cross_encoder_gives_its_run_buffers_back` in a child
    /// started with `MallocLargeCache=0`, as the daemon is. Prints how much
    /// the footprint rose over 20 recall-sized calls of long candidates.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "run in a child by a_daemon_cross_encoder_gives_its_run_buffers_back"]
    fn ce_run_buffers_probe() {
        let ce = CrossEncoder::load_embedded().expect("load embedded model");
        ce.score("warm", &["one short unit"]).expect("warm");
        let before = footprint_kib();
        let long: String = "The daemon keeps the index, the model and the page cache. "
            .repeat(60)
            .chars()
            .take(2_400)
            .collect();
        let units: Vec<&str> = (0..CE_POOL).map(|_| long.as_str()).collect();
        for i in 0..20 {
            let query = format!("how does the daemon hold memory, call {i}");
            assert_eq!(ce.score(&query, &units).expect("score").len(), CE_POOL);
        }
        println!(
            "ce-footprint-rise-kib={}",
            footprint_kib().saturating_sub(before)
        );
    }

    /// In the daemon (large-block cache off) with `CSR_CE_ARENA=0` the
    /// session runs without ORT's CPU arena, so 20 calls of 24 full-length
    /// pairs leave the footprint where it was. With the arena and no shrinkage
    /// (`CSR_CE_ARENA_SHRINK=0`) the rise is the largest run's buffers, kept
    /// for the life of the process; with the arena and the shrinkage every run
    /// now asks for (the default) the rise is smaller.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_daemon_cross_encoder_gives_its_run_buffers_back() {
        let rise_kib = |keep_arena: bool, shrink: bool| -> u64 {
            let mut probe =
                std::process::Command::new(std::env::current_exe().expect("test binary"));
            probe
                .args([
                    "--exact",
                    "search::cross_encoder::tests::ce_run_buffers_probe",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(crate::daemon_start::VARIABLE, "0")
                .env_remove("CSR_CE_MODEL_DIR")
                .env_remove("CSR_CE_ARENA")
                .env_remove(SHRINK_OFF);
            if !keep_arena {
                probe.env("CSR_CE_ARENA", "0");
            }
            if !shrink {
                probe.env(SHRINK_OFF, "0");
            }
            let out = probe.output().expect("run the probe");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            stdout
                .lines()
                .find_map(|line| line.split("ce-footprint-rise-kib=").nth(1))
                .unwrap_or_else(|| panic!("no probe line in: {stdout}"))
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .parse()
                .expect("number")
        };
        // With no arena the shrinkage option must not be sent: ORT refuses it.
        let without = rise_kib(false, true);
        let kept = rise_kib(true, false);
        let shrunk = rise_kib(true, true);
        assert!(
            without < 24 * 1024,
            "arena off: footprint rose {} MiB",
            without / 1024
        );
        // The arena as it was kept before the shrinkage: what the switch takes away.
        assert!(
            kept > 48 * 1024,
            "arena kept: footprint rose only {} MiB",
            kept / 1024
        );
        assert!(
            shrunk < kept,
            "arena shrunk after each run: rose {} MiB, kept {} MiB",
            shrunk / 1024,
            kept / 1024
        );
    }
}
