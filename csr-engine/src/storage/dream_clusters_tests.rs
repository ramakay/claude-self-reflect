use super::*;
use crate::storage::dream_items::load_dream_items;
use crate::storage::witness_ledger::{self, WitnessLedgerRow};
use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

fn open() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::storage::migrations::run(&conn).unwrap();
    conn
}

fn insert_episode(conn: &Connection, id: &str, json: &str, timestamp: &str) {
    conn.execute(
        "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
        params![id, json, timestamp],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_witness_with_verdict(
    conn: &Connection,
    project: &str,
    file: &str,
    symbol: Option<&str>,
    stamp: &str,
    verdict: VerdictKind,
    receipt_oid: Option<&str>,
    witnessed_at: &str,
) {
    witness_ledger::insert_witness(
        conn,
        &WitnessLedgerRow {
            id: 0,
            project: project.into(),
            file: file.into(),
            symbol: symbol.map(|s| s.to_string()),
            span_start: Some(1),
            span_end: Some(3),
            stamp: stamp.into(),
            tier: "committed".into(),
            at_oid: Some("at-oid".into()),
            source_kind: "backfill".into(),
            source_id: Some(stamp.into()),
        },
    )
    .unwrap();
    let witness_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE project = ?1 AND file = ?2 AND stamp = ?3",
            params![project, file, stamp],
            |r| r.get(0),
        )
        .unwrap();
    witness_verdicts::insert_verdict_if_changed(
        conn,
        &WitnessVerdictRow {
            witness_id,
            verdict,
            successor_witness_id: None,
            receipt_oid: receipt_oid.map(|s| s.to_string()),
            observed_head_oid: "head".into(),
        },
    )
    .unwrap();
    conn.execute(
        "UPDATE witness_verdicts SET created_at = ?1 WHERE witness_id = ?2",
        params![witnessed_at, witness_id],
    )
    .unwrap();
}

fn insert_generation(
    conn: &Connection,
    project: &str,
    file: &str,
    generation_id: &str,
    head_oid: &str,
    status: &str,
    created_at: &str,
) {
    conn.execute(
        "INSERT INTO witness_generations
                (generation_id, project, file, head_oid, extractor_version, status, created_at)
             VALUES (?1, ?2, ?3, ?4, 'v1', ?5, ?6)",
        params![generation_id, project, file, head_oid, status, created_at],
    )
    .unwrap();
}

fn evidence(
    file: &str,
    symbol: Option<&str>,
    verdict: &str,
    receipt: Option<&str>,
) -> DreamEvidence {
    DreamEvidence {
        symbol: symbol.map(|s| s.to_string()),
        file: file.to_string(),
        verdict: verdict.to_string(),
        receipt_oid: receipt.map(|s| s.to_string()),
        witnessed_at: "2026-01-01 00:00:00".to_string(),
    }
}

fn rank(
    grade: DreamItemGrade,
    class: VerdictClass,
    date: &str,
    kind: ItemKindRank,
    oldest: &str,
    id: &str,
) -> ClusterRank {
    ClusterRank {
        grade,
        verdict_class: class,
        witnessed_date_desc: Reverse(date.to_string()),
        kind,
        oldest_open: oldest.to_string(),
        id: id.to_string(),
    }
}

// ---- fingerprint ---------------------------------------------------------

#[test]
fn fingerprint_groups_identical_evidence_sets_regardless_of_order() {
    let a = vec![
        evidence("src/a.rs", Some("alpha"), "superseded_by", Some("oid1")),
        evidence("src/b.rs", Some("beta"), "anchor_obsolete", Some("oid2")),
    ];
    let b = vec![
        evidence("src/b.rs", Some("beta"), "anchor_obsolete", Some("oid2")),
        evidence("src/a.rs", Some("alpha"), "superseded_by", Some("oid1")),
    ];
    assert_eq!(evidence_fingerprint(&a), evidence_fingerprint(&b));
}

#[test]
fn fingerprint_ignores_witnessed_at_but_separates_real_differences() {
    let base = vec![evidence(
        "src/a.rs",
        Some("alpha"),
        "superseded_by",
        Some("oid1"),
    )];

    let mut later = base.clone();
    later[0].witnessed_at = "2029-12-31 23:59:59".to_string();
    assert_eq!(
        evidence_fingerprint(&base),
        evidence_fingerprint(&later),
        "a MAX(created_at) aggregate must not split an identical evidence set"
    );

    let other_symbol = vec![evidence(
        "src/a.rs",
        Some("gamma"),
        "superseded_by",
        Some("oid1"),
    )];
    let other_receipt = vec![evidence(
        "src/a.rs",
        Some("alpha"),
        "superseded_by",
        Some("oid9"),
    )];
    let other_verdict = vec![evidence(
        "src/a.rs",
        Some("alpha"),
        "anchor_obsolete",
        Some("oid1"),
    )];
    let no_receipt = vec![evidence("src/a.rs", Some("alpha"), "superseded_by", None)];
    for different in [other_symbol, other_receipt, other_verdict, no_receipt] {
        assert_ne!(
            evidence_fingerprint(&base),
            evidence_fingerprint(&different),
            "a different evidence tuple must not collapse into the same cluster"
        );
    }
}

#[test]
fn session_grade_items_sharing_evidence_collapse_into_one_cluster() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"clean up the leftover mess","status":"pending"},{"content":"write the migration note","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/other/checkout/src/storage/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.active.len(), 1, "{:?}", feed.active);
    let cluster = &feed.active[0];
    assert!(!cluster.standalone);
    assert_eq!(cluster.grade, DreamItemGrade::SessionGrade);
    assert_eq!(cluster.counts.items, 2);
    assert_eq!(cluster.counts.todos, 2);
    assert_eq!(cluster.items.len(), 2);
}

#[test]
fn different_evidence_sets_in_one_session_stay_separate_clusters() {
    let conn = open();
    // Same session_id, two episodes with different touched files — the
    // fingerprint, not the session alone, decides.
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"clean up the leftover mess","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-2",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-02T00:00:00Z","todos":[{"content":"revisit the onboarding copy","status":"pending"}],"files_modified":["/repo/src/storage/other.rs"]}"#,
        "2026-01-02T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/other.rs",
        Some("other_fn"),
        "b3:2",
        VerdictKind::SupersededBy,
        Some("oid2"),
        "2026-01-05 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.active.len(), 2, "{:?}", feed.active);
    assert_ne!(feed.active[0].fingerprint, feed.active[1].fingerprint);
}

// ---- item-grade standalone ------------------------------------------------

#[test]
fn item_grade_items_stay_standalone_clusters_of_one() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"},{"content":"fix `other_fn` too","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/queries.rs",
        Some("other_fn"),
        "b3:2",
        VerdictKind::SupersededBy,
        Some("oid2"),
        "2026-01-05 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(
        feed.active.len(),
        2,
        "two item-grade items must not merge even inside one session: {:?}",
        feed.active
    );
    for cluster in &feed.active {
        assert!(cluster.standalone);
        assert_eq!(cluster.grade, DreamItemGrade::ItemGrade);
        assert_eq!(cluster.counts.items, 1);
    }
}

// ---- ranking tiers, each asserted independently ----------------------------

#[test]
fn rank_tier_1_item_grade_before_session_grade() {
    let item = rank(
        DreamItemGrade::ItemGrade,
        VerdictClass::Unreceipted,
        "2020-01-01",
        ItemKindRank::Todo,
        "9999",
        "zzzz",
    );
    let session = rank(
        DreamItemGrade::SessionGrade,
        VerdictClass::ReceiptBearingAdverse,
        "2030-01-01",
        ItemKindRank::Blocker,
        "0000",
        "aaaa",
    );
    assert!(
        item < session,
        "tier 1 must dominate every later tier: {item:?} vs {session:?}"
    );
}

#[test]
fn rank_tier_2_receipt_bearing_before_restorative_before_unreceipted() {
    let mk = |class| {
        rank(
            DreamItemGrade::ItemGrade,
            class,
            "2026-01-01",
            ItemKindRank::Todo,
            "0000",
            "same",
        )
    };
    assert!(mk(VerdictClass::ReceiptBearingAdverse) < mk(VerdictClass::Restorative));
    assert!(mk(VerdictClass::Restorative) < mk(VerdictClass::Unreceipted));
}

#[test]
fn rank_tier_3_newest_witnessed_date_first() {
    let newer = rank(
        DreamItemGrade::ItemGrade,
        VerdictClass::ReceiptBearingAdverse,
        "2026-02-01",
        ItemKindRank::Todo,
        "0000",
        "same",
    );
    let older = rank(
        DreamItemGrade::ItemGrade,
        VerdictClass::ReceiptBearingAdverse,
        "2026-01-01",
        ItemKindRank::Todo,
        "0000",
        "same",
    );
    assert!(newer < older);
}

#[test]
fn rank_tier_4_blocker_before_todo() {
    let blocker = rank(
        DreamItemGrade::ItemGrade,
        VerdictClass::ReceiptBearingAdverse,
        "2026-01-01",
        ItemKindRank::Blocker,
        "0000",
        "same",
    );
    let todo = rank(
        DreamItemGrade::ItemGrade,
        VerdictClass::ReceiptBearingAdverse,
        "2026-01-01",
        ItemKindRank::Todo,
        "0000",
        "same",
    );
    assert!(blocker < todo);
}

#[test]
fn rank_tier_5_oldest_open_item_breaks_the_tie() {
    let older = rank(
        DreamItemGrade::ItemGrade,
        VerdictClass::ReceiptBearingAdverse,
        "2026-01-01",
        ItemKindRank::Todo,
        "2026-01-01 00:00:00",
        "same",
    );
    let newer = rank(
        DreamItemGrade::ItemGrade,
        VerdictClass::ReceiptBearingAdverse,
        "2026-01-01",
        ItemKindRank::Todo,
        "2026-06-01 00:00:00",
        "same",
    );
    assert!(older < newer, "the longest-open item surfaces first");
}

#[test]
fn rank_tier_3_uses_the_date_so_tier_4_is_reachable() {
    // Same day, different seconds: tier 3 must not consume the comparison
    // or blocker-before-todo could never fire in practice.
    let blocker_later_seconds = ClusterRank {
        witnessed_date_desc: Reverse(iso_date("2026-01-01 23:59:59")),
        ..rank(
            DreamItemGrade::ItemGrade,
            VerdictClass::ReceiptBearingAdverse,
            "2026-01-01",
            ItemKindRank::Blocker,
            "0000",
            "same",
        )
    };
    let todo_earlier_seconds = ClusterRank {
        witnessed_date_desc: Reverse(iso_date("2026-01-01 00:00:01")),
        ..rank(
            DreamItemGrade::ItemGrade,
            VerdictClass::ReceiptBearingAdverse,
            "2026-01-01",
            ItemKindRank::Todo,
            "0000",
            "same",
        )
    };
    assert!(blocker_later_seconds < todo_earlier_seconds);
}

#[test]
fn rank_end_to_end_item_grade_cluster_outranks_session_grade_cluster() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-item",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-session",
        r#"{"schema":"v2","session_id":"sess-2","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"clean up the leftover mess","status":"pending"}],"files_modified":["/repo/src/storage/other.rs"]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-02 00:00:00",
    );
    // Newer evidence for the session-grade cluster — tier 1 must still win.
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/other.rs",
        Some("other_fn"),
        "b3:2",
        VerdictKind::SupersededBy,
        Some("oid2"),
        "2026-06-02 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.active.len(), 2, "{:?}", feed.active);
    assert_eq!(feed.active[0].grade, DreamItemGrade::ItemGrade);
    assert_eq!(feed.active[1].grade, DreamItemGrade::SessionGrade);
}

// ---- settled --------------------------------------------------------------

#[test]
fn settled_cluster_carries_the_completing_receipt() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-open",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-done",
        r#"{"schema":"v2","session_id":"sess-9","project":"proj","timestamp":"2026-02-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"completed"}]}"#,
        "2026-02-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert!(
        feed.active.is_empty(),
        "a completed item must leave Active: {:?}",
        feed.active
    );
    assert_eq!(feed.settled.len(), 1, "{:?}", feed.settled);
    let cluster = &feed.settled[0];
    assert_eq!(cluster.partition, ClusterPartition::Settled);
    let receipt = cluster
        .settled
        .as_ref()
        .expect("a settled cluster must carry its completing receipt");
    assert_eq!(receipt.session_id, "sess-9");
    assert_eq!(receipt.completed_date, "2026-02-01");
    assert_eq!(cluster.counts.completed_items, 1);
    assert_eq!(cluster.items[0].completed.as_ref(), Some(receipt));
}

#[test]
fn partially_completed_cluster_stays_active_but_marks_the_completed_item() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-open",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"clean up the leftover mess","status":"pending"},{"content":"write the migration note","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-done",
        r#"{"schema":"v2","session_id":"sess-9","project":"proj","timestamp":"2026-02-01T00:00:00Z","todos":[{"content":"write the migration note","status":"completed"}]}"#,
        "2026-02-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.active.len(), 1, "{:?}", feed.active);
    let cluster = &feed.active[0];
    assert_eq!(cluster.counts.items, 2);
    assert_eq!(cluster.counts.completed_items, 1);
    assert!(
        cluster.settled.is_none(),
        "a cluster with an open item is not settled"
    );
    assert!(cluster.items.iter().any(|i| i.completed.is_some()));
    assert!(cluster.items.iter().any(|i| i.completed.is_none()));
}

#[test]
fn an_older_completion_never_settles_a_reopened_item() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-done-first",
        r#"{"schema":"v2","session_id":"sess-0","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"completed"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-reopened",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-02-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-02-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-03-05 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.settled.len(), 0);
    assert_eq!(feed.active.len(), 1, "{:?}", feed.active);
    assert!(feed.active[0].items[0].completed.is_none());
}

// ---- archive ---------------------------------------------------------------

#[test]
fn evidence_older_than_the_current_pass_is_archived_with_its_own_pass() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );
    insert_generation(
        &conn,
        "proj",
        "src/queries.rs",
        "gen-old",
        "oldhead",
        "complete",
        "2026-01-02 00:00:00",
    );
    insert_generation(
        &conn,
        "proj",
        "src/queries.rs",
        "gen-new",
        "newhead",
        "complete",
        "2026-03-01 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert!(feed.active.is_empty(), "{:?}", feed.active);
    assert_eq!(feed.archive.len(), 1, "{:?}", feed.archive);
    let cluster = &feed.archive[0];
    assert_eq!(cluster.partition, ClusterPartition::Archive);
    let pass = cluster
        .archive_pass
        .as_ref()
        .expect("the pass covering the evidence must be named");
    assert_eq!(pass.generation_id, "gen-old");
    assert_eq!(pass.head_oid, "oldhead");
    assert_eq!(pass.status, "complete");
}

#[test]
fn evidence_at_the_current_pass_stays_active() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-03-05 00:00:00",
    );
    insert_generation(
        &conn,
        "proj",
        "src/queries.rs",
        "gen-new",
        "newhead",
        "complete",
        "2026-03-01 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.active.len(), 1, "{:?}", feed.active);
    assert!(feed.archive.is_empty());
    assert!(feed.active[0].archive_pass.is_none());
}

#[test]
fn an_incomplete_manifest_is_not_a_pass_boundary() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );
    insert_generation(
        &conn,
        "proj",
        "src/queries.rs",
        "gen-failed",
        "head",
        "incomplete",
        "2026-03-01 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(
        feed.active.len(),
        1,
        "a failed publication must not retire a live conclusion: {:?}",
        feed.archive
    );
}

#[test]
fn settled_wins_over_archive() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-open",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-done",
        r#"{"schema":"v2","session_id":"sess-9","project":"proj","timestamp":"2026-02-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"completed"}]}"#,
        "2026-02-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );
    insert_generation(
        &conn,
        "proj",
        "src/queries.rs",
        "gen-new",
        "newhead",
        "complete",
        "2026-06-01 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.settled.len(), 1);
    assert!(feed.archive.is_empty());
    assert!(feed.settled[0].archive_pass.is_none());
}

// ---- project ordering ------------------------------------------------------

fn seed_two_projects(conn: &Connection) {
    for (project, session, file, symbol, stamp) in [
        ("alpha", "sess-a", "/checkout/src/a.rs", "alpha_fn", "b3:a"),
        ("zulu", "sess-z", "/checkout/src/z.rs", "zulu_fn", "b3:z"),
    ] {
        insert_episode(
            conn,
            &format!("ep-{project}"),
            &format!(
                r#"{{"schema":"v2","session_id":"{session}","project":"{project}","timestamp":"2026-01-01T00:00:00Z","todos":[{{"content":"fix `{symbol}` regression","status":"pending"}}]}}"#
            ),
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            conn,
            project,
            file,
            Some(symbol),
            stamp,
            VerdictKind::SupersededBy,
            Some("oid"),
            "2026-01-05 00:00:00",
        );
    }
}

#[test]
fn current_project_sorts_first_and_every_project_is_present() {
    let conn = open();
    seed_two_projects(&conn);

    let default_order = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(default_order.projects, vec!["alpha", "zulu"]);
    assert_eq!(default_order.active[0].project, "alpha");

    let zulu_first = load_dream_clusters(&conn, Some("zulu")).unwrap();
    assert_eq!(zulu_first.projects, vec!["zulu", "alpha"]);
    assert_eq!(zulu_first.active[0].project, "zulu");
    assert_eq!(zulu_first.active[1].project, "alpha");
    assert_eq!(
        zulu_first.active.len(),
        2,
        "current-first must reorder, never hide"
    );
}

#[test]
fn a_current_project_with_no_clusters_is_not_invented() {
    let conn = open();
    seed_two_projects(&conn);
    let feed = load_dream_clusters(&conn, Some("nonexistent")).unwrap();
    assert_eq!(feed.projects, vec!["alpha", "zulu"]);
}

// ---- determinism -------------------------------------------------------------

#[test]
fn order_is_stable_under_shuffled_input() {
    // Same logical corpus, rows inserted in two different orders. Insert
    // order reaches the query through `ORDER BY rowid`, so a comparator
    // that is not a total order would show up here.
    fn seed(conn: &Connection, reversed: bool) {
        let mut episodes: Vec<(&str, String)> = (0..6)
            .map(|i| {
                let id: &str = Box::leak(format!("ep-{i}").into_boxed_str());
                (
                    id,
                    format!(
                        r#"{{"schema":"v2","session_id":"sess-{i}","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{{"content":"fix `sym_{i}` regression","status":"pending"}}]}}"#
                    ),
                )
            })
            .collect();
        if reversed {
            episodes.reverse();
        }
        for (id, json) in &episodes {
            insert_episode(conn, id, json, "2026-01-01T00:00:00Z");
        }
        let mut witnesses: Vec<usize> = (0..6).collect();
        if reversed {
            witnesses.reverse();
        }
        for i in witnesses {
            insert_witness_with_verdict(
                conn,
                "proj",
                &format!("/checkout/src/f{i}.rs"),
                Some(&format!("sym_{i}")),
                &format!("b3:{i}"),
                VerdictKind::SupersededBy,
                Some("oid"),
                "2026-01-05 00:00:00",
            );
        }
    }

    let forward = open();
    seed(&forward, false);
    let reverse = open();
    seed(&reverse, true);

    let a = load_dream_clusters(&forward, None).unwrap();
    let b = load_dream_clusters(&reverse, None).unwrap();
    assert_eq!(a.active.len(), 6, "{:?}", a.active);
    let ids_a: Vec<&str> = a.active.iter().map(|c| c.id.as_str()).collect();
    let ids_b: Vec<&str> = b.active.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids_a, ids_b, "cluster order must not depend on row order");

    // And re-running over the same connection is byte-identical.
    let again = load_dream_clusters(&forward, None).unwrap();
    assert_eq!(a, again);
}

// ---- agreement with the item feed --------------------------------------------

#[test]
fn clusters_open_items_match_load_dream_items() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-item",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-session",
        r#"{"schema":"v2","session_id":"sess-2","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"clean up the leftover mess","status":"pending"}],"files_modified":["/repo/src/storage/other.rs"],"blockers":"waiting on `other_fn` upstream"}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &conn,
        "ep-unqualified",
        r#"{"schema":"v2","session_id":"sess-3","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"revisit the onboarding copy","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/storage/other.rs",
        Some("other_fn"),
        "b3:2",
        VerdictKind::AnchorObsolete,
        Some("oid2"),
        "2026-01-06 00:00:00",
    );

    let items = load_dream_items(&conn).unwrap();
    let mut item_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    item_ids.sort();
    assert!(!item_ids.is_empty(), "fixture must qualify something");

    let feed = load_dream_clusters(&conn, None).unwrap();
    let mut cluster_ids: Vec<String> = feed
        .active
        .iter()
        .chain(feed.settled.iter())
        .chain(feed.archive.iter())
        .flat_map(|c| c.items.iter())
        .filter(|i| i.completed.is_none())
        .map(|i| i.id.clone())
        .collect();
    cluster_ids.sort();

    assert_eq!(
        cluster_ids, item_ids,
        "the cluster feed's open items (and their ids) must be exactly \
             load_dream_items' output — two gates that disagree would ship two \
             different feeds"
    );

    // Grades must agree too, per id.
    for item in &items {
        let cluster = feed
            .active
            .iter()
            .find(|c| c.items.iter().any(|ci| ci.id == item.id))
            .unwrap_or_else(|| panic!("no cluster carries item {}", item.id));
        assert_eq!(cluster.grade, item.grade, "grade drift on {}", item.item);
    }
}

// ---- counts and receipts -------------------------------------------------------

#[test]
fn counts_and_receipts_are_measured_from_stored_rows() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"clean up the leftover mess","status":"pending"}],"files_modified":["/repo/src/a.rs","/repo/src/b.rs"],"blockers":"blocked on the storage rewrite"}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/a.rs",
        Some("alpha_fn"),
        "b3:1",
        VerdictKind::SupersededBy,
        Some("oid1"),
        "2026-01-05 00:00:00",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/b.rs",
        Some("beta_fn"),
        "b3:2",
        VerdictKind::AnchorReinstated,
        None,
        "2026-01-04 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    assert_eq!(feed.active.len(), 1, "{:?}", feed.active);
    let cluster = &feed.active[0];
    assert_eq!(cluster.counts.items, 2);
    assert_eq!(cluster.counts.blockers, 1);
    assert_eq!(cluster.counts.todos, 1);
    assert_eq!(cluster.counts.evidence, 2);
    assert_eq!(cluster.counts.files, 2);
    assert_eq!(cluster.counts.symbols, 2);
    assert_eq!(cluster.counts.receipts, 1, "only one row stores a receipt");
    assert_eq!(cluster.receipts, vec!["oid1".to_string()]);
    assert_eq!(cluster.verdict_class, VerdictClass::ReceiptBearingAdverse);
    assert_eq!(cluster.conclusion.receipt_oid.as_deref(), Some("oid1"));
    assert_eq!(cluster.conclusion.symbol.as_deref(), Some("alpha_fn"));
    assert_eq!(cluster.witnessed_date, "2026-01-05");
    assert_eq!(cluster.rank.kind, ItemKindRank::Blocker);
    assert_eq!(cluster.items[0].kind, "blocker");
}

#[test]
fn a_missing_receipt_drops_the_clause_instead_of_faking_one() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    insert_witness_with_verdict(
        &conn,
        "proj",
        "/checkout/src/queries.rs",
        Some("run_query"),
        "b3:1",
        VerdictKind::AnchorObsolete,
        None,
        "2026-01-05 00:00:00",
    );

    let feed = load_dream_clusters(&conn, None).unwrap();
    let cluster = &feed.active[0];
    assert!(cluster.receipts.is_empty());
    assert_eq!(cluster.counts.receipts, 0);
    assert_eq!(cluster.conclusion.receipt_oid, None);
    assert_eq!(
        cluster.verdict_class,
        VerdictClass::Unreceipted,
        "an adverse verdict with no stored receipt must not rank as receipt-bearing"
    );
}

// ---- empty ------------------------------------------------------------------

#[test]
fn empty_database_returns_empty_vecs_without_error() {
    let conn = open();
    let feed = load_dream_clusters(&conn, None).unwrap();
    assert!(feed.active.is_empty());
    assert!(feed.settled.is_empty());
    assert!(feed.archive.is_empty());
    assert!(feed.projects.is_empty());
    assert_eq!(feed.total_active, 0);
    assert_eq!(feed.total_settled, 0);
    assert_eq!(feed.total_archive, 0);
    assert_eq!(feed, DreamClusterFeed::default());
}

#[test]
fn episodes_with_no_verdict_evidence_produce_no_clusters() {
    let conn = open();
    insert_episode(
        &conn,
        "ep-1",
        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"revisit the onboarding copy","status":"pending"}]}"#,
        "2026-01-01T00:00:00Z",
    );
    let feed = load_dream_clusters(&conn, Some("proj")).unwrap();
    assert!(feed.active.is_empty());
    assert!(feed.projects.is_empty(), "no evidence, no project row");
}
