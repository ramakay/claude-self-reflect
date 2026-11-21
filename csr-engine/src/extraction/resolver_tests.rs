use super::*;
use crate::storage::codegraph::{self, upsert_node, upsert_repo_defs, EdgeRow, NodeRow};
use crate::storage::migrations;

fn mem() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrations::run(&conn).unwrap();
    conn
}

fn def(id: &str, file: &str, name: &str) -> NodeRow {
    NodeRow {
        id: id.into(),
        repo: "r".into(),
        project: "proj".into(),
        file: file.into(),
        lang: "rust".into(),
        kind: "function".into(),
        name: name.into(),
        first_conv_id: "c".into(),
        last_conv_id: "c".into(),
        ..NodeRow::default()
    }
}

fn module_node(id: &str, file: &str) -> NodeRow {
    NodeRow {
        id: id.into(),
        repo: "r".into(),
        project: "proj".into(),
        file: file.into(),
        lang: "rust".into(),
        kind: "module".into(),
        name: file.into(),
        first_conv_id: "c".into(),
        last_conv_id: "c".into(),
        ..NodeRow::default()
    }
}

fn call_edge(src: &str, name: &str, file: &str) -> EdgeRow {
    EdgeRow {
        src_id: src.into(),
        dst_id: format!("name:{name}"),
        kind: "calls".into(),
        src_file: file.into(),
        resolved: 0,
        weight: 1.0,
        ..EdgeRow::default()
    }
}

fn import_edge(src: &str, name: &str, file: &str) -> EdgeRow {
    EdgeRow {
        src_id: src.into(),
        dst_id: format!("name:{name}"),
        kind: "imports".into(),
        src_file: file.into(),
        resolved: 0,
        weight: 1.0,
        ..EdgeRow::default()
    }
}

/// An `imports` edge carrying `from:<module>` evidence, as extraction
/// (Phase 5 TASK 1) now writes it — feeds the X1 module-aware tier.
fn import_edge_from(src: &str, name: &str, file: &str, module: &str) -> EdgeRow {
    EdgeRow {
        evidence: format!("from:{module}"),
        ..import_edge(src, name, file)
    }
}

/// A `calls` edge carrying `via:<qualifier>` evidence, as extraction
/// (Phase 6 TASK A) now writes it — feeds the X1b qualifier tier.
fn call_edge_via(src: &str, name: &str, file: &str, qualifier: &str) -> EdgeRow {
    EdgeRow {
        evidence: format!("via:{qualifier}"),
        ..call_edge(src, name, file)
    }
}

fn insert_evolution(conn: &Connection, session_id: &str, file_path: &str) {
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, file_path) VALUES (?1, ?2, ?3)",
        params![format!("{session_id}:{file_path}"), session_id, file_path],
    )
    .unwrap();
}

// ─── X4 local-binding witness tier (WCR truth pass, TASK 2) ───

/// `scope` (X4 adversarial review, Finding 1; a full chain as of Finding
/// 4, though every EXISTING test below still passes a bare name — a
/// bare name is a valid degenerate chain, zero `>anon<idx>` segments —
/// see `chain_prefix_matches`'s doc comment) — the nearest enclosing
/// named def's name/chain, or `""` for a module-level binding.
fn insert_local_binding(conn: &Connection, project: &str, file: &str, scope: &str, name: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO local_bindings (project, file, scope, name) VALUES (?1, ?2, ?3, ?4)",
        params![project, file, scope, name],
    )
    .unwrap();
}

/// Second X4 adversarial review, Finding 4: seeds an `edge_scope_chains`
/// row directly — the shape `eval::codegraph::backfill_wcr_witnesses`
/// would have written for a `(src_id, dst_id, kind)` edge whose call/
/// import site sits at `chain` (see `Pending::call_scope_chains`'s doc
/// comment). `project`/`file` are fixed at `"proj"`/the edge's own
/// `src_file` convention used throughout this test module (`def`/
/// `call_edge` above all hardcode `project = "proj"`). `chain` joined
/// `edge_scope_chains`' own PRIMARY KEY as of the Codex round 4
/// adversarial review (Finding 1), so calling this MORE THAN ONCE for
/// the SAME `(src_id, dst_name, kind)` with DIFFERENT `chain` values
/// inserts one row PER chain (never overwrites) — exactly what a
/// multi-occurrence aggregated edge needs.
fn insert_edge_scope_chain(
    conn: &Connection,
    file: &str,
    src_id: &str,
    dst_name: &str,
    kind: &str,
    chain: &str,
) {
    conn.execute(
        "INSERT OR IGNORE INTO edge_scope_chains (project, file, src_id, dst_id, kind, chain)
             VALUES ('proj', ?1, ?2, ?3, ?4, ?5)",
        params![file, src_id, format!("name:{dst_name}"), kind, chain],
    )
    .unwrap();
}

/// A graph that exercises every kind of verdict: same-file and unique-def
/// binds, a co-edit-broken ambiguity, an unbreakable one, an unexplained
/// name, and an edge whose source file is gone.
fn mixed_graph(conn: &Connection) {
    for (id, file, name) in [
        ("foo", "a.rs", "foo"),
        ("bar", "a.rs", "bar"),
        ("baz", "b.rs", "baz"),
        ("dup_x", "x.rs", "dup"),
        ("dup_y", "y.rs", "dup"),
        ("twin_x", "x.rs", "twin"),
        ("twin_y", "y.rs", "twin"),
        ("caller", "z.rs", "caller"),
        ("ghost", "gone.rs", "ghost"),
    ] {
        upsert_node(conn, &def(id, file, name)).unwrap();
    }
    codegraph::replace_file_edges(
        conn,
        "proj",
        "a.rs",
        &[
            call_edge("foo", "bar", "a.rs"),
            call_edge("foo", "baz", "a.rs"),
        ],
    )
    .unwrap();
    codegraph::replace_file_edges(
        conn,
        "proj",
        "z.rs",
        &[
            call_edge("caller", "dup", "z.rs"),
            call_edge("caller", "twin", "z.rs"),
            call_edge("caller", "mystery", "z.rs"),
        ],
    )
    .unwrap();
    codegraph::replace_file_edges(
        conn,
        "proj",
        "gone.rs",
        &[call_edge("ghost", "mystery", "gone.rs")],
    )
    .unwrap();
    // z.rs and x.rs were edited together twice; y.rs never.
    for session in ["s1", "s2"] {
        insert_evolution(conn, session, "z.rs");
        insert_evolution(conn, session, "x.rs");
    }
}

fn edge_dump(conn: &Connection) -> Vec<(String, String, String, i64, String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT src_id, dst_id, kind, resolved, boundary, evidence FROM code_edges
                 ORDER BY src_id, dst_id, kind",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

#[path = "resolver_tests/tier_tests.rs"]
mod tier_tests;

#[path = "resolver_tests/chain_tests.rs"]
mod chain_tests;
