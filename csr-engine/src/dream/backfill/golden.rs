//! Golden fixture integration test (item D of the ox3 audit response).
//!
//! Loads the synthetic `acme-telemetry` corpus documented in
//! `ox3_sample_dataset.sql` (inlined below as a Rust constant — this module
//! never reads the file at test time) directly into `episode_index` /
//! `witness_ledger` / `witness_verdicts`, then runs the deterministic
//! (Stage 1, Stage 2-3) and mocked-LLM (Stage 4-5) halves of the pipeline
//! against it with an INJECTED clock (`now = 2026-08-25T00:00:00Z`, P5),
//! asserting the T5 golden-expectations table from the audit reply:
//!
//! - **STALE, corrected by A-b**: this table originally promised exactly 3
//!   `dream_relations` rows (R1 a relapse, R2/R3 ledger pairs). A-b (the
//!   backfill-robust death-time fix) made the fixture's planted relapse
//!   (R1, `ep-001` -> `ep-003`) correctly stop generating: its governing
//!   witness's `at_oid`/`receipt_oid` are synthetic hex strings that
//!   resolve against no real commit, so its only available death-time tier
//!   is `Provenance::CreatedAtFallback`, which A-b makes UNORDERABLE by
//!   construction — R1 is a valid planted relapse deliberately discarded
//!   for insufficient provenance, not a bug. The correct count from THIS
//!   fixture is now exactly 2 (R2, R3 — both ledger pairs), asserted in
//!   [`stage2_3_gate_project_matches_the_golden_relations`] /
//!   [`stage4_5_adjudication_promotes_the_surviving_golden_relations`].
//!   [`real_git_verified_relapse_recovers_end_to_end`] is a SEPARATE, new
//!   fixture (F5 fix, Codex review pass 1, finding #7) — a real tempfile
//!   git repo with RESOLVING oids — restoring the golden contract's proof
//!   that a relapse CAN be recovered end-to-end, not just correctly
//!   suppressed when its provenance is too weak.
//! - exactly 1 `backfill_unfinished_receipts` row (ep-004), with ep-006
//!   picked up by the (P1-fixed) prev-episode chain instead.
//! - the five quote/Jaccard test vectors from the fixture's own comment
//!   block, run through the P7 matcher.
//!
//! Stage 0 (`materialize_episode_index`/`fill_aliveness`) is deliberately
//! NOT run here — the fixture rows are inserted directly into
//! `episode_index` with `present_at_head`/`days_since_last_touch`
//! pre-populated (matching the audit's own T1 trace: "Stage 0 ... is out of
//! scope for the golden run").

use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::narrative::ParsedNarrative;
use crate::storage::Storage;

use super::adjudicate::{load_episode, run_adjudication_with, AdjudicateAttempt};
use super::pairs::Relation;
use super::rank::{gate_project, persist_relations};
use super::unfinished::{scan_unfinished_at, SeedDisposition};
use super::verify::{quote_verified, verify_and_apply, OidCache, VerifyOutcome};

/// Verbatim from `ox3_sample_dataset.sql` (episode_index / witness_ledger /
/// witness_verdicts sections only — the file's own comment blocks and the
/// quote-vector documentation are reproduced in this module's doc comments
/// instead of parsed at test time).
const FIXTURE_SQL: &str = r#"
INSERT INTO episode_index VALUES ('ep-001','s-01','acme-telemetry','2026-02-10T14:00:00Z','completed',
 'Implement frame parsing for the telemetry stream using a callback-based parser',
 'Wrote parse_frame() callback parser in src/parser.rs; handles partial frames via re-entrant callbacks and a ring buffer',
 NULL, NULL, 0, '["src/parser.rs"]',
 '[{"file":"src/parser.rs","node_kind":"function","name":"parse_frame","body_hash":"aaaa111122223333"}]',
 NULL, 1, 68, datetime('now'));

INSERT INTO episode_index VALUES ('ep-002','s-02','acme-telemetry','2026-03-05T09:30:00Z','completed',
 'Parser drops frames under sustained load; redesign it',
 'Replaced the callback parser with an iterator-based FrameIter; parse_frame callback design retired because re-entrancy caused frame drops under load',
 NULL, NULL, 0, '["src/parser.rs"]',
 '[{"file":"src/parser.rs","node_kind":"function","name":"frame_iter","body_hash":"bbbb444455556666"}]',
 NULL, 1, 68, datetime('now'));

INSERT INTO episode_index VALUES ('ep-003','s-05','acme-telemetry','2026-06-18T20:15:00Z','completed',
 'Add reconnect logic for dropped telemetry links',
 'Added exponential reconnect in src/link.rs; also patched parse_frame in src/parser.rs to handle the new heartbeat frame via a callback shim',
 NULL, NULL, 0, '["src/link.rs","src/parser.rs"]',
 '[{"file":"src/link.rs","node_kind":"function","name":"reconnect","body_hash":"eeee777788889999"},{"file":"src/parser.rs","node_kind":"function","name":"parse_frame","body_hash":"cccc000011112222"}]',
 NULL, 1, 68, datetime('now'));

INSERT INTO episode_index VALUES ('ep-004','s-03','acme-telemetry','2026-03-20T11:00:00Z','partial',
 'Wire the retry budget into the uploader',
 'Started retry budget plumbing in src/uploader.rs; upload_batch takes a budget param now',
 'finish retry budget wiring in src/uploader.rs; add backoff cap and jitter',
 'flaky auth token refresh blocks the integration test',
 2, '["src/uploader.rs"]',
 '[{"file":"src/uploader.rs","node_kind":"function","name":"upload_batch","body_hash":"dddd333344445555"}]',
 NULL, 1, 150, datetime('now'));

INSERT INTO episode_index VALUES ('ep-000','s-08','acme-telemetry','2026-07-30T10:00:00Z','completed',
 'Add JSON config loading',
 'Wrote load_config() in src/config.rs reading config.json with serde_json',
 NULL, NULL, 0, '["src/config.rs"]',
 '[{"file":"src/config.rs","node_kind":"function","name":"load_config","body_hash":"1111aaaa2222bbbb"}]',
 NULL, 1, 5, datetime('now'));
INSERT INTO episode_index VALUES ('ep-005','s-09','acme-telemetry','2026-08-20T16:45:00Z','completed',
 'Swap JSON config to TOML',
 'Rewrote src/config.rs: load_config now parses config.toml; JSON path deleted',
 NULL, NULL, 0, '["src/config.rs"]',
 '[{"file":"src/config.rs","node_kind":"function","name":"load_config","body_hash":"3333cccc4444dddd"}]',
 NULL, 1, 5, datetime('now'));

INSERT INTO episode_index VALUES ('ep-006','s-04','acme-telemetry','2026-04-02T13:00:00Z','partial',
 'Migrate the ops dashboards to the new metrics schema',
 'Mapped half the dashboard panels to the new schema',
 'migrate the remaining dashboards', NULL,
 1, '["dash/panels.yaml"]', '[]', NULL, 1, 40, datetime('now'));
INSERT INTO episode_index VALUES ('ep-007','s-04b','acme-telemetry','2026-04-03T09:00:00Z','completed',
 'Actually, first fix the alert routing bug',
 'Fixed alert routing dedup in src/alerts.rs',
 NULL, NULL, 0, '["src/alerts.rs"]',
 '[{"file":"src/alerts.rs","node_kind":"function","name":"route_alert","body_hash":"5555eeee6666ffff"}]',
 'ep-006', 1, 40, datetime('now'));

INSERT INTO episode_index VALUES ('ep-008','s-06','acme-telemetry','2026-05-01T10:00:00Z','completed',
 'Add p99 latency gauge',
 'Added p99_gauge() to src/metrics.rs',
 NULL, NULL, 0, '["src/metrics.rs"]',
 '[{"file":"src/metrics.rs","node_kind":"function","name":"p99_gauge","body_hash":"7777000088881111"}]',
 NULL, 1, 20, datetime('now'));
INSERT INTO episode_index VALUES ('ep-009','s-07','acme-telemetry','2026-05-22T15:00:00Z','completed',
 'Count dropped packets per link',
 'Added drop_counter() to src/metrics.rs',
 NULL, NULL, 0, '["src/metrics.rs"]',
 '[{"file":"src/metrics.rs","node_kind":"function","name":"drop_counter","body_hash":"9999222200003333"}]',
 NULL, 1, 20, datetime('now'));

INSERT INTO witness_ledger VALUES (1,'acme-telemetry','src/parser.rs','parse_frame',10,80,
 'b3:0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9','committed',
 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1','conversation','s-01','2026-02-10T14:05:00Z');
INSERT INTO witness_ledger VALUES (2,'acme-telemetry','src/parser.rs','frame_iter',10,120,
 'b3:1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a','committed',
 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2','conversation','s-02','2026-03-05T09:35:00Z');
INSERT INTO witness_ledger VALUES (3,'acme-telemetry','src/config.rs','load_config',5,40,
 'b3:2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b','committed',
 'c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3','conversation','s-08','2026-07-30T10:05:00Z');
INSERT INTO witness_ledger VALUES (4,'acme-telemetry','src/config.rs','load_config',5,55,
 'b3:3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c','committed',
 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4','conversation','s-09','2026-08-20T16:50:00Z');

INSERT INTO witness_verdicts VALUES (1,1,'superseded_by',2,
 'feedfacefeedfacefeedfacefeedfacefeedface',
 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2','2026-03-05T09:40:00Z');
INSERT INTO witness_verdicts VALUES (2,3,'superseded_by',4,
 'cafebabecafebabecafebabecafebabecafebabe',
 'd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4','2026-08-20T16:55:00Z');
"#;

const PROJECT: &str = "acme-telemetry";

fn seeded() -> Storage {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            conn.execute_batch(FIXTURE_SQL)?;
            Ok(())
        })
        .unwrap();
    storage
}

/// The fixture's pinned reference date (its own header comment: "Reference
/// date (\"today\") = 2026-08-25").
fn golden_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-25T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

// -----------------------------------------------------------------------
// Stage 1 (unfinished scan): ep-006 chain-picked (P1), ep-004 the sole
// negative receipt with the documented confusion matrix and Queue U
// priority (P6/P10 make this deterministic and reproducible).
// -----------------------------------------------------------------------

#[test]
fn stage1_unfinished_scan_matches_the_golden_dispositions() {
    let _g = crate::daemon::dream_cadence::env_test_guard();
    let storage = seeded();
    let report = storage
        .with_connection(|conn| scan_unfinished_at(conn, golden_now()))
        .unwrap();

    assert_eq!(
        report.stats.seeds_considered, 2,
        "exactly ep-004 and ep-006 match the seed predicate"
    );
    assert_eq!(
        report.stats.picked_up_by_chain, 1,
        "P1: the episode-id-shaped prev_episode_id must close ep-006's chain"
    );
    assert_eq!(report.stats.never_picked_up, 1);

    let by_id: std::collections::HashMap<&str, SeedDisposition> = report
        .dispositions
        .iter()
        .map(|d| (d.episode_id(), d.clone()))
        .collect();

    assert!(
        matches!(by_id["ep-006"], SeedDisposition::PickedUpByChain { .. }),
        "ep-006 must never surface as a negative receipt (DISTRACTOR B)"
    );

    let SeedDisposition::NeverPickedUp {
        receipt,
        queue_eligible,
    } = &by_id["ep-004"]
    else {
        panic!("ep-004 must be NeverPickedUp");
    };
    let queue_eligible = *queue_eligible;
    assert!(queue_eligible, "open todos + present_at_head=true");
    assert_eq!(receipt.corpus_scanned, 7);
    assert_eq!(receipt.max_score, 0.0);
    assert_eq!(receipt.argmax_episode.as_deref(), Some("ep-006"));
    // P6: only one matched negative in this tiny corpus -- tau must fall
    // back rather than fit a degenerate threshold from it.
    assert_eq!(report.tau_fit.negative_count, 1);
    assert_eq!(receipt.confusion_matrix.true_positive, 0);
    assert_eq!(receipt.confusion_matrix.false_negative, 1);
    assert_eq!(receipt.confusion_matrix.false_positive, 0);
    assert_eq!(receipt.confusion_matrix.true_negative, 1);

    // Exactly one persisted receipt, and it is NOT ep-006's.
    let receipt_ids: Vec<String> = storage
        .with_connection(|conn| {
            let mut stmt =
                conn.prepare("SELECT seed_episode_id FROM backfill_unfinished_receipts")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(receipt_ids, vec!["ep-004".to_string()]);

    // Queue U = [ep-004], priority = 0.35*0.6 + 0.20*0.4 + 0.25*1.0 + 0.20*1.0 = 0.74
    // (severity(partial)=0.6, todos 2/5=0.4, age>90d bonus fires, alive).
    assert_eq!(report.queue.len(), 1);
    assert_eq!(report.queue[0].episode_id, "ep-004");
    assert!(
        (report.queue[0].priority - 0.74).abs() < 1e-9,
        "priority={}",
        report.queue[0].priority
    );
}

// -----------------------------------------------------------------------
// Stage 2-3 (gate_project): exactly 3 relations, correctly separated by P2,
// correctly ordered by P4's base-vs-final floor distinction.
// -----------------------------------------------------------------------

#[test]
fn stage2_3_gate_project_matches_the_golden_relations() {
    let (relations, stats) = {
        let storage = seeded();
        storage
            .with_connection(|conn| {
                let (relations, stats) = gate_project(conn, PROJECT, golden_now())?;
                persist_relations(conn, &relations)?;
                Ok((relations, stats))
            })
            .unwrap()
    };

    assert_eq!(
        stats.generated_ledger, 2,
        "P2 splits the parse_frame ledger pair from the relapse pair -- no more F3 duplicate"
    );
    // A-b (dream-backfill pass 1): R1 (ep-001 -> ep-003) was this fixture's
    // relapse plant, historically surfaced ONLY via `OidProvenance::
    // CreatedAtFallback` (its `at_oid`/`receipt_oid` are synthetic hex
    // strings that resolve against no real commit) -- see the now-removed
    // assertion this replaced. A-b makes `CreatedAtFallback` unorderable by
    // construction (`death_time::DeathTime::orderable`), so this generator
    // correctly no longer manufactures a relapse pair from it. This is the
    // fixture demonstrating the exact defect A-b exists to close, not a
    // regression: 0 is the CORRECT count now.
    assert_eq!(stats.generated_relapse, 0);
    assert_eq!(
        stats.generated_era, 0,
        "no embeddings -> agglomerative_cluster never merges"
    );
    assert_eq!(
        stats.archived, 0,
        "every candidate here carries a live_file now-hook"
    );
    assert_eq!(
        stats.below_floor, 0,
        "P4: the floor checks pre-penalty base, DISTRACTOR A clears it"
    );
    assert_eq!(stats.queued, 2);

    let queued: Vec<_> = relations.iter().filter(|r| r.status == "queued").collect();
    assert_eq!(queued.len(), 2);
    assert!(
        !queued.iter().any(
            |r| ["ep-006", "ep-007", "ep-008", "ep-009"].contains(&r.ep_a.as_str())
                || ["ep-006", "ep-007", "ep-008", "ep-009"].contains(&r.ep_b.as_str())
        ),
        "DISTRACTOR B/C episodes must never appear in a gated relation"
    );

    let r2 = queued
        .iter()
        .find(|r| r.ep_a == "ep-001" && r.ep_b == "ep-002")
        .expect("R2 ledger pair must exist (P2 fix)");
    let r3 = queued
        .iter()
        .find(|r| r.ep_a == "ep-000" && r.ep_b == "ep-005")
        .expect("R3 ledger pair must exist (P2/P4 fix -- DISTRACTOR A)");

    assert_eq!(r2.generator, super::pairs::Generator::Ledger);
    assert_eq!(r2.relation, Relation::ReplacedBy);
    assert_eq!(r2.topic_key, "symbol:parse_frame");
    assert_eq!(
        r2.load_bearing_oid.as_deref(),
        Some("feedfacefeedfacefeedfacefeedfacefeedface")
    );

    assert_eq!(r3.generator, super::pairs::Generator::Ledger);
    assert_eq!(r3.relation, Relation::ReplacedBy);
    assert_eq!(r3.topic_key, "symbol:load_config");
    assert_eq!(
        r3.load_bearing_oid.as_deref(),
        Some("cafebabecafebabecafebabecafebabecafebabe")
    );

    // T5 ordering: R2 > R3 > 0 (R1 no longer generates -- see above), and
    // R3's evidence floor is on `base` (which P4 keeps out of
    // `GatedRelation`, but the surviving-at-all fact together with the
    // score ordering below is exactly what the floor fix makes possible --
    // pre-P4, R3's post-penalty score alone would have been floored out
    // entirely).
    assert!(
        r2.gate_score > r3.gate_score,
        "DISTRACTOR A must rank below the surviving ledger plant"
    );
    assert!(
        r3.gate_score > 0.0,
        "DISTRACTOR A must still exist, merely demoted"
    );
}

// -----------------------------------------------------------------------
// Stage 4-5 (adjudicate + verify): mocked adjudicator decides the two
// surviving ledger candidates (R1's relapse plant no longer generates as
// of A-b — see the comments above), quotes verify against the real
// episode text, tier promotes to 'witnessed'.
// -----------------------------------------------------------------------

fn mock_adjudicator() -> impl Fn(Option<&str>, &str) -> AdjudicateAttempt {
    |_model, prompt: &str| {
        // Each pair's rendered "B (later)" block contains distinguishing
        // verbatim text unique to that episode -- used only to pick which
        // pre-known, verbatim-real quotes to return, never anything the
        // pipeline itself would treat as a hypothesis label.
        // (quote_a, quote_b, extended, same_approach): the relapse pair is
        // an honest judge's "B returns to A's approach" (same_approach,
        // NOT incompatible); the two ledger pairs are displacements
        // (incompatible). Mirrors what the unprimed prompt now asks.
        #[allow(clippy::type_complexity)]
        let (quote_a, quote_b, extended, same_approach): (&str, &str, bool, bool) = if prompt
            .contains("heartbeat frame")
        {
            // R1: relapse (ep-001 -> ep-003).
            (
                "Wrote parse_frame() callback parser in src/parser.rs",
                "also patched parse_frame in src/parser.rs to handle the new heartbeat frame via a callback shim",
                false,
                true,
            )
        } else if prompt.contains("FrameIter") {
            // R2: ledger (ep-001 -> ep-002).
            (
                "Wrote parse_frame() callback parser in src/parser.rs",
                "Replaced the callback parser with an iterator-based FrameIter",
                false,
                false,
            )
        } else {
            // R3: ledger (ep-000 -> ep-005).
            (
                "Wrote load_config() in src/config.rs reading config.json with serde_json",
                "Rewrote src/config.rs: load_config now parses config.toml",
                false,
                false,
            )
        };
        let body = serde_json::json!({
            "quote_a_attests_a": true,
            "quote_b_attests_b": true,
            "incompatible": !same_approach,
            "same_approach": same_approach,
            "extended": extended,
            "quote_a": quote_a,
            "quote_b": quote_b,
            "oids": [],
        })
        .to_string();
        AdjudicateAttempt::Parsed(ParsedNarrative {
            text: body,
            model: "mock".to_string(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    }
}

#[test]
fn stage4_5_adjudication_promotes_the_surviving_golden_relations() {
    let _g = crate::daemon::dream_cadence::env_test_guard();
    let storage = seeded();
    storage
        .with_connection(|conn| {
            let (relations, _) = gate_project(conn, PROJECT, golden_now())?;
            persist_relations(conn, &relations)?;
            Ok(())
        })
        .unwrap();

    let queue_before = storage
        .with_connection(super::adjudicate::queue_depth)
        .unwrap();
    // A-b (dream-backfill pass 1): R1 (ep-001 -> ep-003) no longer
    // generates as a candidate at all -- its `at_oid`/`receipt_oid` are
    // synthetic, non-git-resolvable hex strings, so its death time can only
    // resolve to `OidProvenance::CreatedAtFallback`, which A-b makes
    // unorderable and therefore un-generatable for this generator. See the
    // matching comment in `stage2_3_gate_project_matches_the_golden_relations`.
    assert_eq!(queue_before, 2);

    let actor = mock_adjudicator();
    let stats = run_adjudication_with(&actor, &storage, 10).unwrap();

    // Round-5: both surviving golden candidates are ledger pairs — anchor-
    // evidenced, promoted deterministically on machine receipts (ts order +
    // OID re-resolution), zero LLM calls. The mock actor is never invoked.
    assert_eq!(stats.deterministic_promoted, 2);
    assert_eq!(stats.deterministic_discarded, 0);
    assert_eq!(stats.attempted, 0, "no era candidates -> no LLM spend");
    assert_eq!(stats.related, 0);
    assert_eq!(stats.unrelated, 0);
    assert_eq!(stats.verify_failed, 0);
    assert_eq!(stats.backlog_after, 0);
    // Deliberately NOT asserted: `adjudicator_suspect` -- a small run
    // trivially sits outside the 10-40% UNRELATED band by construction; the
    // canary is documented as WARN-only and run-completion is what matters.

    let witnessed_count: i64 = storage
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM dream_relations WHERE project = ?1 AND tier = 'witnessed'",
                params![PROJECT],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(witnessed_count, 2);

    let no_r6789: i64 = storage
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM dream_relations
                 WHERE ep_a IN ('ep-006','ep-007','ep-008','ep-009')
                    OR ep_b IN ('ep-006','ep-007','ep-008','ep-009')",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(no_r6789, 0);
}

// -----------------------------------------------------------------------
// F5 fix (Codex review pass 1, finding #7): the golden fixture above (R1)
// only proves SUPPRESSION -- its synthetic, non-resolving OIDs make
// `CreatedAtFallback` (correctly) unorderable, so zero relapse candidates
// is the right answer for THAT fixture, but it means the golden contract
// no longer has a single case that proves a relapse can be RECOVERED
// end-to-end. This is a NEW, independent fixture — a real tempfile git
// repo with RESOLVING oids — that restores that teeth: a symbol whose span
// survives a line inserted ABOVE it (the exact false-death shape F1
// closes) but genuinely dies at a later, real commit, carried all the way
// through gate_project -> adjudicate -> verify to an actually-PROMOTED,
// non-empty-quoted `dream_relations` row.
// -----------------------------------------------------------------------

#[test]
fn real_git_verified_relapse_recovers_end_to_end() {
    use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow};
    use crate::storage::witness_verdicts::{
        insert_verdict_if_changed, VerdictKind, WitnessVerdictRow,
    };

    let _g = crate::daemon::dream_cadence::env_test_guard();
    let strip = |cmd: &mut std::process::Command| {
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
    };
    let run = |repo: &std::path::Path, args: &[&str]| -> bool {
        let mut cmd = std::process::Command::new("git");
        strip(&mut cmd);
        cmd.arg("-C").arg(repo).args(args);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    };
    let head_oid = |repo: &std::path::Path| -> String {
        let mut cmd = std::process::Command::new("git");
        strip(&mut cmd);
        cmd.arg("-C").arg(repo).arg("rev-parse").arg("HEAD");
        String::from_utf8(cmd.output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string()
    };

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let mut init = std::process::Command::new("git");
    strip(&mut init);
    init.arg("init").arg("-q").arg(&repo);
    if !init.status().map(|s| s.success()).unwrap_or(false) {
        return; // git unavailable in this environment -- fail-soft skip
    }
    let commit = |repo: &std::path::Path, msg: &str| {
        assert!(run(repo, &["add", "-A"]));
        assert!(run(
            repo,
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                msg,
            ]
        ));
    };

    let file = repo.join("a.rs");
    // c0: foo's origin.
    std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
    commit(&repo, "c0");
    let c0 = head_oid(&repo);
    let stamp = codewitness::StampKind::Raw
        .compute(&std::fs::read(&file).unwrap())
        .as_str()
        .to_string();
    // c1: a line inserted ABOVE foo -- foo's own body is byte-identical.
    // Under the fixed-line-span re-hash F1 replaces, this shifts foo's
    // recorded coordinates and reads as a FALSE death here.
    std::fs::write(&file, "// unrelated\nfn bar() {}\n\nfn foo() {\n    1\n}\n").unwrap();
    commit(&repo, "c1");
    // c2: foo genuinely dies here.
    std::fs::write(&file, "// unrelated\nfn bar() {}\n\nfn foo() {\n    2\n}\n").unwrap();
    commit(&repo, "c2");
    let c2 = head_oid(&repo);
    let file_str = file.to_string_lossy().to_string();

    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            // Real spans, matching foo's ORIGINAL 0-based coordinates at
            // c0 -- exactly the stale coordinates a fixed-span re-hash
            // would keep re-slicing at every later commit (Codex review
            // pass 1, finding #1's own complaint about the old test suite
            // dodging this with `span = None`).
            insert_witness(
                conn,
                &WitnessLedgerRow {
                    id: 0,
                    project: "p".to_string(),
                    file: file_str.clone(),
                    symbol: Some("foo".to_string()),
                    span_start: Some(0),
                    span_end: Some(2),
                    stamp: stamp.clone(),
                    tier: "committed".to_string(),
                    at_oid: Some(c0.clone()),
                    source_kind: "backfill".to_string(),
                    source_id: None,
                },
            )?;
            let wid: i64 = conn.query_row(
                "SELECT id FROM witness_ledger WHERE file = ?1 AND symbol = 'foo'",
                params![file_str],
                |r| r.get(0),
            )?;
            insert_verdict_if_changed(
                conn,
                &WitnessVerdictRow {
                    witness_id: wid,
                    verdict: VerdictKind::AnchorObsolete,
                    successor_witness_id: None,
                    receipt_oid: None,
                    observed_head_oid: c2.clone(),
                },
            )?;

            conn.execute(
                "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-before', ?1, '[]', '2020-01-01T00:00:00Z')",
                params![format!(
                    r#"{{"schema":"v2","session_id":"s1","project":"p","timestamp":"2020-01-01T00:00:00Z","request":"work on foo the old way","completed":"used foo the old way","outcome":"completed","todos":[],"files_modified":[],"anchors":[{{"file":"{file_str}","node_kind":"function","name":"foo","body_hash":"h1"}}]}}"#
                )],
            )?;
            conn.execute(
                "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-relapse', ?1, '[]', '2099-01-01T00:00:00Z')",
                params![format!(
                    r#"{{"schema":"v2","session_id":"s2","project":"p","timestamp":"2099-01-01T00:00:00Z","request":"touch foo again","completed":"re-touched foo, unaware it was retired","outcome":"partial","todos":[{{"content":"t","status":"pending"}}],"files_modified":[],"anchors":[{{"file":"{file_str}","node_kind":"function","name":"foo","body_hash":"h2"}}]}}"#
                )],
            )?;
            crate::storage::dream_backfill::materialize_episode_index(conn)
        })
        .unwrap();

    let now = crate::temporal::parse_timestamp("2100-01-01T00:00:00Z").unwrap();
    storage
        .with_connection(|conn| {
            let (relations, stats) = gate_project(conn, "p", now)?;
            assert_eq!(
                stats.generated_relapse, 1,
                "the line inserted above foo at c1 must not suppress the real relapse candidate"
            );
            persist_relations(conn, &relations)
        })
        .unwrap();

    let actor = mock_adjudicator();
    let stats = run_adjudication_with(&actor, &storage, 10).unwrap();
    assert_eq!(
        stats.deterministic_promoted, 1,
        "a git-verified relapse promotes deterministically, zero LLM spend"
    );
    assert_eq!(stats.deterministic_discarded, 0);

    let (tier, generator, quote_a, quote_b): (String, String, String, String) = storage
        .with_connection(|conn| {
            conn.query_row(
                "SELECT tier, generator, quote_a, quote_b FROM dream_relations WHERE project = 'p'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        tier, "witnessed",
        "the golden contract proves recovery again, not just suppression"
    );
    assert_eq!(generator, "relapse");
    assert!(
        !quote_a.is_empty(),
        "F2: the promoted quote must be non-empty"
    );
    assert!(
        !quote_b.is_empty(),
        "F2: the promoted quote must be non-empty"
    );
}

// -----------------------------------------------------------------------
// Quote / Jaccard test vectors (fixture's own "QUOTE / JACCARD TEST
// VECTORS" comment block) run through the P7 matcher.
// -----------------------------------------------------------------------

#[test]
fn quote_vectors_match_the_golden_expectations() {
    let storage = seeded();

    // V-PASS-1 (verbatim, long): quote_a against ep-002.completed -> KEEP.
    let ep002 = storage
        .with_connection(|conn| load_episode(conn, "ep-002"))
        .unwrap()
        .unwrap();
    assert!(quote_verified(
        "Replaced the callback parser with an iterator-based FrameIter",
        &super::adjudicate::episode_record_text(&ep002),
        &[],
    ));

    // V-PASS-2 (short quote, insertion noise): quote_b against ep-003 ->
    // KEEP under the P7 expandable-window matcher (a fixed-length windowed
    // Jaccard false-discards this one, per T4/F7).
    let ep003 = storage
        .with_connection(|conn| load_episode(conn, "ep-003"))
        .unwrap()
        .unwrap();
    assert!(quote_verified(
        "patched parse_frame to handle the new heartbeat frame",
        &super::adjudicate::episode_record_text(&ep003),
        &[],
    ));

    // V-FAIL-1 (paraphrase, no exact tokens shared): quote_a against
    // ep-001.completed -> DISCARD, reason fabricated_quote_a. Full
    // verify_and_apply path so the discard reason itself is asserted.
    run_scripted_discard(
        &storage,
        "ep-001",
        "ep-004",
        "extended_by",
        "implemented a callback-driven frame parsing approach",
        "fabricated_quote_a",
    );

    // V-FAIL-2 (empty quote) -> DISCARD, reason empty_quote_a.
    run_scripted_discard(
        &storage,
        "ep-002",
        "ep-003",
        "replaced_by",
        "",
        "empty_quote_a",
    );

    // V-FAIL-3 (right words, wrong episode): "Rewrote src/config.rs"
    // presented as evidence on the (ep-001, ep-003) pair, but the words
    // actually live in ep-005's record -> DISCARD, reason
    // misattributed_quote_a. Uses a different placeholder `relation` value
    // than R1's already-promoted 'extended_by' so the seed insert itself
    // doesn't collide with the UNIQUE index.
    run_scripted_discard(
        &storage,
        "ep-001",
        "ep-003",
        "replaced_by",
        "Rewrote src/config.rs",
        "misattributed_quote_a",
    );
}

/// One "scripted vector run" (T5's own phrase): seed a fresh `dream_relations`
/// row for `(ep_a, ep_b)` under `PROJECT`, then call `verify_and_apply` with
/// `quote_a` as the only interesting field of the verdict (a trivially valid
/// quote_b, so quote_a's own reason is what fails first), asserting the
/// resulting discard reason.
fn run_scripted_discard(
    storage: &Storage,
    ep_a: &str,
    ep_b: &str,
    seed_relation: &str,
    quote_a: &str,
    expected_reason: &'static str,
) {
    let id = storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO dream_relations
                    (project, ep_a, ep_b, relation, generator, topic_key, tier, status)
                 VALUES (?1, ?2, ?3, ?4, 'ledger', 'symbol:scripted', 'unverified', 'queued')",
                params![PROJECT, ep_a, ep_b, seed_relation],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .unwrap();

    let candidate = super::adjudicate::QueuedRelation {
        id,
        project: PROJECT.to_string(),
        ep_a: ep_a.to_string(),
        ep_b: ep_b.to_string(),
        topic_key: "symbol:scripted".to_string(),
        generator: "ledger".to_string(),
        relation: "replaced_by".to_string(),
        load_bearing_oid: None,
        oid_provenance: "created_at_fallback".to_string(),
    };
    let a = storage
        .with_connection(|conn| load_episode(conn, ep_a))
        .unwrap()
        .unwrap();
    let b = storage
        .with_connection(|conn| load_episode(conn, ep_b))
        .unwrap()
        .unwrap();

    let outcome = storage
        .with_connection(|conn| {
            let mut cache = OidCache::new();
            let verdict = super::adjudicate::RawVerdict {
                quote_a_attests_a: true,
                quote_b_attests_b: true,
                incompatible: true,
                same_approach: false,
                extended: false,
                quote_a: quote_a.to_string(),
                quote_b: "we now fail fast on timeout errors".to_string(),
                oids: vec![],
            };
            verify_and_apply(
                conn,
                &candidate,
                &a,
                &[],
                &b,
                &[],
                Relation::ReplacedBy,
                &verdict,
                &mut cache,
                "{}",
            )
        })
        .unwrap();
    assert!(
        matches!(outcome, VerifyOutcome::Failed(r) if r == expected_reason),
        "expected {expected_reason}, got a different outcome for ({ep_a}, {ep_b})"
    );

    let discard_count: i64 = storage
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM backfill_discards
                 WHERE reason = ?1 AND pair_key = ?2",
                params![expected_reason, format!("{PROJECT}:{ep_a}:{ep_b}")],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(discard_count, 1);
}
