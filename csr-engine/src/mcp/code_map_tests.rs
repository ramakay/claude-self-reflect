//! `csr_search_by_file` and `csr_code_graph` with the code map: the section is
//! appended to the old answers, stands in for an empty one, and is absent when
//! the map is off or missing.

use std::sync::Arc;

use super::tools::{code_graph_with_map, search_by_file_with_map};
use crate::codemap::testutil::{map_fixture, scan_repo_into, MapFixture};
use crate::storage::codegraph::NodeRow;
use crate::storage::Storage;

fn scanned_storage() -> (MapFixture, Arc<Storage>) {
    let m = map_fixture();
    let storage = Arc::new(Storage::open_memory().unwrap());
    storage
        .with_connection(|conn| {
            scan_repo_into(conn, &m.fx, &m.main);
            Ok(())
        })
        .unwrap();
    (m, storage)
}

async fn by_file(storage: &Arc<Storage>, arg: &str, map_off: bool) -> String {
    search_by_file_with_map(storage, arg, 10, None, map_off)
        .await
        .unwrap()
}

fn graph(
    storage: &Arc<Storage>,
    symbol: Option<&str>,
    file: Option<&str>,
    mode: &str,
    map_off: bool,
) -> String {
    let family = vec!["p".to_string()];
    code_graph_with_map(storage, symbol, file, mode, 20, "p", &family, map_off).unwrap()
}

fn node(id: &str, file: &str, name: &str) -> NodeRow {
    NodeRow {
        id: id.into(),
        project: "p".into(),
        file: file.into(),
        kind: "function".into(),
        name: name.into(),
        ..NodeRow::default()
    }
}

fn section_of(out: &str) -> &str {
    let start = out.find("<code_map").expect("a code_map section");
    let end = out.rfind("</code_map>").expect("closed") + "</code_map>".len();
    &out[start..end]
}

#[tokio::test]
async fn an_untouched_file_is_answered_by_the_map_alone() {
    let (m, storage) = scanned_storage();
    // An absolute path inside the edited worktree.
    let arg = m.diff.join("src/radio.py");
    let out = by_file(&storage, &arg.to_string_lossy(), false).await;
    assert!(
        out.contains("No conversation touched this file; the code map holds it in 3 views."),
        "{out}"
    );
    assert!(
        out.contains("same blob in main, mapfx-same; differs in mapfx-diff (dirty)"),
        "{out}"
    );
    assert!(
        out.contains("<def name='alpha_one' kind='function' lines='1-2'/>"),
        "{out}"
    );
    assert!(
        !out.contains("No conversations found analyzing"),
        "the map is the answer, not the old empty text: {out}"
    );
    assert!(section_of(&out).len() <= 1500, "{}", section_of(&out).len());
    // The linked worktree that shares the blob is named too.
    assert!(m.same.ends_with("mapfx-same"));
}

#[tokio::test]
async fn relative_and_suffix_arguments_resolve_to_the_same_file() {
    let (_m, storage) = scanned_storage();
    for arg in ["src/radio.py", "./src/radio.py", "radio.py", "src/radio.py"] {
        let out = by_file(&storage, arg, false).await;
        assert!(out.contains("<file path='src/radio.py'"), "{arg} -> {out}");
    }
}

#[tokio::test]
async fn an_ambiguous_suffix_lists_ten_paths_and_counts_the_rest() {
    let (_m, storage) = scanned_storage();
    let out = by_file(&storage, "same_name.py", false).await;
    assert!(out.contains("<code_map paths='12'>"), "{out}");
    assert!(out.matches("<file path=").count() <= 10, "{out}");
    assert!(out.contains("<more paths='"), "{out}");
    assert!(section_of(&out).len() <= 1500, "{}", section_of(&out).len());
}

#[tokio::test]
async fn a_path_the_map_does_not_hold_keeps_the_old_text() {
    let (_m, storage) = scanned_storage();
    let with_map = by_file(&storage, "nope/never.py", false).await;
    let without = by_file(&storage, "nope/never.py", true).await;
    assert_eq!(with_map, without);
    assert!(
        with_map.contains("No conversations found analyzing nope/never.py"),
        "{with_map}"
    );
}

#[tokio::test]
async fn the_section_follows_the_ledger_and_the_chunk_results() {
    let (_m, storage) = scanned_storage();
    storage
        .upsert_code_node(&node("n1", "src/radio.py", "alpha_one"))
        .unwrap();
    let out = by_file(&storage, "src/radio.py", false).await;
    let ledger_end = out.find("</file_ledger>").expect("ledger");
    let map_start = out.find("<code_map").expect("map section");
    assert!(ledger_end < map_start, "map after the ledger: {out}");
    assert!(!out.contains("No conversation touched this file"), "{out}");
    // Map off: the ledger alone, byte for byte.
    let off = by_file(&storage, "src/radio.py", true).await;
    assert!(out.starts_with(&off), "ledger text unchanged");
    assert!(!off.contains("<code_map"));

    // A chunk mentioning the file: the chunk results come first, the map after.
    let chunk = crate::import::ConversationChunk {
        id: "c1".into(),
        conversation_id: "conv-1".into(),
        project_name: "p".into(),
        timestamp: "2026-10-01T00:00:00Z".into(),
        content: "we looked at helperspy today".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    // The chunk fallback is a keyword search that strips punctuation from the
    // argument ("helpers.py" -> the word "helperspy"), so the chunk carries
    // that word; the map resolves the same argument as a path suffix.
    let out = by_file(&storage, "helpers.py", false).await;
    assert!(out.contains("conv-1"), "{out}");
    assert!(
        out.find("</file_search>").unwrap() < out.find("<code_map").unwrap(),
        "{out}"
    );
    assert!(!out.contains("No conversation touched this file"), "{out}");
}

#[tokio::test]
async fn the_kill_switch_and_missing_tables_remove_the_section() {
    let (_m, storage) = scanned_storage();
    let off = by_file(&storage, "src/radio.py", true).await;
    assert!(!off.contains("<code_map"), "{off}");
    assert!(off.contains("No conversations found analyzing"), "{off}");

    storage
        .with_connection(|conn| {
            conn.execute_batch("DROP TABLE codemap_queue;")?;
            Ok(())
        })
        .unwrap();
    let missing = by_file(&storage, "src/radio.py", false).await;
    assert_eq!(missing, off, "tables missing -> exactly the old answer");
}

#[test]
fn a_graph_symbol_gets_the_map_definitions_appended() {
    let (_m, storage) = scanned_storage();
    storage
        .upsert_code_node(&node("n1", "src/radio.py", "alpha_one"))
        .unwrap();
    let out = graph(&storage, Some("alpha_one"), None, "neighbors", false);
    assert!(out.contains("<code_graph mode='neighbors'"), "{out}");
    assert!(out.contains("No neighbors found"), "{out}");
    assert!(out.contains("<code_map definitions='1'>"), "{out}");
    assert!(
        out.contains("path='src/radio.py' lines='1-2' presence='same blob in main, mapfx-same; differs in mapfx-diff (dirty)'"),
        "{out}"
    );
    assert!(
        !out.contains("source='code_map'"),
        "the graph knew it; the map only adds: {out}"
    );
    let off = graph(&storage, Some("alpha_one"), None, "neighbors", true);
    assert!(!off.contains("<code_map") && out.starts_with(&off));
}

#[test]
fn a_symbol_the_graph_does_not_know_is_answered_by_the_map() {
    let (_m, storage) = scanned_storage();
    let out = graph(&storage, Some("delta_four"), None, "neighbors", false);
    assert!(out.contains("source='code_map'"), "{out}");
    assert!(
        out.contains("This answer comes from the code map: definitions, not call edges."),
        "{out}"
    );
    assert!(
        out.contains("path='src/radio.py'")
            && out.contains("same blob in mapfx-diff (dirty); differs in main, mapfx-same"),
        "{out}"
    );
    assert!(!out.contains("No neighbors found"), "{out}");

    for (mode, line) in [
        ("callers", "Callers are not in the code map."),
        ("callees", "Callees are not in the code map."),
    ] {
        let out = graph(&storage, Some("delta_four"), None, mode, false);
        assert!(out.contains("source='code_map'"), "{mode}: {out}");
        assert!(out.contains(line), "{mode}: {out}");
        assert!(out.contains("<code_graph mode='"), "{mode}: {out}");
    }
}

#[test]
fn only_exact_names_count_and_an_empty_map_answer_keeps_the_old_empty_result() {
    let (_m, storage) = scanned_storage();
    // `alpha` is a fragment of alpha_one, not a definition name.
    let out = graph(&storage, Some("alpha"), None, "neighbors", false);
    let off = graph(&storage, Some("alpha"), None, "neighbors", true);
    assert_eq!(out, off);
    assert!(out.contains("No neighbors found"), "{out}");
    let out = graph(&storage, Some("not_a_symbol"), None, "callers", false);
    assert!(out.contains("No callers found") && !out.contains("<code_map"));
}

#[test]
fn a_file_argument_reuses_the_path_resolution() {
    let (m, storage) = scanned_storage();
    let out = graph(
        &storage,
        None,
        Some("lib/util/helpers.py"),
        "neighbors",
        false,
    );
    assert!(out.contains("source='code_map'"), "{out}");
    assert!(out.contains("<file path='lib/util/helpers.py'"), "{out}");
    assert!(out.contains("<def name='help_me'"), "{out}");
    let abs = m.main.join("lib/util/helpers.py");
    let out = graph(
        &storage,
        None,
        Some(&abs.to_string_lossy()),
        "neighbors",
        false,
    );
    assert!(out.contains("<file path='lib/util/helpers.py'"), "{out}");
    // Off: the old empty answer.
    let off = graph(
        &storage,
        None,
        Some("lib/util/helpers.py"),
        "neighbors",
        true,
    );
    assert!(off.contains("No neighbors found") && !off.contains("<code_map"));
}

/// The public tool entry points read `CSR_NO_CODEMAP` through
/// `codemap::disabled()`; the injectable seam in the tests above is the same
/// switch.
#[test]
fn the_public_tools_honour_csr_no_codemap() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (_m, storage) = scanned_storage();
    std::env::remove_var("CSR_NO_CODEMAP");
    let on = rt
        .block_on(super::tools::search_by_file(
            &storage,
            "src/radio.py",
            10,
            None,
        ))
        .unwrap();
    assert!(on.contains("<code_map"), "{on}");
    std::env::set_var("CSR_NO_CODEMAP", "1");
    let off = rt
        .block_on(super::tools::search_by_file(
            &storage,
            "src/radio.py",
            10,
            None,
        ))
        .unwrap();
    std::env::remove_var("CSR_NO_CODEMAP");
    assert!(!off.contains("<code_map"), "{off}");
    assert!(off.contains("No conversations found analyzing"), "{off}");
}

/// Demo against a real database, read-only. Not part of the suite:
/// `CSR_SBX_DB=<db> CSR_SBX_FILES='a|b' CSR_SBX_SYMBOLS='X|Y' CSR_SBX_MODES='neighbors|callers'
/// cargo test --lib demo_against_a_real_database -- --ignored --nocapture`
/// prints what the two MCP tool functions return for each file and symbol.
#[test]
#[ignore = "needs CSR_SBX_DB; prints real tool output"]
fn demo_against_a_real_database() {
    let Ok(db) = std::env::var("CSR_SBX_DB") else {
        return;
    };
    let list = |key: &str| -> Vec<String> {
        std::env::var(key)
            .unwrap_or_default()
            .split('|')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    let storage = Arc::new(Storage::open_read_only(std::path::Path::new(&db)).unwrap());
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for file in list("CSR_SBX_FILES") {
        let out = rt
            .block_on(super::tools::search_by_file(&storage, &file, 10, None))
            .unwrap();
        println!("=== csr_search_by_file file_path={file}\n{out}\n");
    }
    let modes = list("CSR_SBX_MODES");
    let modes = if modes.is_empty() {
        vec!["neighbors".to_string()]
    } else {
        modes
    };
    for symbol in list("CSR_SBX_SYMBOLS") {
        for mode in &modes {
            let out = rt
                .block_on(super::tools::code_graph(
                    &storage,
                    Some(&symbol),
                    None,
                    mode,
                    20,
                ))
                .unwrap();
            println!("=== csr_code_graph symbol={symbol} mode={mode}\n{out}\n");
        }
    }
}

#[test]
fn the_map_section_is_off_when_the_kill_switch_is_set_for_the_graph_too() {
    let (_m, storage) = scanned_storage();
    for mode in ["neighbors", "callers", "callees"] {
        let off = graph(&storage, Some("alpha_one"), None, mode, true);
        assert!(!off.contains("<code_map"), "{mode}: {off}");
    }
}
