use super::*;

/// Evidence from the gates that actually ran, not index population.
#[derive(Default)]
pub(super) struct SearchEvidence {
    pub below_floor: bool,
    pub above_floor: bool,
    pub lookalike: bool,
    pub other_filter: bool,
}

impl SearchEvidence {
    pub fn abstention(&self) -> super::super::prompt_log::DecisionReason {
        use super::super::prompt_log::DecisionReason;
        if self.lookalike && !self.other_filter {
            DecisionReason::LookalikeOnly
        } else if self.below_floor && !self.above_floor {
            DecisionReason::BelowFloor
        } else {
            DecisionReason::NoCandidates
        }
    }
}

/// Search chunks using a pre-computed embedding vector (P-1 optimization).
/// Scoped to `project` — chunks from other projects are filtered out, and so
/// is anything `is_self_echo` names.
#[allow(clippy::too_many_arguments)]
pub(super) async fn search_chunks_with_vec(
    engine: &Engine,
    query: &str,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
    cwd: &Path,
    evidence: &mut SearchEvidence,
) -> Vec<RawResult> {
    search_chunks_with_vec_observed(
        engine,
        query,
        crate::search::hybrid::search_mode_from_env(),
        query_vec,
        limit,
        min_score,
        project,
        current_session,
        |paths, within| {
            super::super::scope_folder::conversations_in_asker_repo(
                paths,
                engine.projects_dir(),
                cwd,
                within,
            )
        },
        evidence,
    )
    .await
}

/// Chunk candidates for the prompt: the vector hits, fused with FTS5 keyword
/// hits by the same reciprocal-rank fusion the MCP search path uses
/// (`search::hybrid`, `CSR_SEARCH_MODE` honoured, hybrid by default). The
/// keyword channel searches only the prompt's few rare words
/// (`hybrid::hook_keyword_chunk_hits`), so a long prompt does not rank most of
/// the table. Output is
/// in fused order. A vector hit keeps its cosine; a keyword-only hit has no
/// cosine, so it carries the MCP path's keyword-only score (a decayed 0.45) and
/// is not held to `min_score`, which is a cosine floor. Keyword hits are
/// searched inside `project` when one is given. Every candidate then meets the
/// same filters in [`search_chunks_with_vec_core`].
#[allow(clippy::too_many_arguments)]
pub(super) async fn hybrid_chunk_candidates(
    engine: &Engine,
    query: &str,
    mode: crate::search::hybrid::SearchMode,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    now: &chrono::DateTime<chrono::Utc>,
    evidence: &mut SearchEvidence,
) -> (Vec<crate::search::SearchResult>, HashSet<String>) {
    use crate::search::hybrid::{self, SearchMode};

    let semantic = if mode == SearchMode::Fts {
        Vec::new()
    } else {
        let wait = stages::time(Stage::IndexWait);
        let idx = engine.search().read().await;
        drop(wait);
        let _search = stages::time(Stage::Search);
        let hits = idx.search_chunks(query_vec, limit, min_score);
        // Only an exhausted floor needs a diagnostic probe. Keep the real
        // search and its tombstone/score-exhaustion bounds unchanged.
        if hits.is_empty() && !evidence.below_floor {
            evidence.below_floor |= idx
                .search_chunks(query_vec, 1, -1.0)
                .iter()
                .any(|hit| hit.score < min_score);
        }
        evidence.above_floor |= !hits.is_empty();
        hits
    };
    if mode == SearchMode::Vector {
        return (semantic, HashSet::new());
    }

    let mut keyword = hybrid::hook_keyword_chunk_hits(
        engine.storage(),
        query,
        limit,
        (!project.is_empty()).then_some(project),
    );
    // A chunk the live index does not hold (its embedding was deleted, or it
    // was never embedded) is not recalled by keyword either: the vector index
    // is what says which chunks exist for recall, so a deletion that reached
    // the index also reaches the keyword channel.
    {
        let wait = stages::time(Stage::IndexWait);
        let idx = engine.search().read().await;
        drop(wait);
        let _search = stages::time(Stage::Search);
        keyword
            .hits
            .retain(|(chunk, _, _)| idx.has_chunk(&chunk.id));
    }
    if keyword.hits.is_empty() {
        return (semantic, HashSet::new());
    }
    evidence.above_floor = true;
    let semantic_order: Vec<String> = semantic.iter().map(|r| r.id.clone()).collect();
    let fts_order: Vec<String> = keyword.hits.iter().map(|(c, _, _)| c.id.clone()).collect();
    let order = match mode {
        SearchMode::Fts => fts_order,
        _ => hybrid::fuse_rrf(&semantic_order, &fts_order, hybrid::RRF_K),
    };
    let cosine: HashMap<&str, f32> = semantic.iter().map(|r| (r.id.as_str(), r.score)).collect();
    let timestamps: HashMap<&str, &str> = keyword
        .hits
        .iter()
        .map(|(c, _, _)| (c.id.as_str(), c.timestamp.as_str()))
        .collect();
    let keyword_only: HashSet<String> = order
        .iter()
        .filter(|id| !cosine.contains_key(id.as_str()))
        .cloned()
        .collect();
    let fused = order
        .into_iter()
        .map(|id| {
            let score = cosine.get(id.as_str()).copied().unwrap_or_else(|| {
                hybrid::keyword_only_score(timestamps.get(id.as_str()).copied().unwrap_or(""), now)
            });
            crate::search::SearchResult { id, score }
        })
        .collect();
    (fused, keyword_only)
}

/// Most the candidate list may grow, as a multiple of the requested `limit`,
/// when the content gates keep emptying it.
const REPLENISH_CAP_FACTOR: usize = 4;

/// What the whole chunk search may spend. The daemon answers a prompt within
/// 800 ms (`prompt_transport::REQUEST_BUDGET`); the rest of the prompt (query
/// embedding, reflections, pickup) needs the remainder.
const CHUNK_SEARCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(600);

/// What a repository check may be given, at most
/// ([`super::super::scope_folder::PROMPT_CHECK_TIMEOUT`]), and the part of the budget
/// kept back from it for the retrieval and filtering of the same pass.
const REPO_CHECK_MARGIN: std::time::Duration = std::time::Duration::from_millis(50);

/// A refill is started only when the budget left covers its worst case: a
/// full repository check plus the pass's own retrieval.
const REFILL_MIN_REMAINING: std::time::Duration = std::time::Duration::from_millis(
    super::super::scope_folder::PROMPT_CHECK_TIMEOUT.as_millis() as u64
        + REPO_CHECK_MARGIN.as_millis() as u64,
);

/// [`search_chunks_with_vec`] with the repository test supplied, so unit tests
/// decide which conversations widen without a filesystem or `git`. `same_repo`
/// is called at most once per pass, and only when some candidate's label
/// differs from `project` and has a recorded transcript path. A prompt whose
/// candidates all match by label costs nothing; any other costs one batched
/// query first.
///
/// The content gates (CSR text, tool-output dump) run after retrieval, so a run
/// of top-ranked candidates they reject would leave the slots empty while
/// eligible ones sit just below the cut. When one of them rejected something
/// and fewer chunks survived than the prompt renders ([`KEYWORD_FILL_SLOTS`]),
/// the retrieval is repeated with twice the limit, up to
/// [`REPLENISH_CAP_FACTOR`] times `limit` or until the index has no more
/// candidates above the floor. A prompt neither gate touched, or one that
/// still fills its slots, searches once. The age gate does not trigger a
/// refill: old candidates are common, and a refill on them adds a retrieval to
/// most prompts.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) async fn search_chunks_with_vec_core(
    engine: &Engine,
    query: &str,
    mode: crate::search::hybrid::SearchMode,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
    same_repo: impl FnMut(
        HashMap<String, Vec<String>>,
        std::time::Duration,
    ) -> super::super::scope_folder::RepoCheck,
) -> Vec<RawResult> {
    search_chunks_with_vec_observed(
        engine,
        query,
        mode,
        query_vec,
        limit,
        min_score,
        project,
        current_session,
        same_repo,
        &mut SearchEvidence::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn search_chunks_with_vec_observed(
    engine: &Engine,
    query: &str,
    mode: crate::search::hybrid::SearchMode,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
    mut same_repo: impl FnMut(
        HashMap<String, Vec<String>>,
        std::time::Duration,
    ) -> super::super::scope_folder::RepoCheck,
    evidence: &mut SearchEvidence,
) -> Vec<RawResult> {
    let started = std::time::Instant::now();
    let cap = limit.saturating_mul(REPLENISH_CAP_FACTOR);
    let wanted = KEYWORD_FILL_SLOTS.min(limit);
    let mut fetch = limit;
    // A conversation is checked once per prompt, however many passes meet it:
    // each check can spend its full time budget. Only a completed check is
    // remembered, "not this repository" included. A conversation the check ran
    // out of time before judging stays askable, and is not asked at all once
    // too little of the search budget is left to give a check any time.
    let mut checked: HashSet<String> = HashSet::new();
    let mut widened: super::super::scope_folder::RepoConversations = HashMap::new();
    let mut checked_once = |paths: HashMap<String, Vec<String>>| {
        let fresh: HashMap<String, Vec<String>> = paths
            .into_iter()
            .filter(|(conversation, _)| !checked.contains(conversation))
            .collect();
        let within = CHUNK_SEARCH_BUDGET
            .saturating_sub(started.elapsed())
            .saturating_sub(REPO_CHECK_MARGIN)
            .min(super::super::scope_folder::PROMPT_CHECK_TIMEOUT);
        if !fresh.is_empty() && !within.is_zero() {
            let ids: Vec<String> = fresh.keys().cloned().collect();
            let check = same_repo(fresh, within);
            checked.extend(ids.into_iter().filter(|id| !check.unchecked.contains(id)));
            widened.extend(check.widened);
        }
        widened.clone()
    };
    loop {
        let (raw_results, candidates, gated) = chunk_candidates_pass(
            engine,
            query,
            mode,
            query_vec,
            fetch,
            min_score,
            project,
            current_session,
            &mut checked_once,
            evidence,
        )
        .await;
        if gated == 0 || raw_results.len() >= wanted || candidates < fetch || fetch >= cap {
            return raw_results;
        }
        // One deadline for the whole search: a refill that could not finish
        // inside it would cost the prompt its injection.
        if started.elapsed() + REFILL_MIN_REMAINING > CHUNK_SEARCH_BUDGET {
            return raw_results;
        }
        fetch = fetch.saturating_mul(2).min(cap);
    }
}

/// One retrieval and filtering pass at `limit` candidates. Returns the chunks
/// that survive, how many candidates retrieval produced, and how many the
/// refill gates (CSR text, tool-output dump) rejected.
#[allow(clippy::too_many_arguments)]
pub(super) async fn chunk_candidates_pass(
    engine: &Engine,
    query: &str,
    mode: crate::search::hybrid::SearchMode,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
    same_repo: &mut impl FnMut(
        HashMap<String, Vec<String>>,
    ) -> super::super::scope_folder::RepoConversations,
    evidence: &mut SearchEvidence,
) -> (Vec<RawResult>, usize, usize) {
    let storage = engine.storage();

    let now = chrono::Utc::now();
    let (results, keyword_only) = hybrid_chunk_candidates(
        engine, query, mode, query_vec, limit, min_score, project, &now, evidence,
    )
    .await;
    let mut raw_results = Vec::new();
    let mut kept_keyword_only: Vec<usize> = Vec::new();
    let mut gated = 0usize;

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
            super::super::scope_folder::MAX_PATHS_PER_CONVERSATION,
        ) {
            Ok(paths) if !paths.is_empty() => same_repo(paths),
            Ok(_) => HashMap::new(),
            Err(e) => {
                tracing::warn!(error = %e, "import_state lookup failed; exact project match only");
                HashMap::new()
            }
        }
    };

    // The detector's answer for every fetched chunk, in one batched read: a
    // legacy chunk with no provenance row has an unknown author and is checked.
    let csr_text = storage.csr_text_hits(&fetched);
    // The cached floor and tool-result share of every hit in one query.
    let trust_and_share = storage
        .chunk_trust_and_tool_share(&chunk_ids)
        .unwrap_or_default();
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
                    evidence.other_filter = true;
                    continue;
                }

                if is_self_echo(&chunk, current_session) {
                    evidence.other_filter = true;
                    continue;
                }
                if is_own_subagent(&chunk, current_session, || {
                    storage
                        .get_chunk_provenance(&result.id)
                        .ok()
                        .flatten()
                        .map(|provenance| provenance.source_conv_id)
                }) {
                    evidence.other_filter = true;
                    continue;
                }

                // CSR text is labeled in search and not handed back as memory:
                // read the whole chunk, before the preview is cut. Selection
                // only; the stored chunk is untouched.
                if csr_text.contains_key(&chunk.id) {
                    evidence.other_filter = true;
                    gated += 1;
                    continue;
                }

                // A dump of tool output is what the tool printed, not memory of
                // the work: the session that ran it already read it.
                let (min_trust, tool_result_share) = trust_and_share
                    .get(&chunk.id)
                    .copied()
                    .unwrap_or((crate::provenance::TrustTier::Unknown, None));
                if is_tool_dump(tool_result_share, &chunk.content) {
                    evidence.other_filter = true;
                    gated += 1;
                    continue;
                }

                // Hard age gate: skip chunks older than MAX_CHUNK_AGE_DAYS
                // Prevents stale conversations from winning on semantic similarity alone
                if let Some(ts) = crate::temporal::parse_timestamp(&chunk.timestamp) {
                    let age_days = (now - ts).num_days();
                    // Reject future-dated chunks (clock skew) and stale chunks
                    if !(0..=MAX_CHUNK_AGE_DAYS).contains(&age_days) {
                        evidence.other_filter = true;
                        continue;
                    }
                }

                if keyword_only.contains(&result.id) {
                    kept_keyword_only.push(raw_results.len());
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
                    min_trust,
                });
            }
        }
    }

    // Keyword-only hits fill the slots the vector hits left empty; they never
    // compete with vector hits that passed the same filters. Ranking weighs
    // recency and file overlap beside similarity, so a keyword-only chunk with
    // a weak similarity could otherwise push a strong vector hit out of the
    // rendered slots. `fts` mode asked for keyword hits only, so it keeps all.
    if mode != crate::search::hybrid::SearchMode::Fts && !kept_keyword_only.is_empty() {
        let room = KEYWORD_FILL_SLOTS.saturating_sub(raw_results.len() - kept_keyword_only.len());
        let drop: HashSet<usize> = kept_keyword_only.iter().skip(room).copied().collect();
        let mut index = 0;
        raw_results.retain(|_| {
            index += 1;
            !drop.contains(&(index - 1))
        });
    }

    (raw_results, results.len(), gated)
}

/// Search reflections using a pre-computed embedding vector (P-1 optimization).
/// Scoped to `project` — reflections tagged for other projects are filtered out.
/// Legacy reflections without project tags are allowed through. A reflection
/// derived from the asking session, or from one of its own subagents (its
/// `conv_<id>` tag), is dropped like the raw chunks it was made from.
#[allow(clippy::too_many_arguments)]
pub(super) async fn search_reflections_with_vec(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,
    current_session: Option<&str>,
    evidence: &mut SearchEvidence,
) -> Vec<RawResult> {
    let search = engine.search();
    let storage = engine.storage();

    let results = {
        let wait = stages::time(Stage::IndexWait);
        let idx = search.read().await;
        drop(wait);
        let _search = stages::time(Stage::Search);
        let hits = idx.search_reflections(query_vec, limit, min_score);
        if hits.is_empty() && !evidence.below_floor {
            evidence.below_floor |= idx
                .search_reflections(query_vec, 1, -1.0)
                .iter()
                .any(|hit| hit.score < min_score);
        }
        evidence.above_floor |= !hits.is_empty();
        hits
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
                    evidence.other_filter = true;
                    continue;
                }
            }

            if tags
                .iter()
                .filter_map(|t| t.strip_prefix("conv_"))
                .any(|conv| is_own_conversation(storage, conv, current_session))
            {
                evidence.other_filter = true;
                continue;
            }

            // Episode reflections are structured JSON surfaced via the Tier-0
            // CONTINUUM / pickup paths — never dump the raw blob as a context line.
            if tags.iter().any(|t| t == "session_episode") {
                evidence.other_filter = true;
                continue;
            }

            // Shift Ledger rows are kept for the companion pane and ledger dedupe
            // only; they must never reach Claude through hook injection or search.
            if crate::story_map::store::is_ledger_row(&tags) {
                evidence.other_filter = true;
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
                evidence.other_filter = true;
                continue;
            }
            if crate::injection::specificity::is_generic(&excerpt, &common) {
                evidence.lookalike = true;
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
pub(super) fn extract_file_paths(content: &str) -> Vec<String> {
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
pub(super) fn format_evolution_summary(
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
pub(super) fn path_suffix_match(a: &str, b: &str) -> bool {
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
        // Only a label that can be trusted (see `trusted_outcome`).
        if let Some(outcome) = crate::hooks::session_start::trusted_outcome(ep) {
            line.push_str(&format!(" (outcome={outcome})"));
        }
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

pub(super) struct TrackedGraphSlice {
    pub(super) memory_id: String,
    pub(super) content: String,
}

pub(super) fn build_graph_slices_with_ids(
    storage: &crate::storage::Storage,
    prompt: &str,
    current_files: &[String],
    project: &str,
) -> Vec<TrackedGraphSlice> {
    let mut slices = Vec::new();

    // File-anchored slice: symbols + callers of files named in the prompt.
    // `ledger.callers` is already filtered to resolved=1 edges upstream
    // (src/storage/codegraph.rs file_slice callers query), so these names
    // are verified already and do not need the unverified label. Only the
    // first five of each are read, through the file-name index.
    for file in current_files.iter().take(1) {
        let slice = storage
            .with_connection(|conn| crate::storage::codegraph::file_slice(conn, project, file, 5));
        if let Ok(ledger) = slice {
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
        // "the exact recipe of take 2" names no code: a plain English word is
        // a symbol only when it is not a common one (see `symbol_overlap`).
        if !super::is_identifier_shaped(&token) && super::is_common_word(&token) {
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

        if verified_names.is_empty() {
            // Every callee is an unresolved guess: a line of guesses
            // (`recipe — …unverified guesses: dict, group, search`) tells
            // Claude nothing true. Say nothing; a later token may still name
            // a symbol with real callees.
            continue;
        }
        let line = if unverified_names.is_empty() {
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
pub(super) fn extract_error_patterns(content: &str) -> Vec<String> {
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
pub(super) fn extract_file_paths_from_prompt(prompt: &str) -> Vec<String> {
    let extensions = [
        ".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".toml", ".json", ".yaml", ".yml", ".md",
        ".css", ".html", ".go", ".java", ".c", ".h", ".cpp",
    ];
    let mut files = Vec::new();
    for word in prompt.split_whitespace() {
        // Strip common surrounding punctuation (quotes, backticks, parens) and
        // the `@` of an `@path` mention
        let cleaned = word.trim_matches(|c: char| {
            c == '`' || c == '"' || c == '\'' || c == '(' || c == ')' || c == ',' || c == '@'
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
pub(super) fn extract_error_patterns_from_prompt(prompt: &str) -> Vec<String> {
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

/// Lowercased alphanumeric words of `text`, single-spaced: the form in which
/// two renderings of one item (markdown, quotes, labels aside) compare equal.
pub(super) fn normalized_words(text: &str) -> String {
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
pub(super) fn opening_key(content: &str) -> String {
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
pub(super) struct HandedOnce {
    /// Sessions handed over, and whether a wrapper (episode, story, extract)
    /// stood for one. Raw chunks of one session may sit side by side; nothing
    /// follows a wrapper of the same session.
    sessions: HashMap<String, bool>,
    openings: Vec<String>,
}

impl HandedOnce {
    pub(super) fn note_episode(&mut self, episode: &crate::hooks::stop::Episode) {
        self.sessions.insert(episode.session_id.clone(), true);
        for text in episode_asks(episode) {
            self.note_opening(&text);
        }
        self.note_opening(&episode.completed);
    }

    pub(super) fn note_opening(&mut self, text: &str) {
        let key = opening_key(text);
        if key.len() >= MIN_OPENING_KEY_CHARS {
            self.openings.push(key);
        }
    }

    /// Admit an item unless its session or its opening words are already out.
    pub(super) fn admit(&mut self, session: Option<&String>, wrapper: bool, content: &str) -> bool {
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
pub(super) fn quoted_in_prompt(content: &str, pasted_words: &str) -> bool {
    let key = opening_key(content);
    key.len() >= MIN_OPENING_KEY_CHARS && pasted_words.contains(&key)
}

/// What a tool-dominated chunk must carry to be worth handing back: a
/// statement of a decision, a verdict, a cause or the receipts, or a PR or
/// commit named in a sentence. Anything else in a tool dump is the tool's
/// output, which the session that ran the tool already read.
///
/// The words alone prove nothing: `decision: bool` in a struct, `commit
/// 3f9a2c1…` in a `git log`, and `test result: ok` in a cargo log are output
/// and source. So a label counts only where it opens a line, a list item or a
/// sentence and the line is a sentence, not source. A bare test log is not a
/// receipt: it says what the run printed, not what the person concluded.
pub(super) fn statement_label() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?im)(?:^|[.!?][ \t]+)[ \t>*#|+-]*(?:\d{1,3}[.)][ \t]+[ \t>*#|-]*)?(decisions?|decided|fixed|verdict|root cause|receipts?)\**[ \t]*:\**[ \t]*([^\n]*)",
        )
        .expect("statement label compiles")
    })
}

pub(super) fn change_reference() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(r"(?i)\bPR[ \t]?#\d+|\bcommit[ \t]+[0-9a-f]{7,40}\b")
            .expect("change reference compiles")
    })
}

/// The line is a declaration or a statement of source, not a sentence: it ends
/// in `{` or `;`, it is a field (`name: Type,` or `name = value,`), or what
/// follows its label is one typed token (`Vec<Receipt>`, `Verdict::Ok`). Prose
/// that quotes a call or a path inside a sentence is not source, and neither is
/// a clause that ends in a comma because the sentence goes on.
pub(super) fn source_shaped(line: &str, tail: &str) -> bool {
    let line = line.trim_end();
    if line.ends_with(['{', ';']) || (line.ends_with(',') && is_field(line)) {
        return true;
    }
    let token = tail.trim();
    !token.is_empty()
        && !token.contains(char::is_whitespace)
        && token.contains(['<', '>', '[', ']', '&', '*'])
}

/// `name: Type,` or `name = value,` (after any bullet, `pub` or label
/// markup): a bare identifier, then a value that is one token or opens with a
/// bracket, a reference or a generic. Words after the colon make it a clause.
pub(super) fn is_field(line: &str) -> bool {
    let body = line
        .trim_end_matches(',')
        .trim_start_matches(|c: char| c.is_whitespace() || ">*#|+-".contains(c));
    let body = body.strip_prefix("pub ").unwrap_or(body);
    let Some(at) = body.find([':', '=']) else {
        return false;
    };
    let (name, value) = (body[..at].trim().trim_matches('*'), body[at + 1..].trim());
    let value = value.trim_start_matches(['=', ':', '*', ' ']);
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !value.is_empty()
        && (!value.contains(char::is_whitespace) || value.starts_with(['[', '(', '&', '<']))
}

/// Words made of letters only (an apostrophe or hyphen inside is fine).
pub(super) fn prose_words(text: &str) -> usize {
    text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|word| {
            word.chars().count() >= 2
                && word
                    .chars()
                    .all(|c| c.is_alphabetic() || c == '\'' || c == '-')
        })
        .count()
}

/// A figure: a digit, or a hash (seven or more hex digits in one token).
pub(super) fn has_figure(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
        || text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| token.len() >= 7 && token.chars().all(|c| c.is_ascii_hexdigit()))
}

pub(super) fn states_decision_or_receipt(content: &str) -> bool {
    let labelled = statement_label().captures_iter(content).any(|found| {
        let whole = found.get(0).map_or(0..0, |m| m.range());
        let tail = found.get(2).map_or("", |m| m.as_str());
        // The whole line is judged, from its first character: a label inside a
        // declaration is a field name.
        let line_start = content[..whole.start].rfind('\n').map_or(0, |at| at + 1);
        if source_shaped(&content[line_start..whole.end], tail) {
            return false;
        }
        // A receipts line is figures (sizes, counts, hashes), not sentences.
        if found[1].to_lowercase().starts_with("receipt") {
            has_figure(tail)
        } else {
            prose_words(tail) >= 3
        }
    });
    labelled
        || content.lines().any(|line| {
            change_reference().is_match(line) && !source_shaped(line, "") && prose_words(line) >= 4
        })
}

/// The chunk is mostly tool output and states no decision or receipt. A chunk
/// with no recorded share (imported before spans were) is judged as before.
pub(super) fn is_tool_dump(tool_result_share: Option<f64>, content: &str) -> bool {
    tool_result_share.is_some_and(|share| share >= TOOL_DUMP_SHARE)
        && !states_decision_or_receipt(content)
}

/// Check if content is self-referential noise about CSR internals.
/// Prevents the tool's own development history from polluting its output (Bug 4/5).
/// CSR's own emitted blocks (probe reports, CONTINUUM/briefing echoes) are
/// detected structurally via `extraction::provenance`; the pattern list below
/// covers dev-history vocabulary that isn't an emission format.
pub(super) fn is_self_referential_noise(content: &str) -> bool {
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

/// The bracket label of one handed line: its source and cached trust floor, then the short id of
/// the past session it came from. Without the id Claude cannot open that session (the line was all
/// it had); with it, `get_full_conversation` finds it, as it matches an id by substring. A
/// subagent's chunk names no session Claude could open, so it carries none.
pub(super) fn handoff_label(result: &crate::injection::predictor::ScoredResult) -> String {
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
