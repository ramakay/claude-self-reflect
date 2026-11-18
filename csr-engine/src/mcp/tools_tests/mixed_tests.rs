use super::*;

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
    let scope =
        SearchProjectScope::resolve_with(&storage, None, None, Some("EnhanceMe"), None).unwrap();
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
    assert!(
        rows.iter()
            .zip(&before)
            .all(|(a, b)| a.score.to_bits() == b.score.to_bits()
                && a.chunk.content == b.chunk.content)
    );
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
