//! Aux-source adapter: import `~/.claude/plans/*.md` plan documents into the CSR
//! corpus as embedded, provenance-linked chunks (`source = "plan"`).
//!
//! Plans are user-ratified decisions that often outlive the conversation that
//! produced them (a plan doc gets revised across many sessions), so they need their
//! own idempotent import path rather than piggybacking on the JSONL importer:
//! reimport must replace stale content in place, and correlation to an origin
//! conversation is a best-effort guess, not a guarantee (see `correlate_project`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::engine::Engine;
use crate::import::ConversationChunk;
use crate::provenance::{ChunkProvenance, Speaker};
use crate::storage::Storage;

/// A single `*.md` plan document discovered on disk, ready for correlation + import.
pub struct PlanDoc {
    /// Filename stem, e.g. "witty-strolling-hollerith".
    pub slug: String,
    pub path: PathBuf,
    /// RFC3339 UTC of the file's mtime — the import_state change-detection key.
    pub mtime: String,
    pub content: String,
}

/// List `*.md` files in `plans_dir` that need (re)import: never imported, or their
/// import_state entry `plan:<slug>` has a different mtime than the file's current one.
/// Fails soft on individual files (unreadable file, unstattable mtime) — one bad plan
/// must not abort discovery of the rest; each skip is counted via
/// `storage.bump_aux_counter("plans")` so silent corpus gaps stay visible (the same
/// failure mode that let TodoWrite→TaskCreate rot episode extraction for weeks).
pub fn discover_plans(plans_dir: &Path, storage: &Storage) -> Result<Vec<PlanDoc>> {
    let mut out = Vec::new();
    if !plans_dir.exists() {
        return Ok(out);
    }

    let entries = match std::fs::read_dir(plans_dir) {
        Ok(e) => e,
        Err(_) => {
            storage.bump_aux_counter("plans")?;
            return Ok(out);
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            storage.bump_aux_counter("plans")?;
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let Some(slug) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            storage.bump_aux_counter("plans")?;
            continue;
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                storage.bump_aux_counter("plans")?;
                continue;
            }
        };
        if content.trim().is_empty() {
            continue;
        }

        let Some(mtime) = file_mtime_rfc3339(&path) else {
            storage.bump_aux_counter("plans")?;
            continue;
        };

        let key = format!("plan:{slug}");
        if storage.get_import_state_mtime(&key)?.as_deref() == Some(mtime.as_str()) {
            continue; // unchanged since last import
        }

        out.push(PlanDoc {
            slug,
            path,
            mtime,
            content,
        });
    }

    Ok(out)
}

/// File mtime as RFC3339 UTC, or `None` if the filesystem can't produce one (never
/// touches `import_state` — pure `fs::metadata` read).
fn file_mtime_rfc3339(path: &Path) -> Option<String> {
    let modified = path.metadata().ok()?.modified().ok()?;
    let dt: DateTime<Utc> = modified.into();
    Some(dt.to_rfc3339())
}

/// Stopwords long enough to otherwise pass the `length > 5` distinctiveness filter.
/// Not exhaustive by design — the margin rule (score gap >= 3) is what actually
/// defends against boilerplate false-positives, this list just improves signal.
const STOPWORDS: &[&str] = &[
    "should",
    "would",
    "could",
    "because",
    "through",
    "before",
    "having",
    "during",
    "between",
    "without",
    "however",
    "therefore",
    "although",
    "though",
    "whether",
    "another",
    "itself",
    "myself",
    "something",
    "anything",
    "nothing",
    "everything",
    "everyone",
    "please",
    "thanks",
    "regards",
    "section",
    "follows",
    "following",
];

/// The 12 most distinctive tokens in `text`: length > 5, alphanumeric, lowercased,
/// stopwords out, de-duplicated, longer (rarer proxy) first.
fn distinctive_tokens(text: &str, n: usize) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 5 && w.chars().all(|c| c.is_alphanumeric()))
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect();
    // No corpus-wide document frequency available here, so token length is used as a
    // cheap rarity proxy (longer words tend to be more specific/less common).
    tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));
    tokens.truncate(n);
    tokens
}

/// Lowercased whole-token set of `text` — used to check "verbatim" token membership
/// without substring false positives (e.g. "cat" inside "category").
fn token_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Count of `tokens` present verbatim (whole-word) in `hit_content`.
fn score_hit(tokens: &[String], hit_content: &str) -> usize {
    let hit_tokens = token_set(hit_content);
    tokens.iter().filter(|t| hit_tokens.contains(*t)).count()
}

/// Correlate a plan to its likely origin `(project_name, Option<conversation_id>)`.
///
/// Codex adversarial finding: `fts5_search` OR-joins sanitized terms, so FTS rank alone
/// can promote a hit that only shares ONE common token with the plan — FTS here is
/// candidate generation only; acceptance is decided by the margin rule below.
///
/// Strategy 1 (FTS + verify): score each of the top-5 FTS hits by how many of the
/// plan's 12 distinctive tokens appear verbatim in the hit's content. Accept the top
/// hit only if its score is high AND clearly ahead of the runner-up — that gap is what
/// actually proves "this is the same conversation," not just "a similar topic." Shared
/// boilerplate across two projects scores both hits equally, so the gap collapses to
/// zero and neither is accepted (adversarial test `correlate_shared_boilerplate_ambiguous_none`).
///
/// Strategy 2 (registry window, only tried if Strategy 1 found nothing): if exactly
/// one project has a recorded session whose time window contains the plan's mtime
/// (+/- 2h slack), correlate to that project with no specific conversation.
pub fn correlate_project(plan: &PlanDoc, storage: &Storage) -> Option<(String, Option<String>)> {
    correlate_via_fts(plan, storage).or_else(|| correlate_via_registry_window(plan, storage))
}

fn correlate_via_fts(plan: &PlanDoc, storage: &Storage) -> Option<(String, Option<String>)> {
    const ACCEPT_SCORE: usize = 8;
    const ACCEPT_MARGIN: usize = 3;

    let tokens = distinctive_tokens(&plan.content, 12);
    if tokens.is_empty() {
        return None;
    }

    // fts5_search already strips FTS5 syntax characters per word and OR-joins the
    // result, so malformed input (odd leftover punctuation) can't error the query.
    let query = tokens.join(" ");
    let hits = storage.fts5_search(&query, 5, None).ok()?;
    if hits.is_empty() {
        return None;
    }

    let mut scored: Vec<(usize, &ConversationChunk)> = hits
        .iter()
        .map(|h| (score_hit(&tokens, &h.content), h))
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

    let (top_score, top_hit) = scored[0];
    let runner_up_score = scored.get(1).map(|(s, _)| *s).unwrap_or(0);

    if top_score < ACCEPT_SCORE || top_score.saturating_sub(runner_up_score) < ACCEPT_MARGIN {
        return None;
    }

    // If the hit's origin session is registered, require the plan to have been
    // written during (or shortly after) that session — otherwise a topically similar
    // but stale conversation could be accepted just for lacking a fresher rival. A
    // registry that hasn't caught up yet (or has null timestamps) waives the check
    // rather than rejecting a plan the registry simply doesn't know about.
    if registry_window_check(storage, &top_hit.conversation_id, &plan.mtime) == Some(false) {
        return None;
    }

    Some((
        top_hit.project_name.clone(),
        Some(top_hit.conversation_id.clone()),
    ))
}

/// `Some(true)`/`Some(false)`: a session_registry row exists and the plan's mtime
/// is/isn't inside its window (+4h grace for post-session write-up). `None`: no row,
/// or timestamps missing/unparseable — caller treats this as "can't verify, waive."
fn registry_window_check(
    storage: &Storage,
    conversation_id: &str,
    plan_mtime: &str,
) -> Option<bool> {
    let (first_ts, last_ts) = storage
        .get_session_registry_window(conversation_id)
        .ok()
        .flatten()?;
    let first = parse_rfc3339(&first_ts?)?;
    let last = parse_rfc3339(&last_ts?)?;
    let mtime = parse_rfc3339(plan_mtime)?;
    Some(mtime >= first && mtime <= last + Duration::hours(4))
}

fn correlate_via_registry_window(
    plan: &PlanDoc,
    storage: &Storage,
) -> Option<(String, Option<String>)> {
    let mtime = parse_rfc3339(&plan.mtime)?;
    let slack = Duration::hours(2);
    let rows = storage.list_session_registry_windows().ok()?;

    let mut matching_projects: HashSet<String> = HashSet::new();
    for (project, first_ts, last_ts) in rows {
        let (Some(first_ts), Some(last_ts)) = (first_ts, last_ts) else {
            continue;
        };
        let (Some(first), Some(last)) = (parse_rfc3339(&first_ts), parse_rfc3339(&last_ts)) else {
            continue;
        };
        if mtime >= first - slack && mtime <= last + slack {
            matching_projects.insert(project);
        }
    }

    // Multiple candidate projects is ambiguous, not a pick-one situation — an
    // over-confident guess here would mis-scope a plan into the wrong project's
    // recall forever.
    if matching_projects.len() == 1 {
        matching_projects.into_iter().next().map(|p| (p, None))
    } else {
        None
    }
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// First markdown heading's text if present, else the first 80 chars of the trimmed
/// content — mirrors the JSONL importer's summary-or-first-message fallback, giving
/// timeline display something readable instead of raw markdown.
fn plan_summary(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let text = heading.trim_start_matches('#').trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed.floor_char_boundary(80.min(trimmed.len()));
    Some(trimmed[..end].to_string())
}

/// Split `content` into budget-sized chunks and append them via the JSONL importer's
/// own `push_chunk` (private to `import`, visible here as `plans` is its submodule) —
/// this reuses the exact deterministic chunk-id + budget logic the JSONL path uses
/// instead of re-deriving it, per the spec's "mirror the JSONL call site" instruction.
fn push_plan_chunks(
    chunks: &mut Vec<ConversationChunk>,
    conversation_id: &str,
    project_name: &str,
    timestamp: &str,
    content: &str,
    summary: &Option<String>,
) {
    let budget = super::CHUNK_CHAR_BUDGET;
    let mut start = 0;
    while start < content.len() {
        let mut end = (start + budget).min(content.len());
        end = content.floor_char_boundary(end);
        if end <= start {
            end = content.len();
        }
        super::push_chunk(
            chunks,
            conversation_id,
            project_name,
            timestamp,
            content[start..end].to_string(),
            1,
            summary,
            Speaker::User, // plans are user-ratified — never assistant/tool_result
            false,         // plans are never sidechain transcripts
        );
        start = end;
    }
}

/// Import one plan: wipe any existing chunks for `plan:<slug>` (idempotent even
/// across shrinking reimports), correlate it to a project/conversation, chunk +
/// embed + store it with `source = "plan"`, and record its import_state mtime.
/// Returns the number of chunks created.
pub fn import_plan(engine: &Engine, plan: &PlanDoc) -> Result<usize> {
    let storage = engine.storage();
    let conv_id = format!("plan:{}", plan.slug);

    // Content can shrink between reimports; deterministic chunk ids would only
    // overwrite the chunks that still exist, leaving a stale tail behind. Wipe first,
    // rebuild from scratch, so reimport is a true replace rather than a merge.
    storage.delete_chunks_for_conversation(&conv_id)?;

    let (project_name, source_conv) =
        correlate_project(plan, storage).unwrap_or(("_unscoped".to_string(), None));

    let summary = plan_summary(&plan.content);
    let mut new_chunks: Vec<ConversationChunk> = Vec::new();
    push_plan_chunks(
        &mut new_chunks,
        &conv_id,
        &project_name,
        &plan.mtime,
        &plan.content,
        &summary,
    );

    if new_chunks.is_empty() {
        storage.upsert_import_state_explicit(&conv_id, &conv_id, 0, &plan.mtime)?;
        return Ok(0);
    }

    let texts: Vec<&str> = new_chunks.iter().map(|c| c.content.as_str()).collect();
    // EmbeddingEngine::embed already wraps its model Mutex internally — no manual
    // locking needed here, matching the constraint against locking it directly.
    let embeddings = engine.embeddings().embed(&texts)?;

    {
        // tokio::sync::RwLock has no plain (non-async) writer; blocking_write is its
        // supported bridge for a sync call site like this one — import_plan's public
        // signature is deliberately sync (frozen spec), wiring into an async caller is
        // the orchestrator's job, not this module's.
        let mut idx = engine.search().blocking_write();
        for (chunk, embedding) in new_chunks.iter().zip(embeddings.iter()) {
            storage.insert_chunk_with_source(chunk, embedding, "plan")?;
            if let Some(ref conv) = source_conv {
                // Non-fatal, mirroring Engine::import_file's provenance persistence:
                // losing a provenance edge shouldn't fail the whole import, the chunk
                // still lands in search, just without author-authority weighting.
                if let Err(e) = storage.insert_chunk_provenance(
                    &chunk.id,
                    &ChunkProvenance {
                        author: Speaker::User,
                        source_conv_id: conv.clone(),
                        supersedes: None,
                    },
                ) {
                    eprintln!("CSR: plan chunk provenance persist error (non-fatal): {e}");
                }
            }
            idx.insert_chunk(chunk.id.clone(), embedding.clone());
        }
    }

    storage.upsert_import_state_explicit(&conv_id, &conv_id, new_chunks.len(), &plan.mtime)?;

    Ok(new_chunks.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingEngine;
    use crate::search::SearchEngine;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn write_plan(dir: &Path, slug: &str, content: &str) -> PathBuf {
        let path = dir.join(format!("{slug}.md"));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn test_engine(storage: Arc<Storage>) -> Engine {
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(64)));
        Engine::from_parts(storage, embeddings, search, PathBuf::from("/tmp"))
    }

    /// A body long enough to yield 12 distinctive tokens and to score >= 8 against a
    /// matching corpus chunk, built from repeated but individually-unique long words.
    fn distinctive_body(prefix: &str) -> String {
        format!(
            "# {prefix} planning session\n\n\
             Objective: refactor the {prefix} reinstatement pipeline before merging. \
             We investigated correlation heuristics, provenance annotation, embedding \
             regeneration, chunk deduplication, timestamp verification, adversarial \
             boilerplate detection, registry windowing, and margin-based acceptance \
             thresholds for the {prefix} subsystem migration."
        )
    }

    #[test]
    fn discover_skips_unchanged_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        write_plan(tmp.path(), "alpha", "# Alpha\n\nSome content here.");

        let first = discover_plans(tmp.path(), &storage).unwrap();
        assert_eq!(first.len(), 1);
        let plan = &first[0];
        storage
            .upsert_import_state_explicit("plan:alpha", "plan:alpha", 1, &plan.mtime)
            .unwrap();

        // Same mtime recorded -> nothing to (re)import.
        let second = discover_plans(tmp.path(), &storage).unwrap();
        assert!(second.is_empty());

        // Recorded mtime no longer matches the file's actual mtime (simulating a
        // change without depending on filesystem mtime-setting APIs) -> re-surfaced.
        storage
            .upsert_import_state_explicit("plan:alpha", "plan:alpha", 1, "1970-01-01T00:00:00Z")
            .unwrap();
        let third = discover_plans(tmp.path(), &storage).unwrap();
        assert_eq!(third.len(), 1);
    }

    #[test]
    fn correlate_fts_hit() {
        let storage = Storage::open_memory().unwrap();
        let engine = test_engine(Arc::new(storage));
        let storage = engine.storage();

        let body = distinctive_body("saga");
        let embedding = engine
            .embeddings()
            .embed(&[body.as_str()])
            .unwrap()
            .remove(0);
        let chunk = ConversationChunk {
            id: "chunk-origin".into(),
            conversation_id: "conv-origin".into(),
            project_name: "csr-engine".into(),
            timestamp: "2026-07-20T10:00:00Z".into(),
            content: body.clone(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &embedding).unwrap();

        let plan = PlanDoc {
            slug: "saga-plan".into(),
            path: PathBuf::from("/dev/null"),
            mtime: "2026-07-20T12:00:00Z".into(),
            content: body,
        };

        let result = correlate_project(&plan, storage);
        assert_eq!(
            result,
            Some(("csr-engine".to_string(), Some("conv-origin".to_string())))
        );
    }

    #[test]
    fn correlate_no_match_returns_none() {
        let storage = Storage::open_memory().unwrap();
        let engine = test_engine(Arc::new(storage));
        let storage = engine.storage();

        let unrelated = "Grocery list: eggs, bread, oranges, lettuce, chicken breast.";
        let embedding = engine.embeddings().embed(&[unrelated]).unwrap().remove(0);
        let chunk = ConversationChunk {
            id: "chunk-unrelated".into(),
            conversation_id: "conv-unrelated".into(),
            project_name: "personal".into(),
            timestamp: "2026-07-20T10:00:00Z".into(),
            content: unrelated.to_string(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &embedding).unwrap();

        let plan = PlanDoc {
            slug: "unrelated-plan".into(),
            path: PathBuf::from("/dev/null"),
            mtime: "2026-07-20T12:00:00Z".into(),
            content: distinctive_body("orthogonal"),
        };

        assert_eq!(correlate_project(&plan, storage), None);
    }

    #[test]
    fn correlate_shared_boilerplate_ambiguous_none() {
        let storage = Storage::open_memory().unwrap();
        let engine = test_engine(Arc::new(storage));
        let storage = engine.storage();

        // Same boilerplate preamble seeded into two different projects — a plan that
        // only matches the shared preamble must not be attributed to either.
        let boilerplate = "This specification document begins with a standardized \
            engineering charter preamble referencing organizational governance \
            requirements before any project-specific implementation discussion begins.";

        for (id, conv, project) in [
            ("chunk-a", "conv-a", "project-alpha"),
            ("chunk-b", "conv-b", "project-beta"),
        ] {
            let embedding = engine.embeddings().embed(&[boilerplate]).unwrap().remove(0);
            let chunk = ConversationChunk {
                id: id.into(),
                conversation_id: conv.into(),
                project_name: project.into(),
                timestamp: "2026-07-20T10:00:00Z".into(),
                content: boilerplate.to_string(),
                message_count: 1,
                summary: None,
                author: Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &embedding).unwrap();
        }

        let plan = PlanDoc {
            slug: "boilerplate-plan".into(),
            path: PathBuf::from("/dev/null"),
            mtime: "2026-07-20T12:00:00Z".into(),
            content: boilerplate.to_string(),
        };

        // Margin rule: both hits score identically (same content), gap = 0 < 3.
        assert_eq!(correlate_project(&plan, storage), None);
    }

    #[test]
    fn import_plan_chunks_and_provenance() {
        let storage = Storage::open_memory().unwrap();
        let engine = test_engine(Arc::new(storage));

        let body = distinctive_body("lapi");
        {
            let storage = engine.storage();
            let embedding = engine
                .embeddings()
                .embed(&[body.as_str()])
                .unwrap()
                .remove(0);
            let chunk = ConversationChunk {
                id: "chunk-lapi-origin".into(),
                conversation_id: "conv-lapi-origin".into(),
                project_name: "lapi".into(),
                timestamp: "2026-07-20T10:00:00Z".into(),
                content: body.clone(),
                message_count: 1,
                summary: None,
                author: Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &embedding).unwrap();
        }

        let plan = PlanDoc {
            slug: "lapi-plan".into(),
            path: PathBuf::from("/dev/null"),
            mtime: "2026-07-20T12:00:00Z".into(),
            content: body,
        };

        let created = import_plan(&engine, &plan).unwrap();
        assert!(created > 0);

        let storage = engine.storage();
        let ids = storage
            .get_chunk_ids_for_conversation("plan:lapi-plan")
            .unwrap();
        assert_eq!(ids.len(), created);
        for id in &ids {
            assert_eq!(
                storage.get_chunk_source(id).unwrap().as_deref(),
                Some("plan")
            );
            let prov = storage
                .get_chunk_provenance(id)
                .unwrap()
                .expect("provenance edge");
            assert_eq!(prov.author, Speaker::User);
            assert_eq!(prov.source_conv_id, "conv-lapi-origin");
        }

        // Reimport with shrunk content: old chunks must be gone, new ones present,
        // no duplicates left behind.
        let shrunk = PlanDoc {
            content: "# lapi\n\nShort revised plan.".to_string(),
            ..plan
        };
        let created_again = import_plan(&engine, &shrunk).unwrap();
        let ids_again = storage
            .get_chunk_ids_for_conversation("plan:lapi-plan")
            .unwrap();
        assert_eq!(ids_again.len(), created_again);
        assert!(!ids_again.iter().any(|id| ids.contains(id)) || created_again == created);
        for id in &ids_again {
            let content = storage.get_chunk_content(id).unwrap().unwrap();
            assert!(content.contains("Short revised plan"));
        }
    }

    #[test]
    fn unscoped_plan_gets_sentinel_project() {
        let storage = Storage::open_memory().unwrap();
        let engine = test_engine(Arc::new(storage));

        let plan = PlanDoc {
            slug: "solo-plan".into(),
            path: PathBuf::from("/dev/null"),
            mtime: "2026-07-20T12:00:00Z".into(),
            content: distinctive_body("standalone"),
        };

        let created = import_plan(&engine, &plan).unwrap();
        assert!(created > 0);

        let storage = engine.storage();
        let ids = storage
            .get_chunk_ids_for_conversation("plan:solo-plan")
            .unwrap();
        assert_eq!(ids.len(), created);
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();
        for chunk in &chunks {
            assert_eq!(chunk.project_name, "_unscoped");
        }
        for id in &ids {
            // No correlation -> no provenance edge should have been written.
            assert!(storage.get_chunk_provenance(id).unwrap().is_none());
        }
    }
}
