//! Import `~/.claude/plans/*.md` plan documents into the CSR corpus.
//!
//! Plans are stored as chunks with `source='plan'` and `conversation_id = "plan:<slug>"`.
//! Correlation to origin project/conversation is best-effort (FTS+verify, then registry
//! window); uncorrelated plans land in the `_unscoped` sentinel project.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// A plan markdown document discovered under the plans directory.
pub struct PlanDoc {
    /// Filename stem, e.g. `"witty-strolling-hollerith"`.
    pub slug: String,
    pub path: PathBuf,
    /// RFC3339 UTC of file mtime.
    pub mtime: String,
    pub content: String,
}

/// Heuristic English stopwords (not exhaustive) — long function words that survive the
/// `len > 5` filter and would otherwise pollute distinctiveness scoring.
const STOPWORDS: &[&str] = &[
    "should",
    "would",
    "could",
    "because",
    "before",
    "after",
    "which",
    "their",
    "there",
    "where",
    "these",
    "those",
    "about",
    "through",
    "still",
    "being",
    "having",
    "during",
    "without",
    "between",
    "another",
    "something",
    "someone",
    "everything",
    "anything",
    "nothing",
    "however",
    "although",
    "whether",
    "already",
    "always",
    "really",
    "further",
    "perhaps",
    "several",
    "various",
];

/// List `*.md` in `plans_dir` needing (re)import.
///
/// Skip: non-md, empty files, files whose mtime is unchanged since last import
/// (`storage.is_file_imported`). Fail-soft: unreadable file → skip + `bump_aux_counter("plans")`.
pub fn discover_plans(plans_dir: &Path, storage: &crate::storage::Storage) -> Result<Vec<PlanDoc>> {
    if !plans_dir.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(plans_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if !path.is_file() {
            continue;
        }

        // Unchanged mtime since last import → skip (generic import_state by real path).
        if storage.is_file_imported(&path).unwrap_or(false) {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                // One bad file must not abort the whole scan.
                let _ = storage.bump_aux_counter("plans");
                continue;
            }
        };
        if content.trim().is_empty() {
            continue;
        }

        let slug = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if slug.is_empty() {
            continue;
        }

        let mtime = match mtime_rfc3339(&path) {
            Ok(m) => m,
            Err(_) => {
                let _ = storage.bump_aux_counter("plans");
                continue;
            }
        };

        out.push(PlanDoc {
            slug,
            path,
            mtime,
            content,
        });
    }
    Ok(out)
}

/// Correlate plan to `(project_name, Option<conversation_id>)`.
///
/// Strategy 1: FTS candidate generation + multi-token verification + optional session window.
/// Strategy 2: unique session_registry project containing plan mtime ± 2h.
pub fn correlate_project(
    plan: &PlanDoc,
    storage: &crate::storage::Storage,
) -> Option<(String, Option<String>)> {
    if let Some(hit) = strategy1_fts_verify(plan, storage) {
        return Some(hit);
    }
    strategy2_registry_window(plan, storage)
}

/// Import one plan. Returns number of chunks created.
pub fn import_plan(engine: &crate::engine::Engine, plan: &PlanDoc) -> Result<usize> {
    if plan.content.trim().is_empty() {
        return Ok(0);
    }

    let conversation_id = format!("plan:{}", plan.slug);

    // Clean slate first — reimport with fewer chunks must not leave orphans.
    engine
        .storage()
        .delete_chunks_for_conversation(&conversation_id)?;

    let correlation = correlate_project(plan, engine.storage());
    let project_name = correlation
        .as_ref()
        .map(|(p, _)| p.clone())
        .unwrap_or_else(|| "_unscoped".to_string());
    let correlated_conv_id = correlation.and_then(|(_, c)| c);

    let summary = Some(plan_summary(&plan.content));
    let pieces = split_plan_content(&plan.content);
    let mut chunks = Vec::new();
    for piece in pieces {
        super::push_chunk(
            &mut chunks,
            &conversation_id,
            &project_name,
            &plan.mtime,
            piece,
            1,
            &summary,
            crate::provenance::Speaker::User,
            false,
        );
    }

    let chunk_count = chunks.len();
    if chunk_count == 0 {
        return Ok(0);
    }

    // Single batch is fine — plans are short documents.
    let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = engine.embeddings().embed(&texts)?;

    {
        let mut idx = engine.search().blocking_write();
        for (chunk, embedding) in chunks.iter().zip(embeddings) {
            engine
                .storage()
                .insert_chunk_with_source(chunk, &embedding, "plan")?;
            idx.insert_chunk(chunk.id.clone(), embedding);

            if let Some(ref conv) = correlated_conv_id {
                // Non-fatal: a provenance-write failure must not abort the import.
                if let Err(e) = engine.storage().insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: crate::provenance::Speaker::User,
                        source_conv_id: conv.clone(),
                        supersedes: None,
                    },
                ) {
                    eprintln!("CSR: plan provenance persist error (non-fatal): {e}");
                }
            }
        }
    }

    engine
        .storage()
        .mark_file_imported(&plan.path, chunk_count)?;
    Ok(chunk_count)
}

// ─── helpers ───

fn mtime_rfc3339(path: &Path) -> Result<String> {
    let modified = path.metadata()?.modified()?;
    Ok(chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339())
}

/// First markdown heading, or first 80 chars (char-boundary safe).
fn plan_summary(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('#') {
            return rest.trim_start_matches('#').trim().to_string();
        }
    }
    let end = content.floor_char_boundary(80.min(content.len()));
    content[..end].to_string()
}

/// Split plan body by the JSONL importer's character budget, preferring paragraph breaks.
fn split_plan_content(content: &str) -> Vec<String> {
    let budget = super::CHUNK_CHAR_BUDGET;
    let content = content.trim();
    if content.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut start = 0;
    let len = content.len();

    while start < len {
        let remaining = len - start;
        if remaining <= budget {
            let piece = content[start..].trim();
            if !piece.is_empty() {
                out.push(piece.to_string());
            }
            break;
        }

        let mut end = content.floor_char_boundary(start + budget);
        // Prefer a blank-line break in the last ~100 chars of the budget window.
        let window_start = end.saturating_sub(100).max(start);
        if let Some(rel) = content[window_start..end].rfind("\n\n") {
            let candidate = window_start + rel;
            if candidate > start {
                end = candidate;
            }
        }

        let piece = content[start..end].trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }

        // Advance past the break (and any whitespace) without spinning.
        let mut next = end;
        while next < len {
            let ch = content[next..].chars().next();
            match ch {
                Some(c) if c.is_whitespace() => next += c.len_utf8(),
                _ => break,
            }
        }
        if next <= start {
            next = content.floor_char_boundary(start + 1).min(len);
        }
        start = next;
    }
    out
}

/// 12 most distinctive tokens: lowercase, alnum-split, len>5, stopword-filtered,
/// longest-first as a rarity proxy.
fn distinctive_tokens(content: &str) -> Vec<String> {
    let lower = content.to_lowercase();
    let mut seen = HashSet::new();
    let mut tokens: Vec<String> = Vec::new();
    for tok in lower.split(|c: char| !c.is_alphanumeric()) {
        if tok.len() <= 5 {
            continue;
        }
        if STOPWORDS.contains(&tok) {
            continue;
        }
        if seen.insert(tok.to_string()) {
            tokens.push(tok.to_string());
        }
    }
    tokens.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    tokens.truncate(12);
    tokens
}

fn strategy1_fts_verify(
    plan: &PlanDoc,
    storage: &crate::storage::Storage,
) -> Option<(String, Option<String>)> {
    let tokens = distinctive_tokens(&plan.content);
    if tokens.is_empty() {
        return None;
    }
    let query = tokens.join(" ");
    // FTS is candidate GENERATION only — never accept on rank alone (OR-join inflates hits).
    let hits = storage.fts5_search(&query, 5, None).ok()?;
    if hits.is_empty() {
        return None;
    }

    let mut scored: Vec<(usize, &crate::import::ConversationChunk)> = hits
        .iter()
        .map(|h| {
            let content_lower = h.content.to_lowercase();
            let score = tokens
                .iter()
                .filter(|t| content_lower.contains(t.as_str()))
                .count();
            (score, h)
        })
        .collect();
    scored.sort_by_key(|b| std::cmp::Reverse(b.0));

    let top_score = scored[0].0;
    let runner_up = if scored.len() >= 2 { scored[1].0 } else { 0 };
    // Accept only strong multi-token identity, not single-token FTS noise.
    if top_score < 8 || top_score - runner_up < 3 {
        return None;
    }

    let hit = scored[0].1;
    // Registry window check when present — lagging registry waives (None).
    if let Ok(Some((first_ts, last_ts))) = storage.get_session_window(&hit.conversation_id) {
        match session_window_contains(&plan.mtime, &first_ts, &last_ts) {
            Some(true) => {}
            Some(false) => return None, // real window, plan outside it → false positive
            None => {}                  // unparseable timestamps → waive
        }
    }

    Some((hit.project_name.clone(), Some(hit.conversation_id.clone())))
}

/// `Some(bool)` when both ends parse; `None` means waive the check.
fn session_window_contains(plan_mtime: &str, first_ts: &str, last_ts: &str) -> Option<bool> {
    let plan = chrono::DateTime::parse_from_rfc3339(plan_mtime)
        .ok()?
        .with_timezone(&chrono::Utc);
    let first = chrono::DateTime::parse_from_rfc3339(first_ts)
        .ok()?
        .with_timezone(&chrono::Utc);
    let last = chrono::DateTime::parse_from_rfc3339(last_ts)
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(plan >= first && plan <= last + chrono::Duration::hours(4))
}

fn strategy2_registry_window(
    plan: &PlanDoc,
    storage: &crate::storage::Storage,
) -> Option<(String, Option<String>)> {
    let projects = storage
        .get_projects_with_window_containing(&plan.mtime, 2)
        .ok()?;
    if projects.len() == 1 {
        return Some((projects[0].clone(), None));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingEngine;
    use crate::engine::Engine;
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;
    use crate::search::SearchEngine;
    use crate::storage::Storage;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn zero_emb() -> Vec<f32> {
        vec![0.0; 384]
    }

    fn mk_chunk(id: &str, conv: &str, project: &str, content: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.into(),
            conversation_id: conv.into(),
            project_name: project.into(),
            timestamp: "2026-06-10T12:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        }
    }

    /// Distinctive multi-token plan body (12+ long tokens for Strategy 1).
    fn distinctive_plan_body() -> String {
        "# Hnswvector reindexing procedure\n\n\
         The hnswvector reindexing procedure requires embeddingspace optimization, \
         chunkboundary realignment, provenancegraph traversal, sessionregistry \
         windowing, planimport correlation, distinctivetoken scoring, adversarialmargin \
         validation, boilerplatefiltering rejection, and unscopedsentinel handling for \
         orphaned plan documents.\n"
            .to_string()
    }

    #[test]
    fn discover_skips_unchanged_mtime() {
        let storage = Storage::open_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("witty-plan.md");
        std::fs::write(&path, "# Plan\n\nSome plan content here.\n").unwrap();

        storage.mark_file_imported(&path, 1).unwrap();
        let found = discover_plans(dir.path(), &storage).unwrap();
        assert!(
            found.iter().all(|p| p.path != path),
            "unchanged mtime must be skipped (got {} plans)",
            found.len()
        );

        // Rewrite changes mtime (and content) so rediscovery is required.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "# Plan\n\nUpdated plan content now.\n").unwrap();
        let found = discover_plans(dir.path(), &storage).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].slug, "witty-plan");
        assert!(found[0].content.contains("Updated"));
    }

    #[test]
    fn correlate_fts_hit() {
        let storage = Storage::open_memory().unwrap();
        let body = distinctive_plan_body();
        // Seed a conversation chunk that shares ≥8 distinctive tokens with the plan.
        let seed = "Discussed hnswvector reindexing procedure embeddingspace optimization \
                    chunkboundary realignment provenancegraph traversal sessionregistry \
                    windowing planimport correlation distinctivetoken scoring \
                    adversarialmargin validation.";
        storage
            .insert_chunk(&mk_chunk("c1", "conv-y", "project-x", seed), &zero_emb())
            .unwrap();

        let plan = PlanDoc {
            slug: "test-plan".into(),
            path: PathBuf::from("/tmp/test-plan.md"),
            mtime: "2026-06-10T13:00:00Z".into(),
            content: body,
        };
        let got = correlate_project(&plan, &storage);
        assert_eq!(
            got,
            Some(("project-x".into(), Some("conv-y".into()))),
            "expected FTS+verify hit, got {got:?}"
        );
    }

    #[test]
    fn correlate_no_match_returns_none() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_chunk(
                &mk_chunk(
                    "c1",
                    "conv-a",
                    "other-proj",
                    "Cooking recipes for pasta carbonara and risotto milanese dinner parties.",
                ),
                &zero_emb(),
            )
            .unwrap();

        let plan = PlanDoc {
            slug: "no-match".into(),
            path: PathBuf::from("/tmp/no-match.md"),
            mtime: "2026-06-10T13:00:00Z".into(),
            content: distinctive_plan_body(),
        };
        assert_eq!(correlate_project(&plan, &storage), None);
    }

    #[test]
    fn correlate_shared_boilerplate_ambiguous_none() {
        let storage = Storage::open_memory().unwrap();
        // Shared disclaimer/header — present in plan AND both projects' chunks.
        // Neither chunk carries the plan's distinctive body tokens, so score stays
        // well below 8 / margin-3 even if FTS ranks a boilerplate hit first.
        let boilerplate = "IMPORTANT DISCLAIMER: This document is confidential proprietary \
                           information. Unauthorized redistribution is strictly prohibited.";
        let plan_body = format!("{boilerplate}\n\n{}", distinctive_plan_body());
        storage
            .insert_chunk(
                &mk_chunk("c-a", "conv-a", "proj-a", boilerplate),
                &zero_emb(),
            )
            .unwrap();
        storage
            .insert_chunk(
                &mk_chunk("c-b", "conv-b", "proj-b", boilerplate),
                &zero_emb(),
            )
            .unwrap();

        let plan = PlanDoc {
            slug: "boilerplate-plan".into(),
            path: PathBuf::from("/tmp/boilerplate-plan.md"),
            mtime: "2026-06-10T13:00:00Z".into(),
            content: plan_body,
        };
        assert_eq!(
            correlate_project(&plan, &storage),
            None,
            "margin rule must reject similarity-only boilerplate matches"
        );
    }

    #[test]
    fn import_plan_chunks_and_provenance() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let body = distinctive_plan_body();
        let seed = "Discussed hnswvector reindexing procedure embeddingspace optimization \
                    chunkboundary realignment provenancegraph traversal sessionregistry \
                    windowing planimport correlation distinctivetoken scoring \
                    adversarialmargin validation.";
        storage
            .insert_chunk(
                &mk_chunk("seed-1", "conv-y", "project-x", seed),
                &zero_emb(),
            )
            .unwrap();

        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(16)));
        let projects_dir = tempfile::tempdir().unwrap();
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            projects_dir.path().to_path_buf(),
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("import-me.md");
        std::fs::write(&path, &body).unwrap();
        let mtime = mtime_rfc3339(&path).unwrap();

        let plan = PlanDoc {
            slug: "import-me".into(),
            path: path.clone(),
            mtime,
            content: body.clone(),
        };
        let n = import_plan(&engine, &plan).unwrap();
        assert!(n > 0);

        let conv_id = "plan:import-me";
        let ids = storage.get_chunk_ids_for_conversation(conv_id).unwrap();
        assert_eq!(ids.len(), n);
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();
        assert!(chunks.iter().all(|c| c.conversation_id == conv_id));
        assert!(chunks.iter().all(|c| c.project_name == "project-x"));

        let sources = storage.chunk_sources_for_conversation(conv_id).unwrap();
        assert_eq!(sources.len(), n);
        assert!(sources.iter().all(|s| s == "plan"));

        for id in &ids {
            let prov = storage.get_chunk_provenance(id).unwrap();
            assert!(prov.is_some(), "provenance required when correlated: {id}");
            assert_eq!(prov.unwrap().source_conv_id, "conv-y");
        }

        // Re-import with different content (same slug) — old marker must vanish.
        let old_marker = "UNIQUE_OLD_MARKER_zzzzyyyxxx";
        let new_body = "# Revised plan\n\nNEW_CONTENT_MARKER_aaabbbccc \
             with plenty of filler text to keep the document non-empty for embedding."
            .to_string();
        // First import with old marker embedded so we can prove deletion.
        let plan_old = PlanDoc {
            slug: "import-me".into(),
            path: path.clone(),
            mtime: plan.mtime.clone(),
            content: format!("{body}\n\n{old_marker}\n"),
        };
        import_plan(&engine, &plan_old).unwrap();
        let ids_mid = storage.get_chunk_ids_for_conversation(conv_id).unwrap();
        let mid = storage.get_chunks_by_ids(&ids_mid).unwrap();
        assert!(
            mid.iter().any(|c| c.content.contains(old_marker)),
            "setup: old marker should be present before reimport"
        );

        let plan_new = PlanDoc {
            slug: "import-me".into(),
            path: path.clone(),
            mtime: plan.mtime.clone(),
            content: new_body,
        };
        let n2 = import_plan(&engine, &plan_new).unwrap();
        assert!(n2 > 0);
        let ids2 = storage.get_chunk_ids_for_conversation(conv_id).unwrap();
        let chunks2 = storage.get_chunks_by_ids(&ids2).unwrap();
        assert!(
            chunks2.iter().all(|c| !c.content.contains(old_marker)),
            "old content must be gone after reimport"
        );
        assert!(
            chunks2
                .iter()
                .any(|c| c.content.contains("NEW_CONTENT_MARKER_aaabbbccc")),
            "new content must be present"
        );
        // No orphans: every returned row belongs to this conversation_id (query is exact).
        assert_eq!(ids2.len(), chunks2.len());
    }

    #[test]
    fn unscoped_plan_gets_sentinel_project() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        // Unrelated corpus — no distinctive overlap.
        storage
            .insert_chunk(
                &mk_chunk(
                    "c1",
                    "conv-z",
                    "cooking",
                    "Pasta carbonara recipes and risotto techniques for dinner.",
                ),
                &zero_emb(),
            )
            .unwrap();

        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(16)));
        let projects_dir = tempfile::tempdir().unwrap();
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            projects_dir.path().to_path_buf(),
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orphan.md");
        let body = distinctive_plan_body();
        std::fs::write(&path, &body).unwrap();
        let plan = PlanDoc {
            slug: "orphan".into(),
            path,
            mtime: "2026-06-10T13:00:00Z".into(),
            content: body,
        };
        let n = import_plan(&engine, &plan).unwrap();
        assert!(n > 0);
        let ids = storage
            .get_chunk_ids_for_conversation("plan:orphan")
            .unwrap();
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();
        assert!(chunks.iter().all(|c| c.project_name == "_unscoped"));
        // No provenance edges for uncorrelated plans.
        for id in &ids {
            assert!(storage.get_chunk_provenance(id).unwrap().is_none());
        }
    }
}
