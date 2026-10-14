//! The known-item recall probe: a frozen sample of the user's own earlier
//! messages, each turned into a short deterministic query, searched through
//! the production pipeline. A case is a hit when its conversation (or its
//! proven parent) is in the first five results.
//!
//! The probe never writes anything but diagnostic `meta` (see `health`), and
//! the source chunks stay in the retrieval index: only the query/label pairs
//! are held out.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tokio::sync::RwLock;

use super::{
    HealthSnapshot, MissCause, ProbeCase, ProbeGrade, ProbeMiss, ProbeReport, ProbeSample,
};
use crate::import::ConversationChunk;
use crate::provenance::Speaker;
use crate::search::SearchEngine;
use crate::storage::Storage;
use std::sync::Arc;

/// Cases kept in a frozen sample.
pub const SAMPLE_MAX: usize = 20;
/// Fewer valid cases than this abstains; enrollment stays provisional.
pub const SAMPLE_MIN: usize = 10;
/// Words in a generated query, at most.
pub const QUERY_MAX_WORDS: usize = 12;
/// Result positions that count toward a hit.
pub const TOP_K: usize = 5;
/// Sample format version.
pub const SAMPLE_VERSION: u32 = 1;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "have", "has", "had", "are", "was",
    "were", "not", "but", "you", "your", "our", "they", "their", "then", "than", "into", "about",
    "can", "could", "should", "would", "will",
];

/// Where the probe sends a query. Production runs the real recall pipeline
/// (`mcp::tools`); tests may substitute canned pages.
#[async_trait::async_trait]
pub trait ProbeBackend: Send + Sync {
    /// The conversation ids of the final result page, in result order.
    async fn final_page(&self, query: &str) -> Result<Vec<String>>;
}

/// What `load_or_create_sample` decided.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleLoad {
    pub sample: ProbeSample,
    /// True when the sample is persisted and frozen; false while enrollment is
    /// provisional (fewer than ten cases) and nothing is stored.
    pub frozen: bool,
    /// True when this call created and persisted a new sample.
    pub created: bool,
    /// Set when an explicit refresh could not replace the sample.
    pub note: Option<String>,
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn order_key(chunk_id: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"csr-health-probe-v1\0");
    hasher.update(chunk_id.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Lowercased tokens of letters, numbers and underscore, in order, with
/// short tokens and stopwords dropped.
fn content_tokens(sentence: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if !current.is_empty() {
            let token = current.to_lowercase();
            if token.chars().count() >= 3 && !STOPWORDS.contains(&token.as_str()) {
                tokens.push(token);
            }
            current.clear();
        }
    };
    for ch in sentence.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            flush(&mut current, &mut tokens);
        }
    }
    flush(&mut current, &mut tokens);
    tokens
}

fn document_frequency(conn: &Connection, token: &str) -> Result<u64> {
    let quoted = format!("\"{}\"", token.replace('"', "\"\""));
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
        params![quoted],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

/// The deterministic query for a chunk's text: the highest-scoring sentence's
/// twelve rarest distinct tokens, restored to sentence order. `total_docs` is
/// the chunk count; document frequency comes from `chunks_fts` itself so its
/// own normalization applies. `None` when nothing usable remains.
pub fn query_for_chunk(
    storage: &Storage,
    content: &str,
    total_docs: u64,
) -> Result<Option<String>> {
    storage.with_connection(|conn| query_with_conn(conn, content, total_docs))
}

fn query_with_conn(conn: &Connection, content: &str, total_docs: u64) -> Result<Option<String>> {
    let mut df_cache: HashMap<String, u64> = HashMap::new();
    let mut best: Option<(f64, Vec<String>)> = None;
    for sentence in content.split(['.', '?', '!', '\n']) {
        let mut seen = HashSet::new();
        let distinct: Vec<String> = content_tokens(sentence)
            .into_iter()
            .filter(|token| seen.insert(token.clone()))
            .collect();
        if distinct.is_empty() {
            continue;
        }
        let mut scored: Vec<(usize, f64, String)> = Vec::with_capacity(distinct.len());
        for (position, token) in distinct.into_iter().enumerate() {
            let df = match df_cache.get(&token) {
                Some(df) => *df,
                None => {
                    let df = document_frequency(conn, &token)?;
                    df_cache.insert(token.clone(), df);
                    df
                }
            };
            let idf = ((total_docs as f64 + 1.0) / (df as f64 + 1.0)).ln();
            scored.push((position, idf, token));
        }
        // rarest first; ties keep first occurrence
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(QUERY_MAX_WORDS);
        let score: f64 = scored.iter().map(|(_, idf, _)| idf).sum();
        let better = match &best {
            None => true,
            Some((best_score, _)) => score > *best_score, // earliest wins ties
        };
        if better {
            scored.sort_by_key(|(position, _, _)| *position);
            best = Some((score, scored.into_iter().map(|(_, _, t)| t).collect()));
        }
    }
    Ok(best
        .map(|(_, tokens)| tokens.join(" "))
        .filter(|query| !query.is_empty()))
}

struct Candidate {
    chunk: ConversationChunk,
    is_sidechain: bool,
}

fn eligible_candidates(storage: &Storage, cutoff_at: &str) -> Result<Vec<Candidate>> {
    storage.with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.content, c.timestamp, c.source
             FROM chunks AS c
             JOIN chunk_provenance AS p ON p.chunk_id = c.id
             WHERE c.source IN ('conversation', 'sidechain')
               AND p.author = 'user'
               AND c.tool_result_share IS NOT NULL
               AND c.tool_result_share < 0.2
               AND length(c.content) BETWEEN 200 AND 1500
               AND julianday(c.timestamp) >= julianday(?1, '-7 days')
               AND julianday(c.timestamp) <= julianday(?1)",
        )?;
        let rows = stmt.query_map(params![cutoff_at], |row| {
            let source: String = row.get(5)?;
            Ok(Candidate {
                chunk: ConversationChunk {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    project_name: row.get(2)?,
                    content: row.get(3)?,
                    timestamp: row.get(4)?,
                    message_count: 1,
                    summary: None,
                    // the SQL guarantees author 'user'
                    author: Speaker::User,
                    seq: 0,
                    is_sidechain: source == "sidechain",
                },
                is_sidechain: source == "sidechain",
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
}

/// Select up to twenty cases from user-authored chunks of the seven days
/// before `cutoff_at`, in BLAKE3 order. Chunks with a CSR-text detector hit,
/// unknown/tool-heavy chunks and non-user authors never enter. One case per
/// conversation family and per exact content, for diversity only. A sidechain
/// case carries its proven parent from `chunk_provenance.source_conv_id`.
pub fn select_sample(storage: &Storage, cutoff_at: &str) -> Result<Vec<ProbeCase>> {
    let mut candidates = eligible_candidates(storage, cutoff_at)?;
    candidates.sort_by(|a, b| {
        order_key(&a.chunk.id)
            .cmp(&order_key(&b.chunk.id))
            .then_with(|| a.chunk.id.cmp(&b.chunk.id))
    });
    let total_docs: i64 = storage.with_connection(|conn| {
        Ok(conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?)
    })?;
    let total_docs = total_docs.max(0) as u64;

    let mut cases: Vec<ProbeCase> = Vec::new();
    let mut families: HashSet<String> = HashSet::new();
    let mut hashes: HashSet<String> = HashSet::new();
    for batch in candidates.chunks(64) {
        if cases.len() >= SAMPLE_MAX {
            break;
        }
        let chunks: Vec<ConversationChunk> = batch.iter().map(|c| c.chunk.clone()).collect();
        let hits = storage.csr_text_hits(&chunks);
        for candidate in batch {
            if cases.len() >= SAMPLE_MAX {
                break;
            }
            let chunk = &candidate.chunk;
            if hits.contains_key(&chunk.id) {
                continue;
            }
            let parent = if candidate.is_sidechain {
                storage
                    .parent_session_of_conversation(&chunk.conversation_id)?
                    .filter(|parent| parent != &chunk.conversation_id)
            } else {
                None
            };
            let family = parent
                .clone()
                .unwrap_or_else(|| chunk.conversation_id.clone());
            let hash = content_hash(&chunk.content);
            if families.contains(&family) || hashes.contains(&hash) {
                continue;
            }
            let Some(query) = query_for_chunk(storage, &chunk.content, total_docs)? else {
                continue;
            };
            families.insert(family);
            hashes.insert(hash.clone());
            cases.push(ProbeCase {
                chunk_id: chunk.id.clone(),
                conversation_id: chunk.conversation_id.clone(),
                parent_conversation_id: parent,
                content_hash: hash,
                query,
            });
        }
    }
    Ok(cases)
}

fn new_sample(cutoff_at: &str, cases: Vec<ProbeCase>) -> ProbeSample {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"csr-health-sample-v1\0");
    hasher.update(cutoff_at.as_bytes());
    for case in &cases {
        hasher.update(case.chunk_id.as_bytes());
        hasher.update(b"\0");
    }
    let id = hasher.finalize().to_hex().to_string();
    ProbeSample {
        version: SAMPLE_VERSION,
        sample_id: format!("ps-{}", &id[..12]),
        created_at: cutoff_at.to_string(),
        cutoff_at: cutoff_at.to_string(),
        cases,
    }
}

/// The frozen sample, or a new one. With a frozen sample stored and no
/// `refresh`, the stored sample is returned untouched whatever was imported
/// since. Fewer than ten eligible cases keep enrollment provisional: nothing
/// is stored and a later run may enroll newer data. `refresh` re-runs the
/// same selection at `now`, never consulting scores or misses; it replaces the
/// sample only when it finds at least ten cases. This function does not
/// write: callers persist a `created` sample with `save_health`.
pub fn load_or_create_sample(
    storage: &Storage,
    snapshot: &HealthSnapshot,
    refresh: bool,
    now: DateTime<Utc>,
) -> Result<SampleLoad> {
    let stored = snapshot
        .sample
        .clone()
        .filter(|sample| sample.cases.len() >= SAMPLE_MIN);
    if let (Some(sample), false) = (&stored, refresh) {
        return Ok(SampleLoad {
            sample: sample.clone(),
            frozen: true,
            created: false,
            note: None,
        });
    }
    let cutoff_at = now.to_rfc3339();
    let cases = select_sample(storage, &cutoff_at)?;
    let usable = cases.len() >= SAMPLE_MIN;
    match (stored, usable) {
        (_, true) => Ok(SampleLoad {
            sample: new_sample(&cutoff_at, cases),
            frozen: true,
            created: true,
            note: None,
        }),
        (Some(sample), false) => Ok(SampleLoad {
            sample,
            frozen: true,
            created: false,
            note: Some(format!(
                "refresh found {} eligible cases, fewer than {SAMPLE_MIN}; kept the existing sample",
                cases.len()
            )),
        }),
        (None, false) => Ok(SampleLoad {
            sample: ProbeSample {
                version: SAMPLE_VERSION,
                sample_id: String::new(),
                created_at: cutoff_at.clone(),
                cutoff_at,
                cases,
            },
            frozen: false,
            created: false,
            note: None,
        }),
    }
}

/// Integer thresholds on the cases actually evaluated.
pub fn grade_recall(hits: usize, n: usize) -> ProbeGrade {
    if n < SAMPLE_MIN {
        ProbeGrade::Abstain
    } else if 100 * hits >= 85 * n {
        ProbeGrade::Green
    } else if 100 * hits >= 70 * n {
        ProbeGrade::Amber
    } else {
        ProbeGrade::Red
    }
}

/// Why a case was missed. Never claims a ranking regression: `NotInTop5` only
/// says the conversation was absent although embedding and index entry exist.
pub fn diagnose_miss(
    storage: &Storage,
    search: &SearchEngine,
    chunk_id: &str,
) -> Result<MissCause> {
    let has_embedding: bool = storage.with_connection(|conn| {
        Ok(conn
            .query_row(
                "SELECT 1 FROM chunk_embeddings WHERE chunk_id = ?1",
                params![chunk_id],
                |_| Ok(()),
            )
            .is_ok())
    })?;
    Ok(if !has_embedding {
        MissCause::EmbeddingMissing
    } else if !search.has_chunk(chunk_id) {
        MissCause::IndexMissing
    } else {
        MissCause::NotInTop5
    })
}

/// A case is valid while its chunk exists with the content that was sampled.
fn is_valid(storage: &Storage, case: &ProbeCase) -> Result<bool> {
    let content: Option<String> = storage.with_connection(|conn| {
        Ok(conn
            .query_row(
                "SELECT content FROM chunks WHERE id = ?1",
                params![case.chunk_id],
                |row| row.get(0),
            )
            .ok())
    })?;
    Ok(content.is_some_and(|content| content_hash(&content) == case.content_hash))
}

fn is_hit(case: &ProbeCase, page: &[String]) -> bool {
    page.iter().take(TOP_K).any(|conversation| {
        conversation == &case.conversation_id
            || case.parent_conversation_id.as_deref() == Some(conversation.as_str())
    })
}

/// Run the cases. Deleted or changed chunks are invalidated, never replaced.
/// A search or embedding failure returns an `Error` report with the full
/// denominator: it is neither a smaller `n` nor "no data".
pub async fn run_probe(
    storage: &Storage,
    search: &Arc<RwLock<SearchEngine>>,
    backend: &dyn ProbeBackend,
    sample_id: Option<&str>,
    cases: &[ProbeCase],
    now: DateTime<Utc>,
) -> ProbeReport {
    let at = now.to_rfc3339();
    let mut valid: Vec<&ProbeCase> = Vec::new();
    let mut invalidated = 0;
    for case in cases {
        match is_valid(storage, case) {
            Ok(true) => valid.push(case),
            Ok(false) => invalidated += 1,
            Err(error) => {
                return ProbeReport {
                    at,
                    sample_id: sample_id.map(str::to_string),
                    hits: 0,
                    n: cases.len(),
                    invalidated,
                    grade: ProbeGrade::Error,
                    misses: Vec::new(),
                    error: Some(format!("revalidation failed: {error}")),
                };
            }
        }
    }
    let n = valid.len();
    let mut hits = 0;
    let mut misses = Vec::new();
    for case in valid {
        let page = match backend.final_page(&case.query).await {
            Ok(page) => page,
            Err(error) => {
                return ProbeReport {
                    at,
                    sample_id: sample_id.map(str::to_string),
                    hits,
                    n,
                    invalidated,
                    grade: ProbeGrade::Error,
                    misses,
                    error: Some(format!("search failed: {error}")),
                };
            }
        };
        if is_hit(case, &page) {
            hits += 1;
            continue;
        }
        let cause = {
            let engine = search.read().await;
            diagnose_miss(storage, &engine, &case.chunk_id).unwrap_or(MissCause::NotInTop5)
        };
        misses.push(ProbeMiss {
            chunk_id: case.chunk_id.clone(),
            conversation_id: case.conversation_id.clone(),
            returned: page.into_iter().take(TOP_K).collect(),
            cause,
        });
    }
    ProbeReport {
        at,
        sample_id: sample_id.map(str::to_string),
        hits,
        n,
        invalidated,
        grade: grade_recall(hits, n),
        misses,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{HealthSnapshot, ProbeCase};

    const NOW: &str = "2026-10-13T12:00:00Z";
    const RECENT: &str = "2026-10-10T00:00:00Z";

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn body(i: usize) -> String {
        format!(
            "Investigating the migration planner behaviour number {i}. The scheduler widget{i} keeps \
             stalling after zebra{i} deployments finish. Please check quokka{i} handling carefully \
             before shipping anything to production today for everyone involved here."
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn seed(
        storage: &Storage,
        id: &str,
        conversation: &str,
        content: &str,
        author: &str,
        share: Option<f64>,
        source: &str,
        timestamp: &str,
    ) {
        let chunk = ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "p".into(),
            timestamp: timestamp.into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: source == "sidechain",
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET tool_result_share = ?2, source = ?3 WHERE id = ?1",
                    params![id, share, source],
                )?;
                conn.execute(
                    "INSERT OR REPLACE INTO chunk_provenance (chunk_id, author, source_conv_id, supersedes)
                     VALUES (?1, ?2, ?3, NULL)",
                    params![id, author, conversation],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_users(storage: &Storage, count: usize) {
        for i in 0..count {
            seed(
                storage,
                &format!("u{i}"),
                &format!("conv-{i}"),
                &body(i),
                "user",
                Some(0.0),
                "conversation",
                RECENT,
            );
        }
    }

    fn case(i: usize) -> ProbeCase {
        ProbeCase {
            chunk_id: format!("u{i}"),
            conversation_id: format!("conv-{i}"),
            parent_conversation_id: None,
            content_hash: content_hash(&body(i)),
            query: format!("scheduler widget{i}"),
        }
    }

    struct Canned {
        pages: HashMap<String, Vec<String>>,
        fail_on: Option<String>,
    }

    #[async_trait::async_trait]
    impl ProbeBackend for Canned {
        async fn final_page(&self, query: &str) -> Result<Vec<String>> {
            if self.fail_on.as_deref() == Some(query) {
                anyhow::bail!("embedding unavailable");
            }
            Ok(self.pages.get(query).cloned().unwrap_or_default())
        }
    }

    fn engine() -> Arc<RwLock<SearchEngine>> {
        Arc::new(RwLock::new(SearchEngine::new(8)))
    }

    #[test]
    fn probe_selects_author_user_not_person() {
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 12);
        for (i, author) in ["assistant", "person", "tool_result"].iter().enumerate() {
            seed(
                &storage,
                &format!("x{i}"),
                &format!("other-{i}"),
                &body(100 + i),
                author,
                Some(0.0),
                "conversation",
                RECENT,
            );
        }
        let cases = select_sample(&storage, NOW).unwrap();
        assert_eq!(cases.len(), 12);
        assert!(
            cases.iter().all(|c| c.chunk_id.starts_with('u')),
            "{cases:?}"
        );
    }

    #[test]
    fn probe_excludes_unknown_tool_share_and_contamination() {
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 10);
        seed(
            &storage,
            "null-share",
            "c-null",
            &body(50),
            "user",
            None,
            "conversation",
            RECENT,
        );
        seed(
            &storage,
            "tool-heavy",
            "c-tool",
            &body(51),
            "user",
            Some(0.5),
            "conversation",
            RECENT,
        );
        let marker = format!("{}\n[[CSR:AGENTS:v1]]\n{{}}\n[[/CSR:AGENTS:v1]]", body(52));
        seed(
            &storage,
            "csr-text",
            "c-csr",
            &marker,
            "user",
            Some(0.0),
            "conversation",
            RECENT,
        );
        seed(
            &storage,
            "too-short",
            "c-short",
            "short text only",
            "user",
            Some(0.0),
            "conversation",
            RECENT,
        );
        seed(
            &storage,
            "too-old",
            "c-old",
            &body(53),
            "user",
            Some(0.0),
            "conversation",
            "2026-09-01T00:00:00Z",
        );
        seed(
            &storage,
            "plan-src",
            "c-plan",
            &body(54),
            "user",
            Some(0.0),
            "plan",
            RECENT,
        );
        let cases = select_sample(&storage, NOW).unwrap();
        let ids: Vec<&str> = cases.iter().map(|c| c.chunk_id.as_str()).collect();
        for excluded in [
            "null-share",
            "tool-heavy",
            "csr-text",
            "too-short",
            "too-old",
            "plan-src",
        ] {
            assert!(
                !ids.contains(&excluded),
                "{excluded} must not enroll: {ids:?}"
            );
        }
        assert_eq!(cases.len(), 10);
    }

    #[test]
    fn probe_sample_is_stable_across_imports_and_runs() {
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 12);
        let first =
            load_or_create_sample(&storage, &HealthSnapshot::default(), false, now()).unwrap();
        assert!(first.frozen && first.created);
        let snapshot = HealthSnapshot {
            sample: Some(first.sample.clone()),
            ..Default::default()
        };
        for i in 12..20 {
            seed(
                &storage,
                &format!("u{i}"),
                &format!("conv-{i}"),
                &body(i),
                "user",
                Some(0.0),
                "conversation",
                RECENT,
            );
        }
        let later = now() + chrono::Duration::hours(30);
        let second = load_or_create_sample(&storage, &snapshot, false, later).unwrap();
        assert!(second.frozen && !second.created);
        assert_eq!(
            second.sample, first.sample,
            "new imports and later runs keep the sample"
        );
    }

    #[test]
    fn probe_refresh_is_explicit_and_score_independent() {
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 12);
        let first =
            load_or_create_sample(&storage, &HealthSnapshot::default(), false, now()).unwrap();
        // a history full of misses must not change which cases are chosen
        let mut snapshot = HealthSnapshot {
            sample: Some(first.sample.clone()),
            ..Default::default()
        };
        snapshot.runs = vec![crate::health::RunRecord {
            at: NOW.into(),
            sample_id: Some(first.sample.sample_id.clone()),
            hits: 0,
            n: 12,
            invalidated: 0,
            grade: ProbeGrade::Red,
            misses: (0..12)
                .map(|i| ProbeMiss {
                    chunk_id: format!("u{i}"),
                    conversation_id: format!("conv-{i}"),
                    returned: vec![],
                    cause: MissCause::NotInTop5,
                })
                .collect(),
            duplicate_rate: None,
        }];
        let later = now() + chrono::Duration::hours(1);
        let refreshed = load_or_create_sample(&storage, &snapshot, true, later).unwrap();
        assert!(refreshed.created, "an explicit refresh makes a new sample");
        assert_ne!(refreshed.sample.sample_id, first.sample.sample_id);
        let ids = |s: &ProbeSample| {
            s.cases
                .iter()
                .map(|c| c.chunk_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&refreshed.sample),
            ids(&first.sample),
            "selection ignores scores and misses"
        );
        // without the flag nothing is replaced
        let kept = load_or_create_sample(&storage, &snapshot, false, later).unwrap();
        assert_eq!(kept.sample, first.sample);
        // a refresh with too little data keeps the frozen sample and says so
        let empty = Storage::open_memory().unwrap();
        let kept = load_or_create_sample(&empty, &snapshot, true, later).unwrap();
        assert_eq!(kept.sample, first.sample);
        assert!(!kept.created && kept.note.is_some());
    }

    #[test]
    fn probe_query_is_frozen_and_at_most_twelve_words() {
        let storage = Storage::open_memory().unwrap();
        let long: String = (0..30).map(|i| format!("alphaword{i} ")).collect();
        let content = format!(
            "{long}. {}",
            "padding sentence about nothing in particular repeated to reach the minimum length for \
             enrollment into the sample so the chunk is eligible for selection by the probe"
        );
        seed(
            &storage,
            "long",
            "conv-long",
            &content,
            "user",
            Some(0.0),
            "conversation",
            RECENT,
        );
        seed_users(&storage, 11);
        let cases = select_sample(&storage, NOW).unwrap();
        let long_case = cases
            .iter()
            .find(|c| c.chunk_id == "long")
            .expect("long enrolled");
        assert_eq!(long_case.query.split(' ').count(), QUERY_MAX_WORDS);
        assert!(cases
            .iter()
            .all(|c| !c.query.is_empty() && c.query.split(' ').count() <= QUERY_MAX_WORDS));
        // stopwords and short tokens never appear
        assert!(!long_case
            .query
            .split(' ')
            .any(|w| STOPWORDS.contains(&w) || w.len() < 3));

        // frozen: later imports change document frequencies, not the stored query
        let loaded =
            load_or_create_sample(&storage, &HealthSnapshot::default(), false, now()).unwrap();
        let snapshot = HealthSnapshot {
            sample: Some(loaded.sample.clone()),
            ..Default::default()
        };
        for i in 0..40 {
            seed(
                &storage,
                &format!("n{i}"),
                &format!("n-{i}"),
                &content,
                "assistant",
                None,
                "conversation",
                RECENT,
            );
        }
        let again = load_or_create_sample(&storage, &snapshot, false, now()).unwrap();
        assert_eq!(again.sample, loaded.sample);
    }

    #[tokio::test]
    async fn probe_requires_expected_conversation_or_proven_parent_in_top5() {
        let storage = Storage::open_memory().unwrap();
        // 10 ordinary cases plus one sidechain case whose parent is proven
        seed_users(&storage, 10);
        seed(
            &storage,
            "side",
            "agent-1",
            &body(77),
            "user",
            Some(0.0),
            "sidechain",
            RECENT,
        );
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunk_provenance SET source_conv_id = 'parent-session' WHERE chunk_id = 'side'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let cases = select_sample(&storage, NOW).unwrap();
        let side = cases
            .iter()
            .find(|c| c.chunk_id == "side")
            .expect("sidechain enrolled");
        assert_eq!(
            side.parent_conversation_id.as_deref(),
            Some("parent-session")
        );
        let plain = cases.iter().find(|c| c.chunk_id == "u0").unwrap();
        assert_eq!(plain.parent_conversation_id, None);

        let page = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_hit(plain, &page(&["a", "b", "conv-0"])));
        assert!(
            !is_hit(plain, &page(&["a", "b", "c", "d", "e", "conv-0"])),
            "position six is not top five"
        );
        assert!(
            !is_hit(plain, &page(&["conv-1", "agent-9"])),
            "an unrelated conversation is no hit"
        );
        assert!(
            is_hit(side, &page(&["parent-session"])),
            "the proven parent counts"
        );
        assert!(is_hit(side, &page(&["x", "agent-1"])));
        assert!(
            !is_hit(side, &page(&["agent-2", "other-session"])),
            "another child or session is no hit"
        );

        // through the runner: only the first five positions count
        let mut pages = HashMap::new();
        pages.insert(
            plain.query.clone(),
            page(&["a", "b", "c", "d", "e", "conv-0"]),
        );
        pages.insert(side.query.clone(), page(&["parent-session"]));
        let backend = Canned {
            pages,
            fail_on: None,
        };
        let report = run_probe(
            &storage,
            &engine(),
            &backend,
            Some("s"),
            &[plain.clone(), side.clone()],
            now(),
        )
        .await;
        assert_eq!((report.hits, report.n), (1, 2));
        assert_eq!(report.misses.len(), 1);
        assert_eq!(report.misses[0].returned.len(), TOP_K);
    }

    #[test]
    fn probe_thresholds_use_actual_n() {
        use ProbeGrade::*;
        let table = [
            (10, 10, Green),
            (9, 10, Green),
            (8, 10, Amber),
            (7, 10, Amber),
            (6, 10, Red),
            (11, 11, Green),
            (10, 11, Green),
            (9, 11, Amber),
            (8, 11, Amber),
            (7, 11, Red),
            (20, 20, Green),
            (17, 20, Green),
            (16, 20, Amber),
            (14, 20, Amber),
            (13, 20, Red),
            (0, 20, Red),
        ];
        for (hits, n, want) in table {
            assert_eq!(grade_recall(hits, n), want, "{hits}/{n}");
        }
    }

    #[tokio::test]
    async fn probe_abstains_below_ten() {
        assert_eq!(grade_recall(9, 9), ProbeGrade::Abstain);
        assert_eq!(grade_recall(0, 0), ProbeGrade::Abstain);
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 9);
        let cases: Vec<ProbeCase> = (0..9).map(case).collect();
        let pages = cases
            .iter()
            .map(|c| (c.query.clone(), vec![c.conversation_id.clone()]))
            .collect();
        let backend = Canned {
            pages,
            fail_on: None,
        };
        let report = run_probe(&storage, &engine(), &backend, None, &cases, now()).await;
        assert_eq!(
            (report.hits, report.n, report.grade),
            (9, 9, ProbeGrade::Abstain)
        );
        let empty = run_probe(&storage, &engine(), &backend, None, &[], now()).await;
        assert_eq!((empty.n, empty.grade), (0, ProbeGrade::Abstain));
    }

    #[tokio::test]
    async fn probe_invalidated_cases_are_not_replaced() {
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 14);
        let cases: Vec<ProbeCase> = (0..12).map(case).collect();
        // delete one sampled chunk, change another's text
        storage
            .with_connection(|conn| {
                conn.execute("DELETE FROM chunk_provenance WHERE chunk_id = 'u0'", [])?;
                conn.execute("DELETE FROM chunk_embeddings WHERE chunk_id = 'u0'", [])?;
                conn.execute("DELETE FROM chunks WHERE id = 'u0'", [])?;
                conn.execute(
                    "UPDATE chunks SET content = content || ' edited' WHERE id = 'u1'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let pages = cases
            .iter()
            .map(|c| (c.query.clone(), vec![c.conversation_id.clone()]))
            .collect();
        let backend = Canned {
            pages,
            fail_on: None,
        };
        let report = run_probe(&storage, &engine(), &backend, Some("s"), &cases, now()).await;
        assert_eq!(report.invalidated, 2);
        assert_eq!(
            report.n, 10,
            "the two were dropped, u12 and u13 did not replace them"
        );
        assert_eq!(report.hits, 10);
        assert_eq!(report.grade, ProbeGrade::Green);
    }

    #[tokio::test]
    async fn probe_errors_do_not_shrink_the_denominator() {
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 12);
        let cases: Vec<ProbeCase> = (0..12).map(case).collect();
        let pages = cases
            .iter()
            .map(|c| (c.query.clone(), vec![c.conversation_id.clone()]))
            .collect();
        let backend = Canned {
            pages,
            fail_on: Some(cases[3].query.clone()),
        };
        let report = run_probe(&storage, &engine(), &backend, Some("s"), &cases, now()).await;
        assert_eq!(report.grade, ProbeGrade::Error);
        assert_eq!(report.n, 12, "the denominator stays whole");
        assert!(report
            .error
            .as_deref()
            .unwrap()
            .contains("embedding unavailable"));
        assert_eq!(report.hits, 3, "only what was scored before the failure");
    }

    #[tokio::test]
    async fn probe_miss_causes_distinguish_embedding_index_and_rank() {
        let storage = Storage::open_memory().unwrap();
        seed_users(&storage, 3);
        storage
            .with_connection(|conn| {
                conn.execute("DELETE FROM chunk_embeddings WHERE chunk_id = 'u0'", [])?;
                Ok(())
            })
            .unwrap();
        let mut index = SearchEngine::new(8);
        // u1 has an embedding but is not in the index; u2 is in the index
        index.insert_chunk("u2".to_string(), vec![1.0, 0.0, 0.0, 0.0]);
        let search = Arc::new(RwLock::new(index));
        let cases: Vec<ProbeCase> = (0..3).map(case).collect();
        let backend = Canned {
            pages: HashMap::new(),
            fail_on: None,
        };
        let report = run_probe(&storage, &search, &backend, Some("s"), &cases, now()).await;
        let cause = |id: &str| {
            report
                .misses
                .iter()
                .find(|m| m.chunk_id == id)
                .unwrap()
                .cause
        };
        assert_eq!(cause("u0"), MissCause::EmbeddingMissing);
        assert_eq!(cause("u1"), MissCause::IndexMissing);
        assert_eq!(cause("u2"), MissCause::NotInTop5);
    }
}
