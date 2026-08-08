//! Port of the S5 adversarial cases: six synthetic-repo scenarios that each
//! target one specific way a naive "is this still current" check gets
//! fooled, plus a determinism check.
//!
//! Every fixture is a real, disposable git repository built with the `git`
//! CLI in a temp dir (see `tests/common/mod.rs`) — no mocking of git itself.

mod common;

use codewitness::{
    causal, normalized_diff_id, Anchor, CausalOrder, SupersessionBasis, Tier, Verdict,
};
use common::TempRepo;

const FILE: &str = "witness.txt";

// ---------------------------------------------------------------------
// 1. linear: plain ancestor/descendant chain.
// ---------------------------------------------------------------------
#[test]
fn linear_history_causal_order_is_correct() {
    let repo = TempRepo::new();
    repo.write(FILE, "v1\n");
    let base = repo.commit_all("base");
    repo.write(FILE, "v2\n");
    let middle = repo.commit_all("middle");
    repo.write(FILE, "v3\n");
    let tip = repo.commit_all("tip");

    let auditor = repo.auditor();
    assert_eq!(
        causal::compare(auditor.repo(), base, tip).unwrap(),
        CausalOrder::AncestorOf
    );
    assert_eq!(
        causal::compare(auditor.repo(), tip, base).unwrap(),
        CausalOrder::DescendantOf
    );
    assert_eq!(
        causal::compare(auditor.repo(), middle, middle).unwrap(),
        CausalOrder::Equal
    );
    assert_eq!(
        causal::compare(auditor.repo(), base, middle).unwrap(),
        CausalOrder::AncestorOf
    );

    // Sanity: a witness taken at `base` should audit as Drifted now that
    // the worktree has moved on to v3.
    let anchor = Anchor::new(FILE);
    let witness = auditor.stamp_at(&anchor, base).unwrap();
    assert_eq!(auditor.try_audit(&witness).unwrap(), Verdict::Drifted);
}

// ---------------------------------------------------------------------
// 2. rebase-with-lying-dates: topology must win over wall-clock time.
// ---------------------------------------------------------------------
#[test]
fn rebase_with_lying_dates_uses_topology_not_time() {
    let repo = TempRepo::new();
    repo.write(FILE, "base\n");
    let base = repo.commit_all_dated(
        "base",
        "2020-01-01T00:00:00 +0000",
        "2020-01-01T00:00:00 +0000",
    );

    repo.checkout_new("feature");
    repo.write(FILE, "feature change\n");
    repo.commit_all_dated(
        "feature work",
        "2020-01-02T00:00:00 +0000",
        "2020-01-02T00:00:00 +0000",
    );

    repo.checkout("main");
    repo.write("main-only.txt", "main moved on\n");
    let main_tip = repo.commit_all_dated(
        "main advances",
        "2020-01-03T00:00:00 +0000",
        "2020-01-03T00:00:00 +0000",
    );

    // Rebase `feature` onto `main`, but force an absurdly early committer
    // date on the replayed commit — a real, common way for wall-clock time
    // to lie (backdated commits, clock skew, `--committer-date-is-author-date`
    // combined with a stale author date, etc.).
    repo.checkout("feature");
    repo.rebase_onto("main", "1999-01-01T00:00:00 +0000");
    let feature_tip = repo.head();

    // Confirm the trap is real: the "descendant" now claims an earlier
    // timestamp than its own ancestor.
    assert!(
        repo.committer_time(&feature_tip.to_string()) < repo.committer_time(&main_tip.to_string()),
        "fixture is supposed to make the child's timestamp lie as being earlier than its parent's"
    );
    assert!(repo.committer_time(&main_tip.to_string()) > repo.committer_time(&base.to_string()));

    let auditor = repo.auditor();
    // Despite the lying date, topology says main_tip is an ancestor of the
    // rebased feature_tip, and base is an ancestor of both.
    assert_eq!(
        causal::compare(auditor.repo(), main_tip, feature_tip).unwrap(),
        CausalOrder::AncestorOf
    );
    assert_eq!(
        causal::compare(auditor.repo(), feature_tip, main_tip).unwrap(),
        CausalOrder::DescendantOf
    );
    assert_eq!(
        causal::compare(auditor.repo(), base, feature_tip).unwrap(),
        CausalOrder::AncestorOf
    );
}

// ---------------------------------------------------------------------
// 3. squash: SHA non-ancestry, but identical net content change.
//
// NOTE: an earlier version of this test computed both `DiffId`s from
// literally the same `(base_content, content)` byte pairs (both
// histories were made to reach byte-identical content from a byte-
// identical base) — so `assert_eq!(id_a, id_b)` was really
// `assert_eq!(f(x, y), f(x, y))`, true for *any* deterministic function
// `f`, including a broken one. It proved nothing about `normalized_diff_id`
// specifically. This version gives each history genuinely different
// surrounding padding (mirroring the cherry-pick test below) so the two
// `(old, new)` buffer pairs are byte-*different* on both sides, and only
// the actual patch-identity logic (the context window around the shared
// `mid` -> `MID` edit) can make the ids match.
// ---------------------------------------------------------------------
#[test]
fn squash_breaks_sha_ancestry_but_diff_id_still_matches() {
    let repo = TempRepo::new();
    let base_content = "h1\nh2\nh3\nh4\nh5\nh6\nmid\nt1\nt2\nt3\nt4\nt5\nt6\n";
    repo.write(FILE, base_content);
    let base = repo.commit_all("base");

    // Two-step history on `main`: an unrelated commit first (padding
    // appended far below the line that will actually change), then the
    // real edit.
    repo.write(FILE, &format!("{base_content}two-step-only-tail\n"));
    repo.commit_all("step one: unrelated tail padding");
    let two_step_old_content = std::fs::read(repo.path.join(FILE)).unwrap();
    repo.write(
        FILE,
        &format!("{}two-step-only-tail\n", base_content.replace("mid", "MID")),
    );
    let two_step_tip = repo.commit_all("step two: mid -> MID");
    let two_step_new_content = std::fs::read(repo.path.join(FILE)).unwrap();

    // A squashed alternate history: reset back to `base` and make the
    // *same* mid -> MID edit in a single commit — but with different
    // padding (prepended, not appended) than the two-step branch used.
    repo.git(&["reset", "-q", "--hard", &base.to_string()]);
    repo.write(FILE, &format!("squash-only-head\n{base_content}"));
    let squashed_old_content = std::fs::read(repo.path.join(FILE)).unwrap();
    repo.write(
        FILE,
        &format!("squash-only-head\n{}", base_content.replace("mid", "MID")),
    );
    let squashed_tip = repo.commit_all("squashed: mid -> MID");
    let squashed_new_content = std::fs::read(repo.path.join(FILE)).unwrap();

    // Sanity: the two histories' buffer pairs are genuinely different on
    // both sides — this is not the same input fed in twice.
    assert_ne!(two_step_old_content, squashed_old_content);
    assert_ne!(two_step_new_content, squashed_new_content);

    let auditor = repo.auditor();
    // SHA non-ancestry: the two tips are siblings off `base`, neither
    // reachable from the other.
    assert_eq!(
        causal::compare(auditor.repo(), two_step_tip, squashed_tip).unwrap(),
        CausalOrder::Incomparable
    );

    // Despite genuinely different surrounding content on each side, the
    // same core edit (mid -> MID, with matching context) produces the
    // same patch identity either way — true squash/content equivalence.
    let id_two_step = normalized_diff_id(&two_step_old_content, &two_step_new_content);
    let id_squashed = normalized_diff_id(&squashed_old_content, &squashed_new_content);
    assert_eq!(id_two_step, id_squashed);
}

// ---------------------------------------------------------------------
// 4. exact revert: A -> B -> A. Content identity beats history.
// ---------------------------------------------------------------------
#[test]
fn exact_revert_resurrects_to_intact() {
    let repo = TempRepo::new();
    repo.write(FILE, "original\n");
    let commit_a = repo.commit_all("A: original");

    repo.write(FILE, "mutated\n");
    repo.commit_all("B: mutated");

    // Revert back to exactly A's content.
    repo.write(FILE, "original\n");
    repo.commit_all("C: revert to original");

    let auditor = repo.auditor();
    let anchor = Anchor::new(FILE);
    let witness_at_a = auditor.stamp_at(&anchor, commit_a).unwrap();

    // The worktree is now at C, whose content is byte-identical to A's.
    assert_eq!(auditor.try_audit(&witness_at_a).unwrap(), Verdict::Intact);
}

// ---------------------------------------------------------------------
// 5. near-identical A': Drifted, never Superseded, without explicit
//    successor evidence — and Superseded only from a Committed successor.
// ---------------------------------------------------------------------
#[test]
fn near_identical_edit_is_drifted_not_superseded_without_a_successor() {
    let repo = TempRepo::new();
    repo.write(FILE, "the quick brown fox\n");
    let commit_a = repo.commit_all("A");

    // A': a near-identical, not-quite-equal edit.
    repo.write(FILE, "the quick brown fox!\n");
    let commit_a_prime = repo.commit_all("A-prime: near identical");

    let auditor = repo.auditor();
    let anchor = Anchor::new(FILE);
    let witness_a = auditor.stamp_at(&anchor, commit_a).unwrap();

    // Plain audit: no successor evidence supplied at all -> Drifted.
    assert_eq!(auditor.try_audit(&witness_a).unwrap(), Verdict::Drifted);

    // A worktree-tier "successor" is not strong enough evidence to mint a
    // receipt, even when explicitly supplied.
    let worktree_successor = auditor.stamp(&anchor).unwrap();
    assert_eq!(worktree_successor.tier(), Tier::Worktree);
    assert_eq!(
        auditor
            .audit_against_successor(&witness_a, &worktree_successor)
            .unwrap(),
        Verdict::Drifted
    );

    // Only a Committed-tier successor can mint Superseded, and only when
    // supplied explicitly.
    let committed_successor = auditor.stamp_at(&anchor, commit_a_prime).unwrap();
    assert_eq!(committed_successor.tier(), Tier::Committed);
    match auditor
        .audit_against_successor(&witness_a, &committed_successor)
        .unwrap()
    {
        Verdict::Superseded(receipt) => {
            assert_eq!(receipt.receipt(), commit_a_prime);
            assert_eq!(receipt.basis(), SupersessionBasis::GraphOrdered);
        }
        other => panic!("expected Superseded, got {other:?}"),
    }
}

#[test]
fn worktree_witness_supersession_records_content_only_basis() {
    let repo = TempRepo::new();
    repo.write(FILE, "original\n");
    repo.commit_all("original");

    let auditor = repo.auditor();
    let anchor = Anchor::new(FILE);
    let worktree_witness = auditor.stamp(&anchor).unwrap();
    assert_eq!(worktree_witness.tier(), Tier::Worktree);

    repo.write(FILE, "replacement\n");
    let successor_commit = repo.commit_all("replacement");
    let successor = auditor.stamp_at(&anchor, successor_commit).unwrap();

    match auditor
        .audit_against_successor(&worktree_witness, &successor)
        .unwrap()
    {
        Verdict::Superseded(receipt) => {
            assert_eq!(receipt.basis(), SupersessionBasis::ContentOnly);
            assert_eq!(receipt.receipt(), successor_commit);
        }
        other => panic!("expected Superseded, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// 6. cherry-pick duplicate: same diff-id, distinct topology.
// ---------------------------------------------------------------------
#[test]
fn cherry_pick_duplicate_shares_diff_id_but_not_topology() {
    let repo = TempRepo::new();
    // Padding on both sides of the line that will actually change, wide
    // enough that an unrelated edit elsewhere falls outside the diff
    // context window.
    let base_content = "h1\nh2\nh3\nh4\nh5\nh6\nmid\nt1\nt2\nt3\nt4\nt5\nt6\n";
    repo.write(FILE, base_content);
    repo.commit_all("base");

    repo.checkout_new("feature");
    let feature_parent_content = std::fs::read(repo.path.join(FILE)).unwrap();
    repo.write(FILE, &base_content.replace("mid", "MID"));
    let feature_commit = repo.commit_all("feature: mid -> MID");
    let feature_content = std::fs::read(repo.path.join(FILE)).unwrap();

    repo.checkout("main");
    // Unrelated change, far away from the `mid` line.
    repo.write(FILE, &format!("unrelated-top-of-file\n{base_content}"));
    let main_commit_parent_content = std::fs::read(repo.path.join(FILE)).unwrap();
    repo.commit_all("main: unrelated top-of-file change");

    repo.cherry_pick(feature_commit);
    let cherry_pick_commit = repo.head();
    let main_commit_content = std::fs::read(repo.path.join(FILE)).unwrap();

    let auditor = repo.auditor();
    // Distinct topology: the cherry-pick produced a brand new commit not
    // reachable from (nor reaching) the original feature commit.
    assert_eq!(
        causal::compare(auditor.repo(), feature_commit, cherry_pick_commit).unwrap(),
        CausalOrder::Incomparable
    );

    // But the change itself has the same content identity.
    let id_feature = normalized_diff_id(&feature_parent_content, &feature_content);
    let id_main = normalized_diff_id(&main_commit_parent_content, &main_commit_content);
    assert_eq!(id_feature, id_main);

    let witnessed_feature = auditor
        .stamp_at(&Anchor::new(FILE), feature_commit)
        .unwrap();
    let successor_cherry_pick = auditor
        .stamp_at(&Anchor::new(FILE), cherry_pick_commit)
        .unwrap();
    match auditor
        .audit_against_successor(&witnessed_feature, &successor_cherry_pick)
        .unwrap()
    {
        Verdict::Superseded(receipt) => {
            assert_eq!(receipt.basis(), SupersessionBasis::ContentOnly);
            assert_eq!(receipt.receipt(), cherry_pick_commit);
        }
        other => panic!("expected Superseded, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Determinism: a full audit pass is byte-identical across repeated runs.
// ---------------------------------------------------------------------
#[test]
fn full_audit_pass_is_deterministic_across_repeated_runs() {
    let repo = TempRepo::new();
    repo.write(FILE, "alpha\n");
    let commit_a = repo.commit_all("A");
    repo.write(FILE, "beta\n");
    let commit_b = repo.commit_all("B");
    repo.write("second.txt", "unchanged\n");
    repo.commit_all("C: unrelated file, current worktree state");

    let anchors = vec![Anchor::new(FILE), Anchor::new("second.txt")];
    let commits = [commit_a, commit_b];

    let run = || -> String {
        let auditor = repo.auditor();
        let mut report = String::new();
        for anchor in &anchors {
            for &commit in &commits {
                if let Ok(witness) = auditor.stamp_at(anchor, commit) {
                    let verdict = auditor.try_audit(&witness).unwrap();
                    report.push_str(&format!("{:?} @ {:?} -> {:?}\n", anchor, commit, verdict));
                }
            }
        }
        for pair in commits.windows(1) {
            let a = pair[0];
            for &b in &commits {
                let order = causal::compare(auditor.repo(), a, b).unwrap();
                report.push_str(&format!("compare({a:?}, {b:?}) -> {order:?}\n"));
            }
        }
        report
    };

    let first = run();
    let second = run();
    assert_eq!(
        first, second,
        "identical inputs must produce byte-identical audit output"
    );
    assert!(
        !first.is_empty(),
        "sanity: the fixture must actually exercise something"
    );
}

// ---------------------------------------------------------------------
// 7. Path traversal: an anchor must never resolve outside the workdir.
// ---------------------------------------------------------------------
#[test]
fn absolute_anchor_path_is_rejected() {
    let repo = TempRepo::new();
    repo.write(FILE, "content\n");
    repo.commit_all("base");

    let auditor = repo.auditor();
    #[cfg(unix)]
    let absolute = Anchor::new("/etc/passwd");
    #[cfg(windows)]
    let absolute = Anchor::new("C:\\Windows\\System32\\drivers\\etc\\hosts");

    match auditor.stamp(&absolute) {
        Err(codewitness::Error::PathEscape { .. }) => {}
        other => panic!("expected PathEscape, got {other:?}"),
    }
}

#[test]
fn dot_dot_anchor_path_is_rejected() {
    let repo = TempRepo::new();
    repo.write(FILE, "content\n");
    repo.commit_all("base");

    let auditor = repo.auditor();
    let escaping = Anchor::new("../outside.txt");
    match auditor.stamp(&escaping) {
        Err(codewitness::Error::PathEscape { .. }) => {}
        other => panic!("expected PathEscape, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn symlink_escaping_the_workdir_is_rejected() {
    let repo = TempRepo::new();
    repo.write(FILE, "content\n");
    repo.commit_all("base");

    // A directory entirely outside the repository, holding a file the
    // anchor must not be able to reach through a symlink.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let outside = std::env::temp_dir().join(format!(
        "codewitness-path-escape-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "top secret\n").unwrap();

    // A symlinked directory inside the worktree pointing outside it.
    std::os::unix::fs::symlink(&outside, repo.path.join("linked")).unwrap();

    let auditor = repo.auditor();
    let anchor = Anchor::new("linked/secret.txt");
    match auditor.stamp(&anchor) {
        Err(codewitness::Error::PathEscape { .. }) => {}
        other => panic!("expected PathEscape, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&outside);
}

// ---------------------------------------------------------------------
// 8. Negative tests for `audit_against_successor`'s supersession
//    verification: forged/invalid successor evidence must never mint
//    `Superseded`.
// ---------------------------------------------------------------------

/// A committed successor that is honestly obtained from an *earlier*
/// commit than the witness it's checked against — no forgery needed:
/// causal precedence alone must block supersession.
#[test]
fn successor_causally_preceding_the_witness_is_not_superseded() {
    let repo = TempRepo::new();
    repo.write(FILE, "v1\n");
    let commit_1 = repo.commit_all("v1");
    repo.write(FILE, "v2\n");
    let commit_2 = repo.commit_all("v2");
    repo.write(FILE, "v3\n");
    repo.commit_all("v3: current worktree state");

    let auditor = repo.auditor();
    let anchor = Anchor::new(FILE);

    // Witness taken at the LATER of the two historical commits...
    let witness_at_2 = auditor.stamp_at(&anchor, commit_2).unwrap();
    // ...and an honestly-stamped committed "successor" that is actually
    // an ancestor of it — causally backwards, whatever its content.
    let earlier_as_successor = auditor.stamp_at(&anchor, commit_1).unwrap();

    let verdict = auditor
        .audit_against_successor(&witness_at_2, &earlier_as_successor)
        .unwrap();
    assert!(!verdict.is_superseded());
    assert_eq!(verdict, Verdict::Drifted);
}

#[test]
fn successor_equal_to_the_witness_is_not_superseded() {
    let repo = TempRepo::new();
    repo.write(FILE, "v1\n");
    repo.commit_all("v1");
    repo.write(FILE, "v2\n");
    let commit_2 = repo.commit_all("v2");
    repo.write(FILE, "v3\n");
    repo.commit_all("v3: current worktree state");

    let auditor = repo.auditor();
    let anchor = Anchor::new(FILE);
    let witness = auditor.stamp_at(&anchor, commit_2).unwrap();
    let equal_successor = auditor.stamp_at(&anchor, commit_2).unwrap();

    assert_eq!(
        auditor
            .audit_against_successor(&witness, &equal_successor)
            .unwrap(),
        Verdict::Drifted
    );
}

/// Forgery-based negative tests: these need to construct a `Witness` that
/// claims things an honest `Auditor::stamp_at` call could never produce
/// (a stamp mismatched with its claimed commit's real content, or a
/// commit that doesn't exist). `Witness`'s only public constructor
/// (`Witness::new`) can only mint `Tier::Worktree` evidence (see
/// `witness.rs`'s type-level docs) — by design, there is no direct way to
/// hand-construct a bogus `Tier::Committed` witness. The one *documented*
/// residual path is `serde` round-tripping (a `Witness` rehydrated from
/// untrusted storage carries no more trust than the storage it came
/// from), which is exactly the realistic threat model here: a corrupted
/// or tampered persisted witness store.
#[cfg(feature = "serde")]
mod successor_forgery {
    use super::*;
    use codewitness::Witness;

    fn roundtrip_with(witness: &Witness, mutate: impl FnOnce(&mut serde_json::Value)) -> Witness {
        let mut value = serde_json::to_value(witness).expect("serialize witness");
        mutate(&mut value);
        serde_json::from_value(value).expect("deserialize forged witness")
    }

    /// Replace a serialized `gix::ObjectId` with a same-shaped-but-bogus
    /// one, without assuming exactly how `gix` represents it on the wire
    /// (an externally-tagged enum payload as of this writing) — mutate
    /// whatever numeric byte array is nested inside.
    fn corrupt_object_id(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Array(bytes) => {
                for b in bytes.iter_mut() {
                    *b = serde_json::json!(0xffu8);
                }
            }
            serde_json::Value::Object(map) => {
                for v in map.values_mut() {
                    corrupt_object_id(v);
                }
            }
            serde_json::Value::String(s) => {
                *s = "f".repeat(s.len());
            }
            _ => {}
        }
    }

    #[test]
    fn forged_stamp_on_a_committed_successor_is_not_superseded() {
        let repo = TempRepo::new();
        repo.write(FILE, "original\n");
        let commit_a = repo.commit_all("A");
        repo.write(FILE, "mutated\n");
        let commit_b = repo.commit_all("B: mutated");

        let auditor = repo.auditor();
        let anchor = Anchor::new(FILE);
        let witness_a = auditor.stamp_at(&anchor, commit_a).unwrap();

        // A legitimately committed successor at commit_b, tampered in
        // storage afterwards so its stamp no longer matches commit_b's
        // real content.
        let real_successor = auditor.stamp_at(&anchor, commit_b).unwrap();
        let forged = roundtrip_with(&real_successor, |v| {
            v["stamp"] = serde_json::json!(
                "b3:0000000000000000000000000000000000000000000000000000000000000000"
            );
        });

        let verdict = auditor
            .audit_against_successor(&witness_a, &forged)
            .unwrap();
        assert!(!verdict.is_superseded());
        assert_eq!(verdict, Verdict::Drifted);
    }

    #[test]
    fn successor_at_a_nonexistent_commit_is_not_superseded() {
        let repo = TempRepo::new();
        repo.write(FILE, "original\n");
        let commit_a = repo.commit_all("A");
        repo.write(FILE, "mutated\n");
        let commit_b = repo.commit_all("B: mutated");

        let auditor = repo.auditor();
        let anchor = Anchor::new(FILE);
        let witness_a = auditor.stamp_at(&anchor, commit_a).unwrap();
        let real_successor = auditor.stamp_at(&anchor, commit_b).unwrap();

        // Point `at` at a commit hash this freshly-created repository
        // does not contain.
        let forged = roundtrip_with(&real_successor, |v| {
            corrupt_object_id(&mut v["at"]);
        });

        // A successor whose claimed commit cannot even be read is an
        // *operational* failure, not a checked rejection — it surfaces as
        // an error rather than being folded into a deterministic verdict.
        // Either way, no `Superseded` receipt is minted.
        match auditor.audit_against_successor(&witness_a, &forged) {
            Err(codewitness::Error::Commit { .. }) => {}
            other => panic!("expected Err(Error::Commit), got {other:?}"),
        }
    }
}
