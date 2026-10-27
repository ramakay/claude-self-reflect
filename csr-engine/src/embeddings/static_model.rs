//! model2vec static embeddings (potion-code-16M-v2) for the hook fallback.
//!
//! A static model is a token-embedding table: embedding a text is a tokenizer
//! pass, a row lookup per token, a mean and an L2 normalize. There is no
//! network pass and no ONNX session, so it fits a hook process that has a few
//! hundred milliseconds in total.
//!
//! The table is never loaded whole. `model.safetensors` is parsed by hand
//! (8-byte little-endian header length, JSON header, raw rows) and each
//! token's row is read with `read_at`, so a cold process touches only the rows
//! of the words it embeds.

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use tokenizers::Tokenizer;

/// Longest token run that is averaged, as in model2vec's own default.
const MAX_TOKENS: usize = 512;
/// Characters handed to the tokenizer. A static model gains nothing from the
/// tail of a long text, and tokenizing it costs hook time.
const MAX_CHARS: usize = 4_000;
/// A safetensors header is a few hundred bytes. Anything near this is not one.
const MAX_HEADER: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dtype {
    F32,
    F16,
    Bf16,
}

impl Dtype {
    fn parse(name: &str) -> Result<Self> {
        match name {
            "F32" => Ok(Self::F32),
            "F16" => Ok(Self::F16),
            "BF16" => Ok(Self::Bf16),
            other => bail!("unsupported static model dtype {other}"),
        }
    }

    fn width(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 | Self::Bf16 => 2,
        }
    }

    fn decode(self, bytes: &[u8], out: &mut [f32]) {
        match self {
            Self::F32 => {
                for (slot, raw) in out.iter_mut().zip(bytes.as_chunks::<4>().0) {
                    *slot = f32::from_le_bytes(*raw);
                }
            }
            Self::F16 => {
                for (slot, raw) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
                    *slot = f16_to_f32(u16::from_le_bytes(*raw));
                }
            }
            Self::Bf16 => {
                for (slot, raw) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
                    *slot = f32::from_bits(u32::from(u16::from_le_bytes(*raw)) << 16);
                }
            }
        }
    }
}

/// IEEE 754 half to single precision, subnormals and non-finite values included.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let fraction = u32::from(bits & 0x3ff);
    let out = match (exponent, fraction) {
        (0, 0) => sign,
        (0, _) => {
            // Subnormal: renormalize.
            let shift = fraction.leading_zeros() - 21;
            let fraction = (fraction << shift) & 0x3ff;
            sign | ((113 - shift) << 23) | (fraction << 13)
        }
        (0x1f, 0) => sign | 0x7f80_0000,
        (0x1f, _) => sign | 0x7fc0_0000 | (fraction << 13),
        _ => sign | ((exponent + 112) << 23) | (fraction << 13),
    };
    f32::from_bits(out)
}

pub struct StaticModel {
    tokenizer: Tokenizer,
    file: File,
    /// Absolute offset of row 0 in `model.safetensors`.
    data_start: u64,
    rows: usize,
    dim: usize,
    dtype: Dtype,
}

impl StaticModel {
    /// Open a model2vec directory (`tokenizer.json` + `model.safetensors`).
    pub fn load(dir: &Path) -> Result<Self> {
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|error| anyhow!("static model tokenizer: {error}"))?;
        let path = dir.join("model.safetensors");
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
        let file_len = file.metadata()?.len();

        let mut prefix = [0u8; 8];
        file.read_exact_at(&mut prefix, 0)
            .context("safetensors header length")?;
        let header_len = u64::from_le_bytes(prefix);
        if header_len == 0 || header_len > MAX_HEADER || 8 + header_len > file_len {
            bail!("implausible safetensors header length {header_len}");
        }
        let mut header = vec![0u8; header_len as usize];
        file.read_exact_at(&mut header, 8)
            .context("safetensors header")?;
        let header: serde_json::Value =
            serde_json::from_slice(&header).context("safetensors header json")?;
        let tensors = header
            .as_object()
            .ok_or_else(|| anyhow!("safetensors header is not an object"))?;
        let (_, tensor) = tensors
            .get_key_value("embeddings")
            .or_else(|| {
                tensors.iter().find(|(name, value)| {
                    name.as_str() != "__metadata__"
                        && value["shape"].as_array().is_some_and(|s| s.len() == 2)
                })
            })
            .ok_or_else(|| anyhow!("no 2-D embedding tensor in static model"))?;
        let shape = tensor["shape"]
            .as_array()
            .filter(|shape| shape.len() == 2)
            .ok_or_else(|| anyhow!("embedding tensor is not 2-D"))?;
        let rows = shape[0].as_u64().ok_or_else(|| anyhow!("bad row count"))? as usize;
        let dim = shape[1].as_u64().ok_or_else(|| anyhow!("bad dimension"))? as usize;
        let dtype = Dtype::parse(tensor["dtype"].as_str().unwrap_or_default())?;
        let offsets = tensor["data_offsets"]
            .as_array()
            .filter(|offsets| offsets.len() == 2)
            .ok_or_else(|| anyhow!("missing data_offsets"))?;
        let begin = offsets[0].as_u64().ok_or_else(|| anyhow!("bad offset"))?;
        let end = offsets[1].as_u64().ok_or_else(|| anyhow!("bad offset"))?;
        let expected = rows as u64 * dim as u64 * dtype.width() as u64;
        let data_start = 8 + header_len + begin;
        if rows == 0
            || dim == 0
            || end < begin
            || end - begin != expected
            || data_start + expected > file_len
        {
            bail!("static model tensor does not fit its file");
        }
        Ok(Self {
            tokenizer,
            file,
            data_start,
            rows,
            dim,
            dtype,
        })
    }

    /// The configured model directory: `CSR_STATIC_MODEL_DIR`, else
    /// `$HOME/.claude-self-reflect/models/potion-code-16M-v2`.
    pub fn default_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("CSR_STATIC_MODEL_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir));
        }
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        Some(
            PathBuf::from(home)
                .join(".claude-self-reflect")
                .join("models")
                .join("potion-code-16M-v2"),
        )
    }

    /// The model in [`Self::default_dir`], or `None` when it is absent or
    /// unreadable. A missing model is a supported configuration, not an error.
    pub fn load_default() -> Option<Self> {
        let dir = Self::default_dir()?;
        if !dir.join("model.safetensors").is_file() {
            return None;
        }
        Self::load(&dir).ok()
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Unit-length mean of the token rows, or the zero vector when the text
    /// yields no known token.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut mean = vec![0.0f32; self.dim];
        let text = match text.char_indices().nth(MAX_CHARS) {
            Some((end, _)) => &text[..end],
            None => text,
        };
        let Ok(encoding) = self.tokenizer.encode(text, false) else {
            return mean;
        };
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for id in encoding.get_ids().iter().take(MAX_TOKENS) {
            if (*id as usize) < self.rows {
                *counts.entry(*id).or_default() += 1;
            }
        }
        // Sorted, so the float sum runs in one order and the result is
        // identical from call to call.
        let mut ids: Vec<(u32, u32)> = counts.into_iter().collect();
        ids.sort_unstable();
        let mut bytes = vec![0u8; self.dim * self.dtype.width()];
        let mut row = vec![0.0f32; self.dim];
        let mut total = 0u32;
        for (id, count) in ids {
            let offset = self.data_start + id as u64 * bytes.len() as u64;
            if self.file.read_exact_at(&mut bytes, offset).is_err() {
                continue;
            }
            self.dtype.decode(&bytes, &mut row);
            for (slot, value) in mean.iter_mut().zip(&row) {
                *slot += value * count as f32;
            }
            total += count;
        }
        if total == 0 {
            return mean;
        }
        let norm = mean.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 && norm.is_finite() {
            for value in &mut mean {
                *value /= norm;
            }
        } else {
            mean.fill(0.0);
        }
        mean
    }
}

/// Dot product. Both sides of a static comparison are unit length (or zero).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKENIZER: &str = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],
        "normalizer":{"type":"Lowercase"},"pre_tokenizer":{"type":"WhitespaceSplit"},
        "post_processor":null,"decoder":null,
        "model":{"type":"WordLevel","vocab":{"[UNK]":0,"alpha":1,"beta":2,"gamma":3},"unk_token":"[UNK]"}}"#;

    const ROWS: [[f32; 3]; 4] = [
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 2.0, 0.0],
        [3.0, 4.0, 0.0],
    ];

    fn write_model(dir: &Path, dtype: &str, body: &[u8]) {
        let header = format!(
            r#"{{"embeddings":{{"dtype":"{dtype}","shape":[4,3],"data_offsets":[0,{}]}}}}"#,
            body.len()
        );
        let mut header = header.into_bytes();
        while header.len() % 8 != 0 {
            header.push(b' ');
        }
        let mut file = (header.len() as u64).to_le_bytes().to_vec();
        file.extend_from_slice(&header);
        file.extend_from_slice(body);
        std::fs::write(dir.join("model.safetensors"), file).unwrap();
        std::fs::write(dir.join("tokenizer.json"), TOKENIZER).unwrap();
    }

    fn f32_body() -> Vec<u8> {
        ROWS.iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }

    #[test]
    fn static_model_fixture_loads_and_embeds_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        write_model(dir.path(), "F32", &f32_body());
        let model = StaticModel::load(dir.path()).unwrap();
        assert_eq!(model.dim(), 3);

        // One known token: its row, normalized.
        assert_eq!(model.embed("beta"), vec![0.0, 1.0, 0.0]);
        // Mean of rows 1 and 2 is (0.5, 1.0, 0.0); then unit length.
        let both = model.embed("Alpha beta");
        let norm = (0.25f32 + 1.0).sqrt();
        assert!((both[0] - 0.5 / norm).abs() < 1e-6, "{both:?}");
        assert!((both[1] - 1.0 / norm).abs() < 1e-6, "{both:?}");
        assert_eq!(both[2], 0.0);
        // Unknown words fall back to the [UNK] row; repeat calls are identical.
        assert_eq!(model.embed("zzz"), vec![0.0, 0.0, 1.0]);
        assert_eq!(
            model.embed("alpha beta gamma"),
            model.embed("alpha beta gamma")
        );
        assert_eq!(model.embed("gamma"), vec![0.6, 0.8, 0.0]);
        // Empty text has no tokens: the zero vector, and cosine against it is 0.
        assert_eq!(model.embed(""), vec![0.0; 3]);
        assert_eq!(cosine(&model.embed("alpha"), &model.embed("beta")), 0.0);
        assert!((cosine(&both, &both) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn static_model_reads_half_precision_rows() {
        let dir = tempfile::tempdir().unwrap();
        // Halves of 0, 1, 2, 3 and 4: 0x0000, 0x3c00, 0x4000, 0x4200, 0x4400.
        let half = |v: f32| -> u16 { [0, 0x3c00, 0x4000, 0x4200, 0x4400][v as usize] };
        let body: Vec<u8> = ROWS
            .iter()
            .flatten()
            .flat_map(|v| half(*v).to_le_bytes())
            .collect();
        write_model(dir.path(), "F16", &body);
        let model = StaticModel::load(dir.path()).unwrap();
        assert_eq!(model.embed("gamma"), vec![0.6, 0.8, 0.0]);
        assert_eq!(f16_to_f32(0x0001), 2.0f32.powi(-24));
        assert_eq!(f16_to_f32(0xc000), -2.0);
    }

    #[test]
    fn static_model_rejects_a_truncated_tensor() {
        let dir = tempfile::tempdir().unwrap();
        write_model(dir.path(), "F32", &f32_body());
        // Header claims the full table; cut the file short.
        let path = dir.path().join("model.safetensors");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 4);
        std::fs::write(&path, bytes).unwrap();
        assert!(StaticModel::load(dir.path()).is_err());
    }

    /// Needs the real potion-code-16M-v2 files (`CSR_STATIC_MODEL_DIR` or the
    /// default dir). Run with `--ignored --nocapture` to see the timings.
    #[test]
    #[ignore = "needs the real potion-code-16M-v2 model on disk"]
    fn real_potion_model_loads_and_ranks_related_text_above_unrelated() {
        let loaded = std::time::Instant::now();
        let model = StaticModel::load_default().expect("potion-code-16M-v2 on disk");
        let load = loaded.elapsed();
        assert_eq!(model.dim(), 256);
        let query = model.embed("why does the token refresh reject expired credentials");
        let related = model
            .embed("The refresh path rejects expired tokens before the retry middleware runs.");
        let unrelated = model.embed("Pastel palette for the weekly newsletter header banner.");
        let timed = std::time::Instant::now();
        for _ in 0..100 {
            let _ = model
                .embed("The refresh path rejects expired tokens before the retry middleware runs.");
        }
        eprintln!(
            "potion load={:?} embed={:?} related={:.3} unrelated={:.3}",
            load,
            timed.elapsed() / 100,
            cosine(&query, &related),
            cosine(&query, &unrelated)
        );
        assert!((cosine(&related, &related) - 1.0).abs() < 1e-4);
        assert!(cosine(&query, &related) > cosine(&query, &unrelated));
    }
}
