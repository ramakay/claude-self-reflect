//! Degree-based code-node ranking (v9.4).
//!
//! Codex #2 REJECTED PageRank for v1. This computes a deterministic degree score
//! over the `calls` + `references` subgraph (imports excluded to avoid hub
//! domination) and persists it to `code_node_rank`:
//!
//! `rank = in_degree + 0.5 * out_degree`
//!
//! in_degree counts inbound resolved edges; out_degree counts outbound edges.
//! Rank is the injection-priority signal — add real PageRank only if eval proves
//! a top-K gain.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::Result;
use rusqlite::{params, Connection};

/// Outcome of a rank pass.
#[derive(Debug, Clone, PartialEq)]
pub struct RankStats {
    pub nodes_ranked: usize,
}

/// Outcome of a scoped rank pass.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedRank {
    pub stats: RankStats,
    /// False when the deadline ended the pass before every node was reached.
    pub complete: bool,
}

/// Recompute and persist degree ranks for every node in `project` (empty = all).
pub fn compute_code_rank(conn: &Connection, project: &str) -> Result<RankStats> {
    // out_degree: outbound calls/references per src node.
    let mut out_degree: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT e.src_id, COUNT(*) FROM code_edges e
             JOIN code_nodes n ON n.id = e.src_id
             WHERE e.kind IN ('calls', 'references') AND (?1 = '' OR n.project = ?1)
             GROUP BY e.src_id",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for r in rows {
            let (id, c) = r?;
            out_degree.insert(id, c);
        }
    }

    // in_degree: inbound resolved calls/references per dst node.
    let mut in_degree: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT e.dst_id, COUNT(*) FROM code_edges e
             JOIN code_nodes n ON n.id = e.dst_id
             WHERE e.kind IN ('calls', 'references') AND e.resolved = 1
               AND (?1 = '' OR n.project = ?1)
             GROUP BY e.dst_id",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for r in rows {
            let (id, c) = r?;
            in_degree.insert(id, c);
        }
    }

    // Every node in scope gets a (possibly zero) rank — sorted for determinism.
    let node_ids: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT id FROM code_nodes WHERE (?1 = '' OR project = ?1) ORDER BY id")?;
        let rows = stmt.query_map(params![project], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut ranked = 0usize;
    for id in &node_ids {
        let ind = *in_degree.get(id).unwrap_or(&0);
        let outd = *out_degree.get(id).unwrap_or(&0);
        let rank = ind as f64 + 0.5 * outd as f64;
        conn.execute(
            "INSERT INTO code_node_rank (node_id, rank, in_degree, out_degree, computed_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(node_id) DO UPDATE SET
                 rank = excluded.rank,
                 in_degree = excluded.in_degree,
                 out_degree = excluded.out_degree,
                 computed_at = datetime('now')",
            params![id, rank, ind, outd],
        )?;
        ranked += 1;
    }

    Ok(RankStats {
        nodes_ranked: ranked,
    })
}

/// `compute_code_rank` for the nodes of `files`, the nodes named `names`, and
/// the nodes `extra_ids` lists, all in `project`. Each node gets the rank the
/// full pass would give it; nodes outside that set keep the rank they have.
///
/// The PostToolUse hook uses this: a full pass rewrites a row per node of the
/// project (24k for this repo) on every edit, and the edit moves the degree of
/// the edited file's nodes and of the nodes they call or are called as.
///
/// Stops once `deadline` passes, and says so: `complete` is false and the nodes
/// not reached keep their rank.
pub fn compute_code_rank_scoped(
    conn: &Connection,
    project: &str,
    files: &[String],
    names: &[String],
    extra_ids: &[String],
    deadline: Option<Instant>,
) -> Result<ScopedRank> {
    let mut ids: std::collections::BTreeSet<String> = extra_ids.iter().cloned().collect();
    {
        let mut by_file =
            conn.prepare("SELECT id FROM code_nodes WHERE project = ?1 AND file = ?2")?;
        for file in files {
            for id in by_file.query_map(params![project, file], |row| row.get::<_, String>(0))? {
                ids.insert(id?);
            }
        }
        // `+project` keeps the planner on `idx_code_nodes_name`. Left to itself
        // it picks the (project, file) index and walks every node of the
        // project once per name (measured: 300 ms for a 180-name file).
        let mut by_name =
            conn.prepare("SELECT id FROM code_nodes WHERE name = ?2 AND +project = ?1")?;
        for name in names {
            for id in by_name.query_map(params![project, name], |row| row.get::<_, String>(0))? {
                ids.insert(id?);
            }
        }
    }

    let mut in_scope =
        conn.prepare("SELECT 1 FROM code_nodes WHERE id = ?1 AND (?2 = '' OR project = ?2)")?;
    let mut out_degree = conn.prepare(
        "SELECT COUNT(*) FROM code_edges WHERE src_id = ?1 AND kind IN ('calls', 'references')",
    )?;
    let mut in_degree = conn.prepare(
        "SELECT COUNT(*) FROM code_edges
         WHERE dst_id = ?1 AND kind IN ('calls', 'references') AND resolved = 1",
    )?;
    let mut write = conn.prepare(
        "INSERT INTO code_node_rank (node_id, rank, in_degree, out_degree, computed_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(node_id) DO UPDATE SET
             rank = excluded.rank,
             in_degree = excluded.in_degree,
             out_degree = excluded.out_degree,
             computed_at = datetime('now')",
    )?;
    let own_transaction = conn.is_autocommit();
    if own_transaction {
        conn.execute_batch("BEGIN IMMEDIATE")?;
    }
    let ranked = (|| -> Result<(usize, bool)> {
        let mut ranked = 0usize;
        for (seen, id) in ids.iter().enumerate() {
            if seen % 32 == 0 && deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok((ranked, false));
            }
            if !in_scope.exists(params![id, project])? {
                continue;
            }
            let ind: i64 = in_degree.query_row(params![id], |row| row.get(0))?;
            let outd: i64 = out_degree.query_row(params![id], |row| row.get(0))?;
            let rank = ind as f64 + 0.5 * outd as f64;
            write.execute(params![id, rank, ind, outd])?;
            ranked += 1;
        }
        Ok((ranked, true))
    })();
    if own_transaction {
        match &ranked {
            Ok(_) => conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
    }
    let (nodes_ranked, complete) = ranked?;
    Ok(ScopedRank {
        stats: RankStats { nodes_ranked },
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::codegraph::{
        get_node_rank, replace_file_edges, upsert_node, EdgeRow, NodeRow,
    };
    use crate::storage::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn def(id: &str, name: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            project: "proj".into(),
            file: "a.rs".into(),
            kind: "function".into(),
            name: name.into(),
            ..NodeRow::default()
        }
    }

    fn resolved_call(src: &str, dst: &str) -> EdgeRow {
        EdgeRow {
            src_id: src.into(),
            dst_id: dst.into(),
            kind: "calls".into(),
            src_file: "a.rs".into(),
            resolved: 1,
            weight: 1.0,
            ..EdgeRow::default()
        }
    }

    #[test]
    fn degree_rank_is_deterministic_and_correct() {
        let conn = mem();
        upsert_node(&conn, &def("foo", "foo")).unwrap();
        upsert_node(&conn, &def("bar", "bar")).unwrap();
        upsert_node(&conn, &def("baz", "baz")).unwrap();
        // foo -> bar, baz -> bar : bar has in_degree 2.
        replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[resolved_call("foo", "bar"), resolved_call("baz", "bar")],
        )
        .unwrap();

        let s1 = compute_code_rank(&conn, "proj").unwrap();
        assert_eq!(s1.nodes_ranked, 3);
        let bar1 = get_node_rank(&conn, "bar").unwrap().unwrap();
        assert_eq!(bar1.1, 2, "bar in_degree = 2");
        assert_eq!(bar1.2, 0, "bar out_degree = 0");
        assert_eq!(bar1.0, 2.0, "rank = 2 + 0.5*0");

        let foo1 = get_node_rank(&conn, "foo").unwrap().unwrap();
        assert_eq!(foo1.2, 1, "foo out_degree = 1");
        assert_eq!(foo1.0, 0.5, "rank = 0 + 0.5*1");

        // Determinism: a second pass yields identical ranks.
        compute_code_rank(&conn, "proj").unwrap();
        let bar2 = get_node_rank(&conn, "bar").unwrap().unwrap();
        assert_eq!(bar1, bar2);
    }
}
