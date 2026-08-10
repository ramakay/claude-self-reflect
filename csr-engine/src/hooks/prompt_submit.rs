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
use std::path::Path;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;
use crate::injection::anti_pattern;
use crate::injection::formatter;
use crate::injection::predictor::{self, RawResult};
use crate::injection::{InjectionContext, InjectionItem};
use crate::search::cross_project::resolve_project_from_cwd;
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

/// Handle the prompt-submit hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block Claude Code.
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    if let Err(e) = handle_inner(input, engine, cwd).await {
        eprintln!("CSR: prompt-submit hook error (non-fatal): {}", e);
    }

    // Chunk the active transcript after injection is printed to stdout.
    // Incremental: mtime check makes this a no-op when nothing changed (~0ms).
    // When new content exists: ~30-50ms for 1-2 chunks (well under perceptible lag).
    // Content becomes searchable by the next prompt submit.
    super::import_current_transcript(input, engine, cwd).await;

    Ok(()) // Always succeed
}

/// Maximum age (in minutes) to apply continuity boost.
const CONTINUITY_THRESHOLD_MINUTES: i64 = 2880;

async fn handle_inner(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Extract prompt from input
    let prompt = match input.prompt.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => return Ok(()), // No prompt → silent exit
    };

    // Route A pickup: "continue"-class prompts are shorter than
    // MIN_PROMPT_LENGTH and carry zero searchable signal — yet they are the
    // one case where memory IS the task. No content to correlate, so recency
    // picks the episode. Emit it imperatively, adjacent to the prompt (the
    // position the model reliably reads), instead of silently skipping. Live
    // failure 2026-07-07: the previous session advertised "just say continue"
    // and this hook dropped that exact prompt.
    if is_continuation_prompt(prompt) {
        if let Some((ep, age)) = pick_lineage_episode(engine, cwd, prompt) {
            emit_pickup(
                &ep,
                &age,
                "the user asked to continue; the episode below is the work being resumed.",
            );
        }
        return Ok(());
    }

    // Fast-path: skip slash commands
    if prompt.starts_with('/') {
        return Ok(());
    }

    // Bare acknowledgments ("ok", "y") carry no intent signal worth an
    // embedding pass.
    if prompt.trim().len() < 3 {
        return Ok(());
    }

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
                    if let Some((ep, age)) = pick_lineage_episode(engine, cwd, prompt) {
                        emit_pickup(
                            &ep,
                            &age,
                            "the user asked to continue; the episode below is the work being resumed.",
                        );
                        return Ok(());
                    }
                }
                crate::hooks::intent::Intent::StateRecall => {
                    if let Some((ep, age)) = pick_lineage_episode(engine, cwd, prompt) {
                        emit_pickup(
                            &ep,
                            &age,
                            "the prompt asks for the state of recent work; the episode below is the most recent session (picked by recency, not similarity).",
                        );
                        return Ok(());
                    }
                }
                crate::hooks::intent::Intent::Explore => {
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
                            println!("{}", map);
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

    // Symbol-overlap gate: prompt names an anchored function → one-line pointer
    if let Some(project) = resolve_project_from_cwd(&cwd.to_string_lossy()) {
        if let Ok(rows) = engine.storage().get_project_anchors(&project, 200) {
            let names: Vec<(String, String)> =
                rows.into_iter().map(|(sid, a)| (sid, a.name)).collect();
            if let Some((sid, name)) = symbol_overlap(prompt, &names) {
                println!(
                    "CSR: `{}` was modified in a recent session — episode: csr_reflect_on_past(\"conv_{}\")",
                    name, sid
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
    let anti_patterns =
        anti_pattern::find_anti_patterns(storage, embeddings, search, prompt, 0.5, 2).await;

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
    if let Some((ep, age, score)) = correlated {
        print!("{}", format_semantic_pickup(&ep, &age, score));
    }

    // 2. Search chunks (past conversations) — scoped to current project
    // Over-fetch to compensate for project filtering (Codex M-1 fix)
    let chunk_results =
        search_chunks_with_vec(engine, &query_vec, 15, 0.55, &current_project).await;

    // 3. Search reflections (stored insights) — scoped to current project
    let reflection_results =
        search_reflections_with_vec(engine, &query_vec, 10, 0.45, &current_project).await;

    // 4. Combine and score results (with continuity boost for recent session)
    let current_files: Vec<String> = extract_file_paths_from_prompt(prompt);
    let current_errors: Vec<String> = extract_error_patterns_from_prompt(prompt);

    let mut raw_results: Vec<RawResult> = Vec::new();
    raw_results.extend(chunk_results);
    raw_results.extend(reflection_results);

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
    for slice in build_graph_slices(storage, prompt, &current_files, &current_project) {
        review_items.push(InjectionItem {
            content: slice,
            score: 0.88,
            source: "code_graph".into(),
        });
    }

    // 6. Build InjectionContext
    let mut ctx = InjectionContext {
        anti_patterns,
        relevant_context: review_items,
        ..Default::default()
    };

    // Content-based dedup: skip items whose first 200 chars overlap with already-seen items
    let mut seen_prefixes: HashSet<String> = HashSet::new();
    // Track which memory IDs were actually injected (for TAD logging accuracy)
    let mut injected_memory_ids: Vec<(String, String)> = Vec::new(); // (id, source)

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

        // Track this item as actually injected
        if let Some(ref id) = result.memory_id {
            injected_memory_ids.push((id.clone(), result.source.clone()));
        }

        let item = InjectionItem {
            content: result.content.clone(),
            score: result.final_score,
            source: result.source.clone(),
        };

        ctx.winning_strategies.push(item);
    }

    if ctx.is_empty() {
        return Ok(());
    }

    let formatted = ctx.format(PROMPT_TOKEN_BUDGET);
    if !formatted.is_empty() {
        // stdout injection: Claude Code prepends this to the system prompt.
        // Uses print! (not println!) to avoid double-newline — formatter output ends with \n.
        print!("{}", formatted);
    }

    // TAD: Log retrieval events only for actually-injected items (not filtered-out ones)
    if let Some(ref session_id) = input.session_id {
        for (memory_id, source) in &injected_memory_ids {
            let _ = engine.storage().log_retrieval_event(
                memory_id,
                source,
                "prompt_submit",
                session_id,
            );
        }
    }

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
        formatted.len(),
        &scored,
    );

    Ok(())
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

/// Emit the CSR PICKUP block: reason line, Tier-0 episode state, resume
/// imperative. Used only by Route A lineage pickup (is_continuation_prompt /
/// Continue / StateRecall) — the CONTINUUM banner is reserved for the active
/// conversation lineage, never for a Route-B topic match.
fn emit_pickup(ep: &crate::hooks::stop::Episode, age: &str, reason: &str) {
    println!("CSR PICKUP — {}", reason);
    print!(
        "{}",
        crate::hooks::session_start::format_tier0_block(ep, &[], age)
    );
    println!("Resume from LAST/NEXT above; run the Full state lookup before re-deriving anything.");
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
        .find(|(ep, _)| !is_pickup_echo(prompt, &ep.request))
}

/// Semantic-pickup rendering for a topically-matched-but-not-lineage
/// episode (Route B correlation). Deliberately NOT the Tier-0 CONTINUUM
/// block (format_tier0_block) — that banner is reserved for the active
/// conversation lineage (Route A / SessionStart), so a topic match can
/// never be mistaken for "this is the session you're resuming". Renders
/// as a PICKUP reason line + a single EPISODE-INDEX-style line.
fn format_semantic_pickup(ep: &crate::hooks::stop::Episode, age: &str, score: f32) -> String {
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
/// eligibility still gates on the RAW score so old work stays resumable.
const EPISODE_RECENCY_HALF_LIFE_DAYS: f32 = 7.0;

/// Recency-weighted ranking score. Raw score decides *whether* a candidate
/// may anchor (EPISODE_CORRELATION_MIN); this decides *which* one wins.
fn effective_episode_score(raw: f32, age_days: f32) -> f32 {
    raw * 0.5_f32.powf(age_days.max(0.0) / EPISODE_RECENCY_HALF_LIFE_DAYS)
}

/// Pick the winning candidate from (raw_score, age_days) pairs: highest
/// effective (recency-weighted) score among those whose raw score clears
/// EPISODE_CORRELATION_MIN. Returns the index into `candidates`.
fn pick_episode(candidates: &[(f32, f32)]) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, (raw, _))| *raw >= EPISODE_CORRELATION_MIN)
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
    let project_tag = format!("project_{}", project);
    let now = chrono::Utc::now();
    let mut seen_convs: HashSet<String> = HashSet::new();
    // Collect every eligible candidate, then rank recency-weighted — raw
    // score order alone anchored a 3-week-old prompt-echo over the session
    // that ended two hours earlier (see EPISODE_RECENCY_HALF_LIFE_DAYS).
    let mut eligible: Vec<(f32, f32, crate::hooks::stop::Episode, String)> = Vec::new();
    let debug = std::env::var("CSR_DEBUG_CORRELATE").is_ok();
    for r in &results {
        let Ok(Some((_, tags, _))) = engine.storage().get_reflection_by_id(&r.id) else {
            continue;
        };
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
        if Some(conv_id) == current_session || !seen_convs.insert(conv_id.to_string()) {
            continue;
        }
        let Some((ep, ts)) = episode_for_conversation(engine, conv_id) else {
            continue;
        };
        if !crate::hooks::session_start::episode_carries_state(&ep) {
            continue;
        }
        // Skip episodes whose stored request is a near-verbatim echo of the
        // current prompt — that episode was itself just asking the same
        // question, not state to resume as a pickup.
        if is_pickup_echo(prompt, &ep.request) {
            continue;
        }
        // Unparseable timestamps rank as infinitely old (effective score 0)
        // but stay raw-gate eligible, so they can still win when alone.
        let age_days = crate::temporal::parse_timestamp(&ts)
            .map(|t| ((now - t).num_minutes() as f32 / 1440.0).max(0.0))
            .unwrap_or(f32::INFINITY);
        let age = crate::hooks::session_start::relative_time_label(&ts, &now);
        if std::env::var("CSR_DEBUG_CORRELATE").is_ok() {
            eprintln!(
                "CSR correlate candidate: raw={:.3} age_days={:.2} conv={}",
                r.score, age_days, ep.session_id
            );
        }
        eligible.push((r.score, age_days, ep, age));
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

/// Load the stored episode for a conversation via its `conv_<id>` tag.
/// Returns the episode plus its reflection timestamp.
fn episode_for_conversation(
    engine: &Engine,
    conv_id: &str,
) -> Option<(crate::hooks::stop::Episode, String)> {
    let conv_tag = format!("conv_{}", conv_id);
    let rows = engine
        .storage()
        .get_reflections_by_tag(&conv_tag, 10)
        .ok()?;
    rows.into_iter()
        .filter(|(_, _, tags, _)| tags.iter().any(|t| t == "session_episode"))
        .find_map(|(_, content, _, ts)| {
            serde_json::from_str::<crate::hooks::stop::Episode>(&content)
                .ok()
                .map(|ep| (ep, ts))
        })
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
    let session = engine.storage().get_most_recent_session(&project).ok()??;

    let ts = temporal::parse_timestamp(&session.timestamp)?;
    let age_minutes = (chrono::Utc::now() - ts).num_minutes();
    // C-2 fix: reject future timestamps (clock skew) and sessions beyond threshold
    if !(0..=CONTINUITY_THRESHOLD_MINUTES).contains(&age_minutes) {
        return None;
    }

    Some(session.conversation_id)
}

/// Search chunks using a pre-computed embedding vector (P-1 optimization).
/// Scoped to `project` — chunks from other projects are filtered out.
async fn search_chunks_with_vec(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
) -> Vec<RawResult> {
    let search = engine.search();
    let storage = engine.storage();

    let results = {
        let idx = search.read().await;
        idx.search_chunks(query_vec, limit, min_score)
    };

    let now = chrono::Utc::now();
    let mut raw_results = Vec::new();
    for result in &results {
        if let Ok(chunks) = storage.get_chunks_by_ids(std::slice::from_ref(&result.id)) {
            if let Some(chunk) = chunks.into_iter().next() {
                // Project scope filter: skip chunks from other projects
                if !project.is_empty() && chunk.project_name != project {
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
                });
            }
        }
    }

    raw_results
}

/// Search reflections using a pre-computed embedding vector (P-1 optimization).
/// Scoped to `project` — reflections tagged for other projects are filtered out.
/// Legacy reflections without project tags are allowed through.
async fn search_reflections_with_vec(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
) -> Vec<RawResult> {
    let search = engine.search();
    let storage = engine.storage();

    let results = {
        let idx = search.read().await;
        idx.search_reflections(query_vec, limit, min_score)
    };

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

            // Episode reflections are structured JSON surfaced via the Tier-0
            // CONTINUUM / pickup paths — never dump the raw blob as a context line.
            if tags.iter().any(|t| t == "session_episode") {
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
            raw_results.push(RawResult {
                content: formatter::truncate_item(&content, 300),
                score: result.score,
                source: source.to_string(),
                timestamp: Some(timestamp),
                files: extract_file_paths(&content),
                error_patterns: extract_error_patterns(&content),
                tags,
                conversation_id: None,
                memory_id: Some(result.id.clone()),
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
                slices.push(formatter::truncate_item(&line, 280));
            }
        }
    }

    // Symbol-anchored slice: callees of a symbol named in the prompt.
    for word in prompt.split_whitespace() {
        let token: String = word
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if token.len() < 6 {
            continue;
        }
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
        slices.push(formatter::truncate_item(&line, 280));
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

/// Check if content is self-referential noise about CSR internals.
/// Prevents the tool's own development history from polluting its output (Bug 4/5).
/// CSR's own emitted blocks (probe reports, CONTINUUM/briefing echoes) are
/// detected structurally via `extraction::provenance`; the pattern list below
/// covers dev-history vocabulary that isn't an emission format.
fn is_self_referential_noise(content: &str) -> bool {
    if crate::extraction::provenance::is_csr_emission(content) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Old work must stay resumable — recency weights the ordering, it
        // does not gate eligibility.
        assert_eq!(pick_episode(&[(0.56, 21.0)]), Some(0));
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
}
