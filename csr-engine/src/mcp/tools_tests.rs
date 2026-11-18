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

const CSR_RULE_TEXT: &str = "A standing rule from 10-12: “Keep receipts”\nsee libfile in the tree";

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

fn f1b_chunk(id: &str, conversation_id: &str, content: &str) -> crate::import::ConversationChunk {
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
            let wid = witness_ledger::latest_witness_for_symbol(conn, "proj", file, Some(symbol))?
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
    let mk = |id: &str, conv: &str, content: &str, seq: usize| crate::import::ConversationChunk {
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
    let fused = crate::search::hybrid::fuse_rrf_weighted(&[(&semantic, 1.0), (&fts, 1.0)], RRF_K);
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

const S9R_WITNESS_X: &str = "INSERT INTO assertions (source,target_kind,target_id,state,receipt)
         VALUES ('witness','chunk','x','superseded','abc1234');";

fn rerank_fixture() -> (Vec<String>, Vec<EnrichedResult>, HashMap<String, String>) {
    let ids = ["r-a", "r-b", "r-c", "r-d", "r-e"];
    let enriched: Vec<EnrichedResult> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| enriched_result_conv(id, &format!("conv-{i}"), 0.5, i))
        .collect();
    let session_keys: HashMap<String, String> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.to_string(), format!("conv-{i}")))
        .collect();
    (
        ids.iter().map(|id| id.to_string()).collect(),
        enriched,
        session_keys,
    )
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

#[path = "tools_tests/explicit_tests.rs"]
mod explicit_tests;

#[path = "tools_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "tools_tests/f1b_status_tests.rs"]
mod f1b_status_tests;

#[path = "tools_tests/active_chunk_tests.rs"]
mod active_chunk_tests;

#[path = "tools_tests/e2e_receipt_tests.rs"]
mod e2e_receipt_tests;

#[path = "tools_tests/why_recency_tests.rs"]
mod why_recency_tests;

#[path = "tools_tests/exact_id_tests.rs"]
mod exact_id_tests;
