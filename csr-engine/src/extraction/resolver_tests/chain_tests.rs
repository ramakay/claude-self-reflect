use super::*;

#[test]
fn resolve_is_deterministic_with_internal_module_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src_file = src_dir.join("a.ts");
    std::fs::write(&src_file, "// a\n").unwrap();
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

    let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        first, second,
        "internal_module tier stays deterministic across repeated passes"
    );
    assert_eq!(first.internal_module, 1);

    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(evidence, "module_file:util.ts");
}

// ─── B1 module-path precise bind (WCR Phase 8, TASK A) ───

#[test]
fn module_bind_tier_precise_import_disambiguates_many_candidates() {
    // The classic per-component `styles` const collision: TWO files each
    // define a same-named `styles` symbol, but ComponentA's own
    // `import { styles } from './ComponentA.styles'` names exactly one
    // of them. Without B1 this would fall to B3 coedit (no margin, no
    // code_evolution rows here) and land in `ambiguous_remaining`.
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src/components");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("ComponentA.styles.ts"), "// a styles\n").unwrap();
    std::fs::write(src_dir.join("ComponentB.styles.ts"), "// b styles\n").unwrap();
    let a_styles_str = src_dir
        .join("ComponentA.styles.ts")
        .to_string_lossy()
        .to_string();
    let b_styles_str = src_dir
        .join("ComponentB.styles.ts")
        .to_string_lossy()
        .to_string();
    let comp_a_str = src_dir.join("ComponentA.tsx").to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &def("styles_a", &a_styles_str, "styles")).unwrap();
    upsert_node(&conn, &def("styles_b", &b_styles_str, "styles")).unwrap();
    upsert_node(&conn, &module_node("a_mod", &comp_a_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &comp_a_str,
        &[import_edge_from(
            "a_mod",
            "styles",
            &comp_a_str,
            "./ComponentA.styles",
        )],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.resolved, 1,
        "precise module-path bind despite two `styles` candidates: {stats:?}"
    );
    assert_eq!(
        stats.ambiguous_remaining, 0,
        "an import naming one specific file is not ambiguous"
    );

    let (dst, evidence): (String, String) = conn
        .query_row(
            "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'a_mod' AND kind = 'imports'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        dst, "styles_a",
        "binds to the file the import names, not styles_b"
    );
    assert_eq!(evidence, "module_bind:ComponentA.styles.ts");
}

#[test]
fn a_module_path_with_dotdot_binds_the_same_on_a_first_resolve_as_on_a_warm_one() {
    // A package tree with no `.git`: the plain path lookup spells the first
    // path it sees in a directory as given (`src/../shared.ts`) and every
    // later one resolved (`shared.ts`). An import and a call that both name
    // `../shared` must bind alike, whichever of them is looked up first.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("shared.ts"), "// shared\n").unwrap();
    std::fs::write(root.join("other.ts"), "// other\n").unwrap();
    let shared = root.join("shared.ts").to_string_lossy().to_string();
    let other = root.join("other.ts").to_string_lossy().to_string();
    let main = root.join("src/main.ts").to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &def("helper_shared", &shared, "helper")).unwrap();
    upsert_node(&conn, &def("helper_other", &other, "helper")).unwrap();
    upsert_node(&conn, &def("run", &main, "run")).unwrap();
    upsert_node(&conn, &module_node("main_mod", &main)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &main,
        &[
            import_edge_from("main_mod", "helper", &main, "../shared"),
            call_edge("run", "helper", &main),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.resolved, 2, "both edges bind to ../shared: {stats:?}");
    let dsts: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT dst_id FROM code_edges ORDER BY src_id, kind")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert_eq!(dsts, vec!["helper_shared", "helper_shared"]);
}

#[test]
fn module_bind_tier_works_for_a_calls_edge_via_sibling_import() {
    // Same shape, but the pending edge is a `calls` edge for `styles`
    // (not the `imports` edge itself) — module evidence must come from
    // the SAME-FILE SIBLING `imports` edge (`module_for_pending`'s
    // calls-edge branch).
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src/components");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("ComponentA.styles.ts"), "// a styles\n").unwrap();
    std::fs::write(src_dir.join("ComponentB.styles.ts"), "// b styles\n").unwrap();
    let a_styles_str = src_dir
        .join("ComponentA.styles.ts")
        .to_string_lossy()
        .to_string();
    let b_styles_str = src_dir
        .join("ComponentB.styles.ts")
        .to_string_lossy()
        .to_string();
    let comp_a_str = src_dir.join("ComponentA.tsx").to_string_lossy().to_string();

    let conn = mem();
    upsert_node(&conn, &def("styles_a", &a_styles_str, "styles")).unwrap();
    upsert_node(&conn, &def("styles_b", &b_styles_str, "styles")).unwrap();
    upsert_node(&conn, &def("render", &comp_a_str, "render")).unwrap();
    upsert_node(&conn, &module_node("a_mod", &comp_a_str)).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        &comp_a_str,
        &[
            import_edge_from("a_mod", "styles", &comp_a_str, "./ComponentA.styles"),
            call_edge("render", "styles", &comp_a_str),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.resolved, 2, "both edges bind: {stats:?}");
    assert_eq!(stats.ambiguous_remaining, 0);

    let dst: String = conn
        .query_row(
            "SELECT dst_id FROM code_edges WHERE src_id = 'render' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        dst, "styles_a",
        "calls edge inherits the sibling import's precise binding"
    );
}

#[test]
fn module_bind_tier_falls_through_when_module_resolves_to_no_file() {
    // Relative import evidence is present, but the module doesn't
    // resolve to any real file on disk — module_bind_tier must no-op,
    // falling through to the old ambiguity-resolution behavior (no
    // repo_defs, no code_evolution margin -> stays ambiguous).
    let conn = mem();
    upsert_node(&conn, &def("styles_x", "x.ts", "styles")).unwrap();
    upsert_node(&conn, &def("styles_y", "y.ts", "styles")).unwrap();
    upsert_node(&conn, &module_node("z_mod", "z.ts")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "z.ts",
        &[import_edge_from(
            "z_mod",
            "styles",
            "z.ts",
            "./this_module_does_not_exist_anywhere_zzz",
        )],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.resolved, 0,
        "module doesn't resolve to a real file — must never guess: {stats:?}"
    );
    assert_eq!(stats.ambiguous_remaining, 1);
}

#[test]
fn module_bind_tier_no_import_evidence_multi_candidate_still_ambiguous() {
    // No `from:<module>` data at all on the imports edge (old-style,
    // pre-Phase-5 evidence shape) — module_for_pending must return
    // `None`, module_bind_tier must no-op, and multiple code_nodes
    // candidates stay genuinely ambiguous exactly as before B1 existed.
    let conn = mem();
    upsert_node(&conn, &def("styles_x", "x.ts", "styles")).unwrap();
    upsert_node(&conn, &def("styles_y", "y.ts", "styles")).unwrap();
    upsert_node(&conn, &module_node("z_mod", "z.ts")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "z.ts",
        &[import_edge("z_mod", "styles", "z.ts")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.resolved, 0, "{stats:?}");
    assert_eq!(stats.ambiguous_remaining, 1);
}

#[test]
fn resolve_is_deterministic_with_module_bind_tier() {
    // module_bind is a BIND tier: once it binds, `resolved = 1` and the
    // edge drops out of a SECOND pass's pending set entirely (unlike the
    // classify tiers' "resolve_is_deterministic_with_X_tier" pattern
    // above, which re-run on the same still-pending edge). The
    // meaningful determinism property for a bind tier is "same input on
    // fresh state always makes the same choice" — checked here across
    // two independently seeded connections, matching how B0/B1b/B2/B3
    // (which have no dedicated determinism tests of their own either,
    // for the same reason) are implicitly covered by
    // `resolve_is_deterministic_across_repeated_runs`'s AMBIGUOUS
    // (never-binding) scenario instead.
    fn seed_and_resolve() -> ResolveStats {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src/components");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("ComponentA.styles.ts"), "// a styles\n").unwrap();
        std::fs::write(src_dir.join("ComponentB.styles.ts"), "// b styles\n").unwrap();
        let a_styles_str = src_dir
            .join("ComponentA.styles.ts")
            .to_string_lossy()
            .to_string();
        let b_styles_str = src_dir
            .join("ComponentB.styles.ts")
            .to_string_lossy()
            .to_string();
        let comp_a_str = src_dir.join("ComponentA.tsx").to_string_lossy().to_string();

        let conn = mem();
        upsert_node(&conn, &def("styles_a", &a_styles_str, "styles")).unwrap();
        upsert_node(&conn, &def("styles_b", &b_styles_str, "styles")).unwrap();
        upsert_node(&conn, &module_node("a_mod", &comp_a_str)).unwrap();
        codegraph::replace_file_edges(
            &conn,
            "proj",
            &comp_a_str,
            &[import_edge_from(
                "a_mod",
                "styles",
                &comp_a_str,
                "./ComponentA.styles",
            )],
        )
        .unwrap();

        resolve_edges(&conn, "proj", &|_: &str| true).unwrap()
    }

    let first = seed_and_resolve();
    let second = seed_and_resolve();
    assert_eq!(
        first, second,
        "module_bind tier makes the same choice on fresh, identically seeded state"
    );
    assert_eq!(first.resolved, 1);
    assert_eq!(first.ambiguous_remaining, 0);
}

// ─── drifted tier (WCR Phase 8, TASK B) ───

#[test]
fn drifted_edge_is_skipped_by_all_tiers_and_counted() {
    let conn = mem();
    // A same-file def named `bar` exists — this WOULD bind via B0 if
    // reached. A pre-set `boundary = 'drifted'` must prevent that: every
    // tier, including B0, is skipped for a drifted edge.
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
    let mut e = call_edge("foo", "bar", "a.rs");
    e.boundary = "drifted".to_string();
    e.evidence = "not_in_current_source".to_string();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 1);
    assert_eq!(stats.drifted, 1);
    assert_eq!(
        stats.resolved, 0,
        "drifted edge must never bind, even though bar is same-file"
    );
    assert_eq!(stats.unexplained, 0);
    assert_eq!(
        stats.closure_rate, 1.0,
        "drifted counts toward closure like stale/internal_module"
    );
    assert_eq!(
        stats.internal_binding_rate, 1.0,
        "drifted excluded from the binding denominator (0/0 -> 1.0)"
    );

    let (boundary, evidence, resolved, dst_id): (String, String, i64, String) = conn
        .query_row(
            "SELECT boundary, evidence, resolved, dst_id FROM code_edges
                 WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        boundary, "drifted",
        "resolve_edges must not touch a pre-set drifted boundary"
    );
    assert_eq!(evidence, "not_in_current_source");
    assert_eq!(resolved, 0);
    assert_eq!(
        dst_id, "name:bar",
        "drifted never binds — dst_id stays the placeholder"
    );
}

#[test]
fn drifted_edge_excluded_from_internal_binding_denominator_with_other_edges() {
    // Mix a genuinely bound edge with a drifted one: binding rate must
    // be computed over the non-drifted denominator only (1/1 = 100%,
    // not 1/2).
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
    upsert_node(&conn, &def("baz", "a.rs", "baz")).unwrap();
    let mut drifted_edge = call_edge("foo", "ghost", "a.rs");
    drifted_edge.boundary = "drifted".to_string();
    drifted_edge.evidence = "not_in_current_source".to_string();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[call_edge("foo", "bar", "a.rs"), drifted_edge],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.resolved, 1, "the real edge binds via B0");
    assert_eq!(stats.drifted, 1);
    assert_eq!(
        stats.internal_binding_rate, 1.0,
        "denominator excludes the drifted edge: 1 bound / 1 eligible"
    );
}

#[test]
fn resolve_is_deterministic_with_drifted_tier() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("bar", "a.rs", "bar")).unwrap();
    let mut e = call_edge("foo", "bar", "a.rs");
    e.boundary = "drifted".to_string();
    e.evidence = "not_in_current_source".to_string();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();

    let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        first, second,
        "drifted tier stays deterministic across repeated passes"
    );
    assert_eq!(first.drifted, 1);

    let (boundary, evidence): (String, String) = conn
        .query_row(
            "SELECT boundary, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        boundary, "drifted",
        "byte-identical across the repeated pass"
    );
    assert_eq!(evidence, "not_in_current_source");
}

#[test]
fn x4_local_binding_classifies_when_zero_defs_anywhere_and_witnessed() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
    // No def anywhere for "playTrack" — code_nodes has no def, repo_defs
    // is empty.
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[call_edge("foo", "playTrack", "a.ts")],
    )
    .unwrap();
    // Scoped to "foo" — the SAME function the call site is inside.
    insert_local_binding(&conn, "proj", "a.ts", "foo", "playTrack");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.local, 1);
    assert_eq!(stats.resolved, 0, "local classifies, never binds");
    assert_eq!(stats.unexplained, 0);
    assert_eq!(
        stats.closure_rate, 1.0,
        "local counts toward the closure numerator"
    );

    let (boundary, evidence, resolved, dst_id): (String, String, i64, String) = conn
        .query_row(
            "SELECT boundary, evidence, resolved, dst_id FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(boundary, "local");
    assert_eq!(evidence, "local_scope:playTrack");
    assert_eq!(resolved, 0);
    assert_eq!(dst_id, "name:playTrack", "dst_id stays the placeholder");
}

#[test]
fn x4_local_binding_excluded_from_internal_binding_denominator() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
    upsert_node(&conn, &def("bar", "a.ts", "bar")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[
            // Binds normally via B0 same_file.
            call_edge("foo", "bar", "a.ts"),
            // No def anywhere, but witnessed local.
            call_edge("foo", "playTrack", "a.ts"),
        ],
    )
    .unwrap();
    insert_local_binding(&conn, "proj", "a.ts", "foo", "playTrack");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.resolved, 1, "bar binds via B0");
    assert_eq!(stats.local, 1);
    assert_eq!(
        stats.internal_binding_rate, 1.0,
        "denominator excludes the local edge: 1 bound / 1 eligible"
    );
}

#[test]
fn x4_never_fires_when_a_real_def_exists_elsewhere_for_the_name() {
    // Adversarial self-check: a name that is BOTH a witnessed local
    // SOMEWHERE and has a real def elsewhere in the project must NOT
    // classify local — def_file_count > 0 blocks it (condition (a)).
    // Uses the same receiver-gated (NoBind) shape as
    // `method_call_with_non_self_receiver_does_not_bind_via_unique_def_elsewhere`:
    // a method call off a non-self receiver never reaches a bind tier
    // even though `save` has exactly one code_nodes def elsewhere, so
    // the ONLY thing standing between this edge and a wrong `local`
    // classification is the explicit zero-defs-anywhere check.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    upsert_node(&conn, &def("save_b", "b.rs", "save")).unwrap();
    let mut e = call_edge_via("foo", "save", "a.rs", "obj");
    e.callee_kind = "method".into();
    codegraph::replace_file_edges(&conn, "proj", "a.rs", &[e]).unwrap();
    // "save" also happens to be a local binding name in a.rs itself —
    // must still not classify local, because a real def of "save"
    // exists (in b.rs).
    insert_local_binding(&conn, "proj", "a.rs", "foo", "save");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.local, 0, "a real def elsewhere must block X4");
    assert_eq!(stats.method, 1, "falls through to X2 method instead");

    let boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(boundary, "method");
}

#[test]
fn x4_never_preempts_a_successful_bind_tier() {
    // Adversarial self-check: a local_bindings witness for the SAME
    // (project, file, name) as an edge that a bind tier can already
    // resolve must never be reached at all — bind tiers run to
    // completion (and `continue`) long before `classify_only` (and
    // therefore X4) is ever called.
    let conn = mem();
    upsert_node(&conn, &def("bar", "x.rs", "bar")).unwrap();
    upsert_node(&conn, &def("foo", "z.rs", "foo")).unwrap();
    codegraph::replace_file_edges(&conn, "proj", "z.rs", &[call_edge("foo", "bar", "z.rs")])
        .unwrap();
    insert_local_binding(&conn, "proj", "z.rs", "foo", "bar");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.local, 0);
    assert_eq!(stats.resolved, 1, "B2 unique_def still binds it");

    let (dst, evidence): (String, String) = conn
        .query_row(
            "SELECT dst_id, evidence FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(dst, "bar");
    assert_eq!(evidence, "unique_def");
}

#[test]
fn x4_silent_when_local_bindings_table_is_empty() {
    // Empty local_bindings table (every resolve pass outside the WCR
    // live gate, since only `backfill_wcr_witnesses` ever populates it)
    // -> zero local classifications, tier silent not erroring; the edge
    // falls through to the pre-existing unexplained/ambiguous behavior
    // exactly as before this tier was added.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.rs", "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.rs",
        &[call_edge("foo", "ghost_symbol_xyz", "a.rs")],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.local, 0);
    assert_eq!(stats.unexplained, 1);
}

#[test]
fn resolve_is_deterministic_with_local_tier() {
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[call_edge("foo", "playTrack", "a.ts")],
    )
    .unwrap();
    insert_local_binding(&conn, "proj", "a.ts", "foo", "playTrack");

    let first = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    let second = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        first, second,
        "local tier stays deterministic across repeated passes"
    );
    assert_eq!(first.local, 1);
}

// ─── X4 scope qualification (X4 adversarial review, Finding 1) ───

#[test]
fn x4_local_binding_scope_isolates_sibling_functions() {
    // THE core Finding 1 regression: `handler` is bound only inside
    // `sibling`, never inside `foo` — a flat, scope-blind name-set per
    // (project, file) would wrongly classify BOTH edges below as local
    // (the bug). Scope-qualified, only `sibling`'s own call may match.
    let conn = mem();
    upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
    upsert_node(&conn, &def("sibling", "a.ts", "sibling")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[
            call_edge("foo", "handler", "a.ts"),
            call_edge("sibling", "handler", "a.ts"),
        ],
    )
    .unwrap();
    insert_local_binding(&conn, "proj", "a.ts", "sibling", "handler");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.local, 1, "only sibling's own call may classify local");
    assert_eq!(
        stats.unexplained, 1,
        "foo's call to a same-named but differently-scoped binding stays unexplained"
    );

    let foo_boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'foo' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        foo_boundary, "",
        "foo must NOT classify local — the binding belongs to sibling, not foo"
    );

    let sibling_boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'sibling' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        sibling_boundary, "local",
        "sibling's own call, in its own scope, must classify local"
    );
}

#[test]
fn x4_module_level_binding_matches_only_module_level_edges() {
    // Both directions of the module-vs-function scope boundary in one
    // deterministic test: a module-scope (`scope == ""`) binding must
    // match only a module-level edge (src node name == src file), and a
    // function-scope binding must match only its own function's edge —
    // neither leaks into the other.
    let conn = mem();
    upsert_node(&conn, &module_node("mod_a", "a.ts")).unwrap();
    upsert_node(&conn, &def("foo", "a.ts", "foo")).unwrap();
    insert_local_binding(&conn, "proj", "a.ts", "", "ghostGlobal");
    insert_local_binding(&conn, "proj", "a.ts", "foo", "onlyInFoo");
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[
            // E1: module edge -> module-scope binding. Must classify.
            call_edge("mod_a", "ghostGlobal", "a.ts"),
            // E2: function edge -> module-scope binding. Must NOT.
            call_edge("foo", "ghostGlobal", "a.ts"),
            // E3: module edge -> function-scope binding. Must NOT.
            call_edge("mod_a", "onlyInFoo", "a.ts"),
            // E4: function edge -> its OWN function-scope binding. Must.
            call_edge("foo", "onlyInFoo", "a.ts"),
        ],
    )
    .unwrap();

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(stats.local, 2, "only E1 and E4 classify local");
    assert_eq!(stats.unexplained, 2, "E2 and E3 stay unexplained");

    let boundary_of = |src: &str, dst_name: &str| -> String {
        conn.query_row(
            "SELECT boundary FROM code_edges WHERE src_id = ?1 AND dst_id = ?2 AND kind = 'calls'",
            params![src, format!("name:{dst_name}")],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(boundary_of("mod_a", "ghostGlobal"), "local", "E1");
    assert_eq!(boundary_of("foo", "ghostGlobal"), "", "E2");
    assert_eq!(boundary_of("mod_a", "onlyInFoo"), "", "E3");
    assert_eq!(boundary_of("foo", "onlyInFoo"), "local", "E4");
}

// ─── X4 scope CHAINS (second X4 adversarial review, Finding 4:
// sibling anonymous-closure conflation) ───

#[test]
fn chain_prefix_matches_unit_cases() {
    // Direct unit coverage of the matcher itself, decoupled from the DB.
    assert!(
        chain_prefix_matches("Component", "Component"),
        "equal chains match"
    );
    assert!(
        chain_prefix_matches("Component", "Component>anon7"),
        "an outer chain is a prefix of a chain nested inside it"
    );
    assert!(
        chain_prefix_matches("Component>anon3", "Component>anon3>anon9"),
        "flows further inward within the same closure lineage"
    );
    assert!(
        !chain_prefix_matches("Component>anon3", "Component>anon9"),
        "sibling closures at the same depth must NOT match each other"
    );
    assert!(
        !chain_prefix_matches("Component>anon3", "Component"),
        "an inner binding must NOT flow OUTWARD to the enclosing function body"
    );
    assert!(
        !chain_prefix_matches("Component", "Other"),
        "unrelated named defs must never match"
    );
    assert!(
        chain_prefix_matches("", ""),
        "module-level binding matches a module-level call chain"
    );
    assert!(
        !chain_prefix_matches("", "Component"),
        "module-level chain ('') is NOT generalized into a universal prefix"
    );
    assert!(
        !chain_prefix_matches("Component", ""),
        "a function-scoped binding must not match a module-level call"
    );
    assert!(
        !chain_prefix_matches("Component", "ComponentX>anon1"),
        "segment-boundary-safe: a name-prefix collision is not a chain-prefix match"
    );
}

#[test]
fn x4_chain_sibling_anonymous_closures_do_not_conflate() {
    // MANDATED TEST (Finding 4): the core regression this fix addresses.
    // `handler` is bound as a param inside one anonymous closure
    // (`Component>anon3`) and an UNRELATED `handler()` call sits inside
    // a SIBLING anonymous closure (`Component>anon9`) — both closures
    // are nested in the SAME named function `Component`, so BEFORE this
    // fix (flat scope-equality on the bare enclosing-def name) both
    // reported scope `"Component"` and wrongly matched. No def anywhere
    // for `handler`.
    let conn = mem();
    upsert_node(&conn, &def("Component", "a.tsx", "Component")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.tsx",
        &[call_edge("Component", "handler", "a.tsx")],
    )
    .unwrap();
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "handler",
        "calls",
        "Component>anon9",
    );
    insert_local_binding(&conn, "proj", "a.tsx", "Component>anon3", "handler");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.local, 0,
        "a binding in one closure must never witness a call in a sibling closure"
    );
    assert_eq!(stats.unexplained, 1);

    let boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'Component' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(boundary, "", "sibling-closure call must stay unexplained");
}

#[test]
fn x4_chain_outer_binding_flows_into_nested_closure() {
    // MANDATED TEST (Finding 4): "outer bindings flow inward". A
    // component-level binding (`const playTrack = useCallback(...)`,
    // chain == the bare enclosing-def name, no closure nesting of its
    // own) IS visible to a call made from inside a NESTED closure
    // (`Component>anon7`) — the real-world shape `nearest_def_node`'s
    // own doc comment describes (`anukriti-mvp-expo`'s radio-context.tsx).
    let conn = mem();
    upsert_node(&conn, &def("Component", "a.tsx", "Component")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.tsx",
        &[call_edge("Component", "playTrack", "a.tsx")],
    )
    .unwrap();
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "playTrack",
        "calls",
        "Component>anon7",
    );
    insert_local_binding(&conn, "proj", "a.tsx", "Component", "playTrack");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.local, 1,
        "an outer, non-closure-nested binding must witness a call from a nested closure"
    );

    let boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'Component' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(boundary, "local");
}

#[test]
fn x4_chain_nested_shadowing_flows_inward_not_outward_or_sideways() {
    // MANDATED TEST (Finding 4): "nested shadowing sanity" — a binding
    // declared inside ONE closure (`Component>anon1`) must: (E_inner)
    // witness a call nested EVEN DEEPER inside that SAME closure
    // lineage (`Component>anon1>anon5`); (E_outer) NOT witness a call
    // made directly in the enclosing function's own body, OUTSIDE the
    // closure that declared it (`Component`); (E_sibling) NOT witness a
    // call in an unrelated SIBLING closure at the same depth
    // (`Component>anon2`).
    let conn = mem();
    upsert_node(&conn, &def("Component", "a.tsx", "Component")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.tsx",
        &[
            call_edge("Component", "innerCall", "a.tsx"),
            call_edge("Component", "outerCall", "a.tsx"),
            call_edge("Component", "siblingCall", "a.tsx"),
        ],
    )
    .unwrap();
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "innerCall",
        "calls",
        "Component>anon1>anon5",
    );
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "outerCall",
        "calls",
        "Component",
    );
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "siblingCall",
        "calls",
        "Component>anon2",
    );
    insert_local_binding(&conn, "proj", "a.tsx", "Component>anon1", "innerCall");
    insert_local_binding(&conn, "proj", "a.tsx", "Component>anon1", "outerCall");
    insert_local_binding(&conn, "proj", "a.tsx", "Component>anon1", "siblingCall");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.local, 1,
        "only the call nested DEEPER inside the same closure lineage classifies local"
    );

    let boundary_of = |dst_name: &str| -> String {
        conn.query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'Component' AND dst_id = ?1 AND kind = 'calls'",
            params![format!("name:{dst_name}")],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(boundary_of("innerCall"), "local", "flows further inward");
    assert_eq!(
        boundary_of("outerCall"),
        "",
        "must not flow OUTWARD past the closure that declared it"
    );
    assert_eq!(
        boundary_of("siblingCall"),
        "",
        "must not flow SIDEWAYS into an unrelated sibling closure"
    );
}

#[test]
fn x4_chain_falls_back_to_bare_scope_when_no_edge_scope_chains_row() {
    // No `edge_scope_chains` row for this edge (e.g. outside a
    // `backfill_wcr_witnesses` pass) — `call_scope_chains` must fall back
    // to the pre-chain bare enclosing-def name, preserving the ORIGINAL
    // X4 adversarial review (Finding 1) exact-match behavior exactly.
    // This is also implicitly covered by every OTHER X4 test in this
    // module (none of them insert an `edge_scope_chains` row), but this
    // test names the fallback path directly.
    let conn = mem();
    upsert_node(&conn, &def("Component", "a.ts", "Component")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.ts",
        &[call_edge("Component", "playTrack", "a.ts")],
    )
    .unwrap();
    insert_local_binding(&conn, "proj", "a.ts", "Component", "playTrack");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.local, 1,
        "bare-name fallback still matches a bare-name binding"
    );
}

// ─── X4 universal quantification over multi-chain edges (Codex round 4
// adversarial review, Finding 1: one scope chain reused for multiple
// distinct call sites) ───

#[test]
fn x4_multi_chain_aggregate_not_local_when_binding_scope_a_inserted_first() {
    // MANDATED TEST (Finding 1), order A: the SAME callee (`helper`)
    // called from TWO SIBLING anonymous closures aggregates into ONE
    // `code_edges` row (both share src `Component`) but carries TWO
    // distinct `edge_scope_chains` rows — one per physical site. A
    // binding witnessed inside only ONE of the two sibling closures
    // (`Component>anon3`) must NOT classify the aggregate `local`: it
    // is not a prefix of the OTHER occurrence's chain
    // (`Component>anon9`), and Finding 1 requires a witness to be a
    // prefix of EVERY occurrence, not just one. `Component>anon3` row
    // inserted FIRST here; the companion test below inserts
    // `Component>anon9` first — both must produce the identical result.
    let conn = mem();
    upsert_node(&conn, &def("Component", "a.tsx", "Component")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.tsx",
        &[call_edge("Component", "helper", "a.tsx")],
    )
    .unwrap();
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "helper",
        "calls",
        "Component>anon3",
    );
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "helper",
        "calls",
        "Component>anon9",
    );
    insert_local_binding(&conn, "proj", "a.tsx", "Component>anon3", "helper");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.local, 0,
        "a binding that only encloses ONE of the aggregate's two sibling-closure \
             occurrences must never witness the whole aggregate"
    );
    assert_eq!(stats.unexplained, 1);

    let boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'Component' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(boundary, "", "must stay unexplained, not local");
}

#[test]
fn x4_multi_chain_aggregate_not_local_when_binding_scope_b_inserted_first() {
    // MANDATED TEST (Finding 1), order B: same scenario as the test
    // above, with the TWO `edge_scope_chains` rows inserted in the
    // OPPOSITE order (`Component>anon9` first, `Component>anon3`
    // second). Universal quantification over the full chain SET must
    // be order-independent — both tests must assert the identical
    // outcome (`stats.local == 0`).
    let conn = mem();
    upsert_node(&conn, &def("Component", "a.tsx", "Component")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.tsx",
        &[call_edge("Component", "helper", "a.tsx")],
    )
    .unwrap();
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "helper",
        "calls",
        "Component>anon9",
    );
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "helper",
        "calls",
        "Component>anon3",
    );
    insert_local_binding(&conn, "proj", "a.tsx", "Component>anon3", "helper");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.local, 0,
        "insertion order must never change the outcome — same result as scope-A-first"
    );
    assert_eq!(stats.unexplained, 1);

    let boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'Component' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(boundary, "", "must stay unexplained, not local");
}

#[test]
fn x4_multi_chain_aggregate_local_when_binding_prefixes_every_occurrence() {
    // Positive counterpart: a binding declared directly in `Component`'s
    // OWN body (chain == the bare name, no closure nesting of its own)
    // IS a prefix of BOTH sibling closures' chains — universal
    // quantification must still succeed when the witness genuinely
    // encloses every occurrence, proving the fix doesn't just block
    // everything.
    let conn = mem();
    upsert_node(&conn, &def("Component", "a.tsx", "Component")).unwrap();
    codegraph::replace_file_edges(
        &conn,
        "proj",
        "a.tsx",
        &[call_edge("Component", "helper", "a.tsx")],
    )
    .unwrap();
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "helper",
        "calls",
        "Component>anon3",
    );
    insert_edge_scope_chain(
        &conn,
        "a.tsx",
        "Component",
        "helper",
        "calls",
        "Component>anon9",
    );
    insert_local_binding(&conn, "proj", "a.tsx", "Component", "helper");

    let stats = resolve_edges(&conn, "proj", &|_: &str| true).unwrap();
    assert_eq!(
        stats.local, 1,
        "an outer binding that encloses EVERY occurrence must still classify local"
    );

    let boundary: String = conn
        .query_row(
            "SELECT boundary FROM code_edges WHERE src_id = 'Component' AND kind = 'calls'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(boundary, "local");
}

#[test]
fn a_planned_resolve_replayed_equals_the_direct_resolve() {
    let direct = mem();
    let planned = mem();
    mixed_graph(&direct);
    mixed_graph(&planned);
    let exists = |f: &str| f != "gone.rs";

    let want = resolve_edges(&direct, "proj", &exists).unwrap();
    let before = edge_dump(&planned);
    let (got, writes) = plan_resolve(&planned, "proj", &exists).unwrap();
    assert_eq!(edge_dump(&planned), before, "planning writes nothing");
    assert!(!writes.is_empty());
    assert_eq!(apply_planned(&planned, &writes).unwrap(), 0);

    assert_eq!(edge_dump(&planned), edge_dump(&direct));
    assert_eq!(got.resolved, want.resolved);
    assert_eq!(got.stale, want.stale);
    assert_eq!(got.ambiguous_remaining, want.ambiguous_remaining);
    // The fixture reaches the verdicts the test is about.
    let dump = edge_dump(&direct);
    let find = |src: &str, dst: &str| {
        dump.iter()
            .find(|e| e.0 == src && e.1.ends_with(dst))
            .cloned()
            .unwrap()
    };
    assert_eq!(find("foo", "bar").3, 1);
    assert_eq!(find("foo", "baz").3, 1);
    assert_eq!(find("caller", "dup_x").3, 1, "co-edit breaks the tie");
    assert_eq!(find("caller", "twin_x").3, 1, "co-edit breaks the tie");
    assert_eq!(find("ghost", "name:mystery").5, "file_missing");
}

#[test]
fn the_plan_ends_with_the_pass_and_a_later_resolve_writes_through() {
    let conn = mem();
    mixed_graph(&conn);
    let exists = |f: &str| f != "gone.rs";
    // A pass that dies while planning leaves no plan behind on its thread.
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = plan_resolve(&conn, "proj", &|_: &str| panic!("killed mid-plan"));
    }));
    assert!(died.is_err());
    assert!(!planning());
    assert!(edge_dump(&conn).iter().all(|e| e.3 == 0), "nothing written");

    resolve_edges(&conn, "proj", &exists).unwrap();
    assert!(edge_dump(&conn).iter().any(|e| e.3 == 1), "writes through");
}

#[test]
fn a_write_planned_for_an_edge_that_has_since_changed_is_left_alone() {
    let conn = mem();
    mixed_graph(&conn);
    let (_, writes) = plan_resolve(&conn, "proj", &|_: &str| true).unwrap();
    // An edit re-extracts a.rs between the plan and the replay: foo -> bar
    // is a fresh placeholder with different evidence.
    conn.execute(
        "UPDATE code_edges SET evidence = 'via:other' WHERE src_id = 'foo' AND dst_id = 'name:bar'",
        [],
    )
    .unwrap();
    assert_eq!(apply_planned(&conn, &writes).unwrap(), 1);
    let dump = edge_dump(&conn);
    let foo_bar = dump
        .iter()
        .find(|e| e.0 == "foo" && e.1 == "name:bar")
        .unwrap();
    assert_eq!((foo_bar.3, foo_bar.5.as_str()), (0, "via:other"));
    assert!(
        dump.iter()
            .any(|e| e.0 == "foo" && e.1 == "baz" && e.3 == 1),
        "the other edges were applied"
    );
}
