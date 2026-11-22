use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::format::{self, DisplayRankScore, EnrichedResult};
use crate::search::assertions_partition;
use crate::search::cross_encoder::{self, RetrievalPolicy, CE_POOL};
use crate::search::cross_project;
use crate::search::decay;
use crate::search::SearchEngine;
use crate::storage::chunk_binding::ChunkWitnessVerdict;
use crate::storage::recap_feeds::{dream_consumption_mode, ConsumptionMode};
use crate::storage::witness_verdicts::VerdictChannel;
use crate::storage::Storage;
use crate::temporal;

use super::tool_guards as guards;

/// Demote-channel memories age three times faster when active forgetting is
/// enabled. This models synaptic downscaling: disproven code memories weaken
/// through the existing decay mechanism rather than being deleted. The T4
/// evidence gate limits the multiplier to symbols proven gone at HEAD;
/// Annotate-channel evolution remains rank-neutral.
const ACTIVE_FORGETTING_DECAY_FACTOR: f64 = 3.0;

/// In all-project searches, gently favor the current repository family so
/// local context wins close ties without hiding genuinely stronger memories.
const CURRENT_PROJECT_ALL_SCOPE_BOOST: f32 = 1.15;

struct SearchProjectScope {
    effective_project: Option<String>,
    scope_label: String,
    current_project_for_all_scope: Option<String>,
    family_anchor: Option<String>,
    family_projects: HashSet<String>,
    projects_root_override: Option<PathBuf>,
}

impl SearchProjectScope {
    /// `current_project` (from `cross_project::resolve_current_project`,
    /// `MCP_CLIENT_CWD` only) feeds *only* the `project="all"` family boost —
    /// changing its source would silently change ranking for every explicit
    /// `project="all"` call, which is out of scope here. `default_scope_project`
    /// (from `cross_project::resolve_client_dir` + `resolve_project_from_cwd`,
    /// `MCP_CLIENT_CWD` then `CLAUDE_PROJECT_DIR`) is what an *omitted* project
    /// resolves to — see module docs on why `MCP_CLIENT_CWD` alone left every
    /// Claude Code stdio search unscoped.
    fn resolve_with(
        storage: &Storage,
        requested_project: Option<&str>,
        current_project: Option<&str>,
        default_scope_project: Option<&str>,
        projects_root_override: Option<&Path>,
    ) -> Result<Self> {
        let (effective_project, scope_label, current_project_for_all_scope) =
            match requested_project {
                Some(project) if project.eq_ignore_ascii_case("all") => {
                    (None, "all".to_string(), current_project.map(str::to_string))
                }
                Some(project) if !project.is_empty() => {
                    (Some(project.to_string()), project.to_string(), None)
                }
                _ => match default_scope_project {
                    Some(project) => (Some(project.to_string()), project.to_string(), None),
                    None => (None, "all".to_string(), None),
                },
            };
        let family_anchor = effective_project
            .clone()
            .or_else(|| current_project_for_all_scope.clone());
        let mut family_projects = HashSet::new();
        if let Some(anchor) = family_anchor.as_deref() {
            family_projects.extend(
                storage
                    .list_project_names("", i64::MAX as usize)?
                    .into_iter()
                    .filter(|candidate| match projects_root_override {
                        Some(root) => {
                            cross_project::same_project_family_at_root(anchor, candidate, root)
                        }
                        None => cross_project::same_project_family(anchor, candidate),
                    }),
            );
            family_projects.insert(anchor.to_string());
        }
        Ok(Self {
            effective_project,
            scope_label,
            current_project_for_all_scope,
            family_anchor,
            family_projects,
            projects_root_override: projects_root_override.map(Path::to_path_buf),
        })
    }

    fn is_family(&self, candidate: &str) -> bool {
        self.family_anchor.as_deref().is_some_and(|anchor| {
            if let Some(root) = self.projects_root_override.as_deref() {
                cross_project::same_project_family_at_root(anchor, candidate, root)
            } else {
                cross_project::same_project_family(anchor, candidate)
            }
        })
    }
}

/// Resolve a [`SearchProjectScope`] from the process environment: the single
/// call point `reflect_on_past_with_vec_mode` and
/// `get_more_results_with_vec_active` both use, so the two can't drift apart
/// the way they did when each called `cross_project::resolve_default_scope_project()`
/// independently (W3-S finding 3).
fn resolve_search_scope(storage: &Storage, project: Option<&str>) -> Result<SearchProjectScope> {
    resolve_search_scope_with(
        storage,
        project,
        cross_project::resolve_current_project().as_deref(),
        cross_project::resolve_client_dir().as_deref(),
        None,
    )
}

/// [`resolve_search_scope`] with the environment-derived values (and the
/// projects root) passed in explicitly — the seam a test uses to pin the
/// wiring without env var mutation/races or dependence on this machine's
/// real `$HOME/projects` layout.
fn resolve_search_scope_with(
    storage: &Storage,
    project: Option<&str>,
    current_project: Option<&str>,
    client_dir: Option<&str>,
    projects_root_override: Option<&Path>,
) -> Result<SearchProjectScope> {
    let known_projects = storage.list_project_names("", i64::MAX as usize)?;
    let default_scope_project = client_dir.and_then(|dir| match projects_root_override {
        Some(root) => cross_project::resolve_verified_default_scope_project_at_root(
            dir,
            &known_projects,
            root,
        ),
        None => cross_project::resolve_verified_default_scope_project(dir, &known_projects),
    });
    SearchProjectScope::resolve_with(
        storage,
        project,
        current_project,
        default_scope_project.as_deref(),
        projects_root_override,
    )
}

fn project_scope_multiplier(candidate: &str, scope: &SearchProjectScope) -> f32 {
    if scope.effective_project.is_some() {
        if scope.is_family(candidate) {
            1.0
        } else {
            0.3
        }
    } else if scope.current_project_for_all_scope.is_some() && scope.is_family(candidate) {
        CURRENT_PROJECT_ALL_SCOPE_BOOST
    } else {
        1.0
    }
}

/// Chunk ids for every project in `project`'s family (itself plus any
/// `<project>-<suffix>` alias directory, see `cross_project::same_project_family`).
///
/// `csr_reflect_on_past`/`csr_get_more` widen an explicit-or-defaulted project
/// scope to the family via [`SearchProjectScope`]; `csr_search_insights` and
/// `csr_search_by_concept` used to filter on an exact label match instead, so
/// a family alias (e.g. `claude-self-reflect-csr-engine` under project
/// `claude-self-reflect`) silently lost its chunks in those two tools while
/// keeping them in `csr_reflect_on_past`. `csr_quick_check` already widened
/// this way inline; this is that same logic, shared.
fn family_chunk_ids(storage: &Storage, project: &str) -> Result<HashSet<String>> {
    family_chunk_ids_at_root(storage, project, None)
}

/// [`family_chunk_ids`] with an injectable projects-root — the seam tests use
/// (a tempdir) so alias resolution never depends on this machine's real
/// `$HOME/projects` layout. `None` uses the real home directory, exactly like
/// `family_chunk_ids`/`cross_project::same_project_family`.
fn family_chunk_ids_at_root(
    storage: &Storage,
    project: &str,
    projects_root_override: Option<&Path>,
) -> Result<HashSet<String>> {
    let family_projects = storage
        .list_project_names("", i64::MAX as usize)?
        .into_iter()
        .filter(|candidate| match projects_root_override {
            Some(root) => cross_project::same_project_family_at_root(project, candidate, root),
            None => cross_project::same_project_family(project, candidate),
        });
    let mut ids = HashSet::new();
    for family_project in family_projects {
        ids.extend(storage.get_chunk_ids_for_project(&family_project)?);
    }
    ids.extend(storage.get_chunk_ids_for_project(project)?);
    Ok(ids)
}

/// Tag `store_reflection` stamps on every explicit memory write so the recall
/// policy can tell a deliberate memory from the derived narratives (stories,
/// episodes, extractions) that share the `reflections` table.
pub const EXPLICIT_MEMORY_TAG: &str = "csr:memory";

/// Synthetic score for a reflection reached only through the keyword channel:
/// the same undated floor `score_fts_candidate` gives a keyword-only chunk.
const FTS_REFLECTION_SCORE: f32 = 0.40;

/// Tags that mark a reflection row as pipeline-derived (a story, episode,
/// briefing, cache or extraction) rather than a deliberate memory write. A
/// derived tag always wins over [`EXPLICIT_MEMORY_TAG`]: the summarizer/story
/// paths route through the same storage helper the MCP `store_reflection`
/// tool uses, so a derived row can carry both tags, and standing must follow
/// what the row actually is, not what it happens to be stamped with.
const DERIVED_REFLECTION_TAGS: &[&str] = &[
    "session_story",
    "session_episode",
    "session_briefing",
    "context_cache",
];

pub(crate) fn has_derived_reflection_tag(tags: &[String]) -> bool {
    tags.iter()
        .any(|t| t.starts_with("narrative_") || DERIVED_REFLECTION_TAGS.contains(&t.as_str()))
}

/// Whether a reflection row is an explicit memory write (the `store_reflection`
/// tool) rather than a pipeline-derived narrative. A derived tag
/// (`session_story`/`session_episode`/`session_briefing`/`context_cache`/
/// `narrative_*`) always disqualifies a row, even alongside
/// [`EXPLICIT_MEMORY_TAG`] — see [`has_derived_reflection_tag`]. Absent a
/// derived tag, rows carrying [`EXPLICIT_MEMORY_TAG`] are explicit by
/// construction; rows written before the stamp existed are recognised by
/// shape: a UUID id (every derived writer uses a `v3_` / `extracted` /
/// `story_` / `heuristic` / `fact_` prefix).
pub(crate) fn is_explicit_memory(id: &str, tags: &[String]) -> bool {
    if has_derived_reflection_tag(tags) {
        return false;
    }
    if tags.iter().any(|t| t == EXPLICIT_MEMORY_TAG) {
        return true;
    }
    is_uuid(id)
}

/// Below this raw (pre-decay, pre-scope) cosine, an explicit memory does not
/// clear the recall floor and must not be granted UserHistory standing — same
/// floor `format::QUICK_CHECK_FLOOR` uses to call a match "weak": below it,
/// cosine similarity on this corpus is not calibrated enough to trust as a
/// relevance signal, let alone lift with a flat +0.50. Without this gate a
/// legacy uuid-shaped reflection at cosine 0.12 outranked the actually
/// relevant chunk for the query — measured live 2026-09-22, project
/// EnhanceMe: `ca488584` (raw 0.116, adj 0.616) and `54dbda97` (raw 0.114,
/// adj 0.614) both outranked the target codex chunk at cosine 0.549.
const EXPLICIT_MEMORY_STANDING_FLOOR: f32 = crate::format::QUICK_CHECK_FLOOR;

/// Recall standing for a reflection candidate. Explicit memory writes that
/// clear [`EXPLICIT_MEMORY_STANDING_FLOOR`] on raw cosine rank with
/// user-history standing: `search::rerank` gives user-authored chunks a flat
/// `W_USER` boost, and without equal standing an exact memory match (cosine
/// 1.0) lost the page to any user chunk above cosine ~0.5 — measured live
/// 2026-09-22: seven 0.63-cosine user chunks scored 1.13 against the memory's
/// 1.00. Below the floor the boost does not apply — the row keeps its
/// derived-narrative treatment (cached provenance floor, usually Unknown) —
/// so a memory that is only weakly (or not at all) related to the query
/// cannot use a raw cosine as low as `EXPLICIT_MEMORY_STANDING_FLOOR - 1` to
/// jump above genuinely relevant content. `raw_cosine` is 0.0 for
/// keyword-only reflection hits (no HNSW cosine exists), which conservatively
/// keeps them off this boost too — their own keyword channel still surfaces
/// them (see `fuse_rrf`). This is a ranking policy only: it never writes
/// `reflections.min_trust`, and the row's provenance event stays Unknown
/// (`reflection_tags_cannot_raise_its_observed_event_above_unknown`).
fn reflection_recall_standing(
    storage: &Storage,
    id: &str,
    tags: &[String],
    raw_cosine: f32,
) -> crate::provenance::TrustTier {
    let tier = if is_explicit_memory(id, tags) {
        crate::provenance::TrustTier::UserHistory
    } else {
        storage
            .get_artifact_min_trust(
                crate::storage::artifact_provenance::ArtifactKind::Reflection,
                id,
            )
            .unwrap_or(crate::provenance::TrustTier::Unknown)
    };
    // The floor gates the flat `W_USER` standing boost regardless of which
    // branch produced `tier`: a derived reflection's CACHED provenance floor
    // can independently be `UserHistory` (or higher) without ever going
    // through the explicit-memory path above, and that cached floor must not
    // bypass the relevance gate either — see this fn's doc and the
    // `weak_similarity_cached_user_history_reflection_does_not_outrank_relevant_chunk`
    // test.
    if raw_cosine < EXPLICIT_MEMORY_STANDING_FLOOR
        && tier >= crate::provenance::TrustTier::UserHistory
    {
        crate::provenance::TrustTier::Unknown
    } else {
        tier
    }
}

/// Determine a reflection's project from its tags: the pipeline writers stamp
/// `project_<name>`; explicit memories usually carry the bare project name as
/// a plain tag, which must not read as cross-project (a penalty, or under an
/// explicit scope a sink below in-family rows — see [`reflection_in_family`]).
/// Falls back to `"unknown"` when no tag identifies a project at all.
fn resolve_reflection_project(tags: &[String], scope: &SearchProjectScope) -> String {
    // Prefer a `project_*` tag that already matches the scope's family: a
    // memory-file reflection can carry several `project_*` slugs (one per
    // source memory file) plus a bare family tag, and the *first* one found
    // is not necessarily the one that keeps the row in scope — see
    // `reflection_in_family`'s doc for the live shape this fixes.
    tags.iter()
        .filter_map(|t| t.strip_prefix("project_"))
        .find(|candidate| scope.is_family(candidate))
        .map(str::to_string)
        .or_else(|| {
            tags.iter()
                .find(|t| t.starts_with("project_"))
                .map(|t| t.trim_start_matches("project_").to_string())
        })
        .or_else(|| tags.iter().find(|t| scope.is_family(t)).cloned())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Whether a reflection/story carrying `tags` is in the project family of
/// `scope`. This is the in-family test `sink_out_of_family_under_explicit_scope`
/// relies on; it does NOT exclude anything. Under an explicit project filter,
/// an out-of-family (or `"unknown"`-project) reflection is kept as a candidate
/// and sunk below every in-family row unless it clears the relevance floor or
/// leads the reflection keyword ranking: a hard drop lost a real answer (a
/// sibling-project story ranked 1 in the keyword index; 6cf8a2b). With no
/// filter (`scope.effective_project` is `None`) every reflection is in family.
///
/// Checks EVERY tag (each `project_*` tag with the prefix stripped, plus
/// every bare tag) against `scope.is_family`, not just whichever single tag
/// [`resolve_reflection_project`] would pick to *display* — a memory-file
/// reflection can carry several `project_*` slugs (one per source memory
/// file) plus a bare family tag, and deciding from only the first `project_*`
/// tag misclassified in-family rows whose first tag happened not to match
/// (live 2026-09-22: `e97fa3b4`/`5eb5aebb`/`22ffc13b`, each carrying a
/// non-matching `project_*` slug alongside the matching bare tag
/// `anukriti-meta-campaigns`).
fn reflection_in_family(tags: &[String], scope: &SearchProjectScope) -> bool {
    scope.effective_project.is_none()
        || tags.iter().any(|tag| {
            let candidate = tag.strip_prefix("project_").unwrap_or(tag.as_str());
            scope.is_family(candidate)
        })
}

/// Result row for a reflection hit from either channel. `keyword` marks a hit
/// that arrived through `reflections_fts` rather than the vector index, with
/// the same `[keyword] ` prefix keyword-only chunks carry. It always builds a result row, it does not filter; out-of-family rows are
/// handled by `sink_out_of_family_under_explicit_scope`.
/// The session a candidate belongs to for session-level fusion: its
/// conversation, except that a plan chunk or a sidechain chunk belongs to the
/// conversation it came from when provenance records one. A reflection is its
/// own session (its conversation id is its id).
fn session_key_of(storage: &Storage, chunk: &crate::import::ConversationChunk) -> String {
    if chunk.conversation_id.starts_with("plan:") || chunk.is_sidechain {
        if let Ok(Some(provenance)) = storage.get_chunk_provenance(&chunk.id) {
            if !provenance.source_conv_id.is_empty() {
                return provenance.source_conv_id;
            }
        }
    }
    chunk.conversation_id.clone()
}

/// Posting count of each query identifier, capped just past the heavy-channel
/// threshold (counting stops there). All zeros when the table is absent or the
/// count fails.
fn postings_term_dfs(
    storage: &Storage,
    idents: &[crate::search::identifiers::QueryIdent],
) -> Vec<usize> {
    let cap = crate::search::hybrid::ID_HEAVY_DF_MAX as i64 + 1;
    storage
        .with_connection(|conn| {
            let table: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                   WHERE type = 'table' AND name = 'identifier_postings')",
                [],
                |row| row.get(0),
            )?;
            if !table {
                return Ok(vec![0; idents.len()]);
            }
            let mut stmt = conn.prepare_cached(
                "SELECT COUNT(*) FROM
                   (SELECT 1 FROM identifier_postings WHERE term = ?1 LIMIT ?2)",
            )?;
            idents
                .iter()
                .map(|ident| {
                    let count: i64 =
                        stmt.query_row(rusqlite::params![ident.term, cap], |row| row.get(0))?;
                    Ok(count.max(0) as usize)
                })
                .collect()
        })
        .unwrap_or_else(|_| vec![0; idents.len()])
}

fn reflection_result(
    id: &str,
    score: f32,
    content: &str,
    tags: &[String],
    timestamp: String,
    scope: &SearchProjectScope,
    keyword: bool,
) -> EnrichedResult {
    let project_name = resolve_reflection_project(tags, scope);
    // Cross-project multiplicative penalty (soft): an out-of-family row under
    // an explicit scope keeps the 0.3x arm and is then ordered by
    // `sink_out_of_family_under_explicit_scope`, never dropped.
    let final_score = score * project_scope_multiplier(&project_name, scope);
    let tag_prefix = if tags.iter().any(|t| t == "session_episode") {
        "[episode] "
    } else if tags.iter().any(|t| t == "session_story") {
        "[story] "
    } else if tags.iter().any(|t| t.starts_with("narrative_v3")) {
        "[narrative] "
    } else {
        "[reflection] "
    };
    let channel_prefix = if keyword { "[keyword] " } else { "" };
    EnrichedResult {
        score: final_score,
        chunk: crate::import::ConversationChunk {
            id: id.to_string(),
            conversation_id: id.to_string(),
            project_name,
            timestamp,
            content: format!("{channel_prefix}{tag_prefix}{content}"),
            message_count: 0,
            summary: None,
            // Reflections/episodes are derived narratives, not raw
            // user-authored chunks — no authority boost from the author
            // field; explicit memories earn theirs in
            // `reflection_recall_standing`.
            author: crate::provenance::Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        },
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    }
}

/// Extract a conversation UUID from a query that is (or contains) a
/// `conv_<uuid>` retrieval handle, or that is a bare UUID. Injection blocks
/// hand out `csr_reflect_on_past("conv_<id>")` handles — those must resolve
/// by exact tag, never by embedding (a UUID embeds as noise and returns
/// unrelated cross-project hits; measured live 2026-07-08).
fn extract_conv_id(query: &str) -> Option<&str> {
    if let Some(pos) = query.find("conv_") {
        if let Some(candidate) = query.get(pos + 5..pos + 5 + 36) {
            if is_uuid(candidate) {
                return Some(candidate);
            }
        }
    }
    let trimmed = query.trim();
    if is_uuid(trimmed) {
        return Some(trimmed);
    }
    None
}

/// Strict 8-4-4-4-12 hex UUID shape check.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Exact-tag lookup for a `conv_<uuid>` retrieval handle: every reflection
/// tagged to that conversation (episode, learnings, narratives), newest
/// first, score pinned to 1.0 — an exact handle is not a similarity guess.
/// Returns None when the tag matches nothing so the caller can fall through
/// to semantic search.
///
/// The v10 validity partition applies HERE TOO (`consumption_enabled` is
/// the caller's `validity_partition_enabled() && dream_consumption_enabled()`
/// outcome — this fast path renders verdict text directly, so it must
/// respect the v10.1 opt-in the same as every other consumer): an exact
/// handle to a conversation whose bound code symbol is stale must carry the
/// same `[stale anchor]`/`[evolved]` annotation a semantic hit would — the
/// fast path previously bypassed validity entirely. All rows share one
/// conversation id, so the partition never reorders here; it only annotates
/// and flags.
fn lookup_by_conv_tag(
    storage: &Arc<Storage>,
    conv_id: &str,
    query: &str,
    limit: usize,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
) -> Result<Option<String>> {
    let start = Instant::now();
    let rows = storage.get_reflections_by_tag(&format!("conv_{}", conv_id), limit)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut enriched: Vec<EnrichedResult> = rows
        .into_iter()
        .map(|(id, content, tags, timestamp)| {
            let project_name = tags
                .iter()
                .find(|t| t.starts_with("project_"))
                .map(|t| t.trim_start_matches("project_").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let tag_prefix = if tags.iter().any(|t| t == "session_episode") {
                "[episode] "
            } else if tags.iter().any(|t| t == "session_story") {
                "[story] "
            } else if tags.iter().any(|t| t.starts_with("narrative_v3")) {
                "[narrative] "
            } else {
                "[reflection] "
            };
            EnrichedResult {
                score: 1.0,
                chunk: crate::import::ConversationChunk {
                    id,
                    conversation_id: conv_id.to_string(),
                    project_name,
                    timestamp,
                    content: format!("{}{}", tag_prefix, content),
                    message_count: 0,
                    summary: None,
                    author: crate::provenance::Speaker::ToolResult,
                    seq: 0,
                    is_sidechain: false,
                },
                resolution: None,
                trust: crate::provenance::TrustTier::Unknown,
                validity_demoted: false,
            }
        })
        .collect();
    // Resolve + apply validity on the fast path too (issue: the exact-handle
    // path returned without ever consulting dream verdicts). One batched
    // query for the single conversation id.
    let chunks: Vec<crate::import::ConversationChunk> =
        enriched.iter().map(|result| result.chunk.clone()).collect();
    let validity = resolve_validity_with(storage, &chunks, consumption_mode);
    apply_validity_partition(&mut enriched, &validity, active_forgetting);
    let search_ms = start.elapsed().as_millis() as u64;
    Ok(Some(format::format_search_results(
        &enriched,
        query,
        "conv-tag exact",
        search_ms,
        0,
    )))
}

/// The trained re-ranker's intent for an embedded query, or "other" when the
/// trained re-ranker is not requested. The single place `reflect_on_past` and
/// the health probe resolve it, so the probe cannot drift from production.
async fn resolve_rerank_intent(
    embeddings: &Arc<EmbeddingEngine>,
    query_vec: &[f32],
) -> &'static str {
    if crate::search::trained_rerank::trained_rerank_requested() {
        crate::hooks::intent::ProbeSet::load_or_build(embeddings)
            .await
            .and_then(|probes| probes.classify(query_vec))
            .map_or("other", |(intent, _)| match intent {
                crate::hooks::intent::Intent::Continue => "continue",
                crate::hooks::intent::Intent::StateRecall => "state_recall",
                crate::hooks::intent::Intent::Explore => "explore",
            })
    } else {
        "other"
    }
}

/// Run the recall `run` and report the time of the whole tool call in its header:
/// from `started` (request entry) to the built response. The formatter alone only
/// knows the embed and search stages, which left the header at tens of
/// milliseconds on calls that took seconds.
pub(crate) async fn stamp_whole_call(
    started: Instant,
    run: impl std::future::Future<Output = Result<String>>,
) -> Result<String> {
    let page = run.await?;
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(format::restamp_performance(page, total_ms))
}

/// Full semantic search with rich XML results.
pub async fn reflect_on_past(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    super::stage_timing::scope(
        query,
        reflect_on_past_untimed(
            storage, embeddings, search, query, limit, min_score, project,
        ),
    )
    .await
}

async fn reflect_on_past_untimed(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    validate_page(0, limit)?;
    let search_mode = search_mode_from(std::env::var("CSR_SEARCH_MODE").ok().as_deref());
    // `partition_enabled` (the pre-existing `CSR_NO_VALIDITY_PARTITION` kill
    // switch) gates ancestry availability too, so it must NOT be folded with
    // `CSR_DREAM_CONSUMPTION` — that fold is exactly the regression the
    // rejected first T2 attempt shipped (it killed release-ancestry ranking
    // whenever dream consumption defaulted off). `consumption_enabled` gates
    // ONLY whether the resolved verdict map is populated; ancestry loads
    // independently of it — see `CandidateSignals`'s doc.
    let partition_enabled = validity_partition_enabled();
    let consumption_mode = consumption_mode_for_partition(partition_enabled);
    let active_forgetting = active_forgetting_enabled();
    // Retrieval-handle fast path: `conv_<uuid>` (or a bare UUID) resolves by
    // exact tag. Falls through to semantic search only when the tag matches
    // nothing, so a stale handle still gets a best-effort answer.
    if let Some(conv_id) = extract_conv_id(query) {
        if let Some(result) = lookup_by_conv_tag(
            storage,
            conv_id,
            query,
            limit.max(5),
            consumption_mode,
            active_forgetting,
        )? {
            return Ok(result);
        }
    }

    let embed_start = Instant::now();
    let query_vec = embed_query(embeddings, query).await?;
    let embed_ms = embed_start.elapsed().as_millis() as u64;
    super::stage_timing::stage("embed", embed_start);
    let intent_start = Instant::now();
    let rerank_intent = resolve_rerank_intent(embeddings, &query_vec).await;
    super::stage_timing::stage("intent", intent_start);

    reflect_on_past_with_vec_intent(
        storage,
        search,
        &query_vec,
        query,
        limit,
        min_score,
        project,
        embed_ms,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
    )
    .await
}

/// Keyword-only recall for a server whose configured daemon is not
/// answering: the production pipeline in FTS mode — no query embedding, no
/// vector search, nothing loaded in-process.
pub async fn reflect_keyword_only(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    limit: usize,
    project: Option<&str>,
) -> Result<String> {
    validate_page(0, limit)?;
    let partition_enabled = validity_partition_enabled();
    reflect_on_past_with_vec_mode(
        storage,
        search,
        &[],
        query,
        limit.max(1),
        0.0,
        project,
        0,
        partition_enabled,
        consumption_mode_for_partition(partition_enabled),
        active_forgetting_enabled(),
        "other",
        SearchMode::Fts,
        RecallRerankMode::Baseline,
        true,
    )
    .await
}

/// Reject unbounded pagination before embedding, IPC or storage access.
pub(crate) fn validate_page(offset: usize, limit: usize) -> Result<()> {
    let end = offset
        .checked_add(limit)
        .ok_or_else(|| anyhow::anyhow!("offset + limit overflow"))?;
    anyhow::ensure!(
        offset <= GET_MORE_WINDOW && limit <= GET_MORE_WINDOW && end <= GET_MORE_WINDOW * 2,
        "offset and limit must each be at most {GET_MORE_WINDOW}"
    );
    Ok(())
}

pub(crate) enum KeywordTool<'a> {
    Recall,
    Quick,
    Insights,
    Recency {
        time_range: Option<&'a str>,
        since: Option<&'a str>,
        until: Option<&'a str>,
    },
    Concept,
    More {
        offset: usize,
    },
    Why,
}

/// Keyword candidates keep the same project family and fixed pagination window.
fn keyword_candidates(
    storage: &Arc<Storage>,
    query: &str,
    project: Option<&str>,
    window: Option<(&str, &str)>,
) -> Result<Vec<EnrichedResult>> {
    let scope = resolve_search_scope(storage, project)?;
    let mut hits = if scope.effective_project.is_some() {
        let mut hits = Vec::new();
        for project in &scope.family_projects {
            hits.extend(storage.fts5_search_in_window(
                query,
                GET_MORE_WINDOW,
                Some(project),
                window,
            )?);
        }
        hits
    } else {
        storage.fts5_search_in_window(query, GET_MORE_WINDOW, None, window)?
    };
    hits.sort_by(|a, b| a.2.total_cmp(&b.2).then_with(|| a.0.id.cmp(&b.0.id)));
    hits.dedup_by(|a, b| a.0.id == b.0.id);
    hits.truncate(GET_MORE_WINDOW);
    let results: Vec<_> = hits
        .into_iter()
        .map(|(chunk, _, _)| crate::search::SearchResult {
            id: chunk.id,
            score: 0.0,
        })
        .collect();
    let mut enriched = enrich_results_with_active_forgetting(
        storage,
        &results,
        consumption_mode_for_partition(validity_partition_enabled()),
        active_forgetting_enabled(),
    )?;
    for result in &mut enriched {
        result.chunk.content.insert_str(0, "[keyword] ");
    }
    Ok(enriched)
}

/// Render an outage through the tool's own response contract.
pub(crate) async fn keyword_fallback(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    limit: usize,
    project: Option<&str>,
    tool: KeywordTool<'_>,
) -> Result<String> {
    let offset = match tool {
        KeywordTool::More { offset } => offset,
        _ => 0,
    };
    validate_page(offset, limit)?;
    if matches!(tool, KeywordTool::Recall) {
        return reflect_keyword_only(storage, search, query, limit, project).await;
    }
    let window = match tool {
        KeywordTool::Recency {
            time_range,
            since,
            until,
        } => Some(recency_window(time_range, since, until)?),
        _ => None,
    };
    let dates = window
        .as_ref()
        .map(|(start, end)| (start.to_rfc3339(), end.to_rfc3339()));
    let mut results = keyword_candidates(
        storage,
        query,
        project,
        dates
            .as_ref()
            .map(|(start, end)| (start.as_str(), end.as_str())),
    )?;
    // The live chat sinks on the fallback paths too, so a page served from here
    // continues the order recall's own page was cut from. Evidence for csr_why
    // and the insights summary are not a ranked page of chats.
    // The rig's own rows sink here as in keyword recall, so offset N continues
    // that page. Keyword candidates are transcript rows, never reflections.
    if !matches!(tool, KeywordTool::Why | KeywordTool::Insights) {
        sink_rig_rows(storage, &mut results, &HashMap::new(), query);
        sink_live_session(&mut results, storage, query);
    }
    match tool {
        KeywordTool::Recall => unreachable!(),
        KeywordTool::Quick => Ok(format::format_keyword_quick_check(&results, query)),
        KeywordTool::Insights => {
            results.truncate(limit);
            Ok(format::format_search_insights(&results, query))
        }
        KeywordTool::Recency { time_range, .. } => {
            let (start, end) = window.expect("recency window resolved before FTS");
            results.truncate(limit);
            let desc = temporal::describe_window(time_range, &start, &end);
            let hint = page::recency_hint(time_range, &start, &end, &results);
            Ok(format::format_recency_results_hinted(
                &results,
                query,
                &desc,
                hint.as_deref(),
            ))
        }
        KeywordTool::Concept => {
            results.truncate(limit);
            let (_, label) = cross_project::normalize_project_scope(storage, project)?;
            Ok(format::format_search_results(&results, query, &label, 0, 0))
        }
        KeywordTool::More { offset } => {
            let total = results.len();
            let page: Vec<_> = results.into_iter().skip(offset).take(limit).collect();
            Ok(format::format_more_keyword(&page, query, offset, total))
        }
        KeywordTool::Why => {
            use crate::search::reinstatement::{EvidenceItem, ReinstateTrace, Via};
            let items: Vec<_> = results
                .into_iter()
                .map(|r| EvidenceItem {
                    chunk_id: r.chunk.id,
                    conversation_id: r.chunk.conversation_id,
                    score: 0.0,
                    best_route: Via::Keyword,
                    routes: [Via::Keyword].into_iter().collect(),
                    route_scores: [(Via::Keyword, 0.0)].into_iter().collect(),
                    timestamp: r.chunk.timestamp,
                    excerpt: crate::format::truncate_chars(
                        &r.chunk.content.replace(['\n', '\r'], " "),
                        200,
                    )
                    .to_string(),
                    ratification: None,
                    trust: r.trust,
                })
                .collect();
            // The whole pool is classified before the cut to `limit`.
            let items = super::why_echo::take_clean(storage, query, items, limit);
            render_why_evidence(
                storage,
                query,
                items,
                &HashSet::new(),
                &ReinstateTrace::default(),
            )
        }
    }
}

/// Reranker selection for the production recall pipeline. Runtime honors the
/// deployment flag/latest gate, Baseline forces the deterministic policy, and
/// Candidate applies an explicit not-yet-persisted model for offline gating.
#[derive(Clone, Copy)]
pub(crate) enum RecallRerankMode<'a> {
    Runtime,
    Baseline,
    Candidate(&'a crate::search::trained_rerank::LinearModel),
}

use crate::search::hybrid::{
    collapse_to_sessions_by, id_channel_weight, keyword_chunk_hits, RRF_K,
};
pub use crate::search::hybrid::{search_mode_from, SearchMode};

/// An out-of-family reflection keeps its fused rank under an explicit project
/// filter when it is among the first K rows of the reflection keyword ranking.
/// The keyword channel OR-joins single query words, so merely matching is weak
/// evidence; leading the ranking is not. Measured live 2026-09-23: a real
/// burst answer (a May story from the sibling Anukriti-Campaigns project, only
/// reachable as `[keyword] [story]`) and its narrative ranked 1 and 2, while
/// the rows that leaked into project=EnhanceMe ranked 15, 21 and 24 (and 38 on
/// the other query).
const OUT_OF_FAMILY_KEYWORD_TOP_K: usize = 3;

/// Stable partition under an explicit project filter: weak out-of-family
/// reflections/stories sink below every other result, everything else keeps
/// its fused order. Chunk search is already pre-filtered to the family, so
/// only reflection rows can be out of family. A reflection is in family when
/// its resolved project or ANY of its tags (bare, or with the `project_`
/// prefix stripped) is in the family: explicit memories often carry the bare
/// project name next to a topical `project_<topic>` tag. An out-of-family row
/// keeps its place when its raw cosine clears `QUICK_CHECK_FLOOR` (a strong
/// sibling-project answer, e.g. a May story tagged with a sibling project that
/// a real burst query needed) and sinks when it does not (live 2026-09-23,
/// project=EnhanceMe: an anukriti story at 0.121 and an unknown-project
/// reflection at 0.116 sat at ranks 5-6). A row at the very top of the
/// reflection keyword ranking also keeps its place (see
/// [`OUT_OF_FAMILY_KEYWORD_TOP_K`]). No-op without an explicit project.
fn sink_out_of_family_under_explicit_scope(
    enriched: &mut Vec<EnrichedResult>,
    scope: &SearchProjectScope,
    reflection_tags: &HashMap<String, Vec<String>>,
    reflection_raw_cosine: &HashMap<String, f32>,
    reflection_keyword_ids: &HashSet<&str>,
) {
    if scope.effective_project.is_none() {
        return;
    }
    let in_family = |e: &EnrichedResult| {
        if scope.is_family(&e.chunk.project_name) {
            return true;
        }
        reflection_tags
            .get(&e.chunk.id)
            .is_some_and(|tags| reflection_in_family(tags, scope))
    };
    let strong = |e: &EnrichedResult| {
        reflection_keyword_ids.contains(e.chunk.id.as_str())
            || reflection_raw_cosine
                .get(&e.chunk.id)
                .is_some_and(|c| *c >= crate::format::QUICK_CHECK_FLOOR)
    };
    let (keep, sink): (Vec<_>, Vec<_>) = std::mem::take(enriched)
        .into_iter()
        .partition(|e| in_family(e) || strong(e));
    enriched.extend(keep);
    enriched.extend(sink);
}

/// The Claude Code session this MCP server was spawned for, from the
/// `CLAUDE_CODE_SESSION_ID` its environment carries (Claude Code starts one
/// server process per session). Read once; empty or unset is `None` and every
/// sink below becomes a no-op.
///
/// Known gap, not fixed here: after `/clear` Claude Code gives the chat a new
/// session id, but this process keeps the environment it was spawned with, so
/// the cleared chat's new turns are not covered by the sink.
fn live_session_id() -> Option<String> {
    #[cfg(test)]
    {
        LIVE_SESSION_OVERRIDE.with(|cell| cell.borrow().clone())
    }
    #[cfg(not(test))]
    {
        static LIVE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        LIVE.get_or_init(|| parse_live_session(std::env::var("CLAUDE_CODE_SESSION_ID").ok()))
            .clone()
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only stand-in for the environment variable: parallel tests must
    /// not race on the process environment.
    static LIVE_SESSION_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// The session id in a raw environment value: trimmed, `None` when blank.
fn parse_live_session(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// True when the query names the live session on purpose (the bare id or the
/// `conv_<id>` form), so the caller is asking for that chat.
fn query_names_session(query: &str, live: &str) -> bool {
    query.to_lowercase().contains(&live.to_lowercase())
}

/// Sink every hit that belongs to the live session below every other hit,
/// never dropping one: a search that only matches the caller's own chat still
/// returns it. The session of a hit is [`session_key_of`], so a sidechain
/// (subagent) chunk or a plan chunk that came from the live session sinks with
/// it. Stable: the order inside the other hits, and inside the live-session
/// hits, is unchanged. No-op without a live session, and when the query names
/// that session.
fn sink_session_hits(
    enriched: &mut [EnrichedResult],
    storage: &Storage,
    live: Option<&str>,
    query: &str,
) {
    let Some(live) = live.filter(|live| !query_names_session(query, live)) else {
        return;
    };
    // `sort_by_key` is stable, so each group keeps the order it came in.
    enriched.sort_by_cached_key(|e| is_session_hit(storage, &e.chunk, live));
}

/// True when `chunk` belongs to the live session: its conversation id is the
/// session id (the transcript's file stem; the `conv_` form counts too), or
/// provenance ties it to that conversation ([`session_key_of`]).
fn is_session_hit(storage: &Storage, chunk: &crate::import::ConversationChunk, live: &str) -> bool {
    let key = session_key_of(storage, chunk);
    // A row derived from the live chat (`conv_`, `v3_`, `extracted_v3_`,
    // `story_`, ...) names it as the id's last `_` segment.
    key == live
        || key
            .strip_suffix(live)
            .is_some_and(|prefix| prefix.ends_with('_'))
}

/// [`sink_session_hits`] for bare chunks (the file search holds no scores).
fn sink_session_chunks(
    chunks: &mut [crate::import::ConversationChunk],
    storage: &Storage,
    live: Option<&str>,
    query: &str,
) {
    let Some(live) = live.filter(|live| !query_names_session(query, live)) else {
        return;
    };
    chunks.sort_by_cached_key(|chunk| is_session_hit(storage, chunk, live));
}

/// [`sink_session_hits`] for the session this server runs in.
fn sink_live_session(enriched: &mut [EnrichedResult], storage: &Storage, query: &str) {
    let live = live_session_id();
    sink_session_hits(enriched, storage, live.as_deref(), query);
}

/// Sink the rows that cannot answer a question below the rows that can, never
/// dropping one: first the rows that only restate a question
/// ([`crate::search::echo::is_question_echo`]), then CSR's own search-result
/// and `csr_why` output
/// ([`crate::import::contamination::is_csr_tool_payload`]) together with a
/// screen recorder's frames
/// ([`crate::import::contamination::is_terminal_replay`]). All are decided
/// by the row's own text, never by its similarity to the query, so a question
/// asked again whose row also states the answer keeps its rank. Stable: every
/// class keeps its fused order, so when an echo and an answer come from the
/// same conversation the answer is what represents that conversation.
fn sink_non_answers(enriched: &mut [EnrichedResult], query: &str) {
    let classes: HashMap<String, u8> = enriched
        .iter()
        .map(|result| {
            (
                result.chunk.id.clone(),
                non_answer_class(&result.chunk.content, query),
            )
        })
        .collect();
    enriched.sort_by_key(|result| classes.get(&result.chunk.id).copied().unwrap_or(0));
}

/// 2 for CSR's own payload or a terminal replay, 1 for a question echo, 0 for a
/// row that can answer.
fn non_answer_class(content: &str, query: &str) -> u8 {
    if crate::import::contamination::is_csr_tool_payload(content)
        || crate::import::contamination::is_terminal_replay(content)
    {
        2
    } else if crate::search::echo::is_question_echo(content, query) {
        1
    } else {
        0
    }
}

/// Move every id in `prefix_ids` that also appears in `order` to the front,
/// in `prefix_ids`' own order, leaving every other id's relative order
/// untouched. `prefix_ids` is small (a handful of exact-prefix reflection
/// matches at most) so a linear scan per id is fine.
fn promote_exact_prefix_reflections(order: Vec<String>, prefix_ids: &[String]) -> Vec<String> {
    if prefix_ids.is_empty() {
        return order;
    }
    let prefix_set: HashSet<&str> = prefix_ids.iter().map(String::as_str).collect();
    let mut promoted: Vec<String> = Vec::with_capacity(order.len());
    for id in prefix_ids {
        if order.iter().any(|candidate| candidate == id) {
            promoted.push(id.clone());
        }
    }
    for id in order {
        if !prefix_set.contains(id.as_str()) {
            promoted.push(id);
        }
    }
    promoted
}

/// Everything in `reflect_on_past` after query embedding — the seam the
/// end-to-end partition test drives with a synthetic query vector (no
/// FastEmbed model in tests) and explicit kill-switch outcomes (never by
/// mutating the process env — see `resolve_validity_with`'s doc).
/// `partition_enabled` is `validity_partition_enabled()` and
/// `consumption_enabled` is `partition_enabled && dream_consumption_enabled()`
/// for real callers — kept as two separate parameters all the way down to
/// `CandidateSignals` so dream-verdict consumption can never fold into (and
/// thereby disable) release-ancestry availability.
#[allow(clippy::too_many_arguments)]
async fn reflect_on_past_with_vec_intent(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
    embed_ms: u64,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
    rerank_intent: &str,
    search_mode: SearchMode,
) -> Result<String> {
    reflect_on_past_with_vec_mode(
        storage,
        search,
        query_vec,
        query,
        limit,
        min_score,
        project,
        embed_ms,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
        RecallRerankMode::Runtime,
        true,
    )
    .await
}

/// Render the UserPromptSubmit injection for one prompt through the pipeline
/// the daemon's hook socket runs (`prompt_submit`), for `csr_hook_prompt`.
///
/// The text is the same; the side effects are not. Exposure rows, retrieval
/// events and dream claims are dropped uncommitted, as for a reply the hook
/// never acknowledged, so timing 50 prompts through the `mcp_tool` transport
/// teaches the reranker nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn hook_prompt(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    projects_dir: &Path,
    index_dir: &Path,
    prompt: &str,
    session_id: &str,
    prompt_id: Option<&str>,
    cwd: &str,
) -> Result<String> {
    let engine = crate::engine::Engine::from_parts_with_index_dir(
        storage.clone(),
        embeddings.clone(),
        search.clone(),
        projects_dir.to_path_buf(),
        index_dir.to_path_buf(),
    );
    let input = crate::hooks::HookInput {
        prompt: Some(prompt.to_string()),
        session_id: Some(session_id.to_string()),
        prompt_id: prompt_id.map(str::to_string),
        cwd: Some(cwd.to_string()),
        ..Default::default()
    };
    crate::hooks::prompt_submit::render_uncommitted(&input, &engine, Path::new(cwd)).await
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
async fn reflect_on_past_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
    embed_ms: u64,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
) -> Result<String> {
    reflect_on_past_with_vec_intent(
        storage,
        search,
        query_vec,
        query,
        limit,
        min_score,
        project,
        embed_ms,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        "other",
        SearchMode::Hybrid,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn reflect_on_past_with_vec_mode(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
    embed_ms: u64,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
    rerank_intent: &str,
    search_mode: SearchMode,
    rerank_mode: RecallRerankMode<'_>,
    record_retrievals: bool,
) -> Result<String> {
    let scope_start = Instant::now();
    let scope = resolve_search_scope(storage, project)?;
    super::stage_timing::stage("p_scope", scope_start);
    reflect_on_past_with_vec_in_scope_mode(
        storage,
        search,
        query_vec,
        query,
        limit,
        min_score,
        &scope,
        embed_ms,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
        rerank_mode,
        record_retrievals,
    )
    .await
}

/// Run a curated offline query through the production recall pipeline while
/// selecting its reranker explicitly and suppressing runtime telemetry. The
/// production validity/consumption settings are resolved here, keeping the
/// gate from growing a parallel search implementation.
pub(crate) async fn reflect_for_curated_eval_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    project: &str,
    rerank_mode: RecallRerankMode<'_>,
) -> Result<String> {
    const LIMIT: usize = 10;
    const MIN_SCORE: f32 = 0.20;

    let partition_enabled = validity_partition_enabled();
    let consumption_mode = consumption_mode_for_partition(partition_enabled);
    let active_forgetting = active_forgetting_enabled();
    reflect_on_past_with_vec_mode(
        storage,
        search,
        query_vec,
        query,
        LIMIT,
        MIN_SCORE,
        Some(project),
        0,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        "explore",
        SearchMode::Hybrid,
        rerank_mode,
        false,
    )
    .await
}

#[path = "tools_health_probe.rs"]
mod health_probe;
pub(crate) use health_probe::reflect_for_health_probe;
#[cfg(test)]
use health_probe::{health_probe_scope, reflect_for_health_probe_with_vec};

/// One conversation-level hit from the production recall pipeline for an
/// offline benchmark. Chunk candidates are collapsed after production fusion
/// and reranking, so a long session cannot occupy multiple result slots.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BenchHit {
    pub conversation_id: String,
    pub score: f32,
}

pub(crate) async fn reflect_for_bench_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    fetch: usize,
    mode: SearchMode,
) -> Result<Vec<BenchHit>> {
    let scope = SearchProjectScope::resolve_with(storage, Some("csr-bench"), None, None, None)?;
    let mut candidate_fetch = fetch.saturating_mul(4).max(20);
    let mut previous_candidates = HashSet::new();
    loop {
        let mut pass = reflect_gather_pass(
            storage,
            search,
            query_vec,
            query,
            candidate_fetch,
            candidate_fetch,
            -1.0,
            &scope,
            false,
            ConsumptionMode::Off,
            false,
            "explore",
            mode,
            RecallRerankMode::Baseline,
            0,
            &CallMemo::default(),
        )
        .await?;
        apply_resolutions_before_limit(
            &mut pass.enriched,
            storage,
            &pass.validity,
            candidate_fetch,
            false,
        );
        let candidate_ids = pass
            .enriched
            .iter()
            .map(|result| result.chunk.id.clone())
            .collect::<HashSet<_>>();
        let unique_conversations = pass
            .enriched
            .iter()
            .map(|result| result.chunk.conversation_id.as_str())
            .collect::<HashSet<_>>()
            .len();
        let exhausted = !pass.window_full || candidate_ids == previous_candidates;
        if unique_conversations >= fetch || exhausted || candidate_fetch == usize::MAX {
            let mut seen = HashSet::new();
            return Ok(pass
                .enriched
                .into_iter()
                .filter(|result| seen.insert(result.chunk.conversation_id.clone()))
                .take(fetch)
                .map(|result| BenchHit {
                    conversation_id: result.chunk.conversation_id,
                    score: result.score,
                })
                .collect());
        }
        previous_candidates = candidate_ids;
        candidate_fetch = candidate_fetch.saturating_mul(2);
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
async fn reflect_on_past_with_vec_in_scope(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    min_score: f32,
    scope: &SearchProjectScope,
    embed_ms: u64,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
) -> Result<String> {
    reflect_on_past_with_vec_in_scope_mode(
        storage,
        search,
        query_vec,
        query,
        limit,
        min_score,
        scope,
        embed_ms,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        "other",
        SearchMode::Hybrid,
        RecallRerankMode::Runtime,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn reflect_on_past_with_vec_in_scope_mode(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    min_score: f32,
    scope: &SearchProjectScope,
    embed_ms: u64,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
    rerank_intent: &str,
    search_mode: SearchMode,
    rerank_mode: RecallRerankMode<'_>,
    record_retrievals: bool,
) -> Result<String> {
    let page = reflect_final_page(
        storage,
        search,
        query_vec,
        query,
        limit,
        min_score,
        scope,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
        rerank_mode,
        record_retrievals,
    )
    .await?;
    let format_start = Instant::now();
    let xml = format::format_search_results_with_ce(
        &page.enriched,
        query,
        &scope.scope_label,
        page.search_ms,
        embed_ms,
        &page.display_rank_scores,
        page.ce.as_ref(),
    );
    let xml = annotate_collapsed(xml, page.cross_source_collapsed);
    let xml = annotate_short_page(xml, limit, &page);
    super::stage_timing::stage("format", format_start);
    // Session attributes appear only while the episode channel runs; with it
    // off the page is the chunk-only page, byte for byte.
    if search_mode == SearchMode::Hybrid && crate::search::episodes::channel_active(storage) {
        return Ok(annotate_sessions(xml, &page.enriched));
    }
    Ok(xml)
}

/// The final result page of the production recall pipeline, before it is
/// formatted: what `reflect_on_past` returns and what the health probe reads.
struct FinalPage {
    enriched: Vec<EnrichedResult>,
    display_rank_scores: Vec<DisplayRankScore>,
    search_ms: u64,
    /// Cross-encoder summary of the recall; `None` off the CE path.
    ce: Option<format::CeSummary>,
    /// Rows left out because another row carries the same text.
    cross_source_collapsed: Collapsed,
    /// The last candidate window came back short: no candidate lies beyond it.
    exhausted: bool,
}

/// Candidates fetched from each keyword lane to fill a page of `n` rows: four
/// times the page, at least [`RECALL_POOL_MIN`]. The sinks (non-answers,
/// resolved, out-of-family, demoted) only move or remove rows that were
/// fetched, so a pool as wide as the page leaves the page short or answerless
/// whenever they act. The vector lanes fetch [`vector_pool`].
fn recall_pool(n: usize) -> usize {
    n.saturating_mul(4).max(RECALL_POOL_MIN)
}

const RECALL_POOL_MIN: usize = 60;

/// Candidates fetched from the vector lanes (chunks and reflections) for a page
/// of `n`: twice the keyword pool. The provenance rerank can lift an answer
/// only from a window that contains it, and the earlier askings of the
/// question sit above it in cosine: on the live corpus the flagship answer is
/// in the first 60 vector hits on some days and past them on others, and in the
/// first 120 on both. The keyword lanes gain nothing from the extra depth and
/// pay for every row (a verdict and ancestry lookup each), so they stay at
/// [`recall_pool`].
fn vector_pool(n: usize) -> usize {
    n.saturating_mul(4).max(VECTOR_POOL_MIN)
}

const VECTOR_POOL_MIN: usize = 120;

/// The settled candidate pool of one query: fused, sunk and settled, in final
/// order, and not yet cut to a page. Page one cuts its first `limit` rows and
/// `csr_get_more` serves later offsets from the same list, so every page is a
/// slice of one order.
#[derive(Clone)]
struct SettledPool {
    enriched: Vec<EnrichedResult>,
    display_rank_scores: Vec<DisplayRankScore>,
    search_ms: u64,
    /// Cross-encoder summary of the recall that built this pool.
    ce: Option<format::CeSummary>,
    /// No lane filled its window, so the pool holds every candidate there is.
    exhausted: bool,
    /// How many leading rows a caller has been shown. What a page published is
    /// frozen: a pool rebuilt wider keeps these rows, in this order, and only
    /// what follows them is new.
    published: usize,
    /// Rows the same-text dedupes left out of this pool.
    cross_source_collapsed: Collapsed,
}

impl SettledPool {
    /// This pool widened to a new settle of the same query: the rows already
    /// published first, as they were shown, then the new pool's rows that are
    /// not among them. The new pool sinks echoes it can now see over a wider
    /// window, and re-ordering the published rows to match would repeat some
    /// and skip others across pages.
    fn widened_by(self, fresh: SettledPool) -> SettledPool {
        let frozen = self.published.min(self.enriched.len());
        let mut enriched: Vec<EnrichedResult> = self.enriched.into_iter().take(frozen).collect();
        let shown: HashSet<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
        enriched.extend(
            fresh
                .enriched
                .into_iter()
                .filter(|e| !shown.contains(&e.chunk.id)),
        );
        let mut display_rank_scores = fresh.display_rank_scores;
        let scored: HashSet<String> = display_rank_scores
            .iter()
            .map(|score| score.chunk_id.clone())
            .collect();
        display_rank_scores.extend(
            self.display_rank_scores.into_iter().filter(|score| {
                !scored.contains(&score.chunk_id) && shown.contains(&score.chunk_id)
            }),
        );
        SettledPool {
            published: frozen,
            enriched,
            display_rank_scores,
            search_ms: fresh.search_ms,
            ce: fresh.ce,
            exhausted: fresh.exhausted,
            cross_source_collapsed: fresh.cross_source_collapsed,
        }
    }
}

/// Everything that makes two recall calls the same query for pagination.
#[derive(Clone, PartialEq, Eq)]
struct PoolKey {
    query: String,
    scope_label: String,
    effective_project: Option<String>,
    min_score_bits: u32,
    search_mode: SearchMode,
    partition_enabled: bool,
    consumption: ConsumptionMode,
    active_forgetting: bool,
    /// The project family the ranking boosts and sinks by: `project="all"` with
    /// a current project is a different ranking from the same `all` without one.
    family_anchor: Option<String>,
    current_project: Option<String>,
    projects_root: Option<PathBuf>,
    /// The newest verdict rows when the pool was built (see
    /// [`Storage::verdict_generation`]): a verdict recorded since, by this
    /// process or another, makes the pool stale.
    verdict_generation: (i64, i64),
}

impl PoolKey {
    #[allow(clippy::too_many_arguments)]
    fn new(
        query: &str,
        scope: &SearchProjectScope,
        min_score: f32,
        search_mode: SearchMode,
        partition_enabled: bool,
        consumption: ConsumptionMode,
        active_forgetting: bool,
        verdict_generation: (i64, i64),
    ) -> Self {
        Self {
            query: query.to_string(),
            scope_label: scope.scope_label.clone(),
            effective_project: scope.effective_project.clone(),
            min_score_bits: min_score.to_bits(),
            search_mode,
            partition_enabled,
            consumption,
            active_forgetting,
            family_anchor: scope.family_anchor.clone(),
            current_project: scope.current_project_for_all_scope.clone(),
            projects_root: scope.projects_root_override.clone(),
            verdict_generation,
        }
    }
}

struct CachedPool {
    storage: std::sync::Weak<Storage>,
    key: PoolKey,
    at: Instant,
    pool: SettledPool,
}

/// Pools kept for pagination. A page asked for after the first is served from
/// the pool page one built, so the pages never disagree about the order; a
/// pool older than [`POOL_TTL`] is rebuilt, which is the offset-pagination
/// contract (stable while the data is). A pool that has to be rebuilt (expired,
/// evicted, or shorter than the offset) is rebuilt by the same pipeline with
/// the default rerank intent; the intent feeds only the opt-in trained
/// reranker (`CSR_TRAINED_RERANK=1`), so under the default settings the order
/// is the same.
static POOL_CACHE: std::sync::LazyLock<std::sync::Mutex<Vec<CachedPool>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));
const POOL_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const POOL_CACHE_ENTRIES: usize = 8;

fn cache_pool(storage: &Arc<Storage>, key: PoolKey, pool: SettledPool) {
    let Ok(mut cache) = POOL_CACHE.lock() else {
        return;
    };
    let same_storage = |entry: &CachedPool| {
        entry
            .storage
            .upgrade()
            .is_some_and(|held| Arc::ptr_eq(&held, storage))
    };
    // Drop what is expired, what belongs to a store that is gone, and the
    // previous pool of this very query.
    cache.retain(|entry| {
        entry.at.elapsed() < POOL_TTL
            && entry.storage.upgrade().is_some()
            && !(entry.key == key && same_storage(entry))
    });
    // The cap is per store: the oldest pool of THIS store goes, never another
    // store's. A process serves one store, so this is the same cap there.
    while cache.iter().filter(|entry| same_storage(entry)).count() >= POOL_CACHE_ENTRIES {
        match cache.iter().position(&same_storage) {
            Some(oldest) => {
                cache.remove(oldest);
            }
            None => break,
        }
    }
    cache.push(CachedPool {
        storage: Arc::downgrade(storage),
        key,
        at: Instant::now(),
        pool,
    });
}

/// The cached pool for `key` whatever it covers, if it is still fresh.
fn cached_entry(storage: &Arc<Storage>, key: &PoolKey) -> Option<SettledPool> {
    let cache = POOL_CACHE.lock().ok()?;
    cache
        .iter()
        .find(|entry| {
            entry.key == *key
                && entry.at.elapsed() < POOL_TTL
                && entry
                    .storage
                    .upgrade()
                    .is_some_and(|held| Arc::ptr_eq(&held, storage))
        })
        .map(|entry| entry.pool.clone())
}

/// The cached pool for `key`, if it covers `need` rows: it holds that many, or
/// it holds every candidate there is. The first `need` rows count as published
/// from now on.
fn cached_pool(storage: &Arc<Storage>, key: &PoolKey, need: usize) -> Option<SettledPool> {
    let mut cache = POOL_CACHE.lock().ok()?;
    let entry = cache.iter_mut().find(|entry| {
        entry.key == *key
            && entry.at.elapsed() < POOL_TTL
            && entry
                .storage
                .upgrade()
                .is_some_and(|held| Arc::ptr_eq(&held, storage))
            && (entry.pool.exhausted || entry.pool.enriched.len() >= need)
    })?;
    entry.pool.published = entry
        .pool
        .published
        .max(need.min(entry.pool.enriched.len()));
    Some(entry.pool.clone())
}

/// Forget every pool of `storage`: a verdict recorded through this process
/// must show on the next page, not after the pool expires.
fn clear_pool_cache(storage: &Arc<Storage>) {
    if let Ok(mut cache) = POOL_CACHE.lock() {
        cache.retain(|entry| {
            entry
                .storage
                .upgrade()
                .is_some_and(|held| !Arc::ptr_eq(&held, storage))
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn reflect_final_page(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    limit: usize,
    min_score: f32,
    scope: &SearchProjectScope,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
    rerank_intent: &str,
    search_mode: SearchMode,
    rerank_mode: RecallRerankMode<'_>,
    record_retrievals: bool,
) -> Result<FinalPage> {
    // Read before the pool is built: a verdict that lands while it is built
    // leaves the pool keyed to the older generation, i.e. stale, never fresh.
    let gen_start = Instant::now();
    let verdict_generation = storage.verdict_generation();
    super::stage_timing::stage("p_verdict_gen", gen_start);
    let mut pool = settled_pool(
        storage,
        search,
        query_vec,
        query,
        limit,
        min_score,
        scope,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
        rerank_mode,
    )
    .await?;
    // Only the runtime ranking is paged later; an offline eval run under
    // another reranker must not stand in for it.
    pool.published = limit.min(pool.enriched.len());
    if matches!(rerank_mode, RecallRerankMode::Runtime) {
        cache_pool(
            storage,
            PoolKey::new(
                query,
                scope,
                min_score,
                search_mode,
                partition_enabled,
                consumption_mode,
                active_forgetting,
                verdict_generation,
            ),
            pool.clone(),
        );
    }
    let mut enriched = pool.enriched;
    enriched.truncate(limit);

    if crate::mcp::stage_probe::is_active() {
        let ranked: Vec<(String, String, f32)> = enriched
            .iter()
            .map(|e| (e.chunk.id.clone(), e.chunk.conversation_id.clone(), e.score))
            .collect();
        crate::mcp::stage_probe::record_stage("final_page", &ranked);
    }

    // TAD: log each RETURNED memory as an MCP-search retrieval event — after the
    // limit cut, so telemetry agrees with what the caller actually saw.
    // session_id="mcp" is a fixed label for the retrieval log, not the Claude Code
    // session (that one is `live_session_id`, used by the sink). Non-fatal.
    if record_retrievals {
        for e in &enriched {
            let _ = storage.log_retrieval_event(&e.chunk.id, "chunk", "mcp_search", "mcp");
        }
    }

    Ok(FinalPage {
        enriched,
        display_rank_scores: pool.display_rank_scores,
        search_ms: pool.search_ms,
        ce: pool.ce,
        cross_source_collapsed: pool.cross_source_collapsed,
        exhausted: pool.exhausted,
    })
}

/// Gather, fuse, sink and settle a pool wide enough for the first `need`
/// rows. Sinking happens over the whole pool BEFORE any cut, so the rows that
/// carry no answer give their slots to rows that do.
#[allow(clippy::too_many_arguments)]
async fn settled_pool(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    need: usize,
    min_score: f32,
    scope: &SearchProjectScope,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
    rerank_intent: &str,
    search_mode: SearchMode,
    rerank_mode: RecallRerankMode<'_>,
) -> Result<SettledPool> {
    // OVERFETCH (validity partition, issue 2; non-answer sinks): fetching
    // exactly `need` candidates lets one demoted or sunk top-N hit permanently
    // displace the valid N+1 candidate — it was never fetched, so the sink has
    // nothing to promote. Fetch extra, sink over the full set, cut at the end.
    let first = recall_pool(need);
    let memo = CallMemo::default();
    let mut pass = reflect_gather_pass(
        storage,
        search,
        query_vec,
        query,
        first,
        vector_pool(need),
        min_score,
        scope,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
        rerank_mode,
        need,
        &memo,
    )
    .await?;
    let mut search_ms = pass.search_ms;

    // ADAPTIVE REFETCH (single, bounded): when demotions and the same-text collapse
    // leave fewer than `need` valid distinct texts AND the first window came back full,
    // refetch ONCE at 10*need (hard cap) and repartition; if still short, accept.
    let valid = pass
        .enriched
        .iter()
        .filter(|e| !is_demote_channel(&pass.validity, &e.chunk.id));
    let (rows, distinct) = (valid.clone().count(), page::distinct_rows(valid));
    if distinct < need && pass.window_full {
        // Echoes left out of the first window, or copies the page shows once, took
        // places the page needs: read wider than the first window, not the 10x floor.
        let widened = if pass.echo_dropped > 0 || distinct < rows {
            first.saturating_mul(3)
        } else {
            0
        };
        let refetch = need.saturating_mul(10).max(widened);
        if refetch > first {
            pass = reflect_gather_pass(
                storage,
                search,
                query_vec,
                query,
                refetch,
                refetch,
                min_score,
                scope,
                partition_enabled,
                consumption_mode,
                active_forgetting,
                rerank_intent,
                search_mode,
                rerank_mode,
                need,
                &memo,
            )
            .await?;
            search_ms += pass.search_ms;
        }
    }

    let mut enriched = pass.enriched;
    let ids_before: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
    // Sink resolved chunks AND dream-verdict-demoted chunks BEFORE the limit
    // cut so stale results do not occupy slots that should go to
    // unresolved/non-demoted chunks ranked below them.
    settle_window(&mut enriched, storage, &pass.validity, active_forgetting);
    // Last ordering step: the caller's own live chat answers no past question.
    sink_live_session(&mut enriched, storage, query);
    // The same text in two conversations is one row, kept at the better place
    // it holds in the order the caller gets. Done last: a copy the sinks move
    // down (the live chat, a resolved or demoted row) must not have given the
    // places of the copies above it away.
    let same_text = collapse_same_text(&mut enriched);
    // The label reads the order the caller gets, not the one before the sinks.
    let ce = refresh_ce(pass.ce, pass.ce_inputs.as_ref(), &ids_before, &enriched);
    Ok(SettledPool {
        enriched,
        display_rank_scores: pass.display_rank_scores,
        search_ms,
        ce,
        exhausted: !pass.window_full,
        published: 0,
        cross_source_collapsed: Collapsed {
            same_text,
            ..pass.cross_source_collapsed
        },
    })
}

/// What the passes of one recall share. The adaptive refetch runs the gather
/// pass a second time over a wider window for the same query; a chunk already
/// scored in the first pass keeps its score instead of going through the
/// cross-encoder again.
#[derive(Default)]
struct CallMemo {
    ce: std::sync::Mutex<HashMap<String, f32>>,
}

impl CallMemo {
    /// Memo key of one (chunk id, pair text): the text is part of the key, so a
    /// row whose excerpt changed between passes is scored again.
    fn ce_key(id: &str, text: &str) -> String {
        let digest = blake3::hash(text.as_bytes()).to_hex();
        format!("{id}\u{0}{}", &digest.as_str()[..16])
    }

    /// One score per key, in order: memoised scores as they were, the others
    /// from `score` over their texts (one call, only when something is new).
    fn ce_scores<F>(&self, keys: &[String], texts: &[&str], score: F) -> Result<Vec<f32>>
    where
        F: FnOnce(&[&str]) -> Result<Vec<f32>>,
    {
        let held: Vec<Option<f32>> = {
            let memo = self.ce.lock().unwrap_or_else(|e| e.into_inner());
            keys.iter().map(|key| memo.get(key).copied()).collect()
        };
        let missing: Vec<usize> = (0..keys.len()).filter(|i| held[*i].is_none()).collect();
        super::stage_timing::count("ce_memo_hits", (keys.len() - missing.len()) as u64);
        if missing.is_empty() {
            return Ok(held.into_iter().flatten().collect());
        }
        let fresh_texts: Vec<&str> = missing.iter().map(|&i| texts[i]).collect();
        let fresh = score(&fresh_texts)?;
        anyhow::ensure!(
            fresh.len() == missing.len(),
            "scorer returned {} scores for {} units",
            fresh.len(),
            missing.len()
        );
        let mut memo = self.ce.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = held;
        for (&i, value) in missing.iter().zip(fresh) {
            memo.insert(keys[i].clone(), value);
            out[i] = Some(value);
        }
        Ok(out.into_iter().flatten().collect())
    }
}

/// Output of one [`reflect_gather_pass`]: candidates fully enriched,
/// reranked and deduped — everything up to (but NOT including) the
/// resolution/validity sink and the limit cut — so the caller can inspect
/// how many valid results the partition would leave and decide on the
/// single adaptive refetch before cutting.
struct GatherPass {
    enriched: Vec<EnrichedResult>,
    /// Effective rerank scores keyed only for candidates whose rank differs
    /// from pure raw-score order. Later structural partitions do not alter it.
    display_rank_scores: Vec<DisplayRankScore>,
    validity: HashMap<String, ConvValidity>,
    /// The HNSW window came back full — more candidates may exist beyond it,
    /// so a refetch could backfill demotion-vacated slots.
    window_full: bool,
    /// Rows the echo filter left out of this pass (CSR's own output, logged
    /// copies of the query). They cost the page places the window may have to
    /// be widened to refill.
    echo_dropped: usize,
    search_ms: u64,
    /// Cross-encoder summary over the pooled sessions (hybrid, CE policy,
    /// model available); `None` otherwise.
    ce: Option<format::CeSummary>,
    /// Rows dropped because another row has the same text.
    cross_source_collapsed: Collapsed,
    /// What `ce` was read from, kept so the summary can be read again from a
    /// later order: the cross-encoder score of each session, and the session
    /// of each candidate. `None` whenever `ce` is.
    ce_inputs: Option<CeInputs>,
}

/// The cross-encoder scores behind a [`format::CeSummary`], see
/// `GatherPass::ce_inputs`.
#[derive(Clone)]
struct CeInputs {
    session_scores: HashMap<String, f32>,
    session_of_chunk: HashMap<String, String>,
}

/// The cross-encoder summary for the order a pass finally returns. When the
/// sinks after the pass moved rows, `ce` (read from the order before them)
/// would label the new first row with the old first row's scores; this reads
/// them again from `enriched`. With the order unchanged it is `ce` itself.
fn refresh_ce(
    ce: Option<format::CeSummary>,
    inputs: Option<&CeInputs>,
    ids_before: &[String],
    enriched: &[EnrichedResult],
) -> Option<format::CeSummary> {
    let Some(inputs) = inputs else {
        return ce;
    };
    if enriched.len() == ids_before.len()
        && enriched
            .iter()
            .zip(ids_before)
            .all(|(row, id)| row.chunk.id == *id)
    {
        return ce;
    }
    let final_sessions: Vec<&str> = enriched
        .iter()
        .map(|e| {
            inputs
                .session_of_chunk
                .get(&e.chunk.id)
                .map_or(e.chunk.id.as_str(), String::as_str)
        })
        .collect();
    final_ce_summary(&final_sessions, &inputs.session_scores)
}

/// Compute the exact score used by recall reranking, including its conversation
/// primacy bonus. `search::rerank` intentionally returns candidates rather than
/// a scored wrapper, so the display path reconstructs the same pure policy here
/// while keeping the ranking implementation itself out of the response model.
fn recall_display_rank_scores(
    candidates: &[crate::search::rerank::RankCandidate],
) -> HashMap<String, f32> {
    // Keep these in lockstep with search::rerank's private recall constants.
    const PRIMACY_BAND: f32 = 0.05;
    const PRIMACY_BOOST: f32 = 0.15;

    let eligible = |candidate: &&crate::search::rerank::RankCandidate| {
        candidate.timestamp.is_some()
            && candidate.min_trust >= crate::provenance::TrustTier::UserHistory
            && candidate.provenance.is_some()
            && !crate::search::rerank::is_scaffold_text(&candidate.content)
    };
    let top_eligible = candidates
        .iter()
        .filter(eligible)
        .map(|candidate| candidate.cosine)
        .fold(f32::MIN, f32::max);
    let primacy_conversation = candidates
        .iter()
        .filter(eligible)
        .filter(|candidate| candidate.cosine >= top_eligible - PRIMACY_BAND)
        .filter_map(|candidate| {
            Some((
                candidate.timestamp.as_deref()?,
                candidate.provenance.as_ref()?.source_conv_id.as_str(),
            ))
        })
        .min_by(|left, right| left.0.cmp(right.0))
        .map(|(_, conversation_id)| conversation_id);

    candidates
        .iter()
        .map(|candidate| {
            let primacy_bonus = if candidate.provenance.as_ref().is_some_and(|provenance| {
                candidate.min_trust >= crate::provenance::TrustTier::UserHistory
                    && Some(provenance.source_conv_id.as_str()) == primacy_conversation
            }) {
                PRIMACY_BOOST
            } else {
                0.0
            };
            (
                candidate.id.clone(),
                crate::search::rerank::adjusted_score(candidate) + primacy_bonus,
            )
        })
        .collect()
}

#[cfg(test)]
thread_local! {
    /// Test-only policy override: parallel tests must not race on the process
    /// environment, so they set the policy on their own thread instead.
    static POLICY_OVERRIDE: std::cell::Cell<Option<RetrievalPolicy>> =
        const { std::cell::Cell::new(None) };
}

/// The retrieval policy for one recall call. The single place the MCP recall
/// path reads `CSR_RETRIEVAL_LEGACY` (through [`RetrievalPolicy::from_env`]).
///
/// Under `cfg(test)` the default is [`RetrievalPolicy::Legacy`] unless the
/// test's own thread opts in with `POLICY_OVERRIDE`: the pipeline tests pin
/// ordering on synthetic content, and the cross-encoder would otherwise load
/// whatever model files happen to sit on the machine running them.
fn current_retrieval_policy() -> RetrievalPolicy {
    #[cfg(test)]
    {
        POLICY_OVERRIDE
            .with(|cell| cell.get())
            .unwrap_or(RetrievalPolicy::Legacy)
    }
    #[cfg(not(test))]
    {
        RetrievalPolicy::from_env()
    }
}

/// Sessions whose representative must be in the cross-encoder pool: the S1
/// collapsed top of the list.
const CE_PROTECTED_SESSIONS: usize = 5;

/// Longest unit text (characters) handed to the cross-encoder; its tokenizer
/// truncates to 512 tokens anyway, this only bounds tokenizing a huge chunk.
const CE_UNIT_MAX_CHARS: usize = 4000;

/// The text the cross-encoder reads for one unit: `[<role>, <YYYY-MM-DD>] `
/// when the chunk carries a role and a parseable timestamp (either alone when
/// only one is there), then the chunk content without the keyword-fallback
/// marker.
fn ce_pair_text(chunk: &crate::import::ConversationChunk) -> String {
    let role = chunk.author.as_str();
    let date = temporal::parse_timestamp(&chunk.timestamp)
        .map(|timestamp| timestamp.format("%Y-%m-%d").to_string());
    let header = match (role.is_empty(), date) {
        (false, Some(date)) => format!("[{role}, {date}] "),
        (false, None) => format!("[{role}] "),
        (true, Some(date)) => format!("[{date}] "),
        (true, None) => String::new(),
    };
    let content = chunk
        .content
        .strip_prefix("[keyword] ")
        .unwrap_or(&chunk.content);
    format!(
        "{header}{}",
        format::truncate_chars(content, CE_UNIT_MAX_CHARS)
    )
}

#[path = "tools_ce_pool.rs"]
mod ce_pool;
use ce_pool::{ce_pool_ids, log_ce_rows, log_ce_scores};

/// Order produced by [`ce_reorder`].
#[derive(Debug, PartialEq)]
struct CeOrdered {
    /// One chunk id per session: pooled sessions by cross-encoder score, then
    /// every other session in its S1 place.
    order: Vec<String>,
    /// The pooled sessions' scores, best first.
    session_scores: Vec<f32>,
    /// The same scores by session key, for the final-list summary.
    by_session: HashMap<String, f32>,
}

/// Re-order the S1 collapsed list (one representative chunk id per session)
/// by cross-encoder score. `scored` holds one row per pool chunk:
/// `(chunk_id, session_key, answer_class, ce_score)`. A session's score is the
/// best score among its rows of the lowest answer class present (so an answer
/// row, class 0, speaks for the session whenever it has one), and the session
/// is represented by that row. Ties go to the S1 order. A session with no
/// finite score keeps its S1 place after the pooled ones. Pure: no model.
fn ce_reorder(
    collapsed: &[String],
    session_keys: &HashMap<String, String>,
    scored: &[(String, String, u8, f32)],
) -> CeOrdered {
    let session_of = |id: &str| {
        session_keys
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    };
    let mut s1_place: HashMap<String, usize> = HashMap::new();
    for (place, id) in collapsed.iter().enumerate() {
        s1_place.entry(session_of(id)).or_insert(place);
    }

    let mut sessions: Vec<&str> = Vec::new();
    let mut best: HashMap<&str, usize> = HashMap::new();
    for (row, (_, session, class, score)) in scored.iter().enumerate() {
        if !score.is_finite() {
            continue;
        }
        match best.get(session.as_str()).copied() {
            None => {
                sessions.push(session.as_str());
                best.insert(session.as_str(), row);
            }
            Some(held) => {
                let (_, _, held_class, held_score) = &scored[held];
                if class < held_class
                    || (class == held_class && score.total_cmp(held_score).is_gt())
                {
                    best.insert(session.as_str(), row);
                }
            }
        }
    }
    sessions.sort_by(|left, right| {
        let (left_score, right_score) = (scored[best[left]].3, scored[best[right]].3);
        right_score.total_cmp(&left_score).then_with(|| {
            let place = |session: &str| s1_place.get(session).copied().unwrap_or(usize::MAX);
            place(left).cmp(&place(right))
        })
    });

    let mut order: Vec<String> = sessions
        .iter()
        .map(|session| scored[best[session]].0.clone())
        .collect();
    let session_scores: Vec<f32> = sessions
        .iter()
        .map(|session| scored[best[session]].3)
        .collect();
    for id in collapsed {
        if !best.contains_key(session_of(id).as_str()) {
            order.push(id.clone());
        }
    }
    let by_session: HashMap<String, f32> = sessions
        .iter()
        .map(|session| ((*session).to_string(), scored[best[session]].3))
        .collect();
    CeOrdered {
        order,
        session_scores,
        by_session,
    }
}

/// A content chunk may replace the cross-encoder's pick only when the
/// cross-encoder scored it within this many logits of the pick: a near tie.
/// A tool row that clearly outscores the session's content chunk is the better
/// answer (a memory file read beats the prompt that asked about it), and the
/// swap would hide it. Measured on the 12 probes (F1a): the swapped-out rows
/// that held the answer led the content chunk by 0.5 to 3.8 logits.
const CE_REPRESENTATIVE_TIE: f32 = 0.25;

/// [`ce_reorder`], then the content-representative rule once more: the
/// cross-encoder may pick a session's tool-output row, and when the pool's
/// content chunk scored a near tie ([`CE_REPRESENTATIVE_TIE`]) the page shows
/// the content chunk instead (the rule `apply_content_representatives`
/// applies before the cross-encoder). Session order and scores stay as the
/// cross-encoder computed them; only the displayed row is swapped. A content
/// chunk the cross-encoder did not score, or scored clearly lower, never
/// replaces its pick.
fn ce_reorder_with_representatives(
    collapsed: &[String],
    session_keys: &HashMap<String, String>,
    scored: &[(String, String, u8, f32)],
    pool: &HashMap<String, Vec<PoolChunk>>,
) -> CeOrdered {
    let mut ordered = ce_reorder(collapsed, session_keys, scored);
    let picked = ordered.order.clone();
    apply_content_representatives(&mut ordered.order, session_keys, pool);
    let score_of = |id: &str| {
        scored
            .iter()
            .find(|row| row.0 == id)
            .map(|row| row.3)
            .filter(|score| score.is_finite())
    };
    for (slot, pick) in ordered.order.iter_mut().zip(&picked) {
        if slot == pick {
            continue;
        }
        let near_tie = match (score_of(pick), score_of(slot)) {
            (Some(pick_score), Some(content_score)) => {
                content_score + CE_REPRESENTATIVE_TIE >= pick_score
            }
            _ => false,
        };
        if !near_tie {
            slot.clone_from(pick);
        }
    }
    ordered
}

/// Status-intent queries ("current state of X", "what is left") ask for the
/// newest report on X. After the cross-encoder, the eligible candidates of the
/// first `limit` rows whose score is within [`CE_REPRESENTATIVE_TIE`] of the
/// best eligible one trade slots so the newest session leads. Eligible means a
/// user or assistant chunk (stored author; tool output and boilerplate never
/// qualify) that carries the query subject and is not an echo of the query.
/// Any other query, an unscored session, a missing timestamp or a clear winner
/// leaves `order` exactly as it came.
fn apply_status_recency(
    storage: &Storage,
    query: &str,
    order: Vec<String>,
    enriched: &[EnrichedResult],
    session_keys: &HashMap<String, String>,
    ce_scores: &HashMap<String, f32>,
    limit: usize,
) -> Vec<String> {
    use crate::search::status_intent::{
        is_status_query, matches_subject, reorder_near_ties, subject_stems, StatusCandidate,
    };
    if limit == 0 || ce_scores.is_empty() || !is_status_query(query) {
        return order;
    }
    let stems = subject_stems(query);
    if stems.is_empty() {
        return order;
    }
    let top: Vec<String> = order.iter().take(limit).cloned().collect();
    let by_id: HashMap<&str, &EnrichedResult> = enriched
        .iter()
        .map(|result| (result.chunk.id.as_str(), result))
        .collect();
    let authors = storage
        .with_connection(|conn| crate::storage::queries::chunk_authors(conn, &top))
        .unwrap_or_default();
    let mut candidates: HashMap<String, StatusCandidate> = HashMap::new();
    for id in &top {
        let Some(row) = by_id.get(id.as_str()) else {
            continue;
        };
        let report = matches!(
            authors.get(id),
            Some(crate::provenance::Speaker::User | crate::provenance::Speaker::Assistant)
        );
        candidates.insert(
            id.clone(),
            StatusCandidate {
                score: session_keys
                    .get(id.as_str())
                    .and_then(|session| ce_scores.get(session))
                    .copied(),
                when: chrono::DateTime::parse_from_rfc3339(&row.chunk.timestamp)
                    .ok()
                    .map(|time| time.timestamp()),
                eligible: report
                    && non_answer_class(&row.chunk.content, query) == 0
                    && matches_subject(&row.chunk.content, &stems),
            },
        );
    }
    reorder_near_ties(&order, &candidates, limit, CE_REPRESENTATIVE_TIE)
}

/// Sessions a status query admits new report chunks for, and report chunks
/// admitted per session.
const STATUS_POOL_SESSIONS: usize = 3;
const STATUS_POOL_PER_SESSION: usize = 3;

/// What the status-intent pool added to a call.
#[derive(Debug, Default)]
struct StatusAdmission {
    /// Chunk ids added to the cross-encoder pool beyond the fused window. They
    /// have rows in `enriched`; the page limit does not count them.
    admitted: Vec<String>,
    /// Matched session key -> its newest eligible report chunk.
    picks: HashMap<String, String>,
}

/// The session rows the status rules read, one per chunk row with a stored
/// author. `report` marks a stored user or assistant chunk that is not an echo
/// of the query; tool output, reflections and episodes (no stored author)
/// never are.
fn status_session_row(
    storage: &Storage,
    result: &EnrichedResult,
    author: Option<crate::provenance::Speaker>,
    query: &str,
) -> crate::search::status_intent::SessionRow {
    crate::search::status_intent::SessionRow {
        id: result.chunk.id.clone(),
        session: session_key_of(storage, &result.chunk),
        content: result.chunk.content.clone(),
        when: chrono::DateTime::parse_from_rfc3339(&result.chunk.timestamp)
            .ok()
            .map(|time| time.timestamp()),
        seq: result.chunk.seq,
        report: matches!(
            author,
            Some(crate::provenance::Speaker::User | crate::provenance::Speaker::Assistant)
        ) && non_answer_class(&result.chunk.content, query) == 0,
    }
}

/// Status-intent pool admission (status queries only; any other query returns
/// the default and touches nothing). The sessions of the first-stage rows that
/// carry the subject, at most [`STATUS_POOL_SESSIONS`], get their newest report
/// chunks read straight from the table. Those the first stage did not hold go
/// through the echo and synthetic/artefact filters and join `enriched` as
/// keyword-lane rows, ready for the cross-encoder pool. Each matched session
/// is then represented by its newest eligible report chunk.
#[allow(clippy::too_many_arguments)]
fn status_pool_admission(
    storage: &Arc<Storage>,
    query: &str,
    enriched: &mut Vec<EnrichedResult>,
    chunk_fused: &[String],
    provenance: &HashMap<String, Option<crate::provenance::ChunkProvenance>>,
    signals: &mut CandidateSignals,
    scope: &SearchProjectScope,
    active_forgetting: bool,
    policy: RetrievalPolicy,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    now: &chrono::DateTime<chrono::Utc>,
) -> StatusAdmission {
    use crate::search::status_intent::{
        is_status_query, matched_sessions, min_subject_hits, newest_eligible, subject_stems,
    };
    if policy != RetrievalPolicy::CrossEncoder || !is_status_query(query) {
        return StatusAdmission::default();
    }
    let stems = subject_stems(query);
    if stems.is_empty() {
        return StatusAdmission::default();
    }
    let started = Instant::now();
    let mut authors: HashMap<String, crate::provenance::Speaker> = provenance
        .iter()
        .filter_map(|(id, row)| row.as_ref().map(|row| (id.clone(), row.author)))
        .collect();
    let by_id: HashMap<&str, &EnrichedResult> = enriched
        .iter()
        .map(|result| (result.chunk.id.as_str(), result))
        .collect();
    let first_stage: Vec<crate::search::status_intent::SessionRow> = chunk_fused
        .iter()
        .filter_map(|id| by_id.get(id.as_str()))
        .map(|result| {
            status_session_row(
                storage,
                result,
                authors.get(&result.chunk.id).copied(),
                query,
            )
        })
        .collect();
    let matched = matched_sessions(&first_stage, &stems, STATUS_POOL_SESSIONS);
    if matched.is_empty() {
        super::stage_timing::stage("status_pool", started);
        return StatusAdmission::default();
    }
    let held: HashSet<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
    let newest = storage
        .with_connection(|conn| {
            crate::storage::status_pool::newest_report_chunks(
                conn,
                &matched,
                STATUS_POOL_PER_SESSION,
                &stems,
                min_subject_hits(&stems),
            )
        })
        .unwrap_or_default();
    let new_ids: Vec<String> = newest
        .into_iter()
        .map(|(id, _, _)| id)
        .filter(|id| !held.contains(id))
        .collect();
    let mut admitted: Vec<String> = Vec::new();
    if !new_ids.is_empty() {
        let chunks = storage.get_chunks_by_ids(&new_ids).unwrap_or_default();
        let id_refs: Vec<&str> = chunks.iter().map(|chunk| chunk.id.as_str()).collect();
        let found = storage
            .get_chunk_provenance_many(&id_refs)
            .unwrap_or_default();
        let new_provenance: HashMap<String, Option<crate::provenance::ChunkProvenance>> = chunks
            .iter()
            .map(|chunk| (chunk.id.clone(), found.get(&chunk.id).cloned()))
            .collect();
        let mut fresh: Vec<EnrichedResult> = chunks
            .into_iter()
            .map(|chunk| EnrichedResult {
                score: 0.0,
                chunk,
                resolution: None,
                trust: crate::provenance::TrustTier::Unknown,
                validity_demoted: false,
            })
            .collect();
        // Harness boilerplate is not a report and never enters the pool.
        fresh
            .retain(|result| !crate::search::status_intent::is_harness_text(&result.chunk.content));
        // The same echo and synthetic/artefact filters the first stage passed.
        let no_tags: HashMap<String, Vec<String>> = HashMap::new();
        drop_echo_rows(&mut fresh, &new_provenance, &no_tags, query);
        drop_synthetic_rows(storage, &mut fresh, &new_provenance, &no_tags);
        let kept: Vec<crate::import::ConversationChunk> =
            fresh.iter().map(|result| result.chunk.clone()).collect();
        signals.extend(storage, &kept, partition_enabled, consumption_mode);
        for result in fresh {
            let chunk = result.chunk;
            let (fts_score, _) = score_fts_candidate(
                &chunk,
                now,
                &signals.validity,
                signals.ancestry.get(&chunk.conversation_id),
                active_forgetting,
                policy,
            );
            if let Some(Some(row)) = new_provenance.get(&chunk.id) {
                authors.insert(chunk.id.clone(), row.author);
            }
            admitted.push(chunk.id.clone());
            enriched.push(EnrichedResult {
                score: fts_score * project_scope_multiplier(&chunk.project_name, scope),
                chunk: crate::import::ConversationChunk {
                    content: format!("[keyword] {}", chunk.content),
                    ..chunk
                },
                resolution: None,
                trust: crate::provenance::TrustTier::Unknown,
                validity_demoted: false,
            });
        }
    }
    let rows: Vec<crate::search::status_intent::SessionRow> = enriched
        .iter()
        .map(|result| {
            status_session_row(
                storage,
                result,
                authors.get(&result.chunk.id).copied(),
                query,
            )
        })
        .collect();
    let picks: HashMap<String, String> = matched
        .iter()
        .filter_map(|session| {
            newest_eligible(&rows, session, &stems).map(|id| (session.clone(), id))
        })
        .collect();
    super::stage_timing::count("status_pool_admitted", admitted.len() as u64);
    super::stage_timing::stage("status_pool", started);
    StatusAdmission { admitted, picks }
}

/// Show each matched session by its newest eligible report chunk. A pick that
/// was superseded (`sunk`) never stands. Order and length stay as they came;
/// only the displayed chunk of a matched session changes.
fn apply_status_picks(
    order: Vec<String>,
    session_keys: &HashMap<String, String>,
    picks: &HashMap<String, String>,
    sunk: &HashSet<String>,
) -> Vec<String> {
    if picks.is_empty() {
        return order;
    }
    order
        .into_iter()
        .map(
            |id| match session_keys.get(&id).and_then(|s| picks.get(s)) {
                Some(pick) if !sunk.contains(pick) => pick.clone(),
                _ => id,
            },
        )
        .collect()
}

/// Key of a chunk's text for the cross-source dedupe: whitespace collapsed,
/// lower-cased, the keyword-lane marker removed, hashed.
fn cross_source_key(content: &str) -> [u8; 32] {
    let text = content.strip_prefix("[keyword] ").unwrap_or(content);
    let normalized = text.to_lowercase();
    let collapsed = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    *blake3::hash(collapsed.as_bytes()).as_bytes()
}

/// Leave a Codex rollout row out of `enriched` when a Claude session row
/// (stored source `conversation` or `sidechain`) carries the same normalized
/// text; the Claude row is the origin and keeps the row, the same way a parent
/// session beats its sidechain. When the Codex copy ranked higher, the origin
/// row takes that place, so the text keeps its best rank. Runs before the page
/// cut, so the freed slots refill from the pool. Returns how many rows it left out. A row whose
/// source is unknown never collapses.
fn collapse_cross_source_duplicates(
    enriched: &mut Vec<EnrichedResult>,
    source_of: impl Fn(&str) -> Option<String>,
) -> usize {
    let mut groups: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
    for (index, row) in enriched.iter().enumerate() {
        if row.chunk.content.trim().is_empty() {
            continue;
        }
        groups
            .entry(cross_source_key(&row.chunk.content))
            .or_default()
            .push(index);
    }
    let mut dropped: HashSet<usize> = HashSet::new();
    for members in groups.values().filter(|members| members.len() > 1) {
        let sources: Vec<Option<String>> = members
            .iter()
            .map(|index| source_of(&enriched[*index].chunk.id))
            .collect();
        let is_origin = |source: &Option<String>| {
            matches!(source.as_deref(), Some("conversation" | "sidechain"))
        };
        let Some(keeper) = members
            .iter()
            .zip(&sources)
            .filter(|(_, source)| is_origin(source))
            .map(|(index, _)| *index)
            .min()
        else {
            continue;
        };
        let mut codex: Vec<usize> = members
            .iter()
            .zip(&sources)
            .filter(|(_, source)| source.as_deref() == Some("codex_rollout"))
            .map(|(index, _)| *index)
            .collect();
        let Some(first) = codex.iter().copied().min() else {
            continue;
        };
        // The text ranked at the Codex row's place: the origin row takes that
        // place and its score, and the Codex row goes to the origin's old one.
        if first < keeper {
            let placed = enriched[first].score;
            enriched.swap(first, keeper);
            if enriched[first].score < placed {
                enriched[first].score = placed;
            }
            for index in codex.iter_mut().filter(|index| **index == first) {
                *index = keeper;
            }
        }
        dropped.extend(codex);
    }
    if dropped.is_empty() {
        return 0;
    }
    let mut index = 0;
    enriched.retain(|_| {
        let keep = !dropped.contains(&index);
        index += 1;
        keep
    });
    dropped.len()
}

/// Warn about a cross-encoder failure once per process, not once per query.
fn warn_ce_unavailable(reason: &str) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!("cross-encoder rerank skipped, keeping the S1 order: {reason}");
    }
}

/// A result row for an episode reached by the episode channel. It stands for
/// its session only when the session has no chunk of its own in the pool.
fn episode_row(
    hit: &crate::search::episodes::EpisodeHit,
    now: &chrono::DateTime<chrono::Utc>,
) -> EnrichedResult {
    EnrichedResult {
        score: crate::search::hybrid::keyword_only_score(&hit.timestamp, now),
        chunk: crate::import::ConversationChunk {
            id: hit.id.clone(),
            conversation_id: hit.session.clone(),
            project_name: hit.project.clone(),
            timestamp: hit.timestamp.clone(),
            content: format!("[keyword] {}", crate::search::episodes::excerpt(&hit.text)),
            message_count: 1,
            summary: Some(format!("{}{}", crate::search::episodes::EID_MARK, hit.id)),
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: hit.session.starts_with("agent-"),
        },
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    }
}

/// One chunk of a session's candidate pool: its fused score and whether its
/// stored author marks it as tool output.
#[derive(Debug, Clone, PartialEq)]
struct PoolChunk {
    id: String,
    score: f64,
    boilerplate: bool,
}

/// The chunk that stands for a session. `current` is the collapse's pick. When
/// it is tool output (stored author `tool_result`) and the pool holds a content
/// chunk of the session, the highest-scored content chunk stands instead;
/// otherwise `current` keeps the place. Decided by stored metadata only. An
/// unknown chunk, or a pool without content, abstains.
fn session_representative(current: &str, pool: &[PoolChunk]) -> String {
    let current_is_boilerplate = pool.iter().any(|c| c.id == current && c.boilerplate);
    if !current_is_boilerplate {
        return current.to_string();
    }
    pool.iter()
        .filter(|c| !c.boilerplate)
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .map(|c| c.id.clone())
        .unwrap_or_else(|| current.to_string())
}

/// Swap, in place, each session's collapsed pick for [`session_representative`].
/// `pool` maps a session key to its candidate chunks; `eligible` says whether a
/// chunk may stand for the page (it has a row and shares the pick's standing).
fn apply_content_representatives(
    collapsed: &mut [String],
    session_keys: &HashMap<String, String>,
    pool: &HashMap<String, Vec<PoolChunk>>,
) {
    for slot in collapsed.iter_mut() {
        let Some(session) = session_keys.get(slot.as_str()) else {
            continue;
        };
        let Some(chunks) = pool.get(session) else {
            continue;
        };
        *slot = session_representative(slot, chunks);
    }
}

/// Build the candidate pool for [`apply_content_representatives`] from the
/// chunk-level fused order. Only chunks that have a row, are not episodes, and
/// whose standing is at least the pick's can stand for a session. Provenance
/// authors are read once; a chunk with no known author is content.
fn representative_pool(
    storage: &Storage,
    chunk_fused: &[String],
    enriched: &[EnrichedResult],
    session_keys: &HashMap<String, String>,
    answer_class: &HashMap<String, u8>,
    collapsed: &[String],
) -> HashMap<String, Vec<PoolChunk>> {
    let have_row: HashSet<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    let mut best_class: HashMap<&str, u8> = HashMap::new();
    for id in collapsed {
        if let Some(session) = session_keys.get(id.as_str()) {
            best_class.insert(
                session.as_str(),
                answer_class.get(id.as_str()).copied().unwrap_or(0),
            );
        }
    }
    let wanted: Vec<String> = chunk_fused
        .iter()
        .filter(|id| have_row.contains(id.as_str()))
        .filter(|id| {
            session_keys
                .get(id.as_str())
                .is_some_and(|s| best_class.contains_key(s.as_str()))
        })
        .cloned()
        .collect();
    let authors = storage
        .with_connection(|conn| crate::storage::queries::chunk_authors(conn, &wanted))
        .unwrap_or_default();
    let mut pool: HashMap<String, Vec<PoolChunk>> = HashMap::new();
    for (position, id) in wanted.iter().enumerate() {
        let session = &session_keys[id.as_str()];
        let class = answer_class.get(id.as_str()).copied().unwrap_or(0);
        if class > best_class[session.as_str()] {
            continue;
        }
        pool.entry(session.clone()).or_default().push(PoolChunk {
            id: id.clone(),
            score: 1.0 / (RRF_K + position) as f64,
            boilerplate: authors.get(id) == Some(&crate::provenance::Speaker::ToolResult),
        });
    }
    // The pick itself may sit outside the fused order's rows; it stays unknown
    // (content) in that case, which abstains.
    pool
}

/// Settle the sessions whose collapsed representative is an episode. When the
/// session has a chunk in the pool, that chunk (its best by chunk fusion) keeps
/// its id and carries the episode as its excerpt; otherwise the episode row
/// stands alone. Episode rows left without a session are dropped. Returns the
/// episode id to row id map, which the cross-encoder pool needs to score the
/// excerpt a session actually shows.
fn settle_episode_excerpts(
    enriched: &mut Vec<EnrichedResult>,
    collapsed: &mut [String],
    session_keys: &HashMap<String, String>,
    chunk_fused: &[String],
    hits: &[crate::search::episodes::EpisodeHit],
) -> HashMap<String, String> {
    let by_id: HashMap<&str, &crate::search::episodes::EpisodeHit> =
        hits.iter().map(|hit| (hit.id.as_str(), hit)).collect();
    let mut remap: HashMap<String, String> = HashMap::new();
    for slot in collapsed.iter_mut() {
        let Some(hit) = by_id.get(slot.as_str()) else {
            continue;
        };
        let session = session_keys.get(slot.as_str());
        let surrogate = chunk_fused
            .iter()
            .find(|id| {
                !by_id.contains_key(id.as_str())
                    && session_keys.get(id.as_str()) == session
                    && enriched.iter().any(|e| e.chunk.id == **id)
            })
            .cloned();
        let Some(surrogate) = surrogate else {
            continue;
        };
        if let Some(row) = enriched.iter_mut().find(|e| e.chunk.id == surrogate) {
            let keyword = row.chunk.content.starts_with("[keyword] ");
            row.chunk.content = format!(
                "{}{}",
                if keyword { "[keyword] " } else { "" },
                crate::search::episodes::excerpt(&hit.text)
            );
            row.chunk.summary = Some(format!("{}{}", crate::search::episodes::EID_MARK, hit.id));
        }
        remap.insert(hit.id.clone(), surrogate.clone());
        *slot = surrogate;
    }
    let kept: HashSet<&str> = collapsed.iter().map(String::as_str).collect();
    enriched
        .retain(|e| !by_id.contains_key(e.chunk.id.as_str()) || kept.contains(e.chunk.id.as_str()));
    remap
}

fn xml_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Give each `<r rank="N">` block its `session` (the conversation id) and, when
/// its excerpt is an episode, its `eid`. Rows are in page order.
fn annotate_sessions(mut xml: String, rows: &[EnrichedResult]) -> String {
    for (index, row) in rows.iter().enumerate() {
        let eid = row
            .chunk
            .summary
            .as_deref()
            .and_then(|summary| summary.strip_prefix(crate::search::episodes::EID_MARK))
            .map(|id| format!(" eid=\"{}\"", xml_attr(id)))
            .unwrap_or_default();
        let from = format!("    <r rank=\"{}\">\n", index + 1);
        let to = format!(
            "    <r rank=\"{}\" session=\"{}\"{}>\n",
            index + 1,
            xml_attr(&row.chunk.conversation_id),
            eid
        );
        xml = xml.replacen(&from, &to, 1);
    }
    xml
}

/// `CSR_NO_ECHO_FILTER=1` keeps every candidate in recall (the pre-filter
/// behaviour), for comparing runs.
fn echo_filter_enabled() -> bool {
    std::env::var("CSR_NO_ECHO_FILTER").map_or(true, |value| value != "1")
}

/// Leave out of `enriched` the transcript rows that are CSR's own output or a
/// logged copy of `query` ([`crate::search::echo::runtime_drop`]), and return
/// their ids. Stored reflections, stories and narratives are never judged here:
/// they are written on purpose, and the sinks further down order them.
fn drop_echo_rows(
    enriched: &mut Vec<EnrichedResult>,
    provenance: &HashMap<String, Option<crate::provenance::ChunkProvenance>>,
    reflection_tags: &HashMap<String, Vec<String>>,
    query: &str,
) -> HashSet<String> {
    let mut dropped = HashSet::new();
    if !echo_filter_enabled() {
        return dropped;
    }
    enriched.retain(|e| {
        if reflection_tags.contains_key(&e.chunk.id) {
            return true;
        }
        let author = provenance
            .get(&e.chunk.id)
            .and_then(|row| row.as_ref())
            .map(|row| row.author);
        let drop = crate::search::echo::runtime_drop(
            &e.chunk.content,
            author,
            &e.chunk.conversation_id,
            query,
        )
        .is_some();
        if drop {
            dropped.insert(e.chunk.id.clone());
        }
        !drop
    });
    dropped
}

/// `CSR_NO_RECALL_LANES=1` runs every read of a recall pass on the one
/// connection, in sequence, as it did before the keyword and verdict reads were
/// overlapped.
pub(crate) fn recall_lanes_enabled() -> bool {
    std::env::var("CSR_NO_RECALL_LANES").map_or(true, |value| value != "1")
}

/// The keyword reads of one recall pass: the rows that depend on the query and
/// the window size alone.
struct KeywordLegs {
    postings_hits: Vec<(String, f64)>,
    postings_weight: f64,
    keyword: crate::search::hybrid::KeywordChunkHits,
    reflection_fts_hits: Vec<(String, usize, f64)>,
    reflection_prefix_ids: Vec<String>,
    /// Milliseconds each read took, by stage name (read on the lane thread).
    leg_ms: Vec<(&'static str, f64)>,
}

/// Run the keyword reads of a recall pass on `storage`: the identifier
/// postings (hybrid only), the chunk and reflection bm25 rankings and the
/// exact-prefix reflections. Each read fails open to an empty result.
fn keyword_legs(
    storage: &Storage,
    query: &str,
    fetch: usize,
    search_mode: SearchMode,
    family: Option<&[&str]>,
) -> KeywordLegs {
    read_keyword_legs(storage, query, fetch, search_mode, family, false)
        .expect("a fail-open read always answers")
}

/// [`keyword_legs`] on a lane connection: `None` when any read failed, so the
/// caller reruns the legs on the main connection instead of taking a lane's
/// transient error for "no keyword matches".
fn keyword_legs_strict(
    storage: &Storage,
    query: &str,
    fetch: usize,
    search_mode: SearchMode,
    family: Option<&[&str]>,
) -> Option<KeywordLegs> {
    read_keyword_legs(storage, query, fetch, search_mode, family, true)
}

/// One read of a leg: its rows, an empty result when a failure is tolerated, or
/// `None` when it is not (`strict`).
fn leg_rows<T: Default>(read: anyhow::Result<T>, strict: bool) -> Option<T> {
    match read {
        Ok(rows) => Some(rows),
        Err(_) if !strict => Some(T::default()),
        Err(_) => None,
    }
}

fn read_keyword_legs(
    storage: &Storage,
    query: &str,
    fetch: usize,
    search_mode: SearchMode,
    family: Option<&[&str]>,
    strict: bool,
) -> Option<KeywordLegs> {
    let mut postings_hits: Vec<(String, f64)> = Vec::new();
    let mut postings_weight = 0.0_f64;
    // Empty (no allocation) unless stage timing is on.
    let timing = super::stage_timing::enabled();
    let mut leg_ms: Vec<(&'static str, f64)> = Vec::new();
    if search_mode == SearchMode::Hybrid {
        let idents = crate::search::identifiers::analyze_query(query);
        if !idents.is_empty() {
            let sub_start = Instant::now();
            postings_hits = leg_rows(storage.search_postings(&idents, fetch, family), strict)?;
            if timing {
                leg_ms.push(("f_postings", sub_start.elapsed().as_secs_f64() * 1000.0));
            }
            if !postings_hits.is_empty() {
                postings_weight = id_channel_weight(&idents, &postings_term_dfs(storage, &idents));
            }
        }
    }
    let sub_start = Instant::now();
    let family_vec = family.map(<[&str]>::to_vec);
    let keyword = if strict {
        crate::search::hybrid::keyword_chunk_hits_strict(storage, query, fetch, family_vec)?
    } else {
        keyword_chunk_hits(storage, query, fetch, family_vec)
    };
    if timing {
        leg_ms.push(("f_chunks_fts", sub_start.elapsed().as_secs_f64() * 1000.0));
    }
    let sub_start = Instant::now();
    let reflection_fts_hits = leg_rows(storage.fts5_search_reflections(query, fetch), strict)?;
    if timing {
        leg_ms.push(("f_refl_fts", sub_start.elapsed().as_secs_f64() * 1000.0));
    }
    let sub_start = Instant::now();
    let reflection_prefix_ids =
        leg_rows(storage.fts5_reflection_prefix_matches(query, fetch), strict)?;
    if timing {
        leg_ms.push(("f_refl_prefix", sub_start.elapsed().as_secs_f64() * 1000.0));
    }
    Some(KeywordLegs {
        postings_hits,
        postings_weight,
        keyword,
        reflection_fts_hits,
        reflection_prefix_ids,
        leg_ms,
    })
}

/// Start [`keyword_legs`] on the blocking pool over a lane connection of its
/// own (a WAL reader, so it never waits on the main connection). `None` when
/// the pass has no keyword channel, lanes are off, or the store has no file; the
/// pass then runs the legs inline, which returns the same rows. The task yields
/// `None` when the lane would not open or any of its reads failed.
fn spawn_keyword_lane(
    storage: &Arc<Storage>,
    query: &str,
    fetch: usize,
    scope: &SearchProjectScope,
    search_mode: SearchMode,
) -> Option<tokio::task::JoinHandle<Option<KeywordLegs>>> {
    spawn_keyword_lane_with(storage, query, fetch, scope, search_mode, |_| {})
}

/// [`spawn_keyword_lane`] with a hook run on the lane connection before its
/// reads (the tests break the lane alone with it).
fn spawn_keyword_lane_with(
    storage: &Arc<Storage>,
    query: &str,
    fetch: usize,
    scope: &SearchProjectScope,
    search_mode: SearchMode,
    prepare: impl FnOnce(&Storage) + Send + 'static,
) -> Option<tokio::task::JoinHandle<Option<KeywordLegs>>> {
    if search_mode == SearchMode::Vector || !recall_lanes_enabled() {
        return None;
    }
    let storage = Arc::clone(storage);
    let query = query.to_string();
    let family: Option<Vec<String>> = scope
        .effective_project
        .is_some()
        .then(|| scope.family_projects.iter().cloned().collect());
    Some(tokio::task::spawn_blocking(move || {
        let lane = storage.open_read_lane().ok().flatten()?;
        prepare(&lane);
        let family: Option<Vec<&str>> = family
            .as_ref()
            .map(|projects| projects.iter().map(String::as_str).collect());
        keyword_legs_strict(&lane, &query, fetch, search_mode, family.as_deref())
    }))
}

/// The keyword legs of a pass: the lane's when it answered whole, otherwise the
/// same reads inline on the main connection.
async fn collect_keyword_legs(
    lane: Option<tokio::task::JoinHandle<Option<KeywordLegs>>>,
    storage: &Storage,
    query: &str,
    fetch: usize,
    search_mode: SearchMode,
    family: Option<&[&str]>,
) -> KeywordLegs {
    match lane {
        Some(handle) => handle.await.ok().flatten(),
        None => None,
    }
    .unwrap_or_else(|| keyword_legs(storage, query, fetch, search_mode, family))
}

/// Judge, on a lane connection of its own, the sessions of the semantic
/// candidates for the synthetic-content filter, so the filter finds their
/// verdicts cached when the keyword rows have arrived. The verdicts are the ones
/// the filter would compute itself, from the same scan.
fn spawn_verdict_prefetch(
    storage: &Arc<Storage>,
    chunks: &[crate::import::ConversationChunk],
) -> Option<tokio::task::JoinHandle<()>> {
    if chunks.is_empty() || !recall_lanes_enabled() || !super::synthetic::filter_enabled() {
        return None;
    }
    let storage = Arc::clone(storage);
    let conversations = distinct_conversation_ids_of_chunks(chunks);
    Some(tokio::task::spawn_blocking(move || {
        if let Ok(Some(lane)) = storage.open_read_lane() {
            let refs: Vec<&str> = conversations.iter().map(String::as_str).collect();
            super::synthetic::prefetch_verdicts(&lane, &refs);
        }
    }))
}

/// Leave out of `enriched` the transcript rows that are eval or synthetic
/// content by provenance ([`super::synthetic`]): rows of a bench/eval session
/// and the tool output of a bench/eval call. Returns their ids. Runs at the
/// echo filter's point, before any ranking. Stored reflections are never
/// judged here.
fn drop_synthetic_rows(
    storage: &Storage,
    enriched: &mut Vec<EnrichedResult>,
    provenance: &HashMap<String, Option<crate::provenance::ChunkProvenance>>,
    reflection_tags: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    if !super::synthetic::filter_enabled() {
        return HashSet::new();
    }
    let candidates: Vec<super::synthetic::ChunkRef> = enriched
        .iter()
        .filter(|e| !reflection_tags.contains_key(&e.chunk.id))
        .map(|e| super::synthetic::ChunkRef {
            id: e.chunk.id.clone(),
            conversation_id: e.chunk.conversation_id.clone(),
            author: provenance
                .get(&e.chunk.id)
                .and_then(|row| row.as_ref())
                .map(|row| row.author),
        })
        .collect();
    let synthetic_start = Instant::now();
    let dropped = super::synthetic::synthetic_ids(storage, &candidates);
    super::stage_timing::stage("synthetic_filter", synthetic_start);
    if !dropped.is_empty() {
        enriched.retain(|e| !dropped.contains(&e.chunk.id));
    }
    dropped
}

/// Sink below every other row the transcript rows that are eval-rig material
/// by provenance ([`super::synthetic::rig_sunk_ids`]): a marked row of a
/// session that ran the bench/eval rig, or a row of such a session that
/// quotes the query. Stable, never dropping: a sunk row still answers when
/// nothing else matches. Stored reflections are never judged here.
fn sink_rig_rows(
    storage: &Storage,
    enriched: &mut [EnrichedResult],
    reflection_tags: &HashMap<String, Vec<String>>,
    query: &str,
) {
    if !super::synthetic::filter_enabled() {
        return;
    }
    let sink_start = Instant::now();
    let sunk = {
        let candidates: Vec<super::synthetic::SinkRef<'_>> = enriched
            .iter()
            .filter(|e| !reflection_tags.contains_key(&e.chunk.id))
            .map(|e| super::synthetic::SinkRef {
                id: &e.chunk.id,
                conversation_id: &e.chunk.conversation_id,
                content: &e.chunk.content,
            })
            .collect();
        super::synthetic::rig_sunk_ids(storage, &candidates, query)
    };
    super::stage_timing::stage("synthetic_sink", sink_start);
    if !sunk.is_empty() {
        enriched.sort_by_key(|e| sunk.contains(&e.chunk.id));
    }
}

/// The cross-encoder stage of hybrid recall: score the pool (first
/// [`CE_POOL`] fused chunks plus the S1 top sessions' representatives) and
/// return the new session order with its score summary. Fails open: with no
/// model or a scoring error the S1 order comes back unchanged, no summary.
#[allow(clippy::too_many_arguments)]
fn ce_rerank_stage(
    query: &str,
    collapsed: Vec<String>,
    fused: Vec<String>,
    enriched: &[EnrichedResult],
    session_keys: &HashMap<String, String>,
    answer_class: &HashMap<String, u8>,
    representatives: &HashMap<String, Vec<PoolChunk>>,
    admitted: &[String],
    memo: &CallMemo,
    daemon: Option<&crate::search::vector_ipc::VectorProxy>,
) -> (Vec<String>, HashMap<String, f32>) {
    if collapsed.is_empty() {
        return (collapsed, HashMap::new());
    }
    let ce_start = Instant::now();
    let result = ce_rerank_stage_inner(
        query,
        collapsed,
        fused,
        enriched,
        session_keys,
        answer_class,
        representatives,
        admitted,
        memo,
        daemon,
    );
    super::stage_timing::stage("ce", ce_start);
    result
}

#[allow(clippy::too_many_arguments)]
fn ce_rerank_stage_inner(
    query: &str,
    collapsed: Vec<String>,
    fused: Vec<String>,
    enriched: &[EnrichedResult],
    session_keys: &HashMap<String, String>,
    answer_class: &HashMap<String, u8>,
    representatives: &HashMap<String, Vec<PoolChunk>>,
    admitted: &[String],
    memo: &CallMemo,
    daemon: Option<&crate::search::vector_ipc::VectorProxy>,
) -> (Vec<String>, HashMap<String, f32>) {
    let by_id: HashMap<&str, &EnrichedResult> = enriched
        .iter()
        .map(|result| (result.chunk.id.as_str(), result))
        .collect();
    let fused: Vec<String> = fused
        .into_iter()
        .filter(|id| by_id.contains_key(id.as_str()))
        .collect();
    let protected: Vec<String> = collapsed
        .iter()
        .take(CE_PROTECTED_SESSIONS)
        .filter(|id| by_id.contains_key(id.as_str()))
        .cloned()
        .collect();
    let mut pool = ce_pool_ids(&fused, &protected, CE_POOL);
    // Status-intent admissions ride on top of the window (empty otherwise).
    for id in admitted {
        if by_id.contains_key(id.as_str()) && !pool.contains(id) {
            pool.push(id.clone());
        }
    }
    tracing::debug!(pool = ?pool, "cross-encoder pool (chunk ids)");
    // A server that forwards to the daemon scores there and never loads the
    // model: the model and its ORT arena would stay resident in every
    // server. Without a daemon the process scores for itself.
    let encoder = match daemon {
        Some(_) => None,
        None => {
            let Some(encoder) = cross_encoder::global() else {
                warn_ce_unavailable("model files unavailable");
                return (collapsed, HashMap::new());
            };
            Some(encoder)
        }
    };
    let texts: Vec<String> = pool
        .iter()
        .map(|id| ce_pair_text(&by_id[id.as_str()].chunk))
        .collect();
    let units: Vec<&str> = texts.iter().map(String::as_str).collect();
    let keys: Vec<String> = pool
        .iter()
        .zip(&texts)
        .map(|(id, text)| CallMemo::ce_key(id, text))
        .collect();
    let score_fresh = |fresh: &[&str]| -> Result<Vec<f32>> {
        match (daemon, encoder) {
            (Some(daemon), _) => {
                super::stage_timing::count("ce_pairs", fresh.len() as u64);
                daemon
                    .rerank(query, fresh)
                    .ok_or_else(|| anyhow::anyhow!("daemon rerank unavailable"))
            }
            (None, Some(encoder)) => encoder.score(query, fresh),
            (None, None) => anyhow::bail!("no cross-encoder"),
        }
    };
    let scores = match memo.ce_scores(&keys, &units, score_fresh) {
        Ok(scores) if scores.len() == pool.len() => scores,
        Ok(scores) => {
            warn_ce_unavailable(&format!(
                "scorer returned {} scores for {} units",
                scores.len(),
                pool.len()
            ));
            return (collapsed, HashMap::new());
        }
        Err(error) => {
            warn_ce_unavailable(&format!("{error:#}"));
            return (collapsed, HashMap::new());
        }
    };
    let scored: Vec<(String, String, u8, f32)> = pool
        .iter()
        .zip(scores)
        .map(|(id, score)| {
            (
                id.clone(),
                session_keys.get(id).cloned().unwrap_or_else(|| id.clone()),
                answer_class.get(id).copied().unwrap_or(0),
                score,
            )
        })
        .collect();
    let ordered =
        ce_reorder_with_representatives(&collapsed, session_keys, &scored, representatives);
    if tracing::enabled!(tracing::Level::DEBUG) {
        let picks = ce_reorder(&collapsed, session_keys, &scored).order;
        log_ce_scores(&scored);
        log_ce_rows(&ordered, &picks, session_keys, &scored, &by_id);
    }
    (ordered.order, ordered.by_session)
}

/// The cross-encoder summary of the list the caller finally shows, for the
/// relevance label. `final_sessions` is the session of each final row, best
/// first; `scores` the cross-encoder score per session. The top is the score of
/// the row that ended up first, the runner-up the next scored row, the median
/// over every scored row: the label reads the scores that order the output,
/// not the pool's best before later filters and sinks moved rows. `None` when
/// the first row has no finite score (not pooled, or the cross-encoder did not
/// run), where the caller keeps the cosine label.
fn final_ce_summary(
    final_sessions: &[&str],
    scores: &HashMap<String, f32>,
) -> Option<format::CeSummary> {
    let mut seen: HashSet<&str> = HashSet::new();
    let ranked: Vec<Option<f32>> = final_sessions
        .iter()
        .filter(|session| seen.insert(**session))
        .map(|session| scores.get(*session).copied().filter(|s| s.is_finite()))
        .collect();
    let top = (*ranked.first()?)?;
    let runner_up = ranked.iter().skip(1).flatten().next().copied();
    let all: Vec<f32> = ranked.iter().flatten().copied().collect();
    let median = format::CeSummary::from_scores(&all)?.median;
    Some(format::CeSummary {
        top,
        runner_up,
        median,
    })
}

/// One retrieval + enrichment pass for `reflect_on_past_with_vec`: HNSW
/// search (chunks + reflections) at window size `fetch`, TAD/decay scoring,
/// FTS5 fallback append (with validity re-resolved for FTS-only
/// conversations), provenance rerank, dedupe.
#[allow(clippy::too_many_arguments)]
async fn reflect_gather_pass(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    fetch: usize,
    vector_fetch: usize,
    min_score: f32,
    scope: &SearchProjectScope,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
    rerank_intent: &str,
    search_mode: SearchMode,
    rerank_mode: RecallRerankMode<'_>,
    page: usize,
    memo: &CallMemo,
) -> Result<GatherPass> {
    // The one read of CSR_RETRIEVAL_LEGACY for this call; everything below
    // takes the policy as a parameter.
    let policy = current_retrieval_policy();
    // The daemon this server reranks through, when it has one (see
    // `ce_rerank_stage_inner`).
    let rerank_daemon = search.read().await.proxy().cloned();
    let fetch = if search_mode == SearchMode::Fts {
        fetch.min(GET_MORE_WINDOW)
    } else {
        fetch
    };
    super::stage_timing::count("passes", 1);
    // The keyword legs read the query alone, so they start now on their own
    // connection and are collected where their rows are first needed.
    let lane_start = Instant::now();
    let mut keyword_lane = spawn_keyword_lane(storage, query, fetch, scope, search_mode);
    super::stage_timing::stage("p_lane_spawn", lane_start);
    let search_start = Instant::now();

    // Search BOTH chunks and reflections unless keyword-only ablation is active.
    let (chunk_results, reflection_results) = if search_mode == SearchMode::Fts {
        (Vec::new(), Vec::new())
    } else {
        let idx = search.read().await;
        let chunks = if scope.effective_project.is_some() {
            let mut ids = HashSet::new();
            for project in &scope.family_projects {
                ids.extend(storage.get_chunk_ids_for_project(project)?);
            }
            idx.search_chunks_filtered(query_vec, vector_fetch, min_score, &ids)
        } else {
            idx.search_chunks(query_vec, vector_fetch, min_score)
        };
        let reflections = idx.search_reflections(query_vec, vector_fetch, min_score);
        (chunks, reflections)
    };
    let search_ms = search_start.elapsed().as_millis() as u64;
    super::stage_timing::stage("hnsw", search_start);
    let decay_start = Instant::now();
    let mut window_full =
        chunk_results.len() == vector_fetch || reflection_results.len() == vector_fetch;

    // Enrich chunk results with metadata
    let chunk_ids: Vec<String> = chunk_results.iter().map(|r| r.id.clone()).collect();
    let sub_start = Instant::now();
    let chunks = storage.get_chunks_by_ids(&chunk_ids)?;
    super::stage_timing::stage("d_chunks_by_ids", sub_start);
    let mut verdict_lane = spawn_verdict_prefetch(storage, &chunks);

    if crate::mcp::stage_probe::is_active() {
        let chunk_conv: HashMap<&str, &str> = chunks
            .iter()
            .map(|chunk| (chunk.id.as_str(), chunk.conversation_id.as_str()))
            .collect();
        let ranked: Vec<(String, String, f32)> = chunk_results
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    chunk_conv
                        .get(r.id.as_str())
                        .map(|c| c.to_string())
                        .unwrap_or_default(),
                    r.score,
                )
            })
            .chain(
                reflection_results
                    .iter()
                    .map(|r| (r.id.clone(), String::new(), r.score)),
            )
            .collect();
        crate::mcp::stage_probe::record_stage("hnsw_fetch", &ranked);
    }

    // v10 dream-verdict validity partition: resolve ONCE, early — one
    // batched query for the whole semantic candidate set (perf requirement)
    // — so both the TAD/decay scoring below and the rerank step further down
    // can preserve the v10 no-stacking path for Demote-channel chunks when
    // active forgetting is off (see `apply_validity_partition`'s doc). `mut`
    // because the FTS fallback
    // below merges in verdicts for conversations the semantic pass never
    // saw. The final sink/annotate step (mirroring
    // `apply_resolutions_before_limit`) reuses this same map.
    let sub_start = Instant::now();
    let mut signals = CandidateSignals::load(storage, &chunks, partition_enabled, consumption_mode);
    super::stage_timing::stage("d_signals_load", sub_start);
    let queried_chunk_ids: HashSet<String> = chunks.iter().map(|chunk| chunk.id.clone()).collect();

    let now = chrono::Utc::now();

    // Batch-fetch TAD events for all chunk results (single DB query)
    let chunk_ids_for_tad: Vec<&str> = chunk_results.iter().map(|r| r.id.as_str()).collect();
    let sub_start = Instant::now();
    let tad_events = storage
        .get_retrieval_events_batch(&chunk_ids_for_tad)
        .unwrap_or_default();
    super::stage_timing::stage("d_tad_events", sub_start);
    let tad_config = decay::DecayConfig::for_search();

    let mut ancestry_applied_ids = HashSet::new();
    let mut enriched: Vec<EnrichedResult> = chunk_results
        .iter()
        .filter_map(|r| {
            chunks.iter().find(|c| c.id == r.id).map(|c| {
                let events = tad_events.get(&c.id).map(|v| v.as_slice()).unwrap_or(&[]);
                let (final_score, ancestry_applied) = score_chunk_candidate(
                    r.score,
                    c,
                    &now,
                    events,
                    &tad_config,
                    &signals.validity,
                    signals.ancestry.get(&c.conversation_id),
                    active_forgetting,
                    scope,
                    policy,
                );
                if ancestry_applied {
                    ancestry_applied_ids.insert(c.id.clone());
                }
                EnrichedResult {
                    score: final_score,
                    chunk: c.clone(),
                    resolution: None,
                    trust: crate::provenance::TrustTier::Unknown,
                    validity_demoted: false,
                }
            })
        })
        .collect();
    let semantic_count = enriched.len();

    // Batch-fetch TAD events for reflection results
    let reflection_ids_for_tad: Vec<&str> =
        reflection_results.iter().map(|r| r.id.as_str()).collect();
    let reflection_tad_events = storage
        .get_retrieval_events_batch(&reflection_ids_for_tad)
        .unwrap_or_default();

    // Enrich reflection results — convert to EnrichedResult using reflection
    // content. Tags are kept per id so the rerank below can tell explicit
    // memory writes from derived narratives (`reflection_recall_standing`).
    // Raw (pre-decay, pre-scope) cosine is kept alongside so that function
    // can gate the flat UserHistory standing by actual relevance instead of
    // handing it out to every legacy-shaped memory regardless of similarity.
    let mut reflection_tags: HashMap<String, Vec<String>> = HashMap::new();
    let mut reflection_raw_cosine: HashMap<String, f32> = HashMap::new();
    let reflection_rows = {
        let ids: Vec<&str> = reflection_results.iter().map(|r| r.id.as_str()).collect();
        storage.get_reflections_by_ids(&ids).unwrap_or_default()
    };
    for r in &reflection_results {
        reflection_raw_cosine.insert(r.id.clone(), r.score);
        if let Some((content, tags, timestamp)) = reflection_rows.get(&r.id).cloned() {
            let decayed_score = if let Some(ts) = crate::temporal::parse_timestamp(&timestamp) {
                let events = reflection_tad_events
                    .get(&r.id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                match policy {
                    RetrievalPolicy::Legacy => {
                        decay::apply_tad(r.score, &ts, &now, events, &tad_config)
                    }
                    // No age or retrieval-history multiplier under the
                    // cross-encoder policy.
                    RetrievalPolicy::CrossEncoder => r.score,
                }
            } else {
                r.score
            };
            enriched.push(reflection_result(
                &r.id,
                decayed_score,
                &content,
                &tags,
                timestamp,
                scope,
                false,
            ));
            reflection_tags.insert(r.id.clone(), tags);
        }
    }

    // Snapshot only candidates produced by the semantic/reflection channels.
    // FTS-only chunks are appended below and still participate in reranking,
    // but must not receive a semantic-list RRF contribution.
    let semantic_candidate_ids: HashSet<String> = enriched
        .iter()
        .map(|result| result.chunk.id.clone())
        .collect();

    super::stage_timing::stage("decay_enrich", decay_start);
    let fts_start = Instant::now();
    let mut fts_order = Vec::new();
    // Identifier-postings channel (hybrid only): ranked chunk ids, best first,
    // and the channel weight the query's identifiers earn. Empty and 0.0 when
    // the query has no identifiers or the postings table is absent or empty.
    let mut postings_hits: Vec<(String, f64)> = Vec::new();
    let mut postings_weight = 0.0_f64;
    let mut reflection_fts_hits: Vec<(String, usize, f64)> = Vec::new();
    let mut reflection_prefix_ids: Vec<String> = Vec::new();
    // Hybrid always supplements semantic retrieval with FTS5. Vector mode
    // skips FTS entirely; FTS mode reaches this block with no semantic hits.
    if search_mode != SearchMode::Vector {
        let family: Option<Vec<&str>> = scope
            .effective_project
            .is_some()
            .then(|| scope.family_projects.iter().map(String::as_str).collect());
        // The legs that read the query alone ran on a lane connection while the
        // semantic candidates were enriched; the same function runs inline when
        // no lane was available.
        let sub_start = Instant::now();
        let legs = collect_keyword_legs(
            keyword_lane.take(),
            storage,
            query,
            fetch,
            search_mode,
            family.as_deref(),
        )
        .await;
        super::stage_timing::stage("f_keyword_wait", sub_start);
        for (name, ms) in &legs.leg_ms {
            super::stage_timing::add_ms(name, *ms);
        }
        postings_hits = legs.postings_hits;
        postings_weight = legs.postings_weight;
        let mut fts_window_full = legs.keyword.window_full;
        let fts_hits = legs.keyword.hits;
        // Reflections have their own keyword index (`reflections_fts`). Merge
        // both families by bm25 into ONE keyword ranking, so a stored memory can
        // earn a keyword rank in the RRF fusion below instead of always arriving
        // semantic-only and losing to any chunk present in both lists.
        reflection_fts_hits = legs.reflection_fts_hits;
        // `reflections_fts` carries no project column (only the tags JSON
        // does), so the keyword channel is unscoped: out-of-family hits are
        // not dropped here — `sink_out_of_family_under_explicit_scope`
        // decides (strong or keyword-top-K rows keep their rank, weak ones sink).
        fts_window_full |= reflection_fts_hits.len() == fetch;
        // Exact-prefix reflections: a memory whose content starts with the
        // query verbatim is a stronger keyword signal than anything the
        // OR-of-terms bm25 ranking can express (bm25 compares corpus stats
        // across `reflections`, so a recurring project-name slug can bury
        // the literal opener under rows that merely mention one of its
        // words — live: `MEMORY-ARCHIVE anukriti-meta-campaigns` never
        // surfaced its 3 memories because 267 rows matched the OR terms).
        // Prepended ahead of the bm25 merge so it always wins the keyword
        // channel rather than just competing on bm25 scale.
        reflection_prefix_ids = legs.reflection_prefix_ids;
        fts_order = {
            let mut keyword_ranking: Vec<(&str, usize, f64)> = fts_hits
                .iter()
                .map(|(chunk, ordinal, bm25)| (chunk.id.as_str(), *ordinal, *bm25))
                .chain(
                    reflection_fts_hits
                        .iter()
                        .map(|(id, ordinal, bm25)| (id.as_str(), *ordinal, *bm25)),
                )
                .collect();
            keyword_ranking.sort_by(|left, right| {
                left.2
                    .total_cmp(&right.2)
                    .then_with(|| left.1.cmp(&right.1))
                    .then_with(|| left.0.cmp(right.0))
            });
            let mut ordered: Vec<String> =
                Vec::with_capacity(reflection_prefix_ids.len() + keyword_ranking.len());
            let mut seen: HashSet<String> = HashSet::new();
            for id in &reflection_prefix_ids {
                if seen.insert(id.clone()) {
                    ordered.push(id.clone());
                }
            }
            for (id, _, _) in keyword_ranking {
                if seen.insert(id.to_string()) {
                    ordered.push(id.to_string());
                }
            }
            ordered
        };
        // Chunks only the postings channel reached: loaded here and appended
        // through the same enrichment path the keyword-only chunks take below.
        let postings_only: Vec<crate::import::ConversationChunk> = if postings_weight > 0.0 {
            let known: HashSet<&str> = enriched
                .iter()
                .map(|e| e.chunk.id.as_str())
                .chain(fts_hits.iter().map(|(chunk, _, _)| chunk.id.as_str()))
                .collect();
            let missing: Vec<String> = postings_hits
                .iter()
                .map(|(id, _)| id.clone())
                .filter(|id| !known.contains(id.as_str()))
                .collect();
            storage.get_chunks_by_ids(&missing).unwrap_or_default()
        } else {
            Vec::new()
        };
        if !fts_hits.is_empty() || !postings_only.is_empty() {
            if !fts_hits.is_empty() {
                window_full |= fts_window_full;
            }
            let existing_ids: HashSet<String> =
                enriched.iter().map(|e| e.chunk.id.clone()).collect();
            let appended: Vec<crate::import::ConversationChunk> = fts_hits
                .into_iter()
                .map(|(chunk, _, _)| chunk)
                .chain(postings_only)
                .filter(|chunk| !existing_ids.contains(&chunk.id))
                .collect();
            // Validity was resolved over the SEMANTIC candidate set only —
            // FTS-appended chunks can carry conversation ids that set never
            // saw, and those must not slip past the partition (or past the
            // active-forgetting decay decision below). Re-resolve for every new
            // chunk id and merge the maps; a new chunk can share a conversation
            // with the semantic pass and still needs its own verdict decision.
            let extra_chunks: Vec<crate::import::ConversationChunk> = appended
                .iter()
                .filter(|chunk| !queried_chunk_ids.contains(&chunk.id))
                .cloned()
                .collect();
            let sub_start = Instant::now();
            let ancestry_revoked =
                signals.extend(storage, &extra_chunks, partition_enabled, consumption_mode);
            super::stage_timing::stage("f_signals_extend", sub_start);
            if ancestry_revoked {
                // Semantic scores were computed before the FTS-only validity
                // batch existed. Replay exactly those candidates without
                // ancestry so a failed later batch disables the signal for
                // the entire search pass, not just the appended candidates.
                ancestry_applied_ids.clear();
                for result in enriched.iter_mut().take(semantic_count) {
                    let Some(raw) = chunk_results.iter().find(|raw| raw.id == result.chunk.id)
                    else {
                        continue;
                    };
                    let events = tad_events
                        .get(&result.chunk.id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    result.score = score_chunk_candidate(
                        raw.score,
                        &result.chunk,
                        &now,
                        events,
                        &tad_config,
                        &signals.validity,
                        None,
                        active_forgetting,
                        scope,
                        policy,
                    )
                    .0;
                }
            }
            for chunk in appended {
                // FTS5 results get a synthetic score slightly below semantic threshold
                // so they rank after good semantic matches but above nothing
                let (fts_score, ancestry_applied) = score_fts_candidate(
                    &chunk,
                    &now,
                    &signals.validity,
                    signals.ancestry.get(&chunk.conversation_id),
                    active_forgetting,
                    policy,
                );
                if ancestry_applied {
                    ancestry_applied_ids.insert(chunk.id.clone());
                }
                let final_fts_score =
                    fts_score * project_scope_multiplier(&chunk.project_name, scope);
                enriched.push(EnrichedResult {
                    score: final_fts_score,
                    chunk: crate::import::ConversationChunk {
                        content: format!("[keyword] {}", chunk.content),
                        ..chunk
                    },
                    resolution: None,
                    trust: crate::provenance::TrustTier::Unknown,
                    validity_demoted: false,
                });
            }
        }
    }

    super::stage_timing::stage("fts", fts_start);
    // Keyword-only reflections: reached through `reflections_fts` or the
    // exact-prefix channel but not the vector window. No validity partition
    // applies (verdicts bind to chunk ids).
    if !reflection_fts_hits.is_empty() || !reflection_prefix_ids.is_empty() {
        let present: HashSet<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let ids = reflection_prefix_ids
            .iter()
            .map(String::as_str)
            .chain(reflection_fts_hits.iter().map(|(id, _, _)| id.as_str()));
        for id in ids {
            if present.contains(id) || !seen.insert(id) {
                continue;
            }
            if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(id) {
                enriched.push(reflection_result(
                    id,
                    FTS_REFLECTION_SCORE,
                    &content,
                    &tags,
                    timestamp,
                    scope,
                    true,
                ));
                reflection_tags.insert(id.to_string(), tags);
            }
        }
    }

    // The postings ranking restricted to rows that made it into `enriched`.
    let postings_order: Vec<String> = if postings_weight > 0.0 {
        let present: HashSet<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        postings_hits
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| present.contains(id.as_str()))
            .collect()
    } else {
        Vec::new()
    };

    // Echo filter (before any ranking, so before the cross-encoder pool): a
    // transcript row that is CSR's own output, or a logged copy of this query,
    // is left out here. The pool above is wider than the page, so the rows
    // behind take the freed places, and the adaptive refetch in `settled_pool`
    // counts what is left.
    let echo_start = Instant::now();
    let chunk_provenance: HashMap<String, Option<crate::provenance::ChunkProvenance>> = {
        let ids: Vec<&str> = enriched
            .iter()
            .filter(|e| !reflection_tags.contains_key(&e.chunk.id))
            .map(|e| e.chunk.id.as_str())
            .collect();
        let found = storage.get_chunk_provenance_many(&ids).unwrap_or_default();
        ids.iter()
            .map(|id| (id.to_string(), found.get(*id).cloned()))
            .collect()
    };
    let mut echo_dropped =
        drop_echo_rows(&mut enriched, &chunk_provenance, &reflection_tags, query);
    super::stage_timing::stage("echo_filter", echo_start);
    // Eval and synthetic content leaves the pool at the same point, by
    // provenance (the session ran the bench/eval rig, or the row is a rig
    // call's output). The page counts both as dropped for the refetch.
    if let Some(handle) = verdict_lane.take() {
        let _ = handle.await;
    }
    echo_dropped.extend(drop_synthetic_rows(
        storage,
        &mut enriched,
        &chunk_provenance,
        &reflection_tags,
    ));
    let postings_order: Vec<String> = postings_order
        .into_iter()
        .filter(|id| !echo_dropped.contains(id))
        .collect();
    fts_order.retain(|id| !echo_dropped.contains(id));

    // Only built while a stage probe is active: `enriched_conv` exists solely
    // to feed the `rerank`/`fuse_rrf` stage-probe checkpoints further down
    // (see their record_stage calls), never the production ranking itself.
    let probe_active = crate::mcp::stage_probe::is_active();
    let enriched_conv: Option<HashMap<String, String>> = probe_active.then(|| {
        enriched
            .iter()
            .map(|e| (e.chunk.id.clone(), e.chunk.conversation_id.clone()))
            .collect()
    });
    if probe_active {
        let mut ranked: Vec<(String, String, f32)> = enriched
            .iter()
            .map(|e| (e.chunk.id.clone(), e.chunk.conversation_id.clone(), e.score))
            .collect();
        ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
        crate::mcp::stage_probe::record_stage("decay_enriched", &ranked);
    }

    // Provenance-aware re-rank (v9.3): authority + meaning layered on the decayed
    // score. A cached floor at UserHistory or above is boosted; lower-floor
    // authority claims and tool-mechanic build logs are demoted, so a founding decision out-ranks the
    // [Edit:]/[Bash:] chunks that used to bury it. Falls back to score order when
    // no provenance/meaning signal differs.
    // NO STACKING (v10): Demote-channel chunks are excluded from reranking
    // entirely rather than reranked-then-demoted — `search::rerank`'s
    // scaffold penalty is a soft score signal, and this chunk is about to be
    // structurally sunk below every non-demoted result in
    // `apply_validity_partition` regardless. Reranking it anyway would
    // compound that soft penalty with the hard sink for the same staleness
    // signal. A side effect does the sinking's job here too: `rank_of`
    // returns `usize::MAX` for any id absent from `order`, and `sort_by_key`
    // is stable, so excluded ids land at the tail in their pre-rerank
    // relative order — `apply_validity_partition`'s explicit partition below
    // is still the authoritative, independently-testable guarantee.
    let rerank_start = Instant::now();
    let candidates: Vec<crate::search::rerank::RankCandidate> = enriched
        .iter()
        .filter(|e| !is_demote_channel(&signals.validity, &e.chunk.id))
        .map(|e| crate::search::rerank::RankCandidate {
            id: e.chunk.id.clone(),
            cosine: e.score,
            content: e.chunk.content.clone(),
            provenance: chunk_provenance.get(&e.chunk.id).cloned().flatten(),
            // Reflection ids are not chunk ids: the chunk floor lookup would
            // read Unknown for every one of them.
            min_trust: match reflection_tags.get(&e.chunk.id) {
                Some(tags) => reflection_recall_standing(
                    storage,
                    &e.chunk.id,
                    tags,
                    reflection_raw_cosine
                        .get(&e.chunk.id)
                        .copied()
                        .unwrap_or(0.0),
                ),
                None => storage
                    .get_chunk_min_trust(&e.chunk.id)
                    .unwrap_or(crate::provenance::TrustTier::Unknown),
            },
            timestamp: Some(e.chunk.timestamp.clone()),
        })
        .collect();
    // Captured before `candidates` is moved into `rerank`/trained-rerank
    // below — only used by the stage probe's rerank-breakdown checkpoint.
    let candidates_for_probe: Option<(Vec<crate::search::rerank::RankCandidate>, Option<String>)> =
        probe_active.then(|| {
            let origin = crate::search::rerank::primacy_conv(&candidates);
            (candidates.clone(), origin)
        });
    let mut raw_order = candidates.clone();
    raw_order.sort_by(|left, right| {
        right
            .cosine
            .partial_cmp(&left.cosine)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let raw_rank: HashMap<String, usize> = raw_order
        .into_iter()
        .enumerate()
        .map(|(rank, candidate)| (candidate.id, rank))
        .collect();
    let need_feature_contexts = match rerank_mode {
        RecallRerankMode::Runtime => crate::search::trained_rerank::trained_rerank_requested(),
        RecallRerankMode::Baseline => false,
        RecallRerankMode::Candidate(_) => true,
    };
    let feature_contexts: HashMap<String, crate::search::trained_rerank::RuntimeFeatureContext> =
        if need_feature_contexts {
            let raw_cosines: HashMap<&str, f64> = chunk_results
                .iter()
                .chain(reflection_results.iter())
                .map(|result| (result.id.as_str(), f64::from(result.score)))
                .collect();
            enriched
                .iter()
                .map(|result| {
                    let source_type = if result.chunk.content.starts_with("[episode] ") {
                        "episode"
                    } else if result.chunk.content.starts_with("[story] ") {
                        "story"
                    } else if result.chunk.content.starts_with("[reflection] ")
                        || result.chunk.content.starts_with("[narrative] ")
                    {
                        "reflection"
                    } else {
                        "chunk"
                    };
                    (
                        result.chunk.id.clone(),
                        crate::search::trained_rerank::RuntimeFeatureContext {
                            cosine: raw_cosines.get(result.chunk.id.as_str()).copied(),
                            decayed_score: Some(f64::from(result.score)),
                            recency: None,
                            graph_proximity: None,
                            source_type: source_type.into(),
                        },
                    )
                })
                .collect()
        } else {
            HashMap::new()
        };
    let trained_order = match rerank_mode {
        RecallRerankMode::Runtime => crate::search::trained_rerank::rerank_with_latest_scored(
            storage,
            &candidates,
            rerank_intent,
            &feature_contexts,
        ),
        RecallRerankMode::Baseline => None,
        RecallRerankMode::Candidate(model) => {
            Some(crate::search::trained_rerank::rerank_with_model_scored(
                &candidates,
                rerank_intent,
                &feature_contexts,
                model,
            )?)
        }
    };
    let adjusted_scores = trained_order.as_ref().map_or_else(
        || match policy {
            RetrievalPolicy::Legacy => recall_display_rank_scores(&candidates),
            // No boost term: the displayed score is the raw score.
            RetrievalPolicy::CrossEncoder => candidates
                .iter()
                .map(|candidate| (candidate.id.clone(), candidate.cosine))
                .collect(),
        },
        |rows| {
            rows.iter()
                .map(|(candidate, score)| (candidate.id.clone(), *score as f32))
                .collect()
        },
    );
    let order: Vec<String> = trained_order
        .map(|rows| rows.into_iter().map(|(candidate, _)| candidate).collect())
        .unwrap_or_else(|| match policy {
            RetrievalPolicy::Legacy => crate::search::rerank::rerank(candidates),
            RetrievalPolicy::CrossEncoder => crate::search::rerank::rerank_plain(candidates),
        })
        .into_iter()
        .map(|c| c.id)
        .collect();
    super::stage_timing::stage("rerank", rerank_start);
    if let Some(enriched_conv) = enriched_conv.as_ref() {
        let ranked: Vec<(String, String, f32)> = order
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    enriched_conv.get(id).cloned().unwrap_or_default(),
                    adjusted_scores.get(id).copied().unwrap_or(0.0),
                )
            })
            .collect();
        crate::mcp::stage_probe::record_stage("rerank", &ranked);
        if let Some((candidates, origin)) = candidates_for_probe.as_ref() {
            crate::mcp::stage_probe::record_rerank_breakdown(
                candidates,
                &order,
                enriched_conv,
                origin.as_deref(),
            );
        }
    }
    let rerank_rank: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(rank, id)| (id.as_str(), rank))
        .collect();
    let display_rank_scores = enriched
        .iter()
        .filter_map(|result| {
            let moved_by_rerank = raw_rank.get(&result.chunk.id).is_some_and(|raw| {
                rerank_rank
                    .get(result.chunk.id.as_str())
                    .is_some_and(|reranked| raw != reranked)
            });
            moved_by_rerank.then(|| {
                adjusted_scores
                    .get(&result.chunk.id)
                    .map(|score| DisplayRankScore {
                        chunk_id: result.chunk.id.clone(),
                        adjusted_score: *score,
                    })
            })?
        })
        .collect();
    let semantic_order: Vec<String> = order
        .iter()
        .filter(|id| semantic_candidate_ids.contains(*id))
        .cloned()
        .collect();
    // Captured before `fts_order` is moved into `final_order` below (the
    // `SearchMode::Fts` arm takes it by value) — only used by the stage
    // probe's fuse_rrf checkpoint.
    let fts_ranked_for_probe: Option<Vec<(String, String)>> =
        enriched_conv.as_ref().map(|enriched_conv| {
            fts_order
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        enriched_conv.get(id).cloned().unwrap_or_default(),
                    )
                })
                .collect()
        });
    // This establishes the initial order for every mode. The validity
    // partition runs later: it preserves the valid section byte-for-byte,
    // while active forgetting may reorder only the demoted section by
    // accelerated decay in hybrid, vector, and FTS modes alike.
    //
    // Hybrid fuses at the session level: semantic, keyword and postings
    // rankings give each session the rank of its best chunk, and the session
    // is represented by that chunk. A channel with weight 0.0 is skipped.
    let mut collapsed_sessions: Option<(HashSet<String>, HashMap<String, String>)> = None;
    let mut ce_session_scores: HashMap<String, f32> = HashMap::new();
    let final_order = match search_mode {
        SearchMode::Hybrid => {
            // Episode channel: bm25 over `episodes_fts`, one more lexical
            // list fused at session level with the chunk channels. Off (None)
            // unless CSR_EPISODES=1 (CSR_NO_EPISODES wins) or with an empty
            // episodes table, where this arm is the chunk-only path.
            let episode_hits = {
                let family: Option<Vec<&str>> = scope
                    .effective_project
                    .is_some()
                    .then(|| scope.family_projects.iter().map(String::as_str).collect());
                crate::search::episodes::gather(storage, query, family.as_deref())
            };
            let mut episode_order: Vec<String> = Vec::new();
            let mut episode_class: HashMap<String, u8> = HashMap::new();
            if let Some(hits) = episode_hits.as_ref() {
                for hit in hits {
                    episode_order.push(hit.id.clone());
                    episode_class.insert(hit.id.clone(), hit.echo_class);
                    enriched.push(episode_row(hit, &now));
                }
            }
            let episode_weight = if episode_hits.is_some() { 1.0 } else { 0.0 };
            // Status-intent queries only: the newest report chunks of the
            // matched sessions join the cross-encoder pool (default otherwise).
            let status_admission = if policy == RetrievalPolicy::CrossEncoder
                && crate::search::status_intent::is_status_query(query)
            {
                let chunk_fused = crate::search::hybrid::fuse_rrf_weighted(
                    &[
                        (&semantic_order, 1.0),
                        (&fts_order, 1.0),
                        (&postings_order, postings_weight),
                    ],
                    RRF_K,
                );
                status_pool_admission(
                    storage,
                    query,
                    &mut enriched,
                    &chunk_fused,
                    &chunk_provenance,
                    &mut signals,
                    scope,
                    active_forgetting,
                    policy,
                    partition_enabled,
                    consumption_mode,
                    &now,
                )
            } else {
                StatusAdmission::default()
            };
            let session_keys_start = Instant::now();
            let session_keys: HashMap<String, String> = enriched
                .iter()
                .map(|e| (e.chunk.id.clone(), session_key_of(storage, &e.chunk)))
                .collect();
            super::stage_timing::stage("session_keys", session_keys_start);
            let answer_class: HashMap<String, u8> = enriched
                .iter()
                .map(|e| {
                    (
                        e.chunk.id.clone(),
                        episode_class
                            .get(&e.chunk.id)
                            .copied()
                            .unwrap_or_else(|| non_answer_class(&e.chunk.content, query)),
                    )
                })
                .collect();
            // S8: superseded units leave every fused channel before session
            // collapse, so session max and the cross-encoder pool never see
            // them. They trail the page once per session, after the cross-encoder.
            let sunk = run_assertions_stage(
                storage,
                &mut enriched,
                &session_keys,
                &signals.validity,
                assertions_enabled(),
            );
            let fused_before_partition: Vec<String> = if sunk.is_empty() {
                Vec::new()
            } else {
                crate::search::hybrid::fuse_rrf_weighted(
                    &[
                        (&semantic_order, 1.0),
                        (&fts_order, 1.0),
                        (&postings_order, postings_weight),
                        (&episode_order, episode_weight),
                    ],
                    RRF_K,
                )
            };
            let semantic_order = assertions_partition::without_sunk(&semantic_order, &sunk);
            let fts_order = assertions_partition::without_sunk(&fts_order, &sunk);
            let postings_order = assertions_partition::without_sunk(&postings_order, &sunk);
            let episode_order = assertions_partition::without_sunk(&episode_order, &sunk);
            let mut collapsed = collapse_to_sessions_by(
                &[
                    (&semantic_order, 1.0),
                    (&fts_order, 1.0),
                    (&postings_order, postings_weight),
                    (&episode_order, episode_weight),
                ],
                |id| {
                    session_keys
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| id.to_string())
                },
                |id| answer_class.get(id).copied().unwrap_or(0),
                RRF_K,
            );
            // Session representative: a tool-output pick yields to the
            // session's best content chunk in the pool. Stored author only.
            // The cross-encoder below applies the same rule to its own picks.
            let mut representative_pool_map: HashMap<String, Vec<PoolChunk>> = HashMap::new();
            if policy == RetrievalPolicy::CrossEncoder {
                let chunk_fused = crate::search::hybrid::fuse_rrf_weighted(
                    &[
                        (&semantic_order, 1.0),
                        (&fts_order, 1.0),
                        (&postings_order, postings_weight),
                    ],
                    RRF_K,
                );
                let representatives_start = Instant::now();
                representative_pool_map = representative_pool(
                    storage,
                    &chunk_fused,
                    &enriched,
                    &session_keys,
                    &answer_class,
                    &collapsed,
                );
                super::stage_timing::stage("representatives", representatives_start);
                apply_content_representatives(
                    &mut collapsed,
                    &session_keys,
                    &representative_pool_map,
                );
            }
            // A session whose best unit is an episode shows that episode as
            // its excerpt, under the session's best chunk when it has one.
            let mut episode_remap: HashMap<String, String> = HashMap::new();
            if let Some(hits) = episode_hits.as_ref() {
                let chunk_fused = crate::search::hybrid::fuse_rrf_weighted(
                    &[
                        (&semantic_order, 1.0),
                        (&fts_order, 1.0),
                        (&postings_order, postings_weight),
                    ],
                    RRF_K,
                );
                episode_remap = settle_episode_excerpts(
                    &mut enriched,
                    &mut collapsed,
                    &session_keys,
                    &chunk_fused,
                    hits,
                );
            }
            // Cross-encoder stage: re-score the top fused units, order the
            // pooled sessions by their best score. Under the legacy policy
            // the S1 order stands untouched.
            let collapsed = if policy == RetrievalPolicy::CrossEncoder {
                let fused: Vec<String> = crate::search::hybrid::fuse_rrf_weighted(
                    &[
                        (&semantic_order, 1.0),
                        (&fts_order, 1.0),
                        (&postings_order, postings_weight),
                        (&episode_order, episode_weight),
                    ],
                    RRF_K,
                )
                .into_iter()
                .map(|id| episode_remap.get(&id).cloned().unwrap_or(id))
                .collect();
                let (reordered, session_ce) = ce_rerank_stage(
                    query,
                    collapsed,
                    fused,
                    &enriched,
                    &session_keys,
                    &answer_class,
                    &representative_pool_map,
                    &status_admission.admitted,
                    memo,
                    rerank_daemon.as_deref(),
                );
                ce_session_scores = session_ce;
                let reordered =
                    apply_status_picks(reordered, &session_keys, &status_admission.picks, &sunk);
                let reordered = apply_status_recency(
                    storage,
                    query,
                    reordered,
                    &enriched,
                    &session_keys,
                    &ce_session_scores,
                    page,
                );
                ce_pool::log_final_order(&reordered, &session_keys, &ce_session_scores);
                if let Some(enriched_conv) = enriched_conv.as_ref() {
                    let ranked: Vec<(String, String, f32)> = reordered
                        .iter()
                        .map(|id| {
                            (
                                id.clone(),
                                enriched_conv.get(id).cloned().unwrap_or_default(),
                                adjusted_scores.get(id).copied().unwrap_or(0.0),
                            )
                        })
                        .collect();
                    crate::mcp::stage_probe::record_stage("ce_rerank", &ranked);
                }
                reordered
            } else {
                collapsed
            };
            let session_of = |id: &str| {
                session_keys
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_string())
            };
            let collapsed = assertions_partition::trail_sunk(
                collapsed,
                &fused_before_partition,
                &sunk,
                session_of,
            );
            // A superseded unit that was in the top `page` keeps the last
            // shown slot, so its receipt note is still on the page.
            let collapsed = assertions_partition::keep_sunk_in_page(
                collapsed,
                &fused_before_partition,
                &sunk,
                page,
            );
            let represented: HashSet<String> = collapsed
                .iter()
                .filter_map(|id| session_keys.get(id).cloned())
                .collect();
            collapsed_sessions = Some((represented, session_keys));
            collapsed
        }
        SearchMode::Vector => {
            let enabled = assertions_enabled();
            let session_keys = assertions_session_keys(storage, &enriched, enabled);
            let sunk = run_assertions_stage(
                storage,
                &mut enriched,
                &session_keys,
                &signals.validity,
                enabled,
            );
            let before = order.clone();
            let tailed = assertions_partition::sunk_to_tail(order, &sunk);
            assertions_partition::keep_sunk_in_page(tailed, &before, &sunk, page)
        }
        SearchMode::Fts => {
            let enabled = assertions_enabled();
            let session_keys = assertions_session_keys(storage, &enriched, enabled);
            let sunk = run_assertions_stage(
                storage,
                &mut enriched,
                &session_keys,
                &signals.validity,
                enabled,
            );
            let before = fts_order.clone();
            let tailed = assertions_partition::sunk_to_tail(fts_order, &sunk);
            assertions_partition::keep_sunk_in_page(tailed, &before, &sunk, page)
        }
    };
    // Exact-prefix reflections win the keyword channel outright (see where
    // `reflection_prefix_ids` is built) — but RRF alone does not reliably
    // surface a hit that is present in ONLY the keyword list ahead of items
    // present (even weakly) in both the semantic AND keyword lists: with
    // enough double-counted competition, position-only fusion still buries
    // it. An exact opening match is strong enough evidence to skip fusion
    // entirely and place it first, the same way an exact `conv_<uuid>`
    // handle bypasses similarity ranking (`extract_conv_id`).
    let final_order = promote_exact_prefix_reflections(final_order, &reflection_prefix_ids);
    if let Some(enriched_conv) = enriched_conv.as_ref() {
        let ranked: Vec<(String, String, f32)> = final_order
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    enriched_conv.get(id).cloned().unwrap_or_default(),
                    adjusted_scores.get(id).copied().unwrap_or(0.0),
                )
            })
            .collect();
        if search_mode == SearchMode::Hybrid {
            let ranked: Vec<(String, String, f32)> = postings_order
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        enriched_conv.get(id).cloned().unwrap_or_default(),
                        adjusted_scores.get(id).copied().unwrap_or(0.0),
                    )
                })
                .collect();
            crate::mcp::stage_probe::record_stage("postings", &ranked);
        }
        crate::mcp::stage_probe::record_stage("fuse_rrf", &ranked);
        // FTS-side rank is separate from the fused rank just recorded above:
        // it's the target's position in the keyword ranking that RRF fused
        // with the semantic ranking, not the post-fusion position.
        if let Some(fts_ranked) = fts_ranked_for_probe.as_ref() {
            crate::mcp::stage_probe::record_fts_rank(fts_ranked);
        }
    }
    let rank_of = |id: &str| {
        final_order
            .iter()
            .position(|candidate_id| candidate_id == id)
            .unwrap_or(usize::MAX)
    };
    enriched.sort_by_key(|e| rank_of(&e.chunk.id));
    // One row per session: a chunk that lost its session to a better chunk
    // leaves the page. Rows in no ranking at all (a demoted chunk the rerank
    // left out) keep their tail place unless their session already has a row.
    if let Some((represented, session_keys)) = collapsed_sessions.as_ref() {
        let kept: HashSet<&str> = final_order.iter().map(String::as_str).collect();
        enriched.retain(|e| {
            kept.contains(e.chunk.id.as_str())
                || session_keys
                    .get(&e.chunk.id)
                    .is_none_or(|session| !represented.contains(session))
        });
    }
    // Rows that carry no answer sink below the rows that do. Runs before the
    // out-of-family sink so that sink, which is the stronger statement about
    // where a row belongs, keeps the last word on order.
    sink_non_answers(&mut enriched, query);
    sink_rig_rows(storage, &mut enriched, &reflection_tags, query);
    // Under an explicit project filter, chunk search is already restricted to
    // the project family, but reflections and stories are not: RRF places them
    // by rank, so a cosine-0.12 story from another project could sit above
    // in-family hits (measured live 2026-09-23, project=EnhanceMe: ranks 5-7
    // were an anukriti story, an unknown-project reflection and a subagent
    // extraction). Sink out-of-family rows below every in-family row instead
    // of dropping them: a hard drop lost the answer to a real burst query
    // (a May story tagged with a sibling project name).
    let reflection_keyword_ids: HashSet<&str> = reflection_fts_hits
        .iter()
        .filter(|(_, ordinal, _)| *ordinal < OUT_OF_FAMILY_KEYWORD_TOP_K)
        .map(|(id, _, _)| id.as_str())
        .collect();
    sink_out_of_family_under_explicit_scope(
        &mut enriched,
        scope,
        &reflection_tags,
        &reflection_raw_cosine,
        &reflection_keyword_ids,
    );

    // Dedupe BEFORE the limit cut (CodeRabbit): truncating first let a higher-
    // ranked plan chunk survive while its origin conversation fell outside the
    // page — inverting the authoritative-origin rule — and could return a short
    // page after filtering.
    let dedupe_start = Instant::now();
    format::dedupe_results(&mut enriched);
    // Plan-doc chunks defer to their origin conversation when both matched (v9.4):
    // the plan restates the decision; the conversation is where it was made.
    let origin_of: std::collections::HashMap<String, String> = enriched
        .iter()
        .filter(|e| e.chunk.conversation_id.starts_with("plan:"))
        .filter_map(|e| {
            storage
                .get_chunk_provenance(&e.chunk.id)
                .ok()
                .flatten()
                .map(|p| (e.chunk.id.clone(), p.source_conv_id))
        })
        .collect();
    format::dedupe_plan_origins(&mut enriched, &origin_of);
    // Sidechain transcripts remain independent conversations, but when both a
    // child and its parent session match, the parent is the authoritative origin.
    let parent_of: std::collections::HashMap<String, String> = enriched
        .iter()
        .filter(|result| result.chunk.is_sidechain)
        .filter_map(|result| {
            storage
                .get_chunk_provenance(&result.chunk.id)
                .ok()
                .flatten()
                .map(|provenance| (result.chunk.id.clone(), provenance.source_conv_id))
        })
        .collect();
    format::dedupe_sidechain_origins(&mut enriched, &parent_of);
    // The same text imported from a Codex rollout and from a Claude session is
    // one memory: the Claude session is the origin and keeps the row.
    let cross_source_collapsed = Collapsed {
        cross_source: collapse_cross_source_duplicates(&mut enriched, |id| {
            storage.get_chunk_source(id).ok().flatten()
        }),
        same_text: 0,
    };
    super::stage_timing::stage("dedupe", dedupe_start);

    if crate::mcp::stage_probe::is_active() {
        let ranked: Vec<(String, String, f32)> = enriched
            .iter()
            .map(|e| (e.chunk.id.clone(), e.chunk.conversation_id.clone(), e.score))
            .collect();
        crate::mcp::stage_probe::record_stage("dedupe", &ranked);
    }

    let ancestry_candidates_used = enriched
        .iter()
        .filter(|result| ancestry_applied_ids.contains(&result.chunk.id))
        .count();
    tracing::debug!(
        ancestry_candidates_used,
        "release ancestry applied to search candidates"
    );
    // The label reads the cross-encoder scores of the list as it finally
    // stands: after the sinks, the dedupes and the exact-prefix promotion.
    let mut ce_inputs = None;
    let ce = match collapsed_sessions.as_ref() {
        Some((_, session_keys)) if !ce_session_scores.is_empty() => {
            let final_sessions: Vec<&str> = enriched
                .iter()
                .map(|e| {
                    session_keys
                        .get(&e.chunk.id)
                        .map_or(e.chunk.id.as_str(), String::as_str)
                })
                .collect();
            let summary = final_ce_summary(&final_sessions, &ce_session_scores);
            if summary.is_some() {
                ce_inputs = Some(CeInputs {
                    session_scores: ce_session_scores.clone(),
                    session_of_chunk: session_keys.clone(),
                });
            }
            summary
        }
        _ => None,
    };
    Ok(GatherPass {
        enriched,
        display_rank_scores,
        validity: signals.validity,
        window_full,
        echo_dropped: echo_dropped.len(),
        search_ms,
        ce,
        cross_source_collapsed,
        ce_inputs,
    })
}

/// Store a reflection with embedding for future search, with `tags` used
/// verbatim (no explicit-memory stamp). Shared by the MCP `store_reflection`
/// tool (which stamps first, see [`store_reflection`]) and every pipeline
/// writer (stories, episodes, extractions) that must NOT get user standing —
/// see `reflection_recall_standing` and `is_explicit_memory`.
pub async fn store_reflection_internal(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    content: &str,
    tags: &[String],
) -> Result<String> {
    let _operation = storage.operation_lock()?;
    let id = uuid::Uuid::new_v4().to_string();

    let embedding = {
        let text = content.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&text)).await??
    };

    store_embedded_reflection(storage, search, id, content, tags, embedding).await
}

async fn store_embedded_reflection(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    id: String,
    content: &str,
    tags: &[String],
    embedding: Vec<f32>,
) -> Result<String> {
    let _operation = storage.operation_lock()?;
    // Read before the commit (see `SearchEngine::defers_to_sync`).
    let fence = storage.change_fence()?;
    storage.insert_reflection(&id, content, tags, &embedding)?;

    #[cfg(test)]
    crate::storage::lock_probe::before_reflection_apply(&id);
    let acknowledged = {
        let mut idx = search.write().await;
        // Past the fence the index sync applies the row from SQLite, in order;
        // it has not been indexed yet, so the answer is "pending".
        !idx.defers_to_sync(true, fence) && idx.insert_reflection(id.clone(), embedding)
    };
    let status = if acknowledged {
        "Reflection stored successfully."
    } else {
        "Reflection stored; semantic indexing is pending."
    };
    Ok(format!("{status}\nID: {id}\nTags: {tags:?}"))
}

/// Store an EXPLICIT memory write for future search: the MCP `store_reflection`
/// tool's only entry point. Stamps [`EXPLICIT_MEMORY_TAG`] so recall can rank
/// this row with user standing (`reflection_recall_standing`) without
/// inferring it from shape. Pipeline writers (stories/episodes/extractions)
/// must call [`store_reflection_internal`] directly instead — they are not
/// deliberate memory writes and must not gain user standing by sharing this
/// stamping path.
pub async fn store_reflection(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    content: &str,
    tags: &[String],
) -> Result<String> {
    let mut tags: Vec<String> = tags.to_vec();
    if !tags.iter().any(|t| t == EXPLICIT_MEMORY_TAG) {
        tags.push(EXPLICIT_MEMORY_TAG.to_string());
    }
    store_reflection_internal(storage, embeddings, search, content, &tags).await
}

/// Record a resolution verdict for one or more chunks.
pub async fn resolve_chunks(
    storage: &Arc<Storage>,
    chunk_ids: Vec<String>,
    status: String,
    evidence: String,
    claim: Option<String>,
    confirmation: Option<&crate::provenance::ResolutionConfirmation>,
) -> Result<String> {
    if !matches!(status.as_str(), "resolved" | "still_open" | "regressed") {
        anyhow::bail!(
            "invalid status '{}': must be resolved, still_open, or regressed",
            status
        );
    }
    if chunk_ids.is_empty() {
        anyhow::bail!("chunk_ids must not be empty");
    }
    if evidence.trim().is_empty() {
        anyhow::bail!("evidence must not be empty");
    }

    let (n, source) = storage.insert_resolutions_with_confirmation(
        &chunk_ids,
        &status,
        &evidence,
        claim.as_deref(),
        confirmation,
    )?;
    clear_pool_cache(storage);

    Ok(format!(
        "recorded {} verdict(s): {} (source: {})",
        n, status, source
    ))
}

#[path = "tools_quick_check.rs"]
mod quick;
pub use quick::quick_check;
#[cfg(test)]
use quick::quick_check_with_vec;

#[path = "tools_page.rs"]
mod page;
use page::{annotate_collapsed, annotate_short_page, collapse_same_text, Collapsed};

/// Search insights — aggregated summary without individual results.
pub async fn search_insights(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
) -> Result<String> {
    search_insights_at_root(storage, embeddings, search, query, project, None).await
}

/// [`search_insights`] with an injectable projects-root — the seam a test
/// uses to exercise family-alias widening (`family_chunk_ids_at_root`)
/// without depending on this machine's real `$HOME/projects` layout.
async fn search_insights_at_root(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
    projects_root_override: Option<&Path>,
) -> Result<String> {
    let (effective_project, _) =
        cross_project::normalize_project_scope_at_root(storage, project, projects_root_override)?;
    let query_vec = embed_query(embeddings, query).await?;

    let results = if let Some(ref p) = effective_project {
        let ids = family_chunk_ids_at_root(storage, p, projects_root_override)?;
        let idx = search.read().await;
        idx.search_chunks_filtered(&query_vec, 10, 0.0, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(&query_vec, 10, 0.0)
    };

    let enriched = enrich_results(storage, &results)?;
    Ok(format::format_search_insights(&enriched, query))
}

/// Get recent work conversations.
pub async fn get_recent_work(
    storage: &Arc<Storage>,
    limit: usize,
    project: Option<&str>,
    group_by: &str,
) -> Result<String> {
    let chunks = storage.get_recent_chunks(limit, project)?;
    let notes = csr_text_notes(storage, &chunks);
    Ok(format::format_recent_work(&chunks, group_by, &notes))
}

fn recency_window(
    time_range: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> {
    Ok(if let Some(tr) = time_range {
        temporal::parse_time_expression(tr)?
    } else if since.is_some() || until.is_some() {
        let s = if let Some(s) = since {
            temporal::parse_time_expression(s)?.0
        } else {
            chrono::Utc::now() - chrono::Duration::days(7)
        };
        let e = if let Some(u) = until {
            temporal::parse_time_expression(u)?.1
        } else {
            chrono::Utc::now()
        };
        (s, e)
    } else {
        let now = chrono::Utc::now();
        (now - chrono::Duration::days(7), now)
    })
}

/// Time-constrained semantic search.
#[allow(clippy::too_many_arguments)]
pub async fn search_by_recency(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    time_range: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    validate_page(0, limit)?;
    let (start, end) = recency_window(time_range, since, until)?;

    let start_str = start.to_rfc3339();
    let end_str = end.to_rfc3339();
    // The exact bounds searched, in the header, so a window that is not the one
    // the caller meant ("last month" is the previous calendar month) is seen.
    // `recency_window` reads `time_range` first, so it alone names the window.
    let named = time_range;
    let time_desc = temporal::describe_window(named, &start, &end);

    // Get chunk IDs within time range, then do filtered HNSW search
    let time_ids: HashSet<String> = storage
        .get_chunk_ids_in_timerange(&start_str, &end_str, project)?
        .into_iter()
        .collect();

    if time_ids.is_empty() {
        let hint = page::recency_hint(named, &start, &end, &[]);
        return Ok(format::format_recency_results_hinted(
            &[],
            query,
            &time_desc,
            hint.as_deref(),
        ));
    }

    let query_vec = embed_query(embeddings, query).await?;
    // Overfetch + partition over the full candidate set + single adaptive
    // refetch + truncate — see `fetch_enrich_partition_adaptive`.
    let enriched = {
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks_filtered(&query_vec, n, min_score, &time_ids),
            query,
            limit,
            consumption_mode_for_partition(validity_partition_enabled()),
        )?
    };
    let hint = page::recency_hint(named, &start, &end, &enriched);
    Ok(format::format_recency_results_hinted(
        &enriched,
        query,
        &time_desc,
        hint.as_deref(),
    ))
}

#[path = "tools_timeline.rs"]
mod timeline;
pub use timeline::get_timeline;

/// Attach the WP2 Stage 2 two-channel attribution render string
/// (`Storage::code_attribution_for_node`) onto every node in `nodes`, in
/// place. Per-node lookup errors fail soft to "unattributed" — a single
/// storage hiccup must never blank an entire result set or fall back to
/// `first_conv_id`.
fn attach_attribution(storage: &Arc<Storage>, nodes: &mut [crate::storage::codegraph::NodeRow]) {
    for n in nodes.iter_mut() {
        if n.id.is_empty() {
            n.attribution = "unattributed".to_string();
            continue;
        }
        n.attribution = storage
            .code_attribution_for_node(&n.id)
            .unwrap_or_else(|_| "unattributed".to_string());
    }
}

/// File ledger (§8b) — deterministic, immutable per-file dossier built from the
/// code graph + code_evolution. FTS5 is only a secondary fallback when the file
/// has no graph/evolution history.
pub async fn search_by_file(
    storage: &Arc<Storage>,
    file_path: &str,
    limit: usize,
    project: Option<&str>,
) -> Result<String> {
    search_by_file_with_map(
        storage,
        file_path,
        limit,
        project,
        crate::codemap::disabled(),
    )
    .await
}

/// Paths of the code map's `<code_map>` section for a file lookup.
const CODE_MAP_PATH_CAP: usize = 10;
/// Definitions of the code map's `<code_map>` section for a symbol lookup.
const CODE_MAP_DEFINITION_CAP: usize = 10;

/// Which repos of the code map a tool call may show: those of one explicit
/// project, its family aliases included. `None` = every repo.
#[derive(Clone, Copy)]
struct MapScope<'a> {
    project: &'a str,
    family_projects: &'a [String],
}

impl<'a> MapScope<'a> {
    /// A scope for an explicit, specific project; none for an empty or `all` one.
    fn for_project(project: &'a str, family_projects: &'a [String]) -> Option<Self> {
        (!project.is_empty() && !project.eq_ignore_ascii_case("all")).then_some(Self {
            project,
            family_projects,
        })
    }

    /// The map repos whose checkout or worktree directory name is the project,
    /// a family alias of it, or in the family list.
    fn repos(&self, conn: &rusqlite::Connection) -> Result<crate::codemap::RepoScope> {
        crate::codemap::repos_matching(conn, &|label| {
            label == self.project
                || self.family_projects.iter().any(|f| f == label)
                || cross_project::same_project_family(self.project, label)
        })
    }
}

/// The code map's answer for a file argument: the formatted `<code_map>` section
/// and how many distinct views hold the matched paths. `None` when the code map
/// is off, its tables are missing, or nothing matches. Read-only; any storage
/// error counts as "no answer", never as a failure of the tool.
fn code_map_file_section(
    storage: &Arc<Storage>,
    arg: &str,
    map_off: bool,
    scope: Option<MapScope<'_>>,
) -> Option<(String, usize)> {
    if map_off || arg.trim().is_empty() {
        return None;
    }
    let lookup = storage
        .with_connection(|conn| {
            let repos = scope.map(|s| s.repos(conn)).transpose()?;
            crate::codemap::lookup_files_scoped(conn, arg, CODE_MAP_PATH_CAP, repos.as_ref())
        })
        .ok()?;
    if lookup.files.is_empty() {
        return None;
    }
    let views: HashSet<&str> = lookup
        .files
        .iter()
        .flat_map(|f| f.presence.iter().map(|p| p.worktree_path.as_str()))
        .collect();
    Some((
        format::format_code_map_files(&lookup.files, lookup.total_paths),
        views.len(),
    ))
}

/// The code map's definitions named exactly `symbol`, formatted as a
/// `<code_map>` section. `None` when off, missing tables, or no definition.
fn code_map_definitions_section(
    storage: &Arc<Storage>,
    symbol: &str,
    map_off: bool,
    scope: Option<MapScope<'_>>,
) -> Option<String> {
    if map_off || symbol.trim().is_empty() {
        return None;
    }
    let hits = storage
        .with_connection(|conn| {
            let repos = scope.map(|s| s.repos(conn)).transpose()?;
            crate::codemap::query_exact_scoped(
                conn,
                symbol.trim(),
                CODE_MAP_DEFINITION_CAP,
                repos.as_ref(),
            )
        })
        .ok()?;
    (!hits.is_empty()).then(|| format::format_code_map_definitions(&hits))
}

/// [`search_by_file`] with the code-map switch injectable, so tests need not
/// touch the process environment.
pub(super) async fn search_by_file_with_map(
    storage: &Arc<Storage>,
    file_path: &str,
    limit: usize,
    project: Option<&str>,
    map_off: bool,
) -> Result<String> {
    // The code-graph ledger (`code_nodes`/`code_evolution`/
    // `code_graph_file_state`) filters by an exact `project` match — no
    // family-alias widening like `family_chunk_ids` gives the chunk search
    // tools. Defaulting an *omitted* project to the caller's
    // `CLAUDE_PROJECT_DIR` family name the way those tools do would narrow
    // this ledger down to just that one project name and silently drop
    // everything filed under a family alias (e.g. `claude-self-reflect`'s
    // `claude-self-reflect-csr-engine` code nodes) — strictly worse than
    // before this tool was scoped at all, since an omitted project used to
    // run fully unscoped. Only an *explicit, specific* project narrows the
    // ledger; omitted or `"all"` stays global, exactly as before the
    // CLAUDE_PROJECT_DIR default-scope fallback existed.
    let proj = match project {
        Some(p) if !p.is_empty() && !p.eq_ignore_ascii_case("all") => p.to_string(),
        _ => String::new(),
    };

    let mut ledger = storage.code_file_ledger(&proj, file_path)?;
    let map = code_map_file_section(
        storage,
        file_path,
        map_off,
        MapScope::for_project(&proj, &[]),
    );
    if !ledger.symbols.is_empty() || !ledger.timeline.is_empty() {
        attach_attribution(storage, &mut ledger.symbols);
        let mut out = format::format_file_ledger(&ledger);
        if let Some((section, _)) = &map {
            out.push('\n');
            out.push_str(section);
        }
        return Ok(out);
    }

    // Secondary enrichment: fall back to FTS5 over chunk content. The ledger
    // is empty here, but that is not the same as "never extracted": a
    // supported-language file with no definitions and no recorded edits is
    // indexed and legitimately empty. Report the real state so the caller can
    // tell a coverage gap from an honest absence.
    // The path is matched as one phrase of its tokens (`csr engine src storage
    // mod rs`), not as free-text words: those fold a path into a single token
    // no chunk holds.
    // With a live session the window is wider than the page, so its sunk hits
    // can give their places to others; without one it is exactly `limit`.
    let live = live_session_id();
    let window = if live.is_some() {
        overfetch(limit)
    } else {
        limit
    };
    let hits = match crate::storage::queries::fts5_path_expr(file_path) {
        Some(expr) => storage.fts5_search_expr(&expr, window, project)?,
        None => Vec::new(),
    };
    let chunks: Vec<_> = hits.into_iter().map(|(chunk, _, _)| chunk).collect();
    let notes = csr_text_notes(storage, &chunks);
    // CSR-text rows after the clean ones, order kept inside each group
    let (clean, csr): (Vec<_>, Vec<_>) = chunks
        .into_iter()
        .partition(|chunk| !notes.contains_key(&chunk.id));
    let mut chunks: Vec<_> = clean.into_iter().chain(csr).collect();
    sink_session_chunks(&mut chunks, storage, live.as_deref(), file_path);
    chunks.truncate(limit);
    let Some((section, views)) = map else {
        return Ok(format::format_file_results(
            &chunks,
            file_path,
            ledger.indexed,
            &notes,
        ));
    };
    if chunks.is_empty() {
        // No conversation touched the file, but the code map holds it.
        return Ok(format::format_file_map_only(
            file_path,
            ledger.indexed,
            views,
            &section,
        ));
    }
    let mut out = format::format_file_results(&chunks, file_path, ledger.indexed, &notes);
    out.push('\n');
    out.push_str(&section);
    Ok(out)
}

/// Code-graph query: neighbors | callers | callees (no transitive impact in v1).
pub async fn code_graph(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    mode: &str,
    limit: usize,
) -> Result<String> {
    let project = cross_project::resolve_current_project().unwrap_or_default();
    let mut family_projects: Vec<String> = storage
        .with_connection(crate::storage::codegraph::project_names)?
        .into_iter()
        .filter(|candidate| {
            project.is_empty() || cross_project::same_project_family(&project, candidate)
        })
        .collect();
    family_projects.sort();
    family_projects.dedup();
    if !project.is_empty() && !family_projects.contains(&project) {
        family_projects.push(project.clone());
    }
    code_graph_for_projects(
        storage,
        symbol,
        file,
        mode,
        limit,
        &project,
        &family_projects,
    )
}

fn code_graph_for_projects(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    mode: &str,
    limit: usize,
    project: &str,
    family_projects: &[String],
) -> Result<String> {
    code_graph_with_map(
        storage,
        symbol,
        file,
        mode,
        limit,
        project,
        family_projects,
        crate::codemap::disabled(),
    )
}

/// [`code_graph_for_projects`] plus the code map: its definitions of the exact
/// symbol name (or its entry for the file) are appended to the graph answer, and
/// replace the empty answer when the conversation code graph does not know the
/// target. `map_off` is the injectable kill switch.
#[allow(clippy::too_many_arguments)]
pub(super) fn code_graph_with_map(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    mode: &str,
    limit: usize,
    project: &str,
    family_projects: &[String],
    map_off: bool,
) -> Result<String> {
    let (graph_knows, graph_out) =
        code_graph_core(storage, symbol, file, mode, limit, project, family_projects)?;
    if map_off {
        return Ok(graph_out);
    }
    // A specific project sees only its own repos in the map, as the graph does.
    let scope = MapScope::for_project(project, family_projects);
    // A symbol wins over a file, as in `resolve_node_id`.
    let section = match (
        symbol.filter(|s| !s.trim().is_empty()),
        file.filter(|f| !f.trim().is_empty()),
    ) {
        (Some(s), _) => code_map_definitions_section(storage, s, map_off, scope),
        (None, Some(f)) => {
            code_map_file_section(storage, f, map_off, scope).map(|(section, _)| section)
        }
        (None, None) => None,
    };
    let Some(section) = section else {
        return Ok(graph_out);
    };
    if graph_knows {
        return Ok(format!("{graph_out}\n{section}"));
    }
    let target_label = symbol.or(file).unwrap_or("");
    let shown_mode = match mode {
        "callers" | "callees" => mode,
        _ => "neighbors",
    };
    Ok(format::format_code_graph_from_map(
        shown_mode,
        target_label,
        &section,
    ))
}

/// The conversation code graph's answer, and whether the graph knows the target
/// at all (false when no node resolves, or a callers lookup finds nothing for a
/// symbol the graph has no node for).
fn code_graph_core(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    mode: &str,
    limit: usize,
    project: &str,
    family_projects: &[String],
) -> Result<(bool, String)> {
    let target_label = symbol.or(file).unwrap_or("").to_string();

    match mode {
        "callers" => {
            let name = match symbol {
                Some(s) if !s.is_empty() => s,
                _ => {
                    return Ok((
                        false,
                        format::format_code_graph(mode, &target_label, &[], &[]),
                    ))
                }
            };
            let mut nodes = storage.with_connection(|conn| {
                crate::storage::codegraph::query_callers_in_projects(
                    conn,
                    name,
                    family_projects,
                    guards::fetch_limit(limit),
                )
            })?;
            guards::keep_real_nodes(&mut nodes, limit);
            attach_attribution(storage, &mut nodes);
            let knows = !nodes.is_empty()
                || resolve_node_id(storage, symbol, file, project, family_projects)?.is_some();
            Ok((
                knows,
                format::format_code_graph(mode, &target_label, &nodes, &[]),
            ))
        }
        "callees" => {
            let node_id = match resolve_node_id(storage, symbol, file, project, family_projects)? {
                Some(id) => id,
                None => {
                    return Ok((
                        false,
                        format::format_code_graph(mode, &target_label, &[], &[]),
                    ))
                }
            };
            let mut nodes = storage.with_connection(|conn| {
                crate::storage::codegraph::query_callees_in_projects(
                    conn,
                    &node_id,
                    family_projects,
                    guards::fetch_limit(limit),
                )
            })?;
            guards::keep_real_nodes(&mut nodes, limit);
            attach_attribution(storage, &mut nodes);
            Ok((
                true,
                format::format_code_graph(mode, &target_label, &nodes, &[]),
            ))
        }
        _ => {
            // Default: neighbors (1-hop, both directions).
            let node_id = match resolve_node_id(storage, symbol, file, project, family_projects)? {
                Some(id) => id,
                None => {
                    return Ok((
                        false,
                        format::format_code_graph("neighbors", &target_label, &[], &[]),
                    ))
                }
            };
            let mut neighbors = storage.with_connection(|conn| {
                crate::storage::codegraph::query_neighbors_in_projects(
                    conn,
                    &node_id,
                    family_projects,
                    None,
                    guards::fetch_limit(limit),
                )
            })?;
            guards::keep_real_edges(&mut neighbors, limit);
            for ne in neighbors.iter_mut() {
                ne.node.attribution = if ne.node.id.is_empty() {
                    "unattributed".to_string()
                } else {
                    storage
                        .code_attribution_for_node(&ne.node.id)
                        .unwrap_or_else(|_| "unattributed".to_string())
                };
            }
            Ok((
                true,
                format::format_code_graph("neighbors", &target_label, &[], &neighbors),
            ))
        }
    }
}

/// Epsilon band for csr_why's recency tie-break (D2). Deliberately matches the
/// existing primacy-boost band in src/search/rerank.rs (PRIMACY_BAND = 0.05) — tight
/// on purpose. A wide band would bury older-but-still-valid evidence just to fix a
/// narrower failure (a stale fact outranking its own correction).
const WHY_RECENCY_EPSILON: f32 = 0.05;
const WHY_RECENCY_HOIST_MARKER: &str = " [recent↑]";

struct WhyRanking {
    items: Vec<crate::search::reinstatement::EvidenceItem>,
    hoisted_chunk_ids: HashSet<String>,
}

/// Anchor for csr_why's recency tie-break: (project, most-touched file of the
/// evidence item's source session). Resolved once per `why()` call via
/// `code_evolution` (`storage.files_for_session`) + chunk metadata
/// (`storage.get_chunks_by_ids`). Items with no resolvable file (reflections, pure-
/// chat sessions) are absent from the map and are therefore never tie-broken —
/// absence, not a wildcard, is the safe default. I/O only; contains no ranking
/// logic (that's in `apply_why_recency_tiebreak`, kept separate and pure so it's
/// unit-testable without a database).
fn resolve_why_anchors(
    storage: &Arc<Storage>,
    items: &[crate::search::reinstatement::EvidenceItem],
) -> HashMap<String, (String, String)> {
    let chunk_ids: Vec<String> = items.iter().map(|i| i.chunk_id.clone()).collect();
    let mut project_by_chunk: HashMap<String, String> = HashMap::new();
    if let Ok(chunks) = storage.get_chunks_by_ids(&chunk_ids) {
        for c in chunks {
            project_by_chunk.insert(c.id, c.project_name);
        }
    }

    let mut file_by_conv: HashMap<String, Option<String>> = HashMap::new();
    let mut anchors: HashMap<String, (String, String)> = HashMap::new();
    for item in items {
        let file = file_by_conv
            .entry(item.conversation_id.clone())
            .or_insert_with(|| {
                storage
                    .files_for_session(&item.conversation_id, 1)
                    .ok()
                    .and_then(|v| v.into_iter().next())
            })
            .clone();
        if let (Some(project), Some(file)) = (project_by_chunk.get(&item.chunk_id), file) {
            anchors.insert(item.chunk_id.clone(), (project.clone(), file));
        }
    }
    anchors
}

/// Pure secondary sort for csr_why (D2 fix): among evidence items that resolve to
/// the SAME anchor (same project + same file, see `resolve_why_anchors`) and whose
/// primary scores sit within `WHY_RECENCY_EPSILON` of each other, prefer the item
/// with the LATER timestamp. Every other pair keeps its primary score order
/// unchanged. No label, no annotation, no demotion, no dependence on dreaming or
/// supersession verdicts — this is a local, symmetric near-tie preference only.
/// Pure and deterministic: takes a precomputed anchor map so it is unit-testable
/// without touching storage.
fn apply_why_recency_tiebreak(
    mut items: Vec<crate::search::reinstatement::EvidenceItem>,
    anchors: &HashMap<String, (String, String)>,
) -> WhyRanking {
    // Two phases, because a pairwise "near-tie" predicate is NOT transitive and
    // must never be handed to sort_by: for same-anchor scores 0.90/0.86/0.82
    // with ascending timestamps, A beats B and B beats C but C beats A. Rust
    // requires a total order and may panic or order unpredictably otherwise.
    //
    // Phase 1: sort by score alone — a genuine total order.
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Deterministic final key so equal scores never depend on input order.
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });

    // Phase 2: reorder only within each maximal run of ADJACENT items that share
    // an anchor and sit within epsilon of the run's leader. Runs are disjoint, so
    // no cross-run comparison happens and no cycle is possible. Items with no
    // anchor, or whose neighbours differ, form runs of one and never move.
    let mut hoisted_chunk_ids = HashSet::new();
    let mut start = 0;
    while start < items.len() {
        let anchor = anchors.get(&items[start].chunk_id);
        let lead = items[start].score;
        let mut end = start + 1;
        if anchor.is_some() {
            while end < items.len()
                && anchors.get(&items[end].chunk_id) == anchor
                && (lead - items[end].score).abs() <= WHY_RECENCY_EPSILON
            {
                end += 1;
            }
        }
        if end - start > 1 {
            // Later timestamp first (RFC3339 sorts lexicographically by time).
            items[start..end].sort_by(|a, b| {
                b.timestamp
                    .cmp(&a.timestamp)
                    .then_with(|| a.chunk_id.cmp(&b.chunk_id))
            });
            for index in start..end {
                if items[index + 1..end]
                    .iter()
                    .any(|sibling| sibling.score > items[index].score)
                {
                    hoisted_chunk_ids.insert(items[index].chunk_id.clone());
                }
            }
        }
        start = end;
    }
    WhyRanking {
        items,
        hoisted_chunk_ids,
    }
}

/// Provenance recall: why does this code/decision exist. Reinstatement walk (seed ->
/// blend + code-graph spread + episode chain), formatted as a cited evidence chain
/// grouped by conversation. Project scope is normalized via
/// `cross_project::normalize_project_scope` (same as every other MCP tool).
pub async fn why(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
    cfg: &crate::search::reinstatement::ReinstateConfig,
) -> Result<String> {
    // With `CSR_STAGE_TIMING_FILE` set, the call appends its per-stage times
    // (the `w_*` stages) like a recall does.
    super::stage_timing::scope(
        query,
        why_untimed(storage, embeddings, search, query, project, cfg),
    )
    .await
}

async fn why_untimed(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
    cfg: &crate::search::reinstatement::ReinstateConfig,
) -> Result<String> {
    let (effective_project, _) = cross_project::normalize_project_scope(storage, project)?;

    let reinstatement = super::why_echo::reinstate_clean(
        storage,
        embeddings,
        search,
        query,
        effective_project.as_deref(),
        cfg,
    )
    .await?;

    // D2: local recency tie-break, csr_why only — see apply_why_recency_tiebreak.
    let w_start = Instant::now();
    let anchors = resolve_why_anchors(storage, &reinstatement.items);
    let ranking = apply_why_recency_tiebreak(reinstatement.items, &anchors);
    super::stage_timing::stage("w_anchors", w_start);
    let w_start = Instant::now();

    // TAD: log each returned chunk as an MCP-search retrieval event. session_id="mcp" is
    // a fixed label for the log (not the Claude Code session) — same pattern as
    // reflect_on_past. Non-fatal: a logging failure must never fail the search.
    for item in &ranking.items {
        let _ = storage.log_retrieval_event(&item.chunk_id, "chunk", "mcp_search", "mcp");
    }

    super::stage_timing::stage("w_retrieval_log", w_start);
    let w_start = Instant::now();
    let rendered = render_why_evidence(
        storage,
        query,
        ranking.items,
        &ranking.hoisted_chunk_ids,
        &reinstatement.trace,
    );
    super::stage_timing::stage("w_render", w_start);
    rendered
}

/// The one rendering of csr_why evidence, ranked or keyword: label the chunks
/// that hold CSR text, sink their evidence below the clean evidence, print, and
/// add the memory hop. Nothing is dropped.
fn render_why_evidence(
    storage: &Arc<Storage>,
    query: &str,
    items: Vec<crate::search::reinstatement::EvidenceItem>,
    hoisted_chunk_ids: &HashSet<String>,
    trace: &crate::search::reinstatement::ReinstateTrace,
) -> Result<String> {
    let items = super::why_echo::sink_echoes(storage, query, items);
    let labels = why_csr_text_labels(storage, &items);
    let items = sink_labeled_evidence(items, &labels);
    let mut rendered = format_why_labeled(query, &items, hoisted_chunk_ids, trace, &labels);
    append_memory_provenance_hop(storage, &items, &mut rendered)?;
    Ok(rendered)
}

/// Read-only additive join: for each conversation the evidence resolved to,
/// look up memory_registry rows whose origin_session_id matches that
/// conversation id, and append up to 3 "distilled into memory" lines
/// (newest first) to `out`. Zero matches => `out` is untouched (no bytes
/// added) so pre-existing csr_why output is byte-identical when no memory
/// references this conversation. Never affects ranking or evidence
/// selection — this runs strictly after `format_why` has already rendered
/// the ordered items.
fn append_memory_provenance_hop(
    storage: &Arc<Storage>,
    items: &[crate::search::reinstatement::EvidenceItem],
    out: &mut String,
) -> Result<()> {
    // Unique conversation ids, first-seen order (order doesn't matter for
    // correctness since we re-sort by freshness below, but keep it
    // deterministic and avoid duplicate queries for the same conversation).
    let mut seen_conv = HashSet::new();
    let mut hits: Vec<crate::storage::queries::MemoryRegistryRow> = Vec::new();
    for item in items {
        if !seen_conv.insert(item.conversation_id.clone()) {
            continue;
        }
        let rows = storage.with_connection(|conn| {
            crate::storage::queries::get_memory_registry_by_origin_session(
                conn,
                &item.conversation_id,
            )
        })?;
        hits.extend(rows);
    }

    if hits.is_empty() {
        return Ok(());
    }

    hits.sort_by_key(|row| {
        let key = row
            .modified_ts
            .as_deref()
            .and_then(crate::temporal::parse_timestamp)
            .map(|dt| dt.timestamp())
            .unwrap_or(row.file_mtime);
        std::cmp::Reverse(key)
    });
    hits.truncate(3);

    for row in &hits {
        let mem_type = row.mem_type.as_deref().unwrap_or("unknown");
        let description = sanitize_memory_description(row.description.as_deref().unwrap_or(""));
        // The link is frontmatter (origin_session_id), which anything that
        // can write a memory file can forge: label it, never raise it.
        out.push_str(&format!(
            "distilled into memory: {} ({}, {}) — {} [trust: unknown; frontmatter link, unverified]\n",
            row.slug, mem_type, row.project, description
        ));
    }

    Ok(())
}

fn sanitize_memory_description(s: &str) -> String {
    let flattened = s.replace(['\n', '\r'], " ");
    crate::format::truncate_chars(&flattened, 200).to_string()
}

/// Format evidence items as: header, grouped-by-conversation body (in the
/// ranked order `items` already carries), footer summary.
/// Stable sink: evidence from a chunk holding CSR text after the clean evidence,
/// order kept inside each group, nothing dropped.
fn sink_labeled_evidence(
    items: Vec<crate::search::reinstatement::EvidenceItem>,
    labels: &HashMap<String, String>,
) -> Vec<crate::search::reinstatement::EvidenceItem> {
    let (clean, csr): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|item| !labels.contains_key(&item.chunk_id));
    clean.into_iter().chain(csr).collect()
}

/// `[CSR text]` notes for the evidence chunks that hold CSR text, by chunk id.
/// The excerpt shown is a cut of the chunk; the detector reads the whole chunk.
fn why_csr_text_labels(
    storage: &Arc<Storage>,
    items: &[crate::search::reinstatement::EvidenceItem],
) -> HashMap<String, String> {
    let ids: Vec<String> = items.iter().map(|item| item.chunk_id.clone()).collect();
    match storage.get_chunks_by_ids(&ids) {
        Ok(chunks) => csr_text_notes(storage, &chunks),
        Err(_) => HashMap::new(),
    }
}

pub(crate) fn format_why(
    query: &str,
    items: &[crate::search::reinstatement::EvidenceItem],
    hoisted_chunk_ids: &HashSet<String>,
    trace: &crate::search::reinstatement::ReinstateTrace,
) -> String {
    format_why_labeled(query, items, hoisted_chunk_ids, trace, &HashMap::new())
}

pub(crate) fn format_why_labeled(
    query: &str,
    items: &[crate::search::reinstatement::EvidenceItem],
    hoisted_chunk_ids: &HashSet<String>,
    trace: &crate::search::reinstatement::ReinstateTrace,
    labels: &HashMap<String, String>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("WHY: {query}\n\n"));

    if items.is_empty() {
        out.push_str("No evidence chain found.\n");
    } else {
        // Group by conversation, preserving first-seen (= highest-relevance)
        // order. Evidence from a chunk holding CSR text is its own group: a
        // conversation's clean rows and its CSR-text rows are not merged, so the
        // sink (clean evidence before CSR-text evidence) survives the grouping.
        type Key = (String, bool);
        let mut order: Vec<Key> = Vec::new();
        let mut groups: HashMap<Key, Vec<&crate::search::reinstatement::EvidenceItem>> =
            HashMap::new();
        for item in items {
            let key = (
                item.conversation_id.clone(),
                labels.contains_key(&item.chunk_id),
            );
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(item);
        }

        for key in &order {
            let conv = &key.0;
            // Render in the order `items` already carries: score-descending, with
            // the D2 same-anchor recency tie-break applied.
            let group = groups.remove(key).unwrap_or_default();
            out.push_str(&format!("conv_{conv}:\n"));
            for it in &group {
                let score_marker = if hoisted_chunk_ids.contains(&it.chunk_id) {
                    WHY_RECENCY_HOIST_MARKER
                } else {
                    ""
                };
                let reached_by = it
                    .routes
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("+");
                let route_scores = it
                    .route_scores
                    .iter()
                    .map(|(route, score)| format!("{route}:{score:.3}"))
                    .collect::<Vec<_>>()
                    .join(",");
                out.push_str(&format!(
                    "  best_via={} reached_by={} route_scores={} score={:.3}{} trust={} [{}] conv_{}: {}\n",
                    it.best_route,
                    reached_by,
                    route_scores,
                    it.score,
                    score_marker,
                    it.trust,
                    format::age_stamp(&it.timestamp),
                    it.conversation_id,
                    it.excerpt
                ));
                if let Some(note) = labels.get(&it.chunk_id) {
                    out.push_str(&format!("    {note}\n"));
                }
            }
            out.push('\n');
        }
    }

    out.push_str(&crate::search::reinstatement::render_receipt(trace, items));
    out.push('\n');

    out
}

/// Resolve a symbol/file to a single best node id (highest rank).
fn resolve_node_id(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    project: &str,
    family_projects: &[String],
) -> Result<Option<String>> {
    if let Some(s) = symbol.filter(|s| !s.is_empty()) {
        let mut nodes = storage.code_nodes_by_name(s, "", i64::MAX as usize)?;
        nodes.retain(|node| !guards::is_scratch_path(&node.file));
        if !family_projects.is_empty() {
            nodes.retain(|node| family_projects.contains(&node.project));
            // `code_nodes_by_name` is already rank-descending. Stable sorting
            // only promotes an exact-project definition over equally valid
            // aliases without disturbing rank order inside either group.
            nodes.sort_by_key(|node| node.project != project);
        }
        return Ok(nodes.into_iter().next().map(|n| n.id));
    }
    if let Some(f) = file.filter(|f| !f.is_empty()) {
        let mut nodes = storage.all_code_nodes()?;
        nodes.retain(|node| {
            node.kind != "module"
                && !guards::is_scratch_path(&node.file)
                && (node.file == f || node.file.ends_with(f))
                && (family_projects.is_empty() || family_projects.contains(&node.project))
        });
        nodes.sort_by_key(|node| node.project != project);
        return Ok(nodes.into_iter().next().map(|n| n.id));
    }
    Ok(None)
}

/// Concept search — semantic search with concept-specific label.
pub async fn search_by_concept(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    concept: &str,
    limit: usize,
    project: Option<&str>,
) -> Result<String> {
    search_by_concept_at_root(storage, embeddings, search, concept, limit, project, None).await
}

/// [`search_by_concept`] with an injectable projects-root — same seam as
/// [`search_insights_at_root`], for the same hermetic-alias-widening test
/// reason.
#[allow(clippy::too_many_arguments)]
async fn search_by_concept_at_root(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    concept: &str,
    limit: usize,
    project: Option<&str>,
    projects_root_override: Option<&Path>,
) -> Result<String> {
    let (effective_project, scope_label) =
        cross_project::normalize_project_scope_at_root(storage, project, projects_root_override)?;
    let embed_started = Instant::now();
    let query_vec = embed_query(embeddings, concept).await?;
    let embed_ms = embed_started.elapsed().as_millis() as u64;
    let search_started = Instant::now();

    // Overfetch + partition over the full candidate set + single adaptive
    // refetch + truncate — see `fetch_enrich_partition_adaptive`.
    let enriched = if let Some(ref p) = effective_project {
        let ids = family_chunk_ids_at_root(storage, p, projects_root_override)?;
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks_filtered(&query_vec, n, 0.3, &ids),
            concept,
            limit,
            consumption_mode_for_partition(validity_partition_enabled()),
        )?
    } else {
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks(&query_vec, n, 0.3),
            concept,
            limit,
            consumption_mode_for_partition(validity_partition_enabled()),
        )?
    };
    // The page reports the time the call took; it used to print 0 for both.
    let search_ms = search_started.elapsed().as_millis() as u64;
    Ok(format::format_search_results(
        &enriched,
        concept,
        &scope_label,
        search_ms,
        embed_ms,
    ))
}

/// Pagination — get more results at offset.
#[allow(clippy::too_many_arguments)]
pub async fn get_more_results(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    offset: usize,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    validate_page(offset, limit)?;
    let consumption_mode = consumption_mode_for_partition(validity_partition_enabled());
    let active_forgetting = active_forgetting_enabled();
    let query_vec = embed_query(embeddings, query).await?;
    if active_forgetting {
        get_more_results_with_vec_active(
            storage,
            search,
            &query_vec,
            query,
            offset,
            limit,
            min_score,
            project,
            consumption_mode,
            true,
        )
        .await
    } else {
        get_more_results_with_vec(
            storage,
            search,
            &query_vec,
            query,
            offset,
            limit,
            min_score,
            project,
            consumption_mode,
        )
        .await
    }
}

/// Hard cap on the pagination candidate window — see
/// [`get_more_results_with_vec`]'s offset-independence contract.
const GET_MORE_WINDOW: usize = 500;

/// Everything in `get_more_results` after query embedding — the seam the
/// end-to-end partition test drives (same rationale as
/// `reflect_on_past_with_vec`). Enrichment + the validity partition run over
/// the FULL fetched set FIRST, and only then is the page cut with
/// skip/take — paginating first re-introduced demoted chunks into later
/// pages and let them displace valid candidates (issue 1; same discipline
/// as `apply_resolutions_before_limit`).
///
/// OFFSET-INDEPENDENT window: the candidate set is always the query's full
/// result list up to [`GET_MORE_WINDOW`], never a window grown from
/// `offset + limit` — repartitioning a per-offset-grown window could
/// duplicate demoted chunks across pages (sunk past one page's boundary,
/// re-fetched into the next) and skip valid candidates discovered only by
/// the larger window. Same query + same window + same data → same global
/// order for every page. Pagination stability holds while the underlying
/// data is unchanged — the standard offset-pagination contract.
#[allow(clippy::too_many_arguments)]
async fn get_more_results_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    offset: usize,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
    consumption_mode: ConsumptionMode,
) -> Result<String> {
    get_more_results_with_vec_active(
        storage,
        search,
        query_vec,
        query,
        offset,
        limit,
        min_score,
        project,
        consumption_mode,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn get_more_results_with_vec_active(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    offset: usize,
    limit: usize,
    min_score: f32,
    project: Option<&str>,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
) -> Result<String> {
    // Same scope resolution as the initial search: family-widened project
    // filter, and in scope=all the current-project family boost. Without
    // this, pagination reconstructed RAW HNSW order while page one was
    // boost-adjusted — a family chunk boosted onto page one reappeared on
    // page two (raw order) while the chunk it displaced was skipped
    // entirely (certified live with a 0.89-family / 0.90-other pair).
    let scope = resolve_search_scope(storage, project)?;
    get_more_results_in_scope(
        storage,
        search,
        query_vec,
        query,
        offset,
        limit,
        min_score,
        &scope,
        consumption_mode,
        active_forgetting,
    )
    .await
}

/// Everything after scope resolution — the seam pagination-ordering tests
/// drive with a constructed [`SearchProjectScope`] (no `MCP_CLIENT_CWD` env
/// dependency, same rationale as `reflect_on_past_with_vec_in_scope`).
#[allow(clippy::too_many_arguments)]
async fn get_more_results_in_scope(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    offset: usize,
    limit: usize,
    min_score: f32,
    scope: &SearchProjectScope,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
) -> Result<String> {
    validate_page(offset, limit)?;
    // The page is a slice of the settled pool page one cut its first rows
    // from: the same fusion, sinks and settling, computed once and kept, so
    // no page can disagree with another about the order. A pool that is gone
    // (expired, evicted, or too short for this offset) is rebuilt by the same
    // pipeline.
    let partition_enabled = validity_partition_enabled();
    let search_mode = search_mode_from(std::env::var("CSR_SEARCH_MODE").ok().as_deref());
    let key = PoolKey::new(
        query,
        scope,
        min_score,
        search_mode,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        storage.verdict_generation(),
    );
    let need = offset.saturating_add(limit);
    let pool = match cached_pool(storage, &key, need) {
        Some(pool) => pool,
        None => {
            let prior = cached_entry(storage, &key);
            let pool = settled_pool(
                storage,
                search,
                query_vec,
                query,
                need,
                min_score,
                scope,
                partition_enabled,
                consumption_mode,
                active_forgetting,
                "other",
                search_mode,
                RecallRerankMode::Runtime,
            )
            .await?;
            // What earlier pages showed stays where it was shown.
            let mut pool = match prior {
                Some(prior) => prior.widened_by(pool),
                None => pool,
            };
            pool.published = pool.published.max(need.min(pool.enriched.len()));
            cache_pool(storage, key, pool.clone());
            pool
        }
    };
    let total = pool.enriched.len();
    let page: Vec<EnrichedResult> = pool.enriched.into_iter().skip(offset).take(limit).collect();
    Ok(format::format_more_results(&page, query, offset, total))
}

/// Outcome of resolving a session id against the filesystem.
///
/// Adversarial review finding 5: a bare substring match used to return the
/// first filesystem-order hit even when it wasn't the only match — an
/// ambiguous short id could silently resolve to the wrong transcript
/// (different machine, different project). `Ambiguous` makes that
/// impossible to hit silently: callers must surface the candidate list.
pub(crate) enum ConversationLookup {
    Found(PathBuf, String),
    NotFound,
    /// More than one distinct file matched and no single exact stem match
    /// broke the tie. `(id, project)` pairs, sorted deterministically.
    Ambiguous(Vec<(String, String)>),
}

/// Locate a conversation's JSONL file by id across `projects_dir`
/// (optionally scoped to a project-name substring). Shared by
/// `get_full_conversation` and `crate::transcript::resolve_session_path` so
/// both surfaces agree on exactly one lookup rule: an exact stem match
/// always wins outright; otherwise every distinct substring match is
/// collected and, if more than one remains, the lookup is reported
/// `Ambiguous` rather than picking whichever the filesystem enumerated
/// first (`read_dir` order is not guaranteed and is not stable across
/// machines).
pub(crate) fn find_conversation_file(
    projects_dir: &Path,
    conversation_id: &str,
    project: Option<&str>,
) -> ConversationLookup {
    find_transcript_file(projects_dir, ".jsonl", conversation_id, project, false)
}

/// The same lookup over the transcript mirror's `<project>/<id>.jsonl.zst`
/// files, for when Claude Code has deleted the JSONL.
pub(crate) fn find_mirrored_conversation(
    data_dir: &Path,
    conversation_id: &str,
    project: Option<&str>,
) -> ConversationLookup {
    find_transcript_file(
        &crate::mirror::transcripts_dir(data_dir),
        ".jsonl.zst",
        conversation_id,
        project,
        true,
    )
}

/// `find_conversation_file`'s one rule, parameterized by the root to search and
/// the file-name suffix that marks a transcript there. `nested` also looks in
/// `<project>/<session>/subagents/agent-*` (the mirror archives those); a hit
/// there still reports its top-level project.
fn find_transcript_file(
    projects_dir: &Path,
    suffix: &str,
    conversation_id: &str,
    project: Option<&str>,
    nested: bool,
) -> ConversationLookup {
    // Validate conversation_id: must be non-empty, alphanumeric + hyphens + underscores only
    if conversation_id.is_empty()
        || !conversation_id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return ConversationLookup::NotFound;
    }

    // Search for JSONL file matching conversation_id
    let search_dirs: Vec<PathBuf> = if let Some(p) = project {
        // Search specific project directories matching the name
        match std::fs::read_dir(projects_dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&p.to_lowercase())
                })
                .map(|e| e.path())
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        match std::fs::read_dir(projects_dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect(),
            Err(_) => Vec::new(),
        }
    };

    // Collect every candidate .jsonl file whose stem matches, tagging
    // whether the match is exact.
    let mut candidates: Vec<(PathBuf, String, bool)> = Vec::new(); // (path, project_name, is_exact)
    for dir in &search_dirs {
        let project_name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut scan = |files: std::fs::ReadDir| {
            for entry in files.flatten() {
                let path = entry.path();
                let file_name = entry.file_name();
                if let Some(stem) = file_name.to_string_lossy().strip_suffix(suffix) {
                    let is_exact = stem == conversation_id;
                    if is_exact || stem.contains(conversation_id) {
                        candidates.push((path.clone(), project_name.clone(), is_exact));
                    }
                }
            }
        };
        if let Ok(files) = std::fs::read_dir(dir) {
            scan(files);
        }
        if nested {
            if let Ok(sessions) = std::fs::read_dir(dir) {
                for session in sessions.flatten().filter(|e| e.path().is_dir()) {
                    if let Ok(agents) = std::fs::read_dir(session.path().join("subagents")) {
                        scan(agents);
                    }
                }
            }
        }
    }

    // An exact stem match wins outright — but only if it's the only one
    // (two files with the literal same session id is pathological, and
    // still needs a human to disambiguate rather than a coin flip).
    let exact: Vec<&(PathBuf, String, bool)> = candidates.iter().filter(|c| c.2).collect();
    if exact.len() == 1 {
        let (path, project_name, _) = exact[0];
        return ConversationLookup::Found(path.clone(), project_name.clone());
    }
    if exact.len() > 1 {
        return ConversationLookup::Ambiguous(candidate_listing(&exact));
    }

    if candidates.is_empty() {
        return ConversationLookup::NotFound;
    }

    // Deduplicate by path (defensive: overlapping search_dirs should not
    // happen, but a duplicate PathBuf must never count as a second
    // distinct candidate).
    let mut distinct: Vec<&(PathBuf, String, bool)> = Vec::new();
    for c in &candidates {
        if !distinct.iter().any(|d| d.0 == c.0) {
            distinct.push(c);
        }
    }

    if distinct.len() == 1 {
        let (path, project_name, _) = distinct[0];
        return ConversationLookup::Found(path.clone(), project_name.clone());
    }

    ConversationLookup::Ambiguous(candidate_listing(&distinct))
}

/// Sorted, deterministic `(id, project)` listing for an ambiguous lookup —
/// order must not depend on filesystem enumeration order.
fn candidate_listing(candidates: &[&(PathBuf, String, bool)]) -> Vec<(String, String)> {
    let mut listing: Vec<(String, String)> = candidates
        .iter()
        .map(|(path, project_name, _)| {
            (
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .trim_end_matches(".zst")
                    .trim_end_matches(".jsonl")
                    .to_string(),
                project_name.clone(),
            )
        })
        .collect();
    listing.sort();
    listing.dedup();
    listing
}

/// Locate the JSONL file for a conversation. When the JSONL is gone but the
/// transcript mirror (under `data_dir`) still has it, point at the mirror copy
/// and say how to read it.
pub fn get_full_conversation(
    projects_dir: &Path,
    data_dir: Option<&Path>,
    conversation_id: &str,
    project: Option<&str>,
) -> String {
    get_full_conversation_with_db(projects_dir, data_dir, None, conversation_id, project)
}

/// [`get_full_conversation`], and when neither the JSONL nor the mirror copy
/// exists, answer from the session's indexed chunks in `db`: the rebuilt text
/// under a header that says it is lossy, cut at the same cap `csr_transcript`
/// uses. A session only the registry knows gets a registry-aware error.
pub fn get_full_conversation_with_db(
    projects_dir: &Path,
    data_dir: Option<&Path>,
    db: Option<&Storage>,
    conversation_id: &str,
    project: Option<&str>,
) -> String {
    use crate::transcript::chunks::{self, DbLookup};
    let mut lookup = find_conversation_file(projects_dir, conversation_id, project);
    let mut mirrored = false;
    if let (ConversationLookup::NotFound, Some(data_dir)) = (&lookup, data_dir) {
        lookup = find_mirrored_conversation(data_dir, conversation_id, project);
        mirrored = true;
    }
    match lookup {
        ConversationLookup::Found(path, project_name) if mirrored => {
            format::format_mirrored_conversation(
                conversation_id,
                &path.to_string_lossy(),
                &project_name,
            )
        }
        ConversationLookup::Found(path, project_name) => format::format_full_conversation(
            conversation_id,
            Some(&path.to_string_lossy()),
            Some(&project_name),
        ),
        ConversationLookup::NotFound => {
            match db.map(|d| chunks::lookup(d, conversation_id, project)) {
                Some(DbLookup::Rebuilt(rebuilt)) => {
                    chunks::full_conversation_text(&rebuilt, FULL_CONVERSATION_BUDGET_BYTES)
                }
                Some(DbLookup::Known(known)) => chunks::known_message(&known, false),
                Some(DbLookup::Ambiguous(candidates)) => {
                    format_ambiguous_conversation(conversation_id, &candidates)
                }
                Some(DbLookup::Unknown) | None => {
                    format::format_full_conversation(conversation_id, None, None)
                }
            }
        }
        ConversationLookup::Ambiguous(candidates) => {
            format_ambiguous_conversation(conversation_id, &candidates)
        }
    }
}

/// Same envelope style as `format::format_full_conversation`'s not-found
/// branch, for the ambiguous-substring-match case (finding 5): list every
/// distinct candidate instead of silently picking whichever the filesystem
/// happened to enumerate first. Kept local to this module (not added to
/// `crate::format`, which is out of scope for this fix) but mirrors its
/// XML-ish shape for consistency.
fn format_ambiguous_conversation(conversation_id: &str, candidates: &[(String, String)]) -> String {
    let listing = candidates
        .iter()
        .map(|(id, project)| format!("  <candidate id=\"{id}\" project=\"{project}\"/>"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<conversation_file>\n<error>Conversation ID '{conversation_id}' is ambiguous: {} files match this substring.</error>\n<candidates>\n{listing}\n</candidates>\n<suggestion>Use a longer/more specific id, or pass --project to narrow the search.</suggestion>\n</conversation_file>",
        candidates.len()
    )
}

/// Bytes of rebuilt text `get_full_conversation` returns: under the 20 KB cap,
/// room left for the closing envelope and the `csr_transcript` hint.
const FULL_CONVERSATION_BUDGET_BYTES: usize = 18_000;

pub use super::session_learnings::get_session_learnings;
pub use super::transcript_tool::{transcript, transcript_with_db};

// ─── Helpers ───

/// Embed a query string via spawn_blocking.
async fn embed_query(embeddings: &Arc<EmbeddingEngine>, query: &str) -> Result<Vec<f32>> {
    let q = query.to_string();
    let emb = embeddings.clone();
    tokio::task::spawn_blocking(move || emb.embed_single(&q)).await?
}

/// FIRST-window size for a requested page of `limit` results, so the
/// validity partition (and the resolution sink) has real candidates to
/// promote when it demotes a top-N hit: `limit + max(limit, 20)` extra,
/// capped at `3 * limit`. Fetching exactly `limit` meant one demoted hit
/// permanently displaced the valid N+1 candidate — it was never fetched at
/// all. When even this window is exhausted by demotions, callers make ONE
/// adaptive refetch at 10*limit — see `fetch_enrich_partition_adaptive`
/// and `reflect_on_past_with_vec`.
fn overfetch(limit: usize) -> usize {
    (limit + limit.max(20))
        .min(limit.saturating_mul(3))
        .max(limit)
}

/// Enrich search results with chunk metadata from storage — kill-switch
/// outcome read from the environment. Callers that must be testable without
/// mutating the process env (see `resolve_validity_with`) use
/// [`enrich_results_with`] directly.
fn enrich_results(
    storage: &Arc<Storage>,
    results: &[crate::search::SearchResult],
) -> Result<Vec<EnrichedResult>> {
    enrich_results_with(
        storage,
        results,
        consumption_mode_for_partition(validity_partition_enabled()),
    )
}

/// Core of [`enrich_results`] with the validity kill-switch outcome passed
/// in: dedupe, resolution-ledger sink, then the v10 validity partition —
/// over the FULL candidate set, before any caller-side cut.
fn enrich_results_with(
    storage: &Arc<Storage>,
    results: &[crate::search::SearchResult],
    consumption_mode: ConsumptionMode,
) -> Result<Vec<EnrichedResult>> {
    enrich_results_with_active_forgetting(storage, results, consumption_mode, false)
}

fn enrich_results_with_active_forgetting(
    storage: &Arc<Storage>,
    results: &[crate::search::SearchResult],
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
) -> Result<Vec<EnrichedResult>> {
    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&ids)?;

    let mut enriched_vec: Vec<EnrichedResult> = results
        .iter()
        .filter_map(|r| {
            chunks
                .iter()
                .find(|c| c.id == r.id)
                .map(|c| EnrichedResult {
                    score: r.score,
                    chunk: c.clone(),
                    resolution: None,
                    trust: crate::provenance::TrustTier::Unknown,
                    validity_demoted: false,
                })
        })
        .collect();
    format::dedupe_results(&mut enriched_vec);
    let validity_chunks: Vec<crate::import::ConversationChunk> =
        enriched_vec.iter().map(|e| e.chunk.clone()).collect();
    let validity = resolve_validity_with(storage, &validity_chunks, consumption_mode);
    if active_forgetting {
        let chunk_ids: Vec<&str> = enriched_vec.iter().map(|e| e.chunk.id.as_str()).collect();
        let tad_events = storage
            .get_retrieval_events_batch(&chunk_ids)
            .unwrap_or_default();
        let tad_config = decay::DecayConfig::for_search();
        let now = chrono::Utc::now();
        for e in &mut enriched_vec {
            if is_demote_channel(&validity, &e.chunk.id) {
                if let Ok(timestamp) = e.chunk.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
                    let events = tad_events
                        .get(&e.chunk.id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    e.score = apply_chunk_decay(
                        e.score,
                        &timestamp,
                        &now,
                        events,
                        &tad_config,
                        &validity,
                        &e.chunk.id,
                        None,
                        true,
                    );
                }
            }
        }
    }
    settle_window(&mut enriched_vec, storage, &validity, active_forgetting);
    Ok(enriched_vec)
}

/// Fetch → enrich → partition → single adaptive refetch → truncate, for the
/// simple search sites (`search_by_recency`, `search_by_concept`; the
/// primary path has its own richer pipeline in `reflect_on_past_with_vec`
/// with the same refetch rule). `fetch_fn(n)` runs the HNSW query at window
/// size `n`. First window is `overfetch(limit)`; when the partition leaves
/// fewer than `limit` valid (non-demoted) results AND that window came back
/// full (more candidates may exist beyond it), refetch ONCE at 10*limit
/// (hard cap) and repartition. If still short, accept: a query whose top
/// 10*limit candidates are all stale cannot backfill further — bounded by
/// design, not silent.
fn fetch_enrich_partition_adaptive(
    storage: &Arc<Storage>,
    fetch_fn: impl Fn(usize) -> Vec<crate::search::SearchResult>,
    query: &str,
    limit: usize,
    consumption_mode: ConsumptionMode,
) -> Result<Vec<EnrichedResult>> {
    let first = overfetch(limit);
    let results = fetch_fn(first);
    let window_full = results.len() == first;
    let mut enriched = enrich_results_with(storage, &results, consumption_mode)?;
    let valid = enriched.iter().filter(|e| !e.validity_demoted).count();
    if valid < limit && window_full {
        let refetch = limit.saturating_mul(10);
        if refetch > first {
            let results = fetch_fn(refetch);
            enriched = enrich_results_with(storage, &results, consumption_mode)?;
        }
    }
    // Before the cut, so the live chat's hits do not hold slots the page needs.
    sink_live_session(&mut enriched, storage, query);
    enriched.truncate(limit);
    Ok(enriched)
}

// ─── v10 dream-verdict validity partition ───
//
// Consumes `storage::chunk_binding::witness_verdict_for_chunks` (see that
// module's "Two-channel consumption" doc) at search time: Demote-channel
// chunks — the underlying symbol is gone/fully stale at the observed HEAD —
// sink below every non-demoted result; Annotate-channel chunks — the symbol
// merely evolved, still intact at HEAD — are annotated with no rank change.
// Same placement discipline as the resolution-ledger sink this mirrors
// (`apply_resolutions`, `apply_resolutions_before_limit`): resolved BEFORE
// the limit cut, so a demoted chunk never occupies a page slot that should
// go to the next best non-demoted candidate.

/// Per-chunk dream-verdict decision. Maps are keyed by chunk id; an absent
/// chunk has no verdict even when a sibling from the same conversation does.
/// `note` is the exact search-facing annotation string.
#[derive(Debug, Clone)]
pub(crate) struct ConvValidity {
    pub(crate) demote: bool,
    pub(crate) note: String,
    /// `demote` is set only because the chunk holds CSR text, not because of a
    /// dream verdict: the dream-verdict footer does not count it.
    pub(crate) csr_only_demote: bool,
}

/// Signals that may influence one search pass. Release ancestry is trusted
/// only when the validity batch covering the same candidates completed: if
/// that read fails, the existing validity partition still fails open, while
/// ancestry fails neutral so an unknown Demote channel cannot be stacked.
///
/// `ancestry_enabled` (the pre-existing `CSR_NO_VALIDITY_PARTITION` kill
/// switch outcome) and `consumption_enabled` (v10.1's new
/// `CSR_DREAM_CONSUMPTION`, default `annotate`) are DELIBERATELY separate
/// parameters below, never folded into one: `ancestry_enabled` gates whether
/// this pass loads ANY signals at all (ancestry included), while
/// `consumption_enabled` gates ONLY whether the resolved verdict map is
/// populated. Folding them (the rejected first T2 attempt's bug) meant
/// `CSR_DREAM_CONSUMPTION=off` silently killed release-
/// ancestry ranking too — an unrelated feature dream-verdict consumption
/// must never touch.
#[derive(Default)]
struct CandidateSignals {
    validity: HashMap<String, ConvValidity>,
    ancestry: HashMap<String, crate::storage::ancestry::AncestryLabel>,
    ancestry_allowed: bool,
}

impl CandidateSignals {
    fn load(
        storage: &Arc<Storage>,
        chunks: &[crate::import::ConversationChunk],
        ancestry_enabled: bool,
        consumption_mode: ConsumptionMode,
    ) -> Self {
        // CSR-text labels do not depend on the ancestry switch or on a dream
        // read: whatever else fails or is off, the labels stand.
        if !ancestry_enabled {
            return Self {
                validity: csr_text_labels_only(storage, chunks),
                ..Self::default()
            };
        }
        let conversation_ids = distinct_conversation_ids_of_chunks(chunks);
        let validity = match resolve_validity_checked(storage, chunks, consumption_mode) {
            Ok(validity) => validity,
            Err(_) if consumption_mode == ConsumptionMode::Full => {
                return Self {
                    validity: csr_text_labels_only(storage, chunks),
                    ..Self::default()
                }
            }
            Err(_) => csr_text_labels_only(storage, chunks),
        };
        let s_start = Instant::now();
        let ancestry = storage
            .ancestry_labels_for_conversations(&conversation_ids)
            .unwrap_or_default();
        super::stage_timing::stage("s_ancestry", s_start);
        Self {
            validity,
            ancestry,
            ancestry_allowed: true,
        }
    }

    /// Extend the same search pass for FTS-only conversations. Once any
    /// validity batch is unreliable, clear all ancestry already loaded for
    /// the pass and keep it disabled; semantic candidates must not retain a
    /// demotion signal that FTS candidates were denied for the same failure.
    fn extend(
        &mut self,
        storage: &Arc<Storage>,
        chunks: &[crate::import::ConversationChunk],
        ancestry_enabled: bool,
        consumption_mode: ConsumptionMode,
    ) -> bool {
        let ancestry_was_allowed = self.ancestry_allowed;
        if !ancestry_enabled {
            keep_csr_text_part(&mut self.validity);
            self.validity.extend(csr_text_labels_only(storage, chunks));
            self.ancestry.clear();
            self.ancestry_allowed = false;
            return ancestry_was_allowed;
        }
        let conversation_ids = distinct_conversation_ids_of_chunks(chunks);
        match resolve_validity_checked(storage, chunks, consumption_mode) {
            Ok(validity) => {
                self.validity.extend(validity);
                if self.ancestry_allowed {
                    self.ancestry.extend(
                        storage
                            .ancestry_labels_for_conversations(&conversation_ids)
                            .unwrap_or_default(),
                    );
                }
            }
            Err(_) => {
                self.validity.extend(csr_text_labels_only(storage, chunks));
                if consumption_mode == ConsumptionMode::Full {
                    self.ancestry.clear();
                    self.ancestry_allowed = false;
                }
            }
        }
        ancestry_was_allowed && !self.ancestry_allowed
    }
}

/// Distinct conversation ids over raw chunk metadata, first-seen order.
/// The storage lookup remains conversation-batched, while reduction below
/// restores the candidate chunk ids before any rank-affecting consumer runs.
fn distinct_conversation_ids_of_chunks(chunks: &[crate::import::ConversationChunk]) -> Vec<String> {
    distinct_conversation_ids_of(chunks.iter().map(|c| c.conversation_id.as_str()))
}

fn distinct_conversation_ids_of<'a>(ids: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.filter(|id| seen.insert(id.to_string()))
        .map(|id| id.to_string())
        .collect()
}

/// Kill switch: `CSR_NO_VALIDITY_PARTITION=1` (same idiom as
/// `CSR_NO_AI_NARRATIVES`) disables the whole v10 validity partition.
/// Default ON (feature enabled) — any other value, or the var unset, keeps
/// it on. A thin wrapper around the process env so [`resolve_validity`]'s
/// actual logic (`resolve_validity_with`) stays a pure function tests can
/// drive by parameter instead of by mutating global process state — two
/// tests calling `std::env::set_var`/`remove_var` on the same var from
/// different threads (cargo runs tests in parallel by default) is a real
/// race, not a hypothetical one (caught live: this exact var, this exact
/// pattern, before this comment existed).
fn validity_partition_enabled() -> bool {
    std::env::var("CSR_NO_VALIDITY_PARTITION").ok().as_deref() != Some("1")
}

fn consumption_mode_for_partition(partition_enabled: bool) -> ConsumptionMode {
    if partition_enabled {
        dream_consumption_mode()
    } else {
        ConsumptionMode::Off
    }
}

/// Opt-in active forgetting flag. Only the exact value `1` enables it;
/// unset and every other value preserve the current ranking byte-for-byte.
fn active_forgetting_enabled() -> bool {
    active_forgetting_enabled_from(std::env::var("CSR_ACTIVE_FORGETTING").ok().as_deref())
}

/// Pure parsing seam for [`active_forgetting_enabled`], avoiding process-env
/// mutation in parallel tests.
fn active_forgetting_enabled_from(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Resolve the v10 validity partition for a batch of chunks, with
/// the kill switch's outcome passed in rather than read from the
/// environment — every entry point evaluates
/// [`validity_partition_enabled`] exactly once and threads the outcome
/// here, so tests drive this by parameter instead of mutating the env var.
/// ONE batched
/// `witness_verdicts_for_chunks` query (one conversation-batched storage
/// read, never one query per chunk) when `enabled`, reduced per chunk via
/// [`ConvValidity`].
/// `enabled = false` returns an empty map — every consumer below treats an
/// absent chunk id as "nothing to do", so
/// an empty map disables the whole feature with no other branching needed.
/// Non-fatal: a storage error here must never fail the calling search (same
/// discipline as `apply_resolutions`).
fn resolve_validity_with(
    storage: &Arc<Storage>,
    chunks: &[crate::import::ConversationChunk],
    consumption_mode: ConsumptionMode,
) -> HashMap<String, ConvValidity> {
    // A failed verdict read costs the dream notes, never the CSR-text labels.
    resolve_validity_checked(storage, chunks, consumption_mode)
        .unwrap_or_else(|_| csr_text_labels_only(storage, chunks))
}

/// Reliability-preserving core for consumers that combine validity with a
/// second rank-affecting signal. The ordinary partition wrapper above keeps
/// its historical fail-open map, while ancestry-aware paths can distinguish
/// a trustworthy empty verdict batch from a failed read.
fn resolve_validity_checked(
    storage: &Arc<Storage>,
    chunks: &[crate::import::ConversationChunk],
    consumption_mode: ConsumptionMode,
) -> Result<HashMap<String, ConvValidity>> {
    if chunks.is_empty() {
        return Ok(HashMap::new());
    }
    // The dream switch turns the dream's own verdict notes off; labeling CSR's
    // text is a separate feature and does not depend on it.
    let mut validity = if consumption_mode == ConsumptionMode::Off {
        HashMap::new()
    } else {
        let identities: Vec<(String, String)> = chunks
            .iter()
            .map(|chunk| (chunk.id.clone(), chunk.conversation_id.clone()))
            .collect();
        let s_start = Instant::now();
        let hits = storage.witness_verdicts_for_chunks(&identities)?;
        super::stage_timing::stage("s_witness", s_start);
        reduce_validity_hits(hits, chunks)
    };
    // The dream mode gates what a dream verdict may do; CSR-text labels and their
    // demotion come after it and do not depend on it.
    normalize_validity_for_mode(&mut validity, consumption_mode);
    add_csr_text_labels(&mut validity, storage, chunks);
    Ok(validity)
}

/// Start of the annotation a result carries when its chunk holds CSR's own
/// output. Same style as `[stale anchor]` / `[evolved]`.
const CSR_TEXT_LABEL: &str = "[CSR text]";

fn csr_text_note(hit: &crate::import::contamination::Hit) -> String {
    format!(
        "{CSR_TEXT_LABEL} this chunk holds CSR's own output ({})",
        hit.formatter
    )
}

/// The `[CSR text]` note for each of these chunks that holds CSR text, for the
/// tools that list chunks without a validity pass. Read-only; no row is dropped.
fn csr_text_notes(
    storage: &Arc<Storage>,
    chunks: &[crate::import::ConversationChunk],
) -> HashMap<String, String> {
    storage
        .csr_text_hits(chunks)
        .iter()
        .map(|(id, hit)| (id.clone(), csr_text_note(hit)))
        .collect()
}

/// Label the results whose chunk text matches a CSR formatter's grammar, the
/// way a dream verdict labels a stale anchor: a `[CSR text]` note, and the same
/// rank sink wherever the dream consumption mode already sinks (`full`), never
/// a drop. Only the result set is looked at, never the corpus. The chunk is not
/// touched: CSR text is kept as it was written.
fn add_csr_text_labels(
    validity: &mut HashMap<String, ConvValidity>,
    storage: &Arc<Storage>,
    chunks: &[crate::import::ConversationChunk],
) {
    let s_start = Instant::now();
    let hits = storage.csr_text_hits(chunks);
    super::stage_timing::stage("s_csr_text", s_start);
    for chunk in chunks {
        let Some(hit) = hits.get(&chunk.id) else {
            continue;
        };
        let note = csr_text_note(hit);
        match validity.get_mut(&chunk.id) {
            Some(existing) => {
                existing.note = format!("{}; {note}", existing.note);
                existing.csr_only_demote = !existing.demote;
                existing.demote = true;
            }
            None => {
                validity.insert(
                    chunk.id.clone(),
                    ConvValidity {
                        demote: true,
                        note,
                        csr_only_demote: true,
                    },
                );
            }
        }
    }
}

/// The `[CSR text]` labels alone, for a pass whose dream verdicts are off,
/// unreadable, or cleared by the kill switch.
fn csr_text_labels_only(
    storage: &Arc<Storage>,
    chunks: &[crate::import::ConversationChunk],
) -> HashMap<String, ConvValidity> {
    let mut labels = HashMap::new();
    add_csr_text_labels(&mut labels, storage, chunks);
    labels
}

/// Drop the dream-verdict part of every entry, keeping its `[CSR text]` part.
fn keep_csr_text_part(validity: &mut HashMap<String, ConvValidity>) {
    validity.retain(|_, entry| match entry.note.find(CSR_TEXT_LABEL) {
        Some(at) => {
            entry.note = entry.note[at..].to_string();
            entry.demote = true;
            entry.csr_only_demote = true;
            true
        }
        None => false,
    });
}

/// Prompt-submit needs the same Demote predicate before applying release
/// ancestry. Unlike the normal validity path, preserve storage failure so
/// prompt scoring can fail open to no ancestry rather than risk stacking.
/// `None` means ancestry itself is unavailable (the pre-existing
/// `CSR_NO_VALIDITY_PARTITION` kill switch, or a genuine storage read
/// failure) — prompt-submit falls back to no ancestry either way.
/// `Some(map)` — possibly EMPTY when `CSR_DREAM_CONSUMPTION` is off — means
/// ancestry itself is fine to use; an empty map just means no verdict is
/// available to check for stacking, not that ancestry must be suppressed
/// (dream-verdict consumption and release-ancestry availability are
/// independent signals — see `CandidateSignals`'s doc).
pub(crate) fn resolve_validity_for_ancestry(
    storage: &Arc<Storage>,
    chunks: &[(String, String)],
) -> Option<HashMap<String, ConvValidity>> {
    if !validity_partition_enabled() {
        return None;
    }
    let consumption_mode = dream_consumption_mode();
    if consumption_mode == ConsumptionMode::Off || chunks.is_empty() {
        return Some(HashMap::new());
    }
    let hits = storage.witness_verdicts_for_chunks(chunks).ok()?;
    let mut validity = reduce_validity_hit_identities(hits, chunks);
    normalize_validity_for_mode(&mut validity, consumption_mode);
    Some(validity)
}

fn normalize_validity_for_mode(
    validity: &mut HashMap<String, ConvValidity>,
    consumption_mode: ConsumptionMode,
) {
    if consumption_mode != ConsumptionMode::Full {
        for verdict in validity.values_mut() {
            verdict.demote = false;
        }
    }
}

fn reduce_validity_hits(
    hits: BTreeMap<String, Vec<ChunkWitnessVerdict>>,
    chunks: &[crate::import::ConversationChunk],
) -> HashMap<String, ConvValidity> {
    let identities: Vec<(String, String)> = chunks
        .iter()
        .map(|chunk| (chunk.id.clone(), chunk.conversation_id.clone()))
        .collect();
    reduce_validity_hit_identities(hits, &identities)
}

fn reduce_validity_hit_identities(
    hits: BTreeMap<String, Vec<ChunkWitnessVerdict>>,
    chunks: &[(String, String)],
) -> HashMap<String, ConvValidity> {
    let chunk_ids: HashSet<&str> = chunks.iter().map(|(id, _)| id.as_str()).collect();
    let mut by_chunk: HashMap<String, Vec<ChunkWitnessVerdict>> = HashMap::new();
    for (stored_chunk_id, chunk_hits) in hits {
        if !chunk_ids.contains(stored_chunk_id.as_str()) {
            continue;
        }
        for hit in chunk_hits {
            if hit.chunk_id != stored_chunk_id {
                continue;
            }
            by_chunk
                .entry(stored_chunk_id.clone())
                .or_default()
                .push(hit);
        }
    }

    by_chunk
        .into_iter()
        .filter_map(|(chunk_id, list)| {
            let chosen = choose_validity_hit(list.iter())?;
            Some((
                chunk_id,
                ConvValidity {
                    demote: chosen.channel == VerdictChannel::Demote,
                    note: validity_note(chosen),
                    csr_only_demote: false,
                },
            ))
        })
        .collect()
}

fn choose_validity_hit<'a>(
    hits: impl Iterator<Item = &'a ChunkWitnessVerdict>,
) -> Option<&'a ChunkWitnessVerdict> {
    let hits: Vec<&ChunkWitnessVerdict> = hits.collect();
    hits.iter()
        .copied()
        .find(|hit| hit.channel == VerdictChannel::Demote)
        .or_else(|| {
            hits.iter()
                .copied()
                .find(|hit| hit.channel == VerdictChannel::Annotate)
        })
}

/// Render one dream-verdict hit as the search-facing annotation string —
/// exact wording is part of the v10 contract (tests match on it verbatim).
fn validity_note(hit: &ChunkWitnessVerdict) -> String {
    let symbol = hit.symbol.as_deref().unwrap_or(hit.file.as_str());
    let receipt = short_oid(hit.receipt_oid.as_deref().unwrap_or("unknown"));
    match hit.channel {
        VerdictChannel::Demote => {
            format!("[stale anchor] {symbol} no longer in current code (receipt {receipt})")
        }
        VerdictChannel::Annotate => {
            format!("[evolved] {symbol} changed since this conversation (as of {receipt})")
        }
    }
}

/// First 7 chars of a receipt oid (git short-SHA convention) — char-boundary
/// safe and never panics on a shorter fixture oid in tests.
fn short_oid(oid: &str) -> &str {
    match oid.char_indices().nth(7) {
        Some((byte_idx, _)) => &oid[..byte_idx],
        None => oid,
    }
}

/// `true` iff `chunk_id` is Demote-channel per `validity`. The ONE predicate
/// every validity-sensitive point shares — `reflect_on_past`'s TAD/decay loop,
/// its rerank-candidate filter, and `apply_validity_partition`'s own sink
/// all call this SAME function, so the three skip points cannot silently
/// drift out of sync with each other (a literal single source of truth for
/// "is this chunk about to be structurally demoted", rather than three
/// independently-maintained copies of the same `.get(...).is_some_and(...)`
/// check).
pub(crate) fn is_demote_channel(validity: &HashMap<String, ConvValidity>, chunk_id: &str) -> bool {
    validity
        .get(chunk_id)
        .is_some_and(|validity| validity.demote)
}

/// Score one semantic chunk from its raw similarity. Keeping this operation
/// in one helper lets an FTS validity failure replay semantic scoring with no
/// ancestry rather than leaving already-applied release decay behind.
#[allow(clippy::too_many_arguments)]
fn score_chunk_candidate(
    score: f32,
    chunk: &crate::import::ConversationChunk,
    now: &chrono::DateTime<chrono::Utc>,
    events: &[decay::RetrievalEvent],
    config: &decay::DecayConfig,
    validity: &HashMap<String, ConvValidity>,
    ancestry: Option<&crate::storage::ancestry::AncestryLabel>,
    active_forgetting: bool,
    scope: &SearchProjectScope,
    policy: RetrievalPolicy,
) -> (f32, bool) {
    let mut ancestry_applied = false;
    let ancestry = (!crate::search::rerank::is_scaffold_text(&chunk.content))
        .then_some(ancestry)
        .flatten();
    // Cross-encoder policy: the decay/TAD multiplier and the release-ancestry
    // multiplier are both 1.0. A Demote-channel chunk keeps its validity path.
    let skip_decay =
        policy == RetrievalPolicy::CrossEncoder && !is_demote_channel(validity, &chunk.id);
    let decayed_score = if skip_decay {
        score
    } else if let Ok(timestamp) = chunk.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
        ancestry_applied = !is_demote_channel(validity, &chunk.id)
            && timestamp < *now
            && ancestry
                .and_then(|label| label.releases_behind_for_decay())
                .is_some_and(|releases| releases > 0);
        apply_chunk_decay(
            score,
            &timestamp,
            now,
            events,
            config,
            validity,
            &chunk.id,
            ancestry,
            active_forgetting,
        )
    } else {
        score
    };
    let final_score = decayed_score * project_scope_multiplier(&chunk.project_name, scope);
    (final_score, ancestry_applied)
}

fn score_fts_candidate(
    chunk: &crate::import::ConversationChunk,
    now: &chrono::DateTime<chrono::Utc>,
    validity: &HashMap<String, ConvValidity>,
    ancestry: Option<&crate::storage::ancestry::AncestryLabel>,
    active_forgetting: bool,
    policy: RetrievalPolicy,
) -> (f32, bool) {
    let demoted = is_demote_channel(validity, &chunk.id);
    if demoted && !active_forgetting {
        return (crate::search::hybrid::FTS_ONLY_BASE_SCORE, false);
    }
    let Ok(timestamp) = chunk.timestamp.parse::<chrono::DateTime<chrono::Utc>>() else {
        return (
            if demoted {
                crate::search::hybrid::FTS_ONLY_BASE_SCORE
            } else {
                crate::search::hybrid::FTS_ONLY_UNDATED_SCORE
            },
            false,
        );
    };
    if demoted {
        return (
            decay::apply_decay_with_age_multiplier(
                crate::search::hybrid::FTS_ONLY_BASE_SCORE,
                &timestamp,
                now,
                None,
                None,
                ACTIVE_FORGETTING_DECAY_FACTOR,
            ),
            false,
        );
    }
    if policy == RetrievalPolicy::CrossEncoder {
        // No age decay, no release ancestry: the flat keyword score stands.
        return (crate::search::hybrid::FTS_ONLY_BASE_SCORE, false);
    }
    let releases_behind = (!crate::search::rerank::is_scaffold_text(&chunk.content))
        .then(|| ancestry.and_then(|label| label.releases_behind_for_decay()))
        .flatten();
    let ancestry_applied = timestamp < *now && releases_behind.is_some_and(|releases| releases > 0);
    (
        decay::apply_decay_with_release_ancestry(
            crate::search::hybrid::FTS_ONLY_BASE_SCORE,
            &timestamp,
            now,
            None,
            None,
            releases_behind,
        ),
        ancestry_applied,
    )
}

/// Apply the existing search TAD policy, with one opt-in exception: a
/// Demote-channel chunk gets accelerated effective age. With the flag off,
/// Demote keeps the exact v10 no-stacking score; Annotate follows the normal
/// decay path in both modes because it is evolved-not-gone.
#[allow(clippy::too_many_arguments)]
fn apply_chunk_decay(
    score: f32,
    timestamp: &chrono::DateTime<chrono::Utc>,
    now: &chrono::DateTime<chrono::Utc>,
    events: &[decay::RetrievalEvent],
    config: &decay::DecayConfig,
    validity: &HashMap<String, ConvValidity>,
    chunk_id: &str,
    ancestry: Option<&crate::storage::ancestry::AncestryLabel>,
    active_forgetting: bool,
) -> f32 {
    if is_demote_channel(validity, chunk_id) {
        if !active_forgetting {
            return score;
        }
        return decay::apply_tad_with_age_multiplier(
            score,
            timestamp,
            now,
            events,
            config,
            ACTIVE_FORGETTING_DECAY_FACTOR,
        );
    }
    decay::apply_tad_with_release_ancestry(
        score,
        timestamp,
        now,
        events,
        config,
        ancestry.and_then(|label| label.releases_behind_for_decay()),
    )
}

/// Apply the resolved [`ConvValidity`] decision over already-scored/reranked
/// `enriched`: Demote-channel chunks sink BELOW every non-demoted result.
/// With active forgetting off, stable order is preserved within each
/// partition, mirroring `apply_resolutions`'s resolved-ledger sink exactly.
/// With active forgetting on, only the demoted partition is sorted by its
/// accelerated-decay score; non-demoted ordering remains untouched.
/// Annotate-channel chunks are annotated IN PLACE, no rank change. Both
/// channels' note is appended to
/// `e.resolution` — merged with any resolution-ledger note already present
/// rather than overwriting it — so `format_search_results` and
/// `format_more_results` render it via the existing `<resolution>` tag with
/// no new field and no formatter signature change. No collision with the
/// ledger's own "N resolved item(s) demoted" footer count: that check is
/// `starts_with("resolved")`, and dream notes always start with
/// `[stale anchor]`/`[evolved]`.
///
/// NO STACKING by default (v10 contract): with active forgetting off, this
/// function remains the ONLY place a Demote-channel chunk's rank moves.
/// `CSR_ACTIVE_FORGETTING=1` adds accelerated temporal decay plus score order
/// within the already-sunk demoted section; provenance reranking still
/// excludes demoted chunks, so valid-section ranking cannot change.
fn apply_validity_partition(
    enriched: &mut Vec<EnrichedResult>,
    validity: &HashMap<String, ConvValidity>,
    active_forgetting: bool,
) {
    if validity.is_empty() {
        return;
    }
    for e in enriched.iter_mut() {
        if let Some(v) = validity.get(&e.chunk.id) {
            e.resolution = Some(match e.resolution.take() {
                Some(existing) => format!("{existing}; {}", v.note),
                None => v.note.clone(),
            });
        }
    }

    let mut kept = Vec::with_capacity(enriched.len());
    let mut demoted = Vec::new();
    for mut e in std::mem::take(enriched) {
        if is_demote_channel(validity, &e.chunk.id) {
            // The ONLY writer of this flag — `format_search_results`'s
            // dream-verdict footer counts it (never a substring of the
            // resolution note), so with the kill switch on (empty map, early
            // return above) the flag stays false everywhere and output is
            // byte-identical to pre-partition behavior. A chunk sunk only for
            // holding CSR text is not a dream verdict and is not counted there.
            e.validity_demoted = !validity.get(&e.chunk.id).is_some_and(|v| v.csr_only_demote);
            demoted.push(e);
        } else {
            kept.push(e);
        }
    }
    if active_forgetting {
        demoted.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    kept.extend(demoted);
    *enriched = kept;
}

/// Kill switch: `CSR_NO_ASSERTIONS=1` skips the S8 assertions stage. A thin
/// wrapper around the process env; the stage itself takes the outcome as a
/// parameter so tests never mutate the environment.
fn assertions_enabled() -> bool {
    assertions_partition::assertions_enabled_from(
        std::env::var("CSR_NO_ASSERTIONS").ok().as_deref(),
    )
}

/// How long the user-decision claim rules are reused before a re-read.
const CLAIM_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

struct ClaimCache {
    store: usize,
    read_at: Instant,
    claims: Arc<Vec<crate::storage::assertions::ClaimAssertion>>,
}

static CLAIM_CACHE: std::sync::Mutex<Option<ClaimCache>> = std::sync::Mutex::new(None);

/// The claim rules, read once per store per [`CLAIM_CACHE_TTL`]. Fails open: a
/// read error is an empty rule set and is not cached.
fn cached_claims(storage: &Storage) -> Arc<Vec<crate::storage::assertions::ClaimAssertion>> {
    let store = storage as *const Storage as usize;
    if let Ok(guard) = CLAIM_CACHE.lock() {
        if let Some(cache) = guard.as_ref() {
            if cache.store == store && cache.read_at.elapsed() < CLAIM_CACHE_TTL {
                return cache.claims.clone();
            }
        }
    }
    let Ok(claims) = storage.claim_assertions() else {
        return Arc::new(Vec::new());
    };
    let claims = Arc::new(claims);
    if let Ok(mut guard) = CLAIM_CACHE.lock() {
        *guard = Some(ClaimCache {
            store,
            read_at: Instant::now(),
            claims: claims.clone(),
        });
    }
    claims
}

/// Session key of every candidate row, for the modes that do not build one.
/// Empty when the stage is off, so the off path does no provenance reads.
fn assertions_session_keys(
    storage: &Storage,
    enriched: &[EnrichedResult],
    enabled: bool,
) -> HashMap<String, String> {
    if !enabled {
        return HashMap::new();
    }
    enriched
        .iter()
        .map(|e| (e.chunk.id.clone(), session_key_of(storage, &e.chunk)))
        .collect()
}

/// Merge the partition's receipt notes into `resolution` with the same
/// `existing; note` join `apply_validity_partition` uses. A witness-source
/// note is skipped for a chunk that already carries a `[stale anchor]` or
/// `[evolved]` validity note: that is the same witness verdict, said twice.
fn merge_assertion_notes(
    enriched: &mut [EnrichedResult],
    outcome: &assertions_partition::PartitionOutcome,
    by_target: &HashMap<String, crate::storage::assertions::Assertion>,
    validity: &HashMap<String, ConvValidity>,
) {
    for e in enriched.iter_mut() {
        let Some(note) = outcome.notes.get(&e.chunk.id) else {
            continue;
        };
        let witness_said = by_target.get(&e.chunk.id).is_some_and(|a| {
            a.source == "witness"
                && *note
                    == assertions_partition::note_for(&a.source, &a.state, &a.source, &a.receipt)
        });
        let validity_said = validity
            .get(&e.chunk.id)
            .is_some_and(|v| v.note.contains("[stale anchor]") || v.note.contains("[evolved]"));
        if witness_said && validity_said {
            continue;
        }
        e.resolution = Some(match e.resolution.take() {
            Some(existing) => format!("{existing}; {note}"),
            None => note.clone(),
        });
    }
}

/// S8 stage over the candidate rows (chunks and episodes): one batched
/// assertion read, claim matching over the candidate texts, receipt notes
/// merged into the rows. Returns the ids that must leave every ranking. Fails
/// open: a read error or a missing table is no partition and no note.
fn run_assertions_stage(
    storage: &Storage,
    enriched: &mut [EnrichedResult],
    session_keys: &HashMap<String, String>,
    validity: &HashMap<String, ConvValidity>,
    enabled: bool,
) -> HashSet<String> {
    if !enabled || enriched.is_empty() {
        return HashSet::new();
    }
    let started = Instant::now();
    let claims = cached_claims(storage);
    super::stage_timing::count("claims_read", claims.len() as u64);
    let sunk = run_assertions_stage_with(storage, &claims, enriched, session_keys, validity);
    super::stage_timing::stage("assertions", started);
    sunk
}

/// A cached witness assertion is a copy of a verdict that may since have been
/// reinstated, or that the consumption mode keeps from ranking. It may sink or
/// annotate a chunk only while the live validity for that chunk says demote
/// now. Anything else leaves the chunk to the validity pass: a `[stale anchor]`
/// or `[evolved]` note without a sink in annotate-only mode, nothing when the
/// verdict is gone. A demote that only labels CSR text is not a witness verdict.
fn retain_live_witness_assertions(
    by_target: &mut HashMap<String, crate::storage::assertions::Assertion>,
    validity: &HashMap<String, ConvValidity>,
) {
    by_target.retain(|id, assertion| {
        assertion.source != "witness"
            || assertion.target_kind != "chunk"
            || validity
                .get(id)
                .is_some_and(|v| v.demote && !v.csr_only_demote)
    });
}

/// [`run_assertions_stage`] with the claim rules passed in.
fn run_assertions_stage_with(
    storage: &Storage,
    claims: &[crate::storage::assertions::ClaimAssertion],
    enriched: &mut [EnrichedResult],
    session_keys: &HashMap<String, String>,
    validity: &HashMap<String, ConvValidity>,
) -> HashSet<String> {
    let (outcome, by_target) = {
        let units: Vec<assertions_partition::Unit<'_>> = enriched
            .iter()
            .map(|e| assertions_partition::Unit {
                id: &e.chunk.id,
                session: session_keys
                    .get(&e.chunk.id)
                    .map(String::as_str)
                    .unwrap_or(&e.chunk.id),
                text: &e.chunk.content,
            })
            .collect();
        let chunk_ids: Vec<&str> = units.iter().map(|u| u.id).collect();
        let mut seen = HashSet::new();
        let session_ids: Vec<&str> = units
            .iter()
            .map(|u| u.session)
            .filter(|s| seen.insert(*s))
            .collect();
        let mut by_target = storage
            .assertions_for_targets(&chunk_ids, &session_ids)
            .unwrap_or_default();
        retain_live_witness_assertions(&mut by_target, validity);
        if by_target.is_empty() && claims.is_empty() {
            return HashSet::new();
        }
        (
            assertions_partition::partition(&units, &by_target, claims),
            by_target,
        )
    };
    merge_assertion_notes(enriched, &outcome, &by_target, validity);
    if !outcome.sunk.is_empty() && crate::mcp::stage_probe::is_active() {
        let ranked: Vec<(String, String, f32)> = enriched
            .iter()
            .filter(|e| outcome.sunk.contains(&e.chunk.id))
            .map(|e| (e.chunk.id.clone(), e.chunk.conversation_id.clone(), e.score))
            .collect();
        crate::mcp::stage_probe::record_stage("assertions_partition", &ranked);
    }
    outcome.sunk
}

/// Label every result with its cached row-level floor. One indexed batch read
/// per family (chunks, then reflections); never a join to provenance events
/// or derivation rows. Unknown stays Unknown and is rendered, not dropped.
/// Non-fatal: a storage error leaves the Unknown default in place.
pub fn apply_trust_labels(enriched: &mut [EnrichedResult], storage: &Arc<Storage>) {
    if enriched.is_empty() {
        return;
    }
    let ids: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
    let Ok(floors) = storage.get_min_trust_batch(&ids) else {
        return;
    };
    for e in enriched.iter_mut() {
        if let Some(tier) = floors.get(&e.chunk.id) {
            e.trust = *tier;
        }
    }
}

/// Annotate `enriched` results with user-confirmed resolution-ledger verdicts
/// and stable-sink confirmed "resolved" entries to the bottom of the slice,
/// preserving relative order otherwise. Agent observations never annotate or
/// move results. Non-fatal: a storage error here must never fail the search.
pub fn apply_resolutions(enriched: &mut Vec<EnrichedResult>, storage: &Arc<Storage>) {
    if enriched.is_empty() {
        return;
    }
    let chunk_ids: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
    let ledger = match storage.get_resolutions_batch(&chunk_ids) {
        Ok(m) => m,
        Err(_) => return,
    };
    if ledger.is_empty() {
        return;
    }

    for e in enriched.iter_mut() {
        if let Some(entry) = ledger.get(&e.chunk.id) {
            e.resolution = format::resolution_note(
                &entry.status,
                &entry.evidence,
                &entry.created_at,
                &entry.source,
            );
        }
    }

    let mut unresolved = Vec::with_capacity(enriched.len());
    let mut resolved = Vec::new();
    for e in std::mem::take(enriched) {
        let is_resolved = ledger
            .get(&e.chunk.id)
            .map(|entry| {
                entry.source == crate::storage::queries::RESOLUTION_SOURCE_USER_CONFIRMED
                    && entry.status == "resolved"
            })
            .unwrap_or(false);
        if is_resolved {
            resolved.push(e);
        } else {
            unresolved.push(e);
        }
    }
    unresolved.extend(resolved);
    *enriched = unresolved;
}

/// Sink resolved-ledger AND dream-verdict-demoted chunks BEFORE the limit
/// cut, then truncate once. `validity` is the caller's already-resolved
/// [`ConvValidity`] decision (see `resolve_validity`) — passed in rather
/// than re-queried here so the whole search issues exactly one
/// `witness_verdicts_for_chunks` query (perf requirement).
fn apply_resolutions_before_limit(
    enriched: &mut Vec<EnrichedResult>,
    storage: &Arc<Storage>,
    validity: &HashMap<String, ConvValidity>,
    limit: usize,
    active_forgetting: bool,
) {
    settle_window(enriched, storage, validity, active_forgetting);
    enriched.truncate(limit);
}

/// The last ordering step of every result window, page one and every later
/// page alike: confirmed-resolved rows sink, trust labels are stamped, and
/// dream-demoted rows sink below the rest. It runs after the non-answer sink,
/// so a verified-resolved row stays below an unresolved one, and both stay
/// where the sinks before them put the rows that carry no answer.
fn settle_window(
    enriched: &mut Vec<EnrichedResult>,
    storage: &Arc<Storage>,
    validity: &HashMap<String, ConvValidity>,
    active_forgetting: bool,
) {
    apply_resolutions(enriched, storage);
    apply_trust_labels(enriched, storage);
    apply_validity_partition(enriched, validity, active_forgetting);
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tools_why_marker_tests.rs"]
mod why_marker_tests;

#[cfg(test)]
#[path = "tools_health_probe_tests.rs"]
mod health_probe_tests;

/// The live probe shape (2026-10-22, "why did we drop Docker and Qdrant"): the
/// question restated by a story, an extraction and a raw chunk; CSR's own
/// `csr_why` and search output; and the chunks that actually answer.
#[cfg(test)]
#[path = "tools_echo_recall_tests.rs"]
mod echo_recall_tests;

#[cfg(test)]
#[path = "tools_representative_tests.rs"]
mod representative_tests;

#[cfg(test)]
#[path = "tools_recall_lane_tests.rs"]
mod recall_lane_tests;
