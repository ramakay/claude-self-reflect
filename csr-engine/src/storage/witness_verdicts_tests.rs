use super::*;
use crate::storage::witness_ledger::{self, WitnessLedgerRow};

fn open() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::storage::migrations::run(&conn).unwrap();
    conn
}

fn ledger_row(at_oid: &str, stamp: &str) -> WitnessLedgerRow {
    WitnessLedgerRow {
        id: 0,
        project: "proj".into(),
        file: "/repo/src/lib.rs".into(),
        symbol: Some("foo".into()),
        span_start: Some(1),
        span_end: Some(3),
        stamp: stamp.into(),
        tier: "committed".into(),
        at_oid: Some(at_oid.into()),
        source_kind: "backfill".into(),
        source_id: Some(at_oid.into()),
    }
}

#[test]
fn no_event_recorded_returns_none() {
    let conn = open();
    assert!(latest_event(&conn, 999).unwrap().is_none());
}

#[test]
fn insert_and_query_round_trip() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let w1 =
        witness_ledger::latest_witness_for_symbol(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .unwrap();
    let wid: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE stamp = 'b3:1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(w1.stamp, "b3:1");

    let row = WitnessVerdictRow {
        witness_id: wid,
        verdict: VerdictKind::AnchorObsolete,
        successor_witness_id: None,
        receipt_oid: Some("headoid".into()),
        observed_head_oid: "headoid".into(),
    };
    let wrote = insert_verdict_if_changed(&conn, &row).unwrap();
    assert!(wrote, "first insert must write");
    let latest = latest_event(&conn, wid).unwrap().unwrap();
    assert_eq!(latest.verdict, VerdictKind::AnchorObsolete);
    assert_eq!(latest.observed_head_oid, "headoid");
}

#[test]
fn identical_event_at_same_head_is_a_no_op() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let wid: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    let row = WitnessVerdictRow {
        witness_id: wid,
        verdict: VerdictKind::AnchorObsolete,
        successor_witness_id: None,
        receipt_oid: Some("headoid".into()),
        observed_head_oid: "headoid".into(),
    };
    assert!(insert_verdict_if_changed(&conn, &row).unwrap());
    assert!(
        !insert_verdict_if_changed(&conn, &row).unwrap(),
        "identical (verdict, successor, observed_head_oid) at the same witness must not re-insert"
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn same_verdict_at_a_newer_head_writes_no_row() {
    // Event identity is the verdict kind only: the same conclusion
    // re-drawn at a newer HEAD, with a different receipt or successor,
    // is not a new event.
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let wid: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    let first = WitnessVerdictRow {
        witness_id: wid,
        verdict: VerdictKind::SupersededBy,
        successor_witness_id: Some(7),
        receipt_oid: Some("head1".into()),
        observed_head_oid: "head1".into(),
    };
    let newer_head = WitnessVerdictRow {
        observed_head_oid: "head2".into(),
        ..first.clone()
    };
    let other_receipt = WitnessVerdictRow {
        receipt_oid: Some("head3".into()),
        ..first.clone()
    };
    let other_successor = WitnessVerdictRow {
        successor_witness_id: Some(8),
        ..first.clone()
    };
    assert!(insert_verdict_if_changed(&conn, &first).unwrap());
    assert!(!is_new_event(
        latest_event(&conn, wid).unwrap().as_ref(),
        &newer_head
    ));
    for candidate in [&newer_head, &other_receipt, &other_successor] {
        assert!(
            !insert_verdict_if_changed(&conn, candidate).unwrap(),
            "same verdict kind must not write: {candidate:?}"
        );
    }
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn b_a_b_at_new_heads_still_writes_three_events() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let wid: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    let at = |verdict: VerdictKind, head: &str| WitnessVerdictRow {
        witness_id: wid,
        verdict,
        successor_witness_id: None,
        receipt_oid: Some(head.into()),
        observed_head_oid: head.into(),
    };
    assert!(insert_verdict_if_changed(&conn, &at(VerdictKind::SupersededBy, "h1")).unwrap());
    assert!(insert_verdict_if_changed(&conn, &at(VerdictKind::AnchorReinstated, "h2")).unwrap());
    assert!(insert_verdict_if_changed(&conn, &at(VerdictKind::SupersededBy, "h3")).unwrap());
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3);
}

#[test]
fn reinstatement_is_a_new_event_not_a_mutation() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let wid: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(42),
            receipt_oid: Some("head1".into()),
            observed_head_oid: "head1".into(),
        },
    )
    .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorReinstated,
            successor_witness_id: None,
            receipt_oid: Some("head2".into()),
            observed_head_oid: "head2".into(),
        },
    )
    .unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM witness_verdicts WHERE witness_id = ?1",
            params![wid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2, "both events must survive — no mutation");
    let latest = latest_event(&conn, wid).unwrap().unwrap();
    assert_eq!(latest.verdict, VerdictKind::AnchorReinstated);
}

#[test]
fn a_b_evolution_is_annotate_not_demote() {
    // Two different at_oid rows for the SAME (project, file, symbol);
    // the OLDER one is superseded by the newer, which IS the row at the
    // observed HEAD — plain A -> B evolution. A negative-latest witness
    // exists AND a witness is intact at HEAD, so the state is the
    // ANNOTATE channel (no rank effect), never Demote.
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    witness_ledger::insert_witness(&conn, &ledger_row("bbb", "b3:2")).unwrap();
    let old_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE at_oid = 'aaa'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let new_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE at_oid = 'bbb'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: old_id,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(new_id),
            receipt_oid: Some("bbb".into()),
            observed_head_oid: "bbb".into(),
        },
    )
    .unwrap();
    let state = symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
        .unwrap()
        .expect("A -> B evolution must surface on the annotate channel");
    assert_eq!(
        state.channel,
        VerdictChannel::Annotate,
        "a witness intact at the observed HEAD (the 'bbb' row) blocks demotion"
    );
    assert_eq!(
        state.representative.receipt_oid.as_deref(),
        Some("bbb"),
        "annotation carries the most recent negative event's receipt"
    );
}

#[test]
fn head_stamp_lookup_is_project_agnostic() {
    // The stamp pass assigns ONE project per file, so the HEAD row for
    // the same absolute file+symbol can sit under another project than
    // the witness being judged. Three witnesses: p1 @aaa (b3:1),
    // p1 @bbb (b3:2), and p2 @ccc (b3:1 — HEAD, same content as aaa).
    // The p1 witness at bbb is obsolete, observed at HEAD ccc. A
    // project-keyed lookup finds no row at ccc under p1 and demotes;
    // the project-agnostic lookup sees b3:1 intact at HEAD: Annotate.
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    witness_ledger::insert_witness(&conn, &ledger_row("bbb", "b3:2")).unwrap();
    let mut head = ledger_row("ccc", "b3:1");
    head.project = "other".into();
    witness_ledger::insert_witness(&conn, &head).unwrap();
    let bbb_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE at_oid = 'bbb'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: bbb_id,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("ccc".into()),
            observed_head_oid: "ccc".into(),
        },
    )
    .unwrap();
    let single = symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
        .unwrap()
        .expect("negative-latest witness must surface");
    assert_eq!(single.channel, VerdictChannel::Annotate);

    let pairs = vec![("proj".to_string(), "/repo/src/lib.rs".to_string())];
    let batched = symbol_verdict_states_for_files(&conn, &pairs).unwrap();
    let key = (
        "proj".to_string(),
        "/repo/src/lib.rs".to_string(),
        "foo".to_string(),
    );
    assert_eq!(batched[&key].channel, VerdictChannel::Annotate);

    let lineages = vec![(
        "proj".to_string(),
        "/repo/src/lib.rs".to_string(),
        "foo".to_string(),
        None,
    )];
    let by_lineage = symbol_verdict_states_for_lineages(&conn, &lineages).unwrap();
    assert_eq!(by_lineage[&key].channel, VerdictChannel::Annotate);
    assert!(
        all_demoted_symbols(&conn).unwrap().is_empty(),
        "not demoted: the symbol is intact at HEAD under another project"
    );
}

#[test]
fn vanished_symbol_is_demote() {
    // Negative latest event AND no ledger row at the observed HEAD —
    // the symbol is truly gone at current truth: DEMOTE channel.
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    witness_ledger::insert_witness(&conn, &ledger_row("bbb", "b3:2")).unwrap();
    let old_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE at_oid = 'aaa'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: old_id,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("headoid".into()),
            observed_head_oid: "headoid".into(), // no ledger row at this oid
        },
    )
    .unwrap();
    let state = symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
        .unwrap()
        .expect("obsolete with no HEAD witness must surface");
    assert_eq!(state.channel, VerdictChannel::Demote);
    assert_eq!(state.representative.verdict, VerdictKind::AnchorObsolete);
}

#[test]
fn no_verdict_recorded_returns_none_for_symbol_query() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    assert!(
        symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_b_a_revert_is_never_demoted_regardless_of_event_insertion_order() {
    // A -> B -> A with the FULL event history a real dream run leaves
    // behind: ledger rows at c1 (stamp A), c2 (stamp B), c3 (stamp A
    // again, c3 == the most recent observed HEAD). The c1 witness
    // carries superseded_by (minted at HEAD c2) then anchor_reinstated
    // (minted at HEAD c3); the c2 (B) witness carries its own
    // superseded_by (receipt c3) and stays NEGATIVE-latest while the A
    // witnesses are intact at HEAD. By the two-channel rule that is
    // ANNOTATE — "the symbol carries history of a rejected change",
    // surfaced with the B-supersession receipt (c3) — and NEVER Demote,
    // whichever ORDER c1's two events were inserted in (C1:
    // order-independence).
    for reversed in [false, true] {
        let conn = open();
        witness_ledger::insert_witness(&conn, &ledger_row("c1", "b3:A")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("c2", "b3:B")).unwrap();
        witness_ledger::insert_witness(&conn, &ledger_row("c3", "b3:A")).unwrap();
        let oid_id = |oid: &str| -> i64 {
            conn.query_row(
                "SELECT id FROM witness_ledger WHERE at_oid = ?1",
                params![oid],
                |r| r.get(0),
            )
            .unwrap()
        };
        let (c1_id, c2_id, c3_id) = (oid_id("c1"), oid_id("c2"), oid_id("c3"));
        let c1_superseded = WitnessVerdictRow {
            witness_id: c1_id,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(c2_id),
            receipt_oid: Some("c2".into()),
            observed_head_oid: "c2".into(),
        };
        let c1_reinstated = WitnessVerdictRow {
            witness_id: c1_id,
            verdict: VerdictKind::AnchorReinstated,
            successor_witness_id: None,
            receipt_oid: Some("c3".into()),
            observed_head_oid: "c3".into(),
        };
        // B's own supersession by the revert commit — its latest (and
        // uncancelled) event.
        let c2_superseded = WitnessVerdictRow {
            witness_id: c2_id,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(c3_id),
            receipt_oid: Some("c3".into()),
            observed_head_oid: "c3".into(),
        };
        let events: [&WitnessVerdictRow; 3] = if reversed {
            [&c1_reinstated, &c1_superseded, &c2_superseded]
        } else {
            [&c1_superseded, &c1_reinstated, &c2_superseded]
        };
        for e in events {
            insert_verdict_if_changed(&conn, e).unwrap();
        }
        let state = symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .expect("B's uncancelled negative event must surface");
        assert_eq!(
            state.channel,
            VerdictChannel::Annotate,
            "A->B->A must never demote (reversed insertion: {reversed})"
        );
        assert_eq!(
            state.representative.receipt_oid.as_deref(),
            Some("c3"),
            "annotation carries the B-supersession receipt"
        );
    }
}

#[test]
fn b_a_b_state_revisit_inserts_three_events_and_reruns_add_nothing() {
    // B -> A -> B: superseded, reinstated, then superseded again with
    // the exact same fields as the first event. The third event DIFFERS
    // from the latest (reinstated), so it must insert — the old UNIQUE
    // identity index would have silently swallowed it (C2). A rerun of
    // the same final event at the same HEAD then adds nothing.
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let wid: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    let superseded = WitnessVerdictRow {
        witness_id: wid,
        verdict: VerdictKind::SupersededBy,
        successor_witness_id: Some(42),
        receipt_oid: Some("succ".into()),
        observed_head_oid: "head1".into(),
    };
    let reinstated = WitnessVerdictRow {
        witness_id: wid,
        verdict: VerdictKind::AnchorReinstated,
        successor_witness_id: None,
        receipt_oid: Some("head2".into()),
        observed_head_oid: "head2".into(),
    };
    assert!(insert_verdict_if_changed(&conn, &superseded).unwrap());
    assert!(insert_verdict_if_changed(&conn, &reinstated).unwrap());
    assert!(
        insert_verdict_if_changed(&conn, &superseded).unwrap(),
        "re-visiting an earlier state must insert a THIRD event"
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3, "B -> A -> B is three events");
    let latest = latest_event(&conn, wid).unwrap().unwrap();
    assert!(
        latest.verdict.is_negative(),
        "final state must be negative (superseded)"
    );
    assert!(
        !insert_verdict_if_changed(&conn, &superseded).unwrap(),
        "rerun at the same HEAD with the same conclusion adds nothing"
    );
    let count_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_after, 3);
}

#[test]
fn all_events_with_anchor_joins_and_orders_newest_first() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    witness_ledger::insert_witness(&conn, &ledger_row("bbb", "b3:2")).unwrap();
    let old_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE at_oid = 'aaa'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let new_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE at_oid = 'bbb'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: old_id,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(new_id),
            receipt_oid: Some("bbb".into()),
            observed_head_oid: "bbb".into(),
        },
    )
    .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: old_id,
            verdict: VerdictKind::AnchorReinstated,
            successor_witness_id: None,
            receipt_oid: Some("ccc".into()),
            observed_head_oid: "ccc".into(),
        },
    )
    .unwrap();
    let events = all_events_with_anchor(&conn).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0].verdict,
        VerdictKind::AnchorReinstated,
        "newest event first"
    );
    assert_eq!(events[0].project, "proj");
    assert_eq!(events[0].symbol.as_deref(), Some("foo"));
    assert_eq!(events[1].verdict, VerdictKind::SupersededBy);
}

#[test]
fn all_events_with_anchor_empty_db_returns_empty() {
    let conn = open();
    assert!(all_events_with_anchor(&conn).unwrap().is_empty());
}

#[test]
fn conversation_ids_for_anchors_uses_transcript_attribution_never_projections() {
    let conn = open();
    // node-a: transcript attribution AND (different) projection endpoints.
    // The projections must never leak into the linkage.
    conn.execute(
        "INSERT INTO code_nodes
                (id, project, file, kind, name, first_conv_id, last_conv_id)
             VALUES ('node-a', 'proj', '/repo/src/lib.rs', 'function', 'foo',
                     'projection-first', 'projection-last')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO code_node_attribution (node_id, channel, source_id)
             VALUES ('node-a', 'transcript', 'conversation-attributed')",
        [],
    )
    .unwrap();
    // node-b: same anchor name via a second kind, its own attribution —
    // deduplicated into the same anchor's list. A git-channel row must
    // not count as conversation linkage.
    conn.execute(
        "INSERT INTO code_nodes
                (id, project, file, kind, name, first_conv_id, last_conv_id)
             VALUES ('node-b', 'proj', '/repo/src/lib.rs', 'method', 'foo',
                     'projection-first', 'projection-last')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO code_node_attribution (node_id, channel, source_id)
             VALUES ('node-b', 'transcript', 'conversation-attributed')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO code_node_attribution (node_id, channel, source_id)
             VALUES ('node-b', 'git', 'abc123def')",
        [],
    )
    .unwrap();
    // projection-only node: endpoints recorded, NO transcript attribution
    // -> must be absent from the map, not linked via projections.
    conn.execute(
        "INSERT INTO code_nodes
                (id, project, file, kind, name, first_conv_id, last_conv_id)
             VALUES ('node-c', 'proj', '/repo/src/lib.rs', 'function', 'bar',
                     'projection-only-conv', 'projection-only-conv')",
        [],
    )
    .unwrap();

    let anchors = vec![
        (
            "proj".to_string(),
            "/repo/src/lib.rs".to_string(),
            "foo".to_string(),
        ),
        (
            "proj".to_string(),
            "/repo/src/lib.rs".to_string(),
            "bar".to_string(),
        ),
    ];
    let links = conversation_ids_for_anchors(&conn, &anchors).unwrap();

    assert_eq!(
        links.get(&anchors[0]).unwrap(),
        &vec!["conversation-attributed".to_string()],
        "transcript attribution only: no projections, no git channel, deduped"
    );
    assert!(
        !links.contains_key(&anchors[1]),
        "projection-only anchor must read as 'no linked conversations'"
    );
}

#[test]
fn last_dream_run_reports_the_globally_newest_event() {
    let conn = open();
    assert!(
        last_dream_run(&conn).unwrap().is_none(),
        "no events ever recorded"
    );
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let wid: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("head1".into()),
            observed_head_oid: "head1".into(),
        },
    )
    .unwrap();
    let (head_oid, created_at) = last_dream_run(&conn).unwrap().unwrap();
    assert_eq!(head_oid, "head1");
    assert!(!created_at.is_empty());
}

#[test]
fn event_totals_by_verdict_counts_every_event_not_just_latest() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let wid: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(42),
            receipt_oid: Some("head1".into()),
            observed_head_oid: "head1".into(),
        },
    )
    .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorReinstated,
            successor_witness_id: None,
            receipt_oid: Some("head2".into()),
            observed_head_oid: "head2".into(),
        },
    )
    .unwrap();
    let (obsolete, superseded, reinstated) = event_totals_by_verdict(&conn).unwrap();
    assert_eq!(obsolete, 0);
    assert_eq!(
        superseded, 1,
        "counts the event even though it's no longer latest"
    );
    assert_eq!(reinstated, 1);
}

#[test]
fn all_demoted_symbols_finds_only_the_demote_channel_anchor() {
    let conn = open();
    // Anchor 1 ("foo"): vanished symbol -> Demote.
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let foo_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE stamp = 'b3:1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: foo_id,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("headoid".into()),
            observed_head_oid: "headoid".into(),
        },
    )
    .unwrap();

    // Anchor 2 ("bar"): plain A -> B evolution -> Annotate, never Demote.
    let mut bar_a = ledger_row("bbb", "b3:bar-a");
    bar_a.symbol = Some("bar".into());
    let mut bar_b = ledger_row("ccc", "b3:bar-b");
    bar_b.symbol = Some("bar".into());
    witness_ledger::insert_witness(&conn, &bar_a).unwrap();
    witness_ledger::insert_witness(&conn, &bar_b).unwrap();
    let bar_a_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE stamp = 'b3:bar-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let bar_b_id: i64 = conn
        .query_row(
            "SELECT id FROM witness_ledger WHERE stamp = 'b3:bar-b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: bar_a_id,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(bar_b_id),
            receipt_oid: Some("ccc".into()),
            observed_head_oid: "ccc".into(),
        },
    )
    .unwrap();

    let demoted = all_demoted_symbols(&conn).unwrap();
    assert_eq!(demoted.len(), 1, "only the vanished 'foo' anchor demotes");
    assert_eq!(demoted[0].symbol.as_deref(), Some("foo"));
    assert_eq!(demoted[0].state.channel, VerdictChannel::Demote);
}

#[test]
fn all_demoted_symbols_empty_when_no_negative_events() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    assert!(all_demoted_symbols(&conn).unwrap().is_empty());
}

// ---- SINCE THEN (journal v2 Phase 5) -----------------------------------

#[test]
fn since_then_conclusion_unverified_when_never_witnessed() {
    let conn = open();
    let conclusion =
        since_then_conclusion(&conn, "proj", "/repo/src/lib.rs", "never_seen").unwrap();
    assert_eq!(conclusion.verdict, None);
    assert!(!conclusion.witnessed);
    assert_eq!(conclusion.receipt_oid, None);
}

#[test]
fn since_then_conclusion_witnessed_unverified_when_no_verdict() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let conclusion = since_then_conclusion(&conn, "proj", "/repo/src/lib.rs", "foo").unwrap();
    assert_eq!(conclusion.verdict, None);
    assert!(
        conclusion.witnessed,
        "a ledger row exists — absence of a verdict must not read the same as absence of evidence"
    );
    assert!(
        conclusion.witnessed_at.is_some(),
        "F1: a witness with no verdict is 'witnessed <date> · unverified since', never a \
             fabricated 'still live' claim — the date must come from the ledger row, not from HEAD"
    );
}

#[test]
fn since_then_conclusion_reports_the_latest_event() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let witness_id: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: None,
            receipt_oid: Some("1234567890abcdef".into()),
            observed_head_oid: "head1".into(),
        },
    )
    .unwrap();
    let conclusion = since_then_conclusion(&conn, "proj", "/repo/src/lib.rs", "foo").unwrap();
    assert_eq!(conclusion.verdict, Some(VerdictKind::SupersededBy));
    assert_eq!(conclusion.receipt_oid.as_deref(), Some("1234567890abcdef"));
    assert!(conclusion.event_date.is_some());
}

#[test]
fn open_proposal_claim_ignores_promoted_and_absent_bindings() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let witness_id: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        open_proposal_claim_for_symbol(&conn, "proj", "/repo/src/lib.rs", "foo").unwrap(),
        None,
        "no chunk binding at all -> no proposal"
    );
    conn.execute(
        "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES ('chunk-1', 'conv-1', 'proj', '2026-01-01T00:00:00Z', 'text', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO witness_chunk_bindings (witness_id, chunk_id) VALUES (?1, 'chunk-1')",
        params![witness_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO resolution_proposals (chunk_id, claim, evidence, session_id, created_at)
             VALUES ('chunk-1', 'foo looks resolved', 'task done', 'sess-1', '2026-01-02 00:00:00')",
        [],
    )
    .unwrap();
    assert_eq!(
        open_proposal_claim_for_symbol(&conn, "proj", "/repo/src/lib.rs", "foo")
            .unwrap()
            .as_deref(),
        Some("foo looks resolved")
    );
    conn.execute(
        "INSERT INTO resolution_ledger
                (chunk_id, status, evidence, claim, source, created_at)
             VALUES ('chunk-1', 'resolved', 'promoted', 'foo looks resolved',
                     'user_confirmed', '2026-01-03 00:00:00')",
        [],
    )
    .unwrap();
    assert_eq!(
        open_proposal_claim_for_symbol(&conn, "proj", "/repo/src/lib.rs", "foo").unwrap(),
        None,
        "a later promotion must retire the proposal as open evidence"
    );
}

#[test]
fn day_digest_counts_only_the_given_day_and_carries_receipts() {
    let conn = open();
    witness_ledger::insert_witness(&conn, &ledger_row("aaa", "b3:1")).unwrap();
    let witness_id: i64 = conn
        .query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO witness_verdicts
                (witness_id, verdict, receipt_oid, observed_head_oid, created_at)
             VALUES (?1, 'superseded_by', 'deadbeef', 'head', '2026-02-10 09:00:00')",
        params![witness_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO witness_verdicts
                (witness_id, verdict, receipt_oid, observed_head_oid, created_at)
             VALUES (?1, 'anchor_obsolete', NULL, 'head', '2026-02-11 09:00:00')",
        params![witness_id],
    )
    .unwrap();

    let digest = day_digest(&conn, "2026-02-10").unwrap();
    assert_eq!(digest.retired_count, 1);
    assert_eq!(digest.receipts, vec!["deadbeef".to_string()]);

    let empty = day_digest(&conn, "2026-02-09").unwrap();
    assert_eq!(empty.retired_count, 0);
    assert_eq!(empty.proposal_count, 0);
    assert!(empty.receipts.is_empty());
}

// --- v2 epoch floor ----------------------------------------------------

fn set_floor(conn: &Connection, floor: i64) {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![LIVE_FLOOR_META_KEY, floor.to_string()],
    )
    .unwrap();
}

fn raw_event(conn: &Connection, witness_id: i64, verdict: &str, receipt: &str) -> i64 {
    conn.execute(
        "INSERT INTO witness_verdicts
                (witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid)
             VALUES (?1, ?2, NULL, ?3, ?3)",
        params![witness_id, verdict, receipt],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn witness(conn: &Connection, at_oid: &str, stamp: &str) -> i64 {
    witness_ledger::insert_witness(conn, &ledger_row(at_oid, stamp)).unwrap();
    conn.query_row(
        "SELECT id FROM witness_ledger WHERE at_oid = ?1",
        params![at_oid],
        |r| r.get(0),
    )
    .unwrap()
}

/// The anchors travel as one bound JSON array, so a long list (past the
/// old 200-per-statement batch), repeats, quotes and non-ASCII names all
/// have to reach the join intact, and a repeat must not double a witness.
#[test]
fn lineage_states_take_a_long_list_with_repeats_and_odd_names() {
    let conn = open();
    let w = witness(&conn, "aaa", "b3:1");
    raw_event(&conn, w, "anchor_obsolete", "rcpt");
    let hit = (
        "proj".to_string(),
        "/repo/src/lib.rs".to_string(),
        "foo".to_string(),
        None,
    );
    let mut anchors: Vec<(String, String, String, Option<String>)> = (0..450)
        .map(|i| {
            (
                "proj".to_string(),
                format!("/repo/src/we\"ird \u{e9}{i}.rs"),
                format!("sym'{i}\"\\"),
                if i % 2 == 0 {
                    None
                } else {
                    Some(format!("gen{i}"))
                },
            )
        })
        .collect();
    anchors.insert(300, hit.clone());
    anchors.push(hit.clone());
    anchors.push(hit.clone());
    let states = symbol_verdict_states_for_lineages(&conn, &anchors).unwrap();
    assert_eq!(states.len(), 1, "only the audited anchor has a state");
    let key = (hit.0.clone(), hit.1.clone(), hit.2.clone());
    assert_eq!(states[&key].negative_witness_ids, vec![w]);
    assert!(symbol_verdict_states_for_lineages(&conn, &[])
        .unwrap()
        .is_empty());
}

#[test]
fn witness_with_only_sub_floor_events_has_no_latest_event() {
    let conn = open();
    let w = witness(&conn, "aaa", "b3:1");
    let old = raw_event(&conn, w, "anchor_obsolete", "old");
    assert!(latest_event(&conn, w).unwrap().is_some(), "floor 0: live");
    set_floor(&conn, old);
    assert!(
        latest_event(&conn, w).unwrap().is_none(),
        "an event at or below the floor is invisible: the join judges the witness fresh"
    );
    // A new live event of the SAME kind still writes (nothing live to dedupe against).
    let row = WitnessVerdictRow {
        witness_id: w,
        verdict: VerdictKind::AnchorObsolete,
        successor_witness_id: None,
        receipt_oid: Some("new".into()),
        observed_head_oid: "new".into(),
    };
    assert!(insert_verdict_if_changed(&conn, &row).unwrap());
    assert_eq!(
        latest_event(&conn, w)
            .unwrap()
            .unwrap()
            .receipt_oid
            .as_deref(),
        Some("new")
    );
}

#[test]
fn every_verdict_reader_ignores_events_at_or_below_the_floor() {
    let conn = open();
    let w_old = witness(&conn, "aaa", "b3:1");
    let w_head = witness(&conn, "bbb", "b3:2");
    let old1 = raw_event(&conn, w_old, "anchor_obsolete", "oldrcpt");
    let old2 = raw_event(&conn, w_old, "superseded_by", "oldrcpt2");
    let today = conn
        .query_row("SELECT SUBSTR(datetime('now'), 1, 10)", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();

    // Floor 0: everything is live, every reader sees the events.
    assert!(
        symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .is_some()
    );
    let pairs = vec![("proj".to_string(), "/repo/src/lib.rs".to_string())];
    assert_eq!(
        symbol_verdict_states_for_files(&conn, &pairs)
            .unwrap()
            .len(),
        1
    );
    let lineages = vec![(
        "proj".to_string(),
        "/repo/src/lib.rs".to_string(),
        "foo".to_string(),
        None,
    )];
    assert_eq!(
        symbol_verdict_states_for_lineages(&conn, &lineages)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(all_demoted_symbols(&conn).unwrap().len(), 1);
    assert_eq!(all_events_with_anchor(&conn).unwrap().len(), 2);
    assert!(last_dream_run(&conn).unwrap().is_some());
    assert_eq!(event_totals_by_verdict(&conn).unwrap(), (1, 1, 0));
    assert_eq!(
        since_then_conclusion(&conn, "proj", "/repo/src/lib.rs", "foo")
            .unwrap()
            .verdict,
        Some(VerdictKind::SupersededBy)
    );
    assert_eq!(day_digest(&conn, &today).unwrap().retired_count, 2);

    // Floor at the newest old event: all of them vanish.
    set_floor(&conn, old2.max(old1));
    assert!(
        symbol_verdict_state(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .is_none()
    );
    assert!(symbol_verdict_states_for_files(&conn, &pairs)
        .unwrap()
        .is_empty());
    assert!(symbol_verdict_states_for_lineages(&conn, &lineages)
        .unwrap()
        .is_empty());
    assert!(all_demoted_symbols(&conn).unwrap().is_empty());
    assert!(all_events_with_anchor(&conn).unwrap().is_empty());
    assert!(last_dream_run(&conn).unwrap().is_none());
    assert_eq!(event_totals_by_verdict(&conn).unwrap(), (0, 0, 0));
    let since = since_then_conclusion(&conn, "proj", "/repo/src/lib.rs", "foo").unwrap();
    assert_eq!(since.verdict, None);
    assert!(since.witnessed, "the ledger still witnessed it");
    let digest = day_digest(&conn, &today).unwrap();
    assert_eq!(digest.retired_count, 0);
    assert!(digest.receipts.is_empty());

    // A live event above the floor is seen again, and only it.
    let live = raw_event(&conn, w_head, "anchor_obsolete", "livercpt");
    assert!(live > old2);
    assert_eq!(all_events_with_anchor(&conn).unwrap().len(), 1);
    assert_eq!(event_totals_by_verdict(&conn).unwrap(), (1, 0, 0));
    assert_eq!(
        last_dream_run(&conn)
            .unwrap()
            .map(|(oid, _)| oid)
            .as_deref(),
        Some("livercpt")
    );
    assert_eq!(day_digest(&conn, &today).unwrap().retired_count, 1);
}

/// `EXPLAIN QUERY PLAN` detail lines for `sql`, every parameter bound NULL.
fn plan_of(conn: &Connection, sql: &str) -> String {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let n = stmt.parameter_count();
    stmt.query_map(
        rusqlite::params_from_iter(std::iter::repeat_n(None::<String>, n)),
        |row| row.get::<_, String>(3),
    )
    .unwrap()
    .collect::<rusqlite::Result<Vec<_>>>()
    .unwrap()
    .join("\n")
}

#[test]
fn symbol_verdict_state_ledger_probes_use_an_index_not_a_table_scan() {
    let conn = open();
    for (name, sql) in [("head stamp", HEAD_STAMP_SQL), ("intact", INTACT_STAMP_SQL)] {
        let plan = plan_of(&conn, sql);
        assert!(
            !plan.contains("SCAN witness_ledger"),
            "{name} lookup scans the ledger:\n{plan}"
        );
        assert!(
            plan.contains("idx_witness_ledger_file_symbol"),
            "{name} lookup must use the (file, symbol, at_oid) index:\n{plan}"
        );
    }
}

#[test]
fn bulk_verdict_state_correlated_ledger_probe_uses_an_index() {
    let conn = open();
    for (name, sql) in [
        ("files", files_states_sql(2)),
        ("lineages", lineage_states_sql()),
    ] {
        let plan = plan_of(&conn, &sql);
        assert!(
            !plan.contains("SCAN wl2") && !plan.contains("SCAN witness_ledger"),
            "{name} wl2 subquery scans the ledger:\n{plan}"
        );
        assert!(
            plan.contains("idx_witness_ledger_file_symbol"),
            "{name} wl2 subquery must use the (file, symbol, at_oid) index:\n{plan}"
        );
    }
}
