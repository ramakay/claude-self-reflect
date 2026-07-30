//! Code-property-graph fragment extractor (v9.4).
//!
//! Parses a single source file (one language per fragment) and emits graph
//! nodes + edges with conversation provenance. Reuses the `ast_analysis` kind
//! tables so the symbol vocabulary matches the rest of CSR.
//!
//! Edges emitted:
//! - `defines`  module node -> each symbol it declares
//! - `calls`    enclosing def -> callee (placeholder `name:<callee>` until resolved)
//! - `imports`  module node -> imported name (placeholder)
//!
//! All tree-sitter work is wrapped in `catch_unwind` (malformed input can panic).

use std::collections::{BTreeMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::LazyLock;

use ast_grep_core::matcher::KindMatcher;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_language::SupportLang;
use sha2::{Digest, Sha256};

use super::ast_analysis::{extract_name_from_def, func_kinds, import_kinds, type_kinds};
use crate::storage::codegraph::{EdgeRow, NodeRow};

/// A fragment of the code graph extracted from one file.
#[derive(Debug, Clone, Default)]
pub struct GraphFragment {
    pub nodes: Vec<NodeRow>,
    pub edges: Vec<EdgeRow>,
}

/// Stable node id: sha256(repo|file|kind|name), truncated to 40 hex chars.
pub fn node_id(repo: &str, file: &str, kind: &str, name: &str) -> String {
    let digest = Sha256::digest(format!("{repo}|{file}|{kind}|{name}").as_bytes());
    let mut s = String::with_capacity(40);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
        if s.len() >= 40 {
            break;
        }
    }
    s.truncate(40);
    s
}

/// 16-hex-char sha256 of a node's source text (change detection without diff).
fn body_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut s = String::with_capacity(16);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
        if s.len() >= 16 {
            break;
        }
    }
    s.truncate(16);
    s
}

/// Call-expression kind(s) per language.
fn call_kinds(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::Rust => &["call_expression"],
        SupportLang::Python => &["call"],
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            &["call_expression"]
        }
        SupportLang::Go => &["call_expression"],
        _ => &[],
    }
}

/// First identifier-like child of a node (a definition's name).
fn child_name<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> Option<String> {
    for child in node.children() {
        match child.kind().as_ref() {
            "identifier" | "name" | "type_identifier" | "property_identifier" => {
                return Some(child.text().to_string());
            }
            _ => {}
        }
    }
    None
}

/// Callee names that are language built-ins or ubiquitous trait / iterator /
/// container methods. They are never useful "who calls X" provenance targets,
/// and their massive name-collision rate pollutes resolution (every `.collect()`
/// repointing to one `collect` def, etc.). We never emit `calls`/`imports`
/// placeholder edges to them. A real user symbol that happens to share one of
/// these names still gets its *def* node — only the noisy call edge is dropped.
static NOISE_CALLEES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // constructors / conversions
        "new",
        "from",
        "into",
        "default",
        "clone",
        "to_string",
        "to_owned",
        "to_vec",
        "as_ref",
        "as_mut",
        "as_str",
        "as_bytes",
        "borrow",
        "borrow_mut",
        // option / result
        "unwrap",
        "unwrap_or",
        "unwrap_or_else",
        "unwrap_or_default",
        "expect",
        "ok",
        "err",
        "and_then",
        "or_else",
        "ok_or",
        "ok_or_else",
        "is_some",
        "is_none",
        "is_ok",
        "is_err",
        // iterator / collection adapters
        "iter",
        "into_iter",
        "iter_mut",
        "collect",
        "map",
        "filter",
        "filter_map",
        "for_each",
        "fold",
        "sum",
        "count",
        "min",
        "max",
        "next",
        "any",
        "all",
        "find",
        "position",
        "enumerate",
        "zip",
        "chain",
        "rev",
        "take",
        "skip",
        "flat_map",
        "flatten",
        "cloned",
        "copied",
        "sort",
        "sort_by",
        // common container ops
        "push",
        "pop",
        "get",
        "get_mut",
        "insert",
        "remove",
        "contains",
        "contains_key",
        "len",
        "is_empty",
        "entry",
        "or_default",
        "or_insert",
        "values",
        "keys",
        "extend",
        "drain",
        "clear",
        "with_capacity",
        // string ops
        "trim",
        "split",
        "splitn",
        "join",
        "replace",
        "starts_with",
        "ends_with",
        "parse",
        "lines",
        "chars",
        "bytes",
        "to_lowercase",
        "to_uppercase",
        // sync / io / ubiquitous macros & traits
        "lock",
        "read",
        "write",
        "await",
        "clone_from",
        "eq",
        "cmp",
        "hash",
        "println",
        "print",
        "eprintln",
        "eprint",
        "format",
        "vec",
        "panic",
    ]
    .into_iter()
    .collect()
});

/// A callee/import name with no provenance value: single char (closure params
/// like `r`/`e`, generics like `T`) or a known language built-in.
fn is_noise_callee(name: &str) -> bool {
    name.len() < 2 || NOISE_CALLEES.contains(name)
}

/// Reduce a callee expression's text to its bare trailing name:
/// `self.foo`, `mod::foo`, `obj.foo` -> `foo`.
fn bare_callee(text: &str) -> Option<String> {
    let tail = text.rsplit(['.', ':', '>']).next().unwrap_or(text);
    let cleaned: String = tail
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Extract imported local binding names from an import AST node.
///
/// Uses tree-sitter grammar fields so multi-symbol imports become one edge
/// per symbol (e.g. `use std::sync::{Arc, OnceLock}` -> `["Arc", "OnceLock"]`)
/// rather than a concatenated blob from the whole statement text.
fn import_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
) -> Vec<String> {
    match lang {
        SupportLang::Rust => rust_import_symbols(node),
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            js_ts_import_symbols(node)
        }
        SupportLang::Python => python_import_symbols(node),
        SupportLang::Go => go_import_symbols(node),
        _ => Vec::new(),
    }
}

fn push_nonempty(out: &mut Vec<String>, s: impl AsRef<str>) {
    let t = s.as_ref().trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
}

/// Rust `use_declaration`: field `argument`, then recurse the use-path tree.
fn rust_import_symbols<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arg) = node.field("argument") {
        rust_use_path_symbols(&arg, &mut out);
    }
    out
}

fn rust_use_path_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    out: &mut Vec<String>,
) {
    match node.kind().as_ref() {
        "identifier" => push_nonempty(out, node.text()),
        "scoped_identifier" => {
            if let Some(name) = node.field("name") {
                push_nonempty(out, name.text());
            }
        }
        "scoped_use_list" => {
            if let Some(list) = node.field("list") {
                rust_use_path_symbols(&list, out);
            }
        }
        "use_list" => {
            for child in node.children() {
                if child.is_named() {
                    rust_use_path_symbols(&child, out);
                }
            }
        }
        "use_as_clause" => {
            if let Some(alias) = node.field("alias") {
                push_nonempty(out, alias.text());
            }
        }
        "use_wildcard" => {
            // `use foo::*;` — no local binding name to track.
        }
        _ => {}
    }
}

/// JS/TS `import_statement`: non-field child `import_clause` holds bindings.
fn js_ts_import_symbols<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> Vec<String> {
    let mut out = Vec::new();
    for child in node.children() {
        if !child.is_named() || child.kind().as_ref() != "import_clause" {
            continue;
        }
        for part in child.children() {
            if !part.is_named() {
                continue;
            }
            match part.kind().as_ref() {
                "identifier" => push_nonempty(&mut out, part.text()),
                "named_imports" => {
                    for spec in part.children() {
                        if !spec.is_named() || spec.kind().as_ref() != "import_specifier" {
                            continue;
                        }
                        if let Some(alias) = spec.field("alias") {
                            push_nonempty(&mut out, alias.text());
                        } else if let Some(name) = spec.field("name") {
                            push_nonempty(&mut out, name.text());
                        }
                    }
                }
                "namespace_import" => {
                    for id in part.children() {
                        if id.is_named() && id.kind().as_ref() == "identifier" {
                            push_nonempty(&mut out, id.text());
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Python `import_statement` / `import_from_statement`.
fn python_import_symbols<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> Vec<String> {
    let mut out = Vec::new();
    match node.kind().as_ref() {
        "import_statement" | "import_from_statement" => {
            // `name` is multiple; `.field("name")` would only return the first.
            // `module_name` on import_from is deliberately skipped.
            // `wildcard_import` is a non-field child and emits nothing.
            for name_node in node.field_children("name") {
                python_import_name(&name_node, &mut out);
            }
        }
        _ => {}
    }
    out
}

fn python_import_name<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    out: &mut Vec<String>,
) {
    match node.kind().as_ref() {
        "aliased_import" => {
            if let Some(alias) = node.field("alias") {
                push_nonempty(out, alias.text());
            }
        }
        "dotted_name" => {
            // Bound name is the last identifier segment (`os.path` -> `path`).
            let mut last: Option<String> = None;
            for child in node.children() {
                if child.is_named() && child.kind().as_ref() == "identifier" {
                    last = Some(child.text().to_string());
                }
            }
            if let Some(name) = last {
                push_nonempty(out, name);
            }
        }
        _ => {}
    }
}

/// Go `import_declaration` -> `import_spec` / `import_spec_list`.
fn go_import_symbols<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> Vec<String> {
    let mut out = Vec::new();
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        match child.kind().as_ref() {
            "import_spec" => go_import_spec(&child, &mut out),
            "import_spec_list" => {
                for spec in child.children() {
                    if spec.is_named() && spec.kind().as_ref() == "import_spec" {
                        go_import_spec(&spec, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn go_import_spec<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>, out: &mut Vec<String>) {
    if let Some(name) = node.field("name") {
        push_nonempty(out, name.text());
        return;
    }
    if let Some(path) = node.field("path") {
        let raw = path.text();
        let unquoted = raw
            .trim()
            .trim_matches(|c| c == '"' || c == '`')
            .trim_matches('\'');
        if let Some(seg) = unquoted.rsplit('/').next() {
            push_nonempty(out, seg);
        }
    }
}

/// Canonical kind for a definition AST kind.
fn canonical_kind(ast_kind: &str, lang: SupportLang) -> &'static str {
    if func_kinds(lang).contains(&ast_kind) {
        "function"
    } else if type_kinds(lang).contains(&ast_kind) {
        "type"
    } else {
        "function"
    }
}

/// Path-driven convenience wrapper: derive the language from `file`'s extension.
/// Returns an empty fragment for unsupported file types.
pub fn extract_graph_fragment_for_file(
    source: &str,
    file: &str,
    repo: &str,
    project: &str,
    conv_id: &str,
    session_id: &str,
) -> GraphFragment {
    match super::ast_analysis::lang_from_path_str(file) {
        Some(lang) => {
            extract_graph_fragment(source, lang, file, repo, project, conv_id, session_id)
        }
        None => GraphFragment::default(),
    }
}

/// Extract a graph fragment from a single source file.
#[allow(clippy::too_many_arguments)]
pub fn extract_graph_fragment(
    source: &str,
    lang: SupportLang,
    file: &str,
    repo: &str,
    project: &str,
    conv_id: &str,
    session_id: &str,
) -> GraphFragment {
    if source.len() < 4 {
        return GraphFragment::default();
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        extract_inner(source, lang, file, repo, project, conv_id, session_id)
    }));
    result.unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn extract_inner(
    source: &str,
    lang: SupportLang,
    file: &str,
    repo: &str,
    project: &str,
    conv_id: &str,
    session_id: &str,
) -> GraphFragment {
    let lang_name = lang_name(lang);
    let mut nodes: BTreeMap<String, NodeRow> = BTreeMap::new();
    let mut edges: BTreeMap<(String, String, String), EdgeRow> = BTreeMap::new();

    // Synthetic module node anchoring the file.
    let module_id = node_id(repo, file, "module", file);
    nodes.insert(
        module_id.clone(),
        NodeRow {
            id: module_id.clone(),
            repo: repo.into(),
            project: project.into(),
            file: file.into(),
            lang: lang_name.into(),
            kind: "module".into(),
            name: file.into(),
            fqname: file.into(),
            body_hash: body_hash(source),
            span_start: 0,
            span_end: 0,
            first_conv_id: conv_id.into(),
            last_conv_id: conv_id.into(),
            last_session_id: session_id.into(),
            // Extracted definition node — always definition-backed.
            name_only: false,
        },
    );

    let grep = lang.ast_grep(source);
    let root = grep.root();

    let mk_node = |kind: &str, name: &str, text: &str, start: i64, end: i64| -> NodeRow {
        let id = node_id(repo, file, kind, name);
        NodeRow {
            id,
            repo: repo.into(),
            project: project.into(),
            file: file.into(),
            lang: lang_name.into(),
            kind: kind.into(),
            name: name.into(),
            fqname: format!("{file}::{name}"),
            body_hash: body_hash(text),
            span_start: start,
            span_end: end,
            first_conv_id: conv_id.into(),
            last_conv_id: conv_id.into(),
            last_session_id: session_id.into(),
            // Extracted definition node — always definition-backed.
            name_only: false,
        }
    };

    let mut add_edge = |src: String, dst: String, kind: &str, resolved: i64| {
        let key = (src.clone(), dst.clone(), kind.to_string());
        edges
            .entry(key)
            .and_modify(|e| e.weight += 1.0)
            .or_insert(EdgeRow {
                src_id: src,
                dst_id: dst,
                kind: kind.into(),
                src_file: file.into(),
                resolved,
                weight: 1.0,
                conv_id: conv_id.into(),
                session_id: session_id.into(),
            });
    };

    // Definitions: functions + types -> defines edge from module.
    for (kinds, canon) in [(func_kinds(lang), "function"), (type_kinds(lang), "type")] {
        for kind in kinds {
            let matcher = KindMatcher::new(kind, lang);
            for n in root.find_all(&matcher) {
                if let Some(name) = extract_name_from_def(&n, lang) {
                    // Skip single-char defs (`|e|`, `|r|`, arrow-fn params mis-read
                    // as defs): never useful provenance, only orphan cruft.
                    if name.len() < 2 {
                        continue;
                    }
                    let text = n.text().to_string();
                    let start = n.start_pos().line() as i64;
                    let end = start + text.lines().count().saturating_sub(1) as i64;
                    let node = mk_node(canon, &name, &text, start, end);
                    let nid = node.id.clone();
                    nodes.insert(nid.clone(), node);
                    add_edge(module_id.clone(), nid, "defines", 1);
                }
            }
        }
    }

    // Imports -> imports edge from module (placeholder dst per imported symbol).
    for kind in import_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for n in root.find_all(&matcher) {
            for sym in import_symbols(&n, lang) {
                if is_noise_callee(&sym) {
                    continue;
                }
                add_edge(module_id.clone(), format!("name:{sym}"), "imports", 0);
            }
        }
    }

    // Call sites -> calls edge from enclosing def (placeholder dst).
    for kind in call_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for n in root.find_all(&matcher) {
            let callee = match n.field("function").and_then(|f| bare_callee(&f.text())) {
                Some(c) => c,
                None => continue,
            };
            // Drop ubiquitous built-ins / single-char names: they are never
            // useful provenance targets and wreck name-based resolution.
            if is_noise_callee(&callee) {
                continue;
            }
            // Walk ancestors for the nearest function-like definition.
            let mut src = module_id.clone();
            for anc in n.ancestors() {
                if func_kinds(lang).contains(&anc.kind().as_ref()) {
                    if let Some(def_name) = child_name(&anc) {
                        src = node_id(
                            repo,
                            file,
                            canonical_kind(anc.kind().as_ref(), lang),
                            &def_name,
                        );
                    }
                    break;
                }
            }
            add_edge(src, format!("name:{callee}"), "calls", 0);
        }
    }

    GraphFragment {
        nodes: nodes.into_values().collect(),
        edges: edges.into_values().collect(),
    }
}

fn lang_name(lang: SupportLang) -> &'static str {
    match lang {
        SupportLang::Rust => "rust",
        SupportLang::Python => "python",
        SupportLang::TypeScript => "typescript",
        SupportLang::Tsx => "tsx",
        SupportLang::JavaScript => "javascript",
        SupportLang::Go => "go",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_nodes_and_call_edge_from_rust() {
        let src = "fn foo() {\n    bar();\n}\nfn bar() {}\nstruct Thing {}\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "conv_1",
            "sess_1",
        );

        let names: Vec<&str> = frag.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"foo"), "nodes: {names:?}");
        assert!(names.contains(&"bar"), "nodes: {names:?}");
        assert!(names.contains(&"Thing"), "nodes: {names:?}");
        // module node present
        assert!(frag.nodes.iter().any(|n| n.kind == "module"));

        // calls edge foo -> name:bar
        let foo_id = node_id("repo", "a.rs", "function", "foo");
        let call = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.src_id == foo_id)
            .expect("foo should have a calls edge");
        assert_eq!(call.dst_id, "name:bar");
        assert_eq!(call.resolved, 0, "calls start unresolved");
        assert_eq!(call.src_file, "a.rs");
        assert_eq!(call.conv_id, "conv_1");

        // defines edges from module
        assert!(frag.edges.iter().any(|e| e.kind == "defines"));
    }

    #[test]
    fn noise_callees_are_not_emitted_as_call_edges() {
        // `foo` calls a real `handle()` plus ubiquitous built-ins.
        let src = "fn foo() {\n    let v: Vec<u8> = Vec::new();\n    v.iter().collect::<Vec<_>>();\n    handle();\n}\nfn handle() {}\n";
        let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
        let call_dsts: Vec<&str> = frag
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .map(|e| e.dst_id.as_str())
            .collect();
        assert!(
            call_dsts.contains(&"name:handle"),
            "real callee kept: {call_dsts:?}"
        );
        for noise in ["name:new", "name:iter", "name:collect"] {
            assert!(
                !call_dsts.contains(&noise),
                "{noise} must be filtered: {call_dsts:?}"
            );
        }
    }

    #[test]
    fn body_hash_changes_with_body() {
        let a = extract_graph_fragment(
            "fn foo() { let x = 1; }",
            SupportLang::Rust,
            "a.rs",
            "r",
            "p",
            "c",
            "s",
        );
        let b = extract_graph_fragment(
            "fn foo() { let x = 2; }",
            SupportLang::Rust,
            "a.rs",
            "r",
            "p",
            "c",
            "s",
        );
        let ha = &a.nodes.iter().find(|n| n.name == "foo").unwrap().body_hash;
        let hb = &b.nodes.iter().find(|n| n.name == "foo").unwrap().body_hash;
        assert_ne!(ha, hb, "body hash must track body change");
    }

    #[test]
    fn malformed_source_does_not_panic() {
        let frag =
            extract_graph_fragment("fn ( {{{ ", SupportLang::Rust, "x.rs", "r", "p", "c", "s");
        // module node always present; no panic is the assertion.
        assert!(frag.nodes.iter().any(|n| n.kind == "module"));
    }

    fn import_dsts(frag: &GraphFragment) -> Vec<&str> {
        frag.edges
            .iter()
            .filter(|e| e.kind == "imports")
            .map(|e| e.dst_id.as_str())
            .collect()
    }

    #[test]
    fn rust_multi_symbol_use_emits_per_symbol_import_edges() {
        let src = "use std::sync::{Arc, OnceLock};\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "conv_1",
            "sess_1",
        );
        let dsts = import_dsts(&frag);
        assert!(
            dsts.contains(&"name:Arc"),
            "expected name:Arc among {dsts:?}"
        );
        assert!(
            dsts.contains(&"name:OnceLock"),
            "expected name:OnceLock among {dsts:?}"
        );
        for d in &dsts {
            assert!(
                !d.contains("ArcOnce"),
                "old blob concatenation must not appear: {d}"
            );
        }
    }

    #[test]
    fn rust_use_alias_emits_alias_binding() {
        let src = "use foo::Bar as Baz;\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "conv_1",
            "sess_1",
        );
        let dsts = import_dsts(&frag);
        assert!(
            dsts.contains(&"name:Baz"),
            "expected name:Baz among {dsts:?}"
        );
        assert!(
            !dsts
                .iter()
                .any(|d| d.contains("BarBaz") || *d == "name:BarBaz"),
            "must not concatenate path+alias: {dsts:?}"
        );
    }

    #[test]
    fn typescript_default_named_namespace_imports() {
        let src = "import fs from 'fs';\nimport { fileURLToPath } from 'url';\nimport * as path from 'path';\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "conv_1",
            "sess_1",
        );
        let dsts = import_dsts(&frag);
        assert!(dsts.contains(&"name:fs"), "expected name:fs among {dsts:?}");
        assert!(
            dsts.contains(&"name:fileURLToPath"),
            "expected name:fileURLToPath among {dsts:?}"
        );
        assert!(
            dsts.contains(&"name:path"),
            "expected name:path among {dsts:?}"
        );
    }

    #[test]
    fn python_from_import_and_aliased_import() {
        // Spec fixture. `join` is extracted correctly but is also in the frozen
        // NOISE_CALLEES set (string ops), so edge emission filters it — same
        // is_noise_callee gate as calls. Assert raw symbols + non-noise edges.
        let src = "from os.path import join, exists\nimport numpy as np\n";
        let grep = SupportLang::Python.ast_grep(src);
        let root = grep.root();
        let mut raw: Vec<String> = Vec::new();
        for kind in import_kinds(SupportLang::Python) {
            let matcher = KindMatcher::new(kind, SupportLang::Python);
            for n in root.find_all(&matcher) {
                raw.extend(import_symbols(&n, SupportLang::Python));
            }
        }
        assert!(
            raw.iter().any(|s| s == "join"),
            "AST extraction must yield join: {raw:?}"
        );
        assert!(
            raw.iter().any(|s| s == "exists"),
            "AST extraction must yield exists: {raw:?}"
        );
        assert!(
            raw.iter().any(|s| s == "np"),
            "AST extraction must yield np: {raw:?}"
        );
        assert!(
            is_noise_callee("join"),
            "fixture assumes join is noise-filtered at edge emission"
        );

        let frag = extract_graph_fragment(
            src,
            SupportLang::Python,
            "a.py",
            "repo",
            "proj",
            "conv_1",
            "sess_1",
        );
        let dsts = import_dsts(&frag);
        assert!(
            !dsts.contains(&"name:join"),
            "join is NOISE_CALLEES — must not emit import edge: {dsts:?}"
        );
        assert!(
            dsts.contains(&"name:exists"),
            "expected name:exists among {dsts:?}"
        );
        assert!(dsts.contains(&"name:np"), "expected name:np among {dsts:?}");
    }

    #[test]
    fn import_edges_do_not_swallow_keyword_tokens() {
        let fixtures: &[(&str, SupportLang, &str)] = &[
            (
                "use std::sync::{Arc, OnceLock};\n",
                SupportLang::Rust,
                "a.rs",
            ),
            ("use foo::Bar as Baz;\n", SupportLang::Rust, "b.rs"),
            (
                "import fs from 'fs';\nimport { fileURLToPath } from 'url';\nimport * as path from 'path';\n",
                SupportLang::TypeScript,
                "a.ts",
            ),
            (
                "from os.path import join, exists\nimport numpy as np\n",
                SupportLang::Python,
                "a.py",
            ),
        ];
        for (src, lang, file) in fixtures {
            let frag = extract_graph_fragment(src, *lang, file, "repo", "proj", "conv_1", "sess_1");
            for e in frag.edges.iter().filter(|e| e.kind == "imports") {
                let name = e.dst_id.strip_prefix("name:").unwrap_or(&e.dst_id);
                assert!(
                    !name.contains("import") && !name.contains("from") && !name.contains("require"),
                    "keyword blob regression in {} dst_id={}: {name}",
                    file,
                    e.dst_id
                );
            }
        }
    }
}
