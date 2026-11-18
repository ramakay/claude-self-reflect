use super::*;

#[test]
fn resolves_same_file_def() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "bar", "a.rs")])
        .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.resolved, 1);
    assert_eq!(stats.unexplained, 0);

    let callees = codegraph::query_callees(&conn, "foo", 10).unwrap();
    assert!(callees.iter().any(|n| n.id == "bar"), "foo -> bar resolved");
}

#[test]
fn ambiguous_stays_unresolved() {
    let conn = mem();
    // Two defs named `bar` in different files; caller in a third file,
    // no import edge, no code_evolution — nothing to disambiguate with.
    upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
    upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
    upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
        .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.resolved, 0, "ambiguous must not be guessed");
    assert_eq!(stats.ambiguous_remaining, 1);

    // Edge remains a placeholder.
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_edges WHERE resolved = 0 AND dst_id = 'name:bar'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1);
}

#[test]
fn project_unique_resolves_cross_file() {
    let conn = mem();
    upsert_node(&conn, &def("bar", "x.rs", "bar")).unwrap();
    upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
        .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.resolved, 1, "unique cross-file name resolves");

    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, "unique_def");
}

#[test]
fn import_bound_picks_the_imported_def_over_ambiguity() {
    let conn = mem();
    // "bar" is ambiguous in code_nodes (x.rs and y.rs both define it),
    // but repo_defs (whole-repo scan) knows the real one lives in y.rs,
    // and z.rs has a textual `import bar` — that combination must win
    // over the plain ambiguity that `unique_def`/`coedit` alone would
    // leave unresolved.
    upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
    upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
    upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
    upsert_node(&conn, &module_node("z_mod", "z.rs")).unwrap();
    upsert_repo_defs(
        &conn,
        "proj",
        "y.rs",
        &[(
            "bar".to_string(),
            "function".to_string(),
            "rust".to_string(),
        )],
    )
    .unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "z.rs",
        &[
            import_edge("z_mod", "bar", "z.rs"),
            call_edge("foo", "bar", "z.rs"),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.total, 2,
        "the imports edge and the calls edge both target bar"
    );
    assert_eq!(stats.resolved, 2);

    let (dst, evidence): (String, String) = conn
        .query_row(
            "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        dst, "bar_y",
        "picks the repo-defs-confirmed import target, not x.rs"
    );
    assert_eq!(evidence, "import_bound");
}

#[test]
fn coedit_binds_on_strict_margin_and_refuses_without_it() {
    let conn = mem();
    // "bar": binds — x.rs shares 2 sessions with z.rs, y.rs shares 0.
    // 2 >= 2 and 2 >= 2*0 -> binds.
    upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
    upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
    upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
        .unwrap();
    for s in ["s1", "s2"] {
        insert_evolution(&conn, s, "z.rs");
        insert_evolution(&conn, s, "x.rs");
    }

    // "baz": refuses — x2.rs shares 3 sessions with z2.rs, y2.rs shares 2.
    // 3 >= 2 but 3 >= 2*2=4 is false -> must not guess.
    upsert_node(&conn, &def("baz_x", "x2.rs", "baz")).unwrap();
    upsert_node(&conn, &def("baz_y", "y2.rs", "baz")).unwrap();
    upsert_node(&conn, &def("foo2", "z2.rs", "foo2")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "z2.rs", &[call_edge("foo2", "baz", "z2.rs")])
        .unwrap();
    for s in ["t1", "t2", "t3"] {
        insert_evolution(&conn, s, "z2.rs");
        insert_evolution(&conn, s, "x2.rs");
    }
    for s in ["t1", "t2"] {
        insert_evolution(&conn, s, "y2.rs");
    }

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();

    let (bar_dst, bar_evidence): (String, String) = conn
        .query_row(
            "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        bar_dst, "bar_x",
        "2-vs-0 margin binds to the co-edited file"
    );
    assert_eq!(bar_evidence, "coedit:2");

    let (baz_resolved, baz_dst): (i64, String) = conn
        .query_row(
            "SELECT resolved, dst_id FROM code_edges WHERE src_id = 'foo2' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(baz_resolved, 0, "3-vs-2 margin (< 2x) must not guess");
    assert_eq!(baz_dst, "name:baz");

    assert_eq!(stats.ambiguous_remaining, 1, "only baz stays ambiguous");
}

#[test]
fn x1_rust_std_import_classified_external() {
    let conn = mem();
    // `use std;` — the bound symbol IS the literal namespace segment.
    upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[import_edge("a_mod", "std", "a.rs")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.external, 1);
    assert_eq!(stats.resolved, 0);

    let (boundary, evidence, resolved): (String, String, i64) = conn
        .query_row(
            "SELECT boundary, evidence, resolved FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(boundary, "external");
    assert_eq!(evidence, "import:std");
    assert_eq!(resolved, 0);
}

#[test]
fn x2_method_call_with_no_def_classified() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    let mut e = call_edge("foo", "push", "a.rs");
    e.callee_kind = "method".into();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.method, 1);
    assert_eq!(stats.resolved, 0);

    let (boundary, evidence, resolved): (String, String, i64) = conn
        .query_row(
            "SELECT boundary, evidence, resolved FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(boundary, "method");
    assert_eq!(evidence, "receiver_call");
    assert_eq!(resolved, 0);
}

// ─── Finding 2: receiver-aware method-call bind gating (WCR truth pass) ───

#[test]
fn method_call_with_unrelated_receiver_does_not_bind_to_same_file_name_collision() {
    // `receiver.save()` — a same-file, same-named `save` def exists,
    // but it's an UNRELATED free function, not evidence about what
    // `receiver` actually is. Before Finding 2, B0 bound purely on
    // bare-name identity, corrupting provenance while flattering the
    // internal-binding metric.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("save_a", "a.rs", "save")).unwrap();
    let mut e = call_edge_via("foo", "save", "a.rs", "receiver");
    e.callee_kind = "method".into();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.resolved, 0,
        "must not bind to the unrelated same-file `save`"
    );
    assert_eq!(stats.method, 1);

    let (boundary, evidence, resolved, dst_id): (String, String, i64, String) = conn
        .query_row(
            "SELECT boundary, evidence, resolved, dst_id FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(boundary, "method");
    assert_eq!(evidence, "receiver_call");
    assert_eq!(resolved, 0);
    assert_eq!(
        dst_id, "name:save",
        "dst_id stays the placeholder — never bound"
    );
}

#[test]
fn method_call_with_self_receiver_binds_via_b0_same_file() {
    // `self.save()` — an instance method calling its own class's
    // method. The self/this/cls receiver IS consistent with B0's
    // same-file assumption, so this must still bind.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("save_a", "a.rs", "save")).unwrap();
    let mut e = call_edge_via("foo", "save", "a.rs", "self");
    e.callee_kind = "method".into();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.resolved, 1,
        "self.save() binds to the same-file `save` def"
    );
    assert_eq!(stats.method, 0);

    let (dst_id, resolved, evidence): (String, i64, String) = conn
        .query_row(
            "SELECT dst_id, resolved, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(dst_id, "save_a");
    assert_eq!(resolved, 1);
    assert_eq!(evidence, "same_file");
}

#[test]
fn method_call_with_non_self_receiver_does_not_bind_via_unique_def_elsewhere() {
    // `obj.save()` — `save` has exactly ONE `code_nodes` def
    // project-wide, but it's in a DIFFERENT file. B2 (`unique_def`) is
    // a cross-file name-only bind tier; `obj` is not self/this/cls, so
    // it must not reach B2 at all, classifying `method` instead.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("save_b", "b.rs", "save")).unwrap();
    let mut e = call_edge_via("foo", "save", "a.rs", "obj");
    e.callee_kind = "method".into();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.resolved, 0,
        "must not bind via B2 unique_def — obj isn't self/this/cls"
    );
    assert_eq!(stats.method, 1);

    let (boundary, evidence, dst_id): (String, String, String) = conn
        .query_row(
            "SELECT boundary, evidence, dst_id FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(boundary, "method");
    assert_eq!(evidence, "receiver_call");
    assert_eq!(dst_id, "name:save");
}

#[test]
fn method_call_with_no_via_evidence_does_not_bind_to_same_file_name_collision() {
    // Simulates a Rust `field_expression` receiver call
    // (`r.map_err(handler)`), which never captures `via:` qualifier
    // evidence at all (see `extraction::codegraph::call_qualifier`).
    // `method_bind_gate` treats "no via: data" the same as "receiver
    // isn't self/this/cls": skip every bind tier, including B0, even
    // though a same-file same-named def exists.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("handler_a", "a.rs", "handler")).unwrap();
    let mut e = call_edge("foo", "handler", "a.rs"); // no `via:` evidence
    e.callee_kind = "method".into();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.resolved, 0,
        "no via: evidence -> NoBind, even with a same-file name collision"
    );
    assert_eq!(stats.method, 1);
}

#[test]
fn unexplained_residual_is_counted() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    // No def anywhere, no import edge, plain (non-method) callee_kind —
    // nothing binds or classifies it.
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[call_edge("foo", "ghost_symbol_xyz", "a.rs")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.resolved, 0);
    assert_eq!(stats.external, 0);
    assert_eq!(stats.method, 0);
    assert_eq!(stats.unexplained, 1);
    assert_eq!(stats.ambiguous_remaining, 0);
}

#[test]
fn resolve_is_deterministic_across_repeated_runs() {
    let conn = mem();
    upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
    upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
    upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
        .unwrap();

    let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(first, second, "repeated passes over unchanged data agree");
}

// ─── X0 builtin/prelude/global tier (WCR Phase 5, TASK 3) ───

#[test]
fn x0_builtin_classifies_per_language() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "Ok", "a.rs")])
        .unwrap();
    upsert_node(&conn, &def("run", "b.ts", "run")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "b.ts", &[call_edge("run", "fetch", "b.ts")])
        .unwrap();
    upsert_node(&conn, &def("main_py", "c.py", "main_py")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "c.py",
        &[call_edge("main_py", "print", "c.py")],
    )
    .unwrap();
    upsert_node(&conn, &def("main_go", "d.go", "main_go")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "d.go",
        &[call_edge("main_go", "make", "d.go")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 4);
    assert_eq!(stats.resolved, 0);
    assert_eq!(
        stats.external, 4,
        "Ok/fetch/print/make all classify external via X0"
    );

    for (src, dst, expected_evidence) in [
        ("foo", "name:Ok", "builtin:rust"),
        ("run", "name:fetch", "builtin:js"),
        ("main_py", "name:print", "builtin:python"),
        ("main_go", "name:make", "builtin:go"),
    ] {
        let (boundary, evidence): (String, String) = conn
            .query_row(
                "SELECT boundary, evidence FROM code_edges WHERE src_id = ?1 AND dst_id = ?2",
                params![src, dst],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(boundary, "external", "{src}/{dst}");
        assert_eq!(evidence, expected_evidence, "{src}/{dst}");
    }
}

#[test]
fn x0_builtin_does_not_fire_when_a_repo_def_exists() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "Ok", "a.rs")])
        .unwrap();
    // A whole-repo scan found a (shadowing, user-defined) `Ok` def — X0
    // must defer to the repo_scan bind tier, never fire alongside it.
    upsert_repo_defs(
        &conn,
        "proj",
        "custom_ok.rs",
        &[("Ok".to_string(), "function".to_string(), "rust".to_string())],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.resolved, 1,
        "single repo_defs candidate binds via repo_scan, not X0"
    );
    assert_eq!(stats.external, 0, "X0 must not fire when a repo def exists");

    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:Ok'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(boundary, "");
    assert!(evidence.starts_with("repo_scan:"), "{evidence}");
}

// ─── Module-aware X1 tier (WCR Phase 5, TASK 4) ───

#[test]
fn x1_module_aware_relative_import_is_never_external() {
    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[import_edge_from("a_mod", "helper", "a.ts", "./util")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 0,
        "relative module specifier must never classify external"
    );
    assert_eq!(stats.unexplained, 1);
}

#[test]
fn x1_module_aware_bare_node_prefixed_module_matches_builtin() {
    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[import_edge_from("a_mod", "fs", "a.ts", "node:fs")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.external, 1);
    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(boundary, "external");
    assert_eq!(evidence, "import:node:fs");
}

#[test]
fn x1_module_aware_calls_edge_uses_sibling_import_module() {
    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
    upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[
            import_edge_from("a_mod", "fs", "a.ts", "node:fs"),
            call_edge("foo", "fs", "a.ts"),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 2);
    assert_eq!(
        stats.external, 2,
        "both the imports edge and the sibling calls edge classify external"
    );

    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        boundary, "external",
        "calls edge inherits the sibling imports edge's module evidence"
    );
    assert_eq!(evidence, "import:node:fs");
}

#[test]
fn x1_module_present_but_unmatched_suppresses_degraded_name_fallback() {
    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
    // Bound symbol name "std" WOULD match the degraded (name-only) path,
    // but the module evidence does not match anything — once module data
    // is present it fully replaces the degraded match, it is not tried
    // as a fallback on a module-classification miss.
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[import_edge_from(
            "a_mod",
            "std",
            "a.rs",
            "zzz_definitely_not_a_real_module_zzz",
        )],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 0,
        "module data present but unmatched must not fall back to degraded name match"
    );
    assert_eq!(stats.unexplained, 1);
}

#[test]
fn resolve_is_deterministic_with_x0_and_module_aware_x1_tiers() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[call_edge("foo", "Ok", "a.rs")])
        .unwrap();
    upsert_node(&conn, &module_node("b_mod", "b.ts")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "b.ts",
        &[import_edge_from("b_mod", "fs", "b.ts", "node:fs")],
    )
    .unwrap();

    let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        first, second,
        "X0 + module-aware X1 tiers stay deterministic across repeated passes"
    );
    assert_eq!(first.external, 2);
}

// ─── X1b qualifier-aware tier (WCR Phase 6, TASK B) ───

#[test]
fn qualifier_tier_rust_path_call_classifies_external_via_stdlib_namespace() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[call_edge_via("foo", "now", "a.rs", "std::time::Instant")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 1,
        "std::time::Instant::now() classifies external via qualifier root"
    );
    assert_eq!(stats.resolved, 0);

    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:now'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(boundary, "external");
    assert_eq!(evidence, "import:std::time::Instant");
}

#[test]
fn qualifier_tier_rust_refuses_when_root_matches_a_repo_module_name() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    // The repo happens to have its own module literally named `std.rs` —
    // the guard must refuse to classify `std::helper()` external just
    // because "std" also matches the Rust builtin namespace.
    upsert_node(&conn, &def("marker", "std.rs", "marker")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[call_edge_via("foo", "helper", "a.rs", "std")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 0,
        "repo-module-name collision must suppress qualifier classification"
    );
    assert_eq!(stats.unexplained, 1);
}

#[test]
fn qualifier_tier_python_module_call_classifies_external_via_stdlib() {
    let conn = mem();
    upsert_node(&conn, &def("handler", "a.py", "handler")).unwrap();
    // Realistic: Python's `attribute` AST node makes `json.dumps()`
    // callee_kind = "method" same as `self.run()` — the qualifier tier
    // must take priority over X2 so this classifies `external`, not the
    // vaguer `method`/receiver_call bucket.
    let mut e = call_edge_via("handler", "dumps", "a.py", "json");
    e.callee_kind = "method".to_string();
    codegraph::replace_file_edges(&conn, "proj", "a.py", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 1,
        "json.dumps() classifies external via qualifier, ahead of X2 method"
    );
    assert_eq!(stats.method, 0);

    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'handler' AND dst_id = 'name:dumps'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(boundary, "external");
    assert_eq!(evidence, "import:json");
}

#[test]
fn qualifier_tier_python_self_receiver_never_classifies_external() {
    let conn = mem();
    upsert_node(&conn, &def("handler", "a.py", "handler")).unwrap();
    // Realistic callee_kind: Python's `attribute` node can't
    // syntactically distinguish `self.run()` from `json.dumps()`, both
    // are "method". `self` must fall through to the generic X2 tier
    // instead of being misclassified `external`.
    let mut e = call_edge_via("handler", "run", "a.py", "self");
    e.callee_kind = "method".to_string();
    codegraph::replace_file_edges(&conn, "proj", "a.py", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 0,
        "self.run() must never classify external — self is a receiver, not a module"
    );
    assert_eq!(
        stats.method, 1,
        "falls through to the generic X2 method tier instead"
    );
    assert_eq!(stats.unexplained, 0);
}

#[test]
fn qualifier_tier_python_refuses_when_repo_owns_the_module_file() {
    let conn = mem();
    upsert_node(&conn, &def("handler", "a.py", "handler")).unwrap();
    upsert_repo_defs(
        &conn,
        "proj",
        "myutil.py",
        &[(
            "real_fn".to_string(),
            "function".to_string(),
            "python".to_string(),
        )],
    )
    .unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.py",
        &[call_edge_via("handler", "helper", "a.py", "myutil")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 0,
        "myutil.py exists in this repo — must not classify external"
    );
    assert_eq!(stats.unexplained, 1);
}

#[test]
fn qualifier_tier_ignores_non_calls_edges() {
    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", "a.py")).unwrap();
    // `via:` evidence only exists on `calls` edges (TASK A); an imports
    // edge that somehow carried one (shouldn't happen) must not trip
    // X1b. Bound symbol name is deliberately NOT a stdlib name itself
    // (unlike the qualifier), isolating this from the pre-existing
    // degraded X1 name-only match — this test is purely about
    // `qualifier_tier`'s own `p.kind != "calls"` guard.
    let mut e = import_edge_from("a_mod", "totally_unknown_symbol_xyz", "a.py", "os");
    e.evidence = "via:os".to_string();
    codegraph::replace_file_edges(&conn, "proj", "a.py", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.external, 0);
    assert_eq!(stats.unexplained, 1);
}

// ─── Stale-file tier (WCR Phase 6, TASK C) ───

#[test]
fn stale_tier_classifies_when_src_file_is_missing_on_disk() {
    const GONE: &str = "/definitely/does/not/exist/on/disk/a.rs";
    let conn = mem();
    upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        GONE,
        &[call_edge("foo", "ghost_symbol_xyz", GONE)],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|f: &str| std::path::Path::new(f).is_file()).unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(
        stats.stale, 1,
        "missing file must classify stale, not unexplained"
    );
    assert_eq!(stats.unexplained, 0);
    assert_eq!(stats.resolved, 0);
    assert_eq!(stats.external, 0);
    assert_eq!(stats.method, 0);
    assert_eq!(
        stats.closure_rate, 1.0,
        "stale counts toward closure like external/method"
    );

    let (boundary, evidence, resolved): (String, String, i64) = conn
        .query_row(
            "SELECT boundary, evidence, resolved FROM code_edges
                 WHERE src_id = 'foo' AND dst_id = 'name:ghost_symbol_xyz'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(boundary, "stale");
    assert_eq!(evidence, "file_missing");
    assert_eq!(
        resolved, 0,
        "stale never binds — dst_id stays a placeholder"
    );
}

#[test]
fn stale_tier_does_not_fire_when_file_exists_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.rs");
    std::fs::write(&file_path, "fn foo() {}\n").unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &def("foo", &file_str, "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &file_str,
        &[call_edge("foo", "ghost_symbol_xyz", &file_str)],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|f: &str| std::path::Path::new(f).is_file()).unwrap();
    assert_eq!(stats.stale, 0, "existing file must not classify stale");
    assert_eq!(stats.unexplained, 1);
}

#[test]
fn stale_tier_reclassifies_a_would_be_ambiguous_edge_when_file_missing() {
    const GONE: &str = "/definitely/does/not/exist/on/disk/z.rs";
    let conn = mem();
    // Two defs named `bar` in different (existing-path) files; caller's
    // own file is missing — the file-existence check runs before the
    // ambiguity is even considered, so it must win.
    upsert_node(&conn, &def("bar_x", "x.rs", "bar")).unwrap();
    upsert_node(&conn, &def("bar_y", "y.rs", "bar")).unwrap();
    upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", GONE, &[call_edge("foo", "bar", GONE)]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|f: &str| std::path::Path::new(f).is_file()).unwrap();
    assert_eq!(
        stats.stale, 1,
        "would-be-ambiguous edge in a missing file classifies stale instead"
    );
    assert_eq!(stats.ambiguous_remaining, 0);
}

#[test]
fn stale_tier_never_preempts_a_successful_bind() {
    // B0 same-file bind must still fire even when the file is reported
    // missing — staleness only applies to the leftover unexplained
    // remainder, never to edges that already bound to a real def.
    const GONE: &str = "/definitely/does/not/exist/on/disk/a.rs";
    let conn = mem();
    upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
    upsert_node(&conn, &def("bar", GONE, "bar")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", GONE, &[call_edge("foo", "bar", GONE)]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| false).unwrap();
    assert_eq!(stats.resolved, 1, "same-file bind unaffected by staleness");
    assert_eq!(stats.stale, 0);
}

#[test]
fn resolve_is_deterministic_with_stale_and_qualifier_tiers() {
    const GONE: &str = "/definitely/does/not/exist/on/disk/a.rs";
    let conn = mem();
    upsert_node(&conn, &def("foo", GONE, "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        GONE,
        &[
            call_edge("foo", "ghost_symbol_xyz", GONE),
            call_edge_via("foo", "now", GONE, "std::time::Instant"),
        ],
    )
    .unwrap();

    let checker: &dyn Fn(&str) -> bool = &|f: &str| std::path::Path::new(f).is_file();
    let first = resolve_edges(&conn, "proj", checker).unwrap();
    let second = resolve_edges(&conn, "proj", checker).unwrap();
    assert_eq!(first, second, "stale + qualifier tiers stay deterministic");
    assert_eq!(first.stale, 1);
    assert_eq!(first.external, 1);
}

// ─── Qualifier -> import two-hop chain (WCR Phase 7, TASK A) ───

#[test]
fn qualifier_import_tier_rust_binds_via_sibling_import_module() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[
            import_edge_from("a_mod", "Instant", "a.rs", "std::time"),
            call_edge_via("foo", "now", "a.rs", "Instant"),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 2,
        "both the Instant import edge and the now() call classify external: {stats:?}"
    );

    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:now'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        evidence, "import:std::time::Instant",
        "reconstructs the fully-qualified path the `use` statement elided"
    );
}

#[test]
fn qualifier_import_tier_rust_bare_use_module_binding() {
    // `use std::env;` then `env::temp_dir()` — the qualifier root `env`
    // is not itself a builtin/dep name, but the sibling import edge for
    // `env` carries `from:std` evidence. (Deliberately NOT `use
    // std::fs;` + `fs::...` — `fs` coincidentally collides with
    // `manifest::NODE_BUILTINS`, so `qualifier_tier`'s own direct check
    // already classifies it without ever reaching this two-hop tier;
    // `env` has no such cross-language collision.)
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[
            import_edge_from("a_mod", "env", "a.rs", "std"),
            call_edge_via("foo", "temp_dir", "a.rs", "env"),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.external, 2, "{stats:?}");

    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:temp_dir'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, "import:std::env");
}

#[test]
fn qualifier_import_tier_requires_a_same_file_import_with_module_evidence() {
    // No import edge for `Instant` in this file at all — must not guess.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[call_edge_via("foo", "now", "a.rs", "Instant")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.external, 0, "{stats:?}");
    assert_eq!(stats.unexplained, 1);
}

#[test]
fn qualifier_import_tier_does_not_fire_for_javascript() {
    // Deliberately out of scope (see qualifier_import_tier's doc
    // comment on replay-safety) — a JS/TS `via:` qualifier backed by a
    // same-file import must not be two-hop-joined.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
    upsert_node(&conn, &module_node("a_mod", "a.ts")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[
            import_edge_from("a_mod", "obj", "a.ts", "some-real-package"),
            call_edge_via("foo", "helper_call", "a.ts", "obj"),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 0,
        "JS/TS is out of scope for the two-hop chain: {stats:?}"
    );
}

#[test]
fn resolve_is_deterministic_with_qualifier_import_tier() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &module_node("a_mod", "a.rs")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[
            import_edge_from("a_mod", "env", "a.rs", "std"),
            call_edge_via("foo", "temp_dir", "a.rs", "env"),
        ],
    )
    .unwrap();

    let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        first, second,
        "qualifier-import two-hop tier stays deterministic across repeated passes"
    );
    assert_eq!(first.external, 2);

    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM code_edges WHERE src_id = 'foo' AND dst_id = 'name:temp_dir'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        evidence, "import:std::env",
        "evidence must stay byte-identical across the repeated pass"
    );
}

// ─── Installed-package witness (WCR Phase 7, TASK B) ───

#[test]
fn installed_package_witness_classifies_external_via_node_modules_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    // `@expo/vector-icons` is deliberately NOT declared — only `expo`
    // is, matching the real-world "Feather -> @expo/vector-icons"
    // transitive/bundled dependency case.
    std::fs::write(
        root.join("package.json"),
        r#"{"dependencies": {"expo": "1.0.0"}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("node_modules/@expo/vector-icons")).unwrap();
    let src_file = src_dir.join("Icon.tsx");
    std::fs::write(&src_file, "// icon\n").unwrap();
    let src_file_str = src_file.to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &src_file_str,
        &[import_edge_from(
            "a_mod",
            "Feather",
            &src_file_str,
            "@expo/vector-icons",
        )],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.external, 1,
        "transitive/bundled dep must classify external via node_modules witness: {stats:?}"
    );

    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(boundary, "external");
    assert_eq!(evidence, "installed:@expo/vector-icons");
}

#[test]
fn installed_package_witness_does_not_fire_when_package_is_absent_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(root.join("package.json"), "{}").unwrap();
    let src_file = src_dir.join("a.ts");
    std::fs::write(&src_file, "// a\n").unwrap();
    let src_file_str = src_file.to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &src_file_str,
        &[import_edge_from(
            "a_mod",
            "Thing",
            &src_file_str,
            "totally-not-installed-anywhere",
        )],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.external, 0, "{stats:?}");
    assert_eq!(stats.unexplained, 1);
}

// ─── Internal-module-file witness fallback (WCR Phase 7, TASK E) ───

#[test]
fn internal_module_tier_classifies_when_relative_target_file_exists_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("a.ts");
    std::fs::write(&src_file, "// a\n").unwrap();
    // No code_nodes/repo_defs def recorded for `util` — only the FILE
    // itself is real.
    std::fs::write(src_dir.join("util.ts"), "export const x = 1;\n").unwrap();
    let src_file_str = src_file.to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &src_file_str,
        &[import_edge_from("a_mod", "helper", &src_file_str, "./util")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.internal_module, 1, "{stats:?}");
    assert_eq!(stats.unexplained, 0);
    assert_eq!(
        stats.external, 0,
        "relative module must never classify external"
    );

    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(boundary, "internal_module");
    assert_eq!(evidence, "module_file:util.ts");
}

#[test]
fn internal_module_tier_resolves_directory_index_file() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(src_dir.join("widgets")).unwrap();
    let src_file = src_dir.join("a.ts");
    std::fs::write(&src_file, "// a\n").unwrap();
    std::fs::write(src_dir.join("widgets/index.ts"), "export const w = 1;\n").unwrap();
    let src_file_str = src_file.to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &src_file_str,
        &[import_edge_from(
            "a_mod",
            "Widget",
            &src_file_str,
            "./widgets",
        )],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.internal_module, 1, "{stats:?}");
    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, "module_file:index.ts");
}

#[test]
fn internal_module_tier_does_not_fire_when_target_file_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("a.ts");
    std::fs::write(&src_file, "// a\n").unwrap();
    let src_file_str = src_file.to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &src_file_str,
        &[import_edge_from("a_mod", "ghost", &src_file_str, "./ghost")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.internal_module, 0, "{stats:?}");
    assert_eq!(stats.unexplained, 1, "no file backs ./ghost anywhere");
}

#[test]
fn internal_module_tier_alias_resolves_against_project_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    let src_dir = root.join("src/screens");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(root.join("package.json"), "{}").unwrap();
    // `@/components/Button` resolves project-root-relative (see
    // manifest::is_relative_module's doc comment).
    std::fs::create_dir_all(root.join("components")).unwrap();
    std::fs::write(
        root.join("components/Button.tsx"),
        "export const Button = 1;\n",
    )
    .unwrap();
    let src_file = src_dir.join("Home.tsx");
    std::fs::write(&src_file, "// home\n").unwrap();
    let src_file_str = src_file.to_string_lossy().to_string();

    let conn = mem();
    // `repo_scan::project_roots` derives roots from `code_nodes.file` —
    // seed one pointing at this project so the alias resolution has a
    // root candidate to try.
    upsert_node(
        &conn,
        &NodeRow {
            id: "root_anchor".into(),
            project: "proj".into(),
            file: src_file_str.clone(),
            kind: "function".into(),
            name: "anchor".into(),
            ..NodeRow::default()
        },
    )
    .unwrap();
    upsert_node(&conn, &module_node("a_mod", &src_file_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &src_file_str,
        &[import_edge_from(
            "a_mod",
            "Button",
            &src_file_str,
            "@/components/Button",
        )],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.internal_module, 1, "{stats:?}");
    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, "module_file:Button.tsx");
}

#[test]
fn internal_module_tier_ignores_calls_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("a.ts");
    std::fs::write(&src_file, "// a\n").unwrap();
    std::fs::write(src_dir.join("util.ts"), "export const x = 1;\n").unwrap();
    let src_file_str = src_file.to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &def("foo", &src_file_str, "foo")).unwrap();
    let mut e = call_edge("foo", "helper", &src_file_str);
    // Defensive only: `via:`/`from:`-shaped evidence should never
    // appear on a real calls edge here, but the guard must hold
    // regardless of what the evidence column happens to contain.
    e.evidence = "from:./util".to_string();
    codegraph::replace_file_edges(&conn, "proj", &src_file_str, &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.internal_module, 0,
        "internal_module_tier is imports-only: {stats:?}"
    );
}
