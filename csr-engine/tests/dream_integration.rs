//! Integration test: v10 "dreaming" (`csr_engine::dream::run_dream`) against
//! a REAL git repository and a REAL `csr-engine` `Storage` — end to end
//! (HEAD stamp-spans prerequisite + successor join + verdict persistence),
//! not just the pure join logic (see `dream`'s own unit tests for that).
//!
//! Scenario: a function is defined, then edited twice (three commits), then
//! the second edit is reverted (a fourth, `git revert` commit) — the exact
//! shape the task spec calls for: "3 commits (function edited twice) ...
//! revert commit ... dream emits anchor_reinstated".

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use csr_engine::embeddings::EmbeddingEngine;
use csr_engine::engine::Engine;
use csr_engine::extraction::codegraph::node_id;
use csr_engine::search::SearchEngine;
use csr_engine::storage::codegraph::NodeRow;
use csr_engine::storage::witness_verdicts::VerdictKind;
use csr_engine::storage::Storage;

/// `git -C <repo>` with the calling process's own `GIT_*` env stripped —
/// same reasoning as `import::backfill`'s `git_in`: this suite may itself
/// run under a git hook that exports `GIT_DIR`, which would otherwise
/// silently redirect these commands at the REAL repository.
fn git_in(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("-C").arg(repo);
    cmd
}

fn init_git_repo(repo: &Path) -> bool {
    let git = |args: &[&str]| git_in(repo).args(args).status();
    if git(&["init", "-q", "-b", "main"])
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        return false; // git unavailable — fail-soft skip, matches import::backfill precedent.
    }
    git(&["config", "user.email", "dream-integration@csr.invalid"]).unwrap();
    git(&["config", "user.name", "CSR Dream Integration Test"]).unwrap();
    git(&["config", "commit.gpgsign", "false"]).unwrap();
    true
}

fn git_head(repo: &Path) -> String {
    String::from_utf8(
        git_in(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

fn commit_all(repo: &Path, msg: &str) -> String {
    assert!(git_in(repo).args(["add", "-A"]).status().unwrap().success());
    assert!(git_in(repo)
        .args(["commit", "-q", "-m", msg])
        .status()
        .unwrap()
        .success());
    git_head(repo)
}

fn test_engine(db_path: &Path) -> Engine {
    let storage = Arc::new(Storage::open(db_path).unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let search = Arc::new(tokio::sync::RwLock::new(SearchEngine::new(16)));
    Engine::from_parts(
        storage,
        embeddings,
        search,
        db_path.parent().unwrap().to_path_buf(),
    )
}

#[test]
fn dream_end_to_end_supersession_rerun_idempotency_and_reinstatement() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    if !init_git_repo(&repo) {
        eprintln!("git unavailable — skipping dream_end_to_end integration test");
        return;
    }

    let file: PathBuf = repo.join("lib.rs");
    let write_body = |n: &str| {
        std::fs::write(&file, format!("fn foo() {{\n    {n}\n}}\n")).unwrap();
    };

    // Commit 1: initial definition. Commit 2/3: two edits to the SAME
    // function, same line count (so the node's span never needs updating).
    write_body("1");
    let c1 = commit_all(&repo, "initial foo");
    write_body("2");
    let c2 = commit_all(&repo, "edit foo (1)");
    write_body("3");
    let c3 = commit_all(&repo, "edit foo (2)");

    let db_path = tmp.path().join("csr.db");
    let engine = test_engine(&db_path);
    let file_str = file.to_string_lossy().to_string();
    let repo_str = repo.to_string_lossy().to_string();

    // Seed the one `code_nodes` row `stamp_spans_into` (dream's own HEAD
    // prerequisite) needs to know WHICH span to stamp — the historical
    // `--at` calls below re-derive spans directly from each commit's blob
    // and don't need this, but the live-HEAD pass does.
    engine
        .storage()
        .upsert_code_node(&NodeRow {
            id: node_id("proj", &file_str, "function", "foo"),
            repo: "proj".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "rust".into(),
            kind: "function".into(),
            name: "foo".into(),
            fqname: String::new(),
            body_hash: String::new(),
            span_start: 0,
            span_end: 2,
            first_conv_id: "conv-1".into(),
            last_conv_id: "conv-1".into(),
            last_session_id: "sess-1".into(),
            repo_root: Some(repo_str.clone()),
            name_only: false,
            attribution: String::new(),
        })
        .unwrap();

    // Historical witnesses for the two OLDER commits (c1, c2) — only the
    // live-HEAD pass inside `run_dream` can capture whatever commit is
    // currently checked out (c3, right now).
    let s1 =
        csr_engine::import::backfill::backfill_stamp_spans_at(&engine, &c1, Some(&repo_str), false)
            .unwrap();
    assert!(s1.spans_stamped >= 1, "commit1 span must stamp: {s1:?}");
    let s2 =
        csr_engine::import::backfill::backfill_stamp_spans_at(&engine, &c2, Some(&repo_str), false)
            .unwrap();
    assert!(s2.spans_stamped >= 1, "commit2 span must stamp: {s2:?}");

    let witness_id_at = |oid: &str| -> i64 {
        engine
            .storage()
            .witnesses_for_file("proj", &file_str)
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some(oid))
            .unwrap_or_else(|| panic!("no witness_ledger row for {oid}"))
            .id
    };

    // ── Run 0: DRY RUN at HEAD = c3 — H5 semantics: the prerequisite
    // stamp-spans pass runs FOR REAL (the c3 witness row must appear), but
    // zero verdict rows are written, and the verdict computation must be
    // identical to the real run that follows. ──
    let dry = csr_engine::dream::run_dream(&engine, None, true).unwrap();
    assert!(
        dry.stamp_spans.spans_stamped >= 1,
        "dry-run must still stamp HEAD spans for real: {dry:?}"
    );
    let w1_id = witness_id_at(&c1); // c3's row exists too — witness_id_at panics if not.
    let w2_id = witness_id_at(&c2);
    let _w3_id = witness_id_at(&c3);
    assert!(
        engine
            .storage()
            .latest_witness_verdict(w1_id)
            .unwrap()
            .is_none(),
        "dry-run must write zero verdict rows"
    );

    // ── Run 1: dream at HEAD = c3 ──
    let run1 = csr_engine::dream::run_dream(&engine, None, false).unwrap();
    assert!(
        run1.superseded >= 2,
        "both commit1 and commit2 witnesses must be superseded by commit3: {run1:?}"
    );
    assert_eq!(
        (
            run1.superseded,
            run1.obsolete,
            run1.reinstated,
            run1.events_written
        ),
        (
            dry.superseded,
            dry.obsolete,
            dry.reinstated,
            dry.events_written
        ),
        "dry run and real run must produce identical verdict computations"
    );
    let v1 = engine
        .storage()
        .latest_witness_verdict(w1_id)
        .unwrap()
        .unwrap();
    assert_eq!(v1.verdict, VerdictKind::SupersededBy);
    assert_eq!(
        v1.receipt_oid.as_deref(),
        Some(c3.as_str()),
        "commit1's successor must be the live HEAD (c3) that ran the join"
    );
    let v2_run1 = engine
        .storage()
        .latest_witness_verdict(w2_id)
        .unwrap()
        .unwrap();
    assert_eq!(v2_run1.verdict, VerdictKind::SupersededBy);

    // ── Run 2: rerun at the SAME HEAD (c3) — must write nothing new ──
    let run2 = csr_engine::dream::run_dream(&engine, None, false).unwrap();
    assert_eq!(
        run2.events_written, 0,
        "an unchanged HEAD with unchanged conclusions must write nothing: {run2:?}"
    );
    // The witness's own verdict is unchanged too — still the run1 conclusion.
    let v1_after_rerun = engine
        .storage()
        .latest_witness_verdict(w1_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        v1_after_rerun, v1,
        "rerun at an unchanged HEAD must not alter the recorded verdict"
    );

    // ── Revert commit3 (undoes edit #2 — content reverts to commit2's) ──
    assert!(git_in(&repo)
        .args(["revert", "--no-edit", "HEAD"])
        .status()
        .unwrap()
        .success());
    let c4 = git_head(&repo);
    assert_ne!(c4, c3, "revert must produce a new commit");

    let run3 = csr_engine::dream::run_dream(&engine, None, false).unwrap();
    assert!(
        run3.reinstated >= 1,
        "commit2's witness must be reinstated once HEAD's content matches it again: {run3:?}"
    );
    let v2_after_revert = engine
        .storage()
        .latest_witness_verdict(w2_id)
        .unwrap()
        .unwrap();
    assert_eq!(v2_after_revert.verdict, VerdictKind::AnchorReinstated);
    assert_eq!(v2_after_revert.receipt_oid.as_deref(), Some(c4.as_str()));
}
