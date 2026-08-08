//! Read-only, project-scoped evidence feeds for session recaps.

use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use rusqlite::params;

use super::Storage;
use crate::hooks::recap::{RetiredLine, SettledFact};

const LEDGER_FEED_LIMIT: i64 = 5;
const RETIRED_FEED_LIMIT: i64 = 3;

/// `CSR_DREAM_CONSUMPTION=1` opts a user IN to v10 witness-ledger dream
/// verdicts reaching them at all. Default OFF ships the witness ledger as
/// experimental derived data: unless explicitly opted in, no dream verdict
/// may demote/annotate search results (see `mcp::tools`'s validity
/// partition) or populate the recap "Learnt-then-retired while away:"
/// clause below. Pure parsing seam (mirrors `active_forgetting_enabled_from`
/// in `mcp::tools`) — the ONE place this parser is defined; `mcp::tools`
/// imports it from here so the two consumers can never diverge on ON/OFF
/// semantics. Only the exact value `1` enables it.
pub fn dream_consumption_enabled_from(value: Option<&str>) -> bool {
    value == Some("1")
}

/// Reads `CSR_DREAM_CONSUMPTION` from the real process env. Real callers
/// use this; tests drive `dream_consumption_enabled_from` or the `_with`
/// seams below directly by parameter instead, to avoid mutating shared
/// process state under cargo's parallel test runner.
pub fn dream_consumption_enabled() -> bool {
    dream_consumption_enabled_from(std::env::var("CSR_DREAM_CONSUMPTION").ok().as_deref())
}

static RECEIPT_OID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\b(?:receipt(?:_oid)?|commit|oid|sha(?:-?1)?|verified\s+at)(?:\s*[:=#]\s*|\s+)([0-9a-f]{7,40})\b|\b([0-9a-f]{7,40})\b\s+--|\b([0-9a-f]{40})\b|^([0-9a-f]{7,40})$)",
    )
    .expect("valid receipt OID regex")
});

/// Keep receipts compact: prefer a git-style short OID, otherwise a short
/// human-readable preview of the evidence text.
fn shorten_receipt(evidence: &str) -> String {
    RECEIPT_OID_RE
        .captures_iter(evidence)
        .find_map(|captures| {
            if let Some(contextual_oid) = captures.get(1) {
                return Some(contextual_oid.as_str());
            }
            (2..=4)
                .filter_map(|group| captures.get(group))
                .map(|matched| matched.as_str())
                .find(|candidate| candidate.chars().any(|c| c.is_ascii_alphabetic()))
        })
        .map(|oid| oid.chars().take(7).collect())
        .unwrap_or_else(|| evidence.chars().take(24).collect())
}

impl Storage {
    /// Resolution ledger evidence for the previous conversation and current
    /// project. The highest ledger id is the current verdict for a chunk.
    pub fn recap_ledger_feeds(
        &self,
        project: &str,
        conversation_id: &str,
    ) -> Result<(Vec<SettledFact>, Vec<SettledFact>)> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| anyhow::anyhow!("lock: {error}"))?;

        // The caller passes the session file id, which IS the main transcript's
        // conversation_id. Facts authored inside this session's sidechains live
        // on chunks with their own conversation_id but point back to the parent
        // via chunk_provenance.source_conv_id — the second UNION arm credits
        // them through idx_chunk_provenance_source_conv (no scans either way).
        let mut settled_statement = conn.prepare(
            "SELECT claim, evidence, status FROM (
                 SELECT COALESCE(r.claim, '') AS claim, r.evidence AS evidence,
                        r.status AS status, r.id AS rid
                 FROM chunks c INDEXED BY idx_chunks_conversation
                 JOIN resolution_ledger r ON r.chunk_id = c.id
                 WHERE c.conversation_id = ?1
                   AND c.project_name = ?2
                   AND r.status = 'resolved'
                   AND r.id = (
                       SELECT MAX(latest.id)
                       FROM resolution_ledger latest
                       WHERE latest.chunk_id = r.chunk_id
                   )
                 UNION
                 SELECT COALESCE(r.claim, ''), r.evidence, r.status, r.id
                 FROM chunk_provenance p INDEXED BY idx_chunk_provenance_source_conv
                 JOIN chunks c ON c.id = p.chunk_id
                 JOIN resolution_ledger r ON r.chunk_id = c.id
                 WHERE p.source_conv_id = ?1
                   AND c.project_name = ?2
                   AND r.status = 'resolved'
                   AND r.id = (
                       SELECT MAX(latest.id)
                       FROM resolution_ledger latest
                       WHERE latest.chunk_id = r.chunk_id
                   )
             )
             ORDER BY rid DESC
             LIMIT ?3",
        )?;
        let settled = settled_statement
            .query_map(
                params![conversation_id, project, LEDGER_FEED_LIMIT],
                settled_fact_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut open_statement = conn.prepare(
            "SELECT COALESCE(r.claim, ''), r.evidence, r.status
             FROM resolution_ledger r INDEXED BY idx_resolution_open_recent
             JOIN chunks c ON c.id = r.chunk_id
             WHERE r.status IN ('still_open', 'regressed')
               AND c.project_name = ?1
               AND NOT EXISTS (
                   SELECT 1
                   FROM resolution_ledger latest
                   WHERE latest.chunk_id = r.chunk_id
                     AND latest.id > r.id
               )
             ORDER BY r.id DESC
             LIMIT ?2",
        )?;
        let still_open = open_statement
            .query_map(params![project, LEDGER_FEED_LIMIT], settled_fact_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok((settled, still_open))
    }

    /// Negative dream verdicts recorded strictly after `since_ts`, scoped by
    /// the project carried directly on their witness ledger rows.
    /// `CSR_DREAM_CONSUMPTION` (default OFF — see `dream_consumption_enabled`)
    /// gates this feed: ships the witness ledger as experimental derived
    /// data, so the "Learnt-then-retired while away:" recap clause never
    /// reaches a user who hasn't explicitly opted in.
    pub fn recap_retired_since(&self, project: &str, since_ts: &str) -> Result<Vec<RetiredLine>> {
        self.recap_retired_since_with(project, since_ts, dream_consumption_enabled())
    }

    /// Core of [`recap_retired_since`] with the dream-consumption opt-in
    /// passed in as a parameter — mirrors `active_forgetting_enabled_from`'s
    /// pattern in `mcp::tools` (tests drive this directly instead of
    /// mutating the process env). `consumption_enabled = false` returns an
    /// empty vector WITHOUT ever touching `witness_verdicts` (the early
    /// return below is before the connection lock and the query) — the
    /// recap composer already drops the "Learnt-then-retired while away:"
    /// clause when its feed is empty, so no composer/grammar change is
    /// needed anywhere.
    pub fn recap_retired_since_with(
        &self,
        project: &str,
        since_ts: &str,
        consumption_enabled: bool,
    ) -> Result<Vec<RetiredLine>> {
        if !consumption_enabled {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|error| anyhow::anyhow!("lock: {error}"))?;
        let mut statement = conn.prepare(
            "SELECT COALESCE(NULLIF(wl.symbol, ''), wl.file),
                    COALESCE(v.receipt_oid, ''),
                    SUBSTR(datetime(v.created_at), 1, 10)
             FROM witness_verdicts v INDEXED BY idx_witness_verdicts_recap_created
             JOIN witness_ledger wl ON wl.id = v.witness_id
             WHERE julianday(v.created_at) > julianday(?1)
               AND wl.project = ?2
               AND v.verdict IN ('superseded_by', 'anchor_obsolete')
             ORDER BY julianday(v.created_at) DESC, v.id DESC
             LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![since_ts, project, RETIRED_FEED_LIMIT], |row| {
                let receipt: String = row.get(1)?;
                Ok(RetiredLine {
                    label: row.get(0)?,
                    receipt_oid: shorten_receipt(&receipt),
                    date: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Count project proposals that have not received any promoted ledger
    /// verdict. Project scope is resolved through the proposal's chunk id.
    pub fn recap_open_proposals(&self, project: &str) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| anyhow::anyhow!("lock: {error}"))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM resolution_proposals p
             CROSS JOIN chunks c ON c.id = p.chunk_id
             WHERE c.project_name = ?1
               AND NOT EXISTS (
                   SELECT 1
                   FROM resolution_ledger r
                   WHERE r.chunk_id = p.chunk_id
                     AND julianday(r.created_at) > julianday(p.created_at)
               )",
            [project],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}

fn settled_fact_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SettledFact> {
    let evidence: String = row.get(1)?;
    Ok(SettledFact {
        claim: row.get(0)?,
        receipt: shorten_receipt(&evidence),
        status: row.get(2)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    fn seed_chunk(storage: &Storage, id: &str, conversation: &str, project: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO chunks
                        (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES (?1, ?2, ?3, '2026-08-01T00:00:00Z', 'fixture', 1)",
                    rusqlite::params![id, conversation, project],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_resolution(
        storage: &Storage,
        chunk_id: &str,
        status: &str,
        evidence: &str,
        claim: &str,
    ) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO resolution_ledger
                        (chunk_id, status, evidence, claim, created_at)
                     VALUES (?1, ?2, ?3, ?4, '2026-08-02T00:00:00Z')",
                    rusqlite::params![chunk_id, status, evidence, claim],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_witness_verdict(
        storage: &Storage,
        project: &str,
        file: &str,
        symbol: Option<&str>,
        verdict: &str,
        receipt: &str,
        created_at: &str,
    ) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO witness_ledger
                        (project, file, symbol, stamp, tier, at_oid, source_kind)
                     VALUES (?1, ?2, ?3, ?4, 'committed', ?5, 'fixture')",
                    rusqlite::params![project, file, symbol, format!("b3:{receipt}"), receipt],
                )?;
                let witness_id = conn.last_insert_rowid();
                conn.execute(
                    "INSERT INTO witness_verdicts
                        (witness_id, verdict, receipt_oid, observed_head_oid, created_at)
                     VALUES (?1, ?2, ?3, 'head000', ?4)",
                    rusqlite::params![witness_id, verdict, receipt, created_at],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn receipt_shortening_prefers_first_oid_and_falls_back_to_preview() {
        assert_eq!(
            shorten_receipt("T2 backfill 2026-07-27: 73878dd -- chore(done)"),
            "73878dd"
        );
        assert_eq!(
            shorten_receipt("receipt deadbeefcafebabe complete"),
            "deadbee"
        );
        assert_eq!(
            shorten_receipt("no oid receipt needs a compact preview"),
            "no oid receipt needs a c"
        );
        assert_eq!(
            shorten_receipt("session 31cb2889-e122-474f-9451-38a79406a1f7"),
            "session 31cb2889-e122-47"
        );
        assert_eq!(
            shorten_receipt("numeric id 123456789 and b3 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
            "numeric id 123456789 and"
        );
        assert_eq!(
            shorten_receipt("commit: abcdef1234567890 finished"),
            "abcdef1"
        );
        assert_eq!(shorten_receipt("commit 1234567 completed"), "1234567");
        // Keyword fused to the hex run is NOT an OID context — separator required.
        assert_eq!(
            shorten_receipt("receipt1234567 pending review here"),
            "receipt1234567 pending r"
        );
        assert_eq!(
            shorten_receipt("commitdeadbeef was not a real commit"),
            "commitdeadbeef was not a"
        );
    }

    #[test]
    fn ledger_feeds_split_latest_verdicts_and_scope_by_project_and_conversation() {
        let storage = Storage::open_memory().unwrap();
        seed_chunk(&storage, "settled", "current", "alpha");
        seed_chunk(&storage, "open", "older", "alpha");
        seed_chunk(&storage, "other-project", "current", "beta");
        seed_chunk(&storage, "other-conversation", "different", "alpha");

        seed_resolution(&storage, "settled", "still_open", "old receipt", "old");
        seed_resolution(
            &storage,
            "settled",
            "resolved",
            "verified at abcdef123456",
            "settled claim",
        );
        seed_resolution(
            &storage,
            "open",
            "regressed",
            "receipt 123abcd9",
            "open claim",
        );
        seed_resolution(
            &storage,
            "other-project",
            "resolved",
            "receipt fedcba9",
            "wrong project",
        );
        seed_resolution(
            &storage,
            "other-conversation",
            "resolved",
            "receipt 7654321",
            "wrong conversation",
        );

        let (settled, still_open) = storage.recap_ledger_feeds("alpha", "current").unwrap();

        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].claim, "settled claim");
        assert_eq!(settled[0].receipt, "abcdef1");
        assert_eq!(settled[0].status, "resolved");
        assert_eq!(still_open.len(), 1);
        assert_eq!(still_open[0].claim, "open claim");
        assert_eq!(still_open[0].status, "regressed");
    }

    #[test]
    fn settled_feed_credits_sidechain_chunks_via_provenance_parent() {
        let storage = Storage::open_memory().unwrap();
        // Sidechain chunk: own conversation_id, parented to "current" via provenance.
        seed_chunk(&storage, "side", "agent-child-conv", "alpha");
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO chunk_provenance (chunk_id, author, source_conv_id)
                     VALUES ('side', 'sidechain', 'current')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        seed_resolution(
            &storage,
            "side",
            "resolved",
            "receipt abc9876",
            "sidechain claim",
        );

        let (settled, _) = storage.recap_ledger_feeds("alpha", "current").unwrap();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].claim, "sidechain claim");
        assert_eq!(settled[0].receipt, "abc9876");
    }

    #[test]
    fn ledger_feeds_cap_each_bucket_at_five_newest_rows() {
        let storage = Storage::open_memory().unwrap();
        for n in 0..6 {
            let settled_id = format!("settled-{n}");
            seed_chunk(&storage, &settled_id, "current", "alpha");
            seed_resolution(
                &storage,
                &settled_id,
                "resolved",
                &format!("receipt aaaaaa{n}"),
                &format!("settled {n}"),
            );
            let open_id = format!("open-{n}");
            seed_chunk(&storage, &open_id, "older", "alpha");
            seed_resolution(
                &storage,
                &open_id,
                "still_open",
                &format!("receipt bbbbbb{n}"),
                &format!("open {n}"),
            );
        }

        let (settled, still_open) = storage.recap_ledger_feeds("alpha", "current").unwrap();

        assert_eq!(settled.len(), 5);
        assert_eq!(still_open.len(), 5);
        assert_eq!(settled[0].claim, "settled 5");
        assert_eq!(still_open[0].claim, "open 5");
    }

    #[test]
    fn retired_feed_scopes_since_boundary_and_caps_newest_three() {
        let storage = Storage::open_memory().unwrap();
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/boundary.rs",
            Some("boundary"),
            "anchor_obsolete",
            "0000000f",
            "2026-08-01T00:00:00Z",
        );
        seed_witness_verdict(
            &storage,
            "beta",
            "/repo/src/other.rs",
            Some("other"),
            "anchor_obsolete",
            "1111111f",
            "2026-08-06T00:00:00Z",
        );
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/reinstated.rs",
            Some("live_again"),
            "anchor_reinstated",
            "2222222f",
            "2026-08-06T00:00:00Z",
        );
        for n in 0..4 {
            seed_witness_verdict(
                &storage,
                "alpha",
                &format!("/repo/src/file-{n}.rs"),
                (n != 0).then_some("retired_symbol"),
                if n % 2 == 0 {
                    "anchor_obsolete"
                } else {
                    "superseded_by"
                },
                &format!("abcde{n}f999"),
                &format!("2026-08-0{}T00:00:00Z", n + 2),
            );
        }

        let retired = storage
            .recap_retired_since_with("alpha", "2026-08-01T00:00:00Z", true)
            .unwrap();

        assert_eq!(retired.len(), 3);
        assert_eq!(retired[0].label, "retired_symbol");
        assert_eq!(retired[0].receipt_oid, "abcde3f");
        assert_eq!(retired[0].date, "2026-08-05");
        assert_eq!(retired[2].label, "retired_symbol");
        assert!(retired.iter().all(|line| line.label != "boundary"));
        assert!(retired.iter().all(|line| line.label != "other"));
        assert!(retired.iter().all(|line| line.label != "live_again"));
    }

    #[test]
    fn retired_feed_normalizes_sqlite_and_rfc3339_timestamps() {
        let storage = Storage::open_memory().unwrap();
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/mixed.rs",
            Some("mixed_timestamp"),
            "anchor_obsolete",
            "abcdef1234567890",
            "2026-08-02 00:00:01",
        );

        let retired = storage
            .recap_retired_since_with("alpha", "2026-08-02T00:00:00Z", true)
            .unwrap();

        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].label, "mixed_timestamp");
    }

    #[test]
    fn retired_feed_preserves_rfc3339_fractional_second_ordering() {
        let storage = Storage::open_memory().unwrap();
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/fractional.rs",
            Some("fractionally_later"),
            "anchor_obsolete",
            "abcdef1234567890",
            "2026-08-02T00:00:00.900Z",
        );

        let retired = storage
            .recap_retired_since_with("alpha", "2026-08-02T00:00:00.100Z", true)
            .unwrap();

        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].label, "fractionally_later");
    }

    #[test]
    fn retired_feed_orders_by_normalized_timestamp_before_id() {
        let storage = Storage::open_memory().unwrap();
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/newer.rs",
            Some("newer_by_time"),
            "anchor_obsolete",
            "abcdef1234567890",
            "2026-08-03 00:00:00",
        );
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/older.rs",
            Some("older_by_time"),
            "anchor_obsolete",
            "fedcba1234567890",
            "2026-08-02T00:00:00Z",
        );

        let retired = storage
            .recap_retired_since_with("alpha", "2026-08-01T00:00:00Z", true)
            .unwrap();

        assert_eq!(retired[0].label, "newer_by_time");
    }

    #[test]
    fn dream_consumption_parsing_is_opt_in_only() {
        assert!(dream_consumption_enabled_from(Some("1")));
        for value in [None, Some("0"), Some("true"), Some("yes"), Some("")] {
            assert!(
                !dream_consumption_enabled_from(value),
                "value {value:?} must leave dream consumption off"
            );
        }
    }

    #[test]
    fn retired_feed_off_returns_empty_without_touching_witness_verdicts() {
        let storage = Storage::open_memory().unwrap();
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/file.rs",
            Some("retired_symbol"),
            "anchor_obsolete",
            "abcdef1",
            "2026-08-05T00:00:00Z",
        );

        let retired = storage
            .recap_retired_since_with("alpha", "2026-08-01T00:00:00Z", false)
            .unwrap();

        assert!(
            retired.is_empty(),
            "CSR_DREAM_CONSUMPTION default OFF must suppress the feed even \
             with a populated witness_verdicts table"
        );
    }

    #[test]
    fn retired_feed_off_never_queries_witness_verdicts_table_at_all() {
        // Stronger than the sibling test above: proves the query itself never
        // fires, not just that its result happens to be empty. If
        // `recap_retired_since_with(..., false)` executed the real query
        // against a DB missing `witness_verdicts` entirely, it would return
        // `Err`, not `Ok(vec![])` — the early return in the function must
        // land before the connection lock / query, exactly as documented.
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                conn.execute_batch("DROP TABLE witness_verdicts")?;
                Ok(())
            })
            .unwrap();

        let retired = storage
            .recap_retired_since_with("alpha", "2026-08-01T00:00:00Z", false)
            .unwrap();
        assert!(retired.is_empty());
    }

    #[test]
    fn retired_feed_off_empties_the_composer_clause_with_no_grammar_change() {
        use crate::hooks::recap::{compose_recap, RecapFeeds};
        use crate::hooks::stop::Episode;

        let storage = Storage::open_memory().unwrap();
        seed_witness_verdict(
            &storage,
            "alpha",
            "/repo/src/file.rs",
            Some("retired_symbol"),
            "anchor_obsolete",
            "abcdef1",
            "2026-08-05T00:00:00Z",
        );
        let retired_while_away = storage
            .recap_retired_since_with("alpha", "2026-08-01T00:00:00Z", false)
            .unwrap();
        assert!(retired_while_away.is_empty());

        let ep = Episode {
            schema: "session_episode/v2".into(),
            session_id: "session-1".into(),
            project: "alpha".into(),
            timestamp: "2026-08-07T10:00:00Z".into(),
            request: "Fix the recap composer".into(),
            investigated: vec![],
            completed: "Implemented deterministic recap output".into(),
            next_steps: None,
            blockers: None,
            outcome: "partial".into(),
            error_signatures: vec![],
            tools_used: vec![],
            files_modified: vec![],
            message_count: 4,
            duration_minutes: 12,
            todos: vec![],
            approved_plan: None,
            prev_episode_id: None,
            anchors: vec![],
        };
        let feeds = RecapFeeds {
            settled: vec![],
            still_open: vec![],
            retired_while_away,
            open_proposals: 0,
        };

        let got = compose_recap(&ep, &feeds, "1h ago").unwrap();
        assert!(
            !got.contains("Learnt-then-retired while away:"),
            "clause must drop when its feed is empty (composer grammar \
             untouched): {got}"
        );
    }

    #[test]
    fn open_proposals_count_excludes_promoted_and_other_projects() {
        let storage = Storage::open_memory().unwrap();
        seed_chunk(&storage, "alpha-open", "a", "alpha");
        seed_chunk(&storage, "alpha-promoted", "a", "alpha");
        seed_chunk(&storage, "beta-open", "b", "beta");
        storage
            .with_connection(|conn| {
                for (chunk_id, session_id) in [
                    ("alpha-open", "s1"),
                    ("alpha-promoted", "s2"),
                    ("beta-open", "s3"),
                ] {
                    conn.execute(
                        "INSERT INTO resolution_proposals
                            (chunk_id, claim, evidence, session_id, created_at)
                         VALUES (?1, 'claim', 'evidence', ?2, '2026-08-01 00:00:00')",
                        rusqlite::params![chunk_id, session_id],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        seed_resolution(&storage, "alpha-promoted", "resolved", "promoted", "claim");

        assert_eq!(storage.recap_open_proposals("alpha").unwrap(), 1);
        assert_eq!(storage.recap_open_proposals("beta").unwrap(), 1);
    }

    #[test]
    fn open_proposal_ignores_prerequisite_ledger_row_but_not_later_promotion() {
        let storage = Storage::open_memory().unwrap();
        seed_chunk(&storage, "proposal", "a", "alpha");
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO resolution_ledger
                        (chunk_id, status, evidence, claim, created_at)
                     VALUES ('proposal', 'still_open', 'prerequisite', 'claim',
                             '2026-08-01 00:00:00')",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO resolution_proposals
                        (chunk_id, claim, evidence, session_id, created_at)
                     VALUES ('proposal', 'claim', 'candidate', 'session',
                             '2026-08-02 00:00:00')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        assert_eq!(storage.recap_open_proposals("alpha").unwrap(), 1);

        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO resolution_ledger
                        (chunk_id, status, evidence, claim, created_at)
                     VALUES ('proposal', 'resolved', 'promoted', 'claim',
                             '2026-08-03T00:00:00Z')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        assert_eq!(storage.recap_open_proposals("alpha").unwrap(), 0);
    }

    #[test]
    fn recap_query_indexes_match_capped_feed_order_and_join_direction() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                let retired_plan = conn
                    .prepare(
                        "EXPLAIN QUERY PLAN
                         SELECT v.witness_id
                         FROM witness_verdicts v INDEXED BY idx_witness_verdicts_recap_created
                         JOIN witness_ledger wl ON wl.id = v.witness_id
                         WHERE julianday(v.created_at) > julianday(?1)
                           AND wl.project = ?2
                           AND v.verdict IN ('superseded_by', 'anchor_obsolete')
                         ORDER BY julianday(v.created_at) DESC, v.id DESC
                         LIMIT 3",
                    )?
                    .query_map(rusqlite::params!["2026-08-01T00:00:00Z", "alpha"], |row| {
                        row.get::<_, String>(3)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .join("\n");
                assert!(retired_plan.contains("idx_witness_verdicts_recap_created"));
                assert!(!retired_plan.contains("USE TEMP B-TREE FOR ORDER BY"));

                let open_plan = conn
                    .prepare(
                        "EXPLAIN QUERY PLAN
                         SELECT r.id
                         FROM resolution_ledger r INDEXED BY idx_resolution_open_recent
                         JOIN chunks c ON c.id = r.chunk_id
                         WHERE r.status IN ('still_open', 'regressed')
                           AND c.project_name = ?1
                           AND NOT EXISTS (
                               SELECT 1 FROM resolution_ledger latest
                               WHERE latest.chunk_id = r.chunk_id
                                 AND latest.id > r.id
                           )
                         ORDER BY r.id DESC
                         LIMIT 5",
                    )?
                    .query_map(["alpha"], |row| row.get::<_, String>(3))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .join("\n");
                assert!(open_plan.contains("idx_resolution_open_recent"));
                assert!(!open_plan.contains("idx_chunks_project"));

                let proposal_plan = conn
                    .prepare(
                        "EXPLAIN QUERY PLAN
                         SELECT COUNT(*)
                         FROM resolution_proposals p
                         CROSS JOIN chunks c ON c.id = p.chunk_id
                         WHERE c.project_name = ?1
                           AND NOT EXISTS (
                               SELECT 1 FROM resolution_ledger r
                               WHERE r.chunk_id = p.chunk_id
                                 AND julianday(r.created_at) > julianday(p.created_at)
                           )",
                    )?
                    .query_map(["alpha"], |row| row.get::<_, String>(3))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .join("\n");
                let proposal_first = proposal_plan.lines().next().unwrap_or_default();
                assert!(proposal_first.contains("SCAN p"));
                assert!(!proposal_plan.contains("idx_chunks_project"));
                Ok(())
            })
            .unwrap();
    }
}
