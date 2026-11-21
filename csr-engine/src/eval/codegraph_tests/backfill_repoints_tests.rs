use super::*;

// ─── content-identity gate on legacy-attribution re-point (Codex round
// 6/7 adversarial review): correspondence alone proves NAME/KIND
// attribution under the frozen old rule, never cross-version SITE
// identity — see `historical_src_content_unchanged`'s doc comment for
// the full finding. ───

#[test]
fn backfill_never_repoints_when_historical_edge_hash_mismatches_current_content() {
    // MANDATED REGRESSION TEST (Codex round 6 adversarial review,
    // verbatim scenario; upgraded to the round-7 edge-level hash by this
    // test's own rewrite): the historical module-src `helper` edge was
    // recorded from a PRIOR version of this file where a closure inside
    // `Alpha` called `helper()`. Since then the file was edited:
    // `Alpha`'s call was REMOVED, and an unrelated closure inside a
    // NEWLY ADDED `Beta` was added that ALSO calls `helper()`. Both
    // Alpha's (historical) and Beta's (fresh) closures legacy-attribute
    // to the MODULE node under the frozen pre-92179d1 rule — a single,
    // unique current-src candidate ("Beta") — so round 5's
    // correspondence rule alone would wrongly re-point the historical
    // edge to Beta, crediting gate closure the evidence does not
    // support: Beta's call was never proven to be the SAME physical call
    // Alpha's was. The historical EDGE's own stamped `src_content_hash`
    // (computed from the OLD, Alpha-containing content) does NOT match
    // the fresh whole-file hash (computed from the CURRENT,
    // Beta-containing content on disk) — the content-identity gate must
    // refuse the re-point. The edge must fall through to the ordinary
    // drift guard exactly as if no legacy correspondence existed at all
    // (module-src edges always satisfy `can_drift`'s condition (c)),
    // never silently guessed onto Beta.
    let old_content =
        "function Alpha() {\n    useEffect(() => {\n        helper();\n    }, []);\n}\n";
    let current_content =
        "function Beta() {\n    useEffect(() => {\n        helper();\n    }, []);\n}\n";
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    std::fs::write(&file_path, current_content).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "tsx".into(),
            kind: "module".into(),
            name: file_str.clone(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    // `Beta`'s real node — present in the shadow exactly as it would be
    // from a real prior extraction. Seeded so that, if the content-
    // identity gate were ever removed or bypassed, the re-point would
    // actually succeed (proving this test catches a real regression,
    // not merely a missing shadow lookup entry).
    let beta_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "Beta");
    seed_backfill_node(&shadow, &beta_id, &file_str, "Beta");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[EdgeRow {
                src_id: module_id.clone(),
                dst_id: "name:helper".into(),
                kind: "calls".into(),
                src_file: file_str.clone(),
                resolved: 0,
                weight: 1.0,
                callee_kind: "direct".into(),
                // Historical EDGE hash from the OLD (Alpha) content —
                // deliberately DIFFERENT from what a fresh re-parse of
                // `current_content` (Beta) will produce. Real, non-empty
                // — isolates "mismatched" from "missing" (the next test).
                src_content_hash: crate::extraction::codegraph::body_hash(old_content),
                ..EdgeRow::default()
            }],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(
        files, 1,
        "the on-disk (current, Beta-containing) file was re-extracted"
    );
    assert_eq!(
        edges, 0,
        "content-identity mismatch refuses the re-point — never counted as updated"
    );

    let (src_id, boundary, evidence, resolved): (String, String, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id, boundary, evidence, resolved FROM code_edges
                     WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        src_id, module_id,
        "must NOT re-point to Beta — content identity is unprovable across the edit"
    );
    assert_ne!(src_id, beta_id);
    assert_eq!(
        boundary, "drifted",
        "falls through to the ordinary drift guard — same honest classification as \
             when no legacy correspondence exists at all"
    );
    assert_eq!(evidence, "not_in_current_source");
    assert_eq!(resolved, 0);
}

#[test]
fn backfill_never_repoints_on_partial_write_skew_node_hash_ahead_of_edge_hash() {
    // MANDATED REGRESSION (Codex round 7 adversarial review, verbatim
    // scenario — the finding this whole fix responds to): a partial live
    // write left `code_nodes.body_hash` refreshed to CURRENT content
    // (simulating a real `upsert_node` that ran successfully) while the
    // `code_edges` row for an old Alpha-closure call was never
    // re-extracted (simulating `replace_file_edges` failing, or simply
    // not having run yet, in its SEPARATE transaction — see
    // `storage::codegraph::replace_file_edges`'s doc comment). Same
    // Alpha/Beta shape as the mismatch test above: the historical edge
    // was recorded when a closure inside `Alpha` called `helper()`;
    // since then `Alpha`'s call was REMOVED and an unrelated closure
    // inside a NEWLY ADDED `Beta` was added that ALSO calls `helper()`,
    // so Beta is the sole legacy-correspondence candidate. The OLD
    // (round-6) gate compared the src NODE's hash — which here DOES
    // match current content (the node was refreshed) — and would have
    // wrongly authenticated the re-point onto Beta. The round-7 gate
    // reads the EDGE's own hash instead, which is absent/stale — never
    // refreshed by the node upsert — so it correctly refuses regardless
    // of what `code_nodes.body_hash` says. An EMPTY edge hash (item 3 of
    // the fix) also means NO evidence either way, so the edge is left
    // completely untouched rather than drifted — see the assertion
    // below.
    let current_content =
        "function Beta() {\n    useEffect(() => {\n        helper();\n    }, []);\n}\n";
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    std::fs::write(&file_path, current_content).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "tsx".into(),
            kind: "module".into(),
            name: file_str.clone(),
            // The NODE's hash is CURRENT — refreshed to match
            // `current_content` (Beta), exactly as a real `upsert_node`
            // call would leave it after re-extracting this file. This is
            // the partial-write skew: node state says "fresh", edge
            // state (below) says otherwise.
            body_hash: crate::extraction::codegraph::body_hash(current_content),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    let beta_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "Beta");
    seed_backfill_node(&shadow, &beta_id, &file_str, "Beta");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[EdgeRow {
                src_id: module_id.clone(),
                dst_id: "name:helper".into(),
                kind: "calls".into(),
                src_file: file_str.clone(),
                resolved: 0,
                weight: 1.0,
                callee_kind: "direct".into(),
                // The EDGE's own hash is ABSENT — never stamped, exactly
                // what a stale row that missed a `replace_file_edges`
                // pass looks like. Deliberately NOT set to the OLD
                // (Alpha) content either — "absent/stale" per the
                // mandated scenario; the missing-hash test above already
                // covers this exact shape, so this test's own distinct
                // contribution is the node hash being CURRENT, not the
                // edge hash being missing.
                ..EdgeRow::default()
            }],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(
        files, 1,
        "the on-disk (current, Beta-containing) file was re-extracted"
    );
    assert_eq!(
        edges, 0,
        "partial write skew (node hash current, edge hash absent) refuses the \
             re-point — the gate must never trust code_nodes.body_hash"
    );

    let (src_id, boundary, resolved): (String, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id, boundary, resolved FROM code_edges
                     WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        src_id, module_id,
        "must NOT re-point to Beta merely because the SRC NODE's hash looks current"
    );
    assert_ne!(src_id, beta_id);
    assert_eq!(
        boundary, "",
        "item 3 of the fix: an EMPTY edge hash is NO evidence either way — left \
             completely untouched, never drifted (drift would overclaim positive \
             knowledge this backfill does not have)"
    );
    assert_eq!(resolved, 0);
}

#[test]
fn backfill_never_repoints_when_historical_edge_hash_is_missing() {
    // MANDATED TEST (Codex round 6 adversarial review; edge-level, and
    // "stays pending rather than drifts", as of round 7's item 3 — see
    // `historical_src_content_unchanged`'s doc comment): a historical
    // EDGE whose `src_content_hash` was never recorded (empty string —
    // a legacy edge written before this column existed) must NEVER be
    // treated as a match by accident, AND must NOT be drift-classified
    // either — an empty hash is NO evidence either way about whether
    // this file changed, so claiming "the call is gone" (drift) would
    // overclaim. `body_hash` is a sha256 digest and is never the empty
    // string for any real text, so an empty stored hash can never
    // coincidentally equal a real fresh one — this test locks that
    // invariant in directly rather than relying on it being merely
    // accidental. Content is otherwise UNCHANGED (same shape as the
    // hash-match positive test) — the only difference is the missing
    // historical hash — so this isolates "missing" from "mismatched"
    // (the sibling test, immediately above, which DOES drift).
    let source =
        "function Component() {\n    useEffect(() => {\n        helper();\n    }, []);\n}\n";
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    std::fs::write(&file_path, source).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "tsx".into(),
            kind: "module".into(),
            name: file_str.clone(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    let component_id =
        crate::extraction::codegraph::node_id("repo", &file_str, "function", "Component");
    seed_backfill_node(&shadow, &component_id, &file_str, "Component");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[EdgeRow {
                src_id: module_id.clone(),
                dst_id: "name:helper".into(),
                kind: "calls".into(),
                src_file: file_str.clone(),
                resolved: 0,
                weight: 1.0,
                callee_kind: "direct".into(),
                // Deliberately MISSING — `EdgeRow::default()`'s
                // `src_content_hash` is "", never overwritten here.
                ..EdgeRow::default()
            }],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(files, 1);
    assert_eq!(
        edges, 0,
        "a missing historical hash refuses the re-point, same as a mismatched one"
    );

    let (src_id, boundary, resolved): (String, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id, boundary, resolved FROM code_edges
                     WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        src_id, module_id,
        "must NOT re-point to Component — no historical hash to verify against"
    );
    assert_ne!(src_id, component_id);
    assert_eq!(
        boundary, "",
        "item 3 of the fix: a missing edge hash leaves the edge completely \
             untouched — pending, unexplained — never drifted, unlike a genuine \
             content mismatch (the sibling test, immediately above)"
    );
    assert_eq!(resolved, 0);
}

#[test]
fn historical_src_content_unchanged_gates_on_edge_hash_not_node_hash() {
    // MANDATED TEST (Codex round 7 adversarial review — supersedes the
    // round-6 version of this test, which exercised a `(kind, name) ->
    // code_nodes.body_hash` map; that mechanism is REMOVED, see
    // `historical_src_content_unchanged`'s doc comment for the full
    // finding): direct unit coverage of the gate function itself, now a
    // plain two-string comparison between a pending EDGE's own stamped
    // `src_content_hash` and the file's freshly recomputed content hash.
    // Matching hash allows; mismatched, and missing (empty), hashes
    // refuse — never a guess either way.
    let fresh_file_hash = "abc123";

    // Matching edge hash -> allowed.
    assert!(historical_src_content_unchanged("abc123", fresh_file_hash));
    // Mismatched edge hash -> refused.
    assert!(!historical_src_content_unchanged("zzz999", fresh_file_hash));
    // Missing (empty) edge hash -> refused — never a guess, even though
    // an empty string trivially can't equal a real sha256 digest; this
    // locks the invariant in directly rather than relying on it being
    // merely accidental.
    assert!(!historical_src_content_unchanged("", fresh_file_hash));
    // Missing (empty) FRESH hash is not a real reachable shape (`body_hash`
    // never returns "" for real content), but must refuse rather than
    // vacuously match two empty strings.
    assert!(!historical_src_content_unchanged("", ""));
}

#[test]
fn backfill_never_repoints_bare_name_survivor_without_legacy_match() {
    // MANDATED TEST (Codex round 5 adversarial review): the EXACT
    // unsoundness the round-4 candidate-COUNT rule had — this used to
    // be `backfill_repoints_when_exactly_one_candidate_src_exists`,
    // asserting a WRONG re-point. `alpha` calls `helper()` directly
    // (not closure-nested — its legacy attribution, under the frozen
    // pre-92179d1 rule, is `alpha` itself, never `ghost`), so it does
    // NOT legacy-correspond to the historical `ghost`-sourced edge.
    // `ghost` itself is entirely ABSENT from the fresh file too, so the
    // ordinary drift guard's calling-symbol-present condition also
    // fails. The edge must stay pending, unexplained — NEVER
    // re-pointed onto `alpha` merely because `helper` survives there.
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.rs");
    std::fs::write(&file_path, "fn alpha() {\n    helper();\n}\n").unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let ghost_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "ghost");
    seed_backfill_node(&shadow, &ghost_id, &file_str, "ghost");
    let alpha_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "alpha");
    seed_backfill_node(&shadow, &alpha_id, &file_str, "alpha");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[stale_calls_edge(&ghost_id, "helper", &file_str, "direct")],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(files, 1);
    assert_eq!(
        edges, 0,
        "alpha's legacy attribution is itself, not ghost — no correspondence, no re-point"
    );
    let (src_id, boundary, resolved, callee_kind): (String, String, i64, String) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id, boundary, resolved, callee_kind FROM code_edges
                     WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        src_id, ghost_id,
        "must stay on its ORIGINAL stale src — never guessed onto alpha"
    );
    assert_ne!(src_id, alpha_id);
    assert_eq!(
        boundary, "",
        "ghost itself is absent from the fresh extraction — not trustworthy drift evidence"
    );
    assert_eq!(resolved, 0);
    assert_eq!(
        callee_kind, "direct",
        "untouched — the original stale value"
    );
}

#[test]
fn backfill_drifts_ghost_call_removal_despite_unrelated_direct_survivor() {
    // MANDATED TEST (Codex round 5 adversarial review — the flagged
    // finding itself): `ghost` STILL EXISTS in the fresh file
    // (conditions (b)/(c) both hold) but no longer calls `helper()` —
    // a genuinely REMOVED call. `alpha`, an unrelated, unchanged
    // function, still calls `helper()` directly — its legacy
    // attribution is `alpha`, never `ghost`, so no fresh site
    // legacy-corresponds to the historical `ghost`-sourced edge.
    // Before this fix, `helper`'s bare-name survival via `alpha` (the
    // sole round-4 "candidate") would have wrongly re-pointed the edge
    // onto `alpha`, corrupting provenance and silently suppressing
    // this legitimate drift. After the fix, the edge correctly falls
    // through to the ordinary drift guard and drifts honestly.
    //
    // POSITIVE CONTROL (Codex round 8, mandated): stamped with a hash
    // matching the fresh file's own recomputed hash — see
    // `backfill_marks_drifted_for_edges_vanished_from_fresh_extraction`'s
    // own positive-control comment for the full rationale. See
    // `backfill_leaves_empty_stamped_calls_edge_unresolved_when_legacy_correspondence_absent`,
    // immediately below, for the sibling regression — same scenario,
    // empty stamp, must NOT drift.
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.rs");
    let source = "fn ghost() {}\nfn alpha() {\n    helper();\n}\n";
    std::fs::write(&file_path, source).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let ghost_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "ghost");
    seed_backfill_node(&shadow, &ghost_id, &file_str, "ghost");
    let alpha_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "alpha");
    seed_backfill_node(&shadow, &alpha_id, &file_str, "alpha");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[EdgeRow {
                src_content_hash: crate::extraction::codegraph::body_hash(source),
                ..stale_calls_edge(&ghost_id, "helper", &file_str, "direct")
            }],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(files, 1);
    assert_eq!(
        edges, 0,
        "mark_drifted does not increment edges_updated — only exact-match/re-point do \
             (see backfill_marks_drifted_for_edges_vanished_from_fresh_extraction)"
    );

    let (src_id, boundary, evidence, resolved): (String, String, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id, boundary, evidence, resolved FROM code_edges
                     WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        src_id, ghost_id,
        "drift never moves src_id — stays on ghost"
    );
    assert_ne!(src_id, alpha_id);
    assert_eq!(
        boundary, "drifted",
        "ghost's own call to helper() was genuinely removed — no legacy correspondence \
             to alpha's unrelated direct call rescues it"
    );
    assert_eq!(evidence, "not_in_current_source");
    assert_eq!(resolved, 0);
}

#[test]
fn backfill_leaves_empty_stamped_calls_edge_unresolved_when_legacy_correspondence_absent() {
    // MANDATED REGRESSION (Codex round 8 adversarial review, item 1 —
    // the empty-stamp invariant): same scenario as
    // `backfill_drifts_ghost_call_removal_despite_unrelated_direct_survivor`
    // immediately above (`ghost`'s own call to `helper()` was removed;
    // `alpha`'s unrelated direct call means `helper` bare-name survives
    // but never legacy-corresponds to `ghost` — `call_legacy_correspondence`
    // misses, hitting the `None` arm), except `ghost`'s edge carries NO
    // stamp at all. Before this fix, the `None` arm called `mark_drifted`
    // on `can_drift` alone, with no content-hash check — an unstamped
    // legacy edge could earn drift credit here even though there is no
    // evidence either way about whether this file's content has changed
    // since the edge was recorded. After the fix, `mark_drifted` itself
    // refuses an empty stamp structurally — the edge stays pending,
    // unexplained, empty boundary. See that function's doc comment for
    // the full invariant.
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.rs");
    std::fs::write(
        &file_path,
        "fn ghost() {}\nfn alpha() {\n    helper();\n}\n",
    )
    .unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let ghost_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "ghost");
    seed_backfill_node(&shadow, &ghost_id, &file_str, "ghost");
    let alpha_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "alpha");
    seed_backfill_node(&shadow, &alpha_id, &file_str, "alpha");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            // Deliberately unstamped — `stale_calls_edge`'s default
            // `src_content_hash` is "".
            &[stale_calls_edge(&ghost_id, "helper", &file_str, "direct")],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(files, 1);
    assert_eq!(edges, 0, "an unstamped edge is never counted as updated");

    let (src_id, boundary, evidence, resolved): (String, String, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id, boundary, evidence, resolved FROM code_edges
                     WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(src_id, ghost_id, "drift-or-not never moves src_id");
    assert_ne!(src_id, alpha_id);
    assert_eq!(
        boundary, "",
        "an empty-stamped legacy edge must NEVER earn drift credit, even when no \
             legacy correspondence exists to rescue it — no stamp means no provable link \
             to any recorded file state"
    );
    assert_eq!(evidence, "", "left completely untouched, not just boundary");
    assert_eq!(resolved, 0, "still pending, never classified either way");
}

#[test]
fn backfill_never_repoints_module_src_edge_when_closure_removed_and_unrelated_direct_survivor_exists(
) {
    // MANDATED TEST (Codex round 5 adversarial review, scenario 3): the
    // historical edge carries the OLD module-src attribution (as if
    // extracted from a PRIOR version of the file with a closure-nested
    // `helper()` call, same shape as
    // `backfill_repoints_closure_call_from_stale_module_src_instead_of_drifting`,
    // above) — but the file has SINCE been edited: the closure is gone
    // entirely, replaced by an UNRELATED top-level function `Alpha`
    // that calls `helper()` directly (not closure-nested). `Alpha`'s
    // legacy attribution is `Alpha` itself, never the module, so it
    // does NOT legacy-correspond to the historical module-src edge —
    // re-pointing to `Alpha` would be exactly the same corruption as
    // the ghost/alpha case, just via the module fallback instead of a
    // named ghost. The edge must fall through to the ordinary drift
    // guard, which — for a module-src edge — always drifts (condition
    // (c)'s `src_name == src_file` case).
    //
    // POSITIVE CONTROL (Codex round 8, mandated): stamped with a hash
    // matching the fresh file's own recomputed hash so this test keeps
    // exercising genuine drift under the empty-stamp invariant added to
    // `mark_drifted` — see that function's doc comment, and
    // `backfill_leaves_empty_stamped_calls_edge_unresolved_when_legacy_correspondence_absent`
    // for the dedicated empty-stamp regression covering this same
    // `None`-correspondence branch.
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    let source = "function Alpha() {\n    helper();\n}\n";
    std::fs::write(&file_path, source).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "tsx".into(),
            kind: "module".into(),
            name: file_str.clone(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    let alpha_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "Alpha");
    shadow
        .upsert_code_node(&NodeRow {
            id: alpha_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "tsx".into(),
            kind: "function".into(),
            name: "Alpha".into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[EdgeRow {
                src_id: module_id.clone(),
                dst_id: "name:helper".into(),
                kind: "calls".into(),
                src_file: file_str.clone(),
                resolved: 0,
                weight: 1.0,
                callee_kind: "direct".into(),
                src_content_hash: crate::extraction::codegraph::body_hash(source),
                ..EdgeRow::default()
            }],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(files, 1);
    assert_eq!(
        edges, 0,
        "mark_drifted does not increment edges_updated — only exact-match/re-point do"
    );

    let (src_id, boundary, evidence, resolved): (String, String, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id, boundary, evidence, resolved FROM code_edges
                     WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        src_id, module_id,
        "drift never moves src_id — stays on the module node"
    );
    assert_ne!(src_id, alpha_id);
    assert_eq!(
        boundary, "drifted",
        "Alpha's unrelated direct call never legacy-corresponds to the module fallback \
             — no re-point, honest drift instead"
    );
    assert_eq!(evidence, "not_in_current_source");
    assert_eq!(resolved, 0);
}

// ─── end-to-end scope-chain threading (Codex round 3 adversarial
// review): backfill -> resolve, through the REAL id-translation path
// (`shadow_name_to_id`), not synthetic `insert_edge_scope_chain` rows —
// this is exactly the class of bug the id-space mismatch was: every
// resolver-level X4 chain test used HAND-SEEDED `edge_scope_chains`
// rows and passed regardless, while the real backfill-produced rows
// (keyed by the fresh-fragment's OWN `repo = ""` ids) silently never
// matched `code_edges.src_id` (keyed by the live pipeline's real
// `repo = project` ids) at read time. These two tests exercise the
// FULL pipeline — `backfill_wcr_witnesses` then
// `resolve_code_edges_with_fs_check` — so a regression in the
// translation step itself (not just the matching logic) fails a test. ───

#[test]
fn backfill_then_resolve_sibling_closures_do_not_conflate_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    std::fs::write(
        &file_path,
        "function Component() {\n\
             \x20   useCallback((handler) => {\n\
             \x20       return handler;\n\
             \x20   }, []);\n\
             \x20   useCallback(() => {\n\
             \x20       handler();\n\
             \x20   }, []);\n\
             }\n",
    )
    .unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    // `Component` already present in the shadow's `code_nodes` — same
    // "already there from a real prior extraction" convention as the
    // re-point tests above; this is what `shadow_name_to_id` resolves
    // against when translating `fragment.call_scope_chains`.
    let component_id =
        crate::extraction::codegraph::node_id("repo", &file_str, "function", "Component");
    seed_backfill_node(&shadow, &component_id, &file_str, "Component");
    // Already correctly attributed (GRAPH src == Component) — this
    // test isolates CHAIN threading, not re-pointing (covered above).
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[stale_calls_edge(
                &component_id,
                "handler",
                &file_str,
                "direct",
            )],
        )
        .unwrap();

    backfill_wcr_witnesses(&shadow).unwrap();
    let stats = shadow
        .resolve_code_edges_with_fs_check("proj", &|_: &str| true)
        .unwrap();
    assert_eq!(
        stats.local, 0,
        "a param bound in one closure must never witness a call in a sibling \
             closure, end-to-end through real backfill-produced (translated) chain data"
    );

    let boundary: String = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT boundary FROM code_edges WHERE src_id = ?1 AND dst_id = 'name:handler'",
                [&component_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_ne!(boundary, "local");
}

#[test]
fn backfill_then_resolve_outer_binding_flows_into_nested_closure_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    std::fs::write(
        &file_path,
        "function Component() {\n\
             \x20   const playTrack = useCallback((track) => {\n\
             \x20       doPlay(track);\n\
             \x20   }, []);\n\
             \x20   useCallback(() => {\n\
             \x20       playTrack();\n\
             \x20   }, [playTrack]);\n\
             }\n",
    )
    .unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let component_id =
        crate::extraction::codegraph::node_id("repo", &file_str, "function", "Component");
    seed_backfill_node(&shadow, &component_id, &file_str, "Component");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[stale_calls_edge(
                &component_id,
                "playTrack",
                &file_str,
                "direct",
            )],
        )
        .unwrap();

    backfill_wcr_witnesses(&shadow).unwrap();
    let stats = shadow
        .resolve_code_edges_with_fs_check("proj", &|_: &str| true)
        .unwrap();
    assert_eq!(
        stats.local, 1,
        "an outer, non-closure-nested binding must witness a call from a nested \
             sibling closure, end-to-end through real backfill-produced (translated) chain data"
    );

    let boundary: String = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT boundary FROM code_edges WHERE src_id = ?1 AND dst_id = 'name:playTrack'",
                [&component_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(boundary, "local");
}

#[test]
fn backfill_repoint_dedupes_instead_of_colliding_when_target_already_exists() {
    // MANDATED REGRESSION (found live, not hypothetical): a file can
    // carry BOTH a stale, old-attribution `helper` edge (src = the
    // module node, pre-92179d1 shape) AND an ALREADY-correctly-
    // attributed `helper` edge (src = `Component`, e.g. from a live
    // watcher re-extraction that already ran post-fix) for the SAME
    // callee simultaneously. A blind re-point `UPDATE` on the stale one
    // would collide with `code_edges`' own PRIMARY KEY
    // `(src_id, dst_id, kind)` — this is EXACTLY what the live gate hit
    // running this fix against the real corpus (`UNIQUE constraint
    // failed: code_edges.src_id, code_edges.dst_id, code_edges.kind`),
    // aborting the ENTIRE backfill pass (no per-file catch) and
    // silently leaving every file after the collision un-backfilled.
    //
    // Codex round 7: content unchanged since recording (`source` is both
    // on disk and the historical stale EDGE's own stamped hash's
    // source), so the content-identity gate must pass and this dedupe
    // path must still exercise a real re-point.
    let source =
        "function Component() {\n    useEffect(() => {\n        helper();\n    }, []);\n}\n";
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    std::fs::write(&file_path, source).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "tsx".into(),
            kind: "module".into(),
            name: file_str.clone(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    let component_id =
        crate::extraction::codegraph::node_id("repo", &file_str, "function", "Component");
    seed_backfill_node(&shadow, &component_id, &file_str, "Component");
    // TWO pending edges for the SAME callee: one stale (module_id, the
    // bug this whole finding is about), one ALREADY correct
    // (component_id — an exact match against the fresh extraction).
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[
                EdgeRow {
                    src_id: module_id.clone(),
                    dst_id: "name:helper".into(),
                    kind: "calls".into(),
                    src_file: file_str.clone(),
                    resolved: 0,
                    weight: 1.0,
                    callee_kind: "direct".into(),
                    src_content_hash: crate::extraction::codegraph::body_hash(source),
                    ..EdgeRow::default()
                },
                EdgeRow {
                    src_id: component_id.clone(),
                    dst_id: "name:helper".into(),
                    kind: "calls".into(),
                    src_file: file_str.clone(),
                    resolved: 0,
                    weight: 1.0,
                    callee_kind: "direct".into(),
                    ..EdgeRow::default()
                },
            ],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow)
        .expect("must not abort with a PRIMARY KEY collision — dedup, don't crash the whole pass");
    assert_eq!(files, 1);
    assert_eq!(
        edges, 2,
        "both edges are handled — the exact match AND the dedup-delete"
    );

    let stale_row_count: i64 = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM code_edges WHERE src_id = ?1 AND dst_id = 'name:helper'",
                [&module_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        stale_row_count, 0,
        "the confirmed-duplicate stale row must be deleted, not left as a collision retry"
    );

    let (surviving_count, boundary, resolved): (i64, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*), MAX(boundary), MAX(resolved) FROM code_edges
                     WHERE src_id = ?1 AND dst_id = 'name:helper'",
                [&component_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(surviving_count, 1, "exactly one surviving row, not two");
    assert_eq!(boundary, "", "the surviving row is a normal pending edge");
    assert_eq!(resolved, 0);
}

#[test]
fn backfill_repoint_dedupe_merges_weight_and_earliest_provenance_into_surviving_row() {
    // MANDATED TEST (Codex round 4 adversarial review, Finding 4): the
    // pre-fix version deleted the stale row on collision and left the
    // surviving (target) row completely untouched — silently discarding
    // whatever the stale row alone carried. This test gives the two
    // colliding rows DISTINCT weight/conv_id/session_id/callee_kind so
    // the merge (not "delete and ignore") is directly observable:
    // weight SUMS (additive, per `add_edge`'s own established
    // semantics), conv_id/session_id keep the EARLIEST-inserted row's
    // own pairing (never split across rows), and callee_kind/evidence
    // are refreshed from the CURRENT fresh extraction on the
    // surviving row regardless of which branch touches it.
    //
    // Codex round 7: content unchanged since recording — same
    // content-identity gate note as the dedupe test above (this test's
    // stale edge now carries its OWN stamped hash, not the src node's).
    let source =
        "function Component() {\n    useEffect(() => {\n        helper();\n    }, []);\n}\n";
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.tsx");
    std::fs::write(&file_path, source).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "tsx".into(),
            kind: "module".into(),
            name: file_str.clone(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    let component_id =
        crate::extraction::codegraph::node_id("repo", &file_str, "function", "Component");
    seed_backfill_node(&shadow, &component_id, &file_str, "Component");
    // Stale row inserted FIRST (smaller `rowid` — "earliest") with its
    // OWN distinct weight/conv/session; target row inserted SECOND.
    // Both carry a deliberately WRONG `callee_kind` ("method") to prove
    // the surviving row's callee_kind is refreshed from the fresh
    // extraction (a real "direct" call), not merely preserved from
    // either historical row.
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[
                EdgeRow {
                    src_id: module_id.clone(),
                    dst_id: "name:helper".into(),
                    kind: "calls".into(),
                    src_file: file_str.clone(),
                    resolved: 0,
                    weight: 3.0,
                    conv_id: "conv-old".into(),
                    session_id: "sess-old".into(),
                    callee_kind: "method".into(),
                    src_content_hash: crate::extraction::codegraph::body_hash(source),
                    ..EdgeRow::default()
                },
                EdgeRow {
                    src_id: component_id.clone(),
                    dst_id: "name:helper".into(),
                    kind: "calls".into(),
                    src_file: file_str.clone(),
                    resolved: 0,
                    weight: 5.0,
                    conv_id: "conv-new".into(),
                    session_id: "sess-new".into(),
                    callee_kind: "method".into(),
                    ..EdgeRow::default()
                },
            ],
        )
        .unwrap();

    backfill_wcr_witnesses(&shadow).expect("must not abort on collision");

    let (surviving_count, weight, conv_id, session_id, callee_kind): (
        i64,
        f64,
        String,
        String,
        String,
    ) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*), MAX(weight), MAX(conv_id), MAX(session_id), MAX(callee_kind)
                     FROM code_edges WHERE src_id = ?1 AND dst_id = 'name:helper'",
                [&component_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(surviving_count, 1, "exactly one surviving row");
    assert_eq!(
        weight, 8.0,
        "weight must SUM (3.0 stale + 5.0 target), not be discarded"
    );
    assert_eq!(
        conv_id, "conv-old",
        "conv_id/session_id pairing from the EARLIEST-inserted (stale) row must survive"
    );
    assert_eq!(session_id, "sess-old");
    assert_eq!(
        callee_kind, "direct",
        "callee_kind must be refreshed from the CURRENT fresh extraction on the surviving row"
    );

    let stale_row_count: i64 = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM code_edges WHERE src_id = ?1 AND dst_id = 'name:helper'",
                [&module_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        stale_row_count, 0,
        "the stale row is deleted after its data is merged"
    );
}

// ─── shadow_name_to_id kind-qualification (Codex round 4 adversarial
// review, Finding 3) ───

#[test]
fn backfill_never_repoints_via_kind_ambiguous_name_collision() {
    // MANDATED-STYLE TEST (Finding 3, Codex round 4 — re-verified under
    // Codex round 5's legacy-correspondence rule): a bare NAME that
    // collides across TWO DIFFERENT node kinds in this shadow's own
    // `code_nodes` (a `function Component` AND a `type Component`, both
    // already present — `node_id` disambiguates them by kind, so both
    // coexist) must NEVER be resolved by picking one lexicographically
    // — even once the WINNING candidate is chosen via legacy
    // correspondence rather than bare-name count. The historical edge
    // carries the OLD module-src attribution (a closure-nested call,
    // same shape as
    // `backfill_repoints_closure_call_from_stale_module_src_instead_of_drifting`,
    // above): its sole legacy-matched fresh site's CURRENT attribution
    // is named "Component" — `shadow_name_to_id` must resolve that
    // (kind, name) pair to the FUNCTION node, never the co-named TYPE
    // node.
    //
    // Codex round 7: content unchanged since recording — same
    // content-identity gate note as the dedupe tests above (the
    // historical edge below carries its OWN stamped hash, not the src
    // node's).
    let source =
        "function Component() {\n    useEffect(() => {\n        helper();\n    }, []);\n}\n";
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.ts");
    std::fs::write(&file_path, source).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "typescript".into(),
            kind: "module".into(),
            name: file_str.clone(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    let component_fn_id =
        crate::extraction::codegraph::node_id("repo", &file_str, "function", "Component");
    seed_backfill_node(&shadow, &component_fn_id, &file_str, "Component");
    // A DIFFERENT node, same bare name, DIFFERENT kind — must not be
    // confused with `component_fn_id` above by a kind-blind lookup.
    let component_type_id =
        crate::extraction::codegraph::node_id("repo", &file_str, "type", "Component");
    shadow
        .upsert_code_node(&NodeRow {
            id: component_type_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "typescript".into(),
            kind: "type".into(),
            name: "Component".into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[EdgeRow {
                src_id: module_id.clone(),
                dst_id: "name:helper".into(),
                kind: "calls".into(),
                src_file: file_str.clone(),
                resolved: 0,
                weight: 1.0,
                callee_kind: "direct".into(),
                src_content_hash: crate::extraction::codegraph::body_hash(source),
                ..EdgeRow::default()
            }],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(files, 1);
    assert_eq!(
        edges, 1,
        "the sole legacy-matched (function, Component) candidate is still uniquely resolvable"
    );
    let repointed_src: String = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT src_id FROM code_edges WHERE dst_id = 'name:helper' AND kind = 'calls'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        repointed_src, component_fn_id,
        "must resolve to the FUNCTION node, never the co-named type node"
    );
    assert_ne!(repointed_src, component_type_id);
}

#[test]
fn backfill_drift_marking_is_deterministic_across_repeated_backfills() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.rs");
    let source = "fn foo() {\n    helper();\n}\n";
    std::fs::write(&file_path, source).unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let foo_id = crate::extraction::codegraph::node_id("repo", &file_str, "function", "foo");
    seed_backfill_node(&shadow, &foo_id, &file_str, "foo");
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[
                stale_calls_edge(&foo_id, "helper", &file_str, "direct"),
                // Stamped (Codex round 8 empty-stamp invariant) so this
                // determinism test still exercises the genuine-drift
                // path, not the (also deterministic, but different)
                // empty-stamp no-op path.
                EdgeRow {
                    src_content_hash: crate::extraction::codegraph::body_hash(source),
                    ..stale_calls_edge(&foo_id, "ghost_call", &file_str, "direct")
                },
            ],
        )
        .unwrap();

    let first = backfill_wcr_witnesses(&shadow).unwrap();
    // Second backfill pass over the SAME (already-drifted) shadow: the
    // `helper` edge already matched (evidence overwritten from the
    // fresh fragment) and stays matched; `ghost_call` is already
    // `boundary = 'drifted'` and re-derives the identical classification
    // (it still won't match the fresh fragment, which hasn't changed).
    let second = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(
        first, second,
        "backfill is deterministic across repeated passes"
    );

    let (boundary, evidence): (String, String) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT boundary, evidence FROM code_edges
                     WHERE src_id = ?1 AND dst_id = 'name:ghost_call'",
                [&foo_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(boundary, "drifted");
    assert_eq!(evidence, "not_in_current_source");
}

// ─── persist_local_bindings transactional replace (X4 adversarial review, Finding 2) ───

#[test]
fn persist_local_bindings_replaces_stale_rows_on_reprocess() {
    // Version A of a file has local bindings; version B (same file)
    // renames/removes all of them. Before the fix, `persist_local_bindings`
    // only ever `INSERT OR IGNORE`d, so version A's rows would survive
    // forever alongside version B's — stale witnesses for names that no
    // longer exist in the file. After the fix, re-persisting must leave
    // ONLY version B's rows.
    let shadow = Arc::new(Storage::open_memory().unwrap());
    let mut version_a = BTreeSet::new();
    version_a.insert(("foo".to_string(), "oldName".to_string()));
    version_a.insert(("foo".to_string(), "alsoOld".to_string()));
    persist_local_bindings(&shadow, "proj", "a.ts", &version_a).unwrap();

    let after_a: Vec<(String, String)> = shadow
        .with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT scope, name FROM local_bindings WHERE project = 'proj' AND file = 'a.ts' ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        after_a,
        vec![
            ("foo".to_string(), "alsoOld".to_string()),
            ("foo".to_string(), "oldName".to_string()),
        ]
    );

    // Version B: every old name renamed away, one new name bound.
    let mut version_b = BTreeSet::new();
    version_b.insert(("foo".to_string(), "newName".to_string()));
    persist_local_bindings(&shadow, "proj", "a.ts", &version_b).unwrap();

    let after_b: Vec<(String, String)> = shadow
        .with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT scope, name FROM local_bindings WHERE project = 'proj' AND file = 'a.ts' ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        after_b,
        vec![("foo".to_string(), "newName".to_string())],
        "only version B's rows must remain — version A's stale rows must be gone"
    );

    // Version C: file now has zero local bindings at all. The DELETE
    // must still run even though the fresh set is empty.
    persist_local_bindings(&shadow, "proj", "a.ts", &BTreeSet::new()).unwrap();
    let count: i64 = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM local_bindings WHERE project = 'proj' AND file = 'a.ts'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        count, 0,
        "an empty fresh set must still clear out prior rows for this (project, file)"
    );
}

#[test]
fn persist_local_bindings_does_not_touch_other_files_rows() {
    // Transactional replace is scoped to (project, file) — a DIFFERENT
    // file's rows must survive untouched.
    let shadow = Arc::new(Storage::open_memory().unwrap());
    let mut a_bindings = BTreeSet::new();
    a_bindings.insert(("foo".to_string(), "x".to_string()));
    persist_local_bindings(&shadow, "proj", "a.ts", &a_bindings).unwrap();
    let mut b_bindings = BTreeSet::new();
    b_bindings.insert(("bar".to_string(), "y".to_string()));
    persist_local_bindings(&shadow, "proj", "b.ts", &b_bindings).unwrap();

    // Reprocess a.ts with an empty set.
    persist_local_bindings(&shadow, "proj", "a.ts", &BTreeSet::new()).unwrap();

    let b_count: i64 = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM local_bindings WHERE project = 'proj' AND file = 'b.ts'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(b_count, 1, "b.ts's row must be untouched by a.ts's replace");
}

// ─── parse_clean gates drift (X4 adversarial review, Finding 3) ───

#[test]
fn backfill_does_not_drift_module_edges_when_parse_has_errors() {
    // Degraded parse: `foo` still extracts as a real def node (condition
    // (b) holds, and (c) holds too — `foo` is the calling symbol), but
    // trailing garbage makes the OVERALL tree contain an ERROR node.
    // Before Finding 3's fix, a module-level `imports` edge here would
    // wrongly drift (the absence of a match looks identical to a
    // genuinely-removed import); after the fix, `parse_clean == false`
    // blocks it — the edge must stay unexplained, not drifted.
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("a.rs");
    std::fs::write(
        &file_path,
        "fn foo() {\n    helper();\n}\nfn bar( {{{ !!! garbage not rust\n",
    )
    .unwrap();
    let file_str = file_path.to_string_lossy().to_string();

    let shadow = Arc::new(Storage::open_memory().unwrap());
    let module_id = crate::extraction::codegraph::node_id("repo", &file_str, "module", &file_str);
    shadow
        .upsert_code_node(&NodeRow {
            id: module_id.clone(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file_str.clone(),
            lang: "rust".into(),
            kind: "module".into(),
            name: file_str.clone(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
    shadow
        .replace_code_file_edges(
            "proj",
            &file_str,
            &[EdgeRow {
                src_id: module_id.clone(),
                dst_id: "name:ghost_module".into(),
                kind: "imports".into(),
                src_file: file_str.clone(),
                resolved: 0,
                weight: 1.0,
                ..EdgeRow::default()
            }],
        )
        .unwrap();

    let (files, edges) = backfill_wcr_witnesses(&shadow).unwrap();
    assert_eq!(
        files, 1,
        "the on-disk (degraded-parse) file was re-extracted"
    );
    assert_eq!(edges, 0, "ghost_module has no match in the fresh fragment");

    let (boundary, evidence, resolved): (String, String, i64) = shadow
        .with_connection(|conn| {
            conn.query_row(
                "SELECT boundary, evidence, resolved FROM code_edges
                     WHERE src_id = ?1 AND dst_id = 'name:ghost_module'",
                [&module_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        boundary, "",
        "a degraded (ERROR-containing) parse must NEVER be trusted as drift \
             evidence, even though `foo` still extracted as a real def node"
    );
    assert_eq!(evidence, "");
    assert_eq!(resolved, 0);
}

#[test]
fn scan_repo_defs_notes_skipped_projects_beyond_cap() {
    // 13 projects (one more than WCR_SCAN_PROJECT_CAP = 12) ranked purely
    // by edge count, descending (p1 has the most edges, p13 the fewest);
    // file paths are synthetic and never exist on disk, so
    // `repo_scan::scan_all` is a real but no-op call for each — this test
    // targets the ranking + skip-note logic, not filesystem walking
    // (covered by repo_scan's own tests).
    let shadow = Arc::new(Storage::open_memory().unwrap());
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    // Zero-padded, fixed-width names (`p01`..`p13`) so no kept project's
    // name is a substring of the skipped one (`p1` would otherwise be a
    // substring of `p13`, breaking the negative assertions below).
    let project_names: Vec<String> = (1..=13).map(|i| format!("p{i:02}")).collect();
    for (i, project) in project_names.iter().enumerate() {
        let edge_count = 13 - i;
        let node_id = format!("{project}_n");
        let file = format!("/nonexistent/{project}/a.rs");
        let node = NodeRow {
            id: node_id.clone(),
            project: project.clone(),
            file: file.clone(),
            kind: "function".into(),
            name: "f".into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        };
        shadow.upsert_code_node(&node).unwrap();
        nodes.push(node);
        for j in 0..edge_count {
            edges.push(EdgeRow {
                src_id: node_id.clone(),
                dst_id: format!("name:target{j}"),
                kind: "calls".into(),
                src_file: file.clone(),
                ..EdgeRow::default()
            });
        }
    }
    let graph_snapshot = GraphSnapshot {
        nodes,
        edges,
        file_states: Vec::new(),
        evolution_projects: BTreeSet::new(),
    };
    let projects: BTreeSet<&str> = project_names.iter().map(String::as_str).collect();

    let skip_note = scan_repo_defs(&shadow, &graph_snapshot, &projects).unwrap();
    assert!(skip_note.contains("cap=12"), "{skip_note}");
    assert!(skip_note.contains("p13"), "{skip_note}");
    for kept in &project_names[..12] {
        assert!(!skip_note.contains(kept.as_str()), "{skip_note}");
    }
}

#[test]
fn scan_repo_defs_empty_note_when_within_cap() {
    let shadow = Arc::new(Storage::open_memory().unwrap());
    let graph_snapshot = GraphSnapshot {
        nodes: vec![NodeRow {
            id: "n1".into(),
            project: "solo".into(),
            file: "/nonexistent/solo/a.rs".into(),
            kind: "function".into(),
            name: "f".into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        }],
        edges: vec![EdgeRow {
            src_id: "n1".into(),
            dst_id: "name:target".into(),
            kind: "calls".into(),
            src_file: "/nonexistent/solo/a.rs".into(),
            ..EdgeRow::default()
        }],
        file_states: Vec::new(),
        evolution_projects: BTreeSet::new(),
    };
    shadow.upsert_code_node(&graph_snapshot.nodes[0]).unwrap();
    let projects: BTreeSet<&str> = ["solo"].into_iter().collect();

    let skip_note = scan_repo_defs(&shadow, &graph_snapshot, &projects).unwrap();
    assert_eq!(skip_note, "", "nothing skipped when under the cap");
}
