//! Reusable ort-based sentence embedder for the S4 model comparison
//! (EmbeddingGemma 2 text tower, Granite r2 97M).
//!
//! Nothing here touches the default retrieval path or `EmbeddingEngine`.
//! Activations stay fp32; fp16 and q4 graphs are never loaded. Output vectors
//! are L2-normalised at the model's native width; [`truncate_mrl`] cuts them
//! to a prefix and re-normalises.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use ort::session::builder::AutoDevicePolicy;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy};

/// Default token cap per text.
pub const DEFAULT_MAX_TOKENS: usize = 512;

/// Document or query side. Selects the sentence-transformers prompt, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Doc,
    Query,
}

/// How token states become one vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pooling {
    /// The graph's own `sentence_embedding` output (mean pool plus Dense layers).
    GraphSentenceEmbedding,
    /// Mean of `last_hidden_state` over the attention mask.
    Mean,
    /// First token of `last_hidden_state`.
    Cls,
}

impl Pooling {
    /// The string recorded in `meta.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Pooling::GraphSentenceEmbedding => "graph_sentence_embedding",
            Pooling::Mean => "mean",
            Pooling::Cls => "cls",
        }
    }
}

/// Prompt prefixes from `config_sentence_transformers.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompts {
    pub document: String,
    pub query: String,
}

fn ort_err<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow!("ort: {e}")
}

// ---------------------------------------------------------------- pure math

/// Scale to unit L2 norm in place. A zero vector is left unchanged.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        let inv = (1.0 / norm) as f32;
        for x in v.iter_mut() {
            *x *= inv;
        }
    }
}

/// Matryoshka truncation: keep the first `d` dimensions, then re-normalise.
pub fn truncate_mrl(v: &[f32], d: usize) -> Vec<f32> {
    let mut out: Vec<f32> = v[..d.min(v.len())].to_vec();
    l2_normalize(&mut out);
    out
}

/// Masked mean pooling. `hidden` is `[b, seq, h]` row-major, `mask` is
/// `[b, seq]`. Padding positions (mask 0) do not contribute. A row with an
/// all-zero mask yields a zero vector.
pub fn masked_mean_pool(
    hidden: &[f32],
    mask: &[i64],
    b: usize,
    seq: usize,
    h: usize,
) -> Vec<Vec<f32>> {
    (0..b)
        .map(|r| {
            let mut acc = vec![0f64; h];
            let mut count = 0f64;
            for t in 0..seq {
                if mask[r * seq + t] == 0 {
                    continue;
                }
                count += 1.0;
                let base = (r * seq + t) * h;
                for (a, x) in acc.iter_mut().zip(&hidden[base..base + h]) {
                    *a += *x as f64;
                }
            }
            if count == 0.0 {
                return vec![0f32; h];
            }
            acc.into_iter().map(|a| (a / count) as f32).collect()
        })
        .collect()
}

/// First-token pooling of `[b, seq, h]`.
pub fn cls_pool(hidden: &[f32], b: usize, seq: usize, h: usize) -> Vec<Vec<f32>> {
    (0..b)
        .map(|r| hidden[r * seq * h..r * seq * h + h].to_vec())
        .collect()
}

// ---------------------------------------------------------------- config files

fn read_prompts(dir: &Path) -> Result<Option<Prompts>> {
    let path = dir.join("config_sentence_transformers.json");
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let (Some(doc), Some(query)) = (
        v["prompts"]["document"].as_str(),
        v["prompts"]["query"].as_str(),
    ) else {
        return Ok(None);
    };
    if doc.is_empty() && query.is_empty() {
        return Ok(None);
    }
    Ok(Some(Prompts {
        document: doc.to_string(),
        query: query.to_string(),
    }))
}

/// `Some(true)` for CLS, `Some(false)` for another declared mode, `None` when
/// the model ships no `1_Pooling/config.json`.
fn read_pooling_is_cls(dir: &Path) -> Result<Option<bool>> {
    let path = dir.join("1_Pooling").join("config.json");
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(v["pooling_mode_cls_token"].as_bool().unwrap_or(false)))
}

// ---------------------------------------------------------------- embedder

/// A graph input the embedder knows how to feed.
#[derive(Clone, Debug)]
enum GraphInput {
    InputIds,
    AttentionMask,
    TokenTypeIds,
    /// The multimodal Gemma graph declares `image_features`, `video_features`
    /// and `audio_features` as `[n, width]` f32. A text-only call feeds `n = 0`.
    EmptyFeatures {
        name: String,
        width: usize,
    },
}

pub struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    pad_id: i64,
    inputs: Vec<GraphInput>,
    output_name: String,
    pooling: Pooling,
    prompts: Option<Prompts>,
    onnx_path: PathBuf,
    truncated: AtomicU64,
    tokens: AtomicU64,
}

impl OnnxEmbedder {
    /// Load `dir/onnx_file` (or an absolute `onnx_file`) with `threads`
    /// intra-op threads (0 = all cores). Truncation defaults to
    /// [`DEFAULT_MAX_TOKENS`]; change it with [`Self::with_max_tokens`].
    pub fn load(dir: &Path, onnx_file: &str, threads: usize) -> Result<Self> {
        let onnx_path = dir.join(onnx_file);
        let name = onnx_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("fp16") || name.contains("q4") {
            bail!(
                "refusing to load {}: fp16 and q4 graphs are not allowed",
                onnx_path.display()
            );
        }
        let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow!("tokenizer load: {e}"))?;
        // Padding is done manually to the longest sequence in the batch.
        tokenizer.with_padding(None);
        let pad_id = tokenizer
            .token_to_id("<pad>")
            .or_else(|| tokenizer.token_to_id("[PAD]"))
            .unwrap_or(0) as i64;
        let threads = if threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        } else {
            threads
        };
        let builder = Session::builder().map_err(ort_err)?;
        let mut builder = builder
            .with_auto_device(AutoDevicePolicy::PreferCPU)
            .map_err(ort_err)?
            .with_intra_threads(threads)
            .map_err(ort_err)?;
        let session = builder
            .commit_from_file(&onnx_path)
            .map_err(ort_err)
            .with_context(|| format!("load {}", onnx_path.display()))?;

        let mut inputs: Vec<GraphInput> = Vec::new();
        for i in session.inputs() {
            let name = i.name();
            let shape_width = i
                .dtype()
                .tensor_shape()
                .and_then(|s| s.get(1).copied())
                .filter(|w| *w > 0)
                .map(|w| w as usize);
            inputs.push(match (name, shape_width) {
                ("input_ids", _) => GraphInput::InputIds,
                ("attention_mask", _) => GraphInput::AttentionMask,
                ("token_type_ids", _) => GraphInput::TokenTypeIds,
                ("image_features" | "video_features" | "audio_features", Some(width)) => {
                    GraphInput::EmptyFeatures {
                        name: name.to_string(),
                        width,
                    }
                }
                _ => {
                    let declared: Vec<String> = session
                        .inputs()
                        .iter()
                        .map(|i| format!("{}:{:?}", i.name(), i.dtype()))
                        .collect();
                    bail!(
                        "unsupported graph input {name:?} in {} (declared inputs: {})",
                        onnx_path.display(),
                        declared.join(", ")
                    );
                }
            });
        }
        let output_names: Vec<String> = session
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        let has = |n: &str| output_names.iter().any(|o| o == n);
        let (pooling, output_name) = if has("sentence_embedding") {
            (
                Pooling::GraphSentenceEmbedding,
                "sentence_embedding".to_string(),
            )
        } else {
            let out = if has("last_hidden_state") {
                "last_hidden_state".to_string()
            } else {
                output_names
                    .first()
                    .cloned()
                    .ok_or_else(|| anyhow!("graph declares no outputs"))?
            };
            let pooling = if read_pooling_is_cls(dir)? == Some(true) {
                Pooling::Cls
            } else {
                Pooling::Mean
            };
            (pooling, out)
        };
        let prompts = read_prompts(dir)?;
        let mut me = Self {
            session: Mutex::new(session),
            tokenizer,
            pad_id,
            inputs,
            output_name,
            pooling,
            prompts,
            onnx_path,
            truncated: AtomicU64::new(0),
            tokens: AtomicU64::new(0),
        };
        me = me.with_max_tokens(DEFAULT_MAX_TOKENS)?;
        Ok(me)
    }

    /// Right-truncate every text to `max_tokens` tokens.
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Result<Self> {
        self.tokenizer
            .with_truncation(Some(TruncationParams {
                direction: TruncationDirection::Right,
                max_length: max_tokens,
                strategy: TruncationStrategy::LongestFirst,
                stride: 0,
            }))
            .map_err(|e| anyhow!("tokenizer truncation: {e}"))?;
        Ok(self)
    }

    pub fn pooling(&self) -> Pooling {
        self.pooling
    }

    pub fn prompts(&self) -> Option<&Prompts> {
        self.prompts.as_ref()
    }

    pub fn onnx_path(&self) -> &Path {
        &self.onnx_path
    }

    /// Texts cut by the token cap so far.
    pub fn truncated_units(&self) -> u64 {
        self.truncated.load(Ordering::Relaxed)
    }

    /// Tokens fed to the graph so far (after truncation, padding excluded).
    pub fn tokens_seen(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }

    /// One L2-normalised native-width vector per text, same order.
    pub fn embed(&self, texts: &[&str], kind: Kind) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prompted: Vec<String> = texts
            .iter()
            .map(|t| match (&self.prompts, kind) {
                (Some(p), Kind::Doc) => format!("{}{}", p.document, t),
                (Some(p), Kind::Query) => format!("{}{}", p.query, t),
                (None, _) => (*t).to_string(),
            })
            .collect();
        let encodings = self
            .tokenizer
            .encode_batch(prompted, true)
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
        let mut types = vec![0i64; b * seq];
        let mut n_trunc = 0u64;
        let mut n_tok = 0u64;
        for (r, e) in encodings.iter().enumerate() {
            if !e.get_overflowing().is_empty() {
                n_trunc += 1;
            }
            n_tok += e.get_ids().len() as u64;
            for (c, id) in e.get_ids().iter().enumerate() {
                ids[r * seq + c] = *id as i64;
                mask[r * seq + c] = 1;
            }
            for (c, t) in e.get_type_ids().iter().enumerate() {
                types[r * seq + c] = *t as i64;
            }
        }
        self.truncated.fetch_add(n_trunc, Ordering::Relaxed);
        self.tokens.fetch_add(n_tok, Ordering::Relaxed);

        let mut feed: Vec<(String, ort::session::SessionInputValue<'static>)> = Vec::new();
        for input in &self.inputs {
            let (name, value): (String, ort::session::SessionInputValue<'static>) = match input {
                GraphInput::InputIds => (
                    "input_ids".to_string(),
                    Tensor::from_array(([b, seq], ids.clone().into_boxed_slice()))
                        .map_err(ort_err)?
                        .into(),
                ),
                GraphInput::AttentionMask => (
                    "attention_mask".to_string(),
                    Tensor::from_array(([b, seq], mask.clone().into_boxed_slice()))
                        .map_err(ort_err)?
                        .into(),
                ),
                GraphInput::TokenTypeIds => (
                    "token_type_ids".to_string(),
                    Tensor::from_array(([b, seq], types.clone().into_boxed_slice()))
                        .map_err(ort_err)?
                        .into(),
                ),
                GraphInput::EmptyFeatures { name, width } => (
                    name.clone(),
                    Tensor::from_array(([0usize, *width], Vec::<f32>::new().into_boxed_slice()))
                        .map_err(ort_err)?
                        .into(),
                ),
            };
            feed.push((name, value));
        }
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow!("embedder session mutex poisoned"))?;
        let outputs = session.run(feed).map_err(ort_err)?;
        let (shape, data) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(ort_err)?;
        let mut rows = match (self.pooling, shape.len()) {
            (Pooling::GraphSentenceEmbedding, 2) if shape[0] as usize == b => {
                let h = shape[1] as usize;
                data.chunks(h).map(<[f32]>::to_vec).collect()
            }
            (Pooling::GraphSentenceEmbedding, _) => {
                bail!("unexpected sentence_embedding shape {:?}", &**shape)
            }
            (p, 3) if shape[0] as usize == b && shape[1] as usize == seq => {
                let h = shape[2] as usize;
                if p == Pooling::Cls {
                    cls_pool(data, b, seq, h)
                } else {
                    masked_mean_pool(data, &mask, b, seq, h)
                }
            }
            _ => bail!("unexpected {} shape {:?}", self.output_name, &**shape),
        };
        for r in rows.iter_mut() {
            l2_normalize(r);
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(v: &[f32]) -> f64 {
        v.iter()
            .map(|x| (*x as f64) * (*x as f64))
            .sum::<f64>()
            .sqrt()
    }

    #[test]
    fn embed_truncate_mrl_renormalises_and_keeps_prefix() {
        let mut v: Vec<f32> = (1..=8).map(|i| i as f32).collect();
        l2_normalize(&mut v);
        let t = truncate_mrl(&v, 3);
        assert_eq!(t.len(), 3);
        assert!((norm(&t) - 1.0).abs() < 1e-5, "norm {}", norm(&t));
        // The prefix keeps its direction: ratios between kept dims are unchanged.
        assert!((t[1] / t[0] - v[1] / v[0]).abs() < 1e-5);
        assert!((t[2] / t[0] - v[2] / v[0]).abs() < 1e-5);
        // Full-width truncation is the identity.
        let full = truncate_mrl(&v, 8);
        for (a, b) in full.iter().zip(&v) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn embed_masked_mean_pool_ignores_padding() {
        // b=2, seq=3, h=2. Row 0 has 2 real tokens; row 1 has 3.
        let hidden = [
            1.0, 2.0, 3.0, 4.0, 100.0, 100.0, // row 0 (last token is padding)
            1.0, 1.0, 2.0, 2.0, 3.0, 3.0, // row 1
        ];
        let mask = [1, 1, 0, 1, 1, 1];
        let out = masked_mean_pool(&hidden, &mask, 2, 3, 2);
        assert_eq!(out[0], vec![2.0, 3.0]);
        assert_eq!(out[1], vec![2.0, 2.0]);
        // An all-padding row is a zero vector, not NaN.
        let z = masked_mean_pool(&hidden[..6], &[0, 0, 0], 1, 3, 2);
        assert_eq!(z[0], vec![0.0, 0.0]);
    }

    #[test]
    fn embed_cls_pool_takes_first_token() {
        let hidden = [1.0, 2.0, 9.0, 9.0, 3.0, 4.0, 9.0, 9.0];
        let out = cls_pool(&hidden, 2, 2, 2);
        assert_eq!(out, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    }

    #[test]
    fn embed_l2_normalize_leaves_zero_vector_alone() {
        let mut z = vec![0.0f32; 4];
        l2_normalize(&mut z);
        assert_eq!(z, vec![0.0; 4]);
    }

    #[test]
    fn embed_refuses_fp16_and_q4_graphs() {
        let err = OnnxEmbedder::load(Path::new("/nonexistent"), "onnx/model_fp16.onnx", 1)
            .err()
            .expect("fp16 must be refused");
        assert!(err.to_string().contains("not allowed"), "{err}");
        let err = OnnxEmbedder::load(Path::new("/nonexistent"), "onnx/model_q4.onnx", 1)
            .err()
            .expect("q4 must be refused");
        assert!(err.to_string().contains("not allowed"), "{err}");
    }
}
