use super::*;
use crate::storage::Storage;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Minimal temp-git-repo helper, self-contained (no dependency on
/// `codewitness`'s own test-only `TempRepo`, which lives in a different
/// crate and isn't exported) — same spirit as
/// `import::backfill`'s existing `init_git_repo`/`git_in` test helpers.
struct TestRepo {
    path: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "csr-dream-test-{}-{n}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create temp repo dir");
        let repo = Self { path };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "dream-test@csr.invalid"]);
        repo.git(&["config", "user.name", "CSR Dream Test"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo
    }

    fn git(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
        let out = cmd
            .current_dir(&self.path)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.path.join(rel), content).expect("write fixture file");
    }

    fn commit(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
        self.head()
    }

    fn checkout_new(&self, name: &str) {
        self.git(&["checkout", "-q", "-b", name]);
    }

    fn checkout(&self, name: &str) {
        self.git(&["checkout", "-q", name]);
    }

    /// `git merge --no-ff` of `branch` into the current branch; `extra`
    /// is spliced in before the branch (e.g. `-s ours`). Returns the
    /// merge commit.
    fn merge_no_ff(&self, branch: &str, extra: &[&str]) -> String {
        let mut args = vec!["merge", "--no-ff", "-q", "-m", "merge"];
        args.extend_from_slice(extra);
        args.push(branch);
        self.git(&args);
        self.head()
    }

    fn head(&self) -> String {
        String::from_utf8(self.git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    /// Absolute path to the fixture file every test in this module
    /// writes to (`lib.rs`) — the correct `WitnessLedgerRow.file` value.
    /// The repo ROOT itself is deliberately never used there:
    /// `repo_root_for_file`'s fallback resolution takes the FILE's
    /// containing directory, so passing the repo root itself would
    /// resolve one directory too high and never find the repo.
    fn file_path(&self) -> String {
        self.path.join("lib.rs").to_string_lossy().to_string()
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn open_storage() -> Storage {
    Storage::open_memory().unwrap()
}

fn ledger_row(project: &str, file: &str, at_oid: &str, stamp: &str) -> WitnessLedgerRow {
    WitnessLedgerRow {
        id: 0,
        project: project.into(),
        file: file.into(),
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

/// Parseable Rust source defining `foo` whose body is `body`.
fn foo_src(body: &str) -> String {
    format!("fn foo() {{\n    {body}\n}}\n")
}

fn run_join(storage: &Storage, dry_run: bool) -> DreamStats {
    let mut stats = DreamStats::default();
    storage
        .with_connection(|conn| dream_join(conn, None, dry_run, &mut stats))
        .unwrap();
    stats
}

#[test]
fn single_at_oid_anchor_has_no_history_to_judge() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("one"));
    repo.commit("c1");
    let storage = open_storage();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(
                conn,
                &ledger_row("proj", &repo.file_path(), &repo.head(), "b3:1"),
            )
        })
        .unwrap();
    let stats = run_join(&storage, false);
    assert_eq!(
        stats.anchors_considered, 0,
        "one at_oid == nothing to compare"
    );
    assert_eq!(stats.witnesses_considered, 0);
}

#[test]
fn linear_history_supersedes_the_older_witness() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2"); // live HEAD.

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(stats.anchors_considered, 1);
    assert_eq!(
        stats.witnesses_considered, 1,
        "only the non-HEAD c1 witness"
    );
    assert_eq!(stats.superseded, 1);
    assert_eq!(stats.obsolete, 0);
    assert_eq!(stats.abstained_incomparable_ancestry, 0);
    assert_eq!(stats.events_written, 1);

    let w1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    let verdict = storage.latest_witness_verdict(w1_id).unwrap().unwrap();
    assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
    assert_eq!(verdict.receipt_oid, Some(c2));
}

#[test]
fn rerun_at_unchanged_head_writes_nothing_new() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
        })
        .unwrap();

    let first = run_join(&storage, false);
    assert_eq!(first.events_written, 1);
    let second = run_join(&storage, false);
    assert_eq!(
        second.events_written, 0,
        "identical HEAD + conclusion must be a no-op"
    );
    assert_eq!(second.events_deduped, 1);
}

#[test]
fn same_conclusion_at_a_newer_head_is_deduped() {
    // c1 -> c2 supersedes the c1 witness. HEAD then moves forward to c3
    // (content changed again): c1 is still superseded, now with a
    // different successor/receipt/observed HEAD. Same verdict kind, so
    // no new event for c1; c2's witness is genuinely newly superseded.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
        })
        .unwrap();
    let first = run_join(&storage, false);
    assert_eq!(first.events_written, 1);

    repo.write("lib.rs", &foo_src("3"));
    let c3 = repo.commit("c3");
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:3"))
        })
        .unwrap();
    let second = run_join(&storage, false);
    assert_eq!(second.superseded, 2, "{second:?}");
    assert_eq!(
        second.events_written, 1,
        "only c2's witness is new: {second:?}"
    );
    assert_eq!(second.events_deduped, 1, "{second:?}");
    let n: i64 = storage
        .with_connection(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn witness_with_only_sub_floor_events_is_judged_fresh_by_the_join() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
        })
        .unwrap();
    assert_eq!(run_join(&storage, false).events_written, 1);
    let deduped = run_join(&storage, false);
    assert_eq!(deduped.events_deduped, 1, "live event dedupes: {deduped:?}");

    // Retire every event so far behind the v2 floor.
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE meta SET value = (SELECT CAST(MAX(id) AS TEXT) FROM witness_verdicts)
                     WHERE key = 'witness_verdicts_v2_floor'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let fresh = run_join(&storage, false);
    assert_eq!(
        fresh.events_written, 1,
        "no live event: the witness is judged fresh and re-minted: {fresh:?}"
    );
    assert_eq!(fresh.events_deduped, 0);
    assert_eq!(fresh.head_not_forward, 0);
}

#[test]
fn dry_run_computes_but_writes_no_verdicts_and_matches_the_real_run() {
    // H5: `--dry-run` suppresses ONLY verdict insertion. A dry run and
    // the real run that follows it must produce IDENTICAL verdict
    // computations, and the dry run must leave zero verdict rows.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
        })
        .unwrap();

    let dry = run_join(&storage, true);
    assert_eq!(dry.superseded, 1);
    assert_eq!(
        dry.events_written, 1,
        "dry-run still reports what WOULD be written"
    );

    let w1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    assert!(
        storage.latest_witness_verdict(w1_id).unwrap().is_none(),
        "dry-run must not actually write a verdict event"
    );

    let real = run_join(&storage, false);
    assert_eq!(
        real, dry,
        "the real run must compute exactly what the dry run reported"
    );
    assert!(
        storage.latest_witness_verdict(w1_id).unwrap().is_some(),
        "the real run actually persists the event"
    );
}

#[test]
fn diverged_branches_supersede_the_ancestor_and_abstain_the_sibling() {
    // Two branches off a common ancestor c1, no merge: c1 -> c_main
    // (live HEAD, on `main`) and c1 -> c_feature (a sibling branch,
    // never merged). The join must supersede the c1 witness (real
    // ancestor of HEAD) and ABSTAIN on the c_feature witness
    // (Incomparable ancestry against HEAD) — never guess either way.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");

    repo.checkout_new("feature");
    repo.write("lib.rs", &foo_src("feature-branch"));
    let c_feature = repo.commit("c_feature");

    repo.checkout("main");
    repo.write("lib.rs", &foo_src("main-branch"));
    let c_main = repo.commit("c_main"); // live HEAD stays on main.

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            witness_ledger::insert_witness(
                conn,
                &ledger_row("proj", &file, &c_feature, "b3:feature"),
            )?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c_main, "b3:main"))
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(stats.anchors_considered, 1);
    assert_eq!(
        stats.witnesses_considered, 2,
        "c1 and c_feature, excluding HEAD (c_main)"
    );
    assert_eq!(stats.superseded, 1, "c1 is a real ancestor of HEAD");
    assert_eq!(
        stats.abstained_incomparable_ancestry, 1,
        "c_feature has no ancestry relationship with HEAD"
    );
    assert_eq!(stats.obsolete, 0);

    let rows = storage.witnesses_for_file("proj", &file).unwrap();
    let c1_id = rows
        .iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    let feature_id = rows
        .iter()
        .find(|r| r.at_oid.as_deref() == Some(c_feature.as_str()))
        .unwrap()
        .id;

    let c1_verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
    assert_eq!(c1_verdict.verdict, VerdictKind::SupersededBy);
    assert_eq!(c1_verdict.receipt_oid, Some(c_main));

    assert!(
        storage
            .latest_witness_verdict(feature_id)
            .unwrap()
            .is_none(),
        "incomparable ancestry must abstain — no event, never a guess"
    );
}

#[test]
fn revert_to_a_prior_stamp_reinstates_it() {
    // A -> B -> A: commit c1 (content "A"), c2 (content "B", superseded
    // marker), then c3 reverts back to content "A" — the SAME stamp as
    // c1. A fresh dream pass (simulating a re-run of `stamp_spans_into`
    // after HEAD moved to c3) must reinstate c1's witness rather than
    // leaving it superseded forever.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("A"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("B"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:B"))
        })
        .unwrap();
    let first = run_join(&storage, false);
    assert_eq!(first.superseded, 1, "c1 must be superseded by c2 first");

    // HEAD moves on: c3 reverts the content back to "A" (same stamp as
    // c1) — a fresh witness_ledger row for c3, as `stamp_spans_into`
    // would mint after re-stamping at the new HEAD.
    repo.write("lib.rs", &foo_src("A"));
    let c3 = repo.commit("c3 (revert)");
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:A"))
        })
        .unwrap();

    let second = run_join(&storage, false);
    assert_eq!(second.reinstated, 1, "c1's stamp matches HEAD (c3) again");

    let c1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
    assert_eq!(verdict.verdict, VerdictKind::AnchorReinstated);
    assert_eq!(verdict.receipt_oid, Some(c3));
}

#[test]
fn symbol_absent_at_head_becomes_obsolete() {
    // The witness's file exists (so the repo/HEAD resolve), but no row
    // in the ledger claims the current HEAD oid for this symbol — the
    // scenario `stamp_spans_into` produces when a span vanishes
    // (`skipped_stamp_error`/`skipped_span_out_of_range`): no fresh row
    // is minted at HEAD, so `H` is absent. Only witnesses that are
    // PROPER ANCESTORS of the observed HEAD become obsolete (H6): the
    // unknown-oid row (not in the object database at all) cannot prove
    // ancestry and must abstain, never guess.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "// foo removed\n");
    let _c2 = repo.commit("c2 (foo gone)"); // live HEAD — deliberately NOT in the ledger.

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            // A second, unknown at_oid so the anchor has >1 distinct
            // at_oid (the join's population gate) without ever matching
            // live HEAD.
            witness_ledger::insert_witness(
                conn,
                &ledger_row(
                    "proj",
                    &file,
                    "0000000000000000000000000000000000000000",
                    "b3:0",
                ),
            )
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(
        stats.obsolete, 1,
        "only c1 (a proven ancestor of HEAD) becomes obsolete"
    );
    assert_eq!(
        stats.head_behind_witness, 1,
        "the unknown-oid row's ancestry is unprovable — abstain"
    );
    assert_eq!(stats.superseded, 0);
}

#[test]
fn checkout_of_an_ancestor_head_abstains_instead_of_obsoleting() {
    // H6: commits c1 -> c2 -> c3; the ledger has rows at c2 and c3;
    // then a HISTORICAL commit (c1) is checked out. H is absent (no
    // ledger row at c1), but HEAD is an ANCESTOR of both witnesses —
    // the run is looking backwards in time and can prove nothing about
    // them. Both must abstain (`head_behind_witness`), never be
    // declared obsolete.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");
    repo.write("lib.rs", &foo_src("3"));
    let c3 = repo.commit("c3");
    repo.git(&["checkout", "-q", &c1]); // detached HEAD at the ancestor.

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:3"))
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(
        stats.obsolete, 0,
        "a HEAD behind the witnesses proves nothing"
    );
    assert_eq!(stats.head_behind_witness, 2);
    assert_eq!(stats.events_written, 0);
}

#[test]
fn divergent_branch_receipt_never_certifies_supersession() {
    // H4: main is c1 -> c_main (live HEAD); a feature branch off c1
    // carries c_feat with the SAME content (same stamp) as c_main.
    // c_feat is a causal descendant of c1, so without the HEAD-path
    // check it would be picked as c1's successor (it was inserted
    // first) — a receipt minted on a divergent, never-merged branch.
    // The successor must be c_main (ancestor-or-equal of HEAD).
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");

    repo.checkout_new("feature");
    repo.write("lib.rs", &foo_src("2"));
    let c_feat = repo.commit("c_feat");

    repo.checkout("main");
    repo.write("lib.rs", &foo_src("2")); // same content as the feature branch.
    let c_main = repo.commit("c_main"); // live HEAD.

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            // c_feat inserted BEFORE c_main so a first-match-wins scan
            // without the HEAD-path check would pick it.
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c_feat, "b3:2"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c_main, "b3:2"))
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 1, "c1 must be superseded: {stats:?}");

    let c1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
    assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
    assert_eq!(
        verdict.receipt_oid,
        Some(c_main),
        "the receipt must come from the HEAD path, never the divergent branch"
    );
}

#[test]
fn checkout_of_matching_older_head_does_not_reinstate() {
    // Forward-only judgment: c1 (content A) -> c2 (content B). Dream at
    // HEAD=c2 marks the c1 witness superseded. Then c1 itself is checked
    // out (HEAD moves BACKWARDS). The c1 witness's stamp equals the HEAD
    // stamp, but HEAD is not a descendant of where the superseded
    // verdict was observed (c2), so the run abstains: no reinstatement,
    // the verdict stays as drawn.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("A"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("B"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:B"))
        })
        .unwrap();

    let first = run_join(&storage, false);
    assert_eq!(first.superseded, 1, "c1 superseded at HEAD=c2 first");

    repo.git(&["checkout", "-q", &c1]); // HEAD moves backwards to c1.
    let second = run_join(&storage, false);
    assert_eq!(second.reinstated, 0, "{second:?}");
    assert_eq!(
        second.head_not_forward, 1,
        "the c1 reinstatement is withheld: HEAD is behind the observed HEAD: {second:?}"
    );
    assert_eq!(second.events_written, 0);

    let c1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
    assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
    assert_eq!(verdict.receipt_oid, Some(c2));
}

#[test]
fn branch_checkout_does_not_flap_obsolete_into_reinstated() {
    // c0 -> c1 on main (symbol present, content A). `dev` branches off
    // c1 and deletes the symbol (c2). Dreaming on `dev` marks the
    // witnesses obsolete. Checking out `main` (symbol present with the
    // identical stamp) must NOT reinstate: main's HEAD (c1) is an
    // ancestor of the observed HEAD (c2), not a descendant.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("0"));
    let c0 = repo.commit("c0");
    repo.write("lib.rs", &foo_src("A"));
    let c1 = repo.commit("c1");
    repo.checkout_new("dev");
    repo.write("lib.rs", "// foo removed\n");
    let _c2 = repo.commit("c2 (symbol deleted)");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c0, "b3:0"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))
        })
        .unwrap();

    let on_dev = run_join(&storage, false);
    assert_eq!(on_dev.obsolete, 2, "{on_dev:?}");
    assert_eq!(on_dev.events_written, 2);

    repo.checkout("main");
    let on_main = run_join(&storage, false);
    assert_eq!(on_main.reinstated, 0, "{on_main:?}");
    assert!(on_main.head_not_forward >= 1, "{on_main:?}");
    assert_eq!(on_main.events_written, 0, "{on_main:?}");

    let c1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    assert_eq!(
        storage
            .latest_witness_verdict(c1_id)
            .unwrap()
            .unwrap()
            .verdict,
        VerdictKind::AnchorObsolete
    );
}

#[test]
fn restore_in_a_descendant_commit_reinstates() {
    // c0 -> c1 (content A) -> c2 (symbol deleted) -> c3 (A restored,
    // identical stamp). Dream at c2: obsolete. Dream at c3, a
    // descendant of the observed HEAD c2: anchor_reinstated fires.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("0"));
    let c0 = repo.commit("c0");
    repo.write("lib.rs", &foo_src("A"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "// foo removed\n");
    let _c2 = repo.commit("c2 (symbol deleted)");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c0, "b3:0"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))
        })
        .unwrap();
    let first = run_join(&storage, false);
    assert_eq!(first.obsolete, 2, "{first:?}");

    repo.write("lib.rs", &foo_src("A"));
    let c3 = repo.commit("c3 (restored)");
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:A"))
        })
        .unwrap();
    let second = run_join(&storage, false);
    assert_eq!(second.reinstated, 1, "{second:?}");
    assert_eq!(second.head_not_forward, 0, "{second:?}");

    let c1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    let verdict = storage.latest_witness_verdict(c1_id).unwrap().unwrap();
    assert_eq!(verdict.verdict, VerdictKind::AnchorReinstated);
    assert_eq!(verdict.receipt_oid, Some(c3));
}

#[test]
fn unparsable_observed_head_abstains_as_head_not_forward() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:1"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:2"))
        })
        .unwrap();
    let w1_id = storage
        .witnesses_for_file("proj", &file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(c1.as_str()))
        .unwrap()
        .id;
    // A prior (obsolete) event whose observed HEAD is not an oid.
    storage
        .with_connection(|conn| {
            witness_verdicts::insert_verdict_if_changed(
                conn,
                &WitnessVerdictRow {
                    witness_id: w1_id,
                    verdict: VerdictKind::AnchorObsolete,
                    successor_witness_id: None,
                    receipt_oid: None,
                    observed_head_oid: "not-an-oid".into(),
                },
            )
        })
        .unwrap();
    let stats = run_join(&storage, false);
    assert_eq!(stats.head_not_forward, 1, "{stats:?}");
    assert_eq!(stats.events_written, 0);
}

#[test]
fn head_row_under_another_project_keeps_same_content_witness_intact() {
    // The stamp pass assigns one project per file, so the same absolute
    // file+symbol is stamped under P1 at c1 and later under P2 at c2
    // (HEAD). Anchor identity is (file, symbol): with identical content
    // the P1 witness is intact, never `anchor_obsolete`.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("A"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &format!("{}// trailer\n", foo_src("A")));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("p1", &file, &c1, "b3:A"))?;
            witness_ledger::insert_witness(conn, &ledger_row("p2", &file, &c2, "b3:A"))
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(stats.anchors_considered, 1, "one (file, symbol) anchor");
    assert_eq!(stats.obsolete, 0, "{stats:?}");
    assert_eq!(stats.superseded, 0, "{stats:?}");
    assert_eq!(stats.intact, 1, "{stats:?}");
    assert_eq!(stats.events_written, 0);
}

#[test]
fn head_row_under_another_project_supersedes_changed_content() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("A"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("B"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("p1", &file, &c1, "b3:A"))?;
            witness_ledger::insert_witness(conn, &ledger_row("p2", &file, &c2, "b3:B"))
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(stats.obsolete, 0, "{stats:?}");
    assert_eq!(stats.superseded, 1, "{stats:?}");

    let p1_id = storage
        .witnesses_for_file("p1", &file)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id;
    let p2_id = storage
        .witnesses_for_file("p2", &file)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .id;
    let verdict = storage.latest_witness_verdict(p1_id).unwrap().unwrap();
    assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
    assert_eq!(verdict.successor_witness_id, Some(p2_id));
    assert_eq!(verdict.receipt_oid, Some(c2));
}

#[test]
fn near_revert_supersedes_rather_than_abstaining() {
    // C3 contract: A -> B -> A' (A' close to but NOT stamp-equal to A)
    // is legitimate supersession — content demonstrably changed and a
    // valid HEAD-path successor exists. Near-revert abstention applies
    // ONLY to reinstatement (exact stamp equality), never here.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("A"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("B"));
    let c2 = repo.commit("c2");
    repo.write("lib.rs", &foo_src("A2")); // A-prime: almost, but not exactly, A.
    let c3 = repo.commit("c3");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c1, "b3:A"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c2, "b3:B"))?;
            witness_ledger::insert_witness(conn, &ledger_row("proj", &file, &c3, "b3:Aprime"))
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(
        stats.superseded, 2,
        "both c1 (A) and c2 (B) are superseded by c3 (A'): {stats:?}"
    );
    assert_eq!(
        stats.reinstated, 0,
        "A' is not stamp-equal — no reinstatement"
    );
    assert_eq!(stats.abstained_no_successor, 0);
    assert_eq!(stats.abstained_incomparable_ancestry, 0);
}
// ---- true receipts (item 2) -------------------------------------------

/// `foo` plus an unrelated sibling, so "unrelated edit" commits can
/// touch the same file without touching `foo`.
fn foo_with(body: &str, tail: &str) -> String {
    format!("{}{tail}", foo_src(body))
}

fn insert_rows(storage: &Storage, file: &str, rows: &[(&str, &str)]) {
    storage
        .with_connection(|conn| {
            for (oid, stamp) in rows {
                witness_ledger::insert_witness(conn, &ledger_row("proj", file, oid, stamp))?;
            }
            Ok(())
        })
        .unwrap();
}

fn witness_at(storage: &Storage, file: &str, oid: &str) -> i64 {
    storage
        .witnesses_for_file("proj", file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(oid))
        .unwrap()
        .id
}

fn latest(storage: &Storage, file: &str, oid: &str) -> Option<WitnessVerdictRow> {
    storage
        .latest_witness_verdict(witness_at(storage, file, oid))
        .unwrap()
}

#[test]
fn obsolete_receipt_is_the_commit_that_deleted_the_symbol() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1 adds foo");
    repo.write("lib.rs", &foo_with("1", "fn other() {}\n"));
    let c2 = repo.commit("c2 unrelated edit, same file");
    repo.write("lib.rs", "fn other() {}\n");
    let c3 = repo.commit("c3 deletes foo");
    repo.write("lib.rs", "fn other() {}\nfn another() {}\n");
    let c4 = repo.commit("c4 unrelated");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:1")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.obsolete, 2, "{stats:?}");
    assert_eq!(stats.receipts_found, 2, "{stats:?}");
    assert_eq!(stats.events_written, 2, "{stats:?}");
    for oid in [&c1, &c2] {
        let v = latest(&storage, &file, oid).unwrap();
        assert_eq!(v.verdict, VerdictKind::AnchorObsolete);
        assert_eq!(v.receipt_oid.as_deref(), Some(c3.as_str()), "not HEAD {c4}");
        assert_eq!(v.observed_head_oid, c4);
    }
}

#[test]
fn superseded_receipt_is_the_commit_that_changed_the_symbol() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2 changes foo");
    repo.write("lib.rs", &foo_with("2", "fn other() {}\n"));
    let c3 = repo.commit("c3 unrelated");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(
        &storage,
        &file,
        &[(&c1, "b3:1"), (&c2, "b3:2"), (&c3, "b3:2")],
    );

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.intact, 1, "{stats:?}");
    let v = latest(&storage, &file, &c1).unwrap();
    assert_eq!(v.verdict, VerdictKind::SupersededBy);
    assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
    assert_eq!(
        v.successor_witness_id,
        Some(witness_at(&storage, &file, &c2))
    );
}

#[test]
fn whole_file_witness_receipt_is_the_commit_that_changed_the_file() {
    let repo = TestRepo::new();
    repo.write("lib.rs", "one\n");
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "two\n");
    let c2 = repo.commit("c2");
    repo.write("lib.rs", "two\n\n");
    let c3 = repo.commit("c3 blank line only");

    let storage = open_storage();
    let file = repo.file_path();
    storage
        .with_connection(|conn| {
            for (oid, stamp) in [(&c1, "b3:1"), (&c2, "b3:2"), (&c3, "b3:3")] {
                let mut row = ledger_row("proj", &file, oid, stamp);
                row.symbol = None;
                row.span_start = None;
                row.span_end = None;
                witness_ledger::insert_witness(conn, &row)?;
            }
            Ok(())
        })
        .unwrap();

    let stats = run_join(&storage, false);
    // c1 -> c3: content changed, receipt c2. c2 -> c3: whitespace only.
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.abstained_same_content, 1, "{stats:?}");
    let v = latest(&storage, &file, &c1).unwrap();
    assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
    assert!(latest(&storage, &file, &c2).is_none());
}

#[test]
fn whitespace_only_reformat_is_not_a_supersession() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "fn foo() {\n\t1   \n}\n\n");
    let c2 = repo.commit("c2 reformat only");

    let storage = open_storage();
    let file = repo.file_path();
    // Raw stamps differ (they are raw), the normalized content does not.
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 0, "{stats:?}");
    assert_eq!(stats.abstained_same_content, 1, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert!(latest(&storage, &file, &c1).is_none());
}

#[test]
fn reinstated_receipt_is_the_commit_that_restored_the_symbol() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1 foo");
    repo.write("lib.rs", &foo_with("1", "// pad\n"));
    let c1b = repo.commit("c1b unrelated");
    repo.write("lib.rs", "// gone\n");
    let c2 = repo.commit("c2 deletes foo");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c1b, "b3:1")]);
    let first = run_join(&storage, false);
    assert_eq!(first.obsolete, 2, "{first:?}");
    assert_eq!(
        latest(&storage, &file, &c1).unwrap().receipt_oid.as_deref(),
        Some(c2.as_str())
    );

    repo.write("lib.rs", "// gone\n// still gone\n");
    let _c3 = repo.commit("c3 unrelated");
    repo.write("lib.rs", &foo_with("1", "// back\n"));
    let c4 = repo.commit("c4 restores identical foo");
    repo.write("lib.rs", &foo_with("1", "// back\n// later\n"));
    let c5 = repo.commit("c5 unrelated");
    insert_rows(&storage, &file, &[(&c5, "b3:1")]);

    let second = run_join(&storage, false);
    assert_eq!(second.reinstated, 2, "{second:?}");
    assert_eq!(second.receipts_found, 2, "{second:?}");
    for oid in [&c1, &c1b] {
        let v = latest(&storage, &file, oid).unwrap();
        assert_eq!(v.verdict, VerdictKind::AnchorReinstated);
        assert_eq!(v.receipt_oid.as_deref(), Some(c4.as_str()), "not HEAD {c5}");
        assert_eq!(v.observed_head_oid, c5);
    }
}

#[test]
fn symbol_present_at_head_without_a_head_row_is_not_obsolete() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_with("1", "// pad\n"));
    let c2 = repo.commit("c2");
    repo.write("lib.rs", &foo_with("1", "// pad\n// more\n"));
    let _c3 = repo.commit("c3 (HEAD, no ledger row)");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:1")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.obsolete, 0, "{stats:?}");
    assert_eq!(stats.abstained_present_at_head, 2, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert!(latest(&storage, &file, &c1).is_none());
}

#[test]
fn unparseable_content_abstains_instead_of_guessing() {
    // "1" / "2" are not a parseable Rust symbol anywhere: state at the
    // witness's own commit is Unknown, so no event, no HEAD fallback.
    let repo = TestRepo::new();
    repo.write("lib.rs", "1");
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "2");
    let c2 = repo.commit("c2");
    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 0, "{stats:?}");
    assert_eq!(stats.abstained_unparseable, 1, "{stats:?}");
    assert_eq!(stats.abstained_not_at_witness, 0, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
}

#[test]
fn unknown_state_inside_the_range_abstains_with_no_receipt() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_with("1", "// pad\n"));
    let c1b = repo.commit("c1b");
    repo.write("lib.rs", "fn foo( {{{ ]\nlet = ;\n");
    let _c2 = repo.commit("c2 syntax error");
    repo.write("lib.rs", "// gone\n");
    let _c3 = repo.commit("c3 foo gone");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c1b, "b3:1")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.obsolete, 0, "{stats:?}");
    assert_eq!(stats.abstained_no_receipt, 2, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert_eq!(stats.receipts_found, 0, "{stats:?}");
}

#[test]
fn rerun_at_a_newer_head_runs_no_receipt_search_and_writes_nothing() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);
    let first = run_join(&storage, false);
    assert_eq!(first.events_written, 1, "{first:?}");
    assert!(first.receipt_searches >= 1, "{first:?}");

    repo.write("lib.rs", &foo_with("2", "fn other() {}\n"));
    let c3 = repo.commit("c3 unrelated");
    insert_rows(&storage, &file, &[(&c3, "b3:2")]);

    let second = run_join(&storage, false);
    assert_eq!(second.superseded, 1, "same conclusion: {second:?}");
    assert_eq!(second.events_deduped, 1, "{second:?}");
    assert_eq!(second.events_written, 0, "{second:?}");
    assert_eq!(second.receipt_searches, 0, "no git search: {second:?}");
    assert_eq!(second.receipts_found, 0, "{second:?}");
    let n: i64 = storage
        .with_connection(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(n, 1);
    // The recorded receipt is still the changing commit, untouched.
    assert_eq!(
        latest(&storage, &file, &c1).unwrap().receipt_oid.as_deref(),
        Some(c2.as_str())
    );
}

fn insert_symbol_rows(storage: &Storage, file: &str, symbol: &str, rows: &[(&str, &str)]) {
    storage
        .with_connection(|conn| {
            for (oid, stamp) in rows {
                let mut row = ledger_row("proj", file, oid, stamp);
                row.symbol = Some(symbol.into());
                witness_ledger::insert_witness(conn, &row)?;
            }
            Ok(())
        })
        .unwrap();
}

fn witness_of(storage: &Storage, file: &str, symbol: &str, oid: &str) -> i64 {
    storage
        .witnesses_for_file("proj", file)
        .unwrap()
        .into_iter()
        .find(|r| r.at_oid.as_deref() == Some(oid) && r.symbol.as_deref() == Some(symbol))
        .unwrap()
        .id
}

#[test]
fn collision_group_members_get_no_verdict_but_an_unrelated_sibling_still_does() {
    // Two `foo` definitions: the second is minted as `foo#2`. Both change
    // between c1 and c2. Neither may be judged: `foo#2` is positional,
    // and the bare `foo` heads a collision group, so which definition it
    // names is itself unproven. A uniquely named `other` is unaffected.
    let repo = TestRepo::new();
    repo.write(
        "lib.rs",
        "fn foo() {\n    1\n}\nfn foo() {\n    a\n}\nfn other() {\n    x\n}\n",
    );
    let c1 = repo.commit("c1");
    repo.write(
        "lib.rs",
        "fn foo() {\n    2\n}\nfn foo() {\n    b\n}\nfn other() {\n    y\n}\n",
    );
    let c2 = repo.commit("c2");

    let storage = open_storage();
    let file = repo.file_path();
    insert_symbol_rows(&storage, &file, "foo", &[(&c1, "b3:1"), (&c2, "b3:2")]);
    insert_symbol_rows(&storage, &file, "foo#2", &[(&c1, "b3:a"), (&c2, "b3:b")]);
    insert_symbol_rows(&storage, &file, "other", &[(&c1, "b3:x"), (&c2, "b3:y")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.abstained_positional_symbol, 1, "{stats:?}");
    assert_eq!(stats.abstained_unparseable, 1, "bare foo: {stats:?}");
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.events_written, 1, "{stats:?}");

    let other = storage
        .latest_witness_verdict(witness_of(&storage, &file, "other", &c1))
        .unwrap()
        .expect("uniquely named sibling is still judged");
    assert_eq!(other.verdict, VerdictKind::SupersededBy);
    assert_eq!(other.receipt_oid.as_deref(), Some(c2.as_str()));
    for symbol in ["foo", "foo#2"] {
        assert!(
            storage
                .latest_witness_verdict(witness_of(&storage, &file, symbol, &c1))
                .unwrap()
                .is_none(),
            "{symbol} must carry no verdict"
        );
        assert!(storage
            .latest_witness_verdict(witness_of(&storage, &file, symbol, &c2))
            .unwrap()
            .is_none());
    }
}

/// A `Foo::render` witness whose method moved to a top-level `render`.
const NESTED_RENDER: &str = "struct Foo;\nimpl Foo {\n    fn render() {\n        1\n    }\n}\n";

fn render_rows(storage: &Storage, file: &str, oids: &[&str]) {
    let rows: Vec<(&str, &str)> = oids.iter().map(|o| (*o, "b3:1")).collect();
    insert_symbol_rows(storage, file, "Foo::render", &rows);
}

#[test]
fn moved_to_top_level_is_not_obsolete_when_the_leaf_survives_at_head() {
    let repo = TestRepo::new();
    repo.write("lib.rs", NESTED_RENDER);
    let c1 = repo.commit("c1 Foo::render");
    repo.write("lib.rs", &format!("{NESTED_RENDER}// pad\n"));
    let c1b = repo.commit("c1b unrelated");
    repo.write("lib.rs", "fn render() {\n    1\n}\n");
    let _c2 = repo.commit("c2 render moved out of impl Foo");

    let storage = open_storage();
    let file = repo.file_path();
    render_rows(&storage, &file, &[&c1, &c1b]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.obsolete, 0, "{stats:?}");
    assert_eq!(stats.abstained_moved, 2, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert!(storage
        .latest_witness_verdict(witness_of(&storage, &file, "Foo::render", &c1))
        .unwrap()
        .is_none());
}

#[test]
fn moved_then_deleted_is_not_obsolete_when_the_leaf_survives_at_the_receipt() {
    let repo = TestRepo::new();
    repo.write("lib.rs", NESTED_RENDER);
    let c1 = repo.commit("c1 Foo::render");
    repo.write("lib.rs", &format!("{NESTED_RENDER}// pad\n"));
    let c1b = repo.commit("c1b unrelated");
    repo.write("lib.rs", "fn render() {\n    1\n}\n");
    let _c2 = repo.commit("c2 moved: Foo::render absent, render present");
    repo.write("lib.rs", "fn other() {}\n");
    let _c3 = repo.commit("c3 render deleted (HEAD)");

    let storage = open_storage();
    let file = repo.file_path();
    render_rows(&storage, &file, &[&c1, &c1b]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.obsolete, 0, "{stats:?}");
    assert_eq!(stats.abstained_moved, 2, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
}

#[test]
fn real_deletion_of_a_qualified_symbol_is_still_obsolete_with_the_deleting_commit() {
    let repo = TestRepo::new();
    repo.write("lib.rs", NESTED_RENDER);
    let c1 = repo.commit("c1 Foo::render");
    repo.write("lib.rs", &format!("{NESTED_RENDER}// pad\n"));
    let c1b = repo.commit("c1b unrelated");
    repo.write("lib.rs", "struct Foo;\nfn other() {}\n");
    let c2 = repo.commit("c2 deletes render for good");
    repo.write("lib.rs", "struct Foo;\nfn other() {}\nfn another() {}\n");
    let _c3 = repo.commit("c3 unrelated (HEAD)");

    let storage = open_storage();
    let file = repo.file_path();
    render_rows(&storage, &file, &[&c1, &c1b]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.abstained_moved, 0, "{stats:?}");
    assert_eq!(stats.obsolete, 2, "{stats:?}");
    let v = storage
        .latest_witness_verdict(witness_of(&storage, &file, "Foo::render", &c1))
        .unwrap()
        .unwrap();
    assert_eq!(v.verdict, VerdictKind::AnchorObsolete);
    assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
}

#[test]
fn symbol_leaf_strips_collision_suffix_and_container() {
    use super::receipt::symbol_leaf;
    assert_eq!(symbol_leaf("make_renderer.render"), "render");
    assert_eq!(symbol_leaf("Foo::bar#2"), "bar");
    assert_eq!(symbol_leaf("remove#10"), "remove");
    assert_eq!(symbol_leaf("out"), "out");
    assert_eq!(symbol_leaf("A.B::c"), "c");
    assert_eq!(symbol_leaf("weird#x"), "weird#x");
}

#[test]
fn symbol_missing_at_its_own_commit_is_not_at_witness_not_unparseable() {
    // Clean parse at both commits, but the file never contained `foo`:
    // a legacy backfill row claiming a symbol at a commit without it.
    let repo = TestRepo::new();
    repo.write("lib.rs", "fn bar() {\n    1\n}\n");
    let c1 = repo.commit("c1 only bar");
    repo.write("lib.rs", "fn bar() {\n    2\n}\n");
    let c2 = repo.commit("c2 only bar");
    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.abstained_not_at_witness, 1, "{stats:?}");
    assert_eq!(stats.abstained_unparseable, 0, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert_eq!(stats.superseded, 0, "{stats:?}");
    let text = stats.format_text(false);
    assert!(text.contains("unparseable=0"), "{text}");
    assert!(text.contains("not_at_witness=1"), "{text}");
    assert!(text.contains("moved=0"), "{text}");
    assert!(text.contains("positional_symbol=0"), "{text}");
}

#[test]
fn rechange_after_a_reinstatement_does_not_reuse_the_cancelled_receipt() {
    // A at C1, B at C2 (superseded, receipt C2), A restored at C3
    // (reinstated), B again at C4. The second supersession of the C1
    // witness is C4's work; the cancelled C2 row and commit must not be
    // picked up again.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1 A");
    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1")]);
    run_join(&storage, false);

    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2 B");
    insert_rows(&storage, &file, &[(&c2, "b3:2")]);
    run_join(&storage, false);
    let first = latest(&storage, &file, &c1).unwrap();
    assert_eq!(first.verdict, VerdictKind::SupersededBy);
    assert_eq!(first.receipt_oid.as_deref(), Some(c2.as_str()));

    repo.write("lib.rs", &foo_src("1"));
    let c3 = repo.commit("c3 A restored");
    insert_rows(&storage, &file, &[(&c3, "b3:1")]);
    run_join(&storage, false);
    let restored = latest(&storage, &file, &c1).unwrap();
    assert_eq!(restored.verdict, VerdictKind::AnchorReinstated);
    assert_eq!(restored.receipt_oid.as_deref(), Some(c3.as_str()));

    repo.write("lib.rs", &foo_src("2"));
    let c4 = repo.commit("c4 B again");
    insert_rows(&storage, &file, &[(&c4, "b3:2")]);
    let stats = run_join(&storage, false);
    assert_eq!(stats.abstained_no_receipt, 0, "{stats:?}");

    let second = latest(&storage, &file, &c1).unwrap();
    assert_eq!(second.verdict, VerdictKind::SupersededBy);
    assert_eq!(
        second.receipt_oid.as_deref(),
        Some(c4.as_str()),
        "second supersession must be C4, not the cancelled C2 {c2}"
    );
    assert_eq!(
        second.successor_witness_id,
        Some(witness_at(&storage, &file, &c4)),
        "successor must be the post-reinstatement row"
    );
}

#[test]
fn definitions_sharing_a_line_abstain_instead_of_sharing_a_stamp() {
    // `foo` and `bar` live on one line, so a change to `bar` changes
    // `foo`'s line-span stamp. `foo` itself never changed.
    let repo = TestRepo::new();
    repo.write(
        "lib.rs",
        "fn foo() -> i32 { 1 } fn bar() -> i32 { 2 }\nfn other() {}\n",
    );
    let c1 = repo.commit("c1");
    repo.write(
        "lib.rs",
        "fn foo() -> i32 { 1 } fn bar() -> i32 { 3 }\nfn other() {}\n",
    );
    let c2 = repo.commit("c2 changes bar only");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 0, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert_eq!(stats.abstained_unparseable, 1, "{stats:?}");
    assert!(latest(&storage, &file, &c1).is_none());
}

#[test]
fn a_class_with_a_method_changed_on_its_own_line_still_supersedes_the_class() {
    // Strict nesting is not an overlap: the class legitimately contains
    // the method's lines, so the method edit is a class edit.
    let repo = TestRepo::new();
    repo.write(
        "mod.py",
        "class Foo:\n    def bar(self):\n        return 1\n",
    );
    let c1 = repo.commit("c1");
    repo.write(
        "mod.py",
        "class Foo:\n    def bar(self):\n        return 2\n",
    );
    let c2 = repo.commit("c2 changes the method body");

    let storage = open_storage();
    let file = repo.path.join("mod.py").to_string_lossy().to_string();
    storage
        .with_connection(|conn| {
            for (oid, stamp) in [(&c1, "b3:1"), (&c2, "b3:2")] {
                let mut row = ledger_row("proj", &file, oid, stamp);
                row.symbol = Some("Foo".into());
                witness_ledger::insert_witness(conn, &row)?;
            }
            Ok(())
        })
        .unwrap();

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.abstained_unparseable, 0, "{stats:?}");
    let v = latest(&storage, &file, &c1).unwrap();
    assert_eq!(v.verdict, VerdictKind::SupersededBy);
    assert_eq!(v.receipt_oid.as_deref(), Some(c2.as_str()));
}

#[test]
fn same_named_definition_inserted_before_an_unchanged_one_abstains() {
    // The new `foo` takes the bare name and the untouched original
    // becomes `foo#2`: the content under the name `foo` changed, but
    // `foo` (the thing the witness is about) did not.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_with("1", "fn other() {}\n"));
    let c1 = repo.commit("c1");
    repo.write(
        "lib.rs",
        &format!("{}{}", foo_src("99"), foo_with("1", "fn other() {}\n")),
    );
    let c2 = repo.commit("c2 new foo above the original");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 0, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert_eq!(stats.abstained_unparseable, 1, "{stats:?}");
    assert!(latest(&storage, &file, &c1).is_none());
}

#[test]
fn successor_commit_without_the_symbol_does_not_certify_supersession() {
    // A ledger row claims `foo` at c2, but c2 has no `foo`.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_with("1", "fn other() {}\n"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "fn other() {}\n");
    let c2 = repo.commit("c2 foo is gone");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.abstained_successor_absent, 1, "{stats:?}");
    assert_eq!(stats.superseded, 0, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert!(latest(&storage, &file, &c1).is_none());
}

#[test]
fn supersession_receipt_skips_commits_where_the_symbol_is_absent() {
    // c2 deletes `foo`, c3 brings it back with different content. The
    // content change is c3's; c2 is a deletion, not a successor.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_with("1", "fn other() {}\n"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "fn other() {}\n");
    let _c2 = repo.commit("c2 foo gone");
    repo.write("lib.rs", &foo_with("2", "fn other() {}\n"));
    let c3 = repo.commit("c3 foo back, different");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c3, "b3:3")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.abstained_successor_absent, 0, "{stats:?}");
    let v = latest(&storage, &file, &c1).unwrap();
    assert_eq!(v.verdict, VerdictKind::SupersededBy);
    assert_eq!(v.receipt_oid.as_deref(), Some(c3.as_str()));
}

#[test]
fn unknown_state_before_the_supersession_match_abstains() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_with("1", "fn other() {}\n"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", "fn foo( {{{ ]\nlet = ;\n");
    let _c2 = repo.commit("c2 syntax error");
    repo.write("lib.rs", &foo_with("2", "fn other() {}\n"));
    let c3 = repo.commit("c3 foo changed");

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c3, "b3:3")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.abstained_no_receipt, 1, "{stats:?}");
    assert_eq!(stats.superseded, 0, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
}

/// Build a two-commit repo whose second commit (the receipt) has the given
/// `GIT_COMMITTER_DATE`, run the join, and return the stored
/// `receipt_committed_at` plus the delivered headline.
fn deliver_with_committer_date(
    committer_date: &str,
) -> (
    Option<String>,
    crate::storage::dream_delivery::DreamHeadline,
    String,
) {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    repo.git(&["add", "-A"]);
    let mut commit = Command::new("git");
    // Same scrub as `TestRepo::git`: an inherited GIT_* (a hook's
    // GIT_INDEX_FILE, say) would point this at the wrong repository.
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            commit.env_remove(&k);
        }
    }
    let committed = commit
        .current_dir(&repo.path)
        .env("GIT_COMMITTER_DATE", committer_date)
        .args(["commit", "-q", "-m", "c2 changes foo"])
        .output()
        .unwrap();
    assert!(committed.status.success(), "{committed:?}");
    let c2 = repo.head();

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);
    let stats = run_join(&storage, false);
    assert_eq!(stats.events_written, 1, "{stats:?}");

    let (receipt, committed_at): (String, Option<String>) = storage
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT receipt_oid, receipt_committed_at FROM witness_verdicts",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(receipt, c2);

    let mut headlines = storage
        .with_connection(|conn| crate::storage::dream_delivery::receipted_conclusions(conn, "proj"))
        .unwrap();
    assert_eq!(headlines.len(), 1);
    (committed_at, headlines.remove(0), c2)
}

#[test]
fn the_receipt_commit_time_is_recorded_and_reaches_the_delivered_headline() {
    // The receipt is committed 22:57 on 2026-09-12 at -07:00 (05:57 on
    // 09-13 UTC). `git log` shows 09-12; so must the delivered line.
    let (committed_at, headline, c2) = deliver_with_committer_date("2026-09-12T22:57:11-07:00");
    assert_eq!(committed_at.as_deref(), Some("2026-09-12T22:57:11-07:00"));
    assert_eq!(headline.witnessed_date, "2026-09-12");
    assert_eq!(headline.witnessed_at, "2026-09-12T22:57:11-07:00");
    assert_eq!(headline.receipt_oid, c2);
}

#[test]
fn a_commit_east_of_utc_near_midnight_is_dated_by_its_local_day() {
    // 00:10 on 2026-09-13 at +05:30 is 18:40 on 09-12 UTC; the committer
    // saw 09-13.
    let (committed_at, headline, _) = deliver_with_committer_date("2026-09-13T00:10:00+05:30");
    assert_eq!(committed_at.as_deref(), Some("2026-09-13T00:10:00+05:30"));
    assert_eq!(headline.witnessed_date, "2026-09-13");
}

#[test]
fn a_utc_commit_keeps_its_own_day_and_a_plus_zero_offset_is_explicit() {
    let (committed_at, headline, _) = deliver_with_committer_date("2026-09-12T10:20:30+0000");
    assert_eq!(committed_at.as_deref(), Some("2026-09-12T10:20:30+00:00"));
    assert_eq!(headline.witnessed_date, "2026-09-12");
}

// ---- merge receipts: a merge that left the span unchanged vs its first
// parent did not author the change ----

/// `foo` with a distinct top and bottom line, so two branches can edit
/// it without a textual conflict.
fn foo_two_ends(top: &str, bottom: &str) -> String {
    format!(
        "fn foo() {{\n    let top = {top};\n    let a = 0;\n    let b = 0;\n    let c = 0;\n    let d = 0;\n    let e = 0;\n    let f = 0;\n    let bottom = {bottom};\n}}\n"
    )
}

fn only_verdict(storage: &Storage, file: &str, oid: &str) -> WitnessVerdictRow {
    latest(storage, file, oid).expect("a verdict was written")
}

/// main: c0 (`foo` = 1) -> c1 (`foo` = 2, authors the change) ->
/// M (merges `side`, which only touched another file).
/// Returns (repo, s1, c1, m).
fn side_branch_witness_repo() -> (TestRepo, String, String, String) {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    repo.write("other.txt", "0\n");
    let _c0 = repo.commit("c0");
    repo.checkout_new("side");
    repo.write("other.txt", "side\n");
    let s1 = repo.commit("s1: side branch, lib.rs untouched");
    repo.checkout("main");
    repo.write("lib.rs", &foo_src("2"));
    let c1 = repo.commit("c1: authors the change");
    let m = repo.merge_no_ff("side", &[]);
    (repo, s1, c1, m)
}

#[test]
fn a_merge_that_left_the_span_unchanged_is_not_the_receipt() {
    let (repo, s1, c1, m) = side_branch_witness_repo();
    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&s1, "b3:1"), (&m, "b3:2")]);

    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.receipts_found, 1, "{stats:?}");
    assert_eq!(stats.abstained_merge_not_author, 0, "{stats:?}");
    let verdict = only_verdict(&storage, &file, &s1);
    assert_eq!(verdict.verdict, VerdictKind::SupersededBy);
    assert_eq!(
        verdict.receipt_oid.as_deref(),
        Some(c1.as_str()),
        "the authoring commit, never the merge {m}"
    );
}

#[test]
fn an_obsolete_conclusion_through_a_merge_names_the_deleting_commit() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    repo.write("other.txt", "0\n");
    let c0 = repo.commit("c0");
    repo.checkout_new("side");
    repo.write("other.txt", "side\n");
    let s1 = repo.commit("s1");
    repo.checkout("main");
    repo.write("lib.rs", "fn other() {}\n");
    let c1 = repo.commit("c1: deletes foo");
    let _m = repo.merge_no_ff("side", &[]);

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c0, "b3:1"), (&s1, "b3:1")]);
    let stats = run_join(&storage, false);
    assert_eq!(stats.obsolete, 2, "{stats:?}");
    assert_eq!(stats.abstained_merge_not_author, 0, "{stats:?}");
    for oid in [&c0, &s1] {
        let verdict = only_verdict(&storage, &file, oid);
        assert_eq!(verdict.verdict, VerdictKind::AnchorObsolete);
        assert_eq!(verdict.receipt_oid.as_deref(), Some(c1.as_str()), "{oid}");
    }
}

#[test]
fn a_side_branch_that_changed_the_span_itself_abstains_instead_of_guessing() {
    // The side branch already changed `foo` before the merge, and the
    // merge keeps mainline's version (`-s ours`): the merge is unchanged
    // vs ^1, but the merge base does not hold the witness's content, so
    // the mainline search cannot be trusted.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let _c0 = repo.commit("c0");
    repo.checkout_new("side");
    repo.write("lib.rs", &foo_src("side"));
    let s1 = repo.commit("s1: side branch edits foo");
    repo.checkout("main");
    repo.write("lib.rs", &foo_src("2"));
    let _c1 = repo.commit("c1");
    let m = repo.merge_no_ff("side", &["-s", "ours"]);

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&s1, "b3:side"), (&m, "b3:2")]);
    let stats = run_join(&storage, false);
    assert_eq!(stats.abstained_merge_not_author, 1, "{stats:?}");
    assert_eq!(stats.receipts_found, 0, "{stats:?}");
    assert_eq!(stats.events_written, 0, "{stats:?}");
    assert!(latest(&storage, &file, &s1).is_none());
}

#[test]
fn a_merge_that_changes_the_span_vs_its_first_parent_stays_the_receipt() {
    // mainline edits foo's top, the side branch edits its bottom; the
    // clean merge has both, so it differs from ^1 and is the receipt.
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_two_ends("1", "1"));
    let _c0 = repo.commit("c0");
    repo.checkout_new("side");
    repo.write("lib.rs", &foo_two_ends("1", "2"));
    let s1 = repo.commit("s1: bottom edited");
    repo.checkout("main");
    repo.write("lib.rs", &foo_two_ends("2", "1"));
    let _c1 = repo.commit("c1: top edited");
    let m = repo.merge_no_ff("side", &[]);
    assert_eq!(
        std::fs::read_to_string(repo.path.join("lib.rs")).unwrap(),
        foo_two_ends("2", "2"),
        "fixture: the merge combined both edits"
    );

    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&s1, "b3:1"), (&m, "b3:2")]);
    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.abstained_merge_not_author, 0, "{stats:?}");
    assert_eq!(
        only_verdict(&storage, &file, &s1).receipt_oid.as_deref(),
        Some(m.as_str())
    );
}

#[test]
fn linear_history_receipts_are_unchanged_by_the_merge_rule() {
    let repo = TestRepo::new();
    repo.write("lib.rs", &foo_src("1"));
    let c1 = repo.commit("c1");
    repo.write("lib.rs", &foo_src("2"));
    let c2 = repo.commit("c2");
    let storage = open_storage();
    let file = repo.file_path();
    insert_rows(&storage, &file, &[(&c1, "b3:1"), (&c2, "b3:2")]);
    let stats = run_join(&storage, false);
    assert_eq!(stats.superseded, 1, "{stats:?}");
    assert_eq!(stats.abstained_merge_not_author, 0, "{stats:?}");
    assert_eq!(
        only_verdict(&storage, &file, &c1).receipt_oid.as_deref(),
        Some(c2.as_str())
    );
}

#[test]
fn merge_not_author_shows_in_the_dream_text() {
    let stats = DreamStats {
        abstained_merge_not_author: 7,
        ..DreamStats::default()
    };
    assert!(
        stats.format_text(false).contains("merge_not_author=7"),
        "{}",
        stats.format_text(false)
    );
}
