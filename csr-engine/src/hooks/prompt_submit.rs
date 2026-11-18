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

use super::prompt_stages::{self as stages, Stage};
use super::HookInput;
use crate::engine::Engine;
use crate::injection::anti_pattern;
use crate::injection::formatter;
use crate::injection::predictor::{self, RawResult};
use crate::injection::send_gate;
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

/// Chunk slots a prompt renders (`scored.iter().take(5)`). Keyword-only hits
/// are admitted only up to this many chunks in total, after the vector hits.
const KEYWORD_FILL_SLOTS: usize = 5;

/// A chunk whose characters are at least this share tool output (the cached
/// `chunks.tool_result_share`) is a dump of what a tool printed, not an
/// account of the work. Chunks are about 900 characters and a span is one
/// message, so the column is nearly always 0.0 or 1.0 (of the 175,000 chunks
/// since 2026-10-01 with a recorded floor, 160,000 sit at exactly 0 or 1 and
/// 947 between 0.2 and 0.5); 0.5 splits the two kinds without a tuned edge.
///
/// The trust floor does not: an assistant message takes the floor of the
/// context before it, so a plain report written after any web fetch or shell
/// output is labelled `external` at share 0.0. Hand-rated 40 `[chunk|external]`
/// items from the 7 days to 2026-10-22: of the 22 at share 0.5 or more, 19
/// were listings, file or code dumps, scraped pages and logs; of the 18 below
/// it, 13 were reports, decisions and receipts.
const TOOL_DUMP_SHARE: f64 = 0.5;

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
/// its session does not. Reviewer-labelled replay 2026-10-11 (15 prompts, grader
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

/// The injection text [`render`] would produce, with nothing committed: no
/// exposure row, no retrieval event, and any dream claim released. The
/// measurement tool `csr_hook_prompt` times the render through this.
pub async fn render_uncommitted(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<String> {
    Ok(prepare(input, engine, cwd).await?.text)
}

pub(super) struct PreparedPrompt {
    pub text: String,
    pub reason: Option<super::prompt_log::DecisionReason>,
    effects: PendingEffects,
}

impl PreparedPrompt {
    pub fn delivered(mut self) -> String {
        self.effects.commit_exposures();
        self.effects.commit_retrievals();
        self.text
    }

    /// Like [`delivered`](Self::delivered), calling `release` between the
    /// receipts that never wait and the ones that can. The daemon gives up its
    /// single-flight permit there: a retrieval event waits for the database
    /// writer as long as any connection does, and no later prompt may queue
    /// behind that.
    pub fn delivered_releasing(mut self, release: impl FnOnce()) -> String {
        self.effects.commit_exposures();
        release();
        self.effects.commit_retrievals();
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
    prepare_with_receipts(input, engine, cwd, engine.storage().clone()).await
}

/// [`prepare`] with the writes that must not be lost, retrieval events and the
/// release of a dream claim, going to `receipts` instead of the engine's own
/// storage. The daemon renders on a bounded-wait connection and passes its
/// long-wait one here.
pub(super) async fn prepare_with_receipts(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
    receipts: std::sync::Arc<crate::storage::Storage>,
) -> Result<PreparedPrompt> {
    prepare_with_receipts_mode(input, engine, cwd, receipts, false).await
}

/// [`prepare_with_receipts`] for a `query_only` serve-only daemon when
/// `serve_only` is set: the prompt-keyed exposure impression is the one write
/// the delivered answer may make (see `exposure::record_impression_serve_only`).
pub(super) async fn prepare_with_receipts_mode(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
    receipts: std::sync::Arc<crate::storage::Storage>,
    serve_only: bool,
) -> Result<PreparedPrompt> {
    let mut prepared = PreparedPrompt {
        text: String::new(),
        reason: None,
        effects: PendingEffects {
            reason: None,
            storage: engine.storage().clone(),
            receipts,
            impressions: Vec::new(),
            retrievals: Vec::new(),
            dream_claims: Vec::new(),
            withheld_claims: Vec::new(),
            prompt_id: input
                .prompt_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
            serve_only,
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
    prepared.reason = prepared.effects.reason;
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
    reason: Option<super::prompt_log::DecisionReason>,
    /// Where the render read. Exposure rows go here and are dropped, not
    /// waited for, when the database is busy.
    storage: std::sync::Arc<crate::storage::Storage>,
    /// Retrieval events and claim releases: written however long the writer
    /// takes, from a thread that holds no admission permit.
    receipts: std::sync::Arc<crate::storage::Storage>,
    impressions: Vec<PendingImpression>,
    retrievals: Vec<(String, String, String)>,
    dream_claims: Vec<(String, Option<String>)>,
    /// Claims the send gate took back; released on drop, even after an ack.
    withheld_claims: Vec<(String, Option<String>)>,
    /// The `prompt_id` of the prompt this render answers. Every exposure row
    /// this render commits is keyed by it, so the consumption audit can join
    /// an injection to its prompt.
    prompt_id: Option<String>,
    /// The daemon is `query_only`; only the keyed exposure write may land.
    serve_only: bool,
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

    /// Exposure rows never wait (`try_record_rerank_exposure`).
    fn commit_exposures(&mut self) {
        // The claims are now receipts and must survive this value's drop.
        self.dream_claims.clear();
        for item in self.impressions.drain(..) {
            let record = if self.serve_only {
                super::exposure::record_impression_serve_only
            } else {
                super::exposure::record_impression_keyed
            };
            record(
                &self.storage,
                item.session.as_deref(),
                self.prompt_id.as_deref(),
                &item.project,
                &item.surface,
                item.query.as_deref(),
                item.embedding.as_deref(),
                &item.intent,
                item.items,
            );
        }
    }

    fn commit_retrievals(&mut self) {
        for (id, source, session) in self.retrievals.drain(..) {
            let _ = self
                .receipts
                .log_retrieval_event(&id, &source, "prompt_submit", &session);
        }
    }

    /// Mark the dream claims from index `from` on as withheld: their lines will
    /// not go out, so they are given back when this value drops. Not now: the
    /// delete can wait on the writer, and the render still holds admission.
    /// An acknowledged abstain clears `dream_claims`, not these.
    fn withhold_dream_claims(&mut self, from: usize) {
        self.withheld_claims.extend(self.dream_claims.drain(from..));
    }

    fn release_dream_claims(&mut self) {
        let mut claims = std::mem::take(&mut self.dream_claims);
        claims.append(&mut self.withheld_claims);
        for (id, session) in claims {
            let _ = self.receipts.with_connection(|conn| {
                conn.execute("DELETE FROM dream_deliveries WHERE dream_id = ?1 AND channel = 'prompt' AND session_id IS ?2",
                    rusqlite::params![id, session])?;
                Ok(())
            });
        }
    }
}

impl Drop for PendingEffects {
    fn drop(&mut self) {
        // Only claims won by this render are listed. Keep the claim throughout
        // rendering to preserve cross-process dedupe, but release it if no full
        // response was acknowledged. On the daemon this runs on its blocking
        // render worker after the admission permit is released, on the
        // receipts connection.
        self.release_dream_claims();
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
    let source = input.source.as_deref();
    if is_machine_prompt(source, pasted) {
        effects.reason = Some(super::prompt_log::DecisionReason::MachinePrompt);
        return Ok(());
    }
    // Retrieval runs on the person's words only. A prompt that pastes CSR's
    // own output back (a pane hand-off list, the story, a recap, an injected
    // block) would otherwise find exactly the items it lists. `pasted` stays
    // the yardstick for "already in front of Claude" below.
    let asked = crate::extraction::provenance::strip_csr_emissions(pasted);
    let prompt = asked.as_str();
    match h_early_exit_for(source, pasted) {
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
                effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
            } else {
                effects.reason = Some(super::prompt_log::DecisionReason::NoCandidates);
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
        let _embed = stages::time(Stage::Embed);
        match tokio::task::spawn_blocking(move || emb.embed_single(&q)).await {
            Ok(Ok(v)) => v,
            _ => return Ok(()), // Can't embed → nothing to inject
        }
    };
    let probes = crate::hooks::intent::ProbeSet::load_or_build(embeddings).await;
    if std::env::var("CSR_DEBUG_CORRELATE").is_ok() {
        if let Some(probes) = &probes {
            for (i, s) in probes.scores(&query_vec) {
                eprintln!("CSR intent score: {i:?} {s:.3}");
            }
        }
    }
    let intent = probes.and_then(|p| p.classify(&query_vec).map(|(intent, _)| intent));
    handle_classified_intent(
        input,
        engine,
        cwd,
        output,
        effects,
        prompt,
        &pasted_words,
        &query_vec,
        intent,
    )
    .await
}

/// Route the classifier's result through the same return paths with fixed
/// vectors in tests, without loading a semantic model.
#[allow(clippy::too_many_arguments)]
async fn handle_classified_intent(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
    output: &mut String,
    effects: &mut PendingEffects,
    prompt: &str,
    pasted_words: &str,
    query_vec: &[f32],
    intent: Option<crate::hooks::intent::Intent>,
) -> Result<()> {
    let mut classified_intent = "other";
    let mut correlated_memo = None;
    if let Some(intent) = intent {
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
                        Some(query_vec),
                        classified_intent,
                        &ep,
                        None,
                    );
                    effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
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
                        Some(query_vec),
                        classified_intent,
                        &ep,
                        None,
                    );
                    effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
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
                let current_project =
                    crate::search::cross_project::resolve_project_from_cwd(&cwd.to_string_lossy())
                        .unwrap_or_default();
                let corr = correlate_episode(
                    engine,
                    query_vec,
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
                            Some(query_vec),
                            classified_intent,
                            ep,
                            None,
                        );
                        effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
                        return Ok(());
                    }
                }
                correlated_memo = Some(corr);
                // No correlated episode or no files — normal flow continues below.
            }
        }
    }

    handle_content(
        input,
        engine,
        cwd,
        output,
        effects,
        prompt,
        pasted_words,
        query_vec,
        classified_intent,
        correlated_memo,
    )
    .await
}

/// The content path after intent classification, also exercised with fixed
/// vectors in tests so telemetry checks do not depend on model predictions.
#[allow(clippy::too_many_arguments)]
async fn handle_content(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
    output: &mut String,
    effects: &mut PendingEffects,
    prompt: &str,
    pasted_words: &str,
    query_vec: &[f32],
    classified_intent: &str,
    correlated_memo: Option<Option<(crate::hooks::stop::Episode, String, f32)>>,
) -> Result<()> {
    // Intent already abstained; short prompts have no searchable content.
    if prompt.len() < MIN_PROMPT_LENGTH {
        effects.reason = Some(super::prompt_log::DecisionReason::TooShort);
        return Ok(());
    }
    // Where this render starts, so the send gate can take all of it back.
    let start = (
        output.len(),
        effects.impressions.len(),
        effects.dream_claims.len(),
    );
    let embeddings = engine.embeddings();
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
            effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
            effects.record_impression(
                input.session_id.as_deref(),
                &project,
                "prompt_submit",
                Some(prompt),
                Some(query_vec),
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
                effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
                effects.record_impression(
                    input.session_id.as_deref(),
                    &project,
                    "prompt_submit",
                    Some(prompt),
                    Some(query_vec),
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
                query_vec,
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
            .any(|ask| quoted_in_prompt(ask, pasted_words))
    });
    // What the PICKUP line already says, so PAST CONTEXT does not say it again.
    let mut handed = HandedOnce::default();
    if let Some((ep, age, score)) = correlated {
        output.push_str(&format_semantic_pickup(&ep, &age, score));
        effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
        preface_exposures.push(episode_exposure_item(&ep, Some(f64::from(score))));
        handed.note_episode(&ep);
    }

    // 2. Search chunks (past conversations) — scoped to current project
    // Over-fetch to compensate for project filtering (Codex M-1 fix)
    let mut evidence = SearchEvidence::default();
    let chunk_results = search_chunks_with_vec(
        engine,
        prompt,
        query_vec,
        15,
        0.55,
        &current_project,
        input.session_id.as_deref(),
        cwd,
        &mut evidence,
    )
    .await;

    // 3. Search reflections (stored insights) — scoped to current project
    let reflection_results = search_reflections_with_vec(
        engine,
        query_vec,
        10,
        0.45,
        &current_project,
        input.session_id.as_deref(),
        &mut evidence,
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
    let _compose = stages::time(Stage::Compose);
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
            evidence.other_filter = true;
            continue;
        }

        // Content-based dedup (Bug 9): skip near-duplicate content
        // Use 200-char prefix to avoid false dedup on items sharing common preambles (F5 fix)
        let prefix: String = result.content.chars().take(200).collect();
        if !seen_prefixes.insert(prefix) {
            evidence.other_filter = true;
            continue;
        }

        // Self-referential noise filter (Bug 4/5): skip content about CSR internals
        if is_self_referential_noise(&result.content) {
            evidence.lookalike = true;
            continue;
        }

        // The person pasted or quoted this item, so it is already in front of
        // Claude: handing it back is the self-echo, not memory.
        if quoted_in_prompt(&result.content, pasted_words) {
            evidence.other_filter = true;
            continue;
        }

        // One session, one wrapper: the same work as a PICKUP episode, a
        // story, a v3 extract and a chunk quoting its last message is one item.
        let conversation = result
            .memory_id
            .as_ref()
            .and_then(|id| source_conversation.get(id));
        if !handed.admit(conversation, result.source != "chunk", &result.content) {
            evidence.other_filter = true;
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

    // Send gate: a block goes out whole or not at all (`injection::send_gate`).
    let mut formatted = (!ctx.is_empty()).then(|| ctx.format_with_receipt(PROMPT_TOKEN_BUDGET));
    let rendered = formatted.as_ref().map_or(0, send_gate::rendered_items);
    let sends_something =
        output.len() > start.0 || formatted.as_ref().is_some_and(|f| !f.text.is_empty());
    if sends_something && send_gate::enabled() {
        let top = formatted
            .as_ref()
            .and_then(|f| send_gate::top_rendered_score(&ctx.winning_strategies, f));
        let reason = match send_gate::decide(top, rendered) {
            send_gate::Gate::Send => None,
            // Nothing ranked survived the filters: they name why.
            _ if rendered == 0 => Some(evidence.abstention()),
            send_gate::Gate::BelowFloor => Some(super::prompt_log::DecisionReason::BelowFloor),
            send_gate::Gate::ThinPool => Some(super::prompt_log::DecisionReason::ThinPool),
        };
        if let Some(reason) = reason {
            output.truncate(start.0);
            effects.impressions.truncate(start.1);
            effects.withhold_dream_claims(start.2);
            effects.reason = Some(reason);
            // Nothing was shown, so nothing is exposed: the empty-render exit below.
            preface_exposures.clear();
            formatted = None;
        }
    }

    let Some(formatted) = formatted else {
        if effects.reason.is_none() {
            effects.reason = Some(evidence.abstention());
        }
        record_prompt_exposure(
            input,
            effects,
            cwd,
            prompt,
            query_vec,
            classified_intent,
            preface_exposures,
        );
        return Ok(());
    };

    if !formatted.text.is_empty() {
        output.push_str(&formatted.text);
        effects.reason = Some(super::prompt_log::DecisionReason::OnTopic);
    } else if effects.reason.is_none() {
        effects.reason = Some(super::prompt_log::DecisionReason::NoCandidates);
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
        query_vec,
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
    super::prompt_log::log_injection_detail(
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

    // A slash command or skill: UserPromptSubmit fires before it expands
    // (Claude Code 2.1.299), so the expansion carries its own context and
    // nothing is injected. Decided on the raw prompt, before any paste
    // stripping or continuation matching.
    if prompt.trim_start().starts_with('/') {
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

/// What `UserPromptSubmit.source` says about who wrote the turn.
///
/// Claude Code 2.1.311 sends one of `user` (typed, queued or an accepted
/// suggestion), `sdk` (a non-interactive caller), `system` (a meta turn) or
/// `loop_wakeup` / `schedule_wakeup` (a scheduled wake-up). `Some(true)` is
/// any value that is not a person typing; `Some(false)` is a typed turn;
/// `None` is an absent or blank field, where the caller falls back to the
/// text heuristic [`is_harness_turn`].
pub(super) fn harness_source(source: Option<&str>) -> Option<bool> {
    let source = source?.trim();
    if source.is_empty() {
        return None;
    }
    // `black_box` keeps the list as data: without it the compiler folds the
    // five comparisons into register compares and the values leave the binary.
    let typed = std::hint::black_box(["user", "prompt", "typed", "queued", "suggestion_accepted"])
        .iter()
        .any(|value| source.eq_ignore_ascii_case(value));
    Some(!typed)
}

/// The release source classification used only by the new veto. Unknown
/// sources are admitted here; H's own source gate runs afterward.
fn release_harness_source(source: Option<&str>) -> Option<bool> {
    let source = source?.trim();
    let is = |values: &[&str]| values.iter().any(|v| source.eq_ignore_ascii_case(v));
    if is(std::hint::black_box(&[
        "user",
        "prompt",
        "typed",
        "queued",
        "suggestion_accepted",
    ])) {
        Some(false)
    } else if is(std::hint::black_box(&[
        "system",
        "loop_wakeup",
        "schedule_wakeup",
        "scheduled",
        "task_notification",
        "peer",
    ])) {
        Some(true)
    } else {
        None
    }
}

const MACHINE_PROMPT_OPENINGS: [&str; 6] = [
    "<cross-session-message",
    "<agent-message",
    "[Cross-session idle notice]",
    "<task-notification",
    "[SYSTEM NOTIFICATION",
    "<system-reminder",
];

const MACHINE_NOTICE_HEADINGS: [(&str, &[&str]); 3] = [
    (
        "Another Claude session sent a message:\n",
        &["<cross-session-message", "<agent-message"],
    ),
    (
        "Another Claude session sent a message while you were working:\n",
        &["<cross-session-message", "<agent-message"],
    ),
    (
        "The coordinator sent a message while you were working:\n",
        &[],
    ),
];

fn opens_as_notice(text: &str) -> bool {
    let opening = text.trim_start();
    MACHINE_PROMPT_OPENINGS
        .iter()
        .any(|marker| opening.starts_with(marker))
        || MACHINE_NOTICE_HEADINGS.iter().any(|(heading, tags)| {
            opening.strip_prefix(heading).is_some_and(|body| {
                let body = body.trim_start();
                if tags.is_empty() {
                    !body.is_empty()
                } else {
                    tags.iter().any(|tag| body.starts_with(tag))
                }
            })
        })
}

/// The release machine veto, before H handles any admitted input. A forwarded
/// note with a human tail passes through in full under the H query contract.
pub(crate) fn is_machine_prompt(source: Option<&str>, prompt: &str) -> bool {
    if release_harness_source(source) == Some(true) || is_harness_turn(prompt) {
        return true;
    }
    let opening = prompt.trim_start();
    if opens_as_notice(opening) {
        return true;
    }
    if !opening.starts_with(crate::extraction::provenance::SIDE_AGENT_NOTE_PREFIX) {
        return false;
    }
    // The opener without the note grammar after it is still not a person.
    crate::extraction::provenance::strip_forwarded_note(opening)
        .is_none_or(|tail| tail.trim().is_empty())
}

/// Apply the release machine veto before the shipped H route decision.
pub(super) fn early_exit_for(source: Option<&str>, prompt: &str) -> Option<EarlyExit> {
    if is_machine_prompt(source, prompt) {
        return Some(EarlyExit::Silent);
    }
    h_early_exit_for(source, prompt)
}

/// [`early_exit`] with the hook's `source` applied first. A harness source is
/// silent whatever the text says; a typed or absent source still goes through
/// the text rules, so the heuristic stays the fallback and a typed turn can
/// only gain silence from it, never lose it.
fn h_early_exit_for(source: Option<&str>, prompt: &str) -> Option<EarlyExit> {
    if harness_source(source) == Some(true) {
        return Some(EarlyExit::Silent);
    }
    early_exit(prompt)
}

/// Whether this prompt is answered without the vector index, so the hook
/// process can skip loading it (see `hooks::hook_needs_index`).
pub(crate) fn exits_before_search(prompt: Option<&str>) -> bool {
    early_exit_for(None, prompt.unwrap_or_default()).is_some()
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
        let wait = stages::time(Stage::IndexWait);
        let idx = engine.search().read().await;
        drop(wait);
        let _search = stages::time(Stage::Search);
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

#[path = "prompt_submit_search.rs"]
mod search_support;
use search_support::*;
pub(crate) use search_support::{build_graph_slices, format_code_map};

/// English words of six letters or more that code also uses as names. A prompt
/// that says one of them ("the current take", "the exact recipe") names no code.
/// Lowercase; compared case-insensitively. Seeded from the run-10 blocks
/// (`current`, `remove`, `recipe`, `machine`, `source`, `publish`).
const COMMON_WORDS: &[&str] = &[
    "action", "always", "before", "better", "branch", "buffer", "button", "change", "client",
    "commit", "common", "config", "create", "current", "delete", "ensure", "filter", "follow",
    "format", "handle", "insert", "latest", "little", "machine", "number", "object", "output",
    "people", "please", "prompt", "public", "publish", "really", "recipe", "record", "remove",
    "render", "report", "result", "return", "review", "search", "select", "server", "should",
    "simple", "source", "status", "string", "target", "update", "values",
];

/// Whether `name` reads as code on its own: it has `_`, `.`, `:`, a digit or
/// a camelCase hump (`validate_token`, `SUNO-OUT.md`, `A::new`, `handleRequest`).
pub(crate) fn is_identifier_shaped(name: &str) -> bool {
    name.contains(['_', '.', ':'])
        || name.chars().any(|c| c.is_ascii_digit())
        || name
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
}

/// Whether `word` is on [`COMMON_WORDS`].
pub(crate) fn is_common_word(word: &str) -> bool {
    COMMON_WORDS.contains(&word.to_ascii_lowercase().as_str())
}

/// Whether `word` stands in `text` as a whole word, not inside a longer one.
fn contains_whole_word(text: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        !text[..i].chars().next_back().is_some_and(is_word)
            && !text[i + word.len()..].chars().next().is_some_and(is_word)
    })
}

/// Match prompt text against anchored symbol names.
/// Names shorter than 6 chars are skipped (too collision-prone: main, init, run).
/// A plain-word name (`machine` of `JudgeTests::machine`) must stand as a whole
/// word and not be a common English one; an identifier-shaped name matches
/// anywhere, as before.
pub fn symbol_overlap(
    prompt: &str,
    anchors: &[(String, String)], // (session_id, name) — name may be qualified (A::new)
) -> Option<(String, String)> {
    anchors
        .iter()
        .find(|(_, name)| {
            // Match the bare segment: prompts say "validate_token", not "Auth::validate_token"
            let bare = name.rsplit("::").next().unwrap_or(name);
            if bare.len() < 6 {
                return false;
            }
            // The qualified name written out (`Cache::update`, `Cache.update`)
            // names the symbol even when its last segment is a common word.
            if name.contains("::")
                && (contains_whole_word(prompt, name)
                    || contains_whole_word(prompt, &name.replace("::", ".")))
            {
                return true;
            }
            if is_identifier_shaped(bare) {
                prompt.contains(bare)
            } else {
                !is_common_word(bare) && contains_whole_word(prompt, bare)
            }
        })
        .cloned()
}

#[cfg(test)]
#[path = "prompt_submit_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "prompt_submit_h_tests.rs"]
mod machine_veto_tests;
