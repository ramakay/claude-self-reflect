use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::format::{self, DisplayRankScore, EnrichedResult};
use crate::search::cross_project;
use crate::search::decay;
use crate::search::SearchEngine;
use crate::storage::chunk_binding::ChunkWitnessVerdict;
use crate::storage::recap_feeds::{dream_consumption_mode, ConsumptionMode};
use crate::storage::witness_verdicts::VerdictChannel;
use crate::storage::Storage;
use crate::temporal;

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
    let rerank_intent = if crate::search::trained_rerank::trained_rerank_requested() {
        crate::hooks::intent::ProbeSet::load_or_build(embeddings)
            .await
            .and_then(|probes| probes.classify(&query_vec))
            .map_or("other", |(intent, _)| match intent {
                crate::hooks::intent::Intent::Continue => "continue",
                crate::hooks::intent::Intent::StateRecall => "state_recall",
                crate::hooks::intent::Intent::Explore => "explore",
            })
    } else {
        "other"
    };

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

/// Reranker selection for the production recall pipeline. Runtime honors the
/// deployment flag/latest gate, Baseline forces the deterministic policy, and
/// Candidate applies an explicit not-yet-persisted model for offline gating.
#[derive(Clone, Copy)]
pub(crate) enum RecallRerankMode<'a> {
    Runtime,
    Baseline,
    Candidate(&'a crate::search::trained_rerank::LinearModel),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMode {
    Hybrid,
    Vector,
    Fts,
}

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Vector => "vector",
            Self::Fts => "fts",
        }
    }
}

pub fn search_mode_from(value: Option<&str>) -> SearchMode {
    match value {
        Some("vector") => SearchMode::Vector,
        Some("fts") => SearchMode::Fts,
        _ => SearchMode::Hybrid,
    }
}

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

fn fuse_rrf(semantic_order: &[String], fts_order: &[String], k: usize) -> Vec<String> {
    let mut fused_order = Vec::with_capacity(semantic_order.len() + fts_order.len());
    let mut scores: HashMap<String, f64> = HashMap::new();

    for ranking in [semantic_order, fts_order] {
        let mut seen = HashSet::new();
        for (rank, id) in ranking.iter().enumerate() {
            if !seen.insert(id.as_str()) {
                continue;
            }
            if !scores.contains_key(id) {
                fused_order.push(id.clone());
            }
            *scores.entry(id.clone()).or_default() += 1.0 / (k + rank) as f64;
        }
    }

    fused_order.sort_by(|left, right| scores[right].total_cmp(&scores[left]));
    fused_order
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
    let scope = resolve_search_scope(storage, project)?;
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
            -1.0,
            &scope,
            false,
            ConsumptionMode::Off,
            false,
            "explore",
            mode,
            RecallRerankMode::Baseline,
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
    // OVERFETCH (validity partition, issue 2): fetching exactly `limit`
    // candidates lets one demoted top-N hit permanently displace the valid
    // N+1 candidate — it was never fetched, so the sink has nothing to
    // promote. Fetch extra, partition over the full set, truncate to `limit`
    // at the very end (`apply_resolutions_before_limit`).
    let first = overfetch(limit);
    let mut pass = reflect_gather_pass(
        storage,
        search,
        query_vec,
        query,
        first,
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
    let mut search_ms = pass.search_ms;

    // ADAPTIVE REFETCH (single, bounded): when demotions leave fewer than
    // `limit` valid (non-demoted) results AND the first window came back
    // full (more candidates may exist beyond it), refetch ONCE at 10*limit
    // (hard cap) and repartition. If still short, accept: a query whose top
    // 10*limit candidates are all stale cannot backfill further — bounded
    // by design, not silent.
    let valid = pass
        .enriched
        .iter()
        .filter(|e| !is_demote_channel(&pass.validity, &e.chunk.id))
        .count();
    if valid < limit && pass.window_full {
        let refetch = limit.saturating_mul(10);
        if refetch > first {
            pass = reflect_gather_pass(
                storage,
                search,
                query_vec,
                query,
                refetch,
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
            search_ms += pass.search_ms;
        }
    }

    let mut enriched = pass.enriched;
    let display_rank_scores = pass.display_rank_scores;
    // Sink resolved chunks AND dream-verdict-demoted chunks BEFORE the limit
    // cut so stale results do not occupy slots that should go to
    // unresolved/non-demoted chunks ranked below them.
    apply_resolutions_before_limit(
        &mut enriched,
        storage,
        &pass.validity,
        limit,
        active_forgetting,
    );

    if crate::mcp::stage_probe::is_active() {
        let ranked: Vec<(String, String, f32)> = enriched
            .iter()
            .map(|e| (e.chunk.id.clone(), e.chunk.conversation_id.clone(), e.score))
            .collect();
        crate::mcp::stage_probe::record_stage("final_page", &ranked);
    }

    // TAD: log each RETURNED memory as an MCP-search retrieval event — after the
    // limit cut, so telemetry agrees with what the caller actually saw.
    // session_id="mcp" is a sentinel (MCP has no session id). Non-fatal.
    if record_retrievals {
        for e in &enriched {
            let _ = storage.log_retrieval_event(&e.chunk.id, "chunk", "mcp_search", "mcp");
        }
    }

    Ok(format::format_search_results_with_rank_scores(
        &enriched,
        query,
        &scope.scope_label,
        search_ms,
        embed_ms,
        &display_rank_scores,
    ))
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
    search_ms: u64,
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
    min_score: f32,
    scope: &SearchProjectScope,
    partition_enabled: bool,
    consumption_mode: ConsumptionMode,
    active_forgetting: bool,
    rerank_intent: &str,
    search_mode: SearchMode,
    rerank_mode: RecallRerankMode<'_>,
) -> Result<GatherPass> {
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
            idx.search_chunks_filtered(query_vec, fetch, min_score, &ids)
        } else {
            idx.search_chunks(query_vec, fetch, min_score)
        };
        let reflections = idx.search_reflections(query_vec, fetch, min_score);
        (chunks, reflections)
    };
    let search_ms = search_start.elapsed().as_millis() as u64;
    let mut window_full = chunk_results.len() == fetch || reflection_results.len() == fetch;

    // Enrich chunk results with metadata
    let chunk_ids: Vec<String> = chunk_results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&chunk_ids)?;

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
    let mut signals = CandidateSignals::load(storage, &chunks, partition_enabled, consumption_mode);
    let queried_chunk_ids: HashSet<String> = chunks.iter().map(|chunk| chunk.id.clone()).collect();

    let now = chrono::Utc::now();

    // Batch-fetch TAD events for all chunk results (single DB query)
    let chunk_ids_for_tad: Vec<&str> = chunk_results.iter().map(|r| r.id.as_str()).collect();
    let tad_events = storage
        .get_retrieval_events_batch(&chunk_ids_for_tad)
        .unwrap_or_default();
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
    for r in &reflection_results {
        reflection_raw_cosine.insert(r.id.clone(), r.score);
        if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(&r.id) {
            let decayed_score = if let Some(ts) = crate::temporal::parse_timestamp(&timestamp) {
                let events = reflection_tad_events
                    .get(&r.id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                decay::apply_tad(r.score, &ts, &now, events, &tad_config)
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

    let mut fts_order = Vec::new();
    let mut reflection_fts_hits: Vec<(String, usize, f64)> = Vec::new();
    let mut reflection_prefix_ids: Vec<String> = Vec::new();
    // Hybrid always supplements semantic retrieval with FTS5. Vector mode
    // skips FTS entirely; FTS mode reaches this block with no semantic hits.
    if search_mode != SearchMode::Vector {
        let fts_searches = if scope.effective_project.is_some() {
            let mut projects: Vec<&str> =
                scope.family_projects.iter().map(String::as_str).collect();
            projects.sort_unstable();
            projects
                .into_iter()
                .map(|project| storage.fts5_search(query, fetch, Some(project)))
                .collect::<Vec<_>>()
        } else {
            vec![storage.fts5_search(query, fetch, None)]
        };
        let mut fts_hits = Vec::new();
        let mut fts_window_full = false;
        for hits in fts_searches.into_iter().flatten() {
            fts_window_full |= hits.len() == fetch;
            fts_hits.extend(hits);
        }
        fts_hits.sort_by(|left, right| {
            left.2
                .total_cmp(&right.2)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.0.id.cmp(&right.0.id))
        });
        // Reflections have their own keyword index (`reflections_fts`). Merge
        // both families by bm25 into ONE keyword ranking, so a stored memory can
        // earn a keyword rank in the RRF fusion below instead of always arriving
        // semantic-only and losing to any chunk present in both lists.
        reflection_fts_hits = storage
            .fts5_search_reflections(query, fetch)
            .unwrap_or_default();
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
        reflection_prefix_ids = storage
            .fts5_reflection_prefix_matches(query, fetch)
            .unwrap_or_default();
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
        if !fts_hits.is_empty() {
            window_full |= fts_window_full;
            let existing_ids: HashSet<String> =
                enriched.iter().map(|e| e.chunk.id.clone()).collect();
            let appended: Vec<crate::import::ConversationChunk> = fts_hits
                .into_iter()
                .map(|(chunk, _, _)| chunk)
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
            let ancestry_revoked =
                signals.extend(storage, &extra_chunks, partition_enabled, consumption_mode);
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
    let candidates: Vec<crate::search::rerank::RankCandidate> = enriched
        .iter()
        .filter(|e| !is_demote_channel(&signals.validity, &e.chunk.id))
        .map(|e| crate::search::rerank::RankCandidate {
            id: e.chunk.id.clone(),
            cosine: e.score,
            content: e.chunk.content.clone(),
            provenance: storage.get_chunk_provenance(&e.chunk.id).ok().flatten(),
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
        || recall_display_rank_scores(&candidates),
        |rows| {
            rows.iter()
                .map(|(candidate, score)| (candidate.id.clone(), *score as f32))
                .collect()
        },
    );
    let order: Vec<String> = trained_order
        .map(|rows| rows.into_iter().map(|(candidate, _)| candidate).collect())
        .unwrap_or_else(|| crate::search::rerank::rerank(candidates))
        .into_iter()
        .map(|c| c.id)
        .collect();
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
    let final_order = match search_mode {
        SearchMode::Hybrid => fuse_rrf(&semantic_order, &fts_order, 60),
        SearchMode::Vector => order,
        SearchMode::Fts => fts_order,
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
    Ok(GatherPass {
        enriched,
        display_rank_scores,
        validity: signals.validity,
        window_full,
        search_ms,
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
    let id = uuid::Uuid::new_v4().to_string();

    let embedding = {
        let text = content.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&text)).await??
    };

    storage.insert_reflection(&id, content, tags, &embedding)?;

    {
        let mut idx = search.write().await;
        idx.insert_reflection(id.clone(), embedding);
    }

    Ok(format!(
        "Reflection stored successfully.\nID: {}\nTags: {:?}",
        id, tags
    ))
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

    Ok(format!(
        "recorded {} verdict(s): {} (source: {})",
        n, status, source
    ))
}

/// Quick existence check — count + top match only.
pub async fn quick_check(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    let query_vec = {
        let q = query.to_string();
        let emb = embeddings.clone();
        tokio::task::spawn_blocking(move || emb.embed_single(&q)).await??
    };

    quick_check_with_vec(storage, search, &query_vec, query, min_score, project).await
}

async fn quick_check_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    min_score: f32,
    project: Option<&str>,
) -> Result<String> {
    let (effective_project, _) = cross_project::normalize_project_scope(storage, project)?;
    let results = if let Some(ref project) = effective_project {
        let ids = family_chunk_ids(storage, project)?;
        let idx = search.read().await;
        idx.search_chunks_filtered(query_vec, 2, min_score, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(query_vec, 2, min_score)
    };

    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let chunks = storage.get_chunks_by_ids(&ids)?;

    let mut enriched: Vec<EnrichedResult> = results
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
    apply_trust_labels(&mut enriched, storage);

    Ok(format::format_quick_check(&enriched, query))
}

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
    Ok(format::format_recent_work(&chunks, group_by))
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
    // Parse time constraints
    let (start, end) = if let Some(tr) = time_range {
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
    };

    let start_str = start.to_rfc3339();
    let end_str = end.to_rfc3339();
    let time_desc = format!("{} to {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"));

    // Get chunk IDs within time range, then do filtered HNSW search
    let time_ids: HashSet<String> = storage
        .get_chunk_ids_in_timerange(&start_str, &end_str, project)?
        .into_iter()
        .collect();

    if time_ids.is_empty() {
        return Ok(format::format_recency_results(&[], query, &time_desc));
    }

    let query_vec = embed_query(embeddings, query).await?;
    // Overfetch + partition over the full candidate set + single adaptive
    // refetch + truncate — see `fetch_enrich_partition_adaptive`.
    let enriched = {
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks_filtered(&query_vec, n, min_score, &time_ids),
            limit,
            consumption_mode_for_partition(validity_partition_enabled()),
        )?
    };
    Ok(format::format_recency_results(&enriched, query, &time_desc))
}

/// Activity timeline.
pub async fn get_timeline(
    storage: &Arc<Storage>,
    time_range: &str,
    project: Option<&str>,
    granularity: &str,
) -> Result<String> {
    let (start, end) = temporal::parse_time_expression(time_range)?;
    let start_str = start.to_rfc3339();
    let end_str = end.to_rfc3339();
    let time_desc = format!("{} to {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"));

    let chunks = storage.get_chunks_in_timerange(&start_str, &end_str, project)?;
    let groups = temporal::group_chunks_by_period(&chunks, granularity);
    Ok(format::format_timeline(&groups, &time_desc))
}

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
    if !ledger.symbols.is_empty() || !ledger.timeline.is_empty() {
        attach_attribution(storage, &mut ledger.symbols);
        return Ok(format::format_file_ledger(&ledger));
    }

    // Secondary enrichment: fall back to FTS5 over chunk content. The ledger
    // is empty here, but that is not the same as "never extracted": a
    // supported-language file with no definitions and no recorded edits is
    // indexed and legitimately empty. Report the real state so the caller can
    // tell a coverage gap from an honest absence.
    let chunks: Vec<_> = storage
        .fts5_search(file_path, limit, project)?
        .into_iter()
        .map(|(chunk, _, _)| chunk)
        .collect();
    Ok(format::format_file_results(
        &chunks,
        file_path,
        ledger.indexed,
    ))
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
    let target_label = symbol.or(file).unwrap_or("").to_string();

    match mode {
        "callers" => {
            let name = match symbol {
                Some(s) if !s.is_empty() => s,
                _ => return Ok(format::format_code_graph(mode, &target_label, &[], &[])),
            };
            let mut nodes = storage.with_connection(|conn| {
                crate::storage::codegraph::query_callers_in_projects(
                    conn,
                    name,
                    family_projects,
                    limit,
                )
            })?;
            attach_attribution(storage, &mut nodes);
            Ok(format::format_code_graph(mode, &target_label, &nodes, &[]))
        }
        "callees" => {
            let node_id = match resolve_node_id(storage, symbol, file, project, family_projects)? {
                Some(id) => id,
                None => return Ok(format::format_code_graph(mode, &target_label, &[], &[])),
            };
            let mut nodes = storage.with_connection(|conn| {
                crate::storage::codegraph::query_callees_in_projects(
                    conn,
                    &node_id,
                    family_projects,
                    limit,
                )
            })?;
            attach_attribution(storage, &mut nodes);
            Ok(format::format_code_graph(mode, &target_label, &nodes, &[]))
        }
        _ => {
            // Default: neighbors (1-hop, both directions).
            let node_id = match resolve_node_id(storage, symbol, file, project, family_projects)? {
                Some(id) => id,
                None => {
                    return Ok(format::format_code_graph(
                        "neighbors",
                        &target_label,
                        &[],
                        &[],
                    ))
                }
            };
            let mut neighbors = storage.with_connection(|conn| {
                crate::storage::codegraph::query_neighbors_in_projects(
                    conn,
                    &node_id,
                    family_projects,
                    None,
                    limit,
                )
            })?;
            for ne in neighbors.iter_mut() {
                ne.node.attribution = if ne.node.id.is_empty() {
                    "unattributed".to_string()
                } else {
                    storage
                        .code_attribution_for_node(&ne.node.id)
                        .unwrap_or_else(|_| "unattributed".to_string())
                };
            }
            Ok(format::format_code_graph(
                "neighbors",
                &target_label,
                &[],
                &neighbors,
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
    let (effective_project, _) = cross_project::normalize_project_scope(storage, project)?;

    let reinstatement = crate::search::reinstatement::reinstate(
        storage,
        embeddings,
        search,
        query,
        effective_project.as_deref(),
        cfg,
    )
    .await?;

    // D2: local recency tie-break, csr_why only — see apply_why_recency_tiebreak.
    let anchors = resolve_why_anchors(storage, &reinstatement.items);
    let ranking = apply_why_recency_tiebreak(reinstatement.items, &anchors);

    // TAD: log each returned chunk as an MCP-search retrieval event. session_id="mcp" is
    // the sentinel (MCP has no session id) — same pattern as reflect_on_past. Non-fatal:
    // a logging failure must never fail the search.
    for item in &ranking.items {
        let _ = storage.log_retrieval_event(&item.chunk_id, "chunk", "mcp_search", "mcp");
    }

    let mut rendered = format_why(
        query,
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &reinstatement.trace,
    );

    append_memory_provenance_hop(storage, &ranking.items, &mut rendered)?;

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
pub(crate) fn format_why(
    query: &str,
    items: &[crate::search::reinstatement::EvidenceItem],
    hoisted_chunk_ids: &HashSet<String>,
    trace: &crate::search::reinstatement::ReinstateTrace,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("WHY: {query}\n\n"));

    if items.is_empty() {
        out.push_str("No evidence chain found.\n");
    } else {
        // Group by conversation, preserving first-seen (= highest-relevance) order.
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Vec<&crate::search::reinstatement::EvidenceItem>> =
            HashMap::new();
        for item in items {
            if !groups.contains_key(&item.conversation_id) {
                order.push(item.conversation_id.clone());
            }
            groups
                .entry(item.conversation_id.clone())
                .or_default()
                .push(item);
        }

        for conv in &order {
            // Render in the order `items` already carries: score-descending, with
            // the D2 same-anchor recency tie-break applied.
            let group = groups.remove(conv).unwrap_or_default();
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
    let query_vec = embed_query(embeddings, concept).await?;

    // Overfetch + partition over the full candidate set + single adaptive
    // refetch + truncate — see `fetch_enrich_partition_adaptive`.
    let enriched = if let Some(ref p) = effective_project {
        let ids = family_chunk_ids_at_root(storage, p, projects_root_override)?;
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks_filtered(&query_vec, n, 0.3, &ids),
            limit,
            consumption_mode_for_partition(validity_partition_enabled()),
        )?
    } else {
        let idx = search.read().await;
        fetch_enrich_partition_adaptive(
            storage,
            |n| idx.search_chunks(&query_vec, n, 0.3),
            limit,
            consumption_mode_for_partition(validity_partition_enabled()),
        )?
    };
    Ok(format::format_search_results(
        &enriched,
        concept,
        &scope_label,
        0,
        0,
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
    let mut all_results = if scope.effective_project.is_some() {
        let mut ids = HashSet::new();
        for family_project in &scope.family_projects {
            ids.extend(storage.get_chunk_ids_for_project(family_project)?);
        }
        let idx = search.read().await;
        idx.search_chunks_filtered(query_vec, GET_MORE_WINDOW, min_score, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(query_vec, GET_MORE_WINDOW, min_score)
    };

    // Apply the scope multiplier to the whole window and re-sort BEFORE
    // enrichment (which preserves input order), so every page is cut from
    // one boost-consistent global order. Remaining known divergence from
    // page one: pagination does not re-run TAD decay or provenance rerank.
    {
        let ids: Vec<String> = all_results.iter().map(|r| r.id.clone()).collect();
        let project_by_id: HashMap<String, String> = storage
            .get_chunks_by_ids(&ids)?
            .into_iter()
            .map(|chunk| (chunk.id.clone(), chunk.project_name))
            .collect();
        for result in &mut all_results {
            if let Some(project_name) = project_by_id.get(&result.id) {
                result.score *= project_scope_multiplier(project_name, scope);
            }
        }
        all_results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    // Enrich + resolution sink + validity partition ONCE over the FULL
    // fixed window, THEN slice the page.
    let enriched = enrich_results_with_active_forgetting(
        storage,
        &all_results,
        consumption_mode,
        active_forgetting,
    )?;
    let total = enriched.len();
    let page: Vec<EnrichedResult> = enriched.into_iter().skip(offset).take(limit).collect();
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
        if let Ok(files) = std::fs::read_dir(dir) {
            for entry in files.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "jsonl") {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                    let is_exact = &*stem == conversation_id;
                    if is_exact || stem.contains(conversation_id) {
                        let project_name = dir
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        candidates.push((path.clone(), project_name, is_exact));
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
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                project_name.clone(),
            )
        })
        .collect();
    listing.sort();
    listing.dedup();
    listing
}

/// Locate the JSONL file for a conversation.
pub fn get_full_conversation(
    projects_dir: &Path,
    conversation_id: &str,
    project: Option<&str>,
) -> String {
    match find_conversation_file(projects_dir, conversation_id, project) {
        ConversationLookup::Found(path, project_name) => format::format_full_conversation(
            conversation_id,
            Some(&path.to_string_lossy()),
            Some(&project_name),
        ),
        ConversationLookup::NotFound => {
            format::format_full_conversation(conversation_id, None, None)
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

/// Server-side cap on `csr_transcript`'s response, applied regardless of the
/// caller's requested `budget_chars` — bounds a misbehaving/huge request
/// against the MCP response size, matching the design doc's "token budget
/// enforced server-side" (Option C rationale).
const MCP_TRANSCRIPT_MAX_BUDGET_CHARS: usize = 60_000;

/// Thin wrapper over [`crate::transcript::run`] for the `csr_transcript` MCP
/// tool: validate the view/role/turns strings (never panics on a bad one —
/// same fail-honest posture as the CLI, but returns the message as `Ok`
/// content instead of exiting the process), clamp the budget, and dispatch
/// through the exact same core the CLI uses.
#[allow(clippy::too_many_arguments)]
pub fn transcript(
    projects_dir: &Path,
    session: &str,
    view: &str,
    project: Option<&str>,
    role: Option<&str>,
    turns: Option<&str>,
    last: Option<usize>,
    grep: Option<&str>,
    tool: Option<&str>,
    json: bool,
    budget_chars: Option<usize>,
    sidechains: bool,
    include_system: bool,
) -> Result<String> {
    use crate::transcript::{parse_turns_spec, RoleFilter, TranscriptRequest, ViewKind};

    let Some(view_kind) = ViewKind::parse(view) else {
        return Ok(format!(
            "<transcript_error>\n<error>unknown view '{view}' — expected one of: stats, prompts, tools, files, errors, slice, grep</error>\n</transcript_error>"
        ));
    };
    let role_filter = match role {
        Some(r) => match RoleFilter::parse(r) {
            Some(rf) => rf,
            None => {
                return Ok(format!(
                    "<transcript_error>\n<error>unknown role '{r}' — expected one of: user, assistant, system, all</error>\n</transcript_error>"
                ));
            }
        },
        None => RoleFilter::All,
    };
    let parsed_turns = match turns {
        Some(spec) => match parse_turns_spec(spec) {
            Ok(range) => Some(range),
            Err(e) => {
                return Ok(format!(
                    "<transcript_error>\n<error>{e}</error>\n</transcript_error>"
                ));
            }
        },
        None => None,
    };

    let req = TranscriptRequest {
        session: session.to_string(),
        view: view_kind,
        project: project.map(str::to_string),
        role: role_filter,
        turns: parsed_turns,
        last,
        grep: grep.map(str::to_string),
        tool: tool.map(str::to_string),
        json,
        budget_chars: budget_chars
            .unwrap_or(crate::transcript::DEFAULT_BUDGET_CHARS)
            .min(MCP_TRANSCRIPT_MAX_BUDGET_CHARS),
        sidechains,
        include_system,
    };

    Ok(crate::transcript::run(projects_dir, &req))
}

/// Get learnings for a specific session.
pub fn get_session_learnings(
    storage: &Arc<Storage>,
    session_id: &str,
    limit: usize,
) -> Result<String> {
    let tag = format!("session_{}", session_id);
    let results = storage.get_reflections_by_tag(&tag, limit)?;

    let learnings: Vec<(String, Vec<String>, String)> = results
        .into_iter()
        .map(|(_id, content, tags, timestamp)| (content, tags, timestamp))
        .collect();

    Ok(format::format_session_learnings(session_id, &learnings))
}

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
    apply_resolutions(&mut enriched_vec, storage);
    apply_trust_labels(&mut enriched_vec, storage);
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
    apply_validity_partition(&mut enriched_vec, &validity, active_forgetting);
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
}

/// Signals that may influence one search pass. Release ancestry is trusted
/// only when the validity batch covering the same candidates completed: if
/// that read fails, the existing validity partition still fails open, while
/// ancestry fails neutral so an unknown Demote channel cannot be stacked.
///
/// `ancestry_enabled` (the pre-existing `CSR_NO_VALIDITY_PARTITION` kill
/// switch outcome) and `consumption_enabled` (v10.1's new
/// `CSR_DREAM_CONSUMPTION`, default OFF) are DELIBERATELY separate
/// parameters below, never folded into one: `ancestry_enabled` gates whether
/// this pass loads ANY signals at all (ancestry included), while
/// `consumption_enabled` gates ONLY whether the resolved verdict map is
/// populated. Folding them (the rejected first T2 attempt's bug) meant
/// `CSR_DREAM_CONSUMPTION`'s default-OFF state silently killed release-
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
        if !ancestry_enabled {
            return Self::default();
        }
        let conversation_ids = distinct_conversation_ids_of_chunks(chunks);
        let validity = match resolve_validity_checked(storage, chunks, consumption_mode) {
            Ok(validity) => validity,
            Err(_) if consumption_mode == ConsumptionMode::Full => return Self::default(),
            Err(_) => HashMap::new(),
        };
        Self {
            validity,
            ancestry: storage
                .ancestry_labels_for_conversations(&conversation_ids)
                .unwrap_or_default(),
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
            self.validity.clear();
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
            Err(_) if consumption_mode == ConsumptionMode::Full => {
                self.ancestry.clear();
                self.ancestry_allowed = false;
            }
            Err(_) => {}
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
    resolve_validity_checked(storage, chunks, consumption_mode).unwrap_or_default()
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
    if consumption_mode == ConsumptionMode::Off || chunks.is_empty() {
        return Ok(HashMap::new());
    }
    let identities: Vec<(String, String)> = chunks
        .iter()
        .map(|chunk| (chunk.id.clone(), chunk.conversation_id.clone()))
        .collect();
    let hits = storage.witness_verdicts_for_chunks(&identities)?;
    let mut validity = reduce_validity_hits(hits, chunks);
    normalize_validity_for_mode(&mut validity, consumption_mode);
    Ok(validity)
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
    if consumption_mode == ConsumptionMode::AnnotateOnly {
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
) -> (f32, bool) {
    let mut ancestry_applied = false;
    let ancestry = (!crate::search::rerank::is_scaffold_text(&chunk.content))
        .then_some(ancestry)
        .flatten();
    let decayed_score =
        if let Ok(timestamp) = chunk.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
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
) -> (f32, bool) {
    let demoted = is_demote_channel(validity, &chunk.id);
    if demoted && !active_forgetting {
        return (0.45, false);
    }
    let Ok(timestamp) = chunk.timestamp.parse::<chrono::DateTime<chrono::Utc>>() else {
        return (if demoted { 0.45 } else { 0.40 }, false);
    };
    if demoted {
        return (
            decay::apply_decay_with_age_multiplier(
                0.45,
                &timestamp,
                now,
                None,
                None,
                ACTIVE_FORGETTING_DECAY_FACTOR,
            ),
            false,
        );
    }
    let releases_behind = (!crate::search::rerank::is_scaffold_text(&chunk.content))
        .then(|| ancestry.and_then(|label| label.releases_behind_for_decay()))
        .flatten();
    let ancestry_applied = timestamp < *now && releases_behind.is_some_and(|releases| releases > 0);
    (
        decay::apply_decay_with_release_ancestry(
            0.45,
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
            // byte-identical to pre-partition behavior.
            e.validity_demoted = true;
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
    apply_resolutions(enriched, storage);
    apply_trust_labels(enriched, storage);
    apply_validity_partition(enriched, validity, active_forgetting);
    enriched.truncate(limit);
}

#[cfg(test)]
mod tests {
    use super::*;
    // Pure parsing seam — tests drive it by parameter instead of mutating
    // the process env, so the non-test build has no use for it.
    use crate::storage::recap_feeds::{dream_consumption_mode_from, ConsumptionMode};

    fn unscoped_search_scope() -> SearchProjectScope {
        SearchProjectScope {
            effective_project: None,
            scope_label: "all".into(),
            current_project_for_all_scope: None,
            family_anchor: None,
            family_projects: HashSet::new(),
            projects_root_override: None,
        }
    }

    fn quick_check_fixture(
        rows: &[(&str, &str, &str, [f32; 4])],
    ) -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for (sequence, (id, project, content, vector)) in rows.iter().enumerate() {
            let chunk = crate::import::ConversationChunk {
                id: (*id).into(),
                conversation_id: format!("conv-{id}"),
                project_name: (*project).into(),
                timestamp: "2099-01-01T00:00:00Z".into(),
                content: (*content).into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: sequence,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, vector).unwrap();
            engine.insert_chunk(chunk.id, vector.to_vec());
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    #[tokio::test]
    async fn stage_probe_records_every_stage_and_final_rank_matches_the_returned_page() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        for i in 0..5 {
            let chunk = crate::import::ConversationChunk {
                id: format!("stage-probe-{i}"),
                conversation_id: format!("codex:stage-probe-conv-{i}"),
                project_name: "test".into(),
                timestamp: "2026-09-20T00:00:00Z".into(),
                content: format!("stage probe content {i}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            // Cosine similarity to the query decreases with i, so the target
            // (i=2) lands at a known, non-trivial rank.
            let vector = [1.0 - (i as f32) * 0.1, 0.05, 0.0, 0.0];
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(chunk.id, vector.to_vec());
        }
        let search = Arc::new(RwLock::new(engine));

        let (output, samples) = crate::mcp::stage_probe::scope("codex:stage-probe-conv-2", || {
            reflect_on_past_with_vec(
                &storage,
                &search,
                &query,
                "stage probe content",
                6,
                0.0,
                None,
                0,
                false,
                ConsumptionMode::Off,
                false,
            )
        })
        .await;
        let output = output.unwrap();
        assert!(
            output.contains("stage-probe-2"),
            "target chunk should be in the returned page: {output}"
        );

        let stage_names: Vec<&str> = samples.iter().map(|sample| sample.stage).collect();
        assert_eq!(
            stage_names,
            vec![
                "hnsw_fetch",
                "decay_enriched",
                "rerank",
                "fuse_rrf",
                "dedupe",
                "final_page",
            ],
            "every documented stage must record exactly once, in pipeline order"
        );
        for sample in &samples {
            assert!(
                sample.target_rank.is_some(),
                "target present at every stage with these fixtures: {} had none",
                sample.stage
            );
        }
        let final_page = samples.last().unwrap();
        assert_eq!(
            final_page.target_rank,
            Some(2),
            "target is the 3rd-most-similar of 5 chunks, 0-indexed rank 2"
        );

        let rerank_sample = samples
            .iter()
            .find(|sample| sample.stage == "rerank")
            .unwrap();
        assert!(
            !rerank_sample.rerank_breakdown.is_empty(),
            "rerank checkpoint must capture a per-term breakdown when the target survives to it"
        );
        let target_rows: Vec<_> = rerank_sample
            .rerank_breakdown
            .iter()
            .filter(|row| row.is_target)
            .collect();
        assert_eq!(
            target_rows.len(),
            1,
            "exactly one row should be flagged as the target's own best chunk"
        );
        assert_eq!(target_rows[0].chunk_id, "stage-probe-2");
        assert!(target_rows[0].breakdown.total.is_finite());
        let displacer_rows: Vec<_> = rerank_sample
            .rerank_breakdown
            .iter()
            .filter(|row| !row.is_target)
            .collect();
        assert_eq!(
            displacer_rows.len(),
            rerank_sample.target_rank.unwrap(),
            "displacer count must equal the target's own rerank rank (everyone ranked above it)"
        );

        let fuse_rrf_sample = samples
            .iter()
            .find(|sample| sample.stage == "fuse_rrf")
            .unwrap();
        assert!(
            fuse_rrf_sample.target_fts_rank.is_some(),
            "target's own content is FTS-matchable, so fuse_rrf must also \
             record its keyword-side rank, not just its fused rank"
        );
    }

    #[tokio::test]
    async fn stage_probe_is_inert_outside_an_active_scope() {
        // `is_active()` must be false with no `scope()` wrapper — the
        // production hot path (MCP tool, hooks) never wraps calls in
        // `scope`, so every `(String, String, f32)` vector build gated on it
        // must be skipped there. Exercised indirectly here: a call outside
        // `scope` returns zero samples via the same production code path
        // that skips the vector builds.
        assert!(!crate::mcp::stage_probe::is_active());
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let chunk = crate::import::ConversationChunk {
            id: "no-scope-0".into(),
            conversation_id: "codex:no-scope-conv".into(),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: "no scope active".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let vector = [1.0_f32, 0.0, 0.0, 0.0];
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(chunk.id, vector.to_vec());
        let search = Arc::new(RwLock::new(engine));

        // No `scope()` wrapper — this must succeed exactly like production
        // callers, and `is_active()` must read false throughout.
        let output = reflect_on_past_with_vec(
            &storage,
            &search,
            &vector,
            "no scope active",
            6,
            0.0,
            None,
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(output.contains("no-scope-0"));
        assert!(!crate::mcp::stage_probe::is_active());
    }

    /// Two `scope()` calls interleaved via `tokio::join!` inside the SAME
    /// task must not cross-contaminate: each records samples only for its
    /// own target. `LocalKey::scope` sets/restores the task-local value
    /// around each individual `poll()`, so sibling scoped futures polled in
    /// turn never see each other's state — the property a naive
    /// process-global `static` would not have.
    #[tokio::test]
    async fn stage_probe_scopes_do_not_cross_contaminate_concurrent_queries() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        for i in 0..3 {
            let chunk = crate::import::ConversationChunk {
                id: format!("concurrent-{i}"),
                conversation_id: format!("codex:concurrent-conv-{i}"),
                project_name: "test".into(),
                timestamp: "2026-09-20T00:00:00Z".into(),
                content: format!("concurrent probe content {i}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            let vector = [1.0 - (i as f32) * 0.1, 0.05, 0.0, 0.0];
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(chunk.id, vector.to_vec());
        }
        let search = Arc::new(RwLock::new(engine));

        let run = |target: &'static str| {
            let storage = storage.clone();
            let search = search.clone();
            async move {
                crate::mcp::stage_probe::scope(target, || {
                    reflect_on_past_with_vec(
                        &storage,
                        &search,
                        &query,
                        "concurrent probe content",
                        6,
                        0.0,
                        None,
                        0,
                        false,
                        ConsumptionMode::Off,
                        false,
                    )
                })
                .await
            }
        };
        let (first, second) = tokio::join!(
            run("codex:concurrent-conv-0"),
            run("codex:concurrent-conv-1")
        );
        let (_, first_samples) = first;
        let (_, second_samples) = second;
        assert_eq!(
            first_samples.last().unwrap().target_rank,
            Some(0),
            "conv-0 probe must see its own target, not the other task's"
        );
        assert_eq!(
            second_samples.last().unwrap().target_rank,
            Some(1),
            "conv-1 probe must see its own target, not the other task's"
        );
    }

    #[test]
    fn explicit_memory_is_told_apart_from_derived_reflections() {
        let uuid = "5eb5aebb-65ab-4fba-9917-86cb83845154";
        assert!(is_explicit_memory(
            "story_x",
            &[EXPLICIT_MEMORY_TAG.to_string()]
        ));
        assert!(is_explicit_memory(
            uuid,
            &["anukriti".into(), "memory-archive".into()]
        ));
        assert!(is_explicit_memory(uuid, &[]));
        assert!(!is_explicit_memory(
            uuid,
            &["session_story".into(), "conv_x".into()]
        ));
        assert!(!is_explicit_memory(uuid, &["session_episode".into()]));
        assert!(!is_explicit_memory(
            uuid,
            &["session_briefing".into(), "schema_v1".into()]
        ));
        assert!(!is_explicit_memory(
            uuid,
            &["narrative_extracted_v3".into(), "status_partial".into()]
        ));
        assert!(!is_explicit_memory("v3_abc", &["context_cache".into()]));
        assert!(!is_explicit_memory("extracted_1", &["free".into()]));
    }

    /// Regression for the live bug: `store_reflection`'s shared insert path
    /// stamped `csr:memory` onto every derived narrative it wrote (stories,
    /// episodes, ...), so a row could carry BOTH the stamp and a derived tag.
    /// The derived tag must win — a pipeline narrative is never a deliberate
    /// memory write, whatever it happens to be stamped with.
    #[test]
    fn derived_tag_overrides_explicit_memory_stamp() {
        let uuid = "5eb5aebb-65ab-4fba-9917-86cb83845154";
        assert!(
            !is_explicit_memory(
                uuid,
                &["session_story".into(), EXPLICIT_MEMORY_TAG.to_string()]
            ),
            "a stamped session_story must not count as an explicit memory"
        );
        assert!(!is_explicit_memory(
            uuid,
            &[
                "narrative_extracted_v3".into(),
                EXPLICIT_MEMORY_TAG.to_string()
            ]
        ));
    }

    /// The bug this closes: `summarizer.rs` wrote stories through the same
    /// `store_reflection` helper the MCP tool uses, so every story got the
    /// `csr:memory` stamp and, via `reflection_recall_standing`, the flat
    /// `W_USER` boost `derived_story_keeps_no_standing_against_user_chunks`
    /// exists to forbid. That test only caught it for reflections written
    /// directly through storage; this one exercises the actual write path a
    /// story takes (`store_reflection_internal`) and proves it does NOT
    /// stamp. Revert the split in `store_reflection`/`store_reflection_internal`
    /// and this fails: the row would carry `csr:memory`.
    #[tokio::test]
    async fn summarizer_story_write_path_does_not_stamp_explicit_memory() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let engine = SearchEngine::new(EmbeddingEngine::dimension());
        let search = Arc::new(RwLock::new(engine));
        let tags = vec![
            "session_story".to_string(),
            "project_test".to_string(),
            "conv_abc".to_string(),
        ];
        let result = store_reflection_internal(
            &storage,
            &embeddings,
            &search,
            "a synthesized session story",
            &tags,
        )
        .await
        .unwrap();
        let id = result
            .lines()
            .find_map(|l| l.strip_prefix("ID: "))
            .expect("id line")
            .to_string();
        let (_, stored_tags, _) = storage.get_reflection_by_id(&id).unwrap().unwrap();
        assert!(
            !stored_tags.iter().any(|t| t == EXPLICIT_MEMORY_TAG),
            "the summarizer's write path must not stamp explicit-memory standing: {stored_tags:?}"
        );
        assert!(!is_explicit_memory(&id, &stored_tags));
    }

    async fn first_cid_for_scope(project: &str, chunk_cos: f32, story_cos: f32) -> String {
        first_cid_for_scope_tagged(
            project,
            chunk_cos,
            story_cos,
            &["session_story", "project_sinkscope-away"],
        )
        .await
    }

    async fn first_cid_for_scope_tagged(
        project: &str,
        chunk_cos: f32,
        story_cos: f32,
        story_tags: &[&str],
    ) -> String {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let chunk_vec = vec![chunk_cos, (1.0 - chunk_cos * chunk_cos).sqrt(), 0.0, 0.0];
        let chunk = crate::import::ConversationChunk {
            id: "in-family-chunk".into(),
            conversation_id: "conv-in-family".into(),
            project_name: "sinkscope-home".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: "deploy notes for the home project".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &chunk_vec).unwrap();
        engine.insert_chunk(chunk.id.clone(), chunk_vec);
        let story_vec = vec![story_cos, (1.0 - story_cos * story_cos).sqrt(), 0.0, 0.0];
        storage
            .insert_reflection(
                "story_elsewhere",
                "a story from a different project",
                &story_tags.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
                &story_vec,
            )
            .unwrap();
        engine.insert_reflection("story_elsewhere".to_string(), story_vec);
        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "deploy notes",
            2,
            0.0,
            Some(project),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        let start = out.find("<cid>").expect("at least one result") + "<cid>".len();
        let end = out[start..].find("</cid>").unwrap() + start;
        out[start..end].to_string()
    }

    // Vector mode already multiplies an out-of-family score by 0.3, so the
    // story must beat the chunk after that multiplier for the sink to decide.
    #[tokio::test]
    async fn explicit_scope_sinks_weak_out_of_family_story_below_in_family_hits() {
        // story 0.40 * 0.3 = 0.12 beats chunk 0.10 without the sink.
        assert_eq!(
            first_cid_for_scope("sinkscope-home", 0.10, 0.40).await,
            "conv-in-family"
        );
    }

    #[tokio::test]
    async fn explicit_scope_keeps_a_strong_out_of_family_story_in_place() {
        // story 0.95 clears QUICK_CHECK_FLOOR; 0.95 * 0.3 = 0.285 beats 0.20.
        assert_ne!(
            first_cid_for_scope("sinkscope-home", 0.20, 0.95).await,
            "conv-in-family"
        );
    }

    #[tokio::test]
    async fn bare_family_tag_keeps_a_memory_with_a_topical_project_tag_in_family() {
        // Live shape of e97fa3b4: a bare project tag plus a topical project_
        // tag that resolves the row's project elsewhere. It must not sink.
        assert_ne!(
            first_cid_for_scope_tagged(
                "sinkscope-home",
                0.10,
                0.40,
                &["sinkscope-home", "project_sinkscope-topic"],
            )
            .await,
            "conv-in-family"
        );
    }

    #[tokio::test]
    async fn explicit_scope_keeps_a_keyword_matched_out_of_family_story_in_place() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let chunk_vec = vec![0.10_f32, (1.0_f32 - 0.01).sqrt(), 0.0, 0.0];
        let chunk = crate::import::ConversationChunk {
            id: "in-family-chunk".into(),
            conversation_id: "conv-in-family".into(),
            project_name: "sinkscope-home".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: "unrelated deploy notes".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &chunk_vec).unwrap();
        engine.insert_chunk(chunk.id.clone(), chunk_vec);
        // Weak semantically (0.40, under the floor) but its text carries the
        // query's own terms, so the keyword channel finds it.
        let story_vec = vec![0.40_f32, (1.0_f32 - 0.16).sqrt(), 0.0, 0.0];
        storage
            .insert_reflection(
                "story_sibling",
                "morning mantras pronunciation comment on the reel",
                &[
                    "session_story".to_string(),
                    "project_sinkscope-away".to_string(),
                ],
                &story_vec,
            )
            .unwrap();
        engine.insert_reflection("story_sibling".to_string(), story_vec);
        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "morning mantras pronunciation",
            2,
            0.0,
            Some("sinkscope-home"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        let start = out.find("<cid>").expect("at least one result") + "<cid>".len();
        let end = out[start..].find("</cid>").unwrap() + start;
        assert_ne!(
            &out[start..end],
            "conv-in-family",
            "a keyword-matched sibling-project story must keep its fused rank:\n{out}"
        );
    }

    #[tokio::test]
    async fn unscoped_search_keeps_the_better_out_of_family_story_first() {
        assert_ne!(
            first_cid_for_scope("all", 0.10, 0.40).await,
            "conv-in-family"
        );
    }

    /// Seven user-authored chunks at cosine 0.63 to the query (the live shape
    /// measured 2026-09-22: one recap paragraph quoted into many sessions)
    /// against one reflection at cosine 1.0. Returns the first `<cid>`.
    async fn memory_vs_user_chunks_first_hit(memory_tags: &[&str]) -> String {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        for i in 0..7 {
            let chunk = crate::import::ConversationChunk {
                id: format!("user-{i}"),
                conversation_id: format!("conv-{i}"),
                project_name: "test".into(),
                timestamp: "2026-09-20T00:00:00Z".into(),
                content: format!("follower growth recap quoted into session {i}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            let vector = vec![0.63_f32, 0.776_6, 0.0, 0.0];
            storage.insert_chunk(&chunk, &vector).unwrap();
            storage
                .with_connection(|conn| {
                    conn.execute("UPDATE chunks SET min_trust = 3 WHERE id = ?1", [&chunk.id])?;
                    Ok(())
                })
                .unwrap();
            engine.insert_chunk(chunk.id, vector);
        }
        let memory_id = "5eb5aebb-65ab-4fba-9917-86cb83845154";
        let tags: Vec<String> = memory_tags.iter().map(|t| t.to_string()).collect();
        storage
            .insert_reflection(
                memory_id,
                "MEMORY-ARCHIVE follower growth tracking May 2026",
                &tags,
                &query,
            )
            .unwrap();
        engine.insert_reflection(memory_id.to_string(), query.to_vec());
        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "follower growth memory",
            1,
            0.0,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        let start = out.find("<cid>").expect("one result") + "<cid>".len();
        let end = out[start..].find("</cid>").unwrap() + start;
        out[start..end].to_string()
    }

    #[tokio::test]
    async fn exact_explicit_memory_outranks_boosted_user_chunks() {
        assert_eq!(
            memory_vs_user_chunks_first_hit(&["anukriti", "memory-archive"]).await,
            "5eb5aebb-65ab-4fba-9917-86cb83845154"
        );
        assert_eq!(
            memory_vs_user_chunks_first_hit(&["free-form", EXPLICIT_MEMORY_TAG]).await,
            "5eb5aebb-65ab-4fba-9917-86cb83845154"
        );
    }

    #[tokio::test]
    async fn derived_story_keeps_no_standing_against_user_chunks() {
        // Policy boundary: a pipeline story at cosine 1.0 still yields to
        // user-authored history, exactly as before this fix.
        // `<cid>` renders the conversation id: `conv-N` for the user chunks,
        // the reflection's own id for the memory.
        let first = memory_vs_user_chunks_first_hit(&["session_story", "conv_x"]).await;
        assert!(first.starts_with("conv-"), "{first}");
    }

    /// Live 2026-09-22 defect (project EnhanceMe, query "codex blocked
    /// revival..."): a legacy uuid-shaped reflection at raw cosine ~0.12 to
    /// the query was still granted UserHistory standing, so `+0.50` lifted it
    /// (adj 0.616) above the actually relevant chunk (cosine 0.549, no
    /// boost). `EXPLICIT_MEMORY_STANDING_FLOOR` must gate the boost off for a
    /// memory this weakly related, so the relevant chunk wins the page.
    #[tokio::test]
    async fn weak_similarity_memory_does_not_outrank_relevant_chunk() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];

        // Relevant chunk: cosine 0.55 to the query, no standing of its own.
        let relevant_cosine = 0.55_f32;
        let relevant_vector = vec![
            relevant_cosine,
            (1.0 - relevant_cosine * relevant_cosine).sqrt(),
            0.0,
            0.0,
        ];
        let chunk = crate::import::ConversationChunk {
            id: "relevant-chunk".into(),
            conversation_id: "conv-relevant".into(),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: "the codex revival is blocked on the iOS deploy step".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &relevant_vector).unwrap();
        engine.insert_chunk(chunk.id.clone(), relevant_vector);

        // Legacy uuid-shaped memory: cosine 0.12, well under the floor.
        let weak_cosine = 0.12_f32;
        let weak_vector = vec![
            weak_cosine,
            (1.0 - weak_cosine * weak_cosine).sqrt(),
            0.0,
            0.0,
        ];
        let memory_id = "ca488584-0000-4000-8000-000000000001";
        storage
            .insert_reflection(
                memory_id,
                "an unrelated anukriti campaign note",
                &["anukriti".to_string()],
                &weak_vector,
            )
            .unwrap();
        engine.insert_reflection(memory_id.to_string(), weak_vector);

        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "codex blocked revival 2026 iOS deploy blocker where stuck",
            2,
            0.0,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        let start = out.find("<cid>").expect("at least one result") + "<cid>".len();
        let end = out[start..].find("</cid>").unwrap() + start;
        assert_eq!(
            &out[start..end],
            "conv-relevant",
            "a cosine-0.12 memory must not outrank a cosine-0.55 relevant chunk:\n{out}"
        );
    }

    /// Opus review finding (medium, W3-K): `EXPLICIT_MEMORY_STANDING_FLOOR`
    /// gated the boost only on the explicit-memory branch of
    /// `reflection_recall_standing`. A derived reflection (no
    /// `csr:memory` stamp, not uuid-shaped) whose CACHED provenance floor is
    /// already `UserHistory` skipped that gate entirely and still got the
    /// flat `W_USER` boost at any cosine — live receipt: `extracted_v3_*`
    /// rows scored `cos=0.535 user=+0.50 total=1.035`, the same shape as the
    /// `ca488584` bug this item was opened for. Same fixture as
    /// `weak_similarity_memory_does_not_outrank_relevant_chunk`, but the
    /// weak memory is a derived row with a pre-cached `UserHistory` floor
    /// instead of an explicit `store_reflection` write.
    #[tokio::test]
    async fn weak_similarity_cached_user_history_reflection_does_not_outrank_relevant_chunk() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];

        let relevant_cosine = 0.55_f32;
        let relevant_vector = vec![
            relevant_cosine,
            (1.0 - relevant_cosine * relevant_cosine).sqrt(),
            0.0,
            0.0,
        ];
        let chunk = crate::import::ConversationChunk {
            id: "relevant-chunk".into(),
            conversation_id: "conv-relevant".into(),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: "the codex revival is blocked on the iOS deploy step".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &relevant_vector).unwrap();
        engine.insert_chunk(chunk.id.clone(), relevant_vector);

        // Derived reflection: no EXPLICIT_MEMORY_TAG, non-uuid id (so
        // `is_explicit_memory` is false), cosine 0.12 (well under the
        // floor), with a cached provenance floor already UserHistory —
        // the shape `insert_derived_reflection` produces for a row whose
        // support set includes a user message.
        let weak_cosine = 0.12_f32;
        let weak_vector = vec![
            weak_cosine,
            (1.0 - weak_cosine * weak_cosine).sqrt(),
            0.0,
            0.0,
        ];
        let derived_id = "extracted_v3_unrelated-conv";
        storage
            .insert_reflection(
                derived_id,
                "an unrelated extracted narrative",
                &["narrative_extracted_v3".to_string()],
                &weak_vector,
            )
            .unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE reflections SET min_trust = ?1 WHERE id = ?2",
                    rusqlite::params![
                        crate::provenance::TrustTier::UserHistory.as_i64(),
                        derived_id
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        engine.insert_reflection(derived_id.to_string(), weak_vector);

        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "codex blocked revival 2026 iOS deploy blocker where stuck",
            2,
            0.0,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        let start = out.find("<cid>").expect("at least one result") + "<cid>".len();
        let end = out[start..].find("</cid>").unwrap() + start;
        assert_eq!(
            &out[start..end],
            "conv-relevant",
            "a cosine-0.12 derived reflection with a cached UserHistory floor must not outrank \
             a cosine-0.55 relevant chunk:\n{out}"
        );
    }

    #[tokio::test]
    async fn bare_project_tag_keeps_memory_in_family_under_a_scope() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let chunk = crate::import::ConversationChunk {
            id: "anchor".into(),
            conversation_id: "conv-anchor".into(),
            project_name: "meta-campaigns".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: "campaign roster baseline".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.6, 0.8, 0.0, 0.0]).unwrap();
        engine.insert_chunk(chunk.id.clone(), vec![0.6, 0.8, 0.0, 0.0]);
        storage
            .insert_reflection(
                "memory-in-family",
                "MEMORY-ARCHIVE campaign roster",
                &["meta-campaigns".into(), EXPLICIT_MEMORY_TAG.into()],
                &query,
            )
            .unwrap();
        engine.insert_reflection("memory-in-family".into(), query.to_vec());
        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "campaign roster memory",
            2,
            0.0,
            Some("meta-campaigns"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        let start = out
            .find("<cid>memory-in-family</cid>")
            .expect("memory on the page");
        let block_start = out[..start].rfind("<r rank=").expect("result block");
        let block = &out[block_start..start];
        let score: f32 = block
            .split("<s")
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split("</s").next())
            .and_then(|s| s.trim().parse().ok())
            .expect("score");
        assert!(
            block.contains("<p>meta-campaigns</p>"),
            "bare project tag must resolve to the scoped project:\n{out}"
        );
        assert!(
            score > 0.9,
            "no cross-project penalty expected, got {score}:\n{out}"
        );
    }

    /// Spec (b): under an explicit project filter, reflections and stories
    /// outside the family — including ones that resolve to "unknown" because
    /// no tag names a project — are NOT dropped (6cf8a2b: a hard drop lost a
    /// real sibling-project answer); they must rank after every in-family row
    /// (`sink_out_of_family_under_explicit_scope` + the 0.3x cross-project
    /// multiplier). Live 2026-09-22 defect (project EnhanceMe): an
    /// unknown-project memory and an anukriti story both out-ranked in-family
    /// results. A bare project tag that matches the scope still keeps the row
    /// in the family.
    #[tokio::test]
    async fn explicit_project_scope_sinks_out_of_family_and_unknown_reflections() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];

        // Out-of-family story: tagged to a different project, cosine 1.0.
        storage
            .insert_reflection(
                "story-out-of-family",
                "MEMORY-ARCHIVE anukriti campaign story",
                &["session_story".into(), "project_anukriti".into()],
                &query,
            )
            .unwrap();
        engine.insert_reflection("story-out-of-family".into(), query.to_vec());

        // Unknown-project explicit memory: no project tag at all, cosine 1.0.
        storage
            .insert_reflection(
                "memory-unknown-project",
                "MEMORY-ARCHIVE some note with no project tag",
                &["free-form".into(), EXPLICIT_MEMORY_TAG.into()],
                &query,
            )
            .unwrap();
        engine.insert_reflection("memory-unknown-project".into(), query.to_vec());

        // In-family explicit memory via a bare project tag, cosine 1.0.
        storage
            .insert_reflection(
                "memory-in-family",
                "MEMORY-ARCHIVE EnhanceMe roster note",
                &["EnhanceMe".into(), EXPLICIT_MEMORY_TAG.into()],
                &query,
            )
            .unwrap();
        engine.insert_reflection("memory-in-family".into(), query.to_vec());

        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "roster memory",
            10,
            0.0,
            Some("EnhanceMe"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();

        assert!(
            out.contains("<cid>memory-in-family</cid>"),
            "an in-family bare-tag memory must stay on the page:\n{out}"
        );
        let pos = |cid: &str| {
            out.find(&format!("<cid>{cid}</cid>"))
                .unwrap_or_else(|| panic!("{cid} must be present (sunk, not dropped):\n{out}"))
        };
        let in_family = pos("memory-in-family");
        assert!(
            pos("story-out-of-family") > in_family,
            "an out-of-family story must rank after every in-family row:\n{out}"
        );
        assert!(
            pos("memory-unknown-project") > in_family,
            "an unknown-project memory must rank after every in-family row:\n{out}"
        );
    }

    /// Opus review finding 1 (W3-K, high): a memory-file reflection carries a
    /// `project_*` tag that does NOT itself resolve to the scope's family
    /// (`project_meta_campaigns_setup` vs scope `anukriti-meta-campaigns`)
    /// alongside the bare family tag every `MEMORY-ARCHIVE` writer also
    /// stamps (`anukriti-meta-campaigns`). `reflection_in_family` must
    /// accept the row on ANY tag clearing `scope.is_family`, not just the
    /// first `project_*` tag found — before this fix it read only that first
    /// tag, resolved to `meta_campaigns_setup`, failed `is_family`, and
    /// dropped the row outright even though it plainly belongs to the
    /// family. Live receipt: `e97fa3b4`/`5eb5aebb`/`22ffc13b` all carry this
    /// exact tag shape and vanished from `project=anukriti-meta-campaigns`
    /// scoped search.
    #[tokio::test]
    async fn family_bare_tag_keeps_memory_file_reflection_in_scope() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(4);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let id = "e97fa3b4-0000-4000-8000-000000000000";
        let tags = vec![
            "project_meta_campaigns_setup".to_string(),
            "anukriti-meta-campaigns".to_string(),
            EXPLICIT_MEMORY_TAG.to_string(),
        ];
        storage
            .insert_reflection(
                id,
                "MEMORY-ARCHIVE anukriti-meta-campaigns paying customers may22",
                &tags,
                &query,
            )
            .unwrap();
        engine.insert_reflection(id.to_string(), query.to_vec());
        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "paying customers memory",
            5,
            0.0,
            Some("anukriti-meta-campaigns"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        assert!(
            out.contains(&format!("<cid>{id}</cid>")),
            "a memory-file reflection with a family bare tag must stay in scope even when its \
             project_* tag alone would not resolve to the family:\n{out}"
        );
    }

    #[tokio::test]
    async fn keyword_channel_reaches_reflections_in_hybrid_mode() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let chunk = crate::import::ConversationChunk {
            id: "semantic-only".into(),
            conversation_id: "conv-s".into(),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: "unrelated words entirely".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &query).unwrap();
        engine.insert_chunk(chunk.id.clone(), query.to_vec());
        // Memory whose vector is orthogonal to the query: only the keyword
        // channel can reach it.
        let memory_id = "memory-keyword-only";
        let orthogonal = vec![0.0_f32, 0.0, 0.0, 1.0];
        storage
            .insert_reflection(
                memory_id,
                "MEMORY-ARCHIVE follower growth archived",
                &[EXPLICIT_MEMORY_TAG.to_string()],
                &orthogonal,
            )
            .unwrap();
        engine.insert_reflection(memory_id.to_string(), orthogonal);
        let search = Arc::new(RwLock::new(engine));
        let run = |mode: SearchMode| {
            let storage = Arc::clone(&storage);
            let search = Arc::clone(&search);
            async move {
                reflect_on_past_with_vec_mode(
                    &storage,
                    &search,
                    &query,
                    "MEMORY-ARCHIVE follower growth",
                    5,
                    0.1,
                    Some("all"),
                    0,
                    false,
                    ConsumptionMode::Off,
                    false,
                    "other",
                    mode,
                    RecallRerankMode::Baseline,
                    false,
                )
                .await
                .unwrap()
            }
        };
        let hybrid = run(SearchMode::Hybrid).await;
        assert!(
            hybrid.contains("<cid>memory-keyword-only</cid>"),
            "{hybrid}"
        );
        assert!(
            hybrid.contains("[keyword] [reflection] MEMORY-ARCHIVE"),
            "keyword-only reflections must carry the keyword marker:\n{hybrid}"
        );
        let vector = run(SearchMode::Vector).await;
        assert!(
            !vector.contains("<cid>memory-keyword-only</cid>"),
            "{vector}"
        );
    }

    /// Spec (e): an exact-prefix reflection must reach page 1 of the keyword
    /// channel even when a common term in the query also appears in many
    /// chunks. Live 2026-09-22: `reflections_fts` itself ranked 3
    /// `MEMORY-ARCHIVE anukriti-meta-campaigns` memories at bm25 8/9/10 of
    /// 267 matches (the OR-of-terms query rewards any row mentioning
    /// "anukriti"), and the combined chunk+reflection bm25 merge buried them
    /// under chunk hits entirely — none of the 3 made the final page.
    #[tokio::test]
    async fn exact_prefix_reflection_reaches_page_one_over_many_keyword_matching_chunks() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(256);
        let query_vec = [1.0_f32, 0.0, 0.0, 0.0];
        // Chunks sit at cosine 0.4 — weakly relevant, but above min_score, so
        // they count on both the semantic and keyword channels (the real
        // shape: generic background chatter that happens to mention the
        // query terms). The memories are cosine 0.0 (excluded from the
        // semantic channel entirely by `min_score`) — only the keyword
        // channel, via the exact-prefix match, can reach them.
        let chunk_vector = vec![0.4_f32, (1.0_f32 - 0.4 * 0.4).sqrt(), 0.0, 0.0];
        let orthogonal = vec![0.0_f32, 1.0, 0.0, 0.0];
        let min_score = 0.3_f32;

        // 200 chunks that all match the OR-of-terms query (mention "x-y-z").
        for i in 0..200 {
            let chunk = crate::import::ConversationChunk {
                id: format!("chunk-{i}"),
                conversation_id: format!("conv-chunk-{i}"),
                project_name: "test".into(),
                timestamp: "2026-09-20T00:00:00Z".into(),
                content: format!("background note about project-x-y-z number {i}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::Assistant,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &chunk_vector).unwrap();
            engine.insert_chunk(chunk.id, chunk_vector.clone());
        }

        // 3 memories whose content starts with the query text verbatim.
        let mut memory_ids = Vec::new();
        for i in 0..3 {
            let id = format!("memory-prefix-{i}");
            storage
                .insert_reflection(
                    &id,
                    &format!("MEMORY-ARCHIVE project-x-y-z memory number {i}"),
                    &[EXPLICIT_MEMORY_TAG.to_string()],
                    &orthogonal,
                )
                .unwrap();
            engine.insert_reflection(id.clone(), orthogonal.clone());
            memory_ids.push(id);
        }

        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query_vec,
            "MEMORY-ARCHIVE project-x-y-z",
            5,
            min_score,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();

        for id in &memory_ids {
            assert!(
                out.contains(&format!("<cid>{id}</cid>")),
                "exact-prefix memory {id} must reach page 1 over 200 OR-matching chunks:\n{out}"
            );
        }
    }

    #[tokio::test]
    async fn curated_candidate_mode_exercises_real_reflect_pipeline_without_telemetry_writes() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for (sequence, (id, conversation_id, timestamp, vector)) in [
            (
                "older".to_string(),
                "conv-older".to_string(),
                "not-a-timestamp".to_string(),
                vec![1.0, 0.0, 0.0, 0.0],
            ),
            (
                "newer".to_string(),
                "conv-newer".to_string(),
                chrono::Utc::now().to_rfc3339(),
                vec![0.99, 0.01, 0.0, 0.0],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let chunk = crate::import::ConversationChunk {
                id: id.clone(),
                conversation_id,
                project_name: "test".into(),
                timestamp,
                content: format!("curated pipeline candidate {id}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::Assistant,
                seq: sequence,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(chunk.id, vector);
        }
        let search = Arc::new(RwLock::new(engine));
        let mut weights = vec![0.0; crate::search::trained_rerank::FEATURE_COUNT];
        weights[2] = 100.0;
        let model = crate::search::trained_rerank::LinearModel {
            weights,
            bias: -50.0,
            normalization: crate::search::trained_rerank::Normalization {
                means: vec![0.0; crate::search::trained_rerank::FEATURE_COUNT],
                scales: vec![1.0; crate::search::trained_rerank::FEATURE_COUNT],
            },
            seed: 7,
        };
        let query = [1.0, 0.0, 0.0, 0.0];

        let baseline = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "curated pipeline",
            2,
            0.0,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        let trained = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "curated pipeline",
            2,
            0.0,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "explore",
            SearchMode::Hybrid,
            RecallRerankMode::Candidate(&model),
            false,
        )
        .await
        .unwrap();

        assert!(baseline.find("<id>older</id>") < baseline.find("<id>newer</id>"));
        assert!(trained.find("<id>newer</id>") < trained.find("<id>older</id>"));
        assert!(storage
            .get_retrieval_events_for_memory("older")
            .unwrap()
            .is_empty());
        assert!(storage
            .get_retrieval_events_for_memory("newer")
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn quick_check_reports_margin_from_second_candidate_but_renders_only_top() {
        let (storage, search) = quick_check_fixture(&[
            ("top", "project-a", "top candidate", [1.0, 0.0, 0.0, 0.0]),
            (
                "runner-up",
                "project-b",
                "runner-up candidate",
                [0.8, 0.6, 0.0, 0.0],
            ),
        ]);

        let xml = quick_check_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "probe",
            0.3,
            Some("all"),
        )
        .await
        .unwrap();

        assert!(xml.contains("<margin>0.200</margin>"), "got: {xml}");
        assert!(xml.contains("<count>1</count>"), "got: {xml}");
        assert!(xml.contains("top candidate"), "got: {xml}");
        assert!(!xml.contains("runner-up candidate"), "got: {xml}");
    }

    #[tokio::test]
    async fn quick_check_reports_na_margin_when_corpus_has_one_candidate() {
        let (storage, search) =
            quick_check_fixture(&[("only", "project-a", "only candidate", [1.0, 0.0, 0.0, 0.0])]);

        let xml = quick_check_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "probe",
            0.3,
            Some("all"),
        )
        .await
        .unwrap();

        assert!(xml.contains("<margin>n/a</margin>"), "got: {xml}");
    }

    #[tokio::test]
    async fn quick_check_project_scope_excludes_out_of_family_candidate() {
        let (storage, search) = quick_check_fixture(&[
            (
                "out-of-family",
                "other-project",
                "out-of-family candidate",
                [1.0, 0.0, 0.0, 0.0],
            ),
            (
                "local-top",
                "scope-project",
                "scoped top candidate",
                [0.8, 0.6, 0.0, 0.0],
            ),
            (
                "local-second",
                "scope-project",
                "scoped runner-up candidate",
                [0.7, 0.714_142_86, 0.0, 0.0],
            ),
        ]);

        let xml = quick_check_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "probe",
            0.3,
            Some("scope-project"),
        )
        .await
        .unwrap();

        assert!(xml.contains("scoped top candidate"), "got: {xml}");
        assert!(xml.contains("<margin>0.100</margin>"), "got: {xml}");
        assert!(!xml.contains("out-of-family candidate"), "got: {xml}");
    }

    #[tokio::test]
    async fn all_scope_cross_project_alias_boost_changes_ranking_after_rerank() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        let current = "scope-family-root";
        let alias = "scope-family-root-csr-engine";
        let cwd = projects.join(current).join("csr-engine");
        std::fs::create_dir_all(&cwd).unwrap();
        let resolved_current = cross_project::resolve_project_from_cwd(cwd.to_str().unwrap());

        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let rows = [
            ("alias-result", alias, vec![0.90, 0.435_889_9, 0.0, 0.0]),
            (
                "other-result",
                "unrelated-project",
                vec![0.95, 0.312_249_9, 0.0, 0.0],
            ),
        ];
        for (sequence, (id, project, vector)) in rows.into_iter().enumerate() {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: format!("conv-{id}"),
                project_name: project.into(),
                timestamp: "2099-01-01T00:00:00Z".into(),
                content: format!("organic scope ranking claim {id}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: sequence,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(chunk.id, vector);
        }
        let search = Arc::new(RwLock::new(engine));
        let without_current =
            SearchProjectScope::resolve_with(&storage, Some("all"), None, None, Some(&projects))
                .unwrap();
        let with_current = SearchProjectScope::resolve_with(
            &storage,
            Some("all"),
            resolved_current.as_deref(),
            None,
            Some(&projects),
        )
        .unwrap();

        assert_eq!(with_current.effective_project, None);
        assert_eq!(
            with_current.current_project_for_all_scope.as_deref(),
            Some(current)
        );
        assert!(with_current.family_projects.contains(alias));
        assert_eq!(
            project_scope_multiplier(alias, &with_current),
            CURRENT_PROJECT_ALL_SCOPE_BOOST
        );

        let query = [1.0, 0.0, 0.0, 0.0];
        let unboosted = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &query,
            "scope ranking",
            2,
            0.1,
            &without_current,
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let boosted = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &query,
            "scope ranking",
            2,
            0.1,
            &with_current,
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();

        assert!(
            unboosted.find("other-result").unwrap() < unboosted.find("alias-result").unwrap(),
            "unboosted all-scope order:\n{unboosted}"
        );
        assert!(
            boosted.find("alias-result").unwrap() < boosted.find("other-result").unwrap(),
            "boosted all-scope order:\n{boosted}"
        );
    }

    // W3-S: default scope (project omitted) falls back to `CLAUDE_PROJECT_DIR`
    // via `default_scope_project`, not just `MCP_CLIENT_CWD` (`current_project`).
    // These exercise `SearchProjectScope::resolve_with` directly with injected
    // values — no env var manipulation, so no race with other lib tests.

    #[test]
    fn default_scope_project_resolves_to_the_client_dirs_family_when_project_omitted() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let scope = SearchProjectScope::resolve_with(&storage, None, None, Some("EnhanceMe"), None)
            .unwrap();
        assert_eq!(scope.effective_project.as_deref(), Some("EnhanceMe"));
        assert_eq!(scope.scope_label, "EnhanceMe");
        assert!(scope.family_projects.contains("EnhanceMe"));
    }

    #[test]
    fn explicit_all_scope_ignores_default_scope_project() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), None, Some("EnhanceMe"), None)
                .unwrap();
        assert_eq!(scope.effective_project, None);
        assert_eq!(scope.scope_label, "all");
    }

    /// W3-S finding 3 receipt: putting the `CLAUDE_PROJECT_DIR` fallback in
    /// `default_scope_project` (used only when `project` is omitted) rather
    /// than in `current_project` (used for the `project="all"` same-family
    /// boost) means an explicit `project="all"` call resolves to the exact
    /// same candidate set with or without a client directory present.
    /// Threading the fallback through `current_project` instead would make
    /// this diverge: `with_client_dir.family_projects` would gain the
    /// `EnhanceMe` family and its ranking boost would newly apply, changing
    /// output for every Claude Code session that ever calls `project="all"`.
    #[test]
    fn explicit_all_scope_is_identical_with_and_without_a_client_dir() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let projects_root = tempfile::tempdir().unwrap();
        let without_client_dir = SearchProjectScope::resolve_with(
            &storage,
            Some("all"),
            None,
            None,
            Some(projects_root.path()),
        )
        .unwrap();
        let with_client_dir = SearchProjectScope::resolve_with(
            &storage,
            Some("all"),
            None,
            Some("EnhanceMe"),
            Some(projects_root.path()),
        )
        .unwrap();
        assert_eq!(
            without_client_dir.effective_project,
            with_client_dir.effective_project
        );
        assert_eq!(without_client_dir.scope_label, with_client_dir.scope_label);
        assert_eq!(
            without_client_dir.current_project_for_all_scope,
            with_client_dir.current_project_for_all_scope
        );
        assert_eq!(
            without_client_dir.family_projects,
            with_client_dir.family_projects
        );
        assert!(
            !with_client_dir.family_projects.contains("EnhanceMe"),
            "the default-scope-only fallback must not leak into project=\"all\": {:?}",
            with_client_dir.family_projects
        );
    }

    #[test]
    fn no_default_scope_project_and_no_explicit_project_is_all() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let scope = SearchProjectScope::resolve_with(&storage, None, None, None, None).unwrap();
        assert_eq!(scope.effective_project, None);
        assert_eq!(scope.scope_label, "all");
    }

    /// W3-S finding 1 receipt: `CLAUDE_PROJECT_DIR=/private/tmp/csr-s1-proof`
    /// (no `projects` path component) is imported by Claude Code as
    /// `-private-tmp-csr-s1-proof` — its own dash-encoding of the full path —
    /// not the last-path-component heuristic `resolve_project_from_cwd`
    /// alone would give (`csr-s1-proof`). `resolve_search_scope_with` is the
    /// single function both `reflect_on_past_with_vec_mode` and
    /// `get_more_results_with_vec_active` call to resolve an omitted
    /// project; this pins that it resolves to the *stored* name, not the
    /// unverified heuristic one.
    #[test]
    fn resolve_search_scope_matches_the_importers_encoding_outside_projects_root() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let stored_project = "-private-tmp-csr-s1-proof";
        let chunk = crate::import::ConversationChunk {
            id: "seed".into(),
            conversation_id: "conv-seed".into(),
            project_name: stored_project.into(),
            timestamp: "2026-09-18T00:00:00Z".into(),
            content: "seed row so list_project_names knows this project".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage
            .insert_chunk(&chunk, &[1.0f32, 0.0, 0.0, 0.0])
            .unwrap();

        let scope = resolve_search_scope_with(
            &storage,
            None,
            None,
            Some("/private/tmp/csr-s1-proof"),
            None,
        )
        .unwrap();
        assert_eq!(scope.effective_project.as_deref(), Some(stored_project));
        assert_eq!(scope.scope_label, stored_project);
    }

    /// Same receipt end to end through `reflect_on_past_with_vec_in_scope`:
    /// a chunk stored under the importer-encoded project name is returned
    /// when `project` is omitted and the client dir sits outside
    /// `~/projects`. Before W3-S finding 1's fix, `default_scope_project`
    /// resolved to `csr-s1-proof` (matches nothing) and this search returned
    /// zero results.
    #[tokio::test]
    async fn defaulted_scope_outside_projects_root_returns_the_stored_project_chunk() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let stored_project = "-private-tmp-csr-s1-proof";
        let chunk = crate::import::ConversationChunk {
            id: "chunk-outside-projects-root".into(),
            conversation_id: "conv-1".into(),
            project_name: stored_project.into(),
            timestamp: "2026-09-18T00:00:00Z".into(),
            content: "rename the function foo to bar in a Python file".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let vector = vec![1.0f32, 0.0, 0.0, 0.0];
        storage.insert_chunk(&chunk, &vector).unwrap();
        let mut engine = SearchEngine::new(vector.len());
        engine.insert_chunk(chunk.id.clone(), vector.clone());
        let search = Arc::new(RwLock::new(engine));

        let scope = resolve_search_scope_with(
            &storage,
            None,
            None,
            Some("/private/tmp/csr-s1-proof"),
            None,
        )
        .unwrap();

        let output = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &vector,
            "rename the function foo to bar in a Python file",
            8,
            0.1,
            &scope,
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(
            output.contains("chunk-outside-projects-root"),
            "expected the stored-project chunk in defaulted-scope output:\n{output}"
        );
    }

    #[tokio::test]
    async fn defaulted_scope_excludes_a_higher_ranked_out_of_family_chunk() {
        // The spec's receipt: with `project` omitted and `CLAUDE_PROJECT_DIR`
        // resolving to `EnhanceMe`, an EnhanceMe-family chunk is returned and
        // a higher-scoring out-of-family chunk is not — even though an
        // unscoped ("all") search would put the out-of-family chunk first.
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mk = |id: &str, project: &str, content: &str| crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: project.into(),
            timestamp: "2099-01-01T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let rows = [
            (
                mk(
                    "chunk-in-family",
                    "EnhanceMe",
                    "codex blocked revival deploy blocker",
                ),
                vec![0.80f32, 0.6, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-out-of-family",
                    "some-other-project",
                    "codex blocked revival deploy blocker, ranked higher",
                ),
                vec![1.0f32, 0.0, 0.0, 0.0],
            ),
        ];
        let mut engine = SearchEngine::new(8);
        for (chunk, vector) in &rows {
            storage.insert_chunk(chunk, vector).unwrap();
            engine.insert_chunk(chunk.id.clone(), vector.clone());
        }
        let search = Arc::new(RwLock::new(engine));
        // Explicit tempdir root so family resolution never touches this
        // machine's real `$HOME/projects` layout.
        let projects_root = tempfile::tempdir().unwrap();
        let scope = SearchProjectScope::resolve_with(
            &storage,
            None,
            None,
            Some("EnhanceMe"),
            Some(projects_root.path()),
        )
        .unwrap();
        let query = [1.0f32, 0.0, 0.0, 0.0];

        let output = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &query,
            "codex blocked revival deploy blocker",
            8,
            0.1,
            &scope,
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();

        assert!(
            output.contains("chunk-in-family") || output.contains("EnhanceMe"),
            "expected the in-family chunk in defaulted-scope output:\n{output}"
        );
        assert!(
            !output.contains("chunk-out-of-family") && !output.contains("ranked higher"),
            "out-of-family chunk leaked into defaulted-scope output:\n{output}"
        );
    }

    #[test]
    fn resolve_node_id_preserves_suffix_matching_and_excludes_module_nodes() {
        use crate::storage::codegraph::NodeRow;

        let storage = Arc::new(Storage::open_memory().unwrap());
        let project = "claude-self-reflect";
        storage
            .upsert_code_node(&NodeRow {
                id: "module-node".into(),
                project: project.into(),
                file: "/repo/src/search/rerank.rs".into(),
                kind: "module".into(),
                name: "rerank".into(),
                ..NodeRow::default()
            })
            .unwrap();
        storage
            .upsert_code_node(&NodeRow {
                id: "function-node".into(),
                project: project.into(),
                file: "/repo/src/search/rerank.rs".into(),
                kind: "function".into(),
                name: "is_scaffold_text".into(),
                ..NodeRow::default()
            })
            .unwrap();

        let family = vec![project.to_string()];
        let resolved = resolve_node_id(
            &storage,
            None,
            Some("src/search/rerank.rs"),
            project,
            &family,
        )
        .unwrap();
        assert_eq!(resolved.as_deref(), Some("function-node"));
    }

    #[test]
    fn resolve_node_id_prefers_exact_project_over_higher_ranked_family_alias() {
        use crate::storage::codegraph::NodeRow;

        let storage = Arc::new(Storage::open_memory().unwrap());
        let current = "fixture-root";
        let alias = "fixture-root-csr-engine";
        for (id, project) in [("exact-def", current), ("alias-def", alias)] {
            storage
                .upsert_code_node(&NodeRow {
                    id: id.into(),
                    project: project.into(),
                    file: "src/provenance.rs".into(),
                    kind: "function".into(),
                    name: "is_csr_emission".into(),
                    ..NodeRow::default()
                })
                .unwrap();
        }
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO code_node_rank (node_id, rank, in_degree, out_degree)
                     VALUES ('exact-def', 1.0, 0, 0), ('alias-def', 10.0, 0, 0)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let family = vec![current.to_string(), alias.to_string()];
        let resolved =
            resolve_node_id(&storage, Some("is_csr_emission"), None, current, &family).unwrap();
        assert_eq!(resolved.as_deref(), Some("exact-def"));
    }

    #[test]
    fn codegraph_callers_cross_explicit_project_family_namespaces() {
        use crate::storage::codegraph::{EdgeRow, NodeRow};

        let current = "fixture-root";
        let alias = "fixture-root-csr-engine";
        let family = vec![current.to_string(), alias.to_string()];
        let storage = Arc::new(Storage::open_memory().unwrap());

        for (id, project, file) in [
            ("target-current", current, "src/provenance.rs"),
            ("target-alias", alias, "src/provenance.rs"),
        ] {
            storage
                .upsert_code_node(&NodeRow {
                    id: id.into(),
                    project: project.into(),
                    file: file.into(),
                    kind: "function".into(),
                    name: "is_csr_emission".into(),
                    ..NodeRow::default()
                })
                .unwrap();
        }
        for (id, name, project, file, target) in [
            (
                "caller-current",
                "base_caller",
                current,
                "src/hooks/stop.rs",
                "target-current",
            ),
            (
                "caller-alias",
                "is_scaffold_text",
                alias,
                "src/search/rerank.rs",
                "target-alias",
            ),
        ] {
            storage
                .upsert_code_node(&NodeRow {
                    id: id.into(),
                    project: project.into(),
                    file: file.into(),
                    kind: "function".into(),
                    name: name.into(),
                    ..NodeRow::default()
                })
                .unwrap();
            storage
                .replace_code_file_edges(
                    project,
                    file,
                    &[EdgeRow {
                        src_id: id.into(),
                        dst_id: target.into(),
                        kind: "calls".into(),
                        src_file: file.into(),
                        resolved: 1,
                        weight: 1.0,
                        ..EdgeRow::default()
                    }],
                )
                .unwrap();
        }

        let rendered = code_graph_for_projects(
            &storage,
            Some("is_csr_emission"),
            None,
            "callers",
            20,
            current,
            &family,
        )
        .unwrap();
        assert!(rendered.contains("base_caller"), "{rendered}");
        assert!(rendered.contains("is_scaffold_text"), "{rendered}");
    }

    fn enriched_result(id: &str, score: f32, seq: usize) -> EnrichedResult {
        EnrichedResult {
            score,
            chunk: crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: "conv-1".into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T10:00:00Z".into(),
                content: format!("{id} claim"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::ToolResult,
                seq,
                is_sidechain: false,
            },
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        }
    }

    #[test]
    fn resolved_results_sink_before_limit_cut() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .insert_resolutions(
                &["resolved".to_string()],
                "resolved",
                "shipped and verified",
                None,
                "user_confirmed",
            )
            .unwrap();
        let mut enriched = vec![
            enriched_result("resolved", 0.9, 0),
            enriched_result("unresolved-1", 0.8, 1),
            enriched_result("unresolved-2", 0.7, 2),
        ];

        apply_resolutions_before_limit(&mut enriched, &storage, &HashMap::new(), 2, false);

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["unresolved-1", "unresolved-2"]);
    }

    #[test]
    fn trust_labels_read_cached_columns_only_for_chunks_and_reflections() {
        use crate::provenance::TrustTier;
        use crate::storage::artifact_provenance::{test_observed_input, InputEnvelope};
        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count,min_trust)
                     VALUES('chunk-user','conv','p','now','text',1,3)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        storage
            .insert_derived_reflection(
                "refl-tool",
                "derived",
                &[],
                &[0.0; 4],
                &InputEnvelope::new(vec![test_observed_input(
                    "tool_result:WebFetch",
                    TrustTier::External,
                    "source",
                )]),
            )
            .unwrap();
        // Hot path contract: the event and derivation tables are not consulted.
        storage
            .with_connection(|c| {
                c.execute_batch(
                    "PRAGMA foreign_keys=OFF;
                     DROP TABLE artifact_derivations; DROP TABLE chunk_spans;
                     DROP TABLE provenance_events;",
                )?;
                Ok(())
            })
            .unwrap();
        let mut enriched = vec![
            enriched_result("chunk-user", 0.9, 0),
            enriched_result("refl-tool", 0.8, 1),
            enriched_result("nowhere", 0.7, 2),
        ];
        apply_trust_labels(&mut enriched, &storage);
        let tiers: Vec<TrustTier> = enriched.iter().map(|e| e.trust).collect();
        assert_eq!(
            tiers,
            [
                TrustTier::UserHistory,
                TrustTier::External,
                TrustTier::Unknown
            ]
        );
    }

    #[test]
    fn format_why_renders_the_cached_floor_per_item() {
        use crate::search::reinstatement::{EvidenceItem, ReinstateTrace, Via};
        use std::collections::{BTreeMap, BTreeSet};
        let item = EvidenceItem {
            chunk_id: "chunk".into(),
            conversation_id: "conv".into(),
            score: 0.8,
            best_route: Via::Seed,
            routes: BTreeSet::from([Via::Seed]),
            route_scores: BTreeMap::from([(Via::Seed, 0.8)]),
            timestamp: "2026-08-01T00:00:00Z".into(),
            excerpt: "evidence".into(),
            ratification: None,
            trust: crate::provenance::TrustTier::External,
        };
        let rendered = format_why("q", &[item], &HashSet::new(), &ReinstateTrace::default());
        assert!(rendered.contains(" trust=external "), "{rendered}");
    }

    #[test]
    fn agent_resolution_neither_annotates_nor_sinks_but_user_confirmed_does() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .insert_resolutions(
                &["agent".to_string()],
                "resolved",
                "agent assertion",
                None,
                "agent",
            )
            .unwrap();
        storage
            .insert_resolutions(
                &["confirmed".to_string()],
                "resolved",
                "user accepted",
                None,
                "user_confirmed",
            )
            .unwrap();
        let mut enriched = vec![
            enriched_result("agent", 0.9, 0),
            enriched_result("confirmed", 0.8, 1),
            enriched_result("plain", 0.7, 2),
        ];

        apply_resolutions(&mut enriched, &storage);

        let ids: Vec<&str> = enriched.iter().map(|item| item.chunk.id.as_str()).collect();
        assert_eq!(ids, ["agent", "plain", "confirmed"]);
        assert!(enriched[0].resolution.is_none());
        assert!(enriched[2]
            .resolution
            .as_deref()
            .is_some_and(|note| note.contains("verified")));
    }

    // --- v10 dream-verdict validity partition ---

    fn enriched_result_conv(id: &str, conv: &str, score: f32, seq: usize) -> EnrichedResult {
        EnrichedResult {
            score,
            chunk: crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T10:00:00Z".into(),
                content: format!("{id} claim"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::ToolResult,
                seq,
                is_sidechain: false,
            },
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        }
    }

    #[tokio::test]
    async fn storage_backed_search_prefers_parent_but_keeps_unmatched_sidechain() {
        fn fixture(include_parent: bool) -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
            let storage = Arc::new(Storage::open_memory().unwrap());
            let mut search = SearchEngine::new(8);
            let make_chunk = |id: &str, conversation_id: &str, is_sidechain: bool| {
                crate::import::ConversationChunk {
                    id: id.into(),
                    conversation_id: conversation_id.into(),
                    project_name: "test".into(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    content: format!("shared parent child recall {id}"),
                    message_count: 1,
                    summary: None,
                    author: crate::provenance::Speaker::Assistant,
                    seq: 0,
                    is_sidechain,
                }
            };

            if include_parent {
                let parent = make_chunk("parent-chunk", "parent-conversation", false);
                storage
                    .insert_chunk(&parent, &[1.0, 0.0, 0.0, 0.0])
                    .unwrap();
                search.insert_chunk(parent.id, vec![1.0, 0.0, 0.0, 0.0]);
            }
            let child = make_chunk("child-chunk", "agent-child", true);
            storage
                .insert_chunk(&child, &[0.99, 0.01, 0.0, 0.0])
                .unwrap();
            storage
                .insert_chunk_provenance(
                    &child.id,
                    &crate::provenance::ChunkProvenance {
                        author: crate::provenance::Speaker::Assistant,
                        source_conv_id: "parent-conversation".into(),
                        supersedes: None,
                    },
                )
                .unwrap();
            search.insert_chunk(child.id, vec![0.99, 0.01, 0.0, 0.0]);
            (storage, Arc::new(RwLock::new(search)))
        }

        let query = [1.0, 0.0, 0.0, 0.0];
        let (storage, search) = fixture(true);
        let with_parent = reflect_on_past_with_vec(
            &storage,
            &search,
            &query,
            "recall",
            2,
            0.1,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(with_parent.contains("<id>parent-chunk</id>"));
        assert!(!with_parent.contains("<id>child-chunk</id>"));

        let (storage, search) = fixture(false);
        let without_parent = reflect_on_past_with_vec(
            &storage,
            &search,
            &query,
            "recall",
            2,
            0.1,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(without_parent.contains("<id>child-chunk</id>"));
    }

    #[tokio::test]
    async fn full_fts_window_refills_slots_opened_by_sidechain_dedupe() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let make_chunk = |id: &str, conversation_id: &str, is_sidechain: bool, content: &str| {
            crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation_id.into(),
                project_name: "test".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::Assistant,
                seq: 0,
                is_sidechain,
            }
        };
        let parent = make_chunk(
            "fts-parent",
            "fts-parent-conversation",
            false,
            "refilltoken refilltoken refilltoken",
        );
        storage.insert_chunk(&parent, &[0.0; 4]).unwrap();
        for index in 0..5 {
            let child = make_chunk(
                &format!("fts-child-{index}"),
                &format!("agent-fts-child-{index}"),
                true,
                "refilltoken refilltoken refilltoken",
            );
            storage.insert_chunk(&child, &[0.0; 4]).unwrap();
            storage
                .insert_chunk_provenance(
                    &child.id,
                    &crate::provenance::ChunkProvenance {
                        author: crate::provenance::Speaker::Assistant,
                        source_conv_id: "fts-parent-conversation".into(),
                        supersedes: None,
                    },
                )
                .unwrap();
        }
        for index in 0..2 {
            let extra = make_chunk(
                &format!("fts-extra-{index}"),
                &format!("fts-extra-conversation-{index}"),
                false,
                "refilltoken with lower density filler words for an additional result",
            );
            storage.insert_chunk(&extra, &[0.0; 4]).unwrap();
        }

        let search = Arc::new(RwLock::new(SearchEngine::new(8)));
        let output = reflect_on_past_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "refilltoken",
            2,
            0.1,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();

        assert!(output.contains("<count>2</count>"), "{output}");
        assert!(output.contains("<id>fts-parent</id>"), "{output}");
        assert!(output.contains("<id>fts-extra-"), "{output}");
        assert!(!output.contains("<id>fts-child-"), "{output}");
    }

    #[tokio::test]
    async fn bench_refetches_past_a_dominant_conversations_initial_chunk_window() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        for index in 0..20 {
            let chunk = crate::import::ConversationChunk {
                id: format!("a-dominant-{index:02}"),
                conversation_id: "dominant-conversation".into(),
                project_name: "csr-bench".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
                content: "windowtoken".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::Assistant,
                seq: index,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
        }
        let gold = crate::import::ConversationChunk {
            id: "z-gold".into(),
            conversation_id: "gold-conversation".into(),
            project_name: "csr-bench".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: "windowtoken".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&gold, &[0.0; 4]).unwrap();
        let search = Arc::new(RwLock::new(SearchEngine::new(32)));

        let hits = reflect_for_bench_with_vec(
            &storage,
            &search,
            &[0.0; 4],
            "windowtoken",
            5,
            SearchMode::Fts,
        )
        .await
        .unwrap();
        assert!(
            hits.iter()
                .take(5)
                .any(|hit| hit.conversation_id == "gold-conversation"),
            "adaptive refetch must find a distinct conversation below the initial 20 chunks: {hits:?}"
        );
    }

    #[tokio::test]
    async fn hybrid_surfaces_exact_identifier_below_semantic_threshold() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let now = chrono::Utc::now().to_rfc3339();
        let semantic = crate::import::ConversationChunk {
            id: "semantic-high".into(),
            conversation_id: "semantic-conversation".into(),
            project_name: "test".into(),
            timestamp: now.clone(),
            content: "unrelated conceptual match".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let identifier = crate::import::ConversationChunk {
            id: "identifier-low".into(),
            conversation_id: "identifier-conversation".into(),
            project_name: "test".into(),
            timestamp: now,
            content: "implementation of parse_widget_identifier".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 1,
            is_sidechain: false,
        };
        let semantic_vec = vec![1.0, 0.0, 0.0, 0.0];
        let identifier_vec = vec![0.4, 0.916_515_1, 0.0, 0.0];
        storage.insert_chunk(&semantic, &semantic_vec).unwrap();
        storage.insert_chunk(&identifier, &identifier_vec).unwrap();
        let mut engine = SearchEngine::new(8);
        engine.insert_chunk(semantic.id.clone(), semantic_vec);
        engine.insert_chunk(identifier.id.clone(), identifier_vec);
        let search = Arc::new(RwLock::new(engine));

        let output = reflect_on_past_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "parse_widget_identifier",
            5,
            0.5,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();

        assert!(
            output.contains("<id>identifier-low</id>"),
            "hybrid search must include an exact FTS identifier even when another semantic hit exceeds 0.5:\n{output}"
        );
    }

    async fn render_ablation_mode(mode: SearchMode) -> String {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let now = chrono::Utc::now().to_rfc3339();
        let make_chunk =
            |id: &str, conversation_id: &str, content: &str| crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation_id.into(),
                project_name: "test".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
        let semantic = make_chunk(
            "mode-semantic",
            "mode-semantic-conversation",
            "conceptual vector-only match",
        );
        let keyword = make_chunk(
            "mode-keyword",
            "mode-keyword-conversation",
            "exact_mode_identifier keyword-only match",
        );
        let semantic_vec = vec![1.0, 0.0, 0.0, 0.0];
        let keyword_vec = vec![0.0, 1.0, 0.0, 0.0];
        storage.insert_chunk(&semantic, &semantic_vec).unwrap();
        storage.insert_chunk(&keyword, &keyword_vec).unwrap();
        let mut engine = SearchEngine::new(8);
        engine.insert_chunk(semantic.id.clone(), semantic_vec);
        engine.insert_chunk(keyword.id.clone(), keyword_vec);

        reflect_on_past_with_vec_mode(
            &storage,
            &Arc::new(RwLock::new(engine)),
            &[1.0, 0.0, 0.0, 0.0],
            "exact_mode_identifier",
            5,
            0.5,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            mode,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn vector_mode_excludes_fts_only_hits() {
        let output = render_ablation_mode(SearchMode::Vector).await;
        assert!(output.contains("<id>mode-semantic</id>"), "{output}");
        assert!(!output.contains("<id>mode-keyword</id>"), "{output}");
    }

    #[tokio::test]
    async fn fts_mode_excludes_semantic_only_hits() {
        let output = render_ablation_mode(SearchMode::Fts).await;
        assert!(output.contains("<id>mode-keyword</id>"), "{output}");
        assert!(!output.contains("<id>mode-semantic</id>"), "{output}");
    }

    #[tokio::test]
    async fn hybrid_limit_one_does_not_double_count_fts_only_hit() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let now = chrono::Utc::now().to_rfc3339();
        let make_chunk =
            |id: &str, conversation_id: &str, content: &str| crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation_id.into(),
                project_name: "test".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
        let semantic = make_chunk(
            "rrf-semantic",
            "rrf-semantic-conversation",
            "true semantic result without the identifier",
        );
        let keyword = make_chunk(
            "rrf-keyword-only",
            "rrf-keyword-conversation",
            "rrf_conflict_identifier keyword-only result",
        );
        let semantic_vec = vec![1.0, 0.0, 0.0, 0.0];
        let keyword_vec = vec![0.0, 1.0, 0.0, 0.0];
        storage.insert_chunk(&semantic, &semantic_vec).unwrap();
        storage.insert_chunk(&keyword, &keyword_vec).unwrap();
        let mut engine = SearchEngine::new(8);
        engine.insert_chunk(semantic.id.clone(), semantic_vec);
        engine.insert_chunk(keyword.id.clone(), keyword_vec);
        let search = Arc::new(RwLock::new(engine));

        for active_forgetting in [false, true] {
            let output = reflect_on_past_with_vec_mode(
                &storage,
                &search,
                &[1.0, 0.0, 0.0, 0.0],
                "rrf_conflict_identifier",
                1,
                0.5,
                Some("all"),
                0,
                true,
                ConsumptionMode::Full,
                active_forgetting,
                "other",
                SearchMode::Hybrid,
                RecallRerankMode::Baseline,
                false,
            )
            .await
            .unwrap();

            assert!(output.contains("<id>rrf-semantic</id>"), "{output}");
            assert!(!output.contains("<id>rrf-keyword-only</id>"), "{output}");
        }
    }

    #[tokio::test]
    async fn fts_mode_keeps_bm25_order_for_valid_results_with_active_forgetting_on_and_off() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let make_chunk = |id: &str, conversation_id: &str, timestamp: &str, content: &str| {
            crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation_id.into(),
                project_name: "test".into(),
                timestamp: timestamp.into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            }
        };
        let bm25_first = make_chunk(
            "bm25-first",
            "bm25-first-conversation",
            "2020-01-01T00:00:00Z",
            "bm25_order_identifier bm25_order_identifier bm25_order_identifier",
        );
        let newest_but_bm25_second = make_chunk(
            "bm25-second",
            "bm25-second-conversation",
            "2099-01-01T00:00:00Z",
            "bm25_order_identifier with several unrelated filler words around it",
        );
        storage.insert_chunk(&bm25_first, &[0.0; 4]).unwrap();
        storage
            .insert_chunk(&newest_but_bm25_second, &[0.0; 4])
            .unwrap();
        let stored_order = storage
            .fts5_search("bm25_order_identifier", 2, Some("test"))
            .unwrap();
        assert_eq!(stored_order[0].0.id, "bm25-first");
        assert_eq!(stored_order[1].0.id, "bm25-second");

        for active_forgetting in [false, true] {
            let output = reflect_on_past_with_vec_mode(
                &storage,
                &Arc::new(RwLock::new(SearchEngine::new(8))),
                &[1.0, 0.0, 0.0, 0.0],
                "bm25_order_identifier",
                2,
                0.5,
                Some("test"),
                0,
                true,
                ConsumptionMode::Full,
                active_forgetting,
                "other",
                SearchMode::Fts,
                RecallRerankMode::Baseline,
                false,
            )
            .await
            .unwrap();

            assert!(
                pos_of(&output, "<id>bm25-first</id>")
                    < pos_of(&output, "<id>bm25-second</id>"),
                "FTS valid results must retain BM25 order with active_forgetting={active_forgetting}:\n{output}"
            );
        }
    }

    fn demote_validity(note: &str) -> ConvValidity {
        ConvValidity {
            demote: true,
            note: note.to_string(),
        }
    }

    fn annotate_validity(note: &str) -> ConvValidity {
        ConvValidity {
            demote: false,
            note: note.to_string(),
        }
    }

    #[test]
    fn active_forgetting_flag_parsing_is_opt_in_only() {
        assert!(active_forgetting_enabled_from(Some("1")));
        for value in [None, Some("0"), Some("true"), Some("yes"), Some("")] {
            assert!(
                !active_forgetting_enabled_from(value),
                "value {value:?} must leave active forgetting off"
            );
        }
    }

    #[test]
    fn search_mode_parses_known_values_and_defaults_to_hybrid() {
        assert_eq!(search_mode_from(None), SearchMode::Hybrid);
        assert_eq!(search_mode_from(Some("hybrid")), SearchMode::Hybrid);
        assert_eq!(search_mode_from(Some("vector")), SearchMode::Vector);
        assert_eq!(search_mode_from(Some("fts")), SearchMode::Fts);
        for value in [Some(""), Some("unknown"), Some("HYBRID"), Some(" fts ")] {
            assert_eq!(
                search_mode_from(value),
                SearchMode::Hybrid,
                "value {value:?} must fall back to hybrid"
            );
        }
    }

    #[test]
    fn rrf_combines_rankings_and_breaks_ties_by_semantic_order() {
        let semantic = ["a", "b", "c"].map(str::to_string);
        let fts = ["c", "b", "d"].map(str::to_string);
        assert_eq!(fuse_rrf(&semantic, &fts, 60), ["c", "b", "a", "d"]);

        let semantic_tie = ["semantic-first", "fts-first"].map(str::to_string);
        let fts_tie = ["fts-first", "semantic-first"].map(str::to_string);
        assert_eq!(
            fuse_rrf(&semantic_tie, &fts_tie, 60),
            ["semantic-first", "fts-first"]
        );
    }

    #[test]
    fn active_forgetting_off_output_is_byte_identical_to_current_behavior() {
        let now = "2026-04-15T10:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let timestamp = "2026-01-15T10:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let validity: HashMap<String, ConvValidity> =
            [("stale".to_string(), demote_validity("stale"))]
                .into_iter()
                .collect();
        let score = 0.91_f32;

        let actual_score = apply_chunk_decay(
            score,
            &timestamp,
            &now,
            &[],
            &decay::DecayConfig::for_search(),
            &validity,
            "stale",
            None,
            false,
        );

        assert_eq!(actual_score.to_bits(), score.to_bits());

        let mut actual = vec![enriched_result_conv(
            "stale",
            "conv-demoted",
            actual_score,
            0,
        )];
        let mut expected = vec![enriched_result_conv("stale", "conv-demoted", score, 0)];
        apply_validity_partition(&mut actual, &validity, false);
        apply_validity_partition(&mut expected, &validity, false);

        assert_eq!(
            format::format_search_results(&actual, "q", "all", 5, 3),
            format::format_search_results(&expected, "q", "all", 5, 3)
        );
    }

    #[test]
    fn active_forgetting_demote_decays_faster_than_identical_clean_chunk() {
        let now = chrono::Utc::now();
        let timestamp = now - chrono::Duration::days(90);
        let validity: HashMap<String, ConvValidity> =
            [("chunk-demoted".to_string(), demote_validity("stale"))]
                .into_iter()
                .collect();
        let config = decay::DecayConfig::for_search();

        let demoted = apply_chunk_decay(
            1.0,
            &timestamp,
            &now,
            &[],
            &config,
            &validity,
            "chunk-demoted",
            None,
            true,
        );
        let clean = apply_chunk_decay(
            1.0,
            &timestamp,
            &now,
            &[],
            &config,
            &validity,
            "conv-clean",
            None,
            true,
        );

        assert!(demoted < clean, "demoted={demoted}, clean={clean}");
    }

    #[test]
    fn verdict_partitioned_chunk_does_not_stack_ancestry_decay() {
        use crate::storage::ancestry::{AncestryLabel, AncestryState};

        let now = chrono::Utc::now();
        let past = now - chrono::Duration::days(30);
        let config = decay::DecayConfig::for_search();
        let validity: HashMap<String, ConvValidity> =
            [("chunk-demoted".to_string(), demote_validity("stale"))]
                .into_iter()
                .collect();
        let label = AncestryLabel {
            conversation_id: "conv-demoted".into(),
            state: AncestryState::Shipped,
            release_tag: Some("v1.0.0".into()),
            releases_behind: 5,
            repository: "/repo".into(),
            refreshed_at: "2026-08-06T12:00:00Z".into(),
        };

        let expected = decay::apply_tad_with_age_multiplier(
            0.9,
            &past,
            &now,
            &[],
            &config,
            ACTIVE_FORGETTING_DECAY_FACTOR,
        );
        let actual = apply_chunk_decay(
            0.9,
            &past,
            &now,
            &[],
            &config,
            &validity,
            "chunk-demoted",
            Some(&label),
            true,
        );

        assert_eq!(actual.to_bits(), expected.to_bits());
    }

    #[test]
    fn semantic_scaffold_suppresses_ancestry_and_neutral_paths_are_bit_identical() {
        use crate::storage::ancestry::{AncestryLabel, AncestryState};

        let now = chrono::Utc::now();
        let mut chunk = enriched_result_conv("scaffold", "conv-scaffold", 0.9, 0).chunk;
        chunk.timestamp = (now - chrono::Duration::days(30)).to_rfc3339();
        chunk.content = "<command-message>quoted workflow</command-message>".into();
        let config = decay::DecayConfig::for_search();
        let label = |releases_behind| AncestryLabel {
            conversation_id: "conv-scaffold".into(),
            state: AncestryState::Shipped,
            release_tag: Some("v1.0.0".into()),
            releases_behind,
            repository: "/repo".into(),
            refreshed_at: now.to_rfc3339(),
        };
        let timestamp = chunk.timestamp.parse().unwrap();
        let pre_change = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
        let missing = score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &HashMap::new(),
            None,
            false,
            &unscoped_search_scope(),
        );
        let current_release = label(0);
        let current = score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &HashMap::new(),
            Some(&current_release),
            false,
            &unscoped_search_scope(),
        );
        let shipped_release = label(5);
        let shipped = score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &HashMap::new(),
            Some(&shipped_release),
            false,
            &unscoped_search_scope(),
        );

        assert_eq!(missing.0.to_bits(), pre_change.to_bits());
        assert_eq!(current.0.to_bits(), pre_change.to_bits());
        assert_eq!(shipped.0.to_bits(), pre_change.to_bits());
        assert!(!missing.1 && !current.1 && !shipped.1);

        chunk.content = "organic conversation".into();
        let organic_pre_change = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
        let organic_missing = score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &HashMap::new(),
            None,
            false,
            &unscoped_search_scope(),
        );
        let organic_current = score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &HashMap::new(),
            Some(&current_release),
            false,
            &unscoped_search_scope(),
        );
        assert_eq!(organic_missing.0.to_bits(), organic_pre_change.to_bits());
        assert_eq!(organic_current.0.to_bits(), organic_pre_change.to_bits());
    }

    #[test]
    fn fts_scoring_none_and_current_release_match_pre_ancestry_bits() {
        use crate::storage::ancestry::{AncestryLabel, AncestryState};

        let now = chrono::Utc::now();
        let mut chunk = enriched_result_conv("fts", "conv-fts", 0.45, 0).chunk;
        chunk.timestamp = (now - chrono::Duration::days(30)).to_rfc3339();
        chunk.content = "zebraquark organic conversation".into();
        let timestamp = chunk.timestamp.parse().unwrap();
        let current_release = AncestryLabel {
            conversation_id: "conv-fts".into(),
            state: AncestryState::Shipped,
            release_tag: Some("v-current".into()),
            releases_behind: 0,
            repository: "/repo".into(),
            refreshed_at: now.to_rfc3339(),
        };

        let pre_change = decay::apply_decay(0.45, &timestamp, &now, None, None);
        let missing = score_fts_candidate(&chunk, &now, &HashMap::new(), None, false);
        let current =
            score_fts_candidate(&chunk, &now, &HashMap::new(), Some(&current_release), false);

        assert_eq!(missing.0.to_bits(), pre_change.to_bits());
        assert_eq!(current.0.to_bits(), pre_change.to_bits());
        assert!(!missing.1 && !current.1);
    }

    #[test]
    fn validity_read_failure_disables_ancestry_score_effect() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let refreshed_at = chrono::Utc::now().to_rfc3339();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES
                       ('conv-demoted', 'shipped', 'v1.0.0', 5, '/repo', ?1),
                       ('conv-fts', 'shipped', 'v1.0.0', 5, '/repo', ?1)",
                    [&refreshed_at],
                )?;
                Ok(())
            })
            .unwrap();
        let chunk = enriched_result_conv("candidate", "conv-demoted", 0.9, 0).chunk;
        let conversation_ids = vec!["conv-demoted".to_string()];
        let mut signals = CandidateSignals::load(
            &storage,
            std::slice::from_ref(&chunk),
            true,
            ConsumptionMode::Full,
        );
        assert!(signals.ancestry.contains_key("conv-demoted"));
        let now = "2026-08-06T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let mut chunk = chunk;
        chunk.timestamp = (now - chrono::Duration::days(30)).to_rfc3339();
        let config = decay::DecayConfig::for_search();
        let (ancestry_score, ancestry_applied) = score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &signals.validity,
            signals.ancestry.get("conv-demoted"),
            false,
            &unscoped_search_scope(),
        );
        assert!(ancestry_applied);

        storage
            .with_connection(|conn| {
                // The validity resolver starts from code_nodes. Removing it
                // gives us a real SQLite read failure while leaving the
                // independently cached ancestry label readable.
                conn.execute_batch("DROP TABLE code_nodes")?;
                Ok(())
            })
            .unwrap();
        assert!(storage
            .witness_verdicts_for_conversations(&conversation_ids)
            .is_err());

        let fts_chunk = enriched_result_conv("fts", "conv-fts", 0.8, 0).chunk;
        let ancestry_revoked = signals.extend(&storage, &[fts_chunk], true, ConsumptionMode::Full);
        assert!(ancestry_revoked);
        assert!(
            signals.ancestry.is_empty(),
            "an FTS validity failure must clear semantic ancestry for the whole pass"
        );

        let (actual, ancestry_applied) = score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &signals.validity,
            signals.ancestry.get("conv-demoted"),
            false,
            &unscoped_search_scope(),
        );
        let timestamp = chunk
            .timestamp
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let expected = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
        assert!(!ancestry_applied);
        assert!(ancestry_score < actual);
        assert_eq!(actual.to_bits(), expected.to_bits());

        let failed_initial = CandidateSignals::load(
            &storage,
            std::slice::from_ref(&chunk),
            true,
            ConsumptionMode::Full,
        );
        assert!(failed_initial.validity.is_empty());
        assert!(failed_initial.ancestry.is_empty());
    }

    #[test]
    fn active_forgetting_does_not_change_annotate_channel_decay() {
        let now = chrono::Utc::now();
        let timestamp = now - chrono::Duration::days(90);
        let validity: HashMap<String, ConvValidity> =
            [("chunk-annotated".to_string(), annotate_validity("evolved"))]
                .into_iter()
                .collect();
        let config = decay::DecayConfig::for_search();

        let annotated = apply_chunk_decay(
            1.0,
            &timestamp,
            &now,
            &[],
            &config,
            &validity,
            "chunk-annotated",
            None,
            true,
        );
        let clean = apply_chunk_decay(
            1.0,
            &timestamp,
            &now,
            &[],
            &config,
            &validity,
            "conv-clean",
            None,
            true,
        );

        assert_eq!(annotated.to_bits(), clean.to_bits());
    }

    #[test]
    fn demoted_chunk_sinks_below_non_demoted_stable_order_preserved() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-demoted", 0.95, 0),
            enriched_result_conv("b", "conv-clean-1", 0.90, 1),
            enriched_result_conv("c", "conv-demoted", 0.85, 2),
            enriched_result_conv("d", "conv-clean-2", 0.80, 3),
        ];
        let validity: HashMap<String, ConvValidity> = [
            (
                "a".to_string(),
                demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
            ),
            (
                "c".to_string(),
                demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
            ),
        ]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity, false);

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        // Non-demoted (b, d) keep their relative order, ahead of demoted
        // (a, c) which ALSO keep their relative order among themselves.
        assert_eq!(ids, ["b", "d", "a", "c"]);
    }

    #[test]
    fn validity_demote_verdict_only_sinks_its_own_chunk_in_shared_conversation() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut enriched = vec![
            enriched_result_conv("chunk-stale", "conv-shared", 0.95, 0),
            enriched_result_conv("chunk-live", "conv-shared", 0.90, 1),
        ];
        // The stored verdict already owns its chunk identity. Neither chunk
        // needs to repeat the symbol/filename text for binding to work.
        enriched[0].chunk.content = "historical implementation discussion".into();
        enriched[1].chunk.content = "unrelated live design discussion".into();
        let live_score = enriched[1].score;
        for result in &enriched {
            storage.insert_chunk(&result.chunk, &[1.0, 0.0]).unwrap();
        }
        let witness_id = install_demote_verdict_for(
            &storage,
            "/repo/src/lib.rs",
            "old_fn",
            "conv-shared",
            "conv-shared",
        );
        assert!(
            resolve_validity_with(
                &storage,
                std::slice::from_ref(&enriched[1].chunk),
                ConsumptionMode::Full,
            )
            .is_empty(),
            "legacy conversation-only identity must abstain when storage contains siblings"
        );
        storage
            .with_connection(|conn| {
                crate::storage::witness_verdicts::bind_witness_to_chunk(
                    conn,
                    witness_id,
                    "chunk-stale",
                )?;
                crate::storage::witness_ledger::insert_witness(
                    conn,
                    &crate::storage::witness_ledger::WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        symbol: Some("old_fn".into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:2".into(),
                        tier: "committed".into(),
                        at_oid: Some("bbb".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("bbb:old_fn".into()),
                    },
                )?;
                let newest_witness = crate::storage::witness_ledger::latest_witness_for_symbol(
                    conn,
                    "proj",
                    "/repo/src/lib.rs",
                    Some("old_fn"),
                )?
                .expect("second witness must exist");
                crate::storage::witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &crate::storage::witness_verdicts::WitnessVerdictRow {
                        witness_id: newest_witness.id,
                        verdict: crate::storage::witness_verdicts::VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: Some("cafebabe".into()),
                        observed_head_oid: "cafebabe".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();
        let chunks: Vec<_> = enriched.iter().map(|result| result.chunk.clone()).collect();
        let validity = resolve_validity_with(&storage, &chunks, ConsumptionMode::Full);

        assert!(is_demote_channel(&validity, "chunk-stale"));
        assert!(!validity.contains_key("chunk-live"));
        assert!(
            resolve_validity_with(
                &storage,
                std::slice::from_ref(&chunks[1]),
                ConsumptionMode::Full,
            )
            .is_empty(),
            "a retrieval window containing only the clean sibling must not inherit the verdict"
        );

        let rerank_ids: Vec<&str> = enriched
            .iter()
            .filter(|result| !is_demote_channel(&validity, &result.chunk.id))
            .map(|result| result.chunk.id.as_str())
            .collect();
        assert_eq!(rerank_ids, ["chunk-live"]);

        let now = chrono::Utc::now();
        let label = crate::storage::ancestry::AncestryLabel {
            conversation_id: "conv-shared".into(),
            state: crate::storage::ancestry::AncestryState::Shipped,
            release_tag: Some("v1.0.0".into()),
            releases_behind: 5,
            repository: "/repo".into(),
            refreshed_at: now.to_rfc3339(),
        };
        let config = decay::DecayConfig::for_search();
        let (_, stale_ancestry_applied) = score_chunk_candidate(
            enriched[0].score,
            &enriched[0].chunk,
            &now,
            &[],
            &config,
            &validity,
            Some(&label),
            false,
            &unscoped_search_scope(),
        );
        let (_, live_ancestry_applied) = score_chunk_candidate(
            enriched[1].score,
            &enriched[1].chunk,
            &now,
            &[],
            &config,
            &validity,
            Some(&label),
            false,
            &unscoped_search_scope(),
        );
        assert!(!stale_ancestry_applied);
        assert!(live_ancestry_applied);

        apply_validity_partition(&mut enriched, &validity, false);

        assert_eq!(enriched[0].chunk.id, "chunk-live");
        assert_eq!(enriched[0].score.to_bits(), live_score.to_bits());
        assert_eq!(enriched[0].resolution, None);
        assert!(!enriched[0].validity_demoted);
        assert_eq!(enriched[1].chunk.id, "chunk-stale");
        assert!(enriched[1].validity_demoted);
        assert_eq!(
            enriched[1].resolution.as_deref(),
            Some("[stale anchor] old_fn no longer in current code (receipt cafebab)")
        );
    }

    #[test]
    fn demoted_chunk_still_returned_if_it_fits_the_limit() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-demoted", 0.95, 0),
            enriched_result_conv("b", "conv-clean", 0.90, 1),
        ];
        let validity: HashMap<String, ConvValidity> = [(
            "a".to_string(),
            demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity, false);
        enriched.truncate(2); // both fit — demoted is sunk, not dropped

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }

    #[test]
    fn demoted_chunk_annotation_string_exact() {
        let mut enriched = vec![enriched_result_conv("a", "conv-demoted", 0.95, 0)];
        let validity: HashMap<String, ConvValidity> = [(
            "a".to_string(),
            demote_validity("[stale anchor] old_fn no longer in current code (receipt abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity, false);

        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("[stale anchor] old_fn no longer in current code (receipt abc1234)")
        );
    }

    #[test]
    fn annotate_channel_does_not_move_rank_only_annotates() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-evolved", 0.70, 0),
            enriched_result_conv("b", "conv-clean", 0.95, 1),
        ];
        let validity: HashMap<String, ConvValidity> = [(
            "a".to_string(),
            annotate_validity("[evolved] foo changed since this conversation (as of abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity, false);

        // Order untouched — "a" (lower score) was already below "b" and
        // stays there; Annotate never moves rank.
        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("[evolved] foo changed since this conversation (as of abc1234)")
        );
        assert_eq!(enriched[1].resolution.as_deref(), None);
    }

    #[test]
    fn annotate_note_merges_with_existing_resolution_ledger_note() {
        let mut enriched = vec![enriched_result_conv("a", "conv-evolved", 0.70, 0)];
        enriched[0].resolution = Some("still_open: earlier verdict".to_string());
        let validity: HashMap<String, ConvValidity> = [(
            "a".to_string(),
            annotate_validity("[evolved] foo changed since this conversation (as of abc1234)"),
        )]
        .into_iter()
        .collect();

        apply_validity_partition(&mut enriched, &validity, false);

        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("still_open: earlier verdict; [evolved] foo changed since this conversation (as of abc1234)")
        );
    }

    #[test]
    fn empty_validity_map_is_a_no_op() {
        let mut enriched = vec![
            enriched_result_conv("a", "conv-1", 0.95, 0),
            enriched_result_conv("b", "conv-2", 0.90, 1),
        ];
        apply_validity_partition(&mut enriched, &HashMap::new(), false);
        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert!(enriched.iter().all(|e| e.resolution.is_none()));
    }

    #[test]
    fn kill_switch_disables_validity_partition() {
        // `enabled = false` (what `validity_partition_enabled` returns when
        // CSR_NO_VALIDITY_PARTITION=1) must make resolution return an empty
        // map regardless of what storage would otherwise report. Driven by
        // parameter, not by mutating the real env var — see
        // `resolve_validity_with`'s doc on why (parallel test races).
        let storage = Arc::new(Storage::open_memory().unwrap());
        let refreshed_at = chrono::Utc::now().to_rfc3339();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES ('conv-1', 'shipped', 'v1.0.0', 5, '/repo', ?1)",
                    [&refreshed_at],
                )?;
                Ok(())
            })
            .unwrap();
        let chunk = enriched_result("chunk-1", 0.9, 0).chunk;
        let out =
            resolve_validity_with(&storage, std::slice::from_ref(&chunk), ConsumptionMode::Off);
        assert!(out.is_empty());
        let signals = CandidateSignals::load(&storage, &[chunk], false, ConsumptionMode::Off);
        assert!(signals.validity.is_empty());
        assert!(
            signals.ancestry.is_empty(),
            "without a validity batch, the kill switch must also disable ancestry"
        );
        assert!(!signals.ancestry_allowed);
    }

    #[test]
    fn validity_partition_enabled_parses_env_var() {
        // The one place this suite touches the real process env var for
        // this feature — every other test drives `resolve_validity_with`
        // directly by parameter to avoid racing with this test under
        // cargo's parallel test runner. The guard serializes us against the
        // other env-sensitive tests (status, eval gate) that also hold it.
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let restore = std::env::var("CSR_NO_VALIDITY_PARTITION").ok();
        std::env::set_var("CSR_NO_VALIDITY_PARTITION", "1");
        assert!(!validity_partition_enabled());
        std::env::set_var("CSR_NO_VALIDITY_PARTITION", "0");
        assert!(validity_partition_enabled());
        std::env::remove_var("CSR_NO_VALIDITY_PARTITION");
        assert!(validity_partition_enabled());
        match restore {
            Some(v) => std::env::set_var("CSR_NO_VALIDITY_PARTITION", v),
            None => std::env::remove_var("CSR_NO_VALIDITY_PARTITION"),
        }
    }

    #[test]
    fn synthetic_db_demoted_symbol_partitions_end_to_end() {
        // Full DB-backed exercise of the v10 validity partition: a real
        // code_nodes + witness_ledger + witness_verdicts fixture producing a
        // genuine Demote-channel verdict, resolved through `resolve_validity`
        // (the same storage round-trip `reflect_on_past` makes) — not a
        // fabricated ConvValidity map like the tests above. Covers:
        // partition order, exact annotation string, no-stacking (the shared
        // `is_demote_channel` predicate agreeing with the partition), and
        // the kill switch, all against one synthetic DB with a
        // demoted-symbol conversation plus two untouched "valid" ones.
        use crate::storage::codegraph::{upsert_node, NodeRow};
        use crate::storage::witness_ledger::{self, WitnessLedgerRow};
        use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .with_connection(|conn| {
                upsert_node(
                    conn,
                    &NodeRow {
                        id: crate::extraction::codegraph::node_id(
                            "proj",
                            "/repo/src/lib.rs",
                            "function",
                            "old_fn",
                        ),
                        repo: "proj".into(),
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        lang: "rust".into(),
                        kind: "function".into(),
                        name: "old_fn".into(),
                        fqname: String::new(),
                        body_hash: String::new(),
                        span_start: 1,
                        span_end: 3,
                        first_conv_id: "conv-demoted".into(),
                        last_conv_id: "conv-demoted".into(),
                        last_session_id: "sess".into(),
                        repo_root: None,
                        name_only: false,
                        attribution: String::new(),
                    },
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        symbol: Some("old_fn".into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:1".into(),
                        tier: "committed".into(),
                        at_oid: Some("aaa".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("aaa".into()),
                    },
                )?;
                let wid = witness_ledger::latest_witness_for_symbol(
                    conn,
                    "proj",
                    "/repo/src/lib.rs",
                    Some("old_fn"),
                )?
                .unwrap()
                .id;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id: wid,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: Some("deadbeef00".into()),
                        observed_head_oid: "deadbeef00".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();

        let mut enriched = vec![
            // Highest raw score of the three — if the partition were
            // stacking with (or losing to) score-based ranking, this would
            // stay #1. It must not.
            enriched_result_conv("stale", "conv-demoted", 0.95, 0),
            enriched_result_conv("valid-1", "conv-clean-1", 0.90, 1),
            enriched_result_conv("valid-2", "conv-clean-2", 0.80, 2),
        ];
        enriched[0].chunk.content = "old_fn stale claim".into();
        for result in &enriched {
            storage.insert_chunk(&result.chunk, &[1.0, 0.0]).unwrap();
        }
        let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();
        let validity = resolve_validity_with(&storage, &chunks, ConsumptionMode::Full);

        // NO STACKING: the shared skip predicate agrees with what the
        // partition below will do — this is the exact check
        // `reflect_on_past`'s TAD loop and rerank filter make.
        assert!(is_demote_channel(&validity, "stale"));
        assert!(!is_demote_channel(&validity, "valid-1"));
        assert!(!is_demote_channel(&validity, "valid-2"));

        apply_validity_partition(&mut enriched, &validity, false);

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(
            ids,
            ["valid-1", "valid-2", "stale"],
            "highest-scored chunk still sinks below both lower-scored valid \
             chunks — the partition, not the score, decides rank"
        );
        assert_eq!(
            enriched[2].resolution.as_deref(),
            Some("[stale anchor] old_fn no longer in current code (receipt deadbee)")
        );
        assert!(enriched[0].resolution.is_none());
        assert!(enriched[1].resolution.is_none());

        // Kill switch, same populated DB and same real storage round-trip
        // (via `resolve_validity_with(..., false)` — see that function's
        // doc on why tests drive this by parameter, never by mutating the
        // real env var): resolution must come back empty and the partition
        // must be a total no-op.
        let validity_off = resolve_validity_with(&storage, &chunks, ConsumptionMode::Off);
        assert!(validity_off.is_empty());

        let mut enriched_off = vec![
            enriched_result_conv("stale", "conv-demoted", 0.95, 0),
            enriched_result_conv("valid-1", "conv-clean-1", 0.90, 1),
        ];
        apply_validity_partition(&mut enriched_off, &validity_off, false);
        let ids_off: Vec<&str> = enriched_off.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(
            ids_off,
            ["stale", "valid-1"],
            "kill switch: original order preserved, nothing demoted/annotated"
        );
        assert!(enriched_off.iter().all(|e| e.resolution.is_none()));
    }

    /// Shared synthetic-DB builder for the dream-consumption tests below: ONE
    /// Demote-channel conversation (`conv-demoted`, `AnchorObsolete` — no ledger
    /// row at the observed HEAD oid, so the symbol is "truly gone"; same shape
    /// as `synthetic_db_demoted_symbol_partitions_end_to_end`'s fixture above,
    /// inlined here so this fixture is self-contained), ONE Annotate-channel
    /// conversation (`conv-annotated`, `SupersededBy` A->B evolution — same
    /// shape as `chunk_binding::a_b_evolution_yields_annotate_not_demote`, so
    /// the symbol IS intact at HEAD), and a release-ancestry cache row for a
    /// THIRD, otherwise-untouched conversation (`conv-ancestry`) proving
    /// ancestry loads independently of whichever verdict channel is present.
    /// `synthetic_db_demoted_symbol_partitions_end_to_end` above is left
    /// untouched on purpose (predates `CSR_DREAM_CONSUMPTION`, drives
    /// `resolve_validity_with` directly by parameter, bypasses the new gate
    /// entirely — zero edits needed, must keep passing byte-for-byte).
    fn dream_consumption_fixture() -> (Arc<Storage>, Vec<EnrichedResult>) {
        use crate::storage::codegraph::{upsert_node, NodeRow};
        use crate::storage::witness_ledger::{self, WitnessLedgerRow};
        use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

        let storage = Arc::new(Storage::open_memory().unwrap());
        storage
            .with_connection(|conn| {
                // conv-demoted / old_fn: single ledger row, no HEAD-oid row —
                // Demote channel.
                upsert_node(
                    conn,
                    &NodeRow {
                        id: crate::extraction::codegraph::node_id(
                            "proj",
                            "/repo/src/lib.rs",
                            "function",
                            "old_fn",
                        ),
                        repo: "proj".into(),
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        lang: "rust".into(),
                        kind: "function".into(),
                        name: "old_fn".into(),
                        fqname: String::new(),
                        body_hash: String::new(),
                        span_start: 1,
                        span_end: 3,
                        first_conv_id: "conv-demoted".into(),
                        last_conv_id: "conv-demoted".into(),
                        last_session_id: "sess".into(),
                        repo_root: None,
                        name_only: false,
                        attribution: String::new(),
                    },
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/lib.rs".into(),
                        symbol: Some("old_fn".into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:1".into(),
                        tier: "committed".into(),
                        at_oid: Some("aaa".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("aaa".into()),
                    },
                )?;
                let old_wid = witness_ledger::latest_witness_for_symbol(
                    conn,
                    "proj",
                    "/repo/src/lib.rs",
                    Some("old_fn"),
                )?
                .unwrap()
                .id;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id: old_wid,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: Some("deadbeef00".into()),
                        observed_head_oid: "deadbeef00".into(),
                    },
                )?;

                // conv-annotated / evolved_fn: two ledger rows (aaa2 -> bbb),
                // the second AT the observed HEAD oid — Annotate channel.
                upsert_node(
                    conn,
                    &NodeRow {
                        id: crate::extraction::codegraph::node_id(
                            "proj",
                            "/repo/src/evolved.rs",
                            "function",
                            "evolved_fn",
                        ),
                        repo: "proj".into(),
                        project: "proj".into(),
                        file: "/repo/src/evolved.rs".into(),
                        lang: "rust".into(),
                        kind: "function".into(),
                        name: "evolved_fn".into(),
                        fqname: String::new(),
                        body_hash: String::new(),
                        span_start: 1,
                        span_end: 3,
                        first_conv_id: "conv-annotated".into(),
                        last_conv_id: "conv-annotated".into(),
                        last_session_id: "sess".into(),
                        repo_root: None,
                        name_only: false,
                        attribution: String::new(),
                    },
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/evolved.rs".into(),
                        symbol: Some("evolved_fn".into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:A".into(),
                        tier: "committed".into(),
                        at_oid: Some("aaa2".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("aaa2".into()),
                    },
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: "/repo/src/evolved.rs".into(),
                        symbol: Some("evolved_fn".into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:B".into(),
                        tier: "committed".into(),
                        at_oid: Some("bbb".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("bbb".into()),
                    },
                )?;
                let a_wid: i64 = conn.query_row(
                    "SELECT id FROM witness_ledger WHERE at_oid = 'aaa2'",
                    [],
                    |r| r.get(0),
                )?;
                let b_wid: i64 = conn.query_row(
                    "SELECT id FROM witness_ledger WHERE at_oid = 'bbb'",
                    [],
                    |r| r.get(0),
                )?;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id: a_wid,
                        verdict: VerdictKind::SupersededBy,
                        successor_witness_id: Some(b_wid),
                        receipt_oid: Some("bbb".into()),
                        observed_head_oid: "bbb".into(),
                    },
                )?;

                // conv-ancestry: an independent release-ancestry cache row
                // with NO witness verdict at all — proves ancestry loads
                // regardless of dream-consumption state.
                let refreshed_at = chrono::Utc::now().to_rfc3339();
                conn.execute(
                    "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES ('conv-ancestry', 'shipped', 'v1.0.0', 3, '/repo', ?1)",
                    [&refreshed_at],
                )?;
                Ok(())
            })
            .unwrap();

        let enriched = vec![
            enriched_result_conv("stale", "conv-demoted", 0.95, 0),
            enriched_result_conv("evolved", "conv-annotated", 0.92, 1),
            enriched_result_conv("valid-1", "conv-clean-1", 0.90, 2),
            enriched_result_conv("valid-2", "conv-clean-2", 0.80, 3),
        ];
        for result in &enriched {
            storage.insert_chunk(&result.chunk, &[1.0, 0.0]).unwrap();
        }
        (storage, enriched)
    }

    #[test]
    fn dream_consumption_off_never_queries_witness_verdicts_but_preserves_ancestry() {
        // The regression this proves: `CandidateSignals::load`'s ancestry gate
        // (`ancestry_enabled`, the pre-existing `CSR_NO_VALIDITY_PARTITION` kill
        // switch) must stay independent from the NEW dream-consumption gate
        // (`consumption_enabled`) — exactly the bug the rejected first T2
        // attempt shipped by folding both into one boolean. Proven two ways:
        // (1) dropping `witness_verdicts` entirely and confirming `load` still
        // succeeds (never touches the table when consumption is off — not just
        // "happens to return an empty result the same way an error would");
        // (2) the release-ancestry cache row for `conv-ancestry` still loads.
        let (storage, _enriched) = dream_consumption_fixture();
        storage
            .with_connection(|conn| {
                conn.execute_batch("DROP TABLE witness_verdicts")?;
                Ok(())
            })
            .unwrap();

        let chunks = vec![
            enriched_result_conv("stale", "conv-demoted", 0.95, 0).chunk,
            enriched_result_conv("evolved", "conv-annotated", 0.92, 1).chunk,
            enriched_result_conv("ancestry", "conv-ancestry", 0.90, 2).chunk,
        ];
        let consumption_mode = dream_consumption_mode_from(Some("off"));
        assert_eq!(consumption_mode, ConsumptionMode::Off);

        let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
        assert!(
            signals.validity.is_empty(),
            "no witness_verdicts query means no verdict can have been read"
        );
        assert!(
            signals.ancestry.contains_key("conv-ancestry"),
            "ancestry must load even though witness_verdicts is gone and consumption \
             is off — the two signals are independent"
        );
        assert!(signals.ancestry_allowed);
    }

    #[test]
    fn dream_consumption_off_suppresses_all_verdict_text_end_to_end() {
        let (storage, mut enriched) = dream_consumption_fixture();
        let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();

        let consumption_mode = dream_consumption_mode_from(Some("0"));
        assert_eq!(consumption_mode, ConsumptionMode::Off);

        let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
        assert!(signals.validity.is_empty());

        apply_validity_partition(&mut enriched, &signals.validity, false);
        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(
            ids,
            ["stale", "evolved", "valid-1", "valid-2"],
            "dream consumption OFF: original score order preserved, nothing demoted \
             or annotated"
        );
        assert!(
            enriched.iter().all(|e| e.resolution.is_none()),
            "no [stale anchor]/[evolved] text may render when consumption is off"
        );
    }

    #[test]
    fn dream_consumption_annotate_only_renders_notes_without_sinking() {
        let (storage, mut enriched) = dream_consumption_fixture();
        let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();

        let consumption_mode = dream_consumption_mode_from(None);
        assert_eq!(consumption_mode, ConsumptionMode::AnnotateOnly);

        let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
        assert!(!is_demote_channel(&signals.validity, "stale"));
        assert!(!is_demote_channel(&signals.validity, "evolved"));

        apply_validity_partition(&mut enriched, &signals.validity, false);
        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, ["stale", "evolved", "valid-1", "valid-2"]);
        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("[stale anchor] old_fn no longer in current code (receipt deadbee)")
        );
        assert_eq!(
            enriched[1].resolution.as_deref(),
            Some("[evolved] evolved_fn changed since this conversation (as of bbb)")
        );
        assert!(enriched.iter().all(|result| !result.validity_demoted));
    }

    #[test]
    fn dream_consumption_full_reproduces_demote_and_annotate_channels_byte_for_byte() {
        let (storage, mut enriched) = dream_consumption_fixture();
        enriched[0].chunk.content = "old_fn stale claim".into();
        enriched[1].chunk.content = "evolved_fn evolved claim".into();
        let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();

        let consumption_mode = dream_consumption_mode_from(Some("full"));
        assert_eq!(consumption_mode, ConsumptionMode::Full);

        let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
        assert!(is_demote_channel(&signals.validity, "stale"));
        assert!(!is_demote_channel(&signals.validity, "evolved"));
        assert!(
            signals.validity.contains_key("evolved"),
            "the Annotate channel must still be present in the map, just not demoted"
        );
        assert!(!is_demote_channel(&signals.validity, "valid-1"));
        assert!(!is_demote_channel(&signals.validity, "valid-2"));

        apply_validity_partition(&mut enriched, &signals.validity, false);
        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(
            ids,
            ["evolved", "valid-1", "valid-2", "stale"],
            "Demote sinks below everything; Annotate stays in place"
        );
        assert_eq!(
            enriched[0].resolution.as_deref(),
            Some("[evolved] evolved_fn changed since this conversation (as of bbb)")
        );
        assert!(enriched[1].resolution.is_none());
        assert!(enriched[2].resolution.is_none());
        assert_eq!(
            enriched[3].resolution.as_deref(),
            Some("[stale anchor] old_fn no longer in current code (receipt deadbee)")
        );
    }

    #[test]
    fn short_oid_truncates_to_seven_chars() {
        assert_eq!(short_oid("abcdef1234567890"), "abcdef1");
    }

    #[test]
    fn short_oid_never_panics_on_short_fixture_oid() {
        assert_eq!(short_oid("bbb"), "bbb");
    }

    #[test]
    fn validity_note_exact_wording_demote() {
        let hit = ChunkWitnessVerdict {
            chunk_id: "chunk-demote".into(),
            file: "/repo/src/lib.rs".into(),
            symbol: Some("old_fn".into()),
            channel: VerdictChannel::Demote,
            verdict: "anchor_obsolete",
            receipt_oid: Some("abcdef1234567890".into()),
        };
        assert_eq!(
            validity_note(&hit),
            "[stale anchor] old_fn no longer in current code (receipt abcdef1)"
        );
    }

    #[test]
    fn validity_note_exact_wording_annotate() {
        let hit = ChunkWitnessVerdict {
            chunk_id: "chunk-annotate".into(),
            file: "/repo/src/lib.rs".into(),
            symbol: Some("foo".into()),
            channel: VerdictChannel::Annotate,
            verdict: "superseded_by",
            receipt_oid: Some("abcdef1234567890".into()),
        };
        assert_eq!(
            validity_note(&hit),
            "[evolved] foo changed since this conversation (as of abcdef1)"
        );
    }

    #[test]
    fn validity_prefers_demote_over_annotate_for_same_chunk() {
        // One chunk touching two symbols — one demoted, one merely evolved —
        // takes its own strongest channel without affecting sibling chunks.
        let demote_hit = ChunkWitnessVerdict {
            chunk_id: "chunk-both".into(),
            file: "/repo/src/a.rs".into(),
            symbol: Some("gone_fn".into()),
            channel: VerdictChannel::Demote,
            verdict: "anchor_obsolete",
            receipt_oid: Some("head1".into()),
        };
        let annotate_hit = ChunkWitnessVerdict {
            chunk_id: "chunk-both".into(),
            file: "/repo/src/b.rs".into(),
            symbol: Some("evolved_fn".into()),
            channel: VerdictChannel::Annotate,
            verdict: "superseded_by",
            receipt_oid: Some("head2".into()),
        };
        let mut chunk = enriched_result_conv("chunk-both", "conv-both", 0.9, 0).chunk;
        chunk.content = "gone_fn and evolved_fn".into();
        let validity = reduce_validity_hits(
            BTreeMap::from([("chunk-both".into(), vec![annotate_hit, demote_hit])]),
            &[chunk],
        );
        assert!(is_demote_channel(&validity, "chunk-both"));
    }

    // --- end-to-end: real tool path through retrieval → FTS-append →
    // validity → rerank → truncate → format ---

    /// Synthetic DB + HNSW index exercised through the ACTUAL
    /// `reflect_on_past_with_vec` / `get_more_results_with_vec` seams (the
    /// full handler minus only the FastEmbed query embedding): chunks,
    /// code_nodes, witness_ledger, witness_verdicts, and an FTS row, with a
    /// genuine Demote-channel verdict against `conv-demoted`.
    /// code_nodes + witness_ledger + witness_verdicts rows producing a
    /// genuine Demote-channel verdict for `conv-demoted` (symbol `old_fn`,
    /// receipt `deadbeef00`) — shared by every DB-backed e2e fixture.
    fn install_demote_verdict(storage: &Arc<Storage>) {
        install_demote_verdict_for(
            storage,
            "/repo/src/lib.rs",
            "old_fn",
            "conv-demoted",
            "conv-demoted-new",
        );
    }

    fn install_demote_verdict_for(
        storage: &Arc<Storage>,
        file: &str,
        symbol: &str,
        first_conv_id: &str,
        last_conv_id: &str,
    ) -> i64 {
        use crate::storage::codegraph::{upsert_node, NodeRow};
        use crate::storage::witness_ledger::{self, WitnessLedgerRow};
        use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

        storage
            .with_connection(|conn| {
                upsert_node(
                    conn,
                    &NodeRow {
                        id: crate::extraction::codegraph::node_id("proj", file, "function", symbol),
                        repo: "proj".into(),
                        project: "proj".into(),
                        file: file.into(),
                        lang: "rust".into(),
                        kind: "function".into(),
                        name: symbol.into(),
                        fqname: String::new(),
                        body_hash: String::new(),
                        span_start: 1,
                        span_end: 3,
                        first_conv_id: first_conv_id.into(),
                        last_conv_id: last_conv_id.into(),
                        last_session_id: "sess".into(),
                        repo_root: None,
                        name_only: false,
                        attribution: String::new(),
                    },
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "proj".into(),
                        file: file.into(),
                        symbol: Some(symbol.into()),
                        span_start: Some(1),
                        span_end: Some(3),
                        stamp: "b3:1".into(),
                        tier: "committed".into(),
                        at_oid: Some("aaa".into()),
                        source_kind: "backfill".into(),
                        source_id: Some(format!("aaa:{symbol}")),
                    },
                )?;
                let wid =
                    witness_ledger::latest_witness_for_symbol(conn, "proj", file, Some(symbol))?
                        .unwrap()
                        .id;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id: wid,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: Some("deadbeef00".into()),
                        observed_head_oid: "deadbeef00".into(),
                    },
                )?;
                Ok(wid)
            })
            .unwrap()
    }

    fn e2e_fixture() -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        install_demote_verdict(&storage);

        let now = chrono::Utc::now().to_rfc3339();
        let mk =
            |id: &str, conv: &str, content: &str, seq: usize| crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "testproj".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            };
        // Cosine similarities against query [1,0,0,0]:
        // stale 1.0 > valid-1 ~0.98 > valid-2 ~0.95; fts-only ~0.0
        // (semantic miss — reachable only via the keyword fallback).
        let rows: [(crate::import::ConversationChunk, Vec<f32>); 4] = [
            (
                mk("chunk-stale", "conv-demoted", "old_fn design discussion", 0),
                vec![1.0, 0.0, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-valid-1",
                    "conv-clean-1",
                    "first current topic notes",
                    1,
                ),
                vec![0.98, 0.199, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-valid-2",
                    "conv-clean-2",
                    "second current topic notes",
                    2,
                ),
                vec![0.95, 0.312, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-fts",
                    "conv-demoted-new",
                    "zebraquark keyword only claim",
                    3,
                ),
                vec![0.0, 0.0, 1.0, 0.0],
            ),
        ];
        let mut engine = SearchEngine::new(16);
        for (chunk, vec) in &rows {
            storage.insert_chunk(chunk, vec).unwrap();
            engine.insert_chunk(chunk.id.clone(), vec.clone());
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    /// Rank of `needle` within `haystack` (byte offset), panicking loudly if
    /// absent — used to assert result ORDER in rendered XML.
    fn pos_of(haystack: &str, needle: &str) -> usize {
        haystack
            .find(needle)
            .unwrap_or_else(|| panic!("expected '{needle}' in output:\n{haystack}"))
    }

    fn active_forgetting_e2e_fixture() -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        install_demote_verdict(&storage);

        let mk = |id: &str,
                  conv: &str,
                  timestamp: &str,
                  content: &str,
                  seq: usize|
         -> crate::import::ConversationChunk {
            crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "testproj".into(),
                timestamp: timestamp.into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            }
        };
        // Timestamps are relative to the real clock the production scoring
        // path reads (`Utc::now()`): a fixed calendar date here would let the
        // old/new decay curves cross over as wall time advances and silently
        // flip the ordering assertions below.
        let fmt_days_ago = |days: i64| {
            (chrono::Utc::now() - chrono::Duration::days(days))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        };
        let old_ts = fmt_days_ago(600);
        let new_ts = fmt_days_ago(5);
        let rows = [
            (
                mk(
                    "chunk-demoted-old",
                    "conv-demoted",
                    &old_ts,
                    "old stale old_fn claim",
                    0,
                ),
                vec![0.55, 0.835_164_67, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-demoted-new",
                    "conv-demoted-new",
                    &new_ts,
                    "recent stale old_fn claim",
                    1,
                ),
                vec![0.50, 0.866_025_4, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-valid-a",
                    "conv-valid-a",
                    "2099-01-01T00:00:00Z",
                    "valid alpha claim",
                    2,
                ),
                vec![0.49, 0.871_722_43, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-valid-b",
                    "conv-valid-b",
                    "2099-01-01T00:00:00Z",
                    "valid beta claim",
                    3,
                ),
                vec![0.48, 0.877_268_5, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-fts-valid",
                    "conv-fts-valid",
                    "2099-01-01T00:00:00Z",
                    "zebraquark fallback-only valid claim",
                    4,
                ),
                vec![0.0, 0.0, 1.0, 0.0],
            ),
        ];

        let mut engine = SearchEngine::new(16);
        for (chunk, vector) in &rows {
            storage.insert_chunk(chunk, vector).unwrap();
            engine.insert_chunk(chunk.id.clone(), vector.clone());
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    #[tokio::test]
    async fn e2e_fts_validity_failure_keeps_unconditional_keyword_hit() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let now = chrono::Utc::now();
        let semantic = crate::import::ConversationChunk {
            id: "chunk-semantic".into(),
            conversation_id: "conv-semantic".into(),
            project_name: "testproj".into(),
            timestamp: (now - chrono::Duration::days(300)).to_rfc3339(),
            content: "semantic candidate above the fallback threshold".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let keyword = crate::import::ConversationChunk {
            id: "chunk-fts-failure".into(),
            conversation_id: "conv-fts-failure".into(),
            project_name: "testproj".into(),
            timestamp: now.to_rfc3339(),
            content: "zebraquark fallback candidate".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 1,
            is_sidechain: false,
        };
        let semantic_vec = vec![0.7, 0.714_142_86, 0.0, 0.0];
        let keyword_vec = vec![0.0, 0.0, 1.0, 0.0];
        storage.insert_chunk(&semantic, &semantic_vec).unwrap();
        storage.insert_chunk(&keyword, &keyword_vec).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES ('conv-semantic', 'shipped', 'v1.0.0', 100, '/repo', ?1)",
                    [now.to_rfc3339()],
                )?;
                // Only the FTS conversation selects this row. SQLite accepts
                // the malformed integer dynamically; NodeRow decoding then
                // supplies a real read error for the second validity batch.
                conn.execute(
                    "INSERT INTO code_nodes
                     (id, file, kind, name, span_start, span_end,
                      first_conv_id, last_conv_id)
                     VALUES ('bad-fts-node', '/repo/src/bad.rs', 'function',
                             'bad', 'not-an-integer', 1,
                             'conv-fts-failure', 'conv-fts-failure')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let mut engine = SearchEngine::new(16);
        engine.insert_chunk(semantic.id.clone(), semantic_vec);
        engine.insert_chunk(keyword.id.clone(), keyword_vec);
        let search = Arc::new(RwLock::new(engine));

        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "zebraquark",
            3,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
        )
        .await
        .unwrap();

        assert!(
            out.contains("<id>chunk-semantic</id>"),
            "the semantic hit must survive the fail-open validity batch:\n{out}"
        );
        assert!(
            out.contains("<id>chunk-fts-failure</id>"),
            "always-on hybrid search must keep the FTS hit even when its validity batch fails open:\n{out}"
        );
    }

    async fn render_fts_candidate(content: &str, ancestry_releases: Option<u32>) -> String {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let now = chrono::Utc::now();
        let chunk = crate::import::ConversationChunk {
            id: "chunk-fts-scaffold".into(),
            conversation_id: "conv-fts-scaffold".into(),
            project_name: "testproj".into(),
            timestamp: (now - chrono::Duration::days(30)).to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let vector = vec![0.0, 1.0];
        storage.insert_chunk(&chunk, &vector).unwrap();
        if let Some(releases) = ancestry_releases {
            storage
                .with_connection(|conn| {
                    conn.execute(
                        "INSERT INTO conversation_ancestry_cache
                         (conversation_id, state, release_tag, releases_behind,
                          repository, refreshed_at)
                         VALUES ('conv-fts-scaffold', 'shipped', 'v1.0.0', ?1,
                                 '/repo', ?2)",
                        rusqlite::params![releases, now.to_rfc3339()],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let mut engine = SearchEngine::new(8);
        engine.insert_chunk(chunk.id.clone(), vector);

        reflect_on_past_with_vec(
            &storage,
            &Arc::new(RwLock::new(engine)),
            &[1.0, 0.0],
            "zebraquark",
            1,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn fts_scaffold_suppresses_ancestry_and_neutral_paths_are_bit_identical() {
        let scaffold = "<command-message>zebraquark quoted workflow</command-message>";
        let pre_change = render_fts_candidate(scaffold, None).await;
        let current_release = render_fts_candidate(scaffold, Some(0)).await;
        let shipped = render_fts_candidate(scaffold, Some(5)).await;

        assert_eq!(
            rendered_result(&current_release, "chunk-fts-scaffold"),
            rendered_result(&pre_change, "chunk-fts-scaffold")
        );
        assert_eq!(
            rendered_result(&shipped, "chunk-fts-scaffold"),
            rendered_result(&pre_change, "chunk-fts-scaffold")
        );

        let organic = "zebraquark organic conversation";
        let organic_pre_change = render_fts_candidate(organic, None).await;
        let organic_current = render_fts_candidate(organic, Some(0)).await;
        assert_eq!(
            rendered_result(&organic_current, "chunk-fts-scaffold"),
            rendered_result(&organic_pre_change, "chunk-fts-scaffold")
        );
    }

    fn rendered_result<'a>(output: &'a str, id: &str) -> &'a str {
        let id_marker = format!("<id>{id}</id>");
        let id_pos = output
            .find(&id_marker)
            .unwrap_or_else(|| panic!("expected {id_marker} in output:\n{output}"));
        let start = output[..id_pos]
            .rfind("    <r rank=")
            .expect("result opening tag before id");
        let relative_end = output[id_pos..]
            .find("    </r>\n")
            .expect("result closing tag after id");
        let end = id_pos + relative_end + "    </r>\n".len();
        &output[start..end]
    }

    #[tokio::test]
    async fn e2e_active_forgetting_reorders_only_demoted_section_and_preserves_fallback_set() {
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let (off_storage, off_search) = active_forgetting_e2e_fixture();
        let off = reflect_on_past_with_vec(
            &off_storage,
            &off_search,
            &q,
            "zebraquark",
            5,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
        )
        .await
        .unwrap();

        let off_ids = [
            "chunk-valid-a",
            "chunk-fts-valid",
            "chunk-valid-b",
            "chunk-demoted-old",
            "chunk-demoted-new",
        ];
        assert!(off_ids.windows(2).all(|pair| {
            pos_of(&off, &format!("<id>{}</id>", pair[0]))
                < pos_of(&off, &format!("<id>{}</id>", pair[1]))
        }));

        let (on_storage, on_search) = active_forgetting_e2e_fixture();
        let on = reflect_on_past_with_vec(
            &on_storage,
            &on_search,
            &q,
            "zebraquark",
            5,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            true,
        )
        .await
        .unwrap();

        assert!(
            pos_of(&on, "<id>chunk-valid-a</id>") < pos_of(&on, "<id>chunk-fts-valid</id>")
                && pos_of(&on, "<id>chunk-fts-valid</id>") < pos_of(&on, "<id>chunk-valid-b</id>")
                && pos_of(&on, "<id>chunk-valid-b</id>")
                    < pos_of(&on, "<id>chunk-demoted-new</id>")
                && pos_of(&on, "<id>chunk-demoted-new</id>")
                    < pos_of(&on, "<id>chunk-demoted-old</id>"),
            "active forgetting must reorder only the demoted section by accelerated score:\n{on}"
        );
        for id in ["chunk-fts-valid", "chunk-valid-a", "chunk-valid-b"] {
            assert_eq!(
                rendered_result(&on, id),
                rendered_result(&off, id),
                "valid result {id} must remain byte-identical"
            );
        }
    }

    #[tokio::test]
    async fn e2e_active_forgetting_pagination_is_disjoint_exhaustive_and_repeatable() {
        let (storage, search) = active_forgetting_e2e_fixture();
        let q = [1.0f32, 0.0, 0.0, 0.0];

        // The initial page ends inside the demoted section. Its accelerated
        // order is [new, old], so get_more must continue with old rather than
        // repeat new from the raw-score order [old, new].
        let page1 = reflect_on_past_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            3,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            true,
        )
        .await
        .unwrap();
        let page2 = get_more_results_with_vec_active(
            &storage,
            &search,
            &q,
            "topic",
            3,
            1,
            0.1,
            Some("all"),
            ConsumptionMode::Full,
            true,
        )
        .await
        .unwrap();

        let expected = [
            ("chunk-valid-a", "valid alpha claim"),
            ("chunk-valid-b", "valid beta claim"),
            ("chunk-demoted-new", "recent stale old_fn claim"),
            ("chunk-demoted-old", "old stale old_fn claim"),
        ];
        assert!(expected[..3].windows(2).all(|pair| {
            pos_of(&page1, &format!("<id>{}</id>", pair[0].0))
                < pos_of(&page1, &format!("<id>{}</id>", pair[1].0))
        }));
        assert!(
            page2.contains("old stale old_fn claim"),
            "get_more must continue the initial active-forgetting order:\n{page2}"
        );

        for (id, marker) in &expected[..3] {
            assert!(
                page1.contains(&format!("<id>{id}</id>")) && !page2.contains(marker),
                "{id} must appear only on page 1"
            );
        }
        assert!(
            !page1.contains("<id>chunk-demoted-old</id>")
                && page2.contains("old stale old_fn claim"),
            "chunk-demoted-old must appear only on page 2"
        );
        assert_eq!(
            expected
                .iter()
                .filter(|(id, marker)| {
                    page1.contains(&format!("<id>{id}</id>")) || page2.contains(marker)
                })
                .count(),
            expected.len(),
            "pages must exhaust the active-forgetting candidate set"
        );

        let demoted_page = get_more_results_with_vec_active(
            &storage,
            &search,
            &q,
            "topic",
            2,
            2,
            0.1,
            Some("all"),
            ConsumptionMode::Full,
            true,
        )
        .await
        .unwrap();
        assert!(
            pos_of(&demoted_page, "recent stale old_fn claim")
                < pos_of(&demoted_page, "old stale old_fn claim"),
            "get_more must preserve accelerated ordering within the demoted section:\n{demoted_page}"
        );
        let demoted_page_again = get_more_results_with_vec_active(
            &storage,
            &search,
            &q,
            "topic",
            2,
            2,
            0.1,
            Some("all"),
            ConsumptionMode::Full,
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            demoted_page, demoted_page_again,
            "repeated get_more requests must return identical ordering"
        );
    }

    #[tokio::test]
    async fn get_more_continues_the_family_boosted_order_not_raw_hnsw() {
        // The certified live failure: scope=all with a current project. A
        // family chunk at raw 0.89 boosts to ~1.02 and wins page one over an
        // other-project chunk at raw 0.90. Pagination that reconstructs RAW
        // order sees [other, family], so offset=1 returns the family chunk
        // AGAIN and the other chunk is skipped forever.
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mk = |id: &str, project: &str, content: &str| crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: project.into(),
            timestamp: "2099-01-01T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let rows = [
            (
                mk(
                    "chunk-family",
                    "alpha-proj",
                    "family claim about pagination",
                ),
                vec![0.89f32, 0.455_960_5, 0.0, 0.0],
            ),
            (
                mk(
                    "chunk-other",
                    "beta-proj",
                    "other-project claim about pagination",
                ),
                vec![0.90f32, 0.435_889_9, 0.0, 0.0],
            ),
        ];
        let mut engine = SearchEngine::new(16);
        for (chunk, vector) in &rows {
            storage.insert_chunk(chunk, vector).unwrap();
            engine.insert_chunk(chunk.id.clone(), vector.clone());
        }
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("alpha-proj"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];

        let page1 = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            "pagination",
            1,
            0.1,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(
            page1.contains("family claim about pagination"),
            "boosted family chunk must win page one:\n{page1}"
        );
        assert!(
            !page1.contains("other-project claim about pagination"),
            "page one has room for exactly one result:\n{page1}"
        );

        let page2 = get_more_results_in_scope(
            &storage,
            &search,
            &q,
            "pagination",
            1,
            1,
            0.1,
            &scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(
            page2.contains("other-project claim about pagination"),
            "offset=1 must surface the chunk page one displaced, not skip it:\n{page2}"
        );
        assert!(
            !page2.contains("family claim about pagination"),
            "the boosted family chunk must not be duplicated onto page two:\n{page2}"
        );
    }

    #[tokio::test]
    async fn e2e_partition_through_real_reflect_and_get_more_paths() {
        let (storage, search) = e2e_fixture();
        let q = [1.0f32, 0.0, 0.0, 0.0];

        // 1) Demoted chunk sinks below BOTH valid ones and carries the exact
        //    annotation + footer, on the real reflect path.
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            3,
            0.3,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
        )
        .await
        .unwrap();
        let p_v1 = pos_of(&out, "<id>chunk-valid-1</id>");
        let p_v2 = pos_of(&out, "<id>chunk-valid-2</id>");
        let p_stale = pos_of(&out, "<id>chunk-stale</id>");
        assert!(
            p_v1 < p_v2 && p_v2 < p_stale,
            "demoted chunk must sink below every valid one:\n{out}"
        );
        assert!(
            out.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
            "exact annotation string must be present:\n{out}"
        );
        assert!(
            out.contains("bound code anchor no longer current (dream verdict)"),
            "dream-verdict footer must fire for a flagged demotion:\n{out}"
        );

        // 2) OVERFETCH: at limit=2 the valid N+1 candidate (valid-2, never
        //    fetched under the old exactly-limit fetch) takes the slot the
        //    demoted top hit vacated.
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            2,
            0.3,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
        )
        .await
        .unwrap();
        assert!(
            out.contains("<id>chunk-valid-2</id>"),
            "overfetch must surface the valid N+1 candidate:\n{out}"
        );
        assert!(
            !out.contains("<id>chunk-stale</id>"),
            "demoted chunk must not occupy a page slot valid candidates can fill:\n{out}"
        );

        // 3) KILL SWITCH: in vector ablation, partition off restores pure
        //    semantic score order — the demoted chunk leads again, no
        //    annotation anywhere.
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &q,
            "topic",
            2,
            0.3,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Vector,
            RecallRerankMode::Runtime,
            true,
        )
        .await
        .unwrap();
        let p_stale = pos_of(&out, "<id>chunk-stale</id>");
        let p_v1 = pos_of(&out, "<id>chunk-valid-1</id>");
        assert!(
            p_stale < p_v1,
            "kill switch must restore pre-partition order:\n{out}"
        );
        assert!(
            !out.contains("[stale anchor]"),
            "no annotation with switch off:\n{out}"
        );
        assert!(
            !out.contains("dream verdict"),
            "no footer with switch off:\n{out}"
        );

        // 4) get_more page 2 respects the partition: full-set partition
        //    yields [valid-1, valid-2, stale], so page 2 (offset 2) is the
        //    demoted chunk, annotated — and page 1 is demotion-free.
        let page1 = get_more_results_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            0,
            2,
            0.3,
            Some("all"),
            ConsumptionMode::Full,
        )
        .await
        .unwrap();
        assert!(page1.contains("first current topic notes"), "got: {page1}");
        assert!(page1.contains("second current topic notes"), "got: {page1}");
        assert!(
            !page1.contains("old_fn design discussion"),
            "page 1 must not contain the demoted chunk:\n{page1}"
        );
        let page2 = get_more_results_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            2,
            2,
            0.3,
            Some("all"),
            ConsumptionMode::Full,
        )
        .await
        .unwrap();
        assert!(
            page2.contains("old_fn design discussion"),
            "page 2 must carry the demoted chunk after the partition:\n{page2}"
        );
        assert!(
            page2.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
            "page 2 must carry the annotation:\n{page2}"
        );
        // Pagination stability (offset-independent window): no chunk may be
        // duplicated across pages 1+2 and none skipped — every fixture
        // conversation appears on exactly one page — and re-requesting page
        // 2 must reproduce it byte-for-byte (same query + same data → same
        // global order).
        assert!(
            !page2.contains("first current topic notes")
                && !page2.contains("second current topic notes"),
            "page 2 must not duplicate page 1 results:\n{page2}"
        );
        for marker in [
            "first current topic notes",
            "second current topic notes",
            "old_fn design discussion",
        ] {
            let on_p1 = page1.contains(marker);
            let on_p2 = page2.contains(marker);
            assert!(
                on_p1 ^ on_p2,
                "'{marker}' must appear on exactly one page (p1: {on_p1}, p2: {on_p2})"
            );
        }
        let page2_again = get_more_results_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            2,
            2,
            0.3,
            Some("all"),
            ConsumptionMode::Full,
        )
        .await
        .unwrap();
        assert_eq!(
            page2, page2_again,
            "re-requested page 2 must be identical (offset-independent window)"
        );

        // 5) FTS-append: a query that misses semantically (orthogonal
        //    vector) but hits the FTS row of a demoted conversation must
        //    still annotate — validity is resolved AFTER the candidate set
        //    is final (the appended conversation ids are merged in).
        let q_miss = [0.0f32, 0.0, 0.0, 1.0];
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &q_miss,
            "zebraquark",
            3,
            0.3,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
        )
        .await
        .unwrap();
        assert!(
            out.contains("[keyword] zebraquark keyword only claim"),
            "FTS fallback must surface the keyword chunk:\n{out}"
        );
        assert!(
            out.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
            "FTS-appended chunk from a demoted conversation must be annotated:\n{out}"
        );
    }

    #[tokio::test]
    async fn e2e_adaptive_refetch_backfills_valid_chunks_beyond_first_window() {
        // 8 demoted chunks outrank 2 valid ones. At limit=2 the first
        // window is overfetch(2) = 6 — ALL demoted, and full (more
        // candidates exist) — so the single adaptive refetch at 10*limit
        // must surface the valid pair from beyond the first window.
        let storage = Arc::new(Storage::open_memory().unwrap());

        let now = chrono::Utc::now().to_rfc3339();
        let mk =
            |id: &str, conv: &str, content: &str, seq: usize| crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "testproj".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            };
        let mut engine = SearchEngine::new(32);
        for i in 0..8 {
            let symbol = format!("old_fn_{i}");
            let conversation_id = format!("conv-demoted-{i}");
            let file = format!("/repo/src/stale_{i}.rs");
            install_demote_verdict_for(
                &storage,
                &file,
                &symbol,
                &conversation_id,
                &conversation_id,
            );
            let chunk = mk(
                &format!("chunk-demoted-{i}"),
                &conversation_id,
                &format!("stale claim number {i}"),
                i,
            );
            // cos ≈ 1.0 against [1,0,0,0] — every demoted chunk outranks
            // both valid ones.
            let vec = vec![1.0, 0.001 * (i + 1) as f32, 0.0, 0.0];
            storage.insert_chunk(&chunk, &vec).unwrap();
            engine.insert_chunk(chunk.id.clone(), vec);
        }
        for (id, conv, content, v, seq) in [
            (
                "chunk-valid-a",
                "conv-live-a",
                "live topic alpha notes",
                vec![0.6, 0.8, 0.0, 0.0], // cos 0.6
                8,
            ),
            (
                "chunk-valid-b",
                "conv-live-b",
                "live topic beta notes",
                vec![0.5, 0.866, 0.0, 0.0], // cos ~0.5
                9,
            ),
        ] {
            let chunk = mk(id, conv, content, seq);
            storage.insert_chunk(&chunk, &v).unwrap();
            engine.insert_chunk(chunk.id.clone(), v);
        }
        let search = Arc::new(RwLock::new(engine));
        let q = [1.0f32, 0.0, 0.0, 0.0];

        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &q,
            "topic",
            2,
            0.3,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
        )
        .await
        .unwrap();
        assert!(
            out.contains("<id>chunk-valid-a</id>") && out.contains("<id>chunk-valid-b</id>"),
            "adaptive refetch must backfill valid chunks from beyond the all-demoted first window:\n{out}"
        );
        assert!(
            !out.contains("<id>chunk-demoted-"),
            "no demoted chunk may occupy a slot the refetched valid pair can fill:\n{out}"
        );
    }

    // --- exact conv-tag lookup (episode-index handle fix) ---
    // Live failure 2026-07-08: csr_reflect_on_past("conv_0b68eace-…") went
    // through semantic embedding and returned unrelated cross-project noise.
    // A conv handle must resolve by tag, never by embedding.

    #[test]
    fn conv_handle_extracts_uuid() {
        assert_eq!(
            extract_conv_id("conv_0b68eace-3e21-4841-9567-58d038cd89d7"),
            Some("0b68eace-3e21-4841-9567-58d038cd89d7")
        );
    }

    #[test]
    fn bare_uuid_extracts() {
        assert_eq!(
            extract_conv_id("0b68eace-3e21-4841-9567-58d038cd89d7"),
            Some("0b68eace-3e21-4841-9567-58d038cd89d7")
        );
    }

    #[test]
    fn conv_handle_inside_sentence_extracts() {
        assert_eq!(
            extract_conv_id("full state: conv_0b68eace-3e21-4841-9567-58d038cd89d7 please"),
            Some("0b68eace-3e21-4841-9567-58d038cd89d7")
        );
    }

    #[test]
    fn plain_text_does_not_extract() {
        assert_eq!(extract_conv_id("that docker issue we fixed"), None);
        assert_eq!(extract_conv_id("convention for naming conversations"), None);
        assert_eq!(extract_conv_id("conv_not-a-real-uuid"), None);
    }

    // --- csr_why recency tie-break (D2) ---

    fn why_item(
        chunk_id: &str,
        conv_id: &str,
        score: f32,
        timestamp: &str,
    ) -> crate::search::reinstatement::EvidenceItem {
        crate::search::reinstatement::EvidenceItem {
            chunk_id: chunk_id.into(),
            conversation_id: conv_id.into(),
            score,
            best_route: crate::search::reinstatement::Via::Seed,
            routes: std::collections::BTreeSet::from([crate::search::reinstatement::Via::Seed]),
            route_scores: std::collections::BTreeMap::from([(
                crate::search::reinstatement::Via::Seed,
                score,
            )]),
            timestamp: timestamp.into(),
            excerpt: "excerpt".into(),
            ratification: None,
            trust: crate::provenance::TrustTier::Unknown,
        }
    }

    #[tokio::test]
    async fn why_rendered_receipt_resolves_family_symbol_and_preserves_all_routes() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "why does `retire_missing_nodes` preserve attribution";
        let query_vec = embeddings.embed_single(query).unwrap();
        let base_project = "claude-self-reflect";
        let alias_project = "claude-self-reflect-csr-engine";

        let chunks = [
            crate::import::ConversationChunk {
                id: "why-base-chunk".into(),
                conversation_id: "why-base-conv".into(),
                project_name: base_project.into(),
                timestamp: "2026-08-01T00:00:00Z".into(),
                content: "general provenance discussion".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            },
            crate::import::ConversationChunk {
                id: "why-symbol-chunk".into(),
                conversation_id: "why-symbol-conv".into(),
                project_name: alias_project.into(),
                timestamp: "2026-08-02T00:00:00Z".into(),
                content: "retire_missing_nodes preserves attribution provenance".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            },
        ];
        let mut engine = SearchEngine::new(query_vec.len());
        for chunk in &chunks {
            storage.insert_chunk(chunk, &query_vec).unwrap();
            engine.insert_chunk(chunk.id.clone(), query_vec.clone());
        }
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO code_nodes
                     (id, repo, project, file, lang, kind, name, fqname,
                      first_conv_id, last_conv_id, last_session_id)
                     VALUES (?1, 'repo', ?2, 'src/search.rs', 'rust', 'function',
                             'retire_missing_nodes',
                             'search::retire_missing_nodes',
                             'legacy-wrong-conv', 'legacy-wrong-conv', 'legacy-wrong-conv')",
                    rusqlite::params!["why-symbol-node", alias_project],
                )?;
                conn.execute(
                    "INSERT INTO code_node_attribution
                     (node_id, channel, source_id, observed_ts, evidence)
                     VALUES (?1, 'transcript', ?2, '2026-08-02T00:00:00Z', 'coedit_event')",
                    rusqlite::params!["why-symbol-node", "why-symbol-conv"],
                )?;
                Ok(())
            })
            .unwrap();

        let rendered = why(
            &storage,
            &embeddings,
            &Arc::new(RwLock::new(engine)),
            query,
            Some(base_project),
            &crate::search::reinstatement::ReinstateConfig::default(),
        )
        .await
        .unwrap();

        assert!(
            rendered.contains("scope=claude-self-reflect,claude-self-reflect-csr-engine"),
            "family scope must be visible in the receipt:\n{rendered}"
        );
        assert!(
            rendered.contains("symbols matched=1 [retire_missing_nodes]"),
            "the exact query identifier must resolve across the family:\n{rendered}"
        );
        assert!(rendered.contains("conv_why-symbol-conv:"), "{rendered}");
        assert!(
            rendered.contains("reached_by=seed+blend+graph"),
            "dedup must preserve both routes to the same evidence:\n{rendered}"
        );
        assert!(
            rendered.contains("graph walks=1 accepted=1 surfaced=1"),
            "walked, accepted, and surfaced must be separate rendered counters:\n{rendered}"
        );
    }

    /// W3-S finding 2: `csr_search_insights` used to filter on an exact
    /// project-label match (`get_chunk_ids_for_project`), unlike
    /// `csr_reflect_on_past`/`csr_quick_check` which widen to the project
    /// family. A session under `claude-self-reflect` would silently lose its
    /// `claude-self-reflect-csr-engine` chunks here while keeping them in
    /// `csr_reflect_on_past`. Tempdir projects root (only the base project
    /// dir present) keeps alias resolution off this machine's real `$HOME`.
    /// Revert `family_chunk_ids_at_root` back to an exact-match id set and
    /// this fails: `total_matches` drops from 2 to 1.
    #[tokio::test]
    async fn search_insights_widens_to_the_project_family() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "codex blocked revival deploy blocker";
        let query_vec = embeddings.embed_single(query).unwrap();

        let projects_root = tempfile::tempdir().unwrap();
        let base_project = "acme-base";
        let alias_project = "acme-base-csr-engine";
        std::fs::create_dir_all(projects_root.path().join(base_project)).unwrap();
        // alias_project's directory is deliberately absent: that's what makes
        // it an alias rather than a genuine sibling repo.

        let rows = [
            ("base-chunk", base_project),
            ("alias-chunk", alias_project),
            ("unrelated-chunk", "unrelated-project"),
        ];
        let mut engine = SearchEngine::new(query_vec.len());
        for (id, project) in rows {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: format!("conv-{id}"),
                project_name: project.into(),
                timestamp: "2026-08-01T00:00:00Z".into(),
                content: "codex blocked revival deploy blocker".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &query_vec).unwrap();
            engine.insert_chunk(chunk.id, query_vec.clone());
        }
        let search = Arc::new(RwLock::new(engine));

        let xml = search_insights_at_root(
            &storage,
            &embeddings,
            &search,
            query,
            Some(base_project),
            Some(projects_root.path()),
        )
        .await
        .unwrap();

        assert!(
            xml.contains("<total_matches>2</total_matches>"),
            "expected the base project plus its alias, not the unrelated project:\n{xml}"
        );
    }

    /// Same finding as `search_insights_widens_to_the_project_family`, for
    /// `csr_search_by_concept`.
    #[tokio::test]
    async fn search_by_concept_widens_to_the_project_family() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "codex blocked revival deploy blocker";
        let query_vec = embeddings.embed_single(query).unwrap();

        let projects_root = tempfile::tempdir().unwrap();
        let base_project = "acme-base";
        let alias_project = "acme-base-csr-engine";
        std::fs::create_dir_all(projects_root.path().join(base_project)).unwrap();

        let rows = [
            ("base-chunk", base_project),
            ("alias-chunk", alias_project),
            ("unrelated-chunk", "unrelated-project"),
        ];
        let mut engine = SearchEngine::new(query_vec.len());
        for (id, project) in rows {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: format!("conv-{id}"),
                project_name: project.into(),
                timestamp: "2026-08-01T00:00:00Z".into(),
                content: "codex blocked revival deploy blocker".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &query_vec).unwrap();
            engine.insert_chunk(chunk.id, query_vec.clone());
        }
        let search = Arc::new(RwLock::new(engine));

        let xml = search_by_concept_at_root(
            &storage,
            &embeddings,
            &search,
            query,
            10,
            Some(base_project),
            Some(projects_root.path()),
        )
        .await
        .unwrap();

        assert!(
            xml.contains("alias-chunk"),
            "family alias chunk missing from search_by_concept output:\n{xml}"
        );
        assert!(
            !xml.contains("unrelated-chunk"),
            "unrelated-project chunk leaked into search_by_concept output:\n{xml}"
        );
    }

    /// W3-S finding 2: the code-graph ledger (`code_nodes`/`code_evolution`)
    /// filters by an EXACT `project` match with no family-alias widening.
    /// Before `project` defaulting existed, an omitted project meant `""`
    /// (fully unscoped) — every project's nodes were visible. If an omitted
    /// project were instead defaulted to a specific project name the way the
    /// chunk-search tools now default (`resolve_search_scope`), this ledger
    /// would silently narrow to just that literal name and drop every node
    /// filed under a family alias. `search_by_file` deliberately does not
    /// default an omitted/`"all"` project at all — it stays `""` — so this
    /// pins that invariant: a node filed under an entirely different project
    /// than any plausible default is still visible when `project` is
    /// omitted.
    #[tokio::test]
    async fn search_by_file_omitted_project_is_not_narrowed_to_a_default_scope() {
        use crate::storage::codegraph::NodeRow;

        let storage = Arc::new(Storage::open_memory().unwrap());
        // Neither "claude-self-reflect" nor any of its aliases: stands in
        // for "whatever project a CLAUDE_PROJECT_DIR default-scope
        // resolution would have produced, which must not gate this ledger".
        let stored_project = "claude-self-reflect-csr-engine";
        storage
            .upsert_code_node(&NodeRow {
                id: "node-correlate-fts-hit".into(),
                project: stored_project.into(),
                file: "src/import/plans.rs".into(),
                kind: "function".into(),
                name: "correlate_fts_hit".into(),
                ..NodeRow::default()
            })
            .unwrap();

        let xml = search_by_file(&storage, "src/import/plans.rs", 10, None)
            .await
            .unwrap();
        assert!(
            xml.contains("correlate_fts_hit"),
            "omitted project must not be narrowed to a default scope that \
             can never match a family alias's exact project name:\n{xml}"
        );

        // "all" must behave identically to omitted.
        let xml_all = search_by_file(&storage, "src/import/plans.rs", 10, Some("all"))
            .await
            .unwrap();
        assert!(xml_all.contains("correlate_fts_hit"), "{xml_all}");

        // An explicit, unrelated project still narrows exactly, unaffected.
        let xml_explicit = search_by_file(
            &storage,
            "src/import/plans.rs",
            10,
            Some("some-other-project"),
        )
        .await
        .unwrap();
        assert!(
            !xml_explicit.contains("correlate_fts_hit"),
            "an explicit, non-matching project must still narrow the ledger:\n{xml_explicit}"
        );
    }

    #[tokio::test]
    async fn why_rendered_receipt_reports_dangling_episode_without_a_fake_hop() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "why provenance episodes can abstain safely";
        let query_vec = embeddings.embed_single(query).unwrap();
        let chunk = crate::import::ConversationChunk {
            id: "episode-current-chunk".into(),
            conversation_id: "episode-current".into(),
            project_name: "claude-self-reflect".into(),
            timestamp: "2026-08-03T00:00:00Z".into(),
            content: "provenance episodes abstain safely".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &query_vec).unwrap();
        storage
            .insert_reflection(
                "episode-current-reflection",
                &serde_json::json!({ "prev_episode_id": "missing-episode-target" }).to_string(),
                &[
                    "session_episode".to_string(),
                    "conv_episode-current".to_string(),
                ],
                &[],
            )
            .unwrap();
        let mut engine = SearchEngine::new(query_vec.len());
        engine.insert_chunk(chunk.id.clone(), query_vec);

        let rendered = why(
            &storage,
            &embeddings,
            &Arc::new(RwLock::new(engine)),
            query,
            Some("claude-self-reflect"),
            &crate::search::reinstatement::ReinstateConfig::default(),
        )
        .await
        .unwrap();

        assert!(
            rendered.contains(
                "episodes links=1 resolved=0 accepted=0 surfaced=0 (below-cut=0, below-threshold=0, dangling=1, out-of-scope=0, unembedded=0)"
            ),
            "dangling targets must remain an explicit abstention:\n{rendered}"
        );
        assert!(
            !rendered.contains("reached_by=episode"),
            "a dangling target must never fabricate episode evidence:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn why_rendered_receipt_selects_three_distinct_semantic_conversations() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "why semantic seeds remain distinct conversations";
        let query_vec = embeddings.embed_single(query).unwrap();
        let rows = [
            ("seed-a-1", "seed-conv-a"),
            ("seed-a-2", "seed-conv-a"),
            ("seed-b", "seed-conv-b"),
            ("seed-c", "seed-conv-c"),
        ];
        let mut engine = SearchEngine::new(query_vec.len());
        for (seq, (id, conv)) in rows.into_iter().enumerate() {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "claude-self-reflect".into(),
                timestamp: format!("2026-08-0{}T00:00:00Z", seq + 1),
                content: format!("semantic evidence from {conv}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &query_vec).unwrap();
            engine.insert_chunk(chunk.id, query_vec.clone());
        }

        let rendered = why(
            &storage,
            &embeddings,
            &Arc::new(RwLock::new(engine)),
            query,
            Some("claude-self-reflect"),
            &crate::search::reinstatement::ReinstateConfig::default(),
        )
        .await
        .unwrap();

        assert!(
            rendered.contains("seeds selected=3 conversations=3 surfaced=3"),
            "multiple chunks from one conversation must consume one seed slot:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn why_rendered_receipt_surfaces_resolved_episode_under_conditional_quota() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "why episode provenance remains reachable";
        let query_vec = embeddings.embed_single(query).unwrap();
        let mut orthogonal = vec![0.0; query_vec.len()];
        orthogonal[0] = -query_vec[1];
        orthogonal[1] = query_vec[0];
        let orthogonal_norm = orthogonal.iter().map(|v| v * v).sum::<f32>().sqrt();
        for value in &mut orthogonal {
            *value /= orthogonal_norm;
        }
        let episode_vec: Vec<f32> = query_vec
            .iter()
            .zip(orthogonal)
            .map(|(query_value, orthogonal_value)| {
                0.40 * query_value + (1.0_f32 - 0.40_f32.powi(2)).sqrt() * orthogonal_value
            })
            .collect();
        let chunks = [
            ("episode-seed-chunk", "episode-seed-conv", query_vec.clone()),
            ("episode-target-chunk", "episode-target-conv", episode_vec),
        ];
        let mut engine = SearchEngine::new(query_vec.len());
        for (seq, (id, conv, vector)) in chunks.into_iter().enumerate() {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "claude-self-reflect".into(),
                timestamp: format!("2026-08-0{}T00:00:00Z", seq + 1),
                content: format!("episode evidence from {conv}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(chunk.id, vector);
        }
        storage
            .insert_reflection(
                "episode-positive-reflection",
                &serde_json::json!({ "prev_episode_id": "episode-target-conv" }).to_string(),
                &[
                    "session_episode".to_string(),
                    "conv_episode-seed-conv".to_string(),
                ],
                &[],
            )
            .unwrap();
        let cfg = crate::search::reinstatement::ReinstateConfig {
            k: 1,
            ..crate::search::reinstatement::ReinstateConfig::default()
        };

        let rendered = why(
            &storage,
            &embeddings,
            &Arc::new(RwLock::new(engine)),
            query,
            Some("claude-self-reflect"),
            &cfg,
        )
        .await
        .unwrap();

        assert!(rendered.contains("conv_episode-target-conv:"), "{rendered}");
        assert!(
            rendered.contains("reached_by=blend+episode"),
            "the episode route must survive even when blend also reached the chunk:\n{rendered}"
        );
        assert!(
            rendered.contains(
                "episodes links=1 resolved=1 accepted=1 surfaced=1 (below-cut=0, below-threshold=0, dangling=0, out-of-scope=0, unembedded=0)"
            ),
            "a valid episode above the structural floor must receive the conditional slot:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn why_rendered_receipt_abstains_from_weak_episode_target() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "why weak episode evidence must abstain";
        let query_vec = embeddings.embed_single(query).unwrap();
        let mut orthogonal = vec![0.0; query_vec.len()];
        orthogonal[0] = -query_vec[1];
        orthogonal[1] = query_vec[0];
        let orthogonal_norm = orthogonal.iter().map(|v| v * v).sum::<f32>().sqrt();
        for value in &mut orthogonal {
            *value /= orthogonal_norm;
        }
        let weak_vec: Vec<f32> = query_vec
            .iter()
            .zip(orthogonal)
            .map(|(query_value, orthogonal_value)| {
                0.10 * query_value + (1.0_f32 - 0.10_f32.powi(2)).sqrt() * orthogonal_value
            })
            .collect();
        let chunks = [
            (
                "episode-weak-seed",
                "episode-weak-seed-conv",
                query_vec.clone(),
            ),
            ("episode-weak-target", "episode-weak-target-conv", weak_vec),
        ];
        let mut engine = SearchEngine::new(query_vec.len());
        for (seq, (id, conv, vector)) in chunks.into_iter().enumerate() {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "claude-self-reflect".into(),
                timestamp: format!("2026-08-0{}T00:00:00Z", seq + 1),
                content: format!("episode evidence from {conv}"),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(chunk.id, vector);
        }
        storage
            .insert_reflection(
                "episode-weak-reflection",
                &serde_json::json!({ "prev_episode_id": "episode-weak-target-conv" }).to_string(),
                &[
                    "session_episode".to_string(),
                    "conv_episode-weak-seed-conv".to_string(),
                ],
                &[],
            )
            .unwrap();

        let rendered = why(
            &storage,
            &embeddings,
            &Arc::new(RwLock::new(engine)),
            query,
            Some("claude-self-reflect"),
            &crate::search::reinstatement::ReinstateConfig::default(),
        )
        .await
        .unwrap();

        assert!(
            !rendered.contains("conv_episode-weak-target-conv:"),
            "a below-floor episode must abstain rather than surface:\n{rendered}"
        );
        assert!(
            rendered.contains("episodes links=1 resolved=1 accepted=0 surfaced=0"),
            "resolution and acceptance must be separate stages:\n{rendered}"
        );
        assert!(rendered.contains("below-threshold=1"), "{rendered}");
    }

    #[tokio::test]
    async fn why_rendered_receipt_appends_distilled_into_memory_line_on_origin_session_match() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "why memory provenance hop surfaces distilled memory";
        let query_vec = embeddings.embed_single(query).unwrap();
        let conversation_id = "why-mem-conv";
        let chunk = crate::import::ConversationChunk {
            id: "why-mem-chunk".into(),
            conversation_id: conversation_id.into(),
            project_name: "claude-self-reflect".into(),
            timestamp: "2026-08-19T00:00:00Z".into(),
            content: "memory provenance hop surfaces distilled memory".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let mut engine = SearchEngine::new(query_vec.len());
        storage.insert_chunk(&chunk, &query_vec).unwrap();
        engine.insert_chunk(chunk.id.clone(), query_vec.clone());

        storage
            .with_transaction(|tx| {
                crate::storage::queries::upsert_memory_registry_batch(
                    tx,
                    &[crate::storage::queries::MemoryRegistryRow {
                        file_path: "/mem/why-hop-memory.md".into(),
                        project: "proj-why-mem".into(),
                        slug: "why-hop-memory".into(),
                        description: Some("a description with\na newline and stuff".into()),
                        mem_type: Some("reference".into()),
                        origin_session_id: Some(conversation_id.into()),
                        modified_ts: Some("2026-08-19T00:00:00Z".into()),
                        file_mtime: 1_700_000_000,
                        content_hash: "hash-why-hop".into(),
                        links_json: "[]".into(),
                        last_seen_scan: 1,
                    }],
                )?;
                Ok(())
            })
            .unwrap();

        let rendered = why(
            &storage,
            &embeddings,
            &Arc::new(RwLock::new(engine)),
            query,
            Some("claude-self-reflect"),
            &crate::search::reinstatement::ReinstateConfig::default(),
        )
        .await
        .unwrap();

        let expected = "distilled into memory: why-hop-memory (reference, proj-why-mem) — a description with a newline and stuff";
        assert!(
            rendered.contains(expected),
            "matching origin_session_id must append a sanitized memory line:\n{rendered}"
        );
        assert!(
            rendered.find("distilled into memory").unwrap() > rendered.find("WHY:").unwrap(),
            "memory line must appear after the WHY header:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn why_rendered_receipt_omits_memory_line_when_no_origin_session_matches() {
        let storage_empty = Arc::new(Storage::open_memory().unwrap());
        let storage_unrelated = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let query = "why memory provenance hop stays silent without a match";
        let query_vec = embeddings.embed_single(query).unwrap();
        let conversation_id = "why-mem-nomatch-conv";
        let chunk = crate::import::ConversationChunk {
            id: "why-mem-nomatch-chunk".into(),
            conversation_id: conversation_id.into(),
            project_name: "claude-self-reflect".into(),
            timestamp: "2026-08-19T00:00:00Z".into(),
            content: "memory provenance hop stays silent without a match".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };

        let mut engine_empty = SearchEngine::new(query_vec.len());
        storage_empty.insert_chunk(&chunk, &query_vec).unwrap();
        engine_empty.insert_chunk(chunk.id.clone(), query_vec.clone());

        let mut engine_unrelated = SearchEngine::new(query_vec.len());
        storage_unrelated.insert_chunk(&chunk, &query_vec).unwrap();
        engine_unrelated.insert_chunk(chunk.id.clone(), query_vec.clone());
        storage_unrelated
            .with_transaction(|tx| {
                crate::storage::queries::upsert_memory_registry_batch(
                    tx,
                    &[crate::storage::queries::MemoryRegistryRow {
                        file_path: "/mem/unrelated.md".into(),
                        project: "proj-why-mem".into(),
                        slug: "unrelated-memory".into(),
                        description: Some("should not appear".into()),
                        mem_type: Some("reference".into()),
                        origin_session_id: Some("unrelated-session-id".into()),
                        modified_ts: Some("2026-08-19T00:00:00Z".into()),
                        file_mtime: 1_700_000_000,
                        content_hash: "hash-unrelated".into(),
                        links_json: "[]".into(),
                        last_seen_scan: 1,
                    }],
                )?;
                Ok(())
            })
            .unwrap();

        let cfg = crate::search::reinstatement::ReinstateConfig::default();
        let rendered_empty = why(
            &storage_empty,
            &embeddings,
            &Arc::new(RwLock::new(engine_empty)),
            query,
            Some("claude-self-reflect"),
            &cfg,
        )
        .await
        .unwrap();
        let rendered_unrelated = why(
            &storage_unrelated,
            &embeddings,
            &Arc::new(RwLock::new(engine_unrelated)),
            query,
            Some("claude-self-reflect"),
            &cfg,
        )
        .await
        .unwrap();

        assert!(
            !rendered_empty.contains("distilled into memory"),
            "zero memory_registry rows must leave csr_why output without a memory line:\n{rendered_empty}"
        );
        assert!(
            !rendered_unrelated.contains("distilled into memory"),
            "non-matching origin_session_id must leave csr_why output without a memory line:\n{rendered_unrelated}"
        );
        assert_eq!(
            rendered_empty, rendered_unrelated,
            "non-matching registry rows must keep output byte-identical to the empty-registry case"
        );
    }

    /// The chain that a pairwise near-tie comparator gets wrong. Scores 0.90 /
    /// 0.86 / 0.82 on one anchor: each adjacent pair is within epsilon but the
    /// ends are 0.08 apart, so a pairwise predicate says A>B, B>C and C>A — a
    /// cycle, which is not a valid total order and can make sort_by panic.
    /// Every input permutation must produce the SAME output and must not panic.
    #[test]
    fn why_recency_tiebreak_is_a_total_order_on_an_epsilon_chain() {
        let a = why_item("a", "conv-a", 0.90, "2026-01-01T00:00:00Z");
        let b = why_item("b", "conv-b", 0.86, "2026-01-02T00:00:00Z");
        let c = why_item("c", "conv-c", 0.82, "2026-01-03T00:00:00Z");
        let mut anchors = HashMap::new();
        for id in ["a", "b", "c"] {
            anchors.insert(id.to_string(), ("proj".to_string(), "F.md".to_string()));
        }

        let perms = [
            vec![a.clone(), b.clone(), c.clone()],
            vec![a.clone(), c.clone(), b.clone()],
            vec![b.clone(), a.clone(), c.clone()],
            vec![b.clone(), c.clone(), a.clone()],
            vec![c.clone(), a.clone(), b.clone()],
            vec![c.clone(), b.clone(), a.clone()],
        ];

        let mut orders = perms
            .into_iter()
            .map(|p| {
                apply_why_recency_tiebreak(p, &anchors)
                    .items
                    .iter()
                    .map(|i| i.chunk_id.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        orders.dedup();

        assert_eq!(
            orders.len(),
            1,
            "every permutation must yield one stable order, got {orders:?}"
        );
    }

    /// The tie-break is worthless if the renderer re-sorts it away. It used to:
    /// each conversation group was re-sorted by ASCENDING timestamp, so the
    /// oldest — i.e. the superseded claim — rendered above its own correction,
    /// and every unit test still passed because they only exercised the pure
    /// ranking function. Assert on the rendered string instead.
    #[test]
    fn format_why_renders_in_ranked_order_not_chronological() {
        let older_wrong = why_item("wrong", "conv-x", 0.840, "2026-01-01T00:00:00Z");
        let newer_fixed = why_item("fixed", "conv-x", 0.810, "2026-01-02T00:00:00Z");
        let mut anchors = HashMap::new();
        for id in ["wrong", "fixed"] {
            anchors.insert(
                id.to_string(),
                ("proj".to_string(), "CLAUDE.md".to_string()),
            );
        }
        let ranking = apply_why_recency_tiebreak(vec![older_wrong, newer_fixed], &anchors);
        assert_eq!(
            ranking.items[0].chunk_id, "fixed",
            "precondition: ranking puts the correction first"
        );

        let rendered = format_why(
            "why",
            &ranking.items,
            &ranking.hoisted_chunk_ids,
            &crate::search::reinstatement::ReinstateTrace::default(),
        );
        let newer_at = rendered.find("0.810").expect("newer item must render");
        let older_at = rendered.find("0.840").expect("older item must render");
        assert!(
            newer_at < older_at,
            "renderer must preserve ranked order; got:\n{rendered}"
        );
    }

    #[test]
    fn why_recency_tiebreak_prefers_newer_within_epsilon_same_anchor() {
        let older_wrong = why_item("wrong", "conv-x", 0.715, "2026-01-01T00:00:00Z");
        let newer_fixed = why_item("fixed", "conv-x", 0.696, "2026-01-02T00:00:00Z"); // 0.019 gap
        let mut anchors = HashMap::new();
        anchors.insert(
            "wrong".to_string(),
            ("proj".to_string(), "CLAUDE.md".to_string()),
        );
        anchors.insert(
            "fixed".to_string(),
            ("proj".to_string(), "CLAUDE.md".to_string()),
        );

        let ranking = apply_why_recency_tiebreak(vec![older_wrong, newer_fixed], &anchors);

        assert_eq!(
            ranking.items[0].chunk_id, "fixed",
            "newer correction must rank first within epsilon on the same anchor"
        );
        let rendered = format_why(
            "why",
            &ranking.items,
            &ranking.hoisted_chunk_ids,
            &crate::search::reinstatement::ReinstateTrace::default(),
        );
        assert!(
            rendered.contains("score=0.696 [recent↑]"),
            "the lower-scored item hoisted by recency must carry a marker:\n{rendered}"
        );
        assert!(
            rendered.find("score=0.696 [recent↑]").unwrap()
                < rendered.find("score=0.715 trust=").unwrap(),
            "the newer item must render before its higher-scored sibling:\n{rendered}"
        );
        assert!(
            rendered.contains("score=0.715 trust="),
            "the higher-scored sibling must not carry a marker:\n{rendered}"
        );
    }

    #[test]
    fn why_recency_tiebreak_leaves_order_when_scores_outside_epsilon() {
        let higher_older = why_item("higher", "conv-old", 0.90, "2026-01-01T00:00:00Z");
        let lower_newer = why_item("lower", "conv-new", 0.50, "2026-01-02T00:00:00Z"); // 0.40 gap, well outside 0.05
        let mut anchors = HashMap::new();
        anchors.insert(
            "higher".to_string(),
            ("proj".to_string(), "CLAUDE.md".to_string()),
        );
        anchors.insert(
            "lower".to_string(),
            ("proj".to_string(), "CLAUDE.md".to_string()),
        );

        let ranking = apply_why_recency_tiebreak(vec![higher_older, lower_newer], &anchors);

        assert_eq!(
            ranking.items[0].chunk_id, "higher",
            "outside epsilon must not be reordered by recency"
        );
        let rendered = format_why(
            "why",
            &ranking.items,
            &ranking.hoisted_chunk_ids,
            &crate::search::reinstatement::ReinstateTrace::default(),
        );
        assert!(
            !rendered.contains('↑'),
            "strict score ordering outside epsilon must not carry a marker:\n{rendered}"
        );
    }

    #[test]
    fn why_recency_tiebreak_leaves_order_across_different_anchors() {
        let older_a = why_item("a-old", "conv-a-old", 0.840, "2026-01-01T00:00:00Z");
        let newer_b = why_item("b-new", "conv-b-new", 0.820, "2026-01-02T00:00:00Z"); // within 0.05 but different file
        let mut anchors = HashMap::new();
        anchors.insert(
            "a-old".to_string(),
            ("proj".to_string(), "file_a.rs".to_string()),
        );
        anchors.insert(
            "b-new".to_string(),
            ("proj".to_string(), "file_b.rs".to_string()),
        );

        let ranking = apply_why_recency_tiebreak(vec![older_a, newer_b], &anchors);

        assert_eq!(
            ranking.items[0].chunk_id, "a-old",
            "different anchors must never be reordered by recency, even within epsilon"
        );
        let rendered = format_why(
            "why",
            &ranking.items,
            &ranking.hoisted_chunk_ids,
            &crate::search::reinstatement::ReinstateTrace::default(),
        );
        assert!(
            !rendered.contains('↑'),
            "items on different anchors must not carry a marker:\n{rendered}"
        );
    }

    // ─── adversarial review finding 5: substring session-id resolution
    // must not silently pick whichever file the filesystem enumerates
    // first when more than one distinct file matches ───

    fn write_session_file(
        projects_dir: &std::path::Path,
        project: &str,
        session_stem: &str,
    ) -> std::path::PathBuf {
        let proj_dir = projects_dir.join(project);
        std::fs::create_dir_all(&proj_dir).unwrap();
        let path = proj_dir.join(format!("{session_stem}.jsonl"));
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn exact_stem_match_wins_over_a_substring_match_in_another_project() {
        let dir = tempfile::tempdir().unwrap();
        // proj-a's session id is an EXACT match for the query; proj-b's
        // session id merely CONTAINS the query as a substring. The exact
        // match must win regardless of filesystem enumeration order.
        write_session_file(dir.path(), "proj-a", "abc123");
        write_session_file(dir.path(), "proj-b", "abc123-extra-suffix");

        match find_conversation_file(dir.path(), "abc123", None) {
            ConversationLookup::Found(path, project) => {
                assert_eq!(project, "proj-a");
                assert!(path.to_string_lossy().contains("proj-a"));
            }
            ConversationLookup::NotFound => panic!("expected a match"),
            ConversationLookup::Ambiguous(candidates) => {
                panic!("exact match must win outright, not report ambiguous: {candidates:?}")
            }
        }
    }

    #[test]
    fn ambiguous_substring_match_reports_every_candidate_instead_of_picking_one() {
        let dir = tempfile::tempdir().unwrap();
        // Neither file is an exact match for "abc123" — both merely
        // contain it. With no exact match to break the tie, this must be
        // reported ambiguous rather than resolved to whichever the
        // filesystem happened to enumerate first.
        write_session_file(dir.path(), "proj-a", "abc123-one");
        write_session_file(dir.path(), "proj-b", "abc123-two");

        match find_conversation_file(dir.path(), "abc123", None) {
            ConversationLookup::Ambiguous(mut candidates) => {
                candidates.sort();
                assert_eq!(candidates.len(), 2);
                assert_eq!(
                    candidates[0],
                    ("abc123-one".to_string(), "proj-a".to_string())
                );
                assert_eq!(
                    candidates[1],
                    ("abc123-two".to_string(), "proj-b".to_string())
                );
            }
            other => panic!(
                "expected Ambiguous with 2 candidates, got a resolved/not-found result instead: {}",
                matches!(other, ConversationLookup::Found(..))
            ),
        }
    }

    #[test]
    fn get_full_conversation_surfaces_ambiguous_candidates_instead_of_a_wrong_file() {
        let dir = tempfile::tempdir().unwrap();
        write_session_file(dir.path(), "proj-a", "abc123-one");
        write_session_file(dir.path(), "proj-b", "abc123-two");

        let out = get_full_conversation(dir.path(), "abc123", None);
        assert!(out.contains("is ambiguous"));
        assert!(out.contains("abc123-one"));
        assert!(out.contains("abc123-two"));
        assert!(out.contains("proj-a"));
        assert!(out.contains("proj-b"));
    }

    #[test]
    fn transcript_run_surfaces_ambiguous_session_id_honestly() {
        let dir = tempfile::tempdir().unwrap();
        write_session_file(dir.path(), "proj-a", "abc123-one");
        write_session_file(dir.path(), "proj-b", "abc123-two");

        let req = crate::transcript::TranscriptRequest {
            session: "abc123".to_string(),
            view: crate::transcript::ViewKind::Stats,
            ..Default::default()
        };
        let out = crate::transcript::run(dir.path(), &req);
        assert!(out.contains("ambiguous"));
        assert!(out.contains("abc123-one"));
        assert!(out.contains("abc123-two"));
    }
}

#[cfg(test)]
mod why_marker_tests {
    use super::*;

    fn evidence(
        chunk_id: &str,
        score: f32,
        timestamp: &str,
    ) -> crate::search::reinstatement::EvidenceItem {
        crate::search::reinstatement::EvidenceItem {
            chunk_id: chunk_id.to_string(),
            conversation_id: "conv-x".to_string(),
            score,
            best_route: crate::search::reinstatement::Via::Seed,
            routes: std::collections::BTreeSet::from([crate::search::reinstatement::Via::Seed]),
            route_scores: std::collections::BTreeMap::from([(
                crate::search::reinstatement::Via::Seed,
                score,
            )]),
            timestamp: timestamp.to_string(),
            excerpt: chunk_id.to_string(),
            ratification: None,
            trust: crate::provenance::TrustTier::Unknown,
        }
    }

    #[test]
    fn same_anchor_within_epsilon() {
        let higher = evidence("higher", 0.715, "2026-01-01T00:00:00Z");
        let newer = evidence("newer", 0.696, "2026-01-02T00:00:00Z");
        let anchors = HashMap::from([
            (
                "higher".to_string(),
                ("project".to_string(), "file.rs".to_string()),
            ),
            (
                "newer".to_string(),
                ("project".to_string(), "file.rs".to_string()),
            ),
        ]);

        let ranking = apply_why_recency_tiebreak(vec![higher, newer], &anchors);
        let rendered = format_why(
            "why",
            &ranking.items,
            &ranking.hoisted_chunk_ids,
            &crate::search::reinstatement::ReinstateTrace::default(),
        );

        assert_eq!(ranking.items[0].chunk_id, "newer");
        assert!(ranking.hoisted_chunk_ids.contains("newer"));
        assert!(rendered.contains("score=0.696 [recent↑]"));
        assert!(!ranking.hoisted_chunk_ids.contains("higher"));
    }

    #[test]
    fn items_outside_epsilon() {
        let higher = evidence("higher", 0.90, "2026-01-01T00:00:00Z");
        let newer = evidence("newer", 0.50, "2026-01-02T00:00:00Z");
        let anchors = HashMap::from([
            (
                "higher".to_string(),
                ("project".to_string(), "file.rs".to_string()),
            ),
            (
                "newer".to_string(),
                ("project".to_string(), "file.rs".to_string()),
            ),
        ]);

        let ranking = apply_why_recency_tiebreak(vec![newer, higher], &anchors);
        let rendered = format_why(
            "why",
            &ranking.items,
            &ranking.hoisted_chunk_ids,
            &crate::search::reinstatement::ReinstateTrace::default(),
        );

        assert_eq!(ranking.items[0].chunk_id, "higher");
        assert!(ranking.hoisted_chunk_ids.is_empty());
        assert!(!rendered.contains(WHY_RECENCY_HOIST_MARKER));
    }

    #[test]
    fn different_anchors() {
        let higher = evidence("higher", 0.715, "2026-01-01T00:00:00Z");
        let newer = evidence("newer", 0.696, "2026-01-02T00:00:00Z");
        let anchors = HashMap::from([
            (
                "higher".to_string(),
                ("project".to_string(), "file-a.rs".to_string()),
            ),
            (
                "newer".to_string(),
                ("project".to_string(), "file-b.rs".to_string()),
            ),
        ]);

        let ranking = apply_why_recency_tiebreak(vec![newer, higher], &anchors);
        let rendered = format_why(
            "why",
            &ranking.items,
            &ranking.hoisted_chunk_ids,
            &crate::search::reinstatement::ReinstateTrace::default(),
        );

        assert_eq!(ranking.items[0].chunk_id, "higher");
        assert!(ranking.hoisted_chunk_ids.is_empty());
        assert!(!rendered.contains(WHY_RECENCY_HOIST_MARKER));
    }
}
