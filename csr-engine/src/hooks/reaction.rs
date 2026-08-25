//! High-precision, abstaining reaction classifier for retrospective labels.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::embeddings::EmbeddingEngine;
use crate::hooks::intent::cosine_sim;

pub const MIN_MARGIN: f32 = 0.08;
pub const REASK_PICKUP_SIMILARITY: f32 = 0.82;
pub const REACTION_TURN_FILTER_VERSION: u32 = 1;
pub const PICKUP_QUESTION_FILTER_VERSION: u32 = 1;
const NEAR_MISS_FLOOR_GAP: f32 = 0.05;
const CACHE_SCHEMA: u32 = 1;

const NON_REACTION_PREFIXES: &[&str] = &[
    "Base directory for this skill:",
    "/",
    "[Request interrupted by user",
];
const QUESTION_LIKE_PREFIXES: &[&str] = &[
    "who ",
    "what ",
    "when ",
    "where ",
    "why ",
    "how ",
    "which ",
    "can ",
    "can you ",
    "could ",
    "could you ",
    "would ",
    "would you ",
    "will ",
    "will you ",
    "do ",
    "does ",
    "did ",
    "is ",
    "are ",
    "was ",
    "were ",
    "should ",
    "please explain ",
    "please tell ",
    "please show ",
];

pub fn is_queued_message(text: &str) -> bool {
    text.trim_start()
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("[queued]"))
}

fn is_image_only_payload(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(first) = lines.next() else {
        return false;
    };
    first.starts_with("[Image:")
        && first.ends_with(']')
        && lines.all(|line| line.starts_with("[Image:") && line.ends_with(']'))
}

pub fn is_substantive_reaction_text(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && !is_queued_message(text)
        && !is_image_only_payload(text)
        && !NON_REACTION_PREFIXES
            .iter()
            .any(|prefix| text.starts_with(prefix))
}

pub fn is_question_like_prior(text: &str) -> bool {
    if is_queued_message(text) || !is_substantive_reaction_text(text) {
        return false;
    }
    let normalized = text.trim().to_ascii_lowercase();
    normalized.contains('?')
        || QUESTION_LIKE_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
}

pub fn pickup_is_eligible(next_user_text: &str, preceding_user_text: &str) -> bool {
    is_substantive_reaction_text(next_user_text) && is_question_like_prior(preceding_user_text)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reaction {
    Acceptance,
    Correction,
    Reask,
    Redirect,
}

impl Reaction {
    pub const ALL: [Self; 4] = [
        Self::Acceptance,
        Self::Correction,
        Self::Reask,
        Self::Redirect,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Acceptance => "acceptance",
            Self::Correction => "correction",
            Self::Reask => "reask",
            Self::Redirect => "redirect",
        }
    }
}

const REACTION_EXEMPLARS: &[(Reaction, &[&str])] = &[
    (
        Reaction::Acceptance,
        &[
            "yes that is exactly right, please proceed with that approach",
            "great, that solved it",
            "perfect, this is what I needed",
            "that works, go ahead",
            "correct, continue from there",
            "thanks, the fix is working now",
            "looks good, implement the next step",
            "continue",
            "proceed",
            "go",
            "go ahead",
            "yes",
            "yes please",
            "okay continue",
            "approved proceed",
        ],
    ),
    (
        Reaction::Correction,
        &[
            "no, that is not what I asked for",
            "that is incorrect, the actual requirement is different",
            "you misunderstood me, I meant the other behavior",
            "not quite, please fix this specific mistake",
            "that assumption is wrong",
            "stop, you changed the wrong file",
            "this does not meet the requirement because",
            "you broke the server, please rerun it",
            "the server is still down, fix it and try again",
            "that change broke the working behavior",
            "this is still wrong, please redo it",
            "no, the result still does not work",
        ],
    ),
    (
        Reaction::Reask,
        &[
            "please answer my original question",
            "again, can you do what I asked",
            "you still have not answered the question",
            "let me ask the same thing another way",
            "can you actually complete the requested task",
            "I am repeating the request because it was missed",
        ],
    ),
    (
        Reaction::Redirect,
        &[
            "let us switch to a different topic",
            "never mind, work on something else instead",
            "new task: investigate this unrelated issue",
            "put that aside for now and focus on another feature",
            "change of direction, I want to discuss something different",
            "forget that, here is a separate request",
        ],
    ),
];

pub fn threshold(reaction: Reaction) -> f32 {
    match reaction {
        Reaction::Acceptance => 0.70,
        Reaction::Correction => 0.70,
        Reaction::Reask => 0.74,
        Reaction::Redirect => 0.76,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReactionDecision {
    pub reaction: Option<Reaction>,
    pub proposed_reaction: Option<Reaction>,
    pub confidence: f32,
    pub runner_up_score: f32,
    pub margin: f32,
    pub pickup_similarity: Option<f32>,
    pub near_miss: bool,
}

pub fn classify_scores(
    scores: &[(Reaction, f32)],
    pickup_similarity: Option<f32>,
) -> ReactionDecision {
    let mut ranked = scores.to_vec();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let proposed_reaction = ranked.first().map(|(reaction, _)| *reaction);
    let confidence = ranked.first().map_or(0.0, |(_, score)| *score);
    let runner_up_score = ranked.get(1).map_or(0.0, |(_, score)| *score);
    let margin = confidence - runner_up_score;
    let regular = proposed_reaction
        .filter(|reaction| confidence >= threshold(*reaction) && margin >= MIN_MARGIN);

    let correction_score = scores
        .iter()
        .find(|(reaction, _)| *reaction == Reaction::Correction)
        .map_or(0.0, |(_, score)| *score);
    let redirect_score = scores
        .iter()
        .find(|(reaction, _)| *reaction == Reaction::Redirect)
        .map_or(0.0, |(_, score)| *score);
    let pickup_reask = pickup_similarity.is_some_and(|similarity| {
        similarity >= REASK_PICKUP_SIMILARITY
            && correction_score < threshold(Reaction::Correction)
            && redirect_score < threshold(Reaction::Redirect)
    });
    let reaction = regular.or_else(|| pickup_reask.then_some(Reaction::Reask));
    let near_miss = reaction.is_none()
        && proposed_reaction
            .is_some_and(|candidate| confidence >= threshold(candidate) - NEAR_MISS_FLOOR_GAP);

    ReactionDecision {
        reaction,
        proposed_reaction,
        confidence,
        runner_up_score,
        margin,
        pickup_similarity,
        near_miss,
    }
}

fn excluded_turn_decision() -> ReactionDecision {
    ReactionDecision {
        reaction: None,
        proposed_reaction: None,
        confidence: 0.0,
        runner_up_score: 0.0,
        margin: 0.0,
        pickup_similarity: None,
        near_miss: false,
    }
}

pub fn classify_turn_scores(
    scores: &[(Reaction, f32)],
    next_user_text: &str,
    preceding_user_text: &str,
    pickup_similarity: Option<f32>,
) -> ReactionDecision {
    if is_queued_message(next_user_text) {
        return excluded_turn_decision();
    }
    let pickup_similarity = pickup_is_eligible(next_user_text, preceding_user_text)
        .then_some(pickup_similarity)
        .flatten();
    classify_scores(scores, pickup_similarity)
}

pub fn classifier_hash() -> String {
    classifier_hash_with_margin(MIN_MARGIN)
}

fn classifier_hash_with_margin(margin: f32) -> String {
    classifier_hash_with_versions(
        margin,
        REACTION_TURN_FILTER_VERSION,
        PICKUP_QUESTION_FILTER_VERSION,
    )
}

fn classifier_hash_with_versions(
    margin: f32,
    reaction_turn_filter_version: u32,
    pickup_question_filter_version: u32,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&CACHE_SCHEMA.to_le_bytes());
    hasher.update(&margin.to_le_bytes());
    hasher.update(&REASK_PICKUP_SIMILARITY.to_le_bytes());
    hasher.update(&reaction_turn_filter_version.to_le_bytes());
    hasher.update(&pickup_question_filter_version.to_le_bytes());
    for prefix in NON_REACTION_PREFIXES {
        hasher.update(prefix.as_bytes());
        hasher.update(&[0]);
    }
    for prefix in QUESTION_LIKE_PREFIXES {
        hasher.update(prefix.as_bytes());
        hasher.update(&[0]);
    }
    for reaction in Reaction::ALL {
        hasher.update(reaction.as_str().as_bytes());
        hasher.update(&threshold(reaction).to_le_bytes());
    }
    for (reaction, exemplars) in REACTION_EXEMPLARS {
        hasher.update(reaction.as_str().as_bytes());
        for exemplar in *exemplars {
            hasher.update(exemplar.as_bytes());
            hasher.update(&[0]);
        }
    }
    hasher.finalize().to_hex().to_string()
}

#[derive(Debug, Clone)]
pub struct ProbeSet {
    probes: Vec<(Reaction, Vec<Vec<f32>>)>,
}

#[derive(Serialize, Deserialize)]
struct ProbeCache {
    classifier_hash: String,
    probes: Vec<(Reaction, Vec<Vec<f32>>)>,
}

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        home.join(".claude-self-reflect")
            .join("reaction_probes.json")
    })
}

impl ProbeSet {
    pub async fn load_or_build(embeddings: &Arc<EmbeddingEngine>) -> Option<Self> {
        let hash = classifier_hash();
        if let Some(path) = cache_path() {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(cache) = serde_json::from_slice::<ProbeCache>(&bytes) {
                    if cache.classifier_hash == hash {
                        return Some(Self {
                            probes: cache.probes,
                        });
                    }
                }
            }
        }

        let engine = embeddings.clone();
        let probes = tokio::task::spawn_blocking(move || {
            REACTION_EXEMPLARS
                .iter()
                .map(|(reaction, sentences)| {
                    let vectors: Vec<Vec<f32>> = sentences
                        .iter()
                        .filter_map(|sentence| engine.embed_single(sentence).ok())
                        .collect();
                    (*reaction, vectors)
                })
                .collect::<Vec<_>>()
        })
        .await
        .ok()?;
        if probes.iter().any(|(_, vectors)| vectors.is_empty()) {
            return None;
        }
        if let Some(path) = cache_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec(&ProbeCache {
                classifier_hash: hash,
                probes: probes.clone(),
            }) {
                let _ = std::fs::write(path, bytes);
            }
        }
        Some(Self { probes })
    }

    pub fn scores(&self, vector: &[f32]) -> [(Reaction, f32); 4] {
        Reaction::ALL.map(|reaction| {
            let score = self
                .probes
                .iter()
                .find(|(candidate, _)| *candidate == reaction)
                .into_iter()
                .flat_map(|(_, vectors)| vectors)
                .map(|probe| cosine_sim(vector, probe))
                .fold(0.0, f32::max);
            (reaction, score)
        })
    }

    pub fn classify(
        &self,
        next_user_text: &str,
        preceding_user_text: &str,
        vector: &[f32],
        pickup_similarity: Option<f32>,
    ) -> ReactionDecision {
        classify_turn_scores(
            &self.scores(vector),
            next_user_text,
            preceding_user_text,
            pickup_similarity,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_each_class_only_above_its_floor_and_margin() {
        for reaction in [
            Reaction::Acceptance,
            Reaction::Correction,
            Reaction::Reask,
            Reaction::Redirect,
        ] {
            let scores = Reaction::ALL.map(|candidate| {
                if candidate == reaction {
                    (candidate, threshold(candidate) + 0.10)
                } else {
                    (candidate, 0.20)
                }
            });
            let decision = classify_scores(&scores, Some(0.10));
            assert_eq!(decision.reaction, Some(reaction));
            assert!(!decision.near_miss);
        }
    }

    #[test]
    fn abstains_when_winner_has_too_little_margin_and_marks_near_miss() {
        let decision = classify_scores(
            &[
                (Reaction::Acceptance, 0.82),
                (Reaction::Correction, 0.77),
                (Reaction::Reask, 0.10),
                (Reaction::Redirect, 0.05),
            ],
            None,
        );
        assert_eq!(decision.reaction, None);
        assert_eq!(decision.proposed_reaction, Some(Reaction::Acceptance));
        assert!(decision.near_miss);
    }

    #[test]
    fn pickup_similarity_can_classify_reask_but_not_override_correction() {
        let reask = classify_scores(
            &[
                (Reaction::Acceptance, 0.20),
                (Reaction::Correction, 0.30),
                (Reaction::Reask, 0.60),
                (Reaction::Redirect, 0.20),
            ],
            Some(0.84),
        );
        assert_eq!(reask.reaction, Some(Reaction::Reask));

        let correction = classify_scores(
            &[
                (Reaction::Acceptance, 0.20),
                (Reaction::Correction, 0.91),
                (Reaction::Reask, 0.60),
                (Reaction::Redirect, 0.20),
            ],
            Some(0.95),
        );
        assert_eq!(correction.reaction, Some(Reaction::Correction));
    }

    #[test]
    fn queued_turn_is_never_a_reaction_or_pickup_reask() {
        let decision = classify_turn_scores(
            &[
                (Reaction::Acceptance, 0.10),
                (Reaction::Correction, 0.91),
                (Reaction::Reask, 0.20),
                (Reaction::Redirect, 0.10),
            ],
            "[queued] No, use the other implementation",
            "Can you fix the implementation?",
            Some(0.99),
        );

        assert_eq!(decision.reaction, None);
        assert_eq!(decision.proposed_reaction, None);
        assert_eq!(decision.pickup_similarity, None);
    }

    #[test]
    fn audited_short_approval_score_clears_the_acceptance_floor() {
        let decision = classify_turn_scores(
            &[
                (Reaction::Acceptance, 0.7001),
                (Reaction::Correction, 0.40),
                (Reaction::Reask, 0.30),
                (Reaction::Redirect, 0.20),
            ],
            "continue",
            "Implement the approved plan.",
            Some(0.10),
        );

        assert_eq!(decision.reaction, Some(Reaction::Acceptance));
    }

    #[test]
    fn pickup_reask_requires_a_question_like_prior_turn() {
        let scores = [
            (Reaction::Acceptance, 0.20),
            (Reaction::Correction, 0.30),
            (Reaction::Reask, 0.60),
            (Reaction::Redirect, 0.20),
        ];

        let repeated_approval = classify_turn_scores(&scores, "continue", "continue", Some(1.0));
        let repeated_question = classify_turn_scores(
            &scores,
            "Can you explain that again?",
            "Can you explain that?",
            Some(0.95),
        );

        assert_eq!(repeated_approval.reaction, None);
        assert_eq!(repeated_approval.pickup_similarity, None);
        assert_eq!(repeated_question.reaction, Some(Reaction::Reask));
        assert_eq!(repeated_question.pickup_similarity, Some(0.95));
    }

    #[test]
    fn classifier_hash_changes_with_configuration() {
        assert_ne!(
            classifier_hash(),
            classifier_hash_with_margin(MIN_MARGIN + 0.01)
        );
    }

    #[test]
    fn classifier_hash_changes_with_reaction_turn_filter_version() {
        assert_ne!(
            classifier_hash(),
            classifier_hash_with_versions(
                MIN_MARGIN,
                REACTION_TURN_FILTER_VERSION + 1,
                PICKUP_QUESTION_FILTER_VERSION,
            )
        );
    }
}
