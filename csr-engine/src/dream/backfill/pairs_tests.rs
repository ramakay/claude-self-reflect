use super::*;
use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow as LedgerRow};
use crate::storage::witness_verdicts::{insert_verdict_if_changed, WitnessVerdictRow};

fn open() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::storage::migrations::run(&conn).unwrap();
    conn
}

// Single-key family fixtures — every pre-family test ran against the
// one project key "proj"; these keep those tests byte-identical in
// intent while exercising the family-shaped signatures.
fn relapse_pairs_for_proj(conn: &Connection, episodes: &[EpisodeRow]) -> Vec<PairCandidate> {
    let family = Family::single("proj");
    let ledger = FamilyLedgerIndex::load(conn, &family).unwrap();
    generate_relapse_pairs(conn, &family, &ledger, episodes).unwrap()
}

fn ledger_pairs_for_proj(conn: &Connection, episodes: &[EpisodeRow]) -> Vec<PairCandidate> {
    let family = Family::single("proj");
    let ledger = FamilyLedgerIndex::load(conn, &family).unwrap();
    generate_ledger_pairs(conn, &family, &ledger, episodes).unwrap()
}

fn v2_episode(
    session_id: &str,
    project: &str,
    ts: &str,
    outcome: &str,
    anchors_json: &str,
) -> String {
    format!(
        r#"{{
                "schema": "v2",
                "session_id": "{session_id}",
                "project": "{project}",
                "timestamp": "{ts}",
                "request": "work on thing",
                "investigated": [],
                "completed": "did some of it",
                "next_steps": null,
                "blockers": null,
                "outcome": "{outcome}",
                "error_signatures": [],
                "tools_used": [],
                "files_modified": [],
                "message_count": 1,
                "duration_minutes": 1,
                "todos": [],
                "approved_plan": null,
                "prev_episode_id": null,
                "anchors": {anchors_json}
            }}"#
    )
}

fn insert_episode(
    conn: &Connection,
    id: &str,
    session_id: &str,
    project: &str,
    ts: &str,
    outcome: &str,
    anchors_json: &str,
) {
    conn.execute(
        "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
        params![
            id,
            v2_episode(session_id, project, ts, outcome, anchors_json),
            ts
        ],
    )
    .unwrap();
    // Tests read `episode_index`, not `reflections`, directly (matching
    // production, where the daemon runs Stage 0 before any later
    // stage) — materialize immediately so each insert is visible to
    // `load_project_episodes` right away.
    crate::storage::dream_backfill::materialize_episode_index(conn).unwrap();
}

fn anchor_json(file: &str, name: &str, body_hash: &str) -> String {
    format!(
        r#"[{{"file":"{file}","node_kind":"function","name":"{name}","body_hash":"{body_hash}"}}]"#
    )
}

fn seed_ledger_witness_full(
    conn: &Connection,
    project: &str,
    file: &str,
    symbol: &str,
    stamp: &str,
    at_oid: &str,
    source_id: Option<&str>,
) -> i64 {
    insert_witness(
        conn,
        &LedgerRow {
            id: 0,
            project: project.to_string(),
            file: file.to_string(),
            symbol: Some(symbol.to_string()),
            span_start: None,
            span_end: None,
            stamp: stamp.to_string(),
            tier: "committed".to_string(),
            at_oid: Some(at_oid.to_string()),
            source_kind: if source_id.is_some() {
                "conversation".to_string()
            } else {
                "backfill".to_string()
            },
            source_id: source_id.map(str::to_string),
        },
    )
    .unwrap();
    conn.query_row(
        "SELECT id FROM witness_ledger WHERE project = ?1 AND file = ?2 AND symbol = ?3 AND stamp = ?4",
        params![project, file, symbol, stamp],
        |r| r.get(0),
    )
    .unwrap()
}

fn seed_ledger_witness(
    conn: &Connection,
    project: &str,
    file: &str,
    symbol: &str,
    stamp: &str,
) -> i64 {
    seed_ledger_witness_full(conn, project, file, symbol, stamp, "deadbeef", None)
}

// -----------------------------------------------------------------
// A-b: minimal real-git-repo scaffolding, used by the two relapse
// tests below whose fixtures need a git-verifiable death (see
// `death_time::resolve_relapse_death_time` — a `CreatedAtFallback`
// death is unorderable and can no longer satisfy this generator's
// gate, so a fixture with no real repo behind it now correctly yields
// zero pairs instead of one).
// -----------------------------------------------------------------

fn strip_git_env(cmd: &mut Command) {
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
}

fn init_repo(dir: &std::path::Path) -> bool {
    std::fs::create_dir_all(dir).unwrap();
    let mut cmd = Command::new("git");
    strip_git_env(&mut cmd);
    cmd.arg("init").arg("-q").arg(dir);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn commit_all(dir: &std::path::Path) -> Option<String> {
    let run = |args: &[&str]| -> bool {
        let mut cmd = Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("-C").arg(dir).args(args);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    };
    if !run(&["add", "-A"])
        || !run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "seed",
        ])
    {
        return None;
    }
    let mut cmd = Command::new("git");
    strip_git_env(&mut cmd);
    cmd.arg("-C").arg(dir).arg("rev-parse").arg("HEAD");
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

fn mark_obsolete(conn: &Connection, witness_id: i64, observed_head: &str) {
    insert_verdict_if_changed(
        conn,
        &WitnessVerdictRow {
            witness_id,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: None,
            observed_head_oid: observed_head.to_string(),
        },
    )
    .unwrap();
}

// -----------------------------------------------------------------
// G-relapse: join correctness (incl. ts ordering)
// -----------------------------------------------------------------

#[test]
fn relapse_pairs_a_pre_verdict_episode_with_the_earliest_post_verdict_re_touch() {
    // A-b: this generator's event time now comes from a git-verified
    // death (`death_time::resolve_relapse_death_time`), so the fixture
    // needs a real repo behind the witness -- a `CreatedAtFallback`
    // death is unorderable and would otherwise yield zero pairs.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    if !init_repo(&repo) {
        return; // git unavailable in this environment -- fail-soft skip
    }
    let file = repo.join("a.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
    let Some(c0) = commit_all(&repo) else {
        return;
    };
    let stamp = codewitness::StampKind::Raw
        .compute(&std::fs::read(&file).unwrap())
        .as_str()
        .to_string();
    // The death commit: foo's body actually changes here.
    std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
    let Some(c1) = commit_all(&repo) else {
        return;
    };
    let file_str = file.to_string_lossy().to_string();

    let conn = open();
    let wid = seed_ledger_witness_full(&conn, "proj", &file_str, "foo", &stamp, &c0, None);
    mark_obsolete(&conn, wid, &c1);

    // Before the verdict: touches foo.
    insert_episode(
        &conn,
        "ep-before",
        "sess-before",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json(&file_str, "foo", "hash1"),
    );
    // After the verdict: an unrelated episode that does NOT touch foo —
    // must never be picked as the relapse.
    insert_episode(
        &conn,
        "ep-unrelated",
        "sess-unrelated",
        "proj",
        "2099-01-01T00:00:00Z",
        "completed",
        &anchor_json(&file_str, "bar", "hashX"),
    );
    // After the verdict: the actual relapse, later still.
    insert_episode(
        &conn,
        "ep-relapse-late",
        "sess-relapse-late",
        "proj",
        "2099-02-01T00:00:00Z",
        "partial",
        &anchor_json(&file_str, "foo", "hash2"),
    );
    // After the verdict, EARLIER than ep-relapse-late — this is the
    // correct relapse pick (earliest post-verdict re-touch).
    insert_episode(
        &conn,
        "ep-relapse-early",
        "sess-relapse-early",
        "proj",
        "2099-01-15T00:00:00Z",
        "partial",
        &anchor_json(&file_str, "foo", "hash3"),
    );

    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = relapse_pairs_for_proj(&conn, &episodes);
    assert_eq!(pairs.len(), 1, "exactly one symbol has a negative verdict");
    let p = &pairs[0];
    assert_eq!(p.ep_a, "ep-before");
    assert_eq!(p.ep_b, "ep-relapse-early");
    assert!(p.ts_a < p.ts_b, "ts ordering must hold");
    assert_eq!(p.generator, Generator::Relapse);
    assert_eq!(p.topic_key, "symbol:foo");
    assert_eq!(
        p.oid_provenance,
        OidProvenance::GitDerived,
        "a span-bisected death must surface as git-derived"
    );
}

#[test]
fn relapse_pairs_with_no_resolvable_repo_yield_nothing_not_a_backfill_run_time_guess() {
    // A-b's core regression test: the OLD basis (receipt_oid / verdict
    // created_at) would happily have manufactured a relapse pair here
    // ordered by backfill-run wall-clock time. With no real repo behind
    // the witness, death time can only resolve to `CreatedAtFallback`,
    // which is unorderable -- zero pairs, not a guess.
    let conn = open();
    let wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:aaa");
    mark_obsolete(&conn, wid, "headoid");

    insert_episode(
        &conn,
        "ep-before",
        "sess-before",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hash1"),
    );
    insert_episode(
        &conn,
        "ep-relapse",
        "sess-relapse",
        "proj",
        "2099-01-01T00:00:00Z",
        "partial",
        &anchor_json("a.rs", "foo", "hash2"),
    );

    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = relapse_pairs_for_proj(&conn, &episodes);
    assert!(
        pairs.is_empty(),
        "an unorderable death must never satisfy the relapse gate"
    );
}

#[test]
fn relapse_requires_a_pre_verdict_touching_episode_to_form_a_pair() {
    let conn = open();
    let wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:aaa");
    mark_obsolete(&conn, wid, "headoid");
    // Only a post-verdict episode exists — no `E_before` to pair with.
    insert_episode(
        &conn,
        "ep-relapse",
        "sess",
        "proj",
        "2099-01-01T00:00:00Z",
        "partial",
        &anchor_json("a.rs", "foo", "hash"),
    );
    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = relapse_pairs_for_proj(&conn, &episodes);
    assert!(pairs.is_empty());
}

#[test]
fn relapse_ignores_symbols_with_no_negative_verdict() {
    let conn = open();
    // Witness exists but no verdict was ever recorded for it.
    seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:aaa");
    insert_episode(
        &conn,
        "ep-1",
        "sess-1",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hash"),
    );
    insert_episode(
        &conn,
        "ep-2",
        "sess-2",
        "proj",
        "2020-02-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hash2"),
    );
    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = relapse_pairs_for_proj(&conn, &episodes);
    assert!(pairs.is_empty());
}

// -----------------------------------------------------------------
// G-ledger
// -----------------------------------------------------------------

#[test]
fn ledger_pairs_old_advocate_with_earliest_successor_touch() {
    let conn = open();
    let old_wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:old");
    let new_wid = seed_ledger_witness(&conn, "proj", "b.rs", "bar", "b3:new");
    // Give the successor a real record so `witness_by_id` resolves it,
    // and wire successor_witness_id directly in the one verdict event.
    insert_verdict_if_changed(
        &conn,
        &WitnessVerdictRow {
            witness_id: old_wid,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: Some(new_wid),
            receipt_oid: Some("receiptoid".to_string()),
            observed_head_oid: "headoid".to_string(),
        },
    )
    .unwrap();

    insert_episode(
        &conn,
        "ep-old",
        "sess-old",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hashold"),
    );
    // Touches the successor's (file, symbol) after the verdict.
    insert_episode(
        &conn,
        "ep-new-early",
        "sess-new-early",
        "proj",
        "2099-01-10T00:00:00Z",
        "completed",
        &anchor_json("b.rs", "bar", "hashnew1"),
    );
    insert_episode(
        &conn,
        "ep-new-late",
        "sess-new-late",
        "proj",
        "2099-02-10T00:00:00Z",
        "completed",
        &anchor_json("b.rs", "bar", "hashnew2"),
    );

    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = ledger_pairs_for_proj(&conn, &episodes);
    assert_eq!(pairs.len(), 1);
    let p = &pairs[0];
    assert_eq!(p.ep_a, "ep-old");
    assert_eq!(p.ep_b, "ep-new-early", "earliest successor touch wins");
    assert_eq!(p.relation, Relation::ReplacedBy);
    assert_eq!(p.oid_provenance, OidProvenance::CreatedAtFallback);
}

#[test]
fn ledger_falls_back_to_same_file_after_receipt_when_no_successor_touch_exists() {
    let conn = open();
    let wid = seed_ledger_witness(&conn, "proj", "a.rs", "foo", "b3:old");
    mark_obsolete(&conn, wid, "headoid");

    insert_episode(
        &conn,
        "ep-old",
        "sess-old",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hashold"),
    );
    // No episode touches a successor (there is none — AnchorObsolete),
    // but a later episode DOES touch the same file again.
    insert_episode(
        &conn,
        "ep-samefile",
        "sess-samefile",
        "proj",
        "2099-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "somethingelse", "hashy"),
    );

    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = ledger_pairs_for_proj(&conn, &episodes);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].ep_b, "ep-samefile");
}

// -----------------------------------------------------------------
// P2 (F2): ledger-edge-first endpoint resolution beats geometric
// reconstruction for a same-session supersession.
// -----------------------------------------------------------------

fn seed_ledger_witness_with_source(
    conn: &Connection,
    project: &str,
    file: &str,
    symbol: &str,
    stamp: &str,
    source_id: &str,
) -> i64 {
    seed_ledger_witness_full(
        conn,
        project,
        file,
        symbol,
        stamp,
        "deadbeef",
        Some(source_id),
    )
}

#[test]
fn ledger_pairs_prefers_the_witness_source_session_over_the_latest_geometric_touch() {
    let conn = open();
    // The witness's OWN recorded source is sess-old (ep-old's session).
    let old_wid =
        seed_ledger_witness_with_source(&conn, "proj", "a.rs", "foo", "b3:old", "sess-old");
    mark_obsolete(&conn, old_wid, "headoid");

    // ep-old: the TRUE advocate, touches foo, session sess-old.
    insert_episode(
        &conn,
        "ep-old",
        "sess-old",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hashold"),
    );
    // ep-same: ALSO touches foo, LATER than ep-old but still before the
    // verdict's (fallback) event time — under pure geometric
    // reconstruction ("latest pre-event-time touch"), this episode
    // would be wrongly picked as e_old instead of the true advocate.
    insert_episode(
        &conn,
        "ep-same",
        "sess-same",
        "proj",
        "2020-01-02T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hashsame"),
    );
    // A genuinely later episode (past the AnchorObsolete verdict's
    // real-wall-clock fallback event time, since no receipt_oid was
    // given) so e_new's own geometric fallback has something to find —
    // this test is only exercising e_old's source-preference, not e_new.
    insert_episode(
        &conn,
        "ep-new",
        "sess-new",
        "proj",
        "2099-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "somethingelse", "hashnew"),
    );

    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = ledger_pairs_for_proj(&conn, &episodes);
    assert_eq!(pairs.len(), 1);
    assert_eq!(
        pairs[0].ep_a, "ep-old",
        "the witness's own source session must win over the later geometric touch"
    );
}

#[test]
fn relapse_pairs_prefers_the_witness_source_session_for_e_before() {
    // A-b: needs a real, git-verifiable death (see the comment on
    // `relapse_pairs_a_pre_verdict_episode_with_the_earliest_post_verdict_re_touch`).
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    if !init_repo(&repo) {
        return;
    }
    let file = repo.join("a.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
    let Some(c0) = commit_all(&repo) else {
        return;
    };
    let stamp = codewitness::StampKind::Raw
        .compute(&std::fs::read(&file).unwrap())
        .as_str()
        .to_string();
    std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
    let Some(c1) = commit_all(&repo) else {
        return;
    };
    let file_str = file.to_string_lossy().to_string();

    let conn = open();
    let old_wid = seed_ledger_witness_full(
        &conn,
        "proj",
        &file_str,
        "foo",
        &stamp,
        &c0,
        Some("sess-old"),
    );
    mark_obsolete(&conn, old_wid, &c1);

    insert_episode(
        &conn,
        "ep-old",
        "sess-old",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json(&file_str, "foo", "hashold"),
    );
    insert_episode(
        &conn,
        "ep-same",
        "sess-same",
        "proj",
        "2020-01-02T00:00:00Z",
        "completed",
        &anchor_json(&file_str, "foo", "hashsame"),
    );
    // The actual relapse, after the verdict.
    insert_episode(
        &conn,
        "ep-relapse",
        "sess-relapse",
        "proj",
        "2099-01-01T00:00:00Z",
        "partial",
        &anchor_json(&file_str, "foo", "hashrelapse"),
    );

    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = relapse_pairs_for_proj(&conn, &episodes);
    assert_eq!(pairs.len(), 1);
    assert_eq!(
        pairs[0].ep_a, "ep-old",
        "the witness's own source session must win e_before over the later geometric touch"
    );
    assert_eq!(pairs[0].ep_b, "ep-relapse");
}

#[test]
fn ledger_pairs_falls_back_to_geometric_when_source_id_is_blank_or_unresolvable() {
    let conn = open();
    // Blank source_id (empty string, not NULL) must fall back exactly
    // like `None` does.
    let old_wid = seed_ledger_witness_with_source(&conn, "proj", "a.rs", "foo", "b3:old", "");
    mark_obsolete(&conn, old_wid, "headoid");

    insert_episode(
        &conn,
        "ep-old",
        "sess-old",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "foo", "hashold"),
    );
    insert_episode(
        &conn,
        "ep-samefile",
        "sess-samefile",
        "proj",
        "2099-01-01T00:00:00Z",
        "completed",
        &anchor_json("a.rs", "somethingelse", "hashy"),
    );

    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let pairs = ledger_pairs_for_proj(&conn, &episodes);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].ep_a, "ep-old");
    assert_eq!(pairs[0].ep_b, "ep-samefile");
}

// -----------------------------------------------------------------
// Era topic-key stability across recluster
// -----------------------------------------------------------------

#[test]
fn era_topic_key_is_stable_across_different_shared_symbol_or_token_orderings() {
    let mut symbols_a: BTreeSet<String> = BTreeSet::new();
    symbols_a.insert("Foo::bar".to_string());
    symbols_a.insert("Baz::qux".to_string());
    let mut tokens_a: BTreeSet<String> = BTreeSet::new();
    tokens_a.insert("frobnicate".to_string());

    // Same sets, different insertion order — BTreeSet already normalizes
    // this, but the point under test is the *function's* stability, not
    // BTreeSet's.
    let mut symbols_b: BTreeSet<String> = BTreeSet::new();
    symbols_b.insert("Baz::qux".to_string());
    symbols_b.insert("Foo::bar".to_string());
    let mut tokens_b: BTreeSet<String> = BTreeSet::new();
    tokens_b.insert("frobnicate".to_string());

    assert_eq!(
        era_topic_key(&symbols_a, &tokens_a),
        era_topic_key(&symbols_b, &tokens_b)
    );
}

#[test]
fn era_topic_key_is_identical_for_all_pairs_from_the_same_cluster_regardless_of_middle() {
    // Simulates the task's literal scenario: two runs of the SAME
    // three-episode story, but a different middle episode is chosen
    // (e.g. because clustering merged in a fourth near-duplicate that
    // changed which one is closest to the temporal midpoint). The
    // endpoints (earliest/latest) are unchanged between runs, so every
    // pair emitted from the cluster — earliest-latest, earliest-middle,
    // middle-latest — must carry the SAME topic_key in both runs.
    let earliest = mk_episode(
        "e1",
        "2020-01-01T00:00:00Z",
        &["Shared::sym"],
        "shared request text",
    );
    let latest = mk_episode(
        "e3",
        "2020-02-01T00:00:00Z",
        &["Shared::sym"],
        "shared request text",
    );
    let middle_run1 = mk_episode("e2a", "2020-01-10T00:00:00Z", &[], "unrelated");
    let middle_run2 = mk_episode(
        "e2b",
        "2020-01-20T00:00:00Z",
        &[],
        "different unrelated text",
    );

    let shared_symbols = shared_anchor_symbols(
        &earliest,
        &latest,
        &anchor_symbol_frequency(&[earliest.clone(), latest.clone()]),
        10,
    );
    let shared_tokens = shared_rare_tokens(
        &earliest,
        &latest,
        &document_frequency(&[earliest.clone(), latest.clone(), middle_run1.clone()]),
        10,
    );
    let key_run1 = era_topic_key(&shared_symbols, &shared_tokens);

    let shared_tokens_run2 = shared_rare_tokens(
        &earliest,
        &latest,
        &document_frequency(&[earliest.clone(), latest.clone(), middle_run2.clone()]),
        10,
    );
    let key_run2 = era_topic_key(&shared_symbols, &shared_tokens_run2);

    assert_eq!(
        key_run1, key_run2,
        "topic_key must not depend on which episode was picked as the middle"
    );
}

fn mk_episode(id: &str, ts: &str, symbols: &[&str], text: &str) -> EpisodeRow {
    EpisodeRow {
        episode_id: id.to_string(),
        session_id: format!("sess-{id}"),
        ts: parse_timestamp(ts).unwrap(),
        outcome: "completed".to_string(),
        todo_count: 0,
        blockers: None,
        request: text.to_string(),
        completed: String::new(),
        next_steps: None,
        anchors: symbols
            .iter()
            .map(|s| AnchorRow {
                file: "a.rs".to_string(),
                node_kind: "function".to_string(),
                name: s.to_string(),
                body_hash: "h".to_string(),
            })
            .collect(),
        present_at_head: None,
    }
}

// -----------------------------------------------------------------
// Era clustering / theta fit
// -----------------------------------------------------------------

#[test]
fn fit_era_theta_falls_back_to_default_with_no_prev_chain_data() {
    let vectors: HashMap<String, Vec<f32>> = HashMap::new();
    assert_eq!(fit_era_theta(&vectors, &[]), DEFAULT_ERA_THETA);
}

#[test]
fn fit_era_theta_picks_a_grid_value_that_keeps_a_chain_pair_together() {
    let mut vectors = HashMap::new();
    vectors.insert("root".to_string(), vec![1.0, 0.0, 0.0]);
    // Near-identical to root — should co-cluster even at a fairly
    // strict theta.
    vectors.insert("desc".to_string(), vec![0.99, 0.01, 0.0]);
    // A totally unrelated episode in the same project — must not force
    // theta down just because it exists.
    vectors.insert("other".to_string(), vec![0.0, 0.0, 1.0]);

    let chain_pairs = vec![("root".to_string(), "desc".to_string())];
    let theta = fit_era_theta(&vectors, &chain_pairs);

    let clusters = agglomerative_cluster(&vectors, theta);
    let membership = cluster_membership(&clusters);
    assert_eq!(membership.get("root"), membership.get("desc"));
    assert!(
        theta >= 0.90,
        "near-identical chain vectors should fit a high theta, got {theta}"
    );
}

#[test]
fn era_gate_kills_file_path_only_cohesion() {
    let conn = open();
    // Two episodes touching the SAME file but with no shared anchor
    // symbol and no shared rare token in their text — must not emit.
    insert_episode(
        &conn,
        "ep-1",
        "sess-1",
        "proj",
        "2020-01-01T00:00:00Z",
        "completed",
        &anchor_json("shared_file.rs", "alpha", "h1"),
    );
    insert_episode(
        &conn,
        "ep-2",
        "sess-2",
        "proj",
        "2020-02-01T00:00:00Z",
        "completed",
        &anchor_json("shared_file.rs", "beta", "h2"),
    );
    let episodes = load_project_episodes(&conn, "proj").unwrap();
    let mut vectors = HashMap::new();
    vectors.insert("ep-1".to_string(), vec![1.0, 0.0]);
    vectors.insert("ep-2".to_string(), vec![0.99, 0.01]);

    let pairs = generate_era_pairs(&conn, &Family::single("proj"), &episodes, &vectors).unwrap();
    assert!(
        pairs.is_empty(),
        "no shared symbol or rare token beyond the shared file path"
    );
}

#[test]
fn latest_verdict_created_at_reads_live_events_only() {
    let conn = open();
    conn.execute(
        "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, at_oid, source_kind)
             VALUES ('proj', '/f.rs', 'foo', 'b3:foo', 'committed', 'oid', 'backfill')",
        [],
    )
    .unwrap();
    let wid = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid, created_at)
             VALUES (?1, 'anchor_obsolete', 'head', '2026-01-01 00:00:00')",
        params![wid],
    )
    .unwrap();
    assert_eq!(
        latest_verdict_created_at(&conn, wid).unwrap().as_deref(),
        Some("2026-01-01 00:00:00")
    );
    conn.execute(
        "INSERT INTO meta(key, value)
             VALUES ('witness_verdicts_v2_floor',
                     (SELECT CAST(MAX(id) AS TEXT) FROM witness_verdicts))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [],
    )
    .unwrap();
    assert!(latest_verdict_created_at(&conn, wid).unwrap().is_none());
    conn.execute(
        "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid, created_at)
             VALUES (?1, 'anchor_reinstated', 'head', '2026-02-02 00:00:00')",
        params![wid],
    )
    .unwrap();
    assert_eq!(
        latest_verdict_created_at(&conn, wid).unwrap().as_deref(),
        Some("2026-02-02 00:00:00")
    );
}
