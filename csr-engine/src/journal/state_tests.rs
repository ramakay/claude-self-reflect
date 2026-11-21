use super::*;

fn cards(n: usize) -> Vec<ClusterCardView> {
    (0..n)
        .map(|i| {
            let cluster = sample_cluster(
                &format!("c{i:02}"),
                "proj",
                DreamItemGrade::ItemGrade,
                "anchor_obsolete",
                Some("abcdef1234567890"),
                "2026-08-09T12:00:00Z",
                "todo",
                "2026-08-01T09:00:00Z",
                1,
            );
            build_cluster_card(
                &cluster,
                BoardColumn::OutdatedClaims,
                Some(i + 1),
                test_now(),
            )
        })
        .collect()
}

fn board_of(clusters: Vec<DreamCluster>) -> BoardFeed {
    board_feed_of(clusters)
}

#[test]
fn parse_cursor_accepts_absent_empty_and_decimal() {
    assert_eq!(parse_cursor(None), Ok(0));
    assert_eq!(parse_cursor(Some("")), Ok(0));
    assert_eq!(parse_cursor(Some("  ")), Ok(0));
    assert_eq!(parse_cursor(Some("42")), Ok(42));
}

#[test]
fn parse_cursor_rejects_garbage_instead_of_clamping() {
    assert_eq!(parse_cursor(Some("-1")), Err(BadCursor("-1".to_string())));
    assert_eq!(
        parse_cursor(Some("'; DROP TABLE chunks--")),
        Err(BadCursor("'; DROP TABLE chunks--".to_string()))
    );
}

#[test]
fn clamp_limit_bounds_the_page() {
    assert_eq!(clamp_limit(None), DEFAULT_PAGE_LIMIT);
    assert_eq!(clamp_limit(Some(0)), DEFAULT_PAGE_LIMIT);
    assert_eq!(clamp_limit(Some(7)), 7);
    assert_eq!(clamp_limit(Some(10_000)), MAX_PAGE_LIMIT);
}

#[test]
fn build_page_walks_the_feed_and_stops() {
    let all = cards(5);
    let first = build_page(&all, 0, 2);
    assert_eq!(first.items.len(), 2);
    assert_eq!(first.total, 5);
    assert_eq!(first.next_cursor.as_deref(), Some("2"));

    let second = build_page(&all, 2, 2);
    assert_eq!(second.items[0].id, "c02");
    assert_eq!(second.next_cursor.as_deref(), Some("4"));

    let last = build_page(&all, 4, 2);
    assert_eq!(last.items.len(), 1);
    assert_eq!(last.next_cursor, None, "last page must not advertise more");
}

#[test]
fn build_page_past_the_end_is_empty_not_wrapped() {
    let all = cards(3);
    let page = build_page(&all, 99, 10);
    assert!(page.items.is_empty());
    assert_eq!(page.total, 3);
    assert_eq!(page.next_cursor, None);
}

// --- board gates ---------------------------------------------------------

#[test]
fn outdated_claims_needs_all_three_of_its_gate_conditions() {
    let full = sample_cluster(
        "a",
        "proj",
        DreamItemGrade::ItemGrade,
        "anchor_obsolete",
        Some("abcdef1234567890"),
        "2026-08-09T12:00:00Z",
        "todo",
        "2026-08-01T09:00:00Z",
        1,
    );
    assert_eq!(
        BoardColumn::classify(&full, false),
        BoardColumn::OutdatedClaims
    );

    // Drop the receipt → not provably stale.
    let no_receipt = sample_cluster(
        "a",
        "proj",
        DreamItemGrade::ItemGrade,
        "anchor_obsolete",
        None,
        "2026-08-09T12:00:00Z",
        "todo",
        "2026-08-01T09:00:00Z",
        1,
    );
    assert_eq!(
        BoardColumn::classify(&no_receipt, false),
        BoardColumn::Observations
    );

    // Drop the item-grade binding → the conclusion is not bound to the
    // item's own symbols, so it cannot be called stale work.
    let session = sample_cluster(
        "a",
        "proj",
        DreamItemGrade::SessionGrade,
        "anchor_obsolete",
        Some("abcdef1234567890"),
        "2026-08-09T12:00:00Z",
        "todo",
        "2026-08-01T09:00:00Z",
        1,
    );
    assert_eq!(
        BoardColumn::classify(&session, false),
        BoardColumn::Observations
    );

    // Restorative verdicts are Observations by directive, never stale.
    let reinstated = sample_cluster(
        "a",
        "proj",
        DreamItemGrade::ItemGrade,
        "anchor_reinstated",
        Some("abcdef1234567890"),
        "2026-08-09T12:00:00Z",
        "todo",
        "2026-08-01T09:00:00Z",
        1,
    );
    assert_eq!(
        BoardColumn::classify(&reinstated, false),
        BoardColumn::Observations
    );
}

#[test]
fn proposals_requires_a_verified_plan_and_outranks_the_other_gates() {
    let cluster = sample_cluster(
        "a",
        "proj",
        DreamItemGrade::ItemGrade,
        "anchor_obsolete",
        Some("abcdef1234567890"),
        "2026-08-09T12:00:00Z",
        "todo",
        "2026-08-01T09:00:00Z",
        1,
    );
    assert_eq!(
        BoardColumn::classify(&cluster, false),
        BoardColumn::OutdatedClaims
    );
    assert_eq!(
        BoardColumn::classify(&cluster, true),
        BoardColumn::Proposals,
        "a verified plan is the highest-confidence gate"
    );
}

#[test]
fn a_sentinel_plan_row_does_not_promote_anything() {
    // `verified_plan_items` is populated ONLY from rows whose steps array
    // is non-empty (the SQL in `StorageDreamFeed::load_board`). A cluster
    // whose item is absent from that set is classified by evidence alone.
    let cluster = sample_cluster(
        "a",
        "proj",
        DreamItemGrade::SessionGrade,
        "anchor_obsolete",
        None,
        "2026-08-09T12:00:00Z",
        "todo",
        "2026-08-01T09:00:00Z",
        1,
    );
    let feed = board_of(vec![cluster]);
    assert!(feed.verified_plan_items.is_empty());
    let board = build_board(&feed, None, test_now());
    assert_eq!(board.columns[0].count, 0, "Proposals must stay empty");
    assert_eq!(board.columns[1].count, 1);
}

// --- ranking -------------------------------------------------------------

/// Every tier boundary, built so that ONLY that tier can decide, and fed
/// in the exact reverse of the expected order.
fn tier_fixture() -> Vec<DreamCluster> {
    vec![
        // 5: oldest open last
        sample_cluster(
            "t5",
            "zzz",
            DreamItemGrade::SessionGrade,
            "anchor_reinstated",
            None,
            "2026-08-01T00:00:00Z",
            "todo",
            "2026-07-30T00:00:00Z",
            9,
        ),
        // 4: blocker before todo (same grade/class/date, older open)
        sample_cluster(
            "t4",
            "yyy",
            DreamItemGrade::SessionGrade,
            "anchor_reinstated",
            None,
            "2026-08-01T00:00:00Z",
            "blocker",
            "2026-07-31T00:00:00Z",
            8,
        ),
        // 3: newer witnessed date
        sample_cluster(
            "t3",
            "xxx",
            DreamItemGrade::SessionGrade,
            "anchor_reinstated",
            None,
            "2026-08-05T00:00:00Z",
            "todo",
            "2026-07-31T00:00:00Z",
            7,
        ),
        // 2: receipt-bearing adverse before restorative
        sample_cluster(
            "t2",
            "www",
            DreamItemGrade::SessionGrade,
            "superseded_by",
            Some("beef000000000000"),
            "2026-08-05T00:00:00Z",
            "todo",
            "2026-07-31T00:00:00Z",
            6,
        ),
        // 1: item-grade before session-grade
        sample_cluster(
            "t1",
            "vvv",
            DreamItemGrade::ItemGrade,
            "superseded_by",
            Some("beef000000000000"),
            "2026-08-05T00:00:00Z",
            "todo",
            "2026-07-31T00:00:00Z",
            5,
        ),
    ]
}

#[test]
fn board_order_is_the_five_tiers_and_nothing_else() {
    let feed = board_of(tier_fixture());
    let order: Vec<String> = ranked_active(&feed)
        .iter()
        .map(|(_, cluster)| cluster.id.clone())
        .collect();
    assert_eq!(order, vec!["t1", "t2", "t3", "t4", "t5"]);
}

/// The fixture is deliberately adversarial: the LAST cluster in rank
/// order (`t5`) has the alphabetically-last project, the most affected
/// items and the oldest evidence, and the FIRST (`t1`) has the fewest
/// items. If item volume, project name or any activity proxy leaked into
/// the comparison, this order would change.
#[test]
fn neither_project_name_nor_item_volume_can_move_a_card() {
    let mut renamed = tier_fixture();
    for (index, cluster) in renamed.iter_mut().enumerate() {
        cluster.project = format!("aaa-{index}");
        cluster.counts.items = 100 - index;
    }
    let feed = board_of(renamed);
    let order: Vec<String> = ranked_active(&feed)
        .iter()
        .map(|(_, cluster)| cluster.id.clone())
        .collect();
    assert_eq!(order, vec!["t1", "t2", "t3", "t4", "t5"]);
}

#[test]
fn shuffled_input_produces_the_same_total_order() {
    let mut shuffled = tier_fixture();
    shuffled.rotate_left(3);
    shuffled.swap(0, 4);
    let feed = board_of(shuffled);
    let order: Vec<String> = ranked_active(&feed)
        .iter()
        .map(|(_, cluster)| cluster.id.clone())
        .collect();
    assert_eq!(order, vec!["t1", "t2", "t3", "t4", "t5"]);
}

/// The ranking type has no field a churn measurement could occupy, and
/// the board sorts on that type alone. This asserts the structural fact
/// so a later "just add a heat weight" edit fails a test.
#[test]
fn the_rank_type_exposes_only_the_five_tiers_plus_the_id_backstop() {
    let cluster = &tier_fixture()[0];
    let rank = &cluster.rank;
    // Naming every field forces a compile error if one is added.
    let dream_clusters::ClusterRank {
        grade: _,
        verdict_class: _,
        witnessed_date_desc: _,
        kind: _,
        oldest_open: _,
        id: _,
    } = rank.clone();
}

// --- unexamined lane -----------------------------------------------------

fn open_item(id: &str, examined: bool, completed: bool) -> OpenItem {
    use crate::storage::dream_clusters::CompletionReceipt;
    OpenItem {
        id: id.to_string(),
        project: "proj".to_string(),
        item: format!("do {id}"),
        kind: "todo".to_string(),
        origin_session: "sess-1".to_string(),
        origin_ts: "2026-08-01T09:00:00Z".to_string(),
        origin_date: "2026-08-01".to_string(),
        completed: completed.then(|| CompletionReceipt {
            session_id: "sess-2".to_string(),
            completed_at: "2026-08-05T09:00:00Z".to_string(),
            completed_date: "2026-08-05".to_string(),
        }),
        examined,
    }
}

#[test]
fn unexamined_holds_only_open_items_with_no_verdict_evidence() {
    let mut feed = board_of(tier_fixture());
    feed.open_items = vec![
        open_item("gated", true, false),
        open_item("done", false, true),
        open_item("bare", false, false),
    ];
    let board = build_board(&feed, None, test_now());
    assert_eq!(board.unexamined.count, 1);
    assert_eq!(board.unexamined.rows[0].item, "do bare");
    assert!(board.unexamined.rows[0]
        .meta
        .contains("no verdict evidence"));

    // And it is nowhere in a column.
    for column in &board.columns {
        for card in &column.cards {
            assert!(!card.conclusion.contains("do bare"));
            assert!(!card.id.contains("bare"));
        }
    }
}

#[test]
fn an_empty_board_says_so_and_invents_nothing() {
    let board = build_board(&BoardFeed::default(), None, test_now());
    assert!(board.empty);
    assert_eq!(board.count_label, "0 conclusions · 0 affected items");
    assert_eq!(board.unexamined.count, 0);
    assert_eq!(board.pass_label, None);
    assert_eq!(board.columns.len(), 3);
    for column in &board.columns {
        assert!(column.cards.is_empty());
        assert!(!column.empty_note.to_lowercase().contains("all clear"));
    }
}

#[test]
fn a_card_without_a_receipt_is_marked_not_live_and_gets_no_rank_numeral() {
    let cluster = sample_cluster(
        "u1",
        "proj",
        DreamItemGrade::ItemGrade,
        "anchor_obsolete",
        None,
        "2026-08-09T12:00:00Z",
        "todo",
        "2026-08-01T09:00:00Z",
        1,
    );
    let card = build_cluster_card(&cluster, BoardColumn::Observations, Some(1), test_now());
    assert!(card.not_live);
    assert_eq!(
        card.ordinal, None,
        "unverified evidence is excluded from ranking"
    );
    assert!(card.meta.contains("receipt unavailable"));
    assert!(
        !card.meta.contains("⌗"),
        "a missing receipt must never render a placeholder oid"
    );
}

#[test]
fn card_meta_drops_the_age_clause_when_the_timestamp_does_not_parse() {
    let mut cluster = sample_cluster(
        "u1",
        "proj",
        DreamItemGrade::ItemGrade,
        "anchor_obsolete",
        Some("abcdef1234567890"),
        "not-a-timestamp",
        "todo",
        "2026-08-01T09:00:00Z",
        2,
    );
    cluster.witnessed_at = "not-a-timestamp".to_string();
    let card = build_cluster_card(&cluster, BoardColumn::OutdatedClaims, Some(1), test_now());
    assert_eq!(card.meta, "⌗abcdef12 · 2 items affected");
}

#[test]
fn age_label_is_relative_and_absent_when_unparseable() {
    let now = test_now();
    assert_eq!(
        age_label("2026-08-11T00:00:00Z", now).as_deref(),
        Some("today")
    );
    assert_eq!(
        age_label("2026-08-10T00:00:00Z", now).as_deref(),
        Some("1d ago")
    );
    assert_eq!(
        age_label("2026-08-01T00:00:00Z", now).as_deref(),
        Some("10d ago")
    );
    assert_eq!(
        age_label("2026-01-01 00:00:00", now).as_deref(),
        Some("7mo ago")
    );
    assert_eq!(age_label("nonsense", now), None);
    assert_eq!(age_label("", now), None);
}

// --- detail --------------------------------------------------------------

#[test]
fn pass_label_renders_only_with_a_real_event() {
    assert_eq!(pass_label(None), None);
    assert_eq!(
        pass_label(Some((
            "0123456789abcdef".to_string(),
            "2026-08-09T12:00:00Z".to_string()
        ))),
        Some("last pass 2026-08-09 · HEAD 01234567".to_string())
    );
}

#[test]
fn detail_drops_the_receipt_clause_when_none_is_stored() {
    let mut item = sample_item("id00", "proj", "finish the gate");
    item.evidence[0].receipt_oid = None;
    let detail = build_detail(&item, &DetailContext::default(), "tok".into());
    assert_eq!(detail.receipt, None);
    assert!(detail
        .evidence_lines
        .iter()
        .any(|line| line.contains("no receipt")));
}

#[test]
fn detail_wording_comes_from_the_certified_projection() {
    let item = sample_item("id00", "proj", "finish the gate");
    let detail = build_detail(&item, &DetailContext::default(), "tok".into());
    assert_eq!(detail.witnessed.as_deref(), Some("2026-08-09"));
    assert_eq!(detail.receipt.as_deref(), Some("abcdef12"));
    assert!(detail.evidence_lines[0].contains("run_report anchor obsolete"));
    // The banned "still live"/"re-verified" framing must never appear.
    for line in &detail.evidence_lines {
        assert!(!line.contains("still live"), "banned wording: {line}");
        assert!(!line.contains("re-verified"), "banned wording: {line}");
    }
}

#[test]
fn session_grade_detail_does_not_claim_the_items_own_symbol() {
    let mut item = sample_item("id00", "proj", "finish the gate");
    item.grade = DreamItemGrade::SessionGrade;
    let detail = build_detail(&item, &DetailContext::default(), "tok".into());
    assert!(detail
        .why_surfaced
        .contains("not about the item's own symbols"));
    assert_eq!(detail.grade_slug, "session-grade");
}

#[test]
fn find_item_matches_only_a_stored_id() {
    let all: Vec<DreamItem> = (0..3)
        .map(|i| sample_item(&format!("id{i:02}"), "proj", &format!("item {i}")))
        .collect();
    assert!(find_item(&all, "id01").is_some());
    assert!(find_item(&all, "id99").is_none());
    assert!(find_item(&all, "../../etc/passwd").is_none());
}

#[test]
fn static_feed_round_trips_through_the_trait() {
    let items: Vec<DreamItem> = (0..2)
        .map(|i| sample_item(&format!("id{i:02}"), "proj", &format!("item {i}")))
        .collect();
    let feed = StaticDreamFeed::new(items).with_last_pass("deadbeefcafe", "2026-08-09");
    let state = JournalState::new(Arc::new(feed));
    let loaded = state.feed().load().expect("static feed load");
    assert_eq!(loaded.len(), 2);
    assert_eq!(
        state.feed().last_pass(),
        Some(("deadbeefcafe".to_string(), "2026-08-09".to_string()))
    );
}

// --- csrf ----------------------------------------------------------------

#[test]
fn csrf_tokens_are_bound_to_the_item_and_to_the_process_key() {
    let key = CsrfKey::random();
    let a = key.token("id00");
    let b = key.token("id01");
    assert_ne!(a, b, "a token must not be reusable across items");
    assert!(key.verify("id00", &a));
    assert!(!key.verify("id01", &a), "a token must not cross items");
    assert!(!key.verify("id00", ""));
    assert!(!key.verify("id00", &a[..a.len() - 1]));

    let other = CsrfKey::random();
    assert_ne!(other.token("id00"), a, "keys must differ per process");
    assert!(!other.verify("id00", &a));
}

#[test]
fn the_csrf_key_is_never_printed() {
    let key = CsrfKey::random();
    assert_eq!(format!("{key:?}"), "CsrfKey(<redacted>)");
}

// --- write path ----------------------------------------------------------

/// The journal writes through the same ledger `csr_resolve` writes to, so
/// its statuses must be inside that tool's accepted set. If `csr_resolve`
/// ever narrows the set, this fails instead of the journal silently
/// writing a status the rest of the system does not understand.
#[tokio::test]
async fn journal_statuses_are_exactly_what_csr_resolve_accepts() {
    let storage = Arc::new(Storage::open_memory().expect("memory storage"));
    for action in [JournalAction::Resolve, JournalAction::Dismiss] {
        crate::mcp::tools::resolve_chunks(
            &storage,
            vec!["chunk-1".to_string()],
            action.status().to_string(),
            "parity probe".to_string(),
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("csr_resolve rejects {}: {e}", action.status()));
    }
}

#[test]
fn dismiss_is_recorded_as_still_open_never_as_resolved() {
    assert_eq!(JournalAction::Resolve.status(), "resolved");
    assert_eq!(JournalAction::Dismiss.status(), "still_open");
    let item = sample_item("id00", "proj", "finish the gate");
    let evidence = JournalAction::Dismiss.evidence_for(&item);
    assert!(evidence.contains("a dismissal is not a resolution"));
    assert!(evidence.contains(JOURNAL_ORIGIN));
}

#[test]
fn a_read_only_feed_refuses_the_write_instead_of_pretending() {
    let item = sample_item("id00", "proj", "finish the gate");
    let feed = StaticDreamFeed::new(vec![item.clone()]);
    assert_eq!(
        feed.record_verdict(&item, JournalAction::Resolve),
        Err(ResolveError::Unsupported)
    );
    assert!(feed.writes().is_empty());
}

/// The production write path, end to end, against a real database: the
/// verdict lands in `resolution_ledger` as `user_confirmed`, while the
/// separate audit row retains the concrete `journal_ui` interaction.
#[test]
fn the_storage_feed_writes_a_journal_ui_verdict_and_an_audit_row() {
    let storage = Arc::new(Storage::open_memory().expect("memory storage"));
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO chunks
                        (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('chunk-1', 'sess-1', 'proj', '2026-08-01T00:00:00Z', 'body', 1)",
                [],
            )?;
            Ok(())
        })
        .expect("seed chunk");

    let feed = StorageDreamFeed::new(storage.clone());
    let item = sample_item("id00", "proj", "finish the gate");
    let receipt = feed
        .record_verdict(&item, JournalAction::Dismiss)
        .expect("write");
    assert_eq!(receipt.status, "still_open");
    assert_eq!(receipt.origin, JOURNAL_ORIGIN);
    assert_eq!(receipt.chunks, 1);

    let (status, source, evidence): (String, String, String) = storage
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT status, source, evidence FROM resolution_ledger
                     WHERE chunk_id = 'chunk-1' ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .expect("ledger row");
    assert_eq!(status, "still_open");
    assert_eq!(source, queries::RESOLUTION_SOURCE_USER_CONFIRMED);
    assert!(evidence.contains("not actionable"));

    let (action, origin, chunks): (String, String, i64) = storage
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT action, origin, chunk_count FROM journal_audit ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .expect("audit row");
    assert_eq!(action, "dismiss");
    assert_eq!(origin, JOURNAL_ORIGIN);
    assert_eq!(chunks, 1);
    storage.with_connection(|conn| {
        let event:(String,i64,i64)=conn.query_row("SELECT e.receipt_kind,e.trust_tier,r.min_trust FROM resolution_ledger r JOIN provenance_events e ON e.event_id=r.event_id WHERE r.chunk_id='chunk-1'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(event,("journal_ui".into(),4,4));
        conn.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON journal_audit BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;")?;
        Ok(())
    }).unwrap();
    assert!(feed.record_verdict(&item, JournalAction::Resolve).is_err());
    storage
        .with_connection(|conn| {
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM resolution_ledger", [], |r| r.get(0))?;
            assert_eq!(
                count, 1,
                "failed UI audit rolls back the ledger and its event"
            );
            let events: i64 = conn.query_row(
                "SELECT COUNT(*) FROM provenance_events WHERE channel='user_confirmation'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(events, 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_resolved_journal_item_leaves_the_reloaded_board() {
    let storage = Arc::new(Storage::open_memory().expect("memory storage"));
    storage
        .with_connection(|conn| {
            let episode = serde_json::json!({
                "schema": "v2",
                "session_id": "sess-resolve",
                "project": "proj",
                "timestamp": "2026-08-01T00:00:00Z",
                "todos": [{
                    "content": "retire `outdated_symbol` claim",
                    "status": "pending"
                }],
                "files_modified": ["/fixture/outdated.rs"]
            })
            .to_string();
            conn.execute(
                "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES ('episode-resolve', ?1, '[]', '2026-08-01T00:00:00Z')",
                [episode],
            )?;
            conn.execute(
                "INSERT INTO chunks
                        (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('chunk-resolve', 'sess-resolve', 'proj',
                             '2026-08-01T00:00:00Z', 'fixture chunk', 1)",
                [],
            )?;
            conn.execute(
                "INSERT INTO witness_ledger
                        (project, file, symbol, span_start, span_end, stamp, tier,
                         at_oid, source_kind, source_id)
                     VALUES ('proj', '/fixture/outdated.rs', 'outdated_symbol', 1, 3,
                             'stamp-resolve', 'committed', 'at-resolve', 'backfill',
                             'at-resolve')",
                [],
            )?;
            let witness_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO witness_verdicts
                        (witness_id, verdict, receipt_oid, observed_head_oid, created_at)
                     VALUES (?1, 'anchor_obsolete', 'receipt-resolve', 'head-resolve',
                             '2026-08-02T00:00:00Z')",
                [witness_id],
            )?;
            Ok(())
        })
        .expect("seed live board fixture");

    let feed = StorageDreamFeed::new(storage);
    let item = feed
        .load()
        .expect("load target")
        .into_iter()
        .next()
        .expect("target item");
    assert_eq!(
        feed.load_board()
            .expect("board before")
            .clusters
            .active
            .len(),
        1
    );

    feed.record_verdict(&item, JournalAction::Resolve)
        .expect("resolve target");

    assert!(
        feed.load_board()
            .expect("board after")
            .clusters
            .active
            .is_empty(),
        "a successful journal resolve must remove its card on reload"
    );
}

#[test]
fn a_session_with_no_chunks_reports_it_and_writes_nothing() {
    let storage = Arc::new(Storage::open_memory().expect("memory storage"));
    let feed = StorageDreamFeed::new(storage.clone());
    let item = sample_item("id00", "proj", "finish the gate");
    let error = feed
        .record_verdict(&item, JournalAction::Resolve)
        .expect_err("no chunks means no write");
    assert_eq!(
        error,
        ResolveError::NoChunks {
            session: "sess-1".to_string()
        }
    );
    assert!(error.to_string().contains("Nothing was written."));

    let count: i64 = storage
        .with_connection(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM resolution_ledger", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .expect("count");
    assert_eq!(count, 0);
}

/// A failed detail read must surface as an error, not as an empty
/// context that the template would render as "nothing on record"
/// (codex X4 finding 9).
#[test]
fn a_failing_detail_read_is_an_error_not_an_empty_context() {
    let item = sample_item("id00", "proj", "finish the gate");
    let feed = StaticDreamFeed::new(vec![item.clone()]).with_detail_error("database is locked");
    let error = feed.detail_context(&item).expect_err("must propagate");
    assert!(error.to_string().contains("database is locked"));
}

#[test]
fn a_successful_empty_detail_read_yields_an_empty_context() {
    let item = sample_item("id00", "proj", "finish the gate");
    let feed = StaticDreamFeed::new(vec![item.clone()]);
    let context = feed.detail_context(&item).expect("successful read");
    assert!(context.brief.empty);
    assert!(context.plan.is_none());
    assert!(context.churn.is_empty());
}
