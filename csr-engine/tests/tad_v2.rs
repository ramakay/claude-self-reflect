use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{Duration, TimeZone, Utc};
use csr_engine::search::decay::{self, DecayConfig};
use csr_engine::storage::ancestry::{refresh_ancestry_cache_at, AncestryState};
use csr_engine::storage::codegraph::{AttributionRow, NodeRow};
use csr_engine::storage::{migrations, Storage};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8 git output")
        .trim()
        .to_string()
}

fn commit(repo: &Path, serial: usize) -> String {
    fs::write(repo.join("history.txt"), format!("release {serial}\n")).unwrap();
    git(repo, &["add", "history.txt"]);
    git(repo, &["commit", "-m", &format!("release {serial}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn release_repo() -> (tempfile::TempDir, Vec<String>, String) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "CSR test"]);

    let mut released = Vec::new();
    for serial in 1..=6 {
        released.push(commit(dir.path(), serial));
        git(dir.path(), &["tag", &format!("v{serial}.0.0")]);
    }
    let unreleased = commit(dir.path(), 7);
    (dir, released, unreleased)
}

fn link(storage: &Storage, repo: &Path, node_id: &str, session: &str, sha: &str) {
    storage
        .upsert_code_node(&NodeRow {
            id: node_id.into(),
            repo: "fixture".into(),
            project: "fixture".into(),
            file: repo.join(format!("{node_id}.rs")).display().to_string(),
            kind: "function".into(),
            name: node_id.into(),
            fqname: node_id.into(),
            repo_root: Some(repo.display().to_string()),
            ..NodeRow::default()
        })
        .unwrap();
    for (channel, source_id) in [("transcript", session), ("git", sha)] {
        storage
            .upsert_code_attribution(&AttributionRow {
                node_id: node_id.into(),
                channel: channel.into(),
                source_id: source_id.into(),
                observed_ts: None,
                evidence: "test".into(),
            })
            .unwrap();
    }
}

fn transcript_only(storage: &Storage, repo: &Path, node_id: &str, session: &str) {
    storage
        .upsert_code_node(&NodeRow {
            id: node_id.into(),
            repo: "fixture".into(),
            project: "fixture".into(),
            file: repo.join(format!("{node_id}.rs")).display().to_string(),
            kind: "function".into(),
            name: node_id.into(),
            fqname: node_id.into(),
            repo_root: Some(repo.display().to_string()),
            ..NodeRow::default()
        })
        .unwrap();
    storage
        .upsert_code_attribution(&AttributionRow {
            node_id: node_id.into(),
            channel: "transcript".into(),
            source_id: session.into(),
            observed_ts: None,
            evidence: "test".into(),
        })
        .unwrap();
}

fn orphan_link(storage: &Storage, node_id: &str, session: &str, sha: &str) {
    for (channel, source_id) in [("transcript", session), ("git", sha)] {
        storage
            .upsert_code_attribution(&AttributionRow {
                node_id: node_id.into(),
                channel: channel.into(),
                source_id: source_id.into(),
                observed_ts: None,
                evidence: "test".into(),
            })
            .unwrap();
    }
}

#[test]
fn five_releases_behind_decays_harder_than_same_age_without_ancestry() {
    let now = Utc.with_ymd_and_hms(2026, 8, 6, 12, 0, 0).unwrap();
    let timestamp = now - Duration::days(30);
    let config = DecayConfig::for_search();

    let wall_clock = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
    let released =
        decay::apply_tad_with_release_ancestry(0.9, &timestamp, &now, &[], &config, Some(5));
    let hand_derived =
        decay::apply_tad_with_age_multiplier(0.9, &timestamp, &now, &[], &config, 2.25);

    assert!(
        released < wall_clock,
        "a commit shipped five releases ago must decay harder: {released} >= {wall_clock}"
    );
    assert_eq!(released.to_bits(), hand_derived.to_bits());

    let capped =
        decay::apply_tad_with_release_ancestry(0.9, &timestamp, &now, &[], &config, Some(100));
    let four_x = decay::apply_tad_with_age_multiplier(0.9, &timestamp, &now, &[], &config, 4.0);
    assert_eq!(capped.to_bits(), four_x.to_bits());
}

#[test]
fn release_ancestry_cache_is_evidence_only_fail_open_and_deterministic() {
    let (repo, released, unreleased) = release_repo();
    let mixed_unreleased = commit(repo.path(), 8);
    let storage = Storage::open_memory().unwrap();
    link(
        &storage,
        repo.path(),
        "released",
        "session-released",
        &released[0],
    );
    link(
        &storage,
        repo.path(),
        "unreleased",
        "session-unreleased",
        &unreleased,
    );
    link(
        &storage,
        repo.path(),
        "mixed-released",
        "session-mixed",
        &released[1],
    );
    link(
        &storage,
        repo.path(),
        "mixed-unreleased",
        "session-mixed",
        &mixed_unreleased,
    );
    // One commit fanning out to two transcript sessions is ambiguous and
    // contributes to neither session.
    link(
        &storage,
        repo.path(),
        "fan-a",
        "session-fan-a",
        &released[2],
    );
    link(
        &storage,
        repo.path(),
        "fan-a-unique-old",
        "session-fan-a",
        &released[4],
    );
    link(
        &storage,
        repo.path(),
        "orphan-valid",
        "session-orphan",
        &released[3],
    );
    orphan_link(
        &storage,
        "orphan-missing-node",
        "session-orphan",
        &released[4],
    );
    transcript_only(&storage, repo.path(), "no-git-link", "session-no-link");
    link(
        &storage,
        repo.path(),
        "fan-b",
        "session-fan-b",
        &released[2],
    );

    let refreshed_at = Utc::now().to_rfc3339();
    refresh_ancestry_cache_at(&storage, &refreshed_at).unwrap();
    let ids = vec![
        "session-released".to_string(),
        "session-unreleased".to_string(),
        "session-mixed".to_string(),
        "session-fan-a".to_string(),
        "session-fan-b".to_string(),
        "session-no-link".to_string(),
        "session-orphan".to_string(),
    ];
    let first = storage.ancestry_labels_for_conversations(&ids).unwrap();

    let shipped = &first["session-released"];
    assert_eq!(shipped.state, AncestryState::Shipped);
    assert_eq!(shipped.release_tag.as_deref(), Some("v1.0.0"));
    assert_eq!(shipped.releases_behind, 5);

    let fresh = &first["session-unreleased"];
    assert_eq!(fresh.state, AncestryState::Unreleased);
    assert_eq!(fresh.releases_behind_for_decay(), None);
    let mixed = &first["session-mixed"];
    assert_eq!(mixed.state, AncestryState::Shipped);
    assert_eq!(mixed.release_tag.as_deref(), Some("v2.0.0"));
    assert_eq!(mixed.releases_behind, 4);

    assert!(!first.contains_key("session-fan-a"));
    assert!(!first.contains_key("session-fan-b"));
    assert!(!first.contains_key("session-no-link"));
    assert!(!first.contains_key("session-orphan"));

    refresh_ancestry_cache_at(&storage, &refreshed_at).unwrap();
    let second = storage.ancestry_labels_for_conversations(&ids).unwrap();
    assert_eq!(
        second, first,
        "fixed inputs must produce identical cache rows"
    );

    assert!(refresh_ancestry_cache_at(&storage, "not-a-timestamp").is_err());
    assert!(
        storage
            .ancestry_labels_for_conversations(&ids)
            .unwrap()
            .is_empty(),
        "failed refresh must invalidate stale demoting rows"
    );
}

#[test]
fn mixed_shipped_releases_use_newest_release_even_with_unreleased_evidence() {
    let (repo, released, unreleased) = release_repo();
    let storage = Storage::open_memory().unwrap();
    link(
        &storage,
        repo.path(),
        "released-v1",
        "session-mixed-v5",
        &released[0],
    );
    link(
        &storage,
        repo.path(),
        "released-v5",
        "session-mixed-v5",
        &released[4],
    );
    link(
        &storage,
        repo.path(),
        "unreleased",
        "session-mixed-v5",
        &unreleased,
    );

    refresh_ancestry_cache_at(&storage, &Utc::now().to_rfc3339()).unwrap();
    let labels = storage
        .ancestry_labels_for_conversations(&["session-mixed-v5".into()])
        .unwrap();
    let mixed = &labels["session-mixed-v5"];

    assert_eq!(mixed.state, AncestryState::Shipped);
    assert_eq!(mixed.release_tag.as_deref(), Some("v5.0.0"));
    assert_eq!(mixed.releases_behind, 1);
}

#[test]
fn same_commit_sha_across_clone_roots_is_fanout_ambiguous() {
    let (repo, released, _unreleased) = release_repo();
    let clone_parent = tempfile::tempdir().unwrap();
    let clone_path = clone_parent.path().join("clone");
    let source = repo.path().display().to_string();
    let destination = clone_path.display().to_string();
    git(repo.path(), &["clone", "--quiet", &source, &destination]);

    let storage = Storage::open_memory().unwrap();
    link(
        &storage,
        repo.path(),
        "original-node",
        "session-original",
        &released[0],
    );
    link(
        &storage,
        &clone_path,
        "clone-node",
        "session-clone",
        &released[0],
    );

    refresh_ancestry_cache_at(&storage, &Utc::now().to_rfc3339()).unwrap();
    let labels = storage
        .ancestry_labels_for_conversations(&[
            "session-original".to_string(),
            "session-clone".to_string(),
        ])
        .unwrap();

    assert!(
        labels.is_empty(),
        "one commit SHA attributed to multiple sessions is ambiguous across clone roots: {labels:?}"
    );
}

#[test]
fn fanout_taints_conversation_even_when_it_has_a_unique_old_commit() {
    let (repo, released, _unreleased) = release_repo();
    let storage = Storage::open_memory().unwrap();
    link(&storage, repo.path(), "fanout-a", "session-a", &released[5]);
    link(
        &storage,
        repo.path(),
        "unique-old",
        "session-a",
        &released[0],
    );
    link(&storage, repo.path(), "fanout-b", "session-b", &released[5]);

    refresh_ancestry_cache_at(&storage, &Utc::now().to_rfc3339()).unwrap();
    let labels = storage
        .ancestry_labels_for_conversations(&["session-a".into(), "session-b".into()])
        .unwrap();

    assert!(
        labels.is_empty(),
        "fan-out evidence invalidates every touched conversation: {labels:?}"
    );
}

#[test]
fn unreleased_label_is_bit_identical_to_wall_clock_only() {
    let (repo, _released, unreleased) = release_repo();
    let storage = Storage::open_memory().unwrap();
    link(&storage, repo.path(), "fresh", "session-fresh", &unreleased);
    refresh_ancestry_cache_at(&storage, &Utc::now().to_rfc3339()).unwrap();
    let labels = storage
        .ancestry_labels_for_conversations(&["session-fresh".into()])
        .unwrap();

    let now = Utc.with_ymd_and_hms(2026, 8, 6, 12, 0, 0).unwrap();
    let timestamp = now - Duration::days(30);
    let config = DecayConfig::for_search();
    let current = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
    let ancestry = decay::apply_tad_with_release_ancestry(
        0.9,
        &timestamp,
        &now,
        &[],
        &config,
        labels["session-fresh"].releases_behind_for_decay(),
    );

    assert_eq!(ancestry.to_bits(), current.to_bits());
}

#[test]
fn ancestry_migration_is_idempotent_and_preserves_rows() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    migrations::run(&conn).unwrap();
    conn.execute(
        "INSERT INTO conversation_ancestry_cache
         (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
         VALUES ('session-1', 'shipped', 'v1.0.0', 3, '/repo', '2026-08-06T12:00:00Z')",
        [],
    )
    .unwrap();

    migrations::run(&conn).unwrap();
    let row: (String, String, i64) = conn
        .query_row(
            "SELECT state, release_tag, releases_behind
             FROM conversation_ancestry_cache WHERE conversation_id = 'session-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("shipped".into(), "v1.0.0".into(), 3));
}

#[test]
fn stale_ancestry_rows_are_not_readable() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("ancestry.sqlite3");
    let storage = Storage::open(&db_path).unwrap();
    let stale = (Utc::now() - Duration::hours(25)).to_rfc3339();
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute(
        "INSERT INTO conversation_ancestry_cache
         (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
         VALUES ('session-stale', 'shipped', 'v1.0.0', 5, '/repo', ?1)",
        [&stale],
    )
    .unwrap();

    let labels = storage
        .ancestry_labels_for_conversations(&["session-stale".into()])
        .unwrap();
    assert!(labels.is_empty(), "stale labels must fail open: {labels:?}");
}

#[test]
fn ancestry_cache_ttl_and_future_skew_boundaries_fail_open() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("ancestry-boundaries.sqlite3");
    let storage = Storage::open(&db_path).unwrap();
    let now = Utc::now();
    let cases = [
        (
            "ttl-inside",
            now - Duration::hours(3) + Duration::minutes(1),
        ),
        (
            "ttl-outside",
            now - Duration::hours(3) - Duration::minutes(1),
        ),
        (
            "future-inside",
            now + Duration::minutes(5) - Duration::seconds(1),
        ),
        (
            "future-outside",
            now + Duration::minutes(5) + Duration::seconds(1),
        ),
    ];
    let conn = rusqlite::Connection::open(db_path).unwrap();
    for (conversation_id, refreshed_at) in cases {
        conn.execute(
            "INSERT INTO conversation_ancestry_cache
             (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
             VALUES (?1, 'shipped', 'v1.0.0', 5, '/repo', ?2)",
            rusqlite::params![conversation_id, refreshed_at.to_rfc3339()],
        )
        .unwrap();
    }

    let labels = storage
        .ancestry_labels_for_conversations(
            &cases
                .iter()
                .map(|(conversation_id, _)| (*conversation_id).to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();

    assert!(labels.contains_key("ttl-inside"));
    assert!(!labels.contains_key("ttl-outside"));
    assert!(labels.contains_key("future-inside"));
    assert!(!labels.contains_key("future-outside"));
}
