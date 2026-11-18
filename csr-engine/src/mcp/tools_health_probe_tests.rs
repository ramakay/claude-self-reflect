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
                            cells.push(format!("{:?}", row.get::<_, rusqlite::types::Value>(i)?));
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
        resolve_search_scope_with(&storage, Some("all"), Some("proj-home"), None, None).unwrap();
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
