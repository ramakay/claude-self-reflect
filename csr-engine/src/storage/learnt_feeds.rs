//! Read-only, session-keyed evidence feeds for the SessionStart `learnt` line.
//!
//! Keyed by session id wherever the table has one (`memory_registry`,
//! `resolution_ledger` through `provenance_events`). Dream verdicts have no
//! session column: they are keyed by project plus the session window, and only
//! count when the receipt commit itself falls inside the window. Every query is
//! an index or primary-key lookup except the confirmed-verdict scan of the
//! ledger-sized `resolution_ledger` (no index on `source`/`event_id`; the table
//! holds one row per `csr_resolve` call). None touches `chunks`.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use super::recap_feeds::{shorten_receipt, ConsumptionMode};
use super::witness_verdicts::LIVE_EVENT_FLOOR_SQL;
use super::Storage;
use crate::hooks::learnt::{ConfirmedItem, LearntFeeds, VerdictGroup};

/// Slugs read per session: three are shown, a few spare let the composer skip
/// one the recap already names.
const NOTE_SLUG_LIMIT: i64 = 6;
const CONFIRMED_LIMIT: i64 = 3;
const VERDICT_GROUP_LIMIT: i64 = 2;
/// Newest verdict rows aggregated, bounding the window scan on a bursty ledger.
const VERDICT_SCAN_LIMIT: i64 = 5000;

impl Storage {
    /// `session_registry.first_ts`, the start of the session window. `None` when
    /// the session is not registered: window-keyed classes then stay empty,
    /// never guessed.
    pub fn learnt_session_start(&self, session_id: &str) -> Result<Option<String>> {
        self.with_connection(|conn| {
            Ok(conn
                .query_row(
                    "SELECT first_ts FROM session_registry WHERE session_id = ?1",
                    params![session_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .filter(|ts| !ts.is_empty()))
        })
    }

    /// Memory notes written in the session: total, newest slugs, newest date.
    pub fn learnt_memory_notes(
        &self,
        session_id: &str,
    ) -> Result<(usize, Vec<String>, Option<String>)> {
        self.with_connection(|conn| {
            let (total, newest): (i64, Option<String>) = conn.query_row(
                "SELECT COUNT(DISTINCT file_path), MAX(modified_ts)
                 FROM memory_registry WHERE origin_session_id = ?1 AND slug <> ''",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if total == 0 {
                return Ok((0, Vec::new(), None));
            }
            let mut statement = conn.prepare(
                "SELECT slug FROM memory_registry
                 WHERE origin_session_id = ?1 AND slug <> ''
                 GROUP BY slug ORDER BY MAX(modified_ts) DESC, slug LIMIT ?2",
            )?;
            let slugs = statement
                .query_map(params![session_id, NOTE_SLUG_LIMIT], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            let date = newest.map(|ts| ts.chars().take(10).collect::<String>());
            Ok((total as usize, slugs, date))
        })
    }

    /// User-confirmed `csr_resolve` verdicts recorded in the session: the latest
    /// confirmed row per chunk whose `event_id` resolves, by primary key, to a
    /// provenance event of this conversation. Agent rows are never read.
    pub fn learnt_confirmed_resolutions(
        &self,
        session_id: &str,
    ) -> Result<(usize, Vec<ConfirmedItem>)> {
        self.with_connection(|conn| {
            let mut statement = conn.prepare(
                "SELECT COALESCE(r.claim, ''), r.evidence, r.status
                 FROM resolution_ledger r
                 JOIN provenance_events pe ON pe.event_id = r.event_id
                 WHERE r.source = 'user_confirmed'
                   AND r.event_id IS NOT NULL
                   AND pe.conversation_id = ?1
                   AND r.id = (
                       SELECT MAX(latest.id) FROM resolution_ledger latest
                       WHERE latest.chunk_id = r.chunk_id
                         AND latest.source = 'user_confirmed'
                   )
                 ORDER BY r.id DESC",
            )?;
            let rows = statement
                .query_map(params![session_id], |row| {
                    let evidence: String = row.get(1)?;
                    Ok(ConfirmedItem {
                        claim: row.get(0)?,
                        status: row.get(2)?,
                        receipt: shorten_receipt(&evidence),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let total = rows.len();
            Ok((
                total,
                rows.into_iter().take(CONFIRMED_LIMIT as usize).collect(),
            ))
        })
    }

    /// Negative dream verdicts recorded inside `[window_start, window_end]` for
    /// `project`, grouped by receipt commit, counting distinct witnesses. A group
    /// counts only when its commit date is inside the window as well: a long
    /// session otherwise sweeps in old backfill commits. On by default
    /// (`annotate`): the clause carries counts and commits, never a retired
    /// claim, so the recap's full-only gate for named retirements does not
    /// apply. Empty under `CSR_DREAM_CONSUMPTION=off`, without touching
    /// `witness_verdicts`.
    pub fn learnt_verdicts(
        &self,
        project: &str,
        window_start: &str,
        window_end: &str,
        mode: ConsumptionMode,
    ) -> Result<Vec<VerdictGroup>> {
        if mode == ConsumptionMode::Off {
            return Ok(Vec::new());
        }
        self.with_connection(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT oid, COUNT(DISTINCT witness_id), MIN(committed)
                 FROM (
                     SELECT v.witness_id AS witness_id,
                            v.receipt_oid AS oid,
                            SUBSTR(v.receipt_committed_at, 1, 10) AS committed
                     FROM witness_verdicts v INDEXED BY idx_witness_verdicts_recap_created
                     JOIN witness_ledger wl ON wl.id = v.witness_id
                     WHERE julianday(v.created_at) BETWEEN julianday(?1) AND julianday(?2)
                       AND wl.project = ?3
                       AND v.verdict IN ('superseded_by', 'anchor_obsolete')
                       AND v.id > {LIVE_EVENT_FLOOR_SQL}
                       AND v.receipt_oid IS NOT NULL AND v.receipt_oid <> ''
                       AND julianday(v.receipt_committed_at)
                           BETWEEN julianday(?1) AND julianday(?2)
                     ORDER BY julianday(v.created_at) DESC, v.id DESC
                     LIMIT ?4
                 )
                 GROUP BY oid
                 ORDER BY COUNT(DISTINCT witness_id) DESC, oid
                 LIMIT ?5"
            ))?;
            let groups = statement
                .query_map(
                    params![
                        window_start,
                        window_end,
                        project,
                        VERDICT_SCAN_LIMIT,
                        VERDICT_GROUP_LIMIT
                    ],
                    |row| {
                        let oid: String = row.get(0)?;
                        Ok(VerdictGroup {
                            oid: shorten_receipt(&oid),
                            count: row.get::<_, i64>(1)?.max(0) as usize,
                            date: row.get(2)?,
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(groups)
        })
    }

    /// Every learnt feed for one session. Fail-open: a missing table, a locked
    /// connection or any query error is an empty feed, never an error.
    pub fn learnt_feeds(
        &self,
        project: &str,
        session_id: &str,
        window_end: &str,
        mode: ConsumptionMode,
    ) -> LearntFeeds {
        let mut feeds = LearntFeeds::default();
        match self.learnt_memory_notes(session_id) {
            Ok((total, slugs, date)) => {
                feeds.notes_total = total;
                feeds.notes = slugs;
                feeds.notes_date = date;
            }
            Err(error) => tracing::debug!(%error, "learnt memory-note feed unavailable"),
        }
        match self.learnt_confirmed_resolutions(session_id) {
            Ok((total, items)) => {
                feeds.confirmed_total = total;
                feeds.confirmed = items;
            }
            Err(error) => tracing::debug!(%error, "learnt confirmed-verdict feed unavailable"),
        }
        if mode != ConsumptionMode::Off {
            match self.learnt_session_start(session_id) {
                Ok(Some(start)) => match self.learnt_verdicts(project, &start, window_end, mode) {
                    Ok(groups) => feeds.verdicts = groups,
                    Err(error) => tracing::debug!(%error, "learnt verdict feed unavailable"),
                },
                Ok(None) => {}
                Err(error) => tracing::debug!(%error, "learnt session window unavailable"),
            }
        }
        feeds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(storage: &Storage, sql: &str, args: &[&dyn rusqlite::ToSql]) {
        storage
            .with_connection(|conn| {
                conn.execute(sql, args)?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_note(storage: &Storage, file: &str, slug: &str, origin: &str, modified: Option<&str>) {
        exec(
            storage,
            "INSERT INTO memory_registry
                (file_path, project, slug, description, mem_type, origin_session_id,
                 modified_ts, file_mtime, content_hash, links_json, last_seen_scan)
             VALUES (?1, 'p', ?2, 'd', 'project', ?3, ?4, 0, 'h', '[]', 1)",
            &[&file, &slug, &origin, &modified],
        );
    }

    fn seed_session(storage: &Storage, sid: &str, first_ts: &str) {
        exec(
            storage,
            "INSERT INTO session_registry (session_id, project, first_ts, last_ts, prompt_count)
             VALUES (?1, 'p', ?2, ?2, 5)",
            &[&sid, &first_ts],
        );
    }

    #[test]
    fn notes_are_scoped_by_origin_session_newest_first() {
        let s = Storage::open_memory().unwrap();
        seed_note(&s, "/a", "oldest", "sess-1", Some("2026-10-14T01:00:00Z"));
        seed_note(&s, "/b", "newest", "sess-1", Some("2026-10-16T09:00:00Z"));
        seed_note(&s, "/c", "middle", "sess-1", Some("2026-10-15T09:00:00Z"));
        seed_note(
            &s,
            "/d",
            "other-session",
            "sess-2",
            Some("2026-10-17T09:00:00Z"),
        );
        seed_note(&s, "/e", "", "sess-1", Some("2026-10-17T09:00:00Z"));
        let (total, slugs, date) = s.learnt_memory_notes("sess-1").unwrap();
        assert_eq!(total, 3);
        assert_eq!(slugs, ["newest", "middle", "oldest"]);
        assert_eq!(date.as_deref(), Some("2026-10-16"));
        assert_eq!(s.learnt_memory_notes("nobody").unwrap(), (0, vec![], None));
        // No modified timestamp at all: counted, but no date to receipt it by.
        seed_note(&s, "/f", "undated", "sess-3", None);
        let (total, slugs, date) = s.learnt_memory_notes("sess-3").unwrap();
        assert_eq!((total, slugs, date), (1, vec!["undated".to_string()], None));
    }

    fn seed_event(storage: &Storage, event: &str, conversation: &str) {
        exec(
            storage,
            "INSERT INTO provenance_events
                (event_id, conversation_id, message_key, seq, channel, receipt_kind, observed_at)
             VALUES (?1, ?2, 'm', 0, 'user', 'transcript', '2026-10-15T00:00:00Z')",
            &[&event, &conversation],
        );
    }

    fn seed_resolution(
        storage: &Storage,
        chunk: &str,
        source: &str,
        event: Option<&str>,
        evidence: &str,
    ) {
        exec(
            storage,
            "INSERT INTO resolution_ledger (chunk_id, status, evidence, claim, source, event_id)
             VALUES (?1, 'resolved', ?2, 'the claim', ?3, ?4)",
            &[&chunk, &evidence, &source, &event],
        );
    }

    #[test]
    fn confirmed_feed_ignores_agent_rows_and_other_sessions() {
        let s = Storage::open_memory().unwrap();
        seed_event(&s, "ev-1", "sess-1");
        seed_event(&s, "ev-2", "sess-2");
        seed_resolution(
            &s,
            "c1",
            "user_confirmed",
            Some("ev-1"),
            "commit a624305 landed",
        );
        seed_resolution(&s, "c2", "agent", Some("ev-1"), "agent assertion");
        seed_resolution(&s, "c3", "user_confirmed", Some("ev-2"), "other session");
        seed_resolution(&s, "c4", "user_confirmed", None, "no event link");
        let (total, items) = s.learnt_confirmed_resolutions("sess-1").unwrap();
        assert_eq!(total, 1);
        assert_eq!(
            items,
            [ConfirmedItem {
                claim: "the claim".into(),
                status: "resolved".into(),
                receipt: "a624305".into()
            }]
        );
        assert_eq!(s.learnt_confirmed_resolutions("nobody").unwrap().0, 0);
    }

    fn seed_verdict(
        storage: &Storage,
        project: &str,
        file: &str,
        oid: &str,
        verdict: &str,
        created: &str,
        committed: Option<&str>,
    ) {
        exec(
            storage,
            "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, at_oid, source_kind)
             VALUES (?1, ?2, NULL, ?3, 'committed', ?4, 'fixture')",
            &[&project, &file, &format!("b3:{file}{oid}"), &oid],
        );
        exec(
            storage,
            "INSERT INTO witness_verdicts
                (witness_id, verdict, receipt_oid, observed_head_oid, created_at, receipt_committed_at)
             VALUES (last_insert_rowid(), ?1, ?2, 'head000', ?3, ?4)",
            &[&verdict, &oid, &created, &committed],
        );
    }

    #[test]
    fn verdicts_hide_only_when_off_and_are_project_scoped_aggregated_and_commit_dated_in_window() {
        let s = Storage::open_memory().unwrap();
        let (start, end) = ("2026-10-08T22:33:09+00:00", "2026-10-15T04:20:30+00:00");
        let in_window = "2026-10-13T08:00:00-07:00";
        for i in 0..3 {
            seed_verdict(
                &s,
                "p",
                &format!("f{i}.rs"),
                "12bf158aaaa",
                "anchor_obsolete",
                "2026-10-13 18:00:00",
                Some(in_window),
            );
        }
        seed_verdict(
            &s,
            "p",
            "g.rs",
            "12bf158aaaa",
            "superseded_by",
            "2026-10-13 18:00:00",
            Some(in_window),
        );
        seed_verdict(
            &s,
            "p",
            "h.rs",
            "d895eea",
            "superseded_by",
            "2026-10-13 19:00:00",
            Some(in_window),
        );
        // Old backfill commit swept into the window: dropped. No commit date: dropped.
        seed_verdict(
            &s,
            "p",
            "old.rs",
            "ca5655a",
            "superseded_by",
            "2026-10-13 19:00:00",
            Some("2026-09-17T10:00:00Z"),
        );
        seed_verdict(
            &s,
            "p",
            "nodate.rs",
            "bbbbbbb",
            "superseded_by",
            "2026-10-13 19:00:00",
            None,
        );
        // Other project, reinstated verdict, and a verdict after the window.
        seed_verdict(
            &s,
            "other",
            "x.rs",
            "eeeeeee",
            "superseded_by",
            "2026-10-13 19:00:00",
            Some(in_window),
        );
        seed_verdict(
            &s,
            "p",
            "y.rs",
            "fffffff",
            "anchor_reinstated",
            "2026-10-13 19:00:00",
            Some(in_window),
        );
        seed_verdict(
            &s,
            "p",
            "z.rs",
            "1111111",
            "superseded_by",
            "2026-10-16 05:20:00",
            Some(in_window),
        );

        assert!(s
            .learnt_verdicts("p", start, end, ConsumptionMode::Off)
            .unwrap()
            .is_empty());
        // Counts only, so on by default: annotate and full agree.
        let groups = s
            .learnt_verdicts("p", start, end, ConsumptionMode::AnnotateOnly)
            .unwrap();
        assert_eq!(
            groups,
            s.learnt_verdicts("p", start, end, ConsumptionMode::Full)
                .unwrap()
        );
        assert_eq!(
            groups,
            [
                VerdictGroup {
                    oid: "12bf158".into(),
                    count: 4,
                    date: "2026-10-13".into()
                },
                VerdictGroup {
                    oid: "d895eea".into(),
                    count: 1,
                    date: "2026-10-13".into()
                },
            ]
        );
    }

    #[test]
    fn feeds_wrapper_needs_a_registered_session_window_and_dreams_not_off() {
        let s = Storage::open_memory().unwrap();
        seed_note(&s, "/a", "a-note", "sess-1", Some("2026-10-16T09:00:00Z"));
        let in_window = "2026-10-13T08:00:00Z";
        seed_verdict(
            &s,
            "p",
            "f.rs",
            "12bf158",
            "superseded_by",
            "2026-10-13 18:00:00",
            Some(in_window),
        );
        let end = "2026-10-15T04:20:30+00:00";

        // Unregistered session: no window, so no verdicts, but notes still stand.
        let feeds = s.learnt_feeds("p", "sess-1", end, ConsumptionMode::Full);
        assert_eq!(feeds.notes, ["a-note"]);
        assert!(feeds.verdicts.is_empty());

        seed_session(&s, "sess-1", "2026-10-08T22:33:09+00:00");
        let feeds = s.learnt_feeds("p", "sess-1", end, ConsumptionMode::Full);
        assert_eq!(feeds.verdicts.len(), 1);
        let feeds = s.learnt_feeds("p", "sess-1", end, ConsumptionMode::AnnotateOnly);
        assert_eq!(feeds.verdicts.len(), 1);
        let feeds = s.learnt_feeds("p", "sess-1", end, ConsumptionMode::Off);
        assert!(feeds.verdicts.is_empty());
        assert_eq!(feeds.notes_total, 1);
    }

    #[test]
    fn a_missing_table_is_an_empty_feed_not_an_error() {
        let s = Storage::open_memory().unwrap();
        exec(&s, "DROP TABLE memory_registry", &[]);
        exec(&s, "DROP TABLE resolution_ledger", &[]);
        let feeds = s.learnt_feeds("p", "sess-1", "2026-10-15T00:00:00Z", ConsumptionMode::Full);
        assert_eq!(feeds, LearntFeeds::default());
    }

    #[test]
    fn learnt_queries_never_scan_chunks_provenance_events_or_witness_verdicts() {
        let s = Storage::open_memory().unwrap();
        let plan = |sql: &str, n: usize| -> String {
            s.with_connection(|conn| {
                let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let mut rows =
                    statement.query(rusqlite::params_from_iter(std::iter::repeat_n("x", n)))?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(row.get::<_, String>(3)?);
                }
                Ok(out.join("\n"))
            })
            .unwrap()
        };
        let notes = plan(
            "SELECT slug FROM memory_registry WHERE origin_session_id = ?1 AND slug <> ''
             GROUP BY slug ORDER BY MAX(modified_ts) DESC, slug LIMIT 6",
            1,
        );
        assert!(notes.contains("idx_memory_registry_origin"), "{notes}");
        let confirmed = plan(
            "SELECT r.id FROM resolution_ledger r
             JOIN provenance_events pe ON pe.event_id = r.event_id
             WHERE r.source = 'user_confirmed' AND r.event_id IS NOT NULL
               AND pe.conversation_id = ?1",
            1,
        );
        assert!(!confirmed.contains("SCAN pe"), "{confirmed}");
        assert!(!confirmed.contains("chunks"), "{confirmed}");
        let verdicts = plan(
            &format!(
                "SELECT v.witness_id FROM witness_verdicts v
                 INDEXED BY idx_witness_verdicts_recap_created
                 JOIN witness_ledger wl ON wl.id = v.witness_id
                 WHERE julianday(v.created_at) BETWEEN julianday(?1) AND julianday(?2)
                   AND wl.project = ?3
                   AND v.verdict IN ('superseded_by', 'anchor_obsolete')
                   AND v.id > {LIVE_EVENT_FLOOR_SQL}"
            ),
            3,
        );
        assert!(
            verdicts.contains("idx_witness_verdicts_recap_created"),
            "{verdicts}"
        );
        assert!(!verdicts.contains("SCAN v"), "{verdicts}");
        assert!(!verdicts.contains("SCAN wl"), "{verdicts}");
    }
}
