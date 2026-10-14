//! UserPromptSubmit hook — predictive injection on every user message.
//!
//! Fires when the user submits a prompt. Searches CSR for relevant past context
//! and injects it via stdout (hookSpecificOutput pattern — Claude Code prepends
//! the output to the system prompt).
//!
//! Fast-path exits (no engine work):
//! - No prompt or prompt too short (< 15 chars)
//! - Slash commands (starts with `/`)
//! - Empty JSON input
//!
//! When a relevant match is found, outputs formatted context to stdout with
//! a 500-token budget (larger than Stop's 300 — this is the main context path).
//!
//! Always returns Ok(()) — never blocks Claude Code (catch-all wrapper).

use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;
use crate::injection::anti_pattern;
use crate::injection::formatter;
use crate::injection::predictor::{self, RawResult};
use crate::injection::{InjectionContext, InjectionItem};
use crate::search::cross_project::resolve_project_from_cwd;
use crate::storage::trained_rerank::ExposureItem;
use crate::temporal;

/// Token budget for prompt-submit injection (larger than Stop hook's 300).
const PROMPT_TOKEN_BUDGET: usize = 500;

/// Minimum prompt length to trigger search (avoids noise from short prompts).
const MIN_PROMPT_LENGTH: usize = 15;

/// Minimum cosine similarity between the prompt and a session's best
/// reflection for the Route B episode-correlation pickup to fire. Calibrated
/// live 2026-07-07: a genuine redundant re-ask of the round-5 session scored
/// 0.46 against that session's v3 reflection — raw episode JSON scores far
/// lower, which is why correlation matches on *any* reflection and maps its
/// conv tag to the episode. Conservative bias: a silent miss costs one manual
/// lookup; a pickup on every prompt trains the model to ignore the channel.
/// Near-misses are logged to hook-timing.log for tuning.
const EPISODE_CORRELATION_MIN: f32 = 0.45;

/// Maximum age (in days) for chunk results. Chunks older than this are filtered out.
/// Reflections are exempt — they're intentionally stored for long-term recall.
/// Prevents 3-month-old conversations from winning on semantic similarity alone.
const MAX_CHUNK_AGE_DAYS: i64 = 21;

/// A derived reflection (session story, v3 extract, heuristic narrative,
/// context cache, convention fact) summarises a conversation and ages like
/// that conversation's chunks: past [`MAX_CHUNK_AGE_DAYS`] it is handed over
/// only on a strong match, raw cosine at or above `format::WEAK_BAND_TOP` (the
/// top of the band where genuine and fabricated matches overlap on this
/// corpus). The long-term exemption above was written for memories someone
/// chose to store; `mcp::tools::is_explicit_memory` rows keep it. Live
/// 2026-10-10: 5- to 7-month-old stories at cosine 0.45-0.55 filled the block,
/// the youngest of them "5 months ago".
///
/// A row that records what happened (an outcome, a lesson, a state reached;
/// `injection::specificity::states_a_finding`) keeps the usual cosine floor
/// for [`FINDING_MAX_AGE_DAYS`]; a row that only restates the ask that opened
/// its session does not. Blind-labelled replay 2026-10-11 (15 prompts, grader
/// without arm markers): of 21 items the 21-day bar removed, the 4 relevant
/// ones were 56-72 days old and each carried a finding, while 9 of the 15
/// irrelevant ones were request-only echoes. Cosine did not separate them:
/// the relevant four sat at 0.452-0.476, below 11 of the 15 irrelevant.
#[allow(clippy::too_many_arguments)]
fn is_stale_derived(
    id: &str,
    tags: &[String],
    excerpt: &str,
    timestamp: &str,
    raw_cosine: f32,
    now: chrono::DateTime<chrono::Utc>,
    common: &crate::injection::specificity::CommonTerms,
) -> bool {
    if crate::mcp::tools::is_explicit_memory(id, tags) || raw_cosine >= crate::format::WEAK_BAND_TOP
    {
        return false;
    }
    let Some(age_days) =
        crate::temporal::parse_timestamp(timestamp).map(|at| (now - at).num_days())
    else {
        return false;
    };
    if age_days <= MAX_CHUNK_AGE_DAYS {
        return false;
    }
    !(age_days <= FINDING_MAX_AGE_DAYS
        && crate::injection::specificity::states_a_finding(id, excerpt, common))
}

/// How long a derived row that records a finding keeps its standing on the
/// usual cosine floor: one half-life of the search decay
/// (`search::decay::DEFAULT_SCALE_DAYS`). Past it, as for every other derived
/// row, a strong match is needed; the 5- to 7-month-old rows of the live
/// 2026-10-10 injection stay out.
const FINDING_MAX_AGE_DAYS: i64 = crate::search::decay::DEFAULT_SCALE_DAYS as i64;

/// Handle the prompt-submit hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block Claude Code.
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    match render(input, engine, cwd).await {
        Ok(output) => print!("{output}"),
        Err(e) => eprintln!("CSR: prompt-submit hook error (non-fatal): {}", e),
    }

    // Chunk the active transcript after injection is printed to stdout.
    // Incremental: mtime check makes this a no-op when nothing changed (~0ms).
    // When new content exists: ~30-50ms for 1-2 chunks (well under perceptible lag).
    // Content becomes searchable by the next prompt submit.
    // Injection is done; from here nothing may touch stdout. Indexing inserts
    // HNSW points, and hnsw_rs prints to stdout on insert, which would land in
    // the model's context as fake retrieved memory. See super::suppress_stdout.
    super::suppress_stdout();
    super::import_current_transcript(input, engine, cwd).await;

    Ok(()) // Always succeed
}

/// Upper bound on `code_nodes_by_name` probes per prompt in `build_graph_slices`.
/// Without it, cost scales with prompt word count.
const MAX_SYMBOL_PROBES: usize = 12;

/// Maximum age (in minutes) to apply continuity boost.
const CONTINUITY_THRESHOLD_MINUTES: i64 = 2880;

/// Render the complete injection without touching process stdout or importing a
/// transcript. Both the legacy handler and the daemon use this exact pipeline.
pub async fn render(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<String> {
    Ok(prepare(input, engine, cwd).await?.delivered())
}

pub(super) struct PreparedPrompt {
    pub text: String,
    effects: PendingEffects,
}

impl PreparedPrompt {
    pub fn delivered(mut self) -> String {
        self.effects.commit();
        self.text
    }
}

/// Exposure bookkeeping is committed only after the hook acknowledges a full
/// response. A timeout must not teach the reranker that undelivered text was seen.
pub(super) async fn prepare(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
) -> Result<PreparedPrompt> {
    let mut prepared = PreparedPrompt {
        text: String::new(),
        effects: PendingEffects {
            storage: engine.storage().clone(),
            impressions: Vec::new(),
            retrievals: Vec::new(),
            dream_claims: Vec::new(),
        },
    };
    handle_inner(
        input,
        engine,
        cwd,
        &mut prepared.text,
        &mut prepared.effects,
    )
    .await?;
    Ok(prepared)
}

struct PendingImpression {
    session: Option<String>,
    project: String,
    surface: String,
    query: Option<String>,
    embedding: Option<Vec<f32>>,
    intent: String,
    items: Vec<ExposureItem>,
}

struct PendingEffects {
    storage: std::sync::Arc<crate::storage::Storage>,
    impressions: Vec<PendingImpression>,
    retrievals: Vec<(String, String, String)>,
    dream_claims: Vec<(String, Option<String>)>,
}

impl PendingEffects {
    #[allow(clippy::too_many_arguments)]
    fn record_impression(
        &mut self,
        session: Option<&str>,
        project: &str,
        surface: &str,
        query: Option<&str>,
        embedding: Option<&[f32]>,
        intent: &str,
        items: Vec<ExposureItem>,
    ) {
        self.impressions.push(PendingImpression {
            session: session.map(str::to_owned),
            project: project.into(),
            surface: surface.into(),
            query: query.map(str::to_owned),
            embedding: embedding.map(ToOwned::to_owned),
            intent: intent.into(),
            items,
        });
    }

    fn commit(&mut self) {
        // The claims are now receipts and must survive this value's drop.
        self.dream_claims.clear();
        for (id, source, session) in self.retrievals.drain(..) {
            let _ = self
                .storage
                .log_retrieval_event(&id, &source, "prompt_submit", &session);
        }
        for item in self.impressions.drain(..) {
            super::exposure::record_impression(
                &self.storage,
                item.session.as_deref(),
                &item.project,
                &item.surface,
                item.query.as_deref(),
                item.embedding.as_deref(),
                &item.intent,
                item.items,
            );
        }
    }
}

impl Drop for PendingEffects {
    fn drop(&mut self) {
        // Only claims won by this render are listed. Keep the claim throughout
        // rendering to preserve cross-process dedupe, but release it if no full
        // response was acknowledged. On the daemon this always runs on its one
        // blocking render worker, with the admission permits still held.
        for (id, session) in self.dream_claims.drain(..) {
            let _ = self.storage.with_connection(|conn| {
                conn.execute("DELETE FROM dream_deliveries WHERE dream_id = ?1 AND channel = 'prompt' AND session_id IS ?2",
                    rusqlite::params![id, session])?;
                Ok(())
            });
        }
    }
}

#[cfg(test)]
#[test]
fn undelivered_dream_claim_is_released_but_acknowledged_claim_survives() {
    use crate::storage::dream_delivery::{already_delivered, claim_delivery, DeliveryChannel};
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    for delivered in [false, true] {
        assert!(claim_delivery(
            &storage,
            "dream",
            DeliveryChannel::Prompt,
            Some("session")
        ));
        let mut pending = PendingEffects {
            storage: storage.clone(),
            impressions: Vec::new(),
            retrievals: Vec::new(),
            dream_claims: vec![("dream".into(), Some("session".into()))],
        };
        if delivered {
            pending.commit();
        }
        drop(pending);
        assert_eq!(
            storage
                .with_connection(|conn| {
                    Ok(already_delivered(conn, "dream", DeliveryChannel::Prompt))
                })
                .unwrap(),
            delivered
        );
    }
}

async fn handle_inner(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
    output: &mut String,
    effects: &mut PendingEffects,
) -> Result<()> {
    // Every exit that needs neither an embedding nor the index is decided in
    // `early_exit`, which also picks the engine this process was built with.
    let pasted = input.prompt.as_deref().unwrap_or_default();
    // Retrieval runs on the person's words only. A prompt that pastes CSR's
    // own output back (a pane hand-off list, the story, a recap, an injected
    // block) would otherwise find exactly the items it lists. `pasted` stays
    // the yardstick for "already in front of Claude" below.
    let asked = crate::extraction::provenance::strip_csr_emissions(pasted);
    let prompt = asked.as_str();
    match early_exit(pasted) {
        Some(EarlyExit::Silent) => return Ok(()),
        Some(EarlyExit::LineagePickup) => {
            if let Some((ep, age)) = pick_lineage_episode(engine, cwd, prompt) {
                emit_pickup(
                    output,
                    &ep,
                    &age,
                    "the user asked to continue; the episode below is the work being resumed.",
                );
                record_episode_exposure(input, effects, cwd, prompt, None, "continue", &ep, None);
            }
            return Ok(());
        }
        None => {}
    }
    let pasted_words = normalized_words(pasted);

    let embeddings = engine.embeddings();

    // P-1 fix: embed prompt ONCE — reused by intent classification, chunk
    // and reflection searches. Short prompts are embedded too: "keep at it"
    // is under MIN_PROMPT_LENGTH yet is exactly the continuation case where
    // memory IS the task — the literal list above only catches stock phrasings.
    let query_vec = {
        let q = prompt.to_string();
        let emb = embeddings.clone();
        match tokio::task::spawn_blocking(move || emb.embed_single(&q)).await {
            Ok(Ok(v)) => v,
            _ => return Ok(()), // Can't embed → nothing to inject
        }
    };
    let mut classified_intent = "other";

    // correlate_episode is expensive (semantic search over reflections); the
    // Explore arm and Route B want the same result for the same prompt, so the
    // first caller fills this memo and Route B reuses it on the fall-through.
    let mut correlated_memo: Option<Option<(crate::hooks::stop::Episode, String, f32)>> = None;

    // Route A (semantic): nearest-prototype intent classification over the
    // same embedding space. The literal phrase match above catches exact
    // continuations for free; this catches rephrasings ("keep at it",
    // "where did we leave off") and replaces the single-intent
    // STATE_RECALL_PROBES check. Falls through to Route B when the prompt
    // is a content question about specific past work.
    if let Some(probes) = crate::hooks::intent::ProbeSet::load_or_build(embeddings).await {
        if std::env::var("CSR_DEBUG_CORRELATE").is_ok() {
            for (i, s) in probes.scores(&query_vec) {
                eprintln!("CSR intent score: {i:?} {s:.3}");
            }
        }
        if let Some((intent, _score)) = probes.classify(&query_vec) {
            match intent {
                crate::hooks::intent::Intent::Continue => {
                    classified_intent = "continue";
                    if let Some((ep, age)) = pick_lineage_episode(engine, cwd, prompt) {
                        emit_pickup(
                            output,
                            &ep,
                            &age,
                            "the user asked to continue; the episode below is the work being resumed.",
                        );
                        record_episode_exposure(
                            input,
                            effects,
                            cwd,
                            prompt,
                            Some(&query_vec),
                            classified_intent,
                            &ep,
                            None,
                        );
                        return Ok(());
                    }
                }
                crate::hooks::intent::Intent::StateRecall => {
                    classified_intent = "state_recall";
                    if let Some((ep, age)) = pick_lineage_episode(engine, cwd, prompt) {
                        emit_pickup(
                            output,
                            &ep,
                            &age,
                            "the prompt asks for the state of recent work; the episode below is the most recent session (picked by recency, not similarity).",
                        );
                        record_episode_exposure(
                            input,
                            effects,
                            cwd,
                            prompt,
                            Some(&query_vec),
                            classified_intent,
                            &ep,
                            None,
                        );
                        return Ok(());
                    }
                }
                crate::hooks::intent::Intent::Explore => {
                    classified_intent = "explore";
                    // Exploration prompt: the user is asking WHERE code lives. The
                    // topic-matched episode (not the latest one) knows which files past
                    // work touched — hand those over instead of letting the agent
                    // re-map the codebase from scratch. Deliberately independent of
                    // the Tier-0 gate above: latest_tier0_episode fetches only the
                    // most recent 50 project-tagged reflections and requires
                    // episode_carries_state, so a project can have zero usable
                    // Tier-0 anchor (legacy tag data, or the matching episode pushed
                    // outside that 50-row window) while correlate_episode — which runs
                    // its own semantic search over reflections rather than reusing the
                    // Tier-0 fetch — still finds a topic-matched episode. Gating
                    // Explore on Tier-0 presence silently dropped the CODE MAP in
                    // exactly that case; this arm no longer depends on it.
                    //
                    // No integration test for the Tier-0-absent / correlate-present
                    // divergence: both paths call episode_carries_state, so a weak
                    // episode fails both. Real divergence is only via the tag/recency
                    // window (latest_tier0_episode's get_reflections_by_tag(..., 50)
                    // vs correlate_episode's independent semantic search), which
                    // needs 51+ project-tagged seeds and is not clean to construct
                    // here without heavy fixtures.
                    let current_project = crate::search::cross_project::resolve_project_from_cwd(
                        &cwd.to_string_lossy(),
                    )
                    .unwrap_or_default();
                    let corr = correlate_episode(
                        engine,
                        &query_vec,
                        &current_project,
                        input.session_id.as_deref(),
                        prompt,
                    )
                    .await;
                    if let Some((ep, age, _score)) = &corr {
                        if let Some(map) = format_code_map(ep, age) {
                            writeln!(output, "{}", map)?;
                            record_episode_exposure(
                                input,
                                effects,
                                cwd,
                                prompt,
                                Some(&query_vec),
                                classified_intent,
                                ep,
                                None,
                            );
                            return Ok(());
                        }
                    }
                    correlated_memo = Some(corr);
                    // No correlated episode or no files — normal flow continues below.
                }
            }
        }
    }

    // Below MIN_PROMPT_LENGTH there is no searchable content beyond intent —
    // and intent abstained above. Stop before content search.
    if prompt.len() < MIN_PROMPT_LENGTH {
        return Ok(());
    }

    // Journal v4 P5 delivery channel (c): the prompt names a symbol or file a
    // dream concluded on. Symbol-grade matching only, receipts mandatory, one
    // line, never repeated (see `hooks::dream_match`). Runs before the
    // anchor-overlap pointer below because a receipted conclusion outranks a
    // "was modified recently" pointer, and both are one line.
    if let Some(project) = resolve_project_from_cwd(&cwd.to_string_lossy()) {
        if let Some((dream_id, line)) = crate::hooks::dream_match::injection_with_id_for_prompt(
            engine.storage(),
            &project,
            prompt,
            input.session_id.as_deref(),
        ) {
            effects
                .dream_claims
                .push((dream_id.clone(), input.session_id.clone()));
            writeln!(output, "{line}")?;
            effects.record_impression(
                input.session_id.as_deref(),
                &project,
                "prompt_submit",
                Some(prompt),
                Some(&query_vec),
                classified_intent,
                vec![ExposureItem {
                    rank: 0,
                    memory_id: dream_id,
                    conversation_id: None,
                    source_type: "dream".into(),
                    baseline_score: None,
                    cosine: None,
                    recency: None,
                    graph_proximity: None,
                    author: None,
                    is_scaffold: false,
                    is_mechanic: false,
                    supersedes: false,
                }],
            );
        }
    }

    // Symbol-overlap gate: prompt names an anchored function → one-line pointer
    if let Some(project) = resolve_project_from_cwd(&cwd.to_string_lossy()) {
        if let Ok(rows) = engine.storage().get_project_anchors(&project, 200) {
            let names: Vec<(String, String)> =
                rows.into_iter().map(|(sid, a)| (sid, a.name)).collect();
            if let Some((sid, name)) = symbol_overlap(prompt, &names) {
                writeln!(output, "{}", format_anchor_notice(&name, &sid))?;
                effects.record_impression(
                    input.session_id.as_deref(),
                    &project,
                    "prompt_submit",
                    Some(prompt),
                    Some(&query_vec),
                    classified_intent,
                    vec![ExposureItem {
                        rank: 0,
                        memory_id: sid.clone(),
                        conversation_id: Some(sid),
                        source_type: "code_anchor".into(),
                        baseline_score: None,
                        cosine: None,
                        recency: None,
                        graph_proximity: Some(1.0),
                        author: None,
                        is_scaffold: false,
                        is_mechanic: false,
                        supersedes: false,
                    }],
                );
            }
        }
    }

    let storage = engine.storage();
    let search = engine.search();

    // Detect session continuity: find the most recent session in this project
    let continued_session_id = detect_continued_session_id(engine, cwd);

    // Resolve project early — used for all scoped searches including anti-patterns
    let current_project =
        crate::search::cross_project::resolve_project_from_cwd(&cwd.to_string_lossy())
            .unwrap_or_default();

    // 1. Search for anti-patterns (highest priority)
    // Anti-patterns use a modified query ("failed approach don't retry: ...") so keep separate embedding.
    // TODO: scope anti-patterns by project once outcome reflections carry project tags (Codex H-1)
    // Opt-out: this stage costs a second full embedding pass (the query is
    // rewritten to "failed approach don't retry: ..."), which the upstream
    // comment above keeps deliberately separate from the main query vector.
    // Set CSR_NO_ANTI_PATTERNS=1 to trade that recall for latency.
    let anti_matches = if std::env::var("CSR_NO_ANTI_PATTERNS").as_deref() == Ok("1") {
        Vec::new()
    } else {
        anti_pattern::find_anti_pattern_matches(storage, embeddings, search, prompt, 0.5, 2).await
    };
    let anti_exposures: Vec<Option<ExposureItem>> = anti_matches
        .iter()
        .map(|matched| {
            Some(ExposureItem {
                rank: -1,
                memory_id: matched.memory_id.clone(),
                conversation_id: None,
                source_type: "anti_pattern".into(),
                baseline_score: Some(f64::from(matched.item.score)),
                cosine: Some(f64::from(matched.item.score)),
                recency: None,
                graph_proximity: None,
                author: None,
                is_scaffold: false,
                is_mechanic: false,
                supersedes: false,
            })
        })
        .collect();
    let anti_patterns = anti_matches
        .into_iter()
        .map(|matched| matched.item)
        .collect();

    // Route B pickup: episode correlation. A content prompt may be re-asking
    // an already-solved problem or retelling past context from lossy human
    // memory — if it semantically matches a stored episode, surface that
    // episode with its retrieval handle, framed as prior art to verify (the
    // episode may be stale or the resemblance coincidental — never asserted
    // as the answer).
    let correlated = match correlated_memo {
        Some(memo) => memo,
        None => {
            correlate_episode(
                engine,
                &query_vec,
                &current_project,
                input.session_id.as_deref(),
                prompt,
            )
            .await
        }
    };
    let mut preface_exposures = Vec::new();
    // An episode whose ask the prompt quotes is the paste talking, not a match.
    let correlated = correlated.filter(|(ep, _, _)| {
        !episode_asks(ep)
            .iter()
            .any(|ask| quoted_in_prompt(ask, &pasted_words))
    });
    // What the PICKUP line already says, so PAST CONTEXT does not say it again.
    let mut handed = HandedOnce::default();
    if let Some((ep, age, score)) = correlated {
        output.push_str(&format_semantic_pickup(&ep, &age, score));
        preface_exposures.push(episode_exposure_item(&ep, Some(f64::from(score))));
        handed.note_episode(&ep);
    }

    // 2. Search chunks (past conversations) — scoped to current project
    // Over-fetch to compensate for project filtering (Codex M-1 fix)
    let chunk_results = search_chunks_with_vec(
        engine,
        &query_vec,
        15,
        0.55,
        &current_project,
        input.session_id.as_deref(),
        cwd,
    )
    .await;

    // 3. Search reflections (stored insights) — scoped to current project
    let reflection_results = search_reflections_with_vec(
        engine,
        &query_vec,
        10,
        0.45,
        &current_project,
        input.session_id.as_deref(),
    )
    .await;

    // 4. Combine and score results (with continuity boost for recent session)
    let current_files: Vec<String> = extract_file_paths_from_prompt(prompt);
    let current_errors: Vec<String> = extract_error_patterns_from_prompt(prompt);

    let mut raw_results: Vec<RawResult> = Vec::new();
    raw_results.extend(chunk_results);
    raw_results.extend(reflection_results);
    // The session each candidate came from: a chunk's conversation, a derived
    // reflection's `conv_` tag. Ranking drops tags, so record it here.
    let source_conversation: HashMap<String, String> = raw_results
        .iter()
        .filter_map(|r| {
            let conv = r.conversation_id.clone().or_else(|| {
                r.tags
                    .iter()
                    .find_map(|t| t.strip_prefix("conv_").map(str::to_owned))
            })?;
            Some((r.memory_id.clone()?, conv))
        })
        .collect();

    let ancestry_chunks: Vec<(String, String)> = raw_results
        .iter()
        .filter(|result| result.source == "chunk")
        .filter_map(|result| Some((result.memory_id.clone()?, result.conversation_id.clone()?)))
        .collect();
    let mut ancestry_conversations: Vec<String> = ancestry_chunks
        .iter()
        .map(|(_, conversation_id)| conversation_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    ancestry_conversations.sort();
    let ancestry_releases =
        crate::mcp::tools::resolve_validity_for_ancestry(storage, &ancestry_chunks)
            .map(|validity| {
                ancestry_releases_for_prompt(
                    storage
                        .ancestry_labels_for_conversations(&ancestry_conversations)
                        .unwrap_or_default(),
                    &ancestry_chunks,
                    &validity,
                )
            })
            .unwrap_or_default();

    let mut scored = predictor::rank_results_with_continuity_and_ancestry(
        raw_results,
        &current_files,
        &current_errors,
        Some(crate::injection::weights::HookPhase::PromptSubmit),
        continued_session_id.as_deref(),
        &ancestry_releases,
    );

    // 4b. Apply outcome-scored multiplier (v9: learning from past injection effectiveness)
    {
        let memory_ids: Vec<&str> = scored
            .iter()
            .filter_map(|r| r.memory_id.as_deref())
            .collect();
        if let Ok(stats) = storage.get_outcome_stats_batch(&memory_ids) {
            for result in &mut scored {
                if let Some(ref mid) = result.memory_id {
                    if let Some(&(successes, failures)) = stats.get(mid) {
                        result.final_score = predictor::apply_outcome_multiplier(
                            result.final_score,
                            successes,
                            failures,
                        );
                    }
                }
            }
            // Re-sort after outcome adjustment
            scored.sort_by(|a, b| {
                b.final_score
                    .partial_cmp(&a.final_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    if let Some(trained) = crate::search::trained_rerank::rerank_prompt_with_latest(
        storage,
        &scored,
        classified_intent,
    ) {
        scored = trained;
    }

    // 5. Session-aware review context (v9: code evolution + consolidated facts)
    let mut review_items: Vec<InjectionItem> = Vec::new();

    // 5a. Code evolution for files mentioned in prompt (scoped by project, Codex M-3)
    for file in current_files.iter().take(3) {
        if let Ok(evolutions) = storage.get_recent_code_evolution(file, &current_project, 3) {
            if !evolutions.is_empty() {
                let summary = format_evolution_summary(file, &evolutions);
                review_items.push(InjectionItem {
                    content: summary,
                    score: 0.9,
                    source: "code_evolution".into(),
                });
            }
        }
    }

    // 5b (removed 2026-07-07): consolidated-fact lines. In practice they
    // rendered as empty pattern labels ("[convention] Pattern: New file
    // creation") — zero-signal lines that train the model to skim the whole
    // injection. The facts remain queryable via MCP; they just don't auto-inject.

    // 5c. Code graph slice (v9.4) — capped relevant_context item within the
    // existing PROMPT_TOKEN_BUDGET (Codex #5: no separate budget). Surfaces the
    // 1-hop neighborhood of files/symbols named in the prompt, with provenance.
    let mut review_exposures = vec![None; review_items.len()];
    // Opt-out: graph slices issue one SQLite query per qualifying prompt token
    // (now bounded by MAX_SYMBOL_PROBES) plus a file-ledger read. Set
    // CSR_NO_GRAPH_SLICES=1 to skip the stage entirely.
    let graph_slices = if std::env::var("CSR_NO_GRAPH_SLICES").as_deref() == Ok("1") {
        Vec::new()
    } else {
        build_graph_slices_with_ids(storage, prompt, &current_files, &current_project)
    };
    for slice in graph_slices {
        review_items.push(InjectionItem {
            content: slice.content,
            score: 0.88,
            source: "code_graph".into(),
        });
        review_exposures.push(Some(ExposureItem {
            rank: -1,
            memory_id: slice.memory_id,
            conversation_id: None,
            source_type: "code_graph".into(),
            baseline_score: Some(0.88),
            cosine: None,
            recency: None,
            graph_proximity: Some(1.0),
            author: None,
            is_scaffold: false,
            is_mechanic: false,
            supersedes: false,
        }));
    }

    // 6. Build InjectionContext
    let mut ctx = InjectionContext {
        anti_patterns,
        relevant_context: review_items,
        ..Default::default()
    };

    // Content-based dedup: skip items whose first 200 chars overlap with already-seen items
    let mut seen_prefixes: HashSet<String> = HashSet::new();
    // Parallel metadata is kept separate from formatter content. Its indexes
    // are reconciled against the formatter receipt after budget truncation.
    let mut winning_exposures: Vec<Option<ExposureItem>> = Vec::new();

    // Distribute scored results into context categories.
    // Skip anti_patterns here — they're already loaded from find_anti_patterns() above.
    for result in scored.iter().take(5) {
        // Deduplicate: skip anti-patterns already found by find_anti_patterns
        if result.source == "anti_pattern" {
            continue;
        }

        // Content-based dedup (Bug 9): skip near-duplicate content
        // Use 200-char prefix to avoid false dedup on items sharing common preambles (F5 fix)
        let prefix: String = result.content.chars().take(200).collect();
        if !seen_prefixes.insert(prefix) {
            continue;
        }

        // Self-referential noise filter (Bug 4/5): skip content about CSR internals
        if is_self_referential_noise(&result.content) {
            continue;
        }

        // The person pasted or quoted this item, so it is already in front of
        // Claude: handing it back is the self-echo, not memory.
        if quoted_in_prompt(&result.content, &pasted_words) {
            continue;
        }

        // One session, one wrapper: the same work as a PICKUP episode, a
        // story, a v3 extract and a chunk quoting its last message is one item.
        let conversation = result
            .memory_id
            .as_ref()
            .and_then(|id| source_conversation.get(id));
        if !handed.admit(conversation, result.source != "chunk", &result.content) {
            continue;
        }

        // The bracket label carries the cached floor beside the source so a
        // tool-derived line and a user-authored line read differently; the
        // author is no longer shown (it was a MAX aggregate).
        let item = InjectionItem {
            content: result.content.clone(),
            score: result.final_score,
            source: handoff_label(result),
        };

        ctx.winning_strategies.push(item);
        winning_exposures.push(scored_exposure_item(result));
    }

    if ctx.is_empty() {
        record_prompt_exposure(
            input,
            effects,
            cwd,
            prompt,
            &query_vec,
            classified_intent,
            preface_exposures,
        );
        return Ok(());
    }

    let formatted = ctx.format_with_receipt(PROMPT_TOKEN_BUDGET);
    if !formatted.text.is_empty() {
        output.push_str(&formatted.text);
    }

    let rendered_exposures = super::exposure::rendered_exposure_items(
        &formatted,
        &anti_exposures,
        &review_exposures,
        &winning_exposures,
    );

    // TAD: Log retrieval events only for actually-injected items (not filtered-out ones)
    if let Some(ref session_id) = input.session_id {
        for item in &rendered_exposures {
            effects.retrievals.push((
                item.memory_id.clone(),
                item.source_type.clone(),
                session_id.clone(),
            ));
        }
    }
    preface_exposures.extend(rendered_exposures);
    record_prompt_exposure(
        input,
        effects,
        cwd,
        prompt,
        &query_vec,
        classified_intent,
        preface_exposures,
    );

    // Structured log to stderr (not visible to Claude, only to debug logs)
    let anti_count = ctx.anti_patterns.len();
    let total = ctx.total_items();
    if total > 0 {
        eprintln!(
            "CSR: Injected {} items ({} anti-patterns) for prompt",
            total, anti_count
        );
    }

    // Detailed injection log for diagnostics (written to hook-timing.log)
    log_injection_detail(
        "prompt-submit",
        prompt,
        total,
        anti_count,
        formatted.text.len(),
        &scored,
    );

    Ok(())
}

fn episode_exposure_item(
    episode: &crate::hooks::stop::Episode,
    score: Option<f64>,
) -> ExposureItem {
    ExposureItem {
        rank: -1,
        memory_id: episode.session_id.clone(),
        conversation_id: Some(episode.session_id.clone()),
        source_type: "episode".into(),
        baseline_score: score,
        cosine: score,
        recency: None,
        graph_proximity: None,
        author: None,
        is_scaffold: false,
        is_mechanic: false,
        supersedes: false,
    }
}

fn scored_exposure_item(result: &predictor::ScoredResult) -> Option<ExposureItem> {
    let memory_id = result.memory_id.clone()?;
    let recency = result.signals.iter().find_map(|signal| match signal {
        predictor::Signal::RecencyBoost(value) => Some(f64::from(*value)),
        _ => None,
    });
    Some(ExposureItem {
        rank: -1,
        memory_id,
        conversation_id: result.conversation_id.clone(),
        source_type: result.source.clone(),
        baseline_score: Some(f64::from(result.final_score)),
        cosine: Some(f64::from(result.raw_score)),
        recency,
        graph_proximity: None,
        author: (result.min_trust >= crate::provenance::TrustTier::UserHistory)
            .then(|| "user".to_string()),
        is_scaffold: crate::search::rerank::is_scaffold_text(&result.content),
        is_mechanic: crate::search::rerank::is_mechanic_text(&result.content),
        supersedes: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn record_prompt_exposure(
    input: &HookInput,
    effects: &mut PendingEffects,
    cwd: &Path,
    prompt: &str,
    query_vec: &[f32],
    intent: &str,
    mut items: Vec<ExposureItem>,
) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert((item.memory_id.clone(), item.source_type.clone())));
    for (rank, item) in items.iter_mut().enumerate() {
        item.rank = rank as i64;
    }
    let project = resolve_project_from_cwd(&cwd.to_string_lossy()).unwrap_or_default();
    effects.record_impression(
        input.session_id.as_deref(),
        &project,
        "prompt_submit",
        Some(prompt),
        Some(query_vec),
        intent,
        items,
    );
}

#[allow(clippy::too_many_arguments)]
fn record_episode_exposure(
    input: &HookInput,
    effects: &mut PendingEffects,
    cwd: &Path,
    prompt: &str,
    query_vec: Option<&[f32]>,
    intent: &str,
    episode: &crate::hooks::stop::Episode,
    score: Option<f64>,
) {
    let project = resolve_project_from_cwd(&cwd.to_string_lossy()).unwrap_or_default();
    effects.record_impression(
        input.session_id.as_deref(),
        &project,
        "prompt_submit",
        Some(prompt),
        query_vec,
        intent,
        vec![episode_exposure_item(episode, score)],
    );
}

fn ancestry_releases_for_prompt(
    labels: HashMap<String, crate::storage::ancestry::AncestryLabel>,
    chunks: &[(String, String)],
    validity: &HashMap<String, crate::mcp::tools::ConvValidity>,
) -> HashMap<String, u32> {
    chunks
        .iter()
        .filter(|(chunk_id, _)| !crate::mcp::tools::is_demote_channel(validity, chunk_id))
        .filter_map(|(chunk_id, conversation_id)| {
            labels
                .get(conversation_id)
                .and_then(|label| label.releases_behind_for_decay())
                .map(|releases| (chunk_id.clone(), releases))
        })
        .collect()
}

/// How a prompt leaves `handle_inner` before anything embeds it or queries
/// the index.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum EarlyExit {
    /// Nothing to inject.
    Silent,
    /// Route A pickup by recency: SQLite only, no embedding, no index.
    LineagePickup,
}

/// The exits that run before the first embedding or index query, in order.
/// One definition serves both the handler and [`exits_before_search`], so the
/// engine choice made before startup cannot drift from what the handler does.
/// Anything added here must stay index-free: these prompts run on an engine
/// whose index is empty.
pub(super) fn early_exit(prompt: &str) -> Option<EarlyExit> {
    // No prompt → silent exit
    if prompt.is_empty() {
        return Some(EarlyExit::Silent);
    }

    // Nobody typed a harness turn, so nothing in it is a re-asked question.
    // Live failure 2026-09-19: background-task notifications and
    // cross-session messages each drew a PICKUP for an unrelated episode at
    // 0.55-0.62 similarity, plus raw `<task-notification>` chunks as "past
    // context". Silence is the only honest output here.
    if is_harness_turn(prompt) {
        return Some(EarlyExit::Silent);
    }

    // Everything below judges the person's own words: a pasted CSR emission
    // (pane hand-off list, story, recap, injected block) is not an ask.
    let words = crate::extraction::provenance::strip_csr_emissions(prompt);
    let pasted_emission = words.split_whitespace().ne(prompt.split_whitespace());
    let prompt = words.as_str();
    if prompt.is_empty() {
        return Some(EarlyExit::Silent);
    }

    // Route A pickup: "continue"-class prompts are shorter than
    // MIN_PROMPT_LENGTH and carry zero searchable signal — yet they are the
    // one case where memory IS the task. No content to correlate, so recency
    // picks the episode. Emit it imperatively, adjacent to the prompt (the
    // position the model reliably reads), instead of silently skipping. Live
    // failure 2026-07-07: the previous session advertised "just say continue"
    // and this hook dropped that exact prompt.
    if is_continuation_prompt(prompt) {
        return Some(EarlyExit::LineagePickup);
    }

    // Fast-path: skip slash commands
    if prompt.starts_with('/') {
        return Some(EarlyExit::Silent);
    }

    // Bare acknowledgments ("ok", "y") carry no intent signal worth an
    // embedding pass.
    if prompt.trim().len() < 3 {
        return Some(EarlyExit::Silent);
    }

    // A reaction to pasted CSR output ("thoughts?") has nothing left to search
    // for once the paste is gone; searching the paste is the self-echo.
    if pasted_emission && prompt.len() < MIN_PROMPT_LENGTH {
        return Some(EarlyExit::Silent);
    }

    None
}

/// Whether this prompt is answered without the vector index, so the hook
/// process can skip loading it (see `hooks::hook_needs_index`).
pub(crate) fn exits_before_search(prompt: Option<&str>) -> bool {
    early_exit(prompt.unwrap_or_default()).is_some()
}

/// A turn the harness wrote, not the user: task/system notifications and
/// local-command echoes (the steer filter's own predicate — one definition,
/// not a second list to drift) plus messages relayed from another session.
fn is_harness_turn(prompt: &str) -> bool {
    crate::transcript::instrumentation::is_noisy_steer_text(prompt)
        || prompt.contains("<cross-session-message")
}

/// Detect the most recent session's conversation_id if it ended recently.
/// Returns `Some(conversation_id)` if within the continuity threshold.
/// A continuation prompt: the user is asking to resume prior work, so memory
/// IS the task. Matched tightly — a short prompt that opens with a
/// continuation phrase — so a longer prompt that merely contains "continue"
/// ("the tests continue to fail") is never hijacked into a pickup.
fn is_continuation_prompt(prompt: &str) -> bool {
    let p = prompt
        .trim()
        .trim_end_matches(['.', '!', '…', '?'])
        .trim_end()
        .to_lowercase();
    if p.len() > 60 {
        return false;
    }
    const PHRASES: [&str; 7] = [
        "continue",
        "resume",
        "carry on",
        "keep going",
        "pick up where we left off",
        "continue where we left off",
        "where were we",
    ];
    PHRASES
        .iter()
        .any(|ph| p == *ph || p.starts_with(&format!("{ph} ")))
}

/// The last line of a CSR PICKUP block.
pub(crate) const PICKUP_RESUME_LINE: &str =
    "Resume from LAST/NEXT above; run the Full state lookup before re-deriving anything.";

/// Emit the CSR PICKUP block: reason line, Tier-0 episode state, resume
/// imperative. Used only by Route A lineage pickup (is_continuation_prompt /
/// Continue / StateRecall) — the CONTINUUM banner is reserved for the active
/// conversation lineage, never for a Route-B topic match.
pub(crate) fn emit_pickup(
    output: &mut String,
    ep: &crate::hooks::stop::Episode,
    age: &str,
    reason: &str,
) {
    output.push_str(&format!("CSR PICKUP — {}\n", reason));
    output.push_str(&crate::hooks::session_start::format_tier0_block(
        ep,
        &[],
        age,
    ));
    output.push_str(PICKUP_RESUME_LINE);
    output.push('\n');
}

/// The one-line pointer for a prompt that names an anchored function.
pub(crate) fn format_anchor_notice(symbol: &str, session_id: &str) -> String {
    format!(
        "CSR: `{symbol}` was modified in a recent session — episode: csr_reflect_on_past(\"conv_{session_id}\")"
    )
}

/// What an episode asked: its first prompt, plus its latest task when that
/// differs. The pickup lines now name the latest task, so echo and paste
/// checks must see it too.
fn episode_asks(ep: &crate::hooks::stop::Episode) -> Vec<String> {
    let mut asks = vec![ep.request.clone()];
    if let Some(intent) = crate::hooks::recap::recap_intent(ep) {
        if intent != ep.request {
            asks.push(intent);
        }
    }
    asks
}

/// Whether the prompt restates any ask of the episode (see `is_pickup_echo`).
fn is_episode_echo(prompt: &str, ep: &crate::hooks::stop::Episode) -> bool {
    episode_asks(ep)
        .iter()
        .any(|ask| is_pickup_echo(prompt, ask))
}

/// Minimum length (both sides) before a prompt/episode-request containment
/// check is a signal rather than coincidence. Mirrors QUERY_ECHO_MIN_LEN in
/// search/reinstatement.rs::is_query_echo (verbatim/near-verbatim containment,
/// no embedding call) — kept local to this file rather than imported, per scope.
const PICKUP_ECHO_MIN_LEN: usize = 15;

/// True when `prompt` and `episode_request` are near-verbatim of each other
/// (either contains the other, both lowercased) and both are long enough for
/// containment to be a signal rather than coincidence. Catches the case where
/// the "matched" episode is itself just an earlier ASKING of the same
/// question, not state to resume — it must not be surfaced as a pickup.
fn is_pickup_echo(prompt: &str, episode_request: &str) -> bool {
    let p = prompt.trim().to_lowercase();
    let r = episode_request.trim().to_lowercase();
    if p.len() < PICKUP_ECHO_MIN_LEN || r.len() < PICKUP_ECHO_MIN_LEN {
        return false;
    }
    p.contains(&r) || r.contains(&p)
}

/// How many recency-ranked project episodes `pick_lineage_episode` will
/// consider before giving up — an echo skip needs at least one fallback
/// candidate, otherwise a single stale echoing episode silently blocks
/// every Route-A pickup.
const LINEAGE_CANDIDATE_LIMIT: usize = 3;

/// Lineage pickup: the most recent project episode that is NOT an echo
/// of the current prompt. Replaces bare `latest_tier0_episode` at every
/// Route-A call site so a stale episode whose own stored `request` just
/// restates the current question never displaces the active session
/// thread. Falls through up to LINEAGE_CANDIDATE_LIMIT recency-ranked
/// candidates; returns None (no pickup) if all are echoes.
fn pick_lineage_episode(
    engine: &Engine,
    cwd: &Path,
    prompt: &str,
) -> Option<(crate::hooks::stop::Episode, String)> {
    crate::hooks::session_start::recent_episodes(engine, cwd, LINEAGE_CANDIDATE_LIMIT)
        .into_iter()
        .find(|(ep, _)| !is_episode_echo(prompt, ep))
}

/// Semantic-pickup rendering for a topically-matched-but-not-lineage
/// episode (Route B correlation). Deliberately NOT the Tier-0 CONTINUUM
/// block (format_tier0_block) — that banner is reserved for the active
/// conversation lineage (Route A / SessionStart), so a topic match can
/// never be mistaken for "this is the session you're resuming". Renders
/// as a PICKUP reason line + a single EPISODE-INDEX-style line.
pub(crate) fn format_semantic_pickup(
    ep: &crate::hooks::stop::Episode,
    age: &str,
    score: f32,
) -> String {
    let reason = format!(
        "CSR PICKUP — this prompt matches a past episode (similarity {:.2}); \
         the work may already exist — verify against it before re-deriving.\n",
        score
    );
    reason + &crate::hooks::session_start::format_episode_index(&[(ep.clone(), age.to_string())])
}

/// Search floor for episode correlation — well below the firing threshold so
/// near-misses are visible in hook-timing.log for threshold tuning.
const EPISODE_CORRELATION_FLOOR: f32 = 0.30;

/// Half-life (days) for recency weighting in episode correlation ranking.
/// Meta-prompts ("what were we working on") embed closest to past copies of
/// the same question, so raw cosine alone anchors stale state — measured
/// 2026-07-08: a 3-week-old prompt-echo reflection (0.56) outranked the
/// 2-hour-old prior session (0.46). Ordering uses raw × 0.5^(age/half_life);
/// eligibility gates on the RAW score (and, past the chunk window, a strong
/// raw score: see `episode_may_anchor`) so old work stays resumable.
const EPISODE_RECENCY_HALF_LIFE_DAYS: f32 = 7.0;

/// Recency-weighted ranking score. Raw score decides *whether* a candidate
/// may anchor (EPISODE_CORRELATION_MIN); this decides *which* one wins.
fn effective_episode_score(raw: f32, age_days: f32) -> f32 {
    raw * 0.5_f32.powf(age_days.max(0.0) / EPISODE_RECENCY_HALF_LIFE_DAYS)
}

/// Whether a candidate may anchor a PICKUP at all. Past the chunk window
/// (`MAX_CHUNK_AGE_DAYS`) an episode needs a strong match, raw cosine at or
/// above `format::WEAK_BAND_TOP`: old work stays resumable when the prompt
/// really names it, but a weak-band resemblance to a months-old session is
/// mostly coincidence. Measured on 95 real prompts 2026-10-10 (replay, hand
/// labelled): 10 PICKUPs anchored episodes older than 21 days, all at
/// 0.45-0.60, and 1 of the 10 was relevant; 5 of the 8 younger ones were.
/// An unparseable timestamp (infinite age) keeps the raw gate only, as before.
fn episode_may_anchor(raw: f32, age_days: f32) -> bool {
    raw >= EPISODE_CORRELATION_MIN
        && (!age_days.is_finite()
            || age_days <= MAX_CHUNK_AGE_DAYS as f32
            || raw >= crate::format::WEAK_BAND_TOP)
}

/// Pick the winning candidate from (raw_score, age_days) pairs: highest
/// effective (recency-weighted) score among those [`episode_may_anchor`]
/// admits. Returns the index into `candidates`.
fn pick_episode(candidates: &[(f32, f32)]) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, (raw, age))| episode_may_anchor(*raw, *age))
        .max_by(|(_, a), (_, b)| {
            effective_episode_score(a.0, a.1).total_cmp(&effective_episode_score(b.0, b.1))
        })
        .map(|(i, _)| i)
}

/// Best stored episode whose *session* semantically matches the prompt.
///
/// Raw episode JSON embeds poorly (measured 2026-07-07: a genuine re-ask
/// scored 0.46 against the session's v3 reflection but the episode blob never
/// entered the top 20). So correlation matches on ANY reflection above the
/// threshold, then follows its `conv_<id>` tag to that session's episode.
/// Episode eligibility mirrors the Tier-0 anchor (project-scoped, carries
/// state); the current session's own episode is skipped — resurfacing the
/// session to itself is noise, not memory. The best sub-threshold candidate
/// is logged as a near-miss, not returned.
async fn correlate_episode(
    engine: &Engine,
    query_vec: &[f32],
    project: &str,
    current_session: Option<&str>,
    prompt: &str,
) -> Option<(crate::hooks::stop::Episode, String, f32)> {
    let results = {
        let idx = engine.search().read().await;
        idx.search_reflections(query_vec, 50, EPISODE_CORRELATION_FLOOR)
    };
    let common = engine.storage().reflection_common_terms();
    let project_tag = format!("project_{}", project);
    let now = chrono::Utc::now();
    let mut seen_convs: HashSet<String> = HashSet::new();
    let debug = std::env::var("CSR_DEBUG_CORRELATE").is_ok();
    // First the cheap per-row checks, in score order; then every surviving
    // conversation's episode in one lookup (one tag scan per conversation
    // was a quarter of render time); then the episode checks, in that order.
    let mut candidates: Vec<(f32, String)> = Vec::new();
    for r in &results {
        let Ok(Some((content, tags, _))) = engine.storage().get_reflection_by_id(&r.id) else {
            continue;
        };
        // A frame-only row ("Session in <p> project using <tools>") matches
        // prompts by its frame, so it cannot vouch for its session.
        if !tags.iter().any(|t| t == "session_episode")
            && crate::injection::specificity::is_generic(
                &formatter::truncate_item(&content, 300),
                &common,
            )
        {
            continue;
        }
        if debug {
            eprintln!(
                "CSR correlate raw hit: score={:.3} id={} tags={:?}",
                r.score, r.id, tags
            );
        }
        // Project scope: reflections tagged for another project never correlate;
        // legacy reflections without project tags pass (same rule as listing).
        if !project.is_empty()
            && tags.iter().any(|t| t.starts_with("project_"))
            && !tags.contains(&project_tag)
        {
            continue;
        }
        let Some(conv_id) = tags.iter().find_map(|t| t.strip_prefix("conv_")) else {
            continue;
        };
        if is_own_conversation(engine.storage(), conv_id, current_session)
            || !seen_convs.insert(conv_id.to_string())
        {
            continue;
        }
        candidates.push((r.score, conv_id.to_string()));
    }
    let mut episodes = episodes_for_conversations(engine, &candidates);
    // Collect every eligible candidate, then rank recency-weighted — raw
    // score order alone anchored a 3-week-old prompt-echo over the session
    // that ended two hours earlier (see EPISODE_RECENCY_HALF_LIFE_DAYS).
    let mut eligible: Vec<(f32, f32, crate::hooks::stop::Episode, String)> = Vec::new();
    for (score, conv_id) in &candidates {
        let Some((ep, ts)) = episodes.remove(conv_id) else {
            continue;
        };
        if !crate::hooks::session_start::episode_carries_state(&ep) {
            continue;
        }
        // Skip episodes whose stored request is a near-verbatim echo of the
        // current prompt — that episode was itself just asking the same
        // question, not state to resume as a pickup.
        if is_episode_echo(prompt, &ep) {
            continue;
        }
        // Unparseable timestamps rank as infinitely old (effective score 0)
        // but stay raw-gate eligible, so they can still win when alone.
        let age_days = crate::temporal::parse_timestamp(&ts)
            .map(|t| ((now - t).num_minutes() as f32 / 1440.0).max(0.0))
            .unwrap_or(f32::INFINITY);
        let age = crate::hooks::session_start::relative_time_label(&ts, &now);
        if debug {
            eprintln!(
                "CSR correlate candidate: raw={:.3} age_days={:.2} conv={}",
                score, age_days, ep.session_id
            );
        }
        eligible.push((*score, age_days, ep, age));
    }
    let scores: Vec<(f32, f32)> = eligible.iter().map(|e| (e.0, e.1)).collect();
    match pick_episode(&scores) {
        Some(i) => {
            let (raw, _, ep, age) = eligible.swap_remove(i);
            Some((ep, age, raw))
        }
        None => {
            // `results` arrive score-desc, so the first eligible is the best raw.
            if let Some((raw, _, ep, _)) = eligible.first() {
                log_episode_near_miss(*raw, &ep.session_id);
            }
            None
        }
    }
}

/// The stored episode of each conversation, via its `conv_<id>` tag: among the
/// conversation's 10 newest tagged reflections, the newest `session_episode`
/// row that parses, with that row's timestamp. One lookup for all of them.
fn episodes_for_conversations(
    engine: &Engine,
    candidates: &[(f32, String)],
) -> HashMap<String, (crate::hooks::stop::Episode, String)> {
    let ids: Vec<&str> = candidates.iter().map(|(_, id)| id.as_str()).collect();
    let Ok(rows) = engine.storage().episode_rows_by_conversation(&ids, 10) else {
        return HashMap::new();
    };
    rows.into_iter()
        .filter_map(|(conv, rows)| {
            rows.into_iter()
                .find_map(|(content, ts)| {
                    serde_json::from_str::<crate::hooks::stop::Episode>(&content)
                        .ok()
                        .map(|ep| (ep, ts))
                })
                .map(|found| (conv, found))
        })
        .collect()
}

/// Log the best sub-threshold episode candidate for threshold tuning.
fn log_episode_near_miss(score: f32, session_id: &str) {
    crate::telemetry::append_timing_line(&format!(
        "CSR prompt-submit episode near-miss: score={:.3} (min={}) conv={}",
        score, EPISODE_CORRELATION_MIN, session_id
    ));
}

fn detect_continued_session_id(engine: &Engine, cwd: &Path) -> Option<String> {
    let project = resolve_project_from_cwd(&cwd.to_string_lossy())?;
    let (conversation_id, timestamp) =
        engine.storage().latest_chunk_in_project(&project).ok()??;

    let ts = temporal::parse_timestamp(&timestamp)?;
    let age_minutes = (chrono::Utc::now() - ts).num_minutes();
    // C-2 fix: reject future timestamps (clock skew) and sessions beyond threshold
    if !(0..=CONTINUITY_THRESHOLD_MINUTES).contains(&age_minutes) {
        return None;
    }

    Some(conversation_id)
}

/// Openings of blocks the harness writes into the user channel. A chunk that
/// starts with one renders as nothing but that block in a 300-char excerpt.
const HARNESS_BLOCK_OPENINGS: [&str; 4] = [
    "<task-notification>",
    "[SYSTEM NOTIFICATION",
    "<cross-session-message",
    "Another Claude session sent a message:",
];

/// A chunk the hook must never hand back as "past context": the session that
/// is asking (its own last turns are already in the model's context, and the
/// watcher indexes them within minutes), or a harness block indexed before the
/// importer learnt to drop those rows.
fn is_self_echo(chunk: &crate::import::ConversationChunk, current_session: Option<&str>) -> bool {
    if current_session.is_some_and(|session| session == chunk.conversation_id) {
        return true;
    }
    let opening = chunk.content.trim_start();
    HARNESS_BLOCK_OPENINGS
        .iter()
        .any(|block| opening.starts_with(block))
}

/// Whether the conversation `conversation_id` is the asking session itself or
/// one of its own subagents (an `agent-` id whose recorded parent is the
/// session). Handing a session its own words, or its subagents', back as memory
/// is an echo, whichever retrieval path (raw chunk, derived reflection or
/// context cache) carried them.
fn is_own_conversation(
    storage: &crate::storage::Storage,
    conversation_id: &str,
    current_session: Option<&str>,
) -> bool {
    let Some(session) = current_session else {
        return false;
    };
    conversation_id == session
        || (conversation_id.starts_with("agent-")
            && storage
                .parent_session_of_conversation(conversation_id)
                .ok()
                .flatten()
                .as_deref()
                == Some(session))
}

/// A chunk one of this session's own subagents wrote: filed under the subagent's `agent-` id, with the
/// session that ran it as its parent in provenance. `is_self_echo` catches the session's own chunks by
/// id; these slip past it, and handing a session its own subagents' words back is the same echo.
fn is_own_subagent(
    chunk: &crate::import::ConversationChunk,
    current_session: Option<&str>,
    parent: impl FnOnce() -> Option<String>,
) -> bool {
    match current_session {
        Some(session) if chunk.conversation_id.starts_with("agent-") => {
            parent().as_deref() == Some(session)
        }
        _ => false,
    }
}

/// Search chunks using a pre-computed embedding vector (P-1 optimization).
/// Scoped to `project` — chunks from other projects are filtered out, and so
/// is anything `is_self_echo` names.
async fn search_chunks_with_vec(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
    cwd: &Path,
) -> Vec<RawResult> {
    search_chunks_with_vec_core(
        engine,
        query_vec,
        limit,
        min_score,
        project,
        current_session,
        |paths| super::scope_folder::conversations_in_asker_repo(paths, engine.projects_dir(), cwd),
    )
    .await
}

/// [`search_chunks_with_vec`] with the repository test supplied, so unit tests
/// decide which conversations widen without a filesystem or `git`. `same_repo`
/// is called at most once, and only when some candidate's label differs from
/// `project` and has a recorded transcript path. A prompt whose candidates all
/// match by label costs nothing; any other costs one batched query first.
async fn search_chunks_with_vec_core(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
    same_repo: impl FnOnce(HashMap<String, Vec<String>>) -> super::scope_folder::RepoConversations,
) -> Vec<RawResult> {
    let search = engine.search();
    let storage = engine.storage();

    let results = {
        let idx = search.read().await;
        idx.search_chunks(query_vec, limit, min_score)
    };

    let now = chrono::Utc::now();
    let mut raw_results = Vec::new();

    // Batch the row fetch. `get_chunks_by_ids` already takes a slice, but was
    // being called once per hit via `from_ref`, making this N+1 over the HNSW
    // result set. Fetch once and index by id, preserving the original result
    // order (and therefore score order) when rebuilding.
    let chunk_ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    // The batch fails as a unit, so one undecodable row would drop every hit.
    // On error fall back to per-id lookups, which lose only the bad row.
    let fetched = storage
        .get_chunks_by_ids_with_provenance(&chunk_ids)
        .unwrap_or_else(|_| {
            chunk_ids
                .iter()
                .filter_map(|id| {
                    storage
                        .get_chunks_by_ids_with_provenance(std::slice::from_ref(id))
                        .ok()
                })
                .flatten()
                .collect()
        });
    // A candidate filed under another label can still be this repository: a
    // session started in a subdirectory is labelled after that subdirectory's
    // folder. See `scope_folder` for the rule. A failed lookup widens nothing.
    let mut mismatched: Vec<String> = Vec::new();
    if !project.is_empty() {
        for chunk in fetched.iter().filter(|c| c.project_name != project) {
            if !mismatched.contains(&chunk.conversation_id) {
                mismatched.push(chunk.conversation_id.clone());
            }
        }
    }
    let widened = if mismatched.is_empty() {
        HashMap::new()
    } else {
        match storage.import_paths_for_conversations(
            &mismatched,
            super::scope_folder::MAX_PATHS_PER_CONVERSATION,
        ) {
            Ok(paths) if !paths.is_empty() => same_repo(paths),
            Ok(_) => HashMap::new(),
            Err(e) => {
                tracing::warn!(error = %e, "import_state lookup failed; exact project match only");
                HashMap::new()
            }
        }
    };

    let mut chunk_by_id: std::collections::HashMap<String, _> =
        fetched.into_iter().map(|c| (c.id.clone(), c)).collect();

    for result in &results {
        {
            if let Some(chunk) = chunk_by_id.remove(&result.id) {
                // Project scope filter: the label matches, or the conversation
                // was filed from inside the asker's repository and this chunk
                // carries the label of the folder it was filed under.
                if !project.is_empty()
                    && chunk.project_name != project
                    && !widened
                        .get(&chunk.conversation_id)
                        .is_some_and(|labels| labels.contains(&chunk.project_name))
                {
                    continue;
                }

                if is_self_echo(&chunk, current_session) {
                    continue;
                }
                if is_own_subagent(&chunk, current_session, || {
                    storage
                        .get_chunk_provenance(&result.id)
                        .ok()
                        .flatten()
                        .map(|provenance| provenance.source_conv_id)
                }) {
                    continue;
                }

                // Hard age gate: skip chunks older than MAX_CHUNK_AGE_DAYS
                // Prevents stale conversations from winning on semantic similarity alone
                if let Some(ts) = crate::temporal::parse_timestamp(&chunk.timestamp) {
                    let age_days = (now - ts).num_days();
                    // Reject future-dated chunks (clock skew) and stale chunks
                    if !(0..=MAX_CHUNK_AGE_DAYS).contains(&age_days) {
                        continue;
                    }
                }

                raw_results.push(RawResult {
                    content: formatter::truncate_item(&chunk.content, 300),
                    score: result.score,
                    source: "chunk".to_string(),
                    timestamp: Some(chunk.timestamp),
                    files: extract_file_paths(&chunk.content),
                    error_patterns: vec![],
                    tags: vec![],
                    conversation_id: Some(chunk.conversation_id),
                    memory_id: Some(result.id.clone()),
                    author: Some(chunk.author),
                    min_trust: storage
                        .get_chunk_min_trust(&result.id)
                        .unwrap_or(crate::provenance::TrustTier::Unknown),
                });
            }
        }
    }

    raw_results
}

/// Search reflections using a pre-computed embedding vector (P-1 optimization).
/// Scoped to `project` — reflections tagged for other projects are filtered out.
/// Legacy reflections without project tags are allowed through. A reflection
/// derived from the asking session, or from one of its own subagents (its
/// `conv_<id>` tag), is dropped like the raw chunks it was made from.
async fn search_reflections_with_vec(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
) -> Vec<RawResult> {
    let search = engine.search();
    let storage = engine.storage();

    let results = {
        let idx = search.read().await;
        idx.search_reflections(query_vec, limit, min_score)
    };
    let common = storage.reflection_common_terms();
    let now = chrono::Utc::now();

    let mut raw_results = Vec::new();
    for result in &results {
        if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(&result.id) {
            // Project scope filter: skip reflections tagged for other projects.
            // Legacy reflections without any project tag are allowed through.
            if !project.is_empty() {
                let project_tag = format!("project_{}", project);
                let has_project_tags = tags.iter().any(|t| t.starts_with("project_"));
                if has_project_tags && !tags.contains(&project_tag) {
                    continue;
                }
            }

            if tags
                .iter()
                .filter_map(|t| t.strip_prefix("conv_"))
                .any(|conv| is_own_conversation(storage, conv, current_session))
            {
                continue;
            }

            // Episode reflections are structured JSON surfaced via the Tier-0
            // CONTINUUM / pickup paths — never dump the raw blob as a context line.
            if tags.iter().any(|t| t == "session_episode") {
                continue;
            }

            // A row whose rendered excerpt is its writer's frame alone
            // ("Session in <p> project using <tools>", "[Heuristic] Project:
            // … Messages: …") says nothing about the work. See
            // `injection::specificity` for how the frame is measured.
            let excerpt = formatter::truncate_item(&content, 300);
            if is_stale_derived(
                &result.id,
                &tags,
                &excerpt,
                &timestamp,
                result.score,
                now,
                &common,
            ) {
                continue;
            }
            if crate::injection::specificity::is_generic(&excerpt, &common) {
                continue;
            }

            let source = if tags
                .iter()
                .any(|t| t == "outcome_incomplete" || t == "outcome_abandoned")
            {
                "anti_pattern"
            } else {
                "reflection"
            };
            let min_trust = storage
                .get_artifact_min_trust(
                    crate::storage::artifact_provenance::ArtifactKind::Reflection,
                    &result.id,
                )
                .unwrap_or(crate::provenance::TrustTier::Unknown);
            raw_results.push(RawResult {
                content: excerpt,
                score: result.score,
                source: source.to_string(),
                timestamp: Some(timestamp),
                files: extract_file_paths(&content),
                error_patterns: extract_error_patterns(&content),
                tags,
                conversation_id: None,
                memory_id: Some(result.id.clone()),
                author: Some(crate::provenance::Speaker::ToolResult),
                min_trust,
            });
        }
    }

    raw_results
}

/// Extract file paths from content (simple heuristic: lines containing common extensions).
fn extract_file_paths(content: &str) -> Vec<String> {
    let extensions = [
        ".rs", ".py", ".ts", ".js", ".toml", ".json", ".yaml", ".yml",
    ];
    let mut files = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim().trim_start_matches("- ");
        for ext in &extensions {
            if trimmed.ends_with(ext) || trimmed.contains(&format!("{} ", ext)) {
                // Extract the path-like token
                if let Some(path) = trimmed.split_whitespace().find(|w| w.contains(ext)) {
                    files.push(path.to_string());
                    break;
                }
            }
        }
    }

    files
}

/// Format code evolution records into a concise review context string.
fn format_evolution_summary(
    file: &str,
    evolutions: &[crate::storage::queries::CodeEvolutionRow],
) -> String {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for (_, _, fa, fr, _) in evolutions {
        if let Ok(fns) = serde_json::from_str::<Vec<String>>(fa) {
            added.extend(fns);
        }
        if let Ok(fns) = serde_json::from_str::<Vec<String>>(fr) {
            removed.extend(fns);
        }
    }
    added.sort();
    added.dedup();
    removed.sort();
    removed.dedup();

    let mut parts = vec![format!("{}: ", file)];
    if !added.is_empty() {
        parts.push(format!("+{} fns ({})", added.len(), added.join(", ")));
    }
    if !removed.is_empty() {
        if !added.is_empty() {
            parts.push(", ".into());
        }
        parts.push(format!("-{} fns ({})", removed.len(), removed.join(", ")));
    }
    parts.push(format!(" across {} edits", evolutions.len()));
    parts.concat()
}

/// True when `short` is a path-suffix of `long` (or they're equal), with
/// the match required to land on a path-segment boundary. Prevents basename
/// substring collisions such as "Ring.swift" spuriously matching
/// ".../ChannelRing.swift" — those are different files that merely share a
/// suffix of characters, not a suffix of path segments.
fn path_suffix_match(a: &str, b: &str) -> bool {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    if short.is_empty() {
        return false;
    }
    long == short
        || (long.ends_with(short) && long.as_bytes()[long.len() - short.len() - 1] == b'/')
}

/// Exploration-intent injection: file pointers from the correlated episode.
/// Payload over prose — each line is a path the agent can open immediately,
/// and the footer is a ready-to-run recall call (agents obey literal calls,
/// not "consider using" advice).
pub(crate) fn format_code_map(ep: &crate::hooks::stop::Episode, age: &str) -> Option<String> {
    let files: Vec<&String> = ep
        .files_modified
        .iter()
        .filter(|f| !f.trim().is_empty())
        .take(5)
        .collect();
    if files.is_empty() {
        return None;
    }
    let mut out = format!(
        "CSR CODE MAP — prompt matches feature work from conv_{} ({}):\n",
        ep.session_id, age
    );
    for f in &files {
        // Path-suffix match: anchor.file may be absolute while files_modified
        // entries are relative (or vice versa), depending on the extraction caller.
        // The suffix must land on a path-segment boundary — otherwise a
        // basename substring collision (e.g. "Ring.swift" vs
        // ".../ChannelRing.swift") would inflate the anchor count for an
        // unrelated file.
        let anchor_count = ep
            .anchors
            .iter()
            .filter(|a| path_suffix_match(&a.file, f))
            .count();
        let mut line = format!("  {}", f);
        if anchor_count > 0 {
            line.push_str(&format!(
                " ({} anchor{})",
                anchor_count,
                if anchor_count == 1 { "" } else { "s" }
            ));
        }
        // outcome is a plain String on Episode — use directly, no Option fallback.
        line.push_str(&format!(" (outcome={})", ep.outcome));
        // Char-boundary-safe truncate (paths are usually ASCII, but stay correct).
        if line.len() > 120 {
            let mut end = 120;
            while end > 0 && !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&format!(
        "Read these before mapping; full thread: csr_reflect_on_past(\"conv_{}\")\n",
        ep.session_id
    ));
    Some(out)
}

/// Build compact code-graph slices for files/symbols named in the prompt.
/// Returns at most 2 short lines. Callee names that come from an unresolved
/// placeholder edge (no verified definition) are rendered under an explicit
/// `unverified` label — never mixed silently into the plain call list.
pub(crate) fn build_graph_slices(
    storage: &crate::storage::Storage,
    prompt: &str,
    current_files: &[String],
    project: &str,
) -> Vec<String> {
    build_graph_slices_with_ids(storage, prompt, current_files, project)
        .into_iter()
        .map(|slice| slice.content)
        .collect()
}

struct TrackedGraphSlice {
    memory_id: String,
    content: String,
}

fn build_graph_slices_with_ids(
    storage: &crate::storage::Storage,
    prompt: &str,
    current_files: &[String],
    project: &str,
) -> Vec<TrackedGraphSlice> {
    let mut slices = Vec::new();

    // File-anchored slice: symbols + callers of files named in the prompt.
    // `ledger.callers` is already filtered to resolved=1 edges upstream
    // (src/storage/codegraph.rs file_ledger callers query), so these names
    // are verified already and do not need the unverified label.
    for file in current_files.iter().take(1) {
        if let Ok(ledger) = storage.code_file_ledger(project, file) {
            if !ledger.symbols.is_empty() {
                let syms: Vec<String> = ledger
                    .symbols
                    .iter()
                    .take(5)
                    .map(|s| s.name.clone())
                    .collect();
                let callers: Vec<String> = ledger
                    .callers
                    .iter()
                    .take(4)
                    .map(|(n, _)| n.clone())
                    .collect();
                let mut line = format!("{} — symbols: {}", file, syms.join(", "));
                if !callers.is_empty() {
                    line.push_str(&format!(" · callers: {}", callers.join(", ")));
                }
                slices.push(TrackedGraphSlice {
                    memory_id: ledger.symbols[0].id.clone(),
                    content: formatter::truncate_item(&line, 280),
                });
            }
        }
    }

    // Symbol-anchored slice: callees of a symbol named in the prompt.
    //
    // Each qualifying token costs one `code_nodes_by_name` query, and the loop
    // only breaks once a token actually yields a slice. A long prompt whose
    // words match no graph node therefore issues one SQLite query per word with
    // no upper bound. Cap the probes so worst-case cost is fixed.
    let mut symbol_probes = 0usize;
    for word in prompt.split_whitespace() {
        if symbol_probes >= MAX_SYMBOL_PROBES {
            break;
        }
        let token: String = word
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if token.len() < 6 {
            continue;
        }
        symbol_probes += 1;
        // Ask for 2 so an ambiguous name (multiple definitions sharing the
        // same name) is detectable instead of silently picking one.
        let nodes = match storage.code_nodes_by_name(&token, project, 2) {
            Ok(nodes) => nodes,
            Err(_) => continue,
        };
        if nodes.len() != 1 {
            // 0 matches: nothing to say. >1 matches: ambiguous name —
            // do not present one arbitrary definition as authoritative.
            continue;
        }
        let Some(node) = nodes.into_iter().next() else {
            continue;
        };
        let callees = match storage.code_query_callees(&node.id, 6) {
            Ok(callees) => callees,
            Err(_) => continue,
        };
        if callees.is_empty() {
            continue;
        }
        // A callee is verified only when it is backed by a real definition
        // node: not the `unresolved` placeholder sentinel, and not a
        // name-only match (see storage::codegraph::query_callees).
        let (verified, unverified): (Vec<_>, Vec<_>) = callees
            .iter()
            .partition(|c| c.kind != "unresolved" && !c.name_only);
        let verified_names: Vec<&str> = verified.iter().map(|c| c.name.as_str()).collect();
        let unverified_names: Vec<&str> = unverified.iter().map(|c| c.name.as_str()).collect();

        let line = if verified_names.is_empty() {
            // Every callee is an unresolved guess — say so explicitly
            // instead of presenting a bare list that looks like fact.
            format!(
                "{} — no verified callees; unverified guesses: {}",
                node.name,
                unverified_names.join(", ")
            )
        } else if unverified_names.is_empty() {
            format!("{} calls → {}", node.name, verified_names.join(", "))
        } else {
            format!(
                "{} calls → {} · unverified: {}",
                node.name,
                verified_names.join(", "),
                unverified_names.join(", ")
            )
        };
        slices.push(TrackedGraphSlice {
            memory_id: node.id,
            content: formatter::truncate_item(&line, 280),
        });
        break; // one symbol slice is enough for the budget
    }

    slices
}

/// Extract error-like patterns from content.
fn extract_error_patterns(content: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Heuristic: lines starting with error indicators
        if trimmed.starts_with("Error:")
            || trimmed.starts_with("error[")
            || trimmed.starts_with("error:")
            || trimmed.starts_with("panicked at")
            || trimmed.contains("FAILED")
        {
            patterns.push(trimmed.to_string());
        }
    }
    patterns
}

/// Extract file paths from the user's prompt text.
/// Looks for path-like tokens with common code extensions.
fn extract_file_paths_from_prompt(prompt: &str) -> Vec<String> {
    let extensions = [
        ".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".toml", ".json", ".yaml", ".yml", ".md",
        ".css", ".html", ".go", ".java", ".c", ".h", ".cpp",
    ];
    let mut files = Vec::new();
    for word in prompt.split_whitespace() {
        // Strip common surrounding punctuation (quotes, backticks, parens)
        let cleaned = word.trim_matches(|c: char| {
            c == '`' || c == '"' || c == '\'' || c == '(' || c == ')' || c == ','
        });
        if cleaned.contains('/') || cleaned.contains('.') {
            for ext in &extensions {
                if cleaned.ends_with(ext) {
                    files.push(cleaned.to_string());
                    break;
                }
            }
        }
    }
    files.dedup();
    files
}

/// Extract error-like patterns from the user's prompt text.
fn extract_error_patterns_from_prompt(prompt: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    for line in prompt.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Error:")
            || trimmed.starts_with("error[")
            || trimmed.starts_with("error:")
            || trimmed.starts_with("panicked at")
            || trimmed.contains("FAILED")
            || trimmed.contains("cannot find")
            || trimmed.contains("not found")
        {
            patterns.push(trimmed.to_string());
        }
    }
    patterns
}

/// Log injection details to the hook-timing.log for diagnostics.
fn log_injection_detail(
    hook: &str,
    query: &str,
    total_items: usize,
    anti_count: usize,
    stdout_bytes: usize,
    scored: &[predictor::ScoredResult],
) {
    let query_preview: String = query.chars().take(80).collect();
    let top_scores: Vec<String> = scored
        .iter()
        .take(3)
        .map(|s| format!("{:.3}/{}", s.final_score, s.source))
        .collect();
    let ancestry_candidates = scored
        .iter()
        .filter(|result| {
            result
                .signals
                .iter()
                .any(|signal| matches!(signal, predictor::Signal::AncestryDecay(_)))
        })
        .count();
    crate::telemetry::append_timing_line(&format!(
        "CSR {} inject: query=\"{}\" items={} anti={} ancestry={} stdout={}B top=[{}]",
        hook,
        query_preview,
        total_items,
        anti_count,
        ancestry_candidates,
        stdout_bytes,
        top_scores.join(", "),
    ));
}

/// Lowercased alphanumeric words of `text`, single-spaced: the form in which
/// two renderings of one item (markdown, quotes, labels aside) compare equal.
fn normalized_words(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Section labels the derived writers put in front of an item's own words
/// (`## User Request`, a ```` ```markdown ```` fence, `## Search Summary`).
const WRAPPER_LABELS: [&str; 3] = ["markdown ", "user request ", "search summary "];

/// Characters of normalized opening that identify an item: long enough that
/// two different items do not share it by accident, short enough to survive
/// the 300-char excerpt and a wrapper label.
const OPENING_KEY_CHARS: usize = 40;

/// Shortest opening that counts as evidence the same words recur.
const MIN_OPENING_KEY_CHARS: usize = 24;

/// The normalized opening of an item once a derived writer's wrapper label is
/// gone: the same note under `## User Request` and bare share it.
fn opening_key(content: &str) -> String {
    let mut words = normalized_words(content);
    while let Some(label) = WRAPPER_LABELS.iter().find(|l| words.starts_with(*l)) {
        words.drain(..label.len());
    }
    words.chars().take(OPENING_KEY_CHARS).collect()
}

/// What one injection has already handed over, by session and by opening
/// words, so the same work never goes out twice in different wrappers. Live
/// 2026-10-10: one block carried the "When claude is killed" note twice, bare
/// and under `## User Request`; on 2026-10-09 an episode went out as the PICKUP
/// line, its v3 extract and its story at once.
#[derive(Default)]
struct HandedOnce {
    /// Sessions handed over, and whether a wrapper (episode, story, extract)
    /// stood for one. Raw chunks of one session may sit side by side; nothing
    /// follows a wrapper of the same session.
    sessions: HashMap<String, bool>,
    openings: Vec<String>,
}

impl HandedOnce {
    fn note_episode(&mut self, episode: &crate::hooks::stop::Episode) {
        self.sessions.insert(episode.session_id.clone(), true);
        for text in episode_asks(episode) {
            self.note_opening(&text);
        }
        self.note_opening(&episode.completed);
    }

    fn note_opening(&mut self, text: &str) {
        let key = opening_key(text);
        if key.len() >= MIN_OPENING_KEY_CHARS {
            self.openings.push(key);
        }
    }

    /// Admit an item unless its session or its opening words are already out.
    fn admit(&mut self, session: Option<&String>, wrapper: bool, content: &str) -> bool {
        if let Some(session) = session {
            if let Some(by_wrapper) = self.sessions.get(session) {
                if wrapper || *by_wrapper {
                    return false;
                }
            }
        }
        let key = opening_key(content);
        if key.len() >= MIN_OPENING_KEY_CHARS
            && self
                .openings
                .iter()
                .any(|seen| seen.starts_with(&key) || key.starts_with(seen.as_str()))
        {
            return false;
        }
        if let Some(session) = session {
            *self.sessions.entry(session.clone()).or_default() |= wrapper;
        }
        self.note_opening(content);
        true
    }
}

/// Whether the item's opening already sits in the prompt: the person pasted or
/// quoted it, so Claude has it in front of it and handing it back is an echo.
fn quoted_in_prompt(content: &str, pasted_words: &str) -> bool {
    let key = opening_key(content);
    key.len() >= MIN_OPENING_KEY_CHARS && pasted_words.contains(&key)
}

/// Check if content is self-referential noise about CSR internals.
/// Prevents the tool's own development history from polluting its output (Bug 4/5).
/// CSR's own emitted blocks (probe reports, CONTINUUM/briefing echoes) are
/// detected structurally via `extraction::provenance`; the pattern list below
/// covers dev-history vocabulary that isn't an emission format.
fn is_self_referential_noise(content: &str) -> bool {
    if crate::extraction::provenance::is_csr_emission(content) {
        return true;
    }
    // CSR text the detector labels in search is skipped here, when context is
    // chosen: selection, not deletion. The chunk itself is untouched.
    if crate::import::contamination::has_csr_text(content) {
        return true;
    }
    // A row that is mostly a pasted CSR emission (another session quoting a
    // pane hand-off list or an injected block back) is CSR's words, not memory.
    let own = crate::extraction::provenance::strip_csr_emissions(content);
    if own.len() * 2 < content.trim().len() {
        return true;
    }
    const NOISE_PATTERNS: &[&str] = &[
        "session_start_hook",
        "session_end_hook",
        "prompt_submit_hook",
        "proves the hook",
        "proves the session",
        "proves the integration",
        "Current Ralph State:",
        "hook success",
        "hook error",
        "CSR engine ready",
        "hooks_integration",
    ];
    let lower = content.to_lowercase();
    NOISE_PATTERNS
        .iter()
        .any(|pattern| lower.contains(&pattern.to_lowercase()))
}

/// Match prompt text against anchored symbol names.
/// Names shorter than 6 chars are skipped (too collision-prone: main, init, run).
pub fn symbol_overlap(
    prompt: &str,
    anchors: &[(String, String)], // (session_id, name) — name may be qualified (A::new)
) -> Option<(String, String)> {
    anchors
        .iter()
        .find(|(_, name)| {
            // Match the bare segment: prompts say "validate_token", not "Auth::validate_token"
            let bare = name.rsplit("::").next().unwrap_or(name);
            bare.len() >= 6 && prompt.contains(bare)
        })
        .cloned()
}

/// The bracket label of one handed line: its source and cached trust floor, then the short id of
/// the past session it came from. Without the id Claude cannot open that session (the line was all
/// it had); with it, `get_full_conversation` finds it, as it matches an id by substring. A
/// subagent's chunk names no session Claude could open, so it carries none.
fn handoff_label(result: &crate::injection::predictor::ScoredResult) -> String {
    let session = result
        .conversation_id
        .as_deref()
        .filter(|id| !id.starts_with("agent-"))
        .and_then(|id| id.get(..8));
    match session {
        Some(id) => format!("{}|{}|session {id}", result.source, result.min_trust),
        None => format!("{}|{}", result.source, result.min_trust),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handed(conversation_id: Option<&str>) -> crate::injection::predictor::ScoredResult {
        crate::injection::predictor::ScoredResult {
            content: "the import reuses the upload queue".into(),
            raw_score: 0.8,
            final_score: 0.8,
            source: "conversation".into(),
            signals: Vec::new(),
            memory_id: Some("chunk-1".into()),
            conversation_id: conversation_id.map(str::to_owned),
            timestamp: None,
            author: None,
            min_trust: crate::provenance::TrustTier::Unknown,
        }
    }

    #[test]
    fn this_sessions_own_subagents_are_never_handed_back_to_it() {
        let chunk = |conversation_id: &str| crate::import::ConversationChunk {
            id: "c".into(),
            conversation_id: conversation_id.into(),
            project_name: "csr-engine".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: "the pane reads the dream log".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: true,
        };
        let parent = |id: &str| {
            let id = id.to_owned();
            move || Some(id)
        };
        let ours = chunk("agent-a3dd7b830b8b7fa52");
        assert!(is_own_subagent(&ours, Some("6ca2a4d5"), parent("6ca2a4d5")));
        // another session's subagent is memory, and so is a chunk with no parent on record
        assert!(!is_own_subagent(
            &ours,
            Some("6ca2a4d5"),
            parent("f98b6500")
        ));
        assert!(!is_own_subagent(&ours, Some("6ca2a4d5"), || None));
        // not a subagent: its parent is never looked up
        let plain = chunk("f98b6500-1111-4222-8333-444455556666");
        assert!(!is_own_subagent(&plain, Some("6ca2a4d5"), || panic!(
            "looked up"
        )));
        assert!(!is_own_subagent(&ours, None, || panic!("looked up")));
    }

    #[tokio::test]
    async fn reflections_derived_from_this_sessions_own_subagents_are_never_handed_back() {
        let root = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let mut search = crate::search::SearchEngine::new(8);
        // A subagent's chunk records the session that ran it as its parent.
        for (conv, parent) in [("agent-ours", "sess-1"), ("agent-theirs", "sess-2")] {
            let chunk = crate::import::ConversationChunk {
                id: format!("chunk-{conv}"),
                conversation_id: conv.into(),
                project_name: "project".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: "subagent words".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::Assistant,
                seq: 0,
                is_sidechain: true,
            };
            storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
            storage
                .insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: crate::provenance::Speaker::Assistant,
                        source_conv_id: parent.into(),
                        supersedes: None,
                    },
                )
                .unwrap();
        }
        for (id, conv) in [
            ("refl-own-child", "agent-ours"),
            ("refl-own-session", "sess-1"),
            ("refl-other-child", "agent-theirs"),
            ("refl-other-session", "sess-9"),
        ] {
            storage
                .insert_reflection(
                    id,
                    &format!("derived from {conv}"),
                    &[
                        "narrative_extracted_v3".into(),
                        format!("conv_{conv}"),
                        "project_project".into(),
                    ],
                    &[1.0, 0.0],
                )
                .unwrap();
            search.insert_reflection(id.into(), vec![1.0, 0.0]);
        }
        let engine = crate::engine::Engine::from_parts(
            storage,
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            std::sync::Arc::new(tokio::sync::RwLock::new(search)),
            root.path().to_path_buf(),
        );

        let ids = |results: Vec<RawResult>| {
            let mut ids: Vec<String> = results.into_iter().filter_map(|r| r.memory_id).collect();
            ids.sort();
            ids
        };
        let mine = ids(search_reflections_with_vec(
            &engine,
            &[1.0, 0.0],
            10,
            0.0,
            "project",
            Some("sess-1"),
        )
        .await);
        assert_eq!(mine, ["refl-other-child", "refl-other-session"]);
        // another session sees its own subagent's words as memory, and ours too
        let other = ids(search_reflections_with_vec(
            &engine,
            &[1.0, 0.0],
            10,
            0.0,
            "project",
            Some("sess-2"),
        )
        .await);
        assert_eq!(
            other,
            ["refl-other-session", "refl-own-child", "refl-own-session"]
        );
        // no session known: nothing is excluded
        let anon =
            ids(search_reflections_with_vec(&engine, &[1.0, 0.0], 10, 0.0, "project", None).await);
        assert_eq!(anon.len(), 4);
    }

    #[test]
    fn a_handed_line_names_the_session_claude_can_open() {
        let label = handoff_label(&handed(Some("a3e857eb-1111-4222-8333-444455556666")));
        assert!(label.starts_with("conversation|"), "{label}");
        assert!(label.ends_with("|session a3e857eb"), "{label}");
        // a subagent's chunk, or none at all: no id to open
        assert!(!handoff_label(&handed(Some("agent-a1b2c3d4e5"))).contains("session"));
        assert!(!handoff_label(&handed(None)).contains("session"));
    }

    #[tokio::test]
    async fn chunk_search_reads_cached_floor_without_provenance_event_tables() {
        let root = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let chunk = crate::import::ConversationChunk {
            id: "cached-search-floor".into(),
            conversation_id: "search-conv".into(),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: "cached structural trust".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET min_trust=3 WHERE id='cached-search-floor'",
                    [],
                )?;
                conn.execute("DROP TABLE chunk_spans", [])?;
                conn.execute("DROP TABLE provenance_events", [])?;
                Ok(())
            })
            .unwrap();
        let mut search = crate::search::SearchEngine::new(8);
        search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
        let engine = crate::engine::Engine::from_parts(
            storage,
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            std::sync::Arc::new(tokio::sync::RwLock::new(search)),
            root.path().to_path_buf(),
        );

        let results =
            search_chunks_with_vec_core(&engine, &[1.0, 0.0], 1, 0.0, "project", None, |_| {
                HashMap::new()
            })
            .await;

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].min_trust,
            crate::provenance::TrustTier::UserHistory
        );
    }

    #[tokio::test]
    async fn chunk_search_never_echoes_the_asking_session_or_a_harness_block() {
        let root = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let mut search = crate::search::SearchEngine::new(8);
        let rows = [
            ("own-turn", "live-session", "grep output from two minutes ago"),
            (
                "harness-block",
                "older-session",
                "<task-notification>\n<task-id>a0b2</task-id>\n<status>completed</status>\n</task-notification>\nRead the output file.",
            ),
            ("past-work", "older-session", "we moved the index load out of the hook"),
        ];
        for (id, conversation_id, content) in rows {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation_id.into(),
                project_name: "project".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
            search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
        }
        let engine = crate::engine::Engine::from_parts(
            storage,
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            std::sync::Arc::new(tokio::sync::RwLock::new(search)),
            root.path().to_path_buf(),
        );

        let recalled = |session: Option<&'static str>| {
            let engine = &engine;
            async move {
                let mut ids: Vec<String> = search_chunks_with_vec_core(
                    engine,
                    &[1.0, 0.0],
                    10,
                    0.0,
                    "project",
                    session,
                    |_| HashMap::new(),
                )
                .await
                .into_iter()
                .filter_map(|result| result.memory_id)
                .collect();
                ids.sort();
                ids
            }
        };

        assert_eq!(recalled(Some("live-session")).await, ["past-work"]);
        // Another session asking still sees that turn; the harness block never returns.
        assert_eq!(
            recalled(Some("someone-else")).await,
            ["own-turn", "past-work"]
        );
        assert_eq!(recalled(None).await, ["own-turn", "past-work"]);
    }

    /// Engine over three chunks filed under three labels, with the transcript
    /// paths `import_state` would hold for them. Returns the recalled ids and
    /// the path map `same_repo` was handed (`None` when it was never called).
    async fn recall_across_labels(
        project: &str,
        import_rows: &[(&str, &str)],
        widen: &[(&str, &str)],
    ) -> (Vec<String>, Option<HashMap<String, Vec<String>>>) {
        let root = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let mut search = crate::search::SearchEngine::new(8);
        for (id, conversation_id, label) in [
            ("root-chunk", "root-session", "repo"),
            ("sub-chunk", "sub-session", "repo-sub"),
            ("other-chunk", "other-session", "other"),
        ] {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation_id.into(),
                project_name: label.into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: format!("work recorded under {label}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
            search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
        }
        for (file_path, conversation_id) in import_rows {
            storage
                .upsert_import_state_explicit(file_path, conversation_id, 1, "0")
                .unwrap();
        }
        let engine = crate::engine::Engine::from_parts(
            storage,
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            std::sync::Arc::new(tokio::sync::RwLock::new(search)),
            root.path().to_path_buf(),
        );
        let seen = std::cell::RefCell::new(None);
        let mut ids: Vec<String> =
            search_chunks_with_vec_core(&engine, &[1.0, 0.0], 10, 0.0, project, None, |paths| {
                *seen.borrow_mut() = Some(paths.clone());
                widen
                    .iter()
                    .map(|(id, label)| (id.to_string(), HashSet::from([label.to_string()])))
                    .collect()
            })
            .await
            .into_iter()
            .filter_map(|result| result.memory_id)
            .collect();
        ids.sort();
        (ids, seen.into_inner())
    }

    const IMPORT_ROWS: [(&str, &str); 3] = [
        (
            "/cc/projects/-u-projects-repo/root-session.jsonl",
            "root-session",
        ),
        (
            "/cc/projects/-u-projects-repo-sub/sub-session.jsonl",
            "sub-session",
        ),
        (
            "/cc/projects/-u-projects-other/other-session.jsonl",
            "other-session",
        ),
    ];

    #[tokio::test]
    async fn a_conversation_the_repository_test_accepts_is_recalled_under_another_label() {
        let (ids, seen) =
            recall_across_labels("repo", &IMPORT_ROWS, &[("sub-session", "repo-sub")]).await;
        assert_eq!(ids, ["root-chunk", "sub-chunk"]);
        // Only the candidates whose label differs are put to the test, with
        // every path recorded for them; the exact match never is.
        let seen = seen.expect("same_repo is consulted when a label differs");
        let mut asked: Vec<&String> = seen.keys().collect();
        asked.sort();
        assert_eq!(asked, ["other-session", "sub-session"]);
        assert_eq!(
            seen["sub-session"],
            ["/cc/projects/-u-projects-repo-sub/sub-session.jsonl"]
        );
    }

    #[tokio::test]
    async fn a_widened_conversation_admits_only_the_label_of_its_folder() {
        // import_state is written after the chunks, so a chunk overwritten by a
        // same-stem file from elsewhere can sit under a conversation whose
        // recorded path passes. Its label is that other file's, and it stays out.
        let (ids, _) =
            recall_across_labels("repo", &IMPORT_ROWS, &[("sub-session", "something-else")]).await;
        assert_eq!(ids, ["root-chunk"]);
    }

    /// Through the production adapter: a real repository on disk, real `git`,
    /// decode from `/`, several `import_state` rows for one id.
    #[tokio::test]
    async fn the_production_rule_recalls_a_subdirectory_session_of_a_real_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, tools) = (root.join("projects/repo"), root.join("projects/repo-tools"));
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::create_dir_all(&tools).unwrap();
        let git_init = |dir: &Path| {
            let mut cmd = std::process::Command::new("git");
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("GIT_") {
                    cmd.env_remove(&k);
                }
            }
            cmd.arg("init")
                .arg("-q")
                .arg(dir)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !git_init(&repo) || !git_init(&tools) {
            return; // git unavailable in this environment
        }
        let projects_dir = root.join("cc");
        let folder = |dir: &Path| {
            crate::search::cross_project::encode_project_folder(&dir.to_string_lossy())
        };
        let in_sub = |name: &str| {
            format!(
                "{}/{}/{name}",
                projects_dir.display(),
                folder(&repo.join("sub"))
            )
        };
        let in_tools = |name: &str| format!("{}/{}/{name}", projects_dir.display(), folder(&tools));

        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let mut search = crate::search::SearchEngine::new(8);
        for (id, conversation_id, label) in [
            ("root-chunk", "s-root", "repo"),
            ("sub-chunk", "s-sub", "repo-sub"),
            ("overwritten-chunk", "s-sub", "elsewhere"),
            ("tools-chunk", "s-tools", "repo-tools"),
            ("shared-stem-chunk", "journal", "repo-sub"),
        ] {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation_id.into(),
                project_name: label.into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: format!("work recorded under {label}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
            search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
        }
        for (file_path, conversation_id) in [
            (in_sub("s-sub.jsonl"), "s-sub"),
            (in_tools("s-tools.jsonl"), "s-tools"),
            (in_sub("w1/journal.jsonl"), "journal"),
            (in_tools("w2/journal.jsonl"), "journal"),
        ] {
            storage
                .upsert_import_state_explicit(&file_path, conversation_id, 1, "0")
                .unwrap();
        }
        let engine = crate::engine::Engine::from_parts(
            storage,
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            std::sync::Arc::new(tokio::sync::RwLock::new(search)),
            projects_dir,
        );
        let mut ids: Vec<String> = search_chunks_with_vec(
            &engine,
            &[1.0, 0.0],
            10,
            0.0,
            "repo",
            None,
            &repo.join("sub"),
        )
        .await
        .into_iter()
        .filter_map(|result| result.memory_id)
        .collect();
        ids.sort();
        assert_eq!(ids, ["root-chunk", "sub-chunk"]);
    }

    #[tokio::test]
    async fn a_conversation_the_repository_test_rejects_stays_out() {
        let (ids, _) = recall_across_labels("repo", &IMPORT_ROWS, &[]).await;
        assert_eq!(ids, ["root-chunk"]);
    }

    #[tokio::test]
    async fn no_recorded_transcript_path_means_no_repository_test_at_all() {
        // Differing labels but nothing in import_state: nothing to decide on.
        let (ids, seen) = recall_across_labels("repo", &[], &[("sub-session", "repo-sub")]).await;
        assert_eq!(ids, ["root-chunk"]);
        assert!(seen.is_none());
    }

    #[tokio::test]
    async fn an_unscoped_search_never_consults_the_repository_test() {
        let (ids, seen) = recall_across_labels("", &IMPORT_ROWS, &[]).await;
        assert_eq!(ids, ["other-chunk", "root-chunk", "sub-chunk"]);
        assert!(seen.is_none());
    }

    #[test]
    fn prompt_ancestry_excludes_only_demoted_chunk_in_shared_conversation() {
        use crate::storage::ancestry::{AncestryLabel, AncestryState};

        let label = |conversation_id: &str| AncestryLabel {
            conversation_id: conversation_id.into(),
            state: AncestryState::Shipped,
            release_tag: Some("v1.0.0".into()),
            releases_behind: 5,
            repository: "/repo".into(),
            refreshed_at: "2026-08-06T12:00:00Z".into(),
        };
        let labels = [("shared".into(), label("shared"))].into_iter().collect();
        let chunks = vec![
            ("chunk-demoted".to_string(), "shared".to_string()),
            ("chunk-clean".to_string(), "shared".to_string()),
        ];
        let validity = [(
            "chunk-demoted".into(),
            crate::mcp::tools::ConvValidity {
                demote: true,
                note: "stale".into(),
                csr_only_demote: false,
            },
        )]
        .into_iter()
        .collect();

        let releases = ancestry_releases_for_prompt(labels, &chunks, &validity);

        assert!(!releases.contains_key("chunk-demoted"));
        assert_eq!(releases.get("chunk-clean"), Some(&5));
    }

    // --- episode recency ranking (Route B stale-anchor fix) ---

    #[test]
    fn fresh_episode_outranks_stale_higher_score() {
        // Live failure 2026-07-08: a 3-week-old prompt-echo reflection (0.56)
        // beat the 2-hour-old prior session (0.46), so PICKUP anchored stale
        // state. Candidates are (raw_score, age_days).
        let cands = vec![(0.56, 21.0), (0.46, 0.1)];
        assert_eq!(pick_episode(&cands), Some(1));
    }

    #[test]
    fn stale_episode_still_reachable_when_only_match() {
        // Old work must stay resumable — recency weights the ordering; past
        // the chunk window it takes a strong match (see episode_may_anchor).
        assert_eq!(pick_episode(&[(0.56, 21.0)]), Some(0));
        assert_eq!(pick_episode(&[(0.70, 72.0)]), Some(0));
    }

    #[test]
    fn a_weak_resemblance_to_months_old_work_is_not_a_pickup() {
        // The candidates the pasted hand-off list drew on 2026-10-10 (raw,
        // age in days); only the 20.6-day-old one was young enough, and it
        // fired at 0.515 on an unrelated episode. The person's own words in
        // that prompt score 0.328 against the best episode.
        let pasted = [
            (0.542, 72.39),
            (0.536, 95.68),
            (0.526, 26.13),
            (0.507, 121.24),
        ];
        assert_eq!(pick_episode(&pasted), None);
        assert_eq!(pick_episode(&[(0.328, 6.37)]), None);
        // Inside the chunk window the raw gate alone decides, as before.
        assert_eq!(pick_episode(&[(0.515, 20.56)]), Some(0));
        // Unknown age keeps the raw gate only.
        assert_eq!(pick_episode(&[(0.50, f32::INFINITY)]), Some(0));
    }

    #[test]
    fn below_min_similarity_never_anchors() {
        assert_eq!(pick_episode(&[(0.44, 0.1)]), None);
        assert_eq!(pick_episode(&[]), None);
    }

    #[test]
    fn zero_age_effective_score_is_raw() {
        assert!((effective_episode_score(0.5, 0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn equal_age_preserves_score_order() {
        assert_eq!(pick_episode(&[(0.46, 1.0), (0.56, 1.0)]), Some(1));
    }

    #[test]
    fn symbol_overlap_detects_anchored_names() {
        let anchors = vec![
            ("s1".to_string(), "validate_token".to_string()),
            ("s1".to_string(), "main".to_string()), // short/common — must be ignored
        ];
        assert_eq!(
            symbol_overlap("the validate_token check is wrong", &anchors),
            Some(("s1".to_string(), "validate_token".to_string()))
        );
        assert_eq!(symbol_overlap("main thing to do today", &anchors), None);
        assert_eq!(symbol_overlap("unrelated prompt", &anchors), None);
    }

    #[test]
    fn test_extract_file_paths() {
        let content = "Modified files:\n- src/auth.rs\n- Cargo.toml\n- some text";
        let files = extract_file_paths(content);
        assert!(files.contains(&"src/auth.rs".to_string()));
        assert!(files.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn test_extract_error_patterns() {
        let content =
            "Log:\nError: connection refused\ninfo: compiling\nerror[E0308]: type mismatch";
        let errors = extract_error_patterns(content);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("connection refused"));
        assert!(errors[1].contains("E0308"));
    }

    #[test]
    fn test_slash_command_detection() {
        // Verify our fast-path logic
        assert!("/help".starts_with('/'));
        assert!("/commit".starts_with('/'));
        assert!(!"fix the bug".starts_with('/'));
    }

    #[test]
    fn test_min_prompt_length() {
        assert!("short".len() < MIN_PROMPT_LENGTH);
        assert!("fix the authentication timeout bug".len() >= MIN_PROMPT_LENGTH);
    }

    #[test]
    fn test_max_chunk_age_constant() {
        // Verify the age gate is set to 21 days
        assert_eq!(MAX_CHUNK_AGE_DAYS, 21);
        // Reflections should NOT be gated (only chunks)
        // This is enforced by the filter only being in search_chunks_with_vec,
        // not in search_reflections_with_vec.
    }

    #[test]
    fn test_self_referential_noise_detected() {
        assert!(is_self_referential_noise(
            "This proves the session_start_hook.py successfully retrieved"
        ));
        assert!(is_self_referential_noise(
            "Current Ralph State: Iteration: 198"
        ));
        assert!(is_self_referential_noise(
            "hook success: session-end completed"
        ));
        assert!(is_self_referential_noise(
            "CSR engine ready. Session: abc123"
        ));
        assert!(is_self_referential_noise(
            "Test in hooks_integration module"
        ));
        assert!(is_self_referential_noise("proves the hook works correctly"));
    }

    #[test]
    fn test_project_scope_reflection_filter_logic() {
        // Reflections tagged for a different project should be filtered
        let tags_other = ["project_anukriti".to_string(), "session_story".to_string()];
        let project = "csr";
        let project_tag = format!("project_{}", project);
        let has_project_tags = tags_other.iter().any(|t| t.starts_with("project_"));
        assert!(has_project_tags && !tags_other.contains(&project_tag));

        // Legacy reflections without project tags should pass through
        let tags_legacy = ["session_story".to_string()];
        let has_project_tags_legacy = tags_legacy.iter().any(|t| t.starts_with("project_"));
        assert!(!has_project_tags_legacy); // no filter applied

        // Same-project reflections should pass
        let tags_same = ["project_csr".to_string(), "session_story".to_string()];
        let has_project_tags_same = tags_same.iter().any(|t| t.starts_with("project_"));
        assert!(has_project_tags_same && tags_same.contains(&project_tag));
    }

    /// Context selection skips a chunk the detector labels (CSR's own pane
    /// sends); a person's sentence that only mentions the words stays eligible.
    #[test]
    fn predictive_context_skips_chunks_with_a_detector_hit() {
        assert!(is_self_referential_noise(
            "A standing rule from 10-12: “Keep receipts”"
        ));
        assert!(is_self_referential_noise(
            "CSR remembered: “Use component ownership”."
        ));
        assert!(!is_self_referential_noise(
            "My mother said it best: `A standing rule from 10-12: “Keep receipts”` is worth keeping in the docs."
        ));
    }

    #[test]
    fn test_non_noise_content_passes() {
        assert!(!is_self_referential_noise(
            "Fix the authentication timeout bug"
        ));
        assert!(!is_self_referential_noise(
            "Docker compose memory issue resolved"
        ));
        assert!(!is_self_referential_noise(
            "Use batch embedding for 3x speedup"
        ));
        // "proves the" without hook/session/integration should NOT be filtered (F6 fix)
        assert!(!is_self_referential_noise(
            "This proves the approach works for authentication"
        ));
    }

    // --- is_continuation_prompt boundaries (Route A classifier) ---
    // Route A only handles no-signal prompts recency-style; content prompts
    // are covered by Route B semantic correlation, so these asserts test the
    // boundary (hijack prevention), not the phrase list.

    #[test]
    fn harness_turns_are_recognised_and_typed_prompts_are_not() {
        for harness in [
            "<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\n<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>\n</system-reminder>",
            "[SYSTEM NOTIFICATION - NOT USER INPUT] background job finished",
            "[queued] [SYSTEM NOTIFICATION - NOT USER INPUT] background job finished",
            "<local-command-caveat>Caveat: generated by local commands</local-command-caveat>",
            "<cross-session-message from=\"uds:/tmp/x.sock\" from-name=\"peer\">feedback for you</cross-session-message>",
        ] {
            assert!(is_harness_turn(harness), "{harness}");
        }
        for typed in [
            "why does the task notification hook fire twice?",
            "fix the statusline badge count",
            "continue",
        ] {
            assert!(!is_harness_turn(typed), "{typed}");
        }
    }

    #[test]
    fn early_exits_skip_the_index_and_searchable_prompts_do_not() {
        assert_eq!(early_exit(""), Some(EarlyExit::Silent));
        assert_eq!(
            early_exit("<task-notification>\n<status>completed</status>\n</task-notification>"),
            Some(EarlyExit::Silent)
        );
        assert_eq!(early_exit("/compact"), Some(EarlyExit::Silent));
        assert_eq!(early_exit("ok"), Some(EarlyExit::Silent));
        assert_eq!(early_exit("continue"), Some(EarlyExit::LineagePickup));
        // A harness turn that happens to open with a continuation phrase stays silent.
        assert_eq!(
            early_exit("continue <task-notification>done</task-notification>"),
            Some(EarlyExit::Silent)
        );
        assert!(exits_before_search(None));
        assert!(exits_before_search(Some("continue")));
        // Short but not a stock phrase: the intent classifier may route it to
        // the Explore arm, which searches reflections.
        for searched in [
            "where is auth?",
            "keep at it",
            "fix the statusline badge count",
        ] {
            assert_eq!(early_exit(searched), None, "{searched}");
            assert!(!exits_before_search(Some(searched)), "{searched}");
        }
    }

    #[test]
    fn continuation_prompt_boundaries() {
        // Bare and lightly decorated forms fire.
        assert!(is_continuation_prompt("continue"));
        assert!(is_continuation_prompt("  Resume.  "));
        assert!(is_continuation_prompt("continue with the rerank work"));
        // Contains the verb but doesn't open with it — never hijacked.
        assert!(!is_continuation_prompt("the tests continue to fail on CI"));
        // Opens with the verb but is a long, self-scoped instruction (>60 chars).
        assert!(!is_continuation_prompt(
            "continue refactoring the session_start module and then rewrite the anchor verification logic"
        ));
        assert!(!is_continuation_prompt("fix the auth bug"));
        assert!(!is_continuation_prompt("/continue"));
    }

    // --- pickup echo guard (stale episode whose request restates the prompt) ---

    #[test]
    fn is_pickup_echo_rejects_near_verbatim_match() {
        // Containment either direction, both sides long enough — the episode
        // was itself just asking the same question, not state to resume.
        assert!(is_pickup_echo(
            "what's the status of the sessionstart debugging work",
            "status of the sessionstart debugging work",
        ));
        assert!(is_pickup_echo(
            "status of the sessionstart debugging work",
            "what's the status of the sessionstart debugging work",
        ));
    }

    #[test]
    fn an_episode_whose_latest_task_restates_the_prompt_is_an_echo() {
        let mut ep = code_map_episode();
        ep.request = "anthropics codemod side pane thoughts for herdr users".into();
        ep.message_count = 30_393;
        ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
        ep.anchor_request = Some("How do we show cost as % of max plan".into());
        assert!(is_episode_echo("how do we show cost as % of max plan", &ep));
        // The first prompt still counts, as before.
        assert!(is_episode_echo(
            "anthropics codemod side pane thoughts for herdr users",
            &ep
        ));
        assert!(!is_episode_echo("fix the login validation for signup", &ep));

        let mut handed = HandedOnce::default();
        handed.note_episode(&ep);
        assert!(!handed.admit(None, true, "How do we show cost as % of max plan, again"));
    }

    #[test]
    fn is_pickup_echo_accepts_unrelated_or_paraphrase() {
        // Genuine content / paraphrase against a different episode request —
        // no containment, so not an echo.
        assert!(!is_pickup_echo(
            "fix the auth timeout bug",
            "status of the sessionstart debugging work",
        ));
        // Too short on either side is coincidence, not a signal.
        assert!(!is_pickup_echo(
            "short",
            "status of the sessionstart debugging work"
        ));
        assert!(!is_pickup_echo(
            "what's the status of the sessionstart debugging work",
            "short",
        ));
    }

    #[test]
    fn format_semantic_pickup_has_no_continuum_banner() {
        // Route B topic matches must never render the lineage CONTINUUM block.
        let out = format_semantic_pickup(&code_map_episode(), "2d ago", 0.52);
        assert!(out.contains("CSR PICKUP"));
        assert!(
            !out.contains("CSR CONTINUUM"),
            "semantic pickup must not render lineage CONTINUUM banner: {out}"
        );
        assert!(out.contains("csr_reflect_on_past(\"conv_abc123\")"));
    }

    // --- format_code_map (Feature B, exploration-intent injection) ---

    fn code_map_episode() -> crate::hooks::stop::Episode {
        crate::hooks::stop::Episode {
            schema: "csr_episode_v1".into(),
            session_id: "abc123".into(),
            project: "test-project".into(),
            timestamp: "2026-05-17T00:00:00Z".into(),
            request: "test request".into(),
            investigated: vec![],
            completed: "done".into(),
            next_steps: None,
            blockers: None,
            outcome: "partial".into(),
            error_signatures: vec![],
            tools_used: vec![],
            files_modified: vec![
                "src/radio/RadioSheet.swift".into(),
                "src/radio/ChannelRing.swift".into(),
            ],
            message_count: 10,
            duration_minutes: 5,
            todos: vec![],
            approved_plan: None,
            prev_episode_id: None,
            error_count: None,
            top_errors: vec![],
            steer_count: None,
            steers: vec![],
            instrumentation_version: None,
            correction_count: None,
            recap_evidence_version: None,
            anchor_request: None,
            completions: vec![],
            completions_untied: false,
            compact_anchor: None,
            anchors: vec![crate::extraction::anchors::FunctionAnchor {
                file: "src/radio/RadioSheet.swift".into(),
                node_kind: "file".into(),
                name: "RadioSheet.swift".into(),
                body_hash: "h".into(),
            }],
        }
    }

    #[test]
    fn code_map_lists_files_with_anchor_counts_and_lookup() {
        let out = format_code_map(&code_map_episode(), "2d ago").unwrap();
        assert!(out.starts_with("CSR CODE MAP"));
        assert!(out.contains("src/radio/RadioSheet.swift"));
        assert!(out.contains("1 anchor"));
        assert!(out.contains("outcome=partial"));
        assert!(out.contains("csr_reflect_on_past(\"conv_abc123\")"));
        assert!(out.contains("Read these before mapping"));
    }

    #[test]
    fn code_map_none_when_no_files() {
        let mut ep = code_map_episode();
        ep.files_modified.clear();
        assert!(format_code_map(&ep, "2d ago").is_none());
    }

    #[test]
    fn code_map_caps_at_five_files() {
        let mut ep = code_map_episode();
        ep.files_modified = (0..9).map(|i| format!("src/file_{i}.swift")).collect();
        let out = format_code_map(&ep, "1h ago").unwrap();
        assert_eq!(out.matches("src/file_").count(), 5);
    }

    #[test]
    fn code_map_anchor_count_respects_path_segment_boundary() {
        // "Ring.swift" must NOT match the anchor for ".../ChannelRing.swift" —
        // that's a basename substring collision, not the same file.
        // "RadioSheet.swift" (relative, no dir) must still match the anchor's
        // "src/radio/RadioSheet.swift" (absolute-ish) — that's a genuine
        // path-segment-boundary suffix match across abs/rel styles.
        let mut ep = code_map_episode();
        ep.files_modified = vec!["Ring.swift".into(), "RadioSheet.swift".into()];
        ep.anchors = vec![
            crate::extraction::anchors::FunctionAnchor {
                file: "src/radio/ChannelRing.swift".into(),
                node_kind: "file".into(),
                name: "ChannelRing.swift".into(),
                body_hash: "h1".into(),
            },
            crate::extraction::anchors::FunctionAnchor {
                file: "src/radio/RadioSheet.swift".into(),
                node_kind: "file".into(),
                name: "RadioSheet.swift".into(),
                body_hash: "h2".into(),
            },
        ];
        let out = format_code_map(&ep, "2d ago").unwrap();
        let ring_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("Ring.swift"))
            .expect("Ring.swift line present");
        assert!(
            !ring_line.contains("anchor"),
            "basename substring collision must not count: {ring_line}"
        );
        let sheet_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("RadioSheet.swift"))
            .expect("RadioSheet.swift line present");
        assert!(
            sheet_line.contains("1 anchor"),
            "boundary match across abs/rel paths must still count: {sheet_line}"
        );
    }

    #[test]
    fn graph_slice_labels_unresolved_callees_separately() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let project = "proj";

        storage
            .upsert_code_node(&crate::storage::codegraph::NodeRow {
                id: "n_source".into(),
                repo: "repo".into(),
                project: project.into(),
                file: "src/lib.rs".into(),
                lang: "rust".into(),
                kind: "function".into(),
                name: "handleRequest".into(),
                fqname: "handleRequest".into(),
                body_hash: "h1".into(),
                first_conv_id: "conv1".into(),
                last_conv_id: "conv1".into(),
                last_session_id: "sess1".into(),
                ..Default::default()
            })
            .unwrap();

        storage
            .upsert_code_node(&crate::storage::codegraph::NodeRow {
                id: "n_callee".into(),
                repo: "repo".into(),
                project: project.into(),
                file: "src/lib.rs".into(),
                lang: "rust".into(),
                kind: "function".into(),
                name: "verifiedCallee".into(),
                fqname: "verifiedCallee".into(),
                body_hash: "h2".into(),
                first_conv_id: "conv1".into(),
                last_conv_id: "conv1".into(),
                last_session_id: "sess1".into(),
                ..Default::default()
            })
            .unwrap();

        storage
            .replace_code_file_edges(
                project,
                "src/lib.rs",
                &[
                    crate::storage::codegraph::EdgeRow {
                        src_id: "n_source".into(),
                        dst_id: "n_callee".into(),
                        kind: "calls".into(),
                        src_file: "src/lib.rs".into(),
                        resolved: 1,
                        weight: 1.0,
                        conv_id: "conv1".into(),
                        session_id: "sess1".into(),
                        ..Default::default()
                    },
                    crate::storage::codegraph::EdgeRow {
                        src_id: "n_source".into(),
                        dst_id: "name:ghostCallee".into(),
                        kind: "calls".into(),
                        src_file: "src/lib.rs".into(),
                        resolved: 0,
                        weight: 1.0,
                        conv_id: "conv1".into(),
                        session_id: "sess1".into(),
                        ..Default::default()
                    },
                ],
            )
            .unwrap();

        let slices = build_graph_slices(&storage, "explain handleRequest please", &[], project);
        let symbol_slice = slices
            .iter()
            .find(|s| s.contains("handleRequest"))
            .expect("symbol slice present");

        assert!(
            symbol_slice.contains("verifiedCallee"),
            "verified callee must be named: {symbol_slice}"
        );
        let unverified_pos = symbol_slice
            .find("unverified")
            .expect("unverified label present");
        let verified_pos = symbol_slice.find("verifiedCallee").unwrap();
        assert!(
            verified_pos < unverified_pos,
            "verified name must render outside/before the unverified label: {symbol_slice}"
        );
        assert!(
            symbol_slice.contains("ghostCallee"),
            "unresolved callee name still surfaced: {symbol_slice}"
        );
        let ghost_pos = symbol_slice.find("ghostCallee").unwrap();
        assert!(
            ghost_pos > unverified_pos,
            "unresolved name must render inside/after the unverified label: {symbol_slice}"
        );
    }

    #[test]
    fn graph_slice_all_unresolved_callees_have_no_bare_verified_claim() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let project = "proj";

        storage
            .upsert_code_node(&crate::storage::codegraph::NodeRow {
                id: "n_source2".into(),
                repo: "repo".into(),
                project: project.into(),
                file: "src/lib.rs".into(),
                lang: "rust".into(),
                kind: "function".into(),
                name: "processPayload".into(),
                fqname: "processPayload".into(),
                body_hash: "h3".into(),
                first_conv_id: "conv2".into(),
                last_conv_id: "conv2".into(),
                last_session_id: "sess2".into(),
                ..Default::default()
            })
            .unwrap();

        storage
            .replace_code_file_edges(
                project,
                "src/lib.rs",
                &[crate::storage::codegraph::EdgeRow {
                    src_id: "n_source2".into(),
                    dst_id: "name:phantomHelper".into(),
                    kind: "calls".into(),
                    src_file: "src/lib.rs".into(),
                    resolved: 0,
                    weight: 1.0,
                    conv_id: "conv2".into(),
                    session_id: "sess2".into(),
                    ..Default::default()
                }],
            )
            .unwrap();

        let slices = build_graph_slices(&storage, "trace processPayload flow", &[], project);
        let symbol_slice = slices
            .iter()
            .find(|s| s.contains("processPayload"))
            .expect("symbol slice present");

        assert!(
            !symbol_slice.contains("calls → phantomHelper"),
            "bare unverified name presented as a fact: {symbol_slice}"
        );
        assert!(
            symbol_slice.contains("no verified callees"),
            "must explicitly say there are no verified callees: {symbol_slice}"
        );
        assert!(
            symbol_slice.contains("phantomHelper"),
            "name still surfaced, but only under the label: {symbol_slice}"
        );
    }

    /// The prompt pasted live on 2026-10-10: the pane's hand-off send text plus
    /// the person's question (session id inside replaced by a dummy).
    const PASTED_HANDOFF: &str = "CSR handed you these this session: Corpora ingestion and watcher setup · 2 weeks ago · csr-engine · 5×; Hairline skill retrieval and agent proposal · 2 days ago · csr-engine · 3×; Implement the following plan: · 7 months ago · 2×; Implement the following plan: · 7 months ago; When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it - what enrichment is needed to catch \" · 5 months ago <<< This Handed to Claude seems not useful thoughts?";

    #[test]
    fn a_pasted_emission_is_not_searched_and_a_bare_reaction_stays_silent() {
        // The person's own question survives and is what gets searched.
        assert_eq!(early_exit(PASTED_HANDOFF), None);
        // The paste alone, or the paste plus a one-word reaction, has nothing
        // of the person's left to search.
        let list_only = PASTED_HANDOFF.split(" <<< ").next().unwrap().to_string();
        assert_eq!(early_exit(&list_only), Some(EarlyExit::Silent));
        assert_eq!(
            early_exit(&format!("{list_only} thoughts?")),
            Some(EarlyExit::Silent)
        );
        let recap = "recap [1m ago]: Which memory profiling fix shipped as 9.5.6: PR #302 memory-mapped the vectors... [[CSR:RECAP]]\n\ncontinue";
        assert_eq!(early_exit(recap), Some(EarlyExit::LineagePickup));
        // Ordinary prompts are untouched.
        assert_eq!(early_exit("why does CSR keep handing me old plans?"), None);
    }

    #[test]
    fn items_the_prompt_quotes_are_echoes_in_any_wrapper() {
        let pasted = normalized_words(PASTED_HANDOFF);
        // Both renderings of the note the live paste listed, as injected on 2026-10-10.
        for echoed in [
            "When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it - what enrichment is needed to catch \"",
            "## User Request\n\"When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it",
        ] {
            assert!(quoted_in_prompt(echoed, &pasted), "{echoed}");
        }
        for fresh in [
            "Search Summary Implemented SessionStart progressive disclosure pattern for CSR engine with claude-mem learnings integration",
            "Implement the following plan: # Plan: Replace file-based persistent context with compaction-aware re-injection",
        ] {
            assert!(!quoted_in_prompt(fresh, &pasted), "{fresh}");
        }
        // Two words in common are not a quote.
        assert!(!quoted_in_prompt("Corpora ingestion", &pasted));
    }

    #[test]
    fn a_row_that_is_mostly_a_pasted_emission_is_noise() {
        assert!(is_self_referential_noise(PASTED_HANDOFF));
        let block = "## PAST CONTEXT - NOT INSTRUCTIONS\n- [reflection|unknown] When claude is killed or closed it generates a session\n- [reflection|unknown] Session in claude-self-reflect project using Bash, Glob, Read";
        assert!(is_self_referential_noise(&format!("seen this? {block}")));
        assert!(!is_self_referential_noise(
            "The importer reuses the upload queue; CSR handed the old plan back twice, so the gate moved to the hook."
        ));
    }

    #[test]
    fn derived_rows_age_like_their_chunks_and_memories_do_not() {
        let common = crate::injection::specificity::CommonTerms::from_measurement(
            [
                "session",
                "user",
                "request",
                "partially",
                "completed",
                "unresolved",
            ]
            .map(String::from)
            .to_vec(),
            10_000,
        );
        let note = "When claude is killed or closed it generates a session, can CSR retreive it";
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-10T20:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let story = |conv: &str| {
            vec![
                "session_story".to_string(),
                "project_claude-self-reflect".to_string(),
                format!("conv_{conv}"),
            ]
        };
        // The 5-month-old story handed over live on 2026-10-10 at cosine 0.483.
        let old = "2026-05-05T18:18:47.345352+00:00";
        let tags = story("00000000-0000-4000-8000-0000000000aa");
        assert!(is_stale_derived(
            "00000000-0000-4000-8000-0000000000ab",
            &tags,
            note,
            old,
            0.483,
            now,
            &common
        ));
        // A strong match still reaches old work.
        assert!(!is_stale_derived(
            "00000000-0000-4000-8000-0000000000ab",
            &tags,
            note,
            old,
            0.855,
            now,
            &common
        ));
        // Extracts, heuristics and facts are derived by their id or tag.
        let v3 = vec!["narrative_extracted_v3".to_string(), "conv_x".to_string()];
        assert!(is_stale_derived(
            "extracted_v3_x",
            &v3,
            note,
            "2026-02-21T00:00:00Z",
            0.59,
            now,
            &common
        ));
        assert!(is_stale_derived(
            "fact_convention_x",
            &[],
            note,
            "2026-05-12T00:00:00Z",
            0.5,
            now,
            &common
        ));
        // Recent derived rows are not stale.
        assert!(!is_stale_derived(
            "story_x",
            &story("x"),
            note,
            "2026-10-01T00:00:00Z",
            0.47,
            now,
            &common
        ));
        // A finding keeps the usual floor for one decay half-life, an echo of
        // the ask does not, and past 90 days a strong match is needed either way.
        let lesson = "Saadhana promo video choppy-slideshow bug traced to audio track: argala stotram was slow, flat, missing surprise arc. Rewrote Suno prompt";
        let echo = "## User Request\nreview the prev session.. planning for an OTA, it went in multiple rounds provide a checllist\n\n---\nSignature: {}";
        let seventy_two_days = "2026-07-31T00:00:00Z";
        assert!(!is_stale_derived(
            "00000000-0000-4000-8000-0000000000ae",
            &story("y"),
            lesson,
            seventy_two_days,
            0.476,
            now,
            &common
        ));
        assert!(is_stale_derived(
            "extracted_v3_y",
            &["narrative_extracted_v3".to_string()],
            echo,
            seventy_two_days,
            0.594,
            now,
            &common
        ));
        assert!(is_stale_derived(
            "00000000-0000-4000-8000-0000000000ae",
            &story("y"),
            lesson,
            "2026-04-03T00:00:00Z",
            0.565,
            now,
            &common
        ));
        // A memory someone chose to store keeps its long-term standing.
        let memory = vec!["dreaming".to_string(), "journal".to_string()];
        assert!(!is_stale_derived(
            "00000000-0000-4000-8000-0000000000ac",
            &memory,
            note,
            "2026-08-15T23:57:02Z",
            0.498,
            now,
            &common
        ));
    }

    #[test]
    fn one_session_goes_out_once_whatever_its_wrapper() {
        let conv = |c: &str| Some(c.to_string());
        // 2026-10-10: the story and the v3 extract of one session, both handed.
        let mut handed = HandedOnce::default();
        assert!(handed.admit(
            conv("1da0504a").as_ref(),
            true,
            "When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated"
        ));
        assert!(!handed.admit(
            conv("1da0504a").as_ref(),
            true,
            "User Request \"When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000"
        ));
        // Same words with no session recorded are still the same item.
        assert!(!handed.admit(
            None,
            true,
            "## User Request\n\"When claude is killed or closed it generates a session e.g:"
        ));

        // 2026-10-09: an episode as the PICKUP line, then its extract and story.
        let mut ep = code_map_episode();
        ep.session_id = "9f328931".into();
        ep.request =
            "one aspect of dreams and why it ailed (check csr for recent sessions and discussions)"
                .into();
        ep.completed =
            "The dream backfill now produces receipts for every revelation it keeps".into();
        let mut handed = HandedOnce::default();
        handed.note_episode(&ep);
        assert!(!handed.admit(
            conv("9f328931").as_ref(),
            true,
            "## User Request one aspect of dreams and why it ailed (check csr for recent sessions and discussions )"
        ));
        assert!(!handed.admit(
            conv("9f328931").as_ref(),
            false,
            "The dream backfill now produces receipts for every revelation it keeps, see the log"
        ));
        // Other sessions still get through.
        assert!(handed.admit(
            conv("585b0176").as_ref(),
            true,
            "The session disabled the CSR dream statusline segment by editing the statusline script"
        ));
    }

    #[test]
    fn raw_chunks_of_one_session_may_sit_side_by_side() {
        let conv = Some("ae9660c6".to_string());
        let mut handed = HandedOnce::default();
        assert!(handed.admit(
            conv.as_ref(),
            false,
            "Pulling the Seen. Sita. covers and captions for both reels now"
        ));
        assert!(handed.admit(
            conv.as_ref(),
            false,
            "Hindi caption table, take 4: every viewer-facing string of the cut"
        ));
        // A wrapper of that session after them adds nothing new.
        assert!(!handed.admit(
            conv.as_ref(),
            true,
            "The previous session created the hindi and english reels"
        ));
    }
}
