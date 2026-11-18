use super::*;
use std::process::Command;

// --- fixture repositories -------------------------------------------
//
// Real `git init` + real commits + the real OIDs they produce, per the
// `tests/dream_integration.rs` precedent. Nothing here fakes an OID.

/// `git -C <repo>` with the caller's own `GIT_*` environment stripped —
/// this suite may run under a git hook that exports `GIT_DIR`, which
/// would redirect these commands at the real repository.
fn git_in(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    cmd.arg("-C").arg(repo);
    cmd
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        assert!(
            git_in(repo)
                .args(["init", "-q", "-b", "main"])
                .status()
                .expect("git must be available to run the astdiff fixtures")
                .success(),
            "git init failed"
        );
        for args in [
            ["config", "user.email", "astdiff@csr.invalid"],
            ["config", "user.name", "CSR AstDiff Fixture"],
            ["config", "commit.gpgsign", "false"],
        ] {
            assert!(git_in(repo).args(args).status().unwrap().success());
        }
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, name: &str, contents: impl AsRef<[u8]>) {
        let path = self.root().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn remove(&self, name: &str) {
        std::fs::remove_file(self.root().join(name)).unwrap();
    }

    /// Stage everything and commit; returns the real commit OID.
    fn commit(&self, message: &str) -> String {
        let repo = self.root();
        assert!(git_in(repo).args(["add", "-A"]).status().unwrap().success());
        assert!(git_in(repo)
            .args(["commit", "-q", "--allow-empty", "-m", message])
            .status()
            .unwrap()
            .success());
        let out = git_in(repo).args(["rev-parse", "HEAD"]).output().unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn request(&self, path: &str, before: &str, after: &str) -> AstDiffRequest {
        AstDiffRequest {
            repo_root: self.root().to_path_buf(),
            path: path.to_string(),
            before_oid: before.to_string(),
            after_oid: after.to_string(),
        }
    }
}

fn diffed(outcome: &AstDiffOutcome) -> &AstDiff {
    match outcome {
        AstDiffOutcome::Diffed(diff) => diff,
        AstDiffOutcome::Abstained(reason) => {
            panic!("expected a diff, abstained: {}", reason.sentence())
        }
    }
}

fn abstained(outcome: &AstDiffOutcome) -> &Abstention {
    match outcome {
        AstDiffOutcome::Abstained(reason) => reason,
        AstDiffOutcome::Diffed(diff) => panic!(
            "expected an abstention, got a diff with {:?} and {} before / {} after nodes",
            diff.counts,
            diff.before.len(),
            diff.after.len()
        ),
    }
}

fn names(nodes: &[SymbolNode]) -> Vec<&str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

fn status_of(nodes: &[SymbolNode], name: &str) -> SymbolStatus {
    nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node named {name} in {:?}", names(nodes)))
        .status
}

// --- diff cases -----------------------------------------------------

#[test]
fn symbol_added_is_added_and_the_survivor_stays_intact() {
    let fx = Fixture::new();
    fx.write("src/auth.rs", "fn keep() -> u8 { 1 }\n");
    let before = fx.commit("one");
    fx.write("src/auth.rs", "fn keep() -> u8 { 1 }\n\nfn fresh() {}\n");
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/auth.rs", &before, &after));
    let diff = diffed(&outcome);
    assert!(diff.before_present && diff.after_present);
    assert_eq!(names(&diff.before), vec!["keep"]);
    assert_eq!(names(&diff.after), vec!["keep", "fresh"]);
    assert_eq!(status_of(&diff.after, "fresh"), SymbolStatus::Added);
    assert_eq!(status_of(&diff.before, "keep"), SymbolStatus::Intact);
    assert_eq!(
        diff.counts,
        DiffCounts {
            intact: 1,
            changed: 0,
            removed: 0,
            added: 1
        }
    );
    assert_eq!(diff.connectors.len(), 1);
    assert_eq!(
        diff.connectors[0],
        Connector {
            before: 0,
            after: 0,
            changed: false
        }
    );
}

#[test]
fn symbol_removed_is_removed() {
    let fx = Fixture::new();
    fx.write("src/auth.rs", "fn keep() {}\nfn doomed() {}\n");
    let before = fx.commit("one");
    fx.write("src/auth.rs", "fn keep() {}\n");
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/auth.rs", &before, &after));
    let diff = diffed(&outcome);
    assert_eq!(status_of(&diff.before, "doomed"), SymbolStatus::Removed);
    assert!(diff
        .before
        .iter()
        .find(|n| n.name == "doomed")
        .unwrap()
        .paired_with
        .is_none());
    assert_eq!(
        diff.counts,
        DiffCounts {
            intact: 1,
            changed: 0,
            removed: 1,
            added: 0
        }
    );
}

#[test]
fn a_rename_is_removed_plus_added_and_never_claimed_as_a_rename() {
    let fx = Fixture::new();
    fx.write("src/auth.rs", "fn old_name() -> u8 { 7 }\n");
    let before = fx.commit("one");
    // Identical body, different name. Even that is not enough evidence
    // to assert the two are the same symbol.
    fx.write("src/auth.rs", "fn new_name() -> u8 { 7 }\n");
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/auth.rs", &before, &after));
    let diff = diffed(&outcome);
    assert_eq!(status_of(&diff.before, "old_name"), SymbolStatus::Removed);
    assert_eq!(status_of(&diff.after, "new_name"), SymbolStatus::Added);
    assert!(diff.connectors.is_empty(), "a rename must not be connected");
    assert_eq!(
        diff.counts,
        DiffCounts {
            intact: 0,
            changed: 0,
            removed: 1,
            added: 1
        }
    );
}

#[test]
fn same_name_changed_body_is_changed_with_a_connector() {
    let fx = Fixture::new();
    fx.write("src/auth.rs", "fn check(t: &str) -> bool { t.len() > 8 }\n");
    let before = fx.commit("one");
    fx.write(
        "src/auth.rs",
        "fn check(t: &str) -> bool { t.len() > 12 }\n",
    );
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/auth.rs", &before, &after));
    let diff = diffed(&outcome);
    assert_eq!(status_of(&diff.before, "check"), SymbolStatus::Changed);
    assert_eq!(status_of(&diff.after, "check"), SymbolStatus::Changed);
    assert_eq!(diff.connectors.len(), 1);
    assert!(diff.connectors[0].changed);
    assert_eq!(diff.counts.changed, 1);
    assert_eq!(diff.counts.intact, 0);
}

#[test]
fn reformatting_alone_reads_intact() {
    let fx = Fixture::new();
    fx.write("src/auth.rs", "fn check(t:&str)->bool{t.len()>8}\n");
    let before = fx.commit("one");
    fx.write(
        "src/auth.rs",
        "fn check(t: &str) -> bool {\n    t.len() > 8\n}\n",
    );
    let after = fx.commit("two");

    let diff = compute(&fx.request("src/auth.rs", &before, &after));
    let diff = diffed(&diff);
    assert_eq!(diff.counts.intact, 1);
    assert_eq!(diff.counts.changed, 0);
}

#[test]
fn file_absent_before_reports_creation_not_an_abstention() {
    let fx = Fixture::new();
    fx.write("README.md", "seed\n");
    let before = fx.commit("no rust yet");
    fx.write("src/new.rs", "fn born() {}\n");
    let after = fx.commit("added");

    let outcome = compute(&fx.request("src/new.rs", &before, &after));
    let diff = diffed(&outcome);
    assert!(!diff.before_present, "file did not exist at the before oid");
    assert!(diff.after_present);
    assert!(diff.before.is_empty());
    assert_eq!(status_of(&diff.after, "born"), SymbolStatus::Added);
    assert_eq!(diff.counts.added, 1);
    assert_eq!(diff.counts.removed, 0);
}

#[test]
fn file_absent_after_reports_deletion_not_an_abstention() {
    let fx = Fixture::new();
    fx.write("src/gone.rs", "fn vanishing() {}\n");
    let before = fx.commit("one");
    fx.remove("src/gone.rs");
    let after = fx.commit("deleted");

    let outcome = compute(&fx.request("src/gone.rs", &before, &after));
    let diff = diffed(&outcome);
    assert!(diff.before_present);
    assert!(!diff.after_present, "file was deleted at the after oid");
    assert!(diff.after.is_empty());
    assert_eq!(status_of(&diff.before, "vanishing"), SymbolStatus::Removed);
    assert_eq!(diff.counts.removed, 1);
}

#[test]
fn impl_members_are_qualified_so_same_named_methods_do_not_cross_pair() {
    let fx = Fixture::new();
    fx.write(
        "src/pair.rs",
        "struct A; struct B;\nimpl A { fn new() -> u8 { 1 } }\nimpl B { fn new() -> u8 { 2 } }\n",
    );
    let before = fx.commit("one");
    fx.write(
        "src/pair.rs",
        "struct A; struct B;\nimpl A { fn new() -> u8 { 1 } }\nimpl B { fn new() -> u8 { 99 } }\n",
    );
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/pair.rs", &before, &after));
    let diff = diffed(&outcome);
    assert_eq!(status_of(&diff.before, "A::new"), SymbolStatus::Intact);
    assert_eq!(status_of(&diff.before, "B::new"), SymbolStatus::Changed);
    let member = diff.before.iter().find(|n| n.name == "B::new").unwrap();
    assert_eq!(member.container.as_deref(), Some("B"));
    assert_eq!(member.depth, 1);
    let top = diff.before.iter().find(|n| n.name == "A").unwrap();
    assert_eq!(top.depth, 0);
    assert_eq!(top.container, None);
}

#[test]
fn source_order_is_preserved_on_both_sides() {
    let fx = Fixture::new();
    fx.write("src/order.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
    let before = fx.commit("one");
    fx.write("src/order.rs", "fn c() {}\nfn a() {}\nfn b() {}\n");
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/order.rs", &before, &after));
    let diff = diffed(&outcome);
    assert_eq!(names(&diff.before), vec!["a", "b", "c"]);
    assert_eq!(names(&diff.after), vec!["c", "a", "b"]);
    assert!(diff.before.windows(2).all(|w| w[0].line <= w[1].line));
    // Connectors are emitted in before order but point at the moved
    // after-side indices.
    assert_eq!(
        diff.connectors,
        vec![
            Connector {
                before: 0,
                after: 1,
                changed: false
            },
            Connector {
                before: 1,
                after: 2,
                changed: false
            },
            Connector {
                before: 2,
                after: 0,
                changed: false
            },
        ]
    );
    assert_eq!(diff.counts.intact, 3);
}

#[test]
fn typescript_classes_and_functions_both_diff() {
    let fx = Fixture::new();
    fx.write(
        "web/app.ts",
        "export class Store { load() { return 1; } }\nfunction helper() { return 2; }\n",
    );
    let before = fx.commit("one");
    fx.write(
        "web/app.ts",
        "export class Store { load() { return 5; } }\n",
    );
    let after = fx.commit("two");

    let outcome = compute(&fx.request("web/app.ts", &before, &after));
    let diff = diffed(&outcome);
    assert_eq!(diff.language, "TypeScript");
    assert_eq!(status_of(&diff.before, "helper"), SymbolStatus::Removed);
    assert_eq!(
        status_of(&diff.before, "Store::load"),
        SymbolStatus::Changed
    );
}

// --- abstention paths ------------------------------------------------

#[test]
fn binary_blob_abstains_as_not_utf8() {
    let fx = Fixture::new();
    fx.write("src/bin.rs", "fn real() {}\n");
    let before = fx.commit("one");
    fx.write("src/bin.rs", [0x00u8, 0xff, 0xfe, 0x00, 0x80, 0x01]);
    let after = fx.commit("binary now");

    let outcome = compute(&fx.request("src/bin.rs", &before, &after));
    match abstained(&outcome) {
        Abstention::NotUtf8 { side, path } => {
            assert_eq!(*side, Side::After);
            assert_eq!(path, "src/bin.rs");
        }
        other => panic!("expected NotUtf8, got {other:?}"),
    }
    assert!(abstained(&outcome).sentence().contains("not valid UTF-8"));
}

#[test]
fn unknown_oid_abstains_and_names_the_oid() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let before = fx.commit("one");
    let missing = "0123456789abcdef0123456789abcdef01234567";

    let outcome = compute(&fx.request("src/a.rs", &before, missing));
    match abstained(&outcome) {
        Abstention::UnknownOid { side, oid, .. } => {
            assert_eq!(*side, Side::After);
            assert_eq!(oid, missing);
        }
        other => panic!("expected UnknownOid, got {other:?}"),
    }
    assert!(abstained(&outcome).sentence().contains(missing));
    assert!(
        !abstained(&outcome).is_stable(),
        "a fetch could supply this commit later; it must not be cached"
    );
}

#[test]
fn malformed_oid_never_reaches_git() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let real = fx.commit("one");

    for hostile in ["HEAD", "main@{1}", "", "zz", "../../etc/passwd", "a..b"] {
        let outcome = compute(&fx.request("src/a.rs", hostile, &real));
        match abstained(&outcome) {
            Abstention::MalformedOid { side, oid } => {
                assert_eq!(*side, Side::Before);
                assert_eq!(oid, hostile);
            }
            other => panic!("{hostile:?} should be MalformedOid, got {other:?}"),
        }
    }
}

#[test]
fn escaping_or_absolute_paths_are_rejected_before_any_git_access() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let one = fx.commit("one");

    for hostile in ["../outside.rs", "/etc/hosts", "  ", "a/../../b.rs"] {
        let outcome = compute(&fx.request(hostile, &one, &one));
        assert!(
            matches!(abstained(&outcome), Abstention::PathRejected { .. }),
            "{hostile:?} should be PathRejected, got {:?}",
            abstained(&outcome)
        );
    }
}

#[test]
fn unsupported_extension_abstains_with_the_extension() {
    let fx = Fixture::new();
    fx.write("ios/Player.swift", "class Player { func show() {} }\n");
    let before = fx.commit("one");
    fx.write("ios/Player.swift", "class Player { func hide() {} }\n");
    let after = fx.commit("two");

    let outcome = compute(&fx.request("ios/Player.swift", &before, &after));
    match abstained(&outcome) {
        Abstention::UnsupportedLanguage { path, extension } => {
            assert_eq!(path, "ios/Player.swift");
            assert_eq!(extension, "swift");
        }
        other => panic!("expected UnsupportedLanguage, got {other:?}"),
    }
}

#[test]
fn absent_from_both_sides_abstains_rather_than_returning_an_empty_diff() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let before = fx.commit("one");
    fx.write("src/b.rs", "fn b() {}\n");
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/never.rs", &before, &after));
    match abstained(&outcome) {
        Abstention::AbsentFromBothSides {
            path,
            before_oid,
            after_oid,
        } => {
            assert_eq!(path, "src/never.rs");
            assert_eq!(before_oid, &before);
            assert_eq!(after_oid, &after);
        }
        other => panic!("expected AbsentFromBothSides, got {other:?}"),
    }
}

#[test]
fn a_file_with_no_named_symbols_abstains_instead_of_reading_as_unchanged() {
    let fx = Fixture::new();
    fx.write("src/only_comments.rs", "// before\nmod other;\n");
    let before = fx.commit("one");
    fx.write(
        "src/only_comments.rs",
        "// after, definitely different\nmod other;\n",
    );
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/only_comments.rs", &before, &after));
    assert!(
        matches!(abstained(&outcome), Abstention::NoSymbolsExtracted { .. }),
        "got {:?}",
        abstained(&outcome)
    );
}

#[test]
fn unparseable_garbage_abstains_rather_than_inventing_symbols() {
    let fx = Fixture::new();
    // Valid UTF-8, structurally meaningless Rust: tree-sitter recovers
    // no named definition, so there is nothing honest to show.
    fx.write("src/garbage.rs", ")))} <<< ??? &&& (((\n");
    let before = fx.commit("one");
    fx.write("src/garbage.rs", "@@@@ ]]]] ;;;; ####\n");
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/garbage.rs", &before, &after));
    assert!(
        matches!(abstained(&outcome), Abstention::NoSymbolsExtracted { .. }),
        "got {:?}",
        abstained(&outcome)
    );
}

#[test]
fn oversized_blob_abstains_with_the_measured_size() {
    let fx = Fixture::new();
    fx.write("src/big.rs", "fn small() {}\n");
    let before = fx.commit("one");
    let mut huge = String::from("fn small() {}\n");
    while huge.len() <= MAX_BLOB_BYTES {
        huge.push_str("// padding padding padding padding padding padding\n");
    }
    fx.write("src/big.rs", &huge);
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/big.rs", &before, &after));
    match abstained(&outcome) {
        Abstention::FileTooLarge {
            side, bytes, limit, ..
        } => {
            assert_eq!(*side, Side::After);
            assert_eq!(*bytes, huge.len());
            assert_eq!(*limit, MAX_BLOB_BYTES);
        }
        other => panic!("expected FileTooLarge, got {other:?}"),
    }
}

/// Total bytes of everything under `.git/objects`, i.e. the *stored*
/// (zlib-compressed) size of the repository's object database.
fn odb_bytes(repo: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else {
                *total += meta.len();
            }
        }
    }
    let mut total = 0;
    walk(&repo.join(".git").join("objects"), &mut total);
    total
}

/// Finding 7. The guard has to decide from the blob's declared size,
/// not from the length of a `Vec<u8>` that has already been allocated.
#[test]
fn an_oversized_blob_is_refused_from_its_header_before_it_is_materialised() {
    let fx = Fixture::new();
    fx.write("src/bomb.rs", "fn small() {}\n");
    let before = fx.commit("one");

    // 8 MiB — sixteen times the guard — and pathologically
    // compressible, so git stores it in a few kilobytes. This is the
    // review's "very large compressed git blob": a post-allocation
    // check would have had to inflate all 8 MiB on every request just
    // to discover the file was too big to parse.
    const DECLARED: usize = 8 * 1024 * 1024;
    let mut bomb = String::with_capacity(DECLARED + 32);
    bomb.push_str("fn small() {}\n");
    while bomb.len() < DECLARED {
        bomb.push('A');
    }
    fx.write("src/bomb.rs", &bomb);
    let after = fx.commit("bomb");

    let auditor = codewitness::Auditor::open(fx.root().to_path_buf()).unwrap();
    let after_commit = auditor.resolve_commit(&after).unwrap();

    // Metadata alone knows the size, and knows it is over the cap.
    let plan = inspect_blob(
        &auditor,
        Path::new("src/bomb.rs"),
        "src/bomb.rs",
        after_commit,
        Side::After,
    )
    .expect("the header must be readable");
    assert_eq!(
        plan,
        BlobPlan::Blob {
            bytes: bomb.len() as u64
        }
    );
    assert!(bomb.len() > MAX_BLOB_BYTES);

    // ...and the entire object database is smaller than the cap, so the
    // declared size provably did not come from measuring what was read:
    // there are not 8 MiB anywhere on disk to have read.
    let stored = odb_bytes(fx.root());
    assert!(
        stored < MAX_BLOB_BYTES as u64,
        "the whole odb is {stored} bytes while the blob declares {} — \
             the size must have come from the header, not from content",
        bomb.len()
    );

    // The guard path refuses it, with the declared size and the limit.
    let refused = read_side(
        &auditor,
        Path::new("src/bomb.rs"),
        "src/bomb.rs",
        after_commit,
        Side::After,
    )
    .expect_err("an oversized blob must not be read");
    match &refused {
        Abstention::FileTooLarge {
            side, bytes, limit, ..
        } => {
            assert_eq!(*side, Side::After);
            assert_eq!(*bytes, bomb.len());
            assert_eq!(*limit, MAX_BLOB_BYTES);
        }
        other => panic!("expected FileTooLarge, got {other:?}"),
    }

    // End to end: an explicit abstention, never an empty tree.
    let outcome = compute(&fx.request("src/bomb.rs", &before, &after));
    assert!(outcome.diff().is_none());
    assert!(abstained(&outcome)
        .sentence()
        .contains("over the 524288-byte parse limit"));
}

/// A blob that fits is still read, and the header size it was admitted
/// on is the real content length — the guard is a bound, not a wall.
#[test]
fn a_blob_under_the_cap_reports_its_exact_size_and_is_read() {
    let fx = Fixture::new();
    let source = "fn fits() {}\n";
    fx.write("src/fits.rs", source);
    let one = fx.commit("one");

    let auditor = codewitness::Auditor::open(fx.root().to_path_buf()).unwrap();
    let commit = auditor.resolve_commit(&one).unwrap();
    assert_eq!(
        inspect_blob(
            &auditor,
            Path::new("src/fits.rs"),
            "src/fits.rs",
            commit,
            Side::Before
        )
        .unwrap(),
        BlobPlan::Blob {
            bytes: source.len() as u64
        }
    );
    assert!(matches!(
        read_side(
            &auditor,
            Path::new("src/fits.rs"),
            "src/fits.rs",
            commit,
            Side::Before
        )
        .unwrap(),
        SideRead::Present(_)
    ));
}

/// An absent path is reported as absence by metadata inspection too —
/// the "created / deleted" fact must not become an abstention.
#[test]
fn a_path_absent_from_the_tree_inspects_as_absent_not_as_a_failure() {
    let fx = Fixture::new();
    fx.write("src/here.rs", "fn here() {}\n");
    let one = fx.commit("one");

    let auditor = codewitness::Auditor::open(fx.root().to_path_buf()).unwrap();
    let commit = auditor.resolve_commit(&one).unwrap();
    assert_eq!(
        inspect_blob(
            &auditor,
            Path::new("src/nowhere.rs"),
            "src/nowhere.rs",
            commit,
            Side::Before
        )
        .unwrap(),
        BlobPlan::Absent
    );
}

/// Git stores a symlink's target as an ordinary blob, so an
/// object-level check would hand back `"real.rs"` as if it were the
/// file's content. The entry mode is the only place the distinction
/// survives, and this pins that it is the thing being checked.
#[cfg(unix)]
#[test]
fn a_symlink_abstains_instead_of_diffing_its_target_string() {
    let fx = Fixture::new();
    fx.write("src/real.rs", "fn real() {}\n");
    std::os::unix::fs::symlink("real.rs", fx.root().join("src/link.rs")).unwrap();
    let one = fx.commit("a symlink beside a real file");

    let outcome = compute(&fx.request("src/link.rs", &one, &one));
    match abstained(&outcome) {
        Abstention::UnreadableBlob { side, path, detail } => {
            assert_eq!(*side, Side::Before);
            assert_eq!(path, "src/link.rs");
            assert!(detail.contains("symlink"), "detail was {detail:?}");
        }
        other => panic!("expected UnreadableBlob, got {other:?}"),
    }
}

#[test]
fn a_directory_at_the_path_is_an_unreadable_blob_not_an_absence() {
    let fx = Fixture::new();
    fx.write("src/thing.rs/inner.rs", "fn inner() {}\n");
    let one = fx.commit("path is a directory");

    let outcome = compute(&fx.request("src/thing.rs", &one, &one));
    match abstained(&outcome) {
        Abstention::UnreadableBlob { side, detail, .. } => {
            assert_eq!(*side, Side::Before);
            assert!(detail.contains("tree"), "detail was {detail:?}");
        }
        other => panic!("expected UnreadableBlob, got {other:?}"),
    }
}

#[test]
fn a_non_repository_root_abstains() {
    let dir = tempfile::tempdir().unwrap();
    let request = AstDiffRequest {
        repo_root: dir.path().to_path_buf(),
        path: "src/a.rs".to_string(),
        before_oid: "0123456789abcdef0123456789abcdef01234567".to_string(),
        after_oid: "89abcdef0123456789abcdef0123456789abcdef".to_string(),
    };
    let outcome = compute(&request);
    assert!(
        matches!(
            abstained(&outcome),
            Abstention::RepositoryUnavailable { .. }
        ),
        "got {:?}",
        abstained(&outcome)
    );
    assert!(!abstained(&outcome).is_stable());
}

/// The binding rule: no abstention may render as an empty tree, and
/// every one must say why.
#[test]
fn every_abstention_variant_carries_a_reason_sentence() {
    let all = vec![
        Abstention::RepositoryUnavailable {
            repo_root: "/tmp/x".into(),
            detail: "not a repo".into(),
        },
        Abstention::PathRejected {
            path: "../x".into(),
            detail: "contains a `..` component".into(),
        },
        Abstention::MalformedOid {
            side: Side::Before,
            oid: "HEAD".into(),
        },
        Abstention::UnknownOid {
            side: Side::After,
            oid: "abcdef1".into(),
            detail: "no such commit".into(),
        },
        Abstention::UnsupportedLanguage {
            path: "a.swift".into(),
            extension: "swift".into(),
        },
        Abstention::FileTooLarge {
            side: Side::After,
            path: "a.rs".into(),
            bytes: 999_999,
            limit: MAX_BLOB_BYTES,
        },
        Abstention::UnreadableBlob {
            side: Side::Before,
            path: "a.rs".into(),
            detail: "odb error".into(),
        },
        Abstention::NotUtf8 {
            side: Side::After,
            path: "a.rs".into(),
        },
        Abstention::ParsePanicked {
            side: Side::Before,
            path: "a.rs".into(),
        },
        Abstention::AbsentFromBothSides {
            path: "a.rs".into(),
            before_oid: "aaaa".into(),
            after_oid: "bbbb".into(),
        },
        Abstention::NoSymbolsExtracted {
            path: "a.rs".into(),
            before_oid: "aaaa".into(),
            after_oid: "bbbb".into(),
        },
    ];
    for reason in all {
        let sentence = reason.sentence();
        assert!(
            sentence.starts_with("AST comparison abstained:"),
            "{reason:?} must lead with the abstention, got {sentence:?}"
        );
        assert!(sentence.len() > 40, "{reason:?} gave a stub: {sentence:?}");
        assert!(
            !sentence.contains("{"),
            "{reason:?} left an unformatted placeholder: {sentence:?}"
        );
    }
}

/// The abstention contract swept end to end over **real** fixture
/// repositories: every failure mode the engine can actually reach must
/// come back `Abstained`, with a reason, and with no diff attached —
/// so nothing downstream can render it as "nothing changed".
///
/// Ten of the eleven variants are produced here from live inputs.
/// [`Abstention::ParsePanicked`] is the exception and is stated rather
/// than claimed: it exists only for a tree-sitter panic, which cannot
/// be provoked from source text, so it is covered by
/// `every_abstention_variant_carries_a_reason_sentence` (its sentence)
/// and by construction (`extract_symbols` returns `None` only from
/// `catch_unwind`) — not by an executed panic.
#[test]
fn every_reachable_failure_mode_abstains_with_a_reason_and_no_diff() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    fx.write("src/only_comments.rs", "// nothing but a comment\n");
    fx.write("ios/P.swift", "class P {}\n");
    fx.write("src/dir.rs/inner.rs", "fn inner() {}\n");
    let one = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    fx.write("src/binary.rs", [0x00u8, 0xff, 0xfe, 0x80]);
    let mut huge = String::from("fn big() {}\n");
    while huge.len() <= MAX_BLOB_BYTES {
        huge.push_str("// padding padding padding padding padding\n");
    }
    fx.write("src/huge.rs", &huge);
    let two = fx.commit("two");

    /// One case: a label, the request that provokes it, and the
    /// predicate naming the exact variant it must produce.
    type Case = (&'static str, AstDiffRequest, fn(&Abstention) -> bool);

    let elsewhere = tempfile::tempdir().unwrap();
    let cases: Vec<Case> = vec![
        (
            "repository unavailable",
            AstDiffRequest {
                repo_root: elsewhere.path().to_path_buf(),
                path: "src/a.rs".into(),
                before_oid: one.clone(),
                after_oid: two.clone(),
            },
            |r| matches!(r, Abstention::RepositoryUnavailable { .. }),
        ),
        (
            "path rejected",
            fx.request("../escape.rs", &one, &two),
            |r| matches!(r, Abstention::PathRejected { .. }),
        ),
        ("malformed oid", fx.request("src/a.rs", "HEAD", &two), |r| {
            matches!(r, Abstention::MalformedOid { .. })
        }),
        (
            "unknown oid",
            fx.request("src/a.rs", &one, "0123456789abcdef0123456789abcdef01234567"),
            |r| matches!(r, Abstention::UnknownOid { .. }),
        ),
        (
            "unsupported language",
            fx.request("ios/P.swift", &one, &two),
            |r| matches!(r, Abstention::UnsupportedLanguage { .. }),
        ),
        (
            "file too large",
            fx.request("src/huge.rs", &one, &two),
            |r| matches!(r, Abstention::FileTooLarge { .. }),
        ),
        (
            "unreadable blob",
            fx.request("src/dir.rs", &one, &two),
            |r| matches!(r, Abstention::UnreadableBlob { .. }),
        ),
        ("not utf-8", fx.request("src/binary.rs", &one, &two), |r| {
            matches!(r, Abstention::NotUtf8 { .. })
        }),
        (
            "absent from both sides",
            fx.request("src/never.rs", &one, &two),
            |r| matches!(r, Abstention::AbsentFromBothSides { .. }),
        ),
        (
            "no symbols extracted",
            fx.request("src/only_comments.rs", &one, &two),
            |r| matches!(r, Abstention::NoSymbolsExtracted { .. }),
        ),
    ];

    for (label, request, expected) in cases {
        let outcome = compute(&request);
        assert!(
            outcome.diff().is_none(),
            "{label}: an abstention must carry no diff"
        );
        let reason = outcome
            .abstention()
            .unwrap_or_else(|| panic!("{label}: expected an abstention"));
        assert!(expected(reason), "{label}: got {reason:?}");
        let sentence = reason.sentence();
        assert!(
            sentence.starts_with("AST comparison abstained:"),
            "{label}: {sentence:?}"
        );
        assert!(sentence.len() > 40, "{label}: stub sentence {sentence:?}");
        // The cached form must round-trip to the same explicit reason.
        let cache = AstDiffCache::new();
        assert!(
            cache.get_or_compute(&request).diff().is_none(),
            "{label}: the cache must not turn an abstention into a diff"
        );
    }
}

/// The other half of the contract: whenever the engine *does* return a
/// diff, at least one side carries a symbol. An empty tree is never a
/// result, so a renderer can trust that `Diffed` means something to
/// draw.
#[test]
fn a_diffed_outcome_always_carries_at_least_one_symbol() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    fx.write("src/keep.rs", "fn keep() {}\n");
    fx.write("src/gone.rs", "fn gone() {}\n");
    let one = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    fx.write("src/born.rs", "fn born() {}\n");
    fx.remove("src/gone.rs");
    let two = fx.commit("two");

    for path in ["src/a.rs", "src/keep.rs", "src/gone.rs", "src/born.rs"] {
        let outcome = compute(&fx.request(path, &one, &two));
        let diff = diffed(&outcome);
        assert!(
            !(diff.before.is_empty() && diff.after.is_empty()),
            "{path}: a diff with two empty sides would render as `nothing changed`"
        );
        assert!(
            diff.before_present || diff.after_present,
            "{path}: a diff must have existed on at least one side"
        );
    }
}

// --- churn -----------------------------------------------------------

#[test]
fn churn_is_measured_or_absent_and_never_a_fabricated_zero() {
    let fx = Fixture::new();
    fx.write("src/hot.rs", "fn hot() { }\n");
    let before = fx.commit("one");
    fx.write("src/hot.rs", "fn hot() { let x = 1; }\n");
    let after = fx.commit("two");

    let bare = compute(&fx.request("src/hot.rs", &before, &after));
    let bare_diff = diffed(&bare);
    assert_eq!(bare_diff.churn, ChurnTint::Unmeasured);
    assert_eq!(bare_diff.churn.touches(), None);
    assert!(bare_diff.before.iter().all(|n| n.churn.touches().is_none()));
    assert_eq!(
        serde_json::to_string(&bare_diff.churn).unwrap(),
        "null",
        "an unmeasured tint must serialize to null, never to a number"
    );

    let tiles = vec![
        ChurnTile {
            file: "/Users/someone/proj/src/hot.rs".into(),
            touches: 7,
        },
        ChurnTile {
            file: "src/cold.rs".into(),
            touches: 3,
        },
    ];
    let tinted = compute(&fx.request("src/hot.rs", &before, &after)).with_churn(&tiles);
    let tinted_diff = diffed(&tinted);
    assert_eq!(tinted_diff.churn.touches(), Some(7));
    assert!(tinted_diff
        .before
        .iter()
        .all(|n| n.churn.touches() == Some(7)));
    assert_eq!(serde_json::to_string(&tinted_diff.churn).unwrap(), "7");

    // A file with no measured row stays Unmeasured — absence never
    // becomes zero.
    let unmeasured = compute(&fx.request("src/hot.rs", &before, &after)).with_churn(&[ChurnTile {
        file: "src/elsewhere.rs".into(),
        touches: 9,
    }]);
    assert_eq!(diffed(&unmeasured).churn, ChurnTint::Unmeasured);
}

#[test]
fn a_measured_zero_is_distinguishable_from_no_measurement() {
    let measured = ChurnTint::from_churn(
        "src/quiet.rs",
        &[ChurnTile {
            file: "src/quiet.rs".into(),
            touches: 0,
        }],
    );
    assert!(measured.is_measured());
    assert_eq!(measured.touches(), Some(0));
    assert_eq!(serde_json::to_string(&measured).unwrap(), "0");

    let absent = ChurnTint::from_churn("src/quiet.rs", &[]);
    assert!(!absent.is_measured());
    assert_eq!(absent.touches(), None);
    assert_eq!(ChurnTint::default(), ChurnTint::Unmeasured);
}

// --- truncation -------------------------------------------------------

#[test]
fn truncation_discloses_the_measured_total() {
    let fx = Fixture::new();
    let mut source = String::new();
    for index in 0..(MAX_SYMBOLS_PER_SIDE + 20) {
        source.push_str(&format!("fn f{index}() {{}}\n"));
    }
    fx.write("src/many.rs", &source);
    let before = fx.commit("one");
    source.push_str("fn extra() {}\n");
    fx.write("src/many.rs", &source);
    let after = fx.commit("two");

    let outcome = compute(&fx.request("src/many.rs", &before, &after));
    let diff = diffed(&outcome);
    assert_eq!(diff.before.len(), MAX_SYMBOLS_PER_SIDE);
    assert_eq!(diff.before_total, MAX_SYMBOLS_PER_SIDE + 20);
    assert_eq!(diff.after_total, MAX_SYMBOLS_PER_SIDE + 21);
    assert!(diff.truncated(), "a truncated tree must say so");
}

// --- cache ------------------------------------------------------------

#[test]
fn cache_hits_the_second_time_and_evicts_least_recently_used() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    fx.write("src/b.rs", "fn b() {}\n");
    fx.write("src/c.rs", "fn c() {}\n");
    let before = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    fx.write("src/b.rs", "fn b() { let x = 2; }\n");
    fx.write("src/c.rs", "fn c() { let x = 3; }\n");
    let after = fx.commit("two");

    let cache = AstDiffCache::with_limits(2, DEFAULT_CACHE_BYTES);
    let a = fx.request("src/a.rs", &before, &after);
    let b = fx.request("src/b.rs", &before, &after);
    let c = fx.request("src/c.rs", &before, &after);

    let first = cache.get_or_compute(&a);
    assert_eq!(cache.stats().misses, 1);
    assert_eq!(cache.stats().hits, 0);

    let second = cache.get_or_compute(&a);
    assert_eq!(cache.stats().hits, 1, "identical key must hit");
    assert!(
        Arc::ptr_eq(&first, &second),
        "a hit must return the stored Arc"
    );

    cache.get_or_compute(&b);
    assert_eq!(cache.stats().entries, 2);
    assert!(cache.stats().bytes > 0);

    // An insert counts as a use, so `b` is currently the more recent of
    // the two. Read `a` again to make it the most recently used; `c`
    // must then evict `b`.
    cache.get_or_compute(&a);
    assert_eq!(cache.stats().hits, 2);

    cache.get_or_compute(&c);
    assert_eq!(cache.stats().entries, 2, "entry cap must hold");
    assert_eq!(cache.stats().evictions, 1);

    cache.get_or_compute(&a);
    assert_eq!(cache.stats().hits, 3, "`a` must have survived");
    cache.get_or_compute(&b);
    assert_eq!(cache.stats().hits, 3, "`b` must have been evicted");
}

#[test]
fn byte_cap_evicts_even_below_the_entry_cap() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    fx.write("src/b.rs", "fn b() {}\n");
    let before = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    fx.write("src/b.rs", "fn b() { let x = 2; }\n");
    let after = fx.commit("two");

    let a = fx.request("src/a.rs", &before, &after);
    let single = approx_bytes(&compute(&a));
    // Room for one entry, not two.
    let cache = AstDiffCache::with_limits(64, single + 1);
    cache.get_or_compute(&a);
    assert_eq!(cache.stats().entries, 1);
    cache.get_or_compute(&fx.request("src/b.rs", &before, &after));
    assert_eq!(cache.stats().entries, 1, "byte cap must evict");
    assert!(cache.stats().bytes <= single + 1);
    assert_eq!(cache.stats().evictions, 1);
}

#[test]
fn an_entry_larger_than_the_whole_budget_is_served_but_not_stored() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let before = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    let after = fx.commit("two");

    let cache = AstDiffCache::with_limits(8, 1);
    let request = fx.request("src/a.rs", &before, &after);
    let outcome = cache.get_or_compute(&request);
    assert!(outcome.diff().is_some(), "the caller still gets the answer");
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(cache.stats().bytes, 0);
    assert_eq!(cache.stats().rejected_oversize, 1);
}

/// Both bounds hold simultaneously while entries churn, and the byte
/// counter never drifts from what is actually stored — a counter that
/// under-counts would let the cache grow past its budget silently.
#[test]
fn the_cache_holds_both_bounds_under_churn_and_its_byte_count_never_drifts() {
    let fx = Fixture::new();
    let files: Vec<String> = (0..8).map(|i| format!("src/f{i}.rs")).collect();
    for (index, file) in files.iter().enumerate() {
        fx.write(file, format!("fn f{index}() {{}}\n"));
    }
    let before = fx.commit("one");
    for (index, file) in files.iter().enumerate() {
        fx.write(file, format!("fn f{index}() {{ let x = {index}; }}\n"));
    }
    let after = fx.commit("two");

    let one_entry = approx_bytes(&compute(&fx.request(&files[0], &before, &after)));
    // Room for three entries by count, but only ~two by bytes: whichever
    // bound bites first must bite.
    let max_entries = 3;
    let max_bytes = one_entry * 2 + 1;
    let cache = AstDiffCache::with_limits(max_entries, max_bytes);

    // Two passes over eight distinct keys, so entries are inserted,
    // evicted, and re-inserted.
    for _ in 0..2 {
        for file in &files {
            cache.get_or_compute(&fx.request(file, &before, &after));
            let stats = cache.stats();
            assert!(
                stats.entries <= max_entries,
                "entry cap breached: {stats:?} after {file}"
            );
            assert!(
                stats.bytes <= max_bytes,
                "byte cap breached: {stats:?} after {file}"
            );
            let stored: usize = {
                let inner = cache.lock();
                inner.entries.values().map(|entry| entry.bytes).sum()
            };
            assert_eq!(
                stored, stats.bytes,
                "the byte counter drifted from what is stored, after {file}"
            );
        }
    }
    assert!(
        cache.stats().evictions > 0,
        "eight keys through a two-entry budget must have evicted"
    );
    assert!(
        cache.stats().entries <= 2,
        "the byte cap is the tighter one"
    );
}

/// Re-inserting the same key replaces rather than accumulates. Without
/// the subtraction in `insert`, a hot key would inflate `bytes` on every
/// recompute and evict the whole cache.
#[test]
fn reinserting_the_same_key_does_not_double_count_its_bytes() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let before = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    let after = fx.commit("two");

    let request = fx.request("src/a.rs", &before, &after);
    let key = CacheKey::of(&request);
    let value = Arc::new(compute(&request));
    let cache = AstDiffCache::new();
    for _ in 0..5 {
        cache.insert(key.clone(), value.clone());
    }
    let stats = cache.stats();
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.bytes, approx_bytes(value.as_ref()));
    assert_eq!(stats.evictions, 0);
}

#[test]
fn availability_abstentions_are_recomputed_stable_ones_are_cached() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let before = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    let after = fx.commit("two");
    let cache = AstDiffCache::new();

    // Unknown OID: availability-dependent, never cached.
    let unknown = fx.request(
        "src/a.rs",
        &before,
        "0123456789abcdef0123456789abcdef01234567",
    );
    cache.get_or_compute(&unknown);
    cache.get_or_compute(&unknown);
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 2);

    // Unsupported language: derived from the path alone, cached.
    fx.write("ios/P.swift", "class P {}\n");
    let swift_before = fx.commit("three");
    let swift = fx.request("ios/P.swift", &swift_before, &after);
    cache.get_or_compute(&swift);
    cache.get_or_compute(&swift);
    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().entries, 1);

    cache.clear();
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(cache.stats().bytes, 0);
}

#[test]
fn cache_key_separates_paths_oids_and_repositories() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let one = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    let two = fx.commit("two");
    fx.write("src/a.rs", "fn a() { let x = 2; }\n");
    let three = fx.commit("three");

    let cache = AstDiffCache::new();
    cache.get_or_compute(&fx.request("src/a.rs", &one, &two));
    cache.get_or_compute(&fx.request("src/a.rs", &one, &three));
    cache.get_or_compute(&fx.request("src/a.rs", &two, &three));
    assert_eq!(cache.stats().entries, 3, "each oid pair is its own entry");
    assert_eq!(cache.stats().hits, 0);

    let other = Fixture::new();
    other.write("src/a.rs", "fn a() {}\n");
    let other_one = other.commit("one");
    other.write("src/a.rs", "fn a() { let x = 1; }\n");
    let other_two = other.commit("two");
    cache.get_or_compute(&other.request("src/a.rs", &other_one, &other_two));
    assert_eq!(
        cache.stats().entries,
        4,
        "repo root separates identical paths"
    );
}

// --- serialization ----------------------------------------------------

#[test]
fn an_outcome_serializes_with_its_variant_named() {
    let fx = Fixture::new();
    fx.write("src/a.rs", "fn a() {}\n");
    let before = fx.commit("one");
    fx.write("src/a.rs", "fn a() { let x = 1; }\n");
    let after = fx.commit("two");

    let json = serde_json::to_string(&compute(&fx.request("src/a.rs", &before, &after))).unwrap();
    assert!(json.contains("\"diffed\""), "{json}");
    assert!(json.contains("\"changed\""));

    let abstain =
        serde_json::to_string(&compute(&fx.request("nope.swift", &before, &after))).unwrap();
    assert!(abstain.contains("\"abstained\""), "{abstain}");
    assert!(abstain.contains("unsupported_language"), "{abstain}");
}
