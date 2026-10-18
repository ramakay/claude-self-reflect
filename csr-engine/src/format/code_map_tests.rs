//! The `<code_map>` section formatters: shape, escaping and the byte budget.

use super::*;
use crate::codemap::{CodemapFile, CodemapHit, FileDef, ViewPresence};

fn presence(view: &str, blob: &str, same: bool, dirty: i64) -> ViewPresence {
    ViewPresence {
        view: view.into(),
        worktree_path: format!("/w/{view}"),
        branch: Some("b".into()),
        blob_oid: blob.into(),
        same,
        dirty,
    }
}

fn file(path: &str, defs: usize, views: usize) -> CodemapFile {
    CodemapFile {
        repo: "/Users/x/proj/.git".into(),
        path: path.into(),
        primary_blob: "aaaaaaa".into(),
        presence: (0..views)
            .map(|i| {
                presence(
                    &format!("worktree-{i}"),
                    "aaaaaaa",
                    i % 3 != 2,
                    (i % 3 == 2) as i64,
                )
            })
            .map(|mut p| {
                if !p.same {
                    p.blob_oid = "bbbbbbb".into();
                }
                p
            })
            .collect(),
        defs: (0..defs)
            .map(|i| FileDef {
                name: format!("definition_number_{i}"),
                kind: "function".into(),
                span_start: i as i64 * 10,
                span_end: i as i64 * 10 + 8,
            })
            .collect(),
        defs_total: defs,
    }
}

#[test]
fn file_section_shows_presence_and_one_based_lines() {
    let out = format_code_map_files(&[file("src/a.rs", 2, 2)], 1);
    assert!(out.starts_with("<code_map paths='1'>"), "{out}");
    assert!(
        out.contains("<file path='src/a.rs' repo='proj' views='2'>"),
        "{out}"
    );
    assert!(
        out.contains("<presence>same blob in worktree-0, worktree-1</presence>"),
        "{out}"
    );
    assert!(
        out.contains("<def name='definition_number_0' kind='function' lines='1-9'/>"),
        "{out}"
    );
    assert!(out.ends_with("</code_map>"), "{out}");
}

#[test]
fn file_section_is_empty_without_files() {
    assert_eq!(format_code_map_files(&[], 0), "");
    assert_eq!(format_code_map_definitions(&[]), "");
}

#[test]
fn file_section_escapes_markup_in_paths_and_names() {
    let mut f = file("src/a<b>&'.rs", 1, 1);
    f.defs[0].name = "x<y>".into();
    let out = format_code_map_files(&[f], 1);
    assert!(!out.contains("a<b>"), "{out}");
    assert!(out.contains("a&lt;b&gt;&amp;"), "{out}");
    assert!(out.contains("x&lt;y&gt;"), "{out}");
}

#[test]
fn file_section_stays_inside_the_byte_budget_and_says_what_it_dropped() {
    // One file with far more definitions than fit.
    let big = format_code_map_files(&[file("src/big.rs", 40, 9)], 1);
    assert!(
        big.len() <= CODE_MAP_SECTION_BUDGET,
        "{} bytes\n{big}",
        big.len()
    );
    assert!(big.contains("<more defs='"), "{big}");
    assert!(big.ends_with("</code_map>"), "{big}");

    // Ten long-named files: defs shrink first, then whole files drop.
    let files: Vec<CodemapFile> = (0..10)
        .map(|i| {
            file(
                &format!("some/rather/deeply/nested/directory/layout/module_{i}.rs"),
                40,
                9,
            )
        })
        .collect();
    let many = format_code_map_files(&files, 14);
    assert!(
        many.len() <= CODE_MAP_SECTION_BUDGET,
        "{} bytes\n{many}",
        many.len()
    );
    assert!(many.contains("<more paths='"), "{many}");
    assert!(many.contains("paths='14'"), "{many}");

    // A path longer than the budget degrades to a bare marker, still bounded.
    let absurd = format_code_map_files(&[file(&"d/".repeat(900), 1, 1)], 1);
    assert!(absurd.len() <= CODE_MAP_SECTION_BUDGET, "{}", absurd.len());
    assert!(absurd.contains("truncated='true'"), "{absurd}");
}

fn hit(path: &str, name: &str) -> CodemapHit {
    CodemapHit {
        repo: "/Users/x/proj/.git".into(),
        path: path.into(),
        name: name.into(),
        kind: "function".into(),
        fqname: None,
        signature: String::new(),
        span_start: 4,
        span_end: 19,
        primary_blob: "aaaaaaa".into(),
        same_in: vec!["main".into()],
        differs_in: vec!["wt (dirty)".into()],
        presence: vec![
            presence("main", "aaaaaaa", true, 0),
            presence("wt", "bbbbbbb", false, 1),
        ],
    }
}

#[test]
fn definition_section_lists_each_definition_with_its_presence() {
    let out = format_code_map_definitions(&[hit("src/x.rs", "run")]);
    assert!(out.starts_with("<code_map definitions='1'>"), "{out}");
    assert!(
        out.contains(
            "<definition name='run' kind='function' repo='proj' path='src/x.rs' lines='5-20' presence='same blob in main; differs in wt (dirty)'/>"
        ),
        "{out}"
    );
}

#[test]
fn definition_section_drops_whole_definitions_to_fit_the_budget() {
    let hits: Vec<CodemapHit> = (0..10)
        .map(|i| {
            hit(
                &format!("some/rather/deeply/nested/directory/layout/module_{i}.rs"),
                "run",
            )
        })
        .collect();
    let out = format_code_map_definitions(&hits);
    assert!(out.len() <= CODE_MAP_SECTION_BUDGET, "{} bytes", out.len());
    assert!(out.contains("<more definitions='"), "{out}");
}

#[test]
fn map_only_and_map_answer_wrappers_say_where_the_answer_comes_from() {
    let section = "<code_map paths='1'></code_map>";
    let only = format_file_map_only("src/a.rs", false, 3, section);
    assert!(
        only.contains("No conversation touched this file; the code map holds it in 3 views."),
        "{only}"
    );
    assert!(only.contains(section) && only.ends_with("</file_search>"));
    assert!(format_file_map_only("a", false, 1, section).contains("in 1 view."));

    let callers = format_code_graph_from_map("callers", "run", section);
    assert!(callers.contains("source='code_map'"), "{callers}");
    assert!(callers.contains("definitions, not call edges"), "{callers}");
    assert!(
        callers.contains("Callers are not in the code map."),
        "{callers}"
    );
    assert!(format_code_graph_from_map("callees", "run", section)
        .contains("Callees are not in the code map."));
    let neighbors = format_code_graph_from_map("neighbors", "run", section);
    assert!(
        !neighbors.contains("are not in the code map"),
        "{neighbors}"
    );
    assert!(neighbors.ends_with("</code_graph>"));
}
