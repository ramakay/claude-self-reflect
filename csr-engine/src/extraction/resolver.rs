//! Two-pass, name-based edge resolver (v9.4).
//!
//! Pragmatic v1 (Codex #1): no scope graphs. Build a `name -> defs` map, then
//! repoint each `name:<symbol>` placeholder edge to a real def — same-file first,
//! then project-unique. Ambiguity is FIRST-CLASS: ambiguous edges stay
//! `resolved=0` and are never guessed. Deterministic (sorted throughout).
//!
//! Future: swap this for tree-sitter-stack-graphs behind the same signature.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{params, Connection};

/// Resolution outcome surfaced to eval / diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveStats {
    /// Placeholder edges considered.
    pub total: usize,
    /// Placeholder edges repointed to a real def.
    pub resolved: usize,
    /// Placeholder edges left unresolved (ambiguous or no def).
    pub ambiguous: usize,
    pub resolution_rate: f64,
}

/// Resolve all `name:<symbol>` placeholder edges within `project` (empty = all).
pub fn resolve_edges(conn: &Connection, project: &str) -> Result<ResolveStats> {
    // Pass 1: build name -> sorted [(file, id)] for definition nodes.
    let mut by_name: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, file, name FROM code_nodes
             WHERE kind IN ('function', 'type', 'method') AND (?1 = '' OR project = ?1)
             ORDER BY name, file, id",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (id, file, name) = r?;
            by_name.entry(name).or_default().push((file, id));
        }
    }

    // Pass 2: collect placeholder edges (with their src file), deterministically.
    struct Pending {
        src_id: String,
        dst_id: String,
        kind: String,
        src_file: String,
        name: String,
    }
    let mut pending: Vec<Pending> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT e.src_id, e.dst_id, e.kind, n.file
             FROM code_edges e JOIN code_nodes n ON n.id = e.src_id
             WHERE e.resolved = 0 AND e.dst_id LIKE 'name:%'
               AND (?1 = '' OR n.project = ?1)
             ORDER BY e.src_id, e.dst_id, e.kind",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for r in rows {
            let (src_id, dst_id, kind, src_file) = r?;
            let name = dst_id.strip_prefix("name:").unwrap_or(&dst_id).to_string();
            pending.push(Pending {
                src_id,
                dst_id,
                kind,
                src_file,
                name,
            });
        }
    }

    let total = pending.len();
    let mut resolved = 0usize;

    for p in &pending {
        let defs = match by_name.get(&p.name) {
            Some(d) if !d.is_empty() => d,
            _ => continue, // no def with this name -> ambiguous/unresolved
        };

        // Same-file priority: first def (sorted) in the caller's file.
        let same_file = defs.iter().find(|(file, _)| file == &p.src_file);
        let target = if let Some((_, id)) = same_file {
            Some(id.clone())
        } else if defs.len() == 1 {
            Some(defs[0].1.clone())
        } else {
            None // ambiguous: multiple cross-file defs -> do not guess
        };

        if let Some(new_dst) = target {
            // UPDATE OR REPLACE: changing dst_id changes the PK; tolerate collisions.
            conn.execute(
                "UPDATE OR REPLACE code_edges SET dst_id = ?1, resolved = 1
                 WHERE src_id = ?2 AND dst_id = ?3 AND kind = ?4",
                params![new_dst, p.src_id, p.dst_id, p.kind],
            )?;
            resolved += 1;
        }
    }

    let ambiguous = total - resolved;
    let resolution_rate = if total == 0 {
        1.0
    } else {
        resolved as f64 / total as f64
    };

    Ok(ResolveStats {
        total,
        resolved,
        ambiguous,
        resolution_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::codegraph::{upsert_node, EdgeRow, NodeRow};
    use crate::storage::{codegraph, migrations};

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

    #[test]
    fn resolves_same_file_def() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
        upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "bar", "a.rs")])
            .unwrap();

        let stats = resolve_edges(&conn, "proj").unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.resolved, 1);
        assert_eq!(stats.ambiguous, 0);

        let callees = codegraph::query_callees(&conn, "foo", 10).unwrap();
        assert!(callees.iter().any(|n| n.id == "bar"), "foo -> bar resolved");
    }

    #[test]
    fn ambiguous_stays_unresolved() {
        let conn = mem();
        // Two defs named `bar` in different files; caller in a third file.
        upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
            .unwrap();

        let stats = resolve_edges(&conn, "proj").unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.resolved, 0, "ambiguous must not be guessed");
        assert_eq!(stats.ambiguous, 1);

        // Edge remains a placeholder.
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM code_edges WHERE resolved = 0 AND dst_id = 'name:bar'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn project_unique_resolves_cross_file() {
        let conn = mem();
        upsert_node(&conn, &def("bar", "x.rs", "bar")).unwrap();
        upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
        codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
            .unwrap();

        let stats = resolve_edges(&conn, "proj").unwrap();
        assert_eq!(stats.resolved, 1, "unique cross-file name resolves");
    }
}
