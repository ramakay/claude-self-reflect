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
        KeywordTool::Quick => Ok(format::format_keyword_quick_check(&results)),
        KeywordTool::Insights => {
            results.truncate(limit);
            Ok(format::format_search_insights(&results, query))
        }
        KeywordTool::Recency { .. } => {
            let (start, end) = window.expect("recency window resolved before FTS");
            results.truncate(limit);
            let desc = format!("{} to {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"));
            Ok(format::format_recency_results(&results, query, &desc))
        }
        KeywordTool::Concept => {
            results.truncate(limit);
            let (_, label) = cross_project::normalize_project_scope(storage, project)?;
            Ok(format::format_search_results(&results, query, &label, 0, 0))
        }
        KeywordTool::More { offset } => {
            let total = results.len();
            let page: Vec<_> = results.into_iter().skip(offset).take(limit).collect();
            Ok(format::format_more_results(&page, query, offset, total))
        }
        KeywordTool::Why => {
            use crate::search::reinstatement::{EvidenceItem, ReinstateTrace, Via};
            let items: Vec<_> = results
                .into_iter()
                .take(limit)
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

/// The (at most two) rows `format_quick_check` judges: the hit it may show and
/// the runner-up its margin is read against.
fn quick_check_hits(
    mut enriched: Vec<EnrichedResult>,
    storage: &Storage,
    live: Option<&str>,
    query: &str,
) -> Vec<EnrichedResult> {
    let Some(live) = live else {
        enriched.truncate(2);
        return enriched;
    };
    // `format_quick_check` judges only its first row against the floor, so the
    // floor is applied here, before the row to show is picked: the best
    // qualifying row outside the live session, else the qualifying live one.
    // When nothing qualifies the order stays best-first, so the negative
    // answer reports the best score it rejected.
    let mut ordered = enriched.clone();
    sink_session_hits(&mut ordered, storage, Some(live), query);
    let Some(at) = ordered
        .iter()
        .position(|e| e.score >= format::QUICK_CHECK_FLOOR)
    else {
        enriched.truncate(2);
        return enriched;
    };
    let shown = ordered.remove(at);
    // The runner-up the margin reads against must not outscore the row shown.
    let runner_up = ordered.into_iter().find(|e| e.score <= shown.score);
    std::iter::once(shown).chain(runner_up).collect()
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

/// The probe's scope: explicit all-project, with no current-project boost and
/// no client-directory default, so nothing in the process environment (cwd,
/// `MCP_CLIENT_CWD`, `CLAUDE_PROJECT_DIR`) reaches the measurement.
fn health_probe_scope(storage: &Storage) -> Result<SearchProjectScope> {
    SearchProjectScope::resolve_with(storage, Some("all"), None, None, None)
}

/// The health probe's production search: limit 5, minimum score 0.3, explicit
/// all-project scope built without the ambient cwd or client-project boost (so
/// the daemon's cwd cannot move the measurement), the runtime reranker and the
/// production validity, forgetting and search-mode settings. Retrieval
/// telemetry is suppressed through the same `record_retrievals=false` seam the
/// curated eval uses. Returns the conversation ids of the actual final page, in
/// result order.
pub(crate) async fn reflect_for_health_probe_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    rerank_intent: &str,
) -> Result<Vec<String>> {
    const LIMIT: usize = 5;
    const MIN_SCORE: f32 = 0.3;

    let scope = health_probe_scope(storage)?;
    let search_mode = search_mode_from(std::env::var("CSR_SEARCH_MODE").ok().as_deref());
    let partition_enabled = validity_partition_enabled();
    let consumption_mode = consumption_mode_for_partition(partition_enabled);
    let active_forgetting = active_forgetting_enabled();
    let page = reflect_final_page(
        storage,
        search,
        query_vec,
        query,
        LIMIT,
        MIN_SCORE,
        &scope,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
        RecallRerankMode::Runtime,
        false,
    )
    .await?;
    Ok(page
        .enriched
        .into_iter()
        .take(LIMIT)
        .map(|result| result.chunk.conversation_id)
        .collect())
}

/// [`reflect_for_health_probe_with_vec`] after local query embedding and the
/// shared intent resolution. No MCP client, subprocess or model call.
pub(crate) async fn reflect_for_health_probe(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
) -> Result<Vec<String>> {
    let query_vec = embed_query(embeddings, query).await?;
    let rerank_intent = resolve_rerank_intent(embeddings, &query_vec).await;
    reflect_for_health_probe_with_vec(storage, search, &query_vec, query, rerank_intent).await
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
    let xml = annotate_cross_source(xml, page.cross_source_collapsed);
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
    /// Codex rows left out because a Claude session row carries the same text.
    cross_source_collapsed: usize,
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
    /// Codex rows the cross-source dedupe left out of this pool.
    cross_source_collapsed: usize,
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
    let verdict_generation = storage.verdict_generation();
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

    // ADAPTIVE REFETCH (single, bounded): when demotions leave fewer than
    // `need` valid (non-demoted) results AND the first window came back
    // full (more candidates may exist beyond it), refetch ONCE at 10*need
    // (hard cap) and repartition. If still short, accept: a query whose top
    // 10*need candidates are all stale cannot backfill further — bounded
    // by design, not silent.
    let valid = pass
        .enriched
        .iter()
        .filter(|e| !is_demote_channel(&pass.validity, &e.chunk.id))
        .count();
    if valid < need && pass.window_full {
        // Echoes left out of the first window took places the page needs: ask
        // for a window wider than the first, not just the 10x floor, which is
        // smaller than the first window for a small page.
        let widened = if pass.echo_dropped > 0 {
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
    // The label reads the order the caller gets, not the one before the sinks.
    let ce = refresh_ce(pass.ce, pass.ce_inputs.as_ref(), &ids_before, &enriched);
    Ok(SettledPool {
        enriched,
        display_rank_scores: pass.display_rank_scores,
        search_ms,
        ce,
        exhausted: !pass.window_full,
        published: 0,
        cross_source_collapsed: pass.cross_source_collapsed,
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
    /// Codex rows dropped because a Claude session row has the same text.
    cross_source_collapsed: usize,
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

/// The chunk ids the cross-encoder scores: the first `limit` distinct ids of
/// the fused chunk ranking, then every id in `protected` (the S1 top
/// sessions' representatives) forced in by replacing, from the tail, ids that
/// are not themselves protected. The pool never exceeds `limit`.
fn ce_pool_ids(fused: &[String], protected: &[String], limit: usize) -> Vec<String> {
    let mut pool: Vec<String> = Vec::with_capacity(limit);
    let mut seen: HashSet<&str> = HashSet::new();
    for id in fused {
        if pool.len() >= limit {
            break;
        }
        if seen.insert(id.as_str()) {
            pool.push(id.clone());
        }
    }
    for id in protected {
        if pool.contains(id) {
            continue;
        }
        if pool.len() < limit {
            pool.push(id.clone());
        } else if let Some(tail) = pool.iter().rposition(|held| !protected.contains(held)) {
            pool.remove(tail);
            pool.push(id.clone());
        }
    }
    pool
}

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

/// Add the per-search cross-source collapse counter to the `<meta>` block of a
/// rendered page. Absent when nothing collapsed, so a page without the event
/// is byte for byte what it was.
fn annotate_cross_source(xml: String, collapsed: usize) -> String {
    if collapsed == 0 {
        return xml;
    }
    xml.replacen(
        "  </meta>\n",
        &format!("    <cross-source-collapsed>{collapsed}</cross-source-collapsed>\n  </meta>\n"),
        1,
    )
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
    let Some(encoder) = cross_encoder::global() else {
        warn_ce_unavailable("model files unavailable");
        return (collapsed, HashMap::new());
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
    let scores = match memo.ce_scores(&keys, &units, |fresh| encoder.score(query, fresh)) {
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
    let fetch = if search_mode == SearchMode::Fts {
        fetch.min(GET_MORE_WINDOW)
    } else {
        fetch
    };
    super::stage_timing::count("passes", 1);
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
        if search_mode == SearchMode::Hybrid {
            let idents = crate::search::identifiers::analyze_query(query);
            if !idents.is_empty() {
                postings_hits = storage
                    .search_postings(&idents, fetch, family.as_deref())
                    .unwrap_or_default();
                if !postings_hits.is_empty() {
                    postings_weight =
                        id_channel_weight(&idents, &postings_term_dfs(storage, &idents));
                }
            }
        }
        let keyword = keyword_chunk_hits(storage, query, fetch, family);
        let mut fts_window_full = keyword.window_full;
        let fts_hits = keyword.hits;
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
    let cross_source_collapsed = collapse_cross_source_duplicates(&mut enriched, |id| {
        storage.get_chunk_source(id).ok().flatten()
    });
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
    // With a live session the window is wider than the two rows shown, so
    // its sunk hits can give their places to others; without one it is 2.
    let live = live_session_id();
    let window = if live.is_some() { overfetch(2) } else { 2 };
    let results = if let Some(ref project) = effective_project {
        let ids = family_chunk_ids(storage, project)?;
        let idx = search.read().await;
        idx.search_chunks_filtered(query_vec, window, min_score, &ids)
    } else {
        let idx = search.read().await;
        idx.search_chunks(query_vec, window, min_score)
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
    let labeled_chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();
    let validity = csr_text_labels_only(storage, &labeled_chunks);
    apply_validity_partition(&mut enriched, &validity, false);
    let enriched = quick_check_hits(enriched, storage, live.as_deref(), query);

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
            query,
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
                    limit,
                )
            })?;
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
                    limit,
                )
            })?;
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
    // a fixed label for the log (not the Claude Code session) — same pattern as
    // reflect_on_past. Non-fatal: a logging failure must never fail the search.
    for item in &ranking.items {
        let _ = storage.log_retrieval_event(&item.chunk_id, "chunk", "mcp_search", "mcp");
    }

    render_why_evidence(
        storage,
        query,
        ranking.items,
        &ranking.hoisted_chunk_ids,
        &reinstatement.trace,
    )
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
                    chunks::full_conversation_text(&rebuilt, MCP_TRANSCRIPT_MAX_BUDGET_CHARS)
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
    data_dir: Option<&Path>,
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
    transcript_with_db(
        projects_dir,
        data_dir,
        None,
        session,
        view,
        project,
        role,
        turns,
        last,
        grep,
        tool,
        json,
        budget_chars,
        sidechains,
        include_system,
    )
}

/// [`transcript`], answering from the indexed chunks in `db` when the JSONL and
/// the mirror copy are both gone.
#[allow(clippy::too_many_arguments)]
pub fn transcript_with_db(
    projects_dir: &Path,
    data_dir: Option<&Path>,
    db: Option<&Storage>,
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

    Ok(crate::transcript::run_with_sources(
        projects_dir,
        data_dir,
        db,
        &req,
    ))
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
        let hits = storage.witness_verdicts_for_chunks(&identities)?;
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
    let hits = storage.csr_text_hits(chunks);
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
mod tests {
    #[test]
    fn f2a_refetch_reuses_first_pass_ce_scores() {
        let memo = CallMemo::default();
        let texts = ["alpha", "beta", "gamma"];
        let keys: Vec<String> = ["a", "b", "c"]
            .iter()
            .zip(texts)
            .map(|(id, text)| CallMemo::ce_key(id, text))
            .collect();
        let calls = std::cell::RefCell::new(Vec::<Vec<String>>::new());
        let scorer = |fresh: &[&str]| -> Result<Vec<f32>> {
            calls
                .borrow_mut()
                .push(fresh.iter().map(|t| t.to_string()).collect());
            Ok(fresh.iter().map(|t| t.len() as f32).collect())
        };
        let first = memo.ce_scores(&keys, &texts, scorer).unwrap();
        assert_eq!(first, vec![5.0, 4.0, 5.0]);
        // Second pass: a wider pool holding the same three chunks plus one new.
        let texts2 = ["delta!", "alpha", "beta", "gamma"];
        let keys2: Vec<String> = ["d", "a", "b", "c"]
            .iter()
            .zip(texts2)
            .map(|(id, text)| CallMemo::ce_key(id, text))
            .collect();
        let second = memo.ce_scores(&keys2, &texts2, scorer).unwrap();
        assert_eq!(second, vec![6.0, 5.0, 4.0, 5.0]);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            vec!["delta!".to_string()],
            "only the new unit is scored"
        );
        // A third pass of already scored units never calls the scorer.
        let third = memo
            .ce_scores(&keys, &texts, |_| panic!("scorer must not run"))
            .unwrap();
        assert_eq!(third, first);
        // The same id with changed text is a new unit.
        assert_ne!(
            CallMemo::ce_key("a", "alpha"),
            CallMemo::ce_key("a", "alpha2")
        );
    }

    use super::*;
    use crate::search::hybrid::fuse_rrf;
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
                "postings",
                "fuse_rrf",
                "dedupe",
                "final_page",
            ],
            "every documented stage must record exactly once, in pipeline order"
        );
        // The fixture query names no identifier: the postings stage is
        // recorded, and empty.
        let postings_sample = samples
            .iter()
            .find(|sample| sample.stage == "postings")
            .unwrap();
        assert_eq!(postings_sample.total_candidates, 0);
        for sample in samples.iter().filter(|sample| sample.stage != "postings") {
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

    /// The production write path (`store_reflection_internal`, real embeddings)
    /// for a story and a ledger row with the same words: every tool that reads
    /// reflections returns the story and never the shift.
    #[tokio::test]
    async fn shift_ledger_rows_written_for_real_never_reach_the_search_tools() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(EmbeddingEngine::dimension())));
        let words = "the scheduler now retries the upload queue after a timeout";
        store_reflection_internal(
            &storage,
            &embeddings,
            &search,
            &format!("{words} STORYPROSE"),
            &[
                "session_story".to_string(),
                "project_shiftleak".to_string(),
                "conv_leak1".to_string(),
            ],
        )
        .await
        .unwrap();
        store_reflection_internal(
            &storage,
            &embeddings,
            &search,
            &format!("[shift: model-derived] {words} LEAKEDSHIFT"),
            &[
                "narrative_shift".to_string(),
                "shift_project:shiftleak".to_string(),
                "shift_conv:leak1".to_string(),
                "shift_sig_00aa11bb22cc33dd".to_string(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            storage
                .get_ledger_reflections_by_tag("narrative_shift", 5)
                .unwrap()
                .len(),
            1,
            "the ledger row is stored (never deleted)"
        );

        let replies = [
            reflect_on_past(&storage, &embeddings, &search, words, 5, 0.0, None)
                .await
                .unwrap(),
            get_more_results(&storage, &embeddings, &search, words, 0, 5, 0.0, None)
                .await
                .unwrap(),
            why(
                &storage,
                &embeddings,
                &search,
                words,
                None,
                &crate::search::reinstatement::ReinstateConfig::default(),
            )
            .await
            .unwrap(),
            quick_check(&storage, &embeddings, &search, words, 0.0, None)
                .await
                .unwrap(),
            search_insights(&storage, &embeddings, &search, words, None)
                .await
                .unwrap(),
            search_by_concept(&storage, &embeddings, &search, words, 5, None)
                .await
                .unwrap(),
        ];
        for (i, reply) in replies.iter().enumerate() {
            assert!(!reply.contains("LEAKEDSHIFT"), "tool {i} leaked:\n{reply}");
        }
        assert!(
            replies[0].contains("STORYPROSE"),
            "the story still comes back:\n{}",
            replies[0]
        );
    }

    #[tokio::test]
    async fn shift_ledger_rows_never_reach_mcp_or_hook_outputs() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let query_vec = [1.0_f32, 0.0, 0.0, 0.0];
        let story_vec = vec![0.85_f32, (1.0 - 0.85 * 0.85_f32).sqrt(), 0.0, 0.0];
        let ledger_vec = vec![0.99_f32, (1.0 - 0.99 * 0.99_f32).sqrt(), 0.0, 0.0];

        let story_text =
            "MEMORY-ARCHIVE quarantine-shift-canary story prose about quarantine-shift-canary";
        let ledger_text =
            "MEMORY-ARCHIVE quarantine-shift-canary [shift: model-derived] LEAKED_SHIFT_CLAIM";

        storage
            .insert_reflection(
                "story-clean",
                story_text,
                &[
                    "session_story".to_string(),
                    "project_csr".to_string(),
                    "conv_q1".to_string(),
                    "session_q1".to_string(),
                ],
                &story_vec,
            )
            .unwrap();
        engine.insert_reflection("story-clean".to_string(), story_vec);

        storage
            .insert_reflection(
                "ledger-secret",
                ledger_text,
                &[
                    "narrative_shift".to_string(),
                    "shift_project:csr".to_string(),
                    "shift_conv:q1".to_string(),
                    "shift_rev_1".to_string(),
                    "shift_kind_risk".to_string(),
                    "shift_sig_0123456789abcdef".to_string(),
                    "shift_grounded".to_string(),
                    "project_csr".to_string(),
                    "conv_q1".to_string(),
                    "session_q1".to_string(),
                ],
                &ledger_vec,
            )
            .unwrap();
        engine.insert_reflection("ledger-secret".to_string(), ledger_vec);

        let search = Arc::new(RwLock::new(engine));

        for mode in [SearchMode::Vector, SearchMode::Fts, SearchMode::Hybrid] {
            let out = reflect_on_past_with_vec_mode(
                &storage,
                &search,
                &query_vec,
                "MEMORY-ARCHIVE quarantine-shift-canary",
                5,
                0.0,
                Some("csr"),
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
            .unwrap();
            assert!(
                out.contains("story prose"),
                "expected story-clean in {mode:?} output:\n{out}"
            );
            assert!(
                !out.contains("LEAKED_SHIFT_CLAIM"),
                "ledger row leaked in {mode:?} output:\n{out}"
            );
        }

        let kw_only = reflect_keyword_only(
            &storage,
            &search,
            "MEMORY-ARCHIVE quarantine-shift-canary",
            5,
            Some("csr"),
        )
        .await
        .unwrap();
        assert!(kw_only.contains("story prose"), "{kw_only}");
        assert!(
            !kw_only.contains("LEAKED_SHIFT_CLAIM"),
            "ledger row leaked in reflect_keyword_only:\n{kw_only}"
        );

        let by_conv = lookup_by_conv_tag(
            &storage,
            "q1",
            "quarantine-shift-canary",
            10,
            ConsumptionMode::Off,
            false,
        )
        .unwrap()
        .expect("conv_q1 has story");
        assert!(by_conv.contains("story prose"), "{by_conv}");
        assert!(
            !by_conv.contains("LEAKED_SHIFT_CLAIM"),
            "ledger row leaked in lookup_by_conv_tag:\n{by_conv}"
        );

        let learnings = get_session_learnings(&storage, "q1", 10).unwrap();
        assert!(learnings.contains("story prose"), "{learnings}");
        assert!(
            !learnings.contains("LEAKED_SHIFT_CLAIM"),
            "ledger row leaked in get_session_learnings:\n{learnings}"
        );

        // Hook / anti-pattern / csr_why HNSW reflection lookup gate:
        let raw_hits = search.read().await.search_reflections(&query_vec, 10, 0.0);
        assert!(raw_hits.iter().any(|r| r.id == "ledger-secret"));
        let hydrated: Vec<String> = raw_hits
            .iter()
            .filter_map(|r| storage.get_reflection_by_id(&r.id).ok().flatten())
            .map(|(content, _, _)| content)
            .collect();
        assert_eq!(hydrated, vec![story_text.to_string()]);
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

    // --- live-session sink: the caller's own chat never tops its own search ---

    const LIVE: &str = "6ca2a4d5-5785-4952-90ed-a5c245cbdfee";

    fn row_ids(rows: &[EnrichedResult]) -> Vec<&str> {
        rows.iter().map(|e| e.chunk.id.as_str()).collect()
    }

    /// Runs `body` with `LIVE` as this thread's live session id.
    fn with_live_session<T>(live: Option<&str>, body: impl FnOnce() -> T) -> T {
        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = live.map(str::to_string));
        let out = body();
        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        out
    }

    fn live_mix() -> Vec<EnrichedResult> {
        vec![
            enriched_result_conv("self-1", LIVE, 0.95, 0),
            enriched_result_conv("other-1", "conv-other-1", 0.90, 1),
            enriched_result_conv("self-2", LIVE, 0.85, 2),
            enriched_result_conv("other-2", "conv-other-2", 0.80, 3),
        ]
    }

    #[test]
    fn live_session_hits_sink_below_others_and_keep_each_groups_order() {
        let storage = Storage::open_memory().unwrap();
        let mut rows = live_mix();
        sink_session_hits(&mut rows, &storage, Some(LIVE), "how did we fix it");
        assert_eq!(row_ids(&rows), ["other-1", "other-2", "self-1", "self-2"]);
    }

    #[test]
    fn live_session_only_results_still_return_in_order() {
        let storage = Storage::open_memory().unwrap();
        let mut rows = vec![
            enriched_result_conv("self-1", LIVE, 0.95, 0),
            enriched_result_conv("self-2", LIVE, 0.85, 1),
        ];
        sink_session_hits(&mut rows, &storage, Some(LIVE), "how did we fix it");
        assert_eq!(row_ids(&rows), ["self-1", "self-2"]);
    }

    #[test]
    fn a_query_naming_the_session_leaves_the_order_alone() {
        let storage = Storage::open_memory().unwrap();
        for query in [
            format!("what happened in {LIVE}"),
            format!("conv_{LIVE} summary"),
            format!("WHAT HAPPENED IN {}", LIVE.to_uppercase()),
        ] {
            let mut rows = live_mix();
            sink_session_hits(&mut rows, &storage, Some(LIVE), &query);
            assert_eq!(
                row_ids(&rows),
                ["self-1", "other-1", "self-2", "other-2"],
                "{query}"
            );
        }
        // A different session id in the query is not the live one.
        let mut rows = live_mix();
        sink_session_hits(&mut rows, &storage, Some(LIVE), "in 11111111-2222-3333");
        assert_eq!(row_ids(&rows), ["other-1", "other-2", "self-1", "self-2"]);
    }

    #[test]
    fn without_a_live_session_the_rows_are_byte_identical() {
        let storage = Storage::open_memory().unwrap();
        let before = live_mix();
        let mut rows = live_mix();
        sink_session_hits(&mut rows, &storage, None, "how did we fix it");
        assert_eq!(row_ids(&rows), row_ids(&before));
        assert!(rows
            .iter()
            .zip(&before)
            .all(|(a, b)| a.score.to_bits() == b.score.to_bits()
                && a.chunk.content == b.chunk.content));
        // A blank or unset variable reads as no session.
        assert_eq!(parse_live_session(None), None);
        assert_eq!(parse_live_session(Some(String::new())), None);
        assert_eq!(parse_live_session(Some("  \n".into())), None);
        assert_eq!(
            parse_live_session(Some(format!(" {LIVE}\n"))).as_deref(),
            Some(LIVE)
        );
        with_live_session(None, || assert_eq!(live_session_id(), None));
        // The `conv_` form of the id is the same session.
        let mut prefixed = vec![
            enriched_result_conv("self-p", &format!("conv_{LIVE}"), 0.9, 0),
            enriched_result_conv("other-p", "conv-other", 0.8, 1),
        ];
        sink_session_hits(&mut prefixed, &storage, Some(LIVE), "q");
        assert_eq!(row_ids(&prefixed), ["other-p", "self-p"]);
    }

    /// A summary derived from the live chat (its id ends in `_<sid>`) sinks
    /// with it; another chat's summary and an id that only runs into the sid
    /// do not.
    #[test]
    fn a_row_derived_from_the_live_chat_sinks_with_it() {
        let storage = Storage::open_memory().unwrap();
        let mut rows = vec![
            enriched_result_conv("ext", &format!("extracted_v3_{LIVE}"), 0.99, 0),
            enriched_result_conv("v3", &format!("v3_{LIVE}"), 0.98, 1),
            enriched_result_conv("story", &format!("story_{LIVE}"), 0.97, 2),
            enriched_result_conv(
                "other-ext",
                "extracted_v3_11111111-2222-3333-4444-555555555555",
                0.96,
                3,
            ),
            enriched_result_conv("run-in", &format!("x{LIVE}"), 0.95, 4),
        ];
        sink_session_hits(&mut rows, &storage, Some(LIVE), "q");
        assert_eq!(
            row_ids(&rows),
            ["other-ext", "run-in", "ext", "v3", "story"]
        );
    }

    #[test]
    fn a_sidechain_chunk_whose_parent_is_the_live_session_sinks() {
        let storage = Storage::open_memory().unwrap();
        let mut child = enriched_result_conv("child", "agent-a0c1", 0.97, 0);
        child.chunk.is_sidechain = true;
        let mut orphan = enriched_result_conv("orphan", "agent-zzzz", 0.96, 1);
        orphan.chunk.is_sidechain = true;
        storage
            .insert_chunk(&child.chunk, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        storage
            .insert_chunk_provenance(
                "child",
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::Assistant,
                    source_conv_id: LIVE.into(),
                    supersedes: None,
                },
            )
            .unwrap();
        let mut rows = vec![
            child,
            orphan,
            enriched_result_conv("other", "conv-other", 0.5, 2),
        ];
        sink_session_hits(&mut rows, &storage, Some(LIVE), "q");
        // The orphan sidechain has no parent on record: it is not the live chat.
        assert_eq!(row_ids(&rows), ["orphan", "other", "child"]);
    }

    #[test]
    fn file_search_chunks_of_the_live_session_sink() {
        let storage = Storage::open_memory().unwrap();
        let mut chunks: Vec<_> = live_mix().into_iter().map(|e| e.chunk).collect();
        sink_session_chunks(&mut chunks, &storage, Some(LIVE), "src/mcp/tools.rs");
        let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["other-1", "other-2", "self-1", "self-2"]);
    }

    /// The cut comes after the sink: with a page of one, the best other hit
    /// takes the slot, and with no live session the best hit overall does.
    #[test]
    fn simple_search_sink_runs_before_the_page_cut() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        for (id, conv, vector) in [
            ("self-1", LIVE, [1.0, 0.0, 0.0, 0.0]),
            ("other-1", "conv-other-1", [0.9, 0.1, 0.0, 0.0]),
        ] {
            let chunk = enriched_result_conv(id, conv, 1.0, 0).chunk;
            storage.insert_chunk(&chunk, &vector).unwrap();
        }
        let fetch = |_: usize| {
            vec![
                crate::search::SearchResult {
                    id: "self-1".into(),
                    score: 0.99,
                },
                crate::search::SearchResult {
                    id: "other-1".into(),
                    score: 0.90,
                },
            ]
        };
        let page = |live: Option<&str>, query: &str| {
            with_live_session(live, || {
                fetch_enrich_partition_adaptive(&storage, fetch, query, 1, ConsumptionMode::Off)
                    .unwrap()
                    .iter()
                    .map(|e| e.chunk.id.clone())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(page(Some(LIVE), "q"), ["other-1"]);
        assert_eq!(page(None, "q"), ["self-1"]);
        assert_eq!(page(Some(LIVE), &format!("in {LIVE}")), ["self-1"]);
    }

    fn live_pipeline_fixture() -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut search = SearchEngine::new(8);
        for (id, conv, vector) in [
            ("self-chunk", LIVE, [1.0f32, 0.0, 0.0, 0.0]),
            ("other-chunk", "conv-other", [0.9, 0.1, 0.0, 0.0]),
        ] {
            let mut chunk = enriched_result_conv(id, conv, 1.0, 0).chunk;
            chunk.content = format!("live session sink probe {id}");
            storage.insert_chunk(&chunk, &vector).unwrap();
            search.insert_chunk(chunk.id, vector.to_vec());
        }
        (storage, Arc::new(RwLock::new(search)))
    }

    async fn live_pipeline_page(live: Option<&str>, query: &str, limit: usize) -> String {
        let (storage, search) = live_pipeline_fixture();
        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = live.map(str::to_string));
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            query,
            limit,
            0.1,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        out
    }

    #[tokio::test]
    async fn recall_pipeline_sinks_the_live_chat_but_still_returns_it() {
        let before = live_pipeline_page(None, "sink probe unset", 1).await;
        assert!(before.contains("<id>self-chunk</id>"), "{before}");

        let after = live_pipeline_page(Some(LIVE), "sink probe set", 1).await;
        assert!(after.contains("<id>other-chunk</id>"), "{after}");
        assert!(!after.contains("<id>self-chunk</id>"), "{after}");

        // Both fit: the live chat is still there, below the other hit.
        let both = live_pipeline_page(Some(LIVE), "sink probe both", 5).await;
        let other_at = pos_of(&both, "<id>other-chunk</id>");
        let self_at = pos_of(&both, "<id>self-chunk</id>");
        assert!(other_at < self_at, "{both}");

        // Naming the session on purpose lifts the sink.
        let named = live_pipeline_page(Some(LIVE), &format!("probe {LIVE}"), 1).await;
        assert!(named.contains("<id>self-chunk</id>"), "{named}");
    }

    #[tokio::test]
    async fn quick_check_skips_past_the_live_chat() {
        let rows = [
            ("top", "project-a", "top candidate", [1.0, 0.0, 0.0, 0.0]),
            (
                "runner-up",
                "project-b",
                "runner-up candidate",
                [0.8, 0.6, 0.0, 0.0],
            ),
        ];
        let run = |live: Option<&'static str>| {
            let (storage, search) = quick_check_fixture(&rows);
            async move {
                LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = live.map(str::to_string));
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
                LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
                xml
            }
        };
        let unset = run(None).await;
        assert!(unset.contains("top candidate"), "{unset}");
        let set = run(Some("conv-top")).await;
        assert!(set.contains("runner-up candidate"), "{set}");
        assert!(!set.contains("top candidate"), "{set}");
    }

    /// Quick check on cosines against `[1, 0, 0, 0]`: the live chat 0.90 and one
    /// other chat at `other_cosine`.
    async fn quick_check_with_live_chat(other_cosine: f32, self_cosine: f32) -> String {
        let other_y = (1.0 - other_cosine * other_cosine).sqrt();
        let self_y = (1.0 - self_cosine * self_cosine).sqrt();
        let (storage, search) = quick_check_fixture(&[
            (
                "self",
                "project-a",
                "self candidate",
                [self_cosine, self_y, 0.0, 0.0],
            ),
            (
                "other",
                "project-b",
                "other candidate",
                [other_cosine, other_y, 0.0, 0.0],
            ),
        ]);
        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = Some("conv-self".into()));
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
        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        xml
    }

    /// The only hit above the floor is the live chat: it is shown, not a
    /// below-floor other hit that the sink moved to the front.
    #[tokio::test]
    async fn quick_check_shows_the_qualifying_live_hit_when_no_other_qualifies() {
        let xml = quick_check_with_live_chat(0.40, 0.90).await;
        assert!(xml.contains("<found>true</found>"), "{xml}");
        assert!(xml.contains("self candidate"), "{xml}");
        assert!(!xml.contains("other candidate"), "{xml}");
    }

    /// A qualifying other hit is shown over the better-scoring live hit, and
    /// its margin is not read against a runner-up that beats it.
    #[tokio::test]
    async fn quick_check_prefers_the_qualifying_other_hit_over_the_live_one() {
        let xml = quick_check_with_live_chat(0.60, 0.90).await;
        assert!(xml.contains("<found>true</found>"), "{xml}");
        assert!(xml.contains("other candidate"), "{xml}");
        assert!(!xml.contains("self candidate"), "{xml}");
        assert!(!xml.contains("<margin>-"), "{xml}");
    }

    /// Nothing clears the floor: the answer is still the negative claim, with
    /// the best score rejected.
    #[tokio::test]
    async fn quick_check_with_nothing_above_the_floor_stays_negative_with_a_live_chat() {
        let xml = quick_check_with_live_chat(0.35, 0.40).await;
        assert!(xml.contains("<found>false</found>"), "{xml}");
        assert!(xml.contains("<best_rejected_score>0.400"), "{xml}");
    }

    /// With the daemon down every tool answers from the keyword fallback. The
    /// live chat sinks there too, so recall(limit=1) followed by get_more(offset=1)
    /// shows each of the two hits exactly once.
    #[tokio::test]
    async fn keyword_fallbacks_sink_the_live_chat_and_paginate_continuously() {
        let v = [1.0, 0.0, 0.0, 0.0];
        let (storage, search) = quick_check_fixture(&[
            (
                "self",
                "alpha",
                "SELFMARK fallbackword fallbackword fallbackword fallbackword",
                v,
            ),
            (
                "other",
                "alpha",
                "OTHERMARK fallbackword pad pad pad pad pad pad pad pad pad pad pad pad",
                v,
            ),
        ]);
        storage
            .with_connection(|conn| {
                conn.execute("UPDATE chunks SET timestamp='2026-10-02T00:00:00Z'", [])?;
                Ok(())
            })
            .unwrap();
        // The raw keyword order has the live chat first.
        let raw = keyword_candidates(&storage, "fallbackword", Some("alpha"), None).unwrap();
        assert!(raw[0].chunk.content.contains("SELFMARK"), "premise");

        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = Some("conv-self".into()));
        let recall = keyword_fallback(
            &storage,
            &search,
            "fallbackword",
            1,
            Some("alpha"),
            KeywordTool::Recall,
        )
        .await
        .unwrap();
        let more = keyword_fallback(
            &storage,
            &search,
            "fallbackword",
            1,
            Some("alpha"),
            KeywordTool::More { offset: 1 },
        )
        .await
        .unwrap();
        // Page one holds `other` alone; page two holds `self` alone.
        assert_eq!(recall.matches("<id>other</id>").count(), 1, "{recall}");
        assert!(!recall.contains("SELFMARK"), "{recall}");
        assert_eq!(more.matches("SELFMARK").count(), 1, "{more}");
        assert!(!more.contains("OTHERMARK"), "{more}");

        async fn one_page(
            storage: &Arc<Storage>,
            search: &Arc<RwLock<SearchEngine>>,
            tool: KeywordTool<'_>,
        ) -> String {
            keyword_fallback(storage, search, "fallbackword", 1, Some("alpha"), tool)
                .await
                .unwrap()
        }
        let quick = one_page(&storage, &search, KeywordTool::Quick).await;
        let concept = one_page(&storage, &search, KeywordTool::Concept).await;
        let recency = one_page(
            &storage,
            &search,
            KeywordTool::Recency {
                time_range: None,
                since: Some("2026-10-01"),
                until: Some("2026-10-03"),
            },
        )
        .await;
        LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        for (name, reply) in [("quick", quick), ("concept", concept), ("recency", recency)] {
            assert!(reply.contains("OTHERMARK"), "{name}: {reply}");
            assert!(!reply.contains("SELFMARK"), "{name}: {reply}");
        }
        // Without a live session the raw keyword order is what every tool shows.
        let unset = one_page(&storage, &search, KeywordTool::Concept).await;
        assert!(unset.contains("SELFMARK"), "{unset}");
    }

    /// Keyword recall sinks an eval session's rig row; get_more(offset=1) on the
    /// fallback must continue that order, not re-serve the clean row.
    #[tokio::test]
    async fn keyword_get_more_continues_the_rig_sunk_order() {
        let v = [1.0, 0.0, 0.0, 0.0];
        let (storage, search) = quick_check_fixture(&[
            (
                "rig",
                "alpha",
                "RIGMARK fallbackword fallbackword fallbackword ran in /Users/x/.testbed/core-rewrite/wt",
                v,
            ),
            (
                "clean",
                "alpha",
                "CLEANMARK fallbackword pad pad pad pad pad pad pad pad pad pad pad pad",
                v,
            ),
        ]);
        // Five more rig rows make conv-rig an eval session by share.
        for seq in 0..5 {
            let chunk = crate::import::ConversationChunk {
                id: format!("rig-extra-{seq}"),
                conversation_id: "conv-rig".into(),
                project_name: "alpha".into(),
                timestamp: "2099-01-01T00:00:00Z".into(),
                content: "ran in /Users/x/.testbed/core-rewrite/wt".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::ToolResult,
                seq: 10 + seq,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &v).unwrap();
        }
        let raw = keyword_candidates(&storage, "fallbackword", Some("alpha"), None).unwrap();
        assert!(raw[0].chunk.content.contains("RIGMARK"), "premise");

        let recall = keyword_fallback(
            &storage,
            &search,
            "fallbackword",
            1,
            Some("alpha"),
            KeywordTool::Recall,
        )
        .await
        .unwrap();
        let more = keyword_fallback(
            &storage,
            &search,
            "fallbackword",
            1,
            Some("alpha"),
            KeywordTool::More { offset: 1 },
        )
        .await
        .unwrap();
        assert!(recall.contains("CLEANMARK"), "{recall}");
        assert!(!recall.contains("RIGMARK"), "{recall}");
        assert!(more.contains("RIGMARK"), "{more}");
        assert!(!more.contains("CLEANMARK"), "{more}");
    }

    /// CE order [live:9, other:1] with a page of one: the sink puts `other`
    /// first, so the summary must be read from that order, not keep the "high"
    /// label the live chat's scores earned.
    #[test]
    fn ce_summary_follows_the_order_the_sinks_leave() {
        let scores: HashMap<String, f32> = [("conv-live".to_string(), 9.0), ("conv-o".into(), 1.0)]
            .into_iter()
            .collect();
        let inputs = CeInputs {
            session_scores: scores.clone(),
            session_of_chunk: [
                ("live".to_string(), "conv-live".to_string()),
                ("o".into(), "conv-o".into()),
            ]
            .into_iter()
            .collect(),
        };
        let before = final_ce_summary(&["conv-live", "conv-o"], &scores);
        assert_eq!(
            before.unwrap().label_with(&format::CeBands::SHIPPED),
            "high",
            "premise: the live chat's scores earn high"
        );
        let ids_before = vec!["live".to_string(), "o".to_string()];
        let unchanged = vec![
            enriched_result_conv("live", "conv-live", 0.9, 0),
            enriched_result_conv("o", "conv-o", 0.8, 1),
        ];
        assert_eq!(
            refresh_ce(before, Some(&inputs), &ids_before, &unchanged),
            before,
            "no reorder: the summary is the pass's own"
        );
        let sunk = vec![
            enriched_result_conv("o", "conv-o", 0.8, 1),
            enriched_result_conv("live", "conv-live", 0.9, 0),
        ];
        let after = refresh_ce(before, Some(&inputs), &ids_before, &sunk).unwrap();
        assert_eq!(after.top, 1.0);
        assert_eq!(after.runner_up, Some(9.0));
        assert_ne!(after.label_with(&format::CeBands::SHIPPED), "high");
        // Nothing to read it from: the pass's summary stands.
        assert_eq!(refresh_ce(before, None, &ids_before, &sunk), before);
    }

    /// The pipeline, not just the map: with the dream switches unset (annotate
    /// only) and with the validity kill switch (Off), a logged CSR emission in a
    /// conversation is dropped before ranking and the clean result stays; the
    /// same text in a plan document is not an emission and both come back.
    #[tokio::test]
    async fn search_drops_logged_csr_emissions_and_keeps_plan_text() {
        // CSR text in a plan document is kept. The same text in a user row of a
        // conversation is a logged emission and the echo filter drops it. The
        // `[CSR text]` label and sink for the tools that keep such rows are
        // covered by the quick-check, recent-work and file-search tests below.
        for (csr_conversation, partition, mode) in [
            ("plan:conv-csr", true, ConsumptionMode::AnnotateOnly),
            ("plan:conv-csr", false, ConsumptionMode::Off),
            ("conv-csr", true, ConsumptionMode::AnnotateOnly),
        ] {
            let kept = csr_conversation.starts_with("plan:");
            let storage = Arc::new(Storage::open_memory().unwrap());
            let mut search = SearchEngine::new(8);
            for (id, conversation, content, vector, author) in [
                (
                    "csr-chunk",
                    csr_conversation,
                    format!("shared recall text\n{CSR_RECAP_TEXT}"),
                    [1.0, 0.0, 0.0, 0.0],
                    crate::provenance::Speaker::User,
                ),
                (
                    "clean-chunk",
                    "conv-clean",
                    "shared recall text about ordinary work".to_string(),
                    [0.9, 0.1, 0.0, 0.0],
                    crate::provenance::Speaker::User,
                ),
            ] {
                let chunk = crate::import::ConversationChunk {
                    id: id.into(),
                    conversation_id: conversation.into(),
                    project_name: "test".into(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    content,
                    message_count: 1,
                    summary: None,
                    author,
                    seq: 0,
                    is_sidechain: false,
                };
                storage.insert_chunk(&chunk, &vector).unwrap();
                storage
                    .insert_chunk_provenance(
                        &chunk.id,
                        &crate::provenance::ChunkProvenance {
                            author,
                            source_conv_id: conversation.into(),
                            supersedes: None,
                        },
                    )
                    .unwrap();
                search.insert_chunk(chunk.id, vector.to_vec());
            }
            let search = Arc::new(RwLock::new(search));
            let out = reflect_on_past_with_vec(
                &storage,
                &search,
                &[1.0, 0.0, 0.0, 0.0],
                "recall",
                5,
                0.1,
                Some("all"),
                0,
                partition,
                mode,
                false,
            )
            .await
            .unwrap();
            if !kept {
                assert!(
                    !out.contains("<id>csr-chunk</id>"),
                    "{mode:?}: the logged emission is dropped\n{out}"
                );
                assert!(out.contains("<id>clean-chunk</id>"), "{out}");
                continue;
            }
            assert!(
                out.contains("<id>csr-chunk</id>"),
                "{mode:?}: the plan row is kept\n{out}"
            );
            assert!(out.contains("<id>clean-chunk</id>"), "{out}");
        }
    }

    const CSR_RULE_TEXT: &str =
        "A standing rule from 10-12: “Keep receipts”\nsee libfile in the tree";

    /// A stored user chunk holding CSR text (and its true author in
    /// chunk_provenance), with a vector for the search index.
    fn store_csr_text_chunk(
        storage: &Arc<Storage>,
        search: &mut SearchEngine,
        id: &str,
        conversation: &str,
        content: &str,
        vector: [f32; 4],
    ) {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "test".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &vector).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::User,
                    source_conv_id: conversation.into(),
                    supersedes: None,
                },
            )
            .unwrap();
        search.insert_chunk(id.to_string(), vector.to_vec());
    }

    /// Every chunk-returning tool labels a chunk holding CSR text and never
    /// drops its row: quick check, recent work, the file search fallback.
    #[tokio::test]
    async fn the_other_chunk_returning_tools_label_csr_text_and_keep_the_row() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut index = SearchEngine::new(8);
        store_csr_text_chunk(
            &storage,
            &mut index,
            "csr-chunk",
            "conv-csr",
            CSR_RULE_TEXT,
            [1.0, 0.0, 0.0, 0.0],
        );
        let search = Arc::new(RwLock::new(index));

        let quick = quick_check_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "q",
            0.1,
            Some("all"),
        )
        .await
        .unwrap();
        assert!(quick.contains("<found>true</found>"), "{quick}");
        assert!(quick.contains("[CSR text]"), "quick check: {quick}");

        let recent = get_recent_work(&storage, 5, None, "conversation")
            .await
            .unwrap();
        assert!(recent.contains("conv-csr"), "the row is kept: {recent}");
        assert!(recent.contains("[CSR text]"), "recent work: {recent}");

        let by_file = search_by_file(&storage, "libfile", 5, None).await.unwrap();
        assert!(by_file.contains("conv-csr"), "the row is kept: {by_file}");
        assert!(by_file.contains("[CSR text]"), "file search: {by_file}");
    }

    /// With no ledger for the file, the fallback finds a chunk that names the
    /// path, and not one that only holds the path's words apart.
    #[tokio::test]
    async fn file_search_fallback_finds_a_chunk_that_names_a_slash_path() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut index = SearchEngine::new(8);
        store_csr_text_chunk(
            &storage,
            &mut index,
            "names-it",
            "conv-names",
            "changed csr-engine/src/storage/mod.rs to add a column",
            [1.0, 0.0, 0.0, 0.0],
        );
        store_csr_text_chunk(
            &storage,
            &mut index,
            "scattered",
            "conv-scattered",
            "the storage engine in src of csr, a mod for rs files",
            [0.0, 1.0, 0.0, 0.0],
        );
        let out = search_by_file(&storage, "csr-engine/src/storage/mod.rs", 5, None)
            .await
            .unwrap();
        assert!(out.contains("conv-names"), "{out}");
        assert!(!out.contains("conv-scattered"), "{out}");
    }

    /// The file search fallback sinks a CSR-text row below the clean rows, order
    /// kept inside each group, nothing dropped.
    #[tokio::test]
    async fn file_search_sinks_a_csr_text_row_below_the_clean_ones() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut index = SearchEngine::new(8);
        store_csr_text_chunk(
            &storage,
            &mut index,
            "csr-chunk",
            "conv-csr",
            "A standing rule from 10-12: “Keep receipts”\nlibfile libfile libfile libfile",
            [1.0, 0.0, 0.0, 0.0],
        );
        store_csr_text_chunk(
            &storage,
            &mut index,
            "clean-chunk",
            "conv-clean",
            "notes about the tree, see libfile once among much other unrelated text here",
            [0.0, 1.0, 0.0, 0.0],
        );
        let rows = storage.fts5_search("libfile", 5, None).unwrap();
        assert_eq!(
            rows[0].0.id, "csr-chunk",
            "the CSR row ranks first before the sink"
        );
        let out = search_by_file(&storage, "libfile", 5, None).await.unwrap();
        let csr = out.find("conv-csr").expect("kept");
        let clean = out.find("conv-clean").expect("kept");
        assert!(clean < csr, "clean first\n{out}");
    }

    /// The printed csr_why output keeps every clean row above every CSR-text
    /// row even when they share a conversation: the formatter's grouping by
    /// conversation must not lift the CSR row back up.
    #[test]
    fn printed_why_output_keeps_clean_evidence_above_csr_text_evidence() {
        use crate::search::reinstatement::{EvidenceItem, ReinstateTrace, Via};
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut index = SearchEngine::new(8);
        for (id, conversation, content) in [
            ("c", "A", CSR_RULE_TEXT),
            ("a1", "A", "we moved the index load out of the hook"),
            ("b1", "B", "we capped the retry loop"),
        ] {
            store_csr_text_chunk(
                &storage,
                &mut index,
                id,
                conversation,
                content,
                [1.0, 0.0, 0.0, 0.0],
            );
        }
        let item = |id: &str, conversation: &str, score: f32| EvidenceItem {
            chunk_id: id.into(),
            conversation_id: conversation.into(),
            score,
            best_route: Via::Keyword,
            routes: [Via::Keyword].into_iter().collect(),
            route_scores: [(Via::Keyword, score)].into_iter().collect(),
            timestamp: "2026-10-13T00:00:00Z".into(),
            excerpt: format!("EXCERPT-{id}"),
            ratification: None,
            trust: crate::provenance::TrustTier::Unknown,
        };
        // ranked: C (0.95) in A, then A1 (0.90) in A, then B1 (0.85) in B
        let rendered = render_why_evidence(
            &storage,
            "q",
            vec![
                item("c", "A", 0.95),
                item("a1", "A", 0.90),
                item("b1", "B", 0.85),
            ],
            &HashSet::new(),
            &ReinstateTrace::default(),
        )
        .unwrap();
        let at = |needle: &str| {
            rendered
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} dropped\n{rendered}"))
        };
        assert!(
            at("EXCERPT-a1") < at("EXCERPT-b1") && at("EXCERPT-b1") < at("EXCERPT-c"),
            "printed order must be A1, B1, C\n{rendered}"
        );
        assert!(rendered.contains("[CSR text]"), "{rendered}");
    }

    #[test]
    fn why_evidence_sinks_a_csr_text_chunk_below_the_clean_ones() {
        use crate::search::reinstatement::{EvidenceItem, Via};
        let item = |id: &str, score: f32| EvidenceItem {
            chunk_id: id.into(),
            conversation_id: format!("conv-{id}"),
            score,
            best_route: Via::Keyword,
            routes: [Via::Keyword].into_iter().collect(),
            route_scores: [(Via::Keyword, score)].into_iter().collect(),
            timestamp: "2026-10-13T00:00:00Z".into(),
            excerpt: "x".into(),
            ratification: None,
            trust: crate::provenance::TrustTier::Unknown,
        };
        let items = vec![item("csr", 0.9), item("a", 0.8), item("b", 0.7)];
        let labels: HashMap<String, String> =
            [("csr".to_string(), "[CSR text] x".to_string())].into();
        let sunk = sink_labeled_evidence(items, &labels);
        let ids: Vec<&str> = sunk.iter().map(|i| i.chunk_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "csr"]);
    }

    /// A failed dream-verdict read must not lose the CSR-text labels (recency,
    /// concept and keyword fallback all use this wrapper).
    #[test]
    fn a_failed_verdict_read_keeps_the_csr_text_labels() {
        use crate::provenance::Speaker;
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut index = SearchEngine::new(8);
        store_csr_text_chunk(
            &storage,
            &mut index,
            "csr-chunk",
            "conv-csr",
            CSR_RULE_TEXT,
            [1.0, 0.0, 0.0, 0.0],
        );
        let chunk = csr_text_result(
            "csr-chunk",
            "conv-csr",
            Speaker::User,
            CSR_RULE_TEXT,
            0.9,
            0,
        )
        .chunk;
        // a real SQLite read failure: the validity resolver starts from code_nodes
        storage
            .with_connection(|conn| {
                conn.execute_batch("DROP TABLE code_nodes")?;
                Ok(())
            })
            .unwrap();
        assert!(resolve_validity_checked(
            &storage,
            std::slice::from_ref(&chunk),
            ConsumptionMode::AnnotateOnly
        )
        .is_err());
        for mode in [ConsumptionMode::AnnotateOnly, ConsumptionMode::Full] {
            let validity = resolve_validity_with(&storage, std::slice::from_ref(&chunk), mode);
            let entry = validity
                .get("csr-chunk")
                .unwrap_or_else(|| panic!("{mode:?}: label lost"));
            assert!(entry.note.starts_with("[CSR text]"), "{}", entry.note);
            assert!(entry.demote);
        }
    }

    #[test]
    fn why_evidence_from_a_csr_text_chunk_is_labeled_and_kept() {
        use crate::search::reinstatement::{EvidenceItem, ReinstateTrace, Via};
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut index = SearchEngine::new(8);
        store_csr_text_chunk(
            &storage,
            &mut index,
            "csr-chunk",
            "conv-csr",
            CSR_RULE_TEXT,
            [1.0, 0.0, 0.0, 0.0],
        );
        let item = EvidenceItem {
            chunk_id: "csr-chunk".into(),
            conversation_id: "conv-csr".into(),
            score: 0.9,
            best_route: Via::Keyword,
            routes: [Via::Keyword].into_iter().collect(),
            route_scores: [(Via::Keyword, 0.9)].into_iter().collect(),
            timestamp: "2026-10-13T00:00:00Z".into(),
            excerpt: "A standing rule".into(),
            ratification: None,
            trust: crate::provenance::TrustTier::Unknown,
        };
        let labels = why_csr_text_labels(&storage, std::slice::from_ref(&item));
        let rendered = format_why_labeled(
            "q",
            &[item],
            &HashSet::new(),
            &ReinstateTrace::default(),
            &labels,
        );
        assert!(rendered.contains("conv_conv-csr"), "kept: {rendered}");
        assert!(rendered.contains("[CSR text]"), "{rendered}");
    }

    /// 20% of a real corpus has no chunk_provenance row. Such a conversation
    /// chunk has an unknown author, not a tool author: it is labeled and sunk
    /// like any other.
    #[tokio::test]
    async fn a_legacy_chunk_with_no_provenance_row_is_labeled_and_sunk() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut search = SearchEngine::new(8);
        for (id, conversation, content, vector) in [
            (
                "legacy-csr",
                "conv-legacy",
                "A standing rule from 10-12: “Keep receipts”\nshared recall text".to_string(),
                [1.0, 0.0, 0.0, 0.0],
            ),
            (
                "legacy-clean",
                "conv-clean",
                "shared recall text about ordinary work".to_string(),
                [0.9, 0.1, 0.0, 0.0],
            ),
        ] {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation.into(),
                project_name: "test".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content,
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            // no insert_chunk_provenance: a legacy row
            storage.insert_chunk(&chunk, &vector).unwrap();
            search.insert_chunk(chunk.id, vector.to_vec());
        }
        let search = Arc::new(RwLock::new(search));
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "recall",
            5,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::AnnotateOnly,
            false,
        )
        .await
        .unwrap();
        let csr = out.find("<id>legacy-csr</id>").expect("kept");
        let clean = out.find("<id>legacy-clean</id>").expect("kept");
        assert!(clean < csr, "clean first\n{out}");
        assert!(out.contains("[CSR text]"), "{out}");
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

    fn f1b_chunk(
        id: &str,
        conversation_id: &str,
        content: &str,
    ) -> crate::import::ConversationChunk {
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
            is_sidechain: false,
        }
    }

    async fn f1b_fts_search(storage: &Arc<Storage>, query: &str, limit: usize) -> String {
        let search = Arc::new(RwLock::new(SearchEngine::new(8)));
        reflect_on_past_with_vec(
            storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            query,
            limit,
            0.1,
            Some("all"),
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn f1b_dedupe_codex_and_claude_identical_text_collapse_to_origin() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let codex = f1b_chunk(
            "f1b-codex",
            "codex-session",
            "Dedupetoken the   rollout NOTES say the gate is green",
        );
        storage
            .insert_chunk_with_source(&codex, &[0.0; 4], "codex_rollout")
            .unwrap();
        let claude = f1b_chunk(
            "f1b-claude",
            "claude-session",
            "dedupetoken the rollout notes say\nthe gate is green",
        );
        storage
            .insert_chunk_with_source(&claude, &[0.0; 4], "conversation")
            .unwrap();

        let output = f1b_fts_search(&storage, "dedupetoken", 5).await;
        assert!(output.contains("<count>1</count>"), "{output}");
        assert!(output.contains("<cid>claude-session</cid>"), "{output}");
        assert!(!output.contains("codex-session"), "{output}");
        assert!(
            output.contains("<cross-source-collapsed>1</cross-source-collapsed>"),
            "{output}"
        );
    }

    #[tokio::test]
    async fn f1b_dedupe_different_text_not_collapsed() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let codex = f1b_chunk(
            "f1b-codex",
            "codex-session",
            "dedupetoken the gate is green",
        );
        storage
            .insert_chunk_with_source(&codex, &[0.0; 4], "codex_rollout")
            .unwrap();
        let claude = f1b_chunk(
            "f1b-claude",
            "claude-session",
            "dedupetoken the gate is red",
        );
        storage
            .insert_chunk_with_source(&claude, &[0.0; 4], "conversation")
            .unwrap();

        let output = f1b_fts_search(&storage, "dedupetoken", 5).await;
        assert!(output.contains("<count>2</count>"), "{output}");
        assert!(output.contains("codex-session"), "{output}");
        assert!(output.contains("claude-session"), "{output}");
        assert!(!output.contains("cross-source-collapsed"), "{output}");
    }

    #[test]
    fn f1b_dedupe_origin_row_takes_the_best_rank_of_the_text() {
        let row = |id: &str, text: &str, score: f32| EnrichedResult {
            score,
            chunk: f1b_chunk(id, id, text),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        };
        let source = |id: &str| match id {
            "codex-a" | "codex-b" => Some("codex_rollout".to_string()),
            "claude-a" => Some("conversation".to_string()),
            "other" => Some("sidechain".to_string()),
            _ => None,
        };
        let mut rows = vec![
            row("codex-a", "the Answer  text", 0.9),
            row("other", "something else", 0.8),
            row("claude-a", "the answer text", 0.7),
            row("codex-b", "unmatched codex text", 0.6),
        ];
        let collapsed = collapse_cross_source_duplicates(&mut rows, source);
        assert_eq!(collapsed, 1);
        let ids: Vec<&str> = rows.iter().map(|r| r.chunk.id.as_str()).collect();
        assert_eq!(ids, ["claude-a", "other", "codex-b"]);
        assert_eq!(rows[0].score, 0.9);
        // A Codex copy with no Claude twin stays.
        let mut alone = vec![row("codex-b", "unmatched codex text", 0.6)];
        assert_eq!(collapse_cross_source_duplicates(&mut alone, source), 0);
        assert_eq!(alone.len(), 1);
    }

    #[tokio::test]
    async fn f1b_dedupe_refills_slot() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let text = "refilltoken refilltoken refilltoken";
        let codex = f1b_chunk("f1b-codex", "codex-session", text);
        storage
            .insert_chunk_with_source(&codex, &[0.0; 4], "codex_rollout")
            .unwrap();
        let claude = f1b_chunk("f1b-claude", "claude-session", text);
        storage
            .insert_chunk_with_source(&claude, &[0.0; 4], "conversation")
            .unwrap();
        for index in 0..2 {
            let extra = f1b_chunk(
                &format!("f1b-extra-{index}"),
                &format!("f1b-extra-conversation-{index}"),
                "refilltoken with lower density filler words for an additional result",
            );
            storage.insert_chunk(&extra, &[0.0; 4]).unwrap();
        }

        let output = f1b_fts_search(&storage, "refilltoken", 2).await;
        assert!(output.contains("<count>2</count>"), "{output}");
        assert!(output.contains("<id>f1b-claude</id>"), "{output}");
        assert!(!output.contains("<id>f1b-codex</id>"), "{output}");
        assert!(output.contains("<id>f1b-extra-"), "{output}");
        assert!(
            output.contains("<cross-source-collapsed>1</cross-source-collapsed>"),
            "{output}"
        );
    }

    #[test]
    fn f1b_status_recency_stage_reads_stored_author_and_subject() {
        use crate::provenance::Speaker;
        let storage = Storage::open_memory().unwrap();
        let mut rows: Vec<EnrichedResult> = Vec::new();
        let mut keys: HashMap<String, String> = HashMap::new();
        for (id, session, when, author, text) in [
            (
                "old-report",
                "s-old",
                "2026-09-13T10:00:00Z",
                Speaker::Assistant,
                "Vidhi maestro hardening workflow: launched, sweeps pending",
            ),
            (
                "new-tool",
                "s-tool",
                "2026-10-01T10:00:00Z",
                Speaker::ToolResult,
                "vidhi maestro hardening workflow log tail",
            ),
            (
                "new-report",
                "s-new",
                "2026-09-20T10:00:00Z",
                Speaker::Assistant,
                "Vidhi maestro hardening workflow: done, two flows left",
            ),
            (
                "new-offtopic",
                "s-off",
                "2026-10-02T10:00:00Z",
                Speaker::Assistant,
                "unrelated note about radio stations",
            ),
        ] {
            let mut chunk = f1b_chunk(id, session, text);
            chunk.timestamp = when.into();
            chunk.author = author;
            storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
            storage
                .insert_chunk_provenance(
                    id,
                    &crate::provenance::ChunkProvenance {
                        author,
                        source_conv_id: String::new(),
                        supersedes: None,
                    },
                )
                .unwrap();
            keys.insert(id.into(), session.into());
            rows.push(EnrichedResult {
                score: 0.5,
                chunk,
                resolution: None,
                trust: crate::provenance::TrustTier::Unknown,
                validity_demoted: false,
            });
        }
        let ce: HashMap<String, f32> = [
            ("s-old", 10.0),
            ("s-tool", 9.9),
            ("s-new", 9.9),
            ("s-off", 9.9),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let order: Vec<String> = ["old-report", "new-tool", "new-report", "new-offtopic"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let status =
            "what is the current state of the Vidhi maestro hardening workflow and what is left";

        let out = apply_status_recency(&storage, status, order.clone(), &rows, &keys, &ce, 4);
        // The newer report takes the older report's slot; the tool row and the
        // off-topic row hold theirs.
        assert_eq!(
            out,
            ["new-report", "new-tool", "old-report", "new-offtopic"]
        );
        // Not a status query: untouched.
        let plain = "vidhi maestro hardening workflow notes";
        assert_eq!(
            apply_status_recency(&storage, plain, order.clone(), &rows, &keys, &ce, 4),
            order
        );
        // No cross-encoder scores: untouched.
        assert_eq!(
            apply_status_recency(
                &storage,
                status,
                order.clone(),
                &rows,
                &keys,
                &HashMap::new(),
                4
            ),
            order
        );
    }

    /// Insert `id` with stored `author` into `storage`; return its enriched row.
    fn f3b_put(
        storage: &Storage,
        id: &str,
        session: &str,
        when: &str,
        author: crate::provenance::Speaker,
        text: &str,
    ) -> EnrichedResult {
        let mut chunk = f1b_chunk(id, session, text);
        chunk.timestamp = when.into();
        chunk.author = author;
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author,
                    source_conv_id: String::new(),
                    supersedes: None,
                },
            )
            .unwrap();
        EnrichedResult {
            score: 0.5,
            chunk,
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        }
    }

    const F3B_STATUS: &str =
        "what is the current state of the Vidhi Android maestro hardening workflow and what is left";

    /// A session whose first-stage row is an old report, with a newer report, a
    /// newer harness tail and a newer tool row outside the first stage.
    fn f3b_session() -> (Arc<Storage>, Vec<EnrichedResult>) {
        use crate::provenance::Speaker;
        let storage = Arc::new(Storage::open_memory().unwrap());
        let report = "Vidhi Android maestro hardening: sweeps status";
        let first = f3b_put(
            &storage,
            "old-report",
            "s1",
            "2026-09-15T02:32:22Z",
            Speaker::User,
            report,
        );
        f3b_put(
            &storage,
            "new-report",
            "s1",
            "2026-09-15T04:58:24Z",
            Speaker::Assistant,
            "Prod is live. Vidhi Android maestro receipts: green",
        );
        f3b_put(
            &storage,
            "harness-tail",
            "s1",
            "2026-09-15T05:00:30Z",
            Speaker::User,
            "vidhi Android maestro PASS\n</result>\n</task-notification>",
        );
        f3b_put(
            &storage,
            "tool-newest",
            "s1",
            "2026-09-15T05:10:00Z",
            Speaker::ToolResult,
            report,
        );
        (storage, vec![first])
    }

    #[allow(clippy::too_many_arguments)]
    fn f3b_admit(
        storage: &Arc<Storage>,
        query: &str,
        enriched: &mut Vec<EnrichedResult>,
    ) -> StatusAdmission {
        let fused: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
        let provenance: HashMap<String, Option<crate::provenance::ChunkProvenance>> = fused
            .iter()
            .map(|id| (id.clone(), storage.get_chunk_provenance(id).unwrap()))
            .collect();
        let mut signals = CandidateSignals::load(storage, &[], false, ConsumptionMode::Off);
        status_pool_admission(
            storage,
            query,
            enriched,
            &fused,
            &provenance,
            &mut signals,
            &unscoped_search_scope(),
            false,
            RetrievalPolicy::CrossEncoder,
            false,
            ConsumptionMode::Off,
            &chrono::Utc::now(),
        )
    }

    #[test]
    fn f3b_status_query_admits_a_newer_same_session_report_outside_the_page() {
        let (storage, mut enriched) = f3b_session();
        let admission = f3b_admit(&storage, F3B_STATUS, &mut enriched);
        // The newer report was outside the first stage and joins the pool. The
        // tool row is never read; the harness tail is read but dropped.
        assert_eq!(admission.admitted, ["new-report"], "{admission:?}");
        let ids: HashSet<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert!(ids.contains("new-report") && !ids.contains("harness-tail"));
        // Admitted rows are keyword-lane rows with a row of their own.
        let row = enriched
            .iter()
            .find(|e| e.chunk.id == "new-report")
            .unwrap();
        assert!(row.chunk.content.starts_with("[keyword] "));
        // The pool takes them on top of the window.
        let mut pool = ce_pool_ids(&["old-report".into()], &[], CE_POOL);
        pool.extend(admission.admitted.iter().cloned());
        assert_eq!(pool, ["old-report", "new-report"]);
    }

    #[test]
    fn f3b_status_newest_eligible_chunk_represents_its_session() {
        let (storage, mut enriched) = f3b_session();
        let admission = f3b_admit(&storage, F3B_STATUS, &mut enriched);
        // The harness tail is newer, but the harness wrote it: the newest
        // eligible report is the assistant chunk.
        assert_eq!(
            admission.picks.get("s1").map(String::as_str),
            Some("new-report")
        );
        let keys: HashMap<String, String> = enriched
            .iter()
            .map(|e| (e.chunk.id.clone(), "s1".to_string()))
            .collect();
        let swapped = apply_status_picks(
            vec!["old-report".into()],
            &keys,
            &admission.picks,
            &HashSet::new(),
        );
        assert_eq!(swapped, ["new-report"]);
        // A superseded pick never stands.
        let sunk: HashSet<String> = ["new-report".to_string()].into();
        assert_eq!(
            apply_status_picks(vec!["old-report".into()], &keys, &admission.picks, &sunk),
            ["old-report"]
        );
    }

    #[test]
    fn f3b_status_non_status_query_changes_nothing() {
        let (storage, mut enriched) = f3b_session();
        let before: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
        let plain = "vidhi android maestro hardening workflow notes";
        let admission = f3b_admit(&storage, plain, &mut enriched);
        assert!(admission.admitted.is_empty() && admission.picks.is_empty());
        let after: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
        assert_eq!(before, after);
        let order = vec!["old-report".to_string()];
        let keys: HashMap<String, String> = [("old-report".to_string(), "s1".to_string())].into();
        assert_eq!(
            apply_status_picks(order.clone(), &keys, &admission.picks, &HashSet::new()),
            order
        );
        // A status query under the legacy policy admits nothing either.
        let fused = before.clone();
        let mut signals = CandidateSignals::load(&storage, &[], false, ConsumptionMode::Off);
        let legacy = status_pool_admission(
            &storage,
            F3B_STATUS,
            &mut enriched,
            &fused,
            &HashMap::new(),
            &mut signals,
            &unscoped_search_scope(),
            false,
            RetrievalPolicy::Legacy,
            false,
            ConsumptionMode::Off,
            &chrono::Utc::now(),
        );
        assert!(legacy.admitted.is_empty() && legacy.picks.is_empty());
        assert_eq!(enriched.len(), before.len());
    }

    #[test]
    fn f3b_status_harness_or_tool_result_row_is_never_the_representative() {
        use crate::provenance::Speaker;
        let storage = Arc::new(Storage::open_memory().unwrap());
        let first = f3b_put(
            &storage,
            "report",
            "s1",
            "2026-09-15T02:00:00Z",
            Speaker::Assistant,
            "Vidhi Android maestro hardening: status",
        );
        // Newer, same subject, but the harness wrote one and the other is tool
        // output: neither may stand for the session.
        f3b_put(
            &storage,
            "harness-only",
            "s1",
            "2026-09-15T05:00:30Z",
            Speaker::User,
            "<task-notification> Vidhi Android maestro PASS </task-notification>",
        );
        let tool = f3b_put(
            &storage,
            "tool-newer",
            "s1",
            "2026-09-15T06:00:00Z",
            Speaker::ToolResult,
            "Vidhi Android maestro log",
        );
        let mut enriched = vec![first, tool];
        let admission = f3b_admit(&storage, F3B_STATUS, &mut enriched);
        assert!(admission.admitted.is_empty(), "{admission:?}");
        assert_eq!(
            admission.picks.get("s1").map(String::as_str),
            Some("report")
        );
        assert!(!enriched.iter().any(|e| e.chunk.id == "harness-only"));
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
            "rrfconflictword keyword-only result",
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
                "rrfconflictword",
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
    async fn hybrid_identifier_query_lifts_the_exact_name_through_the_postings_channel() {
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
            "id-semantic",
            "id-semantic-conversation",
            "true semantic result without the name",
        );
        let named = make_chunk(
            "id-named",
            "id-named-conversation",
            "rrf_conflict_identifier keyword-only result",
        );
        let semantic_vec = vec![1.0, 0.0, 0.0, 0.0];
        let named_vec = vec![0.0, 1.0, 0.0, 0.0];
        storage.insert_chunk(&semantic, &semantic_vec).unwrap();
        storage.insert_chunk(&named, &named_vec).unwrap();
        let mut engine = SearchEngine::new(8);
        engine.insert_chunk(semantic.id.clone(), semantic_vec);
        engine.insert_chunk(named.id.clone(), named_vec);
        let search = Arc::new(RwLock::new(engine));

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
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();

        assert!(output.contains("<id>id-named</id>"), "{output}");
        assert!(!output.contains("<id>id-semantic</id>"), "{output}");
    }

    #[tokio::test]
    async fn hybrid_returns_one_row_per_session_with_its_best_chunk() {
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
        let chunks = [
            make_chunk(
                "sess-a-1",
                "sess-a",
                "sessionword first note about sessionword",
            ),
            make_chunk("sess-a-2", "sess-a", "sessionword second note elsewhere"),
            make_chunk("sess-b-1", "sess-b", "sessionword third note"),
        ];
        let vectors = [
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.9, 0.1, 0.0, 0.0],
            vec![0.8, 0.2, 0.0, 0.0],
        ];
        let mut engine = SearchEngine::new(8);
        for (chunk, vector) in chunks.iter().zip(vectors.iter()) {
            storage.insert_chunk(chunk, vector).unwrap();
            engine.insert_chunk(chunk.id.clone(), vector.clone());
        }
        let search = Arc::new(RwLock::new(engine));

        let output = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "sessionword",
            10,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();

        assert_eq!(output.matches("<cid>sess-a</cid>").count(), 1, "{output}");
        assert_eq!(output.matches("<cid>sess-b</cid>").count(), 1, "{output}");
        assert!(output.contains("<id>sess-a-1</id>"), "{output}");
        assert!(!output.contains("<id>sess-a-2</id>"), "{output}");
    }

    /// Recall over a two-session fixture, with the perf block (wall-clock
    /// times) removed so two runs compare byte for byte.
    async fn episode_fixture_page(
        storage: &Arc<Storage>,
        search: &Arc<RwLock<SearchEngine>>,
    ) -> String {
        let page = reflect_on_past_with_vec_mode(
            storage,
            search,
            &[1.0, 0.0, 0.0, 0.0],
            "sessionword",
            10,
            0.1,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        match (page.find("<perf>"), page.find("</perf>")) {
            (Some(start), Some(end)) => format!("{}{}", &page[..start], &page[end..]),
            _ => page,
        }
    }

    fn put_episode(storage: &Storage, conversation: &str, ordinal: i64, text: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO episodes (id, conversation_id, project_name, ordinal,
                         first_uuid, last_uuid, timestamp, text, token_est, echo_class)
                     VALUES (?1, ?2, 'test', ?3, 'f', 'l', ?4, ?5, 10, 0)",
                    rusqlite::params![
                        format!("ep:{conversation}:{ordinal}"),
                        conversation,
                        ordinal,
                        chrono::Utc::now().to_rfc3339(),
                        text
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn episode_fixture() -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let now = chrono::Utc::now().to_rfc3339();
        let mut engine = SearchEngine::new(8);
        let rows = [
            (
                "sess-a-1",
                "sess-a",
                "sessionword first note",
                vec![1.0, 0.0, 0.0, 0.0],
            ),
            (
                "sess-b-1",
                "sess-b",
                "sessionword third note",
                vec![0.8, 0.2, 0.0, 0.0],
            ),
        ];
        for (id, conversation, content, vector) in rows {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation.into(),
                project_name: "test".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(chunk.id.clone(), vector);
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn kill_switch_matches_s5_output() {
        let _guard = crate::search::episodes::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_EPISODES");
        std::env::set_var("CSR_EPISODES", "1");
        let (storage, search) = episode_fixture();
        // Pre-change path: no episodes at all.
        let before = episode_fixture_page(&storage, &search).await;
        assert!(!before.contains("session=\""), "{before}");

        put_episode(&storage, "sess-c", 0, "sessionword episode only detail");
        put_episode(&storage, "sess-a", 0, "sessionword first note");

        std::env::set_var("CSR_NO_EPISODES", "1");
        let killed = episode_fixture_page(&storage, &search).await;
        std::env::remove_var("CSR_NO_EPISODES");
        assert_eq!(killed, before, "kill switch must equal the S5 path");

        // Channel on: the episode-only session appears with its eid, and a
        // session with chunks keeps its best chunk id.
        let on = episode_fixture_page(&storage, &search).await;
        assert!(
            on.contains("session=\"sess-c\" eid=\"ep:sess-c:0\""),
            "{on}"
        );
        assert!(on.contains("session=\"sess-a\""), "{on}");
        assert!(on.contains("<id>sess-a-1</id>"), "{on}");
        assert_eq!(on.matches("session=\"sess-a\"").count(), 1, "{on}");

        // An empty episodes table is the S5 path as well.
        storage
            .with_connection(|conn| {
                conn.execute("DELETE FROM episodes", [])?;
                Ok(())
            })
            .unwrap();
        let emptied = episode_fixture_page(&storage, &search).await;
        assert_eq!(
            emptied, before,
            "an empty episodes table must equal the S5 path"
        );
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
            csr_only_demote: false,
        }
    }

    fn annotate_validity(note: &str) -> ConvValidity {
        ConvValidity {
            demote: false,
            note: note.to_string(),
            csr_only_demote: false,
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
            RetrievalPolicy::Legacy,
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
            RetrievalPolicy::Legacy,
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
            RetrievalPolicy::Legacy,
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
            RetrievalPolicy::Legacy,
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
            RetrievalPolicy::Legacy,
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
        let missing = score_fts_candidate(
            &chunk,
            &now,
            &HashMap::new(),
            None,
            false,
            RetrievalPolicy::Legacy,
        );
        let current = score_fts_candidate(
            &chunk,
            &now,
            &HashMap::new(),
            Some(&current_release),
            false,
            RetrievalPolicy::Legacy,
        );

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
            RetrievalPolicy::Legacy,
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
            RetrievalPolicy::Legacy,
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
            RetrievalPolicy::Legacy,
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
            RetrievalPolicy::Legacy,
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

    fn csr_text_result(
        id: &str,
        conversation: &str,
        author: crate::provenance::Speaker,
        content: &str,
        score: f32,
        seq: usize,
    ) -> EnrichedResult {
        let mut result = enriched_result_conv(id, conversation, score, seq);
        result.chunk.author = author;
        result.chunk.content = content.into();
        result
    }

    const CSR_RECAP_TEXT: &str = "recap [2m ago]: Fixed the importer. [[CSR:RECAP]]";

    /// Label, never delete: a result whose chunk holds CSR's own output carries
    /// a `[CSR text]` note in every mode (the dream switch does not turn it
    /// off), is kept whole, and sinks below the clean results in every mode.
    #[test]
    fn a_result_holding_csr_text_is_labeled_kept_and_sunk_in_every_mode() {
        use crate::provenance::Speaker;
        let storage = Arc::new(Storage::open_memory().unwrap());
        let results = || {
            vec![
                csr_text_result("csr", "conv-a", Speaker::User, CSR_RECAP_TEXT, 0.95, 0),
                csr_text_result("clean", "conv-b", Speaker::User, "ordinary work", 0.90, 1),
                csr_text_result(
                    "tool",
                    "conv-c",
                    Speaker::ToolResult,
                    CSR_RECAP_TEXT,
                    0.85,
                    2,
                ),
                csr_text_result("plan", "plan:x", Speaker::User, CSR_RECAP_TEXT, 0.80, 3),
            ]
        };
        for (mode, order) in [
            (ConsumptionMode::Full, ["clean", "tool", "plan", "csr"]),
            (
                ConsumptionMode::AnnotateOnly,
                ["clean", "tool", "plan", "csr"],
            ),
            (ConsumptionMode::Off, ["clean", "tool", "plan", "csr"]),
        ] {
            let mut enriched = results();
            let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();
            // stored with their authors, as the importer writes them: a chunk
            // with no provenance row at all would be a legacy chunk of unknown author
            for chunk in &chunks {
                let _ = storage.insert_chunk(chunk, &[0.0; 4]);
                let _ = storage.insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: chunk.author,
                        source_conv_id: chunk.conversation_id.clone(),
                        supersedes: None,
                    },
                );
            }
            let validity = resolve_validity_with(&storage, &chunks, mode);
            apply_validity_partition(&mut enriched, &validity, false);

            let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
            assert_eq!(ids, order, "{mode:?}");
            for e in &enriched {
                let labeled = e
                    .resolution
                    .as_deref()
                    .is_some_and(|r| r.starts_with("[CSR text]"));
                assert_eq!(labeled, e.chunk.id == "csr", "{mode:?} {}", e.chunk.id);
                // never counted as a dream verdict, never edited
                assert!(!e.validity_demoted);
            }
            assert_eq!(
                enriched
                    .iter()
                    .find(|e| e.chunk.id == "csr")
                    .unwrap()
                    .chunk
                    .content,
                CSR_RECAP_TEXT
            );
        }
    }

    #[test]
    fn a_csr_text_label_joins_a_dream_note_instead_of_replacing_it() {
        use crate::provenance::Speaker;
        let storage = Arc::new(Storage::open_memory().unwrap());
        let chunk = csr_text_result("c", "conv", Speaker::Assistant, CSR_RECAP_TEXT, 0.9, 0).chunk;
        let mut validity: HashMap<String, ConvValidity> = [(
            "c".to_string(),
            demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
        )]
        .into_iter()
        .collect();
        add_csr_text_labels(&mut validity, &storage, &[chunk]);
        let note = &validity["c"].note;
        assert!(note.starts_with("[stale anchor]"), "{note}");
        assert!(note.contains("[CSR text]"), "{note}");
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
        // Only the base project directory exists, so the `-csr-engine` namespace is an
        // alias of it; the real `$HOME/projects` plays no part.
        let projects = tempfile::tempdir().unwrap();
        std::fs::create_dir(projects.path().join("claude-self-reflect")).unwrap();
        let _root = crate::search::cross_project::ProjectsRootGuard::set(projects.path());
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

        let out = get_full_conversation(dir.path(), None, "abc123", None);
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
    #[tokio::test]
    async fn a_stored_reflection_deleted_before_its_index_insert_stays_out_of_the_index() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(8)));
        let _race = crate::storage::lock_probe::delete_reflection_then_sync(
            storage.clone(),
            search.clone(),
        );
        store_embedded_reflection(
            &storage,
            &search,
            "gone".into(),
            "quartzgone",
            &[],
            vec![0.1; 384],
        )
        .await
        .unwrap();
        assert!(!search.read().await.has_reflection("gone"));
    }

    #[tokio::test]
    async fn a_failed_fence_read_stops_a_reflection_before_it_commits() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(8)));
        let _fail = crate::storage::lock_probe::fail_fence_reads();
        assert!(store_embedded_reflection(
            &storage,
            &search,
            "x".into(),
            "quartzx",
            &[],
            vec![0.1; 384]
        )
        .await
        .is_err());
        assert_eq!(storage.count_reflection_embeddings().unwrap(), 0);
    }

    #[tokio::test]
    async fn r3_missing_store_ack_reports_pending_id_and_reflection_fts_finds_it() {
        use crate::search::vector_ipc::{ProxyTimings, VectorClient, VectorProxy};
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let storage = Arc::new(Storage::open(&db).unwrap());
        let proxy = Arc::new(VectorProxy::new(
            VectorClient::for_database(&db).unwrap(),
            storage.clone(),
        ));
        proxy.set_timings(ProxyTimings {
            backoff_first: std::time::Duration::from_millis(1),
            backoff_total: std::time::Duration::from_millis(5),
            down_window: std::time::Duration::from_secs(30),
        });
        let search = Arc::new(RwLock::new(SearchEngine::proxied(proxy)));
        let reply = store_embedded_reflection(
            &storage,
            &search,
            "pending-id".into(),
            "quartzpending acknowledgment",
            &[],
            vec![0.1; 384],
        )
        .await
        .unwrap();
        assert!(reply.contains("semantic indexing is pending"), "{reply}");
        assert!(reply.contains("ID: pending-id"));
        assert!(!reply.contains("successfully"));
        assert!(storage
            .fts5_search("quartzpending", 10, None)
            .unwrap()
            .is_empty());
        assert_eq!(
            storage
                .fts5_search_reflections("quartzpending", 10)
                .unwrap()[0]
                .0,
            "pending-id"
        );
    }

    #[tokio::test]
    async fn r3_keyword_fallback_preserves_dates_scope_pagination_and_formats() {
        let rows: Vec<_> = (0..8)
            .map(|n| (format!("id{n}"), format!("quartzneedle unique{n}")))
            .collect();
        let input: Vec<_> = rows
            .iter()
            .map(|(id, text)| (id.as_str(), "alpha", text.as_str(), [1.0, 0.0, 0.0, 0.0]))
            .collect();
        let (storage, search) = quick_check_fixture(&input);
        storage
            .with_connection(|conn| {
                conn.execute("UPDATE chunks SET timestamp='2026-10-02T00:00:00Z'", [])?;
                conn.execute(
                    "UPDATE chunks SET timestamp='2020-01-01T00:00:00Z' WHERE id='id0'",
                    [],
                )?;
                conn.execute("UPDATE chunks SET project_name='beta' WHERE id='id1'", [])?;
                Ok(())
            })
            .unwrap();
        let candidates = keyword_candidates(&storage, "quartzneedle", Some("alpha"), None).unwrap();
        assert_eq!(candidates.len(), 7);
        assert!(candidates.iter().all(|r| r.chunk.project_name == "alpha"));
        let page = keyword_fallback(
            &storage,
            &search,
            "quartzneedle",
            3,
            Some("alpha"),
            KeywordTool::More { offset: 3 },
        )
        .await
        .unwrap();
        assert!(page.contains("<offset>3</offset>"), "{page}");
        assert!(
            page.contains("<results_returned>3</results_returned>"),
            "{page}"
        );
        for row in candidates.iter().skip(3).take(3) {
            assert!(page.contains(&row.chunk.content));
        }
        for row in candidates.iter().take(3) {
            assert!(!page.contains(&row.chunk.content));
        }
        let recency = keyword_fallback(
            &storage,
            &search,
            "quartzneedle",
            10,
            Some("alpha"),
            KeywordTool::Recency {
                time_range: None,
                since: Some("2026-10-01"),
                until: Some("2026-10-03"),
            },
        )
        .await
        .unwrap();
        assert!(!recency.contains("unique0"), "{recency}");
        assert!(!recency.contains("unique1"), "{recency}");
        assert!(recency.contains("unique2"), "{recency}");
        for (tool, marker) in [
            (KeywordTool::Quick, "<quick_search>"),
            (KeywordTool::Insights, "<search_summary>"),
            (KeywordTool::Why, "WHY: quartzneedle"),
        ] {
            let reply = keyword_fallback(&storage, &search, "quartzneedle", 5, Some("alpha"), tool)
                .await
                .unwrap();
            assert!(reply.starts_with(marker), "{reply}");
            assert!(!reply.contains("unique1"));
        }
        assert!(validate_page(usize::MAX, 1).is_err());
        assert!(validate_page(0, usize::MAX).is_err());
    }

    #[tokio::test]
    async fn r3_recency_fts_filters_before_candidate_limit() {
        let mut rows: Vec<_> = (0..500)
            .map(|n| (format!("old{n}"), "quartzdated".to_string()))
            .collect();
        rows.push((
            "recent".into(),
            format!("quartzdated {}", "padding ".repeat(100)),
        ));
        rows.push(("unrelated".into(), "quartzdated".into()));
        let input: Vec<_> = rows
            .iter()
            .map(|(id, text)| (id.as_str(), "alpha", text.as_str(), [1.0, 0.0, 0.0, 0.0]))
            .collect();
        let (storage, search) = quick_check_fixture(&input);
        storage.with_connection(|conn| {
            conn.execute("UPDATE chunks SET timestamp='2020-01-01T00:00:00Z'", [])?;
            conn.execute("UPDATE chunks SET timestamp='2026-10-02T00:00:00Z' WHERE id IN ('recent', 'unrelated')", [])?;
            conn.execute("UPDATE chunks SET project_name='beta' WHERE id='unrelated'", [])?;
            Ok(())
        }).unwrap();
        assert!(storage
            .fts5_search("quartzdated", 500, Some("alpha"))
            .unwrap()
            .iter()
            .all(|(chunk, _, _)| chunk.id != "recent"));
        for project in [None, Some("alpha")] {
            let reply = keyword_fallback(
                &storage,
                &search,
                "quartzdated",
                10,
                project,
                KeywordTool::Recency {
                    time_range: None,
                    since: Some("2026-10-01"),
                    until: Some("2026-10-03"),
                },
            )
            .await
            .unwrap();
            assert!(reply.contains("padding"), "{reply}");
            assert!(!reply.contains("conv-old"), "{reply}");
            if project.is_some() {
                assert!(!reply.contains("conv-unrelated"), "{reply}");
            }
        }
    }

    #[tokio::test]
    async fn r3_keyword_fallback_formatters_omit_semantic_confidence() {
        let (storage, search) =
            quick_check_fixture(&[("lexical", "alpha", "quartzlexical", [1.0, 0.0, 0.0, 0.0])]);
        for tool in [
            KeywordTool::Concept,
            KeywordTool::Insights,
            KeywordTool::Quick,
            KeywordTool::More { offset: 0 },
            KeywordTool::Recency {
                time_range: None,
                since: Some("2000-01-01"),
                until: Some("2100-01-01"),
            },
        ] {
            let reply =
                keyword_fallback(&storage, &search, "quartzlexical", 5, Some("alpha"), tool)
                    .await
                    .unwrap();
            assert!(reply.contains("keyword"), "{reply}");
            for forbidden in [
                "0.000",
                "weak",
                "calibrat",
                "<average_score>",
                "<range>",
                "<s>",
            ] {
                assert!(!reply.contains(forbidden), "{forbidden}: {reply}");
            }
        }
    }

    #[tokio::test]
    async fn r3_keyword_pagination_stays_inside_the_500_candidate_window() {
        let rows: Vec<_> = (0..510)
            .map(|n| (format!("id{n:04}"), format!("quartzwindow unique{n:04}")))
            .collect();
        let input: Vec<_> = rows
            .iter()
            .map(|(id, text)| (id.as_str(), "alpha", text.as_str(), [1.0, 0.0, 0.0, 0.0]))
            .collect();
        let (storage, search) = quick_check_fixture(&input);
        let page = keyword_fallback(
            &storage,
            &search,
            "quartzwindow",
            10,
            Some("alpha"),
            KeywordTool::More { offset: 499 },
        )
        .await
        .unwrap();
        assert!(
            page.contains("<total_available>500</total_available>"),
            "{page}"
        );
        assert!(
            page.contains("<results_returned>1</results_returned>"),
            "{page}"
        );
    }

    // ---- S2: cross-encoder stage, pure pieces -------------------------------

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|id| id.to_string()).collect()
    }

    fn keys(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, session)| (id.to_string(), session.to_string()))
            .collect()
    }

    fn row(id: &str, session: &str, class: u8, score: f32) -> (String, String, u8, f32) {
        (id.into(), session.into(), class, score)
    }

    // ---- S8: assertions partition, tools-level --------------------------------

    fn assertion_decision() -> crate::storage::assertions::UserDecision {
        crate::storage::assertions::UserDecision {
            key: "merge-mode".into(),
            subject_terms: vec!["admin".into(), "merge".into()],
            superseded_phrases: vec!["carrier branch".into()],
            current_phrases: vec!["--squash works".into()],
            current_claim: "ANSWER-TEXT-THAT-MUST-NOT-APPEAR".into(),
            decided_at: "2026-08-09".into(),
            receipt: "rcpt-decision".into(),
        }
    }

    fn assertion_row(id: &str, conv: &str, content: &str) -> EnrichedResult {
        let mut row = enriched_result_conv(id, conv, 0.5, 0);
        row.chunk.content = content.into();
        row
    }

    /// The Hybrid arm's partition steps around the collapse, without the
    /// cross-encoder: filter the lists, collapse, trail the sunk sessions.
    fn collapse_with_partition(
        lists: &[&[&str]; 2],
        sunk: &HashSet<String>,
        session_keys: &HashMap<String, String>,
    ) -> Vec<String> {
        let semantic = ids(lists[0]);
        let fts = ids(lists[1]);
        let fused =
            crate::search::hybrid::fuse_rrf_weighted(&[(&semantic, 1.0), (&fts, 1.0)], RRF_K);
        let semantic = assertions_partition::without_sunk(&semantic, sunk);
        let fts = assertions_partition::without_sunk(&fts, sunk);
        let session_of = |id: &str| {
            session_keys
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        };
        let collapsed =
            collapse_to_sessions_by(&[(&semantic, 1.0), (&fts, 1.0)], session_of, |_| 0, RRF_K);
        assertions_partition::trail_sunk(collapsed, &fused, sunk, session_of)
    }

    #[test]
    fn superseded_best_chunk_loses_the_session_slot_and_a_sunk_session_trails() {
        // sA: a1 (best, superseded) and a2. sB: b1. sC: c1, c2 (both superseded).
        let session_keys = keys(&[
            ("a1", "sA"),
            ("a2", "sA"),
            ("b1", "sB"),
            ("c1", "sC"),
            ("c2", "sC"),
        ]);
        let lists: [&[&str]; 2] = [&["c1", "a1", "c2", "b1", "a2"], &["a1", "c1", "b1"]];
        let none = HashSet::new();
        let before = collapse_with_partition(&lists, &none, &session_keys);
        // Unpartitioned, a1 holds sA's slot and sA's other chunk is not shown.
        assert!(before.contains(&"a1".to_string()), "{before:?}");
        assert!(!before.contains(&"a2".to_string()), "{before:?}");
        let sunk: HashSet<String> = ["a1", "c1", "c2"].iter().map(|s| s.to_string()).collect();
        let after = collapse_with_partition(&lists, &sunk, &session_keys);
        // sB (b1) and sA (now its next chunk, a2) lead; sC trails alone.
        assert_eq!(after, ids(&["b1", "a2", "c1"]));
    }

    #[test]
    fn nothing_sunk_leaves_the_collapse_untouched() {
        let session_keys = keys(&[("a1", "sA"), ("a2", "sA"), ("b1", "sB")]);
        let lists: [&[&str]; 2] = [&["a1", "b1", "a2"], &["b1", "a1"]];
        let semantic = ids(lists[0]);
        let fts = ids(lists[1]);
        let direct = collapse_to_sessions_by(
            &[(&semantic, 1.0), (&fts, 1.0)],
            |id| {
                session_keys
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_string())
            },
            |_| 0,
            RRF_K,
        );
        let staged = collapse_with_partition(&lists, &HashSet::new(), &session_keys);
        assert_eq!(staged, direct);
    }

    #[test]
    fn assertion_stage_sinks_a_claim_match_and_notes_it_without_claim_text() {
        let storage = Storage::open_memory().unwrap();
        storage.rebuild_assertions(&[assertion_decision()]).unwrap();
        let claims = storage.claim_assertions().unwrap();
        assert_eq!(claims.len(), 1);
        let mut rows = vec![
            assertion_row("old", "convA", "merge needs a carrier branch for admin"),
            assertion_row("new", "convB", "admin merge: --squash works for admin"),
            assertion_row("other", "convC", "unrelated"),
        ];
        rows[2].resolution = Some("existing note".into());
        let session_keys = keys(&[("old", "convA"), ("new", "convB"), ("other", "convC")]);
        let sunk =
            run_assertions_stage_with(&storage, &claims, &mut rows, &session_keys, &HashMap::new());
        assert_eq!(sunk, HashSet::from(["old".to_string()]));
        let note = rows[0].resolution.as_deref().unwrap();
        assert_eq!(note, "[superseded] merge-mode (receipt rcpt-decision)");
        assert!(!note.contains("ANSWER-TEXT"), "{note}");
        assert!(rows[1].resolution.is_none());
        assert_eq!(rows[2].resolution.as_deref(), Some("existing note"));
    }

    #[test]
    fn assertion_notes_join_existing_and_skip_a_repeated_witness_verdict() {
        use crate::storage::assertions::Assertion;
        let witness = Assertion {
            source: "witness".into(),
            target_kind: "chunk".into(),
            target_id: "w".into(),
            state: "superseded".into(),
            claim: None,
            receipt: "abc1234".into(),
        };
        let resolve = Assertion {
            source: "csr_resolve".into(),
            target_id: "r".into(),
            receipt: "fixed".into(),
            ..witness.clone()
        };
        let by_target: HashMap<String, Assertion> =
            [("w".to_string(), witness), ("r".to_string(), resolve)]
                .into_iter()
                .collect();
        let units_rows = || {
            vec![
                assertion_row("w", "cw", "text"),
                assertion_row("r", "cr", "text"),
            ]
        };
        let rows0 = units_rows();
        let units: Vec<assertions_partition::Unit<'_>> = rows0
            .iter()
            .map(|e| assertions_partition::Unit {
                id: &e.chunk.id,
                session: &e.chunk.conversation_id,
                text: &e.chunk.content,
            })
            .collect();
        let outcome = assertions_partition::partition(&units, &by_target, &[]);
        // The chunk "w" already has the dream's stale-anchor note; "r" has an older note.
        let validity: HashMap<String, ConvValidity> = [(
            "w".to_string(),
            demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
        )]
        .into_iter()
        .collect();
        let mut rows = units_rows();
        rows[1].resolution = Some("older".into());
        merge_assertion_notes(&mut rows, &outcome, &by_target, &validity);
        assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
        assert_eq!(
            rows[1].resolution.as_deref(),
            Some("older; [superseded] csr_resolve (receipt fixed)")
        );
        // Without the validity note the witness row is annotated.
        let mut rows = units_rows();
        merge_assertion_notes(&mut rows, &outcome, &by_target, &HashMap::new());
        assert_eq!(
            rows[0].resolution.as_deref(),
            Some("[superseded] witness (receipt abc1234)")
        );
    }

    fn s9r_stage_store(rows: &str) -> Storage {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|c| Ok(c.execute_batch(rows)?))
            .unwrap();
        storage
    }

    fn s9r_run_stage(
        storage: &Storage,
        validity: &HashMap<String, ConvValidity>,
    ) -> (HashSet<String>, Vec<EnrichedResult>) {
        let mut rows = vec![
            assertion_row("x", "convX", "some text"),
            assertion_row("y", "convY", "other text"),
        ];
        let session_keys = keys(&[("x", "convX"), ("y", "convY")]);
        let sunk = run_assertions_stage_with(storage, &[], &mut rows, &session_keys, validity);
        (sunk, rows)
    }

    #[test]
    fn s9r_p1_stale_agent_superseded_row_ignored_at_read() {
        let storage = s9r_stage_store(
            "INSERT INTO assertions (source,target_kind,target_id,state,pattern_json,receipt)
             VALUES ('csr_resolve','chunk','x','superseded',
                     '{\"status\":\"resolved\",\"ledger_source\":\"agent\"}','agent claimed fixed'),
                    ('csr_resolve','chunk','y','superseded',
                     '{\"status\":\"resolved\",\"ledger_source\":\"user_confirmed\"}','owner said fixed');",
        );
        let (sunk, rows) = s9r_run_stage(&storage, &HashMap::new());
        assert_eq!(sunk, HashSet::from(["y".to_string()]));
        assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
    }

    const S9R_WITNESS_X: &str =
        "INSERT INTO assertions (source,target_kind,target_id,state,receipt)
         VALUES ('witness','chunk','x','superseded','abc1234');";

    #[test]
    fn s9r_witness_reinstated_does_not_sink() {
        // A later anchor_reinstated leaves no live verdict: the validity map
        // holds nothing for the chunk, so the cached assertion must not sink it.
        let storage = s9r_stage_store(S9R_WITNESS_X);
        let (sunk, rows) = s9r_run_stage(&storage, &HashMap::new());
        assert!(sunk.is_empty(), "{sunk:?}");
        assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
    }

    #[test]
    fn s9r_witness_annotate_only_mode_does_not_sink() {
        // Annotate-only or off consumption: validity carries the note but no demote.
        let storage = s9r_stage_store(S9R_WITNESS_X);
        let validity: HashMap<String, ConvValidity> = [(
            "x".to_string(),
            ConvValidity {
                demote: false,
                note: "[stale anchor] foo gone (receipt abc1234)".into(),
                csr_only_demote: false,
            },
        )]
        .into_iter()
        .collect();
        let (sunk, rows) = s9r_run_stage(&storage, &validity);
        assert!(sunk.is_empty(), "{sunk:?}");
        assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
    }

    #[test]
    fn s9r_witness_csr_text_demote_does_not_keep_the_assertion() {
        let storage = s9r_stage_store(S9R_WITNESS_X);
        let validity: HashMap<String, ConvValidity> = [(
            "x".to_string(),
            ConvValidity {
                demote: true,
                note: "[CSR text] this chunk holds CSR's own output (f)".into(),
                csr_only_demote: true,
            },
        )]
        .into_iter()
        .collect();
        let (sunk, _) = s9r_run_stage(&storage, &validity);
        assert!(sunk.is_empty(), "{sunk:?}");
    }

    #[test]
    fn s9r_witness_live_demote_still_sinks() {
        let storage = s9r_stage_store(S9R_WITNESS_X);
        let validity: HashMap<String, ConvValidity> = [(
            "x".to_string(),
            demote_validity("[stale anchor] foo gone (receipt abc1234)"),
        )]
        .into_iter()
        .collect();
        let (sunk, rows) = s9r_run_stage(&storage, &validity);
        assert_eq!(sunk, HashSet::from(["x".to_string()]));
        // The dream note already says it; the assertion does not repeat it.
        assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
    }

    #[test]
    fn kill_switch_matches_a_store_with_no_assertions_table() {
        let with_rules = Storage::open_memory().unwrap();
        with_rules
            .rebuild_assertions(&[assertion_decision()])
            .unwrap();
        let claims = with_rules.claim_assertions().unwrap();
        let without_table = Storage::open_memory().unwrap();
        without_table
            .with_connection(|conn| Ok(conn.execute_batch("DROP TABLE assertions")?))
            .unwrap();
        assert!(without_table.claim_assertions().unwrap().is_empty());
        assert!(without_table
            .assertions_for_targets(&["old"], &["convA"])
            .unwrap()
            .is_empty());

        let make = || {
            vec![assertion_row(
                "old",
                "convA",
                "merge needs a carrier branch for admin",
            )]
        };
        let session_keys = keys(&[("old", "convA")]);

        // Switch off: the stage is skipped, whatever the store holds.
        assert!(!assertions_partition::assertions_enabled_from(Some("1")));
        let mut off = make();
        let sunk_off =
            run_assertions_stage(&with_rules, &mut off, &session_keys, &HashMap::new(), false);
        // No table, switch on: nothing sinks, nothing is noted.
        let mut bare = make();
        let sunk_bare = run_assertions_stage_with(
            &without_table,
            &[],
            &mut bare,
            &session_keys,
            &HashMap::new(),
        );
        assert!(sunk_off.is_empty() && sunk_bare.is_empty());
        assert_eq!(off[0].resolution, bare[0].resolution);
        assert!(off[0].resolution.is_none());
        // The same rows with the switch on and rules present do sink.
        let mut on = make();
        let sunk_on = run_assertions_stage_with(
            &with_rules,
            &claims,
            &mut on,
            &session_keys,
            &HashMap::new(),
        );
        assert_eq!(sunk_on.len(), 1);
        assert!(on[0].resolution.is_some());
        // Switch off leaves the lists as they were.
        let order = ids(&["old", "x"]);
        assert_eq!(
            assertions_partition::sunk_to_tail(order.clone(), &sunk_off),
            order
        );
        assert_eq!(assertions_partition::without_sunk(&order, &sunk_off), order);
    }

    #[test]
    fn ce_reorder_orders_pooled_sessions_by_session_max_and_keeps_the_rest() {
        // S1 order: sA, sB, sC, sD. The pool scored sA once, sB twice (its
        // second chunk is the better one) and sC once; sD was not pooled.
        let collapsed = ids(&["a1", "b1", "c1", "d1"]);
        let session_keys = keys(&[("a1", "sA"), ("b1", "sB"), ("c1", "sC"), ("d1", "sD")]);
        let scored = vec![
            row("a1", "sA", 0, 2.0),
            row("b1", "sB", 0, 1.0),
            row("b2", "sB", 0, 5.0),
            row("c1", "sC", 0, 4.0),
        ];
        let ordered = ce_reorder(&collapsed, &session_keys, &scored);
        assert_eq!(ordered.order, ids(&["b2", "c1", "a1", "d1"]));
        assert_eq!(ordered.session_scores, vec![5.0, 4.0, 2.0]);
    }

    #[test]
    fn ce_reorder_ties_keep_the_s1_order_and_answer_rows_speak_for_a_session() {
        let collapsed = ids(&["a1", "b1", "e1"]);
        let session_keys = keys(&[("a1", "sA"), ("b1", "sB"), ("e1", "sE")]);
        let scored = vec![
            row("b1", "sB", 0, 3.0),
            row("a1", "sA", 0, 3.0),
            // sE has an echo that scores high and an answer that scores low:
            // the answer row represents it.
            row("e1", "sE", 1, 9.0),
            row("e2", "sE", 0, 1.0),
        ];
        let ordered = ce_reorder(&collapsed, &session_keys, &scored);
        assert_eq!(ordered.order, ids(&["a1", "b1", "e2"]));
        assert_eq!(ordered.session_scores, vec![3.0, 3.0, 1.0]);
    }

    #[test]
    fn ce_reorder_a_session_with_only_non_answer_rows_is_still_scored() {
        let collapsed = ids(&["a1", "e1"]);
        let session_keys = keys(&[("a1", "sA"), ("e1", "sE")]);
        let scored = vec![row("a1", "sA", 0, 1.0), row("e1", "sE", 2, 6.0)];
        let ordered = ce_reorder(&collapsed, &session_keys, &scored);
        assert_eq!(ordered.order, ids(&["e1", "a1"]));
    }

    #[test]
    fn ce_reorder_skips_non_finite_scores_and_empty_pools_keep_s1() {
        let collapsed = ids(&["a1", "b1"]);
        let session_keys = keys(&[("a1", "sA"), ("b1", "sB")]);
        let scored = vec![row("a1", "sA", 0, f32::NAN), row("b1", "sB", 0, 0.5)];
        let ordered = ce_reorder(&collapsed, &session_keys, &scored);
        assert_eq!(ordered.order, ids(&["b1", "a1"]));
        assert_eq!(ordered.session_scores, vec![0.5]);
        let none = ce_reorder(&collapsed, &session_keys, &[]);
        assert_eq!(none.order, collapsed);
        assert!(none.session_scores.is_empty());
    }

    #[test]
    fn ce_pool_takes_the_first_24_fused_and_always_holds_the_s1_top_five() {
        let fused: Vec<String> = (0..40).map(|n| format!("f{n}")).collect();
        // Five protected representatives: two inside the first 24, three out.
        let protected = ids(&["f2", "f9", "f30", "f35", "zz"]);
        let pool = ce_pool_ids(&fused, &protected, CE_POOL);
        assert_eq!(pool.len(), CE_POOL);
        for id in &protected {
            assert!(pool.contains(id), "{id} missing from {pool:?}");
        }
        // The three forced in displaced the three last unprotected ids.
        for gone in ["f23", "f22", "f21"] {
            assert!(!pool.contains(&gone.to_string()), "{gone} kept: {pool:?}");
        }
        assert!(pool.contains(&"f20".to_string()));
        let distinct: HashSet<&String> = pool.iter().collect();
        assert_eq!(distinct.len(), pool.len());
    }

    #[test]
    fn ce_pool_with_a_short_fused_list_appends_the_protected() {
        let pool = ce_pool_ids(&ids(&["a", "b", "a"]), &ids(&["b", "c"]), CE_POOL);
        assert_eq!(pool, ids(&["a", "b", "c"]));
    }

    #[test]
    fn ce_pair_text_carries_role_and_date_and_drops_the_keyword_marker() {
        let mut chunk = enriched_result_conv("p1", "conv", 0.5, 0).chunk;
        chunk.author = crate::provenance::Speaker::User;
        chunk.timestamp = "2026-09-03T08:15:00Z".into();
        chunk.content = "[keyword] we chose sqlite".into();
        assert_eq!(ce_pair_text(&chunk), "[user, 2026-09-03] we chose sqlite");
        chunk.timestamp = "not a time".into();
        assert_eq!(ce_pair_text(&chunk), "[user] we chose sqlite");
    }

    #[test]
    fn cross_encoder_policy_skips_decay_and_legacy_keeps_it() {
        let now = chrono::Utc::now();
        let mut chunk = enriched_result_conv("old", "conv-old", 0.9, 0).chunk;
        chunk.timestamp = (now - chrono::Duration::days(400)).to_rfc3339();
        let config = decay::DecayConfig::for_search();
        let score = |policy| {
            score_chunk_candidate(
                0.9,
                &chunk,
                &now,
                &[],
                &config,
                &HashMap::new(),
                None,
                false,
                &unscoped_search_scope(),
                policy,
            )
            .0
        };
        assert_eq!(
            score(RetrievalPolicy::CrossEncoder).to_bits(),
            0.9f32.to_bits()
        );
        assert!(score(RetrievalPolicy::Legacy) < 0.9);

        let fts =
            |policy| score_fts_candidate(&chunk, &now, &HashMap::new(), None, false, policy).0;
        assert_eq!(
            fts(RetrievalPolicy::CrossEncoder).to_bits(),
            crate::search::hybrid::FTS_ONLY_BASE_SCORE.to_bits()
        );
        assert!(fts(RetrievalPolicy::Legacy) < crate::search::hybrid::FTS_ONLY_BASE_SCORE);
    }

    /// Opt the current test thread into a retrieval policy; restored on drop.
    struct PolicyGuard;

    fn force_policy(policy: RetrievalPolicy) -> PolicyGuard {
        POLICY_OVERRIDE.with(|cell| cell.set(Some(policy)));
        PolicyGuard
    }

    impl Drop for PolicyGuard {
        fn drop(&mut self) {
            POLICY_OVERRIDE.with(|cell| cell.set(None));
        }
    }

    #[test]
    fn policy_override_is_per_thread_and_tests_default_to_legacy() {
        // No test sets CSR_RETRIEVAL_LEGACY; the override is the test seam.
        assert_eq!(current_retrieval_policy(), RetrievalPolicy::Legacy);
        {
            let _cross_encoder = force_policy(RetrievalPolicy::CrossEncoder);
            assert_eq!(current_retrieval_policy(), RetrievalPolicy::CrossEncoder);
            std::thread::spawn(|| {
                assert_eq!(current_retrieval_policy(), RetrievalPolicy::Legacy);
            })
            .join()
            .unwrap();
        }
        assert_eq!(current_retrieval_policy(), RetrievalPolicy::Legacy);
    }

    #[tokio::test]
    async fn hybrid_recall_under_the_cross_encoder_policy_returns_every_session_and_fails_open() {
        let _policy = force_policy(RetrievalPolicy::CrossEncoder);
        let storage = Arc::new(Storage::open_memory().unwrap());
        let now = chrono::Utc::now().to_rfc3339();
        let mut engine = SearchEngine::new(8);
        let rows = [
            (
                "ce-a",
                "ce-conv-a",
                "we dropped Docker for a single binary",
                [1.0, 0.0, 0.0, 0.0],
            ),
            (
                "ce-b",
                "ce-conv-b",
                "unrelated note about lunch",
                [0.8, 0.6, 0.0, 0.0],
            ),
            (
                "ce-c",
                "ce-conv-c",
                "Qdrant needed a server we never wanted",
                [0.9, 0.0, 0.43, 0.0],
            ),
        ];
        for (id, conversation, content, vector) in rows {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation.into(),
                project_name: "test".into(),
                timestamp: now.clone(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            engine.insert_chunk(id.to_string(), vector.to_vec());
        }
        let search = Arc::new(RwLock::new(engine));
        let output = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "why did we drop Docker",
            3,
            0.0,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        for id in ["ce-a", "ce-b", "ce-c"] {
            assert!(output.contains(&format!("<id>{id}</id>")), "{output}");
        }
        // A scorer on the machine adds the CE attributes; without model files
        // the S1 order stands and the cosine label path is used.
        assert_eq!(
            output.contains("ce-top="),
            cross_encoder::global().is_some(),
            "{output}"
        );
        // No flat boost on this path: the user-authored rows carry no `adj`.
        assert!(!output.contains("adj="), "{output}");
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

#[cfg(test)]
mod health_probe_tests {
    use super::*;
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;

    fn chunk(id: &str, conversation: &str, project: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: project.into(),
            timestamp: "2026-10-10T00:00:00Z".into(),
            content: format!("deploy notes for {project} {id}"),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        }
    }

    fn vector(cosine: f32) -> Vec<f32> {
        vec![cosine, (1.0 - cosine * cosine).sqrt(), 0.0, 0.0]
    }

    fn fixture(rows: &[(&str, &str, &str, f32)]) -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for (id, conversation, project, cosine) in rows {
            let c = chunk(id, conversation, project);
            storage.insert_chunk(&c, &vector(*cosine)).unwrap();
            engine.insert_chunk(c.id.clone(), vector(*cosine));
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    /// Every row of every table the probe must leave alone, as text.
    fn snapshot(storage: &Storage) -> Vec<(String, Vec<String>)> {
        const TABLES: &[&str] = &[
            "retrieval_events",
            "retrieval_stats",
            "rerank_exposure_impressions",
            "rerank_exposure_items",
            "rerank_reaction_labels",
            "chunks",
            "reflections",
            "chunk_embeddings",
        ];
        TABLES
            .iter()
            .map(|table| {
                let rows = storage
                    .with_connection(|conn| {
                        let mut stmt =
                            conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
                        let width = stmt.column_count();
                        let rows = stmt.query_map([], |row| {
                            let mut cells = Vec::new();
                            for i in 0..width {
                                cells.push(format!(
                                    "{:?}",
                                    row.get::<_, rusqlite::types::Value>(i)?
                                ));
                            }
                            Ok(cells.join("|"))
                        })?;
                        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
                    })
                    .unwrap();
                (table.to_string(), rows)
            })
            .collect()
    }

    fn cids(output: &str) -> Vec<String> {
        output
            .split("<cid>")
            .skip(1)
            .map(|part| part.split("</cid>").next().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn health_probe_uses_runtime_pipeline_without_telemetry_or_reinforcement() {
        let rows: Vec<(String, String, String)> = (0..7)
            .map(|i| (format!("c{i}"), format!("conv-{i}"), "alpha".to_string()))
            .collect();
        let spec: Vec<(&str, &str, &str, f32)> = rows
            .iter()
            .enumerate()
            .map(|(i, (id, conv, project))| {
                (
                    id.as_str(),
                    conv.as_str(),
                    project.as_str(),
                    0.95 - 0.05 * i as f32,
                )
            })
            .collect();
        let (storage, search) = fixture(&spec);
        let query = [1.0_f32, 0.0, 0.0, 0.0];

        let before = snapshot(&storage);
        let page =
            reflect_for_health_probe_with_vec(&storage, &search, &query, "deploy notes", "other")
                .await
                .unwrap();
        let after = snapshot(&storage);
        assert_eq!(
            before, after,
            "the probe wrote to a table it must leave alone"
        );
        assert_eq!(page.len(), 5, "limit five");
        assert_eq!(page[0], "conv-0");

        // the same call through the production pipeline with the probe's
        // parameters returns the same page, so the probe measures production
        let production = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query,
            "deploy notes",
            5,
            0.3,
            Some("all"),
            0,
            validity_partition_enabled(),
            consumption_mode_for_partition(validity_partition_enabled()),
            active_forgetting_enabled(),
            "other",
            search_mode_from(std::env::var("CSR_SEARCH_MODE").ok().as_deref()),
            RecallRerankMode::Runtime,
            true,
        )
        .await
        .unwrap();
        assert_eq!(cids(&production), page);
        // control: the production call with telemetry on does write, so the
        // snapshot above is capable of seeing a write
        assert_ne!(snapshot(&storage), before);
    }

    #[tokio::test]
    async fn health_probe_scope_does_not_depend_on_process_cwd() {
        // "home" is the ambient current project; "elsewhere" scores a little higher
        let (storage, search) = fixture(&[
            ("home-chunk", "conv-home", "proj-home", 0.80),
            ("far-chunk", "conv-far", "proj-far", 0.86),
        ]);
        let query = [1.0_f32, 0.0, 0.0, 0.0];

        let scope = health_probe_scope(&storage).unwrap();
        assert_eq!(scope.scope_label, "all");
        assert!(scope.effective_project.is_none());
        assert!(scope.current_project_for_all_scope.is_none());
        assert!(scope.family_anchor.is_none());

        let probe =
            reflect_for_health_probe_with_vec(&storage, &search, &query, "deploy notes", "other")
                .await
                .unwrap();
        assert_eq!(probe, ["conv-far", "conv-home"]);

        // control: a process whose cwd resolves to proj-home boosts it 1.15x
        // (0.80 * 1.15 beats 0.86), so an ambient scope would reorder the page
        let ambient =
            resolve_search_scope_with(&storage, Some("all"), Some("proj-home"), None, None)
                .unwrap();
        let ambient_out = reflect_on_past_with_vec_in_scope_mode(
            &storage,
            &search,
            &query,
            "deploy notes",
            5,
            0.3,
            &ambient,
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Runtime,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            cids(&ambient_out)[0],
            "conv-home",
            "the control must see the boost: {ambient_out}"
        );
    }
}

/// The live probe shape (2026-10-22, "why did we drop Docker and Qdrant"): the
/// question restated by a story, an extraction and a raw chunk; CSR's own
/// `csr_why` and search output; and the chunks that actually answer.
#[cfg(test)]
mod echo_recall_tests {
    use super::*;
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;
    use crate::storage::recap_feeds::ConsumptionMode;

    const QUERY: &str = "why did we drop Docker and Qdrant";
    const SIG: &str = r#"Signature: {"completion_status":"partial","confidence":"MEDIUM","frameworks":["rust"],"pattern_reusability":"medium","error_recovery":false,"total_edits":0,"iteration_count":2,"tools_used":["ToolSearch"],"files_modified":[],"concepts":[]}"#;
    const WHY_ROW: &str = "WHY: why did we drop Qdrant\n\nconv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255:\n  via=reflection score=0.794 [2026-07-15T19:39:01.638330+00:00] conv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255: ## User Request why did we drop Docker and Qdrant";
    const SEARCH_ROW: &str = "🎯 RESULTS: 5 matches (partial relevance, top score: 0.450)\n⚡ PERFORMANCE: 6ms (1 collection searched)\n\n<search>\n  <summary count=\"5\" relevance=\"partial\" top-score=\"0.450\">\n    <preview>[keyword] why did we drop Docker and Qdrant</preview>\n  </summary>\n  <meta>\n    <q>why</q>\n  </meta>\n  <results>\n  </results>\n</search>";

    fn vec_at(cos: f32) -> Vec<f32> {
        vec![cos, (1.0 - cos * cos).sqrt(), 0.0, 0.0]
    }

    fn chunk(id: &str, conv: &str, cos: f32, content: &str) -> (ConversationChunk, Vec<f32>) {
        (
            ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: "echoproj".into(),
                timestamp: "2026-10-12T00:00:00Z".into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: Speaker::Assistant,
                seq: 0,
                is_sidechain: false,
            },
            vec_at(cos),
        )
    }

    fn ids(out: &str) -> Vec<String> {
        out.split("<id>")
            .skip(1)
            .filter_map(|part| part.split("</id>").next())
            .map(str::to_string)
            .collect()
    }

    async fn recall(
        rows: &[(ConversationChunk, Vec<f32>)],
        reflections: &[(&str, &[&str], String, f32)],
    ) -> Vec<String> {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for (row, vector) in rows {
            storage.insert_chunk(row, vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector.clone());
        }
        for (id, tags, content, cos) in reflections {
            let tags: Vec<String> = tags.iter().map(|tag| tag.to_string()).collect();
            storage
                .insert_reflection(id, content, &tags, &vec_at(*cos))
                .unwrap();
            engine.insert_reflection(id.to_string(), vec_at(*cos));
        }
        let search = Arc::new(RwLock::new(engine));
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            QUERY,
            30,
            0.0,
            Some("echoproj"),
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
        ids(&out)
    }

    fn position(order: &[String], id: &str) -> usize {
        order
            .iter()
            .position(|candidate| candidate == id)
            .unwrap_or_else(|| panic!("{id} missing from {order:?}"))
    }

    type ProbeChunks = Vec<(ConversationChunk, Vec<f32>)>;
    type ProbeReflections = Vec<(&'static str, &'static [&'static str], String, f32)>;

    fn probe_rows() -> (ProbeChunks, ProbeReflections) {
        let rows = vec![
            chunk(
                "echo-chunk",
                "conv1",
                0.75,
                "why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.",
            ),
            chunk("why-row", "conv9", 0.80, WHY_ROW),
            chunk("search-row", "conv9", 0.78, SEARCH_ROW),
            chunk(
                "answer-chunk",
                "conv1",
                0.50,
                "Installing it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded index was faster anyway.",
            ),
            chunk(
                "answer-chunk-2",
                "conv2",
                0.52,
                "Memory has answer. Qdrant died with whole v7 Python/Docker stack in v8.0 rewrite. Why drop: install friction, Docker Desktop needed.",
            ),
        ];
        let reflections = vec![
            (
                "story_conv1",
                &["session_story", "project_echoproj", "conv_conv1"][..],
                format!("why did we drop Docker and Qdrant for a single Rust binary? --- {SIG}. Partially completed"),
                0.68,
            ),
            (
                "extracted_v3_conv1",
                &["narrative_extracted_v3", "project_echoproj", "conv_conv1"][..],
                format!("## User Request\nwhy did we drop Docker and Qdrant for a single Rust binary?\n\n\n---\n{SIG}\nContext:\n## Validation"),
                0.72,
            ),
        ];
        (rows, reflections)
    }

    #[tokio::test]
    async fn pagination_keeps_the_sink_order_of_page_one() {
        let echo = "why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.";
        let answer = "Installing it was too painful, and the scale never needed a database server.";
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for (row, vector) in [
            chunk("echo-chunk", "conv1", 0.90, echo),
            chunk("answer-chunk", "conv2", 0.80, answer),
        ] {
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let page = |offset: usize| {
            get_more_results_in_scope(
                &storage,
                &search,
                &q,
                QUERY,
                offset,
                1,
                0.0,
                &scope,
                ConsumptionMode::Off,
                false,
            )
        };
        let first = page(0).await.unwrap();
        let second = page(1).await.unwrap();
        assert!(
            first.contains("too painful") && !first.contains("Checking past"),
            "{first}"
        );
        assert!(
            second.contains("Checking past") && !second.contains("too painful"),
            "{second}"
        );
    }

    /// Page one and the pages after it order the window the same way: rows that
    /// carry no answer sink, then a confirmed-resolved row sinks below the
    /// unresolved ones. Walking get_more one row at a time must visit every
    /// row page one returned exactly once, in page one's order.
    #[tokio::test]
    async fn page_one_and_get_more_cover_every_result_once_in_the_same_order() {
        let phrases = [
            "Checking past sessions",
            "too painful",
            "Resolved note",
            "Phase 0 spike",
        ];
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for (row, vector) in [
            chunk(
                "echo-chunk",
                "conv1",
                0.95,
                "why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.",
            ),
            chunk(
                "answer-chunk",
                "conv2",
                0.90,
                "Installing it was too painful, and the scale never needed a database server.",
            ),
            chunk(
                "resolved-chunk",
                "conv3",
                0.92,
                "Resolved note: the Qdrant container was removed in the v8 rewrite and verified.",
            ),
            chunk(
                "plain-chunk",
                "conv4",
                0.85,
                "A Phase 0 spike showed an embedded index was faster anyway.",
            ),
        ] {
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        storage
            .insert_resolutions(
                &["resolved-chunk".to_string()],
                "resolved",
                "shipped and verified",
                None,
                "user_confirmed",
            )
            .unwrap();
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let order_of = |text: &str| -> Vec<usize> {
            let mut found: Vec<(usize, usize)> = phrases
                .iter()
                .enumerate()
                .filter_map(|(at, phrase)| text.find(phrase).map(|pos| (pos, at)))
                .collect();
            found.sort();
            found.into_iter().map(|(_, at)| at).collect()
        };

        let page_one = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            10,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let first_page = order_of(&page_one);

        let mut paged = Vec::new();
        for offset in 0..4 {
            let page = get_more_results_in_scope(
                &storage,
                &search,
                &q,
                QUERY,
                offset,
                1,
                0.0,
                &scope,
                ConsumptionMode::Off,
                false,
            )
            .await
            .unwrap();
            let seen = order_of(&page);
            assert_eq!(seen.len(), 1, "offset {offset}: {page}");
            paged.push(seen[0]);
        }
        let beyond = get_more_results_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            4,
            1,
            0.0,
            &scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(beyond.contains("No results"), "{beyond}");

        let mut every_row = paged.clone();
        every_row.sort();
        assert_eq!(every_row, vec![0, 1, 2, 3], "each row once: {paged:?}");
        assert_eq!(paged, first_page, "same order as page one");
        // Echo (0) above the resolved row (2), both below the answers.
        assert_eq!(&paged[2..], &[0, 2], "{paged:?}");
    }

    /// Fifty earlier askings of the question sit in the vector window ahead of
    /// the three answers. A page of five used to be cut from a fifteen-row
    /// window the echoes filled; the pool is now wide enough to reach past them,
    /// the sink moves them down, and the page is full with the answers on it.
    #[tokio::test]
    async fn a_five_row_page_is_full_and_holds_the_answers_behind_fifty_echoes() {
        let echo = "why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.";
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let mut rows = Vec::new();
        for n in 0..50 {
            rows.push(chunk(
                &format!("echo-{n:02}"),
                &format!("echoconv{n:02}"),
                0.95 - n as f32 * 0.002,
                echo,
            ));
        }
        for (n, text) in [
            "Installing it was too painful, and the scale never needed a database server.",
            "A Phase 0 spike showed an embedded index was faster anyway.",
            "Docker Desktop was required for every install, and users gave up there.",
        ]
        .iter()
        .enumerate()
        {
            rows.push(chunk(
                &format!("answer-{n}"),
                &format!("answerconv{n}"),
                0.60 - n as f32 * 0.01,
                text,
            ));
        }
        for (row, vector) in &rows {
            storage.insert_chunk(row, vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector.clone());
        }
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let page = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            QUERY,
            5,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let shown = ids(&page);
        assert_eq!(shown.len(), 5, "a full page: {shown:?}");
        for answer in ["answer-0", "answer-1", "answer-2"] {
            assert!(
                shown.iter().any(|id| id == answer),
                "{answer} must be on the five-row page: {shown:?}"
            );
        }
    }

    /// A pool mixing vector chunks, a vector reflection, a keyword-only chunk
    /// and a keyword-only reflection, walked three rows at a time. Every row
    /// appears once and the pages concatenate to the single large page, in the
    /// order page one fixed. Page one's own order is the one fusion, sink and
    /// settling produced, so the later pages cannot rebuild a vector-only one.
    #[tokio::test]
    async fn three_pages_of_a_mixed_pool_are_slices_of_one_order() {
        let phrases = [
            "ALPHA-vector",
            "BRAVO-vector",
            "CHARLIE-vector",
            "DELTA-vector",
            "ECHO-reflection-vector",
            "FOXTROT-needle-only-chunk",
            "GOLF-needle-only-reflection",
            "HOTEL-vector",
            "INDIA-echo-row",
        ];
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for (n, (text, cos)) in [
            ("ALPHA-vector Qdrant was removed from the install path.", 0.90),
            ("BRAVO-vector the embedded index replaced the server.", 0.84),
            ("CHARLIE-vector Docker is no longer needed to run it.", 0.80),
            ("DELTA-vector the scale never needed a database server.", 0.74),
            ("HOTEL-vector a Phase 0 spike showed it was faster.", 0.70),
            (
                "INDIA-echo-row why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.",
                0.88,
            ),
        ]
        .iter()
        .enumerate()
        {
            let (row, vector) = chunk(&format!("row-{n}"), &format!("conv{n}"), *cos, text);
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        // Keyword-only chunk: in the store and its keyword index, not in the
        // vector index.
        let (needle, needle_vec) = chunk(
            "needle-chunk",
            "convneedle",
            0.10,
            "FOXTROT-needle-only-chunk the Qdrant Docker drop is recorded here.",
        );
        storage.insert_chunk(&needle, &needle_vec).unwrap();
        // A vector reflection, and a keyword-only one.
        let tags = vec![
            "narrative_extracted_v3".to_string(),
            "project_echoproj".to_string(),
        ];
        storage
            .insert_reflection(
                "refl-vector",
                "ECHO-reflection-vector Qdrant was dropped because install was painful.",
                &tags,
                &vec_at(0.78),
            )
            .unwrap();
        engine.insert_reflection("refl-vector".to_string(), vec_at(0.78));
        storage
            .insert_reflection(
                "refl-needle",
                "GOLF-needle-only-reflection we drop Docker and Qdrant to stay a single binary.",
                &tags,
                &vec_at(0.05),
            )
            .unwrap();
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let order_of = |text: &str| -> Vec<usize> {
            let mut found: Vec<(usize, usize)> = phrases
                .iter()
                .enumerate()
                .filter_map(|(at, phrase)| text.find(phrase).map(|pos| (pos, at)))
                .collect();
            found.sort();
            found.into_iter().map(|(_, at)| at).collect()
        };

        let whole = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            9,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let whole_order = order_of(&whole);
        assert_eq!(whole_order.len(), 9, "all nine rows come back: {whole}");

        let first = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            3,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let mut paged = order_of(&first);
        for offset in [3, 6] {
            let page = get_more_results_in_scope(
                &storage,
                &search,
                &q,
                QUERY,
                offset,
                3,
                0.0,
                &scope,
                ConsumptionMode::Off,
                false,
            )
            .await
            .unwrap();
            let seen = order_of(&page);
            assert_eq!(seen.len(), 3, "offset {offset}: {page}");
            paged.extend(seen);
        }
        let mut every_row = paged.clone();
        every_row.sort();
        assert_eq!(
            every_row,
            (0..9).collect::<Vec<_>>(),
            "each row once: {paged:?}"
        );
        assert_eq!(paged, whole_order, "the pages are slices of one order");
    }

    /// Pages after the first are cut from the pool page one settled, not from a
    /// fresh search: a row that appears in the index between the two calls does
    /// not shuffle the order the first page promised.
    #[tokio::test]
    async fn later_pages_come_from_the_pool_page_one_built() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for n in 0..6 {
            let (row, vector) = chunk(
                &format!("row-{n}"),
                &format!("conv{n}"),
                0.90 - n as f32 * 0.05,
                &format!("ROW{n} the install was painful for reason number {n}."),
            );
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let _ = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            2,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        // A better row arrives after page one.
        let (late, late_vec) = chunk("row-late", "convlate", 0.99, "LATE the best row of all.");
        storage.insert_chunk(&late, &late_vec).unwrap();
        search.write().await.insert_chunk(late.id.clone(), late_vec);
        let page = get_more_results_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            2,
            2,
            0.0,
            &scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(page.contains("ROW2") && page.contains("ROW3"), "{page}");
        assert!(!page.contains("LATE"), "{page}");
    }

    /// Walk past the first fifteen rows at the default page size: page one at
    /// limit 5 holds a sixty-row pool (seventy rows exist, so the window is
    /// full), and offsets 5 to 25 are all slices of it.
    /// Each row appears once, in page one's order, and a row that arrives after
    /// page one never shows up.
    #[tokio::test]
    async fn a_walk_in_pages_of_five_stays_on_the_pool_page_one_built() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for n in 0..70 {
            let (row, vector) = chunk(
                &format!("row-{n:02}"),
                &format!("conv{n:02}"),
                0.95 - n as f32 * 0.005,
                &format!("ROW{n:02} the install was painful for reason number {n}."),
            );
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let rows_in = |text: &str| -> Vec<usize> {
            let mut found: Vec<(usize, usize)> = (0..70)
                .filter_map(|n| text.find(&format!("ROW{n:02}")).map(|pos| (pos, n)))
                .collect();
            found.sort();
            found.into_iter().map(|(_, n)| n).collect()
        };
        let first = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            5,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let mut walked = rows_in(&first);
        assert_eq!(walked.len(), 5, "{first}");
        let (late, late_vec) = chunk("row-late", "convlate", 0.99, "LATE the best row of all.");
        storage.insert_chunk(&late, &late_vec).unwrap();
        search.write().await.insert_chunk(late.id.clone(), late_vec);
        for offset in (5..30).step_by(5) {
            let page = get_more_results_in_scope(
                &storage,
                &search,
                &q,
                QUERY,
                offset,
                5,
                0.0,
                &scope,
                ConsumptionMode::Off,
                false,
            )
            .await
            .unwrap();
            assert!(!page.contains("LATE"), "offset {offset}: {page}");
            let seen = rows_in(&page);
            assert_eq!(seen.len(), 5, "offset {offset}: {page}");
            walked.extend(seen);
        }
        assert_eq!(walked, (0..30).collect::<Vec<_>>(), "once each, in order");
    }

    /// The row markers (`ECHO007`, `ANSWER2`, `ROW5`) of a page, in order. Page
    /// one and the later pages name rows differently, so tests read the text; the
    /// summary's preview repeats the first row, so a marker counts once a page.
    fn marks(out: &str) -> Vec<String> {
        let mut seen = HashSet::new();
        out.split_whitespace()
            .flat_map(|word| word.split(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|word| {
                ["ECHO", "ANSWER", "ROW"].iter().any(|tag| {
                    word.strip_prefix(tag).is_some_and(|rest| {
                        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
                    })
                })
            })
            .filter(|word| seen.insert(*word))
            .map(str::to_string)
            .collect()
    }

    /// Rows the vector window of page one cannot see, an echo-filled first
    /// 120, then five answers. Rebuilding the pool wider for the offset past
    /// the cached one sinks the echoes behind the answers; the page must not
    /// repeat the echoes it showed or skip the answers. What a page published
    /// stays where it was shown.
    #[tokio::test]
    async fn a_page_past_the_cached_pool_repeats_no_row_and_shows_the_answers() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for n in 0..120 {
            let (row, vector) = chunk(
                &format!("echo-{n:03}"),
                &format!("convecho{n:03}"),
                0.99 - n as f32 * 0.002,
                &format!("ECHO{n:03} why did we drop Docker and Qdrant?"),
            );
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        for n in 0..5 {
            let (row, vector) = chunk(
                &format!("answer-{n}"),
                &format!("convanswer{n}"),
                0.60 - n as f32 * 0.01,
                &format!(
                    "ANSWER{n} Qdrant went because installing a server was painful, reason {n}."
                ),
            );
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        let search = Arc::new(RwLock::new(engine));
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let first = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            5,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let mut seen: Vec<String> = marks(&first);
        assert_eq!(seen.len(), 5, "{first}");
        for offset in (5..125).step_by(5) {
            let page = get_more_results_in_scope(
                &storage,
                &search,
                &q,
                QUERY,
                offset,
                5,
                0.0,
                &scope,
                ConsumptionMode::Off,
                false,
            )
            .await
            .unwrap();
            let got = marks(&page);
            assert_eq!(got.len(), 5, "offset {offset}: {page}");
            if offset == 120 {
                assert!(
                    got.iter().all(|id| id.starts_with("ANSWER")),
                    "the five answers follow the 120 echoes already shown: {got:?}"
                );
            }
            seen.extend(got);
        }
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            seen.len(),
            "no row repeats across pages: {seen:?}"
        );
        assert_eq!(
            seen.iter().filter(|id| id.starts_with("ANSWER")).count(),
            5,
            "every answer appears once: {seen:?}"
        );
    }

    fn six_rows() -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for n in 0..8 {
            let (row, vector) = chunk(
                &format!("row-{n}"),
                &format!("conv{n}"),
                0.90 - n as f32 * 0.04,
                &format!("ROW{n} the install was painful for reason number {n}."),
            );
            storage.insert_chunk(&row, &vector).unwrap();
            engine.insert_chunk(row.id.clone(), vector);
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    type Rig = (Arc<Storage>, Arc<RwLock<SearchEngine>>, SearchProjectScope);

    const Q4: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

    async fn page_one() -> Rig {
        let (storage, search) = six_rows();
        let scope =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let first = reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &Q4,
            QUERY,
            5,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(first.contains("ROW0") && !first.contains("ROW5"), "{first}");
        (storage, search, scope)
    }

    async fn page_two(rig: &Rig) -> String {
        let (storage, search, scope) = rig;
        get_more_results_in_scope(
            storage,
            search,
            &Q4,
            QUERY,
            5,
            3,
            0.0,
            scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap()
    }

    /// A verdict another process wrote after page one (a direct ledger insert
    /// here: the in-process cache is not told) still reaches the next page.
    #[tokio::test]
    async fn a_verdict_recorded_after_page_one_shows_on_the_next_page() {
        let rig = page_one().await;
        let before = rig.0.verdict_generation();
        rig.0
            .insert_resolutions(
                &["row-5".to_string()],
                "resolved",
                "fixed in 9.5.8",
                None,
                crate::storage::queries::RESOLUTION_SOURCE_USER_CONFIRMED,
            )
            .unwrap();
        assert_ne!(before, rig.0.verdict_generation());
        let page = page_two(&rig).await;
        assert!(page.contains("<resolution>"), "{page}");
        let order = marks(&page);
        assert_eq!(
            order.last().map(String::as_str),
            Some("ROW5"),
            "a confirmed-resolved row sinks below the unresolved ones: {order:?}"
        );
    }

    /// Recording a verdict through `csr_resolve` drops the pools of the store.
    #[tokio::test]
    async fn resolving_a_chunk_drops_the_cached_pools() {
        let rig = page_one().await;
        let held = |storage: &Arc<Storage>| {
            POOL_CACHE.lock().unwrap().iter().any(|entry| {
                entry
                    .storage
                    .upgrade()
                    .is_some_and(|held| Arc::ptr_eq(&held, storage))
            })
        };
        assert!(held(&rig.0), "page one cached its pool");
        resolve_chunks(
            &rig.0,
            vec!["row-6".to_string()],
            "regressed".to_string(),
            "broke again".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(!held(&rig.0), "the store's pools are gone");
        let page = page_two(&rig).await;
        assert_eq!(marks(&page), ["ROW5", "ROW6", "ROW7"], "{page}");
    }

    /// `project="all"` with a current project boosts that family; the same
    /// `all` search without one does not (the health probe). Their pools are
    /// not interchangeable.
    #[tokio::test]
    async fn a_pool_built_for_another_current_project_is_not_reused() {
        let (storage, search) = six_rows();
        let boosted =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        let unboosted =
            SearchProjectScope::resolve_with(&storage, Some("all"), None, None, None).unwrap();
        let other =
            SearchProjectScope::resolve_with(&storage, Some("all"), Some("elsewhere"), None, None)
                .unwrap();
        let q = [1.0f32, 0.0, 0.0, 0.0];
        reflect_on_past_with_vec_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            5,
            0.0,
            &boosted,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let key = |scope: &SearchProjectScope| {
            PoolKey::new(
                QUERY,
                scope,
                0.0,
                SearchMode::Hybrid,
                true,
                ConsumptionMode::Off,
                false,
                storage.verdict_generation(),
            )
        };
        assert!(cached_pool(&storage, &key(&boosted), 5).is_some());
        assert!(cached_pool(&storage, &key(&unboosted), 5).is_none());
        assert!(cached_pool(&storage, &key(&other), 5).is_none());
    }

    /// Codex's inputs, whole: none of these rows is a question echo or a CSR
    /// payload, so none of them sinks.
    #[test]
    fn the_rows_codex_found_sinking_stay_where_their_text_puts_them() {
        let cases = [
            (
                format!("[reflection] ## User Request\nwhy did the import time out?\n\n## Active Issues\nFix: raise the timeout to 120s.\n\n---\n{SIG}"),
                "why did the import time out",
            ),
            (
                "WHY: storage\n\nNo evidence chain found.\n\nRoot cause: migration 42 wrote the lock.".to_string(),
                "storage",
            ),
            (
                "[mcp__claude_self_reflect__csr_why]\nFix: use SQLite.".to_string(),
                "why sqlite",
            ),
            (
                "How long we retain caches is 300 seconds".to_string(),
                "how long do we retain caches",
            ),
            (
                "Recall uses SQLite and HNSW".to_string(),
                "what does recall use",
            ),
        ];
        for (row, query) in cases {
            assert_eq!(non_answer_class(&row, query), 0, "{row}");
        }
    }

    /// A memory whose text opens with the query is promoted ahead of fusion,
    /// whatever its similarity.
    #[tokio::test]
    async fn an_exact_opener_memory_is_first_whatever_its_similarity() {
        let rows = vec![chunk(
            "strong-chunk",
            "conv1",
            0.90,
            "The embedded index was faster, so the server went away.",
        )];
        let reflections = vec![(
            "opener-memory",
            &["memory-decay", "echoproj"][..],
            format!("{QUERY}: because installing it was painful."),
            0.20,
        )];
        let order = recall(&rows, &reflections).await;
        assert_eq!(order[0], "opener-memory", "{order:?}");
    }

    #[tokio::test]
    async fn the_answer_outranks_every_echo_of_its_own_question() {
        let (rows, reflections) = probe_rows();
        let order = recall(&rows, &reflections).await;
        // `echo-chunk` shares conv1 with `answer-chunk`: one row per session,
        // and the answer is the row that stands for conv1.
        assert!(!order.contains(&"echo-chunk".to_string()), "{order:?}");
        let first_echo = ["story_conv1", "extracted_v3_conv1"]
            .iter()
            .map(|id| position(&order, id))
            .min()
            .unwrap();
        for answer in ["answer-chunk", "answer-chunk-2"] {
            assert!(
                position(&order, answer) < first_echo,
                "{answer} must lead the question echoes: {order:?}"
            );
        }
        assert!(
            ["answer-chunk", "answer-chunk-2"].contains(&order[0].as_str()),
            "rank 1 must carry an answer: {order:?}"
        );
    }

    /// A forked worker's tmux pane recording is indexed as text. Its frames
    /// repeat the typed question and a spinner, so it matches the question
    /// best and says nothing. Live excerpt (agent-aa8201b27121421bd).
    const KEYSTROKE_FRAMES: &str = r#"y did we drop Docker and Qdrant
24 20.706 9 ❯ why did we drop Docker and Qdrant f
25 20.87 9 ❯ why did we drop Docker and Qdrant for
26 21.034 9 ❯ why did we drop Docker and Qdrant for a
27 21.2 9 ❯ why did we drop Docker and Qdrant for a s
28 21.367 9 ❯ why did we drop Docker and Qdrant for a sin
29 21.532 9 ❯ why did we drop Docker and Qdrant for a singl
30 21.697 9 ❯ why did we drop Docker and Qdrant for a single
31 21.863 9 ❯ why did we drop Docker and Qdrant for a single Ru
32 22.028 9 ❯ why did we drop Docker and Qdrant for a single Rust
33 22.197 9 ❯ why did we drop Docker and Qdrant for a single Rust b
34 22.363 9 ❯ why did we drop Docker and Qdrant for a single Rust bin
35 22.528 9 ❯ why did we drop Docker and Qdrant for a single Rust binar
36 22.693 9 ❯ why did we drop Docker and Qdrant for a single Rust binary
37 23.53 11 ❯ why did we d"#;
    const SPINNER_FRAMES: &str = r#"154 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | · Actioning… (3s · thinking with medium effort) | ❯
55 27.318 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✢ Actioning… (3s · thinking with medium effort) | ❯
56 27.485 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✶ Actioning… (4s · thinking with medium effort) |"#;

    #[tokio::test]
    async fn terminal_replay_frames_do_not_outrank_answers() {
        let (mut rows, reflections) = probe_rows();
        rows.push(chunk("keystroke-frames", "conv7", 0.90, KEYSTROKE_FRAMES));
        rows.push(chunk("spinner-frames", "conv7", 0.88, SPINNER_FRAMES));
        let order = recall(&rows, &reflections).await;
        // Both recordings are conv7: one row stands for the session.
        assert!(!order.contains(&"spinner-frames".to_string()), "{order:?}");
        for frames in ["keystroke-frames"] {
            for answer in ["answer-chunk", "answer-chunk-2"] {
                assert!(
                    position(&order, answer) < position(&order, frames),
                    "{frames} must sit below {answer}: {order:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_chunk_with_one_prompt_line_among_real_content_keeps_its_rank() {
        let (mut rows, reflections) = probe_rows();
        rows.push(chunk(
            "screen-with-answer",
            "conv8",
            0.95,
            "\u{276f} why did we drop Docker and Qdrant for a single Rust binary?\n\u{23fa} Installing it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded index was faster anyway.",
        ));
        let order = recall(&rows, &reflections).await;
        assert_eq!(order[0], "screen-with-answer", "{order:?}");
    }

    /// CSR's own `csr_why` and search output is left out of recall before any
    /// ranking (it used to sink to the bottom): the answers are all that is left.
    #[tokio::test]
    async fn csr_why_and_search_output_are_left_out_of_recall() {
        let (rows, reflections) = probe_rows();
        let order = recall(&rows, &reflections).await;
        for noise in ["why-row", "search-row"] {
            assert!(
                !order.contains(&noise.to_string()),
                "{noise} is CSR's own output and must not be recalled: {order:?}"
            );
        }
        for answer in ["answer-chunk", "answer-chunk-2"] {
            assert!(order.contains(&answer.to_string()), "{order:?}");
        }
    }

    #[tokio::test]
    async fn a_repeated_question_that_also_carries_its_answer_is_not_demoted() {
        let (mut rows, reflections) = probe_rows();
        rows.push(chunk(
            "repeat-with-answer",
            "conv3",
            0.60,
            "why did we drop Docker and Qdrant? Because installing Docker was too painful and an embedded index was faster.",
        ));
        let order = recall(&rows, &reflections).await;
        // `echo-chunk` shares conv1 with `answer-chunk`: one row per session.
        assert!(!order.contains(&"echo-chunk".to_string()), "{order:?}");
        for echo in ["story_conv1", "extracted_v3_conv1"] {
            assert!(
                position(&order, "repeat-with-answer") < position(&order, echo),
                "a question that carries its answer must lead the bare echoes: {order:?}"
            );
        }
        assert!(!order.contains(&"why-row".to_string()), "{order:?}");
    }

    #[tokio::test]
    async fn a_derived_story_that_opens_with_the_query_is_not_promoted_to_first() {
        let (rows, reflections) = probe_rows();
        let order = recall(&rows, &reflections).await;
        assert_ne!(order[0], "story_conv1", "{order:?}");
    }

    #[tokio::test]
    async fn an_explicit_memory_that_opens_with_the_query_is_still_promoted() {
        let (rows, mut reflections) = probe_rows();
        reflections.push((
            "5eb5aebb-65ab-4fba-9917-86cb83845154",
            &["csr:memory", "echoproj"][..],
            "why did we drop Docker and Qdrant: the install burden and the scale never needed a server.".to_string(),
            0.30,
        ));
        let order = recall(&rows, &reflections).await;
        assert_eq!(
            order[0], "5eb5aebb-65ab-4fba-9917-86cb83845154",
            "{order:?}"
        );
    }

    // ---- echo filter: CSR's own output and logged copies of the query ----

    fn set_author(storage: &Storage, id: &str, conv: &str, author: Speaker) {
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author,
                    source_conv_id: conv.into(),
                    supersedes: None,
                },
            )
            .unwrap();
    }

    /// `echoes` rows that log the query being asked (a tool result, a call
    /// rendering, a typed prompt) in the best vector places, then `answers`
    /// rows that answer it.
    fn echo_filled(echoes: usize, answers: usize) -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        for n in 0..echoes {
            let (text, author) = match n % 3 {
                0 => (
                    format!("ECHO{n:03} {{\"query\": \"{QUERY}\", \"limit\": 5}}"),
                    Speaker::ToolResult,
                ),
                1 => (
                    format!("ECHO{n:03} Pulling CSR context.\n\n[mcp__claude-self-reflect__csr_reflect_on_past: {QUERY}]"),
                    Speaker::Assistant,
                ),
                _ => (
                    format!("{QUERY} and then fix it, please ECHO{n:03}"),
                    Speaker::User,
                ),
            };
            let (row, vector) = chunk(
                &format!("echo-{n:03}"),
                &format!("convecho{n:03}"),
                0.99 - n as f32 * 0.002,
                &text,
            );
            storage.insert_chunk(&row, &vector).unwrap();
            set_author(&storage, &row.id, &row.conversation_id, author);
            engine.insert_chunk(row.id.clone(), vector);
        }
        for n in 0..answers {
            let (row, vector) = chunk(
                &format!("answer-{n}"),
                &format!("convanswer{n}"),
                0.50 - n as f32 * 0.01,
                // No word of the query, so only the vector lane can reach it.
                &format!("ANSWER{n} installing a server was painful for reason {n}."),
            );
            storage.insert_chunk(&row, &vector).unwrap();
            set_author(&storage, &row.id, &row.conversation_id, Speaker::Assistant);
            engine.insert_chunk(row.id.clone(), vector);
        }
        (storage, Arc::new(RwLock::new(engine)))
    }

    async fn limit_five(storage: &Arc<Storage>, search: &Arc<RwLock<SearchEngine>>) -> String {
        let scope =
            SearchProjectScope::resolve_with(storage, Some("all"), Some("echoproj"), None, None)
                .unwrap();
        reflect_on_past_with_vec_in_scope(
            storage,
            search,
            &Q4,
            QUERY,
            5,
            0.0,
            &scope,
            0,
            true,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap()
    }

    /// The trap from the first attempt: a filter that drops the echoes from a
    /// page fetched at exactly `limit` and never refills it returned three
    /// weak rows at limit 5. Eight logged copies outrank six answers; the
    /// page is still five answers.
    #[tokio::test]
    async fn a_limit_five_page_is_refilled_with_answers_when_logged_copies_outrank_them() {
        let (storage, search) = echo_filled(8, 6);
        let page = limit_five(&storage, &search).await;
        let seen = marks(&page);
        assert_eq!(seen.len(), 5, "five rows when six answers exist: {page}");
        assert!(
            seen.iter().all(|mark| mark.starts_with("ANSWER")),
            "no logged copy of the query on the page: {seen:?}"
        );
    }

    /// More logged copies than the first window holds: the window is widened
    /// once and the page is still five answers.
    #[tokio::test]
    async fn a_limit_five_page_widens_the_window_when_it_held_only_logged_copies() {
        let (storage, search) = echo_filled(130, 6);
        let page = limit_five(&storage, &search).await;
        let seen = marks(&page);
        assert_eq!(seen.len(), 5, "five rows when six answers exist: {page}");
        assert!(
            seen.iter().all(|mark| mark.starts_with("ANSWER")),
            "{seen:?}"
        );
    }

    /// Fewer answers than the page: every answer shows, no logged copy fills
    /// the rest.
    #[tokio::test]
    async fn a_page_with_fewer_answers_than_places_shows_no_logged_copy() {
        let (storage, search) = echo_filled(8, 3);
        let page = limit_five(&storage, &search).await;
        let seen = marks(&page);
        assert_eq!(seen, ["ANSWER0", "ANSWER1", "ANSWER2"], "{page}");
    }

    /// Rows that repeat the query's words but are not records of it stay.
    #[tokio::test]
    async fn prose_that_repeats_the_query_is_not_dropped() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut engine = SearchEngine::new(8);
        let rows = [
            // An answer that restates the question inside its own sentence.
            (
                "keep-prose",
                Speaker::Assistant,
                "ROW1 You asked why did we drop Docker and Qdrant, and the answer is install burden.".to_string(),
            ),
            // A code read that mentions the markers is a mention, not CSR's output.
            (
                "keep-code",
                Speaker::ToolResult,
                "ROW2 const NEXT: &str = \"NEXT:\"; const LAST: &str = \"LAST:\"; // TODOS: ANCHORS:".to_string(),
            ),
        ];
        for (n, (id, author, text)) in rows.iter().enumerate() {
            let (row, vector) = chunk(id, &format!("conv{n}"), 0.90 - n as f32 * 0.05, text);
            storage.insert_chunk(&row, &vector).unwrap();
            set_author(&storage, id, &row.conversation_id, *author);
            engine.insert_chunk(row.id.clone(), vector);
        }
        let search = Arc::new(RwLock::new(engine));
        let page = limit_five(&storage, &search).await;
        assert_eq!(marks(&page), ["ROW1", "ROW2"], "{page}");
    }
}

#[cfg(test)]
mod representative_tests {
    use super::*;

    fn chunk(id: &str, score: f64, boilerplate: bool) -> PoolChunk {
        PoolChunk {
            id: id.to_string(),
            score,
            boilerplate,
        }
    }

    #[test]
    fn boilerplate_top_with_content_second_picks_content() {
        let pool = vec![chunk("tool", 0.9, true), chunk("talk", 0.5, false)];
        assert_eq!(session_representative("tool", &pool), "talk");
    }

    #[test]
    fn highest_scored_content_wins_over_lower_content() {
        let pool = vec![
            chunk("tool", 0.9, true),
            chunk("low", 0.2, false),
            chunk("mid", 0.5, false),
        ];
        assert_eq!(session_representative("tool", &pool), "mid");
    }

    #[test]
    fn boilerplate_only_keeps_the_pick() {
        let pool = vec![chunk("tool", 0.9, true), chunk("tool2", 0.4, true)];
        assert_eq!(session_representative("tool", &pool), "tool");
    }

    #[test]
    fn content_top_is_unchanged() {
        let pool = vec![chunk("talk", 0.9, false), chunk("tool", 0.5, true)];
        assert_eq!(session_representative("talk", &pool), "talk");
    }

    #[test]
    fn unknown_pick_abstains() {
        let pool = vec![chunk("talk", 0.9, false)];
        assert_eq!(session_representative("missing", &pool), "missing");
    }

    #[test]
    fn apply_swaps_only_sessions_with_a_boilerplate_pick() {
        let mut collapsed = vec!["t1".to_string(), "c2".to_string(), "e3".to_string()];
        let keys: HashMap<String, String> = [("t1", "s1"), ("c2", "s2"), ("e3", "s3")]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let mut pool: HashMap<String, Vec<PoolChunk>> = HashMap::new();
        pool.insert(
            "s1".into(),
            vec![chunk("t1", 0.9, true), chunk("a1", 0.3, false)],
        );
        pool.insert(
            "s2".into(),
            vec![chunk("c2", 0.9, false), chunk("t2", 0.3, true)],
        );
        apply_content_representatives(&mut collapsed, &keys, &pool);
        assert_eq!(collapsed, vec!["a1", "c2", "e3"]);
    }

    type RepFixture = (
        Vec<String>,
        HashMap<String, String>,
        HashMap<String, Vec<PoolChunk>>,
    );

    fn f1a_rep_fixture() -> RepFixture {
        let collapsed = vec!["a1".to_string(), "c2".to_string()];
        let keys: HashMap<String, String> = [("t1", "s1"), ("a1", "s1"), ("c2", "s2")]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let mut pool: HashMap<String, Vec<PoolChunk>> = HashMap::new();
        pool.insert(
            "s1".into(),
            vec![chunk("t1", 0.9, true), chunk("a1", 0.3, false)],
        );
        pool.insert("s2".into(), vec![chunk("c2", 0.9, false)]);
        (collapsed, keys, pool)
    }

    #[test]
    fn s9r_ce_keeps_content_representative_over_tool_row() {
        // The cross-encoder scores the tool row of s1 highest by a hair and
        // picks it; the content chunk scores within the tie margin, so the
        // content chunk is shown.
        let (collapsed, keys, pool) = f1a_rep_fixture();
        let scored = vec![
            ("t1".to_string(), "s1".to_string(), 0u8, 5.0f32),
            ("a1".to_string(), "s1".to_string(), 0u8, 4.9f32),
            ("c2".to_string(), "s2".to_string(), 0u8, 3.0f32),
        ];
        let plain = ce_reorder(&collapsed, &keys, &scored);
        assert_eq!(plain.order, vec!["t1", "c2"], "CE alone picks the tool row");
        let ordered = ce_reorder_with_representatives(&collapsed, &keys, &scored, &pool);
        assert_eq!(ordered.order, vec!["a1", "c2"]);
        assert_eq!(ordered.session_scores, plain.session_scores);
        // A session without a boilerplate pick is untouched.
        let only_content = vec![scored[1].clone(), scored[2].clone()];
        let before = ce_reorder(&collapsed, &keys, &only_content);
        let after = ce_reorder_with_representatives(&collapsed, &keys, &only_content, &pool);
        assert_eq!(before, after);
    }

    #[test]
    fn f1a_ce_keeps_tool_row_that_clearly_outscores_the_content_chunk() {
        // Bisected cause of the probe rank-1 drop (58de0ee5): a tool-result row
        // holding the answer (a memory file read) beat the session's user-text
        // chunk by 0.5 to 3.8 logits and was swapped out anyway.
        let (collapsed, keys, pool) = f1a_rep_fixture();
        for content_score in [4.5f32, 2.0, -3.0] {
            let scored = vec![
                ("t1".to_string(), "s1".to_string(), 0u8, 5.0f32),
                ("a1".to_string(), "s1".to_string(), 0u8, content_score),
                ("c2".to_string(), "s2".to_string(), 0u8, 3.0f32),
            ];
            let ordered = ce_reorder_with_representatives(&collapsed, &keys, &scored, &pool);
            assert_eq!(
                ordered.order,
                vec!["t1", "c2"],
                "content at {content_score}"
            );
        }
    }

    #[test]
    fn f1a_ce_keeps_tool_row_when_the_content_chunk_was_not_scored() {
        // No score for the content chunk: nothing says it is as good as the pick.
        let (collapsed, keys, pool) = f1a_rep_fixture();
        let scored = vec![
            ("t1".to_string(), "s1".to_string(), 0u8, 5.0f32),
            ("c2".to_string(), "s2".to_string(), 0u8, 3.0f32),
        ];
        let ordered = ce_reorder_with_representatives(&collapsed, &keys, &scored, &pool);
        assert_eq!(ordered.order, vec!["t1", "c2"]);
    }

    #[test]
    fn f1a_final_ce_summary_reads_the_final_order() {
        let scores: HashMap<String, f32> = [("a", 9.0f32), ("b", 7.0), ("c", 1.0)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        // Pool order was a, b, c; a sink moved b to the top.
        let summary = final_ce_summary(&["b", "a", "c"], &scores).unwrap();
        assert_eq!(summary.top, 7.0);
        assert_eq!(summary.runner_up, Some(9.0));
        assert_eq!(summary.median, 7.0);
        // Unpooled rows skip the runner-up and the median.
        let summary = final_ce_summary(&["a", "x", "c", "a"], &scores).unwrap();
        assert_eq!(summary.top, 9.0);
        assert_eq!(summary.runner_up, Some(1.0));
        assert_eq!(summary.median, 5.0);
    }

    #[test]
    fn f1a_final_ce_summary_is_none_without_a_first_score() {
        let scores: HashMap<String, f32> = [("a".to_string(), 9.0f32)].into_iter().collect();
        assert!(final_ce_summary(&["x", "a"], &scores).is_none());
        assert!(final_ce_summary(&[], &scores).is_none());
        let nan: HashMap<String, f32> = [("a".to_string(), f32::NAN)].into_iter().collect();
        assert!(final_ce_summary(&["a"], &nan).is_none());
    }

    fn rig_row(conversation: &str, seq: usize, text: &str) -> EnrichedResult {
        EnrichedResult {
            score: 1.0,
            chunk: crate::import::ConversationChunk {
                id: format!("{conversation}-{seq}"),
                conversation_id: conversation.into(),
                project_name: "p".into(),
                timestamp: "2026-10-01T00:00:00Z".into(),
                content: text.into(),
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

    /// An assistant chunk holding one Bash call.
    fn rig_call(seq: usize, text: &str) -> EnrichedResult {
        let mut row = rig_row("rig", seq, text);
        row.chunk.author = crate::provenance::Speaker::Assistant;
        row
    }

    fn rig_store(rows: &[&EnrichedResult]) -> Storage {
        let storage = Storage::open_memory().unwrap();
        for row in rows {
            storage
                .insert_chunk_with_source(&row.chunk, &vec![0.1; 384], "conversation")
                .unwrap();
            storage
                .insert_chunk_provenance(
                    &row.chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: row.chunk.author,
                        source_conv_id: row.chunk.conversation_id.clone(),
                        supersedes: None,
                    },
                )
                .unwrap();
        }
        storage
    }

    fn rig_ids(rows: &[EnrichedResult]) -> Vec<&str> {
        rows.iter().map(|row| row.chunk.id.as_str()).collect()
    }

    #[test]
    fn synth_sink_rig_rows_demotes_in_place_and_leaves_other_queries_alone() {
        let query = "why do hooks use catch-all wrappers";
        let planted = "why do hooks use catch-all wrappers so they never block claude code? \
            [synthetic re-ask cycle 3] Synthetic E3 test answer.";
        // A session that ran the rig twice among forty Bash calls.
        let mut rig: Vec<EnrichedResult> = vec![
            rig_call(0, "[Bash: csr-engine bench --probe --db-path /x/db]"),
            rig_call(1, "[Bash: python3 s3/probe_ab.py compare a b]"),
        ];
        rig.extend((2..42).map(|seq| rig_call(seq, "[Bash: ls]")));
        let candidate = rig_row("rig", 50, planted);
        let real = rig_row("real", 0, planted);
        let answer = rig_row("answer", 0, "hooks wrap their bodies so a bug never blocks");
        let mut all: Vec<&EnrichedResult> = rig.iter().collect();
        all.extend([&candidate, &real, &answer]);
        let storage = rig_store(&all);
        let tags = HashMap::new();

        // Page order from the ranker: the planted row first.
        let mut page = vec![candidate.clone(), real.clone(), answer.clone()];
        sink_rig_rows(&storage, &mut page, &tags, query);
        assert_eq!(rig_ids(&page), ["real-0", "answer-0", "rig-50"]);

        // A query the rows do not hold: the page is untouched.
        let mut page = vec![candidate.clone(), real.clone(), answer.clone()];
        sink_rig_rows(&storage, &mut page, &tags, "how do I sort a vec");
        assert_eq!(rig_ids(&page), ["rig-50", "real-0", "answer-0"]);

        // A stored reflection with the same text is never judged.
        let mut tagged = HashMap::new();
        tagged.insert("rig-50".to_string(), vec!["reflection".to_string()]);
        let mut page = vec![candidate.clone(), real.clone(), answer.clone()];
        sink_rig_rows(&storage, &mut page, &tagged, query);
        assert_eq!(rig_ids(&page), ["rig-50", "real-0", "answer-0"]);
    }
}
