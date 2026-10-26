//! Cross-encoder scorer for `cross-encoder/ettin-reranker-32m-v1`
//! (ModernBERT, INT8 arm64 ONNX) plus the single retrieval-policy switch.
//!
//! The ONNX graph stops at `last_hidden_state`. The sentence-transformers head
//! (CLS pooling, Dense+GELU, LayerNorm, Dense -> logit) is applied here in Rust
//! from the small safetensors files shipped beside the model. Nothing in this
//! module downloads anything; `global()` returns `None` when files are absent.

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

/// Directory holding the reranker files.
pub fn model_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CSR_CE_MODEL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let cache = crate::embeddings::cache::cache_dir();
    cache
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(cache)
        .join("rerankers")
        .join("ettin-reranker-32m-v1")
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

fn read_st(path: &Path) -> Result<HashMap<String, Tensor32>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    parse_safetensors(&bytes).with_context(|| format!("parse {}", path.display()))
}

struct Head {
    dense1_w: Vec<f32>,
    ln_w: Vec<f32>,
    ln_b: Vec<f32>,
    dense2_w: Vec<f32>,
    dense2_b: f32,
}

impl Head {
    fn load(dir: &Path) -> Result<Self> {
        let mut d1 = read_st(&dir.join("2_Dense/model.safetensors"))?;
        let mut ln = read_st(&dir.join("3_LayerNorm/model.safetensors"))?;
        let mut d2 = read_st(&dir.join("4_Dense/model.safetensors"))?;
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

pub struct CrossEncoder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    head: Head,
    pad_id: i64,
}

impl CrossEncoder {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
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
        let head = Head::load(dir)?;
        let builder = Session::builder().map_err(ort_err)?;
        let mut builder = builder
            .with_auto_device(AutoDevicePolicy::PreferCPU)
            .map_err(ort_err)?;
        let session = builder
            .commit_from_file(dir.join("model_qint8_arm64.onnx"))
            .map_err(ort_err)?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            head,
            pad_id,
        })
    }

    /// One raw logit per unit, same order. One `session.run` for the batch.
    pub fn score(&self, query: &str, units: &[&str]) -> Result<Vec<f32>> {
        if units.is_empty() {
            return Ok(Vec::new());
        }
        let started = Instant::now();
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
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow!("cross-encoder session mutex poisoned"))?;
        let outputs = session
            .run(ort::inputs![
                "input_ids" => ids_t,
                "attention_mask" => mask_t,
            ])
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
        CALLS.fetch_add(1, Ordering::Relaxed);
        PAIRS.fetch_add(b as u64, Ordering::Relaxed);
        TOTAL_MS.fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        Ok(scores)
    }
}

/// Lazily loaded process-wide scorer. `None` when the model files are missing
/// or fail to load (one warning, never a panic, never a download).
pub fn global() -> Option<&'static CrossEncoder> {
    static CE: OnceLock<Option<CrossEncoder>> = OnceLock::new();
    CE.get_or_init(|| {
        let dir = model_dir();
        match CrossEncoder::load(&dir) {
            Ok(ce) => {
                LOADED.store(true, Ordering::Relaxed);
                Some(ce)
            }
            Err(e) => {
                tracing::warn!(
                    "cross-encoder {CE_MODEL_ID} unavailable at {}: {e:#}",
                    dir.display()
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

    #[test]
    #[ignore = "needs the Ettin reranker files in model_dir()"]
    fn ettin_red_planet_parity() {
        let dir = model_dir();
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
}
