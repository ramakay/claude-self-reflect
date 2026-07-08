//! Prompt intent classification for the UserPromptSubmit hook.
//!
//! Nearest-prototype classifier over the in-process MiniLM embedding space:
//! each intent is a set of exemplar sentences; a prompt is classified as the
//! intent whose closest exemplar clears that intent's abstain threshold.
//! No model ships — the exemplars ride the same 384-dim space the hook
//! already embeds every prompt into, so classification is K×M dot products.
//!
//! Exemplar vectors are cached on disk (keyed by a hash of the exemplar set)
//! because each hook invocation is a short-lived process: without the cache
//! every prompt would pay one ONNX forward pass per exemplar.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::embeddings::EmbeddingEngine;

/// Continuation-class intents the hook can act on. Anything else abstains
/// and falls through to episode correlation / content search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    /// "continue", "keep at it" — resume the latest work, recency picks it.
    Continue,
    /// "what were we working on" — asking for the state of recent work.
    StateRecall,
}

/// Exemplar sentences per intent. Editing this table is the whole tuning
/// surface — the disk cache invalidates itself via `exemplar_hash`.
const INTENT_EXEMPLARS: &[(Intent, &[&str])] = &[
    (
        Intent::Continue,
        &[
            "continue",
            "resume",
            "keep going",
            "carry on",
            "go on",
            "proceed",
            "keep at it",
            "back to it",
            "finish it",
            "finish what you started",
            "keep working on it",
            "pick up where we left off",
            "let's continue the work",
            "resume the task from before",
            "do the next step",
            "get back to what we were doing",
        ],
    ),
    (
        Intent::StateRecall,
        &[
            "what were we working on last, what should we do next",
            "where did we leave off in the previous session, what is the current status of our work",
            "pick up where we left off and continue the work from last time",
            "what were we working on",
            "where did we leave off",
            "what is the status of our work",
            "what did we do last session",
            "what did we just discuss",
            "what should we do next",
            "what's left to do",
            "catch me up on where we are",
            "remind me what we were doing",
            "summarize the current state of our work",
        ],
    ),
];

/// Per-intent abstain thresholds (max cosine against that intent's
/// exemplars). Calibrated live 2026-07-08 against the real model:
/// - Continue positives 0.851–1.000 ("lets get back to what we were doing"
///   0.851); worst negative 0.398 ("sure"); acknowledgments ("yes",
///   "thanks", "sounds good") ≤0.355; content commands ≤0.264 including
///   "the tests continue to fail on CI" at 0.264. 0.60 sits mid-gap.
/// - StateRecall positives ≥0.608 ("whats the latest status"); mixed
///   recall+content 0.468 ("what did we just discuss in csr to fix" —
///   correctly abstains: it names the work, Route B correlation serves it);
///   negatives ≤0.275. 0.55 splits the 0.468↔0.608 gap.
fn threshold(intent: Intent) -> f32 {
    match intent {
        Intent::Continue => 0.60,
        Intent::StateRecall => 0.55,
    }
}

/// Hash of the exemplar table (intent tags + sentences, order-sensitive).
/// Changing any exemplar invalidates the on-disk vector cache.
fn exemplar_hash() -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (intent, sentences) in INTENT_EXEMPLARS {
        format!("{intent:?}").hash(&mut h);
        for s in *sentences {
            s.hash(&mut h);
        }
    }
    h.finish()
}

/// Embedded exemplar vectors for every intent, ready to classify against.
pub struct ProbeSet {
    probes: Vec<(Intent, Vec<Vec<f32>>)>,
}

#[derive(Serialize, Deserialize)]
struct ProbeCache {
    hash: u64,
    probes: Vec<(Intent, Vec<Vec<f32>>)>,
}

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude-self-reflect").join("intent_probes.json"))
}

impl ProbeSet {
    /// Load exemplar vectors from the disk cache, or embed and cache them.
    /// Returns None only if embedding fails (hook then skips classification
    /// rather than blocking — hooks never block the session).
    pub async fn load_or_build(embeddings: &Arc<EmbeddingEngine>) -> Option<Self> {
        let hash = exemplar_hash();
        if let Some(path) = cache_path() {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(cache) = serde_json::from_slice::<ProbeCache>(&bytes) {
                    if cache.hash == hash {
                        return Some(Self {
                            probes: cache.probes,
                        });
                    }
                }
            }
        }

        let emb = embeddings.clone();
        let probes = tokio::task::spawn_blocking(move || {
            INTENT_EXEMPLARS
                .iter()
                .map(|(intent, sentences)| {
                    let vecs = sentences
                        .iter()
                        .filter_map(|s| emb.embed_single(s).ok())
                        .collect::<Vec<_>>();
                    (*intent, vecs)
                })
                .collect::<Vec<_>>()
        })
        .await
        .ok()?;

        if probes.iter().all(|(_, vecs)| vecs.is_empty()) {
            return None;
        }

        if let Some(path) = cache_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec(&ProbeCache {
                hash,
                probes: probes.clone(),
            }) {
                let _ = std::fs::write(&path, bytes);
            }
        }

        Some(Self { probes })
    }

    /// Per-intent max cosine against that intent's exemplars. Exposed
    /// separately from `classify` so abstained scores stay visible for
    /// threshold calibration (CSR_DEBUG_CORRELATE).
    pub fn scores(&self, query_vec: &[f32]) -> Vec<(Intent, f32)> {
        self.probes
            .iter()
            .map(|(intent, vecs)| {
                let score = vecs
                    .iter()
                    .map(|p| cosine_sim(query_vec, p))
                    .fold(0.0_f32, f32::max);
                (*intent, score)
            })
            .collect()
    }

    /// Classify a prompt vector: per-intent max cosine, argmax across
    /// intents, fire only above that intent's threshold. Both current
    /// intents route to the same recency pickup, so no inter-intent margin
    /// is enforced — adjacent scores are fine as long as the winner clears
    /// its gate. Returns the winning intent and its score, or None (abstain).
    pub fn classify(&self, query_vec: &[f32]) -> Option<(Intent, f32)> {
        self.scores(query_vec)
            .into_iter()
            .fold(None, |best: Option<(Intent, f32)>, (intent, score)| {
                if best.is_none_or(|(_, b)| score > b) {
                    Some((intent, score))
                } else {
                    best
                }
            })
            .filter(|(intent, score)| *score >= threshold(*intent))
    }

    #[cfg(test)]
    fn from_probes(probes: Vec<(Intent, Vec<Vec<f32>>)>) -> Self {
        Self { probes }
    }
}

/// Plain cosine similarity; 0.0 when either vector has zero norm.
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(x: f32, y: f32, z: f32) -> Vec<f32> {
        let n = (x * x + y * y + z * z).sqrt();
        vec![x / n, y / n, z / n]
    }

    fn synthetic_probes() -> ProbeSet {
        // Continue points along +x, StateRecall along +y.
        ProbeSet::from_probes(vec![
            (Intent::Continue, vec![unit(1.0, 0.0, 0.0)]),
            (
                Intent::StateRecall,
                vec![unit(0.0, 1.0, 0.0), unit(0.1, 1.0, 0.0)],
            ),
        ])
    }

    #[test]
    fn classify_picks_closest_intent_above_threshold() {
        let probes = synthetic_probes();
        let (intent, score) = probes.classify(&unit(1.0, 0.1, 0.0)).unwrap();
        assert_eq!(intent, Intent::Continue);
        assert!(score > 0.9);

        let (intent, _) = probes.classify(&unit(0.05, 1.0, 0.0)).unwrap();
        assert_eq!(intent, Intent::StateRecall);
    }

    #[test]
    fn classify_abstains_below_threshold() {
        let probes = synthetic_probes();
        // Orthogonal to both intents → max cosine ~0 → abstain.
        assert!(probes.classify(&unit(0.0, 0.0, 1.0)).is_none());
        // Between the two but under both gates (cos 45° ≈ 0.707 > gates —
        // use a vector far enough from both instead).
        assert!(probes.classify(&unit(0.3, 0.3, 1.0)).is_none());
    }

    #[test]
    fn classify_uses_max_over_exemplars_not_mean() {
        // One distant + one close exemplar for StateRecall: max must win.
        let probes = ProbeSet::from_probes(vec![(
            Intent::StateRecall,
            vec![unit(0.0, 0.0, 1.0), unit(0.0, 1.0, 0.0)],
        )]);
        let (intent, score) = probes.classify(&unit(0.0, 1.0, 0.05)).unwrap();
        assert_eq!(intent, Intent::StateRecall);
        assert!(score > 0.95);
    }

    #[test]
    fn cosine_sim_identical_is_one() {
        let v = vec![0.6, 0.8, 0.0];
        assert!((cosine_sim(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_sim_orthogonal_is_zero() {
        assert!(cosine_sim(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_sim_zero_vector_is_zero() {
        assert_eq!(cosine_sim(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn zero_vector_abstains() {
        let probes = synthetic_probes();
        assert!(probes.classify(&[0.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn exemplar_hash_is_stable() {
        assert_eq!(exemplar_hash(), exemplar_hash());
    }

    #[test]
    fn thresholds_ordered_continue_stricter() {
        assert!(threshold(Intent::Continue) > threshold(Intent::StateRecall));
    }

    #[test]
    fn probe_cache_roundtrips_through_json() {
        let cache = ProbeCache {
            hash: exemplar_hash(),
            probes: vec![(Intent::Continue, vec![vec![0.1, 0.2]])],
        };
        let bytes = serde_json::to_vec(&cache).unwrap();
        let back: ProbeCache = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.hash, cache.hash);
        assert_eq!(back.probes.len(), 1);
        assert_eq!(back.probes[0].0, Intent::Continue);
    }
}
