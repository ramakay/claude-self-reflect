//! Shift Map endpoint and edge grounding against local SQLite tables.
//!
//! Checks parsed [`ShiftEntry`] endpoints and edges against `code_evolution`,
//! `code_nodes`, `code_edges`, and `witness_verdicts` (above the v2 epoch floor).
//! Fails open on any missing table or SQL error so the story pipeline never breaks.

use std::collections::BTreeSet;

use rusqlite::{params, Connection};
use serde::Serialize;

use super::{sig, EdgeOp, Endpoint, ShiftEntry, Side};
use crate::extraction::ast_analysis::lang_from_path_str;
use crate::storage::witness_verdicts::LIVE_FLOOR_META_KEY;
use crate::storage::Storage;

const MAX_EVO_ROWS: usize = 2000;
const MAX_NODE_ROWS: usize = 50;
const MAX_RECEIPTS: usize = 6;

/// Grounded shift entry serialized into `meta["story_map:<conv_id>"]` for the pane.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroundedShift {
    pub sig: String,
    pub kind: String,
    pub grounded: bool,
    pub moved: bool,
    pub liked: bool,
    pub claim: String,
    pub receipts: Vec<String>,
    pub before: Vec<Part>,
    pub after: Vec<Part>,
}

/// An endpoint card or connector operator on one side of a [`GroundedShift`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Part {
    Endpoint {
        scope: String,
        sym: Option<String>,
        mark: Option<String>,
        tier: &'static str,
    },
    Op {
        op: &'static str,
        tier: &'static str,
    },
}

#[derive(Debug, Clone)]
struct EvoRow {
    id: String,
    file_path: String,
    language: String,
    removed_only: Vec<String>,
}

#[derive(Debug, Clone)]
struct NodeMatch {
    id: String,
    first_conv_id: String,
    last_conv_id: String,
}

#[derive(Debug)]
struct EndpointEval {
    verified: bool,
    contradicted: bool,
    receipts: Vec<String>,
    node_ids: Vec<String>,
}

/// Grounds parsed `@shifts` entries against `code_evolution`, `code_nodes`,
/// `code_edges`, and `witness_verdicts`.
///
/// Preserves input order, omits contradicted entries, and never panics or
/// returns an error. Any query failure treats the affected check as unchecked
/// (model-only, never contradicted).
pub fn ground_shifts(
    storage: &Storage,
    project: &str,
    conv_id: &str,
    entries: &[ShiftEntry],
) -> Vec<GroundedShift> {
    if entries.is_empty() {
        return Vec::new();
    }
    storage
        .with_connection(|conn| Ok(ground_with_conn(conn, project, conv_id, entries)))
        .unwrap_or_else(|_| {
            entries
                .iter()
                .map(|e| ungrounded_fallback(project, e))
                .collect()
        })
}

fn ungrounded_fallback(project: &str, entry: &ShiftEntry) -> GroundedShift {
    GroundedShift {
        sig: sig(project, entry),
        kind: entry.kind.as_str().to_string(),
        grounded: false,
        moved: false,
        liked: false,
        claim: entry.claim.clone(),
        receipts: Vec::new(),
        before: side_to_unverified_parts(&entry.before),
        after: side_to_unverified_parts(&entry.after),
    }
}

fn side_to_unverified_parts(side: &Option<Side>) -> Vec<Part> {
    let Some(s) = side else {
        return Vec::new();
    };
    let mut parts = vec![endpoint_part(&s.from, false)];
    if let Some((op, ref to)) = s.edge {
        parts.push(Part::Op {
            op: op.as_str(),
            tier: "model",
        });
        parts.push(endpoint_part(to, false));
    }
    parts
}

fn endpoint_part(ep: &Endpoint, verified: bool) -> Part {
    Part::Endpoint {
        scope: ep.scope.clone(),
        sym: ep.sym.clone(),
        mark: ep.mark.map(|c| c.to_string()),
        tier: if verified { "verified" } else { "model" },
    }
}

fn ground_with_conn(
    conn: &Connection,
    project: &str,
    conv_id: &str,
    entries: &[ShiftEntry],
) -> Vec<GroundedShift> {
    let evo_rows = load_evo_rows(conn, conv_id);
    let floor = read_witness_floor(conn);

    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let before_eval = eval_side(conn, project, conv_id, floor, &evo_rows, &entry.before);
        let after_eval = eval_side(conn, project, conv_id, floor, &evo_rows, &entry.after);

        if before_eval.contradicted || after_eval.contradicted {
            continue;
        }

        let grounded = if entry.after.is_some() {
            after_eval.any_verified
        } else {
            before_eval.any_verified
        };

        let receipts = capped_receipts(before_eval.receipts.into_iter().chain(after_eval.receipts));

        out.push(GroundedShift {
            sig: sig(project, entry),
            kind: entry.kind.as_str().to_string(),
            grounded,
            moved: false,
            liked: false,
            claim: entry.claim.clone(),
            receipts,
            before: before_eval.parts,
            after: after_eval.parts,
        });
    }
    out
}

/// Deduped, at most `MAX_RECEIPTS`, commit receipts first (they are the strongest), then the
/// edge, node and edit receipts; sorted within each kind so the order is stable.
fn capped_receipts(all: impl Iterator<Item = String>) -> Vec<String> {
    let rank = |r: &str| match r.split(':').next() {
        Some("oid") => 0,
        Some("edge") => 1,
        Some("node") => 2,
        _ => 3,
    };
    let mut set: BTreeSet<(u8, String)> = BTreeSet::new();
    for r in all {
        set.insert((rank(&r), r));
    }
    set.into_iter().map(|(_, r)| r).take(MAX_RECEIPTS).collect()
}

struct SideEval {
    parts: Vec<Part>,
    receipts: Vec<String>,
    any_verified: bool,
    contradicted: bool,
}

fn eval_side(
    conn: &Connection,
    project: &str,
    conv_id: &str,
    floor: i64,
    evo_rows: &[EvoRow],
    side: &Option<Side>,
) -> SideEval {
    let Some(s) = side else {
        return SideEval {
            parts: Vec::new(),
            receipts: Vec::new(),
            any_verified: false,
            contradicted: false,
        };
    };

    let from_ev = eval_endpoint(conn, project, conv_id, floor, evo_rows, &s.from);
    if from_ev.contradicted {
        return SideEval {
            parts: Vec::new(),
            receipts: Vec::new(),
            any_verified: false,
            contradicted: true,
        };
    }

    let mut receipts = from_ev.receipts.clone();
    let mut any_verified = from_ev.verified;
    let mut parts = vec![endpoint_part(&s.from, from_ev.verified)];

    if let Some((op, ref to)) = s.edge {
        let to_ev = eval_endpoint(conn, project, conv_id, floor, evo_rows, to);
        if to_ev.contradicted {
            return SideEval {
                parts: Vec::new(),
                receipts: Vec::new(),
                any_verified: false,
                contradicted: true,
            };
        }
        any_verified = any_verified || to_ev.verified;
        receipts.extend(to_ev.receipts.iter().cloned());

        let edge_rcpts = check_edge(conn, op, &from_ev, &to_ev);
        let op_verified = !edge_rcpts.is_empty();
        receipts.extend(edge_rcpts);

        parts.push(Part::Op {
            op: op.as_str(),
            tier: if op_verified { "verified" } else { "model" },
        });
        parts.push(endpoint_part(to, to_ev.verified));
    }

    SideEval {
        parts,
        receipts,
        any_verified,
        contradicted: false,
    }
}

fn eval_endpoint(
    conn: &Connection,
    project: &str,
    conv_id: &str,
    floor: i64,
    evo_rows: &[EvoRow],
    ep: &Endpoint,
) -> EndpointEval {
    let mut receipts: Vec<String> = Vec::new();
    let mut removed_in_evo = false;

    // 1. code_evolution check (file-level touch alone grounds the endpoint).
    for row in evo_rows {
        if file_matches_scope(&row.file_path, &ep.scope) {
            receipts.push(format_evo_receipt(&row.id));
            if let Some(ref sym) = ep.sym {
                if row.language != "text" && row.removed_only.iter().any(|s| s == sym) {
                    removed_in_evo = true;
                }
            }
        }
    }

    // 2. code_nodes check (only when sym is Some).
    let mut node_ids: Vec<String> = Vec::new();
    let mut node_seen_elsewhere = false;
    let mut node_query_ok = true;

    if let Some(ref sym) = ep.sym {
        match query_matching_nodes(conn, project, sym, &ep.scope) {
            Ok(nodes) => {
                for n in nodes {
                    node_ids.push(n.id.clone());
                    if !conv_id.is_empty()
                        && (n.last_conv_id == conv_id || n.first_conv_id == conv_id)
                    {
                        receipts.push(format_node_receipt(&n.id));
                    } else {
                        node_seen_elsewhere = true;
                    }
                }
            }
            Err(_) => {
                node_query_ok = false;
            }
        }
    }

    // 3. witness_verdicts check (must be bound to this conversation).
    if let Ok(oids) =
        query_witness_verdicts(conn, project, conv_id, floor, &ep.scope, ep.sym.as_deref())
    {
        for oid in oids {
            receipts.push(format_oid_receipt(&oid));
        }
    }

    // Contradiction check B: mark == '-', sym is Some, still live in code_nodes for
    // (project, name=sym, file suffix match), and NOT listed in any *_removed column
    // of this conversation's code_evolution rows for that file.
    if ep.mark == Some('-') && ep.sym.is_some() && !node_ids.is_empty() && !removed_in_evo {
        return EndpointEval {
            verified: false,
            contradicted: true,
            receipts: Vec::new(),
            node_ids,
        };
    }

    let verified = !receipts.is_empty();
    if verified {
        return EndpointEval {
            verified: true,
            contradicted: false,
            receipts,
            node_ids,
        };
    }

    // Contradiction check A: unverified endpoint whose scope looks like a supported
    // source file, with an edit mark ('+' | '~' | '-'), and not seen elsewhere in
    // the project's code_nodes or witness_ledger.
    let is_edit_mark = matches!(ep.mark, Some('+' | '~' | '-'));
    if is_edit_mark && is_supported_source_scope(&ep.scope) {
        let seen = if !node_query_ok {
            // Query failed -> fail open (unchecked, never contradicted).
            true
        } else if node_seen_elsewhere {
            true
        } else {
            seen_elsewhere_in_project(conn, project, &ep.scope, ep.sym.as_deref()).unwrap_or(true)
        };
        if !seen {
            return EndpointEval {
                verified: false,
                contradicted: true,
                receipts: Vec::new(),
                node_ids,
            };
        }
    }

    EndpointEval {
        verified: false,
        contradicted: false,
        receipts: Vec::new(),
        node_ids,
    }
}

/// Checks whether `file` matches `scope` by exact match, `"/" + scope` suffix,
/// or (when `scope` has no `/` and no `.`) basename stem match.
pub(crate) fn file_matches_scope(file: &str, scope: &str) -> bool {
    if file == scope {
        return true;
    }
    if file.len() > scope.len() {
        let split_at = file.len() - scope.len();
        if file.is_char_boundary(split_at)
            && &file[split_at..] == scope
            && file.as_bytes()[split_at - 1] == b'/'
        {
            return true;
        }
    }
    if !scope.contains('/') && !scope.contains('.') {
        let base = file.rsplit('/').next().unwrap_or(file);
        if let Some((stem, _ext)) = base.rsplit_once('.') {
            if !stem.is_empty() && stem == scope {
                return true;
            }
        } else if base == scope {
            return true;
        }
    }
    false
}

/// A scope looks like a source file when it has a `.ext` recognized by
/// `extraction::ast_analysis::lang_from_path_str`.
fn is_supported_source_scope(scope: &str) -> bool {
    let base = scope.rsplit('/').next().unwrap_or(scope);
    base.contains('.') && lang_from_path_str(base).is_some()
}

fn format_evo_receipt(id: &str) -> String {
    let hex_part = id.strip_prefix("evo_").unwrap_or(id);
    let token = hex_part.split('_').next().unwrap_or(hex_part);
    let short: String = token.chars().take(8).collect();
    format!("evo:{short}")
}

fn format_node_receipt(id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    format!("node:{short}")
}

fn format_oid_receipt(oid: &str) -> String {
    let short: String = oid.chars().take(7).collect();
    format!("oid:{short}")
}

fn format_edge_receipt(src_id: &str, dst_id: &str) -> String {
    let s4: String = src_id.chars().take(4).collect();
    let d4: String = dst_id.chars().take(4).collect();
    format!("edge:{s4}_{d4}")
}

fn parse_json_str_list(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn load_evo_rows(conn: &Connection, conv_id: &str) -> Vec<EvoRow> {
    if conv_id.is_empty() {
        return Vec::new();
    }
    let sql = "SELECT id, file_path, language,
                      functions_added, functions_removed,
                      types_added, types_removed,
                      imports_added, imports_removed
               FROM code_evolution
               WHERE session_id = ?1
               ORDER BY timestamp
               LIMIT ?2";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![conv_id, MAX_EVO_ROWS as i64], |r| {
        let id: String = r.get(0)?;
        let file_path: String = r.get(1)?;
        let language: String = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        let fr: String = r.get::<_, Option<String>>(4)?.unwrap_or_default();
        let tr: String = r.get::<_, Option<String>>(6)?.unwrap_or_default();
        let ir: String = r.get::<_, Option<String>>(8)?.unwrap_or_default();

        let mut removed_only = Vec::new();
        if language != "text" {
            for col in [&fr, &tr, &ir] {
                removed_only.extend(parse_json_str_list(col));
            }
        }
        Ok(EvoRow {
            id,
            file_path,
            language,
            removed_only,
        })
    }) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

fn read_witness_floor(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1",
        params![LIVE_FLOOR_META_KEY],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

fn query_matching_nodes(
    conn: &Connection,
    project: &str,
    sym: &str,
    scope: &str,
) -> rusqlite::Result<Vec<NodeMatch>> {
    let mut stmt = conn.prepare(
        "SELECT id, file, first_conv_id, last_conv_id
         FROM code_nodes
         WHERE project = ?1 AND name = ?2
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![project, sym, MAX_NODE_ROWS as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        ))
    })?;
    let mut matches = Vec::new();
    for row in rows {
        let (id, file, first_conv_id, last_conv_id) = row?;
        if file_matches_scope(&file, scope) {
            matches.push(NodeMatch {
                id,
                first_conv_id,
                last_conv_id,
            });
        }
    }
    Ok(matches)
}

fn query_witness_verdicts(
    conn: &Connection,
    project: &str,
    conv_id: &str,
    floor: i64,
    scope: &str,
    sym: Option<&str>,
) -> rusqlite::Result<Vec<String>> {
    if conv_id.is_empty() {
        return Ok(Vec::new());
    }
    let is_stem = !scope.contains('/') && !scope.contains('.');
    let mut stmt = conn.prepare(
        "SELECT l.file, l.symbol, v.receipt_oid
         FROM witness_verdicts v
         JOIN witness_ledger l ON l.id = v.witness_id
         WHERE l.project = ?1
           AND v.id > ?2
           AND v.receipt_oid IS NOT NULL
           AND v.receipt_oid != ''
           AND v.id = (SELECT MAX(v2.id) FROM witness_verdicts v2 WHERE v2.witness_id = v.witness_id)
           AND (?3 = 1 OR l.file = ?4 OR substr(l.file, -(length(?4) + 1)) = '/' || ?4)
           AND (?5 IS NULL OR l.symbol = ?5)
           AND EXISTS (
               SELECT 1
               FROM witness_chunk_bindings b
               JOIN chunks c ON c.id = b.chunk_id
               WHERE b.witness_id = l.id
                 AND c.conversation_id = ?6
           )
         LIMIT 50",
    )?;
    let rows = stmt.query_map(
        params![project, floor, is_stem as i64, scope, sym, conv_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        },
    )?;
    let mut oids = Vec::new();
    for row in rows {
        let (file, row_sym, oid) = row?;
        if !file_matches_scope(&file, scope) {
            continue;
        }
        if let Some(s) = sym {
            if row_sym.as_deref() != Some(s) {
                continue;
            }
        }
        oids.push(oid);
    }
    Ok(oids)
}

fn seen_elsewhere_in_project(
    conn: &Connection,
    project: &str,
    scope: &str,
    sym: Option<&str>,
) -> rusqlite::Result<bool> {
    let is_stem = !scope.contains('/') && !scope.contains('.');

    if let Some(s) = sym {
        // Check code_nodes by (project, name) + suffix match.
        let nodes = query_matching_nodes(conn, project, s, scope)?;
        if !nodes.is_empty() {
            return Ok(true);
        }
        // Check witness_ledger by (project, symbol) + suffix match.
        let mut stmt = conn.prepare(
            "SELECT file FROM witness_ledger
             WHERE project = ?1 AND symbol = ?2
               AND (?3 = 1 OR file = ?4 OR substr(file, -(length(?4) + 1)) = '/' || ?4)
             LIMIT 50",
        )?;
        let rows = stmt.query_map(params![project, s, is_stem as i64, scope], |r| {
            r.get::<_, String>(0)
        })?;
        for r in rows {
            if file_matches_scope(&r?, scope) {
                return Ok(true);
            }
        }
        Ok(false)
    } else {
        // File-only check in code_nodes.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT file FROM code_nodes
             WHERE project = ?1
               AND (?2 = 1 OR file = ?3 OR substr(file, -(length(?3) + 1)) = '/' || ?3)
             LIMIT 200",
        )?;
        let rows = stmt.query_map(params![project, is_stem as i64, scope], |r| {
            r.get::<_, String>(0)
        })?;
        for r in rows {
            if file_matches_scope(&r?, scope) {
                return Ok(true);
            }
        }
        // File-only check in witness_ledger.
        let mut wstmt = conn.prepare(
            "SELECT DISTINCT file FROM witness_ledger
             WHERE project = ?1
               AND (?2 = 1 OR file = ?3 OR substr(file, -(length(?3) + 1)) = '/' || ?3)
             LIMIT 200",
        )?;
        let wrows = wstmt.query_map(params![project, is_stem as i64, scope], |r| {
            r.get::<_, String>(0)
        })?;
        for r in wrows {
            if file_matches_scope(&r?, scope) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn check_edge(
    conn: &Connection,
    op: EdgeOp,
    from_ev: &EndpointEval,
    to_ev: &EndpointEval,
) -> Vec<String> {
    if from_ev.node_ids.is_empty() || to_ev.node_ids.is_empty() {
        return Vec::new();
    }
    let Ok(mut stmt) =
        conn.prepare("SELECT 1 FROM code_edges WHERE src_id = ?1 AND dst_id = ?2 LIMIT 1")
    else {
        return Vec::new();
    };

    let mut receipts = Vec::new();
    for a in &from_ev.node_ids {
        for b in &to_ev.node_ids {
            let forward = stmt.query_row(params![a, b], |_| Ok(())).is_ok();
            if forward {
                receipts.push(format_edge_receipt(a, b));
                return receipts;
            }
            if op == EdgeOp::Flows {
                let reverse = stmt.query_row(params![b, a], |_| Ok(())).is_ok();
                if reverse {
                    receipts.push(format_edge_receipt(b, a));
                    return receipts;
                }
            }
        }
    }
    receipts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story_map::parse_shift_line;
    use serde_json::json;

    fn parse1(line: &str) -> ShiftEntry {
        parse_shift_line(line).expect("valid shift line")
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_evo(
        storage: &Storage,
        id: &str,
        session_id: &str,
        file_path: &str,
        language: &str,
        fa: &str,
        fr: &str,
    ) {
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language,
                        functions_added, functions_removed
                    ) VALUES (?1, ?2, 'csr', ?3, ?4, ?5, ?6)",
                    params![id, session_id, file_path, language, fa, fr],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_node(
        storage: &Storage,
        id: &str,
        project: &str,
        file: &str,
        name: &str,
        first_conv: &str,
        last_conv: &str,
    ) {
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_nodes (
                        id, repo, project, file, lang, kind, name, first_conv_id, last_conv_id
                    ) VALUES (?1, 'repo', ?2, ?3, 'rust', 'function', ?4, ?5, ?6)",
                    params![id, project, file, name, first_conv, last_conv],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_edge(storage: &Storage, src_id: &str, dst_id: &str, kind: &str) {
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_edges (src_id, dst_id, kind) VALUES (?1, ?2, ?3)",
                    params![src_id, dst_id, kind],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn symbol_in_code_evolution_grounds_with_evo_receipt() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_9f12a0c1_1760400",
            "conv_1",
            "/repo/csr-engine/src/summarizer.rs",
            "rust",
            r#"["ground_shifts"]"#,
            "[]",
        );
        let entry = parse1("decision|-|summarizer.rs:ground_shifts+|Check shift endpoints");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(out[0].receipts, vec!["evo:9f12a0c1"]);
        assert_eq!(
            out[0].after,
            vec![Part::Endpoint {
                scope: "summarizer.rs".into(),
                sym: Some("ground_shifts".into()),
                mark: Some("+".into()),
                tier: "verified",
            }]
        );
    }

    #[test]
    fn file_level_touch_row_grounds_an_entry() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_abcd1234_1760400",
            "conv_1",
            "/repo/docs/architecture.md",
            "text",
            "[]",
            "[]",
        );
        let entry = parse1("shift|-|docs/architecture.md~|Document architecture shift");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(out[0].receipts, vec!["evo:abcd1234"]);
    }

    #[test]
    fn file_touched_symbol_unlisted_still_grounded_at_file_level() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_11223344_1760400",
            "conv_1",
            "/repo/src/summarizer.rs",
            "rust",
            r#"["other_fn"]"#,
            "[]",
        );
        let entry = parse1("shift|-|summarizer.rs:store_story~|Touch inside store_story");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(out[0].receipts, vec!["evo:11223344"]);
    }

    #[test]
    fn stem_scope_matches_the_file_basename() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_feedcafe_1760400",
            "conv_1",
            "/repo/csr-engine/src/summarizer.rs",
            "rust",
            r#"["store_story"]"#,
            "[]",
        );
        let entry = parse1("shift|-|summarizer:store_story~|Stem matches summarizer.rs");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(out[0].receipts, vec!["evo:feedcafe"]);
    }

    #[test]
    fn node_from_this_conversation_grounds() {
        let storage = Storage::open_memory().unwrap();
        seed_node(
            &storage,
            "44b18e209999aaaa",
            "csr",
            "/repo/src/summarizer.rs",
            "store_story",
            "conv_0",
            "conv_1",
        );
        let entry = parse1("shift|-|summarizer.rs:store_story~|Node updated in conv_1");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(out[0].receipts, vec!["node:44b18e20"]);
    }

    #[test]
    fn node_from_another_conversation_does_not_ground() {
        let storage = Storage::open_memory().unwrap();
        seed_node(
            &storage,
            "44b18e209999aaaa",
            "csr",
            "/repo/src/summarizer.rs",
            "store_story",
            "conv_old",
            "conv_other",
        );
        let entry = parse1("shift|-|summarizer.rs:store_story~|Touched in another conv");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        // Seen in project -> not contradicted, but not grounded by conv_1.
        assert_eq!(out.len(), 1);
        assert!(!out[0].grounded);
        assert!(out[0].receipts.is_empty());
    }

    #[test]
    fn verdict_bound_to_this_conversation_grounds_with_oid_receipt() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('witness_verdicts_v2_floor', '5')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('chk_1', 'conv_1', 'csr', '2026-10-14T00:00:00Z', 'hello', 1)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO witness_ledger (id, project, file, symbol, stamp, tier, source_kind)
                     VALUES (10, 'csr', '/repo/src/pane.py', 'session_story', 'st1', 'committed', 'backfill')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO witness_chunk_bindings (witness_id, chunk_id) VALUES (10, 'chk_1')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO witness_verdicts (id, witness_id, verdict, receipt_oid, observed_head_oid)
                     VALUES (6, 10, 'superseded_by', '3f2a1b0998877', 'head123')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let entry = parse1("shift|-|pane.py:session_story~|Witness verdict grounds endpoint");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(out[0].receipts, vec!["oid:3f2a1b0"]);
    }

    #[test]
    fn verdict_below_the_floor_or_without_receipt_is_ignored() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES ('witness_verdicts_v2_floor', '10')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('chk_1', 'conv_1', 'csr', '2026-10-14T00:00:00Z', 'hello', 1)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO witness_ledger (id, project, file, symbol, stamp, tier, source_kind)
                     VALUES (1, 'csr', '/repo/src/pane.py', 'session_story', 'st1', 'committed', 'backfill'),
                            (2, 'csr', '/repo/src/pane.py', 'session_story', 'st2', 'committed', 'backfill')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO witness_chunk_bindings (witness_id, chunk_id) VALUES (1, 'chk_1'), (2, 'chk_1')",
                    [],
                )?;
                // Verdict 8 is <= floor (10); verdict 11 has empty receipt_oid.
                c.execute(
                    "INSERT INTO witness_verdicts (id, witness_id, verdict, receipt_oid, observed_head_oid)
                     VALUES (8, 1, 'superseded_by', '3f2a1b0998877', 'head1'),
                            (11, 2, 'superseded_by', '', 'head2')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let entry = parse1("shift|-|pane.py:session_story~|Floor filters old verdicts");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        // Witness ledger row exists in project -> not contradicted, but ungrounded.
        assert_eq!(out.len(), 1);
        assert!(!out[0].grounded);
        assert!(out[0].receipts.is_empty());
    }

    #[test]
    fn edge_in_code_edges_gives_edge_receipt_and_verified_op() {
        let storage = Storage::open_memory().unwrap();
        seed_node(
            &storage,
            "aaaa111122223333",
            "csr",
            "/repo/src/summarizer.rs",
            "store_story",
            "conv_1",
            "conv_1",
        );
        seed_node(
            &storage,
            "bbbb444455556666",
            "csr",
            "/repo/src/story_map.rs",
            "ground_shifts",
            "conv_1",
            "conv_1",
        );
        // Reverse edge in DB, tested with `~>` which accepts reverse direction too.
        seed_edge(&storage, "bbbb444455556666", "aaaa111122223333", "calls");

        let entry = parse1(
            "shift|-|summarizer.rs:store_story~ ~> story_map.rs:ground_shifts+|Flow edge verified",
        );
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert!(out[0].receipts.contains(&"edge:bbbb_aaaa".to_string()));
        assert_eq!(
            out[0].after[1],
            Part::Op {
                op: "~>",
                tier: "verified"
            }
        );
    }

    #[test]
    fn co_edited_endpoints_keep_the_op_model_tier() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_1111aaaa_1",
            "conv_1",
            "/repo/src/a.rs",
            "rust",
            r#"["fa"]"#,
            "[]",
        );
        seed_evo(
            &storage,
            "evo_2222bbbb_2",
            "conv_1",
            "/repo/src/b.rs",
            "rust",
            r#"["fb"]"#,
            "[]",
        );
        let entry = parse1("shift|-|a.rs:fa~ -> b.rs:fb~|Co-edited without code_edges row");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(
            out[0].after[1],
            Part::Op {
                op: "->",
                tier: "model"
            }
        );
        assert!(!out[0].receipts.iter().any(|r| r.starts_with("edge:")));
    }

    #[test]
    fn unknown_source_file_with_edit_mark_is_dropped() {
        let storage = Storage::open_memory().unwrap();
        let entry = parse1("shift|-|nonexistent.rs:phantom+|Hallucinated source edit");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert!(out.is_empty());
    }

    #[test]
    fn source_file_seen_in_the_project_is_not_contradicted() {
        let storage = Storage::open_memory().unwrap();
        seed_node(
            &storage,
            "9999888877776666",
            "csr",
            "/repo/src/existing.rs",
            "other_fn",
            "conv_old",
            "conv_old",
        );
        // File-only edit claim on existing.rs (no symbol, so seen_elsewhere checks file).
        let entry = parse1("shift|-|existing.rs~|Untouched in conv_1 but exists in project");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(!out[0].grounded);
    }

    #[test]
    fn concept_endpoint_is_model_only_and_kept() {
        let storage = Storage::open_memory().unwrap();
        let entry =
            parse1("risk|pane.py:session_story -> reflections|meta:story_map+|Concept target");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(!out[0].grounded);
        assert!(out[0].receipts.is_empty());
        assert_eq!(
            out[0].after,
            vec![Part::Endpoint {
                scope: "meta".into(),
                sym: Some("story_map".into()),
                mark: Some("+".into()),
                tier: "model",
            }]
        );
    }

    #[test]
    fn removed_symbol_still_live_is_dropped() {
        let storage = Storage::open_memory().unwrap();
        // Touch the file in conv_1, but do NOT remove `live_fn`, and keep `live_fn` in code_nodes.
        seed_evo(
            &storage,
            "evo_aaaa0000_1",
            "conv_1",
            "/repo/src/lib.rs",
            "rust",
            r#"["other_fn"]"#,
            "[]",
        );
        seed_node(
            &storage,
            "1234567890abcdef",
            "csr",
            "/repo/src/lib.rs",
            "live_fn",
            "conv_1",
            "conv_1",
        );
        let entry = parse1("shift|lib.rs:live_fn-|lib.rs:other_fn+|False removal claim");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert!(out.is_empty());
    }

    #[test]
    fn removed_symbol_listed_in_removed_column_is_kept() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_aaaa0000_1",
            "conv_1",
            "/repo/src/lib.rs",
            "rust",
            "[]",
            r#"["old_fn"]"#,
        );
        // Even if code_nodes still has a stale row, functions_removed confirms the removal.
        seed_node(
            &storage,
            "1234567890abcdef",
            "csr",
            "/repo/src/lib.rs",
            "old_fn",
            "conv_0",
            "conv_0",
        );
        let entry = parse1("shift|lib.rs:old_fn-|-|Honest removal confirmed by evo");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert_eq!(out[0].receipts, vec!["evo:aaaa0000"]);
    }

    #[test]
    fn other_conversation_rows_do_not_ground() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_deadbeef_1",
            "conv_other",
            "/repo/src/lib.rs",
            "rust",
            r#"["my_fn"]"#,
            "[]",
        );
        seed_node(
            &storage,
            "deadbeef00001111",
            "csr",
            "/repo/src/lib.rs",
            "my_fn",
            "conv_other",
            "conv_other",
        );
        let entry = parse1("shift|-|lib.rs:my_fn+|From another conversation");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(!out[0].grounded);
        assert!(out[0].receipts.is_empty());
    }

    #[test]
    fn empty_after_side_grounds_on_the_before_side() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_778899aa_1",
            "conv_1",
            "/repo/src/old.rs",
            "rust",
            "[]",
            r#"["retire"]"#,
        );
        let entry = parse1("shift|old.rs:retire-|-|Pure deletion grounds on before");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(out[0].grounded);
        assert!(out[0].after.is_empty());
        assert_eq!(out[0].receipts, vec!["evo:778899aa"]);
    }

    #[test]
    fn missing_tables_fail_open_to_model_only() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|c| {
                c.execute_batch("DROP TABLE code_edges; DROP TABLE code_nodes;")?;
                Ok(())
            })
            .unwrap();
        let entry = parse1("shift|-|missing.rs:some_fn+|Tables dropped must fail open");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert!(!out[0].grounded);
    }

    #[test]
    fn json_shape_matches_the_pane_contract() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_9f12a0c1_1760400",
            "conv_1",
            "/repo/src/summarizer.rs",
            "rust",
            r#"["store_story"]"#,
            "[]",
        );
        let entry = parse1(
            "shift|summarizer.rs:store_story~ -> reflections-|summarizer.rs:store_story~ -> reflections:narrative_shift+|Keep prior shift reflections on story rewrite",
        );
        let expected_sig = sig("csr", &entry);
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        let val = serde_json::to_value(&out[0]).unwrap();
        assert_eq!(
            val,
            json!({
                "sig": expected_sig,
                "kind": "shift",
                "grounded": true,
                "moved": false,
                "liked": false,
                "claim": "Keep prior shift reflections on story rewrite",
                "receipts": ["evo:9f12a0c1"],
                "before": [
                    {"scope": "summarizer.rs", "sym": "store_story", "mark": "~", "tier": "verified"},
                    {"op": "->", "tier": "model"},
                    {"scope": "reflections", "sym": null, "mark": "-", "tier": "model"}
                ],
                "after": [
                    {"scope": "summarizer.rs", "sym": "store_story", "mark": "~", "tier": "verified"},
                    {"op": "->", "tier": "model"},
                    {"scope": "reflections", "sym": "narrative_shift", "mark": "+", "tier": "model"}
                ]
            })
        );
    }

    #[test]
    fn receipts_rank_commit_first_then_sorted_deduped_and_capped_at_six() {
        let storage = Storage::open_memory().unwrap();
        for (i, hex) in [
            "ecececec", "aaaaaaaa", "bbbbbbbb", "cccccccc", "dddddddd", "eeeeeeee", "ffffffff",
        ]
        .iter()
        .enumerate()
        {
            seed_evo(
                &storage,
                &format!("evo_{hex}_{i}"),
                "conv_1",
                "/repo/src/summarizer.rs",
                "rust",
                r#"["store_story"]"#,
                "[]",
            );
        }
        // Duplicate receipt id prefix via node as well.
        seed_node(
            &storage,
            "1111222233334444",
            "csr",
            "/repo/src/summarizer.rs",
            "store_story",
            "conv_1",
            "conv_1",
        );
        let entry = parse1("shift|-|summarizer.rs:store_story~|Many receipts");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].receipts,
            vec![
                "node:11112222",
                "evo:aaaaaaaa",
                "evo:bbbbbbbb",
                "evo:cccccccc",
                "evo:dddddddd",
                "evo:ecececec",
            ]
        );
    }

    #[test]
    fn like_wildcards_in_scope_do_not_match_other_files() {
        let storage = Storage::open_memory().unwrap();
        seed_evo(
            &storage,
            "evo_12345678_1",
            "conv_1",
            "/x/aXb.rs",
            "rust",
            r#"["run"]"#,
            "[]",
        );
        seed_node(
            &storage,
            "abcdef0123456789",
            "csr",
            "/x/aXb.rs",
            "run",
            "conv_1",
            "conv_1",
        );
        // Scope `a_b.rs` has an underscore; must NOT match `/x/aXb.rs` and must be dropped as contradicted.
        let entry = parse1("shift|-|a_b.rs:run+|Underscore is literal not wildcard");
        let out = ground_shifts(&storage, "csr", "conv_1", &[entry]);
        assert!(out.is_empty());
    }
}
