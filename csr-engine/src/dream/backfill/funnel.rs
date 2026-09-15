//! Dream backfill pass 2, item 3: the SQL-stage funnel counter both
//! reviewers demanded — "the stage where the count collapses is where the
//! bar lives". These are pure corpus counts, one query per stage, over the
//! REAL schema (`storage::migrations::run`): no git, no LLM, no
//! `episode_index`/`v_*` views that don't exist (see
//! `intent_channel`'s and `subagent_iface`'s module docs for that
//! reconciliation — this module inherits the same real-schema constraint).
//!
//! Wired to `csr-engine dream backfill --funnel` (see `cli.rs`).
//!
//! # Why only two of the six counts are asserted monotone
//!
//! `episodes`/`subagent_episodes` are the SAME population
//! (`episode_index` rows for this family) with an added filter, so
//! `subagent_episodes <= episodes` always holds. `overlap_symbols` is a
//! SQL `INTERSECT` of `funeral_symbols`'s own symbol set with the
//! anchor-symbol set, so `overlap_symbols <= funeral_symbols` always
//! holds. The other stages count fundamentally different things
//! (`negative_verdicts` counts verdict EVENTS, not distinct symbols — a
//! relapsed symbol can die more than once) and are not claimed to form a
//! single end-to-end monotone chain; the printed table is a diagnostic
//! funnel, not a formal one.
use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Connection;

use super::family::{canon_file, Family};

#[derive(Debug, Clone, Copy, Default)]
pub struct SqlStages {
    pub episodes: i64,
    pub subagent_episodes: i64,
    pub funeral_symbols: i64,
    pub overlap_symbols: i64,
    pub negative_verdicts: i64,
    pub reinstated: i64,
}

fn in_clause(n: usize) -> String {
    in_clause_from(1, n)
}

/// Same as [`in_clause`] but starting the placeholder index at `start` —
/// needed when a single query embeds two independent `IN (...)` clauses:
/// rusqlite binds `?N` positionally, so reusing `?1` in both would demand
/// only one bound value where the query text actually needs two sets.
fn in_clause_from(start: usize, n: usize) -> String {
    (0..n)
        .map(|i| format!("?{}", start + i))
        .collect::<Vec<_>>()
        .join(",")
}

fn family_params(fam: &Family) -> Vec<&dyn rusqlite::ToSql> {
    fam.members
        .iter()
        .map(|m| m as &dyn rusqlite::ToSql)
        .collect()
}

/// Does `table` have `column`? Same `pragma_table_info` idiom
/// `storage::migrations::has_column` uses (duplicated locally — that
/// function is private to a different module and this codebase's own
/// convention is small per-module helpers over a shared-utility import;
/// see `family.rs`'s module doc).
fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        rusqlite::params![table, column],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// One family's SQL-stage funnel. `fam.members` (never empty per
/// [`Family`]'s own invariant) drives every `project IN (...)` clause,
/// same pattern `death_time::detect_bulk` already uses.
pub fn sql_stages(conn: &Connection, fam: &Family) -> Result<SqlStages> {
    if fam.members.is_empty() {
        return Ok(SqlStages::default());
    }
    let clause = in_clause(fam.members.len());

    let episodes: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM episode_index WHERE project IN ({clause})"),
        family_params(fam).as_slice(),
        |r| r.get(0),
    )?;

    // `evidence_kind` is an A-a future column (see subagent_iface's module
    // doc) — 0 on every corpus that hasn't run that migration yet, never
    // an error.
    let subagent_episodes: i64 = if table_has_column(conn, "episode_index", "evidence_kind")? {
        conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM episode_index WHERE project IN ({clause}) \
                 AND evidence_kind IN ('subagent_edit','workflow')"
            ),
            family_params(fam).as_slice(),
            |r| r.get(0),
        )?
    } else {
        0
    };

    let funeral_symbols: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT w.symbol) FROM witness_ledger w \
             JOIN witness_verdicts v ON v.witness_id = w.id \
             WHERE w.project IN ({clause}) AND w.symbol IS NOT NULL \
             AND v.verdict IN ('anchor_obsolete','superseded_by')"
        ),
        family_params(fam).as_slice(),
        |r| r.get(0),
    )?;

    let negative_verdicts: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM witness_verdicts v \
             JOIN witness_ledger w ON w.id = v.witness_id \
             WHERE w.project IN ({clause}) \
             AND v.verdict IN ('anchor_obsolete','superseded_by')"
        ),
        family_params(fam).as_slice(),
        |r| r.get(0),
    )?;

    let reinstated: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM witness_verdicts v \
             JOIN witness_ledger w ON w.id = v.witness_id \
             WHERE w.project IN ({clause}) AND v.verdict = 'anchor_reinstated'"
        ),
        family_params(fam).as_slice(),
        |r| r.get(0),
    )?;

    // F6 fix (Codex review pass 1 / main-thread finding): the previous
    // version's `json_extract(a.value, '$.symbol')` queried a field that
    // does not exist in the real anchor JSON shape
    // (`extraction::anchors::FunctionAnchor` serializes the symbol's name
    // under `name`, never `symbol`) — `json_extract` on a missing field
    // returns SQL NULL for every row, which the `IS NOT NULL` filter then
    // discarded unconditionally, so this stage read `0` on every corpus
    // regardless of real overlap (measured: raw exact-file overlap 795,
    // canonicalized ~1115, funnel read 0). Fixed by reading `$.name`, AND
    // by computing the intersection the SAME way the real relapse join
    // does it (`pairs::index_by_symbol` / `FamilyLedgerIndex`): on
    // `(canon_file(file), symbol)` TUPLES, not bare symbol names alone —
    // a symbol name overlap in two UNRELATED files is not evidence a
    // relapse generator could ever act on. Computed in Rust rather than
    // SQL `json_each` so `canon_file` (a Rust function, not a SQLite one)
    // applies identically on both sides.
    let overlap_symbols: i64 = {
        let mut anchor_pairs: HashSet<(String, String)> = HashSet::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT anchors_json FROM episode_index WHERE project IN ({clause})"
            ))?;
            let rows = stmt.query_map(family_params(fam).as_slice(), |r| r.get::<_, String>(0))?;
            for aj in rows {
                let aj = aj?;
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&aj) else {
                    continue;
                };
                let Some(arr) = value.as_array() else {
                    continue;
                };
                for item in arr {
                    let (Some(file), Some(name)) = (
                        item.get("file").and_then(|v| v.as_str()),
                        item.get("name").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    if file.is_empty() || name.is_empty() {
                        continue;
                    }
                    anchor_pairs.insert((canon_file(file), name.to_string()));
                }
            }
        }

        let mut funeral_pairs: HashSet<(String, String)> = HashSet::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT w.file, w.symbol FROM witness_ledger w
                 JOIN witness_verdicts v ON v.witness_id = w.id
                 WHERE w.project IN ({clause}) AND w.symbol IS NOT NULL
                   AND v.verdict IN ('anchor_obsolete','superseded_by')"
            ))?;
            let rows = stmt.query_map(family_params(fam).as_slice(), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (file, symbol) = row?;
                funeral_pairs.insert((canon_file(&file), symbol));
            }
        }

        anchor_pairs.intersection(&funeral_pairs).count() as i64
    };

    Ok(SqlStages {
        episodes,
        subagent_episodes,
        funeral_symbols,
        overlap_symbols,
        negative_verdicts,
        reinstated,
    })
}

/// `csr-engine dream backfill --funnel` render for one family.
pub fn print_sql_stages(family_name: &str, s: &SqlStages) {
    println!("== sql funnel [{family_name}] ==");
    println!("S1  episodes                    {:>6}", s.episodes);
    println!("S2  subagent/workflow episodes  {:>6}", s.subagent_episodes);
    println!("S3  funeral symbols             {:>6}", s.funeral_symbols);
    println!("S4  anchor∩funeral symbols      {:>6}", s.overlap_symbols);
    println!("S5  negative verdicts           {:>6}", s.negative_verdicts);
    println!("S6  reinstated                  {:>6}", s.reinstated);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn seed_episode(conn: &Connection, id: &str, project: &str, anchors_json: &str) {
        conn.execute(
            "INSERT INTO episode_index (episode_id, session_id, project, ts, outcome, anchors_json) \
             VALUES (?1, 'sess', ?2, '2026-01-01T00:00:00Z', 'done', ?3)",
            params![id, project, anchors_json],
        )
        .unwrap();
    }

    fn seed_dead_symbol(conn: &Connection, project: &str, symbol: &str, verdict: &str) {
        conn.execute(
            "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, at_oid, source_kind) \
             VALUES (?1, '/f.rs', ?2, ?3, 'committed', 'oid', 'backfill')",
            params![project, symbol, format!("b3:{symbol}")],
        )
        .unwrap();
        let wid: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE project=?1 AND symbol=?2",
                params![project, symbol],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid) VALUES (?1, ?2, 'head')",
            params![wid, verdict],
        )
        .unwrap();
    }

    // -----------------------------------------------------------------
    // Required test 4: sql_stages returns monotone-nonincreasing counts
    // for the two pairs that are provably bounded by construction.
    // -----------------------------------------------------------------

    #[test]
    fn sql_stages_is_monotone_nonincreasing_where_structurally_guaranteed() {
        let conn = open();
        let fam = Family::single("proj");

        // F6 fix (Codex review pass 1 / main-thread finding): the real
        // anchor JSON shape (`extraction::anchors::FunctionAnchor`) keys
        // the symbol's name under `name`, never `symbol` — this fixture
        // now matches that real shape (the OLD fixture's `"symbol":...`
        // key happened to match the OLD, buggy SQL's own `$.symbol`
        // extraction, which is exactly how this bug hid behind a passing
        // test). `file` must also match `seed_dead_symbol`'s `/f.rs` for
        // the (canon_file, name) tuple join to overlap at all.
        seed_episode(
            &conn,
            "ep-1",
            "proj",
            r#"[{"file":"/f.rs","name":"dead_one"}]"#,
        );
        seed_episode(
            &conn,
            "ep-2",
            "proj",
            r#"[{"file":"/f.rs","name":"never_dead"}]"#,
        );
        seed_dead_symbol(&conn, "proj", "dead_one", "anchor_obsolete");
        seed_dead_symbol(&conn, "proj", "dead_two", "superseded_by");
        seed_dead_symbol(&conn, "proj", "resurrected", "anchor_reinstated");

        let s = sql_stages(&conn, &fam).unwrap();
        assert_eq!(s.episodes, 2);
        assert_eq!(
            s.subagent_episodes, 0,
            "no evidence_kind column on this schema yet"
        );
        assert_eq!(s.funeral_symbols, 2, "dead_one + dead_two, NOT resurrected");
        assert_eq!(
            s.overlap_symbols, 1,
            "only dead_one is anchored by an episode"
        );
        assert_eq!(s.negative_verdicts, 2);
        assert_eq!(s.reinstated, 1);

        // The two structurally-guaranteed subset relations:
        assert!(
            s.subagent_episodes <= s.episodes,
            "subagent_episodes is a filtered subset of episodes"
        );
        assert!(
            s.overlap_symbols <= s.funeral_symbols,
            "overlap_symbols is an INTERSECT, bounded by both operands"
        );
    }

    // -----------------------------------------------------------------
    // F6 (Codex review pass 1 / main-thread finding): S4 must read the
    // real anchor field (`name`) and the real (canon_file, symbol) join
    // granularity, not a nonexistent `$.symbol` field that always read 0.
    // -----------------------------------------------------------------

    #[test]
    fn sql_stages_overlap_reads_real_anchors_not_a_nonexistent_json_field() {
        let conn = open();
        let fam = Family::single("proj");
        for i in 0..5 {
            seed_episode(
                &conn,
                &format!("ep-{i}"),
                "proj",
                &format!(r#"[{{"file":"/f{i}.rs","name":"sym{i}"}}]"#),
            );
            conn.execute(
                "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, at_oid, source_kind) \
                 VALUES ('proj', ?1, ?2, ?3, 'committed', 'oid', 'backfill')",
                params![format!("/f{i}.rs"), format!("sym{i}"), format!("b3:sym{i}")],
            )
            .unwrap();
            let wid: i64 = conn
                .query_row(
                    "SELECT id FROM witness_ledger WHERE project='proj' AND symbol=?1",
                    params![format!("sym{i}")],
                    |r| r.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid) VALUES (?1, 'anchor_obsolete', 'head')",
                params![wid],
            )
            .unwrap();
        }
        let s = sql_stages(&conn, &fam).unwrap();
        assert_eq!(
            s.overlap_symbols, 5,
            "S4 must read the real 5-symbol overlap, not 0 from a nonexistent JSON field"
        );
    }

    #[test]
    fn sql_stages_overlap_matches_via_canon_file_across_a_worktree_path() {
        let conn = open();
        let fam = Family::single("proj");
        // Anchor recorded under a worktree checkout path; ledger recorded
        // under the main-checkout path -- the same real relapse join
        // (`FamilyLedgerIndex`) canonicalizes both sides via `canon_file`
        // before matching, and S4 must do the same.
        seed_episode(
            &conn,
            "ep-1",
            "proj",
            r#"[{"file":"/repo/.claude/worktrees/wt/src/x.rs","name":"foo"}]"#,
        );
        conn.execute(
            "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, at_oid, source_kind) \
             VALUES ('proj', '/repo/src/x.rs', 'foo', 'b3:foo', 'committed', 'oid', 'backfill')",
            [],
        )
        .unwrap();
        let wid: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE project='proj' AND symbol='foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid) VALUES (?1, 'anchor_obsolete', 'head')",
            params![wid],
        )
        .unwrap();
        let s = sql_stages(&conn, &fam).unwrap();
        assert_eq!(
            s.overlap_symbols, 1,
            "a worktree-anchor path and its main-checkout ledger path must canon_file to the same overlap"
        );
    }

    #[test]
    fn sql_stages_empty_family_is_all_zero() {
        let conn = open();
        let fam = Family {
            name: "empty".into(),
            members: vec![],
        };
        let s = sql_stages(&conn, &fam).unwrap();
        assert_eq!(s.episodes, 0);
        assert_eq!(s.funeral_symbols, 0);
    }
}
