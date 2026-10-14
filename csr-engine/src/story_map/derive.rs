//! Receipts-only `@shifts` derivation for extracted compaction stories.
//!
//! Synthesizes up to 4 [`ShiftEntry`] items directly from a conversation's
//! `code_evolution` rows without any model call. Fails open on any query or
//! parse error by returning an empty list.

use rusqlite::{params, Connection};

use super::{parse_shift_line, story_map_disabled, ShiftEntry};
use crate::storage::Storage;

const MAX_EVO_ROWS: usize = 2000;
const MAX_ENTRIES: usize = 4;
const MAX_SCOPE_CHARS: usize = 48;
const MAX_SYM_CHARS: usize = 40;
const MAX_CLAIM_CHARS: usize = 72;

#[derive(Debug, Default)]
struct FileAccum {
    file_path: String,
    language: String,
    added: Vec<String>,
    removed: Vec<String>,
    touches: usize,
}

#[derive(Debug)]
struct RankedCandidate {
    has_symbol_change: bool,
    distinct_symbols: usize,
    touches: usize,
    scope: String,
    entry: ShiftEntry,
}

/// Derives up to 4 [`ShiftEntry`] items from this conversation's `code_evolution`
/// rows alone (zero model calls).
///
/// Returns an empty `Vec` when `story_map_disabled()`, `conv_id` is empty, or on
/// any SQLite error.
pub fn derive_receipt_shifts(storage: &Storage, conv_id: &str) -> Vec<ShiftEntry> {
    if story_map_disabled() || conv_id.is_empty() {
        return Vec::new();
    }
    storage
        .with_connection(|conn| Ok(derive_with_conn(conn, conv_id)))
        .unwrap_or_default()
}

fn derive_with_conn(conn: &Connection, conv_id: &str) -> Vec<ShiftEntry> {
    let sql = "SELECT id, file_path, language,
                      functions_added, functions_removed,
                      types_added, types_removed
               FROM code_evolution
               WHERE session_id = ?1
               ORDER BY timestamp
               LIMIT ?2";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![conv_id, MAX_EVO_ROWS as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        ))
    }) else {
        return Vec::new();
    };

    let mut files: Vec<FileAccum> = Vec::new();
    for row in rows.flatten() {
        let (_id, file_path, language, fa, fr, ta, tr) = row;
        if file_path.is_empty() {
            continue;
        }
        let acc = match files.iter_mut().find(|f| f.file_path == file_path) {
            Some(existing) => existing,
            None => {
                files.push(FileAccum {
                    file_path,
                    language: language.clone(),
                    ..Default::default()
                });
                files.last_mut().expect("just pushed")
            }
        };
        if (acc.language.is_empty() || acc.language == "text") && !language.is_empty() {
            acc.language = language.clone();
        }
        acc.touches += 1;

        if language != "text" {
            for col in [&fa, &ta] {
                for sym in parse_json_str_list(col) {
                    if is_valid_sym(&sym) && !acc.added.contains(&sym) {
                        acc.added.push(sym);
                    }
                }
            }
            for col in [&fr, &tr] {
                for sym in parse_json_str_list(col) {
                    if is_valid_sym(&sym) && !acc.removed.contains(&sym) {
                        acc.removed.push(sym);
                    }
                }
            }
        }
    }

    let mut candidates: Vec<RankedCandidate> = Vec::with_capacity(files.len());
    for file in files {
        let Some((scope, basename)) = extract_scope_and_basename(&file.file_path) else {
            continue;
        };

        let mut changed: Vec<String> = Vec::new();
        let mut added_only: Vec<String> = Vec::new();
        for sym in &file.added {
            if file.removed.contains(sym) {
                changed.push(sym.clone());
            } else {
                added_only.push(sym.clone());
            }
        }
        let removed_only: Vec<String> = file
            .removed
            .into_iter()
            .filter(|sym| !file.added.contains(sym))
            .collect();

        let distinct_symbols = changed.len() + added_only.len() + removed_only.len();
        let has_symbol_change = file.language != "text" && distinct_symbols > 0;

        let (before, after, raw_claim) = if distinct_symbols >= 2 {
            (
                "-".to_string(),
                format!("{scope}~"),
                format_multi_symbol_claim(
                    changed.len(),
                    added_only.len(),
                    removed_only.len(),
                    &basename,
                ),
            )
        } else if let Some(sym) = changed.first() {
            (
                "-".to_string(),
                format!("{scope}:{sym}~"),
                format!("Changed {sym}"),
            )
        } else if let Some(sym) = added_only.first() {
            (
                "-".to_string(),
                format!("{scope}:{sym}+"),
                format!("Added {sym}"),
            )
        } else if let Some(sym) = removed_only.first() {
            (
                format!("{scope}:{sym}-"),
                "-".to_string(),
                format!("Removed {sym}"),
            )
        } else {
            (
                "-".to_string(),
                format!("{scope}~"),
                format!("Edited {basename}"),
            )
        };

        let claim = sanitize_claim(&raw_claim);
        let line = format!("shift|{before}|{after}|{claim}");
        let Some(entry) = parse_shift_line(&line) else {
            continue;
        };

        candidates.push(RankedCandidate {
            has_symbol_change,
            distinct_symbols,
            touches: file.touches,
            scope,
            entry,
        });
    }

    candidates.sort_by(|a, b| {
        b.has_symbol_change
            .cmp(&a.has_symbol_change)
            .then_with(|| b.distinct_symbols.cmp(&a.distinct_symbols))
            .then_with(|| b.touches.cmp(&a.touches))
            .then_with(|| a.scope.cmp(&b.scope))
    });

    candidates
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|c| c.entry)
        .collect()
}

fn parse_json_str_list(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn is_scope_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '@' | '-')
}

fn is_valid_sym(s: &str) -> bool {
    let len = s.chars().count();
    (1..=MAX_SYM_CHARS).contains(&len)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.'))
}

fn extract_scope_and_basename(file_path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = file_path.split('/').filter(|p| !p.is_empty()).collect();
    let basename = (*parts.last()?).to_string();
    let base_len = basename.chars().count();
    if !(1..=MAX_SCOPE_CHARS).contains(&base_len) || !basename.chars().all(is_scope_char) {
        return None;
    }

    let two_comp = if parts.len() >= 2 {
        format!("{}/{}", parts[parts.len() - 2], basename)
    } else {
        basename.clone()
    };

    let scope = if two_comp.chars().count() <= MAX_SCOPE_CHARS {
        two_comp
    } else {
        basename.clone()
    };

    if !scope.chars().all(is_scope_char) {
        return None;
    }
    Some((scope, basename))
}

fn format_multi_symbol_claim(
    changed: usize,
    added: usize,
    removed: usize,
    basename: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (cap, low, count) in [
        ("Changed", "changed", changed),
        ("Added", "added", added),
        ("Removed", "removed", removed),
    ] {
        if count > 0 {
            let word = if parts.is_empty() { cap } else { low };
            parts.push(format!("{word} {count}"));
        }
    }
    format!("{} symbols in {basename}", parts.join(", "))
}

fn sanitize_claim(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .filter(|&c| c != '|' && (' '..='~').contains(&c))
        .take(MAX_CLAIM_CHARS)
        .collect();
    filtered.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::story_map::ground::ground_shifts;
    use crate::story_map::store::{build_story_map, load_story_map, save_story_map};
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
            r#"["c1", "c2", "r1"]"#,
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
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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

        std::env::set_var("CSR_NO_STORY_MAP", "1");
        let off = derive_receipt_shifts(&storage, "conv_1");
        std::env::remove_var("CSR_NO_STORY_MAP");

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
}
