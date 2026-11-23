use std::collections::HashSet;

use super::*;
use crate::story_map::ground::ground_shifts;
use crate::story_map::store::{build_story_map, load_story_map, save_story_map};
use crate::story_map::test_switch;
use crate::story_map::{ShiftEntry, ShiftKind};

#[allow(clippy::too_many_arguments)]
fn seed_evo_full(
    storage: &Storage,
    id: &str,
    session_id: &str,
    file_path: &str,
    language: &str,
    ts: &str,
    fa: &str,
    fr: &str,
    ta: &str,
    tr: &str,
) {
    storage
        .with_connection(|c| {
            c.execute(
                "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed
                    ) VALUES (?1, ?2, 'csr', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![id, session_id, file_path, language, ts, fa, fr, ta, tr],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn added_symbol_makes_an_after_only_entry() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_10000001_1",
        "conv_1",
        "/repo/csr-engine/src/summarizer.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["ground_shifts"]"#,
        "[]",
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, ShiftKind::Shift);
    assert!(entries[0].before.is_none());
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "src/summarizer.rs:ground_shifts+"
    );
    assert_eq!(entries[0].claim, "Added ground_shifts");
}

#[test]
fn removed_symbol_makes_a_before_only_entry() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_20000002_1",
        "conv_1",
        "/repo/csr-engine/src/old_mod.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        "[]",
        r#"["legacy_helper"]"#,
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        ShiftEntry::render_side(&entries[0].before),
        "src/old_mod.rs:legacy_helper-"
    );
    assert!(entries[0].after.is_none());
    assert_eq!(entries[0].claim, "Removed legacy_helper");
}

#[test]
fn symbol_added_and_removed_in_one_file_is_changed() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_30000003_1",
        "conv_1",
        "/repo/csr-engine/src/summarizer.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["store_story"]"#,
        r#"["store_story"]"#,
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].before.is_none());
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "src/summarizer.rs:store_story~"
    );
    assert_eq!(entries[0].claim, "Changed store_story");
}

#[test]
fn single_symbol_keeps_the_symbol_endpoint() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_single_1",
        "conv_1",
        "/repo/csr-engine/src/story_map/derive.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["derive_receipt_shifts"]"#,
        "[]",
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].before.is_none());
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "story_map/derive.rs:derive_receipt_shifts+"
    );
    assert_eq!(entries[0].claim, "Added derive_receipt_shifts");
}

#[test]
fn many_symbols_become_one_file_level_entry_with_counts() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_many_1",
        "conv_1",
        "/repo/csr-engine/src/codex_rollout.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["c1", "c2", "a1", "a2", "a3"]"#,
        r#"["c1", "c2"]"#,
        "[]",
        "[]",
    );
    // The removal is a separate edit: one edit removing r1 while adding a1..a3 would be a swap.
    seed_evo_full(
        &storage,
        "evo_many_1b",
        "conv_1",
        "/repo/csr-engine/src/codex_rollout.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        "[]",
        r#"["r1"]"#,
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_many_2",
        "conv_1",
        "/repo/csr-engine/src/foo.rs",
        "rust",
        "2026-10-14T00:00:02Z",
        r#"["f1", "f2", "f3", "f4", "f5"]"#,
        "[]",
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_many_3",
        "conv_1",
        "/repo/csr-engine/src/bar.rs",
        "rust",
        "2026-10-14T00:00:03Z",
        "[]",
        r#"["b1", "b2"]"#,
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 3);

    assert!(entries[0].before.is_none());
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "src/codex_rollout.rs~"
    );
    assert_eq!(
        entries[0].claim,
        "Changed 2, added 3, removed 1 symbols in codex_rollout.rs"
    );

    assert!(entries[1].before.is_none());
    assert_eq!(ShiftEntry::render_side(&entries[1].after), "src/foo.rs~");
    assert_eq!(entries[1].claim, "Added 5 symbols in foo.rs");

    assert!(entries[2].before.is_none());
    assert_eq!(ShiftEntry::render_side(&entries[2].after), "src/bar.rs~");
    assert_eq!(entries[2].claim, "Removed 2 symbols in bar.rs");
}

#[test]
fn file_touch_row_makes_an_edited_entry() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_40000004_1",
        "conv_1",
        "/repo/docs/architecture.md",
        "text",
        "2026-10-14T00:00:01Z",
        "[]",
        "[]",
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].before.is_none());
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "docs/architecture.md~"
    );
    assert_eq!(entries[0].claim, "Edited architecture.md");
}

#[test]
fn more_than_four_files_keep_the_top_four_deterministically() {
    let storage = Storage::open_memory().unwrap();
    // 1. Touch-only file (lowest rank: no symbol changes, 3 touches)
    for i in 0..3 {
        seed_evo_full(
            &storage,
            &format!("evo_t_{i}"),
            "conv_1",
            "/repo/docs/readme.md",
            "text",
            &format!("2026-10-14T00:00:0{i}Z"),
            "[]",
            "[]",
            "[]",
            "[]",
        );
    }
    // 2. Code file with 3 symbols (rank #1)
    seed_evo_full(
        &storage,
        "evo_c1",
        "conv_1",
        "/repo/src/alpha.rs",
        "rust",
        "2026-10-14T00:00:10Z",
        r#"["f1", "f2", "f3"]"#,
        "[]",
        "[]",
        "[]",
    );
    // 3. Code file with 1 symbol and 2 touches (rank #2)
    seed_evo_full(
        &storage,
        "evo_c2_a",
        "conv_1",
        "/repo/src/zeta.rs",
        "rust",
        "2026-10-14T00:00:11Z",
        r#"["z1"]"#,
        "[]",
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_c2_b",
        "conv_1",
        "/repo/src/zeta.rs",
        "rust",
        "2026-10-14T00:00:12Z",
        r#"["z1"]"#,
        "[]",
        "[]",
        "[]",
    );
    // 4 & 5. Two code files tied on (1 symbol, 1 touch), ordered by scope ascending:
    // src/beta.rs (rank #3) before src/gamma.rs (rank #4).
    seed_evo_full(
        &storage,
        "evo_c4",
        "conv_1",
        "/repo/src/gamma.rs",
        "rust",
        "2026-10-14T00:00:13Z",
        r#"["g1"]"#,
        "[]",
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_c3",
        "conv_1",
        "/repo/src/beta.rs",
        "rust",
        "2026-10-14T00:00:14Z",
        r#"["b1"]"#,
        "[]",
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 4);
    let scopes: Vec<String> = entries
        .iter()
        .map(|e| e.after.as_ref().unwrap().from.scope.clone())
        .collect();
    assert_eq!(
        scopes,
        vec!["src/alpha.rs", "src/zeta.rs", "src/beta.rs", "src/gamma.rs"]
    );
}

#[test]
fn other_conversations_rows_are_ignored() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_other",
        "conv_other",
        "/repo/src/secret.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["leaked"]"#,
        "[]",
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_mine",
        "conv_1",
        "/repo/src/mine.rs",
        "rust",
        "2026-10-14T00:00:02Z",
        r#"["kept"]"#,
        "[]",
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "src/mine.rs:kept+"
    );
}

#[test]
fn invalid_paths_and_symbols_are_skipped() {
    let storage = Storage::open_memory().unwrap();
    // Path with space -> invalid scope chars -> file skipped completely.
    seed_evo_full(
        &storage,
        "evo_bad_path",
        "conv_1",
        "/repo/my folder/bad file.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["good_sym"]"#,
        "[]",
        "[]",
        "[]",
    );
    // Basename > 48 chars -> skipped.
    let long_base = format!("/repo/src/{}.rs", "a".repeat(50));
    seed_evo_full(
        &storage,
        "evo_long_base",
        "conv_1",
        &long_base,
        "rust",
        "2026-10-14T00:00:02Z",
        r#"["good_sym"]"#,
        "[]",
        "[]",
        "[]",
    );
    // Valid file whose 2-component path > 48 chars falls back to basename,
    // and invalid symbols are ignored while valid symbol is kept.
    let long_dir = format!("/repo/{}/ok_file.rs", "d".repeat(45));
    let overlong_sym = "s".repeat(41);
    let fa_json = serde_json::to_string(&vec![
        "bad sym with spaces".to_string(),
        overlong_sym,
        "valid_fn".to_string(),
    ])
    .unwrap();
    seed_evo_full(
        &storage,
        "evo_good_fallback",
        "conv_1",
        &long_dir,
        "rust",
        "2026-10-14T00:00:03Z",
        &fa_json,
        "[]",
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "ok_file.rs:valid_fn+"
    );
    assert_eq!(entries[0].claim, "Added valid_fn");
}

#[test]
fn claim_is_ascii_and_at_most_72_chars() {
    let storage = Storage::open_memory().unwrap();
    // 40-char symbol + 1000 other symbols: tests length cap; but even simpler,
    // test with a 40-char symbol and many other symbols so "Changed <40> and 12 more" is checked,
    // and test sanitize_claim directly for >72 chars.
    let sym40 = "a".repeat(40);
    let mut syms = vec![sym40.clone()];
    for i in 0..20 {
        syms.push(format!("extra_symbol_{i}"));
    }
    let fa = serde_json::to_string(&syms).unwrap();
    seed_evo_full(
        &storage,
        "evo_claim",
        "conv_1",
        "/repo/src/lib.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        &fa,
        "[]",
        "[]",
        "[]",
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].claim.len() <= 72);
    assert!(entries[0]
        .claim
        .bytes()
        .all(|b| (0x20..=0x7E).contains(&b) && b != b'|'));

    let huge_raw = format!("Added {} and 999999999999999999999999999999 more", sym40);
    let capped = sanitize_claim(&huge_raw);
    assert!(capped.len() <= 72);
    assert!(!capped.ends_with(' '));
    let line = format!("shift|-|src/lib.rs:{sym40}+|{capped}");
    let parsed = parse_shift_line(&line).expect("must parse");
    assert!(parsed.claim.len() <= 72);
}

#[test]
fn kill_switch_returns_nothing() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_kill",
        "conv_1",
        "/repo/src/lib.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["foo"]"#,
        "[]",
        "[]",
        "[]",
    );

    let switch = test_switch::on();
    let off = derive_receipt_shifts(&storage, "conv_1");
    drop(switch);

    assert!(off.is_empty());
    let on = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(on.len(), 1);
}

#[test]
fn empty_conversation_returns_nothing() {
    let storage = Storage::open_memory().unwrap();
    assert!(derive_receipt_shifts(&storage, "").is_empty());
    assert!(derive_receipt_shifts(&storage, "conv_with_no_rows").is_empty());
}

#[test]
fn derived_entries_ground_against_their_own_rows() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_full(
        &storage,
        "evo_9f12a0c1_1",
        "conv_e2e",
        "/repo/csr-engine/src/summarizer.rs",
        "rust",
        "2026-10-14T00:00:01Z",
        r#"["store_story"]"#,
        r#"["store_story"]"#,
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_88776655_2",
        "conv_e2e",
        "/repo/csr-engine/src/story_map/derive.rs",
        "rust",
        "2026-10-14T00:00:02Z",
        r#"["derive_receipt_shifts"]"#,
        "[]",
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_deadbeef_3",
        "conv_e2e",
        "/repo/csr-engine/src/legacy.rs",
        "rust",
        "2026-10-14T00:00:03Z",
        "[]",
        r#"["old_fn"]"#,
        "[]",
        "[]",
    );
    seed_evo_full(
        &storage,
        "evo_1122aabb_4",
        "conv_e2e",
        "/repo/docs/notes.md",
        "text",
        "2026-10-14T00:00:04Z",
        "[]",
        "[]",
        "[]",
        "[]",
    );

    let derived = derive_receipt_shifts(&storage, "conv_e2e");
    assert_eq!(derived.len(), 4);

    let grounded = ground_shifts(&storage, "csr", "conv_e2e", &derived);
    assert_eq!(grounded.len(), 4);
    for g in &grounded {
        assert!(g.grounded, "entry {} must be grounded", g.claim);
        assert!(
            g.receipts.iter().any(|r| r.starts_with("evo:")),
            "entry {} must carry an evo: receipt",
            g.claim
        );
    }

    let map = build_story_map(
        None,
        "csr",
        "conv_e2e",
        1,
        1760400000,
        "receipts",
        25,
        &HashSet::new(),
        grounded,
    );
    save_story_map(&storage, &map).unwrap();

    let loaded = load_story_map(&storage, "conv_e2e").expect("saved story_map");
    assert_eq!(loaded.current.by, "receipts");
    assert_eq!(loaded.current.entries.len(), 4);
    assert!(loaded.current.entries.iter().all(|e| e.grounded));
}

const NONE: (&str, &str) = ("[]", "[]");

#[allow(clippy::type_complexity)]
fn seed_evo_swap(
    storage: &Storage,
    id: &str,
    file_path: &str,
    funcs: (&str, &str),
    types: (&str, &str),
    imports: (&str, &str),
) {
    storage
        .with_connection(|c| {
            c.execute(
                "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed,
                        imports_added, imports_removed
                    ) VALUES (?1, 'conv_1', 'csr', ?2, 'rust', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    file_path,
                    "2026-10-14T00:00:01Z",
                    funcs.0,
                    funcs.1,
                    types.0,
                    types.1,
                    imports.0,
                    imports.1
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn function_swap_in_one_edit_gets_a_before_and_an_after() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_aa000001_1",
        "/repo/csr-engine/src/storage/queries.rs",
        (
            r#"["conversation_scope_table_exists","get_conversation_scope","upsert_conversation_scope"]"#,
            r#"["insert_conversation_scope"]"#,
        ),
        NONE,
        NONE,
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    let swap = entries
        .iter()
        .find(|e| e.before.is_some())
        .expect("a swap entry with a before side");
    assert_eq!(
        ShiftEntry::render_side(&swap.before),
        "storage/queries.rs:insert_conversation_scope-"
    );
    let after = ShiftEntry::render_side(&swap.after);
    assert_eq!(after, "storage/queries.rs:upsert_conversation_scope+");
    assert_eq!(
        swap.claim,
        "Replaced insert_conversation_scope with 3 symbols in queries.rs"
    );
    assert!(swap.claim.len() <= 72);
}

#[test]
fn short_names_keep_the_and_n_more_clause_and_the_file() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_aa000009_1",
        "/repo/csr-engine/src/storage/queries.rs",
        (r#"["a_scope","b_scope","c_scope"]"#, r#"["old_scope"]"#),
        NONE,
        NONE,
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(
        entries[0].claim,
        "Replaced old_scope with a_scope and 2 more in queries.rs"
    );
}

#[test]
fn swap_ranks_ahead_of_the_plain_entry_for_the_same_file() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_aa000002_1",
        "/repo/csr-engine/src/storage/queries.rs",
        (r#"["get_scope"]"#, r#"["insert_scope"]"#),
        NONE,
        NONE,
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].claim,
        "Replaced insert_scope with get_scope in queries.rs"
    );
    assert_eq!(
        ShiftEntry::render_side(&entries[0].before),
        "storage/queries.rs:insert_scope-"
    );
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "storage/queries.rs:get_scope+"
    );
    assert!(entries[1].before.is_none(), "today's entry comes second");
    assert_eq!(entries[1].claim, "Added 1, removed 1 symbols in queries.rs");
}

#[test]
fn import_swap_in_the_same_module_names_what_it_adds() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_bb000001_1",
        "/repo/csr-engine/examples/hook_recall_replay.rs",
        NONE,
        NONE,
        (
            r#"["use rusqlite::{Connection, OpenFlags, OptionalExtension};"]"#,
            r#"["use rusqlite::{Connection, OpenFlags};"]"#,
        ),
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(
        entries[0].claim,
        "Swapped the rusqlite import for one that adds OptionalExtension"
    );
    assert_eq!(
        ShiftEntry::render_side(&entries[0].before),
        "examples/hook_recall_replay.rs:rusqlite~"
    );
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "examples/hook_recall_replay.rs:OptionalExtension+"
    );
}

#[test]
fn import_swap_across_modules_names_both() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_bb000002_1",
        "/repo/csr-engine/src/index.rs",
        NONE,
        NONE,
        (
            r#"["use indexmap::IndexSet;"]"#,
            r#"["use std::collections::HashSet;"]"#,
        ),
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(
        entries[0].claim,
        "Swapped the std::collections import for indexmap"
    );
    assert_eq!(
        ShiftEntry::render_side(&entries[0].before),
        "src/index.rs:std.collections-"
    );
    assert_eq!(
        ShiftEntry::render_side(&entries[0].after),
        "src/index.rs:indexmap+"
    );
}

#[test]
fn multi_line_js_imports_and_dropped_items_read_in_plain_words() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_bb000003_1",
        "/repo/installer/postinstall.js",
        NONE,
        NONE,
        (
            "[\"import {\\n  existsSync,\\n  renameSync\\n} from 'fs';\"]",
            "[\"import {\\n  existsSync\\n} from 'fs';\"]",
        ),
    );
    seed_evo_swap(
        &storage,
        "evo_bb000004_1",
        "/repo/installer/cli.js",
        NONE,
        NONE,
        (
            "[\"import { join } from 'path';\"]",
            "[\"import { join, resolve } from 'path';\"]",
        ),
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    let claims: Vec<&str> = entries.iter().map(|e| e.claim.as_str()).collect();
    assert!(
        claims.contains(&"Swapped the fs import for one that adds renameSync"),
        "{claims:?}"
    );
    assert!(
        claims.contains(&"Swapped the path import for one that drops resolve"),
        "{claims:?}"
    );
    let dropped = entries
        .iter()
        .find(|e| e.claim.ends_with("drops resolve"))
        .unwrap();
    assert_eq!(
        ShiftEntry::render_side(&dropped.before),
        "installer/cli.js:resolve-"
    );
    assert_eq!(
        ShiftEntry::render_side(&dropped.after),
        "installer/cli.js:path~"
    );
}

#[test]
fn long_import_lists_fall_back_to_counts_instead_of_a_cut_word() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_bb000005_1",
        "/repo/installer/lib.js",
        NONE,
        NONE,
        (
            "[\"import { accessSync, closeSync, constants, fchmodSync, fstatSync, writeSync } from 'fs';\"]",
            "[\"import { accessSync, chmodSync, closeSync, constants, copyFileSync } from 'fs';\"]",
        ),
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(
        entries[0].claim,
        "Swapped the fs import for one that adds 3 names and drops 2 names"
    );
    assert!(entries[0].claim.len() <= 72);
}

#[test]
fn nested_rust_use_groups_compare_by_their_top_level_items() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_bb000006_1",
        "/repo/csr-engine/src/io.rs",
        NONE,
        NONE,
        (
            r#"["use std::{io::{self, Read}, fs, path::Path};"]"#,
            r#"["use std::{io::{self, Read}, fs};"]"#,
        ),
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(
        entries[0].claim,
        "Swapped the std import for one that adds path::Path"
    );
}

#[test]
fn a_fourth_import_tweak_does_not_crowd_out_a_function_swap() {
    let storage = Storage::open_memory().unwrap();
    for (i, f) in ["a", "b", "c"].iter().enumerate() {
        seed_evo_swap(
            &storage,
            &format!("evo_bb00001{i}_1"),
            &format!("/repo/csr-engine/src/{f}_use.rs"),
            NONE,
            NONE,
            (r#"["use x::{One, Two};"]"#, r#"["use x::{One};"]"#),
        );
    }
    seed_evo_swap(
        &storage,
        "evo_bb000019_1",
        "/repo/csr-engine/src/real.rs",
        (r#"["new_fn"]"#, r#"["old_fn"]"#),
        NONE,
        NONE,
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    let claims: Vec<&str> = entries.iter().map(|e| e.claim.as_str()).collect();
    assert_eq!(entries.len(), 4);
    assert!(claims[0].starts_with("Swapped the x import"), "{claims:?}");
    assert!(claims[1].starts_with("Swapped the x import"), "{claims:?}");
    assert_eq!(claims[2], "Replaced old_fn with new_fn in real.rs");
    assert!(claims[3].starts_with("Swapped the x import"), "{claims:?}");
}

#[test]
fn type_swap_reads_as_a_replacement() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_cc000001_1",
        "/repo/csr-engine/src/storage/mod.rs",
        NONE,
        (r#"["ConversationScopeRow"]"#, r#"["ScopeRow"]"#),
        NONE,
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(
        entries[0].claim,
        "Replaced ScopeRow with ConversationScopeRow in mod.rs"
    );
    assert_eq!(
        ShiftEntry::render_side(&entries[0].before),
        "storage/mod.rs:ScopeRow-"
    );
}

#[test]
fn rows_without_a_same_edit_swap_give_todays_entries() {
    let storage = Storage::open_memory().unwrap();
    // One edit removes, a later edit adds: two edits, not a swap.
    seed_evo_swap(
        &storage,
        "evo_dd000001_1",
        "/repo/csr-engine/src/alpha.rs",
        ("[]", r#"["old_fn"]"#),
        NONE,
        NONE,
    );
    seed_evo_swap(
        &storage,
        "evo_dd000001_22",
        "/repo/csr-engine/src/alpha.rs",
        (r#"["new_fn"]"#, "[]"),
        NONE,
        NONE,
    );
    // Imports only added: not a swap.
    seed_evo_swap(
        &storage,
        "evo_dd000002_1",
        "/repo/csr-engine/src/beta.rs",
        NONE,
        NONE,
        (r#"["use std::fmt;"]"#, "[]"),
    );
    // A symbol edited in place is a change, not a swap.
    seed_evo_swap(
        &storage,
        "evo_dd000003_1",
        "/repo/csr-engine/src/gamma.rs",
        (r#"["same_fn"]"#, r#"["same_fn"]"#),
        NONE,
        NONE,
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    let claims: Vec<&str> = entries.iter().map(|e| e.claim.as_str()).collect();
    assert_eq!(
        claims,
        vec![
            "Added 1, removed 1 symbols in alpha.rs",
            "Changed same_fn",
            "Edited beta.rs"
        ]
    );
    assert!(entries.iter().all(|e| e.before.is_none()));
}

#[test]
fn architecture_swaps_rank_first_then_function_swaps_then_the_rest() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_ee000001_1",
        "/repo/csr-engine/src/a_import.rs",
        NONE,
        NONE,
        (r#"["use b::Two;"]"#, r#"["use a::One;"]"#),
    );
    seed_evo_swap(
        &storage,
        "evo_ee000002_1",
        "/repo/csr-engine/src/b_type.rs",
        NONE,
        (r#"["NewType"]"#, r#"["OldType"]"#),
        NONE,
    );
    seed_evo_swap(
        &storage,
        "evo_ee000003_1",
        "/repo/csr-engine/src/c_fn.rs",
        (r#"["new_fn"]"#, r#"["old_fn"]"#),
        NONE,
        NONE,
    );
    seed_evo_swap(
        &storage,
        "evo_ee000004_1",
        "/repo/csr-engine/src/d_plain.rs",
        (r#"["a","b","c","d"]"#, "[]"),
        NONE,
        NONE,
    );

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].claim, "Swapped the a import for b");
    assert_eq!(
        entries[1].claim,
        "Replaced OldType with NewType in b_type.rs"
    );
    assert_eq!(entries[2].claim, "Replaced old_fn with new_fn in c_fn.rs");
    assert!(
        entries[3].before.is_none(),
        "a plain entry fills the last slot"
    );
}

#[test]
fn test_only_swaps_rank_last_and_lose_the_cap() {
    let storage = Storage::open_memory().unwrap();
    // Names starting test_, and a file under tests/, both count as test-only.
    seed_evo_swap(
        &storage,
        "evo_ff000001_1",
        "/repo/csr-engine/src/search/mod.rs",
        (r#"["test_new_drift"]"#, r#"["test_old_drift"]"#),
        NONE,
        NONE,
    );
    seed_evo_swap(
        &storage,
        "evo_ff000002_1",
        "/repo/csr-engine/tests/integration.rs",
        (r#"["extract_rank_scores"]"#, r#"["regex_lite_extract"]"#),
        NONE,
        NONE,
    );
    seed_evo_swap(
        &storage,
        "evo_ff000003_1",
        "/repo/csr-engine/src/plain.rs",
        (r#"["p1"]"#, "[]"),
        NONE,
        NONE,
    );
    let entries = derive_receipt_shifts(&storage, "conv_1");
    let swap_positions: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.claim.starts_with("Replaced "))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(entries.len(), 4);
    assert_eq!(swap_positions, vec![3], "a test-only swap is last");
    assert_eq!(
        entries[3].claim,
        "Replaced test_old_drift with test_new_drift in mod.rs"
    );

    // Four plain entries push the test-only swaps out of the cap entirely.
    for (i, f) in ["w", "x", "y", "z"].iter().enumerate() {
        seed_evo_swap(
            &storage,
            &format!("evo_ff00001{i}_1"),
            &format!("/repo/csr-engine/src/{f}.rs"),
            (r#"["a1","a2","a3","a4","a5"]"#, "[]"),
            NONE,
            NONE,
        );
    }
    let capped = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(capped.len(), 4);
    assert!(capped.iter().all(|e| !e.claim.starts_with("Replaced ")));
}

#[test]
fn swap_entries_ground_against_their_own_rows() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_99000001_1",
        "/repo/csr-engine/src/storage/queries.rs",
        (r#"["get_scope"]"#, r#"["insert_scope"]"#),
        NONE,
        NONE,
    );
    seed_evo_swap(
        &storage,
        "evo_99000002_1",
        "/repo/csr-engine/src/hooks/recall.rs",
        NONE,
        NONE,
        (
            r#"["use rusqlite::{Connection, OptionalExtension};"]"#,
            r#"["use rusqlite::Connection;"]"#,
        ),
    );
    let derived = derive_receipt_shifts(&storage, "conv_1");
    assert_eq!(derived.len(), 4);
    let grounded = ground_shifts(&storage, "csr", "conv_1", &derived);
    assert_eq!(grounded.len(), 4, "no swap entry is contradicted away");
    for g in &grounded {
        assert!(g.grounded, "{} must be grounded", g.claim);
        assert!(g.receipts.iter().any(|r| r.starts_with("evo:")));
    }
    let swap = grounded
        .iter()
        .find(|g| g.claim.starts_with("Replaced"))
        .unwrap();
    assert!(!swap.before.is_empty() && !swap.after.is_empty());
}

#[test]
fn kill_switch_also_silences_swaps() {
    let storage = Storage::open_memory().unwrap();
    seed_evo_swap(
        &storage,
        "evo_88000001_1",
        "/repo/csr-engine/src/storage/queries.rs",
        (r#"["get_scope"]"#, r#"["insert_scope"]"#),
        NONE,
        NONE,
    );
    let switch = test_switch::on();
    let off = derive_receipt_shifts(&storage, "conv_1");
    drop(switch);
    assert!(off.is_empty());
    assert!(derive_receipt_shifts(&storage, "conv_1")
        .iter()
        .any(|e| e.before.is_some()));
}

#[test]
fn missing_table_fails_open() {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|c| {
            c.execute("DROP TABLE code_evolution", [])?;
            Ok(())
        })
        .unwrap();

    let entries = derive_receipt_shifts(&storage, "conv_1");
    assert!(entries.is_empty());
}
