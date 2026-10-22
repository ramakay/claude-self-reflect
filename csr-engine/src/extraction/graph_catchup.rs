//! One file's share of the code graph's resolve and rank passes, and the
//! catch-up for the hook runs that ran out of time before finishing them.
//!
//! The PostToolUse hook used to re-resolve and re-rank the whole project after
//! every edit: 59k pending edges and 24k nodes for this repo, 30 s a run.
//! It now resolves the edited file's edges and ranks its nodes, under a
//! wall-clock budget. What the budget cuts off is left to the daemon: the hook
//! flags the file dirty in `code_graph_file_state` (the per-file state the graph
//! already keeps) and stores what it read before the edit replaced the file's
//! nodes; the code map's loop calls [`run`] on each tick and finishes the file.
//!
//! No project-wide resolve follows. A pending edge elsewhere is in scope of
//! an edit exactly when it names a symbol the file defines or defined (see
//! `resolver::ResolveScope`); every other edge's verdict cannot depend on the
//! edit.

use std::time::Instant;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::resolver::{self, ResolveScope, ScopedResolve};
use crate::search::code_rank;
use crate::storage::{codegraph, queries};

/// What the hook reads about a file before its extraction replaces the old
/// nodes and edges, and the later passes still need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Before {
    /// Names the file defined. A def the edit removed can leave an edge
    /// elsewhere with one candidate; the names now in the file are read later.
    pub names: Vec<String>,
    /// Ids the file's edges pointed at, whose in-degree the edit lowers.
    pub targets: Vec<String>,
}

/// Read the file's pre-edit names and call targets.
pub fn read_before(conn: &Connection, project: &str, file: &str) -> Result<Before> {
    Ok(Before {
        names: codegraph::def_names_in_file(conn, project, file)?,
        targets: codegraph::call_targets_from_file(conn, project, file)?,
    })
}

/// What one file's pass did.
#[derive(Debug, Clone, PartialEq)]
pub struct FileWork {
    pub resolve: ScopedResolve,
    pub ranked: usize,
    /// False when the deadline left some of the resolve or rank undone.
    pub complete: bool,
}

/// Resolve the edges in scope of `file` and rank the nodes its edges touch,
/// stopping at `deadline`.
pub fn resolve_and_rank(
    conn: &Connection,
    project: &str,
    file: &str,
    before: &Before,
    deadline: Option<Instant>,
) -> Result<FileWork> {
    let files = [file.to_string()];
    let resolve = resolver::resolve_edges_scoped(
        conn,
        project,
        &|f: &str| {
            crate::extraction::repo_path::canonical_repo_path(std::path::Path::new(f)).is_file()
        },
        &ResolveScope {
            files: &files,
            removed_names: &before.names,
            deadline,
        },
    )?;
    let mut names = codegraph::def_names_in_file(conn, project, file)?;
    names.extend(before.names.iter().cloned());
    names.sort();
    names.dedup();
    let mut targets = before.targets.clone();
    targets.extend(codegraph::call_targets_from_file(conn, project, file)?);
    let rank =
        code_rank::compute_code_rank_scoped(conn, project, &files, &names, &targets, deadline)?;
    Ok(FileWork {
        complete: resolve.deferred == 0 && rank.complete,
        ranked: rank.stats.nodes_ranked,
        resolve,
    })
}

fn hint_key(project: &str, file: &str) -> String {
    format!("graph_catchup:{project}\u{1f}{file}")
}

/// Leave `file` for the daemon: flag it dirty and keep `before` for [`run`].
pub fn defer(conn: &Connection, project: &str, file: &str, before: &Before) -> Result<()> {
    if let Ok(json) = serde_json::to_string(before) {
        queries::set_meta(conn, &hint_key(project, file), &json)?;
    }
    codegraph::mark_file_dirty(conn, project, file)
}

/// Most files one daemon tick takes.
const MAX_FILES_PER_RUN: usize = 8;

/// Finish the files hooks flagged dirty, without a deadline. Returns how many
/// it cleared. A file whose pass fails is flagged again and tried on the next
/// tick.
pub fn run(conn: &Connection) -> Result<usize> {
    let mut done = 0usize;
    for (project, file) in codegraph::dirty_files(conn, MAX_FILES_PER_RUN)? {
        let key = hint_key(&project, &file);
        let before: Before = queries::get_meta(conn, &key)?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        // Cleared first: a hook that flags the file again while this pass runs
        // leaves the flag set.
        codegraph::mark_file_clean(conn, &project, &file)?;
        match resolve_and_rank(conn, &project, &file, &before, None) {
            Ok(_) => {
                let _ = conn.execute("DELETE FROM meta WHERE key = ?1", [&key]);
                done += 1;
            }
            Err(e) => {
                codegraph::mark_file_dirty(conn, &project, &file)?;
                eprintln!("CSR: graph catch-up for {file} failed (will retry): {e}");
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::codegraph::{self, upsert_node, EdgeRow, NodeRow};
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
            ..NodeRow::default()
        }
    }

    fn call(src: &str, name: &str, file: &str) -> EdgeRow {
        EdgeRow {
            src_id: src.into(),
            dst_id: format!("name:{name}"),
            kind: "calls".into(),
            src_file: file.into(),
            weight: 1.0,
            ..EdgeRow::default()
        }
    }

    fn edge_state(conn: &Connection, src: &str) -> (i64, String) {
        conn.query_row(
            "SELECT resolved, dst_id FROM code_edges WHERE src_id = ?1",
            [src],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn a_deferred_file_is_finished_by_the_daemon_pass() {
        let conn = mem();
        upsert_node(&conn, &def("a_fn", "a.rs", "a_fn")).unwrap();
        upsert_node(&conn, &def("a_new", "a.rs", "fresh")).unwrap();
        upsert_node(&conn, &def("b_fn", "b.rs", "b_fn")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call("a_fn", "fresh", "a.rs")])
            .unwrap();
        codegraph::replace_file_edges(&conn, "proj", "b.rs", &[call("b_fn", "fresh", "b.rs")])
            .unwrap();
        codegraph::upsert_file_state(&conn, "proj", "a.rs", "h", false).unwrap();

        // The hook's budget ran out before it resolved anything.
        let before = Before {
            names: vec!["old_name".into()],
            targets: vec![],
        };
        defer(&conn, "proj", "a.rs", &before).unwrap();
        assert_eq!(
            codegraph::dirty_files(&conn, 10).unwrap(),
            vec![("proj".to_string(), "a.rs".to_string())]
        );
        assert_eq!(edge_state(&conn, "a_fn").0, 0);

        assert_eq!(run(&conn).unwrap(), 1);
        assert!(codegraph::dirty_files(&conn, 10).unwrap().is_empty());
        // Its own edge, and the edge elsewhere that names its new def.
        assert_eq!(edge_state(&conn, "a_fn"), (1, "a_new".to_string()));
        assert_eq!(edge_state(&conn, "b_fn"), (1, "a_new".to_string()));
        assert!(
            queries::get_meta(&conn, &hint_key("proj", "a.rs"))
                .unwrap()
                .is_none(),
            "the hint is dropped once the file is finished"
        );
        assert_eq!(run(&conn).unwrap(), 0, "nothing left to do");
    }

    #[test]
    fn a_pass_with_time_left_completes_and_one_without_does_not() {
        let conn = mem();
        upsert_node(&conn, &def("a_fn", "a.rs", "a_fn")).unwrap();
        upsert_node(&conn, &def("a_b", "a.rs", "callee")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call("a_fn", "callee", "a.rs")])
            .unwrap();
        let before = Before::default();

        let late = resolve_and_rank(&conn, "proj", "a.rs", &before, Some(Instant::now())).unwrap();
        assert!(!late.complete);
        assert_eq!(late.resolve.deferred, 1);
        assert_eq!(edge_state(&conn, "a_fn").0, 0);

        let in_time = resolve_and_rank(
            &conn,
            "proj",
            "a.rs",
            &before,
            Some(Instant::now() + std::time::Duration::from_secs(60)),
        )
        .unwrap();
        assert!(in_time.complete);
        assert_eq!(edge_state(&conn, "a_fn"), (1, "a_b".to_string()));
        let (rank, in_degree, out_degree) =
            codegraph::get_node_rank(&conn, "a_b").unwrap().unwrap();
        assert_eq!((rank, in_degree, out_degree), (1.0, 1, 0));
    }
}
