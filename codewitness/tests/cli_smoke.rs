#![allow(dead_code)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::TempRepo;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn labels_and_bench_run_deterministically_on_tiny_repo() {
    let fixture = Fixture::new();
    let binary = env!("CARGO_BIN_EXE_codewitness");

    let labels_first = Command::new(binary)
        .args(["labels", "--repo"])
        .arg(&fixture.repo.path)
        .output()
        .expect("run labels");
    assert_success(&labels_first);
    let labels_second = Command::new(binary)
        .args(["labels", "--repo"])
        .arg(&fixture.repo.path)
        .output()
        .expect("run labels again");
    assert_success(&labels_second);
    assert_eq!(labels_first.stdout, labels_second.stdout);
    let labels: serde_json::Value =
        serde_json::from_slice(&labels_first.stdout).expect("labels emits JSON");
    assert_eq!(labels["n_commits_reachable"], 5);
    assert_eq!(labels["n_unreleased"], 1);
    assert_eq!(labels["labels"].as_array().unwrap().len(), 5);
    let expected_order = fixture.repo.git(&["rev-list", "HEAD"]);
    let expected_order = String::from_utf8(expected_order.stdout).expect("git emits utf8 SHAs");
    let actual_order: Vec<&str> = labels["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["commit"].as_str().unwrap())
        .collect();
    assert_eq!(actual_order, expected_order.lines().collect::<Vec<_>>());

    let result_path = fixture.repo.path.join("tierA_results.json");
    fixture.run_bench(binary, &result_path);
    let first_results = fs::read(&result_path).expect("read first bench result");
    fixture.run_bench(binary, &result_path);
    let second_results = fs::read(&result_path).expect("read second bench result");
    assert_eq!(first_results, second_results);

    let results: serde_json::Value =
        serde_json::from_slice(&first_results).expect("bench emits JSON");
    assert_eq!(results["sampled_tags"].as_array().unwrap().len(), 4);
    assert_eq!(results["beliefs_scored"], 9);
    assert_eq!(
        results["provenance"]["per_tag_stamping_stats"][0]["stderr"],
        "fake successful stamper diagnostic\n"
    );
    assert_confusion(&results, "grep", (1, 0, 6, 2));
    assert_confusion(&results, "dream/CSR", (3, 1, 5, 0));
    for arm in ["recency-30", "recency-90", "recency-180"] {
        assert!(
            results["arm_metrics"].get(arm).is_some(),
            "missing {arm} metrics"
        );
    }
}

#[test]
#[ignore = "CI runs this explicitly to emit byte-for-byte determinism evidence"]
fn determinism_hashes_match_for_real_labels_and_bench_runs() {
    let fixture = Fixture::new();
    let (labels_first, labels_second, bench_first, bench_second) =
        fixture.determinism_hashes(env!("CARGO_BIN_EXE_codewitness"));

    println!("labels sha256: {labels_first}");
    println!("labels sha256: {labels_second}");
    println!("bench sha256:  {bench_first}");
    println!("bench sha256:  {bench_second}");
    assert_eq!(
        labels_first, labels_second,
        "labels output changed between runs"
    );
    assert_eq!(
        bench_first, bench_second,
        "bench output changed between runs"
    );
}

#[test]
fn bench_rejects_nonzero_stamp_exit() {
    assert_bench_fails("nonzero", "intentional stamper failure");
}

#[test]
fn bench_rejects_missing_at_commit() {
    assert_bench_fails("missing_at_commit", "omitted at_commit");
}

#[test]
fn bench_rejects_empty_ledger() {
    assert_bench_fails("empty", "final belief set is empty");
}

#[test]
fn bench_rejects_partial_ledger_insertion() {
    assert_bench_fails("partial", "ledger row count");
}

#[test]
fn bench_rejects_final_only_ledger() {
    assert_bench_fails(
        "final_only",
        "aggregate scored-belief vector is empty: sampled intermediate tags contain no committed beliefs",
    );
}

#[test]
fn bench_rejects_collapsed_belief_keys() {
    assert_bench_fails("collapsed", "unique belief count");
}

#[test]
fn bench_rejects_incoherent_stamp_counters() {
    assert_bench_fails("incoherent", "incoherent stamp-spans counters");
}

fn assert_bench_fails(mode: &str, expected_stderr: &str) {
    let fixture = Fixture::new();
    let output = fixture.bench_output(
        env!("CARGO_BIN_EXE_codewitness"),
        mode,
        &fixture.repo.path.join(format!("{mode}.json")),
    );
    assert!(
        !output.status.success(),
        "bench unexpectedly succeeded in {mode} mode\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected_stderr),
        "stderr did not contain {expected_stderr:?}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_confusion(results: &serde_json::Value, arm: &str, expected: (u64, u64, u64, u64)) {
    let metrics = &results["arm_metrics"][arm];
    assert_eq!(
        (
            metrics["tp"].as_u64().unwrap(),
            metrics["fp"].as_u64().unwrap(),
            metrics["tn"].as_u64().unwrap(),
            metrics["fn"].as_u64().unwrap(),
        ),
        expected,
        "wrong {arm} confusion matrix"
    );
}

struct Fixture {
    repo: TempRepo,
    stamper: PathBuf,
    scratch: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo = TempRepo::new();
        repo.write(
            "src/lib.rs",
            "fn method() {}\nfn other() {}\nfn changed() {}\nfn revert() {}\n",
        );
        let c0 = repo.commit_all("initial");
        repo.git(&["tag", "v8.0.0"]);

        repo.write("README.md", "one\n");
        let c1 = repo.commit_all("one");
        repo.git(&["tag", "v8.1.0"]);

        repo.write("README.md", "two\n");
        let c2 = repo.commit_all("two");
        repo.git(&["tag", "v8.2.0"]);

        repo.write("README.md", "final\n");
        let c3 = repo.commit_all("final");
        repo.git(&["tag", "v9.5.0"]);

        repo.write("README.md", "unreleased\n");
        repo.commit_all("unreleased");

        let template = repo.path.join("template.db");
        create_ledger(
            &template,
            &repo.path.join("src/lib.rs").to_string_lossy(),
            &[
                (c0.to_string(), vec![("Module::method#2", "m0")]),
                (
                    c1.to_string(),
                    vec![
                        ("Module::method#2", "m"),
                        ("Module.other#17", "o"),
                        ("Module::changed#2", "b"),
                        ("Module::gone#3", "g"),
                        ("Module::revert#4", "r"),
                    ],
                ),
                (
                    c2.to_string(),
                    vec![
                        ("Module::method#2", "m"),
                        ("Module.other#17", "o"),
                        ("Module::changed#2", "c"),
                        ("Module::revert#4", "x"),
                    ],
                ),
                (
                    c3.to_string(),
                    vec![
                        ("Module::method#2", "m"),
                        ("Module.other#17", "o"),
                        ("Module::changed#2", "c"),
                        ("Module::revert#4", "r"),
                    ],
                ),
            ],
        );

        let empty_template = repo.path.join("empty-template.db");
        create_ledger(&empty_template, "unused", &[]);
        let partial_template = repo.path.join("partial-template.db");
        create_ledger(
            &partial_template,
            &repo.path.join("src/lib.rs").to_string_lossy(),
            &[
                (c0.to_string(), vec![("Module::method#2", "m0")]),
                (c1.to_string(), vec![("Module::method#2", "m")]),
                (c2.to_string(), vec![("Module::method#2", "m")]),
                (c3.to_string(), vec![("Module::method#2", "m")]),
            ],
        );
        let final_only_template = repo.path.join("final-only-template.db");
        create_ledger(
            &final_only_template,
            &repo.path.join("src/lib.rs").to_string_lossy(),
            &[(
                c3.to_string(),
                vec![
                    ("Module::method#2", "m"),
                    ("Module.other#17", "o"),
                    ("Module::changed#2", "c"),
                    ("Module::revert#4", "r"),
                ],
            )],
        );
        let collapsed_template = repo.path.join("collapsed-template.db");
        create_ledger(
            &collapsed_template,
            &repo.path.join("src/lib.rs").to_string_lossy(),
            &[
                (c0.to_string(), vec![("Module::method#2", "m0")]),
                (
                    c1.to_string(),
                    vec![("Module::method#2", "m"), ("Module::method#2", "m2")],
                ),
                (c2.to_string(), vec![("Module::method#2", "m")]),
                (c3.to_string(), vec![("Module::method#2", "m")]),
            ],
        );

        let stamper = repo.path.join("fake-stamper.sh");
        let script = format!(
            "#!/bin/sh\n\
             [ \"$#\" -eq 10 ] || {{ echo 'bad argc' >&2; exit 64; }}\n\
             [ \"$1\" = --db-path ] || {{ echo 'missing --db-path' >&2; exit 64; }}\n\
             db=$2\n\
             [ \"$3\" = --projects-dir ] || {{ echo 'missing --projects-dir' >&2; exit 64; }}\n\
             [ \"$4\" = \"$EXPECTED_PROJECTS_DIR\" ] || {{ echo 'wrong projects dir' >&2; exit 64; }}\n\
             [ \"$5\" = codegraph ] || {{ echo 'missing codegraph' >&2; exit 64; }}\n\
             [ \"$6\" = stamp-spans ] || {{ echo 'missing stamp-spans' >&2; exit 64; }}\n\
             [ \"$7\" = --at ] || {{ echo 'missing --at' >&2; exit 64; }}\n\
             tag=$8\n\
             [ \"$9\" = --repo ] || {{ echo 'missing --repo' >&2; exit 64; }}\n\
             repo=${{10}}\n\
             [ \"$repo\" = \"$EXPECTED_REPO\" ] || {{ echo 'wrong repo' >&2; exit 64; }}\n\
             if [ \"$STAMPER_MODE\" = nonzero ]; then echo 'intentional stamper failure' >&2; exit 9; fi\n\
             echo 'fake successful stamper diagnostic' >&2\n\
             source='{}'\n\
             [ \"$STAMPER_MODE\" = empty ] && source='{}'\n\
             [ \"$STAMPER_MODE\" = partial ] && source='{}'\n\
             [ \"$STAMPER_MODE\" = final_only ] && source='{}'\n\
             [ \"$STAMPER_MODE\" = collapsed ] && source='{}'\n\
             [ \"$STAMPER_MODE\" = incoherent ] && source='{}'\n\
             if [ ! -f \"$db\" ]; then cp \"$source\" \"$db\"; fi\n\
             oid=$(git -C \"$repo\" rev-parse \"$tag^{{commit}}\") || exit 1\n\
             spans=1\n\
             [ \"$tag\" = v8.1.0 ] && spans=5\n\
             [ \"$tag\" = v8.2.0 ] && spans=4\n\
             [ \"$tag\" = v9.5.0 ] && spans=4\n\
             [ \"$STAMPER_MODE\" = empty ] && spans=0\n\
             if [ \"$STAMPER_MODE\" = final_only ] && [ \"$tag\" != v9.5.0 ]; then spans=0; fi\n\
             if [ \"$STAMPER_MODE\" = collapsed ]; then spans=1; [ \"$tag\" = v8.1.0 ] && spans=2; fi\n\
             [ \"$STAMPER_MODE\" = missing_at_commit ] || printf 'at_commit: %s\\n' \"$oid\"\n\
             checked=1\n\
             processed=1\n\
             if [ \"$STAMPER_MODE\" = empty ] || {{ [ \"$STAMPER_MODE\" = final_only ] && [ \"$tag\" != v9.5.0 ]; }}; then checked=0; processed=0; fi\n\
             [ \"$STAMPER_MODE\" = incoherent ] && processed=2\n\
             printf 'files checked: %s\\nfiles processed: %s\\nspans stamped: %s\\nwhole-file witnesses: 0\\nskipped: no_repo_root=0 file_missing=0 non_git=0 outside_repo_root=0 stamp_error=0 span_out_of_range=0 rev_unresolved=0 non_utf8=0\\ndisambiguated symbols: 0\\n' \"$checked\" \"$processed\" \"$spans\"\n",
            template.display(),
            empty_template.display(),
            partial_template.display(),
            final_only_template.display(),
            collapsed_template.display(),
            empty_template.display(),
        );
        fs::write(&stamper, script).expect("write fake stamper");
        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&stamper).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&stamper, permissions).unwrap();
        }

        let scratch = repo.path.join("scratch");
        Self {
            repo,
            stamper,
            scratch,
        }
    }

    fn run_bench(&self, binary: &str, out: &Path) {
        let output = self.bench_output(binary, "success", out);
        assert_success(&output);
    }

    fn determinism_hashes(&self, binary: &str) -> (String, String, String, String) {
        let labels = || {
            let output = Command::new(binary)
                .args(["labels", "--repo"])
                .arg(&self.repo.path)
                .output()
                .expect("run labels for determinism evidence");
            assert_success(&output);
            sha256_hex(&output.stdout)
        };
        let labels_first = labels();
        let labels_second = labels();

        let result_path = self.repo.path.join("determinism-results.json");
        self.run_bench(binary, &result_path);
        let bench_first = sha256_hex(&fs::read(&result_path).expect("read first bench result"));
        self.run_bench(binary, &result_path);
        let bench_second = sha256_hex(&fs::read(&result_path).expect("read second bench result"));

        (labels_first, labels_second, bench_first, bench_second)
    }

    fn bench_output(&self, binary: &str, mode: &str, out: &Path) -> std::process::Output {
        Command::new(binary)
            .args(["bench", "--repo"])
            .arg(&self.repo.path)
            .args(["--binary"])
            .arg(&self.stamper)
            .args(["--scratch-dir"])
            .arg(&self.scratch)
            .args(["--tags-count", "4", "--out"])
            .arg(out)
            .env("STAMPER_MODE", mode)
            .env(
                "EXPECTED_PROJECTS_DIR",
                self.scratch.join("scratch_projects"),
            )
            .env("EXPECTED_REPO", &self.repo.path)
            .output()
            .expect("run bench")
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn create_ledger(path: &Path, file: &str, rows: &[(String, Vec<(&str, &str)>)]) {
    let connection = Connection::open(path).expect("create fixture ledger");
    connection
        .execute_batch(
            "CREATE TABLE witness_ledger (
                at_oid TEXT NOT NULL,
                file TEXT NOT NULL,
                symbol TEXT,
                stamp TEXT NOT NULL,
                tier TEXT NOT NULL
            );",
        )
        .unwrap();
    for (oid, beliefs) in rows {
        for (symbol, stamp) in beliefs {
            connection
                .execute(
                    "INSERT INTO witness_ledger (at_oid, file, symbol, stamp, tier)
                     VALUES (?1, ?2, ?3, ?4, 'committed')",
                    params![oid, file, symbol, stamp],
                )
                .unwrap();
        }
    }
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
