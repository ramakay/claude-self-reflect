use super::*;
use crate::storage::migrations;

fn mem() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrations::run(&conn).unwrap();
    conn
}

fn node(id: &str, file: &str, kind: &str, name: &str, conv: &str) -> NodeRow {
    NodeRow {
        id: id.into(),
        repo: "repo".into(),
        project: "proj".into(),
        file: file.into(),
        lang: "rust".into(),
        kind: kind.into(),
        name: name.into(),
        body_hash: "h".into(),
        first_conv_id: conv.into(),
        last_conv_id: conv.into(),
        last_session_id: "sess".into(),
        ..NodeRow::default()
    }
}

#[test]
fn upsert_preserves_first_conv_id() {
    let conn = mem();
    upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
    // Re-upsert with a new conv and body — first_conv_id must stay conv_A.
    let mut n = node("n1", "a.rs", "function", "foo", "conv_B");
    n.body_hash = "h2".into();
    upsert_node(&conn, &n).unwrap();
    let got = get_node(&conn, "n1").unwrap().unwrap();
    assert_eq!(got.first_conv_id, "conv_A", "first conv is immutable");
    assert_eq!(got.last_conv_id, "conv_B", "last conv updates");
    assert_eq!(got.body_hash, "h2");
}

#[test]
fn replace_file_edges_is_per_file() {
    let conn = mem();
    let e1 = EdgeRow {
        src_id: "n1".into(),
        dst_id: "name:bar".into(),
        kind: "calls".into(),
        src_file: "a.rs".into(),
        weight: 1.0,
        ..EdgeRow::default()
    };
    replace_file_edges(&conn, "proj", "a.rs", std::slice::from_ref(&e1)).unwrap();
    // Replacing a.rs's edges with an empty set wipes only a.rs.
    let e_other = EdgeRow {
        src_id: "n2".into(),
        dst_id: "name:baz".into(),
        kind: "calls".into(),
        src_file: "b.rs".into(),
        weight: 1.0,
        ..EdgeRow::default()
    };
    replace_file_edges(&conn, "proj", "b.rs", std::slice::from_ref(&e_other)).unwrap();
    replace_file_edges(&conn, "proj", "a.rs", &[]).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "only b.rs edge remains");
}

#[test]
fn replacing_shared_path_edges_keeps_the_other_project() {
    use crate::extraction::codegraph::extract_graph_fragment_for_file;

    let conn = mem();
    let file = "/shared/src/lib.rs";
    let [first, second] = ["project-a", "project-b"].map(|project| {
        // The live extractor and import backfill use the project as the node
        // ID namespace even when two projects share a canonical file path.
        let fragment = extract_graph_fragment_for_file(
            "fn caller() { helper(); }",
            file,
            project,
            project,
            "conversation",
            "session",
        );
        assert!(fragment.parse_clean);
        assert!(fragment.edges.iter().any(|edge| edge.kind == "calls"));
        for node in &fragment.nodes {
            upsert_node(&conn, node).unwrap();
        }
        fragment.edges
    });
    let stored_projects = || -> Vec<String> {
        conn.prepare(
            "SELECT DISTINCT n.project FROM code_edges e
             JOIN code_nodes n ON n.id = e.src_id ORDER BY n.project",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    replace_file_edges(&conn, "project-a", file, &first).unwrap();
    replace_file_edges(&conn, "project-b", file, &second).unwrap();
    assert_eq!(stored_projects(), ["project-a", "project-b"]);

    // Re-extraction with no edges retires just the current project's edges.
    replace_file_edges(&conn, "project-a", file, &[]).unwrap();
    assert_eq!(stored_projects(), ["project-b"]);

    // The graph snapshot's unscoped caller can still replace the entire file.
    replace_file_edges(&conn, "", file, &[]).unwrap();
    assert!(stored_projects().is_empty());
}

#[test]
fn file_ledger_holds_symbols_and_history() {
    let conn = mem();
    upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
    crate::storage::queries::insert_code_evolution(
        &conn,
        "sess",
        "proj",
        "a.rs",
        "rust",
        "Edit",
        "[\"foo\"]",
        "[]",
        "[]",
        "[]",
        "[]",
        "[]",
        None,
    )
    .unwrap();
    let ledger = file_ledger(&conn, "proj", "a.rs").unwrap();
    assert_eq!(ledger.symbols.len(), 1);
    assert_eq!(ledger.symbols[0].name, "foo");
    assert_eq!(ledger.symbols[0].first_conv_id, "conv_A");
    assert_eq!(ledger.timeline.len(), 1);
    assert_eq!(ledger.timeline[0].tool_name, "Edit");
}

#[test]
fn query_callers_classifies_name_only_vs_definition() {
    let conn = mem();
    upsert_node(
        &conn,
        &node("target1", "t.rs", "function", "target_fn", "conv_T"),
    )
    .unwrap();
    // Caller that resolved directly to the real id.
    upsert_node(
        &conn,
        &node("caller_resolved", "a.rs", "function", "caller_a", "conv_A"),
    )
    .unwrap();
    replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[EdgeRow {
            src_id: "caller_resolved".into(),
            dst_id: "target1".into(),
            kind: "calls".into(),
            src_file: "a.rs".into(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();
    // Caller that only matched via the bare-name placeholder.
    upsert_node(
        &conn,
        &node(
            "caller_placeholder",
            "b.rs",
            "function",
            "caller_b",
            "conv_B",
        ),
    )
    .unwrap();
    replace_file_edges(
        &conn,
        "proj",
        "b.rs",
        &[EdgeRow {
            src_id: "caller_placeholder".into(),
            dst_id: "name:target_fn".into(),
            kind: "calls".into(),
            src_file: "b.rs".into(),
            resolved: 0,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();

    let callers = query_callers(&conn, "target_fn", "proj", 20).unwrap();
    let resolved_caller = callers.iter().find(|n| n.id == "caller_resolved").unwrap();
    assert!(
        !resolved_caller.name_only,
        "definition-backed match must not be name_only"
    );
    let placeholder_caller = callers
        .iter()
        .find(|n| n.id == "caller_placeholder")
        .unwrap();
    assert!(
        placeholder_caller.name_only,
        "placeholder-only match must be name_only"
    );
}

#[test]
fn query_callers_scopes_caller_by_project() {
    let conn = mem();
    upsert_node(
        &conn,
        &node("target1", "t.rs", "function", "target_fn", "conv_T"),
    )
    .unwrap();
    // Caller in a DIFFERENT project — must not leak into a "proj"-scoped query.
    let mut other = node("caller_other", "x.rs", "function", "caller_x", "conv_X");
    other.project = "other".into();
    upsert_node(&conn, &other).unwrap();
    replace_file_edges(
        &conn,
        "other",
        "x.rs",
        &[EdgeRow {
            src_id: "caller_other".into(),
            dst_id: "name:target_fn".into(),
            kind: "calls".into(),
            src_file: "x.rs".into(),
            resolved: 0,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();

    let callers = query_callers(&conn, "target_fn", "proj", 20).unwrap();
    assert!(
        callers.iter().all(|n| n.id != "caller_other"),
        "caller from another project must not leak: {:?}",
        callers.iter().map(|n| &n.id).collect::<Vec<_>>()
    );
}

#[test]
fn query_callers_includes_callers_of_every_family_definition() {
    let conn = mem();
    let base = "claude-self-reflect";
    let alias = "claude-self-reflect-csr-engine";

    let mut base_def = node(
        "target_base",
        "src/hooks/session_start.rs",
        "function",
        "is_csr_emission",
        "conv_T1",
    );
    base_def.project = base.into();
    upsert_node(&conn, &base_def).unwrap();

    let mut alias_def = node(
        "target_alias",
        "src/hooks/mod.rs",
        "function",
        "is_csr_emission",
        "conv_T2",
    );
    alias_def.project = alias.into();
    upsert_node(&conn, &alias_def).unwrap();

    let mut base_caller = node(
        "caller_base",
        "src/hooks/stop.rs",
        "function",
        "base_caller",
        "conv_C1",
    );
    base_caller.project = base.into();
    upsert_node(&conn, &base_caller).unwrap();
    replace_file_edges(
        &conn,
        base,
        &base_caller.file,
        &[EdgeRow {
            src_id: base_caller.id.clone(),
            dst_id: base_def.id.clone(),
            kind: "calls".into(),
            src_file: base_caller.file.clone(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();

    let mut alias_caller = node(
        "caller_alias",
        "src/search/rerank.rs",
        "function",
        "is_scaffold_text",
        "conv_C2",
    );
    alias_caller.project = alias.into();
    upsert_node(&conn, &alias_caller).unwrap();
    replace_file_edges(
        &conn,
        alias,
        &alias_caller.file,
        &[EdgeRow {
            src_id: alias_caller.id.clone(),
            dst_id: alias_def.id.clone(),
            kind: "calls".into(),
            src_file: alias_caller.file.clone(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();

    let family = vec![base.to_string(), alias.to_string()];
    let callers = query_callers_in_projects(&conn, "is_csr_emission", &family, 20).unwrap();
    let ids: Vec<&str> = callers.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids, vec!["caller_alias", "caller_base"]);

    let neighbors =
        query_neighbors_in_projects(&conn, "target_base", &family, Some("calls"), 20).unwrap();
    let inbound_ids: Vec<&str> = neighbors
        .iter()
        .filter(|edge| edge.direction == "in")
        .map(|edge| edge.node.id.as_str())
        .collect();
    assert_eq!(inbound_ids, vec!["caller_alias", "caller_base"]);
}

#[test]
fn selected_definition_does_not_merge_unrelated_same_name_callees_or_neighbors() {
    let conn = mem();
    let family = vec!["proj".to_string(), "proj-subdir".to_string()];

    for mut definition in [
        node(
            "selected",
            "src/selected.rs",
            "function",
            "duplicate",
            "conv_S",
        ),
        node(
            "unrelated",
            "src/unrelated.rs",
            "function",
            "duplicate",
            "conv_U",
        ),
    ] {
        definition.project = "proj".into();
        upsert_node(&conn, &definition).unwrap();
    }
    for callee in [
        node("wanted", "src/wanted.rs", "function", "wanted", "conv_W"),
        node("wrong", "src/wrong.rs", "function", "wrong", "conv_X"),
    ] {
        upsert_node(&conn, &callee).unwrap();
    }
    replace_file_edges(
        &conn,
        "proj",
        "src/selected.rs",
        &[EdgeRow {
            src_id: "selected".into(),
            dst_id: "wanted".into(),
            kind: "calls".into(),
            src_file: "src/selected.rs".into(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();
    replace_file_edges(
        &conn,
        "proj",
        "src/unrelated.rs",
        &[EdgeRow {
            src_id: "unrelated".into(),
            dst_id: "wrong".into(),
            kind: "calls".into(),
            src_file: "src/unrelated.rs".into(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();

    let callees = query_callees_in_projects(&conn, "selected", &family, 20).unwrap();
    assert_eq!(
        callees
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        vec!["wanted"]
    );

    let neighbors =
        query_neighbors_in_projects(&conn, "selected", &family, Some("calls"), 20).unwrap();
    let outbound = neighbors
        .iter()
        .filter(|edge| edge.direction == "out")
        .map(|edge| edge.node.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(outbound, vec!["wanted"]);
}

#[test]
fn explicit_family_queries_reject_out_of_family_exact_anchor_ids() {
    let conn = mem();
    let family = vec!["family-root".to_string()];

    for mut row in [
        node(
            "outside-target",
            "src/outside_target.rs",
            "function",
            "outside_target",
            "conv-O1",
        ),
        node(
            "outside-source",
            "src/outside_source.rs",
            "function",
            "outside_source",
            "conv-O2",
        ),
    ] {
        row.project = "other-project".into();
        upsert_node(&conn, &row).unwrap();
    }
    for mut row in [
        node(
            "family-caller",
            "src/family_caller.rs",
            "function",
            "family_caller",
            "conv-F1",
        ),
        node(
            "family-callee",
            "src/family_callee.rs",
            "function",
            "family_callee",
            "conv-F2",
        ),
    ] {
        row.project = "family-root".into();
        upsert_node(&conn, &row).unwrap();
    }
    replace_file_edges(
        &conn,
        "family-root",
        "src/family_caller.rs",
        &[EdgeRow {
            src_id: "family-caller".into(),
            dst_id: "outside-target".into(),
            kind: "calls".into(),
            src_file: "src/family_caller.rs".into(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();
    replace_file_edges(
        &conn,
        "other-project",
        "src/outside_source.rs",
        &[EdgeRow {
            src_id: "outside-source".into(),
            dst_id: "family-callee".into(),
            kind: "calls".into(),
            src_file: "src/outside_source.rs".into(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }],
    )
    .unwrap();

    assert!(
        query_callers_in_projects(&conn, "outside-target", &family, 20)
            .unwrap()
            .is_empty()
    );
    assert!(
        query_callees_in_projects(&conn, "outside-source", &family, 20)
            .unwrap()
            .is_empty()
    );
    assert!(
        query_neighbors_in_projects(&conn, "outside-source", &family, None, 20)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn query_callees_marks_unresolved_name_only() {
    let conn = mem();
    upsert_node(
        &conn,
        &node("caller1", "a.rs", "function", "caller_fn", "conv_A"),
    )
    .unwrap();
    upsert_node(
        &conn,
        &node("callee1", "b.rs", "function", "callee_fn", "conv_B"),
    )
    .unwrap();
    replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[
            EdgeRow {
                src_id: "caller1".into(),
                dst_id: "callee1".into(),
                kind: "calls".into(),
                src_file: "a.rs".into(),
                resolved: 1,
                weight: 1.0,
                ..EdgeRow::default()
            },
            EdgeRow {
                src_id: "caller1".into(),
                dst_id: "name:ghost_fn".into(),
                kind: "calls".into(),
                src_file: "a.rs".into(),
                resolved: 0,
                weight: 1.0,
                ..EdgeRow::default()
            },
        ],
    )
    .unwrap();

    let callees = query_callees(&conn, "caller1", 20).unwrap();
    let resolved = callees.iter().find(|n| n.id == "callee1").unwrap();
    assert!(!resolved.name_only, "resolved callee must not be name_only");
    let unresolved = callees.iter().find(|n| n.kind == "unresolved").unwrap();
    assert!(
        unresolved.name_only,
        "unresolved placeholder callee must be name_only"
    );
    assert_eq!(unresolved.name, "ghost_fn");
}

#[test]
fn repo_defs_upsert_lookup_and_per_file_replace() {
    let conn = mem();
    upsert_repo_defs(
        &conn,
        "proj",
        "a.rs",
        &[
            ("foo".into(), "function".into(), "rust".into()),
            ("Bar".into(), "type".into(), "rust".into()),
        ],
    )
    .unwrap();
    upsert_repo_defs(
        &conn,
        "proj",
        "b.rs",
        &[("foo".into(), "function".into(), "rust".into())],
    )
    .unwrap();

    let hits = lookup_repo_defs(&conn, "proj", "foo").unwrap();
    assert_eq!(hits.len(), 2, "foo defined in both files: {hits:?}");
    assert!(hits.contains(&("a.rs".to_string(), "function".to_string())));
    assert!(hits.contains(&("b.rs".to_string(), "function".to_string())));

    // Replacing a.rs's defs with a set that drops `foo` must not leave a stale row.
    upsert_repo_defs(
        &conn,
        "proj",
        "a.rs",
        &[("Baz".into(), "type".into(), "rust".into())],
    )
    .unwrap();
    let hits = lookup_repo_defs(&conn, "proj", "foo").unwrap();
    assert_eq!(
        hits,
        vec![("b.rs".to_string(), "function".to_string())],
        "a.rs's foo def must be gone after replace: {hits:?}"
    );
}

// ─── WP2 Stage 2: two-channel attribution ───

fn attr(node_id: &str, channel: &str, source_id: &str, ts: Option<&str>) -> AttributionRow {
    AttributionRow {
        node_id: node_id.into(),
        channel: channel.into(),
        source_id: source_id.into(),
        observed_ts: ts.map(|s| s.into()),
        evidence: "test".into(),
    }
}

#[test]
fn format_attribution_neither_channel_is_unattributed() {
    assert_eq!(format_attribution(&[]), "unattributed");
}

#[test]
fn format_attribution_transcript_only() {
    let rows = vec![attr("n1", "transcript", "70690eeb12345678", None)];
    assert_eq!(format_attribution(&rows), "transcript:70690eeb");
}

#[test]
fn format_attribution_git_only() {
    let rows = vec![attr("n1", "git", "624e7229abcdef", None)];
    assert_eq!(format_attribution(&rows), "git:624e7229");
}

#[test]
fn format_attribution_both_channels_agree_no_disagreement_label() {
    let rows = vec![
        attr(
            "n1",
            "transcript",
            "70690eeb12345678",
            Some("2026-07-27T10:00:00Z"),
        ),
        attr("n1", "git", "624e7229abcdef", Some("2026-07-27T11:00:00Z")),
    ];
    let out = format_attribution(&rows);
    assert_eq!(out, "transcript:70690eeb + git:624e7229");
    assert!(!out.contains("disagreement"));
}

#[test]
fn format_attribution_both_channels_over_48h_apart_is_disagreement() {
    // R8 example: git `624e7229` vs transcript 2026-07-27, labeled
    // disagreement per the plan's acceptance spot-check.
    let rows = vec![
        attr(
            "n1",
            "transcript",
            "70690eeb12345678",
            Some("2026-07-27T10:00:00Z"),
        ),
        attr("n1", "git", "624e7229abcdef", Some("2026-08-01T10:00:00Z")),
    ];
    let out = format_attribution(&rows);
    assert!(out.contains("transcript:70690eeb"), "got: {out}");
    assert!(out.contains("git:624e7229"), "got: {out}");
    assert!(out.contains("disagreement"), "got: {out}");
    // Never merge into a single collapsed value — both must be visible.
    assert!(out.contains('+') || out.contains("vs"), "got: {out}");
}

#[test]
fn format_attribution_both_channels_unparseable_timestamps_no_false_disagreement() {
    // Missing/garbage timestamps must never be silently treated as a
    // >48h gap — we can't prove disagreement we can't measure.
    let rows = vec![
        attr("n1", "transcript", "abc12345", None),
        attr("n1", "git", "def67890", Some("not-a-timestamp")),
    ];
    let out = format_attribution(&rows);
    assert_eq!(out, "transcript:abc12345 + git:def67890");
}

#[test]
fn upsert_and_get_attribution_round_trips_and_is_idempotent() {
    let conn = mem();
    upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
    let row = attr("n1", "transcript", "sess-1", Some("2026-07-27T10:00:00Z"));
    upsert_attribution(&conn, &row).unwrap();
    // Re-run with a different source_id for the same (node_id, channel):
    // must replace, never duplicate (PK is node_id+channel).
    let row2 = attr("n1", "transcript", "sess-2", Some("2026-07-28T10:00:00Z"));
    upsert_attribution(&conn, &row2).unwrap();

    let got = get_attribution(&conn, "n1").unwrap();
    assert_eq!(got.len(), 1, "one row per channel: {got:?}");
    assert_eq!(got[0].source_id, "sess-2");
}

#[test]
fn code_node_attribution_check_constraint_rejects_bad_channel() {
    let conn = mem();
    let bad = conn.execute(
        "INSERT INTO code_node_attribution (node_id, channel, source_id) VALUES ('n1', 'guess', 's1')",
        [],
    );
    assert!(
        bad.is_err(),
        "CHECK(channel IN ('transcript','git')) must reject 'guess'"
    );
}

#[test]
fn attribution_for_node_unattributed_when_neither_channel_present() {
    let conn = mem();
    upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
    assert_eq!(attribution_for_node(&conn, "n1").unwrap(), "unattributed");
}

/// D6 safety guard: an extraction that yielded nothing must retire nothing.
/// A file saved mid-edit parses to zero nodes; retiring against that empty
/// set would hard-delete the file's whole attribution provenance.
#[test]
fn retire_missing_nodes_empty_extraction_retires_nothing() {
    use crate::extraction::ast_analysis::lang_from_path_str;
    use crate::extraction::codegraph::extract_graph_fragment;

    let conn = mem();
    let lang = lang_from_path_str("a.rs").expect("rust");
    let source = "fn alpha() {\n    let x = 1;\n    let _ = x;\n}\n";
    let fragment = extract_graph_fragment(source, lang, "a.rs", "repo", "proj", "c1", "s1");
    for n in &fragment.nodes {
        upsert_node(&conn, n).unwrap();
    }
    let before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_nodes WHERE project = 'proj' AND file = 'a.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(before > 0, "fixture must seed nodes or the test is vacuous");

    // Simulate a parse failure: zero observed nodes.
    let retired = retire_missing_nodes(&conn, "proj", "a.rs", &[]).unwrap();

    assert_eq!(retired, 0, "an empty extraction must retire nothing");
    let after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_nodes WHERE project = 'proj' AND file = 'a.rs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        after, before,
        "no node may be deleted on an empty extraction"
    );
}

/// D6 regression guard: moving a function within a file must refresh span
/// and leave exactly one `code_nodes` row (id is stable; upsert updates span).
#[test]
fn retire_missing_nodes_span_updates_on_move_no_duplicate() {
    use crate::extraction::ast_analysis::lang_from_path_str;
    use crate::extraction::codegraph::extract_graph_fragment;

    let conn = mem();
    let lang = lang_from_path_str("a.rs").expect("rust");
    let source1 = "fn extractable() {\n    let x = 1;\n    let y = x + 1;\n    let _ = y;\n}\n";
    let fragment1 = extract_graph_fragment(source1, lang, "a.rs", "repo", "proj", "c1", "s1");
    for n in &fragment1.nodes {
        upsert_node(&conn, n).unwrap();
    }
    let ids1: Vec<String> = fragment1.nodes.iter().map(|n| n.id.clone()).collect();
    retire_missing_nodes(&conn, "proj", "a.rs", &ids1).unwrap();

    // Same function, pushed down ~50 blank lines so span_start increases.
    let pad = "\n".repeat(50);
    let source2 = format!(
        "{pad}fn extractable() {{\n    let x = 1;\n    let y = x + 1;\n    let _ = y;\n}}\n"
    );
    let fragment2 = extract_graph_fragment(&source2, lang, "a.rs", "repo", "proj", "c2", "s2");
    for n in &fragment2.nodes {
        upsert_node(&conn, n).unwrap();
    }
    let ids2: Vec<String> = fragment2.nodes.iter().map(|n| n.id.clone()).collect();
    retire_missing_nodes(&conn, "proj", "a.rs", &ids2).unwrap();

    let rows = nodes_by_name(&conn, "extractable", "proj", 10).unwrap();
    let extractable: Vec<_> = rows
        .into_iter()
        .filter(|n| n.file == "a.rs" && n.kind == "function")
        .collect();
    assert_eq!(
        extractable.len(),
        1,
        "exactly one extractable row after move: {:?}",
        extractable
            .iter()
            .map(|n| (n.id.clone(), n.span_start, n.span_end))
            .collect::<Vec<_>>()
    );
    let span_start = extractable[0].span_start;
    assert!(
        span_start > 40,
        "span_start must reflect the padded source 2 position, got {span_start}"
    );
}

/// D6: a deleted function must be retired (FK-safe: rank row deleted first).
/// Uses `Storage::open_memory()` so `PRAGMA foreign_keys=ON` is active.
#[test]
fn retire_missing_nodes_deletes_absent_function_fk_safe() {
    use crate::extraction::ast_analysis::lang_from_path_str;
    use crate::extraction::codegraph::{extract_graph_fragment, node_id};

    let storage = crate::storage::Storage::open_memory().unwrap();
    let lang = lang_from_path_str("a.rs").expect("rust");
    let source1 = "fn alpha() {\n    let a = 1;\n}\n\nfn beta() {\n    let b = 2;\n}\n";
    let fragment1 = extract_graph_fragment(source1, lang, "a.rs", "repo", "proj", "c1", "s1");
    for n in &fragment1.nodes {
        storage.upsert_code_node(n).unwrap();
    }
    let ids1: Vec<String> = fragment1.nodes.iter().map(|n| n.id.clone()).collect();
    storage
        .retire_missing_code_nodes("proj", "a.rs", &ids1)
        .unwrap();

    let beta_id = node_id("repo", "a.rs", "function", "beta");
    let alpha_id = node_id("repo", "a.rs", "function", "alpha");
    assert!(
        storage.get_code_node(&beta_id).unwrap().is_some(),
        "beta present after first extract"
    );
    assert!(
        storage.get_code_node(&alpha_id).unwrap().is_some(),
        "alpha present after first extract"
    );

    // Seed a rank row so FK ON would fail if we delete code_nodes first.
    {
        let conn = storage.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO code_node_rank (node_id, rank, in_degree, out_degree)
                 VALUES (?1, 1.0, 0, 0)",
            rusqlite::params![beta_id],
        )
        .unwrap();
    }

    // Source 2: beta deleted.
    let source2 = "fn alpha() {\n    let a = 1;\n}\n";
    let fragment2 = extract_graph_fragment(source2, lang, "a.rs", "repo", "proj", "c2", "s2");
    for n in &fragment2.nodes {
        storage.upsert_code_node(n).unwrap();
    }
    let ids2: Vec<String> = fragment2.nodes.iter().map(|n| n.id.clone()).collect();
    storage
        .retire_missing_code_nodes("proj", "a.rs", &ids2)
        .unwrap();

    assert!(
        storage.get_code_node(&beta_id).unwrap().is_none(),
        "beta must be retired after deletion from source"
    );
    assert!(
        storage.get_code_node(&alpha_id).unwrap().is_some(),
        "alpha must survive retirement of beta"
    );
    // Rank row for beta must also be gone (hygiene + FK ordering).
    assert!(
        storage.code_get_node_rank(&beta_id).unwrap().is_none(),
        "code_node_rank for beta must be deleted with the node"
    );
}

#[test]
fn an_extraction_queues_the_file_and_only_the_generation_it_saw_settles() {
    let conn = Connection::open_in_memory().unwrap();
    crate::storage::migrations::run(&conn).unwrap();

    // Rows from before the columns existed are not pending.
    conn.execute(
        "INSERT INTO code_graph_file_state (project, file, content_hash, dirty)
             VALUES ('p', 'old.rs', 'h', 0)",
        [],
    )
    .unwrap();
    assert!(pending_resolve_files(&conn).unwrap().is_empty());

    upsert_file_state_pending(&conn, "p", "a.rs", "h1").unwrap();
    assert_eq!(
        pending_resolve_files(&conn).unwrap(),
        [("p".to_string(), "a.rs".to_string(), 1)]
    );

    // The file is extracted again while a pass that saw generation 1 runs.
    upsert_file_state_pending(&conn, "p", "a.rs", "h2").unwrap();
    assert!(!settle_resolved_gen(&conn, "p", "a.rs", 1).unwrap());
    assert_eq!(pending_resolve_files(&conn).unwrap()[0].2, 2);

    // A pass that saw the current generation settles it, once.
    assert!(settle_resolved_gen(&conn, "p", "a.rs", 2).unwrap());
    assert!(pending_resolve_files(&conn).unwrap().is_empty());
    assert!(!settle_resolved_gen(&conn, "p", "a.rs", 2).unwrap());

    // Marking clean for re-extraction is a different flag and queues nothing.
    upsert_file_state(&conn, "p", "a.rs", "h3", false).unwrap();
    assert!(pending_resolve_files(&conn).unwrap().is_empty());
}

/// H2: the counter a drain's plan is validated against must move for EVERY
/// write to a table the resolve reads, whichever code path makes it. These
/// are raw statements on purpose: a writer that goes around the helpers
/// (backfill, co-edit replay, repo scan, a migration) is the case that
/// must not slip past.
#[test]
fn every_write_a_resolve_reads_moves_the_project_counter() {
    let conn = Connection::open_in_memory().unwrap();
    crate::storage::migrations::run(&conn).unwrap();
    let node = "INSERT INTO code_nodes (id, project, file, kind, name) \
                    VALUES ('n1', 'p', 'a.rs', 'function', 'f')";
    let edge = "INSERT INTO code_edges (src_id, dst_id, kind, src_file) \
                    VALUES ('n1', 'name:g', 'calls', 'a.rs')";
    let steps: &[(&str, &str)] = &[
        ("insert node", node),
        ("update node", "UPDATE code_nodes SET span_end = 9 WHERE id = 'n1'"),
        ("insert edge", edge),
        ("update edge evidence", "UPDATE code_edges SET evidence = 'from:x' WHERE src_id = 'n1'"),
        ("update edge dst", "UPDATE code_edges SET dst_id = 'n2', resolved = 1 WHERE src_id = 'n1'"),
        ("delete edge", "DELETE FROM code_edges WHERE src_id = 'n1'"),
        ("insert repo_defs", "INSERT INTO repo_defs (project, file, name, kind) VALUES ('p', 'a.rs', 'f', 'function')"),
        ("delete repo_defs", "DELETE FROM repo_defs WHERE project = 'p'"),
        ("insert local_bindings", "INSERT INTO local_bindings (project, file, scope, name) VALUES ('p', 'a.rs', 's', 'x')"),
        ("insert edge_scope_chains", "INSERT INTO edge_scope_chains (project, file, src_id, dst_id, kind, chain) VALUES ('p', 'a.rs', 'n1', 'name:g', 'calls', 'c')"),
        ("insert code_evolution", "INSERT INTO code_evolution (id, session_id, project_name, file_path) VALUES ('e1', 's', 'p', 'a.rs')"),
        ("code_evolution under another label (it is the file that counts)", "INSERT INTO code_evolution (id, session_id, project_name, file_path) VALUES ('e2', 's', 'q', 'a.rs')"),
        ("delete node", "DELETE FROM code_nodes WHERE id = 'n1'"),
    ];
    for (what, sql) in steps {
        let before = project_gen(&conn, "p").unwrap();
        conn.execute(sql, []).unwrap();
        assert!(
            project_gen(&conn, "p").unwrap() > before,
            "{what}: the counter did not move"
        );
    }
    // The co-edit replay's own entry point, for a file the project has a node in.
    conn.execute(
        "INSERT INTO code_nodes (id, project, file, kind, name) \
             VALUES ('n3', 'p', 'b.rs', 'function', 'h')",
        [],
    )
    .unwrap();
    let before = project_gen(&conn, "p").unwrap();
    assert!(crate::storage::queries::insert_code_evolution_backfill(
        &conn,
        "bf-1",
        "s2",
        "p",
        "b.rs",
        "rust",
        "Edit",
        "2026-01-01T00:00:00Z",
        None,
    )
    .unwrap());
    assert!(project_gen(&conn, "p").unwrap() > before, "co-edit replay");
}

fn gen_of(conn: &Connection, key: &str) -> i64 {
    gen_row(conn, key).unwrap()
}

fn fresh_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::storage::migrations::run(&conn).unwrap();
    conn
}

fn plain_node(conn: &Connection, id: &str, project: &str, file: &str) {
    conn.execute(
        "INSERT INTO code_nodes (id, project, file, kind, name) VALUES (?1, ?2, ?3, 'function', ?1)",
        params![id, project, file],
    )
    .unwrap();
}

fn evolution(conn: &Connection, id: &str, file: &str) {
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path) VALUES (?1, 's', 'label', ?2)",
        params![id, file],
    )
    .unwrap();
}

/// A co-edit row moves the counters of the projects with a node in its file
/// (or one that ends in the same file name) and of the project it is
/// labelled with, and no other: not every project.
#[test]
fn a_coedit_row_moves_only_the_projects_that_own_its_file() {
    let conn = fresh_db();
    plain_node(&conn, "n1", "p", "/r/a.rs");
    plain_node(&conn, "n2", "q", "/r/c.rs");
    let (p0, q0, star0) = (
        project_gen(&conn, "p").unwrap(),
        project_gen(&conn, "q").unwrap(),
        gen_of(&conn, "*"),
    );

    // A file no project has nodes in, labelled with p.
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path) VALUES ('e0', 's', 'nobody', '/r/none.rs')",
        [],
    )
    .unwrap();
    assert_eq!(
        (
            project_gen(&conn, "p").unwrap(),
            project_gen(&conn, "q").unwrap()
        ),
        (p0, q0)
    );

    // p's file, labelled with nobody: p moves, q does not.
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path) VALUES ('e1', 's', 'nobody', '/r/a.rs')",
        [],
    )
    .unwrap();
    assert!(project_gen(&conn, "p").unwrap() > p0);
    assert_eq!(project_gen(&conn, "q").unwrap(), q0);
    assert_eq!(gen_of(&conn, "*"), star0, "no write bumps every project");
    assert_eq!(
        gen_of(&conn, "p"),
        1,
        "the project's own row is for its own writes"
    );

    // Update and delete reach the old and the new file.
    let (p1, q1) = (
        project_gen(&conn, "p").unwrap(),
        project_gen(&conn, "q").unwrap(),
    );
    conn.execute(
        "UPDATE code_evolution SET file_path = '/r/c.rs' WHERE id = 'e1'",
        [],
    )
    .unwrap();
    assert!(project_gen(&conn, "p").unwrap() > p1 && project_gen(&conn, "q").unwrap() > q1);
    let q2 = project_gen(&conn, "q").unwrap();
    conn.execute("DELETE FROM code_evolution WHERE id = 'e1'", [])
        .unwrap();
    assert!(project_gen(&conn, "q").unwrap() > q2);
}

/// `/tmp` is a symlink into `/private`: the resolver may spell the node's file
/// either way against the row's.
#[test]
fn a_coedit_row_matches_a_node_file_under_either_private_spelling() {
    let conn = fresh_db();
    plain_node(&conn, "n1", "p", "/tmp/x/a.rs");
    plain_node(&conn, "n2", "q", "/private/tmp/x/b.rs");
    let (p0, q0) = (
        project_gen(&conn, "p").unwrap(),
        project_gen(&conn, "q").unwrap(),
    );
    evolution(&conn, "e1", "/private/tmp/x/a.rs");
    evolution(&conn, "e2", "/tmp/x/b.rs");
    assert!(project_gen(&conn, "p").unwrap() > p0);
    assert!(project_gen(&conn, "q").unwrap() > q0);
}

/// An edge with no source node bumps no project's resolve input but the
/// destination's in-degree: never every project.
#[test]
fn a_dangling_edge_moves_no_counter_but_its_destinations() {
    let conn = fresh_db();
    plain_node(&conn, "d1", "p", "a.rs");
    plain_node(&conn, "n2", "q", "c.rs");
    let (p0, q0, star0) = (
        project_gen(&conn, "p").unwrap(),
        project_gen(&conn, "q").unwrap(),
        gen_of(&conn, "*"),
    );
    // Unresolved, to a name: nothing reads it.
    conn.execute(
        "INSERT INTO code_edges (src_id, dst_id, kind, src_file) VALUES ('ghost', 'name:x', 'calls', 'g.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE code_edges SET evidence = 'e' WHERE src_id = 'ghost'",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM code_edges WHERE src_id = 'ghost'", [])
        .unwrap();
    assert_eq!(
        (
            project_gen(&conn, "p").unwrap(),
            project_gen(&conn, "q").unwrap()
        ),
        (p0, q0)
    );
    assert_eq!(gen_of(&conn, "*"), star0);

    // Resolved, to a node of p: p's rank input moved, q's did not.
    conn.execute(
        "INSERT INTO code_edges (src_id, dst_id, kind, src_file, resolved) VALUES ('ghost', 'd1', 'calls', 'g.rs', 1)",
        [],
    )
    .unwrap();
    assert!(project_gen(&conn, "p").unwrap() > p0);
    assert_eq!(project_gen(&conn, "q").unwrap(), q0);
    assert_eq!(gen_of(&conn, "*"), star0);
    let p1 = project_gen(&conn, "p").unwrap();
    conn.execute("DELETE FROM code_edges WHERE src_id = 'ghost'", [])
        .unwrap();
    assert!(project_gen(&conn, "p").unwrap() > p1);
    assert_eq!(gen_of(&conn, "*"), star0);
}

/// Retiring a node takes its edges with it, so none dangle.
#[test]
fn retiring_a_node_leaves_no_dangling_edge() {
    let conn = fresh_db();
    plain_node(&conn, "keep", "p", "a.rs");
    plain_node(&conn, "gone", "p", "a.rs");
    for src in ["keep", "gone"] {
        conn.execute(
            "INSERT INTO code_edges (src_id, dst_id, kind, src_file) VALUES (?1, 'name:x', 'calls', 'a.rs')",
            [src],
        )
        .unwrap();
    }
    let star0 = gen_of(&conn, "*");
    assert_eq!(
        retire_missing_nodes(&conn, "p", "a.rs", &["keep".to_string()]).unwrap(),
        1
    );
    let srcs: Vec<String> = conn
        .prepare("SELECT src_id FROM code_edges")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(srcs, ["keep"]);
    assert_eq!(gen_of(&conn, "*"), star0);
}

/// A database on the first trigger revision (every project bumped by a
/// co-edit row or a dangling edge) gets the new bodies once, and its
/// dangling edges are deleted.
#[test]
fn a_database_on_the_old_triggers_is_upgraded_once() {
    let conn = fresh_db();
    let bump_all = "INSERT INTO code_graph_project_gen (project, gen) VALUES ('*', 1) \
                        ON CONFLICT(project) DO UPDATE SET gen = gen + 1;";
    for (name, event, table) in [
        ("code_evolution_gen_ai", "INSERT", "code_evolution"),
        ("code_edges_gen_ai", "INSERT", "code_edges"),
    ] {
        conn.execute_batch(&format!(
            "DROP TRIGGER {name}; CREATE TRIGGER {name} AFTER {event} ON {table} BEGIN {bump_all} END;"
        ))
        .unwrap();
    }
    conn.execute("DELETE FROM meta WHERE key = 'graph_gen_triggers_rev'", [])
        .unwrap();
    plain_node(&conn, "n1", "p", "a.rs");
    conn.execute(
        "INSERT INTO code_edges (src_id, dst_id, kind, src_file) VALUES ('ghost', 'name:x', 'calls', 'g.rs')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO code_edges (src_id, dst_id, kind, src_file) VALUES ('n1', 'name:x', 'calls', 'a.rs')",
        [],
    )
    .unwrap();
    let star_old = gen_of(&conn, "*");
    assert!(star_old > 0, "the old trigger bumps every project");

    crate::storage::migrations::run(&conn).unwrap();
    let edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(edges, 1, "the dangling edge is gone, the live one stays");
    evolution(&conn, "e1", "/r/unrelated.rs");
    conn.execute(
        "INSERT INTO code_edges (src_id, dst_id, kind, src_file) VALUES ('ghost', 'name:y', 'calls', 'g.rs')",
        [],
    )
    .unwrap();
    assert_eq!(
        gen_of(&conn, "*"),
        star_old,
        "the new triggers bump no every-project row"
    );

    // A second open changes nothing.
    let before = gen_of(&conn, "p");
    crate::storage::migrations::run(&conn).unwrap();
    assert_eq!(gen_of(&conn, "p"), before);
    let edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(edges, 2, "an up-to-date database is not swept again");
}

/// A node stored under a linked worktree's path, a row stored under the
/// canonical main path: the resolver canonicalizes the node's file onto the
/// row's, so the trigger has to reach it by the shared tail.
#[test]
fn a_coedit_row_under_the_main_path_reaches_a_node_stored_under_a_worktree() {
    let conn = fresh_db();
    plain_node(&conn, "n1", "p", "/wt/.claude/worktrees/x/src/deep/b.rs");
    plain_node(&conn, "n2", "q", "/elsewhere/src/other.rs");
    let (p0, q0) = (
        project_gen(&conn, "p").unwrap(),
        project_gen(&conn, "q").unwrap(),
    );
    evolution_for(&conn, "e1", "nobody", "/main/src/deep/b.rs");
    assert!(project_gen(&conn, "p").unwrap() > p0);
    assert_eq!(project_gen(&conn, "q").unwrap(), q0);
    // A file with no slash is its own last component.
    plain_node(&conn, "n3", "q", "lone.rs");
    let q1 = project_gen(&conn, "q").unwrap();
    evolution_for(&conn, "e2", "nobody", "lone.rs");
    assert!(project_gen(&conn, "q").unwrap() > q1);
    // The lookup is by index.
    let plan: String = conn
        .query_row(
            "EXPLAIN QUERY PLAN SELECT 1 FROM code_nodes n WHERE substr(n.file, length(rtrim(n.file, replace(n.file, '/', ''))) + 1) = 'b.rs'",
            [],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("idx_code_nodes_basename"), "{plan}");
}

fn evolution_for(conn: &Connection, id: &str, label: &str, file: &str) {
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path) VALUES (?1, 's', ?2, ?3)",
        params![id, label, file],
    )
    .unwrap();
}

/// The swap pins the heartbeat a build from before the co-edit counters
/// reads, in the same step as the new trigger bodies, and removes the
/// counter rows an earlier revision kept among the projects.
#[test]
fn the_swap_makes_an_old_follower_stand_down() {
    let conn = fresh_db();
    let pin = |c: &Connection| -> Option<String> {
        c.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [crate::extraction::graph_drain::LEGACY_HEARTBEAT_KEY],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
    };
    let stored = pin(&conn);
    // What the old build does with the stored beat: it is a fresh, capable
    // drainer that is not itself.
    assert!(crate::extraction::graph_drain::other_drainer_is_live(
        stored.as_deref(),
        "mcp-1-0",
        now_secs() as u64
    ));

    // An older revision: the beat is an old daemon's, and a counter row sits
    // among the projects.
    conn.execute("DELETE FROM meta WHERE key = 'graph_gen_triggers_rev'", [])
        .unwrap();
    conn.execute(
        "UPDATE meta SET value = '1 1 daemon-1-0' WHERE key = ?1",
        [crate::extraction::graph_drain::LEGACY_HEARTBEAT_KEY],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO code_graph_project_gen (project, gen) VALUES ('*coedit:p', 4)",
        [],
    )
    .unwrap();
    crate::storage::migrations::run(&conn).unwrap();
    assert_eq!(pin(&conn), stored);
    let leftovers: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_graph_project_gen WHERE project LIKE '*coedit:%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0);
}

#[test]
fn file_slice_reads_nodes_and_edges_by_index_only() {
    let conn = mem();
    let (symbols, callers) = file_slice_sql();
    for sql in [symbols, callers] {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = stmt
            .query_map(params!["a.rs", "%a.rs", "proj", "a.rs", 5], |r| r.get(3))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        let plan = plan.join("\n");
        assert!(plan.contains("idx_code_nodes_basename"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
    }
}

#[test]
fn file_slice_keeps_the_ledger_rows_and_order_for_a_named_path() {
    let conn = mem();
    for (id, name) in [("n1", "zeta"), ("n2", "alpha"), ("n3", "mid")] {
        upsert_node(&conn, &node(id, "/repo/src/a.rs", "function", name, "c")).unwrap();
    }
    upsert_node(
        &conn,
        &node("c1", "/repo/src/b.rs", "function", "caller", "c"),
    )
    .unwrap();
    let edge = EdgeRow {
        src_id: "c1".into(),
        dst_id: "n1".into(),
        kind: "calls".into(),
        resolved: 1,
        src_file: "/repo/src/b.rs".into(),
        weight: 1.0,
        ..EdgeRow::default()
    };
    replace_file_edges(&conn, "proj", "/repo/src/b.rs", &[edge]).unwrap();
    let ledger = file_ledger(&conn, "proj", "src/a.rs").unwrap();
    let slice = file_slice(&conn, "proj", "src/a.rs", 2).unwrap();
    let ids = |rows: &[NodeRow]| rows.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&slice.symbols), ids(&ledger.symbols[..2]));
    assert_eq!(slice.callers, ledger.callers);
    assert_eq!(
        slice.callers,
        [("caller".to_string(), "/repo/src/b.rs".to_string())]
    );
}
