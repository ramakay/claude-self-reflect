//! S8 assertions: one table that says which claims later evidence retired.
//!
//! Four feeds write it, none of them with a model:
//!
//! * `user_decision`: a frozen seed file of owner decisions. One `claim` row each.
//! * `csr_resolve`: the latest `resolution_ledger` row per chunk.
//! * `witness`: the latest live `witness_verdicts` event per witness, expanded to
//!   its chunks through `witness_chunk_bindings`.
//! * `task`: every `resolution_proposals` row, state `proposed` (annotate only,
//!   never sinks: proposals stay invisible until a human promotes one).
//!
//! A row with no receipt is never written; it is counted in `skipped_no_receipt`.
//! The table is additive and derived. [`Storage::rebuild_assertions`] deletes and
//! refills every source in one transaction; the csr_resolve write path upserts its
//! own row in the transaction that appends the ledger row. Both readers fail open:
//! a database without the table answers with nothing.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use super::witness_verdicts::LIVE_EVENT_FLOOR_SQL;
use super::Storage;

/// Receipt text cut for ledger evidence and proposals.
const EVIDENCE_RECEIPT_CHARS: usize = 80;
/// Receipt text cut for commit ids.
const OID_RECEIPT_CHARS: usize = 7;
/// Most bound parameters per batched read statement.
const MAX_PARAMS: usize = 500;

/// Additive migration: table and lookup index. Safe to run twice.
pub fn migrate_assertions(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS assertions (
            id INTEGER PRIMARY KEY,
            source TEXT NOT NULL
                CHECK(source IN ('user_decision','csr_resolve','witness','task')),
            target_kind TEXT NOT NULL CHECK(target_kind IN ('chunk','session','claim')),
            target_id TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('superseded','annotate','proposed')),
            claim TEXT,
            pattern_json TEXT,
            receipt TEXT NOT NULL CHECK(length(receipt)>0),
            asserted_at TEXT,
            source_row TEXT,
            UNIQUE(source,target_kind,target_id)
         );
         CREATE INDEX IF NOT EXISTS idx_assertions_target
            ON assertions(target_kind, target_id);",
    )?;
    Ok(())
}

/// One assertion as a reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    pub source: String,
    pub target_kind: String,
    pub target_id: String,
    pub state: String,
    pub claim: Option<String>,
    pub receipt: String,
}

/// One owner decision from the frozen seed file.
#[derive(Debug, Clone, Deserialize)]
pub struct UserDecision {
    pub key: String,
    pub subject_terms: Vec<String>,
    pub superseded_phrases: Vec<String>,
    pub current_phrases: Vec<String>,
    pub current_claim: String,
    pub decided_at: String,
    pub receipt: String,
}

#[derive(Deserialize)]
struct DecisionFile {
    decisions: Vec<UserDecision>,
}

/// Read the `decisions` array of a seed file.
pub fn load_decisions(path: &Path) -> Result<Vec<UserDecision>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read decisions file {}", path.display()))?;
    let file: DecisionFile = serde_json::from_str(&raw)
        .with_context(|| format!("parse decisions file {}", path.display()))?;
    Ok(file.decisions)
}

/// A `claim` row read back: the match rule of one user decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimAssertion {
    pub key: String,
    pub subject_terms: Vec<String>,
    pub superseded_phrases: Vec<String>,
    pub current_phrases: Vec<String>,
    pub receipt: String,
}

impl ClaimAssertion {
    /// The frozen rule: every subject term, at least one superseded phrase, no
    /// current phrase. Case-insensitive, no date bound. Empty phrases are ignored.
    pub fn matches(&self, text: &str) -> bool {
        let hay = text.to_lowercase();
        let has = |p: &String| {
            let p = p.trim().to_lowercase();
            !p.is_empty() && hay.contains(&p)
        };
        let blank = |p: &String| p.trim().is_empty();
        self.subject_terms.iter().filter(|t| !blank(t)).all(has)
            && self.superseded_phrases.iter().any(has)
            && !self.current_phrases.iter().any(has)
    }
}

/// What one rebuild wrote.
#[derive(Debug, Clone, Serialize, Default)]
pub struct AssertionBuildReport {
    /// Keyed `<source>/<state>`.
    pub by_source_state: BTreeMap<String, i64>,
    pub skipped_no_receipt: i64,
    pub elapsed_ms: u64,
}

/// A row to insert, before the receipt check.
struct Row {
    source: &'static str,
    target_kind: &'static str,
    target_id: String,
    state: &'static str,
    claim: Option<String>,
    pattern_json: Option<String>,
    receipt: String,
    asserted_at: Option<String>,
    source_row: Option<String>,
}

fn cut(text: &str, chars: usize) -> String {
    text.trim().chars().take(chars).collect()
}

/// Assertion state for a ledger verdict.
fn resolve_state(status: &str) -> &'static str {
    match status {
        "resolved" => "superseded",
        _ => "annotate",
    }
}

fn table_exists(conn: &Connection) -> bool {
    conn.prepare("SELECT 1 FROM assertions LIMIT 0").is_ok()
}

fn insert_row(tx: &Transaction<'_>, row: &Row, report: &mut AssertionBuildReport) -> Result<()> {
    if row.receipt.trim().is_empty() {
        report.skipped_no_receipt += 1;
        return Ok(());
    }
    tx.execute(
        "INSERT INTO assertions
            (source, target_kind, target_id, state, claim, pattern_json, receipt,
             asserted_at, source_row)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.source,
            row.target_kind,
            row.target_id,
            row.state,
            row.claim,
            row.pattern_json,
            row.receipt,
            row.asserted_at,
            row.source_row
        ],
    )?;
    *report
        .by_source_state
        .entry(format!("{}/{}", row.source, row.state))
        .or_insert(0) += 1;
    Ok(())
}

fn feed_user_decisions(
    tx: &Transaction<'_>,
    decisions: &[UserDecision],
    report: &mut AssertionBuildReport,
) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for d in decisions {
        if !seen.insert(d.key.as_str()) {
            anyhow::bail!("duplicate decision key '{}'", d.key);
        }
        let pattern = serde_json::json!({
            "subject_terms": d.subject_terms,
            "superseded_phrases": d.superseded_phrases,
            "current_phrases": d.current_phrases,
        });
        insert_row(
            tx,
            &Row {
                source: "user_decision",
                target_kind: "claim",
                target_id: d.key.clone(),
                state: "superseded",
                claim: Some(d.current_claim.clone()),
                pattern_json: Some(pattern.to_string()),
                receipt: d.receipt.clone(),
                asserted_at: Some(d.decided_at.clone()),
                source_row: None,
            },
            report,
        )?;
    }
    Ok(())
}

fn feed_csr_resolve(tx: &Transaction<'_>, report: &mut AssertionBuildReport) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT id, chunk_id, status, evidence, claim, source, created_at
         FROM resolution_ledger
         WHERE id IN (SELECT MAX(id) FROM resolution_ledger GROUP BY chunk_id)
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, chunk_id, status, evidence, claim, ledger_source, created_at) in rows {
        insert_row(
            tx,
            &resolve_row(
                id,
                chunk_id,
                &status,
                &evidence,
                claim,
                &ledger_source,
                created_at,
            ),
            report,
        )?;
    }
    Ok(())
}

fn resolve_row(
    id: i64,
    chunk_id: String,
    status: &str,
    evidence: &str,
    claim: Option<String>,
    ledger_source: &str,
    created_at: Option<String>,
) -> Row {
    Row {
        source: "csr_resolve",
        target_kind: "chunk",
        target_id: chunk_id,
        state: resolve_state(status),
        claim,
        pattern_json: Some(
            serde_json::json!({ "status": status, "ledger_source": ledger_source }).to_string(),
        ),
        receipt: cut(evidence, EVIDENCE_RECEIPT_CHARS),
        asserted_at: created_at,
        source_row: Some(format!("resolution_ledger:{id}")),
    }
}

fn feed_witness(tx: &Transaction<'_>, report: &mut AssertionBuildReport) -> Result<()> {
    // Latest live event per witness; a chunk bound to several witnesses takes
    // the event with the highest id.
    let sql = format!(
        "SELECT b.chunk_id, v.id, v.witness_id, v.verdict,
                COALESCE(NULLIF(TRIM(v.receipt_oid), ''), v.observed_head_oid),
                v.created_at
         FROM witness_verdicts v
         JOIN witness_chunk_bindings b ON b.witness_id = v.witness_id
         WHERE v.id IN (
                SELECT MAX(id) FROM witness_verdicts
                WHERE id > {LIVE_EVENT_FLOOR_SQL} GROUP BY witness_id)
           AND v.verdict IN ('anchor_obsolete','superseded_by')
         ORDER BY v.id, b.chunk_id"
    );
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    // Rows arrive in ascending event id: the last event for a chunk wins.
    type Event = (i64, i64, String, Option<String>, Option<String>);
    let mut latest: BTreeMap<String, Event> = BTreeMap::new();
    for (chunk_id, vid, witness_id, verdict, oid, created_at) in rows {
        latest.insert(chunk_id, (vid, witness_id, verdict, oid, created_at));
    }
    for (chunk_id, (vid, witness_id, verdict, oid, created_at)) in latest {
        let state = if verdict == "anchor_obsolete" {
            "superseded"
        } else {
            "annotate"
        };
        insert_row(
            tx,
            &Row {
                source: "witness",
                target_kind: "chunk",
                target_id: chunk_id,
                state,
                claim: None,
                pattern_json: Some(
                    serde_json::json!({ "witness_id": witness_id, "verdict": verdict }).to_string(),
                ),
                receipt: cut(oid.as_deref().unwrap_or(""), OID_RECEIPT_CHARS),
                asserted_at: created_at,
                source_row: Some(format!("witness_verdicts:{vid}")),
            },
            report,
        )?;
    }
    Ok(())
}

fn feed_task(tx: &Transaction<'_>, report: &mut AssertionBuildReport) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT id, chunk_id, claim, evidence, session_id, created_at
         FROM resolution_proposals
         WHERE id IN (SELECT MAX(id) FROM resolution_proposals GROUP BY chunk_id)
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, chunk_id, claim, evidence, session_id, created_at) in rows {
        insert_row(
            tx,
            &Row {
                source: "task",
                target_kind: "chunk",
                target_id: chunk_id,
                state: "proposed",
                claim,
                pattern_json: Some(serde_json::json!({ "session_id": session_id }).to_string()),
                receipt: cut(&evidence, EVIDENCE_RECEIPT_CHARS),
                asserted_at: created_at,
                source_row: Some(format!("resolution_proposals:{id}")),
            },
            report,
        )?;
    }
    Ok(())
}

/// Re-derive the csr_resolve assertion of each chunk from its newest ledger row,
/// inside the caller's transaction. The write path of csr_resolve calls this
/// after it appends ledger rows. No-op when the table is missing. A newest row
/// with no receipt removes the old assertion, as a rebuild would.
pub(crate) fn upsert_resolution_assertions(conn: &Connection, chunk_ids: &[String]) -> Result<()> {
    if chunk_ids.is_empty() || !table_exists(conn) {
        return Ok(());
    }
    for chunk_id in chunk_ids {
        let newest = conn.query_row(
            "SELECT id, status, evidence, claim, source, created_at
             FROM resolution_ledger WHERE chunk_id = ?1 ORDER BY id DESC LIMIT 1",
            [chunk_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        );
        let (id, status, evidence, claim, ledger_source, created_at) = match newest {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => continue,
            Err(e) => return Err(e.into()),
        };
        let row = resolve_row(
            id,
            chunk_id.clone(),
            &status,
            &evidence,
            claim,
            &ledger_source,
            created_at,
        );
        if row.receipt.is_empty() {
            conn.execute(
                "DELETE FROM assertions
                 WHERE source='csr_resolve' AND target_kind='chunk' AND target_id=?1",
                [chunk_id],
            )?;
            continue;
        }
        conn.execute(
            "INSERT INTO assertions
                (source, target_kind, target_id, state, claim, pattern_json, receipt,
                 asserted_at, source_row)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(source, target_kind, target_id) DO UPDATE SET
                state = excluded.state, claim = excluded.claim,
                pattern_json = excluded.pattern_json, receipt = excluded.receipt,
                asserted_at = excluded.asserted_at, source_row = excluded.source_row",
            params![
                row.source,
                row.target_kind,
                row.target_id,
                row.state,
                row.claim,
                row.pattern_json,
                row.receipt,
                row.asserted_at,
                row.source_row
            ],
        )?;
    }
    Ok(())
}

fn state_rank(state: &str) -> u8 {
    match state {
        "superseded" => 0,
        "annotate" => 1,
        _ => 2,
    }
}

fn source_rank(source: &str) -> u8 {
    match source {
        "user_decision" => 0,
        "csr_resolve" => 1,
        "witness" => 2,
        _ => 3,
    }
}

impl Storage {
    /// Delete and refill every source in one transaction.
    pub fn rebuild_assertions(&self, decisions: &[UserDecision]) -> Result<AssertionBuildReport> {
        let started = Instant::now();
        self.with_connection(|conn| {
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM assertions", [])?;
            let mut report = AssertionBuildReport::default();
            feed_user_decisions(&tx, decisions, &mut report)?;
            feed_csr_resolve(&tx, &mut report)?;
            feed_witness(&tx, &mut report)?;
            feed_task(&tx, &mut report)?;
            tx.commit()?;
            report.elapsed_ms = started.elapsed().as_millis() as u64;
            Ok(report)
        })
    }

    /// Assertions on the given chunks and sessions, keyed by target id. When
    /// several sources hit one target the strongest state wins (superseded, then
    /// annotate, then proposed), ties broken by source order. Fails open.
    pub fn assertions_for_targets(
        &self,
        chunk_ids: &[&str],
        session_ids: &[&str],
    ) -> Result<HashMap<String, Assertion>> {
        self.with_connection(|conn| {
            let mut out: HashMap<String, Assertion> = HashMap::new();
            if !table_exists(conn) {
                return Ok(out);
            }
            for (kind, ids) in [("chunk", chunk_ids), ("session", session_ids)] {
                for batch in ids.chunks(MAX_PARAMS - 1) {
                    let marks = (0..batch.len())
                        .map(|i| format!("?{}", i + 2))
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!(
                        "SELECT source, target_kind, target_id, state, claim, receipt
                         FROM assertions
                         WHERE target_kind = ?1 AND target_id IN ({marks})"
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let mut args: Vec<&dyn rusqlite::ToSql> = vec![&kind];
                    args.extend(batch.iter().map(|s| s as &dyn rusqlite::ToSql));
                    let rows = stmt.query_map(args.as_slice(), |r| {
                        Ok(Assertion {
                            source: r.get(0)?,
                            target_kind: r.get(1)?,
                            target_id: r.get(2)?,
                            state: r.get(3)?,
                            claim: r.get(4)?,
                            receipt: r.get(5)?,
                        })
                    })?;
                    for row in rows {
                        let row = row?;
                        let better = match out.get(&row.target_id) {
                            None => true,
                            Some(cur) => {
                                (state_rank(&row.state), source_rank(&row.source))
                                    < (state_rank(&cur.state), source_rank(&cur.source))
                            }
                        };
                        if better {
                            out.insert(row.target_id.clone(), row);
                        }
                    }
                }
            }
            Ok(out)
        })
    }

    /// The match rule of every user decision. Fails open.
    pub fn claim_assertions(&self) -> Result<Vec<ClaimAssertion>> {
        self.with_connection(|conn| {
            if !table_exists(conn) {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare(
                "SELECT target_id, pattern_json, receipt FROM assertions
                 WHERE source = 'user_decision' AND target_kind = 'claim'
                 ORDER BY id",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            #[derive(Deserialize, Default)]
            struct Pattern {
                #[serde(default)]
                subject_terms: Vec<String>,
                #[serde(default)]
                superseded_phrases: Vec<String>,
                #[serde(default)]
                current_phrases: Vec<String>,
            }
            let mut out = Vec::with_capacity(rows.len());
            for (key, pattern, receipt) in rows {
                let p: Pattern = pattern
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                out.push(ClaimAssertion {
                    key,
                    subject_terms: p.subject_terms,
                    superseded_phrases: p.superseded_phrases,
                    current_phrases: p.current_phrases,
                    receipt,
                });
            }
            Ok(out)
        })
    }

    /// Rows for `assertions list`, newest first. Fails open.
    pub fn list_assertions(
        &self,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>> {
        self.with_connection(|conn| {
            if !table_exists(conn) {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare(
                "SELECT id, source, target_kind, target_id, state, claim, receipt,
                        asserted_at, source_row
                 FROM assertions WHERE (?1 IS NULL OR source = ?1)
                 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![source, limit as i64], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "source": r.get::<_, String>(1)?,
                    "target_kind": r.get::<_, String>(2)?,
                    "target_id": r.get::<_, String>(3)?,
                    "state": r.get::<_, String>(4)?,
                    "claim": r.get::<_, Option<String>>(5)?,
                    "receipt": r.get::<_, String>(6)?,
                    "asserted_at": r.get::<_, Option<String>>(7)?,
                    "source_row": r.get::<_, Option<String>>(8)?,
                }))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
}

/// `csr-engine assertions build`: rebuild and return the report as JSON text.
pub fn cli_build(db_path: &Path, decisions: &Path, out: Option<&Path>) -> Result<String> {
    let decisions = load_decisions(decisions)?;
    let storage = Storage::open(db_path)?;
    storage.set_busy_timeout(std::time::Duration::from_secs(30))?;
    let report = storage.rebuild_assertions(&decisions)?;
    let text = serde_json::to_string_pretty(&report)?;
    if let Some(out) = out {
        std::fs::write(out, &text).with_context(|| format!("write {}", out.display()))?;
    }
    Ok(text)
}

/// `csr-engine assertions list`.
pub fn cli_list(db_path: &Path, source: Option<&str>, limit: usize) -> Result<String> {
    let storage = Storage::open(db_path)?;
    let rows = storage.list_assertions(source, limit)?;
    Ok(serde_json::to_string_pretty(&rows)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(s: &Storage, sql: &str) {
        s.with_connection(|c| Ok(c.execute_batch(sql)?)).unwrap();
    }

    fn decision(key: &str) -> UserDecision {
        UserDecision {
            key: key.into(),
            subject_terms: vec!["fork".into(), "PR".into()],
            superseded_phrases: vec!["unmergeable".into()],
            current_phrases: vec!["--admin --squash".into()],
            current_claim: "merge with --admin".into(),
            decided_at: "2026-08-18".into(),
            receipt: "memory file".into(),
        }
    }

    fn rows(s: &Storage, source: &str) -> Vec<(String, String, String)> {
        s.list_assertions(Some(source), 100)
            .unwrap()
            .into_iter()
            .map(|v| {
                (
                    v["target_id"].as_str().unwrap().to_string(),
                    v["state"].as_str().unwrap().to_string(),
                    v["receipt"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn migration_is_idempotent() {
        let s = Storage::open_memory().unwrap();
        s.with_connection(|c| {
            migrate_assertions(c)?;
            migrate_assertions(c)?;
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('assertions','idx_assertions_target')",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn user_decision_feed_writes_claim_rows() {
        let s = Storage::open_memory().unwrap();
        let report = s.rebuild_assertions(&[decision("k1")]).unwrap();
        assert_eq!(report.by_source_state["user_decision/superseded"], 1);
        let claims = s.claim_assertions().unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].key, "k1");
        assert_eq!(claims[0].receipt, "memory file");
        assert_eq!(claims[0].subject_terms, vec!["fork", "PR"]);
        let list = s.list_assertions(Some("user_decision"), 10).unwrap();
        assert_eq!(list[0]["target_kind"], "claim");
        assert_eq!(list[0]["claim"], "merge with --admin");
        assert_eq!(list[0]["asserted_at"], "2026-08-18");
    }

    #[test]
    fn csr_resolve_feed_takes_latest_row_per_chunk() {
        let s = Storage::open_memory().unwrap();
        let long = "x".repeat(120);
        s.insert_resolutions(&["c1".into()], "still_open", "first", None, "agent")
            .unwrap();
        s.insert_resolutions(&["c1".into()], "resolved", &long, Some("done"), "agent")
            .unwrap();
        s.insert_resolutions(&["c2".into()], "regressed", "back again", None, "agent")
            .unwrap();
        s.insert_resolutions(&["c3".into()], "still_open", "open", None, "agent")
            .unwrap();
        // The write path already upserted; the rebuild must agree with it.
        let report = s.rebuild_assertions(&[]).unwrap();
        assert_eq!(report.by_source_state["csr_resolve/superseded"], 1);
        assert_eq!(report.by_source_state["csr_resolve/annotate"], 2);
        let got: HashMap<_, _> = rows(&s, "csr_resolve")
            .into_iter()
            .map(|(id, st, rc)| (id, (st, rc)))
            .collect();
        assert_eq!(got["c1"].0, "superseded");
        assert_eq!(got["c1"].1.chars().count(), 80);
        assert_eq!(got["c2"].0, "annotate");
        assert_eq!(got["c3"].0, "annotate");
        let src: String = s
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT source_row FROM assertions WHERE target_id='c1'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(src, "resolution_ledger:2");
    }

    fn witness_fixture(s: &Storage) {
        exec(
            s,
            "INSERT INTO witness_ledger (id, project, file, stamp, tier, source_kind)
               VALUES (1,'p','a.rs','s1','committed','t'),
                      (2,'p','b.rs','s2','committed','t'),
                      (3,'p','c.rs','s3','committed','t'),
                      (4,'p','d.rs','s4','committed','t');
             INSERT INTO witness_chunk_bindings VALUES (1,'w1'),(2,'w2'),(3,'w3'),(4,'w4');
             -- w1: obsolete, no reinstatement
             INSERT INTO witness_verdicts (id, witness_id, verdict, receipt_oid, observed_head_oid)
               VALUES (101,1,'anchor_obsolete','abcdef1234567','head0000000');
             -- w2: obsolete then reinstated: no row
             INSERT INTO witness_verdicts (id, witness_id, verdict, receipt_oid, observed_head_oid)
               VALUES (102,2,'anchor_obsolete','1111111aaaa','head1111111'),
                      (103,2,'anchor_reinstated',NULL,'head2222222');
             -- w3: superseded_by, no receipt_oid: head oid
             INSERT INTO witness_verdicts (id, witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid)
               VALUES (104,3,'superseded_by',4,NULL,'9999999zzzz');",
        );
    }

    #[test]
    fn witness_feed_maps_state_and_receipt() {
        let s = Storage::open_memory().unwrap();
        witness_fixture(&s);
        let report = s.rebuild_assertions(&[]).unwrap();
        assert_eq!(report.by_source_state["witness/superseded"], 1);
        assert_eq!(report.by_source_state["witness/annotate"], 1);
        let got: HashMap<_, _> = rows(&s, "witness")
            .into_iter()
            .map(|(id, st, rc)| (id, (st, rc)))
            .collect();
        assert_eq!(got.len(), 2, "reinstated witness must write no row");
        assert_eq!(got["w1"], ("superseded".into(), "abcdef1".into()));
        assert_eq!(got["w3"], ("annotate".into(), "9999999".into()));
    }

    #[test]
    fn task_feed_is_proposed_only() {
        let s = Storage::open_memory().unwrap();
        s.insert_resolution_proposal("t1", Some("claim"), "task finished with tests", "sess")
            .unwrap();
        let report = s.rebuild_assertions(&[]).unwrap();
        assert_eq!(report.by_source_state["task/proposed"], 1);
        assert_eq!(
            rows(&s, "task"),
            vec![(
                "t1".to_string(),
                "proposed".to_string(),
                "task finished with tests".to_string()
            )]
        );
        let list = s.list_assertions(Some("task"), 5).unwrap();
        assert_eq!(list[0]["source_row"], "resolution_proposals:1");
    }

    #[test]
    fn row_without_receipt_is_skipped_and_counted() {
        let s = Storage::open_memory().unwrap();
        s.insert_resolution_proposal("t1", None, "   ", "sess")
            .unwrap();
        s.insert_resolution_proposal("t2", None, "real evidence", "sess")
            .unwrap();
        exec(
            &s,
            "INSERT INTO witness_ledger (id, project, file, stamp, tier, source_kind)
               VALUES (1,'p','a.rs','s','committed','t');
             INSERT INTO witness_chunk_bindings VALUES (1,'w1');
             INSERT INTO witness_verdicts (id, witness_id, verdict, receipt_oid, observed_head_oid)
               VALUES (5,1,'anchor_obsolete','','');",
        );
        let mut d = decision("nok");
        d.receipt = String::new();
        let report = s.rebuild_assertions(&[d]).unwrap();
        assert_eq!(report.skipped_no_receipt, 3);
        let total: i64 = report.by_source_state.values().sum();
        assert_eq!(total, 1);
        assert_eq!(rows(&s, "task").len(), 1);
    }

    #[test]
    fn matches_applies_the_frozen_rule() {
        let c = ClaimAssertion {
            key: "k".into(),
            subject_terms: vec!["fork".into(), "PR".into()],
            superseded_phrases: vec!["unmergeable".into(), "cannot merge normally".into()],
            current_phrases: vec!["--admin --squash".into()],
            receipt: "r".into(),
        };
        assert!(c.matches("A FORK pr is UNMERGEABLE here"));
        assert!(c.matches("fork PR: cannot merge normally"));
        // current_phrases exception
        assert!(!c.matches("fork PR unmergeable but gh pr merge --ADMIN --Squash works"));
        // every subject term required
        assert!(!c.matches("fork branch is unmergeable"));
        // at least one superseded phrase required
        assert!(!c.matches("fork PR merges fine"));
    }

    #[test]
    fn missing_table_returns_empty() {
        let s = Storage::open_memory().unwrap();
        exec(&s, "DROP TABLE assertions;");
        assert!(s
            .assertions_for_targets(&["c1"], &["s1"])
            .unwrap()
            .is_empty());
        assert!(s.claim_assertions().unwrap().is_empty());
        // The write path must not fail on an old schema either.
        s.insert_resolutions(&["c1".into()], "resolved", "ev", None, "agent")
            .unwrap();
    }

    #[test]
    fn csr_resolve_insert_upserts_its_assertion_row() {
        let s = Storage::open_memory().unwrap();
        s.insert_resolutions(&["c1".into()], "still_open", "waiting on CI", None, "agent")
            .unwrap();
        let map = s.assertions_for_targets(&["c1"], &[]).unwrap();
        assert_eq!(map["c1"].state, "annotate");
        assert_eq!(map["c1"].source, "csr_resolve");
        assert_eq!(map["c1"].receipt, "waiting on CI");
        s.insert_resolutions_with_confirmation(
            &["c1".into()],
            "resolved",
            "merged in abc123",
            Some("the claim"),
            None,
        )
        .unwrap();
        let map = s.assertions_for_targets(&["c1"], &[]).unwrap();
        assert_eq!(map["c1"].state, "superseded");
        assert_eq!(map["c1"].claim.as_deref(), Some("the claim"));
        let n: i64 = s
            .with_connection(|c| {
                Ok(c.query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(n, 1, "upsert, not a second row");
    }

    #[test]
    fn failed_ledger_insert_leaves_no_assertion() {
        let s = Storage::open_memory().unwrap();
        let r = s.insert_resolutions(&["c1".into()], "bogus", "ev", None, "agent");
        assert!(r.is_err());
        assert!(s.assertions_for_targets(&["c1"], &[]).unwrap().is_empty());
    }

    #[test]
    fn assertions_for_targets_batches_and_prefers_strongest_state() {
        let s = Storage::open_memory().unwrap();
        s.insert_resolution_proposal("c1", None, "proposal", "sess")
            .unwrap();
        s.insert_resolutions(&["c1".into()], "still_open", "open", None, "agent")
            .unwrap();
        s.rebuild_assertions(&[]).unwrap();
        for i in 0..700 {
            exec(
                &s,
                &format!(
                    "INSERT INTO assertions (source,target_kind,target_id,state,receipt)
                     VALUES ('witness','chunk','bulk{i}','annotate','abc1234')"
                ),
            );
        }
        let ids: Vec<String> = (0..700).map(|i| format!("bulk{i}")).collect();
        let mut refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        refs.push("c1");
        let map = s.assertions_for_targets(&refs, &[]).unwrap();
        assert_eq!(map.len(), 701);
        assert_eq!(map["c1"].state, "annotate", "annotate beats proposed");
    }
}
