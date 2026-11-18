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

use anyhow::Result;
use rusqlite::{params, Connection};

/// Outcome of a rank pass.
#[derive(Debug, Clone, PartialEq)]
pub struct RankStats {
    pub nodes_ranked: usize,
}

/// One node's degree rank as `code_node_rank` stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct RankRow {
    pub node_id: String,
    pub rank: f64,
    pub in_degree: i64,
    pub out_degree: i64,
}

/// The rank of every node in `project` (empty = all), sorted by id. Reads only.
pub fn rank_rows(conn: &Connection, project: &str) -> Result<Vec<RankRow>> {
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

    Ok(node_ids
        .into_iter()
        .map(|node_id| {
            let in_degree = *in_degree.get(&node_id).unwrap_or(&0);
            let out_degree = *out_degree.get(&node_id).unwrap_or(&0);
            RankRow {
                rank: in_degree as f64 + 0.5 * out_degree as f64,
                node_id,
                in_degree,
                out_degree,
            }
        })
        .collect())
}

/// `rows` that differ from what `code_node_rank` holds (or that it lacks).
/// Reads only. A drain writes these, so a pass that changed few edges writes
/// few rank rows.
pub fn changed_rank_rows(conn: &Connection, rows: Vec<RankRow>) -> Result<Vec<RankRow>> {
    let mut stored: BTreeMap<String, (f64, i64, i64)> = BTreeMap::new();
    let mut stmt =
        conn.prepare("SELECT node_id, rank, in_degree, out_degree FROM code_node_rank")?;
    let iter = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (
                row.get::<_, f64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ),
        ))
    })?;
    for r in iter {
        let (id, v) = r?;
        stored.insert(id, v);
    }
    Ok(rows
        .into_iter()
        .filter(|r| stored.get(&r.node_id) != Some(&(r.rank, r.in_degree, r.out_degree)))
        .collect())
}

/// Upsert `rows` into `code_node_rank`.
pub fn write_rank_rows(conn: &Connection, rows: &[RankRow]) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO code_node_rank (node_id, rank, in_degree, out_degree, computed_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(node_id) DO UPDATE SET
             rank = excluded.rank,
             in_degree = excluded.in_degree,
             out_degree = excluded.out_degree,
             computed_at = datetime('now')",
    )?;
    for r in rows {
        stmt.execute(params![r.node_id, r.rank, r.in_degree, r.out_degree])?;
    }
    Ok(())
}

/// Recompute and persist degree ranks for every node in `project` (empty = all).
pub fn compute_code_rank(conn: &Connection, project: &str) -> Result<RankStats> {
    let rows = rank_rows(conn, project)?;
    write_rank_rows(conn, &rows)?;
    Ok(RankStats {
        nodes_ranked: rows.len(),
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
