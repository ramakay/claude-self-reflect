use super::*;
use std::io::Write;

/// Write a synthetic JSONL transcript with one Edit tool_use under a
/// Claude-style project dir, and return the projects-base path.
fn synthetic_projects(tmp: &Path, jsonl_line: &str) -> PathBuf {
    let proj = tmp.join("-Users-me-projects-demo");
    std::fs::create_dir_all(&proj).unwrap();
    let mut f = std::fs::File::create(proj.join("conv-abc.jsonl")).unwrap();
    writeln!(f, "{jsonl_line}").unwrap();
    tmp.to_path_buf()
}

fn edit_line() -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": "sess-1",
        "message": {
            "content": [{
                "type": "tool_use",
                "name": "Edit",
                "input": {
                    "file_path": "/Users/me/projects/demo/src/lib.rs",
                    "old_string": "",
                    "new_string": "fn foo() {\n    bar();\n}\nfn bar() {}\n"
                }
            }]
        }
    })
    .to_string()
}

#[test]
fn collect_edits_extracts_edit_write_multiedit() {
    let msg = serde_json::json!({
        "message": {"content": [
            {"type": "tool_use", "name": "Edit",
             "input": {"file_path": "a.rs", "new_string": "fn a() {}"}},
            {"type": "tool_use", "name": "Write",
             "input": {"file_path": "b.rs", "content": "fn b() {}"}},
            {"type": "tool_use", "name": "MultiEdit",
             "input": {"file_path": "a.rs", "edits": [
                 {"old_string": "x", "new_string": "fn c() {}"}]}},
            {"type": "tool_use", "name": "Read",
             "input": {"file_path": "ignored.rs"}},
        ]}
    });
    let mut out = BTreeMap::new();
    collect_edits(&msg, &mut out);
    assert_eq!(out.len(), 2, "Read is ignored; a.rs + b.rs only");
    assert!(out["a.rs"].contains("fn a()"));
    assert!(out["a.rs"].contains("fn c()"), "MultiEdit appends to a.rs");
    assert!(out["b.rs"].contains("fn b()"));
}

#[test]
fn backfill_extracts_nodes_and_edges_from_jsonl_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let projects_dir = synthetic_projects(tmp.path(), &edit_line());
    let storage = Storage::open_memory().unwrap();

    let stats = backfill_into(&storage, &projects_dir, false).unwrap();

    assert_eq!(stats.files_scanned, 1);
    assert_eq!(stats.conversations, 1);
    assert_eq!(stats.code_files, 1);
    assert!(stats.nodes_upserted >= 3, "module + foo + bar");
    assert!(stats.edges_written >= 1, "at least the foo->bar calls edge");
    assert_eq!(stats.projects, 1);

    // The graph is actually persisted: foo calls bar, resolvable.
    let foo_id = crate::extraction::codegraph::node_id(
        "demo",
        "/Users/me/projects/demo/src/lib.rs",
        "function",
        "foo",
    );
    let callees = storage.code_query_callees(&foo_id, 10).unwrap();
    assert!(
        callees.iter().any(|n| n.name == "bar"),
        "foo should call bar after backfill; got {:?}",
        callees.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
}

/// A file a hook snapshotted while the replay ran was read by the replay
/// before that edit: the flush must not put the older edges over it.
#[test]
fn backfill_leaves_a_file_the_live_hook_touched_since_it_started() {
    let tmp = tempfile::tempdir().unwrap();
    let projects_dir = synthetic_projects(tmp.path(), &edit_line());
    let file = "/Users/me/projects/demo/src/lib.rs";
    let live_edge = |storage: &Storage| -> bool {
        storage
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM code_edges WHERE dst_id = 'name:live_only')",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    };
    for (touched, kept) in [(true, true), (false, false)] {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|c| {
                crate::storage::codegraph::replace_file_edges(
                    c,
                    "demo",
                    file,
                    &[EdgeRow {
                        src_id: "live".into(),
                        dst_id: "name:live_only".into(),
                        kind: "calls".into(),
                        src_file: file.into(),
                        ..EdgeRow::default()
                    }],
                )?;
                if touched {
                    // Consumed after the replay began.
                    c.execute(
                        "INSERT INTO code_graph_snap_hw (project, file, seq) VALUES ('demo', ?1, ?2)",
                        rusqlite::params![file, crate::storage::codegraph::now_ns() + 60_000_000_000],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        backfill_into(&storage, &projects_dir, false).unwrap();
        assert_eq!(live_edge(&storage), kept, "touched = {touched}");
    }
}

#[test]
fn backfill_prefers_disk_file_over_fragment() {
    // JSONL records only a PARTIAL edit (just `foo`), but the real file on
    // disk has the whole module (foo -> bar, plus baz). Backfill must use
    // the disk copy so `bar`/`baz` exist and the call edge resolves.
    let tmp = tempfile::tempdir().unwrap();

    // Real source file on disk.
    let src_dir = tmp.path().join("realsrc");
    std::fs::create_dir_all(&src_dir).unwrap();
    let real_file = src_dir.join("lib.rs");
    std::fs::write(
        &real_file,
        "fn foo() {\n    bar();\n}\nfn bar() {}\nfn baz() {}\n",
    )
    .unwrap();
    let real_path = real_file.to_string_lossy().to_string();

    // JSONL transcript whose Edit fragment only mentions `foo` (no bar/baz).
    let proj = tmp.path().join("-Users-me-projects-demo");
    std::fs::create_dir_all(&proj).unwrap();
    let line = serde_json::json!({
        "type": "assistant",
        "sessionId": "sess-1",
        "message": {"content": [{
            "type": "tool_use",
            "name": "Edit",
            "input": {
                "file_path": real_path,
                "old_string": "",
                "new_string": "fn foo() {\n    bar();\n}\n"
            }
        }]}
    })
    .to_string();
    let mut f = std::fs::File::create(proj.join("conv-disk.jsonl")).unwrap();
    writeln!(f, "{line}").unwrap();

    let storage = Storage::open_memory().unwrap();
    let stats = backfill_into(&storage, tmp.path(), false).unwrap();

    assert_eq!(stats.files_from_disk, 1, "the real file came from disk");

    // `baz` exists ONLY in the disk file, never in the JSONL fragment.
    let baz = storage.code_nodes_by_name("baz", "", 10).unwrap();
    assert!(!baz.is_empty(), "baz recovered from disk, not the fragment");

    // foo -> bar resolves because both defs are present (full file).
    let foo_id = crate::extraction::codegraph::node_id("demo", &real_path, "function", "foo");
    let callees = storage.code_query_callees(&foo_id, 10).unwrap();
    assert!(
        callees.iter().any(|n| n.name == "bar"),
        "foo->bar resolved from disk file; got {:?}",
        callees.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
}

#[test]
fn dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let projects_dir = synthetic_projects(tmp.path(), &edit_line());
    let storage = Storage::open_memory().unwrap();

    let stats = backfill_into(&storage, &projects_dir, true).unwrap();
    // Counting still happens.
    assert!(stats.nodes_upserted >= 3);
    assert_eq!(stats.conversations, 1);
    assert_eq!(stats.resolution_rate, 0.0, "no resolve in dry-run");

    // But nothing was persisted: no `foo` node exists in the graph.
    let nodes = storage.code_nodes_by_name("foo", "", 10).unwrap();
    assert!(nodes.is_empty(), "dry-run must not write code_nodes");
}

// ─── WP2 Stage 2: attribution backfill ───

use crate::storage::codegraph::{upsert_node as codegraph_upsert_node, NodeRow};

fn attr_node(id: &str, file: &str, name: &str) -> NodeRow {
    NodeRow {
        id: id.into(),
        repo: "repo".into(),
        project: "proj".into(),
        file: file.into(),
        lang: "rust".into(),
        kind: "function".into(),
        name: name.into(),
        first_conv_id: "legacy".into(),
        last_conv_id: "legacy".into(),
        ..NodeRow::default()
    }
}

/// Insert a `code_evolution` row via the real production path
/// (`queries::insert_code_evolution`, timestamp = `Utc::now()` at call
/// time), acquiring the storage mutex itself. Sequential calls therefore
/// get non-decreasing timestamps, exactly like the live hook.
fn insert_evolution_event(
    storage: &Storage,
    session_id: &str,
    file_path: &str,
    functions_added: &str,
    types_added: &str,
) {
    storage
        .with_connection(|conn| {
            crate::storage::queries::insert_code_evolution(
                conn,
                session_id,
                "proj",
                file_path,
                "rust",
                "Edit",
                functions_added,
                "[]",
                types_added,
                "[]",
                "[]",
                "[]",
                None,
            )
        })
        .unwrap();
}

#[test]
fn transcript_channel_picks_the_earliest_event_not_the_latest() {
    // Fixture for the earliest-event join (H4 remediation, receipt R2):
    // two code_evolution events name the same symbol in the same file —
    // the EARLIER one must win the transcript attribution, never the
    // later one.
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "foo")).unwrap();
            Ok(())
        })
        .unwrap();

    insert_evolution_event(&storage, "sess-older", "a.rs", "[\"foo\"]", "[]");
    insert_evolution_event(&storage, "sess-newer", "a.rs", "[\"foo\"]", "[]");

    let stats = backfill_attribution_into(&storage, false).unwrap();
    assert_eq!(stats.transcript_attributed, 1);

    let rows = storage.code_attribution_rows("n1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].channel, "transcript");
    assert_eq!(
        rows[0].source_id, "sess-older",
        "earliest-inserted event must win, not the later one: {rows:?}"
    );
}

#[test]
fn transcript_channel_join_requires_matching_file_and_name() {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "foo")).unwrap();
            Ok(())
        })
        .unwrap();
    // Names "foo", but in a DIFFERENT file — must not attribute a.rs's foo.
    insert_evolution_event(&storage, "sess-1", "b.rs", "[\"foo\"]", "[]");

    let stats = backfill_attribution_into(&storage, false).unwrap();
    assert_eq!(
        stats.transcript_attributed, 0,
        "event naming 'foo' in a DIFFERENT file must not attribute a.rs's foo"
    );
    assert_eq!(
        storage.code_attribution_for_node("n1").unwrap(),
        "unattributed"
    );
}

#[test]
fn nodes_with_no_matching_event_are_unattributed_not_falling_back_to_first_conv_id() {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "orphan")).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = backfill_attribution_into(&storage, false).unwrap();
    assert_eq!(stats.transcript_attributed, 0);
    assert_eq!(stats.nodes_checked, 1);
    assert_eq!(
        storage.code_attribution_for_node("n1").unwrap(),
        "unattributed",
        "no event and no git span must never fall back to first_conv_id"
    );
}

#[test]
fn dry_run_attribution_backfill_writes_nothing() {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "foo")).unwrap();
            crate::storage::queries::insert_code_evolution(
                conn,
                "sess-1",
                "proj",
                "a.rs",
                "rust",
                "Edit",
                "[\"foo\"]",
                "[]",
                "[]",
                "[]",
                "[]",
                "[]",
                None,
            )
            .unwrap();
            Ok(())
        })
        .unwrap();

    let stats = backfill_attribution_into(&storage, true).unwrap();
    assert_eq!(stats.transcript_attributed, 1, "counting still happens");
    assert_eq!(
        storage.code_attribution_for_node("n1").unwrap(),
        "unattributed",
        "dry-run must not write code_node_attribution"
    );
}

#[test]
fn git_channel_skips_module_sentinel_but_not_line_one_symbols() {
    // CodeRabbit PR #279: `span_start` is 0-based, so a real symbol
    // defined on line 1 also has `span_start == 0`. Only the module
    // sentinel (kind='module', 0/0 span) is span-invalid; a line-1
    // function must proceed past the span gate (here it then falls to
    // git_file_missing, proving it was NOT counted as invalid span).
    let storage = Storage::open_memory().unwrap();
    let mut sentinel = attr_node("n1", "/nonexistent/a.rs", "a.rs");
    sentinel.kind = "module".into();
    sentinel.span_start = 0;
    sentinel.span_end = 0;
    let mut line_one_fn = attr_node("n2", "/nonexistent/b.rs", "foo");
    line_one_fn.span_start = 0;
    line_one_fn.span_end = 3;
    storage
        .with_connection(|conn| {
            codegraph_upsert_node(conn, &sentinel).unwrap();
            codegraph_upsert_node(conn, &line_one_fn).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = backfill_attribution_into(&storage, false).unwrap();
    assert_eq!(stats.git_invalid_span, 1, "only the module sentinel");
    assert_eq!(stats.git_file_missing, 1, "line-1 fn passed the span gate");
    assert_eq!(stats.git_attributed, 0);
}

#[test]
fn git_log_introducing_commit_returns_the_oldest_commit_not_the_newest() {
    // R8's exact trap: a naive `-1 --reverse` returns the NEWEST commit
    // because `-n`/`-1` applies before `--reverse`. This test creates two
    // commits touching the same line and asserts the FIRST (oldest) one
    // is returned.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| git_in(repo).args(args).status();
    if git(&["init", "-q"]).map(|s| !s.success()).unwrap_or(true) {
        return; // git unavailable — fail-soft skip, matches repo_root.rs precedent
    }
    git(&["config", "user.email", "t@example.com"]).unwrap();
    git(&["config", "user.name", "Test"]).unwrap();

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
    git(&["add", "lib.rs"]).unwrap();
    git(&["commit", "-q", "-m", "introduce foo"]).unwrap();
    let oldest_hash = git_head(repo);

    // A second commit that also touches line 2 (same range).
    std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
    git(&["commit", "-q", "-am", "tweak foo"]).unwrap();

    let got = git_log_introducing_commit(&repo.to_string_lossy(), "lib.rs", 1, 3);
    let (hash, _ts) = got.expect("introducing commit must resolve");
    assert_eq!(
        hash, oldest_hash,
        "must return the OLDEST commit touching the range, not the newest"
    );
}

// ─── stamp-spans backfill (v10 "dreaming" substrate) ───

/// A `git -C <repo>` command with the hook-time environment stripped.
/// When this suite runs under a `git commit` hook (our pre-commit runs
/// `cargo test --lib`), git exports `GIT_DIR`/`GIT_INDEX_FILE`/... —
/// absolute paths in a linked worktree — which would silently redirect
/// these temp-repo commands at the REAL repository (staging test files
/// into the actual index). Strip every `GIT_*` var so the command only
/// ever sees the temp repo.
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
    if git(&["init", "-q"]).map(|s| !s.success()).unwrap_or(true) {
        return false; // git unavailable — fail-soft skip, matches repo_root.rs precedent
    }
    git(&["config", "user.email", "t@example.com"]).unwrap();
    git(&["config", "user.name", "Test"]).unwrap();
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

/// Fixture source: two `impl` blocks, each defining a same-named
/// `is_empty` method — the adversarial-gate audit's proven false
/// positive (`CodeContext::is_empty` joined to `AstDiff::is_empty` under
/// the old bare-name identity). 0-based line numbers (matching
/// `NodeRow::span_start`/`span_end`'s convention): the first
/// `is_empty` spans lines 3-5 inside `impl CodeContext` (lines 2-6); the
/// second spans lines 11-13 inside `impl AstDiff` (lines 10-14).
fn dup_impl_fixture_src() -> &'static str {
    "struct CodeContext;\n\nimpl CodeContext {\n    fn is_empty(&self) -> bool {\n        true\n    }\n}\n\nstruct AstDiff;\n\nimpl AstDiff {\n    fn is_empty(&self) -> bool {\n        false\n    }\n}\n"
}

#[test]
fn collision_rate_probe_fixture_corpus() {
    use ast_grep_language::SupportLang;

    struct Case {
        category: &'static str,
        lang: SupportLang,
        source: &'static str,
        file: &'static str,
        expected_disambiguated: usize,
    }

    let cases = vec![
        Case {
            category: "ts_nested_functions",
            lang: SupportLang::TypeScript,
            source: "function alpha() {\n  function shared() {}\n}\nfunction beta() {\n  function shared() {}\n}\n",
            file: "/repo/nested.ts",
            expected_disambiguated: 0,
        },
        Case {
            category: "ts_object_literal_methods",
            lang: SupportLang::TypeScript,
            source: "const handlers = {\n  left: {\n    run() {}\n  },\n  right: {\n    run() {}\n  }\n};\n",
            file: "/repo/objects.ts",
            expected_disambiguated: 0,
        },
        Case {
            category: "ts_arrow_const_scopes",
            lang: SupportLang::TypeScript,
            source: "const left = () => {\n  function shared() {}\n};\nconst right = () => {\n  function shared() {}\n};\n",
            file: "/repo/arrows.ts",
            expected_disambiguated: 0,
        },
        Case {
            category: "ts_module_overloads",
            lang: SupportLang::TypeScript,
            source: "function parse(value: string) { return value; }\nfunction parse(value: number) { return value; }\nfunction parse(value: string | number) { return value; }\n",
            file: "/repo/overloads.ts",
            expected_disambiguated: 2,
        },
        Case {
            category: "python_nested_defs",
            lang: SupportLang::Python,
            source: "def outer_a():\n    def shared():\n        pass\n\ndef outer_b():\n    def shared():\n        pass\n",
            file: "/repo/nested.py",
            expected_disambiguated: 0,
        },
        Case {
            category: "python_property_pairs",
            lang: SupportLang::Python,
            source: "class Box:\n    @property\n    def value(self):\n        return self._value\n    @value.setter\n    def value(self, value):\n        self._value = value\n",
            file: "/repo/properties.py",
            expected_disambiguated: 1,
        },
    ];

    let mut total_defs = 0usize;
    let mut total_legacy_disambiguated = 0usize;
    let mut total_disambiguated = 0usize;
    for case in cases {
        let fragment = extract_graph_fragment(
            case.source,
            case.lang,
            case.file,
            "repo",
            "proj",
            "conv",
            "session",
        );
        let defs: Vec<_> = fragment
            .nodes
            .iter()
            .filter(|node| node.kind == "function")
            .map(|node| {
                (
                    node.kind.clone(),
                    node.name.clone(),
                    node.span_start,
                    node.span_end,
                )
            })
            .collect();
        let containers = container_spans(case.source, case.lang);
        let legacy_containers: Vec<_> = containers
            .iter()
            .filter(|container| !container.name.contains(container.separator))
            .filter(|container| {
                case.source
                    .lines()
                    .nth(container.start as usize)
                    .is_some_and(|line| line.trim_start().starts_with("class "))
            })
            .cloned()
            .collect();
        let (_, legacy_disambiguated) = qualify_witness_symbols(&defs, &legacy_containers);
        let (symbols, disambiguated) = qualify_witness_symbols(&defs, &containers);
        eprintln!(
            "collision-probe {}: defs={} before_#N={} after_#N={} symbols={symbols:?}",
            case.category,
            defs.len(),
            legacy_disambiguated,
            disambiguated
        );
        assert_eq!(
            disambiguated, case.expected_disambiguated,
            "category {} symbols={symbols:?} containers={containers:?}",
            case.category
        );
        total_defs += defs.len();
        total_legacy_disambiguated += legacy_disambiguated;
        total_disambiguated += disambiguated;
    }
    eprintln!(
        "collision-probe total: defs={total_defs} before_#N={total_legacy_disambiguated} after_#N={total_disambiguated} rate={:.1}%",
        100.0 * total_disambiguated as f64 / total_defs as f64
    );
    assert_eq!((total_defs, total_legacy_disambiguated), (17, 7));
    assert_eq!((total_defs, total_disambiguated), (17, 3));
}

fn qualified_fixture_symbols(
    source: &str,
    lang: ast_grep_language::SupportLang,
    defs: &[(&str, i64, i64)],
) -> (Vec<String>, usize) {
    let defs: Vec<_> = defs
        .iter()
        .map(|(name, start, end)| ("function".to_string(), (*name).to_string(), *start, *end))
        .collect();
    qualify_witness_symbols(&defs, &container_spans(source, lang))
}

#[test]
fn qualifies_typescript_nested_functions_by_function_parent() {
    use ast_grep_language::SupportLang;

    let source = "function alpha() {\n  function shared() {}\n}\nfunction beta() {\n  function shared() {}\n}\n";
    let (symbols, disambiguated) = qualified_fixture_symbols(
        source,
        SupportLang::TypeScript,
        &[
            ("alpha", 0, 2),
            ("shared", 1, 1),
            ("beta", 3, 5),
            ("shared", 4, 4),
        ],
    );
    assert_eq!(symbols, ["alpha", "alpha.shared", "beta", "beta.shared"]);
    assert_eq!(disambiguated, 0);
}

#[test]
fn qualifies_typescript_object_methods_by_property_key_path() {
    use ast_grep_language::SupportLang;

    let source =
        "const handlers = {\n  left: {\n    run() {}\n  },\n  right: {\n    run() {}\n  }\n};\n";
    let (symbols, disambiguated) = qualified_fixture_symbols(
        source,
        SupportLang::TypeScript,
        &[("run", 2, 2), ("run", 5, 5)],
    );
    assert_eq!(symbols, ["handlers.left.run", "handlers.right.run"]);
    assert_eq!(disambiguated, 0);
}

#[test]
fn qualifies_typescript_nested_functions_by_arrow_const_parent() {
    use ast_grep_language::SupportLang;

    let source = "const left = () => {\n  function shared() {}\n};\nconst right = () => {\n  function shared() {}\n};\n";
    let (symbols, disambiguated) = qualified_fixture_symbols(
        source,
        SupportLang::TypeScript,
        &[("shared", 1, 1), ("shared", 4, 4)],
    );
    assert_eq!(symbols, ["left.shared", "right.shared"]);
    assert_eq!(disambiguated, 0);
}

#[test]
fn rederive_symbol_spans_qualifies_methods_and_abstains_when_unparseable() {
    let src = b"struct Ab;\n\nimpl Ab {\n    fn is_empty(&self) -> bool {\n        true\n    }\n}\n\nfn free() {}\n";
    let all = rederive_symbol_spans(src, "/r/a.rs", "/r", "p", "abc", &|_| true)
        .expect("clean rust parse");
    assert_eq!(all.nodes.len(), all.symbols.len());
    assert!(
        all.symbols.iter().any(|s| s == "Ab::is_empty"),
        "{:?}",
        all.symbols
    );
    assert!(all.symbols.iter().any(|s| s == "free"), "{:?}", all.symbols);
    let only_free =
        rederive_symbol_spans(src, "/r/a.rs", "/r", "p", "abc", &|n| n.name == "free").unwrap();
    assert_eq!(only_free.symbols, ["free"]);
    assert!(rederive_symbol_spans(src, "/r/notes.txt", "/r", "p", "abc", &|_| true).is_none());
    assert!(rederive_symbol_spans(
        &[0xff, 0xfe, 0x00, 0x01, 0x02],
        "/r/a.rs",
        "/r",
        "p",
        "abc",
        &|_| true
    )
    .is_none());
}

#[test]
fn qualifies_python_nested_defs_by_function_parent() {
    use ast_grep_language::SupportLang;

    let source = "def outer_a():\n    def shared():\n        pass\n\ndef outer_b():\n    def shared():\n        pass\n";
    let (symbols, disambiguated) = qualified_fixture_symbols(
        source,
        SupportLang::Python,
        &[
            ("outer_a", 0, 2),
            ("shared", 1, 2),
            ("outer_b", 4, 6),
            ("shared", 5, 6),
        ],
    );
    assert_eq!(
        symbols,
        ["outer_a", "outer_a.shared", "outer_b", "outer_b.shared"]
    );
    assert_eq!(disambiguated, 0);
}

#[test]
fn qualify_witness_symbols_disambiguates_residual_collisions_by_span_order() {
    // Two defs that land on the identical bare symbol (empty
    // `containers` — qualification cannot resolve them, e.g. two free
    // functions or a container this pass couldn't name) must be
    // disambiguated deterministically by SPAN order, never silently
    // joined — and never by `defs`' own input order (index 0 here is
    // the span-LATER occurrence, deliberately, to prove the sort is by
    // span, not position in the input slice).
    let defs = vec![
        ("function".to_string(), "orphan".to_string(), 10i64, 12i64),
        ("function".to_string(), "orphan".to_string(), 1i64, 3i64),
    ];
    let (qualified, disambiguated) = qualify_witness_symbols(&defs, &[]);
    assert_eq!(disambiguated, 1);
    assert_eq!(
        qualified[1], "orphan",
        "span-earliest occurrence stays bare"
    );
    assert_eq!(
        qualified[0], "orphan#2",
        "span-later occurrence gets the safety-net suffix"
    );
}

#[test]
fn qualify_witness_symbols_leaves_non_function_kinds_bare() {
    // `type`/`const` rows are never container-qualified — only
    // `kind == "function"` is (methods are the only shape that can
    // collide across impl/class bodies).
    let defs = vec![("type".to_string(), "Thing".to_string(), 0i64, 2i64)];
    let containers = vec![ContainerSpan {
        name: "Outer".into(),
        separator: "::",
        start: 0,
        end: 5,
    }];
    let (qualified, disambiguated) = qualify_witness_symbols(&defs, &containers);
    assert_eq!(disambiguated, 0);
    assert_eq!(qualified[0], "Thing");
}

#[test]
fn stamp_spans_into_qualifies_symbol_identity_by_impl_container() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("dup.rs");
    std::fs::write(&file, dup_impl_fixture_src()).unwrap();
    git_in(repo).args(["add", "dup.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();

    let file_path = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();

    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            // Simulate an existing corpus already damaged by the old
            // `(repo,file,kind,name)` collapse: only ONE bare method row
            // survived. The HEAD pass must re-extract the file and mint
            // both physical definitions into a fresh append-only fork.
            let mut diff_is_empty = attr_node("n-diff", &file_path, "is_empty");
            diff_is_empty.repo_root = Some(repo_root.clone());
            diff_is_empty.span_start = 11;
            diff_is_empty.span_end = 13;
            codegraph_upsert_node(conn, &diff_is_empty).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into(&storage, false).unwrap();
    assert_eq!(stats.spans_stamped, 2);
    assert_eq!(
        stats.disambiguated_symbols, 0,
        "container qualification alone resolves this — no residual collision"
    );

    let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
    assert_eq!(rows.len(), 2);
    let symbols: BTreeSet<_> = rows.iter().map(|r| r.symbol.clone()).collect();
    assert_eq!(
        symbols,
        BTreeSet::from([
            Some("CodeContext::is_empty".to_string()),
            Some("AstDiff::is_empty".to_string()),
        ]),
        "the two impl-block methods must resolve to DISTINCT container-qualified \
             symbols, never both landing on bare 'is_empty'"
    );
}

#[test]
fn stamp_failure_publishes_no_partial_rederived_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("dup.rs");
    std::fs::write(&file, dup_impl_fixture_src()).unwrap();
    git_in(repo).args(["add", "dup.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();

    let file_path = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            let mut collapsed = attr_node("n-shared", &file_path, "is_empty");
            collapsed.repo_root = Some(repo_root.clone());
            collapsed.first_conv_id = "conv-partial".into();
            collapsed.last_conv_id = "conv-partial".into();
            codegraph_upsert_node(conn, &collapsed)?;
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into_cancellable_with(&storage, false, None, &|anchor, bytes| {
        if anchor.symbol.as_deref() == Some("AstDiff::is_empty") {
            anyhow::bail!("forced sibling stamp failure");
        }
        Ok(codewitness::Auditor::stamp_file_content(anchor, bytes)?
            .as_str()
            .to_string())
    })
    .unwrap();
    assert_eq!(stats.spans_stamped, 1, "one sibling hashes successfully");
    assert_eq!(
        stats.skipped_stamp_error, 1,
        "one sibling is forced to fail"
    );

    storage
        .with_connection(|conn| {
            let published: i64 = conn.query_row(
                "SELECT COUNT(*) FROM witness_ledger
                     WHERE project = 'proj' AND file = ?1
                       AND source_kind = 'backfill_rederived_v2'",
                rusqlite::params![file_path],
                |row| row.get(0),
            )?;
            let incomplete: i64 = conn.query_row(
                "SELECT COUNT(*) FROM witness_generations
                     WHERE project = 'proj' AND file = ?1 AND status = 'incomplete'",
                rusqlite::params![file_path],
                |row| row.get(0),
            )?;
            assert_eq!(
                published, 0,
                "successful sibling must not be published alone"
            );
            assert_eq!(incomplete, 1, "failed run remains observable as incomplete");

            let bound = crate::storage::chunk_binding::witness_verdict_for_chunks(
                conn,
                &["conv-partial".to_string()],
            )?;
            assert!(
                bound.is_empty(),
                "binding must abstain instead of seeing the successful sibling"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn stamp_spans_into_rederivation_drops_stale_same_name_span() {
    // Persisted graph rows are candidates, not authority. The second
    // row below claims `orphan` at a span whose source actually defines
    // `orphan_two`; production re-extraction must not infer that it is a
    // second `orphan` merely because an old code_nodes row says so.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("dup2.rs");
    std::fs::write(
        &file,
        "fn orphan() {\n    1\n}\n\nfn orphan_two() {\n    2\n}\n",
    )
    .unwrap();
    git_in(repo).args(["add", "dup2.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();

    let file_path = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();

    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            let mut a = attr_node("n-a", &file_path, "orphan");
            a.repo_root = Some(repo_root.clone());
            a.span_start = 0;
            a.span_end = 2;
            codegraph_upsert_node(conn, &a).unwrap();

            // Stale/corrupt same-name row at `orphan_two`'s span.
            let mut b = attr_node("n-b", &file_path, "orphan");
            b.repo_root = Some(repo_root.clone());
            b.span_start = 4;
            b.span_end = 6;
            codegraph_upsert_node(conn, &b).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into(&storage, false).unwrap();
    assert_eq!(stats.spans_stamped, 1);
    assert_eq!(stats.disambiguated_symbols, 0);

    let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
    let symbols: BTreeSet<_> = rows.iter().map(|r| r.symbol.clone()).collect();
    assert_eq!(symbols, BTreeSet::from([Some("orphan".to_string())]),);
}

#[test]
fn stamp_spans_mints_committed_witnesses_matching_head_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n\nfn bar() {\n    2\n}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();
    let head = git_head(repo);

    let file_path = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();

    let storage = Storage::open_memory().unwrap();
    storage
        .insert_chunk(
            &crate::import::ConversationChunk {
                id: "chunk-foo-edit".into(),
                conversation_id: "conv".into(),
                project_name: "proj".into(),
                timestamp: "2026-08-09T00:00:00Z".into(),
                content: "edited foo".into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::Assistant,
                seq: 0,
                is_sidechain: false,
            },
            &[0.0; 4],
        )
        .unwrap();
    storage
        .with_connection(|conn| {
            let foo_id =
                crate::extraction::codegraph::node_id("proj", &file_path, "function", "foo");
            let mut foo = attr_node(&foo_id, &file_path, "foo");
            foo.repo_root = Some(repo_root.clone());
            foo.span_start = 0;
            foo.span_end = 2;
            codegraph_upsert_node(conn, &foo).unwrap();
            crate::storage::codegraph::set_last_chunk_id(conn, &foo_id, "chunk-foo-edit")?;

            let bar_id =
                crate::extraction::codegraph::node_id("proj", &file_path, "function", "bar");
            let mut bar = attr_node(&bar_id, &file_path, "bar");
            bar.repo_root = Some(repo_root.clone());
            bar.span_start = 4;
            bar.span_end = 6;
            codegraph_upsert_node(conn, &bar).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into(&storage, false).unwrap();
    assert_eq!(stats.files_processed, 1);
    assert_eq!(stats.spans_stamped, 2, "foo + bar");
    assert_eq!(
        stats.whole_files_stamped, 0,
        "spans exist, no whole-file fallback"
    );

    let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r.tier, "committed");
        assert_eq!(r.at_oid.as_deref(), Some(head.as_str()));
        assert_eq!(r.source_kind, "backfill_rederived_v2");
    }

    // Stamps must match codewitness's own `stamp_at` output exactly.
    let auditor = codewitness::Auditor::discover(&repo_root).unwrap();
    let head_oid: codewitness::ObjectId = head.parse().unwrap();
    let foo_anchor = codewitness::Anchor::new("lib.rs")
        .with_symbol("foo")
        .with_span(1, 3);
    let expected_foo = auditor.stamp_at(&foo_anchor, head_oid).unwrap();
    let foo_row = rows
        .iter()
        .find(|r| r.symbol.as_deref() == Some("foo"))
        .unwrap();
    assert_eq!(foo_row.stamp, expected_foo.stamp().as_str());
    assert_eq!(foo_row.span_start, Some(0));
    assert_eq!(foo_row.span_end, Some(2));
    storage
        .with_connection(|conn| {
            let bound_chunk: String = conn.query_row(
                "SELECT chunk_id FROM witness_chunk_bindings WHERE witness_id = ?1",
                rusqlite::params![foo_row.id],
                |row| row.get(0),
            )?;
            assert_eq!(bound_chunk, "chunk-foo-edit");
            let bar_bindings: i64 = conn.query_row(
                "SELECT COUNT(*) FROM witness_chunk_bindings b
                     JOIN witness_ledger w ON w.id = b.witness_id
                     WHERE w.symbol = 'bar'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(bar_bindings, 0, "unattributed sibling must stay unbound");
            Ok(())
        })
        .unwrap();

    // Idempotency plus cadence short-circuit: rerunning at the same HEAD
    // and extractor version must do no blob parsing or span stamping.
    let stats2 = stamp_spans_into(&storage, false).unwrap();
    assert_eq!(stats2.files_processed, 0);
    assert_eq!(stats2.spans_stamped, 0);
    assert_eq!(stats2.skipped_complete_generation, 1);
    let rows_after = storage.witnesses_for_file("proj", &file_path).unwrap();
    assert_eq!(rows_after.len(), 2, "rerun must not add new rows");
}

#[test]
fn stamp_spans_falls_back_to_whole_file_witness_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("data.rs");
    std::fs::write(&file, "// just a comment, no functions\n").unwrap();
    git_in(repo).args(["add", "data.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();
    let head = git_head(repo);

    let file_path = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();

    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            // Only the synthetic module sentinel — no function/type/const spans.
            let mut module = attr_node("n-mod", &file_path, &file_path);
            module.kind = "module".into();
            module.repo_root = Some(repo_root.clone());
            module.span_start = 0;
            module.span_end = 0;
            codegraph_upsert_node(conn, &module).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into(&storage, false).unwrap();
    assert_eq!(stats.spans_stamped, 0);
    assert_eq!(stats.whole_files_stamped, 1);

    let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].symbol, None);
    assert_eq!(rows[0].at_oid.as_deref(), Some(head.as_str()));

    // Idempotent: whole-file rows are NULL-keyed, and the COALESCE-based
    // `idx_witness_ledger_identity` UNIQUE index (+ INSERT OR IGNORE)
    // dedupes them at the DB level — see `storage::witness_ledger`'s
    // module doc.
    let stats2 = stamp_spans_into(&storage, false).unwrap();
    assert_eq!(
        stats2.whole_files_stamped, 0,
        "unchanged generation skipped"
    );
    assert_eq!(stats2.skipped_complete_generation, 1);
    let rows_after = storage.witnesses_for_file("proj", &file_path).unwrap();
    assert_eq!(
        rows_after.len(),
        1,
        "whole-file rerun must not duplicate (identity index)"
    );
}

#[test]
fn stamp_spans_skips_span_that_overflows_u32_line_range() {
    // A corrupt persisted span near i64::MAX is not reproduced by the
    // production extractor, so re-derivation drops it without attempting
    // a lossy conversion. With no matching current anchor, the whole-file
    // fallback still fires.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();

    let file_path = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();

    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            // `span_end + 1` would overflow i64; also exercises the
            // u32::try_from bound via span_start > u32::MAX.
            let mut corrupt = attr_node("n-corrupt", &file_path, "corrupt");
            corrupt.repo_root = Some(repo_root.clone());
            corrupt.span_start = i64::from(u32::MAX) + 1;
            corrupt.span_end = i64::MAX;
            codegraph_upsert_node(conn, &corrupt).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into(&storage, false).expect("must never panic or fail");
    assert_eq!(stats.skipped_span_out_of_range, 0);
    assert_eq!(stats.spans_stamped, 0);
    assert_eq!(
        stats.whole_files_stamped, 1,
        "no valid span survived, so the whole-file fallback applies"
    );
    let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].symbol, None, "only the whole-file witness lands");
}

#[test]
fn stamp_spans_skips_missing_file_and_non_git_repo_without_failing() {
    let storage = Storage::open_memory().unwrap();

    // (a) file no longer exists on disk.
    storage
        .with_connection(|conn| {
            let mut ghost = attr_node("n-ghost", "/nonexistent/ghost.rs", "ghost_fn");
            ghost.repo_root = Some("/tmp".into());
            ghost.span_start = 0;
            ghost.span_end = 1;
            codegraph_upsert_node(conn, &ghost).unwrap();
            Ok(())
        })
        .unwrap();

    // (b) file exists, but its repo_root has no discoverable git repository.
    let tmp = tempfile::tempdir().unwrap();
    let non_git_dir = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&non_git_dir).unwrap();
    let plain_file = non_git_dir.join("plain.rs");
    std::fs::write(&plain_file, "fn plain() {}\n").unwrap();
    let plain_path = plain_file.to_string_lossy().to_string();
    storage
        .with_connection(|conn| {
            let mut plain = attr_node("n-plain", &plain_path, "plain");
            plain.repo_root = Some(non_git_dir.to_string_lossy().to_string());
            plain.span_start = 0;
            plain.span_end = 0;
            codegraph_upsert_node(conn, &plain).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into(&storage, false).expect("must never fail the run");
    assert_eq!(stats.skipped_file_missing, 1);
    assert_eq!(stats.skipped_non_git, 1);
    assert_eq!(stats.spans_stamped, 0);
    assert_eq!(stats.whole_files_stamped, 0);
}

#[test]
fn stamp_spans_dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();

    let file_path = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();

    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            let mut foo = attr_node("n-foo", &file_path, "foo");
            foo.repo_root = Some(repo_root.clone());
            foo.span_start = 0;
            foo.span_end = 2;
            codegraph_upsert_node(conn, &foo).unwrap();
            Ok(())
        })
        .unwrap();

    let stats = stamp_spans_into(&storage, true).unwrap();
    assert_eq!(stats.spans_stamped, 1, "counting still happens");
    assert!(
        storage
            .witnesses_for_file("proj", &file_path)
            .unwrap()
            .is_empty(),
        "dry-run must not write witness_ledger"
    );
}

// ─── historical mode: `stamp-spans --at <rev>` ───

#[test]
fn stamp_spans_at_qualifies_symbol_identity_by_impl_container() {
    // Real v9.5 shape: coexisting same-named methods in two impl blocks
    // must both survive the production extractor and reach the ledger.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("dup.rs");
    let src = dup_impl_fixture_src();
    std::fs::write(&file, src).unwrap();
    git_in(repo).args(["add", "dup.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "init"])
        .status()
        .unwrap();
    let commit = git_head(repo);

    let file_abs = file.to_string_lossy().to_string();
    let repo_root = repo.to_string_lossy().to_string();
    let storage = Storage::open_memory().unwrap();

    let stats = stamp_spans_historical_into(&storage, &commit, Some(&repo_root), false).unwrap();
    // Two struct declarations + two same-named methods.
    assert_eq!(stats.spans_stamped, 4);
    assert_eq!(stats.disambiguated_symbols, 0);

    let rows = storage.witnesses_for_file("", &file_abs).unwrap();
    assert_eq!(rows.len(), 4);
    let symbols: BTreeSet<_> = rows.iter().map(|r| r.symbol.clone()).collect();
    assert_eq!(
        symbols,
        BTreeSet::from([
            // The struct decl itself is `kind == "type"` — qualification
            // only ever applies to `kind == "function"` (methods).
            Some("CodeContext".to_string()),
            Some("CodeContext::is_empty".to_string()),
            Some("AstDiff".to_string()),
            Some("AstDiff::is_empty".to_string()),
        ]),
        "both impl-block methods survive extraction and receive distinct qualified names"
    );
}

#[test]
fn stamp_spans_at_cross_commit_join_does_not_collide_across_impl_blocks() {
    // The exact scenario the adversarial-gate audit flagged: two
    // UNRELATED `is_empty` methods, in two DIFFERENT `impl` blocks,
    // whose historical witnesses must never look like "the same
    // symbol's history" just because they share a bare name. Modeled
    // across commits (each commit has only ONE `is_empty` definition,
    // so this never touches the separate, out-of-scope `NodeRow`
    // collision `extract_inner`'s own per-parse node collection has
    // for TWO same-named methods co-existing in a SINGLE parse — see
    // `stamp_spans_at_qualifies_symbol_identity_by_impl_container`'s
    // doc comment): commit1 has `CodeContext::is_empty`; commit2
    // REPLACES it with an unrelated `AstDiff::is_empty`. Before this
    // fix, both would mint the identical bare `is_empty` symbol and a
    // naive `(file, symbol)` lookup across the two commits would read
    // as one method's evolution — nonsensical, since they are two
    // unrelated types' methods.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("dup.rs");
    let src1 = "struct CodeContext;\n\nimpl CodeContext {\n    fn is_empty(&self) -> bool {\n        true\n    }\n}\n";
    std::fs::write(&file, src1).unwrap();
    git_in(repo).args(["add", "dup.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "c1"])
        .status()
        .unwrap();
    let commit1 = git_head(repo);

    // Commit2 replaces CodeContext's impl entirely with AstDiff's —
    // only one `is_empty` definition exists in the file AT EACH commit.
    let src2 = "struct AstDiff;\n\nimpl AstDiff {\n    fn is_empty(&self) -> bool {\n        false\n    }\n}\n";
    std::fs::write(&file, src2).unwrap();
    git_in(repo).args(["add", "dup.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "c2"])
        .status()
        .unwrap();
    let commit2 = git_head(repo);

    let repo_root = repo.to_string_lossy().to_string();
    let file_abs = file.to_string_lossy().to_string();
    let storage = Storage::open_memory().unwrap();

    stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), false).unwrap();
    stamp_spans_historical_into(&storage, &commit2, Some(&repo_root), false).unwrap();

    let rows = storage.witnesses_for_file("", &file_abs).unwrap();
    // Each commit's file has 1 struct decl (type) + 1 impl method
    // (function) = 2 rows/commit x 2 commits.
    assert_eq!(
        rows.len(),
        4,
        "struct decl + is_empty witness per commit, correctly qualified by ITS OWN container"
    );

    let ctx_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.symbol.as_deref() == Some("CodeContext::is_empty"))
        .collect();
    let diff_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.symbol.as_deref() == Some("AstDiff::is_empty"))
        .collect();
    assert_eq!(ctx_rows.len(), 1, "commit1's witness");
    assert_eq!(diff_rows.len(), 1, "commit2's witness");
    assert_eq!(ctx_rows[0].at_oid.as_deref(), Some(commit1.as_str()));
    assert_eq!(diff_rows[0].at_oid.as_deref(), Some(commit2.as_str()));
    assert!(
        rows.iter().all(|r| r.symbol.as_deref() != Some("is_empty")),
        "no row should ever mint the collision-prone bare symbol for a method \
             defined inside an impl block — a bare-symbol join here would incoherently \
             treat two unrelated types' methods as one symbol's history"
    );
}

#[test]
fn stamp_spans_at_differs_for_changed_symbol_and_matches_for_unchanged_across_two_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n\nfn bar() {\n    2\n}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "c1"])
        .status()
        .unwrap();
    let commit1 = git_head(repo);

    // Change ONLY foo's body between commits; bar is byte-identical.
    std::fs::write(&file, "fn foo() {\n    999\n}\n\nfn bar() {\n    2\n}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "c2"])
        .status()
        .unwrap();
    let commit2 = git_head(repo);

    let repo_root = repo.to_string_lossy().to_string();
    let file_abs = file.to_string_lossy().to_string();
    let storage = Storage::open_memory().unwrap();

    // Repo the graph has never seen (no `code_nodes` seeded): `--repo`
    // targets it directly, project tag falls back to "".
    let stats1 = stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), false).unwrap();
    assert_eq!(stats1.spans_stamped, 2, "foo + bar at commit1");
    assert_eq!(stats1.skipped_rev_unresolved, 0);
    assert_eq!(
        stats1.at_commits,
        vec![(repo_root.clone(), commit1.clone())]
    );
    assert!(stats1
        .format_text(false)
        .contains(&format!("at_commit: {commit1}")));

    let stats2 = stamp_spans_historical_into(&storage, &commit2, Some(&repo_root), false).unwrap();
    assert_eq!(stats2.spans_stamped, 2, "foo + bar at commit2");

    let rows = storage.witnesses_for_file("", &file_abs).unwrap();
    assert_eq!(rows.len(), 4, "foo+bar at each of 2 commits");

    let foo_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.symbol.as_deref() == Some("foo"))
        .collect();
    let bar_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.symbol.as_deref() == Some("bar"))
        .collect();
    assert_eq!(foo_rows.len(), 2, "foo stamped at both commits");
    assert_eq!(bar_rows.len(), 2, "bar stamped at both commits");
    for r in rows.iter() {
        assert_eq!(r.tier, "committed");
        assert_eq!(r.source_kind, "backfill");
    }

    assert_ne!(
        foo_rows[0].stamp, foo_rows[1].stamp,
        "foo's body changed between commits — stamps must differ"
    );
    assert_eq!(
        bar_rows[0].stamp, bar_rows[1].stamp,
        "bar is byte-identical across commits — stamps must match"
    );
    let foo_oids: BTreeSet<_> = foo_rows.iter().map(|r| r.at_oid.clone()).collect();
    assert_eq!(
        foo_oids,
        BTreeSet::from([Some(commit1.clone()), Some(commit2.clone())]),
        "each commit's foo row is pinned to its own at_oid"
    );

    // Idempotency: rerunning `--at commit1` must not add new rows.
    let stats1_again =
        stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), false).unwrap();
    assert_eq!(stats1_again.spans_stamped, 2, "re-attempted, not net-new");
    let rows_after = storage.witnesses_for_file("", &file_abs).unwrap();
    assert_eq!(
        rows_after.len(),
        rows.len(),
        "rerun of commit1 must not duplicate rows"
    );
}

#[test]
fn stamp_spans_at_default_visits_known_repo_roots_and_tags_project_from_code_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn only() {}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "c1"])
        .status()
        .unwrap();
    let commit1 = git_head(repo);

    let repo_root = repo.to_string_lossy().to_string();
    let file_path = file.to_string_lossy().to_string();
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            let mut n = attr_node("n-only", &file_path, "only");
            n.repo_root = Some(repo_root.clone());
            n.span_start = 0;
            n.span_end = 0;
            codegraph_upsert_node(conn, &n).unwrap();
            Ok(())
        })
        .unwrap();

    // No `--repo`: the historical backfill must discover this root the
    // same way `stamp_spans_into` discovers its default footprint — via
    // `code_nodes.repo_root` — and tag rows with that root's project.
    let stats = stamp_spans_historical_into(&storage, &commit1, None, false).unwrap();
    assert_eq!(stats.spans_stamped, 1);
    assert_eq!(stats.at_commits, vec![(repo_root.clone(), commit1.clone())]);

    let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].symbol.as_deref(), Some("only"));
    assert_eq!(rows[0].project, "proj");
    assert_eq!(rows[0].at_oid.as_deref(), Some(commit1.as_str()));
}

#[test]
fn stamp_spans_at_skips_repo_where_rev_does_not_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn only() {}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "c1"])
        .status()
        .unwrap();

    let repo_root = repo.to_string_lossy().to_string();
    let storage = Storage::open_memory().unwrap();

    let bogus_rev = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let stats = stamp_spans_historical_into(&storage, bogus_rev, Some(&repo_root), false).unwrap();
    assert_eq!(
        stats.skipped_rev_unresolved, 1,
        "a SHA absent from the object database must be a soft skip, never a hard failure"
    );
    assert_eq!(stats.spans_stamped, 0);
    assert!(stats.at_commits.is_empty());
}

#[test]
fn stamp_spans_at_dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    if !init_git_repo(repo) {
        return;
    }

    let file = repo.join("lib.rs");
    std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
    git_in(repo).args(["add", "lib.rs"]).status().unwrap();
    git_in(repo)
        .args(["commit", "-q", "-m", "c1"])
        .status()
        .unwrap();
    let commit1 = git_head(repo);

    let repo_root = repo.to_string_lossy().to_string();
    let file_abs = file.to_string_lossy().to_string();
    let storage = Storage::open_memory().unwrap();

    let stats = stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), true).unwrap();
    assert_eq!(stats.spans_stamped, 1, "counting still happens in dry-run");
    assert!(
        storage
            .witnesses_for_file("", &file_abs)
            .unwrap()
            .is_empty(),
        "dry-run must not write witness_ledger"
    );
}
